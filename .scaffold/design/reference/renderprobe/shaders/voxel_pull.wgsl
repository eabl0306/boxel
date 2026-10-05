// Tiers High + Mid: vertex pulling from the quad arena (needs DownlevelFlags::VERTEX_STORAGE).
// Per-draw data = DrawRef (chunk_slot, quad_start) read as an INSTANCE-STEP vertex attribute at
// location 0; the draw sets first_instance = DrawRef index, instance_count = 1, base_vertex = 0.
@group(0) @binding(0) var block_textures: texture_2d_array<f32>;
@group(0) @binding(1) var block_sampler: sampler;
@group(1) @binding(0) var<uniform> view: View;
@group(2) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(2) @binding(1) var<storage, read> chunks: array<ChunkGpu>;

@vertex
fn vs_pull(@builtin(vertex_index) vi: u32, @location(0) draw: vec2<u32>) -> VsOut {
  let ch = chunks[draw.x];
  let q = quads[draw.y + (vi >> 2u)];
  return finish_vertex(decode_quad(q, vi & 3u), ch.origin, view);
}

@fragment
fn fs_opaque(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, i.layer);
  if (albedo.a < 0.5) { discard; } // cutout blocks (leaves)
  return shade_fragment(albedo, i, view);
}
