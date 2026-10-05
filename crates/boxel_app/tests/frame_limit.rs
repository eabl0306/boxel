//! `FrameLimit` and `FramePacer`: the pacing math on a synthetic clock, then the headless runner
//! on the real clock.
use boxel_app::prelude::*;
use boxel_app::{DEFAULT_MAX_FPS, FramePacer, FrameWait, RunMode};
use boxel_ecs::prelude::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MS: Duration = Duration::from_millis(1);
const US: Duration = Duration::from_micros(1);

#[test]
fn frame_limit_defaults_to_144_and_zero_is_unlimited() {
    assert_eq!(DEFAULT_MAX_FPS, 144);
    assert_eq!(FrameLimit::default(), FrameLimit::new(144));
    assert_eq!(
        FrameLimit::default().period(),
        Some(Duration::from_nanos(6_944_444))
    );
    assert_eq!(
        FrameLimit::new(60).period(),
        Some(Duration::from_nanos(16_666_667))
    );
    assert_eq!(FrameLimit::new(1).period(), Some(Duration::from_secs(1)));
    assert!(FrameLimit::UNLIMITED.is_unlimited());
    assert_eq!(FrameLimit::new(0), FrameLimit::UNLIMITED);
    assert_eq!(FrameLimit::UNLIMITED.period(), None);
    assert!(!FrameLimit::new(30).is_unlimited());
    // A period rounds to the nanosecond; past 2e9 fps it would be zero, which is unlimited.
    assert_eq!(FrameLimit::new(u32::MAX).period(), None);
    assert_eq!(
        FrameLimit::new(1_000_000_000).period(),
        Some(Duration::from_nanos(1))
    );
}

#[test]
fn the_first_frame_and_unlimited_frames_are_always_ready() {
    let t0 = Instant::now();
    let mut pacer = FramePacer::new();
    let period = Some(10 * MS);
    assert_eq!(pacer.wait(t0, period), FrameWait::Ready);
    assert_eq!(pacer.next_frame(period), None);
    pacer.start_frame(t0, None);
    assert_eq!(pacer.anchor(), Some(t0));
    assert_eq!(pacer.wait(t0, None), FrameWait::Ready);
    // A limit set after unlimited frames counts from the latest frame.
    assert_eq!(pacer.next_frame(period), Some(t0 + 10 * MS));
    pacer.reset();
    assert_eq!(pacer.wait(t0, period), FrameWait::Ready);
}

#[test]
fn wait_sleeps_then_spins_then_runs() {
    let t0 = Instant::now();
    let margin = 300 * US;
    let mut pacer = FramePacer::with_spin_margin(margin);
    assert_eq!(pacer.spin_margin(), margin);
    let period = Some(10 * MS);
    pacer.start_frame(t0, period);
    let due = t0 + 10 * MS;
    assert_eq!(
        pacer.wait(t0 + 2 * MS, period),
        FrameWait::Sleep { wake: due - margin }
    );
    assert_eq!(
        pacer.wait(due - margin, period),
        FrameWait::Spin { until: due },
        "exactly at the wake instant: spin"
    );
    assert_eq!(pacer.wait(due - US, period), FrameWait::Spin { until: due });
    assert_eq!(pacer.wait(due, period), FrameWait::Ready);
    assert_eq!(pacer.wait(due + MS, period), FrameWait::Ready);
    // No spin margin: sleep right up to the deadline.
    let mut exact = FramePacer::with_spin_margin(Duration::ZERO);
    exact.start_frame(t0, period);
    assert_eq!(exact.wait(t0, period), FrameWait::Sleep { wake: due });
}

#[test]
fn deadlines_accumulate_without_drift() {
    let t0 = Instant::now();
    let period = Some(Duration::from_nanos(6_944_444));
    let mut pacer = FramePacer::new();
    pacer.start_frame(t0, period);
    // Every frame starts up to 0.9 ms late (timer slack, scheduling): the grid does not move.
    for k in 1..=10_000u32 {
        let due = pacer.next_frame(period).unwrap();
        let late = Duration::from_nanos(u64::from(k % 10) * 90_000);
        pacer.start_frame(due + late, period);
        assert_eq!(pacer.anchor(), Some(due));
    }
    assert_eq!(
        pacer.anchor(),
        Some(t0 + Duration::from_nanos(6_944_444) * 10_000),
        "10 000 frames take exactly 10 000 periods"
    );
}

