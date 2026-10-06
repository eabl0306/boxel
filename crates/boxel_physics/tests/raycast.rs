//! The DDA voxel raycast against a brute-force oracle and hand-written cases: axis-aligned
//! rays, rays through edges and corners, zero and degenerate direction components, starts
//! inside a block and on a boundary, `max_t` boundaries and the distance cap, unloaded chunks,
//! filters, and uniform storage.
//!
//! The oracle tests every block on its own (slabs with plane distances by division, not the
//! DDA's reciprocal) and states the tie rule independently. Two strategies feed it: continuous
//! random rays, and dyadic rays (quarter-block origins, directions in `{0, 1/2, 1, 2}` with
//! signs) that pass exactly through edges and corners, where every distance is exact.
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    ChunkSource, MAX_RAY_DISTANCE, Probe, RayResult, TestWorld, VoxelFilter, VoxelHit, VoxelSource,
    raycast_voxels,
};
use boxel_voxel::{
    BlockPos, BlockState, CHUNK_VOLUME, ChunkData, ChunkPos, Face, LocalPos, WorldPos,
    storage::Paletted,
};
use boxel_world::ChunkMap;
use common::{LEAVES, STONE, WATER, at, random_field, table};
use proptest::prelude::*;

fn cast(world: &TestWorld, origin: DVec3, dir: DVec3, max_t: f64) -> RayResult {
    raycast_voxels(
        &mut &*world,
        &table(),
        WorldPos::from_dvec3(origin),
        dir,
        max_t,
        VoxelFilter::Collision,
    )
}

fn hit(result: RayResult) -> VoxelHit {
    match result {
        RayResult::Hit(hit) => hit,
        other => panic!("expected a hit, got {other:?}"),
    }
}

#[test]
fn axis_aligned_rays_hit_the_entered_face_exactly() {
    let mut world = TestWorld::new();
    world.set(BlockPos::new(5, 0, 0), STONE);
    world.set(BlockPos::new(0, -3, 0), STONE);
    world.set(BlockPos::new(0, 0, -40), STONE);
    let o = DVec3::new(0.5, 0.5, 0.5);

    let h = hit(cast(&world, o, DVec3::X, 100.0));
    assert_eq!(
        (h.block, h.face, h.t),
        (BlockPos::new(5, 0, 0), Some(Face::NegX), 4.5)
    );
    assert_eq!(h.point.to_dvec3(), DVec3::new(5.0, 0.5, 0.5));
    assert_eq!(h.state, STONE);
    assert_eq!(h.adjacent(), Some(BlockPos::new(4, 0, 0)));

    let h = hit(cast(&world, o, DVec3::NEG_Y, 100.0));
    assert_eq!(
        (h.block, h.face, h.t),
        (BlockPos::new(0, -3, 0), Some(Face::PosY), 2.5)
    );
    assert_eq!(h.point.to_dvec3(), DVec3::new(0.5, -2.0, 0.5));
    assert_eq!(h.adjacent(), Some(BlockPos::new(0, -2, 0)));

    // Across a chunk border (z = -32), with a non-unit direction: t is in multiples of dir.
    let h = hit(cast(&world, o, DVec3::new(0.0, 0.0, -2.0), 100.0));
    assert_eq!(
        (h.block, h.face, h.t),
        (BlockPos::new(0, 0, -40), Some(Face::PosZ), 19.75)
    );
    assert_eq!(h.point.to_dvec3(), DVec3::new(0.5, 0.5, -39.0));

    // Nothing within max_t.
    assert_eq!(cast(&world, o, DVec3::NEG_X, 30.0), RayResult::Miss);
    assert_eq!(cast(&world, o, DVec3::Y, 30.0), RayResult::Miss);
}

