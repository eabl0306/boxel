//! Sun shadows on High (ADR 0065): the mirror of `shadow_common.wgsl`'s `ShadowCascade` and the
//! bindings of `shadow_cull.wgsl` and `shadow.wgsl`. Mid and Low never see any of it.
use bytemuck::{Pod, Zeroable};

/// Cascades the shadow uniform holds (`MAX_SHADOW_CASCADES` in `shadow_common.wgsl`).
pub const MAX_CASCADES: usize = 4;

/// Bytes of one [`ShadowCascadeGpu`]: the dynamic offset step of [`GROUP_CASCADE`], the
/// largest `min_uniform_buffer_offset_alignment` wgpu allows.
pub const CASCADE_STRIDE: u64 = 256;

/// Bytes of the shadow uniform: [`MAX_CASCADES`] entries.
pub const UNIFORM_SIZE: u64 = CASCADE_STRIDE * MAX_CASCADES as u64;

/// Group 0 of `cull_shadow_casters`: the `View` uniform (0), the chunk table (1), the draw args
/// (2), the draw refs (3), the cull state (4: [`STATE_COUNTS`] draw counts, then the dropped
/// buckets at [`STATE_DROPPED_OFFSET`]) and the shadow uniform (5, every cascade).
pub const CULL_GROUP: u32 = 0;
/// Workgroup size of `cull_shadow_casters`.
pub const CULL_WORKGROUP_SIZE: u32 = 64;
/// Draw counts in the cull state: two per cascade and page id, the opaque and cutout lists of
/// cascade `c` and page `p` at `(2c + k) * MAX_PAGES + p` (`k` 0 opaque, 1 cutout).
pub const STATE_COUNTS: u32 = 2 * MAX_CASCADES as u32 * super::chunk_memory::MAX_PAGES;
/// Byte offset of the dropped bucket count in the cull state.
pub const STATE_DROPPED_OFFSET: u64 = 4 * STATE_COUNTS as u64;
/// Bytes of the cull state (cleared every frame).
pub const STATE_SIZE: u64 = STATE_DROPPED_OFFSET + 16;
/// The shadow draws' group of one cascade's [`ShadowCascadeGpu`] (dynamic offset
/// `CASCADE_STRIDE * c`); groups 0 to 2 are the voxel pipelines' (materials, view, geometry).
pub const GROUP_CASCADE: u32 = 3;

/// One cascade of the shadow uniform (`ShadowCascade` in `shadow_common.wgsl`, 256 bytes). The
/// cull reads every entry; the frame's shared fields (`counts`, `light`, `band`) are written the
/// same into each one.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct ShadowCascadeGpu {
    /// Camera-relative position (relative to the `View`'s render origin) to the cascade's map:
    /// x and y in -1..1 across the sun (y up), depth 0 on the sun's side, 1 on the far side.
    pub view_proj: [[f32; 4]; 4],
    /// Its box, six inward planes (`xyz` normal, `w` d), camera-relative: left, right, bottom,
    /// top, the sun's side, the far side.
    pub planes: [[f32; 4]; 6],
    /// x = texel size in blocks, y = the cutout alpha test's mip level, z and w = the view
    /// distances it serves.
    pub params: [f32; 4],
    /// xyz = unit direction toward the sun, w = where the shadows end (view distance).
    pub light: [f32; 4],
    /// x = cascades this frame, y = the face buckets that cast (bit `f`), z = chunks one page's
    /// list of one cascade holds, w = pages with lists.
    pub counts: [u32; 4],
    /// x and y = the camera realm's voxels `[x, y)` in `y` relative to the render origin
    /// (saturated to `i32`), z = the camera realm, w = 0.
    pub band: [i32; 4],
    /// Up to [`CASCADE_STRIDE`].
    pub pad: [[u32; 4]; 2],
}

const _: () = assert!(size_of::<ShadowCascadeGpu>() as u64 == CASCADE_STRIDE);
