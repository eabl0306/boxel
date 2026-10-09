#!/usr/bin/env bash
# Runs a heavy command (cargo build/test/clippy) without making the desktop slow.
#   usage: tools/quiet.sh cargo test --workspace
# Meant for background work (agents, scripts), not for your own interactive builds.
#
# - Refuses to start when the disk is almost full (QUIET_MIN_FREE_GB, default 40).
# - At most QUIET_SLOTS heavy commands at once (default 2); the rest wait their turn.
# - Each one gets nproc/QUIET_SLOTS cargo jobs, low CPU and IO priority, and a soft RAM cap
#   (QUIET_MEM_HIGH, default 60% of RAM split between the slots) so it never pushes your
#   apps into swap. Over the cap the command slows down instead of the desktop.
# - No incremental cache (CARGO_INCREMENTAL=0): it is most of a target dir and one-shot builds
#   never reuse it.
set -euo pipefail

[[ $# -gt 0 ]] || { echo "usage: tools/quiet.sh <command> [args...]" >&2; exit 64; }

slots=${QUIET_SLOTS:-2}
min_free_gb=${QUIET_MIN_FREE_GB:-40}
lock_dir=${QUIET_LOCK_DIR:-${XDG_RUNTIME_DIR:-/tmp}/boxel-quiet}
cpus=$(nproc 2>/dev/null || echo 4)

check_dir=${CARGO_TARGET_DIR:-.}
[[ -d $check_dir ]] || check_dir=.
free_gb=$(df -Pk "$check_dir" | awk 'NR == 2 { print int($4 / 1048576) }')
if (( free_gb < min_free_gb )); then
    echo "quiet: only ${free_gb} GB free on $(df -P "$check_dir" | awk 'NR == 2 { print $6 }')" \
        "(need ${min_free_gb}); free space first (old target dirs?)" >&2
    exit 75
fi

export CARGO_INCREMENTAL=${CARGO_INCREMENTAL:-0}
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-$(( cpus / slots > 0 ? cpus / slots : 1 ))}

mkdir -p "$lock_dir"
waited=0
while :; do
    for ((i = 1; i <= slots; i++)); do
        exec {fd}>"$lock_dir/slot$i.lock"
        if flock -n "$fd"; then
            # The lock stays held through exec: fd is inherited by the command and its children.
            # They can tell they hold a slot (tools/perf refuses to run inside one).
            export BOXEL_QUIET_SLOT=$i
            prio=(nice -n 10)
            command -v ionice >/dev/null && prio+=(ionice -c2 -n7)
            # --expand-environment=no: otherwise systemd-run rewrites "$VAR" in the arguments.
            scope=(systemd-run --user --scope --quiet --expand-environment=no)
            if command -v systemd-run >/dev/null && "${scope[@]}" true 2>/dev/null; then
                mem_kb=$(awk '/MemTotal/ { print $2 }' /proc/meminfo)
                mem_high=${QUIET_MEM_HIGH:-$(( mem_kb * 6 / 10 / slots ))K}
                exec "${scope[@]}" -p "MemoryHigh=$mem_high" -p CPUWeight=20 -- "${prio[@]}" "$@"
            fi
            exec "${prio[@]}" "$@"
        fi
        exec {fd}>&-
    done
    (( waited++ % 30 == 0 )) && echo "quiet: $slots heavy commands already running, waiting..." >&2
    sleep 2
done
