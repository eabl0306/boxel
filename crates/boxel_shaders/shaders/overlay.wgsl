// Screen-space overlay rectangles drawn on top of the frame (all tiers, GL 3.3 included).
// Standalone module: no bindings, one instance per rectangle, 4-vertex triangle strip.

struct OverlayOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };

// `rect` is (min.x, min.y, max.x, max.y) in clip space; `color` is linear RGBA.
@vertex
fn vs_overlay(
  @builtin(vertex_index) vi: u32,
  @location(0) rect: vec4<f32>,
  @location(1) color: vec4<f32>,
) -> OverlayOut {
  let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
  var o: OverlayOut;
  o.pos = vec4(mix(rect.xy, rect.zw, corner), 0.0, 1.0);
  o.color = color;
  return o;
}

@fragment
fn fs_overlay(i: OverlayOut) -> @location(0) vec4<f32> {
  return i.color;
}
