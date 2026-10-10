#!/usr/bin/env python3
"""A fixed set of snapshot scenes and the md5 of every image (issue #48: the content moves to
boxel_demo and every image must stay identical, byte for byte).

usage:
  snapshot_set.py --out DIR [--bin SNAPSHOT] [--pack DIR]... [--no-procedural]
                  [--tiers high,mid,low] [--scenes NAME|GROUP,...] [--load-world DIR]
                  [--check MANIFEST [--retries N]] [--repeat N] [--adapter NAME] [--dry-run]
  snapshot_set.py --list

Each scene (`SCENES`, `--list`) runs `examples/snapshot` once per tier and texture source
(procedural, and each `--pack`) with `--adapter nvidia --no-lkg --tier T --timeout 120`. Images
go to DIR/<source>/<tier>/<scene>-<shot>.png, the output of each run to <scene>.log next to
them, and DIR/manifest.txt lists the md5 of every image (`md5sum -c` format, sorted by path).
DIR/commands.txt holds the exact commands.

Groups (the proof of the plan for #48): s1 forest at noon and midnight; s2 the overworld with mc
caves, from the surface and in a cave; s3 terrain and the overworld with no sea; s4 a flat world
with every demo block placed (stairs corners, fences into gates, walls, torches, slabs,
waterlogged blocks, plants, lamps) and the outline of a fence, stairs, a wall and a gate; s5
water and lava flowing for 400 ticks, at three fluid times; s6 the s4 edits saved to a world
directory (DIR/<source>/<tier>/world), then that world loaded with no edits. `--load-world DIR`
makes s6-load read a copy of DIR instead, such as the world the B0 binary wrote.

Without `--bin`, the release `snapshot` example of this tree is built first (through
tools/quiet.sh, CARGO_TARGET_DIR respected). With `--check MANIFEST`, every image of this run must
have the md5 the manifest lists; images the run did not produce are not compared. Exit codes: 0
ok, 1 an image differs or is missing from the manifest, 2 a run or the build failed, 64 usage
error.

The High tier is not fully deterministic: now and then a pixel or two of an image differ by one
from run to run of the same binary (seen on B0, 21d16ec; Mid and Low were stable). So a baseline
is taken with `--repeat N` (each image is the most frequent one of N runs; DIR/variants.txt lists
the images that changed between runs), and `--check` renders a differing scene again, up to
`--retries` times (default 4), until the image equals the baseline md5 exactly. A real change
differs on every retry.

The scene list is frozen once the B0 baseline exists (`snapshot_set_test.py` holds its
fingerprint): a changed scene needs its images rendered again by the B0 binary."""
import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TIERS = ('high', 'mid', 'low')
NOON = ('noon', '12:00')
NIGHT = ('night', '00:00')

# The surface camera of the docs (docs/benchmarks.md) looking 60 degrees down, so the whole view
# stays inside the loaded radius: on the High tier, a view that reaches the edge of the loaded
# area changes a pixel or two from run to run (seen on B0, 21d16ec). And the open cave voxel
# that `headless_world --cave --camera 656,-1000,16,-90,0` finds.
SURFACE = '16,140,16,-90,-60'
CAVE = '648.5,-1014.5,5.5,-90,0'
# The forest lake of the docs.
LAKE = '205,66,-205,-40,-20'


def place(name, x, y, z):
    return ['--place', f'boxel:{name}@{x},{y},{z}']


def fill(name, a, b):
    return ['--fill', f'boxel:{name}@{a[0]},{a[1]},{a[2]}:{b[0]},{b[1]},{b[2]}']


