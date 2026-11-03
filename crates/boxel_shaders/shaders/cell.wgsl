// Cell quads (ADR 0050): what a cell draws from its data (an item in a frame, the items on a
// shelf), baked into its chunk's mesh by the mesher and drawn after the solid terrain, on every
// tier with this same pipeline and the same vertices. Prepended with common.wgsl (the View, the
// light and the shading of the terrain).
//
// Exactly ONE uniform block (the View) and ONE texture/sampler pair (the cell texture array and
// its sampler, the block sampler's settings) per pipeline, no storage buffer, no immediates (ADR
// 0011). Per vertex (boxel_shaders::gpu::CellVertex):
//   origin = the chunk's world voxel min corner, low 32 bits (wraps; only used minus
//            render_origin, as the terrain does)
//   pos    = the corner in its chunk (blocks)
//   uv     = the point of the image it shows (a fraction of it)
//   data.x = image layer (12 bits) | face (3 bits) << 12 | full bright (1 bit) << 15 | the
//            cell's LightValue << 16
//   data.y = tint, sRGB 0xRRGGBB | opacity (8 bits, translucent quads only) << 24
//   sky    = the sky's red, green and blue at the cell (0x0RGB, ADR 0064): the LightValue's sky
//            level three times where the sky has no tint
// fs_cell is a cutout terrain fragment: alpha test, then shade_fragment with the quad's face,
// its cell's light at the 4 corners (its sky per channel where it is tinted), AO open, its chunk's realm (realm planes are chunk
// aligned, so the chunk's realm is its cell's) and the dynamic lights near it (ADR 0062). fs_cell_translucent (ADR 0055, its pipeline
// blends and writes depth) shades the same way and gives the image's alpha times the opacity,
// dropping texels of alpha under a tenth as Minecraft's translucent item shader does.
//
// Pattern quads (ADR 0056) have their own pipeline, vs_cell_pattern and fs_cell_pattern, with
// the pattern texture array bound as group 0: a pattern fixed to the screen, as Minecraft's
// rendertype_end_portal. data.x's layer is the first of three (base, layer image, tints) and
// data.y's high byte (the opacity's) the number of layers over the base; the uv is not read.

@group(0) @binding(0) var cell_textures: texture_2d_array<f32>;
@group(0) @binding(1) var cell_sampler: sampler;
@group(1) @binding(0) var<uniform> view: View;

const CELL_LAYER_MASK: u32 = 0xFFFu;
const CELL_FACE_SHIFT: u32 = 12u;
const CELL_BRIGHT_BIT: u32 = 0x8000u;
const CELL_LIGHT_SHIFT: u32 = 16u;
const CELL_OPACITY_SHIFT: u32 = 24u;
// Texels under this alpha are not drawn by a translucent quad (Minecraft's `color.a < 0.1`).
const CELL_MIN_ALPHA: f32 = 0.1;

struct CellOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) data: vec2<u32>,
  @location(2) view_dist: f32,
  @location(3) @interpolate(flat) realm: u32,      // the realm of the quad's chunk (realm_at), the sky's red, green and blue from bit 4 (CELL_SKY_SHIFT), then the dynamic lights that may reach it from bit 16 (AO_DYNAMIC_SHIFT)
};

// Where CellOut::realm keeps the cell's sky red, green and blue (12 bits, between the realm and
// the dynamic lights).
const CELL_SKY_SHIFT: u32 = 4u;

// A cell quad's corners are within about a block of each other, so the dynamic lights within
// this many blocks of the reach of one corner hold every light of the quad.
const CELL_DYNAMIC_MARGIN: f32 = 2.0;

// The corner `pos` of a quad of the chunk at `origin`, relative to the render origin.
fn cell_rel(origin: vec3<i32>, pos: vec3<f32>) -> vec3<f32> {
  // wrapping i32 subtraction first, as finish_vertex: exact at any world position
  return vec3<f32>(origin - view.render_origin.xyz) + pos;
}

@vertex
fn vs_cell(
  @location(0) origin: vec3<i32>,
  @location(1) pos: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) data: vec2<u32>,
  @location(4) sky: u32,
) -> CellOut {
  let rel = cell_rel(origin, pos);
  var o: CellOut;
  o.clip = view.view_proj * vec4(rel, 1.0);
  o.uv = uv;
  o.data = data;
  o.view_dist = length(rel - view.camera_rel.xyz);
  o.realm = realm_at(origin.y - view.render_origin.y) | ((sky & TINT_CORNER_MASK) << CELL_SKY_SHIFT);
  if (view.dynamic_count.x != 0u) {
    let lights = dynamic_mask(rel - vec3(CELL_DYNAMIC_MARGIN), rel + vec3(CELL_DYNAMIC_MARGIN));
    o.realm = o.realm | (lights << AO_DYNAMIC_SHIFT);
  }
  return o;
}

