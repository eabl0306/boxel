//! The chunk streaming state machine behind the world systems: interest (load/unload), edits,
//! job dispatch and result collection. ECS-free: the systems in `plugin.rs` are thin wrappers,
//! so the rules are unit tested directly.
//!
//! Slot invariants:
//! - In a slot whose data is `Ready` or `Missing`, `neighbors_ready` bit `i` is set iff neighbor
//!   `i` is outside `WorldSettings::vertical_bounds` or has a slot whose data is `Ready` or
//!   `Missing`. Slots still waiting for their data get their bits when it arrives (nothing reads
//!   them before).
//! - A slot has at most one generation or mesh job in flight (generation runs before meshing,
//!   and a mesh job is only dispatched from `MeshState::Queued`), plus at most one light job
//!   (`ChunkSlot::light_job`).
//! - `WorldJobs::published` holds exactly the chunks whose last event was an `Upsert`, so every
//!   `Upsert` is eventually followed by a `Remove` (unload, remesh to empty) or a replacing
//!   `Upsert`.
//! - A published chunk has a `Ready` mesh, or has [`Interest::MESH`] and a remesh `Queued` or
//!   `InFlight` (so `ChunkMap::ready_meshes()` only misses published chunks during a remesh).
//! - An `Empty` mesh is never stale: a data change remeshes it, and unloading a neighbor resets
//!   it (a `Ready` mesh kept in the hysteresis band is checked by epoch when wanted again).
//! - Once the interest backlog is empty, every slot's interest is what the viewers want
//!   ([`Planner::level`]), every wanted position has a slot, and every unwanted unpinned slot is
//!   inside some viewer's keep region with data that was at least started.
use crate::interest::{Level, Planner, Viewer};
use crate::jobs::JobExecutor;
use crate::scheduler::{JobId, JobKind, JobScheduler};
use crate::sky::{
    ColumnId, SkyMap, SkyRule, SkyState, WalkJob, layer_below, may_block, padded_chunks,
};
use crate::{
    BlockChanged, BlockEdit, CachedLight, ChunkId, ChunkMap, ChunkMeshEvent, ChunkSlot, DataState,
    Interest, MeshState, WorldContent, WorldSettings, WorldStats,
};
use boxel_ecs::prelude::*;
use boxel_light::{
    ChunkLight, LayerData, LightInput, LightPath, LightScratch, PaddedLight, Rescan, SkyInput,
    SkyTile,
};
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_SHIFT, ChunkData, ChunkFaceInfo, ChunkKey, ChunkPos,
    Face, LightValue, LocalPos, NEIGHBOR_OFFSETS, OPAQUE_OPACITY, RenderShape, neighbor_index,
};
use crossbeam_channel::{Receiver, Sender};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// `neighbors_ready` with all 26 bits set.
pub(crate) const ALL_NEIGHBORS: u32 = (1 << 26) - 1;

/// An edit batch that never got a result is closed once it is this many frames old and
/// [`STALE_BATCH_TIME`] has passed since it opened (its members, waiting for neighbors or a sky
/// walk that may take long, publish on their own). Both, so a fast frame rate does not close it
/// sooner (3000 frames per second make 120 frames 40 ms, less than a busy walk).
const STALE_BATCH_FRAMES: u32 = 120;
/// See [`STALE_BATCH_FRAMES`].
const STALE_BATCH_TIME: Duration = Duration::from_secs(1);

/// Fewest frames the held results of an edit batch wait for the rest, whatever
/// `WorldSettings::edit_batch_hold` says: results held in frame `N` go out in frame
/// `N + MIN_HOLD_FRAMES` at the earliest. A late member may need two more job steps (its job
/// queued behind a full scheduler starts in the next one and arrives in the one after, and so
/// does a remesh that waited for a sky walk), and at 60 Hz or less the hold alone gives one.
const MIN_HOLD_FRAMES: u32 = 2;

/// Positions re-evaluated per frame whatever `WorldSettings::interest_budget` says (progress).
const MIN_INTEREST_STEPS: usize = 64;
/// Interest positions looked up together before their steps run (the budget is checked between
/// batches; [`MIN_INTEREST_STEPS`] is a multiple of it).
const LOOKUP_BATCH: usize = 32;

// `process_interest` marks the batch indices that created or unloaded a slot in a `u64`, and
// stops only between batches.
const _: () = assert!(
    LOOKUP_BATCH <= u64::BITS as usize && MIN_INTEREST_STEPS.is_multiple_of(LOOKUP_BATCH),
    "LOOKUP_BATCH must fit the u64 of changed indices and divide MIN_INTEREST_STEPS"
);

/// Stale job priorities refreshed per frame whatever the interest budget says (progress), and
/// the batch size of the refreshes that use the rest of the budget.
const PRIORITY_BATCH: usize = 256;

/// Added to the priority (squared chunk distance) of light jobs: they run after the mesh jobs
/// of a view of radius 16 around their viewer, so they fill the light cache when streaming
/// leaves room, without delaying it.
const LIGHT_JOB_BIAS: u32 = 16 * 16;

/// Largest `WorldSettings::light_cache_radius` (larger values are clamped): about 137 000
/// chunks within it, already far more light than is worth keeping (16 to 64 KiB per lit chunk).
pub(crate) const MAX_LIGHT_CACHE_RADIUS: u32 = 32;

/// Light cache positions checked per frame whatever the interest budget says (progress).
const MIN_LIGHT_STEPS: usize = 256;

/// Jobs started or priorities refreshed between two checks of the job budget.
const DISPATCH_CHECK: usize = 16;

/// The automatic in-flight cap keeps the workers busy for this many frames.
const AUTO_FRAMES: f64 = 1.5;

/// How far the average job cost of the automatic cap moves toward the average of one frame's
/// finished jobs (a time constant of about 8 frames). Frames with fewer than
/// [`JOB_COST_FULL_FRAME`] finished jobs move it proportionally less.
const JOB_COST_WEIGHT: f64 = 1.0 / 8.0;

/// Finished jobs for a frame to count fully in the average job cost.
const JOB_COST_FULL_FRAME: f64 = 32.0;

/// Upper bound of the automatic in-flight cap.
pub(crate) const MAX_AUTO_IN_FLIGHT: usize = 4096;

/// Index of the opposite offset: `NEIGHBOR_OFFSETS` is lexicographic in `(dy, dx, dz)` without
/// the center, so negating an offset mirrors its index.
pub(crate) const fn opposite_neighbor(i: usize) -> usize {
    25 - i
}

/// What a job sends back (exactly one per spawned job; see `jobs::JobExecutor`).
#[derive(Debug)]
pub(crate) enum JobMessage {
    /// Generation finished, with the chunk's face info (cave culling).
    Generated {
        id: ChunkId,
        data: ChunkData,
        faces: ChunkFaceInfo,
    },
    /// Meshing finished (`mesh` may be empty).
    Meshed {
        id: ChunkId,
        data_version: u32,
        epoch: u32,
        mesh: ChunkMesh,
        /// The chunk's light, when the job keeps it (within `light_cache_radius`).
        light: Option<ChunkLight>,
        /// How the light box was computed (`None`: light off).
        path: Option<LightPath>,
        /// The center's face info, when the slot had none (after an edit).
        faces: Option<ChunkFaceInfo>,
    },
    /// A light job finished.
    Lit {
        id: ChunkId,
        data_version: u32,
        epoch: u32,
        light: ChunkLight,
    },
    /// A sky walk finished.
    Walked {
        column: ColumnId,
        /// The column version the walk read.
        version: u32,
        tile: SkyTile,
        /// Chunks it generated privately.
        generated: u32,
    },
    /// The job saw the slot's cancel flag and did nothing.
    Cancelled { id: JobId, kind: JobKind },
    /// The job panicked, or was dropped without running.
    Failed {
        id: JobId,
        kind: JobKind,
        panic: String,
    },
}

/// A job's message and how long its body ran on the worker (zero when it did not run).
#[derive(Debug)]
pub(crate) struct JobReport {
    message: JobMessage,
    busy: Duration,
}

/// A `Meshed` result waiting for the per-frame event budget (or for the rest of its edit
/// batch).
#[derive(Debug)]
struct MeshedResult {
    id: ChunkId,
    data_version: u32,
    epoch: u32,
    mesh: ChunkMesh,
    light: Option<ChunkLight>,
    faces: Option<ChunkFaceInfo>,
}

/// The remeshes requested by one call of [`WorldJobs::apply_edits`] (or by the sky tiles it sent
/// back to a walk): their meshes are published in the same frame.
#[derive(Debug)]
struct EditBatch {
    /// Slots whose result has not arrived.
    members: Vec<ChunkId>,
    /// Fresh results waiting for the rest.
    held: Vec<MeshedResult>,
    /// Since when `held` waits: `Some` exactly when `held` is not empty.
    hold: Option<HoldClock>,
    /// Frames since the batch was opened.
    age: u32,
    /// The latest frame time ([`WorldJobs::begin_frame`]) when the batch was opened.
    opened: Instant,
}

impl EditBatch {
    fn new(now: Instant) -> Self {
        Self {
            members: Vec::new(),
            held: Vec::new(),
            hold: None,
            age: 0,
            opened: now,
        }
    }
}

/// How long the held results of an edit batch have waited.
#[derive(Clone, Copy, Debug)]
struct HoldClock {
    /// Frame time ([`WorldJobs::begin_frame`]) of the frame that held the first result.
    since: Instant,
    /// Frames since then ([`WorldJobs::age_batches`] steps, the holding frame's included).
    frames: u32,
}

impl HoldClock {
    /// The clock of two merged batches: the older one.
    fn older(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        match (a, b) {
            (Some(a), Some(b)) => Some(Self {
                since: a.since.min(b.since),
                frames: a.frames.max(b.frames),
            }),
            (a, b) => a.or(b),
        }
    }
}

/// The sky of a light box ([`WorldJobs::sky_for`]).
enum SkyFor {
    /// The 9 tiles around the chunk, or `None` when the box needs none (under the walk floor,
    /// or light off).
    Ready(Option<[Arc<SkyTile>; 9]>),
    /// A tile is not walked yet: the slot waits for it.
    Waiting,
}

/// Owned by every spawned job: sends the job's single message, or `Failed` from `Drop` if the
/// job ends (panic outside the guarded body, executor dropping the closure) without sending.
struct ResultToken {
    id: JobId,
    kind: JobKind,
    tx: Option<Sender<JobReport>>,
}

impl ResultToken {
    fn send(mut self, message: JobMessage, busy: Duration) {
        if let Some(tx) = self.tx.take() {
            // Err only if the world (receiver) is gone: nobody is waiting for the result.
            let _ = tx.send(JobReport { message, busy });
        }
    }
}

impl Drop for ResultToken {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(JobReport {
                message: JobMessage::Failed {
                    id: self.id,
                    kind: self.kind,
                    panic: "job dropped before sending a result".to_string(),
                },
                busy: Duration::ZERO,
            });
        }
    }
}

thread_local! {
    /// One mesher per worker thread (~0.6 MiB of scratch, reused across jobs).
    static MESHER: RefCell<Mesher> = RefCell::new(Mesher::new());
    /// One light scratch per worker thread (~0.7 MiB, reused across jobs) and the padded light
    /// of the last job.
    static LIGHT: RefCell<(LightScratch, PaddedLight)> = RefCell::default();
}

/// The light part of a mesh job, captured on the main thread.
struct LightJob {
    tables: Arc<boxel_voxel::LightTables>,
    /// The 9 tiles around the chunk (by `boxel_light::box_light::tile_index`), or `None` when
    /// the box is under the walk floor.
    tiles: Option<[Arc<SkyTile>; 9]>,
    /// Keep the chunk's light for gameplay.
    keep: bool,
}

/// Computes the light of `pos` from its 27 chunks (center at 13) into this thread's scratch and
/// runs `f` on it. A panic inside resets the scratch (its light array must be zero between
/// calls).
fn with_light<R>(
    job: &LightJob,
    pos: ChunkPos,
    chunks: [Option<&ChunkData>; 27],
    f: impl FnOnce(&PaddedLight, LightPath) -> R,
) -> R {
    /// Resets the thread's scratch unless disarmed (a panic unwound through it).
    struct Reset(bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            if self.0 {
                LIGHT.set(Default::default());
            }
        }
    }
    let mut reset = Reset(true);
    let out = LIGHT.with_borrow_mut(|(scratch, padded)| {
        let tiles = job.tiles.as_ref().map(|t| std::array::from_fn(|i| &*t[i]));
        let input = LightInput {
            origin: pos,
            chunks,
            sky: tiles.map_or(SkyInput::BelowFloor, SkyInput::Tiles),
            tables: &job.tables,
        };
        let path = boxel_light::compute(&input, scratch, padded);
        f(padded, path)
    });
    reset.0 = false;
    out
}

/// How mesh jobs turn their input into a mesh (a seam for unit tests).
pub(crate) type MeshFn = fn(&MeshInput<'_>, &MaterialTable, MeshOptions) -> ChunkMesh;

/// What [`WorldJobs::refresh_position`] did with one position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Created a slot (with a generation job, or ready air when the generator declares the
    /// layer all air).
    Create,
    /// Unloaded the slot.
    Unload,
    /// Updated the slot's interest.
    Update,
    /// Nothing there and nothing wanted.
    Skip,
}

/// Feature `trace`: time of the interest steps by [`Step`], reported as `world.step <kind>`
/// spans (`boxel_log::span_times` adds their `busy_ns`). One clock read per step; a span per
/// step would cost more than most steps.
#[cfg(feature = "trace")]
struct StepTimer {
    last: Instant,
    ns: [u64; 6],
    calls: [u64; 6],
}

