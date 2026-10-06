use crate::{
    App, First, FixedMain, FixedPostUpdate, Plugin, RunFixedMainLoop, RunFixedMainLoopSystems,
};
use boxel_ecs::message::{
    MessageRegistry, ShouldUpdateMessages, message_update_system, signal_message_update_system,
};
use boxel_ecs::prelude::*;
use std::time::{Duration, Instant};

/// Frame time (real clock, clamped). Updated in `First` by [`time_system`].
///
/// Two deltas per update:
/// - [`Time::delta`] advances the simulation: wall-clock time since the previous update minus
///   any declared stall ([`Time::declare_stall`]), clamped to [`Time::max_delta`]. `elapsed`
///   and the [`FixedTime`] accumulator follow it.
/// - [`Time::real_delta`] measures the frame: wall-clock time since the previous update, never
///   clamped and stalls included. Use it for frame-time statistics.
///
/// Inside `FixedMain` (and so in `FixedUpdate`) this resource is temporarily replaced by the
/// fixed-step view: `delta() == FixedTime::timestep()`, `elapsed() == FixedTime::elapsed()`,
/// same `frame_count()` and `real_delta()`. Bevy-style physics code (`time.delta_secs()` in
/// `FixedUpdate`) is therefore correct. The frame view is restored after each step.
#[derive(Resource, Debug, Clone)]
pub struct Time {
    startup: Instant,
    last_update: Option<Instant>,
    delta: Duration,
    real_delta: Duration,
    /// Stall excluded from the latest `delta`.
    stall: Duration,
    /// Stalls declared since the latest update (applied by the next one).
    pending_stall: Duration,
    elapsed: Duration,
    frame_count: u64,
    max_delta: Duration,
}

impl Default for Time {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}

impl Time {
    /// New clock starting at `startup`. `max_delta` defaults to 250 ms.
    pub fn new(startup: Instant) -> Self {
        Self {
            startup,
            last_update: None,
            delta: Duration::ZERO,
            real_delta: Duration::ZERO,
            stall: Duration::ZERO,
            pending_stall: Duration::ZERO,
            elapsed: Duration::ZERO,
            frame_count: 0,
            max_delta: Duration::from_millis(250),
        }
    }
    /// The instant this clock started (the `startup` given to [`Time::new`]).
    pub fn startup(&self) -> Instant {
        self.startup
    }
    /// The instant of the latest update (`None` before the first).
    pub fn last_update(&self) -> Option<Instant> {
        self.last_update
    }
    /// Simulation time since the previous update (0 on the first): wall-clock time minus
    /// declared stalls, clamped to `max_delta`.
    pub fn delta(&self) -> Duration {
        self.delta
    }
    /// `delta` in seconds.
    pub fn delta_secs(&self) -> f32 {
        self.delta.as_secs_f32()
    }
    /// Wall-clock time between the previous update and the latest one (0 on the first), not
    /// clamped and including declared stalls. Use it to measure frame times; use
    /// [`Time::delta`] to advance the simulation.
    pub fn real_delta(&self) -> Duration {
        self.real_delta
    }
    /// The part of the latest `real_delta` that `delta` excludes because a stall was declared
    /// ([`Time::declare_stall`]); zero when none was.
    pub fn stall(&self) -> Duration {
        self.stall
    }
    /// Sum of all (clamped) deltas.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }
    /// `elapsed` in seconds (f64).
    pub fn elapsed_secs_f64(&self) -> f64 {
        self.elapsed.as_secs_f64()
    }
    /// Number of updates so far.
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }
    /// Maximum delta per update.
    pub fn max_delta(&self) -> Duration {
        self.max_delta
    }
    /// Sets the maximum delta per update.
    pub fn set_max_delta(&mut self, max: Duration) {
        self.max_delta = max;
    }

    /// Declares that `duration` of the wall-clock time since the latest update was a known
    /// stall the simulation must not catch up on: GPU bring-up, device-loss recovery, a process
    /// that was suspended. The next update subtracts it from [`Time::delta`], so neither
    /// `elapsed` nor the [`FixedTime`] accumulator see it (no burst of fixed steps, no jump of
    /// anything moved with `delta`). [`Time::real_delta`] still includes it, so frame-time
    /// statistics stay honest, and [`Time::stall`] reports the part that was excluded.
    ///
    /// Call it right after the stall, from any schedule (before or after `First`): the stall
    /// always lies in the interval of the next update. Declarations add up until that update
    /// and never remove more than its real interval. A declaration before the first update is
    /// dropped (the first delta is zero anyway).
    pub fn declare_stall(&mut self, duration: Duration) {
        self.pending_stall = self.pending_stall.saturating_add(duration);
        tracing::debug!(
            "stall of {:.1} ms declared: the next frame delta excludes it",
            duration.as_secs_f64() * 1e3
        );
    }

    /// Advances the clock to `now` (deterministic tests use this directly). First call: delta 0.
    /// Later calls: `real_delta = now - last` and `delta = min(real_delta - declared stalls,
    /// max_delta)`. `frame_count` grows on every call.
    pub fn update_with_instant(&mut self, now: Instant) {
        let pending = std::mem::take(&mut self.pending_stall);
        (self.real_delta, self.stall) = match self.last_update {
            None => (Duration::ZERO, Duration::ZERO),
            Some(last) => {
                let real = now.saturating_duration_since(last);
                (real, pending.min(real))
            }
        };
        self.delta = (self.real_delta - self.stall).min(self.max_delta);
        self.last_update = Some(now);
        self.elapsed += self.delta;
        self.frame_count += 1;
    }

    /// The fixed-step view used while `FixedMain` runs.
    fn fixed_view(&self, fixed: &FixedTime) -> Time {
        Time {
            delta: fixed.timestep,
            elapsed: fixed.elapsed,
            ..self.clone()
        }
    }
}

