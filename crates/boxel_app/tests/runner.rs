//! `ScheduleRunnerPlugin` (headless runner): exit codes, `Shutdown`, pacing, idle steps and plugin
//! lifecycle.
use boxel_app::prelude::*;
use boxel_app::{
    FrameDeadline, FramePacer, IDLE_MARGIN, IDLE_STEP, IdleInfo, IdleStep, IdleWanted, IdleWork,
    PluginsState, RunMode,
};
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

/// What an app saw: its updates (with the published deadline and idle info) and its idle steps
/// (with the deadline they ran under), in order.
#[derive(Clone, Copy, Debug)]
enum Seen {
    Update {
        at: Instant,
        deadline: FrameDeadline,
        idle: IdleInfo,
    },
    Step {
        start: Instant,
        until: Instant,
        end: Instant,
        due: Option<Instant>,
    },
}

type Log = Arc<std::sync::Mutex<Vec<Seen>>>;

/// Steps of the current gap.
#[derive(Resource, Default)]
struct GapSteps(u32);

/// An app that logs `frames` updates under `mode` and `limit`, and idle steps that busy-wait
/// `spin` (at most until their end) and answer `answer(step of the gap)`.
fn idle_app(
    mode: RunMode,
    limit: Option<FrameLimit>,
    frames: usize,
    spin: Duration,
    answer: fn(u32) -> IdleWork,
) -> (App, Log) {
    let log: Log = Arc::default();
    let (updates, steps) = (log.clone(), log.clone());
    let mut app = App::new();
    if let Some(limit) = limit {
        app.insert_resource(limit);
    }
    app.add_plugins(ScheduleRunnerPlugin { run_mode: mode })
        .init_resource::<GapSteps>()
        .add_systems(
            Update,
            move |deadline: Option<Res<FrameDeadline>>,
                  idle: Option<Res<IdleInfo>>,
                  mut gap: ResMut<GapSteps>,
                  mut w: MessageWriter<AppExit>| {
                let mut log = updates.lock().expect("lock");
                log.push(Seen::Update {
                    at: Instant::now(),
                    deadline: deadline.as_deref().copied().unwrap_or_default(),
                    idle: idle.as_deref().copied().unwrap_or_default(),
                });
                gap.0 = 0;
                if log
                    .iter()
                    .filter(|s| matches!(s, Seen::Update { .. }))
                    .count()
                    == frames
                {
                    w.write(AppExit::Success);
                }
            },
        )
        .add_systems(
            Idle,
            move |mut step: ResMut<IdleStep>,
                  deadline: Res<FrameDeadline>,
                  mut gap: ResMut<GapSteps>| {
                let start = Instant::now();
                let stop = (start + spin).min(step.until);
                while Instant::now() < stop {
                    std::hint::spin_loop();
                }
                gap.0 += 1;
                step.want(answer(gap.0));
                steps.lock().expect("lock").push(Seen::Step {
                    start,
                    until: step.until,
                    end: Instant::now(),
                    due: deadline.next,
                });
            },
        );
    (app, log)
}

fn take(log: Log) -> Vec<Seen> {
    Arc::try_unwrap(log)
        .expect("app dropped")
        .into_inner()
        .expect("lock")
}

/// The steps between update `i` and the next one, and that next update.
fn gaps(log: &[Seen]) -> Vec<(Seen, Vec<Seen>, Option<Seen>)> {
    let updates: Vec<usize> = (0..log.len())
        .filter(|&i| matches!(log[i], Seen::Update { .. }))
        .collect();
    updates
        .iter()
        .enumerate()
        .map(|(n, &i)| {
            let next = updates.get(n + 1).copied();
            let steps = log[i + 1..next.unwrap_or(log.len())].to_vec();
            (log[i], steps, next.map(|j| log[j]))
        })
        .collect()
}

