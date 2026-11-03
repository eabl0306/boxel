// Tier Low: no storage buffers in any stage (desktop GL 3.3 / GLES 3.0 class).
// PackedQuad is an INSTANCE-STEP vertex attribute (TriangleStrip, 4 vertices per instance,
// first_instance = first quad of the bucket in the chunk's page, bound as the vertex buffer). The
// chunk origin (low 32 bits) and its tinted palette entries (ADR 0064) come from immediates. The quad's light palette entry is a second
// INSTANCE-STEP attribute (location 1) from a light page parallel to the quad page, filled per quad
// on upload: an attribute, not a binding (ADR 0011).
// Exactly ONE uniform block and ONE texture/sampler pair per pipeline: wgpu 30 skips the GL
// uniform-block remap on NVIDIA GL 3.3 contexts, so every block would land on binding 0.
struct ChunkImmediate { origin: vec4<i32> }; // xyz = chunk origin, w = tinted entries (boxel_shaders::gpu::ChunkImmediate)
@group(0) @binding(0) var block_textures: texture_2d_array<f32>;
@group(0) @binding(1) var block_sampler: sampler;
@group(1) @binding(0) var<uniform> view: View;
var<immediate> chunk: ChunkImmediate;

@vertex
fn vs_instanced(@builtin(vertex_index) vi: u32, @location(0) q: vec2<u32>, @location(1) lw: vec2<u32>) -> VsOut {
  var o = finish_vertex(decode_quad(q, lw, vi), chunk.origin.xyz, view);
  // A tinted entry (ADR 0064): the other kind flat, as Mid draws it.
  if (((q.y >> MAT_LIGHT_SHIFT) & MAT_LIGHT_MASK) < u32(chunk.origin.w)) {
    o = with_tint(o, tint_flat_corners(lw));
  }
  return o;
}

// Opaque buckets (pipeline with cull_mode Back): no discard, early-Z stays on.
@fragment
fn fs_opaque(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, out_layer(i));
  return shade_fragment(albedo, i, view);
}

// Cutout bucket (pipeline with cull_mode None): alpha test.
@fragment
fn fs_cutout(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, out_layer(i));
  if (!alpha_test_passes(albedo)) { discard; }
  return shade_fragment(albedo, i, view);
}

// Translucent bucket (pipeline with alpha blending, depth test without depth write, cull_mode
// None; drawn last, back to front per chunk): the shaded color with the texture's alpha.
// Fully clear texels (the inside of glass) are dropped.
@fragment
fn fs_translucent(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, out_layer(i));
  if (albedo.a <= 0.0) { discard; }
  return vec4(shade_fragment(albedo, i, view).rgb, albedo.a);
}

// The translucent bucket again, after its color, only while there is fog (ADR 0058): its
// pipeline writes depth and no color (an empty write mask), so the fog march ends at the
// nearest translucent surface. Texels under 0.1 alpha (the clear inside of glass) let the fog
// through, like Minecraft's cutout glass.
@fragment
fn fs_translucent_depth(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, out_layer(i));
  if (albedo.a < 0.1) { discard; }
  return vec4(0.0);
}
