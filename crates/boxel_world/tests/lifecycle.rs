//! Worlds opened and closed while the app runs (ADR 0034, issue #58), through the public API:
//! a world opened after another one starts as it would in a fresh process (blocks, light, sky
//! tops, meshes, ticks and clock, once settled), nothing of a closed world reaches the next one,
//! a saved world comes back whole (edits, ticks, clock, saved entities, game data), and the
//! requests, errors and frame order behave as documented. Positions sit at 2^40.
use boxel_app::{App, Idle, PreUpdate, Startup, Update};
use boxel_diagnostic::DiagnosticsBoard;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockRegistryBuilder, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey,
    ChunkPos, LocalPos, testing,
};
use boxel_world::jobs::{InlineExecutor, JobExecutor};
use boxel_world::lifecycle::{
    CloseOptions, OpenWorldError, WorldCommandsExt, WorldEpoch, WorldEvent, WorldLifecycle,
    WorldLifecycleSystems, WorldScope, WorldScoped, WorldSpec, WorldStatus, close_world,
    finish_closing, open_world,
};
use boxel_world::persist::{
    ChunkStore, EntityCodec, MemoryStore, PersistPlugin, PersistSettings, SaveBatch, Saved,
    SavedIds, WorldStore, add_entity_codec,
};
use boxel_world::{
    BlockTicks, ChunkMap, ChunkViewer, DataState, DaylightPlugin, EditQueue, FluidPlugin,
    MeshState, SkyMap, TimeOfDay, WorldPlugin, WorldSettings, WorldStats,
};
use boxel_worldgen::WorldGenerator;
use std::collections::BTreeMap;
use std::io;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 2^40 blocks east and south: the worlds of these tests sit there.
const FAR: i64 = 1 << 40;

/// Ground of `test:solid`, one to sixteen blocks above y = 0, its height a hash of the column
/// and the seed (a different seed is a different world); solid below y = 0 down to chunk -1.
struct Hills {
    seed: u64,
}

impl Hills {
    fn height(&self, x: i64, z: i64) -> i64 {
        let h = (x as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ (z as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
            ^ self.seed.wrapping_mul(0x1656_67b1_9e37_79f9);
        let h = h ^ (h >> 31);
        1 + (h.wrapping_mul(0x9e37_79b9_7f4a_7c15) >> 60) as i64
    }
}

impl WorldGenerator for Hills {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData {
        let solid = solid(registry);
        match key.pos.y {
            ..0 => ChunkData::uniform(solid),
            1.. => ChunkData::uniform(BlockState::AIR),
            0 => {
                let mut data = ChunkData::uniform(BlockState::AIR);
                for x in 0..32 {
                    for z in 0..32 {
                        let h = self.height(key.pos.x * 32 + x, key.pos.z * 32 + z);
                        for y in 0..h {
                            data.set(LocalPos::new(x as u32, y as u32, z as u32), solid);
                        }
                    }
                }
                data
            }
        }
    }

    fn vertical_chunks(&self) -> RangeInclusive<i64> {
        -1..=0
    }

    fn name(&self) -> &str {
        "hills"
    }
}

fn solid(registry: &BlockRegistry) -> BlockState {
    registry.default_state(registry.block_by_name("test:solid").expect("test:solid"))
}

fn state(app: &App, name: &str) -> BlockState {
    let registry = &app.world().resource::<boxel_world::WorldContent>().registry;
    registry.default_state(registry.block_by_name(name).expect(name))
}

/// A saved thing at a block, with a value: what the test codec saves of a [`Saved`] entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Token {
    at: BlockPos,
    value: u64,
}

struct TokenCodec;

impl EntityCodec for TokenCodec {
    fn name(&self) -> &str {
        "test:token"
    }

    fn version(&self) -> u32 {
        1
    }

    fn position(&self, world: &World, entity: Entity) -> Option<BlockPos> {
        world.get::<Token>(entity).map(|t| t.at)
    }

    fn save(&self, world: &World, entity: Entity, _: &SavedIds<'_>) -> Option<Vec<u8>> {
        let t = world.get::<Token>(entity)?;
        let mut out = Vec::new();
        for v in [t.at.x, t.at.y, t.at.z] {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&t.value.to_le_bytes());
        Some(out)
    }

    fn load(
        &self,
        world: &mut World,
        entity: Entity,
        _: u32,
        bytes: &[u8],
        _: &SavedIds<'_>,
    ) -> Result<(), String> {
        let word = |i: usize| -> Result<[u8; 8], String> {
            bytes
                .get(i * 8..i * 8 + 8)
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| "short".to_string())
        };
        let token = Token {
            at: BlockPos::new(
                i64::from_le_bytes(word(0)?),
                i64::from_le_bytes(word(1)?),
                i64::from_le_bytes(word(2)?),
            ),
            value: u64::from_le_bytes(word(3)?),
        };
        world.entity_mut(entity).insert(token);
        Ok(())
    }
}

/// Runs jobs only when told: a close stays `Closing` while its save job waits here.
#[derive(Default)]
struct Held(Mutex<Vec<Box<dyn FnOnce() + Send>>>);

impl JobExecutor for Held {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        self.0.lock().unwrap().push(job);
    }
}

impl Held {
    fn run_all(&self) {
        loop {
            let jobs = std::mem::take(&mut *self.0.lock().unwrap());
            if jobs.is_empty() {
                return;
            }
            for job in jobs {
                job();
            }
        }
    }
}

