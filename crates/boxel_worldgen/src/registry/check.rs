//! The freeze checks' framework (ADR 0039 sec. 12): the problems a check can find
//! ([`Problem`]), a collector that gathers them all ([`Checks`]) and the one error that reports
//! them ([`FreezeError`]).

use super::{GenEntry, GenId, GenIx, GenRegistry, GenRegistryBuilder};
use crate::realm::{MAX_REALM_PLANES, REALM_PLANE, RealmForm, RealmSpan};
use crate::region::{KindForm, MAX_SLOPE};
use boxel_voxel::{BlockRegistry, MAX_BLOCK_COORD};
use std::fmt;

/// One problem a freeze check found, with the ids it involves. Its message names them, so a
/// refused mix of mods says which entries clash.
///
/// The later steps add their checks here (wrappers that target missing slots, core terms, features
/// without a declared reach...). [`Problem::Other`] carries the game's own.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Problem {
    /// An id registered more than once in one registry.
    Duplicate {
        /// The registry ([`GenRegistryBuilder::new`]).
        registry: &'static str,
        /// The id.
        id: GenId,
        /// How many times it was registered.
        count: usize,
    },
    /// Two registries frozen with one name ([`Checks::freeze`]): their digest lines and messages
    /// could not be told apart.
    DuplicateRegistry {
        /// The name ([`GenRegistryBuilder::new`]).
        registry: &'static str,
    },
    /// An id that its registry does not hold, named by another entry or asked for at world
    /// open.
    Unknown {
        /// The registry it was looked up in.
        registry: &'static str,
        /// The id.
        id: GenId,
        /// The entry that names it (a preset, a dimension, a rule...), `None` for the root of a
        /// world open (its preset, which no entry names).
        by: Option<GenId>,
    },
    /// A block name that the block registry lacks, named by a world generation entry (a realm's
    /// floor or fluid, a decoration stage, a generator spec): a typo fails at world open, not in a
    /// worker in the middle of a game.
    UnknownBlock {
        /// The entry that names it.
        by: GenId,
        /// The block name.
        name: String,
    },
    /// A generator spec whose parameters are not of the type its generator kind reads.
    Params {
        /// The dimension the spec is for.
        dimension: GenId,
        /// The generator kind.
        kind: GenId,
        /// The type the kind reads.
        expected: &'static str,
    },
    /// A realm whose span holds no `y` (its bottom plane is not under its top one).
    EmptySpan {
        /// The dimension.
        dimension: GenId,
        /// The realm.
        realm: GenId,
        /// Its span.
        span: RealmSpan,
    },
    /// A realm plane that is not a multiple of [`REALM_PLANE`].
    OffPlane {
        /// The dimension.
        dimension: GenId,
        /// The realm.
        realm: GenId,
        /// The plane's `y`.
        y: i64,
    },
    /// A realm plane outside the world, `-MAX_BLOCK_COORD..=MAX_BLOCK_COORD` (an open end is
    /// `None`, not a far plane).
    PlaneOutsideWorld {
        /// The dimension.
        dimension: GenId,
        /// The realm.
        realm: GenId,
        /// The plane's `y`.
        y: i64,
    },
    /// Two realms of one dimension whose spans overlap.
    Overlap {
        /// The dimension.
        dimension: GenId,
        /// One realm.
        a: GenId,
        /// The other (the same as `a` when a dimension lists a realm twice).
        b: GenId,
    },
    /// A stretch of `y` that no realm of a dimension covers: the realms must cover every `y`
    /// exactly once.
    Gap {
        /// The dimension.
        dimension: GenId,
        /// The uncovered stretch.
        span: RealmSpan,
        /// The realm under it, `None` if the stretch goes down forever.
        below: Option<GenId>,
        /// The realm over it, `None` if the stretch goes up forever.
        above: Option<GenId>,
    },
    /// A dimension whose realms have more than [`MAX_REALM_PLANES`] planes.
    TooManyPlanes {
        /// The dimension.
        dimension: GenId,
        /// Every plane, ascending.
        planes: Vec<i64>,
    },
    /// A dimension without exactly one realm the far LOD draws.
    DrawRealms {
        /// The dimension.
        dimension: GenId,
        /// Its realms with `lod = Draw` (none, or more than one).
        draw: Vec<GenId>,
    },
    /// A realm whose form is not built yet.
    FormNotBuilt {
        /// The dimension.
        dimension: GenId,
        /// The realm.
        realm: GenId,
        /// Its form.
        form: RealmForm,
    },
    /// A realm whose floor does not fit its form: a `Ground` or `Cavern` realm lies on rock (a
    /// floor that is not air), a `Ceiling` or `Void` realm over air.
    FloorPolicy {
        /// The dimension.
        dimension: GenId,
        /// The realm.
        realm: GenId,
        /// Its form.
        form: RealmForm,
        /// Its floor's block name.
        floor: String,
    },
    /// A region map without its fallback rule: its last rule (the lowest priority) must have no
    /// condition and a `Cell` scope, so every lattice point gets a kind.
    NoFallback {
        /// The realm.
        realm: GenId,
        /// Its last rule, `None` if it has none.
        last: Option<GenId>,
    },
    /// A selector rule with a wrong number of entries: a `Point` rule needs exactly one, a `Cell`
    /// rule at least one.
    RuleEntries {
        /// The rule.
        rule: GenId,
        /// Whether its scope is `Point`.
        point: bool,
        /// The kinds of its entries (its own and the added ones).
        kinds: Vec<GenId>,
    },
    /// A selector entry whose weight is not in `1..=64`.
    EntryWeight {
        /// The rule.
        rule: GenId,
        /// The entry's kind.
        kind: GenId,
        /// Its weight.
        weight: u8,
    },
    /// A kind listed twice in one selector rule (by the rule, or by an added entry).
    DuplicateEntry {
        /// The rule.
        rule: GenId,
        /// The kind.
        kind: GenId,
    },
    /// A `Cell` rule whose region cells are smaller than the region lattice or over the largest
    /// cellular cells.
    CellSize {
        /// The rule.
        rule: GenId,
        /// Its cells' `log2` size.
        shift: u32,
        /// The smallest size it may have: the lattice's `log2` spacing.
        lattice: u32,
        /// The largest.
        max: u32,
    },
    /// Kinds of a realm that can meet whose slope and border relief break the 3:1 cap: the
    /// steepest one's slope plus the largest border relief over 543 blocks (at the default
    /// lattice) is over 3 (ADR 0039 sec. 6). One kind: its own slope is over 3.
    KindSlope {
        /// The realm.
        realm: GenId,
        /// The one, two or three kinds.
        kinds: Vec<GenId>,
        /// The steepest one's slope over one block, its jitter included, in 1/256 block per
        /// block, Q16 (the raw value of a `Q16`).
        slope: u64,
        /// The largest border relief between them, in 1/256 block.
        relief: u64,
        /// The lattice's `log2` spacing (the blend band is 3 lattice cells wide).
        lattice: u32,
    },
    /// A kind whose form is not built yet.
    KindFormNotBuilt {
        /// The kind.
        kind: GenId,
        /// Its form.
        form: KindForm,
    },
    /// A kind whose relief within a blend band is not inside its relief.
    KindLimits {
        /// The kind.
        kind: GenId,
    },
    /// Additive shape modifiers whose slopes, summed (ADR 0039 sec. 6 and 12), break the 3:1 cap
    /// alone (`kinds` empty) or on top of kinds they cut: the kinds' steepest slope plus the
    /// modifiers' plus the largest border relief over 543 blocks (at the default lattice) is
    /// over 3.
    ShapeSlope {
        /// The realm.
        realm: GenId,
        /// Every additive shape modifier of the realm.
        modifiers: Vec<GenId>,
        /// The one, two or three kinds, none for the modifiers alone.
        kinds: Vec<GenId>,
        /// The kinds' steepest slope over one block, its jitter included, in 1/256 block per
        /// block, Q16 (0 without kinds).
        slope: u64,
        /// The modifiers' slopes summed, the same way.
        added: u64,
        /// The largest border relief between the kinds, in 1/256 block.
        relief: u64,
        /// The lattice's `log2` spacing.
        lattice: u32,
    },
    /// A capped shape modifier ([`SlopeMode::Capped`](crate::region::SlopeMode::Capped)) whose
    /// cap, plus the additive slopes of the modifiers that run after it, is over 3 (ADR 0039
    /// sec. 6 and 12).
    ShapeCap {
        /// The realm.
        realm: GenId,
        /// The capped modifier.
        modifier: GenId,
        /// Its cap over one block, its jitter included, in 1/256 block per block, Q16.
        cap: u64,
        /// The additive modifiers after it, their slopes summed the same way.
        after: Vec<GenId>,
        /// Their slopes summed.
        added: u64,
    },
    /// A carver whose apron is wider than [`MAX_APRON`](crate::region::MAX_APRON).
    CarverApron {
        /// The carver.
        carver: GenId,
        /// Its apron, in blocks.
        apron: u32,
    },
    /// A check the engine does not define (the game's own), with its ids and message.
    Other {
        /// The ids the problem involves.
        ids: Vec<GenId>,
        /// What is wrong; it should name the ids.
        message: String,
    },
}

