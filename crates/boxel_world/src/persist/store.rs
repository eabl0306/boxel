//! [`WorldStore`]: an open world, shared by the main thread and the jobs (ADR 0024).
use super::backend::{ChunkStore, RedbStore, SaveBatch};
use super::format::{
    self, FORMAT_VERSION, FormatError, IdMap, decode_chunk, decode_clock, decode_names,
    decode_ticks, encode_chunk, encode_names,
};
use boxel_ecs::prelude::*;
use boxel_voxel::{BlockPos, BlockRegistry, ChunkData, ChunkKey};
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// File of a world directory ([`WorldStore::open_dir`]).
pub const WORLD_FILE: &str = "world.redb";

/// Metadata names the engine uses: the `boxel:` prefix is reserved
/// ([`WorldStore::put_meta`] refuses it).
pub(crate) const META_FORMAT: &str = "boxel:format";
/// The id map ([`IdMap`]): every state name the world has used.
pub(crate) const META_STATES: &str = "boxel:states";
/// The generator's name when the world was created (a different one only warns).
pub(crate) const META_GENERATOR: &str = "boxel:generator";
/// The pending block ticks.
pub(crate) const META_TICKS: &str = "boxel:ticks";
/// The world clock (`TimeOfDay::tick`).
pub(crate) const META_CLOCK: &str = "boxel:clock";
/// Prefix of the engine's metadata names.
const RESERVED: &str = "boxel:";

/// Why a world could not be opened.
#[derive(Debug)]
pub enum PersistError {
    /// The store could not be read or written.
    Io(io::Error),
    /// The world was saved with another layout ([`FORMAT_VERSION`](super::FORMAT_VERSION)):
    /// it is left as it is.
    Version {
        /// The world's version.
        found: u32,
        /// The version this build reads and writes.
        supported: u32,
    },
    /// The world's metadata is damaged (the id map, the version): it is left as it is.
    Damaged(String),
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PersistError::Io(e) => write!(f, "world store: {e}"),
            PersistError::Version { found, supported } => write!(
                f,
                "the world was saved with format version {found}; this build reads version \
                 {supported}"
            ),
            PersistError::Damaged(what) => write!(f, "the world is damaged: {what}"),
        }
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PersistError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for PersistError {
    fn from(e: io::Error) -> Self {
        PersistError::Io(e)
    }
}

/// Counts of a world's saves and loads, since it opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PersistStats {
    /// Chunk records written.
    pub saved_chunks: usize,
    /// Chunk records read (chunk loads and sky walks).
    pub loaded_chunks: usize,
    /// Chunk records that could not be read: the chunk was generated instead (and its record
    /// left as it is until the chunk is edited).
    pub load_errors: usize,
    /// Save batches that failed (their chunks stay in memory and are written again later).
    pub save_errors: usize,
    /// Chunks waiting in memory for a save to write them.
    pub pending: usize,
}

/// A metadata value waiting for a save: its bytes, or what the save job encodes into them (the
/// pending ticks can be a long list: their encoding stays off the main thread).
#[derive(Clone, Debug)]
pub(crate) enum MetaValue {
    /// Encoded already.
    Bytes(Vec<u8>),
    /// The pending block ticks ([`format::encode_ticks`]).
    Ticks(Arc<Vec<(BlockPos, u64)>>),
}

impl MetaValue {
    /// The bytes to store.
    fn encode(&self) -> Vec<u8> {
        match self {
            MetaValue::Bytes(bytes) => bytes.clone(),
            MetaValue::Ticks(ticks) => format::encode_ticks(ticks),
        }
    }
}

/// A chunk waiting to be written, with the order it was handed over in.
struct Staged {
    data: Arc<ChunkData>,
    seq: u64,
}

/// What [`WorldStore::open`] read for the rest of the world: the saved chunk keys (the world's
/// index), the pending ticks and the clock.
#[derive(Debug, Default)]
pub(crate) struct Opened {
    pub(crate) keys: Vec<ChunkKey>,
    pub(crate) ticks: Option<Vec<(BlockPos, u64)>>,
    pub(crate) clock: Option<u64>,
}

struct Inner {
    backend: Arc<dyn ChunkStore>,
    ids: IdMap,
    /// Chunks handed over for saving and not written yet, by key: loads read them first, so a
    /// chunk that unloads and comes back before its save lands keeps its edits.
    pending: Mutex<HashMap<ChunkKey, Staged, foldhash::fast::FixedState>>,
    next_seq: AtomicU64,
    saved: AtomicUsize,
    loaded: AtomicUsize,
    load_errors: AtomicUsize,
    save_errors: AtomicUsize,
    opened: Mutex<Option<Opened>>,
}

/// An open world (ADR 0024): its store, its [`IdMap`] and the chunks waiting to be written.
/// Cheap to clone (one `Arc`), shared by the main thread and the jobs; also a resource once
/// [`PersistPlugin`](super::PersistPlugin) is added, for game data
/// ([`WorldStore::meta`], [`WorldStore::put_meta`]).
#[derive(Clone, Resource)]
pub struct WorldStore {
    inner: Arc<Inner>,
}

