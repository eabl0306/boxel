//! Player edits at a distance (ADR 0031): the cells of edited chunks come from their blocks.
//!
//! A generator's coarse source knows nothing of what the player built or dug, so the LOD keeps
//! the set of edited chunks: every chunk a [`BlockChanged`](crate::BlockChanged) names and, with
//! persistence, every saved chunk (listed once at startup). For each one it computes a
//! [`Pyramid`]: the chunk's cells at levels 1 to [`PYRAMID_LEVELS`] by the exact cell rule of
//! [`boxel_voxel::lod`] (at those levels a cell lies in exactly one chunk). A column job lays the
//! pyramids of the edited chunks its nodes touch over the source's cells, borders included, and
//! widens the column's band to their layers (a tower above the terrain, a shaft under it).
//!
//! - A changed chunk waits [`LodSettings::edit_debounce`](super::LodSettings::edit_debounce)
//!   after its last change (edits come in clusters, flowing water writes every few ticks), then a
//!   pyramid job reads its blocks (the loaded copy, else the saved one, else the generator's) and
//!   the nodes whose padded cells reach into it are marked stale. They are built again at their
//!   usual priority; the renderer keeps drawing their old meshes until the new ones replace them.
//! - Pyramids are cached ([`PYRAMID_CACHE`], least recently used out). A column that needs one
//!   the cache lacks (after a restart, or evicted) waits for a pyramid job before it runs.
//! - Without persistence an edited chunk that unloads is generated again when it comes back, so
//!   it leaves the set and its nodes go back to the generator's cells. With persistence it stays
//!   (it is saved), and its pyramid is read from the store when needed.
//! - An edited chunk above the generator's top chunk layer raises the plan's ceiling in its
//!   columns ([`Ceiling`](super::plan::Ceiling)): a platform over the terrain gets nodes at
//!   every level, as a tower does.
//! - The nodes an edit reaches are built again by one job per column, so their results land
//!   apart: same-level neighbors change on the renderer together (`tree` module docs).
//! - Level 6 and up (preset `extreme`) does not show edits: a cell spans 8 chunks there.
//!
//! The fallback for generators without a coarse source (`fallback`) builds level-1 cells from
//! the real chunks themselves, so it needs no pyramid: an edit only marks its nodes stale.
use super::plan::{ColumnKey, Span};
use crate::persist::WorldStore;
use crate::{ChunkMap, ChunkSlot, DataState};
use boxel_mesh::lod::LodMaterials;
use boxel_voxel::lod::{CellPartial, LodClasses};
use boxel_voxel::{
    BlockRegistry, BlockState, CHUNK_VOLUME, ChunkData, ChunkKey, ChunkPos, DimensionId,
};
use boxel_worldgen::WorldGenerator;
use boxel_worldgen::lod::{LodCells, LodColumn};
use foldhash::{HashMap, HashSet};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The levels a [`Pyramid`] holds: 1 to 5 (a level-5 cell is one chunk).
pub(crate) const PYRAMID_LEVELS: u8 = 5;
/// Cells of a pyramid: `16^3 + 8^3 + 4^3 + 2^3 + 1`.
const PYRAMID_CELLS: usize = 4096 + 512 + 64 + 8 + 1;
/// Pyramids kept (about 9.4 KiB each: 19 MiB).
pub(crate) const PYRAMID_CACHE: usize = 2048;
/// Chunks one pyramid job reads at most.
pub(crate) const PYRAMIDS_PER_JOB: usize = 8;
/// Pyramid jobs in flight at most (they do not count against the column jobs' cap: they are
/// short and rare).
pub(crate) const PYRAMID_JOBS: usize = 2;

/// Index of block `(x, y, z)` (each `0..32`) in a dense chunk: `y << 10 | x << 5 | z`.
const fn dense_index(x: usize, y: usize, z: usize) -> usize {
    (y << 10) | (x << 5) | z
}

/// The state every cell of a chunk of `state` blocks has, at any level: the cell rule on one
/// state.
fn uniform_cell(classes: &LodClasses, state: BlockState) -> BlockState {
    CellPartial::of_block(classes, [0, 0, 0], state).state()
}

