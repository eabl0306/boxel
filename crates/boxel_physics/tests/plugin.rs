//! Bodies in a headless app (phase P2): `WorldPlugin` with inline jobs over a flat test world
//! (stone at y 0..=2 and grass at y = 3 relative to a base block, so the ground is at y = 4),
//! `PhysicsPlugin`, and `TimePlugin` on a synthetic clock (frames of a set length, so every run
//! has the same fixed steps).
//!
//! Falling onto the exact surface with one impact (the same speed as the pure integrator),
//! freezing over unloaded chunks, block edits under and into a body, teleports through
//! `Transform` and `Teleport`, interpolation, every message reaching `Update` at 144 fps, the
//! jump latch, walking and jumping onto a step, the stats line, determinism (two runs, 144 vs
//! 64 fps, interpolation on or off) and the same scene far from the origin.
mod common;

use boxel_app::{App, First, FixedTime, FixedUpdate, Time, TimePlugin, TimeSystems, Update};
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3, Vec2};
use boxel_physics::{
    BodyPosition, BodySettings, CharacterController, Collider, Contacts, Frozen, Interpolated,
    MoveIntent, PhysicsPlugin, PhysicsSettings, PhysicsStats, RigidBody, Teleport, TestWorld,
    Velocity, VoxelImpact, integrate_velocity, step_body,
};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, Face, MAX_BLOCK_COORD,
    WorldPos,
};
use boxel_world::jobs::InlineExecutor;
use boxel_world::{ChunkMap, ChunkViewer, EditQueue, WorldPlugin, WorldStats};
use boxel_worldgen::{FlatGenerator, WorldGenerator};
use common::{STONE, player, table};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One fixed step at 64 Hz (exactly `FixedTime::timestep()`).
const STEP: Duration = Duration::from_nanos(15_625_000);
/// A frame at 144 fps: at most one step each.
const FRAME_144: Duration = Duration::from_nanos(6_944_444);

/// Flat layers (stone y 0..=2, grass y = 3) moved by whole chunks to `base`.
struct ShiftedFlat {
    flat: FlatGenerator,
    base: ChunkPos,
}

impl WorldGenerator for ShiftedFlat {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let pos = ChunkPos::new(
            key.pos.x - self.base.x,
            key.pos.y - self.base.y,
            key.pos.z - self.base.z,
        );
        self.flat.generate(ChunkKey { pos, ..key }, registry)
    }
    fn name(&self) -> &str {
        "shifted flat"
    }
}

/// The synthetic clock: every update advances it by `next`.
#[derive(Resource)]
struct Clock {
    time: Time,
    now: Instant,
    next: Duration,
}

/// Replaces the real clock (runs right after `time_system`).
fn synthetic_time(mut clock: ResMut<Clock>, mut time: ResMut<Time>) {
    let now = clock.now + clock.next;
    clock.now = now;
    clock.time.update_with_instant(now);
    *time = clock.time.clone();
}

/// Every `VoxelImpact` read in `Update`.
#[derive(Resource, Default)]
struct Impacts(Vec<VoxelImpact>);

fn log_impacts(mut impacts: MessageReader<VoxelImpact>, mut log: ResMut<Impacts>) {
    log.0.extend(impacts.read().copied());
}

/// A settled flat world around `base` and its app.
struct Sim {
    app: App,
    base: I64Vec3,
}

