//! The integrator of a body (`step_body`, `integrate_velocity`, `predict`) without an `App`:
//! exact landings, resting without drift, terminal speed, drag and friction, the speed limit,
//! bounces that decay, impacts, freezing on unloaded chunks, `predict` against a real flight,
//! and translation invariance of whole flights.
mod common;

use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    BodySettings, BodyStep, CharacterController, LocalBox, StepParams, TestWorld,
    integrate_velocity, predict, step_body,
};
use boxel_voxel::{BlockPos, ChunkPos, Face, MAX_BLOCK_COORD, WorldPos};
use common::{STONE, at, fill, player, table};

/// A stone floor with its top at y = 0 around `base`, 20 blocks each way.
fn floor(base: I64Vec3) -> TestWorld {
    let mut world = TestWorld::new();
    fill(&mut world, base, [-20, -1, -20], [20, -1, 20], STONE);
    world
}

fn step(
    world: &mut TestWorld,
    pos: WorldPos,
    vel: DVec3,
    on_ground: bool,
    settings: &BodySettings,
) -> BodyStep {
    step_body(
        world,
        &table(),
        pos,
        vel,
        player(),
        on_ground,
        settings,
        &StepParams::default(),
    )
    .expect("loaded")
}

/// Runs `steps` steps from `pos` at rest; returns every step.
fn run(
    world: &mut TestWorld,
    mut pos: WorldPos,
    mut vel: DVec3,
    settings: &BodySettings,
    steps: usize,
) -> Vec<BodyStep> {
    let mut on_ground = false;
    let mut out = Vec::with_capacity(steps);
    for _ in 0..steps {
        let s = step(world, pos, vel, on_ground, settings);
        pos = s.pos;
        vel = s.vel;
        on_ground = s.sweep.on_ground();
        out.push(s);
    }
    out
}

#[test]
fn a_dropped_body_lands_exactly_on_the_floor_with_one_impact() {
    let base = I64Vec3::new(5, -3, 7);
    let mut world = floor(base);
    let start = WorldPos::from_block(at(base, 0, 40, 0), DVec3::new(0.5, 0.0, 0.5));
    let steps = run(&mut world, start, DVec3::ZERO, &BodySettings::player(), 200);
    let last = steps.last().expect("steps");
    assert_eq!(last.pos.block, at(base, 0, 0, 0));
    assert_eq!(
        last.pos.frac,
        DVec3::new(0.5, 0.0, 0.5),
        "exactly on the plane"
    );
    assert_eq!(last.vel, DVec3::ZERO);
    assert!(last.sweep.on_ground());

    let impacts: Vec<_> = steps.iter().flat_map(BodyStep::impacts).collect();
    assert_eq!(impacts.len(), 1, "{impacts:?}");
    let impact = impacts[0];
    assert_eq!(impact.face, Face::NegY);
    assert_eq!(impact.block, at(base, 0, -1, 0));
    // The speed is the integrator's own velocity of the landing step: replay it in open air.
    let landing = steps
        .iter()
        .position(|s| s.impacts().next().is_some())
        .expect("landing step");
    let params = StepParams::default();
    let (_, v) = predict(
        start,
        DVec3::ZERO,
        &BodySettings::player(),
        &params,
        landing as u32 + 1,
    );
    assert_eq!(impact.speed.to_bits(), (-v.y).to_bits());
    // 40 blocks with drag 0.4 /s: 80 (1 - e^(-0.4 t)) at h(t) = 40, about 40.6 blocks/s.
    assert!((39.0..42.0).contains(&impact.speed), "{}", impact.speed);
}

#[test]
fn a_resting_body_never_drifts() {
    let base = I64Vec3::new(-40, 12, 3);
    let mut world = floor(base);
    let start = WorldPos::from_block(at(base, 2, 0, -3), DVec3::new(0.3, 0.0, 0.7));
    let mut pos = start;
    let mut vel = DVec3::ZERO;
    let mut on_ground = true;
    for _ in 0..10_000 {
        let s = step(&mut world, pos, vel, on_ground, &BodySettings::player());
        assert_eq!(s.impacts().count(), 0, "resting is not an impact");
        pos = s.pos;
        vel = s.vel;
        on_ground = s.sweep.on_ground();
        assert!(on_ground);
    }
    assert_eq!(pos, start, "bit for bit");
    assert_eq!(vel, DVec3::ZERO);
}

