//! Step-up (ADR 0016, P5): walking bodies climb ledges up to their step height without jumping,
//! as in Minecraft. First the pure sweep (`move_box_stepping`): slabs, full blocks, stairs of
//! slabs, ceilings, obstacles, the ground rule, unloaded chunks, a proptest and the far-coords
//! check; then characters in an `App` (`CharacterController::step_height`).
mod common;
mod sim;

use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BodySettings, CharacterController, Collider, Contacts, LocalBox, MoveIntent, OnBlock,
    Projectile, ProjectileTarget, RigidBody, StepUp, SweepConfig, SweepOut, Teleport, TestWorld,
    Velocity, move_box, move_box_stepping, move_box_stepping_unpruned,
};
use boxel_voxel::{BlockPos, BlockState, ChunkPos, Face, MAX_BLOCK_COORD, WorldPos};
use boxel_world::ChunkMap;
use common::{Rng, STONE, at, bottom_slab, fill, table, top_slab};
use proptest::prelude::*;
use sim::{FRAME_144, Hits, STEP, Sim};

/// A 0.5 x 1.75 x 0.5 box with its origin at the feet (binary fractions: exact positions).
fn body() -> LocalBox {
    LocalBox::new(DVec3::new(-0.25, 0.0, -0.25), DVec3::new(0.25, 1.75, 0.25))
}

/// One step of walking +X: 1/16 of a block across and one step of resting gravity down.
const WALK: DVec3 = DVec3::new(0.0625, -1.0 / 128.0, 0.0);

/// A stone floor under `base` (its top at `base.y`).
fn floor(base: I64Vec3) -> TestWorld {
    let mut world = TestWorld::new();
    fill(&mut world, base, [-4, -1, -4], [16, -1, 4], STONE);
    world
}

/// One move from `start` by `d` with a step-up of `height`.
fn stepping(
    world: &mut TestWorld,
    start: WorldPos,
    d: DVec3,
    obstacles: &[LocalBox],
    step: StepUp,
) -> SweepOut {
    move_box_stepping(
        world,
        &table(),
        start,
        body(),
        d,
        obstacles,
        &SweepConfig::default(),
        step,
    )
    .expect("loaded")
}

/// `steps` moves by `d` from `start`, each one grounded if the previous one landed (the first
/// one is), with a step-up of `height`. `obstacles` are in the frame of `start.block`.
fn walk(
    world: &mut TestWorld,
    start: WorldPos,
    d: DVec3,
    obstacles: &[LocalBox],
    height: f64,
    steps: usize,
) -> Vec<SweepOut> {
    let origin = start.block.as_i64vec3();
    let mut pos = start;
    let mut grounded = true;
    (0..steps)
        .map(|_| {
            // The obstacles in the frame of this move's block.
            let shift = (origin - pos.block.as_i64vec3()).as_dvec3();
            let local: Vec<LocalBox> = obstacles.iter().map(|o| o.translate(shift)).collect();
            let out = stepping(world, pos, d, &local, StepUp { height, grounded });
            pos = out.end;
            grounded = out.on_ground();
            out
        })
        .collect()
}

/// The feet at `base + (x, y, z)`.
fn feet(base: I64Vec3, x: f64, y: f64, z: f64) -> WorldPos {
    WorldPos::from_block(BlockPos::from_i64vec3(base), DVec3::new(x, y, z))
}

/// The height of `p` above `base.y`, exact for the small heights of these tests.
fn height(base: I64Vec3, p: WorldPos) -> f64 {
    (p.block.y - base.y) as f64 + p.frac.y
}

/// Where `p` is across, relative to `base.x`.
fn along(base: I64Vec3, p: WorldPos) -> f64 {
    (p.block.x - base.x) as f64 + p.frac.x
}

#[test]
fn walking_into_a_bottom_slab_steps_up_onto_it() {
    let base = I64Vec3::new(-40, 7, 1000);
    let mut world = floor(base);
    world.set(at(base, 1, 0, 0), bottom_slab());
    let outs = walk(&mut world, feet(base, 0.5, 0.0, 0.5), WALK, &[], 0.6, 8);
    // Four moves bring the box flush with the slab (touching is not a clip).
    for out in &outs[..4] {
        assert_eq!(height(base, out.end), 0.0);
        assert!(!out.touched(Face::PosX));
    }
    assert_eq!(along(base, outs[3].end), 0.75);
    // The fifth goes over it: the whole move across, exactly on its top, the slab as ground.
    let up = &outs[4];
    assert_eq!(height(base, up.end), 0.5, "{up:?}");
    assert_eq!(up.moved.x, 0.0625);
    assert_eq!(up.moved.y, 0.5);
    assert!(up.on_ground() && !up.touched(Face::PosX) && !up.touched(Face::PosY));
    assert_eq!(up.ground, Some(at(base, 1, 0, 0)));
    assert_eq!(up.blocks, [None, Some(at(base, 1, 0, 0)), None]);
    // Then it walks on the slab.
    for out in &outs[5..] {
        assert_eq!(height(base, out.end), 0.5);
        assert_eq!(out.moved.x, 0.0625);
    }
}

