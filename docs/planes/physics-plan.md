# Physics for boxel: review of proposals A and B, and one implementation plan

## 1. Verdict

**Use A as the base.** It uses Minecraft's per-axis clipping of AABBs, which is the smallest design that covers the owner's scope. It already has the exact i64 frame, the freeze policy, a sorted grid and an exact DDA raycast.

**Take five pieces from B:**
- Interpolation runs right after the fixed loop.
- A setter for the maximum fixed steps per frame.
- Contact faces are snapped exactly onto the block plane.
- Solid bodies are pushed apart by position, and stacking is allowed. This matches the owner's rule "solid = push, no overlap".
- `WorldPos` lives in `boxel_voxel`.

**Leave these out of v1:** B's TOI move-and-slide, exact sphere-vs-voxel, step-up, carrying riders, and general CCD against entities. Only projectiles get segment tests.

**B compares itself against a version of A that does not exist.** Its cost table and its precision section assume A uses absolute f64 and has no sleeping. The real A uses an i64 cell plus an f64 offset, and it has sleeping and a parallel step. So B's "1.3x cost" and "A snags at 2^45" claims do not apply.

## 2. Scores (1 to 5)

| Criterion | A | B |
|---|---|---|
| Fits the owner's scope | 4. Per-axis AABBs, gravity, raycast, solid/sensor. A few extras (step-up in P1, five small tuning components), and its default horizontal-only push breaks "solid = no overlap". | 3. Covers the same ground, but TOI slide with multi-normal ties, exact sphere-vs-voxel with feature culling, CCD against entities and 1/16 shape boxes go beyond "simple". Its stacking push fits the solid rule. |
| Correctness | 4. Exact i64 frame, freeze, exact DDA, no tunneling. Gaps: no plane snapping, a corner case in `normalize`, the wake rule misses chunk reloads, and Update reads mixed Transforms. | 4. Continuous TOI, plane snapping, push-out when embedded. TOI ties and the skin are where bugs hide. Its claims about A tunneling and snagging on seams are wrong. |
| Cost at 100 / 1,000 / 10,000 bodies | 4. Cheap clipping. Freeze, sleeping and a parallel step are all planned. | 3. About 1.3x per body by its own estimate. Sleeping and the incremental grid are deferred. Its cost baseline is wrong. |
| API for games | 4. Bevy-like. Runs in FixedPostUpdate, so gameplay needs no ordering. Too many small components. | 4. Compact `Body` with `teleport()`. Interpolation sits after the loop (good). Running in FixedUpdate sets forces gameplay to order itself against Integrate/Finish. |
| Determinism | 5. Sorted pairs, no FMA, translation-invariance test. | 5. Same, plus an exact dt of 2^-6 and an honest note on the streaming-timing limit. |
| Risk | 4. Proven algorithm, one new crate, low coupling. | 3. Error-prone TOI and sphere code. P0 touches 3 crates and needs agreement with the lighting work first. |
| **Total** | **25** | **22** |

## 3. Checked against the code

**Fixed time and messages**
- `FixedTime` defaults to 64 Hz with at most 8 steps per frame. `from_hz(64)` gives exactly 15.625 ms.
- There is no setter for the maximum steps per frame. A's "lower it" lever and B's decision both need one.
- `overstep_fraction()` returns f32.
- Inside `FixedMain`, `Time` is the fixed view, but `delta_secs()` is f32. Physics must use `FixedTime::timestep().as_secs_f64()`.
- The TimePlugin "Waiting" rule is real and has contract tests. Update messages reach FixedUpdate, and messages written in a fixed step can be read in Update in the same frame. A's plan for messages works as written.
- `RunFixedMainLoop` holds a single exclusive system and has no sets. Any system added there without an order is ambiguous, so B's `RunFixedMainLoopSystems` is needed.

**Transform and camera**
- `Transform` has an f64 translation, an f32 `Quat` rotation and no hierarchy.
- Render `extract` reads `(Camera, Transform)` in `Render`. `CameraView` floors the f64 translation.
- Following the player means a separate camera entity that copies the player's Transform plus the eye height.

