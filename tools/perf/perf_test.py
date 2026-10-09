#!/usr/bin/env python3
"""Tests of tools/perf: python3 tools/perf/perf_test.py (or `just test-tools`).

No GPU and no real stress run: `measure` runs a fake stress binary (a Python script that writes
a summary and a CSV) so the watching, the redo of throttled runs and the metrics are covered."""
import contextlib
import io
import json
import math
import os
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import machine  # noqa: E402
import perf  # noqa: E402
import runs  # noqa: E402
from runs import Case  # noqa: E402


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
    m = {'fps': fps, 'p50': 0.5, 'p99': p99, 'slow': slow, 'skipped': 0, 'catch_up': 20.0,
         'pstates': None, 'quads': 100, 'draws': 7, 'gpu_chunks': 50}
    m.update(more)
    return m


def run(m=None, error=None, noisy=None):
    return runs.Run(Case(), 'x', metrics=m, error=error, noisy=noisy)


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


class CaseTest(unittest.TestCase):
    def test_parse(self):
        self.assertEqual(runs.parse_case('surface'), Case('rtx', 'surface', 16, 0))
        self.assertEqual(runs.parse_case('144/780m/forest'), Case('780m', 'forest', 16, 144))
        self.assertEqual(runs.parse_case('rtx/cave-path/r32/cap144'),
                         Case('rtx', 'cave-path', 32, 144))
        self.assertEqual(runs.parse_case('dive/uncapped', Case(cap=144)).cap, 0)
        self.assertEqual(runs.parse_case('dive', Case(gpu='780m', radius=8)),
                         Case('780m', 'dive', 8, 0))
        for bad in ('walk', 'rtx/r0', 'gtx/surface', 'r99'):
            with self.assertRaises(ValueError):
                runs.parse_case(bad)

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
        big = Case('rtx', 'surface', 32, 0).args(20)
        self.assertEqual(big[big.index('--arena-quads') + 1], '50331648')
        self.assertEqual(big[big.index('--timeout') + 1], '260')
        self.assertNotIn('--arena-quads', Case('rtx', 'forest', 32, 0).args(20))

    def test_supports_reads_the_help(self):
        with tempfile.TemporaryDirectory() as d:
            old = fake_binary(d, 'old', 'usage: stress [--scenario static|fly|dive|spin]\n'
                                        '  --caves MODE  cave mode\n')
            new = fake_binary(d, 'new', 'usage: stress [--scenario static|fly|dive|spin|path]\n'
                                        '  --cave              start in a cave\n')
            self.assertTrue(runs.supports(old, Case(scenario='surface')))
            self.assertFalse(runs.supports(old, Case(scenario='cave-static')))
            self.assertFalse(runs.supports(old, Case(scenario='cave-path')))
            self.assertTrue(runs.supports(new, Case(scenario='cave-static')))
            self.assertTrue(runs.supports(new, Case(scenario='cave-path')))


def fake_binary(folder, name, help_text):
    path = os.path.join(folder, name)
    with open(path, 'w') as f:
        f.write(f'#!/bin/sh\ncat <<"EOF"\n{help_text}EOF\n')
    os.chmod(path, os.stat(path).st_mode | stat.S_IXUSR)
    return path


class MetricsTest(unittest.TestCase):
    def test_skipped_refreshes(self):
        self.assertEqual(runs.skipped_refreshes(every_refresh()), 0)
        self.assertEqual(runs.skipped_refreshes(every_refresh(skip={10, 50, 51}), 144.0), 3)
        # Without the refresh rate, the median gap is the period.
        self.assertEqual(runs.skipped_refreshes(every_refresh(skip={10})), 1)
        self.assertIsNone(runs.skipped_refreshes([5.0]))

    def test_throttled(self):
        self.assertIsNone(runs.throttled(summary(), every_refresh()))
        self.assertIn('under 100 fps', runs.throttled(summary(timeline_fps=[140, 30, 140]), []))
        # Uncapped, the loop keeps its fps but the compositor shows 30 frames a second.
        shown = every_refresh(1) + [1000 + i * 33.3 for i in range(30)] + \
            [2000 + t for t in every_refresh(1)]
        self.assertIn('frames shown (1)', runs.throttled(summary(), shown))

    def test_metrics(self):
        m = runs.metrics(summary(fps=4500, slow=2, slow75=1), every_refresh(), {'P0': 1.0})
        self.assertEqual((m['fps'], m['slow'], m['skipped'], m['catch_up']), (4500, 2, 0, 20.0))
        self.assertEqual(m['pstates'], {'P0': 1.0})
        self.assertEqual(m['quads'], 100)
        capped = runs.metrics(summary(slow=1500, slow75=12, cap=144), [])
        self.assertEqual(capped['slow'], 12, 'capped runs count the frames over 7.5 ms')
        self.assertIsNone(capped['skipped'])

    def test_nvidia(self):
        self.assertIsNone(runs.parse_nv('timestamp, pstate', 1.0))
        s = runs.parse_nv('2026/10/08 12:00:00.000, P0, 2400, 8000, 25.10, 47', 5.0)
        self.assertEqual((s['pstate'], s['gr_mhz'], s['unix']), ('P0', '2400', 5.0))
        samples = [{'unix': t, 'pstate': p} for t, p in
                   [(1, 'P8'), (2, 'P0'), (3, 'P0'), (4, 'P2'), (5, 'P0'), (9, 'P8')]]
        self.assertEqual(runs.pstate_share(samples, 2, 5), {'P0': 0.75, 'P2': 0.25})
        self.assertEqual(runs.pstate_share([], 0, 1), {})


