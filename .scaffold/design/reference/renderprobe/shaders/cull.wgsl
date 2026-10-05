// Tier High: GPU frustum + per-face-bucket culling. Writes compacted DrawIndexedIndirectArgs,
// the matching DrawRef and an atomic draw count for multi_draw_indexed_indirect_count.
// The *_count path is NOT validated by wgpu: never write past view.counts.y (max draws).
struct DrawArgs { index_count: u32, instance_count: u32, first_index: u32, base_vertex: i32, first_instance: u32 };

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> chunks: array<ChunkGpu>;
@group(0) @binding(2) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(3) var<storage, read_write> draw_refs: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read_write> draw_count: atomic<u32>;

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
  if ((ch.counts[7] & 1u) == 0u) { return; }
  let mn = vec3<f32>(ch.origin - view.render_origin.xyz);
  let mx = mn + vec3(CHUNK_SIZE);
  if (!in_frustum(mn, mx)) { return; }
  var start = ch.quad_base;
  for (var f = 0u; f < 6u; f++) {
    let n = ch.counts[f];
    if (n > 0u && bucket_visible(f, mn, mx, view.camera_rel.xyz)) {
      let s = atomicAdd(&draw_count, 1u);
      if (s < view.counts.y) {
        draw_args[s] = DrawArgs(n * 6u, 1u, 0u, 0, s);
        draw_refs[s] = vec2(slot, start);
      }
    }
    start += n;
  }
}