#[test]
fn run_loop_runs_idle_steps_between_updates_before_the_deadline() {
    let period = Duration::from_millis(10);
    let spin = Duration::from_micros(400);
    // Waiting 5 times per gap, then done.
    let answer = |n: u32| {
        if n <= 5 {
            IdleWork::Waiting
        } else {
            IdleWork::Done
        }
    };
    let (mut app, log) = idle_app(
        RunMode::Loop { wait: None },
        Some(FrameLimit::new(100)),
        20,
        spin,
        answer,
    );
    assert_eq!(app.run(), AppExit::Success);
    let log = take(log);
    assert!(
        matches!(log[0], Seen::Update { .. }),
        "no step before the first update"
    );
    let gaps = gaps(&log);
    assert_eq!(gaps.len(), 20);
    // What every gap holds whatever the OS does with the thread. The exact counts only hold in
    // a gap it left alone (a thread off the CPU for a few ms loses steps, and an update late by
    // more than a period restarts the grid), so they are checked in at least one.
    let mut clean = 0;
    for (k, (update, steps, next)) in gaps.iter().enumerate() {
        let Seen::Update { at, deadline, .. } = *update else {
            unreachable!()
        };
        let due = deadline.next.expect("paced");
        let Some(next) = next else {
            assert!(steps.is_empty(), "no idle after the exit");
            continue;
        };
        assert!(steps.len() <= 6, "frame {k}: done after 5 polls");
        let end = due - FramePacer::DEFAULT_SPIN_MARGIN - IDLE_MARGIN;
        let mut busy = Duration::ZERO;
        for step in steps {
            let Seen::Step {
                start,
                until,
                end: ended,
                due: step_due,
            } = *step
            else {
                unreachable!()
            };
            assert!(start > at, "frame {k}: after the update");
            assert_eq!(step_due, Some(due), "frame {k}");
            assert!(until <= end, "frame {k}: {:?} past the slot", until - end);
            assert!(until - start <= IDLE_STEP, "frame {k}");
            busy += ended - start;
        }
        let Seen::Update {
            at: next_at,
            deadline: next_deadline,
            idle,
        } = *next
        else {
            unreachable!()
        };
        let next_due = next_deadline.next.expect("paced");
        assert!(
            next_at >= due,
            "frame {k}: the next update waits for its deadline"
        );
        assert!(
            next_due >= due + period,
            "frame {k}: never ahead of the grid"
        );
        // The next update reports this gap.
        assert_eq!(idle.steps as usize, steps.len(), "frame {k}");
        assert!(
            idle.tail >= busy && idle.tail <= next_at - at,
            "frame {k}: {idle:?}"
        );
        // Left alone: the 5 polls and the done step, the grid kept (idle steps never delay an
        // update), and the steps not counted in the update's own time.
        clean += usize::from(
            steps.len() == 6 && next_due == due + period && next_deadline.last_update < idle.tail,
        );
    }
    assert!(clean > 0, "no gap ran its 6 steps on the grid");
}

#[test]
fn ready_steps_run_back_to_back_until_the_slot_ends() {
    let (mut app, log) = idle_app(
        RunMode::Loop {
            wait: Some(Duration::from_millis(5)),
        },
        None,
        20,
        Duration::ZERO,
        |_| IdleWork::Ready,
    );
    assert_eq!(app.run(), AppExit::Success);
    let log = take(log);
    // Every step inside its slot; several steps, and one that ends with the slot, in at least
    // one gap (the OS can take the thread off the CPU for a whole gap).
    let (mut several, mut reached) = (0, 0);
    for (update, steps, next) in gaps(&log) {
        let Seen::Update { deadline, .. } = update else {
            unreachable!()
        };
        if next.is_none() {
            continue;
        }
        let end = deadline.next.expect("paced") - FramePacer::DEFAULT_SPIN_MARGIN - IDLE_MARGIN;
        for step in &steps {
            let Seen::Step { start, until, .. } = *step else {
                unreachable!()
            };
            assert!(until <= end && until - start <= IDLE_STEP, "{step:?}");
            reached += usize::from(until == end);
        }
        several += usize::from(steps.len() > 1);
    }
    assert!(several > 0, "Ready runs the next step at once");
    assert!(reached > 0, "the steps go on until the slot ends");
}