FAKE_STRESS = r'''#!/usr/bin/env python3
"""Fake stress: writes the files a run writes. The first run of COUNT is throttled."""
import json, os, sys
args = sys.argv[1:]
out = {args[i]: args[i + 1] for i in range(len(args) - 1) if args[i].startswith('--')}
count = os.environ['FAKE_COUNT']
n = int(open(count).read()) if os.path.exists(count) else 0
open(count, 'w').write(str(n + 1))
if os.environ.get('FAKE_FAIL'):
    print('stress: no usable GPU', file=sys.stderr)
    sys.exit(2)
data = json.loads(os.environ['FAKE_SUMMARY'])
if n == 0 and os.environ.get('FAKE_THROTTLE_FIRST'):
    data['timeline'][1]['fps'] = 30.0
print('stress: measuring fly for 3 s', flush=True)
with open(out['--summary'], 'w') as f:
    f.write('# stress\n\n## Data\n\n```json\n' + json.dumps(data) + '\n```\n')
with open(out['--csv'], 'w') as f:
    f.write('frame,t_ms,presented\n')
    for i in range(3 * 144):
        f.write(f'{i},{i * 1000 / 144:.3f},1\n')
'''


class MeasureTest(unittest.TestCase):
    def setUp(self):
        d = tempfile.TemporaryDirectory()
        self.addCleanup(d.cleanup)
        self.dir = d.name
        self.binary = os.path.join(self.dir, 'stress')
        with open(self.binary, 'w') as f:
            f.write(FAKE_STRESS)
        os.chmod(self.binary, 0o755)
        env = {'XDG_RUNTIME_DIR': self.dir, 'FAKE_COUNT': os.path.join(self.dir, 'count'),
               'FAKE_SUMMARY': json.dumps(summary(fps=900.0))}
        for patch in (mock.patch.dict(os.environ, env),
                      mock.patch.object(machine, 'wait_quiet', return_value=True),
                      mock.patch.object(machine, 'heavy_processes', return_value=[]),
                      mock.patch.object(machine, 'MAX_OTHER_CPU', 101.0)):
            patch.start()
            self.addCleanup(patch.stop)
        self.said = []

    def measure(self, **kw):
        prefix = os.path.join(self.dir, 'runs', 'case-1')
        os.makedirs(os.path.dirname(prefix), exist_ok=True)
        return runs.measure(self.binary, Case(), prefix, 'label', 3, say=self.said.append, **kw)

    def test_a_good_run(self):
        r = self.measure()
        self.assertIsNone(r.error)
        self.assertIsNone(r.noisy)
        self.assertEqual((r.metrics['fps'], r.metrics['skipped']), (900.0, 0))
        prefix = os.path.join(self.dir, 'runs', 'case-1')
        for ext in ('.md', '.csv', '.log', '.watch.json'):
            self.assertTrue(os.path.exists(prefix + ext), ext)
        with open(prefix + '.log') as f:
            self.assertIn('--label label', f.read())

    def test_a_throttled_run_is_redone(self):
        with mock.patch.dict(os.environ, {'FAKE_THROTTLE_FIRST': '1'}):
            r = self.measure()
        self.assertIsNone(r.noisy)
        self.assertEqual(len(r.discarded), 1)
        self.assertIn('under 100 fps', self.said[0])
        moved = os.listdir(os.path.join(self.dir, 'runs', 'discarded'))
        self.assertIn('case-1.a1.md', moved)

    def test_noisy_runs_keep_the_last_one(self):
        with mock.patch.object(machine, 'heavy_processes', return_value=['rustc']):
            r = self.measure(attempts=2)
        self.assertEqual(r.noisy, 'overlapped rustc')
        self.assertEqual(len(r.discarded), 2)
        self.assertIsNotNone(r.metrics)

    def test_a_failed_run(self):
        with mock.patch.dict(os.environ, {'FAKE_FAIL': '1'}):
            r = self.measure()
        self.assertEqual(r.rc, 2)
        self.assertIn('exit code 2', r.error)
        self.assertIn('no usable GPU', r.detail)


