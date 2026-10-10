//! Walls and fence gates collide like fences (ADR 0027): 1.5 blocks tall, half a block into the
//! cell above, picked a block tall (13 sixteenths a gate between walls). An open gate lets a
//! walker through. Everything exact and the same bit for bit far from the origin.
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    CollisionShape, LocalBox, RayResult, StepUp, SweepConfig, SweepOut, TestWorld, VoxelFilter,
    move_box_stepping, raycast_voxels,
};
use boxel_voxel::{BlockPos, BlockState, Face, WorldPos};
use common::{STONE, at, fill, table};

fn state(name: &str) -> BlockState {
    boxel_demo::blocks::registry().parse_state(name).unwrap()
}

/// A straight wall along X, without a post (its middle bar: `z` from 5/16 to 11/16).
fn wall() -> BlockState {
    state("boxel:cobblestone_wall[east=low,west=low,up=false]")
}

/// A closed gate in a row along X (it faces south, between walls).
fn gate(open: bool) -> BlockState {
    state(&format!(
        "boxel:oak_fence_gate[facing=south,in_wall=true,open={open}]"
    ))
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

/// A stone floor at y = -1 around `base`, a wall row along X at z = 0 from x = -3 to 3 with a
/// gate at x = 1 (closed or open).
fn scene(base: I64Vec3, open: bool) -> TestWorld {
    let mut world = TestWorld::new();
    fill(&mut world, base, [-6, -1, -6], [6, -1, 6], STONE);
    fill(&mut world, base, [-3, 0, 0], [3, 0, 0], wall());
    world.set(at(base, 1, 0, 0), gate(open));
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
fn the_table_knows_walls_and_closed_gates_are_tall() {
    let t = table();
    // Picked as Minecraft: a low run 14/16 tall, a post a block, a gate between walls 13/16.
    let picked = [
        (wall(), 14.0 / 16.0),
        (state("boxel:cobblestone_wall"), 1.0),
        (gate(false), 13.0 / 16.0),
    ];
    for (s, top) in picked {
        let CollisionShape::Boxes(boxes) = t.shape(s) else {
            panic!("collides with boxes");
        };
        assert!(boxes.iter().all(|b| b.max.y == 1.5 && b.min.y == 0.0));
        assert!(t.is_tall(s));
        let CollisionShape::Boxes(outline) = t.outline(s) else {
            panic!("picked by boxes");
        };
        let highest = outline.iter().map(|b| b.max.y).fold(0.0, f64::max);
        assert_eq!(highest, top);
    }
    assert_eq!(t.shape(gate(true)), CollisionShape::Empty);
    assert!(!t.is_tall(gate(true)));
    assert!(matches!(t.outline(gate(true)), CollisionShape::Boxes(_)));
    assert_eq!(t.reach(), 0.5);
}

#[test]
fn walls_and_closed_gates_stop_a_walker_and_cannot_be_jumped() {
    let mut results = Vec::new();
    for base in bases() {
        let mut world = scene(base, false);
        let mut run = Vec::new();
        // Into the wall at x = -1.5 and into the gate at x = 1.5, plain and with the step-up:
        // stopped at the bar (z = 5/16) or the gate (z = 6/16), less the body's half width.
        for (x, stop) in [(-1.5, 0.0625), (1.5, 0.125)] {
            let start = DVec3::new(x, 0.0, -1.0);
            let d = DVec3::new(0.0, 0.0, 2.0);
            let plain = sweep(&mut world, base, start, d, StepUp::NONE);
            assert_eq!(sweep(&mut world, base, start, d, WALK), plain);
            assert_eq!(local(base, plain.end), DVec3::new(x, 0.0, stop));
            assert!(plain.touched(Face::PosZ));
            run.push(local(base, plain.end));
            // A jump peaks at 1.24 blocks; at 1.4375 the feet are still below the top.
            let high = sweep(
                &mut world,
                base,
                DVec3::new(x, 1.4375, -1.0),
                d,
                StepUp::NONE,
            );
            assert_eq!(local(base, high.end), DVec3::new(x, 1.4375, stop));
            // At 1.5 it goes over.
            let over = sweep(&mut world, base, DVec3::new(x, 1.5, -1.0), d, StepUp::NONE);
            assert_eq!(local(base, over.end), DVec3::new(x, 1.5, 1.0));
        }
        // Falling onto the wall: it lands on its top, 1.5 up.
        let fall = sweep(
            &mut world,
            base,
            DVec3::new(-2.5, 3.0, 0.5),
            DVec3::new(0.0, -3.0, 0.0),
            StepUp::NONE,
        );
        assert_eq!(local(base, fall.end), DVec3::new(-2.5, 1.5, 0.5));
        assert!(fall.touched(Face::NegY));
        run.push(local(base, fall.end));
        results.push(run);
    }
    assert!(results.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn an_open_gate_lets_a_walker_through() {
    for base in bases() {
        let mut world = scene(base, true);
        let start = DVec3::new(1.5, 0.0, -1.0);
        let d = DVec3::new(0.0, 0.0, 2.0);
        let out = sweep(&mut world, base, start, d, WALK);
        assert_eq!(local(base, out.end), DVec3::new(1.5, 0.0, 1.0));
        // Next to it, the wall still stops.
        let out = sweep(&mut world, base, DVec3::new(0.5, 0.0, -1.0), d, WALK);
        assert_eq!(local(base, out.end), DVec3::new(0.5, 0.0, 0.0625));
    }
}

#[test]
fn collision_rays_meet_the_tall_part_and_picking_does_not() {
    let t = table();
    for base in bases() {
        let mut world = scene(base, false);
        world.set(at(base, -2, 1, 4), STONE);
        let origin = |p: DVec3| WorldPos::from_block(BlockPos::from_i64vec3(base), p);
        let ray = |world: &mut TestWorld, filter| {
            raycast_voxels(
                world,
                &t,
                origin(DVec3::new(-1.5, 1.25, -2.5)),
                DVec3::Z,
                10.0,
                filter,
            )
        };
        let RayResult::Hit(hit) = ray(&mut world, VoxelFilter::Collision) else {
            panic!("the tall part");
        };
        assert_eq!(
            (hit.block, hit.face),
            (at(base, -2, 0, 0), Some(Face::NegZ))
        );
        assert_eq!(hit.t, 2.5 + 0.3125);
        // Picking goes over it to the stone behind.
        let RayResult::Hit(hit) = ray(&mut world, VoxelFilter::Outline { fluids: false }) else {
            panic!("the stone");
        };
        assert_eq!(hit.block, at(base, -2, 1, 4));
        // A gate between walls is picked 13/16 tall: a ray at 0.875 goes over it.
        let RayResult::Hit(hit) = raycast_voxels(
            &mut world,
            &t,
            origin(DVec3::new(1.5, 0.75, -2.5)),
            DVec3::Z,
            10.0,
            VoxelFilter::Outline { fluids: false },
        ) else {
            panic!("the gate");
        };
        assert_eq!((hit.block, hit.t), (at(base, 1, 0, 0), 2.5 + 0.375));
        let over = raycast_voxels(
            &mut world,
            &t,
            origin(DVec3::new(1.5, 0.875, -2.5)),
            DVec3::Z,
            5.0,
            VoxelFilter::Outline { fluids: false },
        );
        assert_eq!(over, RayResult::Miss);
    }
}
