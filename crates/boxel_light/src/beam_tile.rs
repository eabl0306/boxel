//! Beam tiles (ADR 0061): per chunk column, the beams of the sky sources that can reach a window
//! of y, found by reading the column's content like the sky tops.
//!
//! A sky source is a cell that emits direct sky along a straight beam in its column
//! ([`SkySourceDef`]): from its own cell through the next ones in its direction while they let
//! direct sky through (opacity 0, the test of the sky top), at most `range` cells, and never past
//! the plane of its realm. Beam cells are direct sky, so a light box ORs them into its direct rows
//! ([`crate::LightInput::beams`]) and the sky flood spreads from them as from the open sky.
//!
//! A beam is not local: a source up to `MAX_BEAM - 1` cells above or below a box can light
//! it, and a blocker in a chunk the mesh job does not hold can end it. So, like `T`, beams are
//! computed from the world content (loaded snapshot, saved chunk, private generation or known
//! air), never from what happens to be loaded, and any load order gives the same light.
//!
//! [`build`] reads the column from `w_hi + MAX_BEAM - 1` down to `w_lo - MAX_BEAM + 1` layer by
//! layer, with the [`LayerData`] callback of [`crate::sky_tile::walk`]. It keeps every source that
//! can reach the window by its range ([`reaches`]) with its whole beam, unclipped by the window,
//! so an edit can retrace one beam in place ([`BeamTile::edit`]). Beams are traced 32 columns at
//! a time: one pass down for the down beams, one up for the up beams, each `AND`ing the blocker
//! rows (`opaque | partial`) of a layer's masks with the columns whose beam is still open.
//!
//! **Realm planes** are a plain ascending slice (`RealmStack::planes` of the world generator):
//! the plane `p` is the bottom cell of the realm above it, so a down beam stops at the largest
//! plane at or under its source, and an up beam under the smallest plane above it. The clip does
//! not need the planes to be chunk aligned.
//!
//! **Light filters** (ADR 0060) are ignored: a beam is direct sky, which the filters leave white.
//! A filter of opacity 0 (colored glass) lets the beam through, one of opacity 1 or more ends it
//! like any other blocker. Every absolute comparison is an `i64` difference, so a tile is the same
//! at any height up to the world limit (ADR 0014).
use crate::LayerData;
use boxel_voxel::{
    BeamDir, BlockState, CHUNK_AREA, CHUNK_SHIFT, LightMasks, LightTables, LocalPos, MAX_BEAM,
    MAX_CHUNK_COORD, SkySourceDef,
};
use std::ops::RangeInclusive;

/// How far a beam reaches past its source, in cells.
const REACH: i64 = MAX_BEAM as i64 - 1;

/// Tallest window of a tile, in voxels. Beams are kept as 16-bit offsets from
/// `w_lo - MAX_BEAM`, and a beam that reaches the window lies in
/// `w_lo - MAX_BEAM + 1 ..= w_hi + MAX_BEAM - 1`.
pub const MAX_WINDOW: i64 = (1 << 16) - 2 * MAX_BEAM as i64;

/// `index` entries of a tile with beams: one per column, then the end.
const INDEX_LEN: usize = CHUNK_AREA + 1;

/// Packed range bits of [`Packed::flags`]: `range - 1`.
const FLAG_RANGE: u16 = 0xFF;
/// The beam goes up.
const FLAG_UP: u16 = 1 << 8;

/// One beam of a source, as a tile stores it (8 B). Offsets count from the tile's base,
/// `w_lo - MAX_BEAM`. Bits 9..16 of `flags` are free (colored beams, constant sources).
///
/// The field order is the sort order of a column (by `lo` first, as [`crate::box_light`] reads
/// it), and makes the order unique.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Packed {
    /// Lowest cell of the beam.
    pub(crate) lo: u16,
    /// Highest cell.
    pub(crate) hi: u16,
    /// The source.
    y: u16,
    /// `range - 1` and [`FLAG_UP`].
    flags: u16,
}

/// A beam with absolute coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Beam {
    /// The source cell.
    pub source: i64,
    /// Lowest cell of the beam (the source for an up beam).
    pub lo: i64,
    /// Highest cell (the source for a down beam).
    pub hi: i64,
    /// What the source emits.
    pub def: SkySourceDef,
}

