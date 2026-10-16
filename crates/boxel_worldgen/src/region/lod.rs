//! The far LOD of a realm (ADR 0039 sec. 10): [`RegionGenerator`]'s [`LodSource`], one region
//! pass per node column over the `35 x 35` cell corners of its padded node.
//!
//! 1. The Context's fields at the corners ([`Context::lod_grid`], once).
//! 2. Selection: the kinds of the lattice points of the node plus the blend's reach, the windows
//!    of the lattice cells the corners fall in, and from them each corner's weights `c_k` with
//!    the chunks' formula, so a corner on a chunk column gets the chunk's bits. A lattice cell
//!    whose window holds one kind is interior: no weight, no blend (the per-block interior test:
//!    a lattice cell is `L / s` cells on a side, at least 4 x 4 cells up to level 6 at the default
//!    lattice). From cells as large as the lattice, every corner is a lattice point and `c_k` is
//!    a box count.
//! 3. Per kind present, [`TerrainKind::lod_heights`](super::TerrainKind::lod_heights) on the
//!    corners of the lattice cells whose window holds it (a mask), with its margin per cell.
//! 4. The exact blend of the corner heights.
//! 5. The coarse top of each cell. The blended height is a convex combination of the kinds', so
//!    inside a cell `H <= sum_k c_k T_k / (9 L^2)`, with `T_k` the largest corner of kind `k` plus
//!    its margin. While a cell lies in one lattice cell (cells of at most `L` blocks: level 8 at
//!    the default lattice) the bound is bilinear and its largest value is at a corner: the top is
//!    the largest of the 4 corners' `sum_k c_k T_k / (9 L^2)`. Beyond, the selection reads only
//!    the corners (every corner is a lattice point) and the top is the largest `T_k` of the kinds
//!    at the corners: an approximation that may miss a kind inside a cell, never asked for today
//!    (`MAX_LEVELS` is 6).
//! 6. The shape modifiers' coarse forms ([`ShapeModifier::lod`](super::ShapeModifier::lod)), then
//!    the realm's fluid rule on the corners without margin (a cell is wet when its 4 corners lie
//!    under the level, so coasts do not move with the level), then the surface stage's
//!    [`SurfaceStage::lod_top`](super::SurfaceStage::lod_top). A wet cell's ground is its highest
//!    corner's, without margin, so under the level: with a margin a shallow floor would reach the
//!    surface row far away and the sea would show as land.
//! 7. One pass of the cell rule (the demo's surface, generalized): a cell holding ground is solid
//!    with the state of its highest Solid block (the runs of the surface stage, then the realm's
//!    floor), or the fluid when the realm's fluid covers its ground; a cell over the ground is the
//!    fluid if the fluid fills half of it, else air. A node with no air cell (border included)
//!    and no fluid inside draws nothing and is emitted `Uniform(floor)`.
//!
//! The band: under the rock of every padded column, the realm's floor (`below`); over every
//! column's ground and fluid, air. Clipped to the realm's span: no cell at or over the top plane
//! is drawn (the realm above is never in the far LOD), and the cells under the bottom plane are
//! the floor.
//!
//! The memo ([`LodScratch::memo`]) keeps, for one generator at a time (its build key): the
//! blocks of the registry, a table of lattice points' kinds, the Context's fields at the last
//! corner grid (one slot) and each kind's corner heights and margins there (a slot per kind), so
//! a second job of the same column (tall columns) recomputes nothing. Off
//! ([`RegionSettings::memo`](super::RegionSettings::memo)), every call starts empty.

use super::blend::{self, Counts, Window};
use super::decorate::{LodColumns, LodTop};
use super::generator::RegionGenerator;
use super::kind::{Context, HEIGHT_SHIFT, HeightQ, KindGrid, block_of};
use crate::block;
use crate::lod::{
    LOD_PADDED, LodCells, LodClass, LodClasses, LodColumn, LodNode, LodRequest, LodScratch,
    LodSource,
};
use crate::realm::FluidRule;
use crate::registry::{BuildKey, BuildMemo};
use boxel_voxel::{BlockRegistry, BlockState};

/// Name in panics.
const NAME: &str = "RegionGenerator (far LOD)";

/// Cell columns along a padded node edge.
pub(crate) const EDGE: usize = LOD_PADDED;
/// Cell corners along a padded node edge.
pub(crate) const CORNERS: usize = EDGE + 1;
/// Cells in one horizontal layer of a padded node: the stride of `y` in [`LodCells`].
const LAYER: usize = EDGE * EDGE;

/// Lattice points per side of the memo's table: direct-mapped, `32 x 32` (8 km at the default
/// lattice), so the lattice points of one node never evict each other up to level 6.
const TABLE: usize = 32;

/// The blocks of a registry the pass writes.
#[derive(Clone, Debug)]
struct Blocks {
    classes: LodClasses,
    floor: BlockState,
    /// The realm's fluid, `AIR` without one.
    fluid: BlockState,
}

