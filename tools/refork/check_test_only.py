#!/usr/bin/env python3
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