// The most layers a pattern quad draws over its base (boxel_shaders::gpu::cell).
const PATTERN_LAYERS_MAX: u32 = 16u;

// Layer k's turn (cos, sin): Minecraft's (k * k * 4321 + k * 9) * 2 degrees, taken modulo 360
// exactly (whole degrees), so no tier takes the cosine of a large angle. A switch, not an array:
// no shader keeps an array in a local variable (naga.rs).
fn pattern_turn(k: u32) -> vec2<f32> {
  switch k {
    case 1u: { return vec2(0.939692621, 0.342020143); } // 20 degrees
    case 2u: { return vec2(0.719339800, 0.694658370); } // 44 degrees
    case 3u: { return vec2(0.309016994, 0.951056516); } // 72 degrees
    case 4u: { return vec2(-0.241921896, 0.970295726); } // 104 degrees
    case 5u: { return vec2(-0.766044443, 0.642787610); } // 140 degrees
    case 6u: { return vec2(-1.0, 0.0); } // 180 degrees
    case 7u: { return vec2(-0.719339800, -0.694658370); } // 224 degrees
    case 8u: { return vec2(0.034899497, -0.999390827); } // 272 degrees
    case 9u: { return vec2(0.809016994, -0.587785252); } // 324 degrees
    case 10u: { return vec2(0.939692621, 0.342020143); } // 20 degrees
    case 11u: { return vec2(0.173648178, 0.984807753); } // 80 degrees
    case 12u: { return vec2(-0.809016994, 0.587785252); } // 144 degrees
    case 13u: { return vec2(-0.848048096, -0.529919264); } // 212 degrees
    case 14u: { return vec2(0.241921896, -0.970295726); } // 284 degrees
    case 15u: { return vec2(1.0, 0.0); } // 0 degrees
    default: { return vec2(0.173648178, 0.984807753); } // 16: 80 degrees
  }
}

// A pattern quad is seen only from the side its face looks to (Minecraft culls the back of the
// end portal's faces). The eye's side of an axis-aligned quad's plane is the same at its four
// corners (their coordinate along the normal is the same float), so a quad seen from behind
// puts all of them outside the clip volume and draws nothing.
@vertex
fn vs_cell_pattern(
  @location(0) origin: vec3<i32>,
  @location(1) pos: vec3<f32>,
  @location(2) uv: vec2<f32>,
  @location(3) data: vec2<u32>,
  @location(4) sky: u32,
) -> CellOut {
  let rel = cell_rel(origin, pos);
  var o: CellOut;
  o.clip = view.view_proj * vec4(rel, 1.0);
  let normal = face_normal((data.x >> CELL_FACE_SHIFT) & 7u);
  if (dot(normal, view.camera_rel.xyz - rel) <= 0.0) {
    o.clip = vec4(2.0, 2.0, 0.5, 1.0);
  }
  o.uv = uv;
  o.data = data;
  o.view_dist = length(rel - view.camera_rel.xyz);
  o.realm = 0u;
  return o;
}

// sRGB to linear, per channel (the texture is sampled linear from its sRGB format).
fn cell_linear(c: vec3<f32>) -> vec3<f32> {
  let low = c / 12.92;
  let high = pow((c + vec3(0.055)) / 1.055, vec3(2.4));
  return select(high, low, c <= vec3(0.04045));
}

// Linear to sRGB, per channel: the value Minecraft's shaders work with (it has no linear light).
fn cell_display(c: vec3<f32>) -> vec3<f32> {
  let low = c * 12.92;
  let high = 1.055 * pow(c, vec3(1.0 / 2.4)) - vec3(0.055);
  return select(high, low, c <= vec3(0.0031308));
}

