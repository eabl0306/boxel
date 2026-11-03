# Colored sky through stained glass: design plan

Follows ADR 0060 (issue #11, light plan P8). This is a design document: nothing here is built.
Paths are relative to the repo root. Numbers marked "measured" come from ADR 0015 and ADR 0060;
the rest are estimates that each lane's measurement checks.

**Status: approved (2026-11-02).** The owner's answers are in sec. 11. Lanes C0 to C2 are done:
ADR 0064, whose sec. 9 lists where C1 differs from this plan (the low 16 bits carry the uncolored
sky, decision 2; two `u16` arrays instead of one `u32`; separate colored walk and beam build). Sec. 1
and 4.6 are stale on one point: `VsOut::ao` bits 16..31 are the dynamic light mask of ADR 0062,
so C4 needs another place for the tint bit (ADR 0064 sec. 8). ADR 0064 sec. 10 lists what C2
added (tile landings also remesh where tints changed).

**Verdict.** Build Option D, "sparse colored sky": sky light gets three channels only in the
boxes and chunks where a filter actually changed its color, and the mesh carries it in a second
format of the same 8-byte light palette entry. The light rule is exact (the sky is filtered like
block light, per channel); worlds without colored glass stay byte-identical and pay nothing; no
GPU buffer, uniform, binding or vertex buffer grows on any tier. The one approximation is on the
GPU: a face lit both by tinted sky and by block light that both vary across its corners keeps one
of the two smooth and the other flat (sec. 4.5). If the owner wants that exact too, Option D'
(sec. 3.5) costs Low a third vertex buffer.

The three reasons ADR 0060 gave against colored sky, answered:

| Reason | Answer in Option D |
|---|---|
| More bits per voxel double `PaddedLight` (77 to 154 KiB per job), `ChunkLight`, the palette and the GPU format in all tiers | Only jobs whose padded chunk really has colored sky use the wide form. Every other job, chunk and palette entry keeps today's size. The GPU entry stays 8 bytes on every tier |
| Routing filtered sky into block light stops it following day and night | The tint stays in sky channels, which go through the sky color and daylight in the shader, as today |
| Darkening sky without color makes glass count for the sky tops | The sky top `T` does not change. The filters above `T` go in a separate list in the sky tile |

## 1. What exists today (checked against the code)

- `LightValue` is 16 bits: sky, red, green, blue, 4 bits each (`boxel_voxel::light`). Light is a
  pure function of the blocks plus one sky top `T(x, z)` per column, the highest voxel with
  opacity above 0 (`sky_tile::walk`). Colored glass has opacity 0, so it is never a top.
- ADR 0060 filters: `LightTables::light_filter` gives `[r, g, b]` in fifteenths. A block channel
  entering a filter voxel becomes `transmit(l - cost, f)`, a 16 by 16 table that scales the
  brightness on the curve `l / (60 - 3l)`. `LightMasks::filter` marks filter voxels, built only
  for chunks whose palette holds a filter state. The sky flood never reads it.
- `boxel_light::compute` lights the padded chunk (34^3) from a 64^3 box. The box light is one
  `u16` per voxel (512 KiB); `LightScratch` is about 0.7 MiB per worker (measured). Direct sky is
  a row mask from the 9 tiles; one sky flood, then one flood per block channel. Fast paths:
  `FastDark` (0.04 us measured) and `FastOpen`.
- Outputs: `PaddedLight` (`Uniform` or 34^3 `u16`, 77 KiB) and `ChunkLight` (`Uniform` 0 B,
  `SkyOnly` 16 KiB, `Full` 64 KiB), kept within `light_cache_radius` (8) for gameplay.
- Mesh (`boxel_mesh`): corner light is the rounded per-channel mean of up to 4 front cells
  (`shading::face_corner_lights`, 4 lanes of 16 bits in a `u64`). A chunk palette holds up to 4096
  entries of `[u32; 2]` (4 corners of 16 bits); quads index it with 12 bits. Overflow falls back to
  `LightQuantization::Block3`, then `Flat`. The merge key is `[corners: u64, material | ao |
  surface: 25 bits]`, so 39 bits of the second word are free.
- GPU: High and Mid read the entry from the quad arena at `ChunkGpu::light_base + index`. Low gets
  it expanded per quad in a second instance-step buffer (`Uint32x2`), reserved at the arena's size
  (64 MiB by default). `common.wgsl::voxel_light` computes `max(curve(sky) * view.sky_light.rgb,
  curve(rgb) * gain, ambient)`. Cells (`CellVertex`, 40 B) and models (`ModelVertex`, 32 B) carry
  one 16-bit `LightValue` each.
- Free room: `ChunkGpu.flags` bits 7..31 (the cull reads bit 0 and bits 1..6, masked),
  `ChunkImmediate.origin.w` on Low, `VsOut::ao` bits 16..31 (bits 12..15 go to the realm index of
  the sky sources plan).
- **A bug found while reading:** `WorldJobs::apply_edits` widens the remesh set by the light reach
  only when opacity or emission change (`streaming.rs`, `light_reach`). Placing colored glass in
  air, or swapping red glass for blue, changes neither, so only the up to 8 chunks whose padded
  region holds the voxel are relit. Block light that crosses the glass into a farther chunk stays
  stale until something else remeshes it. Lane C2 fixes it (a filter change counts like an
  opacity change); it can also land alone before this plan.

## 2. The rule

What "correct" means here: **sky light is filtered exactly as ADR 0060 filters block light, per
channel.**

- Sky light has three channels `S_r, S_g, S_b` (levels 0 to 15).
- **Spread sky**: a channel entering a filter voxel becomes `transmit(l - cost, f)`, the ADR 0060
  rule. Other voxels cost `max(1, opacity)` as today.
- **Direct sky**: a voxel above `T` is direct, as today. Its level per channel is 15 passed
  through every filter at or above it in its column, with no cost (direct sky never falls with
  depth): `D_c(y) = transmit(... transmit(15, f_1) ..., f_n)` over the filters `y <= y_i`, all of
  them above `T` (a filter above `T` has opacity 0 by definition of `T`).
- A direct voxel with `D_c = 15` is closed to the flood of channel `c`, as today. A tinted direct
  voxel (`D_c < 15`) is a source at `D_c` and stays open: a brighter neighbor can raise it (the
  edge of a glass roof gets white light from the open column next to it).
- Channels mix by max, as today. `T` is unchanged, so the walk, the floor, the fast paths and the
  sky sources plan keep their meaning.

Why it stays exact and unique: `transmit` never raises a level and is monotone (ADR 0060), and
tinted direct voxels are sources of at most 15. Every light of 1 or more in the padded chunk
still comes from a source at most 14 voxels away, inside the box; the tints of the direct voxels
come from the column, which the tile computes from content (sec. 4.1). The fixpoint is unique and
the highest-first floods still pop each voxel at its final level.

Why spread sky must be tinted: **a stained-glass window in a wall is the main use**, and it
passes no direct sky at all (direct sky is vertical). A room with a roof is lit only by spread
sky through its windows. A design that tints only direct sky shows nothing there.

What it gives (the game's dye colors, computed with the table):

| Glass | Filter | Direct sky under it | Spread sky right behind it (14 enters) |
|---|---|---|---|
| Red | `[10, 3, 2]` | `(13, 7, 6)` | `(12, 6, 5)` |
| Light blue | `[3, 11, 13]` | `(7, 14, 14)` | `(6, 13, 13)` |
| Black | `[2, 2, 2]` | `(6, 6, 6)` | `(5, 5, 5)` |
| Red then light blue | | `(5, 6, 5)` | |
| Clear glass | none | `(15, 15, 15)`, as today | as today |

Gray and black glass darken the sky with equal channels: that needs no wide format (sec. 4.3).

## 3. The options

Each option is costed on the same items. "Job" is one light box (a mesh or light job); "worker"
is a mesh thread's scratch.

### 3.1 Option A: full RGB sky in every voxel

`LightValue` grows to 24 bits (`u32`): three sky and three block channels everywhere.

- **Memory per job**: `PaddedLight::Full` 77 to 154 KiB for every lit job; the box array 512 KiB
  to 1 MiB per worker (`LightScratch` about 1.2 MiB).
- **Memory per chunk**: `ChunkLight::Full` 64 to 128 KiB; a sky-only form needs 12 bits per voxel
  (48 KiB instead of 16). Palette entries 8 to 16 B (12 used), so the palette part of the arena
  doubles in every chunk.
- **Light job time**: wider arrays and extract in every lit box, estimated +10 to 25 % on
  `box_surface` without any glass; three sky floods where filters are.
- **Mesh**: corner light over 6 lanes (two `u64` spreads) in every chunk; merge key 96 + 25 bits.
- **GPU**: High and Mid read two arena slots per entry and carry a `vec4<u32>` light varying. Low's
  light buffer goes from `Uint32x2` to `Uint32x4`: its fixed VRAM goes from 64 to 128 MiB (96 MiB
  with `Uint32x3`). Same shader on all tiers, so pixels stay identical.
- **Day and night**: yes (sky channels times the sky color).
- **Far coordinates**: same as today (only the column tints are absolute, sec. 4.9).
- **Without colored glass**: same pixels, but not free: every job, chunk, palette and Low pay.
- **Risk**: XL. Every protected area, a new mesh format, a new gpu baseline, a fixed 64 MiB more
  on Low (the 780M and weaker GPUs).

### 3.2 Option B: sky level plus a tint index

One sky level per voxel plus a 4-bit index into a small tint palette (per chunk or global).

- **Memory per job**: +19 KiB (a nibble per padded voxel, 96 KiB in all); per worker about
  +128 KiB.
- **Memory per chunk**: `ChunkLight` +16 KiB; palette entry +2 B (4 corner indices), which does
  not fit 8 B, so 16 B in the arena; plus a tint table per chunk on the GPU.
- **Light job time**: about today's: one flood carrying (level, tint).
- **GPU**: High and Mid read the tint table from the arena; Low has no storage, so it needs the
  tint colors expanded per quad (4 corners of 12 bits): Low's buffer grows as in A.
- **Day and night**: yes.
- **Correctness: no.** A voxel reached by red sky at 14 from one side and white sky at 12 from the
  other is `(14, 12, 12)` under the rule; one level and one tint cannot say that. Whichever path
  wins, the color jumps where the winner changes, and a quad cannot interpolate between two tints
  at its corners. ADR 0060 rejected it for the same reason.
- **Risk**: L, for a rule that is not unique-by-construction and shows seams. Rejected.

### 3.3 Option C: tint carried only by direct sky (column based)

The sky tile keeps, per column, the filters above `T`; direct voxels take the column's tint;
spread sky is either white (C1) or mixes (C2).

- **C2 (diffuse mixes)** needs per-channel spread sky: it is Option D.
- **C1 (diffuse white)**:
  - **Memory per job**: none per voxel; a 34 x 34 column tint table (about 1 KiB).
  - **Memory per chunk**: the tile's filter list (sec. 4.1); `ChunkLight` unchanged.
  - **Light job time**: about today's (a column scan).
  - **Mesh and GPU**: a face's sky still needs a color per corner or per quad, and neither the quad
    nor today's entry has a free bit. So it needs the same new entry format as D (sec. 4.5).
  - **Correctness: no.** Windows (the main case, sec. 2) get nothing; under a glass roof the
    direct cells are red and the spread cells right next to them are white, so roof edges and
    overhangs glow white.
  - **Risk**: M to L (the GPU half of D) for half of the effect. Rejected.

### 3.4 Option D: sparse colored sky (recommended)

The rule of sec. 2, computed exactly, stored wide only where the color shows (sec. 4).

- **Memory per job**: 77 KiB as today; 154 KiB (`PaddedLight::Colored`) only in jobs whose padded
  chunk has some voxel with unequal sky channels. Per worker +256 to 512 KiB for the two extra sky
  channels of the box, allocated on the first colored box.
- **Memory per chunk**: `ChunkLight::Colored` 128 KiB only for colored chunks inside the r8 cache.
  GPU: 0 B (entries stay 8 B; a colored chunk may have somewhat more distinct entries and quads).
  Sky tile: +8 B per tile, plus about 16 B per filter voxel above `T` (a full glass roof over a
  chunk column: about 16 KiB, against the tile's 8 KiB).
- **Light job time**: 0 without glass (one branch). A colored box runs three sky floods that price
  filter voxels, instead of one: estimated 2 to 3x `box_surface` (46.4 us measured), so 100 to
  150 us, only for boxes near sky-lit glass.
- **Mesh**: unchanged code and output for normal chunks. A colored chunk computes corner light
  twice (sky RGB lanes and today's lanes), packs tinted entries, and renumbers its palette (one
  pass over its quads). Estimated +20 to 40 % mesh time in those chunks only.
- **GPU per tier**: no buffer, binding, uniform or vertex buffer change. High and Mid read the
  count of tinted entries from `ChunkGpu.flags` bits 7..19; Low from `ChunkImmediate.origin.w`.
  One bit in `VsOut::ao` picks the decode; one branch in `voxel_light`. `CellVertex` 40 to 44 B and
  `ModelVertex` 32 to 36 B (sec. 4.7). Same bits and same code on every tier: identical pixels.
- **Day and night**: yes, sky channels times the (per-realm) sky color.
- **Far coordinates**: exact; column tints are compared relative to the box in `i64` (sec. 4.9).
- **Without colored glass**: byte-identical meshes, chunk table and pixels; one branch per box,
  per walk layer and per fragment.
- **Risk**: L. Protected areas: light, world edits, mesh, render and shaders. The flattened faces
  (sec. 4.5) are the one visible compromise.

### 3.5 Option D': D with exact tinted faces

Tinted entries take 12 B instead of 8, so no face is ever flattened.

- High and Mid: a tinted entry takes two arena slots (or a side section after the palette).
- Low: the 4 extra bytes per quad need a third instance-step buffer (stride 4), reserved like the
  light buffer: +32 MiB fixed VRAM, or allocated per page on the first colored chunk with a second
  Low pipeline. `boxel_gfx::tier::floors::VERTEX_BUFFERS` goes from 2 to 3 (`boxel_gfx`,
  protected; GL ES 3.0 adapters have the slots, but the policy tests change).
- Everything else as D. Size L+ (adds the Low buffer, its upload and the Low draw loop).

### 3.6 Summary

| | A: RGB everywhere | B: tint index | C1: direct only | **D: sparse (rec.)** | D': sparse, exact GPU |
|---|---|---|---|---|---|
| `PaddedLight` per job | 154 KiB always | 96 KiB always | 77 KiB | 77; 154 only colored | as D |
| Scratch per worker | +512 KiB | +128 KiB | 0 | +256 to 512 KiB, lazy | as D |
| `ChunkLight` (r8) | 128 / 48 KiB | +16 KiB | as today | 128 KiB only colored | as D |
| Light job time | +10 to 25 % every lit box (est.) | about today | about today | 0, or 2 to 3x in colored boxes | as D |
| Palette entry | 16 B everywhere | 16 B + tint table | needs D's format | 8 B everywhere | 8 B, 16 B tinted |
| Low fixed VRAM | 64 to 128 MiB | 64 to 128 MiB | as D | unchanged | +32 MiB, or per page |
| Exact light | yes | no | no (no windows) | yes | yes |
| Exact on GPU | yes | no | no | all but flattened faces | yes |
| No glass: free and byte-identical | no | no | yes | yes | yes |
| Size | XL | L | M to L | L | L+ |

## 4. Option D in detail

### 4.1 Sky tiles: the filters above the top

- `SkyTile` gains `tints: Option<Box<TileTints>>`: per column, the filter voxels above `T`,
  highest first, as `(y: i64, filter: [u8; 3])`. Layout like the sky sources plan's beam tile:
  `index: [u16; 1025]` offsets and one `Vec` of entries. `None` in every tile of a world without
  sky-lit glass. Bounds `tint_max` (highest filter y) for the box gate.
- `sky_tile::walk` ORs nothing new into the blocker test. Per layer it reads `masks.filter` only if
  it is not empty (a chunk whose palette has no filter state has an empty mask) and records, per
  row, `filter & open` bits: filters met while their column is still open are above `T`.
- `SkyTile::raise` drops the entries at or below the new top. `rescan_column` collects the filters
  it passes on its way down to the new top. New `SkyTile::set_filter(x, z, y, Option<[u8; 3]>)`
  for an edit above `T`. Each keeps the property "an edited tile equals a fresh walk".
- The walk start `S` must cover edited filters too: `SkyColumn::edit_top` also rises for a placed
  filter voxel, or a glass block above every blocker would be missed.

### 4.2 The light box

- **Gate.** `tables.any_filter()`, the box is `Sky::Mixed`, and either some of the 27 chunks has a
  non-empty filter mask or some of the 9 tiles has `tint_max >= box bottom`. Otherwise the box runs
  today's code, unchanged.
- **`FastOpen`** additionally needs every `tint_max` of the 9 tiles below the padded chunk's
  bottom (a padded chunk under a glass roof is direct but not white).
- **Direct rows.** `build_direct` keeps the white direct rows. For each box column with a tint
  crossing the box, it walks the column's entries once and writes the tinted direct voxels: their
  bit is cleared from the per-channel closed rows where `D_c < 15` and they are queued as seeds at
  `D_c`.
- **Floods.** Three sky floods, one per channel (red in today's sky nibble, green and blue in a
  second box array made on the first colored box), each pricing the filter voxels one by one with
  `Special::Filter`, as the block floods already do. Block floods unchanged.
- **Extract.** If any padded voxel has unequal sky channels, `PaddedLight::Colored`; otherwise
  today's `Full` or `Uniform` with the common sky level. So a gray or black glass roof, which only
  darkens, costs a colored box but no wide output.
- **Later, if measured slow:** skip the floods of channels that no filter in the box reduces.

### 4.3 Outputs: `PaddedLight` and `ChunkLight`

- `PaddedLight::Colored(Box<[u32; PADDED_VOLUME]>)`, 154 KiB. Low 16 bits: today's `LightValue`
  with the sky nibble set to the **brightest** sky channel; bits 16..27: sky red, green, blue.
  Every existing reader of the low 16 bits (gameplay, ticks of ADR 0059, F3, the light reach of
  edits) keeps working and reads the brightest channel, which is what ADR 0059 already asks for.
- `ChunkLight::Colored(Box<[u32; CHUNK_VOLUME]>)`, 128 KiB, same layout. `ChunkLight::get` keeps
  returning `LightValue`; new `ChunkLight::sky_rgb` and `ChunkMap::sky_rgb_at`.
- A padded result whose sky channels are all equal never becomes `Colored`, so a world whose
  registry has filters but whose glass is all in the dark, or gray, keeps today's forms.

### 4.4 World: edits and the walk

- **A filter placed, removed or changed above `T`**: `set_filter` on the tile, then the band from
  14 under `T` to 14 above the filter in its column is remeshed (`band_chunks`), in the same atomic
  batch. That is the same bound as moving a top from `T` to that height today.
- **A filter placed, removed or changed at or below `T`**: the light reach rule, with the fix of
  sec. 1 (a filter change counts like an opacity change, so `r` is the highest level around,
  sky included, at most 14).
- A tile not `Exact` yet: version bump, as today.
- Load order: tints come from content through the walk, like `T`, so streaming never relights
  anything and any load order gives the same light.
- `Reference` computes the same tints and colored light from scratch.

### 4.5 Mesh: the tinted palette entry

The palette entry stays `[u32; 2]`. A colored chunk has two kinds of entries:

- **Normal (N)**: today's 4 corners of `LightValue`. Used when, at all 4 corners, the three sky
  channels are equal (the common value goes in the sky nibble).
- **Tinted (T)**: 4 corners of one kind of light (3 x 4 bits, 48 bits), the other kind flat for
  the face (3 x 4 bits, the rounded per-channel mean of its corners, as `Flat` rounds), and a tag
  bit for which kind keeps its corners. Laid out so no field crosses the two words. 3 bits spare.
  - Tag 0: sky corners, block flat. Tag 1: block corners, sky flat.
  - The mesher keeps the corners of the kind whose corners differ more (sum over channels of max
    minus min), ties to sky. **The entry is exact whenever one kind is flat across the face**:
    every face with no block light, every face with no colored sky, every face where a lamp's
    light is even across it. Only a face with both a tint gradient and a lamp gradient loses the
    smoothness of one of them, and only inside that face.
- **Order**: T entries first, then N entries. `ChunkMesh::tinted_entries` (`t`, 0 to 4096) says
  where the N entries start. The mesher fills two palettes (T and N) and, only if `t > 0`, adds `t`
  to the light index of the N quads in one pass. A chunk with `t = 0` is byte-identical to today.
- **Corner light**: the colored path reads the `u32` cells and runs `face_corner_lights` twice,
  once on today's 16-bit view and once on the sky RGB view, with the same rounding per channel.
  The merge key holds the 4 x 24 bits of corners in its two words (96 + 25 bits fit in 128).
- **Quantization**: `Block3` applies to the block channels of both kinds; at `Flat`, a colored chunk
  drops its tint (sky = brightest channel, N entries only), so the 1024-entry guarantee holds. It
  is counted, like the existing fallbacks.
- **AO elision**: a face is dark when all six channels are 0 at its corners.
- Fluid faces use the same palette path. LOD meshes have no light and do not change.

### 4.6 GPU per tier

- **High and Mid** (`voxel_pull.wgsl`): the vertex shader already reads `ChunkGpu`; `t` is
  `(flags >> 7) & 0x1FFF`. A quad whose light index is below `t` sets `VsOut::ao` bit 16. The High
  cull reads `flags & 1` and the wall bits only, so it is unaffected.
- **Low** (`voxel_instanced.wgsl`): `t` comes in `ChunkImmediate.origin.w`, today always 0; the
  draw loop already pushes the immediates once per chunk. The light attribute stays `Uint32x2`;
  the CPU expansion copies entries as they are.
- **Fragment** (`common.wgsl`, shared by all tiers): `voxel_light` keeps today's code for N quads.
  For T quads: decode the tag, interpolate the corner kind bilinearly, take the flat kind as is,
  `sky = vec3(curve(S_r), curve(S_g), curve(S_b)) * sky color`, `block = vec3(curve(r), curve(g),
  curve(b)) * gain`, then the same max with the ambient floor. Two more `level_curve` calls on
  T quads only.
- **Low's one uniform block**: untouched. `View` does not grow.
- **Pixel identity**: every tier decodes the same 8 bytes with the same function, so tinted images
  are identical on High, Mid and Low; normal quads run today's code, so every existing image is
  unchanged.
- **Cost on every world**: one bit test per fragment (the branch is flat per quad). Expected within
  noise; lane C4 measures it with the smoke.

### 4.7 Cells and models

- Cells (ADR 0050) are lit by their cell's light, flat. `LitCellQuad::light` becomes the 32-bit
  colored value; `CellVertex` gains one `u32` (40 to 44 B) with the sky RGB; `cell.wgsl` builds a
  T entry (tag 0, equal corners) when the sky channels differ. Cells are few per chunk.
- Models (ADR 0040) are lit by the instance's cell: `ModelVertex` gains a `u32` with the sky RGB
  (32 to 36 B), and `model.wgsl` takes the T path when it is set.
- Both are their own vertex buffers on every tier, so Low's floors do not change.

### 4.8 Day, night and realms

Tinted sky is sky light: the shader multiplies each sky channel's brightness by the frame's sky
color times daylight, so a red-lit room goes dark at night like the open sky, and its lamps keep
working through the block channels. With the sky sources plan's per-realm table (its sec. 3), the
same product uses `realm_env[realm].sky`; nothing else changes.

### 4.9 Far coordinates (ADR 0014)

- Tile entries keep `y` as `i64`, like tops; the box compares them with saturating `i64`
  differences relative to its bottom, as `build_direct` does with `T`. Beam tints use the beam
  tile's 16-bit offsets (sec. 5).
- The GPU side reads only counts and indices, no positions.
- Tests run the same neighborhood at 0, +-2^31, +-2^40 and `MAX_BLOCK_COORD - 64`.

### 4.10 Worlds without colored glass

- Registry without filters: `any_filter` is false and every new path is one skipped branch.
- Registry with filters but none placed (the game today), or glass only in the dark: the walk
  sees empty filter masks, no tile has tints, no box takes the colored path, no padded result is
  colored. Meshes, palettes, `ChunkGpu` (bits 7..19 stay 0), Low's immediates (w stays 0) and
  pixels are byte-identical. Lane C2 and C3 test it against `main`.

## 5. Colored beams (sky sources plan)

The sky sources plan's beams are direct sky cells (its sec. 2.1). A colored beam follows the
same rule as a colored column:

- A beam passes through opacity-0 cells, so it passes through colored glass, as it does today.
- The beam starts at 15 per channel at its source. After each filter cell in its direction (down
  or up), each channel becomes `transmit(level, f)`, with no cost. Beam cells are then tinted
  direct sources exactly as tinted column cells are, and mix by per-channel max with the column's
  direct sky (a beam under a glass roof can be whiter than the roof's tint, or the other way).
- **Beam tile**: `beam_tile::build` already traces each beam with a bit scan of its z-row; it also
  scans the filter rows over `[lo, hi]` and records the filter cells of tinted beams (a flag in
  `Beam::flags` and a side list, about 8 B per filter cell). Untinted beams are unchanged.
- **Box**: `build_direct` ORs untinted beams into the white direct rows as the sky sources plan
  says, and writes tinted beam cells like tinted column cells (sec. 4.2).
- **Edits**: a filter placed, removed or changed inside a beam retints it in place; the beam part
  past the filter, plus or minus 14, is remeshed. Same bound as cutting a beam.
- **Sources themselves** cannot carry a filter: `set_sky_source` refuses a state with one
  (`RegistryError::InvalidSkySource`), so a beam always starts white.
- **Gate**: unchanged. A beam tile is needed only where sources are; tints add nothing to that test.

Lane fit: colored beams are a small part of lane C1 (light) and C2 (world), written on top of the
sky sources plan's lanes 2 and 3. Sec. 7 orders the lanes so they never edit the same functions
at the same time.

## 6. The game side

- Nothing new to register: the 16 stained glass blocks and panes already have their ADR 0060
  filters, and the sky follows them.
- Gameplay that reads sky light (`ChunkMap::light_at`, ticks of ADR 0059) reads the brightest
  sky channel: under red glass a crop sees 13 instead of 15, under black glass 6. Minecraft's
  glass does not darken anything; this is the engine's choice, to confirm (sec. 10).
- The game must not give water or leaves a filter unless it wants every ocean or forest to take
  the colored path (sec. 8).

## 7. Build order, in lanes

The sky sources plan is being built now and owns `box_light.rs::build_direct`, `beam_tile.rs`,
`sky.rs` and the realm uniform. To avoid two lanes in the same functions:

- **C0** (docs) any time.
- **C1** (light) starts after sky sources lane 2 merges.
- **C2** (world) after sky sources lane 3 and C1.
- **C3** (mesh) after C1 (it needs `PaddedLight::Colored`); it may run in parallel with C2.
- **C4** (render and shaders) after sky sources lane 4 (both edit `voxel_light` and `VsOut::ao`)
  and C3.
- **C5** (debug, demo, tools) after C4.

Until C4 lands, a `WorldSettings::colored_sky` switch (default off) keeps C1 to C3 dark in the
world: with it off, no box takes the colored path. C4 turns it on by default if the owner agrees
(sec. 10). Each lane gets the owner's OK for its protected area.

**Lane C0. ADR (docs only).** The next free ADR number (0061 goes to the sky sources), in
Spanish, from this plan; an amendment to ADR 0060 ("El cielo queda blanco" becomes "el cielo se tiñe", with the
alternatives costed here) and to ADR 0015 sec. 4 to 6 and 8. XS.

**Lane C1. Colored sky in `boxel_light`** (protected: light). M.
- Code: `SkyTile::tints`, the walk, `raise`, `rescan_column`, `set_filter`; the box gate,
  `classify`, tinted `build_direct`, per-channel sky floods, colored `extract`;
  `PaddedLight::Colored`, `ChunkLight::Colored`; oracle with per-channel sky; tinted beams in
  `beam_tile.rs`.
- Tests:
  - `tests/oracle.rs`: random worlds with filters above and below `T`, partial filters, heavy
    opacities, emitters, sky sources through glass: every inner chunk equals the oracle bit for
    bit, with and without fast paths.
  - Hand scenes: under a red roof `(13, 7, 6)`; red then light blue `(5, 6, 5)`; black glass gives
    a non-colored result; a light blue window lights a closed room; the edge of a glass roof gets
    white 14 from the open column; clear glass keeps today's light exactly.
  - `tests/sky_walk.rs`: tints equal a brute-force scan; `raise`, `rescan_column` and `set_filter`
    equal a fresh walk.
  - `tests/far_coords.rs`: filters and glass roofs at 0, +-2^31, +-2^40, `MAX_BLOCK_COORD - 64`.
  - A registry without filters, and one with filters but none placed: results equal to `main`'s
    bit for bit.
- Level-1 measurement: criterion `light`, one run per side: `box_cave_dark`, `box_surface`,
  `box_surface_cold_masks_filters` unchanged within noise; new `light/box_glass_roof` (a surface
  box under a red glass roof, target 150 us or less) and `light/box_window` (a room lit through a
  colored window), each with its clear-glass twin; `sky/walk_loaded` unchanged and a new
  `sky/walk_loaded_glass`.

**Lane C2. World** (`boxel_world`; protected: light, streaming edits). S to M.
- Code: the light reach fix (filter changes), `set_filter` in the edit path and its band,
  `edit_top` for filters, tinted beam edits, `Reference`, `ChunkMap::sky_rgb_at`, `WorldStats`
  counters (`colored_light_boxes`, `colored_padded`, `colored_chunk_light`).
- Tests (`src/tests/light.rs`, `streaming/tests/sky.rs`, `tests/integration.rs`):
  - A world without filters, and one with filters registered but none placed: zero colored boxes,
    meshes byte-identical to `main`.
  - Edits compared with `Reference`: glass placed and removed above `T` and below it; red swapped
    for blue (the bug of sec. 1, which fails on `main`); a glass block above every blocker
    (`edit_top`); an opaque block placed over a glass roof and removed again; glass in a beam.
  - Shuffled load orders and 1 vs 6 threads: identical mesh hashes in a world with a glass house.
  - Far coordinates: the same world at x = 2^40 and y = -2^40 gives identical bits.
- Level-1 measurement: `headless_world --view-distance 16` overworld and forest: same chunks,
  meshes, quads and palette entries as `main`. Then the demo glass scene (lane C5's, or a test
  generator) with the colored counters.

**Lane C3. Mesh** (`boxel_mesh`; protected by being on the render path). M.
- Code: the colored corner path, T packing and tag choice, two palettes and the renumbering,
  `ChunkMesh::tinted_entries`, `corner_light` decode, quantization for T, AO elision,
  `reference_face_shading` with colored light, `LitCellQuad` colored light.
- Tests: T pack and unpack round trip for every tag; a face with one flat kind is exact; equal
  channels give N; the renumbering keeps every merge (quads equal to a reference built with one
  palette); `Flat` drops the tint and fits; normal chunks byte-identical (existing regression
  tests); proptest of colored faces against `reference_face_shading`.
- Level-1 measurement: criterion `gather_mesh/surface` and `gather_mesh/cave_lit` unchanged; new
  `gather_mesh/glass_roof` against its clear-glass twin (target +40 % or less).

**Lane C4. Render and shaders** (`boxel_render`, `boxel_shaders`; protected: render and shaders).
M.
- Code: `t` into `ChunkGpu.flags` (chunk table) and `ChunkImmediate.origin.w` (Low draw loop);
  `voxel_pull.wgsl`, `voxel_instanced.wgsl`, `common.wgsl` (T decode, `voxel_light`), `cell.wgsl`,
  `model.wgsl`; `CellVertex` and `ModelVertex` plus their attribute offsets; the model poser.
- Tests:
  - `tests/naga.rs`: `View` unchanged, Low still one uniform block and two vertex buffers;
    `CellVertex` 44 B and `ModelVertex` 36 B.
  - `gpu_terrain` and every existing gpu view: identical pixels (no filters in those scenes).
  - `gpu_light_filter` extended: at noon the floor under a red glass roof is red and none of it is
    white; at midnight the same floor follows the night sky, not lit; a light blue window tints a
    closed room; a face with a lamp and tinted sky renders the flattened entry the same everywhere;
    a sign (cell) and a model under the roof are tinted; identical pixels on High, Mid and Low of
    the 780M, the RTX 5050, llvmpipe, and Low on GL of the RTX, and at every `FAR_SHIFTS`.
  - Run: `BOXEL_REQUIRE_GPU=1 tools/quiet.sh cargo test --workspace --locked --test '*gpu*' --
    --test-threads=1`.
- Level-1 measurement: `just perf-smoke surface forest` (the per-fragment branch on worlds without
  glass, expected within noise); then `glass-house` from lane C5.

**Lane C5. Debug, demo and tools.** S.
- F3: the sky RGB at the eye when it is colored. `boxel_demo` gets a glass scene (a greenhouse with
  a red roof, a room with a light blue window, a black skylight, a lamp under the roof) and
  `voxel_world --stained-glass` starts there. `headless_world` prints a `colored:` line (colored
  boxes, colored meshes, T entries, flattened entries). `tools/perf` gets a `glass-house` case with
  its test.
- Tests: the scene builds on the ground; the overlay shows the sky RGB inside the greenhouse.

## 8. Risks

- **Flattened faces.** A face lit by a lamp and by tinted sky, both varying, shows one of them flat
  across the face (a step of 1 or 2 levels at face edges, like Minecraft without smooth light).
  Mitigation: the tag keeps the kind that varies more; `headless_world` counts them in the demo
  scene; Option D' removes it at Low's cost.
- **Big glass roofs.** A dome or a glass ceiling high above the ground makes every box between the
  roof and the ground (plus 16) colored: 2 to 3x light time in those boxes, and colored chunks in
  the r8 cache at 128 KiB. Bounded by what players build; measured by the `glass-house` case.
- **Filters on common blocks.** If a game gives water or leaves a filter, every ocean or forest
  box takes the colored path. The engine allows it; the cost is the game's choice (sec. 10).
- **The per-fragment branch** costs every world one bit test. If the smoke shows a cost, the
  T decode moves behind a pipeline constant set only when some chunk has `t > 0`.
- **Lane conflicts** with the sky sources plan, in `build_direct`, `beam_tile.rs`, `sky.rs`,
  `voxel_light` and `VsOut::ao`. The order of sec. 7 avoids them.
- **Palette pressure.** Colored chunks have more distinct entries; a chunk that overflows to `Flat`
  loses its tint. Counted.
- **Gameplay change.** Sky levels under colored glass are lower than today (13 under red, 6 under
  black). Ticks and spawning that read sky light see it.

## 9. Size

C0 XS, C1 M, C2 S to M, C3 M, C4 M, C5 S. Total L, against XL for Option A. Protected areas:
light, world edits, mesh, render, shaders. Not touched: frame pacing, `boxel_gfx`, physics,
streaming order (`interest.rs`), `vendor/`.

## 10. Questions for the owner

1. **Option D with flattened faces, or D' exact** at the cost of a third vertex buffer on Low
   (+32 MiB fixed, or per page with a second Low pipeline)?
2. **Darker sky under colored glass for gameplay.** Crops and spawning read the brightest sky
   channel: 13 under red, 6 under black glass. Keep it, or give gameplay the uncolored level?
3. **On by default** once lane C4 lands, or a world setting the game turns on?
4. **One filter for both lights**, or a separate opt-out so a game can filter block light through
   a block (water, say) without tinting the sky there (and without the cost in every ocean)?
5. **The edit bug of sec. 1** (filter changes do not widen the remesh): fix it now on its own, or
   with lane C2?

## 11. Decisions of the owner (2026-11-02)

1. **Option D on Low and Mid, exact faces on High only.** Low and Mid keep the flattened faces
   of sec. 4.5. High draws tinted faces exactly (D' or an equivalent that needs no Low buffer).
   This is a High-only difference, like sun shadows (#9): the tier pixel tests compare Low and
   Mid with each other, and High with its own reference on scenes with tinted faces. Scenes
   without tinted faces stay identical on all three tiers.
2. **Gameplay reads the uncolored sky.** Crops (and spawning, later) see the sky level the
   column would have without the tint, so greenhouses of any stained glass work as in
   Minecraft. Only the picture is tinted.
3. **Off by default.** The engine ships `colored_sky` off; the game turns it on.
4. **One filter for both lights.** No per-block sky opt-out. The game puts no filter on water.
5. **The edit bug of sec. 1 is fixed now**, on its own, before lane C2.

