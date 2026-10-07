//! Collisions between bodies in a headless app (phase P3), on the harness of `sim`: a flat test
//! world (the ground at y = 4), `PhysicsPlugin` and a synthetic clock.
//!
//! Stacks that rest exactly (also after a fast fall), pushes on every axis split by weight, a
//! push that never enters a wall, kinematic bodies that push and lift, static obstacles,
//! sensors and layers, sensor and contact events (once each, also after a despawn), entity
//! raycasts merged with the voxel raycast, box queries, the block placement check,
//! determinism (two runs, 144 vs 64 fps, interpolation on or off) and the same scene far from
//! the origin.
mod common;
mod sim;

use boxel_app::FixedUpdate;
use boxel_ecs::prelude::*;
use boxel_ecs::system::RunSystemOnce;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BodyFilter, BodyIndex, BodySettings, CharacterController, Collider, CollisionLayers,
    CollisionShape, Contacts, Interpolated, LocalBox, MoveIntent, PhysicsQuery, PhysicsSettings,
    PhysicsStats, RayFilter, RayHit, ReportContacts, RigidBody, Teleport, Velocity, VoxelFilter,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, Face, MAX_BLOCK_COORD, WorldPos};
use boxel_world::ChunkMap;
use common::STONE;
use sim::{Collisions, FRAME_144, Impacts, STEP, Sim};
use std::time::Duration;

/// A crate: a solid cube of 0.75 standing on its position.
fn crate_box() -> Collider {
    Collider::standing(0.75, 0.75)
}

/// `sim.pos` as an `f64` vector relative to the base (exact for these small scenes).
fn local(sim: &Sim, p: WorldPos) -> DVec3 {
    WorldPos::from_block(BlockPos::from_i64vec3(sim.base), DVec3::ZERO).delta_to(&p)
}

/// The collision events logged so far, then cleared.
fn take_collisions(sim: &mut Sim) -> Vec<(bool, Entity, Entity)> {
    std::mem::take(&mut sim.app.world_mut().resource_mut::<Collisions>().0)
}

/// The pair `(a, b)` sorted by entity bits, as the events give it.
fn pair(a: Entity, b: Entity) -> (Entity, Entity) {
    if a.to_bits() < b.to_bits() {
        (a, b)
    } else {
        (b, a)
    }
}

/// Fills the blocks `base + min ..= base + max` with stone and waits for the edit.
fn wall(sim: &mut Sim, min: [i64; 3], max: [i64; 3]) {
    for y in min[1]..=max[1] {
        for x in min[0]..=max[0] {
            for z in min[2]..=max[2] {
                sim.edit(sim.block(x, y, z), STONE);
            }
        }
    }
    let last = sim.block(max[0], max[1], max[2]);
    while sim.app.world().resource::<ChunkMap>().block_at(last) != Some(STONE) {
        sim.idle_frame();
    }
}

#[test]
fn crates_dropped_on_each_other_stack_and_rest_exactly() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // Awake all along: every step pushes each pair (sleeping is tested in `sleep.rs`).
    sim.app
        .world_mut()
        .resource_mut::<PhysicsSettings>()
        .sleep_steps = 0;
    // Spawned top first: the higher crates have the lower entities, so pair order alone would
    // push the top of the stack first.
    let mut stack: Vec<Entity> = (0..4)
        .rev()
        .map(|i| sim.body(sim.pos(0.5, 5.0 + 2.0 * f64::from(i), 0.5), crate_box()))
        .collect();
    stack.reverse();
    sim.steps(256);
    for (level, &body) in stack.iter().enumerate() {
        assert_eq!(
            sim.position(body),
            sim.pos(0.5, 4.0 + 0.75 * level as f64, 0.5),
            "level {level}"
        );
        assert_eq!(sim.get::<Velocity>(body).0, DVec3::ZERO);
        let contacts = sim.get::<Contacts>(body);
        assert!(
            contacts.on_ground && contacts.touched(Face::NegY),
            "{level}"
        );
        if level < 3 {
            assert!(contacts.touched(Face::PosY), "something on top of {level}");
        }
    }
    // On the ground block only the bottom one; the others stand on crates.
    assert_eq!(
        sim.get::<Contacts>(stack[0]).ground_block,
        Some(sim.block(0, 3, 0))
    );
    assert_eq!(sim.get::<Contacts>(stack[1]).ground_block, None);
    // Resting: a thousand more steps change no bit, and no crate ever reaches the ground.
    let rest: Vec<_> = stack.iter().map(|&b| sim.position(b)).collect();
    sim.steps(1000);
    let after: Vec<_> = stack.iter().map(|&b| sim.position(b)).collect();
    assert_eq!(after, rest);
    let stats = *sim.app.world().resource::<PhysicsStats>();
    assert_eq!(stats.pairs, 3, "{stats:?}");
    assert_eq!(stats.pushes, 3, "each pair once per step: {stats:?}");
}

