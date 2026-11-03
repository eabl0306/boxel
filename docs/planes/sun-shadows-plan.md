# Sun shadows on the High tier: design plan

Issue #9 (light plan P8: "sun shadow cascades on High"). This is a design document: nothing here
is built. Paths are relative to the repo root. Numbers marked "measured" come from
`docs/rendimiento/` and the ADRs; the rest are estimates that each lane's measurement checks.

**Status: proposal (2026-11-02).** Waiting for the owner's answers to sec. 9.

**Verdict.** Build Option A, "voxel cascades": classic cascaded shadow maps drawn from the quad
arena that is already on the GPU, with five voxel-specific choices that make them cheap and safe:

1. Shadows only **darken the sky term** of today's light, by at most a strength `k`. Every way a
   shadow can fail (a caster missing, a list full, a chunk dropped, a receiver out of range) falls
   back to today's image, never to light where there was none. Caves keep `sky = 0`, so they never
   get sun.
2. Casters are the **faces that look away from the sun**, chosen per face bucket by the shadow
   cull (3 of the 6 opaque buckets). Voxel geometry is closed, so this leaves a full block of margin
   behind every lit face: no shadow acne, no peter-panning, almost no depth bias.
3. Only chunks with some **sky-lit face** cast (one flag bit per chunk, from its light palette).
   Underground the shadow pass draws nothing.
4. The **shadow distance is fixed** (160 blocks by default), so the cost does not grow from
   radius 16 to radius 32.
5. Mid and Low do not change at all: no new binding, uniform field, vertex attribute or module text
   reaches them. High with shadows off is today's High.

Estimated cost at 1280x720: +0.08 to 0.15 ms of GPU per frame on the RTX 5050 and +0.3 to 0.6 ms
on the Radeon 780M, the same at radius 16 and 32; 51 MiB of VRAM on High only.

The voxel-native alternatives (an occupancy volume ray marched per pixel, or a heightmap of the sky
tops) are costed in sec. 3. They are exact in places where shadow maps are not, but they cost per
pixel on the 780M, need a new data path from the world to the renderer, and get leaves, slabs,
entities and overhangs worse.

## 1. What exists today (checked against the code)

**Light in the fragment** (`crates/boxel_shaders/shaders/common.wgsl`):

- `voxel_light(i, view)` interpolates the 4 corner `LightValue`s of the quad's palette entry
  (`corner_light`, `bilinear`), raises the block channels to the dynamic lights
  (`dynamic_levels`, ADR 0062) and returns
  `max(level_curve(S) * env.sky.rgb, level_curve(rgb) * gain, env.ambient.rgb)`, where `env` is
  `realm_env(out_realm(i))`. So the baked sky level `S` (0 to 15) is already in the fragment: it
  is the outdoor mask the issue asks for, at no cost. `level_curve(0)` is 0, so anything
  multiplied into the sky term is invisible where `S = 0`.
- `shade_fragment` multiplies albedo, `face_shade(i.face, realm)`, `ao_factor` and `voxel_light`,
  then mixes the fog. Emissive quads skip all of it.
- `fragment_position(frag)` rebuilds a fragment's camera-relative position from its framebuffer
  position and depth through `View::unproject` (added for ADR 0062). A shadow lookup can use it
  without a new varying.
- `VsOut::ao` is full: AO bits 0..7, flags 8..11, realm 12..15 (`AO_REALM_SHIFT`), dynamic light
  mask 16..31 (`AO_DYNAMIC_SHIFT`). There is no free bit in it.
- `View` is 1408 B plus the 8 KiB box table (`crates/boxel_shaders/tests/naga.rs` asserts
  `1408 + 8192`). `RealmEnv` is 3 vec4 per realm and all 12 floats are used (`sky.w` and
  `ambient.w` hold the +Y and -Y face shades). It is the one uniform block of Low (ADR 0011), so
  any field added to it reaches Low.
- `model.wgsl::model_shade` repeats the light formula with one `LightValue` per instance;
  `cell.wgsl` goes through `shade_fragment`; `lod_common.wgsl` has its own open-sky shading.

**Shader modules** (`crates/boxel_shaders/src/lib.rs`): composition is concatenation in
`LazyLock<String>`s; `common.wgsl` is prepended to `VoxelPull`, `VoxelInstanced`, `Cull`, `Lod`,
`LodPull`, `LodCull`, `Model` and `Cell`. **High and Mid share `VoxelPull`**, so a receiver that
needs a new binding cannot go into `voxel_pull.wgsl` without reaching Mid; High needs its own
module.

**The High path** (`crates/boxel_render/src/path/high.rs`, `crates/boxel_shaders/shaders/cull.wgsl`):

- `HighPath::encode_compute` clears the draw counts and dispatches `cull_chunks`, one thread per
  slot. The cull skips slots without `FLAG_LIVE` (the camera window of ADR 0014), slots the cave
  culling hides (`cull.visible`, ADR 0022), tests the chunk AABB against `View::frustum`, and per
  face bucket runs `bucket_visible` with `View::camera_rel`. It writes one opaque and one cutout
  `DrawIndexedIndirect` list per page (`ListPair`, about 8 MiB at 4 pages).
- `HighPath::draw_solid` binds each page (`bindings::GROUP_GEOMETRY`) and issues one
  `multi_draw_indexed_indirect_count` per page and list. Translucent buckets come from a CPU list.
- None of this can be reused as is for the sun: the cave culling bits and the camera frustum
  would drop casters behind the camera, and `bucket_visible` tests a point, not a direction. A
  shadow cull is a second dispatch with its own shader.
- The cull keeps 4 storage buffers per stage (the Vulkan minimum). `wgpu` gives 4 bind groups;
  the voxel pipelines use 0 (materials), 1 (view) and 2 (geometry), so group 3 is free on High.
