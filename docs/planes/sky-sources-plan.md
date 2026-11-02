# Sky sources and per-realm ambient: design plan

Issue #140 (EW-SKYSRC), which replaces #130 (EW-SKYUNDER). Owner decision of 2026-11-02. This is a
design document: nothing here is built. Paths are relative to the repo root. Numbers marked
"measured" come from ADR 0015; the rest are estimates to check in each lane's measurement.

**Status: approved by the owner (2026-11-02).** The owner's answers to the open questions are
recorded as decisions in sec. 9, and per-realm face shading was added to Option A (sec. 3 and
lane 4).

**Lanes done:** 0 (ADR 0061, and the amendments to ADR 0015 and ADR 0039) and 1 (source states
in `boxel_voxel`). Lane 1 keeps the source list beside the masks (`ChunkData::sky_sources`,
`OwnedLightMasks::sky_sources`) instead of in the `LightMasks` view: in the view it cost 8 % on
`masks/cave_1bit`, and only the beam tiles read it.

**Verdict.** Build Option A: sky sources write the existing sky channel, and each realm gets its
own sky color, intensity, daylight curve, ambient color and face shading, applied in the shader
per realm. This is what the
owner's decision implies, it needs no new light channel and no save format change, and it costs
nothing in a world without sources. Option B (a zone with its own curve inside a realm) is not
needed by the decision; if it is ever needed, its cheapest form is "constant sources" that write
the block light channels (S to M on top of A), not a third channel (XL).

## 1. What exists today (checked against the code)

- Light is a pure function of the blocks plus one sky top `T(x, z)` per column (ADR 0015 sec. 1
  and 4). `T` is the highest voxel with opacity above 0 in the world content, found by
  `boxel_light::sky_tile::walk` from the generator's top (`GeneratorTop::of(vertical_chunks)`)
  down to the world floor `walk_floor = top - sky_walk_limit` (1024). Streaming never moves it.
- `boxel_light::compute` lights the padded chunk (local `-1..=32`) from a 64^3 box (chunk plus 16
  per side) and the 27 chunks the mesh job already holds. Direct sky is a set of z-row masks
  (`build_direct`, from the 9 tiles); seeds are the open non-direct voxels next to a direct one;
  one bucket-queue flood per channel. Fast paths: `FastDark` (no emitter, box under every top of
  its 9 tiles) and `FastOpen` (no emitter, padded chunk above every top). `SkyInput::BelowFloor`
  skips the tiles when the box is under the floor of a world whose columns never reach the void.
- `boxel_world::sky` keeps one `SkyColumn` per loaded chunk column (`Pending`, `InFlight`,
  `Exact(Arc<SkyTile>)`), walked lazily by `JobKind::SkyTile` jobs (`WalkJob`), which read
  loaded snapshots, saved chunks (`SavedIndex::layers`) and private generations. Edits move tops
  in place (`SkyTile::raise`, `rescan_column`) or send the column back to `Pending`; the remesh
  set is the column band plus or minus 14 (`sky_edit`, `changed_band`, `sky_band`), all in one
  atomic edit batch.
- The shaders compute `max(curve(sky) * view.sky_light.rgb, curve(rgb) * gain, ambient)`
  (`common.wgsl::voxel_light`; the same formula in `model.wgsl::model_shade` and
  `lod_common.wgsl`). `view.sky_light` is one vec4 for the whole frame: `DaylightCurve::at
  (TimeOfDay)` sampled in `boxel_render::plugin::scene_light`. So there is one global sky color
  times daylight for every voxel of the world.
- Realms (ADR 0039 sec. 2) are y-slabs of a dimension with planes on multiples of 4096 (chunk
  aligned: a chunk is in exactly one realm), at most 8 planes (`MAX_REALM_PLANES`, so at most 9
  realms), found by `RealmStack::realm_of(y)`. `RealmSpec` has `sky: RealmSky` (`Open`, `Under`,
  `Closed`) and `env: GenId`, but nothing in the light reads them yet, and only `RealmForm::Ground`
  is built. The world crate does not read realms at all today (only `persist/format.rs` saves
  their spans).
- Free room we can use: `ChunkImmediate.origin.w` on Low, `ChunkGpu.flags` bits 7..31,
  `VsOut::ao` bits 12..31, `ModelVertex.data` bits 24..31. `ChunkGpu` has no free word (64 B,
  the last pad became `page`). `ViewUniform` is 352 B plus the 8 KiB box table (the naga test
  asserts `352 + 8192`); GL ES 3.0 guarantees a 16 KiB uniform block.
- Per-cell data (`BlockData`, ADR 0048) cannot live on air ("el aire nunca tiene datos"), and a
  data change does not bump the chunk version or remesh.

## 2. The model

### 2.1 What a source is

A sky source is a cell that emits sky light at level 15 along a straight beam in its column:

- **Direction**: down (default) or up.
- **Range** `R`, from 1 to `MAX_BEAM` (engine constant, 256 voxels, 8 chunk layers; decided,
  sec. 9). `R` counts the source cell itself.
- **Beam**: the source cell and the next cells in its direction, while each cell does not block
  sky (opacity 0, the same test as `LightTables::blocks_sky` uses for `T`), at most `R` cells,
  and never past the plane of the source's realm. Formally, for a down source at `(x, y, z)`:
  `beam = { (x, y', z) : lo <= y' <= y }`, where `lo` is the largest of `y - R + 1`, the realm's
  bottom plane, and `b + 1` with `b` the highest sky blocker below `y` (if any). Up is the mirror.
- **Beam cells are direct sky**: level 15 in the sky channel, exactly like a voxel above `T`.
  They seed the sky flood like any direct voxel (neighbors at `15 - cost`) and are closed to it.
- **Spread**: from the beam, light spreads as normal sky light: minus `max(1, opacity)` per
  cell, stopped by opacity 15. Walls and roofs shadow it exactly like the open sky, because the
  flood is the same code.
- **Semi-opaque blocks** (leaves, water: opacity 1 to 14) end the beam, as they end direct sky
  at `T`: under a leaf the light is spread light (14, then 13, ...), the same as under a tree in
  the open. This is the reading of "reduce it as they reduce sky light": a source above leaves
  looks exactly like the sky above leaves.
