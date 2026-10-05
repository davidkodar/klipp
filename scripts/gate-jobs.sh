#!/bin/sh
# How many jobs a gate build runs: half the cores, and no more than the
# memory free right now allows at about 2.5 GB a job, at least two.
#
# Half the cores alone was not enough. A test binary's link takes about
# 2 GB, and with the whole suite to build sixteen of them linking at once
# drove the machine into swap and the OOM killer took a release build
# running beside it (2026-10-05). Memory is read when the gate starts,
# not when the push joined the queue: what is free then is what counts.
#
# Usage: gate-jobs.sh [cores [available_mb]]; both read from the machine
# when left out. Prints the number.
cores=${1:-$(nproc 2>/dev/null || echo 4)}
avail=${2:-$(awk '/^MemAvailable:/ { print int($2 / 1024) }' /proc/meminfo 2>/dev/null)}
jobs=$((cores / 2))
case $avail in
    ''|*[!0-9]*) ;;
    *)
        by_memory=$((avail / 2560))
        [ "$by_memory" -lt "$jobs" ] && jobs=$by_memory
        ;;
esac
[ "$jobs" -lt 2 ] && jobs=2
echo "$jobs"
