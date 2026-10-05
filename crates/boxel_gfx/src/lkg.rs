use crate::{AdapterSummary, Tier};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// wgpu version recorded in entries (keep in sync with the `=30.0.1` pin).
pub const WGPU_VERSION: &str = "30.0.1";
/// A `Pending` entry younger than this may belong to a concurrent instance still in GPU
/// bring-up; it is left alone when the file is opened.
pub const PENDING_GRACE_SECS: u64 = 30;
/// Unexplained deaths during bring-up before a pair becomes `Bad`.
pub const STRIKES_FOR_BAD: u32 = 2;

/// Outcome recorded for an (adapter, tier) pair. LKG exists to survive crashes that cannot be
/// caught in-process (driver segfaults, hangs killed by the user). In-process failures
/// (`request_device` errors, failed smoke tests) are NOT recorded: they are re-detected every
/// start in milliseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LkgStatus {
    /// An attempt started and has not been confirmed yet (written before `request_device`,
    /// cleared by the first presented frame, a clean exit or a Rust panic).
    Pending,
    /// Presented at least one frame with this build.
    Good,
    /// One unexplained death during bring-up (or a repeated device loss). Still attempted, but
    /// ranked lower ([`crate::score`]).
    Suspect,
    /// [`STRIKES_FOR_BAD`] strikes: skipped by `select_gpu` unless named explicitly.
    Bad,
}

/// One persisted record. An entry applies only if adapter key (includes `driver_info`), tier,
/// `boxel_version`, `wgpu_version` and `build` all match the running process; other entries are
/// kept in the file but ignored (a driver update or a rebuild gives the pair a fresh start).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LkgEntry {
    /// Adapter identity.
    pub adapter: AdapterSummary,
    /// Tier.
    pub tier: Tier,
    /// Outcome.
    pub status: LkgStatus,
    /// Unexplained deaths during bring-up since the last `Good`.
    #[serde(default)]
    pub strikes: u32,
    /// `env!("CARGO_PKG_VERSION")` of `boxel_gfx`.
    pub boxel_version: String,
    /// [`WGPU_VERSION`].
    pub wgpu_version: String,
    /// [`build_fingerprint`] of the writing executable.
    #[serde(default)]
    pub build: String,
    /// Process id that wrote `Pending` (0 otherwise).
    #[serde(default)]
    pub pid: u32,
    /// Status before `Pending` (restored by [`LkgStore::cancel_pending`]).
    #[serde(default)]
    pub previous: Option<LkgStatus>,
    /// Seconds since the Unix epoch.
    pub updated_unix: u64,
    /// Optional reason (last strike or loss text).
    #[serde(default)]
    pub reason: Option<String>,
}

/// File contents (`version = 2`; version 1 files are discarded).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastKnownGood {
    /// Format version.
    pub version: u32,
    /// Records.
    pub entries: Vec<LkgEntry>,
}

/// LKG I/O errors.
#[derive(Debug, thiserror::Error)]
pub enum LkgError {
    /// Read/write failure.
    #[error("last-known-good file {path}: {source}")]
    Io {
        /// Path.
        path: String,
        /// Cause.
        source: std::io::Error,
    },
    /// Corrupt file (it is then replaced by an empty one).
    #[error("last-known-good file {path} is invalid: {message}")]
    Parse {
        /// Path.
        path: String,
        /// Message.
        message: String,
    },
}

/// A loaded LKG file plus its path (`None` = in-memory only).
#[derive(Clone, Debug, Default)]
pub struct LkgStore {
    /// Backing file.
    pub path: Option<PathBuf>,
    /// Build fingerprint of this process ([`build_fingerprint`]).
    pub build: String,
    /// No writes at all. `select_gpu` sets it when debug instance flags are active
    /// (`DEBUG`, `VALIDATION`, `GPU_BASED_VALIDATION`, `VALIDATION_INDIRECT_CALL`, from
    /// `--validation` or `WGPU_*` env vars): debug configurations fail for reasons that say
    /// nothing about the driver.
    pub read_only: bool,
    /// Contents.
    pub data: LastKnownGood,
}

