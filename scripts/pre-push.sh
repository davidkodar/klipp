#!/bin/sh
# Run the workspace's tests before anything leaves this machine.
#
# This is the project's only automated gate. There is no CI — the repo is
# private and cloud runners were ruled out — so without this, nothing ever
# runs the test suite except a human remembering to. That matters more
# here than it would elsewhere for two reasons:
#
#   * the suite is the correctness argument for a colour pipeline whose
#     CPU and GPU paths have to agree, and most of those parity tests can
#     only tell you something on a machine with a GPU. This box has one;
#     a cloud runner would not, and would have skipped them silently;
#
#   * a shader that fails to compile makes its converter return None, and
#     around ninety GPU tests are written to *skip* on that and report
#     ok. `klipp-effects`' `gpu_pipelines_build` canary turns that into a
#     failure, but only if something runs it.
#
# Kept in the repo rather than only in .git/hooks so it is versioned and
# survives a fresh clone. Install with:
#
#     scripts/install-hooks.sh
#
# Timing, warm, on this machine: about 140 s for the whole workspace, of
# which klipp-engine is 54 s — it builds GPU devices and its HDR
# reference-output tests wait on real timers. A cold build is longer, but
# that build was needed anyway.
#
# To bypass for a genuine reason (pushing a work-in-progress branch,
# mid-bisect, a machine with no GPU):
#
#     KLIPP_SKIP_TESTS=1 git push
#
# `git push --no-verify` also works and skips the LFS hook too, which is
# why the explicit variable exists — it skips the tests and nothing else.

set -e

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$ROOT" || exit 0

if [ "$KLIPP_SKIP_TESTS" = "1" ]; then
    printf '\n  pre-push: KLIPP_SKIP_TESTS=1 set — tests NOT run for this push.\n\n' >&2
    exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
    printf '\n  pre-push: cargo not on PATH; cannot run the tests. Refusing rather\n' >&2
    printf '            than letting an unchecked push through silently. Set\n' >&2
    printf '            KLIPP_SKIP_TESTS=1 if this is deliberate.\n\n' >&2
    exit 1
fi

printf '\n  pre-push: running the workspace tests (~140 s warm).\n'
printf '            KLIPP_SKIP_TESTS=1 git push   to bypass.\n\n'

if cargo test --workspace; then
    printf '\n  pre-push: tests pass.\n\n'
    exit 0
fi

printf '\n  pre-push: tests FAILED — push refused.\n' >&2
printf '            If gpu_pipelines_build is among them, a shader stopped\n' >&2
printf '            compiling and the rest of the GPU suite is skipping\n' >&2
printf '            rather than failing. Fix that one first.\n\n' >&2
exit 1