- High is Vulkan with `multi_draw_indexed_indirect_count`: the RTX 5050 and the 780M (RADV) are
  both High. The far LOD already has a High-only GPU path (`LodPass::gpu_driven`), the precedent
  for a pass that only High has, and it already skips that path on integrated GPUs
  (`lod::gpu_driven_pays`, `wgpu::DeviceType::IntegratedGpu`), the precedent for question 1.

**Arena and chunk table**: quads live in pages (ADR 0035), 8 bytes each; `ChunkGpu` is 64 B with
`flags` bit 0 = live and bits 1..6 = edge walls (`chunk_table.rs`, `ChunkRecord::gpu`). The
colored sky plan reserves bits 7..19 for its tinted entry count. The light palette of each chunk
follows its quads (`light_base`, `light_len`), so the CPU can tell at upload whether any quad of
a chunk has sky light (an OR over the palette's sky nibbles).

**Light plan P8 checked**: "geometry-only depth from the same arena" is right for cubes, but box
faces, planes and sloped fluids decode their shape from `mat` bits (`decode_quad`: turns, plane
kind, fluid heights) and the cutout alpha test needs the texture layer, so the shadow vertex
shader reads the whole 8-byte quad (the same cache line). "A second cull dispatch" is right, as a
separate shader. "The baked sky level as the outdoor mask" is right and costs nothing (above).

**The sun** (`crates/boxel_world/src/daylight.rs`): `TimeOfDay::sun_elevation` is
`sin(2 pi * day_fraction)`, 0 at sunrise (tick 0, 06:00), 1 at noon. There is no sun direction,
no azimuth and no tilt anywhere. `DaylightCurve::sample` maps the elevation to sky light (white
1.0 by day, warm 0.6 at the horizon, blue 0.2 at night). `RealmLighting` holds per realm a
`RealmLight { curve, ambient, face_shade }`; `boxel_render::plugin::scene_light` turns it into
`SceneLight` and `SceneRealms::uniform` into `View::realm_env`.

**Targets**: main depth is `Depth32Float`, reverse-Z, camera-relative (ADR 0007, ADR 0014).

**Tests**: tier tests compare every tier against the best tier of the same adapter (0.1 % of the
pixels, channel tolerance 2, as `gpu_dynamic_lights.rs` states), at `FAR_SHIFTS` positions that
are multiples of 32 (`gpu_cave_culling.rs`, `gpu_realms.rs`). Failing images are saved under
`CARGO_TARGET_TMPDIR`. There are no golden images of the GPU output on disk.

**Measured costs used below**: at 1280x720 (the `stress` default), the RTX at radius 16 on the
surface runs p50 0.41 ms uncapped and the 780M 1.66 ms (`control-2026-10-13.md`); the 780M drew
8.4 to 10 M quads in about 3.1 ms (`cuevas-v1-2026-10.md`); the overworld has 3.66 M quads at
radius 16 on the surface and 14 to 15.5 M at radius 32 (`luz-p7-2026-10.md`); the far LOD costs
0.54 to 0.60 ms on the RTX and 1.1 to 1.85 ms on the 780M; the fog 1.06 to 1.11 ms on the RTX.

## 2. The rule

What "correct" means here.

- The sun is a direction `L` (toward the sun) from `TimeOfDay` and a sun path tilt (sec. 4.1).
- Per realm, a shadow strength `k_r = strength * sun_r * fade(elevation)`: `strength` from the
  settings (0.4 by default), `sun_r` 1 if the realm has the sun (sec. 4.6), `fade` a smoothstep
  from 0 at the horizon to 1 at an elevation sine of 0.2 (about 11.5 degrees), so long sunset
  streaks fade out and the night has none.
- A fragment's sun visibility `v` (0 to 1) is the shadow map's answer (filtered), times a ramp on
  `N . L` (a face looking away from the sun is in its own shadow), faded to 1 at the end of the
  shadow distance.
- The sky term becomes `level_curve(S) * env.sky.rgb * (1 - k_r * (1 - v))`. Block light, dynamic
  lights, the ambient floor, AO, face shade, emissive quads and fog do not change.

Why this form:

- **It only darkens.** The factor is in `[1 - k, 1]`, and every later step (`max`, products,
  the fog `mix`) is monotone, so a shadowed image is never brighter than the same image without
  shadows, per pixel and channel. Sunlit surfaces keep today's exact color. A cheap invariant for
  tests (sec. 5).
- **Every failure gives today's look.** A missing caster means `v = 1`, which is today.
- **Caves get no sun by construction**: `S = 0` makes the sky term 0 whatever `v` says. That is
  also why chunks with no sky-lit face can be left out of the shadow pass (sec. 4.2).
- **The baked sky still does the soft part.** Under a tree today the ground has sky 13 or 14
  (spread light); the shadow adds the crisp sun shadow on top of it. Inside a house with a window,
  the floor patch the sun reaches through the window keeps its baked sky light, the rest of the
  room loses `k`: a sunlit patch.

What it does not do: it does not add light. A sunbeam through a window into a room with sky 5 is
the room's sky 5 with no shadow, not full sunlight. Adding a direct sun term would need its own
mask against caves and would break the "falls back to today" property; it is left out on purpose.

## 3. The options

Costs at 1280x720 unless stated; per-pixel parts scale by 2.25 at 1920x1080. "Casters" are the
quads drawn into shadow maps.

**How the casters were estimated (Option A).** The overworld surface at radius 16 has 3.66 M
quads in about 800 chunk columns, about 4 500 per column, most in the caves under the surface.
About 1 700 per column are in chunks with sky-lit faces. The cascades of sec. 4.1 cover about 70
columns around the camera, about 150 with the extrusion toward a sun at 30 degrees. Faces away
from the sun are about half of them. That gives 0.1 to 0.3 M caster quads per frame, the same at
radius 16 and 32, against 3.66 M to 15.5 M in the arena. The forest has fewer quads per column
(about 1 000, mostly leaves), all of them cutout.

### 3.1 Option A: cascaded shadow maps from the arena (recommended)

A depth-only pass per cascade from the sun, drawing the arena's quads through a second cull
dispatch; the main fragment shader on High compares its position against them.

- **GPU memory (High only)**: 3 cascades of 2048 x 2048 `Depth32Float` in one array texture,
  48 MiB (12 MiB at 1024^2, 64 MiB with 4 cascades); draw lists per cascade capped at the chunks a
  cascade can hold, about 0.9 MiB each at 4 pages; a 0.5 KiB uniform. About 51 MiB. Mid and Low:
  0.
- **Passes per frame**: one compute dispatch (all cascades), then one depth-only render pass per
  cascade, each with 2 multi-draws per page (opaque, cutout). The main pass binds one more group.
  At night (`k = 0`) or with no sky-lit chunk in reach: none, and the main pass runs today's
  pipelines.
- **Cost**:

  | | r16 | r32 |
  |---|---|---|
  | RTX 5050 | +0.08 to 0.15 ms | +0.08 to 0.15 ms |
  | Radeon 780M | +0.3 to 0.6 ms | +0.3 to 0.6 ms |

  RTX: cull 0.01 to 0.02 ms (15 to 60 thousand slots), casters 0.02 to 0.05 ms, clears and fill
  of 12.6 M depth texels about 0.02 ms, the lookup in the main pass 0.03 to 0.06 ms. 780M: about
  4 times that, from its quad rate (8.4 to 10 M quads in 3.1 ms, measured) and fill rate. At 17:00
  (long shadows) the extrusion can double the casters. Underground: the cull only.
- **Quality**:
  - Acne: back-face casters leave a block of margin behind every lit face (sec. 4.2); a small
    slope bias and a normal offset of 1.5 texels cover box faces down to 1/16 block.
  - Peter-panning: none on closed voxel geometry: the caster is the far side of the occluder, so
    the shadow starts at the contact.
  - Shimmering: camera moves are stable (cascades are spheres snapped to their texel grid);
    camera turns are stable (sphere size does not change); the sun moves in small steps (sec. 4.1)
    so edges stand still between steps and move less than a texel at each step.
  - Cascade seams: a blend band of 10 % between cascades; texel sizes of about 1/50, 1/20 and 1/8
    block, all sharper than a block edge.
  - Aliasing: 2x2 hardware PCF (one compare sample); edges are soft over one texel.
- **Interactions**: sec. 4.6 has the detail. Caves: none by the rule. Sky sources: a realm whose
  light comes from sources has no sun (`k = 0`); a source under rock in a sun realm is shadowed
  like any covered place. Colored glass: glass casts nothing; its tint comes from the colored sky
  plan. Leaves: alpha-tested casters, dappled shadows. Water: casts nothing, receives. Cells and
  models: cast and receive (lane S5). Far coordinates: exact (camera-relative, snapping anchored
  on a 1024-voxel grid). Day and night: fades with the sun; no moon.
- **Risk**: L. Protected areas: render and shaders. No world, mesher, light or streaming change.

### 3.2 Option B: ray march an occupancy volume

A toroidal 3D bit volume of opaque voxels around the camera (clipmap of 3 levels), and per
sky-lit fragment a DDA toward the sun through it.

- **GPU memory**: 3 levels of 256^3 bits, 6 MiB; +6 MiB for a "partial" bit (leaves, slabs);
  +16 MiB for a 4-bit filter index if glass tints. 6 to 28 MiB.
- **Data path**: new. The renderer gets meshes, not voxels; it would need each chunk's
  `LightMasks` opaque rows (4 KiB, they exist) through the extract, written with
  `queue.write_texture` when a chunk loads or changes: about 1 MiB per crossing.
- **Passes per frame**: none (a small compute for the coarse levels of dirty bricks).
- **Cost** (per pixel, so the same at r16 and r32): about 0.7 M rays of 16 to 48 steps.

  | | r16 and r32, 720p | 1080p |
  |---|---|---|
  | RTX 5050 | +0.15 to 0.4 ms | +0.3 to 0.9 ms |
  | Radeon 780M | +0.8 to 1.6 ms | +1.8 to 3.6 ms |

- **Quality**: exact block shadows, no acne, no peter-panning, no shimmering when the sun moves,
  no cascades. Hard, pixel-sharp edges; soft edges need more rays. Slabs, stairs, fences and
  torches cast as full blocks unless the volume has sub-voxels (8 times the memory). Leaves cast
  solid or need a hash pattern for holes.
- **Interactions**: caves as A (gate on `S`). Colored glass: exact tinted shadows with the filter
  volume. Water: skipped. Cells and models: do not cast (they are not voxels). Far coordinates:
  exact (integer DDA relative to the anchor). Day and night: as A.
- **Risk**: L to XL: a new data path from the world to the renderer, a new GPU structure kept in
  step with streaming, and a per-pixel cost that grows with resolution on the GPU that can least
  afford it.

### 3.3 Option C: heightmap of the sky tops (shadow columns)

A 2D texture of the sky top `T(x, z)` around the camera (the value `sky_tile::walk` already
computes exactly, from content), and per fragment a 2D march toward the sun: the sun is blocked
when some column along the way has `T` above the ray.

- **GPU memory**: 1024^2 `i32` relative to the anchor, 4 MiB, plus a max pyramid, 1.4 MiB.
- **Data path**: new, from `SkyMap` tiles (4 KiB per 32x32 tile when a tile becomes `Exact` or an
  edit moves a top).
- **Passes per frame**: none.
- **Cost**: RTX +0.05 to 0.15 ms, 780M +0.2 to 0.5 ms at 720p, the same at r16 and r32.
- **Quality**: only voxels above `T` (direct sky) can be in the sun: under any overhang, porch or
  bridge it is always shadow, even when a low sun would shine under it. Trees cast solid columns
  from the top of the canopy to the ground, so at sunset their shadows are long dark streaks with
  no light under the canopy and no holes. Edges are block columns. No acne, no shimmering.
- **Interactions**: glass has opacity 0, so it never casts (right for clear glass, no tint).
  Leaves have opacity 1, so they count as solid columns. Cells, models and anything not in `T`
  do not cast. Sky source beams are direct sky below `T`: always shadowed.
- **Risk**: M. Cheap and exact for hills, wrong for most built things.

### 3.4 Other ways, rejected

- **Bake sun visibility into the meshes** like the sky light: the sun moves, so every chunk would
  remesh every few game minutes. Even a few fixed sun positions multiply the palette.
- **Hardware ray queries**: acceleration structures for 3.66 to 15.5 M quads (two triangles each)
  take hundreds of MiB, every remesh rebuilds one, and `wgpu`'s ray queries are still
  experimental.
- **Screen-space shadows** (march the depth buffer toward the sun): misses every caster off
  screen or behind something. Useful only as extra contact detail, not as the shadow.
- **A hybrid of A and C** (cascades near, heightmap far, for mountain shadows at sunset): adds
  C's data path for a few distant shadows. Left for later (sec. 6, "Later").

### 3.5 Summary

| | **A: cascades (rec.)** | B: occupancy march | C: heightmap |
|---|---|---|---|
| VRAM on High | 51 MiB (12 to 64) | 6 to 28 MiB | 5.4 MiB |
| New data from the world | none | voxels per chunk | sky tops per tile |
| Extra passes | 1 dispatch + 3 depth passes | none | none |
| RTX, r16 and r32 | +0.08 to 0.15 ms | +0.15 to 0.4 ms | +0.05 to 0.15 ms |
| 780M, r16 and r32 | +0.3 to 0.6 ms | +0.8 to 1.6 ms | +0.2 to 0.5 ms |
| Grows with resolution | little | yes | some |
| Slabs, fences, torches | exact | full blocks | columns |
| Leaves | dappled | solid | solid columns |
| Under overhangs | right | right | always shadow |
| Entities and cells cast | yes (S5) | no | no |
| Tinted glass shadows | no (sky tint only) | possible | no |
| Shimmer when the sun moves | small steps | none | none |
| Falls back to today on failure | yes | yes | yes |
| Size | L | L to XL | M |

## 4. Option A in detail

### 4.1 The sun and the cascades (CPU, every frame)

- **Direction.** `SunPath { tilt_degrees }` (a new resource beside `TimeOfDay`, default 30):
  with `theta = 2 pi * day_fraction`, the direction toward the sun is
  `(cos theta, sin theta * cos tilt, sin theta * sin tilt)`. It rises at +X (east, as in
  Minecraft), stands at its highest at noon (elevation sine `cos tilt` there, 0.87 at 30 degrees)
  and sets at -X. With tilt 0 it is Minecraft's path, but then +Z and -Z faces are edge-on to the
  sun all day and the noon shadows are vertical; the tilt avoids both (question 4).
  `sun_elevation` keeps its meaning for the daylight curve (the sky light does not change).
- **Steps.** The angle is rounded to steps of 0.05 degrees (one step about every 1/6 s with the
  20-minute day). Between steps the shadow maps stand still; a step moves the edge of a 32-block
  tower's shadow by less than a texel of the far cascade.
- **Splits.** 3 cascades over view distance `[0, 20)`, `[20, 56)`, `[56, 160]`, with 10 % blend
  bands; the last one fades to no shadow over its last 10 %. The shadow distance is a setting
  (question 5); the splits scale with it.
- **Fit.** Each cascade is the bounding sphere of its slice of the camera frustum, so its size
  does not change when the camera turns; its light-space box is the sphere's square in the plane
  across `L` and, along `L`, from the sphere's far side to the top of the loaded chunks plus a
  margin (casters above the receivers). Depth is `Depth32Float`, so the long box costs nothing in
  precision.
- **Snapping, exact at any distance (ADR 0014).** The box center is snapped to whole texels in
  light space relative to an **anchor**: the render origin rounded down to a multiple of
  `SHADOW_ANCHOR = 1024` voxels, in `i64`. The CPU computes the anchor's offset from the render
  origin (at most 1023 per axis, exact in `f32`) and does the light-space math in `f64` relative
  to it. Two scenes that differ by a multiple of 1024 voxels get the same cascades relative to
  their render origin, bit for bit. When the camera crosses a 1024 boundary the snap grid shifts
  by a fraction of a texel once (one frame of edge movement every 1024 blocks of travel).
- **Matrices** are camera-relative like `view_proj`: the shadow pass and the lookups subtract the
  render origin with the same wrapping `i32` arithmetic as today.

### 4.2 Casters and the shadow cull (GPU, High)

New `shadow_cull.wgsl` (its own module, High only), one thread per slot, looping over cascades:

- **Skips** a slot without `FLAG_LIVE`, without the new **`FLAG_SKY_LIT`** (`ChunkGpu.flags`
  bit 31, set at upload when any palette entry has sky above 0 at a corner), or outside the
  camera's realm band (sec. 4.6). It ignores the cave culling bits and the camera frustum: a tree
  behind the camera shades the ground in front of it.
- **Tests** the chunk AABB against the cascade box (6 planes, camera-relative, as `in_frustum`).
- **Buckets**: draws face bucket `f` only if its normal looks away from the sun
  (`dot(n_f, L) < 0`, a 6-bit mask the CPU computes once per frame). Edge-on buckets project to
  lines and are skipped. Edge walls follow the main cull's rule (left out while the chunk across
  is resident). The cutout bucket is always drawn.
