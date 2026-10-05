# boxel M0 scaffold specification (final)

Audience: the implementation agents that build milestone M0, and the lead that runs the phase
gates. Language: English (code, identifiers and rustdoc are English; the human-facing
architecture docs are Spanish). Date: 2026-10-05. Toolchain: Rust 1.98.1 stable.
Repository root: `/home/ezequiel/Documentos/projects/boxel` (called `$REPO`).

This file is the single source of truth. It is self-contained: you do not need the research
notes. Companion material ships next to it and is copied into the repository by WP0 under
`.scaffold/design/` (so nothing depends on the `/tmp` scratchpad, which is a tmpfs):

- `ARCHITECTURE.md` (Spanish): the why. Becomes `docs/architecture.md`.
- `adr/0001..0012` (Spanish): one decision each. Become `docs/adr/`.
- `skeleton/`: **the compile-checked M0 skeleton** (every crate, every frozen public signature,
  manifests, tooling, refork tooling, the `.scaffold/` helpers). WP0 copies it verbatim.
- `reference/renderprobe/`: the architect's validated render probe (three tier submission paths,
  bgm-to-PackedQuad conversion with a brute-force oracle, readback, image comparison). It
  predates two decisions of this spec (the 8-bucket layout with a cutout bucket, and
  `VoxelRenderer::new` taking device parts): where it differs, this spec wins.
- `reference/hidden_window_probe.rs`: the Wayland presentation probe behind section 7.6.

Skeleton verification (run by the architect on 2026-10-05 on a scratch copy, all green):
`CARGO_BUILD_WARNINGS=deny cargo check --workspace --all-targets --all-features --locked`;
`cargo test --workspace --locked` (42 test targets, 39 tests passed, 0 failed, 2 ignored stubs
owned by WP3/WP7; the GPU contract tests also passed on RADV, RTX 5050, lavapipe, NVIDIA GL 3.3
and llvmpipe GL);
`cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`;
`CARGO_BUILD_WARNINGS=deny cargo doc --workspace --no-deps --all-features --locked`;
`cargo fmt --check`; `cargo deny --all-features check`; `python3 tools/ecs_surface.py`;
`python3 tools/no_bevy.py`; `python3 tools/vendor_lock_sync.py`;
`cargo +1.97.1 check --workspace --all-targets --all-features --locked` (MSRV);
`tools/refork/refork.sh` (1.2 s offline) followed by `cargo test --workspace --lib --locked`
(961 passed) and `cargo test -p boxel_ecs --doc --features debug --locked` (480 passed) inside
`vendor/`.

---

## 0. Rules for every package

1. **Normative sources, in order:** this spec, then the skeleton source (doc comments in the
   skeleton are part of the contract: many `[P]` rules are written there), then
   `ARCHITECTURE.md`.
2. **Notation.** `[S]` = implemented for real by the skeleton and covered by its contract tests
   (`tests/contract.rs`, `tests/naga.rs`, `tests/derives.rs`, `tests/gpu_contract.rs`): frozen
   behaviour, do not rewrite it. `[P]` = `todo!("WPn")` in the skeleton, implemented by package n.
   Two `[S]` items are *baselines* whose observable behaviour is frozen but whose representation
   the owner improves: `ChunkData::set` and `ChunkData::from_dense` (WP4).
3. **Frozen API.** Every `pub` item that exists in the skeleton keeps its name, signature, fields,
   derives and trait impls. You may add private items, private modules, and *new* `pub` items in
   crates you own (additive only). During a phase you may only USE items of other crates that
   exist in the skeleton (your copy only has the skeleton versions of other crates anyway).
4. **Deviation protocol.** If a frozen item is wrong or impossible to implement: keep it
   compiling, implement the closest correct behaviour, and add an entry to YOUR file
   `.scaffold/deviations/<wp>.md` (English: item, problem, what you did, what the lead must
   decide). If the defect blocks you or other packages, start the entry with `BLOCKING:` and
   tell the lead in your final report; continue with the parts you can do. Only the lead
   changes non-owned files (section 11.2).
5. **Ownership.** Each package owns the paths of its row in `.scaffold/owners.tsv` (section
   11.1) plus `.scaffold/deviations/<wp>.md`. Never edit anything else, not even to fix a
   typo. Read anything. The lead's gate (`scaffold.py gate`) rejects packages that changed
   non-owned paths.
6. **Shared files.** Every `Cargo.toml`, `Cargo.lock`, `vendor/`, tooling files, `docs/` and
   `.scaffold/` (except your deviations file) belong to WP0, then to the lead, then to WP10.
   Build with `--locked` and never add dependencies. Need one? Record a deviation.
7. **Skeleton attribute.** Each crate root with `[P]` items starts with
   `#![allow(unused_variables, unused_mut, dead_code, clippy::todo, reason = "skeleton stubs")]`
   preceded by `// SKELETON: remove this attribute when the crate is fully implemented (owner: WPn).`
   Example stubs carry `#![allow(clippy::todo, reason = "skeleton stub")]` with the same marker.
   The owner removes both lines when done; no `todo!()` may remain in owned code.
8. **Tests that need another package's `[P]` code** are written anyway, in an integration test
   file of your crate, and marked `#[ignore = "integration: needs WPn"]`. WP9 or WP10 removes the
   ignores.
9. **Private working copy.** Every parallel package works in its own copy of the repository,
   created by the lead with `.scaffold/bin/scaffold.py start <wp>` at
   `/home/ezequiel/.cache/boxel-agents/<wp>/ws`, with its own target dir
   `/home/ezequiel/.cache/boxel-agents/<wp>/target` (seeded from a prebuilt target, so the first
   build is incremental) and `CARGO_BUILD_JOBS=4`. Run every command from your `ws` directory.
   Never edit `$REPO` directly. `/tmp` is a 12 GB tmpfs with a quota: never put target dirs there.
10. **Style.** Format only what you own: `cargo fmt -p <owned crate>`; root examples with
    `rustfmt --edition 2024 examples/<file>.rs`. Never `cargo fmt --all` (it also formats
    `vendor/`, which must stay byte-identical to the refork output). Lint only your crates:
    `cargo clippy -p <crate> --all-targets --all-features --no-deps --locked -- -D warnings`
    (and `--example <name>` for owned examples). No `unsafe` (only `boxel_gfx::hal`). Library
    errors with `thiserror`; no `anyhow` in libraries. Logging with `tracing` macros, never
    `println!` in libraries (examples use `#![expect(clippy::print_stdout, clippy::print_stderr,
    reason = "CLI output")]`). No `std::thread::sleep` outside runners (clippy
    `disallowed-methods`). Do not use long dashes in any text you write.
11. **Version control.** Do not run `git init`, commit or push. The lead may create a local git
    baseline only with Ezequiel's explicit approval (section 11.2, preflight).
