// Far LOD nodes (ADR 0031): what both LOD pipelines share, prepended with common.wgsl to
// lod.wgsl (instanced, every tier) and lod_pull.wgsl (GPU-driven, High).
//
// A node is a 32^3 grid of cells of 2^k blocks (level k). Its quads are ordinary PackedQuads in
// cells (cubes only, AO open); with no light, a quad's light palette index holds its seam (bits
// 0-1), its darkness under water (bits 2-5, boxel_mesh::lod::dark_of) and its bank flag (bit 6,
// boxel_mesh::lod::is_bank). The vertex shader scales a quad to blocks and collapses the quads
// of the octants finer detail draws; the fragment shader colors a face with the 1x1 mip of its
// texture layer (the texture's mean, tint included), lit as open sky (less its darkness),
// fogged like the terrain. The water bucket (issue #50) is drawn translucent, one layer per
// pixel: a depth prepass, then its color where the depth is its own (lod_water_vertex). Both
// pipelines run these same functions on the same quads, so they draw the same pixels.
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
// A quad's seam: bits 0-1 of its light palette index (boxel_mesh::lod::Seam, 0 for none).
const LOD_SEAM_SHIFT: u32 = 20u;
// A quad's darkness: bits 2-5 of its light palette index (boxel_mesh::lod::DARK_SHIFT).
const LOD_DARK_SHIFT: u32 = 22u;
const LOD_DARK_MASK: u32 = 15u;
// A bank: bit 6 of its light palette index (boxel_mesh::lod::BANK_BIT).
const LOD_BANK_BIT: u32 = 0x4000000u;
// The depth of a bank (reverse-Z, a share of its w): 2^-30, behind anything nearer than 2^30
// times the near plane, in front of the sky (0).
const LOD_BANK_DEPTH: f32 = 9.313225746154785e-10;
// LodOut::bits of a LOD quad: the texture layer (bits 0-11), its face (12-14), then the color
// mip, the tint flag, the emissive flag, the level and the darkness (flat, for the fragment).
const LOD_OUT_FACE_SHIFT: u32 = 12u;
const LOD_OUT_MIP_SHIFT: u32 = 16u;
const LOD_OUT_TINT_BIT: u32 = 0x200000u;
const LOD_OUT_EMISSIVE_BIT: u32 = 0x400000u;
const LOD_OUT_LEVEL_SHIFT: u32 = 24u;
const LOD_OUT_DARK_SHIFT: u32 = 28u;

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
// shape LOD meshes hold), the same math, with the quad scaled from cells to blocks. With `water`
// (a quad of the water bucket), a skirt on a side (a seam quad: the side of the fluid's top cell
// by a plane where finer detail can lie lower) closes the step down to a finer fluid's top,
// which lies at least FLUID_SURFACE up that cell (the near water's surface in its lowest block,
// and a finer LOD level's top is higher still): it starts there, so less of it is a second
// layer under the finer water, and it is shaded as the top it continues. Where the finer top
// lies higher in that cell (near water whose top block is the cell's upper one), the skirt
// under it is a second layer: the LOD does not know the finer top (ADR 0031, known limits).
//
// A bank (opaque, boxel_mesh::lod::add_banks) lies on its cell's side away from its normal and
// gets LOD_BANK_DEPTH: it shows only where nothing else is drawn, the sky through ground the
// cell rule dropped. One on a side is collapsed unless the eye is above its top, so every ray
// that meets it goes down past it, into the bed or the ground beyond.
fn lod_vertex(q: vec2<u32>, corner: u32, node: vec4<i32>, water: bool) -> LodOut {
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
  let bank = !water && (q.y & LOD_BANK_BIT) != 0u;
  // Cells to blocks: exact in f32 (at most 32 * 2^15). The edge growth works in blocks on
  // screen, so it is the same at every level.
  let scale = f32(1u << k);
  var local = (p + max(select(n, -n, bank), vec3(0.0)) + u * (cu * qw) + v * (cv * qh)) * scale;
  // A side's v is y; a skirt is one cell tall (only the top cell of a fluid has one).
  let skirt = water && f >= 2u && ((q.y >> LOD_SEAM_SHIFT) & 3u) != 0u;
  if (skirt && c < 2u) {
    local.y = local.y + FLUID_SURFACE;
  }
  let origin = vec3(node.x & ~i32(LOD_LEVEL_MASK), node.y, node.z);
  // finish_vertex: the wrapping i32 subtraction first, then the edge growth.
  let base = vec3<f32>(origin - view.render_origin.xyz);
  let rel = base + local;
  let clip = view.view_proj * vec4(rel, 1.0);
  let axis_u = select(view.view_proj[0], view.view_proj[2], f == 2u || f == 3u);
  let axis_v = select(view.view_proj[1], view.view_proj[2], f < 2u);
  let t = vec2(2.0 * cu - 1.0, 2.0 * cv - 1.0) * edge_growth(clip, axis_u, axis_v, 0.5 * view.viewport.xy);
  var o: LodOut;
  o.clip = clip + axis_u * t.x + axis_v * t.y;
  if (bank) {
    o.clip.z = o.clip.w * LOD_BANK_DEPTH;
  }
  let oct = ((geo >> LOD_OCTANT_X_SHIFT) & 1u)
    | (((geo >> LOD_OCTANT_Y_SHIFT) & 1u) << 1u)
    | (((geo >> LOD_OCTANT_Z_SHIFT) & 1u) << 2u);
  // A bank on a side (v is y) with the eye at or under its top.
  let low_eye = bank && f >= 2u && view.camera_rel.y <= base.y + (p.y + qh) * scale;
  if (((w >> oct) & 1u) != 0u || low_eye) {
    // Hidden: every corner at one point outside the depth range, the whole quad is clipped.
    o.clip = vec4(0.0, 0.0, -1.0, 1.0);
  }
  let emissive = (((geo >> GEO_FLAGS_SHIFT) & 15u) & FLAG_EMISSIVE) != 0u;
  o.bits = (q.y & MAT_LAYER_MASK)
    | (select(f, 0u, skirt) << LOD_OUT_FACE_SHIFT)
    | (((w >> LOD_MIP_SHIFT) & LOD_MIP_MASK) << LOD_OUT_MIP_SHIFT)
    | select(0u, LOD_OUT_TINT_BIT, (w & LOD_TINT_BIT) != 0u)
    | select(0u, LOD_OUT_EMISSIVE_BIT, emissive)
    | (k << LOD_OUT_LEVEL_SHIFT)
    | (((q.y >> LOD_DARK_SHIFT) & LOD_DARK_MASK) << LOD_OUT_DARK_SHIFT);
  o.view_dist = length(rel - view.camera_rel.xyz);
  return o;
}

