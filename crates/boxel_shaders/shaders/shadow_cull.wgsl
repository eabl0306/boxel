// Sun shadows (ADR 0065, High only): the casters of every cascade, one thread per chunk slot.
// The twin of cull.wgsl for the sun's boxes instead of the camera's frustum.
//
// A slot casts only if it is live, has a sky-lit face (FLAG_SKY_LIT, set at upload: a chunk
// without one can only shade receivers with no sky light, which the shadows never touch) and
// overlaps the camera realm's band (no realm shadows another). The cave culling bits and the
// camera's frustum are not read: a tree behind the camera shades the ground in front of it.
// For each cascade whose box the chunk's box meets, it draws the face buckets that look away
// from the sun (shadow.counts.y, from the CPU) and the cutout bucket; translucent quads never
// cast. Edge walls follow cull.wgsl's rule.
//
// Lists: cascade c and page p own draws [(c * L + p) * 4C, ... + 4C), the opaque ones first
// (3C: at most 3 buckets of a chunk look away from the sun), then the cutout ones (C), with
// L = shadow.counts.w pages and C = shadow.counts.z chunks. Their counts are
// state.counts[(2c + k) * MAX_PAGES + p], k = 0 opaque, 1 cutout. A list that is full drops the
// bucket (its shadow is missing: today's look) and counts it in state.dropped. As in cull.wgsl,
// every value stays in range for the unvalidated *_count draws, and writes stay inside the lists
// that exist (c < cascades the CPU sized them for, p < L).
struct DrawArgs { index_count: u32, instance_count: u32, first_index: u32, base_vertex: i32, first_instance: u32 };

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> chunks: array<ChunkGpu>;
@group(0) @binding(2) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(3) var<storage, read_write> draw_refs: array<vec2<u32>>;
// counts: 2 per cascade and page id (cleared every frame, read by the indirect count draws);
// dropped[0]: buckets left out because their list was full.
struct ShadowCullState { counts: array<atomic<u32>, 2048>, dropped: array<atomic<u32>, 4> };
@group(0) @binding(4) var<storage, read_write> state: ShadowCullState;
@group(0) @binding(5) var<uniform> shadow: array<ShadowCascade, 4>;

// Quads covered by the shared index buffer (boxel_mesh::MAX_QUADS_PER_BUCKET, checked by a naga test).
const MAX_QUADS_PER_BUCKET: u32 = 98304u;
// Page ids are below this (boxel_shaders::gpu::chunk_memory::MAX_PAGES).
const MAX_PAGES: u32 = 256u;
// A face bucket's count word (boxel_shaders::gpu::ChunkGpu::COUNT_BITS, WALL_SHIFT).
const COUNT_MASK: u32 = 0x1FFFFFu;
const WALL_SHIFT: u32 = 21u;
// ChunkGpu::flags bit 31 (boxel_shaders::gpu::ChunkGpu::FLAG_SKY_LIT): some face of the chunk
// has sky light at a corner.
const FLAG_SKY_LIT: u32 = 0x80000000u;

fn in_box(c: u32, mn: vec3<f32>, mx: vec3<f32>) -> bool {
  for (var k = 0u; k < 6u; k++) {
    let pl = shadow[c].planes[k];
    let pv = select(mn, mx, pl.xyz >= vec3(0.0));
    if (dot(pl.xyz, pv) + pl.w < 0.0) { return false; }
  }
  return true;
}

// The next draw of a list of `cap` draws whose count is state.counts[k]. At or past `cap` the
// list is full: the increment is undone and the bucket counted as dropped.
fn take(k: u32, cap: u32) -> u32 {
  let s = atomicAdd(&state.counts[k], 1u);
  if (s >= cap) {
    atomicSub(&state.counts[k], 1u);
    atomicAdd(&state.dropped[0], 1u);
  }
  return s;
}

@compute @workgroup_size(64)
fn cull_shadow_casters(@builtin(global_invocation_id) gid: vec3<u32>) {
  let slot = gid.x;
  if (slot >= view.counts.x) { return; }
  let ch = chunks[slot];
  if ((ch.flags & 1u) == 0u || (ch.flags & FLAG_SKY_LIT) == 0u) { return; }
  let frame = shadow[0];
  let pages = min(frame.counts.w, MAX_PAGES);
  let p = ch.page;
  if (p >= pages) { return; }
  // Wrapping i32 subtraction, exact for live chunks (see cull.wgsl); live chunks are less than
  // 2^31 - 32 away, so adding 32 cannot wrap.
  let rel = ch.origin - view.render_origin.xyz;
  if (rel.y >= frame.band.y || rel.y + 32 <= frame.band.x) { return; }
  let mn = vec3<f32>(rel);
  let mx = mn + vec3(CHUNK_SIZE);
  let cap = frame.counts.z;
  let cascades = min(frame.counts.x, MAX_SHADOW_CASCADES);
  for (var c = 0u; c < cascades; c++) {
    if (!in_box(c, mn, mx)) { continue; }
    let base = (c * pages + p) * 4u * cap;
    var start = ch.quad_base;
    for (var f = 0u; f < 6u; f++) {
      let n = ch.counts[f] & COUNT_MASK;
      let across = ((ch.flags >> (1u + f)) & 1u) != 0u;
      let drawn = n - select(0u, min(ch.counts[f] >> WALL_SHIFT, n), across);
      if (drawn > 0u && ((frame.counts.y >> f) & 1u) != 0u) {
        let s = take((2u * c) * MAX_PAGES + p, 3u * cap);
        if (s < 3u * cap) {
          let i = base + s;
          draw_args[i] = DrawArgs(min(drawn, MAX_QUADS_PER_BUCKET) * 6u, 1u, 0u, 0, i);
          draw_refs[i] = vec2(slot, start);
        }
      }
      start += n;
    }
    // the cutout bucket: all faces mixed, vs_shadow_cutout drops the ones facing the sun
    let nc = ch.counts[6];
    if (nc > 0u) {
      let s = take((2u * c + 1u) * MAX_PAGES + p, cap);
      if (s < cap) {
        let i = base + 3u * cap + s;
        draw_args[i] = DrawArgs(min(nc, MAX_QUADS_PER_BUCKET) * 6u, 1u, 0u, 0, i);
        draw_refs[i] = vec2(slot, start);
      }
    }
  }
}
