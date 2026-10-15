#!/usr/bin/env python3
"""Tests of tools/perf: python3 tools/perf/perf_test.py (or `just test-tools`).

No GPU and no real stress run: the runs go to fake stress binaries (Python scripts that write a
summary and a CSV, or fail like stress without a GPU), so the watching, the redo of throttled
runs, the verdicts and the exit codes are covered."""
import contextlib
import io
import json
import math
import os
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import machine  # noqa: E402
import perf  # noqa: E402
import runs  # noqa: E402
from runs import Case  # noqa: E402

# The tests run the harness in this process: never as if inside a quiet.sh slot.
os.environ.pop('BOXEL_QUIET_SLOT', None)


def summary(fps=1000.0, p99=1.0, slow=0, slow75=0, cap=0, timeline_fps=None, catch_up=20.0):
    return {
        'format': 1,
        'run': {'scenario': 'fly', 'radius': 16, 'speed': 64, 'fps_limit': cap,
                'refresh_hz': 144.0, 'backend': 'Vulkan', 'tier': 'High', 'adapter': 'GPU'},
        'frames': {'avg_fps': fps, 'p50_ms': 0.5, 'p99_ms': p99, 'over_144hz': slow,
                   'over_7_5ms': slow75},
        'world': {'border_catch_up': {'avg_ms': catch_up},
                  'final': {'quads': 100, 'draws': 7, 'gpu_chunks': 50}},
        'timeline': [{'second': i, 'fps': f} for i, f in enumerate(timeline_fps or [fps] * 3)],
    }


def every_refresh(seconds=3, hz=144.0, skip=()):
    """Presented frame times (ms), one per refresh, leaving out the refreshes in `skip`."""
    period = 1000.0 / hz
    return [i * period for i in range(int(seconds * hz)) if i not in skip]


def metrics(fps=1000.0, p99=1.0, slow=0, **more):
    m = {'fps': fps, 'p50': 0.5, 'p99': p99, 'slow': slow, 'slow75': 0, 'skipped': 0,
         'catch_up': 20.0, 'pstates': None, 'quads': 100, 'draws': 7, 'gpu_chunks': 50}
    m.update(more)
    return m


def run(m=None, error=None, noisy=None, gave_up=False):
    return runs.Run(Case(), 'x', metrics=m, error=error, noisy=noisy, gave_up=gave_up)


# Refreshes a fake run of 3 s skips to land in the high skip mode: 11 of 432, over 1 %.
HIGH_MODE = tuple(range(5, 430, 40))

# Checkpoint round 1, rtx/surface/r16/uncapped (bench-results/control/1-20261009-200903): fps of
# each second and mean power of each run. New runs 2 and 3 fell into the NVIDIA's slow state, for
# the whole run and for its first 9 seconds.
ROUND1_SURFACE = {
    'base': [
        (2303.5, 85.9, [2140, 2147, 2140, 2108, 2136, 2145, 2140, 2189, 2314, 2441, 2418, 2413,
                        2482, 2553, 2444, 2416, 2427, 2417, 2275, 2326]),
        (2313.1, 86.1, [2145, 2169, 2154, 2126, 2146, 2157, 2155, 2197, 2298, 2434, 2436, 2413,
                        2520, 2561, 2445, 2433, 2442, 2413, 2281, 2336]),
        (2306.2, 86.0, [2077, 2156, 2154, 2124, 2148, 2166, 2151, 2195, 2314, 2434, 2436, 2416,
                        2501, 2561, 2435, 2435, 2404, 2406, 2276, 2334])],
    'new': [
        (2290.6, 85.2, [2108, 2138, 2144, 2119, 2140, 2154, 2142, 2188, 2307, 2410, 2412, 2378,
                        2488, 2525, 2419, 2400, 2378, 2392, 2255, 2313]),
        (2042.9, 80.8, [1962, 1855, 1875, 1833, 1861, 1878, 1883, 1936, 2024, 2450, 2170, 2124,
                        2222, 2270, 2144, 2141, 2121, 2095, 1986, 2029]),
        (2132.7, 80.5, [1949, 1858, 1868, 1820, 1840, 1871, 1854, 1905, 2006, 2399, 2116, 2337,
                        2424, 2472, 2385, 2369, 2342, 2349, 2223, 2266])],
}


def round1_surface(**sides):
    """The metrics of ROUND1_SURFACE (or of the runs given per side), numbered by round."""
    data = dict(ROUND1_SURFACE, **sides)
    return {side: [metrics(fps=fps, fps_s=per_s, nv={'power_w': w}, round=i + 1)
                   for i, (fps, w, per_s) in enumerate(data[side])] for side in ('base', 'new')}


HELP = ('usage: stress [--scenario static|fly|dive|spin|path]\n'
        '  --cave              start in a cave\n')
OLD_HELP = 'usage: stress [--scenario static|fly|dive|spin]\n  --caves MODE  cave mode\n'

FAKE_STRESS = r'''#!/usr/bin/env python3
"""Fake stress: --help prints HELP; a run writes the files a real one writes from SUMMARY (the
first run of the binary throttled when THROTTLE, the refreshes in SKIP left out of the CSV), or
fails like stress without a GPU when FAIL is True or appears in the arguments. With CASES (a
JSON text) it is an outside app: --perf-cases prints CASES, and a run says `app: measuring`."""
import json, os, sys
HELP, SUMMARY, FAIL, THROTTLE, SKIP, CASES = @VALUES@
args = sys.argv[1:]
if '--help' in args:
    print(HELP)
    sys.exit(0)
if '--perf-cases' in args:
    if CASES is None:
        print('stress: unknown argument --perf-cases', file=sys.stderr)
        sys.exit(1)
    print(CASES)
    sys.exit(0)
me = os.path.abspath(__file__)
n = int(open(me + '.count').read()) if os.path.exists(me + '.count') else 0
open(me + '.count', 'w').write(str(n + 1))
open(me + '.args', 'a').write(' '.join(args) + '\n')
if FAIL is True or (FAIL and FAIL in ' '.join(args)):
    print('stress: no usable GPU', file=sys.stderr)
    sys.exit(2)
out = {args[i]: args[i + 1] for i in range(len(args) - 1) if args[i].startswith('--')}
data = json.loads(SUMMARY)
if n == 0 and THROTTLE:
    data['timeline'][1]['fps'] = 30.0
print('app' if CASES else 'stress', 'measuring fly for 3 s', sep=': ', flush=True)
with open(out['--summary'], 'w') as f:
    f.write('# stress\n\n## Data\n\n```json\n' + json.dumps(data) + '\n```\n')
with open(out['--csv'], 'w') as f:
    f.write('frame,t_ms,presented\n')
    for i in range(3 * 144):
        if i not in SKIP:
            f.write(f'{i},{i * 1000 / 144:.3f},1\n')
'''


def fake_stress(folder, name, data=None, fail=False, help_text=HELP, throttle_first=False,
                skip=(), cases=None):
    """A fake stress binary in `folder`; `data` is its summary (default: summary()), `skip` the
    refreshes its runs skip. `cases`: an outside app instead, declaring these {name: label} (a
    string: printed as it is)."""
    path = os.path.join(folder, name)
    if isinstance(cases, dict):
        cases = json.dumps({'format': 1, 'cases': [{'name': n, 'label': label}
                                                   for n, label in cases.items()]})
    values = repr((help_text, json.dumps(data or summary()), fail, throttle_first, tuple(skip),
                   cases))
    with open(path, 'w') as f:
        f.write(FAKE_STRESS.replace('@VALUES@', values))
    os.chmod(path, 0o755)
    return path


def calls(path):
    try:
        with open(path + '.count') as f:
            return int(f.read())
    except OSError:
        return 0


def fake_binary(folder, name, help_text):
    path = os.path.join(folder, name)
    with open(path, 'w') as f:
        f.write(f'#!/bin/sh\ncat <<"EOF"\n{help_text}EOF\n')
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)
    return path