/// One cell column of a padded node, as the cell pass reads it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Col {
    /// The `y` of its top solid block without margin: its highest corner's ground.
    pub(crate) h: i64,
    /// The `y` of its coarse ground's top: the bound of its heights and its surface rise, never
    /// under `h`; `h` if it is wet.
    pub(crate) t: i64,
    /// Rows at or under this `y` are the realm's floor (under the surface stage's runs).
    rock: i64,
    /// The fluid's level if the realm's fluid covers it, `i64::MIN` if it is dry.
    pub(crate) wet: i64,
    /// Its surface stage's runs from `h` down: an index into [`Slots::runs`] (columns share them).
    runs: u32,
}

/// A surface stage's runs from a column's ground down, classified: state, blocks, Solid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Runs {
    top: LodTop,
    runs: [(BlockState, i64, bool); 4],
    len: u8,
    depth: i64,
}

impl Runs {
    fn of(top: LodTop, classes: &LodClasses) -> Self {
        let (mut runs, mut len, mut depth) = ([(BlockState::AIR, 0, false); 4], 0, 0i64);
        for (state, n) in top.used() {
            runs[len] = (state, i64::from(n), classes.class(state) == LodClass::Solid);
            depth = depth.saturating_add(i64::from(n));
            len += 1;
        }
        Self {
            top,
            runs,
            len: len as u8,
            depth,
        }
    }

    fn used(&self) -> &[(BlockState, i64, bool)] {
        &self.runs[..usize::from(self.len)]
    }
}

impl Col {
    const EMPTY: Self = Self {
        h: 0,
        t: 0,
        rock: 0,
        wet: i64::MIN,
        runs: 0,
    };

    /// Whether the realm's fluid covers it.
    pub(crate) const fn is_wet(&self) -> bool {
        self.wet != i64::MIN
    }

    /// Its highest block: ground or fluid.
    const fn top(&self) -> i64 {
        if self.wet > self.t { self.wet } else { self.t }
    }
}

/// A kind's slot: its mask, corner heights and margins at the slots' corner grid.
#[derive(Clone, Debug, Default)]
struct KindSlot {
    /// Whether the kind is present at the corner grid (its heights and margins are set).
    present: bool,
    mask: Vec<bool>,
    heights: Vec<HeightQ>,
    margins: Vec<HeightQ>,
}

/// A lattice cell the corners fall in: its window's kinds with their counts.
#[derive(Clone, Debug)]
struct LatticeCell {
    /// The lattice cell, in lattice units.
    i: i64,
    j: i64,
    /// Its window's kinds and counts, by kind index.
    present: Vec<(u32, Counts)>,
}

impl LatticeCell {
    fn single(&self) -> Option<u32> {
        match self.present.as_slice() {
            [(k, _)] => Some(*k),
            _ => None,
        }
    }
}

/// The buffers of the surface stage's call, per cell column ([`LodColumns`]).
#[derive(Clone, Debug, Default)]
struct Look {
    corner: Vec<u32>,
    ground: Vec<i64>,
    fluid_top: Vec<i64>,
    tops: Vec<LodTop>,
}

/// What one generator keeps on a LOD worker (module docs).
pub(crate) struct Slots<Cx: Context> {
    /// The registry the blocks came from (its address and size), and its blocks.
    blocks: Option<((usize, usize), Blocks)>,
    /// The corner grid the slots below hold: `(level, padded min corner x, z)`.
    at: Option<(u8, i64, i64)>,
    pub(crate) xs: Vec<i64>,
    pub(crate) zs: Vec<i64>,
    /// The Context's slot: its fields at the corners.
    fields: Option<Cx::Grid>,
    /// A slot per kind of the realm.
    kinds: Vec<KindSlot>,
    /// Lattice points' kinds: `(i, j, kind)`, `u32::MAX` for an empty slot.
    lattice: Vec<(i64, i64, u32)>,
    /// Per kind of a lattice cell and corner column, the `(A_k, B_k)` of its weights.
    ab: Vec<(i64, i64)>,
    /// The blended and shaped corner heights.
    pub(crate) corners: Vec<HeightQ>,
    /// Per cell, the bound of its heights (in [`HeightQ`], margins included).
    pub(crate) tops: Vec<i64>,
    /// The cell columns.
    pub(crate) cols: Vec<Col>,
    /// The distinct runs of the cell columns.
    runs: Vec<Runs>,
    /// What the surface stage reads and writes, per cell column.
    look: Look,
    /// The band (before the request's layers).
    pub(crate) band: LodColumn,
}

impl<Cx: Context> Slots<Cx> {
    fn new() -> Self {
        Self {
            blocks: None,
            at: None,
            xs: Vec::new(),
            zs: Vec::new(),
            fields: None,
            kinds: Vec::new(),
            lattice: Vec::new(),
            ab: Vec::new(),
            corners: Vec::new(),
            tops: Vec::new(),
            cols: Vec::new(),
            runs: Vec::new(),
            look: Look::default(),
            band: LodColumn::AIR,
        }
    }

