// boxel: shared voxel shader code (all tiers). Prepended to voxel_pull.wgsl, voxel_instanced.wgsl and cull.wgsl.
//
// PackedQuad = vec2<u32>(geo, mat), 8 bytes, identical bytes on every tier.
//   geo: x:5 | y:5 | z:5 | (w-1):5 | (h-1):5 | face:3 | flags:4 (none defined yet)
//   mat: layer:12 | ao:8 (2 bits per corner, 3 = unoccluded) | light:12 (index into the chunk's
//        light palette)
// face: 0=+Y 1=-Y 2=+X 3=-X 4=+Z 5=-Z  (u,v axes: ±Y -> (X,Z), ±X -> (Z,Y), ±Z -> (X,Y))
//
// Light (ADR 0015): a palette entry is vec2<u32>(c0 | c1 << 16, c2 | c3 << 16), the light of
// the quad's 4 corners in the AO corner order. Each corner is a LightValue: sky:4 | red:4 |
// green:4 | blue:4 (sky in the high nibble). High and Mid read the entry from the quad arena at
// ChunkGpu::light_base + index; Low gets it already expanded per quad as a second instance
// attribute. The fragment interpolates the 4 corners bilinearly at the quad-local (s, t), the
// same on every tier.
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
// 2^32: exact whenever the chunk is less than 2^31 voxels from the camera, at any world position.
// Resident chunks can be farther (the old area right after a teleport); the renderer never draws
// those: only chunks within about 2^30 voxels of the camera are drawn (boxel_render chunk_table).
// The CPU sets the live flag only on them (High culls only live chunks), and the CPU cull of Mid
// and Low skips the rest.

const GEO_POS_MASK: u32 = 31u;
const GEO_Y_SHIFT: u32 = 5u;
const GEO_Z_SHIFT: u32 = 10u;
const GEO_W_SHIFT: u32 = 15u;
const GEO_H_SHIFT: u32 = 20u;
const GEO_FACE_SHIFT: u32 = 25u;
const GEO_FLAGS_SHIFT: u32 = 28u;
const MAT_LAYER_MASK: u32 = 0xFFFu;
const MAT_AO_SHIFT: u32 = 12u;
const MAT_LIGHT_SHIFT: u32 = 20u;
const MAT_LIGHT_MASK: u32 = 0xFFFu;
const LIGHT_SKY_SHIFT: u32 = 12u;
const LIGHT_RED_SHIFT: u32 = 8u;
const LIGHT_GREEN_SHIFT: u32 = 4u;
const LIGHT_BLUE_SHIFT: u32 = 0u;
const LIGHT_MAX_LEVEL: f32 = 15.0;
// A light palette entry with sky 15 at the 4 corners (boxel_mesh::FULL_SKY_ENTRY).
const FULL_SKY_LIGHT: vec2<u32> = vec2<u32>(0xF000F000u, 0xF000F000u);
const CHUNK_SIZE: f32 = 32.0;

struct View {
  view_proj: mat4x4<f32>,          // camera-relative: positions are (world - render_origin)
  frustum: array<vec4<f32>, 6>,    // xyz = inward normal, w = d; inside if dot(n, p) + d >= 0
  render_origin: vec4<i32>,        // xyz = integer voxel origin of the camera-relative frame (low 32 bits)
  camera_rel: vec4<f32>,           // xyz = eye position relative to render_origin
  fog_color: vec4<f32>,
  fog_params: vec4<f32>,           // x = fog start, y = fog end (world units)
  counts: vec4<u32>,               // x = chunk slot count, y = opaque list capacity (= cutout list base), z = cutout list capacity (High)
  sky_light: vec4<f32>,            // rgb = sky light color times the daylight intensity (linear), a = ambient floor
  light_params: vec4<f32>,         // x = AO strength (0.55: the darkest corner keeps 45 %), y = block-light gain, zw = 0
  light_curve: array<vec4<f32>, 4>, // brightness of light levels 0..15 (level 4k + j at [k][j]; level 15 = 1)
};

struct ChunkGpu {                  // 64 bytes, mirrors boxel_shaders::gpu::ChunkGpu
  origin: vec3<i32>,               // world voxel min corner of the chunk, low 32 bits (wraps; only used minus render_origin)
  quad_base: u32,                  // first quad of bucket 0 in the quad arena
  counts: array<u32, 8>,           // [0..6) opaque face buckets, [6] cutout, [7] translucent
  flags: u32,                      // bit0 = live
  light_base: u32,                 // first light palette entry in the quad arena (= quad_base + total quads)
  light_len: u32,                  // light palette entries (>= 1); a quad index not below it reads open sky
  pad: u32,
};

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) layer: u32,
  @location(2) @interpolate(flat) face: u32,
  @location(3) view_dist: f32,
  @location(4) st: vec2<f32>,                       // quad-local (s, t): corner k sits at (k & 1, k >> 1)
  @location(5) @interpolate(flat) light: vec2<u32>, // the quad's light palette entry (4 corner lights)
  @location(6) @interpolate(flat) ao: u32,          // the quad's AO byte (corner k at bits 2k..2k+2)
};

