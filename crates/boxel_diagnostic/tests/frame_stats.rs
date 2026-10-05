//! `FrameStatsPlugin` and `DiagnosticsBoard`.
use boxel_app::prelude::*;
use boxel_diagnostic::{DiagnosticsBoard, FrameStats, FrameStatsPlugin};
use std::time::{Duration, Instant};

/// An app with `FrameStatsPlugin` and a hand-driven `Time` (no `TimePlugin`, so nothing else
/// touches the clock).
fn app_with_manual_time(start: Instant) -> App {
    let mut app = App::new();
    app.add_plugins(FrameStatsPlugin)
        .insert_resource(Time::new(start));
    app.finish();
    app.cleanup();
    app
}

/// Advances the clock to `now`, then runs one frame.
fn frame_at(app: &mut App, now: Instant) {
    app.world_mut()
        .resource_mut::<Time>()
        .update_with_instant(now);
    app.update();
}

fn stats(app: &App) -> FrameStats {
    app.world().resource::<FrameStats>().clone()
}

fn assert_close(actual: Duration, expected_ms: f64) {
    let actual_ms = actual.as_secs_f64() * 1000.0;
    assert!(
        (actual_ms - expected_ms).abs() < 1e-6,
        "{actual_ms} ms != {expected_ms} ms"
    );
}

#[test]
fn stats_after_three_synthetic_frames() {
    let t0 = Instant::now();
    let mut app = app_with_manual_time(t0);

    // Frame 1: the first Time update always has delta 0.
    frame_at(&mut app, t0);
    let s = stats(&app);
    assert_eq!(s.frame, 1);
    assert_eq!(s.last_frame_time, Duration::ZERO);
    assert_eq!(s.avg_frame_time, Duration::ZERO);
    assert_eq!(s.fps, 0.0);
    assert_eq!(s.worst_frame_time, Duration::ZERO);

    // Frame 2: the first real delta seeds the average.
    frame_at(&mut app, t0 + Duration::from_millis(10));
    let s = stats(&app);
    assert_eq!(s.frame, 2);
    assert_eq!(s.last_frame_time, Duration::from_millis(10));
    assert_close(s.avg_frame_time, 10.0);
    assert!((s.fps - 100.0).abs() < 1e-3, "{}", s.fps);
    assert_eq!(s.worst_frame_time, Duration::from_millis(10));

    // Frame 3: EMA with alpha 0.1.
    frame_at(&mut app, t0 + Duration::from_millis(30));
    let s = stats(&app);
    assert_eq!(s.frame, 3);
    assert_eq!(s.last_frame_time, Duration::from_millis(20));
    assert_close(s.avg_frame_time, 10.0 * 0.9 + 20.0 * 0.1);
    assert!((s.fps - 1000.0 / 11.0).abs() < 1e-3, "{}", s.fps);
    assert_eq!(s.worst_frame_time, Duration::from_millis(20));
}

#[test]
fn first_frame_with_a_real_delta_sets_the_average() {
    let t0 = Instant::now();
    let mut app = app_with_manual_time(t0);
    // Consume the zero-delta first update before the app's first frame.
    app.world_mut()
        .resource_mut::<Time>()
        .update_with_instant(t0);
    for (i, ms) in [10u64, 30, 70].into_iter().enumerate() {
        frame_at(&mut app, t0 + Duration::from_millis(ms));
        assert_eq!(stats(&app).frame, i as u64 + 1);
    }
    let s = stats(&app);
    // Deltas 10, 20, 40: avg 10 -> 11 -> 13.9.
    assert_close(
        s.avg_frame_time,
        (10.0 * 0.9 + 20.0 * 0.1) * 0.9 + 40.0 * 0.1,
    );
    assert_eq!(s.last_frame_time, Duration::from_millis(40));
    assert_eq!(s.worst_frame_time, Duration::from_millis(40));
    assert!((s.fps - 1000.0 / 13.9).abs() < 1e-3, "{}", s.fps);
}

#[test]
fn worst_frame_is_kept_until_reset() {
    let t0 = Instant::now();
    let mut app = app_with_manual_time(t0);
    for ms in [0u64, 5, 45, 50, 60] {
        frame_at(&mut app, t0 + Duration::from_millis(ms));
    }
    assert_eq!(stats(&app).worst_frame_time, Duration::from_millis(40));
    assert_eq!(stats(&app).last_frame_time, Duration::from_millis(10));
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
    let s = stats(&app);
    assert_eq!(s.frame, 5);
    assert_eq!(s.frame, app.world().resource::<Time>().frame_count());
    assert!(s.worst_frame_time >= s.last_frame_time);
    assert!(app.world().contains_resource::<DiagnosticsBoard>());
}

#[test]
#[should_panic(expected = "FrameStatsPlugin requires boxel_app::TimePlugin")]
fn frame_stats_without_time_panics_with_a_clear_message() {
    let mut app = App::new();
    app.add_plugins(FrameStatsPlugin);
    app.finish();
}

#[test]
fn board_lines_are_sorted_and_replaced() {
    let mut board = DiagnosticsBoard::default();
    assert_eq!(board.iter().count(), 0);
    assert_eq!(board.get("gpu"), None);
    board.set("world", "chunks=3");
    board.set("gpu", String::from("Vulkan High"));
    board.set("render", "draws=10");
    assert_eq!(
        board.iter().collect::<Vec<_>>(),
        [
            ("gpu", "Vulkan High"),
            ("render", "draws=10"),
            ("world", "chunks=3")
        ]
    );
    board.set("render", "draws=12");
    assert_eq!(board.get("render"), Some("draws=12"));
    assert_eq!(
        board.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        ["gpu", "render", "world"]
    );
    assert_eq!(board.get("missing"), None);
}
