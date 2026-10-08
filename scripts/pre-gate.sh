#!/bin/sh
# The gate's checks, run locally before a push, so a gate run is never
# spent finding what a few local minutes would have (#1341).
#
#     scripts/pre-gate.sh
#
# Run it in the branch's own checkout, with everything committed, once
# the branch is rebased onto the newest main. It works out what the
# branch touches exactly as the gate does (scripts/affected_crates.py),
# then runs the gate's checks on it: clippy on the workspace, rustdoc and
# every test target (unit, integration and doc tests) of the crates
# touched and those built on them, and the scripts' own tests when
# scripts/ changed. Unlike the gate, the tests run to the end
# (--no-fail-fast), so one failure doesn't hide the next.
#
# On a pass it stamps the commit's tree, and the pre-push hook refuses a
# push without that stamp. A tree, not a commit: a reworded message
# keeps its stamp, and a rebase that changes the code needs a new run.
#
# A heavy job: it refuses to start while two other cargo jobs run on the
# machine. KLIPP_PRE_GATE_FORCE=1 starts it anyway.

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || { echo "pre-gate: not in a git checkout" >&2; exit 1; }
cd "$ROOT" || exit 1
_common=$(git rev-parse --path-format=absolute --git-common-dir) || exit 1
_stamps="$_common/klipp-pre-gate"
mkdir -p "$_stamps/logs" || exit 1

say() { printf '  pre-gate: %s\n' "$*"; }
refuse() { printf '\n  pre-gate: %s\n\n' "$*" >&2; exit 1; }

# What is checked is what is committed: the stamp names the commit's tree.
if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
    git status --short >&2
    refuse "uncommitted or untracked changes here — commit them (or move them away) first; the stamp is for the commit."
fi

# The branch must hold the newest main, as the squash merge will: a
# conflict, or a test main changed meanwhile, shows here, not in the gate.
if git remote get-url origin >/dev/null 2>&1; then
    timeout 60 git fetch --quiet origin main || refuse "main could not be fetched; nothing was checked."
    _main=$(git rev-parse --verify origin/main) || refuse "no origin/main"
    if ! git merge-base --is-ancestor "$_main" HEAD; then
        _conflicts=$(git merge-tree --name-only --write-tree HEAD "$_main" 2>/dev/null | awk 'NR > 1 && /^$/ { exit } NR > 1')
        [ -n "$_conflicts" ] && printf '%s\n' "$_conflicts" | sed 's/^/              conflict: /' >&2
        refuse "this branch is behind main ($(git rev-parse --short "$_main")). Rebase it, then run this again:

              git fetch origin && git rebase origin/main"
    fi
else
    _main=
fi

if [ "${KLIPP_PRE_GATE_FORCE:-}" != "1" ] && command -v pgrep >/dev/null 2>&1; then
    _jobs=$(for p in $(pgrep -x cargo); do readlink "/proc/$p/cwd"; done 2>/dev/null | grep -vxF "$ROOT" | sort -u)
    if [ "$(printf '%s' "$_jobs" | grep -c .)" -ge 2 ]; then
        printf '%s\n' "$_jobs" | sed 's/^/              running: /' >&2
        refuse "two cargo jobs are already running on this machine. Wait for a free slot (KLIPP_PRE_GATE_FORCE=1 starts anyway)."
    fi
fi

# A new worktree starts from the main checkout's build, copy-on-write, as
# the gate's checkout does; only this tree's crates rebuild. Its own
# target stays its own (a shared CARGO_TARGET_DIR corrupts builds).
_main_checkout=$(dirname "$_common")
if [ -z "${CARGO_TARGET_DIR:-}" ] && [ "$_main_checkout" != "$ROOT" ] \
    && [ ! -d "$ROOT/target/debug" ] && [ -d "$_main_checkout/target/debug" ]; then
    mkdir -p "$ROOT/target"
    if cp -a --reflink=always "$_main_checkout/target/debug" "$ROOT/target/" 2>/dev/null; then
        git ls-files -z | xargs -0 touch -c --
        say "build started from the main checkout's (copy-on-write)"
    else
        rm -rf "$ROOT/target/debug"
    fi
