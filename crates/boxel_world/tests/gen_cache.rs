//! The generation cache (ADR 0042) in headless apps with inline jobs, on a cache file: a world
//! that comes back with a full cache has the very chunks, light, sky tops and meshes it has
//! without one, near the origin and far from it (±2^31, 2^40, the world limit), with water
//! flowing and random ticks the same; an edit always beats the cache (and never goes into it);
//! a cache of another stamp regenerates; a damaged or foreign cache file starts empty; an old
//! world without a cache opens as ever.
use boxel_app::App;
use boxel_demo::worldgen::TerrainGenerator;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{BlockPos, BlockRegistry, BlockState, ChunkData, ChunkPos, MAX_BLOCK_COORD};
use boxel_world::jobs::InlineExecutor;
use boxel_world::persist::{
    CACHE_FILE, ChunkStore, GenCache, GenCacheSettings, MemoryStore, PersistPlugin,
    PersistSettings, WorldStore,
};
use boxel_world::{
    BlockTickSettings, BlockTicks, BlockTicksPlugin, ChunkMap, ChunkViewer, EditQueue, FluidPlugin,
    RandomTickHandler, SkyMap, TickContext, WorldPlugin, WorldSettings, WorldStats,
};
use boxel_worldgen::WorldGenerator;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A fresh directory under the target dir, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("gen-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How a test app is made.
#[derive(Clone)]
struct Setup {
    backend: Arc<dyn ChunkStore>,
    registry: Arc<BlockRegistry>,
    /// The cache file's directory; `None`: no cache.
    cache: Option<PathBuf>,
    salt: u64,
    fluids: bool,
    /// Random ticks harden grass ([`harden`]), 400 picks a chunk a tick.
    random: bool,
}

impl Setup {
    fn new(cache: Option<&Path>) -> Self {
        Self {
            backend: Arc::new(MemoryStore::new()),
            registry: Arc::new(boxel_demo::blocks::registry()),
            cache: cache.map(Path::to_path_buf),
            salt: 7,
            fluids: false,
            random: false,
        }
    }

    /// A new world (a fresh store) of the same terrain.
    fn fresh(&self) -> Self {
        Self {
            backend: Arc::new(MemoryStore::new()),
            ..self.clone()
        }
    }

    /// Opens the world (and its cache) and starts an app with the viewer at `at`, settled.
    fn start(&self, at: BlockPos) -> App {
        let generator = TerrainGenerator::default();
        let store = WorldStore::open(Arc::clone(&self.backend), &self.registry, generator.name())
            .expect("the world opens");
        if let Some(dir) = &self.cache {
            let cache = GenCache::open_dir(
                dir,
                &self.registry,
                GenCacheSettings {
                    salt: self.salt,
                    ..GenCacheSettings::default()
                },
            )
            .expect("the cache opens");
            store.attach_cache(cache).expect("one cache");
        }
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(generator),
            registry: Arc::clone(&self.registry),
            executor: Arc::new(InlineExecutor),
            settings: WorldSettings {
                light: true,
                ..WorldSettings::default()
            },
        })
        .add_plugins(PersistPlugin::new(store).with_executor(Arc::new(InlineExecutor)));
        if self.fluids {
            app.add_plugins(FluidPlugin);
        }
        if self.random {
            app.add_plugins(BlockTicksPlugin);
            let grass = self.registry.block_by_name("boxel:grass").expect("grass");
            app.world_mut()
                .resource_mut::<BlockTicks>()
                .register_random(
                    &self.registry,
                    grass,
                    RandomTickHandler {
                        reach: [0, 0, 0],
                        tick: harden,
                    },
                );
            app.world_mut()
                .resource_mut::<BlockTickSettings>()
                .random_ticks_per_chunk = 400;
        }
        app.insert_resource(PersistSettings {
            autosave: None,
            retry_after: Duration::ZERO,
            ..PersistSettings::default()
        });
        app.world_mut().spawn((
            Transform::from_translation(center(at)),
            ChunkViewer::new(2).with_vertical_radius(1),
        ));
        settle(&mut app);
        app
    }
}

