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
    /// w = the first height of the debug's edge weights (`f32` bits, 0 for the march).
    pub counts: [u32; 4],
    /// rgb = sky color (where layers fade to), a = 0.
    pub sky: [f32; 4],
    /// xyz = first cell of the debug coverage grid (`fs_fog_debug_coverage`), w = the point
    /// inside each cell it reads (1/4096 cells per axis); for `fs_fog_debug_walk`, xyz = the
    /// walks' first cell, w = their axis and sign (0 to 5: +x, -x, +y, -y, +z, -z).
    pub debug: [i32; 4],
}

/// One layer of the uniform `Fog` (256 bytes, std140). See `fog.wgsl` for each word.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct FogLayerGpu {
    /// x, y = band `[min, max)` relative to the render origin's y; z = reach (blocks along the
    /// ray: where the haze ends, or the cells of a layer without haze); w = where the cells end.
    pub band: [f32; 4],
    /// x, y = fade start and end distance; z = extinction per block of a full cell (`log2`);
    /// w = where the cells start to blend into the haze (`band` w: no blend).
    pub fade: [f32; 4],
    /// xyz = min corner of the origin's cell relative to the render origin (blocks); w = the
    /// haze (the fraction of the profile past the cells).
    pub grid: [f32; 4],
    /// xyz = cell size in blocks; w = blocks between the profile's knots (0: no haze).
    pub size: [f32; 4],
    /// rgb = color of a lit cell; w = height of the profile's first knot relative to the render
    /// origin.
    pub lit: [f32; 4],
    /// Face shades: top, bottom, x, z.
    pub face: [f32; 4],
    /// x, y = distance where the soft fog starts to blend into the cells and where it is all
    /// cells (0, 0: no soft fog); z, w = brightness at the bottom and the top of the band.
    pub soft: [f32; 4],
    /// xyz = offset of the displaced origin cell in its lattice step; w = log2 of the step.
    pub lattice: [u32; 4],
    /// xyz = low words of its lattice point, w = 0.
    pub base_lo: [u32; 4],
    /// xyz = high words; w = seed of the first octave.
    pub base_hi: [u32; 4],
    /// x = 0, y = seed of the second octave, z = octaves, w = flags
    /// ([`fog::LAYER_VOLUME`], [`fog::LAYER_UNIFORM`]).
    pub octave2: [u32; 4],
    /// x = coverage threshold at the band's bottom (0..=4096), y = coverage gain (q8), z = rise
    /// of the threshold to the band's top (0..=4096), w = cell rows of the band.
    pub shape: [u32; 4],
    /// xyz = the wind's fraction of a cell (1/4096), w = 0.
    pub wind: [i32; 4],
    /// x, y = band cells `[first, end)` relative to the origin's cell; z, w = depth of the
    /// bottom and the top edge in blocks (0: none).
    pub cells: [i32; 4],
    /// The profile's mean extinction per block (`log2` units, at a haze of 1) at knots 0 to 3
    /// (`boxel_fog::HazeProfile`).
    pub profile_a: [f32; 4],
    /// The same at knots 4 to 7.
    pub profile_b: [f32; 4],
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

/// Immediate data of the far LOD pipeline (16 bytes, `lod.wgsl`, ADR 0031): the node one draw
/// shows. Bits in [`lod`].
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct LodImmediate {
    /// x = the node's world block origin x, low 32 bits (like [`ChunkGpu::origin`]), OR its
    /// level (a level-`k` origin has `5 + k` zero low bits); y, z = origin y and z (low 32
    /// bits); w = hidden octants | color mip << [`lod::MIP_SHIFT`] | [`lod::TINT_BIT`].
    pub node: [i32; 4],
}

