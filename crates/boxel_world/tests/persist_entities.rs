//! Saved entities (ADR 0024 section 3) in headless apps with inline jobs, through a test codec:
//! an entity is put away when its chunk unloads and comes back once when it loads (near the
//! origin, across ±2^31, at 2^40 and next to the world limit, also in a layer the generator
//! declares all air); it survives a restart; an entity that moved is saved under its new chunk
//! only; a despawned one loses its record; one put where nothing is loaded waits for its chunk;
//! references between saved entities come back; damaged records, another record version, a
//! codec version it cannot read and a codec that is gone leave the records as they are.
use boxel_app::App;
use boxel_ecs::change_detection::Tick;
use boxel_ecs::prelude::*;
use boxel_math::DVec3;
use boxel_transform::Transform;
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockState, CHUNK_SIZE, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos,
    LocalPos, MAX_BLOCK_COORD,
};
use boxel_world::jobs::InlineExecutor;
use boxel_world::persist::format::{EntityPart, decode_entity, encode_entity};
use boxel_world::persist::{
    ChunkStore, EntityCodec, MemoryStore, PersistPlugin, PersistSettings, SaveBatch, Saved,
    SavedId, SavedIds, WorldStore, add_entity_codec,
};
use boxel_world::{BlockTicks, ChunkViewer, EditQueue, WorldPlugin, WorldSettings, WorldStats};
use boxel_worldgen::WorldGenerator;
use std::io;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ the world

/// Stone below `level`, air from `level` up (the generator's top is the layer under `level`, so
/// the layers above are known air).
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

// ------------------------------------------------------------------ the test codecs

/// A test entity: where it is and a value.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
struct Thing {
    at: BlockPos,
    value: u64,
}

/// Saves [`Thing`]: 32 bytes. Version 1 only.
struct ThingCodec;

impl EntityCodec for ThingCodec {
    fn name(&self) -> &str {
        "test:thing"
    }
    fn version(&self) -> u32 {
        1
    }
    fn position(&self, world: &World, entity: Entity) -> Option<BlockPos> {
        world.get::<Thing>(entity).map(|t| t.at)
    }
    fn changed(&self, world: &World, entity: Entity, since: Tick) -> bool {
        let now = world.read_change_tick();
        world
            .entity(entity)
            .get_change_ticks::<Thing>()
            .is_some_and(|t| t.is_changed(since, now))
    }
    fn save(&self, world: &World, entity: Entity, _ids: &SavedIds<'_>) -> Option<Vec<u8>> {
        let t = world.get::<Thing>(entity)?;
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
        version: u32,
        bytes: &[u8],
        _ids: &SavedIds<'_>,
    ) -> Result<(), String> {
        if version != 1 {
            return Err(format!("thing version {version}"));
        }
        let b: [u8; 32] = bytes.try_into().map_err(|_| "thing length".to_string())?;
        let word = |i: usize| {
            let mut w = [0; 8];
            w.copy_from_slice(&b[i * 8..i * 8 + 8]);
            w
        };
        let at = BlockPos::new(
            i64::from_le_bytes(word(0)),
            i64::from_le_bytes(word(1)),
            i64::from_le_bytes(word(2)),
        );
        world.entity_mut(entity).insert(Thing {
            at,
            value: u64::from_le_bytes(word(3)),
        });
        Ok(())
    }
}

/// A reference to another saved entity.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
struct Link(Option<Entity>);

/// Saves [`Link`] as the saved id of its target (0: none).
struct LinkCodec;

