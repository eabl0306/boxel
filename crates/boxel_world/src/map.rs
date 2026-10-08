use crate::ColumnId;
use crate::visibility::XRAY;
use boxel_ecs::prelude::*;
use boxel_light::ChunkLight;
use boxel_mesh::ChunkMesh;
use boxel_voxel::{
    ALL_LINKS, BlockPos, BlockState, ChunkData, ChunkFaceInfo, ChunkKey, ChunkPos, DimensionId,
    LightValue,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

slotmap::new_key_type! {
    /// Generational chunk handle (stale handles never alias a new chunk).
    pub struct ChunkId;
}

impl ChunkId {
    /// Stable `u64` for the renderer (`KeyData::as_ffi`).
    pub fn to_bits(self) -> u64 {
        slotmap::Key::data(&self).as_ffi()
    }
}

/// [`ChunkSlot::face_links`] while unknown (links use the low 15 bits).
const UNKNOWN_LINKS: u16 = 1 << 15;

/// Data lifecycle (orthogonal to [`MeshState`]).
#[derive(Clone, Debug)]
pub enum DataState {
    /// Waiting for a generation job.
    Queued,
    /// Generation job in flight.
    Generating,
    /// Data available.
    Ready(Arc<ChunkData>),
    /// Generation failed: permanently air.
    Missing,
}

impl DataState {
    /// The chunk's blocks can be read: `Ready`, or `Missing` (read as air).
    pub fn is_readable(&self) -> bool {
        matches!(self, DataState::Ready(_) | DataState::Missing)
    }
}

/// A chunk's blocks became readable or stopped being readable (the world's
/// `ChunkDataEvent` message, recorded by the map as its slots change).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DataChange {
    Loaded(ChunkId, ChunkKey),
    Unloaded(ChunkId, ChunkKey),
}

/// Mesh lifecycle.
#[derive(Clone, Debug)]
pub enum MeshState {
    /// No mesh wanted or not yet requested.
    None,
    /// Waiting for neighbors or a job slot.
    Queued,
    /// Mesh job in flight, built from `data_version` with neighbor `epoch`.
    InFlight {
        /// Center data version used.
        data_version: u32,
        /// Slot epoch used.
        epoch: u32,
    },
    /// Uploaded-able mesh.
    Ready {
        /// The mesh (kept CPU-side for renderer resync after device loss / tier change).
        mesh: Arc<ChunkMesh>,
        /// Center data version used.
        data_version: u32,
        /// Slot epoch used.
        epoch: u32,
    },
    /// Meshed to nothing (all air or fully enclosed).
    Empty,
}

bitflags::bitflags! {
    /// Why a chunk is loaded.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Interest: u8 {
        /// Data wanted (inside data radius).
        const DATA = 1;
        /// Mesh wanted (inside mesh radius).
        const MESH = 1 << 1;
        /// Never unload.
        const PINNED = 1 << 2;
    }
}

