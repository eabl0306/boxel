// Cubic fog and clouds (issue #47, ADR 0030), every tier. Standalone module (not prepended with
// common.wgsl). Rules of the other modules hold: no instance/draw builtins, vertex_index only with
// zero first vertex, i32 textureLoad levels, no function-local arrays.
//
// fs_fog_march: one ray per texel (per 2x2 pixels at half resolution) walks the cells of each
// layer (an integer-exact 3D DDA) up to the opaque depth of its pixels and writes, per texel, the
// fog color per unit opacity (x, 11-11-10 sqrt) and one opacity byte per pixel (y, sqrt). The
// algorithm is boxel_fog::march step for step; the noise is boxel_fog::noise bit for bit.
// fs_fog_composite_*: blends a pixel's byte over the main color (dst * T + C), discarding a 0
// byte so pixels without fog stay untouched.
// fs_fog_debug_coverage: the integer noise and coverage of layer 0 for a grid of cells (tests).
//
// Bindings: march = depth (group 0, binding 0) + Fog (group 1, binding 0); composite = the fog
// target (group 0, binding 1); debug = Fog. At most one uniform block and one texture each, no
// sampler (the Low tier on GL puts every block on binding 0 and every texture on unit 0, ADR 0011).

struct FogLayer {
  // x, y: band [min, max) relative to the render origin's y; z: reach (blocks along the ray);
  // w: mean extinction per block (log2 units) for the rest of a ray past the step budget.
  band: vec4<f32>,
  // x, y: fade start and end distance; z: extinction per block of a full cell (log2 units).
  fade: vec4<f32>,
  // xyz: min corner of the origin's cell relative to the render origin (blocks).
  grid: vec4<f32>,
  // xyz: cell size (blocks).
  size: vec4<f32>,
  // rgb: color of a lit cell.
  lit: vec4<f32>,
  // shades of the faces: top, bottom, x, z.
  face: vec4<f32>,
  // xyz: offset of the displaced origin cell inside its lattice step; w: log2 of the step.
  lattice: vec4<u32>,
  // xyz: low words of its lattice point; w: seed mix of the first octave.
  base_lo: vec4<u32>,
  // xyz: high words; w: seed of the first octave.
  base_hi: vec4<u32>,
  // x: seed mix of the second octave; y: its seed; z: octaves; w: flags (LAYER_*).
  octave2: vec4<u32>,
  // x: coverage threshold (0..4096); y: coverage gain (q8).
  shape: vec4<u32>,
  // xyz: the wind's fraction of a cell (1/4096).
  wind: vec4<i32>,
  // x, y: band cells [first, end) relative to the origin's cell.
  cells: vec4<i32>,
};

struct Fog {
  // xyz: ray direction at NDC (0, 0) with dot(dir, forward) = 1; w: near plane.
  ray0: vec4<f32>,
  // xyz: change of the direction per NDC unit along x; w: full width in pixels.
  ray_x: vec4<f32>,
  // xyz: same along y; w: full height in pixels.
  ray_y: vec4<f32>,
  // xyz: eye relative to the render origin.
  eye: vec4<f32>,
  // x: layers (sorted by height); y: flags (FLAG_*); z: step budget.
  counts: vec4<u32>,
  // rgb: sky color (the fade target).
  sky: vec4<f32>,
  // reserved for the ground map.
  ground: vec4<i32>,
  // xyz: first cell of the debug grid.
  debug: vec4<i32>,
  layers: array<FogLayer, 4>,
  ground_map: array<vec4<u32>, 512>,
};

@group(0) @binding(0) var depth_tex: texture_2d<f32>;
@group(0) @binding(1) var fog_tex: texture_2d<u32>;
@group(1) @binding(0) var<uniform> fog: Fog;

const FLAG_HALF: u32 = 1u;
const LAYER_VOLUME: u32 = 1u;
const EXIT_TAU: f32 = 8.66;
const MIN_DIR: f32 = 1e-30;
const FAR: f32 = 1e30;
const RECORDS: vec4<u32> = vec4<u32>(0u, 1u, 2u, 3u);

struct VsOut { @builtin(position) pos: vec4<f32> };

