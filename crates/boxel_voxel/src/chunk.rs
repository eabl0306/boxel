use crate::{
    BlockRegistry, BlockState, CHUNK_AREA, CHUNK_VOLUME, LightMasks, LightTables, LocalPos,
    Occupancy, OwnedLightMasks, Paletted,
};
use std::num::NonZeroU64;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// One chunk's voxel payload. Shared as `Arc<ChunkData>` snapshots; edit through
/// `Arc::make_mut` (copy-on-write when a job still holds the old snapshot).
///
/// Light is not stored here: it is derived data (ADR 0015). The chunk may cache its light masks
/// ([`ChunkData::light_masks`]): built on first use when the storage cannot serve them as is,
/// shared by every reader of the snapshot, dropped by every change of the blocks (and not
/// carried by `clone`). It also caches the answer of [`ChunkData::may_contain`] for one
/// [`StateSet`], dropped the same way.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkData {
    /// Block states (private so that every change goes through a method that resets the
    /// caches: [`ChunkData::set`], [`ChunkData::blocks_mut`], [`ChunkData::compact`]).
    blocks: Paletted,
    /// Bumped by every mutation (mesh results built from an older version are discarded).
    pub version: u32,
    /// Lazily built light masks of `blocks` (only for storage that cannot serve them as is).
    masks: MaskCache,
    /// The last answer of [`ChunkData::may_contain`] and the set it was for.
    contains: ContainsCache,
}

/// A set of block states that a chunk can be asked about ([`ChunkData::may_contain`]: "may you
/// hold one of these?"), with a process-wide unique key under which chunks cache their answer.
/// Each [`StateSet::new`] gets its own key; a clone keeps it (it is the same set). Equality
/// compares the states only.
#[derive(Clone, Debug)]
pub struct StateSet {
    /// Bit `s % 64` of word `s / 64` for each state `s` of the set (no words past the last one).
    words: Box<[u64]>,
    /// The cache key: unique per [`StateSet::new`], below `2^63`.
    key: NonZeroU64,
}

impl StateSet {
    /// The set of `states`, with a fresh cache key.
    pub fn new(states: impl IntoIterator<Item = BlockState>) -> Self {
        static NEXT_KEY: AtomicU64 = AtomicU64::new(1);
        let mut words: Vec<u64> = Vec::new();
        for s in states {
            let word = usize::from(s.0) / 64;
            if words.len() <= word {
                words.resize(word + 1, 0);
            }
            words[word] |= 1 << (s.0 % 64);
        }
        let key = NEXT_KEY.fetch_add(1, Ordering::Relaxed);
        assert!(key < 1 << 63, "StateSet keys ran out");
        Self {
            words: words.into_boxed_slice(),
            key: NonZeroU64::new(key).expect("keys start at 1"),
        }
    }

    /// `s` is in the set.
    #[inline]
    pub fn contains(&self, s: BlockState) -> bool {
        self.words
            .get(usize::from(s.0) / 64)
            .is_some_and(|w| w & (1 << (s.0 % 64)) != 0)
    }

    /// No state at all.
    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// The cache key: equal keys mean the same set (a clone), so a caller may keep answers
    /// about it under the key.
    #[inline]
    pub fn key(&self) -> NonZeroU64 {
        self.key
    }
}

impl Default for StateSet {
    /// The empty set.
    fn default() -> Self {
        Self::new([])
    }
}

impl PartialEq for StateSet {
    fn eq(&self, other: &Self) -> bool {
        self.words == other.words
    }
}

/// The cached answer of [`ChunkData::may_contain`]: `key << 1 | answer` of the last set asked
/// about, 0 when there is none. Atomic so readers of a shared snapshot can fill it; a clone
/// keeps it (same blocks) and equality ignores it.
#[derive(Default)]
struct ContainsCache(AtomicU64);

impl Clone for ContainsCache {
    fn clone(&self) -> Self {
        Self(AtomicU64::new(self.0.load(Ordering::Relaxed)))
    }
}

impl PartialEq for ContainsCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for ContainsCache {}

impl std::fmt::Debug for ContainsCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.load(Ordering::Relaxed) {
            0 => f.write_str("empty"),
            v => write!(f, "key {}: {}", v >> 1, v & 1 != 0),
        }
    }
}

impl ContainsCache {
    /// Forgets the answer (the blocks changed).
    fn clear(&mut self) {
        *self.0.get_mut() = 0;
    }
}

/// The cached light masks of a chunk. A clone starts empty and equality ignores it: it is a
/// pure function of the blocks.
#[derive(Default)]
struct MaskCache(OnceLock<Box<OwnedLightMasks>>);

