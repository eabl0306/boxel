"""Stress cases, one watched run of examples/stress and the numbers the verdicts use.

A case is a GPU, a scenario, a radius and an fps cap, written `rtx/surface/r16/uncapped`
(`gpu/scenario/rN/cap`; any order, missing parts take the defaults, so `surface` or
`780m/forest/144` work too). `measure` runs a case on a quiet machine and redoes the runs that do
not count: they overlapped a build or another benchmark, the rest of the machine was busy, or the
compositor throttled the window to 30 fps (a second under 100 fps or under 50 frames shown, or
under 70 % of a lower cap). A run that still does not count after the last attempt is kept as
`noisy` and its numbers never feed a verdict.
"""
import csv
import dataclasses
import functools
import glob
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
# A second under this fps (or with fewer frames shown) is the compositor's 30 fps throttle. With a
# lower cap the limits are this share of the cap, and caps under MIN_CAP would hide the throttle.
THROTTLE_FPS = 100
THROTTLE_SHOWN = 50
THROTTLE_CAP_SHARE = 0.7
MIN_CAP = 50
# Flags the harness sets itself: extra stress options must not repeat them.
HARNESS_FLAGS = ('--label', '--csv', '--summary')
# Tries of a run before keeping one that does not count (marked noisy).
ATTEMPTS = 4
# A capped run that skipped refreshes on more than this share of its frames is in the high skip
# mode (skip_mode).
HIGH_MODE_SKIPS = 0.01


@dataclasses.dataclass(frozen=True)
class Case:
    gpu: str = 'rtx'
    scenario: str = 'surface'
    radius: int = 16
    cap: int = 0
    # Extra stress options, the same on both sides (`just perf-smoke forest -- --bodies 10000`).
    extra: tuple = ()
    # Extra stress options of the new side only, after `extra` (`perf.py smoke --new-extra
    # '--fog valley'`): an option the new binary turns on, measured against the same binary
    # without it (`--base HEAD`).
    new_extra: tuple = ()

    def __str__(self):
        cap = str(self.cap) if self.cap else 'uncapped'
        name = f'{self.gpu}/{self.scenario}/r{self.radius}/{cap}'
        new = (f"[new: {' '.join(self.new_extra)}]",) if self.new_extra else ()
        return ' '.join((name, *self.extra, *new))

    @property
    def slug(self):
        return re.sub(r'[^A-Za-z0-9.]+', '-', str(self)).strip('-')

    @property
    def label(self):
        """Spanish name for the reports."""
        text = SCENARIOS[self.scenario].label
        if self.radius != 16:
            text += f', radio {self.radius}'
        text += f', tope {self.cap}' if self.cap else ', sin tope'
        text += f", {' '.join(self.extra)}" if self.extra else ''
        return text + (f", solo el nuevo: {' '.join(self.new_extra)}" if self.new_extra else '')

    def args(self, duration, side='base'):
        """The stress arguments of a run; the new side also gets `new_extra`."""
        s = SCENARIOS[self.scenario]
        big = self.radius > BIG_RADIUS
        args = ['--scenario', s.stress, *s.args, *GPUS[self.gpu], '--radius', str(self.radius),
                '--fps-limit', str(self.cap), '--duration', f'{duration:g}',
                '--timeout', str(int(duration) + (240 if big else 150))]
        if big and '--world' in s.args:
            args += BIG_ARENA
        return args + list(self.extra) + (list(self.new_extra) if side == 'new' else [])


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
    if fields['cap'] and fields['cap'] < MIN_CAP:
        raise ValueError(f'{text!r}: caps under {MIN_CAP} hide the compositor\'s 30 fps throttle '
                         '(use 0 or uncapped, 144, ...)')
    case = Case(**fields)
    plain = dataclasses.replace(case, extra=(), new_extra=())
    own = {a for a in plain.args(1) if a.startswith('--')} | set(HARNESS_FLAGS)
    taken = sorted({a for a in (*case.extra, *case.new_extra) if a in own})
    if taken:
        raise ValueError(f"{text!r}: the case already sets {', '.join(taken)}; say it with the "
                         'case (gpu/scenario/rN/cap) or leave it out')
    return case


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