#[test]
fn a_full_block_is_never_stepped() {
    let base = I64Vec3::new(3, -70, -9);
    let mut world = floor(base);
    world.set(at(base, 1, 0, 0), STONE);
    let outs = walk(&mut world, feet(base, 0.5, 0.0, 0.5), WALK, &[], 0.6, 10);
    let last = outs.last().expect("moves");
    assert_eq!(along(base, last.end), 0.75);
    assert_eq!(height(base, last.end), 0.0);
    assert!(last.touched(Face::PosX) && last.on_ground());
    assert_eq!(last.blocks[0], Some(at(base, 1, 0, 0)));
    // The same as without a step-up, bit for bit.
    for (i, out) in outs.iter().enumerate() {
        let start = if i == 0 {
            feet(base, 0.5, 0.0, 0.5)
        } else {
            outs[i - 1].end
        };
        let plain = move_box(
            &mut world,
            &table(),
            start,
            body(),
            WALK,
            &[],
            &SweepConfig::default(),
        )
        .expect("loaded");
        assert_eq!(*out, plain);
    }
    // A step height of one block climbs it.
    let outs = walk(&mut world, feet(base, 0.5, 0.0, 0.5), WALK, &[], 1.0, 10);
    assert_eq!(height(base, outs[9].end), 1.0);
}

/// A stair of slabs and full blocks up +X from x = 1, half a block per step, then a plateau
/// three blocks up, filled underneath.
fn stairs(base: I64Vec3) -> TestWorld {
    let mut world = floor(base);
    for i in 0..5 {
        let x = 1 + i;
        let y = i / 2;
        if y > 0 {
            fill(&mut world, base, [x, 0, -4], [x, y - 1, 4], STONE);
        }
        let top = if i % 2 == 0 { bottom_slab() } else { STONE };
        fill(&mut world, base, [x, y, -4], [x, y, 4], top);
    }
    fill(&mut world, base, [6, 0, -4], [16, 2, 4], STONE);
    world
}

#[test]
fn a_stair_of_slabs_is_climbed_half_a_block_at_a_time() {
    let base = I64Vec3::new(64, -3, -512);
    let mut world = stairs(base);
    let outs = walk(&mut world, feet(base, 0.5, 0.0, 0.5), WALK, &[], 0.6, 120);
    let mut heights = vec![0.0];
    for out in &outs {
        // Never stopped across, always on the ground.
        assert_eq!(out.moved.x, 0.0625, "{out:?}");
        assert!(out.on_ground() && !out.touched(Face::PosX), "{out:?}");
        let h = height(base, out.end);
        if h != *heights.last().expect("one") {
            heights.push(h);
        }
    }
    assert_eq!(heights, [0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0]);
    assert_eq!(along(base, outs[119].end), 8.0);
}

#[test]
fn no_step_into_a_ceiling_or_through_a_gap_too_low() {
    let base = I64Vec3::new(-1, 33, 7);
    let start = feet(base, 0.5, 0.0, 0.5);
    let stopped = |world: &mut TestWorld| {
        let outs = walk(world, start, WALK, &[], 0.6, 10);
        let last = outs.last().expect("moves");
        (along(base, last.end), height(base, last.end))
    };
    // A block over the slab leaves 1.5 blocks of room on it: the 1.75 body does not fit.
    let mut world = floor(base);
    world.set(at(base, 1, 0, 0), bottom_slab());
    world.set(at(base, 1, 2, 0), STONE);
    assert_eq!(stopped(&mut world), (0.75, 0.0));
    // One block higher it fits, and steps up.
    world.set(at(base, 1, 2, 0), BlockState::AIR);
    world.set(at(base, 1, 3, 0), STONE);
    assert_eq!(
        height(base, walk(&mut world, start, WALK, &[], 0.6, 5)[4].end),
        0.5
    );
    // A ceiling right over the head lets it rise only a quarter: not enough for the slab.
    world.set(at(base, 0, 2, 0), STONE);
    assert_eq!(stopped(&mut world), (0.75, 0.0));
    // The second try: with a step of one block, the first lift (1 block) bumps the top slab over
    // the ledge, so it rises only to that slab (0.75) and still clears the bottom slab.
    let mut world = floor(base);
    world.set(at(base, 1, 0, 0), bottom_slab());
    world.set(at(base, 1, 2, 0), top_slab());
    let outs = walk(&mut world, start, WALK, &[], 1.0, 6);
    assert_eq!(height(base, outs[4].end), 0.5);
    assert_eq!(outs[4].moved.x, 0.0625);
    assert!(outs[4].on_ground());
}