# s4: every demo block on the flat world (ground top y = 63, edits at y = 64). Later edits of a
# cell replace earlier ones; the neighbor rules shape stairs, fences, walls and gates.
SHOWCASE = [
    # Two rings of stairs, one facing out and one facing in: outer and inner corners.
    *fill('oak_stairs[facing=north]', (0, 64, 0), (3, 64, 0)),
    *fill('oak_stairs[facing=south]', (0, 64, 3), (3, 64, 3)),
    *fill('oak_stairs[facing=west]', (0, 64, 1), (0, 64, 2)),
    *fill('oak_stairs[facing=east]', (3, 64, 1), (3, 64, 2)),
    *fill('stone_stairs[facing=south]', (6, 64, 0), (9, 64, 0)),
    *fill('stone_stairs[facing=north]', (6, 64, 3), (9, 64, 3)),
    *fill('stone_stairs[facing=east]', (6, 64, 1), (6, 64, 2)),
    *fill('stone_stairs[facing=west]', (9, 64, 1), (9, 64, 2)),
    *fill('stone_stairs[half=top,facing=east]', (12, 64, 0), (12, 64, 3)),
    # A fence run into a closed gate and an open gate, and a fence beside leaves.
    *fill('oak_fence', (0, 64, 6), (3, 64, 6)),
    *place('oak_fence_gate[facing=north]', 4, 64, 6),
    *fill('oak_fence', (5, 64, 6), (6, 64, 6)),
    *place('oak_fence_gate[facing=north,open=true]', 7, 64, 6),
    *fill('oak_fence', (8, 64, 6), (9, 64, 6)),
    *place('leaves', 10, 64, 6),
    *place('oak_fence', 10, 64, 7),
    # A gate between walls.
    *fill('cobblestone_wall', (0, 64, 9), (2, 64, 9)),
    *place('oak_fence_gate[facing=north]', 3, 64, 9),
    *fill('cobblestone_wall', (4, 64, 9), (6, 64, 9)),
    # A wall column with walls beside it (tall sides) and a torch on top.
    *fill('cobblestone_wall', (10, 64, 9), (10, 66, 9)),
    *fill('cobblestone_wall', (9, 64, 9), (9, 65, 9)),
    *fill('cobblestone_wall', (11, 64, 9), (11, 65, 9)),
    *place('torch', 10, 67, 9),
    # Wall torches on the 4 sides of a pillar.
    *fill('cobblestone', (14, 64, 9), (14, 65, 9)),
    *place('wall_torch[facing=north]', 14, 65, 8),
    *place('wall_torch[facing=south]', 14, 65, 10),
    *place('wall_torch[facing=west]', 13, 65, 9),
    *place('wall_torch[facing=east]', 15, 65, 9),
    # Slabs: bottom, top, double.
    *place('stone_slab[type=bottom]', 0, 64, 12),
    *place('stone_slab[type=top]', 1, 64, 12),
    *place('stone_slab[type=double]', 2, 64, 12),
    # Waterlogged slab, stairs, fence and wall.
    *place('stone_slab[waterlogged=true]', 4, 64, 12),
    *place('stone_stairs[waterlogged=true]', 5, 64, 12),
    *place('oak_fence[waterlogged=true]', 6, 64, 12),
    *place('cobblestone_wall[waterlogged=true]', 7, 64, 12),
    # Glass, leaves, logs on the 3 axes, plants, the 4 lamps, cobblestone and obsidian.
    *place('glass', 0, 64, 15),
    *place('leaves', 1, 64, 15),
    *place('log[axis=x]', 2, 64, 15),
    *place('log[axis=y]', 3, 64, 15),
    *place('log[axis=z]', 4, 64, 15),
    *place('short_grass', 6, 64, 15),
    *place('poppy', 7, 64, 15),
    *place('lamp_orange', 9, 64, 15),
    *place('lamp_red', 10, 64, 15),
    *place('lamp_blue', 11, 64, 15),
    *place('lamp_white', 12, 64, 15),
    *place('cobblestone', 14, 64, 15),
    *place('obsidian', 15, 64, 15),
]
SHOWCASE_CAMERA = '8,73,24,0,-38'

# s5: fluids on the flat world, 16 blocks apart so the groups never meet.
FLUIDS = [
    # Water and lava sources.
    *place('water', 0, 64, 0),
    *place('lava', 16, 64, 0),
    # Lava falling onto water.
    *place('water', 32, 64, 0),
    *place('lava', 32, 68, 0),
    # Lava beside water.
    *place('water', 0, 64, 16),
    *place('lava', 2, 64, 16),
    # Water reaching stairs, a slab, a fence and a wall.
    *place('water', 16, 64, 16),
    *place('stone_stairs[facing=west]', 18, 64, 16),
    *place('stone_slab', 16, 64, 18),
    *place('oak_fence', 14, 64, 16),
    *place('cobblestone_wall', 16, 64, 14),
    # Lava reaching stairs and glass.
    *place('lava', 32, 64, 16),
    *place('stone_stairs[facing=west]', 34, 64, 16),
    *place('glass', 32, 64, 18),
]
FLUIDS_CAMERA = '16,80,36,0,-45'

# s2: lamps that light the cave.
CAVE_LAMPS = [
    *place('lamp_white', 653, -1015, 5),
    *place('lamp_blue', 653, -1015, 9),
    *place('lamp_red', 653, -1015, 1),
    *place('lamp_orange', 648, -1038, 5),
]


