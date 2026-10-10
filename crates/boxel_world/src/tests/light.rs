//! Light in the world (ADR 0015, phase P3), with the real mesher: every settled mesh equals the
//! from-scratch [`Reference`], whatever the path of the viewer, the executor or the thread
//! count; streaming never remeshes for light; edits remesh exactly what their light reaches.
use super::{chunk_center, drain_events, move_viewer};
use crate::jobs::{InlineExecutor, JobExecutor, TaskPoolExecutor};
use crate::reference::Reference;
use crate::streaming::WorldJobs;
use crate::tests::support::LayerGen;
use crate::{
    BlockChanged, ChunkMap, ChunkMeshEvent, ChunkViewer, EditQueue, Interest, MeshState, SkyMap,
    WorldContent, WorldPlugin, WorldSettings, WorldStats, WorldSystems,
};
use boxel_app::{App, Update};
use boxel_demo::worldgen::OverworldGenerator;
use boxel_ecs::prelude::*;
use boxel_mesh::MeshOptions;
use boxel_tasks::{TaskPool, TaskPoolBuilder};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockCollision, BlockDef, BlockModel, BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey,
    ChunkPos, FaceTextures, LightValue, LocalPos, RenderShape, TextureId,
};
use boxel_worldgen::WorldGenerator;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The default blocks plus two opaque lamps (orange, blue).
fn lamp_registry() -> (Arc<BlockRegistry>, [BlockState; 2]) {
    let mut b = boxel_demo::blocks::builder();
    let mut lamp = |name: &str, emission| {
        b.add_block(BlockDef {
            name: format!("test:{name}"),
            shape: RenderShape::Opaque,
            model: BlockModel::CUBE,
            collision: BlockCollision::Full,
            emission,
            light_opacity: 15,
            textures: FaceTextures::all(TextureId(0)),
        })
        .expect("lamp")
    };
    let lamps = [
        lamp("lamp_orange", [14, 11, 6]),
        lamp("lamp_blue", [4, 6, 14]),
    ];
    (Arc::new(b.build()), lamps)
}

const STONE: BlockState = BlockState(1);

/// A world with light and the real mesher.
fn lit_app(
    generator: impl WorldGenerator,
    registry: Arc<BlockRegistry>,
    settings: WorldSettings,
    executor: Arc<dyn JobExecutor>,
) -> App {
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        generator: Arc::new(generator),
        registry,
        settings,
        executor,
    });
    app
}

fn spawn(app: &mut App, chunk: ChunkPos, radius: u32, vertical: u32) -> Entity {
    app.world_mut()
        .spawn((
            Transform::from_translation(chunk_center(chunk)),
            ChunkViewer {
                radius,
                vertical_radius: vertical,
            },
        ))
        .id()
}

/// Updates until settled (60 s at most, for the pool executors); returns the mesh events.
fn settle(app: &mut App) -> Vec<ChunkMeshEvent> {
    let start = Instant::now();
    let mut events = Vec::new();
    loop {
        app.update();
        events.extend(drain_events(app));
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            return events;
        }
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "not settled in 60 s: {stats:?}"
        );
        std::thread::yield_now();
    }
}

fn edit(app: &mut App, edits: &[(BlockPos, BlockState)]) {
    let mut queue = app.world_mut().resource_mut::<EditQueue>();
    for &(p, s) in edits {
        queue.set_block(p, s);
    }
}

/// The mesh of every chunk with mesh interest: its hash, `None` if empty.
fn hashes(app: &App) -> BTreeMap<ChunkPos, Option<u64>> {
    let map = app.world().resource::<ChunkMap>();
    map.iter()
        .filter(|(_, s)| s.interest.contains(Interest::MESH))
        .map(|(_, s)| {
            let hash = match s.mesh() {
                MeshState::Ready { mesh, .. } => Some(mesh.hash),
                MeshState::Empty => None,
                other => panic!("{:?}: not settled: {other:?}", s.pos),
            };
            (s.pos, hash)
        })
        .collect()
}

/// Every settled mesh equals the from-scratch reference. Returns how many were compared.
fn assert_matches_reference(app: &App) -> usize {
    let map = app.world().resource::<ChunkMap>();
    let sky = app.world().resource::<SkyMap>();
    let content = app.world().resource::<WorldContent>();
    let options = MeshOptions::with_ao(app.world().resource::<WorldSettings>().ao_policy);
    let mut reference = Reference::new(map, sky, content);
    let mut compared = 0;
    for (_, slot) in map.iter() {
        if !slot.interest.contains(Interest::MESH)
            || map.data_at(slot.pos).is_none()
            || !matches!(slot.mesh(), MeshState::Ready { .. } | MeshState::Empty)
        {
            continue;
        }
        let direct = reference.mesh(slot.pos, options);
        match slot.mesh() {
            MeshState::Ready { mesh, .. } => {
                assert_eq!(**mesh, direct, "{:?}: streamed == from scratch", slot.pos);
            }
            _ => assert!(direct.is_empty(), "{:?}: Empty, not from scratch", slot.pos),
        }
        compared += 1;
    }
    compared
}

