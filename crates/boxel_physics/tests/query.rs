//! The ECS layer: `ChunkSource` over a real `ChunkMap` (every chunk state, vertical bounds),
//! `PhysicsPlugin` and `PhysicsQuery` in a headless app with `WorldPlugin` and inline jobs.
mod common;

use boxel_app::App;
use boxel_ecs::system::RunSystemOnce;
use boxel_math::DVec3;
use boxel_physics::{
    ChunkSource, PhysicsContent, PhysicsPlugin, PhysicsQuery, Probe, RayResult, TestWorld,
    VoxelFilter, VoxelSource, raycast_voxels,
};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, Face, WorldPos,
};
use boxel_world::jobs::InlineExecutor;
use boxel_world::{
    ChunkMap, ChunkSlot, ChunkViewer, DataState, WorldPlugin, WorldSettings, WorldStats,
};
use boxel_worldgen::{FlatGenerator, ForestGenerator, WorldGenerator};
use common::{STONE, table};
use std::ops::RangeInclusive;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Flat layers: stone y 0..=2, grass y = 3; a chunk listed in `panic_at` fails to generate.
struct TestGen {
    flat: FlatGenerator,
    panic_at: Option<ChunkPos>,
}

impl TestGen {
    fn new(panic_at: Option<ChunkPos>) -> Self {
        Self {
            flat: FlatGenerator::new(vec![
                ("boxel:stone".to_string(), 3),
                ("boxel:grass".to_string(), 1),
            ]),
            panic_at,
        }
    }
}

impl WorldGenerator for TestGen {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        assert_ne!(
            Some(key.pos),
            self.panic_at,
            "test generator: this chunk fails"
        );
        self.flat.generate(key, registry)
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        i64::MIN..=i64::MAX
    }
    fn name(&self) -> &str {
        "test"
    }
}

fn app_with(generator: Arc<dyn WorldGenerator>, settings: WorldSettings) -> App {
    let mut app = App::new();
    app.add_plugins((
        WorldPlugin {
            generator,
            settings,
            executor: Arc::new(InlineExecutor),
            ..WorldPlugin::default()
        },
        PhysicsPlugin,
    ));
    app.finish();
    app.cleanup();
    app
}

fn spawn_viewer(app: &mut App, at: DVec3, radius: u32, vertical: u32) {
    app.world_mut().spawn((
        Transform::from_translation(at),
        ChunkViewer::new(radius).with_vertical_radius(vertical),
    ));
}

fn settle(app: &mut App) {
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "not settled: {stats:?}"
        );
    }
}

fn probe(app: &App, p: BlockPos) -> Probe {
    let map = app.world().resource::<ChunkMap>();
    let settings = app.world().resource::<WorldSettings>();
    ChunkSource::new(map, settings).probe(p)
}

#[test]
fn chunk_source_reads_ready_missing_and_absent_chunks() {
    let mut app = app_with(
        Arc::new(TestGen::new(Some(ChunkPos::new(0, -2, 0)))),
        WorldSettings::default(),
    );
    spawn_viewer(&mut app, DVec3::new(16.0, 8.0, 16.0), 1, 1);
    settle(&mut app);
    let grass = BlockRegistry::default_blocks()
        .by_name("boxel:grass")
        .expect("built-in");
    // Ready, packed (stone and grass under air).
    assert_eq!(probe(&app, BlockPos::new(3, 2, 7)), Probe::Block(STONE));
    assert_eq!(probe(&app, BlockPos::new(3, 3, 7)), Probe::Block(grass));
    assert_eq!(
        probe(&app, BlockPos::new(3, 4, 7)),
        Probe::Block(BlockState::AIR)
    );
    // Ready, uniform air (below the layers).
    assert_eq!(
        probe(&app, BlockPos::new(0, -5, 0)),
        Probe::Block(BlockState::AIR)
    );
    // Missing (its generation panicked): air.
    let map = app.world().resource::<ChunkMap>();
    let failed = map
        .id_at(ChunkPos::new(0, -2, 0))
        .and_then(|id| map.get(id));
    assert!(matches!(
        failed.map(ChunkSlot::data),
        Some(DataState::Missing)
    ));
    assert_eq!(
        probe(&app, BlockPos::new(5, -40, 5)),
        Probe::Block(BlockState::AIR)
    );
    // No slot: unloaded.
    assert_eq!(probe(&app, BlockPos::new(5000, 0, 0)), Probe::Unloaded);
    // One source across many chunks gives the same answers as fresh ones (the cache).
    let map = app.world().resource::<ChunkMap>();
    let settings = app.world().resource::<WorldSettings>();
    let mut shared = ChunkSource::new(map, settings);
    for (x, y, z) in [
        (3, 2, 7),
        (40, 3, -20),
        (3, 3, 7),
        (-31, 2, 0),
        (5000, 0, 0),
        (3, 2, 7),
    ] {
        let p = BlockPos::new(x, y, z);
        assert_eq!(shared.probe(p), probe(&app, p), "{p:?}");
    }
}

