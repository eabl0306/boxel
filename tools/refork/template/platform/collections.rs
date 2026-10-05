//! `HashMap` / `HashSet` defaulting to the deterministic [`FixedHasher`],
//! as plain type aliases over `hashbrown` (no newtype wrapper).

use crate::platform::hash::FixedHasher;

/// `hashbrown::HashMap` with [`FixedHasher`] as the default hasher.
pub type HashMap<K, V, S = FixedHasher> = hashbrown::HashMap<K, V, S>;
/// `hashbrown::HashSet` with [`FixedHasher`] as the default hasher.
pub type HashSet<T, S = FixedHasher> = hashbrown::HashSet<T, S>;
/// `hashbrown::HashTable`.
pub use hashbrown::HashTable;

/// Mirror of `std::collections::hash_map`.
pub mod hash_map {
    pub use super::HashMap;
    pub use crate::platform::hash::{DefaultHasher, RandomState};
    pub use hashbrown::hash_map::{
        Drain, EntryRef, ExtractIf, IntoIter, IntoKeys, IntoValues, Iter, IterMut, Keys,
        OccupiedEntry, OccupiedError, RawEntryBuilder, RawEntryBuilderMut, RawEntryMut,
        RawOccupiedEntryMut, VacantEntry, Values, ValuesMut,
    };
    /// Entry with [`FixedHasher`](boxel_utils::platform::hash::FixedHasher) default.
    pub type Entry<'a, K, V, S = crate::platform::hash::FixedHasher> =
        hashbrown::hash_map::Entry<'a, K, V, S>;
}

/// Mirror of `std::collections::hash_set`.
pub mod hash_set {
    pub use super::HashSet;
    pub use hashbrown::hash_set::{
        Difference, Drain, Entry, ExtractIf, Intersection, IntoIter, Iter,
        OccupiedEntry, SymmetricDifference, Union, VacantEntry,
    };
}

/// Mirror of `hashbrown::hash_table`.
pub mod hash_table {
    pub use hashbrown::hash_table::*;
}
