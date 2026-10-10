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
use std::sync::atomic::{AtomicBool, Ordering};
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
    assert_eq!(store.write(&[k], &[], &[]).unwrap(), 1);
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
    store.write(&[k], &[], &[]).unwrap();
    // The old copy was written, the newer one waits for the next write.
    assert_eq!(store.stats().pending, 1);
    assert_eq!(
        store.read_chunk(k).unwrap().as_uniform(),
        Some(BlockState::AIR)
    );
    store.write(&[k], &[], &[]).unwrap();
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
    store.write(&staged, &[], &[]).unwrap();
    // Keys outside the world (a damaged store) are ignored.
    backend
        .save_batch(&SaveBatch {
            chunks: vec![(key(MAX_CHUNK_COORD + 1, 0, 0), vec![0])],
            meta: Vec::new(),
            ..SaveBatch::default()
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

/// A memory store that counts its write transactions.
#[derive(Default)]
struct Counting {
    inner: MemoryStore,
    batches: std::sync::atomic::AtomicUsize,
}

impl ChunkStore for Counting {
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
        self.batches.fetch_add(1, Ordering::SeqCst);
        self.inner.save_batch(batch)
    }
}

/// A job that writes the metadata (the ticks and the clock) writes every staged chunk with it, in
/// one transaction, whatever `max_batch` says; so does the exit.
#[test]
fn a_save_with_metadata_takes_every_staged_chunk() {
    let backend = Arc::new(Counting::default());
    let store = WorldStore::open(
        Arc::clone(&backend) as Arc<dyn ChunkStore>,
        &BlockRegistry::default_blocks(),
        "test",
    )
    .unwrap();
    let mut p = Persist::new(store, Arc::new(InlineExecutor));
    let batches = || backend.batches.load(Ordering::SeqCst);
    for x in 0..10 {
        p.stage(key(x, 0, 0), stone_chunk());
    }
    let (name, value) = store::clock_meta(77);
    p.set_meta(name, value);
    let before = batches();
    p.frame(Instant::now(), 4, Duration::ZERO);
    assert_eq!((backend.inner.len(), batches()), (10, before + 1));
    assert_eq!(p.queued(), 0);
    assert!(backend.load_meta("boxel:clock").unwrap().is_some());

    // At exit.
    for x in 10..20 {
        p.stage(key(x, 0, 0), stone_chunk());
    }
    let (name, value) = store::clock_meta(78);
    p.set_meta(name, value);
    let before = batches();
    p.save_now(4).unwrap();
    assert_eq!((backend.inner.len(), batches()), (20, before + 1));
    // Without metadata, batches of `max_batch`.
    for x in 20..30 {
        p.stage(key(x, 0, 0), stone_chunk());
    }
    let before = batches();
    p.save_now(4).unwrap();
    assert_eq!((backend.inner.len(), batches()), (30, before + 3));
}

/// A job that writes entity records writes every staged chunk with them, in one transaction,
/// whatever `max_batch` says: a body is never on the disk ahead of the blocks it rests on.
#[test]
fn a_save_with_entity_records_takes_every_staged_chunk() {
    let backend = Arc::new(Counting::default());
    let store = WorldStore::open(
        Arc::clone(&backend) as Arc<dyn ChunkStore>,
        &BlockRegistry::default_blocks(),
        "test",
    )
    .unwrap();
    let mut p = Persist::new(store, Arc::new(InlineExecutor));
    let batches = || backend.batches.load(Ordering::SeqCst);
    for x in 0..10 {
        p.stage(key(x, 0, 0), stone_chunk());
    }
    p.entities.record(&p.store, key(9, 0, 0), 1, vec![1]);
    let before = batches();
    p.frame(Instant::now(), 4, Duration::ZERO);
    assert_eq!(
        (backend.inner.len(), backend.inner.entity_count(), batches()),
        (10, 1, before + 1)
    );
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

/// A redb file on a disk that fails while `fail` is set (every read, write, sync and resize),
/// like a disk that is full or goes away for a moment. Locks go to the file, so a database that
/// is still open keeps the next one from opening.
#[derive(Debug)]
struct FailingDisk {
    file: redb::backends::FileBackend,
    fail: Arc<AtomicBool>,
}

impl FailingDisk {
    fn check(&self) -> std::io::Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            Err(std::io::Error::other("the disk failed"))
        } else {
            Ok(())
        }
    }
}

