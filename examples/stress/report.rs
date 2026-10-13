//! Per-frame records of a stress run and what is written from them: the CSV (one row per frame)
//! and the summary (Markdown tables plus a JSON block that `tools/bench_report.py` reads).

use boxel::diagnostic::FrameTimeSummary;
use boxel::world::WorldStats;
use std::fmt::Write as _;
use std::time::Duration;

/// Frame budget at 144 Hz (6.94 ms).
pub const BUDGET_144HZ: Duration = Duration::from_nanos(1_000_000_000 / 144);
/// Frame budget at 60 Hz (16.7 ms).
pub const BUDGET_60HZ: Duration = Duration::from_nanos(1_000_000_000 / 60);
/// A frame this long at a 144 fps cap is a visible hitch, not timer noise (7.5 ms).
pub const LATE_144HZ: Duration = Duration::from_micros(7500);
/// Worst frames listed in the summary.
pub const WORST_FRAMES: usize = 5;
/// Version of the JSON layout (bump on incompatible changes).
pub const FORMAT_VERSION: u32 = 1;

/// One measured frame: the work the frame did and how long it took.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameRecord {
    /// Time since the measurement started, when the frame's `Update` ran.
    pub t: Duration,
    /// Wall-clock time of the whole frame (`Time::real_delta` measured by the next update).
    pub frame_time: Duration,
    /// The frame was presented (otherwise it was rendered offscreen only).
    pub presented: bool,
    /// The camera entered another chunk in this frame.
    pub crossed: bool,
    /// Camera position (x, z) and yaw in degrees.
    pub camera: (f64, f64, f32),
    /// Chunks generated (generator calls that finished) during the frame.
    pub generated: u32,
    /// Jobs (generate and mesh) that finished during the frame.
    pub jobs_done: u32,
    /// `ChunkMeshEvent::Upsert` messages (meshes sent to the renderer).
    pub meshes: u32,
    /// `ChunkMeshEvent::Remove` messages.
    pub removed: u32,
    /// Quads in this frame's upserts.
    pub upload_quads: u64,
    /// Change of `WorldStats::data_ready` over the frame (net: unloads subtract).
    pub d_data_ready: i64,
    /// Change of `WorldStats::chunks` (slots) over the frame.
    pub d_chunks: i64,
    /// World counters after the frame.
    pub world: WorldStats,
    /// Chunks resident on the GPU (`RenderInfo::chunks`).
    pub gpu_chunks: u32,
    /// Quads of the meshes sent and not removed (what the renderer holds unless it dropped
    /// chunks).
    pub quads: u64,
    /// Draw calls (`RenderInfo::draws`).
    pub draws: u32,
    /// Renderer arena entries in use, quads plus light palettes
    /// (`RenderInfo::arena_used_quads`).
    pub arena_used: u32,
    /// Chunks the renderer could not hold so far (`RenderInfo::dropped_chunks`, cumulative).
    pub dropped_chunks: u32,
    /// How long the runner held the frame past its deadline for the presentation gate
    /// (`FramePacingInfo::gate_wait`).
    pub gate_wait: Duration,
    /// The runner stopped waiting with the gate closed (`FramePacingInfo::gate_missed`).
    pub gate_missed: bool,
    /// When the presentation gate last opened before the frame (the compositor's frame
    /// callback on Wayland), since the measurement started (`None`: before it).
    pub gate_at: Option<Duration>,
    /// The runner paced the frame by the display (`FramePacingInfo::display_locked`).
    pub display_locked: bool,
    /// Chunks that passed the CPU cull (`RenderInfo::visible_chunks`; Mid and Low only).
    pub visible_chunks: u32,
    /// Resident chunks outside the cave culling set (`RenderInfo::occluded_chunks`): behind
    /// closed rock or outside the frustum.
    pub occluded: u32,
    /// How long the cave culling search took, when one ran in the frame
    /// (`VisibilityStats::last_search`).
    pub search: Option<Duration>,
    /// Cave culling was on but culled nothing because the eye did not see from open space
    /// (inside rock, or nearer a wall than the near plane; `VisibilityStats::blocked`).
    pub unculled: bool,
    /// Time the renderer took to update its cave culling bits (`RenderInfo::occlusion_time`).
    pub occlusion: Duration,
    /// Time in the idle steps the runner ran after the frame, before the next one
    /// (`IdleInfo::tail`).
    pub idle: Duration,
    /// Those idle steps (`IdleInfo::steps`).
    pub idle_steps: u32,
    /// How far past the end of its slot one of them returned, at worst (`IdleInfo::late`).
    pub idle_late: Duration,
    /// How many of them returned past the end of their slot.
    pub idle_overruns: u64,
    /// When the latest chunk job had finished by the time the record was taken, since the
    /// measurement started (`None`: no job yet since then).
    pub last_job_end: Option<Duration>,
    /// Far LOD draw calls (`RenderInfo::lod_draws`; 0 without `--lod`).
    pub lod_draws: u32,
    /// Quads of those draws (`RenderInfo::lod_quads`).
    pub lod_quads: u64,
    /// Quads copied into the LOD arena during the frame (8 bytes each).
    pub lod_upload_quads: u64,
    /// LOD arena quads in use (`RenderInfo::lod_arena_used_quads`).
    pub lod_arena_used: u32,
    /// LOD column jobs running (`LodStats::in_flight`).
    pub lod_in_flight: u32,
    /// LOD column jobs waiting (`LodStats::queued_jobs`).
    pub lod_queued: u32,
}

/// Append-only list of records stored in fixed blocks: pushing never moves earlier records, so
/// recording has no reallocation spikes of its own.
#[derive(Debug, Default)]
pub struct RecordLog {
    blocks: Vec<Vec<FrameRecord>>,
}

impl RecordLog {
    /// Records per block (about 0.7 MiB).
    pub const BLOCK: usize = 4096;

    /// Adds a record.
    pub fn push(&mut self, record: FrameRecord) {
        if self.blocks.last().is_none_or(|b| b.len() == Self::BLOCK) {
            self.blocks.push(Vec::with_capacity(Self::BLOCK));
        }
        if let Some(block) = self.blocks.last_mut() {
            block.push(record);
        }
    }

    /// In order.
    pub fn iter(&self) -> impl Iterator<Item = &FrameRecord> {
        self.blocks.iter().flatten()
    }

    /// The records as one vector (for the summary, after the measurement).
    pub fn to_vec(&self) -> Vec<FrameRecord> {
        self.iter().copied().collect()
    }

    /// Bytes of the records kept so far (what the run itself adds to the process's memory).
    pub fn heap_bytes(&self) -> usize {
        self.blocks.iter().map(Vec::len).sum::<usize>() * size_of::<FrameRecord>()
    }
}

/// Everything about a run that is not a per-frame number.
#[derive(Clone, Debug, Default)]
pub struct RunInfo {
    /// `static`, `fly`, `dive`, `spin` or `path`.
    pub scenario: String,
    /// Free text (`--label`).
    pub label: String,
    /// Mesh radius in chunks.
    pub radius: u32,
    /// Fly speed in voxels per second (0 when the camera does not fly).
    pub speed: f64,
    /// Terrain seed.
    pub seed: u64,
    /// The world (`--world`, with the caves of the overworld): `forest`, `overworld/mc`...
    /// Empty in runs from before the option, which all streamed the forest.
    pub world: String,
    /// Trees on.
    pub trees: bool,
    /// Voxel light and ambient occlusion (`--light`): `on`, or `off` (the look and the cost of
    /// the world before light). Empty in runs from before the option.
    pub light: String,
    /// Cave culling (`--cave-culling`): `on` or `off`. Empty in runs from before it (no
    /// culling).
    pub cave_culling: String,
    /// Chunk memory (quads) and chunk table (slots) the renderer starts with (`--arena-quads`,
    /// `--chunk-slots` or the world's default; ADR 0035: it grows from there, see
    /// `chunk_memory`).
    pub arena_quads: u32,
    /// See `arena_quads`.
    pub chunk_slots: u32,
    /// Vertical radius in chunks, or `fixed` (`--fixed-height`, the M0 layers only).
    pub vertical: String,
    /// Requested measurement (`--duration`), in seconds.
    pub duration_s: f64,
    /// Seconds between settling and the measurement.
    pub warmup_s: f64,
    /// Seconds from start until the world first settled.
    pub settle_s: f64,
    /// Slots when the world settled.
    pub settle_chunks: usize,
    /// Seconds the world needed after the measurement to settle again (`None`: not within the
    /// limit).
    pub catch_up_s: Option<f64>,
    /// Adapter name.
    pub adapter: String,
    /// Backend name.
    pub backend: String,
    /// Render tier.
    pub tier: String,
    /// Device class.
    pub class: String,
    /// Driver name and version.
    pub driver: String,
    /// Requested present mode (`--present`, `BOXEL_PRESENT`, or `immediate` by default).
    pub present: String,
    /// Updates per second cap (`--fps-limit`); 0 = unlimited, the default and the only mode of
    /// runs from before the option.
    pub fps_limit: u32,
    /// Window size in logical pixels.
    pub window: (u32, u32),
    /// `release` or `debug`.
    pub build: String,
    /// CPU model (Linux; empty elsewhere).
    pub cpu: String,
    /// Hardware threads.
    pub threads: usize,
    /// 1-minute load average at the start and at the end of the measurement (Linux).
    pub load_start: Option<f64>,
    /// See `load_start`.
    pub load_end: Option<f64>,
    /// Start of the measurement, seconds since the Unix epoch.
    pub unix_time: u64,
    /// Frames presented during the measurement.
    pub frames_presented: u64,
    /// Clock time of the world (`--time`, kept still during the run); empty in runs from
    /// before the option, which had no day and night.
    pub time: String,
    /// `--edits`: the scripted edits and their latency.
    pub edits: Option<EditStats>,
    /// Physics bodies of the run (`--bodies`, `--body-kind`, `--projectiles`): `1000 item, 50
    /// projectiles`, or empty without them (and in runs from before the options).
    pub bodies: String,
    /// `on` when the requested present mode is vsync (`--vsync on`, `--present vsync`), else
    /// `off`. Empty in runs from before the option.
    pub vsync: String,
    /// The start pose `x,y,z,yaw,pitch` (after `--cave` moved it). Empty in runs from before.
    pub camera: String,
    /// `--cave`: the cave the camera starts in (`eye=... depth=... path=...`); empty without it.
    pub cave: String,
    /// Block textures (`--pack`): `none` for the procedural ones, else the pack directory's name.
    /// Empty in runs from before the option (procedural).
    pub pack: String,
    /// Milliseconds the renderer took to read the pack and build the texture array
    /// (`RenderInfo::textures`; `None` before the GPU was selected).
    pub texture_load_ms: Option<f64>,
    /// The present mode the surface got (`RenderInfo::present_mode`, `Mailbox`, `Fifo`...).
    pub present_mode: String,
    /// The world's work between frames (`--idle-work`, ADR 0023): `on` or `off`. Empty in runs
    /// from before the option (which had none).
    pub idle_work: String,
    /// Refresh rate of the window's monitor in Hz, as the runner saw it (`None`: unknown).
    pub refresh_hz: Option<f64>,
    /// The fog (`--fog`, `--fog-res`, `--fog-steps`, ADR 0030): `off`, or the preset, the
    /// resolution and a step budget other than the engine's (`valley half`, `inside full 32
    /// steps`). Empty in runs from before the option (no fog).
    pub fog: String,
    /// The far LOD (`--lod`, ADR 0031): `off`, or the preset with its ring radius, levels and
    /// horizon (`far (R 24, K 4, 12288 blocks)`). Empty in runs from before the option (off).
    pub lod: String,
    /// Seconds from the start until the LOD's first fill was drawn (`None`: no LOD, or it did not
    /// fill during the run). Frames before it carry the fill's jobs and uploads.
    pub lod_fill_s: Option<f64>,
    /// LOD arena size in quads the renderer got (0: no LOD).
    pub lod_arena_quads: u32,
    /// What chunk memory did while measuring (ADR 0035; `None` before the measurement).
    pub chunk_memory: Option<super::memory::MemoryRun>,
}

