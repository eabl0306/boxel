//! The chunk streaming state machine behind the world systems: interest (load/unload), edits,
//! job dispatch and result collection. ECS-free: the systems in `plugin.rs` are thin wrappers,
//! so the rules are unit tested directly.
//!
//! Slot invariants:
//! - In a slot whose data is `Ready` or `Missing`, `neighbors_ready` bit `i` is set iff neighbor
//!   `i` is outside `WorldSettings::vertical_bounds` or has a slot whose data is `Ready` or
//!   `Missing`. Slots still waiting for their data get their bits when it arrives (nothing reads
//!   them before).
//! - A slot has at most one job in flight (generation runs before meshing, and a mesh job is only
//!   dispatched from `MeshState::Queued`).
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
use crate::scheduler::{JobKind, JobScheduler};
use crate::{
    BlockEdit, ChunkId, ChunkMap, ChunkMeshEvent, ChunkSlot, DataState, Interest, MeshState,
    WorldContent, WorldSettings,
};
use boxel_ecs::prelude::*;
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, Mesher};
use boxel_voxel::{
    BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, Face, LocalPos, NEIGHBOR_OFFSETS,
    RenderShape, neighbor_index,
};
use crossbeam_channel::{Receiver, Sender};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// `neighbors_ready` with all 26 bits set.
pub(crate) const ALL_NEIGHBORS: u32 = (1 << 26) - 1;

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
    /// Generation finished.
    Generated { id: ChunkId, data: ChunkData },
    /// Meshing finished (`mesh` may be empty).
    Meshed {
        id: ChunkId,
        data_version: u32,
        epoch: u32,
        mesh: ChunkMesh,
    },
    /// The job saw the slot's cancel flag and did nothing.
    Cancelled { id: ChunkId, kind: JobKind },
    /// The job panicked, or was dropped without running.
    Failed {
        id: ChunkId,
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

/// A `Meshed` result waiting for the per-frame event budget.
#[derive(Debug)]
struct MeshedResult {
    id: ChunkId,
    data_version: u32,
    epoch: u32,
    mesh: ChunkMesh,
}

/// Owned by every spawned job: sends the job's single message, or `Failed` from `Drop` if the
/// job ends (panic outside the guarded body, executor dropping the closure) without sending.
struct ResultToken {
    id: ChunkId,
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
/// corner voxel, none for an inner one.
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
    id: ChunkId,
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
    pub(crate) content: &'a WorldContent,
    pub(crate) events: &'a mut Vec<ChunkMeshEvent>,
    generator_layers: Option<RangeInclusive<i64>>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(
        map: &'a mut ChunkMap,
        content: &'a WorldContent,
        events: &'a mut Vec<ChunkMeshEvent>,
    ) -> Self {
        Self {
            map,
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
    /// Mesh results waiting for the event budget (`scheduler.complete()` already called; their
    /// slots stay `InFlight` until applied, so the world is not settled while any wait).
    pending: VecDeque<MeshedResult>,
    /// Chunks whose last published event was an `Upsert`.
    published: HashSet<ChunkId, foldhash::fast::FixedState>,
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
            pending: VecDeque::new(),
            published: HashSet::default(),
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

    /// True if mesh results wait for the event budget.
    pub(crate) fn has_meshes(&self) -> bool {
        !self.pending.is_empty()
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
    /// interest work (positions, stale priorities) is still pending.
    pub(crate) fn interest_pending(
        &self,
        viewers: &[(u64, Viewer)],
        settings: &WorldSettings,
    ) -> bool {
        self.planner.backlog() > 0
            || self.scheduler.stale() > 0
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
    /// `settings.interest_budget`.
    pub(crate) fn update_interest(
        &mut self,
        cx: &mut Ctx<'_>,
        settings: &WorldSettings,
        viewers: &[(u64, Viewer)],
    ) {
        {
            let _span = frame_span!("world.plan");
            self.plan_interest(cx, settings, viewers);
        }
        self.process_interest(cx, settings, settings.interest_budget);
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
    /// budget, more while it lasts). Positions go first so the refresh drops the jobs of the slots
    /// they unloaded.
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
        let _span = frame_span!("world.priorities");
        self.refresh_priorities(cx, PRIORITY_BATCH);
        while self.scheduler.stale() > 0 && start.elapsed() < budget {
            self.refresh_priorities(cx, PRIORITY_BATCH);
        }
    }

    /// Recomputes up to `max` stale job priorities for the current viewers (jobs whose slot no
    /// longer wants them are dropped).
    fn refresh_priorities(&mut self, cx: &Ctx<'_>, max: usize) {
        let (map, planner) = (&*cx.map, &self.planner);
        self.scheduler
            .refresh_priorities(max, |id, kind| current_priority(map, planner, id, kind));
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
        let slot = ChunkSlot::new(ChunkKey {
            dim: settings.dimension,
            lod: 0,
            pos,
        });
        let id = cx.map.insert(slot);
        // Gaining mesh interest only queues the mesh here (the data is not ready yet): no slot
        // comes or goes, so `near` stays valid.
        self.set_interest(cx, id, interest);
        if let Some(near) = near {
            cx.map
                .set_data(id, DataState::Ready(self.uniform(BlockState::AIR)));
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
        let Some(slot) = cx.map.remove(id) else {
            return;
        };
        slot.cancel.store(true, Ordering::Relaxed);
        self.failure_logged.remove(&id);
        if self.published.remove(&id) {
            cx.events.push(ChunkMeshEvent::Remove {
                id,
                key: slot.key(),
            });
        }
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
    /// `Ready` and all 26 neighbors are ready. Otherwise it waits (it is scheduled again when the
    /// last neighbor becomes ready).
    fn schedule_mesh(&mut self, cx: &mut Ctx<'_>, id: ChunkId, priority: u32) {
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
                self.scheduler.push(id, JobKind::Mesh, priority);
            }
            DataState::Ready(_) | DataState::Queued | DataState::Generating => {}
        }
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
        if self.published.remove(&id) {
            let key = cx.map.slots[id].key();
            cx.events.push(ChunkMeshEvent::Remove { id, key });
        }
    }

    /// `MeshState::Empty`, and `Remove` if a mesh was published.
    fn set_mesh_empty(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        cx.map.set_mesh_and_pending(id, MeshState::Empty, false);
        self.unpublish(cx, id);
    }

    /// `MeshState::None`, and `Remove` if a mesh was published.
    fn drop_mesh(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        cx.map.set_mesh_and_pending(id, MeshState::None, false);
        self.unpublish(cx, id);
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
                }
            }
        }
    }

    // ---- Edits -------------------------------------------------------------------------------

    /// Applies edits grouped by chunk (copy-on-write, `ChunkData::set` bumps the version);
    /// remeshes each changed chunk and every neighbor whose padded region (the voxels its mesh
    /// reads) holds a changed voxel: face neighbors for visibility, and edge and corner
    /// neighbors too, whose AO and corner light read them (their epoch is bumped). All at
    /// priority 0. Edits to chunks that are not `Ready` are dropped.
    pub(crate) fn apply_edits(&mut self, cx: &mut Ctx<'_>, edits: Vec<BlockEdit>) {
        let mut by_chunk: BTreeMap<ChunkPos, Vec<(LocalPos, BlockState)>> = BTreeMap::new();
        for edit in edits {
            let (chunk, local) = edit.pos.split();
            by_chunk.entry(chunk).or_default().push((local, edit.state));
        }
        for (chunk, changes) in by_chunk {
            let Some(id) = cx.map.id_at(chunk) else {
                tracing::debug!(
                    ?chunk,
                    edits = changes.len(),
                    "edits dropped: chunk not loaded"
                );
                continue;
            };
            let Some(data) = cx.map.ready_data_mut(id) else {
                tracing::debug!(
                    ?chunk,
                    edits = changes.len(),
                    "edits dropped: chunk not ready"
                );
                continue;
            };
            if changes
                .iter()
                .all(|&(local, state)| data.get(local) == state)
            {
                continue;
            }
            let data = Arc::make_mut(data);
            let mut touched = 0u32;
            for (local, state) in changes {
                if data.set(local, state) != state {
                    touched |= padded_neighbors(local);
                }
            }
            self.request_remesh(cx, id, 0);
            for (i, offset) in NEIGHBOR_OFFSETS.iter().enumerate() {
                if touched & (1 << i) == 0 {
                    continue;
                }
                if let Some(n) = cx.map.id_at(chunk.offset(*offset)) {
                    let neighbor = &mut cx.map.slots[n];
                    neighbor.epoch = neighbor.epoch.wrapping_add(1);
                    self.request_remesh(cx, n, 0);
                }
            }
        }
    }

    // ---- Jobs --------------------------------------------------------------------------------

    /// Frame bookkeeping before the job step: measures the frame interval, applies the
    /// in-flight cap (`settings.max_jobs_in_flight`, or the automatic one when it is 0) and takes
    /// the mesher options of the jobs it starts.
    pub(crate) fn begin_frame(&mut self, settings: &WorldSettings, now: Instant) {
        self.mesh_options = MeshOptions::with_ao(settings.ao_policy);
        self.auto_cap.observe_frame(now);
        let cap = match settings.max_jobs_in_flight {
            0 => self.auto_cap.cap(),
            n => n,
        };
        self.scheduler.set_max_in_flight(cap);
    }

    /// The job step of a frame: collects finished jobs (freeing their in-flight slots) and starts
    /// queued ones, again and again while results keep arriving, within `settings.job_budget`.
    /// Collecting first means a slot freed by a job that finished since the last frame is reused
    /// in this frame, not the next one.
    pub(crate) fn run_jobs(&mut self, cx: &mut Ctx<'_>, settings: &WorldSettings) {
        let start = Instant::now();
        let budget = settings.job_budget;
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
            let (map, planner) = (&*cx.map, &self.planner);
            let refreshed = self.scheduler.refresh_ahead(DISPATCH_CHECK, |id, kind| {
                current_priority(map, planner, id, kind)
            });
            if refreshed > 0 {
                work += refreshed;
            } else {
                let Some(job) = self.scheduler.pop_ready() else {
                    break;
                };
                let ok = match job.kind {
                    JobKind::Generate => self.start_generate(cx, job.id),
                    JobKind::Mesh => self.start_mesh(cx, job.id),
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
            id,
            JobKind::Generate,
            cancel,
            move || JobMessage::Generated {
                id,
                data: generator.generate(key, &registry),
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
        cx.map.set_mesh_and_pending(
            id,
            MeshState::InFlight {
                data_version,
                epoch,
            },
            false,
        );
        let materials = Arc::clone(&cx.content.materials);
        let (mesh_fn, options) = (self.mesh_fn, self.mesh_options);
        spawn_job(
            self.executor.as_ref(),
            &self.tx,
            id,
            JobKind::Mesh,
            cancel,
            move || {
                // No light yet: open sky everywhere (ADR 0015, phase P3 computes it here).
                let input = MeshInput {
                    center: &center,
                    neighbors: std::array::from_fn(|i| neighbors[i].as_deref()),
                    light: None,
                };
                let mesh = mesh_fn(&input, &materials, options);
                JobMessage::Meshed {
                    id,
                    data_version,
                    epoch,
                    mesh,
                }
            },
        );
        true
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
                JobMessage::Generated { id, data } => {
                    self.auto_cap.observe_job(busy);
                    self.on_generated(cx, id, data);
                }
                JobMessage::Meshed {
                    id,
                    data_version,
                    epoch,
                    mesh,
                } => {
                    self.auto_cap.observe_job(busy);
                    self.pending.push_back(MeshedResult {
                        id,
                        data_version,
                        epoch,
                        mesh,
                    });
                }
                JobMessage::Cancelled { id, kind } => {
                    tracing::trace!(?id, ?kind, "chunk job cancelled (slot unloaded)");
                }
                JobMessage::Failed { id, kind, panic } => self.on_failed(cx, id, kind, &panic),
            }
            if start.elapsed() >= budget {
                break;
            }
        }
        drained
    }

    /// Applies waiting mesh results within `settings.max_mesh_events_per_frame` events.
    pub(crate) fn apply_meshes(&mut self, cx: &mut Ctx<'_>, settings: &WorldSettings) {
        let budget = settings.max_mesh_events_per_frame.max(1);
        let first = cx.events.len();
        while cx.events.len() - first < budget {
            let Some(result) = self.pending.pop_front() else {
                break;
            };
            self.apply_meshed(cx, result);
        }
    }

    fn on_generated(&mut self, cx: &mut Ctx<'_>, id: ChunkId, data: ChunkData) {
        let Some(slot) = cx.map.slots.get(id) else {
            return;
        };
        if !matches!(slot.data(), DataState::Generating) || slot.cancel.load(Ordering::Relaxed) {
            return;
        }
        let data = self.snapshot(data);
        cx.map.set_data(id, DataState::Ready(data));
        self.data_available(cx, id);
    }

    fn on_failed(&mut self, cx: &mut Ctx<'_>, id: ChunkId, kind: JobKind, panic: &str) {
        self.failed += 1;
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
                    cx.map.set_data(id, DataState::Missing);
                    self.data_available(cx, id);
                }
            }
            JobKind::Mesh => {
                if !matches!(slot.mesh(), MeshState::InFlight { .. }) {
                    return;
                }
                if slot.remesh_pending() {
                    // The data changed meanwhile: try again with the new data.
                    let priority = self.priority(slot.pos);
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
            let priority = self.priority(slot.pos);
            cx.map.set_mesh_and_pending(id, MeshState::Queued, false);
            self.schedule_mesh(cx, id, priority);
            return;
        }
        if result.mesh.is_empty() {
            self.set_mesh_empty(cx, id);
            return;
        }
        let key = slot.key();
        let mesh = Arc::new(result.mesh);
        cx.map.set_mesh(
            id,
            MeshState::Ready {
                mesh: Arc::clone(&mesh),
                data_version: result.data_version,
                epoch: result.epoch,
            },
        );
        self.published.insert(id);
        cx.events.push(ChunkMeshEvent::Upsert { id, key, mesh });
    }
}

/// Priority of a queued job for the current viewers, `None` if its slot no longer wants it.
fn current_priority(map: &ChunkMap, planner: &Planner, id: ChunkId, kind: JobKind) -> Option<u32> {
    let slot = map.slots.get(id)?;
    let queued = match kind {
        JobKind::Generate => matches!(slot.data(), DataState::Queued),
        JobKind::Mesh => matches!(slot.mesh(), MeshState::Queued),
    };
    queued.then(|| planner.priority(slot.pos))
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
    let opaque = |d: &ChunkData| {
        d.as_uniform()
            .is_some_and(|s| registry.shape(s) == RenderShape::Opaque)
    };
    opaque(data)
        && FACE_NEIGHBORS
            .iter()
            .all(|&i| near.get(map, pos, i).is_some_and(|d| opaque(d)))
}

#[cfg(test)]
mod tests;
