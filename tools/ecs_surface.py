#!/usr/bin/env python3
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
