//! [`WorldStore`]: an open world, shared by the main thread and the jobs (ADR 0024).
use super::backend::{ChunkStore, RedbStore, SaveBatch, closed_error};
use super::format::{
    self, FORMAT_VERSION, FormatError, IdMap, decode_chunk, decode_clock, decode_names,
    decode_ticks, decode_worldgen, encode_chunk, encode_names, encode_worldgen, open_entity,
    seal_entity,
};
use super::saver::{SavedIndex, saved_index};
use crate::lod::LodSeed;
use boxel_ecs::prelude::*;
use boxel_voxel::{BlockPos, BlockRegistry, ChunkData, ChunkKey};
use boxel_worldgen::preset::WorldgenRecord;
use std::collections::HashMap;
use std::fmt;
use std::io;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

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
/// The next saved entity id (ids are never given twice, also after their entity is gone).
pub(crate) const META_ENTITIES: &str = "boxel:entities";
/// The world generation record ([`WorldgenRecord`], ADR 0039 sec. 17): written only when the
/// world is created, never into a world that lacks it (that one stays a legacy world).
pub(crate) const META_WORLDGEN: &str = "boxel:worldgen";
/// Prefix of the engine's metadata names.
const RESERVED: &str = "boxel:";

/// Openings of a world in this process ([`WorldStore::open`]), to tell two openings apart.
static OPENINGS: AtomicU64 = AtomicU64::new(0);

/// How many worlds this process opened so far: a store opened later has an
/// [`opened_seq`](WorldStore::opened_seq) at least this.
pub(crate) fn openings() -> u64 {
    OPENINGS.load(Ordering::SeqCst)
}

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

/// What a world's generator is, for [`WorldStore::open`]: its name (`boxel:generator`, kept from
/// when the world was made: another one only warns) and, for a world made through a preset, its
/// world generation record (`boxel:worldgen`, ADR 0039 sec. 17), written only if the world is
/// new. A name alone (`&str`) is a world without a record, as every world was before presets.
#[derive(Clone, Copy, Debug)]
pub struct GeneratorInfo<'a> {
    /// The generator's name ([`WorldGenerator::name`](boxel_worldgen::WorldGenerator::name)).
    pub name: &'a str,
    /// The record a new world saves ([`OpenedWorld::record`](boxel_worldgen::preset::OpenedWorld)),
    /// `None` for none.
    pub worldgen: Option<&'a WorldgenRecord>,
}

impl<'a> GeneratorInfo<'a> {
    /// The generator `name`, with no record.
    pub const fn new(name: &'a str) -> Self {
        Self {
            name,
            worldgen: None,
        }
    }

    /// With the record `record`, saved if the world is new.
    pub const fn with_worldgen(mut self, record: &'a WorldgenRecord) -> Self {
        self.worldgen = Some(record);
        self
    }
}

impl<'a> From<&'a str> for GeneratorInfo<'a> {
    fn from(name: &'a str) -> Self {
        Self::new(name)
    }
}

/// What a world holds under `boxel:worldgen` (ADR 0039 sec. 17).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SavedWorldgen {
    /// No record: a world made before records, or by a game that saves none. It is a legacy
    /// world and stays one: it opens through the legacy factory of its own type, unchanged.
    Legacy,
    /// The record it was made with.
    Record(WorldgenRecord),
    /// A record this build cannot read (damaged, or of a newer record version): the world opens
    /// all the same, without a diff.
    Unreadable(FormatError),
}

impl SavedWorldgen {
    /// What `backend`'s world holds, read now (before [`WorldStore::open`], so its live record can
    /// keep the saved dimension numbers).
    pub fn read(backend: &dyn ChunkStore) -> io::Result<Self> {
        Ok(match backend.load_meta(META_WORLDGEN)? {
            None => Self::Legacy,
            Some(bytes) => match decode_worldgen(&bytes) {
                Ok(record) => Self::Record(record),
                Err(e) => Self::Unreadable(e),
            },
        })
    }

