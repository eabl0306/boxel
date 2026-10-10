//! The player modes of `CharacterController` (issue #37, ADR 0026): sneaking (slower, never off
//! an edge deeper than the step height), sprinting and creative flight (no gravity, up and down
//! at will, landing ends it). First the pure edge guard (`guard_edge`), then characters in an
//! `App`; everything bit for bit the same far from the origin.
mod common;
mod sim;

use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    CharacterController, Collider, Contacts, EDGE_MARGIN, LocalBox, MoveIntent, SweepConfig,
    TestWorld, Velocity, guard_edge,
};
use boxel_voxel::{BlockPos, BlockState, MAX_BLOCK_COORD, WorldPos};
use boxel_world::ChunkMap;
use common::{STONE, bottom_slab, fill, table};
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
    let fence = boxel_voxel::BlockRegistry::default_blocks()
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