class FakeMachine(unittest.TestCase):
    """A quiet machine: no heavy process, no other CPU, the run lock in a temp dir and the
    harness's output kept in self.said."""

    def setUp(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        self.dir = d.name
        self.said = []
        env = {'XDG_RUNTIME_DIR': self.dir}
        for patch in (mock.patch.dict(os.environ, env),
                      mock.patch.object(machine, 'wait_quiet', return_value=True),
                      mock.patch.object(machine, 'heavy_processes', return_value=[]),
                      mock.patch.object(machine, 'MAX_OTHER_CPU', 101.0),
                      mock.patch.object(runs.shutil, 'which', return_value=None),
                      mock.patch.object(perf, 'say', lambda text='': self.said.append(text))):
            patch.start()
            self.addCleanup(patch.stop)

    def binary(self, name, **kw):
        path = fake_stress(self.dir, name, **kw)
        return perf.Binary(path, name, name, name)

    def output(self):
        return '\n'.join(self.said)


class MachineTest(unittest.TestCase):
    def test_classify(self):
        self.assertEqual(machine.classify('rustc', '/home/u/.rustup/bin/rustc'), 'rustc')
        # /proc/<pid>/comm keeps 15 characters.
        self.assertEqual(machine.classify('build-script-bu', ''), 'build-script-bu')
        self.assertEqual(machine.classify('stress', '/r/target-perftools/release/examples/stress'),
                         'boxel:stress')
        self.assertEqual(machine.classify('boxel_world-1a', '/r/target/debug/deps/boxel_world-1a'),
                         'boxel:boxel_world-1a')
        self.assertEqual(machine.classify('stress', '/r/bench-results/bins/abc/stress'),
                         'boxel:stress')
        self.assertEqual(machine.classify('stress-main', '/home/u/.cache/boxel/i3/stress-main'),
                         'boxel:stress-main')
        self.assertIsNone(machine.classify('python3', '/usr/bin/python3.14'))
        self.assertIsNone(machine.classify('gnome-shell', '/usr/bin/gnome-shell'))

    def test_other_cpu(self):
        # 100 ticks per sample, 30 busy, 20 of them the run's: the rest used 10 %.
        samples = [(100 * i, 30 * i, 20 * i) for i in range(10)]
        self.assertEqual(machine.other_cpu(samples), (10.0, 10.0))
        # One 2 s window where the rest of the machine took 60 more ticks.
        spike = samples[:5] + [(t, b + 60, r) for t, b, r in samples[5:]]
        mean, max2s = machine.other_cpu(spike)
        self.assertGreater(max2s, 20.0)
        self.assertLess(mean, max2s)
        self.assertEqual(machine.other_cpu([(0, 0, 0)]), (0.0, 0.0))

    def test_overlap(self):
        clean = {'heavy': [], 'other_cpu_mean': 3.0, 'other_cpu_max2s': 9.0, 'samples': 40}
        self.assertIsNone(machine.overlap(clean))
        self.assertEqual(machine.overlap(dict(clean, heavy=['cargo', 'rustc'])),
                         'overlapped cargo, rustc')
        self.assertIn('40 % CPU', machine.overlap(dict(clean, other_cpu_max2s=40.0)))

    def test_watcher_sees_a_short_process(self):
        proc = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(0.8)'])
        w = machine.Watcher(proc.pid).start()
        proc.wait()
        result = w.stop()
        self.assertGreaterEqual(result['samples'], 1)
        self.assertNotIn('python3', ' '.join(result['heavy']))

    def test_quiet_sh_marks_its_slot(self):
        quiet = os.path.join(perf.TREE, 'tools', 'quiet.sh')
        with tempfile.TemporaryDirectory() as d:
            env = dict(os.environ, QUIET_LOCK_DIR=d, QUIET_MIN_FREE_GB='0')
            env.pop('BOXEL_QUIET_SLOT', None)
            done = subprocess.run([quiet, sys.executable, '-c',
                                   'import os; print(os.environ.get("BOXEL_QUIET_SLOT"))'],
                                  env=env, capture_output=True, text=True, timeout=60)
        self.assertEqual(done.returncode, 0, done.stderr)
        self.assertEqual(done.stdout.strip(), '1')


class CaseTest(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(runs.parse_case('surface'), Case('rtx', 'surface', 16, 0))
        self.assertEqual(runs.parse_case('144/780m/forest'), Case('780m', 'forest', 16, 144))
        self.assertEqual(runs.parse_case('rtx/cave-path/r32/cap144'),
                         Case('rtx', 'cave-path', 32, 144))
        self.assertEqual(runs.parse_case('dive/uncapped', Case(cap=144)).cap, 0)
        self.assertEqual(runs.parse_case('dive', Case(gpu='780m', radius=8)),
                         Case('780m', 'dive', 8, 0))
        self.assertEqual(runs.parse_case('forest/60').cap, 60)
        for bad in ('walk', 'rtx/r0', 'gtx/surface', 'r99', 'forest/30', 'surface/cap40'):
            with self.assertRaises(ValueError):
                runs.parse_case(bad)
        with self.assertRaises(ValueError):
            runs.parse_case('surface', Case(cap=20))

    def test_extra_stress_options(self):
        case = runs.parse_case('forest', Case(extra=('--bodies', '10000')))
        self.assertEqual(case.args(10)[-2:], ['--bodies', '10000'])
        self.assertEqual(str(case), 'rtx/forest/r16/uncapped --bodies 10000')
        self.assertEqual(case.slug, 'rtx-forest-r16-uncapped-bodies-10000')
        self.assertIn('--bodies 10000', case.label)
        for taken in (('--radius', '8'), ('--speed', '8'), ('--summary', 'x'), ('--adapter', 'x')):
            with self.assertRaises(ValueError):
                runs.parse_case('forest', Case(extra=taken))

    def test_new_side_options(self):
        case = runs.parse_case('780m/surface', Case(extra=('--size', '1920x1080'),
                                                    new_extra=('--fog', 'valley')))
        self.assertEqual(case.args(10)[-2:], ['--size', '1920x1080'])
        self.assertEqual(case.args(10, 'new')[-4:], ['--size', '1920x1080', '--fog', 'valley'])
        self.assertEqual(str(case), '780m/surface/r16/uncapped --size 1920x1080 [new: --fog valley]')
        self.assertEqual(case.slug, '780m-surface-r16-uncapped-size-1920x1080-new-fog-valley')
        self.assertIn('solo el nuevo: --fog valley', case.label)
        for taken in (('--radius', '8'), ('--csv', 'x'), ('--adapter', 'x')):
            with self.assertRaises(ValueError):
                runs.parse_case('forest', Case(new_extra=taken))

    def test_names_round_trip(self):
        for case in perf.MATRIX:
            self.assertEqual(runs.parse_case(str(case)), case)
        self.assertEqual(str(Case('780m', 'forest', 16, 144)), '780m/forest/r16/144')
        self.assertEqual(Case().slug, 'rtx-surface-r16-uncapped')
        self.assertEqual(Case('rtx', 'cave-path', 32, 144).label,
                         'cueva abierta, ruta a 8, radio 32, tope 144')

    def test_args(self):
        args = Case('780m', 'forest', 16, 144).args(10)
        self.assertEqual(args[:2], ['--scenario', 'fly'])
        for flag, value in (('--adapter', '780m'), ('--fps-limit', '144'), ('--radius', '16'),
                            ('--duration', '10'), ('--timeout', '160')):
            self.assertEqual(args[args.index(flag) + 1], value)
        self.assertNotIn('--arena-quads', args)
        # every case starts from the binary's own chunk memory and grows (ADR 0035)
        big = Case('rtx', 'surface', 32, 0).args(20)
        self.assertNotIn('--arena-quads', big)
        self.assertNotIn('--chunk-slots', big)
        self.assertEqual(big[big.index('--timeout') + 1], '260')
        boost = runs.parse_case('rtx/boost/r16/144').args(10)
        self.assertEqual(boost[:2], ['--scenario', 'fly'])
        self.assertEqual(boost[boost.index('--speed') + 1], '100')
        self.assertNotIn('--world', boost, 'the forest, like the forest case')

    def test_a_binary_with_a_fixed_arena_gets_the_big_one_over_radius_16(self):
        """Binaries from before chunk memory grew (perf-bisect across #57) dropped chunks of
        the overworld over radius 16 with their own arena."""
        fixed = Case('rtx', 'surface', 32, 0).args(20, fixed_arena=True)
        self.assertEqual(fixed[fixed.index('--arena-quads') + 1], '50331648')
        self.assertEqual(fixed[fixed.index('--chunk-slots') + 1], '98304')
        for small in (Case('rtx', 'forest', 32, 0), Case('rtx', 'surface', 16, 0)):
            self.assertNotIn('--arena-quads', small.args(20, fixed_arena=True))
        # an explicit arena comes after it: stress keeps the last value
        own = runs.parse_case('rtx/cave-path/r32', Case(extra=('--arena-quads', '25165824')))
        args = own.args(20, fixed_arena=True)
        self.assertEqual(args[-2:], ['--arena-quads', '25165824'])
        self.assertLess(args.index('--arena-quads'), len(args) - 2)

    def test_the_grow_case_steps_the_radius_from_the_floor(self):
        case = runs.parse_case('rtx/grow/r32', Case(extra=('--chunk-memory', 'fixed'),
                                                    new_extra=('--chunk-memory', 'grow')))
        base, new = case.args(20), case.args(20, 'new')
        self.assertEqual(base[base.index('--radius-steps') + 1], '8,32@1')
        self.assertNotIn('--arena-quads', base)
        # the new side's own flags come last: stress keeps them
        self.assertEqual(new[-2:], ['--chunk-memory', 'grow'])
        self.assertLess(new.index('--chunk-memory'), len(new) - 2)
        # no case sets the arena any more, so any case takes it as an option
        big = runs.parse_case('rtx/cave-static/r32', Case(new_extra=('--arena-quads', '1')))
        self.assertEqual(big.args(20, 'new')[-2:], ['--arena-quads', '1'])

    def test_supports_reads_the_help(self):
        with tempfile.TemporaryDirectory() as d:
            old = fake_binary(d, 'old', OLD_HELP)
            new = fake_binary(d, 'new', HELP)
            self.assertTrue(runs.supports(old, Case(scenario='surface')))
            self.assertFalse(runs.supports(old, Case(scenario='cave-static')))
            self.assertFalse(runs.supports(old, Case(scenario='cave-path')))
            self.assertTrue(runs.supports(new, Case(scenario='cave-static')))
            self.assertTrue(runs.supports(new, Case(scenario='cave-path')))
            # the grow case needs --radius-steps too
            self.assertFalse(runs.supports(new, Case(scenario='grow')))
            steps = fake_binary(d, 'steps', HELP + '  --radius-steps R0,R1@T1  steps\n')
            self.assertTrue(runs.supports(steps, Case(scenario='grow')))
            # the switch case needs the switch scenario
            self.assertFalse(runs.supports(new, Case(scenario='switch')))
            switch = fake_binary(d, 'switch', HELP.replace('path]', 'path|switch]'))
            self.assertTrue(runs.supports(switch, Case(scenario='switch')))
            self.assertTrue(runs.supports(switch, Case(scenario='cave-path')))
            # chunk memory grows from the binary that has --chunk-memory on
            self.assertFalse(runs.grows(new))
            grows = fake_binary(d, 'grows', HELP + '  --chunk-memory fixed|grow  grows\n')
            self.assertTrue(runs.grows(grows))

    def test_the_switch_case_flies_the_surface_and_changes_worlds(self):
        case = runs.parse_case('780m/switch/r16')
        args = case.args(10)
        self.assertEqual(args[args.index('--scenario') + 1], 'switch')
        self.assertEqual(args[args.index('--speed') + 1], '64')
        self.assertEqual(args[args.index('--switch-every') + 1], '4')
        self.assertEqual(args[args.index('--world') + 1], 'overworld')
        # a longer run may space the switches out (stress keeps the last value)
        long = runs.parse_case('rtx/switch', Case(extra=('--switch-every', '20')))
        self.assertEqual(long.args(200)[-2:], ['--switch-every', '20'])


class MetricsTest(unittest.TestCase):
    def test_skipped_refreshes(self):
        self.assertEqual(runs.skipped_refreshes(every_refresh()), 0)
        self.assertEqual(runs.skipped_refreshes(every_refresh(skip={10, 50, 51}), 144.0), 3)
        # Without the refresh rate, the median gap is the period.
        self.assertEqual(runs.skipped_refreshes(every_refresh(skip={10})), 1)
        self.assertIsNone(runs.skipped_refreshes([5.0]))

    def test_skip_mode(self):
        self.assertEqual(runs.skip_mode(0, 2800), 'low')
        self.assertEqual(runs.skip_mode(28, 2800), 'low', 'exactly 1 %')
        self.assertEqual(runs.skip_mode(29, 2800), 'high')
        self.assertIsNone(runs.skip_mode(None, 2800))
        self.assertIsNone(runs.skip_mode(3, 0))
        # 432 refreshes in 3 s: 5 skipped of 427 shown is over 1 %, 4 of 428 is not.
        high = every_refresh(skip={10, 50, 90, 130, 170})
        low = every_refresh(skip={10, 50, 90, 130})
        self.assertEqual(runs.metrics(summary(cap=144), high)['mode'], 'high')
        self.assertEqual(runs.metrics(summary(cap=144), low)['mode'], 'low')
        self.assertIsNone(runs.metrics(summary(), high)['mode'], 'uncapped runs have no mode')

    def test_throttled(self):
        self.assertIsNone(runs.throttled(summary(), every_refresh()))
        self.assertIn('under 100 fps', runs.throttled(summary(timeline_fps=[140, 30, 140]), []))
        # Uncapped, the loop keeps its fps but the compositor shows 30 frames a second.
        shown = every_refresh(1) + [1000 + i * 33.3 for i in range(30)] + \
            [2000 + t for t in every_refresh(1)]
        self.assertIn('frames shown (1)', runs.throttled(summary(), shown))

    def test_throttled_with_a_low_cap(self):
        """A cap of 60 runs at 60 fps every second: that is not the 30 fps throttle."""
        at_60 = every_refresh(hz=60.0)
        self.assertIsNone(runs.throttled(summary(cap=60, timeline_fps=[60, 59, 60]), at_60))
        self.assertIn('under 42 fps',
                      runs.throttled(summary(cap=60, timeline_fps=[60, 30, 60]), at_60))
        at_30 = every_refresh(1, 60.0) + [1000 + i * 33.3 for i in range(30)] + \
            [2000 + t for t in every_refresh(1, 60.0)]
        self.assertIn('frames shown (1)', runs.throttled(summary(cap=60), at_30))
        # Capped at 144 the limits stay those of the uncapped runs.
        self.assertIn('under 100 fps',
                      runs.throttled(summary(cap=144, timeline_fps=[144, 90, 144]), []))

    def test_metrics(self):
        m = runs.metrics(summary(fps=4500, slow=2, slow75=1, timeline_fps=[4400, 4600, 4500]),
                         every_refresh(), {'P0': 1.0}, {'power_w': 80.0})
        self.assertEqual((m['fps'], m['slow'], m['skipped'], m['catch_up']), (4500, 2, 0, 20.0))
        self.assertEqual(m['pstates'], {'P0': 1.0})
        self.assertEqual(m['fps_s'], [4400, 4600, 4500])
        self.assertEqual(m['nv'], {'power_w': 80.0})
        self.assertEqual(m['quads'], 100)
        capped = runs.metrics(summary(slow=1500, slow75=12, cap=144), [])
        self.assertEqual(capped['slow'], 1500, 'the smoke counts frames over 6.94 ms, capped too')
        self.assertEqual(capped['slow75'], 12)
        self.assertIsNone(capped['skipped'])

    def test_nvidia(self):
        self.assertIsNone(runs.parse_nv('timestamp, pstate', 1.0))
        line = '2026/10/08 12:00:00.000, P0, 2400, 8000, 25.10, 47'
        self.assertIsNone(runs.parse_nv(line, 1.0), 'NV_FIELDS has 8 columns')
        s = runs.parse_nv(line, 5.0, runs.NV_BASE)
        self.assertEqual((s['pstate'], s['gr_mhz'], s['unix']), ('P0', '2400', 5.0))
        s = runs.parse_nv(line + ', 0x0000000000000004, 61', 5.0)
        self.assertEqual((s['power_w'], s['reasons'], s['temp_c']),
                         ('25.10', '0x0000000000000004', '61'))
        samples = [{'unix': t, 'pstate': p} for t, p in
                   [(1, 'P8'), (2, 'P0'), (3, 'P0'), (4, 'P2'), (5, 'P0'), (9, 'P8')]]
        self.assertEqual(runs.pstate_share(samples, 2, 5), {'P0': 0.75, 'P2': 0.25})
        self.assertEqual(runs.pstate_share([], 0, 1), {})

    def fake_nvidia_smi(self, folder, fields):
        """A fake nvidia-smi that knows `fields` only: a query of another field fails, like the
        real one; one without -lms prints a line, one with it loops."""
        line = ', '.join(['2026/10/08 12:00:00.000', 'P0', '2400', '8000', '25.10', '47',
                          '0x0000000000000004', '61'][:len(fields)])
        path = os.path.join(folder, 'nvidia-smi')
        with open(path, 'w') as f:
            f.write(f'#!/bin/sh\ncase "$1" in --query-gpu={",".join(fields)}) ;; *) exit 2 ;; '
                    f'esac\necho "{line}"\n[ "$3" = -lms ] || exit 0\n'
                    f'while :; do echo "{line}"; sleep 0.05; done\n')
        os.chmod(path, 0o755)
        runs.query_fields.cache_clear()
        self.addCleanup(runs.query_fields.cache_clear)
        return mock.patch.dict(os.environ, {'PATH': folder + os.pathsep + os.environ['PATH']})

    def test_nvidia_sampler_closes_its_pipe(self):
        with tempfile.TemporaryDirectory() as d, self.fake_nvidia_smi(d, runs.NV_FIELDS):
            sampler = runs.NvidiaSampler().start()
            time.sleep(0.3)
            samples = sampler.stop()
            runs.write_nv(os.path.join(d, 'run.nv.csv'), samples)
            with open(os.path.join(d, 'run.nv.csv')) as f:
                rows = f.read().splitlines()
        self.assertGreater(len(samples), 0)
        self.assertTrue(sampler._proc.stdout.closed)
        self.assertEqual((samples[0]['reasons'], samples[0]['temp_c']), ('0x0000000000000004',
                                                                         '61'))
        self.assertIn('cpu_mhz', samples[0], 'the CPU frequency goes with each sample')
        self.assertEqual(rows[0], 'unix,pstate,clocks.gr,clocks.mem,power.draw,utilization.gpu,'
                                  'clocks_event_reasons.active,temperature.gpu,cpu_mhz')
        self.assertTrue(rows[1].split(',', 1)[1].startswith('P0,2400,8000,25.10,47,0x'), rows[1])

    def test_an_older_driver_keeps_the_p_states(self):
        """A driver without clocks_event_reasons fails a query that names it: sample the base
        fields then."""
        with tempfile.TemporaryDirectory() as d, self.fake_nvidia_smi(d, runs.NV_BASE):
            self.assertEqual(runs.query_fields(), runs.NV_BASE)
            sampler = runs.NvidiaSampler().start()
            time.sleep(0.3)
            samples = sampler.stop()
        self.assertGreater(len(samples), 0)
        self.assertEqual(samples[0]['pstate'], 'P0')
        self.assertNotIn('reasons', samples[0])

    def test_cpu_mhz(self):
        with tempfile.TemporaryDirectory() as d:
            paths = [os.path.join(d, str(i)) for i in range(3)]
            for path, khz in zip(paths, ('3600000\n', '2800000\n', 'x')):
                with open(path, 'w') as f:
                    f.write(khz)
            self.assertEqual(runs.cpu_mhz(paths + [os.path.join(d, 'gone')]), 3200)
        self.assertIsNone(runs.cpu_mhz([]))

    def test_nv_means(self):
        samples = [{'unix': t, 'pstate': 'P4', 'gr_mhz': str(c), 'power_w': str(w),
                    'reasons': r, 'temp_c': '60', 'cpu_mhz': 3000}
                   for t, c, w, r in [(1, 180, 3.0, '0x0000000000000001'),
                                      (2, 2750, 80.0, '0x0000000000000004'),
                                      (3, 2750, 84.0, '0x0000000000000004'),
                                      (4, 2760, 82.0, '0x0000000000000000'),
                                      (5, 2740, 86.0, '0x0000000000000004'),
                                      (9, 180, 3.0, '0x0000000000000001')]]
        m = runs.nv_means(samples, 2, 5)
        self.assertEqual((m['gr_mhz'], m['power_w'], m['temp_c'], m['cpu_mhz']),
                         (2750.0, 83.0, 60.0, 3000.0))
        self.assertIsNone(m['util'], 'a field nobody sampled')
        self.assertEqual(m['reasons'], {'0x0': 0.25, '0x4': 0.75})
        self.assertIsNone(runs.nv_means(samples, 6, 8))
        self.assertIsNone(runs.nv_means([]))


class MeasureTest(FakeMachine):
    def measure(self, binary, **kw):
        prefix = os.path.join(self.dir, 'runs', 'case-1')
        os.makedirs(os.path.dirname(prefix), exist_ok=True)
        return runs.measure(binary, Case(), prefix, 'label', 3, say=self.said.append, **kw)

    def test_a_good_run(self):
        r = self.measure(fake_stress(self.dir, 'stress', summary(fps=900.0)))
        self.assertIsNone(r.error)
        self.assertIsNone(r.noisy)
        self.assertTrue(r.counts)
        self.assertEqual((r.metrics['fps'], r.metrics['skipped']), (900.0, 0))
        prefix = os.path.join(self.dir, 'runs', 'case-1')
        for ext in ('.md', '.csv', '.log', '.watch.json'):
            self.assertTrue(os.path.exists(prefix + ext), ext)
        with open(prefix + '.log') as f:
            self.assertIn('--label label', f.read())

    def test_a_throttled_run_is_redone(self):
        r = self.measure(fake_stress(self.dir, 'stress', throttle_first=True))
        self.assertIsNone(r.noisy)
        self.assertTrue(r.counts)
        self.assertEqual(len(r.discarded), 1)
        self.assertIn('under 100 fps', self.said[0])
        moved = os.listdir(os.path.join(self.dir, 'runs', 'discarded'))
        self.assertIn('case-1.a1.md', moved)

    def test_a_run_still_noisy_does_not_count(self):
        with mock.patch.object(machine, 'heavy_processes', return_value=['rustc']):
            r = self.measure(fake_stress(self.dir, 'stress'), attempts=2)
        self.assertEqual(r.noisy, 'overlapped rustc')
        self.assertFalse(r.counts)
        self.assertTrue(r.ran, 'the binary ran: a failure of the other side is not the machine')
        self.assertEqual(len(r.discarded), 1, 'one attempt redone, the last one kept as noisy')
        self.assertIsNotNone(r.metrics)
        # Its files leave the folder, so bench-report on it never sees them.
        prefix = os.path.join(self.dir, 'runs', 'case-1')
        self.assertFalse(os.path.exists(prefix + '.md'))
        self.assertIn('case-1.a2.md', os.listdir(os.path.join(self.dir, 'runs', 'discarded')))

    def test_the_machine_never_got_quiet(self):
        binary = fake_stress(self.dir, 'stress')
        with mock.patch.object(machine, 'wait_quiet', return_value=False):
            r = self.measure(binary)
        self.assertIsNone(r.error, 'a busy machine is not a failure of the binary')
        self.assertTrue(r.gave_up)
        self.assertIn('never got quiet', r.noisy)
        self.assertFalse(r.counts or r.ran)
        self.assertEqual(calls(binary), 0)

    def test_the_waits_share_one_deadline(self):
        waits = []

        def wait(max_wait_s=None, say=None):
            waits.append(max_wait_s)
            time.sleep(0.05)
            return True
        with mock.patch.dict(os.environ, {'PERF_MAX_WAIT': '100'}), \
                mock.patch.object(machine, 'wait_quiet', wait), \
                mock.patch.object(machine, 'heavy_processes', return_value=['rustc']):
            self.measure(fake_stress(self.dir, 'stress'), attempts=3)
        self.assertEqual(len(waits), 3)
        self.assertLessEqual(waits[0], 100)
        self.assertLess(waits[2], waits[1])
        self.assertLess(waits[1], waits[0])

    def test_a_failed_run(self):
        r = self.measure(fake_stress(self.dir, 'stress', fail=True))
        self.assertEqual(r.rc, 2)
        self.assertIn('exit code 2', r.error)
        self.assertIn('no usable GPU', r.detail)
        self.assertFalse(r.counts or r.ran)


class SmokeTest(unittest.TestCase):
    def test_gross(self):
        base = metrics(fps=1000, p99=1.0, slow=0)
        self.assertEqual(perf.gross(base, metrics(fps=900, p99=1.4, slow=0)), [])
        self.assertIn('under 85%', perf.gross(base, metrics(fps=800))[0])
        self.assertIn('over 1.5x', perf.gross(base, metrics(p99=1.6))[0])
        self.assertIn('over 6.94 ms, the base had none', perf.gross(base, metrics(slow=1))[0])
        self.assertEqual(perf.gross(metrics(slow=3), metrics(slow=9)), [])
        self.assertEqual(perf.gross(base, metrics(fps=960), fps_min=0.95), [])
        self.assertEqual(len(perf.gross(base, metrics(fps=940), fps_min=0.95)), 1)

    def test_gross_capped_uses_6_94_ms(self):
        """The policy rule: frames over 6.94 ms where the base had none, capped or not."""
        capped = [runs.metrics(summary(cap=144, slow=s, slow75=s75), []) for s, s75 in
                  ((1500, 0), (1500, 3))]
        self.assertEqual(perf.gross(*capped), [], 'the base had frames over 6.94 ms')
        none = runs.metrics(summary(cap=144, slow=0, slow75=0), [])
        some = runs.metrics(summary(cap=144, slow=2, slow75=0), [])
        self.assertIn('2 frames over 6.94 ms', perf.gross(none, some)[0])

    def test_gross_compares_p99_within_one_skip_mode(self):
        """Capped on the NVIDIA, a new run in the high skip mode doubles p99 by itself."""
        base, high = metrics(p99=7.5, mode='low'), metrics(p99=13.9, mode='high')
        self.assertIn('over 1.5x', perf.gross(base, high)[0])
        self.assertEqual(perf.gross(base, high, modes=True), [])
        self.assertIn('over 1.5x', perf.gross(base, metrics(p99=13.9, mode='low'), modes=True)[0])
        self.assertIn('under 85%', perf.gross(base, metrics(fps=500, p99=13.9, mode='high'),
                                               modes=True)[0])

    def result(self, *problems, new_error=None, base_error=None):
        r = perf.SmokeResult(Case())
        for p in problems:
            r.pairs.append((run(metrics()), run(metrics(fps=500 if p else 1000))))
            r.problems.append(p)
        if new_error or base_error:
            r.pairs.append((run(error=base_error) if base_error else run(metrics()),
                            run(error=new_error) if new_error else run(metrics())))
        return r

    def test_status_and_verdict(self):
        self.assertEqual(self.result([]).status, 'ok')
        once = self.result(['fps 500 under 85% of 1000'], [])
        self.assertEqual(once.status, 'ok')
        self.assertIn('the repeat did not', once.verdict())
        twice = self.result(['a'], ['fps 500 under 85% of 1000'])
        self.assertEqual(twice.status, 'worse')
        self.assertEqual(twice.verdict(), 'WORSE: fps 500 under 85% of 1000')
        self.assertEqual(self.result(new_error='exit code 4').status, 'failed')
        self.assertEqual(self.result(base_error='exit code 2').status, 'unmeasured')
        skipped = perf.SmokeResult(Case(), skipped='no cave')
        self.assertEqual((skipped.status, skipped.verdict()), ('skipped', 'skipped: no cave'))
        lost = perf.SmokeResult(Case(), lost='new has no cave-path scenario')
        self.assertEqual((lost.status, lost.verdict()),
                         ('failed', 'FAILED: new has no cave-path scenario'))

    def test_a_noisy_run_is_never_ok(self):
        for b, n in ((run(metrics(), noisy='overlapped rustc'), run(metrics())),
                     (run(metrics()), run(metrics(), noisy='seconds under 100 fps (0)')),
                     (run(metrics()), None),
                     (run(metrics()), run(noisy='never got quiet', gave_up=True))):
            r = perf.SmokeResult(Case(), pairs=[(b, n)], problems=[[]])
            self.assertEqual(r.status, 'unmeasured', (b, n))
            self.assertTrue(r.verdict().startswith('not measured: '), r.verdict())
            self.assertEqual(perf.exit_code([r]), 2)
        # A clean first pair that looked worse and a noisy repeat confirm nothing.
        r = perf.SmokeResult(Case(), pairs=[(run(metrics()), run(metrics(fps=500))),
                                            (run(metrics()), run(metrics(), noisy='overlapped'))],
                             problems=[['fps 500 under 85% of 1000']])
        self.assertEqual(r.status, 'unmeasured')
        self.assertIn('the first pair looked worse', r.verdict())

    def test_both_sides_failing_is_the_machine(self):
        """No usable GPU fails both sides: that says nothing about the new binary."""
        both = self.result(new_error='exit code 2', base_error='exit code 2')
        self.assertEqual(both.status, 'unmeasured')
        self.assertIn('base run exit code 2; new run exit code 2', both.verdict())
        self.assertEqual(perf.exit_code([both]), 2)
        # The new run failing after a base that did not count is not blamed either.
        r = perf.SmokeResult(Case(), pairs=[(run(noisy='never got quiet', gave_up=True),
                                             run(error='exit code 2'))])
        self.assertEqual(r.status, 'unmeasured')
        # A noisy base still ran to the end: the new binary's failure is its own.
        r = perf.SmokeResult(Case(), pairs=[(run(metrics(), noisy='overlapped rustc'),
                                             run(error='exit code 4'))])
        self.assertEqual(r.status, 'failed')

    def test_exit_code(self):
        ok, worse = self.result([]), self.result(['a'], ['b'])
        self.assertEqual(perf.exit_code([ok]), 0)
        self.assertEqual(perf.exit_code([ok, worse]), 1)
        self.assertEqual(perf.exit_code([ok, self.result(base_error='x')]), 2)
        self.assertEqual(perf.exit_code([worse, self.result(base_error='x')]), 1)
        self.assertEqual(perf.exit_code([perf.SmokeResult(Case(), skipped='x')]), 2,
                         'nothing measured is not ok')
        self.assertEqual(perf.exit_code([ok, perf.SmokeResult(Case(), skipped='x')]), 2)
        self.assertEqual(perf.exit_code([perf.SmokeResult(Case(), lost='x')]), 1)

    def test_table(self):
        noisy = perf.SmokeResult(Case(scenario='dive'),
                                 pairs=[(run(metrics(fps=30)), run(metrics(fps=30),
                                                                   noisy='seconds under 100 fps'))],
                                 problems=[[]])
        table = perf.smoke_table([self.result([]), perf.SmokeResult(Case(scenario='cave-path'),
                                                                     skipped='no cave'), noisy])
        lines = table.splitlines()
        self.assertEqual(lines[0], '| case | fps | p99 ms | frames > 6.94 ms | verdict |')
        self.assertEqual(lines[2], '| rtx/surface/r16/uncapped | 1000 -> 1000 (+0.0 %) | '
                                   '1 -> 1 | 0 -> 0 | ok |')
        self.assertEqual(lines[3], '| rtx/cave-path/r16/uncapped | - | - | - | skipped: no cave |')
        self.assertEqual(lines[4], '| rtx/dive/r16/uncapped | - | - | - | not measured: new run '
                                   'seconds under 100 fps |', 'no numbers from a noisy run')

    def test_usage_errors(self):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            self.assertEqual(perf.main(['smoke', 'walk']), 64)
            self.assertEqual(perf.main(['nothing']), 64)
            self.assertEqual(perf.main(['check', '--dry-run', '--', '--bodies', '1']), 64)
            self.assertEqual(perf.main(['smoke', 'forest', '--', '--radius', '8']), 64)
            self.assertEqual(perf.main(['cases']), 0)
        self.assertIn("'walk' is not a GPU", err.getvalue())
        self.assertIn('already sets --radius', err.getvalue())
        self.assertIn('rtx/surface/r32/144', out.getvalue())

    def test_refuses_to_run_inside_quiet_sh(self):
        err = io.StringIO()
        with mock.patch.dict(os.environ, {'BOXEL_QUIET_SLOT': '1'}), \
                mock.patch.object(perf, 'tree_binary') as build, \
                contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(err):
            self.assertEqual(perf.main(['smoke']), 64)
            self.assertEqual(perf.main(['bisect', 'surface', 'HEAD~1', 'HEAD']), 64)
        build.assert_not_called()
        self.assertIn('do not run perf-* through tools/quiet.sh', err.getvalue())


class SmokeRunTest(FakeMachine):
    """smoke_case and cmd_smoke on fake binaries."""

    def smoke(self, base, new, case=Case(), **kw):
        return perf.smoke_case(case, base, new, os.path.join(self.dir, 'smoke'), 3, **kw)

    def test_a_clean_pair(self):
        r = self.smoke(self.binary('base'), self.binary('new'))
        self.assertEqual((r.status, perf.exit_code([r])), ('ok', 0))

    def test_a_throttled_window_on_every_attempt_is_not_ok(self):
        """Both sides at 30 fps (a hidden window): equal numbers, but nothing was measured."""
        slow = summary(fps=30, p99=33, slow=300, timeline_fps=[30, 30, 30])
        r = self.smoke(self.binary('base', data=slow), self.binary('new', data=slow))
        self.assertEqual(r.status, 'unmeasured')
        self.assertIn('under 100 fps', r.verdict())
        self.assertEqual(perf.exit_code([r]), 2)

    def test_a_busy_machine_hides_nothing(self):
        """A build overlapping every attempt of both sides: never an ok."""
        base, new = self.binary('base'), self.binary('new', data=summary(fps=800))
        with mock.patch.object(machine, 'heavy_processes', return_value=['rustc']):
            r = self.smoke(base, new)
        self.assertEqual(r.status, 'unmeasured')
        self.assertEqual(calls(base.path), runs.ATTEMPTS)
        self.assertEqual(calls(new.path), 0, 'a base that does not count ends the pair')

    def test_no_gpu_on_both_sides_is_not_the_branch(self):
        r = self.smoke(self.binary('base', fail=True), self.binary('new', fail=True), flip=True)
        self.assertEqual(r.status, 'unmeasured')
        self.assertEqual(perf.exit_code([r]), 2)

    def test_the_new_binary_failing_alone(self):
        base, new = self.binary('base'), self.binary('new', fail=True)
        r = self.smoke(base, new, flip=True)
        self.assertEqual(r.status, 'failed')
        self.assertEqual(calls(base.path), 1, 'the base runs to tell the binary from the machine')
        self.assertEqual(perf.exit_code([r]), 1)

    def test_a_new_run_that_waited_out_ends_the_pair(self):
        base, new = self.binary('base'), self.binary('new')
        with mock.patch.object(machine, 'wait_quiet', return_value=False):
            r = self.smoke(base, new, flip=True)
        self.assertEqual(r.status, 'unmeasured')
        self.assertTrue(r.gave_up)
        self.assertEqual(calls(base.path) + calls(new.path), 0)

    def test_a_base_with_a_fixed_arena_runs_radius_32_with_the_big_one(self):
        """perf-bisect across #57: the old binary gets the arena it needs, the new one grows."""
        old = self.binary('old')
        new = self.binary('new', help_text=HELP + '  --chunk-memory fixed|grow  grows\n')
        r = self.smoke(old, new, Case('rtx', 'surface', 32, 144))
        self.assertEqual(r.status, 'ok', self.output())
        with open(old.path + '.args') as f:
            self.assertIn('--arena-quads 50331648 --chunk-slots 98304', f.read())
        with open(new.path + '.args') as f:
            self.assertNotIn('--arena-quads', f.read())

    def test_scenarios_the_base_or_the_new_binary_lack(self):
        cave = Case(scenario='cave-path')
        old_base = self.binary('old', help_text=OLD_HELP)
        r = self.smoke(old_base, self.binary('new'), cave)
        self.assertEqual((r.status, perf.exit_code([r])), ('skipped', 2))
        r = self.smoke(self.binary('base'), self.binary('lost', help_text=OLD_HELP), cave)
        self.assertEqual((r.status, perf.exit_code([r])), ('failed', 1))
        self.assertIn('lost has no cave-path scenario and the base has it', r.verdict())

    def test_a_gross_regression_repeats_once(self):
        base, new = self.binary('base'), self.binary('new', data=summary(fps=500))
        r = self.smoke(base, new)
        self.assertEqual(r.status, 'worse')
        self.assertEqual((calls(base.path), calls(new.path)), (2, 2))

    def test_a_capped_nvidia_pair_in_different_skip_modes(self):
        low = {'data': summary(fps=143, p99=7.5, cap=144)}
        high = {'data': summary(fps=141, p99=13.9, cap=144), 'skip': HIGH_MODE}
        base, new = self.binary('base', **low), self.binary('new', **high)
        r = self.smoke(base, new, Case('rtx', 'forest', 16, 144))
        self.assertEqual(r.status, 'ok', self.output())
        self.assertEqual(r.verdict(), 'ok (p99 not compared: skip modes low -> high)')
        self.assertEqual((calls(base.path), calls(new.path)), (1, 1), 'nothing to repeat')
        # The 780M has no skip modes.
        r = self.smoke(base, new, Case('780m', 'forest', 16, 144))
        self.assertEqual(r.status, 'worse')
        self.assertIn('p99 13.90 ms over 1.5x 7.50 ms', r.verdict())

    def cmd_smoke(self, cases, base, new, extra=(), flags=()):
        a = perf.parser().parse_args(['smoke', *cases, '--duration', '3', *flags])
        a.extra = list(extra)
        with mock.patch.object(perf, 'resolve', side_effect=[base, new]), \
                mock.patch.object(perf, 'root', return_value=self.dir), \
                mock.patch.object(perf, 'merge_base', return_value='base'):
            return perf.cmd_smoke(a)

    def test_cmd_smoke_stops_once_the_machine_never_got_quiet(self):
        base, new = self.binary('base'), self.binary('new')
        with mock.patch.object(machine, 'wait_quiet', return_value=False) as wait:
            code = self.cmd_smoke(['surface', 'forest', 'dive'], base, new)
        self.assertEqual(code, 2)
        self.assertEqual(wait.call_count, 1, 'one wait out, not one per run')
        self.assertIn('not run, the machine never got quiet for an earlier case', self.output())
        self.assertIn('could not measure everything', self.output())

    def test_cmd_smoke_passes_extra_options_to_both_sides(self):
        base, new = self.binary('base'), self.binary('new')
        code = self.cmd_smoke(['forest'], base, new, extra=['--bodies', '10000'])
        self.assertEqual(code, 0, self.output())
        for b in (base, new):
            with open(b.path + '.args') as f:
                self.assertIn(' --bodies 10000 ', f.read())
        self.assertIn('rtx/forest/r16/uncapped --bodies 10000', self.output())

    def test_cmd_smoke_passes_new_extra_options_to_the_new_side_only(self):
        base, new = self.binary('base'), self.binary('new')
        code = self.cmd_smoke(['forest'], base, new, extra=['--bodies', '10'],
                              flags=['--new-extra', '--fog valley'])
        self.assertEqual(code, 0, self.output())
        with open(base.path + '.args') as f:
            args = f.read()
            self.assertIn(' --bodies 10 ', args)
            self.assertNotIn('--fog', args)
        with open(new.path + '.args') as f:
            self.assertIn(' --bodies 10 --fog valley ', f.read())
        self.assertIn('the new side also runs with --fog valley', self.output())
        self.assertIn('rtx/forest/r16/uncapped --bodies 10 [new: --fog valley]', self.output())

    def test_cmd_smoke_notes_identical_binaries(self):
        base = self.binary('base')
        same = os.path.join(self.dir, 'copy')
        with open(base.path) as f, open(same, 'w') as g:
            g.write(f.read())
        os.chmod(same, 0o755)
        self.cmd_smoke(['surface'], base, perf.Binary(same, 'copy'))
        self.assertIn('both binaries are the same', self.output())


# The cases a fake outside app declares.
APP_CASES = {'surface': 'superficie, vuelo a 64', 'underground': 'bajo tierra, vuelo a 64'}


class OutsideTest(FakeMachine):
    """An outside app's cases (runs.app_cases) and perf-smoke --outside on fake apps."""

    def app(self, name, cases=APP_CASES, **kw):
        return fake_stress(self.dir, name, cases=cases, **kw)

    def test_the_app_declares_its_cases(self):
        cases = runs.app_cases(self.app('app'))
        self.assertEqual(list(cases), ['surface', 'underground'])
        self.assertEqual(cases['underground'].label, 'bajo tierra, vuelo a 64')
        self.assertIsNone(cases['surface'].stress)
        with self.assertRaisesRegex(ValueError, 'exited with 1: not an outside app'):
            runs.app_cases(fake_stress(self.dir, 'stress'))
        for name, text in [('format', json.dumps({'format': 2, 'cases': []})),
                           ('garbage', 'surface, underground'),
                           ('twice', json.dumps({'format': 1, 'cases': [
                               {'name': 'a', 'label': 'x'}, {'name': 'a', 'label': 'y'}]}))]:
            with self.assertRaisesRegex(ValueError, 'printed no list of format 1', msg=name):
                runs.app_cases(self.app(name, cases=text))
        bad = self.app('bad', cases={n: 'x' for n in ('rtx', 'r16', 'cap60', 'uncapped',
                                                      'Big', 'ok-2')})
        with self.assertRaisesRegex(ValueError, 'Big, cap60, r16, rtx, uncapped \\('):
            runs.app_cases(bad)

    def test_a_case_of_the_app_runs_by_its_name(self):
        declared = runs.app_cases(self.app('app'))
        case = runs.parse_case('780m/underground/144', Case(), declared)
        self.assertEqual(str(case), '780m/underground/r16/144')
        self.assertEqual(case.label, 'bajo tierra, vuelo a 64, tope 144')
        args = case.args(10)
        self.assertEqual(args[:6], ['--perf-case', 'underground', '--adapter', '780m',
                                    '--backend', 'vulkan'])
        self.assertNotIn('--scenario', args)
        self.assertIn('--radius 16 --fps-limit 144 --duration 10', ' '.join(args))
        with self.assertRaisesRegex(ValueError, "'forest' is not a GPU"):
            runs.parse_case('forest', Case(), declared)
        with self.assertRaisesRegex(ValueError, "declares no 'surface' case"):
            runs.parse_case('rtx', Case(), {'underground': declared['underground']})
        with self.assertRaisesRegex(ValueError, 'already sets --perf-case'):
            runs.parse_case('surface', Case(extra=('--perf-case', 'x')), declared)
        self.assertTrue(runs.MEASURING.match('app: measuring surface for 10 s'))
        self.assertTrue(runs.MEASURING.match('stress: measuring fly for 10 s'))

    def cmd_smoke(self, base, new, cases=(), extra=()):
        argv = ['smoke', '--outside', '--base', base, '--new', new, '--duration', '3', *cases]
        if extra:
            argv += ['--', *extra]
        with mock.patch.object(perf, 'root', return_value=self.dir), \
                contextlib.redirect_stderr(io.StringIO()) as err:
            code = perf.main(argv)
        self.said.append(err.getvalue())
        return code

    def test_every_declared_case_against_the_base(self):
        base, new = self.app('base'), self.app('new', data=summary(fps=950))
        self.assertEqual(self.cmd_smoke(base, new, extra=['--lod', 'near']), 0, self.output())
        with open(new + '.args') as f:
            runs_of_new = f.read().splitlines()
        self.assertEqual([line.split()[:2] for line in runs_of_new],
                         [['--perf-case', 'surface'], ['--perf-case', 'underground']])
        self.assertTrue(all(' --lod near --label new-file-' in line for line in runs_of_new))
        self.assertEqual(calls(base), 2)
        self.assertIn('| rtx/underground/r16/uncapped --lod near | 1000 -> 950 (-5.0 %)',
                      self.output())

    def test_the_same_binary_on_both_sides_checks_the_app(self):
        one = self.app('app')
        self.assertEqual(self.cmd_smoke(one, one, ['surface']), 0, self.output())
        self.assertIn('both binaries are the same: a check of the app', self.output())
        self.assertEqual(calls(one), 2)

    def test_cases_one_side_lacks(self):
        base = self.app('base', cases={'surface': 's', 'gone': 'g'})
        new = self.app('new', cases={'surface': 's', 'coast': 'c'})
        self.assertEqual(self.cmd_smoke(base, new, ['coast']), 2, self.output())
        self.assertIn('base file-', self.output())
        self.assertIn('has no coast scenario |', self.output())
        self.assertEqual(self.cmd_smoke(base, new, ['gone']), 1, self.output())
        self.assertIn('has no gone scenario and the base has it', self.output())
        # A base from before the app declared cases: nothing to compare against.
        old = fake_stress(self.dir, 'old')
        self.assertEqual(self.cmd_smoke(old, new, ['surface']), 2, self.output())
        self.assertIn('the base declares no cases', self.output())
        self.assertEqual(calls(old) + calls(new), 0)

    def test_two_built_binaries_and_cases_from_the_new_one(self):
        new = self.app('new')
        self.assertEqual(self.cmd_smoke('HEAD', new), 64)
        self.assertIn("--outside takes --base BIN, a built binary of the app", self.output())
        self.assertEqual(self.cmd_smoke(new, fake_stress(self.dir, 'stress')), 2)
        self.assertIn('not an outside app', self.output())
        self.assertEqual(self.cmd_smoke(new, new, ['dive']), 64)
        self.assertIn("'dive' is not a GPU", self.output())

    def test_cases_lists_the_apps(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(perf.main(['cases', '--outside', self.app('app')]), 0)
        self.assertIn('  underground  --perf-case underground  (bajo tierra, vuelo a 64)',
                      self.output())


class TreeBinaryTest(unittest.TestCase):
    def setUp(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        self.dir = d.name
        self.target = os.path.join(self.dir, 'target')
        self.builds = 0
        for patch in (mock.patch.object(perf, 'root', return_value=self.dir),
                      mock.patch.object(perf, 'say', lambda text='': None),
                      mock.patch.dict(os.environ, {'CARGO_TARGET_DIR': self.target,
                                                   'PERF_TARGET_DIR': os.path.join(self.dir,
                                                                                   'perf')}),
                      mock.patch.object(perf, 'cargo_build', self.build)):
            patch.start()
            self.addCleanup(patch.stop)

    def build(self, cwd, target, log):
        self.builds += 1
        out = os.path.join(target, 'release', 'examples', 'stress')
        os.makedirs(os.path.dirname(out), exist_ok=True)
        with open(out, 'w') as f:
            f.write(f'binary {self.builds}\n')
        return out

    def test_the_tree_never_shares_the_base_target_dir(self):
        with tempfile.TemporaryDirectory() as d, \
                mock.patch.dict(os.environ, {'PERF_TARGET_DIR': d, 'CARGO_TARGET_DIR': d}), \
                mock.patch.object(perf, 'cargo_build') as build:
            with self.assertRaises(perf.PerfError) as e:
                perf.tree_binary()
        self.assertIn('PERF_TARGET_DIR', str(e.exception))
        build.assert_not_called()

    def test_a_target_dir_another_tree_built_in_is_refused(self):
        perf.claim(self.target, '/some/other/worktree')
        with self.assertRaises(perf.PerfError) as e:
            perf.tree_binary()
        self.assertIn('/some/other/worktree', str(e.exception))
        self.assertEqual(self.builds, 0)
        perf.claim(os.path.join(self.dir, 'mine'), perf.TREE)
        perf.claim(os.path.join(self.dir, 'mine'), perf.TREE)

    def test_dirty_builds_never_share_a_path(self):
        state = ('a' * 40, 'files', True)
        with mock.patch.object(perf, 'tree_state', return_value=state):
            first = perf.tree_binary()
            second = perf.tree_binary()
        self.assertNotEqual(first.path, second.path)
        self.assertNotEqual(first.label, second.label)
        with open(first.path) as f:
            self.assertEqual(f.read(), 'binary 1\n', 'a later build left the first one alone')
        self.assertIsNone(first.sha)

    def test_a_tree_that_changes_during_the_build_gives_no_binary(self):
        states = iter([('a' * 40, 'files', False), ('a' * 40, 'edited', True)])
        with mock.patch.object(perf, 'tree_state', lambda: next(states)):
            with self.assertRaises(perf.PerfError) as e:
                perf.tree_binary()
        self.assertIn('changed while stress was building', str(e.exception))
        self.assertFalse(os.path.exists(perf.bench('bins', 'a' * 40, 'stress')))
        self.assertEqual([n for n in os.listdir(perf.bench('bins'))
                          if n.startswith('.stage-')], [])

    def test_a_clean_build_is_cached_by_its_commit(self):
        state = ('b' * 40, 'files', False)
        with mock.patch.object(perf, 'tree_state', return_value=state):
            first = perf.tree_binary()
            again = perf.tree_binary()
        self.assertEqual(first.path, perf.bench('bins', 'b' * 40, 'stress'))
        self.assertEqual((again.path, self.builds), (first.path, 1))

    def test_tree_state_sees_edits_and_new_files(self):
        with tempfile.TemporaryDirectory() as repo:
            def git(*args):
                subprocess.run(['git', '-c', 'user.email=t@t', '-c', 'user.name=t', *args],
                               cwd=repo, check=True, capture_output=True)
            git('init', '-q')
            with open(os.path.join(repo, 'a.rs'), 'w') as f:
                f.write('1\n')
            git('add', 'a.rs')
            git('commit', '-qm', 'x')
            with mock.patch.object(perf, 'TREE', repo):
                clean = perf.tree_state()
                self.assertFalse(clean[2])
                with open(os.path.join(repo, 'b.rs'), 'w') as f:
                    f.write('new\n')
                untracked = perf.tree_state()
                self.assertNotEqual(untracked[1], clean[1])
                self.assertFalse(untracked[2], 'an untracked file alone keeps the tree clean')
                with open(os.path.join(repo, 'a.rs'), 'w') as f:
                    f.write('2\n')
                edited = perf.tree_state()
                self.assertTrue(edited[2])
                self.assertNotEqual(edited[1], untracked[1])
                self.assertEqual(perf.tree_state(), edited)


class CheckTest(unittest.TestCase):
    def test_next_number(self):
        self.assertEqual(perf.next_number([]), 1)
        self.assertEqual(perf.next_number(['perf-control-2', 'perf-control-10', 'perf-control-x']),
                         11)
        self.assertEqual(perf.next_number(['perf-control-2'], {'number': 4}), 5)

    def test_last_checkpoint(self):
        with mock.patch.object(perf, 'commit', lambda ref: 'sha-of-' + ref):
            self.assertEqual(perf.last_checkpoint({'sha': 's', 'tag': 'perf-control-3'}, []),
                             ('s', 'perf-control-3'))
            self.assertEqual(perf.last_checkpoint(None, ['perf-control-2', 'perf-control-10',
                                                         'perf-control-x']),
                             ('sha-of-perf-control-10', 'perf-control-10'),
                             'a lost control/last falls back to the highest tag')
            self.assertEqual(perf.last_checkpoint(None, []), (None, None))

    def test_schedule(self):
        cases = [Case(scenario='surface'), Case(scenario='forest'), Case(scenario='dive')]
        sides = {c: ['base', 'new'] for c in cases}
        sides[cases[2]] = ['new']
        plan = perf.schedule(cases, sides, 3)
        self.assertEqual(len(plan), 3 * 5)
        first = [(c.scenario, s) for r, c, s in plan if r == 0]
        self.assertEqual(first, [('surface', 'base'), ('surface', 'new'), ('forest', 'new'),
                                 ('forest', 'base'), ('dive', 'new')])
        self.assertEqual([c.scenario for r, c, s in plan if r == 1 and s == 'new'],
                         ['forest', 'dive', 'surface'])
        # Each case runs base first in some rounds and new first in others.
        firsts = {(r, c): s for r, c, s in reversed(plan)}
        self.assertEqual({firsts[(r, cases[0])] for r in range(3)}, {'base', 'new'})

    def test_worse(self):
        self.assertTrue(perf.worse('fps', [1000, 1010, 990], [850, 860, 870]))
        self.assertFalse(perf.worse('fps', [1000, 1010, 800], [850, 860, 870]), 'ranges overlap')
        self.assertFalse(perf.worse('fps', [1000, 1010, 990], [950, 940, 960]), 'under 10 %')
        self.assertTrue(perf.worse('p99', [1.0, 1.1, 1.0], [1.5, 1.4, 1.6]))
        self.assertFalse(perf.worse('p99', [1.0, 1.1, 1.0], [0.5, 0.6, 0.5]), 'better')
        self.assertTrue(perf.worse('slow', [0, 0, 0], [1, 3, 2]))
        self.assertFalse(perf.worse('slow', [0, 0, 0], [0, 3, 2]))
        self.assertTrue(perf.worse('slow', [10, 12, 11], [40, 30, 35]))
        self.assertFalse(perf.worse('skipped', [1, 1, 1], [90, 100, 80]), 'never flagged')
        self.assertFalse(perf.worse('catch_up', [None], [30.0]))

    def test_capped_slow_frames_are_over_7_5_ms(self):
        capped, uncapped = Case(cap=144), Case()
        base = [metrics(slow=1500, slow75=10, mode='low')] * 3
        new = [metrics(slow=1500, slow75=40, mode='low')] * 3
        self.assertEqual(perf.flagged_metrics(capped, {'base': base, 'new': new}), ['slow'])
        self.assertEqual(perf.flagged_metrics(uncapped, {'base': base, 'new': new}), [])

    # rtx/forest/r16/144 of checkpoint round 1: (p99, slow75, mode) of each run.
    FOREST_BASE = [(13.79, 54, 'high'), (7.49, 27, 'low'), (7.47, 28, 'low')]
    FOREST_NEW = [(13.91, 77, 'high'), (13.91, 66, 'high'), (13.93, 74, 'high')]

    def runs_of(self, *runs_):
        return [metrics(p99=p99, slow=1500, slow75=s75, mode=mode) for p99, s75, mode in runs_]

    def test_capped_nvidia_is_judged_within_each_skip_mode(self):
        r = {'base': self.runs_of(*self.FOREST_BASE), 'new': self.runs_of(*self.FOREST_NEW)}
        rtx = Case('rtx', 'forest', 16, 144)
        self.assertEqual(perf.flagged_metrics(rtx, r), [],
                         'more runs in the high mode is chance, not a regression')
        self.assertTrue(perf.unjudged(rtx, 'p99', r['base'], r['new']))
        self.assertFalse(perf.unjudged(rtx, 'fps', r['base'], r['new']))
        # The 780M has no skip modes: the same numbers there are worse.
        self.assertEqual(perf.flagged_metrics(Case('780m', 'forest', 16, 144), r), ['p99'])
        self.assertFalse(perf.unjudged(Case('780m', 'forest', 16, 144), 'p99', r['base'],
                                       r['new']))

    def test_a_change_within_one_skip_mode_is_still_worse(self):
        rtx = Case('rtx', 'surface', 16, 144)
        low = {'base': self.runs_of((7.4, 20, 'low'), (7.5, 25, 'low'), (13.9, 90, 'high')),
               'new': self.runs_of((9.6, 24, 'low'), (9.8, 22, 'low'), (13.9, 95, 'high'))}
        self.assertEqual(perf.flagged_metrics(rtx, low), ['p99'])
        high = {'base': self.runs_of((13.9, 100, 'high'), (14.0, 110, 'high'), (7.4, 20, 'low')),
                'new': self.runs_of((13.9, 250, 'high'), (14.0, 260, 'high'), (7.4, 21, 'low'))}
        self.assertEqual(perf.flagged_metrics(rtx, high), ['slow'])

    def test_a_skip_mode_needs_two_runs_per_side(self):
        rtx = Case('rtx', 'surface', 16, 144)
        r = {'base': self.runs_of((7.4, 20, 'low'), (13.9, 100, 'high'), (14.0, 110, 'high')),
             'new': self.runs_of((9.9, 60, 'low'), (13.9, 105, 'high'), (13.95, 100, 'high'))}
        self.assertEqual(list(perf.mode_groups(r['base'], r['new'])), ['high'])
        self.assertEqual(perf.flagged_metrics(rtx, r), [], 'one low run per side is a coin toss')
        self.assertFalse(perf.unjudged(rtx, 'p99', r['base'], r['new']))
        # Runs without a mode (older results) are never compared.
        none = {'base': [metrics(p99=7.4)] * 3, 'new': [metrics(p99=13.9)] * 3}
        self.assertEqual(perf.flagged_metrics(rtx, none), [])
        self.assertTrue(perf.unjudged(rtx, 'p99', none['base'], none['new']))

    def test_fisher(self):
        self.assertAlmostEqual(perf.fisher(0, 3, 3, 3), 0.1)
        self.assertAlmostEqual(perf.fisher(1, 3, 3, 3), 0.4)
        self.assertAlmostEqual(perf.fisher(2, 3, 2, 3), 1.0)
        self.assertAlmostEqual(perf.fisher(3, 21, 6, 21), 0.4537, places=4)
        self.assertEqual(perf.fisher(0, 0, 0, 0), 1.0)

    def test_report_shows_the_skip_modes(self):
        forest = Case('rtx', 'forest', 16, 144)
        rows = {forest: {'base': self.runs_of(*self.FOREST_BASE),
                         'new': self.runs_of(*self.FOREST_NEW), 'errors': [], 'unmeasured': []},
                Case('rtx', 'forest', 16, 0): {'base': [metrics()] * 3, 'new': [metrics()] * 3,
                                               'errors': [], 'unmeasured': []}}
        text = perf.render_report(self.info(), rows)
        self.assertIn('**Resultado:** sin empeoramientos claros.', text)
        self.assertIn('| Puesta al día ms | Modo alto | Estado NVIDIA |', text)
        self.assertIn('| 9.58 -> 13.92 | 36.33 -> 72.33 |', text)
        self.assertIn('| 1/3 -> 3/3 | - -> - |', text)
        self.assertIn('| 20 -> 20 | - -> - | - -> - |', text, 'uncapped: no mode')
        self.assertIn('- p99 y frames lentos sin juzgar, porque las corridas cayeron en modos '
                      'distintos: rtx/forest/r16/144 (modo alto 1/3 -> 3/3, Fisher p = 0.40).',
                      text)
        self.assertNotIn('**peor** |', text)
        self.assertNotIn('—', text)

    def test_pstates_text(self):
        self.assertEqual(perf.pstates_text([None, {}]), '-')
        self.assertEqual(perf.pstates_text([{'P0': 1.0}, {'P0': 0.8, 'P2': 0.2}]),
                         'P0 90 %, P2 10 %')
        self.assertEqual(perf.pstates_text([{'P8': 0.5, 'P10': 0.5}]), 'P8 50 %, P10 50 %')

    def test_slow_runs_of_round_1(self):
        """Round 1's uncapped surface: the two new runs in the NVIDIA's slow state, and only
        them."""
        case = Case('rtx', 'surface', 16, 0)
        r = round1_surface()
        found = perf.slow_runs(case, r['base'], r['new'])
        self.assertEqual([(side, i) for side, i, _, _ in found], [('new', 1), ('new', 2)])
        self.assertEqual([(slow, seconds) for _, _, slow, seconds in found], [(19, 20), (10, 20)])
        self.assertEqual(perf.slow_runs(Case('rtx', 'surface', 16, 144), r['base'], r['new']), [],
                         'capped runs sit at the cap')
        self.assertEqual(perf.slow_runs(case, [], r['new']), [], 'a first round')
        self.assertEqual(perf.spread_text(case, r), ' (ahora 2043..2291)')
        self.assertEqual(perf.spread_text(Case('rtx', 'surface', 16, 144), r), '')
        # Without the slow runs the base is not slow against the new one either.
        fast = round1_surface(new=ROUND1_SURFACE['new'][:1])
        self.assertEqual(perf.slow_runs(case, fast['base'], fast['new']), [])
        self.assertEqual(perf.spread_text(case, fast), '')

    def test_a_short_dip_is_not_a_slow_run(self):
        """Two seconds under the other side (a bisect base run had that) are no slow state."""
        case = Case()
        base = [metrics(fps=1000, fps_s=[1000] * 20) for _ in range(3)]
        dip = [1000] * 20
        dip[3] = dip[11] = 850
        self.assertEqual(perf.slow_runs(case, base, [metrics(fps_s=dip)]), [])
        dip[4] = dip[5] = dip[6] = 850
        self.assertEqual(perf.slow_runs(case, base, [metrics(fps_s=dip)]), [('new', 0, 5, 20)])
        self.assertEqual(perf.slow_seconds(metrics(fps_s=[900, None]), [metrics(fps_s=[1000])]),
                         (1, 1), 'the seconds both have')
        self.assertEqual(perf.slow_seconds(metrics(), base), (0, 0), 'runs from before fps_s')

    def info(self, **more):
        info = {'number': 3, 'date': '2026-10-20', 'tag': 'perf-control-3', 'new': 'a' * 40,
                'base': 'bbbbbbbb', 'base_sha': 'b' * 40, 'base_tag': 'perf-control-2',
                'rounds': 3, 'duration': 20, 'skipped': [], 'discarded': 2, 'noisy': 0,
                'folder': 'bench-results/control/3-2026-10-20', 'new_failed': [],
                'incomplete': [], 'checkpoint': True, 'off_main': False}
        info.update(more)
        return info

    def test_report(self):
        rtx = Case('rtx', 'surface', 16, 0)
        cave = Case('780m', 'cave-path', 16, 144)
        same = [metrics(fps=f, pstates={'P0': 1.0}) for f in (1000, 1010, 990)]
        results = {
            rtx: {'base': same, 'new': [metrics(fps=f, pstates={'P0': 0.5, 'P2': 0.5})
                                        for f in (700, 710, 690)], 'errors': [],
                  'unmeasured': []},
            cave: {'base': same, 'new': same[:2], 'errors': ['new, ronda 3: exit code 4'],
                   'unmeasured': []},
        }
        text = perf.render_report(self.info(skipped=['rtx/cave-path/r32/144 solo ahora'],
                                            new_failed=['780m/cave-path/r16/144 (exit code 4)'],
                                            incomplete=['780m/cave-path/r16/144'],
                                            checkpoint=False, tag=None), results)
        self.assertTrue(text.startswith('# Ronda de control 3 (2026-10-20)\n'))
        self.assertIn('`bbbbbbbb` (`perf-control-2`)', text)
        self.assertNotIn('(tag `', text, 'an incomplete round gets no tag')
        self.assertIn('**Resultado:** empeoró en rtx/surface/r16/uncapped. Buscar el merge con '
                      '`just perf-bisect rtx/surface/r16/uncapped bbbbbbbb aaaaaaaa --metric fps`.',
                      text)
        self.assertIn('| superficie, vuelo a 64, sin tope | 1000 -> 700 **peor** | 0.50 -> 0.50 |',
                      text)
        self.assertIn('| P0 100 % -> P0 50 %, P2 50 % |', text)
        self.assertIn('## Radeon 780M', text)
        self.assertNotIn('Estado NVIDIA |\n|---|---:|---:|---:|---:|---:|---:|\n| cueva', text)
        self.assertIn('**El binario nuevo falló:** 780m/cave-path/r16/144 (exit code 4).', text)
        self.assertIn('**Ronda incompleta:** faltan corridas de 780m/cave-path/r16/144 (ver las '
                      'notas).', text)
        self.assertIn('No cuenta como control', text)
        self.assertIn('- Sin datos: rtx/cave-path/r32/144 solo ahora.', text)
        self.assertIn('- 780m/cave-path/r16/144 falló: new, ronda 3: exit code 4', text)
        self.assertIn('Corridas descartadas y repetidas: 2', text)
        self.assertNotIn('## Data', text, 'bench_report_test parses docs with a Data section')
        self.assertNotIn('—', text)

    def test_report_suggests_the_bisect_of_the_flagged_metric(self):
        case = Case('rtx', 'surface', 16, 0)
        base = [metrics(catch_up=c) for c in (20.0, 21.0, 19.0)]
        new = [metrics(catch_up=c) for c in (30.0, 31.0, 29.0)]
        text = perf.render_report(self.info(), {case: {'base': base, 'new': new, 'errors': [],
                                                       'unmeasured': []}})
        self.assertIn(f'`just perf-bisect {case} bbbbbbbb aaaaaaaa --metric catch_up`', text)
        self.assertIn('(tag `perf-control-3`)', text)

    def test_report_shows_the_slow_runs(self):
        case = Case('rtx', 'surface', 16, 0)
        r = dict(round1_surface(), errors=[], unmeasured=[])
        text = perf.render_report(self.info(), {case: r})
        self.assertIn('**Resultado:** sin empeoramientos claros.', text, 'the runs overlap')
        self.assertIn('**Corridas lentas:** rtx/surface/r16/uncapped tiene corridas sin tope',
                      text)
        self.assertIn('| superficie, vuelo a 64, sin tope | 2308 -> 2155 (ahora 2043..2291) |',
                      text)
        self.assertIn('  - rtx/surface/r16/uncapped: ronda 2 de ahora (19 de 20 s lentos, 80.8 W '
                      'contra 85.8 W de las demás), ronda 3 de ahora (10 de 20 s lentos, 80.5 W '
                      'contra 85.8 W de las demás). Media de fps sin ellas: 2308 -> 2291.', text)
        self.assertIn('[sin-tope-lento-2026-10.md](sin-tope-lento-2026-10.md)', text)
        self.assertTrue(os.path.exists(os.path.join(perf.TREE, perf.SLOW_DOC)), perf.SLOW_DOC)
        # Every run of a side slow: that can be the code.
        slow = round1_surface(new=ROUND1_SURFACE['new'][1:2] * 3)
        text = perf.render_report(self.info(), {case: dict(slow, errors=[], unmeasured=[])})
        self.assertIn('Son todas las de ahora: puede que sea el código.', text)
        self.assertIn('**Resultado:** empeoró en rtx/surface/r16/uncapped.', text)
        # Nothing slow, nothing said.
        fast = round1_surface(new=ROUND1_SURFACE['new'][:1])
        text = perf.render_report(self.info(), {case: dict(fast, errors=[], unmeasured=[])})
        self.assertNotIn('Corridas lentas', text)
        self.assertNotIn('..', text.split('## Notas')[0].replace('../', ''))

    def test_first_report(self):
        results = {Case(): {'base': [], 'new': [metrics()], 'errors': [], 'unmeasured': []}}
        text = perf.render_report(self.info(number=1, base=None, base_sha=None, base_tag=None),
                                  results)
        self.assertIn('Primera ronda', text)
        self.assertIn('**Resultado:** primera ronda, sin comparación.', text)
        self.assertIn('| superficie, vuelo a 64, sin tope | 1000 | 0.50 | 1 | 0 | 0 | 20 | - | - |',
                      text)

    def dry_run(self, *args, last=None, tags='', main=True):
        out, err = io.StringIO(), io.StringIO()
        real_git = perf.git

        def git(*a, **kw):
            return tags if a[:2] == ('tag', '--list') else real_git(*a, **kw)
        with mock.patch.object(perf, 'read_last', return_value=last), \
                mock.patch.object(perf, 'git', git), \
                mock.patch.object(perf, 'on_main', return_value=main), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = perf.main(['check', '--dry-run', *args])
        return code, out.getvalue(), err.getvalue()

    def test_dry_run_prints_the_plan(self):
        code, out, _ = self.dry_run('--no-tag')
        self.assertEqual(code, 0, out)
        self.assertIn(f'{len(perf.MATRIX)} cases x 3 rounds', out)
        self.assertIn('dry run, nothing measured', out)

    def test_dry_run_against_the_last_checkpoint(self):
        head = perf.commit('HEAD')
        code, out, _ = self.dry_run(last={'number': 41, 'tag': 'perf-control-41', 'sha': head})
        self.assertEqual(code, 0, out)
        self.assertIn('perf-check: round 42 (perf-control-42)', out)
        self.assertIn('x 3 rounds x 2 sides of 20 s', out)
        self.assertIn(f'base: {head} (the last checkpoint, perf-control-41;', out)

    def test_a_lost_last_file_falls_back_to_the_highest_tag(self):
        head = perf.commit('HEAD')
        with mock.patch.object(perf, 'commit', lambda ref: head):
            code, out, _ = self.dry_run(tags='perf-control-2\nperf-control-7')
        self.assertEqual(code, 0, out)
        self.assertIn('round 8 (perf-control-8)', out)
        self.assertIn(f'base: {head} (the last checkpoint, perf-control-7 '
                      '(bench-results/control/last is missing)', out)
        self.assertNotIn('first checkpoint round', out)

    def test_a_commit_off_main_is_refused(self):
        code, out, err = self.dry_run(main=False)
        self.assertEqual(code, 2)
        self.assertIn('is not on main', err)
        code, out, _ = self.dry_run('--off-main', main=False)
        self.assertEqual(code, 0, out)
        self.assertIn('off main: the report stays in', out)


class CheckRunTest(FakeMachine):
    """cmd_check on fake binaries: what moves the base, what tags, what it exits with."""

    MATRIX = [Case('rtx', 'surface', 16, 0), Case('780m', 'forest', 16, 144),
              Case('rtx', 'cave-path', 16, 0)]

    def setUp(self):
        super().setUp()
        self.tags = []
        self.report = os.path.join(self.dir, 'control.md')
        real_git = perf.git

        def git(*a, **kw):
            if a[:2] == ('tag', '--list'):
                return '\n'.join(self.tags)
            if a[0] == 'tag':
                self.tags.append(a[1])
                return ''
            return real_git(*a, **kw)
        self.binaries = {}
        for patch in (mock.patch.object(perf, 'root', return_value=self.dir),
                      mock.patch.object(perf, 'MATRIX', self.MATRIX),
                      mock.patch.object(perf, 'git', git),
                      mock.patch.object(perf, 'commit', lambda ref: ref),
                      mock.patch.object(perf, 'subject', lambda sha: sha),
                      mock.patch.object(perf, 'on_main', return_value=True),
                      mock.patch.object(perf, 'report_path', lambda date: self.report),
                      mock.patch.object(perf, 'commit_binary',
                                        lambda sha, name='': self.binaries[sha])):
            patch.start()
            self.addCleanup(patch.stop)
        os.makedirs(perf.bench('control'))
        self.write_last('old')

    def write_last(self, sha):
        with open(perf.bench('control', 'last'), 'w') as f:
            json.dump({'number': 1, 'tag': 'perf-control-1', 'sha': sha}, f)
        self.tags = ['perf-control-1']

    def last(self):
        with open(perf.bench('control', 'last')) as f:
            return json.load(f)['sha']

    def check(self, old=None, new=None, *args):
        self.binaries['old'] = self.binary('old', **(old or {}))
        self.binaries['new'] = self.binary('new', **(new or {}))
        a = perf.parser().parse_args(['check', '--ref', 'new', '--rounds', '2', '--duration',
                                      '3', *args])
        a.extra = []
        code = perf.cmd_check(a)
        with open(self.report) as f:
            return code, f.read()

    def test_a_complete_round_moves_the_base_and_tags(self):
        code, text = self.check()
        self.assertEqual(code, 0, self.output())
        self.assertEqual(self.last(), 'new')
        self.assertEqual(self.tags, ['perf-control-1', 'perf-control-2'])
        self.assertIn('**Resultado:** sin empeoramientos claros.', text)

    def test_no_gpu_leaves_the_round_incomplete(self):
        """The RTX missing fails its cases on both sides: not a checkpoint, and not the binary."""
        code, text = self.check({'fail': 'nvidia'}, {'fail': 'nvidia'})
        self.assertEqual(code, 2, self.output())
        self.assertEqual(self.last(), 'old', 'the base stays')
        self.assertEqual(self.tags, ['perf-control-1'], 'no tag')
        self.assertIn('**Ronda incompleta:** faltan corridas de rtx/surface/r16/uncapped, '
                      'rtx/cave-path/r16/uncapped', text)
        self.assertNotIn('El binario nuevo falló', text)
        self.assertIn('INCOMPLETE round', self.output())

    def test_accept_partial(self):
        code, text = self.check({'fail': 'nvidia'}, {'fail': 'nvidia'}, '--accept-partial')
        self.assertEqual(code, 2)
        self.assertEqual(self.last(), 'new')
        self.assertIn('perf-control-2', self.tags)
        self.assertIn('Se aceptó igual', text)

    def test_the_new_binary_failing_where_the_old_one_ran(self):
        code, text = self.check(None, {'fail': 'nvidia'})
        self.assertEqual(code, 1)
        self.assertEqual(self.last(), 'old')
        self.assertIn('**El binario nuevo falló:** rtx/surface/r16/uncapped (exit code 2', text)

    def test_noisy_runs_stay_out_of_the_means(self):
        throttled = summary(fps=30, timeline_fps=[30, 30, 30])
        code, text = self.check(None, {'data': throttled})
        self.assertEqual(code, 2, self.output())
        self.assertEqual(self.last(), 'old')
        self.assertIn('| superficie, vuelo a 64, sin tope | sin datos | sin datos |', text)
        self.assertIn('no cuenta: new, ronda 1: seconds under 100 fps', text)
        self.assertNotIn(' -> 30', text)

    def test_a_scenario_the_new_binary_lost(self):
        code, text = self.check(None, {'help_text': OLD_HELP})
        self.assertEqual(code, 1)
        self.assertIn('rtx/cave-path/r16/uncapped (el binario nuevo ya no tiene el escenario)',
                      text)
        self.assertNotIn('solo ahora', text)
        self.assertEqual(self.last(), 'old')

    def test_a_scenario_only_the_new_binary_has(self):
        code, text = self.check({'help_text': OLD_HELP}, None)
        self.assertEqual(code, 0, self.output())
        self.assertIn('rtx/cave-path/r16/uncapped solo ahora (el binario de antes no tiene el '
                      'escenario)', text)

    def test_a_round_off_main_moves_nothing(self):
        with mock.patch.object(perf, 'on_main', return_value=False):
            a = perf.parser().parse_args(['check', '--ref', 'new', '--rounds', '1',
                                          '--duration', '3', '--off-main'])
            a.extra = []
            self.binaries['old'] = self.binary('old')
            self.binaries['new'] = self.binary('new')
            code = perf.cmd_check(a)
        self.assertEqual(code, 0, self.output())
        self.assertEqual(self.last(), 'old')
        self.assertEqual(self.tags, ['perf-control-1'])
        self.assertFalse(os.path.exists(self.report), 'no report in docs/rendimiento')

    def test_more_runs_in_the_high_skip_mode_is_not_worse(self):
        """Capped on the NVIDIA, a new binary whose runs all landed in the high skip mode."""
        with mock.patch.object(perf, 'MATRIX', [Case('rtx', 'forest', 16, 144)]):
            code, text = self.check({'data': summary(fps=143, p99=7.5, slow75=20, cap=144)},
                                    {'data': summary(fps=141, p99=13.9, slow75=70, cap=144),
                                     'skip': HIGH_MODE})
        self.assertEqual(code, 0, self.output())
        self.assertIn('**Resultado:** sin empeoramientos claros.', text)
        self.assertIn('| 0/2 -> 2/2 |', text)
        self.assertIn('sin juzgar, porque las corridas cayeron en modos distintos: '
                      'rtx/forest/r16/144 (modo alto 0/2 -> 2/2', text)
        self.assertIn('p99 13.90 ms, slow 0, high skip mode', self.output())

    def test_a_worse_round_says_so(self):
        code, text = self.check(None, {'data': summary(catch_up=40.0)})
        self.assertEqual(code, 1)
        self.assertIn('--metric catch_up', text)
        self.assertEqual(self.last(), 'new', 'a complete round is the next base')


class BisectTest(unittest.TestCase):
    def test_first_bad(self):
        for count in range(1, 12):
            for bad in range(count):
                calls = []

                def is_bad(i):
                    calls.append(i)
                    return i >= bad
                self.assertEqual(perf.first_bad(count, is_bad), bad, (count, bad))
                self.assertLessEqual(len(calls), math.ceil(math.log2(count)) if count > 1 else 0)
                self.assertNotIn(count - 1, calls, 'the last one is known to be bad')


class BisectRunTest(FakeMachine):
    """cmd_bisect on fake binaries: commit c0 is GOOD, c1..c4 the merges after it."""

    def setUp(self):
        super().setUp()
        self.binaries = {}
        real_git = perf.git

        def git(*a, **kw):
            if a[0] == 'rev-list':
                return 'c1\nc2\nc3\nc4'
            return real_git(*a, **kw)
        for patch in (mock.patch.object(perf, 'root', return_value=self.dir),
                      mock.patch.object(perf, 'git', git),
                      mock.patch.object(perf, 'commit', lambda ref: ref),
                      mock.patch.object(perf, 'subject', lambda sha: sha),
                      mock.patch.object(perf, 'commit_binary',
                                        lambda sha, name='': self.binaries[sha])):
            patch.start()
            self.addCleanup(patch.stop)

    def bisect(self, *args, case='surface', **per_commit):
        for sha in ('c0', 'c1', 'c2', 'c3', 'c4'):
            self.binaries[sha] = self.binary(sha, **per_commit.get(sha, {}))
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            code = perf.main(['bisect', case, 'c0', 'c4', '--duration', '3', *args])
        return code, self.output() + err.getvalue()

    def test_no_gpu_is_not_a_bad_commit(self):
        everyone = {sha: {'fail': True} for sha in ('c0', 'c1', 'c2', 'c3', 'c4')}
        code, out = self.bisect(**everyone)
        self.assertEqual(code, 2, out)
        self.assertNotIn('first bad commit', out)
        self.assertIn('could not measure c4', out)

    def test_a_busy_machine_is_not_a_bad_commit(self):
        with mock.patch.object(machine, 'wait_quiet', return_value=False):
            code, out = self.bisect()
        self.assertEqual(code, 2, out)
        self.assertNotIn('first bad commit', out)

    def test_a_gross_regression(self):
        slow = {'data': summary(fps=500)}
        code, out = self.bisect(c3=slow, c4=slow)
        self.assertEqual(code, 1, out)
        self.assertIn('first bad commit: c3', out)

    def test_a_crash_is_a_bad_commit(self):
        code, out = self.bisect(c2={'fail': True}, c3={'fail': True}, c4={'fail': True})
        self.assertEqual(code, 1, out)
        self.assertIn('first bad commit: c2', out)

    def test_the_checkpoint_rule_of_a_metric(self):
        """A catch-up regression the smoke check cannot see."""
        late = {'data': summary(catch_up=30.0)}
        code, out = self.bisect(c2=late, c3=late, c4=late)
        self.assertEqual(code, 2, out)
        self.assertIn('nothing to look for (--metric M', out)
        self.said.clear()
        with mock.patch.object(runs, 'measure', wraps=runs.measure) as measure:
            code, out = self.bisect('--metric', 'catch_up', '--rounds', '2', c2=late, c3=late,
                                    c4=late)
        self.assertEqual(code, 1, out)
        self.assertIn('first bad commit: c2', out)
        self.assertIn('catch_up: 20 -> 30 WORSE', out)
        self.assertIn('catch_up per run: base 20 / 20, new 30 / 30', out)
        self.assertTrue(all(c.kwargs['nvidia'] for c in measure.call_args_list),
                        'the bisect samples the NVIDIA on the RTX, like the round')

    def test_the_metric_rule_refuses_the_smoke_thresholds(self):
        """--fps-min and --p99-max only set the smoke check: with --metric they did nothing."""
        for option in (['--fps-min', '0.95'], ['--p99-max', '1.2']):
            code, out = self.bisect('--metric', 'fps', *option)
            self.assertEqual(code, 64, out)
            self.assertIn('--fps-min and --p99-max set the smoke check', out)
        self.assertEqual(calls(self.binaries['c0'].path), 0, 'nothing ran')
        self.said.clear()
        slow = {'data': summary(fps=900)}
        code, out = self.bisect('--fps-min', '0.95', c3=slow, c4=slow)
        self.assertEqual(code, 1, out)
        self.assertIn('first bad commit: c3', out)

    def test_slow_runs_are_named(self):
        """A run in the NVIDIA's slow state shows in the bisect, with the mean without it."""
        rounds = [summary(fps=1000, timeline_fps=[1000] * 4)] * 2
        slow = summary(fps=880, timeline_fps=[880] * 4)
        sides = {'base': [runs.metrics(s, []) for s in rounds],
                 'new': [runs.metrics(s, []) for s in (rounds[0], slow)]}
        lines = perf.slow_lines(Case(), sides)
        self.assertEqual(lines[0], '    slow run: new 2 is over 8% under the base runs in 4 of 4 s')
        self.assertIn("without them fps is 1000 -> 1000; a slow run among normal ones is the "
                      "GPU's slow state, not the code", lines[1])
        self.assertEqual(perf.slow_lines(Case(cap=144), sides), [])

    def test_smoke_limits(self):
        a = perf.parser().parse_args(['smoke'])
        self.assertEqual(perf.smoke_limits(a), (perf.FPS_MIN, perf.P99_MAX))
        a = perf.parser().parse_args(['smoke', '--fps-min', '0.9', '--p99-max', '1.2'])
        self.assertEqual(perf.smoke_limits(a), (0.9, 1.2))


    def test_the_checkpoint_rule_compares_capped_nvidia_runs_of_one_skip_mode(self):
        low = {'data': summary(fps=143, p99=7.5, cap=144)}
        high = {'data': summary(fps=141, p99=13.9, cap=144), 'skip': HIGH_MODE}
        commits = {'c0': low, 'c1': low, 'c2': high, 'c3': high, 'c4': high}
        code, out = self.bisect('--metric', 'p99', '--rounds', '2', case='rtx/forest/r16/144',
                                **commits)
        self.assertEqual(code, 2, out)
        self.assertIn('p99: 7.50 -> 13.90 (not judged: high skip mode 0/2 -> 2/2)', out)
        self.assertIn('nothing to look for', out)
        self.said.clear()
        code, out = self.bisect('--metric', 'p99', '--rounds', '2', case='780m/forest/r16/144',
                                **commits)
        self.assertEqual(code, 1, out)
        self.assertIn('first bad commit: c2', out)


class RecipesTest(unittest.TestCase):
    def test_justfile_has_the_recipes(self):
        path = os.path.join(perf.TREE, 'justfile')
        with open(path) as f:
            text = f.read()
        for recipe in ('perf-smoke', 'perf-check', 'perf-bisect'):
            self.assertTrue(f'\n{recipe} ' in text, f'no {recipe} recipe')
        self.assertTrue('tools/perf/perf_test.py' in text, 'test-tools runs these tests')

    def test_docs_have_no_long_dashes(self):
        for name in ('machine.py', 'runs.py', 'perf.py'):
            with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), name)) as f:
                self.assertNotIn('—', f.read(), name)


if __name__ == '__main__':
    unittest.main()