/// The beams of one chunk column that can reach its window.
#[derive(Clone, Debug)]
pub struct BeamTile {
    /// Lowest row of the window.
    w_lo: i64,
    /// Highest row.
    w_hi: i64,
    /// What [`Packed`] offsets count from: `w_lo - MAX_BEAM`.
    base: i64,
    /// Start of each column's beams in `beams`, indexed `(x << 5) | z`, then the end. Empty
    /// when the tile has no beam (no memory for the common empty tile).
    index: Box<[u32]>,
    /// Every beam, by column, each column sorted by [`Packed`]'s order.
    beams: Vec<Packed>,
    /// Lowest cell of every beam (above `hi` when there is none).
    lo: i64,
    /// Highest cell.
    hi: i64,
    /// Bumped by every change of the beams (for callers that cache results built from the tile).
    pub version: u32,
}

/// Two tiles are equal when they have the same window and the same beams; the version is not
/// compared.
impl PartialEq for BeamTile {
    fn eq(&self, other: &Self) -> bool {
        (self.w_lo, self.w_hi) == (other.w_lo, other.w_hi)
            && self.index == other.index
            && self.beams == other.beams
    }
}

impl Eq for BeamTile {}

/// Outcome of [`BeamTile::edit`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retrace {
    /// No beam cell changed (the source list may have: a source replaced by another with the
    /// same beam).
    Unchanged,
    /// The beam cells changed between `lo` and `hi` (every cell that became or stopped being a
    /// beam cell lies there). The light changes at most 14 cells beyond.
    Changed {
        /// Lowest changed cell.
        lo: i64,
        /// Highest changed cell.
        hi: i64,
    },
    /// Tracing needed a layer the caller does not have: the tile is unchanged and must be built
    /// again.
    Unresolved,
}

/// True if a source `def` at `y` can reach the window `window` by its range: a down source in
/// `w_lo ..= w_hi + range - 1`, an up source in `w_lo - range + 1 ..= w_hi`. A tile keeps exactly
/// these sources, whatever ends their beams.
pub fn reaches(def: SkySourceDef, y: i64, window: &RangeInclusive<i64>) -> bool {
    let r = i64::from(def.range) - 1;
    match def.dir {
        BeamDir::Down => *window.start() <= y && y <= window.end() + r,
        BeamDir::Up => window.start() - r <= y && y <= *window.end(),
    }
}

/// The farthest cell a source `def` at `y` may light: `range - 1` cells away in its direction,
/// clipped at the plane of its realm (`planes` ascending; plane `p` is the bottom cell of the
/// realm above it).
pub fn beam_limit(def: SkySourceDef, y: i64, planes: &[i64]) -> i64 {
    let r = i64::from(def.range) - 1;
    // Planes at or under the source: its realm's bottom is the last of them.
    let k = planes.partition_point(|&p| p <= y);
    match def.dir {
        BeamDir::Down => {
            let floor = if k > 0 { planes[k - 1] } else { i64::MIN };
            (y - r).max(floor)
        }
        BeamDir::Up => {
            let ceiling = planes.get(k).map_or(i64::MAX, |&p| p - 1);
            (y + r).min(ceiling)
        }
    }
}

fn pack_flags(def: SkySourceDef) -> u16 {
    let up = if def.dir == BeamDir::Up { FLAG_UP } else { 0 };
    (def.range - 1) | up
}

fn unpack_flags(flags: u16) -> SkySourceDef {
    SkySourceDef {
        dir: if flags & FLAG_UP != 0 {
            BeamDir::Up
        } else {
            BeamDir::Down
        },
        range: (flags & FLAG_RANGE) + 1,
    }
}

impl BeamTile {
    /// A tile without beams for the window `window` (`lo ..= hi`, at most [`MAX_WINDOW`] rows).
    /// Panics on an empty or taller window.
    pub fn empty(window: RangeInclusive<i64>) -> Self {
        let (w_lo, w_hi) = window.into_inner();
        assert!(
            w_lo <= w_hi && w_hi - w_lo < MAX_WINDOW,
            "beam tile window {w_lo}..={w_hi} is empty or taller than {MAX_WINDOW}"
        );
        Self {
            w_lo,
            w_hi,
            base: w_lo - i64::from(MAX_BEAM),
            index: Box::new([]),
            beams: Vec::new(),
            lo: 1,
            hi: 0,
            version: 0,
        }
    }

