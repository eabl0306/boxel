//! `#[repr(C)]` mirrors of the WGSL structs (bytemuck `Pod`). Layouts are frozen and tested.
use bytemuck::{Pod, Zeroable};

/// Uniform `View` (336 bytes, std140-compatible: only vec4/mat4 members).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct ViewUniform {
    /// Camera-relative view-projection (column-major).
    pub view_proj: [[f32; 4]; 4],
    /// Frustum planes (left, right, bottom, top, near, far), camera-relative.
    pub frustum: [[f32; 4]; 6],
    /// xyz = render origin (voxels), low 32 bits of the `i64` value (wrapping), w = 0.
    pub render_origin: [i32; 4],
    /// xyz = eye relative to render origin, w = 0.
    pub camera_rel: [f32; 4],
    /// Fog color (linear RGB, a = 1).
    pub fog_color: [f32; 4],
    /// x = fog start, y = fog end, zw = 0.
    pub fog_params: [f32; 4],
    /// x = chunk slot count; y = opaque draw-list capacity (also the first index of the cutout
    /// list); z = cutout draw-list capacity; w = 0. Capacities are at most
    /// [`bindings::MAX_DRAWS_PER_LIST`].
    pub counts: [u32; 4],
    /// rgb = sky light color times the daylight intensity (linear), a = ambient floor (no lit
    /// surface is darker than its albedo times this).
    pub sky_light: [f32; 4],
    /// x = AO strength (`1 - x` is what the darkest corner keeps), y = block-light gain, zw = 0.
    pub light_params: [f32; 4],
    /// Brightness of light levels `0..=15`: level `4k + j` at `[k][j]`, level 15 = 1. The
    /// shader interpolates it linearly between levels.
    pub light_curve: [[f32; 4]; 4],
}

/// Storage `ChunkGpu` (64 bytes) in the chunk table, one per render chunk slot.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct ChunkGpu {
    /// World voxel coordinates of the chunk min corner, low 32 bits of the `i64` value (wrapping).
    /// The shaders only use it minus `ViewUniform::render_origin` (wrapping too), which is exact
    /// for chunks less than `2^31` voxels from the camera.
    pub origin: [i32; 3],
    /// First quad of bucket 0 in the arena (buckets are contiguous, `boxel_mesh` order).
    pub quad_base: u32,
    /// Quad counts per bucket: `[0..6)` opaque faces, `[6]` cutout, `[7]` translucent.
    pub counts: [u32; 8],
    /// Bit 0 = [`ChunkGpu::FLAG_LIVE`].
    pub flags: u32,
    /// First entry of the chunk's light palette in the arena (`quad_base` + total quads): High
    /// and Mid read a quad's 4 corner lights at `light_base` + its palette index.
    pub light_base: u32,
    /// Entries of the chunk's light palette (at least 1). A quad whose index is not below it is
    /// lit as open sky, as on Low and on the CPU, instead of reading another chunk's data.
    pub light_len: u32,
    /// Padding to 64 bytes (WGSL struct alignment 16).
    pub pad: u32,
}

impl ChunkGpu {
    /// `flags` bit: slot is live, i.e. holds a chunk the cull may draw. The renderer sets it only
    /// for chunks near enough to the camera for the wrapped origin difference to be exact. Only
    /// the High cull reads it (Mid tests the same window on the CPU, and its flags may lag).
    pub const FLAG_LIVE: u32 = 1;
}

/// Per-draw reference read as an instance-step `Uint32x2` vertex attribute (location 0) by
/// `vs_pull`: which chunk slot and which first quad. 8 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct DrawRef {
    /// Chunk table slot.
    pub chunk_slot: u32,
    /// First quad of the drawn bucket in the arena.
    pub quad_start: u32,
}

/// Immediate data of the Low tier (16 bytes): chunk origin.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct ChunkImmediate {
    /// xyz = world voxel origin (low 32 bits, like [`ChunkGpu::origin`]), w = 0.
    pub origin: [i32; 4],
}

/// Matches `wgpu::util::DrawIndexedIndirectArgs` and WGSL `DrawArgs` (20 bytes, tightly packed).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct DrawIndexedIndirect {
    /// Indices to draw (6 per quad).
    pub index_count: u32,
    /// Always 1.
    pub instance_count: u32,
    /// Always 0.
    pub first_index: u32,
    /// Always 0 (never rely on base vertex reaching the shader).
    pub base_vertex: i32,
    /// `DrawRef` index (requires `INDIRECT_FIRST_INSTANCE`).
    pub first_instance: u32,
}

/// Bind group layout slots (frozen).
pub mod bindings {
    /// Group 0 (static): block texture array (binding 0) + sampler (binding 1). Fragment.
    pub const GROUP_MATERIALS: u32 = 0;
    /// Group 1 (per frame): `View` uniform (binding 0). Vertex | Fragment.
    pub const GROUP_VIEW: u32 = 1;
    /// Group 2 (High/Mid): quad arena (binding 0) + chunk table (binding 1), read-only storage. Vertex.
    pub const GROUP_GEOMETRY: u32 = 2;
    /// Cull pipeline group 0: view (0), chunks (1), draw args (2), draw refs (3), draw counts (4,
    /// `array<atomic<u32>, 2>`: opaque, cutout).
    pub const CULL_GROUP: u32 = 0;
    /// Max draws per indirect list and per `multi_draw_indexed_indirect_count` call. wgpu-hal
    /// passes `max_count` to `vkCmdDrawIndexedIndirectCount` without checking
    /// `maxDrawIndirectCount`, whose Vulkan minimum is 65535 (desktop drivers report 2^32 - 1).
    /// Draws past the capacity are dropped by `cull.wgsl` (never written).
    pub const MAX_DRAWS_PER_LIST: u32 = 65535;
    /// Immediate bytes used by the Low tier.
    pub const LOW_IMMEDIATE_SIZE: u32 = 16;
    /// Workgroup size of `cull_chunks`.
    pub const CULL_WORKGROUP_SIZE: u32 = 64;
}
