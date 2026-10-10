//! Projectiles (phase P4) in a headless app, on the harness of `sim`: a flat test world (the
//! ground at y = 4), `PhysicsPlugin` and a synthetic clock.
//!
//! An arrow at 60 blocks/s never misses a mob 0.6 wide, whatever the phase of its steps; arrows
//! stick in blocks (exactly on the face, also the top of a slab or a fence) and fall when the
//! block goes; bouncing projectiles bounce and come to rest; piercing ones hit each body once,
//! in order; `ignore`, layers and sensors filter the hits; a free flight equals `predict`;
//! unloaded chunks freeze them; the results are bit-identical far from the origin and on several
//! threads.
mod common;
mod sim;

use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    BodySettings, Collider, CollisionLayers, Contacts, OnBlock, OnEntity, PhysicsStats, Projectile,
    ProjectileHit, ProjectileTarget, RigidBody, StepParams, Teleport, Velocity, predict,
};
use boxel_voxel::{BlockPos, BlockState, Face, MAX_BLOCK_COORD, WorldPos};
use common::STONE;
use sim::{Hits, STEP, Sim};

/// Half the edge of an arrow's box.
const ARROW_HALF: f64 = 0.05;

/// A projectile at `from` with velocity `v` and these settings and modes, plus `extra`, with an
/// arrow's box.
fn shoot(
    sim: &mut Sim,
    from: WorldPos,
    v: DVec3,
    projectile: Projectile,
    settings: BodySettings,
    extra: impl Bundle,
) -> Entity {
    shoot_with(
        sim,
        Collider::aabb(DVec3::splat(ARROW_HALF)),
        from,
        v,
        projectile,
        settings,
        extra,
    )
}

/// [`shoot`] with another collider.
fn shoot_with(
    sim: &mut Sim,
    collider: Collider,
    from: WorldPos,
    v: DVec3,
    projectile: Projectile,
    settings: BodySettings,
    extra: impl Bundle,
) -> Entity {
    sim.app
        .world_mut()
        .spawn((
            RigidBody::Dynamic,
            projectile,
            collider,
            settings,
            Teleport(from),
            Velocity(v),
            extra,
        ))
        .id()
}

/// An arrow without gravity (straight lines make exact checks easy).
fn straight() -> BodySettings {
    BodySettings {
        gravity_scale: 0.0,
        drag_xz: 0.0,
        drag_y: 0.0,
        ..BodySettings::arrow()
    }
}

fn take_hits(sim: &mut Sim) -> Vec<ProjectileHit> {
    std::mem::take(&mut sim.app.world_mut().resource_mut::<Hits>().0)
}

/// `sim.pos` as an `f64` vector relative to the base (exact for these small scenes).
fn local(sim: &Sim, p: WorldPos) -> DVec3 {
    WorldPos::from_block(BlockPos::from_i64vec3(sim.base), DVec3::ZERO).delta_to(&p)
}

#[test]
fn an_arrow_at_60_never_misses_a_mob_0_6_wide() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let mob = sim.spawn(sim.pos(20.5, 4.0, 0.5), ());
    // 64 arrows, one per 1/64 of a step's travel (60 / 64 = 0.94 blocks, more than the mob is
    // wide): every phase of the steps against the mob.
    let arrows: Vec<Entity> = (0..64)
        .map(|k| {
            let from = sim.pos(0.5 + f64::from(k) * 60.0 / 64.0 / 64.0, 5.0, 0.5);
            shoot(
                &mut sim,
                from,
                DVec3::new(60.0, 0.0, 0.0),
                Projectile::arrow(),
                straight(),
                (),
            )
        })
        .collect();
    sim.steps(40);
    let hits = take_hits(&mut sim);
    for &arrow in &arrows {
        let mine: Vec<_> = hits.iter().filter(|h| h.projectile == arrow).collect();
        assert_eq!(mine.len(), 1, "{mine:?}");
        assert_eq!(mine[0].target, ProjectileTarget::Entity(mob));
        assert_eq!(mine[0].speed, 60.0);
        // Stopped touching the mob: its center half an arrow before the mob's face.
        let at = local(&sim, sim.position(arrow));
        assert!((at.x - (20.2 - ARROW_HALF)).abs() < 1e-9, "{at:?}");
        assert!((local(&sim, mine[0].point).x - (20.2 - ARROW_HALF)).abs() < 1e-9);
        assert_eq!(sim.get::<Velocity>(arrow).0, DVec3::ZERO);
    }
    // The mob was not pushed (the game applies a knockback).
    assert_eq!(sim.position(mob), sim.pos(20.5, 4.0, 0.5));
    // With gravity (aimed a little up) too.
    for k in 0..16 {
        let from = sim.pos(0.5 + f64::from(k) * 0.06, 5.0, 0.5);
        shoot(
            &mut sim,
            from,
            DVec3::new(60.0, 3.0, 0.0),
            Projectile::arrow(),
            BodySettings::arrow(),
            (),
        );
    }
    sim.steps(40);
    let hits = take_hits(&mut sim);
    let on_mob = hits
        .iter()
        .filter(|h| h.target == ProjectileTarget::Entity(mob))
        .count();
    assert_eq!(on_mob, 16, "{hits:?}");
}

