// boxel: shared voxel shader code (all tiers). Prepended to voxel_pull.wgsl, voxel_instanced.wgsl and cull.wgsl.
//
// PackedQuad = vec2<u32>(geo, mat), 8 bytes, identical bytes on every tier.
//   geo: x:5 | y:5 | z:5 | (w-1):5 | (h-1):5 | face:3 | reserved:4
//   mat: layer:12 | ao:8 (2 bits per corner, 3 = unoccluded) | sky:4 | block:4 | flags:4
// face: 0=+Y 1=-Y 2=+X 3=-X 4=+Z 5=-Z  (u,v axes: ±Y -> (X,Z), ±X -> (Z,Y), ±Z -> (X,Y))
//
// RULES (enforced by boxel_shaders tests):
//  - never use @builtin(instance_index), @builtin(draw_index), @builtin(num_workgroups);
//    per-draw data arrives through the DrawRef instance-step vertex attribute or immediates.
//  - @builtin(vertex_index) is only used with first_vertex = 0 / base_vertex = 0 draws.
//  - textureLoad mip levels are always i32 (naga GLSL bug gfx-rs/wgpu#10037).
//
// World integer positions are i64 on the CPU. Chunk origins and render_origin arrive as their
// low 32 bits (two's complement wrapping) and are only ever subtracted from each other in i32.
// WGSL integer arithmetic wraps, so (chunk_origin - render_origin) is the true difference modulo
// 2^32: exact whenever the chunk is less than 2^31 voxels from the camera, which every live
// chunk is, at any world position.

const GEO_POS_MASK: u32 = 31u;
const GEO_Y_SHIFT: u32 = 5u;
const GEO_Z_SHIFT: u32 = 10u;
const GEO_W_SHIFT: u32 = 15u;
const GEO_H_SHIFT: u32 = 20u;
const GEO_FACE_SHIFT: u32 = 25u;
const MAT_LAYER_MASK: u32 = 0xFFFu;
const MAT_AO_SHIFT: u32 = 12u;
const MAT_SKY_SHIFT: u32 = 20u;
const MAT_BLOCK_SHIFT: u32 = 24u;
const MAT_FLAGS_SHIFT: u32 = 28u;
const CHUNK_SIZE: f32 = 32.0;

struct View {
  view_proj: mat4x4<f32>,          // camera-relative: positions are (world - render_origin)
  frustum: array<vec4<f32>, 6>,    // xyz = inward normal, w = d; inside if dot(n, p) + d >= 0
  render_origin: vec4<i32>,        // xyz = integer voxel origin of the camera-relative frame (low 32 bits)
  camera_rel: vec4<f32>,           // xyz = eye position relative to render_origin
  fog_color: vec4<f32>,
  fog_params: vec4<f32>,           // x = fog start, y = fog end (world units)
  counts: vec4<u32>,               // x = chunk slot count, y = opaque list capacity (= cutout list base), z = cutout list capacity (High)
};

struct ChunkGpu {                  // 64 bytes, mirrors boxel_shaders::gpu::ChunkGpu
  origin: vec3<i32>,               // world voxel min corner of the chunk, low 32 bits (wraps; only used minus render_origin)
  quad_base: u32,                  // first quad of bucket 0 in the quad arena
  counts: array<u32, 8>,           // [0..6) opaque face buckets, [6] cutout, [7] translucent
  flags: u32,                      // bit0 = live
  pad0: u32,
  pad1: u32,
  pad2: u32,
};

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) layer: u32,
  @location(2) shade: f32,
  @location(3) view_dist: f32,
};

struct Decoded { local: vec3<f32>, uv: vec2<f32>, face: u32, layer: u32, shade: f32 };