- **Lists**: per cascade and page, one opaque and one cutout compacted list, sized for the most
  chunks a cascade box can hold (from the shadow distance and the vertical radius), with the same
  full-list guard as `cull_chunks`. A full list drops casters (today's look), counted in
  `RenderStats`.
- **Bindings**: one uniform (`ShadowUniform`: render origin, slot count, cascade planes, bucket
  mask) and 4 storage buffers (chunk table, args, refs, counts), within the Vulkan minimum.

Why back faces. Terrain meshes are closed (faces between solid blocks are never meshed, across
chunks too), so a ray from the sun crosses a front face, then the occluder, then a back face.
Storing only back faces puts the stored depth at the far side of the first occluder: a lit face
compares against a depth at least one block (1/16 for the thinnest box) behind it. A face that
looks away from the sun is a caster itself and is put in shadow by `N . L` without a lookup. This
is why acne and peter-panning are not a problem here.

Why the sky-lit gate is safe. A chunk left out has no face with sky light. Under the rule of
sec. 2 its faces can only matter as the occluder of a receiver with sky above 0, and a back face
that faces such a receiver across air is lit by that sky unless it is more than 14 blocks of air
from any sky. Where the gate misses a caster, the receiver keeps today's look. A test builds that
case on purpose (lane S3).

### 4.3 The shadow pass (GPU, High)

New `shadow.wgsl` (High only):

- `vs_shadow`: reads the quad and its chunk as `vs_pull` does, runs `decode_quad` (exact shapes
  for slabs, stairs, fences, torches, planes, fluids), multiplies by the cascade's matrix, and grows
  each corner by half a shadow texel in the quad's plane so T-junction cracks cannot leak light
  through the map (the light-space twin of `edge_growth`). It binds the main `View` (group 1) for
  the box table, the page (group 2) and the cascade's entry of `ShadowUniform` (group 3, dynamic
  offset). The light palette entry is not read.