/// An app with no world yet: the world (inline jobs), fluids, the clock, the test codec and
/// saves (on `io`, without autosave), updated once (its startup ran).
fn app_with(io: Arc<dyn JobExecutor>) -> App {
    let registry = Arc::new(testing::registry());
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        executor: Arc::new(InlineExecutor),
        ..WorldPlugin::closed(registry)
    })
    .add_plugins((FluidPlugin, DaylightPlugin))
    .add_plugins(PersistPlugin::without_store().with_executor(io))
    .insert_resource(PersistSettings {
        autosave: None,
        ..PersistSettings::default()
    });
    add_entity_codec(&mut app, TokenCodec);
    app.finish();
    app.cleanup();
    app.update();
    app
}

fn app() -> App {
    app_with(Arc::new(InlineExecutor))
}

/// The settings of the test worlds: three layers around the ground.
fn settings() -> WorldSettings {
    WorldSettings {
        vertical_bounds: Some(-1..=1),
        ..WorldSettings::default()
    }
}

fn spec(seed: u64) -> WorldSpec {
    WorldSpec::new(Arc::new(Hills { seed }), settings())
}

fn open_store(backend: &Arc<MemoryStore>) -> WorldStore {
    WorldStore::open(
        Arc::clone(backend) as Arc<dyn ChunkStore>,
        &testing::registry(),
        "hills",
    )
    .expect("the world opens")
}

/// The center of the chunk column `(cx, cz)` from [`FAR`], at the ground.
fn spot(cx: i64, cz: i64) -> DVec3 {
    DVec3::new(
        (FAR + cx * 32 + 16) as f64,
        20.0,
        (-FAR + cz * 32 + 16) as f64,
    )
}

/// A block of the column `(cx, cz)` from [`FAR`].
fn block(cx: i64, cz: i64, x: i64, y: i64, z: i64) -> BlockPos {
    BlockPos::new(FAR + cx * 32 + x, y, -FAR + cz * 32 + z)
}

/// The viewer of a world (despawned with it).
fn viewer(app: &mut App) -> Entity {
    app.world_mut()
        .spawn((
            Transform::from_translation(spot(0, 0)),
            ChunkViewer::new(2).with_vertical_radius(1),
            WorldScoped,
        ))
        .id()
}

fn settle(app: &mut App) {
    for _ in 0..500 {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        if stats.chunks > 0 && stats.is_settled() {
            for _ in 0..3 {
                app.update();
            }
            return;
        }
    }
    panic!("not settled: {:?}", app.world().resource::<WorldStats>());
}

fn status(app: &App) -> WorldStatus {
    app.world().resource::<WorldLifecycle>().status()
}

fn epoch(app: &App) -> u64 {
    app.world().resource::<WorldEpoch>().get()
}

fn block_at(app: &App, pos: BlockPos) -> Option<BlockState> {
    app.world().resource::<ChunkMap>().block_at(pos)
}

/// Opens `spec` now and spawns its viewer.
fn open(app: &mut App, spec: WorldSpec) {
    open_world(app.world_mut(), spec).expect("the world opens");
    viewer(app);
}

/// Closes the world now; with inline jobs it ends at once.
fn close(app: &mut App) {
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    assert!(finish_closing(app.world_mut(), Duration::from_secs(10)));
    assert_eq!(status(app), WorldStatus::Closed);
}

/// Everything a settled world shows, by chunk position: blocks, cached light, mesh.
type Snapshot = BTreeMap<
    ChunkPos,
    (
        Vec<BlockState>,
        Option<boxel_light::ChunkLight>,
        Option<boxel_mesh::ChunkMesh>,
    ),
>;

fn snapshot(app: &App) -> (Snapshot, BTreeMap<(i64, i64), Vec<i64>>) {
    let map = app.world().resource::<ChunkMap>();
    let mut chunks = Snapshot::new();
    for (_, slot) in map.iter() {
        let blocks = match slot.data() {
            DataState::Ready(data) => {
                let mut out = vec![BlockState::AIR; CHUNK_VOLUME];
                data.to_dense(&mut out);
                out
            }
            _ => Vec::new(),
        };
        let mesh = match slot.mesh() {
            MeshState::Ready { mesh, .. } => Some(boxel_mesh::ChunkMesh::clone(mesh)),
            _ => None,
        };
        chunks.insert(slot.key().pos, (blocks, slot.light().cloned(), mesh));
    }
    let sky = app.world().resource::<SkyMap>();
    let mut tops = BTreeMap::new();
    for &pos in chunks.keys() {
        if let Some(tile) = sky.tile(pos.x, pos.z) {
            tops.insert((pos.x, pos.z), tile.tops().to_vec());
        }
    }
    (chunks, tops)
}

/// The world's messages since the last call.
#[derive(Resource, Default)]
struct Events(Vec<WorldEvent>);

fn log_events(mut reader: MessageReader<WorldEvent>, mut log: ResMut<Events>) {
    log.0.extend(reader.read().cloned());
}

// ----------------------------------------------------------------------------- the invariants

