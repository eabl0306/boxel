use crate::interest::Viewer;
use crate::jobs::{JobExecutor, TaskPoolExecutor};
use crate::sky::{SkyMap, SkyRule};
use crate::streaming::{Ctx, ViewerChunks, WorldJobs};
use crate::{ChunkId, ChunkMap, WorldStats};
use boxel_app::{App, Last, Plugin, Update};
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use boxel_light::sky_tile::DEFAULT_WALK_LIMIT;
use boxel_mesh::{AoPolicy, ChunkMesh, MaterialTable};
use boxel_tasks::{AsyncComputeTaskPool, TaskPool};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, ChunkKey, ChunkPos, DimensionId, LightTables,
};
use boxel_worldgen::{TerrainGenerator, WorldGenerator};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Streams chunks around this entity (needs `Transform`). The loaded volume is a vertical
/// cylinder around the viewer's chunk:
/// - meshes for the columns within `radius` (a circle in xz) and the layers within
///   `vertical_radius` above and below;
/// - data one chunk further in every direction (each mesh has its 26 neighbors);
/// - chunks the viewer left unload `WorldSettings::unload_hysteresis` chunks further still.
///
/// The world has no height limit unless `WorldSettings::vertical_bounds` sets one. Mesh chunks per
/// viewer: about `pi x radius^2 x (2 x vertical_radius + 1)`; with data, about
/// `pi x (radius + 1)^2 x (2 x vertical_radius + 3)`.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkViewer {
    /// Horizontal mesh radius in chunks.
    pub radius: u32,
    /// Vertical mesh radius in chunks (layers above and below the viewer's chunk).
    pub vertical_radius: u32,
}

impl ChunkViewer {
    /// Vertical radius of [`ChunkViewer::new`]: 8 chunks (256 voxels) up and down.
    pub const DEFAULT_VERTICAL_RADIUS: u32 = 8;

    /// `radius` chunks around, [`ChunkViewer::DEFAULT_VERTICAL_RADIUS`] up and down.
    pub const fn new(radius: u32) -> Self {
        Self {
            radius,
            vertical_radius: Self::DEFAULT_VERTICAL_RADIUS,
        }
    }

    /// The same viewer with another vertical radius.
    pub const fn with_vertical_radius(self, vertical_radius: u32) -> Self {
        Self {
            vertical_radius,
            ..self
        }
    }
}

impl Default for ChunkViewer {
    /// Radius 12, vertical radius 8.
    fn default() -> Self {
        Self::new(12)
    }
}

/// Streaming knobs. Read every frame: changes apply on the next frame.
#[derive(Resource, Clone, Debug)]
pub struct WorldSettings {
    /// Dimension streamed (M0: 0).
    pub dimension: DimensionId,
    /// Max jobs in flight. `0` (the default) sizes it every frame: enough jobs to keep every
    /// async compute worker busy for 1.5 frames, from the measured job cost and frame interval
    /// (at least 2 per worker, at most 4096).
    pub max_jobs_in_flight: usize,
    /// Main-thread time per frame for applying viewer changes: creating, updating and unloading
    /// chunk slots, and refreshing queued job priorities. A big change (a jump, a new viewer, a
    /// larger radius) is spread over several frames, nearest chunks first. At least 64 positions
    /// are processed per frame whatever the budget.
    pub interest_budget: Duration,
    /// Main-thread time per frame for collecting finished jobs and starting new ones (repeated
    /// while results keep arriving).
    pub job_budget: Duration,
    /// Max `ChunkMeshEvent`s published per frame (upload smoothing).
    pub max_mesh_events_per_frame: usize,
    /// Extra chunks beyond the data region (horizontally and vertically) before unloading.
    pub unload_hysteresis: u32,
    /// Fixed world height: only these chunk layers are ever loaded, and neighbors outside them
    /// count as air. `None` (the default): no height limit. With
    /// `Some(generator.vertical_chunks())` and a large `ChunkViewer::vertical_radius`, the world
    /// streams like boxel M0 did (every layer of the generator, whatever the viewer's height).
    pub vertical_bounds: Option<RangeInclusive<i64>>,
    /// Which faces the mesher bakes ambient occlusion into (ADR 0015). The default,
    /// `BakedLitOnly`, skips faces whose 4 corners are dark (most cave faces). A change applies
    /// to the meshes built after it. With `light` off every face counts as lit, so every face
    /// gets AO unless this is `AoPolicy::Off`.
    pub ao_policy: AoPolicy,
    /// Voxel light (ADR 0015): sky light from per-column sky tops plus RGB block light, baked
    /// into each mesh by its job. Off: meshes see open sky everywhere and no sky tops are
    /// computed; the look before light also needs `ao_policy: AoPolicy::Off` (it had no AO).
    /// Read when the world starts (a later change does nothing).
    pub light: bool,
    /// How deep, in voxels under the generator's top, a column is walked looking for its sky
    /// top. A column still open there (a natural shaft deeper than this) gets its top at that
    /// floor: no direct sky below it. A box under the floor needs no sky tile at all, so deep
    /// viewers walk nothing. Read when the world starts.
    pub sky_walk_limit: u32,
    /// Chunks (distance to the nearest viewer) within which the world keeps each meshed
    /// chunk's light for gameplay queries ([`ChunkMap::light_at`](crate::ChunkMap::light_at))
    /// and for sizing edit remeshes (16 to 64 KiB per lit chunk, nothing for uniform light). A
    /// mesh job keeps it; a chunk without one (all air, enclosed) or that came within the
    /// radius later gets a light job, after the view's mesh jobs. Chunks that leave the radius
    /// drop their light. At most 32 (larger values are clamped).
    pub light_cache_radius: u32,
    /// Frames the finished meshes of one edit batch wait for the rest of the batch before they
    /// are published anyway (0: no wait): a mesh that arrives in frame `N` is published by frame
    /// `N + edit_batch_hold_frames`. Holding them keeps a frame from showing half of an edit's
    /// light change.
    pub edit_batch_hold_frames: u32,
}

