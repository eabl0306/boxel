//! Windowed check of `FrameLimit` with the real winit runner (no renderer): measures the update
//! rate at 60 and 144 fps, changing the limit at runtime, and checks that 0 is unlimited.
//! Fails when a measured rate is off by more than 3 %, or when the runner paced a phase by the
//! display: nothing presents here, so the redraws come at once and the limit's grid must govern
//! even when the limit matches the monitor (ADR 0017).
//!
//! That last check cannot catch a display lock that should not happen, only document it: on
//! Wayland the window is never mapped (nothing is drawn), so winit reports no monitor and no
//! refresh rate, and the redraws come at once; on X11 the gate never follows frame callbacks.
//! Either way `display_locked` stays false. The display lock itself is covered by the unit
//! tests of `RunnerCore` in `src/runner.rs`.
//!
//! Opt-in (`BOXEL_WINDOW_TESTS=1`, or `just test-window`): it opens a window on the desktop
//! (on Wayland it is never mapped, since nothing is drawn) and its timing needs a machine that
//! is not saturated. Without the variable it prints a note and passes. A watchdog thread kills
//! the process after 30 s. `harness = false`: winit needs the main thread.
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "test output")]

use boxel_app::{App, AppExit, FrameLimit, Update};
use boxel_ecs::prelude::*;
use boxel_window::{FramePacingInfo, WindowPlugin, WindowSettings, WindowState};
use std::process::ExitCode;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

/// One measurement: a limit, a warm-up and a measuring time.
struct Phase {
    max_fps: u32,
    warmup: Duration,
    measure: Duration,
}

const PHASES: [Phase; 3] = [
    Phase {
        max_fps: 60,
        warmup: Duration::from_millis(300),
        measure: Duration::from_millis(1500),
    },
    Phase {
        max_fps: 144,
        warmup: Duration::from_millis(200),
        measure: Duration::from_millis(1500),
    },
    Phase {
        max_fps: 0,
        warmup: Duration::from_millis(100),
        measure: Duration::from_millis(400),
    },
];

/// Allowed relative error of a limited rate.
const TOLERANCE: f64 = 0.03;
/// Unlimited must reach at least this many times the highest limit.
const UNLIMITED_FACTOR: f64 = 3.0;
const WATCHDOG: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct Measured {
    max_fps: u32,
    rate: f64,
    /// Shortest, mean and longest interval between updates.
    intervals: (Duration, Duration, Duration),
    occluded: bool,
    /// Updates of the phase the runner paced by the display.
    display_locked: usize,
}

#[derive(Resource, Default)]
struct Check {
    phase: usize,
    phase_start: Option<Instant>,
    /// Update times of the measuring part of the phase.
    times: Vec<Instant>,
    occluded: bool,
    display_locked: usize,
    /// The monitor's refresh rate as the runner saw it (millihertz).
    refresh: Option<u32>,
    results: Vec<Measured>,
}

fn drive(
    mut check: ResMut<Check>,
    mut limit: ResMut<FrameLimit>,
    mut state: ResMut<WindowState>,
    pacing: Res<FramePacingInfo>,
    mut exit: MessageWriter<AppExit>,
) {
    let now = Instant::now();
    // Stand in for a renderer: consume the presentation gate so the runner requests a redraw
    // every frame (the redraw events wake the loop between deadlines, as in a real app).
    state.frame_ready = false;
    let Some(phase) = PHASES.get(check.phase) else {
        return;
    };
    let start = *check.phase_start.get_or_insert_with(|| {
        limit.max_fps = phase.max_fps;
        now
    });
    let elapsed = now - start;
    check.occluded |= state.occluded;
    check.refresh = check.refresh.or(pacing.refresh_millihertz);
    if elapsed >= phase.warmup {
        check.times.push(now);
        check.display_locked += usize::from(pacing.display_locked);
    }
    if elapsed >= phase.warmup + phase.measure {
        let times = std::mem::take(&mut check.times);
        let intervals: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
        let span = times[times.len() - 1] - times[0];
        let count = u32::try_from(intervals.len()).unwrap_or(u32::MAX).max(1);
        let measured = Measured {
            max_fps: phase.max_fps,
            rate: f64::from(count) / span.as_secs_f64(),
            intervals: (
                intervals.iter().copied().min().unwrap_or_default(),
                span / count,
                intervals.iter().copied().max().unwrap_or_default(),
            ),
            occluded: std::mem::take(&mut check.occluded),
            display_locked: std::mem::take(&mut check.display_locked),
        };
        check.results.push(measured);
        check.phase += 1;
        check.phase_start = None;
        if check.phase == PHASES.len() {
            exit.write(AppExit::Success);
        }
    }
}

