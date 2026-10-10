//! The player modes of `CharacterController` (issue #37, ADR 0026): sneaking (slower, never off
//! an edge deeper than the step height, crouching where there is no room to stand), sprinting
//! (it ends moving backward, crouching or against a wall; the sprint-jump) and creative flight
//! (no gravity, up and down at will, landing ends it). First the pure edge guard
//! (`guard_edge`), then characters in an `App`; everything bit for bit the same far from the
//! origin.
mod common;
mod sim;

use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    CharacterController, Collider, Contacts, Crouch, EDGE_MARGIN, LocalBox, MoveIntent,
    SweepConfig, TestWorld, Velocity, guard_edge,
};
use boxel_voxel::{BlockPos, BlockState, MAX_BLOCK_COORD, WorldPos};
use boxel_world::ChunkMap;
use common::{STONE, bottom_slab, fill, table, top_slab};
use sim::{STEP, Sim};

/// A 0.5 x 1.75 x 0.5 box with its origin at the feet (binary fractions: exact positions).
fn body() -> LocalBox {
    LocalBox::new(DVec3::new(-0.25, 0.0, -0.25), DVec3::new(0.25, 1.75, 0.25))
}

// ------------------------------------------------------------------------------ edge guard

/// A stone platform `x, z in -3..=2` with its top at y = 0, around `base`.
fn platform(base: I64Vec3) -> TestWorld {
    let mut world = TestWorld::new();
    fill(&mut world, base, [-3, -1, -3], [2, -1, 2], STONE);
    world
}

fn guard(world: &mut TestWorld, base: I64Vec3, at: DVec3, d: DVec3, drop: f64) -> DVec3 {
    guard_edge(
        world,
        &table(),
        WorldPos::from_block(BlockPos::from_i64vec3(base), at),
        body(),
        d,
        &[],
        &SweepConfig::default(),
        drop,
    )
    .expect("loaded")
}

#[test]
fn the_guard_stops_a_move_at_the_edge() {
    for base in [
        I64Vec3::ZERO,
        I64Vec3::new(1 << 31, -(1 << 31), 1 << 40),
        I64Vec3::splat(MAX_BLOCK_COORD - 64),
    ] {
        let mut world = platform(base);
        // In the middle: nothing changes.
        let d = DVec3::new(0.25, -0.0078125, -0.125);
        assert_eq!(
            guard(&mut world, base, DVec3::new(0.5, 0.0, 0.5), d, 0.6),
            d
        );
        // Near the east edge (x = 3), moving east: cut where the box keeps the margin.
        let at_edge = DVec3::new(2.5, 0.0, 0.5);
        let cut = guard(&mut world, base, at_edge, DVec3::new(1.0, -0.5, 0.0), 0.6);
        assert_eq!(cut, DVec3::new(0.75 - EDGE_MARGIN, -0.5, 0.0));
        // Already at the margin: not a bit further, and never backward.
        let at_margin = DVec3::new(3.25 - EDGE_MARGIN, 0.0, 0.5);
        let d = DVec3::new(0.0625, 0.0, 0.0625);
        assert_eq!(
            guard(&mut world, base, at_margin, d, 0.6),
            DVec3::new(0.0, 0.0, 0.0625)
        );
        let past = DVec3::new(3.25 - EDGE_MARGIN / 2.0, 0.0, 0.5);
        assert_eq!(
            guard(&mut world, base, past, d, 0.6),
            DVec3::new(0.0, 0.0, 0.0625)
        );
        // Away from the edge is free.
        let back = DVec3::new(-0.0625, 0.0, 0.0);
        assert_eq!(guard(&mut world, base, at_margin, back, 0.6), back);
        // The north west corner, moving out diagonally: both axes cut.
        let corner = DVec3::new(-2.75, 0.0, -2.75);
        let cut = guard(&mut world, base, corner, DVec3::new(-1.0, 0.0, -1.0), 0.6);
        assert_eq!(cut, DVec3::new(-0.5 + EDGE_MARGIN, 0.0, -0.5 + EDGE_MARGIN));
    }
}

