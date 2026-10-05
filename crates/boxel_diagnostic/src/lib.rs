//! Frame statistics and a shared, named diagnostics board that other crates fill
//! (`"gpu"`, `"render"`, `"world"`) and [`LogDiagnosticsPlugin`] prints periodically.
// SKELETON: remove this attribute when the crate is fully implemented (owner: WP1).
#![allow(
    unused_variables,
    unused_mut,
    dead_code,
    clippy::todo,
    reason = "skeleton stubs"
)]

use boxel_app::{App, Plugin};
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

/// Inserts [`FrameStats`] and [`DiagnosticsBoard`]; updates `FrameStats` in `Last`.
/// Requires `boxel_app::TimePlugin`.
#[derive(Default)]
pub struct FrameStatsPlugin;

impl Plugin for FrameStatsPlugin {
    fn build(&self, app: &mut App) {
        todo!("WP1")
    }
}

/// Logs `fps`, frame times and every [`DiagnosticsBoard`] line at info level every `interval`.
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
        todo!("WP1")
    }
}