12. **ECS surface.** Engine crates and our examples may only use these `boxel_ecs` modules:
    `prelude`, `component`, `entity`, `message`, `query`, `resource`, `schedule`, `system`,
    `world`, `change_detection`, `lifecycle`, `intern`, `label`, `error`, `bundle`
    (`tools/ecs_surface.py`, which also catches grouped imports and `boxel::ecs::` paths). In
    `ParamSet`, write `&'static` explicitly: `ParamSet<(Query<&'static mut A>, Query<&'static B>)>`
    (the elided form fails with E0477 "the type `&mut A` does not fulfill the required
    lifetime").
13. **Windows on the desktop.** Windowed runs open on Ezequiel's live Wayland session. Use the
    automation flags of section 9 (`--frames`, `--timeout`), never leave a window open, and never
    assume a human can press keys.
14. **Done** for any package means: owned code formatted, no skeleton attribute, no `todo!()`,
    clippy clean for owned targets, `CARGO_BUILD_WARNINGS=deny cargo doc -p <owned crates>
    --no-deps --locked` clean, owned tests green, deviations recorded, `scaffold.py check <wp>`
    reports 0 paths outside ownership.

---

## 1. M0 scope

### In scope (all must work at the end of M0, subject to the cut list)

- Workspace, tooling, CI definition, licenses, NOTICE.
- `vendor/`: stripped fork of bevy_ecs 0.20.0-rc.2 + backport of bevy#25940, generated by
  `tools/refork/refork.sh`, with its committed `vendor/Cargo.lock`.
- `boxel_app`: App, plugins, plugin groups, main/startup/fixed/shutdown schedules, time (with the
  fixed-step view in `FixedMain`), messages that survive until a fixed step, task pools,
  headless runner.
- `boxel_log`, `boxel_diagnostic`, `boxel_math`, `boxel_transform`.
- `boxel_window`: winit 0.30 runner (frame-callback presentation gate, redraw watchdog, ordered
  shutdown), non-send primary window, window messages, keyboard/mouse input, cursor grab.
- `boxel_camera`: camera-relative view math, fly camera.
- `boxel_gfx`: complete runtime detection: preferences and overrides (CLI, env, config file),
  instance creation with `catch_unwind` (and the `panic = "abort"` path), classification,
  scoring, smoke tests, selection with fallbacks and per-adapter surface format, crash-only
  last-known-good, device-lost health flags, surface helpers, readback, `hal` escape hatch.
- `boxel_shaders`: the five WGSL files, GPU mirror types, naga tests on every backend.
- `boxel_render`: **all three tier paths fully implemented**:
  - High: GPU frustum + face-bucket culling in compute, two compacted indirect lists (opaque,
    cutout) + `DrawRef`, one `multi_draw_indexed_indirect_count` per list. (Hi-Z is M2.)
  - Mid: CPU culling, `DrawRef` buffer, `draw_indexed` per visible bucket.
  - Low: CPU culling, instanced triangle strips, chunk origin in immediates.
  - Shared: opaque pipeline without `discard` + cutout pipeline with alpha test, quad arena with
    offset-allocator, chunk table, procedural block texture array with mips, view uniform,
    main color + depth targets, blit, screenshots, render plugin with GPU bootstrap, extract
    boundary, mesh extraction, device-loss recovery, LKG confirmation.
- `boxel_voxel`, `boxel_worldgen`, `boxel_mesh`, `boxel_world`: coordinates, registry, paletted
  chunks, terrain generator, binary greedy mesher to PackedQuad (8 buckets), chunk streaming with
  prioritized jobs (exactly one result per job), edits, mesh events, `ChunkStore` trait.
- Facade `boxel` with `prelude`, `DefaultPlugins`, `MinimalPlugins`; `boxel_facade_check`.
- Examples `gpu_info` (in `boxel_gfx`), `window_smoke` (in `boxel_window`), `voxel_world`,
  `headless_world`; GPU tests `gpu_contract`, `gpu_select`, `gpu_tiers`, `gpu_terrain`.

### Out of scope (later milestones, ARCHITECTURE.md section 15)

Hi-Z occlusion, translucent pass (the translucent bucket is meshed and uploaded but not drawn),
interior faces between adjacent cutout blocks ("fancy leaves"), ambient occlusion, LOD, real
persistence (redb), block properties and id maps, raycast/editing UI, pipelined rendering /
render sub-app, async GPU init, WESL, GPU timestamps, dynamic linking, multiple windows,
web/mobile, `static-dxc`, GL clip control.

### Cut list (priority order)

If a package runs out of budget, cut in this order and record the cut in its deviations file.
Nothing else may be cut without the lead.

1. **Runtime device-loss re-selection** (WP8). Keep `GpuHealth`, the log line, `GpuFailed` and a
   clean exit with code 2; re-selection moves to M1. Section 12 row 13 then expects exit 2.
2. **Edits system** (WP6). Keep `EditQueue` (public API); the system moves to M1 with raycasts.
3. **Bench targets**: numbers are informational in M0 (measured on a loaded machine).
4. **Windows/macOS CI jobs**: defined but not blocking (they cannot run before the first push).

### M0 definition of done

1. `cargo build --workspace --all-targets --all-features --locked`, `just lint` and `just doc`
   pass with `CARGO_BUILD_WARNINGS=deny`.
2. `just ci` passes (with typos installed, or `BOXEL_SKIP_TYPOS=1` stated in the final report);
   `just test-lavapipe`, `just test-llvmpipe`, `just test-gpu` and `just test-vendor` pass on the
   dev machine.
3. `voxel_world` renders voxel terrain on this machine with `backend=Vulkan tier=High` (NVIDIA),
   with `--tier mid` and `--tier low` on Vulkan, and on GL with `tier=Mid` (radeonsi) and
   `--tier low`; screenshots of the same camera differ by at most 0.5 % of pixels between tiers
   on the same adapter (`--compare-to`).
4. `gpu_info` prints the expected classification for every adapter of this machine (section 12).
5. The simulation keeps updating while the window is minimized, on Vulkan and on GL
   (`voxel_world --minimize-test`).
6. Device-loss recovery works on Vulkan (`--simulate-device-loss`), unless cut (cut list 1).
7. No `todo!()`, no skeleton attributes, no `#[ignore = "integration..."]`, no `.scaffold/` left.

---

## 2. Repository layout (exact M0 file tree)

Owner in brackets (phase A: WP1..WP7, phase B: WP8, WP9; section 11). Files marked
`(skeleton)` are copied from `skeleton/` by WP0.

```
boxel/
├── Cargo.toml  Cargo.lock             [WP0] (skeleton) workspace + facade package; lock committed
├── README.md                          [WP9] Spanish, short
├── LICENSE-MIT  LICENSE-APACHE  NOTICE [WP0] (skeleton)
├── rust-toolchain.toml  rustfmt.toml  clippy.toml  typos.toml  deny.toml  justfile  .gitignore  [WP0] (skeleton)
├── .cargo/config.toml  .cargo/config.local.toml.example                                     [WP0] (skeleton)
├── .github/workflows/{ci.yml, miri.yml}  .github/dependabot.yml                             [WP0] (skeleton)
├── .scaffold/                         [WP0] (skeleton + design copy) M0 only; WP10 deletes it
│   ├── owners.tsv  bin/scaffold.py  deviations/wp0.md .. wp10.md
│   └── design/{SCAFFOLD_SPEC.md, ARCHITECTURE.md, adr/, reference/}
├── docs/
│   ├── architecture.md                [WP0] copy of ARCHITECTURE.md
│   └── adr/0001-...md .. 0012-...md   [WP0] copies of adr/*
├── src/lib.rs                         [WP0] (skeleton) facade: final in the skeleton
├── tests/                             [WP9] root integration tests (ambiguity check)
├── examples/
│   ├── voxel_world.rs                 [WP8]
│   └── headless_world.rs              [WP9]
├── tools/
│   ├── ecs_surface.py  no_bevy.py  vendor_lock_sync.py                                     [WP0] (skeleton)
│   └── refork/
│       ├── refork.sh  patch_tests.py  check_test_only.py  UPSTREAM_VERSION                  [WP0] (skeleton)
│       ├── patches/0001-bevy-25940-fix-entity-equivalent-index-set.patch                  [WP0] (skeleton)
│       └── template/{Cargo.toml, Cargo.lock, LICENSE-*, platform/*, crates/*/Cargo.toml}    [WP0] (skeleton)
├── vendor/                            [WP0] GENERATED by tools/refork/refork.sh (own Cargo workspace)
│   ├── Cargo.toml  Cargo.lock  LICENSE-MIT  LICENSE-APACHE  UPSTREAM.md
│   └── crates/{boxel_ecs, boxel_ecs_macros, boxel_ecs_macro_logic, boxel_macro_utils,
│               boxel_ptr, boxel_tasks, boxel_utils}/
└── crates/
    ├── boxel_app/        [WP1]  src/{lib,app,plugin,plugin_group,schedules,time,task_pool,runner}.rs  tests/contract.rs
    ├── boxel_log/        [WP1]  src/lib.rs
    ├── boxel_diagnostic/ [WP1]  src/lib.rs
    ├── boxel_math/       [WP1]  src/{lib,aabb,frustum,projection}.rs  tests/contract.rs
    ├── boxel_transform/  [WP1]  src/lib.rs
    ├── boxel_window/     [WP2]  src/{lib,window,input,runner}.rs  examples/window_smoke.rs
    ├── boxel_camera/     [WP2]  src/lib.rs
    ├── boxel_gfx/        [WP3]  src/{lib,tier,caps,classify,score,prefs,instance,context,lkg,select,smoke,report,surface,readback,hal}.rs
    │                            tests/{contract,gpu_contract,gpu_select}.rs  examples/gpu_info.rs
    ├── boxel_shaders/    [WP7]  shaders/{common,voxel_pull,voxel_instanced,cull,blit}.wgsl  src/{lib,gpu}.rs  tests/naga.rs
    ├── boxel_render/     [WP7, src/plugin.rs WP8, tests/gpu_terrain.rs WP9]
    │                            src/{lib,plugin,renderer,smoke,textures,arena,chunk_table,targets}.rs  src/path/{mod,high,mid,low}.rs
    │                            tests/{gpu_tiers,gpu_terrain}.rs
    ├── boxel_voxel/      [WP4]  src/{lib,coords,registry,storage,chunk}.rs  tests/contract.rs  benches/paletted.rs
    ├── boxel_worldgen/   [WP4]  src/{lib,noise}.rs  benches/terrain.rs
    ├── boxel_mesh/       [WP5]  src/{lib,packed,padded,mesher}.rs  tests/contract.rs  benches/mesh.rs
    ├── boxel_world/      [WP6, tests/ WP9 in phase B]  src/{lib,map,plugin,jobs,scheduler,persist}.rs  tests/
    └── boxel_facade_check/ [WP0] (skeleton) tests/derives.rs: derives through ::boxel::ecs only
```

Packages may add files inside their owned directories (new modules, tests, benches).

---

## 3. Manifests

All manifests below are final for M0 and exist in the skeleton. Versions were verified against
crates.io / the local registry (wgpu/naga 30.0.1, winit 0.30.13, glam 0.34.0,
binary-greedy-meshing 0.5.2, bevy 0.20.0-rc.2).

### 3.1 Root `Cargo.toml` (workspace + facade)

```toml
# file: Cargo.toml
[package]
name = "boxel"
description = "A Bevy-style voxel engine with runtime GPU tier detection on top of wgpu"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
readme = "README.md"
keywords = ["voxel", "game-engine", "gamedev", "wgpu", "ecs"]
categories = ["game-engines", "graphics", "rendering"]
exclude = [".github/", "tools/", "docs/", "vendor/"]

[workspace]
resolver = "3"
members = ["crates/*"]
# The vendored ECS fork is its own Cargo workspace (vendor/Cargo.toml): Bevy's lint set,
# its own lockfile for `cargo test`/Miri inside vendor/, zero edits to library code.
exclude = ["vendor"]

[workspace.package]
version = "0.1.0-dev"
edition = "2024"
# MSRV = stable - 2 (1.99 is current upstream stable). Must stay >= wgpu 30 (1.87) and the ECS fork (1.95).
rust-version = "1.97"
license = "MIT OR Apache-2.0"
repository = "https://github.com/OWNER/boxel"

[workspace.dependencies]
# ---- boxel crates (path + version so they stay publishable)
boxel_app = { path = "crates/boxel_app", version = "0.1.0-dev" }
boxel_log = { path = "crates/boxel_log", version = "0.1.0-dev" }
boxel_diagnostic = { path = "crates/boxel_diagnostic", version = "0.1.0-dev" }
boxel_math = { path = "crates/boxel_math", version = "0.1.0-dev" }
boxel_transform = { path = "crates/boxel_transform", version = "0.1.0-dev" }
boxel_window = { path = "crates/boxel_window", version = "0.1.0-dev" }
boxel_camera = { path = "crates/boxel_camera", version = "0.1.0-dev" }
boxel_gfx = { path = "crates/boxel_gfx", version = "0.1.0-dev", default-features = false }
boxel_shaders = { path = "crates/boxel_shaders", version = "0.1.0-dev" }
boxel_render = { path = "crates/boxel_render", version = "0.1.0-dev" }
boxel_voxel = { path = "crates/boxel_voxel", version = "0.1.0-dev" }
boxel_mesh = { path = "crates/boxel_mesh", version = "0.1.0-dev" }
boxel_worldgen = { path = "crates/boxel_worldgen", version = "0.1.0-dev" }
boxel_world = { path = "crates/boxel_world", version = "0.1.0-dev" }
# ---- vendored ECS fork (generated by tools/refork; see vendor/UPSTREAM.md)
boxel_ecs = { path = "vendor/crates/boxel_ecs", version = "0.1.0", default-features = false, features = ["std", "async_executor", "multi_threaded", "backtrace"] }
boxel_tasks = { path = "vendor/crates/boxel_tasks", version = "0.1.0", default-features = false, features = ["async_executor", "multi_threaded"] }

# ---- GPU / platform: exact pins, upgraded deliberately (ADR 0001)
wgpu = { version = "=30.0.1", default-features = false, features = ["std", "parking_lot", "wgsl", "vulkan", "gles", "dx12", "metal"] }
naga = { version = "=30.0.1", default-features = false }
winit = "=0.30.13"
pollster = "1.0.1"

# ---- engine libraries
glam = { version = "=0.34.0", features = ["bytemuck"] }
bytemuck = { version = "1.25.2", features = ["derive"] }
binary-greedy-meshing = "=0.5.2"
offset-allocator = "0.2.0"
slotmap = "1.1.1"
foldhash = "0.2.0"
crossbeam-channel = "0.5.17"
bitflags = "2.13.2"
serde = { version = "1.0.229", features = ["derive"] }
toml = "1.1.6"
thiserror = "2.0.21"
variadics_please = "2.0.0"
tracing = "0.1.44"
tracing-subscriber = { version = "0.3.23", features = ["env-filter"] }
tracing-tracy = "=0.12.0"
profiling = { version = "1.0.18", default-features = false }
log = "0.4.34"
png = "0.18.1"

# ---- dev-only
criterion = { version = "0.8.2", features = ["html_reports"] }
proptest = "1.11.0"

[workspace.lints.rust]
missing_docs = "warn"
unsafe_code = "deny"            # opt out locally with #[expect(unsafe_code, reason = "...")]
unsafe_op_in_unsafe_fn = "deny"
unused_qualifications = "warn"
# rust_2018_idioms is deliberately off: `elided_lifetimes_in_paths` fires on every Bevy-style system param.

[workspace.lints.clippy]
doc_markdown = "warn"
manual_let_else = "warn"
match_same_arms = "warn"
redundant_closure_for_method_calls = "warn"
redundant_else = "warn"
semicolon_if_nothing_returned = "warn"
undocumented_unsafe_blocks = "warn"
unwrap_or_default = "warn"
ptr_as_ptr = "warn"
ptr_cast_constness = "warn"
ref_as_ptr = "warn"
allow_attributes_without_reason = "warn"
print_stdout = "warn"
print_stderr = "warn"
dbg_macro = "warn"
todo = "warn"
large_stack_arrays = "warn"
large_stack_frames = "warn"
trivially_copy_pass_by_ref = "warn"
type_complexity = "allow"
too_many_arguments = "allow"
module_name_repetitions = "allow"

[lints]
workspace = true

[features]
default = ["window", "render", "voxel", "png", "debug"]
window = ["dep:boxel_window", "dep:boxel_camera"]
voxel = ["dep:boxel_voxel", "dep:boxel_mesh", "dep:boxel_worldgen", "dep:boxel_world"]
render = ["window", "voxel", "dep:boxel_gfx", "dep:boxel_shaders", "dep:boxel_render"]
# PNG screenshots (RequestScreenshot) and RgbaImage::{save_png, load_png}.
png = ["boxel_gfx?/png", "boxel_render?/png"]
# System/component names in ECS diagnostics (B0001 conflicts, schedule errors) and trace spans.
# Without it they print "<Enable the debug feature to see the name>". Disable for shipping builds.
debug = ["boxel_ecs/debug"]
# Spawn/insert caller locations in ECS diagnostics.
track_location = ["boxel_ecs/track_location"]
trace = ["debug", "boxel_ecs/trace"]
trace_tracy = ["trace", "boxel_log/tracy"]
# Vulkan escape hatch (Windows/Linux only; a no-op elsewhere).
hal = ["boxel_gfx?/hal"]

[dependencies]
boxel_ecs.workspace = true
boxel_tasks.workspace = true
boxel_app.workspace = true
boxel_log.workspace = true
boxel_diagnostic.workspace = true
boxel_math.workspace = true
boxel_transform.workspace = true
boxel_window = { workspace = true, optional = true }
boxel_camera = { workspace = true, optional = true }
boxel_gfx = { workspace = true, optional = true }
boxel_shaders = { workspace = true, optional = true }
boxel_render = { workspace = true, optional = true }
boxel_voxel = { workspace = true, optional = true }
boxel_mesh = { workspace = true, optional = true }
boxel_worldgen = { workspace = true, optional = true }
boxel_world = { workspace = true, optional = true }

[dev-dependencies]
tracing.workspace = true

[[example]]
name = "headless_world"
required-features = ["voxel"]

[[example]]
name = "voxel_world"
required-features = ["render", "voxel", "png"]

# Profiles only apply to this workspace's own examples/tests/benches.
[profile.dev]
opt-level = 1
[profile.dev.package."*"]
opt-level = 3
[profile.release]
lto = "thin"
[profile.profiling]
inherits = "release"
debug = "line-tables-only"
strip = "none"
[profile.dist]
inherits = "release"
lto = "fat"
codegen-units = 1
strip = "symbols"

[package.metadata.docs.rs]
all-features = true
rustdoc-args = ["--cfg", "docsrs"]
```

Notes:

- `vendor/` is excluded from the workspace: it is its own Cargo workspace (Bevy's lint set, own
  committed lockfile). Its crates are path dependencies (`boxel_ecs`, `boxel_tasks`).
- `boxel_gfx` is a workspace dependency with `default-features = false`: the facade `png`
  feature really controls PNG support (`boxel_gfx?/png`, `boxel_render?/png`).
- Facade feature `debug` (default) enables `boxel_ecs/debug`: ECS diagnostics (B0001 conflicts,
  schedule errors, trace spans) print system and component names instead of
  `<Enable the debug feature to see the name>`. `trace` implies it. Engine crates enable it for
  their own tests only, through a dev-dependency (`boxel_ecs = { workspace = true, features =
  ["debug"] }`), so it never leaks into users' builds.
- `hal` compiles to nothing on Apple targets (section 5.8).
- `rust_2018_idioms` is deliberately not enabled (`elided_lifetimes_in_paths` fires on every
  `ResMut<T>` system parameter).
- glam 0.34 removed `Mat4::perspective_*`/`look_at_*`: use `glam::camera` (wrapped in
  `boxel_math::projection`).

### 3.2 Crate manifests

`crates/boxel_app/Cargo.toml`:

```toml
# file: crates/boxel_app/Cargo.toml
[package]
name = "boxel_app"
description = "App, plugins, schedules, time and task pools for boxel (inspired by bevy_app, not a fork)"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
boxel_ecs.workspace = true
boxel_tasks.workspace = true
variadics_please.workspace = true
tracing.workspace = true

[dev-dependencies]
# Tests get system/component names in ECS diagnostics (dev-only; users opt in through boxel/debug).
boxel_ecs = { workspace = true, features = ["debug"] }

[lints]
workspace = true
```

`crates/boxel_log/Cargo.toml`:

```toml
# file: crates/boxel_log/Cargo.toml
[package]
name = "boxel_log"
description = "Logging setup for boxel (tracing + tracing-subscriber, optional Tracy)"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
tracy = ["dep:tracing-tracy", "dep:profiling", "profiling/profile-with-tracing"]

[dependencies]
boxel_app.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
tracing-tracy = { workspace = true, optional = true }
profiling = { workspace = true, optional = true }
thiserror.workspace = true

[lints]
workspace = true
```

`crates/boxel_diagnostic/Cargo.toml`:

```toml
# file: crates/boxel_diagnostic/Cargo.toml
[package]
name = "boxel_diagnostic"
description = "Frame statistics and periodic diagnostic logging for boxel"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
boxel_ecs.workspace = true
boxel_app.workspace = true
tracing.workspace = true

[dev-dependencies]
# Tests get system/component names in ECS diagnostics (dev-only; users opt in through boxel/debug).
boxel_ecs = { workspace = true, features = ["debug"] }

[lints]
workspace = true
```

`crates/boxel_math/Cargo.toml`:

```toml
# file: crates/boxel_math/Cargo.toml
[package]
name = "boxel_math"
description = "Math types and helpers for boxel (glam re-export, AABB, frustum, reverse-Z projection)"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
glam.workspace = true
bytemuck.workspace = true

[dev-dependencies]
proptest.workspace = true

[lints]
workspace = true
```

`crates/boxel_transform/Cargo.toml`:

```toml
# file: crates/boxel_transform/Cargo.toml
[package]
name = "boxel_transform"
description = "Transform component for boxel (f64 translation for large voxel worlds)"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
boxel_ecs.workspace = true
boxel_math.workspace = true

[lints]
workspace = true
```

`crates/boxel_window/Cargo.toml`:

```toml
# file: crates/boxel_window/Cargo.toml
[package]
name = "boxel_window"
description = "winit 0.30 runner, primary window and input for boxel"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
boxel_ecs.workspace = true
boxel_app.workspace = true
boxel_math.workspace = true
winit.workspace = true
tracing.workspace = true

[dev-dependencies]
# Tests get system/component names in ECS diagnostics (dev-only; users opt in through boxel/debug).
boxel_ecs = { workspace = true, features = ["debug"] }

[lints]
workspace = true
```

`crates/boxel_camera/Cargo.toml`:

```toml
# file: crates/boxel_camera/Cargo.toml
[package]
name = "boxel_camera"
description = "Camera component, camera-relative view math and a fly camera controller for boxel"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
boxel_ecs.workspace = true
boxel_app.workspace = true
boxel_math.workspace = true
boxel_transform.workspace = true
boxel_window.workspace = true

[dev-dependencies]
# Tests get system/component names in ECS diagnostics (dev-only; users opt in through boxel/debug).
boxel_ecs = { workspace = true, features = ["debug"] }

[lints]
workspace = true
```

`crates/boxel_gfx/Cargo.toml`:

```toml
# file: crates/boxel_gfx/Cargo.toml
[package]
name = "boxel_gfx"
description = "boxel's thin layer over wgpu: runtime backend/adapter detection, capability tiers, smoke tests"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
default = ["png"]
# PNG encoding for RgbaImage::save_png (screenshots, test artifacts).
png = ["dep:png"]
# Vulkan escape hatch (as_hal). Unsafe; compiled only on Windows/Linux/Android/FreeBSD
# (wgpu-hal's Vulkan backend is off on Apple targets without vulkan-portability).
hal = []

[dependencies]
wgpu.workspace = true
pollster.workspace = true
serde.workspace = true
toml.workspace = true
thiserror.workspace = true
tracing.workspace = true
png = { workspace = true, optional = true }

[dev-dependencies]
pollster.workspace = true
# examples/gpu_info.rs logs through boxel_log (dev-only: the library stays ECS-free).
boxel_log.workspace = true

[lints]
workspace = true
```

`crates/boxel_shaders/Cargo.toml`:

```toml
# file: crates/boxel_shaders/Cargo.toml
[package]
name = "boxel_shaders"
description = "WGSL shaders and their #[repr(C)] GPU mirror types for boxel"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
wgpu.workspace = true
bytemuck.workspace = true

[dev-dependencies]
naga = { workspace = true, features = ["wgsl-in", "glsl-out", "msl-out", "hlsl-out", "spv-out"] }
boxel_mesh.workspace = true

[lints]
workspace = true
```

`crates/boxel_render/Cargo.toml`:

```toml
# file: crates/boxel_render/Cargo.toml
[package]
name = "boxel_render"
description = "boxel's voxel renderer: High/Mid/Low tier paths over one quad format, no render graph"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
# PNG screenshots (RequestScreenshot). Without it ScreenshotSaved carries an error.
png = ["boxel_gfx/png"]

[dependencies]
boxel_ecs.workspace = true
boxel_app.workspace = true
boxel_diagnostic.workspace = true
boxel_math.workspace = true
boxel_transform.workspace = true
boxel_window.workspace = true
boxel_camera.workspace = true
boxel_gfx.workspace = true
boxel_shaders.workspace = true
boxel_voxel.workspace = true
boxel_mesh.workspace = true
boxel_world.workspace = true
wgpu.workspace = true
bytemuck.workspace = true
offset-allocator.workspace = true
pollster.workspace = true
tracing.workspace = true
thiserror.workspace = true

[dev-dependencies]
boxel_gfx = { workspace = true, features = ["png"] }
boxel_worldgen.workspace = true
# Tests get system/component names in ECS diagnostics (dev-only; users opt in through boxel/debug).
boxel_ecs = { workspace = true, features = ["debug"] }
pollster.workspace = true

[lints]
workspace = true
```

`crates/boxel_voxel/Cargo.toml`:

```toml
# file: crates/boxel_voxel/Cargo.toml
[package]
name = "boxel_voxel"
description = "Voxel data model for boxel: coordinates, block registry, paletted 32^3 chunks"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
glam.workspace = true
bitflags.workspace = true
foldhash.workspace = true
thiserror.workspace = true

[dev-dependencies]
proptest.workspace = true
criterion.workspace = true

[[bench]]
name = "paletted"
harness = false

[lints]
workspace = true
```

`crates/boxel_mesh/Cargo.toml`:

```toml
# file: crates/boxel_mesh/Cargo.toml
[package]
name = "boxel_mesh"
description = "Binary greedy meshing of 32^3 voxel chunks into 8-byte PackedQuads for boxel"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
boxel_voxel.workspace = true
binary-greedy-meshing.workspace = true
bytemuck.workspace = true

[dev-dependencies]
proptest.workspace = true
criterion.workspace = true

[[bench]]
name = "mesh"
harness = false

[lints]
workspace = true
```

`crates/boxel_worldgen/Cargo.toml`:

```toml
# file: crates/boxel_worldgen/Cargo.toml
[package]
name = "boxel_worldgen"
description = "World generation traits and M0 generators (flat, noise terrain) for boxel"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
boxel_voxel.workspace = true

[dev-dependencies]
criterion.workspace = true

[[bench]]
name = "terrain"
harness = false

[lints]
workspace = true
```

`crates/boxel_world/Cargo.toml`:

```toml
# file: crates/boxel_world/Cargo.toml
[package]
name = "boxel_world"
description = "Chunk map, streaming lifecycle, job scheduling and edits for boxel"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
boxel_ecs.workspace = true
boxel_tasks.workspace = true
boxel_app.workspace = true
boxel_diagnostic.workspace = true
boxel_math.workspace = true
boxel_transform.workspace = true
boxel_voxel.workspace = true
boxel_mesh.workspace = true
boxel_worldgen.workspace = true
slotmap.workspace = true
foldhash.workspace = true
crossbeam-channel.workspace = true
bitflags.workspace = true
tracing.workspace = true

[dev-dependencies]
# Tests get system/component names in ECS diagnostics (dev-only; users opt in through boxel/debug).
boxel_ecs = { workspace = true, features = ["debug"] }

[lints]
workspace = true
```

`crates/boxel_facade_check/Cargo.toml` (publish = false; depends only on the facade):

```toml
# file: crates/boxel_facade_check/Cargo.toml
[package]
name = "boxel_facade_check"
description = "CI-only: proves the derive macros resolve ::boxel::ecs for a crate that depends only on the boxel facade"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
publish = false

# Depends ONLY on the facade (no boxel_ecs): BevyManifest then resolves derives to ::boxel::ecs,
# which is what real users get. A direct boxel_ecs dependency would mask a broken refork rename.
[dependencies]
boxel = { path = "../..", default-features = false }

[lints]
workspace = true
```

### 3.3 Vendored workspace

Produced by `tools/refork/refork.sh` from `tools/refork/template/` (section 10.12). Never edit
`vendor/` by hand. Facts verified on the skeleton:

- Library code identical to upstream except mechanical renames (proved on every refork by
  `check_test_only.py`); `boxel_utils/src/platform/` is the `bevy_platform` replacement.
- Every `.rs` file that differs from upstream starts with
  `// boxel: modified by tools/refork (...)` (Apache-2.0 section 4(b)); 171 files today.
- `vendor/Cargo.toml` declares `check-cfg` values for the removed upstream features
  (`bevy_reflect`, `hotpatching`, `bevy_debug_stepping`, `reflect_functions`), so the library
  builds warning-free under `CARGO_BUILD_WARNINGS=deny` without touching code.
- `vendor/Cargo.lock` is committed and kept across reforks; `tools/vendor_lock_sync.py` checks it
  agrees with the root lock on shared packages (65 today).
- Tests: `RUST_BACKTRACE=1 cargo test --workspace --lib --locked` (961 pass) and
  `cargo test -p boxel_ecs --doc --features debug --locked` (480 pass) inside `vendor/`.

---

## 4. Cross-cutting conventions

- **Units.** 1 world unit = 1 voxel. World space right-handed, +Y up, cameras look down local -Z.
  Clip space: wgpu (Y up, Z in [0, 1]). Depth: reverse-Z infinite (`Greater`, clear 0). On GL
  backends wgpu remaps z to [-1, 1] and loses the reverse-Z precision gain (about 0.1 m depth
  resolution at 384 m with near 0.1): keep `Camera::near >= 0.1`.
- **Camera-relative rendering.** GPU positions are `world - render_origin`, where
  `render_origin = floor(camera translation)` (IVec3). Chunk origins are absolute `i32` voxel
  coordinates; shaders subtract in integers first.
- **Determinism.** Worldgen, texture synthesis, chunk map iteration for saving (sorted) and tests
  are deterministic. Hash maps that are iterated use `foldhash::fast::FixedState`.
- **Error handling.** Libraries return typed errors; plugins log and degrade (never `panic!` on
  GPU or driver problems; panics are for programmer errors only).
- **Threads.** The winit window is a non-send resource (`NonSend<PrimaryWindow>`); window
  methods, surface creation, acquire and present run on the main thread. GPU bring-up
  (selection, smoke tests, `VoxelRenderer::new`) runs on one thread because wgpu error scopes are
  thread-local.
- **GPU tests.** Integration test files whose name contains `gpu` (`tests/gpu_*.rs`). They skip
  (print a line and return) when no adapter/device is available, unless
  `boxel_gfx::gpu_tests_required()` (`BOXEL_REQUIRE_GPU=1`), in which case they fail. They honor
  `WGPU_BACKEND` and `WGPU_ADAPTER_NAME` (case-insensitive substring filter; see
  `crates/boxel_gfx/tests/gpu_contract.rs` for the 6-line pattern). Each GPU test binary may hold
  several tests; the just recipes and CI run them with `--test-threads=1` (multi-threaded EGL
  init is not trusted). PNG artifacts go to `env!("CARGO_TARGET_TMPDIR")/<test>/`, which exists
  only for integration tests and benches; unit tests use
  `std::env::temp_dir().join(<unique name>)`.
- **Logging targets.** Use the crate name as target (default). Stable info lines:
  `boxel: <GpuContext::summary_line()>` when the GPU is ready, and `DECISION: ...` at the end of a
  selection report.


---

## 5. Crate specifications

Each subsection: owner, purpose, what the skeleton already implements (`[S]`), what the owner must
implement (`[P]`, with the behaviour rules), the public API (generated from the compiled skeleton:
doc comments and bodies stripped; the skeleton files carry the full doc comments, which are part
of the contract), tests and acceptance criteria.

### 5.1 `boxel_math` (WP1)

Purpose: glam re-export and the shared geometry helpers. Pure, no ECS. Fully `[S]` (no skeleton
attribute): `Aabb` (all), `Frustum` (Gribb-Hartmann extraction from a reverse-Z infinite
matrix: left = r3 + r0, right = r3 - r0, bottom = r3 + r1, top = r3 - r1, near = r3 - r2, far =
(0, 0, 0, 1); `set_max_distance` replaces the far plane with `(-forward, dot(forward, eye) + d)`;
`intersects_aabb` is the p-vertex test), `projection`. Contract tests in `tests/contract.rs`.

WP1 adds: a proptest (if any corner of an AABB is inside all planes then `intersects_aabb` is
true) and edge cases of its choice.

Public API (`boxel_math`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_math/src/aabb.rs
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Aabb {
    pub min: Vec3,
    pub max: Vec3,
}

impl Aabb {
    pub const fn new(min: Vec3, max: Vec3) -> Self;

    pub fn from_center_half_extents(center: Vec3, half_extents: Vec3) -> Self;

    pub fn center(&self) -> Vec3;

    pub fn half_extents(&self) -> Vec3;

    pub fn contains_point(&self, p: Vec3) -> bool;

    pub fn intersects(&self, other: &Aabb) -> bool;
}

// ---- crates/boxel_math/src/frustum.rs
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frustum {
    pub planes: [Vec4; 6],
}

impl Frustum {
    pub fn from_view_proj_reverse_z(view_proj: Mat4) -> Self;

    pub fn set_max_distance(&mut self, eye: Vec3, forward: Vec3, distance: f32);

    pub fn intersects_aabb(&self, aabb: &Aabb) -> bool;

    pub fn to_array(self) -> [[f32; 4]; 6];
}

// ---- crates/boxel_math/src/lib.rs
pub use glam;
pub use glam::{
    DQuat, DVec2, DVec3, IVec2, IVec3, Mat3, Mat4, Quat, UVec2, UVec3, Vec2, Vec3, Vec3A, Vec4,
};

pub mod projection;

pub use aabb::Aabb;
pub use frustum::Frustum;

pub mod prelude {
    pub use crate::{Aabb, Frustum};
    pub use glam::{DQuat, DVec3, IVec2, IVec3, Mat4, Quat, UVec2, UVec3, Vec2, Vec3, Vec4};
}

// ---- crates/boxel_math/src/projection.rs
pub fn perspective_infinite_reverse(fov_y_radians: f32, aspect: f32, near: f32) -> Mat4;

pub fn look_to(eye: Vec3, dir: Vec3, up: Vec3) -> Mat4;
```

### 5.2 `boxel_app` (WP1)

Purpose: the application layer (ADR 0004).

`[S]` (validated by `tests/contract.rs`, 10 tests): `App` core (`new`, `empty`, `world*`,
`update`, `run`, `set_runner`, `plugins_state`, `finish`, `cleanup` (plugins added from another
plugin's `finish` stay registered and are finished in the same loop), `shutdown`,
`add_plugins`, `is_plugin_added`, `get_added_plugins`, `add_systems`, `configure_sets`,
`add_schedule`, `init_schedule`, `edit_schedule`, `add_message`, `insert_resource`,
`init_resource`, `insert_non_send`, `init_non_send`, `should_exit` (the first error if any
`AppExit` is an error, else `Success`)), `AppExit`, `Plugin`, `Plugins` (tuple marker impls via
`variadics_please::all_tuples!`, see NOTICE), `PluginGroup`,
`PluginGroupBuilder::{start, add, set, disable, enable, finish}`, all schedule labels including
`Shutdown`, `MainScheduleOrder`, `FixedMainScheduleOrder`, `MainSchedulePlugin`,
`ScheduleRunnerPlugin` constructors and `build`, the whole `time` module, the whole
`task_pool` module.

Frozen behaviour worth knowing (all `[S]`):

- `TimePlugin`: `time_system` in `First`; `run_fixed_main_loop` in `RunFixedMainLoop` (64 Hz,
  at most 8 steps per frame, whole leftover steps dropped with a rate-limited warning);
  `signal_message_update_system` in `FixedPostUpdate` and `MessageRegistry::should_update =
  Waiting`, so a `FixedUpdate` reader never loses messages written in `Update` (contract test:
  30 frames at 5.2 ms with a 15.6 ms step lose none). While `FixedMain` runs, `Res<Time>` is the
  fixed view (`delta == FixedTime::timestep`, `elapsed == FixedTime::elapsed`).
- `TaskPoolOptions`: workers = `available_parallelism - 1` (the main thread runs systems too);
  io 10 % in [1, 2], async compute 40 % in [1, 8], compute the rest; `split(16) == (2, 6, 7)`.
- `Shutdown` is not part of the frame: runners call `App::shutdown()` once after the last update
  (the default runner does it); the winit runner calls it in `exiting` while the event loop is
  alive.

`[P]` rules:

- `PluginGroupBuilder::add_before/add_after`: insert the new entry relative to `Target` (panic
  with the type name if absent); if `T` already exists, move it.
- `run_headless(app, mode)`: wait until `plugins_state() != Adding` (yield), `finish`,
  `cleanup`; `Once`: one update; `Loop`: update, stop on `should_exit()`, else sleep
  `wait - elapsed` if positive (`#[expect(clippy::disallowed_methods, reason = "headless runner
  pacing")]`). In both modes call `app.shutdown()` before returning `should_exit()` or `Success`.

Public API (`boxel_app`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_app/src/app.rs
pub type RunnerFn = Box<dyn FnOnce(App) -> AppExit>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginsState {
    Adding,
    Ready,
    Finished,
    Cleaned,
}

#[derive(Message, Debug, Clone, Default, PartialEq, Eq)]
pub enum AppExit {
    #[default]
    Success,
    Error(NonZeroU8),
}

impl AppExit {
    pub const fn error() -> Self;

    pub fn from_code(code: u8) -> Self;

    pub const fn is_success(&self) -> bool;

    pub const fn code(&self) -> u8;
}

impl std::process::Termination for AppExit {
    fn report(self) -> std::process::ExitCode;
}

pub struct App {
    world: World,
    plugins: Vec<Box<dyn Plugin>>,
    plugin_names: HashSet<String>,
    plugin_build_depth: usize,
    plugins_state: PluginsState,
    runner: Option<RunnerFn>,
}

impl Default for App {
    fn default() -> Self;
}

impl App {
    pub fn new() -> Self;

    pub fn empty() -> Self;

    pub fn world(&self) -> &World;

    pub fn world_mut(&mut self) -> &mut World;

    pub fn update(&mut self);

    pub fn run(&mut self) -> AppExit;

    pub fn set_runner(&mut self, f: impl FnOnce(App) -> AppExit + 'static) -> &mut Self;

    pub fn plugins_state(&mut self) -> PluginsState;

    pub fn finish(&mut self);

    pub fn cleanup(&mut self);

    pub fn shutdown(&mut self);

    pub fn add_plugins<M>(&mut self, plugins: impl Plugins<M>) -> &mut Self;

    pub(crate) fn add_boxed_plugin(&mut self, plugin: Box<dyn Plugin>);

    pub fn is_plugin_added<T: Plugin>(&self) -> bool;

    pub fn get_added_plugins<T: Plugin>(&self) -> Vec<&T>;

    pub fn add_systems<M>(
        &mut self,
        schedule: impl ScheduleLabel,
        systems: impl IntoScheduleConfigs<ScheduleSystem, M>,
    ) -> &mut Self;

    pub fn configure_sets<M>(
        &mut self,
        schedule: impl ScheduleLabel,
        sets: impl IntoScheduleConfigs<InternedSystemSet, M>,
    ) -> &mut Self;

    pub fn add_schedule(&mut self, schedule: Schedule) -> &mut Self;

    pub fn init_schedule(&mut self, label: impl ScheduleLabel) -> &mut Self;

    pub fn edit_schedule(
        &mut self,
        label: impl ScheduleLabel,
        f: impl FnOnce(&mut Schedule),
    ) -> &mut Self;

    pub fn add_message<M: Message>(&mut self) -> &mut Self;

    pub fn insert_resource<R: Resource>(&mut self, resource: R) -> &mut Self;

    pub fn init_resource<R: Resource + FromWorld>(&mut self) -> &mut Self;

    pub fn insert_non_send<R: 'static>(&mut self, resource: R) -> &mut Self;

    pub fn init_non_send<R: 'static + FromWorld>(&mut self) -> &mut Self;

    pub fn should_exit(&self) -> Option<AppExit>;
}

struct PlaceholderPlugin;

impl Plugin for PlaceholderPlugin {
    fn build(&self, _app: &mut App);
    fn is_unique(&self) -> bool;
}

fn run_once(mut app: App) -> AppExit;

// ---- crates/boxel_app/src/lib.rs
pub use app::{App, AppExit, PluginsState, RunnerFn};
pub use plugin::{Plugin, Plugins};
pub use plugin_group::{PluginGroup, PluginGroupBuilder};
pub use runner::{RunMode, ScheduleRunnerPlugin};
pub use schedules::{
    First, FixedMain, FixedMainScheduleOrder, FixedPostUpdate, FixedPreUpdate, FixedUpdate, Last,
    Main, MainScheduleOrder, MainSchedulePlugin, PostStartup, PostUpdate, PreStartup, PreUpdate,
    RunFixedMainLoop, Shutdown, Startup, Update,
};
pub use task_pool::{TaskPoolOptions, TaskPoolPlugin, TaskPoolThreadAssignmentPolicy};
pub use time::{FixedTime, Time, TimePlugin, TimeSystems, run_fixed_main_loop, time_system};

pub mod prelude {
    pub use crate::{
        App, AppExit, First, FixedTime, FixedUpdate, Last, Main, MainScheduleOrder, Plugin,
        PluginGroup, PluginGroupBuilder, PostStartup, PostUpdate, PreStartup, PreUpdate,
        ScheduleRunnerPlugin, Shutdown, Startup, TaskPoolOptions, TaskPoolPlugin, Time, TimePlugin,
        Update,
    };
}

// ---- crates/boxel_app/src/plugin.rs
pub trait Plugin: Any + Send + Sync {
    fn build(&self, app: &mut App);
    fn ready(&self, _app: &App) -> bool;
    fn finish(&self, _app: &mut App);
    fn cleanup(&self, _app: &mut App);
    fn name(&self) -> &str;
    fn is_unique(&self) -> bool;
}

impl<T: Fn(&mut App) + Send + Sync + 'static> Plugin for T {
    fn build(&self, app: &mut App);
}

pub(crate) mod sealed {
    use crate::{App, Plugin, PluginGroup};
    use variadics_please::all_tuples;

    pub trait Plugins<Marker> {
        fn add_to_app(self, app: &mut App);
    }
    pub struct PluginMarker;
    pub struct PluginGroupMarker;
    pub struct PluginsTupleMarker;

    impl<P: Plugin> Plugins<PluginMarker> for P {
        fn add_to_app(self, app: &mut App);
    }

    impl<P: PluginGroup> Plugins<PluginGroupMarker> for P {
        fn add_to_app(self, app: &mut App);
    }

    macro_rules! impl_plugins_tuples {
        ($(#[$meta:meta])* $(($param: ident, $plugins: ident)),*) => {
            $(#[$meta])*
            impl<$($param, $plugins),*> Plugins<(PluginsTupleMarker, $($param,)*)> for ($($plugins,)*)
            where $($plugins: Plugins<$param>),*
            {
                #[allow(non_snake_case, unused_variables, reason = "macro-generated tuple impl")]
                fn add_to_app(self, app: &mut App);
            }
        }
    }

    all_tuples!(impl_plugins_tuples, 0, 15, P, S);
}

pub trait Plugins<Marker>: sealed::Plugins<Marker> {}
impl<Marker, T: sealed::Plugins<Marker>> Plugins<Marker> for T {}

// ---- crates/boxel_app/src/plugin_group.rs
pub trait PluginGroup: Sized {
    fn build(self) -> PluginGroupBuilder;

    fn set<T: Plugin>(self, plugin: T) -> PluginGroupBuilder;
}

struct Entry {
    type_id: TypeId,
    plugin: Box<dyn Plugin>,
    enabled: bool,
}

pub struct PluginGroupBuilder {
    entries: Vec<Entry>,
}

impl PluginGroupBuilder {
    pub fn start() -> Self;

    #[expect(
        clippy::should_implement_trait,
        reason = "Bevy-style builder method name"
    )]
    pub fn add<T: Plugin>(mut self, plugin: T) -> Self;

    pub fn set<T: Plugin>(mut self, plugin: T) -> Self;

    pub fn disable<T: Plugin>(mut self) -> Self;

    pub fn enable<T: Plugin>(mut self) -> Self;

    pub fn add_before<Target: Plugin, T: Plugin>(self, plugin: T) -> Self;

    pub fn add_after<Target: Plugin, T: Plugin>(self, plugin: T) -> Self;

    pub fn finish(self, app: &mut App);
}

impl PluginGroup for PluginGroupBuilder {
    fn build(self) -> PluginGroupBuilder;
}

// ---- crates/boxel_app/src/runner.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunMode {
    Once,
    Loop {
        wait: Option<Duration>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ScheduleRunnerPlugin {
    pub run_mode: RunMode,
}

impl ScheduleRunnerPlugin {
    pub fn run_once() -> Self;
    pub fn run_loop(wait: Duration) -> Self;
}

impl Plugin for ScheduleRunnerPlugin {
    fn build(&self, app: &mut App);
}

fn run_headless(app: App, mode: RunMode) -> AppExit;

// ---- crates/boxel_app/src/schedules.rs
macro_rules! labels {
    ($($(#[$m:meta])* $name:ident),* $(,)?) => {$(
        $(#[$m])*
        #[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash, Default)]
        pub struct $name;
    )*};
}

labels! {
    Main,
    PreStartup,
    Startup,
    PostStartup,
    First,
    PreUpdate,
    RunFixedMainLoop,
    FixedMain,
    FixedPreUpdate,
    FixedUpdate,
    FixedPostUpdate,
    Update,
    PostUpdate,
    Last,
    Shutdown,
}

#[derive(Resource, Debug)]
pub struct MainScheduleOrder {
    pub labels: Vec<InternedScheduleLabel>,
    pub startup_labels: Vec<InternedScheduleLabel>,
}

impl Default for MainScheduleOrder {
    fn default() -> Self;
}

impl MainScheduleOrder {
    pub fn insert_after(&mut self, after: impl ScheduleLabel, schedule: impl ScheduleLabel);

    pub fn insert_before(&mut self, before: impl ScheduleLabel, schedule: impl ScheduleLabel);
}

#[derive(Resource, Debug)]
pub struct FixedMainScheduleOrder {
    pub labels: Vec<InternedScheduleLabel>,
}

impl Default for FixedMainScheduleOrder {
    fn default() -> Self;
}

fn run_main(world: &mut World, mut started: Local<bool>);

fn run_fixed_main(world: &mut World);

pub struct MainSchedulePlugin;

impl Plugin for MainSchedulePlugin {
    fn build(&self, app: &mut App);
}

// ---- crates/boxel_app/src/task_pool.rs
#[derive(Clone, Debug)]
pub struct TaskPoolThreadAssignmentPolicy {
    pub min_threads: usize,
    pub max_threads: usize,
    pub percent: f32,
}

#[derive(Clone, Debug)]
pub struct TaskPoolOptions {
    pub min_total_threads: usize,
    pub max_total_threads: usize,
    pub io: TaskPoolThreadAssignmentPolicy,
    pub async_compute: TaskPoolThreadAssignmentPolicy,
    pub compute: TaskPoolThreadAssignmentPolicy,
}

impl Default for TaskPoolOptions {
    fn default() -> Self;
}

impl TaskPoolThreadAssignmentPolicy {
    fn threads_for(&self, total: usize) -> usize;
}

impl TaskPoolOptions {
    pub fn split(&self, hardware_threads: usize) -> (usize, usize, usize);

    pub fn create_default_pools(&self);
}

#[derive(Default)]
pub struct TaskPoolPlugin {
    pub task_pool_options: TaskPoolOptions,
}

impl Plugin for TaskPoolPlugin {
    fn build(&self, _app: &mut App);
}

// ---- crates/boxel_app/src/time.rs
#[derive(Resource, Debug, Clone)]
pub struct Time {
    startup: Instant,
    last_update: Option<Instant>,
    delta: Duration,
    elapsed: Duration,
    frame_count: u64,
    max_delta: Duration,
}

impl Default for Time {
    fn default() -> Self;
}

impl Time {
    pub fn new(startup: Instant) -> Self;
    pub fn delta(&self) -> Duration;
    pub fn delta_secs(&self) -> f32;
    pub fn elapsed(&self) -> Duration;
    pub fn elapsed_secs_f64(&self) -> f64;
    pub fn frame_count(&self) -> u64;
    pub fn max_delta(&self) -> Duration;
    pub fn set_max_delta(&mut self, max: Duration);
    pub fn update_with_instant(&mut self, now: Instant);

    fn fixed_view(&self, fixed: &FixedTime) -> Time;
}

#[derive(Resource, Debug, Clone)]
pub struct FixedTime {
    timestep: Duration,
    accumulator: Duration,
    elapsed: Duration,
    max_steps_per_frame: u32,
    last_overrun_warning: Option<Instant>,
}

impl Default for FixedTime {
    fn default() -> Self;
}

impl FixedTime {
    pub fn from_hz(hz: f64) -> Self;
    pub fn timestep(&self) -> Duration;
    pub fn set_timestep(&mut self, timestep: Duration);
    pub fn accumulator(&self) -> Duration;
    pub fn elapsed(&self) -> Duration;
    pub fn overstep_fraction(&self) -> f32;
    pub fn accumulate(&mut self, delta: Duration);
    pub fn expend(&mut self) -> bool;
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimeSystems;

pub fn time_system(mut time: ResMut<Time>);

pub fn run_fixed_main_loop(world: &mut World);

#[derive(Default)]
pub struct TimePlugin;

impl Plugin for TimePlugin {
    fn build(&self, app: &mut App);
}
```

Tests to add: `add_before/add_after` ordering and move; `run_once` and `run_loop` return the
written `AppExit` (loop stops after a system writes it on frame 5) and run `Shutdown` once;
`TaskPoolPlugin` makes `ComputeTaskPool::try_get()` return `Some`; a headless app with
`TaskPoolPlugin + TimePlugin + ScheduleRunnerPlugin::run_loop(..)` runs and exits.

### 5.3 `boxel_log` (WP1)

`[P]`: `init_logging(filter)`: `EnvFilter::try_from_default_env()` (if `RUST_LOG` is set and
valid) else `EnvFilter::try_new(filter)` (`BadFilter` on error); `tracing_subscriber::registry()`
with a `fmt` layer (ANSI only if stderr is a terminal) and the filter; with feature `tracy` add
`tracing_tracy::TracyLayer::default()`; `try_init()` (sets the `log` bridge because
tracing-subscriber's `tracing-log` default feature is on); `AlreadySet` if a subscriber exists.
`LogPlugin::build`: call it; `AlreadySet` -> debug log; `BadFilter` -> `eprintln!` once and fall
back to `DEFAULT_FILTER`.

Public API (`boxel_log`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_log/src/lib.rs
pub const DEFAULT_FILTER: &str = "info,wgpu_core=warn,wgpu_hal=warn,naga=warn";

#[derive(Debug, thiserror::Error)]
pub enum LogInitError {
    #[error("a global tracing subscriber is already set")]
    AlreadySet,
    #[error("invalid log filter: {0}")]
    BadFilter(String),
}

pub fn init_logging(filter: &str) -> Result<(), LogInitError>;

#[derive(Clone, Debug)]
pub struct LogPlugin {
    pub filter: String,
}

impl Default for LogPlugin {
    fn default() -> Self;
}

impl Plugin for LogPlugin {
    fn build(&self, app: &mut App);
}
```

Tests: second `init_logging` returns `AlreadySet`; an invalid filter returns `BadFilter` (each in
its own integration test binary so global state does not leak).

### 5.4 `boxel_diagnostic` (WP1)

`[S]`: `FrameStats`, `DiagnosticsBoard` (all methods).

`[P]`: `FrameStatsPlugin::build`: `init_resource::<FrameStats>()`,
`init_resource::<DiagnosticsBoard>()`, a system in `Last` that reads `Time`: `frame += 1`,
`last_frame_time = delta`, `avg = avg * 0.9 + delta * 0.1` (first frame: `avg = delta`),
`fps = 1 / avg` (0 if avg is 0), `worst = max(worst, delta)`. `LogDiagnosticsPlugin::build`: a
`Last` system (after the stats system) with a `Local` timer; every `interval`:
`info!("fps={:.0} frame={:.2}ms avg={:.2}ms worst={:.2}ms")` then one `info!("{key}: {value}")`
per board line; resets `worst`.

Public API (`boxel_diagnostic`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_diagnostic/src/lib.rs
#[derive(Resource, Debug, Clone, Default)]
pub struct FrameStats {
    pub frame: u64,
    pub last_frame_time: Duration,
    pub avg_frame_time: Duration,
    pub fps: f32,
    pub worst_frame_time: Duration,
}

#[derive(Resource, Debug, Clone, Default)]
pub struct DiagnosticsBoard {
    entries: BTreeMap<&'static str, String>,
}

impl DiagnosticsBoard {
    pub fn set(&mut self, key: &'static str, value: impl Into<String>);
    pub fn get(&self, key: &str) -> Option<&str>;
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)>;
}

#[derive(Default)]
pub struct FrameStatsPlugin;

impl Plugin for FrameStatsPlugin {
    fn build(&self, app: &mut App);
}

#[derive(Clone, Debug)]
pub struct LogDiagnosticsPlugin {
    pub interval: Duration,
}

impl Default for LogDiagnosticsPlugin {
    fn default() -> Self;
}

impl Plugin for LogDiagnosticsPlugin {
    fn build(&self, app: &mut App);
}
```

Tests: stats after three synthetic frames (drive `Time::update_with_instant` directly); board
ordering and replacement.

### 5.5 `boxel_transform` (WP1)

All `[S]`. Owner adds tests: `forward/right/up` for identity and for `from_yaw_pitch(.., PI/2, 0)`.

Public API (`boxel_transform`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_transform/src/lib.rs
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    pub translation: DVec3,
    pub rotation: Quat,
}

impl Default for Transform {
    fn default() -> Self;
}

impl Transform {
    pub const IDENTITY: Self = Self {
        translation: DVec3::ZERO,
        rotation: Quat::IDENTITY,
    };

    pub const fn from_translation(translation: DVec3) -> Self;

    pub const fn from_xyz(x: f64, y: f64, z: f64) -> Self;

    pub const fn with_rotation(mut self, rotation: Quat) -> Self;

    pub fn from_yaw_pitch(translation: DVec3, yaw: f32, pitch: f32) -> Self;

    pub fn forward(&self) -> Vec3;

    pub fn right(&self) -> Vec3;

    pub fn up(&self) -> Vec3;
}

pub mod prelude {
    pub use crate::Transform;
}
```

### 5.6 `boxel_window` (WP2)

Purpose: winit 0.30.13 runner, primary window, window and input messages (ADR 0008).

`[S]`: all types, messages, resources and their defaults; `ButtonInput::get_pressed`.

Key facts (verified):

- winit 0.30.13 never sends `WindowEvent::Occluded` on Wayland or Windows
  (`winit-0.30.13/src/event.rs`, `Occluded` docs).
- On Wayland, once `Window::pre_present_notify()` requested a frame callback, winit holds
  `RedrawRequested` until the compositor delivers it (`platform_impl/linux/wayland/event_loop`).
  A hidden surface gets no frame callbacks.
- Measured on this machine (`reference/hidden_window_probe.rs`, window minimized for 2 s):
  presenting every update with FIFO froze the loop on GL (0 updates) and throttled Vulkan to
  ~105 updates/s; presenting only after `RedrawRequested` kept ~218k updates/s with `Poll` on
  Vulkan (NVIDIA, RADV) and GL (radeonsi), and 0 presents while hidden. Visible, the gated loop
  presented at display rate (200 frames in 1.5 s on GL).

`[P]` rules:

- `ButtonInput`: `press` inserts into `pressed` and, if newly inserted, `just_pressed`;
  `release` removes from `pressed` and, if it was held, inserts into `just_released`;
  `clear_just` clears both `just_*` sets; `reset_all` releases everything (moving held inputs to
  `just_released`).
- Input systems (all in `PreUpdate`, set `InputSystems`): `keyboard_input_system` and
  `mouse_button_input_system` call `clear_just` then apply events (key repeats ignored);
  accumulators reset then sum. A private system releases all buttons on `WindowFocused(false)`.
- `InputPlugin::build`: registers `KeyboardInput`, `MouseButtonInput`, `MouseMotion`,
  `MouseWheel`, `CursorMoved`, `WindowFocused`; inits `ButtonInput<KeyCode>`,
  `ButtonInput<MouseButton>`, `AccumulatedMouseMotion`, `AccumulatedMouseScroll`; adds systems.
- `WindowPlugin::build`: `EventLoop::new()` (panic with a clear message if it fails); insert
  `DisplayHandle(event_loop.owned_display_handle())`, `self.primary` as `WindowSettings`,
  `WindowState::default()`, `CursorOptions { grab: None, visible: true }`, a private resource
  with `exit_on_close` and `update_mode`; register `WindowCreated`, `WindowResized`,
  `WindowCloseRequested`, `WindowFocused`, `WindowOccluded`;
  `set_runner(move |app| winit_runner(app, event_loop))`.
- `winit_runner`: the frozen contract in its doc comment (7 steps). Details:
  - `resumed`: create once with `Window::default_attributes().with_title(..)
    .with_inner_size(LogicalSize::new(w, h)).with_resizable(..)` (+ borderless fullscreen if
    asked); `Arc<Window>`; `app.insert_non_send(PrimaryWindow { .. })`; set `WindowState {
    physical_size, scale_factor, focused: true, occluded: false, frame_ready: false }`; write
    `WindowCreated`; mark a redraw as pending (winit delivers the first `RedrawRequested` on its
    own on Wayland; call `request_redraw()` once to be sure on other platforms).
  - `window_event`: `Resized(s)` with non-zero size -> `WindowState` + `WindowResized`;
    `ScaleFactorChanged` -> state; `Focused(f)`; `Occluded(o)` (store `winit_occluded`);
    `CloseRequested`; `KeyboardInput { event: KeyEvent { physical_key: PhysicalKey::Code(code),
    state, repeat, .. } }`; `MouseInput`; `MouseWheel` (`LineDelta` as is, `PixelDelta / 32`);
    `CursorMoved`; `RedrawRequested` -> `frame_ready = true`, clear the pending-redraw time.
  - `device_event`: `DeviceEvent::MouseMotion { delta }` -> `MouseMotion` while focused.
  - `about_to_wait`: apply `CursorOptions` if changed (`set_cursor_grab(Locked)`, fallback
    `Confined`, then `set_cursor_visible`); `occluded = winit_occluded || pending redraw older
    than 100 ms`, write `WindowOccluded` on change; `app.update()`; if `!frame_ready` and no
    redraw is pending: `request_redraw()` and record the time; `should_exit()` -> store and
    `event_loop.exit()`; control flow `Poll`, or `WaitUntil(now + wait)` in `Reactive` mode, or
    `WaitUntil(now + 33 ms)` while occluded (without it a hidden window spins at ~200k
    updates/s).
  - `exiting`: `app.shutdown()`, then `world_mut().remove_non_send::<PrimaryWindow>()`.
  - Return the stored `AppExit`, `Success` by default, `AppExit::error()` if `run_app` fails.
- `examples/window_smoke.rs` (section 9.5).

Public API (`boxel_window`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_window/src/input.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyboardInput {
    pub key: KeyCode,
    pub state: ButtonState,
    pub repeat: bool,
}

#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseButtonInput {
    pub button: MouseButton,
    pub state: ButtonState,
}

#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct MouseMotion {
    pub delta: Vec2,
}

#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct MouseWheel {
    pub delta: Vec2,
}

#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct CursorMoved {
    pub position: Vec2,
}

#[derive(Resource, Debug, Clone)]
pub struct ButtonInput<T: Copy + Eq + Hash + Send + Sync + 'static> {
    pressed: HashSet<T>,
    just_pressed: HashSet<T>,
    just_released: HashSet<T>,
}

impl<T: Copy + Eq + Hash + Send + Sync + 'static> Default for ButtonInput<T> {
    fn default() -> Self;
}

impl<T: Copy + Eq + Hash + Send + Sync + 'static> ButtonInput<T> {
    pub fn press(&mut self, input: T);
    pub fn release(&mut self, input: T);
    pub fn pressed(&self, input: T) -> bool;
    pub fn just_pressed(&self, input: T) -> bool;
    pub fn just_released(&self, input: T) -> bool;
    pub fn any_pressed(&self, inputs: impl IntoIterator<Item = T>) -> bool;
    pub fn get_pressed(&self) -> impl Iterator<Item = &T>;
    pub fn clear_just(&mut self);
    pub fn reset_all(&mut self);
}

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct AccumulatedMouseMotion {
    pub delta: Vec2,
}

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct AccumulatedMouseScroll {
    pub delta: Vec2,
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct InputSystems;

pub fn keyboard_input_system(
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut events: MessageReader<KeyboardInput>,
);

pub fn mouse_button_input_system(
    mut buttons: ResMut<ButtonInput<MouseButton>>,
    mut events: MessageReader<MouseButtonInput>,
);

pub fn accumulate_mouse_motion_system(
    mut acc: ResMut<AccumulatedMouseMotion>,
    mut events: MessageReader<MouseMotion>,
);

pub fn accumulate_mouse_scroll_system(
    mut acc: ResMut<AccumulatedMouseScroll>,
    mut events: MessageReader<MouseWheel>,
);

#[derive(Default)]
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App);
}

// ---- crates/boxel_window/src/lib.rs
pub use input::{
    AccumulatedMouseMotion, AccumulatedMouseScroll, ButtonInput, ButtonState, CursorMoved,
    InputPlugin, InputSystems, KeyboardInput, MouseButtonInput, MouseMotion, MouseWheel,
    accumulate_mouse_motion_system, accumulate_mouse_scroll_system, keyboard_input_system,
    mouse_button_input_system,
};
pub use runner::{UpdateMode, winit_runner};
pub use window::{
    CursorGrab, CursorOptions, DisplayHandle, PrimaryWindow, WindowCloseRequested, WindowCreated,
    WindowFocused, WindowMode, WindowOccluded, WindowPlugin, WindowResized, WindowSettings,
    WindowState,
};

pub use winit;
pub use winit::event::MouseButton;
pub use winit::keyboard::KeyCode;

pub mod prelude {
    pub use crate::{
        AccumulatedMouseMotion, AccumulatedMouseScroll, ButtonInput, CursorGrab, CursorOptions,
        InputPlugin, KeyCode, MouseButton, PrimaryWindow, WindowPlugin, WindowSettings,
        WindowState,
    };
}

// ---- crates/boxel_window/src/runner.rs
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateMode {
    #[default]
    Continuous,
    Reactive {
        wait: Duration,
    },
}

pub fn winit_runner(app: App, event_loop: EventLoop<()>) -> AppExit;

// ---- crates/boxel_window/src/window.rs
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowMode {
    #[default]
    Windowed,
    BorderlessFullscreen,
}

#[derive(Resource, Clone, Debug)]
pub struct WindowSettings {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub resizable: bool,
    pub mode: WindowMode,
}

impl Default for WindowSettings {
    fn default() -> Self;
}

#[derive(Clone, Debug)]
pub struct WindowPlugin {
    pub primary: WindowSettings,
    pub exit_on_close: bool,
    pub update_mode: UpdateMode,
}

impl Default for WindowPlugin {
    fn default() -> Self;
}

impl Plugin for WindowPlugin {
    fn build(&self, app: &mut App);
}

#[derive(Resource, Clone, Debug)]
pub struct DisplayHandle(pub OwnedDisplayHandle);

#[derive(Clone, Debug)]
pub struct PrimaryWindow {
    pub window: Arc<Window>,
    pub id: WindowId,
}

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowState {
    pub physical_size: UVec2,
    pub scale_factor: f64,
    pub focused: bool,
    pub occluded: bool,
    pub frame_ready: bool,
}

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CursorOptions {
    pub grab: CursorGrab,
    pub visible: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorGrab {
    #[default]
    None,
    Locked,
}

#[derive(Message, Clone, Copy, Debug)]
pub struct WindowCreated;

#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowResized {
    pub width: u32,
    pub height: u32,
}

#[derive(Message, Clone, Copy, Debug)]
pub struct WindowCloseRequested;

#[derive(Message, Clone, Copy, Debug)]
pub struct WindowFocused(pub bool);

#[derive(Message, Clone, Copy, Debug)]
pub struct WindowOccluded(pub bool);
```

Tests (headless, no event loop): `ButtonInput` semantics; `InputPlugin` with `App::new()`
(write `KeyboardInput`/`MouseMotion` messages, `app.update()`, check resources); focus loss
releases keys. The runner is exercised by `window_smoke` (agent-verifiable) and by WP8/WP10.

### 5.7 `boxel_camera` (WP2)

`[S]`: `Camera` + `Default` + `projection`, `CameraView` and `CameraView::compute`
(`render_origin = floor(translation)`, `eye_rel` in [0, 1), `view = look_to(eye_rel, forward,
up)`, frustum far plane at `view_distance`), `FlyCamera` + `Default`.

`[P]`: `FlyCameraPlugin`: three systems in `Update` (set `FlyCameraSystems`) for entities with
`FlyCamera + Transform`, skipped when `!enabled`:

1. grab: `MouseButton::Left` just pressed -> `CursorOptions { grab: Locked, visible: false }`;
   `Escape` just pressed -> `{ None, true }`;
2. look (only while `CursorOptions.grab == Locked`): `yaw -= dx * sensitivity`,
   `pitch = clamp(pitch - dy * sensitivity, -89 deg, 89 deg)`,
   `transform.rotation = Quat::from_rotation_y(yaw) * Quat::from_rotation_x(pitch)`;
3. move: horizontal basis from yaw only (W/S forward/back, A/D left/right), Space up,
   ShiftLeft down, ControlLeft multiplies speed by `boost`;
   `translation += dir.normalize_or_zero() * speed * dt` (in f64).

Public API (`boxel_camera`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_camera/src/lib.rs
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub fov_y_radians: f32,
    pub near: f32,
    pub view_distance: f32,
    pub active: bool,
}

impl Default for Camera {
    fn default() -> Self;
}

impl Camera {
    pub fn projection(&self, aspect: f32) -> Mat4;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraView {
    pub render_origin: IVec3,
    pub eye_rel: Vec3,
    pub forward: Vec3,
    pub view: Mat4,
    pub proj: Mat4,
    pub view_proj: Mat4,
    pub frustum: Frustum,
}

impl CameraView {
    pub fn compute(camera: &Camera, transform: &Transform, aspect: f32) -> Self;
}

#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct FlyCamera {
    pub speed: f32,
    pub boost: f32,
    pub sensitivity: f32,
    pub yaw: f32,
    pub pitch: f32,
    pub enabled: bool,
}

impl Default for FlyCamera {
    fn default() -> Self;
}

#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub struct FlyCameraSystems;

#[derive(Default)]
pub struct FlyCameraPlugin;

impl Plugin for FlyCameraPlugin {
    fn build(&self, app: &mut App);
}

pub mod prelude {
    pub use crate::{Camera, CameraView, FlyCamera, FlyCameraPlugin};
}
```

Tests: `compute` puts `eye_rel` in [0, 1) at large coordinates (1e7); a point straight ahead
projects inside the frustum and one behind does not; fly systems with inserted input resources
and a manually advanced `Time` (`Time::update_with_instant`).

### 5.8 `boxel_gfx` (WP3)

Purpose: runtime GPU detection and the thin layer over wgpu (ADR 0001, 0002, 0010). No ECS.
Read ARCHITECTURE.md section 5 completely.

`[S]`: `Tier` (all), `floors`, `tier_request`, `DeviceClass` + `From<DeviceType>`,
`GlVersion::{parse, supports_storage_and_compute}`, `AdapterSummary::{from_info, key}`,
`AdapterCaps::{from_adapter, class, summary}`, `TierPolicy` + `Default`, `GpuHealth` (all),
`GpuContext` + `summary_line`, `GpuPreferences::{overlay, power_or_default, lkg_enabled}` and the
serde helpers, `PresentModePref`, `CLI_HELP`, `PRESENT_MODES`, `default_backends`,
`ImageDiff::fraction`, `LkgStore::with_path`, the LKG constants, `SelectError`, `Exclusion`,
`SelectRequest`, report types, `FrameAcquire`, `SmokeLevel` + `resolve`, `SmokeContext`, trait
`SmokeTest`, `submit_and_wait`, `run_in_error_scopes`, `choose_surface_format`,
`choose_alpha_mode`, `choose_present_mode`, `gpu_tests_required`, module `hal`,
`read_texture_rgba8`, `RgbaImage::{compare, non_background_fraction, save_png, load_png}`.
Contract tests: `tests/contract.rs` (tier floors, GL parsing, surface choices) and
`tests/gpu_contract.rs` (readback, PNG round trip, error scopes, and `tier_request` accepted by
every real adapter: verified on RADV, RTX 5050, lavapipe and NVIDIA GL 3.3).

`[P]` rules (the doc comments in the skeleton state most of them; summary):

- `classify`: the doc comment; every threshold is a `floors` constant (classification and the
  device request cannot disagree). Reasons are one line each (`"High: no
  MULTI_DRAW_INDIRECT_COUNT"`, `"Mid: GL 3.3 < 4.3 (no SSBO/compute in GLSL 330)"`,
  `"High: software adapters capped at Mid"`).
- `score`: the doc comment (Suspect LKG pairs get -500).
- `instance_flags`, `create_instance`, `create_headless_instance`, `create_gl_instance`:
  `create_instance` builds `InstanceDescriptor { backends, flags, display, .. }` from
  `new_with_display_handle(Box::new(d.clone()))` / `new_without_display_handle()`, then
  `desc.backend_options = desc.backend_options.with_env()`. Never call
  `InstanceDescriptor::with_env` (an empty `WGPU_BACKEND` would disable every backend). Run
  `wgpu::Instance::new` inside `std::panic::catch_unwind(AssertUnwindSafe(..))` with the panic
  hook swapped for a silent one during the call (`take_hook`/`set_hook`, restored right after);
  on panic record the message in `notes`, remove `GL`, retry once. Under `#[cfg(panic =
  "abort")]` leave GL out when other backends are requested and set `gl_deferred` (doc comment).
- `GpuPreferences::from_args`: flags of `CLI_HELP`, both `--flag value` and `--flag=value`;
  `--lkg` sets `use_lkg = Some(true)`, `--no-lkg` `Some(false)`; returns unconsumed args in
  order (`--help` stays there for the example). `from_boxel_env` (`BOXEL_LKG=1|0`,
  `BOXEL_NO_LKG=1` as an alias of `BOXEL_LKG=0`), `from_wgpu_env`, `from_config_file`, `resolve`:
  as documented; config schema: a `[gpu]` table with the struct's field names (`backends` as a
  comma list string, `power` `high|low|none`, `max_tier` `high|mid|low`, `present_mode`
  `vsync|mailbox|immediate`). Factor env reading through an internal function that takes a
  lookup closure so tests never mutate the process environment.