#[test]
fn a_ledge_within_the_drop_can_be_walked_down() {
    let base = I64Vec3::new(-7, 3, 1 << 33);
    let mut world = platform(base);
    // Half a block lower east of the platform (a bottom slab's top).
    fill(&mut world, base, [3, -1, -3], [5, -1, 2], bottom_slab());
    let at_edge = DVec3::new(2.5, 0.0, 0.5);
    let d = DVec3::new(1.0, 0.0, 0.0);
    assert_eq!(guard(&mut world, base, at_edge, d, 0.6), d, "0.5 deep");
    assert_eq!(
        guard(&mut world, base, at_edge, d, 0.4),
        DVec3::new(0.75 - EDGE_MARGIN, 0.0, 0.0),
        "deeper than the drop"
    );
}

#[test]
fn the_guard_does_nothing_without_support_or_drop() {
    let base = I64Vec3::new(64, 0, -64);
    let mut world = platform(base);
    let d = DVec3::new(2.0, 0.0, 0.0);
    // In the air (a jump, or standing on a dynamic body): no support to keep.
    assert_eq!(
        guard(&mut world, base, DVec3::new(0.5, 1.0, 0.5), d, 0.6),
        d
    );
    // No drop (0, negative, NaN): no guard.
    for drop in [0.0, -1.0, f64::NAN] {
        assert_eq!(
            guard(&mut world, base, DVec3::new(2.5, 0.0, 0.5), d, drop),
            d
        );
    }
    // A static body's box is support too: standing on a crate over the void.
    let mut empty = TestWorld::new();
    let crate_box = LocalBox::new(DVec3::new(-1.0, -1.0, -1.0), DVec3::new(1.0, 0.0, 1.0));
    let cut = guard_edge(
        &mut empty,
        &table(),
        WorldPos::from_block(BlockPos::from_i64vec3(base), DVec3::new(0.5, 0.0, 0.5)),
        body(),
        d,
        &[crate_box],
        &SweepConfig::default(),
        0.6,
    )
    .expect("loaded");
    assert_eq!(cut, DVec3::new(0.75 - EDGE_MARGIN, 0.0, 0.0));
    // A fence's top (its boxes reach half a block over its cell) holds a walker on it.
    let mut fences = TestWorld::new();
    let fence = boxel_demo::blocks::registry()
        .parse_state("boxel:oak_fence[east=true,west=true]")
        .unwrap();
    fill(&mut fences, base, [-3, -2, 0], [2, -2, 0], fence);
    let on_top = DVec3::new(0.5, -0.5, 0.5);
    let cut = guard(&mut fences, base, on_top, DVec3::new(0.0, 0.0, 1.0), 0.6);
    // The arms span z 6/16 to 10/16: the box keeps the margin of them.
    assert_eq!(cut, DVec3::new(0.0, 0.0, 0.375 - EDGE_MARGIN));
}

// ------------------------------------------------------------------------------ characters

/// Places `state` in every block of `min..=max` (relative to the base) and waits for the edits.
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

/// A walker with the exact 0.5 x 1.75 box, standing at `(x, y, z)` (relative to the base).
fn walker(sim: &mut Sim, x: f64, y: f64, z: f64) -> Entity {
    let pos = sim.pos(x, y, z);
    sim.body(
        pos,
        (
            Collider::standing(0.5, 1.75),
            CharacterController::default(),
        ),
    )
}

fn intent(sim: &mut Sim, body: Entity, f: impl FnOnce(&mut MoveIntent)) {
    let mut intent = sim
        .app
        .world_mut()
        .get_mut::<MoveIntent>(body)
        .expect("intent");
    f(&mut intent);
}

