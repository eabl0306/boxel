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
    /// Outside the generator's vertical range: permanently air, never generated.
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
}

impl WorldStats {
    /// Nothing queued or running: everything wanted is generated and meshed.
    pub fn is_settled(&self) -> bool {
        self.generating == 0 && self.meshing == 0 && self.queued_jobs == 0 && self.in_flight == 0
    }
}

/// All loaded chunks. Owned by the main thread (ECS resource); workers only see `Arc` snapshots.
#[derive(Resource, Default)]
pub struct ChunkMap {
    pub(crate) slots: slotmap::SlotMap<ChunkId, ChunkSlot>,
    pub(crate) index: HashMap<ChunkPos, ChunkId, foldhash::fast::FixedState>,
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
        todo!("WP6")
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
    /// Slot-based counters (`queued_jobs` and `in_flight` are 0 here; the `WorldStats` resource
    /// has them).
    pub fn stats(&self) -> WorldStats {
        todo!("WP6")
    }
}
