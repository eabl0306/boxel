use crate::BUCKET_COUNT;
use boxel_voxel::{Face, LightValue};
use bytemuck::{Pod, Zeroable};
use std::hash::{BuildHasher, Hasher};
use std::ops::Range;

/// Bit layout of [`PackedQuad`] v2 (frozen; mirrored by constants in `common.wgsl`).
///
/// `geo`: `x:5 | y:5 | z:5 | (w-1):5 | (h-1):5 | face:3 | flags:4` (flag bit 0:
/// [`FLAG_EMISSIVE`](crate::layout::FLAG_EMISSIVE); bits 1 to 3: the quad's shape code,
/// [`GEO_SHAPE_SHIFT`](crate::layout::GEO_SHAPE_SHIFT), ADR 0018)
/// `mat`: `layer:12 | ao:8 (2 bits per corner, 3 = open) | light:12 (palette index)`
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
    /// Shift of the 4 flag bits.
    pub const GEO_FLAGS_SHIFT: u32 = 28;
    /// Flag bit (of the 4 at [`GEO_FLAGS_SHIFT`]): the quad's block emits light, so the shader
    /// draws it at the full brightness of its texture (no light, face shading or AO), the way
    /// Minecraft draws glowing blocks. Set by the mesher from `MaterialTable::emissive`.
    pub const FLAG_EMISSIVE: u32 = 1;
    /// Shift of the 3-bit shape code (flag bits 1 to 3, ADR 0018): how the shader turns the
    /// quad's cell and face into geometry. Codes other than [`SHAPE_CUBE`] and
    /// [`SHAPE_TURNED`] come from the per-block path and have `w = h = 1`.
    pub const GEO_SHAPE_SHIFT: u32 = 29;
    /// Mask of the shape code (after the shift).
    pub const GEO_SHAPE_MASK: u32 = 7;
    /// A cube face (the greedy quads).
    pub const SHAPE_CUBE: u8 = 0;
    /// A cube face whose texture is turned a quarter (transposed: its vertical runs along the
    /// face's u axis), for logs lying along X or Z. Greedy quads too.
    pub const SHAPE_TURNED: u8 = 1;
    /// A face of the lower half of the cell (bottom slab): side faces span `y` in `0..0.5`, the
    /// +Y face lies at `y + 0.5`.
    pub const SHAPE_LOWER_HALF: u8 = 2;
    /// A face of the upper half of the cell (top slab): side faces span `y` in `0.5..1`, the -Y
    /// face lies at `y + 0.5`.
    pub const SHAPE_UPPER_HALF: u8 = 3;
    /// A cross plant quad through the cell's diagonal, full height: face `PosX` is the one from
    /// `(0, 0)` to `(1, 1)` in (x, z), face `PosZ` the one from `(1, 0)` to `(0, 1)`. Drawn from
    /// both sides (cutout pipeline), without face shading, its light is the block's own cell.
    pub const SHAPE_CROSS: u8 = 4;
    /// A face of a fluid's surface cell (`BlockModel::Fluid`): the cell's top drops to
    /// [`FLUID_SURFACE`] of a block. The +Y face lies there and the side faces span `y` in
    /// `0..FLUID_SURFACE`; the -Y face is the cube's. Greedy quads (any `w`; side faces have
    /// `h = 1`, since a surface cell never has a surface cell of its fluid above or below it).
    pub const SHAPE_FLUID_SURFACE: u8 = 5;
    /// Height of a fluid's surface within its cell (14/16, about Minecraft's 8/9).
    pub const FLUID_SURFACE: f32 = 0.875;
    /// A face of a fluid cell whose top is not flat (flowing water, a slope down to a drop;
    /// ADR 0021): the top's 4 corners have their own heights, given in sixteenths of a block by
    /// the quad ([`PackedQuad::fluid_heights`]). Per-block quads (`w = h = 1`): the 10 bits of
    /// `w - 1` and `h - 1` hold the heights of corners 0 and 1 (bits 15..19 and 19..23, 4 bits
    /// each, `height - 1`) and the AO byte those of corners 2 and 3 (the quad has no AO). Corner
    /// `k` of the cell's top is at `(x, z) = (k & 1, k >> 1)`. The +Y face is the quad through
    /// the 4 corners, the side faces run from `y = 0` up to the corners of their edge and the
    /// -Y face is the cube's.
    pub const SHAPE_FLUID_SLOPE: u8 = 7;
    /// Shift of the corner heights in `geo` of a [`SHAPE_FLUID_SLOPE`] quad (corners 0 and 1).
    pub const GEO_FLUID_SHIFT: u32 = GEO_W_SHIFT;
    /// Mask of the texture layer.
    pub const MAT_LAYER_MASK: u32 = 0xFFF;
    /// Shift of the 4x2-bit AO field.
    pub const MAT_AO_SHIFT: u32 = 12;
    /// Shift of the light palette index.
    pub const MAT_LIGHT_SHIFT: u32 = 20;
    /// Mask of the light palette index (after the shift).
    pub const MAT_LIGHT_MASK: u32 = 0xFFF;
}