- **Opaque pipeline**: no fragment stage (depth only, fastest path), `cull_mode: None` (the
  buckets are already chosen), depth bias: constant 1, slope 1.5, tuned in lane S3.
- **Cutout pipeline**: `fs_shadow_cutout` alpha-tests the texture at a fixed mip level chosen from
  the cascade's texel size (`textureSampleLevel`), so leaves cast dappled shadows. In the vertex
  shader, a cutout quad that faces the sun is collapsed to a point (the same back-face rule as the
  buckets), and crosses and planes (small plants) are collapsed too (question 6).
- **Not drawn**: translucent quads (water, glass, ice) and the far LOD.
- **Order**: after `encode_compute`, before the main pass, one render pass per cascade layer
  (cleared to the far depth). No interaction with the far LOD's pyramid or the fog.

### 4.4 Receivers (fragment, High)

New `sun_shadow.wgsl` (High only) defines `sun_factor` and group 3: `shadow_maps:
texture_depth_2d_array`, `shadow_cmp: sampler_comparison`, `shadow: ShadowUniform`.

- `sun_factor(i, sky_level)`: returns 1 at once when `k` of the fragment's realm is 0, when
  `sky_level` is 0, or past the shadow distance (`i.view_dist`). Otherwise it takes the fragment
  position from `fragment_position(i.clip)`, offsets it along the face normal by 1.5 texels of the
  cascade, picks the cascade by view distance (blending in the band), does one hardware 2x2 PCF
  compare, multiplies by the `N . L` ramp (0 at `N . L <= 0`, 1 from 0.1, about 6 degrees), fades
  to 1 at the end of the distance, and returns `1 - k * (1 - v)`.
