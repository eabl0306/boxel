//! Sleeping bodies (phase P4) in a headless app, on the harness of `sim`: a flat test world (the
//! ground at y = 4), `PhysicsPlugin` and a synthetic clock.
//!
//! Bodies fall asleep after resting `sleep_steps` steps and then cost nothing; they wake on a
//! velocity, a controller that wants to move, a teleport, a push, a support that moves or goes
//! away, a block change near them, a chunk unload (then they freeze) and a settings change.
//! Sleeping bodies keep their sensor overlaps and are still found by queries and rays.
mod common;
mod sim;

use boxel_app::FixedPostUpdate;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BodyFilter, BodyIndex, BodySettings, CharacterController, Collider, Contacts, Interpolated,
    LocalBox, MoveIntent, PhysicsQuery, PhysicsStats, RayFilter, RayHit, RigidBody, Sleeping,
    Teleport, Velocity,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockState, WorldPos};
use boxel_world::ChunkMap;
use common::STONE;
use sim::{Collisions, STEP, Sim};

/// A crate: a solid cube of 0.75 standing on its position.
fn crate_box() -> Collider {
    Collider::standing(0.75, 0.75)
}

/// Steps until `entity` sleeps (at most `max` steps); returns how many it took.
fn until_asleep(sim: &mut Sim, entity: Entity, max: usize) -> usize {
    for n in 0..max {
        if sim.asleep(entity) {
            return n;
        }
        sim.steps(1);
    }
    panic!("not asleep after {max} steps: {:?}", sim.position(entity));
}

fn stats(sim: &Sim) -> PhysicsStats {
    *sim.app.world().resource::<PhysicsStats>()
}

/// Sets `entity`'s velocity (as gameplay does in a fixed step or in `Update`).
fn set_velocity(sim: &mut Sim, entity: Entity, v: DVec3) {
    sim.app
        .world_mut()
        .get_mut::<Velocity>(entity)
        .expect("body")
        .0 = v;
}

#[test]
fn a_resting_crate_falls_asleep_and_stays_exactly_in_place() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.body(
        sim.pos(0.5, 6.0, 0.5),
        (crate_box(), Interpolated::default()),
    );
    let steps = until_asleep(&mut sim, body, 256);
    // About 20 steps to fall 2 blocks, then 32 still ones.
    assert!((40..=80).contains(&steps), "{steps}");
    let rest = sim.pos(0.5, 4.0, 0.5);
    assert_eq!(sim.position(body), rest);
    assert_eq!(sim.get::<Velocity>(body).0, DVec3::ZERO);
    assert_eq!(
        sim.translation(body),
        rest.to_dvec3(),
        "no interpolation left"
    );
    assert!(sim.get::<Contacts>(body).on_ground);
    sim.steps(1);
    let s = stats(&sim);
    assert_eq!((s.bodies, s.dynamic, s.sleeping), (1, 0, 1), "{s:?}");
    // Asleep it does not move, and the index still has it.
    sim.steps(500);
    assert!(sim.asleep(body));
    assert_eq!(sim.position(body), rest);
    assert_eq!(sim.translation(body), rest.to_dvec3());
    let index = sim.app.world().resource::<BodyIndex>();
    assert_eq!(index.entities().collect::<Vec<_>>(), [body]);
    assert_eq!(index.len(), 1);
}

#[test]
fn a_stack_falls_asleep_exactly_and_stays() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let stack: Vec<Entity> = (0..4)
        .map(|i| sim.body(sim.pos(0.5, 5.0 + 2.0 * f64::from(i), 0.5), crate_box()))
        .collect();
    sim.steps(256);
    for (level, &body) in stack.iter().enumerate() {
        assert!(sim.asleep(body), "level {level}");
        assert_eq!(
            sim.position(body),
            sim.pos(0.5, 4.0 + 0.75 * level as f64, 0.5),
            "level {level}"
        );
    }
    let s = stats(&sim);
    assert_eq!(
        (s.sleeping, s.dynamic, s.pairs, s.pushes),
        (4, 0, 0, 0),
        "{s:?}"
    );
    let rest: Vec<_> = stack.iter().map(|&b| sim.position(b)).collect();
    sim.steps(1000);
    assert_eq!(
        stack.iter().map(|&b| sim.position(b)).collect::<Vec<_>>(),
        rest
    );
    assert!(stack.iter().all(|&b| sim.asleep(b)));
}