fn controller(sim: &mut Sim, body: Entity) -> Mut<'_, CharacterController> {
    sim.app
        .world_mut()
        .get_mut::<CharacterController>(body)
        .expect("controller")
}

/// The feet of `body` relative to the base.
fn feet(sim: &Sim, body: Entity) -> DVec3 {
    let p = sim.position(body);
    (p.block.as_i64vec3() - sim.base).as_dvec3() + p.frac
}

#[test]
fn walking_sprinting_and_sneaking_have_their_speeds() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let top = |sim: &mut Sim, sprint: bool, sneak: bool| {
        let body = walker(sim, 0.5, 4.0, 0.5);
        intent(sim, body, |i| {
            i.wish = Vec2::Y;
            i.sprint = sprint;
            i.sneak = sneak;
        });
        sim.steps(160);
        let v = sim.get::<Velocity>(body).0;
        sim.app.world_mut().despawn(body);
        v.z
    };
    let walk = CharacterController::default().walk_speed;
    for (sprint, sneak, factor) in [
        (false, false, 1.0),
        (true, false, 1.3),
        (false, true, 0.3),
        (true, true, 0.3),
    ] {
        let v = top(&mut sim, sprint, sneak);
        assert!(
            (v - walk * factor).abs() < 1e-6,
            "sprint {sprint} sneak {sneak}: {v}"
        );
    }
}

/// The ground of the test world is at y = 4; a pit 3 deep from x = 3 to 6, z = -3 to 3.
fn pit(sim: &mut Sim) {
    build(sim, [3, 1, -3], [6, 3, 3], BlockState::AIR);
}

#[test]
fn a_sneaking_walker_stops_at_the_edge_and_falls_without_sneaking() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    pit(&mut sim);
    let body = walker(&mut sim, 1.5, 4.0, 0.5);
    sim.steps(64);
    intent(&mut sim, body, |i| {
        i.wish = Vec2::X;
        i.sneak = true;
    });
    let mut fastest: f64 = 0.0;
    for _ in 0..192 {
        sim.steps(1);
        let p = feet(&sim, body);
        assert_eq!(p.y, 4.0, "never off the edge");
        assert!(p.x - 0.25 < 3.0 - EDGE_MARGIN / 2.0, "{p}");
        fastest = fastest.max(sim.get::<Velocity>(body).0.x);
    }
    let walk = CharacterController::default().walk_speed;
    assert!(
        fastest > 0.29 * walk && fastest < 0.3 * walk + 1e-9,
        "{fastest}"
    );
    // At the edge: the box keeps the margin of the last block, standing, its velocity across
    // stopped.
    let p = feet(&sim, body);
    assert!((p.x - (3.25 - EDGE_MARGIN)).abs() < 1e-12, "{p}");
    assert!(sim.get::<Contacts>(body).on_ground);
    assert_eq!(sim.get::<Velocity>(body).0.x, 0.0);
    // Let go of the sneak key: over the edge and down into the pit.
    intent(&mut sim, body, |i| i.sneak = false);
    sim.steps(64);
    assert_eq!(feet(&sim, body).y, 1.0);
}

#[test]
fn a_sneaking_walker_steps_down_a_slab() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    pit(&mut sim);
    build(&mut sim, [3, 3, -3], [6, 3, 3], bottom_slab());
    let body = walker(&mut sim, 1.5, 4.0, 0.5);
    intent(&mut sim, body, |i| {
        i.wish = Vec2::X;
        i.sneak = true;
    });
    sim.steps(192);
    let p = feet(&sim, body);
    assert_eq!(p.y, 3.5);
    assert!(p.x > 4.0 && p.x < 6.5, "{p}");
}

