// The game UI (boxel_ui's paint list, ADR 0033) drawn over the frame, on every tier (GL 3.3
// included). Standalone module: one texture read with textureLoad (no sampler, no uniform, no
// immediates), one instance per quad, a 4-vertex triangle strip.
//
// Every value a fragment uses is flat and every step is integer arithmetic, so High, Mid and Low
// pick the same texel for the same pixel (the CPU reference is boxel_ui::raster):
//   texel = src + ((p - corner) * src_size + rem) / size   per axis
// where `corner` is the drawn rectangle's top-left pixel and `rem` the remainder of the mapping
// at that corner (computed on the CPU), which equals boxel_ui's
//   texel = src0 + (p - origin) * src_size / size
// for a mapping starting at `origin`, wherever that origin is.

@group(0) @binding(0) var ui_atlas: texture_2d<f32>;

struct UiOut {
  @builtin(position) pos: vec4<f32>,
  // Linear RGBA with straight alpha, from the quad's sRGB tint (exact table on the CPU).
  @location(0) @interpolate(flat) tint: vec4<f32>,
  // The mapping's size in pixels (x, y), then its remainder at the drawn corner (x, y).
  @location(1) @interpolate(flat) scale: vec4<u32>,
  // Source texel at the drawn corner | source size << 16 (x, then y); the drawn corner's
  // pixel, x | y << 16; unused.
  @location(2) @interpolate(flat) src: vec4<u32>,
};

// `rect` is the drawn rectangle (min.x, min.y, max.x, max.y) in clip space.
@vertex
fn vs_ui(
  @builtin(vertex_index) vi: u32,
  @location(0) rect: vec4<f32>,
  @location(1) tint: vec4<f32>,
  @location(2) scale: vec4<u32>,
  @location(3) src: vec4<u32>,
) -> UiOut {
  let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
  var o: UiOut;
  o.pos = vec4(mix(rect.xy, rect.zw, corner), 0.0, 1.0);
  o.tint = tint;
  o.scale = scale;
  o.src = src;
  return o;
}

@fragment
fn fs_ui(i: UiOut) -> @location(0) vec4<f32> {
  // The fragment position is the pixel's center: truncation gives the pixel exactly.
  let p = vec2<u32>(i.pos.xy);
  let corner = vec2<u32>(i.src.z & 0xffffu, i.src.z >> 16u);
  let s0 = vec2<u32>(i.src.x & 0xffffu, i.src.y & 0xffffu);
  let sn = vec2<u32>(i.src.x >> 16u, i.src.y >> 16u);
  let texel = s0 + ((p - corner) * sn + i.scale.zw) / i.scale.xy;
  return textureLoad(ui_atlas, vec2<i32>(texel), 0i) * i.tint;
}
