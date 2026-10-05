//! Paletted block storage and light storage for one 32^3 chunk.
use crate::{BlockState, CHUNK_VOLUME, LocalPos};

/// Paletted container. Indices never span `u64` words (`64 / bits` entries per word).
/// Growth on `set`: Uniform -> Packed(1) -> 2 -> 4 -> 8 -> Direct (> 256 distinct states).
/// `set` appends to the palette in place while it fits the current width; shrinking/compaction
/// only happens in [`Paletted::compact`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Paletted {
    /// Every voxel has this state (60% of a typical terrain world is uniform air).
    Uniform(BlockState),
    /// Palette + packed indices.
    Packed {
        /// Distinct states (index = packed value).
        palette: Vec<BlockState>,
        /// Bits per index: 1, 2, 4 or 8.
        bits: u8,
        /// `CHUNK_VOLUME * bits / 64` words.
        words: Box<[u64]>,
    },
    /// Raw states (more than 256 distinct).
    Direct(Box<[BlockState]>),
}

impl Default for Paletted {
    fn default() -> Self {
        Paletted::Uniform(BlockState::AIR)
    }
}

impl Paletted {
    /// State at `p`.
    #[inline]
    pub fn get(&self, p: LocalPos) -> BlockState {
        match self {
            Paletted::Uniform(s) => *s,
            Paletted::Packed {
                palette,
                bits,
                words,
            } => {
                let b = *bits as usize;
                let per_word = 64 / b;
                let i = p.index();
                let idx = (words[i / per_word] >> ((i % per_word) * b)) & ((1u64 << b) - 1);
                palette[idx as usize]
            }
            Paletted::Direct(d) => d[p.index()],
        }
    }
    /// Sets `p`, returns the previous state.
    ///
    /// Uniform becomes `Packed` with 1 bit on the first different state. In `Packed`, a state
    /// already in the palette reuses its index; a new state is appended while the palette fits
    /// the current width, otherwise the indices are re-encoded at the next width
    /// (1 -> 2 -> 4 -> 8) or, past 256 entries, the container becomes `Direct`.
    pub fn set(&mut self, p: LocalPos, s: BlockState) -> BlockState {
        let i = p.index();
        match self {
            Paletted::Uniform(u) => {
                let previous = *u;
                if previous != s {
                    let mut words = vec![0u64; CHUNK_VOLUME / 64].into_boxed_slice();
                    write_index(&mut words, 1, i, 1);
                    *self = Paletted::Packed {
                        palette: vec![previous, s],
                        bits: 1,
                        words,
                    };
                }
                previous
            }
            Paletted::Packed {
                palette,
                bits,
                words,
            } => {
                let previous = palette[read_index(words, *bits, i)];
                if previous == s {
                    return previous;
                }
                if let Some(idx) = palette.iter().position(|&e| e == s) {
                    write_index(words, *bits, i, idx);
                } else if palette.len() < 1 << *bits {
                    palette.push(s);
                    write_index(words, *bits, i, palette.len() - 1);
                } else if *bits < 8 {
                    let wider = *bits * 2;
                    *words = repack(words, *bits, wider, |idx| idx);
                    *bits = wider;
                    palette.push(s);
                    write_index(words, *bits, i, palette.len() - 1);
                } else {
                    let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
                    std::mem::take(self).decode_into(&mut dense);
                    dense[i] = s;
                    *self = Paletted::Direct(dense.into_boxed_slice());
                }
                previous
            }
            Paletted::Direct(d) => std::mem::replace(&mut d[i], s),
        }
    }
    /// Encodes a dense array (`len == CHUNK_VOLUME`) with the smallest representation.
    ///
    /// The palette lists the distinct states in first-seen order: 1 state gives `Uniform`; up to
    /// 2, 4, 16 or 256 give `Packed` with 1, 2, 4 or 8 bits; more give `Direct`.
    pub fn encode(states: &[BlockState]) -> Paletted {
        assert_eq!(
            states.len(),
            CHUNK_VOLUME,
            "encode needs CHUNK_VOLUME states"
        );
        match PaletteBuilder::collect(states) {
            Some(builder) => builder.finish(states),
            None => Paletted::Direct(states.into()),
        }
    }
    /// Decodes into `out` (`len == CHUNK_VOLUME`).
    pub fn decode_into(&self, out: &mut [BlockState]) {
        assert_eq!(
            out.len(),
            CHUNK_VOLUME,
            "decode_into needs CHUNK_VOLUME entries"
        );
        match self {
            Paletted::Uniform(s) => out.fill(*s),
            Paletted::Packed {
                palette,
                bits,
                words,
            } => {
                let b = *bits as usize;
                let per_word = 64 / b;
                let mask = (1u64 << b) - 1;
                for (w, chunk) in words.iter().zip(out.chunks_mut(per_word)) {
                    for (j, slot) in chunk.iter_mut().enumerate() {
                        *slot = palette[((w >> (j * b)) & mask) as usize];
                    }
                }
            }
            Paletted::Direct(d) => out.copy_from_slice(d),
        }
    }
    /// `Some(s)` if uniform.
    pub fn as_uniform(&self) -> Option<BlockState> {
        match self {
            Paletted::Uniform(s) => Some(*s),
            _ => None,
        }
    }
    /// Palette length (1 for Uniform, `CHUNK_VOLUME`-agnostic distinct count for Direct is not tracked: returns 0).
    pub fn palette_len(&self) -> usize {
        match self {
            Paletted::Uniform(_) => 1,
            Paletted::Packed { palette, .. } => palette.len(),
            Paletted::Direct(_) => 0,
        }
    }
    /// Bits per voxel (0 Uniform, 1/2/4/8 Packed, 16 Direct).
    pub fn bits_per_voxel(&self) -> u8 {
        match self {
            Paletted::Uniform(_) => 0,
            Paletted::Packed { bits, .. } => *bits,
            Paletted::Direct(_) => 16,
        }
    }
    /// Removes unused palette entries and narrows the width (or collapses to Uniform).
    ///
    /// `Packed` keeps the relative order of the surviving palette entries (so compacting an
    /// [`Paletted::encode`] result that uses every entry changes nothing); `Direct` is re-encoded
    /// as with [`Paletted::encode`] when at most 256 distinct states remain.
    pub fn compact(&mut self) {
        match self {
            Paletted::Uniform(_) => {}
            Paletted::Packed {
                palette,
                bits,
                words,
            } => {
                let b = *bits as usize;
                let per_word = 64 / b;
                let mask = (1u64 << b) - 1;
                let mut used = [false; MAX_PACKED_PALETTE];
                for &w in words.iter() {
                    for j in 0..per_word {
                        used[((w >> (j * b)) & mask) as usize] = true;
                    }
                }
                // Surviving entries in palette order; duplicate entries merge.
                let mut remap = [0u8; MAX_PACKED_PALETTE];
                let mut kept: Vec<BlockState> = Vec::new();
                for (old, &state) in palette.iter().enumerate().take(1 << b) {
                    if !used[old] {
                        continue;
                    }
                    let new = kept.iter().position(|&k| k == state).unwrap_or_else(|| {
                        kept.push(state);
                        kept.len() - 1
                    });
                    remap[old] = new as u8;
                }
                if let [only] = kept[..] {
                    *self = Paletted::Uniform(only);
                    return;
                }
                let narrow = width_for(kept.len());
                if narrow == *bits && kept == *palette {
                    palette.shrink_to_fit();
                    return;
                }
                *words = repack(words, *bits, narrow, |idx| usize::from(remap[idx]));
                *bits = narrow;
                kept.shrink_to_fit();
                *palette = kept;
            }
            Paletted::Direct(d) => {
                if let Some(builder) = PaletteBuilder::collect(d) {
                    let encoded = builder.finish(d);
                    *self = encoded;
                }
            }
        }
    }
    /// Heap bytes used.
    pub fn heap_bytes(&self) -> usize {
        match self {
            Paletted::Uniform(_) => 0,
            Paletted::Packed { palette, words, .. } => {
                palette.capacity() * size_of::<BlockState>() + size_of_val::<[u64]>(words)
            }
            Paletted::Direct(d) => size_of_val::<[BlockState]>(d),
        }
    }
}

