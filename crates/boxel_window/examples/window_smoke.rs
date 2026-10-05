//! Window and runner smoke test. Agent-verifiable: no human input needed.
//!
//! `cargo run -p boxel_window --example window_smoke -- [--frames N] [--resize-test]`
//!
//! Contract: `WindowPlugin` + `InputPlugin` + `TimePlugin`; prints
//! `boxel-window: created WxH scale=S` on `WindowCreated`; writes `AppExit::Success` after N
//! updates (default 120) and prints `boxel-window: updates=N redraws=M occluded_frames=K`;
//! `--resize-test` requests a +100x+50 inner size at update 30 and exits 3 if no
//! `WindowResized` arrives within 60 updates. Exit 0 on success.
//!
//! Extras: `--timeout SECS` (default 30) exits 4 if the run takes longer, and a watchdog thread
//! kills the process 5 s after that if the event loop stops updating; `--help`; exit 1 on a
//! usage error.
//!
//! There is no renderer here, so a system in `Last` stands in for one: it consumes the
//! presentation gate (`WindowState::frame_ready`) at most every 16 ms, like a 60 Hz display, and
//! the runner (in `Reactive` mode) then requests the next redraw. On Wayland nothing is ever
//! drawn, so the window is never mapped (it does not show up on the desktop); on X11 it shows
//! as an empty window for about a second.
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

use boxel_app::{App, AppExit, Last, TimePlugin, Update};
use boxel_ecs::prelude::*;
use boxel_window::winit::dpi::PhysicalSize;
use boxel_window::{
    InputPlugin, PrimaryWindow, UpdateMode, WindowCreated, WindowPlugin, WindowResized,
    WindowSettings, WindowState,
};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: window_smoke [--frames N] [--resize-test] [--timeout SECS] [--help]\n\
     exit codes: 0 ok, 1 usage error, 3 resize check failed, 4 timeout";

/// Update at which `--resize-test` requests the new size.
const RESIZE_AT: u64 = 30;
/// Updates allowed for the `WindowResized` to arrive.
const RESIZE_WITHIN: u64 = 60;
/// Simulated display refresh interval.
const FRAME_INTERVAL: Duration = Duration::from_millis(16);
/// Extra time the watchdog thread waits after `--timeout` before killing the process.
const WATCHDOG_GRACE: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
struct Options {
    frames: u64,
    resize_test: bool,
    timeout: Duration,
}

/// `Ok(None)` for `--help`.
fn parse_args(mut args: impl Iterator<Item = String>) -> Result<Option<Options>, String> {
    let mut options = Options {
        frames: 120,
        resize_test: false,
        timeout: Duration::from_secs(30),
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--frames" => options.frames = number(&arg, args.next())?,
            "--timeout" => options.timeout = Duration::from_secs(number(&arg, args.next())?),
            "--resize-test" => options.resize_test = true,
            "--help" | "-h" => return Ok(None),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Some(options))
}