fn center(p: BlockPos) -> DVec3 {
    DVec3::new(p.x as f64 + 0.5, p.y as f64 + 0.5, p.z as f64 + 0.5)
}

fn settle(app: &mut App) {
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        let pending = app.world().resource::<WorldStore>().stats().pending;
        if stats.chunks > 0 && stats.is_settled() && pending == 0 {
            app.update();
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "not settled");
    }
}

/// Ends the app as the game does: the exit save and the store's close (which closes the cache,
/// durably).
fn close(mut app: App) {
    app.shutdown();
}

fn cache_of(app: &App) -> GenCache {
    app.world()
        .resource::<WorldStore>()
        .cache()
        .expect("a cache")
        .clone()
}

/// What a world holds: every loaded chunk's blocks (the whole value: storage and version), its
/// light and its mesh.
type Snapshot = BTreeMap<ChunkPos, (ChunkData, Option<boxel_light::ChunkLight>, Option<u64>)>;

fn snapshot(app: &App) -> Snapshot {
    let map = app.world().resource::<ChunkMap>();
    let meshes: BTreeMap<ChunkPos, u64> = map
        .ready_meshes()
        .map(|(_, k, mesh)| (k.pos, mesh.hash))
        .collect();
    map.iter()
        .filter_map(|(_, slot)| {
            let pos = slot.key().pos;
            let data = map.data_at(pos)?;
            Some((
                pos,
                (
                    (**data).clone(),
                    slot.light().cloned(),
                    meshes.get(&pos).copied(),
                ),
            ))
        })
        .collect()
}

fn tops(app: &App, o: BlockPos) -> Vec<Option<i64>> {
    let sky = app.world().resource::<SkyMap>();
    (-40..=40)
        .step_by(8)
        .flat_map(|dx| (-40..=40).step_by(8).map(move |dz| (dx, dz)))
        .map(|(dx, dz)| sky.top(o.x + dx, o.z + dz))
        .collect()
}

/// The places: near the origin, across ±2^31, at 2^40 and next to the world limit, on the
/// terrain's surface (multiples of 2^14 far out, so the viewer's `f64` position is exact).
fn places() -> [BlockPos; 4] {
    let edge = i64::from(i32::MAX) + 1;
    let limit = MAX_BLOCK_COORD - (1 << 14);
    [
        BlockPos::new(0, 70, 0),
        BlockPos::new(edge - 8, 70, -edge - 8),
        BlockPos::new(1 << 40, 70, -(1 << 40)),
        BlockPos::new(limit, 70, -limit),
    ]
}

#[test]
fn a_world_from_the_cache_is_the_world_from_the_generator() {
    for (i, o) in places().into_iter().enumerate() {
        let dir = TempDir::new(&format!("same-{i}"));
        let plain = Setup::new(None).start(o);
        let expected = snapshot(&plain);
        let expected_tops = tops(&plain, o);
        drop(plain);
        // First visit: a miss for every chunk with content, each written to the file.
        let setup = Setup::new(Some(&dir.0));
        let first = setup.start(o);
        assert_eq!(snapshot(&first), expected, "{o:?}: the first visit");
        let cache = cache_of(&first);
        let stats = cache.stats();
        assert_eq!(stats.chunk_hits, 0);
        assert!(stats.chunk_misses > 0 && stats.offered > 0, "{stats:?}");
        close(first);
        assert!(dir.0.join(CACHE_FILE).exists());
        // Second visit, in a new world of the same terrain: every chunk with content is a hit.
        let second = setup.fresh().start(o);
        let stats = cache_of(&second).stats();
        assert!(stats.chunk_hits > 0, "{o:?}: {stats:?}");
        assert_eq!(snapshot(&second), expected, "{o:?}: from the cache");
        assert_eq!(tops(&second, o), expected_tops, "{o:?}: sky tops");
        close(second);
    }
}

