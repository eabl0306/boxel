//! Sky tops: per chunk column, the highest voxel of each of its 32x32 columns that stops direct
//! sky, found by walking the column down through the world content.
//!
//! The top `T(x, z)` is defined over the world content (generator plus edits), never over what
//! happens to be loaded, so it does not change while chunks stream. A voxel with `y > T` is
//! direct sky (level 15); `T = i64::MIN` means the column is open forever.
//!
//! [`walk`] computes a tile: from its start `S`, the highest voxel any content of the chunk
//! column may have (the generator's top voxel or the column's highest edited blocker, whichever
//! is higher), layer by layer downwards, each layer read from a loaded snapshot, generated
//! privately, or known to be air. It stops when every column has met a blocker. Two cut-offs
//! keep it bounded and deterministic:
//!
//! - **Floor.** A column still open at the floor gets `T = floor` ([`SkyTile::floor`]), as if
//!   a blocker sat there. The floor belongs to the world, not to the tile: the generator's top
//!   voxel minus the walk limit ([`walk_floor`]), and edits never move it. A blocker placed far
//!   above the generator only raises the start of its tile, so it changes its own column and no
//!   other, and an edit applied to a tile ([`SkyTile::raise`], [`SkyTile::rescan_column`]) gives
//!   the tops a fresh walk of the edited content gives. Only natural shafts deeper than the
//!   limit notice (their bottom gets no direct sky).
//! - **Void.** A layer the caller marks [`LayerData::Void`] is air, and so is everything below
//!   it (below a generator's vertical range, with nothing loaded there): a column still open
//!   there gets `T = i64::MIN`. Layers below the world limit (`-MAX_CHUNK_COORD`) are void too,
//!   so a walk without a floor (`i64::MIN`) still ends.
//!
//! **Tints** (colored sky, ADR 0064): a tile walked with [`walk_colored`] also keeps, per column,
//! the light filters above `T` (opacity 0 by the definition of `T`: colored glass), highest
//! first ([`SkyTile::tints`]). Direct sky under them is filtered ([`crate::box_light`]). The
//! tile of [`walk`] keeps none and is the tile of before; so is every tile of a world without
//! glass above its tops (`tints` stays `None`). Edits keep them like the tops
//! ([`SkyTile::raise`], [`SkyTile::rescan_column`], [`SkyTile::set_filter`]): an edited tile
//! equals a fresh walk of the edited content.
//!
//! Every top of a walked tile is therefore either `i64::MIN` or at least the floor. A light box
//! whose top is at or below every top of its 3x3 tiles has no direct sky; when no column of a
//! world can reach the void within the limit (the overworld, or bounded generators whose
//! columns are closed), that holds for every box below the floor.
use boxel_voxel::{
    CHUNK_AREA, CHUNK_SHIFT, ChunkData, LightTables, LocalPos, MAX_CHUNK_COORD, MAX_LIGHT,
};
use std::ops::RangeInclusive;
use std::sync::Arc;

/// Columns per tile (32x32).
pub const TILE_AREA: usize = CHUNK_AREA;

/// The default walk limit, in voxels below the generator's top voxel.
pub const DEFAULT_WALK_LIMIT: u32 = 1024;

/// What one chunk layer of the walked column holds.
#[derive(Debug)]
pub enum LayerData<'a> {
    /// All air (a missing chunk, or a layer outside the generator's range).
    Air,
    /// All air, and so is every layer from here down to layer `n` (exclusive): the walk reads
    /// layer `n` next. Skips long gaps of air (an edit far above the generator, below a
    /// bounded generator) in one step. A value at or above the current layer reads as
    /// [`LayerData::Air`].
    AirUntil(i64),
    /// All air, and so is every layer below it.
    Void,
    /// A borrowed chunk.
    Ref(&'a ChunkData),
    /// A loaded snapshot.
    Shared(Arc<ChunkData>),
    /// A chunk generated just for the walk.
    Owned(ChunkData),
}

impl LayerData<'_> {
    /// The chunk, if the layer has blocks.
    pub(crate) fn chunk(&self) -> Option<&ChunkData> {
        match self {
            LayerData::Air | LayerData::AirUntil(_) | LayerData::Void => None,
            LayerData::Ref(c) => Some(c),
            LayerData::Shared(c) => Some(c),
            LayerData::Owned(c) => Some(c),
        }
    }
}