#[cfg(feature = "trace")]
impl StepTimer {
    /// By `Step as usize`, then air creates, then the batches.
    const NAMES: [&'static str; 6] = ["create", "unload", "update", "skip", "create_air", "batch"];
    /// Index of a `Step::Create` of a known-air slot.
    const CREATE_AIR: usize = 4;
    /// Index of filling a batch of positions and looking them up (once per batch).
    const BATCH: usize = 5;

    fn new() -> Self {
        Self {
            last: Instant::now(),
            ns: [0; 6],
            calls: [0; 6],
        }
    }

    /// The time since the previous lap (or `new`) was spent on step kind `i` (see `NAMES`).
    fn lap(&mut self, i: usize) {
        let now = Instant::now();
        self.ns[i] += u64::try_from((now - self.last).as_nanos()).unwrap_or(u64::MAX);
        self.calls[i] += 1;
        self.last = now;
    }

    fn report(&self) {
        for (i, name) in Self::NAMES.iter().enumerate() {
            if self.calls[i] > 0 {
                tracing::info_span!(
                    "world.step",
                    name = *name,
                    busy_ns = self.ns[i],
                    calls = self.calls[i]
                );
            }
        }
    }
}

/// The production [`MeshFn`]: this thread's `boxel_mesh::Mesher`.
pub(crate) fn mesh_with_thread_mesher(
    input: &MeshInput<'_>,
    materials: &MaterialTable,
    options: MeshOptions,
) -> ChunkMesh {
    MESHER.with_borrow_mut(|mesher| mesher.mesh_chunk(input, materials, options))
}

/// The neighbors (bit `i` = `NEIGHBOR_OFFSETS[i]`) whose padded region, the chunk and its
/// 1-voxel border that their mesh reads, contains local voxel `p` of this chunk: up to 7 for a
/// corner voxel, none for an inner one (the reference of `box_chunks(p, 0)`).
#[cfg(test)]
fn padded_neighbors(p: LocalPos) -> u32 {
    if p.border_faces() == 0 {
        return 0;
    }
    let reaches = |c: u32, d: i32| d == 0 || (d < 0 && c == 0) || (d > 0 && c == 31);
    NEIGHBOR_OFFSETS
        .iter()
        .enumerate()
        .filter(|(_, o)| reaches(p.x(), o.x) && reaches(p.y(), o.y) && reaches(p.z(), o.z))
        .fold(0, |bits, (i, _)| bits | 1 << i)
}

/// Text of a panic payload.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Spawns `body` on `executor` under the result contract: cancelled jobs send `Cancelled`, a
/// panicking body sends `Failed`, a dropped job sends `Failed` through the token. The report
/// carries how long the body ran.
fn spawn_job(
    executor: &dyn JobExecutor,
    tx: &Sender<JobReport>,
    id: JobId,
    kind: JobKind,
    cancel: Arc<AtomicBool>,
    body: impl FnOnce() -> JobMessage + Send + 'static,
) {
    let token = ResultToken {
        id,
        kind,
        tx: Some(tx.clone()),
    };
    executor.spawn(Box::new(move || {
        if cancel.load(Ordering::Relaxed) {
            token.send(JobMessage::Cancelled { id, kind }, Duration::ZERO);
            return;
        }
        let start = Instant::now();
        let message = match catch_unwind(AssertUnwindSafe(body)) {
            Ok(message) => message,
            Err(payload) => JobMessage::Failed {
                id,
                kind,
                panic: panic_text(payload.as_ref()),
            },
        };
        token.send(message, start.elapsed());
    }));
}

/// Every `ChunkViewer` as `(entity bits, viewer)`, sorted by entity; refreshed every frame from
/// `Transform` + `ChunkViewer` (only written when it changes).
#[derive(Resource, Debug, Default)]
pub(crate) struct ViewerChunks(pub(crate) Vec<(u64, Viewer)>);

/// What the state machine works on during one system run: the map, the content and the
/// `ChunkMeshEvent`s to publish (written by the calling system).
pub(crate) struct Ctx<'a> {
    pub(crate) map: &'a mut ChunkMap,
    pub(crate) sky: &'a mut SkyMap,
    pub(crate) content: &'a WorldContent,
    pub(crate) events: &'a mut Vec<ChunkMeshEvent>,
    generator_layers: Option<RangeInclusive<i64>>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(
        map: &'a mut ChunkMap,
        sky: &'a mut SkyMap,
        content: &'a WorldContent,
        events: &'a mut Vec<ChunkMeshEvent>,
    ) -> Self {
        Self {
            map,
            sky,
            content,
            events,
            generator_layers: None,
        }
    }

    /// True if the generator declares layer `y` all air (outside its `vertical_chunks()`, queried
    /// once per run): such chunks are created as air without a generation job.
    fn known_air(&mut self, y: i64) -> bool {
        !self
            .generator_layers
            .get_or_insert_with(|| self.content.generator.vertical_chunks())
            .contains(&y)
    }
}

/// Sizes the in-flight cap when `WorldSettings::max_jobs_in_flight` is 0: enough jobs to keep
/// every async compute worker busy for [`AUTO_FRAMES`] frames, from moving averages of the job
/// cost (measured on the workers) and of the frame interval.
///
/// The job cost is averaged per frame first, then across frames: results arrive in no useful
/// order (cheap generations and expensive meshes mixed), so a per-job average would follow the
/// last few results of each frame and make the cap jump from frame to frame.
#[derive(Debug)]
pub(crate) struct AutoCap {
    workers: usize,
    /// Average seconds a job body runs.
    job: f64,
    /// Average seconds between frames.
    frame: f64,
    last_frame: Option<Instant>,
    /// Seconds of the jobs finished since the frame started, and how many.
    frame_busy: f64,
    frame_jobs: u32,
}

impl AutoCap {
    /// Before any measurement: 50 us per job, 60 Hz.
    pub(crate) fn new(workers: usize) -> Self {
        Self {
            workers: workers.max(1),
            job: 50e-6,
            frame: 1.0 / 60.0,
            last_frame: None,
            frame_busy: 0.0,
            frame_jobs: 0,
        }
    }

    /// A finished job body ran for `busy` (samples clamped to [1 us, 20 ms]). Counted at the
    /// start of the next frame.
    fn observe_job(&mut self, busy: Duration) {
        self.frame_busy += busy.as_secs_f64().clamp(1e-6, 20e-3);
        self.frame_jobs = self.frame_jobs.saturating_add(1);
    }

    /// A frame starts at `now`: folds the jobs of the previous frame into the job cost, and the
    /// interval into the frame time (clamped to [50 us, 33 ms]: a longer one is a hitch, not the
    /// frame rate).
    fn observe_frame(&mut self, now: Instant) {
        if self.frame_jobs > 0 {
            let jobs = f64::from(self.frame_jobs);
            let weight = JOB_COST_WEIGHT * (jobs / JOB_COST_FULL_FRAME).min(1.0);
            self.job += (self.frame_busy / jobs - self.job) * weight;
            self.frame_busy = 0.0;
            self.frame_jobs = 0;
        }
        if let Some(last) = self.last_frame {
            let sample = now
                .saturating_duration_since(last)
                .as_secs_f64()
                .clamp(50e-6, 33e-3);
            self.frame += (sample - self.frame) / 8.0;
        }
        self.last_frame = Some(now);
    }

    /// The cap for the current averages.
    pub(crate) fn cap(&self) -> usize {
        auto_cap(self.workers, self.frame, self.job)
    }
}

/// `workers x ceil(AUTO_FRAMES x frame / job)`, at least 2 per worker, at most
/// [`MAX_AUTO_IN_FLIGHT`] (times in seconds).
pub(crate) fn auto_cap(workers: usize, frame: f64, job: f64) -> usize {
    let workers = workers.max(1);
    let per_worker = (AUTO_FRAMES * frame / job.max(1e-7)).ceil().max(0.0);
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "clamped to MAX_AUTO_IN_FLIGHT first; worker counts are small"
    )]
    let n = (workers as f64 * per_worker).min(MAX_AUTO_IN_FLIGHT as f64) as usize;
    n.clamp(2 * workers, MAX_AUTO_IN_FLIGHT.max(2 * workers))
}

/// Private job state of the world (scheduler, result channel, executor, event bookkeeping).
#[derive(Resource)]
pub(crate) struct WorldJobs {
    pub(crate) scheduler: JobScheduler,
    tx: Sender<JobReport>,
    rx: Receiver<JobReport>,
    executor: Arc<dyn JobExecutor>,
    /// Mesh job body (always [`mesh_with_thread_mesher`] outside unit tests).
    pub(crate) mesh_fn: MeshFn,
    /// Mesher options of new mesh jobs (from `WorldSettings::ao_policy`, every frame).
    mesh_options: MeshOptions,
    /// `WorldSettings::light_cache_radius`, squared (every frame).
    light_cache_sq: u32,
    /// `WorldSettings::edit_batch_hold` (every frame).
    hold: Duration,
    /// Time of the current frame (given to [`WorldJobs::begin_frame`]): the clock of the edit
    /// batch hold.
    now: Instant,
    /// Tests: the time [`WorldJobs::clock_now`] gives the world plugin's frames instead of the
    /// real clock.
    #[cfg(test)]
    pub(crate) manual_clock: Option<Instant>,
    /// Open edit batches by id.
    batches: BTreeMap<u32, EditBatch>,
    /// Id of the next edit batch.
    next_batch: u32,
    /// Light counters for [`WorldStats`] (`waiting_sky` is a gauge, the rest add up).
    light_counts: WorldStats,
    /// Viewer chunks (by viewer id) and light cache radius of the last light cache plan.
    light_plan: Option<(Vec<(u64, ChunkPos)>, u32)>,
    /// Positions whose distance to the viewers may have crossed the light cache radius since
    /// the last plan: their slot fills or drops its cached light.
    light_backlog: Vec<ChunkPos>,
    /// Mesh jobs started per chunk position (tests: no light ripples).
    #[cfg(test)]
    pub(crate) mesh_jobs: std::collections::HashMap<ChunkPos, u32>,
    /// Mesh results waiting for the event budget (`scheduler.complete()` already called; their
    /// slots stay `InFlight` until applied, so the world is not settled while any wait).
    pending: VecDeque<MeshedResult>,
    /// Mesh results of slots in an edit batch: applied before `pending`, whatever the event
    /// budget (a batch is published as a whole anyway), so a streaming backlog never delays an
    /// edit.
    batch_pending: Vec<MeshedResult>,
    /// A slot whose result may wait in `pending` joined an edit batch: `pending` is searched for
    /// batch results once before the next results are applied.
    rescan_pending: bool,
    /// Chunks whose last published event was an `Upsert`, with that mesh (an identical new
    /// mesh is not sent again).
    published: std::collections::HashMap<ChunkId, Arc<ChunkMesh>, foldhash::fast::FixedState>,
    /// Viewers, interest settings and the interest work not done yet.
    pub(crate) planner: Planner,
    /// The automatic in-flight cap.
    pub(crate) auto_cap: AutoCap,
    /// One shared snapshot per uniform state (uniform chunks are most of a tall world).
    uniform: Vec<(BlockState, Arc<ChunkData>)>,
    /// Positions re-evaluated by the interest since start (tests and benchmarks).
    pub(crate) interest_steps: usize,
    /// Positions looked up again because an earlier step of their batch created or unloaded a
    /// slot (tests check that path runs).
    #[cfg(test)]
    pub(crate) relookups: usize,
    /// Failed jobs since start (the `failed=` field of the `"world"` board line).
    pub(crate) failed: usize,
    /// Chunks whose failure was already error-logged.
    failure_logged: HashSet<ChunkId, foldhash::fast::FixedState>,
}