impl Default for WorldSettings {
    fn default() -> Self {
        Self {
            dimension: DimensionId(0),
            max_jobs_in_flight: 0,
            interest_budget: Duration::from_millis(1),
            job_budget: Duration::from_millis(2),
            max_mesh_events_per_frame: 512,
            unload_hysteresis: 2,
            vertical_bounds: None,
            ao_policy: AoPolicy::default(),
            light: true,
            sky_walk_limit: DEFAULT_WALK_LIMIT,
            light_cache_radius: 8,
            edit_batch_hold_frames: 2,
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
    /// Light view of `registry` (opacity and emission per state).
    pub light_tables: Arc<LightTables>,
}

impl WorldContent {
    /// The content of a world with `generator` and `registry` (the tables derived from it).
    pub fn new(generator: Arc<dyn WorldGenerator>, registry: Arc<BlockRegistry>) -> Self {
        Self {
            generator,
            materials: Arc::new(MaterialTable::from_registry(&registry)),
            light_tables: Arc::new(LightTables::from_registry(&registry)),
            registry,
        }
    }
}

/// Mesh changes for the renderer, written in `WorldSystems::Collect` (and on unload).
#[derive(Message, Clone, Debug)]
pub enum ChunkMeshEvent {
    /// New or replaced mesh (never empty).
    Upsert {
        /// Chunk id.
        id: ChunkId,
        /// Key. The mesh's world origin is `key.pos.origin()`, an `i64` voxel position: the
        /// renderer keeps it in `i64` on the CPU and uploads its low 32 bits
        /// (`boxel_math::wrap_to_i32`).
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
    /// Viewer interest: load/unload and job priorities, within `WorldSettings::interest_budget`.
    Interest,
    /// Apply `EditQueue`.
    Edits,
    /// Collect finished jobs and start new ones, within `WorldSettings::job_budget`.
    Dispatch,
    /// Publish mesh results as `ChunkMeshEvent`s (up to `max_mesh_events_per_frame`).
    Collect,
}

/// Adds the world: resources (`ChunkMap`, `SkyMap`, `WorldSettings`, `WorldContent`,
/// `EditQueue`, `WorldStats`, internal job state), the `ChunkMeshEvent` message and the chained systems in
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
        let content = WorldContent::new(Arc::clone(&self.generator), Arc::clone(&self.registry));
        let rule = SkyRule::new(
            self.settings.light,
            self.generator.vertical_chunks(),
            self.settings.sky_walk_limit,
        );
        if rule == SkyRule::Open {
            tracing::warn!(
                generator = self.generator.name(),
                "the generator does not declare its top (WorldGenerator::vertical_chunks): \
                 light runs without sky tops, every column is open sky"
            );
        }
        let workers = async_compute_threads();
        let mut jobs = WorldJobs::new(1, workers, Arc::clone(&self.executor));
        jobs.begin_frame(&self.settings, Instant::now());
        app.insert_resource(self.settings.clone())
            .insert_resource(content)
            .init_resource::<ChunkMap>()
            .insert_resource(SkyMap::new(rule))
            .init_resource::<EditQueue>()
            .init_resource::<WorldStats>()
            .init_resource::<ViewerChunks>()
            .insert_resource(jobs)
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
                    run_jobs.in_set(WorldSystems::Dispatch),
                    publish_meshes.in_set(WorldSystems::Collect),
                ),
            )
            .add_systems(Last, publish_stats);
    }
}

/// Threads of the async compute pool (initializing a default pool if `TaskPoolPlugin` did not
/// run first).
fn async_compute_threads() -> usize {
    AsyncComputeTaskPool::get_or_init(TaskPool::default)
        .thread_num()
        .max(1)
}