- Crosses and planes (plants) receive with `+Y` as their normal, as they are shaded today.
- The hook in `common.wgsl::voxel_light` is one factor on the sky term:
  `let sky = level_curve(l.x) * env.sky.rgb * sun_factor(i, l.x);`. Every module that includes
  `common.wgsl` appends either `sun_off.wgsl` (`return 1.0;`) or `sun_shadow.wgsl`. Multiplying by
  1.0 is exact in IEEE 754, so the off modules give today's floats.
- New module ids: `VoxelPullShadowed` (common, `sun_shadow`, `voxel_pull`) for High's opaque,
  cutout and translucent pipelines; `CellShadowed` and `ModelShadowed` in lane S5. Mid keeps
  `VoxelPull` with `sun_off`, Low keeps `VoxelInstanced` with `sun_off`.
- **Switching**: High builds the shadowed pipelines when shadows are turned on (a one-time cost
  of creating 3 pipelines, as the fog does when it first appears) and uses today's pipelines in
  frames where shadows are off, at night, or with no sky-lit chunk within reach of the cascades
  (then every receiver in range has `S = 0` and the image is the same either way).

### 4.5 Settings and data

- `RenderSettings::sun_shadows: SunShadowSettings { enabled: false, on_integrated: false,
  strength: 0.4, distance: 160, cascades: 3, resolution: 2048 }`, read every frame (resolution
  and cascades when they change). Ignored on Mid and Low; on an integrated GPU only with
  `on_integrated` (question 1).
- `SunPath` resource (tilt) in `boxel_world::daylight`, added by `DaylightPlugin`.
- `RealmLight::sun: Option<bool>`: whether the realm's sky has the sun. `None` means "the realm
  follows the global curve" (`curve` is `None`): those have the sun; realms with their own curve
  (a cavern, the Nether-like realm) have none unless they say so.
