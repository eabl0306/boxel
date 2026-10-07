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
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, ChunkPos, MAX_CHUNK_COORD};
use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use support::{CountingExecutor, LayerGen, fake_mesh};

mod light;
mod neighbors;

/// Fixtures shared with the module-level unit tests.
pub(crate) mod support {
    use crate::jobs::JobExecutor;
    use boxel_mesh::{ChunkMesh, MaterialTable, MeshInput, MeshOptions, PackedQuad};
    use boxel_voxel::{BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos};
    use boxel_worldgen::WorldGenerator;
    use std::ops::RangeInclusive;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Uniform stone from the bottom of `vertical` up to chunk y = `surface - 1` (-1 by default),
    /// uniform air elsewhere (both mesh fast paths), with an optional key that panics. Records
    /// every position it generates.
    pub(crate) struct LayerGen {
        pub(crate) vertical: RangeInclusive<i64>,
        pub(crate) surface: i64,
        pub(crate) panic_at: Option<ChunkPos>,
        pub(crate) generated: Arc<Mutex<Vec<ChunkPos>>>,
    }

    impl LayerGen {
        pub(crate) fn new(vertical: RangeInclusive<i64>) -> Self {
            Self {
                vertical,
                surface: 0,
                panic_at: None,
                generated: Arc::default(),
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
            self.generated.lock().expect("lock").push(key.pos);
            if key.pos.y < self.surface && self.vertical.contains(&key.pos.y) {
                ChunkData::uniform(registry.by_name("boxel:stone").expect("stone"))
            } else {
                ChunkData::uniform(BlockState::AIR)
            }
        }
        fn vertical_chunks(&self) -> RangeInclusive<i64> {
            self.vertical.clone()
        }
        fn name(&self) -> &str {
            "layers"
        }
    }

    /// Deterministic stand-in for `boxel_mesh::Mesher`: empty for an
    /// all-air center, else `1 + center.version` quads (an edit produces a different mesh).
    pub(crate) fn fake_mesh(
        input: &MeshInput<'_>,
        _materials: &MaterialTable,
        _options: MeshOptions,
    ) -> ChunkMesh {
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        input.center.to_dense(&mut dense);
        if dense.iter().all(|s| s.is_air()) {
            return ChunkMesh::default();
        }
        let n = 1 + input.center.version;
        let mut counts = [0; 8];
        counts[0] = n;
        ChunkMesh::from_quads(vec![PackedQuad::default(); n as usize], counts)
    }

