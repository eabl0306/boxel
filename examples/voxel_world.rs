//! Windowed voxel terrain demo.
//!
//! `cargo run --release --example voxel_world -- [GPU options] [options]` (see `--help`).
//! Interactive by default: left click grabs the mouse, WASD/Space/`ShiftLeft` fly, `ControlLeft`
//! boosts, Escape releases the cursor, F3 toggles the debug overlay, F5 cycles the frame limit
//! (30, 60, 144, 240, unlimited) and F6 toggles vsync. `--frames`, `--duration`, `--screenshot`,
//! `--minimize-test`, `--resize-test`, `--vsync-test` and the like make it close by itself
//! (`--duration` keeps the interactive path: fly camera on, nothing rendered while hidden); the
//! last summary line gives the update and presented frame rates and the CPU use (Linux).
//!
//! Exit codes: 0 success, 1 usage error, 2 no usable GPU, 3 check failed (screenshot, image
//! diff, resize, minimize), 4 timeout.
//!
//! The terrain is `TerrainGenerator` plus a few deterministic trees (log trunk, leaf canopy) so
//! the cutout pipeline is visible; `--no-trees` gives the plain generator.
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

use boxel::app::{TaskPoolOptions, TaskPoolThreadAssignmentPolicy};
use boxel::camera::FlyCameraSystems;
use boxel::gfx::wgpu::PresentMode;
use boxel::gfx::{CLI_HELP, DeviceClass, RgbaImage};
use boxel::prelude::*;
use boxel::render::{DebugDestroyDevice, GpuFailed, GpuReady, SurfaceInfo};
use boxel::voxel::{
    BlockRegistry, BlockState, CHUNK_SIZE, ChunkData, ChunkKey, LocalPos, MAX_BLOCK_COORD,
};
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
  --no-trees              plain TerrainGenerator (no trees, so no cutout leaves)
  --debug-overlay         start with the F3 debug overlay visible (F3 toggles it)
  --help                  this text
exit codes: 0 ok, 1 usage error, 2 no usable GPU, 3 check failed, 4 timeout";

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
    trees: bool,
    debug_overlay: bool,
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
        trees: true,
        debug_overlay: false,
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
            "--no-trees" => o.trees = false,
            "--debug-overlay" => o.debug_overlay = true,
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
// Terrain with trees

/// Trees: at most one per `TREE_CELL`^2 columns, on grass only.
const TREE_CELL: i64 = 8;
/// Canopy reach around the trunk.
const CANOPY_RADIUS: i64 = 2;
/// Shortest trunk, and the tallest tree above the ground (trunk + 2 canopy layers above it).
const TRUNK_MIN: i64 = 4;
const TREE_HEIGHT_MAX: i64 = TRUNK_MIN + 2 + 2;
/// Salt so the tree lattice is not correlated with the height noise.
const TREE_SALT: u64 = 0x007e_e50f_b0e1;

/// [`TerrainGenerator`] plus deterministic trees. Each chunk places the parts of every tree
/// whose blocks reach into it (trunk column heights come from `height_at`), so trees crossing
/// chunk borders are consistent without neighbor data.
struct Forest {
    terrain: TerrainGenerator,
}

/// World coordinates are `i64` (cells, trunks and blocks); inside the former `i32` range the
/// trees are the same as before (same cells, same `hash2` values).
#[derive(Clone, Copy, Debug)]
struct Tree {
    x: i64,
    z: i64,
    ground: i64,
    trunk: i64,
}

impl Forest {
    fn tree_in_cell(&self, cx: i64, cz: i64) -> Option<Tree> {
        let h = hash2(self.terrain.seed ^ TREE_SALT, cx, cz);
        if !h.is_multiple_of(3) {
            return None;
        }
        let span = (TREE_CELL - 2 * CANOPY_RADIUS) as u32;
        let x = cx * TREE_CELL + CANOPY_RADIUS + i64::from((h >> 8) % span);
        let z = cz * TREE_CELL + CANOPY_RADIUS + i64::from((h >> 16) % span);
        let ground = self.terrain.height_at(x, z);
        let s = &self.terrain.settings;
        if ground <= s.sand_level || ground > s.snow_level {
            return None;
        }
        Some(Tree {
            x,
            z,
            ground: ground.into(),
            trunk: TRUNK_MIN + i64::from((h >> 24) % 3),
        })
    }
}

