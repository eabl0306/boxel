// Far LOD nodes (ADR 0031), the same pipeline on every tier. Prepended with common.wgsl.
//
// A node is a 32^3 grid of cells of 2^k blocks (level k). Its quads are ordinary PackedQuads in
// cells (cubes only, no light palette, AO open): drawn like the Low tier's quads, one instanced
// triangle strip per node, `draw(0..4, first..first + n)` over the LOD arena. The vertex shader
// scales the quad to blocks and collapses the quads of the octants finer detail draws; the
// fragment shader colors a face with the 1x1 mip of its texture layer (the texture's mean, tint
// included), lit as open sky, fogged like the terrain.
//
// Exactly ONE uniform block (View) and ONE texture, no sampler (textureLoad), like Low (ADR 0011).
//
// Immediates (LodImmediate, boxel_shaders::gpu): x = node origin x, low 32 bits, OR the level in
// bits 0-3 (a level-k origin has 5 + k zero low bits); y, z = node origin y, z (low 32 bits);
// w = hidden octants (bits 0-7, bit x | y << 1 | z << 2) | color mip (bits 8-12) | level tint
// (bit 13).
struct LodImmediate { node: vec4<i32> };
const LOD_LEVEL_MASK: u32 = 15u;
const LOD_MIP_SHIFT: u32 = 8u;
const LOD_MIP_MASK: u32 = 31u;
const LOD_TINT_BIT: u32 = 0x2000u;
// The octant of a quad: bit 4 of each of its min cell's coordinates in geo (boxel_mesh::lod::octant_of).
const LOD_OCTANT_X_SHIFT: u32 = 4u;
const LOD_OCTANT_Y_SHIFT: u32 = 9u;
const LOD_OCTANT_Z_SHIFT: u32 = 14u;
// VsOut::layer of a LOD quad: the texture layer (bits 0-11), then the color mip, the tint flag
// and the level (flat, for the fragment).
const LOD_OUT_MIP_SHIFT: u32 = 16u;
const LOD_OUT_TINT_BIT: u32 = 0x200000u;
const LOD_OUT_LEVEL_SHIFT: u32 = 24u;

@group(0) @binding(0) var block_textures: texture_2d_array<f32>;
@group(1) @binding(0) var<uniform> view: View;
var<immediate> lod: LodImmediate;

@vertex
fn vs_lod(@builtin(vertex_index) vi: u32, @location(0) q: vec2<u32>) -> VsOut {
  let w = u32(lod.node.w);
  let k = u32(lod.node.x) & LOD_LEVEL_MASK;
  var d = decode_quad(q, FULL_SKY_LIGHT, vi);
  // Cells to blocks: exact in f32 (at most 32 * 2^15). The edge growth of finish_vertex works
  // in blocks on screen, so it is the same at every level.
  d.local = d.local * f32(1u << k);
  let origin = vec3(lod.node.x & ~i32(LOD_LEVEL_MASK), lod.node.y, lod.node.z);
  var o = finish_vertex(d, origin, view);
  let oct = ((q.x >> LOD_OCTANT_X_SHIFT) & 1u)
    | (((q.x >> LOD_OCTANT_Y_SHIFT) & 1u) << 1u)
    | (((q.x >> LOD_OCTANT_Z_SHIFT) & 1u) << 2u);
  if (((w >> oct) & 1u) != 0u) {
    // Hidden: every corner at one point outside the depth range, the whole quad is clipped.
    o.clip = vec4(0.0, 0.0, -1.0, 1.0);
  }
  o.layer = d.layer
    | (((w >> LOD_MIP_SHIFT) & LOD_MIP_MASK) << LOD_OUT_MIP_SHIFT)
    | select(0u, LOD_OUT_TINT_BIT, (w & LOD_TINT_BIT) != 0u)
    | (k << LOD_OUT_LEVEL_SHIFT);
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

// Opaque, cull_mode Back, no discard: early-Z stays on (ADR 0012).
@fragment
fn fs_lod(i: VsOut) -> @location(0) vec4<f32> {
  let mip = i32((i.layer >> LOD_OUT_MIP_SHIFT) & LOD_MIP_MASK);
  let c = textureLoad(block_textures, vec2<i32>(0, 0), i32(i.layer & MAT_LAYER_MASK), mip);
  var rgb = c.rgb;
  if ((i.layer & LOD_OUT_TINT_BIT) != 0u) {
    rgb = rgb * lod_tint((i.layer >> LOD_OUT_LEVEL_SHIFT) & LOD_LEVEL_MASK);
  }
  // Face shade, sky light (day and night), emissive faces at full brightness, terrain fog.
  return shade_fragment(vec4(rgb, 1.0), i, view);
}
