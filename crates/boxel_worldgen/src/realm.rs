//! The realms of a dimension along y (ADR 0039 sec. 2): each realm's slab between two planes
//! ([`RealmSpan`]), its form ([`RealmForm`]), whether the far LOD draws it ([`RealmLod`]), the
//! rest of its spec ([`RealmSpec`]: datum, [`FluidRule`], floor, [`RealmSky`], environment), the
//! freeze checks of a layout ([`check_realms`]) and the stack that finds the realm of a `y`
//! ([`RealmStack`]).
//!
//! A realm's terrain comes from its own generator, and the generator of a dimension dispatches each
//! chunk to the generator of its realm (WG-REGION).

use crate::registry::{Checks, GenEntry, GenId, HashParams, ParamsHasher, Problem};
use boxel_voxel::{CHUNK_SHIFT, MAX_BLOCK_COORD};
use std::collections::BTreeSet;
use std::fmt;
use std::ops::RangeInclusive;
use std::sync::LazyLock;

/// Planes between realms sit on multiples of this many blocks: no far LOD node up to level 7
/// (4,096 blocks) straddles one, and a chunk is always in exactly one realm.
pub const REALM_PLANE: i64 = 4096;

/// At most this many planes per dimension, so finding the realm of a `y` stays a binary search
/// of at most four comparisons (nine realms).
pub const MAX_REALM_PLANES: usize = 8;

/// [`RealmSpan::wide`]'s bottom of an open span: the lowest `y`.
const OPEN_LO: i128 = i64::MIN as i128;
/// [`RealmSpan::wide`]'s top of an open span: one past the highest `y`.
const OPEN_HI: i128 = i64::MAX as i128 + 1;

/// A realm's slab of the dimension, `lo <= y < hi`. `None` is an open end: `lo = None` goes
/// down forever, `hi = None` up forever.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RealmSpan {
    /// The bottom plane (inside the span), `None` for none.
    pub lo: Option<i64>,
    /// The top plane (outside the span), `None` for none.
    pub hi: Option<i64>,
}

impl RealmSpan {
    /// Every `y`.
    pub const ALL: Self = Self::new(None, None);

    /// `lo <= y < hi`.
    pub const fn new(lo: Option<i64>, hi: Option<i64>) -> Self {
        Self { lo, hi }
    }

    /// `lo <= y < hi`, both planes finite.
    pub const fn between(lo: i64, hi: i64) -> Self {
        Self::new(Some(lo), Some(hi))
    }

    /// `y < hi`.
    pub const fn below(hi: i64) -> Self {
        Self::new(None, Some(hi))
    }

    /// `y >= lo`.
    pub const fn above(lo: i64) -> Self {
        Self::new(Some(lo), None)
    }

    /// The `y` inside as `[lo, hi)` in `i128`: an open bottom is `i64::MIN`, an open top
    /// `i64::MAX + 1`. So the bounds are exact on `i64`: `below(i64::MIN)` is empty, and
    /// `between(i64::MIN, hi)` holds the same `y` as `below(hi)`.
    const fn wide(self) -> (i128, i128) {
        let lo = match self.lo {
            Some(lo) => lo as i128,
            None => OPEN_LO,
        };
        let hi = match self.hi {
            Some(hi) => hi as i128,
            None => OPEN_HI,
        };
        (lo, hi)
    }

    /// Whether no `y` is inside.
    pub const fn is_empty(self) -> bool {
        let (lo, hi) = self.wide();
        lo >= hi
    }

    /// Whether `y` is inside.
    pub const fn contains(self, y: i64) -> bool {
        let (lo, hi) = self.wide();
        lo <= y as i128 && (y as i128) < hi
    }

    /// Whether some `y` is in both spans (two spans that meet at a plane do not overlap).
    pub const fn overlaps(self, other: Self) -> bool {
        let (a_lo, a_hi) = self.wide();
        let (b_lo, b_hi) = other.wide();
        let lo = if a_lo > b_lo { a_lo } else { b_lo };
        let hi = if a_hi < b_hi { a_hi } else { b_hi };
        lo < hi
    }

    /// The finite planes, bottom first.
    pub fn planes(self) -> impl Iterator<Item = i64> {
        self.lo.into_iter().chain(self.hi)
    }
}