/// One chunk's bookkeeping. Its key is stored field by field (see [`ChunkSlot::key`]), so the
/// small fields share the padding a nested `ChunkKey` would leave.
///
/// `data`, `mesh` and `remesh_pending` are read with their getters and changed only through
/// [`ChunkMap`], which counts them for [`ChunkMap::stats`].
#[derive(Clone, Debug)]
pub struct ChunkSlot {
    /// Position (the key's `pos`).
    pub pos: ChunkPos,
    /// Dimension (the key's `dim`).
    pub dim: DimensionId,
    /// Level of detail (the key's `lod`).
    pub lod: u8,
    /// Data state.
    data: DataState,
    /// Mesh state.
    mesh: MeshState,
    /// Which faces of the chunk see each other through its open voxels (cave culling, ADR
    /// 0022): the [`ChunkFaceInfo::links`] of its current data, computed by the generation job,
    /// cleared by an edit that changes what occludes and computed again by the next mesh job.
    /// [`UNKNOWN_LINKS`] while unknown (a `u16` keeps the slot at 112 bytes).
    face_links: u16,
    /// Interest flags.
    pub interest: Interest,
    /// Bit `i` set when neighbor `NEIGHBOR_OFFSETS[i]` is `Ready` or `Missing` (26 bits). Only
    /// kept while this slot's own data is `Ready` or `Missing`: the bits are computed when the
    /// data arrives, and mean nothing before.
    pub neighbors_ready: u32,
    /// Bumped when a neighbor's border data changes (closes the neighbor-staleness gap).
    pub epoch: u32,
    /// [`REMESH_PENDING`], [`SKY_EDITED`], [`LIGHT_JOB`] and [`SHOWN`] (one byte, so the slot
    /// stays at 112 bytes).
    bits: u8,
    /// The mesh job is waiting for a sky tile of its 3x3 columns (counted in
    /// [`WorldStats::waiting_sky`]).
    pub(crate) waits_sky: bool,
    /// The edit batch whose remesh this slot belongs to (its mesh is published with the rest of
    /// the batch).
    pub(crate) batch: Option<u32>,
    /// Set on unload; jobs check it before heavy work (cooperative cancellation).
    pub cancel: Arc<AtomicBool>,
    /// The chunk column of the slot in the `SkyMap`.
    pub(crate) column: ColumnId,
    /// The chunk's light, kept within `WorldSettings::light_cache_radius` of a viewer
    /// ([`ChunkMap::light_at`]): from its mesh job, or from a light job when the mesh job did not
    /// keep it (the chunk was farther then, or it is meshed without a job).
    pub(crate) light: Option<Arc<CachedLight>>,
}

/// [`ChunkSlot::bits`]: a remesh was requested while a job was in flight.
const REMESH_PENDING: u8 = 1;
/// [`ChunkSlot::bits`]: an edit changed a voxel that stops direct sky here: unloading the chunk
/// loses the edit (no persistence yet), so its column is walked again.
const SKY_EDITED: u8 = 1 << 1;
/// [`ChunkSlot::bits`]: a light job for this slot is queued or running (at most one).
const LIGHT_JOB: u8 = 1 << 2;
/// [`ChunkSlot::bits`]: the renderer shows the chunk as its blocks are ([`ChunkSlot::shown`]).
const SHOWN: u8 = 1 << 3;

/// A chunk's cached light and the content it was computed from (one allocation, so the slot
/// stays small).
#[derive(Clone, Debug)]
pub(crate) struct CachedLight {
    pub(crate) light: ChunkLight,
    /// Center data version of the light box.
    pub(crate) data_version: u32,
    /// Slot epoch of the light box (bumped by every change of the neighbors or of the sky tops
    /// that may change the chunk's light).
    pub(crate) epoch: u32,
}

/// World counters. Also an ECS resource, refreshed every frame in `Last` by the world plugin
/// (including the scheduler counts); apps read `Res<WorldStats>`.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorldStats {
    /// Slots.
    pub chunks: usize,
    /// Data Ready.
    pub data_ready: usize,
    /// Queued + Generating.
    pub generating: usize,
    /// Mesh Ready (non-empty).
    pub mesh_ready: usize,
    /// Mesh Empty.
    pub mesh_empty: usize,
    /// Mesh Queued + `InFlight` + remesh pending.
    pub meshing: usize,
    /// Jobs waiting in the scheduler.
    pub queued_jobs: usize,
    /// Jobs running.
    pub in_flight: usize,
    /// Viewer changes not applied yet (chunk positions and work lists waiting for the interest
    /// budget); 0 when every wanted chunk has a slot.
    pub interest_backlog: usize,
    /// Light boxes of mesh jobs computed with the floods, since start (ADR 0015).
    pub light_full: usize,
    /// Light boxes of mesh jobs that took a fast path (uniformly dark or open sky, or below the
    /// walk floor), since start.
    pub light_fast: usize,
    /// Light jobs finished, since start: the light of a chunk near a viewer whose mesh job did
    /// not keep it (chunks meshed without a job, such as all-air ones, and chunks that came
    /// within `WorldSettings::light_cache_radius` after their mesh job started).
    pub light_jobs: usize,
    /// Chunk columns with a sky tile slot (one per column with a loaded chunk).
    pub sky_columns: usize,
    /// Sky walks finished, since start.
    pub sky_walks: usize,
    /// Chunks generated by sky walks for layers that were not loaded, since start.
    pub sky_private_generations: usize,
    /// Mesh jobs waiting for the sky tile of a column around them (part of `meshing`).
    pub waiting_sky: usize,
    /// Edit batches whose meshes were held back so they publish together, since start.
    pub edit_batches_held: usize,
    /// Edit batches published in parts because some meshes took longer than
    /// `WorldSettings::edit_batch_hold`, or closed after 120 frames and a second without any
    /// mesh (their meshes then go out on their own), since start.
    pub edit_batches_split: usize,
    /// Chunks remeshed by edits beyond the chunks whose padded region holds the changed voxel
    /// (light and sky changes), since start.
    pub light_fanout: usize,
}

