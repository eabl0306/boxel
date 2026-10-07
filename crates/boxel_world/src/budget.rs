//! Per-frame world budgets that grow into the idle part of a capped frame (issue #3).
//!
//! With a frame cap the world gets fewer frames per second, and its work has a fixed budget per
//! frame (`WorldSettings::interest_budget` and `job_budget`), so it catches up more slowly after
//! a chunk-border crossing than uncapped. When the runner publishes the next frame's deadline
//! ([`FrameDeadline`]), the budgeted steps may run until `next - max(MIN_MARGIN, period / 2) -
//! rest`, where `rest` is the main-thread time of the rest of an update (render included),
//! learned from the previous updates. The time left is split between the two steps in the ratio
//! of their budgets; a budget never shrinks below the setting, and a zero budget stays zero.
//! Without a deadline (unlimited, or no pacing runner) the budgets are the settings, exactly.
//!
//! Half the period is kept free because the update must present well before the next frame: on
//! Wayland the compositor takes a frame about 4.5 ms after its frame callback at 144 Hz (ADR
//! 0017), so a frame that ends near its deadline would miss its refresh.
use crate::WorldSettings;
use boxel_app::FrameDeadline;
use boxel_ecs::prelude::*;
use std::time::{Duration, Instant};

/// Least time kept free before the next frame's deadline.
pub(crate) const MIN_MARGIN: Duration = Duration::from_micros(1500);
/// A smaller sample of the rest of an update lowers the estimate by this fraction of the gap
/// (a larger one raises it at once: spikes count fully, quiet frames are trusted slowly).
const REST_DECAY: u32 = 8;

/// The budget state of the current frame (a private world resource).
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FrameBudget {
    /// When this frame's budgeted world steps must end (`None`: the settings' budgets).
    end: Option<Instant>,
    /// Estimated main-thread time of an update outside the budgeted world steps (`None`: no
    /// sample yet).
    rest: Option<Duration>,
    /// Time spent in budgeted world steps this frame (`None`: not measured, no deadline).
    spent: Option<Duration>,
}

impl FrameBudget {
    /// Starts a frame under `deadline`: learns the rest of the previous update (its length
    /// minus the world steps measured in it) and sets this frame's end.
    pub(crate) fn begin(&mut self, deadline: Option<&FrameDeadline>) {
        let previous = self.spent.take();
        let Some((next, period, last_update)) =
            deadline.and_then(|d| Some((d.next?, d.period?, d.last_update)))
        else {
            self.end = None;
            return;
        };
        if !last_update.is_zero() {
            let sample = last_update.saturating_sub(previous.unwrap_or_default());
            self.rest = Some(match self.rest {
                Some(rest) if sample < rest => rest - (rest - sample) / REST_DECAY,
                _ => sample,
            });
        }
        self.spent = Some(Duration::ZERO);
        self.end = self.rest.and_then(|rest| {
            next.checked_sub(MIN_MARGIN.max(period / 2))?
                .checked_sub(rest)
        });
    }

    /// Time left for the budgeted steps at `now` (`None`: no deadline).
    fn left(&self, now: Instant) -> Option<Duration> {
        self.end.map(|end| end.saturating_duration_since(now))
    }

    /// The interest step's budget at `now`: its share of the time left, at least the setting.
    pub(crate) fn interest(&self, settings: &WorldSettings, now: Instant) -> Duration {
        let base = settings.interest_budget;
        let total = base.saturating_add(settings.job_budget);
        match self.left(now) {
            Some(left) if !base.is_zero() && left > base => {
                base.max(left.mul_f64(base.as_secs_f64() / total.as_secs_f64()))
            }
            _ => base,
        }
    }

    /// The job step's budget at `now` (the last budgeted step): the time left, at least the
    /// setting.
    pub(crate) fn jobs(&self, settings: &WorldSettings, now: Instant) -> Duration {
        let base = settings.job_budget;
        match self.left(now) {
            Some(left) if !base.is_zero() => base.max(left),
            _ => base,
        }
    }

    /// A clock reading for [`FrameBudget::spend`] (`None` without a deadline: nothing is
    /// measured then).
    pub(crate) fn clock(&self) -> Option<Instant> {
        self.spent.map(|_| Instant::now())
    }

