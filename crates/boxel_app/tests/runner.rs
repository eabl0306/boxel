//! `ScheduleRunnerPlugin` (headless runner): exit codes, `Shutdown`, pacing and plugin lifecycle.
use boxel_app::prelude::*;
use boxel_app::{FrameDeadline, PluginsState, RunMode};
use boxel_ecs::prelude::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// Shared counters (the app is moved into the runner, so tests observe it through these).
#[derive(Clone, Default)]
struct Counters {
    startups: Arc<AtomicU32>,
    updates: Arc<AtomicU32>,
    shutdowns: Arc<AtomicU32>,
}

impl Counters {
    fn get(c: &AtomicU32) -> u32 {
        c.load(Ordering::SeqCst)
    }
}

/// An app that counts startups, updates and shutdowns, and writes `exit` on update `exit_on`.
fn counting_app(counters: &Counters, exit_on: u32, exit: AppExit) -> App {
    let mut app = App::new();
    let startups = counters.startups.clone();
    let updates = counters.updates.clone();
    let shutdowns = counters.shutdowns.clone();
    app.add_systems(Startup, move || {
        startups.fetch_add(1, Ordering::SeqCst);
    })
    .add_systems(Update, move |mut w: MessageWriter<AppExit>| {
        let n = updates.fetch_add(1, Ordering::SeqCst) + 1;
        if n == exit_on {
            w.write(exit.clone());
        }
    })
    .add_systems(Shutdown, move || {
        shutdowns.fetch_add(1, Ordering::SeqCst);
    });
    app
}

#[test]
fn run_once_returns_the_written_exit_and_runs_shutdown_once() {
    let c = Counters::default();
    let mut app = counting_app(&c, 1, AppExit::from_code(4));
    app.add_plugins(ScheduleRunnerPlugin::run_once());
    assert_eq!(app.run(), AppExit::from_code(4));
    assert_eq!(Counters::get(&c.startups), 1);
    assert_eq!(Counters::get(&c.updates), 1);
    assert_eq!(Counters::get(&c.shutdowns), 1);
}

#[test]
fn run_once_without_an_exit_is_success() {
    let c = Counters::default();
    let mut app = counting_app(&c, u32::MAX, AppExit::error());
    app.add_plugins(ScheduleRunnerPlugin::run_once());
    assert_eq!(app.run(), AppExit::Success);
    assert_eq!(Counters::get(&c.updates), 1);
    assert_eq!(Counters::get(&c.shutdowns), 1);
}

#[test]
fn run_loop_stops_on_the_frame_that_writes_the_exit() {
    let c = Counters::default();
    let mut app = counting_app(&c, 5, AppExit::from_code(3));
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(1)));
    assert_eq!(app.run(), AppExit::from_code(3));
    assert_eq!(Counters::get(&c.startups), 1);
    assert_eq!(Counters::get(&c.updates), 5);
    assert_eq!(Counters::get(&c.shutdowns), 1);
}

#[test]
fn run_loop_returns_success_and_supports_no_wait() {
    let c = Counters::default();
    let mut app = counting_app(&c, 5, AppExit::Success);
    app.add_plugins(ScheduleRunnerPlugin {
        run_mode: RunMode::Loop { wait: None },
    });
    assert_eq!(app.run(), AppExit::Success);
    assert_eq!(Counters::get(&c.updates), 5);
    assert_eq!(Counters::get(&c.shutdowns), 1);
}

#[test]
fn run_loop_paces_frames() {
    let c = Counters::default();
    let mut app = counting_app(&c, 5, AppExit::Success);
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(10)));
    let start = Instant::now();
    assert_eq!(app.run(), AppExit::Success);
    // Four sleeps between five updates (the exit frame does not sleep). Lower bound only: the
    // machine may be loaded.
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_millis(40), "{elapsed:?}");
}

