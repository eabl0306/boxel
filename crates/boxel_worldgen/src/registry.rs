//! Registries of world generation (ADR 0039 sec. 11 to 13): the ids every entry is known by
//! ([`GenId`]), the registries the app's plugins fill and freeze before any world opens
//! ([`GenRegistryBuilder`], [`GenRegistry`]), the salts that come from the ids, the digest of
//! what a preset reaches ([`RegistryDigest`]) with its diff against a saved one
//! ([`WorldgenDiff`]), the build counter that keys per-worker memos ([`BuildKey`]) and the
//! checks that make a bad mix of mods fail at load with its ids in the message ([`Checks`],
//! [`FreezeError`]).
//!
//! The rules that let a mod add an entry without moving any other:
//! - Order comes from the ids, never from registration or hash order. Freezing sorts the entries
//!   by `(phase, priority, id)` ([`Order`]; most registries have no order and sort by id alone)
//!   and gives them dense indices ([`GenIx`]) in that order, so plugins may register in any
//!   order.
//! - Randomness comes from the ids: an entry's salt is `splitmix64(fnv1a64(id bytes))`
//!   ([`GenId::salt`]) and its seed `mix_seed(seed, salt)` ([`GenId::seed`]), never `seed + salt`:
//!   an fbm gives octave `k` the seed `seed + k`, so an added salt could land on another entry's
//!   octave. Adding an entry shifts the indices after it, never another entry's salt or seed.
//! - What an entry does is in its digest line: its id, its [`GenEntry::version`] and a hash of
//!   its parameters ([`HashParams`]: a fixed byte encoding, the same in every run and on every
//!   platform).
//!
//! The checks run twice, at runtime and not only in CI: when the app is built,
//! [`GenRegistryBuilder::freeze`] refuses duplicates; when a world opens, the resolution of its
//! preset goes through [`Checks`] (unknown ids with [`Checks::resolve`], each dimension's realm
//! layout with [`crate::realm::check_realms`], and the checks the later steps add). Every
//! problem found is reported at once, in one [`FreezeError`] whose message names the ids.

use std::cmp::Reverse;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::ops::Index;

mod build;
mod check;
mod digest;
mod hash;
mod id;

pub use build::{BuildKey, BuildMemo};
pub use check::{Checks, FreezeError, Problem};
pub use digest::{Changed, DigestBuilder, DigestEntry, DigestScope, RegistryDigest, WorldgenDiff};
pub use hash::{HashParams, ParamsHasher, fnv1a64, hash_params};
pub use id::GenId;

/// Where an entry sorts before its id when its registry freezes: by phase (ascending), then by
/// priority (higher first), then by id.
///
/// Higher priority comes first everywhere: in a selector the first matching rule decides, so a
/// region map's fallback rule (the lowest priority, ADR 0039 sec. 4) comes last; in a decoration
/// phase it runs first. [`Order::NONE`] is the default, so the entries of a registry without
/// order sort by id alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Order {
    /// The phase (decoration stages: `FILL`, `SURFACE`, ...), ascending.
    pub phase: u16,
    /// The priority inside the phase, higher first.
    pub priority: i32,
}

impl Order {
    /// No order: phase 0, priority 0.
    pub const NONE: Self = Self::new(0, 0);

    /// Phase `phase`, priority `priority`.
    pub const fn new(phase: u16, priority: i32) -> Self {
        Self { phase, priority }
    }

    /// The part of the freeze order that comes before the id.
    fn sort_key(self) -> (u16, Reverse<i32>) {
        (self.phase, Reverse(self.priority))
    }
}

impl HashParams for Order {
    fn hash_params(&self, h: &mut ParamsHasher) {
        h.write(&self.phase).write(&self.priority);
    }
}

/// A value a [`GenRegistry`] holds: plain data, or a trait object over it (`Box<dyn Kind>` with
/// `trait Kind: GenEntry`). Its id is not part of it: plugins register `(id, entry)` pairs.
///
/// [`HashParams`] hashes its parameters: everything besides its code that changes its output,
/// so a changed parameter shows in the digest. A change of the code is a new
/// [`GenEntry::version`].
pub trait GenEntry: HashParams {
    /// The version of the entry's code: bumped when it gives other output for the same
    /// parameters (ADR 0039 sec. 11). The digest lists it.
    fn version(&self) -> u32;

