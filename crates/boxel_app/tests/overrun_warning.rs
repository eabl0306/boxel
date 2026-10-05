//! The text of the fixed-step overrun warning, captured by a global subscriber (own test binary,
//! one test: the global subscriber cannot leak into other tests).
use boxel_app::prelude::*;
use boxel_ecs::prelude::*;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata};

/// Keeps every warning message.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<String>>>);

impl Capture {
    fn warnings(&self) -> Vec<String> {
        self.0.lock().expect("lock").clone()
    }
}

struct Message(String);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        *metadata.level() <= Level::WARN
    }
    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.lock().expect("lock").push(message.0);
    }
    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

#[derive(Resource)]
struct Clock {
    time: Time,
    now: Instant,
    frame: Duration,
}

fn synthetic_time(mut clock: ResMut<Clock>, mut time: ResMut<Time>) {
    let frame = clock.frame;
    clock.now += frame;
    let now = clock.now;
    clock.time.update_with_instant(now);
    *time = clock.time.clone();
}

#[test]
fn the_warning_counts_the_overruns_since_the_previous_one() {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(capture.clone()).expect("first subscriber");

    let start = Instant::now();
    let mut app = App::new();
    app.add_plugins(TimePlugin)
        .insert_resource(Clock {
            time: Time::new(start),
            now: start,
            frame: Duration::from_millis(16),
        })
        .add_systems(First, synthetic_time.after(boxel_app::TimeSystems));
    // Startup: one undeclared 300 ms hitch inside the grace, nothing logged.
    app.update();
    app.world_mut().resource_mut::<Clock>().frame = Duration::from_millis(300);
    app.update();
    app.world_mut().resource_mut::<Clock>().frame = Duration::from_millis(16);
    for _ in 0..20 {
        app.update();
    }
    assert!(capture.warnings().is_empty(), "{:?}", capture.warnings());

    // A known stall declared by the app: nothing either.
    app.world_mut()
        .resource_mut::<Clock>()
        .time
        .declare_stall(Duration::from_millis(1_000));
    app.world_mut().resource_mut::<Clock>().frame = Duration::from_millis(1_016);
    app.update();
    assert!(capture.warnings().is_empty(), "{:?}", capture.warnings());

    // Real overload: 200 ms frames for 6 s of app time.
    app.world_mut().resource_mut::<Clock>().frame = Duration::from_millis(200);
    for _ in 0..30 {
        app.update();
    }
    let warnings = capture.warnings();
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].starts_with("fixed timestep overrun: 1 frame(s) so far ran the maximum of 8"),
        "{}",
        warnings[0]
    );
    // The second one sums the 25 frames in between (rate limit of 5 s, 200 ms frames).
    assert!(
        warnings[1].starts_with("fixed timestep overrun: 25 frame(s) in the last 5.0 s"),
        "{}",
        warnings[1]
    );
    assert!(
        warnings[1].contains("ms of simulated time"),
        "{}",
        warnings[1]
    );
}