/// Test 2 (the headline invariant): a world opened after another one equals the same world in a
/// fresh app once both settle (blocks, light, sky tops and meshes per chunk position; the clock
/// and the tick counter right after the open), with and without a store, and with and without
/// a clock in the spec.
#[test]
fn a_world_after_another_equals_the_same_world_in_a_fresh_app() {
    for (saved, clock) in [
        (false, None),
        (true, None),
        (false, Some(1234)),
        (true, Some(99)),
    ] {
        let spec_b = || {
            let mut spec = spec(2);
            spec.clock = clock;
            if saved {
                spec = spec.with_store(open_store(&Arc::new(MemoryStore::new())));
            }
            spec
        };
        // A, edited and ticking, then B.
        let mut switched = app();
        open(
            &mut switched,
            spec(1).with_store(open_store(&Arc::new(MemoryStore::new()))),
        );
        settle(&mut switched);
        let water = state(&switched, "test:fluid");
        switched
            .world_mut()
            .resource_mut::<EditQueue>()
            .set_block(block(0, 0, 3, 20, 3), water);
        switched.world_mut().resource_mut::<TimeOfDay>().tick = 5_000;
        settle(&mut switched);
        assert!(
            !switched
                .world()
                .resource::<BlockTicks>()
                .pending()
                .is_empty()
        );
        close(&mut switched);
        open(&mut switched, spec_b());
        let opened = (
            switched.world().resource::<TimeOfDay>().tick,
            switched.world().resource::<BlockTicks>().now(),
            switched.world().resource::<BlockTicks>().pending(),
        );
        settle(&mut switched);

        let mut fresh = app();
        open(&mut fresh, spec_b());
        let expected = (
            fresh.world().resource::<TimeOfDay>().tick,
            fresh.world().resource::<BlockTicks>().now(),
            fresh.world().resource::<BlockTicks>().pending(),
        );
        settle(&mut fresh);
        assert_eq!(
            opened, expected,
            "clock and ticks at the open ({saved}, {clock:?})"
        );
        assert_eq!(opened.0, clock.unwrap_or(TimeOfDay::default().tick));
        let (a, b) = (snapshot(&switched), snapshot(&fresh));
        assert!(a.0.len() > 20, "the world loaded");
        assert_eq!(
            a.0.keys().collect::<Vec<_>>(),
            b.0.keys().collect::<Vec<_>>()
        );
        for (pos, chunk) in &a.0 {
            assert!(
                chunk == &b.0[pos],
                "chunk {pos:?} differs ({saved}, {clock:?})"
            );
        }
        assert_eq!(a.1, b.1, "sky tops");
    }
}

/// Test 3: no `ChunkId` is handed out twice: the ids of a reopened world are new.
#[test]
fn chunk_ids_never_repeat_across_worlds() {
    let mut app = app();
    open(&mut app, spec(1));
    settle(&mut app);
    let before: Vec<u64> = app
        .world()
        .resource::<ChunkMap>()
        .iter()
        .map(|(id, _)| id.to_bits())
        .collect();
    close(&mut app);
    open(&mut app, spec(1));
    settle(&mut app);
    let after: Vec<u64> = app
        .world()
        .resource::<ChunkMap>()
        .iter()
        .map(|(id, _)| id.to_bits())
        .collect();
    assert_eq!(before.len(), after.len());
    assert!(after.iter().all(|id| !before.contains(id)));
}

/// Test 4: right after a close every per-world resource is empty (the lifecycle's checks ran
/// and held), every per-world message is cleared, the store is gone, the epoch moved once at
/// the close and once at the open, and the world stays empty while closed.
#[test]
fn a_closed_world_leaves_nothing() {
    let mut app = app();
    let backend = Arc::new(MemoryStore::new());
    let e0 = epoch(&app);
    open(&mut app, spec(1).with_store(open_store(&backend)));
    assert_eq!(epoch(&app), e0 + 1);
    settle(&mut app);
    assert!(app.world().contains_resource::<WorldStore>());
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    assert_eq!(epoch(&app), e0 + 2);
    let world = app.world();
    let scope = world.resource::<WorldScope>();
    assert_eq!(scope.last_check(), Some(&[][..]));
    assert_eq!(scope.messages_waiting(world), 0);
    assert!(!world.contains_resource::<WorldStore>());
    assert_eq!(status(&app), WorldStatus::Closed);
    for _ in 0..5 {
        app.update();
    }
    let world = app.world();
    assert!(world.resource::<WorldScope>().not_closed(world).is_empty());
    assert!(world.resource::<ChunkMap>().is_empty());
}

// ------------------------------------------------------------------------- a saved world

