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
  // xyz: low words of its lattice point.
  base_lo: vec4<u32>,
  // xyz: high words; w: seed of the first octave.
  base_hi: vec4<u32>,
  // y: seed of the second octave; z: octaves; w: flags (LAYER_*).
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
const LAYER_UNIFORM: u32 = 2u;
const UNIFORM_PIECES: u32 = 8u;
const EXIT_TAU: f32 = 8.66;
const MIN_DIR: f32 = 1e-30;
const HI_MIX: u32 = 0x9e3779b1u;
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

// Corner values of a lattice step, z0 plane in a (x0y0, x1y0, x0y1, x1y1), z1 plane in b
// (boxel_fog::noise::corner_values): each axis' words folded into one (lo ^ hi * HI_MIX), then a
// hash chain from the octave's seed whose first links the corners share (14 hashes). Flat noise
// has the same y words at both y corners.
struct Corners { a: vec4<i32>, b: vec4<i32> };

fn corners(w0: Words, w1: Words, seed: u32) -> Corners {
  let v0 = w0.lo ^ (w0.hi * HI_MIX);
  let v1 = w1.lo ^ (w1.hi * HI_MIX);
  let hx0 = lowbias32(seed ^ v0.x);
  let hx1 = lowbias32(seed ^ v1.x);
  let h00 = lowbias32(hx0 ^ v0.y);
  let h10 = lowbias32(hx1 ^ v0.y);
  let h01 = lowbias32(hx0 ^ v1.y);
  let h11 = lowbias32(hx1 ^ v1.y);
  var c: Corners;
  c.a = vec4<i32>(
    i32(lowbias32(h00 ^ v0.z) >> 20u), i32(lowbias32(h10 ^ v0.z) >> 20u),
    i32(lowbias32(h01 ^ v0.z) >> 20u), i32(lowbias32(h11 ^ v0.z) >> 20u));
  c.b = vec4<i32>(
    i32(lowbias32(h00 ^ v1.z) >> 20u), i32(lowbias32(h10 ^ v1.z) >> 20u),
    i32(lowbias32(h01 ^ v1.z) >> 20u), i32(lowbias32(h11 ^ v1.z) >> 20u));
  return c;
}

// Trilinear (flat noise: the y weight is 0, so the y lerps give their first corner).
fn interp(c: Corners, w: vec3<i32>) -> i32 {
  let sx = smooth_w(w.x);
  let sy = smooth_w(w.y);
  let sz = smooth_w(w.z);
  let a00 = lerp_i(c.a.x, c.a.y, sx);
  let a10 = lerp_i(c.a.z, c.a.w, sx);
  let a01 = lerp_i(c.b.x, c.b.y, sx);
  let a11 = lerp_i(c.b.z, c.b.w, sx);
  return lerp_i(lerp_i(a00, a10, sy), lerp_i(a01, a11, sy), sz);
}

// The corners of the last lattice step of each octave a ray looked up in one layer, and the
// first octave's step bound (boxel_fog::noise::step_bound): its cells [lo1, hi1) and the most
// noise any of them can have.
struct NoiseCache {
  key1: vec3<i32>,
  c1: Corners,
  lo1: vec3<i32>,
  hi1: vec3<i32>,
  max1: i32,
  key2: vec3<i32>,
  c2: Corners,
};

fn new_cache() -> NoiseCache {
  var c: NoiseCache;
  c.key1 = vec3<i32>(0x7fffffff);
  c.key2 = vec3<i32>(0x7fffffff);
  return c;
}

// The cell `d` cells from the origin's cell in layer `li`: brings the first octave's corners and
// step bound up to date and returns the cell's position in the lattice (1/4096 cells).
fn lattice_cell(li: u32, d: vec3<i32>, cache: ptr<function, NoiseCache>) -> vec3<i32> {
  let lat = fog.layers[li].lattice;
  let l = lat.w;
  let volume = (fog.layers[li].octave2.w & LAYER_VOLUME) != 0u;
  var k = bitcast<vec3<i32>>(lat.xyz) + d;
  if (!volume) {
    k.y = 0;
  }
  let q = fog.layers[li].wind.xyz;
  let num = (k << vec3<u32>(12u)) - q;
  let lattice1 = num >> vec3<u32>(12u + l);
  if (any(lattice1 != (*cache).key1)) {
    (*cache).key1 = lattice1;
    let lo = fog.layers[li].base_lo;
    let hi = fog.layers[li].base_hi;
    // Flat noise: both y corners at lattice y 0.
    let up1 = vec3<i32>(1, i32(volume), 1);
    let w0 = add_words(lo.xyz, hi.xyz, lattice1);
    let w1 = add_words(lo.xyz, hi.xyz, lattice1 + up1);
    let c = corners(w0, w1, hi.w);
    (*cache).c1 = c;
    // Cells k of the step: from L * lattice (one more with a wind fraction), L of them.
    (*cache).lo1 = (lattice1 << vec3<u32>(l)) + vec3<i32>(q > vec3<i32>(0)) - bitcast<vec3<i32>>(lat.xyz);
    (*cache).hi1 = (*cache).lo1 + vec3<i32>(1 << l);
    let m = max(max(max(c.a.x, c.a.y), max(c.a.z, c.a.w)), max(max(c.b.x, c.b.y), max(c.b.z, c.b.w)));
    if (!volume) {
      // Flat noise: the step has every height. Wide but far from overflowing (the skip adds and
      // multiplies these; a ray's cells stay within 4097 of the origin's).
      (*cache).lo1.y = -(1 << 29);
      (*cache).hi1.y = 1 << 29;
    }
    (*cache).max1 = m;
  }
  return num;
}

