//! Work between frames (ADR 0022): the [`Idle`](crate::Idle) schedule, the step a runner gives
//! it and what the runners report about it.
//!
//! Under a frame limit every update ends early and the runner waits for the next deadline. A
//! runner slots short idle steps into that wait, from right after the update (and its present)
//! to [`IDLE_MARGIN`] before the spin margin of the next deadline. Each step runs the `Idle`
//! schedule once with an [`IdleStep`] that says when it must return; its systems say whether
//! they want another step at once ([`IdleWork::Ready`]), only after a short poll
//! ([`IdleWork::Waiting`]) or none before the next update ([`IdleWork::Done`]). Without a limit,
//! or without `Idle` systems, no step ever runs. An app can also say after each update whether
//! a step would find work ([`IdleWanted`]); when none would, the runner opens no slot.
use crate::schedules::Idle;
use crate::{App, FramePacer};
use boxel_ecs::prelude::*;
use std::time::{Duration, Instant};

/// Time kept free between the end of the idle slot and the spin margin of the next deadline:
/// at the default spin margin (300 us) the slot ends 1 ms before the frame is due, so a step
/// that returns a little late still leaves the update its full period.
pub const IDLE_MARGIN: Duration = Duration::from_micros(700);
/// Longest idle step: the most time the runner goes without handling window events or checking
/// the presentation gate.
pub const IDLE_STEP: Duration = Duration::from_micros(500);
/// Shortest idle step worth running: with less time left in the slot the runner just waits.
pub const MIN_IDLE_STEP: Duration = Duration::from_micros(200);
/// How long the runner waits before the next step after [`IdleWork::Waiting`] (work in flight
/// elsewhere, such as jobs on worker threads).
pub const IDLE_POLL: Duration = Duration::from_micros(250);

/// What the `Idle` systems want after a step. Ordered by urgency: [`IdleStep::want`] keeps the
/// most urgent of the systems' answers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdleWork {
    /// Nothing to do until the next update.
    #[default]
    Done,
    /// Work is in flight elsewhere: poll again after [`IDLE_POLL`].
    Waiting,
    /// More work now: run the next step at once.
    Ready,
}

/// The current idle step (a resource that exists only while the `Idle` schedule runs).
///
/// Systems stop their work by [`IdleStep::until`] and never block; they report what they want
/// next with [`IdleStep::want`].
#[derive(Resource, Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdleStep {
    /// When the step must end.
    pub until: Instant,
    work: IdleWork,
}

impl IdleStep {
    /// A step that ends at `until` and wants nothing yet.
    pub const fn new(until: Instant) -> Self {
        Self {
            until,
            work: IdleWork::Done,
        }
    }

    /// Records what a system wants next (the most urgent answer of the step wins).
    pub fn want(&mut self, work: IdleWork) {
        self.work = self.work.max(work);
    }

    /// The most urgent answer so far.
    pub fn work(&self) -> IdleWork {
        self.work
    }
}

/// Whether a step after the current update would find work, asked by the update's own systems
/// so a runner opens no idle slot (no `Poll`, no step) after an update that leaves nothing to do.
///
/// Optional. Without this resource a runner opens a slot after every update with room for a
/// step, and the first step finds out. With it, the runners clear it before every update
/// ([`IdleWanted::before_update`]) and open a slot only if a system of that update asked for one
/// ([`IdleWanted::want`], the answers add up), so in an app that has it every plugin with `Idle`
/// systems must ask when it leaves work. `boxel_world` adds it and asks at the end of its frame
/// steps.
///
/// Why (ADR 0022): a slot whose only step answers [`IdleWork::Done`] costs about 6 us, yet on
/// the RTX 5050 under PRIME it made the runs that keep skipping refreshes more frequent (issue
/// #5). Without a slot the wait is the plain one of an app without `Idle` systems.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdleWanted(bool);

impl IdleWanted {
    /// Asks for idle steps after this update when `wanted` (one ask is enough: later answers
    /// never take it back).
    pub fn want(&mut self, wanted: bool) {
        self.0 |= wanted;
    }