- **Sources do not move `T`** (they have opacity 0), and a beam above `T` adds nothing (its cells
  are already direct). Sources never touch `SkyTile`.
- **Mixing**: sources and the open sky share the sky channel, so a voxel lit by both takes the
  max, as all channels do today. The result is still a unique fixpoint, independent of order,
  thread count and position.

Why a beam and not a point emitter: a block-light emitter falls by 1 per cell from the start, so
15 levels is all it ever reaches. A beam carries 15 down a whole cavern height and spreads from
every cell of it, which is what makes a cavern floor look sunlit, with real shadows under
overhangs: an overhang ends the beams of the columns over it, and the cells under it get only
spread light.

What it gives, per use:

- **A whole cavern lit like daylight**: one source in the first air cell under the roof, in every
  roof column (or every second column: beams at 15 with spread between them never drop under 14),
  pointing down, with `R` at least the cavern height. The roof's underside faces are lit by the
  source cells (15), the floor by the beam ends, overhangs cast sky shadows.
- **A single sunbeam through a cave roof**: one source under the roof, down, `R` the cave height.
  A lit column of 15 with a soft halo of up to 14 cells.
- **The lit underside of a ceiling realm**: a source in the first air cell under the underside in
  every column, down, `R` 32 to 64. The underside and the inverted mountains read lit; their
  flanks are lit by the neighbor columns' beams and by spread. This replaces #130's "sky from
  below": no mirrored walk, no `Under` rule.

### 2.2 Storage: a block state (chosen)

Three options were weighed: a block state, per-cell data (`BlockData`), or a list of points per
chunk.

**Chosen: a block state** with an engine flag. The game registers a block (for example
`game:sky_source`, with states for direction and a few ranges) and marks it:

```rust
pub struct SkySourceDef { pub dir: BeamDir, pub range: u16 } // BeamDir::Down | BeamDir::Up
BlockRegistryBuilder::set_sky_source(state: BlockState, def: SkySourceDef) -> Result<(), RegistryError>
```

Validation (`RegistryError::InvalidSkySource`): `RenderShape::None`, light opacity 0, emission 0,
no collision, `1 <= range <= MAX_BEAM`. The engine never names the block (`tools/no_content.py`);
tests use `test:sky_source`. The game calls `set_keeps_fluids_out` on it, so water does not
wash a generated source away (decided, sec. 9).

Why:

- **Everything else is free.** Edits (`EditQueue::set`), `BlockChanged`, atomic edit batches,
  the chunk version bump that invalidates in-flight meshes, saving and loading (names through the
  id map), streaming, private generation and the generator writing chunks all already work for
  blocks. No new edit kind, no new message, no record format.
- **Uniform-air shortcuts stay correct by construction.** A chunk holding a source is no longer
  uniform air, so every "uniform air means nothing here" path (`is_uniform_empty`, the walk's
  uniform fast path, `known_air`) keeps working without a special case. A point list on an air
  chunk would have to be checked in each of those.
- **A cheap "may hold a source" test exists**: a palette scan like `ChunkData::may_emit`, or the
  existing `ChunkData::may_contain(&StateSet)`.
- **Survival cannot reach it**: shape `None` and no collision make it invisible, non-solid and
  not targeted by picking, like air. Only the game's creative item places and removes it (sec. 6).

Costs of this choice (accepted):

- It takes a cell. A source cannot share a cell with water or leaves (it can share with air
  only). For ambience that is fine: sources go in the first air cell under a roof.
- A cave chunk of 1 bit (air and stone) that gets a source becomes a 2-bit chunk: voxel storage
  4 to 8 KiB, and its `LightMasks` must be built once (`OwnedLightMasks`, about 16 us measured
  for `masks/surface_packed`, plus 4 KiB). Only the chunks that hold sources pay, usually one
  layer of chunks under a roof.

Rejected:

- **Per-cell data**: cannot live on air, and a data change neither bumps the version nor remeshes
  (ADR 0048). It would still need a carrier block, so it is a block state with extra steps.
- **A point list per chunk** (`ChunkData.sky_sources`): no cell taken and 1-bit chunks stay 1-bit,
  but it needs a new edit kind, a new message, a chunk record field (record version 3), and a
  check in every uniform-air shortcut. More code in protected areas for a benefit (sharing a cell
  with water) nobody asked for.

### 2.3 Beam tiles: how a box learns about beams

A beam is not local: a source up to `MAX_BEAM - 1` cells above a box can light it, and the beam
can be ended by a blocker in a chunk the mesh job does not hold. This is the same problem the sky
top solves, so it gets the same answer: a per-column structure computed from content, never from
what is loaded.

**`BeamTile`** (new, `crates/boxel_light/src/beam_tile.rs`), one per chunk column, exact for a
window of voxels `[w_lo, w_hi]`:

- `sources`: every source of the column that can reach the window (a down source with
  `w_lo <= y <= w_hi + R - 1`, an up source with `w_lo - R + 1 <= y <= w_hi`), each with its full,
  unclipped beam `[lo, hi]`. Kept per (x, z) column, sorted by `lo`.
- Layout: `index: Box<[u16; 1025]>` (offsets per column) and `beams: Vec<Beam>`, with
  `Beam { y: u16, lo: u16, hi: u16, flags: u16 }` as offsets from `w_lo - MAX_BEAM`. A window is
  at most the loaded vertical extent plus `2 * MAX_BEAM` (about 1 100 voxels at a vertical radius
  of 8), so 16-bit offsets are exact at any height (ADR 0014). 8 B per source plus 2 KiB.
- `version`, like `SkyTile`.

**`beam_tile::build(window, column, realm planes, tables, layer)`** walks the column from
`w_hi + MAX_BEAM - 1` down to `w_lo - MAX_BEAM + 1`, layer by layer, with the same
`LayerData` callback as `sky_tile::walk` (loaded snapshot, saved chunk, private generation, or
known air). Per layer it reads the source list and the blocker rows (`opaque | partial`) from the
chunk's `LightMasks`; per source it traces the beam with a bit scan in its z-row (at most `R`
cells). A layer that is `Air` or `AirUntil` has no source and no blocker.