class SmokeTest(unittest.TestCase):
    def test_gross(self):
        base = metrics(fps=1000, p99=1.0, slow=0)
        self.assertEqual(perf.gross(base, metrics(fps=900, p99=1.4, slow=0)), [])
        self.assertIn('under 85%', perf.gross(base, metrics(fps=800))[0])
        self.assertIn('over 1.5x', perf.gross(base, metrics(p99=1.6))[0])
        self.assertIn('the base had none', perf.gross(base, metrics(slow=1))[0])
        self.assertEqual(perf.gross(metrics(slow=3), metrics(slow=9)), [])
        self.assertEqual(perf.gross(base, metrics(fps=960), fps_min=0.95), [])
        self.assertEqual(len(perf.gross(base, metrics(fps=940), fps_min=0.95)), 1)

    def result(self, *problems, new_error=None, base_error=None):
        r = perf.SmokeResult(Case())
        for p in problems:
            r.pairs.append((run(metrics()), run(metrics(fps=500 if p else 1000))))
            r.problems.append(p)
        if new_error or base_error:
            r.pairs.append((run(error=base_error), run(error=new_error)))
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
        self.assertEqual(self.result(base_error='exit code 2').status, 'broken')
        skipped = perf.SmokeResult(Case(), skipped='no cave')
        self.assertEqual((skipped.status, skipped.verdict()), ('skipped', 'skipped: no cave'))
        noisy = perf.SmokeResult(Case(), pairs=[(run(metrics(), noisy='overlapped rustc'),
                                                 run(metrics()))], problems=[[]])
        self.assertIn('noisy: overlapped rustc', noisy.verdict())

    def test_exit_code(self):
        ok, worse = self.result([]), self.result(['a'], ['b'])
        self.assertEqual(perf.exit_code([ok]), 0)
        self.assertEqual(perf.exit_code([ok, worse]), 1)
        self.assertEqual(perf.exit_code([ok, self.result(base_error='x')]), 2)
        self.assertEqual(perf.exit_code([worse, self.result(base_error='x')]), 1)
        self.assertEqual(perf.exit_code([perf.SmokeResult(Case(), skipped='x')]), 0)

    def test_table(self):
        table = perf.smoke_table([self.result([]), perf.SmokeResult(Case(scenario='cave-path'),
                                                                     skipped='no cave')])
        lines = table.splitlines()
        self.assertEqual(lines[0], '| case | fps | p99 ms | slow frames | verdict |')
        self.assertEqual(lines[2], '| rtx/surface/r16/uncapped | 1000 -> 1000 (+0.0 %) | '
                                   '1 -> 1 | 0 -> 0 | ok |')
        self.assertEqual(lines[3], '| rtx/cave-path/r16/uncapped | - | - | - | skipped: no cave |')

    def test_the_tree_never_shares_the_base_target_dir(self):
        with tempfile.TemporaryDirectory() as d, \
                mock.patch.dict(os.environ, {'PERF_TARGET_DIR': d, 'CARGO_TARGET_DIR': d}), \
                mock.patch.object(perf, 'cargo_build') as build:
            with self.assertRaises(perf.PerfError) as e:
                perf.tree_binary()
        self.assertIn('PERF_TARGET_DIR', str(e.exception))
        build.assert_not_called()

    def test_usage_errors(self):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            self.assertEqual(perf.main(['smoke', 'walk']), 64)
            self.assertEqual(perf.main(['nothing']), 64)
            self.assertEqual(perf.main(['cases']), 0)
        self.assertIn("'walk' is not a GPU", err.getvalue())
        self.assertIn('rtx/surface/r32/144', out.getvalue())