/// What `--edits` measured: scripted edits (lamp placed and removed, shaft dug and capped) and
/// how long the world took to publish each one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct EditStats {
    /// Steps applied.
    pub steps: usize,
    /// Steps skipped (a column without ground in reach).
    pub skipped: usize,
    /// Steps that started on a settled world, timed until it settled again.
    pub timed: usize,
    /// Frames from a timed step to the world settled again, the step's own frame included:
    /// median and slowest.
    pub frames_p50: u32,
    /// See `frames_p50`.
    pub frames_max: u32,
    /// The same in milliseconds.
    pub ms_p50: f64,
    /// See `ms_p50`.
    pub ms_max: f64,
    /// Edit batches held back so their meshes publish together (`WorldStats`), during the
    /// measurement.
    pub batches_held: usize,
    /// Edit batches published in parts (some meshes took longer than the hold), during the
    /// measurement.
    pub batches_split: usize,
    /// Chunks remeshed by light changes beyond the edited voxels' chunks, during the
    /// measurement.
    pub light_fanout: usize,
}

impl EditStats {
    /// JSON object.
    fn json(&self) -> Json {
        Json::Obj(vec![
            ("steps", Json::uint(self.steps)),
            ("skipped", Json::uint(self.skipped)),
            ("timed", Json::uint(self.timed)),
            ("frames_p50", Json::uint(self.frames_p50)),
            ("frames_max", Json::uint(self.frames_max)),
            ("ms_p50", Json::num(self.ms_p50, 3)),
            ("ms_max", Json::num(self.ms_max, 3)),
            ("batches_held", Json::uint(self.batches_held)),
            ("batches_split", Json::uint(self.batches_split)),
            ("light_fanout", Json::uint(self.light_fanout)),
        ])
    }
}

/// Frame-time statistics of a subset of frames (for example the frames that crossed a chunk
/// border).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SubsetStats {
    /// Frames in the subset.
    pub frames: usize,
    /// Median.
    pub p50: Duration,
    /// 99th percentile.
    pub p99: Duration,
    /// Slowest.
    pub max: Duration,
}

impl SubsetStats {
    fn of<'a>(records: impl Iterator<Item = &'a FrameRecord>) -> Option<Self> {
        let s = FrameTimeSummary::from_frame_times(records.map(|r| r.frame_time))?;
        Some(Self {
            frames: s.frames,
            p50: s.p50,
            p99: s.p99,
            max: s.max,
        })
    }
}

/// Average, 95th percentile, maximum, first and last value of a counter over the frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spread {
    /// Mean over the frames.
    pub avg: f64,
    /// Nearest-rank 95th percentile.
    pub p95: usize,
    /// Largest.
    pub max: usize,
    /// First frame.
    pub first: usize,
    /// Last frame.
    pub last: usize,
}

impl Spread {
    fn of(values: impl Iterator<Item = usize>) -> Self {
        let mut v: Vec<usize> = values.collect();
        let (Some(&first), Some(&last)) = (v.first(), v.last()) else {
            return Self::default();
        };
        let avg = v.iter().sum::<usize>() as f64 / v.len() as f64;
        v.sort_unstable();
        let rank = (v.len() * 95).div_ceil(100).max(1);
        Self {
            avg,
            p95: v[rank - 1],
            max: v[v.len() - 1],
            first,
            last,
        }
    }
}

/// One second of the measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SecondStats {
    /// Second index (0 = the first second).
    pub second: u64,
    /// Frames that started in this second.
    pub frames: usize,
    /// Frames divided by their summed frame time.
    pub fps: f64,
    /// Slowest frame.
    pub worst: Duration,
    /// Chunks generated.
    pub generated: u64,
    /// Meshes sent to the renderer.
    pub meshes: u64,
    /// Chunk borders crossed.
    pub crossings: u64,
    /// Most jobs waiting in one frame of the second.
    pub queued: usize,
    /// Most jobs running in one frame of the second.
    pub in_flight: usize,
}

/// How long the world needed to finish the chunks a border crossing asked for: from the
/// crossing frame to the first frame with nothing queued, running, generating or meshing. That
/// is on the frame clock (each frame's start), so under a frame limit it comes in whole
/// periods; the work lag measures from the crossing frame's start to the end of the last job
/// the world had run by then, whatever the frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderCatchUp {
    /// Crossings in the measurement (`cut_off` included).
    pub crossings: usize,
    /// Crossings the world finished before the next one (or, for the last one, before the end).
    pub finished: usize,
    /// 1 when the measurement ended before the world finished the last crossing, else 0. That
    /// crossing is neither finished nor late: the run stopped too soon to tell.
    pub cut_off: usize,
    /// Mean time to finish, over the finished crossings (zero if none).
    pub avg: Duration,
    /// Longest time to finish.
    pub max: Duration,
    /// Mean work lag over the finished crossings: from the crossing frame's start to the end of
    /// the latest job when the settled frame was recorded (zero when that job ended before the
    /// crossing).
    pub work_avg: Duration,
    /// Longest work lag.
    pub work_max: Duration,
    /// Mean frames from the crossing frame to the settled one (0: the crossing frame itself).
    pub frames_avg: f64,
    /// Most frames.
    pub frames_max: usize,
}

impl BorderCatchUp {
    /// `None` without crossings. A crossing not finished before the next crossing counts as
    /// unfinished (the world is behind the camera); the last crossing, if the measurement ends
    /// before the world finished it, counts as cut off.
    fn of(records: &[FrameRecord]) -> Option<Self> {
        let crossings: Vec<usize> = (0..records.len()).filter(|&i| records[i].crossed).collect();
        let (mut finished, mut cut_off) = (0usize, 0usize);
        let (mut sum, mut max) = (Duration::ZERO, Duration::ZERO);
        let (mut work_sum, mut work_max) = (Duration::ZERO, Duration::ZERO);
        let (mut frames_sum, mut frames_max) = (0usize, 0usize);
        for (n, &at) in crossings.iter().enumerate() {
            let next = crossings.get(n + 1).copied();
            let start = records[at].t;
            let until = next.unwrap_or(records.len());
            match records[at..until].iter().position(|r| r.world.is_settled()) {
                Some(frames) => {
                    let done = &records[at + frames];
                    let lag = done.t.saturating_sub(start);
                    let work = done
                        .last_job_end
                        .map_or(Duration::ZERO, |end| end.saturating_sub(start));
                    finished += 1;
                    sum += lag;
                    max = max.max(lag);
                    work_sum += work;
                    work_max = work_max.max(work);
                    frames_sum += frames;
                    frames_max = frames_max.max(frames);
                }
                None if next.is_none() => cut_off = 1,
                None => {}
            }
        }
        let mean = |sum: Duration| {
            if finished == 0 {
                Duration::ZERO
            } else {
                sum / finished as u32
            }
        };
        (!crossings.is_empty()).then(|| Self {
            crossings: crossings.len(),
            finished,
            cut_off,
            avg: mean(sum),
            max,
            work_avg: mean(work_sum),
            work_max,
            frames_avg: if finished == 0 {
                0.0
            } else {
                frames_sum as f64 / finished as f64
            },
            frames_max,
        })
    }

    /// Crossings whose outcome is known: finished or overtaken by the next crossing.
    pub fn judged(&self) -> usize {
        self.crossings - self.cut_off
    }

    /// The sentence of the summary.
    pub fn text(&self) -> String {
        let cut = if self.cut_off > 0 {
            " The measurement ended before the world finished the last border, so it is left out."
        } else {
            ""
        };
        match (self.finished, self.judged()) {
            (_, 0) => {
                "The measurement ended before the world finished the only chunk border.".to_string()
            }
            (0, judged) => format!(
                "The world finished none of the {judged} chunk borders before the next one.{cut}"
            ),
            (finished, judged) => format!(
                "After a chunk border the world finished the new chunks in {:.0} ms on average, \
                 {:.0} ms at most ({finished} of {judged} borders finished before the next \
                 one), {:.1} frames on average, {} at most; its last job ended {:.1} ms after \
                 the crossing frame started on average, {:.1} ms at most.{cut}",
                ms(self.avg),
                ms(self.max),
                self.frames_avg,
                self.frames_max,
                ms(self.work_avg),
                ms(self.work_max),
            ),
        }
    }
}

/// Slope of the job queue over the measurement, in jobs per second: a least-squares line
/// through `queued_jobs` over `t`, each frame weighted by its frame time (a time average, so
/// periods with many short frames do not count more). A queue that fills at each border and
/// empties before the next one gives about 0 wherever the run stops; a queue that keeps growing
/// gives its growth per second. 0 with fewer than two distinct times.
fn queue_slope(records: &[FrameRecord]) -> f64 {
    let weight = |r: &FrameRecord| r.frame_time.as_secs_f64();
    let total: f64 = records.iter().map(weight).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let mean_t = records
        .iter()
        .map(|r| weight(r) * r.t.as_secs_f64())
        .sum::<f64>()
        / total;
    let mean_q = records
        .iter()
        .map(|r| weight(r) * r.world.queued_jobs as f64)
        .sum::<f64>()
        / total;
    let (mut covariance, mut variance) = (0.0, 0.0);
    for r in records {
        let dt = r.t.as_secs_f64() - mean_t;
        covariance += weight(r) * dt * (r.world.queued_jobs as f64 - mean_q);
        variance += weight(r) * dt * dt;
    }
    // Below 1 microsecond squared on average the times are all the same (rounding noise).
    if variance > total * 1e-12 {
        covariance / variance
    } else {
        0.0
    }
}

/// Totals of the world counters and their rates per measured second.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WorldTotals {
    /// Chunks generated.
    pub generated: u64,
    /// Meshes sent to the renderer.
    pub meshes: u64,
    /// Meshes removed.
    pub removed: u64,
    /// Jobs finished.
    pub jobs: u64,
    /// Quads uploaded.
    pub upload_quads: u64,
    /// Chunk borders crossed by the camera.
    pub crossings: u64,
}

impl WorldTotals {
    fn add(&mut self, r: &FrameRecord) {
        self.generated += u64::from(r.generated);
        self.meshes += u64::from(r.meshes);
        self.removed += u64::from(r.removed);
        self.jobs += u64::from(r.jobs_done);
        self.upload_quads += r.upload_quads;
        self.crossings += u64::from(r.crossed);
    }
}

/// Distribution of a set of intervals.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IntervalStats {
    /// Intervals.
    pub count: usize,
    /// Median.
    pub p50: Duration,
    /// 99th percentile (nearest rank).
    pub p99: Duration,
    /// Longest.
    pub max: Duration,
    /// Standard deviation (population).
    pub stddev: Duration,
    /// Intervals longer than 1.5 times the median: with one interval per display refresh, each
    /// one is a refresh that showed no new frame.
    pub gaps: usize,
}

impl IntervalStats {
    /// `None` for an empty set.
    fn of(intervals: impl Iterator<Item = Duration>) -> Option<Self> {
        let mut v: Vec<Duration> = intervals.collect();
        if v.is_empty() {
            return None;
        }
        v.sort_unstable();
        let n = v.len();
        let p50 = v[n / 2];
        let rank = |p: usize| v[((n * p).div_ceil(100)).clamp(1, n) - 1];
        let mean = v.iter().map(Duration::as_secs_f64).sum::<f64>() / n as f64;
        let var = v
            .iter()
            .map(|d| (d.as_secs_f64() - mean).powi(2))
            .sum::<f64>()
            / n as f64;
        Some(Self {
            count: n,
            p50,
            p99: rank(99),
            max: v[n - 1],
            stddev: Duration::from_secs_f64(var.sqrt()),
            gaps: v
                .iter()
                .filter(|&&d| d.as_secs_f64() > p50.as_secs_f64() * 1.5)
                .count(),
        })
    }
}

/// How evenly the frames were paced and presented.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pacing {
    /// Frames presented, and per measured second.
    pub presented: usize,
    /// See `presented`.
    pub presented_fps: f64,
    /// Intervals between the starts of consecutive presented frames (what the screen got).
    pub presented_intervals: Option<IntervalStats>,
    /// Frame times (update intervals): their spread.
    pub frame_intervals: Option<IntervalStats>,
    /// Frames longer than 7.5 ms.
    pub over_7_5ms: usize,
    /// Frames the runner held past their deadline for the presentation gate, the longest hold
    /// and the total.
    pub gate_waits: usize,
    /// See `gate_waits`.
    pub gate_wait_max: Duration,
    /// See `gate_waits`.
    pub gate_wait_total: Duration,
    /// Frames that stopped waiting with the gate closed (they ran without presenting).
    pub gate_misses: usize,
    /// Frames the runner paced by the display (ADR 0017).
    pub display_locked: usize,
    /// Intervals between consecutive openings of the gate (frame callbacks on Wayland).
    pub callbacks: Option<IntervalStats>,
}

