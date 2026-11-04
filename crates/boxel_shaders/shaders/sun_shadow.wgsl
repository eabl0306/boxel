// boxel: the sun factor from the sun shadow maps (ADR 0065, lanes S4 and S5; High only, while
// shadows are on). Appended after common.wgsl and shadow_common.wgsl in place of sun_off.wgsl,
// before voxel_pull.wgsl, model.wgsl or cell.wgsl (whose `view` it reads): Mid, Low and High
// without shadows never see it.
//
// sun_factor(i, sky_level) = 1 - k * (1 - v) scales the sky term of voxel_light, k the camera
// realm's strength, v the sun's visibility at the fragment (0 to 1):
//
// - 1 at once (today's light) where nothing can change: k = 0, no sky light (sky_level is the
//   raw level 0..15, so caves stay unlit), a fragment of another realm (no realm shadows
//   another), or past where the shadows end (shadow.light.w, the interpolated view distance).
// - A face that looks away from the sun is in its own shadow: v rises from 0 with the face
//   edge-on to 1 at N . L = SUN_RAMP (about 6 degrees), a soft step as the sun crosses a wall.
//   Crosses and planes (plants) take +Y, as they are shaded.
// - Else the fragment's position (fragment_position: no varying), moved SUN_NORMAL_OFFSET
//   texels of the cascade out along its normal, compared with the cascade's map (one hardware
//   2x2 PCF compare: lit where its depth is at most the map's). The cascade is the first whose
//   view distances reach the fragment's exact distance; over the last SUN_BLEND of it the next
//   one blends in. A point outside its cascade's box knows no caster: lit (today's look).
// - v fades to 1 over the last SUN_BLEND of the shadow distance.
//
// sun_factor_clip (lane S5) is the same factor for a receiver without a face, an entity model:
// its normal comes from the screen derivatives of its position.
//
// So the factor is in [1 - k, 1]: shadows only darken, and every failure gives today's image.
@group(3) @binding(0) var sun_maps: texture_depth_2d_array;
@group(3) @binding(1) var sun_compare: sampler_comparison;
@group(3) @binding(2) var<uniform> shadow: array<ShadowCascade, 4>;

// N . L from which a face is fully in the sun (owner decision 3 of ADR 0065).
const SUN_RAMP: f32 = 0.1;
// Texels of its cascade a receiver moves out along its normal before the compare.
const SUN_NORMAL_OFFSET: f32 = 1.5;
// The blend band between cascades and the fade at the end (boxel_render::shadow::CASCADE_BLEND).
const SUN_BLEND: f32 = 0.1;

// The sun's visibility (0 to 1) at `pos` (camera-relative, normal `n`) in cascade `c`'s map.
fn sun_visibility(c: u32, pos: vec3<f32>, n: vec3<f32>) -> f32 {
  let e = shadow[c];
  let p = (e.view_proj * vec4(pos + n * (SUN_NORMAL_OFFSET * e.params.x), 1.0)).xyz;
  if (any(abs(p.xy) >= vec2(1.0)) || p.z < 0.0 || p.z > 1.0) {
    return 1.0;
  }
  let uv = vec2(0.5 * p.x + 0.5, 0.5 - 0.5 * p.y);
  // No derivatives: valid in the non-uniform branches above and below.
  return textureSampleCompareLevel(sun_maps, sun_compare, uv, c, p.z);
}

// True where the factor is 1 whatever the maps hold: k = 0, no sky light, another realm than
// the camera's, or past where the shadows end.
fn sun_skipped(sky_level: f32, view_dist: f32, realm: u32) -> bool {
  let frame = shadow[0];
  return frame.receive.x <= 0.0 || sky_level <= 0.0 || view_dist >= frame.light.w
    || realm != u32(frame.band.z);
}

// The step in N . L: 0 for a face that looks away from the sun, 1 from SUN_RAMP on.
fn sun_ramp(n: vec3<f32>) -> f32 {
  return smoothstep(0.0, SUN_RAMP, dot(n, shadow[0].light.xyz));
}

// The sun's visibility at `pos` (camera-relative, normal `n`) in the cascade of its distance,
// blended with the next one over the last SUN_BLEND of it.
fn sun_cascades(pos: vec3<f32>, n: vec3<f32>) -> f32 {
  let d = length(pos - view.camera_rel.xyz);
  let count = min(shadow[0].counts.x, MAX_SHADOW_CASCADES);
  var c = 0u;
  while (c + 1u < count && d >= shadow[c].params.w) {
    c++;
  }
  var m = sun_visibility(c, pos, n);
  let band = shadow[c].params.w * (1.0 - SUN_BLEND);
  if (c + 1u < count && d > band) {
    let t = (d - band) / (shadow[c].params.w - band);
    m = mix(m, sun_visibility(c + 1u, pos, n), t);
  }
  return m;
}

// 1 - k * (1 - v) for visibility `v`, which fades to 1 over the last SUN_BLEND of the shadow
// distance.
fn sun_strength(v: f32, view_dist: f32) -> f32 {
  let end = shadow[0].light.w;
  let fade = end * (1.0 - SUN_BLEND);
  let w = mix(v, 1.0, clamp((view_dist - fade) / (end - fade), 0.0, 1.0));
  return 1.0 - shadow[0].receive.x * (1.0 - w);
}

fn sun_factor(i: VsOut, sky_level: f32) -> f32 {
  if (sun_skipped(sky_level, i.view_dist, out_realm(i))) {
    return 1.0;
  }
  let n = face_normal(out_face(i));
  var v = sun_ramp(n);
  if (v > 0.0) {
    v = v * sun_cascades(fragment_position(i.clip), n);
  }
  return sun_strength(v, i.view_dist);
}

// The factor of the fragment at `clip` (its @builtin(position)), `view_dist` from the eye, of
// realm `realm` and sky level `sky_level`, for a receiver that carries no face (an entity
// model's posed box): its normal is the plane of its position's screen derivatives, turned
// toward the eye (the side the fragment shows). Derivatives: call it in uniform control flow,
// before any discard.
fn sun_factor_clip(clip: vec4<f32>, view_dist: f32, realm: u32, sky_level: f32) -> f32 {
  let pos = fragment_position(clip);
  let e = cross(dpdx(pos), dpdy(pos));
  if (sun_skipped(sky_level, view_dist, realm)) {
    return 1.0;
  }
  // A plane seen edge-on has no normal: n = 0, in its own shadow (it covers no pixel anyway).
  var n = e * inverseSqrt(max(dot(e, e), 1e-30));
  if (dot(n, view.camera_rel.xyz - pos) < 0.0) {
    n = -n;
  }
  var v = sun_ramp(n);
  if (v > 0.0) {
    v = v * sun_cascades(pos, n);
  }
  return sun_strength(v, view_dist);
}
