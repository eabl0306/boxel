//! The chunk streaming state machine behind the world systems: interest (load/unload), edits,
//! job dispatch and result collection. ECS-free: the systems in `plugin.rs` are thin wrappers,
//! so the rules are unit tested directly.
//!
//! Slot invariants:
//! - `neighbors_ready` bit `i` is set iff neighbor `i` is outside the generator's vertical range
//!   or has a slot whose data is `Ready` or `Missing`.
//! - A slot has at most one job in flight (generation runs before meshing, and a mesh job is only
//!   dispatched from `MeshState::Queued`).
//! - `WorldJobs::published` holds exactly the chunks whose last event was an `Upsert`, so every
//!   `Upsert` is eventually followed by a `Remove` (unload, remesh to empty) or a replacing
//!   `Upsert`.
//! - A published chunk has a `Ready` mesh, or has [`Interest::MESH`] and a remesh `Queued` or
//!   `InFlight` (so `ChunkMap::ready_meshes()` only misses published chunks during a remesh).
//! - An `Empty` mesh is never stale: a data change remeshes it, and unloading a neighbor resets
//!   it (a `Ready` mesh kept in the hysteresis band is checked by epoch when wanted again).
use crate::interest::{self, ViewerChunk};
use crate::jobs::JobExecutor;
use crate::scheduler::{JobKind, JobScheduler};
use crate::{
    BlockEdit, ChunkId, ChunkMap, ChunkMeshEvent, ChunkSlot, DataState, Interest, MeshState,
    WorldContent, WorldSettings,
};
use boxel_ecs::prelude::*;
use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, Mesher};
use boxel_voxel::{
    BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, Face, LocalPos, NEIGHBOR_OFFSETS,
    RenderShape,
};
use crossbeam_channel::{Receiver, Sender};
use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// `neighbors_ready` with all 26 bits set.
pub(crate) const ALL_NEIGHBORS: u32 = (1 << 26) - 1;

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
    tx: Option<Sender<JobMessage>>,
}

impl ResultToken {
    fn send(mut self, message: JobMessage) {
        if let Some(tx) = self.tx.take() {
            // Err only if the world (receiver) is gone: nobody is waiting for the result.
            let _ = tx.send(message);
        }
    }
}

impl Drop for ResultToken {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(JobMessage::Failed {
                id: self.id,
                kind: self.kind,
                panic: "job dropped before sending a result".to_string(),
            });
        }
    }
}

thread_local! {
    /// One mesher per worker thread (~210 KiB of scratch, reused across jobs).
    static MESHER: RefCell<Mesher> = RefCell::new(Mesher::new());
}

/// How mesh jobs turn their input into a mesh (a seam for unit tests).
pub(crate) type MeshFn = fn(&MeshInput<'_>, &MaterialTable) -> ChunkMesh;

/// The production [`MeshFn`]: this thread's `boxel_mesh::Mesher`.
pub(crate) fn mesh_with_thread_mesher(
    input: &MeshInput<'_>,
    materials: &MaterialTable,
) -> ChunkMesh {
    MESHER.with_borrow_mut(|mesher| mesher.mesh_chunk(input, materials))
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
/// panicking body sends `Failed`, a dropped job sends `Failed` through the token.
fn spawn_job(
    executor: &dyn JobExecutor,
    tx: &Sender<JobMessage>,
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
            token.send(JobMessage::Cancelled { id, kind });
            return;
        }
        let message = match catch_unwind(AssertUnwindSafe(body)) {
            Ok(message) => message,
            Err(payload) => JobMessage::Failed {
                id,
                kind,
                panic: panic_text(payload.as_ref()),
            },
        };
        token.send(message);
    }));
}

/// Viewer chunks, refreshed every frame from `Transform` + `ChunkViewer` (sorted).
#[derive(Resource, Debug, Default)]
pub(crate) struct ViewerChunks(pub(crate) Vec<ViewerChunk>);

/// What the state machine works on during one system run: the map, the content and the
/// `ChunkMeshEvent`s to publish (written by the calling system).
pub(crate) struct Ctx<'a> {
    pub(crate) map: &'a mut ChunkMap,
    pub(crate) content: &'a WorldContent,
    pub(crate) events: &'a mut Vec<ChunkMeshEvent>,
    vertical: Option<RangeInclusive<i32>>,
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
            vertical: None,
        }
    }

    /// The generator's vertical chunk range (queried once per run).
    fn vertical(&mut self) -> RangeInclusive<i32> {
        self.vertical
            .get_or_insert_with(|| self.content.generator.vertical_chunks())
            .clone()
    }
}

