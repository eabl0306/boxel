//! The per-axis AABB sweep (`move_box`): exact landing, the axis order, no snags at block and
//! chunk seams, resting without drift, depenetration, obstacles, fluids, unloaded chunks, the
//! world limit, degenerate inputs, and a proptest over random block fields.
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    CollisionTable, LocalBox, MAX_SWEEP_DISTANCE, Probe, Shape, SweepConfig, SweepOut, TestWorld,
    Unloaded, VoxelSource, axis_order, move_box,
};
use boxel_voxel::{BlockPos, ChunkPos, Face, MAX_BLOCK_COORD, WorldPos};
use common::{STONE, WATER, at, fill, player, random_field, table};
use proptest::prelude::*;

const EPS: f64 = 1e-7;

fn sweep(
    world: &mut TestWorld,
    start: WorldPos,
    shape: LocalBox,
    d: DVec3,
) -> Result<SweepOut, Unloaded> {
    move_box(
        world,
        &table(),
        start,
        shape,
        d,
        &[],
        &SweepConfig::default(),
    )
}

/// A 0.5 x 1.8 x 0.5 box with its origin at the feet: binary fractions, so expected positions
/// are exact.
fn narrow() -> LocalBox {
    LocalBox::new(DVec3::new(-0.25, 0.0, -0.25), DVec3::new(0.25, 1.8, 0.25))
}

/// A stone floor whose top is the plane y = `top`, over x and z in `-r..=r` around `base`.
fn floor(world: &mut TestWorld, base: I64Vec3, top: i64, r: i64) {
    fill(world, base, [-r, top - 1, -r], [r, top - 1, r], STONE);
}

#[test]
fn falling_boxes_land_exactly_on_the_block_plane() {
    for base in [
        I64Vec3::ZERO,
        I64Vec3::new(-1000, -1000, -1000),
        I64Vec3::new(31, -33, 64),
    ] {
        let mut world = TestWorld::new();
        floor(&mut world, base, 0, 3);
        for height in [0.1, 0.5, 0.999, 3.25, 7.0] {
            for frac in [0.05, 0.3, 0.5, 0.71] {
                let start =
                    WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(frac, height, 1.0 - frac));
                let out = sweep(&mut world, start, player(), DVec3::new(0.0, -20.0, 0.0))
                    .expect("loaded");
                // Exactly on the plane: block y = base y, frac 0, no rounding left.
                assert_eq!(out.end.block, at(base, 0, 0, 0), "{height} {frac}");
                assert_eq!(out.end.frac, DVec3::new(frac, 0.0, 1.0 - frac));
                assert_eq!(out.moved, DVec3::new(0.0, -height, 0.0));
                assert!(out.on_ground() && out.touched(Face::NegY));
                assert_eq!(out.faces, 1 << Face::NegY.index());
                assert_eq!(out.ground, Some(at(base, 0, -1, 0)));
                assert!(!out.in_fluid && !out.depenetrated);
            }
        }
    }
}

#[test]
fn the_axis_order_is_y_then_the_larger_horizontal_move() {
    assert_eq!(axis_order(DVec3::new(1.0, 5.0, 0.5)), [1, 0, 2]);
    assert_eq!(axis_order(DVec3::new(0.5, -5.0, -1.0)), [1, 2, 0]);
    assert_eq!(
        axis_order(DVec3::new(-1.0, 0.0, 1.0)),
        [1, 0, 2],
        "x on a tie"
    );

    // A two-block column at (1, 0..=1, 1) next to a 0.5-wide box at (0.5, 0, 0.5): the order
    // decides which axis the column stops (Minecraft's corner behavior, frozen here).
    let mut world = TestWorld::new();
    floor(&mut world, I64Vec3::ZERO, 0, 4);
    world.fill(BlockPos::new(1, 0, 1), BlockPos::new(1, 1, 1), STONE);
    let start = WorldPos::from_dvec3(DVec3::new(0.5, 0.0, 0.5));
    // |x| > |z|: X moves freely first (z footprint is block 0), then Z hits the column.
    let out = sweep(&mut world, start, narrow(), DVec3::new(0.5, 0.0, 0.375)).expect("loaded");
    assert_eq!(out.end.to_dvec3(), DVec3::new(1.0, 0.0, 0.75));
    assert_eq!(out.faces, 1 << Face::PosZ.index());
    // |z| > |x|: Z first, then X hits the column.
    let out = sweep(&mut world, start, narrow(), DVec3::new(0.375, 0.0, 0.5)).expect("loaded");
    assert_eq!(out.end.to_dvec3(), DVec3::new(0.75, 0.0, 1.0));
    assert_eq!(out.faces, 1 << Face::PosX.index());
}

