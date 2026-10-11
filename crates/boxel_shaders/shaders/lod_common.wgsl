// Far LOD nodes (ADR 0031): what both LOD pipelines share, prepended with common.wgsl to
// lod.wgsl (instanced, every tier) and lod_pull.wgsl (GPU-driven, High).
//
// A node is a 32^3 grid of cells of 2^k blocks (level k). Its quads are ordinary PackedQuads in
// cells (cubes only, no light palette, AO open). The vertex shader scales a quad to blocks and
// collapses the quads of the octants finer detail draws; the fragment shader colors a face with
// the 1x1 mip of its texture layer (the texture's mean, tint included), lit as open sky, fogged
// like the terrain. Both pipelines run these same functions on the same quads, so they draw the
// same pixels.
//
// Exactly ONE uniform block (View) and ONE texture, no sampler (textureLoad), like Low (ADR 0011).
//
// A node (LodImmediate on every tier, LodNodeGpu::node on High; boxel_shaders::gpu): x = node
// origin x, low 32 bits, OR the level in bits 0-3 (a level-k origin has 5 + k zero low bits);
// y, z = node origin y, z (low 32 bits); w = hidden octants (bits 0-7, bit x | y << 1 | z << 2) |
// color mip (bits 8-12) | level tint (bit 13) | live (bit 14, the High cull only).
const LOD_LEVEL_MASK: u32 = 15u;
const LOD_MIP_SHIFT: u32 = 8u;
const LOD_MIP_MASK: u32 = 31u;
const LOD_TINT_BIT: u32 = 0x2000u;
// The octant of a quad: bit 4 of each of its min cell's coordinates in geo (boxel_mesh::lod::octant_of).
const LOD_OCTANT_X_SHIFT: u32 = 4u;
const LOD_OCTANT_Y_SHIFT: u32 = 9u;
const LOD_OCTANT_Z_SHIFT: u32 = 14u;
// LodOut::bits of a LOD quad: the texture layer (bits 0-11), its face (12-14), then the color
// mip, the tint flag, the emissive flag and the level (flat, for the fragment).
const LOD_OUT_FACE_SHIFT: u32 = 12u;
const LOD_OUT_MIP_SHIFT: u32 = 16u;
const LOD_OUT_TINT_BIT: u32 = 0x200000u;
const LOD_OUT_EMISSIVE_BIT: u32 = 0x400000u;
const LOD_OUT_LEVEL_SHIFT: u32 = 24u;

// What the vertex stage hands the fragment: a LOD quad is an opaque cube face lit as open sky
// with AO open, so only its face, texture and flags and the distance for the fog are needed (6
// values where the terrain's VsOut has 14: the vertex stage is what LOD costs).
struct LodOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) @interpolate(flat) bits: u32,
  @location(1) view_dist: f32,
};

@group(0) @binding(0) var block_textures: texture_2d_array<f32>;
@group(1) @binding(0) var<uniform> view: View;

