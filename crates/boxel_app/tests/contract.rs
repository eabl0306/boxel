//! Contract tests for skeleton-implemented items (frozen behavior).
use boxel_app::prelude::*;
use boxel_ecs::prelude::*;

#[derive(Resource, Default)]
struct Counter(u32);

struct CountPlugin(u32);
impl Plugin for CountPlugin {
    fn build(&self, app: &mut App) {
        let step = self.0;
        app.init_resource::<Counter>()
            .add_systems(Update, move |mut c: ResMut<Counter>| c.0 += step);
    }
}

struct Group;
impl PluginGroup for Group {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start().add(CountPlugin(1))
    }
}

#[test]
fn plugins_groups_and_schedules() {
    let mut app = App::new();
    app.add_plugins((Group.set(CountPlugin(5)), |app: &mut App| {
        app.add_systems(Startup, |mut c: ResMut<Counter>| c.0 += 100);
    }));
    app.update();
    app.update();
    assert_eq!(app.world().resource::<Counter>().0, 110);
    assert_eq!(app.get_added_plugins::<CountPlugin>()[0].0, 5);
}

#[test]
#[should_panic(expected = "already added")]
fn unique_plugins_panic_on_duplicates() {
    App::new()
        .add_plugins(CountPlugin(1))
        .add_plugins(CountPlugin(2));
}

#[test]
fn app_exit_is_observed() {
    let mut app = App::new();
    app.add_systems(Update, |mut w: MessageWriter<AppExit>| {
        w.write(AppExit::from_code(3));
    });
    app.update();
    assert_eq!(app.should_exit(), Some(AppExit::from_code(3)));
}

#[test]
fn should_exit_prefers_the_first_error() {
    let mut app = App::new();
    app.add_systems(Update, |mut w: MessageWriter<AppExit>| {
        w.write(AppExit::from_code(2));
        w.write(AppExit::Success);
        w.write(AppExit::from_code(5));
    });
    app.update();
    assert_eq!(app.should_exit(), Some(AppExit::from_code(2)));
}

struct Late;
impl Plugin for Late {
    fn build(&self, _app: &mut App) {}
}
struct AddsLateInFinish;
impl Plugin for AddsLateInFinish {
    fn build(&self, _app: &mut App) {}
    fn finish(&self, app: &mut App) {
        app.add_plugins(Late);
    }
}

#[test]
fn plugins_added_during_finish_stay_registered() {
    let mut app = App::new();
    app.add_plugins(AddsLateInFinish);
    assert_eq!(app.plugins_state(), boxel_app::PluginsState::Ready);
    app.finish();
    assert!(app.is_plugin_added::<Late>());
    assert!(app.is_plugin_added::<AddsLateInFinish>());
    app.cleanup();
    assert!(app.is_plugin_added::<Late>());
}

#[derive(Resource)]
struct Clock {
    time: Time,
    now: std::time::Instant,
    step: std::time::Duration,
}

/// Replaces the real clock with a synthetic one (runs right after `time_system`).
fn synthetic_time(mut clock: ResMut<Clock>, mut time: ResMut<Time>) {
    let step = clock.step;
    clock.now += step;
    let now = clock.now;
    clock.time.update_with_instant(now);
    *time = clock.time.clone();
}

fn app_with_synthetic_time(frame: std::time::Duration) -> App {
    let start = std::time::Instant::now();
    let mut app = App::new();
    app.add_plugins(TimePlugin)
        .insert_resource(Clock {
            time: Time::new(start),
            now: start,
            step: frame,
        })
        .add_systems(First, synthetic_time.after(boxel_app::TimeSystems));
    app
}

#[derive(Message)]
struct Ping;
#[derive(Resource, Default)]
struct Pings {
    written: u32,
    read_fixed: u32,
}

#[test]
fn messages_written_in_update_reach_fixed_update() {
    // 64 Hz fixed step (15.6 ms) with 5.2 ms frames: one fixed step every 3 frames.
    let mut app = app_with_synthetic_time(std::time::Duration::from_micros(5_200));
    app.add_message::<Ping>()
        .init_resource::<Pings>()
        .add_systems(
            Update,
            |mut w: MessageWriter<Ping>, mut p: ResMut<Pings>| {
                w.write(Ping);
                p.written += 1;
            },
        )
        .add_systems(
            FixedUpdate,
            |mut r: MessageReader<Ping>, mut p: ResMut<Pings>| {
                p.read_fixed += r.read().count() as u32;
            },
        );
    for _ in 0..30 {
        app.update();
    }
    let p = app.world().resource::<Pings>();
    assert_eq!(p.written, 30);
    // At most the messages written since the last fixed step are still pending.
    assert!(
        p.read_fixed >= 27,
        "lost messages: read {} of 30",
        p.read_fixed
    );
}

