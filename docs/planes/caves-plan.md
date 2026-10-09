# Infinite-depth cave generator: proposal review and implementation plan

Read-only review. I read the code on master and on `perf/border-opt`. Repo root: el repo. Numbers marked "measured" come from docs/ or target/criterion. Everything else is an estimate.

## 1. Scores (1-5)

**Proposal 1: Fidelity (DeepGenerator)**
- Looks like Minecraft: **5**. It has every 1.18 cave type plus pillars, ravines, deepslate, a lava level, ores and hashed eras, all on gradient noise.
- Gen cost and spikes: **3**. The 4x8x4 grid is right. But every voxel is interpolated and painted (120-250 us est.), there is no per-cell classification, and it gives up uniform chunks.
- Render and culling: **4**. It is the only proposal with evidence: a Python simulation where 47-48 of 48 underground chunks get a mesh, at 630-920 quads each. But it overstates the High draw-list limit (see section 2).
- Exactness and determinism: **4**. It is exact everywhere (`div_euclid` eras, integer warp, rational frequency per axis). It copies hash2's tile scheme, which has no legacy values to protect, and it runs f64 math on every voxel.
- API for games: **3**. One big generator with rich settings (EraDef, per-type `enabled` flags). Configurable, but not composable.
- Risk and size: **2**. Largest scope: about 18 new blocks, eras, lava, ravines and ores. Its phase 2 alone is fine.

**Proposal 2: Cost first (CaveTerrainGenerator)**
- Looks: **3**. Cheese and spaghetti only, stone only, and value noise, which looks boxy and axis-aligned.
- Gen cost and spikes: **5**. Each cell is classified by the min and max of its corners, the 1-bit mask goes straight into Packed, and an analytic chunk bound is optional. The measured baselines it quotes are correct.
- Render and culling: **5**. The most complete. It checks residency against the arena, the slots and the draw lists, and gives a full cave-culling design: 15-bit face links, a BFS in Manhattan-distance order, a visibility bitset in `cull.wgsl` and `CpuCull`, and a test that culling on and off give the same pixels.
- Exactness: **4**. It is exact (power-of-two cells by shift, a full 64-bit hash3). But only power-of-two frequencies are allowed, and the f32 SIMD path must keep the same operation order as the scalar one.
- API: **3**. The generator is closed. The engine hooks are reusable: FaceLinks, ChunkVisibility, `from_occupancy` and a quad budget.
- Risk and size: **3**. The generator is small, but culling touches the world, the renderer and the shaders. Its 55% meshed estimate is probably low given the simulation.

**Proposal 3: Engine and API first (Pipeline)**
- Looks: **3**. Gradient noise, cheese and tunnels. But the default roof of 8 means no cave entrances, which is not Minecraft-like. No noodles, ravines or depth bands.
- Gen cost: **4**. Corner-sign fast paths keep Uniform stone and `is_enclosed_opaque` working, with integer interpolation. But it computes heights in every underground chunk (about 11 us wasted) and has no direct-to-Packed write.
- Render: **3**. It correctly flags the arena and table overflow, `RESIDENT_CAPACITY`, the upload byte budget and the 3D-distance job priority. It leaves culling to M2 with no design.
- Exactness: **5**. The carve decision is made in integers (i32 fixed-point trilinear). Octaves finer than a voxel are dropped, so cells stay i64. A point oracle is checked against the bulk path near, far and deep. One bug: its hash3 ends in `^ z` with no final mix, so neighboring z cells differ only by an XOR.
- API: **5**. Carver and Decorator traits, a point plus bulk contract, explicit salts, a LOD-aware ChunkCtx, Trees moved into the library, and the existing goldens reused as oracles.
- Risk and size: **4**. The refactor is guarded by goldens. But it puts the Pipeline refactor before the cave test case the owner wants next.

## 2. Where the proposals disagree, checked against the code

- **High draw-list limit (Proposal 1 is wrong, Proposal 2 is right).**
  - In `crates/boxel_render/src/renderer.rs`, 10,922 chunks is only the threshold where a warning fires (`6 * drawable > cap_opaque`).
  - The real cap is 65,535 visible face buckets, counted after frustum culling and the per-bucket backface test in `crates/boxel_shaders/shaders/cull.wgsl`.
  - At radius 16 underground that is about 15k draws, which fits. At radius 32 underground without culling it is about 45-60k, close to the cap. Overflow drops arbitrary buckets, which shows as holes.