impl Problem {
    /// Every id the problem involves.
    pub fn ids(&self) -> Vec<&GenId> {
        match self {
            Self::Duplicate { id, .. } => vec![id],
            Self::DuplicateRegistry { .. } => Vec::new(),
            Self::Unknown { id, by, .. } => std::iter::once(id).chain(by).collect(),
            Self::UnknownBlock { by, .. } => vec![by],
            Self::Params {
                dimension, kind, ..
            } => vec![dimension, kind],
            Self::EmptySpan {
                dimension, realm, ..
            }
            | Self::OffPlane {
                dimension, realm, ..
            }
            | Self::PlaneOutsideWorld {
                dimension, realm, ..
            }
            | Self::FormNotBuilt {
                dimension, realm, ..
            }
            | Self::FloorPolicy {
                dimension, realm, ..
            } => vec![dimension, realm],
            Self::NoFallback { realm, last } => std::iter::once(realm).chain(last).collect(),
            Self::RuleEntries { rule, kinds, .. } => std::iter::once(rule).chain(kinds).collect(),
            Self::EntryWeight { rule, kind, .. } | Self::DuplicateEntry { rule, kind } => {
                vec![rule, kind]
            }
            Self::CellSize { rule, .. } => vec![rule],
            Self::KindSlope { realm, kinds, .. } => std::iter::once(realm).chain(kinds).collect(),
            Self::KindFormNotBuilt { kind, .. } | Self::KindLimits { kind } => vec![kind],
            Self::ShapeSlope {
                realm,
                modifiers,
                kinds,
                ..
            } => std::iter::once(realm)
                .chain(modifiers)
                .chain(kinds)
                .collect(),
            Self::ShapeCap {
                realm,
                modifier,
                after,
                ..
            } => [realm, modifier].into_iter().chain(after).collect(),
            Self::CarverApron { carver, .. } => vec![carver],
            Self::Overlap { dimension, a, b } => vec![dimension, a, b],
            Self::Gap {
                dimension,
                below,
                above,
                ..
            } => std::iter::once(dimension)
                .chain(below)
                .chain(above)
                .collect(),
            Self::TooManyPlanes { dimension, .. } => vec![dimension],
            Self::DrawRealms { dimension, draw } => {
                std::iter::once(dimension).chain(draw).collect()
            }
            Self::Other { ids, .. } => ids.iter().collect(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate {
                registry,
                id,
                count,
            } => write!(f, "{registry} {id} is registered {count} times"),
            Self::DuplicateRegistry { registry } => {
                write!(f, "more than one registry is named {registry}")
            }
            Self::Unknown {
                registry,
                id,
                by: Some(by),
            } => write!(f, "{by} names {registry} {id}, which is not registered"),
            Self::Unknown {
                registry,
                id,
                by: None,
            } => write!(f, "{registry} {id} is not registered"),
            Self::UnknownBlock { by, name } => write!(
                f,
                "{by} names the block {name:?}, which is not in the block registry"
            ),
            Self::Params {
                dimension,
                kind,
                expected,
            } => write!(
                f,
                "the generator spec of dimension {dimension} gives generator kind {kind} \
                 parameters that are not its {expected}"
            ),
            Self::EmptySpan {
                dimension,
                realm,
                span,
            } => write!(
                f,
                "realm {realm} of dimension {dimension} has an empty span {span}"
            ),
            Self::OffPlane {
                dimension,
                realm,
                y,
            } => write!(
                f,
                "realm {realm} of dimension {dimension} has a plane at y {y}, which is not a \
                 multiple of {REALM_PLANE}"
            ),
            Self::PlaneOutsideWorld {
                dimension,
                realm,
                y,
            } => write!(
                f,
                "realm {realm} of dimension {dimension} has a plane at y {y}, outside the world \
                 (-{MAX_BLOCK_COORD} to {MAX_BLOCK_COORD})"
            ),
            Self::Gap {
                dimension,
                span,
                below,
                above,
            } => {
                write!(f, "dimension {dimension} has no realm for y in {span}")?;
                match (below, above) {
                    (Some(b), Some(a)) => write!(f, ", between realms {b} and {a}"),
                    (Some(b), None) => write!(f, ", over realm {b}"),
                    (None, Some(a)) => write!(f, ", under realm {a}"),
                    (None, None) => Ok(()),
                }
            }
            Self::Overlap { dimension, a, b } if a == b => {
                write!(f, "dimension {dimension} lists realm {a} twice")
            }
            Self::Overlap { dimension, a, b } => {
                write!(f, "realms {a} and {b} of dimension {dimension} overlap")
            }
            Self::TooManyPlanes { dimension, planes } => write!(
                f,
                "dimension {dimension} has {} realm planes {planes:?}, at most {MAX_REALM_PLANES}",
                planes.len()
            ),
            Self::DrawRealms { dimension, draw } if draw.is_empty() => write!(
                f,
                "dimension {dimension} has no realm the far LOD draws (lod Draw), it needs one"
            ),
            Self::DrawRealms { dimension, draw } => {
                write!(
                    f,
                    "dimension {dimension} has {} realms the far LOD draws (",
                    draw.len()
                )?;
                for (i, realm) in draw.iter().enumerate() {
                    let sep = if i == 0 { "" } else { ", " };
                    write!(f, "{sep}{realm}")?;
                }
                f.write_str("), it needs exactly one")
            }
            Self::FormNotBuilt {
                dimension,
                realm,
                form,
            } => write!(
                f,
                "realm {realm} of dimension {dimension} has the form {form}, which is not built \
                 yet"
            ),
            Self::FloorPolicy {
                dimension,
                realm,
                form,
                floor,
            } => {
                let needs = if matches!(form, RealmForm::Ceiling | RealmForm::Void) {
                    "air"
                } else {
                    "a floor that is not air"
                };
                write!(
                    f,
                    "realm {realm} of dimension {dimension} has the form {form} and the floor \
                     {floor}; its form needs {needs}"
                )
            }
            Self::NoFallback { realm, last: None } => write!(
                f,
                "the region map of realm {realm} has no rule; it needs a fallback rule (no \
                 condition, Cell scope) last"
            ),
            Self::NoFallback {
                realm,
                last: Some(last),
            } => write!(
                f,
                "the region map of realm {realm} has no fallback rule: its last rule {last} has a \
                 condition or a Point scope"
            ),
            Self::RuleEntries { rule, point, kinds } => {
                write!(f, "selector rule {rule} has {} entries (", kinds.len())?;
                list(f, kinds)?;
                if *point {
                    f.write_str("); a Point rule needs exactly one")
                } else {
                    f.write_str("); a Cell rule needs at least one")
                }
            }
            Self::EntryWeight { rule, kind, weight } => write!(
                f,
                "selector rule {rule} gives kind {kind} the weight {weight}, outside 1 to {}",
                crate::region::MAX_WEIGHT
            ),
            Self::DuplicateEntry { rule, kind } => {
                write!(f, "selector rule {rule} lists kind {kind} more than once")
            }
            Self::CellSize {
                rule,
                shift,
                lattice,
                max,
            } => write!(
                f,
                "selector rule {rule} has region cells of 2^{shift} blocks, outside 2^{lattice} \
                 (the lattice) to 2^{max}"
            ),
            Self::KindSlope {
                realm,
                kinds,
                slope,
                relief: 0,
                ..
            } if kinds.len() == 1 => write!(
                f,
                "terrain kind {} of realm {realm} has a slope of {:.3}, over {MAX_SLOPE}",
                kinds[0],
                // Display only: the check itself is exact.
                *slope as f64 / 65536.0 / 256.0
            ),
            Self::KindSlope {
                realm,
                kinds,
                slope,
                relief,
                lattice,
            } => {
                f.write_str("kinds ")?;
                list(f, kinds)?;
                // Display only: the check itself is exact.
                let slope = *slope as f64 / 65536.0 / 256.0;
                let relief = *relief as f64 / 256.0;
                let band = 3.0 * f64::from(1u32 << lattice) / std::f64::consts::SQRT_2;
                write!(
                    f,
                    " of realm {realm} can meet with a slope of {:.3} (the steepest {slope:.3} \
                     plus a border relief of {relief:.1} blocks over {band:.0}), over \
                     {MAX_SLOPE}",
                    slope + relief / band
                )
            }
            Self::KindFormNotBuilt { kind, form } => write!(
                f,
                "terrain kind {kind} has the form {form}, which is not built yet"
            ),
            Self::KindLimits { kind } => write!(
                f,
                "terrain kind {kind} declares a relief within the blend band outside its relief"
            ),
            Self::ShapeSlope {
                realm,
                modifiers,
                kinds,
                slope,
                added,
                relief,
                lattice,
            } => {
                f.write_str("shape modifiers ")?;
                list(f, modifiers)?;
                // Display only: the check itself is exact.
                let q = |v: u64| v as f64 / 65536.0 / 256.0;
                if kinds.is_empty() {
                    return write!(
                        f,
                        " of realm {realm} add a slope of {:.3}, over {MAX_SLOPE}",
                        q(*added)
                    );
                }
                f.write_str(" on kinds ")?;
                list(f, kinds)?;
                let relief = *relief as f64 / 256.0;
                let band = 3.0 * f64::from(1u32 << lattice) / std::f64::consts::SQRT_2;
                write!(
                    f,
                    " of realm {realm} give a slope of {:.3} (the kinds' steepest {:.3} plus the \
                     modifiers' {:.3} plus a border relief of {relief:.1} blocks over {band:.0}), \
                     over {MAX_SLOPE}",
                    q(*slope) + q(*added) + relief / band,
                    q(*slope),
                    q(*added),
                )
            }
            Self::ShapeCap {
                realm,
                modifier,
                cap,
                after,
                added,
            } => {
                // Display only: the check itself is exact.
                let q = |v: u64| v as f64 / 65536.0 / 256.0;
                write!(
                    f,
                    "shape modifier {modifier} of realm {realm} caps its heights' slope at {:.3}",
                    q(*cap)
                )?;
                if !after.is_empty() {
                    f.write_str(", and the additive ")?;
                    list(f, after)?;
                    write!(f, " after it add {:.3}", q(*added))?;
                }
                write!(f, ": over {MAX_SLOPE}")
            }
            Self::CarverApron { carver, apron } => write!(
                f,
                "carver {carver} declares an apron of {apron} blocks, over {} (one chunk)",
                crate::region::MAX_APRON
            ),
            Self::Other { message, .. } => f.write_str(message),
        }
    }
}

/// `a, b and c`.
fn list(f: &mut fmt::Formatter<'_>, ids: &[GenId]) -> fmt::Result {
    for (i, id) in ids.iter().enumerate() {
        let sep = match i {
            0 => "",
            _ if i + 1 == ids.len() => " and ",
            _ => ", ",
        };
        write!(f, "{sep}{id}")?;
    }
    Ok(())
}

/// Every problem the checks found (never none): the one error of a failed freeze or world open.
/// Its message lists them all, one per line, each with its ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreezeError {
    problems: Vec<Problem>,
}