    /// The record, if the world has a readable one.
    pub fn record(&self) -> Option<&WorldgenRecord> {
        match self {
            Self::Record(record) => Some(record),
            Self::Legacy | Self::Unreadable(_) => None,
        }
    }
}

/// Counts of a world's saves and loads, since it opened.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PersistStats {
    /// Chunk records written.
    pub saved_chunks: usize,
    /// Chunk records read (chunk loads and sky walks).
    pub loaded_chunks: usize,
    /// Chunk records that could not be read. A damaged record: the chunk was generated instead
    /// (and its record left as it is until the chunk is edited). A store that failed: the chunk
    /// stays out (air nobody can edit) until it loads again, so its record is never saved over.
    pub load_errors: usize,
    /// Save batches that failed (their chunks stay in memory and are written again later).
    pub save_errors: usize,
    /// Chunks waiting in memory for a save to write them.
    pub pending: usize,
    /// Entity records written (removals not counted).
    pub saved_entities: usize,
    /// Entities brought back from saved records.
    pub loaded_entities: usize,
    /// Entity records that could not be read (damaged, another record version, the store
    /// failed) or brought back (no codec of this run places them): they stay saved as they are.
    pub entity_errors: usize,
    /// Entity records and removals waiting in memory for a save to write them.
    pub pending_entities: usize,
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

/// An entity record waiting to be written, as content without its envelope (the save job
/// seals it: the checksum and LZ4 stay off the main thread); `None`: its removal. With the order
/// it was handed over in.
struct StagedEntity {
    bytes: Option<Arc<Vec<u8>>>,
    seq: u64,
}

/// An entity record (or removal) a save job writes: the staged copy as it was when the job
/// was made. A job never reads newer copies: a move stages the removal and the new record
/// together, so a job that took one of them took the other one too.
#[derive(Clone, Debug)]
pub(crate) struct EntityWrite {
    pub(crate) chunk: ChunkKey,
    pub(crate) id: u64,
    /// The record's content, unsealed (the job seals it); `None`: the record is removed.
    pub(crate) bytes: Option<Arc<Vec<u8>>>,
    seq: u64,
}

/// Entity records waiting to be written, by chunk and saved id.
type PendingEntities = HashMap<
    ChunkKey,
    HashMap<u64, StagedEntity, foldhash::fast::FixedState>,
    foldhash::fast::FixedState,
>;

/// What [`WorldStore::open`] read for the rest of the world: the saved chunk keys (the world's
/// index), the pending ticks and the clock.
#[derive(Debug, Default)]
pub(crate) struct Opened {
    /// The saved layers of each chunk column.
    pub(crate) saved: SavedIndex,
    /// The far LOD's index of the same chunks.
    pub(crate) lod: LodSeed,
    pub(crate) ticks: Option<Vec<(BlockPos, u64)>>,
    pub(crate) clock: Option<u64>,
    /// Every saved entity: the chunk whose record holds it and its saved id.
    pub(crate) entities: Vec<(ChunkKey, u64)>,
    /// The next saved entity id: past every id saved and every id given before.
    pub(crate) next_entity: u64,
}

struct Inner {
    backend: Arc<dyn ChunkStore>,
    ids: IdMap,
    /// What the world holds under `boxel:worldgen`, as it opened.
    worldgen: SavedWorldgen,
    /// Chunks handed over for saving and not written yet, by key: loads read them first, so a
    /// chunk that unloads and comes back before its save lands keeps its edits.
    pending: Mutex<HashMap<ChunkKey, Staged, foldhash::fast::FixedState>>,
    /// Entity records handed over and not written yet: loads read them over the stored ones.
    pending_entities: Mutex<PendingEntities>,
    next_seq: AtomicU64,
    saved: AtomicUsize,
    loaded: AtomicUsize,
    load_errors: AtomicUsize,
    save_errors: AtomicUsize,
    saved_entities: AtomicUsize,
    loaded_entities: AtomicUsize,
    entity_errors: AtomicUsize,
    opened: Mutex<Option<Opened>>,
    /// [`WorldStore::close`] ran: every operation fails quietly from then on.
    closed: AtomicBool,
    /// Which opening of the process this is (increasing), taken before the index was read.
    opened_seq: u64,
}

