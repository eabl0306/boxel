#!/usr/bin/env python3
"""Tests of tools/snapshot_set.py: python3 tools/snapshot_set_test.py (or `just test-tools`).

No GPU: the runs go to a fake snapshot binary (a Python script) that writes each shot as a file
derived from its arguments and keeps a counter in --world-dir, so the manifest, the check, the
save and load scenes and the exit codes are covered."""
import contextlib
import io
import os
import stat
import sys
import tempfile
import textwrap
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import snapshot_set as ss  # noqa: E402

# The scene list the B0 images (bench-results/split48/b0) were rendered from. A change here
# means the B0 binary must render the changed scenes again.
B0_FINGERPRINT = '92af33cb85b36ac5bc8bae789f42a1b6'

FAKE = textwrap.dedent('''\
    #!{python}
    """Fake snapshot: each --shot file holds the md5 of the arguments that are not paths, plus
    the counter in --world-dir (bumped after the shots, as the real one saves after them)."""
    import hashlib, os, sys
    args = sys.argv[1:]
    shots, world, keep = [], None, []
    it = iter(args)
    for a in it:
        if a == '--shot':
            clock, path = next(it).split('=', 1)
            shots.append(path)
            keep.append(clock)
        elif a == '--world-dir':
            world = next(it)
        elif a == '--pack':
            keep += [a, os.path.basename(next(it))]
        else:
            keep.append(a)
    if os.environ.get('FAKE_FAIL') and os.environ['FAKE_FAIL'] in ' '.join(args):
        print('snapshot: boom')
        sys.exit(2)
    saved = 0
    if world:
        os.makedirs(world, exist_ok=True)
        counter = os.path.join(world, 'counter')
        if os.path.exists(counter):
            saved = int(open(counter).read())
    salt = os.environ.get('FAKE_SALT', '')
    # FAKE_FLAKY=TEXT:N: the Nth render (from 1) of each shot whose path holds TEXT changes.
    flaky, _, nth = os.environ.get('FAKE_FLAKY', '').partition(':')
    for path in shots:
        body = ' '.join(keep) + f' saved={{saved}}'
        if salt and salt in path:
            body += ' changed'
        if flaky and flaky in path:
            name = os.path.basename(path).split('.')[0]
            count = os.path.join(os.environ['FAKE_STATE'], name)
            n = int(open(count).read()) + 1 if os.path.exists(count) else 1
            open(count, 'w').write(str(n))
            if n == int(nth):
                body += ' flaky'
        open(path, 'w').write(hashlib.md5(body.encode()).hexdigest() + '\\n')
    if any(a.startswith('boxel:') and 'drop' in a for a in args):
        print('edits: set=1 skipped=0 not_applied=1')
    print('shot: ok')
    if world:
        open(counter, 'w').write(str(saved + 1))
''')


class Fixture:
    def __init__(self):
        self.dir = tempfile.TemporaryDirectory()
        self.root = self.dir.name
        self.bin = os.path.join(self.root, 'fake-snapshot')
        with open(self.bin, 'w') as f:
            f.write(FAKE.format(python=sys.executable))
        os.chmod(self.bin, os.stat(self.bin).st_mode | stat.S_IEXEC)
        self.pack = os.path.join(self.root, 'Pack')
        os.makedirs(self.pack)
        open(os.path.join(self.pack, 'pack.mcmeta'), 'w').close()

    def run(self, *args, out='out'):
        argv = ['--out', os.path.join(self.root, out), '--bin', self.bin, *args]
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
            code = ss.main(argv)
        return code, buf.getvalue()

    def path(self, *parts):
        return os.path.join(self.root, *parts)

    def close(self):
        self.dir.cleanup()


