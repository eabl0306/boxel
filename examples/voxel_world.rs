//! Windowed voxel terrain demo.
//!
//! `cargo run --release --example voxel_world -- [GPU options] [options]` (see `--help`).
//! Interactive by default: left click grabs the mouse, WASD/Space/`ShiftLeft` fly, `ControlLeft`
//! boosts, Escape (or leaving the window) releases the cursor, F3 toggles the debug overlay, F5
//! cycles the frame limit (30, 60, 144, 240, unlimited) and F6 toggles vsync. While the mouse is
//! grabbed, left click breaks the block under the crosshair and right click places the selected
//! block on the face it points at (keys 1 to 9 select it; F3 shows the target and the
//! selection), never where a solid body is (the walking player: `PhysicsQuery::block_is_free`).
//! Picking is a voxel raycast from the camera (`PhysicsQuery::raycast_voxels`), reach 6 blocks. G switches between flying and walking: walking is a player body with gravity
//! (`boxel::physics`, a 0.6 x 1.8 x 0.6 box and a `CharacterController`) that WASD moves along
//! the view and Space makes jump, with the camera at its eye. F3 shows the mode and a physics
//! line (`PhysicsStats`) in both modes.
//!
//! `--frames`, `--duration`, `--screenshot`, `--minimize-test`, `--resize-test`, `--vsync-test`
//! and the like make it close by itself (`--duration` keeps the interactive path: fly camera
//! on, nothing rendered while hidden); the last summary line gives the update and presented
//! frame rates and the CPU use (Linux).
//!
//! Exit codes: 0 success, 1 usage error, 2 no usable GPU, 3 check failed (screenshot, image
//! diff, resize, minimize), 4 timeout.
//!
//! The world is `ForestGenerator` by default: `TerrainGenerator` plus deterministic trees (log
//! trunk, leaf canopy) so the cutout pipeline is visible; `--no-trees` gives the plain
//! generator, and `--world overworld` the caves.
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

#[path = "shared/player.rs"]
mod player;
#[path = "shared/world.rs"]
mod world;

use boxel::app::{TaskPoolOptions, TaskPoolThreadAssignmentPolicy};
use boxel::camera::FlyCameraSystems;
use boxel::debug_overlay::{Crosshair, OverlayLines};
use boxel::gfx::wgpu::PresentMode;
use boxel::gfx::{CLI_HELP, DeviceClass, RgbaImage};
use boxel::physics::{PhysicsPlugin, Probe as VoxelProbe};
use boxel::prelude::*;
use boxel::render::{DebugDestroyDevice, GpuFailed, GpuReady, SurfaceInfo};
use boxel::voxel::{BlockRegistry, MAX_BLOCK_COORD};
use boxel::window::winit::dpi::PhysicalSize;
use boxel::window::{PrimaryWindow, WindowResized};
use boxel::world::{BlockEdit, WorldContent, WorldStats, WorldSystems};
use player::WalkPlugin;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};
use world::{World, WorldChoice};

const USAGE: &str = "\
usage: voxel_world [GPU options] [options]
  --frames N              exit 0 after N rendered frames (automation: renders while hidden,
                          disables fly-camera input)
  --wait-chunks           start counting frames once the world settled for 3 frames
  --duration SECS         exit 0 after SECS seconds of the normal interactive path (fly
                          camera on, nothing rendered while hidden)
  --timeout SECS          wall-clock limit, exit 4 (default 120 with automation flags; with
                          --duration at least SECS + 30)
  --fps-limit N           max updates per second, 0 = unlimited (default 144; F5 cycles it)
  --vsync on|off          vsync (default off, so the fps limit governs; F6 toggles it)
  --vsync-test            switch vsync at frame 30 and back at frame 60, check the surface
                          follows (implies --frames 120)
  --screenshot PATH       at frame N save a PNG of the frame (implies --frames 120)
  --expect-terrain        after the screenshot: non-background fraction in [0.05, 0.99]
  --compare-to PATH       after the screenshot: compare with PATH (channel tolerance 2)
  --max-diff F            allowed differing pixel fraction for --compare-to (default 0.005)
  --minimize-test         minimize 60 updates after the first presented frame, run 3 s,
                          check that updates and fixed steps kept going
  --resize-test           at frame 30 (and after 10 presented frames) grow the window by
                          100x50 and check the surface follows (implies --frames 120)
  --seed N                terrain seed
  --view-distance N       mesh radius in chunks (default 12; 6 on a software adapter)
  --vertical-radius N     vertical mesh radius in chunks, up and down (default 8)
  --fixed-height          stream every layer of the generator and nothing else, whatever the
                          camera height (boxel M0 streaming, for comparisons)
  --size WxH              window size in logical pixels (default 1280x720)
  --camera x,y,z,yaw,pitch  initial camera, degrees (default 0,120,0,45,-30)
  --simulate-device-loss N[,M..]  destroy the GPU device at frame N (recovery check; a
                          list repeats it: the second loss of an adapter lowers its tier)
  --async-threads N       async compute pool size (chunk jobs)
  --debug-overlay         start with the F3 debug overlay visible (F3 toggles it)
  --walk                  start walking on the ground under the camera (G toggles walking)
  --help                  this text
exit codes: 0 ok, 1 usage error, 2 no usable GPU, 3 check failed, 4 timeout";

/// The whole help: [`USAGE`] and the world options.
fn usage() -> String {
    format!("{USAGE}\nworld options:\n{}", world::usage(World::Forest))
}

/// Default frames when a flag implies `--frames`.
const IMPLIED_FRAMES: u64 = 120;
/// Default `--timeout` for automation runs.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
/// `--duration` without `--timeout`: the default timeout is at least the duration plus this.
const DURATION_GRACE: Duration = Duration::from_secs(30);
/// Extra time the watchdog thread waits after `--timeout` before killing the process.
const WATCHDOG_GRACE: Duration = Duration::from_secs(15);
/// Consecutive settled frames before `--wait-chunks` starts counting.
const SETTLED_FRAMES: u32 = 3;
/// `--resize-test`: frame of the request and frames allowed for `WindowResized`.
const RESIZE_AT: u64 = 30;
const RESIZE_WITHIN: u64 = 60;
/// `--resize-test` also waits for this many presented frames (the window is mapped and past the
/// compositor's initial configure sequence, which would override a client-side size), or for
/// `RESIZE_FORCE_AT` frames if the window never presents (hidden).
const RESIZE_AFTER_PRESENTS: u64 = 10;
const RESIZE_FORCE_AT: u64 = 600;
/// Frames between `WindowResized` and the surface check (`SurfaceInfo` is published by the
/// renderer's `Cleanup`, after this app's `Update` systems).
const RESIZE_SETTLE_FRAMES: u64 = 2;
/// `--minimize-test`: updates after the first present, and the hidden run time.
const MINIMIZE_AFTER_UPDATES: u64 = 60;
const MINIMIZED_FOR: Duration = Duration::from_secs(3);
const MINIMIZED_MIN_UPDATES: u64 = 60;
const MINIMIZED_MIN_FIXED_STEPS: u64 = 60;
/// Default mesh radius, and the one used on software adapters.
const DEFAULT_RADIUS: u32 = 12;
const SOFTWARE_RADIUS: u32 = 6;
/// `--vsync-test`: frame of the first switch, frame of the switch back, and frames allowed for
/// `RenderInfo::present_mode` to follow (the surface is reconfigured in the next `Render`).
const VSYNC_AT: u64 = 30;
const VSYNC_BACK_AT: u64 = 60;
const VSYNC_SETTLE_FRAMES: u64 = 2;

// ---------------------------------------------------------------------------------------------
// Options

