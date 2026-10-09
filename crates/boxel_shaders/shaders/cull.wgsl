// Tier High: GPU frustum + per-face-bucket culling. Writes two compacted lists of
// DrawIndexedIndirectArgs with matching DrawRefs and atomic counts for
// multi_draw_indexed_indirect_count: opaque face buckets at [0, counts.y) and the cutout bucket
// at [counts.y, counts.y + counts.z). The *_count path is NOT validated by wgpu, so every value
// stays in range: index_count <= 6 * MAX_QUADS_PER_BUCKET (the shared index buffer), no write past
// a list's capacity, and each stored count ends <= its capacity (<= 65535 = Vulkan's minimum
// maxDrawIndirectCount): a bucket that finds its list full undoes its increment. Those buckets are
// dropped; which ones depends on GPU scheduling, not on distance (M3 segments the lists).
//
// Cave culling (ADR 0022): with view.counts.w = 1, a slot whose bit in `cull.visible` is 0 is
// not drawn (the world's visibility search does not reach it). The bits share the buffer of the
// draw counts so the pass keeps 4 storage buffers (the Vulkan minimum per stage).
struct DrawArgs { index_count: u32, instance_count: u32, first_index: u32, base_vertex: i32, first_instance: u32 };

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> chunks: array<ChunkGpu>;
@group(0) @binding(2) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(3) var<storage, read_write> draw_refs: array<vec2<u32>>;
// counts[0] opaque, counts[1] cutout (cleared every frame, read by the indirect count draws);
// visible: bit (slot & 31) of word (slot >> 5) set when slot `slot` may be seen.
struct CullState { counts: array<atomic<u32>, 2>, visible: array<u32> };
@group(0) @binding(4) var<storage, read_write> cull: CullState;

// Quads covered by the shared index buffer (boxel_mesh::MAX_QUADS_PER_BUCKET, checked by a naga test).
const MAX_QUADS_PER_BUCKET: u32 = 98304u;

fn in_frustum(mn: vec3<f32>, mx: vec3<f32>) -> bool {
  for (var k = 0u; k < 6u; k++) {
    let pl = view.frustum[k];
    let pv = select(mn, mx, pl.xyz >= vec3(0.0));
    if (dot(pl.xyz, pv) + pl.w < 0.0) { return false; }
  }
  return true;
}

// chunk-level backface test per face bucket: a +Y bucket can only be seen from above its min y, etc.
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
fn cull_chunks(@builtin(global_invocation_id) gid: vec3<u32>) {
  let slot = gid.x;
  if (slot >= view.counts.x) { return; }
  let ch = chunks[slot];
  if ((ch.flags & 1u) == 0u) { return; }
  if (view.counts.w != 0u && ((cull.visible[slot >> 5u] >> (slot & 31u)) & 1u) == 0u) { return; }
  // wrapping i32 subtraction: exact for chunks nearer than 2^31 voxels, which every live chunk is
  // (the CPU clears the live flag of chunks far from the camera, see common.wgsl)
  let mn = vec3<f32>(ch.origin - view.render_origin.xyz);
  let mx = mn + vec3(CHUNK_SIZE);
  if (!in_frustum(mn, mx)) { return; }
  var start = ch.quad_base;
  for (var f = 0u; f < 6u; f++) {
    let n = ch.counts[f];
    if (n > 0u && bucket_visible(f, mn, mx, view.camera_rel.xyz)) {
      let s = atomicAdd(&cull.counts[0], 1u);
      if (s < view.counts.y) {
        draw_args[s] = DrawArgs(min(n, MAX_QUADS_PER_BUCKET) * 6u, 1u, 0u, 0, s);
        draw_refs[s] = vec2(slot, start);
      } else {
        atomicSub(&cull.counts[0], 1u);
      }
    }
    start += n;
  }
  // cutout bucket: all faces mixed, no bucket-level backface test (drawn with cull_mode None)
  let nc = ch.counts[6];
  if (nc > 0u) {
    let c = atomicAdd(&cull.counts[1], 1u);
    if (c < view.counts.z) {
      let s = view.counts.y + c;
      draw_args[s] = DrawArgs(min(nc, MAX_QUADS_PER_BUCKET) * 6u, 1u, 0u, 0, s);
      draw_refs[s] = vec2(slot, start);
    } else {
      atomicSub(&cull.counts[1], 1u);
    }
  }
}