impl WorldJobs {
    /// Empty state with `max_in_flight` job slots (the cap until the first frame sets one) and
    /// `workers` async compute threads (for the automatic cap).
    pub(crate) fn new(
        max_in_flight: usize,
        workers: usize,
        executor: Arc<dyn JobExecutor>,
    ) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            scheduler: JobScheduler::new(max_in_flight),
            tx,
            rx,
            executor,
            mesh_fn: mesh_with_thread_mesher,
            mesh_options: MeshOptions::default(),
            // `WorldSettings::default()` until the first `begin_frame`.
            light_cache_sq: 8 * 8,
            hold: WorldSettings::DEFAULT_EDIT_BATCH_HOLD,
            now: Instant::now(),
            #[cfg(test)]
            manual_clock: None,
            batches: BTreeMap::new(),
            next_batch: 0,
            light_counts: WorldStats::default(),
            light_plan: None,
            light_backlog: Vec::new(),
            #[cfg(test)]
            mesh_jobs: std::collections::HashMap::new(),
            pending: VecDeque::new(),
            batch_pending: Vec::new(),
            rescan_pending: false,
            published: std::collections::HashMap::default(),
            planner: Planner::default(),
            auto_cap: AutoCap::new(workers),
            uniform: Vec::new(),
            interest_steps: 0,
            #[cfg(test)]
            relookups: 0,
            failed: 0,
            failure_logged: HashSet::default(),
        }
    }

    /// Priority of a new job for `pos` (nearest current viewer).
    fn priority(&self, pos: ChunkPos) -> u32 {
        self.planner.priority(pos)
    }

    /// True if jobs are queued or results wait in the channel.
    pub(crate) fn has_job_work(&self) -> bool {
        self.scheduler.queued() > 0 || !self.rx.is_empty()
    }

    /// True if mesh results wait for the event budget, or an edit batch is open (its results
    /// may be held, and it ages every frame).
    pub(crate) fn has_meshes(&self) -> bool {
        !self.pending.is_empty() || !self.batch_pending.is_empty() || !self.batches.is_empty()
    }

    /// Mesh results waiting to be applied (tests).
    #[cfg(test)]
    pub(crate) fn pending_meshes(&self) -> usize {
        self.pending.len() + self.batch_pending.len()
    }

    /// Copies the light counters into `stats`.
    pub(crate) fn light_stats(&self, stats: &mut WorldStats) {
        let c = &self.light_counts;
        stats.light_full = c.light_full;
        stats.light_fast = c.light_fast;
        stats.light_jobs = c.light_jobs;
        stats.sky_walks = c.sky_walks;
        stats.sky_private_generations = c.sky_private_generations;
        stats.waiting_sky = c.waiting_sky;
        stats.edit_batches_held = c.edit_batches_held;
        stats.edit_batches_split = c.edit_batches_split;
        stats.light_fanout = c.light_fanout;
    }

    /// One shared snapshot of a uniform chunk of `state`.
    fn uniform(&mut self, state: BlockState) -> Arc<ChunkData> {
        if let Some((_, data)) = self.uniform.iter().find(|(s, _)| *s == state) {
            return Arc::clone(data);
        }
        let data = Arc::new(ChunkData::uniform(state));
        self.uniform.push((state, Arc::clone(&data)));
        data
    }

    /// `data` as a snapshot: uniform chunks share one per state.
    fn snapshot(&mut self, data: ChunkData) -> Arc<ChunkData> {
        match data.as_uniform() {
            Some(state) if data == ChunkData::uniform(state) => self.uniform(state),
            _ => Arc::new(data),
        }
    }

    // ---- Interest ----------------------------------------------------------------------------

    /// True if the viewers or the interest settings changed since the last plan, or earlier
    /// interest work (positions, stale priorities, light cache positions) is still pending.
    pub(crate) fn interest_pending(
        &self,
        viewers: &[(u64, Viewer)],
        settings: &WorldSettings,
    ) -> bool {
        self.planner.backlog() > 0
            || self.scheduler.stale() > 0
            || !self.light_backlog.is_empty()
            || self.light_plan_changed(viewers, light_cache_radius(settings))
            || self.planner.changed(
                viewers,
                settings.unload_hysteresis,
                settings.vertical_bounds.as_ref(),
            )
    }

    /// Viewer changes not applied yet (positions and work lists).
    pub(crate) fn interest_backlog(&self) -> usize {
        self.planner.backlog()
    }

    /// Plans the work of a viewer or settings change, then applies interest work within
    /// `budget` (`settings.interest_budget`, or more in a capped frame with time to spare).
    pub(crate) fn update_interest(
        &mut self,
        cx: &mut Ctx<'_>,
        settings: &WorldSettings,
        viewers: &[(u64, Viewer)],
        budget: Duration,
    ) {
        {
            let _span = frame_span!("world.plan");
            self.plan_interest(cx, settings, viewers);
            self.plan_light(cx, settings, viewers);
        }
        self.process_interest(cx, settings, budget);
    }

    /// True if the viewers' chunks or the light cache radius changed since the last light plan.
    fn light_plan_changed(&self, viewers: &[(u64, Viewer)], radius: u32) -> bool {
        self.light_plan.as_ref().is_none_or(|(centers, r)| {
            *r != radius
                || centers.len() != viewers.len()
                || centers
                    .iter()
                    .zip(viewers)
                    .any(|(&(a, c), &(b, v))| a != b || c != v.center)
        })
    }

    /// Lists the positions whose distance to the viewers crossed the light cache radius since
    /// the last light plan (into a sphere or out of all of them): `refresh_light_cache` then
    /// fills or drops their light. A move lists two shells, a new viewer or radius whole
    /// spheres.
    fn plan_light(&mut self, cx: &Ctx<'_>, settings: &WorldSettings, viewers: &[(u64, Viewer)]) {
        let radius = light_cache_radius(settings);
        self.light_cache_sq = radius * radius;
        if !self.light_plan_changed(viewers, radius) {
            return;
        }
        let new: Vec<(u64, ChunkPos)> = viewers.iter().map(|&(id, v)| (id, v.center)).collect();
        let (old, old_radius) = self
            .light_plan
            .replace((new.clone(), radius))
            .unwrap_or_default();
        if cx.sky.rule == SkyRule::Off {
            return;
        }
        let out = &mut self.light_backlog;
        let centers = |list: &[(u64, ChunkPos)]| list.iter().map(|&(_, c)| c).collect::<Vec<_>>();
        let (old_centers, new_centers) = (centers(&old), centers(&new));
        let outside = |p: ChunkPos, centers: &[ChunkPos], r: u32| {
            centers
                .iter()
                .all(|c| p.distance_sq(*c) > u128::from(r) * u128::from(r))
        };
        // Viewers paired by id (both lists are sorted by id).
        let (mut i, mut j) = (0, 0);
        while i < old.len() || j < new.len() {
            let (o, n) = (old.get(i), new.get(j));
            let (from, to) = match (o, n) {
                (Some(&(a, _)), Some(&(b, _))) if a == b => {
                    i += 1;
                    j += 1;
                    (o.map(|o| o.1), n.map(|n| n.1))
                }
                (Some(&(a, _)), Some(&(b, _))) if a < b => {
                    i += 1;
                    (o.map(|o| o.1), None)
                }
                (Some(_), None) => {
                    i += 1;
                    (o.map(|o| o.1), None)
                }
                _ => {
                    j += 1;
                    (None, n.map(|n| n.1))
                }
            };
            if from == to && radius == old_radius {
                continue;
            }
            if let Some(to) = to {
                sphere_minus(to, radius, from.map(|f| (f, old_radius)), |p| {
                    if outside(p, &old_centers, old_radius) {
                        out.push(p);
                    }
                });
            }
            if let Some(from) = from {
                sphere_minus(from, old_radius, to.map(|t| (t, radius)), |p| {
                    if outside(p, &new_centers, radius) {
                        out.push(p);
                    }
                });
            }
        }
    }

    /// Fills or drops the cached light of the listed positions (at least [`MIN_LIGHT_STEPS`],
    /// then until `budget` after `start`): a slot within the light cache radius gets a light
    /// job if its light is not current, one outside it drops its light.
    fn refresh_light_cache(&mut self, cx: &mut Ctx<'_>, start: Instant, budget: Duration) {
        let mut steps = 0usize;
        while let Some(p) = self.light_backlog.pop() {
            if let Some(id) = cx.map.id_at(p) {
                if self.planner.priority(p) <= self.light_cache_sq {
                    self.want_light(cx, id);
                } else {
                    cx.map.set_light(id, None);
                }
            }
            steps += 1;
            if steps >= MIN_LIGHT_STEPS
                && steps.is_multiple_of(DISPATCH_CHECK)
                && start.elapsed() >= budget
            {
                break;
            }
        }
    }

    /// Records new viewers or settings: lists the positions to re-evaluate (nothing is applied
    /// yet) and marks queued job priorities stale.
    pub(crate) fn plan_interest(
        &mut self,
        cx: &mut Ctx<'_>,
        settings: &WorldSettings,
        viewers: &[(u64, Viewer)],
    ) {
        let bounds_changed =
            self.planner.is_planned() && self.planner.bounds() != settings.vertical_bounds.as_ref();
        let map = &*cx.map;
        let drift = self.planner.update(
            viewers,
            settings.unload_hysteresis,
            settings.vertical_bounds.as_ref(),
            || map.slots.values().map(|s| s.pos).collect(),
        );
        if let Some(drift) = drift {
            self.scheduler.invalidate_priorities(drift);
            // Room for the view twice: after a jump the old view stays loaded until the new one
            // is created, and growing the map then would rehash it inside a loading frame.
            // Reserving is cheap while the map is small (the first plan); a larger view (radius,
            // new viewer) grows it here, in the frame of the change.
            let room = self
                .planner
                .capacity_hint()
                .saturating_mul(2)
                .saturating_sub(cx.map.slots.len());
            cx.map.reserve(room);
        }
        if bounds_changed {
            self.refresh_neighbor_bits(cx);
        }
    }

    /// The vertical bounds changed (a rare settings change): neighbor bits follow the new bounds,
    /// and every slot whose bits changed has its mesh redone, since its neighbors are no longer
    /// what the mesh was built from. Walks every slot once. Slots still waiting for their data
    /// are skipped: their bits are computed when it arrives. Their epoch is not bumped either,
    /// unlike a walk of every slot (which bumps it where the bounds change their bits): such a
    /// slot has no mesh built or building (`None` or `Queued`), so no recorded epoch exists that
    /// a bump would invalidate. Skipping them changes the value of their epoch, never what is
    /// meshed or published.
    fn refresh_neighbor_bits(&mut self, cx: &mut Ctx<'_>) {
        let ids: Vec<ChunkId> = cx
            .map
            .slots
            .iter()
            .filter(|(_, slot)| matches!(slot.data(), DataState::Ready(_) | DataState::Missing))
            .map(|(id, _)| id)
            .collect();
        for id in ids {
            let pos = cx.map.slots[id].pos;
            let bits = own_neighbor_bits(cx.map, &self.planner, pos);
            let slot = &mut cx.map.slots[id];
            if slot.neighbors_ready == bits {
                continue;
            }
            slot.neighbors_ready = bits;
            slot.epoch = slot.epoch.wrapping_add(1);
            if !matches!(slot.mesh(), MeshState::None) {
                let priority = self.priority(pos);
                self.request_remesh(cx, id, priority);
            }
        }
    }

    /// Applies planned interest work for about `budget`: re-evaluates listed positions (at least
    /// [`MIN_INTEREST_STEPS`]), then refreshes stale job priorities (one batch whatever the
    /// budget, more while it lasts), then fills or drops the light cache of the positions that
    /// crossed its radius (at least [`MIN_LIGHT_STEPS`]). Positions go first so the refresh
    /// drops the jobs of the slots they unloaded.
    pub(crate) fn process_interest(
        &mut self,
        cx: &mut Ctx<'_>,
        settings: &WorldSettings,
        budget: Duration,
    ) {
        let start = Instant::now();
        let mut steps = 0usize;
        {
            let _span = frame_span!("world.positions");
            #[cfg(feature = "trace")]
            let mut timer = StepTimer::new();
            let mut batch = [(ChunkPos::default(), None); LOOKUP_BATCH];
            loop {
                let mut len = 0;
                while len < LOOKUP_BATCH
                    && let Some(pos) = self.planner.next()
                {
                    batch[len] = (pos, None);
                    len += 1;
                }
                if len == 0 {
                    break;
                }
                // Every position of the batch is looked up (and its slot read) before any step
                // runs: the lookups do not depend on each other, so their cache misses overlap
                // instead of stalling one step at a time.
                for (pos, id) in &mut batch[..len] {
                    *id = cx.map.id_at(*pos);
                    if let Some(id) = *id {
                        std::hint::black_box(cx.map.slots[id].interest);
                    }
                }
                #[cfg(feature = "trace")]
                timer.lap(StepTimer::BATCH);
                // Batch indices whose step created or unloaded a slot: a position listed twice
                // in one batch is looked up again after them.
                let mut changed = 0u64;
                for i in 0..len {
                    let (pos, mut id) = batch[i];
                    let mut earlier = changed;
                    while earlier != 0 {
                        let j = earlier.trailing_zeros() as usize;
                        earlier &= earlier - 1;
                        if batch[j].0 == pos {
                            id = cx.map.id_at(pos);
                            #[cfg(test)]
                            {
                                self.relookups += 1;
                            }
                            break;
                        }
                    }
                    let step = self.refresh_position(cx, settings, pos, id);
                    if matches!(step, Step::Create | Step::Unload) {
                        changed |= 1 << i;
                    }
                    #[cfg(feature = "trace")]
                    timer.lap(match step {
                        Step::Create if cx.known_air(pos.y) => StepTimer::CREATE_AIR,
                        step => step as usize,
                    });
                }
                steps += len;
                if steps >= MIN_INTEREST_STEPS && start.elapsed() >= budget {
                    break;
                }
            }
            #[cfg(feature = "trace")]
            timer.report();
        }
        self.interest_steps += steps;
        {
            let _span = frame_span!("world.priorities");
            self.refresh_priorities(cx, PRIORITY_BATCH);
            while self.scheduler.stale() > 0 && start.elapsed() < budget {
                self.refresh_priorities(cx, PRIORITY_BATCH);
            }
        }
        let _span = frame_span!("world.light_cache");
        self.refresh_light_cache(cx, start, budget);
    }

    /// Recomputes up to `max` stale job priorities for the current viewers (jobs whose slot no
    /// longer wants them are dropped).
    fn refresh_priorities(&mut self, cx: &Ctx<'_>, max: usize) {
        let (map, sky, planner) = (&*cx.map, &*cx.sky, &self.planner);
        self.scheduler.refresh_priorities(max, |id, kind| {
            current_priority(map, sky, planner, id, kind)
        });
    }

    /// Brings the slot at `pos` (`id`, looked up by the caller) in line with the current viewers:
    /// creates it if wanted and missing, updates its interest, or unloads it (unwanted, unpinned,
    /// and either never started or outside every keep region). Returns what it did.
    fn refresh_position(
        &mut self,
        cx: &mut Ctx<'_>,
        settings: &WorldSettings,
        pos: ChunkPos,
        id: Option<ChunkId>,
    ) -> Step {
        debug_assert_eq!(id, cx.map.id_at(pos), "stale lookup of {pos:?}");
        let level = self.planner.level(pos);
        let want = level.interest();
        match id {
            Some(id) => {
                let slot = &cx.map.slots[id];
                let pinned = slot.interest & Interest::PINNED;
                if want.is_empty()
                    && pinned.is_empty()
                    && (matches!(slot.data(), DataState::Queued) || level == Level::Out)
                {
                    // Unwanted data that was never started is dropped right away; anything else
                    // waits for the hysteresis band.
                    self.unload(cx, id);
                    Step::Unload
                } else {
                    self.set_interest(cx, id, want | pinned);
                    Step::Update
                }
            }
            None if !want.is_empty() => {
                self.create_slot(cx, settings, pos, want);
                Step::Create
            }
            None => Step::Skip,
        }
    }

    /// New slot at `pos`: data `Queued` with a generation job, or `Ready` air at once if the
    /// generator declares the layer all air (then its 26 neighbors are looked up once, for its
    /// own bits and for theirs). A `Queued` slot's own neighbor bits are not looked up: they are
    /// computed when its data arrives.
    pub(crate) fn create_slot(
        &mut self,
        cx: &mut Ctx<'_>,
        settings: &WorldSettings,
        pos: ChunkPos,
        interest: Interest,
    ) -> ChunkId {
        let near = cx.known_air(pos.y).then(|| neighbor_ids(cx.map, pos));
        let column = cx.sky.attach(pos, settings.dimension);
        let slot = ChunkSlot::new(
            ChunkKey {
                dim: settings.dimension,
                lod: 0,
                pos,
            },
            column,
        );
        let id = cx.map.insert(slot);
        // Gaining mesh interest only queues the mesh here (the data is not ready yet): no slot
        // comes or goes, so `near` stays valid.
        self.set_interest(cx, id, interest);
        if let Some(near) = near {
            cx.map
                .set_data(id, DataState::Ready(self.uniform(BlockState::AIR)));
            cx.map.set_faces(id, Some(ChunkFaceInfo::OPEN));
            self.data_available_near(cx, id, pos, &near);
        } else {
            self.scheduler
                .push(id, JobKind::Generate, self.priority(pos));
        }
        id
    }

    /// Sets a slot's interest and reacts to gaining or losing [`Interest::MESH`].
    fn set_interest(&mut self, cx: &mut Ctx<'_>, id: ChunkId, interest: Interest) {
        let slot = &mut cx.map.slots[id];
        let had_mesh = slot.interest.contains(Interest::MESH);
        slot.interest = interest;
        match (had_mesh, interest.contains(Interest::MESH)) {
            (true, false) => {
                // Ready/Empty meshes stay until unload (hysteresis band). A remesh that is still
                // queued or running is no longer wanted: the old mesh goes now (`Remove`), so the
                // events keep agreeing with `ready_meshes()`. A running job keeps the slot
                // `InFlight` (one job per slot); its result is dropped when it arrives.
                cx.map.set_remesh_pending(id, false);
                match cx.map.slots[id].mesh() {
                    MeshState::Queued => self.drop_mesh(cx, id),
                    MeshState::InFlight { .. } => self.unpublish(cx, id),
                    MeshState::None | MeshState::Ready { .. } | MeshState::Empty => {}
                }
            }
            (false, true) => {
                let priority = self.priority(slot.pos);
                match slot.mesh() {
                    MeshState::None => {
                        cx.map.set_mesh(id, MeshState::Queued);
                        self.schedule_mesh(cx, id, priority);
                    }
                    MeshState::Ready {
                        data_version,
                        epoch,
                        ..
                    } => {
                        let current = match slot.data() {
                            DataState::Ready(d) => Some(d.version),
                            _ => None,
                        };
                        if current != Some(*data_version) || slot.epoch != *epoch {
                            // Built before a neighbor was unloaded or the data changed.
                            self.request_remesh(cx, id, priority);
                        }
                    }
                    MeshState::Queued | MeshState::InFlight { .. } | MeshState::Empty => {}
                }
                // A current mesh near a viewer whose light was not kept.
                self.want_light(cx, id);
            }
            _ => {}
        }
    }

    /// Removes a slot: cancels its job, publishes `Remove` if its mesh was published, clears its
    /// bit in the 26 neighbors and bumps their epoch. A neighbor with an `Empty` mesh forgets it
    /// (it may have been empty only because of this chunk, which may come back different).
    ///
    /// A slot outside the vertical bounds (left behind by a bounds change) becomes air for its
    /// neighbors instead: their bit stays set and the ones with mesh interest are remeshed.
    pub(crate) fn unload(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        self.leave_batch(cx, id);
        self.set_waits_sky(cx, id, false);
        let Some(slot) = cx.map.remove(id) else {
            return;
        };
        slot.cancel.store(true, Ordering::Relaxed);
        self.failure_logged.remove(&id);
        if self.published.remove(&id).is_some() {
            cx.events.push(ChunkMeshEvent::Remove {
                id,
                key: slot.key(),
            });
        }
        self.column_lost_slot(cx, &slot, id);
        let outside = !self.planner.in_bounds(slot.pos.y);
        // All 26 lookups first: independent, so their cache misses overlap.
        let near = neighbor_ids(cx.map, slot.pos);
        for (i, &n) in near.iter().enumerate() {
            let Some(n) = n else {
                continue;
            };
            let neighbor = &mut cx.map.slots[n];
            let bit = 1 << opposite_neighbor(i);
            if outside {
                neighbor.neighbors_ready |= bit;
            } else {
                neighbor.neighbors_ready &= !bit;
            }
            neighbor.epoch = neighbor.epoch.wrapping_add(1);
            if matches!(neighbor.mesh(), MeshState::Empty)
                || (outside && neighbor.interest.contains(Interest::MESH))
            {
                // Nothing is published for an `Empty` mesh. Without mesh interest (the usual
                // case: a mesh chunk's neighbors are always wanted) this resets it to `None`, so
                // it is meshed again when wanted; a `Ready` mesh is checked by epoch instead.
                // (Remeshing adds or removes no slot: `near` stays valid.)
                let priority = self.priority(slot.pos.offset(NEIGHBOR_OFFSETS[i]));
                self.request_remesh(cx, n, priority);
            }
        }
    }

    // ---- Mesh bookkeeping --------------------------------------------------------------------

    /// For a slot in `MeshState::Queued` that wants a mesh: resolves the empty fast paths that
    /// need no neighbors (`Missing` data, uniform air), or queues a mesh job once its data is
    /// `Ready`, all 26 neighbors are ready and, when its light box may hold direct sky, the sky
    /// tiles of its 3x3 columns are walked. Otherwise it waits (it is scheduled again when the
    /// last neighbor becomes ready, or when the tile lands).
    fn schedule_mesh(&mut self, cx: &mut Ctx<'_>, id: ChunkId, priority: u32) {
        self.set_waits_sky(cx, id, false);
        let slot = &cx.map.slots[id];
        if !matches!(slot.mesh(), MeshState::Queued) || !slot.interest.contains(Interest::MESH) {
            return;
        }
        match slot.data() {
            DataState::Missing => self.set_mesh_empty(cx, id),
            DataState::Ready(data) if is_uniform_empty(data, &cx.content.registry) => {
                self.set_mesh_empty(cx, id);
            }
            DataState::Ready(_) if slot.neighbors_ready == ALL_NEIGHBORS => {
                if matches!(self.sky_for(cx, id, false), SkyFor::Ready(_)) {
                    self.scheduler.push(id, JobKind::Mesh, priority);
                }
            }
            DataState::Ready(_) | DataState::Queued | DataState::Generating => {}
        }
    }

    /// The sky of the light box of slot `id` (without `collect`, only whether it is ready:
    /// `Ready(None)`). When a tile of its 3x3 columns is not walked yet, the slot waits for it
    /// ([`SkyFor::Waiting`]) and the walk is requested.
    fn sky_for(&mut self, cx: &mut Ctx<'_>, id: ChunkId, collect: bool) -> SkyFor {
        let pos = cx.map.slots[id].pos;
        match cx.sky.rule {
            SkyRule::Off => return SkyFor::Ready(None),
            SkyRule::Open => {
                let open = &cx.sky.open;
                return SkyFor::Ready(collect.then(|| std::array::from_fn(|_| Arc::clone(open))));
            }
            SkyRule::Walk(_) if !cx.sky.needs_tiles(pos) => return SkyFor::Ready(None),
            SkyRule::Walk(_) => {}
        }
        let mut tiles: [Option<Arc<SkyTile>>; 9] = Default::default();
        let mut waiting = false;
        for (i, tile) in (0i64..).zip(tiles.iter_mut()) {
            // `boxel_light::box_light::tile_index`: (dx + 1) * 3 + (dz + 1).
            let (dx, dz) = (i / 3 - 1, i % 3 - 1);
            let Some(col) = cx.sky.id_at(pos.x + dx, pos.z + dz) else {
                // A chunk with its 26 neighbors loaded has every column around it.
                debug_assert!(false, "no sky column next to {pos:?}");
                *tile = Some(Arc::clone(&cx.sky.open));
                continue;
            };
            let column = &mut cx.sky.columns[col];
            if let Some(exact) = column.exact() {
                if collect {
                    *tile = Some(Arc::clone(exact));
                }
                continue;
            }
            waiting = true;
            if !column.waiters.contains(&id) {
                column.waiters.push(id);
            }
            let priority = self.planner.column_priority(column.x, column.z);
            self.request_walk(cx.sky, col, priority);
        }
        if waiting {
            self.set_waits_sky(cx, id, true);
            return SkyFor::Waiting;
        }
        SkyFor::Ready(collect.then(|| tiles.map(|t| t.expect("every tile was set"))))
    }

    /// Queues the walk of column `col` if it is `Pending`, not queued and not waiting for data.
    fn request_walk(&mut self, sky: &mut SkyMap, col: ColumnId, priority: u32) {
        let Some(c) = sky.columns.get_mut(col) else {
            return;
        };
        if matches!(c.state, SkyState::Pending { .. }) && !c.queued && c.waiting_for.is_none() {
            c.queued = true;
            self.scheduler.push(col, JobKind::SkyTile, priority);
        }
    }

    /// Marks whether slot `id` waits for a sky tile ([`WorldStats::waiting_sky`]).
    fn set_waits_sky(&mut self, cx: &mut Ctx<'_>, id: ChunkId, waits: bool) {
        if let Some(slot) = cx.map.slots.get_mut(id)
            && slot.waits_sky != waits
        {
            slot.waits_sky = waits;
            let gauge = &mut self.light_counts.waiting_sky;
            *gauge = if waits { *gauge + 1 } else { *gauge - 1 };
        }
    }

    /// The data of slot `id` arrived (or the slot is gone): a walk of its column that waited for
    /// it is queued again.
    fn walk_data_arrived(&mut self, cx: &mut Ctx<'_>, col: ColumnId, id: ChunkId) {
        let Some(c) = cx.sky.columns.get_mut(col) else {
            return;
        };
        if c.waiting_for == Some(id) {
            c.waiting_for = None;
            let priority = self.planner.column_priority(c.x, c.z);
            self.request_walk(cx.sky, col, priority);
        }
    }

    /// Slot `slot` (id `id`) was removed: its column loses it (and goes with its last slot). A
    /// chunk whose edits moved a sky top takes them with it (nothing is persisted yet), so its
    /// column is walked again.
    fn column_lost_slot(&mut self, cx: &mut Ctx<'_>, slot: &ChunkSlot, id: ChunkId) {
        let col = slot.column;
        self.walk_data_arrived(cx, col, id);
        if cx.sky.detach(col, slot.pos.y) || !slot.sky_edited() {
            return;
        }
        let c = &mut cx.sky.columns[col];
        c.version = c.version.wrapping_add(1);
        if let SkyState::Exact(tile) = &c.state {
            c.state = SkyState::Pending {
                old: Some(Arc::clone(tile)),
            };
        }
        let priority = self.planner.column_priority(c.x, c.z);
        self.request_walk(cx.sky, col, priority);
    }

    // ---- Edit batches ------------------------------------------------------------------------

    /// A new, empty edit batch.
    fn new_batch(&mut self) -> u32 {
        let b = self.next_batch;
        self.next_batch = self.next_batch.wrapping_add(1);
        self.batches.insert(b, EditBatch::new(self.now));
        b
    }

    /// Slot `id` (with mesh interest) is remeshed as part of batch `b`. A slot already in an open
    /// batch merges that batch into `b`: neither is published without the other.
    fn join_batch(&mut self, cx: &mut Ctx<'_>, b: u32, id: ChunkId) {
        let slot = &mut cx.map.slots[id];
        match slot.batch {
            Some(old) if old == b && self.batches.contains_key(&b) => return,
            Some(old) if old != b => {
                if let Some(merged) = self.batches.remove(&old) {
                    for &m in merged
                        .members
                        .iter()
                        .chain(merged.held.iter().map(|r| &r.id))
                    {
                        if let Some(s) = cx.map.slots.get_mut(m) {
                            s.batch = Some(b);
                        }
                    }
                    let now = self.now;
                    let batch = self.batches.entry(b).or_insert_with(|| EditBatch::new(now));
                    batch.members.extend(merged.members);
                    // The batch waits from the oldest held result of the two (it keeps its own
                    // age: the merged batch now waits for its members too).
                    batch.hold = HoldClock::older(batch.hold, merged.hold);
                    batch.held.extend(merged.held);
                }
            }
            _ => {}
        }
        cx.map.slots[id].batch = Some(b);
        let now = self.now;
        let batch = self.batches.entry(b).or_insert_with(|| EditBatch::new(now));
        if !batch.members.contains(&id) {
            batch.members.push(id);
        }
    }

    /// Slot `id` leaves its batch without a result to publish (its mesh became empty or was
    /// dropped, or it unloads; a held result of it is discarded). The batch is published once no
    /// member is left. The batch's hold clock restarts with its next held result if this one
    /// was its only one.
    fn leave_batch(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        let Some(slot) = cx.map.slots.get_mut(id) else {
            return;
        };
        let Some(b) = slot.batch.take() else {
            return;
        };
        let Some(batch) = self.batches.get_mut(&b) else {
            return;
        };
        batch.members.retain(|&m| m != id);
        batch.held.retain(|r| r.id != id);
        if batch.held.is_empty() {
            batch.hold = None;
        }
        if batch.members.is_empty() {
            self.flush_batch(cx, b, false);
        }
    }

    /// Closes batch `b` and publishes its held results (`split`: some members are still out,
    /// they are published on their own when they arrive; counted in
    /// `WorldStats::edit_batches_split`).
    fn flush_batch(&mut self, cx: &mut Ctx<'_>, b: u32, split: bool) {
        let Some(batch) = self.batches.remove(&b) else {
            return;
        };
        if split {
            self.light_counts.edit_batches_split += 1;
        }
        for &m in batch.members.iter().chain(batch.held.iter().map(|r| &r.id)) {
            if let Some(s) = cx.map.slots.get_mut(m)
                && s.batch == Some(b)
            {
                s.batch = None;
            }
        }
        for result in batch.held {
            self.apply_meshed(cx, result);
        }
    }

    /// Frame step of the open batches, after the frame's results were applied: a batch whose
    /// first result was held `edit_batch_hold` or more before this frame's time, and at least
    /// [`MIN_HOLD_FRAMES`] frames before this one, is published as it is (results held in frame
    /// `N` of time `t` wait for the rest until the first frame that is both, and go out then at
    /// the latest). The hold is a time: without a frame cap a frame can last a fraction of a
    /// millisecond, less than one mesh job. A batch that never got a result is closed once it is
    /// [`STALE_BATCH_FRAMES`] frames and [`STALE_BATCH_TIME`] old. Both count as split: the
    /// members still out are published on their own.
    fn age_batches(&mut self, cx: &mut Ctx<'_>) {
        let (hold, now) = (self.hold, self.now);
        let due: Vec<u32> = self
            .batches
            .iter_mut()
            .filter_map(|(&b, batch)| {
                debug_assert_eq!(batch.hold.is_some(), !batch.held.is_empty(), "batch {b}");
                batch.age += 1;
                let due = match &mut batch.hold {
                    Some(clock) => {
                        clock.frames += 1;
                        clock.frames > MIN_HOLD_FRAMES
                            && now.saturating_duration_since(clock.since) >= hold
                    }
                    None => {
                        let stale = batch.age > STALE_BATCH_FRAMES
                            && now.saturating_duration_since(batch.opened) >= STALE_BATCH_TIME;
                        if stale {
                            tracing::debug!(
                                batch = b,
                                members = batch.members.len(),
                                "edit batch closed without a result: its meshes go out on their own"
                            );
                        }
                        stale
                    }
                };
                due.then_some(b)
            })
            .collect();
        for b in due {
            self.flush_batch(cx, b, true);
        }
    }

    /// Remeshes slot `id` for edit batch `b`: its light or data changed, so its epoch is bumped
    /// (a running job read the old content) and the remesh runs first (priority 0). A result of
    /// the slot held in an open batch is stale now: it is dropped and the remesh starts at
    /// once, so the slot stays a member that delivers.
    fn remesh_for_edit(&mut self, cx: &mut Ctx<'_>, id: ChunkId, b: u32) {
        let slot = &mut cx.map.slots[id];
        slot.epoch = slot.epoch.wrapping_add(1);
        self.drop_held(cx, id);
        let slot = &cx.map.slots[id];
        if matches!(slot.mesh(), MeshState::InFlight { .. }) {
            // Its result may already wait in `pending`.
            self.rescan_pending = true;
        }
        if slot.interest.contains(Interest::MESH) {
            self.join_batch(cx, b, id);
        }
        self.request_remesh(cx, id, 0);
    }

    /// Drops the result of slot `id` held in its batch, if any: the slot goes back to
    /// `MeshState::Queued` (its job is done) and stays a member of the batch. The batch's hold
    /// clock restarts with its next held result if this was the only one.
    fn drop_held(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        let Some(b) = cx.map.slots[id].batch else {
            return;
        };
        let Some(batch) = self.batches.get_mut(&b) else {
            return;
        };
        let Some(i) = batch.held.iter().position(|r| r.id == id) else {
            return;
        };
        batch.held.remove(i);
        if batch.held.is_empty() {
            batch.hold = None;
        }
        batch.members.push(id);
        cx.map.set_mesh_and_pending(id, MeshState::Queued, false);
    }

    /// Asks for a new mesh of `id` (its data or a neighbor changed). Slots without
    /// [`Interest::MESH`] drop their mesh instead (rebuilt when they are wanted again).
    fn request_remesh(&mut self, cx: &mut Ctx<'_>, id: ChunkId, priority: u32) {
        let slot = &cx.map.slots[id];
        let wanted = slot.interest.contains(Interest::MESH);
        if let MeshState::InFlight { .. } = slot.mesh() {
            // The running job used the old data or epoch: its result is rejected, then requeued
            // (or dropped if the mesh is no longer wanted).
            cx.map.set_remesh_pending(id, wanted);
            return;
        }
        if wanted {
            cx.map.set_mesh(id, MeshState::Queued);
            self.schedule_mesh(cx, id, priority);
        } else {
            self.drop_mesh(cx, id);
        }
    }

    /// `Remove` if the slot's mesh was published (its state is left as is).
    fn unpublish(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        if self.published.remove(&id).is_some() {
            let key = cx.map.slots[id].key();
            cx.events.push(ChunkMeshEvent::Remove { id, key });
        }
    }

    /// `MeshState::Empty`, and `Remove` if a mesh was published. A chunk meshed to nothing
    /// without a job (all air, enclosed, a failed job) gets a light job if it is near a viewer.
    fn set_mesh_empty(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        self.set_waits_sky(cx, id, false);
        // The empty fast paths (missing data, uniform chunks) need no job to know their faces.
        let slot = &cx.map.slots[id];
        if slot.face_links().is_none() {
            let faces = match slot.data() {
                DataState::Missing => Some(ChunkFaceInfo::OPEN),
                DataState::Ready(data) => data
                    .as_uniform()
                    .map(|s| ChunkFaceInfo::uniform(cx.content.registry.occludes(s))),
                DataState::Queued | DataState::Generating => None,
            };
            if faces.is_some() {
                cx.map.set_faces(id, faces);
            }
        }
        cx.map.set_mesh_and_pending(id, MeshState::Empty, false);
        self.unpublish(cx, id);
        self.leave_batch(cx, id);
        self.want_light(cx, id);
    }

    /// `MeshState::None`, and `Remove` if a mesh was published.
    fn drop_mesh(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        self.set_waits_sky(cx, id, false);
        cx.map.set_mesh_and_pending(id, MeshState::None, false);
        self.unpublish(cx, id);
        self.leave_batch(cx, id);
    }

    /// The slot's data became `Ready` or `Missing`: refresh its own neighbor bits, set its bit in
    /// the 26 neighbors and schedule every mesh that became possible.
    fn data_available(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        let pos = cx.map.slots[id].pos;
        let near = neighbor_ids(cx.map, pos);
        self.data_available_near(cx, id, pos, &near);
    }

    /// [`WorldJobs::data_available`] for the slot `id` at `pos` with its neighbors already
    /// looked up (`near`, current: no slot came or went since).
    fn data_available_near(
        &mut self,
        cx: &mut Ctx<'_>,
        id: ChunkId,
        pos: ChunkPos,
        near: &[Option<ChunkId>; 26],
    ) {
        cx.map.slots[id].neighbors_ready = neighbor_bits(cx.map, &self.planner, pos, near);
        self.schedule_mesh(cx, id, self.priority(pos));
        // Scheduling meshes adds or removes no slot: `near` stays valid.
        for (i, &n) in near.iter().enumerate() {
            let Some(n) = n else {
                continue;
            };
            let bit = 1 << opposite_neighbor(i);
            let neighbor = &mut cx.map.slots[n];
            if neighbor.neighbors_ready & bit == 0 {
                neighbor.neighbors_ready |= bit;
                if neighbor.neighbors_ready == ALL_NEIGHBORS {
                    let npos = pos.offset(NEIGHBOR_OFFSETS[i]);
                    self.schedule_mesh(cx, n, self.priority(npos));
                    // An all-air chunk meshed while a neighbor was missing.
                    self.want_light(cx, n);
                }
            }
        }
    }

    // ---- Edits -------------------------------------------------------------------------------

    /// Applies edits grouped by chunk (copy-on-write, `ChunkData::set` bumps the version) and
    /// remeshes, as one edit batch at priority 0 (epochs bumped), every chunk whose mesh may
    /// change (ADR 0015):
    ///
    /// - every chunk whose padded region (what its mesh reads) holds a changed voxel: the center
    ///   and up to 7 neighbors, edges and corners included (faces and AO);
    /// - when the voxel's opacity or emission changed, every chunk whose padded region meets the
    ///   box `p +- r`, with `r` the reach of the light change: the highest level, by channel,
    ///   of the old light at `p` and its 6 neighbors, and of the old and new emission, at most
    ///   14. The old light comes from the chunks' light caches; without a current one, `r` is
    ///   14. Up to 8 chunks;
    /// - when the voxel's sky top moved (a blocker placed over it, or the blocker at it removed),
    ///   every loaded chunk whose padded region meets the column's band: 14 voxels around the
    ///   column, from 14 under the lower of the two tops to 14 over the higher. A removed top
    ///   whose next blocker is not in the loaded data sends the column back to a walk: the
    ///   chunks around it are remeshed when the new tile lands.
    ///
    /// Edits to chunks that are not `Ready` are dropped.
    ///
    /// Returns what changed (the `BlockChanged` messages), from the same per-voxel record the
    /// light uses: per position, the state before its first edit and after its last, in the
    /// order positions were first edited, without the positions that end as they were.
    pub(crate) fn apply_edits(
        &mut self,
        cx: &mut Ctx<'_>,
        edits: Vec<BlockEdit>,
    ) -> Vec<BlockChanged> {
        let mut by_chunk: BTreeMap<ChunkPos, Vec<(usize, LocalPos, BlockState)>> = BTreeMap::new();
        for (order, edit) in edits.into_iter().enumerate() {
            let (chunk, local) = edit.pos.split();
            by_chunk
                .entry(chunk)
                .or_default()
                .push((order, local, edit.state));
        }
        let light_on = cx.sky.rule != SkyRule::Off;
        let tables = Arc::clone(&cx.content.light_tables);
        // 1. What each edit changes, from the content and the light before any of them.
        let mut changes: Vec<(ChunkId, LocalPos, BlockState, BlockState)> = Vec::new();
        // Per position edited: the order of its first edit and its change so far (`new` is the
        // state after the earlier edits of this call).
        let mut changed: Vec<(usize, BlockChanged)> = Vec::new();
        let mut remesh = BTreeSet::new();
        let mut faces = BTreeSet::new();
        for (chunk, edits) in by_chunk {
            let Some(id) = cx.map.id_at(chunk) else {
                tracing::debug!(
                    ?chunk,
                    edits = edits.len(),
                    "edits dropped: chunk not loaded"
                );
                continue;
            };
            let DataState::Ready(data) = cx.map.slots[id].data() else {
                tracing::debug!(
                    ?chunk,
                    edits = edits.len(),
                    "edits dropped: chunk not ready"
                );
                continue;
            };
            // Per voxel of this chunk edited so far: its entry in `changed`.
            let mut seen: BTreeMap<LocalPos, usize> = BTreeMap::new();
            for (order, local, state) in edits {
                let entry = *seen.entry(local).or_insert_with(|| {
                    let before = data.get(local);
                    changed.push((
                        order,
                        BlockChanged {
                            pos: BlockPos::from_parts(chunk, local),
                            old: before,
                            new: before,
                        },
                    ));
                    changed.len() - 1
                });
                let old = changed[entry].1.new;
                if old == state {
                    continue;
                }
                changed[entry].1.new = state;
                let p = BlockPos::from_parts(chunk, local);
                box_chunks(p, 0, &mut faces);
                let reach = if light_on
                    && (tables.opacity(old) != tables.opacity(state)
                        || tables.emission(old) != tables.emission(state))
                {
                    light_reach(cx.map, &tables, p, old, state)
                } else {
                    0
                };
                box_chunks(p, reach, &mut remesh);
                changes.push((id, local, old, state));
            }
        }
        changed.retain(|(_, change)| change.old != change.new);
        // Each position once: the orders are distinct.
        changed.sort_unstable_by_key(|&(order, _)| order);
        let changed = changed.into_iter().map(|(_, change)| change).collect();
        if changes.is_empty() {
            return changed;
        }
        // 2. The data. A chunk where what occludes changed needs its face info again (its next
        // mesh job computes it; until then the visibility search sees it open).
        let registry = Arc::clone(&cx.content.registry);
        let mut refaced = BTreeSet::new();
        for &(id, local, old, state) in &changes {
            if let Some(data) = cx.map.ready_data_mut(id) {
                Arc::make_mut(data).set(local, state);
            }
            if registry.occludes(old) != registry.occludes(state) {
                refaced.insert(id);
            }
        }
        for id in refaced {
            cx.map.set_faces(id, None);
        }
        // 3. The sky tops, from the new data.
        let batch = self.new_batch();
        let walked = matches!(cx.sky.rule, SkyRule::Walk(_));
        for &(id, local, old, state) in &changes {
            let (placed, removed) = (tables.blocks_sky(state), tables.blocks_sky(old));
            if !walked || placed == removed {
                continue;
            }
            let p = BlockPos::from_parts(cx.map.slots[id].pos, local);
            cx.map.slots[id].set_sky_edited();
            if let Some(band) = self.sky_edit(cx, p, placed, batch) {
                band_chunks(cx.map, cx.sky, &band, &mut remesh);
            }
        }
        // 4. The remeshes.
        remesh.extend(faces.iter().copied());
        let mut fanout = 0;
        for pos in remesh {
            let Some(id) = cx.map.id_at(pos) else {
                continue;
            };
            fanout += usize::from(!faces.contains(&pos));
            self.remesh_for_edit(cx, id, batch);
        }
        self.light_counts.light_fanout += fanout;
        self.close_if_empty(cx, batch);
        changed
    }

    /// A voxel at `p` now stops direct sky (`placed`) or no longer does: updates the sky top of
    /// its column. Returns the band to remesh when the top moved; `None` when it did not, or
    /// when the column must be walked again (pending: the walk's result remeshes, as part of
    /// `batch`).
    fn sky_edit(
        &mut self,
        cx: &mut Ctx<'_>,
        p: BlockPos,
        placed: bool,
        batch: u32,
    ) -> Option<[RangeInclusive<i64>; 3]> {
        let SkyRule::Walk(rule) = &cx.sky.rule else {
            return None;
        };
        let layers = rule.layers.clone();
        // In a world whose columns can reach the void, content under the floor decides whether
        // the open columns of the tile end at the floor or stay open forever: walk again.
        let under_floor = rule.void_reachable && p.y < rule.floor;
        let (chunk, local) = p.split();
        let col = cx.sky.id_at(chunk.x, chunk.z)?;
        let c = &mut cx.sky.columns[col];
        c.edited = true;
        if placed {
            c.edit_top = c.edit_top.max(p.y);
        }
        let SkyState::Exact(tile) = &mut c.state else {
            // Not walked yet, or a walk is running with the old content: it is discarded, and
            // what the next tile changes is remeshed with this batch.
            c.version = c.version.wrapping_add(1);
            c.batch = Some(batch);
            return None;
        };
        if under_floor {
            let old = Some(Arc::clone(tile));
            c.state = SkyState::Pending { old };
            c.version = c.version.wrapping_add(1);
            c.batch = Some(batch);
            self.request_walk(cx.sky, col, 0);
            return None;
        }
        let (lx, lz) = (local.x(), local.z());
        let top = tile.top(lx, lz);
        if placed {
            if p.y <= top {
                return None;
            }
            Arc::make_mut(tile).raise(lx, lz, p.y);
            return Some(sky_band(p.x, p.z, top, p.y));
        }
        if p.y != top {
            return None;
        }
        // The top was cleared: the next blocker down, in the loaded data.
        let (x, z, loaded) = (c.x, c.z, &c.layers);
        let map = &*cx.map;
        let found =
            Arc::make_mut(tile).rescan_column(lx, lz, p.y - 1, &cx.content.light_tables, |cy| {
                rescan_layer(map, (x, z), cy, loaded, &layers)
            });
        match found {
            Rescan::Found(next) => Some(sky_band(p.x, p.z, next, top)),
            Rescan::Unresolved => {
                let old = Some(Arc::clone(tile));
                c.state = SkyState::Pending { old };
                c.version = c.version.wrapping_add(1);
                c.batch = Some(batch);
                self.request_walk(cx.sky, col, 0);
                None
            }
        }
    }

    // ---- Jobs --------------------------------------------------------------------------------

    /// The time of a frame starting now: the real clock, or a test's `manual_clock`.
    pub(crate) fn clock_now(&self) -> Instant {
        #[cfg(test)]
        if let Some(now) = self.manual_clock {
            return now;
        }
        Instant::now()
    }

    /// Frame bookkeeping before the job step: measures the frame interval, applies the
    /// in-flight cap (`settings.max_jobs_in_flight`, or the automatic one when it is 0) and takes
    /// the mesher options of the jobs it starts. `now` is the frame's time: the edit batch hold
    /// is measured with it.
    pub(crate) fn begin_frame(&mut self, settings: &WorldSettings, now: Instant) {
        self.mesh_options = MeshOptions::with_ao(settings.ao_policy);
        self.light_cache_sq = light_cache_radius(settings).pow(2);
        self.hold = settings.edit_batch_hold;
        self.now = now;
        self.auto_cap.observe_frame(now);
        let cap = match settings.max_jobs_in_flight {
            0 => self.auto_cap.cap(),
            n => n,
        };
        self.scheduler.set_max_in_flight(cap);
    }

    /// The job step of a frame: collects finished jobs (freeing their in-flight slots) and starts
    /// queued ones, again and again while results keep arriving, within `budget`
    /// (`settings.job_budget`, or more in a capped frame with time to spare). Collecting first
    /// means a slot freed by a job that finished since the last frame is reused in this frame,
    /// not the next one.
    pub(crate) fn run_jobs(&mut self, cx: &mut Ctx<'_>, budget: Duration) {
        let start = Instant::now();
        loop {
            let collected = {
                let _span = frame_span!("world.drain");
                self.drain_results(cx, start, budget)
            };
            let started = {
                let _span = frame_span!("world.dispatch");
                self.dispatch_until(cx, start, budget)
            };
            if (collected == 0 && started == 0) || start.elapsed() >= budget {
                break;
            }
        }
    }

    /// Starts queued jobs while below the in-flight cap. Jobs whose slot no longer wants them are
    /// discarded (their in-flight slot is released immediately).
    #[cfg(test)]
    pub(crate) fn dispatch(&mut self, cx: &mut Ctx<'_>) {
        self.dispatch_until(cx, Instant::now(), Duration::MAX);
    }

    /// [`WorldJobs::dispatch`] until `budget` after `start` (checked every
    /// [`DISPATCH_CHECK`] jobs or priority refreshes). Stale priorities that may come before the
    /// next job are refreshed first, so jobs start nearest first even right after a viewer move.
    /// Returns the jobs started.
    fn dispatch_until(&mut self, cx: &mut Ctx<'_>, start: Instant, budget: Duration) -> usize {
        let (mut started, mut work, mut check) = (0, 0usize, DISPATCH_CHECK);
        while !self.scheduler.is_full() {
            let (map, sky, planner) = (&*cx.map, &*cx.sky, &self.planner);
            let refreshed = self.scheduler.refresh_ahead(DISPATCH_CHECK, |id, kind| {
                current_priority(map, sky, planner, id, kind)
            });
            if refreshed > 0 {
                work += refreshed;
            } else {
                let Some(job) = self.scheduler.pop_ready() else {
                    break;
                };
                let ok = match (job.kind, job.id) {
                    (JobKind::Generate, JobId::Chunk(id)) => self.start_generate(cx, id),
                    (JobKind::Mesh, JobId::Chunk(id)) => self.start_mesh(cx, id),
                    (JobKind::SkyTile, JobId::Column(col)) => self.start_walk(cx, col),
                    (JobKind::Light, JobId::Chunk(id)) => self.start_light(cx, id),
                    _ => false,
                };
                if ok {
                    started += 1;
                } else {
                    self.scheduler.complete();
                }
                work += 1;
            }
            if work >= check {
                if start.elapsed() >= budget {
                    break;
                }
                check = work + DISPATCH_CHECK;
            }
        }
        started
    }

    fn start_generate(&mut self, cx: &mut Ctx<'_>, id: ChunkId) -> bool {
        let Some(slot) = cx.map.slots.get(id) else {
            return false;
        };
        if !matches!(slot.data(), DataState::Queued) {
            return false;
        }
        let (key, cancel) = (slot.key(), Arc::clone(&slot.cancel));
        cx.map.set_data(id, DataState::Generating);
        let generator = Arc::clone(&cx.content.generator);
        let registry = Arc::clone(&cx.content.registry);
        spawn_job(
            self.executor.as_ref(),
            &self.tx,
            JobId::Chunk(id),
            JobKind::Generate,
            cancel,
            move || {
                let data = generator.generate(key, &registry);
                let faces = ChunkFaceInfo::compute(&data, &registry);
                JobMessage::Generated { id, data, faces }
            },
        );
        true
    }

    fn start_mesh(&mut self, cx: &mut Ctx<'_>, id: ChunkId) -> bool {
        let map = &*cx.map;
        let Some(slot) = map.slots.get(id) else {
            return false;
        };
        if !matches!(slot.mesh(), MeshState::Queued)
            || !slot.interest.contains(Interest::MESH)
            || slot.neighbors_ready != ALL_NEIGHBORS
        {
            return false;
        }
        let DataState::Ready(center) = slot.data() else {
            return false;
        };
        let pos = slot.pos;
        let registry = &cx.content.registry;
        if is_uniform_empty(center, registry) {
            self.set_mesh_empty(cx, id);
            return false;
        }
        // Each neighbor is looked up once: the face neighbors first, for the enclosed check.
        let mut near = NearData::default();
        if is_enclosed_opaque(map, registry, pos, center, &mut near) {
            self.set_mesh_empty(cx, id);
            return false;
        }
        let center = Arc::clone(center);
        let neighbors: [Option<Arc<ChunkData>>; 26] =
            std::array::from_fn(|i| near.get(map, pos, i).cloned());
        let (data_version, epoch) = (center.version, slot.epoch);
        let cancel = Arc::clone(&slot.cancel);
        // After an edit that changed what occludes, the job computes the face info again.
        let faces_for = slot
            .face_links()
            .is_none()
            .then(|| Arc::clone(&cx.content.registry));
        // The tiles were walked when the job was queued, but an edit may have sent one back.
        let light = match self.sky_for(cx, id, true) {
            SkyFor::Waiting => return false,
            SkyFor::Ready(_) if cx.sky.rule == SkyRule::Off => None,
            SkyFor::Ready(tiles) => Some(LightJob {
                tables: Arc::clone(&cx.content.light_tables),
                tiles,
                keep: self.planner.priority(pos) <= self.light_cache_sq,
            }),
        };
        cx.map.set_mesh_and_pending(
            id,
            MeshState::InFlight {
                data_version,
                epoch,
            },
            false,
        );
        #[cfg(test)]
        {
            *self.mesh_jobs.entry(pos).or_default() += 1;
        }
        let materials = Arc::clone(&cx.content.materials);
        let (mesh_fn, options) = (self.mesh_fn, self.mesh_options);
        spawn_job(
            self.executor.as_ref(),
            &self.tx,
            JobId::Chunk(id),
            JobKind::Mesh,
            cancel,
            move || {
                let near: [Option<&ChunkData>; 26] =
                    std::array::from_fn(|i| neighbors[i].as_deref());
                let faces = faces_for.map(|registry| ChunkFaceInfo::compute(&center, &registry));
                let Some(light) = light else {
                    let input = MeshInput {
                        center: &center,
                        neighbors: near,
                        light: None,
                    };
                    let mesh = mesh_fn(&input, &materials, options);
                    return JobMessage::Meshed {
                        id,
                        data_version,
                        epoch,
                        mesh,
                        light: None,
                        path: None,
                        faces,
                    };
                };
                // The 27 chunks in `boxel_light::box_light::chunk_index` order: the
                // neighbor order with the center inserted at 13.
                let chunks = std::array::from_fn(|i| match i {
                    13 => Some(&*center),
                    i if i < 13 => near[i],
                    i => near[i - 1],
                });
                with_light(&light, pos, chunks, |padded, path| {
                    let input = MeshInput {
                        center: &center,
                        neighbors: near,
                        light: Some(padded),
                    };
                    let mesh = mesh_fn(&input, &materials, options);
                    JobMessage::Meshed {
                        id,
                        data_version,
                        epoch,
                        mesh,
                        light: light.keep.then(|| ChunkLight::from_padded(padded)),
                        path: Some(path),
                        faces,
                    }
                })
            },
        );
        true
    }

    /// Starts the walk of column `col` if it is still `Pending`: first waits for the data of the
    /// loaded layers it reads (down to just under its lowest waiting mesh), so it generates as
    /// few layers as it can; then captures the loaded layers and spawns the walk.
    fn start_walk(&mut self, cx: &mut Ctx<'_>, col: ColumnId) -> bool {
        let SkyRule::Walk(rule) = &cx.sky.rule else {
            return false;
        };
        let rule = rule.clone();
        let Some(c) = cx.sky.columns.get_mut(col) else {
            return false;
        };
        c.queued = false;
        let SkyState::Pending { old } = &c.state else {
            return false;
        };
        let old = old.clone();
        let Some(start) = rule.start(c.edit_top) else {
            // Nothing in the column: open forever.
            let open = Arc::clone(&cx.sky.open);
            self.land_tile(cx, col, old, open);
            return false;
        };
        // Only the loaded layers are visited: the range can span far more layers than are
        // loaded (an edit far above, a viewer far below).
        let (x, z) = (c.x, c.z);
        let top = c.hi().min(start >> CHUNK_SHIFT);
        let floor = rule.floor >> CHUNK_SHIFT;
        let map = &*cx.map;
        let wait_from = c
            .waiters
            .iter()
            .filter_map(|&w| map.get(w))
            .map(|s| s.pos.y - 1)
            .min()
            .map_or(floor, |y| y.max(floor));
        let slot_at = |cy| map.id_at(ChunkPos::new(x, cy, z));
        let waiting = c.layers_in(wait_from, top).iter().rev().find_map(|&cy| {
            slot_at(cy).filter(|&id| {
                matches!(
                    map.slots[id].data(),
                    DataState::Queued | DataState::Generating
                )
            })
        });
        if let Some(id) = waiting {
            c.waiting_for = Some(id);
            return false;
        }
        let captured: Vec<(i64, Option<Arc<ChunkData>>)> = c
            .layers_in(floor, top)
            .iter()
            .rev()
            .filter_map(|&cy| match map.slots[slot_at(cy)?].data() {
                DataState::Ready(data) => Some((cy, Some(Arc::clone(data)))),
                DataState::Missing => Some((cy, None)),
                DataState::Queued | DataState::Generating => None,
            })
            .collect();
        let tables = &cx.content.light_tables;
        let content_below = rule.void_reachable
            && c.layers_in(i64::MIN, floor - 1).iter().any(|&cy| {
                map.data_at(ChunkPos::new(x, cy, z))
                    .is_some_and(|d| may_block(d, tables))
            });
        let version = c.version;
        c.state = SkyState::InFlight { version, old };
        let job = WalkJob {
            x,
            z,
            dim: c.dim,
            start,
            floor: rule.floor,
            layers: rule.layers,
            captured,
            content_below,
        };
        let cancel = Arc::clone(&c.cancel);
        let generator = Arc::clone(&cx.content.generator);
        let registry = Arc::clone(&cx.content.registry);
        let tables = Arc::clone(tables);
        spawn_job(
            self.executor.as_ref(),
            &self.tx,
            JobId::Column(col),
            JobKind::SkyTile,
            cancel,
            move || {
                let (tile, generated) = job.run(generator.as_ref(), &registry, &tables);
                JobMessage::Walked {
                    column: col,
                    version,
                    tile,
                    generated,
                }
            },
        );
        true
    }

    /// True if slot `id` wants a light job: light is on, the slot has mesh interest, data and a
    /// current mesh (a mesh job keeps the light of chunks near a viewer), it is within the light
    /// cache radius, and its cached light is not current.
    fn light_wanted(&self, cx: &Ctx<'_>, id: ChunkId) -> bool {
        let Some(slot) = cx.map.get(id) else {
            return false;
        };
        let mesh_current = match (slot.mesh(), slot.data()) {
            (
                MeshState::Ready {
                    data_version,
                    epoch,
                    ..
                },
                DataState::Ready(data),
            ) => *data_version == data.version && *epoch == slot.epoch,
            (MeshState::Empty, DataState::Ready(_)) => true,
            _ => false,
        };
        cx.sky.rule != SkyRule::Off
            && slot.interest.contains(Interest::MESH)
            && slot.neighbors_ready == ALL_NEIGHBORS
            && mesh_current
            && !slot.remesh_pending()
            && slot.current_light().is_none()
            && self.planner.priority(slot.pos) <= self.light_cache_sq
    }

    /// Queues a light job for slot `id` if it wants one ([`WorldJobs::light_wanted`]) and has
    /// none queued or running. A uniform chunk of a fully opaque state that emits nothing needs
    /// no job: no light enters it.
    fn want_light(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        if cx.map.get(id).is_none_or(ChunkSlot::light_job) || !self.light_wanted(cx, id) {
            return;
        }
        let tables = &cx.content.light_tables;
        let slot = &mut cx.map.slots[id];
        if let DataState::Ready(data) = slot.data()
            && let Some(state) = data.as_uniform()
            && tables.opacity(state) == OPAQUE_OPACITY
            && tables.emission(state) == LightValue::DARK
        {
            let (data_version, epoch) = (data.version, slot.epoch);
            slot.light = Some(Arc::new(CachedLight {
                light: ChunkLight::Uniform(LightValue::DARK),
                data_version,
                epoch,
            }));
            return;
        }
        slot.set_light_job(true);
        let priority = light_priority(self.planner.priority(slot.pos));
        self.scheduler.push(id, JobKind::Light, priority);
    }

    /// Starts the light job of slot `id` if it still wants one, its 26 neighbors are ready and
    /// the sky tiles around it are walked (else it waits for them, like a mesh).
    fn start_light(&mut self, cx: &mut Ctx<'_>, id: ChunkId) -> bool {
        let Some(slot) = cx.map.slots.get_mut(id) else {
            return false;
        };
        slot.set_light_job(false);
        if !self.light_wanted(cx, id) {
            return false;
        }
        let tiles = match self.sky_for(cx, id, true) {
            SkyFor::Waiting => return false,
            SkyFor::Ready(tiles) => tiles,
        };
        let map = &*cx.map;
        let slot = &map.slots[id];
        let DataState::Ready(center) = slot.data() else {
            return false;
        };
        let pos = slot.pos;
        let mut near = NearData::default();
        let neighbors: [Option<Arc<ChunkData>>; 26] =
            std::array::from_fn(|i| near.get(map, pos, i).cloned());
        let center = Arc::clone(center);
        let (data_version, epoch) = (center.version, slot.epoch);
        let cancel = Arc::clone(&slot.cancel);
        let light = LightJob {
            tables: Arc::clone(&cx.content.light_tables),
            tiles,
            keep: true,
        };
        cx.map.slots[id].set_light_job(true);
        spawn_job(
            self.executor.as_ref(),
            &self.tx,
            JobId::Chunk(id),
            JobKind::Light,
            cancel,
            move || {
                // The 27 chunks in `boxel_light::box_light::chunk_index` order.
                let chunks = std::array::from_fn(|i| match i {
                    13 => Some(&*center),
                    i if i < 13 => neighbors[i].as_deref(),
                    i => neighbors[i - 1].as_deref(),
                });
                with_light(&light, pos, chunks, |padded, _| JobMessage::Lit {
                    id,
                    data_version,
                    epoch,
                    light: ChunkLight::from_padded(padded),
                })
            },
        );
        true
    }

    /// A light job of slot `id` finished: its light is cached if it is still the light of the
    /// slot's content and the slot is still near a viewer; else the slot may want another job.
    fn on_lit(
        &mut self,
        cx: &mut Ctx<'_>,
        id: ChunkId,
        data_version: u32,
        epoch: u32,
        light: ChunkLight,
    ) {
        let Some(slot) = cx.map.slots.get_mut(id) else {
            return;
        };
        slot.set_light_job(false);
        let current = matches!(slot.data(), DataState::Ready(d) if d.version == data_version)
            && slot.epoch == epoch;
        if current && self.planner.priority(slot.pos) <= self.light_cache_sq {
            slot.light = Some(Arc::new(CachedLight {
                light,
                data_version,
                epoch,
            }));
        } else {
            self.want_light(cx, id);
        }
    }

    /// Column `col` gets `tile` (it was `old`): every loaded chunk whose light the changed tops
    /// can reach is remeshed (one edit batch: the column's, or a new one), and the meshes that
    /// waited for the tile are scheduled.
    fn land_tile(
        &mut self,
        cx: &mut Ctx<'_>,
        col: ColumnId,
        old: Option<Arc<SkyTile>>,
        tile: Arc<SkyTile>,
    ) {
        let c = &mut cx.sky.columns[col];
        let (x, z) = (c.x, c.z);
        c.state = SkyState::Exact(Arc::clone(&tile));
        let batch = c.batch.take();
        let waiters = std::mem::take(&mut c.waiters);
        if let Some(old) = old
            && let Some(band) = changed_band(&old, &tile, x, z)
        {
            let mut chunks = BTreeSet::new();
            band_chunks(cx.map, cx.sky, &band, &mut chunks);
            let b = batch.unwrap_or_else(|| self.new_batch());
            self.light_counts.light_fanout += chunks.len();
            for pos in chunks {
                if let Some(id) = cx.map.id_at(pos) {
                    self.remesh_for_edit(cx, id, b);
                }
            }
            self.close_if_empty(cx, b);
        }
        for w in waiters {
            if let Some(slot) = cx.map.get(w) {
                let priority = self.priority(slot.pos);
                self.schedule_mesh(cx, w, priority);
                // A light job that waited for the tile.
                self.want_light(cx, w);
            }
        }
    }

    /// Batch `b` closes if no member joined it.
    fn close_if_empty(&mut self, cx: &mut Ctx<'_>, b: u32) {
        if self
            .batches
            .get(&b)
            .is_some_and(|batch| batch.members.is_empty())
        {
            self.flush_batch(cx, b, false);
        }
    }

    // ---- Collect -----------------------------------------------------------------------------

    /// Drains job results within `settings.job_budget`, then applies mesh results within
    /// `settings.max_mesh_events_per_frame` events (the rest wait for the next call).
    #[cfg(test)]
    pub(crate) fn collect(&mut self, cx: &mut Ctx<'_>, settings: &WorldSettings) {
        self.drain_results(cx, Instant::now(), settings.job_budget);
        self.apply_meshes(cx, settings);
    }

    /// Drains job results until `budget` after `start` (at least one per call; every message
    /// releases its in-flight slot). Mesh results wait for [`WorldJobs::apply_meshes`]. Returns
    /// how many results were drained.
    fn drain_results(&mut self, cx: &mut Ctx<'_>, start: Instant, budget: Duration) -> usize {
        let mut drained = 0;
        while let Ok(JobReport { message, busy }) = self.rx.try_recv() {
            self.scheduler.complete();
            drained += 1;
            match message {
                JobMessage::Generated { id, data, faces } => {
                    self.auto_cap.observe_job(busy);
                    self.on_generated(cx, id, data, faces);
                }
                JobMessage::Meshed {
                    id,
                    data_version,
                    epoch,
                    mesh,
                    light,
                    path,
                    faces,
                } => {
                    self.auto_cap.observe_job(busy);
                    match path {
                        Some(LightPath::Full { .. }) => self.light_counts.light_full += 1,
                        Some(LightPath::FastDark | LightPath::FastOpen) => {
                            self.light_counts.light_fast += 1;
                        }
                        None => {}
                    }
                    let result = MeshedResult {
                        id,
                        data_version,
                        epoch,
                        mesh,
                        light,
                        faces,
                    };
                    if in_batch(cx.map, id) {
                        self.batch_pending.push(result);
                    } else {
                        self.pending.push_back(result);
                    }
                }
                JobMessage::Lit {
                    id,
                    data_version,
                    epoch,
                    light,
                } => {
                    self.auto_cap.observe_job(busy);
                    self.light_counts.light_jobs += 1;
                    self.on_lit(cx, id, data_version, epoch, light);
                }
                JobMessage::Walked {
                    column,
                    version,
                    tile,
                    generated,
                } => {
                    self.auto_cap.observe_job(busy);
                    self.light_counts.sky_walks += 1;
                    self.light_counts.sky_private_generations += generated as usize;
                    self.on_walked(cx, column, version, tile);
                }
                JobMessage::Cancelled { id, kind } => {
                    tracing::trace!(?id, ?kind, "job cancelled (slot or column unloaded)");
                }
                JobMessage::Failed { id, kind, panic } => self.on_failed(cx, id, kind, &panic),
            }
            if start.elapsed() >= budget {
                break;
            }
        }
        drained
    }

    /// Applies waiting mesh results: first every result of an edit batch, whatever the budget
    /// (the meshes of a batch go out together), then the others within
    /// `settings.max_mesh_events_per_frame` events. Then publishes the edit batches that waited
    /// long enough.
    pub(crate) fn apply_meshes(&mut self, cx: &mut Ctx<'_>, settings: &WorldSettings) {
        if std::mem::take(&mut self.rescan_pending) && !self.pending.is_empty() {
            let all = std::mem::take(&mut self.pending);
            let mut rest = VecDeque::with_capacity(all.len());
            for result in all {
                if in_batch(cx.map, result.id) {
                    self.batch_pending.push(result);
                } else {
                    rest.push_back(result);
                }
            }
            self.pending = rest;
        }
        for result in std::mem::take(&mut self.batch_pending) {
            self.apply_meshed(cx, result);
        }
        let budget = settings.max_mesh_events_per_frame.max(1);
        let first = cx.events.len();
        while cx.events.len() - first < budget {
            let Some(result) = self.pending.pop_front() else {
                break;
            };
            self.apply_meshed(cx, result);
        }
        self.age_batches(cx);
    }

    fn on_generated(
        &mut self,
        cx: &mut Ctx<'_>,
        id: ChunkId,
        data: ChunkData,
        faces: ChunkFaceInfo,
    ) {
        let Some(slot) = cx.map.slots.get(id) else {
            return;
        };
        if !matches!(slot.data(), DataState::Generating) || slot.cancel.load(Ordering::Relaxed) {
            return;
        }
        let data = self.snapshot(data);
        cx.map.set_data(id, DataState::Ready(data));
        cx.map.set_faces(id, Some(faces));
        self.data_available(cx, id);
        let col = cx.map.slots[id].column;
        self.walk_data_arrived(cx, col, id);
    }

    /// A walk of column `col` (its `version`) finished: the tile becomes the column's, unless
    /// the column changed meanwhile (then it is walked again).
    fn on_walked(&mut self, cx: &mut Ctx<'_>, col: ColumnId, version: u32, tile: SkyTile) {
        let Some(c) = cx.sky.columns.get_mut(col) else {
            return;
        };
        let SkyState::InFlight { version: v, old } = &mut c.state else {
            return;
        };
        let old = old.take();
        if *v != version || c.version != version {
            c.state = SkyState::Pending { old };
            let priority = self.planner.column_priority(c.x, c.z);
            self.request_walk(cx.sky, col, priority);
            return;
        }
        self.land_tile(cx, col, old, Arc::new(tile));
    }

    fn on_failed(&mut self, cx: &mut Ctx<'_>, id: JobId, kind: JobKind, panic: &str) {
        self.failed += 1;
        let id = match id {
            JobId::Chunk(id) => id,
            JobId::Column(col) => {
                // A walk that panics (private generations that panic read as air already)
                // would panic again: the column gets open sky rather than blocking its meshes.
                let Some(c) = cx.sky.columns.get_mut(col) else {
                    return;
                };
                tracing::error!(column = ?(c.x, c.z), "sky walk failed: {panic}");
                let SkyState::InFlight { old, .. } = &mut c.state else {
                    return;
                };
                let old = old.take();
                let open = Arc::clone(&cx.sky.open);
                self.land_tile(cx, col, old, open);
                return;
            }
        };
        let Some(slot) = cx.map.slots.get(id) else {
            tracing::debug!(?kind, "job for an unloaded chunk failed: {panic}");
            return;
        };
        if self.failure_logged.insert(id) {
            tracing::error!(chunk = ?slot.pos, ?kind, "chunk job failed: {panic}");
        }
        match kind {
            JobKind::Generate => {
                if matches!(slot.data(), DataState::Generating) {
                    // Permanently air, like chunks outside the vertical bounds.
                    let col = slot.column;
                    cx.map.set_data(id, DataState::Missing);
                    cx.map.set_faces(id, Some(ChunkFaceInfo::OPEN));
                    self.data_available(cx, id);
                    self.walk_data_arrived(cx, col, id);
                }
            }
            JobKind::SkyTile => {}
            JobKind::Light => {
                // Not retried: it would fail again. The chunk's next mesh job keeps its light.
                cx.map.slots[id].set_light_job(false);
            }
            JobKind::Mesh => {
                if !matches!(slot.mesh(), MeshState::InFlight { .. }) {
                    return;
                }
                if slot.remesh_pending() {
                    // The data changed meanwhile: try again with the new data.
                    let priority = mesh_priority(&self.planner, slot);
                    cx.map.set_mesh_and_pending(id, MeshState::Queued, false);
                    self.schedule_mesh(cx, id, priority);
                } else {
                    self.set_mesh_empty(cx, id);
                }
            }
        }
    }

    /// Accepts a mesh result if it still matches the slot (same data version and epoch, no
    /// remesh requested, mesh still wanted); otherwise requeues the mesh or drops it.
    fn apply_meshed(&mut self, cx: &mut Ctx<'_>, result: MeshedResult) {
        let id = result.id;
        let Some(slot) = cx.map.slots.get(id) else {
            return;
        };
        let MeshState::InFlight {
            data_version,
            epoch,
        } = *slot.mesh()
        else {
            return;
        };
        if slot.cancel.load(Ordering::Relaxed) {
            return;
        }
        if !slot.interest.contains(Interest::MESH) {
            self.drop_mesh(cx, id);
            return;
        }
        let current_version = match slot.data() {
            DataState::Ready(d) => Some(d.version),
            _ => None,
        };
        let fresh = !slot.remesh_pending()
            && (data_version, epoch) == (result.data_version, result.epoch)
            && current_version == Some(result.data_version)
            && slot.epoch == result.epoch;
        if !fresh {
            let priority = mesh_priority(&self.planner, slot);
            cx.map.set_mesh_and_pending(id, MeshState::Queued, false);
            self.schedule_mesh(cx, id, priority);
            return;
        }
        // Part of an edit batch: wait for the rest of it.
        if !self.hold.is_zero()
            && let Some(b) = slot.batch
            && let Some(batch) = self.batches.get_mut(&b)
        {
            batch.members.retain(|&m| m != id);
            let first = batch.held.is_empty();
            batch.held.push(result);
            if batch.members.is_empty() {
                self.flush_batch(cx, b, false);
            } else if first {
                batch.hold = Some(HoldClock {
                    since: self.now,
                    frames: 0,
                });
                self.light_counts.edit_batches_held += 1;
            }
            return;
        }
        self.leave_batch_published(cx, id);
        // The faces of the data this mesh shows, set with the mesh (the visibility search only
        // trusts them once the mesh is published).
        if result.faces.is_some() {
            cx.map.set_faces(id, result.faces);
        }
        let near = self.planner.priority(cx.map.slots[id].pos) <= self.light_cache_sq;
        let light = result.light.filter(|_| near).map(|light| {
            Arc::new(CachedLight {
                light,
                data_version: result.data_version,
                epoch: result.epoch,
            })
        });
        cx.map.set_light(id, light);
        if result.mesh.is_empty() {
            self.set_mesh_empty(cx, id);
            return;
        }
        let key = cx.map.slots[id].key();
        // A mesh identical to the published one (a remesh whose light or AO did not change) is
        // kept as it is: no event, no upload.
        let unchanged = self
            .published
            .get(&id)
            .filter(|old| old.hash == result.mesh.hash && ***old == result.mesh)
            .cloned();
        let mesh = unchanged.clone().unwrap_or_else(|| Arc::new(result.mesh));
        cx.map.set_mesh(
            id,
            MeshState::Ready {
                mesh: Arc::clone(&mesh),
                data_version: result.data_version,
                epoch: result.epoch,
            },
        );
        if unchanged.is_none() {
            self.published.insert(id, Arc::clone(&mesh));
            cx.events.push(ChunkMeshEvent::Upsert { id, key, mesh });
        }
        // The job may have started before the chunk came near a viewer.
        self.want_light(cx, id);
    }

    /// Slot `id` publishes its result on its own (no hold): it leaves its batch, if any.
    fn leave_batch_published(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        let Some(b) = cx.map.slots[id].batch.take() else {
            return;
        };
        if let Some(batch) = self.batches.get_mut(&b) {
            batch.members.retain(|&m| m != id);
            if batch.members.is_empty() {
                self.flush_batch(cx, b, false);
            }
        }
    }
}