#[derive(Clone, Debug)]
struct Options {
    frames: Option<u64>,
    wait_chunks: bool,
    timeout: Option<Duration>,
    screenshot: Option<PathBuf>,
    expect_terrain: bool,
    compare_to: Option<PathBuf>,
    max_diff: f64,
    minimize_test: bool,
    resize_test: bool,
    seed: u64,
    view_distance: Option<u32>,
    vertical_radius: u32,
    fixed_height: bool,
    size: (u32, u32),
    camera: [f64; 5],
    /// Frames at which the device is destroyed, ascending.
    simulate_device_loss: Vec<u64>,
    async_threads: Option<usize>,
    world: WorldChoice,
    debug_overlay: bool,
    /// Start walking (G switches walking and flying).
    walk: bool,
    duration: Option<Duration>,
    /// `None`: the engine default (`FrameLimit::default()`, 144).
    fps_limit: Option<u32>,
    /// `None`: the engine default (`PresentSettings::default()`, vsync off).
    vsync: Option<bool>,
    vsync_test: bool,
}

impl Options {
    /// Scripted runs (`--frames`, `--minimize-test`, `--resize-test`): fly-camera input is off
    /// and `--timeout` defaults to [`DEFAULT_TIMEOUT`]. A `--duration` run is not one: it times
    /// the normal interactive path.
    fn automation(&self) -> bool {
        self.frames.is_some() || self.minimize_test || self.resize_test
    }

    /// `--timeout` when none is given: [`DEFAULT_TIMEOUT`] for automation runs, and with
    /// `--duration` at least the duration plus [`DURATION_GRACE`], so the run ends with exit 0
    /// and not with the timeout.
    fn default_timeout(&self) -> Option<Duration> {
        let automation = self.automation().then_some(DEFAULT_TIMEOUT);
        let duration = self
            .duration
            .map(|d| d.saturating_add(DURATION_GRACE).max(DEFAULT_TIMEOUT));
        automation.max(duration)
    }
}

