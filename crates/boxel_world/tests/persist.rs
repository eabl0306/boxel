//! Saving and loading the world (ADR 0024) in headless apps with inline jobs (and once on the
//! task pools, as in the game): edits survive an unload and a restart of the `App`, near the
//! origin and far from it (±2^31, 2^40 and the world limit); chunks nobody edited are never
//! saved; the light's sky tops of a restarted world
//! are the ones it had, also where they come from a saved chunk that is not loaded; flowing
//! water goes on after a restart as if nothing happened; damaged records, damaged or missing
//! files, another format version and block states that no longer exist are handled; a chunk
//! that comes back before its save lands, and a store that fails, keep the edits.
use boxel_app::App;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockPos, BlockRegistry, BlockRegistryBuilder,
    BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos, FaceTextures, LocalPos,
    MAX_BLOCK_COORD, RenderShape, TextureId,
};
use boxel_world::jobs::{InlineExecutor, IoTaskPoolExecutor, JobExecutor, TaskPoolExecutor};
use boxel_world::persist::{
    ChunkStore, FORMAT_VERSION, MemoryStore, PersistError, PersistPlugin, PersistSettings,
    SaveBatch, WorldStore,
};
use boxel_world::{
    BlockTicks, ChunkMap, ChunkViewer, EditQueue, FluidPlugin, SkyMap, TimeOfDay, WorldPlugin,
    WorldSettings, WorldStats,
};
use boxel_worldgen::WorldGenerator;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ the world

/// Stone below `level`, air from `level` up, at any height (the generator's top is the layer
/// under `level`).
struct Ground {
    level: i64,
}

impl WorldGenerator for Ground {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let stone = registry.by_name("boxel:stone").unwrap();
        let o = key.pos.origin();
        if o.y >= self.level {
            return ChunkData::uniform(BlockState::AIR);
        }
        if o.y + i64::from(CHUNK_SIZE) <= self.level {
            return ChunkData::uniform(stone);
        }
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        for (i, slot) in dense.iter_mut().enumerate() {
            if o.y + i64::from(LocalPos::from_index(i).y()) < self.level {
                *slot = stone;
            }
        }
        ChunkData::from_dense(&dense)
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        i64::MIN..=(self.level - 1) >> 5
    }
    fn name(&self) -> &str {
        "ground"
    }
}

/// An invisible block named `name`.
fn plain(name: &str) -> BlockDef {
    BlockDef {
        name: name.to_string(),
        shape: RenderShape::None,
        model: BlockModel::CUBE,
        collision: BlockCollision::None,
        emission: [0; 3],
        light_opacity: 0,
        textures: FaceTextures::all(TextureId(0)),
    }
}

/// How a test app is made.
#[derive(Clone)]
struct Setup {
    backend: Arc<dyn ChunkStore>,
    registry: Arc<BlockRegistry>,
    level: i64,
    light: bool,
    fluids: bool,
    /// Where chunk jobs run.
    jobs: Arc<dyn JobExecutor>,
    /// Where save jobs run.
    io: Arc<dyn JobExecutor>,
    /// Vertical mesh radius of the viewer.
    vertical: u32,
}

impl Setup {
    fn new(backend: Arc<dyn ChunkStore>, level: i64) -> Self {
        Self {
            backend,
            registry: Arc::new(BlockRegistry::default_blocks()),
            level,
            light: false,
            fluids: false,
            jobs: Arc::new(InlineExecutor),
            io: Arc::new(InlineExecutor),
            vertical: 1,
        }
    }

    /// Opens the world and starts an app with the viewer at `at`, settled.
    fn start(&self, at: BlockPos) -> (App, Entity) {
        let generator = Ground { level: self.level };
        let store = WorldStore::open(Arc::clone(&self.backend), &self.registry, generator.name())
            .expect("the world opens");
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(generator),
            registry: Arc::clone(&self.registry),
            executor: Arc::clone(&self.jobs),
            settings: WorldSettings {
                light: self.light,
                ..WorldSettings::default()
            },
        })
        .add_plugins(PersistPlugin {
            store,
            executor: Arc::clone(&self.io),
        });
        if self.fluids {
            app.add_plugins(FluidPlugin);
        }
        // No autosave: these tests decide when chunks are saved.
        app.insert_resource(PersistSettings {
            autosave: None,
            retry_after: Duration::ZERO,
            ..PersistSettings::default()
        });
        let viewer = app
            .world_mut()
            .spawn((
                Transform::from_translation(center(at)),
                ChunkViewer::new(1).with_vertical_radius(self.vertical),
            ))
            .id();
        settle(&mut app);
        (app, viewer)
    }
}

/// The center of block `p` as a viewer position (exact for the positions these tests use).
fn center(p: BlockPos) -> DVec3 {
    DVec3::new(p.x as f64 + 0.5, p.y as f64 + 0.5, p.z as f64 + 0.5)
}

