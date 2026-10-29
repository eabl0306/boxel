//! The generation cache (ADR 0042): what the generator made, kept on disk so a place visited
//! before comes back without generating it again. It is never part of the world: it lives in its
//! own file ([`CACHE_FILE`], next to the world's), deleting it loses nothing, and the world stays
//! seed, generator and edits (ADR 0024). Edited chunks keep going to the world's file and always
//! win: the chunk job reads the saved copy first and the cache only for a chunk nobody saved.
//!
//! - **What it keeps.** A generated chunk's storage exactly as the generator returned it (no
//!   compaction, its version too), so a hit is the very [`ChunkData`] a generation gives; chunks
//!   of uniform air are left out (generating them is nearly free). And the far LOD's source
//!   output (ADR 0031): a column's band and every node it emitted, in its order, with their
//!   cells. Never a mesh, never light.
//! - **Stamps.** Every record carries a stamp of the generator's name and version (the chunk
//!   generator's, or its LOD source's), a hash of the registry's state names in id order and the
//!   game's salt ([`GenCacheSettings::salt`]). A record of another stamp, a damaged one or a
//!   missing one is a miss: the job generates and writes the record again. Blocks are stored by
//!   their runtime ids, which the registry hash pins.
//! - **Writes** never stall a frame or a job: the job that generated encodes its record and
//!   offers it to a bounded queue ([`GenCacheSettings::queue`]); when the queue is full the
//!   record is dropped. One writer thread per cache drains it in batches, one write
//!   transaction each without fsync, and makes them durable every few seconds, on
//!   [`GenCache::flush`] and when it closes. A crash loses the last records, never the world's.
//! - **The cap.** Records are grouped by region (32 by 32 chunk columns; a LOD node by the
//!   region of its corner). The writer keeps each region's estimated size on disk and when it was
//!   last used; past [`GenCacheSettings::max_bytes`] it drops whole regions: first the ones whose
//!   last write had a stamp this run never used, then the ones used longest ago.
//!
//! The mechanism is the engine's; what to stamp beyond the generator (the seed, the build), the
//! cap and whether it is on are the game's.
use super::format::{
    CACHE_BAND_MAGIC, CACHE_CHUNK_MAGIC, CACHE_NODE_MAGIC, FormatError, Reader, checksum, open,
    ordered, put_storage, put_varint, read_storage, seal,
};
use boxel_voxel::{BlockRegistry, BlockState, ChunkData, ChunkKey, DimensionId};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{
    LOD_PADDED_VOLUME, LodCells, LodColumn, LodNode, LodRequest, LodScratch,
};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};
use redb::{ReadableDatabase, ReadableTable, TableDefinition};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

/// The cache's file in a world directory ([`GenCache::open_dir`]).
pub const CACHE_FILE: &str = "cache.redb";

/// Version of the cache's layout (tables and keys). A file of another version is emptied when it
/// opens: nothing in it is worth migrating.
const CACHE_FORMAT: u32 = 1;
/// Version of the cache's record layouts.
const RECORD_VERSION: u8 = 1;

/// Bytes of a region key: dimension (2), region x and z (8 each).
const REGION_KEY_LEN: usize = 18;
/// Bytes of a record key: its region, what it is (1), its lod (1) and x, z, y (8 each).
const KEY_LEN: usize = REGION_KEY_LEN + 2 + 24;
/// Bytes of a record's stamp, before its envelope.
const STAMP_LEN: usize = 8;
/// A region is `2^REGION_SHIFT` chunk columns on a side (1,024 blocks).
const REGION_SHIFT: u32 = 5;

/// Records: key to stamp and envelope.
const RECORDS: TableDefinition<'static, &[u8; KEY_LEN], &[u8]> = TableDefinition::new("records");
/// Regions: key to when one was last used, its estimated size and the stamp of its last write.
const REGIONS: TableDefinition<'static, &[u8; REGION_KEY_LEN], &[u8]> =
    TableDefinition::new("regions");
/// The layout version (`format`).
const META: TableDefinition<'static, &str, &[u8]> = TableDefinition::new("meta");

/// redb's page cache of a cache file: a hit is read once per visit, so it holds little.
const PAGE_CACHE_BYTES: usize = 16 << 20;
/// Records a write transaction takes at most.
const BATCH: usize = 512;
/// The longest the writer leaves written records without making them durable.
const DURABLE_EVERY: Duration = Duration::from_secs(5);
/// Estimated bytes on disk of a record besides its value and key: redb's tree, pages and slack
/// (measured on a TFC world: about 600 bytes for a tiny record, 40 % more for a big one).
const RECORD_OVERHEAD: u64 = 600;
/// When the cap is passed, regions go until the cache is this fraction of it (so the next
/// region written does not evict again at once).
const EVICT_TO: f64 = 0.9;

/// What a record is: the third part of its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Chunk = 0,
    Node = 1,
    Band = 2,
}

/// Knobs of a [`GenCache`], fixed when it opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GenCacheSettings {
    /// Mixed into every record's stamp with the generator's name and version and the registry:
    /// whatever else the generated terrain depends on that the generator's version does not say
    /// (the world's seed; in a development build, the build). A cache file holds records of one
    /// world, but a salt that names the seed keeps a new world in an old folder from reading its
    /// records.
    pub salt: u64,
    /// Keeps generated chunks. Default true.
    pub chunks: bool,
    /// Keeps the far LOD's source output. Default true.
    pub lod: bool,
    /// The cap, in estimated bytes on disk: past it whole regions are dropped (module docs). The
    /// file never shrinks by itself (redb reuses its free pages); [`GenCache::clear`] compacts
    /// it. Default 1 GiB.
    pub max_bytes: u64,
    /// Records (one chunk, or one LOD column) waiting for the writer at most: one more is
    /// dropped. Default 1,024.
    pub queue: usize,
}

impl Default for GenCacheSettings {
    fn default() -> Self {
        Self {
            salt: 0,
            chunks: true,
            lod: true,
            max_bytes: 1 << 30,
            queue: 1024,
        }
    }
}