/// `[lo, hi)`, with `-inf` and `+inf` for open ends.
impl fmt::Display for RealmSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.lo {
            Some(lo) => write!(f, "[{lo}, ")?,
            None => f.write_str("(-inf, ")?,
        }
        match self.hi {
            Some(hi) => write!(f, "{hi})"),
            None => f.write_str("+inf)"),
        }
    }
}

impl HashParams for RealmSpan {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.lo).write(&self.hi);
    }
}

/// The form of a realm: which surfaces its kinds give (ADR 0039 sec. 2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RealmForm {
    /// Solid under one surface (the world's surface).
    Ground,
    /// Solid under a floor and over a roof, with the floor under the roof (a game's nether).
    Cavern,
    /// Solid over an underside (a ceiling of the sky).
    Ceiling,
    /// Air, with volume kinds only (sky islands).
    Void,
}

impl RealmForm {
    /// The forms the engine builds so far: [`RealmForm::Ground`] (WG-REGION). `Cavern` comes
    /// with WG-CAVERN, `Ceiling` with the sky ceiling step and `Void` when a realm needs it.
    pub const BUILT: &'static [Self] = &[Self::Ground];

    /// Whether the form is built ([`RealmForm::BUILT`]); the freeze check refuses the others.
    pub fn is_built(self) -> bool {
        Self::BUILT.contains(&self)
    }

    /// The form's stable number, for the parameter hash.
    const fn code(self) -> u8 {
        match self {
            Self::Ground => 0,
            Self::Cavern => 1,
            Self::Ceiling => 2,
            Self::Void => 3,
        }
    }
}

impl fmt::Display for RealmForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl HashParams for RealmForm {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.code());
    }
}

/// Whether the far LOD draws a realm. Exactly one realm per dimension is drawn (ADR 0039
/// sec. 10); the others are never in the LOD.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RealmLod {
    /// The far LOD draws this realm's band.
    Draw,
    /// Never in the far LOD.
    Off,
}

impl HashParams for RealmLod {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&u8::from(*self == Self::Off));
    }
}

/// The part of a realm the layout checks read: its span, its form and whether the LOD draws it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RealmLayout {
    /// The slab of y.
    pub span: RealmSpan,
    /// The form.
    pub form: RealmForm,
    /// Drawn by the far LOD or not.
    pub lod: RealmLod,
}

impl HashParams for RealmLayout {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.span).write(&self.form).write(&self.lod);
    }
}

/// The freeze checks of the realms of `dimension`, in the dimension's order (ADR 0039 sec. 12),
/// recorded in `checks`:
///
/// - every span holds some `y` ([`Problem::EmptySpan`]), and its planes are multiples of
///   [`REALM_PLANE`] ([`Problem::OffPlane`]) inside the world, `-MAX_BLOCK_COORD..=
///   MAX_BLOCK_COORD` ([`Problem::PlaneOutsideWorld`]; an open end is `None`, never a far
///   plane);
/// - every form is built ([`Problem::FormNotBuilt`]);
/// - no two spans overlap ([`Problem::Overlap`], once per pair) and no `y` is outside every span
///   ([`Problem::Gap`], once per uncovered stretch), so the realms cover every `y` exactly once
///   and finding the realm of a `y` never fails;
/// - at most [`MAX_REALM_PLANES`] distinct planes ([`Problem::TooManyPlanes`]);
/// - exactly one realm with [`RealmLod::Draw`] ([`Problem::DrawRealms`]).
pub fn check_realms<'a>(
    checks: &mut Checks,
    dimension: &GenId,
    realms: impl IntoIterator<Item = (&'a GenId, &'a RealmLayout)>,
) {
    let realms: Vec<(&GenId, &RealmLayout)> = realms.into_iter().collect();
    let mut planes = BTreeSet::new();
    for &(realm, layout) in &realms {
        if layout.span.is_empty() {
            checks.push(Problem::EmptySpan {
                dimension: dimension.clone(),
                realm: realm.clone(),
                span: layout.span,
            });
        }
        for y in layout.span.planes() {
            if y.rem_euclid(REALM_PLANE) != 0 {
                checks.push(Problem::OffPlane {
                    dimension: dimension.clone(),
                    realm: realm.clone(),
                    y,
                });
            }
            if !(-MAX_BLOCK_COORD..=MAX_BLOCK_COORD).contains(&y) {
                checks.push(Problem::PlaneOutsideWorld {
                    dimension: dimension.clone(),
                    realm: realm.clone(),
                    y,
                });
            }
            planes.insert(y);
        }
        if !layout.form.is_built() {
            checks.push(Problem::FormNotBuilt {
                dimension: dimension.clone(),
                realm: realm.clone(),
                form: layout.form,
            });
        }
    }
    for (i, &(a, la)) in realms.iter().enumerate() {
        for &(b, lb) in &realms[i + 1..] {
            if la.span.overlaps(lb.span) {
                checks.push(Problem::Overlap {
                    dimension: dimension.clone(),
                    a: a.clone(),
                    b: b.clone(),
                });
            }
        }
    }
    check_cover(checks, dimension, &realms);
    if planes.len() > MAX_REALM_PLANES {
        checks.push(Problem::TooManyPlanes {
            dimension: dimension.clone(),
            planes: planes.into_iter().collect(),
        });
    }
    let draw: Vec<GenId> = realms
        .iter()
        .filter(|(_, layout)| layout.lod == RealmLod::Draw)
        .map(|&(realm, _)| realm.clone())
        .collect();
    if draw.len() != 1 {
        checks.push(Problem::DrawRealms {
            dimension: dimension.clone(),
            draw,
        });
    }
}

