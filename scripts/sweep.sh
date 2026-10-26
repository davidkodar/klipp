#!/bin/sh
# The measured sweeps: every audio effect, picture effect, transition,
# colour control and export preset measured against what it says, and
# the save/load and undo/redo round trips. About a minute once built.
#
#   scripts/sweep.sh            every part
#   scripts/sweep.sh colour     one part (audio, picture, transitions,
#                               colour, round_trips, export)
#
# Each part writes its table to target/sweep/<part>.txt; this prints
# each part's one-line summary. A part fails on anything new it finds,
# or on a known item that now measures right (take it off the part's
# KNOWN list, in its test file). Cargo's output goes to
# target/sweep/<part>.log.

set -u
cd "$(dirname "$0")/.." || exit 1
mkdir -p target/sweep

run() {
    part=$1
    shift
    if cargo test "$@" >"target/sweep/$part.log" 2>&1; then
        status=ok
    else
        status=FAILED
        failed=1
    fi
    summary=$(head -n 1 "target/sweep/$part.txt" 2>/dev/null)
    echo "$status  ${summary:-sweep_$part: no report, see target/sweep/$part.log}"
}

failed=0
want=${1:-all}
for part in audio picture transitions colour round_trips export; do
    [ "$want" = all ] || [ "$want" = "$part" ] || continue
    case $part in
        audio) run audio -p klipp-audio --test sweep_audio ;;
        export) run export -p klipp-render --test all sweep_export ;;
        *) run "$part" -p klipp-document --test "sweep_$part" ;;
    esac
done
exit $failed
