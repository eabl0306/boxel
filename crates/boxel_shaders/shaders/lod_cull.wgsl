// Far LOD nodes (ADR 0031), tier High: GPU frustum and per-face-bucket culling of the node table.
// Prepended with common.wgsl.
//
// One invocation per node slot. A live node (its quads in the arena, inside the GPU window) with
// an octant left to draw whose box (32 * 2^k blocks) meets the frustum writes one indexed draw per
// face bucket that can face the camera (the box-level back-face test of cull.wgsl) into the list
// of its segment, with its DrawRef: slots [s * LOD_SEGMENT_SLOTS, (s + 1) * LOD_SEGMENT_SLOTS) write
// list s, which starts at draw s * LOD_SEGMENT_DRAWS and has its count in counts[s]. A segment
// has at most 6 * LOD_SEGMENT_SLOTS <= 65535 draws (Vulkan's minimum maxDrawIndirectCount), so no
// list can overflow and every draw is written. The *_count path is NOT validated by wgpu: index
// counts stay within the shared index buffer (MAX_QUADS_PER_BUCKET quads).
struct DrawArgs { index_count: u32, instance_count: u32, first_index: u32, base_vertex: i32, first_instance: u32 };
struct LodNodeGpu { node: vec4<i32>, quad_base: u32, counts: array<u32, 3> };

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> nodes: array<LodNodeGpu>;
@group(0) @binding(2) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(3) var<storage, read_write> draw_refs: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read_write> counts: array<atomic<u32>>;

const LOD_LEVEL_MASK: u32 = 15u;
const LOD_HIDDEN_MASK: u32 = 0xFFu;
const LOD_LIVE_BIT: u32 = 0x4000u;
// Slots per segment, and the draws its list holds (boxel_shaders::gpu::lod, checked by a naga test).
const LOD_SEGMENT_SLOTS: u32 = 10922u;
const LOD_SEGMENT_DRAWS: u32 = 65532u;
// Quads covered by the shared index buffer (boxel_mesh::MAX_QUADS_PER_BUCKET).
const MAX_QUADS_PER_BUCKET: u32 = 98304u;

fn in_frustum(mn: vec3<f32>, mx: vec3<f32>) -> bool {
  for (var k = 0u; k < 6u; k++) {
    let pl = view.frustum[k];
    let pv = select(mn, mx, pl.xyz >= vec3(0.0));
    if (dot(pl.xyz, pv) + pl.w < 0.0) { return false; }
  }
  return true;
}

// Node-level backface test per face bucket, as cull.wgsl's for chunks.
fn bucket_visible(f: u32, mn: vec3<f32>, mx: vec3<f32>, cam: vec3<f32>) -> bool {
  switch f {
    case 0u: { return cam.y > mn.y; }
    case 1u: { return cam.y < mx.y; }
    case 2u: { return cam.x > mn.x; }
    case 3u: { return cam.x < mx.x; }
    case 4u: { return cam.z > mn.z; }
    default: { return cam.z < mx.z; }
  }
}

@compute @workgroup_size(64)
fn cull_lod(@builtin(global_invocation_id) gid: vec3<u32>) {
  let slot = gid.x;
  if (slot >= arrayLength(&nodes)) { return; }
  let n = nodes[slot];
  let w = u32(n.node.w);
  if ((w & LOD_LIVE_BIT) == 0u || (w & LOD_HIDDEN_MASK) == LOD_HIDDEN_MASK) { return; }
  let k = u32(n.node.x) & LOD_LEVEL_MASK;
  // Wrapping i32 subtraction: exact inside the GPU window (the CPU clears the live bit outside).
  let origin = vec3(n.node.x & ~i32(LOD_LEVEL_MASK), n.node.y, n.node.z);
  let mn = vec3<f32>(origin - view.render_origin.xyz);
  let mx = mn + vec3(f32(32u << k));
  if (!in_frustum(mn, mx)) { return; }
  let seg = slot / LOD_SEGMENT_SLOTS;
  let first = seg * LOD_SEGMENT_DRAWS;
  var start = n.quad_base;
  for (var f = 0u; f < 6u; f++) {
    let c = (n.counts[f >> 1u] >> ((f & 1u) * 16u)) & 0xFFFFu;
    if (c > 0u && bucket_visible(f, mn, mx, view.camera_rel.xyz)) {
      let s = first + atomicAdd(&counts[seg], 1u);
      draw_args[s] = DrawArgs(min(c, MAX_QUADS_PER_BUCKET) * 6u, 1u, 0u, 0, s);
      draw_refs[s] = vec2(slot, start);
    }
    start += c;
  }
}
