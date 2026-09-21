#!/bin/sh
# Run clippy, rustdoc and the workspace's tests before anything leaves
# this machine.
#
# This is the project's only automated gate. There is no CI: on a private
# repo the minutes are metered, and whatever a runner reported would be
# advisory anyway, because it has no GPU. CONTRIBUTING.md has the long
# version under "The gate runs on your machine".
#
# So without this, nothing ever runs the test suite except a human
# remembering to. That matters more here than it would elsewhere for two
# reasons:
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

# Cargo defaults to one job per core, which on a many-core machine is
# enough concurrent rustc and LLVM to exhaust memory when several large
# crates link at once. The OOM killer then takes rustc mid-write and
# leaves a truncated object behind, which surfaces later as
# `undefined hidden symbol: anon.*.llvm.*` in some crate nobody
# touched — a failure that looks like anything except what it is.
#
# Half the cores, at least two. Respected if already set.
if [ -z "$CARGO_BUILD_JOBS" ]; then
    _cores=$(nproc 2>/dev/null || echo 4)
    CARGO_BUILD_JOBS=$(( _cores / 2 ))
    [ "$CARGO_BUILD_JOBS" -lt 2 ] && CARGO_BUILD_JOBS=2
    export CARGO_BUILD_JOBS
fi

printf '\n  pre-push: sign-off, clippy, rustdoc, then the workspace tests (~75 s warm).\n'
printf '            KLIPP_SKIP_TESTS=1 git push   to bypass.\n\n'

# Direct pushes to main, refused here because GitHub will not refuse
# them for us: branch protection and rulesets are both gated behind
# GitHub Pro on a private repository. The same trade as the CI one —
# while the repo is private everyone pushing to it has this hook, so
# local enforcement covers everybody; when it goes public, server-side
# protection becomes available free and should be turned on, at which
# point this block is belt-and-braces rather than the only belt.
#
# CONTRIBUTING.md: main is the only long-lived branch, and everything
# reaches it through a pull request.
if [ "$KLIPP_ALLOW_MAIN_PUSH" != "1" ]; then
    branch=$(git symbolic-ref --short -q HEAD || echo "")
    if [ "$branch" = "main" ] \
       && git rev-parse --verify --quiet origin/main >/dev/null 2>&1 \
       && [ -n "$(git rev-list origin/main..HEAD 2>/dev/null)" ]; then
        n=$(git rev-list --count origin/main..HEAD)
        printf '\n  pre-push: %s commit(s) on main that are not on origin/main.\n\n' "$n" >&2
        printf '            main takes pull requests, not pushes. Move them onto\n' >&2
        printf '            a branch and open one:\n\n' >&2
        printf '              git switch -c <type>/<what>\n' >&2
        printf '              git switch main && git reset --hard origin/main\n' >&2
        printf '              git switch -\n' >&2
        printf '              git push -u origin HEAD && gh pr create --fill\n\n' >&2
        printf '            KLIPP_ALLOW_MAIN_PUSH=1 git push   if you truly mean it.\n\n' >&2
        exit 1
    fi
fi

# The checks below run against the working tree, because that is what
# cargo reads. So the working tree has to BE the commit being pushed,
# or the gate certifies something that never leaves the machine.
#
# This is not hypothetical: a fix once sat uncommitted on disk while
# the commit it belonged to went out green, and main landed with a
# rustdoc failure that then refused everybody's next push.
if [ "$KLIPP_ALLOW_DIRTY" != "1" ]; then
    if ! git diff --quiet HEAD -- || ! git diff --cached --quiet; then
        printf '\n  pre-push: the working tree is not the commit being pushed.\n\n' >&2
        printf '            clippy, rustdoc and the tests all read what is on\n' >&2
        printf '            disk, so passing them would say nothing about what\n' >&2
        printf '            actually leaves. Changed:\n\n' >&2
        git -c color.ui=never status --short -- . | sed 's/^/              /' >&2
        printf '\n            Commit them, stash them, or KLIPP_ALLOW_DIRTY=1\n' >&2
        printf '            git push if you know the difference does not matter.\n\n' >&2
        exit 1
    fi
fi

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

# Built first and separately, so a compile error is reported as one
# rather than as "tests FAILED" — which sends you reading test names
# for a problem that is not in any of them.
if ! cargo test --workspace --no-run; then
    printf '\n  pre-push: the tests did not BUILD — push refused.\n' >&2
    printf '            Nothing ran. This is a compile or link error, not a\n' >&2
    printf '            failing assertion.\n\n' >&2
    printf '            `undefined hidden symbol: anon.*.llvm.*` in a crate you\n' >&2
    printf '            did not touch is a truncated object from an earlier\n' >&2
    printf '            interrupted build, not your change:\n\n' >&2
    printf '              cargo clean -p <that crate>\n\n' >&2
    exit 1
fi

if cargo test --workspace; then
    printf '\n  pre-push: tests pass.\n\n'
    exit 0
fi

printf '\n  pre-push: tests FAILED — push refused.\n' >&2
printf '            They built, so these are assertions, not compile errors.\n' >&2
printf '            If gpu_pipelines_build is among them, a shader stopped\n' >&2
printf '            compiling and the rest of the GPU suite is skipping\n' >&2
printf '            rather than failing. Fix that one first.\n\n' >&2
exit 1
