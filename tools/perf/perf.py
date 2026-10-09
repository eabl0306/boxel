#!/usr/bin/env python3
"""boxel performance checks: the smoke check of each task, the checkpoint round and the bisect.

usage:
  perf.py smoke [--base REF|BIN] [--new REF|BIN] [--gpu G] [--radius N] [--cap N] [CASE...]
                [-- STRESS OPTIONS]
  perf.py check [--base REF|BIN] [--ref REF] [--rounds N] [--dry-run] [--no-tag]
                [--accept-partial] [--off-main]
  perf.py bisect CASE GOOD BAD [--metric M]... [-- STRESS OPTIONS]
  perf.py cases

smoke (level 1, `just perf-smoke`): one 10 s stress run per side of each case, base against the
new binary, on a quiet machine. The base is a git ref (default: the merge base of HEAD and main)
or a binary; its binary comes from bench-results/bins/<sha>/stress, built from a git worktree of
the base when missing. The new side is this checkout as it is (built in place), or --new. A case
fails only on a gross regression: fps under 85 % of the base, p99 over 1.5 times the base, or
frames over 6.94 ms where the base had none. A failing pair is repeated once and only a second
failure counts. Cases: surface and forest by default, on the RTX, radius 16, uncapped. Options
after `--` go to stress on both sides (`-- --bodies 10000`).

check (level 2, `just perf-check`): the fixed matrix (`perf.py cases`), 3 interleaved runs of
20 s per case and side, on a commit of main, against the binary of the last checkpoint
(bench-results/control/last, else the highest perf-control-N tag). Writes
docs/rendimiento/control-YYYY-MM-DD.md. Only a complete round (every run counted) becomes the
next base and gets the local tag perf-control-N, unless --accept-partial. --dry-run only prints
the plan; --off-main measures another commit without touching the checkpoints.

bisect (`just perf-bisect CASE GOOD BAD`): the smoke check of CASE against GOOD on the
first-parent commits (the merges) of GOOD..BAD, by halves, to find the first bad one. With
--metric M (what the checkpoint report flagged), the checkpoint rule of M on 3 interleaved runs
of 20 s per side instead.

A run that overlapped a build or another benchmark, or that the compositor throttled, is redone;
one that still does not count after 4 attempts never feeds a verdict. Builds go through
tools/quiet.sh, so do not run this inside it. PERF_TARGET_DIR (default <repo>/target-perf) holds
the base worktree and its target dir. Exit codes: 0 ok; 1 worse, or the new binary failed (or
lost a scenario) where the base ran; 2 could not measure everything (a build or the base failed,
both sides failed, a run never counted, the machine never got quiet, the base lacks a case);
64 usage error.
"""
import argparse
import dataclasses
import datetime
import fcntl
import functools
import hashlib
import json
import math
import os
import shutil
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.dirname(HERE))
import bench_report  # noqa: E402
import runs  # noqa: E402
from runs import Case  # noqa: E402

TREE = os.path.dirname(os.path.dirname(HERE))
QUIET = os.path.join(TREE, 'tools', 'quiet.sh')

DEFAULT_SMOKE = ('surface', 'forest')
SMOKE_S = 10
CHECK_S = 20
CHECK_ROUNDS = 3
FPS_MIN = 0.85
P99_MAX = 1.5
MATRIX = [Case(gpu, scenario, 16, cap)
          for gpu in ('rtx', '780m')
          for scenario in ('surface', 'cave-path', 'cave-static', 'forest', 'dive')
          for cap in (144, 0)] + [Case('rtx', 'cave-path', 32, 144),
                                  Case('rtx', 'surface', 32, 144)]
# Binaries of dirty trees older than this are removed from the cache (no smoke runs that long).
DIRTY_KEEP_S = 3 * 24 * 3600


class PerfError(Exception):
    """Something to measure could not be set up (exit 2)."""


def say(text=''):
    print(text, flush=True)


def git(*args, cwd=None):
    done = subprocess.run(['git', *args], cwd=cwd or TREE, capture_output=True, text=True)
    if done.returncode != 0:
        raise PerfError(f'git {" ".join(args)}: {done.stderr.strip()}')
    return done.stdout.strip()


def git_bytes(*args):
    done = subprocess.run(['git', *args], cwd=TREE, capture_output=True)
    if done.returncode != 0:
        raise PerfError(f'git {" ".join(args)}: {done.stderr.decode(errors="replace").strip()}')
    return done.stdout


@functools.cache
def root():
    """The main checkout (shared by every worktree): its bench-results/ holds the caches."""
    common = git('rev-parse', '--path-format=absolute', '--git-common-dir')
    return os.path.dirname(common)


def bench(*parts):
    return os.path.join(root(), 'bench-results', *parts)


def perf_dir():
    return os.path.abspath(os.environ.get('PERF_TARGET_DIR') or os.path.join(root(), 'target-perf'))


def commit(ref):
    try:
        return git('rev-parse', '--verify', '--quiet', ref + '^{commit}')
    except PerfError:
        raise PerfError(f'{ref!r} is not a commit') from None


def subject(sha):
    return git('log', '-1', '--format=%h %s', sha)


def on_main(sha):
    """True if `sha` is main or one of its ancestors."""
    done = subprocess.run(['git', 'merge-base', '--is-ancestor', sha, 'main'], cwd=TREE,
                          capture_output=True, text=True)
    if done.returncode > 1:
        raise PerfError(f'cannot tell whether {sha[:8]} is on main: {done.stderr.strip()}')
    return done.returncode == 0


def no_quiet_slot():
    """Refuses to run inside tools/quiet.sh: the builds go through quiet.sh themselves (the outer
    slot would keep them waiting, forever when two harnesses hold both slots) and the runs would
    get the slot's low priority and RAM cap."""
    if os.environ.get('BOXEL_QUIET_SLOT'):
        raise ValueError('do not run perf-* through tools/quiet.sh: it builds through quiet.sh '
                         'already, and the outer slot would block its builds')


# ---------------------------------------------------------------------------------------------
# Binaries

