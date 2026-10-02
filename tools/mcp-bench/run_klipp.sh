#!/bin/sh
# The test Klipp: its own control socket, so the user's Klipp keeps its own.
export KLIPP_CONTROL_SOCKET="${KLIPP_BENCH_SOCKET:-${XDG_RUNTIME_DIR:-/tmp}/klipp-bench.sock}"
exec "${KLIPP_BENCH_APP:-$(dirname "$0")/bin/klipp-app}" > "$(dirname "$0")/klipp.out" 2>&1
