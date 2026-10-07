#!/usr/bin/env python3
"""Tables from boxel stress runs (the files `examples/stress.rs --summary` writes).

usage:
  python3 tools/bench_report.py RUN...              one row per run
  python3 tools/bench_report.py --compare BASE NEW  each metric of two runs and the change in %

RUN, BASE and NEW are summary files (Markdown with a JSON block, or plain JSON) or directories
(every summary inside, not recursive). Comparing two directories pairs the runs with the same
world, light (--light), scenario, radius, vertical radius, speed, fps limit, vsync, physics
bodies (--bodies), scripted edits (--edits), backend, tier and adapter; a different clock time (--time)
only gets a note, since day and night change no mesh. Light is 'on', 'off' or 'none' (runs from
before the light render, without a clock, had no voxel light); runs with a clock but from before
--light had it on. A metric that was 0 in BASE has no percent change ('n/a') but still gets a
verdict (0 spikes before, 3 after: worse). Runs with --edits also compare the edit latency, and
runs that recorded pacing (presented frames, gate waits) compare it too. Runs from before --vsync
count as vsync on only if they asked for --present vsync.
Output is Markdown, so it can go into docs as is.
Exit codes: 0 ok, 1 usage error or a file without a summary, 2 nothing to compare."""
import json
import os
import sys

FORMAT = 1
DATA_HEADING = '## Data'

# (label, path into the JSON, direction: +1 higher is better, -1 lower is better, 0 neutral)
METRICS = [
    ('avg fps', ('frames', 'avg_fps'), 1),
    ('p50 ms', ('frames', 'p50_ms'), -1),
    ('p95 ms', ('frames', 'p95_ms'), -1),
    ('p99 ms', ('frames', 'p99_ms'), -1),
    ('p99.9 ms', ('frames', 'p999_ms'), -1),
    ('max ms', ('frames', 'max_ms'), -1),
    ('1% low fps', ('frames', 'low_1_fps'), 1),
    ('frames > 6.94 ms', ('frames', 'over_144hz'), -1),
    ('frames > 16.7 ms', ('frames', 'over_60hz'), -1),
    ('border frames p99 ms', ('frames', 'crossing', 'p99_ms'), -1),
    ('border frames max ms', ('frames', 'crossing', 'max_ms'), -1),
    ('generated/s', ('world', 'generated_per_s'), 0),
    ('meshes/s', ('world', 'meshes_per_s'), 0),
    ('jobs queued max', ('world', 'queued', 'max'), -1),
    ('queue trend jobs/s', ('world', 'backlog_trend_per_s'), -1),
    ('border catch-up avg ms', ('world', 'border_catch_up', 'avg_ms'), -1),
    ('border catch-up max ms', ('world', 'border_catch_up', 'max_ms'), -1),
    ('borders finished', ('world', 'border_catch_up', 'finished'), 0),
    ('catch-up after stop s', ('world', 'catch_up_s'), -1),
]

# Metrics of runs with --edits (compared when either run has them).
EDIT_METRICS = [
    ('edit frames p50', ('world', 'edits', 'frames_p50'), -1),
    ('edit frames max', ('world', 'edits', 'frames_max'), -1),
    ('edit ms p50', ('world', 'edits', 'ms_p50'), -1),
    ('edit ms max', ('world', 'edits', 'ms_max'), -1),
    ('edit batches split', ('world', 'edits', 'batches_split'), -1),
    ('light fan-out', ('world', 'edits', 'light_fanout'), 0),
]

# Pacing metrics (runs from 2026-10-07 on; compared when either run has them).
PACING_METRICS = [
    ('frames > 7.5 ms', ('frames', 'over_7_5ms'), -1),
    ('frame sd ms', ('frames', 'stddev_ms'), -1),
    ('presented fps', ('pacing', 'presented_fps'), 1),
    ('presented p99 ms', ('pacing', 'presented_intervals', 'p99_ms'), -1),
    ('presented sd ms', ('pacing', 'presented_intervals', 'stddev_ms'), -1),
    ('presented gaps', ('pacing', 'presented_intervals', 'gaps'), -1),
    ('gate waits', ('pacing', 'gate_waits'), -1),
    ('gate misses', ('pacing', 'gate_misses'), -1),
    ('callback gaps', ('pacing', 'callbacks', 'gaps'), -1),
]

