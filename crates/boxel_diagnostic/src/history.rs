//! [`FrameTimeHistory`]: a ring buffer of recent frame times, and [`FrameTimeSummary`]:
//! percentiles, maximum, average and "1% low" over any set of frame times.

use boxel_ecs::prelude::*;
use std::time::Duration;

/// The last frame times, oldest overwritten first. [`crate::FrameStatsPlugin`] pushes
/// `Time::real_delta` (wall clock, not clamped, stalls included) once per frame from the second
/// update on, so the history shows the frames as the player felt them.
///
/// Recording is O(1) and never allocates (the buffer is allocated once, at the full capacity).
/// [`FrameTimeHistory::summary`] sorts a copy, O(N log N): call it a few times per second at
/// most, not from every system of every frame.
///
/// To change the capacity, insert your own history before or after adding the plugin:
/// `app.insert_resource(FrameTimeHistory::new(16_384))`.
#[derive(Resource, Clone, Debug)]
pub struct FrameTimeHistory {
    /// Samples; grows to `capacity` and then wraps.
    samples: Vec<Duration>,
    capacity: usize,
    /// Slot the next sample goes to once the buffer is full (the oldest sample).
    next: usize,
    /// Samples pushed since creation (or the last [`FrameTimeHistory::clear`]).
    recorded: u64,
}

impl Default for FrameTimeHistory {
    fn default() -> Self {
        Self::new(Self::DEFAULT_CAPACITY)
    }
}

impl FrameTimeHistory {
    /// Smallest capacity: 1024 frames (about 7 s at 144 fps).
    pub const MIN_CAPACITY: usize = 1024;
    /// Default capacity: 4096 frames (about 28 s at 144 fps, 64 KiB).
    pub const DEFAULT_CAPACITY: usize = 4096;

    /// An empty history keeping the last `capacity` frames (raised to
    /// [`FrameTimeHistory::MIN_CAPACITY`] if smaller).
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(Self::MIN_CAPACITY);
        Self {
            samples: Vec::with_capacity(capacity),
            capacity,
            next: 0,
            recorded: 0,
        }
    }

    /// Records one frame time, dropping the oldest one when full.
    pub fn push(&mut self, frame_time: Duration) {
        if self.samples.len() < self.capacity {
            self.samples.push(frame_time);
        } else {
            self.samples[self.next] = frame_time;
            self.next = (self.next + 1) % self.capacity;
        }
        self.recorded += 1;
    }

    /// Frames held (at most [`FrameTimeHistory::capacity`]).
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// No frame recorded yet.
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    /// Frames kept.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Frames pushed in total, including the ones already overwritten.
    pub fn recorded(&self) -> u64 {
        self.recorded
    }

    /// The newest frame time.
    pub fn latest(&self) -> Option<Duration> {
        if self.samples.len() < self.capacity {
            self.samples.last().copied()
        } else {
            let newest = (self.next + self.capacity - 1) % self.capacity;
            Some(self.samples[newest])
        }
    }

    /// Frame times from the oldest to the newest.
    pub fn iter(&self) -> impl Iterator<Item = Duration> + '_ {
        // Before the first wrap `next` is 0: everything is in `older`.
        let (newer, older) = self.samples.split_at(self.next);
        older.iter().chain(newer).copied()
    }

    /// Frames held that took longer than `budget` (for example 6.94 ms for 144 Hz).
    pub fn frames_over(&self, budget: Duration) -> usize {
        self.samples.iter().filter(|&&t| t > budget).count()
    }

    /// Statistics over the frames held (`None` while empty).
    pub fn summary(&self) -> Option<FrameTimeSummary> {
        FrameTimeSummary::from_frame_times(self.samples.iter().copied())
    }

    /// Forgets every frame (keeps the capacity and its allocation).
    pub fn clear(&mut self) {
        self.samples.clear();
        self.next = 0;
        self.recorded = 0;
    }
}

/// Frame-time statistics of a set of frames.
///
/// Percentiles use the nearest-rank method: `pXX` is the smallest frame time such that at least
/// XX % of the frames are not slower. With fewer than 1000 frames, `p999` is the maximum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameTimeSummary {
    /// Frames summarized.
    pub frames: usize,
    /// Sum of the frame times.
    pub total: Duration,
    /// Mean frame time.
    pub mean: Duration,
    /// Median frame time.
    pub p50: Duration,
    /// 95th percentile.
    pub p95: Duration,
    /// 99th percentile.
    pub p99: Duration,
    /// 99.9th percentile.
    pub p999: Duration,
    /// Fastest frame.
    pub min: Duration,
    /// Slowest frame.
    pub max: Duration,
    /// Frames per second over the whole set: `frames / total` (0 if `total` is zero).
    pub avg_fps: f64,
    /// "1% low": the average fps of the slowest 1 % of the frames (at least one frame), that
    /// is `1 / mean` of those frame times (0 if they are all zero).
    pub low_1_percent_fps: f64,
}

impl FrameTimeSummary {
    /// Statistics of `frame_times` in any order (`None` if empty). Sorts a copy.
    pub fn from_frame_times(frame_times: impl IntoIterator<Item = Duration>) -> Option<Self> {
        let mut sorted: Vec<Duration> = frame_times.into_iter().collect();
        sorted.sort_unstable();
        Self::from_sorted(&sorted)
    }

    /// Statistics of frame times already sorted ascending (`None` if empty). Unsorted input
    /// gives meaningless percentiles.
    pub fn from_sorted(sorted: &[Duration]) -> Option<Self> {
        let (&min, &max) = (sorted.first()?, sorted.last()?);
        let frames = sorted.len();
        let total: Duration = sorted.iter().sum();
        let slowest = frames.div_ceil(100);
        let slow_total: Duration = sorted[frames - slowest..].iter().sum();
        let fps = |count: usize, time: Duration| {
            if time.is_zero() {
                0.0
            } else {
                count as f64 / time.as_secs_f64()
            }
        };
        Some(Self {
            frames,
            total,
            mean: mean(total, frames),
            p50: percentile(sorted, 50.0)?,
            p95: percentile(sorted, 95.0)?,
            p99: percentile(sorted, 99.0)?,
            p999: percentile(sorted, 99.9)?,
            min,
            max,
            avg_fps: fps(frames, total),
            low_1_percent_fps: fps(slowest, slow_total),
        })
    }
}

/// Nearest-rank percentile `p` (0 to 100) of frame times sorted ascending: the value at rank
/// `ceil(p / 100 * n)` (rank 1 for `p = 0`). `None` if `sorted` is empty or `p` is not in
/// `0..=100`.
pub fn percentile(sorted: &[Duration], p: f64) -> Option<Duration> {
    if sorted.is_empty() || !(0.0..=100.0).contains(&p) {
        return None;
    }
    // `p * n / 100` is a multiple of 1e-5 for any `p` with up to three decimals, so removing
    // 1e-6 before `ceil` only cancels floating-point noise (99.9 * 1000 / 100 = 999.0000000001).
    let rank = (p * sorted.len() as f64 / 100.0 - RANK_EPSILON).ceil() as usize;
    sorted.get(rank.clamp(1, sorted.len()) - 1).copied()
}

/// See [`percentile`].
const RANK_EPSILON: f64 = 1e-6;

/// `total / frames`, rounded down to the nanosecond.
fn mean(total: Duration, frames: usize) -> Duration {
    let nanos = total.as_nanos() / frames.max(1) as u128;
    Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}
