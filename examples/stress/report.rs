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
}

/// Everything about a run that is not a per-frame number.
#[derive(Clone, Debug, Default)]
pub struct RunInfo {
    /// `static`, `fly`, `dive` or `spin`.
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
    /// Renderer quad arena (quads) and chunk table (slots) the renderer got: the requested ones
    /// (`--arena-quads`, `--chunk-slots` or the world's default) cut to the device's limits.
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
/// crossing frame to the first frame with nothing queued, running, generating or meshing.
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
}

impl BorderCatchUp {
    /// `None` without crossings. A crossing not finished before the next crossing counts as
    /// unfinished (the world is behind the camera); the last crossing, if the measurement ends
    /// before the world finished it, counts as cut off.
    fn of(records: &[FrameRecord]) -> Option<Self> {
        let crossings: Vec<usize> = (0..records.len()).filter(|&i| records[i].crossed).collect();
        let (mut finished, mut cut_off) = (0usize, 0usize);
        let (mut sum, mut max) = (Duration::ZERO, Duration::ZERO);
        for (n, &at) in crossings.iter().enumerate() {
            let next = crossings.get(n + 1).copied();
            let start = records[at].t;
            let until = next.unwrap_or(records.len());
            match records[at..until].iter().find(|r| r.world.is_settled()) {
                Some(done) => {
                    let lag = done.t.saturating_sub(start);
                    finished += 1;
                    sum += lag;
                    max = max.max(lag);
                }
                None if next.is_none() => cut_off = 1,
                None => {}
            }
        }
        (!crossings.is_empty()).then(|| Self {
            crossings: crossings.len(),
            finished,
            cut_off,
            avg: if finished == 0 {
                Duration::ZERO
            } else {
                sum / finished as u32
            },
            max,
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
                 one).{cut}",
                ms(self.avg),
                ms(self.max),
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
dropped_chunks";

/// The CSV text: header plus one row per record.
pub fn csv(records: &[FrameRecord]) -> String {
    let mut out = String::with_capacity(160 * (records.len() + 1));
    out.push_str(CSV_HEADER);
    out.push('\n');
    for (i, r) in records.iter().enumerate() {
        let w = &r.world;
        let _ = writeln!(
            out,
            "{i},{:.3},{:.4},{},{},{:.2},{:.2},{:.2},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
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
    Json::Obj(vec![
        ("format", Json::uint(FORMAT_VERSION)),
        ("run", run),
        ("frames", frames),
        ("world", world),
        ("worst", Json::Arr(worst)),
        ("timeline", Json::Arr(timeline)),
    ])
}

// ---------------------------------------------------------------------------------------------
// Markdown

/// Heading of the JSON block (`tools/bench_report.py` reads the first JSON code block after it).
pub const DATA_HEADING: &str = "## Data";

/// Short name of a run: `fly r16 32 vox/s`, `static r12`, `fly r32 64 vox/s limit 144 fps`.
pub fn run_title(info: &RunInfo) -> String {
    let mut title = if matches!(info.scenario.as_str(), "fly" | "dive") {
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
    if info.fps_limit > 0 {
        title = format!("{title} limit {} fps", info.fps_limit);
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
        "- GPU: {}, {} {} ({}), driver `{}`, present {}, fps limit {}, window {}x{}.",
        info.adapter,
        info.backend,
        info.tier,
        info.class,
        info.driver,
        info.present,
        if info.fps_limit == 0 {
            "none".to_string()
        } else {
            info.fps_limit.to_string()
        },
        info.window.0,
        info.window.1,
    );
    let _ = writeln!(
        out,
        "- World: {}, seed {}, trees {}, light {}, radius {}, settled in {:.2} s with {} chunks; \
         {:.0} s measured after {:.0} s of warmup. Renderer: {} quads, {} chunk slots.{}\n",
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
        info.radius,
        info.settle_s,
        info.settle_chunks,
        info.duration_s,
        info.warmup_s,
        info.arena_quads,
        info.chunk_slots,
        if info.time.is_empty() {
            String::new()
        } else {
            format!(" Clock {} (still).", info.time)
        },
    );

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

/// One line for the terminal.
pub fn one_line(info: &RunInfo, s: &Summary) -> String {
    let f = &s.frames;
    format!(
        "stress: {} {} {}: frames={} avg={:.0} fps p50={:.2} p99={:.2} p99.9={:.2} max={:.2} ms \
         1%low={:.0} fps >6.94ms={} >16.7ms={} generated/s={:.0} queued_max={}",
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
             most (2 of 3 borders finished before the next one). The measurement ended before \
             the world finished the last border, so it is left out."
        );
        let none: Vec<FrameRecord> = (0..5u64).map(|i| frame(i, 1.0)).collect();
        assert_eq!(BorderCatchUp::of(&none), None);
        // A crossing frame that is already settled finishes at once.
        let mut quick = none.clone();
        quick[1].crossed = true;
        let b = BorderCatchUp::of(&quick).expect("one crossing");
        assert_eq!((b.finished, b.cut_off, b.max), (1, 0, Duration::ZERO));
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
        assert_eq!(columns, 28);
        assert!(lines.iter().all(|l| l.split(',').count() == columns));
        assert!(
            lines[501].starts_with("500,1000.000,10.0000,0,1,"),
            "{}",
            lines[501]
        );
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
        assert!(text.contains(", trees off, light ?, radius 16,"), "{text}");
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
        info.bodies = "1000 item, 0 projectiles".to_string();
        info.edits = Some(EditStats::default());
        assert_eq!(
            run_title(&info),
            "static r16 limit 144 fps bodies 1000 item, 0 projectiles edits"
        );
    }

    #[test]
    fn utc_dates() {
        assert_eq!(utc_date(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(utc_date(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(utc_date(1_791_195_045), "2026-10-05 10:10:45 UTC");
    }
}