/// Updates until the world is settled and nothing waits to be written (60 s max).
fn settle(app: &mut App) {
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        let pending = app.world().resource::<WorldStore>().stats().pending;
        if stats.chunks > 0 && stats.is_settled() && pending == 0 {
            // One more frame: the save job of this one reports in the next.
            app.update();
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "not settled");
    }
}

fn move_viewer(app: &mut App, viewer: Entity, to: BlockPos) {
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .unwrap()
        .translation = center(to);
    settle(app);
}

fn edit(app: &mut App, edits: &[(BlockPos, BlockState)]) {
    let mut queue = app.world_mut().resource_mut::<EditQueue>();
    for &(p, s) in edits {
        queue.set_block(p, s);
    }
    settle(app);
}

fn block(app: &App, p: BlockPos) -> BlockState {
    app.world()
        .resource::<ChunkMap>()
        .block_at(p)
        .unwrap_or_else(|| panic!("{p:?} is not loaded"))
}

/// Every voxel of the loaded chunks in `chunks`.
fn voxels(app: &App, chunks: &BTreeSet<ChunkPos>) -> BTreeMap<ChunkPos, Vec<BlockState>> {
    let map = app.world().resource::<ChunkMap>();
    chunks
        .iter()
        .map(|&c| {
            let data = map.data_at(c).unwrap_or_else(|| panic!("{c:?} not loaded"));
            let mut out = vec![BlockState::AIR; CHUNK_VOLUME];
            data.to_dense(&mut out);
            (c, out)
        })
        .collect()
}

fn saved_keys(backend: &dyn ChunkStore) -> BTreeSet<ChunkPos> {
    backend.keys().unwrap().into_iter().map(|k| k.pos).collect()
}

fn at(o: BlockPos, x: i64, y: i64, z: i64) -> BlockPos {
    BlockPos::new(o.x + x, o.y + y, o.z + z)
}

/// The test places: near the origin, across ±2^31, at 2^40 and next to the world limit
/// (multiples of 2^14 there, so the viewer's `f64` position is exact).
fn origins() -> [BlockPos; 4] {
    let edge = i64::from(i32::MAX) + 1;
    let limit = MAX_BLOCK_COORD - (1 << 14);
    [
        BlockPos::new(0, 0, 0),
        BlockPos::new(edge - 8, -edge + 8, -edge - 8),
        BlockPos::new(1 << 40, -(1 << 40), 1 << 40),
        BlockPos::new(limit, limit, -limit),
    ]
}

/// Edits around `o` (the ground's top is `o.y - 1`): a stone tower crossing into the layer above
/// the generator (known air there), a shaft dug through two layers, a log on its side and a
/// slab, across chunk borders.
fn scene(registry: &BlockRegistry, o: BlockPos) -> Vec<(BlockPos, BlockState)> {
    let stone = registry.by_name("boxel:stone").unwrap();
    let log = registry.parse_state("boxel:log[axis=x]").unwrap();
    let slab = registry.parse_state("boxel:stone_slab[type=top]").unwrap();
    let mut edits = Vec::new();
    for y in 0..40 {
        edits.push((at(o, 3, y, 3), stone));
    }
    for y in 1..=40 {
        edits.push((at(o, -5, -y, -5), BlockState::AIR));
    }
    edits.push((at(o, 1, 0, -1), log));
    edits.push((at(o, -1, 0, 1), slab));
    edits
}

// ------------------------------------------------------------------ round trips

#[test]
fn edits_survive_unload_and_restart_near_and_far() {
    for o in origins() {
        round_trip(&Setup::new(Arc::new(MemoryStore::new()), o.y), o);
    }
}

/// The game's executors: chunk jobs on the async compute pool, saves on the IO pool.
#[test]
fn edits_survive_unload_and_restart_with_the_task_pools() {
    let o = origins()[2];
    let mut setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
    setup.jobs = Arc::new(TaskPoolExecutor);
    setup.io = Arc::new(IoTaskPoolExecutor);
    round_trip(&setup, o);
}