impl Sim {
    /// `base` is a multiple of 1024 on every axis, so the viewer's `f64` position is exact up
    /// to the world limit. Frames last `frame`. The viewer at `base` loads one chunk around it.
    fn new(base: I64Vec3, frame: Duration) -> Self {
        let start = Instant::now();
        let mut app = App::new();
        app.add_plugins((
            TimePlugin,
            WorldPlugin {
                generator: Arc::new(ShiftedFlat {
                    flat: FlatGenerator::new(vec![
                        ("boxel:stone".to_string(), 3),
                        ("boxel:grass".to_string(), 1),
                    ]),
                    base: ChunkPos::containing(BlockPos::from_i64vec3(base)),
                }),
                executor: Arc::new(InlineExecutor),
                ..WorldPlugin::default()
            },
            PhysicsPlugin::default(),
        ))
        .insert_resource(Clock {
            time: Time::new(start),
            now: start,
            next: frame,
        })
        .init_resource::<Impacts>()
        .add_systems(First, synthetic_time.after(TimeSystems))
        .add_systems(Update, log_impacts);
        app.finish();
        app.cleanup();
        let mut sim = Self { app, base };
        sim.viewer(DVec3::ZERO);
        sim.settle();
        sim
    }

    /// A chunk viewer (radius 1) at `base + local`.
    fn viewer(&mut self, local: DVec3) {
        self.app.world_mut().spawn((
            Transform::from_translation(self.base.as_dvec3() + local),
            ChunkViewer::new(1).with_vertical_radius(1),
        ));
    }

    fn settle(&mut self) {
        let start = Instant::now();
        loop {
            self.app.update();
            let stats = *self.app.world().resource::<WorldStats>();
            if stats.chunks > 0 && stats.is_settled() {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(60),
                "not settled: {stats:?}"
            );
        }
    }

    /// `base + local`, exact.
    fn pos(&self, x: f64, y: f64, z: f64) -> WorldPos {
        WorldPos::from_block(BlockPos::from_i64vec3(self.base), DVec3::new(x, y, z))
    }

    fn block(&self, x: i64, y: i64, z: i64) -> BlockPos {
        BlockPos::from_i64vec3(self.base + I64Vec3::new(x, y, z))
    }

    /// A dynamic body placed exactly at `pos` (through `Teleport`), with `bundle`.
    fn body(&mut self, pos: WorldPos, bundle: impl Bundle) -> Entity {
        self.app
            .world_mut()
            .spawn((RigidBody::Dynamic, Teleport(pos), bundle))
            .id()
    }

    /// A dynamic player-size body placed exactly at `pos`, plus `extra`.
    fn spawn(&mut self, pos: WorldPos, extra: impl Bundle) -> Entity {
        self.body(pos, (Collider::standing(0.6, 1.8), extra))
    }

    /// One update of `frame`.
    fn frame(&mut self, frame: Duration) {
        self.app.world_mut().resource_mut::<Clock>().next = frame;
        self.app.update();
    }

    /// `n` updates of one fixed step each.
    fn steps(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(STEP);
        }
    }

    fn edit(&mut self, pos: BlockPos, state: BlockState) {
        self.app
            .world_mut()
            .resource_mut::<EditQueue>()
            .set_block(pos, state);
    }

    fn get<T: Component + Copy>(&self, entity: Entity) -> T {
        *self.app.world().get::<T>(entity).expect("component")
    }

    fn position(&self, entity: Entity) -> WorldPos {
        self.get::<BodyPosition>(entity).0
    }

    fn translation(&self, entity: Entity) -> DVec3 {
        self.get::<Transform>(entity).translation
    }

    fn frozen(&self, entity: Entity) -> bool {
        self.app.world().get::<Frozen>(entity).is_some()
    }
}

/// The same flat ground as a `TestWorld`, around `(0, 0, 0)`.
fn test_ground() -> TestWorld {
    let grass = BlockRegistry::default_blocks()
        .by_name("boxel:grass")
        .expect("built-in");
    let mut world = TestWorld::new();
    world.fill(BlockPos::new(-40, 0, -40), BlockPos::new(40, 2, 40), STONE);
    world.fill(BlockPos::new(-40, 3, -40), BlockPos::new(40, 3, 40), grass);
    world
}

