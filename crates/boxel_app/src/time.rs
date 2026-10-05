use crate::{App, First, FixedMain, FixedPostUpdate, Plugin, RunFixedMainLoop};
use boxel_ecs::message::{MessageRegistry, ShouldUpdateMessages, signal_message_update_system};
use boxel_ecs::prelude::*;
use std::time::{Duration, Instant};

/// Frame time (real clock, clamped). Updated in `First` by [`time_system`].
///
/// Inside `FixedMain` (and so in `FixedUpdate`) this resource is temporarily replaced by the
/// fixed-step view: `delta() == FixedTime::timestep()`, `elapsed() == FixedTime::elapsed()`,
/// same `frame_count()`. Bevy-style physics code (`time.delta_secs()` in `FixedUpdate`) is
/// therefore correct. The frame view is restored after each step.
#[derive(Resource, Debug, Clone)]
pub struct Time {
    startup: Instant,
    last_update: Option<Instant>,
    delta: Duration,
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
            elapsed: Duration::ZERO,
            frame_count: 0,
            max_delta: Duration::from_millis(250),
        }
    }
    /// Time since the previous update (0 on the first), clamped to `max_delta`.
    pub fn delta(&self) -> Duration {
        self.delta
    }
    /// `delta` in seconds.
    pub fn delta_secs(&self) -> f32 {
        self.delta.as_secs_f32()
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
    /// Advances the clock to `now` (deterministic tests use this directly). First call: delta 0.
    /// Later calls: `delta = min(now - last, max_delta)`. `frame_count` grows on every call.
    pub fn update_with_instant(&mut self, now: Instant) {
        self.delta = match self.last_update {
            None => Duration::ZERO,
            Some(last) => now.saturating_duration_since(last).min(self.max_delta),
        };
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

/// Fixed-timestep accumulator driving `FixedMain` (default 64 Hz, max 8 steps per frame).
#[derive(Resource, Debug, Clone)]
pub struct FixedTime {
    timestep: Duration,
    accumulator: Duration,
    elapsed: Duration,
    max_steps_per_frame: u32,
    last_overrun_warning: Option<Instant>,
}

impl Default for FixedTime {
    fn default() -> Self {
        Self::from_hz(64.0)
    }
}

impl FixedTime {
    /// Timestep of `1 / hz` seconds.
    pub fn from_hz(hz: f64) -> Self {
        Self {
            timestep: Duration::from_secs_f64(1.0 / hz),
            accumulator: Duration::ZERO,
            elapsed: Duration::ZERO,
            max_steps_per_frame: 8,
            last_overrun_warning: None,
        }
    }
    /// Step length.
    pub fn timestep(&self) -> Duration {
        self.timestep
    }
    /// Changes the step length.
    pub fn set_timestep(&mut self, timestep: Duration) {
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
}

/// System set of [`time_system`] (in `First`).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimeSystems;

/// Updates [`Time`] from the real clock.
pub fn time_system(mut time: ResMut<Time>) {
    time.update_with_instant(Instant::now());
}

/// Exclusive system in `RunFixedMainLoop`: accumulates `Time::delta` into [`FixedTime`] and
/// runs `FixedMain` while `expend()` returns true (at most `max_steps_per_frame` times; whole
/// steps left over are dropped with a warning at most once per second, the remainder is kept).
/// While `FixedMain` runs, [`Time`] holds the fixed-step view (see [`Time`]).
pub fn run_fixed_main_loop(world: &mut World) {
    let delta = world.resource::<Time>().delta();
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
    if steps == max_steps {
        let mut fixed = world.resource_mut::<FixedTime>();
        let step = fixed.timestep;
        let mut dropped = 0u32;
        while !step.is_zero() && fixed.accumulator >= step {
            fixed.accumulator -= step;
            dropped += 1;
        }
        let now = Instant::now();
        if dropped > 0
            && fixed
                .last_overrun_warning
                .is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1))
        {
            fixed.last_overrun_warning = Some(now);
            tracing::warn!(
                "fixed timestep overrun: ran {max_steps} steps this frame, dropped {dropped} more"
            );
        }
    }
}

/// Inserts [`Time`] and [`FixedTime`], adds [`time_system`] in `First` (in [`TimeSystems`]),
/// [`run_fixed_main_loop`] in `RunFixedMainLoop`, and makes buffered messages survive until a
/// fixed step has seen them: `signal_message_update_system` runs in `FixedPostUpdate` and
/// `MessageRegistry::should_update` starts as `Waiting` (Bevy's behavior). Without this, a
/// `FixedUpdate` reader would lose messages whenever frames run faster than half the fixed rate.
#[derive(Default)]
pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Time>()
            .init_resource::<FixedTime>()
            .add_systems(First, time_system.in_set(TimeSystems))
            .add_systems(RunFixedMainLoop, run_fixed_main_loop)
            .add_systems(FixedPostUpdate, signal_message_update_system);
        app.world_mut()
            .get_resource_or_init::<MessageRegistry>()
            .should_update = ShouldUpdateMessages::Waiting;
    }
}
