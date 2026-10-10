//! Sky tops of the loaded chunk columns (ADR 0015).
//!
//! [`SkyMap`] holds one [`SkyColumn`] per chunk column with at least one loaded slot: created
//! with the column's first slot, dropped with its last. Its [`SkyTile`] (the sky top `T` of each
//! of its 32x32 columns) is computed by a [`JobKind::SkyTile`](crate::scheduler::JobKind) job
//! that walks the column down ([`WalkJob`]), only once a mesh above the walk floor needs it.
//!
//! `T` depends on the world content only (the generator plus the edits of loaded chunks), never
//! on what happens to be loaded, so streaming never changes it. Edits update a tile in place
//! (`SkyTile::raise`, `SkyTile::rescan_column`) or send the column back to `Pending` for a new
//! walk; a walk result whose column changed meanwhile (its `version`) is discarded.
use crate::ChunkId;
use crate::persist::WorldStore;
use boxel_ecs::prelude::*;
use boxel_light::sky_tile::{GeneratorTop, walk, walk_floor};
use boxel_light::{LayerData, SkyTile};
use boxel_voxel::{
    CHUNK_SHIFT, ChunkData, ChunkKey, ChunkPos, DimensionId, LightTables, MAX_CHUNK_COORD,
};
use boxel_worldgen::WorldGenerator;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

slotmap::new_key_type! {
    /// Generational handle of a chunk column in the [`SkyMap`].
    pub struct ColumnId;
}

/// Top of a chunk's light box above its origin: the box is the chunk +- 16 voxels.
pub(crate) const BOX_TOP: i64 = 31 + 16;

/// How the world finds its sky tops, fixed when it starts (`WorldSettings::light`,
/// `WorldSettings::sky_walk_limit` and the generator's range).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SkyRule {
    /// The world computes no light: meshes see open sky everywhere (the look before ADR 0015).
    Off,
    /// Light without sky tops: every column is open sky. For a generator that does not declare
    /// where its content ends (`WorldGenerator::vertical_chunks` left at its default): there is
    /// no start to walk from (`GeneratorTop::Unbounded`).
    Open,
    /// Columns are walked down.
    Walk(WalkRule),
}

/// The world-wide part of a walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WalkRule {
    /// The generator's top voxel (`None`: it places nothing; only edits block the sky).
    pub(crate) top: Option<i64>,
    /// Columns still open here get `T = floor` (`i64::MIN` without a generator top).
    pub(crate) floor: i64,
    /// The generator's chunk layers.
    pub(crate) layers: RangeInclusive<i64>,
    /// True if a walk can reach the void (below a bounded generator) above the floor: a column
    /// may then be open forever (`T = i64::MIN`), so even a box below the floor needs its tiles.
    pub(crate) void_reachable: bool,
}

impl SkyRule {
    /// The rule of a world with light on or off, a generator of chunk layers `layers` and a walk
    /// limit of `limit` voxels.
    pub(crate) fn new(light: bool, layers: RangeInclusive<i64>, limit: u32) -> Self {
        if !light {
            return SkyRule::Off;
        }
        match GeneratorTop::of(&layers) {
            GeneratorTop::Unbounded => SkyRule::Open,
            GeneratorTop::Empty => SkyRule::Walk(WalkRule {
                top: None,
                floor: i64::MIN,
                layers,
                void_reachable: true,
            }),
            GeneratorTop::Voxel(top) => {
                let floor = walk_floor(top, limit);
                // The walk reads the layer under the generator's range (as void) only if that
                // layer is not wholly under the floor.
                let bottom = *layers.start();
                let void_reachable = bottom >= -MAX_CHUNK_COORD && (bottom << CHUNK_SHIFT) > floor;
                SkyRule::Walk(WalkRule {
                    top: Some(top),
                    floor,
                    layers,
                    void_reachable,
                })
            }
        }
    }
}

impl WalkRule {
    /// Where a walk of a column whose highest edited blocker is `edit_top` starts: `None` if the
    /// column has no content at all (every top is `i64::MIN`).
    pub(crate) fn start(&self, edit_top: i64) -> Option<i64> {
        let s = self.top.unwrap_or(i64::MIN).max(edit_top);
        (s != i64::MIN).then_some(s)
    }
}