/// Most entries of one chunk's light palette (the 12-bit index of [`PackedQuad`]).
pub const MAX_PALETTE_ENTRIES: usize = 4096;

/// One light palette entry: the [`LightValue`] of the 4 corners of a quad, `word0 = c0 | c1 <<
/// 16`, `word1 = c2 | c3 << 16`. Corner order (u, v): 0 = (0, 0), 1 = (1, 0), 2 = (0, 1),
/// 3 = (1, 1), the order of the AO field and of `common.wgsl`.
pub type PaletteEntry = [u32; 2];

/// The palette entry of 4 corner lights.
pub const fn palette_entry(corners: [LightValue; 4]) -> PaletteEntry {
    [
        corners[0].0 as u32 | (corners[1].0 as u32) << 16,
        corners[2].0 as u32 | (corners[3].0 as u32) << 16,
    ]
}

/// The 4 corner lights of a palette entry.
pub const fn palette_corners(entry: PaletteEntry) -> [LightValue; 4] {
    [
        LightValue(entry[0] as u16),
        LightValue((entry[0] >> 16) as u16),
        LightValue(entry[1] as u16),
        LightValue((entry[1] >> 16) as u16),
    ]
}

/// Palette entry of open sky everywhere (what a mesh without light uses).
pub const FULL_SKY_ENTRY: PaletteEntry = palette_entry([LightValue::FULL_SKY; 4]);

/// One greedy quad, 8 bytes, identical on every tier (storage buffer element in High/Mid,
/// instance-step `Uint32x2` vertex attribute in Low).
///
/// Canonical meaning (frozen): `pos` = chunk-local voxel (0..32) of the quad's min corner block;
/// the quad lies on that block's `face`; it spans `w` voxels along the face's u axis and `h`
/// along its v axis ([`Face::axes`]). The face plane is at `pos[n] + 1` for positive faces and
/// `pos[n]` for negative ones (added by the shader). The light of its 4 corners is entry
/// [`PackedQuad::light_index`] of its chunk's [`ChunkMesh::palette`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct PackedQuad {
    /// Geometry word.
    pub geo: u32,
    /// Material word.
    pub mat: u32,
}

