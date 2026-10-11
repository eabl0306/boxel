//! `boxel_voxel`: the voxel data model. No ECS, no GPU.
//!
//! - [`coords`]: 32^3 chunks (`CHUNK_SHIFT = 5`), `ChunkPos`/`BlockPos` (`i64`, bounded by the
//!   world limit [`MAX_CHUNK_COORD`]), [`WorldPos`] (a block plus an offset in it: exact
//!   sub-voxel positions at any distance), `LocalPos`, faces, 26-neighborhood. Local index layout
//!   `(y << 10) | (x << 5) | z` (z fastest), the same order as binary-greedy-meshing's padded
//!   linearization.
//! - [`registry`]: data-driven blocks; runtime `BlockState(u16)` is dense and never persisted raw.
//!   Each state has an RGB emission and a light opacity (ADR 0015).
//! - [`state`]: block state properties (ADR 0019): `game:log[axis=x]`, one dense id per
//!   combination of values, contiguous per block.
//! - [`shapes`]: the boxes of the models made of boxes (ADR 0020: slabs, and the [`BoxModel`]s a
//!   game registers, `BlockModel::Boxes`), and [`rules`]: the neighbor and use rules a block
//!   registers (the states that follow their neighbors, what using a block does) and the face
//!   covers they are written with.
//! - [`fluid`]: the fluid types a registry declares (ADR 0021, ADR 0025): how each flows and
//!   what it does with the others.
//! - [`faces`]: which faces of a chunk see each other through non-occluding voxels
//!   ([`ChunkFaceInfo`]), for the world's cave culling (ADR 0022).
//! - [`storage`]: Minecraft-style paletted container (Uniform | Packed 1/2/4/8 bits | Direct u16).
//! - [`light`]: the light value format ([`LightValue`]) and the per-state [`LightTables`]. Light
//!   itself is derived data, computed by `boxel_light`; chunks do not store it.
//! - [`lod`]: far level of detail (ADR 0031): the [`LodClass`] of every state and the cell rule
//!   that turns a cube of blocks into one coarse cell ([`CellPartial`]).
//! - [`ChunkData`]: one chunk's immutable snapshot payload (shared as `Arc<ChunkData>`; edits use
//!   `Arc::make_mut` copy-on-write), with its light masks ([`LightMasks`]: opaque, partial,
//!   emitters; built and cached only when the storage cannot serve them as is), and
//!   [`ChunkData::may_contain`], a cached palette scan for a [`StateSet`]. Voxel arrays never
//!   live inline in ECS components.
//! - `testing` (with the `testing` feature): a minimal block set with `test:` names for engine
//!   tests; the engine itself holds no content (#48).

pub mod coords;
pub mod faces;
pub mod fluid;
pub mod light;
pub mod lod;
pub mod registry;
pub mod rules;
pub mod shapes;
pub mod state;
pub mod storage;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

mod chunk;
mod masks;

pub use chunk::{ChunkData, StateSet};
pub use coords::{
    BlockPos, CHUNK_AREA, CHUNK_MASK, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME, ChunkKey, ChunkPos,
    DimensionId, Face, LocalPos, MAX_BLOCK_COORD, MAX_CHUNK_COORD, NEIGHBOR_OFFSETS, WorldPos,
    neighbor_index,
};
pub use faces::{ALL_LINKS, ChunkFaceInfo, link_bit};
pub use fluid::{FluidHardening, FluidId, FluidReplace, FluidRules, FluidType, MAX_FLUIDS};
pub use light::{LightTables, LightValue, MAX_LIGHT, OPAQUE_OPACITY};
pub use lod::{CellPartial, LodClass, LodClasses};
pub use masks::{LightMasks, MaskRows, MaskWords, OwnedLightMasks};
pub use registry::{
    Axis, BlockCollision, BlockDef, BlockId, BlockModel, BlockRegistry, BlockRegistryBuilder,
    BlockState, BoxModel, BoxModelId, CollisionBox, FLUID_LEVEL, FaceTextures,
    MAX_COLLISION_HEIGHT, MAX_STATES, RegistryError, RenderShape, SlabHalf, StateFlags, TextureId,
    TextureImage, TextureSpec, WATERLOGGED,
};
pub use shapes::{Facing, MAX_MODEL_BOXES, ModelBox};
pub use state::{Property, StateName, StateParseError, StateValues};
pub use storage::{Occupancy, Paletted};

/// Common items.
pub mod prelude {
    pub use crate::{
        BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, Face, LocalPos,
        WorldPos,
    };
}
