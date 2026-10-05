//! Last-known-good store: crash-only semantics (ADR 0002), file format, atomic
//! save. Files live under `CARGO_TARGET_TMPDIR`; the user's config directory is never touched.
use boxel_gfx::{
    AdapterSummary, DeviceClass, LastKnownGood, LkgEntry, LkgError, LkgStatus, LkgStore,
    PENDING_GRACE_SECS, STRIKES_FOR_BAD, Tier, WGPU_VERSION, build_fingerprint,
    install_lkg_panic_hook,
};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const BUILD: &str = "0.1.0-dev+12345";
/// A pid that is not this test process.
const OTHER_PID: u32 = u32::MAX - 7;

fn file(test: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("lkg")
        .join(test);
    let _ = std::fs::remove_dir_all(&dir);
    dir.join("boxel").join("gpu-lkg.toml")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn gpu(name: &str) -> AdapterSummary {
    AdapterSummary {
        name: name.to_string(),
        backend: "Vulkan".to_string(),
        class: DeviceClass::Discrete,
        vendor: 0x10de,
        device: 0x2dd8,
        driver: "NVIDIA".to_string(),
        driver_info: "595.91.07".to_string(),
    }
}

fn entry(adapter: &AdapterSummary, tier: Tier, status: LkgStatus) -> LkgEntry {
    LkgEntry {
        adapter: adapter.clone(),
        tier,
        status,
        strikes: 0,
        boxel_version: env!("CARGO_PKG_VERSION").to_string(),
        wgpu_version: WGPU_VERSION.to_string(),
        build: BUILD.to_string(),
        pid: 0,
        previous: None,
        updated_unix: now(),
        reason: None,
    }
}

/// Writes `entries` as a valid version-2 file.
fn write(path: &Path, entries: Vec<LkgEntry>) {
    let mut store = LkgStore::with_path(path);
    store.build = BUILD.to_string();
    store.data.entries = entries;
    store.save().expect("save");
}

fn open(path: &Path) -> LkgStore {
    let (store, error) = LkgStore::open(Some(path.to_path_buf()), BUILD.to_string());
    assert!(error.is_none(), "{error:?}");
    store
}

#[test]
fn stale_pending_of_another_pid_becomes_suspect_then_bad() {
    let path = file("stale_pending");
    let a = gpu("RTX");
    let stale = |strikes| LkgEntry {
        status: LkgStatus::Pending,
        pid: OTHER_PID,
        previous: Some(LkgStatus::Good),
        strikes,
        updated_unix: now() - PENDING_GRACE_SECS - 1,
        ..entry(&a, Tier::High, LkgStatus::Pending)
    };
    write(&path, vec![stale(0)]);
    let store = open(&path);
    assert_eq!(store.status(&a, Tier::High), Some(LkgStatus::Suspect));
    let e = &store.data.entries[0];
    assert_eq!((e.strikes, e.pid, e.previous), (1, 0, None));
    assert!(e.reason.as_deref().unwrap_or("").contains("died"), "{e:?}");
    // Saved by `open`: a second reader sees the strike without re-applying it.
    assert_eq!(open(&path).data.entries[0].strikes, 1);

    // The same pair crashes again: second strike, Bad.
    write(&path, vec![stale(1)]);
    let store = open(&path);
    assert_eq!(STRIKES_FOR_BAD, 2);
    assert_eq!(store.status(&a, Tier::High), Some(LkgStatus::Bad));
    assert_eq!(store.data.entries[0].strikes, 2);
}

#[test]
fn fresh_pending_of_another_pid_is_left_alone() {
    let path = file("fresh_pending");
    let a = gpu("RTX");
    let fresh = LkgEntry {
        pid: OTHER_PID,
        updated_unix: now() - 2,
        ..entry(&a, Tier::High, LkgStatus::Pending)
    };
    write(&path, vec![fresh.clone()]);
    let store = open(&path);
    assert_eq!(store.data.entries, vec![fresh]);
    assert_eq!(store.status(&a, Tier::High), Some(LkgStatus::Pending));
}

#[test]
fn cancel_pending_restores_the_previous_status() {
    let path = file("cancel_pending");
    let (a, b) = (gpu("RTX"), gpu("RADV"));
    write(&path, vec![entry(&a, Tier::High, LkgStatus::Good)]);
    let mut store = open(&path);
    store.begin_attempt(&a, Tier::High);
    store.begin_attempt(&b, Tier::Mid);
    assert_eq!(store.status(&a, Tier::High), Some(LkgStatus::Pending));
    // `begin_attempt` saves: a crash right now leaves both pairs pending in the file.
    let on_disk = open(&path);
    assert_eq!(on_disk.status(&a, Tier::High), Some(LkgStatus::Pending));
    assert_eq!(on_disk.status(&b, Tier::Mid), Some(LkgStatus::Pending));
    assert_eq!(on_disk.data.entries[0].pid, std::process::id());
    assert_eq!(on_disk.data.entries[0].previous, Some(LkgStatus::Good));

    store.cancel_pending();
    assert_eq!(store.status(&a, Tier::High), Some(LkgStatus::Good));
    assert_eq!(
        store.status(&b, Tier::Mid),
        None,
        "no previous status: removed"
    );
    let on_disk = open(&path);
    assert_eq!(on_disk.data.entries.len(), 1);
    assert_eq!(on_disk.status(&a, Tier::High), Some(LkgStatus::Good));
    assert_eq!(on_disk.data.entries[0].pid, 0);
}

#[test]
fn cancel_pending_leaves_other_processes_alone() {
    let path = file("cancel_other");
    let a = gpu("RTX");
    let theirs = LkgEntry {
        pid: OTHER_PID,
        updated_unix: now(),
        ..entry(&a, Tier::Low, LkgStatus::Pending)
    };
    write(&path, vec![theirs.clone()]);
    let mut store = open(&path);
    store.cancel_pending();
    assert_eq!(store.data.entries, vec![theirs]);
}

#[test]
fn confirm_good_resets_strikes() {
    let path = file("confirm_good");
    let a = gpu("RTX");
    let suspect = LkgEntry {
        strikes: 1,
        reason: Some("crash".to_string()),
        ..entry(&a, Tier::High, LkgStatus::Suspect)
    };
    write(&path, vec![suspect]);
    let mut store = open(&path);
    store.begin_attempt(&a, Tier::High);
    store.confirm_good(&a, Tier::High);
    let e = &open(&path).data.entries[0];
    assert_eq!(
        (e.status, e.strikes, e.pid, e.previous, e.reason.clone()),
        (LkgStatus::Good, 0, 0, None, None)
    );
    // A pair confirmed without a prior attempt is created as Good.
    let b = gpu("RADV");
    store.confirm_good(&b, Tier::Mid);
    assert_eq!(open(&path).status(&b, Tier::Mid), Some(LkgStatus::Good));
}

#[test]
fn add_strike_marks_suspect_then_bad() {
    let path = file("add_strike");
    let a = gpu("RTX");
    let mut store = open(&path);
    store.confirm_good(&a, Tier::High);
    store.add_strike(&a, Tier::High, "device lost twice".to_string());
    assert_eq!(store.status(&a, Tier::High), Some(LkgStatus::Suspect));
    store.add_strike(&a, Tier::High, "device lost twice".to_string());
    let on_disk = open(&path);
    assert_eq!(on_disk.status(&a, Tier::High), Some(LkgStatus::Bad));
    assert_eq!(
        on_disk.data.entries[0].reason.as_deref(),
        Some("device lost twice")
    );
}

#[test]
fn read_only_never_writes() {
    let path = file("read_only");
    let a = gpu("RTX");
    let mut store = open(&path);
    store.read_only = true;
    store.begin_attempt(&a, Tier::High);
    store.confirm_good(&a, Tier::High);
    store.add_strike(&a, Tier::High, "x".to_string());
    store.cancel_pending();
    store.clear();
    store.save().unwrap();
    assert!(!path.exists(), "a read-only store must not create the file");

    write(&path, vec![entry(&a, Tier::High, LkgStatus::Good)]);
    let before = std::fs::read(&path).unwrap();
    let mut store = open(&path);
    store.read_only = true;
    store.begin_attempt(&a, Tier::High);
    store.clear();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn entries_of_other_builds_and_versions_are_ignored_but_kept() {
    let path = file("other_build");
    let a = gpu("RTX");
    let other_build = LkgEntry {
        build: "0.1.0-dev+999".to_string(),
        ..entry(&a, Tier::High, LkgStatus::Bad)
    };
    let other_wgpu = LkgEntry {
        wgpu_version: "29.0.0".to_string(),
        ..entry(&a, Tier::Mid, LkgStatus::Bad)
    };
    let mut new_driver = gpu("RTX");
    new_driver.driver_info = "600.0".to_string();
    write(
        &path,
        vec![
            other_build,
            other_wgpu,
            entry(&new_driver, Tier::Low, LkgStatus::Bad),
        ],
    );
    let mut store = open(&path);
    for tier in Tier::DESCENDING {
        assert_eq!(store.status(&a, tier), None, "{tier}");
    }
    assert_eq!(store.status(&new_driver, Tier::Low), Some(LkgStatus::Bad));
    store.begin_attempt(&a, Tier::High);
    store.cancel_pending();
    assert_eq!(
        open(&path).data.entries.len(),
        3,
        "foreign entries are kept"
    );
}

#[test]
fn corrupt_and_version_1_files_give_an_empty_store_and_an_error() {
    let path = file("corrupt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for bytes in [
        &b"this is not toml [[["[..],
        &b"version = 1\n[[entries]]\nadapter = \"x\"\ntier = \"high\"\nstatus = \"good\"\n"[..],
        &b"version = 2\nentries = 3\n"[..],
        // Not UTF-8: corrupt contents, not an I/O failure.
        &b"version = 2\nentries = []\n\xff\xfe"[..],
    ] {
        let text = String::from_utf8_lossy(bytes);
        std::fs::write(&path, bytes).unwrap();
        let (store, error) = LkgStore::open(Some(path.clone()), BUILD.to_string());
        assert!(store.data.entries.is_empty());
        assert!(!store.read_only, "{text}");
        assert!(
            matches!(error, Some(LkgError::Parse { .. })),
            "{text}: {error:?}"
        );
        // The broken file was replaced by an empty, valid one.
        let reopened = open(&path);
        assert_eq!(reopened.data.version, 2);
        assert!(reopened.data.entries.is_empty());
    }
}

#[test]
fn unreadable_file_gives_a_read_only_store_and_is_never_overwritten() {
    // A directory where the file should be: the read fails with an I/O error that is not
    // "not found" (works the same as root, unlike a permission test).
    let path = file("unreadable");
    std::fs::create_dir_all(&path).unwrap();
    let (mut store, error) = LkgStore::open(Some(path.clone()), BUILD.to_string());
    assert!(store.read_only);
    assert!(matches!(error, Some(LkgError::Io { .. })), "{error:?}");
    store.confirm_good(&gpu("RTX"), Tier::High);
    store.clear();
    assert!(store.save().is_ok(), "read-only save is a silent no-op");
    assert!(path.is_dir(), "never replaced");
    // A forced save over it (what `gpu_info --clear-lkg` does) reports the failure and leaves
    // no temporary file behind.
    let forced = LkgStore::with_path(&path).save();
    assert!(matches!(forced, Err(LkgError::Io { .. })), "{forced:?}");
    assert!(!path.with_file_name("gpu-lkg.toml.tmp").exists());
}

#[test]
fn missing_file_and_no_path() {
    let path = file("missing");
    let store = open(&path);
    assert!(store.data.entries.is_empty());
    assert!(!path.exists(), "opening never creates the file by itself");
    let (mut memory, error) = LkgStore::open(None, BUILD.to_string());
    assert!(error.is_none());
    memory.begin_attempt(&gpu("RTX"), Tier::High);
    assert_eq!(
        memory.status(&gpu("RTX"), Tier::High),
        Some(LkgStatus::Pending)
    );
    memory.save().unwrap();
}

#[test]
fn save_is_atomic_and_creates_directories() {
    let path = file("atomic");
    let tmp = path.with_file_name("gpu-lkg.toml.tmp");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    // A stale temporary file from an interrupted save is simply replaced.
    std::fs::write(&tmp, "garbage").unwrap();
    write(&path, vec![entry(&gpu("RTX"), Tier::High, LkgStatus::Good)]);
    assert!(
        !tmp.exists(),
        "the temporary file is renamed over the target"
    );
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("version = 2"), "{text}");
    let parsed: LastKnownGood = toml::from_str(&text).unwrap();
    assert_eq!(parsed.entries.len(), 1);
}

#[test]
fn keeps_at_most_64_entries_dropping_the_oldest() {
    let path = file("prune");
    let mut entries: Vec<LkgEntry> = (0..64)
        .map(|i| LkgEntry {
            updated_unix: 1_000 + i,
            ..entry(&gpu(&format!("gpu {i}")), Tier::Low, LkgStatus::Good)
        })
        .collect();
    entries.swap(0, 40);
    write(&path, entries);
    let mut store = open(&path);
    store.confirm_good(&gpu("new"), Tier::High);
    let on_disk = open(&path);
    assert_eq!(on_disk.data.entries.len(), 64);
    assert_eq!(
        on_disk.status(&gpu("gpu 0"), Tier::Low),
        None,
        "oldest dropped"
    );
    assert_eq!(
        on_disk.status(&gpu("gpu 1"), Tier::Low),
        Some(LkgStatus::Good)
    );
    assert_eq!(
        on_disk.status(&gpu("new"), Tier::High),
        Some(LkgStatus::Good)
    );
}

#[test]
fn clear_removes_everything() {
    let path = file("clear");
    write(&path, vec![entry(&gpu("RTX"), Tier::High, LkgStatus::Bad)]);
    let mut store = open(&path);
    store.clear();
    assert!(open(&path).data.entries.is_empty());
}

#[test]
fn default_path_and_fingerprint() {
    let path = LkgStore::default_path("voxel_world");
    if let Some(path) = path {
        assert!(
            path.ends_with("boxel/voxel_world/gpu-lkg.toml"),
            "{}",
            path.display()
        );
    }
    let fingerprint = build_fingerprint();
    let (version, mtime) = fingerprint.split_once('+').unwrap();
    assert_eq!(version, env!("CARGO_PKG_VERSION"));
    assert!(
        mtime.parse::<u64>().unwrap() > 0,
        "test binaries have an mtime"
    );
    assert_eq!(build_fingerprint(), fingerprint, "stable within a process");
    assert_eq!(LkgStore::with_path(&PathBuf::from("x")).build, fingerprint);
}

#[test]
fn panic_hook_restores_pending_entries_once_installed() {
    let path = file("panic_hook");
    let a = gpu("RTX");
    write(&path, vec![entry(&a, Tier::High, LkgStatus::Suspect)]);
    let mut store = open(&path);
    store.begin_attempt(&a, Tier::High);
    store.begin_attempt(&a, Tier::Mid);
    install_lkg_panic_hook(path.clone());
    // A second call is a no-op: this path must stay untouched by the hook.
    let other = file("panic_hook_second");
    let b = gpu("RADV");
    let mut second = open(&other);
    second.begin_attempt(&b, Tier::Low);
    install_lkg_panic_hook(other.clone());

    let panicked = std::thread::spawn(|| panic!("simulated Rust panic (expected by this test)"))
        .join()
        .is_err();
    assert!(panicked);
    let on_disk = open(&path);
    assert_eq!(on_disk.status(&a, Tier::High), Some(LkgStatus::Suspect));
    assert_eq!(on_disk.status(&a, Tier::Mid), None);
    assert_eq!(open(&other).status(&b, Tier::Low), Some(LkgStatus::Pending));
}
