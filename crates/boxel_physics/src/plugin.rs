//! [`PhysicsPlugin`] and the physics view of the world's blocks.

use crate::components::{
    CollisionEnded, CollisionStarted, Disturbed, PhysicsSettings, PhysicsStats, ProjectileHit,
    SleepLog, Vacated, VoxelImpact,
};
use crate::contacts::{
    BodyIndex, ContactSet, ObstacleIndex, broadphase, build_obstacles, solve, write_collisions,
};
use crate::core::CollisionTable;
use crate::projectile::{PendingHits, step_projectiles, write_hits};
use crate::sleep::{sleep_bodies, wake_bodies, wake_to_pose};
use crate::systems::{
    AwakeCount, PendingImpacts, PhysicsSystems, WritebackTick, drive_characters, interpolate,
    move_kinematic, prepare_bodies, publish_stats, step_dynamic, write_back, write_events,
};
use boxel_app::{
    App, FixedPostUpdate, FixedTime, Last, Plugin, RunFixedMainLoop, RunFixedMainLoopSystems,
};
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::SingleThreadedExecutor;
use boxel_world::{BlockChanged, ChunkDataEvent, WorldContent};
use std::sync::Arc;

/// The physics view of the world's blocks: the [`CollisionTable`] derived from
/// `WorldContent::registry`. Inserted by [`PhysicsPlugin`] in `Plugin::finish`.
#[derive(Resource, Clone, Debug)]
pub struct PhysicsContent {
    /// How each block state collides, plus the fluid and outline flags.
    pub collision: Arc<CollisionTable>,
}

/// Adds physics to an app with a voxel world (`WorldPlugin`) and time (`TimePlugin`). Not part
/// of `DefaultPlugins`: games add it.
///
/// - Bodies ([`crate::RigidBody`] and the other components) move in fixed steps, in
///   `FixedPostUpdate`, in the chained [`PhysicsSystems`]; interpolated `Transform`s are written
///   after the frame's steps (`RunFixedMainLoopSystems::AfterFixedMainLoop`).
/// - Resources: [`PhysicsSettings`], [`PhysicsStats`] (a `"physics"` line on `DiagnosticsBoard`,
///   refreshed in `Last`), [`BodyIndex`] (the bodies for entity queries) and, in
///   `Plugin::finish`, [`PhysicsContent`] from the world's registry, which
///   [`crate::PhysicsQuery`] and the step read.
/// - Messages: [`VoxelImpact`], [`CollisionStarted`], [`CollisionEnded`] and
///   [`ProjectileHit`]. It reads the world's `BlockChanged` and `ChunkDataEvent` to wake
///   sleeping bodies.
/// - In `Plugin::finish` it sets `FixedTime::set_max_steps_per_frame` to
///   [`PhysicsPlugin::max_steps_per_frame`] (3 by default): after a hitch, a frame runs at most
///   that many fixed steps, which bounds the cost of a catch-up frame. The cap is app-wide, see
///   the field.
/// - With [`PhysicsPlugin::single_threaded`] (the default) `FixedPostUpdate` and
///   `RunFixedMainLoop` run on the single-threaded executor: the physics systems are a chain,
///   so threads only add overhead.
///
/// Runners call `App::finish` before the first update; a test that drives `App::update` by hand
/// calls it too.
///
/// # Panics
///
/// In `finish`, if the app has no `WorldContent` (add `WorldPlugin`).
#[derive(Clone, Copy, Debug)]
pub struct PhysicsPlugin {
    /// The most fixed steps a frame runs (`FixedTime::set_max_steps_per_frame`), set in
    /// `Plugin::finish`. `None` leaves `FixedTime` as it is (the engine default is 8). 0 counts
    /// as 1.
    ///
    /// The cap is for the whole app, not only physics: every `FixedMain` schedule
    /// (`FixedUpdate` too) runs at most this many times a frame, and the time left over is
    /// dropped (an overrun, logged at most every 5 s). So it is also a frame rate floor: below
    /// `steps * rate` fps (21.3 fps with 3 steps at 64 Hz, 42.7 fps at 128 Hz) the fixed-step
    /// game runs in slow motion. A game with few bodies that must keep real time on slow
    /// machines can pass `None`. `finish` overrides a cap set earlier (in another plugin's
    /// `build`); change it after `App::finish` to keep your own.
    pub max_steps_per_frame: Option<u32>,
    /// Run `FixedPostUpdate` (the physics step and any other system there) and
    /// `RunFixedMainLoop` (input sampling, the fixed loop and the interpolation, every frame) on
    /// the single-threaded executor. The physics systems are chained, so the multi-threaded
    /// executor gains nothing and costs about 24 us per run on its own (measured on
    /// `FixedPostUpdate`: an empty step takes 0.65 us instead of 25 us, and 1000 bodies 0.22 ms
    /// instead of 0.32 ms). `RunFixedMainLoop` is a chain too (before the loop, the exclusive
    /// loop, after it). `false` leaves both executors alone.
    pub single_threaded: bool,
}