/// A light filter above the sky top of its column (ADR 0064): its height and its filter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tint {
    /// Absolute y.
    pub y: i64,
    /// [`LightTables::light_filter`] of its state.
    pub filter: [u8; 3],
}

/// The tints of a tile with at least one.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TileTints {
    /// Start of each column's tints in `entries`, indexed `(x << 5) | z`, then the end.
    index: Box<[u32]>,
    /// Every tint, by column, each column highest first.
    entries: Vec<Tint>,
    /// Highest tint.
    max: i64,
}

impl TileTints {
    /// From `(column, tint)` pairs in any column order, each column's tints highest first;
    /// `None` if there are none.
    fn from_found(found: Vec<(u16, Tint)>) -> Option<Box<Self>> {
        if found.is_empty() {
            return None;
        }
        let mut index = vec![0u32; TILE_AREA + 1];
        for &(col, _) in &found {
            index[col as usize + 1] += 1;
        }
        for i in 1..=TILE_AREA {
            index[i] += index[i - 1];
        }
        // A counting sort by column, stable: each column keeps its order (highest first).
        let mut next = index.clone();
        let mut entries = vec![
            Tint {
                y: 0,
                filter: [0; 3]
            };
            found.len()
        ];
        for (col, t) in found {
            entries[next[col as usize] as usize] = t;
            next[col as usize] += 1;
        }
        let max = entries.iter().map(|t| t.y).max().unwrap_or(i64::MIN);
        Some(Box::new(Self {
            index: index.into_boxed_slice(),
            entries,
            max,
        }))
    }

    fn column(&self, col: usize) -> &[Tint] {
        &self.entries[self.index[col] as usize..self.index[col + 1] as usize]
    }
}

/// The sky tops of one chunk column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkyTile {
    /// `T` per column, indexed `(x << 5) | z` (local x, z), like one y layer of a chunk.
    top: Box<[i64; TILE_AREA]>,
    /// Lowest top (`i64::MIN` if a column is open forever).
    min: i64,
    /// Highest top.
    max: i64,
    /// Top given to columns still open at the walk floor (the world's, [`walk_floor`]).
    floor: i64,
    /// Bumped by every change of a top (for callers that cache results built from the tile).
    pub version: u32,
    /// True once an edit changed a top (the tile no longer follows the generator alone).
    pub edited: bool,
    /// True if the tile keeps its tints (walked with [`walk_colored`], or
    /// [`SkyTile::with_colored_sky`]).
    colored: bool,
    /// The tints, `None` when the tile has none (always, when not `colored`).
    tints: Option<Box<TileTints>>,
}

/// Outcome of [`SkyTile::rescan_column`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rescan {
    /// The column has this new top.
    Found(i64),
    /// The scan needed a layer the caller does not have: the tile must be walked again.
    Unresolved,
}