/// Test 1: A, B, A again. A at 2^40 gets edits, pending water ticks, a clock, a saved entity and
/// a game value; B (another seed, unsaved) on the same spot shows its own blocks; A opened again
/// from a new opening of its store has them all back, bit for bit.
#[test]
fn a_world_comes_back_whole_after_another_one() {
    let mut app = app();
    let backend = Arc::new(MemoryStore::new());
    open(&mut app, spec(1).with_store(open_store(&backend)));
    settle(&mut app);
    let (glass, water) = (state(&app, "test:glass"), state(&app, "test:fluid"));
    let (edit, wet) = (block(1, -1, 5, 25, 7), block(0, 0, 9, 22, 9));
    {
        let mut queue = app.world_mut().resource_mut::<EditQueue>();
        queue.set_block(edit, glass);
        queue.set_block(wet, water);
    }
    let token = Token {
        at: block(-1, 1, 2, 30, 2),
        value: 0xfeed,
    };
    app.world_mut().spawn((Saved::new(), token));
    settle(&mut app);
    app.world_mut().resource_mut::<TimeOfDay>().tick = 777_777;
    app.world_mut()
        .resource_mut::<WorldLifecycle>()
        .stage_meta("game:player", b"at the lake".to_vec());
    let ticks = app.world().resource::<BlockTicks>().pending();
    assert!(!ticks.is_empty(), "the water waits for its ticks");
    close(&mut app);
    assert!(
        app.world()
            .resource::<WorldLifecycle>()
            .last_report()
            .unwrap()
            .saved
    );
    assert!(
        app.world_mut()
            .query::<&Token>()
            .iter(app.world())
            .next()
            .is_none()
    );

    open(&mut app, spec(2));
    settle(&mut app);
    assert_eq!(
        block_at(&app, edit),
        Some(BlockState::AIR),
        "B has its own blocks"
    );
    assert_ne!(app.world().resource::<TimeOfDay>().tick, 777_777);
    close(&mut app);

    let store = open_store(&backend);
    open(&mut app, spec(1).with_store(store));
    assert_eq!(app.world().resource::<TimeOfDay>().tick, 777_777);
    assert_eq!(app.world().resource::<BlockTicks>().pending(), ticks);
    settle(&mut app);
    assert_eq!(block_at(&app, edit), Some(glass));
    let tokens: Vec<Token> = app
        .world_mut()
        .query::<&Token>()
        .iter(app.world())
        .copied()
        .collect();
    assert_eq!(tokens, vec![token]);
    let store = app.world().resource::<WorldStore>();
    assert_eq!(
        store.meta("game:player").unwrap(),
        Some(b"at the lake".to_vec())
    );
}

/// Test 8: saved entities at 2^40 are put away at the close, a saved viewer too, and come back
/// once; one the game despawned before the close loses its record.
#[test]
fn saved_entities_are_put_away_at_the_close_and_come_back() {
    let mut app = app();
    let backend = Arc::new(MemoryStore::new());
    open(&mut app, spec(1).with_store(open_store(&backend)));
    let saved_viewer = app
        .world_mut()
        .spawn((
            Saved::new(),
            Token {
                at: block(0, 0, 1, 20, 1),
                value: 1,
            },
            Transform::from_translation(spot(1, 1)),
            ChunkViewer::new(1).with_vertical_radius(1),
        ))
        .id();
    let kept = Token {
        at: block(1, 0, 4, 18, 4),
        value: 2,
    };
    app.world_mut().spawn((Saved::new(), kept));
    app.world_mut().spawn((
        Saved::new(),
        Token {
            at: block(0, 1, 4, 18, 4),
            value: 3,
        },
    ));
    settle(&mut app);
    // The game kills it (it may have been put away and brought back: another entity).
    let gone = app
        .world_mut()
        .query::<(Entity, &Token)>()
        .iter(app.world())
        .find(|(_, t)| t.value == 3)
        .map(|(e, _)| e)
        .expect("alive");
    app.world_mut().despawn(gone);
    close(&mut app);
    assert!(app.world().get_entity(saved_viewer).is_err());

    open(&mut app, spec(1).with_store(open_store(&backend)));
    app.world_mut().spawn((
        Transform::from_translation(spot(1, 1)),
        ChunkViewer::new(2).with_vertical_radius(1),
        WorldScoped,
    ));
    settle(&mut app);
    let mut values: Vec<u64> = app
        .world_mut()
        .query::<&Token>()
        .iter(app.world())
        .map(|t| t.value)
        .collect();
    values.sort_unstable();
    assert_eq!(
        values,
        vec![1, 2],
        "the viewer and the kept one, once; the despawned one gone"
    );
}

// --------------------------------------------------------------------------------- errors