#[test]
fn obstacles_are_ledges_and_ceilings_too() {
    let base = I64Vec3::new(5, 5, 5);
    let start = feet(base, 0.5, 0.0, 0.5);
    let ledge = |top: f64| LocalBox::new(DVec3::new(1.0, 0.0, -1.0), DVec3::new(2.0, top, 2.0));
    // A static box half a block high is stepped onto; one a block high is not.
    let mut world = floor(base);
    let outs = walk(&mut world, start, WALK, &[ledge(0.5)], 0.6, 6);
    assert_eq!(height(base, outs[4].end), 0.5);
    assert!(outs[4].on_ground());
    assert_eq!(outs[4].ground, None, "an obstacle is not a block");
    let outs = walk(&mut world, start, WALK, &[ledge(1.0)], 0.6, 6);
    assert_eq!(
        (along(base, outs[5].end), height(base, outs[5].end)),
        (0.75, 0.0)
    );
    // A box over the slab, 2.3125 up: the full lift bumps it, the lower one (0.5625) clears the
    // slab and the body fits under it.
    world.set(at(base, 1, 0, 0), bottom_slab());
    let roof = LocalBox::new(DVec3::new(1.0, 2.3125, -1.0), DVec3::new(2.0, 3.0, 2.0));
    let outs = walk(&mut world, start, WALK, &[roof], 0.6, 6);
    assert_eq!(height(base, outs[4].end), 0.5);
    assert_eq!(outs[4].moved.x, 0.0625);
    // 2.25 up it no longer fits.
    let roof = LocalBox::new(
        DVec3::new(1.0, 2.25 - 1.0 / 64.0, -1.0),
        DVec3::new(2.0, 3.0, 2.0),
    );
    let outs = walk(&mut world, start, WALK, &[roof], 0.6, 6);
    assert_eq!(
        (along(base, outs[5].end), height(base, outs[5].end)),
        (0.75, 0.0)
    );
}

#[test]
fn only_a_body_on_the_ground_steps_up() {
    let base = I64Vec3::new(0, 0, 0);
    let mut world = floor(base);
    world.set(at(base, 1, 0, 0), STONE);
    // In the air a quarter below the block's top, flush with its face, falling slowly.
    let start = feet(base, 0.75, 0.75, 0.5);
    let air = stepping(
        &mut world,
        start,
        WALK,
        &[],
        StepUp {
            height: 0.6,
            grounded: false,
        },
    );
    assert!(air.touched(Face::PosX) && !air.on_ground());
    assert_eq!(along(base, air.end), 0.75);
    // On the ground in the previous step (Minecraft's rule): it steps up.
    let grounded = stepping(
        &mut world,
        start,
        WALK,
        &[],
        StepUp {
            height: 0.6,
            grounded: true,
        },
    );
    assert_eq!(height(base, grounded.end), 1.0);
    assert!(grounded.on_ground());
    // A move that lands is on the ground too.
    let start = feet(base, 0.75, 1.0 / 256.0, 0.5);
    world.set(at(base, 1, 0, 0), bottom_slab());
    let landing = stepping(
        &mut world,
        start,
        WALK,
        &[],
        StepUp {
            height: 0.6,
            grounded: false,
        },
    );
    assert_eq!(height(base, landing.end), 0.5);
}

#[test]
fn no_step_height_is_the_plain_sweep() {
    let base = I64Vec3::new(9, 9, -9);
    let mut world = stairs(base);
    let start = feet(base, 0.75, 0.0, 0.5);
    let plain = move_box(
        &mut world,
        &table(),
        start,
        body(),
        WALK,
        &[],
        &SweepConfig::default(),
    )
    .expect("loaded");
    assert!(plain.touched(Face::PosX));
    for height in [0.0, -1.0, f64::NAN] {
        let step = StepUp {
            height,
            grounded: true,
        };
        assert_eq!(stepping(&mut world, start, WALK, &[], step), plain);
    }
    // Sliding along a wall: the lifted move goes no further, the plain one stands.
    let wall = LocalBox::new(DVec3::new(1.0, 0.0, -4.0), DVec3::new(2.0, 2.0, 4.0));
    let d = DVec3::new(0.0625, -1.0 / 128.0, 0.03125);
    let plain = move_box(
        &mut world,
        &table(),
        start,
        body(),
        d,
        &[wall],
        &SweepConfig::default(),
    )
    .expect("loaded");
    assert!(plain.touched(Face::PosX) && plain.moved.z == 0.03125);
    assert_eq!(
        stepping(
            &mut world,
            start,
            d,
            &[wall],
            StepUp {
                height: 0.6,
                grounded: true,
            }
        ),
        plain
    );
}

