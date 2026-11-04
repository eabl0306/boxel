// Sun shadows (ADR 0065, lane S5, High only): the entity models' depth in a cascade's map.
// Appended after common.wgsl, sun_off.wgsl, shadow_common.wgsl and model.wgsl (its ModelOut and
// model_texel, the skin in group 0): the same vertices as vs_model, placed by the cascade's
// matrix (both are relative to the render origin).
//
// The pipelines cull the faces seen from the sun (cull_mode Front: the cascades' light space has
// the main camera's handedness), so a closed box casts with the faces that look away from the
// sun, as the terrain's buckets do, with the box's thickness as the margin behind each lit
// face. vs_model_shadow alone draws the opaque boxes; fs_model_shadow_cutout drops the texels
// the cutout boxes' alpha test drops (a hat's holes let the sun through).
@group(3) @binding(0) var<uniform> cascade: ShadowCascade;

@vertex
fn vs_model_shadow(
  @location(0) pos: vec3<f32>,
  @location(1) uv: vec2<f32>,
  @location(2) rect: vec4<u32>,
  @location(3) data: u32,
  @location(4) sky: u32,
) -> ModelOut {
  var o: ModelOut;
  o.clip = cascade.view_proj * vec4(pos, 1.0);
  o.uv = uv;
  o.rect = rect;
  o.data = data;
  o.sky = sky;
  o.view_dist = 0.0;
  return o;
}

@fragment
fn fs_model_shadow_cutout(i: ModelOut) {
  if (!alpha_test_passes(model_texel(i))) { discard; }
}
