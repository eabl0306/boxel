// Sun shadows (ADR 0065, lane S5, High only): the cell quads' depth in a cascade's map. Appended
// after common.wgsl, sun_off.wgsl, shadow_common.wgsl and cell.wgsl (its CellOut and cell_rel,
// the cell or block texture array in group 0, the View in group 1).
//
// Only cutout quads come here (a sign's board and text, an item in a frame): the translucent
// ones cast nothing, as glass and water, and neither do the pattern quads. A quad whose face does
// not look away from the sun (its shading face, the nearest cube face) is collapsed, the rule of
// the terrain's cutout bucket: a closed board casts with its back faces, its thickness the margin
// behind each lit face. fs_cell_shadow drops the texels the alpha test drops, at the cascade's
// mip level.
@group(3) @binding(0) var<uniform> cascade: ShadowCascade;

// Where a collapsed quad's corners go: one point outside the map, so its triangles have no area.
const CELL_SHADOW_COLLAPSED: vec4<f32> = vec4<f32>(2.0, 2.0, 2.0, 1.0);

@vertex
fn vs_cell_shadow(
  @location(0) origin: vec3<i32>,
  @location(1) pos: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) data: vec2<u32>,
  @location(4) sky: u32,
) -> CellOut {
  var o: CellOut;
  o.clip = cascade.view_proj * vec4(cell_rel(origin, pos), 1.0);
  if (dot(face_normal((data.x >> CELL_FACE_SHIFT) & 7u), cascade.light.xyz) >= 0.0) {
    o.clip = CELL_SHADOW_COLLAPSED;
  }
  o.uv = uv;
  o.data = data;
  o.view_dist = 0.0;
  o.realm = 0u;
  return o;
}

@fragment
fn fs_cell_shadow(i: CellOut) {
  let layer = i.data.x & CELL_LAYER_MASK;
  let albedo = textureSampleLevel(cell_textures, cell_sampler, i.uv, layer, cascade.params.y);
  if (!alpha_test_passes(albedo)) { discard; }
}