/// Priority of a queued job for the current viewers, `None` if its slot (or column) no longer
/// wants it.
fn current_priority(
    map: &ChunkMap,
    sky: &SkyMap,
    planner: &Planner,
    id: JobId,
    kind: JobKind,
) -> Option<u32> {
    let id = match id {
        JobId::Chunk(id) => id,
        JobId::Column(col) => {
            let c = sky.columns.get(col)?;
            // A walk an edit sent the column back to runs first, like the edit's remeshes.
            let priority = match c.batch {
                Some(_) => 0,
                None => planner.column_priority(c.x, c.z),
            };
            return c.queued.then_some(priority);
        }
    };
    let slot = map.slots.get(id)?;
    match kind {
        JobKind::Generate => {
            matches!(slot.data(), DataState::Queued).then(|| planner.priority(slot.pos))
        }
        JobKind::Mesh => {
            matches!(slot.mesh(), MeshState::Queued).then(|| mesh_priority(planner, slot))
        }
        // Kept until popped (`start_light` clears the flag).
        JobKind::Light => slot
            .light_job()
            .then(|| light_priority(planner.priority(slot.pos))),
        JobKind::SkyTile => None,
    }
}

/// Priority of a mesh job of `slot`: 0 for a member of an edit batch (edits run first, also
/// after a viewer move refreshes the queue), else its distance.
fn mesh_priority(planner: &Planner, slot: &ChunkSlot) -> u32 {
    if slot.batch.is_some() {
        0
    } else {
        planner.priority(slot.pos)
    }
}