/// Mesh jobs started per chunk so far.
fn mesh_jobs(app: &App) -> HashMap<ChunkPos, u32> {
    app.world().resource::<WorldJobs>().mesh_jobs.clone()
}

/// The chunks with more mesh jobs in `after` than in `before`.
fn remeshed(before: &HashMap<ChunkPos, u32>, after: &HashMap<ChunkPos, u32>) -> BTreeSet<ChunkPos> {
    after
        .iter()
        .filter(|(p, n)| before.get(p).copied().unwrap_or(0) < **n)
        .map(|(p, _)| *p)
        .collect()
}

fn stats(app: &App) -> WorldStats {
    *app.world().resource::<WorldStats>()
}

fn light_at(app: &App, p: BlockPos) -> LightValue {
    app.world()
        .resource::<ChunkMap>()
        .light_at(p)
        .unwrap_or_else(|| panic!("no cached light at {p:?}"))
}

/// Runs jobs on a pool of its own (a thread count of the test's choice).
struct PoolExecutor(TaskPool);

impl PoolExecutor {
    fn new(threads: usize) -> Arc<Self> {
        Arc::new(Self(TaskPoolBuilder::new().num_threads(threads).build()))
    }
}

impl JobExecutor for PoolExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        self.0.spawn(async move { job() }).detach();
    }
}

/// Runs jobs at once, or holds them while `hold` is set (then `run` releases some).
#[derive(Default)]
struct SwitchExecutor {
    hold: AtomicBool,
    jobs: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl SwitchExecutor {
    /// Runs the first `n` held jobs.
    fn run(&self, n: usize) {
        let jobs: Vec<_> = {
            let mut held = self.jobs.lock().expect("lock");
            let n = n.min(held.len());
            held.drain(..n).collect()
        };
        for job in jobs {
            job();
        }
    }
    fn held(&self) -> usize {
        self.jobs.lock().expect("lock").len()
    }
}

impl JobExecutor for SwitchExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        if self.hold.load(Ordering::Relaxed) {
            self.jobs.lock().expect("lock").push(job);
        } else {
            job();
        }
    }
}

/// `inner` moved by `offset` chunks: the same world somewhere else.
struct Offset<G> {
    inner: G,
    offset: ChunkPos,
}

impl<G: WorldGenerator> WorldGenerator for Offset<G> {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let p = key.pos;
        let o = self.offset;
        let pos = ChunkPos::new(p.x - o.x, p.y - o.y, p.z - o.z);
        self.inner.generate(ChunkKey { pos, ..key }, registry)
    }
    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        let r = self.inner.vertical_chunks();
        r.start().saturating_add(self.offset.y)..=r.end().saturating_add(self.offset.y)
    }
    fn name(&self) -> &str {
        "offset"
    }
}

fn overworld() -> OverworldGenerator {
    OverworldGenerator::default()
}

/// Settings of the small overworld views of these tests.
fn small() -> WorldSettings {
    WorldSettings {
        max_jobs_in_flight: 64,
        interest_budget: Duration::MAX,
        job_budget: Duration::MAX,
        ..WorldSettings::default()
    }
}

/// The surface layer of the overworld at column `(x, z)`.
fn surface(x: i64, z: i64) -> i64 {
    overworld().height_at(x, z)
}

#[test]
fn meshes_do_not_depend_on_the_path_the_executor_or_the_threads() {
    let target = ChunkPos::new(2, 2, -1);
    let run = |executor: Arc<dyn JobExecutor>, path: &[ChunkPos]| {
        let (registry, _) = lamp_registry();
        let mut app = lit_app(overworld(), registry, small(), executor);
        let viewer = spawn(&mut app, path[0], 2, 2);
        for &p in path {
            move_viewer(&mut app, viewer, p);
            for _ in 0..2 {
                app.update();
            }
        }
        move_viewer(&mut app, viewer, target);
        settle(&mut app);
        (hashes(&app), app)
    };
    let fly: Vec<ChunkPos> = (-4..=2).map(|x| ChunkPos::new(x, 2, -1)).collect();
    let dive: Vec<ChunkPos> = (2..=9).rev().map(|y| ChunkPos::new(2, y, -1)).collect();
    let teleport = [ChunkPos::new(4000, 2, -900)];
    let (want, app) = run(Arc::new(InlineExecutor), &fly);
    assert!(want.values().filter(|h| h.is_some()).count() > 20);
    assert!(assert_matches_reference(&app) > 20);
    let s = stats(&app);
    assert!(s.light_full > 0 && s.sky_walks > 0, "{s:?}");
    for (name, executor, path) in [
        (
            "dive",
            Arc::new(InlineExecutor) as Arc<dyn JobExecutor>,
            &dive[..],
        ),
        ("teleport", Arc::new(InlineExecutor), &teleport[..]),
        ("task pool", Arc::new(TaskPoolExecutor), &fly[..]),
        ("1 thread", PoolExecutor::new(1), &dive[..]),
        ("6 threads", PoolExecutor::new(6), &teleport[..]),
    ] {
        assert!(run(executor, path).0 == want, "{name}: meshes differ");
    }
}