/// The blocks a ray enters, in order, the start block first (a custom filter that accepts
/// nothing and records).
fn visited(origin: DVec3, dir: DVec3, max_t: f64) -> Vec<BlockPos> {
    let world = TestWorld::new();
    let mut seen = Vec::new();
    let mut record = |p: BlockPos, _: BlockState| {
        seen.push(p);
        false
    };
    let result = raycast_voxels(
        &mut &world,
        &table(),
        WorldPos::from_dvec3(origin),
        dir,
        max_t,
        VoxelFilter::Custom(&mut record),
    );
    assert_eq!(result, RayResult::Miss);
    seen
}

#[test]
fn ties_step_x_then_y_then_z() {
    // Exactly through an edge: x and y planes at the same t. x steps first, then y.
    assert_eq!(
        visited(DVec3::new(0.5, 0.5, 0.5), DVec3::new(1.0, 1.0, 0.0), 1.6),
        [
            BlockPos::new(0, 0, 0),
            BlockPos::new(1, 0, 0),
            BlockPos::new(1, 1, 0),
            BlockPos::new(2, 1, 0),
            BlockPos::new(2, 2, 0),
        ]
    );
    // Exactly through a corner: x, then y, then z, all at t = 0.5.
    assert_eq!(
        visited(DVec3::new(0.5, 0.5, 0.5), DVec3::new(-1.0, -1.0, -1.0), 0.6),
        [
            BlockPos::new(0, 0, 0),
            BlockPos::new(-1, 0, 0),
            BlockPos::new(-1, -1, 0),
            BlockPos::new(-1, -1, -1),
        ]
    );
    // The entered faces follow the step: the corner block is entered through its +Z face.
    let mut world = TestWorld::new();
    world.set(BlockPos::new(-1, -1, -1), STONE);
    let h = hit(cast(&world, DVec3::splat(0.5), DVec3::splat(-1.0), 5.0));
    assert_eq!((h.face, h.t), (Some(Face::PosZ), 0.5));
    assert_eq!(h.point.to_dvec3(), DVec3::splat(0.0));
}

#[test]
fn starts_inside_on_boundaries_and_without_direction() {
    let mut world = TestWorld::new();
    world.set(BlockPos::new(2, 0, 0), STONE);
    // Inside a stone: t = 0, no face, nowhere to place.
    let h = hit(cast(&world, DVec3::new(2.5, 0.5, 0.5), DVec3::X, 10.0));
    assert_eq!((h.block, h.face, h.t), (BlockPos::new(2, 0, 0), None, 0.0));
    assert_eq!(h.adjacent(), None);
    // On the boundary x = 3 moving -X: the origin block is 3 (floor), the stone is entered at
    // t = 0 through its +X face.
    let h = hit(cast(&world, DVec3::new(3.0, 0.5, 0.5), DVec3::NEG_X, 10.0));
    assert_eq!(
        (h.block, h.face),
        (BlockPos::new(2, 0, 0), Some(Face::PosX))
    );
    assert_eq!(h.t, 0.0);
    // On the boundary x = 2 moving +X: the origin block is the stone itself.
    let h = hit(cast(&world, DVec3::new(2.0, 0.5, 0.5), DVec3::X, 10.0));
    assert_eq!((h.block, h.face), (BlockPos::new(2, 0, 0), None));
    // No direction: only the origin block counts.
    assert_eq!(
        cast(&world, DVec3::new(0.5, 0.5, 0.5), DVec3::ZERO, 10.0),
        RayResult::Miss
    );
    assert_eq!(
        cast(
            &world,
            DVec3::new(0.5, 0.5, 0.5),
            DVec3::ZERO,
            f64::INFINITY
        ),
        RayResult::Miss
    );
    assert!(
        cast(&world, DVec3::new(2.5, 0.5, 0.5), DVec3::ZERO, 10.0)
            .hit()
            .is_some()
    );
    // Zero components never step: a ray along x at z = 0.999 stays in z block 0.
    world.set(BlockPos::new(6, 0, 1), STONE);
    assert_eq!(
        cast(&world, DVec3::new(3.5, 0.5, 0.999), DVec3::X, 10.0),
        RayResult::Miss
    );
}

