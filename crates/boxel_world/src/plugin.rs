use crate::ChunkId;
use crate::jobs::{JobExecutor, TaskPoolExecutor};
use boxel_app::{App, Plugin};
use boxel_ecs::prelude::*;
use boxel_mesh::{ChunkMesh, MaterialTable};
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkKey, DimensionId};
use boxel_worldgen::{TerrainGenerator, WorldGenerator};
use std::sync::Arc;
use std::time::Duration;

/// Streams chunks around this entity (needs `Transform`). `radius` is the horizontal mesh
/// radius in chunks (circle in xz); data is loaded one chunk further; vertically the generator's
/// `vertical_chunks()` range is used.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkViewer {
    /// Mesh radius in chunks.
    pub radius: u32,
}

impl Default for ChunkViewer {
    fn default() -> Self {
        Self { radius: 12 }
    }
}

/// Streaming knobs.
#[derive(Resource, Clone, Debug)]
pub struct WorldSettings {
    /// Dimension streamed (M0: 0).
    pub dimension: DimensionId,
    /// Max jobs in flight (0 = 2 x async compute threads, from
    /// `AsyncComputeTaskPool::get_or_init(TaskPool::default).thread_num()`).
    pub max_jobs_in_flight: usize,
    /// Time budget for draining job results per frame.
    pub collect_budget: Duration,
    /// Max `ChunkMeshEvent`s published per frame (upload smoothing).
    pub max_mesh_events_per_frame: usize,
    /// Extra chunks beyond the data radius before unloading.
    pub unload_hysteresis: u32,
}

impl Default for WorldSettings {
    fn default() -> Self {
        Self {
            dimension: DimensionId(0),
            max_jobs_in_flight: 0,
            collect_budget: Duration::from_millis(4),
            max_mesh_events_per_frame: 512,
            unload_hysteresis: 2,
        }
    }
}

/// Generator, registry and the derived material table (inserted by [`WorldPlugin`]).
#[derive(Resource, Clone)]
pub struct WorldContent {
    /// Generator.
    pub generator: Arc<dyn WorldGenerator>,
    /// Blocks.
    pub registry: Arc<BlockRegistry>,
    /// Mesher view of `registry`.
    pub materials: Arc<MaterialTable>,
}

/// Mesh changes for the renderer, written in `WorldSystems::Collect` (and on unload).
#[derive(Message, Clone, Debug)]
pub enum ChunkMeshEvent {
    /// New or replaced mesh (never empty).
    Upsert {
        /// Chunk id.
        id: ChunkId,
        /// Key (origin = `key.pos.origin()`).
        key: ChunkKey,
        /// Mesh.
        mesh: Arc<ChunkMesh>,
    },
    /// Mesh gone (unloaded, or remeshed to empty).
    Remove {
        /// Chunk id.
        id: ChunkId,
        /// Key.
        key: ChunkKey,
    },
}

/// One block change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEdit {
    /// Position.
    pub pos: BlockPos,
    /// New state.
    pub state: BlockState,
}

/// Pending edits, applied in `WorldSystems::Edits` (edits to non-Ready chunks are dropped with a
/// debug log in M0).
#[derive(Resource, Debug, Default)]
pub struct EditQueue {
    pub(crate) edits: Vec<BlockEdit>,
}

impl EditQueue {
    /// Queues a block change.
    pub fn set_block(&mut self, pos: BlockPos, state: BlockState) {
        self.edits.push(BlockEdit { pos, state });
    }
    /// Queued edits.
    pub fn len(&self) -> usize {
        self.edits.len()
    }
    /// Nothing queued.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }
}

/// World system sets, chained in `Update`.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorldSystems {
    /// Viewer interest, load/unload, priorities.
    Interest,
    /// Apply `EditQueue`.
    Edits,
    /// Start jobs.
    Dispatch,
    /// Drain results, publish `ChunkMeshEvent`s.
    Collect,
}

/// Adds the world: resources (`ChunkMap`, `WorldSettings`, `WorldContent`, `EditQueue`,
/// `WorldStats`, internal job state), the `ChunkMeshEvent` message and the chained systems in
/// `Update`. A stats system in `Last` refreshes `WorldStats` and publishes a `"world"` line to
/// `DiagnosticsBoard` if present. `TaskPoolPlugin` is recommended (pool sizes); without it the
/// default executor initializes a default async compute pool.
pub struct WorldPlugin {
    /// Generator.
    pub generator: Arc<dyn WorldGenerator>,
    /// Blocks.
    pub registry: Arc<BlockRegistry>,
    /// Knobs.
    pub settings: WorldSettings,
    /// Where jobs run (tests use `jobs::InlineExecutor`).
    pub executor: Arc<dyn JobExecutor>,
}

impl Default for WorldPlugin {
    /// `TerrainGenerator::default()`, `BlockRegistry::default_blocks()`, default settings,
    /// `TaskPoolExecutor`. (Calls WP4 code: do not use in tests before integration.)
    fn default() -> Self {
        Self {
            generator: Arc::new(TerrainGenerator::default()),
            registry: Arc::new(BlockRegistry::default_blocks()),
            settings: WorldSettings::default(),
            executor: Arc::new(TaskPoolExecutor),
        }
    }
}

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App) {
        todo!("WP6")
    }
}
