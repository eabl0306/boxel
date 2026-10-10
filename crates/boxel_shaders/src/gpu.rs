//! `#[repr(C)]` mirrors of the WGSL structs (bytemuck `Pod`). Layouts are frozen and tested.
use bytemuck::{Pod, Zeroable};

/// Model boxes the `View` uniform holds after [`ViewUniform`] (`boxel_voxel::MAX_MODEL_BOXES`,
/// ADR 0020): two `u32` words each, two boxes per `vec4<u32>`.
pub const VIEW_BOX_CAPACITY: usize = 1024;
/// Byte offset of the model box table in the `View` uniform buffer, right after the per-frame
/// [`ViewUniform`].
pub const VIEW_BOXES_OFFSET: u64 = size_of::<ViewUniform>() as u64;
/// Bytes of the whole `View` uniform buffer: the per-frame [`ViewUniform`] (written every frame)
/// and the model box table (written when the blocks change).
pub const VIEW_BUFFER_SIZE: u64 = VIEW_BOXES_OFFSET + 8 * VIEW_BOX_CAPACITY as u64;

/// The per-frame part of the uniform `View` (352 bytes, std140-compatible: only vec4/mat4
/// members). The buffer continues with the model box table ([`VIEW_BOXES_OFFSET`]).
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
    /// list); z = cutout draw-list capacity; w = 1 when the cave culling bits of the cull state
    /// are in use (High skips the slots whose bit is 0), else 0. Capacities are at most
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
    /// xy = render target size in pixels (the voxel shaders grow each quad by a fixed fraction
    /// of a pixel to close cracks at T-junctions), z = seconds of the fluid animation (the flow
    /// textures scroll with it, ADR 0025; wrapped by the caller), w = the flow textures that do
    /// not scroll because their resource pack animates them (ADR 0028): bit 0 water, bit 1 lava,
    /// as a float (0 to 3).
    pub viewport: [f32; 4],
}

/// Layers the fog uniform holds (`boxel_fog::MAX_FOG_LAYERS`).
pub const FOG_MAX_LAYERS: usize = 4;
/// Byte offset of the layers in the fog uniform buffer.
pub const FOG_LAYERS_OFFSET: u64 = size_of::<FogHeader>() as u64;
/// Bytes of the whole fog uniform buffer: header and layers.
pub const FOG_BUFFER_SIZE: u64 =
    FOG_LAYERS_OFFSET + (FOG_MAX_LAYERS * size_of::<FogLayerGpu>()) as u64;
// GL 3.3 guarantees 16 KiB per uniform block.
const _: () = assert!(FOG_BUFFER_SIZE <= 16384);

/// The per-frame header of the uniform `Fog` (`fog.wgsl`, 112 bytes, std140: only vec4
/// members). The layers ([`FogLayerGpu`]) follow it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct FogHeader {
    /// xyz = ray direction at NDC (0, 0), scaled so `dot(dir, forward) = 1`; w = near plane.
    pub ray0: [f32; 4],
    /// xyz = change of the direction per NDC unit along x; w = full target width in pixels.
    pub ray_x: [f32; 4],
    /// xyz = same along y; w = full target height in pixels.
    pub ray_y: [f32; 4],
    /// xyz = eye relative to the render origin, w = 0.
    pub eye: [f32; 4],
    /// x = layer count (sorted by height), y = flags ([`fog::FLAG_HALF`]), z = step budget,
    /// w = 0.
    pub counts: [u32; 4],
    /// rgb = sky color (where layers fade to), a = 0.
    pub sky: [f32; 4],
    /// xyz = first cell of the debug coverage grid (`fs_fog_debug_coverage`), w = 0.
    pub debug: [i32; 4],
}

/// One layer of the uniform `Fog` (208 bytes, std140). See `fog.wgsl` for each word.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct FogLayerGpu {
    /// x, y = band `[min, max)` relative to the render origin's y; z = reach (blocks along the
    /// ray); w = mean extinction per block (`log2` units).
    pub band: [f32; 4],
    /// x, y = fade start and end distance; z = extinction per block of a full cell (`log2`).
    pub fade: [f32; 4],
    /// xyz = min corner of the origin's cell relative to the render origin (blocks).
    pub grid: [f32; 4],
    /// xyz = cell size in blocks.
    pub size: [f32; 4],
    /// rgb = color of a lit cell.
    pub lit: [f32; 4],
    /// Face shades: top, bottom, x, z.
    pub face: [f32; 4],
    /// xyz = offset of the displaced origin cell in its lattice step; w = log2 of the step.
    pub lattice: [u32; 4],
    /// xyz = low words of its lattice point, w = 0.
    pub base_lo: [u32; 4],
    /// xyz = high words; w = seed of the first octave.
    pub base_hi: [u32; 4],
    /// x = 0, y = seed of the second octave, z = octaves, w = flags
    /// ([`fog::LAYER_VOLUME`], [`fog::LAYER_UNIFORM`]).
    pub octave2: [u32; 4],
    /// x = coverage threshold (0..=4096), y = coverage gain (q8), zw = 0.
    pub shape: [u32; 4],
    /// xyz = the wind's fraction of a cell (1/4096), w = 0.
    pub wind: [i32; 4],
    /// x, y = band cells `[first, end)` relative to the origin's cell, zw = 0.
    pub cells: [i32; 4],
}

/// Flags of the fog uniform.
pub mod fog {
    /// [`super::FogHeader::counts`] y: the march runs at half resolution.
    pub const FLAG_HALF: u32 = 1;
    /// [`super::FogLayerGpu::octave2`] w: 3D noise.
    pub const LAYER_VOLUME: u32 = 1;
    /// [`super::FogLayerGpu::octave2`] w: every cell is full (the march takes a chord whole).
    pub const LAYER_UNIFORM: u32 = 2;
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
    /// Cull pipeline group 0: view (0), chunks (1), draw args (2), draw refs (3), cull state (4:
    /// the two draw counts, opaque and cutout, as `atomic<u32>`, then one visibility bit per
    /// chunk slot from byte [`CULL_VISIBLE_OFFSET`], ADR 0022).
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
    /// Byte offset of the per-slot visibility bits in the cull state buffer (after the two draw
    /// counts).
    pub const CULL_VISIBLE_OFFSET: u64 = 8;
    /// Fog march: the main depth (`texture_2d<f32>`, unfilterable) in this group...
    pub const FOG_GROUP_DEPTH: u32 = 0;
    /// ... at this binding.
    pub const FOG_BINDING_DEPTH: u32 = 0;
    /// Fog composite: the fog target (`texture_2d<u32>`) in this group...
    pub const FOG_GROUP_TARGET: u32 = 0;
    /// ... at this binding.
    pub const FOG_BINDING_TARGET: u32 = 1;
    /// Fog march and debug: the `Fog` uniform in this group...
    pub const FOG_GROUP_UNIFORM: u32 = 1;
    /// ... at this binding.
    pub const FOG_BINDING_UNIFORM: u32 = 0;
}
