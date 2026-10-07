//! Stress benchmark: frame times (and their spikes) while the world streams.
//!
//! `cargo run --release --example stress -- --scenario fly --speed 64 --radius 16 \
//!   --summary out.md --csv out.csv` (see `--help`; `just bench-fly` and friends wrap it).
//!
//! Scenarios (the camera is scripted, never driven by input):
//! - `static`: the camera stays put; measures the settled world.
//! - `fly`: the camera flies straight ahead at `--speed` voxels per second, so new chunks
//!   stream in all the time.
//! - `dive`: the camera moves straight down at `--speed` voxels per second (new layers of
//!   chunks under it all the time; with `--world overworld` it digs into the caves).
//! - `spin`: the camera turns 360 degrees in place over the measurement (visibility changes,
//!   no new chunks).
//!
//! `--edits` adds scripted block edits during the measurement (a lamp placed and removed, a
//! shaft dug and capped, one per second near the start) and reports how many frames the world
//! takes to publish each one. The world clock stays still at `--time` (noon by default).
//!
//! `--bodies N --body-kind item|mob --projectiles M` add physics bodies around the start (see
//! [`bodies`]): what they add to the frame is the physics part of the frame rule (ADR 0016).
//!
//! `--light off` turns the voxel light and the ambient occlusion off (`WorldSettings::light` and
//! `ao_policy: AoPolicy::Off`): the look and the cost of the world before light (ADR 0015).
//!
//! Every run waits until the world settled, runs `--warmup` seconds, then measures
//! `--duration` seconds. Each frame records its wall-clock time (`Time::real_delta`, the whole
//! loop: world and render) and the work it did: chunks generated and jobs finished (counted by
//! wrapping the generator and the job executor), meshes sent and removed (`ChunkMeshEvent`),
//! `WorldStats` (and its change), the GPU chunks, draws, arena use and dropped chunks
//! (`RenderInfo`), the resident quads and whether it was presented. The window renders every
//! update even when covered (`render_when_hidden`), but only about one frame per monitor refresh
//! is presented (the compositor's frame callback opens the gate); the others render offscreen. It presents with
//! `immediate` unless `--present`/`BOXEL_PRESENT` say otherwise, and closes itself.
//!
//! Exit codes: 0 success, 1 usage error, 2 no usable GPU, 3 an output file could not be
//! written, 4 timeout (the world did not settle, or the run took longer than `--timeout`).
#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]

#[path = "stress/bodies.rs"]
mod bodies;
#[path = "shared/clock.rs"]
mod clock;
#[path = "stress/edits.rs"]
mod edits;
#[path = "shared/pack.rs"]
mod pack;
#[path = "stress/report.rs"]
mod report;
#[path = "stress/spans.rs"]
mod spans;
#[path = "shared/world.rs"]
mod world;

use bodies::{BodiesPlugin, BodyOptions};
use boxel::app::{TaskPoolOptions, TaskPoolThreadAssignmentPolicy};
use boxel::gfx::{CLI_HELP, PresentModePref};
use boxel::log::span_times::{self, SpanFrame, SpanTimes};
use boxel::mesh::AoPolicy;
use boxel::prelude::*;
use boxel::render::{GpuFailed, TextureSource};
use boxel::voxel::{BlockPos, BlockRegistry, ChunkData, ChunkKey, ChunkPos, MAX_BLOCK_COORD};
use boxel::window::FramePacingInfo;
use boxel::world::jobs::{JobExecutor, TaskPoolExecutor};
use boxel::world::{ChunkId, ChunkMeshEvent, WorldContent, WorldStats, WorldSystems};
use edits::EditScript;
use report::{FrameRecord, RecordLog, RunInfo, Summary};
use spans::{SpanLog, SpanRow};
use std::collections::HashMap;
use std::f64::consts::TAU;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use world::{World, WorldChoice};

const USAGE: &str = "\
usage: stress [--scenario static|fly|dive|spin] [GPU options] [options]
  --scenario NAME     static (default), fly, dive (straight down) or spin
  --radius N          mesh radius in chunks (default 16)
  --speed V           fly and dive speed in voxels per second (default 32)
  --duration SECS     measured time (default 20)
  --warmup SECS       time between the world settling and the measurement (default 2)
  --timeout SECS      wall-clock limit for the whole run, exit 4 (default 120 + warmup +
                      duration)
  --csv PATH          write one row per measured frame
  --summary PATH      write the summary (Markdown tables plus a JSON block for
                      tools/bench_report.py)
  --label TEXT        free text kept in the summary (for example a commit)
  --seed N            terrain seed
  --camera x,y,z,yaw,pitch  start camera, degrees (default 16,140,16,-90,-20: the middle of a
                      chunk, above the terrain, looking along +X;
                      fly moves along the yaw direction, horizontally)
  --size WxH          window size in logical pixels (default 1280x720)
  --async-threads N   async compute pool size (chunk jobs)
  --vertical-radius N chunks loaded above and below the camera (default 8)
  --fixed-height      load only the generator's layers, like M0 (for comparisons with older runs;
                      not with the overworld, which has no floor)
  --arena-quads N     renderer quad arena (default 8388608, 25165824 with the overworld)
  --chunk-slots N     renderer chunk table (default 32768, 65536 with the overworld)
  --fps-limit N       at most N updates per second (default 0: unlimited, the uncapped loop)
  --vsync on|off      vsync: on is --present vsync (Fifo), off is --present novsync (Mailbox,
                      else Immediate); without it the run presents with --present immediate
  --spans             busy time per system and span in each frame, added to the summary (needs
                      a build with --features trace)
  --spans-csv PATH    also write it per frame (implies --spans)
  --edits             scripted edits during the measurement, one per second near the start: a
                      lamp placed and removed, a shaft dug and capped; the summary gives the
                      frames each one took to publish (meant for --scenario static)
  --time HH[:MM]      clock time of the world, kept still (default 12:00)
  --light on|off      voxel light and ambient occlusion (default on); off: neither, the look and
                      the cost of the world before light
  --pack DIR          block textures from a Minecraft Java resource pack (the directory with
                      pack.mcmeta; default: procedural); the summary records it and its load time
  --help              this text
The run presents with --present immediate unless --vsync, --present or BOXEL_PRESENT is given.
exit codes: 0 ok, 1 usage error, 2 no usable GPU, 3 output not written, 4 timeout";

/// The whole help: [`USAGE`], the physics options and the world options.
fn usage() -> String {
    format!(
        "{USAGE}\nphysics options:\n{}\nworld options:\n{}",
        bodies::USAGE,
        world::usage(World::Forest)
    )
}

/// Consecutive settled frames before the world counts as settled.
const SETTLED_FRAMES: u32 = 3;
/// Default `--timeout` part spent waiting for the world to settle.
const SETTLE_ALLOWANCE: Duration = Duration::from_secs(120);
/// After the measurement, the world gets this long (within `--timeout`) to settle again.
const CATCH_UP_LIMIT: Duration = Duration::from_secs(60);
/// Extra time the watchdog thread waits after `--timeout` before killing the process.
const WATCHDOG_GRACE: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------------------------
// Options

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    Static,
    Fly,
    Dive,
    Spin,
}

impl Scenario {
    const ALL: [Scenario; 4] = [
        Scenario::Static,
        Scenario::Fly,
        Scenario::Dive,
        Scenario::Spin,
    ];

