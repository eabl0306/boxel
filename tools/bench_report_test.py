#!/usr/bin/env python3
"""Tests of tools/bench_report.py: python3 tools/bench_report_test.py (or `just test-tools`).

Also parses every committed stress summary under docs/rendimiento/ (the JSON data of each run),
so a change of the summary layout in examples/stress.rs that the tool does not follow fails
here."""
import contextlib
import copy
import io
import json
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import bench_report as br  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

SAMPLE = {
    'format': 1,
    'run': {
        'scenario': 'fly', 'label': 'abc', 'radius': 16, 'speed': 32, 'seed': '1',
        'backend': 'Vulkan', 'tier': 'High', 'adapter': 'GPU', 'build': 'release',
        'duration_s': 20, 'window': '1280x720',
    },
    'frames': {
        'count': 1000, 'avg_fps': 500.0, 'p50_ms': 2.0, 'p95_ms': 2.5, 'p99_ms': 3.0,
        'p999_ms': 5.0, 'max_ms': 8.0, 'low_1_fps': 200.0, 'over_144hz': 2, 'over_60hz': 0,
        'crossing': {'frames': 10, 'p50_ms': 3.0, 'p99_ms': 6.0, 'max_ms': 6.5},
        'other': {'frames': 990, 'p50_ms': 2.0, 'p99_ms': 3.0, 'max_ms': 8.0},
    },
    'world': {
        'generated_per_s': 100.0, 'meshes_per_s': 50.0,
        'queued': {'avg': 1.0, 'p95': 3, 'max': 10, 'first': 0, 'last': 0},
        'backlog_trend_per_s': 0.0, 'catch_up_s': 0.5,
        'border_catch_up': {'crossings': 10, 'finished': 10, 'avg_ms': 120.0, 'max_ms': 250.0},
    },
}


def markdown(data):
    return ('# boxel stress: fly r16 32 vox/s\n\n## Frames\n\n| a |\n\n## Data\n\nText.\n\n'
            '```json\n' + json.dumps(data, indent=2) + '\n```\n')


def run_main(*args):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = br.main(list(args))
    return code, out.getvalue(), err.getvalue()


class ParseTest(unittest.TestCase):
    def test_markdown_and_json(self):
        self.assertEqual(br.parse_summary(markdown(SAMPLE)), SAMPLE)
        self.assertEqual(br.parse_summary(json.dumps(SAMPLE)), SAMPLE)

    def test_rejects_other_files(self):
        with self.assertRaises(br.ReportError):
            br.parse_summary('# just a doc\n\n```json\n{"format": 1}\n```\n')
        other = dict(SAMPLE, format=2)
        with self.assertRaises(br.ReportError):
            br.parse_summary(markdown(other))
        with self.assertRaises(ValueError):
            br.parse_summary('## Data\n\n```json\n{not json}\n```\n')