#[test]
fn falls_reach_the_terminal_speed_and_the_speed_limit_holds() {
    let settings = BodySettings::player();
    let params = StepParams::default();
    let mut v = DVec3::ZERO;
    for _ in 0..64 * 60 {
        v = integrate_velocity(v, false, &settings, &params);
    }
    // Implicit drag: v = (v + g dt) / (1 + k dt) settles at g / k = 32 / 0.4 = 80 (after a
    // minute, within 80 e^-24).
    assert!((v.y + 80.0).abs() < 1e-6, "{v:?}");
    // A lower limit clamps every axis.
    let slow = StepParams {
        max_speed: 10.0,
        ..params
    };
    let v = integrate_velocity(DVec3::new(50.0, -50.0, -3.0), false, &settings, &slow);
    assert_eq!((v.x, v.y), (10.0, -10.0));
    assert!(v.z < 0.0 && v.z > -3.0);
    // Garbage in, no garbage kept.
    let v = integrate_velocity(
        DVec3::new(f64::NAN, f64::INFINITY, 1.0),
        false,
        &settings,
        &params,
    );
    assert!(v.is_finite(), "{v:?}");
    assert_eq!(v.x, 0.0);
}

#[test]
fn ground_friction_stops_a_slide_faster_than_air_drag() {
    let settings = BodySettings::player();
    let params = StepParams::default();
    let mut ground = DVec3::new(8.0, 0.0, 0.0);
    let mut air = ground;
    for _ in 0..32 {
        ground = integrate_velocity(ground, true, &settings, &params);
        air = integrate_velocity(air, false, &settings, &params);
    }
    // Half a second: about e^-5.5 of the speed left on the ground, e^-0.95 in the air.
    assert!(ground.x < 0.05, "{ground:?}");
    assert!(air.x > 2.5, "{air:?}");
}

#[test]
fn a_default_jump_peaks_about_a_block_and_a_quarter() {
    // Minecraft's jump peaks at 1.25 blocks. With gravity before the move (64 Hz), 9 blocks/s
    // would peak at 1.11; the default 9.5 peaks at 1.24, so a jump clears a one-block step.
    let jump = DVec3::new(0.0, CharacterController::default().jump_speed, 0.0);
    let params = StepParams::default();
    let settings = BodySettings::player();
    let top = (1..64)
        .map(|n| {
            predict(WorldPos::default(), jump, &settings, &params, n)
                .0
                .to_dvec3()
                .y
        })
        .fold(0.0, f64::max);
    assert!((1.2..1.3).contains(&top), "{top}");
}

#[test]
fn a_ball_bounces_lower_each_time_and_comes_to_rest() {
    let base = I64Vec3::ZERO;
    let mut world = floor(base);
    let start = WorldPos::from_block(at(base, 0, 10, 0), DVec3::splat(0.5));
    let ball = LocalBox::from_center_half(DVec3::ZERO, DVec3::splat(0.25));
    let settings = BodySettings::ball();
    let params = StepParams::default();
    let (mut pos, mut vel, mut on_ground) = (start, DVec3::ZERO, false);
    let mut bounces = Vec::new();
    for _ in 0..64 * 20 {
        let s = step_body(
            &mut world,
            &table(),
            pos,
            vel,
            ball,
            on_ground,
            &settings,
            &params,
        )
        .expect("loaded");
        if s.sweep.on_ground() && s.vel.y > 0.0 {
            bounces.push(s.vel.y);
        }
        pos = s.pos;
        vel = s.vel;
        on_ground = s.sweep.on_ground();
    }
    assert!(bounces.len() >= 3, "{bounces:?}");
    for pair in bounces.windows(2) {
        assert!(pair[1] < pair[0], "{bounces:?}");
    }
    // Each bounce keeps 0.6 of the impact speed.
    assert!(bounces[0] > 10.0, "{bounces:?}");
    assert_eq!(vel.y, 0.0, "at rest");
    assert_eq!(
        pos.frac.y, 0.25,
        "resting on the floor (center 0.25 above it)"
    );
    assert_eq!(pos.block.y, 0);
}

