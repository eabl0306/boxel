**Verdict:** I recommend proposal A, corrected and extended. Light is baked into the quads through a per-chunk light palette and computed inside the existing mesh job. The method for finding where sky light starts comes from C, and B's prerequisite of caching the occupancy masks is adopted. Two additions cut most of the cost (section 3). This was a read-only review: nothing was built, tested or run. Paths below are under el repo.

## 1. Scores (1 to 5)

**A. Baked light, light palette, light inside the mesh job (39 of 50)**
- **Visual: 5.** Minecraft-style smooth light and ambient occlusion (AO) on every face. Merged quads stay pixel-exact with per-pixel interpolation. RGB mixing.
- **Frame and streaming: 3.** No new main-thread work, and deep chunks cost about 1 us. But quads grow x1.7 to 1.9, almost all from AO. That raises vertex work, uploads and catch-up.
- **Memory: 3.** The 8 B quad and a small palette are kept. AO inflation adds about 0.8x quads on the GPU and in the CPU mesh copies (about 60 M quads at r32 underground).
- **Sky: 3.** The model is right (one sky top per column, independent of what is loaded, exact box). But its exact-height method and its `sky_floor` lower bound are wrong for the overworld (see check 6).
- **Edits: 4.** Bounded fan-out (8 chunks plus sky bands), no removal BFS, atomic batches, skip of identical meshes. Every light change is a remesh, which is affordable.
- **Tiers: 5.** One mesh format for all tiers. Low gets a second instance vertex buffer instead of a new binding, so ADR 0011 still holds.
- **Colored light: 4.** Per-channel max in one BFS. Many colors put pressure on the palette, with a deterministic fallback.
- **Day/night: 5.** Uniform only.
- **Sun shadows later: 4.** Depth pass reads only the geometry word. Baked sky works as an outdoor mask.
- **Implementation risk: 3.** The mesher fork and the layout change are contained. It assumes an occupancy cache that does not exist. Quad cost is unmeasured.

**B. One 34^3 light brick per chunk, sampled per pixel (32 of 50)**
- **Visual: 2.** The 4-tap Minecraft blend rule is good, but AO is unsolved. PackedQuad AO is never computed today (always 0xFF), so "+0% quads with AO kept" is wrong. Baking AO costs the +36 to 65% it claims to avoid. Low gets flat light.
- **Frame and streaming: 3.** No extra quads. But it adds a separate 0.1 to 0.3 ms job per surface chunk and 1 to 4.4 MiB of bricks per crossing, so light pops in 1 to 3 frames after the mesh.
- **Memory: 2.** 20 to 80 KB per lit chunk: 98 to 317 MiB at r32, 0.4 to 0.9 GiB in the worst case, plus an equal CPU copy.
- **Sky: 4.** Exact `sky_tops` contract with brute-force tests and persisted edited tops. The default scan covers bounded generators only.
- **Edits: 4.** Exact relight set (5.38 chunks on average, 8 max). Relight is separate from the AO remesh.
- **Tiers: 2.** Low needs a vendored wgpu-hal patch (bug confirmed in the source) or falls back to flat light. Mid is capped by its 128 MiB binding floor.
- **Colored light: 4.** Per-channel max, palette-compressed bricks.
- **Day/night: 5.** Uniform only.
- **Sun shadows later: 4.** Extra High pass on top of the volume.
- **Implementation risk: 2.** New job kind, a pool with 3 size classes, palette formats, a wgpu-hal fork, first paint with guessed light.

**C. Sparse 8^3 bricks sampled per pixel, sky computed by walking down each column (36 of 50)**
- **Visual: 3.** Getting AO and smooth light from the same samples is elegant. But every chunk stored as uniform 0 (all dark caves) has no AO, and Low gets flat light.
- **Frame and streaming: 3.** No extra quads and light runs in parallel jobs. But each pixel in a mixed chunk does up to 8 dependent memory reads (about +0.3 to 0.6 ms on the 780M, estimated). Meshes wait for their light, and deep viewers pay about 9 ms of worker time per crossing for column walks.
- **Memory: 4.** The leanest volume: about 40 MiB at r16 surface, about 0 underground (estimates).
- **Sky: 4.** Walking each column down through loaded or privately generated chunks is the correct exact method, with a deterministic depth limit.
- **Edits: 4.** Edits in darkness touch one chunk, and work is merged per chunk. Mesh and light can be one frame apart.
- **Tiers: 2.** Same Low problem as B.
- **Colored light: 5.** RGB in the same u16, no palette pressure.
- **Day/night: 4.** Uniform only, but its 20 Hz tick does not match the 64 Hz `FixedUpdate`.
- **Sun shadows later: 5.** Sky level per pixel as the outdoor mask, entities included.
- **Implementation risk: 2.** The most complex design (interior table, 150 border tiles per chunk, pool with free lists, a marker value for opaque voxels, meshes gated on light) plus the wgpu-hal patch. Its case against baking assumes 16 B per quad and a serial light stage, and A has neither.

