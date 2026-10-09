//! Fences collide 1.5 blocks tall, as in Minecraft (issue #46, ADR 0026): their boxes reach
//! half a block into the cell above. The sweep sees them from the row above (moving across, and
//! landing on their tops), the collision raycast meets them there, the step-up never climbs
//! them from the ground, and picking keeps their outline a block tall. Everything is exact and
//! the same bit for bit far from the origin.
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    CollisionShape, LocalBox, RayResult, StepUp, SweepConfig, SweepOut, TestWorld, VoxelFilter,
    move_box, move_box_stepping, raycast_voxels,
};
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, Face, WorldPos};
use common::{STONE, at, fill, table};

fn state(name: &str) -> BlockState {
    BlockRegistry::default_blocks().parse_state(name).unwrap()
}

/// A fence running along X (connected east and west).
fn fence() -> BlockState {
    state("boxel:oak_fence[east=true,west=true]")
}

/// The bases the scenes are moved to: the origin, ±2^31, 2^40 and near the world limit.
fn bases() -> [I64Vec3; 5] {
    [
        I64Vec3::new(0, 0, 0),
        I64Vec3::new(1 << 31, -(1 << 31), 1 << 31),
        I64Vec3::new(-(1 << 31), 1 << 31, 7),
        I64Vec3::new(1 << 40, 5, -(1 << 40)),
        I64Vec3::new((1 << 61) - 4096, -(1 << 61) + 4096, (1 << 61) - 4096),
    ]
}

/// A stone floor at y = -1 around `base`, a fence row along X at z = 0 from x = -3 to 3, and a
/// stone block at (0, 0, -3) to stand on one block up.
fn scene(base: I64Vec3) -> TestWorld {
    let mut world = TestWorld::new();
    fill(&mut world, base, [-6, -1, -6], [6, -1, 6], STONE);
    fill(&mut world, base, [-3, 0, 0], [3, 0, 0], fence());
    world.set(at(base, 0, 0, -2), STONE);
    world
}

/// `pos` relative to `base`, exact.
fn local(base: I64Vec3, pos: WorldPos) -> DVec3 {
    let b = pos.block.as_i64vec3() - base;
    DVec3::new(b.x as f64, b.y as f64, b.z as f64) + pos.frac
}

/// A 0.5 x 1.75 x 0.5 box with its origin at the feet (binary fractions: exact positions).
fn body() -> LocalBox {
    LocalBox::new(DVec3::new(-0.25, 0.0, -0.25), DVec3::new(0.25, 1.75, 0.25))
}

fn sweep(world: &mut TestWorld, base: I64Vec3, start: DVec3, d: DVec3, step: StepUp) -> SweepOut {
    move_box_stepping(
        world,
        &table(),
        WorldPos::from_block(BlockPos::from_i64vec3(base), start),
        body(),
        d,
        &[],
        &SweepConfig::default(),
        step,
    )
    .expect("loaded")
}

/// Walking: the step-up of a player standing on the ground.
const WALK: StepUp = StepUp {
    height: 0.6,
    grounded: true,
};

#[test]
fn the_table_knows_how_high_fences_reach() {
    let t = table();
    let CollisionShape::Boxes(boxes) = t.shape(fence()) else {
        panic!("a fence collides with boxes");
    };
    assert!(boxes.iter().all(|b| b.max.y == 1.5 && b.min.y == 0.0));
    let CollisionShape::Boxes(outline) = t.outline(fence()) else {
        panic!("a fence is picked by boxes");
    };
    assert!(outline.iter().all(|b| b.max.y == 1.0));
    assert!(t.is_tall(fence()) && t.is_tall(state("boxel:oak_fence")));
    assert!(!t.is_tall(STONE) && !t.is_tall(state("boxel:stone_slab")));
    assert_eq!(t.reach(), 0.5);
}