/// `Ok(None)` for `--help`.
fn parse_options(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options {
        frames: None,
        wait_chunks: false,
        timeout: None,
        screenshot: None,
        expect_terrain: false,
        compare_to: None,
        max_diff: 0.005,
        minimize_test: false,
        resize_test: false,
        seed: TerrainGenerator::default().seed,
        view_distance: None,
        vertical_radius: ChunkViewer::DEFAULT_VERTICAL_RADIUS,
        fixed_height: false,
        size: (1280, 720),
        camera: [0.0, 120.0, 0.0, 45.0, -30.0],
        simulate_device_loss: Vec::new(),
        async_threads: None,
        world: WorldChoice::default(),
        debug_overlay: false,
        walk: false,
        duration: None,
        fps_limit: None,
        vsync: None,
        vsync_test: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        if o.world.parse_flag(flag, &mut value)? {
            continue;
        }
        match flag {
            "--help" | "-h" => return Ok(None),
            "--frames" => o.frames = Some(number(flag, &value()?)?),
            "--wait-chunks" => o.wait_chunks = true,
            "--timeout" => o.timeout = Some(Duration::from_secs(number(flag, &value()?)?)),
            "--screenshot" => o.screenshot = Some(PathBuf::from(value()?)),
            "--expect-terrain" => o.expect_terrain = true,
            "--compare-to" => o.compare_to = Some(PathBuf::from(value()?)),
            "--max-diff" => {
                let v = value()?;
                o.max_diff = v
                    .parse::<f64>()
                    .ok()
                    .filter(|f| (0.0..=1.0).contains(f))
                    .ok_or_else(|| format!("{flag}: {v:?} is not a fraction in [0, 1]"))?;
            }
            "--minimize-test" => o.minimize_test = true,
            "--resize-test" => o.resize_test = true,
            "--seed" => o.seed = number(flag, &value()?)?,
            "--view-distance" => {
                let radius = number(flag, &value()?)?;
                o.view_distance = Some(
                    u32::try_from(radius)
                        .ok()
                        .filter(|r| (1..=64).contains(r))
                        .ok_or_else(|| format!("{flag}: {radius} is not in 1..=64"))?,
                );
            }
            "--vertical-radius" => {
                let radius = number(flag, &value()?)?;
                o.vertical_radius = u32::try_from(radius)
                    .ok()
                    .filter(|r| *r <= 64)
                    .ok_or_else(|| format!("{flag}: {radius} is not in 0..=64"))?;
            }
            "--fixed-height" => o.fixed_height = true,
            "--size" => {
                let v = value()?;
                o.size = v
                    .split_once(['x', 'X'])
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .filter(|&(w, h): &(u32, u32)| w > 0 && h > 0 && w <= 16384 && h <= 16384)
                    .ok_or_else(|| format!("{flag}: {v:?} is not WxH"))?;
            }
            "--camera" => {
                let v = value()?;
                let parts: Vec<f64> = v
                    .split(',')
                    .map(|p| p.trim().parse::<f64>())
                    .collect::<Result<_, _>>()
                    .map_err(|_| format!("{flag}: {v:?} is not x,y,z,yaw,pitch"))?;
                o.camera = parts
                    .try_into()
                    .ok()
                    .filter(|c: &[f64; 5]| c.iter().all(|x| x.is_finite()))
                    .ok_or_else(|| format!("{flag}: {v:?} is not x,y,z,yaw,pitch"))?;
                if o.camera[..3]
                    .iter()
                    .any(|c| c.abs() > MAX_BLOCK_COORD as f64)
                {
                    return Err(format!(
                        "{flag}: {v:?} is outside the world: x, y and z must be within \
                         +-{MAX_BLOCK_COORD} (2^61 voxels)"
                    ));
                }
            }
            "--simulate-device-loss" => {
                let v = value()?;
                o.simulate_device_loss = v
                    .split(',')
                    .map(|n| number(flag, n.trim()))
                    .collect::<Result<_, _>>()?;
                o.simulate_device_loss.sort_unstable();
            }
            "--async-threads" => {
                let n = number(flag, &value()?)?;
                o.async_threads = Some(
                    usize::try_from(n)
                        .ok()
                        .filter(|n| (1..=64).contains(n))
                        .ok_or_else(|| format!("{flag}: {n} is not in 1..=64"))?,
                );
            }
            "--debug-overlay" => o.debug_overlay = true,
            "--walk" => o.walk = true,
            "--duration" => o.duration = Some(Duration::from_secs(number(flag, &value()?)?)),
            "--fps-limit" => {
                let n = number(flag, &value()?)?;
                o.fps_limit = Some(
                    u32::try_from(n)
                        .ok()
                        .filter(|n| *n <= 100_000)
                        .ok_or_else(|| format!("{flag}: {n} is not in 0..=100000"))?,
                );
            }
            "--vsync" => {
                let v = value()?;
                o.vsync = Some(match v.as_str() {
                    "on" => true,
                    "off" => false,
                    _ => return Err(format!("{flag}: {v:?} is not on or off")),
                });
            }
            "--vsync-test" => o.vsync_test = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    o.world.validate(o.fixed_height)?;
    if (o.expect_terrain || o.compare_to.is_some()) && o.screenshot.is_none() {
        return Err("--expect-terrain and --compare-to need --screenshot".to_string());
    }
    if o.frames.is_none() && (o.screenshot.is_some() || o.resize_test || o.vsync_test) {
        o.frames = Some(IMPLIED_FRAMES);
    }
    if o.frames.is_none() && (o.wait_chunks || !o.simulate_device_loss.is_empty()) {
        return Err("--wait-chunks and --simulate-device-loss need --frames".to_string());
    }
    if o.timeout.is_none() {
        o.timeout = o.default_timeout();
    }
    Ok(Some(o))
}

fn number(flag: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{flag}: {value:?} is not a non-negative integer"))
}

// ---------------------------------------------------------------------------------------------
// App state

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shot {
    Off,
    Pending,
    Requested,
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Resize {
    Off,
    Pending,
    Requested {
        at: u64,
        target: (u32, u32),
    },
    /// `WindowResized` arrived at frame `at`; the surface follows in the next `Render`.
    Resized {
        at: u64,
    },
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Minimize {
    Off,
    /// Waiting for the first presented frame.
    WaitPresent,
    /// Counting updates after the first present.
    Counting {
        from: u64,
    },
    /// Minimized at `at` with the counters of that moment.
    Minimized {
        at: Instant,
        updates: u64,
        presents: u64,
        fixed: u64,
    },
    /// Checked.
    Done,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VsyncTest {
    Off,
    /// Before frame `VSYNC_AT`.
    Pending,
    /// Vsync was switched to `vsync` at frame `at`; the surface had `original` before.
    Switched {
        at: u64,
        original: PresentMode,
        vsync: bool,
        reconfigures: u32,
    },
    /// Switched back at frame `at`.
    Restored {
        at: u64,
        original: PresentMode,
        switched_to: PresentMode,
        reconfigures: u32,
    },
    Done,
}

/// The start of the rate measurement printed at exit.
#[derive(Clone, Copy, Debug)]
struct Probe {
    at: Instant,
    updates: u64,
    presented: u64,
    cpu: Option<Duration>,
    /// Started once the world had settled (loading excluded).
    settled: bool,
}

impl Probe {
    fn start(run: &Run, info: &RenderInfo, settled: bool) -> Self {
        Self {
            at: Instant::now(),
            updates: run.updates,
            presented: info.frames_presented,
            cpu: process_cpu_time(),
            settled,
        }
    }
}

#[derive(Resource)]
struct Run {
    options: Options,
    started: Instant,
    updates: u64,
    settled_streak: u32,
    /// `frames_rendered` when frame counting started.
    counting_from: Option<u64>,
    shot: Shot,
    /// Device losses written so far.
    losses_sent: usize,
    resize: Resize,
    minimize: Minimize,
    vsync_test: VsyncTest,
    probe: Option<Probe>,
    /// The frame limit as of the latest `drive` (F5 may change it).
    limit: FrameLimit,
    exiting: bool,
}

impl Run {
    fn new(options: Options) -> Self {
        Self {
            vsync_test: if options.vsync_test {
                VsyncTest::Pending
            } else {
                VsyncTest::Off
            },
            probe: None,
            limit: FrameLimit::new(options.fps_limit.unwrap_or(FrameLimit::default().max_fps)),
            counting_from: (!options.wait_chunks).then_some(0),
            shot: if options.screenshot.is_some() {
                Shot::Pending
            } else {
                Shot::Off
            },
            resize: if options.resize_test {
                Resize::Pending
            } else {
                Resize::Off
            },
            minimize: if options.minimize_test {
                Minimize::WaitPresent
            } else {
                Minimize::Off
            },
            options,
            started: Instant::now(),
            updates: 0,
            settled_streak: 0,
            losses_sent: 0,
            exiting: false,
        }
    }

    /// Frames counted for automation (after settling with `--wait-chunks`).
    fn counted(&self, info: &RenderInfo) -> Option<u64> {
        self.counting_from
            .map(|base| info.frames_rendered.saturating_sub(base))
    }

    /// Every requested check is done (`--frames` reached, `--duration` elapsed, screenshot
    /// checked, resize seen, minimize and vsync tests finished).
    fn complete(&self, info: &RenderInfo) -> bool {
        let frames_done = match self.options.frames {
            Some(frames) => self.counted(info).is_some_and(|c| c >= frames),
            None => true,
        };
        frames_done
            && self
                .options
                .duration
                .is_none_or(|d| self.started.elapsed() >= d)
            && matches!(self.shot, Shot::Off | Shot::Done)
            && matches!(self.resize, Resize::Off | Resize::Done)
            && matches!(self.minimize, Minimize::Off | Minimize::Done)
            && matches!(self.vsync_test, VsyncTest::Off | VsyncTest::Done)
    }

    /// Prints the summary and writes the exit status once.
    fn exit(&mut self, code: u8, info: &RenderInfo, exit: &mut MessageWriter<AppExit>) {
        if self.exiting {
            return;
        }
        self.exiting = true;
        println!(
            "boxel: frames_rendered={} frames_presented={} recoveries={} chunks={} dropped={} \
             elapsed={:.1}s exit={code}",
            info.frames_rendered,
            info.frames_presented,
            info.recoveries,
            info.chunks,
            info.dropped_chunks,
            self.started.elapsed().as_secs_f64()
        );
        if let Some(probe) = self.probe {
            let seconds = probe.at.elapsed().as_secs_f64();
            let cpu = match (probe.cpu, process_cpu_time()) {
                (Some(start), Some(end)) if seconds > 0.0 => format!(
                    "{:.1}%",
                    end.saturating_sub(start).as_secs_f64() / seconds * 100.0
                ),
                _ => "n/a".to_string(),
            };
            let limit = match self.limit.max_fps {
                0 => "none".to_string(),
                fps => fps.to_string(),
            };
            let present = info
                .present_mode
                .map_or_else(|| "none".to_string(), |m| format!("{m:?}"));
            println!(
                "boxel: rates over {seconds:.1}s ({}): updates={:.1}/s presented={:.1}/s \
                 limit={limit} present={present} cpu={cpu} (100% = one core)",
                if probe.settled {
                    "world settled"
                } else {
                    "while loading"
                },
                (self.updates - probe.updates) as f64 / seconds.max(1e-9),
                (info.frames_presented - probe.presented) as f64 / seconds.max(1e-9),
            );
        }
        exit.write(AppExit::from_code(code));
    }
}

/// CPU time used by the whole process so far (all threads): the sum of the first field of
/// `/proc/self/task/*/schedstat` (nanoseconds on a CPU). Linux only; `None` elsewhere.
fn process_cpu_time() -> Option<Duration> {
    let tasks = std::fs::read_dir("/proc/self/task").ok()?;
    let mut total = 0u64;
    for task in tasks.flatten() {
        // A thread that exits meanwhile is skipped.
        let Ok(stat) = std::fs::read_to_string(task.path().join("schedstat")) else {
            continue;
        };
        if let Some(nanos) = stat
            .split_whitespace()
            .next()
            .and_then(|v| v.parse::<u64>().ok())
        {
            total += nanos;
        }
    }
    Some(Duration::from_nanos(total))
}

#[derive(Resource, Default)]
struct FixedSteps(u64);

// ---------------------------------------------------------------------------------------------
// Systems

fn spawn_camera(mut commands: Commands, run: Res<Run>) {
    let o = &run.options;
    let radius = o.view_distance.unwrap_or(DEFAULT_RADIUS);
    let [x, y, z, yaw, pitch] = o.camera;
    let (yaw, pitch) = ((yaw as f32).to_radians(), (pitch as f32).to_radians());
    commands.spawn((
        Camera {
            view_distance: radius as f32 * 32.0,
            ..Camera::default()
        },
        Transform::from_yaw_pitch(DVec3::new(x, y, z), yaw, pitch),
        FlyCamera {
            enabled: !o.automation(),
            yaw,
            pitch,
            ..FlyCamera::default()
        },
        ChunkViewer::new(radius).with_vertical_radius(if o.fixed_height {
            u32::MAX
        } else {
            o.vertical_radius
        }),
    ));
}

/// `boxel: <summary>` on every (re)selection; exit 2 when no GPU can be used.
fn gpu_events(
    mut ready: MessageReader<GpuReady>,
    mut failed: MessageReader<GpuFailed>,
    mut run: ResMut<Run>,
    info: Res<RenderInfo>,
    present: Res<PresentSettings>,
    mut viewers: Query<(&mut ChunkViewer, &mut Camera)>,
    mut exit: MessageWriter<AppExit>,
) {
    for event in ready.read() {
        println!("boxel: {}", event.summary);
        let limit = match run.limit.max_fps {
            0 => "unlimited".to_string(),
            fps => format!("{fps} fps"),
        };
        let mode = info
            .present_mode
            .map_or_else(|| "none".to_string(), |m| format!("{m:?}"));
        println!(
            "boxel: frame limit {limit}, vsync wanted {}, present mode {mode}",
            on_off(present.is_vsync()),
        );
        if info.class == Some(DeviceClass::Software) && run.options.view_distance.is_none() {
            for (mut viewer, mut camera) in &mut viewers {
                viewer.radius = SOFTWARE_RADIUS;
                camera.view_distance = SOFTWARE_RADIUS as f32 * 32.0;
            }
            println!("boxel: software adapter: view distance {SOFTWARE_RADIUS} chunks");
        }
    }
    if let Some(event) = failed.read().last() {
        eprintln!("boxel: no usable GPU: {}", event.error);
        run.exit(2, &info, &mut exit);
    }
}

fn count_fixed_steps(mut steps: ResMut<FixedSteps>) {
    steps.0 += 1;
}

/// Timeout, settling, device loss, screenshot request, resize test and the final exit.
fn drive(
    mut run: ResMut<Run>,
    info: Res<RenderInfo>,
    stats: Res<WorldStats>,
    surface: Res<SurfaceInfo>,
    window: NonSend<PrimaryWindow>,
    state: Res<WindowState>,
    limit: Res<FrameLimit>,
    mut resized: MessageReader<WindowResized>,
    mut destroy: MessageWriter<DebugDestroyDevice>,
    mut screenshot: MessageWriter<RequestScreenshot>,
    mut exit: MessageWriter<AppExit>,
) {
    run.updates += 1;
    run.limit = *limit;
    if run.exiting {
        return;
    }
    if let Some(timeout) = run.options.timeout
        && run.started.elapsed() > timeout
    {
        eprintln!("boxel: timeout after {}s", timeout.as_secs());
        run.exit(4, &info, &mut exit);
        return;
    }
    if run.options.frames.is_some() && run.counting_from.is_none() {
        let settled = info.ready && stats.chunks > 0 && stats.is_settled();
        run.settled_streak = if settled { run.settled_streak + 1 } else { 0 };
        if run.settled_streak >= SETTLED_FRAMES {
            run.counting_from = Some(info.frames_rendered);
            println!(
                "boxel: world settled after {:.1}s: chunks={} meshed={} empty={}",
                run.started.elapsed().as_secs_f64(),
                stats.chunks,
                stats.mesh_ready,
                stats.mesh_empty
            );
        }
    }
    let Some(counted) = run.counted(&info) else {
        return;
    };
    // Rates are measured from the first rendered frame, and again from the moment the world
    // settles (so loading does not count when the run is long enough).
    let world_settled = stats.chunks > 0 && stats.is_settled();
    let restart = match run.probe {
        None => info.ready && info.frames_rendered > 0,
        Some(probe) => !probe.settled && world_settled,
    };
    if restart {
        let probe = Probe::start(&run, &info, world_settled);
        run.probe = Some(probe);
    }

    if let Some(&at) = run.options.simulate_device_loss.get(run.losses_sent)
        && counted >= at
        && info.ready
    {
        run.losses_sent += 1;
        println!("boxel: simulating device loss at frame {counted}");
        destroy.write(DebugDestroyDevice);
    }

    for event in resized.read() {
        if let Resize::Requested { at, .. } = run.resize {
            println!(
                "boxel: resized to {}x{} after {} frames",
                event.width,
                event.height,
                counted.saturating_sub(at)
            );
            run.resize = Resize::Resized { at: counted };
        }
    }
    match run.resize {
        Resize::Pending
            if counted >= RESIZE_AT
                && (info.frames_presented >= RESIZE_AFTER_PRESENTS
                    || counted >= RESIZE_FORCE_AT) =>
        {
            let size = state.physical_size;
            let target = (size.x + 100, size.y + 50);
            let applied = window
                .window
                .request_inner_size(PhysicalSize::new(target.0, target.1));
            println!(
                "boxel: requested {}x{} at frame {counted} (applied now: {applied:?})",
                target.0, target.1
            );
            run.resize = Resize::Requested {
                at: counted,
                target,
            };
        }
        Resize::Resized { at } if counted >= at + RESIZE_SETTLE_FRAMES => run.resize = Resize::Done,
        Resize::Requested { at, target } if counted.saturating_sub(at) > RESIZE_WITHIN => {
            eprintln!(
                "boxel: resize test failed: no WindowResized to {}x{} within {RESIZE_WITHIN} \
                 frames",
                target.0, target.1
            );
            run.exit(3, &info, &mut exit);
            return;
        }
        _ => {}
    }

    // `--frames` reached, or a `--duration` run (`complete` checks the time). Without either,
    // `minimize_test` decides when to exit.
    let target_reached = match run.options.frames {
        Some(frames) => counted >= frames,
        None => run.options.duration.is_some(),
    };
    if !target_reached {
        return;
    }
    if run.shot == Shot::Pending {
        if let Some(path) = run.options.screenshot.clone() {
            println!("boxel: screenshot at frame {counted} -> {}", path.display());
            screenshot.write(RequestScreenshot { path });
        }
        run.shot = Shot::Requested;
    }
    if !run.complete(&info) {
        return;
    }
    if run.options.resize_test {
        let window_size = state.physical_size;
        println!(
            "boxel: resize check: window {}x{} surface {}x{} reconfigures={} recreates={} \
             errors={}",
            window_size.x,
            window_size.y,
            surface.size.x,
            surface.size.y,
            surface.reconfigures,
            surface.recreates,
            surface.errors
        );
        if surface.errors > 0 || surface.size != window_size {
            eprintln!("boxel: resize test failed: surface errors or size mismatch");
            run.exit(3, &info, &mut exit);
            return;
        }
    }
    run.exit(0, &info, &mut exit);
}

/// Checks a saved screenshot (`--expect-terrain`, `--compare-to`); `drive` exits afterwards.
fn check_screenshot(
    mut saved: MessageReader<ScreenshotSaved>,
    mut run: ResMut<Run>,
    info: Res<RenderInfo>,
    mut exit: MessageWriter<AppExit>,
) {
    for event in saved.read() {
        if run.shot != Shot::Requested {
            continue;
        }
        run.shot = Shot::Done;
        if let Err(message) = screenshot_checks(&run.options, event) {
            eprintln!("boxel: {message}");
            run.exit(3, &info, &mut exit);
        }
    }
}

fn screenshot_checks(options: &Options, event: &ScreenshotSaved) -> Result<(), String> {
    event
        .result
        .as_ref()
        .map_err(|e| format!("screenshot {} failed: {e}", event.path.display()))?;
    println!("boxel: screenshot saved to {}", event.path.display());
    if !options.expect_terrain && options.compare_to.is_none() {
        return Ok(());
    }
    let image = RgbaImage::load_png(&event.path)
        .map_err(|e| format!("cannot read {}: {e}", event.path.display()))?;
    if options.expect_terrain {
        let fraction = image.non_background_fraction();
        println!("boxel: terrain fraction={fraction:.4}");
        if !(0.05..=0.99).contains(&fraction) {
            return Err(format!(
                "expect-terrain failed: non-background fraction {fraction:.4} not in [0.05, 0.99]"
            ));
        }
    }
    if let Some(reference) = &options.compare_to {
        let other = RgbaImage::load_png(reference)
            .map_err(|e| format!("cannot read {}: {e}", reference.display()))?;
        if (image.width, image.height) != (other.width, other.height) {
            return Err(format!(
                "compare-to failed: size {}x{} differs from {}x{} ({})",
                image.width,
                image.height,
                other.width,
                other.height,
                reference.display()
            ));
        }
        let diff = image.compare(&other, 2);
        println!(
            "boxel: diff fraction={:.6} max_channel={}",
            diff.fraction(),
            diff.max_channel_diff
        );
        if diff.fraction() > options.max_diff {
            return Err(format!(
                "compare-to failed: {:.4} % of pixels differ (max {:.4} %)",
                diff.fraction() * 100.0,
                options.max_diff * 100.0
            ));
        }
    }
    Ok(())
}

/// `--minimize-test`: 60 updates after the first presented frame, minimize, run 3 s, check.
fn minimize_test(
    mut run: ResMut<Run>,
    info: Res<RenderInfo>,
    steps: Res<FixedSteps>,
    window: NonSend<PrimaryWindow>,
    mut exit: MessageWriter<AppExit>,
) {
    if run.exiting {
        return;
    }
    let updates = run.updates;
    match run.minimize {
        Minimize::WaitPresent if info.frames_presented > 0 => {
            run.minimize = Minimize::Counting { from: updates };
        }
        Minimize::Counting { from } if updates - from >= MINIMIZE_AFTER_UPDATES => {
            println!("boxel: minimizing at update {updates}");
            window.window.set_minimized(true);
            run.minimize = Minimize::Minimized {
                at: Instant::now(),
                updates,
                presents: info.frames_presented,
                fixed: steps.0,
            };
        }
        Minimize::Minimized {
            at,
            updates: u0,
            presents: p0,
            fixed: s0,
        } if at.elapsed() >= MINIMIZED_FOR => {
            let (u, p, s) = (
                updates - u0,
                info.frames_presented - p0,
                steps.0.saturating_sub(s0),
            );
            println!("boxel: minimized updates={u} presents={p} fixed_steps={s}");
            run.minimize = Minimize::Done;
            if u < MINIMIZED_MIN_UPDATES || s < MINIMIZED_MIN_FIXED_STEPS {
                eprintln!(
                    "boxel: minimize test failed: the simulation froze (need updates >= \
                     {MINIMIZED_MIN_UPDATES} and fixed_steps >= {MINIMIZED_MIN_FIXED_STEPS})"
                );
                run.exit(3, &info, &mut exit);
            } else if run.complete(&info) {
                run.exit(0, &info, &mut exit);
            }
        }
        _ => {}
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

/// `--vsync-test`: switch vsync at frame `VSYNC_AT` and back at `VSYNC_BACK_AT`; each time the
/// surface must be reconfigured (same device) without errors, vsync on must give `Fifo` and the
/// switch back must restore the original mode.
fn vsync_test(
    mut run: ResMut<Run>,
    info: Res<RenderInfo>,
    surface: Res<SurfaceInfo>,
    mut present: ResMut<PresentSettings>,
    mut exit: MessageWriter<AppExit>,
) {
    if run.exiting {
        return;
    }
    let (Some(counted), Some(mode)) = (run.counted(&info), info.present_mode) else {
        return;
    };
    let failure = match run.vsync_test {
        VsyncTest::Pending if counted >= VSYNC_AT => {
            let vsync = !present.is_vsync();
            present.set_vsync(vsync);
            println!(
                "boxel: vsync test: {mode:?}, switching vsync {} at frame {counted}",
                on_off(vsync)
            );
            run.vsync_test = VsyncTest::Switched {
                at: counted,
                original: mode,
                vsync,
                reconfigures: surface.reconfigures,
            };
            None
        }
        VsyncTest::Switched {
            at,
            original,
            vsync,
            reconfigures,
        } if counted >= VSYNC_BACK_AT.max(at + VSYNC_SETTLE_FRAMES) => {
            if surface.reconfigures == reconfigures {
                Some("the surface was not reconfigured".to_string())
            } else if vsync && mode != PresentMode::Fifo {
                Some(format!("vsync on gave {mode:?}, not Fifo"))
            } else {
                present.set_vsync(!vsync);
                println!(
                    "boxel: vsync test: {mode:?}, switching vsync {} at frame {counted}",
                    on_off(!vsync)
                );
                run.vsync_test = VsyncTest::Restored {
                    at: counted,
                    original,
                    switched_to: mode,
                    reconfigures: surface.reconfigures,
                };
                None
            }
        }
        VsyncTest::Restored {
            at,
            original,
            switched_to,
            reconfigures,
        } if counted >= at + VSYNC_SETTLE_FRAMES => {
            if surface.reconfigures == reconfigures {
                Some("the surface was not reconfigured back".to_string())
            } else if mode != original {
                Some(format!("switching back gave {mode:?}, not {original:?}"))
            } else if surface.errors > 0 {
                Some(format!("{} surface errors", surface.errors))
            } else {
                println!(
                    "boxel: vsync test: {original:?} -> {switched_to:?} -> {mode:?} \
                     reconfigures={} errors=0",
                    surface.reconfigures
                );
                run.vsync_test = VsyncTest::Done;
                None
            }
        }
        _ => None,
    };
    if let Some(failure) = failure {
        eprintln!("boxel: vsync test failed: {failure}");
        run.vsync_test = VsyncTest::Done;
        run.exit(3, &info, &mut exit);
    }
}

// ---------------------------------------------------------------------------------------------
// Block picking and editing

/// How far the camera reaches when picking blocks, in blocks.
const REACH: f64 = 6.0;
/// The blocks keys 1 to 9 select for placing (names missing from the registry are skipped).
const HOTBAR: [&str; 9] = [
    "boxel:stone",
    "boxel:dirt",
    "boxel:grass",
    "boxel:planks",
    "boxel:log",
    "boxel:leaves",
    "boxel:sand",
    "boxel:gravel",
    "boxel:snow",
];
const HOTBAR_KEYS: [KeyCode; 9] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
];

/// Block picking: the block under the crosshair and the block to place.
#[derive(Resource, Debug)]
struct Picking {
    /// Placeable blocks: [`HOTBAR`] resolved against the registry.
    hotbar: Vec<(&'static str, BlockState)>,
    /// Index into `hotbar`.
    selected: usize,
    /// The block under the crosshair, refreshed every frame.
    target: Option<VoxelHit>,
}

impl Picking {
    fn new(registry: &BlockRegistry) -> Self {
        Self {
            hotbar: HOTBAR
                .iter()
                .filter_map(|&name| Some((name, registry.by_name(name)?)))
                .collect(),
            selected: 0,
            target: None,
        }
    }

    /// The selected block (air if the hotbar is empty).
    fn block(&self) -> BlockState {
        self.hotbar
            .get(self.selected)
            .map_or(BlockState::AIR, |&(_, state)| state)
    }
}

/// What a click asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Click {
    Break,
    Place,
}

impl Click {
    /// This frame's click: left breaks, right places. Only while the cursor was grabbed before
    /// this frame: the click that grabs it does nothing else.
    fn from_input(buttons: &ButtonInput<MouseButton>, grabbed: bool) -> Option<Click> {
        if !grabbed {
            None
        } else if buttons.just_pressed(MouseButton::Left) {
            Some(Click::Break)
        } else if buttons.just_pressed(MouseButton::Right) {
            Some(Click::Place)
        } else {
            None
        }
    }
}

/// The edit a click makes on `hit`. Breaking sets the block to air. Placing puts `block` in
/// front of the face the ray entered, if `free` says that cell can take it (air or a fluid,
/// loaded, and no solid body there); a ray that started inside a block has no face and places
/// nothing.
fn click_edit(
    click: Click,
    hit: &VoxelHit,
    block: BlockState,
    free: impl FnOnce(BlockPos) -> bool,
) -> Option<BlockEdit> {
    match click {
        Click::Break => Some(BlockEdit {
            pos: hit.block,
            state: BlockState::AIR,
        }),
        Click::Place => {
            let pos = hit.adjacent()?;
            (!block.is_air() && free(pos)).then_some(BlockEdit { pos, state: block })
        }
    }
}

/// `+Y`, `-X`...
fn face_name(face: Face) -> &'static str {
    match face {
        Face::PosY => "+Y",
        Face::NegY => "-Y",
        Face::PosX => "+X",
        Face::NegX => "-X",
        Face::PosZ => "+Z",
        Face::NegZ => "-Z",
    }
}

/// The F3 lines of block picking.
fn picking_lines(picking: &Picking, registry: &BlockRegistry) -> [String; 2] {
    let target = match &picking.target {
        Some(hit) => format!(
            "Target {} {} {} {}  face {}",
            registry.name(hit.state),
            hit.block.x,
            hit.block.y,
            hit.block.z,
            hit.face.map_or("inside", face_name)
        ),
        None => format!("Target none (reach {REACH})"),
    };
    let place = match picking.hotbar.get(picking.selected) {
        Some((name, _)) => format!("Place [{}] {name}  (keys 1-9)", picking.selected + 1),
        None => "Place nothing".to_string(),
    };
    [target, place]
}

/// Picks the block under the screen center (a voxel raycast from the active camera) and applies
/// this frame's click. Runs before the fly camera, so the cursor state is the one the click was
/// made with, and before the world's systems (`WorldSystems::Interest`, then `Edits`), so it
/// reads the map unambiguously and the edit shows this frame.
fn pick_blocks(
    physics: PhysicsQuery,
    cameras: Query<(&Camera, &Transform)>,
    cursor: Res<CursorOptions>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    content: Res<WorldContent>,
    mut picking: ResMut<Picking>,
    mut edits: ResMut<EditQueue>,
    lines: Option<ResMut<OverlayLines>>,
) {
    if let Some(slot) = HOTBAR_KEYS.iter().position(|&key| keys.just_pressed(key))
        && slot < picking.hotbar.len()
    {
        picking.selected = slot;
    }
    let target = cameras
        .iter()
        .find(|(camera, _)| camera.active)
        .and_then(|(_, transform)| {
            physics
                .raycast_voxels(
                    WorldPos::from_dvec3(transform.translation),
                    transform.forward().as_dvec3(),
                    REACH,
                    VoxelFilter::Outline { fluids: false },
                )
                .hit()
                .copied()
        });
    if picking.target != target {
        picking.target = target;
    }
    let click = Click::from_input(&buttons, cursor.grab == CursorGrab::Locked);
    if let (Some(hit), Some(click)) = (target, click) {
        // The placement check: never a block inside the player (or any solid body).
        let free = |pos: BlockPos| match physics.probe(pos) {
            VoxelProbe::Block(state) => {
                (state.is_air() || physics.collision().is_fluid(state))
                    && physics.block_is_free(pos)
            }
            VoxelProbe::Unloaded => false,
        };
        if let Some(edit) = click_edit(click, &hit, picking.block(), free) {
            edits.set_block(edit.pos, edit.state);
        }
    }
    if let Some(mut lines) = lines {
        let [target, place] = picking_lines(&picking, &content.registry);
        if lines.get("pick target") != Some(target.as_str()) {
            lines.set("pick target", target);
        }
        if lines.get("pick place") != Some(place.as_str()) {
            lines.set("pick place", place);
        }
    }
}

/// Block picking and editing: [`Picking`] (built from the world's registry in `finish`) and
/// [`pick_blocks`] in `Update`. Needs `WorldPlugin`, `PhysicsPlugin` and input.
struct PickingPlugin;

impl Plugin for PickingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            pick_blocks
                .before(FlyCameraSystems)
                .before(WorldSystems::Interest),
        );
    }

    fn finish(&self, app: &mut App) {
        let registry = std::sync::Arc::clone(&app.world().resource::<WorldContent>().registry);
        app.insert_resource(Picking::new(&registry));
    }
}

/// Kills the process if `main` has not returned `limit` after start (the event loop stopped
/// updating, so the in-app timeout cannot fire). Dropping the returned sender disarms it.
fn spawn_watchdog(limit: Duration) -> Sender<()> {
    let (disarm, armed) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if armed.recv_timeout(limit) == Err(RecvTimeoutError::Timeout) {
            eprintln!("boxel: watchdog: still running after {limit:?}, exiting with 4");
            std::process::exit(4);
        }
    });
    disarm
}