impl LkgStore {
    /// Default location: `$XDG_CONFIG_HOME/boxel/<app>/gpu-lkg.toml` (fallback `~/.config`) on
    /// Linux, `%APPDATA%\boxel\<app>\gpu-lkg.toml` on Windows,
    /// `~/Library/Application Support/boxel/<app>/gpu-lkg.toml` on macOS. No extra crates.
    pub fn default_path(app_name: &str) -> Option<PathBuf> {
        todo!("WP3")
    }

    /// Loads `path` (missing file = empty store; corrupt or version-1 file = empty store + `Err`
    /// to log). Crash detection: every `Pending` entry written by another pid more than
    /// [`PENDING_GRACE_SECS`] ago gets `strikes += 1` and becomes `Bad` if
    /// `strikes >= STRIKES_FOR_BAD`, else `Suspect`; then saves.
    pub fn open(path: Option<PathBuf>, build: String) -> (LkgStore, Option<LkgError>) {
        todo!("WP3")
    }

    /// Atomic save (write `*.tmp`, then rename). No-op without a path or when `read_only`.
    pub fn save(&self) -> Result<(), LkgError> {
        todo!("WP3")
    }

    /// Status of `(adapter, tier)` for the running versions and build, if recorded.
    pub fn status(&self, adapter: &AdapterSummary, tier: Tier) -> Option<LkgStatus> {
        todo!("WP3")
    }

    /// Before `request_device`: marks the pair `Pending` with this pid, remembering the previous
    /// status, and saves. Errors are logged, never returned (LKG must never block startup).
    pub fn begin_attempt(&mut self, adapter: &AdapterSummary, tier: Tier) {
        todo!("WP3")
    }

    /// After the first presented frame: `Good`, `strikes = 0`, saves.
    pub fn confirm_good(&mut self, adapter: &AdapterSummary, tier: Tier) {
        todo!("WP3")
    }

    /// Restores every `Pending` entry of THIS process to its previous status (or removes it if
    /// it had none) and saves. Called after an in-process selection failure, on clean exit
    /// (`Shutdown` schedule) and by the panic hook ([`install_lkg_panic_hook`]).
    pub fn cancel_pending(&mut self) {
        todo!("WP3")
    }

    /// Adds a strike without a crash (second device loss of a pair in one session).
    pub fn add_strike(&mut self, adapter: &AdapterSummary, tier: Tier, reason: String) {
        todo!("WP3")
    }

    /// Removes every entry and saves (`gpu_info --clear-lkg`).
    pub fn clear(&mut self) {
        todo!("WP3")
    }

    /// Store backed by `path` with an empty file (tests).
    pub fn with_path(path: &Path) -> LkgStore {
        LkgStore {
            path: Some(path.to_path_buf()),
            build: build_fingerprint(),
            read_only: false,
            data: LastKnownGood {
                version: 2,
                entries: Vec::new(),
            },
        }
    }
}

/// `"{CARGO_PKG_VERSION}+{mtime}"` where `mtime` is the modification time of
/// `std::env::current_exe()` in Unix seconds (`"{CARGO_PKG_VERSION}+0"` if unavailable). No
/// build script: every rebuild of the app starts with a clean LKG history, so fixing a bug
/// that killed the process during bring-up clears its strikes.
pub fn build_fingerprint() -> String {
    todo!("WP3")
}

/// Installs (once per process; later calls are no-ops) a panic hook that runs the previous hook
/// and then restores this process' `Pending` entries in `path` (a Rust panic is not a driver
/// crash). With `panic = "abort"` the hook still runs before the abort.
pub fn install_lkg_panic_hook(path: PathBuf) {
    todo!("WP3")
}