impl Pacing {
    fn of(records: &[FrameRecord], seconds: f64) -> Self {
        let presented: Vec<Duration> = records
            .iter()
            .filter(|r| r.presented)
            .map(|r| r.t)
            .collect();
        let mut gates: Vec<Duration> = records.iter().filter_map(|r| r.gate_at).collect();
        gates.dedup();
        let waits = records.iter().filter(|r| !r.gate_wait.is_zero());
        Self {
            presented: presented.len(),
            presented_fps: if seconds > 0.0 {
                presented.len() as f64 / seconds
            } else {
                0.0
            },
            presented_intervals: IntervalStats::of(presented.windows(2).map(|w| w[1] - w[0])),
            frame_intervals: IntervalStats::of(records.iter().map(|r| r.frame_time)),
            over_7_5ms: records.iter().filter(|r| r.frame_time > LATE_144HZ).count(),
            gate_waits: waits.clone().count(),
            gate_wait_max: waits.clone().map(|r| r.gate_wait).max().unwrap_or_default(),
            gate_wait_total: waits.map(|r| r.gate_wait).sum(),
            gate_misses: records.iter().filter(|r| r.gate_missed).count(),
            display_locked: records.iter().filter(|r| r.display_locked).count(),
            callbacks: IntervalStats::of(gates.windows(2).map(|w| w[1] - w[0])),
        }
    }
}

/// The idle steps the runner ran between frames (ADR 0023).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct IdleStats {
    /// Mean time in idle steps per frame, and its 99th percentile (nearest rank).
    pub tail_mean: Duration,
    /// See `tail_mean`.
    pub tail_p99: Duration,
    /// Mean idle steps per frame.
    pub steps_mean: f64,
    /// Frames with at least one idle step.
    pub frames: usize,
    /// Steps that returned past the end of their slot, and the worst of them.
    pub overruns: u64,
    /// See `overruns`.
    pub overrun_max: Duration,
}

impl IdleStats {
    fn of(records: &[FrameRecord]) -> Self {
        let n = records.len();
        if n == 0 {
            return Self::default();
        }
        let mut tails: Vec<Duration> = records.iter().map(|r| r.idle).collect();
        tails.sort_unstable();
        Self {
            tail_mean: tails.iter().sum::<Duration>() / n as u32,
            tail_p99: tails[(n * 99).div_ceil(100).clamp(1, n) - 1],
            steps_mean: records.iter().map(|r| f64::from(r.idle_steps)).sum::<f64>() / n as f64,
            frames: records.iter().filter(|r| r.idle_steps > 0).count(),
            overruns: records.iter().map(|r| r.idle_overruns).sum(),
            overrun_max: records
                .iter()
                .map(|r| r.idle_late)
                .max()
                .unwrap_or_default(),
        }
    }
}

/// What the summary reports.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    /// Frame-time statistics of every measured frame.
    pub frames: FrameTimeSummary,
    /// Frames slower than the 144 Hz budget (6.94 ms).
    pub over_144hz: usize,
    /// Frames slower than the 60 Hz budget (16.7 ms).
    pub over_60hz: usize,
    /// Frames where the camera crossed a chunk border, and the others.
    pub crossing: Option<SubsetStats>,
    /// See `crossing`.
    pub other: Option<SubsetStats>,
    /// Totals over the measurement.
    pub totals: WorldTotals,
    /// Jobs waiting in the scheduler.
    pub queued: Spread,
    /// Jobs running.
    pub in_flight: Spread,
    /// Slots generating or meshing (`generating + meshing`).
    pub pending_chunks: Spread,
    /// Growth of the job queue per second ([`queue_slope`]): clearly above 0, the world falls
    /// behind the camera.
    pub backlog_trend: f64,
    /// Time to finish the chunks of each border crossing (`None` without crossings).
    pub border: Option<BorderCatchUp>,
    /// The slowest frames with their index, slowest first.
    pub worst: Vec<(usize, FrameRecord)>,
    /// Per second.
    pub timeline: Vec<SecondStats>,
    /// The last record (final counters).
    pub last: FrameRecord,
    /// Pacing and presentation.
    pub pacing: Pacing,
    /// Cave culling.
    pub culling: Culling,
    /// The idle steps between frames.
    pub idle: IdleStats,
    /// The far LOD (zeros without `--lod`).
    pub lod: LodFrames,
}

/// What the far LOD drew and uploaded over the measurement (`--lod`, ADR 0031).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LodFrames {
    /// Draw calls.
    pub draws: Spread,
    /// Quads of those draws.
    pub quads: Spread,
    /// Quads uploaded in all.
    pub upload_quads: u64,
    /// Frames that uploaded any.
    pub upload_frames: usize,
    /// Quads uploaded per frame.
    pub uploads: Spread,
    /// Arena quads in use.
    pub arena_used: Spread,
    /// Column jobs running.
    pub in_flight: Spread,
    /// Column jobs waiting.
    pub queued: Spread,
}

impl LodFrames {
    fn of(records: &[FrameRecord]) -> Self {
        Self {
            draws: Spread::of(records.iter().map(|r| r.lod_draws as usize)),
            quads: Spread::of(records.iter().map(|r| r.lod_quads as usize)),
            upload_quads: records.iter().map(|r| r.lod_upload_quads).sum(),
            upload_frames: records.iter().filter(|r| r.lod_upload_quads > 0).count(),
            uploads: Spread::of(records.iter().map(|r| r.lod_upload_quads as usize)),
            arena_used: Spread::of(records.iter().map(|r| r.lod_arena_used as usize)),
            in_flight: Spread::of(records.iter().map(|r| r.lod_in_flight as usize)),
            queued: Spread::of(records.iter().map(|r| r.lod_queued as usize)),
        }
    }
}

/// What cave culling did over the measurement.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Culling {
    /// Chunks that passed the CPU cull (0 on High).
    pub visible: Spread,
    /// Resident chunks outside the cave culling set, the frustum's included: from a cave the
    /// frustum hides most of them, so the mean barely shows cave culling turned off for a while
    /// (an edit next to the camera). A still camera shows it frame by frame (`occluded` column).
    pub occluded: Spread,
    /// Draw calls.
    pub draws: Spread,
    /// Frames in which a search ran.
    pub searches: usize,
    /// Frames that culled nothing because the eye did not see from open space.
    pub unculled: usize,
    /// Search time: mean, nearest-rank 99th percentile and largest (zero without searches).
    pub search_avg: Duration,
    /// See `search_avg`.
    pub search_p99: Duration,
    /// See `search_avg`.
    pub search_max: Duration,
    /// The renderer's update of its bits per slot, over every frame: mean, nearest-rank 99th
    /// percentile and largest.
    pub bits_avg: Duration,
    /// See `bits_avg`.
    pub bits_p99: Duration,
    /// See `bits_avg`.
    pub bits_max: Duration,
}

impl Culling {
    fn of(records: &[FrameRecord]) -> Self {
        // Mean, nearest-rank 99th percentile and largest (zero when empty).
        let spread = |mut times: Vec<Duration>| {
            times.sort_unstable();
            let n = times.len();
            if n == 0 {
                Default::default()
            } else {
                (
                    times.iter().sum::<Duration>() / n as u32,
                    times[(n * 99).div_ceil(100).max(1) - 1],
                    times[n - 1],
                )
            }
        };
        let searches: Vec<Duration> = records.iter().filter_map(|r| r.search).collect();
        let n = searches.len();
        let (search_avg, search_p99, search_max) = spread(searches);
        let (bits_avg, bits_p99, bits_max) = spread(records.iter().map(|r| r.occlusion).collect());
        Self {
            visible: Spread::of(records.iter().map(|r| r.visible_chunks as usize)),
            occluded: Spread::of(records.iter().map(|r| r.occluded as usize)),
            draws: Spread::of(records.iter().map(|r| r.draws as usize)),
            searches: n,
            unculled: records.iter().filter(|r| r.unculled).count(),
            search_avg,
            search_p99,
            search_max,
            bits_avg,
            bits_p99,
            bits_max,
        }
    }
}

impl Summary {
    /// Summary of the records (`None` if there are none).
    pub fn compute(records: &[FrameRecord]) -> Option<Self> {
        let frames = FrameTimeSummary::from_frame_times(records.iter().map(|r| r.frame_time))?;
        let last = *records.last()?;
        let mut totals = WorldTotals::default();
        records.iter().for_each(|r| totals.add(r));
        let mut worst: Vec<(usize, FrameRecord)> = records.iter().copied().enumerate().collect();
        // Slowest first; ties keep the earlier frame first.
        worst.sort_by(|(ia, a), (ib, b)| b.frame_time.cmp(&a.frame_time).then(ia.cmp(ib)));
        worst.truncate(WORST_FRAMES);
        Some(Self {
            over_144hz: records
                .iter()
                .filter(|r| r.frame_time > BUDGET_144HZ)
                .count(),
            over_60hz: records
                .iter()
                .filter(|r| r.frame_time > BUDGET_60HZ)
                .count(),
            crossing: SubsetStats::of(records.iter().filter(|r| r.crossed)),
            other: SubsetStats::of(records.iter().filter(|r| !r.crossed)),
            totals,
            queued: Spread::of(records.iter().map(|r| r.world.queued_jobs)),
            in_flight: Spread::of(records.iter().map(|r| r.world.in_flight)),
            pending_chunks: Spread::of(
                records.iter().map(|r| r.world.generating + r.world.meshing),
            ),
            backlog_trend: queue_slope(records),
            border: BorderCatchUp::of(records),
            worst,
            timeline: timeline(records),
            last,
            pacing: Pacing::of(records, frames.total.as_secs_f64()),
            culling: Culling::of(records),
            idle: IdleStats::of(records),
            lod: LodFrames::of(records),
            frames,
        })
    }

    /// `count / measured seconds` (0 when nothing was measured).
    pub fn per_second(&self, count: u64) -> f64 {
        let seconds = self.frames.total.as_secs_f64();
        if seconds > 0.0 {
            count as f64 / seconds
        } else {
            0.0
        }
    }
}

/// Groups the records by the second their frame started in.
fn timeline(records: &[FrameRecord]) -> Vec<SecondStats> {
    let mut out: Vec<SecondStats> = Vec::new();
    let mut time_in_second = Duration::ZERO;
    for r in records {
        let second = r.t.as_secs();
        if out.last().is_none_or(|s| s.second != second) {
            close_second(out.last_mut(), time_in_second);
            time_in_second = Duration::ZERO;
            out.push(SecondStats {
                second,
                ..SecondStats::default()
            });
        }
        let Some(s) = out.last_mut() else {
            continue;
        };
        s.frames += 1;
        s.worst = s.worst.max(r.frame_time);
        s.generated += u64::from(r.generated);
        s.meshes += u64::from(r.meshes);
        s.crossings += u64::from(r.crossed);
        s.queued = s.queued.max(r.world.queued_jobs);
        s.in_flight = s.in_flight.max(r.world.in_flight);
        time_in_second += r.frame_time;
    }
    close_second(out.last_mut(), time_in_second);
    out
}