impl SkyTile {
    /// A tile from explicit tops (indexed `(x << 5) | z`) and floor.
    pub fn from_tops(top: Box<[i64; TILE_AREA]>, floor: i64) -> Self {
        let mut tile = Self {
            top,
            min: 0,
            max: 0,
            floor,
            version: 0,
            edited: false,
            colored: false,
            tints: None,
        };
        tile.update_bounds();
        tile
    }
    /// The same tile, keeping tints from now on (ADR 0064): [`SkyTile::set_filter`] records
    /// them, and [`SkyTile::raise`] and [`SkyTile::rescan_column`] keep them.
    pub fn with_colored_sky(mut self) -> Self {
        self.colored = true;
        self
    }
    /// The same tile keeping tints ([`SkyTile::with_colored_sky`]), with `tints` (local column
    /// `(x, z)` and tint, any order) as its tints: those at or below their column's top, or
    /// with filter `[15; 3]`, are dropped. For tiles built by hand (tests, references).
    pub fn with_tints(mut self, tints: impl IntoIterator<Item = (u32, u32, Tint)>) -> Self {
        let mut found: Vec<(u16, Tint)> = tints
            .into_iter()
            .map(|(x, z, t)| ((((x as usize) << CHUNK_SHIFT) | z as usize) as u16, t))
            .filter(|&(col, t)| t.y > self.top[col as usize] && t.filter != [MAX_LIGHT; 3])
            .collect();
        found.sort_by_key(|&(col, t)| (col, std::cmp::Reverse(t.y)));
        found.dedup_by_key(|&mut (col, t)| (col, t.y));
        self.colored = true;
        self.tints = TileTints::from_found(found);
        self
    }
    /// True if the tile keeps its tints.
    pub fn colored_sky(&self) -> bool {
        self.colored
    }
    /// The tints of local column `(x, z)` (each in `0..32`), highest first: the light filters
    /// above its top. Empty for a tile that does not keep them.
    #[inline]
    pub fn tints(&self, x: u32, z: u32) -> &[Tint] {
        self.tints_at(((x as usize) << CHUNK_SHIFT) | z as usize)
    }
    /// [`SkyTile::tints`] of column `(x << 5) | z`.
    #[inline]
    pub(crate) fn tints_at(&self, col: usize) -> &[Tint] {
        self.tints.as_ref().map_or(&[], |t| t.column(col))
    }
    /// The highest tint of the tile, `i64::MIN` without one.
    #[inline]
    pub fn tint_max(&self) -> i64 {
        self.tints.as_ref().map_or(i64::MIN, |t| t.max)
    }
    /// The number of tints.
    pub fn tint_count(&self) -> usize {
        self.tints.as_ref().map_or(0, |t| t.entries.len())
    }
    /// Heap bytes of the tile: the tops, plus the tints' index and entries when it has some.
    pub fn heap_bytes(&self) -> usize {
        TILE_AREA * size_of::<i64>()
            + self.tints.as_ref().map_or(0, |t| {
                size_of::<TileTints>()
                    + t.index.len() * size_of::<u32>()
                    + t.entries.capacity() * size_of::<Tint>()
            })
    }
    /// The voxel at `y` of column `(x, z)`, above its top, became a light filter `filter`
    /// (`None`, or `[15; 3]`: no longer one). For a state of opacity 0: a blocker moves the
    /// top ([`SkyTile::raise`]). Returns true if the tints changed; always false for a tile
    /// that does not keep tints, or at or below the top.
    pub fn set_filter(&mut self, x: u32, z: u32, y: i64, filter: Option<[u8; 3]>) -> bool {
        let col = ((x as usize) << CHUNK_SHIFT) | z as usize;
        if !self.colored || y <= self.top[col] {
            return false;
        }
        let filter = filter.filter(|f| *f != [MAX_LIGHT; 3]);
        let mut column = self.tints_at(col).to_vec();
        // Highest first: the first entry at or below `y`.
        let at = column.partition_point(|t| t.y > y);
        let here = column.get(at).is_some_and(|t| t.y == y);
        match (here, filter) {
            (true, Some(f)) if column[at].filter == f => return false,
            (true, Some(f)) => column[at].filter = f,
            (true, None) => {
                column.remove(at);
            }
            (false, Some(f)) => column.insert(at, Tint { y, filter: f }),
            (false, None) => return false,
        }
        self.set_tints(col, column);
        self.version = self.version.wrapping_add(1);
        self.edited = true;
        true
    }
    /// Replaces the tints of column `col`.
    fn set_tints(&mut self, col: usize, column: Vec<Tint>) {
        let mut found: Vec<(u16, Tint)> = Vec::new();
        for c in 0..TILE_AREA {
            if c == col {
                found.extend(column.iter().map(|&t| (c as u16, t)));
            } else {
                found.extend(self.tints_at(c).iter().map(|&t| (c as u16, t)));
            }
        }
        self.tints = TileTints::from_found(found);
    }
    /// Every column with top `t`.
    pub fn uniform(t: i64, floor: i64) -> Self {
        Self::from_tops(new_tops(t), floor)
    }
    /// Top of local column `(x, z)`, each in `0..32`.
    #[inline]
    pub fn top(&self, x: u32, z: u32) -> i64 {
        self.top[((x as usize) << CHUNK_SHIFT) | z as usize]
    }
    /// Every top, indexed `(x << 5) | z`.
    pub fn tops(&self) -> &[i64; TILE_AREA] {
        &self.top
    }
    /// Lowest top.
    pub fn min(&self) -> i64 {
        self.min
    }
    /// Highest top.
    pub fn max(&self) -> i64 {
        self.max
    }
    /// The top of columns still open at the walk floor ([`walk_floor`]).
    pub fn floor(&self) -> i64 {
        self.floor
    }
    /// A blocker placed at `y` in column `(x, z)`: the top rises to `y` if it was below.
    /// Returns true if it changed.
    pub fn raise(&mut self, x: u32, z: u32, y: i64) -> bool {
        let i = ((x as usize) << CHUNK_SHIFT) | z as usize;
        if y <= self.top[i] {
            return false;
        }
        self.set(i, y);
        // Tints at or below the new top are no longer above it.
        let tints = self.tints_at(i);
        if tints.last().is_some_and(|t| t.y <= y) {
            let keep = tints.iter().copied().filter(|t| t.y > y).collect();
            self.set_tints(i, keep);
        }
        true
    }
    /// The blocker at the top of column `(x, z)` was removed (or made clear): finds the next
    /// one at `from` or below, reading layer `cy` with `layer(cy)` (`None` when the caller does
    /// not have that layer, typically when it is not loaded). Stops at the floor (top = floor)
    /// and at [`LayerData::Void`] or below the world limit (top = `i64::MIN`). On `Unresolved`
    /// the tile is unchanged.
    ///
    /// A tile without a floor (`i64::MIN`, as built by hand) relies on `layer` to answer
    /// [`LayerData::Void`] or `None` below the content.
    ///
    /// A tile that keeps tints also collects the filters between the new top and the old one,
    /// the old top's voxel included (it may have become glass), so it reads from the old top's
    /// layer down.
    pub fn rescan_column<'a>(
        &mut self,
        x: u32,
        z: u32,
        from: i64,
        tables: &LightTables,
        mut layer: impl FnMut(i64) -> Option<LayerData<'a>>,
    ) -> Rescan {
        if self.colored {
            return self.rescan_colored(x, z, from, tables, layer);
        }
        let i = ((x as usize) << CHUNK_SHIFT) | z as usize;
        let mut cy = from >> CHUNK_SHIFT;
        let found = loop {
            let y0 = cy << CHUNK_SHIFT;
            if y0 + 31 < self.floor || from < self.floor {
                break self.floor;
            }
            if cy < -MAX_CHUNK_COORD {
                break i64::MIN;
            }
            let Some(data) = layer(cy) else {
                return Rescan::Unresolved;
            };
            if matches!(data, LayerData::Void) {
                break i64::MIN;
            }
            if let Some(chunk) = data.chunk() {
                let hi = from.saturating_sub(y0).min(31);
                let lo = self.floor.saturating_sub(y0).max(0);
                let hit = (lo..=hi)
                    .rev()
                    .find(|&ly| tables.blocks_sky(chunk.get(LocalPos::new(x, ly as u32, z))));
                if let Some(ly) = hit {
                    break y0 + ly;
                }
            }
            cy = next_layer(&data, cy);
        };
        self.set(i, found);
        Rescan::Found(found)
    }
    /// [`SkyTile::rescan_column`] of a tile that keeps tints: one voxel at a time from the old
    /// top down, blockers counted at `from` and below, filters above the floor and at or below
    /// the old top collected.
    fn rescan_colored<'a>(
        &mut self,
        x: u32,
        z: u32,
        from: i64,
        tables: &LightTables,
        mut layer: impl FnMut(i64) -> Option<LayerData<'a>>,
    ) -> Rescan {
        let i = ((x as usize) << CHUNK_SHIFT) | z as usize;
        let old = self.top[i];
        let start = from.max(old);
        let mut found_tints: Vec<Tint> = Vec::new();
        let mut cy = start >> CHUNK_SHIFT;
        let found = loop {
            let y0 = cy << CHUNK_SHIFT;
            if y0 + 31 < self.floor {
                break self.floor;
            }
            if cy < -MAX_CHUNK_COORD {
                break i64::MIN;
            }
            let Some(data) = layer(cy) else {
                return Rescan::Unresolved;
            };
            if matches!(data, LayerData::Void) {
                break i64::MIN;
            }
            if let Some(chunk) = data.chunk() {
                let hi = start.saturating_sub(y0).min(31);
                let lo = self.floor.saturating_sub(y0).max(0);
                let mut hit = None;
                for ly in (lo..=hi).rev() {
                    let y = y0 + ly;
                    let s = chunk.get(LocalPos::new(x, ly as u32, z));
                    if tables.blocks_sky(s) {
                        if y <= from {
                            hit = Some(y);
                            break;
                        }
                    } else if y <= old && y > self.floor && tables.filters(s) {
                        found_tints.push(Tint {
                            y,
                            filter: tables.light_filter(s),
                        });
                    }
                }
                if let Some(y) = hit {
                    break y;
                }
            }
            cy = next_layer(&data, cy);
        };
        // Below the floor nothing counts; a top at the floor keeps only the tints above it.
        found_tints.retain(|t| t.y > found);
        self.set(i, found);
        if !found_tints.is_empty() {
            let mut column = self.tints_at(i).to_vec();
            column.extend(found_tints);
            self.set_tints(i, column);
        }
        Rescan::Found(found)
    }
    /// Sets one top, keeping the bounds, the version and the edited flag.
    fn set(&mut self, i: usize, t: i64) {
        let old = self.top[i];
        self.top[i] = t;
        self.version = self.version.wrapping_add(1);
        self.edited = true;
        if t < self.min {
            self.min = t;
        } else if t > self.max {
            self.max = t;
        }
        if old == self.min || old == self.max {
            self.update_bounds();
        }
    }
    fn update_bounds(&mut self) {
        let (min, max) = self
            .top
            .iter()
            .fold((i64::MAX, i64::MIN), |(lo, hi), &t| (lo.min(t), hi.max(t)));
        (self.min, self.max) = (min, max);
    }
}