/// Streaming never remeshes for light: light depends on the content only, and every mesh job
/// waits for the sky tiles it needs.
#[test]
fn a_world_at_rest_meshes_each_chunk_once() {
    for executor in [
        Arc::new(InlineExecutor) as Arc<dyn JobExecutor>,
        PoolExecutor::new(6),
    ] {
        let (registry, _) = lamp_registry();
        let mut app = lit_app(overworld(), registry, small(), executor);
        spawn(&mut app, ChunkPos::new(0, 2, 0), 3, 3);
        settle(&mut app);
        let jobs = mesh_jobs(&app);
        assert!(jobs.len() > 50);
        assert!(jobs.values().all(|&n| n == 1), "{jobs:?}");
        let s = stats(&app);
        assert_eq!(s.light_full + s.light_fast, jobs.len());
        assert_eq!((s.edit_batches_held, s.light_fanout), (0, 0));
    }
}

/// Random edits (stone, air, leaves, lamps), some batches applied before the previous ones
/// settled: afterwards every mesh equals the from-scratch rebuild, and every chunk whose mesh
/// changed was remeshed by the edits.
#[test]
fn random_edits_leave_every_mesh_as_a_rebuild_from_scratch() {
    let (registry, lamps) = lamp_registry();
    let leaves = registry.by_name("boxel:leaves").expect("leaves");
    let mut app = lit_app(overworld(), registry, small(), Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, 2, 0), 2, 2);
    settle(&mut app);
    let before_hashes = hashes(&app);
    let before_jobs = mesh_jobs(&app);
    let mut rng = 0x5eed_u64;
    let mut next = |n: u64| {
        rng = rng
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (rng >> 33) % n
    };
    let states = [BlockState::AIR, STONE, leaves, lamps[0], lamps[1]];
    for round in 0..6 {
        let mut batch = Vec::new();
        for _ in 0..12 {
            let (x, z) = (next(80) as i64 - 40, next(80) as i64 - 40);
            let y = surface(x, z) - 12 + next(20) as i64;
            batch.push((BlockPos::new(x, y, z), states[next(5) as usize]));
        }
        edit(&mut app, &batch);
        if round % 2 == 0 {
            app.update();
        } else {
            settle(&mut app);
        }
    }
    settle(&mut app);
    assert!(assert_matches_reference(&app) > 20);
    let after = hashes(&app);
    let changed: BTreeSet<ChunkPos> = after
        .iter()
        .filter(|(p, h)| before_hashes.get(p) != Some(h))
        .map(|(p, _)| *p)
        .collect();
    assert!(!changed.is_empty());
    let remeshed = remeshed(&before_jobs, &mesh_jobs(&app));
    assert!(
        changed.is_subset(&remeshed),
        "{changed:?} not in {remeshed:?}"
    );
    assert!(stats(&app).light_fanout > 0);
}

#[test]
fn removing_a_lamp_equals_never_placing_it() {
    let (registry, lamps) = lamp_registry();
    let mut app = lit_app(overworld(), registry, small(), Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, 2, 0), 2, 2);
    settle(&mut app);
    let before = hashes(&app);
    let p = BlockPos::new(9, surface(9, 7) + 2, 7);
    edit(&mut app, &[(p, lamps[1])]);
    settle(&mut app);
    assert_ne!(hashes(&app), before, "the lamp lights its surroundings");
    let lit = light_at(&app, BlockPos::new(p.x + 1, p.y, p.z));
    assert_eq!(lit.rgb(), [3, 5, 13], "blue lamp light one voxel away");
    assert_matches_reference(&app);
    edit(&mut app, &[(p, BlockState::AIR)]);
    settle(&mut app);
    assert_eq!(hashes(&app), before);
    assert_eq!(
        light_at(&app, BlockPos::new(p.x + 1, p.y, p.z)).rgb(),
        [0; 3]
    );
}

/// Deep in the overworld every box is under the walk floor: no sky tile at all, dark light,
/// and digging in the dark remeshes only the chunks whose padded region holds the voxel.
#[test]
fn digging_in_a_dark_cave_remeshes_only_the_ao_set() {
    let (registry, _) = lamp_registry();
    let mut app = lit_app(overworld(), registry, small(), Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, -32, 0), 2, 2);
    settle(&mut app);
    let s = stats(&app);
    assert_eq!((s.sky_walks, s.light_full), (0, 0), "{s:?}");
    assert!(s.light_fast > 0);
    // A stone voxel next to cave air, in a chunk meshed with its light, away from the chunk's
    // border (its 6 neighbors read the same light).
    let map = app.world().resource::<ChunkMap>();
    let p = map
        .iter()
        .filter(|(_, s)| s.light().is_some() && s.interest.contains(Interest::MESH))
        .find_map(|(_, s)| {
            let data = map.data_at(s.pos)?;
            (1..31u32).find_map(|y| {
                (1..31u32).find_map(|x| {
                    (1..31u32).find_map(|z| {
                        let air_above = data.get(LocalPos::new(x, y + 1, z)) == BlockState::AIR;
                        (data.get(LocalPos::new(x, y, z)) == STONE && air_above)
                            .then(|| BlockPos::from_parts(s.pos, LocalPos::new(x, y, z)))
                    })
                })
            })
        })
        .expect("a cave floor");
    assert_eq!(light_at(&app, p), LightValue::DARK);
    let before = mesh_jobs(&app);
    edit(&mut app, &[(p, BlockState::AIR)]);
    settle(&mut app);
    let (chunk, _) = p.split();
    let remeshed = remeshed(&before, &mesh_jobs(&app));
    assert_eq!(
        remeshed,
        BTreeSet::from([chunk]),
        "an inner voxel: its chunk only"
    );
    assert_eq!(stats(&app).light_fanout, 0);
    assert_matches_reference(&app);
}