/// Edits around `o`, unloads and loads them again, restarts the app on the same store and
/// compares every voxel of the edited chunks; only those are saved.
fn round_trip(setup: &Setup, o: BlockPos) {
    let backend = &setup.backend;
    let (mut app, viewer) = setup.start(o);
    let edits = scene(&setup.registry, o);
    edit(&mut app, &edits);
    let chunks: BTreeSet<ChunkPos> = edits
        .iter()
        .map(|(p, _)| ChunkPos::containing(*p))
        .collect();
    let before = voxels(&app, &chunks);
    assert!(
        saved_keys(backend.as_ref()).is_empty(),
        "{o:?}: nothing saved yet"
    );

    // Away: the edited chunks unload and are saved; untouched chunks are not.
    let away = at(o, 1 << 13, 0, 0);
    move_viewer(&mut app, viewer, away);
    for c in &chunks {
        assert!(
            app.world().resource::<ChunkMap>().id_at(*c).is_none(),
            "{c:?}"
        );
    }
    assert_eq!(saved_keys(backend.as_ref()), chunks, "{o:?}: only edits");

    // Back: loaded, not generated.
    move_viewer(&mut app, viewer, o);
    assert_eq!(voxels(&app, &chunks), before, "{o:?}: after reloading");
    app.shutdown();
    drop(app);

    // A new app on the same store.
    let (app, _) = setup.start(o);
    assert_eq!(voxels(&app, &chunks), before, "{o:?}: after a restart");
    for &(p, s) in &edits {
        assert_eq!(block(&app, p), s, "{o:?}: {p:?}");
    }
    assert_eq!(
        saved_keys(backend.as_ref()),
        chunks,
        "{o:?}: still only edits"
    );
    let stats = app.world().resource::<WorldStore>().stats();
    assert_eq!(stats.load_errors, 0);
    assert!(stats.loaded_chunks >= chunks.len(), "{stats:?}");
}

#[test]
fn loaded_edits_are_saved_on_exit_and_by_the_autosave() {
    let o = BlockPos::new(-70, 10, 33);
    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    let setup = Setup::new(Arc::clone(&backend), o.y);
    let stone = setup.registry.by_name("boxel:stone").unwrap();

    // Exit: never unloaded, saved by the `Shutdown` schedule.
    let (mut app, _) = setup.start(o);
    edit(&mut app, &[(at(o, 0, 2, 0), stone)]);
    assert!(saved_keys(backend.as_ref()).is_empty());
    app.shutdown();
    drop(app);
    let chunk = ChunkPos::containing(at(o, 0, 2, 0));
    assert_eq!(saved_keys(backend.as_ref()), BTreeSet::from([chunk]));
    let (mut app, _) = setup.start(o);
    assert_eq!(block(&app, at(o, 0, 2, 0)), stone);

    // Autosave: every frame here, without unloading or exiting.
    app.insert_resource(PersistSettings {
        autosave: Some(Duration::ZERO),
        ..PersistSettings::default()
    });
    edit(&mut app, &[(at(o, 40, 2, 0), stone)]);
    let other = ChunkPos::containing(at(o, 40, 2, 0));
    assert_eq!(saved_keys(backend.as_ref()), BTreeSet::from([chunk, other]));
    // The loaded copy is clean again: the next autosave writes nothing new.
    let saved = app.world().resource::<WorldStore>().stats().saved_chunks;
    settle(&mut app);
    assert_eq!(
        app.world().resource::<WorldStore>().stats().saved_chunks,
        saved
    );
}

// ------------------------------------------------------------------ light

/// A world with light: the sky tops of every column around `o` (loaded tiles).
fn tops(app: &App, o: BlockPos, columns: &[(i64, i64)]) -> Vec<(i64, i64, i64)> {
    let sky = app.world().resource::<SkyMap>();
    columns
        .iter()
        .map(|&(dx, dz)| {
            let (x, z) = (o.x + dx, o.z + dz);
            (x, z, sky.top(x, z).expect("walked"))
        })
        .collect()
}

#[test]
fn sky_tops_come_back_from_saved_chunks_even_unloaded() {
    for o in [origins()[0], origins()[2]] {
        let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
        let mut setup = Setup::new(Arc::clone(&backend), o.y);
        setup.light = true;
        let stone = setup.registry.by_name("boxel:stone").unwrap();
        // A block high above the ground (6 layers up: never loaded below), and a shaft whose
        // bottom is the column's new top.
        let high = at(o, 2, 200, 2);
        let (mut app, viewer) = setup.start(high);
        edit(&mut app, &[(high, stone)]);
        move_viewer(&mut app, viewer, o);
        let mut shaft: Vec<(BlockPos, BlockState)> = (1..=20)
            .map(|y| (at(o, -3, -y, 4), BlockState::AIR))
            .collect();
        shaft.push((at(o, 6, 0, 6), stone));
        edit(&mut app, &shaft);
        let columns = [(2, 2), (-3, 4), (6, 6), (0, 0)];
        let before = tops(&app, o, &columns);
        assert_eq!(before[0].2, high.y, "{o:?}: the high block is the top");
        assert_eq!(before[1].2, o.y - 21, "{o:?}: the stone under the shaft is");
        assert_eq!(before[2].2, o.y, "{o:?}: a block on the ground is");
        assert_eq!(before[3].2, o.y - 1, "{o:?}: the ground is");
        assert!(
            app.world()
                .resource::<ChunkMap>()
                .id_at(ChunkPos::containing(high))
                .is_none(),
            "the high block's chunk is not loaded"
        );
        app.shutdown();
        drop(app);

        // Restarted below the high block: its column is walked from the saved chunk.
        let (app, _) = setup.start(o);
        assert!(
            app.world()
                .resource::<ChunkMap>()
                .id_at(ChunkPos::containing(high))
                .is_none()
        );
        assert_eq!(tops(&app, o, &columns), before, "{o:?}");
    }
}

