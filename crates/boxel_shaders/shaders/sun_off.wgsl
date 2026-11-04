// boxel: the sun factor without sun shadows (ADR 0065). Appended after common.wgsl to every module
// that includes it: Mid and Low always, High while its shadowed modules do not exist or shadows
// are off. High's sun_shadow.wgsl (lanes S4 and S5) defines the same functions from its shadow
// maps.
//
// sun_factor(i, sky_level) scales the sky term of voxel_light, and sun_factor_clip(clip,
// view_dist, realm, sky_level) that of an entity model (model.wgsl, lane S5): 1 - k * (1 - v)
// for a strength k and a sun visibility v, so it lies in [1 - k, 1]. Here both are exactly 1.0,
// and x * 1.0 == x in IEEE 754, so these modules give the floats they gave before the hook. No
// binding, View field or vertex attribute: Low's one uniform block and every bind group layout
// stay as they are.
fn sun_factor(i: VsOut, sky_level: f32) -> f32 {
  return 1.0;
}

fn sun_factor_clip(clip: vec4<f32>, view_dist: f32, realm: u32, sky_level: f32) -> f32 {
  return 1.0;
}