class FormatTest(unittest.TestCase):
    def test_fmt(self):
        self.assertEqual(br.fmt(None), '-')
        self.assertEqual(br.fmt(True), 'yes')
        self.assertEqual(br.fmt(32.0), '32')
        self.assertEqual(br.fmt(2.345), '2.35')
        self.assertEqual(br.fmt(1234.5), '1234')
        self.assertEqual(br.fmt('x'), 'x')

    def test_change(self):
        self.assertEqual(br.change(2.0, 1.0, -1), ('-50.0 %', 'better'))
        self.assertEqual(br.change(2.0, 3.0, -1), ('+50.0 %', 'worse'))
        self.assertEqual(br.change(100.0, 150.0, 1), ('+50.0 %', 'better'))
        self.assertEqual(br.change(100.0, 100.5, 1), ('+0.5 %', ''))
        self.assertEqual(br.change(100.0, 150.0, 0), ('+50.0 %', ''))
        self.assertEqual(br.change(3, 3, -1), ('0 %', ''))
        self.assertEqual(br.change(4, 0, -1), ('-100.0 %', 'better'))
        self.assertEqual(br.change(-2.0, -1.0, -1), ('+50.0 %', 'worse'))
        self.assertEqual(br.change(None, 3, -1), ('-', ''))

    def test_change_from_zero(self):
        # No percent from 0, but the direction still decides.
        self.assertEqual(br.change(0, 3, -1), ('n/a', 'worse'))
        self.assertEqual(br.change(0, -0.5, -1), ('n/a', 'better'))
        self.assertEqual(br.change(0.0, 120.0, 1), ('n/a', 'better'))
        self.assertEqual(br.change(0, -1, 1), ('n/a', 'worse'))
        self.assertEqual(br.change(0, 3, 0), ('n/a', ''))
        self.assertEqual(br.change(0, 0.0, -1), ('0 %', ''))

    def test_title_and_get(self):
        self.assertEqual(br.title(SAMPLE), 'fly r16 32 vox/s')
        static = copy.deepcopy(SAMPLE)
        static['run']['scenario'] = 'static'
        self.assertEqual(br.title(static), 'static r16')
        static['run']['light'] = 'on'
        self.assertEqual(br.title(static), 'static r16', 'light on keeps the title')
        static['run']['light'] = 'off'
        self.assertEqual(br.title(static), 'static r16 light off')
        static['run']['light'] = 'none'
        self.assertEqual(br.title(static), 'static r16')
        static['run']['fps_limit'] = 60
        self.assertEqual(br.title(static), 'static r16 limit 60 fps')
        static['run']['vsync'] = 'off'
        self.assertEqual(br.title(static), 'static r16 limit 60 fps', 'vsync off keeps the title')
        static['run']['vsync'] = 'on'
        self.assertEqual(br.title(static), 'static r16 limit 60 fps vsync')
        static['run']['vsync'] = 'off'
        static['run']['bodies'] = '10000 item, 0 projectiles'
        self.assertEqual(br.title(static), 'static r16 limit 60 fps bodies 10000 item, 0 projectiles')
        static['run']['edits'] = True
        self.assertEqual(br.title(static),
                         'static r16 limit 60 fps bodies 10000 item, 0 projectiles edits')
        self.assertEqual(br.get(SAMPLE, ('frames', 'crossing', 'max_ms')), 6.5)
        self.assertIsNone(br.get(SAMPLE, ('frames', 'missing', 'max_ms')))


class OutputTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)

    def write(self, name, data, sub=''):
        folder = os.path.join(self.dir.name, sub)
        os.makedirs(folder, exist_ok=True)
        path = os.path.join(folder, name)
        with open(path, 'w', encoding='utf-8') as f:
            f.write(markdown(data) if name.endswith('.md') else json.dumps(data))
        return path

    def test_table_has_one_row_per_run(self):
        a = self.write('a.md', SAMPLE)
        b = self.write('b.json', SAMPLE)
        code, out, _ = run_main(a, b)
        self.assertEqual(code, 0)
        rows = [line for line in out.splitlines() if line.startswith('| fly r16')]
        self.assertEqual(len(rows), 2)
        self.assertIn('| 500 | 2 | 3 | 5 | 8 | 200 | 2 | 0 | 6.50 | 100 | 10 | 250 | - | - |',
                      rows[0])

    def test_directories_skip_other_files(self):
        self.write('run.md', SAMPLE, 'runs')
        with open(os.path.join(self.dir.name, 'runs', 'README.md'), 'w') as f:
            f.write('# notes\n')
        code, out, _ = run_main(os.path.join(self.dir.name, 'runs'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('| fly r16'), 1)
        empty = os.path.join(self.dir.name, 'empty')
        os.makedirs(empty)
        self.assertEqual(run_main(empty)[0], 2)

    def test_compare_two_files(self):
        faster = copy.deepcopy(SAMPLE)
        faster['frames']['p99_ms'] = 1.5
        faster['run']['radius'] = 32
        code, out, _ = run_main('--compare', self.write('a.md', SAMPLE), self.write('b.md', faster))
        self.assertEqual(code, 0)
        self.assertIn('| p99 ms | 3 | 1.50 | -50.0 % | better |', out)
        self.assertIn('Note: the settings differ (radius 16 -> 32)', out)

    def test_compare_flags_new_spikes(self):
        clean = copy.deepcopy(SAMPLE)
        clean['frames']['over_144hz'] = 0
        spiky = copy.deepcopy(clean)
        spiky['frames']['over_144hz'] = 5
        spiky['frames']['over_60hz'] = 1
        code, out, _ = run_main('--compare', self.write('a.md', clean), self.write('b.md', spiky))
        self.assertEqual(code, 0)
        self.assertIn('| frames > 6.94 ms | 0 | 5 | n/a | worse |', out)
        self.assertIn('| frames > 16.7 ms | 0 | 1 | n/a | worse |', out)
        code, out, _ = run_main('--compare', self.write('b.md', spiky), self.write('a.md', clean))
        self.assertIn('| frames > 6.94 ms | 5 | 0 | -100.0 % | better |', out)

    def test_compare_directories_pairs_runs(self):
        spin = copy.deepcopy(SAMPLE)
        spin['run'].update(scenario='spin', speed=0)
        self.write('1-fly.md', SAMPLE, 'base')
        self.write('2-spin.md', spin, 'base')
        self.write('3-fly.md', SAMPLE, 'new')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'base'),
                                os.path.join(self.dir.name, 'new'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('### '), 1, 'only fly has a pair')
        self.assertNotIn('Note:', out)
        self.write('4-spin.md', spin, 'other')
        code, _, err = run_main('--compare', os.path.join(self.dir.name, 'new'),
                                os.path.join(self.dir.name, 'other'))
        self.assertEqual(code, 2)
        self.assertIn('no runs', err)

    def test_compare_directories_keeps_worlds_apart(self):
        # Old runs have no world: the forest (trees on) or the terrain (trees off).
        caves = copy.deepcopy(SAMPLE)
        caves['run']['world'] = 'overworld/mc'
        bare = copy.deepcopy(SAMPLE)
        bare['run']['trees'] = False
        self.write('1-fly.md', SAMPLE, 'base')
        self.write('2-caves.md', caves, 'base')
        self.write('3-bare.md', bare, 'base')
        named = copy.deepcopy(SAMPLE)
        named['run']['world'] = 'forest'
        self.write('4-fly.md', named, 'new')
        self.write('5-caves.md', caves, 'new')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'base'),
                                os.path.join(self.dir.name, 'new'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('### '), 2, 'forest with forest, caves with caves')
        self.assertIn('1-fly.md -> 4-fly.md', out)
        self.assertIn('2-caves.md -> 5-caves.md', out)
        self.assertIn('fly r16 32 vox/s overworld/mc', out)

    def test_compare_directories_keeps_gpus_apart(self):
        # Same backend and tier on two adapters: each run pairs with its own GPU.
        amd = copy.deepcopy(SAMPLE)
        amd['run']['adapter'] = 'AMD'
        amd['frames']['p99_ms'] = 9.0
        self.write('1-gpu.md', SAMPLE, 'base')
        self.write('2-amd.md', amd, 'base')
        self.write('3-amd.json', amd, 'new')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'base'),
                                os.path.join(self.dir.name, 'new'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('### '), 1)
        self.assertIn('2-amd.md -> 3-amd.json', out)
        self.assertIn('| p99 ms | 9 | 9 | 0 % |  |', out)
        self.assertNotIn('Note:', out)

    def test_compare_directories_keeps_fps_limits_apart(self):
        # Old runs have no fps_limit: they were all uncapped (0).
        named = copy.deepcopy(SAMPLE)
        named['run']['fps_limit'] = 0
        capped = copy.deepcopy(SAMPLE)
        capped['run']['fps_limit'] = 144
        capped['frames']['avg_fps'] = 144.0
        self.write('1-fly.md', SAMPLE, 'base')
        self.write('2-capped.md', capped, 'base')
        self.write('3-fly.md', named, 'new')
        self.write('4-capped.md', capped, 'new')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'base'),
                                os.path.join(self.dir.name, 'new'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('### '), 2, 'uncapped with uncapped, capped with capped')
        self.assertIn('1-fly.md -> 3-fly.md', out)
        self.assertIn('### fly r16 32 vox/s limit 144 fps: 2-capped.md -> 4-capped.md', out)
        self.assertNotIn('Note:', out)
        code, out, _ = run_main('--compare', self.write('a.md', SAMPLE), self.write('b.md', capped))
        self.assertEqual(code, 0)
        self.assertIn('Note: the settings differ (fps limit 0 -> 144)', out)

    def test_compare_directories_keeps_vsync_apart(self):
        # Runs from before --vsync count as vsync on only if they asked for --present vsync.
        old = copy.deepcopy(SAMPLE)
        old['run']['present'] = 'vsync'
        new = copy.deepcopy(SAMPLE)
        new['run']['vsync'] = 'on'
        off = copy.deepcopy(SAMPLE)
        off['run']['vsync'] = 'off'
        self.write('1-old.md', old, 'base')
        self.write('2-plain.md', SAMPLE, 'base')
        self.write('3-on.md', new, 'new')
        self.write('4-off.md', off, 'new')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'base'),
                                os.path.join(self.dir.name, 'new'))
        self.assertEqual(code, 0)
        self.assertIn('### fly r16 32 vox/s vsync: 1-old.md -> 3-on.md', out)
        self.assertIn('### fly r16 32 vox/s: 2-plain.md -> 4-off.md', out)

    def test_compare_adds_pacing_metrics_when_recorded(self):
        paced = copy.deepcopy(SAMPLE)
        paced['frames']['over_7_5ms'] = 3
        paced['pacing'] = {
            'presented_fps': 140.5, 'gate_waits': 10, 'gate_misses': 3,
            'presented_intervals': {'p99_ms': 13.9, 'stddev_ms': 0.8, 'gaps': 3},
            'callbacks': {'gaps': 3},
        }
        code, out, _ = run_main('--compare', self.write('a.md', SAMPLE), self.write('b.md', paced))
        self.assertEqual(code, 0)
        self.assertIn('| presented fps | - | 140 | - |  |', out)
        self.assertIn('| gate misses | - | 3 | - |  |', out)
        code, out, _ = run_main('--compare', self.write('c.md', SAMPLE), self.write('d.md', SAMPLE))
        self.assertNotIn('presented fps', out)

    def test_idle_work_is_a_column_and_a_note_not_a_key(self):
        # Runs from before --idle-work read as 'none'; on and off pair, with a note.
        on = copy.deepcopy(SAMPLE)
        on['run']['idle_work'] = 'on'
        on['world']['border_catch_up'].update(frames_avg=2.5, work_avg_ms=9.0, work_max_ms=15.5)
        on['idle'] = {'tail_mean_ms': 1.2, 'tail_p99_ms': 3.4, 'steps_mean': 4.0,
                      'frames': 900, 'overruns': 0, 'overrun_max_ms': 0}
        off = copy.deepcopy(on)
        off['run']['idle_work'] = 'off'
        off['world']['border_catch_up']['work_max_ms'] = 30.0
        code, out, _ = run_main(self.write('old.md', SAMPLE), self.write('on.md', on))
        self.assertEqual(code, 0)
        rows = [line for line in out.splitlines() if line.startswith('| fly r16')]
        self.assertTrue(rows[0].endswith('| none | - |'), rows[0])
        self.assertTrue(rows[1].endswith('| on | 15.50 |'), rows[1])
        self.write('1-fly.md', off, 'off')
        self.write('1-fly.md', on, 'on')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'off'),
                                os.path.join(self.dir.name, 'on'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('### '), 1, 'off and on pair')
        self.assertIn('Note: the settings differ (idle work off -> on)', out)
        self.assertIn('| border work lag max ms | 30 | 15.50 | -48.3 % | better |', out)
        self.assertIn('| idle steps past their slot | 0 | 0 | 0 % |  |', out)
        code, out, _ = run_main('--compare', self.write('a.md', SAMPLE), self.write('b.md', SAMPLE))
        self.assertNotIn('idle steps', out)
        self.assertNotIn('Note:', out)

    def test_compare_directories_keeps_bodies_apart(self):
        # Old runs have no bodies field: they had no physics bodies.
        crowded = copy.deepcopy(SAMPLE)
        crowded['run']['bodies'] = '1000 mob, 0 projectiles'
        self.write('1-fly.md', SAMPLE, 'base')
        self.write('2-crowded.md', crowded, 'base')
        self.write('3-fly.md', SAMPLE, 'new')
        self.write('4-crowded.md', crowded, 'new')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'base'),
                                os.path.join(self.dir.name, 'new'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('### '), 2, 'runs without bodies with each other')
        self.assertIn('2-crowded.md -> 4-crowded.md', out)
        code, out, _ = run_main('--compare', self.write('a.md', SAMPLE), self.write('b.md', crowded))
        self.assertEqual(code, 0)
        self.assertIn("Note: the settings differ (bodies  -> 1000 mob, 0 projectiles)", out)

    def test_compare_directories_keeps_edits_apart(self):
        # Old runs have no edits and no clock: no scripted edits, no day and night.
        edited = copy.deepcopy(SAMPLE)
        edited['run']['scenario'] = 'static'
        edited['run']['time'] = '12:00'
        edited['world']['edits'] = {
            'steps': 18, 'skipped': 0, 'timed': 16, 'frames_p50': 4, 'frames_max': 9,
            'ms_p50': 12.5, 'ms_max': 40.0, 'batches_held': 16, 'batches_split': 1,
            'light_fanout': 40,
        }
        still = copy.deepcopy(edited)
        still['world']['edits'] = None
        self.write('1-still.md', still, 'base')
        self.write('2-edited.md', edited, 'base')
        slower = copy.deepcopy(edited)
        slower['world']['edits']['frames_max'] = 12
        slower['run']['time'] = '21:30'
        self.write('3-still.md', still, 'new')
        self.write('4-edited.md', slower, 'new')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'base'),
                                os.path.join(self.dir.name, 'new'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('### '), 2, 'runs with edits with each other')
        self.assertIn('### static r16 edits: 2-edited.md -> 4-edited.md', out)
        self.assertIn('| edit frames max | 9 | 12 | +33.3 % | worse |', out)
        self.assertIn('Note: the settings differ (clock 12:00 -> 21:30)', out)
        self.assertEqual(out.count('| edit frames p50 |'), 1, 'only the runs with edits')
        code, out, _ = run_main('--compare', self.write('a.md', SAMPLE), self.write('b.md', edited))
        self.assertEqual(code, 0)
        self.assertIn('edits no -> yes', out)
        self.assertIn('clock  -> 12:00', out)
        self.assertIn('| edit ms p50 | - | 12.50 | - |  |', out)

    def test_compare_directories_keeps_light_apart(self):
        # No light field: 'on' with a clock (after the light render), 'none' without (before it).
        lit = copy.deepcopy(SAMPLE)
        lit['run']['time'] = '12:00'
        on = copy.deepcopy(lit)
        on['run']['light'] = 'on'
        off = copy.deepcopy(lit)
        off['run']['light'] = 'off'
        off['frames']['p99_ms'] = 2.5
        self.write('1-old.md', SAMPLE, 'base')
        self.write('2-lit.md', lit, 'base')
        self.write('3-off.md', off, 'base')
        self.write('4-on.md', on, 'new')
        self.write('5-off.json', off, 'new')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'base'),
                                os.path.join(self.dir.name, 'new'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('### '), 2, 'on with on, off with off, none alone')
        self.assertIn('2-lit.md -> 4-on.md', out)
        self.assertIn('### fly r16 32 vox/s light off: 3-off.md -> 5-off.json', out)
        self.assertNotIn('Note:', out)
        self.assertEqual(br.load(os.path.join(self.dir.name, 'base', '1-old.md'))['run']['light'],
                         'none')
        code, out, _ = run_main('--compare', self.write('a.md', SAMPLE), self.write('b.md', off))
        self.assertEqual(code, 0)
        self.assertIn('light none -> off', out)
        self.assertIn('| p99 ms | 3 | 2.50 | -16.7 % | better |', out)

    def test_compare_directories_keeps_packs_apart(self):
        # No pack field: the procedural textures ('none').
        pack = copy.deepcopy(SAMPLE)
        pack['run']['pack'] = 'BlockPixel'
        pack['run']['texture_load_ms'] = 41.5
        self.write('1-old.md', SAMPLE, 'base')
        self.write('2-pack.md', pack, 'base')
        self.write('3-none.md', SAMPLE, 'new')
        self.write('4-pack.json', pack, 'new')
        code, out, _ = run_main('--compare', os.path.join(self.dir.name, 'base'),
                                os.path.join(self.dir.name, 'new'))
        self.assertEqual(code, 0)
        self.assertEqual(out.count('### '), 2, 'procedural with procedural, pack with pack')
        self.assertIn('### fly r16 32 vox/s pack BlockPixel: 2-pack.md -> 4-pack.json', out)
        self.assertIn('1-old.md -> 3-none.md', out)
        self.assertEqual(br.load(os.path.join(self.dir.name, 'base', '1-old.md'))['run']['pack'],
                         'none')
        code, out, _ = run_main('--compare', self.write('a.md', SAMPLE), self.write('b.md', pack))
        self.assertEqual(code, 0)
        self.assertIn('pack none -> BlockPixel', out)

    def test_usage_errors(self):
        self.assertEqual(run_main()[0], 1)
        self.assertEqual(run_main('--help')[0], 0)
        self.assertEqual(run_main('--compare', 'only-one')[0], 1)
        self.assertEqual(run_main(os.path.join(self.dir.name, 'missing.md'))[0], 1)
        bad = os.path.join(self.dir.name, 'bad.md')
        with open(bad, 'w') as f:
            f.write('# not a summary\n')
        code, _, err = run_main(bad)
        self.assertEqual(code, 1)
        self.assertIn('no "## Data" section', err)


class CommittedSummariesTest(unittest.TestCase):
    def test_every_committed_summary_parses(self):
        folder = os.path.join(ROOT, 'docs', 'rendimiento')
        files = []
        for dirpath, _, names in os.walk(folder):
            for name in names:
                path = os.path.join(dirpath, name)
                with open(path, encoding='utf-8') as f:
                    text = f.read()
                if name.endswith('.json') or (name.endswith('.md') and br.DATA_HEADING in text):
                    files.append(path)
        self.assertGreaterEqual(len(files), 13, 'the October 2026 baseline')
        for path in files:
            run = br.load(path)
            self.assertIn(run['run']['light'], ('on', 'off', 'none'), path)
            br.table([run])
            br.compare(run, run)
            for _, keys, _ in br.METRICS + br.EDIT_METRICS:
                optional = {'catch_up_s', 'crossing', 'border_catch_up', 'edits'}
                if not optional.intersection(keys):
                    self.assertIsNotNone(br.get(run, keys), f'{path}: {keys}')


if __name__ == '__main__':
    unittest.main()