fn verdict(results: &[Measured]) -> Result<(), String> {
    let mut highest = 0.0f64;
    for m in results {
        let (min, mean, max) = m.intervals;
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        println!(
            "frame_limit_window: limit {:>3}: {:9.2} updates/s  interval min {:.3} ms mean {:.3} \
             ms max {:.3} ms{}",
            m.max_fps,
            m.rate,
            ms(min),
            ms(mean),
            ms(max),
            if m.occluded {
                "  (window occluded)"
            } else {
                ""
            }
        );
        if m.display_locked > 0 {
            return Err(format!(
                "limit {}: {} updates paced by the display, but nothing presents",
                m.max_fps, m.display_locked
            ));
        }
        if m.max_fps > 0 {
            let error = (m.rate - f64::from(m.max_fps)) / f64::from(m.max_fps);
            if error.abs() > TOLERANCE {
                return Err(format!(
                    "limit {}: measured {:.2} updates/s ({:+.2} %, allowed {} %)",
                    m.max_fps,
                    m.rate,
                    error * 100.0,
                    TOLERANCE * 100.0
                ));
            }
            highest = highest.max(f64::from(m.max_fps));
        } else if m.rate < highest * UNLIMITED_FACTOR {
            return Err(format!(
                "unlimited: only {:.0} updates/s (expected at least {:.0})",
                m.rate,
                highest * UNLIMITED_FACTOR
            ));
        }
    }
    if results.len() != PHASES.len() {
        return Err(format!("{} of {} phases ran", results.len(), PHASES.len()));
    }
    Ok(())
}

fn has_display() -> bool {
    if cfg!(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd"
    )) {
        ["WAYLAND_DISPLAY", "DISPLAY"]
            .iter()
            .any(|var| std::env::var_os(var).is_some_and(|v| !v.is_empty()))
    } else {
        true
    }
}

fn main() -> ExitCode {
    if std::env::var("BOXEL_WINDOW_TESTS").as_deref() != Ok("1") {
        println!(
            "frame_limit_window: skipped (opens a window; run with BOXEL_WINDOW_TESTS=1 or \
             `just test-window`)"
        );
        return ExitCode::SUCCESS;
    }
    if !has_display() {
        eprintln!("frame_limit_window: BOXEL_WINDOW_TESTS=1 but there is no display");
        return ExitCode::FAILURE;
    }
    let (disarm, armed) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if armed.recv_timeout(WATCHDOG) == Err(RecvTimeoutError::Timeout) {
            eprintln!("frame_limit_window: watchdog: still running after {WATCHDOG:?}");
            std::process::exit(4);
        }
    });

    let mut app = App::new();
    app.add_plugins(WindowPlugin {
        primary: WindowSettings {
            title: "boxel frame_limit_window".to_string(),
            width: 320,
            height: 180,
            ..WindowSettings::default()
        },
        ..WindowPlugin::default()
    })
    .init_resource::<Check>()
    .add_systems(Update, drive);
    // The runner consumes the app: read the results through a `Shutdown` system.
    let (send, results) = mpsc::channel::<(Vec<Measured>, Option<u32>)>();
    app.add_systems(boxel_app::Shutdown, move |mut check: ResMut<Check>| {
        let _ = send.send((std::mem::take(&mut check.results), check.refresh));
    });
    let exit = app.run();
    drop(disarm);
    let (results, refresh) = results.try_recv().unwrap_or_default();
    match refresh {
        Some(mhz) => println!(
            "frame_limit_window: monitor refresh {:.3} Hz",
            f64::from(mhz) / 1000.0
        ),
        None => println!("frame_limit_window: monitor refresh unknown"),
    }
    match (exit, verdict(&results)) {
        (AppExit::Success, Ok(())) => {
            println!("frame_limit_window: ok");
            ExitCode::SUCCESS
        }
        (exit, Ok(())) => {
            eprintln!("frame_limit_window: the app exited with {exit:?}");
            ExitCode::FAILURE
        }
        (_, Err(message)) => {
            eprintln!("frame_limit_window: FAILED: {message}");
            ExitCode::FAILURE
        }
    }
}
