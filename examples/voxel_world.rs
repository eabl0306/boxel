//! Windowed voxel terrain demo.
//!
//! `cargo run --release --example voxel_world -- [GPU options] [options]` (see `--help`).
//! Interactive by default: left click grabs the mouse, WASD/Space/`ShiftLeft` fly, `ControlLeft`
//! boosts, Escape (or leaving the window) releases the cursor, F3 toggles the debug overlay, F5
//! cycles the frame limit (30, 60, 144, 240, unlimited) and F6 toggles vsync. While the mouse is
//! grabbed, left click breaks the block under the crosshair and right click places the selected
//! block on the face it points at (keys 1 to 9 and 0 select it, lamps on 7 to 0; the mouse wheel
//! scrolls through those and the rest: glass, the stone slab, short grass, a poppy, water, lava,
//! stone and oak stairs, the oak fence, the torch, the cobblestone wall and the oak fence gate;
//! F3 shows the target, the selection and the light at the eye and the crosshair), never where a
//! solid body is (the walking player: `PhysicsQuery::state_is_free`, against the placed block's
//! shape). A slab goes in as a top slab when the click hits the bottom of a block or the upper
//! half of a side, and a log lies along the axis of the clicked face; a slab placed on the free
//! half of a slab of the same block makes a double slab. Stairs face away from the player and
//! go upside down like top slabs, and take their corner shape from the stairs next to them; a
//! fence connects to its neighbors; a torch stands on a top face and hangs on a side face (ADR
//! 0020). A wall connects and raises its post like Minecraft's, a fence gate faces the way the
//! player looks and drops into a wall; right click on a gate opens or closes it instead of
//! placing (with Shift it places, as in Minecraft; `BlockUses`, ADR 0027). F3 shows the
//! target's full state (`boxel:log[axis=x]`). Picking is a voxel raycast from the camera
//! (`PhysicsQuery::raycast_voxels`), reach 6 blocks, and the targeted block gets
//! a thin dark outline (`BlockOutline`, ADR 0026) besides the crosshair. G switches between
//! flying and walking: walking is a player body with gravity (`boxel::physics`, a 0.6 x 1.8 x
//! 0.6 box and a `CharacterController`) that WASD moves along the view and Space makes jump,
//! with the camera at its eye. Walking, Shift sneaks (slower, crouched with a lower eye, never
//! off an edge), Ctrl sprints (a wider view; a sprint-jump goes further) and Space twice
//! toggles creative flight (Space rises, Shift descends, landing ends it); the view bobs while
//! walking (V turns it off). The fly camera keeps Shift and Ctrl for going down and boosting.
//! F3 shows the mode (with flying, sneaking, crouching or sprinting) and a physics line
//! (`PhysicsStats`) in both modes. Fences cannot be jumped: they collide 1.5 blocks tall, and
//! so do walls and closed gates. C drops a crate two blocks ahead (a solid dynamic box; bodies
//! have no renderer yet, so F3 counts them and gives the nearest).
//!
//! Water flows (ADR 0021, `FluidPlugin`): placed water is a source that spreads 7 blocks and
//! falls, breaking a block next to a lake or the sea lets it flow, and F3 shows the first water
//! along the view with its level (`boxel:water[level=3]`) and a line of the block ticks. Lava
//! flows 3 blocks and hardens next to water (ADR 0025). A slab, stairs or a fence placed into a
//! water source keeps it (waterlogged), water placed on a dry one fills it, and breaking a
//! waterlogged block leaves its water; a double slab is always dry.
//!
//! Day and night: a day lasts 20 minutes. `[` and `]` move the clock one hour back and forward,
//! T pauses it and F runs it 60 times faster; `--time HH:MM` sets the start (noon by default).
//! `--lamps` places the colored lamps and some lava around the start and begins at night.
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
//!
//! `--world-dir DIR` saves the world there (ADR 0024): the edits, the flowing water, the clock,
//! the camera and the crates are saved on exit (and edited chunks and crates as their chunks
//! unload, and every minute), and loaded on the next start with the same `DIR` (`--camera` and
//! `--time` win over the saved camera and clock).
//!
//! `--lod near|far|extreme` draws coarse terrain past the view distance, to the horizon (ADR
//! 0031, like the Distant Horizons mod); off by default (no LOD plugin at all). F8 cycles far,
//! extreme, hidden and near, F9 tints each level, F3 shows a `lod` line. Software adapters keep
//! it off.
//!
//! The terrain fades into the sky over the last stretch before the view distance (or the LOD's
//! horizon): the last 10 %, at most 64 blocks (`RenderSettings::distance_fog`).
//! `--no-distance-fog` starts without it (a hard edge) and F10 toggles it.
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

#[path = "shared/clock.rs"]
mod clock;
#[path = "shared/crates.rs"]
mod crates;
#[path = "shared/fog.rs"]
mod fog;
#[path = "shared/lod.rs"]
mod lod;
#[path = "shared/pack.rs"]
mod pack;
#[path = "shared/player.rs"]
mod player;
#[path = "shared/world.rs"]
mod world;

use boxel::app::{TaskPoolOptions, TaskPoolThreadAssignmentPolicy};
use boxel::camera::FlyCameraSystems;
use boxel::debug_overlay::{Crosshair, CrosshairTarget, OverlayLines};
use boxel::gfx::wgpu::PresentMode;
use boxel::gfx::{CLI_HELP, DeviceClass, RgbaImage};
use boxel::physics::{PhysicsPlugin, Probe as VoxelProbe, SwimPlugin};
use boxel::prelude::*;
use boxel::render::{DebugDestroyDevice, GpuFailed, GpuReady, SurfaceInfo, TextureSource};
use boxel::voxel::rules;
use boxel::voxel::{Axis, BlockRegistry, Facing, MAX_BLOCK_COORD, SlabHalf};
use boxel::window::winit::dpi::PhysicalSize;
use boxel::window::{PrimaryWindow, WindowResized};
use boxel::world::persist::{PersistPlugin, PersistSettings, WorldStore};
use boxel::world::{
    BlockEdit, BlockUses, FluidPlugin, TimeOfDay, WorldContent, WorldStats, WorldSystems,
};
use boxel_demo::rules::{Kind, Kinds};
use player::WalkPlugin;
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
  --no-distance-fog       no fade into the sky at the view distance or the LOD's horizon: the
                          terrain ends with a hard edge (F10 toggles the fade)
  --fixed-height          stream every layer of the generator and nothing else, whatever the
                          camera height (boxel M0 streaming, for comparisons)
  --size WxH              window size in logical pixels (default 1280x720)
  --camera x,y,z,yaw,pitch  initial camera, degrees (default 0,120,0,45,-30)
  --simulate-device-loss N[,M..]  destroy the GPU device at frame N (recovery check; a
                          list repeats it: the second loss of an adapter lowers its tier)
  --async-threads N       async compute pool size (chunk jobs)
  --debug-overlay         start with the F3 debug overlay visible (F3 toggles it)
  --walk                  start walking on the ground under the camera (G toggles walking)
  --time HH[:MM]          clock time at the start (06:00 sunrise, 12:00 noon, 00:00 midnight;
                          a day lasts 20 minutes); default 12:00, 21:00 with --lamps. Scripted
                          runs (--frames) keep the clock still
  --lamps                 demo: colored lamps and lava on the ground around the start
  --pack DIR              block textures from a Minecraft Java resource pack (the directory
                          with pack.mcmeta; default: procedural textures)
  --world-dir DIR         save the world in DIR on exit and load it from there on start (made
                          if missing; the camera and the clock start where they were unless
                          --camera or --time is given)
  --help                  this text
exit codes: 0 ok, 1 usage error, 2 no usable GPU, 3 check failed, 4 timeout";

/// The whole help: [`USAGE`] and the world options.
fn usage() -> String {
    format!(
        "{USAGE}\nfog options (F7 cycles the presets while the fog is on):\n{}\nLOD options (F8 \
         cycles the presets, F9 tints the levels):\n{}\nworld options:\n{}",
        fog::USAGE,
        lod::USAGE,
        world::usage(World::Forest)
    )
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
    /// `--time`, clock hours.
    time: Option<f64>,
    lamps: bool,
    /// `--pack`.
    textures: TextureSource,
    /// `--fog`.
    fog: fog::FogPreset,
    /// `--fog-res`.
    fog_res: boxel::render::fog::FogResolution,
    /// `--fog-steps`.
    fog_steps: u32,
    /// `--lod`.
    lod: lod::LodPreset,
    /// `--no-distance-fog` clears it: the fade at the view distance (F10 toggles it).
    distance_fog: bool,
    /// `--world-dir`: where the world is saved.
    world_dir: Option<PathBuf>,
    /// `--camera` was given (a saved camera does not replace it).
    camera_given: bool,
}