#[test]
fn a_done_step_closes_the_slot_until_the_next_update() {
    let (mut app, log) = idle_app(
        RunMode::Loop {
            wait: Some(Duration::from_millis(5)),
        },
        None,
        20,
        Duration::ZERO,
        |_| IdleWork::Done,
    );
    assert_eq!(app.run(), AppExit::Success);
    let log = take(log);
    let mut ran = 0;
    for (k, (_, steps, next)) in gaps(&log).iter().enumerate() {
        let Some(Seen::Update { idle, .. }) = next else {
            assert!(steps.is_empty(), "no idle after the exit");
            continue;
        };
        // At most one (none if the OS kept the thread off the CPU past the slot).
        assert!(steps.len() <= 1, "gap {k}: {steps:?}");
        assert_eq!(idle.steps as usize, steps.len(), "gap {k}");
        ran += steps.len();
    }
    assert!(ran > 0, "no gap ran its step");
}

#[test]
fn idle_steps_follow_only_the_updates_that_asked() {
    // Updates that found an ask left from the previous one.
    let stale = Arc::new(AtomicU32::new(0));
    let seen = stale.clone();
    let (mut app, log) = idle_app(
        RunMode::Loop {
            wait: Some(Duration::from_millis(5)),
        },
        None,
        20,
        Duration::ZERO,
        |_| IdleWork::Done,
    );
    app.init_resource::<IdleWanted>().add_systems(
        Update,
        move |mut updates: Local<u32>, mut wanted: ResMut<IdleWanted>| {
            *updates += 1;
            seen.fetch_add(u32::from(wanted.wanted()), Ordering::Relaxed);
            // Odd updates ask; a later answer of the same update never takes it back.
            wanted.want(*updates % 2 == 1);
            wanted.want(false);
        },
    );
    assert_eq!(app.run(), AppExit::Success);
    assert_eq!(
        stale.load(Ordering::Relaxed),
        0,
        "cleared before each update"
    );
    let log = take(log);
    let mut ran = 0;
    for (k, (_, steps, next)) in gaps(&log).iter().enumerate() {
        let Some(Seen::Update { idle, .. }) = next else {
            continue;
        };
        assert_eq!(idle.steps as usize, steps.len(), "gap {k}");
        if k % 2 == 0 {
            // At most one (none if the OS kept the thread off the CPU past the slot).
            assert!(steps.len() <= 1, "gap {k}: {steps:?}");
            ran += steps.len();
        } else {
            assert!(steps.is_empty(), "gap {k}: no ask, no slot: {steps:?}");
        }
    }
    assert!(ran > 0, "no gap that asked ran its step");
}

#[test]
fn run_loop_without_a_limit_or_once_never_idles() {
    let cases = [
        (RunMode::Loop { wait: None }, None),
        (
            RunMode::Loop {
                wait: Some(Duration::ZERO),
            },
            None,
        ),
        // The limit replaces `wait`: unlimited.
        (
            RunMode::Loop {
                wait: Some(Duration::from_millis(5)),
            },
            Some(FrameLimit::UNLIMITED),
        ),
        (RunMode::Once, Some(FrameLimit::new(100))),
    ];
    for (mode, limit) in cases {
        let (mut app, log) = idle_app(mode, limit, 5, Duration::ZERO, |_| IdleWork::Ready);
        assert_eq!(app.run(), AppExit::Success, "{mode:?}");
        let log = take(log);
        assert!(
            log.iter().all(|s| matches!(s, Seen::Update { .. })),
            "{mode:?} {limit:?}: {log:?}"
        );
    }
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