#[test]
fn max_t_is_inclusive() {
    let mut world = TestWorld::new();
    world.set(BlockPos::new(3, 0, 0), STONE);
    let o = DVec3::new(0.5, 0.5, 0.5);
    assert_eq!(hit(cast(&world, o, DVec3::X, 2.5)).t, 2.5);
    assert_eq!(cast(&world, o, DVec3::X, 2.4999), RayResult::Miss);
    assert_eq!(cast(&world, o, DVec3::X, -1.0), RayResult::Miss);
    assert_eq!(cast(&world, o, DVec3::X, f64::NAN), RayResult::Miss);
}

#[test]
fn every_ray_ends_within_the_distance_cap() {
    let mut world = TestWorld::new();
    let o = DVec3::new(0.5, 0.5, 0.5);
    // Air all the way (absent chunks are air): an infinite max_t ends at the cap, not at the
    // world limit 2^61 blocks away.
    for dir in [DVec3::Y, DVec3::NEG_Y, DVec3::new(0.3, -1.0, 0.7)] {
        assert_eq!(cast(&world, o, dir, f64::INFINITY), RayResult::Miss);
        assert_eq!(cast(&world, o, dir, 1e300), RayResult::Miss);
    }
    // The cap is per axis, in blocks, whatever the length of dir.
    let cap = MAX_RAY_DISTANCE as i64;
    world.set(BlockPos::new(cap - 96, 0, 0), STONE);
    world.set(BlockPos::new(0, 0, cap + 104), STONE);
    for scale in [1.0, 2.0, 0.25, 1e-200] {
        let h = hit(cast(&world, o, DVec3::X * scale, f64::INFINITY));
        assert_eq!(h.block, BlockPos::new(cap - 96, 0, 0));
        if scale >= 0.25 {
            assert_eq!(h.t * scale, (cap - 96) as f64 - 0.5);
        }
        assert_eq!(
            cast(&world, o, DVec3::Z * scale, f64::INFINITY),
            RayResult::Miss,
            "past the cap"
        );
    }
}

#[test]
fn rays_with_no_usable_direction_test_only_the_origin_block() {
    let mut world = TestWorld::new();
    world.set(BlockPos::new(0, 3, 0), STONE);
    world.set(BlockPos::new(0, 0, -1), STONE);
    world.set(BlockPos::new(5, 5, 5), STONE);
    let o = DVec3::new(0.5, 0.5, 0.5);
    // An infinite component used to make every step distance 0: the ray stepped for ever at
    // t = 0. A NaN component used to silently drop that axis.
    for dir in [
        DVec3::new(0.0, f64::INFINITY, 0.0),
        DVec3::new(0.0, f64::NEG_INFINITY, 0.0),
        DVec3::new(f64::NAN, 1.0, 0.0),
        DVec3::new(1.0, 1.0, f64::INFINITY),
    ] {
        for max_t in [10.0, f64::INFINITY] {
            assert_eq!(cast(&world, o, dir, max_t), RayResult::Miss, "{dir:?}");
        }
        let inside = hit(cast(&world, DVec3::new(5.5, 5.5, 5.5), dir, 10.0));
        assert_eq!(
            (inside.block, inside.face, inside.t),
            (BlockPos::new(5, 5, 5), None, 0.0)
        );
    }
    // A component so small that its reciprocal overflows never steps: on the plane z = 0
    // moving -Z it used to step at t = NaN into the stone below (0 * inf).
    let tiny = -5e-324;
    let on_plane = DVec3::new(0.5, 0.5, 0.0);
    assert_eq!(
        cast(&world, on_plane, DVec3::new(0.0, 0.0, tiny), 10.0),
        RayResult::Miss
    );
    let h = hit(cast(&world, on_plane, DVec3::new(0.0, 1.0, tiny), 10.0));
    assert_eq!(
        (h.block, h.face, h.t),
        (BlockPos::new(0, 3, 0), Some(Face::NegY), 2.5)
    );
}

