//! The DDA voxel raycast against a brute-force slab oracle and hand-written cases: axis-aligned
//! rays, rays through edges and corners, zero direction components, starts inside a block and on
//! a boundary, `max_t` boundaries, unloaded chunks, filters, and uniform storage.
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{RayResult, TestWorld, VoxelFilter, VoxelHit, raycast_voxels};
use boxel_voxel::{
    BlockPos, BlockState, CHUNK_VOLUME, ChunkData, ChunkPos, Face, LocalPos, WorldPos,
    storage::Paletted,
};
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
    let mut data = ChunkData::uniform(state);
    data.blocks = Paletted::Packed {
        palette: vec![state, BlockState::AIR],
        bits: 1,
        words: vec![0u64; CHUNK_VOLUME / 64].into_boxed_slice(),
    };
    data
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

/// Slab test of the ray against block `p` in the frame of the origin's block, with the DDA's own
/// formula for plane distances: `(near, far, near axis)`, or None if it misses.
fn slab(origin: WorldPos, dir: DVec3, p: BlockPos) -> Option<(f64, f64, usize)> {
    let rel = p.as_i64vec3() - origin.block.as_i64vec3();
    let mut near = f64::NEG_INFINITY;
    let mut far = f64::INFINITY;
    let mut axis = 0;
    for a in 0..3 {
        let lo = rel[a];
        if dir[a] == 0.0 {
            // Never steps: only the origin's own block on this axis.
            if lo != 0 {
                return None;
            }
            continue;
        }
        let inv = 1.0 / dir[a];
        let t_lo = (lo as f64 - origin.frac[a]) * inv;
        let t_hi = ((lo + 1) as f64 - origin.frac[a]) * inv;
        let (enter, exit) = if dir[a] > 0.0 {
            (t_lo, t_hi)
        } else {
            (t_hi, t_lo)
        };
        // On a tie the last axis wins: the DDA steps x, then y, then z, and the block is
        // entered by the last step.
        if enter >= near {
            near = enter;
            axis = a;
        }
        far = far.min(exit);
    }
    (near <= far && far > 0.0).then_some((near, far, axis))
}

/// The nearest full block of `world` inside `base + 0..size` hit by the ray, by testing every
/// block: `(block, face, t)`.
fn oracle(
    world: &TestWorld,
    base: I64Vec3,
    size: i64,
    origin: WorldPos,
    dir: DVec3,
    max_t: f64,
) -> Option<(BlockPos, Option<Face>, f64)> {
    let full = |p: BlockPos| world.get(p) == STONE;
    if full(origin.block) {
        return Some((origin.block, None, 0.0));
    }
    let mut best: Option<(BlockPos, Option<Face>, f64)> = None;
    for y in 0..size {
        for x in 0..size {
            for z in 0..size {
                let p = at(base, x, y, z);
                if !full(p) || p == origin.block {
                    continue;
                }
                let Some((near, _, axis)) = slab(origin, dir, p) else {
                    continue;
                };
                if near < 0.0 || near > max_t || best.is_some_and(|(_, _, t)| t <= near) {
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
                best = Some((p, Some(face), near));
            }
        }
    }
    best
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
        // Across chunk borders at x = 0, y = -32 and z = 32.
        let base = I64Vec3::new(-6, -38, 26);
        let world = random_field(base, 12, density, seed);
        let origin = WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(origin.0, origin.1, origin.2));
        let dir = DVec3::new(dir.0, dir.1, dir.2);
        let result = raycast_voxels(&mut &world, &table(), origin, dir, max_t, VoxelFilter::Collision);
        let expected = oracle(&world, base, 12, origin, dir, max_t);
        match (result, expected) {
            (RayResult::Hit(h), Some((block, face, t))) => {
                prop_assert_eq!((h.block, h.face), (block, face));
                prop_assert_eq!(h.t, t);
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
    }
}