#[test]
fn a_frame_late_by_more_than_one_period_reanchors() {
    let t0 = Instant::now();
    let period = Some(10 * MS);
    let mut pacer = FramePacer::new();
    pacer.start_frame(t0, period);
    // Half a period late: keep the grid, so the next wait is shorter (no lost time).
    pacer.start_frame(t0 + 15 * MS, period);
    assert_eq!(pacer.anchor(), Some(t0 + 10 * MS));
    assert_eq!(pacer.next_frame(period), Some(t0 + 20 * MS));
    // Exactly one period late still keeps the grid.
    pacer.start_frame(t0 + 30 * MS, period);
    assert_eq!(pacer.anchor(), Some(t0 + 20 * MS));
    // A 55 ms hitch: restart the grid at the real start, no burst of catch-up frames.
    let after_hitch = t0 + 85 * MS;
    pacer.start_frame(after_hitch, period);
    assert_eq!(pacer.anchor(), Some(after_hitch));
    assert_eq!(
        pacer.wait(after_hitch + MS, period),
        FrameWait::Sleep {
            wake: after_hitch + 10 * MS - FramePacer::DEFAULT_SPIN_MARGIN
        }
    );
}

#[test]
fn a_frame_started_early_anchors_on_its_start() {
    let t0 = Instant::now();
    let period = Some(10 * MS);
    let mut pacer = FramePacer::new();
    pacer.start_frame(t0, period);
    pacer.start_frame(t0 + 4 * MS, period);
    assert_eq!(pacer.anchor(), Some(t0 + 4 * MS));
}

#[test]
fn reanchor_moves_the_grid_to_an_external_clock() {
    let t0 = Instant::now();
    let period = Some(10 * MS);
    let mut pacer = FramePacer::new();
    pacer.start_frame(t0, period);
    // A runner waited for a frame callback that came 1.5 ms after the deadline.
    pacer.start_frame(t0 + 11_500 * US, period);
    assert_eq!(
        pacer.anchor(),
        Some(t0 + 10 * MS),
        "the grid alone keeps the deadline"
    );
    pacer.reanchor(t0 + 11_500 * US);
    assert_eq!(pacer.next_frame(period), Some(t0 + 21_500 * US));
}

#[test]
fn a_runtime_change_paces_the_next_frame() {
    let t0 = Instant::now();
    let slow = FrameLimit::new(30).period();
    let fast = FrameLimit::new(144).period();
    let mut pacer = FramePacer::new();
    pacer.start_frame(t0, slow);
    assert_eq!(
        pacer.next_frame(slow),
        Some(t0 + Duration::from_nanos(33_333_333))
    );
    // The limit changes during the frame: the next deadline uses the new period at once.
    assert_eq!(
        pacer.next_frame(fast),
        Some(t0 + Duration::from_nanos(6_944_444))
    );
    // Back to unlimited: ready at once.
    assert_eq!(pacer.wait(t0 + MS, None), FrameWait::Ready);
}

/// What [`simulate`] saw.
struct Run {
    /// From the first frame start to the last one.
    span: Duration,
    /// From the first frame start to the last frame's nominal start (its anchor).
    nominal_span: Duration,
    /// Times the loop slept.
    sleeps: u32,
}

/// A loop that follows `wait` on a synthetic clock: sleeps wake up `oversleep` late, spins land
/// exactly, and every frame takes `work`.
fn simulate(fps: u32, frames: u32, work: Duration, oversleep: Duration) -> Run {
    let period = FrameLimit::new(fps).period();
    let mut pacer = FramePacer::new();
    let start = Instant::now();
    let mut now = start;
    let mut sleeps = 0;
    let mut last_start = start;
    for _ in 0..frames {
        loop {
            match pacer.wait(now, period) {
                FrameWait::Ready => break,
                FrameWait::Spin { until } => now = until,
                FrameWait::Sleep { wake } => {
                    now = wake + oversleep;
                    sleeps += 1;
                }
            }
        }
        pacer.start_frame(now, period);
        last_start = now;
        now += work;
    }
    Run {
        span: last_start - start,
        nominal_span: pacer.anchor().unwrap() - start,
        sleeps,
    }
}

