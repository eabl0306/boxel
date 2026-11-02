//! Sky sources in the world (ADR 0061), with the real mesher: a world without sources is the
//! world before them; a lit cavern's meshes equal the from-scratch [`Reference`] and do not
//! depend on the load order, the viewer's path, the executor or the thread count; edits and
//! unloads move beams as a rebuild from scratch would; the same world far from the origin gives
//! the same bits.
use super::*;
use crate::persist::{MemoryStore, Persist, WorldStore};
use crate::tests::support::{LitCavern, Sources, source_registry};
use proptest::prelude::*;
use std::sync::OnceLock;

/// [`golden_digest`] on `main` before sky sources (4eeb410).
const MAIN: u64 = 0x4308_db9a_f635_9758;

/// The meshes of a small overworld view, before and after a few edits (stone, air, leaves,
/// lamps), folded into one number.
fn golden_digest(registry: Arc<BlockRegistry>, lamps: [BlockState; 2]) -> (u64, App) {
    let leaves = registry.by_name("boxel:leaves").expect("leaves");
    let mut app = lit_app(overworld(), registry, small(), Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, 2, 0), 2, 2);
    settle(&mut app);
    let mut digest = 0xcbf2_9ce4_8422_2325_u64;
    let mut fold = |app: &App| {
        for (p, h) in hashes(app) {
            for v in [p.x as u64, p.y as u64, p.z as u64, h.unwrap_or(0)] {
                digest = (digest ^ v).wrapping_mul(0x0100_0000_01b3);
            }
        }
    };
    fold(&app);
    let states = [BlockState::AIR, STONE, leaves, lamps[0], lamps[1]];
    let mut batch = Vec::new();
    for i in 0..40i64 {
        let (x, z) = ((i * 7) % 60 - 30, (i * 13) % 60 - 30);
        let y = surface(x, z) - 6 + i % 9;
        batch.push((BlockPos::new(x, y, z), states[(i % 5) as usize]));
    }
    edit(&mut app, &batch);
    settle(&mut app);
    fold(&app);
    (digest, app)
}

/// Beam jobs, waits, light boxes with beams and columns with a tile.
fn beam_counts(app: &App) -> [usize; 4] {
    let s = stats(app);
    let tiles = app.world().resource::<SkyMap>().beam_stats().tiles;
    [s.beam_jobs, s.waiting_beams, s.light_beam_boxes, tiles]
}

/// Without sources nothing changes: no beam job, no wait, and the meshes `main` gave, bit for
/// bit. A registry with sources that no chunk holds is the same.
#[test]
fn a_world_without_sources_is_the_world_before_them() {
    let (registry, lamps) = lamp_registry();
    let (digest, app) = golden_digest(registry, lamps);
    assert_eq!(digest, MAIN, "{digest:#x}");
    assert_eq!(beam_counts(&app), [0; 4]);
    let (mut b, lamps) = lamp_builder();
    boxel_voxel::testing::add_sky_source(&mut b).expect("sources");
    let (digest, app) = golden_digest(Arc::new(b.build()), lamps);
    assert_eq!(digest, MAIN, "{digest:#x}");
    assert_eq!(beam_counts(&app), [0; 4]);
    let s = stats(&app);
    assert_eq!(s.beam_private_generations, 0);
}

/// Runs held jobs in an order of its seed: a few per call, each picked at random.
struct ShuffleExecutor {
    rng: Mutex<u64>,
    jobs: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl ShuffleExecutor {
    fn new(seed: u64) -> Arc<Self> {
        Arc::new(Self {
            rng: Mutex::new(seed | 1),
            jobs: Mutex::default(),
        })
    }
    /// Runs up to `n` held jobs (the jobs they make the world spawn wait for the next call).
    fn run(&self, n: usize) {
        for _ in 0..n {
            let job = {
                let mut jobs = self.jobs.lock().expect("lock");
                if jobs.is_empty() {
                    return;
                }
                let mut rng = self.rng.lock().expect("lock");
                *rng = rng
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let i = (*rng >> 33) as usize % jobs.len();
                jobs.swap_remove(i)
            };
            job();
        }
    }
}

impl JobExecutor for ShuffleExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        self.jobs.lock().expect("lock").push(job);
    }
}