- `install_device_callbacks`: `device.on_uncaptured_error(Arc::new(move |e| { error!(..);
  health.record_error(..) }))` and `device.set_device_lost_callback(move |reason, msg|
  health.mark_lost(format!("{reason:?}: {msg}")))`.
- `BasicSmokeTest`: section 8.3. `select_gpu`, `init_headless`: section 8.1.
- LKG (`default_path`, `open`, `save`, `status`, `begin_attempt`, `confirm_good`,
  `cancel_pending`, `add_strike`, `clear`, `build_fingerprint`, `install_lkg_panic_hook`): the doc
  comments of `lkg.rs` (crash-only semantics, ADR 0002). TOML via `toml` + serde, file
  `version = 2`; keep at most 64 entries (drop the oldest `updated_unix`). The panic hook uses a
  process-global `OnceLock` so a second call is a no-op; it reloads the file, restores this
  pid's `Pending` entries and saves (errors ignored).
- `configure_surface`, `acquire_frame`: doc comments. `acquire_frame` maps
  `wgpu::CurrentSurfaceTexture::{Success, Suboptimal, Timeout, Occluded, Outdated, Lost,
  Validation}`.
- `SelectionReport::to_text`: table per adapter (name, backend, class, driver_info, GL version,
  max tier + reasons, surface support and chosen surface format, filtered reason, attempts with
  score/outcome/ms), notes, and the final `DECISION:` line (section 8.4).

Apple targets: `boxel_gfx::hal` exists only on Windows, Linux, Android and FreeBSD. wgpu's
`vulkan` feature reaches wgpu-hal only through `wgpu-core-deps-windows-linux-android`, so
`wgpu::hal::vulkan` does not exist on macOS (it would need `vulkan-portability`). The module is
cfg-gated on those targets, so `cargo clippy --all-features` passes on the macOS CI job.

Public API (`boxel_gfx`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_gfx/src/caps.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceClass {
    Discrete,
    Integrated,
    Virtual,
    Software,
    Other,
}

impl From<wgpu::DeviceType> for DeviceClass {
    fn from(t: wgpu::DeviceType) -> Self;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlVersion {
    pub major: u8,
    pub minor: u8,
    pub es: bool,
}

impl GlVersion {
    pub fn parse(driver_info: &str) -> Option<GlVersion>;

    pub fn supports_storage_and_compute(self) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AdapterSummary {
    pub name: String,
    pub backend: String,
    pub class: DeviceClass,
    pub vendor: u32,
    pub device: u32,
    pub driver: String,
    pub driver_info: String,
}

impl AdapterSummary {
    pub fn from_info(info: &wgpu::AdapterInfo) -> Self;

    pub fn key(&self) -> String;
}

#[derive(Clone, Debug)]
pub struct AdapterCaps {
    pub info: wgpu::AdapterInfo,
    pub features: wgpu::Features,
    pub limits: wgpu::Limits,
    pub downlevel: wgpu::DownlevelCapabilities,
    pub gl_version: Option<GlVersion>,
}

impl AdapterCaps {
    pub fn from_adapter(adapter: &wgpu::Adapter) -> Self;

    pub fn class(&self) -> DeviceClass;

    pub fn summary(&self) -> AdapterSummary;
}

// ---- crates/boxel_gfx/src/classify.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TierPolicy {
    pub dx12_high: bool,
    pub software_max: Tier,
}

impl Default for TierPolicy {
    fn default() -> Self;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Classification {
    pub max_tier: Option<Tier>,
    pub reasons: Vec<String>,
}

pub fn classify(caps: &AdapterCaps, policy: TierPolicy) -> Classification;

// ---- crates/boxel_gfx/src/context.rs
#[derive(Debug, Default)]
pub struct GpuHealth {
    lost: AtomicBool,
    lost_reason: Mutex<Option<String>>,
    errors: AtomicU32,
    last_error: Mutex<Option<String>>,
}

impl GpuHealth {
    pub fn is_lost(&self) -> bool;
    pub fn mark_lost(&self, reason: String);
    pub fn lost_reason(&self) -> Option<String>;
    pub fn record_error(&self, message: String);
    pub fn error_count(&self) -> u32;
    pub fn last_error(&self) -> Option<String>;
}

pub fn install_device_callbacks(device: &wgpu::Device, health: &Arc<GpuHealth>);

#[derive(Debug)]
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub caps: AdapterCaps,
    pub tier: Tier,
    pub class: DeviceClass,
    pub output_format: wgpu::TextureFormat,
    pub health: Arc<GpuHealth>,
    pub report: SelectionReport,
}

impl GpuContext {
    pub fn summary_line(&self) -> String;
}

// ---- crates/boxel_gfx/src/hal.rs
pub unsafe fn with_vulkan_device<R>(
    device: &wgpu::Device,
    f: impl FnOnce(&wgpu::hal::vulkan::Device) -> R,
) -> Option<R>;

// ---- crates/boxel_gfx/src/instance.rs
pub fn default_backends() -> wgpu::Backends;

pub fn instance_flags(prefs: &GpuPreferences) -> wgpu::InstanceFlags;

#[derive(Debug)]
pub struct CreatedInstance {
    pub instance: wgpu::Instance,
    pub backends: wgpu::Backends,
    pub flags: wgpu::InstanceFlags,
    pub notes: Vec<String>,
    pub gl_deferred: bool,
}

pub fn create_instance<D>(
    display: Option<D>,
    prefs: &GpuPreferences,
) -> Result<CreatedInstance, crate::SelectError>
where
    D: WgpuHasDisplayHandle + Clone,;

pub fn create_headless_instance(
    prefs: &GpuPreferences,
) -> Result<CreatedInstance, crate::SelectError>;

pub fn create_gl_instance<D>(
    display: Option<D>,
    prefs: &GpuPreferences,
) -> Result<CreatedInstance, crate::SelectError>
where
    D: WgpuHasDisplayHandle + Clone,;

// ---- crates/boxel_gfx/src/lib.rs
#[cfg(all(
    feature = "hal",
    any(
        windows,
        target_os = "linux",
        target_os = "android",
        target_os = "freebsd"
    )
))]
pub mod hal;

pub use caps::{AdapterCaps, AdapterSummary, DeviceClass, GlVersion};
pub use classify::{Classification, TierPolicy, classify};
pub use context::{GpuContext, GpuHealth, install_device_callbacks};
pub use instance::{
    CreatedInstance, create_gl_instance, create_headless_instance, create_instance,
    default_backends, instance_flags,
};
pub use lkg::{
    LastKnownGood, LkgEntry, LkgError, LkgStatus, LkgStore, PENDING_GRACE_SECS, STRIKES_FOR_BAD,
    WGPU_VERSION, build_fingerprint, install_lkg_panic_hook,
};
pub use prefs::{CLI_HELP, GpuPreferences, PRESENT_MODES, PrefsError, PresentModePref};
pub use readback::{ImageDiff, RgbaImage, read_texture_rgba8};
pub use report::{Attempt, AttemptOutcome, CandidateReport, SelectionReport};
pub use score::score;
pub use select::{Exclusion, SelectError, SelectRequest, init_headless, select_gpu};
pub use smoke::{
    BasicSmokeTest, SmokeContext, SmokeLevel, SmokeTest, run_in_error_scopes, submit_and_wait,
};
pub use surface::{
    FrameAcquire, acquire_frame, choose_alpha_mode, choose_present_mode, choose_surface_format,
    configure_surface,
};
pub use tier::{Tier, TierRequest, floors, tier_request};

pub use wgpu;

pub fn gpu_tests_required() -> bool;

// ---- crates/boxel_gfx/src/lkg.rs
pub const WGPU_VERSION: &str = "30.0.1";
pub const PENDING_GRACE_SECS: u64 = 30;
pub const STRIKES_FOR_BAD: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LkgStatus {
    Pending,
    Good,
    Suspect,
    Bad,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LkgEntry {
    pub adapter: AdapterSummary,
    pub tier: Tier,
    pub status: LkgStatus,
    #[serde(default)]
    pub strikes: u32,
    pub boxel_version: String,
    pub wgpu_version: String,
    #[serde(default)]
    pub build: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub previous: Option<LkgStatus>,
    pub updated_unix: u64,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastKnownGood {
    pub version: u32,
    pub entries: Vec<LkgEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum LkgError {
    #[error("last-known-good file {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("last-known-good file {path} is invalid: {message}")]
    Parse {
        path: String,
        message: String,
    },
}

#[derive(Clone, Debug, Default)]
pub struct LkgStore {
    pub path: Option<PathBuf>,
    pub build: String,
    pub read_only: bool,
    pub data: LastKnownGood,
}

impl LkgStore {
    pub fn default_path(app_name: &str) -> Option<PathBuf>;

    pub fn open(path: Option<PathBuf>, build: String) -> (LkgStore, Option<LkgError>);

    pub fn save(&self) -> Result<(), LkgError>;

    pub fn status(&self, adapter: &AdapterSummary, tier: Tier) -> Option<LkgStatus>;

    pub fn begin_attempt(&mut self, adapter: &AdapterSummary, tier: Tier);

    pub fn confirm_good(&mut self, adapter: &AdapterSummary, tier: Tier);

    pub fn cancel_pending(&mut self);

    pub fn add_strike(&mut self, adapter: &AdapterSummary, tier: Tier, reason: String);

    pub fn clear(&mut self);

    pub fn with_path(path: &Path) -> LkgStore;
}

pub fn build_fingerprint() -> String;

pub fn install_lkg_panic_hook(path: PathBuf);

// ---- crates/boxel_gfx/src/prefs.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresentModePref {
    #[default]
    Vsync,
    Mailbox,
    Immediate,
}

pub const PRESENT_MODES: &str = "vsync|mailbox|immediate";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GpuPreferences {
    #[serde(with = "backends_serde")]
    pub backends: Option<wgpu::Backends>,
    pub adapter: Option<String>,
    #[serde(with = "power_serde")]
    pub power: Option<wgpu::PowerPreference>,
    pub max_tier: Option<Tier>,
    pub software: Option<bool>,
    pub validation: Option<bool>,
    pub use_lkg: Option<bool>,
    pub present_mode: Option<PresentModePref>,
}

pub const CLI_HELP: &str = "\
GPU options (also BOXEL_* / WGPU_* env vars, see docs):
  --backend <list>     vulkan,gl,dx12,metal (comma list)      [BOXEL_BACKEND, WGPU_BACKEND]
  --adapter <substr>   pick adapter by name substring         [BOXEL_ADAPTER, WGPU_ADAPTER_NAME]
  --power <high|low>   power preference                       [BOXEL_POWER, WGPU_POWER_PREF]
  --tier <high|mid|low> cap the render tier                   [BOXEL_TIER]
  --software           only CPU rasterizers (lavapipe/llvmpipe/WARP) [BOXEL_SOFTWARE=1]
  --validation         backend validation layers              [BOXEL_GPU_VALIDATION=1]
  --lkg / --no-lkg     use / ignore last-known-good (default: on in release builds) [BOXEL_LKG=1|0]
  --present <mode>     vsync|mailbox|immediate                [BOXEL_PRESENT]
  --gpu-config <path>  TOML file with a [gpu] table           [BOXEL_GPU_CONFIG]
";

#[derive(Debug, thiserror::Error)]
pub enum PrefsError {
    #[error("invalid value {value:?} for {key}: expected {expected}")]
    BadValue {
        key: String,
        value: String,
        expected: &'static str,
    },
    #[error("missing value for {0}")]
    MissingValue(String),
    #[error("cannot read GPU config {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid GPU config {path}: {message}")]
    Parse {
        path: String,
        message: String,
    },
}

impl GpuPreferences {
    pub fn overlay(self, lower: GpuPreferences) -> GpuPreferences;

    pub fn from_args(
        args: &[String],
    ) -> Result<(GpuPreferences, Option<std::path::PathBuf>, Vec<String>), PrefsError>;

    pub fn from_boxel_env() -> (GpuPreferences, Vec<String>);

    pub fn from_wgpu_env() -> (GpuPreferences, Vec<String>);

    pub fn from_config_file(path: &Path) -> Result<GpuPreferences, PrefsError>;

    pub fn resolve(
        cli: GpuPreferences,
        config_path: Option<&Path>,
    ) -> (GpuPreferences, Vec<String>);

    pub fn lkg_enabled(&self) -> bool;

    pub fn power_or_default(&self) -> wgpu::PowerPreference;
}

mod backends_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    #[expect(clippy::trivially_copy_pass_by_ref, reason = "serde `with` signature")]
    pub fn serialize<S: Serializer>(v: &Option<wgpu::Backends>, s: S) -> Result<S::Ok, S::Error>;

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<wgpu::Backends>, D::Error>;
}

mod power_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    #[expect(clippy::trivially_copy_pass_by_ref, reason = "serde `with` signature")]
    pub fn serialize<S: Serializer>(
        v: &Option<wgpu::PowerPreference>,
        s: S,
    ) -> Result<S::Ok, S::Error>;

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<wgpu::PowerPreference>, D::Error>;
}

// ---- crates/boxel_gfx/src/readback.rs
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageDiff {
    pub differing_pixels: usize,
    pub total_pixels: usize,
    pub max_channel_diff: u8,
}

impl ImageDiff {
    pub fn fraction(&self) -> f64;
}

impl RgbaImage {
    pub fn compare(&self, other: &RgbaImage, tolerance: u8) -> ImageDiff;

    pub fn non_background_fraction(&self) -> f64;

    #[cfg(feature = "png")]
    pub fn save_png(&self, path: &std::path::Path) -> std::io::Result<()>;

    #[cfg(feature = "png")]
    pub fn load_png(path: &std::path::Path) -> std::io::Result<RgbaImage>;
}

pub fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<RgbaImage, String>;

// ---- crates/boxel_gfx/src/report.rs
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptOutcome {
    Ok,
    RequestDeviceFailed(String),
    SmokeFailed(String),
    SkippedKnownBad,
    SkippedExcluded,
    NotAttempted,
}

#[derive(Clone, Debug)]
pub struct Attempt {
    pub tier: Tier,
    pub score: i64,
    pub outcome: AttemptOutcome,
    pub elapsed: Duration,
}

#[derive(Clone, Debug)]
pub struct CandidateReport {
    pub adapter: AdapterSummary,
    pub classification: Classification,
    pub surface_supported: Option<bool>,
    pub surface_format: Option<wgpu::TextureFormat>,
    pub filtered: Option<String>,
    pub attempts: Vec<Attempt>,
}

#[derive(Clone, Debug, Default)]
pub struct SelectionReport {
    pub backends: wgpu::Backends,
    pub flags: wgpu::InstanceFlags,
    pub candidates: Vec<CandidateReport>,
    pub chosen: Option<(usize, Tier)>,
    pub notes: Vec<String>,
}

impl SelectionReport {
    pub fn to_text(&self) -> String;
}

// ---- crates/boxel_gfx/src/score.rs
pub fn score(
    caps: &AdapterCaps,
    tier: Tier,
    power: wgpu::PowerPreference,
    lkg: Option<LkgStatus>,
) -> i64;

// ---- crates/boxel_gfx/src/select.rs
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exclusion {
    pub adapter_key: String,
    pub max_tier: Option<Tier>,
}

pub struct SelectRequest<'a> {
    pub instance: &'a CreatedInstance,
    pub surface: Option<&'a wgpu::Surface<'static>>,
    pub prefs: &'a GpuPreferences,
    pub policy: TierPolicy,
    pub lkg: Option<&'a mut LkgStore>,
    pub exclusions: &'a [Exclusion],
    pub smoke: &'a dyn SmokeTest,
    pub smoke_level: SmokeLevel,
    pub create_devices: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SelectError {
    #[error("no graphics backend could be initialized: {0}")]
    NoInstance(String),
    #[error("no usable GPU adapter:\n{}", .0.to_text())]
    NoUsableAdapter(Box<SelectionReport>),
}

pub fn select_gpu(req: SelectRequest<'_>) -> Result<GpuContext, SelectError>;

pub fn init_headless(prefs: &GpuPreferences) -> Result<GpuContext, SelectError>;

// ---- crates/boxel_gfx/src/smoke.rs
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SmokeLevel {
    #[default]
    Auto,
    Compile,
    Draw,
}

impl SmokeLevel {
    pub fn resolve(self, caps: &AdapterCaps) -> SmokeLevel;
}

pub struct SmokeContext<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub tier: Tier,
    pub caps: &'a AdapterCaps,
    pub level: SmokeLevel,
    pub output_format: wgpu::TextureFormat,
}

