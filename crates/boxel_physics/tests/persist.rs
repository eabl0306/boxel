//! Bodies saved with the world (ADR 0024 section 3, `BodyCodec`): bodies of `Saved` entities are
//! put away when their chunk unloads and come back once, with their whole state, when it loads
//! (near the origin, across ±2^31, at 2^40 and next to the world limit); sleeping bodies come
//! back asleep and stay so; after a restart the bodies step bit for bit as they would have
//! without it (also a swimmer holding up, a frozen walker and a body with a pending teleport);
//! a body saved by another codec version, or damaged, stays saved as it is.
mod sim;

use boxel_app::App;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BODY_CODEC, BodyCodec, BodyPosition, BodySettings, CharacterController, Collider, Contacts,
    Frozen, Interpolated, MoveIntent, OnBlock, Projectile, ReportContacts, RigidBody, Sleeping,
    SwimIntent, SwimPlugin, SwimSettings, Swimmer, Velocity,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, ChunkKey, ChunkPos, MAX_BLOCK_COORD, WorldPos};
use boxel_world::ChunkViewer;
use boxel_world::jobs::InlineExecutor;
use boxel_world::persist::format::{EntityPart, encode_entity};
use boxel_world::persist::{
    ChunkStore, EntityCodec, MemoryStore, PersistPlugin, PersistSettings, SaveBatch, Saved,
    SavedId, WorldStore,
};
use sim::Sim;
use std::sync::Arc;
use std::time::Duration;

/// The bases: the origin, across ±2^31, 2^40 and next to the world limit (multiples of 1024).
fn bases() -> [I64Vec3; 4] {
    let edge = 1 << 31;
    let limit = (MAX_BLOCK_COORD - (1 << 14)) & !1023;
    [
        I64Vec3::ZERO,
        I64Vec3::new(edge, -edge, -edge),
        I64Vec3::new(1 << 40, 1 << 40, -(1 << 40)),
        I64Vec3::new(limit, -limit, limit),
    ]
}

/// A settled sim at `base` saving to `backend`; no fixed step runs until the test steps it
/// (frames of zero length), so the steps after a restart are counted exactly.
fn start(backend: &Arc<MemoryStore>, base: I64Vec3) -> Sim {
    let store = WorldStore::open(
        Arc::clone(backend) as Arc<dyn ChunkStore>,
        &BlockRegistry::default_blocks(),
        "shifted flat",
    )
    .expect("the world opens");
    let mut sim = Sim::with_plugins(
        base,
        Duration::ZERO,
        (
            SwimPlugin,
            PersistPlugin {
                store,
                executor: Arc::new(InlineExecutor),
            },
        ),
    );
    sim.app.insert_resource(PersistSettings {
        autosave: None,
        ..PersistSettings::default()
    });
    calm(&mut sim);
    sim
}

/// Updates without fixed steps until the world settles and every save is written.
fn calm(sim: &mut Sim) {
    sim.settle();
    for _ in 0..3 {
        sim.frame(Duration::ZERO);
    }
    let stats = sim.app.world().resource::<WorldStore>().stats();
    assert_eq!((stats.pending, stats.pending_entities), (0, 0));
}

/// Moves the viewer to `base + local` and calms the world down.
fn move_viewer(sim: &mut Sim, local: DVec3) {
    let to = sim.base.as_dvec3() + local;
    let mut q = sim
        .app
        .world_mut()
        .query_filtered::<&mut Transform, With<ChunkViewer>>();
    for mut t in q.iter_mut(sim.app.world_mut()) {
        t.translation = to;
    }
    calm(sim);
}

/// Everything saved of a body, comparable bit for bit (entities as saved ids).
#[derive(Clone, Debug, PartialEq)]
struct State {
    kind: RigidBody,
    block: BlockPos,
    frac: [u64; 3],
    velocity: [u64; 3],
    settings: BodySettings,
    collider: Option<Collider>,
    contacts: Contacts,
    sleeping: bool,
    previous: Option<Option<WorldPos>>,
    report: bool,
    controller: Option<CharacterController>,
    intent: Option<MoveIntent>,
    projectile: Option<(OnBlock, Option<BlockPos>, Option<SavedId>)>,
    swimmer: Option<Swimmer>,
    swim_intent: Option<SwimIntent>,
    frozen: bool,
}

