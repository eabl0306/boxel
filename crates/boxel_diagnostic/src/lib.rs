//! Frame statistics and a shared, named diagnostics board that other crates fill
//! (`"gpu"`, `"render"`, `"world"`) and [`LogDiagnosticsPlugin`] prints periodically.
//!
//! [`FrameStatsPlugin`] keeps two views of the frame time: [`FrameStats`] (last, moving
//! average, worst since the last log) and [`FrameTimeHistory`] (the last frames, for
//! percentiles, the worst frames and the "1% low" through [`FrameTimeSummary`]).

mod history;

pub use history::{FrameTimeHistory, FrameTimeSummary, percentile};

use boxel_app::{App, Last, Plugin, Time};
use boxel_ecs::prelude::*;
use std::collections::BTreeMap;
use std::time::Duration;

/// Frame timing derived from `boxel_app::Time`, updated in `Last`.
#[derive(Resource, Debug, Clone, Default)]
pub struct FrameStats {
    /// Frames counted.
    pub frame: u64,
    /// Last frame duration.
    pub last_frame_time: Duration,
    /// Exponential moving average (alpha 0.1) of the frame time.
    pub avg_frame_time: Duration,
    /// `1 / avg_frame_time`.
    pub fps: f32,
    /// Worst frame time since the last diagnostics log.
    pub worst_frame_time: Duration,
}

/// Named key/value lines shown by [`LogDiagnosticsPlugin`]. Keys are stable identifiers.
#[derive(Resource, Debug, Clone, Default)]
pub struct DiagnosticsBoard {
    entries: BTreeMap<&'static str, String>,
}

impl DiagnosticsBoard {
    /// Sets (replaces) the line for `key`.
    pub fn set(&mut self, key: &'static str, value: impl Into<String>) {
        self.entries.insert(key, value.into());
    }
    /// Reads the line for `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries.get(key).map(String::as_str)
    }
    /// All lines, sorted by key.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.entries.iter().map(|(k, v)| (*k, v.as_str()))
    }
}

/// Inserts [`FrameStats`], [`FrameTimeHistory`] (unless the app inserted its own) and
/// [`DiagnosticsBoard`]; updates the first two in `Last`. Requires `boxel_app::TimePlugin`.
///
/// Per frame: `frame += 1`, `last_frame_time = delta`, `avg_frame_time` is an exponential moving
/// average (`avg * 0.9 + delta * 0.1`) seeded with the first non-zero delta (the first update of
/// `Time` always has delta 0), `fps = 1 / avg` (0 while there is no average) and
/// `worst_frame_time = max(worst, delta)` (reset by [`LogDiagnosticsPlugin`] after each log).
/// From the second update of `Time` on, `Time::real_delta` is pushed to the history (the first
/// update has no previous one to measure from).
#[derive(Default)]
pub struct FrameStatsPlugin;

/// The [`FrameStats`] update system (ordering anchor for the diagnostics logger).
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
struct FrameStatsSystems;

/// EMA weight of the newest frame.
const AVG_ALPHA: f64 = 0.1;

impl Plugin for FrameStatsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FrameStats>()
            .init_resource::<FrameTimeHistory>()
            .init_resource::<DiagnosticsBoard>()
            .add_systems(Last, update_frame_stats.in_set(FrameStatsSystems));
    }

    fn finish(&self, app: &mut App) {
        assert!(
            app.world().contains_resource::<Time>(),
            "FrameStatsPlugin requires boxel_app::TimePlugin (the Time resource is missing)"
        );
    }
}

fn update_frame_stats(
    time: Res<Time>,
    mut stats: ResMut<FrameStats>,
    mut history: ResMut<FrameTimeHistory>,
) {
    if time.frame_count() > 1 {
        history.push(time.real_delta());
    }
    let delta = time.delta();
    stats.frame += 1;
    stats.last_frame_time = delta;
    stats.avg_frame_time = if stats.frame == 1 || stats.avg_frame_time.is_zero() {
        delta
    } else {
        Duration::from_secs_f64(
            stats.avg_frame_time.as_secs_f64() * (1.0 - AVG_ALPHA)
                + delta.as_secs_f64() * AVG_ALPHA,
        )
    };
    stats.fps = if stats.avg_frame_time.is_zero() {
        0.0
    } else {
        1.0 / stats.avg_frame_time.as_secs_f32()
    };
    stats.worst_frame_time = stats.worst_frame_time.max(delta);
}

/// Logs `fps`, frame times and every [`DiagnosticsBoard`] line at info level every `interval`.
/// Requires [`FrameStatsPlugin`]. The timer sums `Time::delta`, so it follows the app clock.
/// Each log resets [`FrameStats::worst_frame_time`].
#[derive(Clone, Debug)]
pub struct LogDiagnosticsPlugin {
    /// Log period.
    pub interval: Duration,
}

impl Default for LogDiagnosticsPlugin {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(2),
        }
    }
}

impl Plugin for LogDiagnosticsPlugin {
    fn build(&self, app: &mut App) {
        let interval = self.interval;
        let log_system = move |time: Res<Time>,
                               mut since_last: Local<Duration>,
                               mut stats: ResMut<FrameStats>,
                               board: Res<DiagnosticsBoard>| {
            *since_last += time.delta();
            if *since_last >= interval {
                *since_last = Duration::ZERO;
                log_diagnostics(&mut stats, &board);
            }
        };
        app.add_systems(Last, log_system.after(FrameStatsSystems));
    }

    fn finish(&self, app: &mut App) {
        assert!(
            app.world().contains_resource::<FrameStats>()
                && app.world().contains_resource::<DiagnosticsBoard>(),
            "LogDiagnosticsPlugin requires FrameStatsPlugin (FrameStats/DiagnosticsBoard missing)"
        );
    }
}

/// Logs the frame line and every board line, then resets `worst_frame_time`.
fn log_diagnostics(stats: &mut FrameStats, board: &DiagnosticsBoard) {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    tracing::info!(
        "fps={:.0} frame={:.2}ms avg={:.2}ms worst={:.2}ms",
        stats.fps,
        ms(stats.last_frame_time),
        ms(stats.avg_frame_time),
        ms(stats.worst_frame_time)
    );
    for (key, value) in board.iter() {
        tracing::info!("{key}: {value}");
    }
    stats.worst_frame_time = Duration::ZERO;
}
