// boxel: what the sun shadow modules share (ADR 0065, High only). Appended after common.wgsl and
// sun_off.wgsl to shadow_cull.wgsl and shadow.wgsl.
//
// The shadow uniform is MAX_SHADOW_CASCADES entries of ShadowCascade, 256 bytes each (the
// dynamic offset step of the shadow draws' group 3, the largest uniform offset alignment wgpu
// allows): the cull binds them all, a cascade's draws bind its own entry. Entry 0 also holds
// what the frame shares (counts, band, light), written the same into every entry.
//
// A cascade's map: view_proj takes a camera-relative position (relative to View::render_origin)
// to x, y in -1..1 across the sun (y up) and depth 0 on the sun's side of its box, 1 on the far
// side. The maps are cleared to 1 and hold the nearest caster; a receiver is lit where its depth
// is at most the map's (LessEqual).

const MAX_SHADOW_CASCADES: u32 = 4u;

struct ShadowCascade {             // 256 bytes, mirrors boxel_shaders::gpu::shadow::ShadowCascadeGpu
  view_proj: mat4x4<f32>,          // camera-relative position to the cascade's map
  planes: array<vec4<f32>, 6>,     // its box: xyz = inward normal, w = d (camera-relative), left, right, bottom, top, sun side, far side
  params: vec4<f32>,               // x = texel size in blocks, y = the cutout alpha test's mip level, z, w = the view distances it serves [start, end)
  light: vec4<f32>,                // xyz = unit direction toward the sun, w = where the shadows end (view distance)
  counts: vec4<u32>,               // x = cascades this frame, y = the face buckets that cast (bit f), z = chunks a page's list of one cascade holds (C), w = pages with lists (L)
  band: vec4<i32>,                 // x, y = the camera realm's voxels [x, y) in y relative to View::render_origin.y (saturated), z = the camera realm
  pad: array<vec4<u32>, 2>,
};
