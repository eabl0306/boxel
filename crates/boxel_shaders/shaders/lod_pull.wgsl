// Far LOD nodes (ADR 0031), tier High: vertex pulling from the LOD arena, drawn by the lists
// lod_cull.wgsl writes (one multi_draw_indexed_indirect_count per segment, the shared quad index
// buffer). Prepended with common.wgsl and lod_common.wgsl.
//
// Per-draw data = DrawRef (node slot, first quad of the face bucket) read as an INSTANCE-STEP
// vertex attribute at location 0, like vs_pull: the draw sets first_instance = its DrawRef
// index, instance_count = 1, base_vertex = 0. The geometry is group 3 (group 2 stays the main
// geometry, which the translucent buckets drawn after the LOD still use).
struct LodNodeGpu { node: vec4<i32>, quad_base: u32, counts: array<u32, 3>, seams: array<u32, 6>, water: u32, water_seams: u32 };

@group(3) @binding(0) var<storage, read> lod_quads: array<vec2<u32>>;
@group(3) @binding(1) var<storage, read> lod_nodes: array<LodNodeGpu>;

@vertex
fn vs_lod_pull(@builtin(vertex_index) vi: u32, @location(0) draw: vec2<u32>) -> LodOut {
  let q = lod_quads[draw.y + (vi >> 2u)];
  return lod_vertex(q, vi & 3u, lod_nodes[draw.x].node, false);
}

// The water bucket (issue #50), from the water lists lod_cull.wgsl writes: its depth prepass and
// its blended color pass both run this.
@vertex
fn vs_lod_pull_water(@builtin(vertex_index) vi: u32, @location(0) draw: vec2<u32>) -> LodWaterOut {
  let q = lod_quads[draw.y + (vi >> 2u)];
  return lod_water_vertex(q, vi & 3u, lod_nodes[draw.x].node);
}