/// One contact surface and a slide along it: the box is pressed into the surface by `press`
/// along the surface normal and moves by `step` along a tangent, 100 times.
#[test]
fn sliding_along_any_surface_never_snags_on_seams() {
    // Around x = -32, z = 32, y = -64: chunk borders in the middle of the slides.
    let base = I64Vec3::new(-32, -64, 32);
    // (surface normal axis, side: +1 means the surface is on the box's + side)
    for (normal, side) in [(1usize, -1i64), (1, 1), (0, -1), (0, 1), (2, -1), (2, 1)] {
        let mut world = TestWorld::new();
        // A slab of stone two blocks from the start, 81 blocks wide on the tangent axes.
        let mut min = [-40, -40, -40];
        let mut max = [40, 40, 40];
        let at_side = if side > 0 { 2 } else { -3 };
        min[normal] = at_side;
        max[normal] = at_side;
        fill(&mut world, base, min, max, STONE);
        // The box (0.5 x 0.5 x 0.5 around its origin) starts touching the slab.
        let shape = LocalBox::new(DVec3::splat(-0.25), DVec3::splat(0.25));
        let mut start = DVec3::splat(0.5);
        start[normal] = if side > 0 { 2.0 - 0.25 } else { -2.0 + 0.25 };
        for tangent in [0usize, 1, 2].into_iter().filter(|&t| t != normal) {
            for sign in [-1.0, 1.0] {
                let mut pos = WorldPos::from_block(at(base, 0, 0, 0), start);
                let mut d = DVec3::ZERO;
                d[normal] = 0.01 * side as f64;
                d[tangent] = 0.37 * sign;
                let face_bit = {
                    let probe = sweep(&mut world, pos, shape, d).expect("loaded");
                    probe.faces
                };
                for i in 0..100 {
                    let out = sweep(&mut world, pos, shape, d).expect("loaded");
                    assert_eq!(
                        out.faces, face_bit,
                        "normal {normal} side {side} tangent {tangent} sign {sign} step {i}"
                    );
                    assert!(
                        (out.moved[tangent] - d[tangent]).abs() < 1e-12,
                        "no snag at step {i}: {:?}",
                        out.moved
                    );
                    assert_eq!(out.moved[normal], 0.0, "pressed but not moved");
                    pos = out.end;
                }
                assert_eq!(face_bit.count_ones(), 1, "only the pressed face touches");
                // 100 steps of 0.37 crossed several blocks and a chunk border.
                let traveled = WorldPos::from_block(at(base, 0, 0, 0), start).delta_to(&pos);
                assert!(
                    (traveled[tangent] - 37.0 * sign).abs() < 1e-9,
                    "{traveled:?}"
                );
            }
        }
    }
}

#[test]
fn a_resting_box_stays_bit_identical_for_10000_steps() {
    let mut world = TestWorld::new();
    let base = I64Vec3::new(-7, 40, 1 << 20);
    floor(&mut world, base, 0, 2);
    let table = table();
    let cfg = SweepConfig::default();
    // One step of gravity at 64 Hz from rest: 32 * (1/64)^2 blocks.
    let fall = DVec3::new(0.0, -32.0 / 4096.0, 0.0);
    let start = WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(0.3, 0.0, 0.7));
    let mut pos = start;
    for _ in 0..10_000 {
        let out = move_box(&mut world, &table, pos, player(), fall, &[], &cfg).expect("loaded");
        assert!(out.on_ground());
        pos = out.end;
        assert_eq!(pos.block, start.block);
        assert_eq!(
            pos.frac.to_array().map(f64::to_bits),
            start.frac.to_array().map(f64::to_bits)
        );
    }
}

