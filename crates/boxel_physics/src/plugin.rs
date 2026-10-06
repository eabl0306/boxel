//! [`PhysicsPlugin`] and the physics view of the world's blocks.

use crate::components::{PhysicsSettings, PhysicsStats, VoxelImpact};
use crate::core::CollisionTable;
use crate::systems::{
    PendingImpacts, PhysicsSystems, drive_characters, interpolate, move_kinematic, prepare_bodies,
    publish_stats, step_dynamic, write_back, write_events,
};
use boxel_app::{
    App, FixedPostUpdate, FixedTime, Last, Plugin, RunFixedMainLoop, RunFixedMainLoopSystems,
};
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::SingleThreadedExecutor;
use boxel_world::WorldContent;
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
///   refreshed in `Last`) and, in `Plugin::finish`, [`PhysicsContent`] from the world's
///   registry, which [`crate::PhysicsQuery`] and the step read.
/// - Message: [`VoxelImpact`].
/// - In `Plugin::finish` it lowers `FixedTime::set_max_steps_per_frame` to
///   [`PhysicsPlugin::max_steps_per_frame`] (3 by default): after a hitch, a frame runs at most
///   that many physics steps, which bounds the cost of a catch-up frame.
/// - With [`PhysicsPlugin::single_threaded`] (the default) `FixedPostUpdate` runs on the
///   single-threaded executor: the physics systems are a chain, so threads only add overhead.
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
    pub max_steps_per_frame: Option<u32>,
    /// Run `FixedPostUpdate` (the physics step and any other system there) on the
    /// single-threaded executor. The physics systems are chained, so the multi-threaded executor
    /// gains nothing and costs about 24 us per step on its own (measured: an empty step takes
    /// 0.65 us instead of 25 us, and 1000 bodies 0.22 ms instead of 0.32 ms). `false` leaves the
    /// schedule's executor alone.
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
            app.edit_schedule(FixedPostUpdate, |schedule| {
                schedule.set_executor(SingleThreadedExecutor::new());
            });
        }
        app.init_resource::<PhysicsSettings>()
            .init_resource::<PhysicsStats>()
            .init_resource::<PendingImpacts>()
            .add_message::<VoxelImpact>()
            .configure_sets(
                FixedPostUpdate,
                (
                    PhysicsSystems::Controller,
                    PhysicsSystems::Prepare,
                    PhysicsSystems::Kinematic,
                    PhysicsSystems::Step,
                    PhysicsSystems::Broadphase,
                    PhysicsSystems::Solve,
                    PhysicsSystems::Events,
                    PhysicsSystems::Writeback,
                )
                    .chain(),
            )
            .add_systems(
                FixedPostUpdate,
                (
                    drive_characters.in_set(PhysicsSystems::Controller),
                    prepare_bodies.in_set(PhysicsSystems::Prepare),
                    move_kinematic.in_set(PhysicsSystems::Kinematic),
                    step_dynamic.in_set(PhysicsSystems::Step),
                    write_events.in_set(PhysicsSystems::Events),
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
