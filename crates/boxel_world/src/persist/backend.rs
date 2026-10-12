//! Where saved bytes live: the [`ChunkStore`] trait and its stores (none, memory, redb). A store
//! keeps bytes by key and knows nothing of their format ([`super::format`] does).
use super::format::{
    ENTITY_KEY_LEN, KEY_LEN, decode_entity_key, decode_key, encode_entity_key, encode_key,
};
use boxel_voxel::ChunkKey;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

/// What one [`ChunkStore::save_batch`] writes, atomically: chunk records, metadata values and
/// entity records.
#[derive(Clone, Debug, Default)]
pub struct SaveBatch {
    /// Chunk records by key (a later entry of the same key wins).
    pub chunks: Vec<(ChunkKey, Vec<u8>)>,
    /// Metadata values by name (a later entry of the same name wins).
    pub meta: Vec<(String, Vec<u8>)>,
    /// Entity records by the chunk that holds them and the entity's saved id: `Some` writes the
    /// record, `None` removes it (the entity moved to another chunk's record or is gone). A later
    /// entry of the same key wins.
    pub entities: Vec<(ChunkKey, u64, Option<Vec<u8>>)>,
}

impl SaveBatch {
    /// Nothing to write.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty() && self.meta.is_empty() && self.entities.is_empty()
    }
}

/// Chunk blob store. Implementations must be callable from worker threads: the world reads chunk
/// records inside generation and sky walk jobs, and writes from one save job at a time (redb
/// allows one write transaction at a time), so batching is the caller's job. Keys carry `i64`
/// chunk positions (up to the world limit, `boxel_voxel::MAX_CHUNK_COORD`): a store must keep
/// all 64 bits of every axis.
pub trait ChunkStore: Send + Sync + 'static {
    /// Loads one chunk record.
    fn load(&self, key: ChunkKey) -> io::Result<Option<Vec<u8>>>;
    /// The keys of every saved chunk (read once, when a world opens).
    fn keys(&self) -> io::Result<Vec<ChunkKey>>;
    /// Loads one metadata value.
    fn load_meta(&self, name: &str) -> io::Result<Option<Vec<u8>>>;
    /// Saves chunk records, metadata and entity records atomically, as one batch: when it
    /// returns `Ok` the batch is durable (a crash keeps all of it), when it fails nothing of it
    /// was written.
    fn save_batch(&self, batch: &SaveBatch) -> io::Result<()>;
    /// The keys of every saved entity: the chunk whose record holds it and its saved id (read
    /// once, when a world opens). A store that keeps no entities has none (the default); one
    /// that does implements this, [`ChunkStore::load_entities`] and the `entities` of
    /// [`SaveBatch`].
    fn entity_keys(&self) -> io::Result<Vec<(ChunkKey, u64)>> {
        Ok(Vec::new())
    }
    /// The entity records of chunk `key`, by saved id, ascending.
    fn load_entities(&self, key: ChunkKey) -> io::Result<Vec<(u64, Vec<u8>)>> {
        let _ = key;
        Ok(Vec::new())
    }
    /// Flushes pending writes.
    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
    /// Closes the store for good: waits for the operation in progress, writes what a clean
    /// shutdown writes and releases the file, whoever still holds the store. Every later
    /// operation fails with [`io::ErrorKind::NotConnected`]. Closing again does nothing. The
    /// default only flushes and keeps the store usable (a store in memory can be opened again).
    fn close(&self) -> io::Result<()> {
        self.flush()
    }
    /// The store's file was not closed cleanly (a crash) and was repaired when the store opened
    /// it. Opening logs nothing (a game may open its world before its logging is up):
    /// [`PersistPlugin`](super::PersistPlugin) logs it. A store without a file never is (the
    /// default).
    fn repaired(&self) -> bool {
        false
    }
}

/// The error of an operation on a closed store or world ([`ChunkStore::close`],
/// [`WorldStore::close`](super::WorldStore::close)).
pub(crate) fn closed_error() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "the world is closed")
}