#[test]
fn a_step_up_that_needs_an_unloaded_chunk_freezes() {
    // The body's top is just under a chunk border: the lift reaches into the chunk above.
    let base = I64Vec3::new(0, 30, 0);
    let mut world = floor(base);
    world.set(at(base, 1, 0, 0), bottom_slab());
    let above = ChunkPos::containing(at(base, 0, 2, 0));
    assert_ne!(above, ChunkPos::containing(at(base, 0, 1, 0)));
    world.set_unloaded(above, true);
    let start = feet(base, 0.75, 0.0, 0.5);
    let plain = move_box(
        &mut world,
        &table(),
        start,
        body(),
        WALK,
        &[],
        &SweepConfig::default(),
    );
    assert!(plain.is_ok(), "the plain move stays in loaded chunks");
    let step = StepUp {
        height: 0.6,
        grounded: true,
    };
    let lifted = move_box_stepping(
        &mut world,
        &table(),
        start,
        body(),
        WALK,
        &[],
        &SweepConfig::default(),
        step,
    );
    let err = lifted.expect_err("unloaded");
    assert_eq!(ChunkPos::containing(err.block), above);
}

/// The overlap of the box at `p` with solid blocks, by a sweep of nothing (it pushes the box out
/// only if it starts inside blocks).
fn inside_blocks(world: &mut TestWorld, p: WorldPos) -> bool {
    move_box(
        world,
        &table(),
        p,
        body(),
        DVec3::ZERO,
        &[],
        &SweepConfig::default(),
    )
    .expect("loaded")
    .depenetrated
}

/// One random case: a stone floor at y = -1 under a field of cubes and slabs from `seed`, and a
/// move from `start` (the blocks the box starts in are cleared; half the starts stand on the
/// floor or at a slab's height). Checks that a step-up never ends inside a block, never rises
/// more than the step height (plus the move's own rise), never goes further across than asked,
/// and that when it changes the move it goes further across than the plain one, which was
/// clipped across on the ground. Whether it stepped up, or `None` when the start is not free.
fn field_case(seed: u64, start: DVec3, d: DVec3, step: StepUp) -> Option<bool> {
    let base = I64Vec3::new(-4, 0, -4);
    let mut world = TestWorld::new();
    fill(&mut world, base, [0, -1, 0], [8, -1, 8], STONE);
    let mut rng = Rng(seed);
    for y in 0..6 {
        for x in 0..9 {
            for z in 0..9 {
                let r = rng.unit();
                if r < 0.2 {
                    world.set(at(base, x, y, z), STONE);
                } else if r < 0.3 {
                    world.set(at(base, x, y, z), bottom_slab());
                } else if r < 0.35 {
                    world.set(at(base, x, y, z), top_slab());
                }
            }
        }
    }
    let y = match rng.next() % 4 {
        0 => 0.0,
        1 => 0.5,
        _ => start.y,
    };
    let start = WorldPos::from_block(BlockPos::new(0, 0, 0), DVec3::new(start.x, y, start.z));
    let bx = body().translate(start.frac);
    for x in bx.min.x.floor() as i64..=bx.max.x.floor() as i64 {
        for y in bx.min.y.floor() as i64..=bx.max.y.floor() as i64 {
            for z in bx.min.z.floor() as i64..=bx.max.z.floor() as i64 {
                world.set(BlockPos::new(x, y, z), BlockState::AIR);
            }
        }
    }
    if inside_blocks(&mut world, start) {
        return None;
    }
    let plain = move_box(
        &mut world,
        &table(),
        start,
        body(),
        d,
        &[],
        &SweepConfig::default(),
    )
    .expect("loaded");
    let out = stepping(&mut world, start, d, &[], step);
    assert!(!inside_blocks(&mut world, out.end), "{out:?}");
    assert!(out.moved.y <= step.height + d.y.max(0.0) + 1e-9, "{out:?}");
    // `(a + b) - a` may round one ulp past `b`.
    assert!(out.moved.x.abs() <= d.x.abs() + 1e-12, "{out:?}");
    assert!(out.moved.z.abs() <= d.z.abs() + 1e-12, "{out:?}");
    if out == plain {
        return Some(false);
    }
    let across = |m: DVec3| m.x * m.x + m.z * m.z;
    assert!(across(out.moved) > across(plain.moved), "{out:?} {plain:?}");
    assert!(plain.faces & 0b11_1100 != 0, "only a clip across steps up");
    assert!(step.grounded || plain.on_ground());
    Some(true)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn a_step_up_is_free_bounded_and_better(
        seed in any::<u64>(),
        start in (0.0f64..1.0, 0.0f64..1.0, 0.0f64..1.0),
        d in (-0.75f64..0.75, -0.25f64..0.25, -0.75f64..0.75),
        height in 0.0f64..1.25,
        grounded in any::<bool>(),
    ) {
        let start = DVec3::new(start.0, start.1, start.2);
        let d = DVec3::new(d.0, d.1, d.2);
        prop_assume!(field_case(seed, start, d, StepUp { height, grounded }).is_some());
    }
}