#[test]
fn a_box_inside_a_block_is_pushed_toward_the_nearest_free_side() {
    let shape = narrow();
    let none = DVec3::ZERO;

    // A block placed at the feet: up (0.5) beats the sides (0.75).
    let mut world = TestWorld::new();
    world.set(BlockPos::new(0, 0, 0), STONE);
    let start = WorldPos::from_dvec3(DVec3::new(0.5, 0.5, 0.5));
    let out = sweep(&mut world, start, shape, none).expect("loaded");
    assert!(out.depenetrated);
    assert_eq!(out.end.to_dvec3(), DVec3::new(0.5, 1.0, 0.5));
    assert_eq!(out.moved, DVec3::new(0.0, 0.5, 0.0));

    // Up and +X are both 0.5 away: +Y wins the tie.
    let start = WorldPos::from_dvec3(DVec3::new(0.75, 0.5, 0.5));
    let out = sweep(&mut world, start, shape, none).expect("loaded");
    assert_eq!(out.end.to_dvec3(), DVec3::new(0.75, 1.0, 0.5));

    // A two-block column with a floor and a ceiling: only +X is free within one block.
    let mut world = TestWorld::new();
    world.fill(BlockPos::new(-3, -1, -3), BlockPos::new(3, -1, 3), STONE);
    world.fill(BlockPos::new(-3, 2, -3), BlockPos::new(3, 2, 3), STONE);
    world.fill(BlockPos::new(0, 0, 0), BlockPos::new(0, 1, 0), STONE);
    let start = WorldPos::from_dvec3(DVec3::new(1.125, 0.0, 0.5));
    let out = sweep(&mut world, start, shape, none).expect("loaded");
    assert!(out.depenetrated);
    assert_eq!(
        out.end.to_dvec3(),
        DVec3::new(1.25, 0.0, 0.5),
        "the face lands on x = 1"
    );
    assert_eq!(
        out.moved,
        DVec3::new(0.125, 0.0, 0.0),
        "exactly the overlap"
    );

    // Buried: no free side within one block, so one block straight up.
    let mut world = TestWorld::new();
    world.fill(BlockPos::new(-3, -3, -3), BlockPos::new(3, 3, 3), STONE);
    let start = WorldPos::from_dvec3(DVec3::new(0.5, 0.0, 0.5));
    let out = sweep(&mut world, start, shape, none).expect("loaded");
    assert!(out.depenetrated);
    assert_eq!(out.end.to_dvec3(), DVec3::new(0.5, 1.0, 0.5));
}

#[test]
fn unloaded_chunks_stop_the_move_and_missing_space_is_air() {
    let mut world = TestWorld::new();
    floor(&mut world, I64Vec3::ZERO, -40, 3);
    world.set_unloaded(ChunkPos::new(0, -1, 0), true);
    let start = WorldPos::from_dvec3(DVec3::new(0.5, 2.0, 0.5));
    // Falling 1.5 blocks stays in chunk y = 0: loaded, air below.
    let out = sweep(&mut world, start, player(), DVec3::new(0.0, -1.5, 0.0)).expect("loaded");
    assert_eq!(out.end.to_dvec3(), DVec3::new(0.5, 0.5, 0.5));
    assert!(!out.on_ground());
    // Falling 3 blocks needs the layer y = -1, in the unloaded chunk.
    let err = sweep(&mut world, start, player(), DVec3::new(0.0, -3.0, 0.0)).unwrap_err();
    assert_eq!(
        err,
        Unloaded {
            block: BlockPos::new(0, -1, 0)
        }
    );
    // Loaded again: it falls through the air to the floor at -40.
    world.set_unloaded(ChunkPos::new(0, -1, 0), false);
    let out = sweep(&mut world, start, player(), DVec3::new(0.0, -50.0, 0.0)).expect("loaded");
    assert_eq!(out.end.to_dvec3(), DVec3::new(0.5, -40.0, 0.5));
}

#[test]
fn obstacles_clip_like_blocks_but_are_not_ground() {
    let mut world = TestWorld::new();
    floor(&mut world, I64Vec3::ZERO, 0, 6);
    // A crate (an obstacle box, in world coordinates here) next to the start, on the +X side.
    let crate_box = LocalBox::new(DVec3::new(1.0625, 0.0, 0.0), DVec3::new(2.0625, 1.0, 1.0));
    // Obstacles are given in the frame of the start's block.
    let run = |world: &mut TestWorld, start: WorldPos, d: DVec3| {
        let frame = start.block.as_i64vec3().as_dvec3();
        move_box(
            world,
            &table(),
            start,
            narrow(),
            d,
            &[crate_box.translate(-frame)],
            &SweepConfig::default(),
        )
        .expect("loaded")
    };
    let out = run(
        &mut world,
        WorldPos::from_dvec3(DVec3::new(0.5, 0.0, 0.5)),
        DVec3::new(2.0, 0.0, 0.0),
    );
    assert_eq!(out.end.to_dvec3(), DVec3::new(0.8125, 0.0, 0.5));
    assert!(out.touched(Face::PosX));

    // Falling onto the crate: lands on its top, on_ground, but no ground block.
    let out = run(
        &mut world,
        WorldPos::from_dvec3(DVec3::new(1.5, 3.0, 0.5)),
        DVec3::new(0.0, -5.0, 0.0),
    );
    assert_eq!(out.end.to_dvec3(), DVec3::new(1.5, 1.0, 0.5));
    assert!(out.on_ground());
    assert_eq!(out.ground, None);

    // A box already overlapping an obstacle can move out of it.
    let out = run(
        &mut world,
        WorldPos::from_dvec3(DVec3::new(1.5, 0.0, 0.5)),
        DVec3::new(-1.0, 0.0, 0.0),
    );
    assert_eq!(out.moved.x, -1.0);
}