#[test]
fn a_crate_falling_fast_lands_on_top_not_through() {
    // At 60 blocks/s a crate moves 0.94 blocks per step: more than the height of the crate it
    // falls on. From these starts it sinks between 0 and 0.94 into it on the step it lands.
    for k in 0..8 {
        let mut sim = Sim::new(I64Vec3::ZERO, STEP);
        let bottom = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
        let start = 12.0 + f64::from(k) * 0.125;
        let top = sim.body(
            sim.pos(0.5, start, 0.5),
            (crate_box(), Velocity(DVec3::new(0.0, -60.0, 0.0))),
        );
        sim.steps(64);
        assert_eq!(
            sim.position(bottom),
            sim.pos(0.5, 4.0, 0.5),
            "start {start}"
        );
        assert_eq!(sim.position(top), sim.pos(0.5, 4.75, 0.5), "start {start}");
    }
}

#[test]
fn overlapping_bodies_are_pushed_apart_on_x_and_z_by_weight() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // Overlapping by 0.25 along X, the same weight: each moves 0.125.
    let a = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    let b = sim.body(sim.pos(1.0, 4.0, 0.5), crate_box());
    // Along Z, `d` three times as heavy as `c`: `c` moves 0.1875, `d` 0.0625.
    let heavy = BodySettings {
        push_weight: 3.0,
        ..BodySettings::player()
    };
    let c = sim.body(sim.pos(0.5, 4.0, 10.5), crate_box());
    let d = sim.body(sim.pos(0.5, 4.0, 11.0), (crate_box(), heavy));
    sim.steps(1);
    assert_eq!(sim.position(a), sim.pos(0.375, 4.0, 0.5));
    assert_eq!(sim.position(b), sim.pos(1.125, 4.0, 0.5));
    assert_eq!(sim.position(c), sim.pos(0.5, 4.0, 10.3125));
    assert_eq!(sim.position(d), sim.pos(0.5, 4.0, 11.0625));
    assert!(sim.get::<Contacts>(a).touched(Face::PosX));
    assert!(sim.get::<Contacts>(b).touched(Face::NegX));
    assert!(sim.get::<Contacts>(d).touched(Face::NegZ));
    // Apart now: nothing moves any more.
    sim.steps(10);
    assert_eq!(sim.position(a), sim.pos(0.375, 4.0, 0.5));
    assert_eq!(sim.position(d), sim.pos(0.5, 4.0, 11.0625));
    assert_eq!(sim.app.world().resource::<PhysicsStats>().pushes, 0);
}

#[test]
fn a_push_never_moves_a_body_into_a_wall() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    wall(&mut sim, [3, 4, -1], [3, 5, 1]);
    // `a` touches the wall (x = 3); `b` overlaps `a` from the west by 0.25.
    let a = sim.body(sim.pos(2.625, 4.0, 0.5), crate_box());
    let b = sim.body(sim.pos(2.125, 4.0, 0.5), crate_box());
    sim.steps(1);
    assert_eq!(sim.position(a), sim.pos(2.625, 4.0, 0.5), "the wall wins");
    assert_eq!(
        sim.position(b),
        sim.pos(1.875, 4.0, 0.5),
        "b takes the push"
    );
    assert!(sim.get::<Contacts>(a).touched(Face::PosX));
}

