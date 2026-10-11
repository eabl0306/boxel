// Far LOD nodes (ADR 0031), tier High: GPU frustum and per-face-bucket culling of the node table.
// Prepended with common.wgsl.
//
// One invocation per node slot. A live node (its quads in the arena, inside the GPU window) with
// an octant left to draw whose box (32 * 2^k blocks) meets the frustum writes one indexed draw per
// face bucket that can face the camera (the box-level back-face test of cull.wgsl) into the list
// of its segment, with its DrawRef. The skirts and fluid walls at the end of a bucket are left out
// where nothing shows them (bucket_quads): slots [s * LOD_SEGMENT_SLOTS, (s + 1) * LOD_SEGMENT_SLOTS) write
// list s, which starts at draw s * LOD_SEGMENT_DRAWS and has its count in counts[s]. A segment
// has at most 6 * LOD_SEGMENT_SLOTS <= 65535 draws (Vulkan's minimum maxDrawIndirectCount), so no
// list can overflow and every draw is written. The *_count path is NOT validated by wgpu: index
// counts stay within the shared index buffer (MAX_QUADS_PER_BUCKET quads).
//
// The cull runs after the opaque and cutout terrain is drawn, with its depth pyramid
// (lod_hiz.wgsl): a node whose nearest point lies behind the farthest full-detail depth over its
// whole screen rectangle (a pixel of margin for the edge growth of finish_vertex) would lose
// every depth test, so it is not drawn. Underground, where the rock in front covers the screen,
// nothing of the LOD is. Those frames count the nodes in the frustum and the ones the pyramid
// hid; while it hides almost nothing the renderer culls without it (occlusion[2] = 0), before
// the main pass, for some frames (boxel_render's lod::pass).
struct DrawArgs { index_count: u32, instance_count: u32, first_index: u32, base_vertex: i32, first_instance: u32 };
struct LodNodeGpu { node: vec4<i32>, quad_base: u32, counts: array<u32, 3>, seams: array<u32, 6>, pad: array<u32, 2> };

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> nodes: array<LodNodeGpu>;
@group(0) @binding(2) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(3) var<storage, read_write> draw_refs: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read_write> counts: array<atomic<u32>>;
@group(0) @binding(5) var hiz: texture_2d<f32>;
// [nodes in the frustum, nodes the pyramid hid, use the pyramid (written by the CPU), unused].
@group(0) @binding(6) var<storage, read_write> occlusion: array<atomic<u32>, 4>;

const LOD_LEVEL_MASK: u32 = 15u;
const LOD_HIDDEN_MASK: u32 = 0xFFu;
const LOD_LIVE_BIT: u32 = 0x4000u;
// Bit LOD_ACROSS_SHIFT + f of node.w: the node of this level across face f draws its cells by
// that face (boxel_shaders::gpu::lod::ACROSS_SHIFT).
const LOD_ACROSS_SHIFT: u32 = 15u;
// Slots per segment, and the draws its list holds (boxel_shaders::gpu::lod, checked by a naga test).
const LOD_SEGMENT_SLOTS: u32 = 10922u;
const LOD_SEGMENT_DRAWS: u32 = 65532u;
// A node's nearest depth is taken this much nearer before the occlusion test (occluded).
const LOD_HIZ_MARGIN: f32 = 1.004;
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

// The quads of face bucket f (count c, its seam quads at the end: on a node face in the low 16
// bits of `seams`, then on a middle plane, the last ones, in the high 16) for a node whose
// octants `hidden` are hidden. The seam quads close the step to other detail across their plane
// and lie between solid cells everywhere else: the middle ones are needed only where an octant
// is drawn while the one across the middle plane behind face f is hidden (then the whole bucket
// is drawn, one run), the node face ones only where the node across is not of this level or does
// not draw by that face (`across` false). The mirror of boxel_shaders::gpu::lod::bucket_quads.
fn bucket_quads(c: u32, seams: u32, f: u32, hidden: u32, across: bool) -> u32 {
  // The octant bit of face f's axis (faces 0-1 y, 2-3 x, 4-5 z) and the octants on its low side.
  let bit = select(select(4u, 1u, f < 4u), 2u, f < 2u);
  let low = select(select(0x0Fu, 0x55u, f < 4u), 0x33u, f < 2u);
  let drawn = ~hidden & 0xFFu;
  var exposed = drawn & ((hidden << bit) & ~low);
  if ((f & 1u) == 0u) {
    exposed = drawn & (hidden >> bit) & low;
  }
  if (exposed != 0u) { return c; }
  let skip = (seams >> 16u) + select(0u, seams & 0xFFFFu, across);
  return c - min(skip, c);
}