#[test]
fn a_message_from_the_first_frame_reaches_fixed_update() {
    // The first frame has a zero delta, like paused time: its message must still wait for the
    // first fixed step (frame 5 with 5.2 ms frames and a 15.6 ms step).
    let mut app = app_with_synthetic_time(std::time::Duration::from_micros(5_200));
    app.add_message::<Ping>()
        .init_resource::<Pings>()
        .add_systems(
            Update,
            |mut w: MessageWriter<Ping>, mut p: ResMut<Pings>| {
                if p.written == 0 {
                    w.write(Ping);
                    p.written = 1;
                }
            },
        )
        .add_systems(
            FixedUpdate,
            |mut r: MessageReader<Ping>, mut p: ResMut<Pings>| {
                p.read_fixed += r.read().count() as u32;
            },
        );
    for _ in 0..10 {
        app.update();
    }
    assert_eq!(app.world().resource::<Pings>().read_fixed, 1);
}

#[test]
fn paused_time_still_rotates_messages() {
    let mut app = app_with_synthetic_time(std::time::Duration::from_micros(5_200));
    app.world_mut()
        .resource_mut::<Clock>()
        .time
        .set_max_delta(std::time::Duration::ZERO);
    app.add_message::<Ping>()
        .add_systems(Update, |mut w: MessageWriter<Ping>| {
            w.write(Ping);
        });
    for _ in 0..100 {
        app.update();
    }
    // Double buffer: this frame's message and the previous one, never the whole history.
    let buffered = app.world().resource::<Messages<Ping>>().len();
    assert!(buffered <= 2, "{buffered} messages kept while paused");
}

#[test]
#[should_panic(expected = "must not be zero")]
fn a_zero_timestep_is_rejected() {
    FixedTime::default().set_timestep(std::time::Duration::ZERO);
}

#[test]
fn a_rate_without_a_usable_step_is_rejected() {
    for hz in [0.0, -1.0, f64::INFINITY, f64::NAN, 1e300] {
        let result = std::panic::catch_unwind(|| FixedTime::from_hz(hz));
        assert!(result.is_err(), "from_hz({hz}) accepted");
    }
}

#[test]
fn time_in_fixed_update_is_the_fixed_view() {
    let mut app = app_with_synthetic_time(std::time::Duration::from_millis(50));
    #[derive(Resource, Default)]
    struct Seen(Vec<std::time::Duration>);
    app.init_resource::<Seen>()
        .add_systems(FixedUpdate, |t: Res<Time>, mut s: ResMut<Seen>| {
            s.0.push(t.delta());
        });
    for _ in 0..4 {
        app.update();
    }
    let step = app.world().resource::<FixedTime>().timestep();
    let seen = &app.world().resource::<Seen>().0;
    assert!(!seen.is_empty());
    assert!(seen.iter().all(|d| *d == step));
    // Outside FixedMain the frame view is restored.
    assert_eq!(
        app.world().resource::<Time>().delta(),
        std::time::Duration::from_millis(50)
    );
}

#[test]
fn task_pool_split_counts_the_main_thread() {
    let o = TaskPoolOptions::default();
    assert_eq!(o.split(16), (2, 6, 7));
    assert_eq!(o.split(1), (1, 1, 1));
}

#[test]
fn shutdown_schedule_runs_once() {
    #[derive(Resource, Default)]
    struct Down(u32);
    let mut app = App::new();
    app.init_resource::<Down>()
        .add_systems(Shutdown, |mut d: ResMut<Down>| d.0 += 1);
    app.finish();
    app.cleanup();
    app.update();
    app.shutdown();
    assert_eq!(app.world().resource::<Down>().0, 1);
}

#[test]
fn non_send_resources_are_supported() {
    struct NotSend(std::rc::Rc<u32>);
    let mut app = App::new();
    app.insert_non_send(NotSend(std::rc::Rc::new(7)))
        .add_systems(Update, |r: NonSend<NotSend>| assert_eq!(*r.0, 7));
    app.update();
}