/// Flies a walker standing at x = 0.5 (relative to `sim`'s base): up for a second, hovers,
/// across, down to the ground. One record per step: position relative to the base, velocity,
/// flying.
fn fly(sim: &mut Sim) -> (Entity, Vec<(I64Vec3, [u64; 3], [u64; 3], bool)>) {
    let body = walker(sim, 0.5, 4.0, 0.5);
    sim.steps(64);
    assert!(sim.asleep(body));
    let mut records = Vec::new();
    let record = |sim: &mut Sim, n: usize, records: &mut Vec<_>| {
        for _ in 0..n {
            sim.steps(1);
            let p = sim.position(body);
            records.push((
                p.block.as_i64vec3() - sim.base,
                p.frac.to_array().map(f64::to_bits),
                sim.get::<Velocity>(body).0.to_array().map(f64::to_bits),
                sim.get::<CharacterController>(body).flying,
            ));
        }
    };
    controller(sim, body).flying = true;
    // Turned on while standing still, it keeps flying.
    record(sim, 8, &mut records);
    intent(sim, body, |i| i.up = true);
    record(sim, 64, &mut records);
    intent(sim, body, |i| i.up = false);
    record(sim, 128, &mut records);
    intent(sim, body, |i| {
        i.wish = Vec2::X;
        i.jump = true;
    });
    record(sim, 128, &mut records);
    intent(sim, body, |i| {
        i.wish = Vec2::ZERO;
        i.sneak = true;
    });
    record(sim, 128, &mut records);
    (body, records)
}

#[test]
fn creative_flight_rises_hovers_flies_and_lands() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let (body, records) = fly(&mut sim);
    let y = |r: &(I64Vec3, [u64; 3], [u64; 3], bool)| r.0.y as f64 + f64::from_bits(r.1[1]);
    let v = |r: &(I64Vec3, [u64; 3], [u64; 3], bool)| r.2.map(f64::from_bits);
    let c = CharacterController::default();
    // Standing still while flying: no gravity pulls it down, so it does not land.
    assert!(records[..8].iter().all(|r| y(r) == 4.0 && r.3));
    // Up: about 7.5 blocks per second after a second.
    let up = &records[8..72];
    assert!(up.windows(2).all(|w| y(&w[1]) > y(&w[0])));
    let climb = v(&up[63])[1];
    assert!((climb - c.fly_vertical_speed).abs() < 1e-3, "{climb}");
    // Let go: it stops within a second and a half and then hovers exactly, no gravity.
    let hover = &records[72..200];
    let stopped = hover.iter().position(|r| v(r)[1] == 0.0).expect("stops");
    assert!(stopped < 96, "{stopped}");
    let height = y(&hover[stopped]);
    assert!(height > 10.0, "{height}");
    assert!(hover[stopped..].iter().all(|r| y(r) == height && r.3));
    // Across: toward the flight speed, with the air drag (the jump does nothing).
    let across = &records[200..328];
    let speed = v(&across[127])[0];
    assert!(speed > 0.97 * c.fly_speed && speed < c.fly_speed, "{speed}");
    assert!(across.iter().all(|r| y(r) == height));
    // Down while sneaking: onto the ground, which ends the flight.
    let down = &records[328..];
    let landed = down.iter().position(|r| !r.3).expect("lands");
    assert_eq!(y(&down[landed]), 4.0);
    assert!(sim.get::<Contacts>(body).on_ground);
    assert!(!sim.get::<CharacterController>(body).flying);
    assert_eq!(feet(&sim, body).y, 4.0);
}

#[test]
fn sprinting_doubles_the_flight_speed() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let flyers = [false, true].map(|sprint| {
        let body = walker(&mut sim, if sprint { 4.5 } else { 0.5 }, 4.0, 0.5);
        controller(&mut sim, body).flying = true;
        intent(&mut sim, body, |i| {
            i.up = true;
            i.wish = Vec2::Y;
            i.sprint = sprint;
        });
        body
    });
    sim.steps(8);
    for body in flyers {
        intent(&mut sim, body, |i| i.up = false);
    }
    sim.steps(120);
    let [slow, fast] = flyers.map(|body| sim.get::<Velocity>(body).0.z);
    let c = CharacterController::default();
    assert!(slow > 0.97 * c.fly_speed && slow < c.fly_speed, "{slow}");
    assert!((fast - 2.0 * slow).abs() < 1e-9 * fast, "{fast} {slow}");
    assert!(
        flyers
            .iter()
            .all(|&body| sim.get::<CharacterController>(body).flying)
    );
}

