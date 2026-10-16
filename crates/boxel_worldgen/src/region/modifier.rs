//! Shape modifiers (ADR 0039 sec. 9 and 10): stages of a realm's decoration that reshape the
//! blended height grid before the fluid rule (rivers, lakes, coasts, cliff risers, dunes), each
//! with a grid form for chunks and points and a coarse form for the far LOD, and the divide term
//! that keeps the coarse tops of a carve by segments sound ([`divide_tops`]).

use super::kind::{Context, HeightQ, KindGrid};
use crate::exact::Slope;
use crate::field::Rect;
use crate::registry::{GenEntry, HashParams, ParamsHasher};

/// What a shape modifier may do to a column's height (ADR 0039 sec. 9), declared: the column
/// bounds, the deep path and the far LOD's tops count on it, and in debug builds every chunk and
/// LOD node checks it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShapeEffect {
    /// Lowers columns and never raises one, by at most this many 1/256 blocks (its deepest cut):
    /// rivers, lakes, canyons. The column bounds' `ground_lo` and the deep path go down by it.
    /// The far LOD may keep the uncarved tops (always sound); a carve by segments sharpens them
    /// with the divide term ([`divide_tops`]).
    LowerOnly(u32),
    /// Raises columns and never lowers one, by at most this many 1/256 blocks: cliff risers,
    /// dunes. The column bounds' top and the far LOD's tops go up by it.
    Raise(u32),
}

impl ShapeEffect {
    /// The deepest cut, in 1/256 blocks (0 for a raise).
    pub const fn lower(self) -> u32 {
        match self {
            Self::LowerOnly(d) => d,
            Self::Raise(_) => 0,
        }
    }

    /// The highest rise, in 1/256 blocks (0 for a cut).
    pub const fn raise(self) -> u32 {
        match self {
            Self::LowerOnly(_) => 0,
            Self::Raise(r) => r,
        }
    }

    /// Whether the effect may turn a height `before` into `after`.
    pub const fn allows(self, before: HeightQ, after: HeightQ) -> bool {
        let (before, after) = (before as i64, after as i64);
        match self {
            Self::LowerOnly(d) => after <= before && before - after <= d as i64,
            Self::Raise(r) => after >= before && after - before <= r as i64,
        }
    }

    /// Whether every change this effect allows, `other` allows too: no deeper cut and no higher
    /// rise (a column effect inside the modifier's own, [`ShapeModifier::column_effect`]).
    pub const fn within(self, other: Self) -> bool {
        self.lower() <= other.lower() && self.raise() <= other.raise()
    }
}

impl HashParams for ShapeEffect {
    fn hash_params(&self, h: &mut ParamsHasher) {
        match self {
            Self::LowerOnly(d) => h.write(&0u8).write(d),
            Self::Raise(r) => h.write(&1u8).write(r),
        };
    }
}

/// A shape modifier (ADR 0039 sec. 9): it reshapes the blended heights of a realm, the same way
/// for every kind (rivers, lakes and the shape of the coast belong to the whole realm). Given to
/// a realm's [`Decoration`](super::Decoration) under its id; the modifiers run by `(priority,
/// id)`, the higher priority first ([`GenEntry::order`]), each with the seed of its id.
///
/// It is a pure function of `(seed, x, z)`, the Context and the height it gets, so a point, a
/// chunk and a LOD corner agree bit for bit; its effect ([`ShapeModifier::effect`]) is a promise
/// the debug checks hold it to.
pub trait ShapeModifier<Cx: Context>: GenEntry + Send + Sync {
    /// What it may do to a height.
    fn effect(&self) -> ShapeEffect;

    /// The slope it may add to the heights it gets, in [`HeightQ`] per block, its rounding as
    /// jitter: where they keep a slope of `L` along any line, its output keeps `L + slope`. A
    /// cut or a rise that is the same at every column adds [`Slope::ZERO`]; a river carve
    /// `min(H, floor + (H - floor) g(n))` adds its walls' steepness, its deepest cut times the
    /// slope of `g(n)` along a line.
    ///
    /// The freeze check holds it to the 3:1 cap (ADR 0039 sec. 6 and 12): the realm's modifiers'
    /// slopes, summed, alone and on top of every kind, pair and triple of kinds they cut, by the
    /// rule the kinds' check uses.
    fn slope(&self) -> Slope;

    /// What it may do to the heights of the columns of `area` (a chunk column, or the part of a
    /// carver's apron in one lattice cell; ADR 0039 sec. 14), inside [`ShapeModifier::effect`]
    /// ([`ShapeEffect::within`]): a river says `LowerOnly(0)` where no segment reaches. The
    /// generator reads it once per chunk column for the column's bounds and the deep path, and
    /// in debug builds holds the chunk's heights to it. The default is the whole effect.
    fn column_effect(&self, cx: &Cx, seed: u64, area: Rect) -> ShapeEffect {
        let _ = (cx, seed, area);
        self.effect()
    }

