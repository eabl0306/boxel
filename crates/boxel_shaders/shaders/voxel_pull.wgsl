// Tiers High + Mid: vertex pulling from a page of chunk quads (needs DownlevelFlags::VERTEX_STORAGE).
// Per-draw data = DrawRef (chunk_slot, quad_start) read as an INSTANCE-STEP vertex attribute at
// location 0; the draw sets first_instance = DrawRef index, instance_count = 1, base_vertex = 0.
// Group 2 binds the page of the drawn chunk: quad_start and ChunkGpu::light_base are page-local.
// The light palette entry of each quad is read from the same page at ChunkGpu::light_base.
@group(0) @binding(0) var block_textures: texture_2d_array<f32>;
@group(0) @binding(1) var block_sampler: sampler;
@group(1) @binding(0) var<uniform> view: View;
@group(2) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(2) @binding(1) var<storage, read> chunks: array<ChunkGpu>;

@vertex
fn vs_pull(@builtin(vertex_index) vi: u32, @location(0) draw: vec2<u32>) -> VsOut {
  let ch = chunks[draw.x];
  let q = quads[draw.y + (vi >> 2u)];
  // The chunk's light palette follows its quads in its page (no extra binding). An index past
  // the palette (only a malformed mesh) reads open sky, as on Low, never another chunk's data.
  let li = (q.y >> MAT_LIGHT_SHIFT) & MAT_LIGHT_MASK;
  let lw = select(FULL_SKY_LIGHT, quads[ch.light_base + min(li, ch.light_len - 1u)], li < ch.light_len);
  return finish_vertex(decode_quad(q, lw, vi & 3u), ch.origin, view);
}

// Opaque buckets (pipeline with cull_mode Back): no discard, early-Z stays on.
@fragment
fn fs_opaque(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, i.layer);
  return shade_fragment(albedo, i, view);
}

// Cutout bucket (pipeline with cull_mode None): alpha test.
@fragment
fn fs_cutout(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, i.layer);
  if (!alpha_test_passes(albedo)) { discard; }
  return shade_fragment(albedo, i, view);
}

// Translucent bucket (pipeline with alpha blending, depth test without depth write, cull_mode
// None; drawn last, back to front per chunk): the shaded color with the texture's alpha.
// Fully clear texels (the inside of glass) are dropped.
@fragment
fn fs_translucent(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, i.layer);
  if (albedo.a <= 0.0) { discard; }
  return vec4(shade_fragment(albedo, i, view).rgb, albedo.a);
}