/// The walker's box height (its collider's, against the voxels).
fn height(sim: &Sim, body: Entity) -> f64 {
    let b = sim.get::<Collider>(body).voxel_box();
    b.max.y - b.min.y
}

/// A roof of top slabs over the ground from x = 3 to 6 (z -3 to 3): under it there are 1.5
/// blocks of room, enough to crouch (1.5) and not to stand (1.75).
fn roof(sim: &mut Sim) {
    build(sim, [3, 5, -3], [6, 5, 3], top_slab());
}

#[test]
fn sneaking_crouches_and_standing_up_needs_room() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    roof(&mut sim);
    let body = walker(&mut sim, 1.5, 4.0, 0.5);
    let standing = Collider::standing(0.5, 1.75);
    sim.steps(64);
    assert!(!sim.get::<Crouch>(body).is_crouching());
    assert!(sim.asleep(body));
    // Sneak: the body wakes up and crouches at once, 1.5 tall, its feet where they were.
    intent(&mut sim, body, |i| i.sneak = true);
    sim.steps(1);
    assert!(sim.get::<Crouch>(body).is_crouching());
    assert_eq!(sim.get::<Crouch>(body).standing(), Some(standing));
    assert_eq!(height(&sim, body), 1.5);
    assert_eq!(sim.get::<Collider>(body).voxel_box().min.y, 0.0);
    assert_eq!(feet(&sim, body), DVec3::new(1.5, 4.0, 0.5));
    // Crouched and still, it falls asleep; let go in the open: it wakes up and stands up
    // again, with the very same collider.
    sim.steps(64);
    assert!(sim.asleep(body) && sim.get::<Crouch>(body).is_crouching());
    intent(&mut sim, body, |i| i.sneak = false);
    sim.steps(1);
    assert!(!sim.get::<Crouch>(body).is_crouching());
    assert_eq!(sim.get::<Collider>(body), standing);
    // Standing, the roof stops it; crouched it walks under (its top flush with the slabs).
    intent(&mut sim, body, |i| i.wish = Vec2::X);
    sim.steps(64);
    assert!(
        (feet(&sim, body).x - 2.75).abs() < 1e-9,
        "{}",
        feet(&sim, body)
    );
    intent(&mut sim, body, |i| i.sneak = true);
    sim.steps(128);
    assert!(feet(&sim, body).x > 3.5, "{}", feet(&sim, body));
    // Under the roof, letting go of the sneak keeps it crouched, and slow.
    intent(&mut sim, body, |i| i.sneak = false);
    for _ in 0..64 {
        sim.steps(1);
        assert!(sim.get::<Crouch>(body).is_crouching());
        assert_eq!(height(&sim, body), 1.5);
        assert_eq!(feet(&sim, body).y, 4.0);
        let v = sim.get::<Velocity>(body).0.x;
        assert!(
            v < 0.3 * CharacterController::default().walk_speed + 1e-9,
            "{v}"
        );
    }
    // Stopped under it, it stays awake, crouched, waiting for room; taking the roof away
    // stands it up.
    intent(&mut sim, body, |i| i.wish = Vec2::ZERO);
    sim.steps(128);
    assert!(!sim.asleep(body));
    assert!(sim.get::<Crouch>(body).is_crouching());
    let x = feet(&sim, body).x.floor() as i64;
    // (The edits apply in the frame's `Update`, after its step: the next step sees them.)
    build(&mut sim, [x - 1, 5, -3], [x + 1, 5, 3], BlockState::AIR);
    sim.steps(2);
    assert!(!sim.get::<Crouch>(body).is_crouching());
    assert_eq!(sim.get::<Collider>(body), standing);
    // Sneaking on, under the rest of the roof and out, then letting go: it stands up and
    // walks at full speed.
    intent(&mut sim, body, |i| {
        i.wish = Vec2::X;
        i.sneak = true;
    });
    sim.steps(320);
    intent(&mut sim, body, |i| i.sneak = false);
    sim.steps(64);
    assert!(feet(&sim, body).x > 7.25, "{}", feet(&sim, body));
    assert!(!sim.get::<Crouch>(body).is_crouching());
    assert_eq!(sim.get::<Collider>(body), standing);
    let v = sim.get::<Velocity>(body).0.x;
    assert!(
        (v - CharacterController::default().walk_speed).abs() < 1e-3,
        "{v}"
    );
    // A flyer that sneaks descends and does not crouch.
    let flyer = walker(&mut sim, -2.5, 4.0, 0.5);
    controller(&mut sim, flyer).flying = true;
    intent(&mut sim, flyer, |i| i.up = true);
    sim.steps(16);
    intent(&mut sim, flyer, |i| {
        i.up = false;
        i.sneak = true;
    });
    sim.steps(2);
    assert!(sim.get::<CharacterController>(flyer).flying);
    assert!(!sim.get::<Crouch>(flyer).is_crouching());
}

