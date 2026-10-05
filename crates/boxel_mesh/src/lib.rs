//! `boxel_mesh`: CPU meshing. Wraps `binary-greedy-meshing` 0.5.2 (`Mesher<32>`, padded 34^3) and
//! converts its quads into the canonical 8-byte [`PackedQuad`], grouped in 8 buckets: 6 opaque
//! face buckets (`Face` order), 1 cutout bucket (all faces of cutout materials such as leaves,
//! drawn with alpha test and no backface culling) and 1 translucent bucket. Same bytes on every
//! render tier.
//! No ECS, no GPU.

mod mesher;
mod packed;
mod padded;

pub use mesher::{MaterialTable, MeshInput, Mesher, reference_visible_faces};
pub use packed::{ChunkMesh, PackedQuad, layout};
pub use padded::{PADDED, PADDED_AREA, PADDED_VOLUME, PaddedChunk, VoxelClass};

/// Number of buckets per chunk mesh (frozen layout).
pub const BUCKET_COUNT: usize = 8;
/// Index of the cutout bucket (`RenderShape::Cutout`, all six faces; alpha-tested pipeline).
pub const CUTOUT_BUCKET: usize = 6;
/// Index of the translucent bucket (`RenderShape::Translucent`; meshed, not drawn in M0).
pub const TRANSLUCENT_BUCKET: usize = 7;
/// Max quads in one opaque face bucket of a 32^3 chunk (checkerboard: 32 * 32 * 16).
pub const MAX_QUADS_PER_FACE_BUCKET: u32 = 16384;
/// Max quads in any bucket (the cutout and translucent buckets hold all six faces). The shared
/// index buffer of the High/Mid tiers covers this many quads.
pub const MAX_QUADS_PER_BUCKET: u32 = 6 * MAX_QUADS_PER_FACE_BUCKET;
