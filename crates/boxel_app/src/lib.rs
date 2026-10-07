//! `boxel_app`: the application layer. Owns the [`App`] (one ECS `World`), the plugin system,
//! the main schedule order, time and the global task pools. Written for boxel and inspired by
//! `bevy_app` 0.20 (MIT OR Apache-2.0); see NOTICE.
//!
//! Frame structure (frozen): `Main` runs `PreStartup`/`Startup`/`PostStartup` once, then every
//! frame `First`, `PreUpdate`, `RunFixedMainLoop`, `Update`, `PostUpdate`, `Last`, plus any label
//! inserted with [`MainScheduleOrder::insert_after`] (the renderer inserts `Render` after `Last`).
//! `RunFixedMainLoop` is split in the chained [`RunFixedMainLoopSystems`]: before the fixed
//! steps, the steps (`FixedMain` zero or more times, see [`FixedTime`]) and after them.
//! [`Shutdown`] runs once at exit (runners call [`App::shutdown`]).
//!
//! Frame rate: [`FrameLimit`] (default 144 fps, 0 = unlimited) is read by the runners before
//! every frame and can change at any time; [`FramePacer`] is the deadline-based pacing both
//! runners share.

mod app;
mod frame_limit;
mod plugin;
mod plugin_group;
mod runner;
mod schedules;
mod task_pool;
mod time;

pub use app::{App, AppExit, PluginsState, RunnerFn};
pub use frame_limit::{
    DEFAULT_MAX_FPS, FrameDeadline, FrameLimit, FramePacer, FrameWait, spin_until,
};
pub use plugin::{Plugin, Plugins};
pub use plugin_group::{PluginGroup, PluginGroupBuilder};
pub use runner::{RunMode, ScheduleRunnerPlugin};
pub use schedules::{
    First, FixedMain, FixedMainScheduleOrder, FixedPostUpdate, FixedPreUpdate, FixedUpdate, Last,
    Main, MainScheduleOrder, MainSchedulePlugin, PostStartup, PostUpdate, PreStartup, PreUpdate,
    RunFixedMainLoop, RunFixedMainLoopSystems, Shutdown, Startup, Update,
};
pub use task_pool::{TaskPoolOptions, TaskPoolPlugin, TaskPoolThreadAssignmentPolicy};
pub use time::{
    DEFAULT_STARTUP_GRACE_FRAMES, FixedOverruns, FixedTime, OVERRUN_WARNING_INTERVAL, Time,
    TimePlugin, TimeSystems, run_fixed_main_loop, time_system,
};

/// Common app items (re-exported by `boxel::prelude`).
pub mod prelude {
    pub use crate::{
        App, AppExit, First, FixedPostUpdate, FixedTime, FixedUpdate, FrameLimit, Last, Main,
        MainScheduleOrder, Plugin, PluginGroup, PluginGroupBuilder, PostStartup, PostUpdate,
        PreStartup, PreUpdate, RunFixedMainLoop, RunFixedMainLoopSystems, ScheduleRunnerPlugin,
        Shutdown, Startup, TaskPoolOptions, TaskPoolPlugin, Time, TimePlugin, Update,
    };
}