/// Stores nothing: every chunk regenerates from the seed (the world without
/// [`PersistPlugin`](super::PersistPlugin)).
#[derive(Clone, Copy, Debug, Default)]
pub struct NullChunkStore;

impl ChunkStore for NullChunkStore {
    fn load(&self, _key: ChunkKey) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn keys(&self) -> io::Result<Vec<ChunkKey>> {
        Ok(Vec::new())
    }
    fn load_meta(&self, _name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn save_batch(&self, _batch: &SaveBatch) -> io::Result<()> {
        Ok(())
    }
}

/// Keeps everything in memory, behind a lock: tests, tools, and worlds that live as long as the
/// process (share it with an `Arc` to reopen the same world in a new `App`).
#[derive(Debug, Default)]
pub struct MemoryStore {
    chunks: Mutex<BTreeMap<[u8; KEY_LEN], Vec<u8>>>,
    meta: Mutex<HashMap<String, Vec<u8>>>,
    entities: Mutex<BTreeMap<[u8; ENTITY_KEY_LEN], Vec<u8>>>,
}

impl MemoryStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Saved chunk records.
    pub fn len(&self) -> usize {
        self.chunks.lock().expect("store lock").len()
    }

    /// No chunk saved.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Saved entity records.
    pub fn entity_count(&self) -> usize {
        self.entities.lock().expect("store lock").len()
    }
}

/// The range of the entity keys of chunk `key`: every saved id.
fn entity_range(key: ChunkKey) -> std::ops::RangeInclusive<[u8; ENTITY_KEY_LEN]> {
    encode_entity_key(key, 0)..=encode_entity_key(key, u64::MAX)
}

impl ChunkStore for MemoryStore {
    fn load(&self, key: ChunkKey) -> io::Result<Option<Vec<u8>>> {
        Ok(self
            .chunks
            .lock()
            .expect("store lock")
            .get(&encode_key(key))
            .cloned())
    }
    fn keys(&self) -> io::Result<Vec<ChunkKey>> {
        Ok(self
            .chunks
            .lock()
            .expect("store lock")
            .keys()
            .map(decode_key)
            .collect())
    }
    fn load_meta(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        Ok(self.meta.lock().expect("store lock").get(name).cloned())
    }
    fn save_batch(&self, batch: &SaveBatch) -> io::Result<()> {
        // Every lock at once: a reader never sees half a batch.
        let mut chunks = self.chunks.lock().expect("store lock");
        let mut meta = self.meta.lock().expect("store lock");
        let mut entities = self.entities.lock().expect("store lock");
        for (key, bytes) in &batch.chunks {
            chunks.insert(encode_key(*key), bytes.clone());
        }
        for (name, bytes) in &batch.meta {
            meta.insert(name.clone(), bytes.clone());
        }
        for (key, id, bytes) in &batch.entities {
            let k = encode_entity_key(*key, *id);
            match bytes {
                Some(bytes) => entities.insert(k, bytes.clone()),
                None => entities.remove(&k),
            };
        }
        Ok(())
    }
    fn entity_keys(&self) -> io::Result<Vec<(ChunkKey, u64)>> {
        Ok(self
            .entities
            .lock()
            .expect("store lock")
            .keys()
            .map(decode_entity_key)
            .collect())
    }
    fn load_entities(&self, key: ChunkKey) -> io::Result<Vec<(u64, Vec<u8>)>> {
        Ok(self
            .entities
            .lock()
            .expect("store lock")
            .range(entity_range(key))
            .map(|(k, v)| (decode_entity_key(k).1, v.clone()))
            .collect())
    }
}

/// Chunk records: encoded key (`format::encode_key`) to record.
const CHUNKS: redb::TableDefinition<'static, &[u8; KEY_LEN], &[u8]> =
    redb::TableDefinition::new("chunks");
/// The keys of `CHUNKS` alone, written in the same transactions: listing them when a world opens
/// reads a few bytes per chunk instead of every record.
const INDEX: redb::TableDefinition<'static, &[u8; KEY_LEN], ()> =
    redb::TableDefinition::new("index");