#[test]
fn a_ray_through_the_air_outside_the_vertical_bounds_ends() {
    // An empty map: every layer outside -1..=1 reads as air (never Unloaded), so only the cap
    // ends a vertical ray that starts above or below the bounds.
    let map = ChunkMap::default();
    let mut src = ChunkSource::with_bounds(&map, Some(-1..=1));
    assert_eq!(
        src.probe(BlockPos::new(0, 200, 0)),
        Probe::Block(BlockState::AIR)
    );
    assert_eq!(src.probe(BlockPos::new(0, 0, 0)), Probe::Unloaded);
    let table = table();
    for (y, dir) in [
        (200.5, DVec3::Y),
        (-200.5, DVec3::NEG_Y),
        (200.5, DVec3::new(0.6, 0.8, 0.0)),
    ] {
        let result = raycast_voxels(
            &mut src,
            &table,
            WorldPos::from_dvec3(DVec3::new(0.5, y, 0.5)),
            dir,
            f64::INFINITY,
            VoxelFilter::Collision,
        );
        assert_eq!(result, RayResult::Miss, "{y} {dir:?}");
    }
    // Toward the bounds it reaches the unloaded layers.
    let result = raycast_voxels(
        &mut src,
        &table,
        WorldPos::from_dvec3(DVec3::new(0.5, 200.5, 0.5)),
        DVec3::NEG_Y,
        f64::INFINITY,
        VoxelFilter::Collision,
    );
    assert_eq!(
        result,
        RayResult::Unloaded {
            block: BlockPos::new(0, 63, 0),
            t: 136.5
        }
    );
}

#[test]
fn unloaded_chunks_end_the_ray_with_their_first_block() {
    let mut world = TestWorld::new();
    world.set(BlockPos::new(40, 0, 0), STONE);
    world.set_unloaded(ChunkPos::new(1, 0, 0), true);
    let result = cast(&world, DVec3::new(0.5, 0.5, 0.5), DVec3::X, 100.0);
    assert_eq!(
        result,
        RayResult::Unloaded {
            block: BlockPos::new(32, 0, 0),
            t: 31.5
        }
    );
    assert!(result.hit().is_none());
    // A hit before the unloaded chunk wins.
    world.set(BlockPos::new(10, 0, 0), STONE);
    assert_eq!(
        hit(cast(&world, DVec3::new(0.5, 0.5, 0.5), DVec3::X, 100.0))
            .block
            .x,
        10
    );
    // Starting in it.
    let result = cast(&world, DVec3::new(33.5, 0.5, 0.5), DVec3::X, 100.0);
    assert_eq!(
        result,
        RayResult::Unloaded {
            block: BlockPos::new(33, 0, 0),
            t: 0.0
        }
    );
}

#[test]
fn filters_choose_what_stops_the_ray() {
    let mut world = TestWorld::new();
    world.set(BlockPos::new(2, 0, 0), WATER);
    world.set(BlockPos::new(4, 0, 0), LEAVES);
    world.set(BlockPos::new(6, 0, 0), STONE);
    let o = WorldPos::from_dvec3(DVec3::new(0.5, 0.5, 0.5));
    let table = table();
    let run = |filter: VoxelFilter<'_>| {
        raycast_voxels(&mut &world, &table, o, DVec3::X, 20.0, filter)
            .hit()
            .map(|h| (h.block.x, h.state))
    };
    assert_eq!(
        run(VoxelFilter::Collision),
        Some((4, LEAVES)),
        "water does not collide"
    );
    assert_eq!(
        run(VoxelFilter::Outline { fluids: false }),
        Some((4, LEAVES))
    );
    assert_eq!(run(VoxelFilter::Outline { fluids: true }), Some((2, WATER)));
    let mut only_stone = |_: BlockPos, s: BlockState| s == STONE;
    assert_eq!(run(VoxelFilter::Custom(&mut only_stone)), Some((6, STONE)));
    assert_eq!(
        format!("{:?}", VoxelFilter::Outline { fluids: true }),
        "Outline { fluids: true }"
    );
}

