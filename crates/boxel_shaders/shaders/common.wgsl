// boxel: shared voxel shader code (all tiers). Prepended to voxel_pull.wgsl, voxel_instanced.wgsl and cull.wgsl.
//
// PackedQuad = vec2<u32>(geo, mat), 8 bytes, identical bytes on every tier.
//   geo: x:5 | y:5 | z:5 | (w-1):5 | (h-1):5 | face:3 | flags:4 (bit 0 = FLAG_EMISSIVE, bits 1..3 =
//        shape code, ADR 0018, 0020, 0021 and 0025: cube, turned cube, fluid cube, cross, fluid
//        surface, model box face, sloped fluid cell; a box face keeps the index of its box in the
//        View's box table in the (w-1, h-1) bits, a sloped fluid cell its 4 corner heights there
//        and in the AO bits, and the bit after the heights for the water of a waterlogged block)
//   mat: layer:12 | ao:8 (2 bits per corner, 3 = unoccluded) | light:12 (index into the chunk's
//        light palette)
// face: 0=+Y 1=-Y 2=+X 3=-X 4=+Z 5=-Z  (u,v axes: ±Y -> (X,Z), ±X -> (Z,Y), ±Z -> (X,Y))
//
// Light (ADR 0015): a palette entry is vec2<u32>(c0 | c1 << 16, c2 | c3 << 16), the light of
// the quad's 4 corners in the AO corner order. Each corner is a LightValue: sky:4 | red:4 |
// green:4 | blue:4 (sky in the high nibble). High and Mid read the entry from the quad arena at
// ChunkGpu::light_base + index; Low gets it already expanded per quad as a second instance
// attribute. The fragment interpolates the 4 corners bilinearly at the quad-local (s, t), the
// same on every tier.
//
// RULES (enforced by boxel_shaders tests):
//  - never use @builtin(instance_index), @builtin(draw_index), @builtin(num_workgroups);
//    per-draw data arrives through the DrawRef instance-step vertex attribute or immediates.
//  - @builtin(vertex_index) is only used with first_vertex = 0 / base_vertex = 0 draws.
//  - textureLoad mip levels are always i32 (naga GLSL bug gfx-rs/wgpu#10037).
//
// World integer positions are i64 on the CPU. Chunk origins and render_origin arrive as their
// low 32 bits (two's complement wrapping) and are only ever subtracted from each other in i32.
// WGSL integer arithmetic wraps, so (chunk_origin - render_origin) is the true difference modulo
// 2^32: exact whenever the chunk is less than 2^31 voxels from the camera, at any world position.
// Resident chunks can be farther (the old area right after a teleport); the renderer never draws
// those: only chunks within about 2^30 voxels of the camera are drawn (boxel_render chunk_table).
// The CPU sets the live flag only on them (High culls only live chunks), and the CPU cull of Mid
// and Low skips the rest.