fn close_second(second: Option<&mut SecondStats>, time: Duration) {
    if let Some(s) = second
        && !time.is_zero()
    {
        s.fps = s.frames as f64 / time.as_secs_f64();
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// `value` rounded to `decimals`, with `-0` turned into `0`.
fn rounded(value: f64, decimals: i32) -> f64 {
    let scale = 10f64.powi(decimals);
    let v = (value * scale).round() / scale;
    if v == 0.0 { 0.0 } else { v }
}

// ---------------------------------------------------------------------------------------------
// CSV

/// CSV header (one column per [`FrameRecord`] field; world counters spelled out).
pub const CSV_HEADER: &str = "frame,t_ms,frame_ms,presented,crossed,cam_x,cam_z,yaw_deg,\
generated,jobs_done,meshes,removed,upload_quads,d_data_ready,d_chunks,chunks,data_ready,\
generating,mesh_ready,mesh_empty,meshing,queued_jobs,in_flight,gpu_chunks,quads,draws,arena_used,\
dropped_chunks,gate_wait_ms,gate_missed,gate_ms,display_locked,visible_chunks,occluded,search_ms,\
unculled,occlusion_ms,idle_ms,idle_steps,idle_late_ms,idle_overruns,job_end_ms,lod_draws,\
lod_quads,lod_upload_quads,lod_arena_used,lod_in_flight,lod_queued";

/// The CSV text: header plus one row per record.
pub fn csv(records: &[FrameRecord]) -> String {
    let mut out = String::with_capacity(160 * (records.len() + 1));
    out.push_str(CSV_HEADER);
    out.push('\n');
    for (i, r) in records.iter().enumerate() {
        let w = &r.world;
        let _ = writeln!(
            out,
            "{i},{:.3},{:.4},{},{},{:.2},{:.2},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},\
             {:.4},{},{},{},{},{},{},{},{:.4},{:.4},{},{:.4},{},{},{},{},{},{},{},{}",
            ms(r.t),
            ms(r.frame_time),
            u8::from(r.presented),
            u8::from(r.crossed),
            r.camera.0,
            r.camera.1,
            r.camera.2,
            r.generated,
            r.jobs_done,
            r.meshes,
            r.removed,
            r.upload_quads,
            r.d_data_ready,
            r.d_chunks,
            w.chunks,
            w.data_ready,
            w.generating,
            w.mesh_ready,
            w.mesh_empty,
            w.meshing,
            w.queued_jobs,
            w.in_flight,
            r.gpu_chunks,
            r.quads,
            r.draws,
            r.arena_used,
            r.dropped_chunks,
            ms(r.gate_wait),
            u8::from(r.gate_missed),
            r.gate_at
                .map_or_else(String::new, |at| format!("{:.3}", ms(at))),
            u8::from(r.display_locked),
            r.visible_chunks,
            r.occluded,
            r.search
                .map_or_else(String::new, |d| format!("{:.4}", ms(d))),
            u8::from(r.unculled),
            ms(r.occlusion),
            ms(r.idle),
            r.idle_steps,
            ms(r.idle_late),
            r.idle_overruns,
            r.last_job_end
                .map_or_else(String::new, |at| format!("{:.3}", ms(at))),
            r.lod_draws,
            r.lod_quads,
            r.lod_upload_quads,
            r.lod_arena_used,
            r.lod_in_flight,
            r.lod_queued,
        );
    }
    out
}

// ---------------------------------------------------------------------------------------------
// JSON (hand-written: the workspace has no JSON crate and the layout is small)

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number (non-finite values are written as `null`).
    Num(f64),
    /// An integer.
    Int(i64),
    /// A string.
    Str(String),
    /// An array.
    Arr(Vec<Json>),
    /// An object (keys in this order).
    Obj(Vec<(&'static str, Json)>),
}

impl Json {
    /// `value` rounded to `decimals`; a value that rounds to zero is `0`, never `-0`.
    fn num(value: f64, decimals: i32) -> Self {
        Json::Num(rounded(value, decimals))
    }

    fn ms(d: Duration) -> Self {
        Self::num(ms(d), 4)
    }

    fn uint(value: impl TryInto<i64>) -> Self {
        Json::Int(value.try_into().unwrap_or(i64::MAX))
    }

    fn opt(value: Option<f64>, decimals: i32) -> Self {
        value.map_or(Json::Null, |v| Self::num(v, decimals))
    }

    /// Writes the value, pretty-printed with two-space indents.
    pub fn write(&self, out: &mut String, indent: usize) {
        match self {
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Num(n) if n.is_finite() => {
                let _ = write!(out, "{n}");
            }
            Json::Null | Json::Num(_) => out.push_str("null"),
            Json::Int(i) => {
                let _ = write!(out, "{i}");
            }
            Json::Str(s) => write_json_string(out, s),
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Arr(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    push_indent(out, indent + 1);
                    item.write(out, indent + 1);
                }
                out.push('\n');
                push_indent(out, indent);
                out.push(']');
            }
            Json::Obj(fields) if fields.is_empty() => out.push_str("{}"),
            Json::Obj(fields) => {
                out.push('{');
                for (i, (key, value)) in fields.iter().enumerate() {
                    out.push_str(if i == 0 { "\n" } else { ",\n" });
                    push_indent(out, indent + 1);
                    write_json_string(out, key);
                    out.push_str(": ");
                    value.write(out, indent + 1);
                }
                out.push('\n');
                push_indent(out, indent);
                out.push('}');
            }
        }
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push_str("  ");
    }
}

fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn subset_json(s: Option<SubsetStats>) -> Json {
    s.map_or(Json::Null, |s| {
        Json::Obj(vec![
            ("frames", Json::uint(s.frames)),
            ("p50_ms", Json::ms(s.p50)),
            ("p99_ms", Json::ms(s.p99)),
            ("max_ms", Json::ms(s.max)),
        ])
    })
}

fn intervals_json(s: Option<IntervalStats>) -> Json {
    s.map_or(Json::Null, |s| {
        Json::Obj(vec![
            ("count", Json::uint(s.count)),
            ("p50_ms", Json::ms(s.p50)),
            ("p99_ms", Json::ms(s.p99)),
            ("max_ms", Json::ms(s.max)),
            ("stddev_ms", Json::ms(s.stddev)),
            ("gaps", Json::uint(s.gaps)),
        ])
    })
}

fn spread_json(s: &Spread) -> Json {
    Json::Obj(vec![
        ("avg", Json::num(s.avg, 1)),
        ("p95", Json::uint(s.p95)),
        ("max", Json::uint(s.max)),
        ("first", Json::uint(s.first)),
        ("last", Json::uint(s.last)),
    ])
}

/// The JSON document of a run.
pub fn json(info: &RunInfo, s: &Summary) -> Json {
    let f = &s.frames;
    let t = &s.totals;
    let w = &s.last.world;
    let run = Json::Obj(vec![
        ("scenario", Json::Str(info.scenario.clone())),
        ("label", Json::Str(info.label.clone())),
        ("radius", Json::uint(info.radius)),
        ("speed", Json::num(info.speed, 2)),
        ("seed", Json::Str(info.seed.to_string())),
        ("world", Json::Str(info.world.clone())),
        ("trees", Json::Bool(info.trees)),
        ("light", Json::Str(info.light.clone())),
        ("cave_culling", Json::Str(info.cave_culling.clone())),
        ("pack", Json::Str(info.pack.clone())),
        ("texture_load_ms", Json::opt(info.texture_load_ms, 2)),
        ("arena_quads", Json::uint(info.arena_quads)),
        ("chunk_slots", Json::uint(info.chunk_slots)),
        ("vertical", Json::Str(info.vertical.clone())),
        ("duration_s", Json::num(info.duration_s, 3)),
        ("warmup_s", Json::num(info.warmup_s, 3)),
        ("settle_s", Json::num(info.settle_s, 3)),
        ("settle_chunks", Json::uint(info.settle_chunks)),
        ("adapter", Json::Str(info.adapter.clone())),
        ("backend", Json::Str(info.backend.clone())),
        ("tier", Json::Str(info.tier.clone())),
        ("class", Json::Str(info.class.clone())),
        ("driver", Json::Str(info.driver.clone())),
        ("present", Json::Str(info.present.clone())),
        ("fps_limit", Json::uint(info.fps_limit)),
        ("bodies", Json::Str(info.bodies.clone())),
        (
            "window",
            Json::Str(format!("{}x{}", info.window.0, info.window.1)),
        ),
        ("build", Json::Str(info.build.clone())),
        ("cpu", Json::Str(info.cpu.clone())),
        ("threads", Json::uint(info.threads)),
        ("load_start", Json::opt(info.load_start, 2)),
        ("load_end", Json::opt(info.load_end, 2)),
        ("unix_time", Json::uint(info.unix_time)),
        ("date_utc", Json::Str(utc_date(info.unix_time))),
        ("frames_presented", Json::uint(info.frames_presented)),
        ("time", Json::Str(info.time.clone())),
        ("vsync", Json::Str(info.vsync.clone())),
        ("camera", Json::Str(info.camera.clone())),
        ("cave", Json::Str(info.cave.clone())),
        ("present_mode", Json::Str(info.present_mode.clone())),
        ("refresh_hz", Json::opt(info.refresh_hz, 3)),
        ("idle_work", Json::Str(info.idle_work.clone())),
        ("fog", Json::Str(info.fog.clone())),
        ("lod", Json::Str(info.lod.clone())),
        ("lod_fill_s", Json::opt(info.lod_fill_s, 2)),
        ("lod_arena_quads", Json::uint(info.lod_arena_quads)),
        (
            "chunk_memory",
            info.chunk_memory.as_ref().map_or(Json::Null, |m| {
                Json::Obj(vec![
                    ("mode", Json::Str(m.mode.clone())),
                    ("max_quads", Json::uint(m.max_quads)),
                    ("radius_steps", Json::Str(m.radius_steps.clone())),
                    ("pages", Json::uint(m.pages)),
                    ("pages_created", Json::uint(m.pages_created)),
                    ("pages_released", Json::uint(m.pages_released)),
                    ("max_page_create_ms", Json::num(m.max_page_create_ms, 3)),
                    ("emergency_pages", Json::uint(m.emergency_pages)),
                    ("late_emergency_pages", Json::uint(m.late_emergency_pages)),
                    ("peak_page_bytes", Json::uint(m.peak_page_bytes)),
                    ("max_relocated_frame", Json::uint(m.max_relocated_frame)),
                    ("refused_waiting", Json::uint(m.refused_waiting)),
                ])
            }),
        ),
    ]);
    let frames = Json::Obj(vec![
        ("count", Json::uint(f.frames)),
        ("seconds", Json::num(f.total.as_secs_f64(), 3)),
        ("avg_fps", Json::num(f.avg_fps, 1)),
        ("mean_ms", Json::ms(f.mean)),
        ("p50_ms", Json::ms(f.p50)),
        ("p95_ms", Json::ms(f.p95)),
        ("p99_ms", Json::ms(f.p99)),
        ("p999_ms", Json::ms(f.p999)),
        ("min_ms", Json::ms(f.min)),
        ("max_ms", Json::ms(f.max)),
        ("low_1_fps", Json::num(f.low_1_percent_fps, 1)),
        ("over_144hz", Json::uint(s.over_144hz)),
        ("over_60hz", Json::uint(s.over_60hz)),
        ("over_7_5ms", Json::uint(s.pacing.over_7_5ms)),
        (
            "stddev_ms",
            s.pacing
                .frame_intervals
                .map_or(Json::Null, |i| Json::ms(i.stddev)),
        ),
        ("crossing", subset_json(s.crossing)),
        ("other", subset_json(s.other)),
    ]);
    let world = Json::Obj(vec![
        ("generated", Json::uint(t.generated)),
        ("generated_per_s", Json::num(s.per_second(t.generated), 1)),
        ("meshes", Json::uint(t.meshes)),
        ("meshes_per_s", Json::num(s.per_second(t.meshes), 1)),
        ("removed", Json::uint(t.removed)),
        ("removed_per_s", Json::num(s.per_second(t.removed), 1)),
        ("jobs", Json::uint(t.jobs)),
        ("jobs_per_s", Json::num(s.per_second(t.jobs), 1)),
        ("upload_quads", Json::uint(t.upload_quads)),
        (
            "upload_quads_per_s",
            Json::num(s.per_second(t.upload_quads), 1),
        ),
        ("crossings", Json::uint(t.crossings)),
        ("queued", spread_json(&s.queued)),
        ("in_flight", spread_json(&s.in_flight)),
        ("pending_chunks", spread_json(&s.pending_chunks)),
        ("backlog_trend_per_s", Json::num(s.backlog_trend, 1)),
        (
            "border_catch_up",
            s.border.map_or(Json::Null, |b| {
                Json::Obj(vec![
                    ("crossings", Json::uint(b.crossings)),
                    ("finished", Json::uint(b.finished)),
                    ("cut_off", Json::uint(b.cut_off)),
                    ("avg_ms", Json::ms(b.avg)),
                    ("max_ms", Json::ms(b.max)),
                    ("frames_avg", Json::num(b.frames_avg, 2)),
                    ("frames_max", Json::uint(b.frames_max)),
                    ("work_avg_ms", Json::ms(b.work_avg)),
                    ("work_max_ms", Json::ms(b.work_max)),
                ])
            }),
        ),
        ("catch_up_s", Json::opt(info.catch_up_s, 3)),
        ("edits", info.edits.map_or(Json::Null, |e| e.json())),
        (
            "final",
            Json::Obj(vec![
                ("chunks", Json::uint(w.chunks)),
                ("data_ready", Json::uint(w.data_ready)),
                ("mesh_ready", Json::uint(w.mesh_ready)),
                ("mesh_empty", Json::uint(w.mesh_empty)),
                ("gpu_chunks", Json::uint(s.last.gpu_chunks)),
                ("quads", Json::uint(s.last.quads)),
                ("draws", Json::uint(s.last.draws)),
                ("arena_used", Json::uint(s.last.arena_used)),
                ("dropped_chunks", Json::uint(s.last.dropped_chunks)),
            ]),
        ),
    ]);
    let worst = s
        .worst
        .iter()
        .map(|(i, r)| {
            Json::Obj(vec![
                ("frame", Json::uint(*i)),
                ("t_s", Json::num(r.t.as_secs_f64(), 3)),
                ("ms", Json::ms(r.frame_time)),
                ("crossed", Json::Bool(r.crossed)),
                ("presented", Json::Bool(r.presented)),
                ("generated", Json::uint(r.generated)),
                ("meshes", Json::uint(r.meshes)),
                ("removed", Json::uint(r.removed)),
                ("upload_quads", Json::uint(r.upload_quads)),
                ("queued", Json::uint(r.world.queued_jobs)),
                ("in_flight", Json::uint(r.world.in_flight)),
            ])
        })
        .collect();
    let timeline = s
        .timeline
        .iter()
        .map(|sec| {
            Json::Obj(vec![
                ("second", Json::uint(sec.second)),
                ("frames", Json::uint(sec.frames)),
                ("fps", Json::num(sec.fps, 1)),
                ("worst_ms", Json::ms(sec.worst)),
                ("generated", Json::uint(sec.generated)),
                ("meshes", Json::uint(sec.meshes)),
                ("crossings", Json::uint(sec.crossings)),
                ("queued_max", Json::uint(sec.queued)),
                ("in_flight_max", Json::uint(sec.in_flight)),
            ])
        })
        .collect();
    let c = &s.culling;
    let culling = Json::Obj(vec![
        ("visible_chunks", spread_json(&c.visible)),
        ("occluded", spread_json(&c.occluded)),
        ("draws", spread_json(&c.draws)),
        ("searches", Json::uint(c.searches)),
        ("unculled_frames", Json::uint(c.unculled)),
        ("search_avg_ms", Json::ms(c.search_avg)),
        ("search_p99_ms", Json::ms(c.search_p99)),
        ("search_max_ms", Json::ms(c.search_max)),
        ("bits_avg_ms", Json::ms(c.bits_avg)),
        ("bits_p99_ms", Json::ms(c.bits_p99)),
        ("bits_max_ms", Json::ms(c.bits_max)),
    ]);
    let p = &s.pacing;
    let pacing = Json::Obj(vec![
        ("presented", Json::uint(p.presented)),
        ("presented_fps", Json::num(p.presented_fps, 2)),
        ("presented_intervals", intervals_json(p.presented_intervals)),
        ("gate_waits", Json::uint(p.gate_waits)),
        ("gate_wait_max_ms", Json::ms(p.gate_wait_max)),
        ("gate_wait_total_ms", Json::ms(p.gate_wait_total)),
        ("gate_misses", Json::uint(p.gate_misses)),
        ("display_locked", Json::uint(p.display_locked)),
        ("callbacks", intervals_json(p.callbacks)),
    ]);
    let i = &s.idle;
    let idle = Json::Obj(vec![
        ("tail_mean_ms", Json::ms(i.tail_mean)),
        ("tail_p99_ms", Json::ms(i.tail_p99)),
        ("steps_mean", Json::num(i.steps_mean, 2)),
        ("frames", Json::uint(i.frames)),
        ("overruns", Json::uint(i.overruns)),
        ("overrun_max_ms", Json::ms(i.overrun_max)),
    ]);
    Json::Obj(vec![
        ("format", Json::uint(FORMAT_VERSION)),
        ("run", run),
        ("frames", frames),
        ("pacing", pacing),
        ("idle", idle),
        ("world", world),
        ("culling", culling),
        ("lod", lod_json(info, &s.lod)),
        ("worst", Json::Arr(worst)),
        ("timeline", Json::Arr(timeline)),
    ])
}

/// The far LOD's object, `null` without `--lod`.
fn lod_json(info: &RunInfo, l: &LodFrames) -> Json {
    if !lod_on(info) {
        return Json::Null;
    }
    Json::Obj(vec![
        ("draws", spread_json(&l.draws)),
        ("quads", spread_json(&l.quads)),
        ("upload_quads", Json::uint(l.upload_quads)),
        ("upload_frames", Json::uint(l.upload_frames)),
        ("upload_quads_per_frame", spread_json(&l.uploads)),
        ("arena_used", spread_json(&l.arena_used)),
        ("arena_quads", Json::uint(info.lod_arena_quads)),
        ("in_flight", spread_json(&l.in_flight)),
        ("queued", spread_json(&l.queued)),
    ])
}

/// The run had the far LOD (`--lod` other than off).
fn lod_on(info: &RunInfo) -> bool {
    !info.lod.is_empty() && info.lod != "off"
}

// ---------------------------------------------------------------------------------------------
// Markdown

/// The far LOD's section (`--lod`; nothing without it).
fn lod_markdown(out: &mut String, info: &RunInfo, l: &LodFrames) {
    if !lod_on(info) {
        return;
    }
    let _ = writeln!(out, "## Far LOD\n");
    let fill = info.lod_fill_s.map_or_else(
        || "It did not fill during the run.".to_string(),
        |f| format!("It filled {f:.2} s after the start."),
    );
    let _ = writeln!(
        out,
        "{} {fill}\n\n| per frame | avg | p95 | max | first | last |\n|---|---:|---:|---:|---:|---:|",
        info.lod
    );
    for (name, sp) in [
        ("draw calls", &l.draws),
        ("quads drawn", &l.quads),
        ("quads uploaded", &l.uploads),
        ("arena quads in use", &l.arena_used),
        ("column jobs in flight", &l.in_flight),
        ("column jobs queued", &l.queued),
    ] {
        let _ = writeln!(
            out,
            "| {name} | {:.1} | {} | {} | {} | {} |",
            sp.avg, sp.p95, sp.max, sp.first, sp.last
        );
    }
    let _ = writeln!(
        out,
        "\n{} quads uploaded in {} frames ({:.2} MiB); arena of {} quads.\n",
        l.upload_quads,
        l.upload_frames,
        l.upload_quads as f64 * 8.0 / (1024.0 * 1024.0),
        info.lod_arena_quads,
    );
}

/// A warning when cave culling was on but culled nothing in more than 1 % of the measured
/// frames because the eye was in rock: such a run does not measure cave culling.
pub fn rock_warning(info: &RunInfo, s: &Summary) -> Option<String> {
    let (unculled, frames) = (s.culling.unculled, s.frames.frames);
    (info.cave_culling == "on" && unculled * 100 > frames).then(|| {
        format!(
            "Warning: in {unculled} of {frames} frames the eye was in rock (or nearer a wall than \
             the near plane) and cave culling culled nothing: this run does not measure it. Start \
             in a cave with --cave."
        )
    })
}

/// Heading of the JSON block (`tools/bench_report.py` reads the first JSON code block after it).
pub const DATA_HEADING: &str = "## Data";

/// Short name of a run: `fly r16 32 vox/s`, `static r12`, `fly r32 64 vox/s limit 144 fps`.
pub fn run_title(info: &RunInfo) -> String {
    let mut title = if matches!(info.scenario.as_str(), "fly" | "dive" | "path") {
        format!("{} r{} {} vox/s", info.scenario, info.radius, info.speed)
    } else {
        format!("{} r{}", info.scenario, info.radius)
    };
    // The forest (the default) keeps the titles of the earlier runs.
    if !info.world.is_empty() && info.world != "forest" {
        title = format!("{title} {}", info.world);
    }
    // Light on (the default) keeps the titles of the earlier runs.
    if info.light == "off" {
        title = format!("{title} light off");
    }
    // So does cave culling on (the default).
    if info.cave_culling == "off" {
        title = format!("{title} culling off");
    }
    // A camera in a cave (`--cave`) is not comparable with one in the rock.
    if !info.cave.is_empty() {
        title = format!("{title} cave");
    }
    // So do the procedural textures.
    if !info.pack.is_empty() && info.pack != "none" {
        title = format!("{title} pack {}", info.pack);
    }
    if info.fps_limit > 0 {
        title = format!("{title} limit {} fps", info.fps_limit);
    }
    // Vsync off (the default) keeps the titles of the earlier runs.
    if info.vsync == "on" {
        title = format!("{title} vsync");
    }
    if !info.bodies.is_empty() {
        title = format!("{title} bodies {}", info.bodies);
    }
    // Same order as `title` in tools/bench_report.py.
    if info.edits.is_some() {
        title = format!("{title} edits");
    }
    title
}

/// The summary document: tables for people, then the JSON block for tools.
pub fn markdown(info: &RunInfo, s: &Summary) -> String {
    let f = &s.frames;
    let t = &s.totals;
    let mut out = String::new();
    let load = |l: Option<f64>| l.map_or_else(|| "?".to_string(), |l| format!("{l:.2}"));
    let _ = writeln!(out, "# boxel stress: {}\n", run_title(info));
    let _ = writeln!(
        out,
        "- Run: {}, {} build, label `{}`, {} ({} threads), load {} -> {}.",
        utc_date(info.unix_time),
        info.build,
        info.label,
        info.cpu,
        info.threads,
        load(info.load_start),
        load(info.load_end),
    );
    let _ = writeln!(
        out,
        "- GPU: {}, {} {} ({}), driver `{}`, present {} ({}), fps limit {}, window {}x{}{}.",
        info.adapter,
        info.backend,
        info.tier,
        info.class,
        info.driver,
        info.present,
        if info.present_mode.is_empty() {
            "?"
        } else {
            &info.present_mode
        },
        if info.fps_limit == 0 {
            "none".to_string()
        } else {
            info.fps_limit.to_string()
        },
        info.window.0,
        info.window.1,
        info.refresh_hz
            .map_or_else(String::new, |hz| format!(", display {hz:.3} Hz")),
    );
    let _ = writeln!(
        out,
        "- World: {}, seed {}, trees {}, light {}, cave culling {}, radius {}, settled in {:.2} s \
         with {} chunks; \
         {:.0} s measured after {:.0} s of warmup. Renderer: {} quads, {} chunk slots, textures \
         {}.{}\n",
        if info.world.is_empty() {
            "?"
        } else {
            &info.world
        },
        info.seed,
        if info.trees { "on" } else { "off" },
        if info.light.is_empty() {
            "?"
        } else {
            &info.light
        },
        if info.cave_culling.is_empty() {
            "?"
        } else {
            &info.cave_culling
        },
        info.radius,
        info.settle_s,
        info.settle_chunks,
        info.duration_s,
        info.warmup_s,
        info.arena_quads,
        info.chunk_slots,
        match (info.pack.as_str(), info.texture_load_ms) {
            ("" | "none", _) => "procedural".to_string(),
            (pack, Some(ms)) => format!("{pack} (loaded in {ms:.1} ms)"),
            (pack, None) => pack.to_string(),
        },
        if info.time.is_empty() {
            String::new()
        } else {
            format!(" Clock {} (still).", info.time)
        },
    );
    if !info.camera.is_empty() {
        let _ = writeln!(
            out,
            "- Camera: start {}{}.\n",
            info.camera,
            if info.cave.is_empty() {
                String::new()
            } else {
                format!(", in a cave ({})", info.cave)
            }
        );
    }

    let _ = writeln!(out, "## Frames\n");
    let _ = writeln!(
        out,
        "| frames | avg fps | p50 ms | p95 ms | p99 ms | p99.9 ms | max ms | 1% low fps | \
         > 6.94 ms | > 16.7 ms |"
    );
    let _ = writeln!(out, "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    let _ = writeln!(
        out,
        "| {} | {:.0} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.0} | {} | {} |\n",
        f.frames,
        f.avg_fps,
        ms(f.p50),
        ms(f.p95),
        ms(f.p99),
        ms(f.p999),
        ms(f.max),
        f.low_1_percent_fps,
        s.over_144hz,
        s.over_60hz,
    );
    let subset = |name: &str, sub: Option<SubsetStats>| match sub {
        Some(sub) => format!(
            "| {name} | {} | {:.2} | {:.2} | {:.2} |",
            sub.frames,
            ms(sub.p50),
            ms(sub.p99),
            ms(sub.max)
        ),
        None => format!("| {name} | 0 | - | - | - |"),
    };
    let _ = writeln!(out, "| frames | count | p50 ms | p99 ms | max ms |");
    let _ = writeln!(out, "|---|---:|---:|---:|---:|");
    let _ = writeln!(out, "{}", subset("crossed a chunk border", s.crossing));
    let _ = writeln!(out, "{}\n", subset("other", s.other));
    pacing_markdown(&mut out, &s.pacing);
    idle_markdown(&mut out, info, &s.idle);

    let _ = writeln!(out, "## World\n");
    let _ = writeln!(
        out,
        "| generated/s | meshes/s | removed/s | jobs/s | quads uploaded/s | crossings |"
    );
    let _ = writeln!(out, "|---:|---:|---:|---:|---:|---:|");
    let _ = writeln!(
        out,
        "| {:.0} | {:.0} | {:.0} | {:.0} | {:.0} | {} |\n",
        s.per_second(t.generated),
        s.per_second(t.meshes),
        s.per_second(t.removed),
        s.per_second(t.jobs),
        s.per_second(t.upload_quads),
        t.crossings,
    );
    let _ = writeln!(
        out,
        "| backlog | avg | p95 | max | first | last |\n|---|---:|---:|---:|---:|---:|"
    );
    for (name, sp) in [
        ("jobs queued", &s.queued),
        ("jobs in flight", &s.in_flight),
        ("chunks generating or meshing", &s.pending_chunks),
    ] {
        let _ = writeln!(
            out,
            "| {name} | {:.1} | {} | {} | {} | {} |",
            sp.avg, sp.p95, sp.max, sp.first, sp.last
        );
    }
    let border = s.border.map_or_else(String::new, |b| b.text() + " ");
    let catch_up = info.catch_up_s.map_or_else(
        || "It did not settle again within the time limit.".to_string(),
        |c| format!("It settled again {c:.2} s after the camera stopped."),
    );
    let w = &s.last.world;
    let _ = writeln!(
        out,
        "\n{border}Queue trend {:+.1} jobs/s (slope over the measurement; clearly above 0: the \
         world falls behind). {catch_up} At the end: {} chunks, {} meshes ({} empty), {} on the \
         GPU, {} quads, {} draws, arena {} of {} entries, {} chunks dropped.\n",
        rounded(s.backlog_trend, 1),
        w.chunks,
        w.mesh_ready,
        w.mesh_empty,
        s.last.gpu_chunks,
        s.last.quads,
        s.last.draws,
        s.last.arena_used,
        info.arena_quads,
        s.last.dropped_chunks,
    );
    lod_markdown(&mut out, info, &s.lod);

    let c = &s.culling;
    let _ = writeln!(out, "## Cave culling\n");
    let _ = writeln!(
        out,
        "| per frame | avg | p95 | max | first | last |\n|---|---:|---:|---:|---:|---:|"
    );
    for (name, sp) in [
        ("chunks hidden (cave culling and frustum)", &c.occluded),
        ("chunks through the CPU cull (Mid, Low)", &c.visible),
        ("draw calls", &c.draws),
    ] {
        let _ = writeln!(
            out,
            "| {name} | {:.1} | {} | {} | {} | {} |",
            sp.avg, sp.p95, sp.max, sp.first, sp.last
        );
    }
    let _ = writeln!(
        out,
        "\nSearches: {} frames, {:.3} ms on average, p99 {:.3} ms, max {:.3} ms. Frames that \
         culled nothing because the eye saw through rock (inside it, or nearer a wall than the \
         near plane): {}. The renderer's bits per slot (not in the search time): {:.3} ms on \
         average, p99 {:.3} ms, max {:.3} ms.\n",
        c.searches,
        ms(c.search_avg),
        ms(c.search_p99),
        ms(c.search_max),
        c.unculled,
        ms(c.bits_avg),
        ms(c.bits_p99),
        ms(c.bits_max),
    );
    if let Some(warning) = rock_warning(info, s) {
        let _ = writeln!(out, "**{warning}**\n");
    }

    if let Some(e) = &info.edits {
        let _ = writeln!(out, "## Edits\n");
        let _ = writeln!(
            out,
            "| steps | skipped | timed | frames p50 | frames max | ms p50 | ms max | batches held \
             | batches split | light fan-out |"
        );
        let _ = writeln!(out, "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {:.2} | {:.2} | {} | {} | {} |\n",
            e.steps,
            e.skipped,
            e.timed,
            e.frames_p50,
            e.frames_max,
            e.ms_p50,
            e.ms_max,
            e.batches_held,
            e.batches_split,
            e.light_fanout,
        );
        let _ = writeln!(
            out,
            "A timed step starts on a settled world and ends when it settles again (every remesh \
             published), its own frame included.\n"
        );
    }

    let _ = writeln!(out, "## Worst {} frames\n", s.worst.len());
    let _ = writeln!(
        out,
        "| frame | t s | ms | crossed | presented | generated | meshes | removed | quads up | \
         queued | in flight |"
    );
    let _ = writeln!(
        out,
        "|---:|---:|---:|---|---|---:|---:|---:|---:|---:|---:|"
    );
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    for (i, r) in &s.worst {
        let _ = writeln!(
            out,
            "| {i} | {:.2} | {:.2} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.t.as_secs_f64(),
            ms(r.frame_time),
            yes_no(r.crossed),
            yes_no(r.presented),
            r.generated,
            r.meshes,
            r.removed,
            r.upload_quads,
            r.world.queued_jobs,
            r.world.in_flight,
        );
    }

    let _ = writeln!(out, "\n## Per second\n");
    let _ = writeln!(
        out,
        "| s | frames | fps | worst ms | generated | meshes | crossings | queued max | in flight \
         max |"
    );
    let _ = writeln!(out, "|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for sec in &s.timeline {
        let _ = writeln!(
            out,
            "| {} | {} | {:.0} | {:.2} | {} | {} | {} | {} | {} |",
            sec.second,
            sec.frames,
            sec.fps,
            ms(sec.worst),
            sec.generated,
            sec.meshes,
            sec.crossings,
            sec.queued,
            sec.in_flight,
        );
    }

    let _ = writeln!(
        out,
        "\n{DATA_HEADING}\n\nFor `tools/bench_report.py` (format {FORMAT_VERSION}).\n"
    );
    out.push_str("```json\n");
    json(info, s).write(&mut out, 0);
    out.push_str("\n```\n");
    out
}

/// The `## Pacing` section.
fn pacing_markdown(out: &mut String, p: &Pacing) {
    let _ = writeln!(out, "## Pacing\n");
    let _ = writeln!(
        out,
        "| intervals | count | p50 ms | p99 ms | max ms | sd ms | over 1.5 x p50 |"
    );
    let _ = writeln!(out, "|---|---:|---:|---:|---:|---:|---:|");
    for (name, i) in [
        ("frames (updates)", p.frame_intervals),
        ("presented frames", p.presented_intervals),
        ("gate openings (frame callbacks)", p.callbacks),
    ] {
        let _ = match i {
            Some(i) => writeln!(
                out,
                "| {name} | {} | {:.3} | {:.3} | {:.3} | {:.3} | {} |",
                i.count,
                ms(i.p50),
                ms(i.p99),
                ms(i.max),
                ms(i.stddev),
                i.gaps
            ),
            None => writeln!(out, "| {name} | 0 | - | - | - | - | - |"),
        };
    }
    let _ = writeln!(
        out,
        "\n{} frames presented ({:.1} per second), {} frames over 7.5 ms. The runner held {} \
         frames past their deadline for the presentation gate ({:.1} ms in all, {:.2} ms at \
         most); {} of them gave up and ran without presenting. {} frames followed the display.\n",
        p.presented,
        p.presented_fps,
        p.over_7_5ms,
        p.gate_waits,
        ms(p.gate_wait_total),
        ms(p.gate_wait_max),
        p.gate_misses,
        p.display_locked,
    );
}

/// The idle steps between frames (a sentence after `## Pacing`).
fn idle_markdown(out: &mut String, info: &RunInfo, i: &IdleStats) {
    let _ = writeln!(
        out,
        "Between frames (world work {}): {:.2} ms of idle steps per frame on average, {:.2} ms \
         at p99, {:.1} steps per frame, in {} frames; {} steps returned past the end of their \
         slot ({:.3} ms at worst).\n",
        if info.idle_work.is_empty() {
            "?"
        } else {
            &info.idle_work
        },
        ms(i.tail_mean),
        ms(i.tail_p99),
        i.steps_mean,
        i.frames,
        i.overruns,
        ms(i.overrun_max),
    );
}

/// One line for the terminal.
pub fn one_line(info: &RunInfo, s: &Summary) -> String {
    let f = &s.frames;
    format!(
        "stress: {} {} {}: frames={} avg={:.0} fps p50={:.2} p99={:.2} p99.9={:.2} max={:.2} ms \
         1%low={:.0} fps >6.94ms={} >16.7ms={} generated/s={:.0} queued_max={} >7.5ms={} \
         shown={:.1}/s gate_waits={} gate_misses={} catch_up={} idle={:.2}ms/frame \
         idle_overruns={}",
        run_title(info),
        info.backend,
        info.tier,
        f.frames,
        f.avg_fps,
        ms(f.p50),
        ms(f.p99),
        ms(f.p999),
        ms(f.max),
        f.low_1_percent_fps,
        s.over_144hz,
        s.over_60hz,
        s.per_second(s.totals.generated),
        s.queued.max,
        s.pacing.over_7_5ms,
        s.pacing.presented_fps,
        s.pacing.gate_waits,
        s.pacing.gate_misses,
        s.border.map_or_else(
            || "-".to_string(),
            |b| format!(
                "{:.1}/{:.1}ms,{:.1}frames,work={:.1}/{:.1}ms",
                ms(b.avg),
                ms(b.max),
                b.frames_avg,
                ms(b.work_avg),
                ms(b.work_max)
            )
        ),
        ms(s.idle.tail_mean),
        s.idle.overruns,
    )
}

/// `YYYY-MM-DD HH:MM:SS UTC` for seconds since the Unix epoch.
pub fn utc_date(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let rem = unix % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t_ms: u64, frame_ms: f64) -> FrameRecord {
        FrameRecord {
            t: Duration::from_millis(t_ms),
            frame_time: Duration::from_secs_f64(frame_ms / 1e3),
            ..FrameRecord::default()
        }
    }

    /// 2.5 s of 2 ms frames, a 10 ms crossing frame at 1.0 s and a 20 ms frame at 2.0 s.
    fn sample() -> Vec<FrameRecord> {
        let mut records = Vec::new();
        let mut t = 0u64;
        while t < 2500 {
            let mut r = frame(t, 2.0);
            r.generated = 1;
            r.meshes = 2;
            r.jobs_done = 3;
            r.upload_quads = 100;
            r.world.queued_jobs = (t / 100) as usize;
            r.world.in_flight = 12;
            if t == 1000 {
                r.frame_time = Duration::from_millis(10);
                r.crossed = true;
            }
            if t == 2000 {
                r.frame_time = Duration::from_millis(20);
                r.removed = 7;
            }
            records.push(r);
            t += 2;
        }
        records
    }

    #[test]
    fn summary_counts_budgets_rates_and_worst_frames() {
        let records = sample();
        let s = Summary::compute(&records).expect("records");
        assert_eq!(s.frames.frames, 1250);
        assert_eq!(s.over_144hz, 2);
        assert_eq!(s.over_60hz, 1);
        assert_eq!(s.frames.max, Duration::from_millis(20));
        assert_eq!(s.totals.generated, 1250);
        assert_eq!(s.totals.meshes, 2500);
        assert_eq!(s.totals.removed, 7);
        assert_eq!(s.totals.crossings, 1);
        // 1248 * 2 + 10 + 20 = 2526 ms measured.
        assert_eq!(s.frames.total, Duration::from_millis(2526));
        assert!((s.per_second(s.totals.generated) - 1250.0 / 2.526).abs() < 1e-9);
        let worst: Vec<usize> = s.worst.iter().map(|(i, _)| *i).collect();
        assert_eq!(
            worst,
            [1000, 500, 0, 1, 2],
            "slowest first, then in frame order"
        );
        let crossing = s.crossing.expect("one crossing frame");
        assert_eq!(
            (crossing.frames, crossing.max),
            (1, Duration::from_millis(10))
        );
        let other = s.other.expect("other frames");
        assert_eq!(other.frames, 1249);
        assert_eq!(other.p50, Duration::from_millis(2));
        assert_eq!(s.queued.first, 0);
        assert_eq!(s.queued.last, 24);
        assert_eq!(s.queued.max, 24);
        assert_eq!(s.in_flight.avg, 12.0);
        // The queue grows by one job every 100 ms.
        assert!((s.backlog_trend - 10.0).abs() < 0.1, "{}", s.backlog_trend);
        assert_eq!(s.last, *records.last().expect("records"));
        assert_eq!(Summary::compute(&[]), None);
    }

    #[test]
    fn timeline_groups_frames_by_second() {
        let s = Summary::compute(&sample()).expect("records");
        let seconds: Vec<u64> = s.timeline.iter().map(|sec| sec.second).collect();
        assert_eq!(seconds, [0, 1, 2]);
        assert_eq!(s.timeline[0].frames, 500);
        assert!((s.timeline[0].fps - 500.0).abs() < 1e-9);
        assert_eq!(s.timeline[1].worst, Duration::from_millis(10));
        assert_eq!(s.timeline[2].frames, 250);
        assert_eq!(s.timeline[2].worst, Duration::from_millis(20));
        assert_eq!(s.timeline[2].queued, 24, "the peak of the second");
        assert_eq!(s.timeline[0].queued, 9);
        assert_eq!(s.timeline[1].crossings, 1);
    }

    /// Frames every 10 ms; a crossing at each frame of `crossings`; busy (one job queued) in
    /// the frames of `busy`.
    fn crossings(frames: u64, crossings: &[u64], busy: impl Fn(u64) -> bool) -> Vec<FrameRecord> {
        (0..frames)
            .map(|i| {
                let mut r = frame(i * 10, 10.0);
                r.crossed = crossings.contains(&i);
                r.world.queued_jobs = usize::from(busy(i));
                r
            })
            .collect()
    }

    #[test]
    fn border_catch_up_measures_the_lag_after_each_crossing() {
        // Crossings at 0 ms (settled again at 30 ms), 100 ms (settled at 180 ms), 200 ms (still
        // busy at the next crossing: late) and 300 ms (still busy when the measurement ends).
        let records = crossings(
            40,
            &[0, 10, 20, 30],
            |i| matches!(i, 0..=2 | 10..=17 | 20..),
        );
        let b = BorderCatchUp::of(&records).expect("crossings");
        assert_eq!((b.crossings, b.finished, b.cut_off), (4, 2, 1));
        assert_eq!(
            b.judged(),
            3,
            "the cut-off crossing is neither finished nor late"
        );
        assert_eq!(b.max, Duration::from_millis(80));
        assert_eq!(b.avg, Duration::from_millis(55));
        assert_eq!(
            b.text(),
            "After a chunk border the world finished the new chunks in 55 ms on average, 80 ms at \
             most (2 of 3 borders finished before the next one), 5.5 frames on average, 8 at \
             most; its last job ended 0.0 ms after the crossing frame started on average, 0.0 ms \
             at most. The measurement ended before the world finished the last border, so it is \
             left out."
        );
        assert_eq!(
            (b.frames_avg, b.frames_max),
            (5.5, 8),
            "3 frames and 8 frames"
        );
        let none: Vec<FrameRecord> = (0..5u64).map(|i| frame(i, 1.0)).collect();
        assert_eq!(BorderCatchUp::of(&none), None);
        // A crossing frame that is already settled finishes at once.
        let mut quick = none.clone();
        quick[1].crossed = true;
        let b = BorderCatchUp::of(&quick).expect("one crossing");
        assert_eq!((b.finished, b.cut_off, b.max), (1, 0, Duration::ZERO));
        assert_eq!((b.frames_max, b.work_max), (0, Duration::ZERO));
    }

    #[test]
    fn border_work_lag_ends_at_the_last_job_before_the_settled_frame() {
        // 7 ms frames (a 144 fps cap); crossings at frames 0 and 10, settled at frames 4 and
        // 12. The jobs of the first ended 22.5 ms in (frame 4 starts at 28 ms); the second's
        // last job ended before its crossing frame (nothing to do): a lag of 0.
        let mut records: Vec<FrameRecord> = (0..20u64)
            .map(|i| {
                let mut r = frame(i * 7, 7.0);
                r.crossed = i == 0 || i == 10;
                r.world.queued_jobs = usize::from(matches!(i, 0..=3 | 10..=11));
                r
            })
            .collect();
        for r in &mut records[..10] {
            r.last_job_end = Some(Duration::from_micros(22_500));
        }
        for r in &mut records[10..] {
            r.last_job_end = Some(Duration::from_millis(60));
        }
        records[1].last_job_end = None;
        let b = BorderCatchUp::of(&records).expect("crossings");
        assert_eq!((b.finished, b.cut_off), (2, 0));
        assert_eq!(
            (b.avg, b.max),
            (Duration::from_millis(21), Duration::from_millis(28))
        );
        assert_eq!((b.frames_avg, b.frames_max), (3.0, 4));
        assert_eq!(b.work_max, Duration::from_micros(22_500));
        assert_eq!(b.work_avg, Duration::from_micros(11_250));
        assert!(
            b.text().contains("its last job ended 11.2 ms after"),
            "{}",
            b.text()
        );
    }

    #[test]
    fn idle_stats_sum_the_steps_between_frames() {
        let mut records: Vec<FrameRecord> = (0..100u64).map(|i| frame(i * 7, 7.0)).collect();
        for (i, r) in records.iter_mut().enumerate() {
            r.idle = Duration::from_micros(if i == 50 { 4000 } else { 1000 });
            r.idle_steps = 4;
        }
        records[7].idle_overruns = 2;
        records[7].idle_late = Duration::from_micros(30);
        records[9].idle_steps = 0;
        records[9].idle = Duration::ZERO;
        let i = IdleStats::of(&records);
        assert_eq!(i.tail_mean, Duration::from_micros((98 * 1000 + 4000) / 100));
        assert_eq!(i.tail_p99, Duration::from_micros(1000));
        assert!((i.steps_mean - 3.96).abs() < 1e-9);
        assert_eq!((i.frames, i.overruns), (99, 2));
        assert_eq!(i.overrun_max, Duration::from_micros(30));
        assert_eq!(IdleStats::of(&[]), IdleStats::default());
        let s = Summary::compute(&records).expect("records");
        let info = RunInfo {
            idle_work: "on".to_string(),
            ..RunInfo::default()
        };
        let text = markdown(&info, &s);
        assert!(
            text.contains("Between frames (world work on): 1.02 ms of idle steps per frame"),
            "{text}"
        );
        assert!(text.contains("\"idle_work\": \"on\""));
        assert!(text.contains("\"overruns\": 2,"));
    }

    #[test]
    fn a_run_ending_right_after_a_crossing_is_not_late() {
        // The last crossing finished in the last frame: finished, not cut off.
        let b = BorderCatchUp::of(&crossings(12, &[1, 11], |i| i == 1)).expect("crossings");
        assert_eq!((b.crossings, b.finished, b.cut_off), (2, 2, 0));
        // The only crossing is cut off by the end.
        let b = BorderCatchUp::of(&crossings(12, &[11], |i| i == 11)).expect("one crossing");
        assert_eq!(
            (b.crossings, b.finished, b.cut_off, b.judged()),
            (1, 0, 1, 0)
        );
        assert_eq!(
            b.text(),
            "The measurement ended before the world finished the only chunk border."
        );
        // Every border overtaken by the next one: the world is behind.
        let b = BorderCatchUp::of(&crossings(30, &[0, 10, 20], |_| true)).expect("crossings");
        assert_eq!((b.crossings, b.finished, b.cut_off), (3, 0, 1));
        assert_eq!(
            b.text(),
            "The world finished none of the 2 chunk borders before the next one. The measurement \
             ended before the world finished the last border, so it is left out."
        );
    }

    /// Frames every `step_ms` for `seconds`, queue from `queue(t in ms)`.
    fn queue_run(seconds: f64, step_ms: u64, queue: impl Fn(u64) -> usize) -> Vec<FrameRecord> {
        let end = (seconds * 1e3) as u64;
        (0..)
            .map(|i| i * step_ms)
            .take_while(|&t| t <= end)
            .map(|t| {
                let mut r = frame(t, step_ms as f64);
                r.world.queued_jobs = queue(t);
                r
            })
            .collect()
    }

    #[test]
    fn queue_trend_is_the_slope_of_the_queue() {
        // A queue growing by 5 jobs every 10 ms grows by 500 per second.
        let ramp = queue_run(2.0, 10, |t| (t / 10 * 5) as usize);
        assert!(
            (queue_slope(&ramp) - 500.0).abs() < 1e-6,
            "{}",
            queue_slope(&ramp)
        );
        // Flat queue.
        assert!(queue_slope(&queue_run(2.0, 1, |_| 7)).abs() < 1e-9);
        // Nothing to fit.
        assert_eq!(queue_slope(&[]), 0.0);
        assert_eq!(queue_slope(&[frame(5, 1.0)]), 0.0);
        assert_eq!(queue_slope(&[frame(5, 1.0), frame(5, 1.0)]), 0.0);
        assert_eq!(queue_slope(&[frame(5, 0.0), frame(6, 0.0)]), 0.0);
    }

    #[test]
    fn queue_trend_does_not_depend_on_where_the_run_stops() {
        // A border every 500 ms from 250 ms: 200 jobs queued for 15 ms, then none. The run stops
        // 2 ms after the eighth border, with the queue full: last minus first would say
        // +53 jobs/s, but the world keeps up.
        let sawtooth = |t: u64| {
            if t >= 250 && (t - 250) % 500 < 15 {
                200
            } else {
                0
            }
        };
        let records = queue_run(3.752, 1, sawtooth);
        assert_eq!(records.last().map(|r| r.world.queued_jobs), Some(200));
        let slope = queue_slope(&records);
        assert!(slope.abs() < 1.0, "{slope}");
        // Ending between two borders gives about the same.
        let between = queue_slope(&queue_run(3.5, 1, sawtooth));
        assert!(between.abs() < 1.0, "{between}");
        // A world that falls behind (each border leaves 20 jobs more) still shows.
        let behind = |t: u64| sawtooth(t) + 20 * ((t + 250) / 500) as usize;
        let slope = queue_slope(&queue_run(3.752, 1, behind));
        assert!((slope - 40.0).abs() < 5.0, "{slope}");
    }

    #[test]
    fn queue_trend_weighs_frames_by_their_time() {
        // First second: 1000 frames of 1 ms with an empty queue. Second second: 10 frames of
        // 100 ms with 10 jobs. In time that is a step from 0 to 10 in the middle of 2 s (slope
        // 7.5); counting frames instead would weigh the first second 100 times more.
        let mut records = queue_run(0.999, 1, |_| 0);
        records.extend((10..20u64).map(|i| {
            let mut r = frame(i * 100, 100.0);
            r.world.queued_jobs = 10;
            r
        }));
        let slope = queue_slope(&records);
        assert!((slope - 7.5).abs() < 0.5, "{slope}");
    }

    #[test]
    fn worst_frames_say_whether_they_were_presented() {
        let mut records: Vec<FrameRecord> = (0..10u64).map(|i| frame(i, 1.0)).collect();
        records[3].frame_time = Duration::from_millis(5);
        records[9].frame_time = Duration::from_millis(4);
        records[9].presented = true;
        let s = Summary::compute(&records).expect("records");
        let text = markdown(&RunInfo::default(), &s);
        assert!(text.contains("| 3 | 0.00 | 5.00 | no | no | 0 |"), "{text}");
        assert!(
            text.contains("| 9 | 0.01 | 4.00 | no | yes | 0 |"),
            "{text}"
        );
        let data = json(&RunInfo::default(), &s);
        let Json::Obj(fields) = data else {
            panic!("an object")
        };
        let worst = fields.iter().find(|(k, _)| *k == "worst").map(|(_, v)| v);
        let Some(Json::Arr(worst)) = worst else {
            panic!("a worst array")
        };
        let presented: Vec<bool> = worst
            .iter()
            .map(|w| match w {
                Json::Obj(f) => f
                    .iter()
                    .any(|(k, v)| *k == "presented" && *v == Json::Bool(true)),
                _ => false,
            })
            .collect();
        assert_eq!(presented, [false, true, false, false, false]);
    }

    #[test]
    fn spread_of_counters() {
        let s = Spread::of((1..=100).rev());
        assert_eq!((s.first, s.last, s.max, s.p95), (100, 1, 100, 95));
        assert!((s.avg - 50.5).abs() < 1e-9);
        assert_eq!(Spread::of(std::iter::empty()), Spread::default());
    }

    #[test]
    fn record_log_keeps_order_across_blocks() {
        let mut log = RecordLog::default();
        for i in 0..(RecordLog::BLOCK * 2 + 3) {
            log.push(frame(i as u64, 1.0));
        }
        let all = log.to_vec();
        assert_eq!(all.len(), RecordLog::BLOCK * 2 + 3);
        assert_eq!(log.blocks.len(), 3);
        assert!(log.blocks.iter().all(|b| b.capacity() == RecordLog::BLOCK));
        assert!(all.windows(2).all(|w| w[0].t < w[1].t));
    }

    #[test]
    fn csv_has_a_header_and_one_row_per_frame() {
        let records = sample();
        let text = csv(&records);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), records.len() + 1);
        let columns = CSV_HEADER.split(',').count();
        assert_eq!(columns, 48);
        assert!(lines.iter().all(|l| l.split(',').count() == columns));
        assert!(
            lines[501].starts_with("500,1000.000,10.0000,0,1,"),
            "{}",
            lines[501]
        );
    }

    #[test]
    fn pacing_counts_presented_gaps_gate_waits_and_late_frames() {
        // 6.95 ms frames, all presented, gate openings every 6.95 ms; frame 10 holds 1.7 ms for
        // the gate and gives up (8.65 ms, not presented), frame 11 comes back (5.25 ms).
        let mut records = Vec::new();
        let mut t = Duration::ZERO;
        for i in 0..40u32 {
            let frame_time = match i {
                10 => Duration::from_micros(8650),
                11 => Duration::from_micros(5250),
                _ => Duration::from_micros(6950),
            };
            let mut r = FrameRecord {
                t,
                frame_time,
                presented: i != 10,
                // One callback per presented frame, a little before it starts.
                gate_at: (i != 10).then(|| t.saturating_sub(Duration::from_micros(100))),
                ..FrameRecord::default()
            };
            if i == 10 {
                r.gate_wait = Duration::from_micros(1700);
                r.gate_missed = true;
                r.gate_at = records.last().and_then(|p: &FrameRecord| p.gate_at);
            }
            records.push(r);
            t += frame_time;
        }
        let p = Pacing::of(&records, t.as_secs_f64());
        assert_eq!(p.presented, 39);
        assert!((p.presented_fps - 39.0 / t.as_secs_f64()).abs() < 1e-9);
        let shown = p.presented_intervals.expect("intervals");
        assert_eq!(
            (shown.count, shown.gaps),
            (38, 1),
            "one refresh without a new frame"
        );
        assert_eq!(shown.p50, Duration::from_micros(6950));
        assert_eq!(shown.max, Duration::from_micros(6950 + 8650));
        assert_eq!(p.over_7_5ms, 1);
        assert_eq!((p.gate_waits, p.gate_misses), (1, 1));
        assert_eq!(p.gate_wait_total, Duration::from_micros(1700));
        let callbacks = p.callbacks.expect("callbacks");
        assert_eq!((callbacks.count, callbacks.gaps), (38, 1));
        let frames = p.frame_intervals.expect("frames");
        assert!(frames.stddev > Duration::ZERO && frames.stddev < Duration::from_millis(1));
        assert_eq!(Pacing::of(&[], 0.0), Pacing::default());
    }

    #[test]
    fn json_strings_are_escaped_and_non_finite_numbers_are_null() {
        let mut out = String::new();
        Json::Obj(vec![
            ("s", Json::Str("a\"b\\c\nd\u{1}é".to_string())),
            ("n", Json::Num(f64::NAN)),
            ("i", Json::Int(-3)),
            ("a", Json::Arr(vec![Json::Bool(true), Json::Null])),
            ("e", Json::Arr(Vec::new())),
        ])
        .write(&mut out, 0);
        assert_eq!(
            out,
            "{\n  \"s\": \"a\\\"b\\\\c\\nd\\u0001é\",\n  \"n\": null,\n  \"i\": -3,\n  \"a\": [\n    \
             true,\n    null\n  ],\n  \"e\": []\n}"
        );
        assert_eq!(Json::num(1.23456, 2), Json::Num(1.23));
        let mut out = String::new();
        Json::num(-0.004, 1).write(&mut out, 0);
        assert_eq!(out, "0", "no negative zero");
        assert_eq!(format!("{:+.1}", rounded(-0.04, 1)), "+0.0");
        assert_eq!(rounded(-0.06, 1), -0.1);
    }

    #[test]
    fn markdown_has_the_tables_and_one_json_block() {
        let info = RunInfo {
            scenario: "fly".to_string(),
            radius: 16,
            speed: 32.0,
            catch_up_s: Some(1.5),
            ..RunInfo::default()
        };
        let s = Summary::compute(&sample()).expect("records");
        let text = markdown(&info, &s);
        assert!(text.starts_with("# boxel stress: fly r16 32 vox/s\n"));
        for heading in [
            "## Frames",
            "## World",
            "## Cave culling",
            "## Worst 5 frames",
            "## Per second",
            DATA_HEADING,
        ] {
            assert!(text.contains(heading), "missing {heading}");
        }
        assert_eq!(text.matches("```json").count(), 1);
        let data = text.split("```json\n").nth(1).expect("json block");
        let data = data.split("\n```").next().expect("closed block");
        assert!(data.starts_with("{\n  \"format\": 1,"));
        assert!(data.contains("\"catch_up_s\": 1.5"));
        assert!(data.contains("\"fps_limit\": 0"));
        assert!(text.contains(", fps limit none, window "));
        assert!(data.contains("\"light\": \"\""), "empty without --light");
        assert!(
            text.contains(", trees off, light ?, cave culling ?, radius 16,"),
            "{text}"
        );
        assert!(data.contains("\"cave_culling\": \"\""));
        assert!(data.contains("\"search_avg_ms\": 0"));
        assert!(data.contains("\"presented\": false"));
        assert!(text.contains("| frame | t s | ms | crossed | presented | generated |"));
        assert!(
            data.contains("\"cut_off\": 1"),
            "the crossing at 1 s never settles"
        );
        assert!(data.contains("\"over_144hz\": 2"));
        assert!(data.contains("\"arena_used\": 0,\n"));
        assert!(data.contains("\"dropped_chunks\": 0\n"));
        assert!(
            text.contains(", arena 0 of 0 entries, 0 chunks dropped."),
            "{text}"
        );
        assert!(one_line(&info, &s).contains(">6.94ms=2 >16.7ms=1"));
        assert!(text.contains("## Pacing"));
        assert!(data.contains("\"over_7_5ms\": 2"));
        assert!(
            data.contains("\"pacing\": {\n    \"presented\": 0,"),
            "{data}"
        );
        assert!(data.contains("\"vsync\": \"\""));
        assert!(
            data.contains("\"idle_work\": \"\""),
            "empty without --idle-work"
        );
        assert!(
            data.contains("\"idle\": {\n    \"tail_mean_ms\": 0,"),
            "{data}"
        );
        assert!(data.contains("\"work_max_ms\": 0"), "{data}");
        assert!(
            data.contains("\"lod\": null,"),
            "no LOD object without --lod"
        );
        assert!(!text.contains("## Far LOD"));
    }

    #[test]
    fn the_far_lod_section_sums_its_uploads_and_spreads_its_counters() {
        let info = RunInfo {
            scenario: "fly".to_string(),
            lod: "far (R 24, K 4, 12288 blocks)".to_string(),
            lod_fill_s: Some(2.5),
            lod_arena_quads: 1 << 20,
            ..RunInfo::default()
        };
        // Four frames: uploads of 100 and 300 quads in two of them.
        let records: Vec<FrameRecord> = [0u64, 100, 0, 300]
            .into_iter()
            .enumerate()
            .map(|(i, uploads)| FrameRecord {
                t: Duration::from_millis(10 * i as u64),
                frame_time: Duration::from_millis(10),
                lod_draws: 2000 + i as u32,
                lod_quads: 1_000_000,
                lod_upload_quads: uploads,
                lod_arena_used: 400 * (i as u32 + 1),
                lod_in_flight: 5,
                ..FrameRecord::default()
            })
            .collect();
        let s = Summary::compute(&records).expect("records");
        assert_eq!((s.lod.upload_quads, s.lod.upload_frames), (400, 2));
        assert_eq!((s.lod.uploads.max, s.lod.uploads.avg), (300, 100.0));
        assert_eq!((s.lod.draws.first, s.lod.draws.last), (2000, 2003));
        assert_eq!(s.lod.arena_used.last, 1600);
        let text = markdown(&info, &s);
        assert!(text.contains("## Far LOD"));
        assert!(text.contains("It filled 2.50 s after the start."), "{text}");
        assert!(
            text.contains("| quads uploaded | 100.0 | 300 | 300 | 0 | 300 |"),
            "{text}"
        );
        assert!(text.contains("400 quads uploaded in 2 frames"), "{text}");
        let data = text.split("```json\n").nth(1).expect("json block");
        assert!(data.contains("\"lod_arena_quads\": 1048576"), "{data}");
        assert!(data.contains("\"upload_quads\": 400,"), "{data}");
        assert!(data.contains("\"upload_frames\": 2,"), "{data}");
        let csv = csv(&records);
        assert!(
            csv.lines()
                .nth(4)
                .is_some_and(|l| l.ends_with(",2003,1000000,300,1600,5,0")),
            "{csv}"
        );
    }

    #[test]
    fn titles_name_the_speed_and_any_world_but_the_forest() {
        let mut info = RunInfo {
            scenario: "dive".to_string(),
            radius: 16,
            speed: 64.0,
            world: "forest".to_string(),
            ..RunInfo::default()
        };
        assert_eq!(run_title(&info), "dive r16 64 vox/s");
        info.world = "overworld/mc".to_string();
        assert_eq!(run_title(&info), "dive r16 64 vox/s overworld/mc");
        info.scenario = "static".to_string();
        assert_eq!(run_title(&info), "static r16 overworld/mc");
        info.world = String::new();
        assert_eq!(run_title(&info), "static r16");
        info.light = "on".to_string();
        assert_eq!(run_title(&info), "static r16", "light on keeps the title");
        info.light = "off".to_string();
        assert_eq!(run_title(&info), "static r16 light off");
        info.light = String::new();
        info.fps_limit = 144;
        assert_eq!(run_title(&info), "static r16 limit 144 fps");
        info.vsync = "off".to_string();
        assert_eq!(
            run_title(&info),
            "static r16 limit 144 fps",
            "vsync off keeps the title"
        );
        info.vsync = "on".to_string();
        assert_eq!(run_title(&info), "static r16 limit 144 fps vsync");
        info.bodies = "1000 item, 0 projectiles".to_string();
        info.edits = Some(EditStats::default());
        assert_eq!(
            run_title(&info),
            "static r16 limit 144 fps vsync bodies 1000 item, 0 projectiles edits"
        );
    }

    #[test]
    fn caves_are_named_and_an_eye_in_rock_is_warned_about() {
        let mut info = RunInfo {
            scenario: "path".to_string(),
            radius: 16,
            speed: 8.0,
            world: "overworld/mc".to_string(),
            cave_culling: "on".to_string(),
            ..RunInfo::default()
        };
        assert_eq!(run_title(&info), "path r16 8 vox/s overworld/mc");
        info.camera = "1.5,-990.5,2.5,-90,0".to_string();
        info.cave = "eye=1.5,-990.5,2.5 depth=1100".to_string();
        assert_eq!(run_title(&info), "path r16 8 vox/s overworld/mc cave");
        let mut records = sample();
        let s = Summary::compute(&records).expect("records");
        assert_eq!(rock_warning(&info, &s), None);
        let text = markdown(&info, &s);
        assert!(
            text.contains("- Camera: start 1.5,-990.5,2.5,-90,0, in a cave (eye=1.5,-990.5,2.5"),
            "{text}"
        );
        assert!(text.contains("\"cave\": \"eye=1.5,-990.5,2.5 depth=1100\""));
        assert!(!text.contains("Warning"));
        // Every frame with the eye in rock.
        for r in &mut records {
            r.unculled = true;
        }
        let s = Summary::compute(&records).expect("records");
        let warning = rock_warning(&info, &s).expect("a warning");
        assert!(warning.contains("--cave"));
        assert!(markdown(&info, &s).contains(&format!("**{warning}**")));
        info.cave_culling = "off".to_string();
        assert_eq!(rock_warning(&info, &s), None, "nothing to measure");
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc_date(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_date(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc_date(1_791_195_045), "2026-10-05 10:10:45 UTC");
    }
}
