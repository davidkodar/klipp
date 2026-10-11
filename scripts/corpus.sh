#!/bin/sh
# Deterministic saved-project inputs; generated media stays under target/.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT"
exec cargo run -p klipp-corpus -- "$@"
