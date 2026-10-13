//! Unit tests of the lifecycle: what needs crate-private state (the job state, the save state,
//! the LOD, the markers of saved entities). The public behavior is in `tests/lifecycle.rs`.
use super::{CloseOptions, WorldScoped, WorldSpec, WorldStatus, close_world, open_world};
use crate::jobs::{InlineExecutor, JobExecutor};
use crate::lod::{LodFeedback, LodResync, LodSettings, LodWorld, LodWorldPlugin};
use crate::persist::{MemoryStore, PersistPlugin, Saved, SavedLog, WorldStore};
use crate::streaming::WorldJobs;
use crate::tests::support::{HoldExecutor, LayerGen, fake_mesh};
use crate::{
    BlockTicks, ChunkMap, ChunkMeshResync, ChunkViewer, EditQueue, WorldPlugin, WorldSettings,
    WorldStats,
};
use boxel_app::{App, IdleWanted};
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, ChunkPos, testing};
use std::sync::Arc;
use std::time::Duration;

/// 2^40: the worlds sit there.
const FAR: i64 = 1 << 40;

fn settings() -> WorldSettings {
    WorldSettings {
        vertical_bounds: Some(-2..=1),
        ..WorldSettings::default()
    }
}

fn spec(surface: i64) -> WorldSpec {
    let mut generator = LayerGen::new(-2..=1);
    generator.surface = surface;
    WorldSpec::new(Arc::new(generator), settings())
}

/// A closed app: the world's jobs on `jobs`, saves inline, the fake mesher, with the far LOD
/// on `lod` if given.
fn app(jobs: Arc<dyn JobExecutor>, lod: Option<Arc<dyn JobExecutor>>) -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        executor: jobs,
        ..WorldPlugin::closed(Arc::new(boxel_demo::blocks::registry()))
    })
    .add_plugins(PersistPlugin::without_store().with_executor(Arc::new(InlineExecutor)));
    if let Some(executor) = lod {
        app.add_plugins(LodWorldPlugin {
            settings: LodSettings::default(),
            executor,
        });
    }
    app.world_mut().resource_mut::<WorldJobs>().mesh_fn = fake_mesh;
    app.update();
    app
}

fn viewer(app: &mut App) {
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(FAR as f64 + 16.0, 16.0, -FAR as f64 + 16.0)),
        ChunkViewer::new(2).with_vertical_radius(1),
        WorldScoped,
    ));
}

fn open(app: &mut App, spec: WorldSpec) {
    open_world(app.world_mut(), spec).expect("opens");
    viewer(app);
}

fn settle(app: &mut App) {
    for _ in 0..300 {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            return;
        }
    }
    panic!("not settled");
}

fn store(backend: &Arc<MemoryStore>) -> WorldStore {
    WorldStore::open(
        Arc::clone(backend) as _,
        &boxel_demo::blocks::registry(),
        "layers",
    )
    .unwrap()
}

/// Test 6: the jobs of a closed world that run after the next one opened change nothing of it
/// (their results go to the closed world's channels), and read nothing that counts an error.
#[test]
fn a_closed_worlds_jobs_change_nothing() {
    let held = Arc::new(HoldExecutor::default());
    let mut app = app(
        Arc::clone(&held) as _,
        Some(Arc::clone(&held) as Arc<dyn JobExecutor>),
    );
    let backend = Arc::new(MemoryStore::new());
    let a = store(&backend);
    let a_handle = a.clone();
    open(&mut app, spec(0).with_store(a));
    for _ in 0..3 {
        app.update();
        held.run_first(held.held() / 2);
    }
    assert!(held.held() > 0, "A's jobs wait");
    let of_a = held.held();
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    assert_eq!(
        app.world().resource::<super::WorldLifecycle>().status(),
        WorldStatus::Closed
    );
    open(&mut app, spec(1));
    for _ in 0..3 {
        app.update();
    }
    let before = (
        *app.world().resource::<WorldStats>(),
        app.world().resource::<ChunkMap>().len(),
    );
    held.run_first(of_a);
    for _ in 0..2 {
        app.update();
    }
    let after = (
        *app.world().resource::<WorldStats>(),
        app.world().resource::<ChunkMap>().len(),
    );
    assert_eq!(before.1, after.1);
    assert_eq!(
        before.0.data_ready, after.0.data_ready,
        "no result of A was applied"
    );
    let stats = a_handle.stats();
    assert_eq!((stats.load_errors, stats.entity_errors), (0, 0));
    // B runs on.
    for _ in 0..300 {
        held.run_all();
        app.update();
        if app.world().resource::<WorldStats>().is_settled() {
            return;
        }
    }
    panic!("B did not settle");
}