/// Metadata: name to value.
const META: redb::TableDefinition<'static, &str, &[u8]> = redb::TableDefinition::new("meta");
/// Entity records: encoded entity key (`format::encode_entity_key`: the chunk, then the saved
/// id) to record. A world written before entities were saved gets the table when it opens; a
/// build that does not know it leaves it alone.
const ENTITIES: redb::TableDefinition<'static, &[u8; ENTITY_KEY_LEN], &[u8]> =
    redb::TableDefinition::new("entities");
/// The keys of `ENTITIES` alone, written in the same transactions (like `INDEX`).
const ENTITY_INDEX: redb::TableDefinition<'static, &[u8; ENTITY_KEY_LEN], ()> =
    redb::TableDefinition::new("entity_index");

/// Page cache of a [`RedbStore`] (redb's default is 1 GiB): the chunks a game reads again soon
/// are the ones it just wrote or loaded, and the system's file cache holds the rest.
pub const REDB_CACHE_BYTES: usize = 64 << 20;

/// The shortest wait between two openings of a [`RedbStore`]'s file after errors: a disk that
/// keeps failing is not opened (and maybe repaired) again by every read.
pub(crate) const REOPEN_EVERY: Duration = Duration::from_secs(1);

/// A world in one redb file (pure Rust, ACID). Every batch is one write transaction committed
/// with immediate durability; reads run in their own read transactions, in parallel with a
/// write. Tables: `chunks` (records by key), `index` (the same keys, to open fast), `meta`,
/// `entities` (entity records by chunk and saved id) and `entity_index` (their keys).
///
/// After one IO error redb fails every operation until the database is opened again (it repairs
/// the file then). So after an error the store closes the database, opens the file again and
/// runs the operation once more: saves and loads work again as soon as the disk does (while it
/// keeps failing, the file is opened again at most once a second). Committed batches are never
/// lost.
///
/// [`ChunkStore::close`] closes the file cleanly at a moment the caller chooses, while clones of
/// the store still live (jobs that read it): nothing opens it again after that. Opening a file
/// that was not closed cleanly (a crash) repairs it ([`ChunkStore::repaired`]).
pub struct RedbStore {
    /// Opens the file again (the path, or a test's backend).
    open: Box<dyn Fn() -> Result<redb::Database, redb::DatabaseError> + Send + Sync>,
    /// Readers and writers share it; an opening takes it alone, so the old database is closed
    /// (and its file lock released) before the new one opens.
    db: RwLock<Handle>,
    /// [`REOPEN_EVERY`] (tests shorten it).
    reopen_every: Duration,
    /// The first opening repaired the file ([`ChunkStore::repaired`]).
    repaired: bool,
    /// Runs after an operation failed, between the read lock and the write lock (tests).
    #[cfg(test)]
    after_failure: Option<Box<dyn Fn(&RedbStore) + Send + Sync>>,
}

/// The open database of a [`RedbStore`].
struct Handle {
    /// `None` after an opening failed, and once closed.
    db: Option<redb::Database>,
    /// [`ChunkStore::close`] ran: every operation fails, and the file is never opened again.
    closed: bool,
    /// Bumped by every opening: an error seen on an older database does not close a newer one.
    epoch: u64,
    /// The last opening after an error.
    reopened: Option<Instant>,
}

impl std::fmt::Debug for RedbStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RedbStore")
    }
}

/// A redb error as an `io::Error` (the store trait's error).
fn io_error(e: impl Into<redb::Error>) -> io::Error {
    match e.into() {
        redb::Error::Io(e) => e,
        e => io::Error::other(e.to_string()),
    }
}

