use boxel_voxel::TextureSpec;

/// Edge of every block texture layer (texels).
pub const BLOCK_TEXTURE_SIZE: u32 = 16;

/// CPU-side block texture array: `layers` layers of `BLOCK_TEXTURE_SIZE`^2 RGBA8 sRGB texels with
/// a full mip chain (16, 8, 4, 2, 1), stored layer-major then mip-major
/// (`wgpu::util::TextureDataOrder::LayerMajor`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockTextureData {
    /// Layer count (>= 1; a magenta layer is used when `specs` is empty).
    pub layers: u32,
    /// Mip levels (5).
    pub mip_levels: u32,
    /// Texel bytes.
    pub data: Vec<u8>,
}

/// Synthesizes the texture array from the registry's specs (deterministic per spec name).
/// Mips are box-filtered in linear space; `Leaves` alpha is kept binary (0/255) in every mip.
pub fn generate_block_textures(specs: &[TextureSpec]) -> BlockTextureData {
    todo!("WP7")
}
