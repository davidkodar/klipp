#!/bin/sh
# How many jobs a gate build runs: half the cores, and no more than the
# memory free right now allows at about 1.8 GB a job, at least two.
#
# Half the cores alone was not enough. A test binary's link takes about
# 2 GB, and with the whole suite to build sixteen of them linking at once
# drove the machine into swap and the OOM killer took a release build
# running beside it (2026-10-05). Memory is read when the gate starts,
# not when the push joined the queue: what is free then is what counts.
#
# 1.8 GB since the gate builds with line tables only (#1046). Measured on
# its first run, with 6 jobs: the largest link 1.37 GB (2 GB before), the
# largest rustc 2.2 GB, at most 9.9 GB in use at once, so 1.65 GB a job,
# and never under 7.5 GB free. This sizes rustc/link jobs only;
# KLIPP_GATE_MB_PER_JOB overrides that estimate. rustdoc's 6.8 GB peak
# needs a separate cap: pre-push.sh defaults to one doc job (#1240).
# KLIPP_GATE_DOC_JOBS overrides it for a quiet machine.
#
# Usage: gate-jobs.sh [cores [available_mb]]; both read from the machine
# when left out. Prints the number.
cores=${1:-$(nproc 2>/dev/null || echo 4)}
avail=${2:-$(awk '/^MemAvailable:/ { print int($2 / 1024) }' /proc/meminfo 2>/dev/null)}
jobs=$((cores / 2))
case $avail in
    ''|*[!0-9]*) ;;
    *)
        per_job=${KLIPP_GATE_MB_PER_JOB:-1800}
        case $per_job in ''|*[!0-9]*|0) per_job=1800 ;; esac
        by_memory=$((avail / per_job))
        [ "$by_memory" -lt "$jobs" ] && jobs=$by_memory
        ;;
esac
[ "$jobs" -lt 2 ] && jobs=2
echo "$jobs"
