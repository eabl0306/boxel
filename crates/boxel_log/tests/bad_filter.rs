//! Filter handling: a bad filter is `BadFilter`, `RUST_LOG` wins when valid, `LogPlugin` falls
//! back to `DEFAULT_FILTER`. Every test re-runs itself in a child process with a controlled
//! `RUST_LOG` (no `unsafe` `set_var`), so each one also gets a fresh global subscriber.
use boxel_app::App;
use boxel_log::{DEFAULT_FILTER, LogInitError, LogPlugin, init_logging};
use std::process::Command;
use tracing::level_filters::LevelFilter;

const CHILD: &str = "BOXEL_LOG_TEST_CHILD";
const BAD: &str = "info,boxel=notalevel";

/// In the parent: runs `test` alone in a child with `RUST_LOG` set to `rust_log` (removed if
/// `None`) and returns false. In the child: returns true (run the body).
fn in_child(test: &str, rust_log: Option<&str>) -> bool {
    if std::env::var_os(CHILD).is_some() {
        return true;
    }
    let exe = std::env::current_exe().expect("test binary path");
    let mut cmd = Command::new(exe);
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, "1");
    match rust_log {
        Some(value) => cmd.env("RUST_LOG", value),
        None => cmd.env_remove("RUST_LOG"),
    };
    let out = cmd.output().expect("spawn the test binary");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "child {test} failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    false
}

#[test]
fn bad_filter_is_rejected() {
    if !in_child("bad_filter_is_rejected", None) {
        return;
    }
    match init_logging(BAD) {
        Err(LogInitError::BadFilter(message)) => assert!(message.contains(BAD), "{message}"),
        other => panic!("expected BadFilter, got {other:?}"),
    }
    // Nothing was installed: a valid filter still works.
    init_logging("debug").expect("valid filter");
    assert_eq!(LevelFilter::current(), LevelFilter::DEBUG);
}

#[test]
fn empty_rust_log_counts_as_unset() {
    if !in_child("empty_rust_log_counts_as_unset", Some("")) {
        return;
    }
    assert!(matches!(init_logging(BAD), Err(LogInitError::BadFilter(_))));
}

#[test]
fn valid_rust_log_wins_over_the_filter() {
    if !in_child("valid_rust_log_wins_over_the_filter", Some("warn")) {
        return;
    }
    init_logging(BAD).expect("RUST_LOG is used, the bad filter is never parsed");
    assert_eq!(LevelFilter::current(), LevelFilter::WARN);
}

#[test]
fn invalid_rust_log_falls_back_to_the_filter() {
    if !in_child("invalid_rust_log_falls_back_to_the_filter", Some(BAD)) {
        return;
    }
    init_logging("trace").expect("the filter argument is used");
    assert_eq!(LevelFilter::current(), LevelFilter::TRACE);
}

#[test]
fn log_plugin_falls_back_to_the_default_filter() {
    if !in_child("log_plugin_falls_back_to_the_default_filter", None) {
        return;
    }
    let mut app = App::new();
    app.add_plugins(LogPlugin {
        filter: BAD.to_string(),
    });
    // DEFAULT_FILTER is installed: its most verbose directive is `info`.
    assert!(DEFAULT_FILTER.starts_with("info,"));
    assert_eq!(LevelFilter::current(), LevelFilter::INFO);
    assert!(matches!(
        init_logging(DEFAULT_FILTER),
        Err(LogInitError::AlreadySet)
    ));
}