// Corner `corner` of quad `q` of `node`: decode_quad and finish_vertex for a cube face (the only
// shape LOD meshes hold), the same math, with the quad scaled from cells to blocks.
fn lod_vertex(q: vec2<u32>, corner: u32, node: vec4<i32>) -> LodOut {
  let w = u32(node.w);
  let k = u32(node.x) & LOD_LEVEL_MASK;
  let geo = q.x;
  let p = vec3<f32>(f32(geo & GEO_POS_MASK), f32((geo >> GEO_Y_SHIFT) & GEO_POS_MASK), f32((geo >> GEO_Z_SHIFT) & GEO_POS_MASK));
  let qw = f32(((geo >> GEO_W_SHIFT) & GEO_POS_MASK) + 1u);
  let qh = f32(((geo >> GEO_H_SHIFT) & GEO_POS_MASK) + 1u);
  let f = (geo >> GEO_FACE_SHIFT) & 7u;
  let n = face_normal(f);
  let u = face_u(f);
  let v = face_v(f);
  var c = corner;
  // Counter-clockwise seen from outside, as decode_quad.
  if (dot(cross(u, v), n) < 0.0) { if (c == 1u) { c = 2u; } else if (c == 2u) { c = 1u; } }
  let cu = f32(c & 1u);
  let cv = f32(c >> 1u);
  // Cells to blocks: exact in f32 (at most 32 * 2^15). The edge growth works in blocks on
  // screen, so it is the same at every level.
  let local = (p + max(n, vec3(0.0)) + u * (cu * qw) + v * (cv * qh)) * f32(1u << k);
  let origin = vec3(node.x & ~i32(LOD_LEVEL_MASK), node.y, node.z);
  // finish_vertex: the wrapping i32 subtraction first, then the edge growth.
  let rel = vec3<f32>(origin - view.render_origin.xyz) + local;
  let clip = view.view_proj * vec4(rel, 1.0);
  let axis_u = select(view.view_proj[0], view.view_proj[2], f == 2u || f == 3u);
  let axis_v = select(view.view_proj[1], view.view_proj[2], f < 2u);
  let t = vec2(2.0 * cu - 1.0, 2.0 * cv - 1.0) * edge_growth(clip, axis_u, axis_v, 0.5 * view.viewport.xy);
  var o: LodOut;
  o.clip = clip + axis_u * t.x + axis_v * t.y;
  let oct = ((geo >> LOD_OCTANT_X_SHIFT) & 1u)
    | (((geo >> LOD_OCTANT_Y_SHIFT) & 1u) << 1u)
    | (((geo >> LOD_OCTANT_Z_SHIFT) & 1u) << 2u);
  if (((w >> oct) & 1u) != 0u) {
    // Hidden: every corner at one point outside the depth range, the whole quad is clipped.
    o.clip = vec4(0.0, 0.0, -1.0, 1.0);
  }
  let emissive = (((geo >> GEO_FLAGS_SHIFT) & 15u) & FLAG_EMISSIVE) != 0u;
  o.bits = (q.y & MAT_LAYER_MASK)
    | (f << LOD_OUT_FACE_SHIFT)
    | (((w >> LOD_MIP_SHIFT) & LOD_MIP_MASK) << LOD_OUT_MIP_SHIFT)
    | select(0u, LOD_OUT_TINT_BIT, (w & LOD_TINT_BIT) != 0u)
    | select(0u, LOD_OUT_EMISSIVE_BIT, emissive)
    | (k << LOD_OUT_LEVEL_SHIFT);
  o.view_dist = length(rel - view.camera_rel.xyz);
  return o;
}

// The debug tint of level k at half strength (F9 in voxel_world): 1 red, 2 green, 3 blue,
// 4 yellow, 5 magenta, 6 cyan, the others none.
fn lod_tint(k: u32) -> vec3<f32> {
  switch k {
    case 1u: { return vec3(1.0, 0.5, 0.5); }
    case 2u: { return vec3(0.5, 1.0, 0.5); }
    case 3u: { return vec3(0.5, 0.5, 1.0); }
    case 4u: { return vec3(1.0, 1.0, 0.5); }
    case 5u: { return vec3(1.0, 0.5, 1.0); }
    case 6u: { return vec3(0.5, 1.0, 1.0); }
    default: { return vec3(1.0); }
  }
}

// Opaque, cull_mode Back, no discard: early-Z stays on (ADR 0012). shade_fragment with the
// light of open sky (sky 15, no block light) and AO open, the same math: the face shade, the sky
// light (day and night), emissive faces at full brightness, the terrain fog.
@fragment
fn fs_lod(i: LodOut) -> @location(0) vec4<f32> {
  let mip = i32((i.bits >> LOD_OUT_MIP_SHIFT) & LOD_MIP_MASK);
  let c = textureLoad(block_textures, vec2<i32>(0, 0), i32(i.bits & MAT_LAYER_MASK), mip);
  var lit = c.rgb;
  if ((i.bits & LOD_OUT_TINT_BIT) != 0u) {
    lit = lit * lod_tint((i.bits >> LOD_OUT_LEVEL_SHIFT) & LOD_LEVEL_MASK);
  }
  if ((i.bits & LOD_OUT_EMISSIVE_BIT) == 0u) {
    let sky = level_curve(LIGHT_MAX_LEVEL) * view.sky_light.rgb;
    let block = vec3(level_curve(0.0)) * view.light_params.y;
    let light = max(max(sky, block), vec3(view.sky_light.a));
    lit = lit * face_shade((i.bits >> LOD_OUT_FACE_SHIFT) & 7u) * light;
  }
  let fog = clamp((i.view_dist - view.fog_params.x) / max(view.fog_params.y - view.fog_params.x, 1.0), 0.0, 1.0);
  return vec4(mix(lit, view.fog_color.rgb, fog), 1.0);
}
