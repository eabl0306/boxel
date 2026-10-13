//! Entities saved with the world (ADR 0024 section 3): the [`Saved`] marker, the
//! [`EntityCodec`]s that plugins register (physics registers the one of its bodies), and the
//! bookkeeping that puts saved entities away with their chunk and brings them back when it loads.
//!
//! The model:
//! - A saved entity has a saved id ([`SavedId`]), given by the world the first time it sees the
//!   entity and never given again. ECS entity ids are never saved: an entity that comes back is
//!   a new entity, and a codec that keeps a reference to another saved entity saves its saved id
//!   ([`SavedIds`]).
//! - Each saved entity has one record in the `entities` table, keyed by the chunk it was in when
//!   it was last saved and its saved id. An entity saved under another chunk than before gets its
//!   old record removed in the same transaction, so the disk never holds an entity twice.
//! - A saved entity whose chunk has no slot (it unloaded, or the entity was put where nothing is
//!   loaded) is put away: saved and despawned. The chunk's generation job reads the records back
//!   when it loads again, and the entity step brings them back before the next fixed step.
//! - The autosave and the exit save every saved entity alive, under its current chunk: only the
//!   ones a codec says changed since their record was made ([`EntityCodec::changed`]) or that
//!   gained or lost a component, and of those only the records that differ. The main thread
//!   makes the records' content; the save job seals it (checksum and LZ4), and the generation
//!   job opens the stored ones.
//! - A record is brought back only if its id is not alive and it is the newest record of its id:
//!   a chunk that unloads and loads again never brings an entity back twice.
use super::format::{EntityPart, entity_parts, entity_payload};
use super::plugin::{PersistSettings, save_meta, stage_game_meta};
use super::saver::{Moment, Persist};
use super::store::WorldStore;
use crate::lifecycle::WorldLifecycle;
use crate::streaming::WorldJobs;
use crate::{BlockTicks, ChunkMap, ChunkViewer, TimeOfDay, WorldSettings};
use boxel_app::App;
use boxel_ecs::archetype::ArchetypeId;
use boxel_ecs::change_detection::Tick;
use boxel_ecs::lifecycle::HookContext;
use boxel_ecs::prelude::*;
use boxel_ecs::world::DeferredWorld;
use boxel_voxel::{BlockPos, ChunkKey, ChunkPos, DimensionId};
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;
use std::sync::Arc;

/// Hash maps of the index: fixed seeds, so the bookkeeping is the same in every run.
type Fixed = foldhash::fast::FixedState;

/// The stable id of a saved entity in its world: given once, kept across restarts (unlike the
/// ECS `Entity`, which changes every time the entity comes back).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SavedId(u64);

impl SavedId {
    /// The id as a number, to save a reference to the entity.
    pub const fn to_bits(self) -> u64 {
        self.0
    }

    /// The id saved with [`SavedId::to_bits`] (`None` for 0, which no entity has).
    pub const fn from_bits(bits: u64) -> Option<Self> {
        if bits == 0 { None } else { Some(Self(bits)) }
    }
}

/// Marker: this entity is saved with the world (with [`PersistPlugin`](super::PersistPlugin)).
/// The registered [`EntityCodec`]s say what is saved of it (physics saves its body); an entity
/// that no codec places in a chunk is not saved.
///
/// - When its chunk unloads, or when it is somewhere with no chunk slot, it is saved and
///   despawned; it comes back, as a new entity with the same components, when the chunk loads,
///   before the next fixed step. Above or below `WorldSettings::vertical_bounds` its chunk is
///   the nearest layer of the column inside them (the layers outside never load).
/// - The autosave and the exit save it under the chunk it is in.
/// - A [`ChunkViewer`] is never put away: it loads its own chunk, and nothing would load it
///   again. It is saved by the autosave and on exit only, and no codec saves the viewer itself:
///   it comes back as a saved entity without it. A game saves its player apart
///   ([`WorldStore::put_meta`](super::WorldStore::put_meta)), or with a codec of its own that
///   puts the viewer back once something loads its chunk.
/// - Despawning it (or removing the marker) removes its record. Inserting a new `Saved` makes it
///   a new saved entity.
///
/// Its [`SavedId`] is given in the first `PreUpdate` after it is added.
#[derive(Component, Debug, Default)]
#[component(on_insert = saved_inserted, on_discard = saved_discarded)]
pub struct Saved {
    /// 0 until the world gives it one.
    id: u64,
    /// Parts of its record no codec of this run read (an unknown codec, a version a codec does
    /// not read): written back as they were.
    kept: Vec<EntityPart>,
}