/// Priority of a light job of a chunk at `priority` (its distance): after the mesh jobs of the
/// view ([`LIGHT_JOB_BIAS`]).
fn light_priority(priority: u32) -> u32 {
    priority.saturating_add(LIGHT_JOB_BIAS)
}

/// The voxels whose light a change of sky tops from `old` to `new` (column `(x, z)`) can reach:
/// the columns whose top moved, +-14 horizontally, from 14 under the lower of the two tops to 14
/// over the higher. `None` if no top moved.
fn changed_band(old: &SkyTile, new: &SkyTile, x: i64, z: i64) -> Option<[RangeInclusive<i64>; 3]> {
    let mut band: Option<[(i64, i64); 3]> = None;
    for (i, (&a, &b)) in old.tops().iter().zip(new.tops()).enumerate() {
        if a == b {
            continue;
        }
        let wx = (x << CHUNK_SHIFT) + (i >> CHUNK_SHIFT) as i64;
        let wz = (z << CHUNK_SHIFT) + (i & 31) as i64;
        let (lo, hi) = (a.min(b).saturating_add(1), a.max(b));
        let r = band.get_or_insert([(wx, wx), (lo, hi), (wz, wz)]);
        for (axis, (l, h)) in r.iter_mut().zip([(wx, wx), (lo, hi), (wz, wz)]) {
            axis.0 = axis.0.min(l);
            axis.1 = axis.1.max(h);
        }
    }
    band.map(|axes| axes.map(|(l, h)| reach(l, h)))
}