#[test]
fn random_fields_have_step_ups() {
    // The proptest above checks the step-ups it gets: make sure the fields give many.
    let mut rng = Rng(7);
    let mut stepped = 0;
    for _ in 0..1000 {
        let mut unit3 = || DVec3::new(rng.unit(), rng.unit(), rng.unit());
        let start = unit3();
        let d = (unit3() * 1.5 - DVec3::splat(0.75)) * DVec3::new(1.0, 1.0 / 3.0, 1.0);
        let step = StepUp {
            height: 0.6,
            grounded: true,
        };
        if field_case(rng.next(), start, d, step) == Some(true) {
            stepped += 1;
        }
    }
    assert!(stepped >= 30, "{stepped}");
}

const SCALARS: [i64; 6] = [
    1 << 31,
    -(1 << 31),
    1 << 40,
    1 << 52,
    MAX_BLOCK_COORD - 64,
    -(MAX_BLOCK_COORD - 64),
];

/// The stairs and a roof obstacle at `base`: every move relative to `base`, floats as bits.
type Record = (I64Vec3, [u64; 3], [u64; 3], u8, Option<I64Vec3>);

fn course(base: I64Vec3) -> Vec<Record> {
    let mut world = stairs(base);
    let mut out = walk(&mut world, feet(base, 0.5, 0.0, 0.5), WALK, &[], 0.6, 120);
    // Diagonally, with a roof over the first slab that only the lower lift clears.
    let roof = LocalBox::new(DVec3::new(1.0, 2.3125, -4.0), DVec3::new(2.0, 3.0, 4.0));
    let d = DVec3::new(0.0625, -1.0 / 128.0, 0.046875);
    out.extend(walk(
        &mut world,
        feet(base, 0.25, 0.0, -2.5),
        d,
        &[roof],
        0.6,
        40,
    ));
    out.iter()
        .map(|o| {
            (
                o.end.block.as_i64vec3() - base,
                o.end.frac.to_array().map(f64::to_bits),
                o.moved.to_array().map(f64::to_bits),
                o.faces,
                o.ground.map(|g| g.as_i64vec3() - base),
            )
        })
        .collect()
}

#[test]
fn step_ups_are_bit_identical_far_from_the_origin() {
    let reference = course(I64Vec3::ZERO);
    for s in SCALARS {
        for shift in [
            I64Vec3::new(s, 0, 0),
            I64Vec3::new(0, s, 0),
            I64Vec3::new(0, 0, s),
            I64Vec3::new(s, -s, s),
        ] {
            assert_eq!(course(shift), reference, "shift {shift}");
        }
    }
}

// ------------------------------------------------------------------- the early skip

#[test]
fn pushing_into_a_wall_skips_the_lifted_moves() {
    // As in the unloaded test, the lift would reach into the chunk above, but the block ahead is
    // a full one: pushing into it, no lifted move can get past, so they are not even tried.
    let base = I64Vec3::new(0, 30, 0);
    let mut world = floor(base);
    world.set(at(base, 1, 0, 0), STONE);
    let above = ChunkPos::containing(at(base, 0, 2, 0));
    world.set_unloaded(above, true);
    let step = StepUp {
        height: 0.6,
        grounded: true,
    };
    // Touching the wall (the box's +X face on x = 1).
    let start = feet(base, 0.75, 0.0, 0.5);
    let plain = move_box(
        &mut world,
        &table(),
        start,
        body(),
        WALK,
        &[],
        &SweepConfig::default(),
    )
    .expect("loaded");
    assert!(plain.touched(Face::PosX));
    let run = |world: &mut TestWorld, unpruned: bool| {
        let f = if unpruned {
            move_box_stepping_unpruned::<TestWorld>
        } else {
            move_box_stepping::<TestWorld>
        };
        f(
            world,
            &table(),
            start,
            body(),
            WALK,
            &[],
            &SweepConfig::default(),
            step,
        )
    };
    assert_eq!(run(&mut world, false), Ok(plain), "skipped: the plain move");
    let err = run(&mut world, true).expect_err("tried: reads the chunk above");
    assert_eq!(ChunkPos::containing(err.block), above);
    // Loaded, both give the plain move.
    world.set_unloaded(above, false);
    assert_eq!(run(&mut world, true), Ok(plain));
    // Not touching it yet (the first hit), the lifted moves are tried: same result.
    let start = feet(base, 0.7, 0.0, 0.5);
    let tried = stepping(&mut world, start, WALK, &[], step);
    assert!(tried.touched(Face::PosX));
    assert_eq!(height(base, tried.end), 0.0);
}