#[test]
fn a_dropped_body_lands_exactly_with_one_impact_like_the_integrator() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let start = sim.pos(0.5, 44.0, 0.5);
    let body = sim.spawn(start, ());
    sim.steps(200);

    assert_eq!(sim.position(body), sim.pos(0.5, 4.0, 0.5));
    assert_eq!(sim.translation(body), DVec3::new(0.5, 4.0, 0.5));
    assert_eq!(sim.get::<Velocity>(body).0, DVec3::ZERO);
    let contacts = sim.get::<Contacts>(body);
    assert!(contacts.on_ground && contacts.touched(Face::NegY));
    assert_eq!(contacts.ground_block, Some(BlockPos::new(0, 3, 0)));
    let impacts = &sim.app.world().resource::<Impacts>().0;
    assert_eq!(impacts.len(), 1, "{impacts:?}");
    let impact = impacts[0];
    assert_eq!(
        (impact.entity, impact.face, impact.block),
        (body, Face::NegY, BlockPos::new(0, 3, 0))
    );

    // The pure integrator over the same ground gives the same landing speed, bit for bit.
    let mut world = test_ground();
    let params = sim
        .app
        .world()
        .resource::<PhysicsSettings>()
        .step_params(1.0 / 64.0);
    let (mut pos, mut vel, mut on_ground) = (start, DVec3::ZERO, false);
    let mut reference = None;
    for _ in 0..200 {
        let out = step_body(
            &mut world,
            &table(),
            pos,
            vel,
            player(),
            on_ground,
            &BodySettings::player(),
            &params,
        )
        .expect("loaded");
        reference = reference.or(out.impacts().next());
        (pos, vel, on_ground) = (out.pos, out.vel, out.sweep.on_ground());
    }
    let reference = reference.expect("the reference lands too");
    assert_eq!(impact.speed.to_bits(), reference.speed.to_bits());
    assert_eq!(pos, sim.position(body));
}

#[test]
fn a_body_over_unloaded_chunks_freezes_until_they_load() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // 640 blocks east: no chunk there (the viewer loads one chunk around the origin).
    let start = sim.pos(640.5, 20.0, 0.5);
    let body = sim.spawn(start, Velocity(DVec3::new(1.0, 0.0, 0.0)));
    sim.steps(10);
    assert!(sim.frozen(body));
    assert_eq!(sim.position(body), start, "no move");
    assert_eq!(
        sim.get::<Velocity>(body).0,
        DVec3::new(1.0, 0.0, 0.0),
        "no gravity"
    );
    assert_eq!(sim.app.world().resource::<PhysicsStats>().frozen, 1);

    // The world loads there: it falls and lands.
    sim.viewer(DVec3::new(640.0, 0.0, 0.0));
    sim.settle();
    sim.steps(200);
    assert!(!sim.frozen(body));
    let landed = sim.position(body);
    assert_eq!((landed.block.y, landed.frac.y), (4, 0.0));
    assert!(sim.get::<Contacts>(body).on_ground);
    assert_eq!(sim.app.world().resource::<PhysicsStats>().frozen, 0);
}

#[test]
fn a_block_placed_into_a_body_pushes_it_out_and_removing_the_ground_drops_it() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.spawn(sim.pos(0.5, 4.0, 0.5), ());
    sim.steps(2);
    assert!(sim.get::<Contacts>(body).on_ground);

    // A stone in the body's feet: the nearest free side is 0.8 blocks along X (closer than the
    // block up), +X first.
    sim.edit(sim.block(0, 4, 0), STONE);
    sim.steps(3);
    let p = sim.position(body);
    assert_eq!((p.block.y, p.frac.y), (4, 0.0), "still on the ground");
    let min_x = p.block.x as f64 + p.frac.x - 0.3;
    assert!(min_x >= 1.0 - 1e-9, "out of the block: {p:?}");
    assert!(sim.get::<Contacts>(body).on_ground);

    // The grass under it goes: it falls one block onto the stone.
    for x in -1..=3 {
        for z in -1..=1 {
            sim.edit(sim.block(x, 3, z), BlockState::AIR);
        }
    }
    sim.steps(64);
    let p = sim.position(body);
    assert_eq!((p.block.y, p.frac.y), (3, 0.0));
    assert_eq!(
        sim.get::<Contacts>(body).ground_block,
        Some(sim.block(p.block.x, 2, 0))
    );
}

