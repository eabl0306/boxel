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
    pub fn set(&mut self, p: LocalPos, s: BlockState) -> BlockState {
        todo!("WP4")
    }
    /// Encodes a dense array (`len == CHUNK_VOLUME`) with the smallest representation.
    pub fn encode(states: &[BlockState]) -> Paletted {
        todo!("WP4")
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
        todo!("WP4")
    }
    /// Bits per voxel (0 Uniform, 1/2/4/8 Packed, 16 Direct).
    pub fn bits_per_voxel(&self) -> u8 {
        todo!("WP4")
    }
    /// Removes unused palette entries and narrows the width (or collapses to Uniform).
    pub fn compact(&mut self) {
        todo!("WP4")
    }
    /// Heap bytes used.
    pub fn heap_bytes(&self) -> usize {
        todo!("WP4")
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

/// 1 bit per voxel, `u32` columns along z, indexed `y * 32 + x` (4 KiB). Base for mesher masks,
/// physics and raycasts (maintained incrementally from M1).
pub type Occupancy = [u32; 1024];

const _: () = assert!(CHUNK_VOLUME == 32768);