impl Saved {
    /// A new saved entity: its id is given when the world first sees it.
    pub fn new() -> Self {
        Self::default()
    }

    /// Its saved id (`None` until the world gives it one).
    pub fn id(&self) -> Option<SavedId> {
        SavedId::from_bits(self.id)
    }
}

/// [`Saved`]'s insert hook: the world looks at the entity in the next entity step.
fn saved_inserted(mut world: DeferredWorld, context: HookContext) {
    if let Some(mut log) = world.get_resource_mut::<SavedLog>() {
        log.added.push(context.entity);
    }
}

/// [`Saved`]'s discard hook (removed, replaced or despawned): its record goes, unless the world
/// itself despawned it (it was put away).
fn saved_discarded(mut world: DeferredWorld, context: HookContext) {
    let Some(id) = world.get::<Saved>(context.entity).map(|s| s.id) else {
        return;
    };
    if id != 0
        && let Some(mut log) = world.get_resource_mut::<SavedLog>()
    {
        log.removed.push((context.entity, id));
    }
}

/// [`Saved`] markers that came or went since the last entity step.
#[derive(Resource, Default)]
pub(crate) struct SavedLog {
    added: Vec<Entity>,
    removed: Vec<(Entity, u64)>,
}

impl SavedLog {
    pub(crate) fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    /// Forgets every marker that came or went.
    pub(crate) fn clear(&mut self) {
        self.added.clear();
        self.removed.clear();
    }
}

/// The saved entities alive, by saved id, for codecs that save references between entities.
pub struct SavedIds<'a> {
    live: &'a HashMap<u64, Entity, Fixed>,
}

impl SavedIds<'_> {
    /// The entity alive with saved id `id`, if it is alive (an entity of a chunk that is not
    /// loaded is not).
    pub fn entity(&self, id: SavedId) -> Option<Entity> {
        self.live.get(&id.0).copied()
    }

    /// The saved id of `entity`, if it is a saved entity that has one.
    pub fn id(&self, world: &World, entity: Entity) -> Option<SavedId> {
        world.get::<Saved>(entity).and_then(Saved::id)
    }
}

/// How one plugin saves its components of [`Saved`] entities: each codec writes its own part of
/// an entity's record, with its name and version, and reads it back. Register it with
/// [`add_entity_codec`] (physics does for its bodies).
///
/// Codecs run on the main thread, in the entity step (`PreUpdate`) and on exit. The parts are
/// read back in the order they were written; a part whose codec is missing in a run, or that its
/// codec cannot read, is kept and written back as it was.
pub trait EntityCodec: Send + Sync + 'static {
    /// The codec's stable name, saved with every part it writes (`boxel:body`). Two codecs of
    /// one app never share it.
    fn name(&self) -> &str;
    /// The version of the bytes [`EntityCodec::save`] writes. [`EntityCodec::load`] gets the
    /// version a part was written with.
    fn version(&self) -> u32;
    /// Where `entity` is, if this codec knows: its record goes under the chunk of this block. The
    /// first codec that answers places the entity; an entity no codec places is not saved.
    fn position(&self, world: &World, entity: Entity) -> Option<BlockPos>;
    /// This codec's part of `entity`, or `None` if the entity has nothing of this codec.
    fn save(&self, world: &World, entity: Entity, ids: &SavedIds<'_>) -> Option<Vec<u8>>;
    /// Whether what this codec saves of `entity` may have changed since the world's change tick
    /// `since` (when the entity's record was last made, or read back). The autosave makes the
    /// record again only for the entities some codec says changed, or that gained or lost a
    /// component: its cost follows what moved, not how many saved entities are alive. The
    /// default says yes (the record is always made again); a codec answers from the change
    /// ticks of the components it saves (`EntityRef::get_change_ticks`).
    fn changed(&self, world: &World, entity: Entity, since: Tick) -> bool {
        let _ = (world, entity, since);
        true
    }
    /// Puts back on `entity` (a new entity with only [`Saved`]) what [`EntityCodec::save`]
    /// wrote with `version`. Every entity of the chunk exists already, so references resolve
    /// through `ids`. An error keeps the part as it was (written back with the entity).
    fn load(
        &self,
        world: &mut World,
        entity: Entity,
        version: u32,
        bytes: &[u8],
        ids: &SavedIds<'_>,
    ) -> Result<(), String>;
}