fn bits(v: DVec3) -> [u64; 3] {
    [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
}

/// Every saved body by saved id.
fn states(app: &mut App) -> Vec<(SavedId, State)> {
    let world = app.world_mut();
    let mut q = world.query_filtered::<Entity, (With<Saved>, With<RigidBody>)>();
    let entities: Vec<Entity> = q.iter(world).collect();
    let world = app.world();
    let mut out: Vec<(SavedId, State)> = entities
        .into_iter()
        .map(|e| {
            let r = world.entity(e);
            let id = r.get::<Saved>().unwrap().id().expect("an id");
            let p = r.get::<BodyPosition>().unwrap().0.normalize();
            let state = State {
                kind: *r.get::<RigidBody>().unwrap(),
                block: p.block,
                frac: bits(p.frac),
                velocity: bits(r.get::<Velocity>().unwrap().0),
                settings: *r.get::<BodySettings>().unwrap(),
                collider: r.get::<Collider>().copied(),
                contacts: *r.get::<Contacts>().unwrap(),
                sleeping: r.contains::<Sleeping>(),
                previous: r.get::<Interpolated>().map(Interpolated::previous),
                report: r.contains::<ReportContacts>(),
                controller: r.get::<CharacterController>().copied(),
                intent: r.get::<MoveIntent>().copied(),
                projectile: r.get::<Projectile>().map(|p| {
                    let shooter = p
                        .ignore
                        .and_then(|s| world.get::<Saved>(s))
                        .and_then(Saved::id);
                    (p.on_block, p.stuck_in(), shooter)
                }),
                swimmer: r.get::<Swimmer>().copied(),
                swim_intent: r.get::<SwimIntent>().copied(),
                frozen: r.contains::<Frozen>(),
            };
            (id, state)
        })
        .collect();
    out.sort_unstable_by_key(|(id, _)| *id);
    out
}

/// A scene of bodies near `base`, apart from each other (no pushes between them): a crate
/// that comes to rest, a walking character with smooth rendering and contact reports, a sliding
/// ball crossing a chunk border, a kinematic platform, a static wall, a swimmer and an arrow shot
/// by a saved shooter (it sticks in the ground).
fn spawn_scene(sim: &mut Sim) {
    let crate_ = Collider::aabb(DVec3::splat(0.375));
    sim.body(sim.pos(1.5, 6.0, 1.5), (Saved::new(), crate_));
    sim.body(
        sim.pos(6.5, 4.0, 6.5),
        (
            Saved::new(),
            Collider::standing(0.6, 1.8),
            CharacterController::default(),
            MoveIntent {
                wish: Vec2::new(0.3, -0.7),
                sprint: true,
                ..MoveIntent::default()
            },
            Interpolated::default(),
            ReportContacts,
        ),
    );
    sim.body(
        sim.pos(28.5, 4.5, 12.5),
        (
            Saved::new(),
            Collider::sphere(0.25),
            BodySettings {
                restitution: 0.6,
                ..BodySettings::item()
            },
            Velocity(DVec3::new(9.0, 3.0, -1.0)),
        ),
    );
    let (platform, wall) = (sim.pos(12.5, 8.0, -9.5), sim.pos(-10.5, 5.0, 0.5));
    let shooter = sim
        .app
        .world_mut()
        .spawn((
            Saved::new(),
            RigidBody::Kinematic,
            Collider::aabb(DVec3::new(1.0, 0.25, 1.0)),
            boxel_physics::Teleport(platform),
            Velocity(DVec3::new(0.0, 0.0, 0.5)),
        ))
        .id();
    sim.app.world_mut().spawn((
        Saved::new(),
        RigidBody::Static,
        Collider::aabb(DVec3::new(0.5, 1.0, 3.0)),
        boxel_physics::Teleport(wall),
    ));
    sim.body(
        sim.pos(-4.5, 4.0, 14.5),
        (
            Saved::new(),
            Collider::standing(0.6, 1.8),
            Swimmer::new(SwimSettings::default()),
            SwimIntent { up: true },
        ),
    );
    sim.body(
        sim.pos(12.5, 6.0, -5.5),
        (
            Saved::new(),
            Projectile::arrow().ignoring(shooter),
            Collider::aabb(DVec3::splat(0.05)),
            BodySettings::arrow(),
            Velocity(DVec3::new(0.0, -30.0, 4.0)),
        ),
    );
}

#[test]
fn bodies_come_back_once_as_they_were_near_and_far() {
    for base in bases() {
        let backend = Arc::new(MemoryStore::new());
        let mut sim = start(&backend, base);
        spawn_scene(&mut sim);
        sim.steps(90);
        calm(&mut sim);
        let before = states(&mut sim.app);
        assert_eq!(before.len(), 7, "{base:?}");
        assert!(
            before.iter().filter(|(_, s)| s.sleeping).count() >= 2,
            "{base:?}: the crate and the arrow sleep"
        );
        for round in 0..3 {
            move_viewer(&mut sim, DVec3::new(8192.0, 0.0, 0.0));
            assert!(
                states(&mut sim.app).is_empty(),
                "{base:?} {round}: put away"
            );
            assert_eq!(
                backend.entity_count(),
                7,
                "{base:?} {round}: one record each"
            );
            move_viewer(&mut sim, DVec3::ZERO);
            assert_eq!(
                states(&mut sim.app),
                before,
                "{base:?} {round}: as they were"
            );
        }
        // Asleep they stay: the chunk loads they came back with wake nobody.
        sim.steps(10);
        let after = states(&mut sim.app);
        for ((id, a), (_, b)) in before.iter().zip(&after) {
            if a.sleeping {
                assert_eq!(a, b, "{base:?} {id:?}: still asleep, unmoved");
            }
        }
        let stats = sim.app.world().resource::<WorldStore>().stats();
        assert_eq!(stats.entity_errors, 0, "{base:?}");
    }
}

/// Runs the scene for `first` steps, then (with `restart`) exits and opens the world again, then
/// `second` more steps: the bodies' states at the end.
fn run(base: I64Vec3, first: usize, second: usize, restart: bool) -> Vec<(SavedId, State)> {
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    spawn_scene(&mut sim);
    sim.steps(first);
    if restart {
        sim.app.shutdown();
        drop(sim);
        sim = start(&backend, base);
        assert_eq!(states(&mut sim.app).len(), 7, "{base:?}: all back");
    }
    sim.steps(second);
    states(&mut sim.app)
}

#[test]
fn bodies_step_the_same_after_a_restart() {
    for base in [bases()[0], bases()[2], bases()[3]] {
        // Mid-flight, mid-slide and mid-walk at step 12; the crate falls asleep later.
        let straight = run(base, 12, 60, false);
        let restarted = run(base, 12, 60, true);
        assert_eq!(restarted, straight, "{base:?}");
        // And twice the same.
        assert_eq!(
            run(base, 12, 60, true),
            restarted,
            "{base:?}: deterministic"
        );
    }
}

#[test]
fn a_body_moved_across_chunks_is_saved_under_its_chunk() {
    let base = bases()[2];
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    let ball = sim.body(
        sim.pos(30.5, 4.6, 3.5),
        (
            Saved::new(),
            Collider::sphere(0.25),
            BodySettings::item(),
            Velocity(DVec3::new(12.0, 2.0, 0.0)),
        ),
    );
    sim.steps(1);
    sim.app.world_mut().run_schedule(boxel_app::Shutdown);
    calm(&mut sim);
    let first = ChunkPos::containing(sim.position(ball).block);
    sim.steps(40);
    let now = sim.position(ball).block;
    assert_ne!(ChunkPos::containing(now), first, "it crossed a border");
    move_viewer(&mut sim, DVec3::new(8192.0, 0.0, 0.0));
    let keys = backend.entity_keys().unwrap();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].0, ChunkKey::new(ChunkPos::containing(now)));
}