// ------------------------------------------------------------------ flowing water

/// Water cells (and their levels) around `o`.
fn water(app: &App, o: BlockPos) -> Vec<(BlockPos, BlockState)> {
    let map = app.world().resource::<ChunkMap>();
    let registry = BlockRegistry::default_blocks();
    let mut out = Vec::new();
    for x in -10..=10 {
        for z in -10..=10 {
            for y in -2..=3 {
                let p = at(o, x, y, z);
                let s = map.block_at(p).unwrap();
                if registry.name(s) == "boxel:water" {
                    out.push((p, s));
                }
            }
        }
    }
    out
}

fn step(app: &mut App) {
    app.world_mut().resource_mut::<BlockTicks>().advance(1);
    app.update();
}

#[test]
fn flowing_water_goes_on_after_a_restart() {
    for o in [origins()[0], origins()[1]] {
        let source = |registry: &BlockRegistry| {
            vec![(
                at(o, 0, 0, 0),
                registry.parse_state("boxel:water[level=0]").unwrap(),
            )]
        };
        // Uninterrupted.
        let mut setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
        setup.fluids = true;
        let (mut uninterrupted, _) = setup.start(o);
        uninterrupted
            .world_mut()
            .resource_mut::<EditQueue>()
            .set_block(source(&setup.registry)[0].0, source(&setup.registry)[0].1);
        uninterrupted.update();
        for _ in 0..40 {
            step(&mut uninterrupted);
        }

        // Saved after 12 ticks, mid flow, and restarted.
        let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
        setup.backend = Arc::clone(&backend);
        let (mut first, _) = setup.start(o);
        first
            .world_mut()
            .resource_mut::<EditQueue>()
            .set_block(source(&setup.registry)[0].0, source(&setup.registry)[0].1);
        first.update();
        for _ in 0..12 {
            step(&mut first);
        }
        assert!(first.world().resource::<BlockTicks>().stats().queued > 0);
        first.shutdown();
        drop(first);
        let (mut second, _) = setup.start(o);
        assert!(second.world().resource::<BlockTicks>().stats().queued > 0);
        for _ in 12..40 {
            step(&mut second);
        }
        assert_eq!(water(&second, o), water(&uninterrupted, o), "{o:?}");
        assert!(water(&second, o).len() > 20, "the water spread");
    }
}

/// Water whose chunks unload mid flow is saved with its ticks, in the same write: after a crash
/// (no `Shutdown`, no autosave) it goes on as if nothing happened, never stuck half spread.
#[test]
fn water_saved_as_it_unloads_goes_on_after_a_crash() {
    for o in [origins()[0], origins()[1]] {
        let source = |registry: &BlockRegistry| {
            (
                at(o, 0, 0, 0),
                registry.parse_state("boxel:water[level=0]").unwrap(),
            )
        };
        let mut setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
        setup.fluids = true;
        let (mut uninterrupted, _) = setup.start(o);
        let (p, s) = source(&setup.registry);
        uninterrupted
            .world_mut()
            .resource_mut::<EditQueue>()
            .set_block(p, s);
        uninterrupted.update();
        for _ in 0..40 {
            step(&mut uninterrupted);
        }

        // Unloaded after 12 ticks, mid flow, and saved; then the game dies.
        let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
        setup.backend = Arc::clone(&backend);
        let (mut first, viewer) = setup.start(o);
        first
            .world_mut()
            .resource_mut::<EditQueue>()
            .set_block(p, s);
        first.update();
        for _ in 0..12 {
            step(&mut first);
        }
        assert!(first.world().resource::<BlockTicks>().stats().queued > 0);
        move_viewer(&mut first, viewer, at(o, 1 << 13, 0, 0));
        assert!(saved_keys(backend.as_ref()).contains(&ChunkPos::containing(p)));
        drop(first);

        let (mut second, _) = setup.start(o);
        assert!(
            second.world().resource::<BlockTicks>().stats().queued > 0,
            "{o:?}: the ticks were saved with the water"
        );
        for _ in 12..40 {
            step(&mut second);
        }
        assert_eq!(water(&second, o), water(&uninterrupted, o), "{o:?}");
    }
}

