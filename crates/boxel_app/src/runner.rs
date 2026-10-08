use crate::frame_limit::sleep_until;
use crate::{
    App, AppExit, FrameDeadline, FrameLimit, FramePacer, IDLE_POLL, IdleInfo, IdleWork, Plugin,
    PluginsState, idle_step_end,
};
use boxel_ecs::world::World;
use std::time::{Duration, Instant};

/// How the headless runner drives updates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    /// One update, then exit.
    Once,
    /// Update until an [`AppExit`] is written, starting one frame every `wait` (deadline based,
    /// see [`FramePacer`]). A [`FrameLimit`] resource, when the app has one, replaces `wait`
    /// and is read before every frame, so it can change at runtime. While paced, an app with
    /// [`Idle`](crate::Idle) systems runs idle steps between updates ([`App::run_idle`]).
    Loop {
        /// Target frame period (`None` or zero = no pacing).
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
/// returning, in both modes. In `Loop` mode it writes [`FrameDeadline`] and [`IdleInfo`] before
/// every update, and, while paced, runs idle steps after each update until the idle slot of the
/// next deadline ends ([`FramePacer::idle_end`]).
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
        RunMode::Loop { wait } => {
            let mut pacer = FramePacer::new();
            let mut last_update = Duration::ZERO;
            let mut idle = IdleInfo::default();
            loop {
                let period = loop_period(app.world(), wait);
                pacer.block_until_due(period);
                let start = Instant::now();
                pacer.start_frame(start, period);
                let world = app.world_mut();
                world.insert_resource(FrameDeadline::after_start(&pacer, period, last_update));
                world.insert_resource(idle.take_gap());
                app.update();
                last_update = start.elapsed();
                if let Some(exit) = app.should_exit() {
                    break Some(exit);
                }
                let period = loop_period(app.world(), wait);
                if let Some(end) = pacer.idle_end(period)
                    && app.has_idle_systems()
                {
                    run_idle_slot(&mut app, end, &mut idle);
                }
            }
        }
    };
    app.shutdown();
    exit.unwrap_or_default()
}

/// Idle steps until the slot ends at `end` or the `Idle` systems are done: the next step at once
/// after [`IdleWork::Ready`], after a short sleep ([`IDLE_POLL`]) after [`IdleWork::Waiting`].
fn run_idle_slot(app: &mut App, end: Instant, info: &mut IdleInfo) {
    loop {
        let now = Instant::now();
        let Some(until) = idle_step_end(now, end) else {
            return;
        };
        let work = app.run_idle(until);
        let ended = Instant::now();
        info.record(now, ended, end);
        match work {
            IdleWork::Done => return,
            IdleWork::Ready => {}
            IdleWork::Waiting => {
                sleep_until(
                    ended
                        .checked_add(IDLE_POLL)
                        .map_or(end, |poll| poll.min(end)),
                );
            }
        }
    }
}

/// The frame period of the headless loop: the app's [`FrameLimit`] if it has one, else `wait`
/// (`None` = unlimited).
fn loop_period(world: &World, wait: Option<Duration>) -> Option<Duration> {
    match world.get_resource::<FrameLimit>() {
        Some(limit) => limit.period(),
        None => wait.filter(|w| !w.is_zero()),
    }
}