/// Where a column's tile stands.
#[derive(Clone, Debug)]
pub(crate) enum SkyState {
    /// Not walked yet, or to walk again (`old`: the tile before, to remesh what changed).
    Pending { old: Option<Arc<SkyTile>> },
    /// A walk of this column `version` is running.
    InFlight {
        version: u32,
        old: Option<Arc<SkyTile>>,
    },
    /// The column's tops.
    Exact(Arc<SkyTile>),
}

/// The sky tile slot of one chunk column.
#[derive(Debug)]
pub(crate) struct SkyColumn {
    /// Chunk column x.
    pub(crate) x: i64,
    /// Chunk column z.
    pub(crate) z: i64,
    /// Dimension of the column's chunks.
    pub(crate) dim: DimensionId,
    pub(crate) state: SkyState,
    /// Bumped by every change of the column's content that may move a top while it is not
    /// `Exact` (a running walk read the old content: its result is discarded).
    pub(crate) version: u32,
    /// An edit changed a voxel that stops direct sky in this column.
    pub(crate) edited: bool,
    /// Highest blocker placed by an edit (`i64::MIN`: none). Never lowered: a stale value only
    /// makes a walk start higher, through air.
    pub(crate) edit_top: i64,
    /// Layers of the column's loaded slots, ascending (one entry per slot). Loops over the
    /// column visit these instead of every layer of a range, which may span `2^56` layers
    /// (viewers far apart in the same column, or an edit far above or below the others).
    pub(crate) layers: Vec<i64>,
    /// A walk job is in the scheduler.
    pub(crate) queued: bool,
    /// Mesh slots waiting for this tile (may hold stale or repeated ids; each is checked when
    /// the tile lands).
    pub(crate) waiters: Vec<ChunkId>,
    /// The walk waits for this slot's data (a layer it would otherwise generate privately).
    pub(crate) waiting_for: Option<ChunkId>,
    /// The edit batch that sent the column back to `Pending`: the chunks the new tile remeshes
    /// join it.
    pub(crate) batch: Option<u32>,
    /// Set when the column is dropped (a running walk stops).
    pub(crate) cancel: Arc<AtomicBool>,
}

impl SkyColumn {
    /// The tile if `Exact`.
    pub(crate) fn exact(&self) -> Option<&Arc<SkyTile>> {
        match &self.state {
            SkyState::Exact(tile) => Some(tile),
            _ => None,
        }
    }
    /// The loaded layers in `lo..=hi`, ascending.
    pub(crate) fn layers_in(&self, lo: i64, hi: i64) -> &[i64] {
        let start = self.layers.partition_point(|&y| y < lo);
        let end = self.layers.partition_point(|&y| y <= hi);
        &self.layers[start..end.max(start)]
    }
    /// The highest loaded layer (`i64::MIN` without slots).
    pub(crate) fn hi(&self) -> i64 {
        self.layers.last().copied().unwrap_or(i64::MIN)
    }
}

/// Sky tops of the loaded chunk columns (ADR 0015): one entry per chunk column with a loaded
/// slot, holding its [`SkyTile`] once walked. Read-only for apps; the world systems keep it.
#[derive(Resource, Debug)]
pub struct SkyMap {
    pub(crate) columns: slotmap::SlotMap<ColumnId, SkyColumn>,
    index: HashMap<(i64, i64), ColumnId, foldhash::fast::FixedState>,
    pub(crate) rule: SkyRule,
    /// Every column open forever (`T = i64::MIN`).
    pub(crate) open: Arc<SkyTile>,
}