impl FreezeError {
    /// The error of `problems`. Panics if there is none.
    pub fn new(problems: Vec<Problem>) -> Self {
        assert!(!problems.is_empty(), "a FreezeError needs a problem");
        Self { problems }
    }

    /// The problems, in the order the checks found them.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// The problems, owned.
    pub fn into_problems(self) -> Vec<Problem> {
        self.problems
    }
}

impl fmt::Display for FreezeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let [problem] = self.problems.as_slice() {
            return write!(f, "world generation check failed: {problem}");
        }
        write!(
            f,
            "world generation checks failed with {} problems:",
            self.problems.len()
        )?;
        for problem in &self.problems {
            write!(f, "\n  {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for FreezeError {}

/// Gathers the problems of several checks, so one error reports them all: at app build, the
/// freeze of every registry ([`Checks::freeze`]); at world open, everything the preset resolves
/// to ([`Checks::resolve`], [`crate::realm::check_realms`], and the checks of the later steps).
#[derive(Clone, Debug, Default)]
pub struct Checks {
    problems: Vec<Problem>,
    /// Every problem pushed so far, a repeated one counted again ([`Checks::found`]).
    found: usize,
    /// The names of the registries frozen so far.
    registries: Vec<&'static str>,
}

impl Checks {
    /// No problem yet.
    pub const fn new() -> Self {
        Self {
            problems: Vec::new(),
            found: 0,
            registries: Vec::new(),
        }
    }

    /// Records `problem`, once: a problem two checks find (a realm's form, checked by its stack
    /// and by its generator) is reported once.
    pub fn push(&mut self, problem: Problem) {
        self.found += 1;
        if !self.problems.contains(&problem) {
            self.problems.push(problem);
        }
    }

    /// How many problems the checks found so far, a repeated one counted again: a builder that
    /// compares it before and after its checks knows whether they failed, even when a problem
    /// was already reported.
    pub const fn found(&self) -> usize {
        self.found
    }

    /// Records every problem of `error` ([`Checks::push`]).
    pub fn extend(&mut self, error: FreezeError) {
        for problem in error.problems {
            self.push(problem);
        }
    }

    /// Freezes `builder` ([`GenRegistryBuilder::freeze`]): the registry, or `None` with its
    /// problems recorded. A name another registry was frozen with here is a
    /// [`Problem::DuplicateRegistry`]: digest lines are keyed by the registry's name, so two
    /// registries with one name would merge their lines and hide a change.
    pub fn freeze<T: GenEntry>(
        &mut self,
        builder: GenRegistryBuilder<T>,
    ) -> Option<GenRegistry<T>> {
        let registry = builder.name();
        if self.registries.contains(&registry) {
            self.push(Problem::DuplicateRegistry { registry });
        } else {
            self.registries.push(registry);
        }
        builder.freeze().map_err(|e| self.extend(e)).ok()
    }

    /// The index of `id` in `registry`, or `None` with a [`Problem::Unknown`] recorded that
    /// says `by` names it. `by` is `None` for the root of a world open, which no entry names
    /// (its preset).
    pub fn resolve<T>(
        &mut self,
        registry: &GenRegistry<T>,
        id: &GenId,
        by: Option<&GenId>,
    ) -> Option<GenIx<T>> {
        let ix = registry.find(id.as_str());
        if ix.is_none() {
            self.push(Problem::Unknown {
                registry: registry.name(),
                id: id.clone(),
                by: by.cloned(),
            });
        }
        ix
    }

    /// Whether `blocks` holds the block `name` as generation looks it up ([`crate::block`]: a
    /// block name, its default state), with a [`Problem::UnknownBlock`] recorded that says `by`
    /// names it when it does not.
    pub fn block(&mut self, blocks: &BlockRegistry, name: &str, by: &GenId) -> bool {
        let known = blocks.by_name(name).is_some();
        if !known {
            self.push(Problem::UnknownBlock {
                by: by.clone(),
                name: name.to_owned(),
            });
        }
        known
    }

    /// The problems so far.
    pub fn problems(&self) -> &[Problem] {
        &self.problems
    }

    /// Whether no check failed so far.
    pub fn is_empty(&self) -> bool {
        self.problems.is_empty()
    }

    /// `Ok` if no check failed, else every problem in one [`FreezeError`].
    pub fn finish(self) -> Result<(), FreezeError> {
        if self.problems.is_empty() {
            Ok(())
        } else {
            Err(FreezeError::new(self.problems))
        }
    }
}
