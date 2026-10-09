//! Unit tests of the saving state: the stores, the index of saved chunks, the staged copies and
//! the rules the streaming code asks them about.
use super::format::tests::plain;
use super::*;
use crate::jobs::InlineExecutor;
use crate::ticks::BlockTicks;
use boxel_voxel::{
    BlockPos, BlockRegistry, BlockRegistryBuilder, BlockState, ChunkData, ChunkKey, ChunkPos,
    DimensionId, LocalPos, MAX_CHUNK_COORD,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn key(x: i64, y: i64, z: i64) -> ChunkKey {
    ChunkKey::new(ChunkPos::new(x, y, z))
}

fn open(backend: &Arc<MemoryStore>) -> WorldStore {
    WorldStore::open(
        Arc::clone(backend) as Arc<dyn ChunkStore>,
        &BlockRegistry::default_blocks(),
        "test",
    )
    .expect("opens")
}

fn stone_chunk() -> Arc<ChunkData> {
    let registry = BlockRegistry::default_blocks();
    let mut data = ChunkData::uniform(BlockState::AIR);
    data.set(
        LocalPos::new(4, 5, 6),
        registry.by_name("boxel:stone").unwrap(),
    );
    Arc::new(data)
}

#[test]
fn a_new_world_writes_its_version_id_map_and_generator_once() {
    let backend = Arc::new(MemoryStore::new());
    let store = open(&backend);
    assert_eq!(
        backend.load_meta("boxel:format").unwrap(),
        Some(FORMAT_VERSION.to_le_bytes().to_vec())
    );
    assert_eq!(
        backend.load_meta("boxel:generator").unwrap(),
        Some(b"test".to_vec())
    );
    let names = backend.load_meta("boxel:states").unwrap().unwrap();
    assert_eq!(
        format::decode_names(&names).unwrap(),
        store.ids().names().to_vec()
    );
    // Opening again with another generator keeps the first name (and only warns).
    drop(store);
    let store = WorldStore::open(
        Arc::clone(&backend) as Arc<dyn ChunkStore>,
        &BlockRegistry::default_blocks(),
        "another",
    )
    .unwrap();
    assert_eq!(
        backend.load_meta("boxel:generator").unwrap(),
        Some(b"test".to_vec())
    );
    // A registry with more states adds their names after the old ones.
    drop(store);
    let mut builder = BlockRegistryBuilder::with_default_blocks();
    builder.add_block(plain("test:gem")).unwrap();
    let bigger = builder.build();
    let store =
        WorldStore::open(Arc::clone(&backend) as Arc<dyn ChunkStore>, &bigger, "test").unwrap();
    let names = store.ids().names();
    assert_eq!(names.last().map(String::as_str), Some("test:gem"));
    assert_eq!(
        format::decode_names(&backend.load_meta("boxel:states").unwrap().unwrap()).unwrap(),
        names
    );
}

#[test]
fn game_metadata_is_kept_and_engine_names_are_refused() {
    let backend = Arc::new(MemoryStore::new());
    let store = open(&backend);
    store.put_meta("game:player", b"1,2,3".to_vec()).unwrap();
    assert_eq!(store.meta("game:player").unwrap(), Some(b"1,2,3".to_vec()));
    assert_eq!(store.meta("game:missing").unwrap(), None);
    let err = store
        .put_meta("boxel:format", b"x".to_vec())
        .expect_err("reserved");
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
}

#[test]
fn staged_copies_are_read_until_written_and_the_newest_is_written() {
    let backend = Arc::new(MemoryStore::new());
    let store = open(&backend);
    let k = key(1, -2, 3);
    assert!(store.read_chunk(k).is_none());
    let first = stone_chunk();
    store.stage(k, Arc::clone(&first));
    assert!(Arc::ptr_eq(&store.read_chunk(k).unwrap(), &first));
    assert_eq!(store.stats().pending, 1);
    // Staged again before the write: the newest copy is the one written.
    store.stage(k, Arc::new(ChunkData::uniform(BlockState::AIR)));
    assert_eq!(store.write(&[k], &[]).unwrap(), 1);
    assert_eq!(store.stats().pending, 0);
    let back = store.read_chunk(k).unwrap();
    assert_eq!(back.as_uniform(), Some(BlockState::AIR));
    assert_eq!(store.stats().saved_chunks, 1);
    assert_eq!(store.stats().loaded_chunks, 1);
}

/// A store whose writes stage a newer copy of a key in the middle of the write (as a chunk that
/// comes back, is edited and unloads again while its save runs).
struct Racing {
    inner: MemoryStore,
    store: std::sync::Mutex<Option<WorldStore>>,
    key: ChunkKey,
}

impl ChunkStore for Racing {
    fn load(&self, key: ChunkKey) -> std::io::Result<Option<Vec<u8>>> {
        self.inner.load(key)
    }
    fn keys(&self) -> std::io::Result<Vec<ChunkKey>> {
        self.inner.keys()
    }
    fn load_meta(&self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.inner.load_meta(name)
    }
    fn save_batch(&self, batch: &SaveBatch) -> std::io::Result<()> {
        if let Some(store) = self.store.lock().unwrap().take() {
            store.stage(self.key, Arc::new(ChunkData::uniform(BlockState::AIR)));
        }
        self.inner.save_batch(batch)
    }
}

#[test]
fn a_copy_staged_during_its_write_stays_staged() {
    let k = key(0, 0, 0);
    let racing = Arc::new(Racing {
        inner: MemoryStore::new(),
        store: std::sync::Mutex::new(None),
        key: k,
    });
    let store = WorldStore::open(
        Arc::clone(&racing) as Arc<dyn ChunkStore>,
        &BlockRegistry::default_blocks(),
        "test",
    )
    .unwrap();
    store.stage(k, stone_chunk());
    *racing.store.lock().unwrap() = Some(store.clone());
    store.write(&[k], &[]).unwrap();
    // The old copy was written, the newer one waits for the next write.
    assert_eq!(store.stats().pending, 1);
    assert_eq!(
        store.read_chunk(k).unwrap().as_uniform(),
        Some(BlockState::AIR)
    );
    store.write(&[k], &[]).unwrap();
    assert_eq!(store.stats().pending, 0);
}

#[test]
fn the_index_knows_saved_and_staged_chunks_by_column() {
    let backend = Arc::new(MemoryStore::new());
    let store = open(&backend);
    for k in [key(2, 5, -1), key(2, -7, -1), key(3, 0, 0)] {
        store.stage(k, stone_chunk());
    }
    let staged: Vec<ChunkKey> = vec![key(2, 5, -1), key(2, -7, -1), key(3, 0, 0)];
    store.write(&staged, &[]).unwrap();
    // Keys outside the world (a damaged store) are ignored.
    backend
        .save_batch(&SaveBatch {
            chunks: vec![(key(MAX_CHUNK_COORD + 1, 0, 0), vec![0])],
            meta: Vec::new(),
        })
        .unwrap();
    drop(store);
    let store = open(&backend);
    let mut p = Persist::new(store, Arc::new(InlineExecutor));
    assert_eq!(p.layers(DimensionId(0), 2, -1), &[-7, 5]);
    assert!(p.is_saved(key(3, 0, 0)));
    assert!(!p.is_saved(key(3, 1, 0)));
    assert!(!p.is_saved(key(MAX_CHUNK_COORD + 1, 0, 0)));
    assert!(!p.is_saved(ChunkKey {
        dim: DimensionId(1),
        ..key(3, 0, 0)
    }));
    p.stage(key(2, 0, -1), stone_chunk());
    assert_eq!(p.layers(DimensionId(0), 2, -1), &[-7, 0, 5]);
    assert_eq!(p.queued(), 1);
    // Staged twice before a job: written once.
    p.stage(key(2, 0, -1), stone_chunk());
    assert_eq!(p.queued(), 1);
    p.frame(Instant::now(), 256, Duration::ZERO);
    p.frame(Instant::now(), 256, Duration::ZERO);
    assert_eq!(p.queued(), 0);
    assert_eq!(p.store.stats().pending, 0);
    assert!(backend.load(key(2, 0, -1)).unwrap().is_some());
}

#[test]
fn save_jobs_take_at_most_a_batch() {
    let backend = Arc::new(MemoryStore::new());
    let mut p = Persist::new(open(&backend), Arc::new(InlineExecutor));
    for x in 0..10 {
        p.stage(key(x, 0, 0), stone_chunk());
    }
    p.frame(Instant::now(), 4, Duration::ZERO);
    assert_eq!(backend.len(), 4);
    assert_eq!(p.queued(), 6);
    // The report comes back in the next frame, which starts the next job.
    p.frame(Instant::now(), 4, Duration::ZERO);
    assert_eq!(backend.len(), 8);
    p.save_now(4).unwrap();
    assert_eq!(backend.len(), 10);
    assert_eq!(p.queued(), 0);
}

#[test]
fn pending_ticks_restore_in_order_relative_to_the_clock() {
    let mut ticks = BlockTicks::default();
    ticks.advance(100);
    let far = BlockPos::new(1 << 40, -(1 << 31), MAX_CHUNK_COORD << 5);
    ticks.schedule(BlockPos::new(0, 0, 0), 5);
    ticks.schedule(far, 2);
    ticks.schedule(BlockPos::new(1, 0, 0), 5);
    let saved = ticks.pending();
    assert_eq!(
        saved,
        vec![
            (far, 2),
            (BlockPos::new(0, 0, 0), 5),
            (BlockPos::new(1, 0, 0), 5)
        ]
    );
    // A new world whose clock starts elsewhere: the same delays, the same order.
    let mut restored = BlockTicks::default();
    restored.advance(7);
    restored.restore(saved.clone());
    assert_eq!(restored.pending(), saved);
    let blob = format::encode_ticks(&saved);
    assert_eq!(format::decode_ticks(&blob).unwrap(), saved);
}