/// Where the cavern views end: the cavern's middle (layers -5 to -1 meshed).
const CAVERN: ChunkPos = ChunkPos::new(0, -3, 0);

/// The cavern viewed from `CAVERN` after `path`, with `shuffle` running the jobs (when given:
/// it is also `executor`). Returns the meshes and the app.
fn cavern_run(
    executor: Arc<dyn JobExecutor>,
    shuffle: Option<&ShuffleExecutor>,
    path: &[ChunkPos],
) -> (BTreeMap<ChunkPos, Option<u64>>, App) {
    let (registry, _) = source_registry();
    let mut app = lit_app(LitCavern, registry, small(), executor);
    let viewer = spawn(&mut app, path[0], 2, 2);
    let step = |app: &mut App| {
        app.update();
        if let Some(s) = shuffle {
            s.run(6);
        }
    };
    for &p in path {
        move_viewer(&mut app, viewer, p);
        for _ in 0..2 {
            step(&mut app);
        }
    }
    move_viewer(&mut app, viewer, CAVERN);
    let start = Instant::now();
    loop {
        step(&mut app);
        drain_events(&mut app);
        if stats(&app).is_settled() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "not settled");
        std::thread::yield_now();
    }
    (hashes(&app), app)
}

fn fly() -> Vec<ChunkPos> {
    (-4..=0).map(|x| ChunkPos::new(x, -3, 0)).collect()
}

fn dive() -> Vec<ChunkPos> {
    (-3..=4).rev().map(|y| ChunkPos::new(0, y, 0)).collect()
}

fn teleport() -> Vec<ChunkPos> {
    vec![ChunkPos::new(4000, -3, -900)]
}

/// The meshes of the cavern flown into with jobs run at once.
fn cavern_meshes() -> &'static BTreeMap<ChunkPos, Option<u64>> {
    static MESHES: OnceLock<BTreeMap<ChunkPos, Option<u64>>> = OnceLock::new();
    MESHES.get_or_init(|| cavern_run(Arc::new(InlineExecutor), None, &fly()).0)
}

/// The lit cavern equals the reference, its beams light it (15 under a roof source, less in a
/// ledge's shadow), and the same meshes come from every path, executor and thread count.
#[test]
fn a_lit_cavern_is_lit_and_does_not_depend_on_the_path_or_the_threads() {
    let (want, app) = cavern_run(Arc::new(InlineExecutor), None, &fly());
    assert_eq!(&want, cavern_meshes());
    assert!(want.values().filter(|h| h.is_some()).count() > 20);
    assert!(assert_matches_reference(&app) > 20);
    let [jobs, waiting, boxes, tiles] = beam_counts(&app);
    assert!(jobs > 0 && boxes > 0 && tiles > 0, "{jobs} {boxes} {tiles}");
    assert_eq!(waiting, 0);
    // Under a roof source, and on the cavern floor of its column: 15. In the ledge's shadow,
    // spread light only.
    assert_eq!(light_at(&app, BlockPos::new(10, -61, 4)).sky(), 15);
    assert_eq!(light_at(&app, BlockPos::new(10, -149, 4)).sky(), 15);
    let shadow = light_at(&app, BlockPos::new(31, -120, 4)).sky();
    assert!((1..15).contains(&shadow), "{shadow}");
    for (name, executor, path) in [
        (
            "dive",
            Arc::new(InlineExecutor) as Arc<dyn JobExecutor>,
            dive(),
        ),
        ("teleport", Arc::new(InlineExecutor), teleport()),
        ("task pool", Arc::new(TaskPoolExecutor), fly()),
        ("1 thread", PoolExecutor::new(1), dive()),
        ("6 threads", PoolExecutor::new(6), teleport()),
    ] {
        assert!(
            cavern_run(executor, None, &path).0 == want,
            "{name}: meshes differ"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4))]

    /// Jobs finishing in any order (generations, walks, beam builds and meshes mixed), on any
    /// path: the same cavern.
    #[test]
    fn a_lit_cavern_does_not_depend_on_the_load_order(seed in any::<u64>(), path in 0usize..3) {
        let path = [fly(), dive(), teleport()][path].clone();
        let shuffle = ShuffleExecutor::new(seed);
        let (got, _) = cavern_run(shuffle.clone(), Some(&shuffle), &path);
        prop_assert!(&got == cavern_meshes(), "seed {seed}");
    }
}

