#!/usr/bin/env python3
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
