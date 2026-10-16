//! The freeze checks' framework (ADR 0039 sec. 12): the problems a check can find
//! ([`Problem`]), a collector that gathers them all ([`Checks`]) and the one error that reports
//! them ([`FreezeError`]).

use super::{GenEntry, GenId, GenIx, GenRegistry, GenRegistryBuilder};
use crate::realm::{MAX_REALM_PLANES, REALM_PLANE, RealmForm, RealmSpan};
use boxel_voxel::MAX_BLOCK_COORD;
use std::fmt;

/// One problem a freeze check found, with the ids it involves. Its message names them, so a
/// refused mix of mods says which entries clash.
///
/// The later steps add their checks here (WG-REGION: a `Point` selector rule with more than one
/// entry, a region map without its fallback rule, the slope and border relief of kinds that can
/// meet, wrappers that target missing slots...). [`Problem::Other`] carries the game's own.
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
    /// An id that its registry does not hold, named by another entry.
    Unknown {
        /// The registry it was looked up in.
        registry: &'static str,
        /// The id.
        id: GenId,
        /// The entry that names it (a preset, a dimension, a rule...).
        by: GenId,
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
            Self::Unknown { id, by, .. } => vec![id, by],
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
            } => vec![dimension, realm],
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
            Self::Unknown { registry, id, by } => {
                write!(f, "{by} names {registry} {id}, which is not registered")
            }
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
            Self::Other { message, .. } => f.write_str(message),
        }
    }
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
    /// The names of the registries frozen so far.
    registries: Vec<&'static str>,
}

impl Checks {
    /// No problem yet.
    pub const fn new() -> Self {
        Self {
            problems: Vec::new(),
            registries: Vec::new(),
        }
    }

    /// Records `problem`.
    pub fn push(&mut self, problem: Problem) {
        self.problems.push(problem);
    }

    /// Records every problem of `error`.
    pub fn extend(&mut self, error: FreezeError) {
        self.problems.extend(error.problems);
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
    /// says `by` names it.
    pub fn resolve<T>(
        &mut self,
        registry: &GenRegistry<T>,
        id: &GenId,
        by: &GenId,
    ) -> Option<GenIx<T>> {
        let ix = registry.find(id.as_str());
        if ix.is_none() {
            self.push(Problem::Unknown {
                registry: registry.name(),
                id: id.clone(),
                by: by.clone(),
            });
        }
        ix
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