/// The registered [`EntityCodec`]s (see [`add_entity_codec`]).
#[derive(Resource, Default, Clone)]
pub struct EntityCodecs(Vec<Arc<dyn EntityCodec>>);

impl EntityCodecs {
    /// Adds `codec`.
    ///
    /// # Panics
    /// If a codec with the same name is registered.
    pub fn add(&mut self, codec: Arc<dyn EntityCodec>) {
        assert!(
            self.get(codec.name()).is_none(),
            "two entity codecs named {:?}",
            codec.name()
        );
        self.0.push(codec);
    }

    /// The codec named `name`.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn EntityCodec>> {
        self.0.iter().find(|c| c.name() == name)
    }

    /// Every codec, in the order they were added.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn EntityCodec>> {
        self.0.iter()
    }
}

/// Registers `codec` in `app` (before or after [`PersistPlugin`](super::PersistPlugin): without
/// it nothing is saved).
pub fn add_entity_codec(app: &mut App, codec: impl EntityCodec) {
    app.init_resource::<EntityCodecs>();
    app.world_mut()
        .resource_mut::<EntityCodecs>()
        .add(Arc::new(codec));
}

/// FNV-1a of a record's content: whether an entity's record changed since it was last staged.
fn fingerprint(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The main thread's state of the saved entities (part of `Persist`).
pub(crate) struct EntityIndex {
    /// The chunk whose record (stored or staged) holds each saved id.
    at: HashMap<u64, ChunkKey, Fixed>,
    /// Saved ids per chunk, for the chunks that hold any.
    per_chunk: HashMap<ChunkKey, u32, Fixed>,
    /// The fingerprint of each id's record as it was last staged or read.
    written: HashMap<u64, u64, Fixed>,
    /// The saved entities alive, by saved id.
    pub(crate) live: HashMap<u64, Entity, Fixed>,
    /// For the entities alive whose record is current (made or read back): the world's change
    /// tick then and the entity's archetype. The autosave skips them while no codec says they
    /// changed and their components are the same ones.
    made: HashMap<u64, (Tick, ArchetypeId), Fixed>,
    /// The next id to give.
    next_id: u64,
    /// The next id changed since a save job last took it.
    next_unsaved: bool,
    /// Records and removals staged and not handed to a save job yet, oldest first.
    queue: Vec<(ChunkKey, u64)>,
    queued: HashSet<(ChunkKey, u64), Fixed>,
    /// Records read by chunk loads, waiting for the entity step.
    pub(crate) arrived: Vec<(ChunkKey, Vec<(u64, Vec<u8>)>)>,
    /// A chunk unloaded since the entity step last looked.
    pub(crate) unloaded: bool,
}

impl EntityIndex {
    /// The index of a world whose saved entities are `keys` and whose next id is `next_id`.
    pub(crate) fn new(keys: Vec<(ChunkKey, u64)>, next_id: u64) -> Self {
        let mut index = Self {
            at: HashMap::default(),
            per_chunk: HashMap::default(),
            written: HashMap::default(),
            live: HashMap::default(),
            made: HashMap::default(),
            next_id: next_id.max(1),
            next_unsaved: false,
            queue: Vec::new(),
            queued: HashSet::default(),
            arrived: Vec::new(),
            unloaded: false,
        };
        for (chunk, id) in keys {
            if let Some(first) = index.at.get(&id) {
                // Never written so (a move removes the old record in the same transaction).
                tracing::warn!(id, ?first, also = ?chunk, "a saved entity has two records: the first is used");
                continue;
            }
            index.at.insert(id, chunk);
            *index.per_chunk.entry(chunk).or_default() += 1;
        }
        index
    }

    /// Chunk `key` holds saved entities (stored or staged).
    pub(crate) fn holds(&self, key: ChunkKey) -> bool {
        self.per_chunk.contains_key(&key)
    }

    /// The chunk whose record holds id `id`.
    pub(crate) fn at(&self, id: u64) -> Option<ChunkKey> {
        self.at.get(&id).copied()
    }

    /// Records or removals wait for a save job.
    pub(crate) fn has_ops(&self) -> bool {
        !self.queue.is_empty()
    }

    /// The records and removals for a save job (all of them).
    pub(crate) fn take_ops(&mut self) -> Vec<(ChunkKey, u64)> {
        self.queued.clear();
        std::mem::take(&mut self.queue)
    }

    /// A failed job's records and removals, back in front (they are still staged).
    pub(crate) fn put_back(&mut self, ops: Vec<(ChunkKey, u64)>) {
        let mut back: Vec<(ChunkKey, u64)> = ops
            .into_iter()
            .filter(|op| self.queued.insert(*op))
            .collect();
        back.append(&mut self.queue);
        self.queue = back;
    }

    /// The next id, if a save job must write it.
    pub(crate) fn take_next_id(&mut self) -> Option<u64> {
        std::mem::take(&mut self.next_unsaved).then_some(self.next_id)
    }

    /// The job that took the next id failed: the next one writes it.
    pub(crate) fn next_id_unsaved(&mut self) {
        self.next_unsaved = true;
    }

    /// A new saved id.
    fn give_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.next_unsaved = true;
        id
    }

    fn queue(&mut self, op: (ChunkKey, u64)) {
        if self.queued.insert(op) {
            self.queue.push(op);
        }
    }

    fn uncount(&mut self, chunk: ChunkKey) {
        if let Some(n) = self.per_chunk.get_mut(&chunk) {
            *n -= 1;
            if *n == 0 {
                self.per_chunk.remove(&chunk);
            }
        }
    }

    /// Stages `bytes` as the record of id `id` under `chunk`, removing its record under another
    /// chunk. Nothing is staged when the record is the one already there.
    pub(crate) fn record(&mut self, store: &WorldStore, chunk: ChunkKey, id: u64, bytes: Vec<u8>) {
        let sum = fingerprint(&bytes);
        let old = self.at.get(&id).copied();
        if old == Some(chunk) && self.written.get(&id) == Some(&sum) {
            return;
        }
        if let Some(old) = old.filter(|&o| o != chunk) {
            store.stage_entity(old, id, None);
            self.queue((old, id));
            self.uncount(old);
        }
        if old != Some(chunk) {
            self.at.insert(id, chunk);
            *self.per_chunk.entry(chunk).or_default() += 1;
        }
        self.written.insert(id, sum);
        store.stage_entity(chunk, id, Some(Arc::new(bytes)));
        self.queue((chunk, id));
    }

    /// Stages the removal of id `id`'s record (the entity is gone).
    pub(crate) fn forget(&mut self, store: &WorldStore, id: u64) {
        self.written.remove(&id);
        if let Some(old) = self.at.remove(&id) {
            store.stage_entity(old, id, None);
            self.queue((old, id));
            self.uncount(old);
        }
    }
}