def outline_camera(x, y, z):
    """A camera 3.5 blocks south of and 2.5 above block (x, y, z), looking at it."""
    return f'{x + 0.5},{y + 2.5},{z + 3.5},0,-35'


class Scene:
    """One run of `snapshot`: its options (camera and shots apart) and its shots."""

    def __init__(self, name, group, args, camera, shots, world_dir=None):
        self.name = name
        self.group = group
        self.args = args
        self.camera = camera
        self.shots = shots
        # None, 'save' (a fresh world directory) or 'load' (a copy of the saved one).
        self.world_dir = world_dir

    def describe(self):
        return {'name': self.name, 'group': self.group, 'args': self.args,
                'camera': self.camera, 'shots': self.shots, 'world_dir': self.world_dir}


def _outline(name, cell):
    return Scene(f's4-outline-{name}', 's4',
                 ['--world', 'flat', *SHOWCASE, '--outline', '{},{},{}'.format(*cell)],
                 outline_camera(*cell), [NOON])


SCENES = [
    Scene('s1-forest', 's1', ['--world', 'forest'], LAKE, [NOON, NIGHT]),
    Scene('s1-forest-wide', 's1', ['--world', 'forest'], SURFACE, [NOON]),
    Scene('s2-overworld', 's2', ['--world', 'overworld', '--caves', 'mc'], SURFACE, [NOON]),
    Scene('s2-cave', 's2', ['--world', 'overworld', '--caves', 'mc'], CAVE, [NOON]),
    Scene('s2-cave-lit', 's2', ['--world', 'overworld', '--caves', 'mc', *CAVE_LAMPS],
          '648.5,-1014.5,5.5,-90,-20', [NOON]),
    Scene('s3-terrain', 's3', ['--world', 'terrain'], SURFACE, [NOON]),
    Scene('s3-overworld-dry', 's3', ['--world', 'overworld', '--no-sea'], SURFACE, [NOON]),
    Scene('s4-showcase', 's4', ['--world', 'flat', *SHOWCASE], SHOWCASE_CAMERA, [NOON, NIGHT]),
    Scene('s4-close', 's4', ['--world', 'flat', *SHOWCASE], '13,68.5,11,40,-30',
          [NOON, NIGHT]),
    _outline('fence', (2, 64, 6)),
    _outline('stairs', (0, 64, 0)),
    _outline('wall', (10, 65, 9)),
    _outline('gate', (7, 64, 6)),
    *[Scene(f's5-fluids-t{t}', 's5',
            ['--world', 'flat', *FLUIDS, '--flow', '400', '--fluid-time', t],
            FLUIDS_CAMERA, [NOON, NIGHT]) for t in ('0', '0.5', '1.25')],
    Scene('s6-save', 's6', ['--world', 'flat', *SHOWCASE], SHOWCASE_CAMERA, [NOON, NIGHT],
          world_dir='save'),
    Scene('s6-load', 's6', ['--world', 'flat'], SHOWCASE_CAMERA, [NOON, NIGHT],
          world_dir='load'),
]


def fingerprint(scenes=None):
    """md5 of the scene definitions (what the B0 images were rendered from)."""
    data = json.dumps([s.describe() for s in (scenes or SCENES)], sort_keys=True)
    return hashlib.md5(data.encode()).hexdigest()


def select(spec):
    """The scenes named by `spec` (comma list of names or groups), in list order."""
    if not spec:
        return list(SCENES)
    wanted = [w.strip() for w in spec.split(',') if w.strip()]
    known = {s.name for s in SCENES} | {s.group for s in SCENES}
    unknown = [w for w in wanted if w not in known]
    if unknown:
        raise ValueError(f'unknown scenes: {", ".join(unknown)} (see --list)')
    return [s for s in SCENES if s.name in wanted or s.group in wanted]


def md5_file(path):
    h = hashlib.md5()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def read_manifest(path):
    """`{relative path: md5}` from a manifest (`md5  path` lines)."""
    out = {}
    with open(path, encoding='utf-8') as f:
        for number, line in enumerate(f, 1):
            line = line.rstrip('\n')
            if not line:
                continue
            digest, sep, rel = line.partition('  ')
            if not sep or len(digest) != 32:
                raise ValueError(f'{path}:{number}: not "md5  path": {line!r}')
            out[rel] = digest
    return out


