//! Work between frames (ADR 0022): the idle slot and step math on a synthetic clock, and
//! `App::run_idle`.
use boxel_app::prelude::*;
use boxel_app::{
    FramePacer, IDLE_MARGIN, IDLE_STEP, IdleInfo, IdleStep, IdleWork, MIN_IDLE_STEP, idle_step_end,
};
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::ScheduleLabel;
use std::time::{Duration, Instant};

const US: Duration = Duration::from_micros(1);

#[test]
fn idle_end_is_due_minus_spin_and_margin() {
    let t0 = Instant::now();
    let pacer = FramePacer::new();
    let period = FrameLimit::new(144).period();
    // No frame yet, or unlimited: no slot.
    assert_eq!(pacer.idle_end(period), None);
    let mut pacer = FramePacer::new();
    pacer.start_frame(t0, period);
    assert_eq!(pacer.idle_end(None), None);
    let spin = FramePacer::DEFAULT_SPIN_MARGIN;
    for fps in [144, 60, 30] {
        let period = FrameLimit::new(fps).period();
        let due = pacer.next_frame(period).expect("paced");
        assert_eq!(
            pacer.idle_end(period),
            Some(due - spin - IDLE_MARGIN),
            "{fps} fps"
        );
    }
    // 1 ms before the deadline at the default spin margin.
    assert_eq!(spin + IDLE_MARGIN, Duration::from_millis(1));
    // Without a spin margin, only the idle margin.
    let mut exact = FramePacer::with_spin_margin(Duration::ZERO);
    exact.start_frame(t0, period);
    let due = exact.next_frame(period).expect("paced");
    assert_eq!(exact.idle_end(period), Some(due - IDLE_MARGIN));
}

#[test]
fn idle_step_end_is_short_and_never_past_the_end() {
    let t0 = Instant::now();
    let end = t0 + Duration::from_millis(5);
    assert_eq!(idle_step_end(t0, end), Some(t0 + IDLE_STEP));
    // Near the end: the step stops there.
    let near = end - 300 * US;
    assert_eq!(idle_step_end(near, end), Some(end));
    assert_eq!(idle_step_end(end - MIN_IDLE_STEP, end), Some(end));
    // Too little left, or past the end: no step.
    assert_eq!(idle_step_end(end - MIN_IDLE_STEP + US, end), None);
    assert_eq!(idle_step_end(end, end), None);
    assert_eq!(idle_step_end(end + US, end), None);
}

#[test]
fn idle_work_keeps_the_most_urgent() {
    assert!(IdleWork::Ready > IdleWork::Waiting && IdleWork::Waiting > IdleWork::Done);
    let until = Instant::now();
    let mut step = IdleStep::new(until);
    assert_eq!(step.work(), IdleWork::Done);
    step.want(IdleWork::Waiting);
    step.want(IdleWork::Done);
    assert_eq!(step.work(), IdleWork::Waiting);
    step.want(IdleWork::Ready);
    step.want(IdleWork::Waiting);
    assert_eq!(step.work(), IdleWork::Ready);
}

#[test]
fn idle_info_counts_a_gap_and_keeps_the_overruns() {
    let t0 = Instant::now();
    let end = t0 + Duration::from_millis(2);
    let mut info = IdleInfo::default();
    info.record(t0, t0 + 300 * US, end);
    info.record(t0 + 500 * US, end + 40 * US, end);
    let gap = info.take_gap();
    assert_eq!(gap.steps, 2);
    assert_eq!(
        gap.tail,
        300 * US + Duration::from_millis(2) - 500 * US + 40 * US
    );
    assert_eq!(
        (gap.late, gap.overruns, gap.overrun_max),
        (40 * US, 1, 40 * US)
    );
    // The next gap starts empty; the overruns add up.
    let empty = info.take_gap();
    assert_eq!((empty.steps, empty.late), (0, Duration::ZERO));
    info.record(t0, end + 10 * US, end);
    let gap = info.take_gap();
    assert_eq!(
        (gap.steps, gap.late, gap.overruns, gap.overrun_max),
        (1, 10 * US, 2, 40 * US)
    );
}

#[test]
fn run_idle_without_systems_is_done() {
    let mut app = App::new();
    assert!(!app.has_idle_systems());
    assert_eq!(
        app.run_idle(Instant::now() + Duration::from_secs(1)),
        IdleWork::Done
    );
    assert!(!app.world().contains_resource::<IdleStep>());
}

#[derive(Resource, Default)]
struct Seen {
    step_ends: Vec<Instant>,
    changed_in_update: Vec<bool>,
}

#[derive(Resource, Default)]
struct Counter(u32);

#[test]
fn run_idle_runs_the_schedule_once_with_its_window() {
    let mut app = App::new();
    app.init_resource::<Seen>()
        .init_resource::<Counter>()
        .add_systems(
            Idle,
            (
                |mut step: ResMut<IdleStep>, mut seen: ResMut<Seen>| {
                    seen.step_ends.push(step.until);
                    step.want(IdleWork::Waiting);
                },
                |mut step: ResMut<IdleStep>, mut counter: ResMut<Counter>| {
                    counter.0 += 1;
                    step.want(IdleWork::Done);
                },
            ),
        )
        .add_systems(Update, |counter: Res<Counter>, mut seen: ResMut<Seen>| {
            let changed = counter.is_changed();
            seen.changed_in_update.push(changed);
        });
    assert!(app.has_idle_systems());
    // `Idle` is not part of the frame.
    let order = app.world().resource::<MainScheduleOrder>();
    assert!(!order.labels.contains(&Idle.intern()));
    assert!(!order.startup_labels.contains(&Idle.intern()));
    app.update();
    app.update();
    assert_eq!(app.world().resource::<Counter>().0, 0, "not run by updates");

    let until = Instant::now() + Duration::from_millis(3);
    assert_eq!(app.run_idle(until), IdleWork::Waiting);
    assert!(!app.world().contains_resource::<IdleStep>());
    assert_eq!(app.world().resource::<Seen>().step_ends, vec![until]);
    assert_eq!(app.world().resource::<Counter>().0, 1);
    // What the step changed reads as changed in the next update (and only there).
    app.update();
    app.update();
    let seen = app.world().resource::<Seen>();
    // First update: added; second: unchanged; after the step: changed; then unchanged again.
    assert_eq!(seen.changed_in_update, vec![true, false, true, false]);
}

#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
struct Note(u32);

#[test]
fn messages_written_by_an_idle_step_reach_the_next_update() {
    #[derive(Resource, Default)]
    struct Read(Vec<u32>);
    let mut app = App::new();
    app.add_message::<Note>()
        .init_resource::<Read>()
        .add_systems(Idle, |mut w: MessageWriter<Note>| {
            w.write(Note(7));
        })
        .add_systems(
            Update,
            |mut r: MessageReader<Note>, mut read: ResMut<Read>| {
                read.0.extend(r.read().map(|n| n.0));
            },
        );
    app.update();
    app.run_idle(Instant::now() + Duration::from_millis(1));
    app.update();
    app.update();
    assert_eq!(app.world().resource::<Read>().0, vec![7]);
}
