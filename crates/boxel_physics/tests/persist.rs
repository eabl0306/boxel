//! Bodies saved with the world (ADR 0024 section 3, `BodyCodec`): bodies of `Saved` entities are
//! put away when their chunk unloads and come back once, with their whole state, when it loads
//! (near the origin, across ±2^31, at 2^40 and next to the world limit); sleeping bodies come
//! back asleep and stay so; after a restart the bodies step bit for bit as they would have
//! without it (also a swimmer holding up, a frozen walker, a crouched walker that stands up
//! after the restart and a body with a pending teleport);
//! a body saved by another codec version, or damaged, stays saved as it is.
mod sim;

use boxel_app::App;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BODY_CODEC, BodyCodec, BodyPosition, BodySettings, CharacterController, Collider, Contacts,
    Crouch, Frozen, Interpolated, MoveIntent, OnBlock, Projectile, ReportContacts, RigidBody,
    Sleeping, SwimIntent, SwimPlugin, SwimSettings, Swimmer, Velocity,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, ChunkKey, ChunkPos, MAX_BLOCK_COORD, WorldPos};
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
        &boxel_demo::blocks::registry(),
        "shifted flat",
    )
    .expect("the world opens");
    let mut sim = Sim::with_plugins(
        base,
        Duration::ZERO,
        (
            SwimPlugin,
            PersistPlugin::new(store).with_executor(Arc::new(InlineExecutor)),
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
    crouch: Option<Crouch>,
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
                crouch: r.get::<Crouch>().copied(),
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
                facing: Vec2::new(0.3, -0.7),
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
        assert!(
            before
                .iter()
                .any(|(_, s)| s.controller.is_some_and(|c| c.sprinting)),
            "{base:?}: the walker sprints"
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
    sim.body(
        sim.pos(30.5, 4.6, 3.5),
        (
            Saved::new(),
            Collider::sphere(0.25),
            BodySettings::item(),
            Velocity(DVec3::new(12.0, 2.0, 0.0)),
        ),
    );
    sim.steps(1);
    // Saved where it is by the exit (which closes the store), then it flies on in a new app.
    sim.app.shutdown();
    drop(sim);
    let keys = backend.entity_keys().unwrap();
    assert_eq!(keys.len(), 1);
    let mut sim = start(&backend, base);
    let ball = {
        let mut q = sim
            .app
            .world_mut()
            .query_filtered::<Entity, (With<Saved>, With<Collider>)>();
        q.single(sim.app.world()).expect("back after the restart")
    };
    let first = ChunkPos::containing(sim.position(ball).block);
    assert_eq!(keys[0].0, ChunkKey::new(first), "saved where it was");
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

/// A character sneaking forward for 30 steps (crouched: its standing collider is kept in
/// `Crouch`), then (with `restart`) the world exits and opens again, then it lets go of Shift
/// and 30 more steps run: it stands up again. The states at the end, and whether it was
/// crouched before the restart.
fn run_crouched(base: I64Vec3, restart: bool) -> (Vec<(SavedId, State)>, bool) {
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    let walker = sim.body(
        sim.pos(6.5, 4.0, 6.5),
        (
            Saved::new(),
            Collider::standing(0.6, 1.8),
            CharacterController::default(),
            MoveIntent {
                wish: Vec2::new(0.0, 0.8),
                sneak: true,
                facing: Vec2::new(0.0, 1.0),
                ..MoveIntent::default()
            },
        ),
    );
    sim.steps(30);
    let crouched = sim.get::<Crouch>(walker).is_crouching();
    if restart {
        sim.app.shutdown();
        drop(sim);
        sim = start(&backend, base);
    }
    let world = sim.app.world_mut();
    let mut q = world.query_filtered::<&mut MoveIntent, With<Saved>>();
    for mut intent in q.iter_mut(world) {
        intent.sneak = false;
    }
    sim.steps(30);
    (states(&mut sim.app), crouched)
}

/// A crouched character comes back crouched with the collider it stands up to: after the
/// restart it stands up exactly as if it had never been saved.
#[test]
fn a_crouched_walker_stands_up_the_same_after_a_restart() {
    for base in [bases()[0], bases()[2], bases()[3]] {
        let (straight, crouched) = run_crouched(base, false);
        assert!(crouched, "{base:?}: crouched while sneaking");
        let stood = &straight[0].1;
        assert!(
            !stood.crouch.unwrap().is_crouching(),
            "{base:?}: stands up without Shift"
        );
        assert_eq!(
            stood.collider,
            Some(Collider::standing(0.6, 1.8)),
            "{base:?}: standing again"
        );
        assert_eq!(run_crouched(base, true).0, straight, "{base:?}");
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

/// Between a body's restore and its first step (frames with no fixed step), what happens around
/// it is not what happened before it came back: the crate a restored crate sleeps on despawned
/// then wakes it, and a `BodyPosition` written then is a nudge (it wakes and falls, and is shown
/// where it is).
#[test]
fn what_happens_after_a_restore_wakes_the_body() {
    let base = bases()[2];
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    let crate_ = Collider::aabb(DVec3::splat(0.375));
    for y in [4.375, 5.125] {
        sim.body(sim.pos(3.5, y, 3.5), (Saved::new(), crate_));
    }
    sim.body(sim.pos(9.5, 4.375, 3.5), (Saved::new(), crate_));
    sim.steps(120);
    move_viewer(&mut sim, DVec3::new(8192.0, 0.0, 0.0));
    move_viewer(&mut sim, DVec3::ZERO);
    // Back, asleep, and no step ran since.
    let world = sim.app.world_mut();
    let mut q = world.query_filtered::<(Entity, &BodyPosition), (With<Saved>, With<Sleeping>)>();
    let mut bodies: Vec<(Entity, WorldPos)> = q.iter(world).map(|(e, p)| (e, p.0)).collect();
    assert_eq!(bodies.len(), 3, "all asleep");
    // By x, then height: the pile's bottom and top, then the one alone.
    bodies.sort_unstable_by_key(|(_, p)| (p.block.x, p.block.y, p.frac.y.to_bits()));
    let (bottom, top, alone) = (bodies[0].0, bodies[1].0, bodies[2].0);
    let top_y = sim.position(top).to_dvec3().y;
    sim.idle_frame();
    sim.app.world_mut().despawn(bottom);
    let lifted = sim.pos(9.5, 7.375, 3.5);
    sim.app
        .world_mut()
        .get_mut::<BodyPosition>(alone)
        .unwrap()
        .0 = lifted;
    sim.idle_frame();
    sim.steps(1);
    assert!(!sim.asleep(top), "the crate under it went");
    assert!(!sim.asleep(alone), "nudged");
    sim.steps(40);
    assert!(sim.position(top).to_dvec3().y < top_y - 0.5, "it fell");
    let y = sim.position(alone).to_dvec3().y;
    assert!(y < lifted.to_dvec3().y - 2.0, "it fell: {y}");
    assert_eq!(sim.translation(alone).y, y, "shown where it is");
}

/// A chunk that loads after a body came back from a save (before its first step) wakes it, as it
/// wakes any sleeper it touches: the blocks there may not be what the body slept on.
#[test]
fn a_chunk_loaded_after_a_restore_wakes_the_body() {
    let base = bases()[0];
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    // At the edge of the loaded data (two chunks around the viewer): one block from chunk 3.
    let edge = sim.body(
        sim.pos(95.5, 4.375, 0.5),
        (Saved::new(), Collider::aabb(DVec3::splat(0.375))),
    );
    sim.steps(120);
    assert!(sim.asleep(edge), "asleep at the edge");
    move_viewer(&mut sim, DVec3::new(8192.0, 0.0, 0.0));
    move_viewer(&mut sim, DVec3::ZERO);
    let world = sim.app.world_mut();
    let mut q = world.query_filtered::<Entity, (With<Saved>, With<Sleeping>)>();
    let edge = q.single(world).expect("back asleep");
    // A second viewer loads chunk 3 before any step (frames of some length: the messages
    // wait for the step).
    sim.viewer(DVec3::new(160.0, 0.0, 0.0));
    let three = ChunkPos::containing(sim.block(96, 4, 0));
    for _ in 0..100 {
        let map = sim.app.world().resource::<boxel_world::ChunkMap>();
        if map
            .id_at(three)
            .and_then(|id| map.get(id))
            .is_some_and(|slot| matches!(slot.data(), boxel_world::DataState::Ready(_)))
        {
            break;
        }
        sim.idle_frame();
    }
    sim.idle_frame();
    sim.steps(1);
    assert!(!sim.asleep(edge), "the load after it came back woke it");
}

/// The chunk loads a sleeping pile came back with do not wake it, also when the frames between
/// its chunk's load and the next step run no step (the messages wait for that step).
#[test]
fn the_loads_a_pile_came_back_with_do_not_wake_it() {
    let base = bases()[2];
    let backend = Arc::new(MemoryStore::new());
    let mut sim = start(&backend, base);
    let crate_ = Collider::aabb(DVec3::splat(0.375));
    for y in [4.375, 5.125, 5.875] {
        sim.body(sim.pos(3.5, y, 3.5), (Saved::new(), crate_));
    }
    sim.steps(120);
    let before = states(&mut sim.app);
    move_viewer(&mut sim, DVec3::new(8192.0, 0.0, 0.0));
    // Back with frames of some length that run no step.
    let mut q = sim
        .app
        .world_mut()
        .query_filtered::<&mut Transform, With<ChunkViewer>>();
    for mut t in q.iter_mut(sim.app.world_mut()) {
        t.translation = sim.base.as_dvec3();
    }
    for _ in 0..100 {
        if states(&mut sim.app).len() == before.len() {
            break;
        }
        sim.idle_frame();
    }
    sim.idle_frame();
    assert_eq!(states(&mut sim.app), before, "back");
    sim.steps(1);
    assert_eq!(
        sim.app
            .world()
            .resource::<boxel_physics::PhysicsStats>()
            .woken,
        0
    );
    sim.steps(20);
    assert_eq!(states(&mut sim.app), before, "asleep where it was");
}