impl WorldStats {
    /// Nothing queued or running and no viewer change pending: everything wanted is generated
    /// and meshed.
    pub fn is_settled(&self) -> bool {
        self.generating == 0
            && self.meshing == 0
            && self.queued_jobs == 0
            && self.in_flight == 0
            && self.interest_backlog == 0
    }
}

// The slot counters of `WorldStats` one slot counts toward, as bits (index into `Tally`).
/// `data_ready`.
const DATA_READY: u8 = 1;
/// `generating`.
const GENERATING: u8 = 1 << 1;
/// `mesh_ready`.
const MESH_READY: u8 = 1 << 2;
/// `mesh_empty`.
const MESH_EMPTY: u8 = 1 << 3;
/// `meshing`.
const MESHING: u8 = 1 << 4;

impl ChunkSlot {
    /// A new slot for `key` in sky column `column`: data `Queued`, no mesh, no interest.
    pub(crate) fn new(key: ChunkKey, column: ColumnId) -> Self {
        Self {
            pos: key.pos,
            dim: key.dim,
            lod: key.lod,
            data: DataState::Queued,
            mesh: MeshState::None,
            face_links: UNKNOWN_LINKS,
            interest: Interest::empty(),
            neighbors_ready: 0,
            epoch: 0,
            bits: 0,
            waits_sky: false,
            batch: None,
            cancel: Arc::new(AtomicBool::new(false)),
            column,
            light: None,
        }
    }
    /// The chunk's key, as events, jobs and stores see it.
    pub fn key(&self) -> ChunkKey {
        ChunkKey {
            dim: self.dim,
            lod: self.lod,
            pos: self.pos,
        }
    }
    /// Data state.
    pub fn data(&self) -> &DataState {
        &self.data
    }
    /// Mesh state.
    pub fn mesh(&self) -> &MeshState {
        &self.mesh
    }
    /// A remesh was requested while a job was in flight.
    pub fn remesh_pending(&self) -> bool {
        self.bits & REMESH_PENDING != 0
    }
    fn set_bit(&mut self, bit: u8, on: bool) {
        if on {
            self.bits |= bit;
        } else {
            self.bits &= !bit;
        }
    }
    /// An edit changed a voxel that stops direct sky here: unloading the chunk loses the edit
    /// (no persistence yet), so its column is walked again.
    pub(crate) fn sky_edited(&self) -> bool {
        self.bits & SKY_EDITED != 0
    }
    /// Sets [`ChunkSlot::sky_edited`].
    pub(crate) fn set_sky_edited(&mut self) {
        self.bits |= SKY_EDITED;
    }
    /// A light job for this slot is queued or running (at most one).
    pub(crate) fn light_job(&self) -> bool {
        self.bits & LIGHT_JOB != 0
    }
    /// Sets [`ChunkSlot::light_job`].
    pub(crate) fn set_light_job(&mut self, on: bool) {
        self.set_bit(LIGHT_JOB, on);
    }
    /// Which faces of the chunk see each other through its open voxels
    /// ([`ChunkFaceInfo::links`]), for its current data (`None` while unknown: before
    /// generation, or after an edit until the next mesh job).
    pub fn face_links(&self) -> Option<u16> {
        (self.face_links != UNKNOWN_LINKS).then_some(self.face_links)
    }
    /// True if the renderer shows this chunk as its blocks are now (cave culling, ADR 0022):
    /// the mesh it was sent last was built from the chunk's current blocks and the current
    /// borders of its 6 face neighbors, or the chunk has nothing to show (a settled `Empty`
    /// mesh). Only what occludes decides which faces of the occluding cubes a mesh has, so a
    /// remesh for light alone keeps the chunk shown. It is not shown before its first mesh is
    /// published (also without mesh interest), after an edit that changes what occludes in it
    /// or on its border (until its new mesh is published), when its mesh is dropped or lost by
    /// a renderer resync, or once a face neighbor unloads (its mesh assumed that neighbor's
    /// blocks). The renderer may still refuse a mesh: `ChunkVisibility::set_unshown` covers that.
    pub fn shown(&self) -> bool {
        self.bits & SHOWN != 0
    }
    /// The face links the visibility search uses for this chunk ([`ChunkFaceInfo::links`]):
    /// its own while it is [`shown`](ChunkSlot::shown) (the renderer draws the blocks the links
    /// were computed from). A chunk with blocks that is not shown gets every link and
    /// [`XRAY`]: where the faces of its occluding cubes are missing (no mesh, or one built from
    /// other blocks) a line of sight goes on into the rock of its neighbors, whose faces
    /// toward it are not meshed, so the search sees every chunk past it. A chunk without blocks
    /// yet (or permanently air, `Missing`) counts as open: every link. Its neighbors' meshes
    /// cannot have been built from blocks it no longer has, since a neighbor that unloads makes
    /// them not shown.
    pub fn visibility_links(&self) -> u16 {
        if self.shown() {
            self.face_links().unwrap_or(ALL_LINKS)
        } else if matches!(self.data, DataState::Ready(_)) {
            ALL_LINKS | XRAY
        } else {
            ALL_LINKS
        }
    }
    /// The chunk's light (32^3), as of its last mesh or light job. `None` outside
    /// `WorldSettings::light_cache_radius`, before the chunk's first mesh or light job, or with
    /// light off. It may lag an edit by a few frames (until the chunk's remesh or light job
    /// lands).
    pub fn light(&self) -> Option<&ChunkLight> {
        self.light.as_deref().map(|c| &c.light)
    }
    /// The cached light if it is the light of the current content: computed from the current
    /// data and epoch, with no remesh requested since.
    pub(crate) fn current_light(&self) -> Option<&ChunkLight> {
        let cached = self.light.as_ref()?;
        let DataState::Ready(data) = &self.data else {
            return None;
        };
        (cached.data_version == data.version
            && cached.epoch == self.epoch
            && !self.remesh_pending()
            && self.batch.is_none())
        .then_some(&cached.light)
    }
    /// Which slot counters of [`WorldStats`] this slot counts toward (bits of [`Tally`]).
    fn tally_bits(&self) -> u8 {
        let data = match self.data {
            DataState::Ready(_) => DATA_READY,
            DataState::Queued | DataState::Generating => GENERATING,
            DataState::Missing => 0,
        };
        let mesh = match self.mesh {
            MeshState::Ready { .. } => MESH_READY,
            MeshState::Empty => MESH_EMPTY,
            MeshState::None | MeshState::Queued | MeshState::InFlight { .. } => 0,
        };
        // One count per slot (`remesh_pending` is only set while a job is in flight).
        let meshing = if self.remesh_pending()
            || matches!(self.mesh, MeshState::Queued | MeshState::InFlight { .. })
        {
            MESHING
        } else {
            0
        };
        data | mesh | meshing
    }
}