fn sprinting(sim: &Sim, body: Entity) -> bool {
    sim.get::<CharacterController>(body).sprinting
}

#[test]
fn a_sprint_starts_forward_and_ends_backward_crouching_or_against_a_wall() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let walk = CharacterController::default().walk_speed;
    let body = walker(&mut sim, 0.5, 4.0, -20.5);
    // Sideways or backward (the facing is +Z) the key does not start it.
    for wish in [Vec2::X, Vec2::NEG_X, Vec2::NEG_Y, Vec2::new(0.6, -0.8)] {
        intent(&mut sim, body, |i| {
            i.facing = Vec2::Y;
            i.wish = wish;
            i.sprint = true;
        });
        sim.steps(2);
        assert!(!sprinting(&sim, body), "{wish}");
    }
    // Forward (also forward and to a side) it does, and it goes on after the key is let go.
    intent(&mut sim, body, |i| i.wish = Vec2::new(0.6, 0.8));
    sim.steps(1);
    assert!(sprinting(&sim, body));
    intent(&mut sim, body, |i| {
        i.wish = Vec2::Y;
        i.sprint = false;
    });
    sim.steps(96);
    assert!(sprinting(&sim, body));
    let v = sim.get::<Velocity>(body).0.z;
    assert!((v - 1.3 * walk).abs() < 1e-6, "{v}");
    // Backward ends it, and so does stopping.
    intent(&mut sim, body, |i| i.wish = Vec2::NEG_Y);
    sim.steps(1);
    assert!(!sprinting(&sim, body));
    intent(&mut sim, body, |i| {
        i.wish = Vec2::Y;
        i.sprint = true;
    });
    sim.steps(1);
    intent(&mut sim, body, |i| {
        i.wish = Vec2::ZERO;
        i.sprint = false;
    });
    sim.steps(1);
    assert!(!sprinting(&sim, body));
    // Sneaking ends it, and the key does not start it while sneaking.
    intent(&mut sim, body, |i| {
        i.wish = Vec2::Y;
        i.sprint = true;
    });
    sim.steps(1);
    assert!(sprinting(&sim, body));
    intent(&mut sim, body, |i| i.sneak = true);
    sim.steps(4);
    assert!(!sprinting(&sim, body));
    intent(&mut sim, body, |i| i.sneak = false);
    sim.steps(2);
    assert!(sprinting(&sim, body));
    // A wall ahead ends it.
    let z = feet(&sim, body).z.floor() as i64 + 3;
    build(&mut sim, [-3, 4, z], [3, 5, z], STONE);
    sim.steps(64);
    assert!((feet(&sim, body).z - (z as f64 - 0.25)).abs() < 1e-9);
    assert!(!sprinting(&sim, body));
}