impl LodImmediate {
    /// The immediates of a level-`level` node at `origin` (low 32 bits of the world origin, its
    /// low `5 + level` bits zero) whose octants `hidden` are hidden (bit `x | y << 1 | z << 2`),
    /// colored with mip `mip` of each texture layer (its 1x1 level), tinted per level with
    /// `tint`.
    pub fn new(origin: [i32; 3], level: u8, hidden: u8, mip: u32, tint: bool) -> Self {
        debug_assert!(u32::from(level) <= lod::LEVEL_MASK && level > 0);
        debug_assert_eq!(
            origin[0] & lod::LEVEL_MASK as i32,
            0,
            "a node origin's low bits"
        );
        let w = u32::from(hidden)
            | ((mip & lod::MIP_MASK) << lod::MIP_SHIFT)
            | if tint { lod::TINT_BIT } else { 0 };
        Self {
            node: [origin[0] | i32::from(level), origin[1], origin[2], w as i32],
        }
    }
}

/// One node of the far LOD's node table on High (`lod_pull.wgsl`, `lod_cull.wgsl`, ADR 0031): the
/// node as [`LodImmediate`] has it (plus [`lod::LIVE_BIT`] and the [`lod::ACROSS_SHIFT`] bits),
/// its first quad in the LOD arena, its 6 face bucket counts, two per word (bucket `2i` in the
/// low 16 bits of `counts[i]`), and the seam quads at the end of each bucket. 64 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct LodNodeGpu {
    /// See [`LodImmediate::node`]; w also has [`lod::LIVE_BIT`] when the cull may draw it, and
    /// bit [`lod::ACROSS_SHIFT`]` + f` when the node across face bucket `f` covers its seam.
    pub node: [i32; 4],
    /// First quad of face bucket 0 in the LOD arena (the buckets follow in order).
    pub quad_base: u32,
    /// Face bucket quads, two per word: bucket `2i` in bits 0-15 of word `i`, `2i + 1` in bits
    /// 16-31 (a face bucket has at most `boxel_mesh::MAX_QUADS_PER_FACE_BUCKET` quads).
    pub counts: [u32; 3],
    /// Per face bucket, the quads at its end that only close a seam
    /// (`boxel_mesh::lod::seam_counts`): those on a node face in bits 0-15, then those on a
    /// middle plane, the last ones, in bits 16-31 ([`lod::bucket_quads`]).
    pub seams: [u32; 6],
    /// Padding to the WGSL struct's 64 bytes.
    pub pad: [u32; 2],
}

impl LodNodeGpu {
    /// The table entry of a node drawn as `immediate`, its quads from `quad_base` with `faces`
    /// quads per face bucket (each below 2^16), live (the cull may draw it) if `live`.
    pub fn new(immediate: LodImmediate, quad_base: u32, faces: [u32; 6], live: bool) -> Self {
        debug_assert!(
            faces.iter().all(|&n| n <= 0xFFFF),
            "a face bucket over 2^16"
        );
        let mut node = immediate.node;
        if live {
            node[3] |= lod::LIVE_BIT as i32;
        }
        let pair = |i: usize| (faces[2 * i] & 0xFFFF) | (faces[2 * i + 1] & 0xFFFF) << 16;
        Self {
            node,
            quad_base,
            counts: [pair(0), pair(1), pair(2)],
            seams: [0; 6],
            pad: [0; 2],
        }
    }

    /// The entry with `seams` (per face bucket, `[on a node face, on a middle plane]`, within
    /// its count) and `across` (bit `f`: the node of the same level across face bucket `f` draws
    /// its cells by that face, [`lod::ACROSS_SHIFT`]).
    pub fn with_seams(mut self, seams: [[u32; 2]; 6], across: u8) -> Self {
        debug_assert!(
            seams.iter().flatten().all(|&n| n <= 0xFFFF),
            "a seam run over 2^16"
        );
        self.seams = seams.map(|[face, middle]| (face & 0xFFFF) | (middle & 0xFFFF) << 16);
        self.node[3] |= i32::from(across & 0x3F) << lod::ACROSS_SHIFT;
        self
    }
}