const GEO_POS_MASK: u32 = 31u;
const GEO_Y_SHIFT: u32 = 5u;
const GEO_Z_SHIFT: u32 = 10u;
const GEO_W_SHIFT: u32 = 15u;
const GEO_H_SHIFT: u32 = 20u;
const GEO_FACE_SHIFT: u32 = 25u;
const GEO_FLAGS_SHIFT: u32 = 28u;
// Flag of a quad whose block emits light: drawn at the full brightness of its texture.
const FLAG_EMISSIVE: u32 = 1u;
// Shape code (boxel_mesh::layout::SHAPE_*): how the quad's cell and face become geometry.
const GEO_SHAPE_SHIFT: u32 = 29u;
const GEO_SHAPE_MASK: u32 = 7u;
const SHAPE_TURNED: u32 = 1u;      // cube face, texture transposed (logs along X or Z)
const SHAPE_FLUID_CUBE: u32 = 2u;  // cube face of a fluid cell under its fluid or a block (greedy)
const SHAPE_CROSS: u32 = 4u;       // plant quad through the cell's diagonal (w = h = 1)
const SHAPE_FLUID_SURFACE: u32 = 5u; // fluid surface cell: its top drops to FLUID_SURFACE (greedy)
const SHAPE_FLUID_SLOPE: u32 = 7u; // fluid cell with a height per top corner (w = h = 1, no AO)
const FLUID_SURFACE: f32 = 0.875;
// The water of a waterlogged block (a sloped fluid cell, ADR 0025): its side and bottom faces
// sit FLUID_INSET inside the cell, behind the block's own faces on the border.
const GEO_FLUID_INSET_BIT: u32 = 0x800000u;
const FLUID_INSET: f32 = 0.001953125; // 1/512
// A fluid quad's depth is scaled by this (1 - 2^-16, ADR 0025): reverse-Z, so it lands that
// fraction of its distance farther and loses every depth tie (finish_vertex).
const FLUID_DEPTH_SCALE: f32 = 0.9999847412109375;
// The flow texture scrolls this many textures a second (ADR 0025): water, and lava (the
// emissive fluid) slower. View::viewport.z wraps at a whole number of both periods.
const WATER_FLOW_SPEED: f32 = 0.5;
const LAVA_FLOW_SPEED: f32 = 0.25;
const SHAPE_BOX: u32 = 6u;         // a face of one model box (slabs, stairs, fences, torches; w = h = 1)
const GEO_BOX_MASK: u32 = 1023u;   // the box index of a SHAPE_BOX quad, at GEO_W_SHIFT
const BOX_COORD_BITS: u32 = 5u;    // packed box word 0: min xyz, max xyz in sixteenths
const BOX_OFFSET_BITS: u32 = 6u;   // packed box word 1: texture offset xyz, biased by BOX_OFFSET_BIAS
const BOX_OFFSET_BIAS: i32 = 32;
const MAT_LAYER_MASK: u32 = 0xFFFu;
const MAT_AO_SHIFT: u32 = 12u;
const MAT_LIGHT_SHIFT: u32 = 20u;
const MAT_LIGHT_MASK: u32 = 0xFFFu;
const LIGHT_SKY_SHIFT: u32 = 12u;
const LIGHT_RED_SHIFT: u32 = 8u;
const LIGHT_GREEN_SHIFT: u32 = 4u;
const LIGHT_BLUE_SHIFT: u32 = 0u;
const LIGHT_MAX_LEVEL: f32 = 15.0;
// A light palette entry with sky 15 at the 4 corners (boxel_mesh::FULL_SKY_ENTRY).
const FULL_SKY_LIGHT: vec2<u32> = vec2<u32>(0xF000F000u, 0xF000F000u);
const CHUNK_SIZE: f32 = 32.0;

struct View {
  view_proj: mat4x4<f32>,          // camera-relative: positions are (world - render_origin)
  frustum: array<vec4<f32>, 6>,    // xyz = inward normal, w = d; inside if dot(n, p) + d >= 0
  render_origin: vec4<i32>,        // xyz = integer voxel origin of the camera-relative frame (low 32 bits)
  camera_rel: vec4<f32>,           // xyz = eye position relative to render_origin
  fog_color: vec4<f32>,
  fog_params: vec4<f32>,           // x = fog start, y = fog end (world units)
  counts: vec4<u32>,               // x = chunk slot count, y = pages with draw lists (L), z = opaque draws per page list (stride; the cutout lists hold stride / 6, after L opaque ones), w = 1: cave culling bits in use (High)
  sky_light: vec4<f32>,            // rgb = sky light color times the daylight intensity (linear), a = ambient floor
  light_params: vec4<f32>,         // x = AO strength (0.55: the darkest corner keeps 45 %), y = block-light gain, zw = 0
  light_curve: array<vec4<f32>, 4>, // brightness of light levels 0..15 (level 4k + j at [k][j]; level 15 = 1)
  viewport: vec4<f32>,             // xy = render target size in pixels, z = fluid animation time in seconds (wraps), w = still flow textures (bit 0 water, bit 1 lava; ADR 0028)
  // Model box table (ADR 0020), written once per renderer: box k is boxes[k >> 1].xy (k even) or
  // .zw (k odd), packed as boxel_mesh::pack_model_box. Read only by SHAPE_BOX quads.
  boxes: array<vec4<u32>, 512>,
};

