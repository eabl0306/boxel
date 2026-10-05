//! `LogDiagnosticsPlugin`: logs every `interval` (app clock), board lines in key order, resets the
//! worst frame. Logs are captured with a thread-local subscriber, so `Last` runs single-threaded.
use boxel_app::prelude::*;
use boxel_diagnostic::{DiagnosticsBoard, FrameStats, FrameStatsPlugin, LogDiagnosticsPlugin};
use boxel_ecs::schedule::SingleThreadedExecutor;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// Records the message of every info event.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

struct MessageVisitor(Option<String>);

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

impl Subscriber for Capture {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::INFO
    }
    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}
    fn event(&self, event: &Event<'_>) {
        if *event.metadata().level() != Level::INFO {
            return;
        }
        let mut visitor = MessageVisitor(None);
        event.record(&mut visitor);
        if let Some(message) = visitor.0 {
            self.0.lock().unwrap().push(message);
        }
    }
    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

fn frame_at(app: &mut App, now: Instant) {
    app.world_mut()
        .resource_mut::<Time>()
        .update_with_instant(now);
    app.update();
}

#[test]
fn logs_every_interval_and_resets_the_worst_frame() {
    let t0 = Instant::now();
    let mut app = App::new();
    app.add_plugins((
        FrameStatsPlugin,
        LogDiagnosticsPlugin {
            interval: Duration::from_millis(100),
        },
    ))
    .insert_resource(Time::new(t0))
    .edit_schedule(Last, |s| {
        s.set_executor(SingleThreadedExecutor::new());
    });
    app.finish();
    app.cleanup();
    {
        let mut board = app.world_mut().resource_mut::<DiagnosticsBoard>();
        board.set("world", "chunks=3");
        board.set("gpu", "Vulkan");
    }

    let capture = Capture::default();
    let lines = capture.0.clone();
    tracing::subscriber::with_default(capture, || {
        // Deltas 0, 30, 30: 60 ms on the timer, nothing logged yet.
        for ms in [0u64, 30, 60] {
            frame_at(&mut app, t0 + Duration::from_millis(ms));
        }
        assert!(lines.lock().unwrap().is_empty());
        assert_eq!(
            app.world().resource::<FrameStats>().worst_frame_time,
            Duration::from_millis(30)
        );

        // Delta 50: 110 ms >= 100 ms, one report. avg = 30 * 0.9 + 50 * 0.1 = 32 ms.
        frame_at(&mut app, t0 + Duration::from_millis(110));
        assert_eq!(
            *lines.lock().unwrap(),
            [
                "fps=31 frame=50.00ms avg=32.00ms worst=50.00ms",
                "gpu: Vulkan",
                "world: chunks=3",
            ]
        );
        assert_eq!(
            app.world().resource::<FrameStats>().worst_frame_time,
            Duration::ZERO
        );

        // The timer restarted: 20 + 40 = 60 ms, no new report; worst counts again.
        frame_at(&mut app, t0 + Duration::from_millis(130));
        frame_at(&mut app, t0 + Duration::from_millis(170));
        assert_eq!(lines.lock().unwrap().len(), 3);
        assert_eq!(
            app.world().resource::<FrameStats>().worst_frame_time,
            Duration::from_millis(40)
        );

        // 60 + 40 = 100 ms: second report.
        frame_at(&mut app, t0 + Duration::from_millis(210));
        let lines = lines.lock().unwrap();
        assert_eq!(lines.len(), 6);
        assert!(lines[3].ends_with("worst=40.00ms"), "{}", lines[3]);
    });
}

#[test]
#[should_panic(expected = "LogDiagnosticsPlugin requires FrameStatsPlugin")]
fn log_diagnostics_without_frame_stats_panics_with_a_clear_message() {
    let mut app = App::new();
    app.add_plugins((TimePlugin, LogDiagnosticsPlugin::default()));
    app.finish();
}

#[test]
fn default_interval_is_two_seconds() {
    assert_eq!(
        LogDiagnosticsPlugin::default().interval,
        Duration::from_secs(2)
    );
}