class Scenes(unittest.TestCase):
    def test_the_scene_list_is_frozen(self):
        self.assertEqual(ss.fingerprint(), B0_FINGERPRINT)

    def test_every_group_of_the_plan(self):
        groups = {s.group for s in ss.SCENES}
        self.assertEqual(groups, {'s1', 's2', 's3', 's4', 's5', 's6'})
        names = [s.name for s in ss.SCENES]
        self.assertEqual(len(names), len(set(names)))
        forest = next(s for s in ss.SCENES if s.name == 's1-forest')
        self.assertEqual([c for _, c in forest.shots], ['12:00', '00:00'])
        outlines = [s for s in ss.SCENES if '--outline' in s.args]
        self.assertEqual([s.name for s in outlines], [
            's4-outline-fence', 's4-outline-stairs', 's4-outline-wall', 's4-outline-gate'])
        fluids = [s for s in ss.SCENES if s.group == 's5']
        self.assertEqual([s.args[s.args.index('--fluid-time') + 1] for s in fluids],
                         ['0', '0.5', '1.25'])
        self.assertTrue(all(s.args[s.args.index('--flow') + 1] == '400' for s in fluids))
        caves = [s for s in ss.SCENES if s.group == 's2']
        self.assertTrue(all(s.args[s.args.index('--caves') + 1] == 'mc' for s in caves))

    def test_the_showcase_places_every_demo_block(self):
        placed = {a.split('@')[0].split('[')[0] for a in ss.SHOWCASE if a.startswith('boxel:')}
        demo = {'oak_stairs', 'stone_stairs', 'oak_fence', 'oak_fence_gate', 'cobblestone_wall',
                'torch', 'wall_torch', 'stone_slab', 'glass', 'leaves', 'log', 'short_grass',
                'poppy', 'lamp_orange', 'lamp_red', 'lamp_blue', 'lamp_white', 'cobblestone',
                'obsidian'}
        self.assertEqual(placed, {f'boxel:{b}' for b in demo})
        for value in ('type=bottom', 'type=top', 'type=double', 'open=true', 'half=top',
                      'axis=x', 'axis=y', 'axis=z', 'waterlogged=true', 'facing=north',
                      'facing=south', 'facing=west', 'facing=east'):
            self.assertTrue(any(value in a for a in ss.SHOWCASE), value)
        self.assertEqual(sum('waterlogged=true' in a for a in ss.SHOWCASE), 4)

    def test_select(self):
        self.assertEqual([s.name for s in ss.select('s6')], ['s6-save', 's6-load'])
        self.assertEqual([s.name for s in ss.select('s3-terrain,s1-forest')],
                         ['s1-forest', 's3-terrain'])
        self.assertEqual(len(ss.select(None)), len(ss.SCENES))
        with self.assertRaises(ValueError):
            ss.select('s7')