struct ChunkGpu {                  // 64 bytes, mirrors boxel_shaders::gpu::ChunkGpu
  origin: vec3<i32>,               // world voxel min corner of the chunk, low 32 bits (wraps; only used minus render_origin)
  quad_base: u32,                  // first quad of bucket 0 in the chunk's page
  counts: array<u32, 8>,           // [0..6) opaque face buckets, [6] cutout, [7] translucent
  flags: u32,                      // bit0 = live
  light_base: u32,                 // first light palette entry in the chunk's page (= quad_base + total quads)
  light_len: u32,                  // light palette entries (>= 1); a quad index not below it reads open sky
  page: u32,                       // the page holding the chunk (< MAX_PAGES); only the High cull reads it
};

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) layer: u32,
  @location(2) @interpolate(flat) face: u32,
  @location(3) view_dist: f32,
  @location(4) st: vec2<f32>,                       // quad-local (s, t): corner k sits at (k & 1, k >> 1)
  @location(5) @interpolate(flat) light: vec2<u32>, // the quad's light palette entry (4 corner lights)
  @location(6) @interpolate(flat) ao: u32,          // the quad's AO byte (corner k at bits 2k..2k+2), then its 4 flags (bits 8..12; FLAG_EMISSIVE)
};

struct Decoded {
  local: vec3<f32>,
  uv: vec2<f32>,
  st: vec2<f32>,
  face: u32,
  layer: u32,
  light: vec2<u32>,
  ao: u32,
  flags: u32,
  grow: vec2<f32>,                 // the corner's outward direction along u and v (-1 or 1)
};

fn face_normal(f: u32) -> vec3<f32> {
  switch f {
    case 0u: { return vec3(0.0, 1.0, 0.0); }
    case 1u: { return vec3(0.0, -1.0, 0.0); }
    case 2u: { return vec3(1.0, 0.0, 0.0); }
    case 3u: { return vec3(-1.0, 0.0, 0.0); }
    case 4u: { return vec3(0.0, 0.0, 1.0); }
    default: { return vec3(0.0, 0.0, -1.0); }
  }
}
fn face_u(f: u32) -> vec3<f32> { if (f == 2u || f == 3u) { return vec3(0.0, 0.0, 1.0); } return vec3(1.0, 0.0, 0.0); }
fn face_v(f: u32) -> vec3<f32> { if (f < 2u) { return vec3(0.0, 0.0, 1.0); } return vec3(0.0, 1.0, 0.0); }
fn face_shade(f: u32) -> f32 {
  switch f {
    case 0u: { return 1.0; }
    case 1u: { return 0.5; }
    case 2u, 3u: { return 0.8; }
    default: { return 0.65; }
  }
}