    /// The grid form: rewrites `heights` (in [`HeightQ`], point `i * zs.len() + j` at `(xs[i],
    /// zs[j])` of `grid`) in place, for a chunk column, a point query or the corners of a LOD
    /// node.
    fn apply(&self, grid: &KindGrid<'_, Cx>, heights: &mut [HeightQ]);

    /// The coarse form, for a far LOD node of `level` whose cell corners are `grid`'s points:
    /// `corners` holds their heights (the blend's and the modifiers' before this one), and
    /// `tops` per cell (`tops[i * (zs.len() - 1) + j]`, in [`HeightQ`]) a bound over every
    /// height of the closed cell. On return `corners` holds this modifier's heights there and
    /// `tops` bounds them.
    ///
    /// The default ([`lod_by_effect`]) is the grid form at the corners and the tops of the
    /// effect: unchanged for a cut (the uncarved bound, always sound), raised by the rise for a
    /// raise. A carve by segments sharpens its tops with [`divide_tops`]; a modifier may also
    /// leave out at far levels what is too small to see (declared in
    /// [`ShapeModifier::lod_left_out`]).
    fn lod(&self, grid: &KindGrid<'_, Cx>, level: u8, corners: &mut [HeightQ], tops: &mut [i64]) {
        let _ = level;
        lod_by_effect(self, grid, corners, tops);
    }

    /// How far its coarse form may leave the far LOD ground over the real one at `level`, in
    /// [`HeightQ`]: the depth of what it leaves out there (small rivers far away). 0 by default.
    fn lod_left_out(&self, level: u8) -> HeightQ {
        let _ = level;
        0
    }
}

/// The default coarse form of a shape modifier ([`ShapeModifier::lod`]): its grid form at the
/// corners, and the tops raised by its rise (a cut keeps them: the heights only went down).
pub fn lod_by_effect<Cx: Context, M: ShapeModifier<Cx> + ?Sized>(
    modifier: &M,
    grid: &KindGrid<'_, Cx>,
    corners: &mut [HeightQ],
    tops: &mut [i64],
) {
    modifier.apply(grid, corners);
    let raise = i64::from(modifier.effect().raise());
    if raise > 0 {
        for top in tops {
            *top = top.saturating_add(raise);
        }
    }
}

/// A cell corner of a carve by segments, for [`divide_tops`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CarveCorner {
    /// The segment nearest to the corner: an id of that one segment (it may fold in the
    /// segment's order), so that two corners with the same id measure their distance to the same
    /// segment. An id that several segments share (an order alone) hides the divides between
    /// them: the distance to a set of segments is not convex.
    pub segment: u64,
    /// The carve's distance input there, `n` (in the carve's fixed point): the distance from the
    /// corner (warped, if the carve warps) to that segment. Rounded down or up to an integer, as
    /// the carve rounds it in its columns, the divide term holds as it is; rounded to the nearest,
    /// it takes one more unit of slack.
    pub n: i64,
}

/// The divide term (ADR 0039 sec. 10): the far LOD tops of a lower-only carve by segments.
///
/// The carve is `H' = carve(H, n)` (a river: `min(H, floor + (H - floor) g(n))`), nondecreasing
/// in the height `H` and in `n`, the distance from a column (warped by a warp `w`) to its nearest
/// segment. `corners` are the `(nx + 1) x (nz + 1)` cell corners of a grid of `nx x nz` cells
/// (corner `(i, j)` at `i * (nz + 1) + j`), and `tops` the cells' bounds of the uncarved heights
/// (`tops[i * nz + j]`), each rewritten to a bound of the carved ones:
///
/// - where the 4 corners of a cell share their nearest segment `A`, the cell's `n` stays under
///   the corners' largest plus `slack` ([`warp_slack`]), and its top becomes `min(top,
///   carve(cell, A, top, n_max))`. The distance to a segment is convex and 1-Lipschitz, so for a
///   point `p` of the cell with bilinear weights `l_c`, `n(p) <= d_A(p + w(p)) <= sum l_c n(c) +
///   sum l_c |w(p) - w(c)|`, and the last sum is at most the warp's slope over half the cell's
///   diagonal;
/// - where they do not, a divide between two valleys may lie inside the cell, higher than any
///   carved corner: the top stays the uncarved bound.
///
/// `carve(cell, segment, height, n)` bounds the carve of any height up to `height` at any
/// distance up to `n` from `segment` in cell `cell` (`i * nz + j`): what else the carve reads
/// (the floor of a river that falls toward the sea, the width of its order) it takes at its
/// highest over that segment and cell.
///
/// Conservative, and exact-safe: it only lowers tops it can prove. Panics if the slices do not
/// fit the grid.
pub fn divide_tops(
    (nx, nz): (usize, usize),
    corners: &[CarveCorner],
    slack: u64,
    tops: &mut [i64],
    carve: impl Fn(usize, u64, i64, i64) -> i64,
) {
    assert_eq!(corners.len(), (nx + 1) * (nz + 1), "divide_tops: corners");
    assert_eq!(tops.len(), nx * nz, "divide_tops: tops");
    let d = nz + 1;
    let slack = i64::try_from(slack).unwrap_or(i64::MAX);
    for i in 0..nx {
        for j in 0..nz {
            let k = i * d + j;
            let cell = [
                corners[k],
                corners[k + d],
                corners[k + 1],
                corners[k + d + 1],
            ];
            if cell.iter().any(|c| c.segment != cell[0].segment) {
                continue;
            }
            let n_max = cell.iter().map(|c| c.n).max().expect("4 corners");
            let at = i * nz + j;
            let top = tops[at];
            tops[at] = top.min(carve(at, cell[0].segment, top, n_max.saturating_add(slack)));
        }
    }
}

/// What a warp adds to a carve's distance inside a cell of `size` blocks, for [`divide_tops`]:
/// its slope (`displacement`, in the distance's units per block: the Lipschitz bound of the
/// offset it adds to a column, as a vector, [`Warp::displacement_slope`], not one component's)
/// over half the cell's diagonal ([`Rect::half_diagonal`]). 0 for no warp; about `0.35 size`
/// blocks for a warp of slope 1/2.
///
/// [`Warp::displacement_slope`]: crate::noise::Warp::displacement_slope
pub fn warp_slack(displacement: Slope, size: i64) -> u64 {
    if displacement == Slope::ZERO {
        return 0;
    }
    displacement.over(Rect::cell(0, 0, size).half_diagonal())
}