/// The level-1 partials of a chunk that is not uniform, `16^3` of them, index `(y * 16 + z) *
/// 16 + x` in cells; `dense` holds its blocks ([`ChunkData::to_dense`]).
fn level1_partials(classes: &LodClasses, dense: &[BlockState]) -> Vec<CellPartial> {
    let mut out = Vec::with_capacity(4096);
    for cy in 0..16usize {
        for cz in 0..16usize {
            for cx in 0..16usize {
                // Local coordinates: the rule's order is the same anywhere.
                let origin = [2 * cx as i64, 2 * cy as i64, 2 * cz as i64];
                out.push(CellPartial::of_cell(classes, origin, 2, |x, y, z| {
                    dense[dense_index(
                        2 * cx + x as usize,
                        2 * cy + y as usize,
                        2 * cz + z as usize,
                    )]
                }));
            }
        }
    }
    out
}

/// A chunk's cells at level 1 only (`16^3` states, index `(y * 16 + z) * 16 + x`), or its one
/// state when every cell has it: the fallback's view of a chunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Level1 {
    /// Every cell is this state.
    Uniform(BlockState),
    /// The cells.
    Cells(Box<[BlockState]>),
}

impl Level1 {
    /// The level-1 cells of `data`.
    pub(crate) fn of_chunk(classes: &LodClasses, data: &ChunkData) -> Self {
        if let Some(state) = data.as_uniform() {
            return Self::Uniform(uniform_cell(classes, state));
        }
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        data.to_dense(&mut dense);
        let cells: Box<[BlockState]> = level1_partials(classes, &dense)
            .iter()
            .map(CellPartial::state)
            .collect();
        Self::Cells(cells)
    }

    /// The cell at `(x, y, z)` (each `0..16`).
    pub(crate) fn get(&self, x: usize, y: usize, z: usize) -> BlockState {
        match self {
            Self::Uniform(s) => *s,
            Self::Cells(cells) => cells[(y * 16 + z) * 16 + x],
        }
    }
}

/// One chunk's cells at levels 1 to [`PYRAMID_LEVELS`], by the exact cell rule: level 1 from the
/// blocks, each level above from the 8 partials below it (exact while a cell holds one fluid
/// kind, [`CellPartial::merge`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Pyramid {
    /// Level `k`'s `(32 >> k)^3` cells after those of the levels below, index
    /// `(y * n + z) * n + x`.
    cells: Box<[BlockState]>,
}

impl Pyramid {
    /// Where level `k`'s cells start.
    const fn offset(level: u8) -> usize {
        let mut at = 0;
        let mut k = 1;
        while k < level {
            let n = 32 >> k;
            at += n * n * n;
            k += 1;
        }
        at
    }

    /// The pyramid of `data`'s blocks.
    pub(crate) fn of_chunk(classes: &LodClasses, data: &ChunkData) -> Self {
        if let Some(state) = data.as_uniform() {
            let cell = uniform_cell(classes, state);
            return Self {
                cells: vec![cell; PYRAMID_CELLS].into(),
            };
        }
        let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
        data.to_dense(&mut dense);
        let mut level = level1_partials(classes, &dense);
        let mut cells = Vec::with_capacity(PYRAMID_CELLS);
        cells.extend(level.iter().map(CellPartial::state));
        for k in 2..=PYRAMID_LEVELS {
            let n = 32usize >> k;
            let below = 2 * n;
            let mut next = Vec::with_capacity(n * n * n);
            for y in 0..n {
                for z in 0..n {
                    for x in 0..n {
                        // The 8 cells below, in a fixed order (the merge stays deterministic).
                        let mut p: Option<CellPartial> = None;
                        for o in 0..8usize {
                            let (dx, dy, dz) = (o & 1, (o >> 1) & 1, o >> 2);
                            let i = ((2 * y + dy) * below + 2 * z + dz) * below + 2 * x + dx;
                            p = Some(p.map_or(level[i], |p| p.merge(level[i])));
                        }
                        next.push(p.expect("8 cells"));
                    }
                }
            }
            cells.extend(next.iter().map(CellPartial::state));
            level = next;
        }
        debug_assert_eq!(cells.len(), PYRAMID_CELLS);
        Self {
            cells: cells.into(),
        }
    }