@vertex
fn vs_fullscreen(@builtin(vertex_index) vi: u32) -> VsOut {
  let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
  var o: VsOut;
  o.pos = vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
  return o;
}

// ---------------------------------------------------------------- integer noise (boxel_fog::noise)

fn lowbias32(v: u32) -> u32 {
  var x = v;
  x ^= x >> 16u;
  x *= 0x7feb352du;
  x ^= x >> 15u;
  x *= 0x846ca68bu;
  x ^= x >> 16u;
  return x;
}

fn seed_mix(seed: u32, hi: vec3<u32>) -> u32 {
  return lowbias32(lowbias32(lowbias32(seed ^ hi.x) ^ hi.y) ^ hi.z);
}

fn smooth_w(w: i32) -> i32 {
  return (((w * w) >> 12u) * (12288 - 2 * w)) >> 12u;
}

fn lerp_i(a: i32, b: i32, s: i32) -> i32 {
  return a + (((b - a) * s) >> 12u);
}

struct Words { lo: vec3<u32>, hi: vec3<u32> };

// (lo, hi) + li per axis, 64-bit two's complement.
// (Integer vectors are never `select`ed: naga writes mix(uvec3, uvec3, bvec3), which GLSL 330
// lacks and NVIDIA's GL compiler rejects. Bool vectors convert to 0/1 instead.)
fn add_words(lo: vec3<u32>, hi: vec3<u32>, li: vec3<i32>) -> Words {
  let n = lo + bitcast<vec3<u32>>(li);
  let carry = vec3<u32>(n < lo) * vec3<u32>(li >= vec3<i32>(0));
  let borrow = vec3<u32>(n > lo) * vec3<u32>(li < vec3<i32>(0));
  var w: Words;
  w.lo = n;
  w.hi = hi + carry - borrow;
  return w;
}

// The 12-bit value of a lattice point: its own seed mix only when a carry changed its high words.
fn corner_value(seed: u32, mix: u32, base_hi: vec3<u32>, lo: vec3<u32>, hi: vec3<u32>, volume: bool) -> i32 {
  var m = mix;
  if (any(hi != base_hi)) {
    m = seed_mix(seed, hi);
  }
  var h = lowbias32(m ^ lo.x);
  if (volume) {
    h = lowbias32(h ^ lo.y);
  }
  return i32(lowbias32(h ^ lo.z) >> 20u);
}

// Corner values of a lattice step. Volume: a = z0 plane (x0y0, x1y0, x0y1, x1y1), b = z1 plane.
// Flat: a = (x0z0, x1z0, x0z1, x1z1).
struct Corners { a: vec4<i32>, b: vec4<i32> };

fn corners(w0: Words, w1: Words, base_hi: vec3<u32>, seed: u32, mix: u32, volume: bool) -> Corners {
  var c: Corners;
  if (volume) {
    c.a = vec4<i32>(
      corner_value(seed, mix, base_hi, vec3(w0.lo.x, w0.lo.y, w0.lo.z), vec3(w0.hi.x, w0.hi.y, w0.hi.z), true),
      corner_value(seed, mix, base_hi, vec3(w1.lo.x, w0.lo.y, w0.lo.z), vec3(w1.hi.x, w0.hi.y, w0.hi.z), true),
      corner_value(seed, mix, base_hi, vec3(w0.lo.x, w1.lo.y, w0.lo.z), vec3(w0.hi.x, w1.hi.y, w0.hi.z), true),
      corner_value(seed, mix, base_hi, vec3(w1.lo.x, w1.lo.y, w0.lo.z), vec3(w1.hi.x, w1.hi.y, w0.hi.z), true));
    c.b = vec4<i32>(
      corner_value(seed, mix, base_hi, vec3(w0.lo.x, w0.lo.y, w1.lo.z), vec3(w0.hi.x, w0.hi.y, w1.hi.z), true),
      corner_value(seed, mix, base_hi, vec3(w1.lo.x, w0.lo.y, w1.lo.z), vec3(w1.hi.x, w0.hi.y, w1.hi.z), true),
      corner_value(seed, mix, base_hi, vec3(w0.lo.x, w1.lo.y, w1.lo.z), vec3(w0.hi.x, w1.hi.y, w1.hi.z), true),
      corner_value(seed, mix, base_hi, vec3(w1.lo.x, w1.lo.y, w1.lo.z), vec3(w1.hi.x, w1.hi.y, w1.hi.z), true));
  } else {
    c.a = vec4<i32>(
      corner_value(seed, mix, base_hi, vec3(w0.lo.x, w0.lo.y, w0.lo.z), vec3(w0.hi.x, w0.hi.y, w0.hi.z), false),
      corner_value(seed, mix, base_hi, vec3(w1.lo.x, w0.lo.y, w0.lo.z), vec3(w1.hi.x, w0.hi.y, w0.hi.z), false),
      corner_value(seed, mix, base_hi, vec3(w0.lo.x, w0.lo.y, w1.lo.z), vec3(w0.hi.x, w0.hi.y, w1.hi.z), false),
      corner_value(seed, mix, base_hi, vec3(w1.lo.x, w0.lo.y, w1.lo.z), vec3(w1.hi.x, w0.hi.y, w1.hi.z), false));
    c.b = c.a;
  }
  return c;
}