    /// Forgets everything kept (the allocations stay).
    fn clear(&mut self) {
        self.blocks = None;
        self.at = None;
        self.fields = None;
        for k in &mut self.kinds {
            k.present = false;
        }
        self.lattice.clear();
    }
}

/// The far LOD memo of a region generator, in [`LodScratch::memo`] (module docs).
pub(crate) struct LodMemo<Cx: Context> {
    memo: BuildMemo<Slots<Cx>>,
}

impl<Cx: Context> LodMemo<Cx> {
    /// The memo `scratch` holds, if it is one of `Cx`'s, else a new one (whatever another source
    /// kept there is dropped).
    pub(crate) fn take(scratch: &mut LodScratch) -> Box<Self> {
        scratch
            .memo
            .take()
            .and_then(|m| m.downcast::<Self>().ok())
            .unwrap_or_else(|| {
                Box::new(Self {
                    memo: BuildMemo::new(Slots::new()),
                })
            })
    }

    /// The slots of the generator of `key`, emptied if another build filled them, or on every
    /// call with the memo off (`keep` false).
    pub(crate) fn slots(&mut self, key: BuildKey, keep: bool) -> &mut Slots<Cx> {
        let slots = self.memo.get(key, Slots::clear);
        if !keep {
            slots.clear();
        }
        slots
    }

    /// The bytes the memo holds on the heap, besides the Context's fields (the game's).
    #[cfg(feature = "test-support")]
    pub(crate) fn heap_bytes(&mut self, key: BuildKey) -> usize {
        use std::mem::size_of;
        let s = self.memo.get(key, Slots::clear);
        let kinds: usize = s
            .kinds
            .iter()
            .map(|k| {
                k.mask.capacity()
                    + k.heights.capacity() * size_of::<HeightQ>()
                    + k.margins.capacity() * size_of::<HeightQ>()
            })
            .sum();
        kinds
            + s.kinds.capacity() * size_of::<KindSlot>()
            + (s.xs.capacity() + s.zs.capacity()) * size_of::<i64>()
            + s.lattice.capacity() * size_of::<(i64, i64, u32)>()
            + s.corners.capacity() * size_of::<HeightQ>()
            + s.tops.capacity() * size_of::<i64>()
            + s.cols.capacity() * size_of::<Col>()
            + s.runs.capacity() * size_of::<Runs>()
            + s.ab.capacity() * size_of::<(i64, i64)>()
            + s.look.corner.capacity() * size_of::<u32>()
            + (s.look.ground.capacity() + s.look.fluid_top.capacity()) * size_of::<i64>()
            + s.look.tops.capacity() * size_of::<LodTop>()
            + s.blocks.as_ref().map_or(0, |(_, b)| {
                b.classes.len() * size_of::<(LodClass, BlockState)>()
            })
    }
}

/// The block `y` of a height `v` over `datum` that may lie outside [`HeightQ`]: `datum + (v >>
/// 8)`, saturating.
const fn block_of_wide(datum: i64, v: i64) -> i64 {
    datum.saturating_add(v >> HEIGHT_SHIFT)
}

/// The largest of the 4 corners of cell `(a, b)` in a corner grid (`CORNERS` wide).
#[inline]
fn max4<T: Copy + Ord>(v: &[T], a: usize, b: usize) -> T {
    let k = a * CORNERS + b;
    v[k].max(v[k + CORNERS])
        .max(v[k + 1])
        .max(v[k + CORNERS + 1])
}

/// [`max4`] with the index of the first corner that holds it (order `00, 10, 01, 11`), as
/// selects rather than branches (the corners of a terrain go every way).
#[inline]
fn argmax4<T: Copy + Ord>(v: &[T], a: usize, b: usize) -> (T, usize) {
    let k = a * CORNERS + b;
    let mut best = (v[k], k);
    for c in [k + CORNERS, k + 1, k + CORNERS + 1] {
        let better = v[c] > best.0;
        best = (
            if better { v[c] } else { best.0 },
            if better { c } else { best.1 },
        );
    }
    best
}

/// `band` cut at `top`, the first `y` the far LOD must not draw (a realm's top plane), at a level
/// of `2^level` blocks a cell: no layer whose padded block lies at or over it holds anything.
pub(crate) fn cut_band(band: LodColumn, top: i64, level: u8) -> LodColumn {
    let (s, e) = (1i128 << level, 32i128 << level);
    // The first layer whose bottom border row starts at or over the plane.
    let first = (i128::from(top) + s + e - 1).div_euclid(e);
    let hi = band.hi.min(i64::try_from(first).unwrap_or(i64::MAX));
    LodColumn {
        hi,
        lo: band.lo.min(hi),
        ..band
    }
}