/// Test 7: what a closed world leaves behind never reaches the next one: an edit or a marker
/// queued while closed, the renderer's lists and resyncs written after the teardown (as its
/// next frame does), the idle step's request. The next world settles as a fresh one does.
#[test]
fn a_closed_worlds_leftovers_never_reach_the_next_one() {
    let inline: Arc<dyn JobExecutor> = Arc::new(InlineExecutor);
    let mut app = app(Arc::clone(&inline), Some(Arc::clone(&inline)));
    open(&mut app, spec(0));
    settle(&mut app);
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    // As a runner does before every update: the closed world asks for no idle step.
    IdleWanted::before_update(app.world_mut());
    app.update();
    assert!(!app.world().resource::<IdleWanted>().wanted());
    // Leftovers, while closed.
    let near = BlockPos::new(FAR + 3, 40, -FAR + 3);
    let stone = boxel_demo::blocks::registry()
        .by_name("boxel:stone")
        .unwrap();
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(near, stone);
    app.world_mut().spawn(Saved::new());
    app.world_mut().write_message(ChunkMeshResync);
    app.world_mut().write_message(LodResync);
    {
        let mut feedback = app.world_mut().resource_mut::<LodFeedback>();
        feedback.chunks.push((ChunkPos::new(0, 0, 0), true));
        feedback.refused.push(boxel_voxel::ChunkKey {
            dim: boxel_voxel::DimensionId(0),
            lod: 1,
            pos: ChunkPos::new(0, 0, 0),
        });
    }
    app.update();
    assert!(app.world().resource::<SavedLog>().is_empty());
    open_world(app.world_mut(), spec(1)).unwrap();
    {
        let world = app.world();
        assert!(world.resource::<EditQueue>().is_empty());
        let feedback = world.resource::<LodFeedback>();
        assert!(feedback.chunks.is_empty() && feedback.refused.is_empty());
    }
    viewer(&mut app);
    settle(&mut app);
    let switched = (
        app.world().resource::<ChunkMap>().len(),
        app.world().resource::<ChunkMap>().block_at(near),
    );

    let mut fresh = self::app(Arc::clone(&inline), Some(inline));
    open(&mut fresh, spec(1));
    settle(&mut fresh);
    let fresh = (
        fresh.world().resource::<ChunkMap>().len(),
        fresh.world().resource::<ChunkMap>().block_at(near),
    );
    assert_eq!(switched, fresh);
}

/// Test 8, an unsaved world: its `Saved` entities are despawned at the close, and the markers
/// they log do not pile up while it is open.
#[test]
fn an_unsaved_worlds_saved_entities_go_with_it() {
    let mut app = app(Arc::new(InlineExecutor), None);
    open(&mut app, spec(0));
    let e = app.world_mut().spawn(Saved::new()).id();
    app.update();
    assert!(app.world().resource::<SavedLog>().is_empty());
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    assert!(app.world().get_entity(e).is_err());
}

/// Test 9, the store's identity: a world opened after another one with another store reads
/// its own (the far LOD compared stores by presence only).
#[test]
fn the_far_lod_follows_the_open_worlds_store() {
    let inline: Arc<dyn JobExecutor> = Arc::new(InlineExecutor);
    let mut app = app(Arc::clone(&inline), Some(inline));
    let (x, y) = (
        store(&Arc::new(MemoryStore::new())),
        store(&Arc::new(MemoryStore::new())),
    );
    let (x_handle, y_handle) = (x.clone(), y.clone());
    open(&mut app, spec(0).with_store(x));
    app.update();
    let lod_store = |app: &App| {
        app.world()
            .resource::<LodWorld>()
            .store_for_tests()
            .cloned()
    };
    assert!(lod_store(&app).is_some_and(|s| s.same(&x_handle)));
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    assert!(lod_store(&app).is_none());
    open(&mut app, spec(0).with_store(y));
    app.update();
    assert!(lod_store(&app).is_some_and(|s| s.same(&y_handle)));
}