#[test]
fn another_salt_regenerates() {
    let dir = TempDir::new("salt");
    let o = places()[0];
    let setup = Setup::new(Some(&dir.0));
    close(setup.start(o));
    let mut other = setup.fresh();
    other.salt = 8;
    let app = other.start(o);
    let stats = cache_of(&app).stats();
    assert_eq!(stats.chunk_hits, 0, "{stats:?}");
    assert!(stats.chunk_misses > 0);
    let expected = snapshot(&Setup::new(None).start(o));
    assert_eq!(snapshot(&app), expected);
    close(app);
    // The first salt's records were rewritten under the second: the first one misses again.
    let app = setup.fresh().start(o);
    assert_eq!(cache_of(&app).stats().chunk_hits, 0);
}

#[test]
fn an_edit_beats_the_cache_and_never_goes_into_it() {
    for (i, o) in places().into_iter().enumerate() {
        let dir = TempDir::new(&format!("edit-{i}"));
        let setup = Setup::new(Some(&dir.0));
        let stone = setup.registry.by_name("boxel:stone").expect("stone");
        // A visit fills the cache; then an edit, saved on exit.
        close(setup.start(o));
        let mut app = setup.start(o);
        let edits: Vec<BlockPos> = (0..20)
            .map(|y| BlockPos::new(o.x + 3, o.y - 10 + y, o.z + 3))
            .collect();
        {
            let mut queue = app.world_mut().resource_mut::<EditQueue>();
            for &p in &edits {
                queue.set_block(p, stone);
            }
        }
        settle(&mut app);
        close(app);
        // Back in the same world: the saved chunk, not the cached one.
        let app = setup.start(o);
        let map = app.world().resource::<ChunkMap>();
        for &p in &edits {
            assert_eq!(map.block_at(p), Some(stone), "{p:?}");
        }
        close(app);
        // A new world of the same terrain reads the cache: the generator's blocks there.
        let app = setup.fresh().start(o);
        let expected = Setup::new(None).start(o);
        let em = expected.world().resource::<ChunkMap>();
        let map = app.world().resource::<ChunkMap>();
        for &p in &edits {
            assert_eq!(map.block_at(p), em.block_at(p), "{p:?}");
        }
        assert!(cache_of(&app).stats().chunk_hits > 0);
    }
}

/// Water placed at `o` after the world settled, `steps` game ticks of flow, and every water
/// block around.
fn flow(setup: &Setup, o: BlockPos, steps: usize) -> Vec<(BlockPos, BlockState)> {
    let mut setup = setup.clone();
    setup.fluids = true;
    let mut app = setup.start(o);
    let water = setup
        .registry
        .parse_state("boxel:water[level=0]")
        .expect("water");
    let top = app
        .world()
        .resource::<SkyMap>()
        .top(o.x, o.z)
        .expect("walked");
    let at = BlockPos::new(o.x, top + 1, o.z);
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(at, water);
    app.update();
    for _ in 0..steps {
        app.world_mut().resource_mut::<BlockTicks>().advance(1);
        app.update();
    }
    let map = app.world().resource::<ChunkMap>();
    let mut out = Vec::new();
    for x in -12..=12 {
        for z in -12..=12 {
            for y in -6..=2 {
                let p = BlockPos::new(at.x + x, at.y + y, at.z + z);
                if let Some(s) = map.block_at(p)
                    && setup.registry.name(s) == "boxel:water"
                {
                    out.push((p, s));
                }
            }
        }
    }
    close(app);
    out
}

#[test]
fn water_flows_the_same_from_the_cache() {
    for o in [places()[0], places()[2]] {
        let dir = TempDir::new(&format!("water-{}", o.x));
        let plain = flow(&Setup::new(None), o, 30);
        assert!(plain.len() > 5, "the water spread");
        let setup = Setup::new(Some(&dir.0));
        assert_eq!(flow(&setup, o, 30), plain, "first visit");
        assert_eq!(flow(&setup.fresh(), o, 30), plain, "from the cache");
    }
}

