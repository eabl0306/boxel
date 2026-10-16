//! Terrain kinds ([`TerrainKind`]) and the realm's shared fields they read ([`Context`]), ADR
//! 0039 sec. 5.

use crate::field::{Slope, Span};
use crate::registry::{GenEntry, GenId, HashParams, ParamsHasher};
use std::fmt;

/// A height in 1/256 of a block over a realm's datum ([`RealmSpec::datum`]): `+-2^23` blocks
/// around it. The block of a height `h` is `datum + (h >> 8)` (an arithmetic shift, so it rounds
/// down).
///
/// [`RealmSpec::datum`]: crate::realm::RealmSpec::datum
pub type HeightQ = i32;

/// The fraction bits of a [`HeightQ`].
pub const HEIGHT_SHIFT: u32 = 8;

/// One block in [`HeightQ`].
pub const HEIGHT_ONE: HeightQ = 1 << HEIGHT_SHIFT;

/// The block `y` of height `h` over `datum`: `datum + (h >> 8)`, saturating.
#[inline]
pub const fn block_of(datum: i64, h: HeightQ) -> i64 {
    datum.saturating_add((h >> HEIGHT_SHIFT) as i64)
}

/// A realm's shared fields (ADR 0039 sec. 5): one typed struct per realm, written by the game.
/// It computes the fields every kind of the realm reads (a base height, a climate) once per chunk
/// column or LOD node ([`Context::grid`]), and exposes named parameters as `i32` at lattice
/// points for the selector's conditions ([`Context::params`], [`Context::param`]).
///
/// The generator builds it from the realm's seed (the world's seed mixed with the realm's salt),
/// so it is a pure function of that seed and its parameters. [`HashParams`] writes its
/// parameters (never its seed), and [`Context::version`] the version of its code: both go to the
/// realm's digest line `context`.
///
/// `()` is the Context of a realm without shared fields or parameters.
pub trait Context: HashParams + Send + Sync + 'static {
    /// The shared fields over a grid of columns.
    type Grid: Send + Sync;

    /// The version of its code: bumped when it gives other fields for the same parameters.
    fn version(&self) -> u32;

    /// The ids of its parameters (`game:temperature`); a parameter's index here is what
    /// [`Context::param`] takes. None by default.
    fn params(&self) -> &[GenId] {
        &[]
    }

    /// Parameter `param` (an index into [`Context::params`]) at block `(x, z)`, a lattice point
    /// of the region map. The default panics: a Context without parameters is never asked.
    fn param(&self, param: usize, x: i64, z: i64) -> i32 {
        let _ = (x, z);
        panic!("Context::param({param}) of a Context without parameters")
    }

    /// The large warp of the region cells: the offset in blocks the selector adds to a lattice
    /// point before it finds the point's region cell. None by default.
    fn warp(&self, x: i64, z: i64) -> (i64, i64) {
        let _ = (x, z);
        (0, 0)
    }

    /// The shared fields at every point of the grid `xs` x `zs` (point `i * zs.len() + j` is
    /// `(xs[i], zs[j])`). A field's value at a point must not depend on the grid it is computed
    /// in, so a point, a chunk and a LOD corner agree bit for bit.
    fn grid(&self, xs: &[i64], zs: &[i64]) -> Self::Grid;
}

/// No shared fields and no parameters.
impl Context for () {
    type Grid = ();

    fn version(&self) -> u32 {
        0
    }

    fn grid(&self, _xs: &[i64], _zs: &[i64]) {}
}

/// What a kind computes its heights over ([`TerrainKind::heights`]): a grid of columns, the
/// realm's [`Context`] with its fields over that grid, and the kind's seed.
pub struct KindGrid<'a, Cx: Context> {
    /// The realm's Context.
    pub cx: &'a Cx,
    /// Its fields over the grid.
    pub fields: &'a Cx::Grid,
    /// The grid's x, in blocks.
    pub xs: &'a [i64],
    /// The grid's z: point `i * zs.len() + j` is `(xs[i], zs[j])`.
    pub zs: &'a [i64],
    /// The kind's seed: the realm's seed mixed with the kind's salt.
    pub seed: u64,
}

impl<Cx: Context> KindGrid<'_, Cx> {
    /// The number of points.
    pub const fn len(&self) -> usize {
        self.xs.len() * self.zs.len()
    }

    /// Whether the grid has no point.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What a kind gives (ADR 0039 sec. 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KindForm {
    /// One height per column, the surfaces of the realm's form.
    Height,
    /// A height and a volume in its regions' core (floating islands). Not built yet (WG-VOLUME):
    /// the checks refuse it.
    HeightAndVolume,
}