#[test]
fn an_arrow_sticks_on_the_face_and_falls_when_the_block_goes() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    sim.edit(sim.block(10, 5, 0), STONE);
    sim.idle_frame();
    let from = sim.pos(0.5, 5.5, 0.5);
    let arrow = shoot(
        &mut sim,
        from,
        DVec3::new(200.0, 0.0, 0.0),
        Projectile::arrow(),
        straight(),
        (),
    );
    sim.steps(8);
    let hits = take_hits(&mut sim);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(
        hits[0].target,
        ProjectileTarget::Block {
            pos: sim.block(10, 5, 0),
            face: Some(Face::NegX),
            state: STONE,
        }
    );
    assert!(sim.asleep(arrow), "stuck");
    assert_eq!(
        sim.get::<Projectile>(arrow).stuck_in(),
        Some(sim.block(10, 5, 0))
    );
    let at = local(&sim, sim.position(arrow));
    assert_eq!(at.x, 10.0 - 1e-7, "on the face, eps outside");
    assert_eq!((at.y, at.z), (5.5, 0.5));
    assert!(sim.get::<Contacts>(arrow).touched(Face::PosX));
    // Gravity from now on (a settings change wakes it), and a block next to it changes: each
    // time it wakes, finds its block still there and sleeps again.
    *sim.app
        .world_mut()
        .get_mut::<BodySettings>(arrow)
        .expect("arrow") = BodySettings::arrow();
    sim.steps(2);
    assert!(sim.asleep(arrow));
    sim.edit(sim.block(9, 4, 0), STONE);
    sim.steps(3);
    assert!(sim.asleep(arrow));
    assert_eq!(local(&sim, sim.position(arrow)).y, 5.5);
    // The block goes: it falls onto the stone below and sticks in its top.
    sim.edit(sim.block(10, 5, 0), BlockState::AIR);
    sim.steps(64);
    assert!(sim.asleep(arrow));
    let at = local(&sim, sim.position(arrow));
    assert_eq!(at.y, 5.0 + 1e-7, "on top of the block at (9, 4, 0)? {at:?}");
    let hits = take_hits(&mut sim);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!(matches!(
        hits[0].target,
        ProjectileTarget::Block {
            face: Some(Face::PosY),
            ..
        }
    ));
}

#[test]
fn a_bouncing_projectile_bounces_and_comes_to_rest() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let from = sim.pos(0.5, 9.0, 0.5);
    let ball = shoot(
        &mut sim,
        from,
        DVec3::new(2.0, -20.0, 0.0),
        Projectile::new(OnBlock::Bounce, OnEntity::Stop),
        BodySettings::ball(),
        (),
    );
    let mut went_up = false;
    for _ in 0..64 {
        sim.steps(1);
        went_up |= sim.get::<Velocity>(ball).0.y > 5.0;
    }
    assert!(went_up, "it bounced");
    let hits = take_hits(&mut sim);
    assert!(!hits.is_empty());
    assert!(matches!(
        hits[0].target,
        ProjectileTarget::Block {
            face: Some(Face::PosY),
            ..
        }
    ));
    // It rests on the ground (center an `eps` above it) and falls asleep.
    for _ in 0..600 {
        if sim.asleep(ball) {
            break;
        }
        sim.steps(1);
    }
    assert!(sim.asleep(ball));
    let at = local(&sim, sim.position(ball));
    assert_eq!(at.y, 4.0 + 1e-7, "{at:?}");
    assert!(sim.get::<Contacts>(ball).on_ground);
    assert_eq!(
        sim.get::<Projectile>(ball).stuck_in(),
        None,
        "resting, not stuck"
    );
}

