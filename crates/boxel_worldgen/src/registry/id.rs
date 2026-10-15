//! [`GenId`], the id of a world generation entry, with its salt and seed.

use super::hash::{HashParams, ParamsHasher, fnv1a64};
use crate::noise::{mix_seed, splitmix64};
use boxel_voxel::{RegistryError, validate_name};
use std::fmt;
use std::sync::Arc;

/// The id of a world generation entry (ADR 0039 sec. 11): a namespaced name with the rule of
/// block names ([`validate_name`]), such as `game:continental`, `game:land_cells` or
/// `game:overworld/surface`. An id is unique inside its registry; two registries may share one
/// (a realm and its environment both `game:nether`).
///
/// Ids order by their bytes, the same on every platform, and that is the order a registry
/// freezes in. Cloning is cheap: the name is shared.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenId(Arc<str>);

impl GenId {
    /// The id `name`. Fails with [`RegistryError::InvalidName`] if `name` breaks the rule.
    pub fn new(name: &str) -> Result<Self, RegistryError> {
        validate_name(name)?;
        Ok(Self(name.into()))
    }

    /// The name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The salt of this id: `splitmix64(fnv1a64(name bytes))`. The name alone fixes it, so it is
    /// the same in every run and on every platform, whatever else is registered.
    pub fn salt(&self) -> u64 {
        splitmix64(fnv1a64(self.0.as_bytes()))
    }

    /// The seed of this entry under `seed` (a world's, or a realm's): `mix_seed(seed, salt)`.
    /// Mixed, never added: an fbm gives octave `k` the seed `seed + k`, so `seed + salt` could
    /// reach another entry's octaves.
    pub fn seed(&self, seed: u64) -> u64 {
        mix_seed(seed, self.salt())
    }
}

impl fmt::Display for GenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for GenId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// As its name ([`HashParams`] for `str`).
impl HashParams for GenId {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(self.as_str());
    }
}