impl RedbStore {
    /// Opens the database file at `path`, or creates it (and its parent directories). A file
    /// that is not a redb database is an error, never overwritten. A file that was not closed
    /// cleanly is repaired, without a log line ([`ChunkStore::repaired`] tells).
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let path = path.to_path_buf();
        // redb runs its repair on a file it creates (an empty one) too: only a file that held a
        // world was repaired.
        let held = std::fs::metadata(&path).is_ok_and(|m| m.len() > 0);
        // Set by every opening that repairs the file; only the first one's is kept.
        let repairs = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&repairs);
        let mut store = Self::with_opener(move || {
            let seen = Arc::clone(&seen);
            redb::Database::builder()
                .set_cache_size(REDB_CACHE_BYTES)
                .set_repair_callback(move |_| seen.store(true, Ordering::Relaxed))
                .create(&path)
        })?;
        store.repaired = held && repairs.load(Ordering::Relaxed);
        Ok(store)
    }

    /// Opens the database `open` gives, and again with it after an error.
    pub(crate) fn with_opener(
        open: impl Fn() -> Result<redb::Database, redb::DatabaseError> + Send + Sync + 'static,
    ) -> io::Result<Self> {
        let db = open().map_err(io_error)?;
        // The tables exist from the start, so a read never meets a missing table.
        let txn = db.begin_write().map_err(io_error)?;
        txn.open_table(CHUNKS).map_err(io_error)?;
        txn.open_table(INDEX).map_err(io_error)?;
        txn.open_table(META).map_err(io_error)?;
        txn.open_table(ENTITIES).map_err(io_error)?;
        txn.open_table(ENTITY_INDEX).map_err(io_error)?;
        txn.commit().map_err(io_error)?;
        Ok(Self {
            open: Box::new(open),
            db: RwLock::new(Handle {
                db: Some(db),
                closed: false,
                epoch: 0,
                reopened: None,
            }),
            reopen_every: REOPEN_EVERY,
            repaired: false,
            #[cfg(test)]
            after_failure: None,
        })
    }

    /// Sets the shortest wait between two openings after errors (default [`REOPEN_EVERY`]).
    #[cfg(test)]
    pub(crate) fn with_reopen_every(mut self, every: Duration) -> Self {
        self.reopen_every = every;
        self
    }

    /// Runs `hook` after an operation failed, once its read lock is released and before it
    /// takes the write lock to open the file again.
    #[cfg(test)]
    pub(crate) fn with_after_failure(
        mut self,
        hook: impl Fn(&RedbStore) + Send + Sync + 'static,
    ) -> Self {
        self.after_failure = Some(Box::new(hook));
        self
    }

    /// Runs `op` on the database; after an error, opens the file again and runs it once more
    /// (see the [type docs](RedbStore)). `op` must leave nothing behind when it fails: a
    /// transaction that is not committed writes nothing.
    fn run<T>(&self, op: impl Fn(&redb::Database) -> Result<T, redb::Error>) -> io::Result<T> {
        let (epoch, error) = {
            let handle = self.db.read().unwrap_or_else(PoisonError::into_inner);
            if handle.closed {
                return Err(closed_error());
            }
            let error = match &handle.db {
                Some(db) => match op(db) {
                    Ok(value) => return Ok(value),
                    Err(e) => io_error(e),
                },
                None => io::Error::other("the world file could not be opened again"),
            };
            (handle.epoch, error)
        };
        #[cfg(test)]
        if let Some(hook) = &self.after_failure {
            hook(self);
        }
        let mut handle = self.db.write().unwrap_or_else(PoisonError::into_inner);
        // Closed meanwhile (the operation may have failed because of it): never opened again.
        if handle.closed {
            return Err(closed_error());
        }
        // Unless another thread opened it again meanwhile.
        if handle.epoch == epoch {
            if handle
                .reopened
                .is_some_and(|at| at.elapsed() < self.reopen_every)
            {
                return Err(error);
            }
            handle.epoch += 1;
            handle.reopened = Some(Instant::now());
            // Closed first: the file stays locked while it is open.
            handle.db = None;
            match (self.open)() {
                Ok(db) => {
                    tracing::info!("the world file was opened again after an error: {error}");
                    handle.db = Some(db);
                }
                Err(e) => {
                    tracing::debug!("the world file cannot be opened again yet: {e}");
                    return Err(error);
                }
            }
        }
        match &handle.db {
            Some(db) => op(db).map_err(io_error),
            None => Err(error),
        }
    }
}

