"""Stress cases, one watched run of examples/stress and the numbers the verdicts use.

A case is a GPU, a scenario, a radius and an fps cap, written `rtx/surface/r16/uncapped`
(`gpu/scenario/rN/cap`; any order, missing parts take the defaults, so `surface` or
`780m/forest/144` work too). `measure` runs a case on a quiet machine and redoes the runs that do
not count: they overlapped a build or another benchmark, the rest of the machine was busy, or the
compositor throttled the window to 30 fps (a second under 100 fps or under 50 frames shown).
"""
import csv
import dataclasses
import json
import os
import re
import shutil
import statistics
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bench_report  # noqa: E402

import machine  # noqa: E402

GPUS = {
    'rtx': ['--adapter', 'nvidia', '--backend', 'vulkan'],
    '780m': ['--adapter', '780m', '--backend', 'vulkan'],
}
GPU_NAMES = {'rtx': 'RTX 5050', '780m': 'Radeon 780M'}

OVERWORLD = ['--world', 'overworld', '--caves', 'mc']
CAVE_CAMERA = ['--camera', '656,-1000,16,-90,0']
# The binary must match these in its --help to run the scenario (cave runs came later).
CAVE_FLAG = r'^\s*--cave\s'
PATH_SCENARIO = r'--scenario \S*\bpath\b'


@dataclasses.dataclass(frozen=True)
class Scenario:
    stress: str
    args: tuple
    label: str
    needs: tuple = ()


SCENARIOS = {
    'surface': Scenario('fly', (*OVERWORLD, '--speed', '64'), 'superficie, vuelo a 64'),
    'forest': Scenario('fly', ('--speed', '32'), 'bosque, vuelo a 32'),
    'dive': Scenario('dive', (*OVERWORLD, '--speed', '64', '--camera', '16,-1000,16,-90,0'),
                     'bajada a 64 desde y = -1000'),
    'cave-path': Scenario('path', (*OVERWORLD, '--cave', *CAVE_CAMERA, '--speed', '8'),
                          'cueva abierta, ruta a 8', (CAVE_FLAG, PATH_SCENARIO)),
    'cave-static': Scenario('static', (*OVERWORLD, '--cave', *CAVE_CAMERA),
                            'cueva abierta, quieta', (CAVE_FLAG,)),
}

# Over this radius the overworld needs a bigger renderer arena and chunk table.
BIG_RADIUS = 16
BIG_ARENA = ['--arena-quads', '50331648', '--chunk-slots', '98304']
# A second under this fps (or with fewer frames shown) is the compositor's 30 fps throttle.
THROTTLE_FPS = 100
THROTTLE_SHOWN = 50
# Tries of a run before keeping one that does not count (marked noisy).
ATTEMPTS = 4


@dataclasses.dataclass(frozen=True)
class Case:
    gpu: str = 'rtx'
    scenario: str = 'surface'
    radius: int = 16
    cap: int = 0

    def __str__(self):
        cap = str(self.cap) if self.cap else 'uncapped'
        return f'{self.gpu}/{self.scenario}/r{self.radius}/{cap}'

    @property
    def slug(self):
        return str(self).replace('/', '-')

    @property
    def label(self):
        """Spanish name for the reports."""
        text = SCENARIOS[self.scenario].label
        if self.radius != 16:
            text += f', radio {self.radius}'
        return text + (f', tope {self.cap}' if self.cap else ', sin tope')

    def args(self, duration):
        s = SCENARIOS[self.scenario]
        big = self.radius > BIG_RADIUS
        args = ['--scenario', s.stress, *s.args, *GPUS[self.gpu], '--radius', str(self.radius),
                '--fps-limit', str(self.cap), '--duration', f'{duration:g}',
                '--timeout', str(int(duration) + (240 if big else 150))]
        if big and '--world' in s.args:
            args += BIG_ARENA
        return args


def parse_case(text, default=Case()):
    """`gpu/scenario/rN/cap` in any order; missing parts come from `default`."""
    fields = dataclasses.asdict(default)
    for part in filter(None, text.lower().split('/')):
        if part in GPUS:
            fields['gpu'] = part
        elif part in SCENARIOS:
            fields['scenario'] = part
        elif re.fullmatch(r'r\d+', part):
            fields['radius'] = int(part[1:])
        elif part == 'uncapped':
            fields['cap'] = 0
        elif re.fullmatch(r'(cap)?\d+', part):
            fields['cap'] = int(part.removeprefix('cap'))
        else:
            raise ValueError(f'{text!r}: {part!r} is not a GPU ({", ".join(GPUS)}), a scenario '
                             f'({", ".join(SCENARIOS)}), a radius (r16) or a cap (144, uncapped)')
    if not 1 <= fields['radius'] <= 64:
        raise ValueError(f'{text!r}: the radius goes from 1 to 64')
    return Case(**fields)


