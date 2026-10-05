#!/usr/bin/env bash
# Waits until the CPU is mostly idle, so a benchmark is not measuring someone else's build.
#   usage: tools/wait-idle.sh [max_wait_s]     (default 60)
# Exit 0 when idle, 1 when still busy after waiting (prints what is using the CPU).
# IDLE_MAX_BUSY: busy percentage that still counts as idle (default 15). Linux only; elsewhere
# it always says idle.
set -euo pipefail

max_wait=${1:-60}
max_busy=${IDLE_MAX_BUSY:-15}
[[ -r /proc/stat ]] || exit 0

busy_pct() {
    local a b
    read -r -a a < /proc/stat
    sleep 2
    read -r -a b < /proc/stat
    # cpu user nice system idle iowait irq softirq steal
    local idle=$(( (b[4] + b[5]) - (a[4] + a[5]) ))
    local total=0 i
    for i in 1 2 3 4 5 6 7 8; do total=$(( total + b[i] - a[i] )); done
    (( total > 0 )) || { echo 0; return; }
    echo $(( 100 * (total - idle) / total ))
}

waited=0
while :; do
    busy=$(busy_pct)
    (( busy <= max_busy )) && exit 0
    if (( waited >= max_wait )); then
        echo "wait-idle: CPU still ${busy}% busy after ${waited}s. Top processes:" >&2
        ps -eo pcpu,comm --sort=-pcpu | sed -n '2,6p' >&2
        exit 1
    fi
    (( waited == 0 )) && echo "wait-idle: CPU ${busy}% busy, waiting up to ${max_wait}s..." >&2
    sleep 3
    waited=$(( waited + 5 ))
done