impl ChunkStore for RedbStore {
    /// Takes the lock alone (so it waits for the operation in progress), marks the store closed
    /// and drops the database under it: redb writes its clean shutdown and releases the file
    /// lock before any other operation sees the handle again.
    fn close(&self) -> io::Result<()> {
        let mut handle = self.db.write().unwrap_or_else(PoisonError::into_inner);
        if handle.closed {
            return Ok(());
        }
        handle.closed = true;
        handle.epoch += 1;
        drop(handle.db.take());
        Ok(())
    }
    fn repaired(&self) -> bool {
        self.repaired
    }
    fn load(&self, key: ChunkKey) -> io::Result<Option<Vec<u8>>> {
        use redb::ReadableDatabase;
        let key = encode_key(key);
        self.run(|db| {
            let txn = db.begin_read()?;
            let table = txn.open_table(CHUNKS)?;
            Ok(table.get(&key)?.map(|v| v.value().to_vec()))
        })
    }
    fn keys(&self) -> io::Result<Vec<ChunkKey>> {
        use redb::{ReadableDatabase, ReadableTable};
        self.run(|db| {
            let txn = db.begin_read()?;
            let table = txn.open_table(INDEX)?;
            let mut keys = Vec::new();
            for entry in table.iter()? {
                keys.push(decode_key(entry?.0.value()));
            }
            Ok(keys)
        })
    }
    fn load_meta(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        use redb::ReadableDatabase;
        self.run(|db| {
            let txn = db.begin_read()?;
            let table = txn.open_table(META)?;
            Ok(table.get(name)?.map(|v| v.value().to_vec()))
        })
    }
    fn save_batch(&self, batch: &SaveBatch) -> io::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        self.run(|db| {
            let txn = db.begin_write()?;
            {
                let mut chunks = txn.open_table(CHUNKS)?;
                let mut index = txn.open_table(INDEX)?;
                for (key, bytes) in &batch.chunks {
                    let key = encode_key(*key);
                    chunks.insert(&key, bytes.as_slice())?;
                    index.insert(&key, ())?;
                }
                let mut meta = txn.open_table(META)?;
                for (name, bytes) in &batch.meta {
                    meta.insert(name.as_str(), bytes.as_slice())?;
                }
                let mut entities = txn.open_table(ENTITIES)?;
                let mut entity_index = txn.open_table(ENTITY_INDEX)?;
                for (key, id, bytes) in &batch.entities {
                    let key = encode_entity_key(*key, *id);
                    match bytes {
                        Some(bytes) => {
                            entities.insert(&key, bytes.as_slice())?;
                            entity_index.insert(&key, ())?;
                        }
                        None => {
                            entities.remove(&key)?;
                            entity_index.remove(&key)?;
                        }
                    }
                }
            }
            // Dropping an uncommitted transaction aborts it: an error above writes nothing.
            txn.commit()?;
            Ok(())
        })
    }
    fn entity_keys(&self) -> io::Result<Vec<(ChunkKey, u64)>> {
        use redb::{ReadableDatabase, ReadableTable};
        self.run(|db| {
            let txn = db.begin_read()?;
            let table = txn.open_table(ENTITY_INDEX)?;
            let mut keys = Vec::new();
            for entry in table.iter()? {
                keys.push(decode_entity_key(entry?.0.value()));
            }
            Ok(keys)
        })
    }
    fn load_entities(&self, key: ChunkKey) -> io::Result<Vec<(u64, Vec<u8>)>> {
        use redb::ReadableDatabase;
        let range = entity_range(key);
        self.run(|db| {
            let txn = db.begin_read()?;
            let table = txn.open_table(ENTITIES)?;
            let mut out = Vec::new();
            for entry in table.range::<&[u8; ENTITY_KEY_LEN]>(range.start()..=range.end())? {
                let (k, v) = entry?;
                out.push((decode_entity_key(k.value()).1, v.value().to_vec()));
            }
            Ok(out)
        })
    }
}