/// A chunk of `state` stored as `Packed` (1 bit, every index 0) instead of `Uniform`.
fn packed(state: BlockState) -> ChunkData {
    ChunkData::from_paletted(Paletted::Packed {
        palette: vec![state, BlockState::AIR],
        bits: 1,
        words: vec![0u64; CHUNK_VOLUME / 64].into_boxed_slice(),
    })
}

#[test]
fn uniform_storage_gives_the_same_rays_as_packed_storage() {
    // The same blocks two ways: chunks stored Uniform, and the same chunks stored Packed.
    let chunks = [
        (ChunkPos::new(0, 0, 0), BlockState::AIR),
        (ChunkPos::new(1, 0, 0), BlockState::AIR),
        (ChunkPos::new(1, 1, 0), BlockState::AIR),
        (ChunkPos::new(2, 0, 0), STONE),
        (ChunkPos::new(0, -1, 0), STONE),
        (ChunkPos::new(1, -1, 1), WATER),
    ];
    let mut uniform = TestWorld::new();
    let mut mixed = TestWorld::new();
    for (pos, state) in chunks {
        uniform.set_chunk(pos, ChunkData::uniform(state));
        mixed.set_chunk(pos, packed(state));
        assert_eq!(
            mixed.chunk(pos).expect("set").get(LocalPos::new(3, 4, 5)),
            state
        );
    }
    let mut rng = common::Rng(7);
    for _ in 0..2000 {
        let origin = DVec3::new(
            rng.unit() * 64.0,
            rng.unit() * 40.0 - 8.0,
            rng.unit() * 32.0,
        );
        let dir = DVec3::new(rng.unit() - 0.3, rng.unit() - 0.6, rng.unit() - 0.5);
        for filter in [0, 1] {
            let run = |world: &TestWorld| {
                let filter = if filter == 0 {
                    VoxelFilter::Collision
                } else {
                    VoxelFilter::Outline { fluids: true }
                };
                raycast_voxels(
                    &mut &*world,
                    &table(),
                    WorldPos::from_dvec3(origin),
                    dir,
                    80.0,
                    filter,
                )
            };
            assert_eq!(run(&uniform), run(&mixed), "{origin:?} {dir:?}");
        }
    }
}

// ---------------------------------------------------------------- oracle

/// How the oracle sees one block: the `t` where the DDA enters it, the number of steps the DDA
/// takes at that `t` up to and including this block (orders the blocks of one tie), and the
/// axis of the entered face.
type Entry = (f64, usize, usize);