#[test]
fn a_fence_stops_a_walker_and_cannot_be_stepped_or_jumped() {
    let mut results = Vec::new();
    for base in bases() {
        let mut world = scene(base);
        let mut run = Vec::new();
        // On the ground, walking south into the row: stopped by the arm at z = 6/16, plain and
        // with the step-up alike (1.5 blocks is far more than 0.6).
        let start = DVec3::new(-1.5, 0.0, -1.0);
        let plain = sweep(
            &mut world,
            base,
            start,
            DVec3::new(0.0, 0.0, 2.0),
            StepUp::NONE,
        );
        let walked = sweep(&mut world, base, start, DVec3::new(0.0, 0.0, 2.0), WALK);
        assert_eq!(walked, plain);
        assert_eq!(local(base, plain.end), DVec3::new(-1.5, 0.0, 0.125));
        assert!(plain.touched(Face::PosZ));
        assert_eq!(plain.blocks[2], Some(at(base, -2, 0, 0)));
        run.push(local(base, plain.end));
        // A jump peaks at 1.24 blocks; even at 1.4375 the feet are below the fence's top: the
        // tall part, in the cell above, stops it too.
        let jump = DVec3::new(-1.5, 1.4375, -1.0);
        let out = sweep(
            &mut world,
            base,
            jump,
            DVec3::new(0.0, 0.0, 2.0),
            StepUp::NONE,
        );
        assert_eq!(local(base, out.end), DVec3::new(-1.5, 1.4375, 0.125));
        assert_eq!(out.blocks[2], Some(at(base, -2, 0, 0)), "the fence below");
        // At 1.5 the feet are on the fence's top: free to go over.
        let over = DVec3::new(-1.5, 1.5, -1.0);
        let out = sweep(
            &mut world,
            base,
            over,
            DVec3::new(0.0, 0.0, 2.0),
            StepUp::NONE,
        );
        assert_eq!(out.faces, 0);
        // Moving along the row on its top (X) never snags on the seams between posts.
        let top = DVec3::new(-2.5, 1.5, 0.5);
        let out = sweep(
            &mut world,
            base,
            top,
            DVec3::new(5.0, 0.0, 0.0),
            StepUp::NONE,
        );
        assert_eq!(out.faces, 0);
        assert_eq!(local(base, out.end), DVec3::new(2.5, 1.5, 0.5));
        results.push(run);
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn from_a_block_up_the_fence_top_is_a_step() {
    for base in bases() {
        let mut world = scene(base);
        // Standing on the stone block at (0, 0, -2), top at y = 1: the fence's top half is at
        // the feet. Without the step-up it is a wall; with it (0.5 < 0.6) the walker climbs
        // onto its top, as in Minecraft.
        let start = DVec3::new(0.5, 1.0, -1.5);
        let d = DVec3::new(0.0, 0.0, 2.0);
        let plain = sweep(&mut world, base, start, d, StepUp::NONE);
        assert_eq!(local(base, plain.end), DVec3::new(0.5, 1.0, 0.125));
        let stepped = sweep(&mut world, base, start, d, WALK);
        assert_eq!(local(base, stepped.end), DVec3::new(0.5, 1.5, 0.5));
        assert!(stepped.on_ground());
        assert_eq!(stepped.ground, Some(at(base, 0, 0, 0)));
    }
}

#[test]
fn bodies_land_on_fence_tops_and_are_pushed_out_of_them() {
    for base in bases() {
        let mut world = scene(base);
        // Falling onto the post of (2, 0, 0): on its top, 1.5 above the fence's block, which
        // is the ground.
        let out = sweep(
            &mut world,
            base,
            DVec3::new(2.5, 4.0, 0.5),
            DVec3::new(0.0, -6.0, 0.0),
            StepUp::NONE,
        );
        assert_eq!(local(base, out.end), DVec3::new(2.5, 1.5, 0.5));
        assert_eq!(out.ground, Some(at(base, 2, 0, 0)));
        // Falling a bit beside the row (the box overlaps it by 1/16): on its top as well.
        let out = sweep(
            &mut world,
            base,
            DVec3::new(2.5, 4.0, 0.1875),
            DVec3::new(0.0, -6.0, 0.0),
            StepUp::NONE,
        );
        assert_eq!(local(base, out.end).y, 1.5);
        // Only touching its side: down to the floor.
        let out = sweep(
            &mut world,
            base,
            DVec3::new(2.5, 4.0, 0.875),
            DVec3::new(0.0, -6.0, 0.0),
            StepUp::NONE,
        );
        assert_eq!(local(base, out.end).y, 0.0);
        // A body inside the fence's top half (a fence placed under it) is pushed out onto a
        // block plane, free of every box.
        let inside = DVec3::new(-1.5, 1.25, 0.5);
        let out = sweep(&mut world, base, inside, DVec3::ZERO, StepUp::NONE);
        assert!(out.depenetrated);
        let end = local(base, out.end);
        assert_eq!(
            end,
            DVec3::new(-1.5, 2.0, 0.5),
            "up onto the next block plane"
        );
        let t = table();
        let me = body().translate(end);
        for x in -3..=3 {
            let CollisionShape::Boxes(boxes) = t.shape(fence()) else {
                unreachable!()
            };
            for b in boxes {
                let b = b.translate(DVec3::new(x as f64, 0.0, 0.0));
                assert!(!b.overlaps(me, 1e-7), "{end} overlaps {b:?}");
            }
        }
    }
}

#[test]
fn collision_rays_meet_the_tall_part_and_picking_does_not() {
    let t = table();
    for base in bases() {
        let mut world = scene(base);
        world.set(at(base, 0, 1, 4), STONE);
        let origin = |p: DVec3| WorldPos::from_block(BlockPos::from_i64vec3(base), p);
        // Level, at 1.25 (the cell above the row): a collision ray hits the arm's side.
        let ray = |world: &mut TestWorld, filter| {
            raycast_voxels(
                world,
                &t,
                origin(DVec3::new(0.5, 1.25, -2.5)),
                DVec3::Z,
                10.0,
                filter,
            )
        };
        let RayResult::Hit(hit) = ray(&mut world, VoxelFilter::Collision) else {
            panic!("the tall part");
        };
        assert_eq!(hit.block, at(base, 0, 0, 0));
        assert_eq!(hit.face, Some(Face::NegZ));
        assert_eq!(hit.t, 2.5 + 0.375);
        assert_eq!(local(base, hit.point), DVec3::new(0.5, 1.25, 0.375));
        // Picking goes over it (the outline is a block tall) to the stone behind.
        let RayResult::Hit(hit) = ray(&mut world, VoxelFilter::Outline { fluids: false }) else {
            panic!("the stone");
        };
        assert_eq!(hit.block, at(base, 0, 1, 4));
        // Straight down onto a post: its top, exactly 1.5 up (in the cell above the fence).
        let RayResult::Hit(hit) = raycast_voxels(
            &mut world,
            &t,
            origin(DVec3::new(-2.5, 5.0, 0.5)),
            -DVec3::Y,
            10.0,
            VoxelFilter::Collision,
        ) else {
            panic!("the post's top");
        };
        assert_eq!(
            (hit.block, hit.face, hit.t),
            (at(base, -3, 0, 0), Some(Face::PosY), 3.5)
        );
        assert_eq!(hit.point.block, at(base, -3, 1, 0));
        assert_eq!(hit.point.frac, DVec3::new(0.5, 0.5, 0.5));
        // From inside the tall part: t = 0, no face.
        let RayResult::Hit(hit) = raycast_voxels(
            &mut world,
            &t,
            origin(DVec3::new(-2.5, 1.25, 0.5)),
            DVec3::X,
            10.0,
            VoxelFilter::Collision,
        ) else {
            panic!("inside");
        };
        assert_eq!(
            (hit.block, hit.face, hit.t),
            (at(base, -3, 0, 0), None, 0.0)
        );
        // Above the tops, a miss; through the gap above the row's end, a miss too.
        let over = raycast_voxels(
            &mut world,
            &t,
            origin(DVec3::new(-5.5, 1.75, 0.5)),
            DVec3::X,
            4.0,
            VoxelFilter::Collision,
        );
        assert_eq!(over, RayResult::Miss);
        // A block in the cell above, in front of the fence's top half, wins over it.
        world.set(at(base, 1, 1, 0), STONE);
        let RayResult::Hit(hit) = raycast_voxels(
            &mut world,
            &t,
            origin(DVec3::new(1.5, 1.25, 2.5)),
            -DVec3::Z,
            10.0,
            VoxelFilter::Collision,
        ) else {
            panic!("the stone at (1, 1, 0)");
        };
        assert_eq!((hit.block, hit.t), (at(base, 1, 1, 0), 1.5));
    }
}

#[test]
fn an_unloaded_chunk_under_a_crossed_cell_stops_a_collision_ray() {
    let t = table();
    let mut world = TestWorld::new();
    // The ray runs just above the bottom of chunk y = 0; the chunk under it is not loaded.
    world.set_unloaded(boxel_voxel::ChunkPos::new(0, -1, 0), true);
    let low = raycast_voxels(
        &mut world,
        &t,
        WorldPos::from_dvec3(DVec3::new(0.5, 0.25, 0.5)),
        DVec3::X,
        4.0,
        VoxelFilter::Collision,
    );
    assert!(matches!(low, RayResult::Unloaded { .. }), "{low:?}");
    // High in the cell nothing under it can reach the ray.
    let high = raycast_voxels(
        &mut world,
        &t,
        WorldPos::from_dvec3(DVec3::new(0.5, 0.75, 0.5)),
        DVec3::X,
        4.0,
        VoxelFilter::Collision,
    );
    assert_eq!(high, RayResult::Miss);
    // A block the ray meets before it comes within reach of the unloaded row is its hit: going
    // down into the stone at x = 1, it gets low only past the stone's face.
    world.set(BlockPos::new(1, 0, 0), STONE);
    let RayResult::Hit(hit) = raycast_voxels(
        &mut world,
        &t,
        WorldPos::from_dvec3(DVec3::new(0.5, 0.75, 0.5)),
        DVec3::new(1.0, -0.375, 0.0),
        4.0,
        VoxelFilter::Collision,
    ) else {
        panic!("the stone at x = 1");
    };
    assert_eq!(
        (hit.block, hit.face, hit.t),
        (BlockPos::new(1, 0, 0), Some(Face::NegX), 0.5)
    );
    world.set(BlockPos::new(1, 0, 0), BlockState::AIR);
    // A sweep across at the bottom of the chunk needs that row too (a fence could reach in).
    let out = move_box(
        &mut world,
        &t,
        WorldPos::from_dvec3(DVec3::new(0.5, 0.0, 0.5)),
        LocalBox::new(DVec3::new(-0.3, 0.0, -0.3), DVec3::new(0.3, 1.8, 0.3)),
        DVec3::new(1.0, 0.0, 0.0),
        &[],
        &SweepConfig::default(),
    );
    assert!(out.is_err());
}

/// No fence box of `world` around `base` overlaps the body at `end` (frame of `base`).
fn clear_of_fences(world: &mut TestWorld, base: I64Vec3, end: DVec3) {
    use boxel_physics::{Probe, VoxelSource};
    let t = table();
    let me = body().translate(end);
    for x in -2..=2 {
        for y in -1..=2 {
            for z in -2..=2 {
                let Probe::Block(state) = world.probe(at(base, x, y, z)) else {
                    continue;
                };
                if !t.is_tall(state) {
                    continue;
                }
                let CollisionShape::Boxes(boxes) = t.shape(state) else {
                    unreachable!()
                };
                for b in boxes {
                    let b = b.translate(DVec3::new(x as f64, y as f64, z as f64));
                    assert!(!b.overlaps(me, 1e-7), "{end} overlaps {b:?}");
                }
            }
        }
    }
}

#[test]
fn a_block_in_the_same_layer_never_hides_a_fence_top() {
    // A stone block at the walker's level right next to a fence that connects to it (the usual
    // build), from each side: the stone comes before the fence in the layer scan from two of
    // them, and the fence's top, half a block higher, must still win.
    for (axis, sign, side) in [
        (2, 1.0, "north"),
        (2, -1.0, "south"),
        (0, 1.0, "west"),
        (0, -1.0, "east"),
    ] {
        let mut dir = DVec3::ZERO;
        dir[axis] = sign;
        // The stone is the fence's neighbor on `side`, the one the walker comes from.
        let mut stone = [0i64; 3];
        stone[axis] = -sign as i64;
        let fence = state(&format!("boxel:oak_fence[{side}=true]"));
        for base in bases() {
            let mut world = TestWorld::new();
            fill(&mut world, base, [-3, -1, -3], [3, -1, 3], STONE);
            world.set(at(base, stone[0], 0, stone[2]), STONE);
            world.set(at(base, 0, 0, 0), fence);
            let center = DVec3::new(0.5, 0.0, 0.5);
            // On the stone, a quarter block short of the fence's arm, walking toward the fence:
            // the plain move stops at the arm; the step-up lands with the box over both blocks,
            // on the fence's top.
            let start = center - dir + DVec3::Y;
            let d = 0.5 * dir;
            let plain = sweep(&mut world, base, start, d, StepUp::NONE);
            assert_eq!(local(base, plain.end), center - 0.75 * dir + DVec3::Y);
            let stepped = sweep(&mut world, base, start, d, WALK);
            let end = center - 0.5 * dir + 1.5 * DVec3::Y;
            assert_eq!(local(base, stepped.end), end, "{side} at {base}");
            assert!(stepped.on_ground());
            assert_eq!(stepped.ground, Some(at(base, 0, 0, 0)));
            clear_of_fences(&mut world, base, end);
            // Falling onto the same spot: on the fence's top too.
            let out = sweep(
                &mut world,
                base,
                end + 2.5 * DVec3::Y,
                DVec3::new(0.0, -6.0, 0.0),
                StepUp::NONE,
            );
            assert_eq!(local(base, out.end), end, "{side} at {base}");
            assert_eq!(out.ground, Some(at(base, 0, 0, 0)));
            // Standing there, one step of gravity keeps it there.
            let out = sweep(
                &mut world,
                base,
                end,
                DVec3::new(0.0, -0.078125, 0.0),
                StepUp::NONE,
            );
            assert_eq!(local(base, out.end), end, "{side} at {base}");
            assert!(out.on_ground());
        }
    }
}