**The light box** (`boxel_light::box_light`): `LightInput` gains `beams: [Option<&BeamTile>; 9]`,
by `tile_index`, where `None` means "no beam can reach this column's part of the box".
`build_direct` ORs, for each of the 64x64 box columns, the beams that cross the box's y range
into the direct rows, after the sky tops. With `SkyInput::BelowFloor` the direct rows start
empty and only get beams. `classify`: if any beam crosses the box, `Sky::Dark` becomes
`Sky::Mixed` (no `FastDark`); `FastOpen` is unchanged (the padded chunk is already all 15). The
exactness argument of the box is unchanged: beam cells are level-15 sources like direct sky, so
every light of 1 or more in the padded chunk comes from a direct or beam cell at most 14 away,
inside the box. The beam set itself is exact because the tile is built from content.

`ChunkLight`, `PaddedLight` and the mesher do not change: they see sky levels, as today.

### 2.4 Who needs a beam tile (the zero-cost gate)

Almost no chunk is near a source, so the world must know, without generating anything, when a
mesh can skip beams. A mesh of chunk `(cx, cy, cz)` needs the beam tile of column `c` (one of its
9) only if some layer of `c` in `cy - 1 - K ..= cy + 1 + K` (with `K = ceil(MAX_BEAM / 32)`, 8)
may hold a source:

1. **Generated sources**: new `WorldGenerator::sky_source_layers(&self) -> RangeInclusive<i64>`
   (chunk layers where `generate` may place a source state), default empty. A generator that
   places sources declares a band (for example the roof band of its cavern realm). With WG4
   (#98) this moves into `ColumnBounds` as a per-(realm, column) field `sources`, which tightens
   the gate to the columns that really have them.
2. **Loaded chunks with sources**: `SourceColumn::layers` lists the loaded layers of the column
   whose snapshot may hold a source (palette scan, updated when data arrives and on edits).
3. **Saved chunks not loaded**: any layer of `SavedIndex::layers(dim, x, z)` in the range. This is
   conservative: the beam job reads those records and usually finds nothing. If measurements
   show many false positives (a heavily built world), a later step adds an engine metadata key
   `boxel:sky_source_chunks` (the saved chunk keys that hold sources), written by the saver,
   with no `FORMAT_VERSION` change.

If none applies, the mesh passes `None` for that column: no job, no wait, no extra work. A world
without sources (every world today) never builds a beam tile, never waits, and must give meshes
byte-identical to today (a regression test of lane 3).

### 2.5 World integration (streaming and load order)

- **`SourceColumn`**, next to `SkyColumn` in `SkyMap`, with the same lifecycle (created with the
  column's first slot, dropped with its last): `state` (`Pending`, `InFlight { version }`,
  `Exact(Arc<BeamTile>)`), `version`, `layers` (loaded layers with sources), `waiters`,
  `cancel`. Created lazily, only when the gate says a mesh needs it.
- **`JobKind::BeamTile`** (`JobId::Column`), with the same cap and priority rules as sky walks.
  A `BeamJob` captures the loaded layers of its range on the main thread, reads saved layers
  from the store and generates the rest privately (counted in `WorldStats::beam_private_generations`),
  exactly as `WalkJob` does. Before starting it waits for loaded layers whose data is still
  generating (the `waiting_for` rule of walks), so it rarely generates.
- **Window**: a mesh needing rows `[a, b]` (its box, `origin - 16 ..= origin + 47`) of a column
  whose tile does not cover them sends the column to `Pending` with the union window. Vertical
  movement therefore re-runs a few jobs, each cheap (loaded layers are bit scans; private
  generation happens only at the edges of the loaded set).
- **`start_mesh`** waits for the needed beam tiles as it waits for sky tiles (`waiting_beams`),
  then passes them to the job.
- **Load order**: a beam tile depends only on the content of its column range (generator, saved
  edits, loaded edits), never on which chunks happen to be loaded, because unloaded layers are
  generated or read from the store. So a chunk that arrives never relights anything, and any
  load order gives the same light (lane 3 tests this with shuffled orders).
- **A beam crossing chunks and layers**: handled by the tile, which spans `2K + 1` layers around
  its window. A source in the top loaded layer that beams down through 6 layers, or a source
  2 layers above the loaded set (not loaded) that beams into it, are both found.
- **Sources in unloaded chunks**: found through (1) and (3) of the gate and read or generated by
  the job. With `NullChunkStore`, a creative source placed in a chunk that later unloads is lost
  with the chunk's edits; its column goes back to `Pending` and the beam disappears, the same as
  an edited sky top today (ADR 0015 sec. 7, "Descarga").
- **Realm planes**: the job takes `generator.realms().planes()` and clips each beam at the plane
  of its source's realm. Planes are chunk-aligned, so the clip is exact and cheap.

### 2.6 Edits

Per changed voxel `p`, inside `apply_block_edits`, in the same atomic batch (`epoch` bumped,
priority 0):

- **A source placed or removed** at `p` (old or new state is a source): its beam is traced in the
  column's tile (in place, from loaded snapshots, at most `R` cells). The changed beam, plus or
  minus 14, gives the remesh set through `band_chunks`, as a moved sky top does.
- **A sky blocker placed inside a beam** (`lo <= p.y <= hi` of some beam of its column): the beam
  is cut; the cut part plus or minus 14 is remeshed.
- **A sky blocker removed where a beam ended** (`p.y = lo - 1` of a down beam, `hi + 1` of an up
  beam): the beam grows down to the next blocker or its range; the new part plus or minus 14 is
  remeshed.
- **Any other edit**: no beam work (beams ended by range never grow).
- If tracing needs a layer that is not loaded (a source at the edge of the vertical radius), or
  the edit is outside the tile's window but within reach of it, the column goes to `Pending`, its
  job joins the edit batch (as a walk does today), and the new tile is diffed against the old one
  to remesh what changed (`changed_band` for beams).
- The existing rules (faces and AO up to 8 chunks, opacity and emission radius, sky tops) are
  unchanged and still apply.
- A column with no tile (the gate said nothing can reach it) and an edit that places the first
  source: the gate becomes true for the chunks around it; their meshes are queued in the batch
  and wait for the new tile.

Fan-out is bounded: a beam is at most 256 cells, so one edit remeshes at most a band of
`256 + 2 * 15` cells in 3x3 columns (about 3 x 3 x 10 = 90 chunks in the worst case, usually
3 x 3 x 2). That bound is why the cap stays at 256 (decided, sec. 9).

### 2.7 The range cap, and why there is one

- It bounds the non-locality: a beam tile reads at most `K = 8` layers beyond its window, and an
  edit's band is at most 256 cells.
- It keeps sources local ambience: a source cannot light a 2 km column by accident.
- It covers the uses: caverns of 100 to 200 cells, cave sunbeams, a ceiling underside lit from
  32 to 64 cells.
- Each state carries its own `R`; the game registers a few (for example 16, 32, 64, 128, 256).

### 2.8 Far LOD

The far LOD does not need to know about sources. LOD faces are lit as open sky (ADR 0031
sec. 12) and the LOD draws only the drawn realm's surface band (ADR 0039 sec. 10). Sources light
caverns, cave interiors and the ceiling realm, none of which the LOD draws (the ceiling is `lod
Off`, and the game turns the LOD off inside a cavern realm). The only LOD change is per-realm
ambient and face shading: LOD fragments use the entry of the drawn realm (sec. 3).

## 3. Per-realm ambient and face shading

The owner's rule: a source takes the curve of the realm it is in; a cavern that is its own realm
has its own curve (for example constant, with no night); a zone in the surface realm darkens at
night with the sky. So each realm needs its own sky color, intensity and daylight curve, applied
to the voxels of that realm.

- **Which realm lights a fragment**: the realm of the voxel in front of its face (the voxel whose
  light the face shows). A top face on the last layer under a plane shows the light of the cell
  above the plane, so it takes the upper realm's curve. For spread light that crosses a plane
  (at most 14 cells), the receiving realm's curve applies, as for the open sky today.
- **Curves**: a `DaylightCurve` per realm, sampled with the one global `TimeOfDay`. A constant
  curve is already expressible (day, dusk and night keys equal), so no new curve type.
- **Engine data** (boxel_world): `RealmLighting { curves: Vec<Option<DaylightCurve>> }` by realm
  index of the current dimension's `RealmStack` (`None` falls back to the global
  `DaylightCurve`), and `RealmPlanes` (the stack's planes), published by the world at open. The
  game fills `RealmLighting` from its environment table (ADR 0039 sec. 16). Fog, clear color and
  the LOD switch stay per camera, in the game, as ADR 0039 sec. 16 already plans.
- **Ambient as a color**: today the ambient floor is one scalar (0.02) for every channel. The
  realm entry makes it an RGB color, so a realm can have a warm or green-gray floor (see the
  dimension types below). In a one-realm world all three channels hold today's 0.02.
- **Face shading per realm**: today `common.wgsl::face_shade` is fixed: top 1.0, bottom 0.5,
  x faces 0.8, z faces 0.65. Minecraft 26.3 shades faces per dimension (`cardinal_light` in the
  dimension type; `the_nether.json` has `cardinal_light: "nether"`, where bottom faces are almost
  as bright as top ones). The realm entry gets a table of 6 factors, one per face in the mesher's
  face order (+Y, -Y, +X, -X, +Z, -Z). The default table is today's values, so nothing changes
  until a realm sets its own; a "flat" table (all 1.0, or close to it) gives the Nether look.
- **Uniform** (`ViewUniform`, `View`): `realm_planes: array<vec4<i32>, 2>` (8 planes relative to
  `render_origin.y`, clamped to the i32 range; unused planes `i32::MAX`) and `realm_env:
  array<RealmEnv, 9>`, with `RealmEnv` 3 vec4: `sky` (rgb = sky color times daylight, w = shade
  of +Y), `ambient` (rgb = ambient color, w = shade of -Y) and `shade` (x, y, z, w = shade of +X,
  -X, +Z, -Z). 2 + 27 = 29 vec4, 464 B: View goes from 352 + 8192 to 816 + 8192 B (9 008 B),
  still one uniform block on Low and well under GL ES 3.0's guaranteed 16 KiB.
- **Shader**: `finish_vertex` computes the front voxel's relative y from the quad's own voxel and
  its face normal (integers, before the corner growth), counts the planes at or under it (8
  integer compares), and packs the realm index in `VsOut::ao` bits 12..15 (no new varying).
  `voxel_light` reads `view.realm_env[realm]` instead of `view.sky_light`, and `face_shade`
  takes the realm and reads its factor from the same entry instead of the fixed switch. The
  same in `cell.wgsl` (realm from the cell origin; cells already go through `shade_fragment`)
  and `lod_common.wgsl` (the drawn realm's entry). `model.wgsl` reads the light from the realm
  entry (realm from the instance position, packed in `ModelVertex.data` bits 24..27); a model's
  face shade is already baked per vertex on the CPU (`ModelVertex.data`, `model::SHADE_SHIFT`),
  so the CPU poser takes it from the instance's realm table and the shader does not change for
  it.
  The relative compare is exact for every chunk less than 2^31 from the camera (ADR 0014).
- **Why planes in the uniform and not a per-chunk index**: no change to `ChunkGpu`, the Low
  immediates, `CellVertex` or the arena; the front-voxel rule falls out for free; and the
  shader cannot disagree with `RealmStack::realm_of`.
- **Pixel identity**: in a one-realm world every plane is `i32::MAX`, every fragment takes realm
  0, `realm_env[0].sky` and `.ambient` hold the same floats as today's `sky_light` (with the
  ambient copied to all three channels), and the shade factors are the same f32 values as the
  literals of today's `face_shade` (1.0, 0.5, 0.8, 0.65, written from Rust as `f32`, which round
  to the same bits as the WGSL literals). The products are taken in the same order, so every
  image is the same bit for bit on every tier. Lane 4 tests it on the existing gpu views.