- **hash3 design.** hash2's tile scheme exists only to keep the old i32 values bit for bit (ADR 0014, `crates/boxel_worldgen/src/noise.rs`). hash3 has no legacy values, so a full 64-bit chain is enough. It must end with a mix: `s(s(s(mixed_seed ^ x) ^ y) ^ z)`.
- **Unbounded `vertical_chunks`.**
  - `known_air` only calls `contains(y)` (`crates/boxel_world/src/streaming.rs`), and `capacity_hint` handles far endpoints. So `i64::MIN..=top` needs no world change (Proposal 3 is right).
  - `--fixed-height` sets the vertical radius to `u32::MAX`, clamped to `MAX_RADIUS` = 4096. With an unbounded generator that means 4096 layers below the camera, so the examples must reject it.
- **Uniform fast path.** Both `is_enclosed_opaque` (streaming.rs) and `trivially_empty` (`crates/boxel_mesh/src/mesher.rs`) need a Uniform opaque center and 6 Uniform opaque face neighbors. Ores or stone variants turn that off. Proposals 1 and 3 are right.
- **Capacity.**
  - Defaults are 8 Mi quads (64 MiB) and 32,768 slots, with `dropped_chunks` counted.
  - Mid and High request the adapter's own `max_storage_buffer_binding_size` (`crates/boxel_gfx/src/tier.rs`), so a 128-256 MiB arena is allowed on desktop Vulkan.
  - `RESIDENT_CAPACITY` is 32,768 in `examples/stress.rs` and has to follow.
- **Reuse.**
  - `Occupancy = [u32; 1024]` and `ChunkData::opaque_occupancy` already exist (`crates/boxel_voxel/src/storage.rs`), so `from_occupancy` is feasible.
  - `fbm_2d_grid` is `pub(crate)`.
  - `Scale` already does exact per-octave splits, so a frequency per axis costs nothing. Proposal 3's `y * k` only allows integer stretch.
- **Roadmap.** In `docs/architecture.md`, M2 lists BFS cave culling for Mid and Low only, and Hi-Z for High. The job priority is plain 3D distance, so underground chunks go before the visible horizon (Proposal 3 is right).
- **Measured baselines.**
  - From target/criterion: `surface_chunk` 46.2 us, `air_chunk_in_band` 11.2 us, encode 15.5 us, decode 13.8 us, gather + mesh 91.8 us, `mesh_caves` (synthetic trig-field scene) 35.3 us.
  - Forest world, radius 32, fly at 64 voxels/s on the 780M: p99 2.70 ms, border-crossing frame 1.68 ms on average, 0 frames over 6.94 ms, 1.88 M quads, GPU about 1 ms per frame.
- **Chunk counts (computed).** Radius 16: 797 mesh columns, 13,549 mesh chunks, 17,119 data chunks. Radius 32: 3,209 columns, 54,553 mesh chunks, 64,771 data chunks.
- **New risk no proposal raised: BFS cave culling leaks with 32³ chunks.**
  - Links are per face pair. A surface chunk whose open-air side face also touches a cave links that side face to its bottom face.
  - With dense caves almost every surface chunk will do this, so a BFS from a surface camera may reach most of the underground.
  - It has to be measured. Hi-Z is the robust fallback on High.

## 3. Synthesized plan

**v1 world (one generator).**
- Surface: today's heightmap, bit for bit.
- Surface rules: today's (grass, sand, snow, 3 dirt).
- Below: stone down to `i64::MIN`, no bedrock.
- Caves (Minecraft 1.18 style): cheese, spaghetti, noodles and entrances.
- Trees that check the ground under them.
- Stone only underground in v1, so chunks below the surface band are 1-bit.

**What comes from each proposal.**
- Proposal 1: the cave model and its look.
- Proposal 3: integer carve decisions, the point spec versus bulk oracle, explicit salts, Trees moved into the library, and the stage structure.
- Proposal 2: cell classification, the direct 1-bit write, and the culling design.
- New in this plan: face info is computed at generation time instead of in the mesh job (phase 4).

**Density model (engineering defaults, versioned constants).**
- Density: `D = min(cheese, spaghetti, noodles, entrance)`. Positive means solid. Starting wavelengths come from Proposal 1, with power-of-two denominators.
- Grid: D is sampled in f64 at a world-anchored 4x8x4 lattice, 9x5x9 = 405 corners per chunk.
- Quantize: clamp D to [-4, 4], then `q = round(D * 2^20)` as i32.
- Per voxel: integer trilinear `S = sum(wx*wy*wz*q)`, with weights summing to 128, so `|S| < 2^30`. Air if `S < 0`. The inner loop runs incrementally along z, using additions only.
- Cells: if all 8 corners are >= 0, fill solid in bulk. If all are < 0, fill air. Otherwise go voxel by voxel. This is exact because the interpolation is a convex mix of the corners.
- Surface fade and entrances need heights only at the 9x9 coarse columns, via `fbm_2d_grid`. These are computed only for chunks in the surface band.
- Output paths:
  - Above the band: air with no job.
  - All 405 corners solid and below the band: `Uniform(stone)`.
  - Below the band: a stone/air Occupancy written through `ChunkData::from_occupancy` (1-bit Packed).
  - In the surface band: the dense path (surface rules, then carve, then `from_dense`).