// The quad's texel `albedo` shaded as a terrain fragment: tint, face, the cell's light, fog. A
// cell whose sky is not its light's sky level three times (ADR 0064) is shaded as a tinted
// entry with its sky at the 4 corners and its block light flat (with_tint); the rest as before.
fn cell_shade(albedo: vec4<f32>, i: CellOut) -> vec4<f32> {
  let tint = vec3<f32>((vec3(i.data.y) >> vec3(16u, 8u, 0u)) & vec3(255u)) / 255.0;
  let light = i.data.x >> CELL_LIGHT_SHIFT;
  var corners = light | (light << 16u);
  var v: VsOut;
  v.clip = i.clip;
  v.uv = i.uv;
  v.layer = 0u;
  v.face = (i.data.x >> CELL_FACE_SHIFT) & 7u;
  let sky = (i.realm >> CELL_SKY_SHIFT) & TINT_CORNER_MASK;
  let level = (light >> LIGHT_SKY_SHIFT) & 15u;
  if (sky != (level | (level << 4u) | (level << 8u))) {
    corners = sky | (sky << TINT_CORNER_BITS);
    let block = light & TINT_CORNER_MASK;
    v = with_tint(v, vec2(block | (block << TINT_CORNER_BITS), block | (block << TINT_CORNER_BITS)));
  }
  v.view_dist = i.view_dist;
  v.st = vec2(0.0);
  v.light = vec2(corners, corners);
  v.ao = 0xFFu | select(0u, FLAG_EMISSIVE << 8u, (i.data.x & CELL_BRIGHT_BIT) != 0u)
    | ((i.realm & 15u) << AO_REALM_SHIFT) | ((i.realm >> AO_DYNAMIC_SHIFT) << AO_DYNAMIC_SHIFT);
  return shade_fragment(vec4(albedo.rgb * cell_linear(tint), albedo.a), v, view);
}

@fragment
fn fs_cell(i: CellOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(cell_textures, cell_sampler, i.uv, i.data.x & CELL_LAYER_MASK);
  if (!alpha_test_passes(albedo)) { discard; }
  return cell_shade(albedo, i);
}

@fragment
fn fs_cell_translucent(i: CellOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(cell_textures, cell_sampler, i.uv, i.data.x & CELL_LAYER_MASK);
  if (albedo.a < CELL_MIN_ALPHA) { discard; }
  let opacity = f32(i.data.y >> CELL_OPACITY_SHIFT) / 255.0;
  return vec4(cell_shade(albedo, i).rgb, albedo.a * opacity);
}

// Tint k of the pattern whose base is layer `base`: texel (k, 0) of its third layer, read as
// linear (the multiplier itself; the content stores it sRGB-encoded).
fn pattern_tint(base: u32, k: u32) -> vec3<f32> {
  return textureLoad(cell_textures, vec2<i32>(i32(k), 0), i32(base + 2u), 0).rgb;
}

// Minecraft's rendertype_end_portal.fsh, in its display values: the base image over the whole
// screen, tinted, plus `layers` layers of the layer image, layer k scaled by (4.5 - k / 4) * 2,
// turned by pattern_turn(k), moved by (17 / k, (2 + k / 1.5) * 1.5 * phase), then halved
// and moved by a quarter, each tinted by its tint; nearest texels of the top mip, the pattern
// textures repeating. The screen point is Minecraft's texProj0 / w: (0, 0) at the bottom left.
// The sum is clamped as Minecraft's 8-bit target does, then fogged as a terrain fragment.
@fragment
fn fs_cell_pattern(i: CellOut) -> @location(0) vec4<f32> {
  let base = i.data.x & CELL_LAYER_MASK;
  let layers = min(i.data.y >> CELL_OPACITY_SHIFT, PATTERN_LAYERS_MAX);
  let screen = vec2(i.clip.x / view.viewport.x, 1.0 - i.clip.y / view.viewport.y);
  let phase = view.light_params.z;
  let sky = textureSampleLevel(cell_textures, cell_sampler, screen, base, 0.0).rgb;
  var color = cell_display(sky) * pattern_tint(base, 0u);
  for (var k = 1u; k <= layers; k = k + 1u) {
    let f = f32(k);
    let turn = pattern_turn(k);
    let p = (4.5 - f / 4.0) * 2.0 * vec2(
      turn.x * screen.x - turn.y * screen.y,
      turn.y * screen.x + turn.x * screen.y,
    );
    let uv = 0.5 * (p + vec2(17.0 / f, (2.0 + f / 1.5) * (phase * 1.5))) + 0.25;
    let stars = textureSampleLevel(cell_textures, cell_sampler, uv, base + 1u, 0.0).rgb;
    color = color + cell_display(stars) * pattern_tint(base, k);
  }
  let tint = vec3<f32>((vec3(i.data.y) >> vec3(16u, 8u, 0u)) & vec3(255u)) / 255.0;
  let lit = cell_linear(min(color * tint, vec3(1.0)));
  let fog = clamp((i.view_dist - view.fog_params.x) / max(view.fog_params.y - view.fog_params.x, 1.0), 0.0, 1.0);
  return vec4(mix(lit, view.fog_color.rgb, fog), 1.0);
}