#[test]
fn ground_is_the_block_under_the_center_or_the_first_one_found() {
    let mut world = TestWorld::new();
    // Only block (1, -1, 0) supports a box centered at x = 0.9 (its footprint is x 0..=1).
    world.set(BlockPos::new(1, -1, 0), STONE);
    let start = WorldPos::from_dvec3(DVec3::new(0.9, 2.0, 0.5));
    let out = sweep(&mut world, start, player(), DVec3::new(0.0, -4.0, 0.0)).expect("loaded");
    assert!(out.on_ground());
    assert_eq!(
        out.ground,
        Some(BlockPos::new(1, -1, 0)),
        "first full block found"
    );
    // With the block under the center full too, the center one wins.
    world.set(BlockPos::new(0, -1, 0), STONE);
    world.set(BlockPos::new(1, -1, 0), STONE);
    let start = WorldPos::from_dvec3(DVec3::new(1.2, 2.0, 0.5));
    let out = sweep(&mut world, start, player(), DVec3::new(0.0, -4.0, 0.0)).expect("loaded");
    assert_eq!(
        out.ground,
        Some(BlockPos::new(1, -1, 0)),
        "the block under the center"
    );
}

#[test]
fn fluids_are_reported_and_never_collide() {
    let mut world = TestWorld::new();
    floor(&mut world, I64Vec3::ZERO, -3, 3);
    fill(&mut world, I64Vec3::ZERO, [-3, -3, -3], [3, 0, 3], WATER);
    let start = WorldPos::from_dvec3(DVec3::new(0.5, 3.0, 0.5));
    let out = sweep(&mut world, start, player(), DVec3::new(0.0, -1.5, 0.0)).expect("loaded");
    assert!(!out.in_fluid, "still above the water (y = 1.5)");
    let out = sweep(&mut world, start, player(), DVec3::new(0.0, -10.0, 0.0)).expect("loaded");
    assert!(out.in_fluid && out.on_ground());
    assert_eq!(
        out.end.to_dvec3().y,
        -3.0,
        "through the water onto the floor"
    );
}

#[test]
fn bad_displacements_are_sanitized() {
    let mut world = TestWorld::new();
    let start = WorldPos::from_dvec3(DVec3::new(0.5, 0.0, 0.5));
    let out = sweep(
        &mut world,
        start,
        player(),
        DVec3::new(f64::NAN, f64::INFINITY, 0.25),
    )
    .expect("loaded");
    assert_eq!(out.moved, DVec3::new(0.0, 0.0, 0.25));
    let out = sweep(&mut world, start, player(), DVec3::new(1e9, 0.0, 0.0)).expect("loaded");
    assert_eq!(out.moved.x, MAX_SWEEP_DISTANCE);
}