// corner 0..3 = (u,v) (0,0) (1,0) (0,1) (1,1); index pattern per quad = [0,1,2, 2,1,3] (strip order 0,1,2,3)
// `light` is the quad's light palette entry (High/Mid: read from the arena; Low: an instance attribute).
fn decode_quad(q: vec2<u32>, light: vec2<u32>, corner_in: u32) -> Decoded {
  let geo = q.x;
  let mat = q.y;
  let shape = (geo >> GEO_SHAPE_SHIFT) & GEO_SHAPE_MASK;
  // A sloped fluid quad is one cell: its extent bits and AO byte hold its corner heights.
  let sloped = shape == SHAPE_FLUID_SLOPE;
  let p = vec3<f32>(f32(geo & GEO_POS_MASK), f32((geo >> GEO_Y_SHIFT) & GEO_POS_MASK), f32((geo >> GEO_Z_SHIFT) & GEO_POS_MASK));
  let w = select(f32(((geo >> GEO_W_SHIFT) & GEO_POS_MASK) + 1u), 1.0, sloped);
  let h = select(f32(((geo >> GEO_H_SHIFT) & GEO_POS_MASK) + 1u), 1.0, sloped);
  let f = (geo >> GEO_FACE_SHIFT) & 7u;
  let n = face_normal(f);
  let u = face_u(f);
  let v = face_v(f);
  var c = corner_in;
  // keep counter-clockwise winding seen from outside: swap corners 1 and 2 when (u x v) points inward
  if (dot(cross(u, v), n) < 0.0) { if (c == 1u) { c = 2u; } else if (c == 2u) { c = 1u; } }
  let cu = f32(c & 1u);
  let cv = f32(c >> 1u);
  var o: Decoded;
  o.local = p + max(n, vec3(0.0)) + u * (cu * w) + v * (cv * h);
  o.uv = select(vec2(cu * w, cv * h), vec2(cu * w, (1.0 - cv) * h), f >= 2u); // side faces: v points down in texture space
  o.st = vec2(cu, cv);
  o.face = f;
  o.layer = mat & MAT_LAYER_MASK;
  o.light = light;
  o.ao = select((mat >> MAT_AO_SHIFT) & 0xFFu, 0xFFu, sloped);
  o.flags = (geo >> GEO_FLAGS_SHIFT) & 15u;
  o.grow = vec2(2.0 * cu - 1.0, 2.0 * cv - 1.0);
  if (shape == SHAPE_BOX) {
    // A face of one model box (ADR 0020): the quad's (w-1, h-1) bits index the View's box
    // table. The face lies on the box's side toward the face's normal and spans the box along
    // u and v; its light/AO coordinates (s, t) are its place on the cube face, so it shades like
    // that part of the cube face. The texture is the part of the cube face's texture under the
    // box moved by its texture offset (Minecraft's uvlock): every value is a sixteenth, exact.
    let index = (geo >> GEO_W_SHIFT) & GEO_BOX_MASK;
    let pair = view.boxes[index >> 1u];
    let odd = (index & 1u) == 1u;
    let w0 = select(pair.x, pair.z, odd);
    let w1 = select(pair.y, pair.w, odd);
    let lo = vec3<f32>((vec3(w0) >> vec3(0u, BOX_COORD_BITS, 2u * BOX_COORD_BITS)) & vec3(31u)) / 16.0;
    let hi = vec3<f32>((vec3(w0) >> vec3(3u * BOX_COORD_BITS, 4u * BOX_COORD_BITS, 5u * BOX_COORD_BITS)) & vec3(31u)) / 16.0;
    let raw = (vec3(w1) >> vec3(0u, BOX_OFFSET_BITS, 2u * BOX_OFFSET_BITS)) & vec3(63u);
    let shift = vec3<f32>(vec3<i32>(raw) - vec3(BOX_OFFSET_BIAS)) / 16.0;
    let an = abs(n);
    let su = select(dot(lo, u), dot(hi, u), (c & 1u) == 1u);
    let sv = select(dot(lo, v), dot(hi, v), (c >> 1u) == 1u);
    let depth = select(dot(lo, an), dot(hi, an), (f & 1u) == 0u);
    o.local = p + an * depth + u * su + v * sv;
    let tu = su + dot(shift, u);
    let tv = sv + dot(shift, v);
    o.uv = select(vec2(tu, tv), vec2(tu, 1.0 - tv), f >= 2u);
    o.st = vec2(su, sv);
  } else if (shape == SHAPE_FLUID_SURFACE) {
    // Fluid surface (water): the cell's top drops to FLUID_SURFACE. Side faces span y in
    // [0, FLUID_SURFACE] (h = 1: a surface cell has no surface cell of its fluid above or below)
    // with their texture row and light coordinate t following the real y, as slabs do; the +Y
    // face moves down to it; the -Y face stays.
    if (f >= 2u) {
      let y = FLUID_SURFACE * cv;
      o.local.y = p.y + y;
      o.uv.y = 1.0 - y;
      o.st.y = y;
    } else if (f == 0u) {
      o.local.y = p.y + FLUID_SURFACE;
    }
  } else if (sloped) {
    // Sloped fluid cell (ADR 0021): corner k of the top, at (x, z) = (k & 1, k >> 1), is
    // ((heights >> 4k) & 15) + 1 sixteenths high (corners 0 and 1 in the extent bits, 2 and 3
    // in the AO byte). The +Y face runs through the 4 corners; a side face runs from y = 0 up
    // to the two corners of its edge, its texture row and light coordinate t following the
    // real y as on slabs; the -Y face stays.
    let heights = ((geo >> GEO_W_SHIFT) & 0xFFu) | (((mat >> MAT_AO_SHIFT) & 0xFFu) << 8u);
    var k = 0u;
    if (f == 0u) {
      k = c;
    } else if (f == 2u || f == 3u) {
      // u = z: the edge at x = 1 (+X) or 0 (-X)
      k = select(0u, 1u, f == 2u) + 2u * (c & 1u);
    } else if (f >= 4u) {
      // u = x: the edge at z = 1 (+Z) or 0 (-Z)
      k = (c & 1u) + select(0u, 2u, f == 4u);
    }
    let top = f32(((heights >> (4u * k)) & 15u) + 1u) / 16.0;
    if (f == 0u) {
      o.local.y = p.y + top;
      // A sloping top shows the flow texture (the mesher picked it): turned about the cell's
      // middle so that the texture's v runs down the slope, the integer direction of
      // boxel_mesh::fluid_flow from the corner heights.
      let hk = vec4<i32>((vec4(heights) >> vec4(0u, 4u, 8u, 12u)) & vec4(15u));
      let flow = vec2<f32>(vec2((hk.x + hk.z) - (hk.y + hk.w), (hk.x + hk.y) - (hk.z + hk.w)));
      if (any(flow != vec2(0.0))) {
        let along = flow * inverseSqrt(dot(flow, flow));
        let q = vec2(cu, cv) - vec2(0.5);
        o.uv = vec2(dot(q, vec2(-along.y, along.x)), dot(q, along)) + vec2(0.5, 0.5 - flow_scroll(o.flags));
      }
    } else if (f >= 2u) {
      let y = top * cv;
      o.local.y = p.y + y;
      o.uv.y = 1.0 - y;
      o.st.y = y;
    }
    // The water of a waterlogged block: its faces but the top inset (ADR 0025).
    if ((geo & GEO_FLUID_INSET_BIT) != 0u && f != 0u) {
      o.local = o.local - n * FLUID_INSET;
    }
  } else if (shape == SHAPE_CROSS) {
    // Face 2 (+X) runs from (0, 0) to (1, 1) in (x, z), face 4 (+Z) from (1, 0) to (0, 1); full
    // height. Drawn from both sides at full face brightness (face 0's shade, as Minecraft draws
    // plants) and never grown: it meets no other quad.
    let start = select(vec3(1.0, 0.0, 0.0), vec3(0.0), f == 2u);
    let along = select(vec3(-1.0, 0.0, 1.0), vec3(1.0, 0.0, 1.0), f == 2u);
    o.local = p + start + along * cu + vec3(0.0, cv, 0.0);
    o.uv = vec2(cu, 1.0 - cv);
    o.face = 0u;
    o.grow = vec2(0.0);
  }
  // The side faces of a fluid show its flow texture running down (ADR 0025).
  let fluid = shape == SHAPE_FLUID_CUBE || shape == SHAPE_FLUID_SURFACE || sloped;
  if (fluid && f >= 2u) {
    o.uv.y = o.uv.y - flow_scroll(o.flags);
  }
  return o;
}