/// An open world (ADR 0024): its store, its [`IdMap`] and the chunks waiting to be written.
/// Cheap to clone (one `Arc`), shared by the main thread and the jobs; also a resource once
/// [`PersistPlugin`](super::PersistPlugin) is added, for game data
/// ([`WorldStore::meta`], [`WorldStore::put_meta`]). It serves one `PersistPlugin`: a new `App`
/// opens the world again (a redb file is locked while it is open: the exit closes it,
/// [`WorldStore::close`]).
#[derive(Clone, Resource)]
pub struct WorldStore {
    inner: Arc<Inner>,
}

impl fmt::Debug for WorldStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorldStore")
            .field("opening", &self.opened_seq())
            .field("closed", &self.is_closed())
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
    pub fn open_dir<'a>(
        dir: impl AsRef<Path>,
        registry: &BlockRegistry,
        generator: impl Into<GeneratorInfo<'a>>,
    ) -> Result<Self, PersistError> {
        let backend = RedbStore::open(dir.as_ref().join(WORLD_FILE))?;
        Self::open(Arc::new(backend), registry, generator)
    }

    /// Opens the world kept in `backend`, or starts a new one there. `registry` must be the
    /// world's (the `WorldPlugin`'s): saved state names are resolved against it ([`IdMap`]),
    /// and its states missing from the world are added. `generator` is the generator's name (a
    /// world made with another one opens with a warning: chunks that were not saved come from
    /// the new generator) and, for a world made through a preset, its world generation record
    /// ([`GeneratorInfo`]): a new world saves it under `boxel:worldgen`, an existing one never
    /// gets one, so a world made before records stays a legacy world
    /// ([`WorldStore::worldgen`]). The format version does not change for it.
    ///
    /// Reads the version, the id map and the index of saved chunks (every key, once), and
    /// builds from it what the world's save state and its far LOD start with, so the world's
    /// open does not walk the saved chunks on the main thread (call this off it). A world with
    /// another format version or a damaged id map is an error and is not touched; damaged
    /// pending ticks or clock only warn (they are lost).
    pub fn open<'a>(
        backend: Arc<dyn ChunkStore>,
        registry: &BlockRegistry,
        generator: impl Into<GeneratorInfo<'a>>,
    ) -> Result<Self, PersistError> {
        let GeneratorInfo {
            name: generator,
            worldgen,
        } = generator.into();
        // Numbered before anything is read (see `opened_seq`).
        let opened_seq = OPENINGS.fetch_add(1, Ordering::Relaxed);
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
                "a saved block state is not in the registry as it was saved: it loads as the closest \
                 state"
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
            if let Some(record) = worldgen {
                batch
                    .meta
                    .push((META_WORLDGEN.to_string(), encode_worldgen(record)));
            }
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
        // Only a new world gets a record: one made before records stays legacy. A record it
        // cannot read only warns (the game opens the world without a diff).
        let worldgen = match (new, worldgen) {
            (true, Some(record)) => SavedWorldgen::Record(record.clone()),
            (true, None) => SavedWorldgen::Legacy,
            (false, _) => SavedWorldgen::read(&*backend)?,
        };
        if let SavedWorldgen::Unreadable(e) = &worldgen {
            tracing::warn!("the world's generation record cannot be read: {e}");
        }
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
        let entities = backend.entity_keys()?;
        // Past every saved id, also when the counter is damaged or older than the records.
        let counter = backend.load_meta(META_ENTITIES)?.and_then(|b| {
            decode_clock(&b)
                .inspect_err(|e| tracing::warn!("saved entity id counter is damaged: {e}"))
                .ok()
        });
        let next_entity = entities
            .iter()
            .map(|&(_, id)| id.saturating_add(1))
            .chain(counter)
            .max()
            .unwrap_or(1)
            .max(1);
        // The indexes the world's save state and its far LOD start from: built here, where a
        // game opens its store off the main thread, they cost the open no walk of the saved
        // chunks there.
        let saved = saved_index(keys);
        let lod = LodSeed::of(&saved);
        Ok(Self {
            inner: Arc::new(Inner {
                backend,
                ids,
                worldgen,
                pending: Mutex::default(),
                pending_entities: Mutex::default(),
                next_seq: AtomicU64::new(0),
                saved: AtomicUsize::new(0),
                loaded: AtomicUsize::new(0),
                load_errors: AtomicUsize::new(0),
                save_errors: AtomicUsize::new(0),
                saved_entities: AtomicUsize::new(0),
                loaded_entities: AtomicUsize::new(0),
                entity_errors: AtomicUsize::new(0),
                opened: Mutex::new(Some(Opened {
                    saved,
                    lod,
                    ticks,
                    clock,
                    entities,
                    next_entity,
                })),
                closed: AtomicBool::new(false),
                opened_seq,
            }),
        })
    }

    /// Closes the world for good, now, on this thread, while clones of it still live (jobs
    /// that read it, a game's task): waits for the store operation in progress, closes the
    /// store underneath ([`ChunkStore::close`]: a redb file writes its clean shutdown and is
    /// released, so it opens again at once and without a repair) and drops the chunks and
    /// entity records still waiting to be written (write them first: the exit does). Closing
    /// again does nothing.
    ///
    /// From then on every operation of every clone fails with the error kind
    /// [`io::ErrorKind::NotConnected`], without a log line and without counting it in
    /// [`PersistStats`]: a reader that comes late gets nothing, never a made-up copy. The
    /// [`PersistPlugin`](super::PersistPlugin) closes its store in `Shutdown`, after the exit
    /// save ([`WorldShutdownSystems::Close`](super::WorldShutdownSystems::Close)).
    pub fn close(&self) -> io::Result<()> {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let result = self.inner.backend.close();
        // Nothing writes them any more: a clone kept after the close holds no chunk data. Taken
        // under their locks, which `stage` and `stage_entity` check the flag under.
        let pending = std::mem::take(&mut *self.inner.pending.lock().expect("pending lock"));
        let entities =
            std::mem::take(&mut *self.inner.pending_entities.lock().expect("pending lock"));
        drop((pending, entities));
        result
    }

    /// The world's file was not closed cleanly (a crash) and was repaired when it opened
    /// ([`ChunkStore::repaired`]). [`PersistPlugin`](super::PersistPlugin) logs it once, when it
    /// is added.
    pub fn repaired(&self) -> bool {
        self.inner.backend.repaired()
    }

    /// [`WorldStore::close`] ran.
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// How many handles (clones) of this world are alive, this one included. For diagnostics
    /// and tests: it changes as jobs come and go, so never wait for a count.
    pub fn handles(&self) -> usize {
        Arc::strong_count(&self.inner)
    }

    /// `other` is a handle of the same opening of the world (a clone of this one).
    pub fn same(&self, other: &WorldStore) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    /// Which opening of the process this is: a later [`WorldStore::open`] has a higher one. Taken
    /// when the open starts, before it reads the index of saved chunks, so an opening whose index
    /// misses a write has a lower number than every opening started after that write.
    pub(crate) fn opened_seq(&self) -> u64 {
        self.inner.opened_seq
    }

    /// The closed error once [`WorldStore::close`] ran: every operation checks it first.
    fn check_open(&self) -> io::Result<()> {
        if self.is_closed() {
            Err(closed_error())
        } else {
            Ok(())
        }
    }

    /// The world's id map (stored state names and the states they load as).
    pub fn ids(&self) -> &IdMap {
        &self.inner.ids
    }

    /// What the world holds under `boxel:worldgen` (ADR 0039 sec. 17): the record it was made
    /// with (for a new world, the one it was just made with), or none for a legacy world. The
    /// game compares it with the live one (`Worldgen::open`'s diff); the store only keeps it.
    pub fn worldgen(&self) -> &SavedWorldgen {
        &self.inner.worldgen
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
            saved_entities: i.saved_entities.load(Ordering::Relaxed),
            loaded_entities: i.loaded_entities.load(Ordering::Relaxed),
            entity_errors: i.entity_errors.load(Ordering::Relaxed),
            pending_entities: i
                .pending_entities
                .lock()
                .expect("pending lock")
                .values()
                .map(HashMap::len)
                .sum(),
        }
    }

    /// A game value saved with the world (`None` if never written). Reads the store: blocks on
    /// the disk, so call it while loading, not every frame.
    pub fn meta(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        self.check_open()?;
        self.inner
            .backend
            .load_meta(name)
            .map_err(|e| self.closed_or(e))
    }

    /// Saves a game value with the world (a player position, a seed), durably, now: blocks on
    /// the disk, so call it while loading or exiting (a `Shutdown` system), not every frame.
    /// Names starting with `boxel:` belong to the engine and are refused.
    pub fn put_meta(&self, name: &str, value: Vec<u8>) -> io::Result<()> {
        self.check_open()?;
        if name.starts_with(RESERVED) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("metadata names starting with {RESERVED:?} belong to the engine"),
            ));
        }
        self.inner
            .backend
            .save_batch(&SaveBatch {
                meta: vec![(name.to_string(), value)],
                ..SaveBatch::default()
            })
            .map_err(|e| self.closed_or(e))
    }

    /// `e`, or the closed error when the world closed meanwhile: an operation the close cut
    /// short is a late reader, not a store that fails (nothing to log or count).
    fn closed_or(&self, e: io::Error) -> io::Error {
        if self.is_closed() { closed_error() } else { e }
    }

    /// A handle that does not keep the world alive (its handle count, once closed).
    pub(crate) fn downgrade(&self) -> WeakStore {
        WeakStore(Arc::downgrade(&self.inner))
    }

    /// What [`WorldStore::open`] read is still there: no world was served from this store.
    pub(crate) fn has_opened(&self) -> bool {
        self.inner.opened.lock().expect("opened lock").is_some()
    }

    /// What [`WorldStore::open`] read for the world (taken once, by the persistence plugin).
    pub(crate) fn take_opened(&self) -> Option<Opened> {
        self.inner.opened.lock().expect("opened lock").take()
    }

    /// Hands chunk `key` over for saving (the newest copy wins): from now on loads of `key` read
    /// this copy until a save job writes it. Dropped once the world is closed.
    pub(crate) fn stage(&self, key: ChunkKey, data: Arc<ChunkData>) {
        let seq = self.inner.next_seq.fetch_add(1, Ordering::Relaxed);
        let mut pending = self.inner.pending.lock().expect("pending lock");
        // Checked under the lock the close empties the copies under: none outlives the close.
        if self.is_closed() {
            drop(pending);
            tracing::debug!(chunk = ?key.pos, "the world is closed: the chunk is not saved");
            return;
        }
        pending.insert(key, Staged { data, seq });
    }

    /// The saved blocks of chunk `key`: the copy waiting to be written, else the stored record.
    /// `Ok(None)` when there is none, or when the record is damaged (logged and counted in
    /// [`PersistStats::load_errors`]): the caller generates the chunk then. `Err` when the store
    /// itself fails (counted too): the record may be fine, so the caller must not make up a copy
    /// that could be edited and saved over it. `Err` without a count or a log once the world is
    /// closed.
    pub(crate) fn load_chunk(&self, key: ChunkKey) -> io::Result<Option<Arc<ChunkData>>> {
        self.check_open()?;
        if let Some(staged) = self.inner.pending.lock().expect("pending lock").get(&key) {
            return Ok(Some(Arc::clone(&staged.data)));
        }
        let bytes = match self.inner.backend.load(key) {
            Ok(bytes) => bytes,
            Err(_) if self.is_closed() => return Err(closed_error()),
            Err(e) => {
                self.inner.load_errors.fetch_add(1, Ordering::Relaxed);
                tracing::error!(chunk = ?key.pos, "saved chunk cannot be read now: {e}");
                return Err(e);
            }
        };
        let Some(bytes) = bytes else {
            return Ok(None);
        };
        match decode_chunk(&bytes, &self.inner.ids) {
            Ok(data) => {
                self.inner.loaded.fetch_add(1, Ordering::Relaxed);
                Ok(Some(Arc::new(data)))
            }
            Err(e) => {
                self.inner.load_errors.fetch_add(1, Ordering::Relaxed);
                tracing::error!(chunk = ?key.pos, "saved chunk is generated again: {e}");
                Ok(None)
            }
        }
    }

    /// [`WorldStore::load_chunk`] for a reader that only looks (a sky walk): a store that fails
    /// reads as no saved copy.
    pub(crate) fn read_chunk(&self, key: ChunkKey) -> Option<Arc<ChunkData>> {
        self.load_chunk(key).ok().flatten()
    }

    /// The staged entity records and removals of `ops` as they are now (those still staged),
    /// for a save job: taken on the main thread when the job is made, so the job writes one
    /// moment of the entities whatever is staged while it waits to run.
    pub(crate) fn entity_writes(&self, ops: &[(ChunkKey, u64)]) -> Vec<EntityWrite> {
        let pending = self.inner.pending_entities.lock().expect("pending lock");
        ops.iter()
            .filter_map(|&(chunk, id)| {
                let s = pending.get(&chunk)?.get(&id)?;
                Some(EntityWrite {
                    chunk,
                    id,
                    bytes: s.bytes.clone(),
                    seq: s.seq,
                })
            })
            .collect()
    }

    /// Encodes and writes the staged copies of `keys` (those still staged), `meta` and the
    /// entity records `entities` ([`WorldStore::entity_writes`]) in one batch. Once it is
    /// durable, a copy or record is dropped unless a newer one of its key was staged meanwhile.
    /// On an error nothing is dropped (the caller tries again later). Returns the chunks
    /// written. Fails with the closed error, not counted, once the world is closed.
    pub(crate) fn write(
        &self,
        keys: &[ChunkKey],
        meta: &[(String, MetaValue)],
        entities: &[EntityWrite],
    ) -> io::Result<usize> {
        self.check_open()?;
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
            entities: entities
                .iter()
                .map(|w| (w.chunk, w.id, w.bytes.as_deref().map(|b| seal_entity(b))))
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
            if self.is_closed() {
                return Err(closed_error());
            }
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
        let mut pending = self.inner.pending_entities.lock().expect("pending lock");
        for w in entities {
            if let Some(records) = pending.get_mut(&w.chunk) {
                if records.get(&w.id).is_some_and(|s| s.seq == w.seq) {
                    records.remove(&w.id);
                }
                if records.is_empty() {
                    pending.remove(&w.chunk);
                }
            }
        }
        drop(pending);
        let records = entities.iter().filter(|w| w.bytes.is_some()).count();
        self.inner
            .saved_entities
            .fetch_add(records, Ordering::Relaxed);
        self.inner.saved.fetch_add(written.len(), Ordering::Relaxed);
        Ok(written.len())
    }

    /// Hands the record of entity `id` in chunk `chunk` over for saving, as content without its
    /// envelope ([`format::entity_payload`]; `None`: its removal; the newest wins): from now on
    /// loads of `chunk` read it over the stored records until a save job writes it. Dropped once
    /// the world is closed.
    pub(crate) fn stage_entity(&self, chunk: ChunkKey, id: u64, bytes: Option<Arc<Vec<u8>>>) {
        let seq = self.inner.next_seq.fetch_add(1, Ordering::Relaxed);
        let mut pending = self.inner.pending_entities.lock().expect("pending lock");
        // Under the lock the close empties it under, as in `stage`.
        if self.is_closed() {
            drop(pending);
            tracing::debug!(id, chunk = ?chunk.pos, "the world is closed: the entity is not saved");
            return;
        }
        pending
            .entry(chunk)
            .or_default()
            .insert(id, StagedEntity { bytes, seq });
    }

    /// The saved entity records of chunk `key`, by id, ascending, as their content
    /// ([`format::entity_payload`]): the stored ones with the records and removals waiting to
    /// be written over them. A stored record that is damaged or of another record version is
    /// left out (logged and counted in [`PersistStats::entity_errors`]; it stays as it is).
    /// `Err` when the store fails (counted too): the records stay as they are; without a count
    /// or a log once the world is closed. Runs in the chunk's generation job, so the envelopes
    /// are opened off the main thread.
    pub(crate) fn load_entities(&self, key: ChunkKey) -> io::Result<Vec<(u64, Vec<u8>)>> {
        self.check_open()?;
        // The staged ones first: a save job that lands between the two reads wrote the same
        // records, and laying the staged ones over the store again changes nothing.
        let staged: HashMap<u64, Option<Arc<Vec<u8>>>, foldhash::fast::FixedState> = self
            .inner
            .pending_entities
            .lock()
            .expect("pending lock")
            .get(&key)
            .map(|records| {
                records
                    .iter()
                    .map(|(&id, s)| (id, s.bytes.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let stored = match self.inner.backend.load_entities(key) {
            Ok(stored) => stored,
            Err(_) if self.is_closed() => return Err(closed_error()),
            Err(e) => {
                self.inner.entity_errors.fetch_add(1, Ordering::Relaxed);
                tracing::error!(chunk = ?key.pos, "saved entities cannot be read now: {e}");
                return Err(e);
            }
        };
        let mut records: std::collections::BTreeMap<u64, Vec<u8>> =
            std::collections::BTreeMap::new();
        for (id, bytes) in stored {
            // A staged record or removal replaces it: it is not even opened.
            if staged.contains_key(&id) {
                continue;
            }
            match open_entity(&bytes) {
                Ok(payload) => {
                    records.insert(id, payload);
                }
                Err(e) => {
                    self.inner.entity_errors.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(id, chunk = ?key.pos, "a saved entity cannot be read: {e}");
                }
            }
        }
        for (id, bytes) in staged {
            match bytes {
                Some(bytes) => records.insert(id, Vec::clone(&bytes)),
                None => records.remove(&id),
            };
        }
        Ok(records.into_iter().collect())
    }

    /// Counts entities brought back (`loaded`) and records that could not be (`errors`).
    pub(crate) fn count_entities(&self, loaded: usize, errors: usize) {
        self.inner
            .loaded_entities
            .fetch_add(loaded, Ordering::Relaxed);
        self.inner
            .entity_errors
            .fetch_add(errors, Ordering::Relaxed);
    }
}

/// A [`WorldStore`] handle that does not keep it alive.
pub(crate) struct WeakStore(Weak<Inner>);

impl WeakStore {
    /// Handles of the world still alive.
    pub(crate) fn handles(&self) -> usize {
        self.0.strong_count()
    }
}

/// The metadata value of the pending ticks (encoded by the save job).
pub(crate) fn ticks_meta(ticks: Vec<(BlockPos, u64)>) -> (String, MetaValue) {
    (META_TICKS.to_string(), MetaValue::Ticks(Arc::new(ticks)))
}

/// The metadata value of the next saved entity id.
pub(crate) fn entities_meta(next: u64) -> (String, MetaValue) {
    (
        META_ENTITIES.to_string(),
        MetaValue::Bytes(format::encode_clock(next)),
    )
}

/// The metadata value of the world clock.
pub(crate) fn clock_meta(tick: u64) -> (String, MetaValue) {
    (
        META_CLOCK.to_string(),
        MetaValue::Bytes(format::encode_clock(tick)),
    )
}
