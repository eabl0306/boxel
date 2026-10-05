//! Plugin-level tests (`App` + test generator) and shared test support. Mesh jobs use a fake,
//! deterministic mesher (`support::fake_mesh`) so these tests check the streaming rules alone;
//! the real mesher and `TerrainGenerator` are covered by `tests/integration.rs`.
use crate::jobs::{InlineExecutor, JobExecutor, TaskPoolExecutor};
use crate::streaming::{ALL_NEIGHBORS, WorldJobs};
use crate::{
    ChunkMap, ChunkMeshEvent, ChunkViewer, DataState, EditQueue, Interest, MeshState, WorldPlugin,
    WorldSettings, WorldStats,
};
use boxel_app::{App, Last, Update};
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use boxel_ecs::schedule::{LogLevel, ScheduleBuildSettings};
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, ChunkPos};
use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use support::{CountingExecutor, LayerGen, fake_mesh};

/// Fixtures shared with the module-level unit tests.
pub(crate) mod support {
    use crate::jobs::JobExecutor;
    use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, PackedQuad};
    use boxel_voxel::{BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos};
    use boxel_worldgen::WorldGenerator;
    use std::ops::RangeInclusive;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Uniform stone below chunk y = 0, uniform air from y = 0 up (both mesh fast paths), with an
    /// optional key that panics.
    pub(crate) struct LayerGen {
        pub(crate) vertical: RangeInclusive<i32>,
        pub(crate) panic_at: Option<ChunkPos>,
    }

    impl LayerGen {
        pub(crate) fn new(vertical: RangeInclusive<i32>) -> Self {
            Self {
                vertical,
                panic_at: None,
            }
        }
    }

    impl WorldGenerator for LayerGen {
        fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
            assert!(
                self.panic_at != Some(key.pos),
                "test generator panics at {:?}",
                key.pos
            );
            if key.pos.y < 0 {
                ChunkData::uniform(registry.by_name("boxel:stone").expect("stone"))
            } else {
                ChunkData::uniform(BlockState::AIR)
            }
        }
        fn vertical_chunks(&self) -> RangeInclusive<i32> {
            self.vertical.clone()
        }
        fn name(&self) -> &str {
            "layers"
        }
    }

    /// Deterministic stand-in for `boxel_mesh::Mesher`: empty for an
    /// all-air center, else `1 + center.version` quads (an edit produces a different mesh).
    pub(crate) fn fake_mesh(input: &MeshInput<'_>, _materials: &MaterialTable) -> ChunkMesh {
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        input.center.to_dense(&mut dense);
        if dense.iter().all(|s| s.is_air()) {
            return ChunkMesh::default();
        }
        let n = 1 + input.center.version;
        let mut mesh = ChunkMesh {
            quads: vec![PackedQuad::default(); n as usize],
            ..ChunkMesh::default()
        };
        mesh.bucket_counts[0] = n;
        mesh
    }

    /// A mesher that always panics.
    pub(crate) fn panicking_mesh(_input: &MeshInput<'_>, _materials: &MaterialTable) -> ChunkMesh {
        panic!("test mesher panic")
    }

    /// Runs jobs inline and counts them.
    #[derive(Default)]
    pub(crate) struct CountingExecutor {
        pub(crate) spawned: AtomicUsize,
    }

    impl JobExecutor for CountingExecutor {
        fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
            self.spawned.fetch_add(1, Ordering::Relaxed);
            job();
        }
    }

    /// Holds jobs until the test runs or drops them.
    #[derive(Default)]
    pub(crate) struct HoldExecutor {
        jobs: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
    }

    impl HoldExecutor {
        pub(crate) fn held(&self) -> usize {
            self.jobs.lock().expect("lock").len()
        }
        pub(crate) fn run_all(&self) {
            let jobs = std::mem::take(&mut *self.jobs.lock().expect("lock"));
            for job in jobs {
                job();
            }
        }
        pub(crate) fn drop_all(&self) {
            drop(std::mem::take(&mut *self.jobs.lock().expect("lock")));
        }
    }

    impl JobExecutor for HoldExecutor {
        fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
            self.jobs.lock().expect("lock").push(job);
        }
    }
}

