use crate::{App, AppExit, Plugin};
use std::time::Duration;

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

fn run_headless(app: App, mode: RunMode) -> AppExit {
    todo!("WP1")
}