#[test]
fn run_loop_writes_the_frame_deadline() {
    let run = |mode: RunMode| {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = seen.clone();
        let mut app = App::new();
        app.add_plugins(ScheduleRunnerPlugin { run_mode: mode })
            .add_systems(
                Update,
                move |deadline: Res<FrameDeadline>, mut w: MessageWriter<AppExit>| {
                    let mut seen = record.lock().expect("lock");
                    seen.push((Instant::now(), *deadline));
                    if seen.len() == 3 {
                        w.write(AppExit::Success);
                    }
                },
            );
        assert_eq!(app.run(), AppExit::Success);
        Arc::try_unwrap(seen)
            .expect("app dropped")
            .into_inner()
            .expect("lock")
    };
    let period = Duration::from_millis(10);
    let seen = run(RunMode::Loop { wait: Some(period) });
    for (i, (at, deadline)) in seen.iter().enumerate() {
        assert_eq!(deadline.period, Some(period), "frame {i}");
        let next = deadline.next.expect("paced");
        assert!(
            next > *at && next - *at <= period,
            "frame {i}: {deadline:?}"
        );
        // The first frame has no previous update.
        assert_eq!(deadline.last_update.is_zero(), i == 0, "frame {i}");
    }
    // Unpaced: no deadline.
    let seen = run(RunMode::Loop { wait: None });
    assert!(
        seen.iter()
            .all(|(_, d)| d.next.is_none() && d.period.is_none())
    );
}

/// Ready only after `polls` calls to `ready`; records the update count seen by `finish` and
/// `cleanup` (`u32::MAX` = never called).
struct SlowPlugin {
    polls: AtomicU32,
    ready_after: u32,
    updates: Arc<AtomicU32>,
    finished_at: Arc<AtomicU32>,
    cleaned_at: Arc<AtomicU32>,
}

impl Plugin for SlowPlugin {
    fn build(&self, _app: &mut App) {}
    fn ready(&self, _app: &App) -> bool {
        self.polls.fetch_add(1, Ordering::SeqCst) + 1 >= self.ready_after
    }
    fn finish(&self, _app: &mut App) {
        let seen = self.updates.load(Ordering::SeqCst);
        self.finished_at.store(seen, Ordering::SeqCst);
    }
    fn cleanup(&self, _app: &mut App) {
        let seen = self.updates.load(Ordering::SeqCst);
        self.cleaned_at.store(seen, Ordering::SeqCst);
    }
}

#[test]
fn runner_waits_for_ready_plugins_then_finishes_and_cleans_up_before_updating() {
    for mode in [RunMode::Once, RunMode::Loop { wait: None }] {
        let c = Counters::default();
        let finished_at = Arc::new(AtomicU32::new(u32::MAX));
        let cleaned_at = Arc::new(AtomicU32::new(u32::MAX));
        let mut app = counting_app(&c, 1, AppExit::Success);
        app.add_plugins((
            SlowPlugin {
                polls: AtomicU32::new(0),
                ready_after: 3,
                updates: c.updates.clone(),
                finished_at: finished_at.clone(),
                cleaned_at: cleaned_at.clone(),
            },
            ScheduleRunnerPlugin { run_mode: mode },
        ));
        assert_eq!(app.plugins_state(), PluginsState::Adding, "{mode:?}");
        assert_eq!(app.run(), AppExit::Success, "{mode:?}");
        assert_eq!(finished_at.load(Ordering::SeqCst), 0, "{mode:?}");
        assert_eq!(cleaned_at.load(Ordering::SeqCst), 0, "{mode:?}");
        assert_eq!(Counters::get(&c.updates), 1, "{mode:?}");
        assert_eq!(Counters::get(&c.shutdowns), 1, "{mode:?}");
    }
}

#[test]
fn the_first_error_wins_in_the_loop() {
    let mut app = App::new();
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::ZERO))
        .add_systems(Update, |mut w: MessageWriter<AppExit>| {
            w.write(AppExit::Success);
            w.write(AppExit::from_code(7));
            w.write(AppExit::from_code(9));
        });
    assert_eq!(app.run(), AppExit::from_code(7));
}