/// Records a [`Problem::Gap`] for every stretch of `y` that no span of `realms` covers, bottom
/// first: a sweep over the spans sorted by their bottom plane.
fn check_cover(checks: &mut Checks, dimension: &GenId, realms: &[(&GenId, &RealmLayout)]) {
    let mut spans: Vec<(i128, i128, &GenId)> = realms
        .iter()
        .filter(|(_, layout)| !layout.span.is_empty())
        .map(|&(realm, layout)| {
            let (lo, hi) = layout.span.wide();
            (lo, hi, realm)
        })
        .collect();
    spans.sort_by_key(|&(lo, hi, _)| (lo, hi));
    // Every `y` under `covered` is covered; `top` is the realm that reaches it.
    let (mut covered, mut top) = (OPEN_LO, None);
    let mut gap = |lo: i128, hi: i128, below: Option<&GenId>, above: Option<&GenId>| {
        // Inside the sweep `lo` is the open bottom or the top of a span that holds some `y`
        // (so never `i64::MIN`), and `hi` a span's bottom plane or the open top: both convert
        // back to an `i64` plane or an open end.
        let plane = |y: i128, open: i128| (y != open).then_some(y as i64);
        checks.push(Problem::Gap {
            dimension: dimension.clone(),
            span: RealmSpan::new(plane(lo, OPEN_LO), plane(hi, OPEN_HI)),
            below: below.cloned(),
            above: above.cloned(),
        });
    };
    for &(lo, hi, realm) in &spans {
        if lo > covered {
            gap(covered, lo, top, Some(realm));
        }
        if hi > covered {
            (covered, top) = (hi, Some(realm));
        }
    }
    if covered < OPEN_HI {
        gap(covered, OPEN_HI, top, None);
    }
}

impl RealmSpan {
    /// The chunk layers inside the span (planes are multiples of [`REALM_PLANE`], so a chunk is
    /// inside or outside as a whole), empty for an empty span.
    pub fn chunk_layers(self) -> RangeInclusive<i64> {
        let lo = self.lo.map_or(i64::MIN, |lo| lo >> CHUNK_SHIFT);
        let hi = self.hi.map_or(i64::MAX, |hi| (hi >> CHUNK_SHIFT) - 1);
        if self.is_empty() {
            RangeInclusive::new(1, 0)
        } else {
            lo..=hi
        }
    }
}

/// The block name of air: a realm's [`RealmSpec::floor`] when nothing lies under its lowest
/// surface ([`RealmForm::Ceiling`], [`RealmForm::Void`]).
pub const AIR_NAME: &str = "boxel:air";

/// How the sky lights a realm (ADR 0039 sec. 15). The direction only: the sky light reads it once
/// EW-REALMSKY lands; until then every world is lit as today.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RealmSky {
    /// Lit from above: a voxel has direct sky over the realm's own content in its column.
    Open,
    /// Lit from below: the air between the realm's bottom plane and its first blocker (the air
    /// under a ceiling of the sky).
    Under,
    /// No direct sky (a game's nether); spread light still crosses the planes.
    Closed,
}