/// What [`RegionGenerator::fill`] found in a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fill {
    /// The cells.
    Cells,
    /// No air anywhere and no fluid inside (none in its border either at level 1, for the fluid
    /// walls): nothing to draw.
    Ground,
    /// All air.
    Air,
}

impl<Cx: Context> RegionGenerator<Cx> {
    /// Steps 1 to 6 of the pass (module docs) for the node column of `req`, into `slots`: the
    /// corners, tops, cell columns and band. A second call for the same corner grid reuses them.
    pub(crate) fn lod_prepare(
        &self,
        req: &LodRequest,
        registry: &BlockRegistry,
        slots: &mut Slots<Cx>,
    ) {
        let level = req.level;
        let s = req.cell_size();
        let [x0, _, z0] = req.origin(0);
        let (px, pz) = (x0 - s, z0 - s);
        let reg_key = (std::ptr::from_ref(registry) as usize, registry.len());
        if slots.blocks.as_ref().is_none_or(|(k, _)| *k != reg_key) {
            let spec = self.spec();
            let fluid = match &spec.fluid {
                FluidRule::SeaLevel { fluid, .. } => block(registry, fluid, NAME),
                FluidRule::None => BlockState::AIR,
            };
            let blocks = Blocks {
                classes: LodClasses::from_registry(registry),
                floor: block(registry, &spec.floor, NAME),
                fluid,
            };
            slots.blocks = Some((reg_key, blocks));
            // The cell columns hold the old registry's states.
            slots.at = None;
        }
        if slots.at == Some((level, px, pz)) {
            return;
        }
        slots.at = None;
        slots.xs.clear();
        slots
            .xs
            .extend((0..CORNERS as i64).map(|i| px.saturating_add(i * s)));
        slots.zs.clear();
        slots
            .zs
            .extend((0..CORNERS as i64).map(|j| pz.saturating_add(j * s)));
        slots.fields = Some(self.cx.lod_grid(&slots.xs, &slots.zs, level));
        if slots.kinds.len() != self.kinds.len() {
            slots.kinds.resize_with(self.kinds.len(), KindSlot::default);
        }
        for k in &mut slots.kinds {
            k.present = false;
            k.mask.clear();
            k.mask.resize(CORNERS * CORNERS, false);
        }
        slots.corners.clear();
        slots.corners.resize(CORNERS * CORNERS, 0);
        slots.tops.clear();
        slots.tops.resize(EDGE * EDGE, 0);
        if s <= 1i64 << self.shift {
            self.lod_blend(level, (px, pz), slots);
        } else {
            self.lod_corners_only(level, slots);
        }
        self.lod_shape(level, slots);
        self.lod_columns(level, registry, slots);
        slots.band = self.lod_band(req, &slots.cols);
        slots.at = Some((level, px, pz));
    }

    /// The kind of lattice point `(i, j)` through the slots' table (or the selector alone with the
    /// memo off: the table is then empty on every call).
    fn lod_lattice_kind(&self, table: &mut Vec<(i64, i64, u32)>, i: i64, j: i64) -> u32 {
        if table.is_empty() {
            table.resize(TABLE * TABLE, (0, 0, u32::MAX));
        }
        let slot = (i as usize % TABLE) * TABLE + j as usize % TABLE;
        let (ti, tj, k) = table[slot];
        if k != u32::MAX && ti == i && tj == j {
            return k;
        }
        let k = self
            .selector
            .select(&self.cx, i << self.shift, j << self.shift);
        table[slot] = (i, j, k);
        k
    }

    /// Calls each present kind's `lod_heights` on its mask.
    fn lod_kind_heights(&self, level: u8, slots: &mut Slots<Cx>) {
        let fields = slots.fields.as_ref().expect("set by lod_prepare");
        for (k, slot) in self.kinds.iter().zip(&mut slots.kinds) {
            if !slot.present {
                continue;
            }
            slot.heights.resize(CORNERS * CORNERS, 0);
            slot.margins.resize(EDGE * EDGE, 0);
            let grid = KindGrid {
                cx: &self.cx,
                fields,
                xs: &slots.xs,
                zs: &slots.zs,
                seed: k.seed,
            };
            k.kind.lod_heights(
                &grid,
                level,
                &slot.mask,
                &mut slot.heights,
                &mut slot.margins,
            );
        }
    }