impl fmt::Debug for WorldStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldStore")
            .field("states", &self.inner.ids.names().len())
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// A damaged metadata value.
fn damaged(what: &str, e: FormatError) -> PersistError {
    PersistError::Damaged(format!("{what}: {e}"))
}

impl WorldStore {
    /// Opens the world in `dir` (the file [`WORLD_FILE`] there), creating both if needed.
    /// `registry` and `generator` are the world's (see [`WorldStore::open`]).
    pub fn open_dir(
        dir: impl AsRef<Path>,
        registry: &BlockRegistry,
        generator: &str,
    ) -> Result<Self, PersistError> {
        let backend = RedbStore::open(dir.as_ref().join(WORLD_FILE))?;
        Self::open(Arc::new(backend), registry, generator)
    }

    /// Opens the world kept in `backend`, or starts a new one there. `registry` must be the
    /// world's (the `WorldPlugin`'s): saved state names are resolved against it ([`IdMap`]),
    /// and its states missing from the world are added. `generator` is the generator's name: a
    /// world made with another one opens with a warning (chunks that were not saved come from
    /// the new generator).
    ///
    /// Reads the version, the id map and the index of saved chunks (every key, once). A world
    /// with another format version or a damaged id map is an error and is not touched; damaged
    /// pending ticks or clock only warn (they are lost).
    pub fn open(
        backend: Arc<dyn ChunkStore>,
        registry: &BlockRegistry,
        generator: &str,
    ) -> Result<Self, PersistError> {
        let keys = backend.keys()?;
        let names = backend.load_meta(META_STATES)?;
        let new = match backend.load_meta(META_FORMAT)? {
            None if keys.is_empty() && names.is_none() => true,
            None => {
                return Err(PersistError::Damaged(
                    "saved chunks without a format version".to_string(),
                ));
            }
            Some(bytes) => {
                let found = <[u8; 4]>::try_from(bytes.as_slice())
                    .map(u32::from_le_bytes)
                    .map_err(|_| PersistError::Damaged("bad format version".to_string()))?;
                if found != FORMAT_VERSION {
                    return Err(PersistError::Version {
                        found,
                        supported: FORMAT_VERSION,
                    });
                }
                false
            }
        };
        let names = match names {
            Some(bytes) => decode_names(&bytes).map_err(|e| damaged("id map", e))?,
            None if new => Vec::new(),
            None => return Err(PersistError::Damaged("no id map".to_string())),
        };
        let (ids, grew) = IdMap::resolve(names, registry);
        for (name, state) in ids.unknown() {
            tracing::warn!(
                saved = name.as_str(),
                now = %registry.state_name(*state),
                "a saved block state is not in the registry: it loads as the closest state"
            );
        }
        let saved_generator = backend.load_meta(META_GENERATOR)?;
        if let Some(saved) = &saved_generator
            && saved.as_slice() != generator.as_bytes()
        {
            tracing::warn!(
                saved = %String::from_utf8_lossy(saved),
                now = generator,
                "the world was made with another generator: chunks that were not saved come \
                 from the new one"
            );
        }
        let mut batch = SaveBatch::default();
        if new {
            batch.meta.push((
                META_FORMAT.to_string(),
                FORMAT_VERSION.to_le_bytes().to_vec(),
            ));
        }
        if saved_generator.is_none() {
            batch
                .meta
                .push((META_GENERATOR.to_string(), generator.as_bytes().to_vec()));
        }
        if new || grew {
            batch
                .meta
                .push((META_STATES.to_string(), encode_names(ids.names())));
        }
        backend.save_batch(&batch)?;
        // Transient state: a damaged copy is dropped, not a reason to refuse the world.
        let ticks = backend.load_meta(META_TICKS)?.and_then(|b| {
            decode_ticks(&b)
                .inspect_err(|e| tracing::error!("saved block ticks are lost: {e}"))
                .ok()
        });
        let clock = backend.load_meta(META_CLOCK)?.and_then(|b| {
            decode_clock(&b)
                .inspect_err(|e| tracing::error!("saved world clock is lost: {e}"))
                .ok()
        });
        Ok(Self {
            inner: Arc::new(Inner {
                backend,
                ids,
                pending: Mutex::default(),
                next_seq: AtomicU64::new(0),
                saved: AtomicUsize::new(0),
                loaded: AtomicUsize::new(0),
                load_errors: AtomicUsize::new(0),
                save_errors: AtomicUsize::new(0),
                opened: Mutex::new(Some(Opened { keys, ticks, clock })),
            }),
        })
    }

    /// The world's id map (stored state names and the states they load as).
    pub fn ids(&self) -> &IdMap {
        &self.inner.ids
    }

    /// The store underneath.
    pub fn backend(&self) -> &Arc<dyn ChunkStore> {
        &self.inner.backend
    }

    /// The counts since the world opened.
    pub fn stats(&self) -> PersistStats {
        let i = &self.inner;
        PersistStats {
            saved_chunks: i.saved.load(Ordering::Relaxed),
            loaded_chunks: i.loaded.load(Ordering::Relaxed),
            load_errors: i.load_errors.load(Ordering::Relaxed),
            save_errors: i.save_errors.load(Ordering::Relaxed),
            pending: i.pending.lock().expect("pending lock").len(),
        }
    }