fn new_tops(t: i64) -> Box<[i64; TILE_AREA]> {
    vec![t; TILE_AREA]
        .into_boxed_slice()
        .try_into()
        .expect("TILE_AREA entries")
}

/// Where the content of a generator ends upwards, from its chunk-y range
/// (`WorldGenerator::vertical_chunks`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeneratorTop {
    /// The range is empty or lies below the world: the generator places nothing, so without
    /// edits every column is open forever.
    Empty,
    /// The top voxel of the range: the start of a walk without edits, and what the world's
    /// floor is measured from ([`walk_floor`]).
    Voxel(i64),
    /// The range ends above the world limit (`WorldGenerator`'s default, `i64::MIN..=i64::MAX`,
    /// promises nothing): there is no start to walk from. Light needs a generator that declares
    /// its top; the world must refuse such a generator or run without sky tops, never walk
    /// from the world limit (every column would end at the floor, about `2^61`, and the whole
    /// world would be dark).
    Unbounded,
}

impl GeneratorTop {
    /// The top of the chunk-y range `layers`.
    pub fn of(layers: &RangeInclusive<i64>) -> Self {
        if layers.is_empty() || *layers.end() < -MAX_CHUNK_COORD {
            GeneratorTop::Empty
        } else if *layers.end() > MAX_CHUNK_COORD {
            GeneratorTop::Unbounded
        } else {
            GeneratorTop::Voxel((*layers.end() << CHUNK_SHIFT) + 31)
        }
    }
    /// The top voxel, if the range is bounded and not empty.
    pub fn voxel(self) -> Option<i64> {
        match self {
            GeneratorTop::Voxel(y) => Some(y),
            GeneratorTop::Empty | GeneratorTop::Unbounded => None,
        }
    }
}