/// The cavern settled around `CAVERN` with jobs run at once. Streaming never remeshes for
/// beams: each chunk is meshed once, even as the tiles' windows grow.
fn cavern_app() -> (App, Sources) {
    let (registry, sources) = source_registry();
    let mut app = lit_app(LitCavern, registry, small(), Arc::new(InlineExecutor));
    spawn(&mut app, CAVERN, 2, 2);
    settle(&mut app);
    let jobs = mesh_jobs(&app);
    assert!(jobs.values().all(|&n| n == 1), "{jobs:?}");
    assert_eq!(stats(&app).light_fanout, 0);
    (app, sources)
}

#[test]
fn placing_a_source_and_removing_it_equals_never_placing_it() {
    let (mut app, sources) = cavern_app();
    let before = hashes(&app);
    // Under the ledge, in its shadow: the new beam lights the floor under it.
    let p = BlockPos::new(30, -101, 5);
    edit(&mut app, &[(p, sources.down[2])]);
    settle(&mut app);
    assert_ne!(hashes(&app), before);
    assert_eq!(light_at(&app, BlockPos::new(30, -140, 5)).sky(), 15);
    assert_matches_reference(&app);
    edit(&mut app, &[(p, BlockState::AIR)]);
    settle(&mut app);
    assert_eq!(hashes(&app), before);
    assert_matches_reference(&app);
}

#[test]
fn a_blocker_cuts_a_beam_and_removing_the_one_that_ended_it_extends_it() {
    let (mut app, _) = cavern_app();
    let before = hashes(&app);
    // A roof source's beam, cut halfway down.
    let mid = BlockPos::new(10, -100, 4);
    edit(&mut app, &[(mid, STONE)]);
    settle(&mut app);
    assert!(light_at(&app, BlockPos::new(10, -101, 4)).sky() < 15);
    assert_eq!(light_at(&app, BlockPos::new(10, -99, 4)).sky(), 15);
    assert_matches_reference(&app);
    edit(&mut app, &[(mid, BlockState::AIR)]);
    settle(&mut app);
    assert_eq!(hashes(&app), before);
    // The ledge ends the beam of (30, 4) at -97: without it, the beam reaches the floor.
    assert!(light_at(&app, BlockPos::new(30, -120, 4)).sky() < 15);
    let ledge: Vec<_> = (-100..=-98)
        .map(|y| (BlockPos::new(30, y, 4), BlockState::AIR))
        .collect();
    edit(&mut app, &ledge);
    settle(&mut app);
    assert_eq!(light_at(&app, BlockPos::new(30, -120, 4)).sky(), 15);
    assert_matches_reference(&app);
}

/// The meshes of layer `y` around the origin.
fn layer(hashes: &BTreeMap<ChunkPos, Option<u64>>, y: i64) -> BTreeMap<ChunkPos, Option<u64>> {
    hashes
        .iter()
        .filter(|(p, _)| p.y == y)
        .map(|(p, h)| (*p, *h))
        .collect()
}

/// The roof sources (layer -2) two layers above the loaded set (layers -8 to -4, meshes -7 to
/// -5) light layer -5 as they do when they are loaded: the build generates them privately.
#[test]
fn a_source_above_the_loaded_set_lights_as_when_loaded() {
    let (registry, _) = source_registry();
    let mut low = lit_app(LitCavern, registry, small(), Arc::new(InlineExecutor));
    spawn(&mut low, ChunkPos::new(0, -6, 0), 2, 1);
    settle(&mut low);
    let top = low
        .world()
        .resource::<ChunkMap>()
        .iter()
        .map(|(_, s)| s.pos.y)
        .max();
    assert_eq!(top, Some(-4), "the roof sources are two layers above");
    assert!(stats(&low).beam_private_generations > 0);
    assert_matches_reference(&low);
    let (high, _) = cavern_app();
    let (a, b) = (layer(&hashes(&low), -5), layer(&hashes(&high), -5));
    assert!(a.len() > 8 && a.values().any(Option::is_some));
    for (p, h) in &a {
        if let Some(other) = b.get(p) {
            assert_eq!(h, other, "{p:?}");
        }
    }
    assert!(a.keys().filter(|p| b.contains_key(p)).count() > 8);
}