impl EntityCodec for LinkCodec {
    fn name(&self) -> &str {
        "test:link"
    }
    fn version(&self) -> u32 {
        1
    }
    fn position(&self, _world: &World, _entity: Entity) -> Option<BlockPos> {
        None
    }
    fn changed(&self, world: &World, entity: Entity, since: Tick) -> bool {
        let now = world.read_change_tick();
        world
            .entity(entity)
            .get_change_ticks::<Link>()
            .is_some_and(|t| t.is_changed(since, now))
    }
    fn save(&self, world: &World, entity: Entity, ids: &SavedIds<'_>) -> Option<Vec<u8>> {
        let link = world.get::<Link>(entity)?;
        let id = link
            .0
            .and_then(|t| ids.id(world, t))
            .map_or(0, SavedId::to_bits);
        Some(id.to_le_bytes().to_vec())
    }
    fn load(
        &self,
        world: &mut World,
        entity: Entity,
        _version: u32,
        bytes: &[u8],
        ids: &SavedIds<'_>,
    ) -> Result<(), String> {
        let b: [u8; 8] = bytes.try_into().map_err(|_| "link length".to_string())?;
        let target = SavedId::from_bits(u64::from_le_bytes(b)).and_then(|id| ids.entity(id));
        world.entity_mut(entity).insert(Link(target));
        Ok(())
    }
}

// ------------------------------------------------------------------ the app

/// How a test app is made: the store and the ground level. With `codecs` off the app has no
/// codec at all (a run of a game without that plugin).
#[derive(Clone)]
struct Setup {
    backend: Arc<dyn ChunkStore>,
    level: i64,
    codecs: bool,
    /// `WorldSettings::vertical_bounds`.
    bounds: Option<RangeInclusive<i64>>,
}

impl Setup {
    fn new(backend: Arc<dyn ChunkStore>, level: i64) -> Self {
        Self {
            backend,
            level,
            codecs: true,
            bounds: None,
        }
    }