#[test]
fn bodies_of_another_codec_version_or_damaged_stay_saved() {
    let base = I64Vec3::ZERO;
    let backend = Arc::new(MemoryStore::new());
    // A good body saved, then copied as a newer codec version and as cut bytes.
    let mut sim = start(&backend, base);
    sim.body(
        sim.pos(1.5, 4.0, 1.5),
        (Saved::new(), Collider::aabb(DVec3::splat(0.375))),
    );
    sim.steps(2);
    sim.app.shutdown();
    drop(sim);
    let keys = backend.entity_keys().unwrap();
    let chunk = keys[0].0;
    let good = backend.load_entities(chunk).unwrap()[0].1.clone();
    let body = boxel_world::persist::format::decode_entity(&good).unwrap();
    assert_eq!(body[0].codec, BODY_CODEC);
    let newer = encode_entity(&[EntityPart {
        version: 2,
        ..body[0].clone()
    }]);
    let cut = encode_entity(&[EntityPart {
        bytes: body[0].bytes[..10].to_vec(),
        ..body[0].clone()
    }]);
    backend
        .save_batch(&SaveBatch {
            entities: vec![
                (chunk, 1000, Some(newer.clone())),
                (chunk, 1001, Some(cut.clone())),
            ],
            ..SaveBatch::default()
        })
        .unwrap();
    let mut sim = start(&backend, base);
    assert_eq!(states(&mut sim.app).len(), 1, "only the good one");
    assert_eq!(
        sim.app
            .world()
            .resource::<WorldStore>()
            .stats()
            .entity_errors,
        2
    );
    move_viewer(&mut sim, DVec3::new(8192.0, 0.0, 0.0));
    let stored = backend.load_entities(chunk).unwrap();
    assert_eq!(stored.len(), 3);
    assert_eq!(stored[1], (1000, newer));
    assert_eq!(stored[2], (1001, cut));
}