#[test]
fn writing_transform_or_adding_teleport_moves_a_body_exactly() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.spawn(sim.pos(0.5, 4.0, 0.5), ());
    sim.steps(2);
    let params = PhysicsSettings::default().step_params(1.0 / 64.0);
    // The next step from `pos` in open air, as the body would do it.
    let expect = |sim: &Sim, pos: WorldPos| {
        let mut air = TestWorld::new();
        step_body(
            &mut air,
            &table(),
            pos,
            sim.get::<Velocity>(body).0,
            player(),
            sim.get::<Contacts>(body).on_ground,
            &BodySettings::player(),
            &params,
        )
        .expect("air")
        .pos
    };

    // Gameplay writes `Transform`: the next step starts from it.
    let target = DVec3::new(8.25, 10.0, 3.75);
    let want = expect(&sim, WorldPos::from_dvec3(target));
    sim.app
        .world_mut()
        .get_mut::<Transform>(body)
        .expect("transform")
        .translation = target;
    sim.steps(1);
    assert_eq!(sim.position(body), want);
    assert_eq!(sim.translation(body), want.to_dvec3());

    // `Teleport` places it exactly and removes itself; the velocity is kept.
    let target = sim.pos(2.125, 30.5, -4.875);
    let falling = sim.get::<Velocity>(body).0;
    assert!(falling.y < 0.0);
    let want = expect(&sim, target);
    sim.app
        .world_mut()
        .entity_mut(body)
        .insert(Teleport(target));
    sim.steps(1);
    assert_eq!(sim.position(body), want);
    assert!(sim.app.world().get::<Teleport>(body).is_none());
    assert!(
        sim.get::<Velocity>(body).0.y < falling.y,
        "kept and still falling"
    );
}

#[test]
fn interpolated_transforms_stay_between_steps_and_never_smear_a_teleport() {
    let mut sim = Sim::new(I64Vec3::ZERO, FRAME_144);
    let body = sim.spawn(
        sim.pos(0.5, 30.0, 0.5),
        (Interpolated::default(), Velocity(DVec3::new(3.0, 0.0, 0.0))),
    );
    let mut teleported = false;
    for frame in 0..400 {
        if frame == 200 {
            // Gameplay moves it 20 blocks east between frames.
            let mut transform = sim
                .app
                .world_mut()
                .get_mut::<Transform>(body)
                .expect("transform");
            transform.translation = DVec3::new(40.5, 20.0, 0.5);
            teleported = true;
        }
        sim.frame(FRAME_144);
        let t = sim.translation(body);
        let curr = sim.position(body);
        let Some(prev) = sim.get::<Interpolated>(body).previous() else {
            continue;
        };
        let alpha = sim
            .app
            .world()
            .resource::<FixedTime>()
            .overstep_fraction_f64();
        let steps = sim.app.world().resource::<FixedTime>().steps_last_frame();
        if teleported && steps == 0 && t == DVec3::new(40.5, 20.0, 0.5) {
            // The frame of the write ran no step: the written value stays until the next one.
            continue;
        }
        assert_eq!(
            t,
            prev.to_dvec3() + prev.delta_to(&curr) * alpha,
            "frame {frame}"
        );
        let (lo, hi) = (
            prev.to_dvec3().min(curr.to_dvec3()),
            prev.to_dvec3().max(curr.to_dvec3()),
        );
        assert!(
            t.cmpge(lo).all() && t.cmple(hi).all(),
            "frame {frame}: {t:?}"
        );
        if teleported {
            assert!(t.x >= 40.5, "no smear back toward the old place: {t:?}");
        }
    }
    assert!(
        sim.get::<Contacts>(body).on_ground,
        "landed after the teleport"
    );
}