**Blocks**
- `BlockDef { name, shape, solid, light_emission: u8, textures }` has no `Default`. Any new field breaks every struct literal: registry.rs, tests/contract.rs, and users' code.
- `StateFlags` already has SOLID and FLUID (FLUID = Translucent and not solid). Leaves are solid. Water is not.

**ChunkMap and the world**
- `ChunkMap` is a `Resource` with public `id_at`, `data_at` and `block_at`. `block_at` gives None for Queued/Generating and AIR for Missing.
- No slots exist outside `vertical_bounds`, so the reader must check `WorldSettings` to treat that space as air.
- `ChunkData.version` changes on every edit. `ChunkId` is generational, so a reloaded chunk has a new id.
- The world writes `ChunkMap` only in `Update`. Physics in the fixed schedules only reads it, so there is no conflict and the streaming budgets are not affected.
- Edits to chunks that are not Ready are dropped.
- There is no BlockChanged message and no chunk-loaded message.

**Tests and ECS**
- `tests/ambiguity.rs` makes First, PreUpdate, Update, PostUpdate and Last strict. FixedPostUpdate and RunFixedMainLoop are not covered.
- The ECS fork has `#[require]` and `par_iter_mut`.
- ComputeTaskPool has 7 threads (of 16), separate from the 6 async chunk workers.

**Other**
- `boxel_math::Aabb` is f32 and cannot be reused.
- `Occupancy` is documented as "base for physics", but nothing maintains it yet. v1 does not need it.
- Both cost anchors hold:
  - paletted `get` costs 0.91 ns (criterion get_random_1024 = 914 ns);
  - a ChunkMap lookup costs 6.5 ns (architecture §8).
- `OverlayDraw` has a single producer: the F3 screen replaces its rects. A crosshair needs a small change.
- `WorldPlugin` is in `DefaultPlugins`. The facade's default features are window, render, voxel, png and debug.

**Fixes to A**
- No substeps are needed if each axis scans layers.
- `normalize` must carry when `frac - floor(frac)` rounds up to 1.0 (a tiny negative frac).
- Waking on version change alone misses reloads.
- Interpolating in PostUpdate makes Update see tick-end values in some frames and stale ones in others.

**Fixes to B**
- Per-axis clipping with a strict footprint does not snag on seams.
- P0 does not need the world messages until sleeping (P4).

## 4. Implementation plan

### 4.1 Crate, dependency graph, facade

**The crate**
- New crate `crates/boxel_physics`, with no third-party physics.
- Depends on boxel_ecs, boxel_app, boxel_math, boxel_transform, boxel_voxel, boxel_world, boxel_diagnostic and foldhash.
- Dev dependencies: proptest, criterion, boxel_worldgen.

**Place in the graph:** above boxel_world, next to boxel_render. Neither boxel_world nor boxel_render ever depends on it.