NV_BASE = ['timestamp', 'pstate', 'clocks.gr', 'clocks.mem', 'power.draw', 'utilization.gpu']
# Uncapped, the NVIDIA sometimes runs a whole stretch about 12 % slower at the same clock and with
# less power (docs/rendimiento/sin-tope-lento-2026-10.md): the active clock event reasons (a bit
# mask, `nvidia-smi --help-query-gpu` names the bits) and the temperature are there to tell why.
NV_FIELDS = NV_BASE + ['clocks_event_reasons.active', 'temperature.gpu']
# The key of each field in a sample. Its .nv.csv column keeps the field's name.
NV_KEYS = {'pstate': 'pstate', 'clocks.gr': 'gr_mhz', 'clocks.mem': 'mem_mhz',
           'power.draw': 'power_w', 'utilization.gpu': 'util',
           'clocks_event_reasons.active': 'reasons', 'temperature.gpu': 'temp_c'}
# Means of a run (nv_means): the numeric fields and the CPU's mean frequency, sampled with them.
NV_NUMBERS = ('gr_mhz', 'mem_mhz', 'power_w', 'util', 'temp_c', 'cpu_mhz')
CPU_FREQ = '/sys/devices/system/cpu/cpu[0-9]*/cpufreq/scaling_cur_freq'


@functools.cache
def query_fields():
    """NV_FIELDS when nvidia-smi knows them all, else NV_BASE: a field the driver does not know
    fails the whole query, and the P-states would go with it. Asked once."""
    try:
        done = subprocess.run(['nvidia-smi', '--query-gpu=' + ','.join(NV_FIELDS),
                               '--format=csv,noheader,nounits'], capture_output=True, text=True,
                              stdin=subprocess.DEVNULL, timeout=15)
    except (OSError, subprocess.TimeoutExpired):
        return NV_BASE
    return NV_FIELDS if done.returncode == 0 else NV_BASE


def parse_nv(line, when, fields=None):
    """One `nvidia-smi --query-gpu` CSV line of `fields` (default NV_FIELDS) as a dict, or
    None."""
    fields = fields or NV_FIELDS
    cells = [c.strip() for c in line.split(',')]
    if len(cells) != len(fields) or not re.fullmatch(r'P\d+', cells[1]):
        return None
    sample = {'unix': when}
    sample.update((NV_KEYS[f], c) for f, c in zip(fields[1:], cells[1:]))
    return sample


def cpu_mhz(paths):
    """The mean of the CPU cores' current frequencies (scaling_cur_freq, kHz) in MHz, or None."""
    values = []
    for path in paths:
        try:
            with open(path) as f:
                values.append(int(f.read()) / 1000)
        except (OSError, ValueError):
            continue
    return round(sum(values) / len(values)) if values else None


def window(samples, start=None, end=None):
    """The samples between `start` and `end` (unix seconds)."""
    return [s for s in samples
            if (start is None or s['unix'] >= start) and (end is None or s['unix'] <= end)]


def pstate_share(samples, start=None, end=None):
    """{P-state: share 0..1} of the samples between `start` and `end` (unix seconds)."""
    inside = window(samples, start, end)
    counts = {}
    for s in inside:
        counts[s['pstate']] = counts.get(s['pstate'], 0) + 1
    return {k: n / len(inside) for k, n in sorted(counts.items())} if inside else {}


def number(text):
    try:
        return float(text)
    except (TypeError, ValueError):
        return None


def nv_means(samples, start=None, end=None):
    """The NVIDIA state of a run, from the samples between `start` and `end`: the mean of each
    NV_NUMBERS field (None without a value) and `reasons`, {clock event reason mask: share}.
    None without samples."""
    inside = window(samples, start, end)
    if not inside:
        return None
    out = {}
    for key in NV_NUMBERS:
        values = [v for s in inside if (v := number(s.get(key))) is not None]
        out[key] = round(sum(values) / len(values), 1) if values else None
    masks = []
    for s in inside:
        try:
            masks.append(hex(int(s.get('reasons'), 16)))
        except (TypeError, ValueError):
            continue
    out['reasons'] = {m: round(masks.count(m) / len(masks), 3) for m in sorted(set(masks))}
    return out