    /// A game value saved with the world (`None` if never written). Reads the store: blocks on
    /// the disk, so call it while loading, not every frame.
    pub fn meta(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.inner.backend.load_meta(name)
    }

    /// Saves a game value with the world (a player position, a seed), durably, now: blocks on
    /// the disk, so call it while loading or exiting (a `Shutdown` system), not every frame.
    /// Names starting with `boxel:` belong to the engine and are refused.
    pub fn put_meta(&self, name: &str, value: Vec<u8>) -> io::Result<()> {
        if name.starts_with(RESERVED) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("metadata names starting with {RESERVED:?} belong to the engine"),
            ));
        }
        self.inner.backend.save_batch(&SaveBatch {
            chunks: Vec::new(),
            meta: vec![(name.to_string(), value)],
        })
    }

    /// What [`WorldStore::open`] read for the world (taken once, by the persistence plugin).
    pub(crate) fn take_opened(&self) -> Option<Opened> {
        self.inner.opened.lock().expect("opened lock").take()
    }

    /// Hands chunk `key` over for saving (the newest copy wins): from now on loads of `key` read
    /// this copy until a save job writes it.
    pub(crate) fn stage(&self, key: ChunkKey, data: Arc<ChunkData>) {
        let seq = self.inner.next_seq.fetch_add(1, Ordering::Relaxed);
        self.inner
            .pending
            .lock()
            .expect("pending lock")
            .insert(key, Staged { data, seq });
    }

    /// The saved blocks of chunk `key`: the copy waiting to be written, else the stored record.
    /// `None` when there is none, or when the record cannot be read (logged and counted in
    /// [`PersistStats::load_errors`]): the caller generates the chunk then.
    pub(crate) fn read_chunk(&self, key: ChunkKey) -> Option<Arc<ChunkData>> {
        if let Some(staged) = self.inner.pending.lock().expect("pending lock").get(&key) {
            return Some(Arc::clone(&staged.data));
        }
        let fail = |what: String| {
            self.inner.load_errors.fetch_add(1, Ordering::Relaxed);
            tracing::error!(chunk = ?key.pos, "saved chunk is generated again: {what}");
            None
        };
        match self.inner.backend.load(key) {
            Ok(Some(bytes)) => match decode_chunk(&bytes, &self.inner.ids) {
                Ok(data) => {
                    self.inner.loaded.fetch_add(1, Ordering::Relaxed);
                    Some(Arc::new(data))
                }
                Err(e) => fail(e.to_string()),
            },
            Ok(None) => None,
            Err(e) => fail(e.to_string()),
        }
    }

    /// Encodes and writes the staged copies of `keys` (those still staged) and `meta` in one
    /// batch. Once it is durable, a copy is dropped unless a newer one of its key was staged
    /// meanwhile. On an error nothing is dropped (the caller tries again later). Returns the
    /// chunks written.
    pub(crate) fn write(
        &self,
        keys: &[ChunkKey],
        meta: &[(String, MetaValue)],
    ) -> io::Result<usize> {
        let staged: Vec<(ChunkKey, Arc<ChunkData>, u64)> = {
            let pending = self.inner.pending.lock().expect("pending lock");
            keys.iter()
                .filter_map(|k| pending.get(k).map(|s| (*k, Arc::clone(&s.data), s.seq)))
                .collect()
        };
        let mut batch = SaveBatch {
            chunks: Vec::with_capacity(staged.len()),
            meta: meta
                .iter()
                .map(|(name, value)| (name.clone(), value.encode()))
                .collect(),
        };
        let mut written = Vec::with_capacity(staged.len());
        for (key, data, seq) in &staged {
            match encode_chunk(data, &self.inner.ids) {
                Ok(bytes) => {
                    batch.chunks.push((*key, bytes));
                    written.push((*key, *seq));
                }
                // Only a state outside the registry fails: the copy stays in memory.
                Err(e) => tracing::error!(chunk = ?key.pos, "chunk cannot be saved: {e}"),
            }
        }
        if let Err(e) = self.inner.backend.save_batch(&batch) {
            self.inner.save_errors.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }
        let mut pending = self.inner.pending.lock().expect("pending lock");
        for (key, seq) in &written {
            if pending.get(key).is_some_and(|s| s.seq == *seq) {
                pending.remove(key);
            }
        }
        drop(pending);
        self.inner.saved.fetch_add(written.len(), Ordering::Relaxed);
        Ok(written.len())
    }
}

/// The metadata value of the pending ticks (encoded by the save job).
pub(crate) fn ticks_meta(ticks: Vec<(BlockPos, u64)>) -> (String, MetaValue) {
    (META_TICKS.to_string(), MetaValue::Ticks(Arc::new(ticks)))
}

/// The metadata value of the world clock.
pub(crate) fn clock_meta(tick: u64) -> (String, MetaValue) {
    (
        META_CLOCK.to_string(),
        MetaValue::Bytes(format::encode_clock(tick)),
    )
}