#[test]
fn a_bouncing_projectile_slides_on_after_it_lands() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // Just above the ground, moving along it: each step its fall is stopped by the ground
    // right away, and the rest of the step goes on along the face.
    let from = sim.pos(0.5, 4.001, 0.5);
    let ball = shoot(
        &mut sim,
        from,
        DVec3::new(4.0, 0.0, 0.0),
        Projectile::new(OnBlock::Bounce, OnEntity::Stop),
        BodySettings::ball(),
        (),
    );
    sim.steps(64);
    let at = local(&sim, sim.position(ball));
    // Ground friction 2 /s from 4 blocks/s: about 1.7 blocks in a second.
    assert!(at.x > 1.5 + 0.5, "slid {}", at.x - 0.5);
    assert_eq!(at.y, 4.0 + 1e-7, "on the ground");
    assert!(sim.get::<Contacts>(ball).on_ground);
    assert!(take_hits(&mut sim).is_empty(), "too slow to report");
}

/// Arrows and balls dropped onto the tops of fence posts (1.5 tall, ADR 0026) and bottom
/// slabs (0.5) around `base`: every final position relative to `base`, after asserting where
/// each one ended.
fn onto_box_tops(base: I64Vec3) -> Vec<(I64Vec3, [u64; 3])> {
    let mut sim = Sim::new(base, STEP);
    let registry = boxel_demo::blocks::registry();
    let fence = registry.parse_state("boxel:oak_fence").expect("fence");
    let slab = registry
        .parse_state("boxel:stone_slab[type=bottom]")
        .expect("slab");
    // The ground is at y = 4: tops at 5.5 (fences) and 4.5 (slabs).
    let blocks = [
        (10, fence, 5.5),
        (12, slab, 4.5),
        (14, fence, 5.5),
        (16, slab, 4.5),
    ];
    for (x, state, _) in blocks {
        sim.edit(sim.block(x, 4, 0), state);
    }
    sim.idle_frame();
    sim.idle_frame();
    // Arrows stop on the first two, balls bounce on the other two.
    let bodies: Vec<Entity> = blocks
        .iter()
        .enumerate()
        .map(|(i, &(x, _, _))| {
            let from = sim.pos(f64::from(x as i32) + 0.5, 9.5, 0.5);
            if i < 2 {
                shoot(
                    &mut sim,
                    from,
                    DVec3::new(0.0, -200.0, 0.0),
                    Projectile::arrow(),
                    straight(),
                    (),
                )
            } else {
                shoot(
                    &mut sim,
                    from,
                    DVec3::new(0.0, -20.0, 0.0),
                    Projectile::new(OnBlock::Bounce, OnEntity::Stop),
                    BodySettings::ball(),
                    (),
                )
            }
        })
        .collect();
    let mut went_up = [false; 2];
    for _ in 0..600 {
        sim.steps(1);
        for (up, &ball) in went_up.iter_mut().zip(&bodies[2..]) {
            *up |= sim.get::<Velocity>(ball).0.y > 2.0;
        }
        if bodies.iter().all(|&b| sim.asleep(b)) {
            break;
        }
    }
    let hits = take_hits(&mut sim);
    for (i, (&body, &(x, state, top))) in bodies.iter().zip(&blocks).enumerate() {
        assert!(sim.asleep(body), "{i}");
        let at = local(&sim, sim.position(body));
        // The center on the top face, `eps` above it.
        assert_eq!(at.y, top + 1e-7, "{i}: {at:?}");
        let first = hits
            .iter()
            .find(|h| h.projectile == body)
            .unwrap_or_else(|| panic!("{i}: no hit"));
        assert_eq!(
            first.target,
            ProjectileTarget::Block {
                pos: sim.block(x, 4, 0),
                face: Some(Face::PosY),
                state,
            }
        );
        assert_eq!(local(&sim, first.point).y, top + 1e-7, "{i}");
        if i < 2 {
            assert_eq!(
                sim.get::<Projectile>(body).stuck_in(),
                Some(sim.block(x, 4, 0))
            );
        } else {
            // It bounced off the top, came to rest on it and is not stuck inside the block.
            assert!(went_up[i - 2], "{i}: it bounced");
            assert_eq!(sim.get::<Projectile>(body).stuck_in(), None, "{i}");
            assert!(sim.get::<Contacts>(body).on_ground);
        }
    }
    bodies
        .iter()
        .map(|&b| {
            let p = sim.position(b);
            (
                p.block.as_i64vec3() - base,
                p.frac.to_array().map(f64::to_bits),
            )
        })
        .collect()
}

