#!/usr/bin/env python3
"""Tests of tools/no_content.py: python3 tools/no_content_test.py (or `just test-tools`).

The rules run on small fixture trees in a temporary directory and on fake `cargo metadata`
output, so no cargo is needed. The lexer also reads every scanned file of this repository, so a
Rust construct it does not know fails here before it fails CI."""
import contextlib
import io
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import no_content as nc  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def found(src):
    """`(rule, token)` of the offenses in `src`."""
    return [(rule, token) for _, rule, token in nc.scan_source(src)]


class Tree:
    """A fixture repository in a temporary directory."""

    def __init__(self, files):
        self.dir = tempfile.TemporaryDirectory()
        self.root = self.dir.name
        for path, text in files.items():
            self.write(path, text)

    def write(self, path, text):
        full = os.path.join(self.root, path)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, 'w', encoding='utf-8') as f:
            f.write(text)

    def run(self, metadata=None, write=False):
        out = io.StringIO()
        code = nc.run(self.root, metadata, write=write, out=out)
        return code, out.getvalue()

    def close(self):
        self.dir.cleanup()


def metadata(*packages):
    """Fake `cargo metadata`: `(name, [(dependency, kind)])` per workspace package."""
    pkgs = [{
        'id': f'{name} 0.1.0', 'name': name, 'manifest_path': f'/repo/{name}/Cargo.toml',
        'dependencies': [{'name': d, 'kind': k} for d, k in deps],
    } for name, deps in packages]
    return {'packages': pkgs, 'workspace_members': [p['id'] for p in pkgs]}