_help = {}


def supports(binary, case):
    """True if `binary` knows the flags and the scenario of `case` (from its --help)."""
    needs = SCENARIOS[case.scenario].needs
    if not needs:
        return True
    if binary not in _help:
        try:
            done = subprocess.run([binary, '--help'], capture_output=True, text=True,
                                  timeout=30)
            _help[binary] = done.stdout + done.stderr
        except (OSError, subprocess.TimeoutExpired):
            _help[binary] = ''
    return all(re.search(need, _help[binary], re.M) for need in needs)


# ---------------------------------------------------------------------------------------------
# NVIDIA state (read only)

NV_FIELDS = ['timestamp', 'pstate', 'clocks.gr', 'clocks.mem', 'power.draw', 'utilization.gpu']


def parse_nv(line, when):
    """One `nvidia-smi --query-gpu` CSV line as a dict, or None."""
    cells = [c.strip() for c in line.split(',')]
    if len(cells) != len(NV_FIELDS) or not re.fullmatch(r'P\d+', cells[1]):
        return None
    return {'unix': when, 'pstate': cells[1], 'gr_mhz': cells[2], 'mem_mhz': cells[3],
            'power_w': cells[4], 'util': cells[5]}


def pstate_share(samples, start=None, end=None):
    """{P-state: share 0..1} of the samples between `start` and `end` (unix seconds)."""
    inside = [s for s in samples
              if (start is None or s['unix'] >= start) and (end is None or s['unix'] <= end)]
    counts = {}
    for s in inside:
        counts[s['pstate']] = counts.get(s['pstate'], 0) + 1
    return {k: n / len(inside) for k, n in sorted(counts.items())} if inside else {}