#[test]
fn obstacles_stop_the_scan_before_unloaded_chunks() {
    let mut world = TestWorld::new();
    world.set_unloaded(ChunkPos::new(1, 0, 0), true);
    world.set_unloaded(ChunkPos::new(0, -1, 0), true);
    let run = |world: &mut TestWorld, start: WorldPos, d: DVec3, obstacle: Option<LocalBox>| {
        let frame = start.block.as_i64vec3().as_dvec3();
        let obstacles: Vec<LocalBox> = obstacle.into_iter().map(|o| o.translate(-frame)).collect();
        move_box(
            world,
            &table(),
            start,
            narrow(),
            d,
            &obstacles,
            &SweepConfig::default(),
        )
    };
    // +X: a crate 0.25 ahead, the unloaded chunk x >= 32 0.375 ahead. The box stops at the
    // crate and never needs the chunk behind it. It moves half a block up: lower, a fence in
    // the unloaded row under it could reach it (ADR 0026), and the move would need that row.
    let start = WorldPos::from_dvec3(DVec3::new(31.375, 0.5, 0.5));
    let crate_box = LocalBox::new(DVec3::new(31.875, 0.0, 0.0), DVec3::new(32.5, 1.0, 1.0));
    let low = WorldPos::from_dvec3(DVec3::new(31.375, 0.0, 0.5));
    assert_eq!(
        run(&mut world, low, DVec3::new(0.75, 0.0, 0.0), Some(crate_box)).unwrap_err(),
        Unloaded {
            block: BlockPos::new(31, -1, 0)
        },
        "the row under the box"
    );
    let d = DVec3::new(0.75, 0.0, 0.0);
    assert_eq!(
        run(&mut world, start, d, None).unwrap_err(),
        Unloaded {
            block: BlockPos::new(32, 0, 0)
        },
        "nothing in front: the move needs the chunk"
    );
    let out = run(&mut world, start, d, Some(crate_box)).expect("the crate is nearer");
    assert_eq!(out.end.to_dvec3().x, 31.625);
    assert!(out.touched(Face::PosX));
    // -Y: a crate top at y = 0.75 under the box, the unloaded chunk y < 0 below it (a fence
    // there could rise to 0.5, ADR 0026: a crate under that would not spare the chunk).
    let start = WorldPos::from_dvec3(DVec3::new(0.5, 1.0, 0.5));
    let crate_box = LocalBox::new(DVec3::new(0.0, -1.0, 0.0), DVec3::new(1.0, 0.75, 1.0));
    let d = DVec3::new(0.0, -1.0, 0.0);
    assert_eq!(
        run(&mut world, start, d, None).unwrap_err(),
        Unloaded {
            block: BlockPos::new(0, -1, 0)
        }
    );
    let out = run(&mut world, start, d, Some(crate_box)).expect("the crate is nearer");
    assert_eq!(out.end.to_dvec3().y, 0.75);
    assert!(out.on_ground());
    assert_eq!(out.ground, None);
    // A block flush with the crate top is still scanned: it is the ground.
    world.set_unloaded(ChunkPos::new(0, -1, 0), false);
    world.set(BlockPos::new(0, -1, 0), STONE);
    let flush = LocalBox::new(DVec3::new(0.0, -1.0, 0.0), DVec3::new(1.0, 0.0, 1.0));
    let out = run(&mut world, start, 1.5 * d, Some(flush)).expect("loaded");
    assert_eq!(out.end.to_dvec3().y, 0.0);
    assert_eq!(out.ground, Some(BlockPos::new(0, -1, 0)));
}

#[test]
fn moves_past_the_world_limit_stop_on_it() {
    let mut world = TestWorld::new();
    let last = MAX_BLOCK_COORD + 31;
    // 0.9 + 0.2 in the last block: the end is clamped just below the limit and `moved` says
    // what happened (before, the end wrapped to frac 0.1 while `moved` said +0.2).
    let start = WorldPos::from_block(BlockPos::new(last, 0, 0), DVec3::new(0.9, 0.5, 0.5));
    let out = sweep(&mut world, start, narrow(), DVec3::new(0.2, 0.0, 0.0)).expect("loaded");
    assert_eq!(out.end.block, start.block);
    assert_eq!(out.end.frac.x, 1.0 - f64::EPSILON / 2.0);
    assert_eq!(out.moved, start.delta_to(&out.end));
    assert!(out.moved.x > 0.09 && out.moved.x < 0.1, "{:?}", out.moved);
    // The negative side stops on the min corner of the first block.
    let start = WorldPos::from_block(
        BlockPos::new(-MAX_BLOCK_COORD, 0, 0),
        DVec3::new(0.125, 0.5, 0.5),
    );
    let out = sweep(&mut world, start, narrow(), DVec3::new(-2.0, 0.0, 0.0)).expect("loaded");
    assert_eq!((out.end.block, out.end.frac.x), (start.block, 0.0));
    assert_eq!(out.moved.x, -0.125);
}

/// Every block is stone, everywhere.
struct Solid;

impl VoxelSource for Solid {
    fn probe(&mut self, _: BlockPos) -> Probe {
        Probe::Block(STONE)
    }
}

