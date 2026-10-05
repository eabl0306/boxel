#!/usr/bin/env python3
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
