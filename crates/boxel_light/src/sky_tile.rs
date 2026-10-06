//! Sky tops: per chunk column, the highest voxel of each of its 32x32 columns that stops direct
//! sky, found by walking the column down through the world content.
//!
//! The top `T(x, z)` is defined over the world content (generator plus edits), never over what
//! happens to be loaded, so it does not change while chunks stream. A voxel with `y > T` is
//! direct sky (level 15); `T = i64::MIN` means the column is open forever.
//!
//! [`walk`] computes a tile: from `S`, the highest voxel any content may have (the generator's
//! top voxel or the highest edited blocker, whichever is higher), layer by layer downwards, each
//! layer read from a loaded snapshot, generated privately, or known to be air. It stops when every
//! column has met a blocker. Two cut-offs keep it bounded and deterministic:
//!
//! - **Limit.** A column still open `limit` voxels below `S` gets `T = S - limit` (the tile's
//!   [`SkyTile::floor`]), as if a blocker sat there. Only natural shafts deeper than the limit
//!   notice (their bottom gets no direct sky).
//! - **Void.** A layer the caller marks [`LayerData::Void`] is air, and so is everything below
//!   it (below a generator's vertical range, with nothing loaded there): a column still open
//!   there gets `T = i64::MIN`.
//!
//! Every top of a walked tile is therefore either `i64::MIN` or at least the floor. A light box
//! whose top is at or below every top of its 3x3 tiles has no direct sky; when no column of a
//! world can reach the void within the limit (the overworld, or bounded generators whose
//! columns are closed), that holds for every box below `S - limit`.
use boxel_voxel::{CHUNK_AREA, CHUNK_SHIFT, ChunkData, LightTables, LocalPos, MAX_CHUNK_COORD};
use std::ops::RangeInclusive;
use std::sync::Arc;

/// Columns per tile (32x32).
pub const TILE_AREA: usize = CHUNK_AREA;

/// The default walk limit, in voxels below the top content.
pub const DEFAULT_WALK_LIMIT: u32 = 1024;

/// What one chunk layer of the walked column holds.
#[derive(Debug)]
pub enum LayerData<'a> {
    /// All air (a missing chunk, or a layer outside the generator's range).
    Air,
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
    fn chunk(&self) -> Option<&ChunkData> {
        match self {
            LayerData::Air | LayerData::Void => None,
            LayerData::Ref(c) => Some(c),
            LayerData::Shared(c) => Some(c),
            LayerData::Owned(c) => Some(c),
        }
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
    /// Top given to columns still open at the walk limit.
    floor: i64,
    /// Bumped by every change of a top (for callers that cache results built from the tile).
    pub version: u32,
    /// True once an edit changed a top (the tile no longer follows the generator alone).
    pub edited: bool,
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
        };
        tile.update_bounds();
        tile
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
    /// The top of columns still open at the walk limit (`S - limit`).
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
        true
    }
    /// The blocker at the top of column `(x, z)` was removed (or made clear): finds the next
    /// one at `from` or below, reading layer `cy` with `layer(cy)` (`None` when the caller does
    /// not have that layer, typically when it is not loaded). Stops at the floor (top = floor)
    /// and at [`LayerData::Void`] (top = `i64::MIN`). On `Unresolved` the tile is unchanged.
    pub fn rescan_column<'a>(
        &mut self,
        x: u32,
        z: u32,
        from: i64,
        tables: &LightTables,
        mut layer: impl FnMut(i64) -> Option<LayerData<'a>>,
    ) -> Rescan {
        let i = ((x as usize) << CHUNK_SHIFT) | z as usize;
        let mut cy = from >> CHUNK_SHIFT;
        let found = loop {
            let y0 = cy << CHUNK_SHIFT;
            if y0 + 31 < self.floor || from < self.floor {
                break self.floor;
            }
            let Some(data) = layer(cy) else {
                return Rescan::Unresolved;
            };
            if matches!(data, LayerData::Void) {
                break i64::MIN;
            }
            if let Some(chunk) = data.chunk() {
                let hi = (from - y0).min(31);
                let lo = (self.floor - y0).max(0);
                let hit = (lo..=hi)
                    .rev()
                    .find(|&ly| tables.blocks_sky(chunk.get(LocalPos::new(x, ly as u32, z))));
                if let Some(ly) = hit {
                    break y0 + ly;
                }
            }
            cy -= 1;
        };
        self.set(i, found);
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

/// Top voxel of a generator whose content lies in the chunk layers `layers`
/// (`WorldGenerator::vertical_chunks`): the walk's start without edits. `None` for an empty
/// range (nothing to walk: every top is `i64::MIN`). A range that ends past the world limit
/// starts at the world limit.
pub fn generator_top_voxel(layers: &RangeInclusive<i64>) -> Option<i64> {
    if layers.is_empty() {
        return None;
    }
    let end = (*layers.end()).clamp(-MAX_CHUNK_COORD, MAX_CHUNK_COORD);
    Some((end << CHUNK_SHIFT) + 31)
}

/// Walks one chunk column down from voxel `top_voxel` (`S`) and returns its sky tops.
///
/// `layer(cy)` gives chunk layer `cy`, from `top_voxel >> 5` downwards, until every column has
/// met a voxel with opacity above 0 (`tables`), the walk passes `S - limit`, or a layer is
/// [`LayerData::Void`]. Voxels above `S` are not read (by contract they are air). See the
/// [module docs](self) for the rules.
pub fn walk<'a>(
    top_voxel: i64,
    limit: u32,
    tables: &LightTables,
    mut layer: impl FnMut(i64) -> LayerData<'a>,
) -> SkyTile {
    let floor = top_voxel.saturating_sub(i64::from(limit));
    let mut top = new_tops(floor);
    // Open columns: bit z of word x.
    let mut open = [u32::MAX; 32];
    let mut remaining = TILE_AREA as u32;
    let mut cy = top_voxel >> CHUNK_SHIFT;
    'layers: while remaining > 0 {
        let y0 = cy << CHUNK_SHIFT;
        if y0 + 31 < floor {
            break;
        }
        let data = layer(cy);
        if matches!(data, LayerData::Void) {
            for (x, bits) in open.iter().enumerate() {
                for_each_bit(*bits, |z| top[(x << CHUNK_SHIFT) | z] = i64::MIN);
            }
            break;
        }
        let Some(chunk) = data.chunk() else {
            cy -= 1;
            continue;
        };
        // Local y range to read: at most S, at least the floor.
        let hi = (top_voxel - y0).min(31) as usize;
        let lo = (floor - y0).max(0) as usize;
        if let Some(s) = chunk.as_uniform() {
            if tables.blocks_sky(s) {
                let y = y0 + hi as i64;
                for (x, bits) in open.iter().enumerate() {
                    for_each_bit(*bits, |z| top[(x << CHUNK_SHIFT) | z] = y);
                }
                break;
            }
            cy -= 1;
            continue;
        }
        let masks = chunk.masks(tables);
        for ly in (lo..=hi).rev() {
            let y = y0 + ly as i64;
            let rows = ly << CHUNK_SHIFT;
            for (x, open_x) in open.iter_mut().enumerate() {
                let hit = (masks.opaque[rows + x] | masks.partial[rows + x]) & *open_x;
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
    SkyTile::from_tops(top, floor)
}

#[inline]
fn for_each_bit(mut bits: u32, mut f: impl FnMut(usize)) {
    while bits != 0 {
        f(bits.trailing_zeros() as usize);
        bits &= bits - 1;
    }
}