### Phase 0: baselines (no code)
- Run stress on master + `perf/border-opt` with the forest world, at radius 16 and 32, scenarios static, fly 64 and spin, on both GPUs, with `--spans`.
- Record fps, p50, p99, p99.9, worst frame, frames over 6.94 ms, border-crossing frame, catch-up time, quads, draws, and the spans `world.positions`, `world.dispatch`, `world.drain`, `render.upsert`, `render.upload`, `render.encode`.
- The RTX was missing in the last round, so redo it there.

### Phase 1: 3D noise (`crates/boxel_worldgen/src/noise/d3.rs`, re-exported from `noise`)
- New public items:
  - `hash3(seed, x, y, z) -> u32` (full 64-bit chain with a final mix)
  - `Frequency3 { xz: Frequency, y: Frequency }`
  - `gradient_noise_3d(seed, x, y, z, Frequency3) -> f64`: improved Perlin, 12 edge gradients from a 16-entry table, quintic fade, clamped to [-1, 1]
  - `Fbm3 { frequency: Frequency3, octaves, lacunarity: u32, gain }`
  - `fbm_3d`
  - `fbm_3d_grid(seed, xs, ys, zs, Fbm3, out)`: public, with one lattice table per octave
  - `fbm_2d_grid` becomes public.
- Rules:
  - Octaves with more than 1 cell per voxel on any axis are dropped, so cells stay i64 and need no i128 path.
  - Use only `+ - * /`, `sqrt` and `round`. No `mul_add`, `powf` or libm.
  - `hash2`, `value_noise_2d` and `fbm_2d` are unchanged.
- Tests (`noise/tests.rs`, `tests/noise_oracle.rs`):
  - hash3 golden values.
  - `fbm_3d_grid` equals `fbm_3d` bit for bit at the origin, across ±2^31 on each axis, at 2^40, at y = -2^60 and near ±2^61.
  - Values stay in [-1, 1]; neighbor steps stay under the Lipschitz bound.
  - Mean and variance of 10^5 samples near and far within 2%.
  - 64³ windows 2^32 apart differ.
- Bench `benches/caves.rs`: `fbm_3d_grid_9x5x9`.
- Acceptance: bit-identical everywhere; `fbm_3d_grid` of 9x5x9 with 3 octaves takes 10 us or less.

### Phase 2: OverworldGenerator v1 (`crates/boxel_worldgen/src/overworld/`)
- Types:
  - `OverworldGenerator { seed, settings }`, implementing `WorldGenerator`, with `vertical_chunks() = i64::MIN..=((max_h + 1 + tree_reach) >> 5)` and `name() = "overworld"`.
  - Public stage structs, each a pure function of (seed, i64 coordinates) with a point form and a bulk form:
    - `Heightmap`: equal to `TerrainGenerator::height_at`.
    - `SurfaceRules`.
    - `Caves { settings: CaveSettings }`, with `CaveSettings { salt, cheese, spaghetti, noodles, entrances: Option<..>, surface_fade, density }`. Exposes `fill(ctx, buf)` and the spec `carves(p, surface) -> bool`.
    - `Trees`: moved from `examples/stress/forest.rs` and `examples/voxel_world.rs`, and it checks `is_solid` at the trunk base.
  - Debug helpers: `block_at(p, registry)`, `is_solid(p)`, `density_at(p)`.
  - A `ForestGenerator` (TerrainGenerator + Trees) so both examples drop their copies.
- In `boxel_voxel`: `ChunkData::from_occupancy(&Occupancy, set, clear)`, returning Uniform when the mask is all set or all clear.
- Examples:
  - `--world flat|terrain|forest|overworld` and `--caves off|closed|mc` in stress, headless_world and voxel_world.
  - Reject `--fixed-height` when `vertical_chunks().start() == i64::MIN`.
  - `--viewer-y` in headless_world; `--arena-quads` and `--chunk-slots` in stress, with `RESIDENT_CAPACITY` following the slots.
  - The overworld world defaults to 24 Mi quads and 65,536 slots.
  - New stress scenario `dive` (straight down at `--speed`).