/// Counts of a [`GenCache`] since it opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GenCacheStats {
    /// Chunks read from the cache instead of generated.
    pub chunk_hits: u64,
    /// Chunks the cache did not have with the right stamp (generated, and offered back).
    pub chunk_misses: u64,
    /// LOD columns read from the cache instead of the generator's source.
    pub lod_hits: u64,
    /// LOD columns the cache did not have whole with the right stamp.
    pub lod_misses: u64,
    /// Records offered to the writer (chunks, and LOD columns as one).
    pub offered: u64,
    /// Records dropped because the writer's queue was full.
    pub dropped: u64,
    /// Records written to the file.
    pub written: u64,
    /// Records that could not be read (damaged, or of another layout): misses.
    pub damaged: u64,
    /// Reads and writes the file refused (each one a miss, or a batch dropped).
    pub errors: u64,
    /// Regions dropped for the cap.
    pub evicted: u64,
    /// The estimated size on disk of every record held.
    pub bytes: u64,
}

/// A region: dimension, and region x and z.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Region {
    dim: DimensionId,
    x: i64,
    z: i64,
}

impl Region {
    /// The region of chunk column (or LOD node column of level `lod`) `x`, `z`.
    fn of(dim: DimensionId, lod: u8, x: i64, z: i64) -> Self {
        let shift = |c: i64| c.wrapping_shl(u32::from(lod)) >> REGION_SHIFT;
        Self {
            dim,
            x: shift(x),
            z: shift(z),
        }
    }

    fn key(self) -> [u8; REGION_KEY_LEN] {
        let mut out = [0; REGION_KEY_LEN];
        out[..2].copy_from_slice(&self.dim.0.to_be_bytes());
        out[2..10].copy_from_slice(&ordered(self.x));
        out[10..18].copy_from_slice(&ordered(self.z));
        out
    }

    /// The first and the last record key of the region.
    fn range(self) -> ([u8; KEY_LEN], [u8; KEY_LEN]) {
        let mut lo = [0; KEY_LEN];
        lo[..REGION_KEY_LEN].copy_from_slice(&self.key());
        let mut hi = [0xff; KEY_LEN];
        hi[..REGION_KEY_LEN].copy_from_slice(&self.key());
        (lo, hi)
    }

    fn from_key(k: &[u8; REGION_KEY_LEN]) -> Self {
        let axis = |at: usize| {
            let mut b = [0; 8];
            b.copy_from_slice(&k[at..at + 8]);
            super::format::unordered(b)
        };
        Self {
            dim: DimensionId(u16::from_be_bytes([k[0], k[1]])),
            x: axis(2),
            z: axis(10),
        }
    }
}

/// The key of a record: its region first, so a region is one range of the table.
fn record_key(kind: Kind, dim: DimensionId, lod: u8, x: i64, y: i64, z: i64) -> [u8; KEY_LEN] {
    let mut out = [0; KEY_LEN];
    out[..REGION_KEY_LEN].copy_from_slice(&Region::of(dim, lod, x, z).key());
    out[REGION_KEY_LEN] = kind as u8;
    out[REGION_KEY_LEN + 1] = lod;
    let at = REGION_KEY_LEN + 2;
    out[at..at + 8].copy_from_slice(&ordered(x));
    out[at + 8..at + 16].copy_from_slice(&ordered(z));
    out[at + 16..at + 24].copy_from_slice(&ordered(y));
    out
}

/// What a region's entry holds.
#[derive(Clone, Copy, Debug, Default)]
struct RegionInfo {
    /// When it was last written or read: milliseconds since the Unix epoch, made to grow by
    /// at least one with every write of the writer (so the order of a run's writes is kept).
    used: u64,
    /// The estimated size on disk of its records.
    bytes: u64,
    /// The stamp of its last write or read.
    stamp: u64,
}

impl RegionInfo {
    fn encode(self) -> [u8; 24] {
        let mut out = [0; 24];
        out[..8].copy_from_slice(&self.used.to_le_bytes());
        out[8..16].copy_from_slice(&self.bytes.to_le_bytes());
        out[16..].copy_from_slice(&self.stamp.to_le_bytes());
        out
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let word = |at: usize| {
            bytes
                .get(at..at + 8)
                .map(|b| u64::from_le_bytes(b.try_into().expect("8 bytes")))
        };
        (bytes.len() == 24).then_some(())?;
        Some(Self {
            used: word(0)?,
            bytes: word(8)?,
            stamp: word(16)?,
        })
    }
}

/// The estimated size on disk of a record whose value is `len` bytes.
fn estimate(len: usize) -> u64 {
    len as u64 + KEY_LEN as u64 + RECORD_OVERHEAD + len as u64 * 2 / 5
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// One record for the writer.
struct Record {
    key: [u8; KEY_LEN],
    region: Region,
    stamp: u64,
    value: Vec<u8>,
}

/// What the writer is asked.
enum Op {
    /// Records to write, in one transaction with whatever comes with them.
    Put(Vec<Record>),
    /// A region was read with this stamp: it was used now.
    Touch(Region, u64),
    /// Makes everything before durable, then answers.
    Flush(Sender<io::Result<()>>),
    /// Drops every record and compacts the file, then answers.
    Clear(Sender<io::Result<()>>),
    /// Makes everything durable and ends.
    Stop,
}

/// What the readers and the writer share.
struct Shared {
    /// `None` once closed.
    db: RwLock<Option<redb::Database>>,
    chunk_hits: AtomicU64,
    chunk_misses: AtomicU64,
    lod_hits: AtomicU64,
    lod_misses: AtomicU64,
    offered: AtomicU64,
    dropped: AtomicU64,
    written: AtomicU64,
    damaged: AtomicU64,
    errors: AtomicU64,
    evicted: AtomicU64,
    bytes: AtomicU64,
    /// The first error was logged (the next ones only count).
    logged: AtomicBool,
}

impl Shared {
    fn error(&self, what: &str, e: impl std::fmt::Display) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        if !self.logged.swap(true, Ordering::Relaxed) {
            tracing::warn!("the generation cache {what}: {e} (later errors only count)");
        }
    }
}

