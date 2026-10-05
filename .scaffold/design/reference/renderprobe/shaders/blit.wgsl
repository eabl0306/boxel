// Fullscreen blit of the main color target to the surface (all tiers). Standalone module.
@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

struct BlitOut { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex
fn vs_fullscreen(@builtin(vertex_index) vi: u32) -> BlitOut {
  let uv = vec2<f32>(f32((vi << 1u) & 2u), f32(vi & 2u));
  var o: BlitOut;
  o.pos = vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
  o.uv = uv;
  return o;
}

// target is an *Srgb format: hardware encodes
@fragment
fn fs_blit(i: BlitOut) -> @location(0) vec4<f32> {
  return textureSample(src_tex, src_sampler, i.uv);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
  let lo = c * 12.92;
  let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
  return select(hi, lo, c <= vec3(0.0031308));
}

// target is a non-sRGB format: encode manually
@fragment
fn fs_blit_encode_srgb(i: BlitOut) -> @location(0) vec4<f32> {
  let c = textureSample(src_tex, src_sampler, i.uv);
  return vec4(linear_to_srgb(c.rgb), c.a);
}
