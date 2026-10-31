// Cell quads (ADR 0050): what a cell draws from its data (an item in a frame, the items on a
// shelf), baked into its chunk's mesh by the mesher and drawn after the solid terrain, on every
// tier with this same pipeline and the same vertices. Prepended with common.wgsl (the View, the
// light and the shading of the terrain).
//
// Exactly ONE uniform block (the View) and ONE texture/sampler pair (the cell texture array and
// its sampler, the block sampler's settings) per pipeline, no storage buffer, no immediates (ADR
// 0011). Per vertex (boxel_shaders::gpu::CellVertex):
//   origin = the chunk's world voxel min corner, low 32 bits (wraps; only used minus
//            render_origin, as the terrain does)
//   pos    = the corner in its chunk (blocks)
//   uv     = the point of the image it shows (a fraction of it)
//   data.x = image layer (12 bits) | face (3 bits) << 12 | full bright (1 bit) << 15 | the
//            cell's LightValue << 16
//   data.y = tint, sRGB 0xRRGGBB
// The fragment is a cutout terrain fragment: alpha test, then shade_fragment with the quad's
// face, its cell's light at the 4 corners and AO open.

@group(0) @binding(0) var cell_textures: texture_2d_array<f32>;
@group(0) @binding(1) var cell_sampler: sampler;
@group(1) @binding(0) var<uniform> view: View;

const CELL_LAYER_MASK: u32 = 0xFFFu;
const CELL_FACE_SHIFT: u32 = 12u;
const CELL_BRIGHT_BIT: u32 = 0x8000u;
const CELL_LIGHT_SHIFT: u32 = 16u;

struct CellOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) data: vec2<u32>,
  @location(2) view_dist: f32,
};

@vertex
fn vs_cell(
  @location(0) origin: vec3<i32>,
  @location(1) pos: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) data: vec2<u32>,
) -> CellOut {
  // wrapping i32 subtraction first, as finish_vertex: exact at any world position
  let rel = vec3<f32>(origin - view.render_origin.xyz) + pos;
  var o: CellOut;
  o.clip = view.view_proj * vec4(rel, 1.0);
  o.uv = uv;
  o.data = data;
  o.view_dist = length(rel - view.camera_rel.xyz);
  return o;
}

// sRGB to linear, per channel (the texture is sampled linear from its sRGB format).
fn cell_linear(c: vec3<f32>) -> vec3<f32> {
  let low = c / 12.92;
  let high = pow((c + vec3(0.055)) / 1.055, vec3(2.4));
  return select(high, low, c <= vec3(0.04045));
}

@fragment
fn fs_cell(i: CellOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(cell_textures, cell_sampler, i.uv, i.data.x & CELL_LAYER_MASK);
  if (!alpha_test_passes(albedo)) { discard; }
  let tint = vec3<f32>((vec3(i.data.y) >> vec3(16u, 8u, 0u)) & vec3(255u)) / 255.0;
  let light = i.data.x >> CELL_LIGHT_SHIFT;
  let corners = light | (light << 16u);
  var v: VsOut;
  v.clip = i.clip;
  v.uv = i.uv;
  v.layer = 0u;
  v.face = (i.data.x >> CELL_FACE_SHIFT) & 7u;
  v.view_dist = i.view_dist;
  v.st = vec2(0.0);
  v.light = vec2(corners, corners);
  v.ao = 0xFFu | select(0u, FLAG_EMISSIVE << 8u, (i.data.x & CELL_BRIGHT_BIT) != 0u);
  return shade_fragment(vec4(albedo.rgb * cell_linear(tint), albedo.a), v, view);
}
