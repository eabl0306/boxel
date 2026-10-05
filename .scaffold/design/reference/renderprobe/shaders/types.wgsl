// boxel/crates/boxel_render/src/shaders/types.wgsl
// Shared GPU types. Prepended to every boxel terrain/cull module.
// Every struct here has a #[repr(C)] Pod mirror in boxel_render::gpu_types and a naga layout test.

// Per-frame camera data. 224 bytes, uniform-buffer (std140) compatible: only vec4/mat4/scalars.
struct View {
    view_proj: mat4x4<f32>,            // reverse-Z, infinite far, camera-relative (render origin)
    camera_pos: vec4<f32>,             // xyz: camera position relative to the render origin
    frustum: array<vec4<f32>, 6>,      // inward planes: dot(n, p) + d >= 0 means inside; [5] is a far
                                       // plane at view distance (needed because the projection is infinite)
    fog_color: vec4<f32>,
    sun_dir: vec4<f32>,                // xyz normalized, w unused (reserved for M2 lighting)
    fog_start: f32,
    fog_end: f32,
    chunk_count: u32,                  // number of chunk slots the cull pass must visit
    max_draws: u32,                    // capacity of draw_args/draw_meta (High tier)
};

// One render chunk slot. 64 bytes, storage-buffer layout.
struct ChunkGpu {
    origin: vec3<i32>,                 // voxel-space min corner relative to the render origin
    lod: u32,                          // voxel scale = 1 << lod (M0: always 0)
    quad_base: u32,                    // index of the first PackedQuad of this chunk in the quad arena
    translucent_count: u32,            // quads in the translucent bucket (stored after the 6 face buckets)
    flags: u32,                        // bit 0 = LIVE
    pad0: u32,
    face_count: array<u32, 6>,         // quads per face bucket, buckets stored consecutively from quad_base
    pad1: vec2<u32>,
};

const CHUNK_FLAG_LIVE: u32 = 1u;
const CHUNK_SIZE: f32 = 32.0;