#[test]
fn a_velocity_wakes_a_sleeping_body_and_zero_does_not() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    until_asleep(&mut sim, body, 128);
    // Writing zero changes nothing.
    set_velocity(&mut sim, body, DVec3::ZERO);
    sim.steps(1);
    assert!(sim.asleep(body));
    // A jump wakes it the next step, and it flies.
    set_velocity(&mut sim, body, DVec3::new(0.0, 8.0, 0.0));
    sim.steps(1);
    assert!(!sim.asleep(body));
    assert!(sim.position(body).to_dvec3().y > 4.1);
    assert_eq!(stats(&sim).woken, 1);
    // It lands and sleeps again.
    until_asleep(&mut sim, body, 256);
    assert_eq!(sim.position(body), sim.pos(0.5, 4.0, 0.5));
}

#[test]
fn a_character_wakes_when_it_wants_to_walk_or_jump() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let walker = sim.spawn(sim.pos(0.5, 4.0, 0.5), CharacterController::default());
    until_asleep(&mut sim, walker, 128);
    sim.app
        .world_mut()
        .get_mut::<MoveIntent>(walker)
        .expect("intent")
        .wish = Vec2::X;
    sim.steps(32);
    assert!(!sim.asleep(walker));
    assert!(sim.position(walker).to_dvec3().x > 1.5);
    sim.app
        .world_mut()
        .get_mut::<MoveIntent>(walker)
        .expect("intent")
        .wish = Vec2::ZERO;
    until_asleep(&mut sim, walker, 256);
    // The jump latch on a sleeping character: one jump.
    sim.app
        .world_mut()
        .get_mut::<MoveIntent>(walker)
        .expect("intent")
        .jump = true;
    sim.steps(8);
    assert!(sim.position(walker).to_dvec3().y > 4.5);
    assert!(!sim.get::<MoveIntent>(walker).jump);
}

#[test]
fn a_teleport_wakes_a_sleeping_body() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.body(
        sim.pos(0.5, 4.0, 0.5),
        (crate_box(), Interpolated::default()),
    );
    until_asleep(&mut sim, body, 128);
    let up = sim.pos(3.5, 9.0, 0.5);
    sim.app.world_mut().entity_mut(body).insert(Teleport(up));
    sim.steps(1);
    assert!(!sim.asleep(body));
    assert!(sim.position(body).to_dvec3().y < 9.0, "falling from there");
    until_asleep(&mut sim, body, 256);
    assert_eq!(sim.position(body), sim.pos(3.5, 4.0, 0.5));
    // A `Transform` write too.
    sim.app
        .world_mut()
        .get_mut::<Transform>(body)
        .expect("body")
        .translation = sim.pos(-2.5, 7.0, 0.5).to_dvec3();
    sim.steps(1);
    assert!(!sim.asleep(body));
    until_asleep(&mut sim, body, 256);
    assert_eq!(sim.position(body), sim.pos(-2.5, 4.0, 0.5));
}

#[test]
fn walking_into_a_sleeping_crate_wakes_and_pushes_it() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let crate_ = sim.body(sim.pos(2.5, 4.0, 0.5), crate_box());
    let walker = sim.spawn(sim.pos(-1.5, 4.0, 0.5), CharacterController::default());
    until_asleep(&mut sim, crate_, 128);
    sim.app
        .world_mut()
        .get_mut::<MoveIntent>(walker)
        .expect("intent")
        .wish = Vec2::X;
    sim.steps(96);
    assert!(sim.position(crate_).to_dvec3().x > 3.0, "pushed");
    assert!(
        sim.position(walker).to_dvec3().x + 0.3 <= sim.position(crate_).to_dvec3().x - 0.375 + 1e-6,
        "never inside it"
    );
}