/// Sky band of one column whose top moved from `a` to `b` (see [`changed_band`]).
fn sky_band(x: i64, z: i64, a: i64, b: i64) -> [RangeInclusive<i64>; 3] {
    [
        reach(x, x),
        reach(a.min(b).saturating_add(1), b.max(a)),
        reach(z, z),
    ]
}

/// Voxels `lo..=hi` widened by the reach of light (14).
fn reach(lo: i64, hi: i64) -> RangeInclusive<i64> {
    lo.saturating_sub(i64::from(LIGHT_REACH))..=hi.saturating_add(i64::from(LIGHT_REACH))
}

/// How far light of level 15 reaches past its source (a voxel of level 1 is 14 away).
const LIGHT_REACH: u8 = 14;

/// The loaded chunks whose padded region meets the voxel box `band` (x, y, z), into `out`.
/// Only each column's loaded layers are visited (the band can span `2^56` layers).
fn band_chunks(
    map: &ChunkMap,
    sky: &SkyMap,
    band: &[RangeInclusive<i64>; 3],
    out: &mut BTreeSet<ChunkPos>,
) {
    let ys = padded_chunks(*band[1].start(), *band[1].end());
    for cx in padded_chunks(*band[0].start(), *band[0].end()) {
        for cz in padded_chunks(*band[2].start(), *band[2].end()) {
            let Some(col) = sky.id_at(cx, cz) else {
                continue;
            };
            for &cy in sky.columns[col].layers_in(*ys.start(), *ys.end()) {
                let pos = ChunkPos::new(cx, cy, cz);
                debug_assert!(map.id_at(pos).is_some(), "column layer {pos:?} not loaded");
                out.insert(pos);
            }
        }
    }
}