#[test]
fn queued_chunks_are_unloaded_and_layers_outside_the_bounds_are_air() {
    // One job in flight at a time and a zero job budget: after one update most chunks are
    // still queued.
    let settings = WorldSettings {
        max_jobs_in_flight: 1,
        job_budget: Duration::ZERO,
        vertical_bounds: Some(-1..=1),
        ..WorldSettings::default()
    };
    let mut app = app_with(Arc::new(TestGen::new(None)), settings);
    spawn_viewer(&mut app, DVec3::new(16.0, 8.0, 16.0), 2, 1);
    app.update();
    let map = app.world().resource::<ChunkMap>();
    let queued = map
        .iter()
        .find(|(_, slot)| matches!(slot.data(), DataState::Queued | DataState::Generating))
        .map(|(_, slot)| slot.pos)
        .expect("a queued chunk after one frame");
    assert_eq!(probe(&app, queued.origin()), Probe::Unloaded);
    // Layers outside -1..=1 never get a slot and read as air, above and below.
    assert!(map.id_at(ChunkPos::new(0, 5, 0)).is_none());
    assert_eq!(
        probe(&app, BlockPos::new(0, 200, 0)),
        Probe::Block(BlockState::AIR)
    );
    assert_eq!(
        probe(&app, BlockPos::new(0, -100, 0)),
        Probe::Block(BlockState::AIR)
    );
}

#[test]
fn physics_query_picks_blocks_from_a_system() {
    let mut app = app_with(Arc::new(TestGen::new(None)), WorldSettings::default());
    spawn_viewer(&mut app, DVec3::new(16.0, 8.0, 16.0), 1, 1);
    settle(&mut app);
    assert!(app.world().contains_resource::<PhysicsContent>());
    let (down, inside, along) = app
        .world_mut()
        .run_system_once(|physics: PhysicsQuery| {
            let pick = |origin: DVec3, dir: DVec3, reach: f64| {
                physics.raycast_voxels(
                    WorldPos::from_dvec3(origin),
                    dir,
                    reach,
                    VoxelFilter::Outline { fluids: false },
                )
            };
            assert!(physics.collision().is_full(STONE));
            assert_eq!(physics.probe(BlockPos::new(0, 0, 0)), Probe::Block(STONE));
            (
                pick(DVec3::new(0.5, 20.5, 0.5), DVec3::NEG_Y, 30.0),
                pick(DVec3::new(0.5, 3.5, 0.5), DVec3::X, 6.0),
                pick(DVec3::new(0.5, 10.5, 0.5), DVec3::X, 5000.0),
            )
        })
        .expect("system runs");
    let hit = down.hit().expect("the grass below");
    assert_eq!(
        (hit.block, hit.face, hit.t),
        (BlockPos::new(0, 3, 0), Some(Face::PosY), 16.5)
    );
    assert_eq!(hit.adjacent(), Some(BlockPos::new(0, 4, 0)));
    // Starting inside the grass layer: hit at t = 0, no face.
    assert_eq!(
        inside.hit().map(|h| (h.block, h.face)),
        Some((BlockPos::new(0, 3, 0), None))
    );
    // Above the ground along +X: loaded air, then the edge of the loaded area.
    assert!(matches!(along, RayResult::Unloaded { .. }), "{along:?}");
}

#[test]
fn chunk_source_agrees_with_the_same_chunks_in_a_test_world() {
    // Generated terrain with trees: uniform air and stone chunks, packed surface chunks. The
    // ChunkSource answers uniform chunks without decoding; the TestWorld always decodes.
    let mut app = app_with(
        Arc::new(ForestGenerator::default()),
        WorldSettings::default(),
    );
    spawn_viewer(&mut app, DVec3::new(0.0, 70.0, 0.0), 1, 1);
    settle(&mut app);
    let map = app.world().resource::<ChunkMap>();
    let settings = app.world().resource::<WorldSettings>();
    let mut copy = TestWorld::new();
    let mut uniform = 0;
    for (_, slot) in map.iter() {
        match slot.data() {
            DataState::Ready(data) => {
                uniform += usize::from(data.as_uniform().is_some());
                copy.set_chunk(slot.pos, (**data).clone());
            }
            DataState::Missing => {}
            DataState::Queued | DataState::Generating => copy.set_unloaded(slot.pos, true),
        }
    }
    assert!(uniform > 0, "some uniform chunks");
    let table = table();
    let mut rng = common::Rng(11);
    for _ in 0..3000 {
        let origin = WorldPos::from_dvec3(DVec3::new(
            rng.unit() * 64.0 - 32.0,
            rng.unit() * 64.0 + 40.0,
            rng.unit() * 64.0 - 32.0,
        ));
        let dir = DVec3::new(rng.unit() - 0.5, rng.unit() - 0.7, rng.unit() - 0.5);
        let ours = raycast_voxels(
            &mut ChunkSource::new(map, settings),
            &table,
            origin,
            dir,
            60.0,
            VoxelFilter::Collision,
        );
        let reference = raycast_voxels(
            &mut &copy,
            &table,
            origin,
            dir,
            60.0,
            VoxelFilter::Collision,
        );
        // Chunks the copy does not have read as air there and as unloaded here: compare only
        // rays that stay inside the loaded area.
        if !matches!(ours, RayResult::Unloaded { .. }) {
            assert_eq!(ours, reference, "{origin:?} {dir:?}");
        }
    }
}

#[test]
#[should_panic(expected = "PhysicsPlugin needs the voxel world")]
fn the_plugin_needs_the_world() {
    let mut app = App::new();
    app.add_plugins(PhysicsPlugin);
    app.finish();
}
