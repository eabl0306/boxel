//! `FrameTimeHistory` (ring buffer), `FrameTimeSummary` (percentiles, 1% low) and how
//! `FrameStatsPlugin` fills the history.
use boxel_app::prelude::*;
use boxel_diagnostic::{
    FrameStats, FrameStatsPlugin, FrameTimeHistory, FrameTimeSummary, percentile,
};
use std::time::{Duration, Instant};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn us(n: u64) -> Duration {
    Duration::from_micros(n)
}

#[test]
fn capacity_is_at_least_1024() {
    assert_eq!(FrameTimeHistory::new(10).capacity(), 1024);
    assert_eq!(FrameTimeHistory::new(5000).capacity(), 5000);
    let default = FrameTimeHistory::default();
    assert_eq!(default.capacity(), FrameTimeHistory::DEFAULT_CAPACITY);
    const { assert!(FrameTimeHistory::DEFAULT_CAPACITY >= FrameTimeHistory::MIN_CAPACITY) };
    assert!(default.is_empty());
    assert_eq!(default.latest(), None);
    assert_eq!(default.summary(), None);
}

#[test]
fn keeps_the_last_frames_in_order() {
    let mut history = FrameTimeHistory::new(1024);
    for i in 1..=3 {
        history.push(us(i));
    }
    assert_eq!(history.len(), 3);
    assert_eq!(history.latest(), Some(us(3)));
    assert_eq!(history.iter().collect::<Vec<_>>(), [us(1), us(2), us(3)]);

    // 1024 + 10 frames in total: the first 10 are gone, order is still oldest to newest.
    for i in 4..=1034 {
        history.push(us(i));
    }
    assert_eq!(history.len(), 1024);
    assert_eq!(history.recorded(), 1034);
    assert_eq!(history.latest(), Some(us(1034)));
    let kept: Vec<_> = history.iter().collect();
    assert_eq!(kept.len(), 1024);
    assert_eq!(kept.first(), Some(&us(11)));
    assert_eq!(kept.last(), Some(&us(1034)));
    assert!(kept.windows(2).all(|w| w[0] < w[1]));

    // Exactly one full wrap more: the newest slot is the last one of the buffer.
    for i in 1035..=2048 {
        history.push(us(i));
    }
    assert_eq!(history.latest(), Some(us(2048)));
    assert_eq!(history.iter().next(), Some(us(1025)));
}

#[test]
fn counts_frames_over_a_budget_and_clears() {
    let mut history = FrameTimeHistory::default();
    for t in [ms(5), ms(7), ms(20), us(6940), us(6941)] {
        history.push(t);
    }
    let budget = us(6940);
    assert_eq!(
        history.frames_over(budget),
        3,
        "strictly slower than the budget"
    );
    assert_eq!(history.frames_over(ms(16)), 1);
    let summary = history.summary().expect("frames");
    assert_eq!(summary.frames, 5);
    assert_eq!(summary.max, ms(20));

    let capacity = history.capacity();
    history.clear();
    assert!(history.is_empty());
    assert_eq!(history.recorded(), 0);
    assert_eq!(history.capacity(), capacity);
    history.push(ms(1));
    assert_eq!(history.iter().collect::<Vec<_>>(), [ms(1)]);
}

#[test]
fn summary_of_1_to_1000_ms() {
    // Reversed on purpose: the input order must not matter.
    let summary = FrameTimeSummary::from_frame_times((1..=1000).rev().map(ms)).expect("frames");
    assert_eq!(summary.frames, 1000);
    assert_eq!(summary.total, ms(500_500));
    assert_eq!(summary.mean, us(500_500));
    assert_eq!(summary.p50, ms(500));
    assert_eq!(summary.p95, ms(950));
    assert_eq!(summary.p99, ms(990));
    assert_eq!(summary.p999, ms(999));
    assert_eq!(summary.min, ms(1));
    assert_eq!(summary.max, ms(1000));
    assert!((summary.avg_fps - 1000.0 / 500.5).abs() < 1e-9);
    // The slowest 1 %: 991..=1000 ms, mean 995.5 ms.
    assert!((summary.low_1_percent_fps - 1.0 / 0.9955).abs() < 1e-9);
}

#[test]
fn one_percent_low_uses_at_least_one_frame() {
    // 150 frames: ceil(1.5) = 2 slowest frames.
    let mut times = vec![ms(2); 148];
    times.extend([ms(10), ms(30)]);
    let summary = FrameTimeSummary::from_frame_times(times).expect("frames");
    assert!((summary.low_1_percent_fps - 1.0 / 0.020).abs() < 1e-9);
    assert_eq!(
        summary.p999,
        ms(30),
        "fewer than 1000 frames: p99.9 is the max"
    );

    // 10 frames: only the slowest one.
    let summary =
        FrameTimeSummary::from_frame_times([ms(4); 9].into_iter().chain([ms(8)])).expect("frames");
    assert!((summary.low_1_percent_fps - 125.0).abs() < 1e-9);
    assert_eq!(summary.p50, ms(4));
    assert_eq!(summary.max, ms(8));
}