    /// Where the entry sorts ([`Order`]); [`Order::NONE`] by default. The registry hashes it
    /// with the parameters, so a new priority shows in the digest.
    fn order(&self) -> Order {
        Order::NONE
    }
}

impl<T: GenEntry + ?Sized> GenEntry for Box<T> {
    fn version(&self) -> u32 {
        (**self).version()
    }
    fn order(&self) -> Order {
        (**self).order()
    }
}

impl<T: GenEntry + ?Sized> GenEntry for std::sync::Arc<T> {
    fn version(&self) -> u32 {
        (**self).version()
    }
    fn order(&self) -> Order {
        (**self).order()
    }
}

/// The dense runtime index of an entry in its frozen [`GenRegistry`]: its position in the freeze
/// order. Indices are of one registry and one run: they shift when an entry is added before
/// them, so nothing saved or seeded may use them (ids and salts are for that).
pub struct GenIx<T: ?Sized> {
    raw: u32,
    of: PhantomData<fn() -> T>,
}

impl<T: ?Sized> GenIx<T> {
    /// The index `raw`. It means something only for the registry it came from.
    pub const fn new(raw: u32) -> Self {
        Self {
            raw,
            of: PhantomData,
        }
    }

    /// The index, for side tables (a memo slot per entry).
    pub const fn index(self) -> usize {
        self.raw as usize
    }

    /// The index as stored: `u32`.
    pub const fn raw(self) -> u32 {
        self.raw
    }
}

// By hand: derives would ask `T` for the same traits.
impl<T: ?Sized> Clone for GenIx<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T: ?Sized> Copy for GenIx<T> {}
impl<T: ?Sized> PartialEq for GenIx<T> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}
impl<T: ?Sized> Eq for GenIx<T> {}
impl<T: ?Sized> PartialOrd for GenIx<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<T: ?Sized> Ord for GenIx<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.raw.cmp(&other.raw)
    }
}
impl<T: ?Sized> Hash for GenIx<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}
impl<T: ?Sized> fmt::Debug for GenIx<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "GenIx({})", self.raw)
    }
}

/// A registry while the app is built: plugins add `(id, entry)` pairs in any order, then
/// [`GenRegistryBuilder::freeze`] checks and sorts them (ADR 0034 sec. 1: before any world
/// opens).
#[derive(Clone, Debug)]
pub struct GenRegistryBuilder<T> {
    name: &'static str,
    entries: Vec<(GenId, T)>,
}

impl<T> GenRegistryBuilder<T> {
    /// An empty registry called `name` in messages and digest lines (`"realm"`,
    /// `"terrain_kind"`). Names are unique among the app's registries.
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            entries: Vec::new(),
        }
    }

    /// Adds `entry` as `id`. A duplicate id is not refused here but by
    /// [`GenRegistryBuilder::freeze`], which reports every duplicate of the registry at once.
    pub fn register(&mut self, id: GenId, entry: T) -> &mut Self {
        self.entries.push((id, entry));
        self
    }

    /// The registry's name.
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The number of registrations so far (duplicates included).
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing was registered.
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<T: GenEntry> GenRegistryBuilder<T> {
    /// Freezes the registry: sorts the entries by `(phase, priority, id)` ([`Order`]), gives
    /// them dense indices in that order and computes each one's salt and parameter hash. The
    /// result is the same whatever order the entries were registered in.
    ///
    /// Fails with a [`Problem::Duplicate`] for every id registered more than once.
    pub fn freeze(self) -> Result<GenRegistry<T>, FreezeError> {
        let Self { name, mut entries } = self;
        entries.sort_by_cached_key(|(id, entry)| (entry.order().sort_key(), id.clone()));
        let mut by_id: Vec<u32> = (0..entries.len())
            .map(|i| u32::try_from(i).expect("more than u32::MAX entries"))
            .collect();
        by_id.sort_by(|&a, &b| entries[a as usize].0.cmp(&entries[b as usize].0));
        let duplicates: Vec<Problem> = by_id
            .chunk_by(|&a, &b| entries[a as usize].0 == entries[b as usize].0)
            .filter(|run| run.len() > 1)
            .map(|run| Problem::Duplicate {
                registry: name,
                id: entries[run[0] as usize].0.clone(),
                count: run.len(),
            })
            .collect();
        if !duplicates.is_empty() {
            return Err(FreezeError::new(duplicates));
        }
        let salts = entries.iter().map(|(id, _)| id.salt()).collect();
        let versions = entries.iter().map(|(_, entry)| entry.version()).collect();
        let params = entries
            .iter()
            .map(|(_, entry)| {
                let mut h = ParamsHasher::new();
                h.write(&entry.order()).write(entry);
                h.finish()
            })
            .collect();
        let (ids, entries): (Vec<_>, Vec<_>) = entries.into_iter().unzip();
        Ok(GenRegistry {
            name,
            ids: ids.into(),
            entries: entries.into(),
            salts,
            versions,
            params,
            by_id: by_id.into(),
        })
    }
}

