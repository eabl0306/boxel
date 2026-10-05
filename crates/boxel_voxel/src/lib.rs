//! `boxel_voxel`: the voxel data model. No ECS, no GPU.
//!
//! - [`coords`]: 32^3 chunks (`CHUNK_SHIFT = 5`), `ChunkPos`/`BlockPos` (`i64`, bounded by the
//!   world limit [`MAX_CHUNK_COORD`]), `LocalPos`, faces, 26-neighborhood. Local index layout
//!   `(y << 10) | (x << 5) | z` (z fastest), the same order as binary-greedy-meshing's padded
//!   linearization.
//! - [`registry`]: data-driven blocks; runtime `BlockState(u16)` is dense and never persisted raw.
//! - [`storage`]: Minecraft-style paletted container (Uniform | Packed 1/2/4/8 bits | Direct u16),
//!   separate light storage.
//! - [`ChunkData`]: one chunk's immutable snapshot payload (shared as `Arc<ChunkData>`; edits use
//!   `Arc::make_mut` copy-on-write). Voxel arrays never live inline in ECS components.

pub mod coords;
pub mod registry;
pub mod storage;

mod chunk;

pub use chunk::ChunkData;
pub use coords::{
    BlockPos, CHUNK_AREA, CHUNK_MASK, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME, ChunkKey, ChunkPos,
    DimensionId, Face, LocalPos, MAX_BLOCK_COORD, MAX_CHUNK_COORD, NEIGHBOR_OFFSETS,
    neighbor_index,
};
pub use registry::{
    BlockDef, BlockRegistry, BlockRegistryBuilder, BlockState, FaceTextures, RegistryError,
    RenderShape, StateFlags, TextureId, TexturePattern, TextureSpec,
};
pub use storage::{LightStorage, Occupancy, Paletted};

/// Common items.
pub mod prelude {
    pub use crate::{
        BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, Face, LocalPos,
    };
}
