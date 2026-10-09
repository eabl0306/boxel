#!/usr/bin/env python3
"""boxel performance checks: the smoke check of each task, the checkpoint round and the bisect.

usage:
  perf.py smoke [--base REF|BIN] [--new REF|BIN] [--gpu G] [--radius N] [--cap N] [CASE...]
  perf.py check [--base REF|BIN] [--ref REF] [--rounds N] [--dry-run] [--no-tag]
  perf.py bisect CASE GOOD BAD
  perf.py cases

smoke (level 1, `just perf-smoke`): one 10 s stress run per side of each case, base against the
new binary, on a quiet machine. The base is a git ref (default: the merge base of HEAD and main)
or a binary; its binary comes from bench-results/bins/<sha>/stress, built from a git worktree of
the base when missing. The new side is this checkout as it is (built in place), or --new. A case
fails only on a gross regression: fps under 85 % of the base, p99 over 1.5 times the base, or
slow frames where the base had none. A failing pair is repeated once and only a second failure
counts. Cases: surface and forest by default, on the RTX, radius 16, uncapped.

check (level 2, `just perf-check`): the fixed matrix (`perf.py cases`), 3 interleaved runs of
20 s per case and side against the binary of the last checkpoint (bench-results/control/last).
Writes docs/rendimiento/control-YYYY-MM-DD.md and tags the commit perf-control-N (local).
--dry-run only prints the plan.

bisect (`just perf-bisect CASE GOOD BAD`): the smoke check of CASE against GOOD on the
first-parent commits (the merges) of GOOD..BAD, by halves, to find the first bad one.

Slow frames are those over 6.94 ms uncapped and over 7.5 ms capped (runs.metrics). Builds go
through tools/quiet.sh; PERF_TARGET_DIR (default <repo>/target-perf) holds the base worktree and
its target dir. Exit codes: 0 ok, 1 gross regression confirmed or the new binary failed, 2 could
not measure (a build or the base failed, the machine never got quiet), 64 usage error.
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


class PerfError(Exception):
    """Something to measure could not be set up (exit 2)."""


def say(text=''):
    print(text, flush=True)


def git(*args, cwd=TREE):
    done = subprocess.run(['git', *args], cwd=cwd, capture_output=True, text=True)
    if done.returncode != 0:
        raise PerfError(f'git {" ".join(args)}: {done.stderr.strip()}')
    return done.stdout.strip()


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


def install(built, dest):
    """Copies a fresh binary into the cache (cargo hard-links its outputs, and a later build
    would change the file under a running benchmark)."""
    os.makedirs(os.path.dirname(dest), exist_ok=True)
    shutil.copy2(built, dest + '.tmp')
    os.replace(dest + '.tmp', dest)


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
                if os.path.exists(os.path.join(src, '.git')):
                    git('checkout', '--quiet', '--detach', '--force', sha, cwd=src)
                else:
                    git('worktree', 'prune')
                    git('worktree', 'add', '--quiet', '--detach', src, sha)
                built = cargo_build(src, folder, bench('bins', f'build-{sha[:8]}.log'))
                install(built, dest)
    return Binary(dest, sha[:8], sha, name)


def tree_binary():
    """This checkout as it is: the cached binary of HEAD when the tree is clean, else built in
    place (CARGO_TARGET_DIR or target/) and copied to the cache."""
    sha = git('rev-parse', 'HEAD')
    dirty = subprocess.run(['git', 'diff', '--quiet', 'HEAD'], cwd=TREE).returncode != 0
    dest = bench('bins', sha, 'stress')
    if not dirty and os.path.exists(dest):
        return Binary(dest, sha[:8], sha, 'this tree')
    target = os.path.join(TREE, os.environ.get('CARGO_TARGET_DIR') or 'target')
    name = os.path.basename(TREE)
    built = cargo_build(TREE, target, bench('bins', f'build-{name}.log'))
    if dirty:
        dest = bench('bins', f'dirty-{name}', 'stress')
    install(built, dest)
    label = f'{sha[:8]}-dirty-{md5(dest)}' if dirty else sha[:8]
    return Binary(dest, label, None if dirty else sha, 'this tree')


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
    """Gross regressions of `new` against `base` (metrics of one run each): the reasons."""
    out = []
    if base.get('fps') and new.get('fps') is not None and new['fps'] < fps_min * base['fps']:
        out.append(f"fps {fmt(new['fps'])} under {fps_min:.0%} of {fmt(base['fps'])}")
    if base.get('p99') and new.get('p99') is not None and new['p99'] > p99_max * base['p99']:
        out.append(f"p99 {fmt(new['p99'])} ms over {p99_max:g}x {fmt(base['p99'])} ms")
    if base.get('slow') == 0 and new.get('slow'):
        out.append(f"{new['slow']} slow frames, the base had none")
    return out


@dataclasses.dataclass
class SmokeResult:
    case: Case
    pairs: list = dataclasses.field(default_factory=list)
    problems: list = dataclasses.field(default_factory=list)
    skipped: str = None

    @property
    def status(self):
        """skipped, failed (the new binary), broken (the base), worse (confirmed) or ok."""
        if self.skipped:
            return 'skipped'
        if any(n.error for _, n in self.pairs):
            return 'failed'
        if any(b.error for b, _ in self.pairs):
            return 'broken'
        if len(self.problems) == 2 and all(self.problems):
            return 'worse'
        return 'ok'

    def verdict(self):
        status = self.status
        if status == 'skipped':
            return f'skipped: {self.skipped}'
        if status == 'failed':
            return 'FAILED: new run ' + next(n.error for _, n in self.pairs if n.error)
        if status == 'broken':
            return 'not measured: base run ' + next(b.error for b, _ in self.pairs if b.error)
        text = 'WORSE: ' + '; '.join(self.problems[-1]) if status == 'worse' else 'ok'
        if status == 'ok' and len(self.problems) == 2:
            text += ' (the first pair looked worse, the repeat did not)'
        noisy = [r.noisy for pair in self.pairs for r in pair if r.noisy]
        if noisy:
            text += f' (noisy: {noisy[0]})'
        return text


def fmt(value):
    return bench_report.fmt(value)


def run_line(side, run):
    if run.error:
        detail = ''.join(f'\n        {line}' for line in run.detail.splitlines())
        return f'    {side:<4} {run.error}{detail}'
    m = run.metrics
    return (f"    {side:<4} {fmt(m['fps'])} fps, p50 {fmt(m['p50'])} ms, p99 {fmt(m['p99'])} ms, "
            f"slow {fmt(m['slow'])} ({run.seconds:.0f} s)")


def run_pair(case, base, new, folder, tag, flip, duration, nvidia=False):
    order = [('base', base), ('new', new)]
    if flip:
        order.reverse()
    done = {}
    for side, binary in order:
        prefix = os.path.join(folder, side, case.slug + tag)
        os.makedirs(os.path.dirname(prefix), exist_ok=True)
        done[side] = runs.measure(binary.path, case, prefix, f'{side}-{binary.label}', duration,
                                  nvidia=nvidia, say=say)
        say(run_line(side, done[side]))
    return done['base'], done['new']


def smoke_case(case, base, new, folder, duration=SMOKE_S, flip=False, fps_min=FPS_MIN,
               p99_max=P99_MAX, nvidia=False):
    """The smoke check of one case: a pair of runs, and a second pair when the first looks
    grossly worse."""
    result = SmokeResult(case)
    missing = [b for b in (base, new) if not runs.supports(b.path, case)]
    if missing:
        result.skipped = f'{missing[0]} has no {case.scenario} scenario'
        say(f'  {case}: {result.skipped}')
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
    if (case.scenario.endswith('static') and b.metrics and n.metrics
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
    """Markdown table of the last pair of each case (ready for a PR description)."""
    lines = ['| case | fps | p99 ms | slow frames | verdict |', '|---|---:|---:|---:|---|']
    for r in results:
        if not r.pairs or not r.pairs[-1][0].metrics or not r.pairs[-1][1].metrics:
            lines.append(f'| {r.case} | - | - | - | {r.verdict()} |')
            continue
        b, n = (x.metrics for x in r.pairs[-1])
        lines.append(f"| {r.case} | {change(b['fps'], n['fps'])} | {fmt(b['p99'])} -> "
                     f"{fmt(n['p99'])} | {fmt(b['slow'])} -> {fmt(n['slow'])} | {r.verdict()} |")
    return '\n'.join(lines)


def exit_code(results):
    statuses = {r.status for r in results}
    if statuses & {'worse', 'failed'}:
        return 1
    return 2 if 'broken' in statuses else 0


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
    default = Case(gpu=a.gpu, radius=a.radius, cap=a.cap)
    cases = [runs.parse_case(text, default) for text in (a.cases or DEFAULT_SMOKE)]
    runs_s = sum(2 * runs.estimate_s(c, a.duration) for c in cases)
    say(f'perf-smoke: {len(cases)} cases, {2 * len(cases)} runs of {a.duration:g} s, about '
        f'{minutes(runs_s)} of runs (more with builds or repeats)')
    base = resolve(a.base or merge_base(), a.base or 'merge base')
    new = resolve(a.new)
    say(f'perf-smoke: base {base} (md5 {md5(base.path)}), new {new} (md5 {md5(new.path)})')
    folder = bench('smoke', stamp())
    results = [smoke_case(c, base, new, folder, a.duration, i % 2 == 1, a.fps_min, a.p99_max,
                          a.nvidia)
               for i, c in enumerate(cases)]
    code = exit_code(results)
    say()
    say(smoke_table(results))
    say()
    verdict = {0: 'ok, no gross regression', 1: 'GROSS REGRESSION or a failed run',
               2: 'could not measure everything'}[code]
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


def next_number(tags, last=None):
    numbers = [int(t.rsplit('-', 1)[1]) for t in tags if t.rsplit('-', 1)[-1].isdigit()]
    if last and isinstance(last.get('number'), int):
        numbers.append(last['number'])
    return max(numbers, default=0) + 1


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


def pstates_text(shares):
    shares = [s for s in shares if s]
    if not shares:
        return '-'
    keys = sorted({k for s in shares for k in s}, key=lambda k: int(k[1:]))
    avg = {k: sum(s.get(k, 0.0) for s in shares) / len(shares) for k in keys}
    return ', '.join(f'{k} {v * 100:.0f} %' for k, v in avg.items() if v >= 0.005)


COLUMNS = [('fps', 'fps'), ('p50', 'p50 ms'), ('p99', 'p99 ms'), ('slow', 'Frames lentos'),
           ('skipped', 'Refrescos salteados'), ('catch_up', 'Puesta al día ms')]


def cell(metric, base, new):
    b = [m[metric] for m in base]
    n = [m[metric] for m in new]
    if not new:
        return 'falló'
    text = fmt(mean(n)) if not base else f'{fmt(mean(b))} -> {fmt(mean(n))}'
    return text + (' **peor**' if base and worse(metric, b, n) else '')


def render_report(info, results):
    """The Spanish report of a checkpoint round. `results`: {case: {'base': [metrics],
    'new': [metrics], 'errors': [text]}} in matrix order."""
    first = info['base'] is None
    lines = [f"# Ronda de control {info['number']} ({info['date']})", '']
    new = f"Commit `{info['new'][:8]}` (tag `{info['tag']}`)"
    if first:
        lines += [f'{new}. Primera ronda: no hay una anterior para comparar, cada celda es la '
                  'media de ahora.']
    else:
        tagged = f" (`{info['base_tag']}`)" if info.get('base_tag') else ''
        lines += [f"{new} contra la ronda anterior, `{info['base']}`{tagged}. Cada celda es la "
                  'media, antes -> ahora.']
    lines += [f"Matriz fija, {info['rounds']} corridas intercaladas de {info['duration']:g} s por "
              'caso y versión, sin compilaciones ni otra carga al mismo tiempo.', '']
    flagged = [str(c) for c, r in results.items()
               if r['base'] and any(worse(m, [x[m] for x in r['base']], [x[m] for x in r['new']])
                                    for m in CHECK_RULES)]
    failed = [str(c) for c, r in results.items() if r['errors']]
    if flagged:
        lines += [f"**Resultado:** empeoró en {', '.join(flagged)}. Buscar el merge con "
                  f"`just perf-bisect {flagged[0]} {info['base']} {info['new'][:8]}`.", '']
    else:
        lines += ['**Resultado:** sin empeoramientos claros.', '']
    if failed:
        lines += [f"**Fallaron:** {', '.join(failed)} (ver las notas).", '']
    for gpu in ('rtx', '780m'):
        rows = [(c, r) for c, r in results.items() if c.gpu == gpu]
        if not rows:
            continue
        nvidia = gpu == 'rtx'
        head = ['Caso'] + [name for _, name in COLUMNS] + (['Estado NVIDIA'] if nvidia else [])
        lines += [f'## {runs.GPU_NAMES[gpu]}', '', '| ' + ' | '.join(head) + ' |',
                  '|---|' + '---:|' * len(COLUMNS) + ('---|' if nvidia else '')]
        for case, r in rows:
            cells = [case.label] + [cell(m, r['base'], r['new']) for m, _ in COLUMNS]
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
    lines += [f"- Corridas descartadas y repetidas: {info['discarded']} (una compilación, otra "
              'carga o el compositor a 30 fps).']
    if info.get('noisy'):
        lines.append(f"- Corridas que quedaron con ruido tras {runs.ATTEMPTS} intentos: "
                     f"{info['noisy']}.")
    lines += [
              f"- Datos de cada corrida en `{info['folder']}` (no se suben).", '']
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
    last = read_last()
    base_spec = a.base or (last or {}).get('sha')
    new_sha = commit(a.ref)
    number = next_number(git('tag', '--list', 'perf-control-*').split(), last)
    tag = f'perf-control-{number}'
    date = datetime.date.today().isoformat()
    folder = bench('control', f'{number}-{date}')
    base_cached = base_spec and (os.path.isfile(base_spec) or os.path.exists(
        bench('bins', commit(base_spec), 'stress')))
    sides = ['base', 'new'] if base_spec else ['new']
    est = a.rounds * len(sides) * sum(runs.estimate_s(c, a.duration) for c in MATRIX)
    say(f'perf-check: round {number} ({tag}), {len(MATRIX)} cases x {a.rounds} rounds x '
        f'{len(sides)} sides of {a.duration:g} s: about {minutes(est)} '
        f'({len(MATRIX) * a.rounds * len(sides)} runs, more with repeats)')
    if base_spec:
        where = 'cached' if base_cached else 'not cached: it will be built'
        origin = 'from --base' if a.base else f"the last checkpoint, {last.get('tag')}"
        say(f'  base: {base_spec} ({origin}; {where})')
    else:
        say('  base: none (first checkpoint round: the new binary alone)')
    say(f'  new: {new_sha[:8]} {subject(new_sha)}')
    for case in MATRIX:
        note = ' (only if the binaries have it)' if runs.SCENARIOS[case.scenario].needs else ''
        say(f'  {case}: {case.label}{note}')
    say(f'  runs: {folder}')
    say(f'  report: {os.path.relpath(report_path(date), TREE)}, then tag {tag} on '
        f'{new_sha[:8]}' + (' (no tag: --no-tag)' if a.no_tag else ''))
    if a.dry_run:
        say('perf-check: dry run, nothing measured')
        return 0
    base = resolve(base_spec, 'checkpoint') if base_spec else None
    new = commit_binary(new_sha, a.ref)
    binaries = {'base': base, 'new': new}
    support, skipped = {}, []
    for case in MATRIX:
        support[case] = [s for s in sides if runs.supports(binaries[s].path, case)]
        if not support[case]:
            skipped.append(f'{case} (los binarios no tienen el escenario)')
        elif len(support[case]) < len(sides):
            skipped.append(f'{case} solo ahora (el binario de antes no tiene el escenario)')
    cases = [c for c in MATRIX if support[c]]
    results = {c: {'base': [], 'new': [], 'errors': []} for c in cases}
    plan = schedule(cases, support, a.rounds)
    discarded = noisy = 0
    for i, (r, case, side) in enumerate(plan, 1):
        say(f'[{i}/{len(plan)}] round {r + 1} {case} {side}')
        prefix = os.path.join(folder, side, f'{case.slug}-{r + 1}')
        os.makedirs(os.path.dirname(prefix), exist_ok=True)
        run = runs.measure(binaries[side].path, case, prefix, f'{side}-{binaries[side].label}',
                           a.duration, nvidia=case.gpu == 'rtx', say=say)
        discarded += len(run.discarded) - (1 if run.noisy else 0)
        noisy += 1 if run.noisy else 0
        say(run_line(side, run))
        if run.error:
            results[case]['errors'].append(f'{side}, ronda {r + 1}: {run.error}')
        else:
            results[case][side].append(run.metrics)
    info = {'number': number, 'date': date, 'tag': tag, 'new': new_sha,
            'base': (base.sha[:8] if base.sha else base.path) if base else None,
            'base_tag': last.get('tag') if base and last and base.sha == last.get('sha') else None,
            'rounds': a.rounds, 'duration': a.duration, 'skipped': skipped,
            'discarded': discarded, 'noisy': noisy, 'folder': os.path.relpath(folder, root())}
    os.makedirs(folder, exist_ok=True)
    report = report_path(date)
    with open(report, 'w', encoding='utf-8') as f:
        f.write(render_report(info, results))
    with open(os.path.join(folder, 'results.json'), 'w') as f:
        json.dump({'info': info, 'results': {str(c): r for c, r in results.items()}}, f,
                  indent=1)
    with open(bench('control', 'last'), 'w') as f:
        json.dump({'number': number, 'tag': tag, 'sha': new_sha, 'date': date,
                   'binary': new.path, 'folder': folder, 'report': report}, f, indent=1)
    if not a.no_tag:
        git('tag', tag, new_sha)
    say(f'perf-check: wrote {os.path.relpath(report, TREE)}'
        + ('' if a.no_tag else f', tagged {new_sha[:8]} {tag} (local; push tags yourself)'))
    return 0


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


def cmd_bisect(a):
    case = runs.parse_case(a.case)
    good_sha, bad_sha = commit(a.good), commit(a.bad)
    commits = git('rev-list', '--first-parent', '--reverse', f'{good_sha}..{bad_sha}').split()
    if not commits:
        raise PerfError(f'no first-parent commits in {a.good}..{a.bad}')
    steps = math.ceil(math.log2(len(commits))) + 1 if len(commits) > 1 else 1
    say(f'perf-bisect: {case} on {len(commits)} first-parent commits of {a.good}..{a.bad}, '
        f'about {steps} smoke checks (each one a build and about '
        f'{minutes(2 * runs.estimate_s(case, a.duration))} of runs)')
    good = commit_binary(good_sha, a.good)
    folder = bench('bisect', stamp())
    verdicts = {}

    def is_bad(i):
        if i not in verdicts:
            sha = commits[i]
            say(f'perf-bisect: testing {subject(sha)}')
            new = commit_binary(sha)
            result = smoke_case(case, good, new, os.path.join(folder, f'{i:03d}-{sha[:8]}'),
                                a.duration, fps_min=a.fps_min, p99_max=a.p99_max)
            if result.status in ('broken', 'skipped'):
                raise PerfError(f'could not measure {sha[:8]}: {result.verdict()}')
            verdicts[i] = result.status in ('worse', 'failed')
        return verdicts[i]

    if not is_bad(len(commits) - 1):
        say(f'perf-bisect: {a.bad} is not grossly worse than {a.good} on {case} with the smoke '
            'check; nothing to look for (try --fps-min 0.95 or --p99-max 1.2 for smaller changes)')
        return 2
    found = commits[first_bad(len(commits), is_bad)]
    say(f'perf-bisect: first bad commit: {subject(found)}')
    return 1


def cmd_cases(_):
    say('scenarios (case: gpu/scenario/rN/cap, e.g. rtx/surface/r16/uncapped):')
    for name, s in runs.SCENARIOS.items():
        say(f"  {name:<12} --scenario {s.stress} {' '.join(s.args)}  ({s.label})")
    for name, args in runs.GPUS.items():
        say(f"gpu {name}: {' '.join(args)}")
    say(f"smoke default: {', '.join(DEFAULT_SMOKE)} (rtx, r16, uncapped)")
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
    c.add_argument('--ref', default='HEAD', help='commit to measure (default HEAD)')
    c.add_argument('--rounds', type=int, default=CHECK_ROUNDS)
    c.add_argument('--duration', type=float, default=CHECK_S)
    c.add_argument('--dry-run', action='store_true', help='print the plan only')
    c.add_argument('--no-tag', action='store_true', help='do not tag the commit')
    b = sub.add_parser('bisect', parents=[thresholds], help='first bad merge of a case')
    b.add_argument('case')
    b.add_argument('good')
    b.add_argument('bad')
    b.add_argument('--duration', type=float, default=SMOKE_S)
    sub.add_parser('cases', help='scenarios and the checkpoint matrix')
    return p


def main(argv):
    try:
        a = parser().parse_args(argv)
    except SystemExit as e:
        return 64 if e.code else 0
    try:
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