- Tests (`tests/overworld.rs`):
  - Every voxel of `generate` equals `block_at` on chunks at the surface, at y = -1, -100, -2^20 and -2^40, at x = 2^40, across ±2^31 and near ±2^61. This also proves there are no seams.
  - `Heightmap` equals `TerrainGenerator`.
  - `ForestGenerator` reproduces the 8 recorded (logs, fnv) pairs.
  - Crafted settings hit both fast paths (Uniform stone and Uniform air).
  - Air fraction of 64³ windows near versus at 2^40 and y = -2^40 within tolerance.
  - Determinism across 8 threads.
  - Adding Trees leaves cave output unchanged (explicit salts).
  - No floating trees.
  - FNV goldens are frozen only after the owner approves the look.
- Benches: `cave_chunk`, `cave_chunk_far` (2^40, y = -2^40), `solid_chunk`, `surface_chunk_caves`, `density_corners_405`. In boxel_mesh: `gather_mesh/cave` with its quad count.
- Acceptance:
  - `cave_chunk` p50 60 us or less (target 40); `_far` within 1.3x of near; `solid_chunk` 25 us or less; `surface_chunk_caves` 90 us or less.
  - `headless_world --world overworld --view-distance 16 --viewer-y 80|-1000 --hz 144` loads in 0.6 s or less at both heights.
  - Record: meshed fraction, quads per mesh (p50, p99, max), voxel MiB.
  - Sanity ranges: meshed fraction 0.6-1.0 underground, p50 of 400-1,200 quads.

### Phase 3: capacity and spike guards (driven by the phase 2 numbers)
- `WorldSettings::max_mesh_quads_per_frame`, default 131,072 quads (1 MiB), applied in `apply_meshes` next to `max_mesh_events_per_frame`.
- Stress report adds `dropped_chunks`, arena use %, and maximum upload bytes per frame.
- Measure horizon latency under the 3D-distance priority. Change it only if fly shows late horizon chunks.
- Acceptance on the RTX, overworld mc at radius 16:
  - Surface camera with fly 64, and underground camera with fly 64 and dive 64.
  - 0 frames over 6.94 ms, p99 4 ms or less, `dropped_chunks` 0.
  - Border-crossing frame 2.5 ms or less on average; catch-up 25 ms or less per crossing.
- On the 780M, radius 16 surface fly must pass. Underground on the 780M is recorded; it is expected to fail until phase 4 (extrapolated about 5 ms of GPU for about 10 M resident quads).

### Phase 4: cave culling on all tiers (brought forward from M2)

**Status (2026-10-08): done, see ADR 0022 and `docs/rendimiento/cuevas-fase4-2026-10.md`.**
Differences from this plan: the search runs in the render extract with the frame's own camera
(not in `WorldSystems::Visibility`, which now only feeds it the link changes), and reuses its
result while the eye moves less than 4 voxels without turning; the links count only while the
renderer shows the chunk as its blocks are (a chunk with data and no published mesh, or with a
mesh built from other blocks, is seen through); nothing is culled with the eye in rock; on High
the bits share the draw-count buffer (4 storage buffers); `opaque_faces` and `all_opaque` are
computed but `is_enclosed_opaque` is not generalized yet. With the eye in a cave (`--cave`) it
hides 44-77 % of the faces at radius 16 (61 % on average). **The gate fails:** from the surface at radius 16 it hides only 15-45 % of the
underground faces (the 32³ leak), so phase 5 should add 16³ sub-cells or Hi-Z.
- In `boxel_voxel`:
  - `ChunkFaceInfo { links: u16, opaque_faces: u8, all_opaque: bool }`.
  - `face_info(&ChunkData, &BlockRegistry)`: a bit-parallel flood fill over the 1,024 z-rows. Cutout and translucent voxels count as passable.
- Computed in the generate job (`start_generate` in streaming.rs), not the mesh job. It only needs the chunk's own voxels, so every data-ready chunk has links, even chunks that never mesh. Recomputed in the mesh job after edits.
- Stored in `ChunkSlot`.
- `opaque_faces` and `all_opaque` generalize `is_enclosed_opaque`: a center that is all opaque, whose 6 neighbors show opaque slabs on the touching faces, meshes to Empty. This keeps ore chunks cheap later.
- New `boxel_world/src/visibility.rs`:
  - A dense ring grid around the cull camera.
  - A BFS in Manhattan-distance order (Sodium style). Pending chunks count as fully linked.
  - It runs only when the camera chunk changes, on edits, and throttled when links change.
  - `ChunkVisibility` resource, `WorldSystems::Visibility` after Collect, `WorldSettings::cave_culling`, and a `world.visibility` span.