    /// Adds the time since `since` (from [`FrameBudget::clock`]) to this frame's world steps.
    pub(crate) fn spend(&mut self, since: Option<Instant>) {
        if let (Some(spent), Some(since)) = (self.spent.as_mut(), since) {
            *spent += since.elapsed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn deadline(next: Instant, period: Duration, last_update: Duration) -> FrameDeadline {
        FrameDeadline {
            next: Some(next),
            period: Some(period),
            last_update,
        }
    }

    #[test]
    fn no_deadline_keeps_the_settings() {
        let settings = WorldSettings::default();
        let now = Instant::now();
        let mut budget = FrameBudget::default();
        for frame in [None, Some(FrameDeadline::default())] {
            budget.begin(frame.as_ref());
            assert_eq!(budget.interest(&settings, now), settings.interest_budget);
            assert_eq!(budget.jobs(&settings, now), settings.job_budget);
            assert_eq!(budget.clock(), None);
        }
    }

    #[test]
    fn budgets_grow_into_the_first_half_of_the_frame() {
        let settings = WorldSettings::default();
        let period = Duration::from_nanos(6_944_444);
        let t0 = Instant::now();
        let mut budget = FrameBudget::default();
        // The first frame knows no rest of the update yet: the settings.
        budget.begin(Some(&deadline(t0 + period, period, Duration::ZERO)));
        assert_eq!(budget.jobs(&settings, t0), settings.job_budget);
        // An update of 1 ms with no world work: the steps may run until the deadline minus half
        // the period minus that 1 ms (2.47 ms from the frame start).
        budget.spent = Some(Duration::ZERO);
        budget.begin(Some(&deadline(t0 + period, period, MS)));
        let left = period - period / 2 - MS;
        assert_eq!(budget.left(t0), Some(left));
        // Interest takes its share (1 of 3), at least its setting; jobs take what is left.
        assert_eq!(budget.interest(&settings, t0), settings.interest_budget);
        let at = t0 + Duration::from_micros(200);
        assert_eq!(
            budget.jobs(&settings, at),
            left - Duration::from_micros(200)
        );
        // Never below the setting, even past the end.
        assert_eq!(budget.jobs(&settings, t0 + period), settings.job_budget);

        // 60 Hz: more room, and the interest share grows past its setting.
        let period = Duration::from_nanos(16_666_667);
        budget.spent = Some(Duration::ZERO);
        budget.begin(Some(&deadline(t0 + period, period, MS)));
        let left = period - period / 2 - MS;
        let share = MS.as_secs_f64() / (MS * 3).as_secs_f64();
        assert_eq!(budget.interest(&settings, t0), left.mul_f64(share));
        assert_eq!(budget.jobs(&settings, t0), left);
        // A zero budget stays zero.
        let zero = WorldSettings {
            interest_budget: Duration::ZERO,
            job_budget: Duration::ZERO,
            ..WorldSettings::default()
        };
        assert_eq!(budget.interest(&zero, t0), Duration::ZERO);
        assert_eq!(budget.jobs(&zero, t0), Duration::ZERO);
    }

    #[test]
    fn the_work_never_reaches_past_the_frame() {
        // Whatever the rest of the update, the steps end by the deadline minus the margin.
        let t0 = Instant::now();
        for period in [Duration::from_micros(500), MS * 2, MS * 7, MS * 33] {
            let mut budget = FrameBudget {
                spent: Some(Duration::ZERO),
                ..FrameBudget::default()
            };
            budget.begin(Some(&deadline(
                t0 + period,
                period,
                Duration::from_nanos(1),
            )));
            let left = budget.left(t0).unwrap_or_default();
            let room = period.saturating_sub(MIN_MARGIN.max(period / 2));
            assert!(left <= room, "{period:?}: {left:?}");
        }
    }

    #[test]
    fn the_rest_of_the_update_rises_at_once_and_falls_slowly() {
        let period = Duration::from_nanos(16_666_667);
        let t0 = Instant::now();
        let mut budget = FrameBudget::default();
        let mut frame = |last_update: Duration, world: Duration| {
            budget.spent = Some(world);
            budget.begin(Some(&deadline(t0 + period, period, last_update)));
            budget.rest.expect("sampled")
        };
        assert_eq!(frame(MS * 3, MS), MS * 2);
        assert_eq!(frame(MS * 6, MS), MS * 5);
        assert_eq!(frame(MS * 2, MS), MS * 5 - (MS * 4) / REST_DECAY);
        // The world's own time is not the rest.
        let rest = MS * 5 - (MS * 4) / REST_DECAY;
        assert_eq!(frame(MS * 20, MS * 20), rest - rest / REST_DECAY);
    }
}