/// One random case for the early skip: a field of stone, slabs and one-block hills, maybe
/// obstacles, and `moves` moves by `d` from `start` (each one from where the one before ended,
/// so they often push into what stopped them). With and without the skip the moves are the
/// same, bit for bit (errors too). How many of them were clipped across.
fn skip_case(
    seed: u64,
    start: DVec3,
    d: DVec3,
    step: StepUp,
    obstacles: bool,
    moves: usize,
) -> usize {
    let base = I64Vec3::new(-4, 0, -4);
    let mut world = TestWorld::new();
    fill(&mut world, base, [0, -1, 0], [8, -1, 8], STONE);
    let mut rng = Rng(seed);
    for x in 0..9 {
        for z in 0..9 {
            // Columns of stone (hills one or two blocks high), slabs on the ground or on stone.
            let r = rng.unit();
            let stone = if r < 0.25 {
                1
            } else if r < 0.35 {
                2
            } else {
                0
            };
            fill(&mut world, base, [x, 0, z], [x, stone - 1, z], STONE);
            let r = rng.unit();
            if r < 0.15 {
                world.set(at(base, x, stone, z), bottom_slab());
            } else if r < 0.2 {
                world.set(at(base, x, stone + 1, z), top_slab());
            }
        }
    }
    let y = match rng.next() % 3 {
        0 => 0.0,
        1 => 0.5,
        _ => start.y,
    };
    let start = WorldPos::from_block(BlockPos::new(0, 0, 0), DVec3::new(start.x, y, start.z));
    let bx = body().translate(start.frac);
    for x in bx.min.x.floor() as i64..=bx.max.x.floor() as i64 {
        for y in bx.min.y.floor() as i64..=bx.max.y.floor() as i64 {
            for z in bx.min.z.floor() as i64..=bx.max.z.floor() as i64 {
                world.set(BlockPos::new(x, y, z), BlockState::AIR);
            }
        }
    }
    let boxes: Vec<LocalBox> = if obstacles {
        (0..3)
            .map(|_| {
                let min = DVec3::new(
                    rng.unit() * 6.0 - 3.0,
                    rng.unit() * 2.0,
                    rng.unit() * 6.0 - 3.0,
                );
                LocalBox::new(min, min + DVec3::new(0.5, 0.25 + rng.unit(), 0.5))
            })
            .collect()
    } else {
        Vec::new()
    };
    let mut pos = start;
    let mut grounded = step.grounded;
    let mut across = 0;
    for _ in 0..moves {
        let shift = (start.block.as_i64vec3() - pos.block.as_i64vec3()).as_dvec3();
        let local: Vec<LocalBox> = boxes.iter().map(|o| o.translate(shift)).collect();
        let step = StepUp {
            height: step.height,
            grounded,
        };
        let cfg = SweepConfig::default();
        let skip = move_box_stepping(&mut world, &table(), pos, body(), d, &local, &cfg, step);
        let tried =
            move_box_stepping_unpruned(&mut world, &table(), pos, body(), d, &local, &cfg, step);
        assert_eq!(skip, tried, "from {pos:?} by {d:?}, {step:?}");
        let Ok(out) = skip else {
            break;
        };
        if out.faces & 0b11_1100 != 0 {
            across += 1;
        }
        pos = out.end;
        grounded = out.on_ground();
    }
    across
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn the_early_skip_never_changes_a_move(
        seed in any::<u64>(),
        start in (0.0f64..1.0, 0.0f64..1.0, 0.0f64..1.0),
        d in (-0.2f64..0.2, -0.2f64..0.2, -0.2f64..0.2),
        axes in 0u8..4,
        height in prop_oneof![Just(0.6), Just(0.5), Just(1.0), 0.0f64..1.25],
        grounded in any::<bool>(),
        obstacles in any::<bool>(),
    ) {
        // Straight moves too: one component exactly zero.
        let d = match axes {
            0 => DVec3::new(d.0, d.1, 0.0),
            1 => DVec3::new(0.0, d.1, d.2),
            _ => DVec3::new(d.0, d.1, d.2),
        };
        let start = DVec3::new(start.0, start.1, start.2);
        skip_case(seed, start, d, StepUp { height, grounded }, obstacles, 4);
    }
}

#[test]
fn walkers_in_random_fields_push_into_walls() {
    // The proptest above only means something if the moves hit things: make sure they do.
    let mut rng = Rng(11);
    let mut across = 0;
    for _ in 0..500 {
        let start = DVec3::new(rng.unit(), rng.unit(), rng.unit());
        let d = DVec3::new(rng.unit() * 0.4 - 0.2, -1.0 / 128.0, rng.unit() * 0.4 - 0.2);
        let step = StepUp {
            height: 0.6,
            grounded: true,
        };
        across += skip_case(rng.next(), start, d, step, false, 8);
    }
    assert!(across >= 1000, "{across}");
}

// --------------------------------------------------------------------------- characters

/// Puts `state` at every block of `base + min ..= base + max` and runs frames until the last one
/// is in the map.
fn build(sim: &mut Sim, min: [i64; 3], max: [i64; 3], state: BlockState) {
    for x in min[0]..=max[0] {
        for y in min[1]..=max[1] {
            for z in min[2]..=max[2] {
                sim.edit(sim.block(x, y, z), state);
            }
        }
    }
    let last = sim.block(max[0], max[1], max[2]);
    while sim.app.world().resource::<ChunkMap>().block_at(last) != Some(state) {
        sim.frame(STEP);
    }
}