#[test]
fn a_sleeping_pile_comes_back_asleep() {
    let base = bases()[2];
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    let crate_ = Collider::aabb(DVec3::splat(0.375));
    for y in [4.375, 5.125, 5.875] {
        sim.body(sim.pos(3.5, y, 3.5), (Saved::new(), crate_));
    }
    sim.steps(120);
    let before = states(&mut sim.app);
    assert!(before.iter().all(|(_, s)| s.sleeping), "the pile sleeps");
    for _ in 0..2 {
        move_viewer(&mut sim, DVec3::new(8192.0, 0.0, 0.0));
        move_viewer(&mut sim, DVec3::ZERO);
        sim.steps(20);
        assert_eq!(states(&mut sim.app), before, "asleep where it was");
    }
    assert_eq!(
        sim.app
            .world()
            .resource::<boxel_physics::PhysicsStats>()
            .woken,
        0
    );
}

/// `BodyCodec::changed` (what the autosave asks before making a record again): a sleeping body
/// changes nothing step after step; a step of an awake body, a velocity set or a teleport does.
#[test]
fn a_sleeping_body_does_not_change_for_the_autosave() {
    let base = bases()[2];
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    let crate_ = sim.body(
        sim.pos(3.5, 5.0, 3.5),
        (
            Saved::new(),
            Collider::aabb(DVec3::splat(0.375)),
            Interpolated::default(),
        ),
    );
    let ball = sim.body(
        sim.pos(9.5, 4.25, 3.5),
        (
            Saved::new(),
            Collider::sphere(0.25),
            Velocity(DVec3::new(2.0, 0.0, 0.0)),
        ),
    );
    sim.steps(120);
    assert!(sim.asleep(crate_));
    let changed =
        |sim: &mut Sim, entity: Entity, since| BodyCodec.changed(sim.app.world(), entity, since);
    let since = sim.app.world_mut().change_tick();
    sim.steps(40);
    calm(&mut sim);
    assert!(!changed(&mut sim, crate_, since), "asleep, unchanged");
    sim.app.world_mut().get_mut::<Velocity>(ball).unwrap().0 = DVec3::new(1.0, 2.0, 0.0);
    sim.steps(1);
    assert!(changed(&mut sim, ball, since), "awake and moving");
    let since = sim.app.world_mut().change_tick();
    sim.idle_frame();
    assert!(!changed(&mut sim, crate_, since));
    let to = sim.pos(3.5, 6.0, 3.5);
    sim.app
        .world_mut()
        .entity_mut(crate_)
        .insert(boxel_physics::Teleport(to));
    assert!(changed(&mut sim, crate_, since), "teleported");
}