/// Default of [`FixedTime::startup_grace_frames`]: updates whose fixed-step overruns are
/// expected (window creation, GPU bring-up, first uploads) and only debug-logged.
pub const DEFAULT_STARTUP_GRACE_FRAMES: u64 = 10;

/// Minimum app-clock time between two fixed-step overrun warnings (overruns in between are
/// summed into the next one).
pub const OVERRUN_WARNING_INTERVAL: Duration = Duration::from_secs(5);

/// Frames that hit the fixed-step limit (see [`run_fixed_main_loop`]), cumulative since startup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FixedOverruns {
    /// Frames after the startup grace that ran the maximum number of steps and still had whole
    /// steps left, which were dropped: the simulation fell behind real time.
    pub frames: u64,
    /// Whole steps dropped in those frames.
    pub dropped_steps: u64,
    /// Overrun frames inside the startup grace (debug-logged only, not counted in `frames`).
    pub startup_frames: u64,
    /// Overrun warnings logged.
    pub warnings: u64,
}

/// Fixed-timestep accumulator driving `FixedMain` (default 64 Hz, max 8 steps per frame).
#[derive(Resource, Debug, Clone)]
pub struct FixedTime {
    timestep: Duration,
    accumulator: Duration,
    elapsed: Duration,
    max_steps_per_frame: u32,
    steps_last_frame: u32,
    startup_grace_frames: u64,
    overruns: FixedOverruns,
    /// Overrun frames and dropped steps not reported by a warning yet.
    unreported: (u64, u64),
    last_overrun_warning: Option<Instant>,
}

impl Default for FixedTime {
    fn default() -> Self {
        Self::from_hz(64.0)
    }
}