fn settings(max_jobs_in_flight: usize) -> WorldSettings {
    WorldSettings {
        max_jobs_in_flight,
        ..WorldSettings::default()
    }
}

/// App with the world plugin, the fake mesher and a diagnostics board.
fn world_app(generator: LayerGen, settings: WorldSettings, executor: Arc<dyn JobExecutor>) -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(generator),
        registry: Arc::new(BlockRegistry::default_blocks()),
        settings,
        executor,
    });
    app.world_mut().resource_mut::<WorldJobs>().mesh_fn = fake_mesh;
    app.world_mut().init_resource::<DiagnosticsBoard>();
    app
}

fn chunk_center(chunk: ChunkPos) -> DVec3 {
    let o = chunk.origin();
    let half = f64::from(CHUNK_SIZE) / 2.0;
    DVec3::new(
        f64::from(o.x) + half,
        f64::from(o.y) + half,
        f64::from(o.z) + half,
    )
}

fn spawn_viewer(app: &mut App, chunk: ChunkPos, radius: u32) -> Entity {
    app.world_mut()
        .spawn((
            Transform::from_translation(chunk_center(chunk)),
            ChunkViewer { radius },
        ))
        .id()
}

fn move_viewer(app: &mut App, viewer: Entity, chunk: ChunkPos) {
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .expect("viewer")
        .translation = chunk_center(chunk);
}

fn drain_events(app: &mut App) -> Vec<ChunkMeshEvent> {
    app.world_mut()
        .resource_mut::<Messages<ChunkMeshEvent>>()
        .drain()
        .collect()
}

/// Updates until `WorldStats` reports a settled, non-empty world; returns every mesh event.
fn settle(app: &mut App, max_frames: usize) -> Vec<ChunkMeshEvent> {
    let mut events = Vec::new();
    for _ in 0..max_frames {
        app.update();
        events.extend(drain_events(app));
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            return events;
        }
    }
    panic!(
        "world not settled after {max_frames} frames: {:?}",
        app.world().resource::<WorldStats>()
    );
}

/// Chunks of a radius-1 viewer in column (cx, cz): the 5x5 square minus its corners.
fn radius1_chunks(cx: i32, cz: i32, vertical: RangeInclusive<i32>) -> BTreeSet<ChunkPos> {
    let mut out = BTreeSet::new();
    for dz in -2..=2_i32 {
        for dx in -2..=2_i32 {
            if dx.abs() == 2 && dz.abs() == 2 {
                continue;
            }
            for y in vertical.clone() {
                out.insert(ChunkPos::new(cx + dx, y, cz + dz));
            }
        }
    }
    out
}

fn loaded(app: &App) -> BTreeSet<ChunkPos> {
    app.world()
        .resource::<ChunkMap>()
        .iter()
        .map(|(_, s)| s.key.pos)
        .collect()
}

fn slot_at(app: &App, pos: ChunkPos) -> &crate::ChunkSlot {
    let map = app.world().resource::<ChunkMap>();
    map.get(map.id_at(pos).expect("loaded")).expect("slot")
}

fn upserts(events: &[ChunkMeshEvent]) -> Vec<ChunkPos> {
    let mut out: Vec<ChunkPos> = events
        .iter()
        .filter_map(|e| match e {
            ChunkMeshEvent::Upsert { key, mesh, .. } => {
                assert!(!mesh.is_empty(), "Upsert meshes are never empty");
                Some(key.pos)
            }
            ChunkMeshEvent::Remove { .. } => None,
        })
        .collect();
    out.sort_unstable();
    out
}

fn removes(events: &[ChunkMeshEvent]) -> Vec<ChunkPos> {
    events
        .iter()
        .filter_map(|e| match e {
            ChunkMeshEvent::Remove { key, .. } => Some(key.pos),
            ChunkMeshEvent::Upsert { .. } => None,
        })
        .collect()
}

/// Radius-1 mesh columns around (0, 0).
const MESH_COLUMNS: [(i32, i32); 5] = [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)];

