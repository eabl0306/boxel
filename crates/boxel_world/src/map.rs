use boxel_ecs::prelude::*;
use boxel_mesh::ChunkMesh;
use boxel_voxel::{BlockPos, BlockState, ChunkData, ChunkKey, ChunkPos};
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

/// One chunk's bookkeeping.
#[derive(Clone, Debug)]
pub struct ChunkSlot {
    /// Key.
    pub key: ChunkKey,
    /// Data state.
    pub data: DataState,
    /// Mesh state.
    pub mesh: MeshState,
    /// Interest flags.
    pub interest: Interest,
    /// Bit `i` set when neighbor `NEIGHBOR_OFFSETS[i]` is `Ready` or `Missing` (26 bits).
    pub neighbors_ready: u32,
    /// Bumped when a neighbor's border data changes (closes the neighbor-staleness gap).
    pub epoch: u32,
    /// A remesh was requested while a job was in flight.
    pub remesh_pending: bool,
    /// Set on unload; jobs check it before heavy work (cooperative cancellation).
    pub cancel: Arc<AtomicBool>,
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
        let meshing = if self.remesh_pending
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
    // `remesh_pending` change only through `update` (or the setters built on it), which keeps
    // `tally` up to date. The other fields may be written in place.
    pub(crate) slots: slotmap::SlotMap<ChunkId, ChunkSlot>,
    pub(crate) index: HashMap<ChunkPos, ChunkId, foldhash::fast::FixedState>,
    tally: Tally,
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
    /// Every non-empty Ready mesh (renderer resync).
    pub fn ready_meshes(&self) -> impl Iterator<Item = (ChunkId, ChunkKey, &Arc<ChunkMesh>)> {
        self.slots.iter().filter_map(|(id, s)| match &s.mesh {
            MeshState::Ready { mesh, .. } => Some((id, s.key, mesh)),
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
            if slot.remesh_pending
                || matches!(slot.mesh, MeshState::Queued | MeshState::InFlight { .. })
            {
                stats.meshing += 1;
            }
        }
        stats
    }
    /// Adds a slot (and its index entry). The position must be free.
    pub(crate) fn insert(&mut self, slot: ChunkSlot) -> ChunkId {
        let (pos, bits) = (slot.key.pos, slot.tally_bits());
        let id = self.slots.insert(slot);
        let previous = self.index.insert(pos, id);
        debug_assert!(previous.is_none(), "two slots at {pos:?}");
        self.tally.change(0, bits);
        id
    }
    /// Removes a slot (and its index entry).
    pub(crate) fn remove(&mut self, id: ChunkId) -> Option<ChunkSlot> {
        let slot = self.slots.remove(id)?;
        self.index.remove(&slot.key.pos);
        self.tally.change(slot.tally_bits(), 0);
        Some(slot)
    }
    /// Runs `f` on a slot and recounts it. Every change of `data`, `mesh` or `remesh_pending`
    /// goes through here. Panics if `id` is not loaded.
    pub(crate) fn update<R>(&mut self, id: ChunkId, f: impl FnOnce(&mut ChunkSlot) -> R) -> R {
        let slot = &mut self.slots[id];
        let before = slot.tally_bits();
        let out = f(slot);
        self.tally.change(before, slot.tally_bits());
        out
    }
    /// Sets a slot's data state.
    pub(crate) fn set_data(&mut self, id: ChunkId, data: DataState) {
        self.update(id, |slot| slot.data = data);
    }
    /// Sets a slot's mesh state.
    pub(crate) fn set_mesh(&mut self, id: ChunkId, mesh: MeshState) {
        self.update(id, |slot| slot.mesh = mesh);
    }
    /// Sets a slot's mesh state and remesh request.
    pub(crate) fn set_mesh_and_pending(&mut self, id: ChunkId, mesh: MeshState, pending: bool) {
        self.update(id, |slot| {
            slot.mesh = mesh;
            slot.remesh_pending = pending;
        });
    }
    /// Sets a slot's remesh request.
    pub(crate) fn set_remesh_pending(&mut self, id: ChunkId, pending: bool) {
        self.update(id, |slot| slot.remesh_pending = pending);
    }
}
