#!/bin/sh
# The cheap local checks that catch what most often fails a gate run,
# run before a push so the gate isn't spent finding them (#1341).
#
#     scripts/pre-gate.sh
#
# Run it in the branch's own checkout with everything committed. It does
# what a few local minutes can, and leaves the rest to the gate:
#
#   1. a merge check against the newest main (no build): a conflict is
#      refused here, with the files named;
#   2. cargo check --all-targets on the crates the change can affect, as
#      scripts/affected_crates.py works them out for the gate: a broken
#      compile anywhere they reach;
#   3. cargo test (every target: unit, integration, doc) on the crates the
#      change itself touches, to the end (--no-fail-fast), plus the
#      repo-wide source scans (effect_entry_readers, legacy_route_gone)
#      when an effect or grade crate changed;
#   4. clippy -D warnings on the touched crates;
#   5. rustdoc -D warnings on the touched crates (--no-deps, one job as
#      the gate runs it: rustdoc is memory-hungry);
#   6. the scripts' own tests when scripts/ changed.
#
# Workspace clippy, rustdoc of the crates built on the touched ones and
# their tests stay the gate's.
#
# On a pass it stamps the commit's tree, and the pre-push hook refuses a
# push without that stamp. A tree, not a commit: a reworded message keeps
# its stamp; a code change, or a rebase that changes the code, needs a
# new run.
#
# A heavy job: it refuses to start while two other cargo jobs run on the
# machine. KLIPP_PRE_GATE_FORCE="<why>" starts it anyway; the reason is
# printed and kept in skips.log.

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

# 1. The merge with the newest main, as the squash merge will make it.
_main=
if git remote get-url origin >/dev/null 2>&1; then
    timeout 60 git fetch --quiet origin main || refuse "main could not be fetched; nothing was checked."
    _main=$(git rev-parse --verify origin/main) || refuse "no origin/main"
    if ! git merge-base --is-ancestor "$_main" HEAD; then
        _mt=0
        _conflicts=$(git merge-tree --name-only --write-tree HEAD "$_main" 2>/dev/null) || _mt=$?
        if [ "$_mt" -eq 1 ]; then
            printf '%s\n' "$_conflicts" | awk 'NR > 1 && /^$/ { exit } NR > 1' | sed 's/^/              conflict: /' >&2
            refuse "this branch no longer merges onto main ($(git rev-parse --short "$_main")). Rebase it, then run this again:

              git fetch origin && git rebase origin/main"
        fi
        say "behind main ($(git rev-parse --short "$_main")) but merges cleanly; the gate tests the merge"
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

# What the branch changes, against where it left main.
_fork=$(git merge-base "${_main:-HEAD}" HEAD)
_changed=$(git diff --name-only "$_fork" HEAD)

# A file put back to the copy a recent main commit replaced reverts that
# commit, tests and all, and still builds and passes: what a squash onto
# a moved origin/main does. KLIPP_PRE_GATE_ALLOW_REVERT="<why>" lets an
# intended revert through; the reason is kept in skips.log.
_stale_rc=0
_stale=$(printf '%s\n' "$_changed" | python3 "$ROOT/scripts/stale_files.py" "$_fork" HEAD) || _stale_rc=$?
[ "$_stale_rc" -le 1 ] || refuse "the check for reverted main files failed (scripts/stale_files.py, exit $_stale_rc); nothing was checked."
if [ "$_stale_rc" -eq 1 ]; then
    printf '%s\n' "$_stale" | awk -F'\t' '{ printf "              %s is %s'\''s parent copy (%s)\n", $1, $2, $3 }' >&2
    case "${KLIPP_PRE_GATE_ALLOW_REVERT:-}" in
        ''|1|yes|true|' '*)
            refuse "this branch puts files back to the copy before a main commit, reverting it. A squash onto a moved origin/main does this: squash onto the commit you rebased on (git reset --soft \$(git merge-base HEAD origin/main)), or rebase again. If the revert is intended: KLIPP_PRE_GATE_ALLOW_REVERT=\"<why>\" scripts/pre-gate.sh" ;;
        *)
            say "reverting on purpose: $KLIPP_PRE_GATE_ALLOW_REVERT"
            printf '%s %s %s allowed a revert: %s\n' "$(date +%Y-%m-%dT%H:%M:%S%z)" "$(git rev-parse --short HEAD)" \
                "$(git symbolic-ref --short -q HEAD || echo detached)" "$KLIPP_PRE_GATE_ALLOW_REVERT" >>"$_stamps/skips.log" ;;
    esac
fi

