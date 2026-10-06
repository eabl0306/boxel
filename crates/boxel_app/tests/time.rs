//! Declared stalls (`Time::declare_stall`), real vs simulation deltas and fixed-step overrun
//! accounting, all on a synthetic clock.
use boxel_app::prelude::*;
use boxel_app::{DEFAULT_STARTUP_GRACE_FRAMES, FixedOverruns, OVERRUN_WARNING_INTERVAL};
use boxel_ecs::prelude::*;
use std::time::{Duration, Instant};

const MS: Duration = Duration::from_millis(1);

// ---------------------------------------------------------------- Time alone

#[test]
fn a_declared_stall_is_excluded_from_delta_but_not_from_real_delta() {
    let t0 = Instant::now();
    let mut time = Time::new(t0);
    time.update_with_instant(t0);
    assert_eq!(
        (time.delta(), time.real_delta(), time.stall()),
        (Duration::ZERO, Duration::ZERO, Duration::ZERO)
    );

    // A 180 ms GPU bring-up inside a 190 ms frame.
    time.declare_stall(MS * 180);
    time.update_with_instant(t0 + MS * 190);
    assert_eq!(time.delta(), MS * 10);
    assert_eq!(time.real_delta(), MS * 190);
    assert_eq!(time.stall(), MS * 180);
    assert_eq!(time.elapsed(), MS * 10);
    assert_eq!(time.last_update(), Some(t0 + MS * 190));

    // The declaration is consumed: the next frame is normal again.
    time.update_with_instant(t0 + MS * 200);
    assert_eq!(
        (time.delta(), time.real_delta(), time.stall()),
        (MS * 10, MS * 10, Duration::ZERO)
    );
    assert_eq!(time.elapsed(), MS * 20);
    assert_eq!(time.frame_count(), 3);
}

#[test]
fn stalls_add_up_and_never_remove_more_than_the_interval() {
    let t0 = Instant::now();
    let mut time = Time::new(t0);
    time.update_with_instant(t0);
    time.declare_stall(MS * 30);
    time.declare_stall(MS * 40);
    time.update_with_instant(t0 + MS * 100);
    assert_eq!((time.delta(), time.stall()), (MS * 30, MS * 70));

    // Overlapping declarations (runner gap + renderer) larger than the frame: delta 0.
    time.declare_stall(MS * 500);
    time.update_with_instant(t0 + MS * 150);
    assert_eq!(
        (time.delta(), time.real_delta(), time.stall()),
        (Duration::ZERO, MS * 50, MS * 50)
    );
}

#[test]
fn the_clamp_applies_after_the_stall_and_real_delta_is_never_clamped() {
    let t0 = Instant::now();
    let mut time = Time::new(t0);
    time.update_with_instant(t0);
    time.declare_stall(MS * 100);
    time.update_with_instant(t0 + MS * 1_000);
    assert_eq!(time.delta(), time.max_delta(), "900 ms clamped to 250 ms");
    assert_eq!(time.real_delta(), MS * 1_000);

    time.update_with_instant(t0 + MS * 3_000);
    assert_eq!(time.delta(), MS * 250);
    assert_eq!(
        time.real_delta(),
        MS * 2_000,
        "frame statistics see the whole hitch"
    );
}

#[test]
fn a_stall_declared_before_the_first_update_is_dropped() {
    let t0 = Instant::now();
    let mut time = Time::new(t0);
    time.declare_stall(MS * 50);
    time.update_with_instant(t0 + MS * 60);
    assert_eq!(
        time.delta(),
        Duration::ZERO,
        "the first delta is always zero"
    );
    time.update_with_instant(t0 + MS * 76);
    assert_eq!((time.delta(), time.stall()), (MS * 16, Duration::ZERO));
}

// ---------------------------------------------------------------- the fixed loop

#[derive(Resource)]
struct Clock {
    time: Time,
    now: Instant,
    /// Frame lengths still to play (the last one repeats).
    frames: Vec<Duration>,
}

#[derive(Resource, Default)]
struct Steps(u32);

