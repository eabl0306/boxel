#!/usr/bin/env python3
"""Generic post-rename patches for boxel_ecs tests/doctests (no library code touched).
1) drop bevy_reflect usage in #[cfg(test)] code, 2) mark reflect doc examples `ignore`,
3) add explicit 'static inside ParamSet<...> (rustc's implied-bounds hack only matches crate `bevy_ecs`)."""
import os, re, sys
root = sys.argv[1]
stats = {'reflect_lines': 0, 'reflect_doc_blocks': 0, 'paramset_lines': 0, 'removed_tests': 0}
def static_ify(line):
    return re.sub(r"(Query|Single|Populated)<&(?!')(mut )?", lambda m: f"{m.group(1)}<&'static {m.group(2) or ''}", line).replace(", &World)>", ", &'static World)>")
for dp, _, fs in os.walk(root):
    for fn in fs:
        if not fn.endswith('.rs'): continue
        p = os.path.join(dp, fn); s = open(p).read(); orig = s
        # remove whole reflect-only test fn
        m = re.search(r"\n    #\[test\]\n    fn mut_untyped_to_reflect\(\) \{.*?\n    \}\n", s, re.S)
        if m: s = s[:m.start()] + "\n" + s[m.end():]; stats['removed_tests'] += 1
        for pat in [r"\n    use bevy_reflect::[^;]*;", r"\n        reflect::ReflectResource,",
                    r"\n    // EntityHashMap should implement Reflect\n    #\[cfg\(feature = \"bevy_reflect\"\)\]\n    assert_impl_all!\(EntityHashMap::<i32>: Reflect\);",
                    r"\n    #\[reflect\(Resource\)\]"]:
            s, n = re.subn(pat, "", s); stats['reflect_lines'] += n
        s = s.replace("#[derive(Resource, Default, Reflect)]", "#[derive(Resource, Default)]")
        lines = s.split('\n'); out = []; in_block = None; depth = 0
        for i, l in enumerate(lines):
            st = l.strip()
            # doc code blocks mentioning bevy_reflect -> ignore
            fence = re.match(r"(///|//!)\s*```", st)
            if fence and in_block:
                in_block = None
            elif fence:
                in_block = True
            if fence and in_block and re.match(r"(///|//!)\s*```(rust|no_run|should_panic)?\s*$", st):
                j = i + 1; blk = []
                while j < len(lines) and not re.match(r"(///|//!)\s*```\s*$", lines[j].strip()): blk.append(lines[j]); j += 1
                if any('bevy_reflect' in b or 'derive(Reflect' in b for b in blk):
                    l = re.sub(r"```(rust|no_run|should_panic)?", "```ignore", l, count=1); stats['reflect_doc_blocks'] += 1
            # ParamSet lifetimes (single or multi-line)
            if 'ParamSet<' in l or depth > 0:
                nl = static_ify(l)
                if nl != l: stats['paramset_lines'] += 1; l = nl
                if 'ParamSet<(' in l and ')>' not in l.split('ParamSet<(')[1]: depth = 1
                elif depth and ')>' in l: depth = 0
            out.append(l)
        s = '\n'.join(out)
        if s != orig: open(p, 'w').write(s)
print(stats)
