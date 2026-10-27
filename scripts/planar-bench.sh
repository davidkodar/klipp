#!/bin/sh
# The planar tracker's real-footage bench (#2754): fetch each clip named in
# crates/klipp-effects/tests/planar_bench.txt, check its sha256, and run
# the bench over them. The clips are kept in a cache, never in the
# repository; KLIPP_PLANAR_BENCH names another folder for them.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
DIR=${KLIPP_PLANAR_BENCH:-${XDG_CACHE_HOME:-$HOME/.cache}/klipp/planar-bench}
mkdir -p "$DIR"
grep '^clip ' "$ROOT/crates/klipp-effects/tests/planar_bench.txt" | while read -r _ name sha url; do
    f="$DIR/$name.mp4"
    if ! echo "$sha  $f" | sha256sum -c --status 2>/dev/null; then
        echo "fetching $name"
        curl -fsSL -o "$f.part" "$url"
        if ! echo "$sha  $f.part" | sha256sum -c --status; then
            echo "$name: what $url gave is not the clip annotated (sha256 differs)" >&2
            rm -f "$f.part"
            exit 1
        fi
        mv "$f.part" "$f"
    fi
done
cd "$ROOT"
KLIPP_PLANAR_BENCH="$DIR" exec cargo test -p klipp-effects --test planar_bench -- --ignored --nocapture "$@"