/// Replaces the real clock with the synthetic one (runs right after `time_system`).
fn synthetic_time(mut clock: ResMut<Clock>, mut time: ResMut<Time>) {
    let step = if clock.frames.len() > 1 {
        clock.frames.remove(0)
    } else {
        clock.frames[0]
    };
    clock.now += step;
    let now = clock.now;
    clock.time.update_with_instant(now);
    *time = clock.time.clone();
}

/// `TimePlugin` on a synthetic clock. The first update has delta 0, then each update advances
/// by the next entry of `frames` (the last entry repeats).
fn app_with_frames(frames: &[Duration]) -> App {
    let start = Instant::now();
    let mut app = App::new();
    app.add_plugins(TimePlugin)
        .init_resource::<Steps>()
        .insert_resource(Clock {
            time: Time::new(start),
            now: start,
            // The first update consumes this entry but has delta 0 anyway.
            frames: [&[Duration::ZERO][..], frames].concat(),
        })
        .add_systems(First, synthetic_time.after(boxel_app::TimeSystems))
        .add_systems(FixedUpdate, |mut steps: ResMut<Steps>| steps.0 += 1);
    app
}

fn overruns(app: &App) -> FixedOverruns {
    app.world().resource::<FixedTime>().overruns()
}

fn steps(app: &App) -> u32 {
    app.world().resource::<Steps>().0
}

#[test]
fn a_declared_bring_up_stall_runs_no_burst_of_fixed_steps() {
    // Frame 1 selects the GPU (300 ms) and declares it, like boxel_render does.
    let mut app = app_with_frames(&[MS * 305, MS * 5]);
    app.update();
    app.world_mut()
        .resource_mut::<Clock>()
        .time
        .declare_stall(MS * 300);
    app.update();
    let time = app.world().resource::<Time>();
    assert_eq!((time.delta(), time.real_delta()), (MS * 5, MS * 305));
    assert_eq!(steps(&app), 0, "5 ms of simulation time: no fixed step yet");
    assert_eq!(app.world().resource::<FixedTime>().steps_last_frame(), 0);
    assert_eq!(overruns(&app), FixedOverruns::default());
}

#[test]
fn an_undeclared_startup_stall_is_dropped_quietly() {
    // The same 305 ms frame without a declaration: 250 ms (clamped) reach the accumulator.
    let mut app = app_with_frames(&[MS * 305, MS * 5]);
    app.update();
    app.update();
    assert_eq!(steps(&app), 8, "the step limit");
    let fixed = app.world().resource::<FixedTime>();
    assert_eq!(fixed.steps_last_frame(), 8);
    assert!(
        fixed.accumulator() < fixed.timestep(),
        "whole steps dropped"
    );
    assert_eq!(
        overruns(&app),
        FixedOverruns {
            startup_frames: 1,
            ..FixedOverruns::default()
        },
        "inside the startup grace: counted apart, no warning"
    );
}

#[test]
fn repeated_overruns_after_startup_warn_at_a_limited_rate() {
    // 200 ms frames on a 64 Hz step: 12 steps due per frame, 8 run, 4 dropped.
    let mut app = app_with_frames(&[MS * 200]);
    app.world_mut()
        .resource_mut::<FixedTime>()
        .set_startup_grace_frames(2);
    for _ in 0..30 {
        app.update();
    }
    let o = overruns(&app);
    // Frames 1 (delta 0) and 2 are not overruns or are in the grace; frames 3..=30 overrun.
    assert_eq!(o.startup_frames, 1);
    assert_eq!(o.frames, 28);
    assert!(o.dropped_steps >= 28 * 3, "{o:?}");
    // First warning at frame 3 (t = 0.4 s), the next one 5 s later (frame 28, t = 5.4 s).
    assert_eq!(OVERRUN_WARNING_INTERVAL, Duration::from_secs(5));
    assert_eq!(o.warnings, 2, "{o:?}");
}