    /// A mesher that always panics.
    pub(crate) fn panicking_mesh(
        _input: &MeshInput<'_>,
        _materials: &MaterialTable,
        _options: MeshOptions,
    ) -> ChunkMesh {
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
            self.run_first(usize::MAX);
        }
        /// Runs the first `n` held jobs, in the order they were spawned.
        pub(crate) fn run_first(&self, n: usize) {
            let jobs: Vec<_> = {
                let mut held = self.jobs.lock().expect("lock");
                let n = n.min(held.len());
                held.drain(..n).collect()
            };
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

/// App with the world plugin, the fake mesher and a diagnostics board. The world has the
/// generator's fixed height (boxel M0 streaming; viewers from [`spawn_viewer`] want every layer).
fn world_app(generator: LayerGen, settings: WorldSettings, executor: Arc<dyn JobExecutor>) -> App {
    let settings = WorldSettings {
        vertical_bounds: Some(generator.vertical.clone()),
        ..settings
    };
    tall_world_app(generator, settings, executor)
}

/// App with the world plugin, the fake mesher and a diagnostics board, with `settings` as given.
fn tall_world_app(
    generator: LayerGen,
    settings: WorldSettings,
    executor: Arc<dyn JobExecutor>,
) -> App {
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
    // Exact in f64 for every chunk of these tests (origins below 2^53).
    o.as_i64vec3().as_dvec3() + DVec3::splat(f64::from(CHUNK_SIZE) / 2.0)
}

/// A viewer that wants every layer (of a fixed-height world).
fn spawn_viewer(app: &mut App, chunk: ChunkPos, radius: u32) -> Entity {
    spawn_tall_viewer(app, chunk, radius, u32::MAX)
}

fn spawn_tall_viewer(app: &mut App, chunk: ChunkPos, radius: u32, vertical_radius: u32) -> Entity {
    app.world_mut()
        .spawn((
            Transform::from_translation(chunk_center(chunk)),
            ChunkViewer {
                radius,
                vertical_radius,
            },
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
fn radius1_chunks(cx: i64, cz: i64, vertical: RangeInclusive<i64>) -> BTreeSet<ChunkPos> {
    let mut out = BTreeSet::new();
    for dz in -2..=2_i64 {
        for dx in -2..=2_i64 {
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
        .map(|(_, s)| s.pos)
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
const MESH_COLUMNS: [(i64, i64); 5] = [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)];

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
        let pos = slot.pos;
        assert!(matches!(slot.data(), DataState::Ready(_)), "{pos:?} data");
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
        match (slot.mesh(), mesh_column, exposed.contains(&pos)) {
            (MeshState::Ready { mesh, .. }, true, true) => assert_eq!(mesh.total_quads(), 1),
            (MeshState::Empty, true, false) | (MeshState::None, false, false) => {}
            (state, ..) => panic!("{pos:?}: unexpected mesh state {state:?}"),
        }
    }
    // Fast paths: air chunks (y >= 0) and enclosed stone (y = -2) never spawn a mesh job. The
    // layers reach the void above the walk floor, so every mesh needs the sky tiles of its
    // 3x3 columns: the 21 data columns are walked once each, from loaded data only.
    let stats = *app.world().resource::<WorldStats>();
    assert_eq!(
        (
            stats.sky_walks,
            stats.sky_private_generations,
            stats.sky_columns
        ),
        (21, 0, 21)
    );
    // The viewer is 8 chunks above layer 1: only (0, 1, 0) is within `light_cache_radius`. It
    // is all air (no mesh job), so a light job computes its light.
    assert_eq!(stats.light_jobs, 1);
    assert_eq!(
        map.light_at(BlockPos::new(5, 40, 7)),
        Some(boxel_voxel::LightValue::FULL_SKY)
    );
    assert_eq!(map.light_at(BlockPos::new(40, 40, 7)), None, "too far");
    assert_eq!(
        executor.spawned.load(Ordering::Relaxed),
        expected.len() + exposed.len() + 21 + 1,
        "one job per generation, one per exposed mesh, one per sky walk and one light job"
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
            interest_backlog: 0,
            light_full: stats.light_full,
            light_fast: stats.light_fast,
            light_jobs: 1,
            sky_columns: 21,
            sky_walks: 21,
            sky_private_generations: 0,
            waiting_sky: 0,
            edit_batches_held: 0,
            edit_batches_split: 0,
            light_fanout: 0,
        }
    );
    assert_eq!(stats.light_full + stats.light_fast, exposed.len());
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

/// World coordinates are i64: the world follows a viewer across the former i32 edge of voxels
/// (chunk 2^26 = voxel 2^31, where positions used to saturate or wrap), far past it, and is
/// clamped at the world limit. Every chunk is generated for its own position.
#[test]
fn streaming_follows_the_viewer_far_from_the_origin() {
    let generator = LayerGen::new(-3..=1);
    let generated = Arc::clone(&generator.generated);
    let mut app = world_app(generator, settings(64), Arc::new(InlineExecutor));
    let edge = 1i64 << 26;
    let take = || -> BTreeSet<ChunkPos> {
        std::mem::take(&mut *generated.lock().expect("lock"))
            .into_iter()
            .collect()
    };
    let viewer = spawn_viewer(&mut app, ChunkPos::new(edge - 1, 0, -edge), 1);
    // Jumps far enough to unload the previous area.
    for (cx, cz) in [
        // The loaded area straddles +2^31 in x (chunks 2^26 - 3..=2^26 + 1) and -2^31 in z.
        (edge - 1, -edge),
        // The mirrored edges: a 2^32-voxel jump that used to land on the same chunks.
        (-edge, edge - 1),
        // 2^40 voxels and much further.
        (1 << 35, -(1 << 35)),
        (-(1 << 50) + 3, (1 << 50) - 3),
    ] {
        move_viewer(&mut app, viewer, ChunkPos::new(cx, 0, cz));
        settle(&mut app, 200);
        let expected = radius1_chunks(cx, cz, -3..=1);
        assert_eq!(loaded(&app), expected, "viewer in column ({cx}, {cz})");
        // Every chunk was generated at its true position, nothing else.
        assert_eq!(take(), expected, "generated around ({cx}, {cz})");
        // Block lookups use the true coordinates.
        let map = app.world().resource::<ChunkMap>();
        let block = BlockPos::new(cx * 32 + 31, -1, cz * 32);
        assert_eq!(block.split().0, ChunkPos::new(cx, -1, cz));
        assert_eq!(
            map.block_at(block),
            Some(BlockState(1)),
            "stone at {block:?}"
        );
        assert_eq!(
            map.block_at(BlockPos::new(cx * 32 + 32 * 3, -1, cz * 32)),
            None
        );
    }
    // Past the world limit the viewer is clamped: the world streams the chunks at the limit.
    let beyond = ChunkPos::new(MAX_CHUNK_COORD * 2, 0, -MAX_CHUNK_COORD * 2);
    move_viewer(&mut app, viewer, beyond);
    settle(&mut app, 200);
    assert_eq!(
        loaded(&app),
        radius1_chunks(MAX_CHUNK_COORD, -MAX_CHUNK_COORD, -3..=1)
    );
}

/// The same in y, whose code paths are separate (layers, vertical bounds, known-air layers):
/// bounded worlds whose layers straddle +2^31 and -2^31 voxels in y, then a world without a
/// height limit following a viewer up and down across those edges and to the world limit.
#[test]
fn streaming_follows_the_viewer_far_in_y() {
    let edge = 1i64 << 26;
    let set = |generated: &Arc<std::sync::Mutex<Vec<ChunkPos>>>| -> BTreeSet<ChunkPos> {
        std::mem::take(&mut *generated.lock().expect("lock"))
            .into_iter()
            .collect()
    };
    // Stone below layer `surface`, air from it up: the first air voxel is `surface * 32`.
    let check_surface = |app: &App, x: i64, surface: i64, z: i64| {
        let map = app.world().resource::<ChunkMap>();
        let top = BlockPos::new(x * 32 + 5, surface * 32, z * 32 + 7);
        assert_eq!(map.block_at(top), Some(BlockState::AIR), "air at {top:?}");
        let below = BlockPos::new(top.x, top.y - 1, top.z);
        assert_eq!(
            map.block_at(below),
            Some(BlockState(1)),
            "stone at {below:?}"
        );
    };

    // Fixed height: every layer of the generator (chunks around -2^26 or 2^26, the former i32
    // edge of voxels), wherever the viewer is.
    for surface in [edge - 1, -edge] {
        let layers = surface - 3..=surface + 1;
        let generator = LayerGen {
            surface,
            ..LayerGen::new(layers.clone())
        };
        let generated = Arc::clone(&generator.generated);
        let mut app = world_app(generator, settings(64), Arc::new(InlineExecutor));
        spawn_viewer(&mut app, ChunkPos::new(3, surface + 40, -2), 1);
        settle(&mut app, 200);
        let expected = radius1_chunks(3, -2, layers);
        assert_eq!(loaded(&app), expected, "surface {surface}");
        assert_eq!(set(&generated), expected, "generated, surface {surface}");
        check_surface(&app, 3, surface, -2);
    }

    // No height limit: the viewer's layers (vertical radius 1, data one more) follow it, and
    // layers outside the generator's are known air, created without a job.
    let gen_layers = edge - 4..=edge;
    let generator = LayerGen {
        surface: edge - 1,
        ..LayerGen::new(gen_layers.clone())
    };
    let generated = Arc::clone(&generator.generated);
    let mut app = tall_world_app(generator, settings(64), Arc::new(InlineExecutor));
    let viewer = spawn_tall_viewer(&mut app, ChunkPos::new(0, edge - 1, 0), 1, 1);
    // Far jumps only (each unloads the previous area): across +2^31, to -2^31, back above
    // +2^31, and past the world limit (clamped).
    for (y, center) in [
        (edge - 1, edge - 1),
        (-edge, -edge),
        (edge + 1, edge + 1),
        (MAX_CHUNK_COORD * 2, MAX_CHUNK_COORD),
    ] {
        move_viewer(&mut app, viewer, ChunkPos::new(0, y, 0));
        settle(&mut app, 200);
        let expected = radius1_chunks(0, 0, center - 2..=center + 2);
        assert_eq!(loaded(&app), expected, "viewer in layer {y}");
        let in_generator: BTreeSet<ChunkPos> = expected
            .iter()
            .copied()
            .filter(|p| gen_layers.contains(&p.y))
            .collect();
        assert_eq!(set(&generated), in_generator, "generated around layer {y}");
    }
    // Back at the surface: the true layers again.
    move_viewer(&mut app, viewer, ChunkPos::new(0, edge - 1, 0));
    settle(&mut app, 200);
    check_surface(&app, 0, edge - 1, 0);
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
    assert!(matches!(gone.mesh(), MeshState::None));
    // Left the mesh radius: keeps its published mesh until unload.
    let kept = slot_at(&app, ChunkPos::new(-1, -1, 0));
    assert_eq!(kept.interest, Interest::DATA);
    assert!(matches!(kept.mesh(), MeshState::Ready { .. }));
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
fn edits_remesh_the_chunk_and_the_neighbors_that_read_the_voxel() {
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
    // The +X+Y edge neighbor reads the voxel too (AO and corner light of its faces).
    let edge = ChunkPos::new(1, 0, 0);
    let (east_epoch, above_epoch) = (slot_at(&app, east).epoch, slot_at(&app, above).epoch);
    let edge_epoch = slot_at(&app, edge).epoch;
    let far_epoch = slot_at(&app, ChunkPos::new(-1, -1, 0)).epoch;

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

    // The east neighbor is remeshed too, but its (fake) mesh comes out the same: only the
    // center's new mesh is sent.
    assert_eq!(upserts(&events), vec![center], "center");
    assert!(removes(&events).is_empty());
    assert_eq!(slot_at(&app, east).epoch, east_epoch + 1);
    assert!(matches!(
        slot_at(&app, east).mesh(),
        MeshState::Ready { epoch, .. } if *epoch == east_epoch + 1
    ));
    assert_eq!(
        slot_at(&app, above).epoch,
        above_epoch + 1,
        "air neighbor stays Empty"
    );
    assert!(matches!(slot_at(&app, above).mesh(), MeshState::Empty));
    assert_eq!(slot_at(&app, edge).epoch, edge_epoch + 1, "edge neighbor");
    assert_eq!(
        slot_at(&app, ChunkPos::new(-1, -1, 0)).epoch,
        far_epoch,
        "the -X neighbor does not read the voxel"
    );
    let map = app.world().resource::<ChunkMap>();
    assert_eq!(map.block_at(edited), Some(BlockState::AIR));
    match slot_at(&app, center).mesh() {
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
    assert!(matches!(failed.data(), DataState::Missing));
    assert!(matches!(failed.mesh(), MeshState::Empty));
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
    // max_jobs_in_flight = 0: the automatic cap, at least 2 per async compute thread.
    let mut app = world_app(
        LayerGen::new(-3..=1),
        settings(0),
        Arc::new(TaskPoolExecutor),
    );
    let threads = boxel_tasks::AsyncComputeTaskPool::get_or_init(boxel_tasks::TaskPool::default)
        .thread_num()
        .max(1);
    let max = app
        .world()
        .resource::<WorldJobs>()
        .scheduler
        .max_in_flight();
    assert!(max >= 2 * threads, "auto cap {max}");
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

/// The structures every interest step touches keep their size: a slot is 112 bytes (its key is
/// stored flat, so the small fields fill the padding a nested `ChunkKey` would leave), an index
/// entry 32.
#[test]
fn hot_structures_stay_small() {
    assert_eq!(size_of::<crate::ChunkSlot>(), 112, "chunk slot");
    assert_eq!(size_of::<(ChunkPos, crate::ChunkId)>(), 32, "index entry");
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

/// Without a height limit the world loads a cylinder around the viewer. A big change (the first
/// viewer) is spread over frames when the interest budget is small, and `WorldStats` shows the
/// backlog until it is applied.
#[test]
fn tall_world_spreads_big_changes_over_frames() {
    let settings = WorldSettings {
        interest_budget: std::time::Duration::ZERO,
        max_jobs_in_flight: 64,
        ..WorldSettings::default()
    };
    let mut app = tall_world_app(LayerGen::new(-3..=1), settings, Arc::new(InlineExecutor));
    spawn_tall_viewer(&mut app, ChunkPos::new(0, 0, 0), 4, 2);
    app.update();
    let first = *app.world().resource::<WorldStats>();
    assert!(
        first.interest_backlog > 0,
        "64 positions per frame: {first:?}"
    );
    assert!(!first.is_settled());
    settle(&mut app, 500);
    // Data: columns within radius 5 plus the xz neighbors of the radius-4 mesh columns, layers
    // -3..=3 (vertical radius 2, plus one).
    let mut expected = BTreeSet::new();
    for y in -3..=3 {
        for z in -6..=6_i64 {
            for x in -6..=6_i64 {
                let mesh = |x: i64, z: i64| x * x + z * z <= 16;
                let data =
                    x * x + z * z <= 25 || (-1..=1).any(|a| (-1..=1).any(|b| mesh(x + a, z + b)));
                if data {
                    expected.insert(ChunkPos::new(x, y, z));
                }
            }
        }
    }
    assert_eq!(loaded(&app), expected);
    assert_eq!(app.world().resource::<WorldStats>().interest_backlog, 0);
    // Stone layers -3..=-1 under air: only the top of the stone (layer -1) is exposed; layer -3
    // sits on layer -4, which is not meshed (outside the mesh layers -2..=2).
    let map = app.world().resource::<ChunkMap>();
    let mut exposed: Vec<i64> = map.ready_meshes().map(|(_, k, _)| k.pos.y).collect();
    exposed.sort_unstable();
    exposed.dedup();
    assert_eq!(exposed, vec![-1]);
}

/// The job step collects before it dispatches and loops while results arrive: with jobs that
/// finish at once, even 2 jobs in flight load the whole world in two frames.
#[test]
fn jobs_run_back_to_back_within_a_frame() {
    let executor = Arc::new(CountingExecutor::default());
    let mut app = world_app(LayerGen::new(-3..=1), settings(2), executor.clone());
    spawn_viewer(&mut app, ChunkPos::new(0, 0, 0), 1);
    app.update();
    app.update();
    let stats = *app.world().resource::<WorldStats>();
    assert!(stats.is_settled(), "{stats:?}");
    // The 10 all-air mesh chunks (layers 0 and 1) have no mesh job: a light job each fills
    // their light cache. The enclosed stone needs none (no light enters it).
    assert_eq!(stats.light_jobs, 10);
    assert_eq!(
        executor.spawned.load(Ordering::Relaxed),
        105 + 10 + 21 + 10,
        "every generation, the exposed meshes, a sky walk per column and the air chunks' light"
    );
}

/// Under a frame cap the runner publishes the next deadline and the world's budgets grow into
/// the idle part of the frame (issue #3): a big change takes fewer frames, and the world ends
/// exactly the same.
#[test]
fn a_frame_deadline_grows_the_budgets_without_changing_the_world() {
    use boxel_app::FrameDeadline;
    use boxel_mesh::ChunkMesh;
    use std::time::{Duration, Instant};
    let run = |deadline: bool| {
        let settings = WorldSettings {
            interest_budget: Duration::from_nanos(1),
            job_budget: Duration::from_nanos(1),
            max_jobs_in_flight: 64,
            ..WorldSettings::default()
        };
        let mut app = tall_world_app(LayerGen::new(-3..=1), settings, Arc::new(InlineExecutor));
        spawn_tall_viewer(&mut app, ChunkPos::new(0, 0, 0), 4, 2);
        let mut events = Vec::new();
        let mut frames = 0;
        loop {
            if deadline {
                let period = Duration::from_secs(2);
                app.insert_resource(FrameDeadline {
                    next: Some(Instant::now() + period),
                    period: Some(period),
                    last_update: Duration::from_millis(1),
                });
            }
            app.update();
            frames += 1;
            events.extend(drain_events(&mut app));
            let stats = *app.world().resource::<WorldStats>();
            if stats.chunks > 0 && stats.is_settled() {
                break;
            }
            assert!(frames < 2000, "not settled: {stats:?}");
        }
        let map = app.world().resource::<ChunkMap>();
        let mut meshes: Vec<_> = map
            .ready_meshes()
            .map(|(_, key, mesh)| (key.pos, ChunkMesh::clone(mesh)))
            .collect();
        meshes.sort_unstable_by_key(|(pos, _)| *pos);
        (frames, loaded(&app), upserts(&events), meshes)
    };
    let (fixed_frames, fixed_loaded, fixed_upserts, fixed_meshes) = run(false);
    let (grown_frames, grown_loaded, grown_upserts, grown_meshes) = run(true);
    assert!(
        grown_frames * 4 <= fixed_frames,
        "{grown_frames} frames with the deadline, {fixed_frames} without"
    );
    assert_eq!(grown_loaded, fixed_loaded);
    assert_eq!(grown_upserts, fixed_upserts);
    assert_eq!(grown_meshes, fixed_meshes);
}
