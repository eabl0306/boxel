//! Where saved bytes live: the [`ChunkStore`] trait and its stores (none, memory, redb). A store
//! keeps bytes by key and knows nothing of their format ([`super::format`] does).
use super::format::{KEY_LEN, decode_key, encode_key};
use boxel_voxel::ChunkKey;
use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::Path;
use std::sync::Mutex;

/// What one [`ChunkStore::save_batch`] writes, atomically: chunk records and metadata values.
#[derive(Clone, Debug, Default)]
pub struct SaveBatch {
    /// Chunk records by key (a later entry of the same key wins).
    pub chunks: Vec<(ChunkKey, Vec<u8>)>,
    /// Metadata values by name (a later entry of the same name wins).
    pub meta: Vec<(String, Vec<u8>)>,
}

impl SaveBatch {
    /// Nothing to write.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty() && self.meta.is_empty()
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
    /// Saves chunk records and metadata atomically, as one batch: when it returns `Ok` the batch
    /// is durable (a crash keeps all of it), when it fails nothing of it was written.
    fn save_batch(&self, batch: &SaveBatch) -> io::Result<()>;
    /// Flushes pending writes.
    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
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
        // Both locks at once: a reader never sees half a batch.
        let mut chunks = self.chunks.lock().expect("store lock");
        let mut meta = self.meta.lock().expect("store lock");
        for (key, bytes) in &batch.chunks {
            chunks.insert(encode_key(*key), bytes.clone());
        }
        for (name, bytes) in &batch.meta {
            meta.insert(name.clone(), bytes.clone());
        }
        Ok(())
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

/// Page cache of a [`RedbStore`] (redb's default is 1 GiB): the chunks a game reads again soon
/// are the ones it just wrote or loaded, and the system's file cache holds the rest.
pub const REDB_CACHE_BYTES: usize = 64 << 20;

/// A world in one redb file (pure Rust, ACID). Every batch is one write transaction committed
/// with immediate durability; reads run in their own read transactions, in parallel with a
/// write. Tables: `chunks` (records by key), `index` (the same keys, to open fast) and `meta`.
pub struct RedbStore {
    db: redb::Database,
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
    /// that is not a redb database is an error, never overwritten.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let db = redb::Database::builder()
            .set_cache_size(REDB_CACHE_BYTES)
            .create(path)
            .map_err(io_error)?;
        // The tables exist from the start, so a read never meets a missing table.
        let txn = db.begin_write().map_err(io_error)?;
        txn.open_table(CHUNKS).map_err(io_error)?;
        txn.open_table(INDEX).map_err(io_error)?;
        txn.open_table(META).map_err(io_error)?;
        txn.commit().map_err(io_error)?;
        Ok(Self { db })
    }
}

impl ChunkStore for RedbStore {
    fn load(&self, key: ChunkKey) -> io::Result<Option<Vec<u8>>> {
        use redb::ReadableDatabase;
        let txn = self.db.begin_read().map_err(io_error)?;
        let table = txn.open_table(CHUNKS).map_err(io_error)?;
        let value = table.get(&encode_key(key)).map_err(io_error)?;
        Ok(value.map(|v| v.value().to_vec()))
    }
    fn keys(&self) -> io::Result<Vec<ChunkKey>> {
        use redb::{ReadableDatabase, ReadableTable};
        let txn = self.db.begin_read().map_err(io_error)?;
        let table = txn.open_table(INDEX).map_err(io_error)?;
        let mut keys = Vec::new();
        for entry in table.iter().map_err(io_error)? {
            let (key, _) = entry.map_err(io_error)?;
            keys.push(decode_key(key.value()));
        }
        Ok(keys)
    }
    fn load_meta(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        use redb::ReadableDatabase;
        let txn = self.db.begin_read().map_err(io_error)?;
        let table = txn.open_table(META).map_err(io_error)?;
        let value = table.get(name).map_err(io_error)?;
        Ok(value.map(|v| v.value().to_vec()))
    }
    fn save_batch(&self, batch: &SaveBatch) -> io::Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let txn = self.db.begin_write().map_err(io_error)?;
        {
            let mut chunks = txn.open_table(CHUNKS).map_err(io_error)?;
            let mut index = txn.open_table(INDEX).map_err(io_error)?;
            for (key, bytes) in &batch.chunks {
                let key = encode_key(*key);
                chunks.insert(&key, bytes.as_slice()).map_err(io_error)?;
                index.insert(&key, ()).map_err(io_error)?;
            }
            let mut meta = txn.open_table(META).map_err(io_error)?;
            for (name, bytes) in &batch.meta {
                meta.insert(name.as_str(), bytes.as_slice())
                    .map_err(io_error)?;
            }
        }
        // Dropping an uncommitted transaction aborts it: an error above writes nothing.
        txn.commit().map_err(io_error)
    }
}