#[test]
fn a_crate_wakes_when_the_one_under_it_moves_away_or_goes() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let bottom = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    let top = sim.body(sim.pos(0.5, 4.75, 0.5), crate_box());
    sim.steps(64);
    assert!(sim.asleep(bottom) && sim.asleep(top));
    // The bottom crate is knocked away: the top one falls to the ground.
    set_velocity(&mut sim, bottom, DVec3::new(24.0, 0.0, 0.0));
    sim.steps(96);
    assert_eq!(
        sim.position(top),
        sim.pos(0.5, 4.0, 0.5),
        "fell to the ground"
    );
    assert!(sim.position(bottom).to_dvec3().x > 1.5);

    // A stack whose bottom crate is despawned.
    let bottom = sim.body(sim.pos(-4.5, 4.0, 0.5), crate_box());
    let top = sim.body(sim.pos(-4.5, 4.75, 0.5), crate_box());
    sim.steps(64);
    assert!(sim.asleep(bottom) && sim.asleep(top));
    sim.app.world_mut().despawn(bottom);
    sim.steps(32);
    assert_eq!(sim.position(top), sim.pos(-4.5, 4.0, 0.5));

    // And one whose bottom crate loses its collider.
    let bottom = sim.body(sim.pos(6.5, 4.0, 0.5), crate_box());
    let top = sim.body(sim.pos(6.5, 4.75, 0.5), crate_box());
    sim.steps(64);
    assert!(sim.asleep(bottom) && sim.asleep(top));
    sim.app.world_mut().entity_mut(bottom).remove::<Collider>();
    sim.steps(32);
    assert_eq!(sim.position(top), sim.pos(6.5, 4.0, 0.5));
}

#[test]
fn a_kinematic_platform_wakes_and_lifts_a_sleeping_crate() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let at = sim.pos(0.5, 4.0, 0.5);
    let platform = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Kinematic,
            Collider::standing(2.0, 0.5),
            Teleport(at),
        ))
        .id();
    let crate_ = sim.body(sim.pos(0.5, 6.0, 0.5), crate_box());
    until_asleep(&mut sim, crate_, 256);
    assert_eq!(sim.position(crate_), sim.pos(0.5, 4.5, 0.5));
    set_velocity(&mut sim, platform, DVec3::new(0.0, 2.0, 0.0));
    sim.steps(64);
    assert!(!sim.asleep(crate_));
    let lifted = sim.position(crate_).to_dvec3().y;
    assert!(
        (lifted - (sim.position(platform).to_dvec3().y + 0.5)).abs() < 1e-9,
        "{lifted}"
    );
    // The platform goes back down: the crate follows it (its support moved away).
    set_velocity(&mut sim, platform, DVec3::new(0.0, -2.0, 0.0));
    sim.steps(64);
    set_velocity(&mut sim, platform, DVec3::ZERO);
    sim.steps(64);
    assert_eq!(sim.position(crate_), sim.pos(0.5, 4.5, 0.5));
}

#[test]
fn block_changes_near_a_sleeper_wake_it_and_far_ones_do_not() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    until_asleep(&mut sim, body, 128);
    // Three blocks away: still asleep.
    sim.edit(sim.block(3, 4, 0), STONE);
    sim.steps(2);
    assert!(sim.asleep(body));
    // One block away (its box plus one block): wakes, and sleeps again.
    sim.edit(sim.block(1, 5, 0), STONE);
    sim.steps(2);
    assert!(!sim.asleep(body));
    until_asleep(&mut sim, body, 128);
    // The block under it goes: it falls one block.
    sim.edit(sim.block(0, 3, 0), BlockState::AIR);
    sim.steps(48);
    assert_eq!(sim.position(body), sim.pos(0.5, 3.0, 0.5));
}

