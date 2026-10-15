//! What a preset reaches, line by line ([`RegistryDigest`]), and what changed since a world was
//! saved ([`WorldgenDiff`]), ADR 0039 sec. 11 and 17.

use super::hash::{HashParams, ParamsHasher};
use super::{GenId, GenIx, GenRegistry};
use std::borrow::Cow;
use std::cmp::Ordering;
use std::fmt;

/// Where a digest line sits: under a realm of a dimension, under a dimension only (its generator
/// kind, the dimension itself), or under the preset (both `None`: the preset's own line). An
/// entry two realms reach is listed in both, so removing it from one shows in that one.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DigestScope {
    /// The dimension, `None` for the preset's lines.
    pub dimension: Option<GenId>,
    /// The realm of the dimension, `None` for the dimension's own lines.
    pub realm: Option<GenId>,
}

impl DigestScope {
    /// The preset's own lines.
    pub const PRESET: Self = Self {
        dimension: None,
        realm: None,
    };

    /// The lines of `dimension` outside its realms.
    pub fn of_dimension(dimension: &GenId) -> Self {
        Self {
            dimension: Some(dimension.clone()),
            realm: None,
        }
    }

    /// The lines of `realm` in `dimension`.
    pub fn of_realm(dimension: &GenId, realm: &GenId) -> Self {
        Self {
            dimension: Some(dimension.clone()),
            realm: Some(realm.clone()),
        }
    }
}

impl fmt::Display for DigestScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (&self.dimension, &self.realm) {
            (None, None) => f.write_str("the preset"),
            (Some(d), None) => write!(f, "dimension {d}"),
            (Some(d), Some(r)) => write!(f, "realm {r} of dimension {d}"),
            (None, Some(r)) => write!(f, "realm {r}"),
        }
    }
}

impl HashParams for DigestScope {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.dimension).write(&self.realm);
    }
}

/// One line of a digest: an entry a preset reaches, where it is reached, and what it is.
///
/// Lines order by scope, registry and id (the key), then version and parameters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DigestEntry {
    /// Where the preset reaches the entry.
    pub scope: DigestScope,
    /// The name of the entry's registry ([`GenRegistry::name`]): an id is unique only inside
    /// its registry.
    pub registry: Cow<'static, str>,
    /// The entry's id.
    pub id: GenId,
    /// The entry's version ([`super::GenEntry::version`]).
    pub version: u32,
    /// The entry's parameter hash ([`GenRegistry::params_hash`]).
    pub params: u64,
}

impl DigestEntry {
    /// The line of entry `ix` of `registry`, reached in `scope`.
    pub fn of<T>(scope: &DigestScope, registry: &GenRegistry<T>, ix: GenIx<T>) -> Self {
        Self {
            scope: scope.clone(),
            registry: Cow::Borrowed(registry.name()),
            id: registry.id(ix).clone(),
            version: registry.version(ix),
            params: registry.params_hash(ix),
        }
    }

    /// What identifies the line: scope, registry and id.
    fn key(&self) -> (&DigestScope, &str, &GenId) {
        (&self.scope, &self.registry, &self.id)
    }
}

impl fmt::Display for DigestEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {} in {} (version {}, params {:#018x})",
            self.registry, self.id, self.scope, self.version, self.params
        )
    }
}

impl HashParams for DigestEntry {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.scope)
            .write(&*self.registry)
            .write(&self.id)
            .write(&self.version)
            .write(&self.params);
    }
}

/// The digest of everything a preset reaches (ADR 0039 sec. 11): the sorted list of its lines
/// ([`DigestEntry`]), one per scope, registry and id, and the 64-bit hash of that list.
///
/// The live digest comes from a [`DigestBuilder`] as the preset is resolved at world open; a
/// saved one from [`RegistryDigest::from_entries`] (the world's save record, WG-SAVE).
/// [`WorldgenDiff::between`] compares them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryDigest {
    entries: Vec<DigestEntry>,
    hash: u64,
}

impl RegistryDigest {
    /// The digest of `entries`, in any order. A key (scope, registry, id) is listed once: of
    /// lines with the same key, the first in line order stays (a damaged list, never one a
    /// [`DigestBuilder`] makes).
    pub fn from_entries(mut entries: Vec<DigestEntry>) -> Self {
        entries.sort();
        entries.dedup_by(|a, b| a.key() == b.key());
        let hash = {
            let mut h = ParamsHasher::new();
            h.write(&entries);
            h.finish()
        };
        Self { entries, hash }
    }