pub trait SmokeTest: Send + Sync {
    fn run(&self, ctx: &SmokeContext<'_>) -> Result<(), String>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BasicSmokeTest;

impl SmokeTest for BasicSmokeTest {
    fn run(&self, ctx: &SmokeContext<'_>) -> Result<(), String>;
}

pub fn submit_and_wait(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    encoder: wgpu::CommandEncoder,
) -> Result<(), String>;

pub fn run_in_error_scopes(
    device: &wgpu::Device,
    f: impl FnOnce() -> Result<(), String>,
) -> Result<(), String>;

// ---- crates/boxel_gfx/src/surface.rs
#[derive(Debug)]
pub enum FrameAcquire {
    Frame {
        texture: wgpu::SurfaceTexture,
        suboptimal: bool,
    },
    Skip,
    Reconfigure,
    Recreate,
}

pub fn choose_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat>;

pub fn choose_alpha_mode(modes: &[wgpu::CompositeAlphaMode]) -> wgpu::CompositeAlphaMode;

pub fn choose_present_mode(
    pref: PresentModePref,
    modes: &[wgpu::PresentMode],
) -> wgpu::PresentMode;

pub fn configure_surface(
    surface: &wgpu::Surface<'_>,
    gpu: &GpuContext,
    width: u32,
    height: u32,
    present: PresentModePref,
) -> Result<wgpu::SurfaceConfiguration, String>;

pub fn acquire_frame(surface: &wgpu::Surface<'_>) -> FrameAcquire;

// ---- crates/boxel_gfx/src/tier.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Low = 1,
    Mid = 2,
    High = 3,
}

impl Tier {
    pub const DESCENDING: [Tier; 3] = [Tier::High, Tier::Mid, Tier::Low];

    pub const fn lower(self) -> Option<Tier>;

    pub const fn as_str(self) -> &'static str;

    pub fn parse(s: &str) -> Option<Tier>;
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result;
}

#[derive(Clone, Debug)]
pub struct TierRequest {
    pub features: wgpu::Features,
    pub limits: wgpu::Limits,
}

pub mod floors {
    pub const IMMEDIATE_SIZE: u32 = 16;
    pub const VERTEX_BUFFERS: u32 = 1;
    pub const TEXTURE_ARRAY_LAYERS: u32 = 256;
    pub const STORAGE_BUFFERS_PER_STAGE: u32 = 4;
    pub const STORAGE_BINDING_SIZE: u64 = 128 << 20;
    pub const WORKGROUP_SIZE_X: u32 = 64;
    pub const INVOCATIONS_PER_WORKGROUP: u32 = 64;
    pub const WORKGROUPS_PER_DIMENSION: u32 = 1024;
}

pub fn tier_request(tier: Tier, adapter_limits: &wgpu::Limits) -> TierRequest;
```

Tests to add:

- Unit or `tests/contract.rs` additions (no GPU): `classify` with synthetic `AdapterCaps` built
  from `wgpu::AdapterInfo { .. }` (plain struct: name, vendor, device, device_type,
  device_pci_bus_id, driver, driver_info, backend, subgroup_min_size, subgroup_max_size,
  transient_saves_memory, limit_bucket), `Features`, `Limits`, `DownlevelCapabilities`. Required
  cases: RADV-like Vulkan -> High; same with `DeviceType::Cpu` -> Mid (software cap) with a
  reason; NVIDIA GL 3.3 (claims `VERTEX_STORAGE | COMPUTE_SHADERS`) -> Low with reason
  "GL 3.3 < 4.3"; radeonsi GL 4.6 (no MDIC) -> Mid; Metal-like (no MDIC) -> Mid; DX12 with MDIC
  -> Mid unless `dx12_high`; no IMMEDIATES -> None; one synthetic adapter sitting exactly at each
  `floors` value classifies for its tier and one just below does not. `score` ordering (High >
  Mid; discrete before integrated with HighPerformance and the reverse with LowPower; Software
  last; Suspect below the other adapters of its tier). `GpuPreferences`: `from_args` (both
  syntaxes, unknown flags left), `resolve` precedence with injected values, config file parsing,
  `lkg_enabled` default. `SmokeLevel::resolve`. `instance_flags` never contains
  `VALIDATION_INDIRECT_CALL` unless the env var is set. A naga validation test of the
  `BasicSmokeTest` WGSL. LKG in `tests/lkg.rs` (uses `CARGO_TARGET_TMPDIR`): pending from another
  pid older than 30 s becomes Suspect then Bad on the second strike; a fresh pending (< 30 s) is
  left alone; `cancel_pending` restores the previous status; `confirm_good` resets strikes;
  `read_only` never writes; entries with another `build` are ignored; corrupt and version-1
  files give an empty store plus an error; atomic save.
- GPU (`tests/gpu_select.rs`, self-skipping): `init_headless(&GpuPreferences::default())`
  succeeds and its `report.to_text()` ends with a `DECISION:` line; for every adapter,
  `BasicSmokeTest` at `SmokeLevel::Draw` passes for every tier `classify` allows (with
  `TierPolicy { software_max: High, .. }`), with `output_format` `Rgba8UnormSrgb` and
  `Bgra8Unorm` (both blit entry points).

Acceptance: unit + GPU tests pass on this machine (all adapters, plus
`__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json WGPU_BACKEND=gl`),
`gpu_info` output matches section 12 rows 4 to 8.

### 5.9 `boxel_shaders` (WP7)

All code `[S]`: WGSL text is final (section 6) and `tests/naga.rs` (7 tests, section 6.7)
passes. The owner may fix WGSL only if a test proves a defect (record a deviation; the lead must
approve because the layout is frozen across WP5/WP7).

Public API (`boxel_shaders`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_shaders/src/gpu.rs
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Pod, Zeroable)]
pub struct ViewUniform {
    pub view_proj: [[f32; 4]; 4],
    pub frustum: [[f32; 4]; 6],
    pub render_origin: [i32; 4],
    pub camera_rel: [f32; 4],
    pub fog_color: [f32; 4],
    pub fog_params: [f32; 4],
    pub counts: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct ChunkGpu {
    pub origin: [i32; 3],
    pub quad_base: u32,
    pub counts: [u32; 8],
    pub flags: u32,
    pub pad: [u32; 3],
}

impl ChunkGpu {
    pub const FLAG_LIVE: u32 = 1;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct DrawRef {
    pub chunk_slot: u32,
    pub quad_start: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct ChunkImmediate {
    pub origin: [i32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Pod, Zeroable)]
pub struct DrawIndexedIndirect {
    pub index_count: u32,
    pub instance_count: u32,
    pub first_index: u32,
    pub base_vertex: i32,
    pub first_instance: u32,
}

pub mod bindings {
    pub const GROUP_MATERIALS: u32 = 0;
    pub const GROUP_VIEW: u32 = 1;
    pub const GROUP_GEOMETRY: u32 = 2;
    pub const CULL_GROUP: u32 = 0;
    pub const MAX_DRAWS_PER_LIST: u32 = 65535;
    pub const LOW_IMMEDIATE_SIZE: u32 = 16;
    pub const CULL_WORKGROUP_SIZE: u32 = 64;
}

// ---- crates/boxel_shaders/src/lib.rs
pub mod gpu;

pub const COMMON_WGSL: &str = include_str!("../shaders/common.wgsl");
pub const VOXEL_PULL_WGSL: &str = include_str!("../shaders/voxel_pull.wgsl");
pub const VOXEL_INSTANCED_WGSL: &str = include_str!("../shaders/voxel_instanced.wgsl");
pub const CULL_WGSL: &str = include_str!("../shaders/cull.wgsl");
pub const BLIT_WGSL: &str = include_str!("../shaders/blit.wgsl");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Vertex,
    Fragment,
    Compute,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ShaderModuleId {
    VoxelPull,
    VoxelInstanced,
    Cull,
    Blit,
}

static VOXEL_PULL: LazyLock<String> = LazyLock::new(|| format!("{COMMON_WGSL}\n{VOXEL_PULL_WGSL}"));
static VOXEL_INSTANCED: LazyLock<String> =
    LazyLock::new(|| format!("{COMMON_WGSL}\n{VOXEL_INSTANCED_WGSL}"));
static CULL: LazyLock<String> = LazyLock::new(|| format!("{COMMON_WGSL}\n{CULL_WGSL}"));

impl ShaderModuleId {
    pub const ALL: [ShaderModuleId; 4] = [
        ShaderModuleId::VoxelPull,
        ShaderModuleId::VoxelInstanced,
        ShaderModuleId::Cull,
        ShaderModuleId::Blit,
    ];

    pub const fn label(self) -> &'static str;

    pub fn source(self) -> &'static str;

    pub const fn entry_points(self) -> &'static [(&'static str, Stage)];

    pub const fn uses_immediates(self) -> bool;

    pub const fn min_gl(self) -> (u8, u8);
}

pub fn create_module(device: &wgpu::Device, id: ShaderModuleId) -> wgpu::ShaderModule;
```

### 5.10 `boxel_render` (WP7; `src/plugin.rs` WP8; `tests/gpu_terrain.rs` WP9)

Purpose: the renderer (ADR 0007, 0010, 0011, 0012). Read ARCHITECTURE.md section 10 and section
7 of this spec.

`[S]`: all public types and their defaults, `ViewParams::from_camera`, `VoxelRenderer::tier`,
`MAIN_COLOR_FORMAT`, `DEPTH_FORMAT`, `BLOCK_TEXTURE_SIZE`.

`[P]`: WP7: every `VoxelRenderer` method, `RendererSmokeTest::run`, `generate_block_textures`,
the internal modules (`arena`, `chunk_table`, `targets`, `path/*`), unit tests and
`tests/gpu_tiers.rs`. WP8: `RenderPlugin::build` and the render systems (`src/plugin.rs`,
section 7.6). WP7 must not edit `src/plugin.rs`; WP8 must not edit anything else in the crate.

Public API (`boxel_render`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_render/src/lib.rs
pub use plugin::{
    DebugDestroyDevice, GpuFailed, GpuReady, Render, RenderInfo, RenderPlugin, RenderSettings,
    RenderSystems, RequestScreenshot, ScreenshotSaved,
};
pub use renderer::{RenderStats, RendererConfig, RendererError, ViewParams, VoxelRenderer};
pub use smoke::RendererSmokeTest;
pub use textures::{BLOCK_TEXTURE_SIZE, BlockTextureData, generate_block_textures};

pub const MAIN_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;

pub mod prelude {
    pub use crate::{RenderInfo, RenderPlugin, RenderSettings, RequestScreenshot, ScreenshotSaved};
}

// ---- crates/boxel_render/src/plugin.rs
#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Render;

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderSystems {
    Setup,
    Extract,
    Prepare,
    Draw,
    Cleanup,
}

#[derive(Resource, Clone, Debug)]
pub struct RenderSettings {
    pub app_name: String,
    pub gpu: GpuPreferences,
    pub gpu_config: Option<PathBuf>,
    pub smoke_level: SmokeLevel,
    pub tier_policy: TierPolicy,
    pub clear_color: [f32; 4],
    pub fog_start_fraction: f32,
    pub arena_quads: u32,
    pub max_chunk_slots: u32,
    pub render_when_hidden: bool,
}

impl Default for RenderSettings {
    fn default() -> Self;
}

#[derive(Resource, Clone, Debug, Default)]
pub struct RenderInfo {
    pub ready: bool,
    pub tier: Option<Tier>,
    pub class: Option<DeviceClass>,
    pub summary: String,
    pub frames_presented: u64,
    pub frames_rendered: u64,
    pub chunks: u32,
    pub draws: u32,
    pub recoveries: u32,
    pub dropped_chunks: u32,
}

#[derive(Message, Clone, Debug)]
pub struct GpuReady {
    pub summary: String,
    pub tier: Tier,
}

#[derive(Message, Clone, Debug)]
pub struct GpuFailed {
    pub error: String,
}

#[derive(Message, Clone, Debug)]
pub struct RequestScreenshot {
    pub path: PathBuf,
}

#[derive(Message, Clone, Debug)]
pub struct ScreenshotSaved {
    pub path: PathBuf,
    pub result: Result<(), String>,
}

#[derive(Message, Clone, Copy, Debug, Default)]
pub struct DebugDestroyDevice;

#[derive(Clone, Debug, Default)]
pub struct RenderPlugin {
    pub settings: RenderSettings,
}

impl Plugin for RenderPlugin {
    fn build(&self, app: &mut App);
}

// ---- crates/boxel_render/src/renderer.rs
#[derive(Clone, Debug)]
pub struct RendererConfig {
    pub arena_quads: u32,
    pub max_chunk_slots: u32,
    pub clear_color: [f32; 4],
}

impl Default for RendererConfig {
    fn default() -> Self;
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewParams {
    pub view_proj: Mat4,
    pub frustum: Frustum,
    pub render_origin: IVec3,
    pub eye_rel: Vec3,
    pub fog_color: [f32; 4],
    pub fog_start: f32,
    pub fog_end: f32,
}

impl ViewParams {
    pub fn from_camera(
        view: &CameraView,
        camera: &Camera,
        fog_color: [f32; 4],
        fog_start_fraction: f32,
    ) -> Self;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    pub chunks: u32,
    pub arena_used_quads: u32,
    pub arena_capacity_quads: u32,
    pub visible_chunks: u32,
    pub draws: u32,
    pub dropped_chunks: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum RendererError {
    #[error("quad arena full ({needed} quads requested)")]
    ArenaFull {
        needed: u32,
    },
    #[error("chunk table full ({0} slots)")]
    ChunkTableFull(u32),
    #[error("pipeline creation failed: {0}")]
    Pipeline(String),
}

pub struct VoxelRenderer {
    tier: Tier,
}

impl VoxelRenderer {
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tier: Tier,
        config: &RendererConfig,
        textures: &[TextureSpec],
        output_format: wgpu::TextureFormat,
        size: UVec2,
    ) -> Result<Self, RendererError>;
    pub fn tier(&self) -> Tier;
    pub fn resize(&mut self, device: &wgpu::Device, size: UVec2);
    pub fn upsert_chunk(
        &mut self,
        queue: &wgpu::Queue,
        id: u64,
        origin: IVec3,
        mesh: &ChunkMesh,
    ) -> Result<(), RendererError>;
    pub fn remove_chunk(&mut self, queue: &wgpu::Queue, id: u64) -> bool;
    pub fn clear_chunks(&mut self, queue: &wgpu::Queue);
    pub fn chunk_count(&self) -> usize;
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &ViewParams,
    );
    pub fn blit(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView);
    pub fn main_color(&self) -> &wgpu::Texture;
    pub fn stats(&self) -> RenderStats;
}

// ---- crates/boxel_render/src/smoke.rs
#[derive(Clone, Copy, Debug, Default)]
pub struct RendererSmokeTest;

impl SmokeTest for RendererSmokeTest {
    fn run(&self, ctx: &SmokeContext<'_>) -> Result<(), String>;
}

// ---- crates/boxel_render/src/textures.rs
pub const BLOCK_TEXTURE_SIZE: u32 = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockTextureData {
    pub layers: u32,
    pub mip_levels: u32,
    pub data: Vec<u8>,
}

pub fn generate_block_textures(specs: &[TextureSpec]) -> BlockTextureData;
```

Tests (WP7): `tests/gpu_tiers.rs` (section 9.4); unit tests for the arena allocator wrapper
(allocate, free, reuse, full), chunk table slot reuse, CPU culling (bucket visibility rules match
`cull.wgsl`, cutout bucket uses frustum only), `generate_block_textures` (sizes, determinism,
leaves alpha binary in every mip). WP8: a test that a system added to `Render` runs on the
calling thread when `TaskPoolPlugin` is present (proves the single-threaded executor; the fork
has no executor-kind getter), and that a system added to `Render` by an earlier plugin survives
`RenderPlugin::build`.

### 5.11 `boxel_voxel` (WP4)

Purpose: the voxel data model (ADR 0005). Pure.

`[S]`: all of `coords`, `BlockState`, `RenderShape`, `StateFlags`, texture/def types,
`BlockRegistryBuilder` (all methods), `BlockRegistry` accessors and `default_blocks` (frozen
ids: 0 air, 1 stone, 2 dirt, 3 grass, 4 sand, 5 gravel, 6 snow, 7 bedrock, 8 log, 9 leaves
(Cutout, solid), 10 water (Translucent, not solid), 11 planks; 13 procedural textures),
`Paletted::{get, as_uniform, decode_into}`, `Default` impls,
`ChunkData::{uniform, get, as_uniform, to_dense}` and the baselines `ChunkData::set` (widens to
`Direct` on the first change) and `ChunkData::from_dense` (`Uniform` or `Direct`).

`[P]` rules:

- `Paletted::set`: Uniform -> Packed(1) on the first different state; in Packed: if the state is
  in the palette write its index; else if `palette.len() < 1 << bits` push and write; else widen
  to the next width (1 -> 2 -> 4 -> 8) by re-encoding, or to `Direct` past 256 entries.
- `Paletted::encode`: distinct states in first-seen order; 1 -> `Uniform`; <= 2, 4, 16, 256 ->
  Packed with bits 1, 2, 4, 8; more -> `Direct`. `palette_len`, `bits_per_voxel`, `heap_bytes`,
  `compact` (rebuild with only used entries; may collapse to Uniform).
- Switch `ChunkData::set` to `Paletted::set` and `ChunkData::from_dense` to `Paletted::encode`
  (observable behaviour unchanged: the contract tests keep passing).
- `ChunkData::opaque_occupancy` (bit `z` of word `y * 32 + x`), `ChunkData::heap_bytes`.

Public API (`boxel_voxel`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_voxel/src/chunk.rs
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkData {
    pub blocks: Paletted,
    pub light: LightStorage,
    pub version: u32,
}

impl ChunkData {
    pub fn uniform(state: BlockState) -> Self;
    #[inline]
    pub fn get(&self, p: LocalPos) -> BlockState;
    pub fn as_uniform(&self) -> Option<BlockState>;
    pub fn set(&mut self, p: LocalPos, state: BlockState) -> BlockState;
    pub fn from_dense(states: &[BlockState]) -> Self;
    pub fn to_dense(&self, out: &mut [BlockState]);
    pub fn opaque_occupancy(&self, registry: &BlockRegistry) -> Box<Occupancy>;
    pub fn heap_bytes(&self) -> usize;
}

// ---- crates/boxel_voxel/src/coords.rs
pub const CHUNK_SHIFT: u32 = 5;
pub const CHUNK_SIZE: i32 = 1 << CHUNK_SHIFT;
pub const CHUNK_MASK: i32 = CHUNK_SIZE - 1;
pub const CHUNK_AREA: usize = (CHUNK_SIZE * CHUNK_SIZE) as usize;
pub const CHUNK_VOLUME: usize = CHUNK_AREA * CHUNK_SIZE as usize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl ChunkPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self;
    pub const fn from_ivec3(v: IVec3) -> Self;
    pub const fn as_ivec3(self) -> IVec3;
    pub const fn origin(self) -> BlockPos;
    pub const fn containing(p: BlockPos) -> Self;
    pub const fn offset(self, d: IVec3) -> Self;
    pub fn neighbors26(self) -> [ChunkPos; 26];
    pub const fn distance_sq(self, other: ChunkPos) -> i64;
    pub const fn distance_sq_xz(self, other: ChunkPos) -> i64;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockPos {
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl BlockPos {
    pub const fn new(x: i32, y: i32, z: i32) -> Self;
    pub const fn as_ivec3(self) -> IVec3;
    pub fn from_world(p: DVec3) -> Self;
    pub const fn split(self) -> (ChunkPos, LocalPos);
    pub const fn from_parts(chunk: ChunkPos, local: LocalPos) -> Self;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct LocalPos(u16);

impl LocalPos {
    pub const fn new(x: u32, y: u32, z: u32) -> Self;
    pub const fn from_index(i: usize) -> Self;
    pub const fn index(self) -> usize;
    pub const fn x(self) -> u32;
    pub const fn y(self) -> u32;
    pub const fn z(self) -> u32;
    pub const fn border_faces(self) -> u8;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum Face {
    PosY = 0,
    NegY = 1,
    PosX = 2,
    NegX = 3,
    PosZ = 4,
    NegZ = 5,
}

impl Face {
    pub const ALL: [Face; 6] = [
        Face::PosY,
        Face::NegY,
        Face::PosX,
        Face::NegX,
        Face::PosZ,
        Face::NegZ,
    ];
    pub const fn from_index(i: u8) -> Option<Face>;
    pub const fn index(self) -> usize;
    pub const fn normal(self) -> IVec3;
    pub const fn opposite(self) -> Face;
    pub const fn axes(self) -> (usize, usize, usize);
}

pub const NEIGHBOR_OFFSETS: [IVec3; 26] = {
    let mut out = [IVec3::ZERO; 26];
    let mut i = 0;
    let mut dy = -1;
    while dy <= 1 {
        let mut dx = -1;
        while dx <= 1 {
            let mut dz = -1;
            while dz <= 1 {
                if !(dx == 0 && dy == 0 && dz == 0) {
                    out[i] = IVec3::new(dx, dy, dz);
                    i += 1;
                }
                dz += 1;
            }
            dx += 1;
        }
        dy += 1;
    }
    out
};

pub const fn neighbor_index(d: IVec3) -> Option<usize>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DimensionId(pub u16);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChunkKey {
    pub dim: DimensionId,
    pub lod: u8,
    pub pos: ChunkPos,
}

impl ChunkKey {
    pub const fn new(pos: ChunkPos) -> Self;
}

// ---- crates/boxel_voxel/src/lib.rs
pub mod coords;
pub mod registry;
pub mod storage;

pub use chunk::ChunkData;
pub use coords::{
    BlockPos, CHUNK_AREA, CHUNK_MASK, CHUNK_SHIFT, CHUNK_SIZE, CHUNK_VOLUME, ChunkKey, ChunkPos,
    DimensionId, Face, LocalPos, NEIGHBOR_OFFSETS, neighbor_index,
};
pub use registry::{
    BlockDef, BlockRegistry, BlockRegistryBuilder, BlockState, FaceTextures, RegistryError,
    RenderShape, StateFlags, TextureId, TexturePattern, TextureSpec,
};
pub use storage::{LightStorage, Occupancy, Paletted};

pub mod prelude {
    pub use crate::{
        BlockPos, BlockRegistry, BlockState, ChunkData, ChunkKey, ChunkPos, Face, LocalPos,
    };
}

// ---- crates/boxel_voxel/src/registry.rs
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct BlockState(pub u16);

impl BlockState {
    pub const AIR: BlockState = BlockState(0);
    pub const fn is_air(self) -> bool;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum RenderShape {
    #[default]
    None,
    Opaque,
    Cutout,
    Translucent,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    pub struct StateFlags: u16 {
        const SOLID = 1;
        const OCCLUDES = 1 << 1;
        const FLUID = 1 << 2;
        const EMISSIVE = 1 << 3;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TexturePattern {
    Noise,
    TopBand {
        color: [u8; 3],
        rows: u8,
    },
    Speckle {
        color: [u8; 3],
        density: u8,
    },
    Leaves,
    Planks,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextureSpec {
    pub name: String,
    pub base_color: [u8; 3],
    pub noise: u8,
    pub pattern: TexturePattern,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TextureId(pub u16);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FaceTextures {
    pub top: TextureId,
    pub bottom: TextureId,
    pub side: TextureId,
}

impl FaceTextures {
    pub const fn all(t: TextureId) -> Self;
    pub const fn get(self, face: Face) -> TextureId;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockDef {
    pub name: String,
    pub shape: RenderShape,
    pub solid: bool,
    pub light_emission: u8,
    pub textures: FaceTextures,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RegistryError {
    #[error("duplicate name {0}")]
    Duplicate(String),
    #[error("invalid name {0}")]
    InvalidName(String),
    #[error("registry full: {0}")]
    Full(&'static str),
    #[error("unknown texture id {0}")]
    UnknownTexture(u16),
}

#[derive(Debug, Default)]
pub struct BlockRegistryBuilder {
    textures: Vec<TextureSpec>,
    blocks: Vec<BlockDef>,
}

impl BlockRegistryBuilder {
    pub fn new() -> Self;

    pub fn add_texture(&mut self, spec: TextureSpec) -> Result<TextureId, RegistryError>;

    pub fn add_block(&mut self, def: BlockDef) -> Result<BlockState, RegistryError>;

    pub fn build(self) -> BlockRegistry;
}

fn validate_name(name: &str) -> Result<(), RegistryError>;

#[derive(Debug, Clone)]
pub struct BlockRegistry {
    names: Vec<String>,
    shapes: Vec<RenderShape>,
    flags: Vec<StateFlags>,
    light: Vec<u8>,
    faces: Vec<FaceTextures>,
    textures: Vec<TextureSpec>,
    by_name: std::collections::HashMap<String, BlockState, foldhash::fast::FixedState>,
}

impl BlockRegistry {
    pub fn default_blocks() -> BlockRegistry;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn by_name(&self, name: &str) -> Option<BlockState>;
    pub fn name(&self, s: BlockState) -> &str;
    pub fn shape(&self, s: BlockState) -> RenderShape;
    pub fn flags(&self, s: BlockState) -> StateFlags;
    pub fn light_emission(&self, s: BlockState) -> u8;
    pub fn face_texture(&self, s: BlockState, face: Face) -> TextureId;
    pub fn textures(&self) -> &[TextureSpec];
    pub fn material(&self, s: BlockState) -> u16;
}

// ---- crates/boxel_voxel/src/storage.rs
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Paletted {
    Uniform(BlockState),
    Packed {
        palette: Vec<BlockState>,
        bits: u8,
        words: Box<[u64]>,
    },
    Direct(Box<[BlockState]>),
}

impl Default for Paletted {
    fn default() -> Self;
}

impl Paletted {
    #[inline]
    pub fn get(&self, p: LocalPos) -> BlockState;
    pub fn set(&mut self, p: LocalPos, s: BlockState) -> BlockState;
    pub fn encode(states: &[BlockState]) -> Paletted;
    pub fn decode_into(&self, out: &mut [BlockState]);
    pub fn as_uniform(&self) -> Option<BlockState>;
    pub fn palette_len(&self) -> usize;
    pub fn bits_per_voxel(&self) -> u8;
    pub fn compact(&mut self);
    pub fn heap_bytes(&self) -> usize;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LightStorage {
    Uniform {
        sky: u8,
        block: u8,
    },
    Packed(Box<[u8]>),
}

impl Default for LightStorage {
    fn default() -> Self;
}

pub type Occupancy = [u32; 1024];

const _: () = assert!(CHUNK_VOLUME == 32768);
```

Tests: proptest set/get against a dense mirror over random sequences (states 0..300 to reach
Direct); encode/decode roundtrip; growth sequence 1 -> 2 -> 4 -> 8 -> Direct; compact collapses
to Uniform; version bump rules; occupancy bits. Bench `benches/paletted.rs`: encode, decode,
random get/set on a terrain-like chunk.

### 5.12 `boxel_worldgen` (WP4)

`[S]`: trait, struct definitions, `TerrainSettings::default`, constructors, `name()`.

`[P]` rules:

- `noise::hash2`: a 64-bit mix (for example SplitMix64 of `seed ^ (x as u64) << 32 ^ z as u64`)
  truncated to u32. `value_noise_2d`: lattice values `hash2 / u32::MAX * 2 - 1` at integer
  corners, smoothstep (`t * t * (3 - 2t)`) bilinear interpolation. `fbm_2d`: sum of octaves with
  frequency `*= lacunarity`, amplitude `*= gain`, normalized by the amplitude sum.
- `TerrainGenerator::height_at(x, z) = base_height + round(amplitude * fbm_2d(seed, x * f,
  z * f, octaves, 2.0, 0.5))`.
- `TerrainGenerator::generate`: compute the 32x32 heights; chunk y range `[y0, y0 + 31]`: if
  `y0 > max_h` -> Uniform air; if `y0 + 31 < bottom` -> Uniform air; if
  `y0 > bottom && y0 + 31 < min_h - dirt_depth` -> Uniform stone; else fill: `y < bottom` air,
  `y == bottom` bedrock, `y < h - dirt_depth` stone, `y < h` dirt, `y == h` top block (sand if
  `h <= sand_level`, snow if `h > snow_level`, else grass), above air. Encode with
  `ChunkData::from_dense`. Block states by name from the registry (looked up once per call).
- `vertical_chunks`: `(bottom >> 5) ..= ((base_height + ceil(amplitude) + 1) >> 5)`.
- `FlatGenerator`: layers from y = 0 upwards; `vertical_chunks = 0 ..= (total_height - 1) >> 5`;
  validate names in `generate` (unknown name -> panic with the name: programmer error).

Public API (`boxel_worldgen`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_worldgen/src/lib.rs
pub mod noise;

pub trait WorldGenerator: Send + Sync + 'static {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData;
    fn vertical_chunks(&self) -> RangeInclusive<i32>;
    fn name(&self) -> &str;
}

#[derive(Clone, Debug)]
pub struct FlatGenerator {
    layers: Vec<(String, u32)>,
}

impl FlatGenerator {
    pub fn new(layers: Vec<(String, u32)>) -> Self;
}

impl WorldGenerator for FlatGenerator {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData;
    fn vertical_chunks(&self) -> RangeInclusive<i32>;
    fn name(&self) -> &str;
}

#[derive(Clone, Debug, PartialEq)]
pub struct TerrainSettings {
    pub base_height: i32,
    pub amplitude: f32,
    pub frequency: f64,
    pub octaves: u32,
    pub dirt_depth: i32,
    pub sand_level: i32,
    pub snow_level: i32,
    pub bottom: i32,
}

impl Default for TerrainSettings {
    fn default() -> Self;
}

#[derive(Clone, Debug)]
pub struct TerrainGenerator {
    pub seed: u64,
    pub settings: TerrainSettings,
}

impl TerrainGenerator {
    pub fn new(seed: u64, settings: TerrainSettings) -> Self;
    pub fn height_at(&self, x: i32, z: i32) -> i32;
}

impl Default for TerrainGenerator {
    fn default() -> Self;
}

impl WorldGenerator for TerrainGenerator {
    fn generate(&self, key: ChunkKey, registry: &BlockRegistry) -> ChunkData;
    fn vertical_chunks(&self) -> RangeInclusive<i32>;
    fn name(&self) -> &str;
}

// ---- crates/boxel_worldgen/src/noise.rs
pub fn hash2(seed: u64, x: i32, z: i32) -> u32;

pub fn value_noise_2d(seed: u64, x: f64, z: f64) -> f64;

pub fn fbm_2d(seed: u64, x: f64, z: f64, octaves: u32, lacunarity: f64, gain: f64) -> f64;
```

Tests: determinism (same key twice, equal); air above `vertical_chunks`; heights within
`base +- amplitude`; flat layers exact; a surface chunk has grass on top. Bench: `generate` for a
surface chunk (target < 0.3 ms release, informational) and an all-air chunk (< 50 us).

### 5.13 `boxel_mesh` (WP5)

Purpose: PackedQuad meshing (ADR 0006, 0012).

`[S]`: `PackedQuad` (all), `layout`, `ChunkMesh` (all), `PaddedChunk` (all), `VoxelClass`,
`MaterialTable::{from_registry, class, shape, bucket, layer, from_parts}`, `Mesher::new`,
constants. Bucket layout (frozen, 8 buckets): `0..6` opaque faces in `Face` order,
`CUTOUT_BUCKET = 6` (every face of `RenderShape::Cutout` materials), `TRANSLUCENT_BUCKET = 7`.
`MAX_QUADS_PER_FACE_BUCKET = 16384`; `MAX_QUADS_PER_BUCKET = 98304` (cutout and translucent
buckets hold all six faces; the shared index buffer covers it).

`[P]` rules:

- `Mesher::gather`: material ids through the table; for the center use `ChunkData::to_dense`
  into a reusable scratch buffer (no per-voxel `get` on Packed data); for neighbours copy only
  the needed border voxels (face: 32x32 slab, edge: 32 voxels, corner: 1) from
  `NEIGHBOR_OFFSETS[i]`; missing neighbour = empty. Uniform chunks fill without decoding.
  `out.clear()` first.
- `Mesher::mesh`: `inner.clear()`; `inner.fast_mesh(&padded.voxels, &padded.opaque,
  &padded.transparent)`; for `face in 0..6`, for each `Quad` in `inner.quads[face]`:
  `binary_greedy_meshing::Face::from(face as u8).vertices_packed(quad)` -> min/max per axis ->
  `pos = min`, `w = max[u] - min[u]`, `h = max[v] - min[v]` (axes from `Face::axes`), and for
  even (positive) faces `pos[normal] -= 1`; `material = quad.voxel_id() as u16`;
  `PackedQuad::new(pos, w, h, face, materials.layer(material, face))`; push to
  `materials.bucket(material, face)`. Write buckets contiguously and the counts. The same
  conversion is validated in `reference/renderprobe/src/main.rs` (`mesh_chunk`).
- `Mesher::mesh_chunk`: fast paths in the doc comment, else `gather` into `self.padded` + `mesh`.
- `reference_visible_faces`: the brute-force oracle documented in the skeleton.

Faces between two adjacent cutout voxels of the same material are omitted by bgm (transparent
voxels hide faces toward the same id). M0 accepts that (leaves render as a textured shell drawn
with `cull_mode: None`); interior leaf faces are an M1 option that does not change the layout.

Public API (`boxel_mesh`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_mesh/src/lib.rs
pub use mesher::{MaterialTable, MeshInput, Mesher, reference_visible_faces};
pub use packed::{ChunkMesh, PackedQuad, layout};
pub use padded::{PADDED, PADDED_AREA, PADDED_VOLUME, PaddedChunk, VoxelClass};

pub const BUCKET_COUNT: usize = 8;
pub const CUTOUT_BUCKET: usize = 6;
pub const TRANSLUCENT_BUCKET: usize = 7;
pub const MAX_QUADS_PER_FACE_BUCKET: u32 = 16384;
pub const MAX_QUADS_PER_BUCKET: u32 = 6 * MAX_QUADS_PER_FACE_BUCKET;

// ---- crates/boxel_mesh/src/mesher.rs
#[derive(Clone, Debug, Default)]
pub struct MaterialTable {
    class: Vec<VoxelClass>,
    shape: Vec<RenderShape>,
    layers: Vec<[u16; 6]>,
}

impl MaterialTable {
    pub fn from_registry(registry: &BlockRegistry) -> Self;
    pub fn class(&self, material: u16) -> VoxelClass;
    pub fn shape(&self, material: u16) -> RenderShape;
    pub fn bucket(&self, material: u16, face: Face) -> usize;
    pub fn layer(&self, material: u16, face: Face) -> u16;
    pub fn from_parts(shape: Vec<RenderShape>, layers: Vec<[u16; 6]>) -> Self;
}

pub struct MeshInput<'a> {
    pub center: &'a ChunkData,
    pub neighbors: [Option<&'a ChunkData>; 26],
}

pub struct Mesher {
    inner: binary_greedy_meshing::Mesher<32>,
    padded: PaddedChunk,
}

impl Default for Mesher {
    fn default() -> Self;
}

impl Mesher {
    pub fn new() -> Self;

    pub fn gather(
        &mut self,
        input: &MeshInput<'_>,
        materials: &MaterialTable,
        out: &mut PaddedChunk,
    );

    pub fn mesh(&mut self, padded: &PaddedChunk, materials: &MaterialTable, out: &mut ChunkMesh);

    pub fn mesh_chunk(&mut self, input: &MeshInput<'_>, materials: &MaterialTable) -> ChunkMesh;
}

pub fn reference_visible_faces(padded: &PaddedChunk) -> BTreeSet<([u32; 3], Face)>;

// ---- crates/boxel_mesh/src/packed.rs
pub mod layout {
    pub const GEO_POS_MASK: u32 = 31;
    pub const GEO_Y_SHIFT: u32 = 5;
    pub const GEO_Z_SHIFT: u32 = 10;
    pub const GEO_W_SHIFT: u32 = 15;
    pub const GEO_H_SHIFT: u32 = 20;
    pub const GEO_FACE_SHIFT: u32 = 25;
    pub const MAT_LAYER_MASK: u32 = 0xFFF;
    pub const MAT_AO_SHIFT: u32 = 12;
    pub const MAT_SKY_SHIFT: u32 = 20;
    pub const MAT_BLOCK_SHIFT: u32 = 24;
    pub const MAT_FLAGS_SHIFT: u32 = 28;
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct PackedQuad {
    pub geo: u32,
    pub mat: u32,
}

impl PackedQuad {
    pub const fn new(pos: [u32; 3], w: u32, h: u32, face: Face, layer: u16) -> Self;
    pub const fn with_light(mut self, sky: u8, block: u8) -> Self;
    pub const fn with_ao(mut self, ao: [u8; 4]) -> Self;
    pub const fn pos(self) -> [u32; 3];
    pub const fn w(self) -> u32;
    pub const fn h(self) -> u32;
    pub const fn face(self) -> Face;
    pub const fn layer(self) -> u16;
    pub const fn sky(self) -> u8;
    pub const fn block_light(self) -> u8;
    pub fn unit_faces(self) -> impl Iterator<Item = ([u32; 3], Face)>;
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChunkMesh {
    pub quads: Vec<PackedQuad>,
    pub bucket_counts: [u32; BUCKET_COUNT],
}

impl ChunkMesh {
    pub fn is_empty(&self) -> bool;
    pub fn total_quads(&self) -> u32;
    pub fn bucket_range(&self, b: usize) -> Range<u32>;
    pub fn bucket(&self, b: usize) -> &[PackedQuad];
    pub fn clear(&mut self);
}

// ---- crates/boxel_mesh/src/padded.rs
pub const PADDED: usize = 34;
pub const PADDED_AREA: usize = PADDED * PADDED;
pub const PADDED_VOLUME: usize = PADDED_AREA * PADDED;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VoxelClass {
    Empty,
    Opaque,
    Transparent,
}

#[derive(Clone, Debug)]
pub struct PaddedChunk {
    pub voxels: Box<[u16]>,
    pub opaque: Box<[u64]>,
    pub transparent: Box<[u64]>,
}

impl Default for PaddedChunk {
    fn default() -> Self;
}

impl PaddedChunk {
    pub fn new() -> Self;
    pub fn clear(&mut self);
    #[inline]
    pub const fn index(x: i32, y: i32, z: i32) -> usize;
    #[inline]
    pub fn set(&mut self, x: i32, y: i32, z: i32, material: u16, class: VoxelClass);
    #[inline]
    pub fn get(&self, x: i32, y: i32, z: i32) -> u16;
}
```

Tests: single voxel -> 6 quads of 1x1 with correct faces/positions; full chunk with empty
neighbours -> 6 quads of 32x32; 3D checkerboard -> 16384 quads per face bucket; a checkerboard of
leaves -> 98304 quads in `CUTOUT_BUCKET`; proptest random fills (opaque, cutout, translucent
materials, at least 256 cases) where the expanded `unit_faces` of all quads equal
`reference_visible_faces` and every unit face has the material of its voxel; bucket routing per
shape; texture layer per face; `gather` with uniform neighbours; `mesh_chunk` fast paths. Bench
`benches/mesh.rs`: `mesh` of a terrain-like padded chunk (target < 60 us release) and gather +
mesh (< 150 us); informational.

### 5.14 `boxel_world` (WP6; `tests/` WP9 in phase B)

Purpose: chunk streaming inside the ECS (ARCHITECTURE.md section 8).

`[S]`: all types, `ChunkId::to_bits`, `ChunkMap::{get, id_at, data_at, len, is_empty, iter,
ready_meshes}`, `WorldStats::is_settled`, `EditQueue`, `ChunkViewer`, `WorldSettings`, defaults,
`WorldPlugin::default`, `jobs::InlineExecutor`, `JobScheduler::{new, queued, in_flight,
max_in_flight}`, `persist` (all).

`[P]` rules:

- `WorldPlugin::build`: insert `WorldSettings`, `WorldContent { generator, registry, materials:
  Arc::new(MaterialTable::from_registry(&registry)) }`, `ChunkMap`, `EditQueue`, `WorldStats`, a
  private `WorldJobs` resource (scheduler, crossbeam channels, executor); register
  `ChunkMeshEvent`; `configure_sets(Update, (Interest, Edits, Dispatch, Collect).chain())`; add
  the four systems; a stats system in `Last` that refreshes `WorldStats` (slot counts +
  scheduler counts) and writes the `"world"` line of `DiagnosticsBoard` if present:
  `chunks=.. ready=.. meshed=.. empty=.. meshing=.. queued=.. in_flight=.. failed=..`.
- `max_jobs_in_flight == 0` means `2 * AsyncComputeTaskPool::get_or_init(TaskPool::default)
  .thread_num()`. Never call `AsyncComputeTaskPool::get()` (it panics without `TaskPoolPlugin`).
- **Job result contract** (frozen, `JobExecutor` doc): every spawned job sends exactly one
  message: `Generated { id, data }`, `Meshed { id, data_version, epoch, mesh }`,
  `Cancelled { id, kind }` (it saw `cancel`) or `Failed { id, kind, panic }`. The job body runs
  inside `std::panic::catch_unwind(AssertUnwindSafe(..))`, and the job owns an in-flight token
  whose `Drop` sends `Failed` if nothing was sent. `Collect` calls `scheduler.complete()` for
  every message, so in-flight slots can never leak. `Failed`: error-log once per chunk, mark the
  slot (`DataState::Missing` for generation, `MeshState::Empty` for meshing), count it in
  `WorldStats` via the `"world"` board line; `Cancelled`: drop silently.
- Interest: viewer chunk = `ChunkPos::containing(BlockPos::from_world(transform.translation))`;
  wanted data set = columns with `distance_sq_xz <= (radius + 1)^2` times the generator's
  `vertical_chunks()`; mesh set = `distance_sq_xz <= radius^2`. Recompute only when a viewer
  changes chunk or radius (or on the first frame). Unload slots with `distance_sq_xz >
  (radius + 1 + hysteresis)^2` for every viewer: set `cancel`, emit `Remove` if the mesh was
  `Ready`, clear the neighbour bits of the 26 neighbours and bump their `epoch`. Priority of new
  jobs: squared 3D distance to the nearest viewer (capped to u32).
- Edits (cut list 2): group by chunk; `Arc::make_mut`; `ChunkData::set`; face neighbours
  touched by `LocalPos::border_faces` get `epoch += 1` and a remesh request; priority 0. Edits
  to chunks that are not `Ready` are dropped with a debug log.
- Dispatch: pop jobs while `pop_ready()` returns one; re-validate the slot (still exists, still
  wants this job); Generate: state `Generating`, spawn a job that checks `cancel`, calls
  `generator.generate(key, &registry)` and sends `Generated`; Mesh: only if
  `neighbors_ready == (1 << 26) - 1` and data `Ready`, else leave it `Queued` (requeued when the
  last neighbour becomes ready); state `InFlight { data_version, epoch }`; spawn a job with the 27
  `Arc`s that uses a thread-local `boxel_mesh::Mesher` and sends `Meshed`. Missing neighbours
  (outside `vertical_chunks`) count as ready and as `None` in `MeshInput`.
- Collect: drain within `collect_budget`; drop stale results (slot gone, `cancel` set, version
  or epoch mismatch: requeue the mesh if the chunk still wants one). Generated:
  `DataState::Ready(Arc::new(data))`, set the neighbour bit in all 26 neighbours (and its own
  bits for neighbours that are already Ready or Missing), queue meshes that became complete.
  Meshed: non-empty -> `MeshState::Ready` + `Upsert` (respect `max_mesh_events_per_frame`: keep
  extra results in a pending queue for the next frame); empty -> `MeshState::Empty` + `Remove` if
  it was Ready before.
- `ChunkMap::block_at`, `ChunkMap::stats` (slot-based; scheduler counts only in the resource).
- `TaskPoolExecutor::spawn`: `AsyncComputeTaskPool::get_or_init(TaskPool::default)
  .spawn(async move { job() }).detach()`.
- `JobScheduler`: binary heap of `Reverse((priority, seq, id, kind))`, `pop_ready` returns
  `None` when `in_flight >= max_in_flight`; `complete` saturating decrement; `reprioritize`
  rebuilds.

Public API (`boxel_world`, generated from the compiled skeleton; bodies and doc comments stripped):

```rust
// ---- crates/boxel_world/src/jobs.rs
pub trait JobExecutor: Send + Sync + 'static {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct TaskPoolExecutor;

impl JobExecutor for TaskPoolExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct InlineExecutor;

impl JobExecutor for InlineExecutor {
    fn spawn(&self, job: Box<dyn FnOnce() + Send + 'static>);
}

// ---- crates/boxel_world/src/lib.rs
pub mod jobs;
pub mod persist;
pub mod scheduler;

pub use map::{ChunkId, ChunkMap, ChunkSlot, DataState, Interest, MeshState, WorldStats};
pub use plugin::{
    BlockEdit, ChunkMeshEvent, ChunkViewer, EditQueue, WorldContent, WorldPlugin, WorldSettings,
    WorldSystems,
};

pub mod prelude {
    pub use crate::{ChunkMap, ChunkViewer, EditQueue, WorldPlugin, WorldSettings};
}

// ---- crates/boxel_world/src/map.rs
slotmap::new_key_type! {
    pub struct ChunkId;
}

impl ChunkId {
    pub fn to_bits(self) -> u64;
}

#[derive(Clone, Debug)]
pub enum DataState {
    Queued,
    Generating,
    Ready(Arc<ChunkData>),
    Missing,
}

#[derive(Clone, Debug)]
pub enum MeshState {
    None,
    Queued,
    InFlight {
        data_version: u32,
        epoch: u32,
    },
    Ready {
        mesh: Arc<ChunkMesh>,
        data_version: u32,
        epoch: u32,
    },
    Empty,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct Interest: u8 {
        const DATA = 1;
        const MESH = 1 << 1;
        const PINNED = 1 << 2;
    }
}

#[derive(Clone, Debug)]
pub struct ChunkSlot {
    pub key: ChunkKey,
    pub data: DataState,
    pub mesh: MeshState,
    pub interest: Interest,
    pub neighbors_ready: u32,
    pub epoch: u32,
    pub remesh_pending: bool,
    pub cancel: Arc<AtomicBool>,
}

#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WorldStats {
    pub chunks: usize,
    pub data_ready: usize,
    pub generating: usize,
    pub mesh_ready: usize,
    pub mesh_empty: usize,
    pub meshing: usize,
    pub queued_jobs: usize,
    pub in_flight: usize,
}

impl WorldStats {
    pub fn is_settled(&self) -> bool;
}

#[derive(Resource, Default)]
pub struct ChunkMap {
    pub(crate) slots: slotmap::SlotMap<ChunkId, ChunkSlot>,
    pub(crate) index: HashMap<ChunkPos, ChunkId, foldhash::fast::FixedState>,
}

impl ChunkMap {
    pub fn get(&self, id: ChunkId) -> Option<&ChunkSlot>;
    pub fn id_at(&self, pos: ChunkPos) -> Option<ChunkId>;
    pub fn data_at(&self, pos: ChunkPos) -> Option<&Arc<ChunkData>>;
    pub fn block_at(&self, p: BlockPos) -> Option<BlockState>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn iter(&self) -> impl Iterator<Item = (ChunkId, &ChunkSlot)>;
    pub fn ready_meshes(&self) -> impl Iterator<Item = (ChunkId, ChunkKey, &Arc<ChunkMesh>)>;
    pub fn stats(&self) -> WorldStats;
}

// ---- crates/boxel_world/src/persist.rs
pub trait ChunkStore: Send + Sync + 'static {
    fn load(&self, key: ChunkKey) -> std::io::Result<Option<Vec<u8>>>;
    fn save_batch(&self, items: &[(ChunkKey, Vec<u8>)]) -> std::io::Result<()>;
    fn flush(&self) -> std::io::Result<()>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NullChunkStore;

impl ChunkStore for NullChunkStore {
    fn load(&self, _key: ChunkKey) -> std::io::Result<Option<Vec<u8>>>;
    fn save_batch(&self, _items: &[(ChunkKey, Vec<u8>)]) -> std::io::Result<()>;
}

// ---- crates/boxel_world/src/plugin.rs
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkViewer {
    pub radius: u32,
}

impl Default for ChunkViewer {
    fn default() -> Self;
}

#[derive(Resource, Clone, Debug)]
pub struct WorldSettings {
    pub dimension: DimensionId,
    pub max_jobs_in_flight: usize,
    pub collect_budget: Duration,
    pub max_mesh_events_per_frame: usize,
    pub unload_hysteresis: u32,
}

impl Default for WorldSettings {
    fn default() -> Self;
}

#[derive(Resource, Clone)]
pub struct WorldContent {
    pub generator: Arc<dyn WorldGenerator>,
    pub registry: Arc<BlockRegistry>,
    pub materials: Arc<MaterialTable>,
}

#[derive(Message, Clone, Debug)]
pub enum ChunkMeshEvent {
    Upsert {
        id: ChunkId,
        key: ChunkKey,
        mesh: Arc<ChunkMesh>,
    },
    Remove {
        id: ChunkId,
        key: ChunkKey,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEdit {
    pub pos: BlockPos,
    pub state: BlockState,
}

#[derive(Resource, Debug, Default)]
pub struct EditQueue {
    pub(crate) edits: Vec<BlockEdit>,
}

impl EditQueue {
    pub fn set_block(&mut self, pos: BlockPos, state: BlockState);
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorldSystems {
    Interest,
    Edits,
    Dispatch,
    Collect,
}

pub struct WorldPlugin {
    pub generator: Arc<dyn WorldGenerator>,
    pub registry: Arc<BlockRegistry>,
    pub settings: WorldSettings,
    pub executor: Arc<dyn JobExecutor>,
}

impl Default for WorldPlugin {
    fn default() -> Self;
}

impl Plugin for WorldPlugin {
    fn build(&self, app: &mut App);
}

// ---- crates/boxel_world/src/scheduler.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JobKind {
    Generate,
    Mesh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueuedJob {
    pub priority: u32,
    pub seq: u64,
    pub id: ChunkId,
    pub kind: JobKind,
}

#[derive(Debug, Default)]
pub struct JobScheduler {
    heap: std::collections::BinaryHeap<std::cmp::Reverse<(u32, u64, ChunkId, u8)>>,
    next_seq: u64,
    in_flight: usize,
    max_in_flight: usize,
}

impl JobScheduler {
    pub fn new(max_in_flight: usize) -> Self;
    pub fn push(&mut self, id: ChunkId, kind: JobKind, priority: u32);
    pub fn pop_ready(&mut self) -> Option<QueuedJob>;
    pub fn complete(&mut self);
    pub fn reprioritize(&mut self, f: impl FnMut(ChunkId, JobKind) -> Option<u32>);
    pub fn queued(&self) -> usize;
    pub fn in_flight(&self) -> usize;
    pub fn max_in_flight(&self) -> usize;
}
```

Tests (phase A, runnable with skeleton crates only): `JobScheduler` ordering, cap and FIFO ties;
interest set computation (pure helper) for radius 0, 1, 12; neighbour bit bookkeeping; stale
result rejection by version and epoch; job contract: a generator that panics for one key and a
job that sees `cancel` both release their in-flight slot (`in_flight()` returns to 0); plugin
wiring with `InlineExecutor` and a test generator returning `ChunkData::uniform(AIR)` above y = 0
and a uniform opaque state below (both mesh fast paths): world settles, `ChunkMap` contains the
expected columns, unload after moving the viewer. Put tests that need the real mesher or
`TerrainGenerator` in `tests/integration.rs` with `#[ignore = "integration: needs WP4/WP5"]`
(real terrain: `ChunkMeshEvent::Upsert` count > 0; edits remesh neighbours). WP9 un-ignores them.

### 5.15 `boxel` facade (WP0; examples WP8/WP9; README WP9)

`src/lib.rs` is final in the skeleton (re-exports, `prelude`, `DefaultPlugins` = Log, TaskPool,
Time, FrameStats, Input, Window, World, Render; `MinimalPlugins` = TaskPool, Time,
ScheduleRunner 16 ms). `crates/boxel_facade_check` (publish = false, depends only on `boxel`
with `default-features = false`) proves every derive (`Component` with storage and `require`,
`Resource`, `Message`, `Event`, `EntityEvent`, `SystemSet`, `ScheduleLabel`, `Bundle`,
`SystemParam`, `QueryData`, `QueryFilter`, `FromWorld`) and a `'static` `ParamSet` expand
through `::boxel::ecs`, the path real users get.

```rust
// file: src/lib.rs
//! # boxel
//!
//! A Bevy-style voxel engine. This facade re-exports the engine crates and provides
//! [`DefaultPlugins`], [`MinimalPlugins`] and a [`prelude`].
//!
//! Derive macros (`Component`, `Resource`, `Message`, `SystemSet`, `ScheduleLabel`...) resolve
//! `::boxel::ecs` when your crate depends on `boxel` (under that exact name).
//!
//! ECS notes:
//! - In `ParamSet`, spell query lifetimes out:
//!   `ParamSet<(Query<&'static mut A>, Query<&'static B>)>`. The elided form fails with
//!   error E0477 ("the type `&mut A` does not fulfill the required lifetime"): rustc's
//!   compatibility hack only recognizes a crate named `bevy_ecs` (see docs/architecture.md).
//! - Resources are entities (`bevy_ecs` 0.20): `World::entities` counts and `Query<Entity>` see
//!   them too.
//! - The window is a non-send resource: use `NonSend<PrimaryWindow>` (main thread). Calling
//!   winit window methods from parallel systems deadlocks on macOS and Windows.
//!
//! Features: `window`, `render`, `voxel`, `png` and `debug` (ECS diagnostics with system and
//! component names) are on by default; `trace`/`trace_tracy` add spans; `hal` adds the Vulkan
//! escape hatch (Windows/Linux).

pub use boxel_app as app;
pub use boxel_diagnostic as diagnostic;
pub use boxel_ecs as ecs;
pub use boxel_log as log;
pub use boxel_math as math;
pub use boxel_tasks as tasks;
pub use boxel_transform as transform;

#[cfg(feature = "window")]
pub use boxel_camera as camera;
#[cfg(feature = "window")]
pub use boxel_window as window;

#[cfg(feature = "render")]
pub use boxel_gfx as gfx;
#[cfg(feature = "render")]
pub use boxel_render as render;
#[cfg(feature = "render")]
pub use boxel_shaders as shaders;

#[cfg(feature = "voxel")]
pub use boxel_mesh as mesh;
#[cfg(feature = "voxel")]
pub use boxel_voxel as voxel;
#[cfg(feature = "voxel")]
pub use boxel_world as world;
#[cfg(feature = "voxel")]
pub use boxel_worldgen as worldgen;

use boxel_app::{PluginGroup, PluginGroupBuilder};

/// Everything a typical app needs.
pub mod prelude {
    pub use crate::{DefaultPlugins, MinimalPlugins};
    pub use boxel_app::prelude::*;
    pub use boxel_diagnostic::{
        DiagnosticsBoard, FrameStats, FrameStatsPlugin, LogDiagnosticsPlugin,
    };
    pub use boxel_ecs::prelude::*;
    pub use boxel_log::LogPlugin;
    pub use boxel_math::prelude::*;
    pub use boxel_transform::prelude::*;

    #[cfg(feature = "window")]
    pub use boxel_camera::prelude::*;
    #[cfg(feature = "window")]
    pub use boxel_window::prelude::*;

    #[cfg(feature = "render")]
    pub use boxel_gfx::{GpuPreferences, Tier};
    #[cfg(feature = "render")]
    pub use boxel_render::prelude::*;

    #[cfg(feature = "voxel")]
    pub use boxel_voxel::prelude::*;
    #[cfg(feature = "voxel")]
    pub use boxel_world::prelude::*;
    #[cfg(feature = "voxel")]
    pub use boxel_worldgen::{TerrainGenerator, WorldGenerator};
}

/// Log, task pools, time, frame stats, input, window + winit runner, voxel world, renderer
/// (each behind its cargo feature). Configure with `.set(...)`, remove with `.disable::<T>()`.
pub struct DefaultPlugins;

impl PluginGroup for DefaultPlugins {
    fn build(self) -> PluginGroupBuilder {
        let group = PluginGroupBuilder::start()
            .add(boxel_log::LogPlugin::default())
            .add(boxel_app::TaskPoolPlugin::default())
            .add(boxel_app::TimePlugin)
            .add(boxel_diagnostic::FrameStatsPlugin);
        #[cfg(feature = "window")]
        let group = group
            .add(boxel_window::InputPlugin)
            .add(boxel_window::WindowPlugin::default());
        #[cfg(feature = "voxel")]
        let group = group.add(boxel_world::WorldPlugin::default());
        #[cfg(feature = "render")]
        let group = group.add(boxel_render::RenderPlugin::default());
        group
    }
}

/// Headless: task pools, time and the loop runner (16 ms). For servers, tools and tests.
pub struct MinimalPlugins;

impl PluginGroup for MinimalPlugins {
    fn build(self) -> PluginGroupBuilder {
        PluginGroupBuilder::start()
            .add(boxel_app::TaskPoolPlugin::default())
            .add(boxel_app::TimePlugin)
            .add(boxel_app::ScheduleRunnerPlugin::run_loop(
                std::time::Duration::from_millis(16),
            ))
    }
}
```


---

## 6. Shaders and GPU data layout

### 6.1 Files and composition

| Module (`ShaderModuleId`) | Source | Entry points | Tiers | GL floor | naga capabilities |
|---|---|---|---|---|---|
| `VoxelPull` | `common.wgsl` + `voxel_pull.wgsl` | `vs_pull`, `fs_opaque`, `fs_cutout` | High, Mid | 430, ES 310 | none |
| `VoxelInstanced` | `common.wgsl` + `voxel_instanced.wgsl` | `vs_instanced`, `fs_opaque`, `fs_cutout` | Low | 330, ES 300 | `IMMEDIATES` |
| `Cull` | `common.wgsl` + `cull.wgsl` | `cull_chunks` | High | 430, ES 310 | none |
| `Blit` | `blit.wgsl` | `vs_fullscreen`, `fs_blit`, `fs_blit_encode_srgb` | all | 330, ES 300 | none |

All four modules pass `tests/naga.rs` with naga 30.0.1: WGSL front end and validator, GLSL at
their floors (with wgpu-hal's `ADJUST_COORDINATE_SPACE`), MSL 2.1 and 2.4, HLSL SM 5.1 (FXC) and
SM 6.0 (DXC), SPIR-V. `VoxelPull` and `Cull` intentionally fail on GLSL 330 (no SSBO/compute).
The pre-cutout versions also ran as real pipelines on NVIDIA/RADV/lavapipe (Vulkan) and NVIDIA
3.3/radeonsi 4.6/llvmpipe 4.5 (GL); the cutout split only adds an entry point and a second
draw list (WP7 re-validates on every adapter with `gpu_tiers`).

### 6.2 PackedQuad bit layout and buckets (frozen)

```
geo (u32): bits 0-4 x | 5-9 y | 10-14 z | 15-19 (w-1) | 20-24 (h-1) | 25-27 face | 28-31 reserved
mat (u32): bits 0-11 texture layer | 12-19 ao (corner c at bits 12+2c, 3 = open) | 20-23 sky light | 24-27 block light | 28-31 flags
face: 0 +Y, 1 -Y, 2 +X, 3 -X, 4 +Z, 5 -Z
axes (u, v): ±Y -> (X, Z); ±X -> (Z, Y); ±Z -> (X, Y)
corner c in 0..4: (u, v) = (c & 1, c >> 1); index pattern per quad [0, 1, 2, 2, 1, 3]; strip order 0, 1, 2, 3
buckets (per chunk, contiguous in the arena): 0..6 opaque faces (Face order), 6 cutout (all faces), 7 translucent
```

Vertex position (shader): `local = pos + max(normal, 0) + u * cu * w + v * cv * h`, where
corners 1 and 2 are swapped when `dot(cross(u, v), normal) < 0` (CCW winding seen from outside for
every face, with both the list pattern and the strip). UV = `(cu * w, cv * h)`, with `v` flipped
for side faces so textures are upright; the sampler uses `Repeat`.

### 6.3 Vertex pulling math and the draw id (ADR 0010)

- High/Mid: vertex buffer slot 0 = `DrawRef` buffer, `VertexStepMode::Instance`, stride 8,
  attribute `Uint32x2` at location 0. Index buffer = shared u32 `[0,1,2,2,1,3] + 4q` for
  `q in 0..MAX_QUADS_PER_BUCKET` (98304 quads, 589824 indices, 2.36 MB). Each draw:
  `index_count = 6 * n`, `instance_count = 1`, `first_index = 0`, `base_vertex = 0`,
  `first_instance = DrawRef index`. Shader: `quad = quads[draw.quad_start + (vertex_index >> 2)]`,
  `corner = vertex_index & 3`, `chunk = chunks[draw.chunk_slot]`. On DX12, naga lowers
  `vertex_index` to `first_vertex + SV_VertexID`: correct only because `first_vertex`/
  `base_vertex` are always 0 (the naga test asserts the lowering so a change is noticed).
- Low: vertex buffer slot 0 = quad arena, `VertexStepMode::Instance`, stride 8, `Uint32x2` at
  location 0, `PrimitiveTopology::TriangleStrip`, `draw(0..4, first_quad..first_quad + n)`.
  Corner = `vertex_index`. Chunk origin = immediates (16 bytes, `ChunkImmediate`).
- Position: `rel = vec3<f32>(chunk.origin - view.render_origin.xyz) + local` (integer
  subtraction first), `clip = view.view_proj * vec4(rel, 1)`.

### 6.4 Bind group layouts and pipelines (frozen)

| Pipeline | Group | Binding | Type | Visibility |
|---|---|---|---|---|
| voxel (all tiers) | 0 | 0 | `Texture { Float { filterable: true }, D2Array }` | FRAGMENT |
| voxel (all tiers) | 0 | 1 | `Sampler(Filtering)` | FRAGMENT |
| voxel (all tiers) | 1 | 0 | `Buffer { Uniform }` (`ViewUniform`, 240 B) | VERTEX, FRAGMENT |
| voxel High/Mid | 2 | 0 | `Buffer { Storage { read_only: true } }` quad arena | VERTEX |
| voxel High/Mid | 2 | 1 | `Buffer { Storage { read_only: true } }` chunk table | VERTEX |
| voxel Low | immediates | 0..16 | `ChunkImmediate` | VERTEX |
| cull (High) | 0 | 0 | `Buffer { Uniform }` view | COMPUTE |
| cull (High) | 0 | 1 | `Buffer { Storage { read_only: true } }` chunk table | COMPUTE |
| cull (High) | 0 | 2 | `Buffer { Storage { read_only: false } }` draw args | COMPUTE |
| cull (High) | 0 | 3 | `Buffer { Storage { read_only: false } }` draw refs | COMPUTE |
| cull (High) | 0 | 4 | `Buffer { Storage { read_only: false } }` draw counts (8 B: opaque, cutout) | COMPUTE |
| blit | 0 | 0 | `Texture { Float { filterable: true }, D2 }` | FRAGMENT |
| blit | 0 | 1 | `Sampler(Filtering)` | FRAGMENT |

Pipeline layouts: voxel High/Mid `[g0, g1, g2]`, `immediate_size: 0`; voxel Low `[g0, g1]`,
`immediate_size: 16`; cull `[cull]`; blit `[blit]`. In wgpu 30 `bind_group_layouts` is
`&[Option<&BindGroupLayout>]` and `VertexState::buffers` is `&[Option<VertexBufferLayout>]`.

Two voxel pipelines per tier (ADR 0012):

- **opaque**: `fs_opaque` (no `discard`, early-Z stays on), `cull_mode: Some(Back)`, buckets 0..6;
- **cutout**: `fs_cutout` (alpha test `a < 0.5` discards), `cull_mode: None`, bucket 6; drawn
  after the opaque buckets.

Shared render state: `TriangleList` (High/Mid) or `TriangleStrip` with
`strip_index_format: None` (Low), `front_face: Ccw`, depth `Depth32Float`,
`depth_write_enabled: Some(true)`, `depth_compare: Some(Greater)` (wgpu 30 makes both `Option`),
color target `Rgba8UnormSrgb` without blending, `multiview_mask: None`, `cache: None`. Blit: no
depth, `TriangleList` of 3 vertices, target = `GpuContext::output_format`, entry `fs_blit` when
the format `is_srgb()`, else `fs_blit_encode_srgb`; both write alpha 1. Samplers: blocks
`Repeat`, mag `Nearest`, min `Linear`, mipmap `Linear` (wgpu 30: `MipmapFilterMode`); blit
`ClampToEdge`, `Linear`.

Low tier rule (ADR 0011): exactly one uniform block and one texture/sampler pair per pipeline
(naga test).

### 6.5 GPU structs (Rust mirrors in `boxel_shaders::gpu`)

| WGSL | Rust | Size | Notes |
|---|---|---|---|
| `View` | `ViewUniform` | 240 | uniform; only vec4/mat4 members (std140 safe); `counts = [slot count, opaque list capacity (= cutout list base), cutout list capacity, 0]` |
| `ChunkGpu` | `ChunkGpu` | 64 | `origin: vec3<i32>`, `quad_base`, `counts: array<u32, 8>` (bucket order), `flags` (bit0 live), 3 padding words |
| `DrawArgs` | `DrawIndexedIndirect` | 20 | same layout as `wgpu::util::DrawIndexedIndirectArgs` |
| (vertex attr) | `DrawRef` | 8 | `chunk_slot`, `quad_start` |
| `ChunkImmediate` | `ChunkImmediate` | 16 | `origin: vec4<i32>` |

`bindings::MAX_DRAWS_PER_LIST = 65535`: each indirect list holds at most that many draws and
each `multi_draw_indexed_indirect_count` call passes at most that as `max_count` (wgpu-hal does
not check Vulkan's `maxDrawIndirectCount`, whose minimum is 65535; `cull.wgsl` never writes past
a list's capacity).

### 6.6 WGSL sources (final, verbatim in the skeleton)

`crates/boxel_shaders/shaders/common.wgsl`:

```wgsl
// file: crates/boxel_shaders/shaders/common.wgsl
// boxel: shared voxel shader code (all tiers). Prepended to voxel_pull.wgsl, voxel_instanced.wgsl and cull.wgsl.
//
// PackedQuad = vec2<u32>(geo, mat), 8 bytes, identical bytes on every tier.
//   geo: x:5 | y:5 | z:5 | (w-1):5 | (h-1):5 | face:3 | reserved:4
//   mat: layer:12 | ao:8 (2 bits per corner, 3 = unoccluded) | sky:4 | block:4 | flags:4
// face: 0=+Y 1=-Y 2=+X 3=-X 4=+Z 5=-Z  (u,v axes: ±Y -> (X,Z), ±X -> (Z,Y), ±Z -> (X,Y))
//
// RULES (enforced by boxel_shaders tests):
//  - never use @builtin(instance_index), @builtin(draw_index), @builtin(num_workgroups);
//    per-draw data arrives through the DrawRef instance-step vertex attribute or immediates.
//  - @builtin(vertex_index) is only used with first_vertex = 0 / base_vertex = 0 draws.
//  - textureLoad mip levels are always i32 (naga GLSL bug gfx-rs/wgpu#10037).

const GEO_POS_MASK: u32 = 31u;
const GEO_Y_SHIFT: u32 = 5u;
const GEO_Z_SHIFT: u32 = 10u;
const GEO_W_SHIFT: u32 = 15u;
const GEO_H_SHIFT: u32 = 20u;
const GEO_FACE_SHIFT: u32 = 25u;
const MAT_LAYER_MASK: u32 = 0xFFFu;
const MAT_AO_SHIFT: u32 = 12u;
const MAT_SKY_SHIFT: u32 = 20u;
const MAT_BLOCK_SHIFT: u32 = 24u;
const MAT_FLAGS_SHIFT: u32 = 28u;
const CHUNK_SIZE: f32 = 32.0;

struct View {
  view_proj: mat4x4<f32>,          // camera-relative: positions are (world - render_origin)
  frustum: array<vec4<f32>, 6>,    // xyz = inward normal, w = d; inside if dot(n, p) + d >= 0
  render_origin: vec4<i32>,        // xyz = integer voxel origin of the camera-relative frame
  camera_rel: vec4<f32>,           // xyz = eye position relative to render_origin
  fog_color: vec4<f32>,
  fog_params: vec4<f32>,           // x = fog start, y = fog end (world units)
  counts: vec4<u32>,               // x = chunk slot count, y = opaque list capacity (= cutout list base), z = cutout list capacity (High)
};

struct ChunkGpu {                  // 64 bytes, mirrors boxel_shaders::gpu::ChunkGpu
  origin: vec3<i32>,               // world voxel coordinates of the chunk min corner (absolute)
  quad_base: u32,                  // first quad of bucket 0 in the quad arena
  counts: array<u32, 8>,           // [0..6) opaque face buckets, [6] cutout, [7] translucent
  flags: u32,                      // bit0 = live
  pad0: u32,
  pad1: u32,
  pad2: u32,
};

struct VsOut {
  @builtin(position) clip: vec4<f32>,
  @location(0) uv: vec2<f32>,
  @location(1) @interpolate(flat) layer: u32,
  @location(2) shade: f32,
  @location(3) view_dist: f32,
};

struct Decoded { local: vec3<f32>, uv: vec2<f32>, face: u32, layer: u32, shade: f32 };

fn face_normal(f: u32) -> vec3<f32> {
  switch f {
    case 0u: { return vec3(0.0, 1.0, 0.0); }
    case 1u: { return vec3(0.0, -1.0, 0.0); }
    case 2u: { return vec3(1.0, 0.0, 0.0); }
    case 3u: { return vec3(-1.0, 0.0, 0.0); }
    case 4u: { return vec3(0.0, 0.0, 1.0); }
    default: { return vec3(0.0, 0.0, -1.0); }
  }
}
fn face_u(f: u32) -> vec3<f32> { if (f == 2u || f == 3u) { return vec3(0.0, 0.0, 1.0); } return vec3(1.0, 0.0, 0.0); }
fn face_v(f: u32) -> vec3<f32> { if (f < 2u) { return vec3(0.0, 0.0, 1.0); } return vec3(0.0, 1.0, 0.0); }
fn face_shade(f: u32) -> f32 {
  switch f {
    case 0u: { return 1.0; }
    case 1u: { return 0.5; }
    case 2u, 3u: { return 0.8; }
    default: { return 0.65; }
  }
}

// corner 0..3 = (u,v) (0,0) (1,0) (0,1) (1,1); index pattern per quad = [0,1,2, 2,1,3] (strip order 0,1,2,3)
fn decode_quad(q: vec2<u32>, corner_in: u32) -> Decoded {
  let geo = q.x;
  let mat = q.y;
  let p = vec3<f32>(f32(geo & GEO_POS_MASK), f32((geo >> GEO_Y_SHIFT) & GEO_POS_MASK), f32((geo >> GEO_Z_SHIFT) & GEO_POS_MASK));
  let w = f32(((geo >> GEO_W_SHIFT) & GEO_POS_MASK) + 1u);
  let h = f32(((geo >> GEO_H_SHIFT) & GEO_POS_MASK) + 1u);
  let f = (geo >> GEO_FACE_SHIFT) & 7u;
  let n = face_normal(f);
  let u = face_u(f);
  let v = face_v(f);
  var c = corner_in;
  // keep counter-clockwise winding seen from outside: swap corners 1 and 2 when (u x v) points inward
  if (dot(cross(u, v), n) < 0.0) { if (c == 1u) { c = 2u; } else if (c == 2u) { c = 1u; } }
  let cu = f32(c & 1u);
  let cv = f32(c >> 1u);
  var o: Decoded;
  o.local = p + max(n, vec3(0.0)) + u * (cu * w) + v * (cv * h);
  o.uv = select(vec2(cu * w, cv * h), vec2(cu * w, (1.0 - cv) * h), f >= 2u); // side faces: v points down in texture space
  o.face = f;
  o.layer = mat & MAT_LAYER_MASK;
  let ao = f32((mat >> (MAT_AO_SHIFT + 2u * c)) & 3u) / 3.0;
  let sky = f32((mat >> MAT_SKY_SHIFT) & 15u) / 15.0;
  let blk = f32((mat >> MAT_BLOCK_SHIFT) & 15u) / 15.0;
  o.shade = face_shade(f) * mix(0.45, 1.0, ao) * (max(sky, blk) * 0.85 + 0.15);
  return o;
}

fn finish_vertex(d: Decoded, chunk_origin: vec3<i32>, view: View) -> VsOut {
  // integer subtraction first: exact for any world position, then small floats
  let rel = vec3<f32>(chunk_origin - view.render_origin.xyz) + d.local;
  var o: VsOut;
  o.clip = view.view_proj * vec4(rel, 1.0);
  o.uv = d.uv;
  o.layer = d.layer;
  o.shade = d.shade;
  o.view_dist = length(rel - view.camera_rel.xyz);
  return o;
}

// Opaque buckets never discard (keeps early-Z); cutout buckets use the alpha test below.
fn alpha_test_passes(albedo: vec4<f32>) -> bool { return albedo.a >= 0.5; }

fn shade_fragment(albedo: vec4<f32>, i: VsOut, view: View) -> vec4<f32> {
  let fog = clamp((i.view_dist - view.fog_params.x) / max(view.fog_params.y - view.fog_params.x, 1.0), 0.0, 1.0);
  return vec4(mix(albedo.rgb * i.shade, view.fog_color.rgb, fog), 1.0);
}
```

`crates/boxel_shaders/shaders/voxel_pull.wgsl`:

```wgsl
// file: crates/boxel_shaders/shaders/voxel_pull.wgsl
// Tiers High + Mid: vertex pulling from the quad arena (needs DownlevelFlags::VERTEX_STORAGE).
// Per-draw data = DrawRef (chunk_slot, quad_start) read as an INSTANCE-STEP vertex attribute at
// location 0; the draw sets first_instance = DrawRef index, instance_count = 1, base_vertex = 0.
@group(0) @binding(0) var block_textures: texture_2d_array<f32>;
@group(0) @binding(1) var block_sampler: sampler;
@group(1) @binding(0) var<uniform> view: View;
@group(2) @binding(0) var<storage, read> quads: array<vec2<u32>>;
@group(2) @binding(1) var<storage, read> chunks: array<ChunkGpu>;

@vertex
fn vs_pull(@builtin(vertex_index) vi: u32, @location(0) draw: vec2<u32>) -> VsOut {
  let ch = chunks[draw.x];
  let q = quads[draw.y + (vi >> 2u)];
  return finish_vertex(decode_quad(q, vi & 3u), ch.origin, view);
}

// Opaque buckets (pipeline with cull_mode Back): no discard, early-Z stays on.
@fragment
fn fs_opaque(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, i.layer);
  return shade_fragment(albedo, i, view);
}

// Cutout bucket (pipeline with cull_mode None): alpha test.
@fragment
fn fs_cutout(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, i.layer);
  if (!alpha_test_passes(albedo)) { discard; }
  return shade_fragment(albedo, i, view);
}
```

`crates/boxel_shaders/shaders/voxel_instanced.wgsl`:

```wgsl
// file: crates/boxel_shaders/shaders/voxel_instanced.wgsl
// Tier Low: no storage buffers in any stage (desktop GL 3.3 / GLES 3.0 class).
// PackedQuad is an INSTANCE-STEP vertex attribute (TriangleStrip, 4 vertices per instance,
// first_instance = first quad of the bucket). The chunk origin comes from immediates.
// Exactly ONE uniform block and ONE texture/sampler pair per pipeline: wgpu 30 skips the GL
// uniform-block remap on NVIDIA GL 3.3 contexts, so every block would land on binding 0.
struct ChunkImmediate { origin: vec4<i32> };
@group(0) @binding(0) var block_textures: texture_2d_array<f32>;
@group(0) @binding(1) var block_sampler: sampler;
@group(1) @binding(0) var<uniform> view: View;
var<immediate> chunk: ChunkImmediate;

@vertex
fn vs_instanced(@builtin(vertex_index) vi: u32, @location(0) q: vec2<u32>) -> VsOut {
  return finish_vertex(decode_quad(q, vi), chunk.origin.xyz, view);
}

// Opaque buckets (pipeline with cull_mode Back): no discard, early-Z stays on.
@fragment
fn fs_opaque(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, i.layer);
  return shade_fragment(albedo, i, view);
}

// Cutout bucket (pipeline with cull_mode None): alpha test.
@fragment
fn fs_cutout(i: VsOut) -> @location(0) vec4<f32> {
  let albedo = textureSample(block_textures, block_sampler, i.uv, i.layer);
  if (!alpha_test_passes(albedo)) { discard; }
  return shade_fragment(albedo, i, view);
}
```

`crates/boxel_shaders/shaders/cull.wgsl`:

```wgsl
// file: crates/boxel_shaders/shaders/cull.wgsl
// Tier High: GPU frustum + per-face-bucket culling. Writes two compacted lists of
// DrawIndexedIndirectArgs with matching DrawRefs and atomic counts for
// multi_draw_indexed_indirect_count: opaque face buckets at [0, counts.y) and the cutout bucket
// at [counts.y, counts.y + counts.z). The *_count path is NOT validated by wgpu: never write past
// a list's capacity (capacities are <= 65535 = Vulkan's minimum maxDrawIndirectCount).
struct DrawArgs { index_count: u32, instance_count: u32, first_index: u32, base_vertex: i32, first_instance: u32 };

@group(0) @binding(0) var<uniform> view: View;
@group(0) @binding(1) var<storage, read> chunks: array<ChunkGpu>;
@group(0) @binding(2) var<storage, read_write> draw_args: array<DrawArgs>;
@group(0) @binding(3) var<storage, read_write> draw_refs: array<vec2<u32>>;
@group(0) @binding(4) var<storage, read_write> draw_counts: array<atomic<u32>, 2>; // [0] opaque, [1] cutout

fn in_frustum(mn: vec3<f32>, mx: vec3<f32>) -> bool {
  for (var k = 0u; k < 6u; k++) {
    let pl = view.frustum[k];
    let pv = select(mn, mx, pl.xyz >= vec3(0.0));
    if (dot(pl.xyz, pv) + pl.w < 0.0) { return false; }
  }
  return true;
}

// chunk-level backface test per face bucket: a +Y bucket can only be seen from above its min y, etc.
fn bucket_visible(f: u32, mn: vec3<f32>, mx: vec3<f32>, cam: vec3<f32>) -> bool {
  switch f {
    case 0u: { return cam.y > mn.y; }
    case 1u: { return cam.y < mx.y; }
    case 2u: { return cam.x > mn.x; }
    case 3u: { return cam.x < mx.x; }
    case 4u: { return cam.z > mn.z; }
    default: { return cam.z < mx.z; }
  }
}

@compute @workgroup_size(64)
fn cull_chunks(@builtin(global_invocation_id) gid: vec3<u32>) {
  let slot = gid.x;
  if (slot >= view.counts.x) { return; }
  let ch = chunks[slot];
  if ((ch.flags & 1u) == 0u) { return; }
  let mn = vec3<f32>(ch.origin - view.render_origin.xyz);
  let mx = mn + vec3(CHUNK_SIZE);
  if (!in_frustum(mn, mx)) { return; }
  var start = ch.quad_base;
  for (var f = 0u; f < 6u; f++) {
    let n = ch.counts[f];
    if (n > 0u && bucket_visible(f, mn, mx, view.camera_rel.xyz)) {
      let s = atomicAdd(&draw_counts[0], 1u);
      if (s < view.counts.y) {
        draw_args[s] = DrawArgs(n * 6u, 1u, 0u, 0, s);
        draw_refs[s] = vec2(slot, start);
      }
    }
    start += n;
  }
  // cutout bucket: all faces mixed, no bucket-level backface test (drawn with cull_mode None)
  let nc = ch.counts[6];
  if (nc > 0u) {
    let c = atomicAdd(&draw_counts[1], 1u);
    if (c < view.counts.z) {
      let s = view.counts.y + c;
      draw_args[s] = DrawArgs(nc * 6u, 1u, 0u, 0, s);
      draw_refs[s] = vec2(slot, start);
    }
  }
}
```

`crates/boxel_shaders/shaders/blit.wgsl`:

```wgsl
// file: crates/boxel_shaders/shaders/blit.wgsl
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

// target is an *Srgb format: hardware encodes. Alpha is forced to 1 (opaque window).
@fragment
fn fs_blit(i: BlitOut) -> @location(0) vec4<f32> {
  return vec4(textureSample(src_tex, src_sampler, i.uv).rgb, 1.0);
}

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
  let lo = c * 12.92;
  let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
  return select(hi, lo, c <= vec3(0.0031308));
}

// target is a non-sRGB 8-bit format (Bgra8Unorm/Rgba8Unorm only, see
// boxel_gfx::choose_surface_format): encode manually. Alpha is forced to 1.
@fragment
fn fs_blit_encode_srgb(i: BlitOut) -> @location(0) vec4<f32> {
  let c = textureSample(src_tex, src_sampler, i.uv);
  return vec4(linear_to_srgb(c.rgb), 1.0);
}
```

### 6.7 naga tests (`crates/boxel_shaders/tests/naga.rs`, `[S]`)

Dev-dependency `naga = { workspace = true, features = ["wgsl-in", "glsl-out", "msl-out",
"hlsl-out", "spv-out"] }`. Seven tests, all green in the skeleton:

1. every module parses, validates (`Capabilities::IMMEDIATES` only where `uses_immediates()`)
   and lists exactly its entry points with the right stages;
2. GLSL at each module's floor (`Desktop(330)` + `Embedded 300` or `Desktop(430)` + `Embedded
   310`, `WriterFlags::ADJUST_COORDINATE_SPACE` like wgpu-hal), and `VoxelPull` fails on 330;
3. MSL 2.1 and 2.4 per entry point (`immediates_buffer: Some(30)` for `VoxelInstanced`,
   `fake_missing_bindings`), HLSL SM 5.1 and 6.0 (`special_constants_binding` and
   `immediates_target` set like wgpu-hal dx12; asserts the `first_vertex` lowering), SPIR-V;
4. no entry point binds `InstanceIndex`, `DrawIndex`, `NumWorkGroups`, `BaseInstance` or
   `BaseVertex`;
5. WGSL `GEO_*`/`MAT_*` constants equal `boxel_mesh::layout`;
6. `View` (240), `ChunkGpu` (64), `DrawArgs` (20), `ChunkImmediate` (16) sizes and offsets match
   the Rust mirrors;
7. `VoxelInstanced` has one uniform, one texture, one sampler and no storage buffers.

---

## 7. Renderer internals (WP7) and plugin flow (WP8)

### 7.1 `VoxelRenderer::new(device, queue, tier, config, textures, output_format, size)`

Takes device parts (not a `GpuContext`) so `RendererSmokeTest` can build a real renderer during
selection. Reads limits with `device.limits()`. Never touches a surface. Everything inside
`boxel_gfx::run_in_error_scopes` (any error -> `RendererError::Pipeline`):

1. Block textures: `generate_block_textures(textures)` -> `create_texture_with_data`
   (`TextureDataOrder::LayerMajor`, `D2` with `depth_or_array_layers = layers`, 5 mips,
   `Rgba8UnormSrgb`, `TEXTURE_BINDING | COPY_DST`), view `D2Array`, sampler (section 6.4).
2. View uniform buffer (`UNIFORM | COPY_DST`, 240 B).
3. Slots: `slots = config.max_chunk_slots`, on High clamped to
   `64 * limits.max_compute_workgroups_per_dimension`.
4. Arena: `arena_quads = min(config.arena_quads, limit / 8)` with limit =
   `max_storage_buffer_binding_size` (High/Mid) or `max_buffer_size` (Low); buffer
   `STORAGE | VERTEX | COPY_DST` (a Low device with 0 storage buffers still accepts the usage,
   verified on lavapipe with webgl2 limits); `offset_allocator::Allocator::with_max_allocs(
   arena_quads, slots * 2)`.
5. Chunk table: High/Mid `STORAGE | COPY_DST` of `slots * 64` bytes, zeroed; CPU mirror for all
   tiers (`Vec<Option<ChunkRecord>>` + free list + `HashMap<u64, slot>`).
6. Shared index buffer (High/Mid): `MAX_QUADS_PER_BUCKET` quads -> 589824 u32 (`INDEX`).
7. Path state, with `cap_opaque = min(6 * slots, MAX_DRAWS_PER_LIST)` and
   `cap_cutout = min(slots, MAX_DRAWS_PER_LIST)`:
   - High: `draw_args` (`INDIRECT | STORAGE`, `(cap_opaque + cap_cutout) * 20` B), `draw_refs`
     (`VERTEX | STORAGE`, `(cap_opaque + cap_cutout) * 8` B), `draw_counts` (`INDIRECT | STORAGE
     | COPY_DST`, 8 B), cull pipeline + bind group, opaque and cutout pull pipelines.
   - Mid: `draw_refs` (`VERTEX | COPY_DST`, `7 * slots * 8` B), opaque and cutout pull
     pipelines.
   - Low: opaque and cutout instanced pipelines.
8. Targets: main color (`MAIN_COLOR_FORMAT`, `RENDER_ATTACHMENT | TEXTURE_BINDING | COPY_SRC`),
   depth (`DEPTH_FORMAT`), blit bind group + pipeline for `output_format`.

### 7.2 Chunk upload

`upsert_chunk(queue, id, origin, mesh)`: empty mesh -> `remove_chunk`. Else allocate
`mesh.total_quads()` quads (free the old allocation first; on failure `ArenaFull` and keep the
old mesh if there was one); `queue.write_buffer(arena, offset * 8, bytemuck::cast_slice(
&mesh.quads))`; get a slot (reuse by id, else free list, else next index, else
`ChunkTableFull`); write `ChunkGpu { origin, quad_base: offset, counts: mesh.bucket_counts,
flags: FLAG_LIVE, pad: [0; 3] }` (High/Mid); update the CPU record (origin, quad_base, bucket
counts, AABB). Every `ArenaFull`/`ChunkTableFull` increments `RenderStats::dropped_chunks`.
`remove_chunk`: free the allocation, write a zeroed `ChunkGpu` to the slot, push the slot to the
free list. `slot_count` = highest live slot + 1 (recomputed lazily).

### 7.3 Frame recording (`render`)

Common: `queue.write_buffer(view, ViewUniform { view_proj, frustum, render_origin, camera_rel:
eye_rel, fog_color, fog_params: [fog_start, fog_end, 0, 0], counts: [slot_count, cap_opaque,
cap_cutout, 0] })`. CPU culling for Mid/Low: for each live record, AABB
`[origin - render_origin, + 32]` against `ViewParams::frustum` (same p-vertex test as the
shader), then the 6 face-bucket tests of `cull.wgsl` (`bucket_visible`) using `eye_rel`; the
cutout bucket only needs the frustum test. Produce `(slot, quad_start, n)` per visible non-empty
bucket, opaque ones first.

- High: `encoder.clear_buffer(draw_counts, 0, None)`; compute pass `cull_chunks` with
  `dispatch_workgroups(slot_count.div_ceil(64), 1, 1)` (skip if 0); render pass (clear color =
  `config.clear_color`, clear depth 0); bind groups 0/1/2, `set_vertex_buffer(0,
  draw_refs.slice(..))`, `set_index_buffer(index, Uint32)`; opaque pipeline +
  `multi_draw_indexed_indirect_count(&draw_args, 0, &draw_counts, 0, cap_opaque)`; cutout
  pipeline + `multi_draw_indexed_indirect_count(&draw_args, cap_opaque * 20, &draw_counts, 4,
  cap_cutout)`. Stats: draws = 2. Log a warning once when `6 * live_slots > cap_opaque` (far
  buckets may be dropped; M3 segments the lists).
- Mid: write the `DrawRef`s (`queue.write_buffer`), render pass, same bindings; opaque pipeline
  and `draw_indexed(0..n * 6, 0, i..i + 1)` per visible face bucket `i`; then the cutout
  pipeline for the cutout buckets.
- Low: render pass, bind groups 0/1, `set_vertex_buffer(0, arena.slice(..))`; opaque pipeline,
  per visible face bucket: `set_immediates(0, bytemuck::bytes_of(&ChunkImmediate { origin: [x,
  y, z, 0] }))`, `draw(0..4, quad_start..quad_start + n)`; then the cutout pipeline likewise.

`blit(encoder, target)`: render pass on `target` (load `Clear(black)`), blit pipeline, bind group,
`draw(0..3, 0..1)`.

### 7.4 `RendererSmokeTest::run`

Inside the caller's error scopes: builds the bind group layouts and pipelines of `ctx.tier`
(High: cull + opaque/cutout pull; Mid: opaque/cutout pull; Low: opaque/cutout instanced) and the
blit pipeline for `ctx.output_format`, records one render pass into a 16x16 target and calls
`boxel_gfx::submit_and_wait`. With `SmokeLevel::Draw`: `VoxelRenderer::new(ctx.device,
ctx.queue, ctx.tier, &RendererConfig { arena_quads: 1024, max_chunk_slots: 8, .. },
&BlockRegistry::default_blocks().textures(), ctx.output_format, 16x16)`, one chunk with one +Y
opaque quad and one cutout quad at the origin, a camera looking down at it, `render`, check that
the center pixel of `main_color` differs from the clear color (`read_texture_rgba8`), then `blit`
into a 16x16 texture of `ctx.output_format` and submit. Returns the error text on failure.

### 7.5 `generate_block_textures`

Deterministic per texture name (seed = FNV-1a of the name). 16x16 RGBA8 sRGB per layer:
`base_color + uniform noise in [-noise, noise]` per texel; patterns: `TopBand` paints the top
`rows` texel rows (row 0 = top of the image, matching the shader's flipped side-face V);
`Speckle` replaces `density` of 256 texels with `color`; `Leaves` sets alpha 0 on ~25 % of
texels; `Planks` darkens every 4th row. Mips 8, 4, 2, 1: box filter in linear space (decode sRGB,
average, re-encode); alpha of `Leaves` thresholded at 0.5 in every mip. Empty input -> one
magenta layer.

### 7.6 `RenderPlugin` (WP8)

`build`: insert `self.settings`, init `RenderInfo`; register `GpuReady`, `GpuFailed`,
`RequestScreenshot`, `ScreenshotSaved`, `DebugDestroyDevice` and `boxel_world::ChunkMeshEvent`
(`add_message` is idempotent); `app.edit_schedule(Render, |s| { s.set_executor(
SingleThreadedExecutor::new()); })` (keeps systems other plugins added to `Render` earlier);
`MainScheduleOrder::insert_after(Last, Render)`; `configure_sets(Render, (Setup, Extract,
Prepare, Draw, Cleanup).chain())`; the systems below; a `Shutdown` system. Private state:
`GpuState` (`WaitingForWindow`, `Ready { ctx: GpuContext, surface: Surface<'static>, config:
SurfaceConfiguration, renderer: VoxelRenderer, resync: bool, confirmed: bool }`, `Failed`),
`InstanceState` (the `CreatedInstance`, created once per process unless `gl_deferred` triggers
the second stage), `LkgRes(Option<LkgStore>)`, `Losses { exclusions: Vec<Exclusion>, per_key:
HashMap<String, u32> }`, `ExtractedFrame`.

Invariants (frozen):

- A `Surface` that was ever configured is never configured with another device. Every entry
  into `WaitingForWindow` drops the old surface; every selection creates a new one.
- `VoxelRenderer::new` runs before `configure_surface` (it only needs `ctx.output_format`).
- Window calls, surface creation, acquire and present happen in `Render` systems (main thread).
- Extract boundary: only `Setup`, `Extract` and `Cleanup` touch the main world; `Prepare` and
  `Draw` read only `ExtractedFrame`, `GpuState` and `NonSend<PrimaryWindow>`.

Systems:

1. `Setup` (exclusive):
   - `WaitingForWindow` with `NonSend<PrimaryWindow>` and `DisplayHandle` present:
     1. `GpuPreferences::resolve(settings.gpu.clone(), settings.gpu_config.as_deref())`; log
        warnings.
     2. Once: `create_instance(Some(display.0.clone()), &prefs)`; error -> step 8 failure path.
     3. Once: if `prefs.lkg_enabled()`: `LkgStore::open(LkgStore::default_path(&app_name),
        build_fingerprint())` (log the error, keep the store) and
        `install_lkg_panic_hook(path)`.
     4. New surface: `instance.create_surface(window.clone())`.
     5. `select_gpu(SelectRequest { instance, surface: Some(&surface), prefs, policy:
        settings.tier_policy, lkg, exclusions, smoke: &RendererSmokeTest, smoke_level:
        settings.smoke_level, create_devices: true })`. On `NoUsableAdapter` with
        `instance.gl_deferred`: drop the surface, `create_gl_instance`, new surface, select
        again (once).
     6. `VoxelRenderer::new(&ctx.device, &ctx.queue, ctx.tier, &RendererConfig { arena_quads,
        max_chunk_slots, clear_color }, textures, ctx.output_format, window size)`, where
        `textures` = `WorldContent::registry.textures()` if the resource exists, else
        `BlockRegistry::default_blocks().textures()`.
     7. `configure_surface(&surface, &ctx, w, h, prefs.present_mode.unwrap_or_default())`.
        If step 6 or 7 fails: push `Exclusion { adapter_key, max_tier: ctx.tier.lower() }`,
        `lkg.cancel_pending()`, drop renderer, surface and context, stay `WaitingForWindow`
        (the next frame re-selects with a new surface). No same-frame retry.
     8. Success: `resync = true`, write `GpuReady`, info-log `boxel: {summary_line}` and the
        report, fill `RenderInfo` and the `"gpu"` board line. Selection failure: `GpuFailed {
        error }`, error-log, state `Failed` (no retries).
   - `Ready` and `ctx.health.is_lost()` (cut list 1): log the reason; count losses per adapter
     key; drop renderer, surface, context (in that order); first loss -> no exclusion; second ->
     `Exclusion { adapter_key, max_tier: tier.lower() }` (or `None` for Low) and
     `lkg.add_strike(..)`; `recoveries += 1`; state `WaitingForWindow`. If cut: write `GpuFailed`
     and state `Failed`.
   - `WindowResized` messages (non-zero): reconfigure the surface (same device) and
     `renderer.resize`.
2. `Extract`: fill `ExtractedFrame`: the first active `Camera` + `Transform` (copies), a
   `WindowState` copy, pending `RequestScreenshot` paths, `DebugDestroyDevice` flag, and the
   mesh work: if `resync`, the list of `ChunkMap::ready_meshes()` (ids, origins, `Arc<ChunkMesh>`)
   and drain pending events; else the `ChunkMeshEvent`s in order.
3. `Prepare`: apply the mesh work (`renderer.clear_chunks` first on resync; `Upsert` ->
   `upsert_chunk(id.to_bits(), key.pos.origin().as_ivec3(), &mesh)`; `Remove` ->
   `remove_chunk`; `ArenaFull`/`ChunkTableFull` are counted and warn-logged at most every 5 s);
   `CameraView::compute` with the surface aspect; `ViewParams::from_camera(..,
   settings.clear_color, settings.fog_start_fraction)`. No camera -> no view this frame.
4. `Draw`: return if not `Ready`, if `ctx.health.is_lost()` or if there is no view.
   - If `window.frame_ready && !window.occluded`: `acquire_frame(&surface)`:
     `Frame` -> encoder, `renderer.render`, `renderer.blit(&mut encoder, &frame_view)`,
     `queue.submit`, `window.pre_present_notify()`, `queue.present(texture)`, mark presented,
     `frames_presented += 1`, `frames_rendered += 1`, reconfigure afterwards if `suboptimal`;
     `Reconfigure` -> `configure_surface` with the current size; `Recreate` -> drop the old
     surface FIRST, create a new one from the window with the same instance, configure it with the
     same device; `Skip` -> nothing.
   - Else, if `settings.render_when_hidden`: encoder, `renderer.render`, `queue.submit`,
     `frames_rendered += 1` (no acquire, no present).
5. `Cleanup`: if a frame was presented, `WindowState::frame_ready = false` (the runner then
   requests the next redraw); on the first presented frame of a selection `lkg.confirm_good`;
   screenshots: `read_texture_rgba8(device, queue, renderer.main_color())` then `save_png`
   (without the `png` feature: `Err("png feature disabled")`), write `ScreenshotSaved`;
   `DebugDestroyDevice`: `ctx.device.destroy()` then `ctx.device.poll(PollType::Poll)` (verified
   on every adapter of this machine: the lost callback fires with `DeviceLostReason::Destroyed`
   and a new device can be created on the same adapter); update `RenderInfo` and the `"render"`
   board line (`tier=.. chunks=.. quads=used/cap draws=.. visible=.. dropped=..`).
6. `Shutdown` system: drop renderer, surface, context in that order; `lkg.cancel_pending()` if a
   selection is still unconfirmed.

---

## 8. GPU selection internals (WP3)

### 8.1 `select_gpu`

```text
created = req.instance                       // &CreatedInstance
report.backends = created.backends; report.flags = created.flags; report.notes = created.notes.clone()
if lkg is Some and created.flags intersects DEBUG|VALIDATION|GPU_BASED_VALIDATION|VALIDATION_INDIRECT_CALL:
    lkg.read_only = true                     // debug configurations say nothing about the driver
adapters = pollster::block_on(created.instance.enumerate_adapters(wgpu::Backends::all()))
for each adapter (enumeration order):
    caps = AdapterCaps::from_adapter(&adapter); cand = CandidateReport { .. }
    filter: Noop/BrowserWebGpu -> filtered; prefs.backends not containing caps.info.backend -> filtered
            prefs.adapter set and name does not contain it (case-insensitive) -> filtered
            prefs.software == Some(true) and class != Software -> filtered
            surface given: !adapter.is_surface_supported(surface) -> filtered
                           cand.surface_format = choose_surface_format(&surface.get_capabilities(&adapter).formats)
                           None -> filtered ("no 8-bit surface format")
    classification = classify(&caps, req.policy)
if prefs.adapter is set and every adapter was filtered by name -> NoUsableAdapter
pairs = for each unfiltered adapter with max_tier = Some(m):
            for t in Tier::DESCENDING where t <= m and t <= prefs.max_tier.unwrap_or(High):
                score(caps, t, prefs.power_or_default(), lkg.status(summary, t))
stable sort pairs by score descending
if !create_devices: record NotAttempted for every pair, return NoUsableAdapter(report)
for (adapter, t) in pairs:
    if excluded(adapter, t): SkippedExcluded; continue
    if lkg says Bad and !(prefs.adapter.is_some() && prefs.max_tier == Some(t)): SkippedKnownBad; continue
    lkg.begin_attempt(summary, t)
    req = tier_request(t, &caps.limits)
    match block_on(adapter.request_device(&DeviceDescriptor { label: Some("boxel"),
          required_features: req.features, required_limits: req.limits,
          experimental_features: ExperimentalFeatures::disabled(),
          memory_hints: MemoryHints::Performance, trace: Trace::Off }))
      Err(e) -> RequestDeviceFailed(e); lkg.cancel_pending(); continue
      Ok((device, queue)) ->
          health = Arc::new(GpuHealth::default()); install_device_callbacks(&device, &health)
          output_format = cand.surface_format.unwrap_or(Rgba8UnormSrgb)
          ctx = SmokeContext { device, queue, tier: t, caps, level: req.smoke_level.resolve(&caps), output_format }
          r = run_in_error_scopes(&device, || smoke.run(&ctx))
          if r is Ok and health.is_lost(): r = Err("device lost during smoke test: ..")
          if r is Ok and health.error_count() > 0: r = Err("uncaptured error: " + last_error)
          if r is Err(e) -> SmokeFailed(e); lkg.cancel_pending(); drop(device); continue
          record Ok; report.chosen = (index, t); info!(report.to_text())
          return GpuContext { instance: created.instance.clone(), adapter, device, queue, caps, tier: t,
                              class: caps.class(), output_format, health, report }
return NoUsableAdapter(report)
```

`ExperimentalFeatures::disabled()`, `MemoryHints::Performance` and `Trace::Off` are the wgpu 30
names (compile-checked by the graphics reviewer's probe).

`create_headless_instance(prefs)`: the generic `create_instance` with a private zero-sized
`NoDisplay` type (implements `wgpu::rwh::HasDisplayHandle` by returning
`Err(HandleError::Unavailable)`; never used because the option is `None`). `init_headless(prefs)`:
`create_headless_instance` + `select_gpu` with `BasicSmokeTest`, `SmokeLevel::Auto`,
`TierPolicy::default()`, LKG `None`, no exclusions, no surface; with `gl_deferred` and no result,
`create_gl_instance(None::<NoDisplay>, prefs)` + `select_gpu` again.

### 8.2 `tier_request` (`[S]`)

`Limits::default().or_worse_values_from(adapter)`, then `max_buffer_size`,
`max_texture_array_layers`, `max_immediate_size` and (Mid/High) `max_storage_buffer_binding_size`
and `max_storage_buffers_per_shader_stage` set to the adapter's own values; features `IMMEDIATES`
(+ `MULTI_DRAW_INDIRECT_COUNT | INDIRECT_FIRST_INSTANCE` on High); `debug_assert!(
limits.check_limits(adapter))`. Contract tests: adapters sitting exactly at every floor accept
every tier at or below theirs; every real adapter of this machine accepts the request.

### 8.3 `BasicSmokeTest`

Its own WGSL (validated by a naga test in `boxel_gfx`), one module per tier:

- Low: `@group(0) @binding(0) var<uniform> u: vec4<f32>; @group(0) @binding(1) var t:
  texture_2d_array<f32>; @group(0) @binding(2) var s: sampler; var<immediate> imm: vec4<f32>;`
  VS with `@builtin(vertex_index)` and `@location(0) q: vec2<u32>` (instance step) producing a
  quad that covers the target; FS returns `u * textureSample(t, s, uv, 0).a + imm * 0.0`.
  Pipeline: strip, one instance.
- Mid: Low's bindings + `@group(1) @binding(0) var<storage, read> a: array<vec2<u32>>;
  @group(1) @binding(1) var<storage, read> b: array<vec4<i32>>;` read in the VS; indexed draw
  with a 6-index buffer.
- High: Mid + a compute entry `@compute @workgroup_size(64)` that `atomicAdd`s into a
  `var<storage, read_write> count: atomic<u32>` and writes one `DrawArgs` into a
  `var<storage, read_write> args: array<DrawArgs>`; buffers with `STORAGE | INDIRECT`; a render
  pass that records `multi_draw_indexed_indirect_count(args, 0, count_buffer, 0, 1)`.
- Every level: the output pipeline targets `ctx.output_format`, one pass is recorded and
  `submit_and_wait` runs inside the scopes. `Draw` level: render to a 4x4 `Rgba8UnormSrgb`
  target, read back with `read_texture_rgba8`, check the center pixel equals `u` (within 2).

### 8.4 Report format (`SelectionReport::to_text`)

```text
GPU selection (instance backends: VULKAN | GL, flags: InstanceFlags(0x0))
[0] AMD Radeon 780M Graphics (RADV PHOENIX)  backend=Vulkan class=Integrated driver="Mesa 26.0.8-1ubuntu0.3"
    max tier: High
    surface: supported (Bgra8UnormSrgb)
    attempt High score=3400  not attempted
[1] NVIDIA GeForce RTX 5050 Laptop GPU  backend=Vulkan class=Discrete driver="595.91.07"
    max tier: High
    surface: supported (Bgra8UnormSrgb)
    attempt High score=3500  OK (212.4 ms)
[2] llvmpipe (LLVM 21.1.8, 256 bits)  backend=Vulkan class=Software driver="Mesa 26.0.8-1ubuntu0.3 (LLVM 21.1.8)"
    max tier: Mid  (High: software adapters capped at Mid)
[3] NVIDIA GeForce RTX 5050 Laptop GPU/PCIe/SSE2  backend=Gl class=Other driver="3.3.0 NVIDIA 595.91.07" gl=3.3
    max tier: Low  (Mid: GL 3.3 < 4.3 (no SSBO/compute in GLSL 330); High: no MULTI_DRAW_INDIRECT_COUNT)
notes:
  - (none)
DECISION: backend=Vulkan tier=High class=Discrete adapter="NVIDIA GeForce RTX 5050 Laptop GPU" driver="595.91.07"
```

---

## 9. Examples and integration tests

All examples: `#![expect(clippy::print_stdout, clippy::print_stderr, reason = "CLI output")]` if
they print. Exit codes: 0 success, 1 usage error, 2 no usable GPU, 3 check failed (screenshot,
image diff, resize, minimize), 4 timeout.

### 9.1 `crates/boxel_gfx/examples/gpu_info.rs` (WP3)

Headless; lives in `boxel_gfx` so it builds without the rest of the engine
(`cargo run -p boxel_gfx --example gpu_info -- [GPU options] [--dry-run]
[--smoke auto|compile|draw] [--all-tiers] [--lkg APP] [--clear-lkg APP] [--help]`).

1. `boxel_log::init_logging("warn")` (ignore `AlreadySet`).
2. `GpuPreferences::from_args` + `resolve` (same overrides as voxel_world). Print warnings.
3. `--lkg APP`: print the LKG file of `APP` (path, entries with status, strikes, build) and exit
   0. `--clear-lkg APP`: `LkgStore::clear` on it and exit 0.
4. `create_headless_instance(&prefs)`; print compiled-in backends
   (`wgpu::Instance::enabled_backend_features()`), requested backends and flags.
5. For every adapter (unfiltered, for information): name, backend, class, vendor/device, driver,
   driver_info, parsed GL version; key features (`MULTI_DRAW_INDIRECT_COUNT`,
   `INDIRECT_FIRST_INSTANCE`, `IMMEDIATES`, `TIMESTAMP_QUERY`, `SUBGROUP`, `SHADER_INT64`,
   `TEXTURE_BINDING_ARRAY`, `PIPELINE_CACHE`, `EXPERIMENTAL_MESH_SHADER`); downlevel
   (`COMPUTE_SHADERS`, `INDIRECT_EXECUTION`, `VERTEX_STORAGE`, `BASE_VERTEX`, webgpu-compliant);
   limits (`max_storage_buffer_binding_size`, `max_storage_buffers_per_shader_stage`,
   `max_immediate_size`, `max_compute_invocations_per_workgroup`,
   `max_compute_workgroup_size_x`, `max_texture_array_layers`); classification with reasons;
   score per allowed tier.
6. `--all-tiers`: for every adapter and every tier it claims, create a device and run
   `BasicSmokeTest` (level from `--smoke`), print a matrix `adapter x tier -> OK | FAIL (msg)`.
7. Unless `--dry-run`: `init_headless`-like selection (BasicSmokeTest, no surface, LKG disabled)
   and print `report.to_text()` (last line `DECISION: ...`). Exit 0 if chosen, 2 otherwise.

### 9.2 `examples/voxel_world.rs` (WP8)

Windowed. `cargo run --release --example voxel_world -- [GPU options] [options]`:

| Option | Meaning |
|---|---|
| `--frames N` | automation: exit 0 after N rendered frames (`RenderInfo::frames_rendered`, counted after settling with `--wait-chunks`); sets `RenderSettings::render_when_hidden = true`; disables fly-camera input |
| `--wait-chunks` | do not count frames until `Res<WorldStats>::is_settled()` held for 3 consecutive frames |
| `--timeout SECS` | global wall-clock limit for automation (default 120 with `--frames`/`--minimize-test`), exit 4 |
| `--screenshot PATH` | at frame N request a screenshot, exit after `ScreenshotSaved` (error -> exit 3) |
| `--expect-terrain` | after the screenshot: `RgbaImage::non_background_fraction` in [0.05, 0.99] else exit 3 |
| `--compare-to PATH` | after the screenshot: `RgbaImage::load_png(PATH)` and `compare(.., 2)`; print `boxel: diff fraction=F max_channel=M`; exit 3 if `fraction > --max-diff` (default 0.005) |
| `--minimize-test` | at update 60 call `window.set_minimized(true)` (main-thread system), then run 3 s; print `boxel: minimized updates=U presents=P fixed_steps=S`; exit 0 if `U >= 60` and `S >= 60` (the simulation kept running), else 3 |
| `--resize-test` | at frame 30 `request_inner_size(+100, +50)`; exit 3 if no `WindowResized` within 60 frames or any surface error is logged |
| `--seed N` | terrain seed (default `TerrainGenerator::default().seed`) |
| `--view-distance N` | `ChunkViewer::radius` in chunks (default 12; 6 on a Software class device) |
| `--size WxH` | window size (default 1280x720) |
| `--camera x,y,z,yaw_deg,pitch_deg` | initial camera (default `0,120,0,45,-30`) |
| `--present vsync|mailbox|immediate` | through `GpuPreferences` |
| `--simulate-device-loss N` | write `DebugDestroyDevice` at frame N |
| `--async-threads N` | override `TaskPoolOptions::async_compute` (min = max = N) for the streaming measurement of section 12 row 22 |
| `--help` | prints usage + `boxel_gfx::CLI_HELP`, exit 0 |

App: `DefaultPlugins` with `.set(WindowPlugin { primary: WindowSettings { title: "boxel
voxel_world", .. } })`, `.set(RenderPlugin { settings: RenderSettings { app_name:
"voxel_world", gpu: <cli prefs>, render_when_hidden: <automation>, .. } })`,
`.set(WorldPlugin { generator: TerrainGenerator::new(seed, default), .. })`, plus
`LogDiagnosticsPlugin` and `FlyCameraPlugin`. Startup: spawn `(Camera { view_distance: radius *
32, .. }, Transform::from_yaw_pitch(..), FlyCamera { enabled: !automation, yaw, pitch, .. },
ChunkViewer { radius })`. Systems: on `GpuReady` print `boxel: <summary>` to stdout (stable line
for scripts) and adjust the radius for Software; on `GpuFailed` print the error to stderr and
exit 2; automation logic as in the table (the minimize/resize systems take
`NonSend<PrimaryWindow>`). The fixed-step counter for `--minimize-test` is a `FixedUpdate`
system.

### 9.3 `examples/headless_world.rs` (WP9)

`MinimalPlugins` + `FrameStatsPlugin` + `WorldPlugin::default()` + one viewer entity
(`Transform` at `(0, 80, 0)`, `ChunkViewer { radius: 4 }`). Runs until
`Res<WorldStats>::is_settled()` (max 30 s, exit 4) and prints `chunks=.. ready=.. meshed=..
empty=.. quads=..` (sum of ready mesh quads) and the elapsed time. Proves streaming + meshing
without a GPU (CI-friendly).

### 9.4 `crates/boxel_render/tests/gpu_tiers.rs` (WP7)

1. Enumerate adapters with a headless instance (`InstanceFlags::empty()`, backends from
   `WGPU_BACKEND` if set and non-empty, else `VULKAN | GL`); filter by `WGPU_ADAPTER_NAME`.
   None -> skip (or fail if `gpu_tests_required()`).
2. Scenes, built in the test from `PackedQuad::new` (no worldgen or mesher needed):
   a heightfield (4x2x4 chunks, the same surface function as the reference probe) plus a
   floating slab and a layer of cutout quads (leaf texture) over part of it; and a far scene:
   a 32x32 opaque slab at about 480 voxels from the camera and a second slab of another texture
   1 voxel behind it (fully hidden).
3. For each adapter, for each tier in High, Mid, Low that a local check allows (`classify` is
   WP3's `[P]` work, so use the `[S]` pieces: High = `MULTI_DRAW_INDIRECT_COUNT |
   INDIRECT_FIRST_INSTANCE` + downlevel `COMPUTE_SHADERS` + Vulkan; Mid = `VERTEX_STORAGE` and,
   on GL, `caps.gl_version` with `supports_storage_and_compute()`; Low = `IMMEDIATES`; software
   adapters get every tier): `request_device` with `tier_request(t, &caps.limits)`, `VoxelRenderer::new(&device, &queue, t, .., Rgba8UnormSrgb, 320x180)`,
   upload, render a fixed camera, read back with `boxel_gfx::read_texture_rgba8`.
4. Assert: every heightfield image has a non-background fraction in [0.05, 0.95]; for each
   adapter, every tier image differs from the best tier image in at most 0.1 % of pixels
   (channel tolerance 2, `RgbaImage::compare`); the far scene with both slabs equals the far
   scene with only the front slab (<= 0.1 %), on every adapter including GL (reverse-Z
   precision is lower there but 1 m at 480 m must still resolve). On failure save PNGs under
   `CARGO_TARGET_TMPDIR/gpu_tiers/`.
5. Print a summary line per adapter/tier with the CPU time of `render`.

### 9.5 `crates/boxel_window/examples/window_smoke.rs` (WP2)

`cargo run -p boxel_window --example window_smoke -- [--frames N] [--resize-test]`.
`WindowPlugin` + `InputPlugin` + `TimePlugin`; prints `boxel-window: created WxH scale=S` on
`WindowCreated`; after N updates (default 120) writes `AppExit::Success` and prints
`boxel-window: updates=N redraws=M occluded_frames=K`; `--resize-test` requests a +100x+50
inner size at update 30 and exits 3 if no `WindowResized` arrives within 60 updates. Exit 0 on
success. Needs no human input.

### 9.6 `crates/boxel_render/tests/gpu_terrain.rs` and `tests/` (WP9)

- `gpu_terrain.rs`: like `gpu_tiers` but the scene is real: `TerrainGenerator::default()` chunks
  around the origin, meshed with `boxel_mesh::Mesher` (with neighbours), uploaded and rendered on
  every allowed tier; same image assertions (tiers within 0.1 % on one adapter).
- `tests/ambiguity.rs` (root package): an `App` with `TaskPoolPlugin`, `TimePlugin`,
  `FrameStatsPlugin`, `InputPlugin`, `FlyCameraPlugin`, `WorldPlugin` (InlineExecutor) and
  `ScheduleBuildSettings { ambiguity_detection: LogLevel::Error, .. }` set on `PreUpdate`,
  `Update`, `PostUpdate` and `Last` (`App::edit_schedule` + `set_build_settings`); one
  `app.update()` must not panic (no ambiguous engine systems).

---

## 10. Tooling files (verbatim, all in the skeleton)

### 10.1 `rust-toolchain.toml`

```toml
# file: rust-toolchain.toml
[toolchain]
# Bump deliberately (1.99.0 is out since 2026-10-01). MSRV lives in Cargo.toml (rust-version).
channel = "1.98.1"
components = ["rustfmt", "clippy"]
profile = "minimal"
```

### 10.2 `.cargo/config.toml` and `.cargo/config.local.toml.example`

```toml
# file: .cargo/config.toml
# Linker: nothing to configure on x86_64-unknown-linux-gnu (rust-lld is the default since 1.90,
# verified on 1.98.1). Per-developer tweaks (mold, target-cpu=native) go in the untracked
# .cargo/config.local.toml (see config.local.toml.example).
include = [{ path = "config.local.toml", optional = true }]

[alias]
lint = "clippy --workspace --all-targets --all-features --locked"

[target.x86_64-pc-windows-msvc]
linker = "rust-lld.exe"
```

```toml
# file: .cargo/config.local.toml.example
# Copy to .cargo/config.local.toml (git-ignored).
# [target.x86_64-unknown-linux-gnu]
# linker = "clang"
# rustflags = ["-C", "link-arg=-fuse-ld=mold"]
# [build]
# rustflags = ["-C", "target-cpu=native"]
```

### 10.3 `rustfmt.toml`

```toml
# file: rustfmt.toml
style_edition = "2024"
newline_style = "Unix"
use_field_init_shorthand = true
use_try_shorthand = true
max_width = 100
```

### 10.4 `clippy.toml`

```toml
# file: clippy.toml
# MSRV is read from `rust-version` in Cargo.toml.
allow-print-in-tests = true
allow-unwrap-in-tests = true
allow-expect-in-tests = true
allow-dbg-in-tests = true
avoid-breaking-exported-api = false
doc-valid-idents = ["WebGPU", "WGSL", "SPIR-V", "GLES", "OpenGL", "Vulkan", "RenderDoc", "DirectX", "MoltenVK", "llvmpipe", "lavapipe", "radeonsi", "RADV", "glvnd", "EGL", "Wayland", "XWayland", ".."]
stack-size-threshold = 524288
array-size-threshold = 16384
disallowed-methods = [
  { path = "std::thread::sleep", reason = "blocks the frame; use the runner's pacing (allowed with #[expect] in runners only)" },
  { path = "wgpu::util::initialize_adapter_from_env", reason = "panics on no match; use boxel_gfx::select_gpu" },
  { path = "wgpu::util::initialize_adapter_from_env_or_default", reason = "use boxel_gfx::select_gpu" },
]
```

### 10.5 `deny.toml`

```toml
# file: deny.toml
# cargo-deny 0.20.x (flags such as --all-features go before `check`).
# vendor/ is a separate Cargo workspace: its crates are path dependencies here, so licenses,
# advisories and bans apply to them too, but [bans.workspace-dependencies] only to crates/*.
[graph]
all-features = true
targets = [
  "x86_64-unknown-linux-gnu",
  "aarch64-unknown-linux-gnu",
  "x86_64-pc-windows-msvc",
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
]

[output]
feature-depth = 1

[advisories]
unmaintained = "workspace"
unsound = "workspace"
yanked = "deny"
ignore = [
  # { id = "RUSTSEC-YYYY-NNNN", reason = "why", expiry = "2027-01-01" },
]

[licenses]
allow = [
  "MIT", "MIT-0", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause",
  "BSL-1.0", "CC0-1.0", "ISC", "Unicode-3.0", "Zlib",
]
confidence-threshold = 0.9
unused-allowed-license = "allow"

[licenses.private]
ignore = true

[bans]
multiple-versions = "warn"
wildcards = "deny"
allow-wildcard-paths = true
highlight = "all"
deny = [
  { crate = "wgpu", deny-multiple-versions = true },
  { crate = "naga", deny-multiple-versions = true },
  { crate = "glam", deny-multiple-versions = true },
  { crate = "winit", deny-multiple-versions = true },
  { crate = "raw-window-handle", deny-multiple-versions = true },
  { crate = "tracy-client", deny-multiple-versions = true },
  # Policy (ADR 0003): no Bevy crates in the tree; the ECS is vendored under vendor/.
  # cargo-deny needs exact names; tools/no_bevy.py (CI) catches every other bevy* name.
  { crate = "bevy", reason = "boxel is standalone" },
  { crate = "bevy_ecs", reason = "use the vendored boxel_ecs" },
  { crate = "bevy_ecs_macros", reason = "use the vendored boxel_ecs_macros" },
  { crate = "bevy_ecs_macro_logic", reason = "use the vendored boxel_ecs_macro_logic" },
  { crate = "bevy_macro_utils", reason = "use the vendored boxel_macro_utils" },
  { crate = "bevy_ptr", reason = "use the vendored boxel_ptr" },
  { crate = "bevy_utils", reason = "use the vendored boxel_utils" },
  { crate = "bevy_app", reason = "use boxel_app" },
  { crate = "bevy_tasks", reason = "use the vendored boxel_tasks" },
  { crate = "bevy_platform", reason = "replaced by boxel_utils::platform" },
  { crate = "bevy_reflect", reason = "not used" },
  { crate = "bevy_reflect_derive", reason = "not used" },
  { crate = "bevy_derive", reason = "not used" },
  { crate = "bevy_math", reason = "use boxel_math (glam)" },
  { crate = "bevy_color", reason = "not used" },
]

[bans.workspace-dependencies]
duplicates = "deny"
include-path-dependencies = true
unused = "warn"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []
```

### 10.6 `typos.toml`

```toml
# file: typos.toml
[files]
# vendor/ is upstream Bevy code (never "fix" it here); docs/ and README.md are in Spanish.
extend-exclude = ["target/", "vendor/", "docs/", "README.md", "*.png", "*.lock"]

[default]
locale = "en-us"

[default.extend-words]

[default.extend-identifiers]
```

### 10.7 `justfile` (just >= 1.56)

`typos` fails with an install hint when the binary is missing; `BOXEL_SKIP_TYPOS=1` skips it
explicitly (CI always runs it through its action). GPU recipes run with `--test-threads=1`.

```just
# file: justfile
# boxel task runner. Requires just >= 1.56.
set shell := ["bash", "-euo", "pipefail", "-c"]
set windows-shell := ["pwsh", "-NoLogo", "-Command"]

msrv := `grep -m1 '^rust-version = ' Cargo.toml | cut -d'"' -f2`

[default]
list:
    @just --list --unsorted

[group('quality')]
[doc('Format all code')]
fmt:
    cargo fmt

[group('quality')]
[doc('Check formatting (CI)')]
fmt-check:
    cargo fmt --check

[group('quality')]
[doc('Clippy on every target and feature; warnings are errors')]
[env('CARGO_BUILD_WARNINGS', 'deny')]
lint:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    python3 tools/ecs_surface.py

[group('quality')]
[doc('Dependency policy: advisories, licenses, bans, sources, no bevy* crates, lock sync')]
deny:
    cargo deny --all-features check
    python3 tools/no_bevy.py
    python3 tools/vendor_lock_sync.py

[group('quality')]
[doc('Spell check (code only; Spanish docs are excluded). BOXEL_SKIP_TYPOS=1 skips it explicitly')]
typos:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ "${BOXEL_SKIP_TYPOS:-}" = "1" ]; then echo "typos: skipped (BOXEL_SKIP_TYPOS=1)"; exit 0; fi
    command -v typos >/dev/null || { echo "typos is not installed: cargo install typos-cli --locked (or BOXEL_SKIP_TYPOS=1)"; exit 1; }
    typos

[group('quality')]
[doc('Build docs; rustdoc warnings are errors')]
[env('CARGO_BUILD_WARNINGS', 'deny')]
doc:
    cargo doc --workspace --no-deps --all-features --locked

[group('quality')]
[doc('Compile with the declared MSRV toolchain')]
msrv:
    rustup toolchain install {{ msrv }} --profile minimal --no-self-update
    cargo +{{ msrv }} check --workspace --all-targets --locked

[group('test')]
[doc('Unit + integration tests (GPU tests self-skip without an adapter)')]
test *args:
    cargo test --workspace --locked {{ args }}

[group('test')]
[doc('GPU tests forced onto Mesa lavapipe (Vulkan, CPU)')]
[env('WGPU_BACKEND', 'vulkan')]
[env('WGPU_ADAPTER_NAME', 'llvmpipe')]
[env('BOXEL_REQUIRE_GPU', '1')]
test-lavapipe:
    cargo test --workspace --locked --test '*gpu*' -- --test-threads=1

# NVIDIA's glvnd EGL vendor wins on hybrid laptops when headless; force Mesa's.
[group('test')]
[doc('GPU tests forced onto Mesa llvmpipe (GL via EGL, CPU)')]
[env('__EGL_VENDOR_LIBRARY_FILENAMES', '/usr/share/glvnd/egl_vendor.d/50_mesa.json')]
[env('LIBGL_ALWAYS_SOFTWARE', '1')]
[env('WGPU_BACKEND', 'gl')]
[env('BOXEL_REQUIRE_GPU', '1')]
test-llvmpipe:
    cargo test --workspace --locked --test '*gpu*' -- --test-threads=1

[group('test')]
[doc('GPU tests on every real adapter of this machine')]
[env('BOXEL_REQUIRE_GPU', '1')]
test-gpu:
    cargo test --workspace --locked --test '*gpu*' -- --nocapture --test-threads=1

[group('test')]
[doc('Tests of the vendored ECS fork (its own workspace; upstream expects backtraces + debug)')]
[env('RUST_BACKTRACE', '1')]
test-vendor:
    cd vendor && cargo test --workspace --lib --locked
    cd vendor && cargo test -p boxel_ecs --doc --features debug --locked

[group('test')]
[doc('Miri on the vendored ECS (needs: rustup toolchain install nightly-2026-09-30 --component miri)')]
[env('MIRIFLAGS', '-Zmiri-disable-isolation')]
[env('CARGO_BUILD_WARNINGS', 'warn')]
[env('RUSTFLAGS', '-Zrandomize-layout')]
miri:
    cd vendor && cargo +nightly-2026-09-30 miri test -p boxel_ecs -p boxel_ptr -p boxel_tasks --lib

[group('test')]
[doc('Benchmarks (criterion). Filter: just bench mesh')]
bench *args:
    cargo bench --workspace {{ args }}

[group('run')]
[doc('Headless GPU report: just gpu-info --backend gl')]
gpu-info *args:
    cargo run -p boxel_gfx --example gpu_info -- {{ args }}

[group('run')]
[doc('Run an example: just run voxel_world --tier mid')]
run example *args:
    cargo run --release --example {{ example }} -- {{ args }}

[group('run')]
[doc('Optimized build with symbols, for Tracy/perf/RenderDoc')]
run-profiling example *args:
    cargo run --profile profiling --features trace_tracy --example {{ example }} -- {{ args }}

[group('run')]
[doc('RenderDoc capture under XWayland (RenderDoc does not support Wayland)')]
[env('WAYLAND_DISPLAY', '')]
[env('QT_QPA_PLATFORM', 'xcb')]
renderdoc example *args:
    cargo build --profile profiling --example {{ example }}
    @echo "In qrenderdoc launch target/profiling/examples/{{ example }} {{ args }} (env WAYLAND_DISPLAY= is set)"
    qrenderdoc

[group('vendor')]
[doc('Regenerate vendor/ from upstream bevy (default version: tools/refork/UPSTREAM_VERSION)')]
refork *version:
    tools/refork/refork.sh {{ version }}

[group('vendor')]
[doc('Check that vendor/Cargo.lock and Cargo.lock agree on shared packages')]
vendor-lock:
    python3 tools/vendor_lock_sync.py

[group('ci')]
[doc('Everything the per-PR CI runs, locally')]
ci: fmt-check lint test doc deny typos test-vendor
```

### 10.8 `.github/workflows/ci.yml`, `.github/workflows/miri.yml`, `.github/dependabot.yml`

Actions pinned to full SHAs (checkout v7.0.1, rust-cache v2.9.2, cargo-deny-action v2.1.1,
typos v1.50.3). Miri lives in its own workflow so it can run on every PR that touches
`vendor/**` or `tools/refork/**` (path filters apply per workflow), with a pinned nightly and
warnings not denied (new nightly lints in upstream code must not fail it).

```yaml
# file: .github/workflows/ci.yml
name: CI
on:
  push:
    branches: [main]
  pull_request:
  workflow_dispatch:
permissions:
  contents: read
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
env:
  CARGO_TERM_COLOR: always
  CARGO_INCREMENTAL: 0
  CARGO_BUILD_WARNINGS: deny # Cargo >= 1.97: rustc + rustdoc warnings are errors without RUSTFLAGS churn
  RUST_BACKTRACE: 1
  CARGO_PROFILE_DEV_DEBUG: line-tables-only
  CARGO_PROFILE_TEST_DEBUG: line-tables-only
jobs:
  fmt:
    name: rustfmt
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - run: cargo fmt --check # never --all: it would also format vendor/

  clippy:
    name: clippy (${{ matrix.os }})
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-24.04, windows-2025, macos-15]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
        with:
          save-if: ${{ github.ref == 'refs/heads/main' }}
      # --all-features includes `hal`, which compiles to nothing on macOS (cfg-gated: wgpu-hal has
      # no Vulkan backend there without vulkan-portability).
      - run: cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
      - if: runner.os == 'Linux'
        run: python3 tools/ecs_surface.py

  test:
    name: test (${{ matrix.os }})
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: ubuntu-24.04 # Mesa lavapipe (Vulkan) + llvmpipe (GL): GPU tests REQUIRED
            require_gpu: "1"
          - os: windows-2025 # WARP / whatever exists: GPU tests self-skip
            require_gpu: ""
          - os: macos-15 # Metal: GPU tests self-skip if no adapter
            require_gpu: ""
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Install Mesa software rasterizers (Linux)
        if: runner.os == 'Linux'
        run: |
          sudo apt-get update -qq
          sudo apt-get install -y --no-install-recommends \
            libvulkan1 mesa-vulkan-drivers libegl1 libegl-mesa0 libgl1-mesa-dri
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
        with:
          save-if: ${{ github.ref == 'refs/heads/main' }}
      - name: Unit + integration tests
        run: cargo test --workspace --locked
      - name: GPU tests on Vulkan (lavapipe)
        if: runner.os == 'Linux'
        env:
          BOXEL_REQUIRE_GPU: ${{ matrix.require_gpu }}
          WGPU_BACKEND: vulkan
          WGPU_ADAPTER_NAME: llvmpipe
        run: cargo test --workspace --locked --test '*gpu*' -- --nocapture --test-threads=1
      - name: GPU tests on GL (llvmpipe via EGL)
        if: runner.os == 'Linux'
        env:
          BOXEL_REQUIRE_GPU: ${{ matrix.require_gpu }}
          WGPU_BACKEND: gl
          __EGL_VENDOR_LIBRARY_FILENAMES: /usr/share/glvnd/egl_vendor.d/50_mesa.json
          LIBGL_ALWAYS_SOFTWARE: "1"
        run: cargo test --workspace --locked --test '*gpu*' -- --nocapture --test-threads=1
      - name: gpu_info (informational; DX12 uses FXC unless dxcompiler.dll is on PATH)
        if: runner.os != 'Linux'
        continue-on-error: true
        run: cargo run --locked -p boxel_gfx --example gpu_info -- --all-tiers
      - name: gpu_info (smoke)
        if: runner.os == 'Linux'
        env:
          WGPU_BACKEND: vulkan
        run: cargo run --locked -p boxel_gfx --example gpu_info

  vendor:
    name: vendored ECS tests
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
        with:
          workspaces: vendor
          save-if: ${{ github.ref == 'refs/heads/main' }}
      - run: cargo test --workspace --lib --locked
        working-directory: vendor
      - run: cargo test -p boxel_ecs --doc --features debug --locked
        working-directory: vendor

  msrv:
    name: MSRV
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Read rust-version from Cargo.toml
        id: msrv
        run: echo "version=$(grep -m1 '^rust-version = ' Cargo.toml | cut -d'"' -f2)" >> "$GITHUB_OUTPUT"
      - run: rustup toolchain install "${{ steps.msrv.outputs.version }}" --profile minimal --no-self-update
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
        with:
          save-if: ${{ github.ref == 'refs/heads/main' }}
      - run: cargo +${{ steps.msrv.outputs.version }} check --workspace --all-targets --all-features --locked

  docs:
    name: docs
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
        with:
          save-if: ${{ github.ref == 'refs/heads/main' }}
      - run: cargo doc --workspace --no-deps --all-features --locked

  deny:
    name: cargo-deny
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: EmbarkStudios/cargo-deny-action@3c6349835b2b7b196a839186cb8b78e02f7b5f25 # v2.1.1
        with:
          command: check
          arguments: --all-features
      - name: no bevy* crates anywhere (root and vendor graphs)
        run: python3 tools/no_bevy.py
      - name: vendor/Cargo.lock agrees with Cargo.lock
        run: python3 tools/vendor_lock_sync.py

  typos:
    name: typos
    runs-on: ubuntu-24.04
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: crate-ci/typos@00f422f3b19c57bc6338715ebfe3316d38768461 # v1.50.3
```

```yaml
# file: .github/workflows/miri.yml
name: Miri
# Weekly, on demand, and on every PR that touches the vendored ECS or its regeneration tooling
# (backports of unsound/UB fixes must not merge without Miri).
on:
  schedule:
    - cron: "0 3 * * 1"
  workflow_dispatch:
  pull_request:
    paths:
      - "vendor/**"
      - "tools/refork/**"
permissions:
  contents: read
concurrency:
  group: ${{ github.workflow }}-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}
env:
  CARGO_TERM_COLOR: always
  # Pinned nightly: bump deliberately (Miri breaks often on new nightlies).
  MIRI_NIGHTLY: nightly-2026-09-30
  # New nightly lints in upstream code we must not edit would otherwise fail the job.
  CARGO_BUILD_WARNINGS: warn
jobs:
  miri:
    name: Miri (vendored ECS)
    runs-on: ubuntu-24.04
    timeout-minutes: 150
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - run: rustup toolchain install "$MIRI_NIGHTLY" --profile minimal --component miri --no-self-update
      - run: cargo +"$MIRI_NIGHTLY" miri test --locked -p boxel_ecs -p boxel_ptr -p boxel_tasks --lib
        working-directory: vendor
        env:
          RUSTFLAGS: -Zrandomize-layout
          MIRIFLAGS: -Zmiri-disable-isolation
```

```yaml
# file: .github/dependabot.yml
version: 2
updates:
  - package-ecosystem: github-actions
    directory: /
    schedule:
      interval: weekly
```

### 10.9 `.gitignore`

```text
# file: .gitignore
/target/
/vendor/target/
**/*.rs.bk
/.cargo/config.local.toml
*.tracy
*.rdc
perf.data*
flamegraph*.svg
/shots/
**/*-actual.png
**/*-diff.png
.idea/
.vscode/
*.swp
.DS_Store
Thumbs.db
# Cargo.lock IS committed (root and vendor/).
```

### 10.10 Licenses and NOTICE

`LICENSE-APACHE` is the standard Apache-2.0 text; `LICENSE-MIT`:

```text
# file: LICENSE-MIT
MIT License

Copyright (c) 2026 boxel contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

```text
# file: NOTICE
boxel
Copyright (c) 2026 boxel contributors. Licensed under MIT OR Apache-2.0.

This repository contains code derived from the Bevy Engine (https://github.com/bevyengine/bevy),
Copyright (c) Bevy Engine contributors, licensed under MIT OR Apache-2.0:

- vendor/crates/boxel_ecs             <- bevy_ecs 0.20.0-rc.2
- vendor/crates/boxel_ecs_macros      <- bevy_ecs_macros 0.20.0-rc.2
- vendor/crates/boxel_ecs_macro_logic <- bevy_ecs_macro_logic 0.20.0-rc.2
- vendor/crates/boxel_macro_utils     <- bevy_macro_utils 0.20.0-rc.2
- vendor/crates/boxel_ptr             <- bevy_ptr 0.20.0-rc.2
- vendor/crates/boxel_tasks           <- bevy_tasks 0.20.0-rc.2
- vendor/crates/boxel_utils           <- bevy_utils 0.20.0-rc.2 and parts of bevy_platform 0.20.0-rc.2
                                         (src/platform/cfg.rs, hash.rs, cell/)
- crates/boxel_app                    <- design inspired by bevy_app 0.20.0-rc.2; the plugin tuple
                                         impl (`Plugins` marker trick) follows bevy_app's plugin.rs

Modifications: crate renames (bevy_* -> boxel_*), hand-written Cargo manifests, a minimal
replacement for bevy_platform, test-only edits, and the upstream fix bevyengine/bevy#25940
backported. The exact procedure is tools/refork/refork.sh; vendor/UPSTREAM.md records the
upstream version. Each vendored crate keeps its LICENSE-MIT and LICENSE-APACHE files.

"Bevy" is a trademark of its respective owners; boxel is not affiliated with or endorsed by the
Bevy project.
```

### 10.11 Repository checks: `tools/ecs_surface.py`, `tools/no_bevy.py`, `tools/vendor_lock_sync.py`

```python
#!/usr/bin/env python3
# file: tools/ecs_surface.py
"""Keeps boxel's own code on a curated boxel_ecs surface (ADR 0003).

Scans crates/, src/ and examples/ for paths through `boxel_ecs::` or `boxel::ecs::`, including
grouped imports such as `use boxel_ecs::{observer::On, world::World};` across lines, and fails if a
first-level module is outside the allowlist. Extend ALLOW only together with an ADR note.
Scope: engine crates and our examples. Derive macros expand to deeper paths; that is expected and
not checked (see ADR 0003: the exit hatch covers engine code only)."""
import os, re, sys

ALLOW = {
    'prelude', 'component', 'entity', 'message', 'query', 'resource', 'schedule', 'system',
    'world', 'change_detection', 'lifecycle', 'intern', 'label', 'error', 'bundle',
}
SKIP_DIRS = {'target', 'boxel_facade_check'}
ROOTS = ['crates', 'src', 'examples']
PATH = re.compile(r'\b(?:boxel_ecs|boxel\s*::\s*ecs)\s*::\s*')

def strip_comments(text):
    text = re.sub(r'/\*.*?\*/', ' ', text, flags=re.S)
    return re.sub(r'//[^\n]*', ' ', text)

def group(text, start):
    """Returns the contents of the balanced {...} starting at text[start] == '{'."""
    depth = 0
    for i in range(start, len(text)):
        depth += {'{': 1, '}': -1}.get(text[i], 0)
        if depth == 0:
            return text[start + 1:i]
    return text[start + 1:]

def split_top(s):
    out, depth, cur = [], 0, ''
    for ch in s:
        if ch == ',' and depth == 0:
            out.append(cur); cur = ''
            continue
        depth += {'{': 1, '}': -1}.get(ch, 0)
        cur += ch
    out.append(cur)
    return [x.strip() for x in out if x.strip()]

def first_segments(text, pos):
    if pos < len(text) and text[pos] == '{':
        return [re.split(r'\s*::\s*|\s+as\s+', item)[0].strip() for item in split_top(group(text, pos))]
    m = re.match(r'[A-Za-z_][A-Za-z0-9_]*', text[pos:])
    return [m.group(0)] if m else []

def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    bad = []
    for top in ROOTS:
        for dp, dirs, files in os.walk(os.path.join(root, top)):
            dirs[:] = [d for d in dirs if d not in SKIP_DIRS]
            for f in files:
                if not f.endswith('.rs'):
                    continue
                p = os.path.join(dp, f)
                text = strip_comments(open(p, encoding='utf-8').read())
                for m in PATH.finditer(text):
                    for seg in first_segments(text, m.end()):
                        if seg and seg != 'self' and seg not in ALLOW:
                            line = text.count('\n', 0, m.start()) + 1
                            bad.append(f'{os.path.relpath(p, root)}:{line}: boxel_ecs::{seg}')
    if bad:
        print('boxel_ecs modules outside the curated surface (extend ALLOW in tools/ecs_surface.py only with an ADR note):')
        print('\n'.join(bad))
        sys.exit(1)
    print('ecs surface ok')

if __name__ == '__main__':
    main()
```

```python
#!/usr/bin/env python3
# file: tools/no_bevy.py
"""Fails if any crate whose name starts with `bevy` is in the dependency graph of the root
workspace or of vendor/ (ADR 0003: zero bevy_* crates). cargo-deny bans need exact names; this
catches every bevy* name, including ones a third-party crate might pull in."""
import json, os, subprocess, sys

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
bad = []
for manifest in ['Cargo.toml', 'vendor/Cargo.toml']:
    out = subprocess.run(
        ['cargo', 'metadata', '--format-version', '1', '--all-features', '--locked',
         '--manifest-path', os.path.join(root, manifest)],
        check=True, capture_output=True, text=True).stdout
    names = {p['name'] for p in json.loads(out)['packages']}
    bad += [f'{manifest}: {n}' for n in sorted(names) if n.startswith('bevy')]
if bad:
    print('bevy crates in the dependency graph (forbidden by ADR 0003):')
    print('\n'.join(bad))
    sys.exit(1)
print('no bevy crates')
```

```python
#!/usr/bin/env python3
# file: tools/vendor_lock_sync.py
"""Checks that vendor/Cargo.lock and the root Cargo.lock agree on shared packages.

vendor/ tests and Miri run against vendor/Cargo.lock while the engine links what the root lock
picked; a version skew means the vendored tests do not cover the code we ship. For every package
name present in both lockfiles, the vendor versions must also be in the root lock. Packages only
used by vendor dev-dependencies are reported as info. Fix a skew with
`(cd vendor && cargo update -p NAME --precise VERSION)` using the root lock's version."""
import os, sys, tomllib

root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

def versions(path):
    data = tomllib.load(open(path, 'rb'))
    out = {}
    for p in data.get('package', []):
        if p.get('source', '').startswith('registry+'):
            out.setdefault(p['name'], set()).add(p['version'])
    return out

root_lock = versions(os.path.join(root, 'Cargo.lock'))
vendor_lock = versions(os.path.join(root, 'vendor', 'Cargo.lock'))
skew = []
for name, vs in sorted(vendor_lock.items()):
    if name in root_lock and not vs <= root_lock[name]:
        skew.append(f'{name}: vendor {sorted(vs)} root {sorted(root_lock[name])}')
if skew:
    print('vendor/Cargo.lock and Cargo.lock disagree:')
    print('\n'.join(skew))
    sys.exit(1)
print(f'lockfiles agree ({len(set(root_lock) & set(vendor_lock))} shared packages)')
```

### 10.12 `tools/refork/`

`refork.sh` (verified end to end on 2026-10-05: library diff zero proved by
`check_test_only.py`, 171 files carry the modification notice, vendor tests 961/961 and doctests
480/480 pass with `--locked`). Changes since v1: crate sources located with `cargo metadata`
(mirrors and `CARGO_HOME` honoured); the derive rename is asserted; un-renamed `bevy_*` paths
fail the run; a backport that is already upstream fails with "delete it"; `vendor/Cargo.lock` is
kept (seeded from `template/Cargo.lock` the first time).

```bash
#!/usr/bin/env bash
# file: tools/refork/refork.sh
# Regenerates vendor/ (stripped fork of bevy_ecs + deps, renamed boxel_*) from crates.io sources.
#   usage: tools/refork/refork.sh [bevy-version]     (default: tools/refork/UPSTREAM_VERSION)
# Policy (ADR 0003): zero edits to library code. Everything boxel-specific lives in other crates.
# Our changes are limited to: hand-written manifests (template/), a small bevy_platform
# replacement (template/platform + vendored cfg/hash/cell files), mechanical renames,
# test-only patches (patch_tests.py, verified by check_test_only.py) and backported upstream
# fixes (patches/*.patch, applied in lexical order; each file name says which upstream PR it
# backports). Every .rs file that differs from upstream gets a one-line "modified" header
# (Apache-2.0 section 4(b)).
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
HERE="$ROOT/tools/refork"
V=${1:-$(tr -d '[:space:]' < "$HERE/UPSTREAM_VERSION")}
TPL="$HERE/template"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
die() { echo "refork: $*" >&2; exit 1; }

echo "== fetching bevy $V sources from crates.io"
mkdir -p "$WORK/fetch/src"
cat > "$WORK/fetch/Cargo.toml" <<TOML
[package]
name = "refork-fetch"
version = "0.0.0"
edition = "2024"
publish = false
[dependencies]
bevy_ecs = "=$V"
bevy_ecs_macros = "=$V"
bevy_ecs_macro_logic = "=$V"
bevy_macro_utils = "=$V"
bevy_ptr = "=$V"
bevy_tasks = "=$V"
bevy_utils = "=$V"
bevy_platform = "=$V"
TOML
touch "$WORK/fetch/src/lib.rs"
(cd "$WORK/fetch" && cargo fetch --quiet)
# Source directory of each crate, from cargo itself (honours CARGO_HOME, mirrors, source replacement).
cargo metadata --format-version 1 --manifest-path "$WORK/fetch/Cargo.toml" > "$WORK/meta.json"
src_dir() {
  python3 - "$WORK/meta.json" "$1" "$V" <<'PY'
import json, os, sys
meta, name, ver = json.load(open(sys.argv[1])), sys.argv[2], sys.argv[3]
hits = [p for p in meta["packages"] if p["name"] == name and p["version"] == ver]
if len(hits) != 1:
    sys.exit(f"cannot locate {name} {ver} in cargo metadata")
print(os.path.dirname(hits[0]["manifest_path"]))
PY
}

OUT="$WORK/vendor"
PRISTINE="$WORK/pristine"   # upstream sources, for the "modified" headers
mkdir -p "$OUT/crates" "$PRISTINE"
cp "$TPL/Cargo.toml" "$TPL/LICENSE-MIT" "$TPL/LICENSE-APACHE" "$OUT/"
for pair in bevy_ecs:boxel_ecs bevy_ecs_macros:boxel_ecs_macros bevy_ecs_macro_logic:boxel_ecs_macro_logic \
            bevy_macro_utils:boxel_macro_utils bevy_ptr:boxel_ptr bevy_tasks:boxel_tasks bevy_utils:boxel_utils; do
  up=${pair%%:*}; ours=${pair##*:}
  dir=$(src_dir "$up") || die "$up $V not found"
  cp -r "$dir" "$OUT/crates/$ours"
  cp -r "$dir" "$PRISTINE/$ours"
  rm -f "$OUT/crates/$ours/Cargo.lock" "$OUT/crates/$ours/Cargo.toml.orig" "$OUT/crates/$ours/.cargo_vcs_info.json" "$OUT/crates/$ours/.cargo-ok"
  cp "$TPL/crates/$ours/Cargo.toml" "$OUT/crates/$ours/Cargo.toml"
done

echo "== bevy_platform replacement"
PLAT=$(src_dir bevy_platform) || die "bevy_platform $V not found"
mkdir -p "$OUT/crates/boxel_utils/src/platform/cell" "$PRISTINE/boxel_utils/src/platform/cell"
cp "$PLAT/src/cfg.rs" "$PLAT/src/hash.rs" "$OUT/crates/boxel_utils/src/platform/"
cp "$PLAT/src/cfg.rs" "$PLAT/src/hash.rs" "$PRISTINE/boxel_utils/src/platform/"
cp "$PLAT/src/cell/"*.rs "$OUT/crates/boxel_utils/src/platform/cell/"
cp "$PLAT/src/cell/"*.rs "$PRISTINE/boxel_utils/src/platform/cell/"
sed "s/@UPSTREAM_VERSION@/$V/g" "$TPL/platform/mod.rs" > "$OUT/crates/boxel_utils/src/platform/mod.rs"
cp "$TPL/platform/collections.rs" "$OUT/crates/boxel_utils/src/platform/"

cd "$OUT/crates"
python3 - <<'PY'
p = 'boxel_utils/src/platform/cfg.rs'; s = open(p).read()
for a, b in [('#[cfg(feature = "alloc")] => {', '#[cfg(all())] => {'),
             ('#[cfg(feature = "std")] => {', '#[cfg(all())] => {'),
             ('#[cfg(all(target_arch = "wasm32", feature = "web"))] => {', '#[cfg(any())] => {'),
             ('#[cfg(all(feature = "alloc", target_has_atomic = "ptr"))] => {', '#[cfg(target_has_atomic = "ptr")] => {'),
             ('#[cfg(feature = "critical-section")] => {', '#[cfg(any())] => {')]:
    assert a in s, f"bevy_platform cfg.rs changed upstream ({a}): update refork.sh"
    s = s.replace(a, b)
open(p, 'w').write(s)
p = 'boxel_utils/src/lib.rs'; s = open(p).read()
old = 'pub mod cfg {\n    pub(crate) use bevy_platform::cfg::*;\n\n    pub use bevy_platform::cfg::{alloc, std};'
assert old in s, "boxel_utils lib.rs layout changed upstream: update refork.sh"
s = s.replace(old, '/// Minimal desktop-only replacement for `bevy_platform` (boxel shim).\npub mod platform;\n\n/// Configuration information for this crate.\npub mod cfg {\n    pub(crate) use crate::platform::cfg::*;\n\n    pub use crate::platform::cfg::{alloc, std};', 1)
s = s.replace('/// Configuration information for this crate.\n/// Minimal desktop-only', '/// Minimal desktop-only', 1)
open(p, 'w').write(s)
PY
grep -rl "bevy_platform" boxel_utils/src | xargs sed -i -E '/^\s*(\/\/\/|\/\/!)/! s/bevy_platform::/crate::platform::/g'

echo "== mechanical renames"
grep -rl --include='*.rs' --include='*.md' -E "bevy_(ecs|ptr|tasks|macro_utils|utils|platform)" . | xargs sed -i -E \
  's/bevy_platform::/boxel_utils::platform::/g; s/\bbevy_platform\b/boxel_utils::platform/g; s/bevy_ecs/boxel_ecs/g; s/bevy_ptr/boxel_ptr/g; s/bevy_tasks/boxel_tasks/g; s/bevy_macro_utils/boxel_macro_utils/g; s/bevy_utils/boxel_utils/g'
MANIFEST=boxel_macro_utils/src/bevy_manifest.rs
sed -i 's/^const BEVY: &str = "bevy";/const BEVY: \&str = "boxel";/; s/strip_prefix("bevy_")/strip_prefix("boxel_")/' "$MANIFEST"
# Derive path resolution depends on these two edits; fail loudly if upstream reformatted them.
grep -q 'const BEVY: &str = "boxel";' "$MANIFEST" || die "derive facade rename failed (const BEVY) in $MANIFEST"
grep -q 'strip_prefix("boxel_")' "$MANIFEST" || die "derive facade rename failed (strip_prefix) in $MANIFEST"
left=$(grep -rn --include='*.rs' -E '\bbevy_(ecs|ptr|tasks|macro_utils|utils|platform)\b' . || true)
[ -z "$left" ] || { echo "$left" | head -5; die "un-renamed bevy_* paths remain"; }
cp -r "$OUT" "$WORK/renamed"   # rename-only snapshot for the test-only check

echo "== test-only patches"
python3 "$HERE/patch_tests.py" .
# Library diff must stay zero: every line patch_tests.py changed must be test or doc code.
python3 "$HERE/check_test_only.py" "$WORK/renamed/crates" "$OUT/crates"

echo "== upstream backports"
cd "$OUT"
for p in "$HERE"/patches/*.patch; do
  [ -e "$p" ] || continue
  name=$(basename "$p")
  if git apply -R --check -p1 "$p" 2>/dev/null; then
    die "$name is already contained in bevy $V: delete it from tools/refork/patches/ and rerun"
  fi
  git apply --check -p1 "$p" || die "$name does not apply to bevy $V: rebase or drop it"
  echo "   applying $name"
  git apply -p1 "$p"
done

echo "== modification notices"
python3 - "$PRISTINE" "$OUT/crates" <<'PY'
import os, sys
pristine, out = sys.argv[1], sys.argv[2]
HEADER = "// boxel: modified by tools/refork (crate renames, platform shim, test-only patches or backports).\n"
n = 0
for dp, _, fs in os.walk(out):
    for f in fs:
        if not f.endswith('.rs'):
            continue
        p = os.path.join(dp, f)
        rel = os.path.relpath(p, out)
        q = os.path.join(pristine, rel)
        s = open(p).read()
        if not os.path.exists(q) or open(q).read() != s:
            open(p, 'w').write(HEADER + s)
            n += 1
print(f"   {n} files carry the notice")
PY

cat > "$OUT/UPSTREAM.md" <<MD
# vendor/: stripped fork of bevy_ecs

Generated by \`tools/refork/refork.sh\` on $(date -u +%Y-%m-%d) from crates.io bevy **$V**.
Do not edit files here by hand: change \`tools/refork/\` and regenerate.

Backported upstream fixes (tools/refork/patches/):
$(for p in "$HERE"/patches/*.patch; do [ -e "$p" ] && echo "- $(basename "$p")"; done)

License: MIT OR Apache-2.0 (Bevy Engine contributors); see LICENSE-MIT, LICENSE-APACHE and the
repository NOTICE. Every modified .rs file starts with a "boxel: modified by tools/refork" line.
MD

echo "== lockfile"
# Keep the committed vendor lock (reproducible tests and Miri); seed from the template otherwise.
if [ -f "$ROOT/vendor/Cargo.lock" ]; then
  cp "$ROOT/vendor/Cargo.lock" "$OUT/Cargo.lock"
elif [ -f "$TPL/Cargo.lock" ]; then
  cp "$TPL/Cargo.lock" "$OUT/Cargo.lock"
fi
(cd "$OUT" && { cargo metadata --offline --format-version 1 > /dev/null 2>&1 || cargo generate-lockfile; })

rm -rf "$ROOT/vendor"
mv "$OUT" "$ROOT/vendor"
echo "== done. Verify: (cd vendor && cargo test --workspace --lib --locked) && cargo check --workspace --all-targets --locked && python3 tools/vendor_lock_sync.py"
```

```python
#!/usr/bin/env python3
# file: tools/refork/check_test_only.py
"""Fails if patch_tests.py changed any line outside test or doc code (ADR 0003: library diff zero).
usage: check_test_only.py <rename-only crates dir> <patched crates dir>
A changed line is allowed if it is blank, a doc comment (/// or //!), inside a #[cfg(test)] module,
or in a file under tests/, benches/ or examples/."""
import difflib, os, re, sys

def test_regions(lines):
    """Line indices inside `#[cfg(test)] mod x { ... }` blocks (naive brace counting)."""
    inside = set()
    i = 0
    while i < len(lines):
        if re.match(r'\s*#\[cfg\(test\)\]', lines[i]):
            j = i
            while j < len(lines) and not re.search(r'\bmod\s+\w+\s*\{', lines[j]):
                if j > i + 3:
                    break
                j += 1
            if j < len(lines) and re.search(r'\bmod\s+\w+\s*\{', lines[j]):
                depth = 0
                k = j
                while k < len(lines):
                    depth += lines[k].count('{') - lines[k].count('}')
                    inside.add(k)
                    if depth <= 0 and k > j:
                        break
                    k += 1
                inside.update(range(i, j))
                i = k
        i += 1
    return inside

def allowed(rel, lines, idx, regions):
    if re.search(r'(^|/)(tests|benches|examples)/', rel):
        return True
    s = lines[idx].strip()
    return s == '' or s.startswith('///') or s.startswith('//!') or idx in regions

def main(a_root, b_root):
    bad = []
    for dp, _, fs in os.walk(b_root):
        for f in fs:
            if not f.endswith('.rs'):
                continue
            b_path = os.path.join(dp, f)
            rel = os.path.relpath(b_path, b_root)
            a_path = os.path.join(a_root, rel)
            if not os.path.exists(a_path):
                bad.append(f'{rel}: new file')
                continue
            a = open(a_path).read().split('\n')
            b = open(b_path).read().split('\n')
            if a == b:
                continue
            ra, rb = test_regions(a), test_regions(b)
            sm = difflib.SequenceMatcher(None, a, b, autojunk=False)
            for tag, i1, i2, j1, j2 in sm.get_opcodes():
                if tag == 'equal':
                    continue
                for i in range(i1, i2):
                    if not allowed(rel, a, i, ra):
                        bad.append(f'{rel}:{i + 1}: - {a[i].strip()}')
                for j in range(j1, j2):
                    if not allowed(rel, b, j, rb):
                        bad.append(f'{rel}:{j + 1}: + {b[j].strip()}')
    if bad:
        print('check_test_only: patch_tests.py touched library code:')
        print('\n'.join(bad[:50]))
        sys.exit(1)
    print('   test-only patches verified (library diff zero)')

if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2])
```

`UPSTREAM_VERSION` contains `0.20.0-rc.2`. `patch_tests.py` is unchanged from the validated
fork template. `patches/0001-bevy-25940-fix-entity-equivalent-index-set.patch` is bevy commit
157e1ce6b (already renamed to `boxel_ecs` paths; applies with `git apply -p1` inside the output
tree). `template/Cargo.lock` seeds `vendor/Cargo.lock`. `template/platform/mod.rs` names the
bevy-platform crate in a form the rename sed cannot mangle and gets `@UPSTREAM_VERSION@`
substituted. `template/Cargo.toml` (the vendor workspace manifest):

```toml
# file: tools/refork/template/Cargo.toml
# vendor/ workspace: the stripped bevy_ecs fork generated by tools/refork/refork.sh.
# It keeps upstream Bevy's lint set (not boxel's). The cfg values below are upstream features
# removed by the refork (bevy_reflect, hotpatching, stepping); declaring them keeps the build
# warning-free without touching library code.
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.lints.clippy]
doc_markdown = "warn"
manual_let_else = "warn"
match_same_arms = "warn"
redundant_closure_for_method_calls = "warn"
redundant_else = "warn"
semicolon_if_nothing_returned = "warn"
type_complexity = "allow"
undocumented_unsafe_blocks = "warn"
unwrap_or_default = "warn"
needless_lifetimes = "allow"
too_many_arguments = "allow"
nonstandard_macro_braces = "warn"
print_stdout = "warn"
print_stderr = "warn"
ptr_as_ptr = "warn"
ptr_cast_constness = "warn"
ref_as_ptr = "warn"
too_long_first_doc_paragraph = "allow"
std_instead_of_core = "warn"
std_instead_of_alloc = "warn"
alloc_instead_of_core = "warn"
allow_attributes = "warn"
allow_attributes_without_reason = "warn"

[workspace.lints.rust]
missing_docs = "warn"
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(docsrs_dep)', 'cfg(feature, values("bevy_reflect", "hotpatching", "bevy_debug_stepping", "reflect_functions"))'] }
unsafe_code = "deny"
unsafe_op_in_unsafe_fn = "warn"
unused_qualifications = "warn"

[profile.dev]
debug = 0
incremental = false

[profile.test]
debug = 0
incremental = false
```

`template/crates/boxel_ecs/Cargo.toml` (the other six follow the same pattern; see the skeleton):

```toml
# file: tools/refork/template/crates/boxel_ecs/Cargo.toml
# Template copied over vendor/crates/boxel_ecs/Cargo.toml by tools/refork/refork.sh. Edit HERE, not in vendor/.
[package]
name = "boxel_ecs"
version = "0.1.0"
edition = "2024"
description = "boxel's entity component system (stripped fork of bevy_ecs)"
license = "MIT OR Apache-2.0"
rust-version = "1.95.0"

[features]
default = ["std", "async_executor", "backtrace", "multi_threaded"]
multi_threaded = ["boxel_tasks/multi_threaded"]
serialize = ["dep:serde", "boxel_utils/serialize", "indexmap/serde"]
backtrace = ["std"]
trace = ["std", "dep:tracing"]
debug = ["boxel_utils/debug", "dep:rand"]
detailed_trace = ["trace"]
track_location = []
async_executor = ["std", "boxel_tasks/async_executor"]
std = [
  "boxel_utils/parallel",
  "boxel_utils/std",
  "bitflags/std",
  "concurrent-queue/std",
  "fixedbitset/std",
  "indexmap/std",
  "serde?/std",
  "nonmax/std",
  "arrayvec/std",
  "log/std",
]

[dependencies]
boxel_ptr = { path = "../boxel_ptr", version = "0.1.0" }
boxel_tasks = { path = "../boxel_tasks", version = "0.1.0", default-features = false }
boxel_utils = { path = "../boxel_utils", version = "0.1.0", default-features = false }
boxel_ecs_macros = { path = "../boxel_ecs_macros", version = "0.1.0" }

bitflags = { version = "2.3", default-features = false }
fixedbitset = { version = "0.5", default-features = false }
serde = { version = "1", default-features = false, features = ["alloc", "serde_derive"], optional = true }
thiserror = { version = "2", default-features = false }
derive_more = { version = "2", default-features = false, features = ["from", "display", "into", "as_ref"] }
nonmax = { version = "0.5.4", default-features = false }
arrayvec = { version = "0.7.4", default-features = false }
smallvec = { version = "1", default-features = false, features = ["union", "const_generics"] }
indexmap = { version = "2.5.0", default-features = false }
variadics_please = { version = "2.0", default-features = false }
tracing = { version = "0.1", default-features = false, optional = true }
log = { version = "0.4", default-features = false }
bumpalo = "3"
slotmap = { version = "1.0.7", default-features = false }
concurrent-queue = { version = "2.5.0", default-features = false }
rand = { version = "0.10", default-features = false, optional = true }

[dev-dependencies]
rand = "0.10"
static_assertions = "1.1.0"
serde_test = "1.0"

[[example]]
name = "events"
path = "examples/events.rs"

[[example]]
name = "resources"
path = "examples/resources.rs"

[[example]]
name = "change_detection"
path = "examples/change_detection.rs"

[lints]
workspace = true
```

Offline note: `refork.sh` calls `cargo fetch`; without network run it with
`CARGO_NET_OFFLINE=true` (the bevy 0.20.0-rc.2 sources are in the local registry).


---

## 11. Work packages and process

Phases: **preflight** (lead) -> **WP0** (one agent) -> **phase A** (WP1..WP7 in parallel) ->
**gate A** (lead) -> **phase B** (WP8, WP9 in parallel) -> **gate B** (lead) -> **phase C**
(WP10, one agent). "The lead" is the orchestrating session; it is the only party that changes
non-owned files during a phase.

Paths: `$REPO=/home/ezequiel/Documentos/projects/boxel`, `$AGENTS=/home/ezequiel/.cache/boxel-agents`,
`$FINAL` = the directory that contains this spec (its copy lives in `$REPO/.scaffold/design/`
after WP0).

### 11.1 Ownership table (`.scaffold/owners.tsv`, in the skeleton)

```text
# file: .scaffold/owners.tsv
# Path ownership per work package (SCAFFOLD_SPEC section 11). Tab separated: <wp> <pattern>.
# A pattern ending in "/" covers the whole directory; "!" excludes (exclusions win).
# Every package also owns .scaffold/deviations/<wp>.md. wp0 and wp10 own everything.
wp1	crates/boxel_app/
wp1	crates/boxel_log/
wp1	crates/boxel_diagnostic/
wp1	crates/boxel_math/
wp1	crates/boxel_transform/
wp2	crates/boxel_window/
wp2	crates/boxel_camera/
wp3	crates/boxel_gfx/
wp4	crates/boxel_voxel/
wp4	crates/boxel_worldgen/
wp5	crates/boxel_mesh/
wp6	crates/boxel_world/
wp7	crates/boxel_render/
wp7	!crates/boxel_render/src/plugin.rs
wp7	crates/boxel_shaders/
# Phase B (after gate A). WP8 = GPU/window side, WP9 = CPU side + integration; both may fix
# phase-A code inside their half. Core crates (boxel_app, log, diagnostic, math, transform) are
# lead-only in phase B.
wp8	crates/boxel_render/
wp8	!crates/boxel_render/tests/gpu_terrain.rs
wp8	crates/boxel_gfx/
wp8	crates/boxel_shaders/
wp8	crates/boxel_window/
wp8	crates/boxel_camera/
wp8	examples/voxel_world.rs
wp9	crates/boxel_voxel/
wp9	crates/boxel_worldgen/
wp9	crates/boxel_mesh/
wp9	crates/boxel_world/
wp9	crates/boxel_render/tests/gpu_terrain.rs
wp9	examples/headless_world.rs
wp9	tests/
wp9	README.md
```

`scaffold.py` (in the skeleton, run with `python3`):

```python
#!/usr/bin/env python3
# file: .scaffold/bin/scaffold.py
"""M0 scaffold helper (deleted with .scaffold/ by WP10). Private working copies per package.

  scaffold.py start <wp>   copy the repo to $AGENTS/<wp>/ws (+ seed target dir), record a manifest
  scaffold.py check <wp>   list files the package changed outside its owned paths (exit 1 if any)
  scaffold.py gate <wp>    check, then copy the package's owned changes back into the repo
  scaffold.py sync <wp> <path>...  copy lead hot-patched (non-owned) repo files into the package copy
  scaffold.py seed         build the shared seed target dir from the repo ($AGENTS/seed)

$AGENTS = $BOXEL_AGENTS_DIR or ~/.cache/boxel-agents. Owned paths: .scaffold/owners.tsv."""
import hashlib, json, os, shutil, subprocess, sys

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
AGENTS = os.environ.get('BOXEL_AGENTS_DIR', os.path.expanduser('~/.cache/boxel-agents'))
SKIP = {'target', '.git'}
ALL_OWNERS = {'wp0', 'wp10'}

def files(root):
    out = {}
    for dp, dirs, fs in os.walk(root):
        rel_dir = os.path.relpath(dp, root)
        if rel_dir == '.':
            dirs[:] = [d for d in dirs if d not in SKIP]
        dirs[:] = [d for d in dirs if d != 'target']
        for f in fs:
            p = os.path.join(dp, f)
            rel = os.path.normpath(os.path.relpath(p, root))
            if rel.startswith('.scaffold/start-manifest'):
                continue
            out[rel] = hashlib.sha256(open(p, 'rb').read()).hexdigest()
    return out

def owns(wp, rel):
    if wp in ALL_OWNERS or rel == f'.scaffold/deviations/{wp}.md':
        return True
    inc, exc = False, False
    for line in open(os.path.join(REPO, '.scaffold/owners.tsv')):
        if line.startswith('#') or not line.strip():
            continue
        who, pat = line.rstrip('\n').split('\t', 1)
        if who != wp:
            continue
        neg = pat.startswith('!')
        pat = pat.lstrip('!')
        hit = rel.startswith(pat) if pat.endswith('/') else rel == pat
        if hit and neg:
            exc = True
        elif hit:
            inc = True
    return inc and not exc

def paths(wp):
    base = os.path.join(AGENTS, wp)
    return os.path.join(base, 'ws'), os.path.join(base, 'target')

def manifest_path(ws, wp):
    return os.path.join(ws, '.scaffold', f'start-manifest-{wp}.json')

def start(wp):
    ws, target = paths(wp)
    os.makedirs(ws, exist_ok=True)
    subprocess.run(['rsync', '-a', '--delete', '--exclude=/target', '--exclude=/.git',
                    '--exclude=/vendor/target', REPO + '/', ws + '/'], check=True)
    json.dump(files(ws), open(manifest_path(ws, wp), 'w'), indent=0)
    seed = os.path.join(AGENTS, 'seed')
    if not os.path.exists(target) and os.path.isdir(seed):
        subprocess.run(['cp', '-a', seed, target], check=True)
    print(f'export CARGO_TARGET_DIR={target} CARGO_BUILD_JOBS=4')
    print(f'cd {ws}')

def changes(wp):
    ws, _ = paths(wp)
    before = json.load(open(manifest_path(ws, wp)))
    now = files(ws)
    changed = sorted(r for r in set(before) | set(now) if before.get(r) != now.get(r))
    return ws, before, now, changed

def check(wp):
    _, before, now, changed = changes(wp)
    bad = [r for r in changed if not owns(wp, r)]
    for r in bad:
        kind = 'added' if r not in before else 'deleted' if r not in now else 'modified'
        print(f'{wp}: {kind} non-owned path {r}')
    print(f'{wp}: {len(changed)} changed paths, {len(bad)} outside ownership')
    return not bad

def gate(wp):
    if not check(wp):
        sys.exit(1)
    ws, before, now, changed = changes(wp)
    repo_now = files(REPO)
    for r in changed:
        if repo_now.get(r) != before.get(r):
            sys.exit(f'{wp}: {r} also changed in the repo since start (lead hot-patch?): merge by hand')
    for r in changed:
        dst = os.path.join(REPO, r)
        if r in now:
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(os.path.join(ws, r), dst)
        elif os.path.exists(dst):
            os.remove(dst)
    print(f'{wp}: copied {len(changed)} owned paths into {REPO}')

def sync(wp, rels):
    ws, _ = paths(wp)
    manifest = json.load(open(manifest_path(ws, wp)))
    for rel in rels:
        rel = os.path.normpath(rel)
        if owns(wp, rel):
            sys.exit(f'{wp}: {rel} is owned by {wp}; the package edits it itself')
        src = os.path.join(REPO, rel)
        dst = os.path.join(ws, rel)
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        shutil.copy2(src, dst)
        manifest[rel] = hashlib.sha256(open(src, 'rb').read()).hexdigest()
        print(f'{wp}: synced {rel}')
    json.dump(manifest, open(manifest_path(ws, wp), 'w'), indent=0)

def seed():
    env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(AGENTS, 'seed'))
    for cmd in (['cargo', 'test', '--workspace', '--all-features', '--locked', '--no-run'],
                ['cargo', 'clippy', '--workspace', '--all-targets', '--all-features', '--locked']):
        subprocess.run(cmd, cwd=REPO, env=env, check=True)

if __name__ == '__main__':
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == 'start':
        start(args[0])
    elif cmd == 'check':
        sys.exit(0 if check(args[0]) else 1)
    elif cmd == 'gate':
        gate(args[0])
    elif cmd == 'sync':
        sync(args[0], args[1:])
    elif cmd == 'seed':
        seed()
    else:
        sys.exit(__doc__)
```

### 11.2 Preflight, gates and escalation (lead)

Preflight, before WP0:

1. Ask Ezequiel in one short Spanish message: (a) may we run `cargo install typos-cli --locked`
   (a download)? If not, local runs use `BOXEL_SKIP_TYPOS=1` and the final report says so; CI
   still runs typos. (b) May the lead create a LOCAL git baseline (`git init` + one commit, no
   remote, no push)? Optional: ownership is enforced by `scaffold.py` either way. (c) Windows
   will open and close by themselves on his desktop during phases A (WP2, short) to C; keep the
   session unlocked.
2. Check: `echo $WAYLAND_DISPLAY` is non-empty; `df -h /home` shows at least 60 GB free;
   `just --version` >= 1.56; `cargo deny --version` 0.20.x; `rustup toolchain list` has 1.98.1
   and 1.97.1; `python3 --version` >= 3.11 (`tomllib`); `rsync` exists.

Gate A (after every phase A package reported done), and the same for gate B with WP8/WP9:

```bash
cd $REPO
for wp in wp1 wp2 wp3 wp4 wp5 wp6 wp7; do python3 .scaffold/bin/scaffold.py gate $wp || exit 1; done
export CARGO_TARGET_DIR=$AGENTS/lead/target
cargo fmt --check && CARGO_BUILD_WARNINGS=deny cargo check --workspace --all-targets --all-features --locked \
  && cargo test --workspace --locked && just lint && just doc && just test-lavapipe && just test-llvmpipe
python3 .scaffold/bin/scaffold.py seed      # refresh the seed target for the next phase
```

A gate failure caused by one package goes back to that package (its `ws` still exists; it fixes
there and the lead gates it again). Integration failures between packages are fixed by the next
phase (B for A, C for B) or by the lead.

Escalation for a wrong frozen item (`BLOCKING:` deviation): the lead decides within the phase.
Either it hot-patches the file in `$REPO` (only the lead may), then runs `scaffold.py sync <wp>
<path>` for every running package that needs it, or it records the decision in
`.scaffold/deviations/wp0.md` and the blocked part moves to the next phase. Never let a package
work around a non-owned defect by editing it.

Resource plan: every package starts from a copy of `$AGENTS/seed` (about 5 GB; copying took 6 s
and gave 0 recompiles in the delivery review), builds with `CARGO_BUILD_JOBS=4`, so seven agents
use at most 28 jobs on 16 threads and about 40 GB of disk (150 GB free). Benchmark numbers from
phase A are informational (loaded machine); WP10 re-measures on an idle machine. GPU tests from
WP3 and WP7 may overlap (they are small).

### WP0: bootstrap (one agent, sequential)

- Owns: everything (then hands shared files to the lead).
- Steps:
  1. `cp -a $FINAL/skeleton/. $REPO/` (includes `.scaffold/`, dotfiles, `Cargo.lock`).
  2. `mkdir -p $REPO/.scaffold/design && cp -a $FINAL/SCAFFOLD_SPEC.md $FINAL/ARCHITECTURE.md
     $FINAL/adr $FINAL/reference $REPO/.scaffold/design/`.
  3. `cp $FINAL/ARCHITECTURE.md $REPO/docs/architecture.md && cp $FINAL/adr/*.md $REPO/docs/adr/`.
  4. `cd $REPO && CARGO_NET_OFFLINE=true tools/refork/refork.sh` (seconds; creates `vendor/` with
     `vendor/Cargo.lock`).
  5. Verify with `CARGO_TARGET_DIR=$AGENTS/wp0/target`:
     `CARGO_BUILD_WARNINGS=deny cargo check --workspace --all-targets --all-features --locked`;
     `cargo test --workspace --locked`;
     `cargo clippy --workspace --all-targets --all-features --locked -- -D warnings`;
     `CARGO_BUILD_WARNINGS=deny cargo doc --workspace --no-deps --all-features --locked`;
     `cargo fmt --check`; `python3 tools/ecs_surface.py`; `cargo deny --all-features check`;
     `python3 tools/no_bevy.py`; `python3 tools/vendor_lock_sync.py`;
     `(cd vendor && RUST_BACKTRACE=1 CARGO_TARGET_DIR=$AGENTS/wp0/vendor-target cargo test --workspace --lib --locked)`;
     `(cd vendor && CARGO_TARGET_DIR=$AGENTS/wp0/vendor-target cargo test -p boxel_ecs --doc --features debug --locked)`.
  6. Seed: `time python3 .scaffold/bin/scaffold.py seed`. Record the wall time in
     `.scaffold/deviations/wp0.md` (compile-time baseline), then
     `touch crates/boxel_app/src/lib.rs && time CARGO_TARGET_DIR=$AGENTS/seed cargo test --workspace --all-features --locked --no-run`
     (incremental baseline). Reference from the delivery review: full `test --no-run` 1m31s wall,
     18m49s CPU; check-only 46 s.
  7. If Ezequiel approved the git baseline: `git init`, `git add -A`, one commit
     `chore: M0 skeleton` ending with the attribution trailer the session prescribes.
- Done: every command green; tree matches section 2.

### WP1: core (phase A)

- Owns: `crates/boxel_app/`, `crates/boxel_log/`, `crates/boxel_diagnostic/`,
  `crates/boxel_math/`, `crates/boxel_transform/`.
- Deliverables: the `[P]` items of sections 5.1 to 5.5 and their tests.
- Verify (in `$AGENTS/wp1/ws`, `CARGO_TARGET_DIR=$AGENTS/wp1/target`):
  `cargo test -p boxel_app -p boxel_log -p boxel_diagnostic -p boxel_math -p boxel_transform --locked`;
  `cargo clippy -p boxel_app -p boxel_log -p boxel_diagnostic -p boxel_math -p boxel_transform --all-targets --all-features --no-deps --locked -- -D warnings`;
  `CARGO_BUILD_WARNINGS=deny cargo doc -p boxel_app -p boxel_log -p boxel_diagnostic -p boxel_math -p boxel_transform --no-deps --locked`;
  `python3 .scaffold/bin/scaffold.py check wp1`.

### WP2: platform (phase A)

- Owns: `crates/boxel_window/`, `crates/boxel_camera/`.
- Deliverables: sections 5.6, 5.7, 9.5.
- Verify: `cargo test -p boxel_window -p boxel_camera --locked`; clippy and doc for both crates
  (as WP1); `cargo run -p boxel_window --example window_smoke --locked` exits 0 and prints the
  `created` line; `-- --resize-test` exits 0; `WAYLAND_DISPLAY= cargo run -p boxel_window
  --example window_smoke --locked` exits 0 (X11 path); `scaffold.py check wp2`.

### WP3: graphics layer (phase A)

- Owns: `crates/boxel_gfx/` (including `examples/gpu_info.rs`).
- Deliverables: sections 5.8, 8, 9.1.
- Verify: `cargo test -p boxel_gfx --all-features --locked`;
  `BOXEL_REQUIRE_GPU=1 cargo test -p boxel_gfx --locked --test gpu_select -- --nocapture --test-threads=1`;
  the same with `WGPU_BACKEND=gl`, with
  `__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json WGPU_BACKEND=gl`,
  with `WGPU_BACKEND=vulkan WGPU_ADAPTER_NAME=llvmpipe`, and with
  `__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json LIBGL_ALWAYS_SOFTWARE=1 WGPU_BACKEND=gl`;
  clippy and doc for `boxel_gfx` (with `--all-features`); section 12 rows 4 to 8 (with
  `cargo run -p boxel_gfx --example gpu_info --locked -- ...`); `scaffold.py check wp3`.

### WP4: voxel data and worldgen (phase A)

- Owns: `crates/boxel_voxel/`, `crates/boxel_worldgen/`.
- Deliverables: sections 5.11, 5.12 (+ benches).
- Verify: `cargo test -p boxel_voxel -p boxel_worldgen --locked`; clippy and doc;
  `cargo bench -p boxel_voxel -p boxel_worldgen --locked -- --quick` (record numbers in your
  deviations file under "measurements"); `scaffold.py check wp4`.

### WP5: meshing (phase A)

- Owns: `crates/boxel_mesh/`.
- Inputs: `boxel_voxel` `[S]` parts (`default_blocks`, `ChunkData::from_dense`/`set` baselines);
  the conversion in `reference/renderprobe/src/main.rs`.
- Deliverables: section 5.13 (+ bench).
- Verify: `cargo test -p boxel_mesh --locked` (oracle proptests with at least 256 cases);
  clippy and doc; `cargo bench -p boxel_mesh --locked -- --quick`; `scaffold.py check wp5`.

### WP6: world (phase A)

- Owns: `crates/boxel_world/`.
- Deliverables: section 5.14 (integration-ignored tests in `tests/integration.rs`).
- Verify: `cargo test -p boxel_world --locked`; clippy and doc; `scaffold.py check wp6`.

### WP7: shaders and renderer core (phase A)

- Owns: `crates/boxel_shaders/`, `crates/boxel_render/` except `src/plugin.rs`.
- Inputs: sections 6 and 7.1 to 7.5; `boxel_gfx` `[S]` items (`tier_request`, `AdapterCaps`,
  `GlVersion`, `run_in_error_scopes`, `submit_and_wait`, `read_texture_rgba8`, `RgbaImage`);
  `boxel_voxel::BlockRegistry::default_blocks`; `boxel_mesh` `[S]` types; the reference probe.
- Deliverables: sections 5.9, 5.10 (WP7 part), 7.1 to 7.5, 9.4.
- Verify: `cargo test -p boxel_shaders -p boxel_render --locked`;
  `BOXEL_REQUIRE_GPU=1 cargo test -p boxel_render --locked --test gpu_tiers -- --nocapture --test-threads=1`
  with the default environment (RADV, RTX 5050, lavapipe on Vulkan), with `WGPU_BACKEND=gl`
  (NVIDIA GL 3.3: Low), with the Mesa EGL vendor (radeonsi: Mid and Low) and with Mesa EGL +
  `LIBGL_ALWAYS_SOFTWARE=1` (llvmpipe GL); clippy and doc for both crates;
  `scaffold.py check wp7`.
- Done: tier images match on every adapter, the far scene passes everywhere, the Draw-level
  `RendererSmokeTest` passes for every tier the local check allows.

### WP8: app path, GPU and window side (phase B)

- Owns (phase B rows of `owners.tsv`): `crates/boxel_render/` (except `tests/gpu_terrain.rs`),
  `crates/boxel_gfx/`, `crates/boxel_shaders/`, `crates/boxel_window/`, `crates/boxel_camera/`,
  `examples/voxel_world.rs`. May fix phase-A defects inside these paths.
- Deliverables: section 7.6 (`src/plugin.rs` and its tests), section 9.2.
- Verify: `cargo test -p boxel_render --locked`; clippy for `boxel_render` and
  `cargo clippy -p boxel --example voxel_world --all-features --no-deps --locked -- -D warnings`;
  section 12 rows 9 to 16, 18, 19, 20 (use `--timeout`); `scaffold.py check wp8`.
- Cut list item 1 is this package's call (record it).

### WP9: integration, CPU side (phase B)

- Owns (phase B rows): `crates/boxel_voxel/`, `crates/boxel_worldgen/`, `crates/boxel_mesh/`,
  `crates/boxel_world/`, `crates/boxel_render/tests/gpu_terrain.rs`, `examples/headless_world.rs`,
  `tests/`, `README.md`. May fix phase-A defects inside these paths.
- Deliverables: section 9.3; un-ignore and fix `crates/boxel_world/tests/integration.rs`;
  section 9.6 (`gpu_terrain.rs`, `tests/ambiguity.rs`); `README.md` in Spanish, short and simple
  (what boxel is, requirements, `just` commands, how to run the examples, GPU overrides table,
  link to `docs/`).
- Verify: `cargo test -p boxel_voxel -p boxel_worldgen -p boxel_mesh -p boxel_world --locked`;
  `cargo test --test ambiguity --locked`;
  `BOXEL_REQUIRE_GPU=1 cargo test -p boxel_render --locked --test gpu_terrain -- --test-threads=1`
  (default env, `WGPU_BACKEND=gl`, Mesa EGL); `cargo run --release --example headless_world
  --locked` (row 17); clippy and doc for the owned crates and the example;
  `scaffold.py check wp9`.

### WP10: verification and cleanup (phase C, one agent, works in `$REPO`)

- Owns: everything. Target dir: `cp -a $AGENTS/seed $AGENTS/wp10/target`.
- Steps: resolve every open deviation (or move it to the final report as an open risk); remove
  every `#[ignore = "integration..."]` that is left; run section 12 completely; save screenshots
  to `shots/` (git-ignored); write `docs/informe-m0.md` in Spanish, short (what works, decision
  lines, image diffs, measurements, cuts, open risks, the manual checklist for Ezequiel); delete
  `.scaffold/` last.
- Verify: section 12, all rows.
- Done: M0 definition of done (section 1).

---

## 12. Verification plan on this machine

Machine facts (measured): Vulkan lists RADV 780M (Integrated), RTX 5050 (Discrete), lavapipe
(Cpu). Headless GL lists exactly one adapter, the glvnd default EGL vendor: NVIDIA, context
**3.3**. With a window on Wayland (or XWayland), GL uses the compositor GPU: **radeonsi 4.6**.
`__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json` gives radeonsi 4.6
headless; adding `LIBGL_ALWAYS_SOFTWARE=1` gives llvmpipe 4.5. `WGPU_POWER_PREF`/`--power`
changes only the ranking. A cold dGPU adds ~1 s to instance creation.

Use `T=$AGENTS/wp10/target` (or the running package's target). Every windowed run uses
`--no-lkg` and `--timeout`, except row 14, which isolates the config dir. `GI="cargo run --locked
-p boxel_gfx --example gpu_info --"`, `VW="cargo run --release --locked --example voxel_world --"`.

| # | Command | Expected |
|---|---|---|
| 1 | `CARGO_TARGET_DIR=$T just ci` | green |
| 2 | `CARGO_TARGET_DIR=$T just test-gpu` | `gpu_contract`, `gpu_select`, `gpu_tiers`, `gpu_terrain` pass on all adapters |
| 3 | `just test-lavapipe`, `just test-llvmpipe`, and `__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json WGPU_BACKEND=gl BOXEL_REQUIRE_GPU=1 cargo test --workspace --locked --test '*gpu*' -- --test-threads=1` | pass (software adapters as in CI; radeonsi GL Mid/Low) |
| 4 | `$GI --all-tiers --smoke draw` | RADV max High, NVIDIA Vulkan max High, lavapipe max Mid (software cap), NVIDIA GL max Low with reason "GL 3.3 < 4.3"; matrix all OK; `DECISION: backend=Vulkan tier=High class=Discrete adapter="NVIDIA GeForce RTX 5050 Laptop GPU"` |
| 5 | `$GI --power low` | `DECISION: backend=Vulkan tier=High class=Integrated adapter="AMD Radeon 780M Graphics (RADV PHOENIX)"` |
| 6 | `WGPU_BACKEND=gl $GI` | one adapter (NVIDIA GL 3.3); `DECISION: backend=Gl tier=Low ...`; smoke level Draw (Auto on GL); no panic, no "Parent device is lost" |
| 7 | `__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json WGPU_BACKEND=gl $GI` | radeonsi 4.6, `tier=Mid` |
| 8 | `$GI --software` | `adapter="llvmpipe ..." backend=Vulkan tier=Mid` |
| 9 | `$VW --no-lkg --timeout 120 --frames 120 --wait-chunks --screenshot shots/vk_high.png --expect-terrain` | stdout `boxel: backend=Vulkan tier=High class=Discrete adapter="NVIDIA GeForce RTX 5050 Laptop GPU" ...`; exit 0; PNG with terrain (leaves visible as cutout) |
| 10 | row 9 with `--tier mid --screenshot shots/vk_mid.png --compare-to shots/vk_high.png`, then `--tier low --screenshot shots/vk_low.png --compare-to shots/vk_high.png` | `tier=Mid` / `tier=Low`; exit 0 (diff <= 0.5 %) |
| 11 | `WGPU_BACKEND=gl $VW --no-lkg --timeout 120 --frames 120 --wait-chunks --screenshot shots/gl.png --expect-terrain` | `backend=Gl tier=Mid` (radeonsi through the Wayland display); exit 0 |
| 12 | row 11 with `--tier low --screenshot shots/gl_low.png --compare-to shots/gl.png` | `backend=Gl tier=Low`; exit 0 |
| 13 | `$VW --no-lkg --timeout 120 --frames 300 --simulate-device-loss 60` | log shows device lost, re-selection with a new surface, `boxel: backend=Vulkan tier=High ...` printed twice, `recoveries=1`, exit 0 (if cut list 1 applied: `GpuFailed` and exit 2) |
| 14 | `XDG_CONFIG_HOME=$AGENTS/wp10/xdg $VW --timeout 60 --frames 30` twice, then `XDG_CONFIG_HOME=$AGENTS/wp10/xdg $GI --lkg voxel_world` | release build so LKG is on; the first run writes the NVIDIA High entry `good`; the second run's report shows the +50 bonus; `--lkg` lists it; Ezequiel's real `~/.config` is untouched |
| 15 | `WAYLAND_DISPLAY= $VW --no-lkg --timeout 60 --frames 60` | X11 path (XWayland), Vulkan High, exit 0 |
| 16 | `$VW --no-lkg --timeout 60 --power low --frames 60` | `class=Integrated` (RADV High) |
| 17 | `cargo run --release --locked --example headless_world` | settles in a few seconds; non-zero `meshed` and `quads` |
| 18 | `__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/10_nvidia.json WGPU_BACKEND=gl $VW --no-lkg --timeout 60 --frames 10` | robustness only: no crash; either NVIDIA GL works as Low or `Instance::new` panics inside wgpu-hal, the panic is caught silently (note in the report) and the app exits 2 with a clear message |
| 19 | `$VW --no-lkg --timeout 60 --minimize-test`, then `WGPU_BACKEND=gl $VW --no-lkg --timeout 60 --minimize-test` | exit 0 on both: `boxel: minimized updates=U presents=P fixed_steps=S` with `U >= 60`, `S >= 60` (the simulation never froze; on GL a naive FIFO present would block) |
| 20 | `$VW --no-lkg --timeout 60 --resize-test --frames 120` | exit 0 |
| 21 | `cargo run --locked -p boxel_window --example window_smoke` and `-- --resize-test` | exit 0 |
| 22 | informational: `$VW --no-lkg --timeout 120 --frames 600 --async-threads 4` and `--async-threads 8` | report the `fps`/`worst` log lines while streaming (task-pool sizing check) |

Manual checklist for Ezequiel (WP10 hands it over; agents cannot do these): fly with mouse and
WASD, Escape releases the cursor; resize by dragging; move the window to another workspace for
10 s and back: the `fps=` log lines keep coming and rendering resumes.

If a row fails because of a wgpu or driver behaviour not covered here, record it in the
package's deviations file with the full log and the `gpu_info` output.

---

## Appendix A: API facts that bite (verified against local registry sources or probes)

- wgpu 30: `Instance::new(desc)` takes the descriptor by value; `enumerate_adapters` is async;
  `request_device(&DeviceDescriptor)` (no trace path argument); `push_error_scope` returns a
  guard (`guard.pop()` is async) and error scopes are thread-local (`ErrorScopeGuard` is
  `!Send`); `Device::on_uncaptured_error(Arc<dyn Fn(Error) + Send + Sync>)`;
  `set_device_lost_callback(Fn(DeviceLostReason, String))`; `Queue::present(frame)` (not
  `frame.present()`); `get_current_texture()` returns `CurrentSurfaceTexture`;
  `BufferSlice::get_mapped_range()` returns a `Result`; `PollType::wait_indefinitely()`;
  `PipelineLayoutDescriptor { bind_group_layouts: &[Option<&BindGroupLayout>], immediate_size }`;
  `VertexState::buffers: &[Option<VertexBufferLayout>]`; `DepthStencilState {
  depth_write_enabled: Option<bool>, depth_compare: Option<CompareFunction>, .. }`;
  `RenderPipelineDescriptor { multiview_mask: None, cache: None, .. }`; `RenderPassDescriptor {
  multiview_mask: None, .. }`; `RenderPassColorAttachment { depth_slice: None, .. }`;
  `TexelCopyTextureInfo`/`TexelCopyBufferInfo`/`TexelCopyBufferLayout`;
  `Limits::max_*_buffer_binding_size` and `max_buffer_size` are `u64`;
  `Limits::or_worse_values_from` and `check_limits` exist; `Limits` has no
  `maxDrawIndirectCount` and wgpu-hal does not check it on the `*_count` path;
  `RequestAdapterOptions` gained `apply_limit_buckets`; `wgpu::wgt::WgpuHasDisplayHandle`;
  `wgpu::hal`, `wgpu::naga`, `wgpu::rwh` re-exports; `InstanceFlags::from_build_config()` returns
  `VALIDATION_INDIRECT_CALL` in release despite its docs; `InstanceDescriptor::with_env()`
  replaces backends with `WGPU_BACKEND` even when it parses to empty; `Dx12Compiler::Auto` =
  static DXC if compiled in, else `dxcompiler.dll` on PATH, else FXC; the GL backend always
  writes GLSL with `ADJUST_COORDINATE_SPACE` (z remapped to [-1, 1], no clip control);
  `wgpu::hal::vulkan` exists only on Windows/Linux/Android/FreeBSD (Apple needs
  `vulkan-portability`).
- WGSL in naga 30: `var<immediate>` (not `var<push_constant>`); no `enable subgroups;`;
  `enable wgpu_binding_array;` needed for binding arrays; HLSL lowers `vertex_index` to
  `first_vertex + SV_VertexID`.
- glam 0.34.0: no `Mat4::perspective_*`/`look_at_*`/`look_to_*`; use
  `glam::camera::rh::proj::directx::*` and `glam::camera::rh::view::*`.
- winit 0.30.13: `ApplicationHandler::{resumed, window_event, device_event, about_to_wait,
  exiting}`; `EventLoop::owned_display_handle()` is `Clone + Send + Sync`; one `EventLoop` per
  process, created on the main thread; `WindowEvent::Occluded` is never sent on Wayland or
  Windows; on Wayland `pre_present_notify()` makes winit hold `RedrawRequested` until the frame
  callback; `set_minimized(true)` works on Wayland (un-minimize does not); on macOS every window
  method off the main thread is a synchronous dispatch to the main queue and `window_handle()`
  fails off the main thread; on Windows `set_cursor_grab`/`set_cursor_visible` wait on the
  event-loop thread.
- boxel_ecs (bevy_ecs 0.20.0-rc.2 renamed): buffered events are `Message`s
  (`MessageReader`/`MessageWriter`/`Messages`), `World::write_message`,
  `MessageRegistry::register_message`, `message_update_system` + `MessageUpdateSystems` +
  `message_update_condition`, `signal_message_update_system`, `ShouldUpdateMessages::{Always,
  Waiting, Ready}`, `World::{insert_non_send, init_non_send, remove_non_send}`, `NonSend`/
  `NonSendMut` in the prelude, `IntoScheduleConfigs`, `ScheduleSystem`, `InternedScheduleLabel`,
  `SingleThreadedExecutor`, `Schedule::{set_executor, set_build_settings}` (no executor-kind
  getter), `ScheduleBuildSettings { ambiguity_detection: LogLevel, .. }`,
  `World::try_run_schedule`, `World::get_resource_or_init`. Resources are entities (they show up
  in entity counts). Derives resolve `::boxel_ecs` (direct dependency) or `::boxel::ecs` (facade
  users). `ParamSet` needs explicit `&'static`.
- boxel_tasks: `AsyncComputeTaskPool::get()` panics if uninitialized; use
  `get_or_init(TaskPool::default)`; worker threads swallow task panics and restart (a detached
  task that panics simply disappears).
- binary-greedy-meshing 0.5.2: `Mesher::<32>::new()`, `fast_mesh(&voxels, &opaque_mask,
  &trans_mask)`, output in `mesher.quads[face]` (faces Up, Down, Right, Left, Front, Back =
  +Y, -Y, +X, -X, +Z, -Z), `Face::vertices_packed(quad)` gives 4 vertices with 6-bit xyz,
  positive faces lie on the `voxel + 1` plane; voxel layout `z + 1 + (x + 1) * 34 + (y + 1) *
  34^2`; masks indexed `x + 1 + (y + 1) * 34` with bit `z + 1`; transparent voxels hide faces
  toward the same id.
- offset-allocator 0.2.0: `Allocator::with_max_allocs(size, max_allocs)`, `allocate(size) ->
  Option<Allocation>` (`allocation.offset`), `free(allocation)`, `storage_report()`.
- png 0.18.1: `Decoder::new(BufRead + Seek)`, `set_transformations(
  Transformations::normalize_to_color8())`, `read_info()`, `output_buffer_size() ->
  Option<usize>`, `next_frame(&mut buf) -> OutputInfo` (`buffer_size()`, `color_type`).

## Appendix B: why the skeleton looks the way it does (short)

- `vendor/` as a nested workspace: keeps Bevy's lints and lockfile, keeps upstream tests
  runnable, and keeps `[bans.workspace-dependencies]` meaningful for our crates.
- `GpuContext` fields are public and `VoxelRenderer::new` takes device parts, so renderer tests
  and the smoke test build renderers without `select_gpu`.
- `SmokeTest` is a trait so selection proves the renderer's *real* pipelines, for the exact
  surface format.
- `VoxelRenderer` is ECS-free so the cross-tier image test does not need a window.
- `ChunkMap::ready_meshes()` keeps meshes CPU-side so device loss and tier changes are a reload.
- Cross-package helpers (`Frustum`, `Time`, `CameraView::compute`, `tier_request`,
  `GlVersion::parse`, `AdapterCaps::from_adapter`, `default_blocks`, `ChunkData` baselines,
  the naga tests) are `[S]` so no package depends on another package's unfinished work.
- `.scaffold/` gives every package a private copy and an ownership gate, so parallel agents never
  see each other's half-edited files.


## Appendix C: Review log (v1 -> final)

Three adversarial reviews of v1: **G** = graphics correctness, **E** = ECS and Rust architecture,
**D** = delivery (parallel agents). `M` marks items from a review's "missing" list. Every item is
fixed (F), partly fixed with the rest rejected (P), rejected with a reason (R), or a duplicate.
"Verified" means checked against registry sources or a compiled probe on 2026-10-05.

| ID | Sev | Issue | Resolution |
|---|---|---|---|
| G1 | major | Retry after `VoxelRenderer::new` failure reconfigures a configured surface with a new device (measured Wayland segfault); smoke test never built the real blit entry point | F. `select_gpu` picks the surface format per adapter (`choose_surface_format`), passes it as `SmokeContext::output_format`, returns it as `GpuContext::output_format`; renderer built before `configure_surface`; any failure excludes the pair and the next frame re-selects with a NEW surface; invariant in 5.8, 7.6, ADR 0002 |
| G2 | major | `Occluded` is never sent on Wayland/Windows, so FIFO present can block the main thread on a hidden window | F (verified + measured). Presentation gate `WindowState::frame_ready` set on `RedrawRequested`, `pre_present_notify` before present, runner requests the next redraw; 100 ms redraw watchdog marks `occluded` (30 Hz pacing). Probe: naive FIFO froze GL (0 updates in 2 s) and throttled Vulkan to ~105/s; gated kept running on all backends. Automated check `--minimize-test` (row 19). Sub-point "prefer Mailbox on Vulkan" R: the gate already keeps FIFO non-blocking and Mailbox would render unbounded fps on laptops; it stays opt-in |
| G3 | major | LKG writes `Bad` for non-driver events (Rust panics, closing early, transient `request_device` errors, debug env); key ignores rebuilds; on in debug | F. Crash-only LKG: in-process failures never write; `Pending` restored on clean exit (`Shutdown`) and by a panic hook; pid + 30 s grace for concurrent instances; 2 strikes (`Suspect` then `Bad`); read-only under debug instance flags; build fingerprint (exe mtime) in the key; off by default in debug builds; `gpu_info --lkg/--clear-lkg` |
| G4 | major | `boxel_gfx::hal` does not compile on macOS with `--all-features` | F (verified in wgpu build.rs and wgpu-core manifest). Module cfg-gated to Windows/Linux/Android/FreeBSD |
| G5 | minor | `tier_request` base limits stricter than classify thresholds; missing workgroup checks; Low texture layers 256 | F. `Limits::default().or_worse_values_from(adapter)` + engine-sized limits from the adapter; shared `floors` used by classify; workgroup size x, invocations, workgroups per dimension checked; layers from the adapter; contract tests at the exact floors and on every real adapter (all accepted) |
| G6 | minor | wgpu GL always remaps z to [-1, 1]: reverse-Z precision gain lost on GL tiers | F (verified). Documented (ARCHITECTURE 10.1, ADR 0007, spec 4); near >= 0.1; far-scene test in `gpu_tiers` (1 m at 480 m); GL clip control tracked for M2 |
| G7 | minor | Cutout drawn in the opaque pipeline with `discard` (early-Z loss everywhere) and back-face culled (hollow canopies) | F. New ADR 0012: 8 buckets (cutout bucket 6, translucent 7), `ChunkGpu` 64 B with `flags`, opaque pipeline without `discard`, cutout pipeline with alpha test and `cull_mode: None`, second indirect list on High. Interior faces between adjacent leaves deferred to M1 (layout unchanged) |
| G8 | minor | naga tests only cover GLSL | F. `tests/naga.rs` is now `[S]` and covers MSL 2.1/2.4, HLSL SM5.1/6.0 (asserting the `first_vertex` lowering) and SPIR-V; 7/7 pass |
| G9 | minor | No `static-dxc`: DX12 falls back to FXC | P. Documented (ADR 0001, Appendix A); Windows/macOS CI run `gpu_info --all-tiers` (informational) to measure smoke-test time. Enabling `static-dxc` R for M0: it pulls a prebuilt DXC through `mach-dxcompiler-rs` that cannot be verified without Windows; M1 decides between it and shipping `dxcompiler.dll` |
| G10 | minor | MDIC `max_count` up to 196608 is not checked against `maxDrawIndirectCount` (min 65535) | F (verified in wgpu-hal). `MAX_DRAWS_PER_LIST = 65535` caps each list and each call; warning when live chunks could exceed it; list segmentation in M3 |
| G11 | minor | Default Compile-level smoke cannot catch silent GL miscompiles; no health check after smoke; pass errors surface at `finish` | F. `SmokeLevel::Auto` (default) = Draw on GL and software adapters; post-smoke `is_lost`/`error_count` check; `submit_and_wait` inside the scopes at every level |
| G12 | minor | `Recreate` order unspecified; Draw ignores device loss | F. `FrameAcquire::Recreate` rule (drop old surface first, same device, never while lost); Draw returns early on `is_lost()` |
| G13 | minor | Surface format fallback could pick float/10-bit; alpha mode could be translucent | F. `choose_surface_format` (8-bit sRGB first, 8-bit unorm second, never float/10-bit), `choose_alpha_mode` (Opaque else Auto), blit writes alpha 1; contract tests |
| G14 | minor | Draw-level renderer smoke test cannot build a `GpuContext` | F. `VoxelRenderer::new(device, queue, tier, ..)` reads limits from `device.limits()` |
| G-M1 | missing | No probe of hidden-window behaviour | F. `reference/hidden_window_probe.rs`, results in 5.6 and ADR 0008 |
| G-M2 | missing | Device-loss recovery onto a different backend never exercised | R for M0: no way to lose a device on one backend and recover on another on demand; listed as an open risk (ARCHITECTURE 16) |
| G-M3 | missing | Hi-Z notes for reverse-Z | F. ARCHITECTURE 10.3 (MIN reduction, far = 0, Depth32Float read as depth texture into an R32Float chain, High only) |
| G-M4 | missing | ArenaFull leaves silent holes | F. `RenderStats::dropped_chunks` / `RenderInfo::dropped_chunks`, rate-limited warning, `"render"` board line; growth in M1 |
| G-M5 | missing | `catch_unwind` useless with `panic = "abort"`; handled EGL panic prints a backtrace | F. `gl_deferred` + `create_gl_instance` two-stage path under `cfg(panic = "abort")`; silent panic hook during `Instance::new`; LKG panic hook still runs before abort |
| G-M6 | missing | Error scopes are thread-local | F. Stated in `run_in_error_scopes`/`SmokeTest` docs, spec 4, ARCHITECTURE 11 (constraint on M3 async init) |
| G-M7 | missing | Low tier on GLES 3.0 without vertex attrib binding never run | R for M0: no such driver on the dev machine or CI; open risk |
| E1 | major | `PrimaryWindow` as a Send resource invites window calls from worker threads (deadlock on macOS/Windows) | F. Non-send resource (`App::insert_non_send`, `NonSend<PrimaryWindow>`); rationale in its rustdoc; surface work stays on the main thread |
| E2 | major | Messages written in `Update` are lost by `FixedUpdate` readers at high frame rates | F (measured). `TimePlugin` adds `signal_message_update_system` + `Waiting`; `[S]` contract test (30 frames, none lost) |
| E3 | major | `hal` on macOS | Duplicate of G4 |
| E4 | major | ECS diagnostics show placeholder names (no `debug` feature) | F. Facade feature `debug` (default) and `trace` implies it; engine crates enable it in dev-dependencies only |
| E5 | major | A panicking or cancelled chunk job leaks an in-flight slot; `AsyncComputeTaskPool::get()` panics without the plugin | F. Exactly-one-result contract (`Generated`/`Meshed`/`Cancelled`/`Failed`), `catch_unwind` + drop token, `get_or_init(TaskPool::default)`; tests required |
| E6 | minor | `Res<Time>` in `FixedUpdate` is the frame delta | F. Fixed view swapped in during `FixedMain`; `[S]` contract test |
| E7 | minor | `should_exit` returns the last message | F. First error, else Success; `[S]` contract test |
| E8 | minor | Plugins added during `finish` are dropped from the registry | F. Index loop with a placeholder; `[S]` contract test |
| E9 | minor | The `::boxel::ecs` derive path is never exercised | F. `crates/boxel_facade_check` (depends only on `boxel`), every derive + `'static` ParamSet; passes |
| E10 | minor | refork.sh fragile (registry path, unasserted sed, library diff not enforced, already-upstream backports) | F. `cargo metadata` source lookup; sed assertions; `check_test_only.py` proves patch_tests only touched test/doc lines; reverse-apply check names stale backports; verified end to end |
| E11 | minor | `vendor/Cargo.lock` never created; locks drift | F. Lock kept across reforks (seeded from `template/Cargo.lock`); vendor tests pass with `--locked`; `tools/vendor_lock_sync.py` in `just deny` and CI |
| E12 | minor | Miri only weekly, warnings denied on nightly, `boxel_tasks` missing | F. Separate `miri.yml` (weekly + PRs touching `vendor/**`/`tools/refork/**`), pinned `nightly-2026-09-30`, `CARGO_BUILD_WARNINGS=warn`, `-p boxel_tasks` |
| E13 | minor | Surface check bypassable; exit-hatch claim overstated | F. `tools/ecs_surface.py` parses grouped/multi-line imports and `boxel::ecs::` paths (tested on the bypass cases); ADR 0003 and ARCHITECTURE 6.1 reworded (hatch covers engine code only) |
| E14 | minor | Facade `png` feature does nothing | F. `boxel_gfx` workspace dep without default features; `png` reaches `boxel_gfx` and `boxel_render` |
| E15 | minor | deny.toml bans only 5 bevy names | F. More exact names + `tools/no_bevy.py` (any `bevy*` in root and vendor graphs) in `just deny` and CI |
| E16 | minor | `Render` schedule creation could drop systems or stay multi-threaded; docs disagree | F. `edit_schedule` + `set_executor`; behavioural test (fork has no executor-kind getter); ARCHITECTURE 6.2 fixed |
| E17 | minor | Extract boundary not enforced | F. Private `ExtractedFrame`; `Prepare`/`Draw` read only it, GPU state and the non-send window (needed for `pre_present_notify`) |
| E18 | minor | Roadmap puts the 0.20 final re-fork in M4 | F. Triggered task, at the latest an M1 entry criterion; M4 keeps only 0.21 / own-core evaluation |
| E19 | minor | Platform shim comment mangled/stale; no per-file modification notice | F. Template text avoids the sed, version substituted; refork adds a one-line notice to every modified `.rs` file (171) |
| E20 | minor | `panic = "abort"` | Duplicate of G-M5 |
| E21 | minor | Teardown order at exit is arbitrary | F. `Shutdown` schedule run by runners (`App::shutdown`); renderer drops renderer, surface, device; runner then removes the window, all in `exiting` |
| E22 | minor | Task pools oversubscribe the CPU | F. Main thread counted, async compute 40 % capped at 8 (16 threads -> 2/6/7); `[S]` test; informational measurement row 22 (`--async-threads`) |
| E-M1 | missing | No schedule ambiguity test | F. `tests/ambiguity.rs` (WP9, 9.6) |
| E-M2 | missing | Additive App APIs | P. `insert_non_send`/`init_non_send` added. `add_observer`/`set_error_handler` R: no M0 consumer; `app.world_mut().add_observer` works |
| E-M3 | missing | No compile-time baseline | F. WP0 step 6 records full and incremental times |
| E-M4 | missing | No written job result contract | F. `JobExecutor` rustdoc + 5.14 |
| E-M5 | missing | Resources are entities | F. Facade docs + Appendix A |
| E-M6 | missing | Quote the E0477 text | F. Rule 12, facade docs |
| E-M7 | missing | Verified-OK list | No action needed (kept as evidence) |
| D1 | major | One shared working dir: half-edited upstream crates break everyone | F. Private copies per package (`scaffold.py start`), gate copies owned paths back |
| D2 | major | Shared deviations file races and breaks ownership | F. `.scaffold/deviations/<wp>.md` per package |
| D3 | major | Cross-package `[P]` items create ordering traps | F. Moved to `[S]` and implemented + tested in the skeleton: Aabb/Frustum, Time/FixedTime/TimePlugin, task pools, `CameraView::compute`, `tier_request`, `GlVersion`, `AdapterCaps::from_adapter`, `default_blocks`, `ChunkData` baselines, naga tests |
| D4 | major | Riskiest code (plugin, voxel_world) written blind; WP9 does all debugging | F. Phase B: WP8 (plugin + voxel_world, runs rows 9-20) and WP9 (CPU side, integration tests, README); WP10 verifies |
| D5 | major | refork deletes/never creates `vendor/Cargo.lock` | Duplicate of E11 |
| D6 | major | `typos` not installed, `just ci` stops | F. Preflight asks to install; recipe fails with a hint; `BOXEL_SKIP_TYPOS=1` explicit skip |
| D7 | major | Agent inputs on the tmpfs; dangling doc references; wrong section reference | F. WP0 copies the design (incl. reference probes) to `.scaffold/design/`; ARCHITECTURE written for `docs/` (no scratchpad paths); section references fixed |
| D8 | major | No VCS: no rollback or ownership check | P. Ownership enforced by `scaffold.py check/gate` without git; local git baseline only with Ezequiel's approval (preflight); rollback is the package's own copy |
| D9 | major | `hal` on macOS | Duplicate of G4 |
| D10 | major | No resource plan | F. Seed target dir, `CARGO_BUILD_JOBS=4`, disk budget, benches informational (11.2) |
| D11 | minor | `CARGO_TARGET_TMPDIR` in unit tests does not compile | F. LKG tests in `tests/lkg.rs`; rule in spec 4 |
| D12 | minor | `cargo fmt` scope rewrites others' files | F. Rule 10: `cargo fmt -p <owned>`, `rustfmt` for owned examples |
| D13 | minor | `gpu_info` as root example compiles everything | F. `crates/boxel_gfx/examples/gpu_info.rs`; `just gpu-info` and CI updated |
| D14 | minor | WP2 verification needs a human | F. `window_smoke` example (agent-verifiable, resize test); manual checklist separated (row list end) |
| D15 | minor | Automated runs hang on a hidden window; no timeout | F. `render_when_hidden` + `frames_rendered`; `--timeout` (exit 4) |
| D16 | minor | No image-diff tool | F. `RgbaImage::load_png` (`[S]`, round-trip test) + `voxel_world --compare-to/--max-diff` |
| D17 | minor | GPU contract test ignores `WGPU_ADAPTER_NAME` | F (verified: picks llvmpipe on Vulkan and GL) |
| D18 | minor | `just doc` fails on a broken link | F. Link fixed; skeleton `cargo doc` clean with warnings denied; doc check in every package's verify |
| D19 | minor | Row 14 writes Ezequiel's real config | F. `XDG_CONFIG_HOME` isolation |
| D20 | minor | Packages may use upstream's new pub items mid-phase | F. Rule 3 |
| D21 | minor | `boxel_render` lacks `boxel_worldgen` dev-dependency | F. Added (used by `gpu_terrain.rs`) |
| D22 | minor | 9.4 suggests a local readback | F. Uses `boxel_gfx::read_texture_rgba8` |
| D23 | minor | Surface check misses brace imports | Duplicate of E13 |
| D24 | minor | Parallel EGL init in GPU test binaries | F. `--test-threads=1` in recipes and CI |
| D25 | minor | No M0 cut list | F. Section 1 cut list (device-loss re-selection, edits system, bench targets, non-Linux CI) |
| D-M1 | missing | Concrete package plan | F. Section 11 (WP0, phase A WP1-7, phase B WP8-9, WP10) |
| D-M2 | missing | Resource and concurrency plan | F. 11.2 |
| D-M3 | missing | Phase gates | F. 11.2 gate script |
| D-M4 | missing | Ownership-violation check | F. `scaffold.py check/gate` (tested) |
| D-M5 | missing | Agent-verifiable vs human checks | F. Section 12 rows vs manual checklist |
| D-M6 | missing | Global timeout and automation mode | F. `--timeout`, `--frames` with `render_when_hidden` |
| D-M7 | missing | Standard image-diff command | F. `--compare-to` |
| D-M8 | missing | Skeleton-items-only rule | F. Rule 3 |
| D-M9 | missing | Escalation path for wrong frozen items | F. `BLOCKING:` entries, lead hot-patch + `scaffold.py sync` (11.2) |
| D-M10 | missing | Windows pop up on Ezequiel's desktop | F. Rule 13, preflight message |