/// With vertical -3..=1: mesh chunks at y = -1 (air above) and y = -3 (nothing below) are
/// exposed stone and need a mesh job; y = -2 is enclosed and y >= 0 is air (fast paths).
fn exposed_chunks() -> Vec<ChunkPos> {
    let mut out: Vec<ChunkPos> = MESH_COLUMNS
        .iter()
        .flat_map(|&(x, z)| [ChunkPos::new(x, -1, z), ChunkPos::new(x, -3, z)])
        .collect();
    out.sort_unstable();
    out
}

#[test]
fn plugin_streams_settles_and_uses_fast_paths() {
    let executor = Arc::new(CountingExecutor::default());
    let mut app = world_app(LayerGen::new(-3..=1), settings(64), executor.clone());
    spawn_viewer(&mut app, ChunkPos::new(0, 9, 0), 1);
    let events = settle(&mut app, 200);

    let expected = radius1_chunks(0, 0, -3..=1);
    assert_eq!(loaded(&app), expected, "21 columns x 5 layers");
    let exposed = exposed_chunks();
    let map = app.world().resource::<ChunkMap>();
    for (_, slot) in map.iter() {
        let pos = slot.key.pos;
        assert!(matches!(slot.data, DataState::Ready(_)), "{pos:?} data");
        let mesh_column = MESH_COLUMNS.contains(&(pos.x, pos.z));
        assert!(slot.interest.contains(Interest::DATA));
        assert_eq!(
            slot.interest.contains(Interest::MESH),
            mesh_column,
            "{pos:?}"
        );
        if mesh_column {
            assert_eq!(slot.neighbors_ready, ALL_NEIGHBORS, "{pos:?} neighbors");
        }
        match (&slot.mesh, mesh_column, exposed.contains(&pos)) {
            (MeshState::Ready { mesh, .. }, true, true) => assert_eq!(mesh.total_quads(), 1),
            (MeshState::Empty, true, false) | (MeshState::None, false, false) => {}
            (state, ..) => panic!("{pos:?}: unexpected mesh state {state:?}"),
        }
    }
    // Fast paths: air chunks (y >= 0) and enclosed stone (y = -2) never spawn a mesh job.
    assert_eq!(
        executor.spawned.load(Ordering::Relaxed),
        expected.len() + exposed.len(),
        "one job per generation plus one per exposed mesh"
    );
    assert_eq!(upserts(&events), exposed);
    assert!(removes(&events).is_empty());
    assert_eq!(map.ready_meshes().count(), exposed.len());

    assert_eq!(map.block_at(BlockPos::new(0, -1, 0)), Some(BlockState(1)));
    assert_eq!(map.block_at(BlockPos::new(5, 0, -7)), Some(BlockState::AIR));
    assert_eq!(map.block_at(BlockPos::new(5, 1000, -7)), None, "not loaded");

    assert_eq!(
        *app.world().resource::<WorldStats>(),
        WorldStats {
            chunks: 105,
            data_ready: 105,
            generating: 0,
            mesh_ready: 10,
            mesh_empty: 15,
            meshing: 0,
            queued_jobs: 0,
            in_flight: 0,
        }
    );
    assert_eq!(
        app.world().resource::<DiagnosticsBoard>().get("world"),
        Some("chunks=105 ready=105 meshed=10 empty=15 meshing=0 queued=0 in_flight=0 failed=0")
    );
}

#[test]
fn moving_far_unloads_everything_and_streams_the_new_area() {
    let mut app = world_app(
        LayerGen::new(-3..=1),
        settings(64),
        Arc::new(InlineExecutor),
    );
    let viewer = spawn_viewer(&mut app, ChunkPos::new(0, 0, 0), 1);
    let ids = |events: &[ChunkMeshEvent], upsert: bool| -> BTreeSet<u64> {
        events
            .iter()
            .filter_map(|e| match (e, upsert) {
                (ChunkMeshEvent::Upsert { id, .. }, true)
                | (ChunkMeshEvent::Remove { id, .. }, false) => Some(id.to_bits()),
                _ => None,
            })
            .collect()
    };
    let first = ids(&settle(&mut app, 200), true);
    assert_eq!(first.len(), 10);

    move_viewer(&mut app, viewer, ChunkPos::new(40, 0, -3));
    let events = settle(&mut app, 200);
    assert_eq!(
        ids(&events, false),
        first,
        "every published mesh is removed, by id"
    );
    assert_eq!(upserts(&events).len(), 10);
    assert!(
        ids(&events, true).is_disjoint(&first),
        "new slots get new ids"
    );
    assert_eq!(loaded(&app), radius1_chunks(40, -3, -3..=1));
}