/// Private job state of the world (scheduler, result channel, executor, event bookkeeping).
#[derive(Resource)]
pub(crate) struct WorldJobs {
    pub(crate) scheduler: JobScheduler,
    tx: Sender<JobMessage>,
    rx: Receiver<JobMessage>,
    executor: Arc<dyn JobExecutor>,
    /// Mesh job body (always [`mesh_with_thread_mesher`] outside unit tests).
    pub(crate) mesh_fn: MeshFn,
    /// Mesh results waiting for the event budget (`scheduler.complete()` already called; their
    /// slots stay `InFlight` until applied, so the world is not settled while any wait).
    pending: VecDeque<MeshedResult>,
    /// Chunks whose last published event was an `Upsert`.
    published: HashSet<ChunkId, foldhash::fast::FixedState>,
    /// Viewers of the last interest computation (`None` before the first one).
    viewers: Option<Vec<ViewerChunk>>,
    /// Failed jobs since start (the `failed=` field of the `"world"` board line).
    pub(crate) failed: usize,
    /// Chunks whose failure was already error-logged.
    failure_logged: HashSet<ChunkId, foldhash::fast::FixedState>,
}

impl WorldJobs {
    /// Empty state with `max_in_flight` job slots.
    pub(crate) fn new(max_in_flight: usize, executor: Arc<dyn JobExecutor>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded();
        Self {
            scheduler: JobScheduler::new(max_in_flight),
            tx,
            rx,
            executor,
            mesh_fn: mesh_with_thread_mesher,
            pending: VecDeque::new(),
            published: HashSet::default(),
            viewers: None,
            failed: 0,
            failure_logged: HashSet::default(),
        }
    }

    /// Priority of a new job for `pos` (nearest viewer of the last interest computation).
    fn priority(&self, pos: ChunkPos) -> u32 {
        interest::priority(pos, self.viewers.as_deref().unwrap_or_default())
    }

    /// True if `viewers` differ from the last interest computation (or there was none).
    pub(crate) fn viewers_changed(&self, viewers: &[ViewerChunk]) -> bool {
        self.viewers.as_deref() != Some(viewers)
    }

    /// True if job results wait in the channel or for the event budget.
    pub(crate) fn has_results(&self) -> bool {
        !self.rx.is_empty() || !self.pending.is_empty()
    }

    // ---- Interest ----------------------------------------------------------------------------

    /// Creates, updates and unloads slots for `viewers`. No-op unless the viewers changed since
    /// the last call (or on the first call).
    pub(crate) fn update_interest(
        &mut self,
        cx: &mut Ctx<'_>,
        settings: &WorldSettings,
        viewers: &[ViewerChunk],
    ) {
        if !self.viewers_changed(viewers) {
            return;
        }
        self.viewers = Some(viewers.to_vec());
        let vertical = cx.vertical();
        let wanted = interest::wanted_chunks(viewers, &vertical);

        let existing: Vec<ChunkId> = cx.map.slots.keys().collect();
        for id in existing {
            let slot = &cx.map.slots[id];
            let pos = slot.key.pos;
            let pinned = slot.interest & Interest::PINNED;
            let want = wanted.get(&pos).copied().unwrap_or(Interest::empty());
            if want.is_empty()
                && pinned.is_empty()
                && (matches!(slot.data, DataState::Queued)
                    || interest::beyond_hysteresis(pos, viewers, settings.unload_hysteresis))
            {
                // Unwanted data that was never started is dropped right away; anything else
                // waits for the hysteresis band.
                self.unload(cx, id);
            } else {
                self.set_interest(cx, id, want | pinned);
            }
        }

        for (&pos, &want) in &wanted {
            if !cx.map.index.contains_key(&pos) {
                self.create_slot(cx, settings, &vertical, pos, want);
            }
        }

        let map = &*cx.map;
        let viewers = self.viewers.as_deref().unwrap_or_default();
        self.scheduler.reprioritize(|id, kind| {
            let slot = map.slots.get(id)?;
            let queued = match kind {
                JobKind::Generate => matches!(slot.data, DataState::Queued),
                JobKind::Mesh => matches!(slot.mesh, MeshState::Queued),
            };
            queued.then(|| interest::priority(slot.key.pos, viewers))
        });
    }