/// Every `ChunkViewer` with its chunk, sorted by entity (only written when it changes). The chunk
/// is exact at any `f64` position inside `i64`; past the world limit (`MAX_CHUNK_COORD`) the
/// viewer is clamped to it.
fn track_viewers(
    viewers: Query<(Entity, &Transform, &ChunkViewer)>,
    mut chunks: ResMut<ViewerChunks>,
) {
    let mut now: Vec<(u64, Viewer)> = viewers
        .iter()
        .map(|(entity, transform, viewer)| {
            let block = BlockPos::from_world(transform.translation);
            let center = ChunkPos::containing(block);
            (
                entity.to_bits(),
                Viewer::new(center, viewer.radius, viewer.vertical_radius),
            )
        })
        .collect();
    now.sort_unstable_by_key(|(id, _)| *id);
    if chunks.0 != now {
        chunks.0 = now;
    }
}

fn update_interest(
    mut map: ResMut<ChunkMap>,
    mut sky: ResMut<SkyMap>,
    mut jobs: ResMut<WorldJobs>,
    content: Res<WorldContent>,
    settings: Res<WorldSettings>,
    viewers: Res<ViewerChunks>,
    mut writer: MessageWriter<ChunkMeshEvent>,
) {
    // Early-outs keep idle frames from touching (and change-marking) the map.
    if !jobs.interest_pending(&viewers.0, &settings) {
        return;
    }
    let mut events = Vec::new();
    let mut cx = Ctx::new(&mut map, &mut sky, &content, &mut events);
    jobs.update_interest(&mut cx, &settings, &viewers.0);
    writer.write_batch(events);
}

fn apply_edits(
    mut queue: ResMut<EditQueue>,
    mut map: ResMut<ChunkMap>,
    mut sky: ResMut<SkyMap>,
    mut jobs: ResMut<WorldJobs>,
    content: Res<WorldContent>,
    mut writer: MessageWriter<ChunkMeshEvent>,
) {
    if queue.is_empty() {
        return;
    }
    let edits = std::mem::take(&mut queue.edits);
    let mut events = Vec::new();
    let mut cx = Ctx::new(&mut map, &mut sky, &content, &mut events);
    jobs.apply_edits(&mut cx, edits);
    writer.write_batch(events);
}

fn run_jobs(
    mut map: ResMut<ChunkMap>,
    mut sky: ResMut<SkyMap>,
    mut jobs: ResMut<WorldJobs>,
    content: Res<WorldContent>,
    settings: Res<WorldSettings>,
    mut writer: MessageWriter<ChunkMeshEvent>,
) {
    jobs.begin_frame(&settings, Instant::now());
    if !jobs.has_job_work() {
        return;
    }
    let mut events = Vec::new();
    let mut cx = Ctx::new(&mut map, &mut sky, &content, &mut events);
    jobs.run_jobs(&mut cx, &settings);
    writer.write_batch(events);
}

fn publish_meshes(
    mut map: ResMut<ChunkMap>,
    mut sky: ResMut<SkyMap>,
    mut jobs: ResMut<WorldJobs>,
    content: Res<WorldContent>,
    settings: Res<WorldSettings>,
    mut writer: MessageWriter<ChunkMeshEvent>,
) {
    if !jobs.has_meshes() {
        return;
    }
    let mut events = Vec::new();
    let mut cx = Ctx::new(&mut map, &mut sky, &content, &mut events);
    jobs.apply_meshes(&mut cx, &settings);
    writer.write_batch(events);
}

/// Refreshes `WorldStats` (slot counts, scheduler counts, interest backlog) and the `"world"`
/// board line. The slot counts are kept by the map (no walk); debug builds check them against a
/// walk of every slot in the frames that changed the map.
fn publish_stats(
    map: Res<ChunkMap>,
    sky: Res<SkyMap>,
    jobs: Res<WorldJobs>,
    mut stats: ResMut<WorldStats>,
    mut failed: Local<Option<usize>>,
    board: Option<ResMut<DiagnosticsBoard>>,
) {
    let mut now = map.stats();
    debug_assert!(
        !map.is_changed() || now == map.recount(),
        "incremental slot counts {now:?} differ from a recount {:?}",
        map.recount()
    );
    now.queued_jobs = jobs.scheduler.queued();
    now.in_flight = jobs.scheduler.in_flight();
    now.interest_backlog = jobs.interest_backlog();
    jobs.light_stats(&mut now);
    now.sky_columns = sky.len();
    let changed = *stats != now || *failed != Some(jobs.failed);
    stats.set_if_neq(now);
    *failed = Some(jobs.failed);
    if let Some(mut board) = board
        && (changed || board.get("world").is_none())
    {
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
        if sky.rule != SkyRule::Off {
            board.set(
                "light",
                format!(
                    "full={} fast={} columns={} walks={} private={} waiting_sky={} \
                     fanout={} batches_held={} split={}",
                    now.light_full,
                    now.light_fast,
                    now.sky_columns,
                    now.sky_walks,
                    now.sky_private_generations,
                    now.waiting_sky,
                    now.light_fanout,
                    now.edit_batches_held,
                    now.edit_batches_split
                ),
            );
        }
    }
}
