//! The layout of a dimension's realms along y (ADR 0039 sec. 2): each realm's slab between two
//! planes ([`RealmSpan`]), its form ([`RealmForm`]) and whether the far LOD draws it
//! ([`RealmLod`]), and the freeze checks of a layout ([`check_realms`]).
//!
//! Only what those checks need lives here so far. The rest of a realm's spec (datum, fluid
//! rule, floor, sky, environment) and the realm stack that dispatches chunks by `y` come with
//! WG-REGION, which builds on these types.

use crate::registry::{Checks, GenId, HashParams, ParamsHasher, Problem};
use std::collections::BTreeSet;
use std::fmt;

/// Planes between realms sit on multiples of this many blocks: no far LOD node up to level 7
/// (4,096 blocks) straddles one, and a chunk is always in exactly one realm.
pub const REALM_PLANE: i64 = 4096;

/// At most this many planes per dimension, so finding the realm of a `y` stays a binary search
/// of three steps.
pub const MAX_REALM_PLANES: usize = 8;

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

    /// The span as `[lo, hi)` in `i128`, open ends past every `i64`.
    const fn wide(self) -> (i128, i128) {
        let lo = match self.lo {
            Some(lo) => lo as i128,
            None => i128::MIN,
        };
        let hi = match self.hi {
            Some(hi) => hi as i128,
            None => i128::MAX,
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
/// - every span holds some `y` ([`Problem::EmptySpan`]) and its planes are multiples of
///   [`REALM_PLANE`] ([`Problem::OffPlane`]);
/// - no two spans overlap ([`Problem::Overlap`], once per pair);
/// - at most [`MAX_REALM_PLANES`] distinct planes ([`Problem::TooManyPlanes`]);
/// - exactly one realm with [`RealmLod::Draw`] ([`Problem::DrawRealms`]);
/// - every form is built ([`Problem::FormNotBuilt`]).
///
/// Gaps between spans are not checked here: what a `y` outside every realm generates is the
/// realm stack's business (WG-REGION).
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

#[cfg(test)]
mod tests;
