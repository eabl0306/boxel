#!/usr/bin/env bash
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
# Source directory of each crate, from cargo itself (honors CARGO_HOME, mirrors, source replacement).
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