/// Grass a random tick lands on turns to stone.
fn harden(cx: &mut TickContext<'_>, pos: BlockPos, _: BlockState) {
    let stone = cx.registry().by_name("boxel:stone").expect("stone");
    cx.set(pos, stone);
}

/// `ticks` game ticks of random ticks around `o` (grass hardens), and the stone that made.
fn random_ticks(setup: &Setup, o: BlockPos, ticks: u64) -> Vec<BlockPos> {
    let mut setup = setup.clone();
    setup.random = true;
    let mut app = setup.start(o);
    let registry = Arc::clone(&setup.registry);
    for _ in 0..ticks {
        app.world_mut().resource_mut::<BlockTicks>().advance(1);
        app.update();
    }
    settle(&mut app);
    let map = app.world().resource::<ChunkMap>();
    let stone = registry.by_name("boxel:stone").expect("stone");
    let terrain = TerrainGenerator::default();
    let mut hardened = Vec::new();
    for (_, slot) in map.iter() {
        let key = slot.key();
        let Some(data) = map.data_at(key.pos) else {
            continue;
        };
        let fresh = terrain.generate(key, &registry);
        if **data != fresh {
            let origin = key.pos.origin();
            for i in 0..boxel_voxel::CHUNK_VOLUME {
                let l = boxel_voxel::LocalPos::from_index(i);
                if data.get(l) == stone && fresh.get(l) != stone {
                    hardened.push(BlockPos::new(
                        origin.x + i64::from(l.x()),
                        origin.y + i64::from(l.y()),
                        origin.z + i64::from(l.z()),
                    ));
                }
            }
        }
    }
    hardened.sort_by_key(|p| (p.x, p.y, p.z));
    close(app);
    hardened
}

#[test]
fn random_ticks_land_the_same_from_the_cache() {
    let o = places()[1];
    let dir = TempDir::new("random");
    let plain = random_ticks(&Setup::new(None), o, 20);
    assert!(!plain.is_empty(), "random ticks hardened some grass");
    let setup = Setup::new(Some(&dir.0));
    assert_eq!(random_ticks(&setup, o, 20), plain, "first visit");
    let again = setup.fresh();
    assert_eq!(random_ticks(&again, o, 20), plain, "from the cache");
}

#[test]
fn a_damaged_cache_file_starts_empty_and_a_world_without_one_opens() {
    let dir = TempDir::new("damaged");
    std::fs::create_dir_all(&dir.0).expect("dir");
    std::fs::write(dir.0.join(CACHE_FILE), b"not a database at all").expect("write");
    let o = places()[0];
    let setup = Setup::new(Some(&dir.0));
    let app = setup.start(o);
    assert_eq!(snapshot(&app), snapshot(&Setup::new(None).start(o)));
    assert!(cache_of(&app).stats().chunk_misses > 0);
    close(app);
    let app = setup.fresh().start(o);
    assert!(cache_of(&app).stats().chunk_hits > 0, "it works after");
    close(app);
    // A world saved without a cache opens with one, and again without.
    let world = Setup::new(None);
    close(world.start(o));
    let mut cached = world.clone();
    cached.cache = Some(dir.0.clone());
    close(cached.start(o));
    close(world.start(o));
}

#[test]
fn a_cache_held_by_another_process_is_refused() {
    let dir = TempDir::new("held");
    let registry = boxel_demo::blocks::registry();
    let held = GenCache::open_dir(&dir.0, &registry, GenCacheSettings::default()).expect("cache");
    assert!(GenCache::open_dir(&dir.0, &registry, GenCacheSettings::default()).is_err());
    held.close();
    assert!(GenCache::open_dir(&dir.0, &registry, GenCacheSettings::default()).is_ok());
}