#[test]
fn kinematic_bodies_push_dynamic_ones_the_whole_way_and_lift_them() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // A kinematic block sliding east at 2 blocks/s into a crate on the ground.
    let pusher_at = sim.pos(-2.0, 4.0, 0.5);
    let pusher = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Kinematic,
            Collider::standing(1.0, 1.0),
            Teleport(pusher_at),
            Velocity(DVec3::new(2.0, 0.0, 0.0)),
        ))
        .id();
    let pushed = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    // A kinematic platform rising at 1 block/s with a crate on it.
    let platform_at = sim.pos(0.5, 4.25, 10.5);
    let platform = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Kinematic,
            Collider::aabb(DVec3::new(1.0, 0.25, 1.0)),
            Teleport(platform_at),
            Velocity(DVec3::new(0.0, 1.0, 0.0)),
        ))
        .id();
    let rider = sim.body(sim.pos(0.5, 4.5, 10.5), crate_box());
    sim.steps(128);

    // The pusher moved 4 blocks (its east face at x = 2.5): the crate is in front of it.
    assert_eq!(sim.position(pusher), sim.pos(2.0, 4.0, 0.5));
    let crate_west = local(&sim, sim.position(pushed)).x - 0.375;
    assert!((crate_west - 2.5).abs() < 1e-9, "{crate_west}");
    assert_eq!(local(&sim, sim.position(pushed)).y, 4.0);

    // The platform rose 2 blocks (its top at 6.5): the crate rides on it.
    assert_eq!(sim.position(platform), sim.pos(0.5, 6.25, 10.5));
    let ride = local(&sim, sim.position(rider));
    assert!((ride.y - 6.5).abs() < 1e-9, "{ride:?}");
    assert_eq!((ride.x, ride.z), (0.5, 10.5));
    assert!(sim.get::<Contacts>(rider).on_ground);
}

#[test]
fn static_colliders_are_obstacles_for_solid_bodies_of_their_layers() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // A collider alone is a static body (it requires `RigidBody::Static`).
    let block = |x: f64| {
        (
            Collider::standing(1.0, 1.0),
            Transform::from_xyz(x, 4.0, 0.5),
        )
    };
    let first = sim.app.world_mut().spawn(block(0.5)).id();
    sim.app.world_mut().spawn(block(3.5));
    sim.app.world_mut().spawn(block(6.5));
    assert_eq!(
        *sim.app.world().get::<RigidBody>(first).expect("required"),
        RigidBody::Static
    );
    // A crate lands on the first block's top, exactly, with no voxel impact.
    let on_top = sim.body(sim.pos(0.5, 9.0, 0.5), crate_box());
    // An item (a dynamic sensor) falls through the second one onto the grass.
    let item = sim.body(
        sim.pos(3.5, 9.0, 0.5),
        (
            Collider::aabb(DVec3::splat(0.125)).sensor(),
            BodySettings::item(),
        ),
    );
    // A crate in another layer falls through the third one.
    let ghost = sim.body(
        sim.pos(6.5, 9.0, 0.5),
        crate_box().with_layers(CollisionLayers::new(2, 2)),
    );
    sim.steps(128);
    assert_eq!(sim.position(on_top), sim.pos(0.5, 5.0, 0.5));
    let contacts = sim.get::<Contacts>(on_top);
    assert!(contacts.on_ground && contacts.ground_block.is_none());
    assert!(
        sim.app
            .world()
            .resource::<Impacts>()
            .0
            .iter()
            .all(|i| i.entity != on_top),
        "obstacles are not voxel impacts"
    );
    assert_eq!(sim.position(item), sim.pos(3.5, 4.125, 0.5));
    assert_eq!(sim.position(ghost), sim.pos(6.5, 4.0, 0.5));
}

#[test]
fn a_sensor_reports_each_overlap_once_and_a_despawn_ends_it() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // A static sensor zone 2 blocks wide around x = 5.5.
    let zone = sim
        .app
        .world_mut()
        .spawn((
            Collider::aabb(DVec3::ONE).sensor(),
            Transform::from_xyz(5.5, 5.0, 0.5),
        ))
        .id();
    // A walker crosses it heading east and stops well past it.
    let walker = sim.spawn(sim.pos(0.5, 4.0, 0.5), CharacterController::default());
    let mut inside = Vec::new();
    for step in 0..256 {
        let wish = if step < 160 { Vec2::X } else { Vec2::ZERO };
        sim.app
            .world_mut()
            .get_mut::<MoveIntent>(walker)
            .expect("intent")
            .wish = wish;
        sim.steps(1);
        let found = sim.app.world().resource::<BodyIndex>().overlap_box(
            sim.pos(5.5, 5.0, 0.5),
            LocalBox::new(DVec3::NEG_ONE, DVec3::ONE),
            &BodyFilter::default(),
        );
        inside.push(found == [walker]);
    }
    assert!(local(&sim, sim.position(walker)).x > 9.0);
    let (a, b) = pair(zone, walker);
    assert_eq!(take_collisions(&mut sim), [(true, a, b), (false, a, b)]);
    // The queries agree: one stretch of steps inside.
    let entered = inside.iter().position(|&i| i).expect("entered");
    let left = entered + inside[entered..].iter().position(|&i| !i).expect("left");
    assert!(inside[left..].iter().all(|&i| !i));

    // A crate dropped into the zone, then despawned while inside.
    let doomed = sim.body(sim.pos(5.5, 4.0, 0.5), crate_box());
    sim.steps(2);
    let (a, b) = pair(zone, doomed);
    assert_eq!(take_collisions(&mut sim), [(true, a, b)]);
    sim.app.world_mut().despawn(doomed);
    sim.steps(2);
    assert_eq!(take_collisions(&mut sim), [(false, a, b)]);
}