/// A frozen registry: its entries in freeze order ([`GenRegistryBuilder::freeze`]), each with
/// its id, dense index ([`GenIx`]), salt, version and parameter hash. It never changes while
/// the app runs.
#[derive(Clone, Debug)]
pub struct GenRegistry<T> {
    name: &'static str,
    ids: Box<[GenId]>,
    entries: Box<[T]>,
    salts: Box<[u64]>,
    versions: Box<[u32]>,
    params: Box<[u64]>,
    /// The indices sorted by id, for [`GenRegistry::find`].
    by_id: Box<[u32]>,
}

impl<T> GenRegistry<T> {
    /// The registry's name ([`GenRegistryBuilder::new`]).
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The number of entries.
    pub const fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether the registry has no entry.
    pub const fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The index of `id`, `None` if the registry lacks it (binary search).
    pub fn find(&self, id: &str) -> Option<GenIx<T>> {
        self.by_id
            .binary_search_by(|&i| self.ids[i as usize].as_str().cmp(id))
            .ok()
            .map(|at| GenIx::new(self.by_id[at]))
    }

    /// The entry at `ix`. Panics if `ix` is out of range (an index of another registry).
    pub fn get(&self, ix: GenIx<T>) -> &T {
        &self.entries[ix.index()]
    }

    /// The id of the entry at `ix`.
    pub fn id(&self, ix: GenIx<T>) -> &GenId {
        &self.ids[ix.index()]
    }

    /// The salt of the entry at `ix` ([`GenId::salt`]).
    pub fn salt(&self, ix: GenIx<T>) -> u64 {
        self.salts[ix.index()]
    }

    /// The seed of the entry at `ix` under `seed`: `mix_seed(seed, salt)` ([`GenId::seed`]).
    pub fn seed(&self, ix: GenIx<T>, seed: u64) -> u64 {
        crate::noise::mix_seed(seed, self.salt(ix))
    }

    /// The version of the entry at `ix` ([`GenEntry::version`]).
    pub fn version(&self, ix: GenIx<T>) -> u32 {
        self.versions[ix.index()]
    }

    /// The parameter hash of the entry at `ix`: its [`Order`] and its [`HashParams`].
    pub fn params_hash(&self, ix: GenIx<T>) -> u64 {
        self.params[ix.index()]
    }

    /// Every index, in freeze order.
    pub fn indices(&self) -> impl ExactSizeIterator<Item = GenIx<T>> + use<T> {
        (0..self.ids.len()).map(|i| GenIx::new(i as u32))
    }

    /// Every entry with its index and id, in freeze order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = (GenIx<T>, &GenId, &T)> {
        self.indices()
            .zip(self.ids.iter().zip(&self.entries))
            .map(|(ix, (id, e))| (ix, id, e))
    }
}

impl<T> Index<GenIx<T>> for GenRegistry<T> {
    type Output = T;
    fn index(&self, ix: GenIx<T>) -> &T {
        self.get(ix)
    }
}

#[cfg(test)]
mod tests;