/// The chunks whose padded region meets the voxels within `r` of `p` (a box), into `out`.
fn box_chunks(p: BlockPos, r: u8, out: &mut BTreeSet<ChunkPos>) {
    let r = i64::from(r);
    for cy in padded_chunks(p.y - r, p.y + r) {
        for cx in padded_chunks(p.x - r, p.x + r) {
            for cz in padded_chunks(p.z - r, p.z + r) {
                out.insert(ChunkPos::new(cx, cy, cz));
            }
        }
    }
}

/// Reach of the light change of voxel `p` going from `old` to `new` (see
/// [`WorldJobs::apply_edits`]): 14 when a light it needs is not cached or not current.
fn light_reach(
    map: &ChunkMap,
    tables: &boxel_voxel::LightTables,
    p: BlockPos,
    old: BlockState,
    new: BlockState,
) -> u8 {
    let mut r = max_level(tables.emission(old)).max(max_level(tables.emission(new)));
    let around = [
        (0, 0, 0),
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ];
    for (dx, dy, dz) in around {
        let (chunk, local) = BlockPos::new(p.x + dx, p.y + dy, p.z + dz).split();
        let light = map
            .id_at(chunk)
            .and_then(|id| map.slots[id].current_light());
        match light {
            Some(light) => r = r.max(max_level(light.get(local))),
            None => return LIGHT_REACH,
        }
    }
    r.min(LIGHT_REACH)
}