    /// The cell at `(x, y, z)` of level `level` (each `0..32 >> level`).
    pub(crate) fn get(&self, level: u8, x: usize, y: usize, z: usize) -> BlockState {
        let n = 32usize >> level;
        self.cells[Self::offset(level) + (y * n + z) * n + x]
    }
}

/// The level-`level` nodes along one axis whose padded cells (one cell of border) reach into
/// chunk coordinate `c`, for levels up to [`PYRAMID_LEVELS`] (a border cell lies in the next
/// chunk): node `n` covers chunks `n << k ..< (n + 1) << k`.
pub(crate) fn nodes_touching(c: i64, level: u8) -> Span {
    let k = u32::from(level);
    (((c + (1 << k) - 1) >> k) - 1, (c + 1) >> k)
}

/// Lays the pyramids `overlays` (chunk position, pyramid) over `cells`, the cells of the
/// level-`level` node at node position `node`: every cell, border included, that lies in one of
/// those chunks takes the pyramid's state.
pub(crate) fn overlay(
    cells: &mut LodCells,
    level: u8,
    node: ChunkPos,
    overlays: &[(ChunkPos, Arc<Pyramid>)],
) {
    // Cells per chunk edge, and the shift from a cell to its chunk (relative to the node).
    let per = 32i64 >> level;
    let shift = 5 - u32::from(level);
    let first = |n: i64| n << level;
    for (chunk, pyramid) in overlays {
        // The chunk's offset from the node's first chunk, then its cells inside -1..=32.
        let range = |c: i64, n: i64| {
            let d = c - first(n);
            let lo = (d * per).max(-1);
            let hi = ((d + 1) * per - 1).min(32);
            (d, lo, hi)
        };
        let (dx, x0, x1) = range(chunk.x, node.x);
        let (dy, y0, y1) = range(chunk.y, node.y);
        let (dz, z0, z1) = range(chunk.z, node.z);
        if x0 > x1 || y0 > y1 || z0 > z1 {
            continue;
        }
        debug_assert_eq!(x0 >> shift, dx);
        for y in y0..=y1 {
            let py = (y - dy * per) as usize;
            for x in x0..=x1 {
                let px = (x - dx * per) as usize;
                for z in z0..=z1 {
                    let pz = (z - dz * per) as usize;
                    cells.set(x as i32, y as i32, z as i32, pyramid.get(level, px, py, pz));
                }
            }
        }
    }
}

/// `band` grown to hold `layers` (the layers edited chunks reach): the layers in between become
/// part of the band. A band of nothing but air takes the layers as they are.
pub(crate) fn widen(band: LodColumn, layers: Option<Span>) -> LodColumn {
    let Some((lo, hi)) = layers else {
        return band;
    };
    if band.lo >= band.hi && band.below == BlockState::AIR {
        return LodColumn {
            lo,
            hi: hi + 1,
            below: band.below,
        };
    }
    LodColumn {
        lo: band.lo.min(lo),
        hi: band.hi.max(hi + 1),
        below: band.below,
    }
}

/// Where a pyramid job reads a chunk's blocks.
#[derive(Clone, Debug)]
pub(crate) enum ChunkSource {
    /// Its loaded blocks, as they were when the job was made.
    Loaded(Arc<ChunkData>),
    /// Loaded as `Missing`: air.
    Air,
    /// Not loaded: the saved copy, else the generator's.
    Unloaded,
}

impl ChunkSource {
    /// Where chunk `pos` of `map` reads now.
    pub(crate) fn of(map: &ChunkMap, pos: ChunkPos) -> Self {
        match map
            .id_at(pos)
            .and_then(|id| map.get(id))
            .map(ChunkSlot::data)
        {
            Some(DataState::Ready(data)) => Self::Loaded(Arc::clone(data)),
            Some(DataState::Missing) => Self::Air,
            _ => Self::Unloaded,
        }
    }