/// Stairs up +X from x = 6 on the test world's ground (y = 4): slab, stone, slab on stone, then
/// a plateau two blocks up from x = 9.
fn sim_stairs(sim: &mut Sim) {
    build(sim, [6, 4, -2], [6, 4, 2], bottom_slab());
    build(sim, [7, 4, -2], [7, 4, 2], STONE);
    build(sim, [8, 4, -2], [8, 4, 2], STONE);
    build(sim, [8, 5, -2], [8, 5, 2], bottom_slab());
    build(sim, [9, 4, -2], [14, 5, 2], STONE);
}

fn wish(sim: &mut Sim, body: Entity, wish: Vec2) {
    sim.app
        .world_mut()
        .get_mut::<MoveIntent>(body)
        .expect("intent")
        .wish = wish;
}

/// The local height of a body's feet in the test world.
fn feet_y(sim: &Sim, body: Entity) -> f64 {
    let p = sim.position(body);
    (p.block.y - sim.base.y) as f64 + p.frac.y
}

/// One record per step of a character walking up [`sim_stairs`]: position relative to the base
/// and velocity, as bits.
type CharacterRecord = (I64Vec3, [u64; 3], [u64; 3]);

/// A character rests until it sleeps, then walks +X up the stairs for 3 s.
fn climb(base: I64Vec3, controller: CharacterController) -> (Sim, Entity, Vec<CharacterRecord>) {
    let mut sim = Sim::new(base, STEP);
    sim_stairs(&mut sim);
    let body = sim.spawn(sim.pos(0.5, 4.0, 0.5), controller);
    sim.steps(64);
    assert!(sim.asleep(body), "rests first");
    let mut records = Vec::new();
    for _ in 0..192 {
        wish(&mut sim, body, Vec2::X);
        sim.steps(1);
        let p = sim.position(body);
        records.push((
            p.block.as_i64vec3() - base,
            p.frac.to_array().map(f64::to_bits),
            sim.get::<Velocity>(body).0.to_array().map(f64::to_bits),
        ));
    }
    (sim, body, records)
}

#[test]
fn a_character_walks_up_a_stair_of_slabs_without_jumping() {
    let (sim, body, records) = climb(I64Vec3::ZERO, CharacterController::default());
    let mut last = 4.0;
    let mut rises = 0;
    for (i, (block, frac, vel)) in records.iter().enumerate() {
        let y = block.y as f64 + f64::from_bits(frac[1]);
        // Never more than a step at once, never a jump.
        assert!(y - last <= 0.6, "step {i}: {last} -> {y}");
        if y > last {
            rises += 1;
            assert_eq!(f64::from_bits(vel[1]), 0.0, "landed on the step");
        }
        // Once at speed, the stairs never stop it.
        if i > 40 {
            assert!(f64::from_bits(vel[0]) > 4.0, "step {i}: {vel:?}");
        }
        last = y;
    }
    assert_eq!(rises, 4);
    assert_eq!(feet_y(&sim, body), 6.0);
    assert!(sim.get::<Contacts>(body).on_ground);
    assert!(!sim.frozen(body));
}

#[test]
fn a_step_height_of_zero_keeps_the_slab_a_wall() {
    let controller = CharacterController {
        step_height: 0.0,
        ..CharacterController::default()
    };
    let (sim, body, _) = climb(I64Vec3::ZERO, controller);
    assert_eq!(feet_y(&sim, body), 4.0);
    let p = sim.position(body);
    assert!(
        ((p.block.x - sim.base.x) as f64 + p.frac.x - 5.7).abs() < 1e-9,
        "{p:?}"
    );
    assert!(sim.get::<Contacts>(body).touched(Face::PosX));
}

#[test]
fn climbing_is_bit_identical_far_from_the_origin() {
    let (_, _, reference) = climb(I64Vec3::ZERO, CharacterController::default());
    for base in [
        I64Vec3::new(1 << 31, 0, 0),
        I64Vec3::new(0, -(1 << 31), 1 << 40),
        I64Vec3::splat(MAX_BLOCK_COORD - 1024),
        I64Vec3::splat(-(MAX_BLOCK_COORD - 1024)),
    ] {
        let (_, _, records) = climb(base, CharacterController::default());
        assert_eq!(records, reference, "base {base}");
    }
}

#[test]
fn walking_off_a_slab_falls_with_gravity() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    build(&mut sim, [-3, 4, -2], [2, 4, 2], bottom_slab());
    let body = sim.spawn(sim.pos(0.5, 4.5, 0.5), CharacterController::default());
    sim.steps(4);
    assert_eq!(feet_y(&sim, body), 4.5);
    let mut between = 0;
    for _ in 0..64 {
        wish(&mut sim, body, Vec2::X);
        sim.steps(1);
        let y = feet_y(&sim, body);
        if y > 4.0 && y < 4.5 {
            between += 1;
        }
    }
    // Not snapped down: it falls over several steps, as before.
    assert!(between >= 3, "{between}");
    assert_eq!(feet_y(&sim, body), 4.0);
    assert!(sim.get::<Contacts>(body).on_ground);
}