/// Whether the DDA visits block `p` (other than the origin's block), from `p` alone.
///
/// Plane distances are exact divisions `(plane - frac) / dir`, independent of the DDA's
/// `(plane - frac) * (1 / dir)`. The tie rule, as the DDA promises it:
/// - A zero direction component never steps: only the origin's block on that axis.
/// - The ray crosses the planes ahead of its origin's block on each axis (from `t = 0` on: a
///   ray that starts on a plane moving away from it never crosses that one).
/// - A block the ray passes through for some length is visited, entered at the largest plane
///   distance of its slabs, through the face of the last axis (x, y, z order) whose crossing
///   reaches it there: on a tie the DDA steps the tied axes x, then y, then z.
/// - A block the ray only touches at one `t` (along an edge or at a corner) is visited only when
///   it is on that staircase: compared with the block before `t`, it changed exactly the first
///   `k >= 1` of the axes that cross at `t`, in x, y, z order, and no other axis (a ray that
///   starts on a plane it never crosses only touches the block behind that plane).
fn visit(origin: WorldPos, dir: DVec3, p: BlockPos) -> Option<Entry> {
    let rel = p.as_i64vec3() - origin.block.as_i64vec3();
    let mut enter = [f64::NEG_INFINITY; 3];
    let mut exit = [f64::INFINITY; 3];
    // Whether the ray crosses the plane through which it enters / leaves `p` on each axis.
    let mut crosses_in = [false; 3];
    let mut crosses_out = [false; 3];
    for a in 0..3 {
        if dir[a] == 0.0 {
            if rel[a] != 0 {
                return None;
            }
            continue;
        }
        let lo = (rel[a] as f64 - origin.frac[a]) / dir[a];
        let hi = ((rel[a] + 1) as f64 - origin.frac[a]) / dir[a];
        // Plane offsets from the origin's block: crossed moving + when >= 1, moving - when <= 0.
        if dir[a] > 0.0 {
            (enter[a], exit[a]) = (lo, hi);
            (crosses_in[a], crosses_out[a]) = (rel[a] >= 1, rel[a] >= 0);
        } else {
            (enter[a], exit[a]) = (hi, lo);
            (crosses_in[a], crosses_out[a]) = (rel[a] < 0, rel[a] <= 0);
        }
    }
    let near = enter.into_iter().fold(f64::NEG_INFINITY, f64::max);
    let far = exit.into_iter().fold(f64::INFINITY, f64::min);
    // The ray only exists from t = 0.
    let t = if near > 0.0 { near } else { 0.0 };
    if t > far {
        return None;
    }
    // Axes whose crossing at t enters this block, and axes the ray crosses a plane on at t.
    let entered: Vec<usize> = (0..3).filter(|&a| crosses_in[a] && enter[a] == t).collect();
    let crossing: Vec<usize> = (0..3)
        .filter(|&a| (crosses_in[a] && enter[a] == t) || (crosses_out[a] && exit[a] == t))
        .collect();
    let face_axis = *entered.last()?;
    if t < far {
        return Some((t, entered.len(), face_axis));
    }
    // Touched at one t only: on the staircase iff `entered` is a leading part of `crossing`,
    // and not behind a plane the ray starts on and never crosses.
    let behind = (0..3).any(|a| !crosses_out[a] && exit[a] == t);
    (!behind && crossing.starts_with(&entered)).then_some((t, entered.len(), face_axis))
}

/// Every block of `base + 0..size` the DDA visits within `max_t`, in visiting order, with the
/// entered face and `t` (the origin's block first, at `t = 0` with no face).
fn oracle_path(
    base: I64Vec3,
    size: i64,
    origin: WorldPos,
    dir: DVec3,
    max_t: f64,
) -> Vec<(BlockPos, Option<Face>, f64)> {
    let inside = |p: BlockPos| {
        let r = p.as_i64vec3() - base;
        r.cmpge(I64Vec3::ZERO).all() && r.cmplt(I64Vec3::splat(size)).all()
    };
    let mut path: Vec<(f64, usize, BlockPos, Option<Face>)> = Vec::new();
    if inside(origin.block) {
        path.push((0.0, 0, origin.block, None));
    }
    for y in 0..size {
        for x in 0..size {
            for z in 0..size {
                let p = at(base, x, y, z);
                if p == origin.block {
                    continue;
                }
                let Some((t, steps, axis)) = visit(origin, dir, p) else {
                    continue;
                };
                if t > max_t {
                    continue;
                }
                let face = match (axis, dir[axis] > 0.0) {
                    (0, true) => Face::NegX,
                    (0, false) => Face::PosX,
                    (1, true) => Face::NegY,
                    (1, false) => Face::PosY,
                    (_, true) => Face::NegZ,
                    (_, false) => Face::PosZ,
                };
                path.push((t, steps, p, Some(face)));
            }
        }
    }
    path.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    path.into_iter()
        .map(|(t, _, p, face)| (p, face, t))
        .collect()
}