#[test]
fn solid_contacts_report_events_only_with_report_contacts() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let base = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    let quiet = sim.body(sim.pos(0.5, 6.0, 0.5), crate_box());
    sim.steps(64);
    assert_eq!(sim.position(quiet), sim.pos(0.5, 4.75, 0.5));
    assert!(take_collisions(&mut sim).is_empty(), "no ReportContacts");

    // With `ReportContacts` a crate landing on another starts one contact, which lasts while it
    // rests there.
    let lower = sim.body(sim.pos(3.5, 4.0, 0.5), crate_box());
    let upper = sim.body(sim.pos(3.5, 6.0, 0.5), (crate_box(), ReportContacts));
    sim.steps(256);
    let (a, b) = pair(lower, upper);
    assert_eq!(take_collisions(&mut sim), [(true, a, b)]);
    assert_eq!(sim.app.world().resource::<PhysicsStats>().overlaps, 1);
    // The lower one goes: the upper one falls onto the grass and the contact ends.
    sim.app.world_mut().despawn(lower);
    sim.steps(64);
    assert_eq!(take_collisions(&mut sim), [(false, a, b)]);
    assert_eq!(sim.position(upper), sim.pos(3.5, 4.0, 0.5));
    let _ = base;
}

#[test]
fn contacts_with_static_and_kinematic_solids_are_reported() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // A crate lands on a static platform: the sweep stops it on the platform's top, so the two
    // only touch. The contact starts once and lasts while it rests there.
    let at = sim.pos(0.5, 4.0, 0.5);
    let platform = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Static,
            Collider::standing(2.0, 0.5),
            Teleport(at),
        ))
        .id();
    let crate_ = sim.body(sim.pos(0.5, 6.0, 0.5), (crate_box(), ReportContacts));
    sim.steps(128);
    assert_eq!(sim.position(crate_), sim.pos(0.5, 4.5, 0.5));
    let (a, b) = pair(platform, crate_);
    assert_eq!(take_collisions(&mut sim), [(true, a, b)]);
    // Moved off it: the contact ends.
    let away = sim.pos(5.5, 4.0, 0.5);
    sim.app
        .world_mut()
        .entity_mut(crate_)
        .insert(Teleport(away));
    sim.steps(2);
    assert_eq!(take_collisions(&mut sim), [(false, a, b)]);

    // A player walking into a kinematic wall (stopped by it, touching) reports it while it
    // presses.
    let at = sim.pos(-4.5, 5.0, 0.5);
    let wall = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Kinematic,
            Collider::aabb(DVec3::new(0.5, 1.0, 2.0)),
            Teleport(at),
        ))
        .id();
    let walker = sim.spawn(
        sim.pos(-1.5, 4.0, 0.5),
        (CharacterController::default(), ReportContacts),
    );
    sim.app
        .world_mut()
        .get_mut::<MoveIntent>(walker)
        .expect("intent")
        .wish = Vec2::NEG_X;
    sim.steps(64);
    assert!((local(&sim, sim.position(walker)).x - (-3.7)).abs() < 1e-9);
    let (a, b) = pair(wall, walker);
    assert_eq!(take_collisions(&mut sim), [(true, a, b)]);
    sim.steps(32);
    assert!(take_collisions(&mut sim).is_empty(), "it lasts");
}

#[test]
fn a_contact_lasts_when_the_upper_body_sleeps_on_an_awake_one() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    wall(&mut sim, [3, 4, -1], [3, 6, 1]);
    // A character pressed against a wall never rests (it wants to walk), and a crate with
    // `ReportContacts` stands on its head: the crate falls asleep there.
    let walker = sim.spawn(sim.pos(1.5, 4.0, 0.5), CharacterController::default());
    sim.app
        .world_mut()
        .get_mut::<MoveIntent>(walker)
        .expect("intent")
        .wish = Vec2::X;
    let crate_ = sim.body(sim.pos(2.5, 7.0, 0.5), (crate_box(), ReportContacts));
    for _ in 0..256 {
        sim.steps(1);
        if sim.asleep(crate_) {
            break;
        }
    }
    assert!(sim.asleep(crate_));
    assert!(!sim.asleep(walker));
    let (a, b) = pair(walker, crate_);
    assert_eq!(take_collisions(&mut sim), [(true, a, b)]);
    // It still rests on the awake character: no `CollisionEnded`.
    sim.steps(64);
    assert!(sim.asleep(crate_));
    assert!(take_collisions(&mut sim).is_empty());
    assert_eq!(sim.app.world().resource::<PhysicsStats>().overlaps, 1);
}