/// A shaft dug through the stone gets direct sky down to its bottom; a cap makes it dark again.
#[test]
fn a_shaft_is_lit_to_its_bottom_and_dark_once_capped() {
    let (registry, _) = lamp_registry();
    let generator = LayerGen::new(-3..=1);
    let settings = WorldSettings {
        vertical_bounds: Some(generator.vertical.clone()),
        ..small()
    };
    let mut app = lit_app(generator, registry, settings, Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, 0, 0), 1, 8);
    settle(&mut app);
    let bottom = BlockPos::new(5, -40, 6);
    let shaft: Vec<(BlockPos, BlockState)> = (-40..=-1)
        .map(|y| (BlockPos::new(5, y, 6), BlockState::AIR))
        .collect();
    edit(&mut app, &shaft);
    settle(&mut app);
    let sky = app.world().resource::<SkyMap>();
    assert_eq!(sky.top(5, 6), Some(-41));
    assert_eq!(light_at(&app, bottom).sky(), 15);
    assert_eq!(
        light_at(&app, BlockPos::new(4, -40, 6)),
        LightValue::DARK,
        "stone"
    );
    assert_matches_reference(&app);
    edit(&mut app, &[(BlockPos::new(5, -1, 6), STONE)]);
    settle(&mut app);
    assert_eq!(app.world().resource::<SkyMap>().top(5, 6), Some(-1));
    assert_eq!(light_at(&app, bottom), LightValue::DARK);
    assert_eq!(light_at(&app, BlockPos::new(5, -2, 6)), LightValue::DARK);
    assert_matches_reference(&app);
}

/// Every `BlockChanged` read in `Update` after the world's systems.
#[derive(Resource, Default)]
struct ChangeLog(Vec<BlockChanged>);

fn log_changes(mut changes: MessageReader<BlockChanged>, mut log: ResMut<ChangeLog>) {
    log.0.extend(changes.read().copied());
}

/// `BlockChanged` comes from the edits' per-voxel change list, the one that sizes the light's
/// remeshes and moves the sky tops (ADR 0015, ADR 0016): one message per position that
/// changed, from its state before the frame's first edit to after its last, in the order of
/// the first edits whatever the chunks; the light of the result matches a rebuild from scratch.
#[test]
fn block_changed_comes_from_the_light_change_list() {
    let (registry, lamps) = lamp_registry();
    let generator = LayerGen::new(-3..=1);
    let settings = WorldSettings {
        vertical_bounds: Some(generator.vertical.clone()),
        ..small()
    };
    let mut app = lit_app(generator, registry, settings, Arc::new(InlineExecutor));
    app.init_resource::<ChangeLog>()
        .add_systems(Update, log_changes.after(WorldSystems::Collect));
    spawn(&mut app, ChunkPos::new(0, 0, 0), 1, 8);
    settle(&mut app);
    app.world_mut().resource_mut::<ChangeLog>().0.clear();
    // `d`: first edited with what it holds, then stone. `a`: the stone at the sky top, dug (the
    // top moves down) then a lamp (back up). `b`: stone over the ground in another chunk (the
    // top moves up). `c`: a lamp in the stone of a third chunk, then stone again (no change).
    // `e`: a lamp on the ground.
    let (a, b, c, d, e) = (
        BlockPos::new(5, -1, 6),
        BlockPos::new(40, 3, 1),
        BlockPos::new(-3, -5, 2),
        BlockPos::new(7, 0, 7),
        BlockPos::new(8, 0, 8),
    );
    edit(
        &mut app,
        &[
            (d, BlockState::AIR),
            (a, BlockState::AIR),
            (b, STONE),
            (c, lamps[0]),
            (a, lamps[0]),
            (d, STONE),
            (c, STONE),
            (e, lamps[1]),
        ],
    );
    app.update();
    let change = |pos, old, new| BlockChanged { pos, old, new };
    assert_eq!(
        app.world().resource::<ChangeLog>().0,
        [
            change(d, BlockState::AIR, STONE),
            change(a, STONE, lamps[0]),
            change(b, BlockState::AIR, STONE),
            change(e, BlockState::AIR, lamps[1]),
        ]
    );
    settle(&mut app);
    let map = app.world().resource::<ChunkMap>();
    assert_eq!(
        [a, b, c, d, e].map(|p| map.block_at(p)),
        [lamps[0], STONE, STONE, STONE, lamps[1]].map(Some)
    );
    let sky = app.world().resource::<SkyMap>();
    assert_eq!((sky.top(5, 6), sky.top(40, 1)), (Some(-1), Some(3)));
    // One voxel from the blue lamp (4, 6, 14) and seven from the orange one (14, 11, 6).
    assert_eq!(light_at(&app, BlockPos::new(9, 0, 8)).rgb(), [7, 5, 13]);
    assert_matches_reference(&app);
}