fi

_head=$(git rev-parse HEAD)
_tree=$(git rev-parse "HEAD^{tree}")
_log="$_stamps/logs/$(git rev-parse --short HEAD).log"
: >"$_log"
TMPDIR=${TMPDIR:-$ROOT/target/test-scratch}
mkdir -p "$TMPDIR" && TMPDIR=$(cd "$TMPDIR" && pwd) || refuse "cannot create the scratch directory $TMPDIR"
export TMPDIR
CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3}
export CARGO_BUILD_JOBS

# The scope, worked out as the gate works it out.
_scope_args=--workspace
_scope=full
_scope_why="the scope could not be worked out"
_fork=${_main:+$(git merge-base "$_main" HEAD)}
_changed=
if [ -n "$_fork" ] && _meta=$(cargo metadata --no-deps --format-version 1 --offline 2>/dev/null) \
    && _metafile=$(mktemp) && printf '%s' "$_meta" >"$_metafile" \
    && _changed=$(git diff --name-only "$_fork" HEAD) \
    && _decision=$(printf '%s\n' "$_changed" | python3 "$ROOT/scripts/affected_crates.py" "$_metafile" --git "$_fork" HEAD 2>/dev/null); then
    case "$_decision" in
        packages\ *) _scope=packages; _scope_why="${_decision#packages }"
            _scope_args=$(printf '%s' "${_decision#packages }" | sed 's/[^ ][^ ]*/-p &/g') ;;
        none\ *) _scope=none; _scope_why="${_decision#none }" ;;
        full\ *) _scope_why="${_decision#full }" ;;
    esac
fi
[ -n "${_metafile:-}" ] && rm -f "$_metafile"
say "checking $(git rev-parse --short HEAD) ($_scope: $_scope_why); log: $_log"

step() {
    _name=$1; shift
    say "$_name ..."
    _t=$(date +%s)
    if "$@" >>"$_log" 2>&1; then
        say "$_name passed ($(( $(date +%s) - _t ))s)"
        return 0
    fi
    grep -E '^(error|warning)(\[|:)|^test .* FAILED|panicked at|^failures:|^    [a-z_:]+$' "$_log" | tail -n 30 >&2
    refuse "$_name FAILED — nothing stamped. The whole output is in $_log"
}

# Nothing a test reads changed (documentation, the gate's own scripts):
# no lint can change, but a document can be included into the docs.
[ "$_scope" = none ] || step clippy cargo clippy --workspace --all-targets -- -D warnings
step rustdoc env RUSTDOCFLAGS="-D warnings" cargo doc -j "${KLIPP_GATE_DOC_JOBS:-1}" $([ "$_scope" = none ] && echo --workspace || echo $_scope_args) --no-deps
if [ -d "$ROOT/scripts/tests" ] && { [ "$_scope" = full ] || printf '%s\n' "$_changed" | grep -q '^\(scripts\|packaging\)/'; }; then
    step "the scripts' tests" python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_*.py'
fi
if [ "$_scope" != none ]; then
    # shellcheck disable=SC2086
    step tests cargo test --no-fail-fast $_scope_args
    _passed=$(grep '^test result' "$_log" | awk '{p += $4} END {print p + 0}')
    say "$_passed tests passed"
fi

[ "$(git rev-parse HEAD)" = "$_head" ] && [ -z "$(git status --porcelain --untracked-files=normal)" ] \
    || refuse "the checkout changed while it was checked — nothing stamped. Run it again."
printf 'commit %s\nmain %s\nscope %s %s\nat %s\n' "$_head" "${_main:-none}" "$_scope" "$_scope_why" "$(date +%s)" >"$_stamps/$_tree"
printf '\n  pre-gate: PASSED — %s is stamped; push it to the gate.\n\n' "$(git rev-parse --short HEAD)"