// What the vertex stage hands the water's fragment: LodOut with an invariant position, so the
// depth prepass and the color pass, two pipelines over the same quads, land on the same depth.
struct LodWaterOut {
  @builtin(position) @invariant clip: vec4<f32>,
  @location(0) @interpolate(flat) bits: u32,
  @location(1) view_dist: f32,
};

// lod_vertex for a quad of the water bucket. Its depth is scaled by FLUID_DEPTH_SCALE, like the
// near water's (finish_vertex): it loses every tie with an opaque face (a shore at its height,
// where the quads overlap by their edge growth). A hidden quad stays outside the depth range.
fn lod_water_vertex(q: vec2<u32>, corner: u32, node: vec4<i32>) -> LodWaterOut {
  let o = lod_vertex(q, corner, node, true);
  var w: LodWaterOut;
  w.clip = vec4(o.clip.xy, o.clip.z * FLUID_DEPTH_SCALE, o.clip.w);
  w.bits = o.bits;
  w.view_dist = o.view_dist;
  return w;
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

// The color of a LOD face with `bits` (LodOut) at `view_dist`: shade_fragment with the light of
// open sky less its darkness (sky 15 - dark, no block light) and AO open, the same math: the
// face shade, the sky light (day and night), emissive faces at full brightness, the terrain fog.
// Its alpha is the texture's mean (the 1x1 mip's).
fn lod_color(bits: u32, view_dist: f32) -> vec4<f32> {
  let mip = i32((bits >> LOD_OUT_MIP_SHIFT) & LOD_MIP_MASK);
  let c = textureLoad(block_textures, vec2<i32>(0, 0), i32(bits & MAT_LAYER_MASK), mip);
  var lit = c.rgb;
  if ((bits & LOD_OUT_TINT_BIT) != 0u) {
    lit = lit * lod_tint((bits >> LOD_OUT_LEVEL_SHIFT) & LOD_LEVEL_MASK);
  }
  if ((bits & LOD_OUT_EMISSIVE_BIT) == 0u) {
    let dark = f32((bits >> LOD_OUT_DARK_SHIFT) & LOD_DARK_MASK);
    let sky = level_curve(LIGHT_MAX_LEVEL - dark) * view.sky_light.rgb;
    let block = vec3(level_curve(0.0)) * view.light_params.y;
    let light = max(max(sky, block), vec3(view.sky_light.a));
    lit = lit * face_shade((bits >> LOD_OUT_FACE_SHIFT) & 7u) * light;
  }
  let fog = clamp((view_dist - view.fog_params.x) / max(view.fog_params.y - view.fog_params.x, 1.0), 0.0, 1.0);
  return vec4(mix(lit, view.fog_color.rgb, fog), c.a);
}

// Opaque, cull_mode Back, no discard: early-Z stays on (ADR 0012).
@fragment
fn fs_lod(i: LodOut) -> @location(0) vec4<f32> {
  return vec4(lod_color(i.bits, i.view_dist).rgb, 1.0);
}

// The water's color pass: blended with its alpha like the near water (fs_translucent), depth
// test GreaterEqual without writes after the prepass, so only the nearest LOD water shows.
@fragment
fn fs_lod_water(i: LodWaterOut) -> @location(0) vec4<f32> {
  return lod_color(i.bits, i.view_dist);
}

// The water's depth prepass: its pipeline writes depth and no color (an empty write mask).
@fragment
fn fs_lod_water_depth() -> @location(0) vec4<f32> {
  return vec4(0.0);
}