impl KindForm {
    /// The forms the engine builds so far: [`KindForm::Height`].
    pub const BUILT: &'static [Self] = &[Self::Height];

    /// Whether the form is built ([`KindForm::BUILT`]).
    pub fn is_built(self) -> bool {
        Self::BUILT.contains(&self)
    }
}

impl fmt::Display for KindForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// What a kind promises about its heights, for the freeze check of borders (ADR 0039 sec. 6).
///
/// Kinds add relief to the Context's shared base, so two kinds meet at a difference of reliefs:
/// within a blend band a kind's relief stays in `band`, and two kinds `a` and `b` differ by at
/// most `D_ab = max(band_a.hi - band_b.lo, band_b.hi - band_a.lo)` there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KindLimits {
    /// The slope of its heights, base included, in [`HeightQ`] per block (a slope of 1 block per
    /// block is 256): the [`Field::slope`](crate::field::Field::slope) of the fields it adds up.
    /// The border check takes its slope over one block, `per_block + jitter`, so its rounding
    /// counts against the cap.
    pub slope: Slope,
    /// Its relief over the base, anywhere, in [`HeightQ`].
    pub relief: Span,
    /// Its relief over the base within a blend band (inside `relief`), in [`HeightQ`].
    pub band: Span,
}

impl HashParams for KindLimits {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.slope.per_block.raw())
            .write(&self.slope.jitter)
            .write(&self.relief.lo)
            .write(&self.relief.hi)
            .write(&self.band.lo)
            .write(&self.band.hi);
    }
}

/// A terrain kind (ADR 0039 sec. 5): the heights of the regions the selector gives it, written
/// against its realm's [`Context`]. Registered under its id in a registry of `Arc<dyn
/// TerrainKind<Cx>>`; its [`GenEntry`] gives its version and parameter hash.
///
/// The generator calls it once per (chunk column, kind present) and once per point query; the
/// loops inside are the kind's, monomorphic. Every height is in [`HeightQ`] over the realm's
/// datum, and a kind's heights must be a pure function of `(seed, x, z)` and the Context: the
/// same at a point, in a chunk and at a LOD corner, bit for bit.
pub trait TerrainKind<Cx: Context>: GenEntry + Send + Sync {
    /// What it gives: [`KindForm::Height`] by default.
    fn form(&self) -> KindForm {
        KindForm::Height
    }

    /// Its slope and reliefs, for the border check (it may read the Context's base).
    fn limits(&self, cx: &Cx) -> KindLimits;

    /// Every height it gives anywhere: the realm's vertical range comes from it.
    fn range(&self, cx: &Cx) -> Span;

    /// Every height it gives within `reach` blocks (Chebyshev) of block `q`, a lattice point, in
    /// [`TerrainKind::range`]. The generator reads it at the 16 lattice points around a chunk
    /// column (with a reach of two lattice cells) for the column's bounds: its low end gives the
    /// realm's `ground_lo`, its high end the air above. A bound under the truth is a bug the
    /// debug span check catches. The default is the whole range.
    fn bounds(&self, cx: &Cx, seed: u64, q: (i64, i64), reach: i64) -> Span {
        let _ = (seed, q, reach);
        self.range(cx)
    }

    /// Its heights at the points of `grid` where `mask` is set (`mask` and `out` are indexed like
    /// the grid). The other entries of `out` may hold anything.
    fn heights(&self, grid: &KindGrid<'_, Cx>, mask: &[bool], out: &mut [HeightQ]);
}

/// A kind at one height everywhere: `height` over the datum, whatever the Context. Its relief
/// is its height (a flat base), so it suits a Context whose base is 0, such as `()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FlatKind {
    /// The height, in [`HeightQ`] over the datum.
    pub height: HeightQ,
}

impl FlatKind {
    /// The kind at `height` over the datum.
    pub const fn new(height: HeightQ) -> Self {
        Self { height }
    }

    /// The kind at `blocks` whole blocks over the datum.
    pub const fn blocks(blocks: i32) -> Self {
        Self::new(blocks << HEIGHT_SHIFT)
    }
}

impl HashParams for FlatKind {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.height);
    }
}

impl GenEntry for FlatKind {
    fn version(&self) -> u32 {
        1
    }
}

impl<Cx: Context> TerrainKind<Cx> for FlatKind {
    fn limits(&self, _cx: &Cx) -> KindLimits {
        let h = Span::point(self.height.into());
        KindLimits {
            slope: Slope::ZERO,
            relief: h,
            band: h,
        }
    }

    fn range(&self, _cx: &Cx) -> Span {
        Span::point(self.height.into())
    }

    fn heights(&self, _grid: &KindGrid<'_, Cx>, _mask: &[bool], out: &mut [HeightQ]) {
        out.fill(self.height);
    }
}
