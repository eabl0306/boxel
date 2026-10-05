use crate::{
    BlockRegistry, BlockState, CHUNK_VOLUME, LightStorage, LocalPos, Occupancy, Paletted,
    RenderShape,
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
    /// Frozen behavior: `get`, `version` and the return value. The representation is WP4's:
    /// this skeleton baseline widens to `Paletted::Direct` on the first change; WP4 switches it
    /// to the palette-preserving `Paletted::set`.
    pub fn set(&mut self, p: LocalPos, state: BlockState) -> BlockState {
        let previous = self.blocks.get(p);
        if previous == state {
            return previous;
        }
        if !matches!(self.blocks, Paletted::Direct(_)) {
            let mut dense = vec![BlockState::AIR; CHUNK_VOLUME];
            self.blocks.decode_into(&mut dense);
            self.blocks = Paletted::Direct(dense.into_boxed_slice());
        }
        if let Paletted::Direct(d) = &mut self.blocks {
            d[p.index()] = state;
        }
        self.version = self.version.wrapping_add(1);
        previous
    }
    /// From a dense array (`len == CHUNK_VOLUME`), version 0.
    ///
    /// Frozen behavior: `get` returns `states[i]`; all-equal input gives `Uniform`. Skeleton
    /// baseline: `Uniform` or `Direct`; WP4 switches it to `Paletted::encode`.
    pub fn from_dense(states: &[BlockState]) -> Self {
        assert_eq!(
            states.len(),
            CHUNK_VOLUME,
            "from_dense needs CHUNK_VOLUME states"
        );
        let blocks = if states.iter().all(|s| *s == states[0]) {
            Paletted::Uniform(states[0])
        } else {
            Paletted::Direct(states.into())
        };
        Self {
            blocks,
            light: LightStorage::default(),
            version: 0,
        }
    }
    /// Decodes into `out` (`len == CHUNK_VOLUME`).
    pub fn to_dense(&self, out: &mut [BlockState]) {
        self.blocks.decode_into(out);
    }
    /// Occupancy of voxels whose shape is `Opaque` per `registry`.
    pub fn opaque_occupancy(&self, registry: &BlockRegistry) -> Box<Occupancy> {
        let _ = RenderShape::Opaque;
        todo!("WP4")
    }
    /// Heap bytes used.
    pub fn heap_bytes(&self) -> usize {
        todo!("WP4")
    }
}