impl SkyMap {
    pub(crate) fn new(rule: SkyRule) -> Self {
        Self {
            columns: slotmap::SlotMap::with_key(),
            index: HashMap::default(),
            rule,
            open: Arc::new(SkyTile::uniform(i64::MIN, i64::MIN)),
        }
    }
    /// Chunk columns with a loaded slot.
    pub fn len(&self) -> usize {
        self.columns.len()
    }
    /// No column.
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
    /// The walked tile of chunk column `(cx, cz)`, if loaded and walked (not while a walk of it
    /// is pending or running).
    pub fn tile(&self, cx: i64, cz: i64) -> Option<&Arc<SkyTile>> {
        self.columns.get(self.id_at(cx, cz)?)?.exact()
    }
    /// The sky top of world column `(x, z)`: its highest voxel that stops direct sky (a voxel
    /// above it is direct sky), `i64::MIN` if open forever. `None` if its tile is not walked.
    pub fn top(&self, x: i64, z: i64) -> Option<i64> {
        let tile = self.tile(x >> CHUNK_SHIFT, z >> CHUNK_SHIFT)?;
        Some(tile.top((x & 31) as u32, (z & 31) as u32))
    }
    /// Columns with a walked tile.
    pub fn walked(&self) -> usize {
        self.columns
            .values()
            .filter(|c| c.exact().is_some())
            .count()
    }
    /// The walk floor of the world: no sky top is below it, except `i64::MIN` (open forever).
    /// `None` when the world walks no column (light off, or a generator without a top).
    pub fn floor(&self) -> Option<i64> {
        match &self.rule {
            SkyRule::Walk(rule) => Some(rule.floor),
            SkyRule::Off | SkyRule::Open => None,
        }
    }
    /// Heap bytes of the tiles (8 KiB each) and the column entries.
    pub fn heap_bytes(&self) -> usize {
        let tiles = self
            .columns
            .values()
            .filter_map(|c| match &c.state {
                SkyState::Exact(t) if !Arc::ptr_eq(t, &self.open) => Some(size_of_val(t.tops())),
                _ => None,
            })
            .sum::<usize>();
        let layers = self
            .columns
            .values()
            .map(|c| c.layers.capacity() * size_of::<i64>())
            .sum::<usize>();
        tiles
            + layers
            + self.columns.capacity() * size_of::<SkyColumn>()
            + self.index.capacity() * size_of::<((i64, i64), ColumnId)>()
    }
    /// The column at `(cx, cz)`.
    pub(crate) fn id_at(&self, cx: i64, cz: i64) -> Option<ColumnId> {
        self.index.get(&(cx, cz)).copied()
    }
    /// A slot at `pos` was created: its column, created if needed.
    pub(crate) fn attach(&mut self, pos: ChunkPos, dim: DimensionId) -> ColumnId {
        if let Some(id) = self.id_at(pos.x, pos.z) {
            let layers = &mut self.columns[id].layers;
            let i = layers.partition_point(|&y| y < pos.y);
            debug_assert!(layers.get(i) != Some(&pos.y), "two slots at {pos:?}");
            layers.insert(i, pos.y);
            return id;
        }
        let id = self.columns.insert(SkyColumn {
            x: pos.x,
            z: pos.z,
            dim,
            state: SkyState::Pending { old: None },
            version: 0,
            edited: false,
            edit_top: i64::MIN,
            layers: vec![pos.y],
            queued: false,
            waiters: Vec::new(),
            waiting_for: None,
            batch: None,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        self.index.insert((pos.x, pos.z), id);
        id
    }
    /// The slot of column `id` at layer `y` was removed; the column goes with its last slot
    /// (true).
    pub(crate) fn detach(&mut self, id: ColumnId, y: i64) -> bool {
        let Some(c) = self.columns.get_mut(id) else {
            return false;
        };
        if let Ok(i) = c.layers.binary_search(&y) {
            c.layers.remove(i);
        } else {
            debug_assert!(false, "no slot at layer {y} of column ({}, {})", c.x, c.z);
        }
        if !c.layers.is_empty() {
            return false;
        }
        c.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        let (x, z) = (c.x, c.z);
        self.columns.remove(id);
        self.index.remove(&(x, z));
        true
    }
    /// True if the light box of the chunk at `pos` needs the tiles of its 3x3 columns: some
    /// voxel of it may be direct sky. False when light is off, every column is open, or the box
    /// is wholly at or under the floor of a world whose columns never reach the void (every top
    /// is then at the floor or above, so the box has no direct sky).
    pub(crate) fn needs_tiles(&self, pos: ChunkPos) -> bool {
        match &self.rule {
            SkyRule::Walk(rule) => {
                rule.void_reachable || pos.origin().y.saturating_add(BOX_TOP) > rule.floor
            }
            SkyRule::Off | SkyRule::Open => false,
        }
    }
}

/// Chunk coordinates (inclusive) along one axis whose padded region (local `-1..=32`, what a
/// mesh and its light read) meets voxels `lo..=hi`.
pub(crate) fn padded_chunks(lo: i64, hi: i64) -> RangeInclusive<i64> {
    (lo.saturating_sub(1) >> CHUNK_SHIFT)..=(hi.saturating_add(1) >> CHUNK_SHIFT)
}

/// The highest of `layers` (ascending) below `y`.
pub(crate) fn layer_below(layers: &[i64], y: i64) -> Option<i64> {
    let i = layers.partition_point(|&l| l < y);
    i.checked_sub(1).map(|i| layers[i])
}

/// A captured layer: its data, or air for a chunk whose generation failed.
fn read(data: Option<&ChunkData>) -> LayerData<'_> {
    data.map_or(LayerData::Air, LayerData::Ref)
}

/// True if `data` holds a voxel that stops direct sky (from its light masks).
pub(crate) fn may_block(data: &ChunkData, tables: &LightTables) -> bool {
    if let Some(s) = data.as_uniform() {
        return tables.blocks_sky(s);
    }
    let masks = data.light_masks(tables);
    (0..boxel_voxel::CHUNK_AREA).any(|r| masks.opaque.row(r) | masks.partial.row(r) != 0)
}

/// A sky walk, captured on the main thread and run on a worker.
pub(crate) struct WalkJob {
    pub(crate) x: i64,
    pub(crate) z: i64,
    pub(crate) dim: DimensionId,
    /// First voxel read (`S`).
    pub(crate) start: i64,
    pub(crate) floor: i64,
    /// The generator's chunk layers.
    pub(crate) layers: RangeInclusive<i64>,
    /// The loaded layers of the walk's range, highest first: their data, or `None` for a chunk
    /// whose generation failed (air).
    pub(crate) captured: Vec<(i64, Option<Arc<ChunkData>>)>,
    /// A loaded chunk under the walk's range may hold a blocker (only looked at when the walk
    /// can reach the void): layers under the generator's range are then air, not void.
    pub(crate) content_below: bool,
    /// Saved layers of the walk's range that are not loaded (ADR 0024): read from `store` before
    /// the walk and walked like `captured` ones.
    pub(crate) saved: Vec<i64>,
    /// Saved layers under the walk's range that are not loaded (only where the walk can reach
    /// the void): one with a blocker counts like `content_below`.
    pub(crate) saved_below: Vec<i64>,
    /// The world's store, when `saved` or `saved_below` is not empty.
    pub(crate) store: Option<WorldStore>,
}

impl WalkJob {
    /// Walks the column: loaded layers from `captured`, saved layers from the store, the rest
    /// generated privately (a generation that panics reads as air, as a `Missing` chunk does)
    /// or known air outside the generator's range. Returns the tile and how many chunks were
    /// generated.
    pub(crate) fn run(
        mut self,
        generator: &dyn WorldGenerator,
        registry: &boxel_voxel::BlockRegistry,
        tables: &LightTables,
    ) -> (SkyTile, u32) {
        let mut generated = 0;
        self.read_saved(generator, registry, tables, &mut generated);
        let tile = walk(self.start, self.floor, tables, |cy| {
            self.layer(cy, tables, |pos| {
                generated += 1;
                self.generate(generator, registry, pos)
            })
        });
        (tile, generated)
    }