# The crates it touches: a changed path inside a crate's folder, named by
# that folder's Cargo.toml.
crates_of() {
    python3 -c '
import sys, tomllib
from pathlib import Path
names = set()
for line in sys.stdin:
    parts = Path(line.strip()).parts
    for depth in range(len(parts) - 1, 0, -1):
        manifest = Path(*parts[:depth]) / "Cargo.toml"
        if manifest.is_file():
            package = tomllib.loads(manifest.read_text()).get("package")
            if package:
                names.add(package["name"])
            break
print(" ".join(sorted(names)))
'
}
_touched=$(printf '%s\n' "$_changed" | crates_of)

# The crates it can affect, as the gate works them out: these are checked
# (compiled), not tested.
_check_args=
_check_why="the crates it touches"
_scope_known=
if _meta=$(cargo metadata --no-deps --format-version 1 --offline 2>/dev/null) \
    && _metafile=$(mktemp) && printf '%s' "$_meta" >"$_metafile" \
    && _decision=$(printf '%s\n' "$_changed" | python3 "$ROOT/scripts/affected_crates.py" "$_metafile" --git "$_fork" HEAD 2>/dev/null); then
    case "$_decision" in
        packages\ *) _scope_known=1; _check_args=$(printf '%s' "${_decision#packages }" | sed 's/[^ ][^ ]*/-p &/g'); _check_why="${_decision#packages }" ;;
        full\ *) _scope_known=1; _check_args=--workspace; _check_why="${_decision#full }" ;;
        none\ *) _scope_known=1; _check_why="${_decision#none }" ;;
    esac
fi
[ -n "${_metafile:-}" ] && rm -f "$_metafile"
# Behind main, a passing run here says nothing about what main changed in
# the crates the gate will test: the gate tests the merge, and a main
# commit in one of those crates can fail it there (a moved test, a new
# reader). Rebase first when main touched a crate in the gate's scope.
if [ -n "$_main" ] && [ "$_fork" != "$_main" ]; then
    _main_crates=$(git diff --name-only "$_fork" "$_main" | crates_of)
    _in_scope=
    # When the gate's scope couldn't be worked out, the gate tests the
    # whole workspace: hold this check to that, never skip it.
    _main_scope=$_check_args
    [ -n "$_scope_known" ] || _main_scope=--workspace
    for _c in $_main_crates; do
        case " $_main_scope " in
            *" --workspace "*|*" -p $_c "*) _in_scope="$_in_scope $_c" ;;
        esac
    done
    [ -z "$_in_scope" ] || refuse "main has moved in crates the gate will test ($_in_scope ); this run would not test what the gate tests. Rebase, then run this again:

              git fetch origin && git rebase origin/main"
fi
_test_args=$(printf '%s' "$_touched" | sed 's/[^ ][^ ]*/-p &/g')
[ -n "$_check_args" ] || _check_args=$_test_args
# The gate runs everything for a change to its own scripts (#873); here
# nothing a crate builds changed, and the scripts' tests below are the check.
if [ -z "$_touched" ] && ! printf '%s\n' "$_changed" | grep -qvE '^(scripts/|packaging/|docs/|\.github/)|\.md$|^$'; then
    _check_args=
fi

# A heavy job only when there is something to build: a change to the
# scripts or the documentation runs no cargo at all.
# Two at a time is the hard limit (more ran the machine out of memory):
# starting a third names its reason, kept beside the push skips.
if [ -n "$_check_args$_test_args" ] && command -v pgrep >/dev/null 2>&1; then
    _jobs=$(for p in $(pgrep -x cargo); do readlink "/proc/$p/cwd"; done 2>/dev/null | grep -vxF "$ROOT" | sort -u)
    if [ "$(printf '%s' "$_jobs" | grep -c .)" -ge 2 ]; then
        printf '%s\n' "$_jobs" | sed 's/^/              running: /' >&2
        case "${KLIPP_PRE_GATE_FORCE:-}" in
            ''|1|yes|true|' '*)
                refuse "two cargo jobs are already running on this machine. Wait for a free slot; to start a third anyway, give the reason: KLIPP_PRE_GATE_FORCE=\"<why>\" scripts/pre-gate.sh" ;;
            *)
                say "starting a THIRD heavy job: $KLIPP_PRE_GATE_FORCE"
                printf '%s %s %s forced a third job: %s\n' "$(date +%Y-%m-%dT%H:%M:%S%z)" "$(git rev-parse --short HEAD)" \
                    "$(git symbolic-ref --short -q HEAD || echo detached)" "$KLIPP_PRE_GATE_FORCE" >>"$_stamps/skips.log" ;;
        esac
    fi
fi

say "checking $(git rev-parse --short HEAD); log: $_log"
say "touched: ${_touched:-no crate}"

step() {
    _name=$1; shift
    say "$_name ..."
    _t=$(date +%s)
    # Stdin closed: a test that spawns a hook reading it would wait forever.
    if "$@" >>"$_log" 2>&1 </dev/null; then
        say "$_name passed ($(( $(date +%s) - _t ))s)"
        return 0
    fi
    grep -E '^(error|warning)(\[|:)|^test .* FAILED|panicked at|^failures:|^    [a-z_:]+$' "$_log" | tail -n 30 >&2
    refuse "$_name FAILED — nothing stamped. The whole output is in $_log"
}

