//! [`WorldgenRecord`]: what a world saves of how it is generated (ADR 0039 sec. 17).

use crate::realm::RealmSpan;
use crate::registry::{DigestBuilder, DigestEntry, GenId, RegistryDigest, WorldgenDiff};
use boxel_voxel::DimensionId;

/// The most names a world's dimension table ([`WorldgenRecord::names`]) holds: one per
/// [`DimensionId`]. A saved table with more is damaged.
pub const MAX_DIMENSIONS: usize = 1 << 16;

/// What a world saves of how it is generated (ADR 0039 sec. 17), under the engine's meta key
/// `boxel:worldgen`, written once, when the world is created ([`Worldgen::open`](super::Worldgen::open)
/// gives it; the bytes are the store's): the preset, the dimension name table, and per
/// dimension its realms, its generator spec and its digest.
///
/// When the world opens again, [`WorldgenRecord::diff`] compares it with the live one. A world
/// without it (made before records) is a legacy world: it opens through the legacy factory of
/// the type its game saved, unchanged, and never gets one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorldgenRecord {
    /// The preset's digest line: its id, version and parameter hash.
    pub preset: DigestEntry,
    /// The dimension name table: dimension `n` (`DimensionId(n)` in chunk and save keys) is
    /// `names[n]`, at most [`MAX_DIMENSIONS`] names, each once. A dimension of the saved table
    /// keeps its number for good, also once the preset drops it, and one the preset adds later
    /// takes the next number. Unlike the block id map, the store saves the table only when the
    /// world is made (the record is never written again), so that later number is given anew at
    /// every open, not saved: nothing reads a number other than 0 until a second dimension, which
    /// changes the save format anyway (ADR 0039 sec. 1).
    pub names: Vec<GenId>,
    /// Each dimension of the preset, in its order.
    pub dimensions: Vec<DimensionRecord>,
}

/// A dimension of a [`WorldgenRecord`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DimensionRecord {
    /// The dimension's id (in the record's name table).
    pub id: GenId,
    /// Its realms, bottom first: id and span (the generator's
    /// [`realms`](crate::WorldGenerator::realms)).
    pub realms: Vec<(GenId, RealmSpan)>,
    /// Its generator spec.
    pub generator: SpecLine,
    /// Every line of its digest: its own (its definition, its spec, its realms' entries) and its
    /// generator's (a region generator's realm lines).
    pub digest: RegistryDigest,
}

/// A generator spec as recorded: the kind's id and version, and the hash of its parameters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SpecLine {
    /// The generator kind.
    pub kind: GenId,
    /// Its version.
    pub version: u32,
    /// The hash of the spec's parameters.
    pub params: u64,
}

impl WorldgenRecord {
    /// The number of `dimension` in the name table, `None` if the world never had it.
    pub fn number(&self, dimension: &str) -> Option<DimensionId> {
        let n = self.names.iter().position(|n| n.as_str() == dimension)?;
        u16::try_from(n).ok().map(DimensionId)
    }

    /// The record of `dimension`, `None` if the preset has no such dimension.
    pub fn dimension(&self, dimension: &str) -> Option<&DimensionRecord> {
        self.dimensions.iter().find(|d| d.id.as_str() == dimension)
    }

    /// Every line of the record: the preset's and each dimension's.
    pub fn digest(&self) -> RegistryDigest {
        let mut digest = DigestBuilder::new();
        digest.push(self.preset.clone());
        for dimension in &self.dimensions {
            digest.extend(&dimension.digest);
        }
        digest.finish()
    }

    /// What changed from this record (the saved one) to `live` (the one the world opens with
    /// now): every line added, removed or changed ([`WorldgenDiff::between`]). A changed realm
    /// span, preset or spec is a changed line too, since each is in a line's parameters.
    pub fn diff(&self, live: &Self) -> WorldgenDiff {
        WorldgenDiff::between(&self.digest(), &live.digest())
    }
}