    fn name(self) -> &'static str {
        match self {
            Scenario::Static => "static",
            Scenario::Fly => "fly",
            Scenario::Dive => "dive",
            Scenario::Spin => "spin",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// True if the camera moves at `--speed`.
    fn moves(self) -> bool {
        matches!(self, Scenario::Fly | Scenario::Dive)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Options {
    scenario: Scenario,
    radius: u32,
    speed: f64,
    duration: Duration,
    warmup: Duration,
    timeout: Duration,
    csv: Option<PathBuf>,
    summary: Option<PathBuf>,
    label: String,
    seed: u64,
    camera: [f64; 5],
    size: (u32, u32),
    async_threads: Option<usize>,
    world: WorldChoice,
    vertical_radius: u32,
    fixed_height: bool,
    /// `--arena-quads` and `--chunk-slots` (`None`: the world's default).
    arena_quads: Option<u32>,
    chunk_slots: Option<u32>,
    /// `--fps-limit` (0: unlimited).
    fps_limit: u32,
    /// `--vsync` (`None`: not given).
    vsync: Option<bool>,
    spans: bool,
    spans_csv: Option<PathBuf>,
    /// `--edits`.
    edits: bool,
    /// `--time`, clock hours.
    time: f64,
    /// `--light`: voxel light and ambient occlusion on (the default) or off.
    light: bool,
    /// `--pack`.
    textures: TextureSource,
    /// `--bodies`, `--body-kind` and `--projectiles`.
    bodies: BodyOptions,
}

impl Options {
    /// Renderer `(arena quads, chunk slots)`: the flags, else the world's defaults.
    fn render_capacity(&self) -> (u32, u32) {
        let (quads, slots) = self.world.render_capacity();
        (
            self.arena_quads.unwrap_or(quads),
            self.chunk_slots.unwrap_or(slots),
        )
    }
}

/// `Ok(None)` for `--help`.
fn parse_options(args: &[String]) -> Result<Option<Options>, String> {
    let mut o = Options {
        scenario: Scenario::Static,
        radius: 16,
        speed: 32.0,
        duration: Duration::from_secs(20),
        warmup: Duration::from_secs(2),
        timeout: Duration::ZERO,
        csv: None,
        summary: None,
        label: String::new(),
        seed: TerrainGenerator::default().seed,
        camera: [16.0, 140.0, 16.0, -90.0, -20.0],
        size: (1280, 720),
        async_threads: None,
        world: WorldChoice::default(),
        vertical_radius: ChunkViewer::DEFAULT_VERTICAL_RADIUS,
        fixed_height: false,
        arena_quads: None,
        chunk_slots: None,
        fps_limit: 0,
        vsync: None,
        spans: false,
        spans_csv: None,
        edits: false,
        time: 12.0,
        light: true,
        textures: TextureSource::Procedural,
        bodies: BodyOptions::default(),
    };
    let mut timeout = None;
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
        if o.world.parse_flag(flag, &mut value)? || o.bodies.parse_flag(flag, &mut value)? {
            continue;
        }
        match flag {
            "--help" | "-h" => return Ok(None),
            "--scenario" => {
                let v = value()?;
                o.scenario = Scenario::parse(&v)
                    .ok_or_else(|| format!("{flag}: {v:?} is not static, fly, dive or spin"))?;
            }
            "--radius" => {
                let r = number(flag, &value()?)?;
                o.radius = u32::try_from(r)
                    .ok()
                    .filter(|r| (1..=64).contains(r))
                    .ok_or_else(|| format!("{flag}: {r} is not in 1..=64"))?;
            }
            "--speed" => o.speed = positive(flag, &value()?)?,
            "--duration" => o.duration = seconds(flag, &value()?)?,
            "--warmup" => {
                let v = value()?;
                o.warmup = if v.trim() == "0" {
                    Duration::ZERO
                } else {
                    seconds(flag, &v)?
                };
            }
            "--timeout" => timeout = Some(seconds(flag, &value()?)?),
            "--csv" => o.csv = Some(PathBuf::from(value()?)),
            "--summary" => o.summary = Some(PathBuf::from(value()?)),
            "--label" => o.label = value()?,
            "--seed" => o.seed = number(flag, &value()?)?,
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
            "--size" => {
                let v = value()?;
                o.size = v
                    .split_once(['x', 'X'])
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .filter(|&(w, h): &(u32, u32)| w > 0 && h > 0 && w <= 16384 && h <= 16384)
                    .ok_or_else(|| format!("{flag}: {v:?} is not WxH"))?;
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
            "--arena-quads" => {
                let n = number(flag, &value()?)?;
                o.arena_quads = Some(
                    u32::try_from(n)
                        .ok()
                        .filter(|n| *n >= 1024)
                        .ok_or_else(|| format!("{flag}: {n} is not in 1024..=4294967295"))?,
                );
            }
            "--chunk-slots" => {
                let n = number(flag, &value()?)?;
                o.chunk_slots = Some(
                    u32::try_from(n)
                        .ok()
                        .filter(|n| (1..=1 << 20).contains(n))
                        .ok_or_else(|| format!("{flag}: {n} is not in 1..=1048576"))?,
                );
            }
            "--vertical-radius" => {
                let r = number(flag, &value()?)?;
                o.vertical_radius = u32::try_from(r)
                    .ok()
                    .filter(|r| *r <= 64)
                    .ok_or_else(|| format!("{flag}: {r} is not in 0..=64"))?;
            }
            "--fixed-height" => o.fixed_height = true,
            "--fps-limit" => {
                let n = number(flag, &value()?)?;
                o.fps_limit = u32::try_from(n)
                    .ok()
                    .filter(|n| *n <= 100_000)
                    .ok_or_else(|| format!("{flag}: {n} is not in 0..=100000"))?;
            }
            "--vsync" => {
                o.vsync = Some(match value()?.as_str() {
                    "on" => true,
                    "off" => false,
                    v => return Err(format!("{flag}: {v:?} is not on or off")),
                });
            }
            "--spans" => o.spans = true,
            "--spans-csv" => {
                o.spans_csv = Some(PathBuf::from(value()?));
                o.spans = true;
            }
            "--edits" => o.edits = true,
            "--time" => o.time = clock::parse_clock(flag, &value()?)?,
            "--pack" => o.textures = pack::parse(flag, &value()?)?,
            "--light" => {
                o.light = match value()?.as_str() {
                    "on" => true,
                    "off" => false,
                    v => return Err(format!("{flag}: {v:?} is not on or off")),
                };
            }
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    o.world.validate(o.fixed_height)?;
    o.timeout = timeout.unwrap_or(SETTLE_ALLOWANCE + o.warmup + o.duration);
    Ok(Some(o))
}

fn number(flag: &str, value: &str) -> Result<u64, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("{flag}: {value:?} is not a non-negative integer"))
}

fn positive(flag: &str, value: &str) -> Result<f64, String> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
        .ok_or_else(|| format!("{flag}: {value:?} is not a positive number"))
}

fn seconds(flag: &str, value: &str) -> Result<Duration, String> {
    let s = positive(flag, value)?;
    Duration::try_from_secs_f64(s).map_err(|_| format!("{flag}: {value:?} is too large"))
}

// ---------------------------------------------------------------------------------------------
// Counting wrappers (exact world counters without touching boxel_world)

/// Counts finished generator calls.
struct CountingGenerator {
    inner: Arc<dyn WorldGenerator>,
    generated: Arc<AtomicU64>,
}

impl WorldGenerator for CountingGenerator {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let data = self.inner.generate(key, registry);
        self.generated.fetch_add(1, Ordering::Relaxed);
        data
    }

    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        self.inner.vertical_chunks()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }
}

/// Runs jobs on the async compute pool (as `TaskPoolExecutor`) and counts the finished ones.
struct CountingExecutor {
    done: Arc<AtomicU64>,
}

impl JobExecutor for CountingExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        let done = Arc::clone(&self.done);
        TaskPoolExecutor.spawn(Box::new(move || {
            job();
            done.fetch_add(1, Ordering::Relaxed);
        }));
    }
}

// ---------------------------------------------------------------------------------------------
// Camera script

/// Camera pose of the scenario `t` seconds into the measurement, from the start pose
/// `[x, y, z, yaw, pitch]` (degrees): `fly` moves along the horizontal yaw direction (yaw 0
/// looks along -Z, -90 along +X), `dive` moves straight down (the view keeps its yaw and
/// pitch), `spin` turns once over `duration` seconds.
fn camera_at(scenario: Scenario, start: [f64; 5], speed: f64, duration: f64, t: f64) -> Transform {
    let [x, y, z, yaw, pitch] = start;
    let mut yaw = yaw.to_radians();
    let mut position = DVec3::new(x, y, z);
    match scenario {
        Scenario::Static => {}
        Scenario::Fly => position += flat_direction(yaw) * (speed * t),
        Scenario::Dive => position.y -= speed * t,
        Scenario::Spin => yaw += TAU * (t / duration).min(1.0),
    }
    Transform::from_yaw_pitch(position, yaw as f32, (pitch as f32).to_radians())
}

/// Horizontal unit direction of `yaw` (radians). Components within 1e-12 of zero are zero, so
/// flying along an axis never drifts across the neighboring chunk border (sin(-90 degrees)
/// leaves -6e-17 in z).
fn flat_direction(yaw: f64) -> DVec3 {
    let snap = |v: f64| if v.abs() < 1e-12 { 0.0 } else { v };
    DVec3::new(snap(-yaw.sin()), 0.0, snap(-yaw.cos()))
}

fn chunk_of(transform: &Transform) -> ChunkPos {
    ChunkPos::containing(BlockPos::from_world(transform.translation))
}