    /// The blocks of chunk `key`. An unloaded chunk that was never saved is the generator's:
    /// air outside its [`vertical_chunks`](WorldGenerator::vertical_chunks), as the world makes
    /// it.
    pub(crate) fn read(
        &self,
        key: ChunkKey,
        store: Option<&WorldStore>,
        generator: &dyn WorldGenerator,
        registry: &BlockRegistry,
    ) -> Arc<ChunkData> {
        match self {
            Self::Loaded(data) => Arc::clone(data),
            Self::Air => Arc::new(ChunkData::uniform(BlockState::AIR)),
            Self::Unloaded => store.and_then(|s| s.read_chunk(key)).unwrap_or_else(|| {
                Arc::new(if generator.vertical_chunks().contains(&key.pos.y) {
                    generator.generate(key, registry)
                } else {
                    ChunkData::uniform(BlockState::AIR)
                })
            }),
        }
    }
}

/// The input of a pyramid job: chunks and where to read them.
pub(crate) struct PyramidInput {
    pub(crate) chunks: Vec<(ChunkKey, ChunkSource)>,
    pub(crate) store: Option<WorldStore>,
    pub(crate) generator: Arc<dyn WorldGenerator>,
    pub(crate) registry: Arc<BlockRegistry>,
    pub(crate) materials: Arc<LodMaterials>,
}

/// A pyramid job's body. `None` when `alive` went off on the way (the world closed).
pub(crate) fn run_pyramids(
    input: &PyramidInput,
    alive: &AtomicBool,
) -> Option<Vec<(ChunkKey, Arc<Pyramid>)>> {
    input
        .chunks
        .iter()
        .map(|(key, source)| {
            if !alive.load(Ordering::Acquire) {
                return None;
            }
            let data = source.read(
                *key,
                input.store.as_ref(),
                &*input.generator,
                &input.registry,
            );
            let classes = input.materials.classes();
            Some((*key, Arc::new(Pyramid::of_chunk(classes, &data))))
        })
        .collect()
}

/// The pyramids kept, least recently used out.
#[derive(Debug, Default)]
struct Cache {
    map: HashMap<ChunkKey, (Arc<Pyramid>, u64)>,
    by_use: BTreeMap<u64, ChunkKey>,
    tick: u64,
}

impl Cache {
    fn get(&mut self, key: ChunkKey) -> Option<Arc<Pyramid>> {
        let (pyramid, used) = self.map.get_mut(&key)?;
        self.by_use.remove(used);
        self.tick += 1;
        *used = self.tick;
        self.by_use.insert(self.tick, key);
        Some(Arc::clone(pyramid))
    }

    #[cfg(test)]
    fn contains(&self, key: ChunkKey) -> bool {
        self.map.contains_key(&key)
    }

    fn insert(&mut self, key: ChunkKey, pyramid: Arc<Pyramid>, cap: usize) {
        self.remove(key);
        self.tick += 1;
        self.map.insert(key, (pyramid, self.tick));
        self.by_use.insert(self.tick, key);
        while self.map.len() > cap {
            let Some((_, oldest)) = self.by_use.pop_first() else {
                break;
            };
            self.map.remove(&oldest);
        }
    }

    fn remove(&mut self, key: ChunkKey) {
        if let Some((_, used)) = self.map.remove(&key) {
            self.by_use.remove(&used);
        }
    }

    fn len(&self) -> usize {
        self.map.len()
    }
}

/// What a column job of the edits gets ([`Edits::for_column`]).
#[derive(Debug)]
pub(crate) enum ForColumn {
    /// The pyramids its nodes touch and the layers every edited chunk of the column reaches.
    Ready {
        overlays: Vec<(ChunkPos, Arc<Pyramid>)>,
        layers: Option<Span>,
    },
    /// Some pyramids are not cached: the column waits for them (they are asked for).
    Wait,
}