struct Inner {
    shared: Arc<Shared>,
    settings: GenCacheSettings,
    /// FNV-1a of the registry's state names, in id order.
    registry: u64,
    /// States of the registry (a stored id past it is damage).
    states: usize,
    tx: Sender<Op>,
    writer: Mutex<Option<JoinHandle<()>>>,
    /// Regions read this run (each one tells the writer once).
    touched: Mutex<HashSet<Region, foldhash::fast::FixedState>>,
    closed: AtomicBool,
    /// [`GenCache::set_enabled`].
    enabled: AtomicBool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        stop(self);
    }
}

/// Stops the writer (it makes everything durable first) and closes the file.
fn stop(inner: &Inner) {
    if inner.closed.swap(true, Ordering::SeqCst) {
        return;
    }
    let writer = inner
        .writer
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(writer) = writer {
        // Err only if the writer is gone already.
        let _ = inner.tx.send(Op::Stop);
        if writer.join().is_err() {
            tracing::error!("the generation cache's writer panicked");
        }
    }
    drop(
        inner
            .shared
            .db
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take(),
    );
}

/// An open generation cache (module docs). Cheap to clone; read by the chunk and LOD jobs on
/// their workers, written by its own thread. Attach it to the world's store
/// ([`WorldStore::attach_cache`](super::WorldStore::attach_cache)) before the world opens; the
/// store's close closes it.
#[derive(Clone)]
pub struct GenCache {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for GenCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenCache")
            .field("settings", &self.inner.settings)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

impl GenCache {
    /// Opens the cache of the world in `dir` (the file [`CACHE_FILE`] there), creating both if
    /// needed (see [`GenCache::open`]).
    pub fn open_dir(
        dir: impl AsRef<Path>,
        registry: &BlockRegistry,
        settings: GenCacheSettings,
    ) -> io::Result<Self> {
        Self::open(dir.as_ref().join(CACHE_FILE), registry, settings)
    }