fn face_normal(f: u32) -> vec3<f32> {
  switch f {
    case 0u: { return vec3(0.0, 1.0, 0.0); }
    case 1u: { return vec3(0.0, -1.0, 0.0); }
    case 2u: { return vec3(1.0, 0.0, 0.0); }
    case 3u: { return vec3(-1.0, 0.0, 0.0); }
    case 4u: { return vec3(0.0, 0.0, 1.0); }
    default: { return vec3(0.0, 0.0, -1.0); }
  }
}
fn face_u(f: u32) -> vec3<f32> { if (f == 2u || f == 3u) { return vec3(0.0, 0.0, 1.0); } return vec3(1.0, 0.0, 0.0); }
fn face_v(f: u32) -> vec3<f32> { if (f < 2u) { return vec3(0.0, 0.0, 1.0); } return vec3(0.0, 1.0, 0.0); }
fn face_shade(f: u32) -> f32 {
  switch f {
    case 0u: { return 1.0; }
    case 1u: { return 0.5; }
    case 2u, 3u: { return 0.8; }
    default: { return 0.65; }
  }
}

// corner 0..3 = (u,v) (0,0) (1,0) (0,1) (1,1); index pattern per quad = [0,1,2, 2,1,3] (strip order 0,1,2,3)
fn decode_quad(q: vec2<u32>, corner_in: u32) -> Decoded {
  let geo = q.x;
  let mat = q.y;
  let p = vec3<f32>(f32(geo & GEO_POS_MASK), f32((geo >> GEO_Y_SHIFT) & GEO_POS_MASK), f32((geo >> GEO_Z_SHIFT) & GEO_POS_MASK));
  let w = f32(((geo >> GEO_W_SHIFT) & GEO_POS_MASK) + 1u);
  let h = f32(((geo >> GEO_H_SHIFT) & GEO_POS_MASK) + 1u);
  let f = (geo >> GEO_FACE_SHIFT) & 7u;
  let n = face_normal(f);
  let u = face_u(f);
  let v = face_v(f);
  var c = corner_in;
  // keep counter-clockwise winding seen from outside: swap corners 1 and 2 when (u x v) points inward
  if (dot(cross(u, v), n) < 0.0) { if (c == 1u) { c = 2u; } else if (c == 2u) { c = 1u; } }
  let cu = f32(c & 1u);
  let cv = f32(c >> 1u);
  var o: Decoded;
  o.local = p + max(n, vec3(0.0)) + u * (cu * w) + v * (cv * h);
  o.uv = select(vec2(cu * w, cv * h), vec2(cu * w, (1.0 - cv) * h), f >= 2u); // side faces: v points down in texture space
  o.face = f;
  o.layer = mat & MAT_LAYER_MASK;
  let ao = f32((mat >> (MAT_AO_SHIFT + 2u * c)) & 3u) / 3.0;
  let sky = f32((mat >> MAT_SKY_SHIFT) & 15u) / 15.0;
  let blk = f32((mat >> MAT_BLOCK_SHIFT) & 15u) / 15.0;
  o.shade = face_shade(f) * mix(0.45, 1.0, ao) * (max(sky, blk) * 0.85 + 0.15);
  return o;
}

fn finish_vertex(d: Decoded, chunk_origin: vec3<i32>, view: View) -> VsOut {
  // wrapping i32 subtraction first: exact for any world position (see the top), then small floats
  let rel = vec3<f32>(chunk_origin - view.render_origin.xyz) + d.local;
  var o: VsOut;
  o.clip = view.view_proj * vec4(rel, 1.0);
  o.uv = d.uv;
  o.layer = d.layer;
  o.shade = d.shade;
  o.view_dist = length(rel - view.camera_rel.xyz);
  return o;
}

// Opaque buckets never discard (keeps early-Z); cutout buckets use the alpha test below.
fn alpha_test_passes(albedo: vec4<f32>) -> bool { return albedo.a >= 0.5; }

fn shade_fragment(albedo: vec4<f32>, i: VsOut, view: View) -> vec4<f32> {
  let fog = clamp((i.view_dist - view.fog_params.x) / max(view.fog_params.y - view.fog_params.x, 1.0), 0.0, 1.0);
  return vec4(mix(albedo.rgb * i.shade, view.fog_color.rgb, fog), 1.0);
}