class Rules(unittest.TestCase):
    def test_a_string_in_code_is_a_hit(self):
        self.assertEqual(found('fn f() -> &str { "boxel:stone" }'), [('string', 'boxel:stone')])
        self.assertEqual(found('const S: &[u8] = b"boxel:sand";'), [('string', 'boxel:sand')])
        self.assertEqual(found('let s = r#"say "boxel:dirt""#;'), [('string', 'boxel:dirt')])

    def test_a_format_is_a_hit(self):
        self.assertEqual(found('let n = format!("boxel:{name}");'),
                         [('string', 'boxel:{name}')])
        self.assertEqual(found('let n = format!("boxel:{name}_flow");'),
                         [('string', 'boxel:{name}_flow')])
        self.assertEqual(found('let n = format!("boxel:{}", x);'), [('string', 'boxel:{}')])

    def test_a_space_after_the_colon_is_not_a_name(self):
        self.assertEqual(found('eprintln!("boxel: {summary}"); let p = "boxel:";'), [])
        self.assertEqual(found('use boxel::voxel; let s = "boxel::render";'), [])

    def test_a_doc_is_a_hit(self):
        src = ('/// Places `boxel:torch` on a wall.\n'
               '//! Inner docs name boxel:lava too.\n'
               '/* block, /* nested */ boxel:glass */\n'
               'fn f() {}\n')
        self.assertEqual(nc.scan_source(src), [
            (1, 'comment', 'boxel:torch'),
            (2, 'comment', 'boxel:lava'),
            (3, 'comment', 'boxel:glass'),
        ])
        # `game:` examples and `boxel:{name}` in prose pass.
        self.assertEqual(found('/// `game:stone`, `boxel:{name}`\nfn f() {}'), [])

    def test_a_name_is_reported_as_itself(self):
        self.assertEqual(found('let s = "boxel:stone_slab[type=top]";'),
                         [('string', 'boxel:stone_slab')])
        self.assertEqual(found('/// `boxel:stone_slab[type=top]`\nfn f() {}'),
                         [('comment', 'boxel:stone_slab')])
        # A reserved prefix does not hide a longer name.
        self.assertEqual(found('let s = "boxel:airship";'), [('string', 'boxel:airship')])

    def test_reserved_names_are_allowed(self):
        names = sorted(nc.RESERVED)
        src = '\n'.join(f'let x = "{n}"; // {n}' for n in names)
        self.assertEqual(found(src), [])
        self.assertEqual(found('let e = "unknown block, drawn as boxel:unknown";'), [])
        self.assertEqual(found('let e = "boxel:air or boxel:stone";'),
                         [('string', 'boxel:stone')])

    def test_identifiers_are_hits_in_code_only(self):
        self.assertEqual(found('use boxel_voxel::{StairShape, WallSides};'),
                         [('ident', 'StairShape'), ('ident', 'WallSides')])
        self.assertEqual(found('let r = BlockRegistry::default_blocks();'),
                         [('ident', 'default_blocks')])
        self.assertEqual(found('use boxel_demo::blocks;'), [('ident', 'boxel_demo')])
        self.assertEqual(found('let s = "StairShape"; // TexturePattern\nlet WallSidesX = 1;'), [])
        self.assertEqual(found('let r#FluidKind = 1;'), [('ident', 'FluidKind')])

    def test_content_models_are_hits(self):
        src = ('match m {\n'
               '    BlockModel::Stairs { .. } => 1,\n'
               '    boxel_voxel::BlockModel::FenceGate { .. } => 2,\n'
               '    BlockModel::Cube(_) | BlockModel::Slab(_) => 3,\n'
               '    Other::Wall => 4,\n'
               '}\n')
        self.assertEqual(nc.scan_source(src), [
            (2, 'ident', 'BlockModel::Stairs'),
            (3, 'ident', 'BlockModel::FenceGate'),
        ])

    def test_a_cfg_test_item_is_skipped(self):
        src = ('fn engine() {}\n'
               '#[cfg(test)]\n'
               'mod tests {\n'
               '    const CLOSE: &str = "}";\n'
               '    fn nested() { if true { let _ = "boxel:stone"; } }\n'
               "    const C: char = '}';\n"
               '    use super::StairShape;\n'
               '}\n'
               'fn after() -> &str { "boxel:dirt" }\n')
        self.assertEqual(nc.scan_source(src), [(9, 'string', 'boxel:dirt')])

    def test_cfg_test_items_ending_in_a_semicolon_or_comma(self):
        src = ('#[cfg(test)]\n'
               'mod tests;\n'
               'struct S {\n'
               '    #[cfg(test)]\n'
               '    #[allow(dead_code)]\n'
               '    shape: StairShape,\n'
               '    sides: WallSides,\n'
               '}\n'
               '#[cfg(test)]\n'
               'use crate::FluidKind;\n'
               '#[cfg(all(test, feature = "x"))]\n'
               'static LIST: [&str; 2] = ["boxel:a", "boxel:b"];\n'
               '#[cfg(any(test, feature = "testing"))]\n'
               'fn not_skipped() -> TerrainGenerator { todo!() }\n')
        self.assertEqual(nc.scan_source(src), [
            (7, 'ident', 'WallSides'),
            (14, 'ident', 'TerrainGenerator'),
        ])

    def test_an_inner_cfg_test_skips_the_file(self):
        self.assertEqual(found('#![cfg(test)]\nfn f() -> &str { "boxel:stone" }'), [])
        self.assertEqual(found('//! Docs.\n#![allow(dead_code)]\n#![cfg(test)]\n'
                               'fn f() -> &str { "boxel:stone" }'), [])

    def test_an_inner_cfg_test_further_down_skips_only_its_block(self):
        # A module or function body that turns itself off hides its own braces only, not the
        # engine code around it.
        src = ('fn engine() -> &str { "boxel:stone" }\n'
               'mod probe {\n'
               '    #![cfg(test)]\n'
               '    const X: &str = "boxel:dirt";\n'
               '    mod inner { const Y: &str = "}"; }\n'
               '}\n'
               'fn body() { #![cfg(test)] let _ = "boxel:sand"; }\n'
               'fn after() -> FluidKind { todo!() }\n')
        self.assertEqual(nc.scan_source(src), [
            (1, 'string', 'boxel:stone'),
            (8, 'ident', 'FluidKind'),
        ])
        # Not at the top of the file and in no block (rustc rejects it): it hides nothing.
        self.assertEqual(found('fn f() -> &str { "boxel:stone" }\n#![cfg(test)]\n'),
                         [('string', 'boxel:stone')])

    def test_chars_and_lifetimes(self):
        src = ("fn f<'a>(x: &'a str) -> char { let q = '\"'; let b = b'\\''; '\\u{7d}' }\n"
               "fn g() -> &'static str { \"boxel:sand\" }\n"
               "fn h() { 'outer: loop { break 'outer; } let c = '\\''; let d = '\\\\'; }\n"
               "const X: &str = \"boxel:gravel\";\n")
        self.assertEqual(nc.scan_source(src), [
            (2, 'string', 'boxel:sand'),
            (4, 'string', 'boxel:gravel'),
        ])

    def test_lines_inside_multiline_tokens(self):
        src = ('/*\n'
               ' * boxel:log\n'
               ' */\n'
               'const S: &str = "first\n'
               'boxel:leaves";\n')
        self.assertEqual(nc.scan_source(src), [
            (2, 'comment', 'boxel:log'),
            (5, 'string', 'boxel:leaves'),
        ])