/// The edited chunks and their pyramids (module docs).
#[derive(Debug)]
pub(crate) struct Edits {
    /// Every edited chunk (lod-0 keys).
    edited: HashSet<ChunkKey>,
    /// Per level (1..=5) and column, the edited chunks whose padded cells it reaches.
    index: HashMap<(u8, i64, i64), Vec<ChunkKey>>,
    /// Changed chunks waiting out the debounce: their last change.
    debounce: HashMap<ChunkKey, Instant>,
    cache: Cache,
    cap: usize,
    /// Chunks whose pyramid is asked for, in order (`true`: their blocks changed, so their nodes
    /// are built again once it comes), and the same keys as a set.
    wanted: VecDeque<(ChunkKey, bool)>,
    asked: HashSet<ChunkKey>,
    /// Chunks whose pyramid job runs: the job and whether the blocks changed.
    computing: HashMap<ChunkKey, (u64, bool)>,
    /// Pyramid jobs in flight: their chunks.
    jobs: HashMap<u64, Vec<ChunkKey>>,
    /// Columns waiting for pyramids, by chunk.
    waiting: HashMap<ChunkKey, Vec<ColumnKey>>,
    /// Chunks whose pyramid job failed: their nodes show the generator's cells.
    broken: HashSet<ChunkKey>,
    /// The generator's top chunk layer (`None`: unbounded, or not known yet), and the edited
    /// chunks above it: they raise the plan's [`Ceiling`](super::plan::Ceiling).
    top: Option<i64>,
    above: HashSet<ChunkKey>,
    /// `above` changed since [`Edits::take_above_changed`].
    above_changed: bool,
    /// The saved chunks were listed (startup).
    pub(crate) seeded: bool,
}

impl Default for Edits {
    fn default() -> Self {
        Self::new(PYRAMID_CACHE)
    }
}

