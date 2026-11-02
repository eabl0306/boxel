// Entity models (ADR 0040), on every tier (GL 3.3 included). Prepended with common.wgsl (the View
// struct and the light curve). The CPU poses the boxes of every instance into plain vertices
// relative to the render origin, so all three tiers run this same pipeline on the same data.
//
// Exactly ONE uniform block (the View) and ONE texture (the instance's skin, read with
// textureLoad: no sampler) per pipeline, no storage buffer, no immediates (ADR 0011). Per vertex:
//   pos  = position relative to the render origin (blocks)
//   uv   = texel coordinates on the skin (perspective-interpolated)
//   rect = the face's texels [min, max) on the skin: the texel is clamped into it, so a face
//          never shows a neighbor's column at its edge
//   data = the instance's light (a LightValue: sky:4 | red:4 | green:4 | blue:4) | the face's
//          shade (0..255, 255 = 1) << 16 | the instance's realm (4 bits) << 24
// The fragment lights the skin as the terrain is lit (the brightness curve, the sky color of the
// time of day and the ambient floor of the instance's realm, the block light gain), without AO,
// then fades it into the fog. The face's shade is the CPU's (it takes the realm's into account).

// Where the instance's realm sits in `data` (boxel_shaders::gpu::model::REALM_SHIFT).
const MODEL_REALM_SHIFT: u32 = 24u;

@group(0) @binding(0) var skin: texture_2d<f32>;
@group(1) @binding(0) var<uniform> view: View;

struct ModelOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) rect: vec4<u32>,
  @location(2) @interpolate(flat) data: u32,
  @location(3) view_dist: f32,
};

@vertex
fn vs_model(
  @location(0) pos: vec3<f32>,
  @location(1) uv: vec2<f32>,
  @location(2) rect: vec4<u32>,
  @location(3) data: u32,
) -> ModelOut {
  var o: ModelOut;
  o.clip = view.view_proj * vec4(pos, 1.0);
  o.uv = uv;
  o.rect = rect;
  o.data = data;
  o.view_dist = length(pos - view.camera_rel.xyz);
  return o;
}

// The skin texel under the fragment, inside its face's rectangle.
fn model_texel(i: ModelOut) -> vec4<f32> {
  let lo = vec2<i32>(i.rect.xy);
  let hi = vec2<i32>(i.rect.zw) - vec2<i32>(1, 1);
  let t = clamp(vec2<i32>(floor(i.uv)), lo, max(hi, lo));
  return textureLoad(skin, t, i32(0));
}

// albedo * face shade * light, then the distance fog (`shade_fragment` of the terrain, with one
// light for the whole instance and no AO).
fn model_shade(albedo: vec3<f32>, i: ModelOut) -> vec4<f32> {
  let l = vec4<f32>((vec4(i.data) >> vec4(LIGHT_SKY_SHIFT, LIGHT_RED_SHIFT, LIGHT_GREEN_SHIFT, LIGHT_BLUE_SHIFT)) & vec4(15u));
  let env = realm_env((i.data >> MODEL_REALM_SHIFT) & 15u);
  let sky = level_curve(l.x) * env.sky.rgb;
  let block = vec3(level_curve(l.y), level_curve(l.z), level_curve(l.w)) * view.light_params.y;
  let light = max(max(sky, block), env.ambient.rgb);
  let shade = f32((i.data >> 16u) & 255u) / 255.0;
  let lit = albedo * shade * light;
  let fog = clamp((i.view_dist - view.fog_params.x) / max(view.fog_params.y - view.fog_params.x, 1.0), 0.0, 1.0);
  return vec4(mix(lit, view.fog_color.rgb, fog), 1.0);
}

// Opaque boxes (pipeline with cull_mode Back): the skin's alpha is ignored, no discard.
@fragment
fn fs_model_opaque(i: ModelOut) -> @location(0) vec4<f32> {
  return model_shade(model_texel(i).rgb, i);
}

// Cutout boxes, the outer layers (pipeline with cull_mode None, so the inside of a hat shows
// through its holes): alpha test, as the terrain's cutout bucket.
@fragment
fn fs_model_cutout(i: ModelOut) -> @location(0) vec4<f32> {
  let albedo = model_texel(i);
  if (!alpha_test_passes(albedo)) { discard; }
  return model_shade(albedo.rgb, i);
}