    /// Opens the world and starts an app with the viewer at `at`, settled.
    fn start(&self, at: BlockPos) -> (App, Entity) {
        let registry = Arc::new(BlockRegistry::default_blocks());
        let generator = Ground { level: self.level };
        let store = WorldStore::open(Arc::clone(&self.backend), &registry, generator.name())
            .expect("the world opens");
        let mut app = App::new();
        app.add_plugins(WorldPlugin {
            generator: Arc::new(generator),
            registry,
            executor: Arc::new(InlineExecutor),
            settings: WorldSettings {
                vertical_bounds: self.bounds.clone(),
                ..WorldSettings::default()
            },
        })
        .add_plugins(PersistPlugin {
            store,
            executor: Arc::new(InlineExecutor),
        });
        if self.codecs {
            add_entity_codec(&mut app, ThingCodec);
            add_entity_codec(&mut app, LinkCodec);
        }
        app.insert_resource(PersistSettings {
            autosave: None,
            retry_after: Duration::ZERO,
            ..PersistSettings::default()
        });
        let viewer = app
            .world_mut()
            .spawn((
                Transform::from_translation(center(at)),
                ChunkViewer::new(1).with_vertical_radius(1),
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

fn at(o: BlockPos, x: i64, y: i64, z: i64) -> BlockPos {
    BlockPos::new(o.x + x, o.y + y, o.z + z)
}

/// Updates until the world is settled and nothing waits to be written (60 s max).
fn settle(app: &mut App) {
    let start = Instant::now();
    loop {
        app.update();
        let stats = *app.world().resource::<WorldStats>();
        let saves = app.world().resource::<WorldStore>().stats();
        if stats.chunks > 0
            && stats.is_settled()
            && saves.pending == 0
            && saves.pending_entities == 0
        {
            // Two more: the entity step of the next frame, and the save job it starts.
            app.update();
            app.update();
            let saves = app.world().resource::<WorldStore>().stats();
            if saves.pending_entities == 0 {
                return;
            }
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

/// Every thing alive, sorted.
fn things(app: &mut App) -> Vec<Thing> {
    let mut q = app.world_mut().query::<&Thing>();
    let mut out: Vec<Thing> = q.iter(app.world()).copied().collect();
    out.sort_unstable_by_key(|t| t.value);
    out
}

/// Every saved entity record of the store: chunk, id and parts.
fn records(backend: &dyn ChunkStore) -> Vec<(ChunkKey, u64, Vec<EntityPart>)> {
    let mut out = Vec::new();
    let mut chunks: Vec<ChunkKey> = backend
        .entity_keys()
        .unwrap()
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    chunks.dedup();
    for chunk in chunks {
        for (id, bytes) in backend.load_entities(chunk).unwrap() {
            out.push((chunk, id, decode_entity(&bytes).expect("a good record")));
        }
    }
    out
}

fn chunk_key(p: BlockPos) -> ChunkKey {
    ChunkKey::new(ChunkPos::containing(p))
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

/// Things around `o`: across chunk borders, one in the layer above the ground (all air for the
/// generator) and two in one chunk.
fn scene(o: BlockPos) -> Vec<Thing> {
    [
        at(o, 1, 0, 1),
        at(o, -1, 0, -1),
        at(o, 2, 0, 1),
        at(o, 0, 40, 0),
        at(o, -20, 3, 17),
    ]
    .into_iter()
    .enumerate()
    .map(|(i, p)| Thing {
        at: p,
        value: 100 + i as u64,
    })
    .collect()
}

fn spawn_things(app: &mut App, things: &[Thing]) {
    for &t in things {
        app.world_mut().spawn((Saved::new(), t));
    }
}

// ------------------------------------------------------------------ the tests

#[test]
fn entities_come_back_once_after_unloads_near_and_far() {
    for o in origins() {
        let setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
        let (mut app, viewer) = setup.start(o);
        let scene = scene(o);
        spawn_things(&mut app, &scene);
        settle(&mut app);
        assert_eq!(things(&mut app), scene, "{o:?}: alive");
        assert!(
            records(setup.backend.as_ref()).is_empty(),
            "{o:?}: nothing saved yet"
        );
        for round in 0..3 {
            let away = at(o, 1 << 13, 0, 0);
            move_viewer(&mut app, viewer, away);
            assert!(things(&mut app).is_empty(), "{o:?} {round}: put away");
            let saved = records(setup.backend.as_ref());
            assert_eq!(saved.len(), scene.len(), "{o:?} {round}: one record each");
            for (chunk, _, _) in &saved {
                assert!(
                    scene.iter().any(|t| chunk_key(t.at) == *chunk),
                    "{o:?}: saved under its own chunk"
                );
            }
            move_viewer(&mut app, viewer, o);
            assert_eq!(things(&mut app), scene, "{o:?} {round}: back once");
        }
        let stats = app.world().resource::<WorldStore>().stats();
        assert_eq!(stats.loaded_entities, 3 * scene.len(), "{o:?}");
        assert_eq!(stats.entity_errors, 0, "{o:?}");
    }
}

#[test]
fn entities_survive_a_restart_and_keep_their_ids() {
    let o = origins()[2];
    let setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
    let (mut app, _) = setup.start(o);
    let scene = scene(o);
    spawn_things(&mut app, &scene);
    settle(&mut app);
    let ids = |app: &mut App| {
        let mut q = app.world_mut().query::<(&Thing, &Saved)>();
        let mut ids: Vec<(u64, SavedId)> = q
            .iter(app.world())
            .map(|(t, s)| (t.value, s.id().expect("an id")))
            .collect();
        ids.sort_unstable();
        ids
    };
    let before = ids(&mut app);
    app.shutdown();
    drop(app);
    let (mut app, _) = setup.start(o);
    assert_eq!(things(&mut app), scene, "back after the restart");
    assert_eq!(ids(&mut app), before, "same saved ids");
    // New ids never repeat an old one.
    app.world_mut()
        .spawn((Saved::new(), Thing { at: o, value: 999 }));
    settle(&mut app);
    let newest = ids(&mut app)
        .into_iter()
        .find(|(v, _)| *v == 999)
        .unwrap()
        .1;
    assert!(before.iter().all(|(_, id)| *id < newest), "{newest:?}");
    // A second restart without changes keeps everything.
    app.shutdown();
    drop(app);
    let (mut app, _) = setup.start(o);
    assert_eq!(things(&mut app).len(), scene.len() + 1);
}

#[test]
fn an_entity_that_moved_is_saved_under_its_new_chunk_only() {
    let o = BlockPos::new(0, 0, 0);
    let setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
    let (mut app, viewer) = setup.start(o);
    let first = Thing {
        at: at(o, 1, 0, 1),
        value: 7,
    };
    let e = app.world_mut().spawn((Saved::new(), first)).id();
    settle(&mut app);
    // Saved where it is (exit), then moved across a chunk border.
    app.world_mut().run_schedule(boxel_app::Shutdown);
    settle(&mut app);
    let saved = records(setup.backend.as_ref());
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].0, chunk_key(first.at));
    let moved = Thing {
        at: at(o, -1, 0, 1),
        value: 7,
    };
    *app.world_mut().get_mut::<Thing>(e).unwrap() = moved;
    move_viewer(&mut app, viewer, at(o, 1 << 13, 0, 0));
    let saved = records(setup.backend.as_ref());
    assert_eq!(saved.len(), 1, "one record: {saved:?}");
    assert_eq!(saved[0].0, chunk_key(moved.at), "under the new chunk");
    move_viewer(&mut app, viewer, o);
    assert_eq!(things(&mut app), vec![moved]);
}

#[test]
fn a_despawned_entity_loses_its_record() {
    let o = BlockPos::new(0, 0, 0);
    let setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
    let (mut app, viewer) = setup.start(o);
    spawn_things(&mut app, &scene(o));
    settle(&mut app);
    move_viewer(&mut app, viewer, at(o, 1 << 13, 0, 0));
    move_viewer(&mut app, viewer, o);
    assert_eq!(records(setup.backend.as_ref()).len(), 5);
    let victim = {
        let mut q = app.world_mut().query::<(Entity, &Thing)>();
        q.iter(app.world()).find(|(_, t)| t.value == 101).unwrap().0
    };
    app.world_mut().despawn(victim);
    // And a marker removed: not saved any more either.
    let unmarked = {
        let mut q = app.world_mut().query::<(Entity, &Thing)>();
        q.iter(app.world()).find(|(_, t)| t.value == 102).unwrap().0
    };
    app.world_mut().entity_mut(unmarked).remove::<Saved>();
    settle(&mut app);
    let saved = records(setup.backend.as_ref());
    assert_eq!(saved.len(), 3, "{saved:?}");
    move_viewer(&mut app, viewer, at(o, 1 << 13, 0, 0));
    move_viewer(&mut app, viewer, o);
    let values: Vec<u64> = things(&mut app).iter().map(|t| t.value).collect();
    // The unmarked one stayed alive (never put away), the despawned one never comes back.
    assert_eq!(values, vec![100, 102, 103, 104]);
}

#[test]
fn an_entity_where_nothing_is_loaded_waits_for_its_chunk() {
    let o = BlockPos::new(1 << 40, 0, 0);
    let setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
    let (mut app, viewer) = setup.start(o);
    let far = Thing {
        at: at(o, 1 << 12, 5, 0),
        value: 1,
    };
    app.world_mut().spawn((Saved::new(), far));
    settle(&mut app);
    assert!(things(&mut app).is_empty(), "put away at once");
    assert_eq!(records(setup.backend.as_ref())[0].0, chunk_key(far.at));
    move_viewer(&mut app, viewer, far.at);
    assert_eq!(things(&mut app), vec![far], "back with its chunk");
}

#[test]
fn references_between_saved_entities_come_back() {
    let o = BlockPos::new(0, 0, 0);
    let setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
    let (mut app, viewer) = setup.start(o);
    let a = Thing {
        at: at(o, 1, 0, 1),
        value: 1,
    };
    let b = Thing {
        at: at(o, 2, 0, 1),
        value: 2,
    };
    let ea = app.world_mut().spawn((Saved::new(), a)).id();
    app.world_mut().spawn((Saved::new(), b, Link(Some(ea))));
    // A link to an entity that is not saved is lost.
    let plain = app.world_mut().spawn_empty().id();
    app.world_mut().spawn((
        Saved::new(),
        Thing {
            at: at(o, 3, 0, 1),
            value: 3,
        },
        Link(Some(plain)),
    ));
    settle(&mut app);
    move_viewer(&mut app, viewer, at(o, 1 << 13, 0, 0));
    move_viewer(&mut app, viewer, o);
    let mut q = app.world_mut().query::<(Entity, &Thing, Option<&Link>)>();
    let all: Vec<(Entity, Thing, Option<Link>)> = q
        .iter(app.world())
        .map(|(e, t, l)| (e, *t, l.copied()))
        .collect();
    let find = |v: u64| all.iter().find(|(_, t, _)| t.value == v).unwrap();
    assert_eq!(find(2).2, Some(Link(Some(find(1).0))), "the link follows");
    assert_eq!(find(3).2, Some(Link(None)));
}

/// Writes `bytes` as the record of id `id` in `chunk`.
fn put_record(backend: &dyn ChunkStore, chunk: ChunkKey, id: u64, bytes: Vec<u8>) {
    backend
        .save_batch(&SaveBatch {
            entities: vec![(chunk, id, Some(bytes))],
            ..SaveBatch::default()
        })
        .unwrap();
}

#[test]
fn bad_records_stay_as_they_are() {
    let o = BlockPos::new(0, 0, 0);
    let backend: Arc<dyn ChunkStore> = Arc::new(MemoryStore::new());
    let thing = |value: u64| Thing {
        at: at(o, 1, 0, 1),
        value,
    };
    let part = |version: u32, value: u64| {
        let mut bytes = Vec::new();
        let t = thing(value);
        for v in [t.at.x, t.at.y, t.at.z] {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
        bytes.extend_from_slice(&value.to_le_bytes());
        EntityPart {
            codec: "test:thing".to_string(),
            version,
            bytes,
        }
    };
    let chunk = chunk_key(thing(0).at);
    // 1: good. 2: damaged (a bit flipped). 3: another record version. 4: a codec version the
    // codec cannot read. 5: good, with a part of a codec that is gone.
    put_record(backend.as_ref(), chunk, 1, encode_entity(&[part(1, 1)]));
    let mut damaged = encode_entity(&[part(1, 2)]);
    let last = damaged.len() - 1;
    damaged[last] ^= 1;
    put_record(backend.as_ref(), chunk, 2, damaged.clone());
    let mut newer = encode_entity(&[part(1, 3)]);
    newer[4] = 2;
    put_record(backend.as_ref(), chunk, 3, newer.clone());
    put_record(backend.as_ref(), chunk, 4, encode_entity(&[part(9, 4)]));
    let gone = EntityPart {
        codec: "test:gone".to_string(),
        version: 3,
        bytes: vec![1, 2, 3],
    };
    put_record(
        backend.as_ref(),
        chunk,
        5,
        encode_entity(&[part(1, 5), gone.clone()]),
    );

    let setup = Setup::new(Arc::clone(&backend), o.y);
    let (mut app, viewer) = setup.start(o);
    let values: Vec<u64> = things(&mut app).iter().map(|t| t.value).collect();
    assert_eq!(values, vec![1, 5]);
    assert_eq!(
        app.world().resource::<WorldStore>().stats().entity_errors,
        3,
        "damaged, newer and unreadable"
    );
    // Unload: the good ones are saved again, the bad ones stay as they were.
    move_viewer(&mut app, viewer, at(o, 1 << 13, 0, 0));
    let stored: Vec<(u64, Vec<u8>)> = backend.load_entities(chunk).unwrap();
    assert_eq!(stored.len(), 5);
    assert_eq!(stored[1].1, damaged);
    assert_eq!(stored[2].1, newer);
    assert_eq!(stored[3].1, encode_entity(&[part(9, 4)]));
    // The gone codec's part is written back with the entity.
    let five = decode_entity(&stored[4].1).unwrap();
    assert_eq!(five, vec![part(1, 5), gone]);
    // A run without the codecs brings nothing back and loses nothing.
    app.shutdown();
    drop(app);
    let bare = Setup {
        codecs: false,
        ..setup.clone()
    };
    let (mut app, _) = bare.start(o);
    assert!(things(&mut app).is_empty());
    app.shutdown();
    drop(app);
    assert_eq!(backend.load_entities(chunk).unwrap(), stored);
    let (mut app, _) = setup.start(o);
    let values: Vec<u64> = things(&mut app).iter().map(|t| t.value).collect();
    assert_eq!(values, vec![1, 5]);
}

/// A memory store that keeps a copy of every write transaction.
#[derive(Default)]
struct Logged {
    inner: MemoryStore,
    batches: Mutex<Vec<SaveBatch>>,
}

impl ChunkStore for Logged {
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
        self.batches.lock().unwrap().push(batch.clone());
        self.inner.save_batch(batch)
    }
    fn entity_keys(&self) -> io::Result<Vec<(ChunkKey, u64)>> {
        self.inner.entity_keys()
    }
    fn load_entities(&self, key: ChunkKey) -> io::Result<Vec<(u64, Vec<u8>)>> {
        self.inner.load_entities(key)
    }
}

/// The saved entities alive are one moment of the world with the edited chunks and the pending
/// ticks: they are saved in the same transaction, also when that snapshot is taken because an
/// edited chunk unloaded while block ticks wait (not only by the autosave).
#[test]
fn entities_are_saved_in_the_moment_of_the_chunks_and_ticks() {
    let o = BlockPos::new(0, 0, 0);
    let backend = Arc::new(Logged::default());
    let setup = Setup::new(Arc::clone(&backend) as Arc<dyn ChunkStore>, o.y);
    let (mut app, viewer) = setup.start(o);
    app.init_resource::<BlockTicks>();
    // A thing where a second viewer keeps the world loaded, an edit in a chunk that unloads,
    // and a tick waiting.
    let thing = Thing {
        at: at(o, 4096, 0, 1),
        value: 1,
    };
    app.world_mut().spawn((
        Transform::from_translation(center(thing.at)),
        ChunkViewer::new(1).with_vertical_radius(1),
    ));
    settle(&mut app);
    let e = app.world_mut().spawn((Saved::new(), thing)).id();
    settle(&mut app);
    let edited = at(o, 5, 0, 5);
    let stone = BlockRegistry::default_blocks()
        .by_name("boxel:stone")
        .unwrap();
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(edited, stone);
    app.world_mut()
        .resource_mut::<BlockTicks>()
        .schedule(edited, 100_000);
    settle(&mut app);
    app.world_mut().get_mut::<Thing>(e).unwrap().value = 2;
    move_viewer(&mut app, viewer, at(o, -4096, 0, 0));
    assert_eq!(things(&mut app).len(), 1, "the thing stayed loaded");
    let batches = backend.batches.lock().unwrap();
    let snapshot = batches
        .iter()
        .find(|b| b.meta.iter().any(|(n, _)| n == "boxel:ticks"))
        .expect("the unload saved the ticks");
    assert!(
        snapshot.chunks.iter().any(|(k, _)| *k == chunk_key(edited)),
        "with the edited chunk"
    );
    let record = snapshot
        .entities
        .iter()
        .find(|(c, _, _)| *c == chunk_key(thing.at))
        .and_then(|(_, _, bytes)| bytes.as_ref())
        .expect("and the thing alive");
    let part = &decode_entity(record).unwrap()[0];
    assert_eq!(part.bytes[24..], 2u64.to_le_bytes(), "as it is now");
}

/// Counts the records made (it saves nothing of its own and never says an entity changed).
struct Probe(Arc<AtomicUsize>);

impl EntityCodec for Probe {
    fn name(&self) -> &str {
        "test:probe"
    }
    fn version(&self) -> u32 {
        1
    }
    fn position(&self, _world: &World, _entity: Entity) -> Option<BlockPos> {
        None
    }
    fn changed(&self, _world: &World, _entity: Entity, _since: Tick) -> bool {
        false
    }
    fn save(&self, _world: &World, _entity: Entity, _ids: &SavedIds<'_>) -> Option<Vec<u8>> {
        self.0.fetch_add(1, Ordering::Relaxed);
        None
    }
    fn load(
        &self,
        _world: &mut World,
        _entity: Entity,
        _version: u32,
        _bytes: &[u8],
        _ids: &SavedIds<'_>,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// A component nothing saves.
#[derive(Component)]
struct Mark;

/// Runs the autosave a few times (every frame it can), then turns it off and settles.
fn autosaves(app: &mut App) {
    let settings = |autosave| PersistSettings {
        autosave,
        retry_after: Duration::ZERO,
        ..PersistSettings::default()
    };
    app.insert_resource(settings(Some(Duration::ZERO)));
    for _ in 0..8 {
        app.update();
    }
    app.insert_resource(settings(None));
    settle(app);
}

/// The autosave makes the record again only for the entities a codec says changed, or that
/// gained or lost a component: entities that did not change cost no encoding, however many
/// are alive. Putting away an entity whose record is current makes nothing either.
#[test]
fn the_autosave_makes_again_only_the_records_that_changed() {
    let o = BlockPos::new(1 << 40, 0, -(1 << 31));
    let setup = Setup::new(Arc::new(MemoryStore::new()), o.y);
    let (mut app, viewer) = setup.start(o);
    let made = Arc::new(AtomicUsize::new(0));
    add_entity_codec(&mut app, Probe(Arc::clone(&made)));
    let scene = scene(o);
    spawn_things(&mut app, &scene);
    settle(&mut app);
    let made = || made.load(Ordering::Relaxed);
    autosaves(&mut app);
    assert_eq!(made(), scene.len(), "each one once");
    autosaves(&mut app);
    assert_eq!(made(), scene.len(), "nothing changed");
    // One changes, one gains a component.
    let entity = |app: &mut App, value: u64| {
        let mut q = app.world_mut().query::<(Entity, &Thing)>();
        q.iter(app.world())
            .find(|(_, t)| t.value == value)
            .unwrap()
            .0
    };
    let (a, b) = (entity(&mut app, 100), entity(&mut app, 101));
    app.world_mut().get_mut::<Thing>(a).unwrap().value = 1100;
    app.world_mut().entity_mut(b).insert(Mark);
    autosaves(&mut app);
    assert_eq!(made(), scene.len() + 2);
    // Unloading keeps the current records as they are.
    move_viewer(&mut app, viewer, at(o, 1 << 13, 0, 0));
    assert!(things(&mut app).is_empty());
    assert_eq!(made(), scene.len() + 2);
    move_viewer(&mut app, viewer, o);
    assert_eq!(things(&mut app).len(), scene.len(), "all back");
}

/// With `WorldSettings::vertical_bounds`, the layers outside never load (physics reads them as
/// air): an entity above the top layer (thrown or shot up there) stays alive while the top
/// layer of its column is loaded, is saved under it, and comes back with it, near and far.
#[test]
fn an_entity_above_the_vertical_bounds_is_kept_with_the_top_layer() {
    for o in origins() {
        let top = o.y >> 5;
        let setup = Setup {
            bounds: Some(top - 1..=top),
            ..Setup::new(Arc::new(MemoryStore::new()), o.y)
        };
        let (mut app, viewer) = setup.start(o);
        let high = Thing {
            at: at(o, 3, 40, 1),
            value: 1,
        };
        let low = Thing {
            at: at(o, 3, -100, 1),
            value: 2,
        };
        spawn_things(&mut app, &[high, low]);
        settle(&mut app);
        assert_eq!(things(&mut app), vec![high, low], "{o:?}: alive");
        move_viewer(&mut app, viewer, at(o, 1 << 13, 0, 0));
        assert!(things(&mut app).is_empty(), "{o:?}: put away");
        let mut keys: Vec<ChunkKey> = records(setup.backend.as_ref())
            .into_iter()
            .map(|(c, _, _)| c)
            .collect();
        keys.sort_unstable_by_key(|k| k.pos.y);
        let column = |y: i64| ChunkKey::new(ChunkPos::new(o.x >> 5, y, o.z >> 5));
        assert_eq!(
            keys,
            vec![column(top - 1), column(top)],
            "{o:?}: under the layers in bounds"
        );
        move_viewer(&mut app, viewer, o);
        assert_eq!(things(&mut app), vec![high, low], "{o:?}: back");
    }
}