class CheckTest(unittest.TestCase):
    def test_next_number(self):
        self.assertEqual(perf.next_number([]), 1)
        self.assertEqual(perf.next_number(['perf-control-2', 'perf-control-10', 'perf-control-x']),
                         11)
        self.assertEqual(perf.next_number(['perf-control-2'], {'number': 4}), 5)

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
        firsts = {(r, c) : s for r, c, s in reversed(plan)}
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

    def test_pstates_text(self):
        self.assertEqual(perf.pstates_text([None, {}]), '-')
        self.assertEqual(perf.pstates_text([{'P0': 1.0}, {'P0': 0.8, 'P2': 0.2}]),
                         'P0 90 %, P2 10 %')
        self.assertEqual(perf.pstates_text([{'P8': 0.5, 'P10': 0.5}]), 'P8 50 %, P10 50 %')

    def info(self, **more):
        info = {'number': 3, 'date': '2026-10-20', 'tag': 'perf-control-3', 'new': 'a' * 40,
                'base': 'bbbbbbbb', 'base_tag': 'perf-control-2', 'rounds': 3, 'duration': 20,
                'skipped': [], 'discarded': 2, 'noisy': 0,
                'folder': 'bench-results/control/3-2026-10-20'}
        info.update(more)
        return info

    def test_report(self):
        rtx = Case('rtx', 'surface', 16, 0)
        cave = Case('780m', 'cave-path', 16, 144)
        same = [metrics(fps=f, pstates={'P0': 1.0}) for f in (1000, 1010, 990)]
        results = {
            rtx: {'base': same, 'new': [metrics(fps=f, pstates={'P0': 0.5, 'P2': 0.5})
                                        for f in (700, 710, 690)], 'errors': []},
            cave: {'base': same, 'new': same[:2], 'errors': ['new, ronda 3: exit code 4']},
        }
        text = perf.render_report(self.info(skipped=['rtx/cave-path/r32/144 solo ahora']),
                                  results)
        self.assertTrue(text.startswith('# Ronda de control 3 (2026-10-20)\n'))
        self.assertIn('`bbbbbbbb` (`perf-control-2`)', text)
        self.assertIn('**Resultado:** empeoró en rtx/surface/r16/uncapped. Buscar el merge con '
                      '`just perf-bisect rtx/surface/r16/uncapped bbbbbbbb aaaaaaaa`.', text)
        self.assertIn('| superficie, vuelo a 64, sin tope | 1000 -> 700 **peor** | 0.50 -> 0.50 |',
                      text)
        self.assertIn('| P0 100 % -> P0 50 %, P2 50 % |', text)
        self.assertIn('## Radeon 780M', text)
        self.assertNotIn('Estado NVIDIA |\n|---|---:|---:|---:|---:|---:|---:|\n| cueva', text)
        self.assertIn('**Fallaron:** 780m/cave-path/r16/144', text)
        self.assertIn('- Sin datos: rtx/cave-path/r32/144 solo ahora.', text)
        self.assertIn('Corridas descartadas y repetidas: 2', text)
        self.assertNotIn('## Data', text, 'bench_report_test parses docs with a Data section')
        self.assertNotIn('\u2014', text)

    def test_first_report(self):
        results = {Case(): {'base': [], 'new': [metrics()], 'errors': []}}
        text = perf.render_report(self.info(number=1, base=None, base_tag=None), results)
        self.assertIn('Primera ronda', text)
        self.assertIn('**Resultado:** sin empeoramientos claros.', text)
        self.assertIn('| superficie, vuelo a 64, sin tope | 1000 | 0.50 | 1 | 0 | 0 | 20 | - |',
                      text)

    def test_dry_run_prints_the_plan(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            code = perf.main(['check', '--dry-run', '--no-tag'])
        self.assertEqual(code, 0, out.getvalue())
        self.assertIn(f'{len(perf.MATRIX)} cases x 3 rounds', out.getvalue())
        self.assertIn('dry run, nothing measured', out.getvalue())

    def test_dry_run_against_the_last_checkpoint(self):
        head = perf.commit('HEAD')
        last = {'number': 41, 'tag': 'perf-control-41', 'sha': head}
        out = io.StringIO()
        with mock.patch.object(perf, 'read_last', return_value=last), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
            code = perf.main(['check', '--dry-run'])
        self.assertEqual(code, 0, out.getvalue())
        text = out.getvalue()
        self.assertIn('perf-check: round 42 (perf-control-42)', text)
        self.assertIn('x 3 rounds x 2 sides of 20 s', text)
        self.assertIn(f'base: {head} (the last checkpoint, perf-control-41;', text)


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
                self.assertNotIn('\u2014', f.read(), name)


if __name__ == '__main__':
    unittest.main()