    /// A private generation of the chunk at `pos` (`None` if the generator panics).
    fn generate(
        &self,
        generator: &dyn WorldGenerator,
        registry: &boxel_voxel::BlockRegistry,
        pos: ChunkPos,
    ) -> Option<ChunkData> {
        let key = ChunkKey {
            dim: self.dim,
            lod: 0,
            pos,
        };
        catch_unwind(AssertUnwindSafe(|| generator.generate(key, registry))).ok()
    }

    /// Reads the saved layers that are not loaded into `captured`, like loaded ones (a record
    /// that cannot be read is generated, as its load would be), and looks for a blocker in the
    /// saved layers under the range.
    fn read_saved(
        &mut self,
        generator: &dyn WorldGenerator,
        registry: &boxel_voxel::BlockRegistry,
        tables: &LightTables,
        generated: &mut u32,
    ) {
        let Some(store) = self.store.take() else {
            return;
        };
        let key = |cy| ChunkKey {
            dim: self.dim,
            lod: 0,
            pos: ChunkPos::new(self.x, cy, self.z),
        };
        for cy in std::mem::take(&mut self.saved) {
            let data = store.read_chunk(key(cy)).or_else(|| {
                *generated += 1;
                self.generate(generator, registry, ChunkPos::new(self.x, cy, self.z))
                    .map(Arc::new)
            });
            self.captured.push((cy, data));
        }
        self.captured
            .sort_unstable_by_key(|(cy, _)| std::cmp::Reverse(*cy));
        if !self.content_below {
            self.content_below = std::mem::take(&mut self.saved_below).into_iter().any(|cy| {
                store
                    .read_chunk(key(cy))
                    .is_some_and(|d| may_block(&d, tables))
            });
        }
    }

