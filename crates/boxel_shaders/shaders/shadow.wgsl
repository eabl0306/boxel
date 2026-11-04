// Sun shadows (ADR 0065, High only): the depth of the casters in one cascade's map, from the
// lists of shadow_cull.wgsl. The quads are read and decoded as vs_pull does (decode_quad: the
// exact shapes of slabs, stairs, fences, torches and fluids); the light palette is not read.
//
// Every corner moves half a shadow texel outward along its quad's two edges, in the quad's plane
// (the light-space twin of edge_growth in common.wgsl): greedy quads meet in T-junctions, and a
// crack between them would let light through the map.
//
// vs_shadow draws the opaque buckets the cull chose (they all look away from the sun) with no
// fragment stage. vs_shadow_cutout draws the cutout bucket, whose faces are mixed: a face that
// does not look away from the sun is collapsed (the same rule as the buckets), and so are
// crosses and planes (small plants cast nothing, owner decision 6); fs_shadow_cutout drops the
// texels the alpha test drops, at the cascade's mip level, so leaves cast dappled shadows.
@group(0) @binding(0) var block_textures: texture_2d_array<f32>;
@group(0) @binding(1) var block_sampler: sampler;
@group(1) @binding(0) var<uniform> view: View;
@group(2) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(2) @binding(1) var<storage, read> chunks: array<ChunkGpu>;
@group(3) @binding(0) var<uniform> cascade: ShadowCascade;

struct ShadowOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) layer: u32,
};

// Where a collapsed quad's corners go: one point outside the map, so its triangles have no area.
const SHADOW_COLLAPSED: vec4<f32> = vec4<f32>(2.0, 2.0, 2.0, 1.0);

// The decoded corner `d` of a quad of draw `draw` (DrawRef: chunk slot, first quad) in the map.
fn shadow_corner(draw: vec2<u32>, d: Decoded) -> ShadowOut {
  let ch = chunks[draw.x];
  let grow = (face_u(d.face) * d.grow.x + face_v(d.face) * d.grow.y) * (0.5 * cascade.params.x);
  // wrapping i32 subtraction first: exact for any world position (see common.wgsl)
  let rel = vec3<f32>(ch.origin - view.render_origin.xyz) + d.local + grow;
  var o: ShadowOut;
  o.clip = cascade.view_proj * vec4(rel, 1.0);
  // a turned cube face (logs along X or Z): the texture's vertical runs along u
  o.uv = select(d.uv, d.uv.yx, (d.flags >> 1u) == SHAPE_TURNED);
  o.layer = d.layer;
  return o;
}

// The quad of corner `vi` of draw `draw`.
fn shadow_quad(draw: vec2<u32>, vi: u32) -> vec2<u32> {
  return quads[draw.y + (vi >> 2u)];
}

@vertex
fn vs_shadow(@builtin(vertex_index) vi: u32, @location(0) draw: vec2<u32>) -> ShadowOut {
  let d = decode_quad(shadow_quad(draw, vi), vec2(0u), vi & 3u);
  return shadow_corner(draw, d);
}

@vertex
fn vs_shadow_cutout(@builtin(vertex_index) vi: u32, @location(0) draw: vec2<u32>) -> ShadowOut {
  let q = shadow_quad(draw, vi);
  let d = decode_quad(q, vec2(0u), vi & 3u);
  let cross = ((q.x >> GEO_SHAPE_SHIFT) & GEO_SHAPE_MASK) == SHAPE_CROSS;
  if (cross || dot(face_normal(d.face), cascade.light.xyz) >= 0.0) {
    var o: ShadowOut;
    o.clip = SHADOW_COLLAPSED;
    o.uv = vec2(0.0);
    o.layer = 0u;
    return o;
  }
  return shadow_corner(draw, d);
}

// The cutout bucket's alpha test (alpha_test_passes, as fs_cutout) at the cascade's mip level.
@fragment
fn fs_shadow_cutout(i: ShadowOut) {
  let albedo = textureSampleLevel(block_textures, block_sampler, i.uv, i.layer, cascade.params.y);
  if (!alpha_test_passes(albedo)) { discard; }
}
