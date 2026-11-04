// boxel: the sun factor from the sun shadow maps (ADR 0065, lane S4; High only, while shadows are
// on). Appended after common.wgsl and shadow_common.wgsl in place of sun_off.wgsl, before
// voxel_pull.wgsl (whose `view` it reads): Mid, Low and High without shadows never see it.
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

fn sun_factor(i: VsOut, sky_level: f32) -> f32 {
  let frame = shadow[0];
  let k = frame.receive.x;
  let end = frame.light.w;
  if (k <= 0.0 || sky_level <= 0.0 || i.view_dist >= end || out_realm(i) != u32(frame.band.z)) {
    return 1.0;
  }
  let n = face_normal(out_face(i));
  var v = smoothstep(0.0, SUN_RAMP, dot(n, frame.light.xyz));
  if (v > 0.0) {
    let pos = fragment_position(i.clip);
    let d = length(pos - view.camera_rel.xyz);
    let count = min(frame.counts.x, MAX_SHADOW_CASCADES);
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
    v = v * m;
  }
  let fade = end * (1.0 - SUN_BLEND);
  v = mix(v, 1.0, clamp((i.view_dist - fade) / (end - fade), 0.0, 1.0));
  return 1.0 - k * (1.0 - v);
}