    /// The rows the tile is exact for.
    pub fn window(&self) -> RangeInclusive<i64> {
        self.w_lo..=self.w_hi
    }

    /// True if the window holds every row of `lo ..= hi`.
    pub fn covers(&self, lo: i64, hi: i64) -> bool {
        self.w_lo <= lo && hi <= self.w_hi
    }

    /// The number of beams.
    pub fn len(&self) -> usize {
        self.beams.len()
    }

    /// True without beams.
    pub fn is_empty(&self) -> bool {
        self.beams.is_empty()
    }

    /// The lowest and highest beam cell of the tile, `None` without beams.
    pub fn bounds(&self) -> Option<(i64, i64)> {
        (self.lo <= self.hi).then_some((self.lo, self.hi))
    }

    /// True if some beam has a cell in `lo ..= hi` of some column (from the bounds alone).
    #[inline]
    pub(crate) fn may_cross(&self, lo: i64, hi: i64) -> bool {
        self.lo <= hi && lo <= self.hi
    }

    /// What the offsets of [`BeamTile::packed`] count from.
    #[inline]
    pub(crate) fn base(&self) -> i64 {
        self.base
    }

    /// The beams of column `col` (`(x << 5) | z`), sorted by `lo`.
    #[inline]
    pub(crate) fn packed(&self, col: usize) -> &[Packed] {
        if self.index.is_empty() {
            return &[];
        }
        &self.beams[self.index[col] as usize..self.index[col + 1] as usize]
    }

    fn unpack(&self, b: Packed) -> Beam {
        Beam {
            source: self.base + i64::from(b.y),
            lo: self.base + i64::from(b.lo),
            hi: self.base + i64::from(b.hi),
            def: unpack_flags(b.flags),
        }
    }

    fn pack(&self, b: &Beam) -> Packed {
        let off = |v: i64| {
            debug_assert!((1..1 << 16).contains(&(v - self.base)), "beam cell {v}");
            (v - self.base) as u16
        };
        Packed {
            lo: off(b.lo),
            hi: off(b.hi),
            y: off(b.source),
            flags: pack_flags(b.def),
        }
    }