    /// Steps 2 to 5 for cells of at most a lattice cell: exact selection, blend and bilinear tops.
    fn lod_blend(&self, level: u8, (px, pz): (i64, i64), slots: &mut Slots<Cx>) {
        let (ls, s) = (self.shift, 1i64 << level);
        let l = 1i64 << ls;
        let span = EDGE as i64 * s;
        let (ci0, ci1) = (px >> ls, px.saturating_add(span) >> ls);
        let (cj0, cj1) = (pz >> ls, pz.saturating_add(span) >> ls);
        // The lattice points the windows read: one cell around, two past.
        let (ni, nj) = ((ci1 - ci0 + 4) as usize, (cj1 - cj0 + 4) as usize);
        let mut kinds = vec![0u32; ni * nj];
        for (a, row) in kinds.chunks_exact_mut(nj).enumerate() {
            for (b, k) in row.iter_mut().enumerate() {
                *k = self.lod_lattice_kind(
                    &mut slots.lattice,
                    ci0 - 1 + a as i64,
                    cj0 - 1 + b as i64,
                );
            }
        }
        let mut cells = Vec::with_capacity((ni - 3) * (nj - 3));
        for ci in ci0..=ci1 {
            for cj in cj0..=cj1 {
                let (a0, b0) = ((ci - ci0) as usize, (cj - cj0) as usize);
                let window = Window(std::array::from_fn(|n| {
                    kinds[(a0 + n / 4) * nj + b0 + n % 4]
                }));
                cells.push(LatticeCell {
                    i: ci,
                    j: cj,
                    present: window.present(),
                });
            }
        }
        // Corner index ranges of a lattice cell along one axis: `(first, last closed, last
        // owned)`, so the closed cell `[c L, (c + 1) L]` and the columns it owns `[c L, (c + 1)
        // L)`.
        let range = |c: i64, p: i64| {
            let lo = (c * l - p).div_euclid(s).clamp(0, EDGE as i64) as usize;
            let hi = ((c + 1) * l - p).div_euclid(s);
            (
                lo,
                hi.clamp(0, EDGE as i64) as usize,
                (hi - 1).clamp(-1, EDGE as i64),
            )
        };
        for cell in &cells {
            let ((a0, a1, _), (b0, b1, _)) = (range(cell.i, px), range(cell.j, pz));
            for &(k, _) in &cell.present {
                let slot = &mut slots.kinds[k as usize];
                slot.present = true;
                for a in a0..=a1 {
                    slot.mask[a * CORNERS + b0..=a * CORNERS + b1].fill(true);
                }
            }
        }
        self.lod_kind_heights(level, slots);
        // Steps 4 and 5, lattice cell by lattice cell: the corners it owns (as a chunk column
        // would) get their blended heights, and the cells whose columns lie in it their bound.
        let n_x = CORNERS;
        for cell in &cells {
            let ((a0, a_last, a_own), (b0, b_last, b_own)) = (range(cell.i, px), range(cell.j, pz));
            let (a_cells, b_cells) = (a_own.min(EDGE as i64 - 1), b_own.min(EDGE as i64 - 1));
            if let Some(k) = cell.single() {
                let slot = &slots.kinds[k as usize];
                if a_own >= a0 as i64 && b_own >= b0 as i64 {
                    for a in a0..=a_own as usize {
                        let r = a * CORNERS + b0..=a * CORNERS + b_own as usize;
                        slots.corners[r.clone()].copy_from_slice(&slot.heights[r]);
                    }
                }
                if a_cells >= a0 as i64 && b_cells >= b0 as i64 {
                    for a in a0..=a_cells as usize {
                        for b in b0..=b_cells as usize {
                            slots.tops[a * EDGE + b] = i64::from(max4(&slot.heights, a, b))
                                + i64::from(slot.margins[a * EDGE + b]);
                        }
                    }
                }
                continue;
            }
            // `c_k = (L - fz) A_k(x) + fz B_k(x)` with `A_k = n00 (L - fx) + n10 fx` and `B_k =
            // n01 (L - fx) + n11 fx`, the cell's own formula over its closed range of corners.
            let present = &cell.present;
            slots.ab.clear();
            slots.ab.resize(present.len() * n_x, (0, 0));
            for (q, &(_, n)) in present.iter().enumerate() {
                for a in a0..=a_last {
                    let fx = slots.xs[a] - cell.i * l;
                    slots.ab[q * n_x + a] =
                        (n[0] * (l - fx) + n[1] * fx, n[2] * (l - fx) + n[3] * fx);
                }
            }
            let fz = |b: usize| slots.zs[b] - cell.j * l;
            if a_own >= a0 as i64 && b_own >= b0 as i64 {
                for a in a0..=a_own as usize {
                    for b in b0..=b_own as usize {
                        let (fz, c) = (fz(b), a * CORNERS + b);
                        let sum: i64 = present
                            .iter()
                            .enumerate()
                            .map(|(q, &(k, _))| {
                                let (ak, bk) = slots.ab[q * n_x + a];
                                ((l - fz) * ak + fz * bk)
                                    * i64::from(slots.kinds[k as usize].heights[c])
                            })
                            .sum();
                        slots.corners[c] = blend::blend(sum, ls);
                    }
                }
            }
            if a_cells < a0 as i64 || b_cells < b0 as i64 {
                continue;
            }
            debug_assert!((a_cells as usize) < a_last && (b_cells as usize) < b_last);
            // Per cell: `T_k`, then `sum_k c_k T_k` at its 4 corners from `S_A(x) = sum_k T_k
            // A_k(x)` and `S_B(x)`; the largest, floored once.
            let mut t = [0i64; 16];
            for a in a0..=a_cells as usize {
                for b in b0..=b_cells as usize {
                    let at = a * EDGE + b;
                    for (t, &(k, _)) in t.iter_mut().zip(present) {
                        let slot = &slots.kinds[k as usize];
                        *t = i64::from(max4(&slot.heights, a, b)) + i64::from(slot.margins[at]);
                    }
                    let (z0, z1) = (fz(b), fz(b + 1));
                    let mut best = i64::MIN;
                    for x in [a, a + 1] {
                        let (mut sa, mut sb) = (0i64, 0i64);
                        for (q, &tk) in t[..present.len()].iter().enumerate() {
                            let (ak, bk) = slots.ab[q * n_x + x];
                            sa += tk * ak;
                            sb += tk * bk;
                        }
                        best = best
                            .max((l - z0) * sa + z0 * sb)
                            .max((l - z1) * sa + z1 * sb);
                    }
                    slots.tops[at] = (best >> (2 * ls)).div_euclid(blend::BOX);
                }
            }
        }
    }

