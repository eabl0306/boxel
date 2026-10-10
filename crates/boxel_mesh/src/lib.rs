//! `boxel_mesh`: CPU meshing. Binary greedy meshing (an in-repo fork of `binary-greedy-meshing`
//! 0.5.2, padded 34^3) into the canonical 8-byte [`PackedQuad`], grouped in 8 buckets: 6 opaque
//! face buckets (`Face` order), 1 cutout bucket (all faces of cutout materials such as leaves,
//! and the sloping tops of opaque fluids; drawn with alpha test and no backface culling) and 1
//! translucent bucket. Same bytes on every render tier.
//!
//! Light and ambient occlusion are baked per quad corner (ADR 0015): each quad carries an AO
//! byte and the index of its 4 corner lights in the chunk's light palette
//! ([`ChunkMesh::palette`]). Both are part of the merge key, and both only depend on the vertex,
//! so a merged quad is exact. [`AoPolicy`] says which faces get AO.
//! No ECS, no GPU.

mod fluid;
mod greedy;
mod mesher;
mod packed;
mod padded;
pub mod shading;

pub use fluid::{FLAT as FLUID_FLAT, fluid_flow, reference_fluid_heights};
pub use mesher::{
    AoPolicy, MaterialTable, MeshInput, MeshOptions, Mesher, reference_box_face_shading_at,
    reference_face_shading, reference_face_shading_at, reference_visible_faces,
};
pub use packed::{
    ChunkMesh, FULL_SKY_ENTRY, LightQuantization, MAX_PALETTE_ENTRIES, PackedQuad, PaletteEntry,
    layout, pack_model_box, palette_corners, palette_entry, unpack_model_box,
};
pub use padded::{FLUID_SURFACE_BIT, PADDED, PADDED_AREA, PADDED_VOLUME, PaddedChunk, VoxelClass};
pub use shading::FaceShading;

/// Number of buckets per chunk mesh (frozen layout).
pub const BUCKET_COUNT: usize = 8;
/// Index of the cutout bucket (`RenderShape::Cutout`, all six faces; alpha-tested pipeline). It
/// also holds the sloping tops of opaque fluids ([`MaterialTable::sloped_bucket`]).
pub const CUTOUT_BUCKET: usize = 6;
/// Index of the translucent bucket (`RenderShape::Translucent`; blended, drawn last).
pub const TRANSLUCENT_BUCKET: usize = 7;
/// Max quads in one opaque face bucket of a 32^3 chunk of opaque cubes (checkerboard: 32 * 32 *
/// 16). An opaque fluid (lava, ADR 0025) hides no neighbor, so scenes with it can reach one face
/// per cell, still under [`MAX_QUADS_PER_BUCKET`].
pub const MAX_QUADS_PER_FACE_BUCKET: u32 = 16384;
/// Max quads in any bucket (the cutout and translucent buckets hold all six faces). The shared
/// index buffer of the High/Mid tiers covers this many quads.
pub const MAX_QUADS_PER_BUCKET: u32 = 6 * MAX_QUADS_PER_FACE_BUCKET;
