//! Windowed voxel terrain demo.
//!
//! `cargo run --release --example voxel_world -- [GPU options] [options]` (see `--help`).
//! Interactive by default: left click grabs the mouse, WASD/Space/`ShiftLeft` fly, `ControlLeft`
//! boosts, Escape releases the cursor, F3 toggles the debug overlay. The automation flags
//! (`--frames`, `--screenshot`, `--minimize-test`, `--resize-test`, ...) make it close by
//! itself.
//!
//! Exit codes: 0 success, 1 usage error, 2 no usable GPU, 3 check failed (screenshot, image
//! diff, resize, minimize), 4 timeout.
//!
//! The terrain is `TerrainGenerator` plus a few deterministic trees (log trunk, leaf canopy) so
//! the cutout pipeline is visible; `--no-trees` gives the plain generator.
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

use boxel::app::{TaskPoolOptions, TaskPoolThreadAssignmentPolicy};
use boxel::camera::FlyCameraSystems;
use boxel::gfx::{CLI_HELP, DeviceClass, RgbaImage};
use boxel::prelude::*;
use boxel::render::{DebugDestroyDevice, GpuFailed, GpuReady, SurfaceInfo};
use boxel::voxel::{BlockRegistry, BlockState, CHUNK_SIZE, ChunkData, ChunkKey, LocalPos};
use boxel::window::winit::dpi::PhysicalSize;
use boxel::window::{PrimaryWindow, WindowResized};
use boxel::world::{WorldStats, WorldSystems};
use boxel::worldgen::TerrainSettings;
use boxel::worldgen::noise::hash2;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

const USAGE: &str = "\
usage: voxel_world [GPU options] [options]
  --frames N              exit 0 after N rendered frames (automation: renders while hidden,
                          disables fly-camera input)
  --wait-chunks           start counting frames once the world settled for 3 frames
  --timeout SECS          wall-clock limit, exit 4 (default 120 with automation flags)
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
  --size WxH              window size in logical pixels (default 1280x720)
  --camera x,y,z,yaw,pitch  initial camera, degrees (default 0,120,0,45,-30)
  --simulate-device-loss N[,M..]  destroy the GPU device at frame N (recovery check; a
                          list repeats it: the second loss of an adapter lowers its tier)
  --async-threads N       async compute pool size (chunk jobs)
  --no-trees              plain TerrainGenerator (no trees, so no cutout leaves)
  --debug-overlay         start with the F3 debug overlay visible (F3 toggles it)
  --help                  this text
exit codes: 0 ok, 1 usage error, 2 no usable GPU, 3 check failed, 4 timeout";

/// Default frames when a flag implies `--frames`.
const IMPLIED_FRAMES: u64 = 120;
/// Default `--timeout` for automation runs.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
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
    size: (u32, u32),
    camera: [f64; 5],
    /// Frames at which the device is destroyed, ascending.
    simulate_device_loss: Vec<u64>,
    async_threads: Option<usize>,
    trees: bool,
    debug_overlay: bool,
}