/// Test 10: every refused open leaves the app's world as it was, and closes the refused store.
#[test]
fn refused_opens_change_nothing() {
    let mut app = app();
    let backend = Arc::new(MemoryStore::new());
    open(&mut app, spec(1).with_store(open_store(&backend)));
    // Two openings of A's store while A is open: they would miss A's last save.
    let (early, early_too) = (open_store(&backend), open_store(&backend));
    let refused = open_store(&Arc::new(MemoryStore::new()));
    let e = epoch(&app);
    assert_eq!(
        open_world(app.world_mut(), spec(2).with_store(refused.clone())),
        Err(OpenWorldError::NotClosed)
    );
    assert!(
        refused.is_closed(),
        "a refused store is closed (inline here)"
    );
    assert_eq!(epoch(&app), e);
    assert_eq!(status(&app), WorldStatus::Open);
    close(&mut app);

    assert_eq!(
        open_world(app.world_mut(), spec(1).with_store(early)),
        Err(OpenWorldError::StoreOpenedEarly)
    );
    // An unrelated world in between does not make it right.
    open(&mut app, spec(2));
    close(&mut app);
    assert_eq!(
        open_world(app.world_mut(), spec(1).with_store(early_too)),
        Err(OpenWorldError::StoreOpenedEarly)
    );
    // The store that served A, and a closed one.
    let served = open_store(&backend);
    open_world(app.world_mut(), spec(1).with_store(served.clone())).unwrap();
    close(&mut app);
    assert_eq!(
        open_world(app.world_mut(), spec(1).with_store(served)),
        Err(OpenWorldError::StoreServed)
    );
    let closed = open_store(&Arc::new(MemoryStore::new()));
    closed.close().unwrap();
    assert_eq!(
        open_world(app.world_mut(), spec(1).with_store(closed)),
        Err(OpenWorldError::StoreClosed)
    );
    // Another registry.
    let other = BlockRegistryBuilder::new().build();
    let foreign = WorldStore::open(Arc::new(MemoryStore::new()), &other, "hills").unwrap();
    assert!(matches!(
        open_world(app.world_mut(), spec(1).with_store(foreign)),
        Err(OpenWorldError::OtherRegistry { .. })
    ));
    // In an idle step.
    let result = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&result);
    app.add_systems(Idle, move |world: &mut World| {
        *seen.lock().unwrap() = Some(open_world(world, spec(1)));
    });
    app.run_idle(Instant::now() + Duration::from_millis(5));
    assert_eq!(*result.lock().unwrap(), Some(Err(OpenWorldError::InIdle)));
    assert_eq!(status(&app), WorldStatus::Closed);
    assert!(app.world().resource::<ChunkMap>().is_empty());

    // A saved world needs the persistence.
    let mut bare = App::new();
    bare.add_plugins(WorldPlugin {
        executor: Arc::new(InlineExecutor),
        ..WorldPlugin::closed(Arc::new(testing::registry()))
    });
    bare.update();
    let store = open_store(&Arc::new(MemoryStore::new()));
    assert_eq!(
        open_world(bare.world_mut(), spec(1).with_store(store)),
        Err(OpenWorldError::NoPersistPlugin)
    );
    assert_eq!(
        bare.world().resource::<WorldLifecycle>().status(),
        WorldStatus::Closed
    );
}

// ------------------------------------------------------------------- requests and frames

/// Test 11: requests through `Commands` run in the next `PreUpdate`, in order: a switch (close,
/// then open) gives what the immediate calls give; an open waits while the world closes; a
/// second close and a save without a world do nothing; a save request writes within two frames.
#[test]
fn requests_run_in_order_in_the_next_frame() {
    let held = Arc::new(Held::default());
    let mut app = app_with(Arc::clone(&held) as Arc<dyn JobExecutor>);
    app.init_resource::<Events>()
        .add_systems(PreUpdate, log_events.after(WorldLifecycleSystems));
    let backend = Arc::new(MemoryStore::new());
    open(&mut app, spec(1).with_store(open_store(&backend)));
    held.run_all();
    settle(&mut app);
    held.run_all();

    // A save request: written by the next save step, without the autosave.
    let glass = state(&app, "test:glass");
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(block(0, 0, 1, 26, 1), glass);
    app.update();
    let saved = app.world().resource::<WorldStore>().stats().saved_chunks;
    app.world_mut().commands().save_world();
    app.update();
    app.update();
    held.run_all();
    app.update();
    assert!(app.world().resource::<WorldStore>().stats().saved_chunks > saved);

    // A switch through commands, the open waiting for the close's save.
    {
        let mut commands = app.world_mut().commands();
        commands.close_world(CloseOptions::default());
        commands.close_world(CloseOptions::default());
        commands.open_world(spec(2));
    }
    app.world_mut().flush();
    app.world_mut().resource_mut::<Events>().0.clear();
    assert_eq!(
        status(&app),
        WorldStatus::Open,
        "nothing before the next PreUpdate"
    );
    app.update();
    assert_eq!(status(&app), WorldStatus::Closing, "the save job waits");
    app.update();
    assert_eq!(status(&app), WorldStatus::Closing, "the open waits too");
    for _ in 0..4 {
        held.run_all();
        app.update();
    }
    assert_eq!(
        status(&app),
        WorldStatus::Open,
        "closed, then the queued open ran"
    );
    let events = std::mem::take(&mut app.world_mut().resource_mut::<Events>().0);
    assert!(matches!(
        events.as_slice(),
        [WorldEvent::Closed { report, .. }, WorldEvent::Opened { .. }] if report.saved
    ));
    viewer(&mut app);
    settle(&mut app);
    let via_commands = snapshot(&app);

    // The same switch, immediately.
    let mut direct = app_with(Arc::new(InlineExecutor));
    open(&mut direct, spec(1));
    settle(&mut direct);
    close(&mut direct);
    open(&mut direct, spec(2));
    settle(&mut direct);
    assert!(via_commands == snapshot(&direct));

    // A save and a close without a world: dropped.
    let mut commands = app.world_mut().commands();
    commands.close_world(CloseOptions::default());
    commands.save_world();
    commands.save_world();
    app.world_mut().flush();
    app.update();
    held.run_all();
    app.update();
    assert_eq!(status(&app), WorldStatus::Closed);
}