    /// Opens the cache file at `path`, or creates it. `registry` is the world's. A file that is
    /// not a cache this build reads (damaged, another layout, not a redb file) is emptied: the
    /// cache only ever loses records. Fails when the file cannot be opened at all (the disk, or
    /// another process holding it): the world then runs without a cache.
    pub fn open(
        path: impl AsRef<Path>,
        registry: &BlockRegistry,
        settings: GenCacheSettings,
    ) -> io::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let create = || {
            redb::Database::builder()
                .set_cache_size(PAGE_CACHE_BYTES)
                .create(path)
        };
        let db = match create() {
            Ok(db) => db,
            // Held by another process, or a disk that fails: no cache this run.
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => {
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    "the generation cache is open in another process",
                ));
            }
            Err(redb::DatabaseError::Storage(redb::StorageError::Io(e)))
                if e.kind() != io::ErrorKind::InvalidData =>
            {
                return Err(e);
            }
            Err(e) => {
                tracing::warn!("the generation cache cannot be read ({e}): it starts empty");
                std::fs::remove_file(path)?;
                create().map_err(|e| io::Error::other(e.to_string()))?
            }
        };
        Self::with_db(db, registry, settings)
    }

    /// A cache in memory, gone when it closes (tests and tools).
    pub fn in_memory(registry: &BlockRegistry, settings: GenCacheSettings) -> io::Result<Self> {
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .map_err(|e| io::Error::other(e.to_string()))?;
        Self::with_db(db, registry, settings)
    }

    fn with_db(
        db: redb::Database,
        registry: &BlockRegistry,
        settings: GenCacheSettings,
    ) -> io::Result<Self> {
        let other = |e: redb::Error| io::Error::other(e.to_string());
        let regions = prepare(&db).map_err(other)?;
        let mut names = Vec::new();
        for i in 0..registry.len() {
            let state = BlockState(u16::try_from(i).expect("states fit u16"));
            names.extend_from_slice(registry.state_name(state).to_string().as_bytes());
            names.push(0);
        }
        let shared = Arc::new(Shared {
            db: RwLock::new(Some(db)),
            chunk_hits: AtomicU64::new(0),
            chunk_misses: AtomicU64::new(0),
            lod_hits: AtomicU64::new(0),
            lod_misses: AtomicU64::new(0),
            offered: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            written: AtomicU64::new(0),
            damaged: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            evicted: AtomicU64::new(0),
            bytes: AtomicU64::new(regions.values().map(|r| r.bytes).sum()),
            logged: AtomicBool::new(false),
        });
        let (tx, rx) = crossbeam_channel::bounded(settings.queue.max(1));
        let writer = Writer {
            shared: Arc::clone(&shared),
            rx,
            max_bytes: settings.max_bytes,
            regions,
            dirty: HashSet::default(),
            stamps: HashSet::default(),
            last_durable: Instant::now(),
            unsynced: false,
            clock: 0,
        };
        let writer = std::thread::Builder::new()
            .name("boxel-gen-cache".to_string())
            .spawn(move || writer.run())?;
        Ok(Self {
            inner: Arc::new(Inner {
                shared,
                settings,
                registry: checksum(&names),
                states: registry.len(),
                tx,
                writer: Mutex::new(Some(writer)),
                touched: Mutex::default(),
                closed: AtomicBool::new(false),
                enabled: AtomicBool::new(true),
            }),
        })
    }

    /// Its settings.
    pub fn settings(&self) -> &GenCacheSettings {
        &self.inner.settings
    }

    /// The counts since it opened.
    pub fn stats(&self) -> GenCacheStats {
        let s = &self.inner.shared;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        GenCacheStats {
            chunk_hits: get(&s.chunk_hits),
            chunk_misses: get(&s.chunk_misses),
            lod_hits: get(&s.lod_hits),
            lod_misses: get(&s.lod_misses),
            offered: get(&s.offered),
            dropped: get(&s.dropped),
            written: get(&s.written),
            damaged: get(&s.damaged),
            errors: get(&s.errors),
            evicted: get(&s.evicted),
            bytes: get(&s.bytes),
        }
    }

    /// Turns the cache off (or on again) while its world runs, as a game's setting does: jobs
    /// started while it is off neither read nor offer records. On when it opens.
    pub fn set_enabled(&self, on: bool) {
        self.inner.enabled.store(on, Ordering::Relaxed);
    }

    /// The jobs use it ([`GenCache::set_enabled`]): on, and not closed.
    pub fn enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Relaxed) && !self.inner.closed.load(Ordering::Relaxed)
    }

    /// `other` is a handle of the same cache.
    pub fn same(&self, other: &GenCache) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Waits until every record offered before is written and durable. Blocks on the disk: for
    /// tests, tools and a world's close.
    pub fn flush(&self) -> io::Result<()> {
        self.ask(Op::Flush)
    }

    /// Drops every record and compacts the file (the "clear the cache" button of a game, while
    /// its world is open; with no world open the file can just be deleted). Blocks on the disk.
    /// Records offered before it are dropped too.
    pub fn clear(&self) -> io::Result<()> {
        self.ask(Op::Clear)
    }

    fn ask(&self, op: fn(Sender<io::Result<()>>) -> Op) -> io::Result<()> {
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(super::backend::closed_error());
        }
        let (tx, rx) = crossbeam_channel::bounded(1);
        self.inner
            .tx
            .send(op(tx))
            .map_err(|_| super::backend::closed_error())?;
        rx.recv().map_err(|_| super::backend::closed_error())?
    }

    /// Makes everything durable, stops the writer and closes the file, now, whoever still holds
    /// a handle: every later read misses and every later record is dropped. Closing again does
    /// nothing.
    pub fn close(&self) {
        stop(&self.inner);
    }

    /// `generator`'s chunk at `key` through the cache, as the world's chunk jobs get it: the
    /// cached copy when there is one with `generator`'s stamp (exactly what generating gives),
    /// else generated now and offered to the cache. A tool or a test can use it to read a world
    /// the way the world does.
    pub fn generate(
        &self,
        generator: &dyn WorldGenerator,
        key: ChunkKey,
        registry: &BlockRegistry,
    ) -> ChunkData {
        let stamp = self.chunk_stamp(generator);
        self.chunk(key, stamp).unwrap_or_else(|| {
            let data = generator.generate(key, registry);
            self.put_chunk(key, stamp, &data);
            data
        })
    }

    /// `generator`'s LOD source output for `req` through the cache, as the far LOD's jobs get it
    /// ([`LodSource::column`](boxel_worldgen::lod::LodSource::column)'s contract: every node of
    /// `req.layers` inside the band through `emit`, and the band): the cached column when it has
    /// all of it with the source's stamp, in the order the source emitted it, else the source's
    /// output now, recorded and offered to the cache. `None` when the generator has no source.
    pub fn source_column(
        &self,
        generator: &dyn WorldGenerator,
        req: &LodRequest,
        registry: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> Option<LodColumn> {
        let source = generator.lod()?;
        let stamp = self.lod_stamp(generator)?;
        if let Some(band) = self.lod_column(req, stamp, emit) {
            return Some(band);
        }
        // Nothing to record into a full queue.
        if self.full() {
            return Some(source.column(req, registry, scratch, emit));
        }
        let mut recorder = self.recorder();
        let band = source.column(req, registry, scratch, &mut |layer, node| {
            recorder.record(layer, node);
            emit(layer, node);
        });
        self.put_lod(req, stamp, band, recorder);
        Some(band)
    }

    /// The stamp of generated chunks of `generator` (module docs).
    pub(crate) fn chunk_stamp(&self, generator: &dyn WorldGenerator) -> u64 {
        self.stamp(b"chunk", generator.name(), generator.version())
    }

    /// The stamp of `generator`'s LOD source output; `None` without a source.
    pub(crate) fn lod_stamp(&self, generator: &dyn WorldGenerator) -> Option<u64> {
        let source = generator.lod()?;
        Some(self.stamp(b"lod", generator.name(), source.version()))
    }

    fn stamp(&self, part: &[u8], name: &str, version: u32) -> u64 {
        let mut bytes = Vec::with_capacity(32 + part.len() + name.len());
        bytes.extend_from_slice(&self.inner.settings.salt.to_le_bytes());
        bytes.extend_from_slice(&self.inner.registry.to_le_bytes());
        bytes.extend_from_slice(&RECORD_VERSION.to_le_bytes());
        bytes.extend_from_slice(part);
        bytes.push(0);
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&version.to_le_bytes());
        checksum(&bytes)
    }

    /// Runs `read` on the open file's records in one read transaction; `None` once closed or
    /// when the file fails (counted).
    fn read<T>(
        &self,
        read: impl FnOnce(&redb::ReadOnlyTable<&'static [u8; KEY_LEN], &'static [u8]>) -> T,
    ) -> Option<T> {
        let db = self
            .inner
            .shared
            .db
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        let db = db.as_ref()?;
        let table = db
            .begin_read()
            .map_err(redb::Error::from)
            .and_then(|txn| txn.open_table(RECORDS).map_err(redb::Error::from));
        match table {
            Ok(table) => Some(read(&table)),
            Err(e) => {
                self.inner.shared.error("cannot be read", e);
                None
            }
        }
    }

    /// The payload of the record at `key` if it has `stamp`: `None` if missing, of another
    /// stamp, damaged (counted) or when the file fails.
    fn payload(
        &self,
        table: &redb::ReadOnlyTable<&'static [u8; KEY_LEN], &'static [u8]>,
        key: &[u8; KEY_LEN],
        stamp: u64,
        magic: [u8; 4],
    ) -> Option<Vec<u8>> {
        let value = match table.get(key) {
            Ok(value) => value?,
            Err(e) => {
                self.inner.shared.error("cannot be read", e);
                return None;
            }
        };
        let bytes = value.value();
        if bytes.len() < STAMP_LEN
            || u64::from_le_bytes(bytes[..STAMP_LEN].try_into().expect("8 bytes")) != stamp
        {
            return None;
        }
        match open(magic, RECORD_VERSION, &bytes[STAMP_LEN..]) {
            Ok(payload) => Some(payload),
            Err(_) => {
                self.inner.shared.damaged.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// A state of a record, checked against the registry.
    fn state(&self, id: u64) -> Result<BlockState, FormatError> {
        u16::try_from(id)
            .ok()
            .filter(|&s| usize::from(s) < self.inner.states)
            .map(BlockState)
            .ok_or(FormatError::Damaged("state outside the registry"))
    }

    /// Tells the writer a region was read with `stamp`, once per region and run.
    fn touch(&self, region: Region, stamp: u64) {
        let first = self
            .inner
            .touched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(region);
        if first {
            // A full queue loses the touch: the region only looks older than it is.
            let _ = self.inner.tx.try_send(Op::Touch(region, stamp));
        }
    }

    /// The writer's queue is full: a record offered now would be dropped, so it is counted as
    /// dropped and not even encoded.
    fn full(&self) -> bool {
        let full = self.inner.tx.is_full();
        if full {
            let s = &self.inner.shared;
            s.offered.fetch_add(1, Ordering::Relaxed);
            s.dropped.fetch_add(1, Ordering::Relaxed);
        }
        full
    }

    /// Offers records to the writer; dropped (counted) when its queue is full or it is closed.
    fn offer(&self, records: Vec<Record>) {
        self.inner.shared.offered.fetch_add(1, Ordering::Relaxed);
        match self.inner.tx.try_send(Op::Put(records)) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.inner.shared.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// The cached chunk at `key` made with `stamp` ([`GenCache::chunk_stamp`]), exactly as its
    /// generation returned it; `None` (a miss, counted) otherwise.
    pub(crate) fn chunk(&self, key: ChunkKey, stamp: u64) -> Option<ChunkData> {
        let k = record_key(
            Kind::Chunk,
            key.dim,
            key.lod,
            key.pos.x,
            key.pos.y,
            key.pos.z,
        );
        let data = self
            .read(|table| self.payload(table, &k, stamp, CACHE_CHUNK_MAGIC))
            .flatten()
            .and_then(|payload| match self.decode_chunk(&payload) {
                Ok(data) => Some(data),
                Err(_) => {
                    self.inner.shared.damaged.fetch_add(1, Ordering::Relaxed);
                    None
                }
            });
        let s = &self.inner.shared;
        match data {
            Some(data) => {
                s.chunk_hits.fetch_add(1, Ordering::Relaxed);
                self.touch(Region::of(key.dim, key.lod, key.pos.x, key.pos.z), stamp);
                Some(data)
            }
            None => {
                s.chunk_misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    fn decode_chunk(&self, payload: &[u8]) -> Result<ChunkData, FormatError> {
        let mut r = Reader::new(payload);
        let version = u32::try_from(r.varint()?)
            .map_err(|_| FormatError::Damaged("chunk version out of range"))?;
        let blocks = read_storage(&mut r, |id| self.state(id))?;
        r.finish()?;
        let mut data = ChunkData::from_paletted(blocks);
        data.version = version;
        Ok(data)
    }

    /// Offers chunk `key`, as its generation with `stamp` returned it, to the writer. Uniform
    /// air is never kept (generating it is nearly free).
    pub(crate) fn put_chunk(&self, key: ChunkKey, stamp: u64, data: &ChunkData) {
        if data.as_uniform() == Some(BlockState::AIR)
            || self.inner.closed.load(Ordering::Relaxed)
            || self.full()
        {
            return;
        }
        let mut payload = Vec::new();
        put_varint(&mut payload, u64::from(data.version));
        // Runtime ids never fail.
        let _ = put_storage(&mut payload, data.blocks(), |s| Ok(u64::from(s.0)));
        let value = stamped(stamp, &seal(CACHE_CHUNK_MAGIC, RECORD_VERSION, &payload));
        self.offer(vec![Record {
            key: record_key(
                Kind::Chunk,
                key.dim,
                key.lod,
                key.pos.x,
                key.pos.y,
                key.pos.z,
            ),
            region: Region::of(key.dim, key.lod, key.pos.x, key.pos.z),
            stamp,
            value,
        }]);
    }

    /// The cached source output of `req` made with `stamp` ([`GenCache::lod_stamp`]): every node
    /// of `req.layers` inside the column's band, emitted through `emit` in the order the source
    /// emitted them, and the band. `None` (a miss, counted; nothing emitted) unless the band and
    /// every such node are cached with `stamp` and read back whole.
    pub(crate) fn lod_column(
        &self,
        req: &LodRequest,
        stamp: u64,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> Option<LodColumn> {
        let [x, z] = req.column;
        let read = self
            .read(|table| {
                let band_key = record_key(Kind::Band, req.dim, req.level, x, 0, z);
                let band = self.payload(table, &band_key, stamp, CACHE_BAND_MAGIC)?;
                let (band, order) = decode_band(&band).ok()?;
                let wanted = |l: i64| req.layers.contains(&l) && band.lo <= l && l < band.hi;
                let order: Vec<i64> = order.into_iter().filter(|&l| wanted(l)).collect();
                // Every wanted layer, or a miss.
                let inside =
                    (*req.layers.start()).max(band.lo)..=(*req.layers.end()).min(band.hi - 1);
                let count = inside
                    .end()
                    .saturating_sub(*inside.start())
                    .saturating_add(1)
                    .max(0);
                if usize::try_from(count).ok()? != order.len() {
                    return None;
                }
                let mut nodes = Vec::with_capacity(order.len());
                for layer in order {
                    let key = record_key(Kind::Node, req.dim, req.level, x, layer, z);
                    let payload = self.payload(table, &key, stamp, CACHE_NODE_MAGIC)?;
                    match self.decode_node(&payload) {
                        Ok(node) => nodes.push((layer, node)),
                        Err(_) => {
                            self.inner.shared.damaged.fetch_add(1, Ordering::Relaxed);
                            return None;
                        }
                    }
                }
                Some((band, nodes))
            })
            .flatten();
        let s = &self.inner.shared;
        let Some((band, nodes)) = read else {
            s.lod_misses.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        s.lod_hits.fetch_add(1, Ordering::Relaxed);
        self.touch(Region::of(req.dim, req.level, x, z), stamp);
        for (layer, node) in &nodes {
            match node {
                CachedNode::Uniform(state) => emit(*layer, LodNode::Uniform(*state)),
                CachedNode::Cells(cells) => emit(*layer, LodNode::Cells(cells)),
            }
        }
        Some(band)
    }

    fn decode_node(&self, payload: &[u8]) -> Result<CachedNode, FormatError> {
        let mut r = Reader::new(payload);
        let node = match r.u8()? {
            NODE_UNIFORM => CachedNode::Uniform(self.state(r.varint()?)?),
            NODE_CELLS => {
                let n = r.count(1 << 16)?;
                let palette = (0..n)
                    .map(|_| self.state(r.varint()?))
                    .collect::<Result<Vec<_>, _>>()?;
                let bits = r.u8()?;
                if !matches!(bits, 1 | 2 | 4 | 8 | 16) || n == 0 || n > 1 << bits {
                    return Err(FormatError::Damaged("bad palette"));
                }
                let per = 64 / usize::from(bits);
                let mask = (1u64 << bits) - 1;
                let mut cells = LodCells::new();
                let mut i = 0;
                for _ in 0..LOD_PADDED_VOLUME.div_ceil(per) {
                    let w = r.u64()?;
                    for j in 0..per {
                        if i == LOD_PADDED_VOLUME {
                            break;
                        }
                        let ix = ((w >> (j * usize::from(bits))) & mask) as usize;
                        cells.0[i] = *palette
                            .get(ix)
                            .ok_or(FormatError::Damaged("palette index out of range"))?;
                        i += 1;
                    }
                }
                CachedNode::Cells(cells)
            }
            _ => return Err(FormatError::Damaged("unknown node kind")),
        };
        r.finish()?;
        Ok(node)
    }

    /// A recorder of what a LOD source emits for one column, for [`GenCache::put_lod`].
    pub(crate) fn recorder(&self) -> LodRecorder {
        LodRecorder { nodes: Vec::new() }
    }

    /// Offers the column the source built for `req` with `stamp` (its band and the nodes
    /// `recorder` saw, in their order) to the writer, as one record.
    pub(crate) fn put_lod(
        &self,
        req: &LodRequest,
        stamp: u64,
        band: LodColumn,
        recorder: LodRecorder,
    ) {
        if self.inner.closed.load(Ordering::Relaxed) {
            return;
        }
        let [x, z] = req.column;
        let region = Region::of(req.dim, req.level, x, z);
        let order: Vec<i64> = recorder.nodes.iter().map(|(l, _)| *l).collect();
        let Some(band) = encode_band(band, &order) else {
            return;
        };
        let mut records = Vec::with_capacity(order.len() + 1);
        for (layer, sealed) in recorder.nodes {
            records.push(Record {
                key: record_key(Kind::Node, req.dim, req.level, x, layer, z),
                region,
                stamp,
                value: stamped(stamp, &sealed),
            });
        }
        // Last: a reader that finds the band finds its nodes (one transaction anyway).
        records.push(Record {
            key: record_key(Kind::Band, req.dim, req.level, x, 0, z),
            region,
            stamp,
            value: stamped(stamp, &band),
        });
        self.offer(records);
    }
}

/// Node payload kinds.
const NODE_UNIFORM: u8 = 0;
const NODE_CELLS: u8 = 1;

/// A node read back.
enum CachedNode {
    Uniform(BlockState),
    Cells(LodCells),
}

/// What a LOD source emitted for one column, each node sealed as it came
/// ([`GenCache::recorder`]).
pub(crate) struct LodRecorder {
    nodes: Vec<(i64, Vec<u8>)>,
}

impl LodRecorder {
    /// Records node `layer` as the source emitted it.
    pub(crate) fn record(&mut self, layer: i64, node: LodNode<'_>) {
        let mut payload = Vec::new();
        match node {
            LodNode::Uniform(state) => {
                payload.push(NODE_UNIFORM);
                put_varint(&mut payload, u64::from(state.0));
            }
            LodNode::Cells(cells) => {
                payload.push(NODE_CELLS);
                let mut palette: Vec<BlockState> = Vec::new();
                let mut index: HashMap<BlockState, u64, foldhash::fast::FixedState> =
                    HashMap::default();
                let ids: Vec<u64> = cells
                    .0
                    .iter()
                    .map(|&s| {
                        *index.entry(s).or_insert_with(|| {
                            palette.push(s);
                            palette.len() as u64 - 1
                        })
                    })
                    .collect();
                put_varint(&mut payload, palette.len() as u64);
                for s in &palette {
                    put_varint(&mut payload, u64::from(s.0));
                }
                let bits: u8 = match palette.len() {
                    0..=2 => 1,
                    3..=4 => 2,
                    5..=16 => 4,
                    17..=256 => 8,
                    _ => 16,
                };
                payload.push(bits);
                let per = 64 / usize::from(bits);
                for chunk in ids.chunks(per) {
                    let w = chunk
                        .iter()
                        .enumerate()
                        .fold(0u64, |w, (j, &id)| w | id << (j * usize::from(bits)));
                    payload.extend_from_slice(&w.to_le_bytes());
                }
            }
        }
        self.nodes
            .push((layer, seal(CACHE_NODE_MAGIC, RECORD_VERSION, &payload)));
    }
}

/// A band record: the band and the layers the source emitted, in its order. `None` when they do
/// not fit a record (a source that emits hundreds of layers is not cached).
fn encode_band(band: LodColumn, order: &[i64]) -> Option<Vec<u8>> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&band.lo.to_le_bytes());
    payload.extend_from_slice(&band.hi.to_le_bytes());
    put_varint(&mut payload, u64::from(band.below.0));
    put_varint(&mut payload, order.len() as u64);
    for l in order {
        payload.extend_from_slice(&l.to_le_bytes());
    }
    (payload.len() <= 4096).then(|| seal(CACHE_BAND_MAGIC, RECORD_VERSION, &payload))
}

fn decode_band(payload: &[u8]) -> Result<(LodColumn, Vec<i64>), FormatError> {
    let mut r = Reader::new(payload);
    let lo = r.i64()?;
    let hi = r.i64()?;
    let below = u16::try_from(r.varint()?)
        .map(BlockState)
        .map_err(|_| FormatError::Damaged("state out of range"))?;
    let n = r.count(payload.len())?;
    let order = (0..n).map(|_| r.i64()).collect::<Result<Vec<_>, _>>()?;
    r.finish()?;
    Ok((LodColumn { lo, hi, below }, order))
}

/// The band record `new` with the layers of `old` it lacks, when both have the same stamp and
/// band (else `None`: `new` replaces `old`). Both stamped.
fn merge_band(old: &[u8], new: &[u8]) -> Option<Vec<u8>> {
    let split = |b: &[u8]| -> Option<(u64, (LodColumn, Vec<i64>))> {
        let stamp = u64::from_le_bytes(b.get(..STAMP_LEN)?.try_into().ok()?);
        let payload = open(CACHE_BAND_MAGIC, RECORD_VERSION, &b[STAMP_LEN..]).ok()?;
        Some((stamp, decode_band(&payload).ok()?))
    };
    let (old_stamp, (old_band, old_order)) = split(old)?;
    let (new_stamp, (new_band, mut order)) = split(new)?;
    if old_stamp != new_stamp || old_band != new_band {
        return None;
    }
    let before = order.len();
    for layer in old_order {
        if !order.contains(&layer) {
            order.push(layer);
        }
    }
    if order.len() == before {
        return None;
    }
    Some(stamped(new_stamp, &encode_band(new_band, &order)?))
}

/// `envelope` behind its stamp.
fn stamped(stamp: u64, envelope: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(STAMP_LEN + envelope.len());
    out.extend_from_slice(&stamp.to_le_bytes());
    out.extend_from_slice(envelope);
    out
}

/// Makes the tables (emptied when the layout version differs) and reads the regions.
fn prepare(
    db: &redb::Database,
) -> Result<HashMap<Region, RegionInfo, foldhash::fast::FixedState>, redb::Error> {
    let txn = db.begin_write()?;
    {
        let meta = txn.open_table(META)?;
        let format = meta
            .get("format")?
            .map(|v| v.value().to_vec())
            .and_then(|b| <[u8; 4]>::try_from(b.as_slice()).ok())
            .map(u32::from_le_bytes);
        if format != Some(CACHE_FORMAT) {
            if format.is_some() {
                tracing::info!("the generation cache has another layout: it starts empty");
            }
            drop(meta);
            txn.delete_table(RECORDS)?;
            txn.delete_table(REGIONS)?;
            let mut meta = txn.open_table(META)?;
            meta.insert("format", CACHE_FORMAT.to_le_bytes().as_slice())?;
        }
    }
    txn.open_table(RECORDS)?;
    txn.open_table(REGIONS)?;
    txn.commit()?;
    let txn = db.begin_read()?;
    let table = txn.open_table(REGIONS)?;
    let mut regions = HashMap::default();
    for entry in table.iter()? {
        let (k, v) = entry?;
        if let Some(info) = RegionInfo::decode(v.value()) {
            regions.insert(Region::from_key(k.value()), info);
        }
    }
    Ok(regions)
}

/// The order regions are dropped in for the cap: the ones whose last stamp is not in `stamps`
/// (this run's) first, then by when they were last used, oldest first.
fn eviction_order(
    regions: &HashMap<Region, RegionInfo, foldhash::fast::FixedState>,
    stamps: &HashSet<u64, foldhash::fast::FixedState>,
) -> Vec<Region> {
    let mut order: Vec<(bool, u64, Region)> = regions
        .iter()
        .map(|(&r, i)| (stamps.contains(&i.stamp), i.used, r))
        .collect();
    order.sort_unstable_by_key(|&(current, used, r)| (current, used, r.dim.0, r.x, r.z));
    order.into_iter().map(|(_, _, r)| r).collect()
}

/// The writer thread's state.
struct Writer {
    shared: Arc<Shared>,
    rx: Receiver<Op>,
    max_bytes: u64,
    regions: HashMap<Region, RegionInfo, foldhash::fast::FixedState>,
    /// Regions changed since the last transaction.
    dirty: HashSet<Region, foldhash::fast::FixedState>,
    /// The stamps written or read this run: a region whose last stamp is not one of them is
    /// stale, evicted first.
    stamps: HashSet<u64, foldhash::fast::FixedState>,
    last_durable: Instant,
    /// Commits since the last durable one.
    unsynced: bool,
    /// The last time [`Writer::tick`] gave.
    clock: u64,
}

impl Writer {
    fn run(mut self) {
        loop {
            let first = match self.rx.recv_timeout(DURABLE_EVERY) {
                Ok(op) => op,
                Err(RecvTimeoutError::Timeout) => {
                    self.sync_if_due();
                    continue;
                }
                // Every handle is gone: nothing is left to write.
                Err(RecvTimeoutError::Disconnected) => {
                    // Counted inside: nothing is left to tell.
                    let _ = self.sync();
                    return;
                }
            };
            let mut ops = vec![first];
            while ops.len() < BATCH {
                match self.rx.try_recv() {
                    Ok(op) => ops.push(op),
                    Err(_) => break,
                }
            }
            let mut writes = Vec::new();
            for op in ops {
                match op {
                    Op::Put(records) => writes.extend(records),
                    Op::Touch(region, stamp) => {
                        self.stamps.insert(stamp);
                        let used = self.tick();
                        let info = self.regions.entry(region).or_default();
                        info.used = used;
                        info.stamp = stamp;
                        self.dirty.insert(region);
                    }
                    Op::Flush(answer) => {
                        self.commit(std::mem::take(&mut writes));
                        let _ = answer.send(self.sync());
                    }
                    Op::Clear(answer) => {
                        writes.clear();
                        let _ = answer.send(self.clear());
                    }
                    Op::Stop => {
                        self.commit(std::mem::take(&mut writes));
                        // Counted inside: the close goes on.
                        let _ = self.sync();
                        return;
                    }
                }
            }
            self.commit(writes);
            self.sync_if_due();
        }
    }

    /// Writes `records` and the changed regions in one transaction without fsync, then evicts
    /// past the cap.
    fn commit(&mut self, records: Vec<Record>) {
        if records.is_empty() && self.dirty.is_empty() {
            return;
        }
        let now = self.tick();
        let written = records.len() as u64;
        let result = self.with_db(|db, writer| {
            let mut txn = db.begin_write()?;
            txn.set_durability(redb::Durability::None)?;
            {
                let mut table = txn.open_table(RECORDS)?;
                for r in &records {
                    // A band joins the layers an earlier request of its column recorded: a
                    // column's second job (the nodes its first one left) asks for fewer layers.
                    let merged = if r.key[REGION_KEY_LEN] == Kind::Band as u8 {
                        let old = table.get(&r.key)?.map(|v| v.value().to_vec());
                        old.and_then(|old| merge_band(&old, &r.value))
                    } else {
                        None
                    };
                    let value = merged.as_deref().unwrap_or(&r.value);
                    let old = table.insert(&r.key, value)?.map(|v| v.value().len());
                    writer.stamps.insert(r.stamp);
                    let info = writer.regions.entry(r.region).or_default();
                    info.bytes =
                        info.bytes.saturating_sub(old.map_or(0, estimate)) + estimate(value.len());
                    info.used = now;
                    info.stamp = r.stamp;
                    writer.dirty.insert(r.region);
                }
                writer.write_regions(&txn)?;
            }
            txn.commit()?;
            Ok(())
        });
        match result {
            Ok(()) => {
                self.unsynced = true;
                self.shared.written.fetch_add(written, Ordering::Relaxed);
            }
            Err(e) => {
                // The sizes counted for the batch may be off until the regions are read again:
                // they only steer the cap.
                self.shared.error("cannot be written", e);
            }
        }
        self.publish();
        if self.total() > self.max_bytes {
            self.evict();
        }
    }

    /// Writes the changed regions' entries into `txn`.
    fn write_regions(&mut self, txn: &redb::WriteTransaction) -> Result<(), redb::Error> {
        let mut table = txn.open_table(REGIONS)?;
        for region in self.dirty.drain() {
            match self.regions.get(&region) {
                Some(info) => {
                    table.insert(&region.key(), info.encode().as_slice())?;
                }
                None => {
                    table.remove(&region.key())?;
                }
            }
        }
        Ok(())
    }

    /// The time of a write: now, and past the last one.
    fn tick(&mut self) -> u64 {
        self.clock = now_ms().max(self.clock + 1);
        self.clock
    }

    fn total(&self) -> u64 {
        self.regions.values().map(|r| r.bytes).sum()
    }

    fn publish(&self) {
        self.shared.bytes.store(self.total(), Ordering::Relaxed);
    }

    /// Drops whole regions until the cache is back under [`EVICT_TO`] of the cap: stale ones
    /// first, then the ones used longest ago.
    fn evict(&mut self) {
        let target = (self.max_bytes as f64 * EVICT_TO) as u64;
        let mut total = self.total();
        let mut gone = Vec::new();
        for region in eviction_order(&self.regions, &self.stamps) {
            if total <= target {
                break;
            }
            total = total.saturating_sub(self.regions[&region].bytes);
            gone.push(region);
        }
        let result = self.with_db(|db, writer| {
            let mut txn = db.begin_write()?;
            txn.set_durability(redb::Durability::None)?;
            {
                let mut table = txn.open_table(RECORDS)?;
                for region in &gone {
                    let (lo, hi) = region.range();
                    table.retain_in::<&[u8; KEY_LEN], _>(&lo..=&hi, |_, _| false)?;
                    writer.regions.remove(region);
                    writer.dirty.insert(*region);
                }
                writer.write_regions(&txn)?;
            }
            txn.commit()?;
            Ok(())
        });
        match result {
            Ok(()) => {
                self.unsynced = true;
                self.shared
                    .evicted
                    .fetch_add(gone.len() as u64, Ordering::Relaxed);
            }
            Err(e) => self.shared.error("cannot evict", e),
        }
        self.publish();
    }

    /// Drops every record and region, durably, and compacts the file.
    fn clear(&mut self) -> io::Result<()> {
        let result = self.with_db(|db, writer| {
            let txn = db.begin_write()?;
            txn.delete_table(RECORDS)?;
            txn.delete_table(REGIONS)?;
            txn.open_table(RECORDS)?;
            txn.open_table(REGIONS)?;
            txn.commit()?;
            writer.regions.clear();
            writer.dirty.clear();
            Ok(())
        });
        self.unsynced = false;
        self.last_durable = Instant::now();
        self.publish();
        result.map_err(|e| io::Error::other(e.to_string()))?;
        // Compaction needs the database alone: readers wait meanwhile (it is a button press).
        let mut db = self
            .shared
            .db
            .write()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(db) = db.as_mut()
            && let Err(e) = db.compact()
        {
            tracing::debug!("the generation cache could not be compacted: {e}");
        }
        Ok(())
    }

    /// Makes every commit durable if [`DURABLE_EVERY`] passed since the last time.
    fn sync_if_due(&mut self) {
        if self.unsynced && self.last_durable.elapsed() >= DURABLE_EVERY {
            // Counted inside; the next one tries again.
            let _ = self.sync();
        }
    }

    /// Makes every commit durable now (an empty commit with immediate durability).
    fn sync(&mut self) -> io::Result<()> {
        if !self.unsynced {
            return Ok(());
        }
        let result = self.with_db(|db, writer| {
            let txn = db.begin_write()?;
            writer.write_regions(&txn)?;
            txn.commit()?;
            Ok(())
        });
        self.last_durable = Instant::now();
        match result {
            Ok(()) => {
                self.unsynced = false;
                Ok(())
            }
            Err(e) => {
                self.shared.error("cannot be made durable", &e);
                Err(io::Error::other(e.to_string()))
            }
        }
    }

    fn with_db<T>(
        &mut self,
        op: impl FnOnce(&redb::Database, &mut Self) -> Result<T, redb::Error>,
    ) -> Result<T, redb::Error> {
        let shared = Arc::clone(&self.shared);
        let db = shared.db.read().unwrap_or_else(PoisonError::into_inner);
        match db.as_ref() {
            Some(db) => op(db, self),
            None => Err(redb::Error::DatabaseClosed),
        }
    }
}

#[cfg(test)]
mod tests;