impl Options {
    fn automation(&self) -> bool {
        self.frames.is_some() || self.minimize_test || self.resize_test
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
        size: (1280, 720),
        camera: [0.0, 120.0, 0.0, 45.0, -30.0],
        simulate_device_loss: Vec::new(),
        async_threads: None,
        trees: true,
        debug_overlay: false,
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
            "--no-trees" => o.trees = false,
            "--debug-overlay" => o.debug_overlay = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if (o.expect_terrain || o.compare_to.is_some()) && o.screenshot.is_none() {
        return Err("--expect-terrain and --compare-to need --screenshot".to_string());
    }
    if o.frames.is_none() && (o.screenshot.is_some() || o.resize_test) {
        o.frames = Some(IMPLIED_FRAMES);
    }
    if o.frames.is_none() && (o.wait_chunks || !o.simulate_device_loss.is_empty()) {
        return Err("--wait-chunks and --simulate-device-loss need --frames".to_string());
    }
    if o.automation() && o.timeout.is_none() {
        o.timeout = Some(DEFAULT_TIMEOUT);
    }
    Ok(Some(o))
}

fn number(flag: &str, value: &str) -> Result<u64, String> {
    value
        .parse()
        .map_err(|_| format!("{flag}: {value:?} is not a non-negative integer"))
}

// ---------------------------------------------------------------------------------------------
// Terrain with trees

/// Trees: at most one per `TREE_CELL`^2 columns, on grass only.
const TREE_CELL: i32 = 8;
/// Canopy reach around the trunk.
const CANOPY_RADIUS: i32 = 2;
/// Shortest trunk, and the tallest tree above the ground (trunk + 2 canopy layers above it).
const TRUNK_MIN: i32 = 4;
const TREE_HEIGHT_MAX: i32 = TRUNK_MIN + 2 + 2;
/// Salt so the tree lattice is not correlated with the height noise.
const TREE_SALT: u64 = 0x007e_e50f_b0e1;

/// [`TerrainGenerator`] plus deterministic trees. Each chunk places the parts of every tree
/// whose blocks reach into it (trunk column heights come from `height_at`), so trees crossing
/// chunk borders are consistent without neighbor data.
struct Forest {
    terrain: TerrainGenerator,
}

#[derive(Clone, Copy, Debug)]
struct Tree {
    x: i32,
    z: i32,
    ground: i32,
    trunk: i32,
}

impl Forest {
    fn tree_in_cell(&self, cx: i32, cz: i32) -> Option<Tree> {
        let h = hash2(self.terrain.seed ^ TREE_SALT, cx, cz);
        if !h.is_multiple_of(3) {
            return None;
        }
        let span = (TREE_CELL - 2 * CANOPY_RADIUS) as u32;
        let x = cx * TREE_CELL + CANOPY_RADIUS + ((h >> 8) % span) as i32;
        let z = cz * TREE_CELL + CANOPY_RADIUS + ((h >> 16) % span) as i32;
        let ground = self.terrain.height_at(x, z);
        let s = &self.terrain.settings;
        if ground <= s.sand_level || ground > s.snow_level {
            return None;
        }
        Some(Tree {
            x,
            z,
            ground,
            trunk: TRUNK_MIN + ((h >> 24) % 3) as i32,
        })
    }
}

impl Tree {
    /// Calls `put(x, y, z, is_log)` for every block of the tree.
    fn blocks(self, mut put: impl FnMut(i32, i32, i32, bool)) {
        let top = self.ground + self.trunk;
        for y in self.ground + 1..=top {
            put(self.x, y, self.z, true);
        }
        for y in top - 1..=top + 2 {
            let (radius, round) = match y - top {
                -1 | 0 => (CANOPY_RADIUS, true),
                1 => (1, false),
                _ => (1, true),
            };
            for dx in -radius..=radius {
                for dz in -radius..=radius {
                    let corner = dx.abs() == radius && dz.abs() == radius;
                    if (round && corner) || (dx == 0 && dz == 0 && y <= top) {
                        continue;
                    }
                    put(self.x + dx, y, self.z + dz, false);
                }
            }
        }
    }
}

impl WorldGenerator for Forest {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let mut data = self.terrain.generate(key, registry);
        let (Some(log), Some(leaves)) = (
            registry.by_name("boxel:log"),
            registry.by_name("boxel:leaves"),
        ) else {
            return data;
        };
        let s = &self.terrain.settings;
        let o = key.pos.origin();
        let size = CHUNK_SIZE;
        if o.y + size <= s.sand_level + 1 || o.y > s.snow_level + TREE_HEIGHT_MAX {
            return data;
        }
        let cells = |lo: i32| {
            (lo - CANOPY_RADIUS).div_euclid(TREE_CELL)
                ..=(lo + size - 1 + CANOPY_RADIUS).div_euclid(TREE_CELL)
        };
        for cx in cells(o.x) {
            for cz in cells(o.z) {
                let Some(tree) = self.tree_in_cell(cx, cz) else {
                    continue;
                };
                tree.blocks(|x, y, z, is_log| {
                    let (lx, ly, lz) = (x - o.x, y - o.y, z - o.z);
                    if [lx, ly, lz].iter().all(|c| (0..size).contains(c)) {
                        let p = LocalPos::new(lx as u32, ly as u32, lz as u32);
                        if data.get(p) == BlockState::AIR {
                            data.set(p, if is_log { log } else { leaves });
                        }
                    }
                });
            }
        }
        data
    }