    /// New slot at `pos` (data `Queued`, generation job queued).
    pub(crate) fn create_slot(
        &mut self,
        cx: &mut Ctx<'_>,
        settings: &WorldSettings,
        vertical: &RangeInclusive<i32>,
        pos: ChunkPos,
        interest: Interest,
    ) -> ChunkId {
        let neighbors_ready = own_neighbor_bits(cx.map, vertical, pos);
        let slot = ChunkSlot {
            key: ChunkKey {
                dim: settings.dimension,
                lod: 0,
                pos,
            },
            data: DataState::Queued,
            mesh: MeshState::None,
            interest: Interest::empty(),
            neighbors_ready,
            epoch: 0,
            remesh_pending: false,
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let id = cx.map.slots.insert(slot);
        cx.map.index.insert(pos, id);
        self.scheduler
            .push(id, JobKind::Generate, self.priority(pos));
        self.set_interest(cx, id, interest);
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
                slot.remesh_pending = false;
                match slot.mesh {
                    MeshState::Queued => self.drop_mesh(cx, id),
                    MeshState::InFlight { .. } => self.unpublish(cx, id),
                    MeshState::None | MeshState::Ready { .. } | MeshState::Empty => {}
                }
            }
            (false, true) => {
                let priority = self.priority(slot.key.pos);
                match &slot.mesh {
                    MeshState::None => {
                        slot.mesh = MeshState::Queued;
                        self.schedule_mesh(cx, id, priority);
                    }
                    MeshState::Ready {
                        data_version,
                        epoch,
                        ..
                    } => {
                        let current = match &slot.data {
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
    pub(crate) fn unload(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        let Some(slot) = cx.map.slots.remove(id) else {
            return;
        };
        cx.map.index.remove(&slot.key.pos);
        slot.cancel.store(true, Ordering::Relaxed);
        self.failure_logged.remove(&id);
        if self.published.remove(&id) {
            cx.events.push(ChunkMeshEvent::Remove { id, key: slot.key });
        }
        for (i, offset) in NEIGHBOR_OFFSETS.iter().enumerate() {
            let npos = slot.key.pos.offset(*offset);
            let Some(&n) = cx.map.index.get(&npos) else {
                continue;
            };
            let neighbor = &mut cx.map.slots[n];
            neighbor.neighbors_ready &= !(1 << opposite_neighbor(i));
            neighbor.epoch = neighbor.epoch.wrapping_add(1);
            if matches!(neighbor.mesh, MeshState::Empty) {
                // Nothing is published for it. Without mesh interest (the usual case: a mesh
                // chunk's neighbors are always wanted) this resets it to `None`, so it is meshed
                // again when wanted; a `Ready` mesh is checked by epoch instead.
                let priority = self.priority(npos);
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
        if !matches!(slot.mesh, MeshState::Queued) || !slot.interest.contains(Interest::MESH) {
            return;
        }
        match &slot.data {
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
        let slot = &mut cx.map.slots[id];
        if let MeshState::InFlight { .. } = slot.mesh {
            // The running job used the old data or epoch: its result is rejected, then requeued
            // (or dropped if the mesh is no longer wanted).
            slot.remesh_pending = slot.interest.contains(Interest::MESH);
            return;
        }
        if slot.interest.contains(Interest::MESH) {
            slot.mesh = MeshState::Queued;
            self.schedule_mesh(cx, id, priority);
        } else {
            self.drop_mesh(cx, id);
        }
    }

    /// `Remove` if the slot's mesh was published (its state is left as is).
    fn unpublish(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        if self.published.remove(&id) {
            let key = cx.map.slots[id].key;
            cx.events.push(ChunkMeshEvent::Remove { id, key });
        }
    }

    /// `MeshState::Empty`, and `Remove` if a mesh was published.
    fn set_mesh_empty(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        let slot = &mut cx.map.slots[id];
        slot.mesh = MeshState::Empty;
        slot.remesh_pending = false;
        self.unpublish(cx, id);
    }

    /// `MeshState::None`, and `Remove` if a mesh was published.
    fn drop_mesh(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        let slot = &mut cx.map.slots[id];
        slot.mesh = MeshState::None;
        slot.remesh_pending = false;
        self.unpublish(cx, id);
    }

    /// The slot's data became `Ready` or `Missing`: refresh its own neighbor bits, set its bit in
    /// the 26 neighbors and schedule every mesh that became possible.
    fn data_available(&mut self, cx: &mut Ctx<'_>, id: ChunkId) {
        let vertical = cx.vertical();
        let pos = cx.map.slots[id].key.pos;
        cx.map.slots[id].neighbors_ready = own_neighbor_bits(cx.map, &vertical, pos);
        self.schedule_mesh(cx, id, self.priority(pos));
        for (i, offset) in NEIGHBOR_OFFSETS.iter().enumerate() {
            let npos = pos.offset(*offset);
            let Some(&n) = cx.map.index.get(&npos) else {
                continue;
            };
            let bit = 1 << opposite_neighbor(i);
            let neighbor = &mut cx.map.slots[n];
            if neighbor.neighbors_ready & bit == 0 {
                neighbor.neighbors_ready |= bit;
                if neighbor.neighbors_ready == ALL_NEIGHBORS {
                    self.schedule_mesh(cx, n, self.priority(npos));
                }
            }
        }
    }

    // ---- Edits -------------------------------------------------------------------------------

    /// Applies edits grouped by chunk (copy-on-write, `ChunkData::set` bumps the version);
    /// remeshes each changed chunk and the face neighbors its changed border voxels touch (their
    /// epoch is bumped), all at priority 0. Edits to chunks that are not `Ready` are dropped.
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
            let DataState::Ready(data) = &mut cx.map.slots[id].data else {
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
            let mut touched = 0u8;
            for (local, state) in changes {
                if data.set(local, state) != state {
                    touched |= local.border_faces();
                }
            }
            self.request_remesh(cx, id, 0);
            for face in Face::ALL {
                if touched & (1 << face.index()) == 0 {
                    continue;
                }
                if let Some(n) = cx.map.id_at(chunk.offset(face.normal())) {
                    let neighbor = &mut cx.map.slots[n];
                    neighbor.epoch = neighbor.epoch.wrapping_add(1);
                    self.request_remesh(cx, n, 0);
                }
            }
        }
    }

    // ---- Dispatch ----------------------------------------------------------------------------

    /// Starts queued jobs while below the in-flight cap. Jobs whose slot no longer wants them are
    /// discarded (their in-flight slot is released immediately).
    pub(crate) fn dispatch(&mut self, cx: &mut Ctx<'_>) {
        while let Some(job) = self.scheduler.pop_ready() {
            let started = match job.kind {
                JobKind::Generate => self.start_generate(cx, job.id),
                JobKind::Mesh => self.start_mesh(cx, job.id),
            };
            if !started {
                self.scheduler.complete();
            }
        }
    }

    fn start_generate(&mut self, cx: &mut Ctx<'_>, id: ChunkId) -> bool {
        let Some(slot) = cx.map.slots.get_mut(id) else {
            return false;
        };
        if !matches!(slot.data, DataState::Queued) {
            return false;
        }
        slot.data = DataState::Generating;
        let key = slot.key;
        let generator = Arc::clone(&cx.content.generator);
        let registry = Arc::clone(&cx.content.registry);
        spawn_job(
            self.executor.as_ref(),
            &self.tx,
            id,
            JobKind::Generate,
            Arc::clone(&slot.cancel),
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
        if !matches!(slot.mesh, MeshState::Queued)
            || !slot.interest.contains(Interest::MESH)
            || slot.neighbors_ready != ALL_NEIGHBORS
        {
            return false;
        }
        let DataState::Ready(center) = &slot.data else {
            return false;
        };
        let pos = slot.key.pos;
        let registry = &cx.content.registry;
        if is_uniform_empty(center, registry) || is_enclosed_opaque(map, registry, pos, center) {
            self.set_mesh_empty(cx, id);
            return false;
        }
        let center = Arc::clone(center);
        let neighbors: [Option<Arc<ChunkData>>; 26] = std::array::from_fn(|i| {
            let neighbor = map.get(map.id_at(pos.offset(NEIGHBOR_OFFSETS[i]))?)?;
            match &neighbor.data {
                DataState::Ready(d) => Some(Arc::clone(d)),
                // Missing chunks are air, like chunks outside the vertical range.
                DataState::Missing | DataState::Queued | DataState::Generating => None,
            }
        });
        let (data_version, epoch) = (center.version, slot.epoch);
        let cancel = Arc::clone(&slot.cancel);
        let slot = &mut cx.map.slots[id];
        slot.mesh = MeshState::InFlight {
            data_version,
            epoch,
        };
        slot.remesh_pending = false;
        let materials = Arc::clone(&cx.content.materials);
        let mesh_fn = self.mesh_fn;
        spawn_job(
            self.executor.as_ref(),
            &self.tx,
            id,
            JobKind::Mesh,
            cancel,
            move || {
                let input = MeshInput {
                    center: &center,
                    neighbors: std::array::from_fn(|i| neighbors[i].as_deref()),
                };
                let mesh = mesh_fn(&input, &materials);
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

    /// Drains job results within `settings.collect_budget` (at least one per call; every message
    /// releases its in-flight slot) and applies mesh results within
    /// `settings.max_mesh_events_per_frame` events (the rest wait for the next call).
    pub(crate) fn collect(&mut self, cx: &mut Ctx<'_>, settings: &WorldSettings) {
        let start = Instant::now();
        while let Ok(message) = self.rx.try_recv() {
            self.scheduler.complete();
            match message {
                JobMessage::Generated { id, data } => self.on_generated(cx, id, data),
                JobMessage::Meshed {
                    id,
                    data_version,
                    epoch,
                    mesh,
                } => self.pending.push_back(MeshedResult {
                    id,
                    data_version,
                    epoch,
                    mesh,
                }),
                JobMessage::Cancelled { id, kind } => {
                    tracing::trace!(?id, ?kind, "chunk job cancelled (slot unloaded)");
                }
                JobMessage::Failed { id, kind, panic } => self.on_failed(cx, id, kind, &panic),
            }
            if start.elapsed() >= settings.collect_budget {
                break;
            }
        }

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
        let Some(slot) = cx.map.slots.get_mut(id) else {
            return;
        };
        if !matches!(slot.data, DataState::Generating) || slot.cancel.load(Ordering::Relaxed) {
            return;
        }
        slot.data = DataState::Ready(Arc::new(data));
        self.data_available(cx, id);
    }

    fn on_failed(&mut self, cx: &mut Ctx<'_>, id: ChunkId, kind: JobKind, panic: &str) {
        self.failed += 1;
        let Some(slot) = cx.map.slots.get_mut(id) else {
            tracing::debug!(?kind, "job for an unloaded chunk failed: {panic}");
            return;
        };
        if self.failure_logged.insert(id) {
            tracing::error!(chunk = ?slot.key.pos, ?kind, "chunk job failed: {panic}");
        }
        match kind {
            JobKind::Generate => {
                if matches!(slot.data, DataState::Generating) {
                    // Permanently air, like chunks outside the vertical range.
                    slot.data = DataState::Missing;
                    self.data_available(cx, id);
                }
            }
            JobKind::Mesh => {
                if !matches!(slot.mesh, MeshState::InFlight { .. }) {
                    return;
                }
                if slot.remesh_pending {
                    // The data changed meanwhile: try again with the new data.
                    slot.remesh_pending = false;
                    slot.mesh = MeshState::Queued;
                    let priority = self.priority(slot.key.pos);
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
        let Some(slot) = cx.map.slots.get_mut(id) else {
            return;
        };
        let MeshState::InFlight {
            data_version,
            epoch,
        } = slot.mesh
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
        let current_version = match &slot.data {
            DataState::Ready(d) => Some(d.version),
            _ => None,
        };
        let fresh = !slot.remesh_pending
            && (data_version, epoch) == (result.data_version, result.epoch)
            && current_version == Some(result.data_version)
            && slot.epoch == result.epoch;
        if !fresh {
            slot.remesh_pending = false;
            slot.mesh = MeshState::Queued;
            let priority = self.priority(slot.key.pos);
            self.schedule_mesh(cx, id, priority);
            return;
        }
        if result.mesh.is_empty() {
            self.set_mesh_empty(cx, id);
            return;
        }
        let mesh = Arc::new(result.mesh);
        slot.mesh = MeshState::Ready {
            mesh: Arc::clone(&mesh),
            data_version: result.data_version,
            epoch: result.epoch,
        };
        self.published.insert(id);
        cx.events.push(ChunkMeshEvent::Upsert {
            id,
            key: slot.key,
            mesh,
        });
    }
}

/// Neighbor bits of a slot at `pos` from the current map: outside `vertical` counts as ready,
/// as do loaded neighbors whose data is `Ready` or `Missing`.
fn own_neighbor_bits(map: &ChunkMap, vertical: &RangeInclusive<i32>, pos: ChunkPos) -> u32 {
    let mut bits = 0;
    for (i, offset) in NEIGHBOR_OFFSETS.iter().enumerate() {
        let npos = pos.offset(*offset);
        let ready = !vertical.contains(&npos.y)
            || map
                .id_at(npos)
                .and_then(|n| map.get(n))
                .is_some_and(|n| matches!(n.data, DataState::Ready(_) | DataState::Missing));
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

/// Uniform opaque chunk whose 6 face neighbors are loaded uniform opaque chunks: fully enclosed,
/// meshes to nothing.
fn is_enclosed_opaque(
    map: &ChunkMap,
    registry: &BlockRegistry,
    pos: ChunkPos,
    data: &ChunkData,
) -> bool {
    let opaque = |d: &ChunkData| {
        d.as_uniform()
            .is_some_and(|s| registry.shape(s) == RenderShape::Opaque)
    };
    opaque(data)
        && Face::ALL.iter().all(|face| {
            map.data_at(pos.offset(face.normal()))
                .is_some_and(|d| opaque(d))
        })
}

#[cfg(test)]
mod tests;