- **The model for a realm environment** (what the Minecraft 26.3 jar's dimension types hold; the
  game's environment table, ADR 0039 sec. 16, should be able to express all of it):

  | Dimension | `has_skylight` | `ambient_light` | `ambient_light_color` | Other |
  |---|---|---|---|---|
  | Nether | false | 0.1 | #302821 | fog start 10, end 96; `sky_light_factor` 0; `cardinal_light` "nether" |
  | End | true | 0.25 | #3f473f | `sky_light_color` #ac60cd; `sky_light_factor` 0 |
  | Overworld | (true) | 0 | #0a0a0a | |

  How each maps here: `has_skylight` false is a `Closed` realm sky (after #129) with no sun in
  its curve; `ambient_light` and `ambient_light_color` become the realm's ambient color (the
  game converts the sRGB color to linear and scales it); `sky_light_color` is the tint of the
  realm's `DaylightCurve`; `sky_light_factor` 0 reads as a sky light that does not follow the
  day (a constant curve); fog start and end stay the game's per-camera environment swap;
  `cardinal_light` is the 6-factor shade table. The exact formulas of the jar are the game's to
  copy; the engine only needs the fields.
- **CPU gameplay**: `ChunkMap::light_at` keeps returning levels. A helper
  `RealmLighting::effective(level, realm, time)` gives the brightness for game rules that care
  (spawning, plant growth), optional.

## 4. Why not tile the sky with sources

The owner asked whether sources could also fix extreme heights, by tiling the sky with sources
instead of starting the walk at the world's top, and whether that costs about the same as today.
The coordinator's answer is right on the conclusion, with one correction about load order.

- **No.** The open sky stays implicit: one sky top per column, walked once per chunk column
  (`sky_tile::walk`).
- **The extreme-height problem is the walk's start and floor**, both global today: the start is
  the generator's top voxel (`GeneratorTop::of(vertical_chunks)`) and the floor is that top minus
  1024 (`walk_floor`). A 12 km spire puts the floor near 11 264 and darkens every lowland column
  (ADR 0039 sec. 15). #129 makes both per column (start from column bounds, floor from
  `ground_lo`), so walks get the same length or shorter.
- **Sources cannot replace it, for three reasons:**
  - A down beam starts at its source: the air above a source is not lit. Tiling the sky would
    need a source at the top of every column, and the world goes up forever.
  - The range cap (256) means a source lights at most 256 cells below it. To reach the ground,
    each column's source would have to sit within 256 cells over that column's ground, which
    means knowing where the ground is: that is `T`, the thing the walk already computes.
  - Without a cap, one edit could remesh a column band kilometers tall, and every beam tile
    would read kilometers of layers.

Numbers, per column of the world (one x, z):

| | Implicit sky (today, and #129) | Sky tiled with sources |
|---|---|---|
| Memory | 8 B (one `i64` top; `SkyTile` is 8 KiB per 1024 columns, measured 7.4 MiB at r16, 27.7 MiB at r32) | about 16 to 20 B: 8 B in a `BeamTile`, 2 B of tile index, plus about 8 B in the chunk that holds it (a uniform air chunk becomes a 1-bit chunk, 4 KiB, with built masks and a source list, 4 KiB more, per 1024 sources) |
| Chunks that must exist | none above the generator's top: `LayerData::AirUntil` skips known air in one step | the chunk holding each source, which turns uniform air (0 B, no job) into a real chunk to generate, save and load |
| Walk cost | one downward bit scan per column; measured 18 us per 1024 columns from loaded data, 128 us when generating | one bit scan of up to `R` cells per source, the same order per column, plus a beam job per chunk column and up to 8 extra layers read or generated around each window |
| Fast paths | `FastOpen` covers boxes above every top | gone for those boxes: every box near the beams' ends takes the full flood |
| Load order | independent: `T` is defined over content | independent too in this design, but only because the beam job generates or reads every chunk holding a source; for a tiled sky that is every column's sky chunk, all the time. Skipping those generations is what would bring the load-order dependence back |

The coordinator's line "each one costs about one sky column's walk" holds for the trace (one bit
scan per column), but not for memory: a source costs about twice a sky top, and the chunk that
holds it stops being free air. Sources are for where the sky cannot reach.

## 5. The two options

### 5.1 Option A: per-realm ambient only (recommended)

Sources write the sky channel (sec. 2); each realm's curve and color apply in the shader (sec. 3).
This is what the owner's decision implies: a cavern that is its own realm gets its own curve (its
`RealmLighting` entry); a lit zone inside the surface realm uses the surface realm's curve and
darkens at night with the sky.

**Crates and files:**

- `crates/boxel_voxel`: `registry.rs` (`SkySourceDef`, `BeamDir`, `set_sky_source`,
  `RegistryError::InvalidSkySource`, `MAX_BEAM`), `light.rs` (`LightTables::sky_source`, a class
  bit, `any_sky_source`), `chunk.rs` and the masks (`LightMasks::sky_sources`, built into
  `OwnedLightMasks` like emitters; the 1-bit borrowed view refuses a palette with a source, as it
  does with an emitter), `ChunkData::may_source`.
- `crates/boxel_light`: new `beam_tile.rs`; `box_light.rs` (`LightInput::beams`,
  `build_direct`, `classify`); `oracle.rs` (sources and brute-force beams); `lib.rs` exports.
- `crates/boxel_worldgen`: `lib.rs` (`WorldGenerator::sky_source_layers`, default empty).
- `crates/boxel_world`: `sky.rs` (`SourceColumn`, `BeamJob`, the gate), `streaming.rs`
  (`start_mesh` waits, `start_beam`, `on_beamed`, `beam_edit` in `apply_block_edits`, unload),
  `scheduler.rs` (`JobKind::BeamTile`), `map.rs` (`WorldStats` counters), `reference.rs` (beams
  from scratch), `daylight.rs` (`RealmLighting`), `lifecycle` (publishes `RealmPlanes` at open).
- `crates/boxel_render`: `plugin.rs` (extract samples one curve per realm), `light.rs`
  (`SceneLight` gains the realm table), the uniform writer.
- `crates/boxel_shaders`: `common.wgsl` (`View`, `RealmEnv`, `finish_vertex`, `voxel_light`,
  `face_shade`), `model.wgsl`, `cell.wgsl`, `lod_common.wgsl`; `src/gpu.rs` (`ViewUniform`,
  `ModelVertex` realm bits); `tests/naga.rs` (sizes).
- The model poser (`boxel_render`, where `ModelVertex.data` gets its shade): the shade factor
  from the instance's realm.