#[test]
fn a_projectile_spawned_without_a_dynamic_body_is_no_obstacle() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // `Collider` alone makes the body static: such a projectile never flies, and must not
    // block anything either.
    let at = sim.pos(0.5, 5.0, 0.5);
    sim.app.world_mut().spawn((
        boxel_physics::Projectile::arrow(),
        Collider::aabb(DVec3::new(1.0, 0.25, 1.0)),
        Teleport(at),
    ));
    let crate_ = sim.body(sim.pos(0.5, 7.0, 0.5), crate_box());
    sim.steps(96);
    assert_eq!(
        sim.position(crate_),
        sim.pos(0.5, 4.0, 0.5),
        "fell through it"
    );
}

#[test]
fn a_box_query_of_the_whole_world_finds_every_body() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let mut crates: Vec<Entity> = (0..12)
        .map(|i| sim.body(sim.pos(-9.5 + 1.5 * f64::from(i), 4.0, 0.5), crate_box()))
        .collect();
    sim.steps(64);
    crates.sort_unstable_by_key(|e| e.to_bits());
    let index = sim.app.world().resource::<BodyIndex>();
    let everything = LocalBox::new(DVec3::splat(f64::NEG_INFINITY), DVec3::splat(f64::INFINITY));
    assert_eq!(
        index.overlap_box(sim.pos(0.0, 0.0, 0.0), everything, &BodyFilter::default()),
        crates
    );
}

#[test]
fn bodies_in_layers_that_do_not_meet_overlap_freely() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let a = sim.body(
        sim.pos(0.5, 4.0, 0.5),
        (
            crate_box().with_layers(CollisionLayers::new(1, 1)),
            ReportContacts,
        ),
    );
    let b = sim.body(
        sim.pos(0.75, 4.0, 0.5),
        crate_box().with_layers(CollisionLayers::new(2, u32::MAX)),
    );
    sim.steps(16);
    assert_eq!(sim.position(a), sim.pos(0.5, 4.0, 0.5));
    assert_eq!(sim.position(b), sim.pos(0.75, 4.0, 0.5));
    assert!(take_collisions(&mut sim).is_empty());
    assert_eq!(sim.app.world().resource::<PhysicsStats>().pairs, 0);
}

/// `PhysicsQuery::raycast` from a system.
fn raycast(
    sim: &mut Sim,
    origin: WorldPos,
    dir: DVec3,
    max_t: f64,
    voxels: bool,
    sensors: bool,
    exclude: Vec<Entity>,
) -> Option<RayHit> {
    sim.app
        .world_mut()
        .run_system_once(move |physics: PhysicsQuery| {
            let filter = RayFilter {
                voxels: voxels.then_some(VoxelFilter::Collision),
                bodies: BodyFilter::excluding(&exclude).with_sensors(sensors),
            };
            physics.raycast(origin, dir, max_t, filter)
        })
        .expect("runs")
}