/// The entity step must run: records arrived, a chunk unloaded while saved entities are alive,
/// markers came or went, or the save step asked for a snapshot of the world.
pub(crate) fn entity_work(jobs: Res<WorldJobs>, log: Res<SavedLog>) -> bool {
    let Some(p) = jobs.persist.as_ref() else {
        return false;
    };
    let e = &p.entities;
    !e.arrived.is_empty()
        || (e.unloaded && !e.live.is_empty())
        || !log.is_empty()
        || p.moment == Moment::Asked
}

/// `PreUpdate`: brings back the entities of the chunks that loaded, follows the markers that
/// came or went, puts away the saved entities whose chunk has no slot and, when the save step
/// asked for a snapshot of the world (the autosave), saves every saved entity alive with the
/// edited chunks, the pending ticks and the clock (before the frame's fixed steps: a body comes
/// back before physics steps its chunk, and goes before physics would freeze it there).
pub(crate) fn entity_step(world: &mut World) {
    let (log, codecs, dim, bounds) = step_inputs(world);
    world.resource_scope(|world, mut jobs: Mut<WorldJobs>| {
        // Saving changes nothing the readers of the world's jobs see.
        let Some(p) = jobs.bypass_change_detection().persist.as_mut() else {
            return;
        };
        let mut step = Step {
            world,
            codecs: &codecs,
            p,
            dim,
            bounds,
        };
        for (chunk, records) in std::mem::take(&mut step.p.entities.arrived) {
            step.restore(chunk, records);
        }
        for (entity, id) in log.removed {
            step.forget(entity, id);
        }
        let fresh = step.adopt(log.added);
        if std::mem::take(&mut step.p.entities.unloaded) || fresh {
            step.put_away();
        }
        if step.p.moment == Moment::Asked {
            step.snapshot();
            step.stage_world();
            step.p.moment = Moment::Taken;
        }
    });
}