- `crates/boxel_debug`: F3 lines and source markers (sec. 6).
- `crates/boxel_demo`, `examples/`: a demo source block and a lit cavern (sec. 6).

**Protected areas touched:** light (`boxel_light`, `boxel_world/src/sky.rs`), streaming
(`streaming.rs`: mesh start, edits, unload; not `interest.rs`), render and shaders (the realm
table and the realm lookup). Not touched: frame pacing, physics, `boxel_gfx`, `vendor/`.

**Memory per chunk:**

- A chunk with no source in reach: 0 B. A world without sources: 0 B everywhere.
- A chunk that holds sources: one palette entry; a 1-bit cave chunk becomes 2-bit (+4 KiB of
  voxels) and builds its masks (+4 KiB, +4 KiB more if partial voxels, + 4 B per source).
- A chunk column with a beam tile: 2 KiB plus 8 B per source in reach. A full ceiling (1024
  sources) is about 10 KiB per column: about 11 MiB at r16 and 42 MiB at r32 if the whole view is
  under such a ceiling (a cavern realm), against 7.4 and 27.7 MiB of sky tiles measured. A source
  every second column divides it by 4.
- `ChunkLight` (gameplay cache, r8): a lit cavern chunk is `SkyOnly` (16 KiB) instead of
  `Uniform` (0 B), like a surface chunk.

**Meshing and light propagation:**

- No mesher change: the light palette format is the same.
- A chunk whose box gets beams takes the full flood instead of `FastDark`: about the surface box,
  56.5 us measured (`light/box_surface`), instead of 0.04 us, plus the beam gather (estimated 5 to
  20 us for a full ceiling). Boxes with no beam in reach are unchanged.
- Quads: lit faces lose dark-face AO elision, so a lit cavern meshes like a forest. Measured
  bounds (ADR 0015, P3, r16): underground at y = -1000 is 8.41 M quads with elision and 14.67 M
  with AO on every face; a view fully inside a lit cavern realm approaches the second number. A
  sunbeam or a small lit zone adds a few chunks' worth. This is the main real cost; the lane's
  `headless_world` run measures it.
- Beam jobs: one per chunk column with a source in reach, re-run on vertical moves; estimated
  20 to 60 us from loaded data, plus 22 to 60 us per private generation at the edges.

**Shader cost per tier:**

- High, Mid, Low run the same `common.wgsl`: per vertex, about 8 integer compares and a pack;
  per fragment, one dynamically indexed uniform vec4 (Low already indexes `light_curve`
  dynamically, so GL ES accepts it). Estimated well under the 2.8 % the edge growth cost.
- Face shading: today's switch on the face becomes one more read of the same realm entry, by
  face (the index is flat per quad). About the same cost as the switch on every tier; on Low it
  is a dynamically indexed member of the one uniform block, as `light_curve` already is.
- Low keeps one uniform block (View grows by 464 B, to 9 008 B) and no new binding or attribute.
- Pixel identity: unchanged images in every one-realm world (sec. 3); new two-realm images must be
  identical on High, Mid and Low and at `FAR_SHIFTS`.

**Save format:** none. Sources are block states saved by name. `RealmLighting` is game data, not
saved. Beam tiles are not saved (like sky tops). An old engine reading a world with source states
treats them as any unknown block.

**Size:** L (S voxel flag, M `boxel_light`, M to L world integration, M per-realm ambient and
shading,
S debug and demo).

**Risks:**

- A fully lit cavern realm costs quads like a forest (up to 1.74x underground) and palettes like
  the surface. Mitigation: sparse source grids; measure in lane 3.
- Beam jobs at the vertical edges of the view generate privately (like sky walks at
  y = -1000, which measured 2.7 private generations per column). The gate keeps this to columns
  with sources in reach.
- The conservative saved-chunk gate may run useless jobs in heavily built worlds (sec. 2.4, 3).
- Edits under a 256-cell beam can remesh up to about 90 chunks in one batch; the batch hold
  (16 ms, 2 frames) may split such a batch.
- The realm lookup per vertex must use the face's front voxel; getting it wrong shows a seam
  one layer under each plane. A gpu test covers it.
- The shade factors must reach the shader as exactly today's f32 values, or every image drifts by
  one level in places. The existing gpu views catch it.

### 5.2 Option B: a zone with its own curve inside one realm

Example: a lit cavern inside the surface realm that does not darken at night. Option A cannot do
this: the zone's light is in the sky channel, and the shader applies the surface realm's curve.
Option B needs one more engine piece. Three ways to build it:

| Piece | What it is | Size | Verdict |
|---|---|---|---|
| **Constant sources** (recommended form of B) | A source flag `light: Sky \| Block([u8; 3])`: a `Block` source writes its beam into the RGB block channels at its color, not into the sky channel. Block light ignores the clock, so the zone stays lit at night, tinted by its levels | S to M on top of A | Cheapest; gives "constant", not an arbitrary curve |
| **A third light channel** | A second sky-like channel (4 more bits per value) with its own curve per realm | XL | Doubles light memory, the palette entry and Low's fixed 64 MiB buffer |
| **A per-zone tag** | A curve selector per quad (3 of the 4 free `geo` flag bits) or per chunk | M to L | Ill-defined where a zone and the sky mix (one cell, two curves), seams at quad and chunk edges, more quads |

**Constant sources, full cost (on top of A):**

- Files: `boxel_voxel` (`SkySourceDef::light`), `boxel_light` (`box_light.rs`: per-channel
  "block direct" rows for constant beams, closed to the RGB floods and seeding them at the
  frontier, as the sky flood is seeded; `beam_tile.rs` keeps the kind in `Beam::flags`;
  `oracle.rs`). No render, shader, world or streaming change beyond A (edits treat both kinds
  the same).
- Protected areas: light only.
- Memory: none per chunk beyond A. `ChunkLight` of a constant-lit chunk is `Full` (64 KiB)
  instead of `SkyOnly` (16 KiB), inside the r8 cache only.
- Light propagation: a constant-lit box runs up to 3 block floods (one per non-zero channel)
  seeded at the beam frontier, instead of 1 sky flood: estimated 3x the sky flood, about 120 to
  170 us per box (the RGB lamp box measured 326 us with 36 000 pops; a frontier-seeded flood
  pops far fewer). Palettes grow (RGB corner variety), so some quads split.