#[test]
fn rays_hit_bodies_and_blocks_in_order_and_queries_find_bodies() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    wall(&mut sim, [8, 4, -1], [8, 5, 1]);
    // On the line y = 4.375, z = 0.5 going east: a crate (x 2.125..2.875), a sensor item at
    // x = 4.5, a static ball (radius 0.5 at x = 6.5, y = 4.5), the wall (x = 8) and a crate
    // behind it.
    let near = sim.body(sim.pos(2.5, 4.0, 0.5), crate_box());
    let sensor = sim
        .app
        .world_mut()
        .spawn((
            Collider::aabb(DVec3::splat(0.25)).sensor(),
            Transform::from_xyz(4.5, 4.375, 0.5),
        ))
        .id();
    let ball = sim
        .app
        .world_mut()
        .spawn((Collider::sphere(0.5), Transform::from_xyz(6.5, 4.5, 0.5)))
        .id();
    let far = sim.body(sim.pos(9.5, 4.0, 0.5), crate_box());
    sim.steps(2);
    let origin = sim.pos(0.0, 4.375, 0.5);
    let east = DVec3::X;
    let hit = |hit: Option<RayHit>| hit.expect("hit");

    let first = hit(raycast(&mut sim, origin, east, 20.0, true, false, vec![]));
    assert_eq!(first.entity(), Some(near));
    assert_eq!(first.t(), 2.125);
    assert_eq!(first.point(), sim.pos(2.125, 4.375, 0.5));
    let RayHit::Body(body) = first else {
        unreachable!()
    };
    assert_eq!(body.normal, DVec3::NEG_X);
    // Sensors only when asked.
    let with_sensors = hit(raycast(
        &mut sim,
        origin,
        east,
        20.0,
        true,
        true,
        vec![near],
    ));
    assert_eq!(with_sensors.entity(), Some(sensor));
    // The ball, exactly: 6.5 - sqrt(0.5^2 - 0.125^2).
    let round = hit(raycast(
        &mut sim,
        origin,
        east,
        20.0,
        true,
        false,
        vec![near],
    ));
    assert_eq!(round.entity(), Some(ball));
    assert!((round.t() - (6.5 - (0.25f64 - 0.015625).sqrt())).abs() < 1e-12);
    // Then the wall, which hides the crate behind it unless blocks are ignored.
    let blocked = hit(raycast(
        &mut sim,
        origin,
        east,
        20.0,
        true,
        false,
        vec![near, ball],
    ));
    assert_eq!(
        blocked.block().map(|b| b.block),
        Some(sim.block(8, 4, 0)),
        "{blocked:?}"
    );
    let through = hit(raycast(
        &mut sim,
        origin,
        east,
        20.0,
        false,
        false,
        vec![near, ball],
    ));
    assert_eq!(through.entity(), Some(far));
    assert_eq!(through.t(), 9.125);
    // `max_t` ends the search.
    assert_eq!(
        raycast(&mut sim, origin, east, 2.0, true, false, vec![]),
        None
    );
    // A ray through the ball's bounding box corner misses the sphere itself.
    let corner = sim.pos(5.0, 4.95, 0.95);
    let index = sim.app.world().resource::<BodyIndex>();
    assert_eq!(
        index.raycast(corner, east, 3.0, &BodyFilter::default()),
        None
    );
    // A ray that starts inside a body hits it at once.
    let inside = index
        .raycast(sim.pos(2.5, 4.2, 0.5), east, 3.0, &BodyFilter::default())
        .expect("inside");
    assert_eq!(
        (inside.entity, inside.t, inside.normal),
        (near, 0.0, DVec3::ZERO)
    );

    // Box queries: touching is not overlapping.
    let at = |x, y, z| sim.pos(x, y, z);
    let unit = LocalBox::new(DVec3::ZERO, DVec3::ONE);
    let all = BodyFilter::default().with_sensors(true);
    assert_eq!(index.overlap_box(at(2.0, 4.5, 0.0), unit, &all), [near]);
    assert!(
        index
            .overlap_box(at(2.875, 4.0, 0.0), unit, &all)
            .is_empty()
    );
    assert_eq!(index.overlap_box(at(4.0, 4.0, 0.0), unit, &all), [sensor]);
    assert!(
        index
            .overlap_box(at(4.0, 4.0, 0.0), unit, &BodyFilter::default())
            .is_empty()
    );
    let wide = LocalBox::new(DVec3::new(0.0, 3.0, -1.0), DVec3::new(12.0, 6.0, 2.0));
    let mut everything = vec![near, sensor, ball, far];
    everything.sort_by_key(|e| e.to_bits());
    assert_eq!(index.overlap_box(at(0.0, 0.0, 0.0), wide, &all), everything);

    // The placement check: solid bodies only, spheres exactly.
    assert!(!index.block_is_free(sim.block(2, 4, 0)), "the crate");
    assert!(index.block_is_free(sim.block(3, 4, 0)));
    assert!(
        index.block_is_free(sim.block(4, 4, 0)),
        "a sensor does not count"
    );
    assert!(!index.block_is_free(sim.block(6, 4, 0)), "the ball");
    assert!(
        index.block_is_free(sim.block(6, 5, 0)),
        "the ball's top only touches"
    );
    // Shaped blocks (ADR 0018): only their boxes count, and a plant never collides.
    assert!(index.shape_is_free(sim.block(2, 4, 0), CollisionShape::Empty));
    assert!(!index.shape_is_free(sim.block(6, 4, 0), CollisionShape::Full));
    let corner = [LocalBox::new(DVec3::ZERO, DVec3::new(0.25, 0.0625, 0.25))];
    assert!(
        index.shape_is_free(sim.block(6, 4, 0), CollisionShape::Boxes(&corner)),
        "a box in the corner misses the ball"
    );
    let middle = [LocalBox::new(
        DVec3::new(0.0, 0.25, 0.0),
        DVec3::new(1.0, 0.75, 1.0),
    )];
    assert!(!index.shape_is_free(sim.block(6, 4, 0), CollisionShape::Boxes(&middle)));
}