#[test]
fn every_impact_reaches_update_at_144_fps() {
    let mut sim = Sim::new(I64Vec3::ZERO, FRAME_144);
    let bodies: BTreeSet<Entity> = (0..24)
        .map(|i| {
            let pos = sim.pos(
                f64::from(i % 6) * 2.0 + 0.5,
                6.0 + f64::from(i),
                -6.5 + f64::from(i / 6) * 3.0,
            );
            sim.spawn(pos, ())
        })
        .collect();
    for _ in 0..144 * 3 {
        sim.frame(FRAME_144);
    }
    let impacts = &sim.app.world().resource::<Impacts>().0;
    assert_eq!(impacts.len(), bodies.len(), "one landing each: {impacts:?}");
    let hit: BTreeSet<Entity> = impacts.iter().map(|i| i.entity).collect();
    assert_eq!(hit, bodies);
    assert!(
        impacts
            .iter()
            .all(|i| i.face == Face::NegY && i.speed > 4.0)
    );
}

#[test]
fn the_jump_latch_jumps_once_per_press_whatever_the_steps_per_frame() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    let body = sim.spawn(sim.pos(0.5, 4.0, 0.5), CharacterController::default());
    sim.steps(3);
    assert!(sim.get::<Contacts>(body).on_ground);
    let jump = CharacterController::default().jump_speed;
    let params = PhysicsSettings::default().step_params(1.0 / 64.0);
    let settings = BodySettings::player();
    let press = |sim: &mut Sim| {
        sim.app
            .world_mut()
            .get_mut::<MoveIntent>(body)
            .expect("intent")
            .jump = true;
    };
    let latch = |sim: &Sim| sim.get::<MoveIntent>(body).jump;
    let steps = |sim: &Sim| sim.app.world().resource::<FixedTime>().steps_last_frame();

    // A press in a frame that runs no step waits for the next step.
    press(&mut sim);
    sim.frame(Duration::from_millis(5));
    assert_eq!(steps(&sim), 0);
    assert!(latch(&sim));
    assert_eq!(sim.get::<Velocity>(body).0, DVec3::ZERO);
    sim.frame(STEP - Duration::from_millis(5));
    assert_eq!(steps(&sim), 1);
    assert!(!latch(&sim));
    let once = integrate_velocity(DVec3::new(0.0, jump, 0.0), true, &settings, &params);
    assert_eq!(sim.get::<Velocity>(body).0, once);

    // Back on the ground, a press in a frame that runs two steps jumps once.
    for _ in 0..64 {
        sim.steps(1);
        if sim.get::<Contacts>(body).on_ground {
            break;
        }
    }
    assert!(sim.get::<Contacts>(body).on_ground);
    assert_eq!(sim.position(body).frac.y, 0.0);
    press(&mut sim);
    sim.frame(STEP * 2);
    assert_eq!(steps(&sim), 2);
    assert!(!latch(&sim));
    let twice = integrate_velocity(once, false, &settings, &params);
    assert_eq!(sim.get::<Velocity>(body).0, twice);
}

#[test]
fn a_character_walks_at_its_speed_and_jumps_onto_a_step() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    // A one-block step from x = 6 on.
    for x in 6..=24 {
        for z in -2..=2 {
            sim.edit(sim.block(x, 4, z), STONE);
        }
    }
    let body = sim.spawn(sim.pos(-2.5, 4.0, 0.5), CharacterController::default());
    let walk = |sim: &mut Sim, wish: Vec2, jump: bool| {
        let mut intent = sim
            .app
            .world_mut()
            .get_mut::<MoveIntent>(body)
            .expect("intent");
        intent.wish = wish;
        intent.jump |= jump;
    };
    // Walking +X on flat ground levels off at the walking speed.
    for _ in 0..64 {
        walk(&mut sim, Vec2::X, false);
        sim.steps(1);
    }
    // Ground friction 12 /s: within 4.317 e^-11 of the top speed after a second.
    let v = sim.get::<Velocity>(body).0;
    let speed = CharacterController::default().walk_speed;
    assert!((v.x - speed).abs() < 1e-3, "{v:?}");
    assert_eq!(v.z, 0.0);
    // Holding jump while walking into the step climbs it.
    for _ in 0..128 {
        walk(&mut sim, Vec2::X, true);
        sim.steps(1);
    }
    let p = sim.position(body);
    assert!(p.block.x >= 6, "on the step: {p:?}");
    let mut on_top = false;
    for _ in 0..64 {
        walk(&mut sim, Vec2::ZERO, false);
        sim.steps(1);
        let p = sim.position(body);
        if sim.get::<Contacts>(body).on_ground && p.block.y == 5 && p.frac.y == 0.0 {
            on_top = true;
            break;
        }
    }
    assert!(on_top, "{:?}", sim.position(body));
}