fn interp(c: Corners, w: vec3<i32>, volume: bool) -> i32 {
  let sx = smooth_w(w.x);
  let sz = smooth_w(w.z);
  if (volume) {
    let sy = smooth_w(w.y);
    let a00 = lerp_i(c.a.x, c.a.y, sx);
    let a10 = lerp_i(c.a.z, c.a.w, sx);
    let a01 = lerp_i(c.b.x, c.b.y, sx);
    let a11 = lerp_i(c.b.z, c.b.w, sx);
    return lerp_i(lerp_i(a00, a10, sy), lerp_i(a01, a11, sy), sz);
  }
  let a0 = lerp_i(c.a.x, c.a.y, sx);
  let a1 = lerp_i(c.a.z, c.a.w, sx);
  return lerp_i(a0, a1, sz);
}

// The corners of the last lattice step of each octave a ray looked up in one layer.
struct NoiseCache {
  key1: vec3<i32>,
  c1: Corners,
  key2: vec3<i32>,
  c2: Corners,
};

fn new_cache() -> NoiseCache {
  var c: NoiseCache;
  c.key1 = vec3<i32>(0x7fffffff);
  c.key2 = vec3<i32>(0x7fffffff);
  return c;
}

// The noise (0..4096) of the cell `d` cells from the origin's cell in layer `li`.
fn cell_noise(li: u32, d: vec3<i32>, cache: ptr<function, NoiseCache>) -> i32 {
  let lat = fog.layers[li].lattice;
  let l = lat.w;
  let volume = (fog.layers[li].octave2.w & LAYER_VOLUME) != 0u;
  var k = bitcast<vec3<i32>>(lat.xyz) + d;
  if (!volume) {
    k.y = 0;
  }
  let num = (k << vec3<u32>(12u)) - fog.layers[li].wind.xyz;
  let lattice1 = num >> vec3<u32>(12u + l);
  let lo = fog.layers[li].base_lo;
  let hi = fog.layers[li].base_hi;
  if (any(lattice1 != (*cache).key1)) {
    (*cache).key1 = lattice1;
    let w0 = add_words(lo.xyz, hi.xyz, lattice1);
    let w1 = add_words(lo.xyz, hi.xyz, lattice1 + vec3<i32>(1));
    (*cache).c1 = corners(w0, w1, hi.xyz, hi.w, lo.w, volume);
  }
  var n = interp((*cache).c1, (num >> vec3<u32>(l)) & vec3<i32>(4095), volume);
  if (fog.layers[li].octave2.z == 2u) {
    let lattice2 = num >> vec3<u32>(11u + l);
    if (any(lattice2 != (*cache).key2)) {
      (*cache).key2 = lattice2;
      let lo2 = lo.xyz << vec3<u32>(1u);
      let hi2 = (hi.xyz << vec3<u32>(1u)) | (lo.xyz >> vec3<u32>(31u));
      let w0 = add_words(lo2, hi2, lattice2);
      let w1 = add_words(lo2, hi2, lattice2 + vec3<i32>(1));
      let o2 = fog.layers[li].octave2;
      (*cache).c2 = corners(w0, w1, hi2, o2.y, o2.x, volume);
    }
    let v2 = interp((*cache).c2, (num >> vec3<u32>(l - 1u)) & vec3<i32>(4095), volume);
    n = ((2 * n + v2) * 21845) >> 16u;
  }
  return n;
}