# Settings that make two runs comparable.
SETTINGS = [
    ('world', ('run', 'world')),
    ('light', ('run', 'light')),
    ('scenario', ('run', 'scenario')),
    ('radius', ('run', 'radius')),
    ('vertical', ('run', 'vertical')),
    ('speed', ('run', 'speed')),
    ('fps limit', ('run', 'fps_limit')),
    ('vsync', ('run', 'vsync')),
    ('present mode', ('run', 'present_mode')),
    ('bodies', ('run', 'bodies')),
    ('edits', ('run', 'edits')),
    ('clock', ('run', 'time')),
    ('seed', ('run', 'seed')),
    ('backend', ('run', 'backend')),
    ('tier', ('run', 'tier')),
    ('adapter', ('run', 'adapter')),
    ('build', ('run', 'build')),
    ('duration s', ('run', 'duration_s')),
    ('window', ('run', 'window')),
]


class ReportError(Exception):
    """A file is not a stress summary."""


def parse_summary(text, name='summary'):
    """The JSON document of a summary: a JSON file, or the first ```json block after the
    '## Data' heading of a Markdown summary."""
    stripped = text.lstrip()
    if stripped.startswith('{'):
        data = json.loads(stripped)
    else:
        at = text.find(DATA_HEADING)
        start = text.find('```json\n', at) if at >= 0 else -1
        end = text.find('\n```', start + 8) if start >= 0 else -1
        if end < 0:
            raise ReportError(f'{name}: no "{DATA_HEADING}" section with a ```json block')
        data = json.loads(text[start + 8:end])
    if not isinstance(data, dict) or data.get('format') != FORMAT:
        raise ReportError(f'{name}: not a stress summary of format {FORMAT}')
    return data


def load(path):
    with open(path, encoding='utf-8') as f:
        data = parse_summary(f.read(), path)
    data['_path'] = path
    run = data.get('run')
    if isinstance(run, dict):
        # Runs from before the vertical radius existed loaded only the generator's layers.
        run.setdefault('vertical', 'fixed')
        # Runs from before --world streamed the forest, or the plain terrain without trees.
        if not run.get('world'):
            run['world'] = 'forest' if run.get('trees', True) else 'terrain'
        # Runs from before --fps-limit were all uncapped.
        run.setdefault('fps_limit', 0)
        # Runs from before --vsync: on only if they asked for vsync.
        if not run.get('vsync'):
            run['vsync'] = 'on' if run.get('present') == 'vsync' else 'off'
        # Runs from before --bodies had no physics bodies.
        run.setdefault('bodies', '')
        # Runs from before --time had no day and night.
        run.setdefault('time', '')
        # Runs from before --light: on if they had the clock (it came with the light render),
        # else from before the light ('none').
        if not run.get('light'):
            run['light'] = 'on' if run['time'] else 'none'
        # Whether the run had --edits (the world's edit latency, null without them).
        run['edits'] = get(data, ('world', 'edits')) is not None
    return data


def expand(paths):
    """Summary files of `paths` (directories: their *.md and *.json files that hold a summary,
    sorted by name)."""
    runs = []
    for path in paths:
        if os.path.isdir(path):
            for name in sorted(os.listdir(path)):
                full = os.path.join(path, name)
                if os.path.isfile(full) and name.endswith(('.md', '.json')):
                    try:
                        runs.append(load(full))
                    except (ReportError, ValueError):
                        continue
        else:
            runs.append(load(path))
    return runs


def get(data, path):
    for key in path:
        if not isinstance(data, dict) or data.get(key) is None:
            return None
        data = data[key]
    return data


def title(run):
    r = run['run']
    text = f"{r['scenario']} r{r['radius']}"
    if r['scenario'] in ('fly', 'dive'):
        text += f" {fmt(r['speed'])} vox/s"
    if r.get('world', 'forest') != 'forest':
        text += f" {r['world']}"
    if r.get('light') == 'off':
        text += ' light off'
    if r.get('fps_limit'):
        text += f" limit {r['fps_limit']} fps"
    if r.get('vsync') == 'on':
        text += ' vsync'
    if r.get('bodies'):
        text += f" bodies {r['bodies']}"
    if r.get('edits'):
        text += ' edits'
    return text


def key(run):
    """Runs with the same key are comparable (same world, light, scenario, radius, height, speed,
    fps limit, vsync, physics bodies, scripted edits and GPU)."""
    r = run['run']
    return (r.get('world', 'forest'), r.get('light', 'none'), r['scenario'], r['radius'],
            r.get('vertical', 'fixed'), r['speed'], r.get('fps_limit') or 0,
            r.get('vsync') or 'off', r.get('bodies') or '', bool(r.get('edits')), r['backend'],
            r['tier'], r.get('adapter'))


