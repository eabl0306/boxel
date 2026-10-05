use crate::BUCKET_COUNT;
use boxel_voxel::Face;
use bytemuck::{Pod, Zeroable};
use std::ops::Range;

/// Bit layout of [`PackedQuad`] (frozen; mirrored by constants in `common.wgsl`).
///
/// `geo`: `x:5 | y:5 | z:5 | (w-1):5 | (h-1):5 | face:3 | reserved:4`
/// `mat`: `layer:12 | ao:8 (2 bits per corner, 3 = open) | sky:4 | block:4 | flags:4`
pub mod layout {
    /// Mask of one 5-bit position/extent field.
    pub const GEO_POS_MASK: u32 = 31;
    /// Shift of y.
    pub const GEO_Y_SHIFT: u32 = 5;
    /// Shift of z.
    pub const GEO_Z_SHIFT: u32 = 10;
    /// Shift of w-1.
    pub const GEO_W_SHIFT: u32 = 15;
    /// Shift of h-1.
    pub const GEO_H_SHIFT: u32 = 20;
    /// Shift of face.
    pub const GEO_FACE_SHIFT: u32 = 25;
    /// Mask of the texture layer.
    pub const MAT_LAYER_MASK: u32 = 0xFFF;
    /// Shift of the 4x2-bit AO field.
    pub const MAT_AO_SHIFT: u32 = 12;
    /// Shift of sky light.
    pub const MAT_SKY_SHIFT: u32 = 20;
    /// Shift of block light.
    pub const MAT_BLOCK_SHIFT: u32 = 24;
    /// Shift of flags.
    pub const MAT_FLAGS_SHIFT: u32 = 28;
}

/// One greedy quad, 8 bytes, identical on every tier (storage buffer element in High/Mid,
/// instance-step `Uint32x2` vertex attribute in Low).
///
/// Canonical meaning (frozen): `pos` = chunk-local voxel (0..32) of the quad's min corner block;
/// the quad lies on that block's `face`; it spans `w` voxels along the face's u axis and `h`
/// along its v axis ([`Face::axes`]). The face plane is at `pos[n] + 1` for positive faces and
/// `pos[n]` for negative ones (added by the shader).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct PackedQuad {
    /// Geometry word.
    pub geo: u32,
    /// Material word.
    pub mat: u32,
}

impl PackedQuad {
    /// New quad with AO fully open (`0xFF`), sky light 15, block light 0, flags 0.
    /// Debug-asserts `pos < 32`, `1 <= w, h <= 32`, `layer < 4096`.
    pub const fn new(pos: [u32; 3], w: u32, h: u32, face: Face, layer: u16) -> Self {
        debug_assert!(pos[0] < 32 && pos[1] < 32 && pos[2] < 32);
        debug_assert!(w >= 1 && w <= 32 && h >= 1 && h <= 32 && (layer as u32) < 4096);
        use layout::*;
        let geo = pos[0]
            | pos[1] << GEO_Y_SHIFT
            | pos[2] << GEO_Z_SHIFT
            | (w - 1) << GEO_W_SHIFT
            | (h - 1) << GEO_H_SHIFT
            | (face as u32) << GEO_FACE_SHIFT;
        let mat = (layer as u32 & MAT_LAYER_MASK) | 0xFF << MAT_AO_SHIFT | 15 << MAT_SKY_SHIFT;
        Self { geo, mat }
    }
    /// Replaces sky/block light (0..=15 each).
    pub const fn with_light(mut self, sky: u8, block: u8) -> Self {
        use layout::*;
        self.mat = (self.mat & !(0xFF << MAT_SKY_SHIFT))
            | ((sky as u32 & 15) << MAT_SKY_SHIFT)
            | ((block as u32 & 15) << MAT_BLOCK_SHIFT);
        self
    }
    /// Replaces AO (per corner 0..=3, corner order (u,v) = (0,0), (1,0), (0,1), (1,1)).
    pub const fn with_ao(mut self, ao: [u8; 4]) -> Self {
        use layout::*;
        let packed = (ao[0] as u32 & 3)
            | (ao[1] as u32 & 3) << 2
            | (ao[2] as u32 & 3) << 4
            | (ao[3] as u32 & 3) << 6;
        self.mat = (self.mat & !(0xFF << MAT_AO_SHIFT)) | packed << MAT_AO_SHIFT;
        self
    }
    /// Min-corner voxel.
    pub const fn pos(self) -> [u32; 3] {
        use layout::*;
        [
            self.geo & GEO_POS_MASK,
            (self.geo >> GEO_Y_SHIFT) & GEO_POS_MASK,
            (self.geo >> GEO_Z_SHIFT) & GEO_POS_MASK,
        ]
    }
    /// Extent along u (1..=32).
    pub const fn w(self) -> u32 {
        ((self.geo >> layout::GEO_W_SHIFT) & layout::GEO_POS_MASK) + 1
    }
    /// Extent along v (1..=32).
    pub const fn h(self) -> u32 {
        ((self.geo >> layout::GEO_H_SHIFT) & layout::GEO_POS_MASK) + 1
    }
    /// Face (invalid bit patterns map to `NegZ`).
    pub const fn face(self) -> Face {
        match Face::from_index(((self.geo >> layout::GEO_FACE_SHIFT) & 7) as u8) {
            Some(f) => f,
            None => Face::NegZ,
        }
    }
    /// Texture layer.
    pub const fn layer(self) -> u16 {
        (self.mat & layout::MAT_LAYER_MASK) as u16
    }
    /// Sky light.
    pub const fn sky(self) -> u8 {
        ((self.mat >> layout::MAT_SKY_SHIFT) & 15) as u8
    }
    /// Block light.
    pub const fn block_light(self) -> u8 {
        ((self.mat >> layout::MAT_BLOCK_SHIFT) & 15) as u8
    }
    /// Every unit face covered by the quad: (voxel position, face). Test/debug helper.
    pub fn unit_faces(self) -> impl Iterator<Item = ([u32; 3], Face)> {
        let (_, ua, va) = self.face().axes();
        let (pos, face, w, h) = (self.pos(), self.face(), self.w(), self.h());
        (0..w).flat_map(move |i| {
            (0..h).map(move |j| {
                let mut p = pos;
                p[ua] += i;
                p[va] += j;
                (p, face)
            })
        })
    }
}

/// A chunk's mesh: quads stored bucket-contiguous (bucket 0 first) + per-bucket counts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkMesh {
    /// All quads, bucket order.
    pub quads: Vec<PackedQuad>,
    /// Quads per bucket (`0..6` = opaque `Face` order, `6` = cutout, `7` = translucent).
    pub bucket_counts: [u32; BUCKET_COUNT],
}

impl ChunkMesh {
    /// No quads.
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }
    /// Total quads.
    pub fn total_quads(&self) -> u32 {
        self.quads.len() as u32
    }
    /// Range of bucket `b` inside `quads`.
    pub fn bucket_range(&self, b: usize) -> Range<u32> {
        let start: u32 = self.bucket_counts[..b].iter().sum();
        start..start + self.bucket_counts[b]
    }
    /// Quads of bucket `b`.
    pub fn bucket(&self, b: usize) -> &[PackedQuad] {
        let r = self.bucket_range(b);
        &self.quads[r.start as usize..r.end as usize]
    }
    /// Clears, keeping capacity.
    pub fn clear(&mut self) {
        self.quads.clear();
        self.bucket_counts = [0; BUCKET_COUNT];
    }
}