// ---------------------------------------------------------------------------------------------
// App state

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    /// Waiting for the GPU and the world (settled for `SETTLED_FRAMES` frames).
    Loading { streak: u32 },
    /// The world settled; measuring starts at `until`.
    Warmup { until: Instant },
    /// Measuring since `start`.
    Measuring { start: Instant },
    /// The camera stopped at `since`; waiting for the world to settle again.
    CatchUp { since: Instant, streak: u32 },
    /// Outputs written, exit requested.
    Done,
}

/// The part of a record known during the frame's `Update` (the rest comes from the next one).
#[derive(Clone, Copy, Debug)]
struct Pending {
    t: Duration,
    crossed: bool,
    camera: (f64, f64, f32),
    /// `FramePacingInfo` of the frame: gate wait, gate missed, last gate opening.
    gate_wait: Duration,
    gate_missed: bool,
    gate_at: Option<Duration>,
    display_locked: bool,
}

#[derive(Resource)]
struct Bench {
    options: Options,
    started: Instant,
    phase: Phase,
    info: RunInfo,
    /// `frames_presented` when the measurement started.
    presented_at_start: u64,
    exiting: bool,
}

/// `--spans`: the span times of each measured frame, next to its [`FrameRecord`].
#[derive(Resource)]
struct SpanRecorder {
    times: SpanTimes,
    /// The layer's last closed frame (reused buffer).
    frame: SpanFrame,
    /// `frame.seq` seen by the previous `record`.
    last_seq: u64,
    log: SpanLog,
}

impl SpanRecorder {
    fn new(times: SpanTimes) -> Self {
        Self {
            times,
            frame: SpanFrame::default(),
            last_seq: 0,
            log: SpanLog::default(),
        }
    }
}

/// Turns the counters of each measured frame into a [`FrameRecord`].
#[derive(Resource)]
struct Recorder {
    generated: Arc<AtomicU64>,
    jobs_done: Arc<AtomicU64>,
    last_generated: u64,
    last_jobs: u64,
    last_stats: WorldStats,
    last_presented: u64,
    /// Quads of every mesh sent to the renderer and not removed.
    resident: HashMap<ChunkId, u32>,
    quads: u64,
    /// Set by `drive` for a measured frame, completed by `record` in the next frame.
    pending: Option<Pending>,
    log: RecordLog,
}