# shellcheck disable=SC2086
if [ -n "$_check_args" ]; then
    step "cargo check ($_check_why)" cargo check --all-targets $_check_args
fi
if [ -n "$_test_args" ]; then
    # The tests the gate will run: the crates this touches, every crate
    # built on them and every crate whose tests use them (affected_crates,
    # the gate's own scope). Testing only the touched crates let a
    # dependent's test fail first in the gate, after its build: three
    # 11-23 minute runs on one evening (#1386, #1406, render_inputs).
    _gate_tests=$_test_args
    [ -n "$_check_args" ] && _gate_tests=$_check_args
    step "tests the gate will run ($_check_why)" cargo test --no-fail-fast $_gate_tests
    # A change in an effect or grade crate can add a reader these scans
    # forbid, in a crate other than the one they live in.
    if printf ' %s ' "$_touched" | grep -qE ' (klipp-effects|klipp-nodes|klipp-graph-format|klipp-graph-lift|klipp-compose|klipp-timeline|klipp-engine|klipp-ui) '; then
        for _scan in klipp-graph-lift:effect_entry_readers klipp-compose:legacy_route_gone; do
            _crate=${_scan%%:*}
            # The complete crate suite above already includes its scans.
            case " $_gate_tests " in *" --workspace "*|*" -p $_crate "*) continue ;; esac
            [ -f "crates/$_crate/tests/${_scan#*:}.rs" ] || continue
            case $_scan in
                klipp-compose:legacy_route_gone)
                    step "the legacy_route_gone scan" cargo test -p klipp-compose --test all legacy_route_gone:: ;;
                *) step "the ${_scan#*:} scan" cargo test -p "$_crate" --test "${_scan#*:}" ;;
            esac
        done
    fi
    # A downstream test reads what other crates produce: a change in one
    # of them passes that crate's own tests and fails only the gate.
    # golden_corpus renders through every crate klipp-compose builds on
    # (#1445's posterize). render_inputs holds the conversion to what the
    # renderer consumes, through the lift and the live eval (#1386, #1406).
    # Each runs when the change touches one of its crate's klipp
    # dependencies (normal or dev) and not the crate itself.
    downstream_test() {
        _crate=$1 _test=$2 _label=$3
        [ -f "crates/$_crate/tests/$_test.rs" ] || return 0
        # Already run above when the gate's scope includes the crate.
        case " $_gate_tests " in *" --workspace "*|*" -p $_crate "*) return 0 ;; esac
        _deps=$(python3 -c '
import sys, tomllib
m = tomllib.load(open(sys.argv[1], "rb"))
deps = {**m.get("dependencies", {}), **m.get("dev-dependencies", {})}
print(" ".join(sorted(k for k in deps if k.startswith("klipp-"))))
' "crates/$_crate/Cargo.toml" 2>/dev/null)
        if [ -z "$_deps" ]; then
            # Fail closed: an unreadable manifest must not skip the check.
            say "$_crate's dependencies could not be read; running $_label anyway"
            step "$_label" cargo test -p "$_crate" --test "$_test"
            return 0
        fi
        for _dep in $_deps; do
            if printf ' %s ' "$_touched" | grep -q " $_dep "; then
                step "$_label" cargo test -p "$_crate" --test "$_test"
                return 0
            fi
        done
    }
    downstream_test klipp-compose golden_corpus "the render references (golden_corpus)"
    downstream_test klipp-convert render_inputs "the conversion's render inputs (render_inputs)"
    step "clippy on $_touched" cargo clippy --all-targets $_test_args -- -D warnings
    # A public doc linking a private item, or a bare [0,1] in prose,
    # passes clippy and every test and fails only the gate's rustdoc.
    step "rustdoc on $_touched" env RUSTDOCFLAGS="-D warnings" cargo doc -j 1 --no-deps $_test_args
    _passed=$(grep '^test result' "$_log" | awk '{p += $4} END {print p + 0}')
    say "$_passed tests passed"
fi
if [ -d "$ROOT/scripts/tests" ] && printf '%s\n' "$_changed" | grep -q '^\(scripts\|packaging\)/'; then
    step "the scripts' tests" env PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_*.py'
fi

[ "$(git rev-parse HEAD)" = "$_head" ] && [ -z "$(git status --porcelain --untracked-files=normal)" ] \
    || refuse "the checkout changed while it was checked — nothing stamped. Run it again."
printf 'commit %s\nmain %s\ntouched %s\nat %s\n' "$_head" "${_main:-none}" "${_touched:-none}" "$(date +%s)" >"$_stamps/$_tree"
printf '\n  pre-gate: PASSED — %s is stamped; push it to the gate.\n\n' "$(git rev-parse --short HEAD)"
