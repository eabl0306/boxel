use crate::jobs::{JobExecutor, TaskPoolExecutor};
use crate::streaming::{Ctx, ViewerChunks, WorldJobs};
use crate::{ChunkId, ChunkMap, WorldStats};
use boxel_app::{App, Last, Plugin, Update};
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use boxel_mesh::{ChunkMesh, MaterialTable};
use boxel_tasks::{AsyncComputeTaskPool, TaskPool};
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkKey, ChunkPos, DimensionId};
use boxel_worldgen::{TerrainGenerator, WorldGenerator};
use std::sync::Arc;
use std::time::Duration;

/// Streams chunks around this entity (needs `Transform`). `radius` is the horizontal mesh
/// radius in chunks (circle in xz); data is loaded one chunk further (plus every column next to a
/// mesh column, so each mesh has its 26 neighbors); vertically the generator's
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
    /// `TaskPoolExecutor`.
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
        let content = WorldContent {
            generator: Arc::clone(&self.generator),
            registry: Arc::clone(&self.registry),
            materials: Arc::new(MaterialTable::from_registry(&self.registry)),
        };
        let max_in_flight = resolve_max_jobs_in_flight(self.settings.max_jobs_in_flight);
        app.insert_resource(self.settings.clone())
            .insert_resource(content)
            .init_resource::<ChunkMap>()
            .init_resource::<EditQueue>()
            .init_resource::<WorldStats>()
            .init_resource::<ViewerChunks>()
            .insert_resource(WorldJobs::new(max_in_flight, Arc::clone(&self.executor)))
            .add_message::<ChunkMeshEvent>()
            .configure_sets(
                Update,
                (
                    WorldSystems::Interest,
                    WorldSystems::Edits,
                    WorldSystems::Dispatch,
                    WorldSystems::Collect,
                )
                    .chain(),
            )
            .add_systems(
                Update,
                (
                    // Reads viewer transforms only (into a private resource). Its order relative
                    // to systems that move viewers does not matter (interest lags at most one
                    // frame), and naming them would tie this crate to camera/gameplay crates.
                    (track_viewers.ambiguous_with_all(), update_interest)
                        .chain()
                        .in_set(WorldSystems::Interest),
                    apply_edits.in_set(WorldSystems::Edits),
                    dispatch_jobs.in_set(WorldSystems::Dispatch),
                    collect_results.in_set(WorldSystems::Collect),
                ),
            )
            .add_systems(Last, publish_stats);
    }
}

/// `0` means `2 x` the async compute pool's threads (initializing a default pool if
/// `TaskPoolPlugin` did not run first).
fn resolve_max_jobs_in_flight(requested: usize) -> usize {
    if requested > 0 {
        return requested;
    }
    let threads = AsyncComputeTaskPool::get_or_init(TaskPool::default).thread_num();
    2 * threads.max(1)
}

/// Viewer chunk + radius of every `ChunkViewer`, sorted (only written when it changes).
fn track_viewers(viewers: Query<(&Transform, &ChunkViewer)>, mut chunks: ResMut<ViewerChunks>) {
    let mut now: Vec<_> = viewers
        .iter()
        .map(|(transform, viewer)| {
            let block = BlockPos::from_world(transform.translation);
            (ChunkPos::containing(block), viewer.radius)
        })
        .collect();
    now.sort_unstable();
    if chunks.0 != now {
        chunks.0 = now;
    }
}

fn update_interest(
    mut map: ResMut<ChunkMap>,
    mut jobs: ResMut<WorldJobs>,
    content: Res<WorldContent>,
    settings: Res<WorldSettings>,
    viewers: Res<ViewerChunks>,
    mut writer: MessageWriter<ChunkMeshEvent>,
) {
    // Early-outs keep idle frames from touching (and change-marking) the map.
    if !jobs.viewers_changed(&viewers.0) {
        return;
    }
    let mut events = Vec::new();
    let mut cx = Ctx::new(&mut map, &content, &mut events);
    jobs.update_interest(&mut cx, &settings, &viewers.0);
    writer.write_batch(events);
}

fn apply_edits(
    mut queue: ResMut<EditQueue>,
    mut map: ResMut<ChunkMap>,
    mut jobs: ResMut<WorldJobs>,
    content: Res<WorldContent>,
    mut writer: MessageWriter<ChunkMeshEvent>,
) {
    if queue.is_empty() {
        return;
    }
    let edits = std::mem::take(&mut queue.edits);
    let mut events = Vec::new();
    let mut cx = Ctx::new(&mut map, &content, &mut events);
    jobs.apply_edits(&mut cx, edits);
    writer.write_batch(events);
}

fn dispatch_jobs(
    mut map: ResMut<ChunkMap>,
    mut jobs: ResMut<WorldJobs>,
    content: Res<WorldContent>,
    mut writer: MessageWriter<ChunkMeshEvent>,
) {
    if jobs.scheduler.queued() == 0 {
        return;
    }
    let mut events = Vec::new();
    let mut cx = Ctx::new(&mut map, &content, &mut events);
    jobs.dispatch(&mut cx);
    writer.write_batch(events);
}

fn collect_results(
    mut map: ResMut<ChunkMap>,
    mut jobs: ResMut<WorldJobs>,
    content: Res<WorldContent>,
    settings: Res<WorldSettings>,
    mut writer: MessageWriter<ChunkMeshEvent>,
) {
    if !jobs.has_results() {
        return;
    }
    let mut events = Vec::new();
    let mut cx = Ctx::new(&mut map, &content, &mut events);
    jobs.collect(&mut cx, &settings);
    writer.write_batch(events);
}

/// Refreshes `WorldStats` (slot counts + scheduler counts) and the `"world"` board line.
fn publish_stats(
    map: Res<ChunkMap>,
    jobs: Res<WorldJobs>,
    mut stats: ResMut<WorldStats>,
    board: Option<ResMut<DiagnosticsBoard>>,
) {
    let mut now = map.stats();
    now.queued_jobs = jobs.scheduler.queued();
    now.in_flight = jobs.scheduler.in_flight();
    stats.set_if_neq(now);
    if let Some(mut board) = board {
        board.set(
            "world",
            format!(
                "chunks={} ready={} meshed={} empty={} meshing={} queued={} in_flight={} failed={}",
                now.chunks,
                now.data_ready,
                now.mesh_ready,
                now.mesh_empty,
                now.meshing,
                now.queued_jobs,
                now.in_flight,
                jobs.failed
            ),
        );
    }
}