// ------------------------------------------------------------------------------ determinism

/// A wandering mob of the scene: its wish and jumps come from its seed and the step number.
#[derive(Component)]
struct Mob(u64);

#[derive(Resource, Default)]
struct SceneSteps(u64);

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn wander(mut steps: ResMut<SceneSteps>, mut mobs: Query<(&Mob, &mut MoveIntent)>) {
    steps.0 += 1;
    for (mob, mut intent) in &mut mobs {
        let r = mix(mob.0 ^ (steps.0 / 24).wrapping_mul(0x1000_0001));
        let unit = |bits: u64| (bits & 0xff) as f32 / 127.5 - 1.0;
        intent.wish = Vec2::new(unit(r), unit(r >> 8));
        if steps.0.is_multiple_of(24) && (r >> 16) & 3 == 0 {
            intent.jump = true;
        }
    }
}

/// One body: block relative to the base, `frac` and velocity bits, contact faces.
type BodyState = (I64Vec3, [u64; 3], [u64; 3], u8);

/// A crowded pen: mobs that wander into each other and into crates (some stacked), bouncing
/// balls, items, a kinematic pusher, a static pillar and a sensor zone, between stone walls.
/// Returns every body's state in spawn order and the collision events as spawn indices.
fn scene(
    base: I64Vec3,
    frame: Duration,
    interpolated: bool,
    steps: u64,
) -> (Vec<BodyState>, Vec<(bool, usize, usize)>) {
    scene_with(base, frame, interpolated, steps, usize::MAX)
}

/// [`scene`] with the step on several threads above `parallel_threshold` bodies.
fn scene_with(
    base: I64Vec3,
    frame: Duration,
    interpolated: bool,
    steps: u64,
    parallel_threshold: usize,
) -> (Vec<BodyState>, Vec<(bool, usize, usize)>) {
    let mut sim = Sim::new(base, frame);
    sim.settings().parallel_threshold = parallel_threshold;
    sim.app
        .init_resource::<SceneSteps>()
        .add_systems(FixedUpdate, wander);
    wall(&mut sim, [-8, 4, -8], [8, 5, -8]);
    wall(&mut sim, [-8, 4, 8], [8, 5, 8]);
    wall(&mut sim, [-8, 4, -7], [-8, 5, 7]);
    wall(&mut sim, [8, 4, -7], [8, 5, 7]);
    let mut bodies = Vec::new();
    let mut add = |sim: &mut Sim, entity: Entity| {
        if interpolated {
            sim.app
                .world_mut()
                .entity_mut(entity)
                .insert(Interpolated::default());
        }
        bodies.push(entity);
    };
    for i in 0..6u32 {
        let x = -5.5 + 2.0 * f64::from(i);
        let mob = sim.spawn(
            sim.pos(x, 4.0, -3.5),
            (CharacterController::default(), Mob(u64::from(i) + 1)),
        );
        if i == 0 {
            sim.app.world_mut().entity_mut(mob).insert(ReportContacts);
        }
        add(&mut sim, mob);
    }
    for i in 0..3u32 {
        // Two stacks of three and loose crates.
        let level = f64::from(i);
        let a = sim.body(sim.pos(-3.5, 6.0 + 2.0 * level, 3.5), crate_box());
        let b = sim.body(sim.pos(3.25, 5.0 + 1.5 * level, 3.75), crate_box());
        let c = sim.body(sim.pos(-0.5 + level, 4.0, 0.5 + 0.25 * level), crate_box());
        for e in [a, b, c] {
            add(&mut sim, e);
        }
    }
    for i in 0..4u32 {
        let d = f64::from(i);
        let ball = sim.body(
            sim.pos(-4.0 + 2.5 * d, 9.0 + d, 1.5),
            (
                Collider::sphere(0.3),
                BodySettings::ball(),
                Velocity(DVec3::new(3.0 - 2.0 * d, 2.0, -1.0 + d)),
            ),
        );
        add(&mut sim, ball);
        let item = sim.body(
            sim.pos(0.5 + d, 8.0, -1.5),
            (
                Collider::aabb(DVec3::splat(0.125)).sensor(),
                BodySettings::item(),
                Velocity(DVec3::new(1.0, 3.0, 0.5 * d)),
            ),
        );
        add(&mut sim, item);
    }
    let pusher_at = sim.pos(-6.5, 4.0, 5.5);
    let pusher = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Kinematic,
            Collider::standing(1.0, 1.0),
            Teleport(pusher_at),
            Velocity(DVec3::new(1.5, 0.0, 0.0)),
        ))
        .id();
    add(&mut sim, pusher);
    let pillar_at = sim.pos(1.5, 4.0, -1.5);
    let pillar = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Static,
            Collider::standing(1.0, 2.0),
            Teleport(pillar_at),
        ))
        .id();
    add(&mut sim, pillar);
    let zone_at = sim.pos(0.5, 5.0, -4.5);
    let zone = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Static,
            Collider::aabb(DVec3::new(2.0, 1.0, 2.0)).sensor(),
            Teleport(zone_at),
        ))
        .id();
    add(&mut sim, zone);

    sim.app.world_mut().resource_mut::<SceneSteps>().0 = 0;
    while sim.app.world().resource::<SceneSteps>().0 < steps {
        sim.frame(frame);
    }
    let index_of = |e: Entity| bodies.iter().position(|&b| b == e).expect("scene body");
    let events = take_collisions(&mut sim)
        .into_iter()
        .map(|(started, a, b)| (started, index_of(a), index_of(b)))
        .collect();
    let states = bodies
        .iter()
        .map(|&entity| {
            let p = sim.position(entity);
            (
                p.block.as_i64vec3() - base,
                p.frac.to_array().map(f64::to_bits),
                sim.get::<Velocity>(entity).0.to_array().map(f64::to_bits),
                sim.get::<Contacts>(entity).faces,
            )
        })
        .collect();
    (states, events)
}