// How far the flow texture of a fluid quad with these flags has scrolled (0..1 of a texture):
// the time in View::viewport.z at WATER_FLOW_SPEED, or LAVA_FLOW_SPEED for an emissive fluid.
// A flow texture its resource pack animates moves by its frames, like Minecraft's, and does not
// scroll (ADR 0028): bit 0 of View::viewport.w for water, bit 1 for lava.
fn flow_scroll(flags: u32) -> f32 {
  let lava = (flags & FLAG_EMISSIVE) != 0u;
  let speed = select(WATER_FLOW_SPEED, LAVA_FLOW_SPEED, lava);
  let still = (u32(view.viewport.w) >> select(0u, 1u, lava)) & 1u;
  return select(fract(view.viewport.z * speed), 0.0, still != 0u);
}

// Edge growth: no cracks at T-junctions. Greedy quads meet in T-junctions (a corner of one quad
// lies on an edge of another, on the same plane or on the edge of a block). The rasterizer snaps
// every vertex to its sub-pixel grid on its own, so such a corner can land a fraction of a
// sub-pixel off that edge, and a pixel center in the sliver between them is covered by neither
// quad: a one-pixel crack that shows the sky through the hollow inside of the terrain. Every
// corner moves outward along its quad's two edge directions, in the quad's plane, so that each
// edge moves EDGE_GROWTH_PX pixels on screen across itself (perpendicular to the edge): about 3
// times the worst snapping error (about 1/180 of a pixel with 8 sub-pixel bits), far too little
// to see. Across, not along: the corner moves along the other edge's direction, and where the
// two edges look almost parallel on screen (a player's view of the ground, about 40 degrees off
// the axes, or anything seen at a grazing angle) only sin(angle) of that move closes the crack,
// so the move along each direction is EDGE_GROWTH_PX / sin(angle) pixels. It stops at
// EDGE_GROWTH_LEN pixels, so the corner of a quad seen almost edge-on does not stick out; the
// gap then still closes by EDGE_GROWTH_LEN * sin(angle). It is kept that small because it costs:
// neighbors overlap by that much, and some of the overlap is shaded twice (on the RTX 5050
// growing 1/16 to 1/8 of a pixel cost 5 to 7 % of the GPU frame; at 1/64 only the math below is
// left, about 3 %). The texture coordinates move with the corner, so the texture inside the quad
// maps exactly as before; the light coordinates do not (cheaper, and the light inside moves by
// less than EDGE_GROWTH_LEN pixels).
const EDGE_GROWTH_PX: f32 = 0.015625;
const EDGE_GROWTH_LEN: f32 = 0.25;