/// The DDA against the oracle on a random field: the blocks it visits inside the field (in
/// order), and the first stone it hits (block, face, `t`, point). `exact`: every plane distance
/// is exact, so `t` must match bit for bit.
fn check_against_oracle(
    seed: u64,
    density: f64,
    origin: DVec3,
    dir: DVec3,
    max_t: f64,
    exact: bool,
) -> Result<(), TestCaseError> {
    // Across chunk borders at x = 0, y = -32 and z = 32.
    let base = I64Vec3::new(-6, -38, 26);
    let size = 12;
    let world = random_field(base, size, density, seed);
    let origin = WorldPos::from_block(at(base, 0, 0, 0), origin);
    let path = oracle_path(base, size, origin, dir, max_t);

    let mut seen = Vec::new();
    let mut record = |p: BlockPos, _: BlockState| {
        let r = p.as_i64vec3() - base;
        if r.cmpge(I64Vec3::ZERO).all() && r.cmplt(I64Vec3::splat(size)).all() {
            seen.push(p);
        }
        false
    };
    let all = raycast_voxels(
        &mut &world,
        &table(),
        origin,
        dir,
        max_t,
        VoxelFilter::Custom(&mut record),
    );
    prop_assert_eq!(all, RayResult::Miss);
    let expected: Vec<BlockPos> = path.iter().map(|&(p, _, _)| p).collect();
    prop_assert_eq!(seen, expected);

    let result = raycast_voxels(
        &mut &world,
        &table(),
        origin,
        dir,
        max_t,
        VoxelFilter::Collision,
    );
    let first = path.into_iter().find(|&(p, _, _)| world.get(p) == STONE);
    match (result, first) {
        (RayResult::Hit(h), Some((block, face, t))) => {
            prop_assert_eq!((h.block, h.face), (block, face));
            if exact {
                prop_assert_eq!(h.t, t);
            } else {
                prop_assert!(
                    (h.t - t).abs() <= 1e-12 * t.max(1.0),
                    "dda t {} oracle t {}",
                    h.t,
                    t
                );
            }
            // The point is on the ray, and exactly on the entered face's plane.
            let along = origin.delta_to(&h.point) - dir * h.t;
            prop_assert!(along.abs().max_element() < 1e-9, "{:?}", along);
            if let Some(face) = h.face {
                let (axis, _, _) = face.axes();
                prop_assert_eq!(h.point.frac[axis], 0.0);
            }
        }
        (RayResult::Miss, None) => {}
        (got, want) => prop_assert!(false, "dda {:?} oracle {:?}", got, want),
    }
    Ok(())
}

/// A direction component of the dyadic strategy: 0, 1/2, 1 or 2, with a sign.
fn dyadic_component() -> impl Strategy<Value = f64> {
    prop::sample::select(vec![-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    #[test]
    fn the_dda_finds_the_nearest_block_like_brute_force(
        seed in any::<u64>(),
        density in 0.0..0.3f64,
        origin in (0.0..12.0f64, 0.0..12.0f64, 0.0..12.0f64),
        dir in (-1.0..1.0f64, -1.0..1.0f64, -1.0..1.0f64),
        max_t in 0.0..30.0f64,
    ) {
        check_against_oracle(
            seed,
            density,
            DVec3::new(origin.0, origin.1, origin.2),
            DVec3::new(dir.0, dir.1, dir.2),
            max_t,
            false,
        )?;
    }

    /// Quarter-block origins and directions in `{0, 1/2, 1, 2}`: rays pass exactly through
    /// edges and corners (and start on them), where only the tie rule decides.
    #[test]
    fn edge_and_corner_ties_follow_the_staircase_like_brute_force(
        seed in any::<u64>(),
        density in 0.0..0.3f64,
        origin in (0..48u8, 0..48u8, 0..48u8),
        dir in (dyadic_component(), dyadic_component(), dyadic_component()),
        max_t in 0..=120u8,
    ) {
        let quarter = |v: u8| f64::from(v) / 4.0;
        check_against_oracle(
            seed,
            density,
            DVec3::new(quarter(origin.0), quarter(origin.1), quarter(origin.2)),
            DVec3::new(dir.0, dir.1, dir.2),
            quarter(max_t),
            true,
        )?;
    }
}