def write_manifest(path, entries):
    with open(path, 'w', encoding='utf-8') as f:
        for rel in sorted(entries):
            f.write(f'{entries[rel]}  {rel}\n')


def compare(baseline, entries):
    """Problems of `entries` against `baseline` (only the paths of `entries` are compared)."""
    problems = []
    for rel in sorted(entries):
        if rel not in baseline:
            problems.append(f'not in the baseline: {rel}')
        elif baseline[rel] != entries[rel]:
            problems.append(f'differs: {rel} (baseline {baseline[rel]}, now {entries[rel]})')
    return problems


class Failed(Exception):
    pass


def build(log):
    """Builds the release snapshot example of this tree; returns the binary."""
    target = os.environ.get('CARGO_TARGET_DIR') or os.path.join(ROOT, 'target')
    cmd = ['cargo', 'build', '--release', '--locked', '--example', 'snapshot']
    if not os.environ.get('BOXEL_QUIET_SLOT'):
        cmd = [os.path.join(ROOT, 'tools', 'quiet.sh')] + cmd
    print(f'snapshot_set: building snapshot (log: {log})', flush=True)
    with open(log, 'w') as f:
        rc = subprocess.run(cmd, cwd=ROOT, stdout=f, stderr=subprocess.STDOUT,
                            stdin=subprocess.DEVNULL).returncode
    if rc != 0:
        raise Failed(f'the build failed (exit {rc}), see {log}')
    return os.path.join(target, 'release', 'examples', 'snapshot')


def command(binary, scene, tier, pack, folder, adapter, timeout, world=None, suffix=''):
    """The command line of one run; `world` is its --world-dir, `suffix` goes after the image
    names (repeats and retries)."""
    cmd = [binary, '--adapter', adapter, '--no-lkg', '--tier', tier, '--timeout', str(timeout)]
    if pack:
        cmd += ['--pack', pack]
    cmd += scene.args + ['--camera', scene.camera]
    if world:
        cmd += ['--world-dir', world]
    for label, clock in scene.shots:
        cmd += ['--shot', f'{clock}={os.path.join(folder, f"{scene.name}-{label}{suffix}.png")}']
    return cmd


def quote(cmd):
    return ' '.join(a if a and all(c.isalnum() or c in '-_./,:=@+' for c in a)
                    else "'" + a.replace("'", "'\\''") + "'" for a in cmd)


def mode(digests):
    """The most frequent md5 (the first seen among equals)."""
    counts = {}
    for d in digests:
        counts[d] = counts.get(d, 0) + 1
    return max(counts, key=lambda d: (counts[d], -digests.index(d)))