#[test]
fn single_frame_and_zero_frames() {
    let one = FrameTimeSummary::from_frame_times([ms(5)]).expect("one frame");
    assert_eq!(
        (one.p50, one.p999, one.min, one.max, one.mean),
        (ms(5), ms(5), ms(5), ms(5), ms(5))
    );
    assert!((one.avg_fps - 200.0).abs() < 1e-9);

    let zeros = FrameTimeSummary::from_frame_times([Duration::ZERO; 3]).expect("frames");
    assert_eq!(zeros.avg_fps, 0.0);
    assert_eq!(zeros.low_1_percent_fps, 0.0);
    assert_eq!(FrameTimeSummary::from_frame_times(std::iter::empty()), None);
    assert_eq!(FrameTimeSummary::from_sorted(&[]), None);
}

#[test]
fn percentile_is_nearest_rank() {
    let sorted: Vec<Duration> = (1..=100).map(ms).collect();
    assert_eq!(percentile(&sorted, 0.0), Some(ms(1)));
    assert_eq!(percentile(&sorted, 1.0), Some(ms(1)));
    assert_eq!(percentile(&sorted, 1.5), Some(ms(2)));
    assert_eq!(percentile(&sorted, 95.0), Some(ms(95)));
    assert_eq!(percentile(&sorted, 99.9), Some(ms(100)));
    assert_eq!(percentile(&sorted, 100.0), Some(ms(100)));
    assert_eq!(percentile(&sorted, -1.0), None);
    assert_eq!(percentile(&sorted, 100.5), None);
    assert_eq!(percentile(&sorted, f64::NAN), None);
    assert_eq!(percentile(&[], 50.0), None);
    // Floating-point noise must not push the rank up (99.9 % of 1000 is rank 999, not 1000).
    let thousand: Vec<Duration> = (1..=1000).map(ms).collect();
    assert_eq!(percentile(&thousand, 99.9), Some(ms(999)));
    assert_eq!(percentile(&thousand, 99.0), Some(ms(990)));
    let big: Vec<Duration> = (1..=1_000_000).map(us).collect();
    assert_eq!(percentile(&big, 99.9), Some(us(999_000)));
    assert_eq!(percentile(&big, 99.99), Some(us(999_900)));
}

/// An app with `FrameStatsPlugin` and a hand-driven clock.
fn manual_app(start: Instant) -> App {
    let mut app = App::new();
    app.add_plugins(FrameStatsPlugin)
        .insert_resource(Time::new(start));
    app.finish();
    app.cleanup();
    app
}

fn frame_at(app: &mut App, now: Instant) {
    app.world_mut()
        .resource_mut::<Time>()
        .update_with_instant(now);
    app.update();
}

#[test]
fn the_plugin_records_real_frame_times() {
    let t0 = Instant::now();
    let mut app = manual_app(t0);
    frame_at(&mut app, t0);
    assert!(
        app.world().resource::<FrameTimeHistory>().is_empty(),
        "the first update has nothing to measure"
    );
    frame_at(&mut app, t0 + ms(10));
    // A 400 ms hitch: `delta` is clamped to 250 ms, the history keeps the real 400 ms.
    frame_at(&mut app, t0 + ms(410));
    // A declared stall is excluded from `delta` but stays in the history.
    app.world_mut()
        .resource_mut::<Time>()
        .declare_stall(ms(100));
    frame_at(&mut app, t0 + ms(530));

    let history = app.world().resource::<FrameTimeHistory>();
    assert_eq!(
        history.iter().collect::<Vec<_>>(),
        [ms(10), ms(400), ms(120)]
    );
    assert_eq!(history.recorded(), 3);
    let stats = app.world().resource::<FrameStats>();
    assert_eq!(stats.frame, 4);
    assert_eq!(
        stats.worst_frame_time,
        ms(250),
        "FrameStats follows the clamped delta"
    );
}

#[test]
fn an_inserted_history_is_kept() {
    let t0 = Instant::now();
    let mut app = App::new();
    app.insert_resource(FrameTimeHistory::new(8192))
        .add_plugins(FrameStatsPlugin)
        .insert_resource(Time::new(t0));
    app.finish();
    app.cleanup();
    frame_at(&mut app, t0);
    frame_at(&mut app, t0 + ms(7));
    let history = app.world().resource::<FrameTimeHistory>();
    assert_eq!(history.capacity(), 8192);
    assert_eq!(history.latest(), Some(ms(7)));
}

#[test]
fn works_with_the_real_clock() {
    let mut app = App::new();
    app.add_plugins((TimePlugin, FrameStatsPlugin));
    app.finish();
    app.cleanup();
    for _ in 0..5 {
        app.update();
    }
    let history = app.world().resource::<FrameTimeHistory>();
    assert_eq!(history.len(), 4);
    let summary = history.summary().expect("4 frames");
    assert!(summary.min <= summary.p50 && summary.p50 <= summary.max);
    assert_eq!(summary.max, history.iter().max().expect("frames"));
}