impl PackedQuad {
    /// New quad with AO fully open (`0xFF`), light palette index 0 and no flags.
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
        let mat = (layer as u32 & MAT_LAYER_MASK) | 0xFF << MAT_AO_SHIFT;
        Self { geo, mat }
    }
    /// Replaces AO (per corner 0..=3, corner order (u,v) = (0,0), (1,0), (0,1), (1,1)).
    pub const fn with_ao(self, ao: [u8; 4]) -> Self {
        let packed = (ao[0] & 3) | (ao[1] & 3) << 2 | (ao[2] & 3) << 4 | (ao[3] & 3) << 6;
        self.with_ao_byte(packed)
    }
    /// Replaces AO with its packed byte (corner `k` at bits `2k..2k+2`).
    pub const fn with_ao_byte(mut self, ao: u8) -> Self {
        use layout::*;
        self.mat = (self.mat & !(0xFF << MAT_AO_SHIFT)) | (ao as u32) << MAT_AO_SHIFT;
        self
    }
    /// Replaces the light palette index (`0..4096`, debug-asserted).
    pub const fn with_light_index(mut self, index: u16) -> Self {
        use layout::*;
        debug_assert!((index as u32) <= MAT_LIGHT_MASK);
        self.mat = (self.mat & !(MAT_LIGHT_MASK << MAT_LIGHT_SHIFT))
            | ((index as u32 & MAT_LIGHT_MASK) << MAT_LIGHT_SHIFT);
        self
    }
    /// Replaces the 4 flag bits (`0..16`, debug-asserted; bit 0 is [`layout::FLAG_EMISSIVE`],
    /// bits 1 to 3 the shape code).
    pub const fn with_flags(mut self, flags: u8) -> Self {
        use layout::*;
        debug_assert!(flags < 16);
        self.geo = (self.geo & !(15 << GEO_FLAGS_SHIFT)) | ((flags as u32 & 15) << GEO_FLAGS_SHIFT);
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
    /// Extent along u (1..=32; 1 for a [`layout::SHAPE_FLUID_SLOPE`] quad, whose extent bits
    /// hold corner heights).
    pub const fn w(self) -> u32 {
        if self.shape() == layout::SHAPE_FLUID_SLOPE {
            return 1;
        }
        ((self.geo >> layout::GEO_W_SHIFT) & layout::GEO_POS_MASK) + 1
    }
    /// Extent along v (1..=32; 1 for a [`layout::SHAPE_FLUID_SLOPE`] quad).
    pub const fn h(self) -> u32 {
        if self.shape() == layout::SHAPE_FLUID_SLOPE {
            return 1;
        }
        ((self.geo >> layout::GEO_H_SHIFT) & layout::GEO_POS_MASK) + 1
    }
    /// The 4 top corner heights of a [`layout::SHAPE_FLUID_SLOPE`] quad's cell, in sixteenths of
    /// a block (`1..=16`; corner `k` at `(x, z) = (k & 1, k >> 1)`), or `None` for another shape.
    pub const fn fluid_heights(self) -> Option<[u8; 4]> {
        if self.shape() != layout::SHAPE_FLUID_SLOPE {
            return None;
        }
        let low = self.geo >> layout::GEO_FLUID_SHIFT;
        let high = self.mat >> layout::MAT_AO_SHIFT;
        Some([
            (low & 15) as u8 + 1,
            ((low >> 4) & 15) as u8 + 1,
            (high & 15) as u8 + 1,
            ((high >> 4) & 15) as u8 + 1,
        ])
    }
    /// The quad with shape [`layout::SHAPE_FLUID_SLOPE`] and these corner heights (sixteenths,
    /// `1..=16`, debug-asserted). Debug-asserts `w = h = 1`; replaces the AO byte.
    pub const fn with_fluid_heights(self, heights: [u8; 4]) -> Self {
        use layout::*;
        debug_assert!(self.w() == 1 && self.h() == 1);
        let mut k = 0;
        while k < 4 {
            debug_assert!(heights[k] >= 1 && heights[k] <= 16);
            k += 1;
        }
        let h = [
            (heights[0].wrapping_sub(1) & 15) as u32,
            (heights[1].wrapping_sub(1) & 15) as u32,
            (heights[2].wrapping_sub(1) & 15) as u32,
            (heights[3].wrapping_sub(1) & 15) as u32,
        ];
        let mut quad = self.with_shape(SHAPE_FLUID_SLOPE);
        quad.geo = (quad.geo & !(0xFF << GEO_FLUID_SHIFT)) | (h[0] | h[1] << 4) << GEO_FLUID_SHIFT;
        quad.mat = (quad.mat & !(0xFF << MAT_AO_SHIFT)) | (h[2] | h[3] << 4) << MAT_AO_SHIFT;
        quad
    }
    /// Face (invalid bit patterns map to `NegZ`).
    pub const fn face(self) -> Face {
        match Face::from_index(((self.geo >> layout::GEO_FACE_SHIFT) & 7) as u8) {
            Some(f) => f,
            None => Face::NegZ,
        }
    }
    /// The 4 flag bits.
    pub const fn flags(self) -> u8 {
        ((self.geo >> layout::GEO_FLAGS_SHIFT) & 15) as u8
    }
    /// Shape code (`layout::SHAPE_*`).
    pub const fn shape(self) -> u8 {
        ((self.geo >> layout::GEO_SHAPE_SHIFT) & layout::GEO_SHAPE_MASK) as u8
    }
    /// Replaces the shape code (`0..8`, debug-asserted).
    pub const fn with_shape(mut self, shape: u8) -> Self {
        use layout::*;
        debug_assert!((shape as u32) <= GEO_SHAPE_MASK);
        self.geo = (self.geo & !(GEO_SHAPE_MASK << GEO_SHAPE_SHIFT))
            | ((shape as u32 & GEO_SHAPE_MASK) << GEO_SHAPE_SHIFT);
        self
    }
    /// True if the [`layout::FLAG_EMISSIVE`] flag is set.
    pub const fn is_emissive(self) -> bool {
        self.flags() as u32 & layout::FLAG_EMISSIVE != 0
    }
    /// Texture layer.
    pub const fn layer(self) -> u16 {
        (self.mat & layout::MAT_LAYER_MASK) as u16
    }
    /// AO of corner `k` (0..=3, 3 = open; open for a [`layout::SHAPE_FLUID_SLOPE`] quad).
    pub const fn ao(self, k: usize) -> u8 {
        (self.ao_byte() >> (2 * k as u32)) & 3
    }
    /// The packed AO byte (corner `k` at bits `2k..2k+2`; open, `0xFF`, for a
    /// [`layout::SHAPE_FLUID_SLOPE`] quad, whose AO bits hold corner heights).
    pub const fn ao_byte(self) -> u8 {
        if self.shape() == layout::SHAPE_FLUID_SLOPE {
            return 0xFF;
        }
        (self.mat >> layout::MAT_AO_SHIFT) as u8
    }
    /// Light palette index.
    pub const fn light_index(self) -> u16 {
        ((self.mat >> layout::MAT_LIGHT_SHIFT) & layout::MAT_LIGHT_MASK) as u16
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

/// How a mesh's corner lights were quantized to fit [`MAX_PALETTE_ENTRIES`] (a deterministic
/// fallback, counted in the stats; almost every chunk is [`LightQuantization::Exact`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LightQuantization {
    /// Corner lights as computed.
    #[default]
    Exact,
    /// The exact palette overflowed: the block channels (red, green, blue) of every corner keep
    /// their top 3 bits (`level & 0b1110`); sky is exact.
    Block3,
    /// The `Block3` palette overflowed too: each face gets one value for its 4 corners (the
    /// rounded per-channel mean), with block channels at 2 bits (`level & 0b1100`). At most
    /// `16 * 4^3 = 1024` entries, so it always fits.
    Flat,
}

impl LightQuantization {
    /// Every level, the order the mesher tries them.
    pub const ALL: [LightQuantization; 3] = [Self::Exact, Self::Block3, Self::Flat];

    /// `corners` at this level.
    pub fn apply(self, corners: [LightValue; 4]) -> [LightValue; 4] {
        match self {
            Self::Exact => corners,
            Self::Block3 => corners.map(|c| LightValue(c.0 & 0xFEEE)),
            Self::Flat => {
                let mut mean = 0u16;
                for shift in [12, 8, 4, 0] {
                    let sum: u16 = corners.iter().map(|c| (c.0 >> shift) & 15).sum();
                    mean |= ((sum + 2) / 4) << shift;
                }
                [LightValue(mean & 0xFCCC); 4]
            }
        }
    }
}

/// A chunk's mesh: quads stored bucket-contiguous (bucket 0 first), per-bucket counts, the light
/// palette the quads index and a hash of all of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkMesh {
    /// All quads, bucket order.
    pub quads: Vec<PackedQuad>,
    /// Quads per bucket (`0..6` = opaque `Face` order, `6` = cutout, `7` = translucent).
    pub bucket_counts: [u32; BUCKET_COUNT],
    /// Light palette (at most [`MAX_PALETTE_ENTRIES`] entries, first-seen order); every quad's
    /// [`PackedQuad::light_index`] is an index into it.
    pub palette: Vec<PaletteEntry>,
    /// How the palette was quantized.
    pub light_quantization: LightQuantization,
    /// [`ChunkMesh::compute_hash`] of the content (the mesher sets it; equal meshes have equal
    /// hashes, so a remesh that changed nothing can be skipped).
    pub hash: u64,
}