/// Bits of [`LodImmediate`] and [`LodNodeGpu`] (mirrored by `lod_common.wgsl` and
/// `lod_cull.wgsl`).
pub mod lod {
    /// The level in the low bits of the origin's x.
    pub const LEVEL_MASK: u32 = 15;
    /// The color mip in word w.
    pub const MIP_SHIFT: u32 = 8;
    /// Its bits.
    pub const MIP_MASK: u32 = 31;
    /// Word w: tint the node with its level's color.
    pub const TINT_BIT: u32 = 1 << 13;
    /// Where a quad's octant bits are in its `geo` word (x, y, z): bit 4 of each coordinate of
    /// its min cell (`boxel_mesh::lod::octant_of`).
    pub const OCTANT_SHIFTS: [u32; 3] = [4, 9, 14];
    /// Immediate bytes of the LOD pipeline.
    pub const IMMEDIATE_SIZE: u32 = 16;
    /// Word w of a [`super::LodNodeGpu`]: the High cull may draw the node (its quads are in the
    /// arena and it lies inside the GPU window).
    pub const LIVE_BIT: u32 = 1 << 14;
    /// Word w of a [`super::LodNodeGpu`], bit `ACROSS_SHIFT + f` (`f` a face bucket): the node
    /// of the same level across face `f` is drawn, with its octants by that face, so the seam
    /// quads of this node on that face lie between solid cells ([`bucket_quads`]).
    pub const ACROSS_SHIFT: u32 = 15;

    /// The quads of face bucket `face` (`count` of them, `seams` = `[on a node face, on a middle
    /// plane]` at its end) the LOD draws for a node whose octants `hidden` are hidden:
    /// all of them if an octant is drawn while the one across the middle plane behind face
    /// `face` is hidden (the middle seam shows there, the others are kept to stay one run),
    /// else neither the middle seam quads nor, with `across`, the node face ones. The instanced
    /// draws are cut so on the CPU; `lod_cull.wgsl` mirrors it on High.
    pub fn bucket_quads(count: u32, seams: [u32; 2], face: usize, hidden: u8, across: bool) -> u32 {
        let (bit, low) = match face / 2 {
            0 => (2, 0x33u8),
            1 => (1, 0x55),
            _ => (4, 0x0F),
        };
        let drawn = !hidden;
        let exposed = if face.is_multiple_of(2) {
            drawn & (hidden >> bit) & low
        } else {
            drawn & (hidden << bit) & !low
        };
        if exposed != 0 {
            return count;
        }
        let [on_face, middle] = seams;
        count.saturating_sub(middle + if across { on_face } else { 0 })
    }
    /// Node slots per segment of the High draw lists: slot `s` writes into list
    /// `s / SEGMENT_SLOTS`, one draw per face bucket at most.
    pub const SEGMENT_SLOTS: u32 = 10922;
    /// Draws one segment's list holds: 6 per slot, within
    /// [`super::bindings::MAX_DRAWS_PER_LIST`], so a list never overflows.
    pub const SEGMENT_DRAWS: u32 = 6 * SEGMENT_SLOTS;
    /// Group of the LOD geometry in `lod_pull.wgsl` (LOD arena at binding 0, node table at
    /// binding 1). Group 2 stays the main geometry, which the translucent buckets drawn after
    /// the LOD still use on High.
    pub const GROUP_GEOMETRY: u32 = 3;
    /// Workgroup size of `cull_lod`.
    pub const CULL_WORKGROUP_SIZE: u32 = 64;
    /// Workgroup edge of `hiz_reduce` (8 x 8).
    pub const HIZ_WORKGROUP_EDGE: u32 = 8;
}

const _: () = assert!(lod::SEGMENT_DRAWS <= bindings::MAX_DRAWS_PER_LIST);
const _: () = assert!(size_of::<LodNodeGpu>() == 64);

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