/// What every run of the entity step reads: the markers that came or went (taken), the codecs,
/// the dimension and the vertical bounds.
fn step_inputs(
    world: &mut World,
) -> (
    SavedLog,
    EntityCodecs,
    DimensionId,
    Option<RangeInclusive<i64>>,
) {
    let settings = world.resource::<WorldSettings>();
    let (dim, bounds) = (settings.dimension, settings.vertical_bounds.clone());
    let log = world
        .get_resource_mut::<SavedLog>()
        .map(|mut log| std::mem::take(&mut *log))
        .unwrap_or_default();
    let codecs = world
        .get_resource::<EntityCodecs>()
        .cloned()
        .unwrap_or_default();
    (log, codecs, dim, bounds)
}

/// `WorldSave`, `SaveSystems::World` (a close and the exit): the world's last moment. The
/// records chunk loads read and no step brought back yet stay as they are on disk; the markers
/// that came or went are followed (a saved entity the game despawned before the close loses its
/// record); every saved entity alive is saved under its chunk, with the edited chunks, the
/// pending ticks, the clock and the game data staged with `WorldLifecycle::stage_meta`, as one
/// moment that the next save job writes whole.
pub(crate) fn close_snapshot(world: &mut World) {
    let (log, codecs, dim, bounds) = step_inputs(world);
    world.resource_scope(|world, mut jobs: Mut<WorldJobs>| {
        let Some(p) = jobs.bypass_change_detection().persist.as_mut() else {
            return;
        };
        let mut step = Step {
            world,
            codecs: &codecs,
            p,
            dim,
            bounds,
        };
        step.p.entities.arrived.clear();
        let forgotten = log
            .removed
            .iter()
            .filter(|&&(entity, id)| step.p.entities.live.get(&id) == Some(&entity))
            .count();
        if forgotten > 0 {
            tracing::debug!(
                forgotten,
                "saved entities despawned before the close lose their records"
            );
        }
        for (entity, id) in log.removed {
            step.forget(entity, id);
        }
        step.adopt(log.added);
        step.snapshot();
        step.stage_world();
        // Whether the autosave's moment was asked or taken, this one covers it.
        step.p.moment = Moment::None;
    });
}

/// `WorldTeardown`, `TeardownSystems::Despawn`: every saved entity alive is despawned, viewers
/// included, after `close_snapshot` made its record (it leaves the index first, so its discard
/// hook removes nothing).
pub(crate) fn put_away_all(world: &mut World) {
    let alive: Vec<Entity> = {
        let Some(mut jobs) = world.get_resource_mut::<WorldJobs>() else {
            return;
        };
        let Some(p) = jobs.bypass_change_detection().persist.as_mut() else {
            return;
        };
        p.entities.made.clear();
        let mut alive: Vec<(u64, Entity)> = p.entities.live.drain().collect();
        alive.sort_unstable_by_key(|&(id, _)| id);
        alive.into_iter().map(|(_, e)| e).collect()
    };
    for entity in alive {
        world.despawn(entity);
    }
}

/// One run of the entity step.
struct Step<'a> {
    world: &'a mut World,
    codecs: &'a EntityCodecs,
    p: &'a mut Persist,
    dim: DimensionId,
    /// `WorldSettings::vertical_bounds`.
    bounds: Option<RangeInclusive<i64>>,
}