## 2. Checked against the code (where the proposals disagree)

1. **Occupancy is not cached.** `ChunkData::opaque_occupancy` recomputes it on every call (`crates/boxel_voxel/src/chunk.rs:112`). The "maintained since M1" note in `crates/boxel_voxel/src/storage.rs:455` is still a plan. A is wrong here, B is right. Phase 0 adds a mask cache.
2. **AO is never computed.** `PackedQuad::new` sets 0xFF and `with_ao` has no caller outside tests. The measured cost of AO in the merge key is +36 to 65% (architecture.md section 9). B's "+0% quads" leaves AO out.
3. **Low cannot take a second texture today.** wgpu-hal 30.0.1 sets `SHADER_BINDING_LAYOUT = supports_compute` (`gles/adapter.rs:644`) and then skips the `glUniform1i` texture-unit remap (`gles/device.rs:544`). A second sampler would land on unit 0. B and C are right.
   - A's route does work. Without vertex buffer layouts, GL reports 16 vertex buffers (`gles/adapter.rs:800-806`), and first-instance emulation rebinds every instance-step buffer (`gles/command.rs:134-160`).
   - The floor `boxel_gfx::tier::floors::VERTEX_BUFFERS` must go from 1 to 2.
4. **C's case against baking does not apply to A.** A keeps 8 B quads through a 12-bit palette index, and its light runs inside the mesh job, so there is no serial light-then-mesh stage.
5. **The fixed step is 64 Hz** (`FixedTime::from_hz(64.0)`, `crates/boxel_app/src/time.rs:192`). C's 20 Hz is wrong; A's 64 Hz is right.
6. **A's sky method is wrong.** The cave roof defaults to 0 (`crates/boxel_worldgen/src/overworld/caves.rs:291`), so any cave type can carve a column's top voxel where its fade allows. Entrance tunnels (64 deep) can also open into caverns below them.
   - So a carve check "only where entrances open" is not exact.
   - And `min_height - surface_reach` is not a valid lower bound on the sky top.
   - The exact method is C's: walk each column down through chunk data, loaded or privately generated.
7. **Spare GPU words exist.** `ChunkGpu` has 3 padding words (`crates/boxel_shaders/src/gpu.rs:41`). `View` is 240 B and `crates/boxel_shaders/tests/naga.rs` asserts that size.
8. **The mesh job already has everything light needs.** `start_mesh` (`crates/boxel_world/src/streaming.rs:1110`) clones the center plus 26 neighbor snapshots and runs only when all 26 neighbors are ready. Light fits inside the job with no new chunk state.
9. **Edits remesh face neighbors only** (`streaming.rs:955-1003`). Edits to chunks that are not ready are dropped, and with `NullChunkStore` edits are lost on unload. AO needs edge and corner neighbors too, and sky changes caused by edits must follow the same lifecycle.
10. **The old light field is dead.** `ChunkData.light` is always `Uniform { sky: 15, block: 0 }` and only tests read it, so it can be removed.
11. **A palette entry is 8 B, the same as a PackedQuad.** The palette can live in the quad arena right after the chunk's 8 buckets. No new binding or allocator is needed on High and Mid.
12. **All built-in generators have a bounded top** (`vertical_chunks().end()`), so the column walk always has a start.

## 3. Decision

**Base design (A):**
- Light is a pure function of the blocks plus one sky top per column (T).
- It is computed inside the mesh job by an exact box BFS (breadth-first flood fill of light levels) over the 27-chunk neighborhood the job already holds.
- Light is baked per quad corner through a per-chunk light palette, with a 12-bit index in PackedQuad v2.
- AO and light are part of the greedy-merge key, in an in-repo fork of binary-greedy-meshing.
- The fragment shader interpolates the 4 corners bilinearly, the same on every tier.

**Taken from the other proposals:**
- From C: the sky top T is computed by walking each column down through loaded or privately generated chunks, with a deterministic depth limit. C's effective-radius rule is used for edits in darkness.
- From B: cached masks as a prerequisite, the exact relight-set property test, and persisted edited sky tops.

