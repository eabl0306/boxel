//! Logging for boxel. One global `tracing` subscriber: fmt output filtered by `RUST_LOG`
//! (falls back to [`DEFAULT_FILTER`]), `log` records bridged (wgpu, naga and the ECS use `log`),
//! and with feature `tracy` a Tracy layer plus `profiling/profile-with-tracing` so wgpu's internal
//! scopes reach Tracy.
// SKELETON: remove this attribute when the crate is fully implemented (owner: WP1).
#![allow(
    unused_variables,
    unused_mut,
    dead_code,
    clippy::todo,
    reason = "skeleton stubs"
)]

use boxel_app::{App, Plugin};

/// Default filter when `RUST_LOG` is unset.
pub const DEFAULT_FILTER: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn";

/// Error from [`init_logging`].
#[derive(Debug, thiserror::Error)]
pub enum LogInitError {
    /// A global subscriber was already installed (not fatal; logging still works).
    #[error("a global tracing subscriber is already set")]
    AlreadySet,
    /// The filter string did not parse.
    #[error("invalid log filter: {0}")]
    BadFilter(String),
}

/// Installs the global subscriber. `RUST_LOG` (if set and valid) wins over `filter`.
/// Usable without an `App` (tools, tests, `gpu_info`).
pub fn init_logging(filter: &str) -> Result<(), LogInitError> {
    todo!("WP1")
}

/// Calls [`init_logging`] in `build`; `AlreadySet` is reported at debug level, not as an error.
#[derive(Clone, Debug)]
pub struct LogPlugin {
    /// Filter used when `RUST_LOG` is unset.
    pub filter: String,
}

impl Default for LogPlugin {
    fn default() -> Self {
        Self {
            filter: DEFAULT_FILTER.to_string(),
        }
    }
}

impl Plugin for LogPlugin {
    fn build(&self, app: &mut App) {
        todo!("WP1")
    }
}
