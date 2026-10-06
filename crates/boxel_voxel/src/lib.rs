//! `boxel_voxel`: the voxel data model. No ECS, no GPU.
//!
//! - [`coords`]: 32^3 chunks (`CHUNK_SHIFT = 5`), `ChunkPos`/`BlockPos` (`i64`, bounded by the
//!   world limit [`MAX_CHUNK_COORD`]), `LocalPos`, faces, 26-neighborhood. Local index layout
//!   `(y << 10) | (x << 5) | z` (z fastest), the same order as binary-greedy-meshing's padded
//!   linearization.
//! - [`registry`]: data-driven blocks; runtime `BlockState(u16)` is dense and never persisted raw.
//!   Each block has an RGB emission and a light opacity (ADR 0015).
//! - [`storage`]: Minecraft-style paletted container (Uniform | Packed 1/2/4/8 bits | Direct u16).
//! - [`light`]: the light value format ([`LightValue`]) and the per-state [`LightTables`]. Light
//!   itself is derived data, computed by `boxel_light`; chunks do not store it.
//! - [`ChunkData`]: one chunk's immutable snapshot payload (shared as `Arc<ChunkData>`; edits use
//!   `Arc::make_mut` copy-on-write), with its cached [`ChunkMasks`] (opaque, partial, solid,
//!   emitters). Voxel arrays never live inline in ECS components.

pub mod coords;
pub mod light;
pub mod registry;
pub mod storage;

mod chunk;
mod masks;

pub use chunk::ChunkData;
pub use coords::{
    BlockPos, CHUNK_AREA, CHUNK_MASK, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME, ChunkKey, ChunkPos,
    DimensionId, Face, LocalPos, MAX_BLOCK_COORD, MAX_CHUNK_COORD, NEIGHBOR_OFFSETS,
    neighbor_index,
};
pub use light::{LightTables, LightValue, MAX_LIGHT, OPAQUE_OPACITY};
pub use masks::ChunkMasks;
pub use registry::{
    BlockDef, BlockRegistry, BlockRegistryBuilder, BlockState, FaceTextures, RegistryError,
    RenderShape, StateFlags, TextureId, TexturePattern, TextureSpec,
};
pub use storage::{Occupancy, Paletted};

/// Common items.
pub mod prelude {
    pub use crate::{
        BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, Face, LocalPos,
    };
}