#[test]
fn a_cache_turned_off_is_neither_read_nor_written() {
    let dir = TempDir::new("off");
    let o = places()[0];
    let setup = Setup::new(Some(&dir.0));
    // Off from the start: nothing offered.
    let generator = TerrainGenerator::default();
    let store = WorldStore::open(
        Arc::new(MemoryStore::new()),
        &setup.registry,
        generator.name(),
    )
    .expect("the world opens");
    let cache = GenCache::open_dir(&dir.0, &setup.registry, GenCacheSettings::default())
        .expect("the cache opens");
    cache.set_enabled(false);
    store.attach_cache(cache.clone()).expect("one cache");
    assert!(
        store.attach_cache(cache.clone()).is_err(),
        "one cache a world"
    );
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(generator),
        registry: Arc::clone(&setup.registry),
        executor: Arc::new(InlineExecutor),
        settings: WorldSettings::default(),
    })
    .add_plugins(PersistPlugin::new(store).with_executor(Arc::new(InlineExecutor)));
    app.world_mut().spawn((
        Transform::from_translation(center(o)),
        ChunkViewer::new(2).with_vertical_radius(1),
    ));
    settle(&mut app);
    let stats = cache.stats();
    assert_eq!(
        (stats.chunk_hits, stats.chunk_misses, stats.offered),
        (0, 0, 0),
        "{stats:?}"
    );
    close(app);
    assert!(!cache.enabled(), "the world's close closed it");
}

/// What a LOD source emits for `req`: the band and each node (its state, or a hash of its
/// cells), in the order emitted.
fn far_column(
    generator: &dyn WorldGenerator,
    req: &boxel_worldgen::lod::LodRequest,
    registry: &BlockRegistry,
    cache: Option<&GenCache>,
) -> (boxel_worldgen::lod::LodColumn, Vec<(i64, Vec<u16>)>) {
    use boxel_worldgen::lod::{LodNode, LodScratch};
    let mut scratch = LodScratch::default();
    let mut nodes = Vec::new();
    let mut emit = |layer: i64, node: LodNode<'_>| {
        let cells = match node {
            LodNode::Uniform(s) => vec![s.0],
            LodNode::Cells(c) => c.0.iter().map(|s| s.0).collect(),
        };
        nodes.push((layer, cells));
    };
    let band = match cache {
        Some(cache) => cache
            .source_column(generator, req, registry, &mut scratch, &mut emit)
            .expect("a source"),
        None => generator
            .lod()
            .expect("a source")
            .column(req, registry, &mut scratch, &mut emit),
    };
    (band, nodes)
}

#[test]
fn far_columns_of_a_real_terrain_come_back_from_the_cache() {
    let registry = boxel_demo::blocks::registry();
    let cache = GenCache::in_memory(&registry, GenCacheSettings::default()).expect("cache");
    let generators: [Box<dyn WorldGenerator>; 2] = [
        Box::new(TerrainGenerator::default()),
        Box::new(boxel_demo::worldgen::ForestGenerator::default()),
    ];
    for generator in &generators {
        for level in 1..=4u8 {
            for x in [-2i64, 0, 5, (1 << 40) >> (5 + level)] {
                let req = boxel_worldgen::lod::LodRequest {
                    dim: boxel_voxel::DimensionId(0),
                    level,
                    column: [x, 1 - x],
                    layers: -2..=6,
                };
                let plain = far_column(&**generator, &req, &registry, None);
                let first = far_column(&**generator, &req, &registry, Some(&cache));
                cache.flush().expect("flush");
                let hits = cache.stats().lod_hits;
                let back = far_column(&**generator, &req, &registry, Some(&cache));
                assert_eq!(cache.stats().lod_hits, hits + 1, "{req:?}");
                assert_eq!(first, plain, "{req:?}: recording");
                assert_eq!(back, plain, "{req:?}: from the cache");
            }
        }
    }
}