- In `boxel_render`: a per-slot bitset (8 KiB at 65,536 slots) bound in `cull.wgsl` next to `FLAG_LIVE`. `CpuCull` skips hidden slots. New `RenderStats::occluded_chunks`. Stress gets `--no-cave-culling`.
- Tests:
  - `face_info` against a naive voxel BFS oracle (proptest).
  - A ray-cast soundness proptest: every chunk with a visible voxel must be reachable.
  - `gpu_terrain`-style cave scene: culling on and off give identical pixels on High, Mid and Low.
- Bench: `cave_bfs` at radius 16 and 32.
- Acceptance:
  - BFS 0.3 ms or less at radius 16 and 1 ms or less at radius 32.
  - `face_info` 15 us or less per chunk.
  - 780M at radius 16 underground, fly and dive: 0 frames over 6.94 ms.
- **Gate.** If, from the surface at radius 16, culling hides less than 70% of the underground quads (the 32³ leak), go to phase 5 next. A BFS over 16³ sub-cells is the second option.

### Phase 5: radius 32
- Hi-Z in two phases on High (MIN reduction because of reverse-Z, R32Float mips).
- Arena growth (an M1 item) and quotas for the full resync.
- Acceptance at radius 32 on the RTX, surface and underground, fly 64 and dive 64: 0 frames over 6.94 ms and p99 5 ms or less. The 780M is recorded.
- Expected residency at radius 32 underground without growth: 35-50 M quads (est.).

### Phase 6: depth and content (after the owner approves the look)
- Deepslate band, stone variants and ores (requires `all_opaque` from phase 4), static lava, pillars, 2D-noise ravines, hashed eras (Proposal 1's scheme).
- Registry grows only from id 12 on; at most 15 states per era (4-bit palette).
- Acceptance: generation cost per chunk no more than 20% above phase 2; the phase 2 tests are extended and goldens re-versioned.

### Phase 7: public Pipeline API (when a second, game-defined stage exists)
- `Pipeline` with `Carver` and `Decorator` traits over the phase 2 stages.
- `ChunkCtx` with lod (stride sampling). `ChunkKey::origin`. A `Shape` trait for 3D density with per-column bounds.
- `OverworldGenerator` becomes `Pipeline::overworld`, guarded by its goldens.

### How to measure, every phase
- Builds and runs: release, through `tools/quiet.sh`, no parallel builds, `tools/wait-idle.sh 60` before each run, interleaved runs with at least 4 per binary.
- Cost of caves alone: same binary with `--world forest` versus `--world overworld --caves mc` versus `--caves off`.
- Culling A/B: `--no-cave-culling`.
- Cameras: surface `--camera 16,140,16,-90,-20` and underground `--camera 16,-1000,16,-90,0`.
- Spans to watch: `world.positions`, `world.dispatch`, `world.drain`, `world.visibility`, `render.upsert`, `render.upload`, `render.encode`.

## 4. Owner decisions (work starts on the default)
1. Cave density and mix. Default: Minecraft 1.18-like, about 10% air, with cheese, spaghetti, noodles and entrances. A `density` knob plus `--caves closed` (no openings) as the culling stress case.
2. Entrances breaking the surface. Default: yes, at about Minecraft's frequency.
3. Depth layering. Default: plain stone forever in v1, then Proposal 1's crust, deepslate to -256 with a lava level, and 512-voxel hashed eras whose caverns grow until about 4,400 voxels below -256, then stop changing.
4. Block content. Default: deepslate, tuff, granite, diorite, andesite, lava, and coal, iron, copper, gold, redstone and diamond ores, all in phase 6.
5. Lava. Default: a static, opaque, light-emitting block with no flow. Bedrock: none anywhere.
6. Lighting. Default: accept lit caves during the engine stress work; skylight right after culling if the look matters sooner.
7. Underground view. Default: keep vertical radius 8, which is about Minecraft's height at radius 16.
8. Ravines and surface. Default: 2D-noise ravines (local and exact, no worm carvers), and keep the heightmap surface (no overhangs) in v1.

## 5. Main risks
- The BFS leak with 32³ chunks (phase 4 gate, Hi-Z as the fallback).
- Residency at radius 32 underground goes beyond the arena and the slot table; it needs arena growth.
- Bigger meshes (about 6 KiB) cause upload spikes until the quad budget lands.
- Culling false negatives; the pixel-equality and ray-cast tests guard against them.
- The bulk path drifting from the point spec; the oracle test guards against it.
- Golden churn while the look is being tuned (freeze and version the goldens).
- Mid and Low draw one call per visible bucket, so about 15k draws at radius 16 underground without culling (est.).