    /// The beams of local column `(x, z)` (each in `0..32`), sorted by their lowest cell (then
    /// highest cell, source and emission).
    pub fn column(&self, x: u32, z: u32) -> impl ExactSizeIterator<Item = Beam> + '_ {
        let col = ((x as usize) << CHUNK_SHIFT) | z as usize;
        self.packed(col).iter().map(|&b| self.unpack(b))
    }

    /// Every beam with its local column `(x, z)`, by column (`(x << 5) | z`), then as
    /// [`BeamTile::column`].
    pub fn iter(&self) -> impl Iterator<Item = (u32, u32, Beam)> + '_ {
        (0..CHUNK_AREA).flat_map(move |col| {
            let (x, z) = ((col >> CHUNK_SHIFT) as u32, (col & 31) as u32);
            self.column(x, z).map(move |b| (x, z, b))
        })
    }

    /// Heap bytes of the tile (2 KiB of index plus 8 B per beam when it has beams).
    pub fn heap_bytes(&self) -> usize {
        self.index.len() * size_of::<u32>() + self.beams.capacity() * size_of::<Packed>()
    }

    /// Sets the tile's beams from `found` (any order), with its index and bounds.
    fn set_beams(&mut self, mut found: Vec<(usize, Packed)>) {
        found.sort_unstable();
        self.beams = found.iter().map(|&(_, b)| b).collect();
        self.index = if found.is_empty() {
            Box::new([])
        } else {
            let mut index = vec![0u32; INDEX_LEN];
            for &(col, _) in &found {
                index[col + 1] += 1;
            }
            for i in 1..INDEX_LEN {
                index[i] += index[i - 1];
            }
            index.into_boxed_slice()
        };
        self.update_bounds();
    }

    fn update_bounds(&mut self) {
        let (lo, hi) = self
            .beams
            .iter()
            .fold((u16::MAX, 0), |(lo, hi), b| (lo.min(b.lo), hi.max(b.hi)));
        (self.lo, self.hi) = if self.beams.is_empty() {
            (1, 0)
        } else {
            (self.base + i64::from(lo), self.base + i64::from(hi))
        };
    }

    /// Updates the beams of column `(x, z)` after the voxel at `y` became `state` (an edit of
    /// the world content), tracing in place what can change (ADR 0061, "Ediciones"):
    ///
    /// - a source at `y` that was there is gone, with its beam; a new one (if `state` is a
    ///   source that reaches the window) is traced from `y`;
    /// - a blocker (opacity above 0) inside a beam cuts it after `y`;
    /// - a clear voxel where a beam ended (just past its last cell, inside its range and realm)
    ///   lets it grow, traced from `y` to the next blocker or its limit.
    ///
    /// Any other edit leaves the beams as they are (a beam that ended at its range never grows).
    /// `planes` are the realm planes the tile was built with. `layer(cy)` reads chunk layer `cy`
    /// of the column as it is after the edit, `None` when the caller does not have it (typically
    /// not loaded): then the result is [`Retrace::Unresolved`] and the tile is unchanged. The
    /// result equals a [`build`] of the edited content, and says which cells changed.
    #[expect(
        clippy::too_many_arguments,
        reason = "the column, the edit and the content"
    )]
    pub fn edit<'a>(
        &mut self,
        x: u32,
        z: u32,
        y: i64,
        state: BlockState,
        planes: &[i64],
        tables: &LightTables,
        mut layer: impl FnMut(i64) -> Option<LayerData<'a>>,
    ) -> Retrace {
        // Every beam lies in the scan range, and a source outside it cannot reach the window.
        if y < self.w_lo - REACH || y > self.w_hi + REACH {
            return Retrace::Unchanged;
        }
        let col = ((x as usize) << CHUNK_SHIFT) | z as usize;
        let old: Vec<Beam> = self.column(x, z).collect();
        let blocks = tables.blocks_sky(state);
        let mut new: Vec<Beam> = old.iter().copied().filter(|b| b.source != y).collect();
        let mut retrace = |from: i64, limit: i64| trace(x, z, from, limit, tables, &mut layer);
        for b in &mut new {
            let limit = beam_limit(b.def, b.source, planes);
            match b.def.dir {
                BeamDir::Down if y < b.source && y >= limit => {
                    if blocks && y >= b.lo {
                        b.lo = y + 1;
                    } else if !blocks && y == b.lo - 1 {
                        let Some(lo) = retrace(y, limit) else {
                            return Retrace::Unresolved;
                        };
                        b.lo = lo;
                    }
                }
                BeamDir::Up if y > b.source && y <= limit => {
                    if blocks && y <= b.hi {
                        b.hi = y - 1;
                    } else if !blocks && y == b.hi + 1 {
                        let Some(hi) = retrace(y, limit) else {
                            return Retrace::Unresolved;
                        };
                        b.hi = hi;
                    }
                }
                _ => {}
            }
        }
        if let Some(def) = tables.sky_source(state)
            && reaches(def, y, &self.window())
        {
            let Some(end) = retrace(y, beam_limit(def, y, planes)) else {
                return Retrace::Unresolved;
            };
            let (lo, hi) = if def.dir == BeamDir::Down {
                (end, y)
            } else {
                (y, end)
            };
            new.push(Beam {
                source: y,
                lo,
                hi,
                def,
            });
        }
        let mut packed: Vec<Packed> = new.iter().map(|b| self.pack(b)).collect();
        packed.sort_unstable();
        if packed.as_slice() == self.packed(col) {
            return Retrace::Unchanged;
        }
        // Splice the column in, shifting the offsets of the columns after it.
        if self.index.is_empty() {
            self.index = vec![0u32; INDEX_LEN].into_boxed_slice();
        }
        let (start, end) = (self.index[col] as usize, self.index[col + 1] as usize);
        let delta = packed.len() as i64 - (end - start) as i64;
        self.beams.splice(start..end, packed);
        for i in &mut self.index[col + 1..] {
            *i = (i64::from(*i) + delta) as u32;
        }
        if self.beams.is_empty() {
            self.index = Box::new([]);
        }
        self.update_bounds();
        self.version = self.version.wrapping_add(1);
        new.sort_unstable_by_key(|b| b.lo);
        match changed_band(&merged(&old), &merged(&new)) {
            Some((lo, hi)) => Retrace::Changed { lo, hi },
            None => Retrace::Unchanged,
        }
    }
}