- Shader: none. Images of every world without constant sources are identical by construction.
- Save format: none (more source states).
- Size: S to M.
- Risks: color is coded in light levels, so it is coarse and shifts with distance (red 8 dies
  after 8 cells, blue 15 after 15), as colored lamps do today; the zone shares `block_gain` with
  lamps; a lamp inside a bright constant zone is washed out (per-channel max), as it is under the
  noon sky today; a zone can only be constant, never a second day cycle.

**Third channel, cost (for the record):** `LightValue` from 16 to 20 bits (u32): `PaddedLight`,
the 512 KiB box array and `ChunkLight::Full` double; the palette entry grows from 8 B (4 x 16
bits) to 16 B, so palettes take two arena entries each and Low's per-quad light buffer goes from
`Uint32x2` to `Uint32x4` (its fixed 64 MiB VRAM becomes 128 MiB); a fourth flood; `VsOut::light`
widens; the curve table doubles. Every protected area, a changed mesh format and a new gpu
baseline. XL, for one extra curve per realm.

**What the owner's decision already implies:** Option A. "A cavern that is its own realm gets its
own curve" is the realm table; "a zone in the surface realm darkens at night" is exactly what
sources in the sky channel do. Option B would add zones that ignore their realm's night, which
the decision does not ask for. Recommendation: build A, keep "constant sources" as the B path if
the game ever needs it.

## 6. The game side

**Worldgen places sources** (game code, in the game's generator):

- A decoration step after carving, per column of a cavern's roof: the first air cell under the
  roof gets a down source with `R` at least the cavern height (capped at 256). Every column for
  an even daylight look, every second column for half the tile memory. Columns under the roof of
  an overhang get the overhang's own source above it, so the overhang shadows the floor below.
- A single sunbeam: a feature places one source under a roof crack, with `R` the cave height.
- The ceiling realm (V4): the step that builds the underside puts a source in the first air cell
  under it in every column, down, `R` 32 to 64.
- The generator declares where it may place them: `sky_source_layers()` today, `ColumnBounds::
  sources` after WG4. A source outside the declared band is a bug the debug span assert
  (ADR 0039 sec. 2.8) should catch; lane 3 adds that check.
- Sources are deterministic content like any block: same seed, same sources.

**The creative item:**

- One item per registered source state (for example "sky source, down, 64"), only in the
  creative inventory, never dropped, never crafted.
- Placing: the game's normal placement puts the state in the targeted air cell.
- Removing: while the item is held, the game's pick ray also targets source states (they are not
  targetable otherwise, since they have no shape or collision); breaking sets the cell to air.
- While the item is held, the game turns on the source markers (below), as Minecraft shows light
  blocks only when holding one.

**Seeing and debugging:**

