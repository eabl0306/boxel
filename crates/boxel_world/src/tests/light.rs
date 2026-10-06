//! Light in the world (ADR 0015, phase P3), with the real mesher: every settled mesh equals the
//! from-scratch [`Reference`], whatever the path of the viewer, the executor or the thread
//! count; streaming never remeshes for light; edits remesh exactly what their light reaches.
use super::{chunk_center, drain_events, move_viewer};
use crate::jobs::{InlineExecutor, JobExecutor, TaskPoolExecutor};
use crate::reference::Reference;
use crate::streaming::WorldJobs;
use crate::tests::support::LayerGen;
use crate::{
    ChunkMap, ChunkMeshEvent, ChunkViewer, EditQueue, Interest, MeshState, SkyMap, WorldContent,
    WorldPlugin, WorldSettings, WorldStats,
};
use boxel_app::App;
use boxel_ecs::prelude::*;
use boxel_mesh::MeshOptions;
use boxel_tasks::{TaskPool, TaskPoolBuilder};
use boxel_transform::Transform;
use boxel_voxel::{
    BlockDef, BlockPos, BlockRegistry, BlockRegistryBuilder, BlockState, ChunkData, ChunkKey,
    ChunkPos, FaceTextures, LightValue, LocalPos, RenderShape, TextureId,
};
use boxel_worldgen::{OverworldGenerator, WorldGenerator};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The default blocks plus two opaque lamps (orange, blue).
fn lamp_registry() -> (Arc<BlockRegistry>, [BlockState; 2]) {
    let mut b = BlockRegistryBuilder::with_default_blocks();
    let mut lamp = |name: &str, emission| {
        b.add_block(BlockDef {
            name: format!("test:{name}"),
            shape: RenderShape::Opaque,
            solid: true,
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

/// The platform's chunk unloads (no persistence yet: its edits are lost): the columns are
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

/// The meshes of one edit go out in the same frame, even when their jobs finish in different
/// frames; held longer than `edit_batch_hold_frames` after they arrive, they go out as they are.
#[test]
fn an_edit_batch_is_published_in_one_frame() {
    // Results that arrive in frame A wait through `hold` more frames; the rest arriving by then
    // completes the batch, else the held part goes out alone.
    for (hold, wait, split) in [(2, 2, false), (1, 3, true)] {
        let (registry, lamps) = lamp_registry();
        let executor = Arc::new(SwitchExecutor::default());
        let generator = LayerGen::new(-3..=1);
        let settings = WorldSettings {
            vertical_bounds: Some(generator.vertical.clone()),
            edit_batch_hold_frames: hold,
            ..small()
        };
        let mut app = lit_app(generator, registry, settings, executor.clone());
        spawn(&mut app, ChunkPos::new(0, 0, 0), 2, 8);
        settle(&mut app);
        executor.hold.store(true, Ordering::Relaxed);
        // A lamp on the stone at a chunk corner lights the top faces of 4 chunks.
        edit(&mut app, &[(BlockPos::new(31, -1, 31), lamps[0])]);
        app.update();
        assert!(drain_events(&mut app).is_empty());
        assert_eq!(executor.held(), 4, "the 4 stone chunks around the corner");
        executor.run(2);
        let mut frames = Vec::new();
        for _ in 0..wait {
            app.update();
            frames.push(drain_events(&mut app).len());
        }
        executor.run(usize::MAX);
        app.update();
        frames.push(drain_events(&mut app).len());
        let s = stats(&app);
        assert_eq!(s.edit_batches_held, 1);
        if split {
            assert_eq!(
                frames,
                [0, 0, 2, 2],
                "held results go out after {hold} frame(s)"
            );
            assert_eq!(s.edit_batches_split, 1);
        } else {
            assert_eq!(frames, [0, 0, 4], "one frame shows the whole batch");
            assert_eq!(s.edit_batches_split, 0);
        }
        executor.hold.store(false, Ordering::Relaxed);
        settle(&mut app);
        assert_matches_reference(&app);
    }
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