#[test]
fn stats_count_bodies_and_feed_the_board() {
    let mut sim = Sim::new(I64Vec3::ZERO, STEP);
    sim.app.init_resource::<DiagnosticsBoard>();
    sim.spawn(sim.pos(0.5, 10.0, 0.5), ());
    sim.spawn(sim.pos(2.5, 10.0, 0.5), ());
    sim.spawn(sim.pos(900.5, 10.0, 0.5), ());
    sim.app
        .world_mut()
        .spawn((RigidBody::Static, Transform::from_xyz(5.0, 5.0, 5.0)));
    sim.steps(2);
    let stats = *sim.app.world().resource::<PhysicsStats>();
    assert_eq!(
        (stats.bodies, stats.dynamic, stats.frozen),
        (4, 3, 1),
        "{stats:?}"
    );
    assert!(stats.steps >= 2);
    let board = sim.app.world().resource::<DiagnosticsBoard>();
    assert_eq!(board.get("physics"), Some(stats.summary().as_str()));
    assert!(stats.summary().starts_with("bodies=4 dynamic=3 frozen=1"));
}

// ------------------------------------------------------------------------- determinism

/// A wandering character of the determinism scene: its wish and jumps come from its seed and
/// the step number (written in `FixedUpdate`, as game AI would).
#[derive(Component)]
struct Walker(u64);

/// Fixed steps since the scene was spawned.
#[derive(Resource, Default)]
struct SceneSteps(u64);

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn wander(mut steps: ResMut<SceneSteps>, mut walkers: Query<(&Walker, &mut MoveIntent)>) {
    steps.0 += 1;
    for (walker, mut intent) in &mut walkers {
        // A new direction every half second.
        let r = mix(walker.0 ^ (steps.0 / 32).wrapping_mul(0x1000_0001));
        let unit = |bits: u64| (bits & 0xff) as f32 / 127.5 - 1.0;
        intent.wish = Vec2::new(unit(r), unit(r >> 8));
        if steps.0.is_multiple_of(32) && (r >> 16) & 3 == 0 {
            intent.jump = true;
        }
    }
}

/// One body's state relative to the scene's base: block, `frac` and velocity bits, contacts.
type BodyState = (I64Vec3, [u64; 3], [u64; 3], u8, Option<I64Vec3>);