class Runner:
    """Runs the scenes of one texture source and tier."""

    def __init__(self, args, binary, outdir, source, pack, tier, out):
        self.args = args
        self.binary = binary
        self.outdir = outdir
        self.source = source
        self.pack = pack
        self.tier = tier
        self.out = out
        self.folder = os.path.join(outdir, source, tier)
        self.saved = os.path.join(self.folder, 'world')
        self.commands = []

    def rel(self, path):
        return os.path.relpath(path, self.outdir).replace(os.sep, '/')

    def attempt(self, scene, k, kind):
        """Runs `scene` once (`k` = 0 the main run, else a repeat or retry). Returns the error,
        or None and `{label: png}`."""
        suffix = f'.{kind}{k}' if k else ''
        world, scratch = None, None
        if scene.world_dir == 'save':
            if k:
                scratch = tempfile.mkdtemp(prefix='snapshot-set-world-')
                world = os.path.join(scratch, 'world')
            else:
                world = self.saved
                if not self.args.dry_run:
                    shutil.rmtree(world, ignore_errors=True)
        elif scene.world_dir == 'load':
            loaded = os.path.abspath(self.args.load_world) if self.args.load_world else self.saved
            if not self.args.dry_run and not os.path.isdir(loaded):
                return (f'{self.source}/{self.tier}/{scene.name}: no saved world at {loaded} (run '
                        's6-save first or --load-world)'), {}
            # A copy: the run saves the world again when it ends.
            scratch = tempfile.mkdtemp(prefix='snapshot-set-world-')
            world = os.path.join(scratch, 'world')
            if not self.args.dry_run:
                shutil.copytree(loaded, world)
        cmd = command(self.binary, scene, self.tier, self.pack, self.folder, self.args.adapter,
                      self.args.timeout, world, suffix)
        pngs = {label: os.path.join(self.folder, f'{scene.name}-{label}{suffix}.png')
                for label, _ in scene.shots}
        try:
            if self.args.dry_run:
                print(quote(cmd), file=self.out)
                return None, pngs
            if not k:
                self.commands.append(quote(cmd))
            log = os.path.join(self.folder, f'{scene.name}.log')
            with open(log, 'a' if k else 'w') as f:
                f.write(('\n# ' + kind + f' {k}\n' if k else '') + quote(cmd) + '\n')
                f.flush()
                rc = subprocess.run(cmd, stdout=f, stderr=subprocess.STDOUT,
                                    stdin=subprocess.DEVNULL).returncode
        finally:
            if scratch:
                shutil.rmtree(scratch, ignore_errors=True)
        with open(log) as f:
            text = f.read().rsplit(quote(cmd), 1)[-1]
        for line in text.splitlines():
            if line.startswith('edits:') and not line.endswith('not_applied=0'):
                print(f'snapshot_set: warning: {self.source}/{self.tier}/{scene.name}: {line}',
                      file=self.out)
        if rc != 0:
            return f'{self.source}/{self.tier}/{scene.name}: exit {rc}, see {log}', {}
        for png in pngs.values():
            if not os.path.isfile(png):
                return f'{self.rel(png)}: not written, see {log}', {}
        return None, pngs

    def scene(self, scene, baseline, notes):
        """Runs `scene` with its repeats and retries; returns the error, or None and the
        `{relative path: md5}` of its images."""
        error, main = self.attempt(scene, 0, 'repeat')
        if error or self.args.dry_run:
            return error, {}
        found = {label: [md5_file(png)] for label, png in main.items()}
        files = {label: {found[label][0]: png} for label, png in main.items()}
        extra = []
        # --repeat: the most frequent image of N runs (the High tier changes a pixel or two
        # from run to run now and then).
        for k in range(1, self.args.repeat):
            error, pngs = self.attempt(scene, k, 'repeat')
            if error:
                return error, {}
            for label, png in pngs.items():
                digest = md5_file(png)
                found[label].append(digest)
                files[label].setdefault(digest, png)
                extra.append(png)
        chosen = {label: mode(digests) for label, digests in found.items()}
        for label, digests in found.items():
            if len(set(digests)) > 1:
                counts = ', '.join(f'{d} x{digests.count(d)}' for d in dict.fromkeys(digests))
                notes.append(f'variants: {self.rel(main[label])}: {counts}')
        # --retries: a differing image is rendered again until it equals the baseline.
        if baseline is not None:
            for k in range(1, self.args.retries + 1):
                wrong = [label for label in chosen
                         if baseline.get(self.rel(main[label])) not in (None, chosen[label])]
                if not wrong:
                    break
                error, pngs = self.attempt(scene, k, 'retry')
                if error:
                    return error, {}
                for label in wrong:
                    digest = md5_file(pngs[label])
                    extra.append(pngs[label])
                    if digest == baseline[self.rel(main[label])]:
                        chosen[label] = digest
                        files[label].setdefault(digest, pngs[label])
                        notes.append(f'retry: {self.rel(main[label])} equals the baseline on '
                                     f'retry {k}')
        for label, png in main.items():
            keep = files[label][chosen[label]]
            if keep != png:
                shutil.copyfile(keep, png)
        for png in extra:
            if os.path.exists(png):
                os.remove(png)
        return None, {self.rel(png): chosen[label] for label, png in main.items()}


