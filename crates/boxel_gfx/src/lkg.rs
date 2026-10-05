use crate::{AdapterSummary, Tier, one_line};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// File format version written by this crate.
const FILE_VERSION: u32 = 2;
/// Entries kept in the file (the oldest `updated_unix` is dropped first).
const MAX_ENTRIES: usize = 64;

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
    ///
    /// `app_name` characters outside `[A-Za-z0-9._-]` become `_` (it is a single directory).
    /// `None` when the base directory is unknown (no `HOME`/`APPDATA`).
    pub fn default_path(app_name: &str) -> Option<PathBuf> {
        default_path_with(app_name, &|key| std::env::var_os(key).map(PathBuf::from))
    }

    /// Loads `path` (missing file = empty store; corrupt or version-1 file = empty store + `Err`
    /// to log). Crash detection: every `Pending` entry written by another pid more than
    /// [`PENDING_GRACE_SECS`] ago gets `strikes += 1` and becomes `Bad` if
    /// `strikes >= STRIKES_FOR_BAD`, else `Suspect`; then saves.
    ///
    /// A file that exists but cannot be read (permissions, I/O) gives an empty, read-only store
    /// plus `LkgError::Io`: a file that could not be read is never overwritten. A file that was
    /// read but is not UTF-8 is corrupt (`LkgError::Parse`, replaced).
    pub fn open(path: Option<PathBuf>, build: String) -> (LkgStore, Option<LkgError>) {
        let mut store = LkgStore {
            path,
            build,
            read_only: false,
            data: LastKnownGood {
                version: FILE_VERSION,
                entries: Vec::new(),
            },
        };
        let Some(path) = store.path.clone() else {
            return (store, None);
        };
        let shown = path.display().to_string();
        let mut error = None;
        let mut dirty = false;
        // Raw bytes first: only a failed read is an I/O error; bad contents (including bytes
        // that are not UTF-8) are a parse error and the file is replaced.
        match std::fs::read(&path) {
            Ok(bytes) => match parse_bytes(&bytes) {
                Ok(data) => store.data = data,
                Err(message) => {
                    // Replaced by an empty file below.
                    dirty = true;
                    error = Some(LkgError::Parse {
                        path: shown.clone(),
                        message,
                    });
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                // Unreadable: never overwrite what we could not read.
                store.read_only = true;
                return (
                    store,
                    Some(LkgError::Io {
                        path: shown,
                        source,
                    }),
                );
            }
        }
        dirty |= store.detect_crashes(std::process::id(), now_unix());
        dirty |= store.prune();
        if dirty
            && let Err(e) = store.save()
            && error.is_none()
        {
            error = Some(e);
        }
        (store, error)
    }

    /// Atomic save (write `*.tmp`, then rename). No-op without a path or when `read_only`.
    pub fn save(&self) -> Result<(), LkgError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if self.read_only {
            return Ok(());
        }
        let io = |source| LkgError::Io {
            path: path.display().to_string(),
            source,
        };
        let text = toml::to_string(&FileRef {
            version: FILE_VERSION,
            entries: &self.data.entries,
        })
        .map_err(|e| LkgError::Parse {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".tmp");
        let tmp = path.with_file_name(tmp_name);
        let written = std::fs::File::create(&tmp).and_then(|mut file| {
            file.write_all(text.as_bytes())?;
            file.sync_all()
        });
        let result = written.and_then(|()| std::fs::rename(&tmp, path));
        if result.is_err() {
            // Best effort: never leave a stray `*.tmp` next to the file.
            let _ = std::fs::remove_file(&tmp);
        }
        result.map_err(io)
    }

    /// Status of `(adapter, tier)` for the running versions and build, if recorded.
    pub fn status(&self, adapter: &AdapterSummary, tier: Tier) -> Option<LkgStatus> {
        self.find(adapter, tier)
            .map(|i| self.data.entries[i].status)
    }

    /// Before `request_device`: marks the pair `Pending` with this pid, remembering the previous
    /// status, and saves. Errors are logged, never returned (LKG must never block startup).
    pub fn begin_attempt(&mut self, adapter: &AdapterSummary, tier: Tier) {
        let pid = std::process::id();
        let i = self.entry(adapter, tier, LkgStatus::Pending);
        let e = &mut self.data.entries[i];
        // A fresh entry has no previous status; a `Pending` one (a concurrent instance, or this
        // process trying the pair again) keeps the status it already remembered.
        if e.status != LkgStatus::Pending {
            e.previous = Some(e.status);
        }
        e.status = LkgStatus::Pending;
        e.pid = pid;
        e.updated_unix = now_unix();
        self.save_logged();
    }

    /// After the first presented frame: `Good`, `strikes = 0`, saves.
    pub fn confirm_good(&mut self, adapter: &AdapterSummary, tier: Tier) {
        let i = self.entry(adapter, tier, LkgStatus::Good);
        let e = &mut self.data.entries[i];
        e.status = LkgStatus::Good;
        e.strikes = 0;
        e.pid = 0;
        e.previous = None;
        e.reason = None;
        e.updated_unix = now_unix();
        self.save_logged();
    }

    /// Restores every `Pending` entry of THIS process to its previous status (or removes it if
    /// it had none) and saves. Called after an in-process selection failure, on clean exit
    /// (`Shutdown` schedule) and by the panic hook ([`install_lkg_panic_hook`]).
    pub fn cancel_pending(&mut self) {
        if self.restore_pending_of(std::process::id()) {
            self.save_logged();
        }
    }

    /// Adds a strike without a crash (second device loss of a pair in one session).
    pub fn add_strike(&mut self, adapter: &AdapterSummary, tier: Tier, reason: String) {
        let i = self.entry(adapter, tier, LkgStatus::Good);
        strike(&mut self.data.entries[i], reason, now_unix());
        self.save_logged();
    }

    /// Removes every entry and saves (`gpu_info --clear-lkg`).
    pub fn clear(&mut self) {
        self.data.entries.clear();
        self.save_logged();
    }

    /// Store backed by `path` with an empty file (tests).
    pub fn with_path(path: &Path) -> LkgStore {
        LkgStore {
            path: Some(path.to_path_buf()),
            build: build_fingerprint(),
            read_only: false,
            data: LastKnownGood {
                version: FILE_VERSION,
                entries: Vec::new(),
            },
        }
    }

    /// True if `e` applies to the running process (same adapter key, tier, versions, build).
    fn applies(&self, e: &LkgEntry, adapter: &AdapterSummary, tier: Tier) -> bool {
        e.tier == tier
            && e.boxel_version == env!("CARGO_PKG_VERSION")
            && e.wgpu_version == WGPU_VERSION
            && e.build == self.build
            && e.adapter.key() == adapter.key()
    }

    /// Index of the entry that applies to `(adapter, tier)`.
    fn find(&self, adapter: &AdapterSummary, tier: Tier) -> Option<usize> {
        self.data
            .entries
            .iter()
            .position(|e| self.applies(e, adapter, tier))
    }

    /// Index of the entry for `(adapter, tier)`, created with `status` if missing (the oldest
    /// entries are dropped beyond [`MAX_ENTRIES`]).
    fn entry(&mut self, adapter: &AdapterSummary, tier: Tier, status: LkgStatus) -> usize {
        if let Some(i) = self.find(adapter, tier) {
            return i;
        }
        self.prune_to(MAX_ENTRIES - 1);
        self.data.entries.push(LkgEntry {
            adapter: adapter.clone(),
            tier,
            status,
            strikes: 0,
            boxel_version: env!("CARGO_PKG_VERSION").to_string(),
            wgpu_version: WGPU_VERSION.to_string(),
            build: self.build.clone(),
            pid: 0,
            previous: None,
            updated_unix: now_unix(),
            reason: None,
        });
        self.data.entries.len() - 1
    }

    /// Strikes every `Pending` entry left by another pid more than [`PENDING_GRACE_SECS`] ago
    /// (that process died during GPU bring-up). Returns whether anything changed.
    fn detect_crashes(&mut self, pid: u32, now: u64) -> bool {
        let mut changed = false;
        for e in &mut self.data.entries {
            if e.status == LkgStatus::Pending
                && e.pid != pid
                && now.saturating_sub(e.updated_unix) > PENDING_GRACE_SECS
            {
                let reason = format!("process {} died during GPU bring-up", e.pid);
                strike(e, reason, now);
                changed = true;
            }
        }
        changed
    }

    /// Restores `pid`'s `Pending` entries. Returns whether anything changed.
    fn restore_pending_of(&mut self, pid: u32) -> bool {
        let before = self.data.entries.len();
        let mut changed = false;
        self.data.entries.retain_mut(|e| {
            if e.status != LkgStatus::Pending || e.pid != pid {
                return true;
            }
            changed = true;
            match e.previous.take() {
                Some(previous) => {
                    e.status = previous;
                    e.pid = 0;
                    true
                }
                None => false,
            }
        });
        changed || self.data.entries.len() != before
    }

    /// Keeps at most [`MAX_ENTRIES`]. Returns whether anything was dropped.
    fn prune(&mut self) -> bool {
        self.prune_to(MAX_ENTRIES)
    }

    /// Drops the oldest entries until at most `max` remain.
    fn prune_to(&mut self, max: usize) -> bool {
        let mut dropped = false;
        while self.data.entries.len() > max {
            let oldest = self
                .data
                .entries
                .iter()
                .enumerate()
                .min_by_key(|(_, e)| e.updated_unix)
                .map_or(0, |(i, _)| i);
            self.data.entries.remove(oldest);
            dropped = true;
        }
        dropped
    }

    /// [`LkgStore::save`], errors logged (LKG must never block startup).
    fn save_logged(&self) {
        if let Err(e) = self.save() {
            tracing::warn!("{e}");
        }
    }
}