    /// True if a system of this update asked for idle steps.
    pub fn wanted(self) -> bool {
        self.0
    }

    /// For runners, before an update: forgets what the previous update asked.
    pub fn before_update(world: &mut World) {
        if let Some(mut wanted) = world.get_resource_mut::<Self>() {
            wanted.set_if_neq(Self(false));
        }
    }

    /// For runners, after an update: whether to open an idle slot. True without an
    /// `IdleWanted` (the app gives no hint), else whether a system of the update asked.
    pub fn after_update(world: &World) -> bool {
        world.get_resource::<Self>().is_none_or(|wanted| wanted.0)
    }
}

/// What the runner did between updates, for diagnostics (benchmarks, overlays). Both runners
/// write it right before each update, next to [`FrameDeadline`](crate::FrameDeadline).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdleInfo {
    /// Time spent in idle steps since the previous update (not the waits between them).
    pub tail: Duration,
    /// Idle steps since the previous update.
    pub steps: u32,
    /// How far past the end of its slot a step since the previous update returned, at worst
    /// (zero: none did).
    pub late: Duration,
    /// Steps that returned after the end of their slot, since the runner started.
    pub overruns: u64,
    /// The worst lateness of those steps.
    pub overrun_max: Duration,
}

impl IdleInfo {
    /// Records a step that ran from `started` to `ended` in a slot that ends at `end`.
    pub fn record(&mut self, started: Instant, ended: Instant, end: Instant) {
        self.tail += ended.saturating_duration_since(started);
        self.steps = self.steps.saturating_add(1);
        if ended > end {
            self.overruns += 1;
            self.late = self.late.max(ended - end);
            self.overrun_max = self.overrun_max.max(ended - end);
        }
    }

    /// What to publish before an update; starts counting the next gap (the overruns add up).
    pub fn take_gap(&mut self) -> Self {
        let gap = *self;
        self.tail = Duration::ZERO;
        self.steps = 0;
        self.late = Duration::ZERO;
        gap
    }
}

/// The end of the next idle step at `now` in a slot that ends at `end`: at most [`IDLE_STEP`]
/// later, never past `end`. `None` when less than [`MIN_IDLE_STEP`] is left.
pub fn idle_step_end(now: Instant, end: Instant) -> Option<Instant> {
    let step = now.checked_add(IDLE_STEP)?;
    (now.checked_add(MIN_IDLE_STEP)? <= end).then(|| step.min(end))
}

impl FramePacer {
    /// The end of the idle slot before the next frame under `period`: its deadline minus the
    /// spin margin and [`IDLE_MARGIN`] (`None`: no frame yet, unlimited, or not representable).
    pub fn idle_end(&self, period: Option<Duration>) -> Option<Instant> {
        self.next_frame(period)?
            .checked_sub(self.spin_margin())?
            .checked_sub(IDLE_MARGIN)
    }
}

impl App {
    /// True if the [`Idle`] schedule has systems (runners slot idle steps only then, and with an
    /// [`IdleWanted`] only after an update that asked for them).
    pub fn has_idle_systems(&self) -> bool {
        self.world()
            .get_resource::<Schedules>()
            .and_then(|schedules| schedules.get(Idle))
            .is_some_and(|schedule| schedule.systems_len() > 0)
    }

    /// Runs one idle step that must end by `until`: the [`Idle`] schedule once, with an
    /// [`IdleStep`] resource that exists only during the step. Returns what its systems want
    /// next ([`IdleWork::Done`] at once without `Idle` systems).
    ///
    /// Change trackers are not cleared, unlike [`App::update`]: what the step changes reads as
    /// changed in the next update, and the messages it writes reach that update's readers.
    pub fn run_idle(&mut self, until: Instant) -> IdleWork {
        if !self.has_idle_systems() {
            return IdleWork::Done;
        }
        let world = self.world_mut();
        world.insert_resource(IdleStep::new(until));
        let _ = world.try_run_schedule(Idle);
        world
            .remove_resource::<IdleStep>()
            .map_or(IdleWork::Done, |step| step.work())
    }
}