class Tree_(unittest.TestCase):
    def setUp(self):
        self.tree = Tree({
            'src/lib.rs': 'pub use boxel_worldgen::TerrainGenerator;\n',
            'crates/boxel_voxel/src/lib.rs': 'pub const A: &str = "boxel:stone";\n',
            'crates/boxel_voxel/src/tests.rs': 'const T: &str = "boxel:stone";\n',
            'crates/boxel_voxel/src/tests/more.rs': 'const T: &str = "boxel:stone";\n',
            'crates/boxel_voxel/src/benches/b.rs': 'const T: &str = "boxel:stone";\n',
            'crates/boxel_voxel/tests/golden.rs': 'const T: &str = "boxel:stone";\n',
            'crates/boxel_demo/src/lib.rs': 'pub const S: &str = "boxel:stone";\n',
            'crates/boxel_facade_check/src/lib.rs': 'const S: &str = "boxel:stone";\n',
        })

    def tearDown(self):
        self.tree.close()

    def test_tests_files_and_demo_are_skipped(self):
        files = nc.scanned_files(self.tree.root)
        self.assertEqual(files, [('boxel', 'src/lib.rs'),
                                 ('boxel_voxel', 'crates/boxel_voxel/src/lib.rs')])
        code, out = self.tree.run()
        self.assertEqual(code, 1)
        self.assertEqual(out.splitlines()[:2], [
            'src/lib.rs:1: ident: TerrainGenerator',
            'crates/boxel_voxel/src/lib.rs:1: string: boxel:stone',
        ])

    def test_the_allowlist_ratchet(self):
        self.tree.write('tools/no_content_allow/boxel.txt', 'src/lib.rs\tTerrainGenerator\n')
        self.tree.write('tools/no_content_allow/boxel_voxel.txt',
                        '# phase 1 deletes these\n'
                        'crates/boxel_voxel/src/lib.rs\tboxel:stone\n')
        code, out = self.tree.run()
        self.assertEqual((code, out.strip()), (
            0, 'no content in engine crates outside the allowlist (2 entries left in '
               'tools/no_content_allow/)'))
        # Unlisted: a new offense fails even in a listed file.
        self.tree.write('crates/boxel_voxel/src/lib.rs',
                        'pub const A: &str = "boxel:stone";\npub fn f(_: FluidKind) {}\n')
        code, out = self.tree.run()
        self.assertEqual(code, 1)
        self.assertIn('crates/boxel_voxel/src/lib.rs:2: ident: FluidKind', out)
        # Stale: a listed offense that is gone fails until its line is deleted.
        self.tree.write('crates/boxel_voxel/src/lib.rs', 'pub const A: u8 = 1;\n')
        code, out = self.tree.run()
        self.assertEqual(code, 1)
        self.assertIn('crates/boxel_voxel/src/lib.rs: stale: boxel:stone (listed in '
                      'tools/no_content_allow/boxel_voxel.txt; delete the line)', out)
        self.tree.write('tools/no_content_allow/boxel_voxel.txt', '')
        self.assertEqual(self.tree.run()[0], 0)

    def test_bad_allowlist_lines_fail(self):
        self.tree.write('tools/no_content_allow/boxel.txt',
                        'src/lib.rs TerrainGenerator\n'
                        'crates/boxel_voxel/src/lib.rs\tboxel:stone\n')
        code, out = self.tree.run()
        self.assertEqual(code, 1)
        self.assertIn("tools/no_content_allow/boxel.txt:1: not path<TAB>token", out)
        self.assertIn('tools/no_content_allow/boxel.txt:2: crates/boxel_voxel/src/lib.rs is not '
                      'in crate boxel', out)

    def test_write_allowlist_creates_the_files(self):
        self.tree.write('tools/no_content_allow/boxel_world.txt', 'old\tentry\n')
        code, out = self.tree.run(write=True)
        self.assertEqual(code, 0, out)
        top = os.path.join(self.tree.root, nc.ALLOW_DIR)
        self.assertEqual(sorted(os.listdir(top)), ['boxel.txt', 'boxel_voxel.txt'])
        with open(os.path.join(top, 'boxel_voxel.txt'), encoding='utf-8') as f:
            self.assertEqual(f.read(), 'crates/boxel_voxel/src/lib.rs\tboxel:stone\n')

    def test_a_clean_tree_passes(self):
        self.tree.write('src/lib.rs', '//! An engine: `game:stone` blocks.\n')
        self.tree.write('crates/boxel_voxel/src/lib.rs', 'pub const AIR: &str = "boxel:air";\n')
        code, out = self.tree.run(metadata(('boxel', []), ('boxel_voxel', [])))
        self.assertEqual((code, out), (0, 'no content in engine crates\n'))