class NvidiaSampler:
    """`nvidia-smi` every 0.5 s (a read-only query) while a run goes on."""

    def __init__(self):
        self.samples = []
        self._proc = None

    def start(self):
        if not shutil.which('nvidia-smi'):
            return self
        self._proc = subprocess.Popen(
            ['nvidia-smi', '--query-gpu=' + ','.join(NV_FIELDS), '--format=csv,noheader,nounits',
             '-lms', '500'], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        threading.Thread(target=self._read, daemon=True).start()
        return self

    def _read(self):
        for line in self._proc.stdout:
            sample = parse_nv(line, time.time())
            if sample:
                self.samples.append(sample)

    def stop(self):
        if self._proc:
            self._proc.terminate()
            self._proc.wait()
        return self.samples


# ---------------------------------------------------------------------------------------------
# Numbers of one run

def presented_times(rows):
    return [float(r['t_ms']) for r in rows if r.get('presented') == '1']


def skipped_refreshes(times_ms, refresh_hz=None):
    """Monitor refreshes that got no new frame: each gap between presented frames counts the
    refresh periods it spans, minus one."""
    gaps = [b - a for a, b in zip(times_ms, times_ms[1:])]
    if not gaps:
        return None
    period = 1000.0 / refresh_hz if refresh_hz else statistics.median(gaps)
    return sum(max(0, round(g / period) - 1) for g in gaps) if period > 0 else None


def throttled(summary, times_ms):
    """Why the run looks throttled by the compositor (30 fps), or None."""
    slow = [s['second'] for s in summary.get('timeline') or [] if s.get('fps', 1e9) < THROTTLE_FPS]
    if slow:
        return f'seconds under {THROTTLE_FPS} fps ({", ".join(map(str, slow[:5]))})'
    if times_ms:
        seconds = int(times_ms[-1] // 1000)
        shown = [0] * (seconds + 1)
        for t in times_ms:
            shown[int(t // 1000)] += 1
        few = [str(i) for i, n in enumerate(shown[:seconds]) if n < THROTTLE_SHOWN]
        if few:
            return f'seconds with under {THROTTLE_SHOWN} frames shown ({", ".join(few[:5])})'
    return None


def metrics(summary, times_ms, nv=None):
    """The numbers the verdicts and the reports use. `slow` counts the frames over 6.94 ms
    uncapped and over 7.5 ms capped: with a cap the compositor's callbacks come every 6.95 ms,
    so half the frames pass 6.94 ms by a hair (docs/rendimiento/ritmo-2026-10.md)."""
    get = bench_report.get
    run = summary.get('run') or {}
    capped = bool(run.get('fps_limit'))
    final = get(summary, ('world', 'final')) or {}
    return {
        'fps': get(summary, ('frames', 'avg_fps')),
        'p50': get(summary, ('frames', 'p50_ms')),
        'p99': get(summary, ('frames', 'p99_ms')),
        'slow': get(summary, ('frames', 'over_7_5ms' if capped else 'over_144hz')),
        'skipped': skipped_refreshes(times_ms, run.get('refresh_hz')),
        'catch_up': get(summary, ('world', 'border_catch_up', 'avg_ms')),
        'pstates': nv,
        'quads': final.get('quads'),
        'draws': final.get('draws'),
        'gpu_chunks': final.get('gpu_chunks'),
    }


# ---------------------------------------------------------------------------------------------
# One run

@dataclasses.dataclass
class Run:
    """A finished run. `error` when the binary failed; `noisy` when every attempt was thrown away
    and this is the last one."""
    case: Case
    prefix: str
    rc: int = 0
    metrics: dict = None
    error: str = None
    detail: str = ''
    noisy: str = None
    seconds: float = 0.0
    discarded: list = dataclasses.field(default_factory=list)


def run_once(binary, case, prefix, label, duration, nvidia=False):
    """One run of `binary` on `case`, watched. Writes PREFIX.{md,csv,log,watch.json} (and
    .nv.csv). Returns (rc, watch, measure start, nvidia samples)."""
    args = [binary, *case.args(duration), '--label', label, '--csv', prefix + '.csv',
            '--summary', prefix + '.md']
    sampler = NvidiaSampler().start() if nvidia else None
    measure_at = None
    with open(prefix + '.log', 'w') as log:
        log.write(' '.join(args) + '\n')
        proc = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                stdin=subprocess.DEVNULL, text=True)
        watcher = machine.Watcher(proc.pid).start()
        for line in proc.stdout:
            log.write(line)
            if measure_at is None and line.startswith('stress: measuring'):
                measure_at = time.time()
        proc.stdout.close()
        rc = proc.wait()
    watch = watcher.stop()
    samples = sampler.stop() if sampler else []
    with open(prefix + '.watch.json', 'w') as f:
        json.dump(watch, f)
    if samples:
        with open(prefix + '.nv.csv', 'w') as f:
            f.write('unix,' + ','.join(NV_FIELDS[1:]) + '\n')
            for s in samples:
                f.write(f"{s['unix']:.3f},{s['pstate']},{s['gr_mhz']},{s['mem_mhz']},"
                        f"{s['power_w']},{s['util']}\n")
    return rc, watch, measure_at, samples


def _log_tail(prefix, lines=6):
    try:
        with open(prefix + '.log') as f:
            return ''.join(f.readlines()[-lines:]).rstrip()
    except OSError:
        return ''


def _discard(prefix, attempt):
    """Moves the files of a run that does not count to discarded/, as NAME.aATTEMPT.*."""
    folder = os.path.join(os.path.dirname(prefix), 'discarded')
    os.makedirs(folder, exist_ok=True)
    for ext in ('.md', '.csv', '.log', '.watch.json', '.nv.csv'):
        if os.path.exists(prefix + ext):
            os.replace(prefix + ext,
                       os.path.join(folder, f'{os.path.basename(prefix)}.a{attempt}{ext}'))


def measure(binary, case, prefix, label, duration, nvidia=False, attempts=None, say=print):
    """A run of `case` that counts: on a quiet machine, redone up to `attempts` times when it
    overlapped something or was throttled."""
    attempts = attempts or ATTEMPTS
    run = Run(case, prefix)
    for attempt in range(1, attempts + 1):
        start = time.monotonic()
        with machine.run_lock(say):
            if not machine.wait_quiet(say=say):
                run.error = 'the machine never got quiet (PERF_MAX_WAIT)'
                return run
            rc, watch, measure_at, samples = run_once(binary, case, prefix, label, duration,
                                                      nvidia)
        run.seconds = time.monotonic() - start
        run.rc = rc
        if rc != 0:
            run.error = f'exit code {rc} (log: {prefix}.log)'
            run.detail = _log_tail(prefix)
            return run
        try:
            with open(prefix + '.md', encoding='utf-8') as f:
                summary = bench_report.parse_summary(f.read(), prefix + '.md')
            with open(prefix + '.csv', newline='') as f:
                times = presented_times(csv.DictReader(f))
        except (OSError, ValueError, bench_report.ReportError) as e:
            run.error = f'no summary: {e}'
            return run
        end = measure_at + duration if measure_at else None
        share = pstate_share(samples, measure_at, end) if nvidia else None
        run.metrics = metrics(summary, times, share)
        reason = machine.overlap(watch) or throttled(summary, times)
        if not reason:
            run.noisy = None
            return run
        run.noisy = reason
        run.discarded.append(reason)
        if attempt < attempts:
            say(f'    thrown away ({reason}), redoing')
            _discard(prefix, attempt)
    return run


def estimate_s(case, duration):
    """Rough wall time of one run: the measurement, its 2 s warmup, and settling, catch-up and
    the quiet check before it (radius 16 on the RTX: 16 to 17 s for a 10 s run)."""
    return duration + 2 + (20 if case.radius > BIG_RADIUS else 6)