#[test]
fn small_moves_keep_chunks_inside_the_hysteresis_band() {
    let mut app = world_app(
        LayerGen::new(-1..=0),
        settings(64),
        Arc::new(InlineExecutor),
    );
    let viewer = spawn_viewer(&mut app, ChunkPos::new(0, 0, 0), 1);
    settle(&mut app, 200);
    move_viewer(&mut app, viewer, ChunkPos::new(1, 0, 0));
    let events = settle(&mut app, 200);
    assert!(
        removes(&events).is_empty(),
        "nothing unloads within hysteresis"
    );
    let mut both = radius1_chunks(0, 0, -1..=0);
    both.extend(radius1_chunks(1, 0, -1..=0));
    assert_eq!(loaded(&app), both);
    // Left the data set: kept (hysteresis) with no interest.
    let gone = slot_at(&app, ChunkPos::new(-2, -1, 0));
    assert!(gone.interest.is_empty());
    assert!(matches!(gone.mesh, MeshState::None));
    // Left the mesh radius: keeps its published mesh until unload.
    let kept = slot_at(&app, ChunkPos::new(-1, -1, 0));
    assert_eq!(kept.interest, Interest::DATA);
    assert!(matches!(kept.mesh, MeshState::Ready { .. }));
    // New mesh columns (2, 0), (1, 1) and (1, -1); the y = -1 layer is exposed stone.
    assert_eq!(
        upserts(&events),
        vec![
            ChunkPos::new(1, -1, -1),
            ChunkPos::new(1, -1, 1),
            ChunkPos::new(2, -1, 0),
        ]
    );
}

#[test]
fn edits_remesh_the_chunk_and_touched_face_neighbors() {
    let mut app = world_app(
        LayerGen::new(-3..=1),
        settings(64),
        Arc::new(InlineExecutor),
    );
    spawn_viewer(&mut app, ChunkPos::new(0, 0, 0), 1);
    settle(&mut app, 200);
    let (center, east, above) = (
        ChunkPos::new(0, -1, 0),
        ChunkPos::new(1, -1, 0),
        ChunkPos::new(0, 0, 0),
    );
    let (east_epoch, above_epoch) = (slot_at(&app, east).epoch, slot_at(&app, above).epoch);

    // Local (31, 31, 5) of chunk (0, -1, 0) touches its +X and +Y faces.
    let edited = BlockPos::new(31, -1, 5);
    {
        let mut queue = app.world_mut().resource_mut::<EditQueue>();
        queue.set_block(edited, BlockState::AIR);
        queue.set_block(BlockPos::new(3, -40, 3), BlockState(1)); // already stone: no-op
        queue.set_block(BlockPos::new(0, 5000, 0), BlockState(1)); // not loaded: dropped
        assert_eq!(queue.len(), 3);
    }
    let events = settle(&mut app, 50);
    assert!(app.world().resource::<EditQueue>().is_empty());

    assert_eq!(
        upserts(&events),
        vec![center, east],
        "center + east neighbor"
    );
    assert!(removes(&events).is_empty());
    assert_eq!(slot_at(&app, east).epoch, east_epoch + 1);
    assert_eq!(
        slot_at(&app, above).epoch,
        above_epoch + 1,
        "air neighbor stays Empty"
    );
    assert!(matches!(slot_at(&app, above).mesh, MeshState::Empty));
    let map = app.world().resource::<ChunkMap>();
    assert_eq!(map.block_at(edited), Some(BlockState::AIR));
    match &slot_at(&app, center).mesh {
        MeshState::Ready {
            mesh, data_version, ..
        } => {
            assert_eq!(*data_version, 1);
            assert_eq!(mesh.total_quads(), 2, "fake mesh of version 1");
        }
        other => panic!("center mesh {other:?}"),
    }
    let untouched = map.data_at(ChunkPos::new(0, -2, 0)).expect("ready");
    assert_eq!(
        untouched.version, 0,
        "a no-op edit does not bump the version"
    );
}