/// The walk limit: a shaft deeper than it gets no direct sky under the floor.
#[test]
fn the_walk_limit_caps_deep_shafts() {
    let (registry, _) = lamp_registry();
    let generator = LayerGen::new(-3..=1);
    // Top voxel 63: the floor is at -37.
    let settings = WorldSettings {
        vertical_bounds: Some(generator.vertical.clone()),
        sky_walk_limit: 100,
        ..small()
    };
    let mut app = lit_app(generator, registry, settings, Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, 0, 0), 1, 8);
    settle(&mut app);
    assert_eq!(app.world().resource::<SkyMap>().floor(), Some(-37));
    let shaft: Vec<(BlockPos, BlockState)> = (-60..=-1)
        .map(|y| (BlockPos::new(5, y, 6), BlockState::AIR))
        .collect();
    edit(&mut app, &shaft);
    settle(&mut app);
    assert_eq!(app.world().resource::<SkyMap>().top(5, 6), Some(-37));
    let sky = |y| light_at(&app, BlockPos::new(5, y, 6)).sky();
    assert_eq!((sky(-36), sky(-37), sky(-40), sky(-60)), (15, 14, 11, 0));
    assert_matches_reference(&app);
}

/// Under a bounded generator a column can be open forever (`i64::MIN`); content under the
/// walk floor closes the open columns of its tile at the floor instead (a fresh walk stops
/// there). Edits keep the tops equal to a fresh walk in both directions.
#[test]
fn content_under_the_floor_closes_open_columns_at_the_floor() {
    let (registry, _) = lamp_registry();
    // Stone in layer -1 only; top voxel 31, floor -69: the void is reachable.
    let settings = WorldSettings {
        sky_walk_limit: 100,
        ..small()
    };
    let mut app = lit_app(
        LayerGen::new(-1..=0),
        registry,
        settings,
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, ChunkPos::new(0, -2, 0), 1, 2);
    settle(&mut app);
    let top = |app: &App| app.world().resource::<SkyMap>().top(5, 6);
    assert_eq!(top(&app), Some(-1));
    let hole: Vec<(BlockPos, BlockState)> = (-32..=-1)
        .map(|y| (BlockPos::new(5, y, 6), BlockState::AIR))
        .collect();
    edit(&mut app, &hole);
    settle(&mut app);
    assert_eq!(top(&app), Some(i64::MIN), "open through the void");
    assert_eq!(light_at(&app, BlockPos::new(5, -32, 6)).sky(), 15);
    assert_matches_reference(&app);
    // Two stones under the floor (the second one gives layer -3 a mesh, so its light is
    // cached).
    let under = [BlockPos::new(5, -100, 6), BlockPos::new(25, -80, 25)];
    edit(&mut app, &under.map(|p| (p, STONE)));
    settle(&mut app);
    assert_eq!(top(&app), Some(-69), "closed at the floor");
    let sky = |y| light_at(&app, BlockPos::new(5, y, 6)).sky();
    assert_eq!((sky(-68), sky(-69), sky(-70)), (15, 14, 13));
    assert_matches_reference(&app);
    edit(&mut app, &under.map(|p| (p, BlockState::AIR)));
    settle(&mut app);
    assert_eq!(top(&app), Some(i64::MIN), "open again");
    assert_matches_reference(&app);
}

/// A platform 5000 voxels up (kept loaded by a second viewer) shadows the ground under it.
#[test]
fn a_platform_far_above_shadows_the_ground() {
    let (registry, _) = lamp_registry();
    let mut app = lit_app(
        LayerGen::new(-3..=1),
        registry,
        small(),
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, ChunkPos::new(0, 0, 0), 1, 1);
    spawn(&mut app, ChunkPos::new(0, 5000 >> 5, 0), 1, 1);
    settle(&mut app);
    // A stone on the ground away from the platform: the air chunk over the ground gets a mesh
    // job, so its light is cached.
    edit(&mut app, &[(BlockPos::new(20, 0, 20), STONE)]);
    settle(&mut app);
    let ground = BlockPos::new(0, 0, 0);
    assert_eq!(light_at(&app, ground).sky(), 15);
    let mut platform = Vec::new();
    for x in -10..=10 {
        for z in -10..=10 {
            platform.push((BlockPos::new(x, 5000, z), STONE));
        }
    }
    edit(&mut app, &platform);
    settle(&mut app);
    assert_eq!(app.world().resource::<SkyMap>().top(0, 0), Some(5000));
    // The nearest direct sky is 11 columns away.
    assert_eq!(light_at(&app, ground).sky(), 4);
    assert_eq!(light_at(&app, BlockPos::new(0, 5001, 0)).sky(), 15);
    assert!(assert_matches_reference(&app) > 0);
}

