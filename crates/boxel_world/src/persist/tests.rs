//! Unit tests of the saving state: the stores, the index of saved chunks, the staged copies and
//! the rules the streaming code asks them about.
use super::format::tests::plain;
use super::*;
use crate::jobs::InlineExecutor;
use crate::ticks::BlockTicks;
use boxel_voxel::{
    BlockPos, BlockState, ChunkData, ChunkKey, ChunkPos, DimensionId, LocalPos, MAX_CHUNK_COORD,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

fn key(x: i64, y: i64, z: i64) -> ChunkKey {
    ChunkKey::new(ChunkPos::new(x, y, z))
}

fn open(backend: &Arc<MemoryStore>) -> WorldStore {
    WorldStore::open(
        Arc::clone(backend) as Arc<dyn ChunkStore>,
        &boxel_demo::blocks::registry(),
        "test",
    )
    .expect("opens")
}

fn stone_chunk() -> Arc<ChunkData> {
    let registry = boxel_demo::blocks::registry();
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
        &boxel_demo::blocks::registry(),
        "another",
    )
    .unwrap();
    assert_eq!(
        backend.load_meta("boxel:generator").unwrap(),
        Some(b"test".to_vec())
    );
    // A registry with more states adds their names after the old ones.
    drop(store);
    let mut builder = boxel_demo::blocks::builder();
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
    store: Mutex<Option<WorldStore>>,
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
        store: Mutex::new(None),
        key: k,
    });
    let store = WorldStore::open(
        Arc::clone(&racing) as Arc<dyn ChunkStore>,
        &boxel_demo::blocks::registry(),
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
    batches: AtomicUsize,
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
        &boxel_demo::blocks::registry(),
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
        &boxel_demo::blocks::registry(),
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

/// Holds a disk's reads while it is shut: an operation in progress, for as long as a test says.
#[derive(Debug, Default)]
struct Gate {
    shut: Mutex<bool>,
    opened: Condvar,
    /// Told when a read stops at the gate.
    waiting: Mutex<Option<mpsc::Sender<()>>>,
}

impl Gate {
    /// Shuts the gate: the receiver hears when a read stops at it.
    fn shut(&self) -> mpsc::Receiver<()> {
        let (tx, rx) = mpsc::channel();
        *self.waiting.lock().unwrap() = Some(tx);
        *self.shut.lock().unwrap() = true;
        rx
    }

    fn open(&self) {
        *self.shut.lock().unwrap() = false;
        self.opened.notify_all();
    }

    /// Waits while the gate is shut.
    fn pass(&self) {
        let mut shut = self.shut.lock().unwrap();
        if *shut && let Some(tx) = self.waiting.lock().unwrap().take() {
            let _ = tx.send(());
        }
        while *shut {
            shut = self.opened.wait(shut).unwrap();
        }
    }
}

/// A redb file on a disk that fails while `fail` is set (every read, write, sync and resize),
/// like a disk that is full or goes away for a moment, and whose reads wait at `gate`. Locks go
/// to the file, so a database that is still open keeps the next one from opening.
#[derive(Debug)]
struct FailingDisk {
    file: redb::backends::FileBackend,
    fail: Arc<AtomicBool>,
    gate: Arc<Gate>,
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
        self.gate.pass();
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

/// Opens `path` on a [`FailingDisk`], without a page cache (every read goes to the disk).
fn disk_opener(
    path: &std::path::Path,
    fail: &Arc<AtomicBool>,
    gate: &Arc<Gate>,
) -> impl Fn() -> Result<redb::Database, redb::DatabaseError> + Send + Sync + 'static {
    let (path, fail, gate) = (path.to_path_buf(), Arc::clone(fail), Arc::clone(gate));
    move || {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        let disk = FailingDisk {
            file: redb::backends::FileBackend::new(file)?,
            fail: Arc::clone(&fail),
            gate: Arc::clone(&gate),
        };
        redb::Database::builder()
            .set_cache_size(0)
            .create_with_backend(disk)
    }
}

/// A [`RedbStore`] of `path` on a [`FailingDisk`] ([`disk_opener`]), without a wait between two
/// openings of the file.
fn failing_redb(path: &std::path::Path, fail: &Arc<AtomicBool>) -> RedbStore {
    RedbStore::with_opener(disk_opener(path, fail, &Arc::default()))
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
        &boxel_demo::blocks::registry(),
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
        &boxel_demo::blocks::registry(),
        "test",
    )
    .unwrap();
    let stone = boxel_demo::blocks::registry()
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
struct Held(Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>);

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
        &boxel_demo::blocks::registry(),
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

// ---- Closing a world -----------------------------------------------------------------------

/// Keeps the message of every event at `INFO` or above logged on the thread it is the
/// subscriber of (`tracing::subscriber::with_default`).
#[derive(Clone, Default)]
struct Logged(Arc<Mutex<Vec<String>>>);

impl Logged {
    fn lines(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// The `message` field of an event.
struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for Logged {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        *metadata.level() <= tracing::Level::INFO
    }
    fn new_span(&self, _span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.lock().unwrap().push(message.0);
    }
    fn enter(&self, _span: &tracing::span::Id) {}
    fn exit(&self, _span: &tracing::span::Id) {}
}

/// A world closed while another thread holds a clone of it (a job): the close does not wait for
/// the clone, every operation of the clone then fails with the closed error, without a log line
/// or a count, what was staged is dropped, staging keeps nothing and closing again does nothing.
/// A redb store underneath is closed too (its file is released); a store in memory stays usable.
#[test]
fn a_closed_world_fails_every_operation_quietly() {
    let tmp = TempFile::new("closed_world");
    let backends: [Arc<dyn ChunkStore>; 2] = [
        Arc::new(MemoryStore::new()),
        Arc::new(RedbStore::open(&tmp.0).unwrap()),
    ];
    let (k, far) = (key(1, 2, 3), key(1 << 40, -(1 << 31), MAX_CHUNK_COORD));
    for (redb, backend) in [false, true].into_iter().zip(backends) {
        let store = WorldStore::open(
            Arc::clone(&backend),
            &boxel_demo::blocks::registry(),
            "test",
        )
        .unwrap();
        // Saved: a chunk, an entity record and a game value. Staged: a chunk and a record.
        store.stage(k, stone_chunk());
        store.stage_entity(k, 1, Some(Arc::new(vec![1])));
        store
            .write(&[k], &[], &store.entity_writes(&[(k, 1)]))
            .unwrap();
        store.put_meta("game:player", b"1".to_vec()).unwrap();
        store.stage(far, stone_chunk());
        store.stage_entity(far, 2, Some(Arc::new(vec![2])));
        let before = store.stats();
        assert_eq!((before.pending, before.pending_entities), (1, 1));

        let (closed, wait) = mpsc::channel::<()>();
        let clone = store.clone();
        let job = std::thread::spawn(move || {
            wait.recv().unwrap();
            let logged = Logged::default();
            let kinds = tracing::subscriber::with_default(logged.clone(), || {
                assert!(clone.read_chunk(k).is_none());
                [
                    clone.load_chunk(k).map(drop),
                    clone.load_chunk(far).map(drop),
                    clone.load_entities(k).map(drop),
                    clone.load_entities(far).map(drop),
                    clone.meta("game:player").map(drop),
                    clone.put_meta("game:player", b"2".to_vec()),
                    clone.write(&[far], &[], &[]).map(drop),
                ]
                .map(|r| r.map_err(|e| e.kind()))
            });
            (kinds, logged.lines())
        });
        store.close().unwrap();
        assert!(store.is_closed());
        closed.send(()).unwrap();
        let (kinds, lines) = job.join().unwrap();
        assert!(
            kinds
                .iter()
                .all(|k| *k == Err(std::io::ErrorKind::NotConnected)),
            "{kinds:?}"
        );
        assert!(lines.is_empty(), "{lines:?}");
        assert_eq!(
            store.stats(),
            PersistStats {
                pending: 0,
                pending_entities: 0,
                ..before
            },
            "the staged copies are dropped, nothing is counted"
        );
        store.stage(k, stone_chunk());
        store.stage_entity(k, 3, None);
        assert_eq!(
            (store.stats().pending, store.stats().pending_entities),
            (0, 0)
        );
        store.close().unwrap();

        if redb {
            let e = backend.load(k).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::NotConnected);
            // Released while the closed store and its clones live.
            let again = RedbStore::open(&tmp.0).unwrap();
            assert!(again.load(k).unwrap().is_some());
            assert_eq!(again.load_meta("game:player").unwrap(), Some(b"1".to_vec()));
            assert!(again.load(far).unwrap().is_none(), "dropped, never written");
        } else {
            assert!(backend.load(k).unwrap().is_some());
            let again = WorldStore::open(backend, &boxel_demo::blocks::registry(), "test")
                .expect("a store in memory opens again");
            assert!(again.read_chunk(k).is_some());
        }
    }
}

/// A reader whose operation failed before the close takes the write lock after it: it gets the
/// closed error and never opens the file again.
#[test]
fn a_reader_that_failed_before_the_close_never_opens_the_file_again() {
    let tmp = TempFile::new("closed_reopen");
    let fail = Arc::new(AtomicBool::new(false));
    let opens = Arc::new(AtomicUsize::new(0));
    let opener = disk_opener(&tmp.0, &fail, &Arc::default());
    let counted = {
        let opens = Arc::clone(&opens);
        move || {
            opens.fetch_add(1, Ordering::SeqCst);
            opener()
        }
    };
    // The close lands between the failed read and its write lock.
    let redb = RedbStore::with_opener(counted)
        .unwrap()
        .with_reopen_every(Duration::ZERO)
        .with_after_failure(|redb| redb.close().unwrap());
    let a = key(1 << 40, -(1 << 31), MAX_CHUNK_COORD);
    redb.save_batch(&SaveBatch {
        chunks: vec![(a, vec![7])],
        ..SaveBatch::default()
    })
    .unwrap();
    fail.store(true, Ordering::SeqCst);
    assert_eq!(
        redb.load(a).unwrap_err().kind(),
        std::io::ErrorKind::NotConnected
    );
    fail.store(false, Ordering::SeqCst);
    assert_eq!(
        redb.load(a).unwrap_err().kind(),
        std::io::ErrorKind::NotConnected
    );
    assert_eq!(opens.load(Ordering::SeqCst), 1, "never opened again");
    // The file is released, and the committed record is there.
    let again = RedbStore::open(&tmp.0).unwrap();
    assert_eq!(again.load(a).unwrap(), Some(vec![7]));
}

/// The close waits for the operation in progress (operations run under the read lock), which
/// ends well; the operations after it get the closed error.
#[test]
fn closing_redb_waits_for_the_operation_in_progress() {
    let tmp = TempFile::new("close_waits");
    let gate = Arc::new(Gate::default());
    let redb = Arc::new(
        RedbStore::with_opener(disk_opener(&tmp.0, &Arc::default(), &gate)).expect("opens"),
    );
    let a = key(-(1 << 40), 3, 1 << 31);
    redb.save_batch(&SaveBatch {
        chunks: vec![(a, vec![9])],
        ..SaveBatch::default()
    })
    .unwrap();
    let waiting = gate.shut();
    let reader = {
        let redb = Arc::clone(&redb);
        std::thread::spawn(move || redb.load(a))
    };
    waiting
        .recv_timeout(Duration::from_secs(10))
        .expect("the read reaches the disk");
    let (done, closed) = mpsc::channel();
    let closer = {
        let redb = Arc::clone(&redb);
        std::thread::spawn(move || done.send(redb.close()).unwrap())
    };
    assert!(
        matches!(
            closed.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "the close waits for the read"
    );
    gate.open();
    assert_eq!(reader.join().unwrap().unwrap(), Some(vec![9]));
    closed.recv().unwrap().unwrap();
    closer.join().unwrap();
    assert_eq!(
        redb.load(a).unwrap_err().kind(),
        std::io::ErrorKind::NotConnected
    );
    let again = RedbStore::open(&tmp.0).unwrap();
    assert_eq!(again.load(a).unwrap(), Some(vec![9]));
}

/// Clones are handles of one opening; another opening of the same world is not the same and
/// comes later.
#[test]
fn handles_of_one_opening_are_the_same() {
    let backend = Arc::new(MemoryStore::new());
    let first = open(&backend);
    let clone = first.clone();
    assert!(first.same(&clone));
    assert_eq!(first.handles(), 2);
    drop(clone);
    assert_eq!(first.handles(), 1);
    let second = open(&backend);
    assert!(!first.same(&second));
    assert!(second.opened_seq() > first.opened_seq());
}

/// A store in memory whose index read stops at `gate` once it has the keys: an opening that has
/// its index and has not finished yet.
struct SlowIndex {
    inner: Arc<MemoryStore>,
    gate: Arc<Gate>,
}

impl ChunkStore for SlowIndex {
    fn load(&self, key: ChunkKey) -> std::io::Result<Option<Vec<u8>>> {
        self.inner.load(key)
    }
    fn keys(&self) -> std::io::Result<Vec<ChunkKey>> {
        let keys = self.inner.keys();
        self.gate.pass();
        keys
    }
    fn load_meta(&self, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        self.inner.load_meta(name)
    }
    fn save_batch(&self, batch: &SaveBatch) -> std::io::Result<()> {
        self.inner.save_batch(batch)
    }
    fn entity_keys(&self) -> std::io::Result<Vec<(ChunkKey, u64)>> {
        self.inner.entity_keys()
    }
    fn load_entities(&self, key: ChunkKey) -> std::io::Result<Vec<(u64, Vec<u8>)>> {
        self.inner.load_entities(key)
    }
}

/// An opening is numbered before it reads its index: one that read it before a world's last
/// save and finished after that world closed comes before every opening after the close, so a
/// check against the number at the close tells its index is stale.
#[test]
fn an_opening_is_numbered_before_it_reads_its_index() {
    let backend = Arc::new(MemoryStore::new());
    let world = open(&backend);
    let gate = Arc::new(Gate::default());
    let indexed = gate.shut();
    let early = {
        let slow = Arc::new(SlowIndex {
            inner: Arc::clone(&backend),
            gate: Arc::clone(&gate),
        });
        std::thread::spawn(move || {
            WorldStore::open(slow, &boxel_demo::blocks::registry(), "test").expect("opens")
        })
    };
    indexed.recv().unwrap();
    // The world's last save and its close, while that opening holds an index without them.
    let edited = key(1, 2, 3);
    world.stage(edited, stone_chunk());
    world.write(&[edited], &[], &[]).unwrap();
    world.close().unwrap();
    let later = open(&backend);
    gate.open();
    let early = early.join().unwrap();
    let has_edited = |store: &WorldStore| {
        let opened = store.take_opened().unwrap();
        let column = (edited.dim, edited.pos.x, edited.pos.z);
        let saved = opened
            .saved
            .get(&column)
            .is_some_and(|l| l.contains(&edited.pos.y));
        assert_eq!(
            opened.lod.len(),
            usize::from(saved),
            "the far LOD's index agrees"
        );
        saved
    };
    assert!(!has_edited(&early));
    assert!(has_edited(&later));
    assert!(
        early.opened_seq() < later.opened_seq(),
        "the opening with the stale index comes first"
    );
}

/// Test 19: a close whose last save keeps failing (the disk fails) gives up after
/// `PersistSettings::close_timeout`, says so in its report, closes the file all the same and
/// ends `Closed`, without a panic; the batches written before stay.
#[test]
fn a_close_whose_last_save_fails_gives_up_and_closes_the_file() {
    use crate::lifecycle::{
        CloseOptions, WorldLifecycle, WorldSpec, WorldStatus, close_world, finish_closing,
        open_world,
    };
    use crate::tests::support::{LayerGen, fake_mesh};
    use crate::{ChunkMap, ChunkViewer, EditQueue, WorldPlugin, WorldSettings, WorldStats};
    use boxel_app::App;
    use boxel_math::DVec3;
    use boxel_transform::Transform;

    let tmp = TempFile::new("close_fails");
    let fail = Arc::new(AtomicBool::new(false));
    let registry = Arc::new(boxel_demo::blocks::registry());
    let stone = registry.by_name("boxel:stone").unwrap();
    let store = WorldStore::open(
        Arc::new(failing_redb(&tmp.0, &fail)) as Arc<dyn ChunkStore>,
        &registry,
        "layers",
    )
    .unwrap();
    let mut app = App::new();
    app.add_plugins(WorldPlugin {
        executor: Arc::new(InlineExecutor),
        ..WorldPlugin::closed(Arc::clone(&registry))
    })
    .add_plugins(PersistPlugin::without_store().with_executor(Arc::new(InlineExecutor)))
    .insert_resource(PersistSettings {
        autosave: None,
        retry_after: Duration::from_millis(5),
        close_timeout: Duration::from_millis(60),
        ..PersistSettings::default()
    });
    app.world_mut()
        .resource_mut::<crate::streaming::WorldJobs>()
        .mesh_fn = fake_mesh;
    app.update();
    let spec = WorldSpec::new(
        Arc::new(LayerGen::new(-2..=1)),
        WorldSettings {
            vertical_bounds: Some(-2..=1),
            ..WorldSettings::default()
        },
    );
    open_world(app.world_mut(), spec.with_store(store)).unwrap();
    app.world_mut().spawn((
        Transform::from_translation(DVec3::new(16.0, 16.0, 16.0)),
        ChunkViewer::new(1).with_vertical_radius(1),
    ));
    let settle = |app: &mut App| {
        for _ in 0..200 {
            app.update();
            let stats = *app.world().resource::<WorldStats>();
            if stats.chunks > 0 && stats.is_settled() {
                return;
            }
        }
        panic!("not settled");
    };
    settle(&mut app);
    let (kept, lost) = (BlockPos::new(3, 40, 3), BlockPos::new(5, 41, 5));
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(kept, stone);
    settle(&mut app);
    app.world_mut()
        .resource_mut::<WorldLifecycle>()
        .request_save();
    for _ in 0..3 {
        app.update();
    }
    assert_eq!(app.world().resource::<WorldStore>().stats().saved_chunks, 1);
    app.world_mut()
        .resource_mut::<EditQueue>()
        .set_block(lost, stone);
    settle(&mut app);
    assert_eq!(
        app.world().resource::<ChunkMap>().block_at(lost),
        Some(stone)
    );

    fail.store(true, Ordering::SeqCst);
    assert!(close_world(app.world_mut(), CloseOptions::default()));
    assert!(finish_closing(app.world_mut(), Duration::from_secs(5)));
    let lifecycle = app.world().resource::<WorldLifecycle>();
    assert_eq!(lifecycle.status(), WorldStatus::Closed);
    let report = lifecycle.last_report().unwrap();
    assert!(report.gave_up && !report.saved, "{report:?}");
    assert!(!report.errors.is_empty());

    fail.store(false, Ordering::SeqCst);
    let reopened = RedbStore::open(&tmp.0).expect("the file is closed");
    let keys = reopened.keys().unwrap();
    assert!(keys.contains(&ChunkKey::new(ChunkPos::containing(kept))));
}

// ---- The world generation record (ADR 0039 sec. 17) -----------------------------------------

/// The live record of a test world: a flat world made through preset `test:flat`.
fn worldgen_record(layers: u32) -> boxel_worldgen::preset::WorldgenRecord {
    test_worldgen()
        .open(
            &boxel_worldgen::GenId::new("test:flat").unwrap(),
            u64::from(layers),
            &boxel_demo::blocks::registry(),
            None,
        )
        .expect("opens")
        .record
}

/// A test app of one legacy preset, `test:flat`: the demo's heightmap (`test:terrain`) made from
/// the seed, as worlds were made before presets.
fn test_worldgen() -> boxel_worldgen::preset::Worldgen {
    use boxel_worldgen::GenId;
    use boxel_worldgen::preset::{
        DimensionDef, GeneratorKind, GeneratorSpec, WorldPreset, WorldgenBuilder,
    };
    let id = |name: &str| GenId::new(name).unwrap();
    let mut builder = WorldgenBuilder::new();
    builder
        .preset(
            id("test:flat"),
            WorldPreset::one(
                id("test:overworld"),
                GeneratorSpec::new(id("test:terrain"), ()),
            ),
        )
        .dimension(id("test:overworld"), DimensionDef::default())
        .generator(
            id("test:terrain"),
            GeneratorKind::legacy(1, |seed| {
                Arc::new(boxel_demo::worldgen::TerrainGenerator::new(
                    seed,
                    boxel_demo::worldgen::TerrainSettings::default(),
                ))
            }),
        );
    let mut checks = boxel_worldgen::registry::Checks::new();
    builder.freeze(&mut checks, ()).expect("frozen")
}

#[test]
fn a_new_world_saves_its_worldgen_record_once_without_a_new_format() {
    let backend = Arc::new(MemoryStore::new());
    let record = worldgen_record(3);
    let store = WorldStore::open(
        Arc::clone(&backend) as Arc<dyn ChunkStore>,
        &boxel_demo::blocks::registry(),
        GeneratorInfo::new("terrain").with_worldgen(&record),
    )
    .unwrap();
    assert_eq!(store.worldgen(), &SavedWorldgen::Record(record.clone()));
    assert_eq!(FORMAT_VERSION, 1, "no new format for the record");
    assert_eq!(
        backend.load_meta("boxel:format").unwrap(),
        Some(FORMAT_VERSION.to_le_bytes().to_vec())
    );
    let bytes = backend.load_meta("boxel:worldgen").unwrap().expect("saved");
    assert_eq!(format::decode_worldgen(&bytes).unwrap(), record);
    // The engine's name: a game cannot write it.
    assert_eq!(
        store
            .put_meta("boxel:worldgen", b"x".to_vec())
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
    // Opened again with another live record (the registries changed), the saved one stays: it
    // is what the world was made with.
    drop(store);
    let other = worldgen_record(4);
    let store = WorldStore::open(
        Arc::clone(&backend) as Arc<dyn ChunkStore>,
        &boxel_demo::blocks::registry(),
        GeneratorInfo::new("terrain").with_worldgen(&other),
    )
    .unwrap();
    assert_eq!(store.worldgen(), &SavedWorldgen::Record(record.clone()));
    assert_eq!(
        SavedWorldgen::read(&*backend).unwrap().record(),
        Some(&record)
    );
    assert_eq!(backend.load_meta("boxel:worldgen").unwrap(), Some(bytes));
}

#[test]
fn a_world_without_the_record_opens_as_legacy_with_its_old_generator() {
    use boxel_worldgen::{GenId, WorldGenerator};
    let registry = boxel_demo::blocks::registry();
    let seed = 7;
    let old = boxel_demo::worldgen::TerrainGenerator::new(
        seed,
        boxel_demo::worldgen::TerrainSettings::default(),
    );
    // A world made before records: the store opened with the generator's name alone, a chunk
    // edited and saved.
    let backend = Arc::new(MemoryStore::new());
    let store = WorldStore::open(
        Arc::clone(&backend) as Arc<dyn ChunkStore>,
        &registry,
        old.name(),
    )
    .unwrap();
    let edited = key(0, 1, 0);
    store.stage(edited, stone_chunk());
    store.write(&[edited], &[], &[]).unwrap();
    drop(store);
    assert_eq!(backend.load_meta("boxel:worldgen").unwrap(), None);

    // Reopened the way a game with presets opens it: the saved record first (none), then the
    // preset through its legacy factory, then the store with the live record.
    let saved = SavedWorldgen::read(&*backend).unwrap();
    assert_eq!(saved, SavedWorldgen::Legacy);
    let opened = test_worldgen()
        .open(
            &GenId::new("test:flat").unwrap(),
            seed,
            &registry,
            saved.record(),
        )
        .expect("opens");
    assert!(
        opened.diff.is_none(),
        "a legacy world has nothing to compare"
    );
    let generator = &opened.first().generator;
    let store = WorldStore::open(
        Arc::clone(&backend) as Arc<dyn ChunkStore>,
        &registry,
        GeneratorInfo::new(generator.name()).with_worldgen(&opened.record),
    )
    .unwrap();
    // It stays legacy: no record written into it, its generator's name unchanged.
    assert_eq!(store.worldgen(), &SavedWorldgen::Legacy);
    assert_eq!(backend.load_meta("boxel:worldgen").unwrap(), None);
    assert_eq!(
        backend.load_meta("boxel:generator").unwrap(),
        Some(old.name().as_bytes().to_vec())
    );
    // The same generator: the same chunks, and the saved one still there.
    for k in [key(0, 0, 0), key(3, 1, -2), key(-1, 2, 5), key(9, -1, 9)] {
        assert_eq!(
            generator.generate(k, &registry),
            old.generate(k, &registry),
            "{k:?}"
        );
    }
    assert_eq!(
        store
            .read_chunk(edited)
            .unwrap()
            .get(LocalPos::new(4, 5, 6)),
        registry.by_name("boxel:stone").unwrap()
    );
}

#[test]
fn an_unreadable_worldgen_record_opens_the_world_without_one() {
    let backend = Arc::new(MemoryStore::new());
    drop(open(&backend));
    // A record of a newer layout (or damaged): the world opens, the record is not read.
    let mut newer = format::encode_worldgen(&worldgen_record(3));
    newer[4] += 1;
    backend
        .save_batch(&SaveBatch {
            meta: vec![("boxel:worldgen".to_string(), newer)],
            ..SaveBatch::default()
        })
        .unwrap();
    let store = open(&backend);
    assert!(
        matches!(
            store.worldgen(),
            SavedWorldgen::Unreadable(FormatError::Version { .. })
        ),
        "{:?}",
        store.worldgen()
    );
    assert_eq!(store.worldgen().record(), None);
}