impl RealmSky {
    /// The stable number of the sky, for the parameter hash.
    const fn code(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Under => 1,
            Self::Closed => 2,
        }
    }
}

impl HashParams for RealmSky {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.code());
    }
}

/// The fluid of a realm, column by column (ADR 0039 sec. 2). Kinds never place fluid, so a
/// border between kinds never makes a wall of water.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FluidRule {
    /// No fluid.
    None,
    /// A column whose ground lies under `level` holds `fluid` from the block over its ground up
    /// to `level`, both included. It reads the column's height before the core terms (which do
    /// not exist yet, so the blended height), so a lowering core stays dry.
    SeaLevel {
        /// The `y` of the highest fluid block.
        level: i64,
        /// The fluid's block name (its default state, a source).
        fluid: String,
    },
}

impl FluidRule {
    /// The `y` of the highest fluid block a column of ground `ground` holds, `None` if it is dry.
    pub const fn top(&self, ground: i64) -> Option<i64> {
        match self {
            Self::SeaLevel { level, .. } if ground < *level => Some(*level),
            _ => None,
        }
    }
}

impl HashParams for FluidRule {
    fn hash_params(&self, h: &mut ParamsHasher) {
        match self {
            Self::None => h.write(&0u8),
            Self::SeaLevel { level, fluid } => h.write(&1u8).write(level).write(fluid),
        };
    }
}

/// A realm of a dimension (ADR 0039 sec. 2): registered under its id (`game:surface`) in the
/// realms' registry, so its id is not a field.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RealmSpec {
    /// The slab of `y`.
    pub span: RealmSpan,
    /// Which surfaces its kinds give. Only [`RealmForm::Ground`] is built.
    pub form: RealmForm,
    /// The origin of its heights (the sea level of a surface realm): kinds work in 1/256 of a
    /// block over it, and a height `h` is the block `datum + (h >> 8)`.
    pub datum: i64,
    /// Its fluid, column by column.
    pub fluid: FluidRule,
    /// The block name of the state under its lowest surface (its rock), [`AIR_NAME`] for the forms
    /// over air. The far LOD reports it under a column's band, and the deep path fills with it
    /// unless the realm's fill stage says otherwise.
    pub floor: String,
    /// How the sky lights it.
    pub sky: RealmSky,
    /// Whether the far LOD draws it (exactly one realm per dimension does).
    pub lod: RealmLod,
    /// Its environment: an id in the game's table of environments (fog, daylight, LOD on or off).
    pub env: GenId,
    /// The version of its entry ([`GenEntry::version`]).
    pub version: u32,
}

impl RealmSpec {
    /// The part of the spec the layout checks read ([`check_realms`]).
    pub const fn layout(&self) -> RealmLayout {
        RealmLayout {
            span: self.span,
            form: self.form,
            lod: self.lod,
        }
    }
}

impl HashParams for RealmSpec {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.span)
            .write(&self.form)
            .write(&self.datum)
            .write(&self.fluid)
            .write(&self.floor)
            .write(&self.sky)
            .write(&self.lod)
            .write(&self.env);
    }
}

impl GenEntry for RealmSpec {
    fn version(&self) -> u32 {
        self.version
    }
}

/// The checks of each realm's floor against its form (ADR 0039 sec. 12), recorded in `checks`: a
/// [`RealmForm::Ground`] or [`RealmForm::Cavern`] realm lies on rock, so its floor is not
/// [`AIR_NAME`]; a [`RealmForm::Ceiling`] or [`RealmForm::Void`] realm lies over air, so its floor
/// is ([`Problem::FloorPolicy`]). The far LOD reports the floor under the drawn realm's band, and
/// it must agree between neighbor columns, so a realm has one.
pub fn check_floors<'a>(
    checks: &mut Checks,
    dimension: &GenId,
    realms: impl IntoIterator<Item = (&'a GenId, &'a RealmSpec)>,
) {
    for (realm, spec) in realms {
        let over_air = matches!(spec.form, RealmForm::Ceiling | RealmForm::Void);
        if over_air != (spec.floor == AIR_NAME) {
            checks.push(Problem::FloorPolicy {
                dimension: dimension.clone(),
                realm: realm.clone(),
                form: spec.form,
                floor: spec.floor.clone(),
            });
        }
    }
}