/// The platform's chunk unloads (no persistence here: its edits are lost): the columns are
/// walked again, their tops go back down and the ground under them is lit again.
#[test]
fn an_unloaded_platform_takes_its_shadow_with_it() {
    let (registry, _) = lamp_registry();
    let mut app = lit_app(
        LayerGen::new(-3..=1),
        registry,
        small(),
        Arc::new(InlineExecutor),
    );
    spawn(&mut app, ChunkPos::new(0, 0, 0), 1, 1);
    let high = spawn(&mut app, ChunkPos::new(0, 6, 0), 1, 0);
    settle(&mut app);
    let mut edits = vec![(BlockPos::new(20, 0, 20), STONE)];
    for x in -10..=10 {
        for z in -10..=10 {
            edits.push((BlockPos::new(x, 200, z), STONE));
        }
    }
    edit(&mut app, &edits);
    settle(&mut app);
    let ground = BlockPos::new(0, 0, 0);
    assert_eq!(light_at(&app, ground).sky(), 4);
    let before = stats(&app);
    app.world_mut().despawn(high);
    settle(&mut app);
    let map = app.world().resource::<ChunkMap>();
    assert!(
        map.id_at(ChunkPos::new(0, 6, 0)).is_none(),
        "the platform unloaded"
    );
    assert!(
        stats(&app).sky_walks >= before.sky_walks + 4,
        "4 columns walked again"
    );
    assert_eq!(app.world().resource::<SkyMap>().top(0, 0), Some(-1));
    assert_eq!(light_at(&app, ground).sky(), 15);
    assert!(assert_matches_reference(&app) > 0);
}

/// A world of stone layers under air with a lamp-ready corner, its jobs held on demand.
fn corner_world(hold: Duration) -> (App, Arc<SwitchExecutor>, [BlockState; 2]) {
    let (registry, lamps) = lamp_registry();
    let executor = Arc::new(SwitchExecutor::default());
    let generator = LayerGen::new(-3..=1);
    let settings = WorldSettings {
        vertical_bounds: Some(generator.vertical.clone()),
        edit_batch_hold: hold,
        ..small()
    };
    let mut app = lit_app(generator, registry, settings, executor.clone());
    spawn(&mut app, ChunkPos::new(0, 0, 0), 2, 8);
    settle(&mut app);
    (app, executor, lamps)
}

/// From now on the world's frames see the time `at` (a manual clock) instead of the real one.
fn set_clock(app: &mut App, at: Instant) {
    app.world_mut().resource_mut::<WorldJobs>().manual_clock = Some(at);
}

/// A lamp on the stone at a chunk corner lights the top faces of 4 chunks.
const CORNER: BlockPos = BlockPos::new(31, -1, 31);

/// The meshes of one edit go out in the same frame, even when their jobs finish in different
/// frames. Results held in frame `N` of time `t` wait for the rest until the first frame that is
/// both at `t + hold` or later and 2 frames after `N`, and go out then at the latest, the rest on
/// their own (the batch is split). The hold is a time, so very short frames (no frame cap) do
/// not split a batch whose jobs take a few of them, and the 2 frames are a floor, so long frames
/// (60 Hz and less) still give a late job two more job steps.
#[test]
fn an_edit_batch_is_published_in_one_frame() {
    let ms = Duration::from_millis;
    // (hold, frame interval, wait, split): 2 results arrive in the first frame, the other 2
    // `wait` frames later. `split`: the frame (from the first) that publishes the first 2.
    let cases = [
        // About 100 fps: the rest within the hold, then late (held 20 ms >= 16 at frame 2).
        (ms(16), ms(10), 2, None),
        (ms(16), ms(10), 3, Some(2)),
        // 144 Hz: two frames are 13.9 ms, so the third publishes (20.8 ms).
        (ms(16), Duration::from_micros(6944), 3, None),
        (ms(16), Duration::from_micros(6944), 4, Some(3)),
        // 1000 fps: 10 frames are still within 16 ms (2 frames were 2 ms).
        (ms(16), ms(1), 10, None),
        (ms(16), ms(1), 20, Some(16)),
        // 60 Hz and 30 Hz: the hold runs out in one frame, the floor waits for a second one.
        (ms(16), ms(17), 2, None),
        (ms(16), ms(17), 3, Some(2)),
        (ms(16), ms(33), 2, None),
        (ms(16), ms(33), 3, Some(2)),
        // A frame longer than a short hold: the floor too.
        (ms(5), ms(33), 2, None),
        (ms(5), ms(33), 3, Some(2)),
    ];
    for (hold, interval, wait, split) in cases {
        let (mut app, executor, lamps) = corner_world(hold);
        let mut now = Instant::now();
        set_clock(&mut app, now);
        executor.hold.store(true, Ordering::Relaxed);
        edit(&mut app, &[(CORNER, lamps[0])]);
        app.update();
        assert!(drain_events(&mut app).is_empty());
        // The 4 stone chunks around the corner first (priority 0), then the light jobs of the 4
        // air chunks above (their light changed, and they have no mesh job).
        assert_eq!(executor.held(), 8);
        executor.run(2);
        let mut frames = Vec::new();
        for _ in 0..wait {
            now += interval;
            set_clock(&mut app, now);
            app.update();
            frames.push(drain_events(&mut app).len());
        }
        executor.run(usize::MAX);
        now += interval;
        set_clock(&mut app, now);
        app.update();
        frames.push(drain_events(&mut app).len());
        let mut want = vec![0; wait + 1];
        match split {
            Some(first) => {
                want[first] = 2;
                want[wait] = 2;
            }
            None => want[wait] = 4,
        }
        let what = format!("hold {hold:?}, frames {interval:?} apart, the rest after {wait}");
        assert_eq!(frames, want, "{what}");
        let s = stats(&app);
        assert_eq!(s.edit_batches_held, 1, "{what}");
        assert_eq!(s.edit_batches_split, usize::from(split.is_some()), "{what}");
        executor.hold.store(false, Ordering::Relaxed);
        settle(&mut app);
        assert_matches_reference(&app);
    }
}