#[test]
fn an_unloaded_chunk_wakes_and_freezes_a_sleeper_and_a_reload_lets_it_rest() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    until_asleep(&mut sim, body, 128);
    // The only viewer leaves: the chunks around the body unload.
    let viewer = {
        let mut viewers = sim
            .app
            .world_mut()
            .query_filtered::<Entity, With<boxel_world::ChunkViewer>>();
        viewers.single(sim.app.world()).expect("one viewer")
    };
    sim.app
        .world_mut()
        .get_mut::<Transform>(viewer)
        .expect("viewer")
        .translation = DVec3::new(32.0 * 20.0, 4.0, 0.0);
    sim.settle();
    sim.steps(2);
    assert!(
        sim.app
            .world()
            .resource::<ChunkMap>()
            .block_at(sim.block(0, 3, 0))
            .is_none()
    );
    assert!(!sim.asleep(body));
    assert!(sim.frozen(body));
    assert_eq!(
        sim.position(body),
        sim.pos(0.5, 4.0, 0.5),
        "frozen in place"
    );
    // Back: it rests again on the regenerated ground and falls asleep.
    sim.app
        .world_mut()
        .get_mut::<Transform>(viewer)
        .expect("viewer")
        .translation = DVec3::ZERO;
    sim.settle();
    until_asleep(&mut sim, body, 128);
    assert!(!sim.frozen(body));
    assert_eq!(sim.position(body), sim.pos(0.5, 4.0, 0.5));
}

#[test]
fn sensor_overlaps_last_while_both_sleep() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let at = sim.pos(0.5, 4.5, 0.5);
    let zone = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Static,
            Collider::aabb(DVec3::new(2.0, 1.0, 2.0)).sensor(),
            Teleport(at),
        ))
        .id();
    let item = sim.body(
        sim.pos(0.5, 6.0, 0.5),
        (
            Collider::aabb(DVec3::splat(0.125)).sensor(),
            BodySettings::item(),
        ),
    );
    // A second item resting inside the first one.
    let other = sim.body(
        sim.pos(0.5, 6.0, 0.5),
        (
            Collider::aabb(DVec3::splat(0.125)).sensor(),
            BodySettings::item(),
        ),
    );
    until_asleep(&mut sim, item, 256);
    until_asleep(&mut sim, other, 256);
    sim.steps(300);
    let events = std::mem::take(&mut sim.app.world_mut().resource_mut::<Collisions>().0);
    assert!(events.iter().all(|&(started, ..)| started), "{events:?}");
    assert_eq!(
        events.len(),
        3,
        "item-zone, other-zone, item-other: {events:?}"
    );
    assert_eq!(stats(&sim).overlaps, 3);
    // Knocked out of the zone: the overlaps with it and with the other item end.
    set_velocity(&mut sim, item, DVec3::new(30.0, 6.0, 0.0));
    sim.steps(64);
    let events = std::mem::take(&mut sim.app.world_mut().resource_mut::<Collisions>().0);
    let ended: Vec<_> = events.iter().filter(|e| !e.0).collect();
    assert_eq!(ended.len(), 2, "{events:?}");
    assert!(ended.iter().all(|e| e.1 == item || e.2 == item));
    let _ = zone;
}

/// `PhysicsQuery::raycast` from a system.
fn ray(sim: &mut Sim, origin: WorldPos, dir: DVec3, exclude: &'static [Entity]) -> Option<RayHit> {
    #[derive(Resource)]
    struct Out(Option<RayHit>);
    let system = move |query: PhysicsQuery, mut commands: Commands| {
        let hit = query.raycast(
            origin,
            dir,
            20.0,
            RayFilter::new(
                boxel_physics::VoxelFilter::Collision,
                BodyFilter::excluding(exclude),
            ),
        );
        commands.insert_resource(Out(hit));
    };
    use boxel_ecs::system::RunSystemOnce;
    sim.app.world_mut().run_system_once(system).expect("runs");
    sim.app.world_mut().flush();
    sim.app.world_mut().remove_resource::<Out>().expect("ran").0
}

#[test]
fn queries_and_rays_find_sleeping_bodies() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.body(sim.pos(3.5, 4.0, 0.5), crate_box());
    until_asleep(&mut sim, body, 128);
    let from = sim.pos(0.5, 4.5, 0.5);
    let hit = ray(&mut sim, from, DVec3::X, &[]).expect("hit");
    assert_eq!(hit.entity(), Some(body));
    assert!((hit.t() - 2.625).abs() < 1e-12, "{hit:?}");
    let index = sim.app.world().resource::<BodyIndex>();
    assert_eq!(
        index.overlap_box(
            sim.pos(3.5, 4.5, 0.5),
            LocalBox::new(DVec3::splat(-0.1), DVec3::splat(0.1)),
            &BodyFilter::default()
        ),
        [body]
    );
    assert!(!index.block_is_free(sim.block(3, 4, 0)));
    assert!(index.block_is_free(sim.block(5, 4, 0)));
    assert!(sim.asleep(body), "queries wake nothing");
}