#[test]
fn projectiles_stop_on_the_tops_of_slabs_and_fences() {
    let reference = onto_box_tops(I64Vec3::ZERO);
    let far = MAX_BLOCK_COORD - 1024;
    for base in [
        I64Vec3::new(1 << 52, -(1 << 40), 1 << 31),
        I64Vec3::new(far, far, far),
    ] {
        assert_eq!(onto_box_tops(base), reference, "base {base:?}");
    }
}

#[test]
fn a_stuck_arrow_flies_again_when_its_velocity_is_set() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    sim.edit(sim.block(10, 5, 0), STONE);
    sim.idle_frame();
    let from = sim.pos(0.5, 5.5, 0.5);
    let arrow = shoot(
        &mut sim,
        from,
        DVec3::new(200.0, 0.0, 0.0),
        Projectile::arrow(),
        straight(),
        (),
    );
    sim.steps(8);
    assert!(sim.asleep(arrow));
    assert_eq!(
        sim.get::<Projectile>(arrow).stuck_in(),
        Some(sim.block(10, 5, 0))
    );
    // Pulled out by the game: it wakes and flies back.
    sim.app
        .world_mut()
        .get_mut::<Velocity>(arrow)
        .expect("arrow")
        .0 = DVec3::new(-16.0, 0.0, 0.0);
    sim.steps(8);
    assert!(!sim.asleep(arrow));
    assert_eq!(sim.get::<Projectile>(arrow).stuck_in(), None);
    assert_eq!(sim.get::<Velocity>(arrow).0, DVec3::new(-16.0, 0.0, 0.0));
    assert!((local(&sim, sim.position(arrow)).x - (10.0 - 1e-7 - 2.0)).abs() < 1e-9);
}

#[test]
fn a_piercing_projectile_hits_each_body_once_in_order() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let mobs: Vec<Entity> = [8.5, 4.5, 12.5]
        .into_iter()
        .map(|x| sim.spawn(sim.pos(x, 4.0, 0.5), ()))
        .collect();
    sim.edit(sim.block(16, 5, 0), STONE);
    sim.idle_frame();
    let from = sim.pos(0.5, 5.0, 0.5);
    let arrow = shoot(
        &mut sim,
        from,
        DVec3::new(30.0, 0.0, 0.0),
        Projectile::new(OnBlock::Stop, OnEntity::Pass),
        straight(),
        (),
    );
    sim.steps(48);
    let targets: Vec<_> = take_hits(&mut sim).iter().map(|h| h.target).collect();
    assert_eq!(
        targets,
        [
            ProjectileTarget::Entity(mobs[1]),
            ProjectileTarget::Entity(mobs[0]),
            ProjectileTarget::Entity(mobs[2]),
            ProjectileTarget::Block {
                pos: sim.block(16, 5, 0),
                face: Some(Face::NegX),
                state: STONE,
            },
        ]
    );
    assert!(sim.asleep(arrow));
}

#[test]
fn ignore_layers_and_sensors_filter_the_hits() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let shooter = sim.spawn(sim.pos(2.5, 4.0, 0.5), ());
    let ghost = sim.body(
        sim.pos(5.5, 4.0, 0.5),
        Collider::standing(0.6, 1.8).with_layers(CollisionLayers::new(2, u32::MAX)),
    );
    let item = sim.body(
        sim.pos(7.5, 4.9, 0.5),
        (
            Collider::aabb(DVec3::splat(0.2)).sensor(),
            BodySettings::item(),
        ),
    );
    let target = sim.spawn(sim.pos(10.5, 4.0, 0.5), ());
    let from = sim.pos(0.5, 5.0, 0.5);
    // Through the shooter (ignored), the ghost (not in the arrow's mask) and the item (a
    // sensor), into the target.
    let arrow = shoot_with(
        &mut sim,
        Collider::aabb(DVec3::splat(ARROW_HALF)).with_layers(CollisionLayers::new(1, 1)),
        from,
        DVec3::new(40.0, 0.0, 0.0),
        Projectile::arrow().ignoring(shooter),
        straight(),
        (),
    );
    sim.steps(24);
    let hits = take_hits(&mut sim);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].target, ProjectileTarget::Entity(target));
    // Without `ignore` the shooter is hit.
    let behind = sim.pos(-1.5, 5.0, 0.5);
    shoot(
        &mut sim,
        behind,
        DVec3::new(40.0, 0.0, 0.0),
        Projectile::arrow(),
        straight(),
        (),
    );
    sim.steps(8);
    let hits = take_hits(&mut sim);
    assert_eq!(hits[0].target, ProjectileTarget::Entity(shooter));
    let _ = (ghost, item, arrow);
}