@dataclasses.dataclass(frozen=True)
class Binary:
    path: str
    label: str
    sha: str = None
    name: str = ''

    def __str__(self):
        return f'{self.name} {self.label}' if self.name else self.label


def md5(path):
    h = hashlib.md5()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()[:8]


def stage(built):
    """A private copy of a fresh binary in the cache folder (cargo hard-links its outputs, and a
    later build would change the file under a running benchmark). `place` moves it in."""
    folder = bench('bins')
    os.makedirs(folder, exist_ok=True)
    tmp = os.path.join(folder, f'.stage-{os.getpid()}-{time.monotonic_ns()}')
    shutil.copy2(built, tmp)
    return tmp


def place(tmp, dest):
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    os.replace(tmp, dest)
    return dest


OWNER = '.perf-tree'


def claim(target, tree):
    """Marks `target` as the target dir of the source tree `tree`, and refuses one that another
    tree built in through this harness: cargo checks workspace crates by their path inside the
    workspace and by mtime, so it can take the other tree's crates for this one's (a 0 s build
    that gives the other tree's binary)."""
    tree = os.path.realpath(tree)
    path = os.path.join(target, OWNER)
    try:
        with open(path) as f:
            owner = f.read().strip()
    except OSError:
        owner = None
    if owner and owner != tree:
        raise PerfError(f'{target} is the target dir of {owner}: cargo could reuse its crates for '
                        f'{tree}. Build this tree in its own target dir (CARGO_TARGET_DIR)')
    if not owner:
        os.makedirs(target, exist_ok=True)
        with open(path, 'w') as f:
            f.write(tree + '\n')


def cargo_build(cwd, target, log):
    """The release stress example through tools/quiet.sh; its output goes to `log`."""
    os.makedirs(os.path.dirname(log), exist_ok=True)
    env = dict(os.environ, CARGO_TARGET_DIR=target)
    say(f'perf: building stress in {cwd} (log: {log})')
    start = time.monotonic()
    with open(log, 'w') as f:
        rc = subprocess.run([QUIET, 'cargo', 'build', '--release', '--locked', '--example',
                             'stress'], cwd=cwd, env=env, stdout=f, stderr=subprocess.STDOUT,
                            stdin=subprocess.DEVNULL).returncode
    if rc != 0:
        with open(log) as f:
            tail = ''.join(f.readlines()[-15:])
        raise PerfError(f'the build failed (exit {rc}), end of {log}:\n{tail}')
    say(f'perf: built in {time.monotonic() - start:.0f} s')
    return os.path.join(target, 'release', 'examples', 'stress')