// The world distances along two axes whose clip-space steps per voxel are `axis_u` and `axis_v`
// (columns of view_proj) that move the edges of a quad at `clip` by EDGE_GROWTH_PX pixels across
// themselves on screen; `half_px` is half the render target size in pixels. Each moves the point
// at most EDGE_GROWTH_LEN pixels and a quarter of its depth (w), so a corner near the eye cannot
// reach the eye; both are 0 at or behind the eye.
fn edge_growth(clip: vec4<f32>, axis_u: vec4<f32>, axis_v: vec4<f32>, half_px: vec2<f32>) -> vec2<f32> {
  // d(ndc.xy)/dt = n / w^2 (pixels per voxel), so the distance that moves the point L pixels is
  // L * w^2 / |n|
  let nu = (axis_u.xy * clip.w - clip.xy * axis_u.w) * half_px;
  let nv = (axis_v.xy * clip.w - clip.xy * axis_v.w) * half_px;
  let inv = inverseSqrt(max(vec2(dot(nu, nu), dot(nv, nv)), vec2(1e-20)));
  // sin of the angle between the two directions on screen
  let s = abs(nu.x * nv.y - nu.y * nv.x) * inv.x * inv.y;
  let len = EDGE_GROWTH_PX / max(s, EDGE_GROWTH_PX / EDGE_GROWTH_LEN);
  let t = min((len * clip.w * clip.w) * inv, vec2(0.25 * clip.w));
  return select(vec2(0.0), t, clip.w > 0.0);
}

fn finish_vertex(d: Decoded, chunk_origin: vec3<i32>, view: View) -> VsOut {
  // wrapping i32 subtraction first: exact for any world position (see the top), then small floats
  let rel = vec3<f32>(chunk_origin - view.render_origin.xyz) + d.local;
  let clip = view.view_proj * vec4(rel, 1.0);
  // Edge growth (above). Moving t voxels along a world axis moves clip by t times that axis's
  // column of view_proj.
  let axis_u = select(view.view_proj[0], view.view_proj[2], d.face == 2u || d.face == 3u);
  let axis_v = select(view.view_proj[1], view.view_proj[2], d.face < 2u);
  let t = d.grow * edge_growth(clip, axis_u, axis_v, 0.5 * view.viewport.xy);
  var o: VsOut;
  o.clip = clip + axis_u * t.x + axis_v * t.y;
  // Depth ties (ADR 0025): a fluid's faces lie on its cell's border, and so can the faces it
  // touches: the far side of glass or leaves next to lava, the top of a stair step or a fence
  // post in water at 16/16. Those are drawn with cull_mode None or after the fluid, all with a
  // strict Greater test, so on equal depths rounding would pick a winner per pixel (z-fighting).
  // A fluid's depth (near / w, reverse-Z) is scaled by FLUID_DEPTH_SCALE: 2^-16 of its distance
  // farther. That is about 100 times the f32 rounding of a depth, and up to 4096 blocks away
  // (far past any view distance) less than the 1/16 of a block between a fluid face and
  // anything truly behind it (box faces and fluid heights are sixteenths). Every face on a fluid
  // face's plane wins. The vertex does not move on screen, so no crack opens.
  let shape = d.flags >> 1u;
  if (shape == SHAPE_FLUID_CUBE || shape == SHAPE_FLUID_SURFACE || shape == SHAPE_FLUID_SLOPE) {
    o.clip.z = o.clip.z * FLUID_DEPTH_SCALE;
  }
  // side faces: the texture's v points down (decode_quad)
  o.uv = d.uv + vec2(t.x, select(t.y, -t.y, d.face >= 2u));
  // a turned cube face (logs along X or Z): the texture's vertical runs along u
  if ((d.flags >> 1u) == SHAPE_TURNED) { o.uv = o.uv.yx; }
  o.layer = d.layer;
  o.face = d.face;
  o.view_dist = length(rel - view.camera_rel.xyz);
  o.st = d.st;
  o.light = d.light;
  o.ao = d.ao | (d.flags << 8u);
  return o;
}

