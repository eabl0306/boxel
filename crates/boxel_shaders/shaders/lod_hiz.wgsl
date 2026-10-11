// Far LOD, tier High (ADR 0031): the depth pyramid the LOD cull tests nodes against, built from
// the main depth right after the opaque and cutout terrain (the LOD and everything after it are
// drawn in a second pass).
//
// Reverse-Z: a larger depth is nearer, 0 is the sky. A texel of level m holds the smallest
// (farthest) depth of the 2^(m+1) x 2^(m+1) pixels it stands for, so a node whose nearest depth
// is below every texel its screen rectangle touches lies behind full detail everywhere and draws
// nothing. Each level is half the one below, rounded down (the texture's own mip sizes; level 0
// is half the target): a texel stands for its 2x2 block below, and the last one of a row or
// column also for the odd one left over, so pixel p lies under texel
// min(floor(p / 2^(m+1)), last) of level m.
@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var reduced: texture_storage_2d<r32float, write>;

// The farthest depth of the source texels under texel `t`: its 2x2 block, to the source's edge
// for the last texel of a row or column.
fn farthest(t: vec2<i32>) -> f32 {
  let last = vec2<i32>(textureDimensions(source)) - 1;
  let end = vec2<i32>(textureDimensions(reduced)) - 1;
  let lo = min(2 * t, last);
  let hi = select(min(2 * t + 1, last), last, t == end);
  var d = textureLoad(source, lo, 0).r;
  for (var y = lo.y; y <= hi.y; y++) {
    for (var x = lo.x; x <= hi.x; x++) {
      d = min(d, textureLoad(source, vec2(x, y), 0).r);
    }
  }
  return d;
}

@compute @workgroup_size(8, 8)
fn hiz_reduce(@builtin(global_invocation_id) gid: vec3<u32>) {
  let size = textureDimensions(reduced);
  if (gid.x >= size.x || gid.y >= size.y) { return; }
  let t = vec2<i32>(gid.xy);
  textureStore(reduced, t, vec4(farthest(t), 0.0, 0.0, 0.0));
}