**New:**
1. **Dark-face AO elision.** A face whose 4 corner lights are all 0 gets AO "open" in its key. Almost all cave geometry is in total darkness, so it merges exactly as today. This is invisible at the default ambient floor, and the merged quads stay pixel-exact.
   - Quad estimates (to measure at gate G1): r16 surface view 4.22 M -> about 4.7 M, because only the about 0.5 M surface and tree quads grow x1.75; r16 underground about x1.0 to 1.05; r32 surface about 18.5 M, which fits the 24 Mi arena.
   - It must be revisited when held or dynamic lights arrive.
2. **Palette in the quad arena.** `light_base = quad_base + total_quads`, stored in `ChunkGpu`. No new GPU binding on High or Mid.
3. **64^3 light box** (chunk plus or minus 16 voxels), so each z-row is one u64. The exact region needed is plus or minus 15.

## 4. Data formats (bit layouts)

**Light value (u16)**, used for voxels and corners:

| Bits | Field |
|---|---|
| 12-15 | sky |
| 8-11 | red |
| 4-7 | green |
| 0-3 | blue |

Registry emission is a u16 with the same nibble positions; the sky nibble is 0.

**PackedQuad v2 (8 B, frozen size)**

`geo` (only the top 4 bits change):

| Bits | Field |
|---|---|
| 0-4 | x |
| 5-9 | y |
| 10-14 | z |
| 15-19 | w-1 |
| 20-24 | h-1 |
| 25-27 | face |
| 28-31 | flags (was reserved, 0 for now) |

`mat`:

| Bits | Field |
|---|---|
| 0-11 | texture layer |
| 12-19 | AO, corner k at bits 12+2k, values 0..3 (3 = open) |
| 20-31 | light palette index |

- The old sky, block and flags fields are gone.
- New constants: `GEO_FLAGS_SHIFT = 28`, `MAT_LIGHT_SHIFT = 20`, `MAT_LIGHT_MASK = 0xFFF`. `MAT_SKY_SHIFT`, `MAT_BLOCK_SHIFT` and `MAT_FLAGS_SHIFT` are removed.

**Light palette entry (`[u32; 2]`, 8 B)**
- `word0 = c0 | c1 << 16`, `word1 = c2 | c3 << 16`.
- Corner order (u, v): 0 = (0,0), 1 = (1,0), 2 = (0,1), 3 = (1,1). This is the same order as AO and `common.wgsl`.
- At most 4096 entries per chunk, in deterministic first-seen order.

**`ChunkMesh`**
- Fields: `quads` (bucket-contiguous), `bucket_counts[8]`, `palette: Vec<[u32; 2]>`, `hash: u64` (foldhash with a fixed seed over quads and palette).
- Arena allocation size is `quads + palette`.

**`ChunkGpu` (64 B)**

| Offset | Field |
|---|---|
| 0 | origin (i32 x3) |
| 12 | quad_base |
| 16 | counts[8] |
| 48 | flags |
| 52 | light_base |
| 56 | pad[2] |

