"""Is the machine quiet enough to measure?

A timed run is only worth something when nothing else heavy runs: no rustc, cargo or linker, no
other boxel binary (a test, an example, another benchmark) and tools/wait-idle.sh says the CPU is
idle. `wait_quiet` waits for that. `Watcher` samples every 0.5 s while a run goes on, so a run
that overlapped something can be thrown away and redone. `run_lock` keeps two harnesses (two
agents) from measuring at the same time.

Linux only (/proc); elsewhere nothing counts as heavy and the CPU always looks idle.
"""
import contextlib
import fcntl
import os
import random
import re
import subprocess
import sys
import threading
import time

TOOLS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
WAIT_IDLE = os.path.join(TOOLS, 'wait-idle.sh')

# Builds: /proc/<pid>/comm keeps 15 characters, so the names are cut the same way.
BUILD = frozenset(name[:15] for name in (
    'rustc', 'cargo', 'cargo-clippy', 'clippy-driver', 'rustdoc', 'build-script-build',
    'cc', 'c++', 'cc1', 'cc1plus', 'collect2', 'ld', 'ld.lld', 'ld.gold', 'ld.bfd', 'lld',
    'rust-lld', 'mold'))
# Boxel binaries: anything run from a cargo target dir or from a benchmark binary cache.
BOXEL_EXE = re.compile(r'/target[^/]*/(debug|release|profiling)/|/bench-results/|/\.cache/boxel/')

# The rest of the machine may use this much CPU (percent, over any 2 s) during a run.
MAX_OTHER_CPU = float(os.environ.get('PERF_MAX_OTHER_CPU', '15'))
SAMPLE_S = 0.5


def classify(comm, exe):
    """The name a heavy process is reported with, or None: a build tool by its name, a boxel
    binary by where its executable lives."""
    if comm in BUILD:
        return comm
    if exe and BOXEL_EXE.search(exe):
        return 'boxel:' + os.path.basename(exe)
    return None


def _pids():
    try:
        return [int(p) for p in os.listdir('/proc') if p.isdigit()]
    except OSError:
        return []


def _read(path):
    try:
        with open(path) as f:
            return f.read()
    except OSError:
        return None


def descendants(root):
    """`root` and every process under it."""
    kids = {}
    for pid in _pids():
        stat = _read(f'/proc/{pid}/stat')
        if stat:
            ppid = int(stat[stat.rfind(')') + 2:].split()[1])
            kids.setdefault(ppid, []).append(pid)
    out, todo = set(), [root]
    while todo:
        pid = todo.pop()
        out.add(pid)
        todo.extend(kids.get(pid, ()))
    return out


def heavy_processes(skip=frozenset()):
    """Sorted names of the heavy processes running now, leaving out `skip` and this one."""
    names = set()
    me = os.getpid()
    for pid in _pids():
        if pid == me or pid in skip:
            continue
        comm = (_read(f'/proc/{pid}/comm') or '').strip()
        try:
            exe = os.readlink(f'/proc/{pid}/exe')
        except OSError:
            exe = ''
        name = classify(comm, exe)
        if name:
            names.add(name)
    return sorted(names)


def cpu_ticks():
    """(total, busy) jiffies of the whole machine."""
    line = (_read('/proc/stat') or 'cpu 0 0 0 0 0 0 0 0').splitlines()[0]
    v = [int(x) for x in line.split()[1:9]]
    return sum(v), sum(v) - v[3] - v[4]


def proc_ticks(pid):
    """utime + stime of `pid` (all its threads), None once it is gone."""
    stat = _read(f'/proc/{pid}/stat')
    if not stat:
        return None
    fields = stat[stat.rfind(')') + 2:].split()
    return int(fields[11]) + int(fields[12])


def other_cpu(samples, window=4):
    """(mean, max over `window` intervals) percent of CPU used by everything but the run, from
    (total, busy, run) tick samples."""
    def share(a, b):
        total = b[0] - a[0]
        return max(0.0, 100.0 * ((b[1] - a[1]) - (b[2] - a[2])) / total) if total > 0 else 0.0
    each = [share(a, b) for a, b in zip(samples, samples[1:])]
    spans = [share(samples[i], samples[i + window]) for i in range(len(samples) - window)]
    if not spans and len(samples) > 1:
        spans = [share(samples[0], samples[-1])]
    mean = round(sum(each) / len(each), 1) if each else 0.0
    return mean, round(max(spans), 1) if spans else 0.0


class Watcher:
    """Samples the machine every 0.5 s while `pid` runs: heavy processes that showed up and the
    CPU the rest of the machine used."""

    def __init__(self, pid):
        self.pid = pid
        self.heavy = set()
        self.samples = []
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._loop, daemon=True)

    def start(self):
        self._thread.start()
        return self

    def _loop(self):
        while True:
            ticks = proc_ticks(self.pid)
            if ticks is None:
                break
            total, busy = cpu_ticks()
            self.samples.append((total, busy, ticks))
            self.heavy.update(heavy_processes(descendants(self.pid)))
            if self._stop.wait(SAMPLE_S):
                break

    def stop(self):
        self._stop.set()
        self._thread.join()
        mean, max2s = other_cpu(self.samples)
        return {'heavy': sorted(self.heavy), 'other_cpu_mean': mean, 'other_cpu_max2s': max2s,
                'samples': len(self.samples)}


def overlap(watch):
    """Why a watched run does not count (another heavy process, a busy machine), or None."""
    if watch['heavy']:
        return 'overlapped ' + ', '.join(watch['heavy'])
    if watch['other_cpu_max2s'] > MAX_OTHER_CPU:
        return f"the rest of the machine used {watch['other_cpu_max2s']:.0f} % CPU"
    return None


def default_wait_s():
    """How long a run may wait for a quiet machine: PERF_MAX_WAIT seconds, default 1800."""
    return float(os.environ.get('PERF_MAX_WAIT', '1800'))


def wait_quiet(max_wait_s=None, say=None):
    """Waits until no heavy process runs and tools/wait-idle.sh passes, then checks once more
    after a random 1 to 3 s (another harness may have passed its own check meanwhile). True when
    quiet, False after `max_wait_s` (default PERF_MAX_WAIT, 1800 s)."""
    if max_wait_s is None:
        max_wait_s = default_wait_s()
    say = say or (lambda text: print(text, file=sys.stderr, flush=True))
    start = time.monotonic()
    told = -60.0
    while True:
        heavy = heavy_processes()
        if not heavy:
            idle = subprocess.run([WAIT_IDLE, '30'], stdout=subprocess.DEVNULL,
                                  stderr=subprocess.DEVNULL).returncode == 0
            if idle:
                time.sleep(random.uniform(1.0, 3.0))
                heavy = heavy_processes()
                if not heavy:
                    return True
        waited = time.monotonic() - start
        if waited >= max_wait_s:
            return False
        if waited - told >= 60:
            told = waited
            what = ', '.join(heavy) if heavy else 'the CPU is busy'
            say(f'perf: waiting for a quiet machine ({what}), {waited:.0f} s so far')
        time.sleep(random.uniform(8.0, 14.0))


@contextlib.contextmanager
def run_lock(say=None):
    """Held while a harness measures: runs of two harnesses never overlap."""
    folder = os.environ.get('XDG_RUNTIME_DIR') or '/tmp'
    path = os.path.join(folder, 'boxel-perf.lock')
    with open(path, 'w') as f:
        try:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            (say or (lambda t: print(t, file=sys.stderr, flush=True)))(
                'perf: another perf run is measuring, waiting for it')
            fcntl.flock(f, fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(f, fcntl.LOCK_UN)