/// The realms of a dimension, bottom first, and the planes between them: what a dimension's
/// generator dispatches by.
///
/// Built through the layout checks, its realms cover every `y` exactly once, so
/// [`RealmStack::realm_of`] always answers: a binary search over at most [`MAX_REALM_PLANES`]
/// planes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RealmStack {
    /// Bottom first.
    realms: Box<[(GenId, RealmSpec)]>,
    /// `planes[i]` is the bottom plane of realm `i + 1`.
    planes: Box<[i64]>,
    /// The realm the far LOD draws.
    drawn: usize,
}

/// The id of the realm (and its environment) of a generator that declares none.
const DEFAULT_REALM: &str = "boxel:default";

static SINGLE: LazyLock<RealmStack> = LazyLock::new(|| {
    let id = GenId::new(DEFAULT_REALM).expect("a valid id");
    RealmStack::of_one(
        id.clone(),
        RealmSpec {
            span: RealmSpan::ALL,
            form: RealmForm::Ground,
            datum: 0,
            fluid: FluidRule::None,
            floor: AIR_NAME.to_owned(),
            sky: RealmSky::Open,
            lod: RealmLod::Draw,
            env: id,
            version: 0,
        },
    )
});

impl RealmStack {
    /// The stack of `realms` in `dimension`, in any order, or `None` with the problems recorded in
    /// `checks`: the layout checks ([`check_realms`]) and the floor checks ([`check_floors`]).
    pub fn build(
        checks: &mut Checks,
        dimension: &GenId,
        mut realms: Vec<(GenId, RealmSpec)>,
    ) -> Option<Self> {
        let before = checks.found();
        let layouts: Vec<(&GenId, RealmLayout)> = realms
            .iter()
            .map(|(id, spec)| (id, spec.layout()))
            .collect();
        check_realms(checks, dimension, layouts.iter().map(|(id, l)| (*id, l)));
        check_floors(
            checks,
            dimension,
            realms.iter().map(|(id, spec)| (id, spec)),
        );
        if checks.found() > before {
            return None;
        }
        // Covering every `y` once, the realms sort by their bottoms: only the first is open.
        realms.sort_by_key(|(_, spec)| spec.span.lo);
        let planes = realms[1..]
            .iter()
            .map(|(_, spec)| spec.span.lo.expect("only the bottom realm is open below"))
            .collect();
        let drawn = realms
            .iter()
            .position(|(_, spec)| spec.lod == RealmLod::Draw)
            .expect("checked: one drawn realm");
        Some(Self {
            realms: realms.into(),
            planes,
            drawn,
        })
    }

    /// One realm, unchecked: a realm generator's own view, where `realm_of` gives it for every
    /// `y`.
    pub(crate) fn of_one(id: GenId, spec: RealmSpec) -> Self {
        Self {
            realms: vec![(id, spec)].into(),
            planes: Box::new([]),
            drawn: 0,
        }
    }

    /// The stack of a generator that declares no realm: one realm, `boxel:default`,
    /// spanning every `y`, with the [`RealmForm::Ground`] form, an open sky and the far LOD on
    /// (its own source, if any). Its floor is air: the engine does not know what lies under such
    /// a generator's terrain.
    pub fn single() -> &'static Self {
        &SINGLE
    }

    /// The number of realms.
    pub fn len(&self) -> usize {
        self.realms.len()
    }

    /// Whether the stack has no realm (never: a stack covers every `y`).
    pub fn is_empty(&self) -> bool {
        self.realms.is_empty()
    }

    /// The index of the realm of `y`: the number of planes at or under it.
    #[inline]
    pub fn realm_of(&self, y: i64) -> usize {
        self.planes.partition_point(|&plane| plane <= y)
    }

    /// The id of realm `ix`. Panics if out of range.
    pub fn id(&self, ix: usize) -> &GenId {
        &self.realms[ix].0
    }

    /// The spec of realm `ix`. Panics if out of range.
    pub fn spec(&self, ix: usize) -> &RealmSpec {
        &self.realms[ix].1
    }

    /// The index of the realm the far LOD draws.
    pub const fn drawn(&self) -> usize {
        self.drawn
    }

    /// The planes between the realms, ascending.
    pub fn planes(&self) -> &[i64] {
        &self.planes
    }

    /// Every realm with its id, bottom first.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&GenId, &RealmSpec)> {
        self.realms.iter().map(|(id, spec)| (id, spec))
    }
}

#[cfg(test)]
mod tests;