#[test]
fn a_point_blank_shot_hits_the_body_it_starts_in_once() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let mob = sim.spawn(sim.pos(2.5, 4.0, 0.5), ());
    sim.steps(2);
    // Spawned inside the mob (the sandbox's arrow at the eye plus half a block, into a mob
    // pressed against the player): hit at once, where it starts.
    let inside = sim.pos(2.5, 5.0, 0.5);
    let arrow = shoot(
        &mut sim,
        inside,
        DVec3::new(0.0, 0.0, 40.0),
        Projectile::arrow(),
        BodySettings::arrow(),
        (),
    );
    sim.steps(1);
    let hits = take_hits(&mut sim);
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert_eq!(hits[0].target, ProjectileTarget::Entity(mob));
    assert_eq!(hits[0].point, inside);
    assert_eq!(sim.position(arrow), inside);
    assert_eq!(sim.get::<Velocity>(arrow).0, DVec3::ZERO);
    // Stopped inside it, it is not hit again: it falls out of the mob onto the ground.
    sim.steps(64);
    assert!(
        take_hits(&mut sim)
            .iter()
            .all(|h| h.target != ProjectileTarget::Entity(mob))
    );
    assert!(local(&sim, sim.position(arrow)).y < 4.1);
    // A piercing one spawned inside reports it once too, and flies on.
    let pierce = shoot(
        &mut sim,
        inside,
        DVec3::new(0.0, 0.0, 40.0),
        Projectile::new(OnBlock::Stop, OnEntity::Pass),
        straight(),
        (),
    );
    sim.steps(4);
    let hits = take_hits(&mut sim);
    assert_eq!(
        hits.iter()
            .filter(|h| h.target == ProjectileTarget::Entity(mob))
            .count(),
        1,
        "{hits:?}"
    );
    assert!(local(&sim, sim.position(pierce)).z > 2.0);
}

#[test]
fn a_free_flight_matches_predict_bit_for_bit() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let from = sim.pos(0.25, 6.0, 0.75);
    let v = DVec3::new(37.0, 21.0, -13.0);
    let arrow = shoot(
        &mut sim,
        from,
        v,
        Projectile::arrow(),
        BodySettings::arrow(),
        (),
    );
    sim.steps(1);
    // The first step started at `from` (the teleport) and moved once.
    let params = StepParams::default().without_speed_limit();
    let one = predict(from, v, &BodySettings::arrow(), &params, 1);
    assert_eq!((sim.position(arrow), sim.get::<Velocity>(arrow).0), one);
    sim.steps(19);
    let twenty = predict(from, v, &BodySettings::arrow(), &params, 20);
    assert_eq!((sim.position(arrow), sim.get::<Velocity>(arrow).0), twenty);
    assert!(take_hits(&mut sim).is_empty());
    // Faster than the bodies' speed limit (100 blocks/s): projectiles have none.
    let fast = shoot(
        &mut sim,
        from,
        DVec3::new(0.0, 300.0, 0.0),
        Projectile::arrow(),
        straight(),
        (),
    );
    sim.steps(1);
    assert_eq!(sim.get::<Velocity>(fast).0, DVec3::new(0.0, 300.0, 0.0));
}

#[test]
fn a_projectile_into_unloaded_chunks_freezes() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // Where the loaded region ends along +X.
    let edge = (0..512)
        .find(|&x| {
            sim.app
                .world()
                .resource::<boxel_world::ChunkMap>()
                .block_at(sim.block(x, 5, 0))
                .is_none()
        })
        .expect("an edge") as f64;
    let from = sim.pos(edge - 3.5, 5.0, 0.5);
    let arrow = shoot(
        &mut sim,
        from,
        DVec3::new(40.0, 0.0, 0.0),
        Projectile::arrow(),
        straight(),
        (),
    );
    sim.steps(8);
    assert!(sim.frozen(arrow));
    let at = local(&sim, sim.position(arrow));
    assert!(at.x < edge && at.x > edge - 2.0, "{at:?} edge {edge}");
    assert_eq!(sim.get::<Velocity>(arrow).0, DVec3::new(40.0, 0.0, 0.0));
    assert!(sim.app.world().resource::<PhysicsStats>().frozen >= 1);
}