impl Recorder {
    /// `slots`: the renderer's chunk table, the most meshes resident at once (the quad map is
    /// allocated once so it never rehashes during the measurement).
    fn new(generated: Arc<AtomicU64>, jobs_done: Arc<AtomicU64>, slots: usize) -> Self {
        Self {
            generated,
            jobs_done,
            last_generated: 0,
            last_jobs: 0,
            last_stats: WorldStats::default(),
            last_presented: 0,
            resident: HashMap::with_capacity(slots),
            quads: 0,
            pending: None,
            log: RecordLog::default(),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Systems

fn spawn_camera(mut commands: Commands, bench: Res<Bench>) {
    let o = &bench.options;
    commands.spawn((
        Camera {
            view_distance: o.radius as f32 * 32.0,
            ..Camera::default()
        },
        camera_at(o.scenario, o.camera, o.speed, 1.0, 0.0),
        ChunkViewer::new(o.radius).with_vertical_radius(if o.fixed_height {
            u32::MAX
        } else {
            o.vertical_radius
        }),
    ));
}

/// Exit 2 when no GPU can be used.
fn gpu_failed(
    mut failed: MessageReader<GpuFailed>,
    mut bench: ResMut<Bench>,
    mut exit: MessageWriter<AppExit>,
) {
    if let Some(event) = failed.read().last() {
        eprintln!("stress: no usable GPU: {}", event.error);
        bench.exit(2, &mut exit);
    }
}

/// `PreUpdate`: completes the record of the previous frame. Its `Time::real_delta` (measured
/// at this frame's `First`) spans that whole frame; `WorldStats` (its `Last`), `RenderInfo` (its
/// `Render`) and the mesh messages (its `Update`) belong to it too. With `--spans`, so do the
/// span times the layer closed when this frame's `Main` schedule started.
fn record(
    time: Res<Time>,
    stats: Res<WorldStats>,
    info: Res<RenderInfo>,
    mut events: MessageReader<ChunkMeshEvent>,
    mut rec: ResMut<Recorder>,
    mut spans: Option<ResMut<SpanRecorder>>,
) {
    if let Some(spans) = spans.as_deref_mut() {
        spans.times.last_frame(&mut spans.frame);
    }
    let (mut meshes, mut removed, mut upload_quads) = (0u32, 0u32, 0u64);
    for event in events.read() {
        match event {
            ChunkMeshEvent::Upsert { id, mesh, .. } => {
                let quads = mesh.total_quads();
                meshes += 1;
                upload_quads += u64::from(quads);
                let old = rec.resident.insert(*id, quads).unwrap_or(0);
                rec.quads = rec.quads + u64::from(quads) - u64::from(old);
            }
            ChunkMeshEvent::Remove { id, .. } => {
                removed += 1;
                if let Some(quads) = rec.resident.remove(id) {
                    rec.quads -= u64::from(quads);
                }
            }
        }
    }
    let generated = rec.generated.load(Ordering::Relaxed);
    let jobs = rec.jobs_done.load(Ordering::Relaxed);
    let now = *stats;
    if let Some(p) = rec.pending.take() {
        let delta = |a: usize, b: usize| a as i64 - b as i64;
        let record = FrameRecord {
            t: p.t,
            frame_time: time.real_delta(),
            presented: info.frames_presented > rec.last_presented,
            crossed: p.crossed,
            camera: p.camera,
            generated: (generated - rec.last_generated) as u32,
            jobs_done: (jobs - rec.last_jobs) as u32,
            meshes,
            removed,
            upload_quads,
            d_data_ready: delta(now.data_ready, rec.last_stats.data_ready),
            d_chunks: delta(now.chunks, rec.last_stats.chunks),
            world: now,
            gpu_chunks: info.chunks,
            quads: rec.quads,
            draws: info.draws,
            arena_used: info.arena_used_quads,
            dropped_chunks: info.dropped_chunks,
            gate_wait: p.gate_wait,
            gate_missed: p.gate_missed,
            gate_at: p.gate_at,
            display_locked: p.display_locked,
        };
        rec.log.push(record);
        if let Some(spans) = spans.as_deref_mut() {
            if spans.frame.seq != spans.last_seq + 1 {
                spans.log.unmatched += 1;
            }
            spans.log.push(SpanRow::from_frame(&spans.frame));
        }
    }
    if let Some(spans) = spans.as_deref_mut() {
        spans.last_seq = spans.frame.seq;
    }
    rec.last_generated = generated;
    rec.last_jobs = jobs;
    rec.last_stats = now;
    rec.last_presented = info.frames_presented;
}

/// `Update`, before the world computes interest: phases, the camera script and the exit.
///
/// Its clock is the start of the frame (`Time::last_update`, set in `First`), so the `t` values
/// of two consecutive records differ by exactly the frame time of the first one.
fn drive(
    time: Res<Time>,
    mut bench: ResMut<Bench>,
    mut rec: ResMut<Recorder>,
    info: Res<RenderInfo>,
    stats: Res<WorldStats>,
    mut cameras: Query<&mut Transform, With<Camera>>,
    mut exit: MessageWriter<AppExit>,
    spans: Option<Res<SpanRecorder>>,
    script: Option<Res<EditScript>>,
    pacing: Option<Res<FramePacingInfo>>,
) {
    if bench.exiting {
        return;
    }
    let pacing = pacing.as_deref().copied().unwrap_or_default();
    let spans = spans.as_deref();
    let script = script.as_deref();
    let now = time.last_update().unwrap_or_else(Instant::now);
    let elapsed = now.saturating_duration_since(bench.started);
    if elapsed > bench.options.timeout {
        match bench.phase {
            Phase::Loading { .. } => eprintln!(
                "stress: the world did not settle within {:.0} s: {:?}",
                bench.options.timeout.as_secs_f64(),
                *stats
            ),
            Phase::CatchUp { .. } => {
                // The measurement is complete; only the catch-up time is missing.
                bench.finish(&rec, &info, &pacing, spans, script, None, &mut exit);
                return;
            }
            _ => eprintln!(
                "stress: timeout after {:.0} s",
                bench.options.timeout.as_secs_f64()
            ),
        }
        bench.exit(4, &mut exit);
        return;
    }
    let settled = info.ready && stats.chunks > 0 && stats.is_settled();
    match bench.phase {
        Phase::Loading { streak } => {
            let streak = if settled { streak + 1 } else { 0 };
            bench.phase = Phase::Loading { streak };
            if streak >= SETTLED_FRAMES {
                let elapsed = elapsed.as_secs_f64();
                println!(
                    "stress: world settled after {elapsed:.2} s: chunks={} meshed={} empty={} \
                     gpu_chunks={}",
                    stats.chunks, stats.mesh_ready, stats.mesh_empty, info.chunks
                );
                bench.info.settle_s = elapsed;
                bench.info.settle_chunks = stats.chunks;
                bench.phase = Phase::Warmup {
                    until: now + bench.options.warmup,
                };
            }
        }
        Phase::Warmup { until } if now >= until => {
            bench.phase = Phase::Measuring { start: now };
            bench.presented_at_start = info.frames_presented;
            bench.info.load_start = load_average();
            bench.info.unix_time = unix_time();
            println!(
                "stress: measuring {} for {:.0} s",
                bench.options.scenario.name(),
                bench.options.duration.as_secs_f64()
            );
            measure_frame(&bench.options, now, now, &pacing, &mut rec, &mut cameras);
        }
        Phase::Measuring { start } => {
            if now.duration_since(start) >= bench.options.duration {
                bench.info.load_end = load_average();
                bench.info.frames_presented = info.frames_presented - bench.presented_at_start;
                bench.phase = Phase::CatchUp {
                    since: now,
                    streak: 0,
                };
            } else {
                measure_frame(&bench.options, start, now, &pacing, &mut rec, &mut cameras);
            }
        }
        Phase::CatchUp { since, streak } => {
            let streak = if settled { streak + 1 } else { 0 };
            bench.phase = Phase::CatchUp { since, streak };
            if streak >= SETTLED_FRAMES {
                let catch_up = Some(now.duration_since(since));
                bench.finish(&rec, &info, &pacing, spans, script, catch_up, &mut exit);
            } else if now.duration_since(since) > CATCH_UP_LIMIT {
                bench.finish(&rec, &info, &pacing, spans, script, None, &mut exit);
            }
        }
        Phase::Warmup { .. } | Phase::Done => {}
    }
}

/// `--edits`, after `drive`: the next scripted edit while measuring (see [`edits`]).
fn scripted_edits(
    time: Res<Time>,
    bench: Res<Bench>,
    stats: Res<WorldStats>,
    script: Option<ResMut<EditScript>>,
    world: Option<(Res<ChunkMap>, Res<WorldContent>, ResMut<EditQueue>)>,
) {
    let (Some(mut script), Some((map, content, mut queue))) = (script, world) else {
        return;
    };
    if !matches!(bench.phase, Phase::Measuring { .. }) {
        return;
    }
    let now = time.last_update().unwrap_or_else(Instant::now);
    script.frame(
        now,
        stats.is_settled(),
        &content,
        |p| map.block_at(p),
        |pos, state| queue.set_block(pos, state),
    );
}

/// Moves the camera for a measured frame and leaves the frame's first half of the record.
fn measure_frame(
    options: &Options,
    start: Instant,
    now: Instant,
    pacing: &FramePacingInfo,
    rec: &mut Recorder,
    cameras: &mut Query<&mut Transform, With<Camera>>,
) {
    let t = now.duration_since(start);
    for mut transform in cameras.iter_mut() {
        let before = chunk_of(&transform);
        let next = camera_at(
            options.scenario,
            options.camera,
            options.speed,
            options.duration.as_secs_f64(),
            t.as_secs_f64(),
        );
        // Only a real move marks the transform changed (a static camera stays untouched).
        transform.set_if_neq(next);
        // Compass yaw of the view direction (0 looks along -Z, -90 along +X).
        let forward = next.forward();
        let yaw = (-forward.x).atan2(-forward.z).to_degrees();
        rec.pending = Some(Pending {
            t,
            crossed: chunk_of(&next) != before,
            camera: (next.translation.x, next.translation.z, yaw),
            gate_wait: pacing.gate_wait,
            gate_missed: pacing.gate_missed,
            gate_at: pacing
                .gate_opened_at
                .and_then(|at| at.checked_duration_since(start)),
            display_locked: pacing.display_locked,
        });
    }
}

impl Bench {
    /// Writes the outputs, prints the result line and exits (0, or 3 if a file failed).
    #[expect(clippy::too_many_arguments, reason = "everything a summary needs")]
    fn finish(
        &mut self,
        rec: &Recorder,
        info: &RenderInfo,
        pacing: &FramePacingInfo,
        spans: Option<&SpanRecorder>,
        script: Option<&EditScript>,
        catch_up: Option<Duration>,
        exit: &mut MessageWriter<AppExit>,
    ) {
        self.phase = Phase::Done;
        self.info.catch_up_s = catch_up.map(|d| d.as_secs_f64());
        self.info.adapter.clone_from(&info.adapter);
        self.info.backend.clone_from(&info.backend);
        self.info.driver.clone_from(&info.driver);
        self.info.present_mode = info
            .present_mode
            .map_or_else(String::new, |mode| format!("{mode:?}"));
        self.info.refresh_hz = pacing.refresh_millihertz.map(|mhz| f64::from(mhz) / 1000.0);
        self.info.tier = info
            .tier
            .map_or_else(|| "?".to_string(), |t| format!("{t:?}"));
        self.info.class = info
            .class
            .map_or_else(|| "?".to_string(), |c| format!("{c:?}"));
        if info.textures.size > 0 {
            self.info.texture_load_ms = Some(info.textures.load_time.as_secs_f64() * 1000.0);
        }
        // What the renderer got: the device's limits may cut the requested arena and table.
        if info.arena_capacity_quads > 0 {
            if (info.arena_capacity_quads, info.chunk_slot_capacity)
                != (self.info.arena_quads, self.info.chunk_slots)
            {
                println!(
                    "stress: the renderer got {} quads and {} chunk slots (asked for {} and {})",
                    info.arena_capacity_quads,
                    info.chunk_slot_capacity,
                    self.info.arena_quads,
                    self.info.chunk_slots
                );
            }
            self.info.arena_quads = info.arena_capacity_quads;
            self.info.chunk_slots = info.chunk_slot_capacity;
        }
        let records = rec.log.to_vec();
        if let (Some(script), Some(first), Some(last)) = (script, records.first(), records.last()) {
            self.info.edits = Some(script.stats(&first.world, &last.world));
        }
        let Some(summary) = Summary::compute(&records) else {
            eprintln!("stress: no frame was measured");
            self.exit(3, exit);
            return;
        };
        println!("{}", report::one_line(&self.info, &summary));
        let mut code = 0;
        if let Some(path) = &self.options.csv
            && let Err(e) = write_file(path, &report::csv(&records))
        {
            eprintln!("stress: {e}");
            code = 3;
        }
        let span_keys = spans.map(|s| s.times.keys()).unwrap_or_default();
        if let Some(path) = &self.options.summary {
            let mut text = report::markdown(&self.info, &summary);
            if let Some(spans) = spans {
                // Before the data block, with the other tables for people.
                let at = text.find(report::DATA_HEADING).unwrap_or(text.len());
                text.insert_str(at, &spans::markdown(&records, &spans.log, &span_keys));
            }
            if let Err(e) = write_file(path, &text) {
                eprintln!("stress: {e}");
                code = 3;
            }
        }
        if let (Some(path), Some(spans)) = (&self.options.spans_csv, spans)
            && let Err(e) = write_file(path, &spans::csv(&records, &spans.log, &span_keys))
        {
            eprintln!("stress: {e}");
            code = 3;
        }
        self.exit(code, exit);
    }

    fn exit(&mut self, code: u8, exit: &mut MessageWriter<AppExit>) {
        if self.exiting {
            return;
        }
        self.exiting = true;
        self.phase = Phase::Done;
        exit.write(AppExit::from_code(code));
    }
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("stress: wrote {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Machine information

/// 1-minute load average (Linux; `None` elsewhere).
fn load_average() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    text.split_whitespace().next()?.parse().ok()
}

/// CPU model name (Linux; empty elsewhere).
fn cpu_name() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, name)| name.trim().to_string())
        })
        .unwrap_or_default()
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Name of the present mode a run uses: `--present`, else a valid `BOXEL_PRESENT` (the engine
/// reads it the same way), else `immediate`, which is then returned to be set (the bench measures
/// the uncapped loop).
fn present_mode(
    cli: Option<PresentModePref>,
    env: Option<PresentModePref>,
) -> (String, Option<PresentModePref>) {
    match cli.or(env) {
        Some(mode) => (format!("{mode:?}").to_lowercase(), None),
        None => ("immediate".to_string(), Some(PresentModePref::Immediate)),
    }
}

/// The world's settings: `vertical_bounds` (`--fixed-height`) and, with `--light off`, no voxel
/// light and no ambient occlusion (the world before light).
fn world_settings(
    options: &Options,
    vertical_bounds: Option<RangeInclusive<i64>>,
) -> WorldSettings {
    let mut settings = WorldSettings {
        vertical_bounds,
        ..WorldSettings::default()
    };
    if !options.light {
        settings.light = false;
        settings.ao_policy = AoPolicy::Off;
    }
    settings
}

/// What the summary says about the run before it starts: the options, the present mode, the
/// renderer capacity asked for and the machine.
fn run_info(options: &Options, present: String) -> RunInfo {
    let (arena_quads, chunk_slots) = options.render_capacity();
    let vsync = if present == PresentModePref::Vsync.as_str() {
        "on"
    } else {
        "off"
    };
    RunInfo {
        scenario: options.scenario.name().to_string(),
        label: options.label.clone(),
        radius: options.radius,
        speed: if options.scenario.moves() {
            options.speed
        } else {
            0.0
        },
        seed: options.seed,
        world: options.world.label(),
        trees: options.world.has_trees(),
        light: if options.light { "on" } else { "off" }.to_string(),
        pack: match &options.textures {
            TextureSource::Procedural => "none".to_string(),
            source => source.label(),
        },
        arena_quads,
        chunk_slots,
        vertical: if options.fixed_height {
            "fixed".to_string()
        } else {
            options.vertical_radius.to_string()
        },
        duration_s: options.duration.as_secs_f64(),
        warmup_s: options.warmup.as_secs_f64(),
        present,
        fps_limit: options.fps_limit,
        window: options.size,
        build: if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
        .to_string(),
        cpu: cpu_name(),
        threads: std::thread::available_parallelism().map_or(0, usize::from),
        time: clock::start_time(options.time, true).clock_text(),
        bodies: options.bodies.label(),
        vsync: vsync.to_string(),
        ..RunInfo::default()
    }
}

/// Adds the bench resources and systems (everything but the engine plugins).
fn add_bench(app: &mut App, options: Options, info: RunInfo, recorder: Recorder, started: Instant) {
    app.insert_resource(recorder)
        .insert_resource(Bench {
            started,
            phase: Phase::Loading { streak: 0 },
            info,
            presented_at_start: 0,
            exiting: false,
            options,
        })
        .add_systems(Startup, spawn_camera)
        .add_systems(PreUpdate, record)
        .add_systems(
            Update,
            (gpu_failed, drive, scripted_edits)
                .chain()
                .before(WorldSystems::Interest),
        );
}

/// Kills the process if `main` has not returned `limit` after start (the event loop stopped
/// updating, so the in-app timeout cannot fire). Dropping the returned sender disarms it.
fn spawn_watchdog(limit: Duration) -> Sender<()> {
    let (disarm, armed) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        if armed.recv_timeout(limit) == Err(RecvTimeoutError::Timeout) {
            eprintln!("stress: watchdog: still running after {limit:?}, exiting with 4");
            std::process::exit(4);
        }
    });
    disarm
}