#[test]
fn overruns_held_back_by_the_rate_limit_are_reported_later() {
    // Two overrun frames close together, then normal frames: the second one is reported once
    // the interval has passed, even though nothing overruns any more.
    let mut frames = vec![MS * 16; 11];
    frames.extend([MS * 300, MS * 300]);
    frames.push(MS * 100);
    let mut app = app_with_frames(&frames);
    for _ in 0..14 {
        app.update();
    }
    let o = overruns(&app);
    assert_eq!((o.frames, o.warnings), (2, 1), "{o:?}");
    // 100 ms frames: 6 steps each, no overrun. 5 s later the held-back frame is reported.
    for _ in 0..50 {
        app.update();
    }
    let o = overruns(&app);
    assert_eq!((o.frames, o.warnings), (2, 2), "{o:?}");
    for _ in 0..100 {
        app.update();
    }
    assert_eq!(overruns(&app).warnings, 2, "nothing new to report");
}

#[test]
fn defaults() {
    let fixed = FixedTime::default();
    assert_eq!(fixed.max_steps_per_frame(), 8);
    assert_eq!(fixed.startup_grace_frames(), DEFAULT_STARTUP_GRACE_FRAMES);
    assert_eq!(fixed.steps_last_frame(), 0);
    assert_eq!(fixed.overruns(), FixedOverruns::default());
}

#[test]
fn the_step_limit_can_be_lowered() {
    // 200 ms frames on a 64 Hz step: 12 steps due per frame; a limit of 3 runs 3 and drops 9.
    let mut app = app_with_frames(&[MS * 200]);
    app.world_mut()
        .resource_mut::<FixedTime>()
        .set_max_steps_per_frame(3);
    app.update();
    app.update();
    let fixed = app.world().resource::<FixedTime>();
    assert_eq!(fixed.max_steps_per_frame(), 3);
    assert_eq!((steps(&app), fixed.steps_last_frame()), (3, 3));
    assert!(
        fixed.accumulator() < fixed.timestep(),
        "whole steps dropped"
    );
    assert_eq!(
        fixed.overruns().startup_frames,
        1,
        "dropped inside the startup grace"
    );
    // Raising it again takes effect on the next frame.
    app.world_mut()
        .resource_mut::<FixedTime>()
        .set_max_steps_per_frame(20);
    app.update();
    // 200 ms plus the leftover of the previous frame (less than one step): 12 or 13 steps.
    let ran = app.world().resource::<FixedTime>().steps_last_frame();
    assert!((12..=13).contains(&ran), "{ran}");
}

#[test]
#[should_panic(expected = "at least one step per frame")]
fn a_step_limit_of_zero_is_rejected() {
    FixedTime::default().set_max_steps_per_frame(0);
}

/// What ran, in order.
#[derive(Resource, Default)]
struct Log(Vec<&'static str>);

#[test]
fn before_and_after_fixed_main_loop_wrap_the_steps_of_each_frame() {
    use boxel_app::{RunFixedMainLoop, RunFixedMainLoopSystems};
    // 40 ms frames on a 15.625 ms step: 2, 3, 2, 3... steps.
    let mut app = app_with_frames(&[MS * 40]);
    app.init_resource::<Log>()
        .add_systems(
            RunFixedMainLoop,
            (
                (|mut log: ResMut<Log>| log.0.push("before"))
                    .in_set(RunFixedMainLoopSystems::BeforeFixedMainLoop),
                (|mut log: ResMut<Log>, time: Res<Time>, fixed: Res<FixedTime>| {
                    // The frame view of Time is back; the leftover is less than one step.
                    assert!(time.delta() == Duration::ZERO || time.delta() == MS * 40);
                    assert!(fixed.overstep_fraction() < 1.0);
                    log.0.push("after");
                })
                .in_set(RunFixedMainLoopSystems::AfterFixedMainLoop),
            ),
        )
        .add_systems(FixedUpdate, |mut log: ResMut<Log>| log.0.push("fixed"));
    app.update();
    assert_eq!(
        std::mem::take(&mut app.world_mut().resource_mut::<Log>().0),
        ["before", "after"],
        "the first frame has delta 0: no step, but both sides run"
    );
    app.update();
    assert_eq!(
        std::mem::take(&mut app.world_mut().resource_mut::<Log>().0),
        ["before", "fixed", "fixed", "after"]
    );
    app.update();
    assert_eq!(
        std::mem::take(&mut app.world_mut().resource_mut::<Log>().0),
        ["before", "fixed", "fixed", "fixed", "after"]
    );
}