fn number(flag: &str, value: Option<String>) -> Result<u64, String> {
    let value = value.ok_or_else(|| format!("{flag} needs a value"))?;
    value
        .parse()
        .map_err(|_| format!("{flag}: {value:?} is not a non-negative integer"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResizeTest {
    Off,
    /// Before update `RESIZE_AT`.
    Waiting,
    /// Requested at update `at`.
    Requested {
        at: u64,
    },
    /// `WindowResized` arrived.
    Done,
}

#[derive(Resource, Debug)]
struct Smoke {
    options: Options,
    started: Instant,
    updates: u64,
    redraws: u64,
    occluded_frames: u64,
    last_present: Option<Instant>,
    resize: ResizeTest,
}

impl Smoke {
    fn new(options: Options) -> Self {
        Self {
            options,
            started: Instant::now(),
            updates: 0,
            redraws: 0,
            occluded_frames: 0,
            last_present: None,
            resize: if options.resize_test {
                ResizeTest::Waiting
            } else {
                ResizeTest::Off
            },
        }
    }

    fn summary(&self) {
        println!(
            "boxel-window: updates={} redraws={} occluded_frames={}",
            self.updates, self.redraws, self.occluded_frames
        );
    }
}

fn report_created(mut created: MessageReader<WindowCreated>, state: Res<WindowState>) {
    for _ in created.read() {
        println!(
            "boxel-window: created {}x{} scale={}",
            state.physical_size.x, state.physical_size.y, state.scale_factor
        );
    }
}

/// Counts updates, runs the resize check and decides when to exit.
fn drive(
    mut smoke: ResMut<Smoke>,
    state: Res<WindowState>,
    window: NonSend<PrimaryWindow>,
    mut resized: MessageReader<WindowResized>,
    mut exit: MessageWriter<AppExit>,
) {
    smoke.updates += 1;
    let updates = smoke.updates;
    if state.occluded {
        smoke.occluded_frames += 1;
    }
    for event in resized.read() {
        if let ResizeTest::Requested { at } = smoke.resize {
            println!(
                "boxel-window: resized to {}x{} after {} updates",
                event.width,
                event.height,
                updates - at
            );
            smoke.resize = ResizeTest::Done;
        }
    }
    match smoke.resize {
        ResizeTest::Waiting if updates >= RESIZE_AT => {
            let size = state.physical_size;
            let target = PhysicalSize::new(size.x + 100, size.y + 50);
            let applied = window.window.request_inner_size(target);
            println!(
                "boxel-window: requested {}x{} at update {updates} (applied now: {applied:?})",
                target.width, target.height
            );
            smoke.resize = ResizeTest::Requested { at: updates };
        }
        ResizeTest::Requested { at } if updates - at > RESIZE_WITHIN => {
            eprintln!("boxel-window: no WindowResized within {RESIZE_WITHIN} updates");
            smoke.summary();
            exit.write(AppExit::from_code(3));
            return;
        }
        _ => {}
    }
    if smoke.started.elapsed() > smoke.options.timeout {
        eprintln!("boxel-window: timeout after {:?}", smoke.options.timeout);
        smoke.summary();
        exit.write(AppExit::from_code(4));
        return;
    }
    let resize_pending = matches!(
        smoke.resize,
        ResizeTest::Waiting | ResizeTest::Requested { .. }
    );
    if updates >= smoke.options.frames && !resize_pending {
        smoke.summary();
        exit.write(AppExit::Success);
    }
}

/// Stands in for a renderer: "presents" when the gate is open and the window is visible, at most
/// once per `FRAME_INTERVAL`, and closes the gate so the runner requests the next redraw.
fn fake_present(mut state: ResMut<WindowState>, mut smoke: ResMut<Smoke>) {
    let now = Instant::now();
    let due = smoke
        .last_present
        .is_none_or(|last| now.duration_since(last) >= FRAME_INTERVAL);
    if state.frame_ready && !state.occluded && due {
        state.frame_ready = false;
        smoke.redraws += 1;
        smoke.last_present = Some(now);
    }
}

/// Kills the process if `main` has not returned `limit` after start (the event loop stopped
/// updating, so the in-app timeout cannot fire). Dropping the returned sender disarms it.
fn spawn_watchdog(limit: Duration) -> Sender<()> {
    let (disarm, armed) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if armed.recv_timeout(limit) == Err(RecvTimeoutError::Timeout) {
            eprintln!("boxel-window: watchdog: still running after {limit:?}, exiting with 4");
            std::process::exit(4);
        }
    });
    disarm
}

fn main() -> AppExit {
    let options = match parse_args(std::env::args().skip(1)) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}");
            return AppExit::Success;
        }
        Err(message) => {
            eprintln!("boxel-window: {message}\n{USAGE}");
            return AppExit::from_code(1);
        }
    };
    let _watchdog = spawn_watchdog(options.timeout.saturating_add(WATCHDOG_GRACE));
    let mut app = App::new();
    app.add_plugins((
        TimePlugin,
        InputPlugin,
        WindowPlugin {
            primary: WindowSettings {
                title: "boxel window_smoke".to_string(),
                width: 640,
                height: 360,
                ..WindowSettings::default()
            },
            update_mode: UpdateMode::Reactive {
                wait: FRAME_INTERVAL,
            },
            ..WindowPlugin::default()
        },
    ))
    .insert_resource(Smoke::new(options))
    .add_systems(Update, (report_created, drive).chain())
    .add_systems(Last, fake_present);
    app.run()
}