    fn vertical_chunks(&self) -> RangeInclusive<i32> {
        let inner = self.terrain.vertical_chunks();
        let tree_top = (self.terrain.settings.snow_level + TREE_HEIGHT_MAX) >> 5;
        *inner.start()..=(*inner.end()).max(tree_top)
    }

    fn name(&self) -> &str {
        "terrain+trees"
    }
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
    exiting: bool,
}

impl Run {
    fn new(options: Options) -> Self {
        Self {
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

    /// Every requested check is done (`--frames` reached, screenshot checked, resize seen,
    /// minimize test finished).
    fn complete(&self, info: &RenderInfo) -> bool {
        let frames_done = match self.options.frames {
            Some(frames) => self.counted(info).is_some_and(|c| c >= frames),
            None => true,
        };
        frames_done
            && matches!(self.shot, Shot::Off | Shot::Done)
            && matches!(self.resize, Resize::Off | Resize::Done)
            && matches!(self.minimize, Minimize::Off | Minimize::Done)
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
        exit.write(AppExit::from_code(code));
    }
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
        ChunkViewer { radius },
    ));
}

/// `boxel: <summary>` on every (re)selection; exit 2 when no GPU can be used.
fn gpu_events(
    mut ready: MessageReader<GpuReady>,
    mut failed: MessageReader<GpuFailed>,
    mut run: ResMut<Run>,
    info: Res<RenderInfo>,
    mut viewers: Query<(&mut ChunkViewer, &mut Camera)>,
    mut exit: MessageWriter<AppExit>,
) {
    for event in ready.read() {
        println!("boxel: {}", event.summary);
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
    mut resized: MessageReader<WindowResized>,
    mut destroy: MessageWriter<DebugDestroyDevice>,
    mut screenshot: MessageWriter<RequestScreenshot>,
    mut exit: MessageWriter<AppExit>,
) {
    run.updates += 1;
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

    let Some(frames) = run.options.frames else {
        return;
    };
    if counted < frames {
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
            eprintln!("boxel: {e}\n\n{USAGE}\n\n{CLI_HELP}");
            return AppExit::from_code(1);
        }
    };
    let options = match parse_options(&rest) {
        Ok(Some(options)) => options,
        Ok(None) => {
            println!("{USAGE}\n\n{CLI_HELP}");
            return AppExit::Success;
        }
        Err(message) => {
            eprintln!("boxel: {message}\n\n{USAGE}\n\n{CLI_HELP}");
            return AppExit::from_code(1);
        }
    };
    let _watchdog = options
        .timeout
        .map(|timeout| spawn_watchdog(timeout.saturating_add(WATCHDOG_GRACE)));

    let terrain = TerrainGenerator::new(options.seed, TerrainSettings::default());
    let generator: Arc<dyn WorldGenerator> = if options.trees {
        Arc::new(Forest { terrain })
    } else {
        Arc::new(terrain)
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
                ..RenderSettings::default()
            },
        })
        .set(WorldPlugin {
            generator,
            ..WorldPlugin::default()
        })
        .set(DebugOverlayPlugin {
            visible: options.debug_overlay,
            ..DebugOverlayPlugin::default()
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

    let mut app = App::new();
    app.add_plugins(plugins)
        .add_plugins((LogDiagnosticsPlugin::default(), FlyCameraPlugin))
        // The camera moves first; the world then computes interest from the new position.
        .configure_sets(Update, FlyCameraSystems.before(WorldSystems::Interest))
        .insert_resource(Run::new(options))
        .init_resource::<FixedSteps>()
        .add_systems(Startup, spawn_camera)
        .add_systems(FixedUpdate, count_fixed_steps)
        .add_systems(
            Update,
            (gpu_events, drive, check_screenshot, minimize_test).chain(),
        );
    app.run()
}
