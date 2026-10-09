// The outline of the targeted block (ADR 0026), on all tiers (GL 3.3 included). Standalone
// module: no bindings. The CPU builds the line quads in clip space, the same for every tier.

struct OutlineOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };

// `clip` is the vertex in clip space; `color` is linear RGBA.
@vertex
fn vs_outline(@location(0) clip: vec4<f32>, @location(1) color: vec4<f32>) -> OutlineOut {
  var o: OutlineOut;
  o.pos = clip;
  o.color = color;
  return o;
}

@fragment
fn fs_outline(i: OutlineOut) -> @location(0) vec4<f32> {
  return i.color;
}