#[test]
fn simulated_loops_hold_the_rate() {
    for fps in [30, 60, 144, 240] {
        for oversleep in [Duration::ZERO, 60 * US, 250 * US] {
            let frames = fps * 10;
            let run = simulate(fps, frames + 1, 2 * MS, oversleep);
            let rate = f64::from(frames) / run.span.as_secs_f64();
            assert!(
                (rate - f64::from(fps)).abs() < f64::from(fps) * 1e-6,
                "fps={fps} oversleep={oversleep:?}: {rate}"
            );
            // Spins land on the deadline: every frame starts on time.
            assert_eq!(run.span, run.nominal_span);
            // One sleep per frame: no busy polling of the clock.
            assert_eq!(run.sleeps, frames, "fps={fps} oversleep={oversleep:?}");
        }
    }
    // Oversleeping past the deadline (a coarse timer) makes each frame late, but the grid
    // absorbs it while it stays under one period: the average rate is still exact.
    let run = simulate(144, 1441, 2 * MS, 2 * MS);
    let rate = 1440.0 / run.nominal_span.as_secs_f64();
    assert!((rate - 144.0).abs() < 1e-4, "{rate}");
    assert_eq!(
        run.span - run.nominal_span,
        2 * MS - FramePacer::DEFAULT_SPIN_MARGIN
    );
    // Frames slower than the period: the limit never adds waiting.
    let run = simulate(144, 101, 10 * MS, Duration::ZERO);
    assert_eq!(run.span, 100 * 10 * MS);
    assert_eq!(run.sleeps, 0);
}

/// Timestamps come from an `Update` system, a little after each frame start; the first update
/// also builds the schedules, so intervals are measured from the second one, with this slack.
const SLACK: Duration = Duration::from_millis(1);

/// Records the start of every update and applies `script(update_index)` to the limit.
fn timed_app(
    limit: Option<FrameLimit>,
    frames: usize,
    script: fn(usize) -> Option<u32>,
) -> Vec<Instant> {
    let starts = Arc::new(Mutex::new(Vec::new()));
    let mut app = App::new();
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(50)));
    if let Some(limit) = limit {
        app.insert_resource(limit);
    }
    let seen = Arc::clone(&starts);
    app.add_systems(
        Update,
        move |limit: Option<ResMut<FrameLimit>>, mut exit: MessageWriter<AppExit>| {
            let mut seen = seen.lock().unwrap();
            seen.push(Instant::now());
            if let (Some(mut limit), Some(fps)) = (limit, script(seen.len())) {
                limit.max_fps = fps;
            }
            if seen.len() == frames {
                exit.write(AppExit::Success);
            }
        },
    );
    assert_eq!(app.run(), AppExit::Success);
    Arc::try_unwrap(starts).unwrap().into_inner().unwrap()
}

#[test]
fn the_headless_loop_uses_the_frame_limit_and_follows_runtime_changes() {
    // 100 fps until the 11th update, then 400 fps: lower bounds only (the machine may be
    // loaded).
    let starts = timed_app(Some(FrameLimit::new(100)), 31, |n| (n == 11).then_some(400));
    let first = starts[10] - starts[1];
    let second = starts[30] - starts[10];
    assert!(first + SLACK >= 90 * MS, "{first:?}");
    assert!(second + SLACK >= 50 * MS, "{second:?}");
    // 20 frames take less time than the 9 before them (200 ms if the change was ignored; the
    // 50 ms `wait` of `run_loop` would need 1 s).
    assert!(second < first, "{first:?} {second:?}");
}

#[test]
fn the_headless_loop_falls_back_to_wait_and_zero_is_unlimited() {
    // No FrameLimit: `run_loop(50 ms)` paces frames 50 ms apart.
    let starts = timed_app(None, 4, |_| None);
    assert!(starts[3] - starts[1] + SLACK >= 100 * MS);
    // FrameLimit 0: unlimited, far below the 50 ms wait.
    let starts = timed_app(Some(FrameLimit::UNLIMITED), 20, |_| None);
    assert!(starts[19] - starts[0] < Duration::from_millis(500));
    // `Loop { wait: None }` without a limit is unlimited too.
    let mut app = App::new();
    app.add_plugins(ScheduleRunnerPlugin {
        run_mode: RunMode::Loop {
            wait: Some(Duration::ZERO),
        },
    })
    .add_systems(
        Update,
        |mut exit: MessageWriter<AppExit>, mut n: Local<u32>| {
            *n += 1;
            if *n == 1000 {
                exit.write(AppExit::Success);
            }
        },
    );
    let start = Instant::now();
    assert_eq!(app.run(), AppExit::Success);
    assert!(start.elapsed() < Duration::from_secs(5));
}