impl Options {
    /// Clock hours at the start: `--time`, else 21:00 with `--lamps` (they show at night), else
    /// noon.
    fn start_hours(&self) -> f64 {
        self.time
            .unwrap_or(if self.lamps { LAMPS_DEMO_HOURS } else { 12.0 })
    }

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
        seed: boxel_demo::worldgen::DEFAULT_SEED,
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
        time: None,
        lamps: false,
        textures: TextureSource::Procedural,
        fog: fog::FogPreset::Off,
        fog_res: boxel::render::fog::FogResolution::Half,
        fog_steps: boxel::fog::DEFAULT_MAX_STEPS,
        lod: lod::LodPreset::Off,
        distance_fog: true,
        world_dir: None,
        camera_given: false,
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
            "--pack" => o.textures = pack::parse(flag, &value()?)?,
            "--fog" => o.fog = fog::parse(flag, &value()?)?,
            "--fog-res" => o.fog_res = fog::parse_resolution(flag, &value()?)?,
            "--fog-steps" => o.fog_steps = fog::parse_steps(flag, &value()?)?,
            "--lod" => o.lod = lod::parse(flag, &value()?)?,
            "--no-distance-fog" => o.distance_fog = false,
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
            "--world-dir" => o.world_dir = Some(PathBuf::from(value()?)),
            "--camera" => {
                o.camera_given = true;
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
            "--time" => o.time = Some(clock::parse_clock(flag, &value()?)?),
            "--lamps" => o.lamps = true,
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
    mut lod: Option<(ResMut<LodSettings>, ResMut<LodCycle>)>,
) {
    for event in ready.read() {
        println!("boxel: {}", event.summary);
        println!("boxel: textures {}", pack::summary(&info.textures));
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
        // Whatever the radius options: far terrain is too slow on a CPU rasterizer.
        if info.class == Some(DeviceClass::Software)
            && let Some((settings, cycle)) = lod.as_mut()
        {
            settings.enabled = false;
            cycle.software = true;
            println!("boxel: software adapter: LOD off");
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
/// The blocks keys 1 to 9 and 0 select for placing (the first ten) and the mouse wheel scrolls
/// through (all of them; names missing from the registry are skipped).
const HOTBAR: [&str; 22] = [
    "boxel:stone",
    "boxel:dirt",
    "boxel:grass",
    "boxel:planks",
    "boxel:log",
    "boxel:leaves",
    "boxel:lamp_orange",
    "boxel:lamp_red",
    "boxel:lamp_blue",
    "boxel:lamp_white",
    "boxel:glass",
    "boxel:stone_slab",
    "boxel:short_grass",
    "boxel:poppy",
    "boxel:water",
    "boxel:lava",
    "boxel:stone_stairs",
    "boxel:oak_stairs",
    "boxel:oak_fence",
    "boxel:torch",
    "boxel:cobblestone_wall",
    "boxel:oak_fence_gate",
];
const HOTBAR_KEYS: [KeyCode; 10] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
    KeyCode::Digit7,
    KeyCode::Digit8,
    KeyCode::Digit9,
    KeyCode::Digit0,
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

/// The state that placing `block` on `hit` puts in, from its state properties (ADR 0018, 0019):
/// a slab (a `type` of `bottom | top | double`) goes in as a top slab when the click hits a
/// bottom face or the upper half of a side face, as a bottom slab otherwise; a block with an
/// `axis` (the logs) lies along the axis of the clicked face. Anything else is `block` itself.
fn placed_state(registry: &BlockRegistry, block: BlockState, hit: &VoxelHit) -> BlockState {
    let Some(face) = hit.face else {
        return block;
    };
    if is_slab(registry, block) {
        let top = match face {
            Face::NegY => true,
            Face::PosY => false,
            _ => hit.point.frac.y >= 0.5,
        };
        return registry
            .with(block, "type", if top { "top" } else { "bottom" })
            .unwrap_or(block);
    }
    registry
        .with(block, "axis", Axis::of_face(face).name())
        .unwrap_or(block)
}

/// The state placing `block` on `hit` puts in, looking along `look`, Minecraft's rules for the
/// blocks of ADR 0020 and [`placed_state`] for the others. Stairs face the way the player looks
/// (their high side away from the player) and go in upside down when the click hits a bottom
/// face or the upper half of a side face, like slabs; their shape comes from their neighbors
/// (the world's neighbor rules). A torch stands on a top face, hangs on a side face as a wall
/// torch facing away from it, and does not go under a block (`None`). A fence gate faces the way
/// the player looks (ADR 0027). Fences, walls and gates connect by themselves.
fn placement(
    registry: &BlockRegistry,
    block: BlockState,
    hit: &VoxelHit,
    look: DVec3,
) -> Option<BlockState> {
    let Some(face) = hit.face else {
        return Some(block);
    };
    let kinds = Kinds::of_registry(registry);
    let is = |kind| kinds.is(registry, block, kind);
    if is(Kind::Stairs) {
        let top = match face {
            Face::NegY => true,
            Face::PosY => false,
            _ => hit.point.frac.y >= 0.5,
        };
        let half = if top { SlabHalf::Top } else { SlabHalf::Bottom };
        boxel_demo::rules::stairs_with(registry, block, Facing::of_look(look.x, look.z), half)
    } else if is(Kind::Gate) {
        registry.with(block, "facing", Facing::of_look(look.x, look.z).name())
    } else if is(Kind::WallPostOverride) || is(Kind::WallTorch) {
        // `boxel:torch` and `boxel:wall_torch` (Minecraft's naming).
        let standing = registry.by_name(&registry.name(block).replace("wall_torch", "torch"))?;
        match face {
            Face::PosY => Some(standing),
            Face::NegY => None,
            side => {
                let wall =
                    registry.by_name(&registry.name(standing).replace("torch", "wall_torch"))?;
                registry.with(wall, "facing", Facing::of_face(side)?.name())
            }
        }
    } else {
        Some(placed_state(registry, block, hit))
    }
}

/// True for a slab: a `type` property with `bottom`, `top` and `double`.
fn is_slab(registry: &BlockRegistry, state: BlockState) -> bool {
    ["bottom", "top", "double"]
        .iter()
        .all(|v| registry.with(state, "type", v).is_some())
}

/// The double slab a right click with slab `held` makes on `hit`, as in Minecraft (ADR 0019):
/// clicking the inner face of a slab of the same block (the top of a bottom slab, the bottom of
/// a top slab) fills its cell, and so does placing the other half into a cell that holds a slab
/// of the same block (`at` reads the cell in front of the face). `None`: an ordinary placement.
fn slab_merge(
    registry: &BlockRegistry,
    held: BlockState,
    hit: &VoxelHit,
    at: impl FnOnce(BlockPos) -> Option<BlockState>,
) -> Option<BlockEdit> {
    let face = hit.face?;
    if !is_slab(registry, held) {
        return None;
    }
    let same = |s: BlockState| registry.block(s) == registry.block(held);
    // A double slab holds no water (ADR 0025).
    let double = |s: BlockState| {
        let double = registry.with(s, "type", "double")?;
        Some(registry.with_water(double, false).unwrap_or(double))
    };
    let half = |s: BlockState| registry.property(s, "type");
    let inner = matches!(
        (face, half(hit.state)),
        (Face::PosY, Some("bottom")) | (Face::NegY, Some("top"))
    );
    if inner && same(hit.state) {
        return Some(BlockEdit {
            pos: hit.block,
            state: double(hit.state)?,
        });
    }
    let pos = hit.adjacent()?;
    let existing = at(pos)?;
    let placed = placed_state(registry, held, hit);
    let other_half = matches!(
        (half(existing), half(placed)),
        (Some("bottom"), Some("top")) | (Some("top"), Some("bottom"))
    );
    (same(existing) && other_half).then_some(BlockEdit {
        pos,
        state: double(existing)?,
    })
}

/// Water placed on a dry block that can hold it fills that block (Minecraft's bucket on a
/// `LiquidBlockContainer`, ADR 0025): the edit that waterlogs the clicked block, or `None` (an
/// ordinary placement) when `held` is not the water source or the block cannot take it.
fn water_fill(registry: &BlockRegistry, held: BlockState, hit: &VoxelHit) -> Option<BlockEdit> {
    hit.face?;
    let source = registry.by_name("boxel:water")?;
    let fill = held == source
        && registry.can_be_waterlogged(hit.state)
        && !registry.is_waterlogged(hit.state);
    fill.then(|| BlockEdit {
        pos: hit.block,
        state: registry.with_water(hit.state, true).unwrap_or(hit.state),
    })
}

/// The water rules of a player's edit (ADR 0025): a block placed into a water source keeps it
/// ([`rules::placed_into`]), a broken waterlogged block leaves its water ([`rules::broken`]).
/// `existing` is what the cell holds now.
fn with_water_rules(registry: &BlockRegistry, edit: BlockEdit, existing: BlockState) -> BlockEdit {
    let state = if edit.state.is_air() {
        rules::broken(registry, existing)
    } else {
        rules::placed_into(registry, edit.state, existing)
    };
    BlockEdit { state, ..edit }
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

/// The F3 line of the first fluid along the view (water passes the picking ray).
fn fluid_line(fluid: Option<VoxelHit>, registry: &BlockRegistry) -> String {
    match fluid {
        Some(hit) => format!(
            "Fluid {} {} {} {}",
            registry.state_name(hit.state),
            hit.block.x,
            hit.block.y,
            hit.block.z
        ),
        None => "Fluid none".to_string(),
    }
}

/// The F3 lines of block picking.
fn picking_lines(picking: &Picking, registry: &BlockRegistry) -> [String; 2] {
    let target = match &picking.target {
        Some(hit) => format!(
            "Target {} {} {} {}  face {}",
            registry.state_name(hit.state),
            hit.block.x,
            hit.block.y,
            hit.block.z,
            hit.face.map_or("inside", face_name)
        ),
        None => format!("Target none (reach {REACH})"),
    };
    let place = match picking.hotbar.get(picking.selected) {
        Some((name, _)) => format!(
            "Place [{}] {name}  (keys 1-9, 0, wheel)",
            picking.selected + 1
        ),
        None => "Place nothing".to_string(),
    };
    [target, place]
}

/// Picks the block under the screen center (a voxel raycast from the active camera) and applies
/// this frame's click. Runs before the fly camera, so the cursor state is the one the click was
/// made with, and before the world's systems (`WorldSystems::Interest`, then `Edits`), so it
/// reads the map unambiguously and the edit shows this frame.
#[expect(
    clippy::too_many_arguments,
    reason = "a Bevy-style system: one parameter per resource it reads"
)]
fn pick_blocks(
    physics: PhysicsQuery,
    cameras: Query<(&Camera, &Transform)>,
    cursor: Res<CursorOptions>,
    buttons: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    scroll: Option<Res<AccumulatedMouseScroll>>,
    content: Res<WorldContent>,
    mut picking: ResMut<Picking>,
    mut edits: ResMut<EditQueue>,
    lines: Option<ResMut<OverlayLines>>,
    crosshair: Option<ResMut<CrosshairTarget>>,
    outline: Option<ResMut<BlockOutline>>,
    uses: Option<Res<BlockUses>>,
) {
    if let Some(slot) = HOTBAR_KEYS.iter().position(|&key| keys.just_pressed(key))
        && slot < picking.hotbar.len()
    {
        picking.selected = slot;
    }
    // The wheel: down selects the next block, up the previous one, wrapping around.
    let len = picking.hotbar.len();
    if let Some(scroll) = scroll
        && scroll.delta.y != 0.0
        && len > 0
    {
        let step = if scroll.delta.y < 0.0 { 1 } else { len - 1 };
        picking.selected = (picking.selected + step) % len;
    }
    let look = cameras
        .iter()
        .find(|(camera, _)| camera.active)
        .map_or(DVec3::NEG_Z, |(_, transform)| {
            transform.forward().as_dvec3()
        });
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
    // The renderer outlines the targeted block (ADR 0026).
    if let Some(mut outline) = outline {
        let block = target.map(|hit| hit.block);
        if outline.block != block {
            outline.block = block;
        }
    }
    // The first fluid along the view, for F3 (the picking ray goes through water).
    let fluid = cameras
        .iter()
        .find(|(camera, _)| camera.active)
        .and_then(|(_, transform)| {
            physics
                .raycast_voxels(
                    WorldPos::from_dvec3(transform.translation),
                    transform.forward().as_dvec3(),
                    REACH,
                    VoxelFilter::Outline { fluids: true },
                )
                .hit()
                .copied()
        })
        .filter(|hit| physics.collision().is_fluid(hit.state));
    // The F3 light probe: the cell in front of the picked face (its light shades that face).
    if let Some(mut crosshair) = crosshair {
        let cell = target.map(|hit| hit.adjacent().unwrap_or(hit.block));
        if crosshair.0 != cell {
            crosshair.0 = cell;
        }
    }
    let click = Click::from_input(&buttons, cursor.grab == CursorGrab::Locked);
    if let (Some(hit), Some(click)) = (target, click) {
        // The placement check: never a block inside the player (or any solid body), against the
        // shape the block will have.
        let registry = &content.registry;
        // Air: this block cannot go on that face (a torch under a block).
        let block = placement(registry, picking.block(), &hit, look).unwrap_or(BlockState::AIR);
        let free = |pos: BlockPos| match physics.probe(pos) {
            VoxelProbe::Block(state) => {
                (state.is_air() || physics.collision().is_fluid(state))
                    && physics.state_is_free(pos, block)
            }
            VoxelProbe::Unloaded => false,
        };
        // A slab on a slab of the same block: the double slab, checked as the full cube.
        let probe = |pos: BlockPos| match physics.probe(pos) {
            VoxelProbe::Block(state) => Some(state),
            VoxelProbe::Unloaded => None,
        };
        let merge = match click {
            Click::Place => slab_merge(registry, picking.block(), &hit, probe)
                .or_else(|| water_fill(registry, picking.block(), &hit)),
            Click::Break => None,
        };
        // A block that is used takes the right click (a gate opens or closes), unless the player
        // sneaks: then it places, as in Minecraft (ADR 0027).
        let sneaking = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
        let used = match (click, uses.as_deref()) {
            (Click::Place, Some(uses)) if !sneaking => uses
                .use_block(registry, hit.state, Facing::of_look(look.x, look.z))
                .map(|state| BlockEdit {
                    pos: hit.block,
                    state,
                }),
            _ => None,
        };
        let edit = match (used, merge) {
            (Some(edit), _) => Some(edit),
            (None, Some(edit)) => physics.state_is_free(edit.pos, edit.state).then_some(edit),
            (None, None) => click_edit(click, &hit, block, free)
                .map(|edit| with_water_rules(registry, edit, probe(edit.pos).unwrap_or_default())),
        };
        if let Some(edit) = edit {
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
        let fluid = fluid_line(fluid, &content.registry);
        if lines.get("pick fluid") != Some(fluid.as_str()) {
            lines.set("pick fluid", fluid);
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
        let registry = Arc::clone(&app.world().resource::<WorldContent>().registry);
        app.insert_resource(Picking::new(&registry));
    }
}

// ---------------------------------------------------------------------------------------------
// Day and night, and the lamps demo

/// `F`: how many times faster than real time the clock runs while toggled.
const FAST_TIME: u64 = 60;
/// `--lamps` starts at this clock hour unless `--time` says otherwise.
const LAMPS_DEMO_HOURS: f64 = 21.0;
/// `--lamps`: the blocks placed around the start, one every 45 degrees.
const LAMPS_DEMO_BLOCKS: [&str; 8] = [
    "boxel:lamp_orange",
    "boxel:lamp_red",
    "boxel:lamp_blue",
    "boxel:lamp_white",
    "boxel:lava",
    "boxel:lamp_orange",
    "boxel:lamp_blue",
    "boxel:lava",
];
/// `--lamps`: distance from the start, in blocks.
const LAMPS_DEMO_RADIUS: f64 = 9.0;
/// `--lamps`: how far below the start the ground is searched.
const LAMPS_DEMO_DEPTH: i64 = 256;

/// Clock keys: `[` and `]` move the time one hour back and forward, T pauses it, F toggles
/// [`FAST_TIME`] (G is the walk toggle). Each change is logged.
fn time_keys(keys: Res<ButtonInput<KeyCode>>, mut time: ResMut<TimeOfDay>) {
    let hour = i64::try_from(time.ticks_per_hour()).unwrap_or(i64::MAX);
    let mut changed = true;
    if keys.just_pressed(KeyCode::BracketLeft) {
        time.scrub(-hour);
    } else if keys.just_pressed(KeyCode::BracketRight) {
        time.scrub(hour);
    } else if keys.just_pressed(KeyCode::KeyT) {
        time.paused = !time.paused;
    } else if keys.just_pressed(KeyCode::KeyF) {
        time.speed = if time.speed == 1 { FAST_TIME } else { 1 };
    } else {
        changed = false;
    }
    if changed {
        println!(
            "boxel: time {} day {}{}{}",
            time.clock_text(),
            time.day(),
            if time.paused { ", paused" } else { "" },
            if time.speed == 1 {
                String::new()
            } else {
                format!(", x{}", time.speed)
            }
        );
    }
}

/// `--lamps`: the start position, until the blocks are placed.
#[derive(Resource, Debug)]
struct LampsDemo {
    /// `None` once placed (or without `--lamps`).
    start: Option<DVec3>,
}

/// The cells of the `--lamps` blocks around `start`, with their names: on the first non-air
/// block under the start height of each column. `None` while a column is not loaded down to
/// its ground (try again next frame); columns without ground within [`LAMPS_DEMO_DEPTH`] are
/// skipped.
fn demo_lamp_cells(
    start: DVec3,
    block_at: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<Vec<(BlockPos, &'static str)>> {
    let mut cells = Vec::new();
    for (i, &name) in LAMPS_DEMO_BLOCKS.iter().enumerate() {
        let angle = i as f64 * std::f64::consts::TAU / LAMPS_DEMO_BLOCKS.len() as f64;
        let x = (start.x + LAMPS_DEMO_RADIUS * angle.cos()).floor() as i64;
        let z = (start.z + LAMPS_DEMO_RADIUS * angle.sin()).floor() as i64;
        let top = start.y.floor() as i64;
        for y in (top.saturating_sub(LAMPS_DEMO_DEPTH)..=top).rev() {
            let state = block_at(BlockPos::new(x, y, z))?;
            if !state.is_air() {
                cells.push((BlockPos::new(x, y + 1, z), name));
                break;
            }
        }
    }
    Some(cells)
}

/// `--lamps`: places the demo blocks once their columns are loaded.
fn place_demo_lamps(
    mut demo: ResMut<LampsDemo>,
    map: Res<ChunkMap>,
    content: Res<WorldContent>,
    mut edits: ResMut<EditQueue>,
) {
    let Some(start) = demo.start else {
        return;
    };
    let Some(cells) = demo_lamp_cells(start, |p| map.block_at(p)) else {
        return;
    };
    let mut placed = 0;
    for (pos, name) in cells {
        if let Some(state) = content.registry.by_name(name) {
            edits.set_block(pos, state);
            placed += 1;
        }
    }
    println!("boxel: --lamps placed {placed} blocks around the start");
    demo.start = None;
}

/// The clock keys and the `--lamps` demo: inserts the starting [`TimeOfDay`] (over the running
/// default of `DaylightPlugin`, which must be added first) and the [`LampsDemo`], and adds
/// [`time_keys`] and [`place_demo_lamps`] to `Update` (the lamps after [`pick_blocks`]: both
/// write the edit queue, and before the world's systems, so they show the same frame).
struct DayNightPlugin {
    time: TimeOfDay,
    /// `--lamps`: where the demo blocks go around.
    lamps: Option<DVec3>,
}

impl Plugin for DayNightPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.time)
            .insert_resource(LampsDemo { start: self.lamps })
            .add_systems(
                Update,
                (
                    time_keys,
                    place_demo_lamps
                        .after(pick_blocks)
                        .before(WorldSystems::Interest),
                ),
            );
    }
}

/// Metadata of a saved world: the world options and seed it was made with.
const WORLD_META: &str = "voxel_world:world";
/// Metadata of a saved world: the camera on exit, as `x,y,z,yaw,pitch` (degrees).
const CAMERA_META: &str = "voxel_world:camera";

/// The world options and seed, as saved with a world (`forest seed 42`).
fn world_signature(o: &Options) -> String {
    format!("{} seed {}", o.world.label(), o.seed)
}

/// `x,y,z,yaw,pitch` with finite numbers inside the world.
fn parse_camera(text: &str) -> Option<[f64; 5]> {
    let parts: Vec<f64> = text
        .split(',')
        .map(|p| p.trim().parse::<f64>().ok())
        .collect::<Option<_>>()?;
    let camera: [f64; 5] = parts.try_into().ok()?;
    let inside = camera.iter().all(|c| c.is_finite())
        && camera[..3]
            .iter()
            .all(|c| c.abs() <= MAX_BLOCK_COORD as f64);
    inside.then_some(camera)
}

/// `--world-dir`: opens the world in `dir` (made if missing). A world made with other world
/// options or another seed is refused (its saved chunks would not match the generated ones);
/// the camera starts where it was saved unless `--camera` was given.
fn open_world(
    dir: &Path,
    registry: &BlockRegistry,
    generator: &str,
    options: &mut Options,
) -> Result<WorldStore, String> {
    let fail = |e: &dyn std::fmt::Display| format!("--world-dir {}: {e}", dir.display());
    let store = WorldStore::open_dir(dir, registry, generator).map_err(|e| fail(&e))?;
    let signature = world_signature(options);
    match store.meta(WORLD_META).map_err(|e| fail(&e))? {
        Some(saved) if saved != signature.as_bytes() => {
            return Err(fail(&format!(
                "the world was made as {:?} and this run asks for {signature:?}: pass the same \
                 --world, --caves, --no-trees, --no-sea and --seed",
                String::from_utf8_lossy(&saved)
            )));
        }
        Some(_) => {}
        None => store
            .put_meta(WORLD_META, signature.into_bytes())
            .map_err(|e| fail(&e))?,
    }
    let saved_camera = store.meta(CAMERA_META).map_err(|e| fail(&e))?;
    if !options.camera_given
        && let Some(camera) = saved_camera.and_then(|b| parse_camera(&String::from_utf8_lossy(&b)))
    {
        options.camera = camera;
    }
    Ok(store)
}

/// `--world-dir`, on exit: the camera, saved with the world.
fn save_camera(store: Res<WorldStore>, cameras: Query<(&Transform, &FlyCamera)>) {
    let Some((transform, fly)) = cameras.iter().next() else {
        return;
    };
    let p = transform.translation;
    let text = format!(
        "{},{},{},{},{}",
        p.x,
        p.y,
        p.z,
        fly.yaw.to_degrees(),
        fly.pitch.to_degrees()
    );
    if let Err(e) = store.put_meta(CAMERA_META, text.into_bytes()) {
        eprintln!("boxel: the camera could not be saved: {e}");
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
    let mut options = match parse_options(&rest) {
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
    let registry = Arc::new(boxel_demo::blocks::registry());
    let store = match options.world_dir.clone() {
        Some(dir) => match open_world(&dir, &registry, generator.name(), &mut options) {
            Ok(store) => Some(store),
            Err(message) => {
                eprintln!("boxel: {message}");
                return AppExit::from_code(1);
            }
        },
        None => None,
    };
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
                textures: options.textures.clone(),
                distance_fog: if options.distance_fog {
                    DistanceFog::default()
                } else {
                    DistanceFog::off()
                },
                ..RenderSettings::default()
            },
        })
        .set(WorldPlugin {
            generator,
            registry,
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
    // The world clock (DaylightPlugin is in DefaultPlugins): scripted runs keep it still.
    let day_night = DayNightPlugin {
        time: clock::start_time(options.start_hours(), options.automation()),
        lamps: options
            .lamps
            .then(|| DVec3::new(options.camera[0], options.camera[1], options.camera[2])),
    };
    let mut app = App::new();
    // Flowing water (after `WorldPlugin`, in `DefaultPlugins`).
    app.add_plugins(plugins)
        .add_plugins((day_night, FluidPlugin));
    // `--fog`: the plugin only when asked (off adds nothing at all); F7 cycles the presets.
    if let Some(settings) = options.fog.settings(options.fog_res, options.fog_steps) {
        app.add_plugins(FogPlugin { settings })
            .insert_resource(FogCycle(options.fog))
            .add_systems(Update, cycle_fog);
    }
    // `--lod`: the plugin only when asked (off adds nothing at all); F8 cycles the presets, F9
    // tints the levels, F3 shows a line.
    if let Some(settings) = options.lod.settings() {
        app.add_plugins(LodPlugin::new(settings))
            .insert_resource(LodCycle::new(options.lod))
            .add_systems(Update, (cycle_lod, lod_overlay_line));
    }
    if let Some(store) = store {
        app.add_plugins(PersistPlugin::new(store))
            .add_systems(Shutdown, save_camera);
        // `--time` sets the clock: the saved one neither replaces it nor is replaced by it.
        if options.time.is_some() {
            app.insert_resource(PersistSettings {
                clock: false,
                ..PersistSettings::default()
            });
        }
    }
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
        SwimPlugin,
        PickingPlugin,
        WalkPlugin { start_walking },
        crates::CratesPlugin,
    ));
    // The camera moves first; the world then computes interest from the new position.
    app.configure_sets(Update, FlyCameraSystems.before(WorldSystems::Interest))
        .insert_resource(Run::new(options))
        .init_resource::<FixedSteps>()
        .add_systems(Startup, spawn_camera)
        .add_systems(FixedUpdate, count_fixed_steps)
        .add_systems(Update, toggle_distance_fog)
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
    fn lod_options_and_the_f8_cycle() {
        assert_eq!(parse(&[]).lod, lod::LodPreset::Off, "no LOD unless asked");
        assert_eq!(parse(&["--lod", "far"]).lod, lod::LodPreset::Far);
        assert!(parse_options(&["--lod".into(), "huge".into()]).is_err());
        assert!(usage().contains("F8 cycles the presets"));
        // F8 from far: extreme, hidden, near, far again; from near: far.
        let mut cycle = LodCycle::new(lod::LodPreset::Far);
        let mut seen = Vec::new();
        for _ in 0..4 {
            (cycle.preset, cycle.hidden) = cycle.next();
            seen.push(cycle.name());
        }
        assert_eq!(
            seen,
            [
                lod::label(lod::LodPreset::Extreme),
                "hidden".to_string(),
                lod::label(lod::LodPreset::Near),
                lod::label(lod::LodPreset::Far),
            ]
        );
        assert_eq!(
            LodCycle::new(lod::LodPreset::Near).next(),
            (lod::LodPreset::Far, false)
        );
    }

    #[test]
    fn distance_fog_option_and_the_f10_toggle() {
        assert!(parse(&[]).distance_fog, "on unless asked");
        assert!(!parse(&["--no-distance-fog"]).distance_fog);
        assert!(usage().contains("--no-distance-fog"));
        assert!(usage().contains("F10 toggles"));
        let mut app = App::new();
        app.insert_resource(RenderSettings::default())
            .init_resource::<ButtonInput<KeyCode>>()
            .add_systems(Update, toggle_distance_fog);
        let press = |app: &mut App| {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.release(KeyCode::F10);
            keys.clear_just();
            keys.press(KeyCode::F10);
            app.update();
            app.world().resource::<RenderSettings>().distance_fog
        };
        assert_eq!(press(&mut app), DistanceFog::off());
        assert_eq!(press(&mut app), DistanceFog::default());
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
    fn world_dir_option_and_saved_values() {
        let o = parse(&["--world-dir", "saves/one", "--seed", "7"]);
        assert_eq!(o.world_dir, Some(PathBuf::from("saves/one")));
        assert!(!o.camera_given);
        assert_eq!(world_signature(&o), "forest seed 7");
        let o = parse(&[
            "--world",
            "overworld",
            "--caves",
            "off",
            "--camera",
            "1,2,3,4,5",
        ]);
        assert!(o.camera_given);
        assert_eq!(
            world_signature(&o),
            format!("overworld/off seed {}", o.seed)
        );
        assert_eq!(
            parse_camera("1.5,-2,3e9,90,-10"),
            Some([1.5, -2.0, 3e9, 90.0, -10.0])
        );
        for bad in [
            "",
            "1,2,3,4",
            "1,2,3,4,5,6",
            "a,2,3,4,5",
            "inf,0,0,0,0",
            "0,0,1e300,0,0",
        ] {
            assert_eq!(parse_camera(bad), None, "{bad}");
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

    /// A headless app with the world (stone y 0..=2, grass at y = 3, inline jobs), its clock
    /// (running from noon) with the clock keys, physics, the fly camera, picking and walking,
    /// settled; the cursor is free and a fly camera at (0.5, 6, 0.5) looks straight down at the
    /// grass (top face at y = 4). `Update` is strict about ambiguities, so picking must be
    /// ordered against the fly camera's grab (both use `CursorOptions`), and walking against the
    /// camera's moves. Every update is one fixed step (a synthetic clock).
    fn picking_app() -> App {
        picking_app_with(None)
    }

    /// [`picking_app`], with the `--lamps` demo around `lamps`.
    fn picking_app_with(lamps: Option<DVec3>) -> App {
        use boxel::ecs::schedule::{LogLevel, ScheduleBuildSettings};
        use boxel::world::jobs::InlineExecutor;
        use boxel::worldgen::FlatGenerator;
        let mut app = App::new();
        app.add_plugins((
            TimePlugin,
            InputPlugin,
            FlyCameraPlugin,
            WorldPlugin {
                generator: Arc::new(FlatGenerator::new(vec![
                    ("boxel:stone".to_string(), 3),
                    ("boxel:grass".to_string(), 1),
                ])),
                executor: Arc::new(InlineExecutor),
                registry: Arc::new(boxel_demo::blocks::registry()),
                ..WorldPlugin::default()
            },
            FluidPlugin,
            DaylightPlugin,
            DayNightPlugin {
                time: clock::start_time(12.0, false),
                lamps,
            },
            PhysicsPlugin::default(),
            SwimPlugin,
            PickingPlugin,
            WalkPlugin::default(),
            // Its systems pass the strict ambiguity check too.
            crates::CratesPlugin,
        ))
        .init_resource::<OverlayLines>()
        // As `DebugOverlayPlugin` does: picking fills it.
        .init_resource::<CrosshairTarget>()
        // As `RenderPlugin` does: picking sets its block.
        .init_resource::<BlockOutline>()
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
        let grass = boxel_demo::blocks::registry()
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
        // The F3 light probe: the air cell over the grass, whose light shades its top face.
        let crosshair = |app: &App| app.world().resource::<CrosshairTarget>().0;
        assert_eq!(crosshair(&app), Some(BlockPos::new(0, 4, 0)));
        let lines = app.world().resource::<OverlayLines>();
        assert_eq!(
            lines.get("pick target"),
            Some("Target boxel:grass 0 3 0  face +Y")
        );
        assert_eq!(
            lines.get("pick place"),
            Some("Place [1] boxel:stone  (keys 1-9, 0, wheel)")
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
        assert_eq!(crosshair(&app), Some(BlockPos::new(0, 3, 0)));

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
            Some("Place [3] boxel:grass  (keys 1-9, 0, wheel)")
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

    /// Right click on a fence gate opens it and closes it instead of placing a block; with Shift
    /// held it places, as in Minecraft (ADR 0027).
    #[test]
    fn right_click_uses_a_gate_and_shift_places_on_it() {
        let mut app = picking_app();
        let registry = boxel_demo::blocks::registry();
        let gate = BlockPos::new(0, 4, 0);
        app.world_mut().resource_mut::<EditQueue>().set_block(
            gate,
            registry.by_name("boxel:oak_fence_gate").expect("gate"),
        );
        app.update();
        app.update();
        // The first click only grabs the cursor.
        click(&mut app, MouseButton::Left);
        assert_eq!(
            app.world().resource::<Picking>().target.map(|t| t.block),
            Some(gate)
        );
        let open = |app: &App| {
            let state = block_at(app, gate).expect("loaded");
            assert_eq!(registry.name(state), "boxel:oak_fence_gate");
            registry.property(state, "open") == Some("true")
        };
        assert!(!open(&app));
        click(&mut app, MouseButton::Right);
        assert!(open(&app), "opened, nothing placed");
        assert_eq!(
            block_at(&app, BlockPos::new(0, 5, 0)),
            Some(BlockState::AIR)
        );
        click(&mut app, MouseButton::Right);
        assert!(!open(&app), "closed again");
        // Sneaking: the selected block (stone) goes on top of the gate, which stays closed. The
        // fly camera would go down with Shift: hold it still.
        let mut cameras = app.world_mut().query::<&mut FlyCamera>();
        for mut camera in cameras.iter_mut(app.world_mut()) {
            camera.speed = 0.0;
        }
        key(&mut app, KeyCode::ShiftLeft, true);
        app.update();
        click(&mut app, MouseButton::Right);
        key(&mut app, KeyCode::ShiftLeft, false);
        app.update();
        assert!(!open(&app));
        assert_eq!(
            block_at(&app, BlockPos::new(0, 5, 0)),
            registry.by_name("boxel:stone")
        );
    }

    #[test]
    fn placed_water_flows_and_f3_shows_its_level() {
        let mut app = picking_app();
        let registry = boxel_demo::blocks::registry();
        let water = |level: u8| {
            registry
                .parse_state(&format!("boxel:water[level={level}]"))
                .expect("built-in")
        };
        let slot = HOTBAR
            .iter()
            .position(|&n| n == "boxel:water")
            .expect("water in the hotbar");
        app.world_mut().resource_mut::<Picking>().selected = slot;
        // The first click grabs the cursor, the second places a source on the grass.
        click(&mut app, MouseButton::Left);
        click(&mut app, MouseButton::Right);
        assert_eq!(block_at(&app, BlockPos::new(0, 4, 0)), Some(water(0)));
        // Every update is one fixed step: 20 game ticks are 64 updates, a flow step 16.
        for _ in 0..200 {
            app.update();
        }
        for d in 1..=7 {
            assert_eq!(
                block_at(&app, BlockPos::new(d, 4, 0)),
                Some(water(d as u8)),
                "{d} blocks away"
            );
        }
        assert_eq!(
            block_at(&app, BlockPos::new(8, 4, 0)),
            Some(BlockState::AIR)
        );
        let lines = app.world().resource::<OverlayLines>();
        assert_eq!(
            lines.get("pick fluid"),
            Some("Fluid boxel:water[level=0] 0 4 0")
        );
        // Picking goes through the water to the grass.
        assert_eq!(
            app.world().resource::<Picking>().target.map(|t| t.block),
            Some(BlockPos::new(0, 3, 0))
        );
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
    fn the_targeted_block_is_outlined() {
        let mut app = picking_app();
        let outline = |app: &App| app.world().resource::<BlockOutline>().block;
        assert_eq!(outline(&app), Some(BlockPos::new(0, 3, 0)));
        // Out of reach of everything: no outline.
        let mut cameras = app
            .world_mut()
            .query_filtered::<&mut Transform, With<FlyCamera>>();
        cameras
            .single_mut(app.world_mut())
            .expect("camera")
            .translation
            .y = 40.0;
        app.update();
        assert_eq!(outline(&app), None);
    }

    /// Walking on the flat grass (top at y = 4) under the camera, landed and asleep.
    fn walking_app() -> (App, Entity) {
        let mut app = picking_app();
        tap(&mut app, player::WALK_KEY);
        for _ in 0..64 {
            app.update();
        }
        let body = walker(&mut app).expect("walking");
        (app, body)
    }

    fn move_line(app: &App) -> String {
        app.world()
            .resource::<OverlayLines>()
            .get("move")
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn ctrl_sprints_shift_sneaks_and_f3_shows_it() {
        use boxel::physics::BodyPosition;
        use player::{SNEAK_KEY, SPRINT_KEY};
        let (mut app, body) = walking_app();
        // W with Ctrl: Minecraft's sprint, 1.3 times the walk.
        key(&mut app, KeyCode::KeyW, true);
        key(&mut app, SPRINT_KEY, true);
        for _ in 0..64 {
            app.update();
        }
        assert_eq!(
            move_line(&app),
            "Walk (G flies)  on ground  speed 5.61 b/s  sprinting"
        );
        key(&mut app, SPRINT_KEY, false);
        // With Shift: a sneak, 0.3 times the walk (it wins over a sprint).
        key(&mut app, SNEAK_KEY, true);
        for _ in 0..64 {
            app.update();
        }
        assert_eq!(
            move_line(&app),
            "Walk (G flies)  on ground  speed 1.30 b/s  sneaking"
        );
        key(&mut app, KeyCode::KeyW, false);
        key(&mut app, SNEAK_KEY, false);
        for _ in 0..64 {
            app.update();
        }
        // A pit three blocks deep just north of the player: sneaking north, it stops at the
        // edge and never falls in.
        let feet = |app: &App| app.world().get::<BodyPosition>(body).expect("body").0;
        let edge = feet(&app).block.z - 1;
        {
            let mut edits = app.world_mut().resource_mut::<EditQueue>();
            for x in -3..=3 {
                for z in edge - 4..=edge {
                    for y in 1..=3 {
                        edits.set_block(BlockPos::new(x, y, z), BlockState::AIR);
                    }
                }
            }
        }
        app.update();
        key(&mut app, KeyCode::KeyW, true);
        key(&mut app, SNEAK_KEY, true);
        for _ in 0..192 {
            app.update();
            let p = feet(&app).to_dvec3();
            assert_eq!(p.y, 4.0, "never off the edge");
        }
        let p = feet(&app).to_dvec3();
        // Its back side keeps a sliver of the last row (z = edge + 1 .. edge + 2).
        let back = p.z + 0.3 - (edge + 1) as f64;
        assert!(back > 0.0 && back < 0.01, "{p} at the edge of {edge}");
        // Without the sneak it walks in.
        key(&mut app, SNEAK_KEY, false);
        for _ in 0..64 {
            app.update();
        }
        assert_eq!(feet(&app).to_dvec3().y, 1.0);
    }

    #[test]
    fn space_twice_flies_shift_descends_and_landing_walks() {
        use boxel::physics::BodyPosition;
        use player::SNEAK_KEY;
        let (mut app, body) = walking_app();
        let flying = |app: &App| {
            app.world()
                .get::<CharacterController>(body)
                .expect("body")
                .flying
        };
        let feet_y = |app: &App| {
            app.world()
                .get::<BodyPosition>(body)
                .expect("body")
                .0
                .to_dvec3()
                .y
        };
        // Two taps of Space: the first jumps, the second turns the flight on.
        tap(&mut app, KeyCode::Space);
        assert!(!flying(&app));
        tap(&mut app, KeyCode::Space);
        assert!(flying(&app));
        assert!(move_line(&app).starts_with("Walk (G flies)  flying"));
        // Held Space rises; let go, it hovers (no gravity).
        key(&mut app, KeyCode::Space, true);
        for _ in 0..32 {
            app.update();
        }
        key(&mut app, KeyCode::Space, false);
        for _ in 0..128 {
            app.update();
        }
        let high = feet_y(&app);
        assert!(high > 6.0, "{high}");
        for _ in 0..16 {
            app.update();
        }
        assert_eq!(feet_y(&app), high);
        assert!(flying(&app));
        // Shift descends until it lands, which ends the flight.
        key(&mut app, SNEAK_KEY, true);
        for _ in 0..256 {
            app.update();
            if !flying(&app) {
                break;
            }
        }
        key(&mut app, SNEAK_KEY, false);
        assert!(!flying(&app));
        assert_eq!(feet_y(&app), 4.0);
        // Up again, and two taps in the air turn it off: it falls back to the grass.
        tap(&mut app, KeyCode::Space);
        tap(&mut app, KeyCode::Space);
        assert!(flying(&app));
        key(&mut app, KeyCode::Space, true);
        for _ in 0..32 {
            app.update();
        }
        key(&mut app, KeyCode::Space, false);
        tap(&mut app, KeyCode::Space);
        tap(&mut app, KeyCode::Space);
        assert!(!flying(&app));
        for _ in 0..128 {
            app.update();
        }
        assert_eq!(feet_y(&app), 4.0);
        assert_eq!(move_line(&app), "Walk (G flies)  on ground  speed 0.00 b/s");
    }

    /// Where the player's eye is (its interpolated feet plus the standing eye height), with no
    /// bob, crouch or step lag.
    fn standing_eye(app: &mut App) -> DVec3 {
        let mut players = app
            .world_mut()
            .query_filtered::<&Transform, With<player::Player>>();
        let feet = players.single(app.world()).expect("walking").translation;
        feet + DVec3::new(0.0, player::EYE_HEIGHT, 0.0)
    }

    fn fov(app: &mut App) -> f32 {
        let mut cameras = app.world_mut().query_filtered::<&Camera, With<FlyCamera>>();
        cameras.single(app.world()).expect("camera").fov_y_radians
    }

    #[test]
    fn shift_lowers_the_eye_smoothly_and_a_sprint_widens_the_view() {
        use boxel::physics::Crouch;
        use player::{CROUCH_EYE_HEIGHT, EYE_HEIGHT, SNEAK_KEY, SPRINT_FOV, SPRINT_KEY};
        let (mut app, body) = walking_app();
        app.world_mut().resource_mut::<player::Walking>().view_bob = false;
        // Shift: the body crouches and the eye eases down to Minecraft's 1.27, not at once.
        key(&mut app, SNEAK_KEY, true);
        let mut eyes = Vec::new();
        for _ in 0..64 {
            app.update();
            eyes.push(camera_at(&mut app).y);
        }
        assert!(
            app.world()
                .get::<Crouch>(body)
                .expect("body")
                .is_crouching()
        );
        assert!(eyes.windows(2).all(|w| w[1] <= w[0]), "{eyes:?}");
        assert!(eyes[0] > 4.0 + CROUCH_EYE_HEIGHT + 0.1, "{eyes:?}");
        assert_eq!(eyes[63], 4.0 + CROUCH_EYE_HEIGHT);
        assert!(move_line(&app).ends_with("sneaking"));
        // Let go: up again, back exactly at the standing eye.
        key(&mut app, SNEAK_KEY, false);
        for _ in 0..64 {
            app.update();
        }
        assert!(
            !app.world()
                .get::<Crouch>(body)
                .expect("body")
                .is_crouching()
        );
        assert_eq!(camera_at(&mut app).y, 4.0 + EYE_HEIGHT);
        // W with Ctrl: the view widens by a tenth, smoothly; the sprint goes on without Ctrl
        // and ends with W, and the view comes back exactly.
        let base = fov(&mut app);
        let wide = (f64::from(base) * SPRINT_FOV) as f32;
        key(&mut app, KeyCode::KeyW, true);
        key(&mut app, SPRINT_KEY, true);
        app.update();
        app.update();
        let first = fov(&mut app);
        assert!(first > base && first < wide, "{first}");
        key(&mut app, SPRINT_KEY, false);
        for _ in 0..64 {
            app.update();
        }
        assert_eq!(fov(&mut app), wide);
        assert!(move_line(&app).ends_with("sprinting"));
        key(&mut app, KeyCode::KeyW, false);
        for _ in 0..64 {
            app.update();
        }
        assert_eq!(fov(&mut app), base);
        // Sprinting and then flying the free camera (G): its own field of view comes back.
        key(&mut app, KeyCode::KeyW, true);
        key(&mut app, SPRINT_KEY, true);
        for _ in 0..8 {
            app.update();
        }
        assert!(fov(&mut app) > base);
        tap(&mut app, player::WALK_KEY);
        assert_eq!(fov(&mut app), base);
        key(&mut app, KeyCode::KeyW, false);
        key(&mut app, SPRINT_KEY, false);
    }

    /// The camera's rotation, and its mouse look's alone (yaw then pitch, no bob).
    fn turns(app: &mut App) -> (Quat, Quat) {
        let mut cameras = app.world_mut().query::<(&Transform, &FlyCamera)>();
        let (transform, fly) = cameras.single(app.world()).expect("camera");
        let look = Quat::from_rotation_y(fly.yaw) * Quat::from_rotation_x(fly.pitch);
        (transform.rotation, look)
    }

    #[test]
    fn walking_bobs_the_view_and_v_turns_it_off() {
        use player::{BOB_KEY, WALK_KEY};
        let (mut app, _) = walking_app();
        let bobbed = |app: &mut App| (camera_at(app) - standing_eye(app)).length();
        // Walking on the grass: the eye sways, at most Minecraft's tenth of a block, and the
        // view tilts with it.
        key(&mut app, KeyCode::KeyW, true);
        let mut most: f64 = 0.0;
        let mut tilted = 0;
        for _ in 0..96 {
            app.update();
            most = most.max(bobbed(&mut app));
            let (rotation, look) = turns(&mut app);
            tilted += usize::from(rotation != look);
        }
        assert!(most > 0.05 && most < 0.12, "{most}");
        assert!(tilted > 48, "{tilted} of 96 frames tilted");
        // V: no bob, the eye stays on the body while it walks, and the view is the mouse look's
        // again at once (no tilt left over).
        tap(&mut app, BOB_KEY);
        for _ in 0..32 {
            app.update();
            assert_eq!(bobbed(&mut app), 0.0);
            let (rotation, look) = turns(&mut app);
            assert_eq!(rotation, look);
        }
        tap(&mut app, BOB_KEY);
        key(&mut app, KeyCode::KeyW, false);
        // Flying (Space twice) it does not bob either, once the walk's bob has faded.
        tap(&mut app, KeyCode::Space);
        tap(&mut app, KeyCode::Space);
        key(&mut app, KeyCode::Space, true);
        for _ in 0..16 {
            app.update();
        }
        key(&mut app, KeyCode::Space, false);
        key(&mut app, KeyCode::KeyW, true);
        for _ in 0..64 {
            app.update();
        }
        for _ in 0..32 {
            app.update();
            assert_eq!(bobbed(&mut app), 0.0);
        }
        key(&mut app, KeyCode::KeyW, false);
        // G while walking: the player goes and the camera keeps no tilt of its last bob.
        let (mut app, _) = walking_app();
        key(&mut app, KeyCode::KeyW, true);
        for _ in 0..32 {
            app.update();
        }
        let (rotation, look) = turns(&mut app);
        assert_ne!(rotation, look, "tilted while walking");
        tap(&mut app, WALK_KEY);
        assert!(walker(&mut app).is_none());
        let (rotation, look) = turns(&mut app);
        assert_eq!(rotation, look);
        key(&mut app, KeyCode::KeyW, false);
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
        let stone = boxel_demo::blocks::registry()
            .by_name("boxel:stone")
            .expect("built-in");
        assert_eq!(block_at(&app, BlockPos::new(0, 4, 0)), Some(stone));
    }

    #[test]
    fn placed_slabs_take_the_clicked_half_and_logs_the_clicked_axis() {
        let registry = boxel_demo::blocks::registry();
        let by = |n: &str| registry.parse_state(n).expect("built-in");
        let hit = |face, y: f64| VoxelHit {
            block: BlockPos::new(0, 0, 0),
            face: Some(face),
            t: 1.0,
            point: WorldPos::from_dvec3(DVec3::new(0.5, y, 0.0)),
            state: by("boxel:stone"),
        };
        let slab = by("boxel:stone_slab");
        let slab_top = by("boxel:stone_slab[type=top]");
        let double = by("boxel:stone_slab[type=double]");
        for (face, y, want) in [
            (Face::PosY, 1.0, slab),
            (Face::NegY, 0.0, slab_top),
            (Face::PosX, 0.25, slab),
            (Face::NegZ, 0.75, slab_top),
            (Face::PosZ, 0.5, slab_top),
        ] {
            for held in [slab, slab_top, double] {
                assert_eq!(
                    placed_state(&registry, held, &hit(face, y)),
                    want,
                    "{face:?}"
                );
            }
        }
        let log = by("boxel:log");
        for (face, want) in [
            (Face::PosY, log),
            (Face::NegX, by("boxel:log[axis=x]")),
            (Face::PosZ, by("boxel:log[axis=z]")),
        ] {
            for held in [log, want] {
                assert_eq!(
                    placed_state(&registry, held, &hit(face, 0.5)),
                    want,
                    "{face:?}"
                );
            }
        }
        // Blocks without those properties stay as they are.
        let stone = by("boxel:stone");
        assert_eq!(placed_state(&registry, stone, &hit(Face::PosX, 0.5)), stone);
        let grass = by("boxel:short_grass");
        assert_eq!(placed_state(&registry, grass, &hit(Face::NegY, 0.0)), grass);
    }

    #[test]
    fn stairs_and_torches_are_placed_like_minecraft() {
        let registry = boxel_demo::blocks::registry();
        let by = |n: &str| registry.parse_state(n).expect("built-in");
        let hit = |face, y: f64| VoxelHit {
            block: BlockPos::new(0, 0, 0),
            face: Some(face),
            t: 1.0,
            point: WorldPos::from_dvec3(DVec3::new(0.5, y, 0.0)),
            state: by("boxel:stone"),
        };
        let stairs = by("boxel:oak_stairs");
        // Looking east: the stairs face east (their high side away from the player).
        let east = DVec3::new(0.9, -0.3, 0.2);
        for (face, y, want) in [
            (Face::PosY, 1.0, "boxel:oak_stairs[facing=east]"),
            (Face::NegY, 0.0, "boxel:oak_stairs[facing=east,half=top]"),
            (Face::NegX, 0.75, "boxel:oak_stairs[facing=east,half=top]"),
            (Face::NegX, 0.25, "boxel:oak_stairs[facing=east]"),
        ] {
            assert_eq!(
                placement(&registry, stairs, &hit(face, y), east),
                Some(by(want)),
                "{face:?} {y}"
            );
        }
        let north = DVec3::new(0.1, 0.0, -1.0);
        assert_eq!(
            placement(&registry, stairs, &hit(Face::PosY, 1.0), north),
            Some(by("boxel:oak_stairs[facing=north]"))
        );
        // Torches: standing on top, on the wall facing away from it, never underneath.
        let torch = by("boxel:torch");
        assert_eq!(
            placement(&registry, torch, &hit(Face::PosY, 1.0), east),
            Some(torch)
        );
        assert_eq!(
            placement(&registry, torch, &hit(Face::NegY, 0.0), east),
            None
        );
        for (face, facing) in [
            (Face::PosX, "east"),
            (Face::NegX, "west"),
            (Face::PosZ, "south"),
            (Face::NegZ, "north"),
        ] {
            assert_eq!(
                placement(&registry, torch, &hit(face, 0.5), east),
                Some(by(&format!("boxel:wall_torch[facing={facing}]")))
            );
        }
        // Everything else as before.
        let slab = by("boxel:stone_slab");
        assert_eq!(
            placement(&registry, slab, &hit(Face::NegY, 0.0), east),
            Some(by("boxel:stone_slab[type=top]"))
        );
        let fence = by("boxel:oak_fence");
        assert_eq!(
            placement(&registry, fence, &hit(Face::PosX, 0.5), east),
            Some(fence)
        );
        // ADR 0027: a gate faces the way the player looks; a wall goes in as it is (its rules
        // connect it).
        let gate = by("boxel:oak_fence_gate");
        assert_eq!(
            placement(&registry, gate, &hit(Face::PosY, 1.0), east),
            Some(by("boxel:oak_fence_gate[facing=east]"))
        );
        assert_eq!(
            placement(&registry, gate, &hit(Face::NegZ, 0.5), north),
            Some(by("boxel:oak_fence_gate[facing=north]"))
        );
        let wall = by("boxel:cobblestone_wall");
        assert_eq!(
            placement(&registry, wall, &hit(Face::PosY, 1.0), east),
            Some(wall)
        );
    }

    #[test]
    fn a_slab_on_the_free_half_of_a_slab_makes_a_double_slab() {
        let registry = boxel_demo::blocks::registry();
        let by = |n: &str| registry.parse_state(n).expect("built-in");
        let (bottom, top) = (by("boxel:stone_slab"), by("boxel:stone_slab[type=top]"));
        let double = by("boxel:stone_slab[type=double]");
        let cell = BlockPos::new(4, 5, 6);
        let hit = |state, face, y: f64| VoxelHit {
            block: cell,
            face: Some(face),
            t: 1.0,
            point: WorldPos::from_dvec3(DVec3::new(4.5, 5.0 + y, 6.0)),
            state,
        };
        let air = |_| Some(BlockState::AIR);
        let at_cell = |pos| Some(BlockEdit { pos, state: double });
        // The inner face of the slab itself, with either half held.
        for held in [bottom, top] {
            assert_eq!(
                slab_merge(&registry, held, &hit(bottom, Face::PosY, 0.5), air),
                at_cell(cell)
            );
            assert_eq!(
                slab_merge(&registry, held, &hit(top, Face::NegY, 0.5), air),
                at_cell(cell)
            );
        }
        // Its outer faces place into the next cell as usual.
        assert_eq!(
            slab_merge(&registry, bottom, &hit(bottom, Face::NegY, 0.0), air),
            None
        );
        assert_eq!(
            slab_merge(&registry, bottom, &hit(top, Face::PosY, 1.0), air),
            None
        );
        // Not a slab of the same block (or not a slab at all): no merge.
        let stone = by("boxel:stone");
        assert_eq!(
            slab_merge(&registry, stone, &hit(bottom, Face::PosY, 0.5), air),
            None
        );
        assert_eq!(
            slab_merge(&registry, bottom, &hit(stone, Face::PosY, 1.0), air),
            None
        );
        assert_eq!(
            slab_merge(&registry, bottom, &hit(double, Face::PosY, 1.0), air),
            None
        );
        // The side of a wall whose next cell holds a bottom slab: the upper half of the side
        // places a top slab there, which merges; the lower half a bottom slab, which does not.
        let east = BlockPos::new(5, 5, 6);
        let next = |pos: BlockPos| (pos == east).then_some(bottom);
        assert_eq!(
            slab_merge(&registry, bottom, &hit(stone, Face::PosX, 0.75), next),
            at_cell(east)
        );
        assert_eq!(
            slab_merge(&registry, bottom, &hit(stone, Face::PosX, 0.25), next),
            None
        );
        // An unloaded cell or a ray from inside a block: nothing.
        assert_eq!(
            slab_merge(&registry, bottom, &hit(stone, Face::PosX, 0.75), |_| None),
            None
        );
        let inside = VoxelHit {
            face: None,
            ..hit(bottom, Face::PosY, 0.5)
        };
        assert_eq!(slab_merge(&registry, bottom, &inside, air), None);
        // A waterlogged slab merges into a dry double slab (ADR 0025).
        let wet = by("boxel:stone_slab[waterlogged=true]");
        assert_eq!(
            slab_merge(&registry, bottom, &hit(wet, Face::PosY, 0.5), air),
            at_cell(cell)
        );
    }

    #[test]
    fn placed_and_broken_blocks_keep_the_water_they_hold() {
        let registry = boxel_demo::blocks::registry();
        let by = |n: &str| registry.parse_state(n).expect("built-in");
        let (water, flowing) = (by("boxel:water"), by("boxel:water[level=2]"));
        let (dry, wet) = (
            by("boxel:oak_fence"),
            by("boxel:oak_fence[waterlogged=true]"),
        );
        let cell = BlockPos::new(-3, 64, 1 << 40);
        let edit = |state| BlockEdit { pos: cell, state };
        // Placed into a water source it keeps the water, into flowing water or air it is dry.
        assert_eq!(with_water_rules(&registry, edit(dry), water), edit(wet));
        assert_eq!(with_water_rules(&registry, edit(dry), flowing), edit(dry));
        assert_eq!(
            with_water_rules(&registry, edit(dry), BlockState::AIR),
            edit(dry)
        );
        let stone = by("boxel:stone");
        assert_eq!(with_water_rules(&registry, edit(stone), water), edit(stone));
        // Broken, a waterlogged block leaves its water; a dry one air.
        let air = edit(BlockState::AIR);
        assert_eq!(with_water_rules(&registry, air, wet), edit(water));
        assert_eq!(with_water_rules(&registry, air, dry), air);
        // Water placed on a dry block that holds water fills it; on anything else (or a wet
        // one) it goes into the next cell as usual.
        let hit = |state| VoxelHit {
            block: cell,
            face: Some(Face::PosY),
            t: 1.0,
            point: WorldPos::from_dvec3(DVec3::new(-2.5, 65.0, 0.5)),
            state,
        };
        let stairs = by("boxel:stone_stairs[facing=west]");
        assert_eq!(
            water_fill(&registry, water, &hit(stairs)),
            Some(edit(registry.with_water(stairs, true).expect("wet")))
        );
        assert_eq!(water_fill(&registry, water, &hit(wet)), None);
        assert_eq!(water_fill(&registry, water, &hit(stone)), None);
        assert_eq!(water_fill(&registry, flowing, &hit(dry)), None);
        assert_eq!(water_fill(&registry, stone, &hit(dry)), None);
        let double = by("boxel:stone_slab[type=double]");
        assert_eq!(water_fill(&registry, water, &hit(double)), None);
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
    fn time_and_lamps_options() {
        let o = parse(&[]);
        assert_eq!((o.start_hours(), o.lamps), (12.0, false));
        assert_eq!(parse(&["--lamps"]).start_hours(), LAMPS_DEMO_HOURS);
        assert_eq!(parse(&["--lamps", "--time", "06:30"]).start_hours(), 6.5);
        let args: Vec<String> = ["--time", "25:00"].map(String::from).to_vec();
        assert!(parse_options(&args).is_err());
        // Every demo block and every hotbar block exists.
        let registry = boxel_demo::blocks::registry();
        for name in LAMPS_DEMO_BLOCKS.iter().chain(&HOTBAR) {
            assert!(registry.by_name(name).is_some(), "{name}");
        }
    }

    #[test]
    fn demo_lamps_go_on_the_ground_once_it_is_loaded() {
        let start = DVec3::new(0.5, 80.0, 0.5);
        // Ground at y = 63, loaded down to y = 0.
        let ground = |p: BlockPos| {
            (p.y >= 0).then_some(if p.y <= 63 {
                BlockState(1)
            } else {
                BlockState::AIR
            })
        };
        let cells = demo_lamp_cells(start, ground).expect("loaded");
        assert_eq!(cells.len(), LAMPS_DEMO_BLOCKS.len());
        for (pos, _) in &cells {
            assert_eq!(pos.y, 64);
            let d = ((pos.x as f64 + 0.5).hypot(pos.z as f64 + 0.5) - LAMPS_DEMO_RADIUS).abs();
            assert!(d < 1.5, "{pos:?}");
        }
        assert_eq!(cells[0], (BlockPos::new(9, 64, 0), "boxel:lamp_orange"));
        // Not loaded yet: wait.
        assert_eq!(demo_lamp_cells(start, |_| None), None);
        // No ground within reach: skipped.
        let empty = demo_lamp_cells(start, |_| Some(BlockState::AIR)).expect("loaded");
        assert!(empty.is_empty());
    }

    /// The clock keys in the app: `]` and `[` move the clock an hour, T pauses it and F speeds
    /// it up, while G (walking) and the hotbar digits leave it alone and the clock keys leave
    /// walking and the hotbar alone.
    #[test]
    fn clock_keys_move_pause_and_speed_up_the_time() {
        use player::WALK_KEY;
        let mut app = picking_app();
        let time = |app: &App| *app.world().resource::<TimeOfDay>();
        let selected = |app: &App| app.world().resource::<Picking>().selected;
        // Running from noon: one tick per fixed step (every update is one).
        let start = time(&app);
        assert!(!start.paused && start.speed == 1);
        app.update();
        assert_eq!(time(&app).tick, start.tick + 1);

        // T pauses it.
        tap(&mut app, KeyCode::KeyT);
        let paused = time(&app);
        assert!(paused.paused);
        app.update();
        assert_eq!(time(&app), paused);
        // ] and [ move it one hour, still paused.
        let hour = paused.ticks_per_hour();
        tap(&mut app, KeyCode::BracketRight);
        assert_eq!(
            time(&app),
            TimeOfDay {
                tick: paused.tick + hour,
                ..paused
            }
        );
        tap(&mut app, KeyCode::BracketLeft);
        assert_eq!(time(&app), paused);
        // F: 60 times faster; it neither walks nor changes the hotbar.
        tap(&mut app, KeyCode::KeyF);
        let fast = TimeOfDay {
            speed: FAST_TIME,
            ..paused
        };
        assert_eq!(time(&app), fast);
        assert!(walker(&mut app).is_none());
        assert_eq!(selected(&app), 0);
        // G walks and 3 picks grass; the clock stays as it was.
        tap(&mut app, WALK_KEY);
        assert!(walker(&mut app).is_some());
        tap(&mut app, KeyCode::Digit3);
        assert_eq!(selected(&app), 2);
        assert_eq!(time(&app), fast);
        // T again: running at 60 ticks per step. F again: real time.
        tap(&mut app, KeyCode::KeyT);
        let running = time(&app);
        assert!(!running.paused);
        app.update();
        assert_eq!(time(&app).tick, running.tick + FAST_TIME);
        tap(&mut app, KeyCode::KeyF);
        let normal = time(&app);
        assert_eq!(normal.speed, 1);
        app.update();
        assert_eq!(time(&app).tick, normal.tick + 1);
        assert!(walker(&mut app).is_some(), "still walking");
    }

    #[test]
    fn lamps_demo_places_its_blocks_once_the_ground_is_loaded() {
        let start = DVec3::new(0.5, 6.0, 0.5);
        let app = picking_app_with(Some(start));
        assert_eq!(app.world().resource::<LampsDemo>().start, None, "placed");
        // On the grass (top at y = 3) of the flat world.
        let ground = |p: BlockPos| {
            Some(if p.y <= 3 {
                BlockState(1)
            } else {
                BlockState::AIR
            })
        };
        let cells = demo_lamp_cells(start, ground).expect("loaded");
        assert_eq!(cells.len(), LAMPS_DEMO_BLOCKS.len());
        let registry = boxel_demo::blocks::registry();
        for (pos, name) in cells {
            assert_eq!(pos.y, 4);
            assert_eq!(
                block_at(&app, pos),
                registry.by_name(name),
                "{pos:?} {name}"
            );
        }
    }

    #[test]
    fn duration_run_completes_once_the_time_is_up() {
        let info = RenderInfo::default();
        assert!(Run::new(parse(&["--duration", "0"])).complete(&info));
        assert!(!Run::new(parse(&["--duration", "3600"])).complete(&info));
    }
}

/// F10: the distance fog on or off (`RenderSettings::distance_fog`; `--no-distance-fog` starts
/// with it off), named in the log.
fn toggle_distance_fog(keys: Res<ButtonInput<KeyCode>>, mut settings: ResMut<RenderSettings>) {
    if !keys.just_pressed(KeyCode::F10) {
        return;
    }
    let fog = &mut settings.distance_fog;
    fog.enabled = !fog.enabled;
    eprintln!("distance fog: {}", on_off(fog.enabled));
}

/// The fog preset F7 shows (`--fog`; only while the fog plugin is on).
#[derive(Resource)]
struct FogCycle(fog::FogPreset);

/// F7: the next fog preset (`off` keeps the plugin with no layers), named in the log.
fn cycle_fog(
    keys: Res<ButtonInput<KeyCode>>,
    mut cycle: ResMut<FogCycle>,
    mut settings: ResMut<boxel::render::fog::FogSettings>,
) {
    if !keys.just_pressed(KeyCode::F7) {
        return;
    }
    let all = fog::FogPreset::ALL;
    let i = all.iter().position(|p| *p == cycle.0).unwrap_or(0);
    cycle.0 = all[(i + 1) % all.len()];
    settings.layers = cycle.0.layers().unwrap_or_default();
    eprintln!("fog: {}", cycle.0.name());
}

/// What F8 shows (`--lod`; only with the LOD plugin): a preset, or the LOD hidden.
#[derive(Resource)]
struct LodCycle {
    /// The preset whose settings are in use.
    preset: lod::LodPreset,
    /// F8 hid the LOD (`LodSettings::enabled` false, the nodes dropped).
    hidden: bool,
    /// A software adapter: the LOD stays off whatever F8 shows.
    software: bool,
}

impl LodCycle {
    fn new(preset: lod::LodPreset) -> Self {
        Self {
            preset,
            hidden: false,
            software: false,
        }
    }

    /// The F8 step after this one: far, extreme, hidden, near, then far again.
    fn next(&self) -> (lod::LodPreset, bool) {
        use lod::LodPreset::{Extreme, Far, Near, Off};
        match (self.preset, self.hidden) {
            (Far, false) => (Extreme, false),
            (Extreme, false) => (Extreme, true),
            (_, true) => (Near, false),
            (Near | Off, false) => (Far, false),
        }
    }

    /// Its name for the log and F3.
    fn name(&self) -> String {
        if self.hidden {
            "hidden".to_string()
        } else {
            lod::label(self.preset)
        }
    }
}

/// F8: the next LOD step (a preset with another arena rebuilds the LOD pass, which fills again
/// coarse first); F9: the level tint. Both named in the log.
fn cycle_lod(
    keys: Res<ButtonInput<KeyCode>>,
    mut cycle: ResMut<LodCycle>,
    mut settings: ResMut<LodSettings>,
) {
    if keys.just_pressed(KeyCode::F8) {
        (cycle.preset, cycle.hidden) = cycle.next();
        if let Some(preset) = cycle.preset.settings() {
            *settings = LodSettings {
                enabled: !cycle.hidden && !cycle.software,
                debug_tint: settings.debug_tint,
                ..preset
            };
        }
        let off = if cycle.software {
            " (off: software adapter)"
        } else {
            ""
        };
        eprintln!("lod: {}{off}", cycle.name());
    }
    if keys.just_pressed(KeyCode::F9) {
        settings.debug_tint = !settings.debug_tint;
        eprintln!("lod: level tint {}", on_off(settings.debug_tint));
    }
}

/// The F3 `lod` lines: the preset, the horizon, nodes resident and drawn, the fill, then jobs
/// queued and running, draws and quads of the frame and the refusals.
fn lod_overlay_line(
    cycle: Res<LodCycle>,
    stats: Res<boxel::world::lod::LodStats>,
    info: Res<RenderInfo>,
    lines: Option<ResMut<OverlayLines>>,
) {
    let Some(mut lines) = lines else {
        return;
    };
    let name = if cycle.hidden {
        "hidden"
    } else {
        cycle.preset.name()
    };
    let state = match stats.off {
        Some(off) => format!("off ({off:?})"),
        None => format!("to {:.0} blocks", stats.horizon),
    };
    lines.set(
        "lod",
        format!(
            "LOD {name} {state}: {} nodes, {} drawn, fill {:.0}%",
            stats.nodes(),
            stats.drawn(),
            stats.fill * 100.0,
        ),
    );
    lines.set(
        "lod work",
        format!(
            "    jobs {} queued {} running, {} draws, {:.2}M quads, {} refused",
            stats.queued_jobs,
            stats.in_flight,
            info.lod_draws,
            info.lod_quads as f64 / 1e6,
            stats.refused
        ),
    );
}