/// Largest palette of a `Packed` container (8-bit indices).
const MAX_PACKED_PALETTE: usize = 256;

/// Smallest packed width (1, 2, 4 or 8 bits) for a palette of `len` entries (`len <= 256`).
fn width_for(len: usize) -> u8 {
    debug_assert!(len <= MAX_PACKED_PALETTE);
    match len {
        0..=2 => 1,
        3..=4 => 2,
        5..=16 => 4,
        _ => 8,
    }
}

/// Packed index of voxel `i` (indices never span words: `64 / bits` per word).
#[inline]
fn read_index(words: &[u64], bits: u8, i: usize) -> usize {
    let b = bits as usize;
    let per_word = 64 / b;
    ((words[i / per_word] >> ((i % per_word) * b)) & ((1u64 << b) - 1)) as usize
}

/// Overwrites the packed index of voxel `i` with `idx` (`idx < 1 << bits`).
#[inline]
fn write_index(words: &mut [u64], bits: u8, i: usize, idx: usize) {
    debug_assert!(idx < 1 << bits);
    let b = bits as usize;
    let per_word = 64 / b;
    let shift = (i % per_word) * b;
    let mask = ((1u64 << b) - 1) << shift;
    let word = &mut words[i / per_word];
    *word = (*word & !mask) | ((idx as u64) << shift);
}