/// The job state of a closed world is a fresh process's, but for what is the app's: the
/// executor, the mesh job body, the test clock, the measured job cost and the uniform snapshots.
#[test]
fn the_job_state_keeps_only_what_is_the_apps() {
    let mut app = app(Arc::new(InlineExecutor), None);
    open(&mut app, spec(0));
    settle(&mut app);
    let (uniform, failed) = {
        let jobs = app.world().resource::<WorldJobs>();
        (jobs.uniform_len(), jobs.failed)
    };
    assert!(uniform > 0);
    let _ = failed;
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    let jobs = app.world().resource::<WorldJobs>();
    assert_eq!(jobs.uniform_len(), uniform);
    assert_eq!(
        jobs.mesh_fn as *const () as usize,
        fake_mesh as *const () as usize
    );
    assert_eq!((jobs.failed, jobs.interest_steps), (0, 0));
    assert!(jobs.is_clear());
}

/// `BlockTicks::clear_world` keeps the handlers (they are the app's).
#[test]
fn clearing_the_ticks_keeps_the_handlers() {
    let registry = testing::registry();
    let mut ticks = BlockTicks::default();
    let solid = registry.block_by_name("test:solid").unwrap();
    let handler = crate::TickHandler {
        delay: 3,
        reach: [1, 1, 1],
        tick: |_, _, _| {},
        react: None,
    };
    ticks.register(&registry, solid, handler);
    ticks.schedule(BlockPos::new(FAR, 0, 0), 5);
    ticks.advance(10);
    ticks.clear_world();
    assert_eq!((ticks.now(), ticks.pending().len()), (0, 0));
    assert!(ticks.is_world_clear());
    assert!(
        ticks
            .handler(registry.default_state(solid))
            .is_some_and(|h| h.delay == 3)
    );
}

/// `Persist::idle`: nothing staged and no job running.
#[test]
fn the_save_state_is_idle_once_written() {
    let held = Arc::new(HoldExecutor::default());
    let backend = Arc::new(MemoryStore::new());
    let mut p = crate::persist::Persist::new(store(&backend), Arc::clone(&held) as _);
    assert!(p.idle());
    let key = boxel_voxel::ChunkKey {
        dim: boxel_voxel::DimensionId(0),
        lod: 0,
        pos: ChunkPos::new(FAR >> 5, 0, 0),
    };
    p.stage(
        key,
        Arc::new(boxel_voxel::ChunkData::uniform(
            boxel_voxel::BlockState::AIR,
        )),
    );
    assert!(!p.idle());
    let now = std::time::Instant::now();
    p.start(now, 16);
    assert!(!p.idle(), "a job runs");
    held.run_all();
    p.poll(now, Duration::from_secs(1));
    assert!(p.idle());
}

/// A store reopened on the same backend world after world keeps one entry of it in the closed
/// backends (the newest close's count), and a store opened before the newest close is still
/// refused.
#[test]
fn a_backend_closed_again_and_again_is_remembered_once() {
    let mut app = app(Arc::new(InlineExecutor), None);
    let backend = Arc::new(MemoryStore::new());
    for _ in 0..5 {
        open(&mut app, spec(0).with_store(store(&backend)));
        settle(&mut app);
        assert!(close_world(app.world_mut(), CloseOptions::default()));
        assert_eq!(
            app.world().resource::<super::WorldLifecycle>().status(),
            WorldStatus::Closed
        );
    }
    let early = store(&backend);
    open(&mut app, spec(0).with_store(store(&backend)));
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    let lifecycle = app.world().resource::<super::WorldLifecycle>();
    assert_eq!(lifecycle.closed_backends.len(), 1);
    assert_eq!(
        open_world(app.world_mut(), spec(0).with_store(early)),
        Err(super::OpenWorldError::StoreOpenedEarly)
    );
}
