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
    /// Not part of the frame: run between updates by a runner that waits for a frame limit,
    /// after the frame presented, in short steps that end before the next deadline (ADR 0023,
    /// [`App::run_idle`]). Its systems stop by [`crate::IdleStep::until`], never block, and say
    /// whether they want another step ([`crate::IdleWork`]). Never runs without a limit.
    Idle,
}

/// The three parts of [`RunFixedMainLoop`], chained (Bevy's names). [`crate::TimePlugin`] puts
/// [`crate::run_fixed_main_loop`] (which runs `FixedMain` zero or more times) in
/// [`RunFixedMainLoopSystems::FixedMainLoop`]; a system added to `RunFixedMainLoop` belongs in
/// one of the other two.
///
/// - [`RunFixedMainLoopSystems::BeforeFixedMainLoop`]: once per frame, before this frame's fixed
///   steps. Sample input for the simulation here (for example a jump latch the fixed steps
///   consume), so a press during a frame that runs no step is not lost and a frame that runs two
///   steps does not see it twice.
/// - [`RunFixedMainLoopSystems::AfterFixedMainLoop`]: once per frame, after them and before
///   `Update`. Interpolate what the steps moved here (`FixedTime::overstep_fraction_f64` is the
///   factor for `f64` positions), so `Update` and the renderer see the same interpolated state.
///
/// `Time` holds the frame view in both (only `FixedMain` sees the fixed-step view).
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[expect(
    clippy::enum_variant_names,
    reason = "Bevy's names, so code written for Bevy ports unchanged"
)]
pub enum RunFixedMainLoopSystems {
    /// Before the fixed steps of the frame.
    BeforeFixedMainLoop,
    /// The fixed steps ([`crate::run_fixed_main_loop`]).
    FixedMainLoop,
    /// After the fixed steps of the frame, before `Update`.
    AfterFixedMainLoop,
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

/// Installs `Main`/`FixedMain` (single-threaded facilitator schedules) and their order resources,
/// the empty single-threaded [`Idle`] schedule, and chains the [`RunFixedMainLoopSystems`] sets
/// in [`RunFixedMainLoop`]. Added by [`App::new`].
pub struct MainSchedulePlugin;

impl Plugin for MainSchedulePlugin {
    fn build(&self, app: &mut App) {
        let mut main = Schedule::new(Main);
        main.set_executor(SingleThreadedExecutor::new());
        let mut fixed_main = Schedule::new(FixedMain);
        fixed_main.set_executor(SingleThreadedExecutor::new());
        // A few short steps per frame: no worth in waking the compute threads for them.
        let mut idle = Schedule::new(Idle);
        idle.set_executor(SingleThreadedExecutor::new());
        app.add_schedule(main)
            .add_schedule(fixed_main)
            .add_schedule(idle)
            .init_resource::<MainScheduleOrder>()
            .init_resource::<FixedMainScheduleOrder>()
            .add_systems(Main, run_main)
            .add_systems(FixedMain, run_fixed_main)
            .configure_sets(
                RunFixedMainLoop,
                (
                    RunFixedMainLoopSystems::BeforeFixedMainLoop,
                    RunFixedMainLoopSystems::FixedMainLoop,
                    RunFixedMainLoopSystems::AfterFixedMainLoop,
                )
                    .chain(),
            );
    }
}