#[test]
fn sleeping_can_be_turned_off_and_set_by_hand() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let a = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    let b = sim.body(sim.pos(3.5, 4.0, 0.5), crate_box());
    sim.steps(64);
    assert!(sim.asleep(a) && sim.asleep(b));
    // Off: everything wakes and nothing sleeps.
    sim.settings().sleep_steps = 0;
    sim.steps(64);
    assert!(!sim.asleep(a) && !sim.asleep(b));
    assert_eq!(stats(&sim).sleeping, 0);
    // A body put to sleep by hand stays asleep (hanging in the air) until woken by hand.
    sim.settings().sleep_steps = 32;
    let floating = sim.body(sim.pos(-3.5, 10.0, 0.5), crate_box());
    sim.steps(1);
    sim.app.world_mut().entity_mut(floating).insert(Sleeping);
    sim.steps(64);
    assert!(sim.asleep(floating));
    let held = sim.position(floating);
    assert!(held.to_dvec3().y > 9.0);
    sim.app
        .world_mut()
        .entity_mut(floating)
        .remove::<Sleeping>();
    sim.steps(128);
    assert_eq!(sim.position(floating), sim.pos(-3.5, 4.0, 0.5));
    // Changing gravity wakes every sleeper.
    sim.steps(64);
    assert!(sim.asleep(a));
    sim.settings().gravity = DVec3::new(0.0, 32.0, 0.0);
    sim.steps(16);
    assert!(sim.position(a).to_dvec3().y > 4.5, "falls up");
}

#[test]
fn a_sleeping_body_that_is_despawned_leaves_no_trace() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.body(sim.pos(0.5, 4.0, 0.5), crate_box());
    until_asleep(&mut sim, body, 128);
    sim.app.world_mut().despawn(body);
    sim.steps(2);
    let index = sim.app.world().resource::<BodyIndex>();
    assert!(index.is_empty());
    assert!(index.block_is_free(sim.block(0, 4, 0)));
    assert_eq!(stats(&sim).sleeping, 0);
}

#[test]
fn many_sleepers_and_a_trickle_of_wakes_keep_the_index_right() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // 400 crates on a 20 x 20 grid fall asleep; then one wakes every few steps.
    let mut crates = Vec::new();
    for i in 0..20 {
        for j in 0..20 {
            let x = -15.0 + 1.5 * f64::from(i);
            let z = -15.0 + 1.5 * f64::from(j);
            crates.push(sim.body(sim.pos(x, 4.0, z), crate_box()));
        }
    }
    sim.steps(64);
    assert!(crates.iter().all(|&c| sim.asleep(c)));
    for (k, &c) in crates.iter().enumerate().step_by(7) {
        set_velocity(&mut sim, c, DVec3::new(0.0, 6.0, 0.0));
        sim.steps(1 + k % 3);
    }
    sim.steps(128);
    assert!(crates.iter().all(|&c| sim.asleep(c)));
    let index = sim.app.world().resource::<BodyIndex>();
    let mut all: Vec<_> = crates.clone();
    all.sort_unstable_by_key(|e| e.to_bits());
    assert_eq!(index.entities().collect::<Vec<_>>(), all);
    for &c in &crates {
        let p = sim.position(c);
        assert_eq!(p.frac.y, 0.0, "back on the ground: {p:?}");
        assert!(!index.block_is_free(p.block));
    }
}

#[test]
fn the_sleep_systems_have_no_ambiguities_with_physics() {
    // `FixedPostUpdate` with every physics set, built strictly (ambiguities are errors).
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    sim.app.edit_schedule(FixedPostUpdate, |schedule| {
        schedule.set_build_settings(boxel_ecs::schedule::ScheduleBuildSettings {
            ambiguity_detection: boxel_ecs::schedule::LogLevel::Error,
            ..Default::default()
        });
    });
    sim.steps(2);
}