// Coverage (0..=4096) from the noise.
fn coverage_of(li: u32, n: i32) -> u32 {
  let shape = fog.layers[li].shape;
  let d = u32(clamp(n + 1 - i32(shape.x), 0, 4095));
  return min((d * shape.y) >> 8u, 4096u);
}

// ---------------------------------------------------------------- march (boxel_fog::march)

struct March {
  // transmittance and color so far
  t: f32,
  c: vec3<f32>,
  // optical depth so far (log2)
  tau: f32,
  // record depths (sorted), their transmittance, the sum of their colors, the next one
  rec_t: vec4<f32>,
  rec_tr: vec4<f32>,
  csum: vec3<f32>,
  next: u32,
  steps: u32,
  done: bool,
};

// Records at or before `upto` take the current values.
fn take(st: ptr<function, March>, upto: f32) {
  loop {
    if ((*st).next >= 4u) { break; }
    if ((*st).rec_t[(*st).next] > upto) { break; }
    (*st).rec_tr = select((*st).rec_tr, vec4<f32>((*st).t), RECORDS == vec4<u32>((*st).next));
    (*st).csum += (*st).c;
    (*st).next += 1u;
  }
}

// [s0, s1] of extinction sigma_t (per unit t, log2) and color col.
fn integrate(st: ptr<function, March>, s0: f32, s1: f32, sigma_t: f32, col: vec3<f32>) {
  loop {
    if ((*st).next >= 4u) { break; }
    let rt = (*st).rec_t[(*st).next];
    if (rt > s1) { break; }
    let tr = (*st).t * exp2(-sigma_t * max(rt - s0, 0.0));
    (*st).rec_tr = select((*st).rec_tr, vec4<f32>(tr), RECORDS == vec4<u32>((*st).next));
    (*st).csum += (*st).c + ((*st).t - tr) * col;
    (*st).next += 1u;
  }
  let len = s1 - s0;
  let t_new = (*st).t * exp2(-sigma_t * len);
  (*st).c += ((*st).t - t_new) * col;
  (*st).t = t_new;
  (*st).tau += sigma_t * len;
  if ((*st).tau > EXIT_TAU) {
    (*st).done = true;
  }
}

// The ray's t interval in layer li's band, clipped to [0, t_end] and to the layer's reach.
fn chord(li: u32, eye: vec3<f32>, dir: vec3<f32>, dir_len: f32, t_end: f32) -> vec2<f32> {
  let band = fog.layers[li].band;
  var ta = 0.0;
  var tb = FAR;
  if (abs(dir.y) < MIN_DIR) {
    if (eye.y < band.x || eye.y >= band.y) {
      return vec2<f32>(1.0, 0.0);
    }
  } else {
    let inv = 1.0 / dir.y;
    let a = (band.x - eye.y) * inv;
    let b = (band.y - eye.y) * inv;
    ta = max(min(a, b), 0.0);
    tb = max(a, b);
  }
  return vec2<f32>(ta, min(min(tb, t_end), band.z / dir_len));
}

// The color of a segment: lit times the shade, faded to the sky by the middle's distance.
fn segment_color(li: u32, s0: f32, s1: f32, shade: f32, dir_len: f32) -> vec3<f32> {
  let fade = fog.layers[li].fade;
  let f = clamp((0.5 * (s0 + s1) * dir_len - fade.x) / max(fade.y - fade.x, 1.0), 0.0, 1.0);
  return mix(fog.layers[li].lit.rgb * shade, fog.sky.rgb, f);
}

// Shade of the face a ray enters a cell through, stepping along `axis` in direction `s`.
fn face_shade(li: u32, axis: u32, s: i32) -> f32 {
  let face = fog.layers[li].face;
  if (axis == 0u) { return face.z; }
  if (axis == 2u) { return face.w; }
  return select(face.x, face.y, s > 0);
}

fn view_t(p: vec2<i32>) -> f32 {
  let d = textureLoad(depth_tex, p, 0).x;
  return select(FAR, fog.ray0.w / d, d > 0.0);
}