class NvidiaSampler:
    """`nvidia-smi` every 0.5 s (a read-only query) while a run goes on."""

    def __init__(self):
        self.samples = []
        self.fields = NV_FIELDS
        self._cpus = []
        self._proc = None
        self._thread = None

    def start(self):
        if not shutil.which('nvidia-smi'):
            return self
        self.fields = query_fields()
        self._cpus = glob.glob(CPU_FREQ)
        self._proc = subprocess.Popen(
            ['nvidia-smi', '--query-gpu=' + ','.join(self.fields),
             '--format=csv,noheader,nounits', '-lms', '500'],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        self._thread = threading.Thread(target=self._read, daemon=True)
        self._thread.start()
        return self

    def _read(self):
        for line in self._proc.stdout:
            sample = parse_nv(line, time.time(), self.fields)
            if sample:
                sample['cpu_mhz'] = cpu_mhz(self._cpus)
                self.samples.append(sample)

    def stop(self):
        if self._proc:
            self._proc.terminate()
            self._proc.wait()
            self._thread.join(timeout=2)
            self._proc.stdout.close()
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


def skip_mode(skipped, shown):
    """The skip mode of a capped run: 'high' when it skipped refreshes on more than 1 % of the
    frames shown, else 'low' (None without frames). Capped runs on the NVIDIA land at random in a
    high or a low mode, about 5 or about 1 skipped refresh per second, from the power state of
    the GPU (issue #5, docs/rendimiento/puesta-al-dia-2026-10.md). Past 1 % the p99 frame is one
    that waited two refreshes (13.9 ms at 144 Hz), so p99 and slow frames follow the mode."""
    if skipped is None or not shown:
        return None
    return 'high' if skipped > HIGH_MODE_SKIPS * shown else 'low'


def throttle_limits(cap):
    """(fps, frames shown) under which a second is the compositor's 30 fps throttle: 100 and 50
    uncapped, and at most 70 % of the cap with one."""
    if not cap:
        return THROTTLE_FPS, THROTTLE_SHOWN
    share = THROTTLE_CAP_SHARE * cap
    return min(THROTTLE_FPS, share), min(THROTTLE_SHOWN, share)


def throttled(summary, times_ms):
    """Why the run looks throttled by the compositor (30 fps), or None."""
    fps, shown_min = throttle_limits((summary.get('run') or {}).get('fps_limit'))
    slow = [s['second'] for s in summary.get('timeline') or [] if s.get('fps', 1e9) < fps]
    if slow:
        return f'seconds under {fmt_limit(fps)} fps ({", ".join(map(str, slow[:5]))})'
    if times_ms:
        seconds = int(times_ms[-1] // 1000)
        shown = [0] * (seconds + 1)
        for t in times_ms:
            shown[int(t // 1000)] += 1
        few = [str(i) for i, n in enumerate(shown[:seconds]) if n < shown_min]
        if few:
            return (f'seconds with under {fmt_limit(shown_min)} frames shown '
                    f'({", ".join(few[:5])})')
    return None


def fmt_limit(value):
    return f'{value:g}' if value == int(value) else f'{value:.1f}'


def metrics(summary, times_ms, nv=None, nv_state=None):
    """The numbers the verdicts and the reports use. `slow` counts the frames over 6.94 ms (the
    smoke rule, capped or not) and `slow75` those over 7.5 ms (the checkpoint's slow frames with
    a cap: there the compositor's callbacks come every 6.95 ms, so half the frames pass 6.94 ms
    by a hair, docs/rendimiento/ritmo-2026-10.md). `mode` is the skip mode of a capped run
    (skip_mode), None uncapped."""
    get = bench_report.get
    run = summary.get('run') or {}
    final = get(summary, ('world', 'final')) or {}
    skipped = skipped_refreshes(times_ms, run.get('refresh_hz'))
    return {
        'fps': get(summary, ('frames', 'avg_fps')),
        'p50': get(summary, ('frames', 'p50_ms')),
        'p99': get(summary, ('frames', 'p99_ms')),
        'slow': get(summary, ('frames', 'over_144hz')),
        'slow75': get(summary, ('frames', 'over_7_5ms')),
        'skipped': skipped,
        'mode': skip_mode(skipped, len(times_ms)) if run.get('fps_limit') else None,
        'catch_up': get(summary, ('world', 'border_catch_up', 'avg_ms')),
        'pstates': nv,
        'quads': final.get('quads'),
        'draws': final.get('draws'),
        'gpu_chunks': final.get('gpu_chunks'),
        # The fps of each second: the path of a scenario is fixed, so runs compare second by
        # second (perf.slow_runs).
        'fps_s': [s.get('fps') for s in summary.get('timeline') or []],
        # The NVIDIA's mean state while measuring (nv_means), None without samples.
        'nv': nv_state,
    }


# ---------------------------------------------------------------------------------------------
# One run

@dataclasses.dataclass
class Run:
    """A finished run. `error`: the binary failed (it exited non-zero or wrote no summary).
    `noisy`: the run does not count, and why: every attempt overlapped something or was throttled
    (the last one's numbers stay in `metrics` for the record only), or the machine never got
    quiet (`gave_up`). `discarded`: why the attempts before the last were thrown away."""
    case: Case
    prefix: str
    rc: int = 0
    metrics: dict = None
    error: str = None
    detail: str = ''
    noisy: str = None
    gave_up: bool = False
    seconds: float = 0.0
    discarded: list = dataclasses.field(default_factory=list)

    @property
    def counts(self):
        """Its numbers can feed a verdict."""
        return self.metrics is not None and not self.error and not self.noisy

    @property
    def ran(self):
        """The binary ran to the end, even if the run does not count: the machine can run it."""
        return self.metrics is not None and not self.error


def run_once(binary, case, prefix, label, duration, nvidia=False, side='base'):
    """One run of `binary` on `case`, watched (`side` 'new' adds the case's new_extra). Writes
    PREFIX.{md,csv,log,watch.json} (and .nv.csv). Returns (rc, watch, measure start, nvidia
    samples)."""
    args = [binary, *case.args(duration, side), '--label', label, '--csv', prefix + '.csv',
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
        write_nv(prefix + '.nv.csv', samples)
    return rc, watch, measure_at, samples


def write_nv(path, samples):
    """The NVIDIA samples of a run as CSV: unix time, then a column per field (named as
    nvidia-smi names it) and cpu_mhz."""
    names = {key: field for field, key in NV_KEYS.items()}
    keys = [k for k in samples[0] if k != 'unix']
    with open(path, 'w') as f:
        f.write(','.join(['unix', *(names.get(k, k) for k in keys)]) + '\n')
        for s in samples:
            cells = ['' if s.get(k) is None else str(s[k]) for k in keys]
            f.write(','.join([f"{s['unix']:.3f}", *cells]) + '\n')


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


def measure(binary, case, prefix, label, duration, nvidia=False, attempts=None, say=print,
            side='base'):
    """A run of `case` that counts: on a quiet machine, redone up to `attempts` times when it
    overlapped something or was throttled. The waits for a quiet machine share one deadline
    (PERF_MAX_WAIT). `side` 'new' adds the case's new_extra options."""
    attempts = attempts or ATTEMPTS
    run = Run(case, prefix)
    max_wait = machine.default_wait_s()
    deadline = time.monotonic() + max_wait
    for attempt in range(1, attempts + 1):
        run.metrics = run.noisy = None
        start = time.monotonic()
        with machine.run_lock(say):
            if not machine.wait_quiet(max(0.0, deadline - start), say=say):
                run.noisy = f'the machine never got quiet in {max_wait:g} s (PERF_MAX_WAIT)'
                run.gave_up = True
                return run
            rc, watch, measure_at, samples = run_once(binary, case, prefix, label, duration,
                                                      nvidia, side)
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
        state = nv_means(samples, measure_at, end) if nvidia else None
        run.metrics = metrics(summary, times, share, state)
        reason = machine.overlap(watch) or throttled(summary, times)
        if not reason:
            run.noisy = None
            return run
        run.noisy = reason
        # Its files go too, so the folder (and bench-report on it) only holds runs that count.
        _discard(prefix, attempt)
        if attempt < attempts:
            run.discarded.append(reason)
            say(f'    thrown away ({reason}), redoing')
    say(f'    still noisy after {attempts} attempts ({run.noisy}): this run does not count')
    return run


def estimate_s(case, duration):
    """Rough wall time of one run: the measurement, its 2 s warmup, and settling, catch-up and
    the quiet check before it (radius 16 on the RTX: 16 to 17 s for a 10 s run)."""
    return duration + 2 + (20 if case.radius > BIG_RADIUS else 6)