class Structure(unittest.TestCase):
    def test_only_dev_dependencies_on_the_demo(self):
        meta = metadata(
            ('boxel_world', [('boxel_voxel', None), ('boxel_demo', 'dev')]),
            ('boxel_render', [('boxel_demo', None)]),
            ('boxel_mesh', [('boxel_demo', 'build')]),
            ('boxel_demo', [('boxel_voxel', None), ('boxel_worldgen', None),
                            ('boxel_pack', None), ('boxel_world', 'dev'), ('proptest', 'dev'),
                            ('serde', None)]),
        )
        self.assertEqual([m for _, m in nc.check_structure(meta)], [
            'boxel_mesh depends on boxel_demo (build); only dev-dependencies may',
            'boxel_render depends on boxel_demo (normal); only dev-dependencies may',
        ])

    def test_the_demo_depends_on_three_engine_crates_only(self):
        meta = metadata(('boxel_demo', [('boxel_voxel', None), ('boxel_world', None),
                                        ('boxel', None), ('boxel_mesh', 'build')]))
        self.assertEqual([m for _, m in nc.check_structure(meta)], [
            'boxel_demo depends on boxel_world (normal); only boxel_pack, boxel_voxel, '
            'boxel_worldgen may be',
            'boxel_demo depends on boxel (normal); only boxel_pack, boxel_voxel, boxel_worldgen '
            'may be',
            'boxel_demo depends on boxel_mesh (build); only boxel_pack, boxel_voxel, '
            'boxel_worldgen may be',
        ])

    def test_packages_outside_the_workspace_are_ignored(self):
        meta = metadata(('boxel_render', [('boxel_demo', None)]))
        meta['workspace_members'] = []
        self.assertEqual(nc.check_structure(meta), [])

    def test_dependency_offenses_fail_the_run(self):
        tree = Tree({'src/lib.rs': '\n'})
        try:
            code, out = tree.run(metadata(('boxel_render', [('boxel_demo', None)])))
        finally:
            tree.close()
        self.assertEqual(code, 1)
        self.assertIn('/repo/boxel_render/Cargo.toml: dependency: boxel_render depends on '
                      'boxel_demo (normal)', out)


class Repository(unittest.TestCase):
    def test_the_lexer_reads_every_scanned_file(self):
        files = nc.scanned_files(ROOT)
        self.assertGreater(len(files), 100)
        for _, path in files:
            with open(os.path.join(ROOT, path), encoding='utf-8') as f:
                src = f.read()
            with self.subTest(path=path):
                tokens = nc.tokenize(src)
                # Every brace of the file is balanced in code: the lexer kept strings, chars
                # and comments apart.
                depth = 0
                for t in tokens:
                    if t.kind == 'punct' and t.text == '{':
                        depth += 1
                    elif t.kind == 'punct' and t.text == '}':
                        depth -= 1
                        self.assertGreaterEqual(depth, 0, f'line {t.line}')
                self.assertEqual(depth, 0)

    def test_main_runs_without_cargo(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            code = nc.main(['--no-structure'])
        self.assertIn(code, (0, 1))
        self.assertTrue(out.getvalue().strip())


if __name__ == '__main__':
    unittest.main()
