//! The build counter ([`BuildKey`]) and the per-worker memo it keys ([`BuildMemo`]), ADR 0039
//! sec. 13.

use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

/// The next key [`BuildKey::next`] hands out; 0 is never one.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// The key of one built generator (a region generator, or a realm stack): a process-wide
/// counter, taken once when the generator is built, never per call.
///
/// The rule for memos: every per-worker memo (sites, lattice points, kinds, plate edges, LOD
/// corners) keeps the key of the generator that filled it, and a call that brings another key
/// resets it first ([`BuildMemo`]). The game builds a fresh generator each time a world opens,
/// so opening world A, closing it and opening world B on the same workers never reads A's
/// memo, even with the same seed and content. Clones of one generator share its key: they give
/// the same output, so they may share a memo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuildKey(NonZeroU64);

impl BuildKey {
    /// A key no other call in this process got (keys only grow; `u64` never runs out).
    pub fn next() -> Self {
        let key = NEXT.fetch_add(1, Ordering::Relaxed);
        Self(NonZeroU64::new(key).expect("the build counter wrapped"))
    }

    /// The key as a number, never 0.
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// A per-worker memo with the [`BuildKey`] of the generator that filled it: the rule of
/// [`BuildKey`] in one place. A worker keeps one per generator slot (a thread local, or a field
/// of its scratch) and asks it for the current generator's key on every call.
#[derive(Clone, Debug, Default)]
pub struct BuildMemo<T> {
    key: Option<BuildKey>,
    memo: T,
}

impl<T> BuildMemo<T> {
    /// A memo that no build filled yet: the first call resets it.
    pub const fn new(memo: T) -> Self {
        Self { key: None, memo }
    }

    /// The memo for the generator of `key`. If another build filled it (or none did), `reset`
    /// empties it first (a `clear` that keeps the allocations), and it is `key`'s from then on.
    pub fn get(&mut self, key: BuildKey, reset: impl FnOnce(&mut T)) -> &mut T {
        if self.key != Some(key) {
            reset(&mut self.memo);
            self.key = Some(key);
        }
        &mut self.memo
    }

    /// The key of the build that filled the memo, `None` before the first call.
    pub const fn key(&self) -> Option<BuildKey> {
        self.key
    }
}