/// While water flows in a loaded chunk, another chunk with edits unloads and is saved: the
/// water's chunk and its ticks are saved with it, of the same moment. After a crash the water
/// goes on as if nothing happened.
#[test]
fn a_crash_after_an_unload_keeps_loaded_water_and_its_ticks_together() {
    // The water stays inside chunk x = 0 (and its reach too); the edit is in chunk x = -2.
    let o = BlockPos::new(16, 0, 16);
    let mut setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
    setup.fluids = true;
    let stone = setup.registry.by_name("boxel:stone").unwrap();
    let source = (
        o,
        setup.registry.parse_state("boxel:water[level=0]").unwrap(),
    );
    let elsewhere = at(o, -64, 1, 0);
    let run = |app: &mut App, from: u32, to: u32| {
        for _ in from..to {
            step(app);
        }
    };
    let (mut uninterrupted, _) = setup.start(o);
    edit(&mut uninterrupted, &[(elsewhere, stone)]);
    uninterrupted
        .world_mut()
        .resource_mut::<EditQueue>()
        .set_block(source.0, source.1);
    uninterrupted.update();
    run(&mut uninterrupted, 0, 40);

    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    setup.backend = Arc::clone(&backend);
    let (mut first, viewer) = setup.start(o);
    edit(&mut first, &[(elsewhere, stone)]);
    first
        .world_mut()
        .resource_mut::<EditQueue>()
        .set_block(source.0, source.1);
    first.update();
    run(&mut first, 0, 12);
    // Three chunks east: the edited chunk unloads, the water's stays (within the hysteresis).
    move_viewer(&mut first, viewer, at(o, 96, 0, 0));
    let map = first.world().resource::<ChunkMap>();
    assert!(map.id_at(ChunkPos::containing(elsewhere)).is_none());
    assert!(map.id_at(ChunkPos::containing(o)).is_some());
    assert!(saved_keys(backend.as_ref()).contains(&ChunkPos::containing(o)));
    drop(first);

    let (mut second, _) = setup.start(o);
    assert_eq!(block(&second, elsewhere), stone);
    run(&mut second, 12, 40);
    assert_eq!(water(&second, o), water(&uninterrupted, o));
}

#[test]
fn the_world_clock_is_saved() {
    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    let setup = Setup::new(Arc::clone(&backend), 0);
    let (mut app, _) = setup.start(BlockPos::new(0, 0, 0));
    app.insert_resource(TimeOfDay::at_clock(21.5));
    let tick = app.world().resource::<TimeOfDay>().tick;
    app.shutdown();
    drop(app);
    let restart = |settings: PersistSettings, hours: f64| {
        let mut app = App::new();
        let store = WorldStore::open(Arc::clone(&backend), &setup.registry, "ground").unwrap();
        app.add_plugins(WorldPlugin {
            executor: Arc::new(InlineExecutor),
            ..WorldPlugin::default()
        })
        .add_plugins(PersistPlugin {
            store,
            executor: Arc::new(InlineExecutor),
        })
        .insert_resource(settings)
        .insert_resource(TimeOfDay::at_clock(hours));
        app.update();
        app
    };
    let app = restart(PersistSettings::default(), 6.0);
    assert_eq!(app.world().resource::<TimeOfDay>().tick, tick);
    drop(app);

    // A game that sets the clock itself: neither restored nor saved.
    let six = TimeOfDay::at_clock(6.0).tick;
    let off = PersistSettings {
        clock: false,
        ..PersistSettings::default()
    };
    let mut app = restart(off.clone(), 6.0);
    assert_eq!(app.world().resource::<TimeOfDay>().tick, six);
    app.shutdown();
    drop(app);
    let app = restart(PersistSettings::default(), 6.0);
    assert_eq!(app.world().resource::<TimeOfDay>().tick, tick, "still");
}

/// A `WorldStore` serves one `PersistPlugin`. A second one (a clone of the store, in a new app)
/// would start without the index of saved chunks and save generated copies over them: it
/// panics. A new app opens the world again.
#[test]
#[should_panic(expected = "a WorldStore serves one PersistPlugin")]
fn a_world_store_serves_one_persist_plugin() {
    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    let registry = BlockRegistry::default_blocks();
    let app = |store: WorldStore| {
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            executor: Arc::new(InlineExecutor),
            ..WorldPlugin::default()
        })
        .add_plugins(PersistPlugin {
            store,
            executor: Arc::new(InlineExecutor),
        });
        app
    };
    drop(app(WorldStore::open(
        Arc::clone(&backend),
        &registry,
        "ground",
    )
    .unwrap()));
    let store = WorldStore::open(backend, &registry, "ground").unwrap();
    drop(app(store.clone()));
    app(store);
}

// ------------------------------------------------------------------ damage and change