- `SceneLight::sun: Option<SceneSun { direction, strength: [f32; 9], camera_realm }>`, filled by
  the render extract (`scene_light`). Not in `View`: it goes to `ShadowUniform`, a High-only
  buffer, so Low's one uniform block does not grow.
- `RenderStats::shadows`: cascades drawn, shadow draws, dropped casters.

### 4.6 Interactions, one by one

- **Baked sky as the outdoor mask (caves).** The factor multiplies `level_curve(S)`, so where
  `S = 0` nothing changes. A cave mouth with `S` 1 to 14 can be shadowed, which is right: its
  light comes from outside.
- **Sky sources (ADR 0061).** A realm whose light comes from sources (a cavern realm with its
  own constant curve) has `sun = false`: `k = 0`, no change. A source under rock in a sun realm
  (a sunbeam in an overworld cave) has the rock above it as a caster, so its floor is darker by `k`
  on High, like any covered place (question 8). The beams themselves are not casters or receivers
  of anything special.
- **Realms (ADR 0039).** "No realm shadows another": the shadow cull keeps only chunks between
  the planes of the camera's realm, and `sun_factor` returns 1 for fragments of other realms
  (realm planes are at least 4096 apart, the shadow distance 160, so at most one other realm is
  ever in range).
- **Colored glass (ADR 0060, colored sky plan).** Glass is translucent and casts nothing. Under a
  stained-glass roof the sky term is already tinted by the colored sky plan, and the roof is no
  caster, so the floor keeps its tint and its sun. A window in a wall: the wall casts, the window
  does not, so the sunlit patch on the floor has the room's tint. Projecting the window's colors
  into the shadow (a color shadow map, +48 MiB) is left for later (question 6).
- **Leaves and cutout.** Leaves cast through the alpha test (dappled), and receive like any face.
  Their baked spread sky still darkens the ground a little, the shadow adds the sun shadow. Plants
  (crosses, planes) do not cast by default: their shadows are smaller than a block, shimmer, and
  cost vertex work in fields.
- **Water and translucent faces.** No translucent quad casts. Water surfaces and glass receive
  (`fs_translucent` goes through `shade_fragment`), so a tree's shadow falls on a lake. The sea
  floor under open water keeps today's look (water is not a caster; its depth darkening is the
  baked sky).
- **Cells and models (ADR 0040, 0050).** Lane S5: model instances and cell quads get a depth-only
  shadow pipeline each (their own vertex buffers, drawn into every cascade they touch), and
  receive through `ModelShadowed` and `CellShadowed`. Until S5 they neither cast nor receive (they
  look as today, also in a shadow, which is the one visible gap of S4).
- **Emissive quads** (lamps, lava) are drawn at full brightness and never shadowed.
- **Dynamic lights (ADR 0062, 0063)** are block light: unchanged.
- **Far coordinates (ADR 0014).** Everything is relative to the render origin; the snapping is
  anchored on a 1024 grid in `i64` (sec. 4.1). Tests run at shifts that are multiples of 1024:
  0, plus or minus 2^31, 2^40, plus or minus 2^35 and `MAX_BLOCK_COORD` rounded to 1024.
- **Day and night.** `k` follows the sun's elevation (fade near the horizon, 0 at night). At night
  nothing is drawn and every image is today's. No moon shadows (question 7): the night sky is a
  dim blue ambient at 0.2, and moonlight shadows would need their own strength and fade.
- **Far LOD and fog.** LOD faces are past the shadow distance: no shadows there, and the last
  cascade fades out before it. The fog march is unchanged (light shafts in the fog from the
  shadow map are a possible later feature).

## 5. Mid and Low stay as today; High gets its own references

**The rule.** On Mid and Low, every byte the GPU sees is the same as today: the same modules
(with `sun_off.wgsl`, which returns exactly 1.0), the same bind group layouts, the same `View`,
no new buffer, texture, pass or vertex attribute. High with shadows off is today's High. So every
existing tier test keeps comparing High, Mid and Low with the same tolerance, with shadows off.

How tests keep it:

- `tests/naga.rs`: `View` still `1408 + 8192`; Low still one uniform block, one texture pair, two
  instance attributes; the `VoxelPull` and `VoxelInstanced` modules have no binding in group 3;
  the shadowed modules and `shadow_cull` are marked High only; the forbidden builtins stay absent.
- Unit test in `boxel_render`: a renderer on Mid and on Low with `sun_shadows.enabled` has no
  shadow pass, no shadow resources and reports `RenderStats::shadows` as none.
- `gpu_sun_shadows.rs`: on Mid and Low, a sunny scene with shadows on equals the same scene with
  shadows off, byte for byte.
- Every existing gpu test (`gpu_terrain`, `gpu_tiers`, `gpu_realms`, `gpu_light_filter`, ...)
  runs unchanged and still passes on all tiers.

**High's own references.** Shadow edges depend on rasterization, so High with shadows cannot be
compared with Mid pixel for pixel. Instead `gpu_sun_shadows.rs` checks it against High with
shadows off and against a CPU oracle:

- **Never brighter**: per pixel and channel, shadows on is at most shadows off.
- **Equal where nothing can change**: a closed cave (`S = 0` everywhere) is byte-identical with
  shadows on and off; so is midnight, strength 0, a realm with `sun = false`, and everything past
  the shadow distance.
- **The oracle**: for each pixel, the CPU takes the surface point from a depth readback and its
  face, and casts an exact voxel ray (Amanatides-Woo) toward the sun through the scene's blocks.
  The GPU factor (shadows on over off) must say "shadow" or "sun" as the oracle does, except
  within 2 texels of an oracle shadow edge; at most 0.5 % of the other pixels may disagree.
- **Far coordinates**: the same scene at shifts that are multiples of 1024 gives identical bytes;
  at a shift that is not, the images may differ only near shadow edges (at most 0.5 %).