    /// Steps 2 to 5 for cells larger than a lattice cell: the kinds at the corners alone (module
    /// docs).
    fn lod_corners_only(&self, level: u8, slots: &mut Slots<Cx>) {
        let ls = self.shift;
        let mut at = vec![0u32; CORNERS * CORNERS];
        for (a, row) in at.as_chunks_mut::<CORNERS>().0.iter_mut().enumerate() {
            for (b, k) in row.iter_mut().enumerate() {
                *k = self.selector.select(&self.cx, slots.xs[a], slots.zs[b]);
            }
        }
        debug_assert!(slots.xs.iter().all(|x| x & ((1 << ls) - 1) == 0));
        for a in 0..EDGE {
            for b in 0..EDGE {
                let corners = [(a, b), (a + 1, b), (a, b + 1), (a + 1, b + 1)];
                for (ka, kb) in corners {
                    let slot = &mut slots.kinds[at[ka * CORNERS + kb] as usize];
                    slot.present = true;
                    for (ca, cb) in corners {
                        slot.mask[ca * CORNERS + cb] = true;
                    }
                }
            }
        }
        self.lod_kind_heights(level, slots);
        for (c, &k) in at.iter().enumerate() {
            slots.corners[c] = slots.kinds[k as usize].heights[c];
        }
        for a in 0..EDGE {
            for b in 0..EDGE {
                let k = a * CORNERS + b;
                let mut top = i64::MIN;
                for c in [k, k + CORNERS, k + 1, k + CORNERS + 1] {
                    let slot = &slots.kinds[at[c] as usize];
                    top = top.max(
                        i64::from(max4(&slot.heights, a, b))
                            + i64::from(slot.margins[a * EDGE + b]),
                    );
                }
                slots.tops[a * EDGE + b] = top;
            }
        }
    }

    /// The shape modifiers' coarse forms over the corners and tops. In debug builds each one is
    /// held to its effect at the corners, and every top to the corners of its cell.
    fn lod_shape(&self, level: u8, slots: &mut Slots<Cx>) {
        let fields = slots.fields.as_ref().expect("set by lod_prepare");
        for m in &self.modifiers {
            let before = cfg!(debug_assertions).then(|| slots.corners.clone());
            let grid = KindGrid {
                cx: &self.cx,
                fields,
                xs: &slots.xs,
                zs: &slots.zs,
                seed: m.seed,
            };
            m.modifier
                .lod(&grid, level, &mut slots.corners, &mut slots.tops);
            if let Some(before) = before {
                self.check_effect(m, (&slots.xs, &slots.zs), &before, &slots.corners);
            }
        }
        if cfg!(debug_assertions) {
            for a in 0..EDGE {
                for b in 0..EDGE {
                    let corner = max4(&slots.corners, a, b);
                    assert!(
                        i64::from(corner) <= slots.tops[a * EDGE + b],
                        "{NAME}: realm {} cell ({}, {}) of level {level} has a top {} under its \
                         corner {corner}: a kind's margin or a shape modifier's coarse form is \
                         unsound",
                        self.realm(),
                        slots.xs[a],
                        slots.zs[b],
                        slots.tops[a * EDGE + b],
                    );
                }
            }
        }
    }