/// With no hold, nothing waits: each mesh of a batch goes out in the frame it arrives.
#[test]
fn a_zero_hold_publishes_each_mesh_when_it_arrives() {
    let (mut app, executor, lamps) = corner_world(Duration::ZERO);
    executor.hold.store(true, Ordering::Relaxed);
    edit(&mut app, &[(CORNER, lamps[0])]);
    app.update();
    executor.run(2);
    app.update();
    assert_eq!(drain_events(&mut app).len(), 2);
    executor.run(usize::MAX);
    app.update();
    assert_eq!(drain_events(&mut app).len(), 2);
    let s = stats(&app);
    assert_eq!((s.edit_batches_held, s.edit_batches_split), (0, 0));
    executor.hold.store(false, Ordering::Relaxed);
    settle(&mut app);
    assert_matches_reference(&app);
}

/// A second edit reaches chunks whose results of the first edit are held: those results are
/// stale, so they are dropped and the chunks remeshed at once. Both edits go out in one frame.
#[test]
fn a_second_edit_of_a_held_chunk_keeps_the_batch_whole() {
    let (mut app, executor, lamps) = corner_world(WorldSettings::DEFAULT_EDIT_BATCH_HOLD);
    // The clock stands still: the hold never runs out.
    set_clock(&mut app, Instant::now());
    executor.hold.store(true, Ordering::Relaxed);
    edit(&mut app, &[(CORNER, lamps[0])]);
    app.update();
    executor.run(2);
    app.update();
    assert!(drain_events(&mut app).is_empty(), "2 results held");
    // The same lamp in blue: every chunk of the first edit, the 2 held ones too.
    edit(&mut app, &[(CORNER, lamps[1])]);
    executor.hold.store(false, Ordering::Relaxed);
    executor.run(usize::MAX);
    let mut frames = Vec::new();
    for _ in 0..4 {
        app.update();
        frames.push(drain_events(&mut app).len());
    }
    assert_eq!(frames, [0, 4, 0, 0], "one frame shows both edits");
    assert_eq!(stats(&app).edit_batches_split, 0);
    settle(&mut app);
    assert_matches_reference(&app);
}

/// The meshes of an edit go out ahead of a streaming backlog: the per-frame event budget only
/// delays streaming meshes, and the edit's remeshes run first.
#[test]
fn edits_jump_the_streaming_backlog() {
    let (registry, lamps) = lamp_registry();
    let settings = WorldSettings {
        max_mesh_events_per_frame: 2,
        ..small()
    };
    let mut app = lit_app(overworld(), registry, settings, Arc::new(InlineExecutor));
    spawn(&mut app, ChunkPos::new(0, 2, 0), 2, 2);
    settle(&mut app);
    // A second viewer far away: its whole view is generated and meshed in one frame, then
    // published 2 meshes a frame.
    spawn(&mut app, ChunkPos::new(60, 2, 0), 2, 2);
    app.update();
    drain_events(&mut app);
    let backlog = app.world().resource::<WorldJobs>().pending_meshes();
    assert!(backlog > 10, "{backlog} meshes waiting");
    let p = BlockPos::new(9, surface(9, 7) + 2, 7);
    edit(&mut app, &[(p, lamps[1])]);
    app.update();
    let (lamp_chunk, _) = p.split();
    let events = drain_events(&mut app);
    let upserts: BTreeSet<ChunkPos> = events
        .iter()
        .filter_map(|e| match e {
            ChunkMeshEvent::Upsert { key, .. } => Some(key.pos),
            ChunkMeshEvent::Remove { .. } => None,
        })
        .collect();
    assert!(upserts.contains(&lamp_chunk), "{upserts:?}");
    assert!(
        app.world().resource::<WorldJobs>().pending_meshes() >= backlog - 2,
        "the backlog is still there"
    );
    settle(&mut app);
    assert_matches_reference(&app);
}