#[test]
fn a_damaged_record_regenerates_its_chunk_and_stays_until_edited() {
    let o = BlockPos::new(5, 0, 5);
    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    let setup = Setup::new(Arc::clone(&backend), o.y);
    let stone = setup.registry.by_name("boxel:stone").unwrap();
    let (mut app, _) = setup.start(o);
    edit(&mut app, &[(at(o, 0, 3, 0), stone)]);
    app.shutdown();
    drop(app);
    let chunk = ChunkPos::containing(at(o, 0, 3, 0));
    let key = ChunkKey::new(chunk);
    let garbage = b"definitely not a chunk".to_vec();
    backend
        .save_batch(&SaveBatch {
            chunks: vec![(key, garbage.clone())],
            meta: Vec::new(),
        })
        .unwrap();

    let (mut app, _) = setup.start(o);
    assert_eq!(block(&app, at(o, 0, 3, 0)), BlockState::AIR, "generated");
    assert_eq!(app.world().resource::<WorldStore>().stats().load_errors, 1);
    app.shutdown();
    drop(app);
    assert_eq!(
        backend.load(key).unwrap(),
        Some(garbage),
        "a record that was only read is left as it is"
    );

    let (mut app, _) = setup.start(o);
    edit(&mut app, &[(at(o, 1, 3, 0), stone)]);
    app.shutdown();
    drop(app);
    let (app, _) = setup.start(o);
    assert_eq!(block(&app, at(o, 1, 3, 0)), stone, "edited: saved again");
    assert_eq!(app.world().resource::<WorldStore>().stats().load_errors, 0);
}

#[test]
fn worlds_of_another_version_or_with_damaged_metadata_are_refused() {
    let registry = BlockRegistry::default_blocks();
    // A world, then damage to one metadata value, then the world again.
    let meta = |name: &str, value: Vec<u8>| {
        let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
        WorldStore::open(Arc::clone(&backend), &registry, "ground").unwrap();
        backend
            .save_batch(&SaveBatch {
                chunks: Vec::new(),
                meta: vec![(name.to_string(), value)],
            })
            .unwrap();
        WorldStore::open(backend, &registry, "ground")
    };
    match meta("boxel:format", 99u32.to_le_bytes().to_vec()) {
        Err(PersistError::Version { found, supported }) => {
            assert_eq!((found, supported), (99, FORMAT_VERSION));
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        meta("boxel:format", vec![1, 2]),
        Err(PersistError::Damaged(_))
    ));
    assert!(matches!(
        meta("boxel:states", b"garbage".to_vec()),
        Err(PersistError::Damaged(_))
    ));
    // Chunks without a version are not taken for a new world.
    let backend = Arc::new(MemoryStore::new());
    backend
        .save_batch(&SaveBatch {
            chunks: vec![(ChunkKey::new(ChunkPos::new(0, 0, 0)), vec![1, 2, 3])],
            meta: Vec::new(),
        })
        .unwrap();
    assert!(matches!(
        WorldStore::open(backend, &registry, "ground"),
        Err(PersistError::Damaged(_))
    ));
    // Damaged ticks or clock only lose themselves.
    assert!(meta("boxel:ticks", b"garbage".to_vec()).is_ok());
    assert!(meta("boxel:clock", b"garbage".to_vec()).is_ok());
}

#[test]
fn a_state_that_no_longer_exists_loads_as_air_and_the_rest_stays() {
    let o = BlockPos::new(0, 0, 0);
    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    let mut setup = Setup::new(Arc::clone(&backend), o.y);
    let mut builder = BlockRegistryBuilder::with_default_blocks();
    let gem = builder.add_block(plain("test:gem")).unwrap();
    setup.registry = Arc::new(builder.build());
    let log = setup.registry.parse_state("boxel:log[axis=z]").unwrap();
    let (mut app, _) = setup.start(o);
    edit(&mut app, &[(at(o, 0, 1, 0), gem), (at(o, 1, 1, 0), log)]);
    app.shutdown();
    drop(app);

    // The next version of the game has no gems.
    setup.registry = Arc::new(BlockRegistry::default_blocks());
    let (app, _) = setup.start(o);
    assert_eq!(block(&app, at(o, 0, 1, 0)), BlockState::AIR);
    assert_eq!(
        block(&app, at(o, 1, 1, 0)),
        setup.registry.parse_state("boxel:log[axis=z]").unwrap()
    );
    let store = app.world().resource::<WorldStore>();
    assert_eq!(store.ids().unknown().len(), 1);
    assert_eq!(store.ids().unknown()[0].0, "test:gem");
}

/// A fresh directory for a redb world (under the target dir), removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("persist-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn redb_worlds_round_trip_and_damaged_files_are_refused() {
    let tmp = TempDir::new("redb");
    // A directory that does not exist yet (nor its parent).
    let dir = tmp.0.join("saves").join("world");
    let registry = Arc::new(BlockRegistry::default_blocks());
    let stone = registry.by_name("boxel:stone").unwrap();
    let o = BlockPos::new(1 << 40, 64, -(1 << 40));
    let start = |dir: &PathBuf| {
        let store = WorldStore::open_dir(dir, &registry, "ground").expect("opens");
        let mut setup = Setup::new(Arc::clone(store.backend()), o.y);
        setup.registry = Arc::clone(&registry);
        drop(store);
        setup
    };
    let setup = start(&dir);
    let (mut app, _) = setup.start(o);
    edit(&mut app, &[(at(o, 2, 5, 2), stone)]);
    app.shutdown();
    drop(app);
    drop(setup);
    let setup = start(&dir);
    let (app, _) = setup.start(o);
    assert_eq!(block(&app, at(o, 2, 5, 2)), stone);
    drop(app);
    drop(setup);

    // Not a database: refused, and left as it is.
    let bad = tmp.0.join("bad");
    std::fs::create_dir_all(&bad).unwrap();
    let file = bad.join(boxel_world::persist::WORLD_FILE);
    let junk: Vec<u8> = (0..20_000u32).map(|i| (i * 7 + 3) as u8).collect();
    std::fs::write(&file, &junk).unwrap();
    assert!(matches!(
        WorldStore::open_dir(&bad, &registry, "ground"),
        Err(PersistError::Io(_))
    ));
    assert_eq!(std::fs::read(&file).unwrap(), junk);
}