/// Borrowed view of [`LastKnownGood`] for saving.
#[derive(Serialize)]
struct FileRef<'a> {
    version: u32,
    entries: &'a [LkgEntry],
}

/// One strike: `Suspect`, or `Bad` at [`STRIKES_FOR_BAD`].
fn strike(e: &mut LkgEntry, reason: String, now: u64) {
    e.strikes += 1;
    e.status = if e.strikes >= STRIKES_FOR_BAD {
        LkgStatus::Bad
    } else {
        LkgStatus::Suspect
    };
    e.pid = 0;
    e.previous = None;
    e.reason = Some(reason);
    e.updated_unix = now;
}

/// Parses the raw bytes of a file (they must be UTF-8 TOML).
fn parse_bytes(bytes: &[u8]) -> Result<LastKnownGood, String> {
    let text = std::str::from_utf8(bytes).map_err(|e| format!("not UTF-8: {e}"))?;
    parse_file(text)
}

/// Parses a file; only version 2 is accepted.
fn parse_file(text: &str) -> Result<LastKnownGood, String> {
    let table: toml::Table = toml::from_str(text).map_err(|e| one_line(&e.to_string()))?;
    match table.get("version").and_then(toml::Value::as_integer) {
        Some(v) if v == i64::from(FILE_VERSION) => {}
        Some(v) => return Err(format!("unsupported version {v} (expected {FILE_VERSION})")),
        None => return Err("missing `version`".to_string()),
    }
    toml::Value::Table(table)
        .try_into::<LastKnownGood>()
        .map_err(|e| one_line(&e.to_string()))
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// [`LkgStore::default_path`] with an injected environment lookup.
fn default_path_with(app_name: &str, var: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    let app: String = app_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let app = match app.as_str() {
        "" | "." | ".." => "_".to_string(),
        _ => app,
    };
    let non_empty = |key: &str| var(key).filter(|p| !p.as_os_str().is_empty());
    let base = if cfg!(windows) {
        non_empty("APPDATA")?
    } else if cfg!(target_os = "macos") {
        non_empty("HOME")?
            .join("Library")
            .join("Application Support")
    } else {
        non_empty("XDG_CONFIG_HOME")
            .filter(|p| p.is_absolute())
            .or_else(|| non_empty("HOME").map(|h| h.join(".config")))?
    };
    Some(base.join("boxel").join(app).join("gpu-lkg.toml"))
}

/// `"{CARGO_PKG_VERSION}+{mtime}"` where `mtime` is the modification time of
/// `std::env::current_exe()` in Unix seconds (`"{CARGO_PKG_VERSION}+0"` if unavailable). No
/// build script: every rebuild of the app starts with a clean LKG history, so fixing a bug
/// that killed the process during bring-up clears its strikes.
pub fn build_fingerprint() -> String {
    let mtime = std::env::current_exe()
        .and_then(std::fs::metadata)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    format!("{}+{mtime}", env!("CARGO_PKG_VERSION"))
}

/// Installs (once per process; later calls are no-ops) a panic hook that runs the previous hook
/// and then restores this process' `Pending` entries in `path` (a Rust panic is not a driver
/// crash). With `panic = "abort"` the hook still runs before the abort.
pub fn install_lkg_panic_hook(path: PathBuf) {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    if INSTALLED.set(()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        previous(info);
        // Errors are ignored: we are already panicking.
        let (mut store, _) = LkgStore::open(Some(path.clone()), build_fingerprint());
        if store.restore_pending_of(std::process::id()) {
            let _ = store.save();
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_layout() {
        let var = |key: &str| match key {
            "XDG_CONFIG_HOME" => Some(PathBuf::from("/xdg")),
            "HOME" => Some(PathBuf::from("/home/u")),
            "APPDATA" => Some(PathBuf::from("C:/Users/u/AppData/Roaming")),
            _ => None,
        };
        let p = default_path_with("voxel_world", &var).unwrap();
        assert!(
            p.ends_with("boxel/voxel_world/gpu-lkg.toml"),
            "{}",
            p.display()
        );
        if cfg!(all(unix, not(target_os = "macos"))) {
            assert_eq!(p, PathBuf::from("/xdg/boxel/voxel_world/gpu-lkg.toml"));
            // Relative or empty XDG_CONFIG_HOME falls back to ~/.config.
            let var = |key: &str| match key {
                "XDG_CONFIG_HOME" => Some(PathBuf::from("rel")),
                "HOME" => Some(PathBuf::from("/home/u")),
                _ => None,
            };
            assert_eq!(
                default_path_with("a/b", &var).unwrap(),
                PathBuf::from("/home/u/.config/boxel/a_b/gpu-lkg.toml")
            );
            assert_eq!(default_path_with("x", &|_| None), None);
        }
    }

    #[test]
    fn version_check() {
        assert!(parse_file("version = 2\nentries = []\n").is_ok());
        assert!(parse_file("version = 1\nentries = []\n").is_err());
        assert!(parse_file("entries = []\n").is_err());
        assert!(parse_file("version = 2\n").is_err());
        assert!(parse_bytes(b"version = 2\nentries = []\n").is_ok());
        let err = parse_bytes(b"version = 2\nentries = []\n\xff\xfe").unwrap_err();
        assert!(err.starts_with("not UTF-8"), "{err}");
    }
}