#[test]
fn crowded_runs_are_bit_identical_at_any_frame_rate_with_or_without_interpolation() {
    let reference = scene(I64Vec3::ZERO, STEP, false, 800);
    // Something happened: mobs met crates, the zone or each other.
    assert!(reference.1.len() >= 4, "{:?}", reference.1);
    assert_eq!(scene(I64Vec3::ZERO, STEP, false, 800), reference, "again");
    assert_eq!(
        scene(I64Vec3::ZERO, FRAME_144, false, 800),
        reference,
        "144 fps"
    );
    assert_eq!(
        scene(I64Vec3::ZERO, STEP, true, 800),
        reference,
        "interpolated"
    );
}

#[test]
fn the_crowded_scene_on_several_threads_is_bit_identical() {
    let reference = scene(I64Vec3::ZERO, STEP, true, 800);
    assert_eq!(
        scene_with(I64Vec3::ZERO, STEP, true, 800, 0),
        reference,
        "every step on the compute threads"
    );
    let far = MAX_BLOCK_COORD - 1024;
    assert_eq!(
        scene_with(I64Vec3::new(far, -far, far), STEP, true, 800, 0),
        reference,
        "far away, on the compute threads"
    );
}

#[test]
fn the_crowded_scene_far_away_is_bit_identical() {
    let far = MAX_BLOCK_COORD - 1024;
    let reference = scene(I64Vec3::ZERO, STEP, false, 300);
    for base in [
        I64Vec3::new(1 << 31, 0, 0),
        I64Vec3::new(0, 1 << 40, -(1 << 31)),
        I64Vec3::new(1 << 52, -(1 << 52), 1 << 40),
        I64Vec3::new(far, far, far),
        I64Vec3::new(-far, -far, -far),
    ] {
        assert_eq!(scene(base, STEP, true, 300), reference, "base {base:?}");
    }
}

#[test]
fn the_index_is_empty_before_the_first_step_and_follows_despawns() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    assert!(sim.app.world().resource::<BodyIndex>().is_empty());
    let a = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    let point = sim.body(sim.pos(3.5, 4.0, 0.5), ());
    sim.steps(1);
    let index = sim.app.world().resource::<BodyIndex>();
    assert_eq!(
        index.entities().collect::<Vec<_>>(),
        [a],
        "no collider, not indexed"
    );
    let _ = point;
    sim.app.world_mut().despawn(a);
    sim.steps(1);
    assert!(sim.app.world().resource::<BodyIndex>().is_empty());
    assert!(
        sim.app
            .world()
            .resource::<BodyIndex>()
            .block_is_free(BlockPos::new(0, 4, 0))
    );
}