// ------------------------------------------------------------------ races and failures

/// Holds save jobs until the test runs them.
#[derive(Default)]
struct HeldJobs(Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>);

impl JobExecutor for HeldJobs {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        self.0.lock().unwrap().push(job);
    }
}

impl HeldJobs {
    fn run_all(&self) {
        let jobs: Vec<_> = self.0.lock().unwrap().drain(..).collect();
        for job in jobs {
            job();
        }
    }
}

/// Updates until the world is settled (saves may wait).
fn settle_world(app: &mut App) {
    let start = Instant::now();
    loop {
        app.update();
        if app.world().resource::<WorldStats>().is_settled() {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "not settled");
    }
}

#[test]
fn a_chunk_that_comes_back_before_its_save_keeps_its_edits() {
    let o = BlockPos::new(0, 0, 0);
    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    let mut setup = Setup::new(Arc::clone(&backend), o.y);
    let held = Arc::new(HeldJobs::default());
    setup.io = Arc::clone(&held) as Arc<dyn JobExecutor>;
    let stone = setup.registry.by_name("boxel:stone").unwrap();
    let (mut app, viewer) = {
        // `start` waits for saves: this one cannot.
        let generator = Ground { level: o.y };
        let store =
            WorldStore::open(Arc::clone(&backend), &setup.registry, generator.name()).unwrap();
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(generator),
            registry: Arc::clone(&setup.registry),
            executor: Arc::new(InlineExecutor),
            settings: WorldSettings {
                light: false,
                ..WorldSettings::default()
            },
        })
        .add_plugins(PersistPlugin {
            store,
            executor: Arc::clone(&setup.io),
        })
        .insert_resource(PersistSettings {
            autosave: None,
            ..PersistSettings::default()
        });
        let viewer = app
            .world_mut()
            .spawn((
                Transform::from_translation(center(o)),
                ChunkViewer::new(1).with_vertical_radius(1),
            ))
            .id();
        settle_world(&mut app);
        (app, viewer)
    };
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(at(o, 0, 1, 0), stone);
    settle_world(&mut app);
    for to in [at(o, 1 << 12, 0, 0), o] {
        app.world_mut()
            .get_mut::<Transform>(viewer)
            .unwrap()
            .translation = center(to);
        settle_world(&mut app);
    }
    // Unloaded, saved by a job that has not run, and loaded again: from the staged copy.
    assert!(backend.keys().unwrap().is_empty(), "the save has not run");
    assert_eq!(block(&app, at(o, 0, 1, 0)), stone);
    held.run_all();
    settle_world(&mut app);
    assert_eq!(saved_keys(backend.as_ref()).len(), 1);
    assert_eq!(app.world().resource::<WorldStore>().stats().pending, 0);
}

/// A store whose writes fail while `fail` is set.
#[derive(Default)]
struct Flaky {
    inner: MemoryStore,
    fail: AtomicBool,
}

impl ChunkStore for Flaky {
    fn load(&self, key: ChunkKey) -> io::Result<Option<Vec<u8>>> {
        self.inner.load(key)
    }
    fn keys(&self) -> io::Result<Vec<ChunkKey>> {
        self.inner.keys()
    }
    fn load_meta(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.inner.load_meta(name)
    }
    fn save_batch(&self, batch: &SaveBatch) -> io::Result<()> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(io::Error::other("disk full"));
        }
        self.inner.save_batch(batch)
    }
}

