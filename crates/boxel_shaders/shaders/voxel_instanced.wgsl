// Tier Low: no storage buffers in any stage (desktop GL 3.3 / GLES 3.0 class).
// PackedQuad is an INSTANCE-STEP vertex attribute (TriangleStrip, 4 vertices per instance,
// first_instance = first quad of the bucket). The chunk origin (low 32 bits) comes from immediates.
// The quad's light palette entry is a second INSTANCE-STEP attribute (location 1) from a buffer
// parallel to the arena, filled per quad on upload: an attribute, not a binding (ADR 0011).
// Exactly ONE uniform block and ONE texture/sampler pair per pipeline: wgpu 30 skips the GL
// uniform-block remap on NVIDIA GL 3.3 contexts, so every block would land on binding 0.
struct ChunkImmediate { origin: vec4<i32> };
@group(0) @binding(0) var block_textures: texture_2d_array<f32>;
@group(0) @binding(1) var block_sampler: sampler;
@group(1) @binding(0) var<uniform> view: View;
var<immediate> chunk: ChunkImmediate;

@vertex
fn vs_instanced(@builtin(vertex_index) vi: u32, @location(0) q: vec2<u32>, @location(1) lw: vec2<u32>) -> VsOut {
  return finish_vertex(decode_quad(q, lw, vi), chunk.origin.xyz, view);
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