#[test]
fn hitting_a_wall_stops_that_axis_and_reports_the_block() {
    let base = I64Vec3::new(100, 0, -100);
    let mut world = floor(base);
    fill(&mut world, base, [3, 0, -5], [3, 3, 5], STONE);
    let start = WorldPos::from_block(at(base, 0, 0, 0), DVec3::new(0.5, 0.0, 0.5));
    let mut pos = start;
    let mut vel = DVec3::new(20.0, 0.0, 1.0);
    let mut hit = None;
    for _ in 0..20 {
        // Air drag (not ground friction), so the slide reaches the wall.
        let s = step(&mut world, pos, vel, false, &BodySettings::player());
        if let Some(impact) = s.impacts().find(|i| i.face == Face::PosX) {
            hit = Some((impact, s));
        }
        pos = s.pos;
        vel = s.vel;
    }
    let (impact, s) = hit.expect("hit the wall");
    // The block in line with the center of the body's +X face.
    assert_eq!(impact.block, at(base, 3, 0, 0));
    assert_eq!(s.sweep.blocks[0], Some(impact.block));
    assert!(impact.speed > 4.0);
    assert_eq!(s.vel.x, 0.0, "no bounce for a player");
    // Flush against the wall: max x of the box (0.3 from the center) on the plane x = 3.
    assert_eq!(s.pos.block.x - base.x, 2);
    assert!((s.pos.frac.x - 0.7).abs() < 1e-12, "{:?}", s.pos);
}

#[test]
fn a_step_into_an_unloaded_chunk_does_nothing() {
    let base = I64Vec3::new(0, 0, 0);
    let mut world = floor(base);
    let pos = WorldPos::from_block(at(base, 31, 5, 0), DVec3::new(0.9, 0.0, 0.5));
    world.set_unloaded(ChunkPos::new(1, 0, 0), true);
    let out = step_body(
        &mut world,
        &table(),
        pos,
        DVec3::new(30.0, 0.0, 0.0),
        player(),
        false,
        &BodySettings::player(),
        &StepParams::default(),
    );
    assert_eq!(
        out.map(|_| ()).map_err(|u| u.block.x),
        Err(32),
        "the first unloaded block"
    );
}

#[test]
fn predict_matches_a_flight_through_open_air_bit_for_bit() {
    let mut world = TestWorld::new();
    let start = WorldPos::from_block(BlockPos::new(-7, 300, 2), DVec3::new(0.125, 0.5, 0.875));
    let vel = DVec3::new(30.0, 12.0, -7.5);
    let settings = BodySettings::arrow();
    let params = StepParams::default();
    let steps = run(&mut world, start, vel, &settings, 150);
    for n in [1, 2, 17, 150] {
        let (pos, v) = predict(start, vel, &settings, &params, n);
        let s = &steps[n as usize - 1];
        assert_eq!((pos, v), (s.pos, s.vel), "step {n}");
    }
}

/// A whole course of one body relative to `base`, floats as bits.
fn flight(base: I64Vec3) -> Vec<(I64Vec3, [u64; 3], [u64; 3], u8)> {
    let mut world = floor(base);
    fill(&mut world, base, [4, 0, -8], [4, 2, 8], STONE);
    let mut pos = WorldPos::from_block(at(base, 0, 6, 0), DVec3::new(0.3, 0.25, 0.7));
    let mut vel = DVec3::new(6.0, 3.0, -2.5);
    let mut on_ground = false;
    let settings = BodySettings::ball();
    let ball = LocalBox::from_center_half(DVec3::ZERO, DVec3::splat(0.3));
    let mut out = Vec::new();
    for _ in 0..256 {
        let s = step_body(
            &mut world,
            &table(),
            pos,
            vel,
            ball,
            on_ground,
            &settings,
            &StepParams::default(),
        )
        .expect("loaded");
        pos = s.pos;
        vel = s.vel;
        on_ground = s.sweep.on_ground();
        out.push((
            pos.block.as_i64vec3() - base,
            pos.frac.to_array().map(f64::to_bits),
            vel.to_array().map(f64::to_bits),
            s.sweep.faces,
        ));
    }
    out
}

#[test]
fn flights_are_bit_identical_at_any_shift() {
    let reference = flight(I64Vec3::ZERO);
    assert!(
        reference
            .iter()
            .any(|s| s.3 & (1 << Face::PosX.index()) != 0),
        "hit the wall"
    );
    assert!(
        reference
            .iter()
            .any(|s| s.3 & (1 << Face::NegY.index()) != 0),
        "bounced"
    );
    for s in [
        1 << 31,
        -(1 << 31),
        1 << 40,
        1 << 52,
        MAX_BLOCK_COORD - 64,
        -(MAX_BLOCK_COORD - 64),
    ] {
        for shift in [
            I64Vec3::new(s, 0, 0),
            I64Vec3::new(0, s, 0),
            I64Vec3::new(0, 0, s),
            I64Vec3::new(s, -s, s),
        ] {
            assert_eq!(flight(shift), reference, "shift {shift:?}");
        }
    }
}