#[test]
fn a_failing_store_keeps_the_edits_in_memory_and_tries_again() {
    let o = BlockPos::new(0, 0, 0);
    let flaky = Arc::new(Flaky::default());
    let setup = Setup::new(Arc::clone(&flaky) as Arc<dyn ChunkStore>, o.y);
    let stone = setup.registry.by_name("boxel:stone").unwrap();
    let (mut app, viewer) = setup.start(o);
    edit(&mut app, &[(at(o, 0, 1, 0), stone)]);
    flaky.fail.store(true, Ordering::Relaxed);
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .unwrap()
        .translation = center(at(o, 1 << 12, 0, 0));
    settle_world(&mut app);
    for _ in 0..3 {
        app.update();
    }
    let stats = app.world().resource::<WorldStore>().stats();
    assert!(stats.save_errors >= 1, "{stats:?}");
    assert_eq!(stats.pending, 1, "the chunk waits in memory");
    assert!(flaky.inner.is_empty());
    // Back while the disk is still full: the edit is there.
    app.world_mut()
        .get_mut::<Transform>(viewer)
        .unwrap()
        .translation = center(o);
    settle_world(&mut app);
    assert_eq!(block(&app, at(o, 0, 1, 0)), stone);
    // The disk works again: the next try writes it.
    flaky.fail.store(false, Ordering::Relaxed);
    settle(&mut app);
    assert_eq!(flaky.inner.len(), 1);
    app.shutdown();
    drop(app);
    let (app, _) = setup.start(o);
    assert_eq!(block(&app, at(o, 0, 1, 0)), stone);
}

/// A store whose loads fail while `fail` is set (a disk that cannot be read for a while).
#[derive(Default)]
struct Unreadable {
    inner: MemoryStore,
    fail: AtomicBool,
}

impl ChunkStore for Unreadable {
    fn load(&self, key: ChunkKey) -> io::Result<Option<Vec<u8>>> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(io::Error::other("the disk cannot be read"));
        }
        self.inner.load(key)
    }
    fn keys(&self) -> io::Result<Vec<ChunkKey>> {
        self.inner.keys()
    }
    fn load_meta(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.inner.load_meta(name)
    }
    fn save_batch(&self, batch: &SaveBatch) -> io::Result<()> {
        self.inner.save_batch(batch)
    }
}

/// A saved chunk the store cannot read is not generated in its place: that copy could be edited
/// and saved over the good record. It stays out (air nobody can edit) until it loads again.
#[test]
fn a_chunk_the_store_cannot_read_is_never_saved_over() {
    let o = BlockPos::new(0, 0, 0);
    let disk = Arc::new(Unreadable::default());
    let setup = Setup::new(Arc::clone(&disk) as Arc<dyn ChunkStore>, o.y);
    let stone = setup.registry.by_name("boxel:stone").unwrap();
    // Two blocks of one chunk, above the ground.
    let (p, q) = (at(o, 2, 3, 2), at(o, 5, 3, 5));
    let (mut app, _) = setup.start(o);
    edit(&mut app, &[(p, stone)]);
    app.shutdown();
    drop(app);

    disk.fail.store(true, Ordering::Relaxed);
    let (mut app, _) = setup.start(o);
    assert_eq!(block(&app, p), BlockState::AIR, "left out");
    edit(&mut app, &[(q, stone)]);
    assert_eq!(block(&app, q), BlockState::AIR, "not editable");
    assert!(app.world().resource::<WorldStore>().stats().load_errors >= 1);
    app.shutdown();
    drop(app);

    disk.fail.store(false, Ordering::Relaxed);
    let (app, _) = setup.start(o);
    assert_eq!(block(&app, p), stone, "the record is intact");
    assert_eq!(block(&app, q), BlockState::AIR);
}

#[test]
fn redb_keeps_every_bit_of_far_keys() {
    let tmp = TempDir::new("keys");
    let path = tmp.0.join(boxel_world::persist::WORLD_FILE);
    let far = [
        0,
        -1,
        i64::from(i32::MAX) + 1,
        i64::from(i32::MIN) - 1,
        1 << 40,
        -(1 << 40),
        boxel_voxel::MAX_CHUNK_COORD,
        -boxel_voxel::MAX_CHUNK_COORD,
    ];
    let keys: Vec<ChunkKey> = far
        .iter()
        .flat_map(|&x| far.iter().map(move |&y| (x, y)))
        .map(|(x, y)| ChunkKey::new(ChunkPos::new(x, y, -x)))
        .collect();
    {
        let store = boxel_world::persist::RedbStore::open(&path).unwrap();
        let batch = SaveBatch {
            chunks: keys
                .iter()
                .map(|k| (*k, format!("{:?}", k.pos).into_bytes()))
                .collect(),
            meta: vec![("game:seed".to_string(), b"42".to_vec())],
        };
        store.save_batch(&batch).unwrap();
    }
    let store = boxel_world::persist::RedbStore::open(&path).unwrap();
    let mut back = store.keys().unwrap();
    back.sort_unstable();
    let mut want = keys.clone();
    want.sort_unstable();
    assert_eq!(back, want);
    for k in &keys {
        assert_eq!(
            store.load(*k).unwrap(),
            Some(format!("{:?}", k.pos).into_bytes())
        );
    }
    assert_eq!(store.load_meta("game:seed").unwrap(), Some(b"42".to_vec()));
    assert_eq!(
        store.load(ChunkKey::new(ChunkPos::new(7, 7, 7))).unwrap(),
        None
    );
}