#[test]
fn a_sprint_grazing_a_wall_goes_on_and_one_turned_by_it_ends() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // A wall along Z at x = 3.
    build(&mut sim, [3, 4, -24], [3, 5, 24], STONE);
    // 5 and 15 degrees off +Z, into the wall (tangents; the wish counts as 1 long).
    for (tan, goes_on) in [(0.0875f32, true), (0.268, false)] {
        let body = walker(&mut sim, 2.5, 4.0, -20.5);
        let wish = Vec2::new(tan, 1.0);
        intent(&mut sim, body, |i| {
            i.wish = wish;
            i.facing = wish;
            i.sprint = true;
        });
        sim.steps(64);
        assert!((feet(&sim, body).x - 2.75).abs() < 1e-9, "against the wall");
        assert_eq!(sprinting(&sim, body), goes_on, "{tan}");
        sim.app.world_mut().despawn(body);
    }
}

#[test]
fn the_sprint_key_held_into_a_wall_does_not_sprint_along_it() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let c = CharacterController::default();
    // A wall along Z at x = 3.
    build(&mut sim, [3, 4, -24], [3, 5, 24], STONE);
    // The key held all along, 5 and 15 degrees into the wall: grazing it, the sprint goes on;
    // turned by it, the last move's clip ends the sprint before the key starts it again
    // (Minecraft's `aiStep`), so the body walks along the wall and a jump gets no push.
    for (tan, sprints) in [(0.0875f32, true), (0.268, false)] {
        let body = walker(&mut sim, 2.5, 4.0, -20.5);
        let wish = Vec2::new(tan, 1.0);
        intent(&mut sim, body, |i| {
            i.wish = wish;
            i.facing = wish;
            i.sprint = true;
        });
        sim.steps(160);
        assert!((feet(&sim, body).x - 2.75).abs() < 1e-9, "against the wall");
        let along = 1.0 / DVec3::new(f64::from(tan), 0.0, 1.0).length();
        let factor = if sprints { c.sprint_factor } else { 1.0 };
        let v = sim.get::<Velocity>(body).0.z;
        assert!(
            (v - factor * c.walk_speed * along).abs() < 1e-6,
            "{tan}: {v} along the wall"
        );
        intent(&mut sim, body, |i| i.jump = true);
        sim.steps(1);
        let pushed = sim.get::<Velocity>(body).0.z - v;
        if sprints {
            assert!(
                pushed > 0.8 * c.sprint_jump_boost * along,
                "{tan}: {pushed}"
            );
        } else {
            assert!(pushed.abs() < 1e-6, "{tan}: {pushed}");
        }
        sim.app.world_mut().despawn(body);
    }
}

#[test]
fn a_sprint_jump_carries_further() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let c = CharacterController::default();
    let runners = [false, true].map(|jumps| {
        let body = walker(&mut sim, if jumps { 4.5 } else { 0.5 }, 4.0, -24.5);
        intent(&mut sim, body, |i| {
            i.wish = Vec2::Y;
            i.sprint = true;
        });
        body
    });
    sim.steps(64);
    let start = runners.map(|body| feet(&sim, body).z);
    // Space held: jump again on every landing.
    let mut boosted = 0.0;
    for step in 0..256 {
        intent(&mut sim, runners[1], |i| i.jump = true);
        let before = sim.get::<Velocity>(runners[1]).0.z;
        sim.steps(1);
        if step == 0 {
            boosted = sim.get::<Velocity>(runners[1]).0.z - before;
        }
    }
    assert!(runners.iter().all(|&body| sprinting(&sim, body)));
    // The first jump pushes along the facing by the boost (less the step's ground friction).
    assert!(
        boosted > 0.8 * c.sprint_jump_boost && boosted < c.sprint_jump_boost,
        "{boosted}"
    );
    // Over four seconds, about Minecraft's 7.1 blocks per second against its sprint's 5.6.
    let [run, jump] = [0, 1].map(|k| (feet(&sim, runners[k]).z - start[k]) / 4.0);
    assert!((run - 1.3 * c.walk_speed).abs() < 1e-3, "{run}");
    assert!(
        jump > 1.2 * run && jump < 1.35 * run,
        "{jump} against {run}"
    );
}