class Runs(unittest.TestCase):
    def setUp(self):
        self.f = Fixture()

    def tearDown(self):
        self.f.close()

    def test_commands_carry_the_gpu_flags_and_the_pack(self):
        code, out = self.f.run('--dry-run', '--pack', self.f.pack, '--tiers', 'mid',
                               '--scenes', 's3-terrain')
        self.assertEqual(code, 0, out)
        lines = out.splitlines()
        self.assertEqual(len(lines), 2)
        for line in lines:
            self.assertIn(' --adapter nvidia --no-lkg --tier mid --timeout 120 ', line)
            self.assertIn('--world terrain --camera 16,140,16,-90,-60 --shot 12:00=', line)
        self.assertNotIn('--pack', lines[0])
        self.assertIn(f'--pack {self.f.pack} ', lines[1])
        self.assertIn('/Pack/mid/s3-terrain-noon.png', lines[1])

    def test_manifest_and_check(self):
        code, out = self.f.run('--pack', self.f.pack, '--scenes', 's1,s5')
        self.assertEqual(code, 0, out)
        manifest = self.f.path('out', 'manifest.txt')
        entries = ss.read_manifest(manifest)
        # (2 + 1 shots of s1, 3 x 2 of s5) x 3 tiers x 2 sources.
        self.assertEqual(len(entries), 9 * 3 * 2)
        self.assertIn('procedural/low/s1-forest-night.png', entries)
        self.assertIn('Pack/high/s5-fluids-t1.25-noon.png', entries)
        with open(manifest) as f:
            lines = f.read().splitlines()
        self.assertEqual(lines, sorted(lines, key=lambda line: line.split('  ', 1)[1]))
        with open(self.f.path('out', 'commands.txt')) as f:
            commands = f.read().splitlines()
        self.assertEqual(commands[0], f'# scenes fingerprint {ss.fingerprint()}; --repeat 1')
        self.assertEqual(len(commands), 1 + 5 * 3 * 2)
        # The same runs again: equal.
        code, out = self.f.run('--pack', self.f.pack, '--scenes', 's1,s5', '--check', manifest,
                               out='again')
        self.assertEqual(code, 0, out)
        self.assertIn('all 54 images equal', out)
        # Part of the set: only what ran is compared.
        code, out = self.f.run('--tiers', 'low', '--scenes', 's1-forest', '--check', manifest,
                               out='part')
        self.assertEqual(code, 0, out)
        self.assertIn('all 2 images equal', out)
        # One changed image fails.
        os.environ['FAKE_SALT'] = 's5-fluids-t0.5-night'
        try:
            code, out = self.f.run('--scenes', 's5', '--tiers', 'high', '--check', manifest,
                                   out='changed')
        finally:
            del os.environ['FAKE_SALT']
        self.assertEqual(code, 1, out)
        self.assertIn('differs: procedural/high/s5-fluids-t0.5-night.png', out)
        self.assertIn('1 of 6 images differ', out)
        # An image the baseline does not have fails.
        code, out = self.f.run('--scenes', 's3-terrain', '--tiers', 'high', '--check', manifest,
                               out='new')
        self.assertEqual(code, 1, out)
        self.assertIn('not in the baseline: procedural/high/s3-terrain-noon.png', out)

    def test_save_then_load(self):
        code, out = self.f.run('--scenes', 's6', '--tiers', 'high')
        self.assertEqual(code, 0, out)
        world = self.f.path('out', 'procedural', 'high', 'world')
        # Saved once by s6-save; s6-load read a copy and left it alone.
        with open(os.path.join(world, 'counter')) as f:
            self.assertEqual(f.read(), '1')
        entries = ss.read_manifest(self.f.path('out', 'manifest.txt'))
        self.assertEqual(len(entries), 4)
        # A rerun starts the save from an empty directory: the same images.
        code, out = self.f.run('--scenes', 's6', '--tiers', 'high', '--check',
                               self.f.path('out', 'manifest.txt'))
        self.assertEqual(code, 0, out)
        with open(os.path.join(world, 'counter')) as f:
            self.assertEqual(f.read(), '1')

    def test_load_world_reads_a_copy_of_the_given_world(self):
        b0 = self.f.path('b0-world')
        os.makedirs(b0)
        with open(os.path.join(b0, 'counter'), 'w') as f:
            f.write('7')
        code, out = self.f.run('--scenes', 's6-load', '--tiers', 'low', '--load-world', b0)
        self.assertEqual(code, 0, out)
        with open(os.path.join(b0, 'counter')) as f:
            self.assertEqual(f.read(), '7')
        # The image saw the given world (counter 7), not a fresh one.
        code, out = self.f.run('--scenes', 's6-load', '--tiers', 'low', out='fresh')
        self.assertEqual(code, 2, out)
        self.assertIn('no saved world', out)
        code, out = self.f.run('--scenes', 's6', '--tiers', 'low', out='fresh')
        self.assertEqual(code, 0, out)
        given = ss.read_manifest(self.f.path('out', 'manifest.txt'))
        fresh = ss.read_manifest(self.f.path('fresh', 'manifest.txt'))
        rel = 'procedural/low/s6-load-noon.png'
        self.assertNotEqual(given[rel], fresh[rel])

    def test_a_failed_run_exits_2(self):
        os.environ['FAKE_FAIL'] = '--world terrain'
        try:
            code, out = self.f.run('--scenes', 's3', '--tiers', 'high')
        finally:
            del os.environ['FAKE_FAIL']
        self.assertEqual(code, 2, out)
        self.assertIn('procedural/high/s3-terrain: exit 2', out)
        entries = ss.read_manifest(self.f.path('out', 'manifest.txt'))
        self.assertEqual(list(entries), ['procedural/high/s3-overworld-dry-noon.png'])

    def flaky(self, text, nth, *args, out):
        state = self.f.path(f'state-{out}')
        os.makedirs(state)
        os.environ.update(FAKE_FLAKY=f'{text}:{nth}', FAKE_STATE=state)
        try:
            return self.f.run(*args, out=out)
        finally:
            del os.environ['FAKE_FLAKY'], os.environ['FAKE_STATE']

    def test_repeat_keeps_the_most_frequent_image(self):
        code, out = self.f.run('--scenes', 's1-forest', '--tiers', 'high', out='plain')
        self.assertEqual(code, 0, out)
        plain = ss.read_manifest(self.f.path('plain', 'manifest.txt'))
        # The second of three runs changes the noon image: the other two win.
        code, out = self.flaky('s1-forest-noon', 2, '--scenes', 's1-forest', '--tiers', 'high',
                               '--repeat', '3', out='repeat')
        self.assertEqual(code, 0, out)
        self.assertEqual(ss.read_manifest(self.f.path('repeat', 'manifest.txt')), plain)
        rel = 'procedural/high/s1-forest-noon.png'
        self.assertIn(f'variants: {rel}: {plain[rel]} x2, ', out)
        with open(self.f.path('repeat', 'variants.txt')) as f:
            self.assertIn(f'variants: {rel}', f.read())
        self.assertEqual(ss.md5_file(self.f.path('repeat', rel)), plain[rel])
        folder = self.f.path('repeat', 'procedural', 'high')
        self.assertEqual(sorted(n for n in os.listdir(folder) if n.endswith('.png')),
                         ['s1-forest-night.png', 's1-forest-noon.png'])
        # The first run changes it: the other two still win.
        code, out = self.flaky('s1-forest-noon', 1, '--scenes', 's1-forest', '--tiers', 'high',
                               '--repeat', '3', out='first')
        self.assertEqual(code, 0, out)
        self.assertEqual(ss.md5_file(self.f.path('first', rel)), plain[rel])

    def test_retries_until_the_baseline_image(self):
        code, out = self.f.run('--scenes', 's2', '--tiers', 'high', out='b0')
        self.assertEqual(code, 0, out)
        manifest = self.f.path('b0', 'manifest.txt')
        # The first render of the cave differs; the first retry equals the baseline.
        code, out = self.flaky('s2-cave-noon', 1, '--scenes', 's2', '--tiers', 'high',
                               '--check', manifest, out='check')
        self.assertEqual(code, 0, out)
        self.assertIn('retry: procedural/high/s2-cave-noon.png equals the baseline on retry 1',
                      out)
        self.assertIn('all 3 images equal', out)
        rel = 'procedural/high/s2-cave-noon.png'
        self.assertEqual(ss.md5_file(self.f.path('check', rel)),
                         ss.read_manifest(manifest)[rel])
        # Without retries the same flake fails.
        code, out = self.flaky('s2-cave-noon', 1, '--scenes', 's2', '--tiers', 'high',
                               '--check', manifest, '--retries', '0', out='strict')
        self.assertEqual(code, 1, out)
        self.assertIn(f'differs: {rel}', out)

    def test_usage_errors(self):
        self.assertEqual(self.f.run('--tiers', 'ultra')[0], 64)
        self.assertEqual(self.f.run('--scenes', 'nope')[0], 64)
        self.assertEqual(self.f.run('--pack', self.f.root)[0], 64)
        self.assertEqual(self.f.run('--no-procedural')[0], 64)
        self.assertEqual(self.f.run('--repeat', '0')[0], 64)
        buf = io.StringIO()
        with contextlib.redirect_stderr(buf):
            self.assertEqual(ss.main([]), 64)

    def test_list(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.assertEqual(ss.main(['--list']), 0)
        lines = buf.getvalue().splitlines()
        self.assertEqual(len(lines), len(ss.SCENES) + 1)
        self.assertEqual(lines[-1], f'fingerprint {ss.fingerprint()}')


if __name__ == '__main__':
    unittest.main()
