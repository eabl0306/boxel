//! The headless app of the ECS physics tests: `WorldPlugin` with inline jobs over a flat test
//! world (stone at y 0..=2 and grass at y = 3 relative to a base block, so the ground is at
//! y = 4), `PhysicsPlugin`, and `TimePlugin` on a synthetic clock (frames of a set length, so
//! every run has the same fixed steps). `Update` logs the impacts and the collision events.
#![allow(dead_code, reason = "each test file uses a different subset")]

use boxel_app::{App, First, FixedTime, Plugins, Time, TimePlugin, TimeSystems, Update};
use boxel_ecs::prelude::*;
use boxel_math::{DVec3, I64Vec3};
use boxel_physics::{
    BodyPosition, Collider, CollisionEnded, CollisionStarted, Frozen, PhysicsPlugin,
    PhysicsSettings, ProjectileHit, RigidBody, Sleeping, Teleport, VoxelImpact,
};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, WorldPos};
use boxel_world::jobs::InlineExecutor;
use boxel_world::{ChunkViewer, EditQueue, WorldPlugin, WorldStats};
use boxel_worldgen::{FlatGenerator, WorldGenerator};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One fixed step at 64 Hz (exactly `FixedTime::timestep()`).
pub const STEP: Duration = Duration::from_nanos(15_625_000);
/// A frame at 144 fps: at most one step each.
pub const FRAME_144: Duration = Duration::from_nanos(6_944_444);

/// Flat layers (stone y 0..=2, grass y = 3) moved by whole chunks to `base`.
pub struct ShiftedFlat {
    pub flat: FlatGenerator,
    pub base: ChunkPos,
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
pub struct Clock {
    time: Time,
    now: Instant,
    next: Duration,
}

/// Replaces the real clock (runs right after `time_system`).
pub fn synthetic_time(mut clock: ResMut<Clock>, mut time: ResMut<Time>) {
    let now = clock.now + clock.next;
    clock.now = now;
    clock.time.update_with_instant(now);
    *time = clock.time.clone();
}

/// Every `VoxelImpact` read in `Update`.
#[derive(Resource, Default)]
pub struct Impacts(pub Vec<VoxelImpact>);

pub fn log_impacts(mut impacts: MessageReader<VoxelImpact>, mut log: ResMut<Impacts>) {
    log.0.extend(impacts.read().copied());
}

/// A collision event read in `Update`: started (`true`) or ended, and the pair.
pub type CollisionEvent = (bool, Entity, Entity);

/// Every `CollisionStarted` and `CollisionEnded` read in `Update`, in order (each step's
/// started events, then its ended ones).
#[derive(Resource, Default)]
pub struct Collisions(pub Vec<CollisionEvent>);

pub fn log_collisions(
    mut started: MessageReader<CollisionStarted>,
    mut ended: MessageReader<CollisionEnded>,
    mut log: ResMut<Collisions>,
) {
    log.0.extend(started.read().map(|e| (true, e.a, e.b)));
    log.0.extend(ended.read().map(|e| (false, e.a, e.b)));
}

/// Every `ProjectileHit` read in `Update`.
#[derive(Resource, Default)]
pub struct Hits(pub Vec<ProjectileHit>);

pub fn log_hits(mut hits: MessageReader<ProjectileHit>, mut log: ResMut<Hits>) {
    log.0.extend(hits.read().copied());
}

/// A settled flat world around `base` and its app.
pub struct Sim {
    pub app: App,
    pub base: I64Vec3,
}

impl Sim {
    /// `base` is a multiple of 1024 on every axis, so the viewer's `f64` position is exact up
    /// to the world limit. Frames last `frame`. The viewer at `base` loads one chunk around it.
    pub fn new(base: I64Vec3, frame: Duration) -> Self {
        Self::with_plugins(base, frame, ())
    }

    /// [`Sim::new`] with more plugins, added after `PhysicsPlugin`.
    pub fn with_plugins<M>(base: I64Vec3, frame: Duration, plugins: impl Plugins<M>) -> Self {
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
        .add_plugins(plugins)
        .insert_resource(Clock {
            time: Time::new(start),
            now: start,
            next: frame,
        })
        .init_resource::<Impacts>()
        .init_resource::<Collisions>()
        .init_resource::<Hits>()
        .add_systems(First, synthetic_time.after(TimeSystems))
        .add_systems(Update, (log_impacts, log_collisions, log_hits));
        app.finish();
        app.cleanup();
        let mut sim = Self { app, base };
        sim.viewer(DVec3::ZERO);
        sim.settle();
        sim
    }

    /// A chunk viewer (radius 1) at `base + local`.
    pub fn viewer(&mut self, local: DVec3) {
        self.app.world_mut().spawn((
            Transform::from_translation(self.base.as_dvec3() + local),
            ChunkViewer::new(1).with_vertical_radius(1),
        ));
    }

    pub fn settle(&mut self) {
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
    pub fn pos(&self, x: f64, y: f64, z: f64) -> WorldPos {
        WorldPos::from_block(BlockPos::from_i64vec3(self.base), DVec3::new(x, y, z))
    }

    pub fn block(&self, x: i64, y: i64, z: i64) -> BlockPos {
        BlockPos::from_i64vec3(self.base + I64Vec3::new(x, y, z))
    }

    /// A dynamic body placed exactly at `pos` (through `Teleport`), with `bundle`.
    pub fn body(&mut self, pos: WorldPos, bundle: impl Bundle) -> Entity {
        self.app
            .world_mut()
            .spawn((RigidBody::Dynamic, Teleport(pos), bundle))
            .id()
    }

    /// A dynamic player-size body placed exactly at `pos`, plus `extra`.
    pub fn spawn(&mut self, pos: WorldPos, extra: impl Bundle) -> Entity {
        self.body(pos, (Collider::standing(0.6, 1.8), extra))
    }

    /// One update of `frame`.
    pub fn frame(&mut self, frame: Duration) {
        self.app.world_mut().resource_mut::<Clock>().next = frame;
        self.app.update();
    }

    /// One update that runs no fixed step (shorter than what the accumulator still needs).
    pub fn idle_frame(&mut self) {
        let fixed = self.app.world().resource::<FixedTime>();
        let room = fixed.timestep() - fixed.accumulator();
        self.frame(room / 2);
        assert_eq!(
            self.app.world().resource::<FixedTime>().steps_last_frame(),
            0
        );
    }

    /// `n` updates of one fixed step each.
    pub fn steps(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(STEP);
        }
    }

    pub fn edit(&mut self, pos: BlockPos, state: BlockState) {
        self.app
            .world_mut()
            .resource_mut::<EditQueue>()
            .set_block(pos, state);
    }

    pub fn get<T: Component + Copy>(&self, entity: Entity) -> T {
        *self.app.world().get::<T>(entity).expect("component")
    }

    pub fn position(&self, entity: Entity) -> WorldPos {
        self.get::<BodyPosition>(entity).0
    }

    pub fn translation(&self, entity: Entity) -> DVec3 {
        self.get::<Transform>(entity).translation
    }

    pub fn frozen(&self, entity: Entity) -> bool {
        self.app.world().get::<Frozen>(entity).is_some()
    }

    pub fn asleep(&self, entity: Entity) -> bool {
        self.app.world().get::<Sleeping>(entity).is_some()
    }

    pub fn settings(&mut self) -> Mut<'_, PhysicsSettings> {
        self.app.world_mut().resource_mut::<PhysicsSettings>()
    }
}
