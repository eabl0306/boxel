#!/usr/bin/env python3
"""Fails if an engine crate names concrete content (issue #48, ADR 0029): the engine keeps only
mechanisms, and the blocks, textures, rules and generators of the example game live in
`crates/boxel_demo`.

1. Structure (`cargo metadata --locked`): no workspace package but `boxel_demo` depends on
   `boxel_demo` as a normal or build dependency (dev is fine), and `boxel_demo` depends on no
   engine crate but `boxel_voxel`, `boxel_worldgen` and `boxel_pack` (so engine unit tests that
   use the demo see one copy of each crate type).
2. Scan: every `.rs` file under `crates/*/src` (but `boxel_demo` and `boxel_facade_check`) and
   under `src/`. Files named `tests.rs`, paths with a `tests` or `benches` component and the item
   after `#[cfg(test)]` are skipped; an inner `#![cfg(test)]` skips its file when it opens the
   file, and else only the `{...}` block it sits in. A small lexer tells code from comments,
   strings, chars and lifetimes, and matches braces.
3. Rules:
   - `string`: a string literal holding `boxel:` followed by `[a-z0-9_{]` (which also catches
     `format!("boxel:{name}")`), unless that name is reserved (`RESERVED`).
   - `comment`: `boxel:[a-z0-9_]+` in a comment or doc comment, unless reserved (docs use
     `game:` examples).
   - `ident`: a content identifier (`IDENTS`) or `BlockModel::<content variant>` in code.
4. No allowlist: every offense fails. (The migration of #48 kept a list that could only lose
   lines; it ended empty and was deleted.)

Prints `path:line: rule: token` per offense and exits 1, or prints `no content in engine
crates` and exits 0. Standard library only."""
import argparse
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

DEMO = 'boxel_demo'
# The engine crates the demo may depend on (normal and build dependencies).
DEMO_DEPS = {'boxel_voxel', 'boxel_worldgen', 'boxel_pack'}
# Crates under crates/ that are not scanned.
UNSCANNED = {DEMO, 'boxel_facade_check'}

# Engine names that stay: the two built-in states and the save format keys.
RESERVED = {
    'boxel:', 'boxel:air', 'boxel:unknown',
    'boxel:format', 'boxel:states', 'boxel:generator', 'boxel:ticks', 'boxel:clock',
    'boxel:entities', 'boxel:body',
}
IDENTS = {
    'default_blocks', 'with_default_blocks', 'BuiltinLamp', 'LAMPS', 'LAVA_EMISSION',
    'TORCH_EMISSION',
    'FluidKind', 'StairShape', 'FenceSides', 'WallSide', 'WallSides', 'TexturePattern',
    'TerrainGenerator', 'TerrainSettings', 'ForestGenerator', 'OverworldGenerator',
    'OverworldSettings',
    'rule_for_model', 'use_rule_for_model',
    'GRASS_TINT', 'FOLIAGE_TINT', 'WATER_TINT',
    DEMO,
}
# `BlockModel::<variant>` for the content models.
MODEL_VARIANTS = {'Stairs', 'Fence', 'Torch', 'WallTorch', 'Wall', 'FenceGate'}

STRING_NAME = re.compile(r'boxel:[a-z0-9_{][a-z0-9_{}]*')
COMMENT_NAME = re.compile(r'boxel:[a-z0-9_]+')


# ---------------------------------------------------------------------------------------------
# Lexer


class Token:
    __slots__ = ('kind', 'text', 'line', 'body', 'body_at')

    def __init__(self, kind, text, line, body=None, body_at=0):
        self.kind = kind  # comment, string, char, lifetime, ident, number, punct
        self.text = text
        self.line = line
        # Strings: the characters between the quotes (escapes left as written), and where they
        # start in `text`.
        self.body = body
        self.body_at = body_at

    def __repr__(self):
        return f'Token({self.kind}, {self.text!r}, {self.line})'


class LexError(Exception):
    pass


def _ident_start(c):
    return c == '_' or c.isalpha()


def _ident_char(c):
    return c == '_' or c.isalnum()