impl ChunkMesh {
    /// Seed of [`ChunkMesh::compute_hash`] (fixed: hashes are equal across runs and threads).
    const HASH_SEED: u64 = 0x6d65_7368_7632;

    /// A mesh from its quads and bucket counts, every quad lit by palette entry 0 = open sky
    /// (what [`PackedQuad::new`] gives), with its hash. For hand-made meshes (tests, tools).
    pub fn from_quads(quads: Vec<PackedQuad>, bucket_counts: [u32; BUCKET_COUNT]) -> Self {
        let palette = if quads.is_empty() {
            Vec::new()
        } else {
            vec![FULL_SKY_ENTRY]
        };
        let mut mesh = Self {
            quads,
            bucket_counts,
            palette,
            ..Self::default()
        };
        mesh.hash = mesh.compute_hash();
        mesh
    }
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
    /// The 4 corner lights of `quad` (open sky if its index is outside the palette).
    pub fn corner_light(&self, quad: PackedQuad) -> [LightValue; 4] {
        self.palette
            .get(quad.light_index() as usize)
            .map_or([LightValue::FULL_SKY; 4], |&e| palette_corners(e))
    }
    /// Clears, keeping capacity.
    pub fn clear(&mut self) {
        self.quads.clear();
        self.bucket_counts = [0; BUCKET_COUNT];
        self.palette.clear();
        self.light_quantization = LightQuantization::Exact;
        self.hash = 0;
    }
    /// foldhash with a fixed seed over the quads, the bucket counts and the palette.
    pub fn compute_hash(&self) -> u64 {
        let mut h = foldhash::fast::FixedState::with_seed(Self::HASH_SEED).build_hasher();
        h.write(bytemuck::cast_slice(&self.quads));
        h.write(bytemuck::cast_slice(&self.bucket_counts));
        h.write(bytemuck::cast_slice(&self.palette));
        h.finish()
    }
}