    /// Step 6: the fluid rule and the surface stage's tops, into the cell columns.
    fn lod_columns(&self, level: u8, registry: &BlockRegistry, slots: &mut Slots<Cx>) {
        let spec = self.spec();
        let level_y = match spec.fluid {
            FluidRule::SeaLevel { level, .. } => Some(level),
            FluidRule::None => None,
        };
        let rise = i64::from(self.reach.rise);
        let Look {
            corner,
            ground,
            fluid_top,
            tops,
        } = &mut slots.look;
        corner.resize(EDGE * EDGE, 0);
        ground.resize(EDGE * EDGE, 0);
        fluid_top.resize(EDGE * EDGE, 0);
        tops.clear();
        tops.resize(EDGE * EDGE, LodTop::default());
        slots.cols.clear();
        slots.cols.resize(EDGE * EDGE, Col::EMPTY);
        for a in 0..EDGE {
            for b in 0..EDGE {
                let at = a * EDGE + b;
                let (hq, c) = argmax4(&slots.corners, a, b);
                let h = block_of(spec.datum, hq);
                let mut t = block_of_wide(spec.datum, slots.tops[at])
                    .saturating_add(rise)
                    .max(h);
                let mut wet = i64::MIN;
                if let Some(level) = level_y
                    && h < level
                {
                    // Under the fluid the ground keeps no margin: a margin over a shallow floor
                    // would reach the surface row and turn the sea to land far away.
                    wet = level;
                    t = h;
                }
                corner[at] = c as u32;
                ground[at] = h;
                fluid_top[at] = if wet == i64::MIN { h } else { wet };
                let col = &mut slots.cols[at];
                col.h = h;
                col.t = t;
                col.wet = wet;
            }
        }
        let columns = LodColumns {
            level,
            cx: &self.cx,
            fields: slots.fields.as_ref().expect("set by lod_prepare"),
            xs: &slots.xs,
            zs: &slots.zs,
            corner,
            ground,
            fluid_top,
        };
        self.surface.lod_top(&columns, registry, tops);
        let blocks = &slots.blocks.as_ref().expect("set by lod_prepare").1;
        // The runs, classified once for the columns that share them (most do): a column looks
        // among the last few kept, else keeps its own.
        slots.runs.clear();
        for (col, top) in slots.cols.iter_mut().zip(tops.iter()) {
            let recent = slots.runs.len().saturating_sub(8);
            let ix = match slots.runs[recent..].iter().rposition(|r| r.top == *top) {
                Some(ix) => recent + ix,
                None => {
                    slots.runs.push(Runs::of(*top, &blocks.classes));
                    slots.runs.len() - 1
                }
            };
            col.runs = ix as u32;
            col.rock = col.h.saturating_sub(slots.runs[ix].depth);
        }
    }

    /// The band of the cell columns (module docs). The cells under the realm's bottom plane are
    /// the floor and those at or over its top plane air (`lod_fill`), so each column's rock and
    /// top are taken inside the span.
    fn lod_band(&self, req: &LodRequest, cols: &[Col]) -> LodColumn {
        let (s, shift) = (req.cell_size(), 5 + u32::from(req.level));
        let span = self.spec().span;
        let inside = |y: i64| {
            let y = span.lo.map_or(y, |lo| y.max(lo - 1));
            span.hi.map_or(y, |hi| y.min(hi - 1))
        };
        let lowest = cols.iter().map(|c| inside(c.rock)).min().unwrap_or(0);
        let highest = cols.iter().map(|c| inside(c.top())).max().unwrap_or(0);
        LodColumn {
            lo: lowest.saturating_sub(s) >> shift,
            hi: (highest.saturating_add(s) >> shift).saturating_add(1),
            below: BlockState::AIR,
        }
    }

    /// The state of the cell `y0..y0 + s` of cell column `c` (step 7, module docs).
    fn lod_cell(
        c: &Col,
        runs: &[(BlockState, i64, bool)],
        blocks: &Blocks,
        y0: i64,
        s: i64,
    ) -> BlockState {
        let y1 = y0 + s - 1;
        if y1 <= c.rock {
            return blocks.floor;
        }
        if y0 > c.t {
            // No ground: the fluid if it fills half of the cell.
            return if c.is_wet() && 2 * (c.wet.min(y1) - y0.max(c.t + 1) + 1) >= s {
                blocks.fluid
            } else {
                BlockState::AIR
            };
        }
        // The fluid over the ground, one block deep over every column at least.
        if c.is_wet() && c.t < y1 && c.wet >= y0 {
            return blocks.fluid;
        }
        // The highest Solid block of `y0..=min(y1, t)`: over `h` the top run's (the margin and the
        // rise), then the runs, then the floor.
        let top = y1.min(c.t);
        if top > c.h {
            match runs.first() {
                Some(&(state, _, true)) => return state,
                Some(_) => {}
                None => return blocks.floor,
            }
        }
        let mut y = top.min(c.h);
        let mut run_top = c.h;
        for &(state, n, solid) in runs {
            let run_bottom = run_top - n;
            if y > run_bottom {
                if y < y0 {
                    return BlockState::AIR;
                }
                if solid {
                    return state;
                }
                y = run_bottom;
            }
            run_top = run_bottom;
        }
        if y >= y0 {
            blocks.floor
        } else {
            BlockState::AIR
        }
    }