// True if the box [mn, mx] (relative to the render origin) lies behind full detail over its
// whole screen rectangle. Never for a box reaching the camera's plane.
fn occluded(mn: vec3<f32>, mx: vec3<f32>) -> bool {
  var lo = vec2(1.0e9);
  var hi = vec2(-1.0e9);
  var nearest = 0.0;
  for (var c = 0u; c < 8u; c++) {
    let p = select(mn, mx, vec3((c & 1u) != 0u, (c & 2u) != 0u, (c & 4u) != 0u));
    let clip = view.view_proj * vec4(p, 1.0);
    if (clip.w <= 1.0e-3) { return false; }
    let ndc = clip.xyz / clip.w;
    lo = min(lo, ndc.xy);
    hi = max(hi, ndc.xy);
    nearest = max(nearest, ndc.z);
  }
  let size = view.viewport.xy;
  // NDC to pixels (y down), a pixel of margin, clamped to the target.
  let p0 = clamp(vec2(lo.x * 0.5 + 0.5, 0.5 - hi.y * 0.5) * size - 1.0, vec2(0.0), size - 1.0);
  let p1 = clamp(vec2(hi.x * 0.5 + 0.5, 0.5 - lo.y * 0.5) * size + 1.0, vec2(0.0), size - 1.0);
  // The level whose texels (2^(m+1) pixels) are at least the rectangle's size: it touches at
  // most 2 x 2 of them.
  let extent = max(p1.x - p0.x, p1.y - p0.y);
  // The pyramid halves its level 0 (rounding down) until its longer side is 1: its last level
  // is floor(log2(longer side)).
  let side = textureDimensions(hiz, 0);
  let top = i32(firstLeadingBit(max(side.x, side.y)));
  let m = clamp(i32(ceil(log2(max(extent, 1.0)))) - 1, 0, top);
  let texel = f32(2 << u32(m));
  let last = vec2<i32>(textureDimensions(hiz, m)) - 1;
  let t0 = min(vec2<i32>(floor(p0 / texel)), last);
  let t1 = min(vec2<i32>(floor(p1 / texel)), last);
  let farthest = min(
    min(textureLoad(hiz, t0, m).r, textureLoad(hiz, vec2(t1.x, t0.y), m).r),
    min(textureLoad(hiz, vec2(t0.x, t1.y), m).r, textureLoad(hiz, t1, m).r),
  );
  // The edge growth of finish_vertex moves a vertex by up to half a pixel, which can bring it
  // nearer than the box by about that much: 0.07 % of its distance. A margin of 0.4 % covers it.
  return nearest * LOD_HIZ_MARGIN < farthest;
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
  if (atomicLoad(&occlusion[2]) != 0u) {
    atomicAdd(&occlusion[0], 1u);
    if (occluded(mn, mx)) {
      atomicAdd(&occlusion[1], 1u);
      return;
    }
  }
  let seg = slot / LOD_SEGMENT_SLOTS;
  let first = seg * LOD_SEGMENT_DRAWS;
  var start = n.quad_base;
  let hidden = w & LOD_HIDDEN_MASK;
  for (var f = 0u; f < 6u; f++) {
    let c = (n.counts[f >> 1u] >> ((f & 1u) * 16u)) & 0xFFFFu;
    let across = ((w >> (LOD_ACROSS_SHIFT + f)) & 1u) != 0u;
    let q = bucket_quads(c, n.seams[f], f, hidden, across);
    if (q > 0u && bucket_visible(f, mn, mx, view.camera_rel.xyz)) {
      let s = first + atomicAdd(&counts[seg], 1u);
      draw_args[s] = DrawArgs(min(q, MAX_QUADS_PER_BUCKET) * 6u, 1u, 0u, 0, s);
      draw_refs[s] = vec2(slot, start);
    }
    start += c;
  }
}