impl FixedTime {
    /// Timestep of `1 / hz` seconds.
    ///
    /// # Panics
    ///
    /// If `hz` is not finite and positive, or so large that the step rounds to zero (as Bevy).
    pub fn from_hz(hz: f64) -> Self {
        assert!(
            hz.is_finite() && hz > 0.0,
            "FixedTime::from_hz: rate must be finite and positive, got {hz}"
        );
        let mut fixed = Self {
            timestep: Duration::ZERO,
            accumulator: Duration::ZERO,
            elapsed: Duration::ZERO,
            max_steps_per_frame: 8,
            steps_last_frame: 0,
            startup_grace_frames: DEFAULT_STARTUP_GRACE_FRAMES,
            overruns: FixedOverruns::default(),
            unreported: (0, 0),
            last_overrun_warning: None,
        };
        fixed.set_timestep(Duration::from_secs_f64(1.0 / hz));
        fixed
    }
    /// Step length.
    pub fn timestep(&self) -> Duration {
        self.timestep
    }
    /// Changes the step length.
    ///
    /// # Panics
    ///
    /// If `timestep` is zero (as Bevy): no fixed step could ever run.
    pub fn set_timestep(&mut self, timestep: Duration) {
        assert!(
            !timestep.is_zero(),
            "FixedTime::set_timestep: the timestep must not be zero"
        );
        self.timestep = timestep;
    }
    /// Pending time not yet consumed by steps.
    pub fn accumulator(&self) -> Duration {
        self.accumulator
    }
    /// Total simulated fixed time (`steps run * timestep`).
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }
    /// `accumulator / timestep`, in `[0, 1)` after a frame's steps (interpolation factor).
    pub fn overstep_fraction(&self) -> f32 {
        if self.timestep.is_zero() {
            0.0
        } else {
            self.accumulator.as_secs_f32() / self.timestep.as_secs_f32()
        }
    }
    /// Adds frame time to the accumulator.
    pub fn accumulate(&mut self, delta: Duration) {
        self.accumulator += delta;
    }
    /// Consumes one step if available; returns true if a step should run.
    pub fn expend(&mut self) -> bool {
        if !self.timestep.is_zero() && self.accumulator >= self.timestep {
            self.accumulator -= self.timestep;
            self.elapsed += self.timestep;
            true
        } else {
            false
        }
    }
    /// Most steps one frame runs (default 8); whole steps beyond it are dropped.
    pub fn max_steps_per_frame(&self) -> u32 {
        self.max_steps_per_frame
    }
    /// Sets the most steps one frame runs (default 8). After a hitch, a frame runs up to this
    /// many steps to catch up and drops the rest (counted in [`FixedTime::overruns`]). A lower
    /// limit bounds the cost of a catch-up frame when steps are expensive (many physics bodies:
    /// 3 is a good value), at the price of simulated time lost sooner.
    ///
    /// # Panics
    ///
    /// If `steps` is 0: no fixed step could ever run.
    pub fn set_max_steps_per_frame(&mut self, steps: u32) {
        assert!(
            steps >= 1,
            "FixedTime::set_max_steps_per_frame: at least one step per frame, got 0"
        );
        self.max_steps_per_frame = steps;
    }
    /// Steps [`run_fixed_main_loop`] ran in the latest frame.
    pub fn steps_last_frame(&self) -> u32 {
        self.steps_last_frame
    }
    /// Updates (`Time::frame_count`) whose overruns are only debug-logged.
    pub fn startup_grace_frames(&self) -> u64 {
        self.startup_grace_frames
    }
    /// Sets the startup grace (default [`DEFAULT_STARTUP_GRACE_FRAMES`]). The first updates
    /// include window creation, GPU bring-up and the first uploads: their overruns are expected
    /// and say nothing about the simulation being too slow. 0 warns from the first frame.
    pub fn set_startup_grace_frames(&mut self, frames: u64) {
        self.startup_grace_frames = frames;
    }
    /// Overrun counters since startup.
    pub fn overruns(&self) -> FixedOverruns {
        self.overruns
    }

    /// Counts an overrun of update `frame` that dropped `dropped` whole steps.
    fn record_overrun(&mut self, frame: u64, dropped: u32) {
        if frame <= self.startup_grace_frames {
            self.overruns.startup_frames += 1;
            tracing::debug!(
                "fixed timestep overrun during startup (frame {frame}): dropped {dropped} steps"
            );
            return;
        }
        self.overruns.frames += 1;
        self.overruns.dropped_steps += u64::from(dropped);
        self.unreported.0 += 1;
        self.unreported.1 += u64::from(dropped);
    }

    /// Warns about the unreported overruns, at most once per [`OVERRUN_WARNING_INTERVAL`] of
    /// the app clock (`now` is `Time::last_update`, so synthetic clocks work).
    fn flush_overrun_warning(&mut self, now: Instant) {
        let (frames, dropped) = self.unreported;
        if frames == 0 {
            return;
        }
        let since = match self.last_overrun_warning {
            Some(last) if now.saturating_duration_since(last) < OVERRUN_WARNING_INTERVAL => {
                return;
            }
            Some(last) => format!(
                "in the last {:.1} s",
                now.saturating_duration_since(last).as_secs_f64()
            ),
            None => "so far".to_string(),
        };
        let lost = self
            .timestep
            .saturating_mul(u32::try_from(dropped).unwrap_or(u32::MAX));
        tracing::warn!(
            "fixed timestep overrun: {frames} frame(s) {since} ran the maximum of {} steps and \
             still dropped {dropped} step(s) ({:.0} ms of simulated time): the simulation is \
             falling behind real time",
            self.max_steps_per_frame,
            lost.as_secs_f64() * 1e3,
        );
        self.overruns.warnings += 1;
        self.last_overrun_warning = Some(now);
        self.unreported = (0, 0);
    }
}