def tokenize(src):
    """The tokens of Rust source `src` (whitespace dropped)."""
    tokens = []
    i, n, line = 0, len(src), 1

    def string_at(start, quote_at):
        """A `"..."` string whose opening quote is at `quote_at` (prefix from `start`)."""
        j = quote_at + 1
        while j < n:
            c = src[j]
            if c == '\\':
                j += 2
                continue
            if c == '"':
                return j + 1, src[quote_at + 1:j], quote_at + 1
            j += 1
        raise LexError(f'line {line}: unterminated string')

    def raw_string_at(hashes_at):
        """A raw string: `hashes_at` is the first `#` or the quote after the `r`."""
        j = hashes_at
        while j < n and src[j] == '#':
            j += 1
        hashes = j - hashes_at
        if j >= n or src[j] != '"':
            return None
        close = '"' + '#' * hashes
        end = src.find(close, j + 1)
        if end < 0:
            raise LexError(f'line {line}: unterminated raw string')
        return end + len(close), src[j + 1:end], j + 1

    while i < n:
        c = src[i]
        if c == '\n':
            line += 1
            i += 1
            continue
        if c.isspace():
            i += 1
            continue
        start = i
        if src.startswith('//', i):
            end = src.find('\n', i)
            end = n if end < 0 else end
            tokens.append(Token('comment', src[i:end], line))
            i = end
            continue
        if src.startswith('/*', i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith('/*', j):
                    depth, j = depth + 1, j + 2
                elif src.startswith('*/', j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            if depth:
                raise LexError(f'line {line}: unterminated block comment')
            text = src[i:j]
            tokens.append(Token('comment', text, line))
            line += text.count('\n')
            i = j
            continue
        if c == '"':
            end, body, at = string_at(i, i)
            text = src[i:end]
            tokens.append(Token('string', text, line, body, at - start))
            line += text.count('\n')
            i = end
            continue
        if c == "'":
            # A char (`'a'`, `'\n'`, `'\u{1F600}'`, `'"'`) or a lifetime or label (`'a`).
            if i + 1 < n and src[i + 1] == '\\':
                # The escaped character may itself be a quote: `'\''`.
                j = src.find("'", i + 3)
                if j < 0 or '\n' in src[i:j]:
                    raise LexError(f'line {line}: unterminated char')
                tokens.append(Token('char', src[i:j + 1], line))
                i = j + 1
                continue
            if i + 2 < n and src[i + 2] == "'":
                tokens.append(Token('char', src[i:i + 3], line))
                line += src[i + 1] == '\n'
                i += 3
                continue
            j = i + 1
            while j < n and _ident_char(src[j]):
                j += 1
            tokens.append(Token('lifetime', src[i:j], line))
            i = j
            continue
        if _ident_start(c):
            j = i + 1
            while j < n and _ident_char(src[j]):
                j += 1
            word = src[i:j]
            nxt = src[j] if j < n else ''
            # Literal prefixes: b"", c"", r"", r#""#, br"", cr"", b''.
            if word in ('r', 'br', 'cr') and nxt in ('"', '#'):
                raw = raw_string_at(j)
                if raw is not None:
                    end, body, at = raw
                    text = src[start:end]
                    tokens.append(Token('string', text, line, body, at - start))
                    line += text.count('\n')
                    i = end
                    continue
            if word in ('b', 'c') and nxt == '"':
                end, body, at = string_at(start, j)
                text = src[start:end]
                tokens.append(Token('string', text, line, body, at - start))
                line += text.count('\n')
                i = end
                continue
            if word == 'b' and nxt == "'":
                k = j + 1
                if k < n and src[k] == '\\':
                    k = src.find("'", k + 2)
                    k = n if k < 0 else k
                else:
                    k += 1
                if k >= n or src[k] != "'":
                    raise LexError(f'line {line}: unterminated byte char')
                tokens.append(Token('char', src[start:k + 1], line))
                i = k + 1
                continue
            if word == 'r' and nxt == '#' and j + 1 < n and _ident_start(src[j + 1]):
                k = j + 2
                while k < n and _ident_char(src[k]):
                    k += 1
                tokens.append(Token('ident', src[j + 1:k], line))
                i = k
                continue
            tokens.append(Token('ident', word, line))
            i = j
            continue
        if c.isdigit():
            j = i + 1
            while j < n and (_ident_char(src[j])):
                j += 1
            tokens.append(Token('number', src[i:j], line))
            i = j
            continue
        if src.startswith('::', i):
            tokens.append(Token('punct', '::', line))
            i += 2
            continue
        tokens.append(Token('punct', c, line))
        i += 1
    return tokens


# ---------------------------------------------------------------------------------------------
# `#[cfg(test)]` items


def _attribute_end(code, k):
    """`code[k]` is the `#` of `#[...]` or `#![...]`: the index after its `]`, or None."""
    j = k + 1
    if j < len(code) and code[j].text == '!':
        j += 1
    if j >= len(code) or code[j].text != '[':
        return None
    depth = 0
    while j < len(code):
        t = code[j].text
        if code[j].kind == 'punct' and t in '([{':
            depth += 1
        elif code[j].kind == 'punct' and t in ')]}':
            depth -= 1
            if depth == 0:
                return j + 1
        j += 1
    return None


def _is_cfg_test(attr):
    """`attr` is the token texts inside `#[...]`: `cfg(test)` or `cfg(all(..., test, ...))`."""
    text = ''.join(attr)
    if text == 'cfg(test)':
        return True
    if text.startswith('cfg(all(') and text.endswith('))'):
        args, depth, cur = [], 0, ''
        for ch in text[len('cfg(all('):-2]:
            if ch == ',' and depth == 0:
                args.append(cur)
                cur = ''
                continue
            depth += ch == '('
            depth -= ch == ')'
            cur += ch
        args.append(cur)
        return 'test' in args
    return False


def _enclosing_block(code, k):
    """The indexes of the `{` that encloses `code[k]` and of its matching `}`, or None at the
    top level (or when the braces do not match)."""
    depth, j = 0, k - 1
    while j >= 0:
        t = code[j]
        if t.kind == 'punct' and t.text == '}':
            depth += 1
        elif t.kind == 'punct' and t.text == '{':
            if depth == 0:
                break
            depth -= 1
        j -= 1
    else:
        return None
    opening = j
    while j < len(code):
        t = code[j]
        if t.kind == 'punct' and t.text == '{':
            depth += 1
        elif t.kind == 'punct' and t.text == '}':
            depth -= 1
            if depth == 0:
                return opening, j
        j += 1
    return None


def test_only(tokens):
    """The indexes (into `tokens`) of the tokens inside `#[cfg(test)]` items, the attribute
    included. An inner `#![cfg(test)]` covers the whole file only among the inner attributes
    that open it; further down it covers only the `{...}` block it sits in (a module or a
    function body), so an empty `mod x { #![cfg(test)] }` cannot hide the code around it."""
    code = [k for k, t in enumerate(tokens) if t.kind != 'comment']
    ct = [tokens[k] for k in code]
    # The end of the inner attributes that open the file.
    head = 0
    while head + 1 < len(ct) and ct[head].kind == 'punct' and ct[head].text == '#' \
            and ct[head + 1].text == '!':
        end = _attribute_end(ct, head)
        if end is None:
            break
        head = end
    skipped = set()
    pos = 0
    while pos < len(ct):
        if not (ct[pos].text == '#' and ct[pos].kind == 'punct'):
            pos += 1
            continue
        end = _attribute_end(ct, pos)
        if end is None:
            pos += 1
            continue
        inner = ct[pos + 1].text == '!'
        body = [t.text for t in ct[pos + (3 if inner else 2):end - 1]]
        if not _is_cfg_test(body):
            pos = end
            continue
        if inner:
            if pos < head:
                return set(range(len(tokens)))
            block = _enclosing_block(ct, pos)
            if block is None:
                # Not at the top and in no block: rustc rejects it, so it hides nothing.
                pos = end
                continue
            first, last = block
            skipped.update(range(code[first], code[last] + 1))
            pos = last + 1
            continue
        # Further attributes of the same item.
        item = end
        while item < len(ct) and ct[item].text == '#':
            more = _attribute_end(ct, item)
            if more is None:
                break
            item = more
        # The item: up to a `;` or `,` at depth 0, through a `{...}` block, or up to the `}`
        # that closes the enclosing scope.
        depth, j = 0, item
        while j < len(ct):
            t = ct[j]
            if t.kind == 'punct':
                if t.text in '([':
                    depth += 1
                elif t.text in ')]':
                    depth -= 1
                elif t.text == '{':
                    braces = 1
                    j += 1
                    while j < len(ct) and braces:
                        if ct[j].kind == 'punct' and ct[j].text == '{':
                            braces += 1
                        elif ct[j].kind == 'punct' and ct[j].text == '}':
                            braces -= 1
                        j += 1
                    if depth == 0:
                        break
                    continue
                elif t.text == '}':
                    break
                elif t.text in ';,' and depth == 0:
                    j += 1
                    break
            j += 1
        first, last = code[pos], code[j - 1] if j > pos else code[pos]
        skipped.update(range(first, last + 1))
        pos = j
    return skipped


# ---------------------------------------------------------------------------------------------
# Rules


def scan_source(src):
    """The offenses of one file as `(line, rule, token)`, in source order."""
    tokens = tokenize(src)
    skipped = test_only(tokens)
    found = []
    prev = []  # the last two code tokens
    for k, t in enumerate(tokens):
        if k in skipped:
            continue
        if t.kind == 'string':
            for m in STRING_NAME.finditer(t.body):
                if m.group() not in RESERVED:
                    at = t.line + t.text[:t.body_at + m.start()].count('\n')
                    found.append((at, 'string', m.group()))
        elif t.kind == 'comment':
            for m in COMMENT_NAME.finditer(t.text):
                if m.group() not in RESERVED:
                    at = t.line + t.text[:m.start()].count('\n')
                    found.append((at, 'comment', m.group()))
        elif t.kind == 'ident':
            if t.text in IDENTS:
                found.append((t.line, 'ident', t.text))
            elif (t.text in MODEL_VARIANTS and len(prev) == 2 and prev[0].text == 'BlockModel'
                  and prev[1].text == '::'):
                found.append((t.line, 'ident', f'BlockModel::{t.text}'))
        if t.kind != 'comment':
            prev = (prev + [t])[-2:]
    return found


def scanned_files(root):
    """The repository-relative paths of the scanned files, sorted, with their crate name."""
    files = []
    crates = os.path.join(root, 'crates')
    roots = [('boxel', 'src')]
    if os.path.isdir(crates):
        roots += [(c, os.path.join('crates', c, 'src')) for c in sorted(os.listdir(crates))
                  if c not in UNSCANNED and os.path.isdir(os.path.join(crates, c, 'src'))]
    for crate, rel in roots:
        top = os.path.join(root, rel)
        for dirpath, dirnames, filenames in os.walk(top):
            dirnames[:] = sorted(d for d in dirnames if d not in ('tests', 'benches'))
            for name in sorted(filenames):
                if not name.endswith('.rs') or name == 'tests.rs':
                    continue
                path = os.path.relpath(os.path.join(dirpath, name), root).replace(os.sep, '/')
                files.append((crate, path))
    return files


def scan_tree(root):
    """Every offense under `root` as `(crate, path, line, rule, token)`."""
    out = []
    for crate, path in scanned_files(root):
        with open(os.path.join(root, path), encoding='utf-8') as f:
            src = f.read()
        try:
            found = scan_source(src)
        except LexError as e:
            raise LexError(f'{path}: {e}') from None
        out += [(crate, path, line, rule, token) for line, rule, token in found]
    return out


# ---------------------------------------------------------------------------------------------
# Structure


def check_structure(metadata):
    """Dependency offenses from `cargo metadata` output, as `(manifest, message)`."""
    members = set(metadata.get('workspace_members', []))
    out = []
    for p in sorted(metadata['packages'], key=lambda p: p['name']):
        if p['id'] not in members:
            continue
        manifest = p.get('manifest_path', p['name'])
        for d in p.get('dependencies', []):
            kind = d.get('kind') or 'normal'
            if kind == 'dev':
                continue
            name = d['name']
            if p['name'] != DEMO and name == DEMO:
                out.append((manifest, f'{p["name"]} depends on {DEMO} ({kind}); only '
                                      'dev-dependencies may'))
            if p['name'] == DEMO and (name == 'boxel' or name.startswith('boxel_')) \
                    and name not in DEMO_DEPS:
                allowed = ', '.join(sorted(DEMO_DEPS))
                out.append((manifest, f'{DEMO} depends on {name} ({kind}); only {allowed} '
                                      'may be'))
    return out


def cargo_metadata(root):
    out = subprocess.run(
        ['cargo', 'metadata', '--format-version', '1', '--all-features', '--locked',
         '--no-deps', '--manifest-path', os.path.join(root, 'Cargo.toml')],
        check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def run(root, metadata, out=None):
    """The check; returns the exit code."""
    out = out or sys.stdout
    lines = [f'{manifest}: dependency: {message}' for manifest, message in
             check_structure(metadata)] if metadata is not None else []
    lines += [f'{path}:{line}: {rule}: {token}' for _, path, line, rule, token in scan_tree(root)]
    if lines:
        print('\n'.join(lines), file=out)
        print(f'{len(lines)} content problems in engine crates (issue #48)', file=out)
        return 1
    print('no content in engine crates', file=out)
    return 0


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    parser.add_argument('--root', default=ROOT, help='repository root (default: this one)')
    parser.add_argument('--metadata', help='cargo metadata JSON file instead of running cargo')
    parser.add_argument('--no-structure', action='store_true',
                        help='skip the dependency check (no cargo)')
    args = parser.parse_args(argv)
    if args.no_structure:
        metadata = None
    elif args.metadata:
        with open(args.metadata, encoding='utf-8') as f:
            metadata = json.load(f)
    else:
        metadata = cargo_metadata(args.root)
    try:
        return run(args.root, metadata)
    except LexError as e:
        print(f'no_content: cannot read {e}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