fn color_code(v: f32, max: f32) -> u32 {
  return u32(floor(sqrt(clamp(v, 0.0, 1.0)) * max + 0.5));
}

@fragment
fn fs_fog_march(@builtin(position) frag: vec4<f32>) -> @location(0) vec2<u32> {
  let texel = vec2<i32>(frag.xy);
  let half = (fog.counts.y & FLAG_HALF) != 0u;
  let size = vec2<f32>(fog.ray_x.w, fog.ray_y.w);
  let center = select(vec2<f32>(texel) + 0.5, vec2<f32>(texel * 2 + 1), half);
  let ndc = vec2<f32>(center.x / size.x * 2.0 - 1.0, 1.0 - center.y / size.y * 2.0);
  let dir = fog.ray0.xyz + ndc.x * fog.ray_x.xyz + ndc.y * fog.ray_y.xyz;
  let eye = fog.eye.xyz;
  let dir_len = length(dir);
  let count = fog.counts.x;

  // No band in reach: no depth read, no fog.
  var reached = false;
  for (var i = 0u; i < count; i++) {
    let c = chord(i, eye, dir, dir_len, FAR);
    reached = reached || c.x < c.y;
  }
  if (!reached) {
    return vec2<u32>(0u);
  }

  // The view depth of each pixel, sorted (5 compare-swaps), with the pixel it came from.
  var t: vec4<f32>;
  if (half) {
    let last = vec2<i32>(size) - 1;
    let p = texel * 2;
    t = vec4<f32>(
      view_t(min(p, last)),
      view_t(min(p + vec2<i32>(1, 0), last)),
      view_t(min(p + vec2<i32>(0, 1), last)),
      view_t(min(p + vec2<i32>(1, 1), last)));
  } else {
    t = vec4<f32>(view_t(texel));
  }
  var k = RECORDS;
  if (t.y < t.x) { t = t.yxzw; k = k.yxzw; }
  if (t.w < t.z) { t = t.xywz; k = k.xywz; }
  if (t.z < t.x) { t = t.zyxw; k = k.zyxw; }
  if (t.w < t.y) { t = t.xwzy; k = k.xwzy; }
  if (t.z < t.y) { t = t.xzyw; k = k.xzyw; }

  var st: March;
  st.t = 1.0;
  st.c = vec3<f32>(0.0);
  st.tau = 0.0;
  st.rec_t = t;
  st.rec_tr = vec4<f32>(1.0);
  st.csum = vec3<f32>(0.0);
  st.next = 0u;
  st.steps = 0u;
  st.done = false;
  let max_steps = fog.counts.z;
  let up = dir.y > 0.0;
  let safe = select(select(vec3<f32>(MIN_DIR), vec3<f32>(-MIN_DIR), dir < vec3<f32>(0.0)), dir,
                    abs(dir) >= vec3<f32>(MIN_DIR));
  let inv = 1.0 / safe;
  let edge = vec3<i32>(safe > vec3<f32>(0.0));
  let stepv = edge * 2 - 1;

  for (var n = 0u; n < count; n++) {
    if (st.done) { break; }
    let li = select(count - 1u - n, n, up);
    let ch = chord(li, eye, dir, dir_len, t.w);
    let ta = ch.x;
    let tb = ch.y;
    if (ta >= tb) { continue; }
    take(&st, ta);
    let sigma_mean = fog.layers[li].band.w * dir_len;
    if (st.steps >= max_steps) {
      integrate(&st, ta, tb, sigma_mean, segment_color(li, ta, tb, 1.0, dir_len));
      continue;
    }
    // The walk (boxel_fog::march::Dda).
    let grid = fog.layers[li].grid.xyz;
    let size3 = fog.layers[li].size.xyz;
    let band_cells = fog.layers[li].cells.xy;
    let p0 = eye + ta * dir;
    var cell = vec3<i32>(floor((p0 - grid) / size3));
    cell.y = clamp(cell.y, band_cells.x, band_cells.y - 1);
    var t_next = (grid + vec3<f32>(cell + edge) * size3 - eye) * inv;
    let density = fog.layers[li].fade.z;
    var cache = new_cache();
    var s0 = ta;
    var shade = 1.0;
    var entry = 1.0;
    if (ta > 0.0) {
      entry = face_shade(li, 1u, select(-1, 1, up));
    }
    var prev_full = false;
    loop {
      if (st.steps >= max_steps) {
        integrate(&st, s0, tb, sigma_mean, segment_color(li, s0, tb, 1.0, dir_len));
        break;
      }
      st.steps += 1u;
      var axis = 2u;
      var t_exit = t_next.z;
      if (t_next.x <= t_next.y && t_next.x <= t_next.z) {
        axis = 0u;
        t_exit = t_next.x;
      } else if (t_next.y <= t_next.z) {
        axis = 1u;
        t_exit = t_next.y;
      }
      let s1 = clamp(t_exit, s0, tb);
      let cov = coverage_of(li, cell_noise(li, cell, &cache));
      let sigma = density * (f32(cov) / 4096.0);
      if (sigma > 0.0) {
        if (!prev_full) {
          shade = entry;
        }
        integrate(&st, s0, s1, sigma * dir_len, segment_color(li, s0, s1, shade, dir_len));
        if (st.done) { break; }
      }
      prev_full = sigma > 0.0;
      if (t_exit >= tb) { break; }
      let along = vec3<bool>(axis == 0u, axis == 1u, axis == 2u);
      cell += stepv * vec3<i32>(along);
      t_next = select(t_next, (grid + vec3<f32>(cell + edge) * size3 - eye) * inv, along);
      entry = face_shade(li, axis, stepv[axis]);
      s0 = s1;
      if (cell.y < band_cells.x || cell.y >= band_cells.y) { break; }
    }
  }
  take(&st, 3.0e38);

  let alpha = clamp(vec4<f32>(1.0) - st.rec_tr, vec4<f32>(0.0), vec4<f32>(1.0));
  let sum = alpha.x + alpha.y + alpha.z + alpha.w;
  if (sum <= 0.0) {
    return vec2<u32>(0u);
  }
  let color = st.csum / sum;
  let codes = vec4<u32>(floor(sqrt(alpha) * 255.0 + 0.5));
  let bytes = (codes.x << (8u * k.x)) | (codes.y << (8u * k.y)) | (codes.z << (8u * k.z))
    | (codes.w << (8u * k.w));
  let rgb = color_code(color.r, 2047.0) | (color_code(color.g, 2047.0) << 11u)
    | (color_code(color.b, 1023.0) << 22u);
  return vec2<u32>(rgb, bytes);
}