/// Layer `cy` of column `(x, z)` for a rescan after an edit: loaded data, or air above the
/// generator's range down to the next loaded layer (an unloaded chunk there lost its edits;
/// `loaded`: the column's loaded layers, ascending). `None` otherwise: inside the range a walk
/// generates the layer, and under it a walk decides between air and void from the loaded
/// content below.
fn rescan_layer<'m>(
    map: &'m ChunkMap,
    (x, z): (i64, i64),
    cy: i64,
    loaded: &[i64],
    layers: &RangeInclusive<i64>,
) -> Option<LayerData<'m>> {
    if layers.is_empty() || cy < *layers.start() {
        return None;
    }
    if let Some(id) = map.id_at(ChunkPos::new(x, cy, z)) {
        match map.slots[id].data() {
            DataState::Ready(data) => return Some(LayerData::Ref(data)),
            DataState::Missing => return Some(LayerData::Air),
            DataState::Queued | DataState::Generating => {}
        }
    }
    if cy > *layers.end() {
        let end = *layers.end();
        let next = layer_below(loaded, cy).map_or(end, |below| below.max(end));
        return Some(LayerData::AirUntil(next));
    }
    None
}

/// `WorldSettings::light_cache_radius`, clamped to [`MAX_LIGHT_CACHE_RADIUS`].
fn light_cache_radius(settings: &WorldSettings) -> u32 {
    settings.light_cache_radius.min(MAX_LIGHT_CACHE_RADIUS)
}

/// Calls `f` for every position within `r` chunks of `c` (squared distance at most `r^2`) that
/// is not within `ro` of `o` (`minus = Some((o, ro))`), column by column: a short move visits
/// only the new shell.
fn sphere_minus(c: ChunkPos, r: u32, minus: Option<(ChunkPos, u32)>, mut f: impl FnMut(ChunkPos)) {
    let r = i64::from(r);
    for dx in -r..=r {
        for dz in -r..=r {
            let h = dx * dx + dz * dz;
            if h > r * r {
                continue;
            }
            let span = (r * r - h).isqrt();
            let (x, z) = (c.x + dx, c.z + dz);
            let (lo, hi) = (c.y - span, c.y + span);
            // The part of this column inside the other sphere, if it meets it.
            let cut = minus.and_then(|(o, ro)| {
                let ro = i128::from(ro);
                let rest = ro * ro - (i128::from(x - o.x).pow(2) + i128::from(z - o.z).pow(2));
                // At most `ro`, so it fits.
                let span = i64::try_from(rest.max(0).isqrt()).ok()?;
                (rest >= 0).then_some((o.y - span, o.y + span))
            });
            let mut visit = |ys: RangeInclusive<i64>| {
                for y in ys {
                    f(ChunkPos::new(x, y, z));
                }
            };
            match cut {
                Some((a, b)) => {
                    visit(lo..=hi.min(a - 1));
                    visit(lo.max(b + 1)..=hi);
                }
                None => visit(lo..=hi),
            }
        }
    }
}

/// The highest channel of `v`.
fn max_level(v: LightValue) -> u8 {
    v.sky().max(v.red()).max(v.green()).max(v.blue())
}

/// The slots of the 26 neighbors of `pos`, in [`NEIGHBOR_OFFSETS`] order (one lookup each).
fn neighbor_ids(map: &ChunkMap, pos: ChunkPos) -> [Option<ChunkId>; 26] {
    std::array::from_fn(|i| map.id_at(pos.offset(NEIGHBOR_OFFSETS[i])))
}

/// [`own_neighbor_bits`] from the neighbors' slots already looked up ([`neighbor_ids`]).
fn neighbor_bits(
    map: &ChunkMap,
    planner: &Planner,
    pos: ChunkPos,
    near: &[Option<ChunkId>; 26],
) -> u32 {
    let mut bits = 0;
    for (i, (offset, n)) in NEIGHBOR_OFFSETS.iter().zip(near).enumerate() {
        let ready = !planner.in_bounds(pos.y + i64::from(offset.y))
            || n.and_then(|n| map.get(n))
                .is_some_and(|n| matches!(n.data(), DataState::Ready(_) | DataState::Missing));
        if ready {
            bits |= 1 << i;
        }
    }
    bits
}

/// Neighbor bits of a slot at `pos` from the current map: outside the vertical bounds counts as
/// ready, as do loaded neighbors whose data is `Ready` or `Missing`.
fn own_neighbor_bits(map: &ChunkMap, planner: &Planner, pos: ChunkPos) -> u32 {
    let mut bits = 0;
    for (i, offset) in NEIGHBOR_OFFSETS.iter().enumerate() {
        let npos = pos.offset(*offset);
        let ready = !planner.in_bounds(npos.y)
            || map
                .id_at(npos)
                .and_then(|n| map.get(n))
                .is_some_and(|n| matches!(n.data(), DataState::Ready(_) | DataState::Missing));
        if ready {
            bits |= 1 << i;
        }
    }
    bits
}

/// True if slot `id` is loaded and a member of an edit batch.
fn in_batch(map: &ChunkMap, id: ChunkId) -> bool {
    map.get(id).is_some_and(|s| s.batch.is_some())
}

/// Uniform chunk of a state without geometry (air): meshes to nothing whatever its neighbors.
fn is_uniform_empty(data: &ChunkData, registry: &BlockRegistry) -> bool {
    data.as_uniform()
        .is_some_and(|s| registry.shape(s) == RenderShape::None)
}

/// Indices of the 6 face neighbors in [`NEIGHBOR_OFFSETS`], in [`Face::ALL`] order.
const FACE_NEIGHBORS: [usize; 6] = {
    let mut out = [0; 6];
    let mut f = 0;
    while f < 6 {
        out[f] = match neighbor_index(Face::ALL[f].normal()) {
            Some(i) => i,
            None => panic!("a face normal is a neighbor offset"),
        };
        f += 1;
    }
    out
};

/// The `Ready` data of the 26 neighbors of one chunk, each looked up on first use only.
#[derive(Default)]
struct NearData<'a> {
    /// Bit `i` set once neighbor `i` was looked up.
    looked: u32,
    data: [Option<&'a Arc<ChunkData>>; 26],
}

impl<'a> NearData<'a> {
    /// `Ready` data of neighbor `i` of `pos` (`None` if not loaded, or `Missing`, `Queued` or
    /// `Generating`: missing chunks mesh as air, like chunks outside the vertical bounds).
    fn get(&mut self, map: &'a ChunkMap, pos: ChunkPos, i: usize) -> Option<&'a Arc<ChunkData>> {
        if self.looked & (1 << i) == 0 {
            self.looked |= 1 << i;
            self.data[i] = map.data_at(pos.offset(NEIGHBOR_OFFSETS[i]));
        }
        self.data[i]
    }
}

/// Uniform opaque chunk whose 6 face neighbors are loaded uniform opaque chunks: fully enclosed,
/// meshes to nothing. The face neighbors it looks up stay in `near`.
fn is_enclosed_opaque<'a>(
    map: &'a ChunkMap,
    registry: &BlockRegistry,
    pos: ChunkPos,
    data: &ChunkData,
    near: &mut NearData<'a>,
) -> bool {
    let opaque = |d: &ChunkData| d.as_uniform().is_some_and(|s| registry.occludes(s));
    opaque(data)
        && FACE_NEIGHBORS
            .iter()
            .all(|&i| near.get(map, pos, i).is_some_and(|d| opaque(d)))
}

#[cfg(test)]
mod tests;