#[test]
fn panicking_generator_marks_the_chunk_missing_and_settles() {
    let mut generator = LayerGen::new(-3..=1);
    generator.panic_at = Some(ChunkPos::new(0, -2, 0));
    let mut app = world_app(generator, settings(64), Arc::new(InlineExecutor));
    spawn_viewer(&mut app, ChunkPos::new(0, 0, 0), 1);
    settle(&mut app, 200);
    let failed = slot_at(&app, ChunkPos::new(0, -2, 0));
    assert!(matches!(failed.data, DataState::Missing));
    assert!(matches!(failed.mesh, MeshState::Empty));
    let map = app.world().resource::<ChunkMap>();
    assert_eq!(
        map.block_at(BlockPos::new(0, -40, 0)),
        Some(BlockState::AIR)
    );
    // The missing chunk counts as a ready (air) neighbor: the exposed chunks still mesh, and the
    // four y = -2 mesh chunks next to it are no longer enclosed.
    assert_eq!(map.ready_meshes().count(), 10 + 4);
    let stats = *app.world().resource::<WorldStats>();
    assert_eq!((stats.in_flight, stats.queued_jobs), (0, 0));
    let line = app
        .world()
        .resource::<DiagnosticsBoard>()
        .get("world")
        .expect("line");
    assert!(line.ends_with("failed=1"), "{line}");
}

#[test]
fn task_pool_executor_settles_the_same_world() {
    // max_jobs_in_flight = 0: 2 x async compute threads.
    let mut app = world_app(
        LayerGen::new(-3..=1),
        settings(0),
        Arc::new(TaskPoolExecutor),
    );
    let max = app
        .world()
        .resource::<WorldJobs>()
        .scheduler
        .max_in_flight();
    assert!(max >= 2 && max.is_multiple_of(2), "auto cap {max}");
    spawn_viewer(&mut app, ChunkPos::new(0, 0, 0), 1);
    let start = std::time::Instant::now();
    let mut events = Vec::new();
    loop {
        app.update();
        events.extend(drain_events(&mut app));
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            break;
        }
        assert!(
            start.elapsed() < std::time::Duration::from_secs(60),
            "not settled in 60 s: {stats:?}"
        );
        std::thread::yield_now();
    }
    assert_eq!(loaded(&app), radius1_chunks(0, 0, -3..=1));
    assert_eq!(
        app.world().resource::<ChunkMap>().ready_meshes().count(),
        10
    );
    assert_eq!(upserts(&events), exposed_chunks());
}

#[test]
fn no_viewer_means_no_chunks() {
    let mut app = world_app(
        LayerGen::new(-3..=1),
        settings(64),
        Arc::new(InlineExecutor),
    );
    for _ in 0..3 {
        app.update();
    }
    assert!(app.world().resource::<ChunkMap>().is_empty());
    assert!(app.world().resource::<WorldStats>().is_settled());
}

/// A system that moves viewers (like `FlyCameraPlugin`) is not ambiguous with the world.
#[test]
fn world_systems_have_no_ambiguities() {
    fn move_viewers(mut q: Query<&mut Transform, With<ChunkViewer>>) {
        for mut t in &mut q {
            t.translation.x += 0.5;
        }
    }
    fn read_stats(_stats: Res<WorldStats>) {}
    fn strict(schedule: &mut Schedule) {
        schedule.set_build_settings(ScheduleBuildSettings {
            ambiguity_detection: LogLevel::Error,
            ..ScheduleBuildSettings::default()
        });
    }

    let mut app = world_app(LayerGen::new(-1..=0), settings(8), Arc::new(InlineExecutor));
    app.add_systems(Update, move_viewers);
    app.edit_schedule(Update, strict)
        .edit_schedule(Last, strict);
    spawn_viewer(&mut app, ChunkPos::new(0, 0, 0), 1);
    app.update();
    app.update();
    assert!(!app.world().resource::<ChunkMap>().is_empty());

    // Control: detection is active (an unordered reader of `WorldStats` in `Last` is reported).
    app.add_systems(Last, read_stats);
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.update()));
    assert!(caught.is_err(), "ambiguity detection is active");
}
