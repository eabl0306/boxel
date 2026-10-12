// Tier High: GPU frustum + per-face-bucket culling. Chunk quads live in pages (ADR 0035) and every
// page has its own two compacted lists of DrawIndexedIndirectArgs with matching DrawRefs and
// atomic counts, drawn by one multi_draw_indexed_indirect_count each with the page bound: the
// opaque face buckets of page p at [p * stride, p * stride + stride) and its cutout buckets at
// [L * stride + p * stride / 6, ... + stride / 6), with L = view.counts.y pages with lists and
// stride = view.counts.z. A page holds at most stride / 6 chunks, so its lists never fill up. The
// *_count path is NOT validated by wgpu, so every value stays in range all the same:
// index_count <= 6 * MAX_QUADS_PER_BUCKET (the shared index buffer), no write outside the page's
// list, and each stored count ends <= its capacity (<= 65532, under Vulkan's minimum
// maxDrawIndirectCount of 65535): a bucket that finds its list full undoes its increment (a
// memory guard; the CPU never puts more chunks in a page). Writes stay inside the lists that
// exist (p < L): without robustBufferAccess2 an out-of-range storage write is clamped, not dropped.
//
// Cave culling (ADR 0022): with view.counts.w = 1, a slot whose bit in `cull.visible` is 0 is
// not drawn (the world's visibility search does not reach it). The bits share the buffer of the
// draw counts so the pass keeps 4 storage buffers (the Vulkan minimum per stage).
struct DrawArgs { index_count: u32, instance_count: u32, first_index: u32, base_vertex: i32, first_instance: u32 };

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> chunks: array<ChunkGpu>;
@group(0) @binding(2) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(3) var<storage, read_write> draw_refs: array<vec2<u32>>;
// counts[p] opaque and counts[MAX_PAGES + p] cutout draws of page p (cleared every frame, read by
// the indirect count draws); visible: bit (slot & 31) of word (slot >> 5) set when slot `slot`
// may be seen.
struct CullState { counts: array<atomic<u32>, 512>, visible: array<u32> };
@group(0) @binding(4) var<storage, read_write> cull: CullState;

// Quads covered by the shared index buffer (boxel_mesh::MAX_QUADS_PER_BUCKET, checked by a naga test).
const MAX_QUADS_PER_BUCKET: u32 = 98304u;
// Page ids are below this (boxel_shaders::gpu::chunk_memory::MAX_PAGES; CullState holds 2 per page).
const MAX_PAGES: u32 = 256u;

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
  // only lists that exist; never true while the CPU is consistent
  let p = ch.page;
  let pages = min(view.counts.y, MAX_PAGES);
  if (p >= pages) { return; }
  if (view.counts.w != 0u && ((cull.visible[slot >> 5u] >> (slot & 31u)) & 1u) == 0u) { return; }
  // wrapping i32 subtraction: exact for chunks nearer than 2^31 voxels, which every live chunk is
  // (the CPU clears the live flag of chunks far from the camera, see common.wgsl)
  let mn = vec3<f32>(ch.origin - view.render_origin.xyz);
  let mx = mn + vec3(CHUNK_SIZE);
  if (!in_frustum(mn, mx)) { return; }
  let stride = view.counts.z;
  var start = ch.quad_base;
  for (var f = 0u; f < 6u; f++) {
    let n = ch.counts[f];
    if (n > 0u && bucket_visible(f, mn, mx, view.camera_rel.xyz)) {
      let s = atomicAdd(&cull.counts[p], 1u);
      if (s < stride) {
        let i = p * stride + s;
        draw_args[i] = DrawArgs(min(n, MAX_QUADS_PER_BUCKET) * 6u, 1u, 0u, 0, i);
        draw_refs[i] = vec2(slot, start);
      } else {
        atomicSub(&cull.counts[p], 1u);
      }
    }
    start += n;
  }
  // cutout bucket: all faces mixed, no bucket-level backface test (drawn with cull_mode None)
  let nc = ch.counts[6];
  if (nc > 0u) {
    let cut = stride / 6u;
    let c = atomicAdd(&cull.counts[MAX_PAGES + p], 1u);
    if (c < cut) {
      let i = pages * stride + p * cut + c;
      draw_args[i] = DrawArgs(min(nc, MAX_QUADS_PER_BUCKET) * 6u, 1u, 0u, 0, i);
      draw_refs[i] = vec2(slot, start);
    } else {
      atomicSub(&cull.counts[MAX_PAGES + p], 1u);
    }
  }
}
