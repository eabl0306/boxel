#!/usr/bin/env python3
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