/// Spawns the scene (a wall, walkers, bouncing balls, items, a kinematic body), runs `steps`
/// fixed steps with frames of `frame`, and returns every body's state in spawn order.
fn scene(base: I64Vec3, frame: Duration, interpolated: bool, steps: u64) -> Vec<BodyState> {
    let mut sim = Sim::new(base, frame);
    sim.app
        .init_resource::<SceneSteps>()
        .add_systems(FixedUpdate, wander);
    for y in 4..=5 {
        for z in -6..=6 {
            sim.edit(sim.block(12, y, z), STONE);
        }
    }
    let wall = sim.block(12, 5, 6);
    while sim.app.world().resource::<ChunkMap>().block_at(wall) != Some(STONE) {
        sim.frame(frame);
    }
    let mut bodies = Vec::new();
    let mut spawn = |sim: &mut Sim, pos: WorldPos, bundle: (Collider, BodySettings, Velocity)| {
        let entity = sim.body(pos, bundle);
        if interpolated {
            sim.app
                .world_mut()
                .entity_mut(entity)
                .insert(Interpolated::default());
        }
        bodies.push(entity);
        entity
    };
    for i in 0..4u32 {
        let pos = sim.pos(2.5 + 3.0 * f64::from(i), 4.0, 0.5);
        let walker = spawn(
            &mut sim,
            pos,
            (
                Collider::standing(0.6, 1.8),
                BodySettings::player(),
                Velocity::default(),
            ),
        );
        sim.app
            .world_mut()
            .entity_mut(walker)
            .insert((CharacterController::default(), Walker(u64::from(i) + 1)));
    }
    for i in 0..4 {
        let d = f64::from(i);
        let pos = sim.pos(1.25 + 2.0 * d, 12.0 + d, 4.5);
        spawn(
            &mut sim,
            pos,
            (
                Collider::sphere(0.25),
                BodySettings::ball(),
                Velocity(DVec3::new(6.0 - 2.0 * d, 5.0, -1.5 + d)),
            ),
        );
        let pos = sim.pos(0.5 + 2.5 * d, 9.0, -4.5);
        spawn(
            &mut sim,
            pos,
            (
                Collider::aabb(DVec3::splat(0.125)).sensor(),
                BodySettings::item(),
                Velocity(DVec3::new(3.0, 4.0, 1.0 + d)),
            ),
        );
    }
    let pos = sim.pos(0.5, 8.0, 8.5);
    let kinematic = sim
        .app
        .world_mut()
        .spawn((
            RigidBody::Kinematic,
            Teleport(pos),
            Velocity(DVec3::new(0.75, 0.0, -0.25)),
        ))
        .id();
    bodies.push(kinematic);
    sim.app.world_mut().resource_mut::<SceneSteps>().0 = 0;
    while sim.app.world().resource::<SceneSteps>().0 < steps {
        sim.frame(frame);
    }
    assert_eq!(sim.app.world().resource::<SceneSteps>().0, steps);
    let local = |p: BlockPos| p.as_i64vec3() - base;
    bodies
        .iter()
        .map(|&entity| {
            let p = sim.position(entity);
            let contacts = sim.get::<Contacts>(entity);
            (
                local(p.block),
                p.frac.to_array().map(f64::to_bits),
                sim.get::<Velocity>(entity).0.to_array().map(f64::to_bits),
                contacts.faces,
                contacts.ground_block.map(local),
            )
        })
        .collect()
}

#[test]
fn runs_are_bit_identical_at_any_frame_rate_with_or_without_interpolation() {
    let reference = scene(I64Vec3::ZERO, STEP, false, 1000);
    // Something happened: the walkers wandered off (the first one starts in block x = 2), the
    // balls and items came to rest on the ground.
    assert!(
        reference[..4]
            .iter()
            .any(|s| (s.0 - I64Vec3::new(2, 4, 0)).abs().max_element() > 3)
    );
    // Friction slows a slide exponentially: nearly still, not exactly 0.
    let at_rest = |s: &BodyState| {
        s.3 == 1 << Face::NegY.index() && s.2.iter().all(|&v| f64::from_bits(v).abs() < 1e-3)
    };
    assert!(reference[4..12].iter().all(at_rest), "{reference:?}");
    assert_eq!(scene(I64Vec3::ZERO, STEP, false, 1000), reference, "again");
    assert_eq!(
        scene(I64Vec3::ZERO, FRAME_144, false, 1000),
        reference,
        "144 fps"
    );
    assert_eq!(
        scene(I64Vec3::ZERO, STEP, true, 1000),
        reference,
        "interpolated"
    );
}

#[test]
fn the_same_scene_far_away_is_bit_identical() {
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
