//! Logging for boxel. One global `tracing` subscriber: fmt output filtered by `RUST_LOG`
//! (falls back to [`DEFAULT_FILTER`]), `log` records bridged (wgpu, naga and the ECS use `log`),
//! and with feature `tracy` a Tracy layer plus `profiling/profile-with-tracing` so wgpu's internal
//! scopes reach Tracy.

use boxel_app::{App, Plugin};
use std::io::IsTerminal;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Default filter when `RUST_LOG` is unset.
///
/// `sctk_adwaita=error`: winit's client-side decorations on Wayland (`sctk-adwaita`) warn about
/// every button-layout entry they cannot draw. GNOME's default layout `":minimize,maximize,close"`
/// has an empty left side, so every window prints `Ignoring unknown button type: ` (empty name).
/// The crate's other warnings are cosmetic too (a layout without valid buttons falls back to the
/// default one, out-of-range title pixels are skipped); its errors (settings portal timeout,
/// font failures) still show.
pub const DEFAULT_FILTER: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn,sctk_adwaita=error";

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
///
/// Output goes to stderr (ANSI colors only when stderr is a terminal), so stdout stays free for
/// the stable lines tools print. An empty `RUST_LOG` counts as unset; an invalid one is ignored
/// with a warning once the subscriber is up. `AlreadySet` also covers a `log` logger installed
/// by someone else (the `log` bridge cannot be set twice).
pub fn init_logging(filter: &str) -> Result<(), LogInitError> {
    // RUST_LOG wins when it is set (non-empty) and valid; otherwise `filter` is used.
    let mut ignored_rust_log = None;
    let from_env = match std::env::var(EnvFilter::DEFAULT_ENV) {
        Ok(value) if !value.trim().is_empty() => match EnvFilter::try_from_default_env() {
            Ok(env_filter) => Some(env_filter),
            Err(error) => {
                ignored_rust_log = Some(error.to_string());
                None
            }
        },
        _ => None,
    };
    let env_filter = match from_env {
        Some(env_filter) => env_filter,
        None => EnvFilter::try_new(filter)
            .map_err(|error| LogInitError::BadFilter(format!("\"{filter}\" ({error})")))?,
    };

    let fmt = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal());
    let subscriber = tracing_subscriber::registry().with(env_filter).with(fmt);
    #[cfg(feature = "tracy")]
    let subscriber = subscriber.with(tracing_tracy::TracyLayer::default());
    subscriber
        .try_init()
        .map_err(|_| LogInitError::AlreadySet)?;

    if let Some(error) = ignored_rust_log {
        tracing::warn!("ignoring invalid RUST_LOG ({error}); using the filter \"{filter}\"");
    }
    Ok(())
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
    fn build(&self, _app: &mut App) {
        match init_logging(&self.filter) {
            Ok(()) => {}
            Err(LogInitError::AlreadySet) => {
                tracing::debug!(
                    "LogPlugin: a global tracing subscriber is already set; keeping it"
                );
            }
            Err(error @ LogInitError::BadFilter(_)) => {
                report_bad_filter(&error);
                if let Err(fallback) = init_logging(DEFAULT_FILTER) {
                    tracing::debug!("LogPlugin: fallback filter not installed: {fallback}");
                }
            }
        }
    }
}

/// The subscriber is not installed yet when the filter is rejected, so this goes to stderr.
#[expect(
    clippy::print_stderr,
    reason = "no subscriber exists yet to report a bad filter"
)]
fn report_bad_filter(error: &LogInitError) {
    eprintln!("boxel_log: {error}; falling back to \"{DEFAULT_FILTER}\"");
}