- F3 overlay (`boxel_debug`): at the crosshair, "sky source: down, range 64" when the target cell
  is a source; at the eye, "beam: yes/no" and the realm with its curve ("realm game:nether,
  curve constant"). The light line already shows the sky level.
- Source markers (`boxel_debug::SkySourceMarkers`, off by default): for sources within 48 cells
  of the camera, read from the `BeamTile`s in `SkyMap`, a small overlay rect at each source's
  projected position and a dotted line of rects along its beam, colored by direction. Uses the
  existing overlay rects, no new pipeline.
- `headless_world`: a `beams:` line with sources, beam tiles, beam jobs, private generations,
  boxes that took the full path because of beams, and tile bytes.
- `voxel_world --sky-sources`: a demo cavern under the start position from `boxel_demo` (a roof
  of sources, an overhang, a single sunbeam), started at 21:00 so the realm curve is visible.

## 7. How #129 and the vertical-world plan fit

- **Nothing must come first** for sources and per-realm ambient. Both work in today's one-realm
  worlds: a sunbeam or a lit zone in the overworld (which darkens at night, as decided), and
  per-realm curves tested with a test generator of two stacked `Ground` realms (legal today:
  `check_realms` only asks for built forms and one drawn realm).
- **Beam clipping at planes** reads `WorldGenerator::realms().planes()`, which exists now.
- **#129 (EW-REALMSKY) is independent** of sources: it fixes the open sky's start and floor per
  column and per realm. Beams never read the floor or `T`. The only shared code is the box's
  direct rows, where both are ORed.
- **The Nether-like cavern realm (V1)** needs sources and per-realm ambient, not #129: it lies
  under the surface's blockers, so it has no direct sky either way. Its realm gets a constant
  curve and its roof gets sources.
- **The ceiling realm (V4)** needs WG4 (#98) and #129 first: today a realm above the surface
  breaks the walk (darkness or `SkyRule::Open`). After #129 the ceiling is `Closed` for the walk
  and its underside is lit by sources from its own generator, so #130's mirrored walk is not
  built.
- **`RealmSky::Under` goes away**: #129's change drops the variant (nothing reads it yet) and the
  ceiling realm uses `Closed` plus sources. ADR 0039 sec. 15 is amended in lane 0.
- **WG4** later tightens the source gate (`ColumnBounds::sources` per realm and column) and the
  "box below the floor" shortcut keeps working: `BelowFloor` covers the sky, beams are separate.

## 8. Build order, in lanes

Lanes 1 to 3 are sequential; lane 4 runs in parallel with 1 to 3; lane 5 after 3 and 4; lane 6
(game) after 5. Each lane gets the owner's OK for its protected area (the design OK is from
2026-11-02).

**Lane 0. ADR (docs only).** `docs/adr/0061-fuentes-de-cielo.md` (Spanish) from this plan (0059
and 0060 go to lanes E15 and E16);
amendments to ADR 0015 (sec. 4 and 5: beams are direct cells; sec. 8: face shade per realm) and
ADR 0039 (sec. 15 without `Under`, sec. 16 with `RealmLighting` and the environment fields of
sec. 3). XS.

**Lane 1. Source states and masks** (`boxel_voxel`; not protected). S.
- Tests: registry validation (shape, opacity, emission, collision, range); masks equal brute
  force for every storage with sources (proptest, as `tests/masks.rs`); a 1-bit palette with a
  source builds its masks; `may_source` never says no wrongly; the cache resets after `set`.
- Bench: `masks/cave_1bit` unchanged (the borrowed view, 0.006 us measured); new
  `masks/cave_with_sources`.

**Lane 2. Beams in `boxel_light`** (protected: light). M.
- Code: `beam_tile.rs`, `LightInput::beams`, `build_direct`, `classify`, oracle with sources.
- Tests:
  - `tests/oracle.rs`: random 5x5x5-chunk worlds with sources (both directions, ranges 1 to 256,
    blockers of opacity 0, 1, 5 and 15, realm planes, sources above and below `T`): every inner
    chunk equals the oracle bit for bit, with and without fast paths.
  - `tests/beam_tile.rs`: `build` equals a brute-force trace over generated and hand-built
    columns; a beam stops at leaves and water like the sky; a beam stops at a plane; windows
    built in pieces equal one big window; an in-place retrace after an edit equals a rebuild.
  - `tests/far_coords.rs`: the same neighborhood with sources at 0, plus or minus 2^31,
    plus or minus 2^40 and `MAX_BLOCK_COORD - 64` gives the same bits (ADR 0014).
- Benches (one run per side): `light/box_cave_dark` must stay at 0.04 us with `beams: None`;
  new `light/box_cavern_ceiling` (a full ceiling over a 64-high cavern, target 100 us or less),
  `light/box_sunbeam` (one source), `beam_tile/build_loaded` and `beam_tile/build_generate`.

**Lane 3. World integration** (`boxel_world`, `boxel_worldgen` trait; protected: light and
streaming). M to L.
- Code: `SourceColumn`, the gate, `JobKind::BeamTile`, `start_mesh`, edits, unload,
  `Reference`, `WorldStats` (`beam_tiles`, `beam_jobs`, `beam_private_generations`,
  `waiting_beams`, `light_beam_boxes`).
- Tests (`src/tests/light.rs`, `streaming/tests/sky.rs`, `tests/integration.rs`):
  - A world without sources: zero beam jobs and waits, and meshes byte-identical to `main`.
  - Shuffled load order: a proptest over load orders, viewer paths (fly, dive, teleport),
    executors and 1 vs 6 threads gives identical mesh hashes in a world with a lit cavern.
  - Edits, each compared with `Reference`: placing a source and removing it equals never placing
    it; a blocker mid-beam cuts it; removing the blocker that ended a beam extends it; a source
    two layers above the loaded set (unloaded) lights the top loaded layer the same as when it is
    loaded; a saved source in an unloaded chunk is read through the store; with
    `NullChunkStore`, unloading a creative source removes its beam; a 256-cell beam edit
    publishes in one batch (or counts as split).
  - Far coordinates: the same world at x = 2^40 and y = -2^40 gives identical bits.
- Level-1 measurement: `headless_world` quads and chunks at r16, y = -1000, without and with the
  demo lit cavern, plus the `beams:` line; the lane 2 benches; then
  `just perf-smoke cave-static cave-path` (and a new `lit-cavern` case from lane 5 once it
  exists; a new case goes in `tools/perf/` with its test). Paste the table in the PR.

**Lane 4. Per-realm ambient and face shading** (`boxel_world` daylight, `boxel_render`,
`boxel_shaders`; protected: render and shaders). M.
- Code: `RealmLighting` (curve, ambient color, 6 shade factors per realm, defaults equal to
  today), `RealmPlanes`, the `View` fields, the realm lookup in `finish_vertex`, `face_shade` from
  the realm entry, `model.wgsl`, `cell.wgsl`, `lod_common.wgsl`, the model poser's shade.
- Tests:
  - `tests/naga.rs`: `View` at 816 + 8192 B, still one uniform block and one texture pair on Low.
  - `gpu_terrain`: every existing view identical on High, Mid and Low (one realm).
  - New gpu scene with two realms meeting at a plane, at noon and at midnight: the lower realm
    with a constant curve stays lit, the upper darkens; a top face just under the plane takes
    the upper realm's curve; identical pixels on all tiers and at every `FAR_SHIFTS` position;
    a plane farther than 2^31 from the camera clamps without changing any visible pixel.
  - New gpu test for flat shading: a realm whose shade table is flat gives top and bottom faces
    of the same block, under the same light, equal pixels; the default table keeps today's 1.0
    and 0.5 ratio. Identical on all tiers.
  - Unit: a realm's ambient color reaches the uniform per channel; the default entry equals
    today's `sky_light` and `face_shade` values bit for bit.
  - Unit: the extract samples one curve per realm; `None` falls back to the global curve.
  - GPU tests: `BOXEL_REQUIRE_GPU=1 tools/quiet.sh cargo test --workspace --locked --test '*gpu*' -- --test-threads=1`.
- Level-1 measurement: `just perf-smoke surface cave-static` (shader cost; expected within noise).

**Lane 5. Debug, demo and tools** (`boxel_debug`, `boxel_demo`, `examples/`, `tools/perf/`). S
to M.
- F3 lines, `SkySourceMarkers`, `voxel_world --sky-sources`, `stress --edits` with a scripted
  source place, remove and beam block, the `lit-cavern` perf case.
- Tests: the overlay shows the source at the crosshair and the realm at the eye; markers project
  inside the screen; `--sky-sources` builds its cavern on the ground.

**Lane 6. Game** (in `game/`). The generator step, `sky_source_layers`, the creative items and
pick mode, the environment table into `RealmLighting`. Measured with the game's own cases.

**Option B lane (only if asked):** constant sources, S to M, light only; tests as lane 2 with
`Block` sources (the oracle floods RGB from beams), plus `gpu_terrain` showing a constant zone
identical at noon and midnight.

## 9. Decisions of the owner (2026-11-02)

The plan's open questions, answered by the owner when approving it:

1. **Range cap: 256 cells** (8 chunk layers). A game that needs taller lit caverns places a
   second row of sources.
2. **Leaves and water end the beam**, as they end the open sky, so a source looks exactly like
   the sky above the same blocks.
3. **Option B: later**, and as constant sources if ever. Nothing in the decision needs it now.
4. **Source blocks keep fluids out**: the game marks the source block `keeps_fluids_out`, so
   generated ambience stays where worldgen put it.

Also decided when approving: per-realm face shading is part of Option A (sec. 3, lane 4).