/// A character walking out of the loaded world (data loads two chunks around the viewer here):
/// it freezes at the edge, holding its velocity and intent. Then (with `restart`) the world is
/// exited and opened again, and `steps` more steps run: the states at the end.
fn run_frozen(base: I64Vec3, restart: bool, steps: usize) -> Vec<(SavedId, State)> {
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    let walker = sim.body(
        sim.pos(93.5, 4.0, 0.5),
        (
            Saved::new(),
            Collider::standing(0.6, 1.8),
            CharacterController::default(),
            MoveIntent {
                wish: Vec2::new(1.0, 0.0),
                ..MoveIntent::default()
            },
        ),
    );
    for _ in 0..200 {
        if sim.frozen(walker) {
            break;
        }
        sim.steps(1);
    }
    assert!(sim.frozen(walker), "{base:?}: frozen at the edge");
    if restart {
        sim.app.shutdown();
        drop(sim);
        sim = start(&backend, base);
    }
    sim.steps(steps);
    states(&mut sim.app)
}

/// A frozen body is saved frozen: in the step it comes back in, its controller does not run
/// (a frozen body's does not), so it goes on exactly as if it had never been saved.
#[test]
fn a_frozen_walker_comes_back_frozen() {
    for base in [bases()[0], bases()[2], bases()[3]] {
        let straight = run_frozen(base, false, 20);
        assert!(straight[0].1.frozen, "{base:?}");
        assert_eq!(run_frozen(base, true, 20), straight, "{base:?}");
    }
}

/// How gameplay placed a body in the last frame before the exit, before any step took it.
#[derive(Clone, Copy, Debug)]
enum Placed {
    /// A `Teleport` of a sleeping crate into the air.
    Teleport,
    /// A `Transform` write of a sleeping crate into the air.
    Transform,
    /// A new body spawned with only a `Transform`, awake.
    NewAwake,
    /// A new body spawned with only a `Transform`, asleep (it stays where it was spawned).
    NewAsleep,
}

/// A crate asleep on the ground, then placed as `how` says; then (with `restart`) the world
/// exits and opens again before any step; then 60 steps: the states at the end.
fn run_placed(base: I64Vec3, how: Placed, restart: bool) -> Vec<(SavedId, State)> {
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    let crate_ = Collider::aabb(DVec3::splat(0.375));
    let resting = sim.body(
        sim.pos(3.5, 5.0, 3.5),
        (Saved::new(), crate_, Interpolated::default()),
    );
    sim.steps(120);
    assert!(sim.asleep(resting), "{base:?}");
    let air = sim.pos(5.5, 9.25, 3.5);
    let shown = sim.base.as_dvec3() + DVec3::new(5.5, 9.25, 3.5);
    let world = sim.app.world_mut();
    match how {
        Placed::Teleport => {
            world
                .entity_mut(resting)
                .insert(boxel_physics::Teleport(air));
        }
        Placed::Transform => world.get_mut::<Transform>(resting).unwrap().translation = shown,
        Placed::NewAwake | Placed::NewAsleep => {
            let mut e = world.spawn((
                Saved::new(),
                RigidBody::Dynamic,
                crate_,
                Transform::from_translation(shown),
            ));
            if matches!(how, Placed::NewAsleep) {
                e.insert(Sleeping);
            }
        }
    }
    if restart {
        sim.app.shutdown();
        drop(sim);
        sim = start(&backend, base);
    }
    sim.steps(60);
    states(&mut sim.app)
}

/// A body placed by gameplay (a pending `Teleport`, a `Transform` write, a new body) and saved
/// before a step took it comes back with that placement still to do: the first step takes it as
/// it would have (a sleeping crate teleported into the air wakes up and falls), bit for bit.
#[test]
fn a_pending_placement_is_taken_after_a_restart() {
    for base in [bases()[0], bases()[2], bases()[3]] {
        for how in [
            Placed::Teleport,
            Placed::Transform,
            Placed::NewAwake,
            Placed::NewAsleep,
        ] {
            let straight = run_placed(base, how, false);
            assert_eq!(run_placed(base, how, true), straight, "{base:?} {how:?}");
        }
    }
}