impl Default for PhysicsPlugin {
    /// At most 3 fixed steps per frame, as the physics plan recommends, and a single-threaded
    /// `FixedPostUpdate`.
    fn default() -> Self {
        Self {
            max_steps_per_frame: Some(3),
            single_threaded: true,
        }
    }
}

impl Plugin for PhysicsPlugin {
    fn build(&self, app: &mut App) {
        if self.single_threaded {
            let single = |schedule: &mut Schedule| {
                schedule.set_executor(SingleThreadedExecutor::new());
            };
            app.edit_schedule(FixedPostUpdate, single)
                .edit_schedule(RunFixedMainLoop, single);
        }
        app.init_resource::<PhysicsSettings>()
            .init_resource::<PhysicsStats>()
            .init_resource::<PendingImpacts>()
            .init_resource::<ObstacleIndex>()
            .init_resource::<BodyIndex>()
            .init_resource::<ContactSet>()
            .init_resource::<SleepLog>()
            .init_resource::<Vacated>()
            .init_resource::<Disturbed>()
            .init_resource::<AwakeCount>()
            .init_resource::<WritebackTick>()
            .init_resource::<PendingHits>()
            .add_message::<VoxelImpact>()
            .add_message::<CollisionStarted>()
            .add_message::<CollisionEnded>()
            .add_message::<ProjectileHit>()
            // The world's (also added by `WorldPlugin`, in any order).
            .add_message::<BlockChanged>()
            .add_message::<ChunkDataEvent>()
            .configure_sets(
                FixedPostUpdate,
                (
                    PhysicsSystems::Prepare,
                    PhysicsSystems::Controller,
                    PhysicsSystems::Wake,
                    PhysicsSystems::Kinematic,
                    PhysicsSystems::Step,
                    PhysicsSystems::Broadphase,
                    PhysicsSystems::Solve,
                    PhysicsSystems::Projectiles,
                    PhysicsSystems::Sleep,
                    PhysicsSystems::Events,
                    PhysicsSystems::Writeback,
                )
                    .chain(),
            )
            .add_systems(
                FixedPostUpdate,
                (
                    (prepare_bodies, wake_to_pose)
                        .chain()
                        .in_set(PhysicsSystems::Prepare),
                    drive_characters.in_set(PhysicsSystems::Controller),
                    wake_bodies.in_set(PhysicsSystems::Wake),
                    (move_kinematic, build_obstacles)
                        .chain()
                        .in_set(PhysicsSystems::Kinematic),
                    step_dynamic.in_set(PhysicsSystems::Step),
                    broadphase.in_set(PhysicsSystems::Broadphase),
                    solve.in_set(PhysicsSystems::Solve),
                    step_projectiles.in_set(PhysicsSystems::Projectiles),
                    sleep_bodies.in_set(PhysicsSystems::Sleep),
                    (write_events, write_collisions, write_hits)
                        .chain()
                        .in_set(PhysicsSystems::Events),
                    write_back.in_set(PhysicsSystems::Writeback),
                ),
            )
            .configure_sets(
                RunFixedMainLoop,
                PhysicsSystems::Interpolate.in_set(RunFixedMainLoopSystems::AfterFixedMainLoop),
            )
            .add_systems(
                RunFixedMainLoop,
                interpolate.in_set(PhysicsSystems::Interpolate),
            )
            // It writes only its own line of the board: its order against the other board
            // writers in `Last` (the world's stats) does not matter.
            .add_systems(Last, publish_stats.ambiguous_with_all());
    }

    fn finish(&self, app: &mut App) {
        let Some(content) = app.world().get_resource::<WorldContent>() else {
            panic!("PhysicsPlugin needs the voxel world: add WorldPlugin (no WorldContent found)");
        };
        let collision = Arc::new(CollisionTable::from_registry(&content.registry));
        app.insert_resource(PhysicsContent { collision });
        if let Some(steps) = self.max_steps_per_frame
            && let Some(mut fixed) = app.world_mut().get_resource_mut::<FixedTime>()
        {
            fixed.set_max_steps_per_frame(steps.max(1));
        }
    }
}