struct Decoded {
  local: vec3<f32>,
  uv: vec2<f32>,
  st: vec2<f32>,
  face: u32,
  layer: u32,
  light: vec2<u32>,
  ao: u32,
};

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
// `light` is the quad's light palette entry (High/Mid: read from the arena; Low: an instance attribute).
fn decode_quad(q: vec2<u32>, light: vec2<u32>, corner_in: u32) -> Decoded {
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
  o.st = vec2(cu, cv);
  o.face = f;
  o.layer = mat & MAT_LAYER_MASK;
  o.light = light;
  o.ao = (mat >> MAT_AO_SHIFT) & 0xFFu;
  return o;
}

fn finish_vertex(d: Decoded, chunk_origin: vec3<i32>, view: View) -> VsOut {
  // wrapping i32 subtraction first: exact for any world position (see the top), then small floats
  let rel = vec3<f32>(chunk_origin - view.render_origin.xyz) + d.local;
  var o: VsOut;
  o.clip = view.view_proj * vec4(rel, 1.0);
  o.uv = d.uv;
  o.layer = d.layer;
  o.face = d.face;
  o.view_dist = length(rel - view.camera_rel.xyz);
  o.st = d.st;
  o.light = d.light;
  o.ao = d.ao;
  return o;
}

// Opaque buckets never discard (keeps early-Z); cutout buckets use the alpha test below.
fn alpha_test_passes(albedo: vec4<f32>) -> bool { return albedo.a >= 0.5; }

// Light levels (sky, red, green, blue) of corner k of a light palette entry.
fn corner_light(words: vec2<u32>, k: u32) -> vec4<f32> {
  let c = select(words.x, words.y, k >= 2u) >> (16u * (k & 1u));
  let shifts = vec4(LIGHT_SKY_SHIFT, LIGHT_RED_SHIFT, LIGHT_GREEN_SHIFT, LIGHT_BLUE_SHIFT);
  return vec4<f32>((vec4(c) >> shifts) & vec4(15u));
}

// Bilinear interpolation of 4 corner values at (s, t), corner k at (k & 1, k >> 1). Written as
// a + (b - a) * s so that 4 equal corners give exactly their value.
fn bilinear(c0: vec4<f32>, c1: vec4<f32>, c2: vec4<f32>, c3: vec4<f32>, st: vec2<f32>) -> vec4<f32> {
  let near = c0 + (c1 - c0) * st.x;
  let far = c2 + (c3 - c2) * st.x;
  return near + (far - near) * st.y;
}

// Brightness of a fractional light level (0..15): linear interpolation in View::light_curve,
// indexed in the uniform itself (a function-local copy of the array, dynamically indexed, may
// live in scratch memory). Every shader that includes this file binds the View uniform as
// `view`.
fn level_curve(l: f32) -> f32 {
  let x = clamp(l, 0.0, LIGHT_MAX_LEVEL);
  let i = min(u32(x), 14u);
  let j = i + 1u;
  return mix(view.light_curve[i >> 2u][i & 3u], view.light_curve[j >> 2u][j & 3u], x - f32(i));
}

// The light reaching a fragment: the 4 corner lights interpolated per channel at (s, t), each
// channel through the brightness curve, then the brightest of sky light (times the sky color),
// block light (times its gain) and the ambient floor.
fn voxel_light(i: VsOut, view: View) -> vec3<f32> {
  let l = bilinear(corner_light(i.light, 0u), corner_light(i.light, 1u), corner_light(i.light, 2u), corner_light(i.light, 3u), i.st);
  let sky = level_curve(l.x) * view.sky_light.rgb;
  let block = vec3(level_curve(l.y), level_curve(l.z), level_curve(l.w)) * view.light_params.y;
  return max(max(sky, block), vec3(view.sky_light.a));
}

// Ambient occlusion factor: the 4 corner AO values (0..3, 3 = open) interpolated at (s, t), from
// 1 - strength (fully occluded) to 1 (open).
fn ao_factor(i: VsOut, view: View) -> f32 {
  let a = vec4<f32>((vec4(i.ao) >> vec4(0u, 2u, 4u, 6u)) & vec4(3u));
  let near = a.x + (a.y - a.x) * i.st.x;
  let far = a.z + (a.w - a.z) * i.st.x;
  return mix(1.0 - view.light_params.x, 1.0, (near + (far - near) * i.st.y) / 3.0);
}

// albedo * face shade * AO * light, then fog. At sky 15 under a white noon sky with AO open
// this is albedo * face shade: the look before light.
fn shade_fragment(albedo: vec4<f32>, i: VsOut, view: View) -> vec4<f32> {
  let lit = albedo.rgb * (face_shade(i.face) * ao_factor(i, view)) * voxel_light(i, view);
  let fog = clamp((i.view_dist - view.fog_params.x) / max(view.fog_params.y - view.fog_params.x, 1.0), 0.0, 1.0);
  return vec4(mix(lit, view.fog_color.rgb, fog), 1.0);
}