fn main() -> AppExit {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut gpu, gpu_config, rest) = match GpuPreferences::from_args(&args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("stress: {e}\n\n{}\n\n{CLI_HELP}", usage());
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
            eprintln!("stress: {message}\n\n{}\n\n{CLI_HELP}", usage());
            return AppExit::from_code(1);
        }
    };
    if cfg!(debug_assertions) {
        eprintln!("stress: debug build: the numbers are not comparable, use --release");
    }
    let times = if options.spans {
        if !cfg!(feature = "trace") {
            eprintln!(
                "stress: --spans needs the system spans: build with --features trace\n\n{}",
                usage()
            );
            return AppExit::from_code(1);
        }
        // Installed before the app: LogPlugin then keeps this subscriber.
        let (layer, times) = span_times::span_times(span_times::MAIN_SCHEDULE);
        if let Err(e) = boxel::log::init_logging_with_layer(boxel::log::DEFAULT_FILTER, layer) {
            eprintln!("stress: --spans: {e}");
            return AppExit::from_code(1);
        }
        Some(times)
    } else {
        None
    };
    let _watchdog = spawn_watchdog(options.timeout.saturating_add(WATCHDOG_GRACE));

    if let Some(on) = options.vsync {
        if gpu.present_mode.is_some() {
            eprintln!("stress: give --vsync or --present, not both\n\n{}", usage());
            return AppExit::from_code(1);
        }
        gpu.present_mode = Some(PresentModePref::from_vsync(on));
    }
    // Uncapped: every update renders (offscreen while the window is covered or between
    // compositor frame callbacks) and presents without waiting for vblank.
    let (present, set) = present_mode(
        gpu.present_mode,
        GpuPreferences::from_boxel_env().0.present_mode,
    );
    if set.is_some() {
        gpu.present_mode = set;
    }
    let generated = Arc::new(AtomicU64::new(0));
    let jobs_done = Arc::new(AtomicU64::new(0));
    let inner = options.world.generator(options.seed);
    let fixed_bounds = options.fixed_height.then(|| inner.vertical_chunks());
    let (arena_quads, chunk_slots) = options.render_capacity();
    let info = run_info(&options, present);
    let mut plugins = DefaultPlugins
        .set(WindowPlugin {
            primary: WindowSettings {
                title: format!("boxel stress ({})", options.scenario.name()),
                width: options.size.0,
                height: options.size.1,
                ..WindowSettings::default()
            },
            ..WindowPlugin::default()
        })
        .set(RenderPlugin {
            settings: RenderSettings {
                app_name: "stress".to_string(),
                gpu,
                gpu_config,
                render_when_hidden: true,
                arena_quads,
                max_chunk_slots: chunk_slots,
                textures: options.textures.clone(),
                ..RenderSettings::default()
            },
        })
        .set(WorldPlugin {
            generator: Arc::new(CountingGenerator {
                inner,
                generated: Arc::clone(&generated),
            }),
            executor: Arc::new(CountingExecutor {
                done: Arc::clone(&jobs_done),
            }),
            settings: world_settings(&options, fixed_bounds),
            ..WorldPlugin::default()
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

    let fps_limit = FrameLimit::new(options.fps_limit);
    let mut app = App::new();
    app.add_plugins(plugins);
    // The day and night clock stays still: every run sees the same light.
    app.insert_resource(clock::start_time(options.time, true));
    if options.edits {
        let [x, y, z, ..] = options.camera;
        app.insert_resource(EditScript::new(DVec3::new(x, y, z)));
    }
    if options.bodies.enabled() {
        let [x, y, z, ..] = options.camera;
        app.add_plugins(BodiesPlugin {
            options: options.bodies,
            center: DVec3::new(x, y, z),
        });
    }
    add_bench(
        &mut app,
        options,
        info,
        Recorder::new(generated, jobs_done, chunk_slots as usize),
        Instant::now(),
    );
    if let Some(times) = times {
        app.insert_resource(SpanRecorder::new(times));
    }
    // The bench measures the uncapped loop unless --fps-limit says otherwise (the engine default
    // is 144 fps).
    app.insert_resource(fps_limit);
    app.run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel::mesh::{ChunkMesh, PackedQuad};

    fn parse(args: &[&str]) -> Result<Option<Options>, String> {
        let args: Vec<String> = args.iter().map(ToString::to_string).collect();
        parse_options(&args)
    }

    #[test]
    fn defaults() {
        let o = parse(&[]).expect("valid").expect("not help");
        assert_eq!(o.scenario, Scenario::Static);
        assert_eq!(o.radius, 16);
        assert_eq!(o.speed, 32.0);
        assert_eq!(o.duration, Duration::from_secs(20));
        assert_eq!(o.warmup, Duration::from_secs(2));
        assert_eq!(o.timeout, Duration::from_secs(142));
        assert_eq!(o.world, WorldChoice::default());
        assert_eq!(o.world.label(), "forest");
        assert_eq!(o.render_capacity(), (8 << 20, 32_768));
        assert_eq!(o.fps_limit, 0, "unlimited");
        assert_eq!((o.csv, o.summary), (None, None));
        assert!(!o.bodies.enabled(), "no physics unless asked");
        assert!(o.light, "light on unless asked");
    }

    #[test]
    fn light_off_turns_off_the_light_and_the_ao() {
        let on = options(&[]);
        let settings = world_settings(&on, None);
        assert!(settings.light);
        assert_eq!(settings.ao_policy, WorldSettings::default().ao_policy);
        assert_eq!(run_info(&on, String::new()).light, "on");

        let off = options(&["--light", "off", "--fixed-height"]);
        assert!(!off.light);
        let settings = world_settings(&off, Some(0..=5));
        assert!(!settings.light);
        assert_eq!(
            settings.ao_policy,
            AoPolicy::Off,
            "the world before light had no AO"
        );
        assert_eq!(settings.vertical_bounds, Some(0..=5));
        let info = run_info(&off, "vsync".to_string());
        assert_eq!(
            (info.light.as_str(), info.present.as_str()),
            ("off", "vsync")
        );
        assert_eq!(info.vertical, "fixed");
        assert!(report::run_title(&info).ends_with(" light off vsync"));

        assert!(options(&["--light=on"]).light);
        assert!(usage().contains("--light on|off"));
    }

    #[test]
    fn body_flags() {
        let o = parse(&[
            "--bodies=10000",
            "--body-kind",
            "item",
            "--projectiles",
            "200",
        ])
        .expect("valid")
        .expect("not help");
        assert_eq!(o.bodies.label(), "10000 item, 200 projectiles");
        assert!(parse(&["--body-kind", "ghost"]).is_err());
        assert!(usage().contains("--projectiles M"));
    }

    #[test]
    fn flags_and_inline_values() {
        let o = parse(&[
            "--scenario=fly",
            "--radius",
            "32",
            "--speed",
            "64",
            "--duration",
            "1.5",
            "--warmup",
            "0",
            "--csv",
            "a.csv",
            "--summary=b.md",
            "--label",
            "abc",
            "--camera",
            "1,2,3,4,5",
            "--size",
            "800x600",
            "--async-threads",
            "4",
            "--no-trees",
            "--seed",
            "9",
            "--fps-limit",
            "144",
        ])
        .expect("valid")
        .expect("not help");
        assert_eq!(o.scenario, Scenario::Fly);
        assert_eq!((o.radius, o.speed), (32, 64.0));
        assert_eq!(o.duration, Duration::from_millis(1500));
        assert_eq!(o.warmup, Duration::ZERO);
        assert_eq!(o.timeout, SETTLE_ALLOWANCE + Duration::from_millis(1500));
        assert_eq!(o.csv.as_deref(), Some(Path::new("a.csv")));
        assert_eq!(o.summary.as_deref(), Some(Path::new("b.md")));
        assert_eq!(o.label, "abc");
        assert_eq!(o.camera, [1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(o.size, (800, 600));
        assert_eq!(o.async_threads, Some(4));
        assert!(!o.world.trees);
        assert_eq!(o.world.label(), "terrain");
        assert_eq!(o.seed, 9);
        assert_eq!(o.fps_limit, 144);
        let o = parse(&["--fps-limit=0"]).expect("valid").expect("not help");
        assert_eq!(FrameLimit::new(o.fps_limit), FrameLimit::UNLIMITED);
        assert_eq!(o.vsync, None);
        let o = parse(&["--vsync", "on"]).expect("valid").expect("not help");
        assert_eq!(o.vsync, Some(true));
        let o = parse(&["--vsync=off"]).expect("valid").expect("not help");
        assert_eq!(o.vsync, Some(false));
        assert!(parse(&["--vsync", "maybe"]).is_err());
        assert_eq!(run_info(&o, "vsync".to_string()).vsync, "on");
        assert_eq!(run_info(&o, "novsync".to_string()).vsync, "off");
        let o = parse(&["--timeout", "30", "--scenario", "spin"])
            .expect("valid")
            .expect("not help");
        assert_eq!(
            (o.timeout, o.scenario),
            (Duration::from_secs(30), Scenario::Spin)
        );
    }

    #[test]
    fn span_flags() {
        let o = parse(&["--spans"]).expect("valid").expect("not help");
        assert!(o.spans);
        assert_eq!(o.spans_csv, None);
        let o = parse(&["--spans-csv", "s.csv"])
            .expect("valid")
            .expect("not help");
        assert!(o.spans, "--spans-csv implies --spans");
        assert_eq!(o.spans_csv.as_deref(), Some(Path::new("s.csv")));
        assert!(!parse(&[]).expect("valid").expect("not help").spans);
    }

    #[test]
    fn edit_and_time_flags() {
        let o = parse(&[]).expect("valid").expect("not help");
        assert_eq!((o.edits, o.time), (false, 12.0));
        let o = parse(&["--edits", "--time", "21:30"])
            .expect("valid")
            .expect("not help");
        assert_eq!((o.edits, o.time), (true, 21.5));
        assert!(parse(&["--time", "7:75"]).is_err());
        assert_eq!(clock::start_time(o.time, true).clock_text(), "21:30");
    }

    #[test]
    fn bad_input_is_rejected() {
        for args in [
            &["--scenario", "walk"][..],
            &["--radius", "0"],
            &["--radius", "65"],
            &["--speed", "-1"],
            &["--speed", "inf"],
            &["--duration", "0"],
            &["--duration"],
            &["--camera", "1,2,3"],
            &["--camera", "3e18,80,0,0,0"],
            &["--camera", "0,0,-1e300,0,0"],
            &["--size", "0x10"],
            &["--async-threads", "0"],
            &["--fly"],
            &["--arena-quads", "100"],
            &["--chunk-slots", "0"],
            &["--chunk-slots", "2000000"],
            &["--fps-limit", "-1"],
            &["--fps-limit", "100001"],
            &["--fps-limit"],
            &["--world", "overworld", "--fixed-height"],
            &["--caves", "mc"],
            &["--light", "yes"],
            &["--light"],
        ] {
            assert!(parse(args).is_err(), "{args:?} was accepted");
        }
        assert_eq!(parse(&["--help"]), Ok(None));
    }

    #[test]
    fn fly_moves_along_the_yaw_direction() {
        let start = [0.0, 140.0, 0.0, -90.0, -20.0];
        let t = camera_at(Scenario::Fly, start, 32.0, 20.0, 2.0);
        assert_eq!(
            t.translation,
            DVec3::new(64.0, 140.0, 0.0),
            "no drift off the axis"
        );
        // Looking where it flies (pitch aside).
        let forward = t.forward();
        assert!(
            forward.x > 0.9 && forward.y < 0.0 && forward.z.abs() < 1e-6,
            "{forward:?}"
        );
        // Yaw 0 looks along -Z.
        let t = camera_at(Scenario::Fly, [0.0, 0.0, 0.0, 0.0, 0.0], 10.0, 1.0, 1.0);
        assert!((t.translation - DVec3::new(0.0, 0.0, -10.0)).length() < 1e-9);
        // Crossing a chunk border: x = 31.9 -> 32.1, and only in x.
        let a = camera_at(Scenario::Fly, start, 32.0, 20.0, 31.9 / 32.0);
        let b = camera_at(Scenario::Fly, start, 32.0, 20.0, 32.1 / 32.0);
        let (ca, cb) = (chunk_of(&a), chunk_of(&b));
        assert_eq!((cb.x - ca.x, cb.y - ca.y, cb.z - ca.z), (1, 0, 0));
        // Past the former i32 edge (2^31 voxels): the crossing is still one chunk in x.
        let edge = [
            2_147_483_647.0 - 31.9,
            140.0,
            -2_147_483_648.0 - 16.0,
            -90.0,
            -20.0,
        ];
        let a = camera_at(Scenario::Fly, edge, 32.0, 20.0, 31.0 / 32.0);
        let b = camera_at(Scenario::Fly, edge, 32.0, 20.0, 33.0 / 32.0);
        let (ca, cb) = (chunk_of(&a), chunk_of(&b));
        assert_eq!((ca.x, cb.x, ca.z), ((1 << 26) - 1, 1 << 26, -(1 << 26) - 1));
        // Far cameras parse.
        let far = parse(&["--camera", "3e9,80,-1e12,0,0"])
            .expect("valid")
            .expect("not help");
        assert_eq!(far.camera[..3], [3e9, 80.0, -1e12]);
        // The default start is the middle of a chunk.
        let o = parse(&[]).expect("valid").expect("not help");
        let begin = camera_at(Scenario::Fly, o.camera, 32.0, 20.0, 0.0);
        let local = begin.translation.rem_euclid(DVec3::splat(32.0));
        assert_eq!((local.x, local.z), (16.0, 16.0));
    }

    #[test]
    fn worlds_set_the_render_capacity_and_flags_override_it() {
        let o = parse(&[
            "--world",
            "overworld",
            "--scenario",
            "dive",
            "--speed",
            "64",
        ])
        .expect("valid")
        .expect("not help");
        assert_eq!((o.scenario, o.speed), (Scenario::Dive, 64.0));
        assert_eq!(o.world.label(), "overworld/mc");
        assert_eq!(o.render_capacity(), (24 << 20, 65_536));
        let o = parse(&[
            "--world=overworld",
            "--caves=closed",
            "--arena-quads",
            "1048576",
            "--chunk-slots=4096",
        ])
        .expect("valid")
        .expect("not help");
        assert_eq!(o.world.label(), "overworld/closed");
        assert_eq!(o.render_capacity(), (1 << 20, 4096));
        // The forest keeps --fixed-height (it has a floor).
        assert!(parse(&["--fixed-height"]).expect("valid").is_some());
    }

    #[test]
    fn dive_moves_straight_down() {
        let start = [16.0, 140.0, 16.0, -90.0, -20.0];
        let t = camera_at(Scenario::Dive, start, 64.0, 20.0, 2.5);
        assert_eq!(t.translation, DVec3::new(16.0, -20.0, 16.0));
        let begin = camera_at(Scenario::Dive, start, 64.0, 20.0, 0.0);
        assert!((t.forward() - begin.forward()).length() < 1e-6, "same view");
        // One layer of chunks every half second at 64 voxels/s.
        let a = chunk_of(&camera_at(Scenario::Dive, start, 64.0, 20.0, 0.1));
        let b = chunk_of(&camera_at(Scenario::Dive, start, 64.0, 20.0, 0.6));
        assert_eq!((b.x - a.x, b.y - a.y, b.z - a.z), (0, -1, 0));
    }

    #[test]
    fn spin_turns_once_and_static_stays() {
        let start = [5.0, 140.0, 7.0, -90.0, -20.0];
        let begin = camera_at(Scenario::Spin, start, 32.0, 20.0, 0.0);
        let half = camera_at(Scenario::Spin, start, 32.0, 20.0, 10.0);
        let end = camera_at(Scenario::Spin, start, 32.0, 20.0, 20.0);
        let after = camera_at(Scenario::Spin, start, 32.0, 20.0, 30.0);
        assert_eq!(begin.translation, half.translation);
        assert!(
            (half.forward().x + begin.forward().x).abs() < 1e-5,
            "half a turn"
        );
        assert!(
            (end.forward() - begin.forward()).length() < 1e-5,
            "a full turn"
        );
        assert_eq!(after, end, "never more than one turn");
        let still = camera_at(Scenario::Static, start, 32.0, 20.0, 15.0);
        assert_eq!(still, begin);
    }

    #[test]
    fn counting_generator_counts_and_forwards() {
        let generated = Arc::new(AtomicU64::new(0));
        let inner: Arc<dyn WorldGenerator> = Arc::new(TerrainGenerator::default());
        let counting = CountingGenerator {
            inner: Arc::clone(&inner),
            generated: Arc::clone(&generated),
        };
        let registry = BlockRegistry::default_blocks();
        let key = ChunkKey::new(ChunkPos::new(0, 1, 0));
        let data = counting.generate(key, &registry);
        assert!(data == inner.generate(key, &registry));
        assert_eq!(generated.load(Ordering::Relaxed), 1);
        assert_eq!(counting.vertical_chunks(), inner.vertical_chunks());
        assert_eq!(counting.name(), inner.name());
    }

    #[test]
    fn counting_executor_counts_finished_jobs() {
        let done = Arc::new(AtomicU64::new(0));
        let executor = CountingExecutor {
            done: Arc::clone(&done),
        };
        let (tx, rx) = mpsc::channel();
        for i in 0..4 {
            let tx = tx.clone();
            executor.spawn(Box::new(move || tx.send(i).expect("receiver alive")));
        }
        for _ in 0..4 {
            rx.recv_timeout(Duration::from_secs(30)).expect("job ran");
        }
        let deadline = Instant::now() + Duration::from_secs(30);
        while done.load(Ordering::Relaxed) < 4 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(done.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn scenario_names_round_trip() {
        for s in Scenario::ALL {
            assert_eq!(Scenario::parse(s.name()), Some(s));
        }
        assert_eq!(Scenario::parse("Fly"), None);
    }

    #[test]
    fn present_mode_is_the_requested_one_or_immediate() {
        use PresentModePref::{Immediate, Mailbox, Vsync};
        assert_eq!(
            present_mode(None, None),
            ("immediate".into(), Some(Immediate))
        );
        assert_eq!(present_mode(Some(Vsync), None), ("vsync".into(), None));
        // BOXEL_PRESENT (already parsed: invalid values are None) applies without --present.
        assert_eq!(present_mode(None, Some(Mailbox)), ("mailbox".into(), None));
        assert_eq!(
            present_mode(Some(Immediate), Some(Vsync)),
            ("immediate".into(), None)
        );
    }

    // -----------------------------------------------------------------------------------------
    // The bench systems in an app: a hand-driven clock, a scripted world and renderer.

    /// What the fake world and renderer do in one frame.
    #[derive(Clone, Debug, Default)]
    struct FakeFrame {
        /// Time until the next frame starts.
        duration: Duration,
        /// `WorldStats` published at the end of the frame (`Last`, as the world plugin).
        stats: WorldStats,
        /// Quads of each mesh sent in `Update` (always the same chunk).
        upserts: Vec<usize>,
        /// Meshes removed in `Update`.
        removes: usize,
        /// Generator calls that finish during the frame.
        generated: u64,
        /// The renderer presents this frame.
        presented: bool,
    }

    #[derive(Resource)]
    struct Script {
        frame: usize,
        frames: Vec<FakeFrame>,
        generated: Arc<AtomicU64>,
    }

    impl Script {
        fn now(&self) -> Option<&FakeFrame> {
            self.frames.get(self.frame)
        }
    }

    fn fake_world(script: Res<Script>, mut meshes: MessageWriter<ChunkMeshEvent>) {
        let Some(frame) = script.now() else {
            return;
        };
        let (id, key) = (ChunkId::default(), ChunkKey::new(ChunkPos::new(0, 0, 0)));
        for &quads in &frame.upserts {
            let mesh = ChunkMesh {
                quads: vec![PackedQuad::default(); quads],
                ..ChunkMesh::default()
            };
            meshes.write(ChunkMeshEvent::Upsert {
                id,
                key,
                mesh: Arc::new(mesh),
            });
        }
        for _ in 0..frame.removes {
            meshes.write(ChunkMeshEvent::Remove { id, key });
        }
        script
            .generated
            .fetch_add(frame.generated, Ordering::Relaxed);
    }

    fn fake_render(
        script: Res<Script>,
        mut stats: ResMut<WorldStats>,
        mut info: ResMut<RenderInfo>,
    ) {
        let Some(frame) = script.now() else {
            return;
        };
        *stats = frame.stats;
        info.ready = true;
        info.frames_presented += u64::from(frame.presented);
        // A device that cuts the arena and the chunk table.
        info.arena_capacity_quads = 1 << 20;
        info.chunk_slot_capacity = 4096;
    }

    /// The bench systems with a fake world and renderer; no window, GPU or threads.
    fn bench_app(options: Options, t0: Instant, frames: Vec<FakeFrame>) -> App {
        let generated = Arc::new(AtomicU64::new(0));
        let mut app = App::new();
        app.add_message::<ChunkMeshEvent>()
            .add_message::<GpuFailed>()
            .init_resource::<WorldStats>()
            .init_resource::<RenderInfo>()
            .insert_resource(Time::new(t0))
            .insert_resource(Script {
                frame: 0,
                frames,
                generated: Arc::clone(&generated),
            });
        let recorder = Recorder::new(generated, Arc::new(AtomicU64::new(0)), 16);
        let info = run_info(&options, "immediate".to_string());
        add_bench(&mut app, options, info, recorder, t0);
        app.add_systems(Update, fake_world.after(drive))
            .add_systems(Last, fake_render);
        app.finish();
        app.cleanup();
        app
    }

    /// Runs the script (frame `i` starts when frame `i - 1` ends) until the app asks to exit:
    /// the exit and the frame that asked, or `None` after the last frame.
    fn run(app: &mut App, t0: Instant) -> Option<(AppExit, usize)> {
        let mut now = t0;
        let frames = app.world().resource::<Script>().frames.len();
        for i in 0..frames {
            app.world_mut().resource_mut::<Script>().frame = i;
            app.world_mut()
                .resource_mut::<Time>()
                .update_with_instant(now);
            app.update();
            if let Some(exit) = app.should_exit() {
                return Some((exit, i));
            }
            now += app.world().resource::<Script>().frames[i].duration;
        }
        None
    }

    fn options(args: &[&str]) -> Options {
        parse(args).expect("valid").expect("not help")
    }

    const BUSY: WorldStats = WorldStats {
        chunks: 10,
        data_ready: 0,
        generating: 0,
        mesh_ready: 0,
        mesh_empty: 0,
        meshing: 0,
        queued_jobs: 5,
        in_flight: 0,
        interest_backlog: 0,
        light_full: 0,
        light_fast: 0,
        light_jobs: 0,
        sky_columns: 0,
        sky_walks: 0,
        sky_private_generations: 0,
        waiting_sky: 0,
        edit_batches_held: 0,
        edit_batches_split: 0,
        light_fanout: 0,
    };
    const SETTLED: WorldStats = WorldStats {
        queued_jobs: 0,
        ..BUSY
    };

    /// `count` frames of `ms` milliseconds with the world in `stats`.
    fn frames(count: usize, ms: u64, stats: WorldStats) -> Vec<FakeFrame> {
        vec![
            FakeFrame {
                duration: Duration::from_millis(ms),
                stats,
                ..FakeFrame::default()
            };
            count
        ]
    }

    #[test]
    fn phases_advance_and_each_record_holds_its_own_frame() {
        // Frame i lasts i + 1 ms, so it starts at i (i + 1) / 2 ms. The world is busy in frames
        // 0 and 1 and settled from frame 2; drive sees a frame's stats in the next frame, so the
        // third settled frame it sees is frame 4, at frame 5 (15 ms): warmup until 25 ms, the
        // measurement starts at frame 7 (28 ms) and its 30 ms end at frame 11 (66 ms, t = 38 ms).
        // Frames 7 to 10 are measured; catch-up needs 3 more settled frames: done at frame 14.
        let mut script: Vec<FakeFrame> = (0..20u64)
            .map(|i| FakeFrame {
                duration: Duration::from_millis(i + 1),
                stats: if i < 2 { BUSY } else { SETTLED },
                ..FakeFrame::default()
            })
            .collect();
        script[2].upserts = vec![6]; // before the measurement: resident, not recorded
        script[6].presented = true; // warmup: not counted
        script[7].upserts = vec![10];
        script[7].generated = 2;
        script[7].stats.data_ready = 3;
        script[8].generated = 3;
        script[8].presented = true;
        script[8].stats.data_ready = 5;
        script[9].upserts = vec![4];
        script[9].stats = WorldStats {
            chunks: 12,
            data_ready: 5,
            queued_jobs: 4,
            ..SETTLED
        };
        script[10].removes = 1;
        script[10].presented = true;
        for frame in &mut script[10..] {
            frame.stats = WorldStats {
                chunks: 12,
                data_ready: 5,
                ..SETTLED
            };
        }
        // Flying at 1000 voxels/s from x = 16 crosses x = 32 between t = 8 and 17 ms.
        let o = options(&[
            "--scenario=fly",
            "--speed=1000",
            "--duration=0.030",
            "--warmup=0.010",
            "--radius=1",
        ]);
        let t0 = Instant::now();
        let mut app = bench_app(o, t0, script);
        assert_eq!(run(&mut app, t0), Some((AppExit::Success, 14)));

        let rec = app.world().resource::<Recorder>();
        let records = rec.log.to_vec();
        let ms = Duration::from_millis;
        let t: Vec<Duration> = records.iter().map(|r| r.t).collect();
        assert_eq!(t, [ms(0), ms(8), ms(17), ms(27)]);
        let frame_times: Vec<Duration> = records.iter().map(|r| r.frame_time).collect();
        assert_eq!(
            frame_times,
            [ms(8), ms(9), ms(10), ms(11)],
            "frames 7 to 10"
        );
        let field = |f: fn(&FrameRecord) -> u64| records.iter().map(f).collect::<Vec<_>>();
        assert_eq!(field(|r| u64::from(r.presented)), [0, 1, 0, 1]);
        assert_eq!(field(|r| u64::from(r.crossed)), [0, 0, 1, 0]);
        assert_eq!(field(|r| u64::from(r.generated)), [2, 3, 0, 0]);
        assert_eq!(field(|r| u64::from(r.meshes)), [1, 0, 1, 0]);
        assert_eq!(field(|r| u64::from(r.removed)), [0, 0, 0, 1]);
        assert_eq!(field(|r| r.upload_quads), [10, 0, 4, 0]);
        // The upsert of frame 2 (6 quads) was replaced by frame 7's (10), then by 4, then removed.
        assert_eq!(field(|r| r.quads), [10, 10, 4, 0]);
        assert_eq!(field(|r| r.d_data_ready as u64), [3, 2, 0, 0]);
        assert_eq!(field(|r| r.d_chunks as u64), [0, 0, 2, 0]);
        assert_eq!(field(|r| r.world.queued_jobs as u64), [0, 0, 4, 0]);
        // Consecutive records are exactly one frame apart.
        assert!(
            records
                .windows(2)
                .all(|w| w[1].t - w[0].t == w[0].frame_time)
        );

        let bench = app.world().resource::<Bench>();
        assert_eq!(bench.phase, Phase::Done);
        assert_eq!(bench.info.settle_s, 0.015);
        assert_eq!(bench.info.settle_chunks, 10);
        assert_eq!(bench.info.frames_presented, 2, "frames 8 and 10");
        assert_eq!(
            bench.info.catch_up_s,
            Some(0.039),
            "frame 11 (66 ms) to 14 (105 ms)"
        );
        // The summary keeps what the renderer got, not the 8 Mi quads and 32 768 slots asked.
        assert_eq!(
            (bench.info.arena_quads, bench.info.chunk_slots),
            (1 << 20, 4096)
        );
    }

    #[test]
    fn a_world_that_never_settles_times_out_with_4() {
        let o = options(&["--timeout=0.020"]);
        let t0 = Instant::now();
        let mut app = bench_app(o, t0, frames(20, 5, BUSY));
        // 25 ms > 20 ms at frame 5.
        assert_eq!(run(&mut app, t0), Some((AppExit::from_code(4), 5)));
        assert!(app.world().resource::<Recorder>().log.to_vec().is_empty());
    }

    #[test]
    fn a_measurement_longer_than_the_timeout_exits_with_4() {
        let o = options(&["--duration=10", "--warmup=0", "--timeout=0.050"]);
        let t0 = Instant::now();
        let mut app = bench_app(o, t0, frames(40, 5, SETTLED));
        // Settled at frame 3 (third settled frame seen), measuring from frame 4, 55 ms > 50 ms
        // at frame 11.
        assert_eq!(run(&mut app, t0), Some((AppExit::from_code(4), 11)));
        let bench = app.world().resource::<Bench>();
        assert_eq!(bench.phase, Phase::Done);
        let records = app.world().resource::<Recorder>().log.to_vec();
        assert_eq!(records.len(), 7, "frames 4 to 10, and no outputs written");
    }

    #[test]
    fn a_catch_up_past_the_timeout_still_succeeds() {
        // The measurement (frames 4 and 5) is complete; the world never settles again.
        let o = options(&["--duration=0.010", "--warmup=0", "--timeout=0.100"]);
        let mut script = frames(5, 5, SETTLED);
        script.extend(frames(40, 5, BUSY));
        let t0 = Instant::now();
        let mut app = bench_app(o, t0, script);
        assert_eq!(run(&mut app, t0), Some((AppExit::Success, 21)));
        let bench = app.world().resource::<Bench>();
        assert_eq!(bench.info.catch_up_s, None);
        assert_eq!(app.world().resource::<Recorder>().log.to_vec().len(), 2);
    }

    #[test]
    fn the_summary_records_the_light() {
        let dir = std::env::temp_dir().join(format!("boxel-stress-light-{}", std::process::id()));
        let path = dir.join("off.md");
        let o = options(&[
            "--light=off",
            "--duration=0.010",
            "--warmup=0",
            "--summary",
            path.to_str().expect("a UTF-8 temp dir"),
        ]);
        let t0 = Instant::now();
        let mut app = bench_app(o, t0, frames(20, 5, SETTLED));
        assert_eq!(
            run(&mut app, t0).map(|(exit, _)| exit),
            Some(AppExit::Success)
        );
        let text = std::fs::read_to_string(&path).expect("summary written");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            text.starts_with("# boxel stress: static r16 light off\n"),
            "{text}"
        );
        assert!(text.contains(", trees on, light off, radius 16,"), "{text}");
        assert!(text.contains("\"light\": \"off\""), "{text}");
        assert!(text.contains("\"pack\": \"none\""), "{text}");
        assert!(text.contains("chunk slots, textures procedural."), "{text}");
    }

    #[test]
    fn no_gpu_exits_with_2() {
        let t0 = Instant::now();
        let mut app = bench_app(options(&[]), t0, frames(5, 5, BUSY));
        app.world_mut().write_message(GpuFailed {
            error: "no adapter".to_string(),
        });
        assert_eq!(run(&mut app, t0), Some((AppExit::from_code(2), 0)));
    }
}