def commit_binary(sha, name=''):
    """The stress binary of a commit: bench-results/bins/<sha>/stress, built in the base worktree
    (PERF_TARGET_DIR/src, checked out at `sha`) when missing."""
    dest = bench('bins', sha, 'stress')
    if not os.path.exists(dest):
        folder = perf_dir()
        os.makedirs(folder, exist_ok=True)
        with open(os.path.join(folder, 'build.lock'), 'w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            if not os.path.exists(dest):
                src = os.path.join(folder, 'src')
                claim(folder, src)
                if os.path.exists(os.path.join(src, '.git')):
                    git('checkout', '--quiet', '--detach', '--force', sha, cwd=src)
                else:
                    git('worktree', 'prune')
                    git('worktree', 'add', '--quiet', '--detach', src, sha)
                built = cargo_build(src, folder, bench('bins', f'build-{sha[:8]}.log'))
                place(stage(built), dest)
    return Binary(dest, sha[:8], sha, name)


def tree_state():
    """(HEAD, fingerprint, dirty) of this checkout. The fingerprint covers the changes to the
    tracked files and the untracked ones (by size and mtime), so a build can tell whether the
    tree changed under it."""
    head = git('rev-parse', 'HEAD')
    diff = git_bytes('diff', '--binary', '--no-ext-diff', '--no-textconv', '--no-color', 'HEAD')
    h = hashlib.sha1(diff)
    others = git_bytes('ls-files', '-z', '--others', '--exclude-standard')
    for name in sorted(filter(None, others.split(b'\0'))):
        try:
            st = os.stat(os.path.join(os.fsencode(TREE), name))
        except OSError:
            continue
        h.update(b'%s\0%d\0%d\0' % (name, st.st_size, st.st_mtime_ns))
    return head, h.hexdigest(), bool(diff)


def prune_dirty():
    """Drops the cached binaries of dirty trees that no smoke can still be running."""
    folder = bench('bins')
    try:
        names = os.listdir(folder)
    except OSError:
        return
    now = time.time()
    for name in names:
        path = os.path.join(folder, name)
        try:
            old = now - os.stat(path).st_mtime > DIRTY_KEEP_S
        except OSError:
            continue
        if old and (name.startswith('dirty-') or name.startswith('.stage-')):
            if os.path.isdir(path):
                shutil.rmtree(path, ignore_errors=True)
            else:
                os.remove(path)


def tree_binary():
    """This checkout as it is: the cached binary of HEAD when the tree is clean, else built in
    place (CARGO_TARGET_DIR or target/) and copied to the cache: bins/<HEAD>/stress when clean,
    bins/dirty-<md5>/stress when not. A tree that changed during the build gives no binary."""
    target = os.path.join(TREE, os.environ.get('CARGO_TARGET_DIR') or 'target')
    if os.path.realpath(target) == os.path.realpath(perf_dir()):
        # Cargo keys workspace crates by their path inside the workspace and checks them by
        # mtime, so a file this tree changed before the last base build would count as built.
        raise PerfError(f'the base builds and this tree share {target}: cargo could reuse the '
                        "base's crates for this tree; give PERF_TARGET_DIR another directory")
    claim(target, TREE)
    head, files, dirty = tree_state()
    cached = bench('bins', head, 'stress')
    if not dirty and os.path.exists(cached):
        return Binary(cached, head[:8], head, 'this tree')
    prune_dirty()
    name = os.path.basename(TREE)
    built = cargo_build(TREE, target, bench('bins', f'build-{name}.log'))
    tmp = stage(built)
    try:
        if tree_state() != (head, files, dirty):
            raise PerfError('this tree changed while stress was building (a commit or an edit), '
                            'so the binary may hold part of each version; run it again once the '
                            'tree stays put')
        if not dirty:
            return Binary(place(tmp, cached), head[:8], head, 'this tree')
        digest = md5(tmp)
        dest = place(tmp, bench('bins', f'dirty-{digest}', 'stress'))
        return Binary(dest, f'{head[:8]}-dirty-{digest}', None, 'this tree')
    finally:
        if os.path.exists(tmp):
            os.remove(tmp)


def resolve(spec, name=''):
    """None: this checkout; an existing file: that binary; else a git ref."""
    if spec is None:
        return tree_binary()
    if os.path.isfile(spec):
        path = os.path.abspath(spec)
        return Binary(path, f'file-{md5(path)}', None, spec)
    return commit_binary(commit(spec), name or spec)


# ---------------------------------------------------------------------------------------------
# Verdicts

def gross(base, new, fps_min=FPS_MIN, p99_max=P99_MAX):
    """Gross regressions of `new` against `base` (metrics of one run each): the reasons. Slow
    frames are those over 6.94 ms, capped or not."""
    out = []
    if base.get('fps') and new.get('fps') is not None and new['fps'] < fps_min * base['fps']:
        out.append(f"fps {fmt(new['fps'])} under {fps_min:.0%} of {fmt(base['fps'])}")
    if base.get('p99') and new.get('p99') is not None and new['p99'] > p99_max * base['p99']:
        out.append(f"p99 {fmt(new['p99'])} ms over {p99_max:g}x {fmt(base['p99'])} ms")
    if base.get('slow') == 0 and new.get('slow'):
        out.append(f"{new['slow']} frames over 6.94 ms, the base had none")
    return out


def new_failed(base, new):
    """True if the new run failed while the base of its pair ran: the binary's fault, not the
    machine's (a missing GPU makes both fail)."""
    return new is not None and bool(new.error) and base is not None and base.ran


def unmeasured_reason(pairs):
    """Why runs of `pairs` do not count: each base or new run that failed or stayed noisy."""
    out = []
    for b, n in pairs:
        for side, run in (('base', b), ('new', n)):
            if run is not None and not run.counts:
                out.append(f'{side} run {run.error or run.noisy}')
    return '; '.join(out) or 'nothing ran'


@dataclasses.dataclass
class SmokeResult:
    case: Case
    pairs: list = dataclasses.field(default_factory=list)
    problems: list = dataclasses.field(default_factory=list)
    # The base has no such scenario (nothing to compare against).
    skipped: str = None
    # The new binary lost a scenario the base has.
    lost: str = None
    # Not run at all (the machine never got quiet for an earlier case).
    not_run: str = None

    @property
    def status(self):
        """failed (the new binary failed where the base ran, or lost the scenario), skipped (the
        base lacks the scenario), unmeasured (a run did not count: the base failed, both failed,
        the machine was busy or throttled or never got quiet), worse (confirmed) or ok."""
        if self.lost or any(new_failed(b, n) for b, n in self.pairs):
            return 'failed'
        if self.skipped:
            return 'skipped'
        if self.not_run or not self.pairs or any(
                r is None or not r.counts for pair in self.pairs for r in pair):
            return 'unmeasured'
        if len(self.problems) == 2 and all(self.problems):
            return 'worse'
        return 'ok'

    @property
    def gave_up(self):
        """A run waited out PERF_MAX_WAIT: later cases would wait as long."""
        return any(r is not None and r.gave_up for pair in self.pairs for r in pair)

    def verdict(self):
        status = self.status
        if status == 'failed':
            if self.lost:
                return f'FAILED: {self.lost}'
            return 'FAILED: new run ' + next(n.error for b, n in self.pairs if new_failed(b, n))
        if status == 'skipped':
            return f'skipped: {self.skipped}'
        if status == 'unmeasured':
            text = 'not measured: ' + (self.not_run or unmeasured_reason(self.pairs))
            if self.problems and self.problems[0]:
                text += f" (the first pair looked worse: {'; '.join(self.problems[0])})"
            return text
        if status == 'worse':
            return 'WORSE: ' + '; '.join(self.problems[-1])
        if len(self.problems) == 2:
            return 'ok (the first pair looked worse, the repeat did not)'
        return 'ok'


def fmt(value):
    return bench_report.fmt(value)


def run_line(side, run):
    if run.error:
        detail = ''.join(f'\n        {line}' for line in run.detail.splitlines())
        return f'    {side:<4} {run.error}{detail}'
    if run.metrics is None:
        return f'    {side:<4} not measured: {run.noisy}'
    m = run.metrics
    text = (f"    {side:<4} {fmt(m['fps'])} fps, p50 {fmt(m['p50'])} ms, p99 {fmt(m['p99'])} ms, "
            f"slow {fmt(m['slow'])} ({run.seconds:.0f} s)")
    return text + (f' NOISY, does not count: {run.noisy}' if run.noisy else '')


def run_pair(case, base, new, folder, tag, flip, duration, nvidia=False):
    """The base and the new run of a pair (the new one first when `flip`); None for a side not
    run. It stops after the first side when the pair cannot count whatever the second does: the
    base did not count, or the new run did not count for the machine's sake. A failed new run
    still needs its base, to tell the binary from the machine."""
    order = [('base', base), ('new', new)]
    if flip:
        order.reverse()
    done = {'base': None, 'new': None}
    for side, binary in order:
        prefix = os.path.join(folder, side, case.slug + tag)
        os.makedirs(os.path.dirname(prefix), exist_ok=True)
        run = runs.measure(binary.path, case, prefix, f'{side}-{binary.label}', duration,
                           nvidia=nvidia, say=say)
        done[side] = run
        say(run_line(side, run))
        if not run.counts and (side == 'base' or not run.error):
            break
    return done['base'], done['new']


def smoke_case(case, base, new, folder, duration=SMOKE_S, flip=False, fps_min=FPS_MIN,
               p99_max=P99_MAX, nvidia=False):
    """The smoke check of one case: a pair of runs, and a second pair when the first looks
    grossly worse."""
    result = SmokeResult(case)
    has_base, has_new = runs.supports(base.path, case), runs.supports(new.path, case)
    if has_base and not has_new:
        result.lost = f'{new} has no {case.scenario} scenario and the base has it'
    elif not has_base:
        result.skipped = f'{base} has no {case.scenario} scenario'
    if result.lost or result.skipped:
        say(f'  {case}: {result.verdict()}')
        return result
    say(f'  {case} ({case.label})')
    for tag in ('', '-repeat'):
        pair = run_pair(case, base, new, folder, tag, flip ^ bool(tag), duration, nvidia)
        result.pairs.append(pair)
        if result.status != 'ok':
            break
        problems = gross(pair[0].metrics, pair[1].metrics, fps_min, p99_max)
        result.problems.append(problems)
        if not problems:
            break
        if not tag:
            say(f"    looks worse ({'; '.join(problems)}), repeating the pair once")
    b, n = result.pairs[-1]
    if (case.scenario.endswith('static') and b and n and b.counts and n.counts
            and b.metrics['quads'] != n.metrics['quads']):
        say(f"    note: the settled world differs: quads {fmt(b.metrics['quads'])} -> "
            f"{fmt(n.metrics['quads'])}, draws {fmt(b.metrics['draws'])} -> "
            f"{fmt(n.metrics['draws'])}")
    say(f'    {result.verdict()}')
    return result


def change(b, n):
    if b is None or n is None:
        return f'{fmt(b)} -> {fmt(n)}'
    pct = f' ({(n - b) / b * 100:+.1f} %)' if b else ''
    return f'{fmt(b)} -> {fmt(n)}{pct}'


def smoke_table(results):
    """Markdown table of the last pair of each case (ready for a PR description). Numbers only
    from runs that count."""
    lines = ['| case | fps | p99 ms | frames > 6.94 ms | verdict |', '|---|---:|---:|---:|---|']
    for r in results:
        pair = r.pairs[-1] if r.pairs else (None, None)
        if not all(x is not None and x.counts for x in pair):
            lines.append(f'| {r.case} | - | - | - | {r.verdict()} |')
            continue
        b, n = (x.metrics for x in pair)
        lines.append(f"| {r.case} | {change(b['fps'], n['fps'])} | {fmt(b['p99'])} -> "
                     f"{fmt(n['p99'])} | {fmt(b['slow'])} -> {fmt(n['slow'])} | {r.verdict()} |")
    return '\n'.join(lines)


def exit_code(results):
    statuses = {r.status for r in results}
    if statuses & {'worse', 'failed'}:
        return 1
    return 2 if statuses & {'unmeasured', 'skipped'} else 0


def minutes(seconds):
    return f'{max(1, round(seconds / 60))} min'


def stamp():
    return datetime.datetime.now().strftime('%Y%m%d-%H%M%S')


def merge_base():
    try:
        return git('merge-base', 'HEAD', 'main')
    except PerfError:
        raise PerfError('no merge base with main: pass --base REF') from None


def cmd_smoke(a):
    no_quiet_slot()
    default = Case(gpu=a.gpu, radius=a.radius, cap=a.cap, extra=tuple(a.extra))
    cases = [runs.parse_case(text, default) for text in (a.cases or DEFAULT_SMOKE)]
    runs_s = sum(2 * runs.estimate_s(c, a.duration) for c in cases)
    say(f'perf-smoke: {len(cases)} cases, {2 * len(cases)} runs of {a.duration:g} s, about '
        f'{minutes(runs_s)} of runs (more with builds or repeats)')
    base = resolve(a.base or merge_base(), a.base or 'merge base')
    new = resolve(a.new)
    base_md5, new_md5 = md5(base.path), md5(new.path)
    say(f'perf-smoke: base {base} (md5 {base_md5}), new {new} (md5 {new_md5})')
    if base_md5 == new_md5 and base.path != new.path:
        say('perf-smoke: note: both binaries are the same, so the branch does not change stress '
            '(only docs, tests or tools) or a build reused the other one')
    folder = bench('smoke', stamp())
    results = []
    for i, c in enumerate(cases):
        if any(r.gave_up or r.not_run for r in results):
            results.append(SmokeResult(c, not_run='not run, the machine never got quiet for an '
                                       'earlier case'))
            say(f'  {c}: {results[-1].verdict()}')
            continue
        results.append(smoke_case(c, base, new, folder, a.duration, i % 2 == 1, a.fps_min,
                                  a.p99_max, a.nvidia))
    code = exit_code(results)
    say()
    say(smoke_table(results))
    say()
    verdict = {0: 'ok, no gross regression',
               1: 'GROSS REGRESSION, or the new binary failed where the base ran',
               2: 'could not measure everything (see the verdicts; nothing here says ok)'}[code]
    say(f'perf-smoke: {verdict}. Runs in {folder} '
        f'(every metric: just bench-report --compare {folder}/base {folder}/new)')
    return code


# ---------------------------------------------------------------------------------------------
# Checkpoint round

def read_last():
    try:
        with open(bench('control', 'last')) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def tag_number(tag):
    tail = tag.rsplit('-', 1)[-1]
    return int(tail) if tail.isdigit() else None


def next_number(tags, last=None):
    numbers = [n for n in map(tag_number, tags) if n is not None]
    if last and isinstance(last.get('number'), int):
        numbers.append(last['number'])
    return max(numbers, default=0) + 1


def last_checkpoint(last, tags):
    """(sha, tag) of the last checkpoint: bench-results/control/last, else the highest
    perf-control-N tag (the file is git-ignored and can get lost), else (None, None)."""
    if last and last.get('sha'):
        return last['sha'], last.get('tag')
    numbered = [(n, t) for t in tags if (n := tag_number(t)) is not None]
    if not numbered:
        return None, None
    tag = max(numbered)[1]
    return commit(tag), tag


def rotate(items, n):
    n %= len(items) or 1
    return items[n:] + items[:n]


def schedule(cases, sides, rounds):
    """(round, case, side) in run order: each round goes through the cases in a rotated order,
    and the side that runs first alternates from case to case and round to round."""
    out = []
    for r in range(rounds):
        for i, case in enumerate(rotate(list(cases), r)):
            order = list(sides[case])
            if (r + i) % 2:
                order.reverse()
            out += [(r, case, side) for side in order]
    return out


def mean(values):
    values = [v for v in values if v is not None]
    return sum(values) / len(values) if values else None


# Checkpoint flags: (metric, direction, relative change). Skipped refreshes depend on the NVIDIA
# power state (issue #5) and get no flag.
CHECK_RULES = {'fps': (1, 0.10), 'p50': (-1, 0.25), 'p99': (-1, 0.25), 'catch_up': (-1, 0.25),
               'slow': (-1, 1.0)}


def check_key(case, metric):
    """The metrics key a checkpoint column reads: with a cap, slow frames are those over 7.5 ms
    (over 6.94 ms there are always about 1500, docs/rendimiento/ritmo-2026-10.md)."""
    return 'slow75' if metric == 'slow' and case.cap else metric


def values(case, metric, side):
    key = check_key(case, metric)
    return [m.get(key) for m in side]


def worse(metric, base, new):
    """True if `new` (values of the runs) is clearly worse than `base`: the mean moved more than
    the rule allows and no new run reached the worst base run."""
    base = [v for v in base if v is not None]
    new = [v for v in new if v is not None]
    if metric not in CHECK_RULES or not base or not new:
        return False
    direction, rel = CHECK_RULES[metric]
    b, n = mean(base), mean(new)
    if direction > 0:
        return n < b * (1 - rel) and max(new) < min(base)
    if b == 0:
        return min(new) > 0
    return n > b * (1 + rel) and min(new) > max(base)


def flagged_metrics(case, r):
    """The checkpoint rules a case breaks, in CHECK_RULES order."""
    if not r['base']:
        return []
    return [m for m in CHECK_RULES
            if worse(m, values(case, m, r['base']), values(case, m, r['new']))]


def pstates_text(shares):
    shares = [s for s in shares if s]
    if not shares:
        return '-'
    keys = sorted({k for s in shares for k in s}, key=lambda k: int(k[1:]))
    avg = {k: sum(s.get(k, 0.0) for s in shares) / len(shares) for k in keys}
    return ', '.join(f'{k} {v * 100:.0f} %' for k, v in avg.items() if v >= 0.005)


COLUMNS = [('fps', 'fps'), ('p50', 'p50 ms'), ('p99', 'p99 ms'), ('slow', 'Frames lentos'),
           ('skipped', 'Refrescos salteados'), ('catch_up', 'Puesta al día ms')]


def cell(case, metric, base, new):
    if not new:
        return 'sin datos'
    b, n = values(case, metric, base), values(case, metric, new)
    text = fmt(mean(n)) if not base else f'{fmt(mean(b))} -> {fmt(mean(n))}'
    return text + (' **peor**' if base and worse(metric, b, n) else '')


def render_report(info, results):
    """The Spanish report of a checkpoint round. `results`: {case: {'base': [metrics],
    'new': [metrics], 'errors': [text], 'unmeasured': [text]}} in matrix order, only runs that
    count in the metrics lists. `info`: the round (see cmd_check)."""
    first = info['base'] is None
    title = (f"# Ronda fuera de main ({info['date']})" if info.get('off_main')
             else f"# Ronda de control {info['number']} ({info['date']})")
    lines = [title, '']
    new = f"Commit `{info['new'][:8]}`" + (f" (tag `{info['tag']}`)" if info.get('tag') else '')
    if first:
        lines += [f'{new}. Primera ronda: no hay una anterior para comparar, cada celda es la '
                  'media de ahora.']
    else:
        tagged = f" (`{info['base_tag']}`)" if info.get('base_tag') else ''
        lines += [f"{new} contra la ronda anterior, `{info['base']}`{tagged}. Cada celda es la "
                  'media, antes -> ahora.']
    lines += [f"Matriz fija, {info['rounds']} corridas intercaladas de {info['duration']:g} s por "
              'caso y versión. Solo cuentan las corridas sin compilaciones ni otra carga al mismo '
              'tiempo.', '']
    flagged = {c: ms for c, r in results.items() if (ms := flagged_metrics(c, r))}
    failed = info.get('new_failed') or []
    incomplete = info.get('incomplete') or []
    if info.get('off_main'):
        lines += ['**Fuera de main:** no cuenta como control, no mueve la base ni pone tag.', '']
    if failed:
        lines += [f"**El binario nuevo falló:** {'; '.join(failed)}.", '']
    if incomplete:
        text = f"**Ronda incompleta:** faltan corridas de {', '.join(incomplete)} (ver las notas)."
        if info.get('checkpoint'):
            text += ' Se aceptó igual (`--accept-partial`): es la base de la próxima ronda.'
        elif not info.get('off_main'):
            base = f"`{info['base']}`" if info['base'] else 'la anterior'
            text += (f' No cuenta como control: no se puso el tag y la próxima ronda compara otra '
                     f'vez contra {base}.')
        lines += [text, '']
    if flagged:
        case, ms = next(iter(flagged.items()))
        where = f"empeoró en {', '.join(map(str, flagged))}."
        if info.get('base_sha'):
            opts = ' '.join(f'--metric {m}' for m in ms)
            where += (f" Buscar el merge con `just perf-bisect {case} {info['base_sha'][:8]} "
                      f"{info['new'][:8]} {opts}`.")
        lines += [f'**Resultado:** {where}', '']
    elif first:
        lines += ['**Resultado:** primera ronda, sin comparación.', '']
    else:
        partial = ' en lo que se pudo medir' if incomplete or failed else ''
        lines += [f'**Resultado:** sin empeoramientos claros{partial}.', '']
    for gpu in ('rtx', '780m'):
        rows = [(c, r) for c, r in results.items() if c.gpu == gpu]
        if not rows:
            continue
        nvidia = gpu == 'rtx'
        head = ['Caso'] + [name for _, name in COLUMNS] + (['Estado NVIDIA'] if nvidia else [])
        lines += [f'## {runs.GPU_NAMES[gpu]}', '', '| ' + ' | '.join(head) + ' |',
                  '|---|' + '---:|' * len(COLUMNS) + ('---|' if nvidia else '')]
        for case, r in rows:
            cells = [case.label] + [cell(case, m, r['base'], r['new']) for m, _ in COLUMNS]
            if nvidia:
                ps = [pstates_text(x['pstates'] for x in side) for side in (r['base'], r['new'])]
                cells.append(f'{ps[0]} -> {ps[1]}' if r['base'] else ps[1])
            lines.append('| ' + ' | '.join(cells) + ' |')
        lines.append('')
    lines += [
        '## Notas', '',
        '- Frames lentos: más de 6.94 ms sin tope y más de 7.5 ms con tope (con tope el '
        'compositor manda los callbacks cada 6.95 ms, ver [ritmo-2026-10.md](ritmo-2026-10.md)).',
        '- **peor**: la media empeora más de un 10 % (fps) o un 25 % (p50, p99, puesta al día), '
        'o los frames lentos se duplican, y ninguna corrida nueva llega a la peor de antes. Los '
        'refrescos salteados dependen del estado de energía de la NVIDIA (issue #5) y no se '
        'marcan.',
    ]
    if info['skipped']:
        lines.append(f"- Sin datos: {'; '.join(info['skipped'])}.")
    for c, r in results.items():
        for e in r['errors']:
            lines.append(f'- {c} falló: {e}')
        for u in r.get('unmeasured', []):
            lines.append(f'- {c} no cuenta: {u}')
    lines += [f"- Corridas descartadas y repetidas: {info['discarded']} (una compilación, otra "
              'carga o el compositor a 30 fps).']
    if info.get('noisy'):
        lines.append(f"- Corridas que quedaron con ruido tras {runs.ATTEMPTS} intentos y no "
                     f"cuentan: {info['noisy']}.")
    lines += [f"- Datos de cada corrida en `{info['folder']}` (no se suben).", '']
    return '\n'.join(lines)


def report_path(date):
    folder = os.path.join(TREE, 'docs', 'rendimiento')
    path = os.path.join(folder, f'control-{date}.md')
    n = 2
    while os.path.exists(path):
        path = os.path.join(folder, f'control-{date}-{n}.md')
        n += 1
    return path


def cmd_check(a):
    if not a.dry_run:
        no_quiet_slot()
    last = read_last()
    tags = git('tag', '--list', 'perf-control-*').split()
    new_sha = commit(a.ref)
    off_main = not on_main(new_sha)
    if off_main and not a.off_main:
        raise PerfError(f'{a.ref} ({new_sha[:8]}) is not on main: a checkpoint round measures '
                        'main (run it where you integrate). --off-main measures it anyway, '
                        'without a tag and without moving the last checkpoint')
    last_sha, last_tag = last_checkpoint(last, tags)
    base_spec = a.base or last_sha
    number = next_number(tags, last)
    tag = f'perf-control-{number}'
    date = datetime.date.today().isoformat()
    folder = bench('control', f'{number}-{stamp()}')
    base_cached = base_spec and (os.path.isfile(base_spec) or os.path.exists(
        bench('bins', commit(base_spec), 'stress')))
    sides = ['base', 'new'] if base_spec else ['new']
    est = a.rounds * len(sides) * sum(runs.estimate_s(c, a.duration) for c in MATRIX)
    say(f'perf-check: round {number} ({tag}), {len(MATRIX)} cases x {a.rounds} rounds x '
        f'{len(sides)} sides of {a.duration:g} s: about {minutes(est)} '
        f'({len(MATRIX) * a.rounds * len(sides)} runs, more with repeats)')
    if base_spec:
        where = 'cached' if base_cached else 'not cached: it will be built'
        if a.base:
            origin = 'from --base'
        elif last:
            origin = f'the last checkpoint, {last_tag}'
        else:
            origin = f'the last checkpoint, {last_tag} (bench-results/control/last is missing)'
        say(f'  base: {base_spec} ({origin}; {where})')
    else:
        say('  base: none (first checkpoint round: the new binary alone)')
    say(f'  new: {new_sha[:8]} {subject(new_sha)}')
    for case in MATRIX:
        note = ' (only if the binaries have it)' if runs.SCENARIOS[case.scenario].needs else ''
        say(f'  {case}: {case.label}{note}')
    say(f'  runs: {folder}')
    if off_main:
        say(f'  off main: the report stays in {folder}, no tag, the last checkpoint stays')
    else:
        say(f'  report: {os.path.relpath(report_path(date), TREE)}; if every run counts (or '
            f'--accept-partial), it becomes the next base' +
            (' (no tag: --no-tag)' if a.no_tag else f' and {new_sha[:8]} gets the tag {tag}'))
    if a.dry_run:
        say('perf-check: dry run, nothing measured')
        return 0
    base = resolve(base_spec, 'checkpoint') if base_spec else None
    new = commit_binary(new_sha, a.ref)
    binaries = {'base': base, 'new': new}
    support, skipped, failed = {}, [], []
    for case in MATRIX:
        has = [s for s in sides if runs.supports(binaries[s].path, case)]
        if not has:
            skipped.append(f'{case} (los binarios no tienen el escenario)')
        elif has == ['base']:
            failed.append(f'{case} (el binario nuevo ya no tiene el escenario)')
        else:
            if len(has) < len(sides):
                skipped.append(f'{case} solo ahora (el binario de antes no tiene el escenario)')
            support[case] = has
    cases = [c for c in MATRIX if c in support]
    results = {c: {'base': [], 'new': [], 'errors': [], 'unmeasured': []} for c in cases}
    base_ran, new_errors = set(), {}
    plan = schedule(cases, support, a.rounds)
    discarded = noisy = 0
    gave_up = False
    for i, (r, case, side) in enumerate(plan, 1):
        if gave_up:
            results[case]['unmeasured'].append(f'{side}, ronda {r + 1}: no se corrió (la máquina '
                                               'nunca quedó tranquila)')
            continue
        say(f'[{i}/{len(plan)}] round {r + 1} {case} {side}')
        prefix = os.path.join(folder, side, f'{case.slug}-{r + 1}')
        os.makedirs(os.path.dirname(prefix), exist_ok=True)
        run = runs.measure(binaries[side].path, case, prefix, f'{side}-{binaries[side].label}',
                           a.duration, nvidia=case.gpu == 'rtx', say=say)
        discarded += len(run.discarded)
        say(run_line(side, run))
        if side == 'base' and run.ran:
            base_ran.add(case)
        if run.counts:
            results[case][side].append(run.metrics)
        elif run.error:
            results[case]['errors'].append(f'{side}, ronda {r + 1}: {run.error}')
            if side == 'new':
                new_errors.setdefault(case, run.error)
        else:
            noisy += 0 if run.gave_up else 1
            results[case]['unmeasured'].append(f'{side}, ronda {r + 1}: {run.noisy}')
        gave_up = gave_up or run.gave_up
    # A new binary that fails where the old one ran is its own fault; failing on both sides (no
    # GPU, say) is the machine's, and only leaves the case incomplete.
    failed += [f'{c} ({e})' for c, e in new_errors.items() if c in base_ran]
    incomplete = [str(c) for c in cases
                  if any(len(results[c][s]) < a.rounds for s in support[c])]
    flagged = [c for c, r in results.items() if flagged_metrics(c, r)]
    complete = not incomplete and not failed
    checkpoint = not off_main and (complete or a.accept_partial)
    tagged = checkpoint and not a.no_tag
    info = {'number': number, 'date': date, 'tag': tag if tagged else None, 'new': new_sha,
            'base': (base.sha[:8] if base.sha else base.path) if base else None,
            'base_sha': base.sha if base else None,
            'base_tag': last_tag if base and base.sha and base.sha == last_sha else None,
            'rounds': a.rounds, 'duration': a.duration, 'skipped': skipped,
            'discarded': discarded, 'noisy': noisy, 'folder': os.path.relpath(folder, root()),
            'new_failed': failed, 'incomplete': incomplete, 'checkpoint': checkpoint,
            'off_main': off_main}
    os.makedirs(folder, exist_ok=True)
    report = os.path.join(folder, 'report.md') if off_main else report_path(date)
    with open(report, 'w', encoding='utf-8') as f:
        f.write(render_report(info, results))
    with open(os.path.join(folder, 'results.json'), 'w') as f:
        json.dump({'info': info, 'results': {str(c): r for c, r in results.items()}}, f,
                  indent=1)
    if checkpoint:
        with open(bench('control', 'last'), 'w') as f:
            json.dump({'number': number, 'tag': tag, 'sha': new_sha, 'date': date,
                       'binary': new.path, 'folder': folder, 'report': report}, f, indent=1)
    if tagged:
        git('tag', tag, new_sha)
    say(f'perf-check: wrote {os.path.relpath(report, TREE)}'
        + (f', tagged {new_sha[:8]} {tag} (local; push tags yourself)' if tagged else ''))
    if not checkpoint and not off_main:
        say('perf-check: INCOMPLETE round: not a checkpoint (no tag, the base stays); '
            '--accept-partial makes it one anyway')
    if failed or flagged:
        say(f"perf-check: WORSE or FAILED: {', '.join([*map(str, flagged), *failed])}")
        return 1
    return 2 if incomplete else 0


# ---------------------------------------------------------------------------------------------
# Bisect

def first_bad(count, is_bad):
    """Index of the first bad item of `count` items, the last one bad and everything before the
    first good, by halves (about log2(count) calls of is_bad)."""
    lo, hi = -1, count - 1
    while hi - lo > 1:
        mid = (lo + hi) // 2
        if is_bad(mid):
            hi = mid
        else:
            lo = mid
    return hi


def metric_bad(case, base, new, folder, metrics, rounds, duration):
    """The checkpoint rules of `metrics` on `rounds` interleaved pairs: True if `new` is worse or
    failed where the base ran. PerfError when a run does not count."""
    if not runs.supports(base.path, case):
        raise PerfError(f'{base} has no {case.scenario} scenario')
    if not runs.supports(new.path, case):
        say(f'    FAILED: {new} has no {case.scenario} scenario and the base has it')
        return True
    pairs = []
    for r in range(rounds):
        b, n = run_pair(case, base, new, folder, f'-{r + 1}', r % 2 == 1, duration)
        if new_failed(b, n):
            say(f'    FAILED: new run {n.error}')
            return True
        if not (b and b.counts and n and n.counts):
            raise PerfError(f'could not measure {new}: {unmeasured_reason([(b, n)])}')
        pairs.append((b.metrics, n.metrics))
    bad = False
    for m in metrics:
        b, n = values(case, m, [p[0] for p in pairs]), values(case, m, [p[1] for p in pairs])
        is_worse = worse(m, b, n)
        bad = bad or is_worse
        say(f"    {m}: {fmt(mean(b))} -> {fmt(mean(n))}{' WORSE' if is_worse else ''}")
    return bad


def cmd_bisect(a):
    no_quiet_slot()
    case = runs.parse_case(a.case, Case(extra=tuple(a.extra)))
    good_sha, bad_sha = commit(a.good), commit(a.bad)
    commits = git('rev-list', '--first-parent', '--reverse', f'{good_sha}..{bad_sha}').split()
    if not commits:
        raise PerfError(f'no first-parent commits in {a.good}..{a.bad}')
    duration = a.duration or (CHECK_S if a.metric else SMOKE_S)
    pairs = a.rounds if a.metric else 1
    rule = (f"the checkpoint rule of {', '.join(a.metric)} on {pairs} runs of {duration:g} s per "
            'side' if a.metric else 'the smoke check')
    steps = math.ceil(math.log2(len(commits))) + 1 if len(commits) > 1 else 1
    say(f'perf-bisect: {case} on {len(commits)} first-parent commits of {a.good}..{a.bad} with '
        f'{rule}, about {steps} steps (each one a build and about '
        f'{minutes(2 * pairs * runs.estimate_s(case, duration))} of runs)')
    good = commit_binary(good_sha, a.good)
    folder = bench('bisect', stamp())
    verdicts = {}

    def is_bad(i):
        if i not in verdicts:
            sha = commits[i]
            say(f'perf-bisect: testing {subject(sha)}')
            new = commit_binary(sha)
            where = os.path.join(folder, f'{i:03d}-{sha[:8]}')
            if a.metric:
                verdicts[i] = metric_bad(case, good, new, where, a.metric, a.rounds, duration)
            else:
                result = smoke_case(case, good, new, where, duration, fps_min=a.fps_min,
                                    p99_max=a.p99_max)
                if result.status in ('unmeasured', 'skipped'):
                    raise PerfError(f'could not measure {sha[:8]}: {result.verdict()}')
                verdicts[i] = result.status in ('worse', 'failed')
        return verdicts[i]

    if not is_bad(len(commits) - 1):
        hint = ('' if a.metric else ' (--metric M uses the checkpoint rule of what the report '
                'flagged; --fps-min 0.95 or --p99-max 1.2 look for smaller changes)')
        say(f'perf-bisect: {a.bad} is not worse than {a.good} on {case} with {rule}; nothing to '
            f'look for{hint}')
        return 2
    found = commits[first_bad(len(commits), is_bad)]
    say(f'perf-bisect: first bad commit: {subject(found)}')
    return 1


def cmd_cases(_):
    say('scenarios (case: gpu/scenario/rN/cap, e.g. rtx/surface/r16/uncapped; caps 0 or '
        f'{runs.MIN_CAP}+):')
    for name, s in runs.SCENARIOS.items():
        say(f"  {name:<12} --scenario {s.stress} {' '.join(s.args)}  ({s.label})")
    for name, args in runs.GPUS.items():
        say(f"gpu {name}: {' '.join(args)}")
    say(f"smoke default: {', '.join(DEFAULT_SMOKE)} (rtx, r16, uncapped); pick the cases your "
        'task touches. Stress options after `--` go to both sides')
    say('checkpoint matrix:')
    for case in MATRIX:
        say(f'  {case}')
    return 0


def parser():
    p = argparse.ArgumentParser(prog='perf.py', description=__doc__.split('\n\n')[0])
    sub = p.add_subparsers(dest='command', required=True)
    thresholds = argparse.ArgumentParser(add_help=False)
    thresholds.add_argument('--fps-min', type=float, default=FPS_MIN,
                            help='fail under this share of the base fps (default 0.85)')
    thresholds.add_argument('--p99-max', type=float, default=P99_MAX,
                            help='fail over this multiple of the base p99 (default 1.5)')
    s = sub.add_parser('smoke', parents=[thresholds], help='level 1: base against this tree')
    s.add_argument('cases', nargs='*', help=f"default: {' '.join(DEFAULT_SMOKE)}")
    s.add_argument('--base', help='git ref or stress binary (default: merge base with main)')
    s.add_argument('--new', help='git ref or stress binary (default: this tree as it is)')
    s.add_argument('--gpu', choices=sorted(runs.GPUS), default='rtx')
    s.add_argument('--radius', type=int, default=16)
    s.add_argument('--cap', type=int, default=0, help='fps cap (default 0: uncapped)')
    s.add_argument('--duration', type=float, default=SMOKE_S)
    s.add_argument('--nvidia', action='store_true', help='sample the NVIDIA state (read only)')
    c = sub.add_parser('check', help='level 2: the checkpoint round')
    c.add_argument('--base', help='git ref or stress binary (default: the last checkpoint)')
    c.add_argument('--ref', default='HEAD', help='commit of main to measure (default HEAD)')
    c.add_argument('--rounds', type=int, default=CHECK_ROUNDS)
    c.add_argument('--duration', type=float, default=CHECK_S)
    c.add_argument('--dry-run', action='store_true', help='print the plan only')
    c.add_argument('--no-tag', action='store_true', help='do not tag the commit')
    c.add_argument('--accept-partial', action='store_true',
                   help='make an incomplete round the next base anyway')
    c.add_argument('--off-main', action='store_true',
                   help='measure a commit off main: no tag, the last checkpoint stays')
    b = sub.add_parser('bisect', parents=[thresholds], help='first bad merge of a case')
    b.add_argument('case')
    b.add_argument('good')
    b.add_argument('bad')
    b.add_argument('--metric', action='append', choices=list(CHECK_RULES),
                   help='judge with the checkpoint rule of this metric (repeatable)')
    b.add_argument('--rounds', type=int, default=CHECK_ROUNDS,
                   help='pairs per commit with --metric (default 3)')
    b.add_argument('--duration', type=float,
                   help=f'seconds per run (default {SMOKE_S}, {CHECK_S} with --metric)')
    sub.add_parser('cases', help='scenarios and the checkpoint matrix')
    return p


def main(argv):
    argv = list(argv)
    extra = []
    if '--' in argv:
        i = argv.index('--')
        argv, extra = argv[:i], argv[i + 1:]
    try:
        a = parser().parse_args(argv)
    except SystemExit as e:
        return 64 if e.code else 0
    a.extra = extra
    try:
        if extra and a.command not in ('smoke', 'bisect'):
            raise ValueError(f'only smoke and bisect take stress options after --, not '
                             f'{a.command}')
        return {'smoke': cmd_smoke, 'check': cmd_check, 'bisect': cmd_bisect,
                'cases': cmd_cases}[a.command](a)
    except ValueError as e:
        print(f'perf: {e}', file=sys.stderr)
        return 64
    except PerfError as e:
        print(f'perf: {e}', file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        return 130


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
