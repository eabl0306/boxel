// Far LOD nodes (ADR 0031), instanced: the pipeline of Mid and Low (and of High without its
// GPU-driven path). Prepended with common.wgsl and lod_common.wgsl.
//
// Drawn like the Low tier's quads, one instanced triangle strip per node,
// `draw(0..4, first..first + n)` over the LOD arena, the node in the immediates (LodImmediate,
// boxel_shaders::gpu; bits in lod_common.wgsl).
struct LodImmediate { node: vec4<i32> };

var<immediate> lod: LodImmediate;

@vertex
fn vs_lod(@builtin(vertex_index) vi: u32, @location(0) q: vec2<u32>) -> LodOut {
  return lod_vertex(q, vi, lod.node);
}