impl Edits {
    /// No edit yet; at most `cap` pyramids kept.
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            edited: HashSet::default(),
            index: HashMap::default(),
            debounce: HashMap::default(),
            cache: Cache::default(),
            cap: cap.max(1),
            wanted: VecDeque::new(),
            asked: HashSet::default(),
            computing: HashMap::default(),
            jobs: HashMap::default(),
            waiting: HashMap::default(),
            broken: HashSet::default(),
            top: None,
            above: HashSet::default(),
            above_changed: false,
            seeded: false,
        }
    }

    /// Edited chunks.
    pub(crate) fn len(&self) -> usize {
        self.edited.len()
    }

    /// Pyramids kept.
    pub(crate) fn cached(&self) -> usize {
        self.cache.len()
    }

    /// True while something is left to do: changes waiting out the debounce, pyramids asked for
    /// or computing, columns waiting.
    pub(crate) fn busy(&self) -> bool {
        !self.debounce.is_empty()
            || !self.wanted.is_empty()
            || !self.computing.is_empty()
            || !self.waiting.is_empty()
    }

    /// Adds chunk `key` to the edited set (a saved chunk, or a changed one).
    pub(crate) fn insert(&mut self, key: ChunkKey) {
        if !self.edited.insert(key) {
            return;
        }
        if self.top.is_some_and(|t| key.pos.y > t) {
            self.above.insert(key);
            self.above_changed = true;
        }
        for level in 1..=PYRAMID_LEVELS {
            let (x0, x1) = nodes_touching(key.pos.x, level);
            let (z0, z1) = nodes_touching(key.pos.z, level);
            for x in x0..=x1 {
                for z in z0..=z1 {
                    self.index.entry((level, x, z)).or_default().push(key);
                }
            }
        }
    }

    /// The generator's top chunk layer is `top` (`None`: unbounded): the edited chunks above it
    /// are listed again.
    pub(crate) fn set_top(&mut self, top: Option<i64>) {
        if self.top == top {
            return;
        }
        self.top = top;
        self.above = match top {
            Some(t) => self
                .edited
                .iter()
                .filter(|k| k.pos.y > t)
                .copied()
                .collect(),
            None => HashSet::default(),
        };
        self.above_changed = true;
    }

    /// True if the edited chunks above the generator's top changed since the last call.
    pub(crate) fn take_above_changed(&mut self) -> bool {
        std::mem::take(&mut self.above_changed)
    }

    /// The edited chunks of `dim` above the generator's top.
    pub(crate) fn above(&self, dim: DimensionId) -> impl Iterator<Item = ChunkPos> + '_ {
        self.above
            .iter()
            .filter(move |k| k.dim == dim)
            .map(|k| k.pos)
    }

    /// Chunk `key` changed at `now` (a [`BlockChanged`](crate::BlockChanged) there).
    pub(crate) fn changed(&mut self, key: ChunkKey, now: Instant) {
        self.insert(key);
        self.debounce.insert(key, now);
    }

    /// Drops chunk `key` from the set (it unloaded without persistence): its pyramid, its wait
    /// and its jobs go, and the columns waiting for it may go on. True if it was edited.
    pub(crate) fn remove(&mut self, key: ChunkKey) -> (bool, Vec<ColumnKey>) {
        if !self.edited.remove(&key) {
            return (false, Vec::new());
        }
        if self.above.remove(&key) {
            self.above_changed = true;
        }
        for level in 1..=PYRAMID_LEVELS {
            let (x0, x1) = nodes_touching(key.pos.x, level);
            let (z0, z1) = nodes_touching(key.pos.z, level);
            for x in x0..=x1 {
                for z in z0..=z1 {
                    if let Some(list) = self.index.get_mut(&(level, x, z)) {
                        list.retain(|&k| k != key);
                        if list.is_empty() {
                            self.index.remove(&(level, x, z));
                        }
                    }
                }
            }
        }
        self.debounce.remove(&key);
        self.cache.remove(key);
        self.computing.remove(&key);
        if self.asked.remove(&key) {
            self.wanted.retain(|&(k, _)| k != key);
        }
        self.broken.remove(&key);
        (true, self.waiting.remove(&key).unwrap_or_default())
    }

    /// The chunks whose debounce ended by `now` (their pyramids are asked for, as changed).
    pub(crate) fn debounced(&mut self, now: Instant, debounce: Duration) -> Vec<ChunkKey> {
        let mut due = Vec::new();
        self.debounce.retain(|&key, &mut last| {
            let wait = now.saturating_duration_since(last) < debounce;
            if !wait {
                due.push(key);
            }
            wait
        });
        due.sort_unstable();
        due
    }

    /// Asks for chunk `key`'s pyramid (`changed`: its blocks changed, so whatever is cached or
    /// computing for it is out of date).
    pub(crate) fn ask(&mut self, key: ChunkKey, changed: bool) {
        if changed {
            self.cache.remove(key);
            self.computing.remove(&key);
            self.broken.remove(&key);
            if let Some(entry) = self.wanted.iter_mut().find(|(k, _)| *k == key) {
                entry.1 = true;
            }
        } else if self.computing.contains_key(&key) {
            return;
        }
        if self.asked.insert(key) {
            self.wanted.push_back((key, changed));
        }
    }

    /// True if pyramids are asked for and a pyramid job may start.
    pub(crate) fn wants_jobs(&self) -> bool {
        !self.wanted.is_empty() && self.jobs.len() < PYRAMID_JOBS
    }

    /// The next chunks of a pyramid job ([`PYRAMIDS_PER_JOB`] at most), noted as computing by
    /// job `id`; `None` if none is asked for or [`PYRAMID_JOBS`] run.
    pub(crate) fn next_job(&mut self, id: u64) -> Option<Vec<ChunkKey>> {
        if self.wanted.is_empty() || self.jobs.len() >= PYRAMID_JOBS {
            return None;
        }
        let mut chunks = Vec::new();
        while chunks.len() < PYRAMIDS_PER_JOB {
            let Some((key, changed)) = self.wanted.pop_front() else {
                break;
            };
            self.asked.remove(&key);
            self.computing.insert(key, (id, changed));
            chunks.push(key);
        }
        self.jobs.insert(id, chunks.clone());
        Some(chunks)
    }

    /// True if job `id` is a pyramid job.
    pub(crate) fn is_job(&self, id: u64) -> bool {
        self.jobs.contains_key(&id)
    }

    /// Pyramid job `id` ended with `pyramids` (none if it failed). Returns the chunks whose
    /// blocks changed (their nodes are built again) and the columns that waited for them.
    pub(crate) fn done(
        &mut self,
        id: u64,
        pyramids: Vec<(ChunkKey, Arc<Pyramid>)>,
    ) -> (Vec<ChunkKey>, Vec<ColumnKey>) {
        let chunks = self.jobs.remove(&id).unwrap_or_default();
        let mut changed = Vec::new();
        let mut ready = Vec::new();
        let mut got: HashSet<ChunkKey> = HashSet::default();
        for (key, pyramid) in pyramids {
            got.insert(key);
            // Only the newest job of a chunk counts (a change asks again).
            if self.computing.get(&key).is_none_or(|&(job, _)| job != id) {
                continue;
            }
            let (_, was_changed) = self.computing.remove(&key).expect("checked");
            self.cache.insert(key, pyramid, self.cap);
            if was_changed {
                changed.push(key);
            }
            ready.extend(self.waiting.remove(&key).unwrap_or_default());
        }
        for key in chunks {
            if got.contains(&key) || self.computing.get(&key).is_none_or(|&(job, _)| job != id) {
                continue;
            }
            // Failed: shown as the generator has it, logged by the caller.
            self.computing.remove(&key);
            self.broken.insert(key);
            ready.extend(self.waiting.remove(&key).unwrap_or_default());
        }
        (changed, ready)
    }

    /// The edited chunks of `dim` whose padded cells column `c` (level 5 at most) reaches, any
    /// layer.
    fn touching(&self, c: ColumnKey, dim: DimensionId) -> impl Iterator<Item = ChunkKey> + '_ {
        let list = if c.level > PYRAMID_LEVELS {
            &[][..]
        } else {
            self.index
                .get(&(c.level, c.x, c.z))
                .map_or(&[][..], Vec::as_slice)
        };
        list.iter().copied().filter(move |k| k.dim == dim)
    }

    /// The node layers of column `c` of `dim` the edited chunks reach (`None`: none).
    pub(crate) fn layers(&self, c: ColumnKey, dim: DimensionId) -> Option<Span> {
        self.touching(c, dim)
            .map(|key| nodes_touching(key.pos.y, c.level))
            .reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
    }

    /// Forgets every pyramid (the registry changed): they are computed again when needed.
    pub(crate) fn forget_pyramids(&mut self) {
        self.cache = Cache::default();
        self.computing.clear();
        self.broken.clear();
    }

    /// What a job of column `c` of `dim` building `layers` needs (see [`ForColumn`]); asks for
    /// the pyramids it lacks and notes the column as waiting for them.
    pub(crate) fn for_column(&mut self, c: ColumnKey, layers: Span, dim: DimensionId) -> ForColumn {
        let touching: Vec<ChunkKey> = self.touching(c, dim).collect();
        if touching.is_empty() {
            return ForColumn::Ready {
                overlays: Vec::new(),
                layers: None,
            };
        }
        let all = self.layers(c, dim);
        let mut overlays = Vec::new();
        let mut missing = Vec::new();
        for key in touching {
            let (lo, hi) = nodes_touching(key.pos.y, c.level);
            if hi < layers.0 || lo > layers.1 || self.broken.contains(&key) {
                continue;
            }
            match self.cache.get(key) {
                Some(p) => overlays.push((key.pos, p)),
                None => missing.push(key),
            }
        }
        // A column reaching more edited chunks than half the cache would wait for ever (the
        // pyramids it waits for evict the ones it has): it goes on with the cached ones.
        if missing.is_empty() || overlays.len() + missing.len() > self.cap / 2 {
            for key in missing {
                self.ask(key, false);
            }
            return ForColumn::Ready {
                overlays,
                layers: all,
            };
        }
        for key in missing {
            self.ask(key, false);
            let list = self.waiting.entry(key).or_default();
            if !list.contains(&c) {
                list.push(c);
            }
        }
        ForColumn::Wait
    }

    /// Testing: true if chunk `key`'s pyramid is cached.
    #[cfg(test)]
    pub(crate) fn has_pyramid(&self, key: ChunkKey) -> bool {
        self.cache.contains(key)
    }

    /// Testing: drops every cached pyramid (as if evicted).
    #[cfg(test)]
    pub(crate) fn evict_all(&mut self) {
        self.cache = Cache::default();
    }
}