/// The light cache follows the viewer: a chunk that comes within `light_cache_radius` after its
/// mesh job gets a light job, a chunk that leaves it drops its light, and digging far from the
/// start still remeshes only the dug chunk (its light is current, so the reach is known).
#[test]
fn the_light_cache_follows_the_viewer() {
    let (registry, _) = lamp_registry();
    let settings = WorldSettings {
        light_cache_radius: 1,
        ..small()
    };
    let mut app = lit_app(overworld(), registry, settings, Arc::new(InlineExecutor));
    let mut center = ChunkPos::new(0, -32, 0);
    let viewer = spawn(&mut app, center, 2, 1);
    settle(&mut app);
    for x in 1..=17 {
        center = ChunkPos::new(x, -32, 0);
        move_viewer(&mut app, viewer, center);
        settle(&mut app);
    }
    let map = app.world().resource::<ChunkMap>();
    let mut cached = 0;
    for (_, slot) in map.iter() {
        if slot.pos.distance_sq(center) > 1 {
            assert!(slot.light().is_none(), "{:?}: light dropped", slot.pos);
        } else if slot.interest.contains(Interest::MESH) {
            assert!(
                slot.current_light().is_some(),
                "{:?}: light cached",
                slot.pos
            );
            cached += 1;
        }
    }
    assert_eq!(cached, 7, "the viewer's chunk and its 6 face neighbors");
    assert!(stats(&app).light_jobs > 0);
    // An inner stone voxel of the viewer's chunk.
    let data = map.data_at(center).expect("loaded");
    let p = (1..31u32)
        .flat_map(|y| (1..31u32).flat_map(move |x| (1..31u32).map(move |z| (x, y, z))))
        .map(|(x, y, z)| LocalPos::new(x, y, z))
        .find(|&l| data.get(l) == STONE)
        .map(|l| BlockPos::from_parts(center, l))
        .expect("a stone voxel");
    assert_eq!(light_at(&app, p), LightValue::DARK);
    let before = mesh_jobs(&app);
    let fanout = stats(&app).light_fanout;
    edit(&mut app, &[(p, BlockState::AIR)]);
    settle(&mut app);
    assert_eq!(
        remeshed(&before, &mesh_jobs(&app)),
        BTreeSet::from([center]),
        "an inner voxel in the dark: its chunk only"
    );
    assert_eq!(stats(&app).light_fanout, fanout);
    assert_matches_reference(&app);
}

/// All-air chunks near the viewer cache their light too (a light job each, they have no mesh
/// job): the light at the eye of a viewer flying over the terrain, with a lamp's light in it.
#[test]
fn all_air_chunks_near_the_viewer_cache_their_light() {
    let (registry, lamps) = lamp_registry();
    let mut app = lit_app(overworld(), registry, small(), Arc::new(InlineExecutor));
    let center = ChunkPos::new(0, 12, 0);
    spawn(&mut app, center, 1, 1);
    settle(&mut app);
    let eye = BlockPos::new(30, center.origin().y + 16, 16);
    let map = app.world().resource::<ChunkMap>();
    let data = map.data_at(center).expect("loaded");
    assert_eq!(
        data.as_uniform(),
        Some(BlockState::AIR),
        "above the terrain"
    );
    assert_eq!(light_at(&app, eye), LightValue::FULL_SKY);
    // A lamp 3 voxels away, in the next chunk: the eye's chunk is still all air.
    edit(
        &mut app,
        &[(BlockPos::new(eye.x + 3, eye.y, eye.z), lamps[1])],
    );
    settle(&mut app);
    let lit = light_at(&app, eye);
    assert_eq!((lit.sky(), lit.rgb()), (15, [1, 3, 11]));
    assert!(stats(&app).light_jobs > 0);
}

/// The same world 2^40 voxels away in x and -2^40 in y meshes to the same bits, edits too.
#[test]
fn far_worlds_mesh_like_the_origin() {
    let far = ChunkPos::new(1 << 35, -(1 << 35), 7);
    let run = |offset: ChunkPos| {
        let (registry, lamps) = lamp_registry();
        let generator = Offset {
            inner: overworld(),
            offset,
        };
        let mut app = lit_app(generator, registry, small(), Arc::new(InlineExecutor));
        let at = |p: ChunkPos| ChunkPos::new(p.x + offset.x, p.y + offset.y, p.z + offset.z);
        spawn(&mut app, at(ChunkPos::new(0, 2, 0)), 2, 2);
        settle(&mut app);
        let first = hashes(&app);
        let lamp = BlockPos::new(
            9 + offset.x * 32,
            surface(9, 7) + 2 + offset.y * 32,
            7 + offset.z * 32,
        );
        edit(&mut app, &[(lamp, lamps[0])]);
        settle(&mut app);
        let relative = |h: BTreeMap<ChunkPos, Option<u64>>| -> BTreeMap<ChunkPos, Option<u64>> {
            h.into_iter()
                .map(|(p, h)| {
                    (
                        ChunkPos::new(p.x - offset.x, p.y - offset.y, p.z - offset.z),
                        h,
                    )
                })
                .collect()
        };
        (relative(first), relative(hashes(&app)), app)
    };
    let (near_first, near_edited, _) = run(ChunkPos::new(0, 0, 0));
    let (far_first, far_edited, app) = run(far);
    assert_eq!(far_first, near_first);
    assert_eq!(far_edited, near_edited);
    assert_ne!(near_edited, near_first);
    assert!(assert_matches_reference(&app) > 20);
}