impl Clone for MaskCache {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl PartialEq for MaskCache {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for MaskCache {}

impl std::fmt::Debug for MaskCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = if self.0.get().is_some() {
            "cached"
        } else {
            "empty"
        };
        f.write_str(state)
    }
}

impl ChunkData {
    /// A chunk filled with `state`, version 0.
    pub fn uniform(state: BlockState) -> Self {
        Self::from_paletted(Paletted::Uniform(state))
    }
    /// A chunk with this block storage as is (no re-encoding), version 0.
    pub fn from_paletted(blocks: Paletted) -> Self {
        Self {
            blocks,
            version: 0,
            masks: MaskCache::default(),
            contains: ContainsCache::default(),
        }
    }
    /// The block storage.
    #[inline]
    pub fn blocks(&self) -> &Paletted {
        &self.blocks
    }
    /// The block storage, to change it in place. Drops the caches; does not bump `version`
    /// (use [`ChunkData::set`] for edits).
    pub fn blocks_mut(&mut self) -> &mut Paletted {
        self.masks.0.take();
        self.contains.clear();
        &mut self.blocks
    }
    /// Compacts the block storage ([`Paletted::compact`]). The voxels stay the same, so the
    /// version and the mask cache do too; the palette may shrink, so the answer of
    /// [`ChunkData::may_contain`] is dropped.
    pub fn compact(&mut self) {
        self.blocks.compact();
        self.contains.clear();
    }
    /// State at `p`.
    #[inline]
    pub fn get(&self, p: LocalPos) -> BlockState {
        self.blocks.get(p)
    }
    /// `Some(state)` if uniform.
    pub fn as_uniform(&self) -> Option<BlockState> {
        self.blocks.as_uniform()
    }
    /// Sets `p`, bumps `version` (wrapping) if the state changed, returns the previous state.
    /// A change drops the caches.
    ///
    /// Frozen behavior: `get`, `version` and the return value. The representation follows
    /// [`Paletted::set`] (the palette grows in place; nothing shrinks until
    /// [`Paletted::compact`]).
    pub fn set(&mut self, p: LocalPos, state: BlockState) -> BlockState {
        let previous = self.blocks.set(p, state);
        if previous != state {
            self.version = self.version.wrapping_add(1);
            self.masks.0.take();
            self.contains.clear();
        }
        previous
    }
    /// From a dense array (`len == CHUNK_VOLUME`), version 0.
    ///
    /// Frozen behavior: `get` returns `states[i]`; all-equal input gives `Uniform`. The
    /// representation is [`Paletted::encode`]'s (the smallest one).
    pub fn from_dense(states: &[BlockState]) -> Self {
        assert_eq!(
            states.len(),
            CHUNK_VOLUME,
            "from_dense needs CHUNK_VOLUME states"
        );
        Self::from_paletted(Paletted::encode(states))
    }
    /// From a 1-bit mask, version 0: `set` where the bit of `occupancy` is 1, `clear` where it
    /// is 0 (bit `z` of word `y * 32 + x`, the layout of [`ChunkData::opaque_occupancy`]).
    ///
    /// Equal to [`ChunkData::from_dense`] of the expanded states, representation included:
    /// `Uniform` when every voxel gets the same state (all bits equal, or `set == clear`),
    /// otherwise `Packed` with 1 bit per voxel and the palette in first-seen order. The mask
    /// becomes the packed words directly (word `m` is masks `2m` and `2m + 1`), with no dense
    /// array and no palette scan.
    pub fn from_occupancy(occupancy: &Occupancy, set: BlockState, clear: BlockState) -> Self {
        let all = occupancy.iter().fold(u32::MAX, |acc, &w| acc & w);
        let any = occupancy.iter().fold(0, |acc, &w| acc | w);
        let blocks = if set == clear || all == u32::MAX {
            Paletted::Uniform(set)
        } else if any == 0 {
            Paletted::Uniform(clear)
        } else {
            // Palette in first-seen order: voxel 0 gets index 0.
            let (palette, flip) = if occupancy[0] & 1 == 1 {
                (vec![set, clear], u64::MAX)
            } else {
                (vec![clear, set], 0)
            };
            let words = occupancy
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&[low, high]| (u64::from(low) | (u64::from(high) << 32)) ^ flip)
                .collect();
            Paletted::Packed {
                palette,
                bits: 1,
                words,
            }
        };
        Self::from_paletted(blocks)
    }
    /// Decodes into `out` (`len == CHUNK_VOLUME`).
    pub fn to_dense(&self, out: &mut [BlockState]) {
        self.blocks.decode_into(out);
    }
    /// The light masks of this chunk ([`LightMasks`]).
    ///
    /// Uniform and 1-bit chunks without emission are served from their storage (static words,
    /// or the packed words kept or flipped by the palette order): no build, no memory. Any
    /// other chunk builds its masks on first use ([`OwnedLightMasks::compute`]) and caches them
    /// until the blocks change.
    ///
    /// The cache belongs to one set of tables (one registry per world): asking with other
    /// tables is a bug, checked in debug builds.
    pub fn light_masks(&self, tables: &LightTables) -> LightMasks<'_> {
        if let Some(masks) = LightMasks::borrowed(&self.blocks, tables) {
            return masks;
        }
        let masks = self
            .masks
            .0
            .get_or_init(|| OwnedLightMasks::compute(self, tables));
        debug_assert!(
            masks.built_with(tables),
            "ChunkData::light_masks called with other light tables than the cached masks"
        );
        masks.view()
    }
    /// True if built light masks are cached (for tests and memory reports).
    pub fn has_cached_light_masks(&self) -> bool {
        self.masks.0.get().is_some()
    }
    /// True if some voxel may emit light per `tables`: a palette scan, so it may answer yes for
    /// a chunk whose emitting palette entries are no longer used, but never no for a chunk with
    /// an emitter. `Direct` storage answers [`LightTables::any_emitter`].
    pub fn may_emit(&self, tables: &LightTables) -> bool {
        if !tables.any_emitter() {
            return false;
        }
        match &self.blocks {
            Paletted::Uniform(s) => tables.emits(*s),
            Paletted::Packed { palette, .. } => palette.iter().any(|&s| tables.emits(s)),
            Paletted::Direct(_) => true,
        }
    }
    /// True if some voxel may hold a state of `set`: a palette scan, so it may answer yes for a
    /// chunk whose matching palette entries are no longer used, but never no for a chunk that
    /// holds one. `Direct` storage answers yes unless `set` is empty.
    ///
    /// A `Packed` chunk caches the answer for the last set asked about (by its key) until the
    /// blocks change, so asking again costs a load. Physics asks with the states whose collision
    /// reaches into the cell above (ADR 0026) before reading cells only for them.
    pub fn may_contain(&self, set: &StateSet) -> bool {
        if set.is_empty() {
            return false;
        }
        match &self.blocks {
            Paletted::Uniform(s) => set.contains(*s),
            Paletted::Packed { palette, .. } => {
                let key = set.key.get();
                let cached = self.contains.0.load(Ordering::Relaxed);
                if cached >> 1 == key {
                    return cached & 1 != 0;
                }
                let answer = palette.iter().any(|&s| set.contains(s));
                self.contains
                    .0
                    .store(key << 1 | u64::from(answer), Ordering::Relaxed);
                answer
            }
            Paletted::Direct(_) => true,
        }
    }
    /// Occupancy of voxels that hide their neighbors per `registry` ([`BlockRegistry::occludes`],
    /// an `Opaque` full cube; light opacity 15 is [`LightMasks::opaque`], which may also hold
    /// `Cutout` blocks).
    ///
    /// Bit `z` of word `y * 32 + x`, so word `k` holds voxels `32 * k .. 32 * k + 32` in
    /// [`LocalPos`] index order.
    pub fn opaque_occupancy(&self, registry: &BlockRegistry) -> Box<Occupancy> {
        let is_opaque = |s: BlockState| registry.occludes(s);
        let mut occupancy: Box<Occupancy> = Box::new([0; CHUNK_AREA]);
        match &self.blocks {
            Paletted::Uniform(s) => {
                if is_opaque(*s) {
                    occupancy.fill(u32::MAX);
                }
            }
            Paletted::Packed {
                palette,
                bits,
                words,
            } => {
                let mut opaque = [false; 256];
                for (slot, &s) in opaque.iter_mut().zip(palette.iter()) {
                    *slot = is_opaque(s);
                }
                let b = *bits as usize;
                let per_word = 64 / b;
                let mask = (1u64 << b) - 1;
                for (wi, &w) in words.iter().enumerate() {
                    let base = wi * per_word;
                    for j in 0..per_word {
                        if opaque[((w >> (j * b)) & mask) as usize] {
                            let i = base + j;
                            occupancy[i >> 5] |= 1 << (i & 31);
                        }
                    }
                }
            }
            Paletted::Direct(states) => {
                for (word, row) in occupancy.iter_mut().zip(states.as_chunks::<32>().0) {
                    for (z, &s) in row.iter().enumerate() {
                        *word |= u32::from(is_opaque(s)) << z;
                    }
                }
            }
        }
        occupancy
    }
    /// Heap bytes used: the block storage plus the cached light masks, if built (excludes
    /// `size_of::<ChunkData>()` itself).
    pub fn heap_bytes(&self) -> usize {
        let masks = self.masks.0.get().map_or(0, |m| m.heap_bytes());
        self.blocks.heap_bytes() + masks
    }
}