    /// Layer `cy` of the column; `generate` makes a chunk that is not loaded. Under the
    /// generator's range only content counts: a loaded chunk without a blocker reads like an
    /// unloaded one, so what is loaded never moves a top (all-air chunks there are loaded or
    /// not depending on the viewers).
    fn layer(
        &self,
        cy: i64,
        tables: &LightTables,
        generate: impl FnOnce(ChunkPos) -> Option<ChunkData>,
    ) -> LayerData<'_> {
        // First captured layer at or below `cy`, and the first one below it.
        let i = self.captured.partition_point(|(c, _)| *c > cy);
        let here = self.captured.get(i).filter(|(c, _)| *c == cy);
        let lower = &self.captured[i + usize::from(here.is_some())..];

        if self.layers.contains(&cy) {
            return match here {
                Some((_, data)) => read(data.as_deref()),
                None => generate(ChunkPos::new(self.x, cy, self.z))
                    .map_or(LayerData::Air, LayerData::Owned),
            };
        }
        // Above the generator's range: air, except loaded (edited) chunks.
        if !self.layers.is_empty() && cy > *self.layers.end() {
            if let Some((_, data)) = here {
                return read(data.as_deref());
            }
            let end = *self.layers.end();
            return LayerData::AirUntil(lower.first().map_or(end, |(c, _)| (*c).max(end)));
        }
        if let Some((_, Some(data))) = here
            && may_block(data, tables)
        {
            return LayerData::Ref(data);
        }
        let content = lower
            .iter()
            .find(|(_, d)| d.as_deref().is_some_and(|d| may_block(d, tables)));
        match content {
            Some((c, _)) => LayerData::AirUntil(*c),
            None if self.content_below => {
                LayerData::AirUntil((self.floor >> CHUNK_SHIFT).saturating_sub(1))
            }
            None => LayerData::Void,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boxel_light::sky_tile::DEFAULT_WALK_LIMIT;
    use boxel_voxel::{BlockRegistry, BlockState, LocalPos};
    use boxel_worldgen::{FlatGenerator, ForestGenerator, OverworldGenerator};

    #[test]
    fn rules_follow_the_generator_range() {
        let limit = DEFAULT_WALK_LIMIT;
        assert_eq!(SkyRule::new(false, 0..=3, limit), SkyRule::Off);
        assert_eq!(
            SkyRule::new(true, i64::MIN..=i64::MAX, limit),
            SkyRule::Open
        );
        let SkyRule::Walk(empty) = SkyRule::new(true, RangeInclusive::new(0, -1), limit) else {
            panic!("an empty generator is walked (edits only)")
        };
        assert_eq!(
            (empty.top, empty.floor, empty.void_reachable),
            (None, i64::MIN, true)
        );
        assert_eq!(empty.start(i64::MIN), None, "no content: open forever");
        assert_eq!(empty.start(40), Some(40));
        // The overworld goes down forever: no column reaches the void.
        let overworld = OverworldGenerator::default();
        let SkyRule::Walk(o) = SkyRule::new(true, overworld.vertical_chunks(), limit) else {
            panic!("the overworld is walked")
        };
        assert!(!o.void_reachable);
        assert_eq!(o.floor, o.top.unwrap() - i64::from(limit));
        // A flat world of 4 layers has its bottom above the floor.
        let SkyRule::Walk(f) = SkyRule::new(true, 0..=0, limit) else {
            panic!("walked")
        };
        assert!(f.void_reachable);
        assert_eq!(f.start(i64::MIN), Some(31));
        assert_eq!(f.start(5000), Some(5000));
        // A bottom far under the floor does not reach the void within the limit.
        let SkyRule::Walk(deep) = SkyRule::new(true, -100..=0, limit) else {
            panic!("walked")
        };
        assert!(!deep.void_reachable);
        // The first layer under the range (-32: voxels -1024..=-993) is read when its top is at
        // or above the floor.
        let reaches = |limit| match SkyRule::new(true, -31..=0, limit) {
            SkyRule::Walk(rule) => (rule.floor, rule.void_reachable),
            other => panic!("{other:?}"),
        };
        assert_eq!(reaches(1023), (-992, false));
        assert_eq!(reaches(1024), (-993, true));
    }

    #[test]
    fn padded_chunks_are_the_chunks_whose_padded_region_meets_the_range() {
        let brute = |lo: i64, hi: i64| -> Vec<i64> {
            (-4..=4)
                .filter(|c| c * 32 - 1 <= hi && c * 32 + 32 >= lo)
                .collect()
        };
        for lo in -40..40 {
            for hi in lo..lo + 40 {
                let got: Vec<i64> = padded_chunks(lo, hi).collect();
                assert_eq!(got, brute(lo, hi), "{lo}..={hi}");
            }
        }
    }

    /// A walk with captured layers equals a walk of generated layers, and generates only the
    /// layers that are not captured.
    #[test]
    fn walk_jobs_read_captured_layers_and_generate_the_rest() {
        let g = ForestGenerator::default();
        let registry = BlockRegistry::default_blocks();
        let tables = LightTables::from_registry(&registry);
        let SkyRule::Walk(rule) = SkyRule::new(true, g.vertical_chunks(), DEFAULT_WALK_LIMIT)
        else {
            panic!("walked")
        };
        let gen_at = |cy| g.generate(ChunkKey::new(ChunkPos::new(3, cy, -2)), &registry);
        let job = |captured: Vec<(i64, Option<Arc<ChunkData>>)>| WalkJob {
            x: 3,
            z: -2,
            dim: DimensionId(0),
            start: rule.top.unwrap(),
            floor: rule.floor,
            layers: rule.layers.clone(),
            captured,
            content_below: false,
            saved: Vec::new(),
            saved_below: Vec::new(),
            store: None,
        };
        let (plain, generated) = job(Vec::new()).run(&g, &registry, &tables);
        assert!(generated > 0);
        let top = *rule.layers.end();
        let captured: Vec<_> = (top - 3..=top)
            .rev()
            .map(|cy| (cy, Some(Arc::new(gen_at(cy)))))
            .collect();
        let (with, fewer) = job(captured).run(&g, &registry, &tables);
        assert_eq!(with, plain);
        assert!(fewer < generated, "{fewer} < {generated}");
    }

    /// Under a bounded generator, layers are void unless a loaded chunk lower down holds a
    /// blocker; an edited chunk above the generator is read where it is.
    #[test]
    fn walk_jobs_find_the_void_and_edits_outside_the_generator() {
        let g = FlatGenerator::new(vec![("boxel:stone".to_string(), 4)]);
        let registry = BlockRegistry::default_blocks();
        let tables = LightTables::from_registry(&registry);
        let stone = registry.by_name("boxel:stone").unwrap();
        let SkyRule::Walk(rule) = SkyRule::new(true, g.vertical_chunks(), DEFAULT_WALK_LIMIT)
        else {
            panic!("walked")
        };
        // A hole through the flat layers at local (1, 1): the generated layer, edited.
        let mut holed = g.generate(ChunkKey::new(ChunkPos::new(0, 0, 0)), &registry);
        for y in 0..32 {
            holed.set(LocalPos::new(1, y, 1), BlockState::AIR);
        }
        let mut deep = ChunkData::uniform(BlockState::AIR);
        deep.set(LocalPos::new(1, 4, 1), stone);
        let job = |captured, content_below| WalkJob {
            x: 0,
            z: 0,
            dim: DimensionId(0),
            start: rule.top.unwrap(),
            floor: rule.floor,
            layers: rule.layers.clone(),
            captured,
            content_below,
            saved: Vec::new(),
            saved_below: Vec::new(),
            store: None,
        };
        let holed = Arc::new(holed);
        let (open, _) = job(vec![(0, Some(Arc::clone(&holed)))], false).run(&g, &registry, &tables);
        assert_eq!(open.top(1, 1), i64::MIN, "open through the void");
        assert_ne!(open.top(0, 0), i64::MIN);
        // A loaded stone 3 layers under the generator closes the hole there.
        let (closed, _) = job(
            vec![(0, Some(Arc::clone(&holed))), (-3, Some(Arc::new(deep)))],
            false,
        )
        .run(&g, &registry, &tables);
        assert_eq!(closed.top(1, 1), -3 * 32 + 4);
        // Content under the walk's range: the column ends at the floor, not in the void.
        let (floored, _) = job(vec![(0, Some(holed))], true).run(&g, &registry, &tables);
        assert_eq!(floored.top(1, 1), rule.floor);
    }
}