#[test]
fn degenerate_shapes_starts_and_configs_are_sanitized() {
    let mut world = TestWorld::new();
    floor(&mut world, I64Vec3::ZERO, 0, 3);
    fill(&mut world, I64Vec3::ZERO, [3, 0, -3], [3, 4, 3], STONE);
    fill(&mut world, I64Vec3::ZERO, [-3, 5, -3], [3, 5, 3], STONE);
    // Thinner than 2 eps (a tiny sphere, a point, a NaN box): widened to 4 eps around the
    // center, so they land, hit walls and ceilings and rest there instead of slipping through.
    let shapes = [
        Shape::Sphere { radius: 1e-9 }.bounds(),
        LocalBox::default(),
        LocalBox::new(DVec3::NAN, DVec3::ONE),
    ];
    for shape in shapes {
        for x in [0.5, 1.0] {
            let start = WorldPos::from_dvec3(DVec3::new(x, 3.0, 0.5));
            let out = sweep(&mut world, start, shape, DVec3::new(0.0, -5.0, 0.0)).expect("loaded");
            assert!(out.on_ground(), "{shape:?} at x = {x}");
            assert!((out.end.to_dvec3().y - 2e-7).abs() < 1e-12, "{:?}", out.end);
            let out = sweep(&mut world, start, shape, DVec3::new(5.0, 0.0, 0.0)).expect("loaded");
            assert!(out.touched(Face::PosX) && out.end.to_dvec3().x < 3.0);
            let up = sweep(&mut world, start, shape, DVec3::new(0.0, 5.0, 0.0)).expect("loaded");
            assert!(up.touched(Face::PosY) && up.end.to_dvec3().y < 5.0);
            let rest = sweep(&mut world, up.end, shape, DVec3::ZERO).expect("loaded");
            assert!(!rest.depenetrated && rest.end == up.end, "{shape:?}");
        }
    }
    // A start outside the canonical form is normalized first, like the raycast's origin.
    let canonical = WorldPos::from_dvec3(DVec3::new(0.5, 3.25, 0.5));
    let reference = sweep(&mut world, canonical, player(), DVec3::new(0.0, -5.0, 0.0));
    let hand_built = WorldPos {
        block: BlockPos::new(0, 1, 0),
        frac: DVec3::new(0.5, 2.25, 0.5),
    };
    assert_eq!(
        sweep(&mut world, hand_built, player(), DVec3::new(0.0, -5.0, 0.0)),
        reference
    );
    let nan = WorldPos {
        block: BlockPos::new(0, 3, 0),
        frac: DVec3::new(0.5, f64::NAN, 0.5),
    };
    assert_eq!(
        sweep(&mut world, nan, player(), DVec3::ZERO)
            .expect("loaded")
            .end
            .frac
            .y,
        0.0
    );
    // A block past the i64 room of the frame: clamped to the world limit, no overflow.
    let far = WorldPos {
        block: BlockPos::new(i64::MAX, i64::MIN, 0),
        frac: DVec3::splat(0.5),
    };
    let out = sweep(&mut world, far, player(), DVec3::ONE).expect("loaded");
    assert_eq!(out.end.block.x, MAX_BLOCK_COORD + 31);
    // Configs: NaN, negative and infinite values are clamped; an infinite push-out inside
    // solid stone used to search for ever.
    let start = WorldPos::from_dvec3(DVec3::new(0.5, 0.5, 0.5));
    for (eps, max_depenetration, pushed) in [
        (f64::NAN, f64::INFINITY, MAX_SWEEP_DISTANCE),
        (-1.0, f64::NAN, 0.0),
        (f64::INFINITY, -3.0, 0.0),
        (1e-7, 2.0, 2.0),
    ] {
        let cfg = SweepConfig {
            eps,
            max_depenetration,
        };
        let out = move_box(
            &mut Solid,
            &table(),
            start,
            player(),
            DVec3::ZERO,
            &[],
            &cfg,
        )
        .expect("loaded");
        assert!(out.depenetrated);
        assert_eq!(out.moved, DVec3::new(0.0, pushed, 0.0), "{cfg:?}");
    }
}

// ---------------------------------------------------------------- proptest