/// Re-encodes `CHUNK_VOLUME` indices from `from` to `to` bits, mapping each index through `map`
/// (results must fit `to` bits).
fn repack(words: &[u64], from: u8, to: u8, map: impl Fn(usize) -> usize) -> Box<[u64]> {
    let (fb, tb) = (from as usize, to as usize);
    let (from_per_word, to_per_word) = (64 / fb, 64 / tb);
    let from_mask = (1u64 << fb) - 1;
    let mut out = vec![0u64; CHUNK_VOLUME / to_per_word].into_boxed_slice();
    for (wi, &w) in words.iter().enumerate() {
        let base = wi * from_per_word;
        for j in 0..from_per_word {
            let idx = map(((w >> (j * fb)) & from_mask) as usize);
            debug_assert!(idx < 1 << tb);
            let i = base + j;
            out[i / to_per_word] |= (idx as u64) << ((i % to_per_word) * tb);
        }
    }
    out
}

/// Block size of the run skipping in [`PaletteBuilder::collect`].
const RUN_BLOCK: usize = 64;

/// True if every state of `block` is `s`. No early exit, so the compiler can vectorize it.
#[inline]
fn all_equal(block: &[BlockState], s: BlockState) -> bool {
    block.iter().fold(true, |all, &t| all & (t == s))
}

/// Fixed-size open-addressing map from state to palette index, sized for the largest packed
/// palette (256 entries in 512 slots, so probing always reaches an empty slot). Keeps
/// [`Paletted::encode`] allocation-free apart from its output.
struct PaletteIndex {
    /// `state << 16 | index`, or [`PaletteIndex::EMPTY`].
    slots: [u32; PaletteIndex::SLOTS],
}

impl PaletteIndex {
    const SLOTS: usize = 2 * MAX_PACKED_PALETTE;
    const EMPTY: u32 = u32::MAX;

    fn new() -> Self {
        Self {
            slots: [Self::EMPTY; Self::SLOTS],
        }
    }

    /// First probe slot (Fibonacci hashing: the top 9 bits of the product).
    #[inline]
    fn home(s: BlockState) -> usize {
        (u32::from(s.0).wrapping_mul(0x9E37_79B9) >> (32 - Self::SLOTS.trailing_zeros())) as usize
    }

    #[inline]
    fn get(&self, s: BlockState) -> Option<usize> {
        let mut slot = Self::home(s);
        loop {
            let v = self.slots[slot];
            if v == Self::EMPTY {
                return None;
            }
            if (v >> 16) as u16 == s.0 {
                return Some((v & 0xffff) as usize);
            }
            slot = (slot + 1) % Self::SLOTS;
        }
    }

    /// Inserts a state that is not present yet (`index < 256`).
    fn insert(&mut self, s: BlockState, index: usize) {
        debug_assert!(index < MAX_PACKED_PALETTE);
        let mut slot = Self::home(s);
        while self.slots[slot] != Self::EMPTY {
            slot = (slot + 1) % Self::SLOTS;
        }
        self.slots[slot] = (u32::from(s.0) << 16) | index as u32;
    }
}

/// Distinct states of a dense array in first-seen order, with their index map.
struct PaletteBuilder {
    palette: Vec<BlockState>,
    index: PaletteIndex,
}

impl PaletteBuilder {
    /// Collects the palette of `states` (not empty); `None` if there are more than 256
    /// distinct states.
    fn collect(states: &[BlockState]) -> Option<Self> {
        let mut index = PaletteIndex::new();
        let mut last = states[0];
        index.insert(last, 0);
        let mut palette = vec![last];
        for block in states.chunks(RUN_BLOCK) {
            // Long runs are common (air, stone): skip whole blocks that repeat the last state.
            if all_equal(block, last) {
                continue;
            }
            for &s in block {
                if s == last {
                    continue;
                }
                last = s;
                if index.get(s).is_none() {
                    if palette.len() == MAX_PACKED_PALETTE {
                        return None;
                    }
                    index.insert(s, palette.len());
                    palette.push(s);
                }
            }
        }
        Some(Self { palette, index })
    }