// The coverage (0..=4096) of the cell at `num` (lattice_cell) in layer `li`: its noise's, but the
// second octave is only computed when the first one alone cannot decide (coverage grows with
// the noise, and the second octave moves it by at most 4095 / 3).
fn cell_coverage(li: u32, num: vec3<i32>, cache: ptr<function, NoiseCache>) -> u32 {
  let l = fog.layers[li].lattice.w;
  let volume = (fog.layers[li].octave2.w & LAYER_VOLUME) != 0u;
  let v1 = interp((*cache).c1, (num >> vec3<u32>(l)) & vec3<i32>(4095));
  if (fog.layers[li].octave2.z != 2u) {
    return coverage_of(li, v1);
  }
  let low = coverage_of(li, (2 * v1 * 21845) >> 16u);
  let high = coverage_of(li, ((2 * v1 + 4095) * 21845) >> 16u);
  if (low == high) {
    return low;
  }
  return coverage_of(li, cell_noise(li, num, cache));
}

// The noise (0..4096) of the cell at `num` (lattice_cell) in layer `li`.
fn cell_noise(li: u32, num: vec3<i32>, cache: ptr<function, NoiseCache>) -> i32 {
  let l = fog.layers[li].lattice.w;
  let volume = (fog.layers[li].octave2.w & LAYER_VOLUME) != 0u;
  var n = interp((*cache).c1, (num >> vec3<u32>(l)) & vec3<i32>(4095));
  if (fog.layers[li].octave2.z == 2u) {
    let lattice2 = num >> vec3<u32>(11u + l);
    if (any(lattice2 != (*cache).key2)) {
      (*cache).key2 = lattice2;
      let lo = fog.layers[li].base_lo;
      let hi = fog.layers[li].base_hi;
      let lo2 = lo.xyz << vec3<u32>(1u);
      let hi2 = (hi.xyz << vec3<u32>(1u)) | (lo.xyz >> vec3<u32>(31u));
      let up2 = vec3<i32>(1, i32(volume), 1);
      let w0 = add_words(lo2, hi2, lattice2);
      let w1 = add_words(lo2, hi2, lattice2 + up2);
      (*cache).c2 = corners(w0, w1, fog.layers[li].octave2.y);
    }
    let v2 = interp((*cache).c2, (num >> vec3<u32>(l - 1u)) & vec3<i32>(4095));
    n = ((2 * n + v2) * 21845) >> 16u;
  }
  return n;
}