impl Tree {
    /// Calls `put(x, y, z, is_log)` for every block of the tree.
    fn blocks(self, mut put: impl FnMut(i64, i64, i64, bool)) {
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
        let size = i64::from(CHUNK_SIZE);
        if o.y + size <= i64::from(s.sand_level) + 1
            || o.y > i64::from(s.snow_level) + TREE_HEIGHT_MAX
        {
            return data;
        }
        // Tree cells that may reach into the chunk (no overflow: origins stay within the world
        // limit, +-2^61).
        let cells = |lo: i64| {
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

    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        let inner = self.terrain.vertical_chunks();
        let tree_top = (i64::from(self.terrain.settings.snow_level) + TREE_HEIGHT_MAX) >> 5;
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
    app.add_plugins((LogDiagnosticsPlugin::default(), FlyCameraPlugin))
        // The camera moves first; the world then computes interest from the new position.
        .configure_sets(Update, FlyCameraSystems.before(WorldSystems::Interest))
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
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel::voxel::{CHUNK_VOLUME, ChunkPos};

    /// Fingerprint (log count, FNV-1a of every voxel state) of the 3x3 chunk columns from
    /// `(x, z)`, layers 1..=4. Same check as in `examples/stress/forest.rs`: the two Forest
    /// copies must stay in sync.
    fn forest_fingerprint(
        forest: &Forest,
        registry: &BlockRegistry,
        x: i64,
        z: i64,
    ) -> (usize, u64) {
        let log = registry.by_name("boxel:log");
        let (mut logs, mut h) = (0, 0xcbf2_9ce4_8422_2325u64);
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        for dz in 0..3 {
            for dx in 0..3 {
                for y in 1..=4 {
                    let key = ChunkKey::new(ChunkPos::new(x + dx, y, z + dz));
                    forest.generate(key, registry).to_dense(&mut dense);
                    logs += dense.iter().filter(|s| Some(**s) == log).count();
                    for s in &dense {
                        for b in u64::from(s.0).to_le_bytes() {
                            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
                        }
                    }
                }
            }
        }
        (logs, h)
    }

    #[test]
    fn forest_inside_i32_is_unchanged_and_crosses_the_former_edge() {
        let forest = Forest {
            terrain: TerrainGenerator::default(),
        };
        let registry = BlockRegistry::default_blocks();
        // Recorded with the i32 coordinates (the last chunks before the old edge included).
        for ((x, z), expected) in [
            ((0, 0), (259, 0x0e4e_2255_79a0_f38d)),
            ((-3, 5), (284, 0xa713_c5ca_ff29_5b64)),
            ((7, -9), (6, 0x2418_de68_72ea_8247)),
            ((31_250, -31_250), (250, 0x9a87_d49f_8d57_45e4)),
            ((67_108_860, -67_108_863), (224, 0xdfd3_f57c_9c42_38a5)),
            ((-67_108_863, 67_108_860), (230, 0x96e8_7562_ea15_86c5)),
            ((12_345_678, -8_765_432), (245, 0x7995_606d_8843_dc2c)),
            ((-67_108_000, -67_100_000), (243, 0x97e4_a0c8_100c_a62c)),
        ] {
            assert_eq!(
                forest_fingerprint(&forest, &registry, x, z),
                expected,
                "({x}, {z})"
            );
        }
        // Across +-2^31 voxels (chunks +-2^26), where the i32 tree math overflowed: trees keep
        // growing on both sides.
        let edge = 1i64 << 26;
        for (x, z) in [(edge - 1, 0), (edge + 2, 5), (-edge - 2, -edge - 1)] {
            let (logs, _) = forest_fingerprint(&forest, &registry, x, z);
            assert!(logs > 0, "no trees at chunk ({x}, {z})");
        }
    }

    fn parse(args: &[&str]) -> Options {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        parse_options(&args)
            .expect("valid options")
            .expect("not --help")
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

    #[test]
    fn duration_run_completes_once_the_time_is_up() {
        let info = RenderInfo::default();
        assert!(Run::new(parse(&["--duration", "0"])).complete(&info));
        assert!(!Run::new(parse(&["--duration", "3600"])).complete(&info));
    }
}