    /// Builds the container for the `states` this palette was collected from.
    fn finish(self, states: &[BlockState]) -> Paletted {
        let Self { palette, index } = self;
        if let [only] = palette[..] {
            return Paletted::Uniform(only);
        }
        let bits = width_for(palette.len());
        let b = bits as usize;
        // `idx * repeat` writes `idx` into every slot of a word.
        let repeat = u64::MAX / ((1u64 << b) - 1);
        let first = states[0];
        let mut last = (
            first,
            index.get(first).expect("first state is in the palette"),
        );
        let words = states
            .chunks_exact(64 / b)
            .map(|run| {
                if all_equal(run, last.0) {
                    return last.1 as u64 * repeat;
                }
                let mut word = 0u64;
                for (j, &s) in run.iter().enumerate() {
                    if s != last.0 {
                        let idx = index.get(s).expect("state was collected into the palette");
                        last = (s, idx);
                    }
                    word |= (last.1 as u64) << (j * b);
                }
                word
            })
            .collect();
        Paletted::Packed {
            palette,
            bits,
            words,
        }
    }
}

/// Sky and block light (4 bits each). M0 only uses `Uniform { sky: 15, block: 0 }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LightStorage {
    /// Same light everywhere.
    Uniform {
        /// Sky light 0..=15.
        sky: u8,
        /// Block light 0..=15.
        block: u8,
    },
    /// One byte per voxel: `sky << 4 | block` (allocated by M1 lighting).
    Packed(Box<[u8]>),
}

impl Default for LightStorage {
    fn default() -> Self {
        LightStorage::Uniform { sky: 15, block: 0 }
    }
}

impl LightStorage {
    /// Heap bytes used (0 for `Uniform`).
    pub fn heap_bytes(&self) -> usize {
        match self {
            LightStorage::Uniform { .. } => 0,
            LightStorage::Packed(bytes) => bytes.len(),
        }
    }
}

/// 1 bit per voxel, `u32` columns along z, indexed `y * 32 + x` (4 KiB). Base for mesher masks,
/// physics and raycasts (maintained incrementally from M1).
pub type Occupancy = [u32; 1024];

const _: () = assert!(CHUNK_VOLUME == 32768);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_for_matches_the_growth_steps() {
        let widths: Vec<u8> = [2, 3, 4, 5, 16, 17, 256].map(width_for).to_vec();
        assert_eq!(widths, [1, 2, 2, 4, 4, 8, 8]);
    }

    #[test]
    fn palette_index_survives_full_load_and_collisions() {
        // States that differ only in high bits collide more in multiplicative hashing; fill
        // the index to its 256-entry limit with them and with ordinary ids.
        let mut index = PaletteIndex::new();
        let states: Vec<BlockState> = (0..128u16)
            .map(|i| BlockState(i << 9))
            .chain((0..128u16).map(|i| BlockState(i * 3 + 1)))
            .collect();
        for (i, &s) in states.iter().enumerate() {
            assert_eq!(index.get(s), None);
            index.insert(s, i);
        }
        for (i, &s) in states.iter().enumerate() {
            assert_eq!(index.get(s), Some(i));
        }
        assert_eq!(index.get(BlockState(u16::MAX)), None);
        assert_eq!(index.get(BlockState(2)), None);
    }

    #[test]
    fn repack_widens_and_narrows_losslessly() {
        let mut words = vec![0u64; CHUNK_VOLUME / 32].into_boxed_slice();
        for i in 0..CHUNK_VOLUME {
            write_index(&mut words, 2, i, (i * 7 + i / 3) % 4);
        }
        let wide = repack(&words, 2, 8, |idx| idx);
        assert_eq!(wide.len(), CHUNK_VOLUME / 8);
        let back = repack(&wide, 8, 2, |idx| idx);
        assert_eq!(back, words);
        for i in (0..CHUNK_VOLUME).step_by(13) {
            assert_eq!(read_index(&wide, 8, i), read_index(&words, 2, i));
        }
        let flipped = repack(&words, 2, 2, |idx| 3 - idx);
        for i in (0..CHUNK_VOLUME).step_by(11) {
            assert_eq!(read_index(&flipped, 2, i), 3 - read_index(&words, 2, i));
        }
    }
}