/// The far end of a beam whose cell `from` is clear, going toward `limit` (`from` included):
/// the last clear cell before a blocker, or `limit`. `None` if a layer is missing.
fn trace<'a>(
    x: u32,
    z: u32,
    from: i64,
    limit: i64,
    tables: &LightTables,
    layer: &mut impl FnMut(i64) -> Option<LayerData<'a>>,
) -> Option<i64> {
    let step: i64 = if limit < from { -1 } else { 1 };
    let mut end = from;
    let mut current: Option<(i64, LayerData<'a>)> = None;
    while end != limit {
        let y = end + step;
        let cy = y >> CHUNK_SHIFT;
        if current.as_ref().is_none_or(|(c, _)| *c != cy) {
            let data = if (-MAX_CHUNK_COORD..=MAX_CHUNK_COORD).contains(&cy) {
                layer(cy)?
            } else {
                LayerData::Air
            };
            // Below a void layer every layer is air: a down beam reaches its limit.
            if step < 0 && matches!(data, LayerData::Void) {
                return Some(limit);
            }
            current = Some((cy, data));
        }
        let chunk = current.as_ref().and_then(|(_, d)| d.chunk());
        let ly = (y - (cy << CHUNK_SHIFT)) as u32;
        if chunk.is_some_and(|c| tables.blocks_sky(c.get(LocalPos::new(x, ly, z)))) {
            break;
        }
        end = y;
    }
    Some(end)
}

/// The cells of `beams` (sorted by `lo`) as disjoint, non-adjacent intervals, ascending.
fn merged(beams: &[Beam]) -> Vec<(i64, i64)> {
    let mut out: Vec<(i64, i64)> = Vec::new();
    for b in beams {
        match out.last_mut() {
            Some(last) if b.lo <= last.1 + 1 => last.1 = last.1.max(b.hi),
            _ => out.push((b.lo, b.hi)),
        }
    }
    out
}

/// The lowest and highest cell in exactly one of the interval sets `a` and `b` (as
/// [`merged`] gives them), `None` if they cover the same cells.
fn changed_band(a: &[(i64, i64)], b: &[(i64, i64)]) -> Option<(i64, i64)> {
    let mut cuts: Vec<i64> = a
        .iter()
        .chain(b)
        .flat_map(|&(lo, hi)| [lo, hi + 1])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let inside = |set: &[(i64, i64)], y: i64| set.iter().any(|&(lo, hi)| lo <= y && y <= hi);
    let mut band: Option<(i64, i64)> = None;
    for w in cuts.windows(2) {
        if inside(a, w[0]) != inside(b, w[0]) {
            let (lo, hi) = (w[0], w[1] - 1);
            band = Some(band.map_or((lo, hi), |(l, _)| (l, hi)));
        }
    }
    band
}

/// A beam being traced.
struct Open {
    /// Column `(x << 5) | z`.
    col: usize,
    /// The source.
    y: i64,
    /// The farthest cell it may reach ([`beam_limit`]).
    limit: i64,
    /// Its far end, once found.
    end: i64,
    def: SkySourceDef,
    /// The next beam open in the same column (`NONE`: the last).
    next: u32,
}

/// No beam.
const NONE: u32 = u32::MAX;

/// The beams open in each column during one pass of [`build`]: a list per column, linked
/// through [`Open::next`], and the open columns as bits (`z` of word `x`).
struct Pass {
    head: Box<[u32; CHUNK_AREA]>,
    /// The farthest limit of the column's open beams in the pass direction (the lowest for a
    /// down pass): once the pass is past it, every beam of the column ended at its limit.
    far: Box<[i64; CHUNK_AREA]>,
    active: [u32; 32],
    count: u32,
}

impl Pass {
    fn new(down: bool) -> Self {
        Self {
            head: Box::new([NONE; CHUNK_AREA]),
            far: Box::new([if down { i64::MAX } else { i64::MIN }; CHUNK_AREA]),
            active: [0; 32],
            count: 0,
        }
    }

    /// Opens beam `i` (in `open`) in its column.
    fn push(&mut self, open: &mut [Open], i: usize, down: bool) {
        let col = open[i].col;
        open[i].next = self.head[col];
        self.head[col] = i as u32;
        let far = &mut self.far[col];
        *far = if down {
            (*far).min(open[i].limit)
        } else {
            (*far).max(open[i].limit)
        };
        let (x, z) = (col >> CHUNK_SHIFT, col & 31);
        if self.active[x] & (1 << z) == 0 {
            self.active[x] |= 1 << z;
            self.count += 1;
        }
    }

    /// Closes the open beams of `col` at the cell just before `blocker` (`None`: at their
    /// limits).
    fn close(&mut self, open: &mut [Open], col: usize, blocker: Option<i64>, down: bool) {
        let mut i = std::mem::replace(&mut self.head[col], NONE);
        while i != NONE {
            let b = &mut open[i as usize];
            b.end = match (blocker, down) {
                (None, _) => b.limit,
                (Some(y), true) => (y + 1).max(b.limit),
                (Some(y), false) => (y - 1).min(b.limit),
            };
            i = b.next;
        }
        self.far[col] = if down { i64::MAX } else { i64::MIN };
        let (x, z) = (col >> CHUNK_SHIFT, col & 31);
        self.active[x] &= !(1 << z);
        self.count -= 1;
    }

    /// Closes at their limits the columns whose every open beam ends before reaching the layer
    /// `y0 ..= y0 + 31`.
    fn close_spent(&mut self, open: &mut [Open], y0: i64, down: bool) {
        for x in 0..32 {
            let mut bits = self.active[x];
            while bits != 0 {
                let z = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let col = (x << CHUNK_SHIFT) | z;
                let spent = if down {
                    self.far[col] > y0 + 31
                } else {
                    self.far[col] < y0
                };
                if spent {
                    self.close(open, col, None, down);
                }
            }
        }
    }

    /// Closes the open columns that meet a blocker in row `row` of `chunk`'s masks, at `y`.
    fn hit(&mut self, open: &mut [Open], masks: &LightMasks<'_>, row: usize, y: i64, down: bool) {
        for x in 0..32 {
            let a = self.active[x];
            if a == 0 {
                continue;
            }
            let mut hit = (masks.opaque.row(row + x) | masks.partial.row(row + x)) & a;
            while hit != 0 {
                let z = hit.trailing_zeros() as usize;
                hit &= hit - 1;
                self.close(open, (x << CHUNK_SHIFT) | z, Some(y), down);
            }
        }
    }
}

/// Builds the beam tile of one chunk column for the window `window` (at most [`MAX_WINDOW`]
/// rows): every sky source that [`reaches`] it, with its whole beam, clipped at the realm planes
/// `planes` (ascending, as `RealmStack::planes`).
///
/// `layer(cy)` gives chunk layer `cy`, from `(w_hi + MAX_BEAM - 1) >> 5` downwards, to
/// `(w_lo - MAX_BEAM + 1) >> 5`: as in [`crate::sky_tile::walk`], [`LayerData::AirUntil`] skips
/// air and [`LayerData::Void`] ends the reads (everything below is air). Layers past the world
/// limit read as air without a call. With tables that have no source, no layer is read.
/// A layer's masks are built only when a beam crosses it or it may hold a source
/// ([`ChunkData::may_source`](boxel_voxel::ChunkData::may_source)).
pub fn build<'a>(
    window: RangeInclusive<i64>,
    planes: &[i64],
    tables: &LightTables,
    mut layer: impl FnMut(i64) -> LayerData<'a>,
) -> BeamTile {
    let mut tile = BeamTile::empty(window);
    if !tables.any_sky_source() {
        return tile;
    }
    let window = tile.window();
    let top = (tile.w_hi + REACH) >> CHUNK_SHIFT;
    let bottom = (tile.w_lo - REACH) >> CHUNK_SHIFT;
    // The column's layers, top first; `None` is air.
    let mut layers: Vec<Option<LayerData<'a>>> = Vec::new();
    layers.resize_with((top - bottom + 1) as usize, || None);
    let mut cy = top;
    while cy >= bottom {
        if cy > MAX_CHUNK_COORD {
            cy -= 1;
            continue;
        }
        if cy < -MAX_CHUNK_COORD {
            break;
        }
        match layer(cy) {
            LayerData::Void => break,
            LayerData::AirUntil(n) if n < cy => {
                cy = n;
                continue;
            }
            LayerData::Air | LayerData::AirUntil(_) => {}
            data => layers[(top - cy) as usize] = Some(data),
        }
        cy -= 1;
    }
    let mut open: Vec<Open> = Vec::new();
    for down in [true, false] {
        let mut pass = Pass::new(down);
        for k in 0..layers.len() {
            let i = if down { k } else { layers.len() - 1 - k };
            let y0 = (top - i as i64) << CHUNK_SHIFT;
            if pass.count > 0 {
                pass.close_spent(&mut open, y0, down);
            }
            let Some(chunk) = layers[i].as_ref().and_then(LayerData::chunk) else {
                continue;
            };
            let sources = chunk.sky_sources(tables);
            let first = open.len();
            for &(p, def) in sources {
                let y = y0 + i64::from(p.y());
                if (def.dir == BeamDir::Down) != down || !reaches(def, y, &window) {
                    continue;
                }
                open.push(Open {
                    col: ((p.x() as usize) << CHUNK_SHIFT) | p.z() as usize,
                    y,
                    limit: beam_limit(def, y, planes),
                    end: y,
                    def,
                    next: NONE,
                });
            }
            if pass.count == 0 && open.len() == first {
                continue;
            }
            let masks = chunk.light_masks(tables);
            // The new sources in pass order: by y, down from the top for down beams.
            let mut next = if down { open.len() } else { first };
            for step in 0..32u32 {
                let ly = if down { 31 - step } else { step };
                if pass.count > 0 {
                    pass.hit(
                        &mut open,
                        &masks,
                        (ly as usize) << CHUNK_SHIFT,
                        y0 + i64::from(ly),
                        down,
                    );
                }
                // A source is clear, so it opens after its own row's blockers.
                if down {
                    while next > first && open[next - 1].y == y0 + i64::from(ly) {
                        next -= 1;
                        pass.push(&mut open, next, down);
                    }
                } else {
                    while next < open.len() && open[next].y == y0 + i64::from(ly) {
                        pass.push(&mut open, next, down);
                        next += 1;
                    }
                }
            }
        }
        for col in 0..CHUNK_AREA {
            if pass.head[col] != NONE {
                pass.close(&mut open, col, None, down);
            }
        }
    }
    let found = open
        .iter()
        .map(|b| {
            let (lo, hi) = if b.def.dir == BeamDir::Down {
                (b.end, b.y)
            } else {
                (b.y, b.end)
            };
            let beam = Beam {
                source: b.y,
                lo,
                hi,
                def: b.def,
            };
            (b.col, tile.pack(&beam))
        })
        .collect();
    tile.set_beams(found);
    tile
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_round_trip_every_range_and_direction() {
        for dir in [BeamDir::Down, BeamDir::Up] {
            for range in 1..=MAX_BEAM {
                let def = SkySourceDef { dir, range };
                assert_eq!(unpack_flags(pack_flags(def)), def);
                assert_eq!(pack_flags(def) & !(FLAG_RANGE | FLAG_UP), 0);
            }
        }
    }

    #[test]
    fn the_changed_band_spans_every_differing_cell() {
        assert_eq!(changed_band(&[(0, 10)], &[(0, 10)]), None);
        assert_eq!(changed_band(&[(0, 10)], &[(3, 10)]), Some((0, 2)));
        assert_eq!(changed_band(&[(0, 10)], &[(0, 4), (8, 12)]), Some((5, 12)));
        assert_eq!(changed_band(&[], &[(5, 5)]), Some((5, 5)));
        assert_eq!(changed_band(&[(-4, -1)], &[]), Some((-4, -1)));
    }

    #[test]
    fn limits_stop_at_the_realm_planes() {
        let down = |range| SkySourceDef {
            dir: BeamDir::Down,
            range,
        };
        let up = |range| SkySourceDef {
            dir: BeamDir::Up,
            range,
        };
        let planes = [0, 4096];
        assert_eq!(beam_limit(down(256), 4100, &planes), 4096);
        assert_eq!(beam_limit(down(256), 4096, &planes), 4096);
        assert_eq!(beam_limit(down(4), 4100, &planes), 4097);
        assert_eq!(beam_limit(up(256), 4090, &planes), 4095);
        assert_eq!(beam_limit(up(256), -1, &planes), -1);
        assert_eq!(beam_limit(up(16), -100, &planes), -85);
        assert_eq!(beam_limit(down(256), -5, &planes), -260);
        assert_eq!(beam_limit(up(1), 7, &[]), 7);
    }
}