type Range = std::ops::Bound<u64>;

impl redb::StorageBackend for FailingDisk {
    fn len(&self) -> std::io::Result<u64> {
        self.file.len()
    }
    fn read(&self, offset: u64, out: &mut [u8]) -> std::io::Result<()> {
        self.check()?;
        self.file.read(offset, out)
    }
    fn set_len(&self, len: u64) -> std::io::Result<()> {
        self.check()?;
        self.file.set_len(len)
    }
    fn sync_data(&self) -> std::io::Result<()> {
        self.check()?;
        self.file.sync_data()
    }
    fn write(&self, offset: u64, data: &[u8]) -> std::io::Result<()> {
        self.check()?;
        self.file.write(offset, data)
    }
    fn close(&self) -> std::io::Result<()> {
        self.file.close()
    }
    fn try_lock_range(&self, start: Range, end: Range) -> Result<bool, redb::BackendError> {
        self.file.try_lock_range(start, end)
    }
    fn try_lock_shared_range(&self, start: Range, end: Range) -> Result<bool, redb::BackendError> {
        self.file.try_lock_shared_range(start, end)
    }
    fn lock_range(&self, start: Range, end: Range) -> Result<(), redb::BackendError> {
        self.file.lock_range(start, end)
    }
    fn lock_shared_range(&self, start: Range, end: Range) -> Result<(), redb::BackendError> {
        self.file.lock_shared_range(start, end)
    }
    fn unlock_range(&self, start: Range, end: Range) -> Result<(), redb::BackendError> {
        self.file.unlock_range(start, end)
    }
    fn query_lock_range(&self, start: Range, end: Range) -> Result<bool, redb::BackendError> {
        self.file.query_lock_range(start, end)
    }
}