/// Test 14: a close asked in `Update` runs in the next `PreUpdate`; one from an exclusive hook
/// ordered before the lifecycle runs in that same `PreUpdate`. Both leave test 4's state.
#[test]
fn a_close_runs_in_the_pre_update_after_the_request() {
    #[derive(Resource, Default)]
    struct CloseNow(bool);

    let mut app = app();
    app.init_resource::<CloseNow>().add_systems(
        PreUpdate,
        (|world: &mut World| {
            if std::mem::take(&mut world.resource_mut::<CloseNow>().0) {
                assert!(close_world(world, CloseOptions::default()));
            }
        })
        .before(WorldLifecycleSystems),
    );
    open(&mut app, spec(1));
    settle(&mut app);
    app.add_systems(Update, |mut commands: Commands, mut once: Local<bool>| {
        if !std::mem::replace(&mut *once, true) {
            commands.close_world(CloseOptions::default());
        }
    });
    app.update();
    assert_eq!(
        status(&app),
        WorldStatus::Open,
        "asked in this frame's Update"
    );
    app.update();
    assert_eq!(status(&app), WorldStatus::Closed);
    assert_eq!(
        app.world().resource::<WorldScope>().last_check(),
        Some(&[][..])
    );
    assert!(app.world().resource::<ChunkMap>().is_empty());

    open(&mut app, spec(1));
    settle(&mut app);
    app.world_mut().resource_mut::<CloseNow>().0 = true;
    app.update();
    assert_eq!(status(&app), WorldStatus::Closed);
    assert_eq!(
        app.world().resource::<WorldScope>().last_check(),
        Some(&[][..])
    );
    assert!(app.world().resource::<ChunkMap>().is_empty());
}

/// Test 12: the world of `WorldPlugin::new` opens before `Startup`: its systems see the store,
/// the restored clock and ticks. A change of `WorldSettings` or `PersistSettings` made after
/// the build and before the first update is kept.
#[test]
fn the_startup_world_opens_before_startup() {
    let registry = Arc::new(testing::registry());
    let backend = Arc::new(MemoryStore::new());
    let build = |store: WorldStore| {
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            executor: Arc::new(InlineExecutor),
            settings: settings(),
            ..WorldPlugin::new(Arc::new(Hills { seed: 1 }), Arc::clone(&registry))
        })
        .add_plugins((FluidPlugin, DaylightPlugin))
        .add_plugins(PersistPlugin::new(store).with_executor(Arc::new(InlineExecutor)));
        app.finish();
        app.cleanup();
        app
    };
    // A saved world with a clock and pending water.
    let mut first = build(open_store(&backend));
    first.update();
    viewer(&mut first);
    settle(&mut first);
    let water = state(&first, "test:fluid");
    first
        .world_mut()
        .resource_mut::<EditQueue>()
        .set_block(block(0, 0, 9, 22, 9), water);
    settle(&mut first);
    first.world_mut().resource_mut::<TimeOfDay>().tick = 4242;
    let ticks = first.world().resource::<BlockTicks>().pending();
    first.shutdown();
    drop(first);

    #[derive(Resource, Default, Debug, PartialEq)]
    struct Seen(Option<(bool, u64, usize)>);
    let mut app = build(open_store(&backend));
    app.init_resource::<Seen>().add_systems(
        Startup,
        |store: Option<Res<WorldStore>>,
         time: Res<TimeOfDay>,
         ticks: Res<BlockTicks>,
         mut seen: ResMut<Seen>| {
            seen.0 = Some((store.is_some(), time.tick, ticks.pending().len()));
        },
    );
    app.world_mut()
        .resource_mut::<WorldSettings>()
        .vertical_bounds = Some(-1..=2);
    app.update();
    assert_eq!(
        app.world().resource::<Seen>().0,
        Some((true, 4242, ticks.len()))
    );
    assert_eq!(
        app.world().resource::<WorldSettings>().vertical_bounds,
        Some(-1..=2)
    );
    app.shutdown();
    drop(app);

    // Without the saved clock: the clock the app started with.
    let mut app = build(open_store(&backend));
    app.insert_resource(PersistSettings {
        clock: false,
        ..PersistSettings::default()
    });
    app.update();
    assert_eq!(
        app.world().resource::<TimeOfDay>().tick,
        TimeOfDay::default().tick
    );
}

// ------------------------------------------------------------------------------ game data

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
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::other("disk full"));
        }
        self.inner.save_batch(batch)
    }
}

/// Test 13: staged game data joins the save moment: in the same transaction as the chunks (a
/// failed batch writes neither), the newest value per name, written by a requested save, the
/// close and the exit.
#[test]
fn staged_game_data_is_saved_with_the_world() {
    let mut app = app();
    let flaky = Arc::new(Flaky::default());
    let store = WorldStore::open(
        Arc::clone(&flaky) as Arc<dyn ChunkStore>,
        &testing::registry(),
        "hills",
    )
    .unwrap();
    open(&mut app, spec(1).with_store(store));
    app.insert_resource(PersistSettings {
        autosave: None,
        retry_after: Duration::ZERO,
        ..PersistSettings::default()
    });
    settle(&mut app);
    let glass = state(&app, "test:glass");
    let edit = block(0, 0, 2, 24, 2);
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(edit, glass);
    app.update();
    {
        let mut lifecycle = app.world_mut().resource_mut::<WorldLifecycle>();
        lifecycle.stage_meta("game:player", b"old".to_vec());
        lifecycle.stage_meta("game:player", b"new".to_vec());
        lifecycle.request_save();
    }
    flaky.fail.store(true, Ordering::SeqCst);
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(flaky.inner.load_meta("game:player").unwrap(), None);
    assert!(
        flaky.inner.keys().unwrap().is_empty(),
        "neither the chunk nor the value"
    );
    flaky.fail.store(false, Ordering::SeqCst);
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(
        flaky.inner.load_meta("game:player").unwrap(),
        Some(b"new".to_vec())
    );
    assert_eq!(flaky.inner.keys().unwrap().len(), 1);

    // At the close.
    app.world_mut()
        .resource_mut::<WorldLifecycle>()
        .stage_meta("game:player", b"closing".to_vec());
    close(&mut app);
    assert_eq!(
        flaky.inner.load_meta("game:player").unwrap(),
        Some(b"closing".to_vec())
    );

    // On exit.
    let backend = Arc::new(MemoryStore::new());
    open(&mut app, spec(1).with_store(open_store(&backend)));
    settle(&mut app);
    app.world_mut()
        .resource_mut::<WorldLifecycle>()
        .stage_meta("game:player", b"exit".to_vec());
    app.shutdown();
    assert_eq!(
        backend.load_meta("game:player").unwrap(),
        Some(b"exit".to_vec())
    );
}