/// The walk floor of a world whose generator's top voxel is `generator_top`: `limit` voxels
/// below it. The same for every tile of the world; edits never move it.
pub fn walk_floor(generator_top: i64, limit: u32) -> i64 {
    generator_top.saturating_sub(i64::from(limit))
}

/// Walks one chunk column down from voxel `start` (`S`) and returns its sky tops, with columns
/// still open at `floor` ([`walk_floor`]) set to the floor.
///
/// `layer(cy)` gives chunk layer `cy`, from `start >> 5` downwards, until every column has met
/// a voxel with opacity above 0 (`tables`), the walk passes the floor, or a layer is
/// [`LayerData::Void`]. Voxels above `start` are not read (by contract they are air), so a
/// start below the floor gives every column the floor. See the [module docs](self) for the
/// rules. The tile keeps no tints.
pub fn walk<'a>(
    start: i64,
    floor: i64,
    tables: &LightTables,
    layer: impl FnMut(i64) -> LayerData<'a>,
) -> SkyTile {
    walk_with::<false>(start, floor, tables, layer)
}

/// [`walk`] for colored sky (ADR 0064): the tile also keeps, per column, the light filters it
/// passes while the column is still open (above the column's top and the floor), and keeps
/// them through edits. The tops are those of [`walk`]; it reads the same layers.
pub fn walk_colored<'a>(
    start: i64,
    floor: i64,
    tables: &LightTables,
    layer: impl FnMut(i64) -> LayerData<'a>,
) -> SkyTile {
    walk_with::<true>(start, floor, tables, layer)
}