// The most noise a cell of the cached first-octave step can have (step_bound).
fn step_max_noise(li: u32, cache: ptr<function, NoiseCache>) -> i32 {
  if (fog.layers[li].octave2.z == 2u) {
    return ((2 * (*cache).max1 + 4095) * 21845) >> 16u;
  }
  return (*cache).max1;
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

// One layer of a ray (boxel_fog::march's layer loop): its chord, the walk of its cells and their
// fog. Every segment goes through one `integrate`: a cell, or a piece of the rest of the chord
// (a layer whose every cell is full, or past the step budget), which keeps the code small.
fn march_layer(li: u32, eye: vec3<f32>, dir: vec3<f32>, dir_len: f32, up: bool, t_end: f32,
               max_steps: u32, st: ptr<function, March>) {
  let ch = chord(li, eye, dir, dir_len, t_end);
  let ta = ch.x;
  let tb = ch.y;
  if (ta >= tb) { return; }
  take(st, ta);
  let safe = select(select(vec3<f32>(MIN_DIR), vec3<f32>(-MIN_DIR), dir < vec3<f32>(0.0)), dir,
                    abs(dir) >= vec3<f32>(MIN_DIR));
  let inv = 1.0 / safe;
  let edge = vec3<i32>(safe > vec3<f32>(0.0));
  let stepv = edge * 2 - 1;
  // The walk (boxel_fog::march::Dda).
  let grid = fog.layers[li].grid.xyz;
  let size3 = fog.layers[li].size.xyz;
  let band_cells = fog.layers[li].cells.xy;
  let density = fog.layers[li].fade.z;
  let flags = fog.layers[li].octave2.w;
  let volume_layer = (flags & LAYER_VOLUME) != 0u;
  var cell = vec3<i32>(floor((eye + ta * dir - grid) / size3));
  cell.y = clamp(cell.y, band_cells.x, band_cells.y - 1);
  var t_next = (grid + vec3<f32>(cell + edge) * size3 - eye) * inv;
  var cache = new_cache();
  var s0 = ta;
  var shade = 1.0;
  var entry = 1.0;
  if (ta > 0.0) {
    entry = face_shade(li, 1u, select(-1, 1, up));
  }
  var prev_full = false;
  // The rest of the chord from `rest_from`, in UNIFORM_PIECES pieces (each faded by its own
  // middle) of one extinction and shade.
  var rest = false;
  var rest_sigma = 0.0;
  var rest_shade = 1.0;
  var rest_from = ta;
  var k = 0u;
  if ((*st).steps >= max_steps) {
    rest = true;
    rest_sigma = fog.layers[li].band.w;
  } else if ((flags & LAYER_UNIFORM) != 0u) {
    // Every cell full: one body, no inner faces, one step.
    (*st).steps += 1u;
    rest = true;
    rest_sigma = density;
    rest_shade = entry;
  }
  var piece = (tb - ta) / f32(UNIFORM_PIECES);
  loop {
    if (!rest && (*st).steps >= max_steps) {
      rest = true;
      rest_sigma = fog.layers[li].band.w;
      rest_shade = 1.0;
      rest_from = s0;
      piece = (tb - s0) / f32(UNIFORM_PIECES);
    }
    var a = s0;
    var b = s0;
    var s1 = s0;
    var sigma = 0.0;
    var last = false;
    var axis = 2u;
    if (rest) {
      a = rest_from + f32(k) * piece;
      k += 1u;
      last = k == UNIFORM_PIECES;
      b = select(a + piece, tb, last);
      sigma = rest_sigma;
      shade = rest_shade;
    } else {
      (*st).steps += 1u;
      let num = lattice_cell(li, cell, &cache);
      if (coverage_of(li, step_max_noise(li, &cache)) == 0u) {
        // The whole lattice step is empty: cross it in one step (Dda::step_exit, Dda::jump).
        let box_edge = cache.lo1 + (cache.hi1 - cache.lo1) * edge;
        let tb3 = (grid + vec3<f32>(box_edge) * size3 - eye) * inv;
        var jump_axis = 0u;
        var t_box = tb3.x;
        if (volume_layer && tb3.y < t_box) {
          jump_axis = 1u;
          t_box = tb3.y;
        }
        if (tb3.z < t_box) {
          jump_axis = 2u;
          t_box = tb3.z;
        }
        if (t_box >= tb) { break; }
        let sj = max(t_box, s0);
        let along = vec3<i32>(vec3<bool>(jump_axis == 0u, jump_axis == 1u, jump_axis == 2u));
        let past = cache.hi1 * edge + (cache.lo1 - 1) * (1 - edge);
        let inside = clamp(vec3<i32>(floor((eye + sj * dir - grid) / size3)), cache.lo1, cache.hi1 - 1);
        cell = past * along + inside * (1 - along);
        if (jump_axis != 1u) {
          cell.y = clamp(cell.y, band_cells.x, band_cells.y - 1);
        }
        t_next = (grid + vec3<f32>(cell + edge) * size3 - eye) * inv;
        entry = face_shade(li, jump_axis, stepv[jump_axis]);
        prev_full = false;
        s0 = sj;
        if (cell.y < band_cells.x || cell.y >= band_cells.y) { break; }
        continue;
      }
      var t_exit = t_next.z;
      if (t_next.x <= t_next.y && t_next.x <= t_next.z) {
        axis = 0u;
        t_exit = t_next.x;
      } else if (t_next.y <= t_next.z) {
        axis = 1u;
        t_exit = t_next.y;
      }
      s1 = clamp(t_exit, s0, tb);
      b = s1;
      sigma = density * (f32(cell_coverage(li, num, &cache)) / 4096.0);
      if (sigma > 0.0 && !prev_full) {
        shade = entry;
      }
      prev_full = sigma > 0.0;
      last = t_exit >= tb;
    }
    if (sigma > 0.0) {
      integrate(st, a, b, sigma * dir_len, segment_color(li, a, b, shade, dir_len));
      if ((*st).done) { break; }
    }
    if (last) { break; }
    if (!rest) {
      let along = vec3<bool>(axis == 0u, axis == 1u, axis == 2u);
      cell += stepv * vec3<i32>(along);
      t_next = select(t_next, (grid + vec3<f32>(cell + edge) * size3 - eye) * inv, along);
      entry = face_shade(li, axis, stepv[axis]);
      s0 = s1;
      if (cell.y < band_cells.x || cell.y >= band_cells.y) { break; }
    }
  }
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
  // Front to back: the bands are disjoint, so upwards for a rising ray, downwards otherwise.
  for (var n = 0u; n < count; n++) {
    if (!st.done) {
      march_layer(select(count - 1u - n, n, up), eye, dir, dir_len, up, t.w, max_steps, &st);
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
  let n = cell_noise(0u, lattice_cell(0u, d, &cache), &cache);
  return vec4<u32>(coverage_of(0u, n), u32(n), 0u, 1u);
}