/// A file for a redb test, removed when dropped.
struct TempFile(std::path::PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("boxel_persist_{name}_{}.redb", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Self(path)
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// A [`RedbStore`] of `path` on a [`FailingDisk`], without a page cache (every read goes to the
/// disk) and without a wait between two openings of the file.
fn failing_redb(path: &std::path::Path, fail: &Arc<AtomicBool>) -> RedbStore {
    let (path, fail) = (path.to_path_buf(), Arc::clone(fail));
    RedbStore::with_opener(move || {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let disk = FailingDisk {
            file: redb::backends::FileBackend::new(file)?,
            fail: Arc::clone(&fail),
        };
        redb::Database::builder()
            .set_cache_size(0)
            .create_with_backend(disk)
    })
    .expect("opens")
    .with_reopen_every(Duration::ZERO)
}

/// redb fails every operation after one IO error until the database is opened again: the store
/// opens it again, so saves and loads work once the disk does, and nothing written is lost.
#[test]
fn redb_works_again_after_the_disk_fails() {
    let tmp = TempFile::new("failing_disk");
    let fail = Arc::new(AtomicBool::new(false));
    let redb = failing_redb(&tmp.0, &fail);
    let record = |k: ChunkKey| format!("{:?}", k.pos).into_bytes();
    let (a, b) = (key(1, 2, 3), key(1 << 40, -(1 << 31), MAX_CHUNK_COORD));
    let batch = |k: ChunkKey| SaveBatch {
        chunks: vec![(k, record(k))],
        meta: Vec::new(),
        ..SaveBatch::default()
    };
    redb.save_batch(&batch(a)).unwrap();

    fail.store(true, Ordering::SeqCst);
    assert!(redb.save_batch(&batch(b)).is_err());
    assert!(redb.load(a).is_err());

    // The disk works again.
    fail.store(false, Ordering::SeqCst);
    assert_eq!(redb.load(a).unwrap(), Some(record(a)));
    redb.save_batch(&batch(b)).unwrap();
    assert_eq!(redb.load(b).unwrap(), Some(record(b)));
    drop(redb);
    let redb = RedbStore::open(&tmp.0).unwrap();
    assert_eq!(redb.load(a).unwrap(), Some(record(a)));
    assert_eq!(redb.load(b).unwrap(), Some(record(b)));
    drop(redb);

    // While the disk keeps failing, the file is not opened again before the wait.
    let redb = failing_redb(&tmp.0, &fail).with_reopen_every(Duration::from_secs(3600));
    fail.store(true, Ordering::SeqCst);
    assert!(redb.load(a).is_err());
    fail.store(false, Ordering::SeqCst);
    assert!(redb.load(a).is_err(), "not opened again yet");
}

/// The saving state on a redb file whose disk fails: the failed save waits in memory (loads read
/// it) and a later try writes it.
#[test]
fn a_failed_redb_save_is_written_once_the_disk_works() {
    let tmp = TempFile::new("failing_save");
    let fail = Arc::new(AtomicBool::new(false));
    let backend: Arc<dyn ChunkStore> = Arc::new(failing_redb(&tmp.0, &fail));
    let store = WorldStore::open(
        Arc::clone(&backend),
        &BlockRegistry::default_blocks(),
        "test",
    )
    .unwrap();
    let mut p = Persist::new(store, Arc::new(InlineExecutor));
    let k = key(-5, 7, 1 << 40);
    p.stage(k, stone_chunk());
    fail.store(true, Ordering::SeqCst);
    p.frame(Instant::now(), 256, Duration::ZERO);
    p.frame(Instant::now(), 256, Duration::ZERO);
    assert!(p.store.stats().save_errors >= 1);
    assert_eq!(p.store.stats().pending, 1);
    assert!(
        p.store.read_chunk(k).is_some(),
        "loads read the copy in memory"
    );

    // The disk works again: the next try writes it.
    fail.store(false, Ordering::SeqCst);
    p.frame(Instant::now(), 256, Duration::ZERO);
    p.frame(Instant::now(), 256, Duration::ZERO);
    assert_eq!(p.store.stats().pending, 0);
    p.save_now(256).unwrap();
    drop(p);
    drop(backend);
    let store = WorldStore::open(
        Arc::new(RedbStore::open(&tmp.0).unwrap()),
        &BlockRegistry::default_blocks(),
        "test",
    )
    .unwrap();
    let stone = BlockRegistry::default_blocks()
        .by_name("boxel:stone")
        .unwrap();
    assert_eq!(
        store.read_chunk(k).unwrap().get(LocalPos::new(4, 5, 6)),
        stone
    );
}

// ---- Entities ------------------------------------------------------------------------------

/// Entity records of `chunk` with ids `ids`, each record its id's bytes.
fn entity_batch(chunk: ChunkKey, ids: &[u64]) -> SaveBatch {
    SaveBatch {
        entities: ids
            .iter()
            .map(|&id| (chunk, id, Some(id.to_le_bytes().to_vec())))
            .collect(),
        ..SaveBatch::default()
    }
}

/// Both stores keep entity records by chunk and id, exact far from the origin: a chunk's load
/// reads its records only, and a removal removes one.
#[test]
fn stores_keep_entity_records_by_chunk() {
    let tmp = TempFile::new("entities");
    let stores: [Arc<dyn ChunkStore>; 2] = [
        Arc::new(MemoryStore::new()),
        Arc::new(RedbStore::open(&tmp.0).unwrap()),
    ];
    let far = 1 << 40;
    let chunks = [
        key(0, 0, 0),
        key(1, 0, 0),
        key(0, 0, 1),
        key(far, -far, MAX_CHUNK_COORD),
        key(-MAX_CHUNK_COORD, MAX_CHUNK_COORD, -(1 << 31)),
    ];
    for store in &stores {
        for (i, &c) in chunks.iter().enumerate() {
            let i = i as u64;
            store
                .save_batch(&entity_batch(c, &[i + 1, u64::MAX - i, 1000 + i]))
                .unwrap();
        }
        let mut keys = store.entity_keys().unwrap();
        keys.sort_unstable_by_key(|&(c, id)| (format::encode_key(c), id));
        assert_eq!(keys.len(), 3 * chunks.len());
        for (i, &c) in chunks.iter().enumerate() {
            let i = i as u64;
            let ids: Vec<u64> = store
                .load_entities(c)
                .unwrap()
                .into_iter()
                .map(|(id, bytes)| {
                    assert_eq!(bytes, id.to_le_bytes());
                    id
                })
                .collect();
            assert_eq!(ids, vec![i + 1, 1000 + i, u64::MAX - i], "{c:?}");
        }
        store
            .save_batch(&SaveBatch {
                entities: vec![(chunks[3], 1000 + 3, None)],
                ..SaveBatch::default()
            })
            .unwrap();
        assert_eq!(store.load_entities(chunks[3]).unwrap().len(), 2);
        assert_eq!(store.entity_keys().unwrap().len(), 3 * chunks.len() - 1);
        assert!(store.load_entities(key(2, 0, 0)).unwrap().is_empty());
    }
    // Kept in the file.
    drop(stores);
    let redb = RedbStore::open(&tmp.0).unwrap();
    assert_eq!(redb.entity_keys().unwrap().len(), 3 * chunks.len() - 1);
}

/// Staged entity records and removals are read over the stored ones until a write lands, and a
/// write drops only the staged entries it wrote. Records are staged as their content and stored
/// sealed (the save job seals them, the load opens them); a stored record that cannot be opened
/// is left out and counted.
#[test]
fn staged_entity_records_are_read_until_written() {
    let backend = Arc::new(MemoryStore::new());
    let c = key(3, -2, 1 << 40);
    let sealed = |id: u64| Some(format::seal_entity(&id.to_le_bytes()));
    backend
        .save_batch(&SaveBatch {
            entities: vec![(c, 3, sealed(3)), (c, 4, sealed(4)), (c, 6, Some(vec![6]))],
            ..SaveBatch::default()
        })
        .unwrap();
    let store = open(&backend);
    store.stage_entity(c, 5, Some(Arc::new(vec![5])));
    store.stage_entity(c, 3, None);
    let ids = |store: &WorldStore| -> Vec<u64> {
        store
            .load_entities(c)
            .unwrap()
            .into_iter()
            .map(|(id, bytes)| {
                let expected = if id == 5 {
                    vec![5]
                } else {
                    id.to_le_bytes().to_vec()
                };
                assert_eq!(bytes, expected, "the content of {id}");
                id
            })
            .collect()
    };
    assert_eq!(ids(&store), vec![4, 5]);
    assert_eq!(store.stats().entity_errors, 1, "6 is not sealed");
    assert_eq!(store.stats().pending_entities, 2);
    store
        .write(&[], &[], &store.entity_writes(&[(c, 5)]))
        .unwrap();
    assert_eq!(store.stats().pending_entities, 1, "the removal waits");
    assert_eq!(backend.entity_count(), 4);
    assert_eq!(
        backend.load_entities(c).unwrap()[2],
        (5, format::seal_entity(&[5])),
        "sealed by the write"
    );
    store
        .write(&[], &[], &store.entity_writes(&[(c, 3)]))
        .unwrap();
    assert_eq!(ids(&store), vec![4, 5]);
    assert_eq!(backend.entity_count(), 3);
    assert_eq!(store.stats().pending_entities, 0);
    assert_eq!(store.stats().saved_entities, 1);
}

/// The index at open: where each id is, which chunks hold entities, the next id past every
/// saved one and past the counter; moving an entity stages its removal from the old chunk.
#[test]
fn the_entity_index_follows_records_and_ids() {
    let backend = Arc::new(MemoryStore::new());
    let (a, b) = (key(0, 0, 0), key(-(1 << 31), 5, 1 << 40));
    backend.save_batch(&entity_batch(a, &[2, 9])).unwrap();
    backend.save_batch(&entity_batch(b, &[4])).unwrap();
    let store = open(&backend);
    let mut p = Persist::new(store, Arc::new(InlineExecutor));
    assert!(p.has_entities(a) && p.has_entities(b) && !p.has_entities(key(1, 0, 0)));
    assert_eq!(p.entities.at(9), Some(a));
    // Moves 9 to b: one removal, one record, both in the next job.
    p.entities.record(&p.store, b, 9, vec![9]);
    // The same record again stages nothing.
    p.entities.record(&p.store, b, 9, vec![9]);
    assert_eq!(p.store.stats().pending_entities, 2);
    p.entities.forget(&p.store, 2);
    assert!(!p.has_entities(a), "a holds nothing now");
    p.frame(Instant::now(), 256, Duration::ZERO);
    assert_eq!(p.store.stats().pending_entities, 0);
    let mut keys = backend.entity_keys().unwrap();
    keys.sort_unstable_by_key(|&(_, id)| id);
    assert_eq!(keys, vec![(b, 4), (b, 9)]);
    // The counter wins when it is past every record.
    backend
        .save_batch(&SaveBatch {
            meta: vec![("boxel:entities".to_string(), format::encode_clock(77))],
            ..SaveBatch::default()
        })
        .unwrap();
    drop(p);
    let mut p = Persist::new(open(&backend), Arc::new(InlineExecutor));
    assert_eq!(p.entities.take_next_id(), None, "nothing new yet");
    assert_eq!(p.entities.at(9), Some(b));
    p.entities.next_id_unsaved();
    assert_eq!(p.entities.take_next_id(), Some(77));
}

/// Runs jobs when the test says so (a busy IO pool).
#[derive(Default)]
struct Held(std::sync::Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>);

impl crate::jobs::JobExecutor for Held {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>) {
        self.0.lock().unwrap().push(job);
    }
}

impl Held {
    fn run(&self) {
        let jobs = std::mem::take(&mut *self.0.lock().unwrap());
        for job in jobs {
            job();
        }
    }
}

/// A job writes the entity records as they were staged when it was made: an entity that moves
/// while the job waits to run is on the disk once, under its old chunk, until the next job
/// writes its move (the removal and the new record together).
#[test]
fn an_entity_moved_while_its_job_waits_is_never_off_the_disk() {
    let backend = Arc::new(MemoryStore::new());
    let held = Arc::new(Held::default());
    let mut p = Persist::new(
        open(&backend),
        Arc::clone(&held) as Arc<dyn crate::jobs::JobExecutor>,
    );
    let (c, d, e) = (key(0, 0, 0), key(1, 0, 0), key(1 << 40, -(1 << 31), 2));
    let keys = || {
        let mut keys = backend.entity_keys().unwrap();
        keys.sort_unstable_by_key(|&(c, id)| (format::encode_key(c), id));
        keys
    };
    p.entities.record(&p.store, c, 7, vec![1]);
    p.frame(Instant::now(), 256, Duration::ZERO);
    // Moves to d before the job runs, then to e before the next one runs.
    p.entities.record(&p.store, d, 7, vec![2]);
    held.run();
    assert_eq!(keys(), vec![(c, 7)], "the job wrote what it took");
    p.frame(Instant::now(), 256, Duration::ZERO);
    p.entities.record(&p.store, e, 7, vec![3]);
    held.run();
    assert_eq!(keys(), vec![(d, 7)]);
    p.frame(Instant::now(), 256, Duration::ZERO);
    held.run();
    assert_eq!(keys(), vec![(e, 7)]);
    p.frame(Instant::now(), 256, Duration::ZERO);
    assert_eq!(p.store.stats().pending_entities, 0);
    assert_eq!(p.store.load_entities(e).unwrap(), vec![(7, vec![3])]);
}

/// A failed save keeps the entity records staged (loads read them) and writes them later.
#[test]
fn a_failed_entity_save_is_written_once_the_disk_works() {
    let tmp = TempFile::new("failing_entities");
    let fail = Arc::new(AtomicBool::new(false));
    let backend: Arc<dyn ChunkStore> = Arc::new(failing_redb(&tmp.0, &fail));
    let store = WorldStore::open(
        Arc::clone(&backend),
        &BlockRegistry::default_blocks(),
        "test",
    )
    .unwrap();
    let mut p = Persist::new(store, Arc::new(InlineExecutor));
    let c = key(-5, 7, 1 << 40);
    p.entities.record(&p.store, c, 1, vec![42]);
    fail.store(true, Ordering::SeqCst);
    p.frame(Instant::now(), 256, Duration::ZERO);
    p.frame(Instant::now(), 256, Duration::ZERO);
    assert!(p.store.stats().save_errors >= 1);
    assert_eq!(p.store.stats().pending_entities, 1);
    fail.store(false, Ordering::SeqCst);
    assert_eq!(p.store.load_entities(c).unwrap(), vec![(1, vec![42])]);
    p.frame(Instant::now(), 256, Duration::ZERO);
    p.frame(Instant::now(), 256, Duration::ZERO);
    assert_eq!(p.store.stats().pending_entities, 0);
    drop(p);
    drop(backend);
    let redb = RedbStore::open(&tmp.0).unwrap();
    assert_eq!(
        redb.load_entities(c).unwrap(),
        vec![(1, format::seal_entity(&[42]))]
    );
}
