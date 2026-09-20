#!/bin/sh
# Run clippy, rustdoc and the workspace's tests before anything leaves
# this machine.
#
# This is the project's real gate. There is a CI workflow, but it stays
# dormant while the repository is private — Actions minutes are metered
# there, and a cold build of this workspace would eat the month's budget
# in a handful of runs. It switches itself on when the repo goes public,
# which is both when the minutes become free and when contributors who
# cannot be made to install this hook start showing up.
#
# So for now, without this, nothing ever runs the test suite except a
# human remembering to. That matters more here than it would elsewhere
# for two reasons:
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
# Clippy and rustdoc run first, with warnings as errors. Both were taken
# to zero across the workspace in September 2026, and a warning count that
# is allowed to drift back up stops meaning anything: the last pass found
# two undefined transmutes, a dropped video packet and three UI bugs in a
# list that had twice been written off as style. Lints the project has
# decided against are declined in Cargo.toml, each with its reason, so a
# new warning here is a new decision to make, not noise to wave through.
#
# Kept in the repo rather than only in .git/hooks so it is versioned and
# survives a fresh clone. Install with:
#
#     scripts/install-hooks.sh
#
# Timing, warm, on this machine: about 75 s for the lot — clippy 15 s,
# rustdoc 8 s, the tests the rest, of which klipp-engine is the largest
# share because it builds GPU devices and drives a real engine. It was
# 165 s until the engine tests stopped sleeping on fixed timers and
# started waiting for the frame buffer to settle. A cold build is longer,
# but that build was needed anyway.
#
# To bypass for a genuine reason (pushing a work-in-progress branch,
# mid-bisect, a machine with no GPU):
#
#     KLIPP_SKIP_TESTS=1 git push
#
# `git push --no-verify` also works and skips the LFS hook too, which is
# why the explicit variable exists — it skips these checks and nothing
# else.

set -e

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$ROOT" || exit 0

if [ "$KLIPP_SKIP_TESTS" = "1" ]; then
    printf '\n  pre-push: KLIPP_SKIP_TESTS=1 set — checks NOT run for this push.\n\n' >&2
    exit 0
fi

if ! command -v cargo >/dev/null 2>&1; then
    printf '\n  pre-push: cargo not on PATH; cannot run the tests. Refusing rather\n' >&2
    printf '            than letting an unchecked push through silently. Set\n' >&2
    printf '            KLIPP_SKIP_TESTS=1 if this is deliberate.\n\n' >&2
    exit 1
fi

printf '\n  pre-push: sign-off, clippy, rustdoc, then the workspace tests (~75 s warm).\n'
printf '            KLIPP_SKIP_TESTS=1 git push   to bypass.\n\n'

if ! "$ROOT/scripts/check-signoff.sh"; then
    printf '  pre-push: authorship check failed — push refused.\n\n' >&2
    exit 1
fi

if ! cargo clippy --workspace --all-targets -- -D warnings; then
    printf '\n  pre-push: clippy warned — push refused.\n' >&2
    printf '            Fix it, or if the lint is wrong for this code, decline\n' >&2
    printf '            it with a reason: in place with #[allow], or for the\n' >&2
    printf '            whole workspace under [workspace.lints.clippy].\n\n' >&2
    exit 1
fi

if ! RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps; then
    printf '\n  pre-push: rustdoc warned — push refused.\n' >&2
    printf '            Usually a [`link`] to something renamed or private, or a\n' >&2
    printf '            bare [0,1] or Vec<T> in prose that needs backticks.\n\n' >&2
    exit 1
fi

if cargo test --workspace; then
    printf '\n  pre-push: tests pass.\n\n'
    exit 0
fi

printf '\n  pre-push: tests FAILED — push refused.\n' >&2
printf '            If gpu_pipelines_build is among them, a shader stopped\n' >&2
printf '            compiling and the rest of the GPU suite is skipping\n' >&2
printf '            rather than failing. Fix that one first.\n\n' >&2
exit 1