**Layout**
- `core/` is pure (glam and boxel_voxel only, testable without an App, like VoxelRenderer's core). Modules: `shape`, `source`, `sweep`, `ray`, `narrow`, `grid`.
- The ECS layer has `plugin`, `components`, `systems`, `query`, `controller`, `stats`.

**Facade**
- Feature `physics = ["voxel", "dep:boxel_physics"]`, on by default.
- `pub use boxel_physics as physics`, and the prelude re-exports `boxel_physics::prelude::*`.
- `trace` forwards `boxel_physics?/trace`.
- `PhysicsPlugin` is not in `DefaultPlugins`; games add it.

**Determinism guard:** `crates/boxel_physics/clippy.toml` disallows `mul_add`, `powf`, `powi`, `exp`, `ln`, `sin` and `cos`, for f64 and for glam types.

**Docs**
- ADR 0015 "física mínima propia".
- architecture.md: remove física from the non-goals, and add the crate to the crate map and the layer diagram.
- P1 closes M1's "raycast y edición".

### 4.2 Small changes outside the crate (P0)

**boxel_voxel::coords: `WorldPos { block: BlockPos, frac: DVec3 }`**, with `frac` in [0,1).
- Constructors and conversions: `from_dvec3`, `from_block(BlockPos, DVec3)`, and `to_dvec3`, which is the only rounding.
- Arithmetic: `offset(DVec3)`, and `delta_to(&WorldPos) -> DVec3`, which takes the i64 difference, converts it to f64 and adds the frac difference.
- `normalize`, with the carry when the result is 1.0.
- The block is clamped to ±`MAX_BLOCK_COORD`.
- Far-coords tests.

**boxel_app**
- `RunFixedMainLoopSystems { BeforeFixedMainLoop, FixedMainLoop, AfterFixedMainLoop }`, chained (Bevy's names). `run_fixed_main_loop` goes in FixedMainLoop.
- `FixedTime::set_max_steps_per_frame(n)` with n ≥ 1.

**boxel_world:** nothing until P4.

### 4.3 Core types (pure)

```
Shape::{Aabb { half: DVec3 }, Sphere { radius: f64 }}
LocalBox { min: DVec3, max: DVec3 }            // f64, relative to a body's block
CollisionShape::{Empty, Full}                  // Boxes(..) reserved for slabs and stairs
CollisionTable                                 // per BlockState, from StateFlags SOLID and FLUID
trait VoxelSource { fn probe(&mut self, p: BlockPos) -> Probe }   // Probe::{Block(BlockState), Unloaded}
sweep::move_box(src, table, origin: BlockPos, bx: LocalBox, d: DVec3, obstacles: &[LocalBox], cfg)
    -> Result<SweepOut { delta, faces: u8, ground: Option<BlockPos>, in_fluid, depenetrated }, Unloaded>
ray::voxels(src, table, origin: WorldPos, dir: DVec3, max_t, filter) -> RayResult
narrow::{box_box, sphere_sphere, sphere_box} -> Option<Contact { normal, depth }>; ray_box, ray_sphere
grid::Grid                                     // sorted uniform grid
```

**`ChunkSource<'a>`** (in the ECS layer) wraps `&ChunkMap` plus `WorldSettings::vertical_bounds`:

| Chunk state | Reader returns |
|---|---|
| Ready | the block (Uniform chunks need no decode) |
| Missing | air |
| outside the vertical bounds | air |
| Queued, Generating, or no slot | Unloaded |

It caches the last (ChunkPos, entry). That costs one 6.5 ns lookup per chunk touched, then about 1 ns per block. `TestWorld` (a block map plus a set of unloaded chunks) is used by tests and benches.

### 4.4 ECS API

**Components**

| Component | Purpose |
|---|---|
| `RigidBody::{Dynamic, Kinematic, Static}` | Body kind. `#[require(Velocity, BodyPosition, Contacts, BodySettings, PhysicsSync)]`. A `Collider` without a `RigidBody` is Static. |
| `Collider { shape, offset: DVec3, response: Response::{Solid, Sensor}, layers: CollisionLayers { member: u32, mask: u32 } }` | The shape and how it reacts. The origin is at the feet: a player is Aabb half (0.3, 0.9, 0.3) with offset (0, 0.9, 0). |
| `Velocity(DVec3)` | Blocks per second. Gameplay writes it. |
| `BodyPosition(WorldPos)` | The authoritative position. |
| `BodySettings { gravity_scale, drag_xz, drag_y, ground_friction, restitution, bounce_min_speed, push_weight }` | Tuning, with presets `player()`, `item()`, `arrow()`, `ball()`. One component instead of five. |
| `Contacts { faces: u8, on_ground, ground_block: Option<BlockPos>, in_fluid }` | Output, rewritten every tick. Face bit order is the frozen Face order: +Y, -Y, +X, -X, +Z, -Z. |
| `Projectile { on_block: Stop \| Bounce, on_entity: Stop \| Pass, ignore: Option<Entity> }` | Projectile mode (P4). |
| `Frozen` | Marker that physics adds and removes. |
| `Sleeping` | Marker (P4). |
| `Interpolated` | Opt-in. Holds the previous position. |
| `ReportContacts` | Solid pairs also emit events. |
| `Teleport(WorldPos)` | Exact teleport, consumed in Prepare. |
| `CharacterController { walk_speed, air_control, jump_speed }`, `MoveIntent { wish: Vec2, jump: bool }` | Optional controller (P2). `jump` is a latch. |

**Resources**
- `PhysicsSettings`. Defaults:

  | Setting | Default |
  |---|---|
  | gravity | (0, -32, 0) |
  | eps | 1e-7 |
  | max_speed | 100 b/s |
  | solver_passes | 2 |
  | push_axes | All |
  | max_depenetration | 1 block per tick |
  | impact_speed | 4 b/s |
  | grid_shift | 2 |
  | parallel_threshold | 256 |
- `CollisionTable`, built in `Plugin::finish` from `WorldContent.registry`. Without WorldPlugin it panics with a clear message.
- `BodyIndex`, the grid snapshot used by queries.
- `PhysicsStats`, which feeds a "physics" line on DiagnosticsBoard in Last.

**Messages**
- `CollisionStarted { a, b }` and `CollisionEnded { a, b }`, with a < b by Entity bits. Sent for sensor pairs, and for solid pairs that carry `ReportContacts`.
- `VoxelImpact { entity, face, block, speed }`.
- `ProjectileHit { projectile, target: Block { pos, face, state } | Entity(e), point: WorldPos, speed }`.

**SystemParam `PhysicsQuery`**
- `raycast_voxels(origin, dir, max_t, VoxelFilter)` (P1).
- `raycast(origin, dir, max_t, &RayFilter) -> Option<RayHit>`, covering voxels and entities (P3).
- `overlap_box(WorldPos, LocalBox, layers)` (P3).
- `block_is_free(BlockPos)`, the placement check (P3).
- `predict(pos, vel, settings, ticks)`: the same integrator without voxels, for exact aim previews.

### 4.5 Schedule and ordering

```
First, PreUpdate (input)
RunFixedMainLoop
  BeforeFixedMainLoop: the game samples input into MoveIntent (jump latch) and does mouse look
  FixedMainLoop, 0..max_steps times:
    FixedUpdate: gameplay, AI and the controller write Velocity (they read BodyPosition and Contacts)
    FixedPostUpdate: PhysicsSystems::Prepare -> Kinematic -> Step -> Broadphase -> Solve -> Events -> Writeback (chained)
  AfterFixedMainLoop: PhysicsSystems::Interpolate
Update: read physics messages; camera follow .before(WorldSystems::Interest);
        picking + EditQueue .before(WorldSystems::Edits); world streaming
PostUpdate, Last (physics board line), Render (extract reads Transform)
```

What each physics set does:

| Set | Work |
|---|---|
| Prepare | New bodies split their Transform into a `BodyPosition`. Teleports are detected (see 4.6 g). Interpolated bodies snapshot their previous position. |
| Kinematic | Moves kinematic bodies by their velocity, ignoring voxels. Rebuilds the obstacle index of static and kinematic solids. |
| Step | Moves Dynamic bodies (4.6 a to c). |
| Broadphase | Rebuilds the grid. |
| Solve | Pushes solid pairs apart and builds the sensor overlap set. |
| Events | Writes the sorted messages. |
| Writeback | Sets `Transform.translation` and `PhysicsSync`. |

Gameplay rule: move bodies through `Velocity`. Writing Transform counts as a teleport.

### 4.6 Algorithms

**a. Integration.** Per Dynamic body, per tick, with dt = `timestep().as_secs_f64()` (2^-6 at 64 Hz):
1. `v += g * gravity_scale * dt`.
2. Drag:
   - `v.y *= 1/(1 + drag_y*dt)`;
   - `v.xz *= 1/(1 + k*dt)`, where k is `ground_friction` if the body was on the ground last tick and `drag_xz` otherwise.

   This form is implicit and stable, and uses only multiply and divide.
3. Clamp each axis to `max_speed` (projectiles are exempt).
4. Sweep with `d = v*dt`.
5. On each clipped axis:
   - if |v| > `bounce_min_speed`, `v = -v*restitution`, otherwise `v = 0`;
   - send `VoxelImpact` if the speed is above `impact_speed`.

   As in Minecraft, the rest of the tick after a bounce is dropped.

Default values (Minecraft per-tick factors converted to per-second rates):

| Value | Default | Minecraft source |
|---|---|---|
| Gravity | 32 b/s² | 0.08 per tick² |
| drag_y | 0.4 /s | 0.98 per tick; gives about 80 b/s terminal fall |
| Player drag_xz | 1.9 /s | 0.91 per tick |
| Player ground_friction | 12 /s | 0.546 per tick |
| Jump speed | 9 b/s | apex about 1.25 blocks |
| Arrow | gravity_scale 0.625, drag 0.2 /s, launch about 60 b/s | |

**b. Body vs voxels.** This is Minecraft's per-axis clip, done in the body's frame. The frame's origin is `BodyPosition.block`, so every block box is a small exact integer box.
- **Depenetration first.** If the box overlaps a Full block by more than eps, move it toward the nearest free side within 1 block (tie order +Y, -Y, +X, -X, +Z, -Z), at most 1 block per tick. If there is none, move +Y.
- **Axis order.** Y first, then whichever of X and Z has the larger |d| (X on a tie). This order is frozen by tests.
- **Per axis, scan layers.** Start at the leading face and go out to the layer the moved face reaches.
  - The footprint is `floor(min+eps) ..= ceil(max-eps)-1` on the other two axes. Blocks that only touch the box are never in it, so nothing snags at block or chunk borders.
  - The first layer that has a Full block, or an obstacle box, clips d.
  - The cost grows linearly with |d|, so there are no substeps and no gathers that grow with d³.
- **Snapping.** After a clip, `pos = plane - face_offset`. Repeating it gives the same value, so resting bodies never drift. With the origin at the feet, a body lands at exactly integer y.
- **Contacts.**
  - A face bit for each clipped axis.
  - `on_ground` when Y was clipped moving down.
  - `ground_block` is the block under the box center.
  - `in_fluid` if any overlapped block has the FLUID flag.
- **Accepted artifacts.** Diagonal moves can cut an outer corner by up to one tick of movement, as in Minecraft. Spheres use their bounding box against voxels.

**c. Unloaded chunks: freeze.** If any probe in a body's tick is Unloaded:
- the tick is undone for that body (position, velocity, no gravity);
- `Frozen` is added;
- it retries next tick, which costs one lookup.

Near the player nothing freezes, because the data region extends one chunk past the mesh radius. Mobs at the edge stop.

**d. Body vs body.**
- **Broadphase.** A sorted uniform grid with 4-block cells and i64 keys.
  - Entries are sorted with `sort_unstable`.
  - Colliders wider than 2 cells go to a `large` list.
  - Each pair is tested only in its home cell, the one at `max(minA, minB)`.
  - Layers are checked both ways.
  - Pairs are sorted by Entity, and buffers are reused.
- **Narrowphase.** Done in f64 on the relative vector from `a.delta_to(b)`, which is exact at any distance.

  | Pair | Separation |
  |---|---|
  | box/box | the axis of least penetration (ties Y, X, Z) |
  | sphere/sphere | along the line between centers (+Y if they coincide) |
  | sphere/box | from the closest point (least-penetration face if the center is inside) |
- **Response.**
  - **Sensor on either side.** No push. The pair joins the overlap set (touching does not count). A Dynamic body with a Sensor collider still hits voxels, which is how dropped items work.
  - **Solid vs solid, both Dynamic.** They are moved apart along the normal and the push is split by `push_weight`.
    - A body already blocked on the face it would be pushed toward counts as immovable for that pair, so stacks rest without sinking.
    - The pushes go through the voxel sweep, so walls always win.
    - The approaching normal velocity is removed.
    - An upward push sets `on_ground`, so you can stand on a crate or a mob.
    - There are 2 passes. Any overlap left over is fixed in later ticks.
  - **Static and Kinematic solids.** They are obstacles inside the sweep. A kinematic body moving into a dynamic one pushes it the whole way.
- **Events.** The current pairs are kept as a sorted Vec. A merge-diff with last tick's list gives Started and Ended, each once. A despawned entity gives Ended on the next tick.

**e. Projectiles.**
- Against voxels: a DDA over the tick's segment from the center point. No tunneling at any speed.
- Against entities: the segment is tested against targets inflated by the projectile's half extents. Box plus box is exact; spheres use their inflated bounding box. The shooter is skipped through `ignore`.
- The earliest hit sends `ProjectileHit`.
  - Stop: v = 0.
  - Bounce: the hit axis is reflected and scaled by restitution.
  - Pass: the projectile keeps going, with one hit per target.

**f. Raycast.** Amanatides-Woo DDA from a `WorldPos` origin, stepping in i64.
- **Exactness.** Each axis keeps an integer boundary count k. Every step recomputes `t = (k - frac) * inv` instead of accumulating it, so hits do not depend on distance or on where the ray starts.
- **Ties.** Step x, then y, then z. The entered face is the opposite of the step. A ray that starts inside a matching block reports face None at t = 0.
- **Filters.**
  - Collision: Full blocks.
  - Outline: RenderShape is not None, fluids optional. Used for picking.
  - Custom: any closure on (BlockPos, BlockState).
- **Results.**
  - `Hit { block, face, t, point, state }`;
  - `Miss`;
  - `Unloaded { block, t }`.
- **Speed.** One chunk lookup per chunk crossed. Uniform chunks need no decode. Jumping across all-air chunks is added only if the sky_1024 bench needs it, and it must produce the same voxel sequence (tested).
- **Entity ray.** A DDA over grid cells. The combined raycast is limited to the voxel hit's t.

**g. Interpolation and teleports.**
- **Interpolation.** In AfterFixedMainLoop, interpolated bodies get `Transform = prev.to_dvec3() + prev.delta_to(curr) * alpha`, with alpha = accumulator / timestep computed in f64. Update and render both see the interpolated position. It adds up to 15.6 ms of latency on translation only.
- **Teleport detection.** A Transform different from `PhysicsSync.last_written` means gameplay moved the body. Physics re-splits it exactly and resets the previous position. Beyond 2^53, use `Teleport(WorldPos)`.
- **Rotation.** Physics never writes `Transform.rotation`.

**h. Sleeping and parallel step (P4).**
- **Going to sleep.** A body sleeps when it is on the ground or resting on another body, |v| < 0.05 b/s for 32 ticks, and nothing pushes it.
- **Waking.** On:
  - a Velocity write;
  - a push;
  - a teleport;
  - a `BlockChanged` within its bounds plus 1 block (found through the grid);
  - a load or unload of its chunk.

  These two messages go in boxel_world and are shared with lighting, which needs both. Agree on their shape before either merges. Polling `ChunkData.version` alone would miss reloads.
- **Parallel step.** Above 256 awake bodies, the step uses `par_iter_mut`. Each body writes only itself and has its own chunk cache, so the result is identical to the serial run.

### 4.7 Precision and determinism rules

- **Position.** `BodyPosition` is authoritative. Transform is a view with one rounding that affects rendering only. Results are bit-identical under any integer shift, up to 2^61.
- **Math.** The step uses only + - × ÷, sqrt, floor, abs, min and max, enforced by clippy.toml.
- **Time step.** dt is f64 from FixedTime. Keep power-of-two rates.
- **Order.** Pairs, pushes and events run in sorted Entity order. Hash maps use foldhash FixedState and are never iterated for order. Parallel work is only per-body.
- **Limit.** How many ticks a body spends Frozen depends on when chunks arrive, so identical runs across machines need a settled world (InlineExecutor, as the tests use).

### 4.8 Tests

All of these run later through tools/quiet.sh.

**Core tests (no App)**
- **Sweep units.**
  - Bodies land exactly on integer y.
  - The axis order rule.
  - No snag when sliding across block and chunk borders, in 6 directions and at negative coordinates.
  - 10,000 resting ticks stay bit-identical.
  - Bounces decay and stop.
  - Depenetration goes in the right direction, with +Y winning ties.
  - Unloaded leaves the state unchanged. Missing and outside `vertical_bounds` count as air.
- **`tests/sweep.rs` proptest.** Random fields with 0 to 60% solid, random box sizes, starts that do not overlap, up to 50 blocks per tick. Checks:
  - never ends overlapping by more than eps;
  - each axis segment's swept box never strictly overlaps a Full block;
  - the face bits match an independent check (box moved 2·eps toward the face);
  - per axis, |moved| ≤ |d| with the same sign.
- **`tests/raycast.rs`.**
  - A brute-force slab oracle on random rays.
  - Hand-written cases: axis-aligned rays, rays exactly through edges and corners, zero direction components, a start inside a solid, max_t boundaries, Unloaded.
  - The uniform-chunk path matches the generic path.
- **`tests/shapes.rs` and `tests/grid.rs`.** Analytic ray tests and MTV checks. The grid is checked by proptest against brute-force O(n²).
- **`tests/far_coords.rs`.** Scenes shifted by 0, ±2^31, 2^40, 2^52 and ±(2^61 - 64) per axis give bit-identical results. Covers `WorldPos` round trips and the `normalize` carry.

**ECS tests (`tests/plugin.rs`)**

Setup: a headless App with WorldPlugin and InlineExecutor. Far-coordinate cases use a shifted test generator.
- **Falling.** A body dropped 40 blocks lands at the exact surface y. It gets one `VoxelImpact`, whose speed equals a reference run of the integrator.
- **Freeze.** A body over a Queued column stays still and Frozen, then falls once the chunk is Ready.
- **Edits.** A block placed into a body pushes it out. Removing the block under a body makes it fall.
- **Teleport and interpolation.** Both teleport paths work. The interpolated position stays between prev and curr, with no smear on teleport.
- **Sensors.** Started and Ended arrive once each. Ended also arrives after a despawn.
- **Solid bodies.** They separate without entering walls, and a box stacked on another box rests.
- **Messages.** Messages written in FixedPostUpdate can all be read in Update at 144 fps over 64 Hz.
- **Jump latch.** A press during a frame with 0 ticks still jumps once. A frame with 2 ticks jumps once.
- **Determinism.** Two Apps give the same state hash after 1,000 ticks. Parallel equals serial. Interpolation on or off gives the same hash.
- **Ambiguity.** `tests/ambiguity.rs` adds PhysicsPlugin and makes FixedPostUpdate and RunFixedMainLoop strict.

### 4.9 Benches and acceptance numbers

Criterion, in `crates/boxel_physics/benches/physics.rs`, on the dev laptop (single thread unless noted).

| Bench | Target |
|---|---|
| tick, 0 bodies | ≤ 20 µs |
| tick, 100 awake bodies on forest terrain | ≤ 0.05 ms |
| tick, 1,000 awake | ≤ 0.5 ms |
| tick, 10,000, 90% asleep | ≤ 0.6 ms |
| tick, 10,000 awake, parallel Step | ≤ 1.5 ms |
| tick, 1,000 projectiles | ≤ 0.3 ms |
| sweep, one player-size body | ≤ 0.3 µs |
| raycast pick_6 / cave_256 / sky_1024 | ≤ 0.5 / 5 / 10 µs |
| grid build + pairs, 10,000 | ≤ 0.3 ms |

Estimate behind these: 25 to 35 probes at about 1 ns, plus a few 6.5 ns lookups, plus the clip math, gives 0.1 to 0.3 µs per body.

**Frame rule.** Measured with the stress recipes at r16 and r32, scenarios static, fly and dive, on both GPUs:
- `--bodies 1000` and `--bodies 10000` (90% asleep) must add no frame over 6.94 ms compared with the same run without bodies, and must raise p99 by at most 0.3 ms.
- p99 and max are the metric, because a tick lands in about 1 of every 2.25 frames.
- Games set `max_steps_per_frame` to 3.

### 4.10 Examples

**voxel_world, P1, no bodies: block picking and editing.**
- Left click breaks while the cursor is grabbed. Right click places. Keys 1 to 9 pick the block.
- Uses `raycast_voxels` with the Outline filter in Update, before `WorldSystems::Edits`.
- F3 shows the target block and face.
- A crosshair needs `OverlayDraw` to accept a second producer (a small render change), or it can be skipped.

**New physics_sandbox (P2 to P4).**

| Input | What it does |
|---|---|
| Mouse, WASD, Space | First-person player: box 0.6 × 1.8 × 0.6, eye at 1.62, Interpolated, CharacterController |
| G | Toggle fly |
| Clicks | Break and place blocks, with the placement check |
| Q | Drop an item (Dynamic + Sensor, picked up on CollisionStarted) |
| F | Fire an arrow (sticks in blocks, knocks back a mob) |
| B | Throw a bouncy ball (sphere, restitution 0.6) |
| M | Spawn wandering dummy mobs (solid boxes, seeded intents) |

- F3 shows a physics line.
- Automation: `--frames N`, and `--drop-test` (100 items dropped from 40 blocks must all settle, or exit 3).

**stress:** new options `--bodies N --projectiles M --body-kind item|mob`.

### 4.11 Phases

- **P0, prerequisites.**
  - Work: `WorldPos`, `RunFixedMainLoopSystems`, the max-steps setter, ADR 0015, architecture.md updates, the facade feature.
  - Exit: the far-coords tests for `WorldPos` and the ambiguity test pass.
- **P1, core and raycast.**
  - Work: `ChunkSource`, `CollisionTable`, the sweep, the DDA, the shape tests, `PhysicsQuery::raycast_voxels`, picking in voxel_world.
  - Exit: the proptests and far-coords tests pass, and the sweep and raycast benches meet their targets.
- **P2, bodies.**
  - Work: plugin, components, gravity and drag, restitution, freeze, `VoxelImpact`, interpolation, teleports, controller with the jump latch, the sandbox (walk, jump, drop items), the F3 line.
  - Exit: the ECS tests pass, and a tick with 1,000 bodies takes ≤ 0.5 ms.
- **P3, body vs body.**
  - Work: grid, narrowphase, solid pushes, sensors and events, layers, static and kinematic obstacles, entity raycast, `overlap_box`, the placement check.
  - Exit: the grid proptest passes, the stacking and sensor tests pass, and the grid bench meets its target.
- **P4, projectiles and scale.**
  - Work: projectiles, sleeping with the shared world messages, the parallel step, the stress runs, results in docs/rendimiento.
  - Exit: an arrow at 60 b/s hits a 0.6-wide mob, the 10k benches pass, and the frame rule holds on both GPUs.
- **P5, on demand.**
  - Step-up (done: ADR 0016, section 9c).
  - Non-full shapes through `BlockDef.collision`.
  - Fluids.
  - Per-block friction.
  - Carrying riders.
  - Exact sphere vs voxels.
  - Simulation distance for mobs.
  - `cast_box`.

### 4.12 Owner decisions (recommended option first)

1. **Unloaded chunks:** freeze bodies until the chunk loads / treat them as solid / treat them as air.
2. **Position:** `WorldPos` is authoritative and Transform is a view / Transform only (contacts degrade past about 2^29).
3. **Tick rate:** 64 Hz with interpolation / 128 Hz (half the latency, double the cost).
4. **Gravity:** 32 b/s² for a Minecraft feel / 9.81.
5. **Solid push:** by position, all axes, stacking allowed / Minecraft's soft horizontal-only push.
6. **Spheres against voxels:** bounding box in v1 / exact now.
7. **Character controller:** optional in boxel_physics (enemies reuse it) / only in game code.
8. **`BlockDef.collision`:** derive it from `solid` for now, and add the field in the same change as colored block light, so the struct breaks once / add it now.
9. **Packaging:** the crate is on by default, the plugin is opt-in / the plugin is in DefaultPlugins.
10. **Max fixed steps per frame with physics:** 3 / keep 8.
11. **Mobs beyond the view:** freeze at the edge / add a data-only simulation distance later.
12. **Writing Transform on a body:** treated as a teleport / ignored with a warning.

### 4.13 Risks

- **Two sources of position.** Gameplay that writes Transform every frame fights the simulation. Mitigation: documentation, plus the teleport rule.
- **Input in fixed ticks.** Input read inside fixed ticks can be lost or doubled. MoveIntent sampled in BeforeFixedMainLoop must be the documented pattern.
- **Minecraft artifacts.** Corner cutting and results that depend on axis order. Frozen in tests.
- **Crowds against walls.** They can overlap briefly. Strict no-overlap would need an iterative solver.
- **Catch-up after a hitch.** Many ticks in one frame. Mitigation: 3 max steps, sleeping, parallel step, the stress gate.
- **Frozen mobs.** Mobs at the edge stay frozen, and there is no entity persistence yet (ChunkStore is pending).
- **Determinism leaks.** For example a stray `mul_add` or HashMap iteration order. Guarded by clippy.toml and the hash tests.
- **Shared world messages.** The messages shared with lighting must be agreed before P4. The `BlockDef` break must be coordinated with the colored-light change.