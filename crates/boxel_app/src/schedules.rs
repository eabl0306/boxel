use crate::{App, Plugin};
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::{InternedScheduleLabel, ScheduleLabel, SingleThreadedExecutor};

macro_rules! labels {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash, Default)]
        pub struct $name;
    )*};
}

labels! {
    /// Root schedule run by `App::update`; runs the others in [`MainScheduleOrder`].
    Main,
    /// Once, before `Startup`.
    PreStartup,
    /// Once, on the first update.
    Startup,
    /// Once, after `Startup`.
    PostStartup,
    /// First every frame: message rotation, time update.
    First,
    /// Input processing.
    PreUpdate,
    /// Runs `FixedMain` zero or more times (accumulator in [`crate::FixedTime`]).
    RunFixedMainLoop,
    /// Root of the fixed-timestep schedules.
    FixedMain,
    /// Fixed step, before `FixedUpdate`.
    FixedPreUpdate,
    /// Fixed step gameplay/simulation.
    FixedUpdate,
    /// Fixed step, after `FixedUpdate`.
    FixedPostUpdate,
    /// Gameplay and engine logic (world streaming lives here).
    Update,
    /// Reactions to `Update` (camera, transforms).
    PostUpdate,
    /// Last of the simulation frame.
    Last,
    /// Not part of the frame: run once by the runner through `App::shutdown` after the last
    /// update, while the window and event loop still exist. Plugins release GPU/window resources
    /// here in a defined order and flush state (last-known-good, future chunk stores).
    Shutdown,
}

/// Order of the per-frame and startup schedules run by [`Main`].
#[derive(Resource, Debug)]
pub struct MainScheduleOrder {
    /// Per-frame labels, in order.
    pub labels: Vec<InternedScheduleLabel>,
    /// Startup labels, run once before the first frame.
    pub startup_labels: Vec<InternedScheduleLabel>,
}

impl Default for MainScheduleOrder {
    fn default() -> Self {
        Self {
            labels: vec![
                First.intern(),
                PreUpdate.intern(),
                RunFixedMainLoop.intern(),
                Update.intern(),
                PostUpdate.intern(),
                Last.intern(),
            ],
            startup_labels: vec![PreStartup.intern(), Startup.intern(), PostStartup.intern()],
        }
    }
}

impl MainScheduleOrder {
    /// Inserts `schedule` right after `after`.
    ///
    /// # Panics
    /// If `after` is not in the order.
    pub fn insert_after(&mut self, after: impl ScheduleLabel, schedule: impl ScheduleLabel) {
        let i = self
            .labels
            .iter()
            .position(|l| (**l).eq(&after))
            .unwrap_or_else(|| panic!("{after:?} is not in MainScheduleOrder"));
        self.labels.insert(i + 1, schedule.intern());
    }

    /// Inserts `schedule` right before `before`.
    ///
    /// # Panics
    /// If `before` is not in the order.
    pub fn insert_before(&mut self, before: impl ScheduleLabel, schedule: impl ScheduleLabel) {
        let i = self
            .labels
            .iter()
            .position(|l| (**l).eq(&before))
            .unwrap_or_else(|| panic!("{before:?} is not in MainScheduleOrder"));
        self.labels.insert(i, schedule.intern());
    }
}

/// Order of the fixed-step schedules run by [`FixedMain`].
#[derive(Resource, Debug)]
pub struct FixedMainScheduleOrder {
    /// Labels, in order.
    pub labels: Vec<InternedScheduleLabel>,
}

impl Default for FixedMainScheduleOrder {
    fn default() -> Self {
        Self {
            labels: vec![
                FixedPreUpdate.intern(),
                FixedUpdate.intern(),
                FixedPostUpdate.intern(),
            ],
        }
    }
}

fn run_main(world: &mut World, mut started: Local<bool>) {
    world.resource_scope(|world, order: Mut<MainScheduleOrder>| {
        if !*started {
            for &label in &order.startup_labels {
                let _ = world.try_run_schedule(label);
            }
            *started = true;
        }
        for &label in &order.labels {
            let _ = world.try_run_schedule(label);
        }
    });
}

fn run_fixed_main(world: &mut World) {
    world.resource_scope(|world, order: Mut<FixedMainScheduleOrder>| {
        for &label in &order.labels {
            let _ = world.try_run_schedule(label);
        }
    });
}

/// Installs `Main`/`FixedMain` (single-threaded facilitator schedules) and their order resources.
/// Added by [`App::new`].
pub struct MainSchedulePlugin;

impl Plugin for MainSchedulePlugin {
    fn build(&self, app: &mut App) {
        let mut main = Schedule::new(Main);
        main.set_executor(SingleThreadedExecutor::new());
        let mut fixed_main = Schedule::new(FixedMain);
        fixed_main.set_executor(SingleThreadedExecutor::new());
        app.add_schedule(main)
            .add_schedule(fixed_main)
            .init_resource::<MainScheduleOrder>()
            .init_resource::<FixedMainScheduleOrder>()
            .add_systems(Main, run_main)
            .add_systems(FixedMain, run_fixed_main);
    }
}