- **Scenes**: a pillar on a floor at noon and at 17:00, an overhang, a house with a window, a tree
  (dappled pixels in its shadow), a lake with a tree on the shore, a cave mouth, a two-realm
  scene, a sky source cavern realm.
- Images are saved under `CARGO_TARGET_TMPDIR/gpu_sun_shadows/` on failure (all of them with
  `GPU_SUN_SHADOWS_SAVE=1`) and `snapshot` gets presets for the owner's review (lane S6).

The colored sky plan has the same need (the owner decided High draws tinted faces exactly,
"like sun shadows"). Whichever lands first owns the helper that runs a High-only scene against
its references; the other reuses it.

## 6. Build order, in lanes

Every lane is in a protected area (render, shaders) and needs the owner's OK for it. Each lane's
measurement follows `AGENTS.md` "Cómo medir": level 1 only, one or two `perf-smoke` cases, the table
in the PR.

**Lane S0. ADR (docs only).** The next free ADR number (0064 is expected to go to the colored
sky's lane C0), in Spanish, from this plan, with the owner's answers; a note in ADR 0015 sec. 11
and in the light plan P8. XS.

**Lane S1. The sun hook** (`boxel_shaders`; protected: shaders). S.
- Code: `shaders/sun_off.wgsl`; `voxel_light` multiplies the sky term by `sun_factor(i, l.x)`;
  every module that includes `common.wgsl` appends `sun_off.wgsl` (`lib.rs`).
- Tests: `tests/naga.rs` (all modules on every backend, layouts unchanged); every gpu test
  unchanged on every tier.
- Level-1 measurement: `just perf-smoke surface` (expected within noise).
- Can land now, before the colored sky's lane C4 (sec. 6.1).

**Lane S2. Sun and cascades on the CPU** (`boxel_world::daylight`, `boxel_render`; protected:
render). S to M.
- Code: `SunPath` and the sun direction with its steps; `RealmLight::sun`; `SunShadowSettings`;
  `SceneSun` in the extract; `boxel_render/src/shadow/cascades.rs` (splits, sphere fit, the
  anchor and the snap, the extrusion, `k` per realm with its fade, the bucket mask).
- Tests (unit and proptest): a cascade contains its frustum slice for any camera orientation; a
  camera move moves the snapped box by whole texels only; cascades relative to the render origin
  are identical at shifts that are multiples of 1024, up to `MAX_BLOCK_COORD`; the sun rises at
  +X, its elevation equals `TimeOfDay::sun_elevation` at tilt 0, and it moves in steps; `k` is 0
  at night and for realms without the sun, and fades near the horizon; `RealmLight::sun` defaults.
- Level-1 measurement: none on the GPU (no GPU change); the cascades take microseconds per frame,
  checked with a unit timing, no bench.

**Lane S3. Shadow maps on High, no receivers yet** (`boxel_render`, `boxel_shaders`; protected:
render and shaders). L.
- Code: `FLAG_SKY_LIT` at upload (`chunk_table.rs`), `shadow_cull.wgsl`, `shadow.wgsl`,
  `boxel_render/src/shadow/pass.rs` (the depth array, the capped lists, pipelines with bias, bind
  groups, the encode before the main pass, the skips), `RenderStats::shadows`, a `stress
  --sun-shadows` flag.
- Tests: `tests/naga.rs` for the new modules; `gpu_sun_shadows.rs` part 1 on High: the shadow
  map read back over hand scenes holds, at chosen texels, the depth of the first back face a CPU
  ray finds (within the bias); glass, water and plants are not in it; a chunk with no sky-lit
  face and a chunk of another realm draw nothing (draw counts); the gate's missed-caster case
  keeps today's pixels; the main image is unchanged (no receiver yet). Mid and Low with the flag:
  no pass.
- Level-1 measurement: `headless_world` quads and chunks unchanged (no world change); `just
  perf-smoke --new-extra='--sun-shadows' rtx/surface/r16 rtx/surface/r32` (the passes alone),
  then the same on `780m/surface/r16`.

**Lane S4. Receivers on High** (`boxel_shaders`, `boxel_render`; protected: render and shaders).
M to L.
- Code: `sun_shadow.wgsl`, `VoxelPullShadowed`, group 3 and its layout on High, the pipeline
  switch, settings plumbing, `voxel_world --sun-shadows` and a key to toggle it.
- Tests: `gpu_sun_shadows.rs` part 2: every check of sec. 5 (never brighter, equal where nothing
  changes, the oracle, far shifts, the scenes); Mid and Low identical with shadows on.
- Level-1 measurement: `just perf-smoke --new-extra='--sun-shadows' rtx/surface/r16
  rtx/forest/r16`, then `rtx/surface/r32` and `780m/surface/r16`, and one more with
  `-- --time 17:00` (long shadows). Targets: on the RTX at most +0.25 ms on p50 and no new frame
  over 6.94 ms; on the 780M at most +0.8 ms on p50 (if the owner wants them on there, question 1).

**Lane S5. Cells and models** (`boxel_render`, `boxel_shaders`; protected: render and shaders). M.
- Code: depth-only shadow pipelines for model instances and cell quads, drawn into the cascades
  they touch; `ModelShadowed` (`model_shade` multiplies its sky term by `sun_factor`, with the
  instance's position and the shaded face's normal) and `CellShadowed`.
- Tests: a model in the sun casts a shadow where the oracle (its boxes as voxels) says; a model
  and a sign under a tree are darker than in the sun; never brighter; Mid and Low identical.
- Level-1 measurement: the models case of ADR 0040's measurement (50 walking figures) with and
  without shadows, on the RTX.

**Lane S6. Debug, demo and tools.** S.
- F3 line (`shadows: 3 cascades, 412 draws, sun 34 deg`), `snapshot` presets for the owner (noon,
  17:00, forest, house with a window, cave mouth, lake), a `tools/perf` case `shadows` (the
  surface flight at 15:00 with `--sun-shadows`) with its test in `perf_test.py`, so the control
  round tracks it.
- Tests: the overlay line, the presets build, the perf case parses.

**Lane S7, only if measurements ask.** Draw the far cascade every second frame, `Depth16Unorm`
or 1024^2 on integrated GPUs, keep cascades whose box, sun step and chunks did not change.

**Later, not in this plan.** Moon shadows; a color shadow map for stained glass; a heightmap
(Option C) beyond the cascades for mountain shadows at sunset; light shafts in the fog.

Order: S0 any time; S1 now; S2 in parallel with S1; S3 after S2; S4 after S1 and S3; S5 after
S4; S6 with S4 and S5.

### 6.1 Fit with the colored sky plan

The colored sky lane C4 edits `common.wgsl::voxel_light` (the tinted entry path), `VsOut::ao`,
`cell.wgsl`, `model.wgsl`, `CellVertex` and `ModelVertex`. This plan edits `voxel_light` once
(lane S1, one factor on the sky term), `model_shade` once (lane S5) and `ChunkGpu.flags` bit 31.

- **S1 lands first.** It is small and can land now; C4 waits for C1 to C3. C4 then rebases by
  keeping `* sun_factor(i, ...)` on its sky term in both the normal and the tinted paths (for the
  tinted path the level passed is the brightest sky channel). If C4 lands first instead, S1 puts
  the factor on both paths: one line each.
- **S5 after C4** if both are pending: C4 changes `ModelVertex` and `model_shade`; S5 then adds
  the factor to the new sky term.
- **No shared bits.** Sun shadows use no `VsOut` bit and no `View` field. `ChunkGpu.flags`: C4 has
  7..19, this plan takes 31.
- **One thing to fix in the colored sky plan**: its sec. 1 and 4.6 say `VsOut::ao` bits 16..31 are
  free and C4 sets bit 16 for tinted quads. ADR 0062 (dynamic lights, landed) has since taken bits
  16..31 (`AO_DYNAMIC_SHIFT`, 16 lights), so C4 needs another place for that bit (a new flat
  varying, or 15 dynamic lights).
- **Shared test helper**: sec. 5, last paragraph.

## 7. Risks

- **Cost on the 780M.** Fill and the lookup are per pixel; the estimate is +0.3 to 0.6 ms at 720p,
  more at 1080p. Mitigation: off by default on integrated GPUs (question 1), lane S7 levers.
- **Shimmer.** Sun steps and the 1024 anchor crossing move edges by under a texel; leaves'
  dapples may still crawl at a distance. Mitigation: the step size and PCF are tunable; the owner
  reviews moving captures.
- **Thin shapes.** Box faces down to 1/16 block, inset fluid faces and fences test the bias.
  Mitigation: back-face casting and the hand scenes of lanes S3 and S4.
- **Sunbeam sources under rock** look darker on High (question 8).
- **Missing casters** at a cave mouth (the gate), in a full list or a dropped chunk keep today's
  look. Counted in `RenderStats::shadows`.
- **The end of the shadows** at 160 blocks shows as a soft band on long, flat views. Mitigation:
  the fade, and Option C later if needed.
- **Pipeline switch** when shadows are turned on: a one-time stall of creating 3 to 7 pipelines.
- **Cross-GPU test drift** at shadow edges: the oracle masks the edges; the byte-exact checks are
  only where nothing can change.
- **Conflicts** with the colored sky lanes (sec. 6.1).

## 8. Size

S0 XS, S1 S, S2 S to M, S3 L, S4 M to L, S5 M, S6 S. Total L. Protected areas: render and
shaders. Not touched: the world, streaming (`streaming.rs`, `interest.rs`), the mesher, the light
crate, frame pacing, physics, `boxel_gfx`, `vendor/`.

## 9. Preguntas para el dueño

Cada una trae lo que recomiendo. Con un "sí" a todo, se construye como dice el plan.

1. **¿Sombras prendidas por defecto?** Recomiendo: el motor las trae apagadas y el juego las
   prende. En la 780M y otras GPU integradas, apagadas salvo que el jugador las pida.
2. **¿Qué tan oscura es la sombra?** Recomiendo: la luz del cielo baja un 40 % en la sombra. Lo
   que da el sol queda igual que hoy. Se ajusta después de ver capturas.
3. **¿Las paredes que no miran al sol también se oscurecen?** Recomiendo: sí, igual que una
   sombra, con un paso suave cuando el sol cruza la pared.
4. **¿El sol pasa justo por arriba al mediodía, como en Minecraft, o un poco inclinado?**
   Recomiendo: inclinado 30 grados. Así las sombras nunca caen derechas y ninguna pared queda de
   canto al sol todo el día.
5. **¿Hasta qué distancia hay sombras?** Recomiendo: 160 bloques, y se desvanecen al final. Más
   lejos cuesta más y casi no se nota.
6. **¿Qué da sombra?** Recomiendo: bloques, hojas (con sus huecos), entidades y carteles. El
   vidrio, el agua y las plantas chicas no. El vidrio de color no tiñe la sombra: el color ya lo
   pone la luz del cielo de color.
7. **¿Sombras de la luna de noche?** Recomiendo: no por ahora. De noche no se dibuja nada y no
   cuesta nada.
8. **Una fuente de cielo bajo roca, en un reino con sol, se ve con sombra en High** (un 40 % más
   oscura que hoy). **¿Está bien?** Recomiendo: sí. Un reino caverna con su propia curva no tiene
   sol y se ve igual que hoy.