/// The slot counters of [`WorldStats`], kept up to date on every slot change (see
/// [`ChunkMap::update`]), so reading them never walks the slots.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Tally {
    /// By bit index of [`ChunkSlot::tally_bits`].
    counts: [usize; 5],
}

impl Tally {
    /// A slot that counted toward `before` now counts toward `after`.
    fn change(&mut self, before: u8, after: u8) {
        if before == after {
            return;
        }
        for (i, count) in self.counts.iter_mut().enumerate() {
            let bit = 1 << i;
            if before & bit != 0 {
                *count -= 1;
            }
            if after & bit != 0 {
                *count += 1;
            }
        }
    }
}

/// All loaded chunks. Owned by the main thread (ECS resource); workers only see `Arc` snapshots.
/// The slot counters of [`ChunkMap::stats`] are kept up to date on every change (no walk).
#[derive(Resource, Default)]
pub struct ChunkMap {
    // Slots are added and removed with `insert` and `remove`, and their `data`, `mesh` and
    // `remesh_pending` (private to this module) change only through `update` and the setters
    // built on it, which keep `tally` up to date (`ready_data_mut` only edits the data of a
    // `Ready` slot, which keeps its counts). The other fields may be written in place.
    pub(crate) slots: slotmap::SlotMap<ChunkId, ChunkSlot>,
    pub(crate) index: HashMap<ChunkPos, ChunkId, foldhash::fast::FixedState>,
    tally: Tally,
    /// Slots whose data became readable or stopped being readable, in order, until the world
    /// plugin publishes them (`ChunkDataEvent`).
    data_changes: Vec<DataChange>,
    /// Positions whose [`ChunkSlot::visibility_links`] changed (a removed slot counts as
    /// [`ALL_LINKS`]), with the new links, in order, until the visibility system takes them.
    face_changes: Vec<(ChunkPos, u16)>,
}