/// Full blocks overlapping `bx` by more than `eps` on every axis.
fn overlaps_full(
    world: &mut TestWorld,
    table: &CollisionTable,
    bx: LocalBox,
    origin: BlockPos,
) -> bool {
    let lo = |v: f64| (v + EPS).floor() as i64;
    let hi = |v: f64| (v - EPS).ceil() as i64 - 1;
    for y in lo(bx.min.y)..=hi(bx.max.y) {
        for x in lo(bx.min.x)..=hi(bx.max.x) {
            for z in lo(bx.min.z)..=hi(bx.max.z) {
                let p = BlockPos::new(origin.x + x, origin.y + y, origin.z + z);
                if let Probe::Block(s) = world.probe(p)
                    && table.is_full(s)
                {
                    return true;
                }
            }
        }
    }
    false
}

fn displacement() -> impl Strategy<Value = f64> {
    prop_oneof![-50.0..50.0f64, -2.0..2.0f64, Just(0.0)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Random fields (0 to 60% stone, across chunk borders), random boxes and starts, moves up
    /// to 50 blocks per axis.
    #[test]
    fn sweeps_never_end_inside_blocks_and_never_cross_them(
        seed in any::<u64>(),
        density in 0.0..0.6f64,
        half in (0.1..1.2f64, 0.1..1.2f64, 0.1..1.2f64),
        start in (0.0..12.0f64, 0.0..12.0f64, 0.0..12.0f64),
        d in (displacement(), displacement(), displacement()),
    ) {
        // The field spans x -35..-23, y -3..9, z 29..41: chunk borders at x = -32, y = 0, z = 32.
        let base = I64Vec3::new(-35, -3, 29);
        let mut world = random_field(base, 12, density, seed);
        let table = table();
        let shape = LocalBox::from_center_half(DVec3::ZERO, DVec3::new(half.0, half.1, half.2));
        let start = WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(start.0, start.1, start.2));
        // Carve the start out of the field so it starts free.
        let origin = start.block;
        let bx = shape.translate(start.frac);
        for y in bx.min.y.floor() as i64..bx.max.y.ceil() as i64 {
            for x in bx.min.x.floor() as i64..bx.max.x.ceil() as i64 {
                for z in bx.min.z.floor() as i64..bx.max.z.ceil() as i64 {
                    world.set(BlockPos::new(origin.x + x, origin.y + y, origin.z + z), boxel_voxel::BlockState::AIR);
                }
            }
        }
        let d = DVec3::new(d.0, d.1, d.2);
        let out = move_box(&mut world, &table, start, shape, d, &[], &SweepConfig::default())
            .expect("all loaded");
        prop_assert!(!out.depenetrated);

        // Never ends overlapping a full block by more than eps.
        let end_rel = start.delta_to(&out.end);
        prop_assert!(!overlaps_full(&mut world, &table, shape.translate(start.frac + end_rel), origin));

        // Per axis: at most |d|, same sign (a snap may move back by eps); the swept box of each
        // axis segment never overlaps a full block.
        let mut pos = start.frac;
        for axis in axis_order(d) {
            let moved = out.moved[axis];
            prop_assert!(moved.abs() <= d[axis].abs() + EPS, "axis {} moved {} of {}", axis, moved, d[axis]);
            prop_assert!(moved * d[axis].signum() >= -EPS, "axis {} moved {} of {}", axis, moved, d[axis]);
            let mut after = pos;
            after[axis] += moved;
            let mut swept = shape.translate(pos);
            swept.min[axis] = pos[axis].min(after[axis]) + shape.min[axis];
            swept.max[axis] = pos[axis].max(after[axis]) + shape.max[axis];
            prop_assert!(!overlaps_full(&mut world, &table, swept, origin), "axis {} crossed a block", axis);
            // A clipped axis has its face bit, and that face really touches: the box moved
            // 2 eps further overlaps a full block.
            let face = match (axis, d[axis] > 0.0) {
                (0, true) => Face::PosX, (0, false) => Face::NegX,
                (1, true) => Face::PosY, (1, false) => Face::NegY,
                (_, true) => Face::PosZ, (_, false) => Face::NegZ,
            };
            let clipped = (moved - d[axis]).abs() > EPS;
            prop_assert_eq!(out.touched(face), clipped && d[axis] != 0.0, "axis {}", axis);
            if out.touched(face) {
                let mut probe = after;
                probe[axis] += 2.0 * EPS * d[axis].signum();
                prop_assert!(overlaps_full(&mut world, &table, shape.translate(probe), origin));
            }
            pos = after;
        }
        prop_assert_eq!(pos, start.frac + out.moved);
    }
}