// ------------------------------------------------------------------------ the close's save

/// An app on `held` IO jobs whose world, saved in `store`, settled with an autosave job in
/// flight (it wrote nothing yet) that holds an edit at `first`, and an edit at `last` made
/// after that job started. Its close's last save comes after that job.
fn closing_app(
    held: &Arc<Held>,
    store: WorldStore,
    settings: PersistSettings,
) -> (App, BlockPos, BlockPos) {
    let mut app = app_with(Arc::clone(held) as Arc<dyn JobExecutor>);
    app.insert_resource(settings);
    open(&mut app, spec(1).with_store(store));
    held.run_all();
    settle(&mut app);
    held.run_all();
    let glass = state(&app, "test:glass");
    let (first, last) = (block(0, 0, 1, 26, 1), block(1, 1, 2, 27, 2));
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(first, glass);
    app.update();
    app.world_mut()
        .resource_mut::<WorldLifecycle>()
        .request_save();
    app.update();
    app.update();
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(last, glass);
    app.update();
    (app, first, last)
}

/// Waits `time` without sleeping the thread (the jobs run on this one).
fn pass(time: Duration) {
    let end = Instant::now() + time;
    while Instant::now() < end {
        std::thread::yield_now();
    }
}

/// Runs `held`'s jobs and frames until the world is closed, for at most 5 s.
fn run_until_closed(app: &mut App, held: &Held) {
    let end = Instant::now() + Duration::from_secs(5);
    while status(app) != WorldStatus::Closed && Instant::now() < end {
        held.run_all();
        app.update();
        std::thread::yield_now();
    }
}

fn last_report(app: &App) -> boxel_world::lifecycle::CloseReport {
    app.world()
        .resource::<WorldLifecycle>()
        .last_report()
        .cloned()
        .expect("a close ended")
}

/// A close with `save: false` (the world is deleted next) writes nothing more: an autosave in
/// flight that fails is not tried again, the close ends once that job reported (not at
/// `close_timeout`), and the report says the world was not saved, without the dropped save's
/// error. With a disk that works, the job in flight lands and the report still says not saved.
#[test]
fn a_close_without_saving_drops_what_waits_and_says_so() {
    for disk_works in [false, true] {
        let held = Arc::new(Held::default());
        let flaky = Arc::new(Flaky::default());
        let store = WorldStore::open(
            Arc::clone(&flaky) as Arc<dyn ChunkStore>,
            &testing::registry(),
            "hills",
        )
        .unwrap();
        let (mut app, first, last) = closing_app(
            &held,
            store,
            PersistSettings {
                autosave: None,
                retry_after: Duration::ZERO,
                close_timeout: Duration::from_secs(60),
                ..PersistSettings::default()
            },
        );
        flaky.fail.store(!disk_works, Ordering::SeqCst);
        assert!(close_world(app.world_mut(), CloseOptions { save: false }));
        for _ in 0..3 {
            held.run_all();
            app.update();
        }
        assert_eq!(status(&app), WorldStatus::Closed, "works {disk_works}");
        let report = last_report(&app);
        assert!(
            !report.saved && !report.gave_up && report.errors.is_empty(),
            "works {disk_works}: {report:?}"
        );
        flaky.fail.store(false, Ordering::SeqCst);
        let keys = flaky.inner.keys().unwrap();
        let saved = |pos: BlockPos| keys.contains(&ChunkKey::new(ChunkPos::containing(pos)));
        assert_eq!(saved(first), disk_works, "the job in flight");
        assert!(!saved(last), "dropped");
    }
}

/// `close_timeout` gives up on a last save that keeps failing, not on one still running: a
/// slow job that outlives it is waited for, and the report says what it did.
#[test]
fn a_slow_last_save_is_waited_for() {
    let held = Arc::new(Held::default());
    let backend = Arc::new(MemoryStore::new());
    let (mut app, first, last) = closing_app(
        &held,
        open_store(&backend),
        PersistSettings {
            autosave: None,
            close_timeout: Duration::from_millis(20),
            ..PersistSettings::default()
        },
    );
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    pass(Duration::from_millis(40));
    app.update();
    app.update();
    assert_eq!(status(&app), WorldStatus::Closing, "the jobs still run");
    for _ in 0..4 {
        held.run_all();
        app.update();
    }
    assert_eq!(status(&app), WorldStatus::Closed);
    let report = last_report(&app);
    assert!(report.saved && !report.gave_up, "{report:?}");
    assert!(report.errors.is_empty(), "{report:?}");
    let keys = backend.keys().unwrap();
    for pos in [first, last] {
        assert!(keys.contains(&ChunkKey::new(ChunkPos::containing(pos))));
    }
}