fn main() -> AppExit {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (gpu, gpu_config, rest) = match GpuPreferences::from_args(&args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("boxel: {e}\n\n{}\n\n{CLI_HELP}", usage());
            return AppExit::from_code(1);
        }
    };
    let options = match parse_options(&rest) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{}\n\n{CLI_HELP}", usage());
            return AppExit::Success;
        }
        Err(message) => {
            eprintln!("boxel: {message}\n\n{}\n\n{CLI_HELP}", usage());
            return AppExit::from_code(1);
        }
    };
    let _watchdog = options
        .timeout
        .map(|timeout| spawn_watchdog(timeout.saturating_add(WATCHDOG_GRACE)));

    let generator = options.world.generator(options.seed);
    let (arena_quads, max_chunk_slots) = options.world.render_capacity();
    let world_settings = WorldSettings {
        vertical_bounds: options.fixed_height.then(|| generator.vertical_chunks()),
        ..WorldSettings::default()
    };
    let mut plugins = DefaultPlugins
        .set(WindowPlugin {
            primary: WindowSettings {
                title: "boxel voxel_world".to_string(),
                width: options.size.0,
                height: options.size.1,
                ..WindowSettings::default()
            },
            ..WindowPlugin::default()
        })
        .set(RenderPlugin {
            settings: RenderSettings {
                app_name: "voxel_world".to_string(),
                gpu,
                gpu_config,
                // Only `--frames` sets it. `--minimize-test` alone keeps the interactive path on
                // purpose: nothing is rendered while hidden, as in a normal run.
                render_when_hidden: options.frames.is_some(),
                arena_quads,
                max_chunk_slots,
                ..RenderSettings::default()
            },
        })
        .set(WorldPlugin {
            generator,
            settings: world_settings,
            ..WorldPlugin::default()
        })
        .set(DebugOverlayPlugin {
            visible: options.debug_overlay,
            // F5 cycles the frame limit, F6 toggles vsync.
            frame_keys: Some(FrameKeys::default()),
        });
    if let Some(threads) = options.async_threads {
        plugins = plugins.set(TaskPoolPlugin {
            task_pool_options: TaskPoolOptions {
                async_compute: TaskPoolThreadAssignmentPolicy {
                    min_threads: threads,
                    max_threads: threads,
                    percent: 0.4,
                },
                ..TaskPoolOptions::default()
            },
        });
    }

    let fps_limit = options.fps_limit;
    let vsync = options.vsync;
    let mut app = App::new();
    app.add_plugins(plugins);
    // After the plugins, so these replace the engine defaults (144 fps, vsync off).
    if let Some(fps) = fps_limit {
        app.insert_resource(FrameLimit::new(fps));
    }
    if let Some(on) = vsync {
        app.insert_resource(PresentSettings::vsync(on));
    }
    let interactive = !options.automation();
    let start_walking = options.walk;
    app.add_plugins((
        LogDiagnosticsPlugin::default(),
        FlyCameraPlugin,
        // At most one body (the walking player): keep the engine's 8 fixed steps per frame, so
        // walking keeps real time down to 8 fps (3 steps would slow it down below 21 fps).
        PhysicsPlugin {
            max_steps_per_frame: None,
            ..PhysicsPlugin::default()
        },
        PickingPlugin,
        WalkPlugin { start_walking },
    ));
    // The camera moves first; the world then computes interest from the new position.
    app.configure_sets(Update, FlyCameraSystems.before(WorldSystems::Interest))
        .insert_resource(Run::new(options))
        .init_resource::<FixedSteps>()
        .add_systems(Startup, spawn_camera)
        .add_systems(FixedUpdate, count_fixed_steps)
        .add_systems(
            Update,
            (
                gpu_events,
                drive,
                check_screenshot,
                minimize_test,
                vsync_test,
            )
                .chain(),
        );
    if interactive {
        // Aiming aid for picking; automation runs keep their screenshots unchanged.
        app.insert_resource(Crosshair {
            visible: true,
            ..Crosshair::default()
        });
    }
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Options {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        parse_options(&args)
            .expect("valid options")
            .expect("not --help")
    }

    #[test]
    fn world_options() {
        let o = parse(&[]);
        assert_eq!(
            (o.world.label(), o.world.trees),
            ("forest".to_string(), true)
        );
        let o = parse(&["--world", "overworld", "--caves", "off"]);
        assert_eq!(o.world.label(), "overworld/off");
        assert_eq!(o.world.render_capacity(), (24 << 20, 65_536));
        assert_eq!(parse(&["--no-trees"]).world.label(), "terrain");
        for args in [
            &["--world", "overworld", "--fixed-height"][..],
            &["--caves", "closed"],
        ] {
            let args: Vec<String> = args.iter().map(ToString::to_string).collect();
            assert!(parse_options(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn camera_far_away_is_accepted_up_to_the_world_limit() {
        let o = parse(&["--camera", "3e9,80,-1e12,10,-5"]);
        assert_eq!(o.camera, [3e9, 80.0, -1e12, 10.0, -5.0]);
        for camera in ["3e18,80,0,0,0", "0,-2.4e18,0,0,0", "0,0,1e300,0,0"] {
            let args: Vec<String> = ["--camera", camera].map(String::from).to_vec();
            let err = parse_options(&args).expect_err(camera);
            assert!(err.contains("outside the world"), "{err}");
        }
    }

    #[test]
    fn duration_keeps_the_interactive_path_and_outlasts_the_default_timeout() {
        let short = parse(&["--duration", "8"]);
        assert!(!short.automation(), "fly-camera input stays on");
        assert_eq!(short.timeout, Some(DEFAULT_TIMEOUT));

        // Longer than the 120 s default: the timeout grows, so the run ends with exit 0.
        let long = parse(&["--duration", "150"]);
        assert!(!long.automation());
        assert_eq!(long.timeout, Some(Duration::from_secs(180)));
        let long = parse(&["--duration", "150", "--frames", "10"]);
        assert!(long.automation());
        assert_eq!(long.timeout, Some(Duration::from_secs(180)));

        // An explicit timeout wins.
        let capped = parse(&["--duration", "150", "--timeout", "20"]);
        assert_eq!(capped.timeout, Some(Duration::from_secs(20)));
    }

    #[test]
    fn automation_flags_turn_input_off_and_get_the_default_timeout() {
        for args in [
            &["--frames", "10"][..],
            &["--minimize-test"],
            &["--resize-test"],
            &["--screenshot", "shot.png"],
            &["--vsync-test"],
        ] {
            let o = parse(args);
            assert!(o.automation(), "{args:?}");
            assert_eq!(o.timeout, Some(DEFAULT_TIMEOUT), "{args:?}");
        }
        let interactive = parse(&[]);
        assert!(!interactive.automation());
        assert_eq!(interactive.timeout, None, "a plain run has no timeout");
    }

    /// A headless app with the world (stone y 0..=2, grass at y = 3, inline jobs), physics,
    /// the fly camera, picking and walking, settled; the cursor is free and a fly camera at
    /// (0.5, 6, 0.5) looks straight down at the grass (top face at y = 4). `Update` is strict
    /// about ambiguities, so picking must be ordered against the fly camera's grab (both use
    /// `CursorOptions`), and walking against the camera's moves. Every update is one fixed step
    /// (a synthetic clock).
    fn picking_app() -> App {
        use boxel::ecs::schedule::{LogLevel, ScheduleBuildSettings};
        use boxel::world::jobs::InlineExecutor;
        use boxel::worldgen::FlatGenerator;
        let mut app = App::new();
        app.add_plugins((
            TimePlugin,
            InputPlugin,
            FlyCameraPlugin,
            WorldPlugin {
                generator: std::sync::Arc::new(FlatGenerator::new(vec![
                    ("boxel:stone".to_string(), 3),
                    ("boxel:grass".to_string(), 1),
                ])),
                executor: std::sync::Arc::new(InlineExecutor),
                ..WorldPlugin::default()
            },
            PhysicsPlugin::default(),
            PickingPlugin,
            WalkPlugin::default(),
        ))
        .init_resource::<OverlayLines>()
        .edit_schedule(Update, |schedule| {
            schedule.set_build_settings(ScheduleBuildSettings {
                ambiguity_detection: LogLevel::Error,
                ..ScheduleBuildSettings::default()
            });
        });
        player::clock::fixed_frames(&mut app, player::clock::STEP);
        let pitch = -std::f32::consts::FRAC_PI_2;
        app.world_mut().spawn((
            Camera::default(),
            FlyCamera {
                pitch,
                ..FlyCamera::default()
            },
            Transform::from_yaw_pitch(DVec3::new(0.5, 6.0, 0.5), 0.0, pitch),
            ChunkViewer::new(1).with_vertical_radius(1),
        ));
        app.finish();
        app.cleanup();
        let start = Instant::now();
        loop {
            app.update();
            let stats = *app.world().resource::<WorldStats>();
            if stats.chunks > 0 && stats.is_settled() {
                // Picking runs early in `Update`: one more frame so it sees the settled world.
                app.update();
                return app;
            }
            assert!(start.elapsed() < Duration::from_secs(60), "{stats:?}");
        }
    }

    /// Presses and releases `button` over two updates, as the window would.
    fn click(app: &mut App, button: MouseButton) {
        use boxel::window::{ButtonState, MouseButtonInput};
        for state in [ButtonState::Pressed, ButtonState::Released] {
            app.world_mut()
                .resource_mut::<Messages<MouseButtonInput>>()
                .write(MouseButtonInput { button, state });
            app.update();
        }
    }

    fn block_at(app: &App, pos: BlockPos) -> Option<BlockState> {
        app.world().resource::<ChunkMap>().block_at(pos)
    }

    fn grab(app: &App) -> CursorGrab {
        app.world().resource::<CursorOptions>().grab
    }

    #[test]
    fn clicks_break_and_place_the_picked_block() {
        use boxel::window::{ButtonState, KeyboardInput, WindowFocused};
        let mut app = picking_app();
        let grass = BlockRegistry::default_blocks()
            .by_name("boxel:grass")
            .expect("built-in");
        // The first click only grabs the cursor (the fly camera does it in the same frame,
        // after picking): the grass stays.
        assert_eq!(grab(&app), CursorGrab::None);
        click(&mut app, MouseButton::Left);
        assert_eq!(grab(&app), CursorGrab::Locked);
        assert_eq!(block_at(&app, BlockPos::new(0, 3, 0)), Some(grass));
        let target = app
            .world()
            .resource::<Picking>()
            .target
            .expect("grass below");
        assert_eq!(
            (target.block, target.face),
            (BlockPos::new(0, 3, 0), Some(Face::PosY))
        );
        // The f32 camera forward is not exactly -Y.
        assert!((target.t - 2.0).abs() < 1e-6, "{}", target.t);
        let lines = app.world().resource::<OverlayLines>();
        assert_eq!(
            lines.get("pick target"),
            Some("Target boxel:grass 0 3 0  face +Y")
        );
        assert_eq!(
            lines.get("pick place"),
            Some("Place [1] boxel:stone  (keys 1-9)")
        );

        // Left click breaks the grass; the stone under it becomes the target.
        click(&mut app, MouseButton::Left);
        assert_eq!(
            block_at(&app, BlockPos::new(0, 3, 0)),
            Some(BlockState::AIR)
        );
        let target = app
            .world()
            .resource::<Picking>()
            .target
            .expect("stone below");
        assert_eq!(target.block, BlockPos::new(0, 2, 0));

        // Key 3 selects grass; right click places it on the stone's top face.
        app.world_mut()
            .resource_mut::<Messages<KeyboardInput>>()
            .write(KeyboardInput {
                key: KeyCode::Digit3,
                state: ButtonState::Pressed,
                repeat: false,
            });
        app.update();
        click(&mut app, MouseButton::Right);
        assert_eq!(block_at(&app, BlockPos::new(0, 3, 0)), Some(grass));
        assert_eq!(
            app.world().resource::<OverlayLines>().get("pick place"),
            Some("Place [3] boxel:grass  (keys 1-9)")
        );

        // Alt-tab frees the cursor; the click that focuses the window again only grabs it.
        app.world_mut().write_message(WindowFocused(false));
        app.update();
        assert_eq!(grab(&app), CursorGrab::None);
        app.world_mut().write_message(WindowFocused(true));
        click(&mut app, MouseButton::Left);
        assert_eq!(grab(&app), CursorGrab::Locked);
        assert_eq!(block_at(&app, BlockPos::new(0, 3, 0)), Some(grass));
    }

    /// Presses or releases `key` (seen by the next update).
    fn key(app: &mut App, key: KeyCode, pressed: bool) {
        use boxel::window::{ButtonState, KeyboardInput};
        let state = if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        };
        app.world_mut()
            .resource_mut::<Messages<KeyboardInput>>()
            .write(KeyboardInput {
                key,
                state,
                repeat: false,
            });
    }

    /// Taps `key`: pressed for one update, released in the next.
    fn tap(app: &mut App, k: KeyCode) {
        key(app, k, true);
        app.update();
        key(app, k, false);
        app.update();
    }

    fn walker(app: &mut App) -> Option<Entity> {
        let mut players = app
            .world_mut()
            .query_filtered::<Entity, With<player::Player>>();
        players.iter(app.world()).next()
    }

    fn camera_at(app: &mut App) -> DVec3 {
        let mut cameras = app
            .world_mut()
            .query_filtered::<&Transform, With<FlyCamera>>();
        cameras.single(app.world()).expect("camera").translation
    }

    #[test]
    fn g_walks_with_gravity_and_jumps_then_flies_again() {
        use boxel::physics::{BodyPosition, Contacts};
        use player::{EYE_HEIGHT, WALK_KEY};
        let mut app = picking_app();
        assert!(walker(&mut app).is_none());
        let line = |app: &App, key: &str| {
            app.world()
                .resource::<OverlayLines>()
                .get(key)
                .unwrap_or_default()
                .to_string()
        };
        assert_eq!(line(&app, "move"), "Fly (G walks)");

        // G: the player appears under the camera's eye and falls onto the grass (top at y = 4).
        tap(&mut app, WALK_KEY);
        for _ in 0..64 {
            app.update();
        }
        let body = walker(&mut app).expect("walking");
        let feet = |app: &App| app.world().get::<BodyPosition>(body).expect("body").0;
        assert_eq!(feet(&app), WorldPos::from_dvec3(DVec3::new(0.5, 4.0, 0.5)));
        assert_eq!(camera_at(&mut app), DVec3::new(0.5, 4.0 + EYE_HEIGHT, 0.5));
        assert_eq!(
            line(&app, "move"),
            "Walk (G flies)  on ground  speed 0.00 b/s"
        );
        // Standing still for half a second, the player body fell asleep (W wakes it).
        assert!(
            line(&app, "physics").starts_with("Physics bodies=1 dynamic=0 frozen=0 sleeping=1"),
            "{}",
            line(&app, "physics")
        );

        // W walks along the camera's heading (yaw 0: north, -Z) at about 4.3 blocks/s.
        key(&mut app, KeyCode::KeyW, true);
        for _ in 0..64 {
            app.update();
        }
        key(&mut app, KeyCode::KeyW, false);
        app.update();
        let p = feet(&app).to_dvec3();
        assert_eq!((p.x, p.y), (0.5, 4.0));
        assert!(p.z < -2.5, "{p:?}");

        // Space jumps about 1.24 blocks and lands again.
        tap(&mut app, KeyCode::Space);
        let mut top: f64 = 4.0;
        for _ in 0..64 {
            app.update();
            top = top.max(feet(&app).to_dvec3().y);
        }
        assert!((5.1..5.3).contains(&top), "{top}");
        assert_eq!(feet(&app).to_dvec3().y, 4.0);
        assert!(app.world().get::<Contacts>(body).expect("body").on_ground);

        // A tap shorter than a frame (pressed and released before the same update, as in a
        // long frame) still jumps.
        key(&mut app, KeyCode::Space, true);
        key(&mut app, KeyCode::Space, false);
        let mut top: f64 = 4.0;
        for _ in 0..64 {
            app.update();
            top = top.max(feet(&app).to_dvec3().y);
        }
        assert!((5.1..5.3).contains(&top), "{top}");
        assert_eq!(feet(&app).to_dvec3().y, 4.0);

        // G again: flying, the body is gone and Space flies the camera up.
        tap(&mut app, WALK_KEY);
        assert!(walker(&mut app).is_none());
        assert_eq!(line(&app, "move"), "Fly (G walks)");
        let before = camera_at(&mut app);
        key(&mut app, KeyCode::Space, true);
        for _ in 0..10 {
            app.update();
        }
        assert!(camera_at(&mut app).y > before.y + 2.0);
    }

    #[test]
    fn right_click_never_places_a_block_inside_the_player() {
        use player::WALK_KEY;
        let mut app = picking_app();
        // Walking: the player lands on the grass under the camera, which looks straight down
        // from its eye at the grass under its feet.
        tap(&mut app, WALK_KEY);
        for _ in 0..64 {
            app.update();
        }
        assert!(walker(&mut app).is_some());
        let target = app
            .world()
            .resource::<Picking>()
            .target
            .expect("grass below");
        assert_eq!(
            (target.block, target.face),
            (BlockPos::new(0, 3, 0), Some(Face::PosY))
        );
        // The first click grabs the cursor; the right click would put stone in the player's
        // feet: refused.
        click(&mut app, MouseButton::Left);
        assert_eq!(grab(&app), CursorGrab::Locked);
        click(&mut app, MouseButton::Right);
        assert_eq!(
            block_at(&app, BlockPos::new(0, 4, 0)),
            Some(BlockState::AIR)
        );
        // Flying, the body is gone: the same click places it.
        tap(&mut app, WALK_KEY);
        assert!(walker(&mut app).is_none());
        click(&mut app, MouseButton::Right);
        let stone = BlockRegistry::default_blocks()
            .by_name("boxel:stone")
            .expect("built-in");
        assert_eq!(block_at(&app, BlockPos::new(0, 4, 0)), Some(stone));
    }

    #[test]
    fn click_edits_need_a_face_a_free_cell_and_a_block() {
        let stone = BlockState(1);
        let hit = VoxelHit {
            block: BlockPos::new(1, 2, 3),
            face: Some(Face::NegX),
            t: 1.0,
            point: WorldPos::default(),
            state: stone,
        };
        assert_eq!(
            click_edit(Click::Break, &hit, stone, |_| false),
            Some(BlockEdit {
                pos: hit.block,
                state: BlockState::AIR
            })
        );
        assert_eq!(
            click_edit(Click::Place, &hit, stone, |p| p == BlockPos::new(0, 2, 3)),
            Some(BlockEdit {
                pos: BlockPos::new(0, 2, 3),
                state: stone
            })
        );
        assert_eq!(
            click_edit(Click::Place, &hit, stone, |_| false),
            None,
            "occupied"
        );
        assert_eq!(
            click_edit(Click::Place, &hit, BlockState::AIR, |_| true),
            None,
            "nothing selected"
        );
        let inside = VoxelHit { face: None, ..hit };
        assert_eq!(
            click_edit(Click::Place, &inside, stone, |_| true),
            None,
            "no face"
        );
        let mut buttons = ButtonInput::<MouseButton>::default();
        buttons.press(MouseButton::Right);
        assert_eq!(Click::from_input(&buttons, true), Some(Click::Place));
        assert_eq!(
            Click::from_input(&buttons, false),
            None,
            "the click that grabs"
        );
    }

    #[test]
    fn duration_run_completes_once_the_time_is_up() {
        let info = RenderInfo::default();
        assert!(Run::new(parse(&["--duration", "0"])).complete(&info));
        assert!(!Run::new(parse(&["--duration", "3600"])).complete(&info));
    }
}