def fmt(value):
    if value is None:
        return '-'
    if isinstance(value, bool):
        return 'yes' if value else 'no'
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        if value == int(value) and abs(value) < 1e15:
            return str(int(value))
        return f'{value:.2f}' if abs(value) < 100 else f'{value:.0f}'
    return str(value)


def change(base, new, direction):
    """(percent text, verdict) for one metric. Changes under 1 % get no verdict; from 0 there
    is no percent ('n/a'), but any change gets one (new spikes are worse)."""
    if not isinstance(base, (int, float)) or not isinstance(new, (int, float)):
        return '-', ''
    if base == new:
        return '0 %', ''
    up = 'better' if direction > 0 else 'worse'
    down = 'worse' if direction > 0 else 'better'
    if base == 0:
        return 'n/a', (up if new > base else down) if direction else ''
    pct = (new - base) / abs(base) * 100.0
    verdict = ''
    if direction and abs(pct) >= 1.0:
        verdict = up if new > base else down
    return f'{pct:+.1f} %', verdict


def table(runs):
    lines = [
        '| run | GPU | frames | avg fps | p50 ms | p99 ms | p99.9 ms | max ms | 1% low fps '
        '| > 6.94 ms | > 16.7 ms | border max ms | generated/s | queued max | border lag max ms '
        '| > 7.5 ms | shown fps |',
        '|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|',
    ]
    for run in runs:
        f, w, r = run['frames'], run['world'], run['run']
        cells = [
            title(run), f"{r['backend']} {r['tier']}", f['count'], f['avg_fps'], f['p50_ms'],
            f['p99_ms'], f['p999_ms'], f['max_ms'], f['low_1_fps'], f['over_144hz'],
            f['over_60hz'], get(f, ('crossing', 'max_ms')), w['generated_per_s'],
            get(w, ('queued', 'max')), get(w, ('border_catch_up', 'max_ms')),
            f.get('over_7_5ms'), get(run, ('pacing', 'presented_fps')),
        ]
        lines.append('| ' + ' | '.join(fmt(c) for c in cells) + ' |')
    return '\n'.join(lines)


def compare(base, new):
    names = (os.path.basename(base['_path']), os.path.basename(new['_path']))
    lines = [f'### {title(base)}: {names[0]} -> {names[1]}', '']
    differ = [
        f'{name} {fmt(get(base, path))} -> {fmt(get(new, path))}'
        for name, path in SETTINGS
        if get(base, path) != get(new, path)
    ]
    if differ:
        lines += ['Note: the settings differ (' + ', '.join(differ) + ').', '']
    lines += ['| metric | base | new | change | |', '|---|---:|---:|---:|---|']
    metrics = METRICS
    if get(base, ('world', 'edits')) is not None or get(new, ('world', 'edits')) is not None:
        metrics = metrics + EDIT_METRICS
    if get(base, ('pacing',)) is not None or get(new, ('pacing',)) is not None:
        metrics = metrics + PACING_METRICS
    for name, path, direction in metrics:
        b, n = get(base, path), get(new, path)
        pct, verdict = change(b, n, direction)
        lines.append(f'| {name} | {fmt(b)} | {fmt(n)} | {pct} | {verdict} |')
    return '\n'.join(lines)


def pairs(base_runs, new_runs):
    """Runs of both sides with the same key, in the order of `base_runs` (the last run of each
    key wins on both sides)."""
    news = {key(r): r for r in new_runs}
    bases = {key(r): r for r in base_runs}
    return [(bases[k], news[k]) for k in dict.fromkeys(key(r) for r in base_runs) if k in news]


def main(argv):
    if not argv or argv[0] in ('-h', '--help'):
        print(__doc__)
        return 0 if argv else 1
    try:
        if argv[0] == '--compare':
            if len(argv) != 3:
                print('usage: bench_report.py --compare BASE NEW', file=sys.stderr)
                return 1
            base, new = argv[1], argv[2]
            if os.path.isdir(base) and os.path.isdir(new):
                matched = pairs(expand([base]), expand([new]))
            else:
                matched = [(load(base), load(new))]
            if not matched:
                print('no runs with the same world, light, scenario, radius, speed, fps limit, '
                      'vsync, bodies, edits, backend, tier and adapter', file=sys.stderr)
                return 2
            print('\n\n'.join(compare(b, n) for b, n in matched))
            return 0
        runs = expand(argv)
    except (OSError, ReportError, ValueError) as e:
        print(f'bench_report: {e}', file=sys.stderr)
        return 1
    if not runs:
        print('bench_report: no summaries found', file=sys.stderr)
        return 2
    print(table(runs))
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