#[test]
fn hitting_a_sleeping_mob_reports_it_and_a_knockback_wakes_it() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let mob = sim.spawn(sim.pos(8.5, 4.0, 0.5), ());
    sim.steps(64);
    assert!(sim.asleep(mob));
    let from = sim.pos(0.5, 5.0, 0.5);
    shoot(
        &mut sim,
        from,
        DVec3::new(60.0, 0.0, 0.0),
        Projectile::arrow(),
        straight(),
        (),
    );
    sim.steps(16);
    let hits = take_hits(&mut sim);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].target, ProjectileTarget::Entity(mob));
    assert!(sim.asleep(mob), "a hit alone does not wake it");
    // The game's knockback: along the arrow's velocity.
    let push = hits[0].velocity.normalize() * 6.0 + DVec3::Y * 4.0;
    sim.app.world_mut().get_mut::<Velocity>(mob).expect("mob").0 = push;
    sim.steps(32);
    assert!(local(&sim, sim.position(mob)).x > 9.0);
}

/// A volley of arrows in every direction around a few mobs and walls: every hit and final
/// state, relative to `base`.
fn volley(base: I64Vec3, parallel: bool) -> (Vec<(I64Vec3, [u64; 3])>, Vec<(usize, i64)>) {
    let mut sim = Sim::new(base, STEP);
    sim.settings().parallel_threshold = if parallel { 0 } else { usize::MAX };
    for x in -6..=6 {
        sim.edit(sim.block(x, 4, 9), STONE);
        sim.edit(sim.block(x, 5, 9), STONE);
    }
    sim.idle_frame();
    sim.idle_frame();
    let mobs: Vec<Entity> = (0..5)
        .map(|i| sim.spawn(sim.pos(-6.0 + 3.0 * f64::from(i), 4.0, 4.5), ()))
        .collect();
    let mut arrows = Vec::new();
    for i in 0..300u32 {
        let a = f64::from(i) * 0.37;
        // Directions without libm: a dyadic fan.
        let dir = DVec3::new(
            (a - (a / 2.0).floor() * 2.0) - 1.0,
            0.15 + f64::from(i % 7) * 0.05,
            0.3 + f64::from(i % 5) * 0.25,
        );
        let mode = if i % 3 == 0 {
            Projectile::new(OnBlock::Bounce, OnEntity::Pass)
        } else {
            Projectile::arrow()
        };
        let from = sim.pos(f64::from(i % 11) - 5.0, 5.0 + f64::from(i % 3) * 0.5, -2.0);
        let settings = if i % 3 == 0 {
            BodySettings::ball()
        } else {
            BodySettings::arrow()
        };
        arrows.push(shoot(&mut sim, from, dir * 45.0, mode, settings, ()));
    }
    sim.steps(80);
    let index_of =
        |e: Entity| -> i64 { mobs.iter().position(|&m| m == e).map_or(-1, |i| i as i64) };
    let hits = take_hits(&mut sim)
        .iter()
        .map(|h| {
            let arrow = arrows
                .iter()
                .position(|&a| a == h.projectile)
                .expect("arrow");
            let target = match h.target {
                ProjectileTarget::Entity(e) => index_of(e),
                ProjectileTarget::Block { pos, .. } => {
                    1000 + (pos.x - base.x) * 100 + (pos.y - base.y) * 10 + (pos.z - base.z)
                }
            };
            (arrow, target)
        })
        .collect();
    let states = arrows
        .iter()
        .map(|&a| {
            let p = sim.position(a);
            (
                p.block.as_i64vec3() - base,
                p.frac.to_array().map(f64::to_bits),
            )
        })
        .collect();
    (states, hits)
}

#[test]
fn volleys_are_bit_identical_far_away_and_on_several_threads() {
    let reference = volley(I64Vec3::ZERO, false);
    assert!(reference.1.len() > 200, "{}", reference.1.len());
    assert!(
        reference.1.iter().any(|&(_, t)| (0..5).contains(&t)),
        "mobs hit"
    );
    assert_eq!(volley(I64Vec3::ZERO, true), reference, "parallel");
    let far = MAX_BLOCK_COORD - 1024;
    for base in [
        I64Vec3::new(1 << 31, 0, 0),
        I64Vec3::new(1 << 52, -(1 << 40), 1 << 31),
        I64Vec3::new(far, far, far),
        I64Vec3::new(-far, -far, -far),
    ] {
        assert_eq!(volley(base, false), reference, "base {base:?}");
    }
}