/// A last save that keeps failing gives up at `close_timeout` even when it may try again at
/// once (`retry_after` zero) and its jobs run off the main thread: the deadline is checked
/// between a failed job and the next try, so a new try never hides the failure.
#[test]
fn a_failing_last_save_gives_up_between_tries() {
    let held = Arc::new(Held::default());
    let flaky = Arc::new(Flaky::default());
    let store = WorldStore::open(
        Arc::clone(&flaky) as Arc<dyn ChunkStore>,
        &testing::registry(),
        "hills",
    )
    .unwrap();
    let (mut app, _, _) = closing_app(
        &held,
        store,
        PersistSettings {
            autosave: None,
            retry_after: Duration::ZERO,
            close_timeout: Duration::from_millis(20),
            ..PersistSettings::default()
        },
    );
    flaky.fail.store(true, Ordering::SeqCst);
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    run_until_closed(&mut app, &held);
    assert_eq!(status(&app), WorldStatus::Closed);
    let report = last_report(&app);
    assert!(report.gave_up && !report.saved, "{report:?}");
    assert!(!report.errors.is_empty(), "{report:?}");
}

/// A store in memory whose close is for good, as a file's: every write after it fails.
#[derive(Default)]
struct Closable {
    inner: MemoryStore,
    closed: AtomicBool,
}

impl Closable {
    fn check(&self) -> io::Result<()> {
        if self.closed.load(Ordering::SeqCst) {
            Err(io::Error::new(io::ErrorKind::NotConnected, "closed"))
        } else {
            Ok(())
        }
    }
}

impl ChunkStore for Closable {
    fn load(&self, key: ChunkKey) -> io::Result<Option<Vec<u8>>> {
        self.check()?;
        self.inner.load(key)
    }
    fn keys(&self) -> io::Result<Vec<ChunkKey>> {
        self.check()?;
        self.inner.keys()
    }
    fn load_meta(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.check()?;
        self.inner.load_meta(name)
    }
    fn save_batch(&self, batch: &SaveBatch) -> io::Result<()> {
        self.check()?;
        self.inner.save_batch(batch)
    }
    fn close(&self) -> io::Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

/// Test 10, while a world closes: a store refused then because it is the closing world's (a
/// clone a game kept to reload it, or another opening of its backend) is dropped, not closed:
/// closing it would close the store the closing world still writes its last moment to. Here an
/// autosave is in flight at the close, so the last moment's job starts after the refusal.
#[test]
fn a_store_refused_while_its_world_closes_keeps_the_last_save() {
    let settings = PersistSettings {
        autosave: None,
        retry_after: Duration::from_millis(5),
        close_timeout: Duration::from_millis(100),
        ..PersistSettings::default()
    };
    for clone in [true, false] {
        let held = Arc::new(Held::default());
        let backend = Arc::new(Closable::default());
        let opening = || {
            WorldStore::open(
                Arc::clone(&backend) as Arc<dyn ChunkStore>,
                &testing::registry(),
                "hills",
            )
            .unwrap()
        };
        let a = opening();
        let kept = a.clone();
        let (mut app, first, last) = closing_app(&held, a, settings.clone());
        assert!(close_world(app.world_mut(), CloseOptions::default()));
        assert_eq!(status(&app), WorldStatus::Closing);
        let refused = if clone { kept.clone() } else { opening() };
        assert_eq!(
            open_world(app.world_mut(), spec(2).with_store(refused)),
            Err(OpenWorldError::NotClosed)
        );
        run_until_closed(&mut app, &held);
        assert_eq!(status(&app), WorldStatus::Closed);
        let report = last_report(&app);
        assert!(report.saved && !report.gave_up, "clone {clone}: {report:?}");
        let keys = backend.inner.keys().unwrap();
        for pos in [first, last] {
            assert!(
                keys.contains(&ChunkKey::new(ChunkPos::containing(pos))),
                "clone {clone}: {pos:?} saved"
            );
        }
        assert!(kept.is_closed());
    }
}

// ------------------------------------------------------------------------- the board lines

/// The board lines of a closed world go with it: while closed, `"world"` shows the empty map
/// (not the closed world's last counts), and a next world with light off shows no `"light"`
/// line, as in a fresh app.
#[test]
fn a_closed_world_leaves_no_board_lines() {
    let line = |app: &App, key: &str| {
        app.world()
            .resource::<DiagnosticsBoard>()
            .get(key)
            .map(str::to_string)
    };
    let mut app = app();
    app.init_resource::<DiagnosticsBoard>();
    open(&mut app, spec(1));
    settle(&mut app);
    let world = line(&app, "world").expect("a world line");
    assert!(!world.starts_with("chunks=0 "), "{world}");
    assert!(line(&app, "light").is_some());
    close(&mut app);
    app.update();
    let world = line(&app, "world").expect("a world line");
    assert!(world.starts_with("chunks=0 "), "{world}");

    let dark = WorldSettings {
        light: false,
        ..settings()
    };
    open(&mut app, WorldSpec::new(Arc::new(Hills { seed: 2 }), dark));
    settle(&mut app);
    assert_eq!(line(&app, "light"), None);
}