/// System set of [`time_system`] (in `First`).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimeSystems;

/// Updates [`Time`] from the real clock.
pub fn time_system(mut time: ResMut<Time>) {
    time.update_with_instant(Instant::now());
}

/// Exclusive system in `RunFixedMainLoop` ([`RunFixedMainLoopSystems::FixedMainLoop`]):
/// accumulates `Time::delta` into [`FixedTime`] and runs `FixedMain` while `expend()` returns
/// true, at most `max_steps_per_frame` times. Whole steps left over after that are dropped (the
/// remainder is kept) and counted in [`FixedTime::overruns`]. While `FixedMain` runs, [`Time`]
/// holds the fixed-step view (see [`Time`]).
///
/// Overrun warnings: overruns in the first [`FixedTime::startup_grace_frames`] updates are
/// expected (bring-up) and only debug-logged. Later ones are summed and warned at most once per
/// [`OVERRUN_WARNING_INTERVAL`] of the app clock, with the frames and steps since the previous
/// warning. Known stalls should be declared with [`Time::declare_stall`], so they never reach
/// the accumulator.
///
/// A frame with a zero `Time::delta` (paused time, e.g. `Time::set_max_delta(Duration::ZERO)`)
/// cannot bring a fixed step closer, so it lets the next `First` rotate the message buffers
/// anyway. Without this, paused time would keep every message ever written.
pub fn run_fixed_main_loop(world: &mut World) {
    let (delta, frame, now) = {
        let time = world.resource::<Time>();
        (time.delta(), time.frame_count(), time.last_update())
    };
    if delta.is_zero()
        && let Some(mut registry) = world.get_resource_mut::<MessageRegistry>()
        && registry.should_update == ShouldUpdateMessages::Waiting
    {
        registry.should_update = ShouldUpdateMessages::Ready;
    }
    let max_steps = {
        let mut fixed = world.resource_mut::<FixedTime>();
        fixed.accumulate(delta);
        fixed.max_steps_per_frame
    };
    let mut steps = 0;
    while steps < max_steps && world.resource_mut::<FixedTime>().expend() {
        let frame_view = world.resource::<Time>().clone();
        let fixed_view = frame_view.fixed_view(world.resource::<FixedTime>());
        *world.resource_mut::<Time>() = fixed_view;
        world.run_schedule(FixedMain);
        *world.resource_mut::<Time>() = frame_view;
        steps += 1;
    }
    let mut fixed = world.resource_mut::<FixedTime>();
    fixed.steps_last_frame = steps;
    if steps == max_steps {
        let step = fixed.timestep;
        let mut dropped = 0u32;
        while !step.is_zero() && fixed.accumulator >= step {
            fixed.accumulator -= step;
            dropped += 1;
        }
        if dropped > 0 {
            fixed.record_overrun(frame, dropped);
        }
    }
    if let Some(now) = now {
        fixed.flush_overrun_warning(now);
    }
}

/// Inserts [`Time`] and [`FixedTime`], adds [`time_system`] in `First` (in [`TimeSystems`]),
/// [`run_fixed_main_loop`] in `RunFixedMainLoop` (in
/// [`RunFixedMainLoopSystems::FixedMainLoop`]), and makes buffered messages survive until a
/// fixed step has seen them: `signal_message_update_system` runs in `FixedPostUpdate` and
/// `MessageRegistry::should_update` starts as `Waiting` (Bevy's behavior). Without this, a
/// `FixedUpdate` reader would lose messages whenever frames run faster than half the fixed rate.
/// While time is paused, buffers rotate every frame (see [`run_fixed_main_loop`]).
///
/// [`time_system`] is marked ambiguous with `message_update_system` (also in `First`, added by
/// [`App::new`]), as in Bevy: the message update is an exclusive system, so the pair counts as
/// a conflict, but it never touches [`Time`] and their order does not matter. Strict ambiguity
/// detection on `First` then stays clean.
#[derive(Default)]
pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Time>()
            .init_resource::<FixedTime>()
            .add_systems(
                First,
                time_system
                    .in_set(TimeSystems)
                    .ambiguous_with(message_update_system),
            )
            .add_systems(
                RunFixedMainLoop,
                run_fixed_main_loop.in_set(RunFixedMainLoopSystems::FixedMainLoop),
            )
            .add_systems(FixedPostUpdate, signal_message_update_system);
        app.world_mut()
            .get_resource_or_init::<MessageRegistry>()
            .should_update = ShouldUpdateMessages::Waiting;
    }
}