// Opaque buckets never discard (keeps early-Z); cutout buckets use the alpha test below;
// the translucent bucket blends with the texture's alpha.
fn alpha_test_passes(albedo: vec4<f32>) -> bool { return albedo.a >= 0.5; }

// Light levels (sky, red, green, blue) of corner k of a light palette entry.
fn corner_light(words: vec2<u32>, k: u32) -> vec4<f32> {
  let c = select(words.x, words.y, k >= 2u) >> (16u * (k & 1u));
  let shifts = vec4(LIGHT_SKY_SHIFT, LIGHT_RED_SHIFT, LIGHT_GREEN_SHIFT, LIGHT_BLUE_SHIFT);
  return vec4<f32>((vec4(c) >> shifts) & vec4(15u));
}

// Bilinear interpolation of 4 corner values at (s, t), corner k at (k & 1, k >> 1). Written as
// a + (b - a) * s so that 4 equal corners give exactly their value.
fn bilinear(c0: vec4<f32>, c1: vec4<f32>, c2: vec4<f32>, c3: vec4<f32>, st: vec2<f32>) -> vec4<f32> {
  let near = c0 + (c1 - c0) * st.x;
  let far = c2 + (c3 - c2) * st.x;
  return near + (far - near) * st.y;
}

// Brightness of a fractional light level (0..15): linear interpolation in View::light_curve,
// indexed in the uniform itself (a function-local copy of the array, dynamically indexed, may
// live in scratch memory). Every shader that includes this file binds the View uniform as
// `view`.
fn level_curve(l: f32) -> f32 {
  let x = clamp(l, 0.0, LIGHT_MAX_LEVEL);
  let i = min(u32(x), 14u);
  let j = i + 1u;
  return mix(view.light_curve[i >> 2u][i & 3u], view.light_curve[j >> 2u][j & 3u], x - f32(i));
}

// The light reaching a fragment: the 4 corner lights interpolated per channel at (s, t), each
// channel through the brightness curve, then the brightest of sky light (times the sky color),
// block light (times its gain) and the ambient floor.
fn voxel_light(i: VsOut, view: View) -> vec3<f32> {
  let l = bilinear(corner_light(i.light, 0u), corner_light(i.light, 1u), corner_light(i.light, 2u), corner_light(i.light, 3u), i.st);
  let sky = level_curve(l.x) * view.sky_light.rgb;
  let block = vec3(level_curve(l.y), level_curve(l.z), level_curve(l.w)) * view.light_params.y;
  return max(max(sky, block), vec3(view.sky_light.a));
}

// Ambient occlusion factor: the 4 corner AO values (0..3, 3 = open) interpolated at (s, t), from
// 1 - strength (fully occluded) to 1 (open).
fn ao_factor(i: VsOut, view: View) -> f32 {
  let a = vec4<f32>((vec4(i.ao) >> vec4(0u, 2u, 4u, 6u)) & vec4(3u));
  let near = a.x + (a.y - a.x) * i.st.x;
  let far = a.z + (a.w - a.z) * i.st.x;
  return mix(1.0 - view.light_params.x, 1.0, (near + (far - near) * i.st.y) / 3.0);
}

// albedo * face shade * AO * light, then fog. At sky 15 under a white noon sky with AO open
// this is albedo * face shade: the look before light. Emissive quads (lamps, lava) glow as in
// Minecraft: their albedo at full brightness, with no face shade, AO or light, then fog.
fn shade_fragment(albedo: vec4<f32>, i: VsOut, view: View) -> vec4<f32> {
  var lit = albedo.rgb;
  if (((i.ao >> 8u) & FLAG_EMISSIVE) == 0u) {
    lit = albedo.rgb * (face_shade(i.face) * ao_factor(i, view)) * voxel_light(i, view);
  }
  let fog = clamp((i.view_dist - view.fog_params.x) / max(view.fog_params.y - view.fog_params.x, 1.0), 0.0, 1.0);
  return vec4(mix(lit, view.fog_color.rgb, fog), 1.0);
}