    /// Step 7: fills `cells` with the node whose cell row 0 starts at block `y` (the padded rows
    /// -1 to 32 included), cells of `2^level` blocks, row by row. `rows` is a buffer.
    fn lod_fill(
        &self,
        slots: &Slots<Cx>,
        (y, level): (i64, u32),
        rows: &mut Vec<(u8, u8)>,
        cells: &mut LodCells,
    ) -> Fill {
        let blocks = &slots.blocks.as_ref().expect("set by lod_prepare").1;
        let span = self.spec().span;
        let s = 1i64 << level;
        // Padded row `r` (0..34) starts at block `bottom + r * s`.
        let bottom = y - s;
        let row = |at: i64| (at.saturating_sub(bottom) >> level).clamp(0, EDGE as i64) as u8;
        // Rows wholly under the bottom plane are the floor; rows from the top plane on, air.
        let below_span = span.lo.map_or(0, row);
        let above_span = span
            .hi
            .map_or(EDGE as u8, |hi| row(hi.saturating_add(s - 1)));
        // Per column: rows under `stone` are the floor (wholly under the rock), rows from `open`
        // on are air (wholly over everything), the rows between take the cell rule.
        rows.clear();
        rows.extend(slots.cols.iter().map(|c| {
            let stone = row(c.rock).max(below_span).min(above_span);
            let open = row(c.top().saturating_add(s)).min(above_span).max(stone);
            (stone, open)
        }));
        let lowest = rows.iter().map(|r| r.0).min().unwrap_or(0);
        let highest = rows.iter().map(|r| r.1).max().unwrap_or(0);
        let has_fluid = blocks.fluid != BlockState::AIR;
        let (mut air, mut fluid, mut fluid_inside, mut solid) = (
            rows.iter().any(|r| r.1 < EDGE as u8),
            false,
            false,
            rows.iter().any(|r| r.0 > 0),
        );
        // The floor under each column's `stone` and air from it on, row by row: a select the
        // compiler can vectorize; then the cell rule over each column's rows in between.
        for (r, layer) in cells.0.as_chunks_mut::<LAYER>().0.iter_mut().enumerate() {
            let r8 = r as u8;
            if r8 < lowest {
                layer.fill(blocks.floor);
            } else if r8 >= highest {
                layer.fill(BlockState::AIR);
            } else {
                for (cell, &(stone, _)) in layer.iter_mut().zip(rows.iter()) {
                    *cell = if r8 < stone {
                        blocks.floor
                    } else {
                        BlockState::AIR
                    };
                }
            }
        }
        for (i, (&(stone, open), c)) in rows.iter().zip(&slots.cols).enumerate() {
            let inner = (1..=32).contains(&(i / EDGE)) && (1..=32).contains(&(i % EDGE));
            for r in usize::from(stone)..usize::from(open) {
                let runs = slots.runs[c.runs as usize].used();
                let state = Self::lod_cell(c, runs, blocks, bottom + r as i64 * s, s);
                cells.0[i + r * LAYER] = state;
                if state == BlockState::AIR {
                    air = true;
                } else if has_fluid && state == blocks.fluid {
                    fluid = true;
                    fluid_inside |= inner && (1..=32).contains(&r);
                } else {
                    solid = true;
                }
            }
        }
        if !air && !fluid_inside && (s > 2 || !fluid) {
            Fill::Ground
        } else if !fluid && !solid {
            Fill::Air
        } else {
            Fill::Cells
        }
    }

    /// The layers of `req` inside the prepared band, each emitted as its cells or uniform.
    fn lod_emit(
        &self,
        req: &LodRequest,
        slots: &Slots<Cx>,
        cells: &mut LodCells,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        let shift = 5 + u32::from(req.level);
        let floor = slots.blocks.as_ref().expect("set by lod_prepare").1.floor;
        let band = LodColumn {
            below: floor,
            ..slots.band
        };
        let first = (*req.layers.start()).max(band.lo);
        let last = (*req.layers.end()).min(band.hi - 1);
        let mut rows = Vec::with_capacity(LAYER);
        for layer in first..=last {
            let level = u32::from(req.level);
            let node = match self.lod_fill(slots, (layer << shift, level), &mut rows, cells) {
                Fill::Cells => LodNode::Cells(cells),
                Fill::Ground => LodNode::Uniform(floor),
                Fill::Air => LodNode::Uniform(BlockState::AIR),
            };
            emit(layer, node);
        }
        band
    }
}

/// The region pass (module docs). Its version is the generator's.
impl<Cx: Context> LodSource for RegionGenerator<Cx> {
    fn column(
        &self,
        req: &LodRequest,
        registry: &BlockRegistry,
        scratch: &mut LodScratch,
        emit: &mut dyn FnMut(i64, LodNode<'_>),
    ) -> LodColumn {
        let mut memo = LodMemo::<Cx>::take(scratch);
        let slots = memo.slots(self.key, self.memo);
        self.lod_prepare(req, registry, slots);
        let band = self.lod_emit(req, slots, &mut scratch.cells, emit);
        scratch.memo = Some(memo);
        band
    }

    fn version(&self) -> u32 {
        Self::version(self)
    }
}