#[test]
fn only_characters_step_up() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    build(&mut sim, [6, 4, -2], [6, 4, 2], bottom_slab());
    // A solid crate slid into the slab stops against it.
    let crate_body = sim.body(
        sim.pos(5.5, 4.0, 0.5),
        (
            Collider::standing(0.5, 0.5),
            BodySettings::item(),
            Velocity(DVec3::new(6.0, 0.0, 0.0)),
        ),
    );
    // An arrow with a controller (a strange mix) flying just over the ground sticks in it.
    let mut arrow = Projectile::arrow();
    arrow.on_block = OnBlock::Stop;
    let arrow = sim.body(
        sim.pos(4.5, 4.25, -1.5),
        (
            arrow,
            Collider::aabb(DVec3::splat(0.05)),
            BodySettings {
                gravity_scale: 0.0,
                ..BodySettings::arrow()
            },
            CharacterController::default(),
            Velocity(DVec3::new(20.0, 0.0, 0.0)),
        ),
    );
    sim.steps(64);
    // Stopped flush with the slab, on the ground.
    assert_eq!(feet_y(&sim, crate_body), 4.0);
    let p = sim.position(crate_body);
    assert_eq!((p.block.x - sim.base.x, p.frac.x), (5, 0.75));
    let hits = &sim.app.world().resource::<Hits>().0;
    assert!(
        hits.iter().any(|h| h.projectile == arrow
            && matches!(h.target, ProjectileTarget::Block { pos, .. } if pos == sim.block(6, 4, -2))),
        "{hits:?}"
    );
    assert!(feet_y(&sim, arrow) < 4.5);
}

#[test]
fn a_character_steps_onto_a_low_static_box_at_any_frame_rate() {
    for frame in [STEP, FRAME_144] {
        let mut sim = Sim::new(I64Vec3::ZERO, frame);
        // A static box from x = 3 to 5, half a block high, and one a block high on it from
        // x = 5.
        let (low, tall) = (sim.pos(4.0, 4.25, 0.5), sim.pos(5.5, 5.0, 0.5));
        sim.app.world_mut().spawn((
            RigidBody::Static,
            Collider::aabb(DVec3::new(1.0, 0.25, 1.0)),
            Teleport(low),
        ));
        sim.app.world_mut().spawn((
            RigidBody::Static,
            Collider::aabb(DVec3::new(0.5, 0.5, 1.0)),
            Teleport(tall),
        ));
        let body = sim.spawn(sim.pos(0.5, 4.0, 0.5), CharacterController::default());
        // Two seconds.
        let frames = if frame == STEP { 128 } else { 288 };
        for _ in 0..frames {
            wish(&mut sim, body, Vec2::X);
            sim.frame(frame);
        }
        // On the low box, stopped by the tall one.
        assert_eq!(feet_y(&sim, body), 4.5);
        let p = sim.position(body);
        assert!(
            ((p.block.x - sim.base.x) as f64 + p.frac.x - 4.7).abs() < 1e-9,
            "{p:?}"
        );
        let contacts = sim.get::<Contacts>(body);
        assert!(contacts.on_ground && contacts.touched(Face::PosX));
    }
}

/// Stairs (ADR 0020) are a slab and a step: a walking body climbs a staircase of them half a
/// block at a time, and their full back stops it.
#[test]
fn a_staircase_of_stairs_is_walked_up() {
    let registry = boxel_demo::blocks::registry();
    let east = registry
        .parse_state("boxel:stone_stairs[facing=east]")
        .unwrap();
    let base = I64Vec3::new(-40, 7, 1000);
    let mut world = floor(base);
    for k in 0..3 {
        world.set(at(base, 1 + k, k, 0), east);
        fill(&mut world, base, [2 + k, 0, 0], [2 + k, k, 0], STONE);
    }
    fill(&mut world, base, [4, 0, -1], [12, 2, 1], STONE);
    let outs = walk(&mut world, feet(base, 0.5, 0.0, 0.5), WALK, &[], 0.6, 120);
    let last = outs.last().unwrap();
    assert_eq!(height(base, last.end), 3.0, "{last:?}");
    assert!(along(base, last.end) > 5.0);
    // Every climb was half a block or less.
    let mut previous = 0.0;
    for out in &outs {
        let h = height(base, out.end);
        assert!(h - previous <= 0.5, "{out:?}");
        previous = h;
    }
    // Back down the other way the stairs' full back faces the body: a block high, no step-up.
    let mut world = floor(base);
    world.set(at(base, 1, 0, 0), east);
    let outs = walk(
        &mut world,
        feet(base, 2.5, 0.0, 0.5),
        DVec3::new(-WALK.x, WALK.y, 0.0),
        &[],
        0.6,
        24,
    );
    let last = outs.last().unwrap();
    assert_eq!(height(base, last.end), 0.0);
    assert_eq!(
        along(base, last.end),
        2.25,
        "stopped by the back of the stairs"
    );
}