// ---------------------------------------------------------------- composite

fn decode_color(word: u32) -> vec3<f32> {
  let s = vec3<f32>(f32(word & 0x7ffu) / 2047.0, f32((word >> 11u) & 0x7ffu) / 2047.0,
                    f32(word >> 22u) / 1023.0);
  return s * s;
}

// The fog over one pixel: color and opacity of byte k (blend SrcAlpha, OneMinusSrcAlpha).
fn composite(texel: vec2<u32>, byte: u32) -> vec4<f32> {
  let s = f32(byte) / 255.0;
  return vec4<f32>(decode_color(texel.x), s * s);
}

@fragment
fn fs_fog_composite_half(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
  let p = vec2<u32>(frag.xy);
  let texel = textureLoad(fog_tex, vec2<i32>(p >> vec2<u32>(1u)), 0).xy;
  let k = (p.x & 1u) | ((p.y & 1u) << 1u);
  let byte = (texel.y >> (8u * k)) & 0xffu;
  if (byte == 0u) {
    discard;
  }
  return composite(texel, byte);
}

@fragment
fn fs_fog_composite_full(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
  let texel = textureLoad(fog_tex, vec2<i32>(frag.xy), 0).xy;
  let byte = texel.y & 0xffu;
  if (byte == 0u) {
    discard;
  }
  return composite(texel, byte);
}

// ---------------------------------------------------------------- tests

// x: coverage, y: noise of layer 0 at cell debug.xyz + (pixel x, 0, pixel y).
@fragment
fn fs_fog_debug_coverage(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<u32> {
  let d = fog.debug.xyz + vec3<i32>(i32(frag.x), 0, i32(frag.y));
  var cache = new_cache();
  let n = cell_noise(0u, d, &cache);
  return vec4<u32>(coverage_of(0u, n), u32(n), 0u, 1u);
}