**`ViewUniform`: 240 B -> 336 B** (vec4 only, std140, still one uniform block on Low). New fields:
- `sky_light: vec4`: rgb = sky color times daylight, a = ambient floor.
- `light_params: vec4`: x = AO strength (default 0.55, which matches today's `mix(0.45, 1, ao)`), y = block-light gain, zw = 0.
- `light_curve: array<vec4, 4>`: 16 levels, brightness per light level, with level 15 = 1.0.

**Low instance inputs**
- Slot 0: quad, `Uint32x2` at location 0 (as today).
- Slot 1: expanded palette entry per quad, `Uint32x2` at location 1, in a Low-only buffer parallel to the arena (same offsets).

**`SkyTile`, one per chunk column**
- `top: Box<[i64; 1024]>`, indexed `(x << 5) | z`: the highest voxel with opacity > 0, or `i64::MIN`.
- Plus `min`, `max`, `version`, `edited: bool`. 8 KiB each: about 7 MiB at r16, 27 MiB at r32.

**`PaddedLight`** (worker scratch): `Uniform(LightValue)` or `Full(Box<[u16; 39304]>)`, in `PaddedChunk::index` order (local coordinates -1..=32).

**`ChunkLight`** (gameplay cache, kept only within `light_cache_radius`):
- `Uniform(LightValue)`: 0 B.
- `SkyOnly(Box<[u8; 16384]>)`: sky nibbles only, 16 KiB.
- `Full(Box<[u16; 32768]>)`: 64 KiB.
- Indexed by `LocalPos::index`.

**`ChunkMasks`** (cached per `ChunkData`). Each mask is an `Occupancy` (4 KiB, bit z of word y*32+x):
- `opaque`: opacity 15.
- `partial`: opacity 1 to 14.
- `solid`: for physics and raycasts.
- `emitters: Vec<(u16 local index, u16 rgb)>`.

## 5. Rules

**Light**
- Opaque means opacity 15. Air and glass are 0, leaves and water are 1.
- A non-opaque voxel's level per channel is the max over its 6 neighbors of `L(n) - max(1, opacity(v))`.
- An emitter voxel takes its own emission, even if it is opaque.
- Channels mix by per-channel max. All math is integer, so the fixpoint is unique and independent of order.

**Sky**
- A voxel is direct sky when `y > T(x, z)`, and then its sky level is 15.
- T is the highest voxel with opacity > 0 in world content (generator plus edits), never "what is loaded".

**Column walk (computing T)**
- Start at `S = max(generator top voxel, highest edited blocker in the column)`.
- Go down layer by layer. Use the ready snapshot if the chunk is loaded, air if it is Missing or outside the bounds, and otherwise generate it privately.
- Stop when all 1024 columns have found a blocker.
- Below the generator range everything is air, so an unresolved column gets `T = i64::MIN`.
- A column still open after `sky_walk_limit` voxels gets `T = S - limit`. This is deterministic and depends only on content.
- Global floor: `floor = generator top - limit`. No column's T is below it, so a box whose top is under the floor has sky 0 without needing any tile.

**Box (chunk plus or minus 16)**
- Opaque and partial z-row masks come from the 27 cached `ChunkMasks`.
- Sky seeds come from differences in T between neighboring columns plus leaves directly under T.
- BFS uses a bucket queue (16 buckets) into non-direct voxels. Block light runs a second BFS from the emitters.
- A touched list clears the scratch, so there is no 512 KiB memset.
- The result for the padded region (-1..=32) is extracted from the box.
- All absolute comparisons are `saturating_sub` in i64 relative to the box, so results are exact at plus or minus 2^61.

**Fast paths** (no full BFS):
- **Dark:** no emitter in the 27 palettes, and the box top is below the floor or below the min T of the 9 tiles -> `Uniform(0)`.
- **Open:** no emitter, and the box bottom is above the max T -> `Uniform(sky 15)`.

**Corner light**
- Samples are the 4 front-layer voxels around the vertex.
- Drop opaque samples. Drop the corner sample when both side samples are opaque.
- Per channel: `(sum + n/2) / n`.
- This depends only on the vertex for every pair of visible coplanar faces, which is what makes merged quads exact.

**AO**
- The usual corner rule over the 3 front-layer neighbors of the vertex: if both sides are opaque, 0; otherwise 3 minus the opaque count. Only opaque blocks count, so leaves cast none.
- With dark-face elision, AO becomes 0xFF when all 4 corner lights are 0.

**Merge key:** (material, AO byte, palette index), mapped to a dense u32 id. Two unit faces merge only if their ids are equal.

**Shading (fragment)**
- `L` = per-channel bilinear interpolation of the 4 corners at the quad-local (s, t).
- `light = max(curve(L.sky) * sky_light.rgb, curve(L.rgb) * block_gain, ambient)`.
- `color = albedo * face_shade * mix(1 - ao_strength, 1, ao_bilinear / 3) * light`, then fog.
- At sky 15 noon with AO open this gives today's image.

## 6. Phases

### P0. Groundwork and ADR (no visual change)

**Docs**
- New `docs/adr/0015-luz-de-voxeles.md`: light as a pure local function, sky tops, PackedQuad v2 with palette, AO policy, the Low second instance buffer.
- Amend ADR 0005 (light leaves `ChunkData`, masks cache), ADR 0006 (mat v2, in-repo merge fork) and ADR 0011 (Low gets a vertex attribute, not a binding).
- Update `docs/architecture.md` sections 7, 9, 10.2 and 10.6.

**Code**
- `crates/boxel_voxel/src/registry.rs`:
  - `BlockDef { emission: [u8; 3], light_opacity: u8 }` replaces `light_emission`.
  - Validation: channels and opacity at most 15, and `RenderShape::Opaque` implies opacity 15 (new `RegistryError::InvalidLight`).
  - Tables: `emission: Vec<u16>`, `opacity: Vec<u8>`; accessors `emission(s)` and `light_opacity(s)`. `EMISSIVE` means emission != 0.
  - `default_blocks`: leaves 1, water 1, opaque blocks 15.
- New `crates/boxel_voxel/src/light.rs`: `LightValue(u16)`, `LightTables::from_registry` (opacity, emission, light-blocking).
- `crates/boxel_voxel/src/chunk.rs`:
  - Remove `light`.
  - Add a private `MaskCache(OnceLock<Box<ChunkMasks>>)`: `Clone` gives empty, `PartialEq` ignores it, and `set` resets it.
  - Add `masks(&self, &LightTables) -> &ChunkMasks`, with fast paths: uniform chunks use a shared static, 1-bit chunks use their packed words (flipped by palette order), other chunks decode once.
  - Add `may_emit(&self, &BlockRegistry) -> bool` (palette scan; it may say yes wrongly but never no wrongly).
- `crates/boxel_voxel/src/storage.rs`: remove `LightStorage`. Fix `crates/boxel_voxel/tests/paletted.rs:536` and `crates/boxel_mesh/tests/common/mod.rs:113`.

**Tests**
- Proptest: masks equal brute force for Uniform, Packed 1, 2, 4 and 8 bits, and Direct.
- The cache resets after `set` and after `Arc::make_mut`.
- `may_emit` never returns a false negative.
- Registry validation.

**Benches** (`crates/boxel_voxel/benches/paletted.rs`): `masks/cave_1bit` at most 2 us; `masks/surface_packed` at most 25 us (baseline: `opaque_occupancy_terrain`).

### P1. New crate `boxel_light` (pure CPU, no ECS, no GPU)

**Code** (`crates/boxel_light/`)
- `src/box_light.rs`:
  - `LightInput { origin: ChunkPos, chunks: [Option<&ChunkData>; 27], sky: SkyInput, tables: &LightTables }`, where `SkyInput` is `BelowFloor` or `Tiles([&SkyTile; 9])`.
  - `LightScratch` (about 0.7 MiB per worker).
  - `fn compute(&LightInput, &mut LightScratch, &mut PaddedLight) -> LightPath`, where `LightPath` is `FastDark`, `FastOpen` or `Full { sky_pops, block_pops }`.
- `src/sky_tile.rs`: `SkyTile`, `fn walk(top_voxel, limit, layer: impl FnMut(i64) -> LayerData) -> SkyTile`, `fn rescan_column(..)`.
- `src/chunk_light.rs`: `ChunkLight::from_padded`.
- `src/oracle.rs` (`cfg(any(test, feature = "oracle"))`): a scalar global solver per channel over an explicit voxel world, with T found by brute force.

**Tests**
- `tests/oracle.rs` (proptest): a 5x5x5-chunk random world (opacity 0, 1 and 15, RGB emitters, T including `i64::MIN`). Every chunk of the inner 3x3x3 gets `PaddedLight` equal, bit for bit, to the global solution.
- Forcing the full path on fast-path inputs gives the same output.
- `tests/far_coords.rs`: the same neighborhood at 0, plus or minus 2^31, plus or minus 2^40 and `MAX_BLOCK_COORD - 64` gives identical bits.
- `tests/sky_walk.rs`: the walk equals a brute-force scan of generated chunks for overworld (with entrances and trees), forest, flat and terrain, at x and z = 2^40. Also covers the limit cap and `i64::MIN` for an empty flat world.

**Benches** (`benches/light.rs`, release, idle machine):

| Bench | Target |
|---|---|
| `light/box_cave_dark` | 5 us or less |
| `light/box_surface` (real overworld at the origin, with trees and an entrance) | 150 us or less p50 |
| `light/box_cave_lamps_rgb` (1.5% of the floor, 3 colors) | 500 us or less |
| `light/box_far` | 1.15x of near or less |
| `sky_tile/walk_generate` | 400 us or less |
| `sky_tile/walk_loaded` | 60 us or less |

If `box_surface` misses its target, switch the sky pass to bitset dilation on u64 rows (C's method), with the same oracle.

### P2. Mesher (`crates/boxel_mesh`)

**Code**
- `src/greedy.rs`: a fork of binary-greedy-meshing 0.5.2 `lib.rs` (MIT notice kept).
  - Face culling is unchanged.
  - In `face_merging`, every `voxels[..] ==` becomes a key comparison against a 34^3 u32 key scratch, filled per face only at visible faces. The existing face-bit guards already restrict comparisons to visible faces.
  - The quads carry the key id. The crate stays as a dev-dependency for the regression test.
- `src/shading.rs`: the AO lookup table indexed by the 8-bit front-layer neighborhood from `PaddedChunk.opaque`, `corner_light`, `LightPalette` (open addressing, first-seen order) and `KeyTable`.
- `src/packed.rs`: v2 constants; `with_light_index`, `light_index`, `flags`; remove `with_light`, `sky` and `block_light`; the `ChunkMesh` fields from section 4.
- `src/mesher.rs`:
  - `MeshInput { center, neighbors, light: Option<&PaddedLight> }`, where `None` means uniform sky 15 (today's look).
  - `MeshOptions { ao: AoPolicy }` with `Off`, `Baked` or `BakedLitOnly` (default `BakedLitOnly`).
  - `to_packed(layer, ao, light_idx)`.
  - Palette overflow (more than 4096 entries): requantize block channels to 3 bits, rebuild the keys, count it.
  - New `reference_face_shading(padded, light, policy)`.

**Tests**
- `tests/oracle.rs`: each quad, expanded to unit faces and decoded through the palette, equals `reference_face_shading`.
- Regression: `AoPolicy::Off` with no light gives quads byte-identical to bgm 0.5.2.
- A `debug_assert` checks that equal keys are constant along merged axes.
- Palette determinism and the forced overflow path.

**Benches** (`benches/mesh.rs`):

| Bench | Target |
|---|---|
| `mesh/cave` with `Off` | 1.15x of 28.8 us or less |
| `gather_mesh/cave` (dark, elision on) | 1.1x of 91.5 us or less |
| `gather_mesh/cave_lit` | 160 us or less |
| `gather_mesh/surface` (light keys, without the box) | 150 us or less |

**Example:** `examples/headless_world.rs --ao off|baked|lit-only` prints quads and palette p50, p99 and max. This gives the first AO numbers, a worst case because light is still uniform.

### P3. World integration (`crates/boxel_world`, headless)

**New `src/sky.rs`**
- `SkyMap` resource, keyed by column with `FixedState` hashing.
- `SkyColumn { state: Pending | InFlight { version } | Exact(Arc<SkyTile>), version, edited }`.
- A column is created with its first slot and dropped with its last.
- `JobKind::SkyTile` (`src/scheduler.rs`) is keyed by column and shares the in-flight cap. Its priority is just after generation of the column's nearest slot.
- Results are discarded if the version changed.

**`src/streaming.rs`**
- `start_mesh`:
  - If `box_top = origin.y + 47` is at or above `floor`, require the 9 columns to be Exact. Otherwise stay `Queued` and reschedule when a tile lands.
  - Pass `SkyInput`, `LightTables` and a `want_chunk_light` flag (within `light_cache_radius`).
- Job body: a thread-local `(Mesher, LightScratch, PaddedLight)`, then `boxel_light::compute`, then `mesh`. The result is `JobMessage::Meshed { .., mesh, light: Option<Arc<ChunkLight>>, batch }`.
- `apply_meshed`: skip the upsert when `hash` is unchanged; hold the results of one `EditBatch` until all arrive or `edit_batch_hold_frames` (2) pass; store `ChunkLight` in the slot.
- `apply_edits`, per changed voxel p:
  - Face change: today's rule.
  - Render-opaque status changed: remesh every chunk whose padded region contains p (up to 8, including edge and corner neighbors).
  - Opacity or emission changed: compute the radius `r = min(14, max(old level at p, max old level of the 6 neighbors, old emission, new emission))` from the `ChunkLight` cache, or 14 if absent. If r is above 0, remesh every chunk whose padded region meets the box p plus or minus r (at most 8).
  - Sky top changed:
    - A blocker placed above T raises T.
    - Removing the voxel at T rescans down through loaded ready data; if the scan leaves the loaded range, the column goes Pending and a walk runs.
    - Then remesh every loaded chunk whose padded region meets the column band plus or minus 14.
  - All fan-out: epoch bump, priority 0, one `EditBatch`.
- Unload: when an edited chunk unloads under `NullChunkStore`, mark its column Pending (the edits are lost, so T may revert). With the later `ChunkStore`, persist per-column top overrides instead.

**`src/map.rs`**
- `ChunkSlot.light`; the mesh hash in `MeshState::Ready`.
- New `WorldStats` counters: `light_full`, `light_fast`, `sky_columns`, `sky_walks`, `sky_private_generations`, `waiting_sky`, `edit_batches_held`, `light_fanout`.

**`src/plugin.rs`**
- `WorldSettings`: `sky_walk_limit = 1024`, `light_cache_radius = 8`, `edit_batch_hold_frames = 2`, `ao_policy`.
- `WorldContent` gains `light_tables`.
- New query `ChunkMap::light_at(BlockPos) -> Option<LightValue>`.

**Tests** (`src/tests.rs`)
- **Determinism:** three viewer paths (fly, dive, teleport), `InlineExecutor` vs `TaskPoolExecutor`, and 1 vs 6 threads all give identical mesh hashes.
- **No light ripples:** without edits, each slot meshes once.
- **Edit completeness:** after random edits settle, every mesh equals a from-scratch rebuild, and the set of chunks that changed is a subset of the remeshed set.
- **Edit cases:**
  - Removing a lamp equals never placing it.
  - Digging in a dark cave remeshes only the AO set.
  - Digging a shaft gives sky 15 at its bottom.
  - Capping the shaft makes it dark.
  - A platform at +5000 shadows the ground.
  - The limit cap applies.
  - No frame shows a partial edit batch.
- **Far coordinates:** the same world at x = 2^40 and y = -2^40 gives identical hashes.

**Example:** `headless_world --light on|off` prints quads, palette sizes, light paths, sky walks and private generations, at r16 and r32, y = 80 and -1000, for overworld and forest.

**Gate G1 (quads vs today, r16):**

| World | Limit | Estimate |
|---|---|---|
| forest (no caves) | 1.9x or less | |
| overworld y = 80 | 1.25x or less | about 1.1x |
| overworld y = -1000 | 1.10x or less | |
| overworld r32 y = 80 | 22 M or less (fits the 24 Mi arena) | |

If a gate fails, the owner chooses GPU-occupancy AO on High and Mid (P8).

### P4. Render

**Shaders**
- `crates/boxel_shaders/shaders/common.wgsl`:
  - New constants, plus the `View` and `ChunkGpu` fields from section 4.
  - `VsOut` replaces `shade` with:
    - `@location(2) @interpolate(flat) face: u32`
    - `@location(4) st: vec2<f32>`
    - `@location(5) @interpolate(flat) light: vec2<u32>`
    - `@location(6) @interpolate(flat) ao: u32`
  - `decode_quad` takes the light words and sets `st` to the corner's (cu, cv) after the existing winding swap.
  - New functions `corner_light(words, k) -> vec4<f32>`, `level_curve(l)` (linear interpolation in the uniform table) and `voxel_light(i, view)`. `shade_fragment` applies the formula from section 5.
- `voxel_pull.wgsl`, in `vs_pull`: `let lw = quads[ch.light_base + ((q.y >> MAT_LIGHT_SHIFT) & MAT_LIGHT_MASK)];`. No new binding.
- `voxel_instanced.wgsl`: `vs_instanced(.., @location(0) q: vec2<u32>, @location(1) lw: vec2<u32>)`.
- `cull.wgsl` only sees the struct field rename.

**Rust**
- `crates/boxel_shaders/src/gpu.rs`: `ChunkGpu.light_base`; `ViewUniform` at 336 B.
- `crates/boxel_gfx/src/tier.rs`: `floors::VERTEX_BUFFERS = 2`, with classify tests at the floor.
- `crates/boxel_render`:
  - `renderer.rs`: `upsert_chunk` allocates `quads + palette`; `ViewParams.light: SceneLight`.
  - `chunk_table.rs`: `ChunkRecord.light_base`, set in `gpu()`.
  - `upload.rs`: the palette is written after the quads; on Low, a `low_light` buffer is written at the quad offsets.
  - `path/mod.rs`: a second Low vertex layout (`Uint32x2` at location 1, stride 8, Instance).
  - `path/low.rs`: `set_vertex_buffer(1, ..)`.
  - `plugin.rs` extract: noon `SceneLight` until P5.

**Tests**
- `tests/naga.rs`: new constants, `View` 336 B, `light_base` at offset 52. The Low rule (one uniform block, one texture plus sampler) is unchanged; Low has 2 instance attributes; the forbidden builtins are still absent.
- `tests/gpu_terrain.rs`: scenes under a tree, under an overhang, a cave with RGB lamps, a sunlit floor beside a wall (AO), and night. Pixels must be identical on High, Mid and Low, and at `FAR_SHIFTS`.
- The GL smoke test reads one lit pixel.

**Owner review:** screenshots of the same scenes plus x = 2^40 and y = -2^40.

### P5. Day/night

- New `crates/boxel_world/src/daylight.rs`: `TimeOfDay { tick: u64, ticks_per_day: u64 (76_800 = 20 min at 64 Hz), paused, speed }`, advanced in `FixedUpdate`.
- `DaylightCurve` gives the sky tint and intensity (day white 1.0, dusk warm about 0.6, night about (0.30, 0.35, 0.55) x 0.2), the ambient floor, and the fog and clear colors.
- Render extract fills `SceneLight` from it.
- `examples/voxel_world.rs`: keys to scrub time and a `--time` option. `examples/stress.rs`: `--time`.
- Tests: ticks are deterministic, and block-lit pixels are identical at noon and midnight.

### P6. Content and tools

- Full-cube emitters: lamps orange (14, 11, 6), red, blue and white, plus a lava placeholder (15, 7, 2). Real torches wait for non-cube block shapes.
- The F3 overlay shows sky and RGB light at the eye or crosshair.
- `--lamps` demo option.
- `stress --edits`: scripted lamp place and remove, shaft dig and cap.

### P7. Measure with the machine free, then decide

Run section 7, then take the AO policy and cave-culling ordering decisions.

**Acceptance on the RTX at r16, uncapped:**
- 0 frames over 6.94 ms.
- p99 at most 4 ms.
- Crossing frame at most 2.5 ms mean.
- Catch-up at most 25 ms for the surface (today 9.6) and for underground flight (today 18.7).
- Descent catch-up no more than 10% above today's 25.0 ms.
- Edit latency: all meshes of a batch published within 2 frames, with 0 split batches.

**Acceptance on the 780M, r16 surface:** 0 frames over 6.94 ms, p99 at most 4 ms, catch-up at most 25 ms (today 13.7).

**With `--fps-limit 144`:** frames over 7.5 ms not above today's counts.

### P8. Later

- GPU-occupancy AO on High and Mid, if G1 or P7 asks for it, or when held lights arrive.
- Sun shadow cascades on High: geometry-only depth from the same arena, a second cull dispatch, and the sky level as the outdoor mask.
- Dynamic held and entity lights in the shader.
- Tinted glass filters.
- Drop the CPU mesh copies and remesh on resync.
- `ChunkLight` on demand for entities in chunks without a mesh.

## 7. What to measure

- **Criterion benches** from P0 to P2: run with `tools/wait-idle.sh` and `tools/quiet.sh`, never while compiling.
- **headless_world** at r16 and r32, y = 80 and -1000, overworld and forest:
  - quads, meshes and arena share;
  - palette entries per chunk (p50, p99, max);
  - light paths (fast dark, fast open, full);
  - sky walks and private generations per crossing;
  - `ChunkLight` cache bytes and SkyMap bytes;
  - quads with AO elision on vs off.
- **stress** on the RTX and the 780M, r16 and r32, surface and underground, uncapped and capped at 144:
  - frames over 6.94 and 7.5 ms, p99;
  - crossing frame and catch-up;
  - GPU wait;
  - biggest upload per frame;
  - arena use and dropped chunks;
  - edit latency and split batches.
- **GPU cost of lighting:** surface p50 frame on the RTX (today 0.32 ms; target at most 0.45) and the extra GPU time on the 780M (target at most 0.3 ms).

## 8. Decisions only the owner can make (recommended defaults)

1. **AO policy:** baked into the merge key on every tier, skipped on faces with zero light (default). Alternatives: always baked, or GPU-occupancy AO on High and Mid. Revisit at G1 and when held lights arrive.
2. **Darkness:** cave ambient floor 0.02 (near black) or 0; night sky intensity 0.2 with a blue tint; day length 20 min.
3. **Sky depth limit:** 1024 voxels below the column's top content. Open shafts deeper than that get no direct sky; a smaller limit spares deep viewers the column walks.
4. **Light values:** leaves and water opacity 1; leaves cast no AO; RGB per-channel max; 4-bit rounded corners (exact quarter levels cost more surface quads); a blocker shades everything below it forever, as in Minecraft.
5. **First emitters:** full-cube colored lamps and a lava placeholder now; torches when non-cube shapes exist.
6. **Brightness curve:** Minecraft-like `f / (4 - 3f)` with a gamma setting.
7. **Gameplay light cache radius:** 8 chunks.
8. **Order:** lighting before cave culling. r32 underground stays blocked on the growing arena, with or without lighting.