/// A creative source of range 256 under the roof of [`Hall`] (layer -1) lights the pillars of
/// layer -5; then the viewer goes down until layers -1 and -2 unload: straight down without
/// persistence (the column stays loaded), away and back with it (the columns go, and come back
/// built from the store). Returns the meshes of layer -5 with the source loaded, and after it
/// unloaded, and the app.
fn unload_source(persist: bool) -> (Meshes, Meshes, App) {
    let (registry, sources) = source_registry();
    let settings = WorldSettings {
        unload_hysteresis: 0,
        ..small()
    };
    let mut app = lit_app(
        Hall,
        Arc::clone(&registry),
        settings,
        Arc::new(InlineExecutor),
    );
    if persist {
        let store =
            WorldStore::open(Arc::new(MemoryStore::new()), &registry, "hall").expect("opens");
        app.world_mut().resource_mut::<WorldJobs>().persist =
            Some(Persist::new(store, Arc::new(InlineExecutor)));
    }
    let viewer = spawn(&mut app, ChunkPos::new(0, -4, 0), 2, 3);
    settle(&mut app);
    edit(&mut app, &[(BlockPos::new(5, -1, 5), sources.down[3])]);
    settle(&mut app);
    assert_eq!(light_at(&app, BlockPos::new(5, -149, 5)).sky(), 15);
    let loaded = layer(&hashes(&app), -5);
    if persist {
        move_viewer(&mut app, viewer, ChunkPos::new(4000, -7, 0));
        settle(&mut app);
    }
    move_viewer(&mut app, viewer, ChunkPos::new(0, -7, 0));
    settle(&mut app);
    let map = app.world().resource::<ChunkMap>();
    assert!(map.data_at(ChunkPos::new(0, -2, 0)).is_none(), "unloaded");
    let unloaded = layer(&hashes(&app), -5);
    (loaded, unloaded, app)
}

type Meshes = BTreeMap<ChunkPos, Option<u64>>;

/// The meshes of layer -5 of [`Hall`] without the creative source, viewed from layer -7.
fn without_source() -> Meshes {
    let (registry, _) = source_registry();
    let mut app = lit_app(Hall, registry, small(), Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, -7, 0), 2, 3);
    settle(&mut app);
    layer(&hashes(&app), -5)
}

/// With persistence the source is saved when its chunk unloads: the beam builds read it from
/// the store and the meshes under it keep their light.
#[test]
fn a_saved_source_in_an_unloaded_chunk_is_read_through_the_store() {
    let (loaded, unloaded, _) = unload_source(true);
    let plain = without_source();
    assert!(!unloaded.is_empty() && unloaded.keys().eq(loaded.keys()));
    let same = |a: &Meshes, b: &Meshes| a.iter().all(|(p, h)| b.get(p).is_none_or(|o| o == h));
    assert!(!same(&loaded, &plain), "the source lights layer -5");
    assert!(same(&unloaded, &loaded), "read from the store");
}

/// Without persistence the source goes with its chunk's edits: its beam is gone.
#[test]
fn an_unloaded_creative_source_takes_its_beam_with_it() {
    let (loaded, unloaded, app) = unload_source(false);
    let plain = without_source();
    assert!(!unloaded.is_empty() && unloaded.keys().eq(loaded.keys()));
    let differs = loaded
        .iter()
        .any(|(p, h)| plain.get(p).is_some_and(|o| o != h));
    assert!(differs, "the source lit layer -5");
    assert!(
        unloaded
            .iter()
            .all(|(p, h)| plain.get(p).is_none_or(|o| o == h))
    );
    assert_matches_reference(&app);
}

/// Stone at layer 0, air from layer -1 to -9 with a stone pillar every 8 blocks, stone at
/// layer -10: a tall dark hall with no source of its own.
struct Hall;