    /// The lines, sorted.
    pub fn entries(&self) -> &[DigestEntry] {
        &self.entries
    }

    /// The 64-bit hash of the lines (their [`HashParams`] encoding).
    pub const fn hash(&self) -> u64 {
        self.hash
    }

    /// The low 32 bits of [`RegistryDigest::hash`]: the version a generator built from these
    /// entries reports (`WorldGenerator::version`, WG-REGION). Take it from the digest of the
    /// generator's dimension ([`RegistryDigest::dimension`]), so a change in another dimension
    /// does not touch it.
    pub const fn version(&self) -> u32 {
        self.hash as u32
    }

    /// The digest of the lines of `dimension` alone (its realms' and its own), with its own
    /// hash.
    pub fn dimension(&self, dimension: &GenId) -> Self {
        let entries = self
            .entries
            .iter()
            .filter(|e| e.scope.dimension.as_ref() == Some(dimension))
            .cloned()
            .collect();
        Self::from_entries(entries)
    }
}

/// Collects the lines of a digest while a preset is resolved: every entry the preset reaches,
/// with where it reaches it. Adding a line twice keeps one.
#[derive(Clone, Debug, Default)]
pub struct DigestBuilder {
    entries: Vec<DigestEntry>,
}

impl DigestBuilder {
    /// No line yet.
    pub const fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Adds the line of entry `ix` of `registry`, reached in `scope`.
    pub fn add<T>(
        &mut self,
        scope: &DigestScope,
        registry: &GenRegistry<T>,
        ix: GenIx<T>,
    ) -> &mut Self {
        self.entries.push(DigestEntry::of(scope, registry, ix));
        self
    }

    /// The digest.
    pub fn finish(self) -> RegistryDigest {
        RegistryDigest::from_entries(self.entries)
    }
}

/// What changed between the digest a world was saved with and the live one (ADR 0039
/// sec. 17). The engine only reports it; the policy is the game's (boxelcraft: warn and go on).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorldgenDiff {
    /// Live lines the saved digest lacks: a mod or a realm added.
    pub added: Vec<DigestEntry>,
    /// Saved lines the live digest lacks: a mod removed.
    pub removed: Vec<DigestEntry>,
    /// Lines in both with another version or other parameters.
    pub changed: Vec<Changed>,
}

/// A line in both digests whose version or parameters differ.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changed {
    /// The line as saved.
    pub saved: DigestEntry,
    /// The line as it is now (same scope, registry and id).
    pub live: DigestEntry,
}

impl WorldgenDiff {
    /// The diff from `saved` to `live`, each list in line order. A pure function of the two
    /// lists: their hashes are not read.
    pub fn between(saved: &RegistryDigest, live: &RegistryDigest) -> Self {
        let mut diff = Self::default();
        let (mut s, mut l) = (
            saved.entries.iter().peekable(),
            live.entries.iter().peekable(),
        );
        loop {
            let order = match (s.peek(), l.peek()) {
                (None, None) => break,
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (Some(a), Some(b)) => a.key().cmp(&b.key()),
            };
            match order {
                Ordering::Less => diff.removed.extend(s.next().cloned()),
                Ordering::Greater => diff.added.extend(l.next().cloned()),
                Ordering::Equal => {
                    let (a, b) = (s.next().expect("peeked"), l.next().expect("peeked"));
                    if (a.version, a.params) != (b.version, b.params) {
                        diff.changed.push(Changed {
                            saved: a.clone(),
                            live: b.clone(),
                        });
                    }
                }
            }
        }
        diff
    }

    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// One line per change, for the game's warning.
impl fmt::Display for WorldgenDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_empty() {
            return f.write_str("world generation unchanged since the world was saved");
        }
        write!(
            f,
            "world generation changed since the world was saved: {} added, {} removed, {} changed",
            self.added.len(),
            self.removed.len(),
            self.changed.len()
        )?;
        for e in &self.added {
            write!(f, "\n  added {e}")?;
        }
        for e in &self.removed {
            write!(f, "\n  removed {e}")?;
        }
        for Changed { saved, live } in &self.changed {
            write!(
                f,
                "\n  changed {} {} in {}: version {} -> {}, params {:#018x} -> {:#018x}",
                live.registry,
                live.id,
                live.scope,
                saved.version,
                live.version,
                saved.params,
                live.params
            )?;
        }
        Ok(())
    }
}
