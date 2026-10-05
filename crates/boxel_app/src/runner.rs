use crate::{App, AppExit, Plugin, PluginsState};
use std::time::{Duration, Instant};

/// How the headless runner drives updates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// One update, then exit.
    Once,
    /// Update until an [`AppExit`] is written; sleep `wait` minus the update time between frames.
    Loop {
        /// Target frame period (`None` = no sleep).
        wait: Option<Duration>,
    },
}

/// Headless runner (servers, tests, CI). Waits for plugins to be ready, calls `finish`/`cleanup`,
/// then updates according to [`RunMode`] and returns the first [`AppExit`].
#[derive(Clone, Copy, Debug)]
pub struct ScheduleRunnerPlugin {
    /// Update policy.
    pub run_mode: RunMode,
}

impl ScheduleRunnerPlugin {
    /// Single update.
    pub fn run_once() -> Self {
        Self {
            run_mode: RunMode::Once,
        }
    }
    /// Loop with a target frame period.
    pub fn run_loop(wait: Duration) -> Self {
        Self {
            run_mode: RunMode::Loop { wait: Some(wait) },
        }
    }
}

impl Plugin for ScheduleRunnerPlugin {
    fn build(&self, app: &mut App) {
        let mode = self.run_mode;
        app.set_runner(move |app| run_headless(app, mode));
    }
}

/// The headless runner installed by [`ScheduleRunnerPlugin`]. Runs `Shutdown` once before
/// returning, in both modes.
fn run_headless(mut app: App, mode: RunMode) -> AppExit {
    while app.plugins_state() == PluginsState::Adding {
        std::thread::yield_now();
    }
    app.finish();
    app.cleanup();
    let exit = match mode {
        RunMode::Once => {
            app.update();
            app.should_exit()
        }
        RunMode::Loop { wait } => loop {
            let start = Instant::now();
            app.update();
            if let Some(exit) = app.should_exit() {
                break Some(exit);
            }
            if let Some(remaining) = wait.and_then(|w| w.checked_sub(start.elapsed()))
                && !remaining.is_zero()
            {
                pace(remaining);
            }
        },
    };
    app.shutdown();
    exit.unwrap_or_default()
}

/// Sleeps between headless frames (the only place the engine sleeps on purpose).
#[expect(clippy::disallowed_methods, reason = "headless runner pacing")]
fn pace(duration: Duration) {
    std::thread::sleep(duration);
}