impl WorldGenerator for Hall {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let stone = registry.by_name("boxel:stone").expect("stone");
        match key.pos.y {
            0 | -10 => ChunkData::uniform(stone),
            -9..=-1 => {
                let dense: Vec<BlockState> = (0..boxel_voxel::CHUNK_VOLUME)
                    .map(|i| {
                        let p = LocalPos::from_index(i);
                        if p.x().is_multiple_of(8) && p.z().is_multiple_of(8) {
                            stone
                        } else {
                            BlockState::AIR
                        }
                    })
                    .collect();
                ChunkData::from_dense(&dense)
            }
            _ => ChunkData::uniform(BlockState::AIR),
        }
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        -10..=0
    }
    fn name(&self) -> &str {
        "hall"
    }
}

/// The first source of a world (the generator places none) gets a tile in the edit's batch; a
/// 256-cell beam's remeshes are published in one frame (or the batch counts as split); removing
/// it gives back the dark hall.
#[test]
fn a_full_beam_edit_publishes_in_one_batch() {
    let (registry, sources) = source_registry();
    let mut app = lit_app(Hall, registry, small(), Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, -5, 0), 1, 5);
    settle(&mut app);
    assert_eq!(beam_counts(&app), [0; 4], "no source: no tile");
    let before = hashes(&app);
    let split = stats(&app).edit_batches_split;
    let p = BlockPos::new(5, -1, 5);
    edit(&mut app, &[(p, sources.down[3])]);
    let start = Instant::now();
    let mut frames = Vec::new();
    loop {
        app.update();
        let upserts = drain_events(&mut app)
            .iter()
            .filter(|e| matches!(e, ChunkMeshEvent::Upsert { .. }))
            .count();
        if upserts > 0 {
            frames.push(upserts);
        }
        if stats(&app).is_settled() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "not settled");
    }
    let s = stats(&app);
    assert!(
        frames.len() == 1 || s.edit_batches_split > split,
        "published over {frames:?}"
    );
    // The beam spans layers -1 to -8: the pillars around it in 8 layers or more changed.
    assert!(frames.iter().sum::<usize>() >= 8, "{frames:?}");
    assert_eq!(light_at(&app, BlockPos::new(5, -256, 5)).sky(), 15);
    assert_eq!(light_at(&app, BlockPos::new(5, -257, 5)).sky(), 14);
    assert_eq!(beam_counts(&app)[1], 0);
    assert_matches_reference(&app);
    edit(&mut app, &[(p, BlockState::AIR)]);
    settle(&mut app);
    assert_eq!(hashes(&app), before);
}

/// The cavern at x = 2^40 and y = -2^40 (and an edit there) gives the meshes it gives at the
/// origin.
#[test]
fn a_lit_cavern_far_from_the_origin_gives_the_same_bits() {
    let shift = ChunkPos::new(1 << 35, -(1 << 35), 0);
    let run = |offset: ChunkPos| {
        let (registry, sources) = source_registry();
        let generator = Offset {
            inner: LitCavern,
            offset,
        };
        let mut app = lit_app(generator, registry, small(), Arc::new(InlineExecutor));
        let at = |p: ChunkPos| ChunkPos::new(p.x + offset.x, p.y + offset.y, p.z + offset.z);
        spawn(&mut app, at(CAVERN), 2, 2);
        settle(&mut app);
        let o = offset.origin();
        let first = hashes(&app);
        edit(
            &mut app,
            &[(BlockPos::new(o.x + 30, o.y - 101, o.z + 5), sources.down[2])],
        );
        settle(&mut app);
        let relative = |h: BTreeMap<ChunkPos, Option<u64>>| -> Vec<_> {
            h.into_iter()
                .map(|(p, h)| {
                    (
                        ChunkPos::new(p.x - offset.x, p.y - offset.y, p.z - offset.z),
                        h,
                    )
                })
                .collect()
        };
        (relative(first), relative(hashes(&app)))
    };
    let near = run(ChunkPos::new(0, 0, 0));
    assert!(near.0 != near.1, "the edit lights the cavern");
    assert!(run(shift) == near);
}