impl ChunkMap {
    /// Slot by id.
    pub fn get(&self, id: ChunkId) -> Option<&ChunkSlot> {
        self.slots.get(id)
    }
    /// Id at a position.
    pub fn id_at(&self, pos: ChunkPos) -> Option<ChunkId> {
        self.index.get(&pos).copied()
    }
    /// [`ChunkSlot::visibility_links`] of the chunk at `pos` ([`ALL_LINKS`] if not loaded).
    pub fn visibility_links_at(&self, pos: ChunkPos) -> u16 {
        self.id_at(pos)
            .and_then(|id| self.slots.get(id))
            .map_or(ALL_LINKS, ChunkSlot::visibility_links)
    }
    /// Ready data at a position.
    pub fn data_at(&self, pos: ChunkPos) -> Option<&Arc<ChunkData>> {
        match &self.slots.get(self.id_at(pos)?)?.data {
            DataState::Ready(d) => Some(d),
            _ => None,
        }
    }
    /// Block at a world position (None if the chunk is not Ready; air if Missing).
    pub fn block_at(&self, p: BlockPos) -> Option<BlockState> {
        let (chunk, local) = p.split();
        match &self.slots.get(self.id_at(chunk)?)?.data {
            DataState::Ready(d) => Some(d.get(local)),
            DataState::Missing => Some(BlockState::AIR),
            DataState::Queued | DataState::Generating => None,
        }
    }
    /// Light at a world position, as of the chunk's last mesh or light job (see
    /// [`ChunkSlot::light`]): `None` if the chunk is not loaded or has no cached light.
    pub fn light_at(&self, p: BlockPos) -> Option<LightValue> {
        let (chunk, local) = p.split();
        let light = self.slots.get(self.id_at(chunk)?)?.light()?;
        Some(light.get(local))
    }
    /// Number of slots.
    pub fn len(&self) -> usize {
        self.slots.len()
    }
    /// No slots.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
    /// All slots.
    pub fn iter(&self) -> impl Iterator<Item = (ChunkId, &ChunkSlot)> {
        self.slots.iter()
    }
    /// Every non-empty Ready mesh (renderer resync; the renderer then writes
    /// [`ChunkMeshResync`](crate::ChunkMeshResync), since this misses the chunks being
    /// remeshed).
    pub fn ready_meshes(&self) -> impl Iterator<Item = (ChunkId, ChunkKey, &Arc<ChunkMesh>)> {
        self.slots.iter().filter_map(|(id, s)| match &s.mesh {
            MeshState::Ready { mesh, .. } => Some((id, s.key(), mesh)),
            _ => None,
        })
    }
    /// Slot-based counters (`queued_jobs`, `in_flight` and `interest_backlog` are 0 here; the
    /// `WorldStats` resource has them). Kept up to date on every slot change, so this is O(1).
    pub fn stats(&self) -> WorldStats {
        let [data_ready, generating, mesh_ready, mesh_empty, meshing] = self.tally.counts;
        WorldStats {
            chunks: self.slots.len(),
            data_ready,
            generating,
            mesh_ready,
            mesh_empty,
            meshing,
            ..WorldStats::default()
        }
    }
    /// [`ChunkMap::stats`] counted from scratch by walking every slot: the cross-check of the
    /// incremental counters (tests, debug builds).
    pub(crate) fn recount(&self) -> WorldStats {
        let mut stats = WorldStats {
            chunks: self.slots.len(),
            ..WorldStats::default()
        };
        for slot in self.slots.values() {
            match slot.data {
                DataState::Ready(_) => stats.data_ready += 1,
                DataState::Queued | DataState::Generating => stats.generating += 1,
                DataState::Missing => {}
            }
            match slot.mesh {
                MeshState::Ready { .. } => stats.mesh_ready += 1,
                MeshState::Empty => stats.mesh_empty += 1,
                MeshState::None | MeshState::Queued | MeshState::InFlight { .. } => {}
            }
            if slot.remesh_pending()
                || matches!(slot.mesh, MeshState::Queued | MeshState::InFlight { .. })
            {
                stats.meshing += 1;
            }
        }
        stats
    }
    /// Room for `additional` more slots without growing the slots or the index.
    pub(crate) fn reserve(&mut self, additional: usize) {
        self.slots.reserve(additional);
        self.index.reserve(additional);
    }
    /// Adds a slot (and its index entry). The position must be free.
    pub(crate) fn insert(&mut self, slot: ChunkSlot) -> ChunkId {
        let (pos, bits) = (slot.pos, slot.tally_bits());
        let id = self.slots.insert(slot);
        let previous = self.index.insert(pos, id);
        debug_assert!(previous.is_none(), "two slots at {pos:?}");
        self.tally.change(0, bits);
        id
    }
    /// Removes a slot (and its index entry).
    pub(crate) fn remove(&mut self, id: ChunkId) -> Option<ChunkSlot> {
        let slot = self.slots.remove(id)?;
        self.index.remove(&slot.pos);
        self.tally.change(slot.tally_bits(), 0);
        if slot.visibility_links() != ALL_LINKS {
            self.face_changes.push((slot.pos, ALL_LINKS));
        }
        if slot.data.is_readable() {
            self.data_changes.push(DataChange::Unloaded(id, slot.key()));
        }
        Some(slot)
    }
    /// Runs `f` on a slot and recounts it. Every change of `data`, `mesh` or `remesh_pending`
    /// goes through here. Panics if `id` is not loaded.
    fn update<R>(&mut self, id: ChunkId, f: impl FnOnce(&mut ChunkSlot) -> R) -> R {
        let slot = &mut self.slots[id];
        let (before, links) = (slot.tally_bits(), slot.visibility_links());
        let out = f(slot);
        self.tally.change(before, slot.tally_bits());
        let now = slot.visibility_links();
        if now != links {
            self.face_changes.push((slot.pos, now));
        }
        out
    }
    /// Sets a slot's data state (and records it when its blocks become readable or stop being
    /// readable).
    pub(crate) fn set_data(&mut self, id: ChunkId, data: DataState) {
        let change = self.update(id, |slot| {
            let (before, after) = (slot.data.is_readable(), data.is_readable());
            slot.data = data;
            match (before, after) {
                (false, true) => Some(DataChange::Loaded(id, slot.key())),
                (true, false) => Some(DataChange::Unloaded(id, slot.key())),
                _ => None,
            }
        });
        if let Some(change) = change {
            self.data_changes.push(change);
        }
    }
    /// The recorded data changes, oldest first (cleared).
    pub(crate) fn take_data_changes(&mut self) -> Vec<DataChange> {
        std::mem::take(&mut self.data_changes)
    }
    /// Data changes are waiting to be published.
    pub(crate) fn has_data_changes(&self) -> bool {
        !self.data_changes.is_empty()
    }
    /// Sets a slot's face links ([`ChunkSlot::face_links`]).
    pub(crate) fn set_faces(&mut self, id: ChunkId, faces: Option<ChunkFaceInfo>) {
        self.update(id, |slot| {
            slot.face_links = faces.map_or(UNKNOWN_LINKS, |f| f.links);
        });
    }
    /// An edit changed what occludes in a slot: its face links are unknown and the renderer no
    /// longer shows its blocks ([`ChunkSlot::shown`]) until its next mesh is published.
    pub(crate) fn forget_faces(&mut self, id: ChunkId) {
        self.update(id, |slot| {
            slot.face_links = UNKNOWN_LINKS;
            slot.set_bit(SHOWN, false);
        });
    }
    /// Sets whether the renderer shows a slot as its blocks are ([`ChunkSlot::shown`]).
    pub(crate) fn set_shown(&mut self, id: ChunkId, shown: bool) {
        self.update(id, |slot| slot.set_bit(SHOWN, shown));
    }
    /// The recorded changes of [`ChunkSlot::visibility_links`], oldest first (cleared).
    pub(crate) fn take_face_changes(&mut self) -> Vec<(ChunkPos, u16)> {
        std::mem::take(&mut self.face_changes)
    }
    /// Sets a slot's mesh state.
    pub(crate) fn set_mesh(&mut self, id: ChunkId, mesh: MeshState) {
        self.update(id, |slot| slot.mesh = mesh);
    }
    /// Sets a slot's mesh state and remesh request.
    pub(crate) fn set_mesh_and_pending(&mut self, id: ChunkId, mesh: MeshState, pending: bool) {
        self.update(id, |slot| {
            slot.mesh = mesh;
            slot.set_bit(REMESH_PENDING, pending);
        });
    }
    /// Sets a slot's mesh state, clears its remesh request and sets whether the renderer shows
    /// its blocks ([`ChunkSlot::shown`]), in one change.
    pub(crate) fn set_mesh_shown(&mut self, id: ChunkId, mesh: MeshState, shown: bool) {
        self.update(id, |slot| {
            slot.mesh = mesh;
            slot.set_bit(REMESH_PENDING, false);
            slot.set_bit(SHOWN, shown);
        });
    }
    /// Sets a slot's cached light.
    pub(crate) fn set_light(&mut self, id: ChunkId, light: Option<Arc<CachedLight>>) {
        self.slots[id].light = light;
    }
    /// Sets a slot's remesh request.
    pub(crate) fn set_remesh_pending(&mut self, id: ChunkId, pending: bool) {
        self.update(id, |slot| slot.set_bit(REMESH_PENDING, pending));
    }
    /// The data of a `Ready` slot, to edit in place (the slot stays `Ready`, so its counts do
    /// not change). `None` if the slot is not loaded or not `Ready`.
    pub(crate) fn ready_data_mut(&mut self, id: ChunkId) -> Option<&mut Arc<ChunkData>> {
        match &mut self.slots.get_mut(id)?.data {
            DataState::Ready(data) => Some(data),
            DataState::Queued | DataState::Generating | DataState::Missing => None,
        }
    }
}
