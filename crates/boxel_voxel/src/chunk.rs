use crate::{
    BlockRegistry, BlockState, CHUNK_AREA, CHUNK_VOLUME, LightStorage, LocalPos, Occupancy,
    Paletted, RenderShape,
};

/// One chunk's voxel payload. Shared as `Arc<ChunkData>` snapshots; edit through
/// `Arc::make_mut` (copy-on-write when a job still holds the old snapshot).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkData {
    /// Block states.
    pub blocks: Paletted,
    /// Light.
    pub light: LightStorage,
    /// Bumped by every mutation (mesh results built from an older version are discarded).
    pub version: u32,
}

impl ChunkData {
    /// A chunk filled with `state`, version 0.
    pub fn uniform(state: BlockState) -> Self {
        Self {
            blocks: Paletted::Uniform(state),
            light: LightStorage::default(),
            version: 0,
        }
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
    ///
    /// Frozen behavior: `get`, `version` and the return value. The representation follows
    /// [`Paletted::set`] (the palette grows in place; nothing shrinks until
    /// [`Paletted::compact`]).
    pub fn set(&mut self, p: LocalPos, state: BlockState) -> BlockState {
        let previous = self.blocks.set(p, state);
        if previous != state {
            self.version = self.version.wrapping_add(1);
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
        Self {
            blocks: Paletted::encode(states),
            light: LightStorage::default(),
            version: 0,
        }
    }
    /// Decodes into `out` (`len == CHUNK_VOLUME`).
    pub fn to_dense(&self, out: &mut [BlockState]) {
        self.blocks.decode_into(out);
    }
    /// Occupancy of voxels whose shape is `Opaque` per `registry`.
    ///
    /// Bit `z` of word `y * 32 + x`, so word `k` holds voxels `32 * k .. 32 * k + 32` in
    /// [`LocalPos`] index order.
    pub fn opaque_occupancy(&self, registry: &BlockRegistry) -> Box<Occupancy> {
        let is_opaque = |s: BlockState| registry.shape(s) == RenderShape::Opaque;
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
    /// Heap bytes used (block and light storage; excludes `size_of::<ChunkData>()` itself).
    pub fn heap_bytes(&self) -> usize {
        self.blocks.heap_bytes() + self.light.heap_bytes()
    }
}
