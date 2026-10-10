# boxel task runner. Requires just >= 1.56.
set shell := ["bash", "-euo", "pipefail", "-c"]
set windows-shell := ["pwsh", "-NoLogo", "-Command"]

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
[doc('Dependency policy: advisories, licenses, bans, sources, no bevy* crates, no content in engine crates, lock sync')]
deny:
    cargo deny --all-features check
    python3 tools/no_bevy.py
    python3 tools/no_content.py
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

# The version is read inside the recipe: a top-level backtick would run on every `just` call,
# also under pwsh on Windows, where grep and cut do not exist.
[group('quality')]
[doc('Compile with the declared MSRV toolchain (as the CI MSRV job)')]
[env('CARGO_BUILD_WARNINGS', 'deny')]
msrv:
    #!/usr/bin/env bash
    set -euo pipefail
    msrv=$(grep -m1 '^rust-version = ' Cargo.toml | cut -d'"' -f2)
    rustup toolchain install "$msrv" --profile minimal --no-self-update
    cargo +"$msrv" check --workspace --all-targets --all-features --locked

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
[env('CARGO_BUILD_WARNINGS', 'warn')]
test-vendor:
    cd vendor && cargo test --workspace --lib --locked
    cd vendor && cargo test -p boxel_ecs --doc --features debug --locked

[group('test')]
[doc('Window tests: frame limit precision with a real window (needs a display, ~5 s)')]
[env('BOXEL_WINDOW_TESTS', '1')]
test-window:
    cargo test --release -p boxel_window --locked --test frame_limit_window

# Flags as in Bevy's CI: the executor reads the clock (isolation off) and some tests do not join
# their threads (leaks ignored). Miri reports 1 CPU by default, which makes the task pool serial
# and fails the contiguous par_iter scheduling tests; 4 emulated CPUs keep them meaningful.
# Tests still run one at a time: with parallel test threads Miri reports a data race inside
# bumpalo 3.20.3 (zero-sized allocations on a fresh Bump write its shared static empty chunk),
# an upstream bug recorded in docs/informe-m0.md.
[group('test')]
[doc('Miri on the vendored ECS (needs: rustup toolchain install nightly-2026-09-30 --component miri)')]
[env('MIRIFLAGS', '-Zmiri-disable-isolation -Zmiri-ignore-leaks -Zmiri-num-cpus=4')]
[env('CARGO_BUILD_WARNINGS', 'warn')]
[env('RUSTFLAGS', '-Zrandomize-layout')]
miri:
    cd vendor && cargo +nightly-2026-09-30 miri test --locked -p boxel_ecs -p boxel_ptr -p boxel_tasks --lib -- --test-threads=1

[group('test')]
[doc('Benchmarks (criterion). Filter: just bench mesh')]
bench *args:
    cargo bench --workspace --bench '*' {{ args }}

[group('test')]
[doc('Tests of the Python tools (tools/bench_report.py, tools/perf, no_content, snapshot_set)')]
test-tools:
    python3 tools/bench_report_test.py
    python3 tools/perf/perf_test.py
    python3 tools/no_content_test.py
    python3 tools/snapshot_set_test.py

# Stress runs (examples/stress.rs, docs/benchmarks.md). Each one opens a window that closes by
# itself and writes bench-results/<timestamp>-<scenario>.{md,csv} (git-ignored). Extra options
# go to the example: just bench-fly --radius 32 --speed 64 --backend vulkan --tier high
[group('bench')]
[doc('Stress run, fixed camera on the settled world: just bench-static --radius 32')]
bench-static *args: (_stress "static" args)

[group('bench')]
[doc('Stress run, camera flying straight (default 32 voxels/s): just bench-fly --speed 64')]
bench-fly *args: (_stress "fly" args)

[group('bench')]
[doc('Stress run, camera turning 360 degrees in place: just bench-spin --radius 16')]
bench-spin *args: (_stress "spin" args)

[group('bench')]
[doc('Stress run, camera going straight down: just bench-dive --world overworld --speed 64')]
bench-dive *args: (_stress "dive" args)

[group('bench')]
[doc('Stress run along a route through a cave: just bench-path --world overworld --cave --speed 8')]
bench-path *args: (_stress "path" args)

[group('bench')]
[doc('static, fly at 32 and 64 voxels/s, and spin, with the same options')]
bench-all *args: (_stress "static" args) (_stress "fly" "--speed 32" args) (_stress "fly" "--speed 64" args) (_stress "spin" args)

[group('bench')]
[doc('Tables from stress summaries: just bench-report bench-results/ (or --compare BASE NEW)')]
bench-report *args:
    python3 tools/bench_report.py {{ args }}

# How to measure (docs/benchmarks.md): per task only perf-smoke, of the cases the task touches;
# perf-check is the checkpoint round, run on main by whoever integrates. Runs wait for a quiet
# machine and redo the ones that overlapped a build or another benchmark. They build through
# tools/quiet.sh, so never wrap them in it. See `python3 tools/perf/perf.py --help`.
[group('bench')]
[doc('Per task: one 10 s run per side, base vs this tree: just perf-smoke [CASE...] [-- STRESS OPTIONS]')]
perf-smoke *args:
    python3 tools/perf/perf.py smoke {{ args }}

[group('bench')]
[doc('Checkpoint round on main (over an hour): the fixed matrix against the last one. --dry-run: plan')]
perf-check *args:
    python3 tools/perf/perf.py check {{ args }}

[group('bench')]
[doc('First bad merge of a case: just perf-bisect rtx/surface/r16/144 GOOD BAD [--metric M]')]
perf-bisect case good bad *args:
    python3 tools/perf/perf.py bisect {{ case }} {{ good }} {{ bad }} {{ args }}

[private]
_stress scenario *args:
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p bench-results
    out="bench-results/$(date +%Y%m%d-%H%M%S)-{{ scenario }}"
    label="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
    git diff --quiet HEAD 2>/dev/null || label="$label-dirty"
    cargo build --release --locked --example stress
    # A busy machine (a build, another session's tests) ruins the numbers: wait, and if it
    # never calms down, run anyway but mark the run.
    tools/wait-idle.sh 60 || label="$label-busy"
    cargo run --release --locked --example stress -- --scenario {{ scenario }} --label "$label" \
        --csv "$out.csv" --summary "$out.md" {{ args }}
    python3 tools/bench_report.py "$out.md"

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
[doc('The main per-PR CI checks, locally (MSRV and the software GPU tests run apart)')]
ci: fmt-check lint test doc deny typos test-vendor test-tools