def run_set(args, out=None):
    """Runs the selected scenes; returns the exit code."""
    out = out or sys.stdout
    scenes = select(args.scenes)
    tiers = [t.strip() for t in args.tiers.split(',') if t.strip()]
    bad = [t for t in tiers if t not in TIERS]
    if bad or not tiers:
        raise ValueError(f'--tiers: {", ".join(bad) or "none"} (high, mid or low)')
    if args.repeat < 1 or args.retries < 0:
        raise ValueError('--repeat must be at least 1 and --retries at least 0')
    sources = ([] if args.no_procedural else [('procedural', None)])
    for pack in args.pack or []:
        pack = os.path.abspath(pack)
        if not os.path.isfile(os.path.join(pack, 'pack.mcmeta')):
            raise ValueError(f'--pack {pack}: no pack.mcmeta')
        sources.append((os.path.basename(pack.rstrip('/')), pack))
    if not sources:
        raise ValueError('no texture source: --no-procedural needs a --pack')
    if args.load_world and not os.path.isdir(args.load_world):
        raise ValueError(f'--load-world {args.load_world}: not a directory')
    baseline = read_manifest(args.check) if args.check else None
    outdir = os.path.abspath(args.out)
    os.makedirs(outdir, exist_ok=True)
    binary = args.bin or (None if args.dry_run else build(os.path.join(outdir, 'build.log')))
    binary = binary or 'snapshot'
    commands, entries, failures, notes = [], {}, [], []
    for source, pack in sources:
        for tier in tiers:
            runner = Runner(args, binary, outdir, source, pack, tier, out)
            os.makedirs(runner.folder, exist_ok=True)
            for scene in scenes:
                error, images = runner.scene(scene, baseline, notes)
                if error:
                    failures.append(error)
                    continue
                entries.update(images)
                if not args.dry_run:
                    print(f'snapshot_set: {source}/{tier}/{scene.name} ok', file=out, flush=True)
            commands += runner.commands
    if args.dry_run:
        return 0
    with open(os.path.join(outdir, 'commands.txt'), 'w', encoding='utf-8') as f:
        f.write(f'# scenes fingerprint {fingerprint()}; --repeat {args.repeat}\n')
        f.write('\n'.join(commands) + '\n')
    with open(os.path.join(outdir, 'variants.txt'), 'w', encoding='utf-8') as f:
        f.write(f'# --repeat {args.repeat} --retries {args.retries}: images that were not the same '
                'on every run\n')
        f.write(''.join(n + '\n' for n in notes))
    # A partial run keeps the entries of earlier runs into the same directory.
    manifest = os.path.join(outdir, 'manifest.txt')
    merged = read_manifest(manifest) if os.path.exists(manifest) else {}
    merged.update(entries)
    write_manifest(manifest, merged)
    if notes:
        print('\n'.join(notes), file=out)
    print(f'snapshot_set: {len(entries)} images, manifest {manifest}', file=out)
    problems = compare(baseline, entries) if baseline is not None else []
    if problems:
        print('\n'.join(problems), file=out)
        print(f'snapshot_set: {len(problems)} of {len(entries)} images differ from '
              f'{args.check}', file=out)
    elif baseline is not None and not failures:
        print(f'snapshot_set: all {len(entries)} images equal {args.check}', file=out)
    if failures:
        print('\n'.join(failures), file=out)
        return 2
    return 1 if problems else 0


def main(argv=None):
    parser = argparse.ArgumentParser(
        description=__doc__.split('\n\n')[0],
        formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--out', help='output directory')
    parser.add_argument('--bin', help='snapshot binary (default: build this tree)')
    parser.add_argument('--pack', action='append', help='resource pack directory (repeatable)')
    parser.add_argument('--no-procedural', action='store_true',
                        help='only the --pack sources')
    parser.add_argument('--tiers', default=','.join(TIERS), help='default high,mid,low')
    parser.add_argument('--scenes', help='comma list of scene names or groups (s1..s6)')
    parser.add_argument('--load-world', help='world directory that s6-load reads (a copy)')
    parser.add_argument('--check', help='manifest the images must match')
    parser.add_argument('--adapter', default='nvidia', help='GPU adapter (default nvidia)')
    parser.add_argument('--timeout', type=float, default=120, help='seconds per run')
    parser.add_argument('--repeat', type=int, default=1,
                        help='runs per scene; each image is the most frequent of them (baselines)')
    parser.add_argument('--retries', type=int, default=4,
                        help='with --check, runs again a scene whose image differs, up to N times '
                             '(default 4)')
    parser.add_argument('--dry-run', action='store_true', help='print the commands only')
    parser.add_argument('--list', action='store_true', help='print the scenes and exit')
    args = parser.parse_args(argv)
    if args.list:
        for s in SCENES:
            shots = ' '.join(f'{label}={clock}' for label, clock in s.shots)
            world = f' world-dir={s.world_dir}' if s.world_dir else ''
            print(f'{s.name} [{s.group}] camera {s.camera} shots {shots}{world}: '
                  f'{quote(s.args)}')
        print(f'fingerprint {fingerprint()}')
        return 0
    if not args.out:
        parser.print_usage(sys.stderr)
        print('snapshot_set: --out is required', file=sys.stderr)
        return 64
    try:
        return run_set(args)
    except ValueError as e:
        print(f'snapshot_set: {e}', file=sys.stderr)
        return 64
    except Failed as e:
        print(f'snapshot_set: {e}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