impl Step<'_> {
    /// The key of the chunk that holds the entities at `chunk`: that chunk, or the layer of its
    /// column next to it inside the world's vertical bounds. A layer outside the bounds never
    /// loads (physics reads it as air, so a body flies or falls through it): an entity there
    /// stays alive while that layer is loaded, and is saved under it.
    fn key(&self, chunk: ChunkPos) -> ChunkKey {
        let y = match &self.bounds {
            Some(b) if !b.is_empty() => chunk.y.clamp(*b.start(), *b.end()),
            _ => chunk.y,
        };
        ChunkKey {
            dim: self.dim,
            lod: 0,
            pos: ChunkPos::new(chunk.x, y, chunk.z),
        }
    }

    /// The chunk an entity's record goes under: the first codec that places it ([`Step::key`]
    /// of the chunk it is in).
    fn chunk_of(&self, entity: Entity) -> Option<ChunkKey> {
        self.codecs
            .iter()
            .find_map(|c| c.position(self.world, entity))
            .map(|block| self.key(ChunkPos::containing(block)))
    }

    /// Whether the record of the entity alive `entity` (saved id `id`) is the one made or read
    /// last: no codec says it changed since, and it has the same components.
    fn current(&self, id: u64, entity: Entity) -> bool {
        let Some(&(tick, archetype)) = self.p.entities.made.get(&id) else {
            return false;
        };
        self.world
            .get_entity(entity)
            .is_ok_and(|e| e.archetype().id() == archetype)
            && !self
                .codecs
                .iter()
                .any(|c| c.changed(self.world, entity, tick))
    }

    /// Notes that the record of the entity alive `entity` (saved id `id`) is current now.
    fn made(&mut self, id: u64, entity: Entity) {
        let tick = self.world.change_tick();
        if let Ok(e) = self.world.get_entity(entity) {
            let archetype = e.archetype().id();
            self.p.entities.made.insert(id, (tick, archetype));
        }
    }

    /// The content of `entity`'s record (the save job seals it): every codec's part, and the
    /// kept parts no codec wrote.
    fn encode(&self, entity: Entity) -> Vec<u8> {
        let ids = SavedIds {
            live: &self.p.entities.live,
        };
        let mut parts: Vec<EntityPart> = self
            .codecs
            .iter()
            .filter_map(|c| {
                c.save(self.world, entity, &ids).map(|bytes| EntityPart {
                    codec: c.name().to_string(),
                    version: c.version(),
                    bytes,
                })
            })
            .collect();
        if let Some(saved) = self.world.get::<Saved>(entity) {
            for part in &saved.kept {
                if !parts.iter().any(|p| p.codec == part.codec) {
                    parts.push(part.clone());
                }
            }
        }
        entity_payload(&parts)
    }

    /// Brings back the entities of `chunk`'s `records` (their content) that are its newest
    /// records and not alive.
    fn restore(&mut self, chunk: ChunkKey, records: Vec<(u64, Vec<u8>)>) {
        let (mut loaded, mut errors) = (0, 0);
        let mut spawned = Vec::new();
        for (id, bytes) in records {
            let e = &mut self.p.entities;
            if e.at(id) != Some(chunk) || e.live.contains_key(&id) {
                continue;
            }
            match entity_parts(&bytes) {
                Ok(parts) => {
                    let entity = self
                        .world
                        .spawn(Saved {
                            id,
                            kept: Vec::new(),
                        })
                        .id();
                    e.live.insert(id, entity);
                    e.written.insert(id, fingerprint(&bytes));
                    spawned.push((entity, id, parts));
                }
                Err(err) => {
                    errors += 1;
                    tracing::error!(id, chunk = ?chunk.pos, "a saved entity cannot be read: {err}");
                }
            }
        }
        // Every entity of the chunk exists before a codec runs: references between them resolve.
        for (entity, id, parts) in spawned {
            let mut kept = Vec::new();
            for part in parts {
                let ids = SavedIds {
                    live: &self.p.entities.live,
                };
                match self.codecs.get(&part.codec) {
                    Some(codec) => {
                        if let Err(err) =
                            codec.load(self.world, entity, part.version, &part.bytes, &ids)
                        {
                            tracing::error!(
                                id,
                                codec = part.codec.as_str(),
                                version = part.version,
                                "a part of a saved entity cannot be read (kept as it is): {err}"
                            );
                            kept.push(part);
                        }
                    }
                    None => {
                        tracing::warn!(
                            id,
                            codec = part.codec.as_str(),
                            "no codec reads a part of a saved entity (kept as it is)"
                        );
                        kept.push(part);
                    }
                }
            }
            if self.chunk_of(entity).is_none() {
                // Nothing places it: it could never be saved again. Its record stays.
                tracing::error!(id, chunk = ?chunk.pos, "a saved entity cannot be brought back");
                self.p.entities.live.remove(&id);
                self.world.despawn(entity);
                errors += 1;
                continue;
            }
            if !kept.is_empty()
                && let Some(mut saved) = self.world.get_mut::<Saved>(entity)
            {
                saved.kept = kept;
            }
            self.made(id, entity);
            loaded += 1;
        }
        if loaded + errors > 0 {
            self.p.store.count_entities(loaded, errors);
        }
    }

    /// `entity` (saved id `id`) lost its marker or was despawned: its record goes.
    fn forget(&mut self, entity: Entity, id: u64) {
        let e = &mut self.p.entities;
        if e.live.get(&id) == Some(&entity) {
            e.live.remove(&id);
            e.made.remove(&id);
            e.forget(&self.p.store, id);
        }
    }

    /// Gives ids to the new saved entities among `added`. Returns whether there were any.
    fn adopt(&mut self, added: Vec<Entity>) -> bool {
        let mut fresh = false;
        for entity in added {
            let e = &mut self.p.entities;
            let Some(mut saved) = self.world.get_mut::<Saved>(entity) else {
                continue;
            };
            // A restored entity is alive under its id already; a copy of one is a new entity.
            if saved.id != 0 && e.live.get(&saved.id) == Some(&entity) {
                continue;
            }
            let id = e.give_id();
            saved.id = id;
            e.live.insert(id, entity);
            fresh = true;
        }
        fresh
    }

    /// The saved entities alive, by id.
    fn alive(&self) -> Vec<(u64, Entity)> {
        let mut alive: Vec<(u64, Entity)> = self
            .p
            .entities
            .live
            .iter()
            .map(|(&id, &e)| (id, e))
            .collect();
        alive.sort_unstable_by_key(|&(id, _)| id);
        alive
    }

    /// Saves and despawns the saved entities whose chunk has no slot (never a [`ChunkViewer`]).
    fn put_away(&mut self) {
        let map = self.world.resource::<ChunkMap>();
        let away: Vec<(u64, Entity, ChunkKey)> = self
            .alive()
            .into_iter()
            .filter_map(|(id, entity)| {
                // A viewer loads its own chunk: put away, nothing would load it again.
                if self.world.get::<ChunkViewer>(entity).is_some() {
                    return None;
                }
                let chunk = self.chunk_of(entity)?;
                map.id_at(chunk.pos)
                    .is_none()
                    .then_some((id, entity, chunk))
            })
            .collect();
        // Every record first: references between the entities that go resolve. A record that
        // is current under that chunk stays as it is.
        let records: Vec<Option<Vec<u8>>> = away
            .iter()
            .map(|&(id, e, chunk)| {
                (self.p.entities.at(id) != Some(chunk) || !self.current(id, e))
                    .then(|| self.encode(e))
            })
            .collect();
        for ((id, entity, chunk), bytes) in away.into_iter().zip(records) {
            if let Some(bytes) = bytes {
                self.p.entities.record(&self.p.store, chunk, id, bytes);
            }
            self.p.entities.live.remove(&id);
            self.p.entities.made.remove(&id);
            self.world.despawn(entity);
        }
    }

    /// Stages the rest of the snapshot of the world: the loaded chunks with edits, the pending
    /// ticks and the clock (what the save step stages when no saved entity is alive).
    fn stage_world(&mut self) {
        let settings = self
            .world
            .get_resource::<PersistSettings>()
            .cloned()
            .unwrap_or_default();
        let mut map = self.world.resource_mut::<ChunkMap>();
        // Saving changes nothing the readers of the map see.
        self.p.stage_dirty(map.bypass_change_detection());
        save_meta(
            self.p,
            &settings,
            self.world.get_resource::<BlockTicks>(),
            self.world.get_resource::<TimeOfDay>(),
        );
        if let Some(mut lifecycle) = self.world.get_resource_mut::<WorldLifecycle>() {
            stage_game_meta(self.p, &mut lifecycle);
        }
    }

    /// Saves every saved entity alive under its chunk: the ones whose record is not current
    /// (see [`EntityCodec::changed`]), and of those only the records that differ.
    fn snapshot(&mut self) {
        let mut stale: Vec<(u64, Entity)> = self
            .p
            .entities
            .live
            .iter()
            .map(|(&id, &e)| (id, e))
            .filter(|&(id, e)| !self.current(id, e))
            .collect();
        stale.sort_unstable_by_key(|&(id, _)| id);
        for (id, entity) in stale {
            let Some(chunk) = self.chunk_of(entity) else {
                continue;
            };
            let bytes = self.encode(entity);
            self.p.entities.record(&self.p.store, chunk, id, bytes);
            self.made(id, entity);
        }
    }
}