fn walk_with<'a, const COLORED: bool>(
    start: i64,
    floor: i64,
    tables: &LightTables,
    mut layer: impl FnMut(i64) -> LayerData<'a>,
) -> SkyTile {
    let colored = |mut tile: SkyTile| {
        tile.colored = COLORED;
        tile
    };
    if start < floor {
        return colored(SkyTile::uniform(floor, floor));
    }
    let mut top = new_tops(floor);
    // Open columns: bit z of word x.
    let mut open = [u32::MAX; 32];
    let mut remaining = TILE_AREA as u32;
    let mut cy = start >> CHUNK_SHIFT;
    // Tints found, by column, each column highest first.
    let mut tints: Vec<(u16, Tint)> = Vec::new();
    let filters = COLORED && tables.any_filter();
    'layers: while remaining > 0 {
        let y0 = cy << CHUNK_SHIFT;
        if y0 + 31 < floor {
            break;
        }
        let data = if cy < -MAX_CHUNK_COORD {
            LayerData::Void
        } else {
            layer(cy)
        };
        if matches!(data, LayerData::Void) {
            for (x, bits) in open.iter().enumerate() {
                for_each_bit(*bits, |z| top[(x << CHUNK_SHIFT) | z] = i64::MIN);
            }
            break;
        }
        let Some(chunk) = data.chunk() else {
            cy = next_layer(&data, cy);
            continue;
        };
        // Local y range to read: at most S, at least the floor.
        let hi = start.saturating_sub(y0).min(31) as usize;
        let lo = floor.saturating_sub(y0).max(0) as usize;
        if let Some(s) = chunk.as_uniform() {
            if tables.blocks_sky(s) {
                let y = y0 + hi as i64;
                for (x, bits) in open.iter().enumerate() {
                    for_each_bit(*bits, |z| top[(x << CHUNK_SHIFT) | z] = y);
                }
                break;
            }
            if filters && tables.filters(s) {
                // A layer of glass: every voxel of every open column, above the floor.
                let filter = tables.light_filter(s);
                for ly in (lo..=hi).rev().filter(|&ly| y0 + ly as i64 > floor) {
                    for (x, bits) in open.iter().enumerate() {
                        for_each_bit(*bits, |z| {
                            let col = ((x << CHUNK_SHIFT) | z) as u16;
                            let y = y0 + ly as i64;
                            tints.push((col, Tint { y, filter }));
                        });
                    }
                }
            }
            cy -= 1;
            continue;
        }
        let masks = chunk.light_masks(tables);
        let glass = filters && !masks.filter.is_empty();
        for ly in (lo..=hi).rev() {
            let y = y0 + ly as i64;
            let rows = ly << CHUNK_SHIFT;
            for (x, open_x) in open.iter_mut().enumerate() {
                let blockers = masks.opaque.row(rows + x) | masks.partial.row(rows + x);
                if glass && y > floor {
                    // Filters of opacity 0 in open columns: above the column's top.
                    let found = masks.filter.row(rows + x) & !blockers & *open_x;
                    for_each_bit(found, |z| {
                        let s = chunk.get(LocalPos::new(x as u32, ly as u32, z as u32));
                        let col = ((x << CHUNK_SHIFT) | z) as u16;
                        let filter = tables.light_filter(s);
                        tints.push((col, Tint { y, filter }));
                    });
                }
                let hit = blockers & *open_x;
                if hit != 0 {
                    for_each_bit(hit, |z| top[(x << CHUNK_SHIFT) | z] = y);
                    *open_x &= !hit;
                    remaining -= hit.count_ones();
                }
            }
            if remaining == 0 {
                break 'layers;
            }
        }
        cy -= 1;
    }
    let mut tile = colored(SkyTile::from_tops(top, floor));
    tile.tints = TileTints::from_found(tints);
    tile
}

/// The layer a walk or a rescan reads after layer `cy` held `data` (never below the first
/// layer under the world limit, which reads as [`LayerData::Void`]).
fn next_layer(data: &LayerData<'_>, cy: i64) -> i64 {
    match *data {
        LayerData::AirUntil(n) if n < cy => n.max(-MAX_CHUNK_COORD - 1),
        _ => cy - 1,
    }
}

#[inline]
fn for_each_bit(mut bits: u32, mut f: impl FnMut(usize)) {
    while bits != 0 {
        f(bits.trailing_zeros() as usize);
        bits &= bits - 1;
    }
}