/// A walker sprints and sprint-jumps into a wall, then crouches under a roof (as [`roof`], from
/// x = -13 to -7) and stays crouched there. One record per step.
fn sprint_and_crouch(sim: &mut Sim) -> Vec<(I64Vec3, [u64; 3], [u64; 3], bool)> {
    build(sim, [-13, 5, -3], [-7, 5, 3], top_slab());
    build(sim, [-18, 4, 8], [-12, 5, 8], STONE);
    let body = walker(sim, -15.5, 4.0, -6.5);
    let mut records = Vec::new();
    let record = |sim: &mut Sim, n: usize, jump: bool, records: &mut Vec<_>| {
        for _ in 0..n {
            if jump {
                intent(sim, body, |i| i.jump = true);
            }
            sim.steps(1);
            let p = sim.position(body);
            records.push((
                p.block.as_i64vec3() - sim.base,
                p.frac.to_array().map(f64::to_bits),
                sim.get::<Velocity>(body).0.to_array().map(f64::to_bits),
                sim.get::<CharacterController>(body).sprinting,
            ));
        }
    };
    intent(sim, body, |i| {
        i.wish = Vec2::new(0.1, 1.0);
        i.facing = Vec2::Y;
        i.sprint = true;
    });
    record(sim, 160, true, &mut records);
    assert!(
        !sim.get::<CharacterController>(body).sprinting,
        "the wall ends it"
    );
    intent(sim, body, |i| {
        i.wish = Vec2::new(1.0, -0.8);
        i.facing = Vec2::X;
        i.sneak = true;
    });
    record(sim, 384, false, &mut records);
    intent(sim, body, |i| i.sneak = false);
    record(sim, 64, false, &mut records);
    let p = feet(sim, body);
    assert!(
        p.x > -12.75 && p.x < -6.25 && p.z < 3.75 && p.y == 4.0,
        "under the roof: {p}"
    );
    assert!(sim.get::<Crouch>(body).is_crouching());
    records
}

#[test]
fn the_player_modes_are_bit_identical_far_from_the_origin() {
    let run = |base: I64Vec3| {
        let mut sim = Sim::new(base, STEP);
        pit(&mut sim);
        let edge = walker(&mut sim, 1.5, 4.0, 0.5);
        intent(&mut sim, edge, |i| {
            i.wish = Vec2::new(1.0, 0.25);
            i.sneak = true;
        });
        let mut records = Vec::new();
        for _ in 0..128 {
            sim.steps(1);
            let p = sim.position(edge);
            records.push((
                p.block.as_i64vec3() - base,
                p.frac.to_array().map(f64::to_bits),
                sim.get::<Velocity>(edge).0.to_array().map(f64::to_bits),
                false,
            ));
        }
        let (_, flight) = fly(&mut sim);
        records.extend(flight);
        records.extend(sprint_and_crouch(&mut sim));
        records
    };
    let reference = run(I64Vec3::ZERO);
    for base in [
        I64Vec3::new(1 << 31, 0, 0),
        I64Vec3::new(0, -(1 << 31), 1 << 40),
        I64Vec3::splat(MAX_BLOCK_COORD - 1024),
        I64Vec3::splat(-(MAX_BLOCK_COORD - 1024)),
    ] {
        assert_eq!(run(base), reference, "base {base}");
    }
}
