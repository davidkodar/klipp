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
#
# A push also needs the commit to have passed scripts/pre-gate.sh, the
# cheap local checks run first (#1341). To push without it, give the
# reason, which the push's log keeps:
#
#     KLIPP_SKIP_PRE_GATE="<why>" git push
#
# The repository's owner turns the whole gate off, and on again, for
# every push from this clone and its worktrees:
#
#     git config klipp.gate off
#     git config klipp.gate on      (or unset: the default)
#
# Off, a push runs none of these checks and says so.

set -e

ROOT=$(git rev-parse --show-toplevel 2>/dev/null) || exit 0
cd "$ROOT" || exit 0

case "$(git config --get klipp.gate 2>/dev/null)" in
    off|false|no|0)
        printf '\n  pre-push: the gate is off (git config klipp.gate off) — checks NOT run.\n' >&2
        printf '            git config klipp.gate on   turns it back on.\n\n' >&2
        exit 0 ;;
esac

if [ "$KLIPP_SKIP_TESTS" = "1" ]; then
    printf '\n  pre-push: KLIPP_SKIP_TESTS=1 set — checks NOT run for this push.\n\n' >&2
    exit 0
fi

# A push that only deletes refs carries no code to check. Git hands the
# hook one line per ref, "<local ref> <local sha> <remote ref> <remote
# sha>", and a deletion's local sha is all zeros. Without this a
# `git push origin --delete <branch>` queued a whole gate behind every
# other session's (2026-09-30). Run by hand from a terminal there is no
# list, and the checks run as always.
if [ ! -t 0 ]; then
    _refs=$(cat)
    if [ -n "$_refs" ] && ! printf '%s\n' "$_refs" | awk '$2 !~ /^0+$/ { code = 1 } END { exit !code }'; then
        printf '\n  pre-push: only deleting refs, nothing to check.\n\n' >&2
        exit 0
    fi
    # Every commit pushed has to be the one checked out here (#1118). The
    # gate builds and tests HEAD, so a branch pushed from a checkout that
    # has another one out (`git push origin <other-branch>`) was never
    # built: the gate tested this checkout's own branch, found nothing in
    # it to test, and let the push through. Deletions and tags carry no
    # code of their own. What is updated is the remote ref (the third
    # field), so that is what decides a tag: a tag pushed onto a branch
    # still brings its commit into that branch (#1120).
    _head=$(git rev-parse HEAD 2>/dev/null)
    _other=$(printf '%s\n' "$_refs" | awk -v head="$_head" '$2 !~ /^0+$/ && $3 !~ /^refs\/tags\// && $2 != head { print $1; exit }')
    if [ -n "$_other" ]; then
        _where=$(git worktree list --porcelain 2>/dev/null | awk -v ref="$_other" '/^worktree / { w = substr($0, 10) } $0 == "branch " ref { print w; exit }')
        printf '\n  pre-push: %s is not the commit checked out here (%s), and the gate\n' "$_other" "$(git rev-parse --short HEAD)" >&2
        printf '            tests what is checked out, so it would go up untested (#1118).\n' >&2
        if [ -n "$_where" ]; then
            printf '            Push it from %s, where it is checked out.\n\n' "$_where" >&2
        else
            printf '            Check it out, or add a worktree for it, and push from there.\n\n' >&2
        fi
        exit 1
    fi
    # And it passed the cheap local checks first (scripts/pre-gate.sh,
    # #1341): the gate is for what a local run can't show, not for a
    # forgotten integration test, a rebase that no longer compiles or a
    # conflict with main, which cost the whole queue a run each. The
    # stamp is the commit's tree, so only a code change needs a new run.
    # A skip names its reason, which this push's log and skips.log keep.
    # Only a line that names a commit counts: run by hand there is no
    # list, and an empty line is not a push.
    if printf '%s\n' "$_refs" | awk 'NF >= 4 && $2 !~ /^0+$/ && $3 !~ /^refs\/tags\// { code = 1 } END { exit !code }'; then
        # Assigned apart: under set -e a failing git inside the assignment
        # would end the gate without a word.
        _stamps=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || _stamps=
        _stamps="$_stamps/klipp-pre-gate"
        # Each pushed commit's own tree, not HEAD's: a push can name a
        # commit other than the one checked out.
        _unstamped=$(printf '%s\n' "$_refs" | while read -r _ref _sha _remote _remote_sha; do
            [ -n "$_sha" ] || continue
            case "$_sha" in *[!0]*) ;; *) continue ;; esac
            case "$_remote" in refs/tags/*) continue ;; esac
            _tree=$(git rev-parse --verify --quiet "$_sha^{tree}" 2>/dev/null) || _tree=
            [ -n "$_tree" ] && [ -f "$_stamps/$_tree" ] || printf '%s\n' "$_sha"
        done; true)
        case "${KLIPP_SKIP_PRE_GATE:-}" in
            '') if [ -n "$_unstamped" ]; then
                    printf '\n  pre-push: %s has not passed scripts/pre-gate.sh — push refused.\n' "$(git rev-parse --short "$(printf '%s\n' "$_unstamped" | head -n 1)" 2>/dev/null)" >&2
                    printf '            Run it here first (minutes; a failed gate run costs everyone\n' >&2
                    printf '            queued behind it a whole run), then push again:\n\n' >&2
                    printf '              scripts/pre-gate.sh\n\n' >&2
                    printf '            To push without it, give the reason:\n' >&2
                    printf '              KLIPP_SKIP_PRE_GATE="<why>" git push\n\n' >&2
                    exit 1
                fi ;;
            1|yes|true|' '*)
                printf '\n  pre-push: KLIPP_SKIP_PRE_GATE needs the reason, not a flag — push refused:\n' >&2
                printf '              KLIPP_SKIP_PRE_GATE="<why>" git push\n\n' >&2
                exit 1 ;;
            *)
                printf '\n  pre-push: pushed WITHOUT scripts/pre-gate.sh: %s\n\n' "$KLIPP_SKIP_PRE_GATE" >&2
                mkdir -p "$_stamps" 2>/dev/null && printf '%s %s %s %s\n' "$(date +%Y-%m-%dT%H:%M:%S%z)" \
                    "$(git rev-parse --short HEAD 2>/dev/null)" "$(git symbolic-ref --short -q HEAD || echo detached)" \
                    "$KLIPP_SKIP_PRE_GATE" >>"$_stamps/skips.log" ;;
        esac
    fi
fi

# Keep the initiating checkout and commit even when the gate changes ROOT
# to its build worktree. Recheck after waiting and before success (#1163).
_push_root=$ROOT
_push_head=$(git rev-parse --verify HEAD)
_check_push_refs() (
    unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR
    cd "$_push_root" || exit 1
    _now=$(git rev-parse --verify HEAD 2>/dev/null) || _now=
    if [ "$_now" != "$_push_head" ]; then
        printf '\n  pre-push: HEAD moved during the gate — push refused. Push again to test the new commit.\n\n' >&2
        exit 1
    fi
    while read -r _ref _sha _remote _remote_sha; do
        [ -n "$_sha" ] || continue
        case "$_sha" in *[!0]*) ;; *) continue ;; esac
        _now=$(git rev-parse --verify --end-of-options "$_ref" 2>/dev/null) || _now=
        if [ "$_now" != "$_sha" ]; then
            printf '\n  pre-push: %s moved during the gate — push refused. Push again to test the new commit.\n\n' "$_ref" >&2
            exit 1
        fi
    done <<EOF
${_refs:-}
EOF
)

if ! command -v cargo >/dev/null 2>&1; then
    printf '\n  pre-push: cargo not on PATH; cannot run the tests. Refusing rather\n' >&2
    printf '            than letting an unchecked push through silently. Set\n' >&2
    printf '            KLIPP_SKIP_TESTS=1 if this is deliberate.\n\n' >&2
    exit 1
fi

# rustdoc can use far more memory per crate than rustc/link jobs. Keep
# its concurrency separate from the build's CARGO_BUILD_JOBS (#1240).
_doc_jobs=${KLIPP_GATE_DOC_JOBS:-1}
# Reject non-digits before the numeric comparison.
case $_doc_jobs in
    *[!0-9]*) _doc_jobs=0 ;;
esac
if ! [ "$_doc_jobs" -gt 0 ] 2>/dev/null; then
    printf '\n  pre-push: KLIPP_GATE_DOC_JOBS must be a positive integer.\n' >&2
    exit 1
fi

# Cargo defaults to one job per core, which on a many-core machine is
# enough concurrent rustc and LLVM to exhaust memory when several large
# crates link at once. The OOM killer then takes rustc mid-write and
# leaves a truncated object behind, which surfaces later as
# `undefined hidden symbol: anon.*.llvm.*` in some crate nobody
# touched — a failure that looks like anything except what it is.
#
# Half the cores and what free memory allows, at least two
# (scripts/gate-jobs.sh); worked out again when the gate is taken, since a
# push can wait an hour in the queue. Respected if already set.
_jobs_given=${CARGO_BUILD_JOBS:+yes}
if [ -z "$CARGO_BUILD_JOBS" ]; then
    CARGO_BUILD_JOBS=$(sh "$ROOT/scripts/gate-jobs.sh" 2>/dev/null || echo 2)
    export CARGO_BUILD_JOBS
fi

# A running Klipp competes with the GPU tests for the device.
#
# Several tests build their own wgpu device and render with it. With
# the application running they contend, and what comes back is a
# handful of failed assertions in the GPU suite with nothing pointing
# at the cause — the same push passes once the app is closed. That has
# happened three times in one session; each time it cost a detour
# through "which test, and what did I break".
#
# Said before the tests run rather than after they fail, because the
# point is to recognise the failure when it arrives.
if command -v pgrep >/dev/null 2>&1 && pgrep -x klipp-app >/dev/null 2>&1; then
    printf '\n  pre-push: Klipp is running.\n' >&2
    printf '            The GPU tests build their own device and will\n' >&2
    printf '            contend with it. If the suite fails somewhere in\n' >&2
    printf '            the GPU tests, close Klipp and push again before\n' >&2
    printf '            looking for the cause in your change.\n\n' >&2
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

# One gate at a time, across every worktree and every session, and in
# the order the sessions arrived.
#
# Several agents work on this repository at once, each in its own
# worktree, and each push runs the whole gate. Four of them together
# exhaust memory: rustc is OOM-killed, background jobs are reaped, and
# the timing-sensitive engine tests fail with nothing wrong in the
# change. So the gate queues on a lock in the git common directory,
# which every worktree of this clone shares.
#
# That lock alone was not enough. flock promises nothing about who gets
# it next, so with four sessions pushing the kernel can hand it to
# whoever happens to ask first and pass the same session over again and
# again. On screen being starved looks exactly like waiting — one line,
# then silence, no position and no cap — and one push waited forty
# minutes that way. So the turn is now taken explicitly:
#
#   * each session appends one line — pid, branch, worktree, arrival
#     time — to klipp-pre-push.queue, under a second lock
#     (klipp-pre-push.queue.lock) held for that append or read and never
#     across the gate itself, so joining the queue is never a wait;
#   * it waits until its own pid is at the head of the queue AND the
#     gate lock is free, then takes the gate, leaving its line in place
#     so the sessions behind it can name what is running;
#   * it drops its line on the way out, in a trap that covers Ctrl-C and
#     a kill as well as a normal exit, and every read of the queue also
#     drops entries whose pid is gone, so a waiter killed with -9 cannot
#     wedge the line;
#   * while it waits it prints its position and who is ahead, refreshed
#     as the queue moves, so a long wait can be told from a lost one.
#
# The queue decides the ORDER; the lock still decides who runs. The head
# of the queue takes the gate only once it also holds the lock, because
# not every gate has a ticket: a branch from before #432 carries a copy
# of this script with no queue in it, and the installed hook runs that
# one under `flock -o` on the same lock. Such a gate cannot be given a
# position — it knows nothing about the queue — so it is waited for as
# what it is, a gate outside the queue, named from the holder file it
# writes just as this one does.
#
# The gate lock is held on a file descriptor, so a killed gate releases
# it and cannot wedge the queue. That holds only while nothing else has
# the descriptor: every child inherits it, and a cargo orphaned by a
# killed hook went on holding the lock for every session. So the
# long-running children below are started with fd 9 closed (`9>&-`).
# It covers the checks, not the network push that follows.
# KLIPP_GATE_NO_WAIT=1 refuses instead of queueing or waiting.
#
# The name is deliberately not klipp-gate.lock, the file sessions wrapped
# `git push` in by hand before this existed: a hook waiting on a lock its
# own parent holds would wait forever.

# Is that pid still there? `kill -0` also fails with EPERM, for a live
# process owned by somebody else, and reading that as "gone" would drop
# a live waiter and let this session jump the line — so where /proc can
# tell the two apart, ask it. A pid may have been reused by the time we
# look, which here only means an entry outlives its waiter by one poll.
_alive() {
    if kill -0 "$1" 2>/dev/null; then
        return 0
    fi
    [ -d "/proc/$1" ]
}

_ordinal() {
    case "$1" in
        11|12|13) printf '%sth' "$1" ;;
        *1) printf '%sst' "$1" ;;
        *2) printf '%snd' "$1" ;;
        *3) printf '%srd' "$1" ;;
        *) printf '%sth' "$1" ;;
    esac
}

# "<branch> on <worktree>" out of a queue line, which is
# "<pid> <branch> <worktree> <arrival>". A branch name cannot contain a
# space and a worktree path can, so the path is whatever is left in the
# middle.
_where() {
    _rest=${1#* }
    _br=${_rest%% *}
    _wt=${_rest#* }
    printf '%s on %s' "$_br" "${_wt% *}"
}

# Read the queue under the meta lock, dropping entries whose pid is
# gone, write it back and print it. The action says what to do with this
# session's own entry: `keep` (the default) gives it exactly one, at the
# back if it had none; `remove` drops it; `peek` neither adds nor drops.
_queue_sync() {
    _action=${1:-keep}
    (
        flock 8 2>/dev/null || exit 1
        _out=
        _mine=0
        if [ -f "$_queue" ]; then
            while IFS= read -r _line || [ -n "$_line" ]; do
                _pid=${_line%% *}
                case "$_pid" in
                    ''|*[!0-9]*) continue ;;
                esac
                if [ "$_pid" = "$$" ]; then
                    if [ "$_action" = remove ]; then
                        continue
                    fi
                    _mine=1
                elif ! _alive "$_pid"; then
                    continue
                fi
                _out=$_out$_line'
'
            done < "$_queue"
        fi
        if [ "$_action" = keep ] && [ "$_mine" -eq 0 ]; then
            _out=$_out$_entry'
'
        fi
        printf '%s' "$_out" >"$_queue" || exit 1
        printf '%s' "$_out"
    ) 8>>"$_qlock"
}

# Leave the queue, whatever ends this process, without touching the exit
# status the gate meant to report.
_leave_queue() {
    _status=$?
    if [ -n "$_queue" ]; then
        _queue_sync remove >/dev/null 2>&1 || true
    fi
    return "$_status"
}

# What the holder file says about whoever holds the gate lock, or nothing
# if it says nothing useful. It is a record left by the last gate to take
# the lock, queued or not, and it is never cleaned up, so it is reported
# as a record: if the pid in it is gone, that is said rather than dressed
# up as a conclusion about who holds the lock now.
_describe_holder() {
    _who=$(cat "$_holder" 2>/dev/null || true)
    if [ -z "$_who" ]; then
        return 0
    fi
    _hpid=${_who%% *}
    _since=$(printf '%s\n' "$_who" | awk '{print $NF}')
    case "$_since" in
        ''|*[!0-9]*) ;;
        *) _who="${_who% *}, held for $(( ($(date +%s) - _since) / 60 )) min" ;;
    esac
    if [ -n "$_hpid" ] && ! _alive "$_hpid"; then
        _who="$_who (that pid is gone, so the record may be stale)"
    fi
    printf '%s' "$_who"
}

_queue=
_common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || _common=
if [ -n "$_common" ] && [ -d "$_common" ] && command -v flock >/dev/null 2>&1; then
    _lock="$_common/klipp-pre-push.lock"
    _holder="$_common/klipp-pre-push.holder"
    _queue="$_common/klipp-pre-push.queue"
    _qlock="$_common/klipp-pre-push.queue.lock"
    _branch=$(git symbolic-ref --short -q HEAD || git rev-parse --short HEAD) || _branch=
    _entry="$$ ${_branch:-?} $ROOT $(date +%s)"
    exec 9>>"$_lock"

    if [ "$KLIPP_GATE_NO_WAIT" = "1" ]; then
        # Refuse on the spot rather than join the queue, and leave no
        # entry behind: not even a place in the line is taken. Queued
        # sessions count, because taking a free lock ahead of them is
        # exactly the queue-jumping this file is here to stop.
        _waiting=$(_queue_sync peek | awk 'END {print NR}')
        if ! flock -n 9; then
            _who=$(_describe_holder)
            printf '\n  pre-push: another gate is running: %s\n' "${_who:-unknown}" >&2
            if [ "${_waiting:-0}" -gt 0 ]; then
                printf '            %s session(s) queued behind it.\n' "$_waiting" >&2
            fi
            printf '            KLIPP_GATE_NO_WAIT=1 set — refusing instead of waiting.\n\n' >&2
            exit 1
        fi
        if [ "${_waiting:-0}" -gt 0 ]; then
            printf '\n  pre-push: %s session(s) are already queued for the gate.\n' "$_waiting" >&2
            printf '            KLIPP_GATE_NO_WAIT=1 set — refusing instead of taking\n' >&2
            printf '            the gate ahead of them.\n\n' >&2
            exit 1
        fi
    else
        trap '_leave_queue' EXIT
        trap '_leave_queue; exit 130' INT
        trap '_leave_queue; exit 143' TERM
        _waited=0
        _shown=
        _joined=$(date +%s)
        while :; do
            _q=$(_queue_sync) || _q=
            _pos=0
            _n=0
            _prev=
            _ahead=
            while IFS= read -r _line; do
                if [ -z "$_line" ]; then
                    continue
                fi
                _n=$(( _n + 1 ))
                if [ "$_pos" -eq 0 ] && [ "${_line%% *}" = "$$" ]; then
                    _pos=$_n
                    _ahead=$_prev
                fi
                _prev=$_line
            done <<QUEUE
$_q
QUEUE
            if [ "$_pos" -eq 0 ]; then
                # The queue could not be read or written. Better an
                # unfair gate than no gate: wait on the lock alone, as
                # this hook did before the queue existed.
                printf '\n  pre-push: cannot keep a place in %s;\n' "$_queue" >&2
                printf '            waiting on the gate lock alone, out of turn.\n\n' >&2
                flock 9
                break
            fi
            if [ "$_pos" -eq 1 ] && flock -n 9; then
                if [ "$_waited" -eq 1 ]; then
                    printf '            got the gate after %s s of waiting.\n\n' \
                        "$(( $(date +%s) - _joined ))" >&2
                fi
                break
            fi
            if [ "$_pos" -eq 1 ]; then
                # Nothing ahead of us in the queue, so whatever holds the
                # lock holds no ticket: a pre-#432 branch wrapped by the
                # installed hook, a KLIPP_GATE_NO_WAIT gate, or a child
                # outliving a gate that was killed.
                _who=$(_describe_holder)
                _msg="1st in the queue, waiting for a gate outside the queue"
                if [ -n "$_who" ]; then
                    _msg="$_msg: $_who"
                else
                    _msg="$_msg (nothing recorded about it)"
                fi
            else
                _msg="$(_ordinal "$_pos") in the queue, behind $(_where "$_ahead")"
            fi
            if [ "$_waited" -eq 0 ]; then
                _waited=1
                printf '\n  pre-push: waiting for the gate. One gate runs at a time, in the\n' >&2
                printf '            order sessions arrived. Ctrl-C to stop waiting;\n' >&2
                printf '            KLIPP_GATE_NO_WAIT=1 git push   refuses instead of waiting,\n' >&2
                printf '            KLIPP_SKIP_TESTS=1 git push     skips the gate and the queue.\n\n' >&2
            fi
            if [ "$_msg" != "$_shown" ]; then
                printf '            %s\n' "$_msg" >&2
                _shown=$_msg
            fi
            sleep 1
        done
    fi
    printf '%s %s %s %s\n' "$$" "${_branch:-?}" "$ROOT" "$(date +%s)" >"$_holder"
fi

# The jobs for what memory is free now, the gate being ours.
if [ -z "$_jobs_given" ] && [ -f "$ROOT/scripts/gate-jobs.sh" ]; then
    CARGO_BUILD_JOBS=$(sh "$ROOT/scripts/gate-jobs.sh" 2>/dev/null || echo "$CARGO_BUILD_JOBS")
    export CARGO_BUILD_JOBS
    printf '  pre-push: building with %s jobs (half the cores, within free memory).\n' "$CARGO_BUILD_JOBS" >&2
fi

# A line per gate that ran, kept beside the queue, so whoever watches it
# can see what passed and what failed after the queue has moved on, and
# how long a gate takes: "start end result step branch worktree log",
# result pass, fail or stopped, step where it ended, log its output (- if
# none was kept). The last 200 are kept.
_started=$(date +%s)
_step=setup
# And its output, so a failure can be read after the push that hit it has
# gone: the checks' output is copied to a log beside the history while
# it still goes to the terminal; the last 50 logs are kept.
_log=
if [ -n "$_queue" ] && mkdir -p "$_common/klipp-pre-push.logs" 2>/dev/null; then
    _log="$_common/klipp-pre-push.logs/$_started-$$.log"
    : >"$_log" 2>/dev/null || _log=
    # Oldest first by name (the start time leads it), all but the last 50.
    ls "$_common/klipp-pre-push.logs" 2>/dev/null | sort | head -n -50 | while read -r _old; do
        rm -f "$_common/klipp-pre-push.logs/$_old"
    done
fi
# A check's output, to the terminal and to the log; its exit status is
# the check's, not tee's.
_logged() {
    if [ -z "$_log" ]; then
        "$@"
        return
    fi
    # Through tee cargo sees no terminal and drops its colours; keep them
    # for a push run from one.
    [ -t 1 ] && [ -z "${CARGO_TERM_COLOR:-}" ] && CARGO_TERM_COLOR=always && export CARGO_TERM_COLOR
    { "$@" 2>&1; echo $? >"$_log.status"; } | tee -a "$_log"
    _rc=$(cat "$_log.status" 2>/dev/null || echo 1)
    rm -f "$_log.status"
    return "$_rc"
}
_record() {
    _status=$1
    [ -n "$_queue" ] || return 0
    case "$_status" in
        0) _result=pass ;;
        130|143) _result=stopped ;;
        *) _result=fail ;;
    esac
    _history="$_common/klipp-pre-push.history"
    # An eighth field marks a push that skipped the advisory check (#928).
    printf '%s %s %s %s %s %s %s%s\n' "$_started" "$(date +%s)" "$_result" "$_step" "${_branch:-?}" "${_pushed_from:-$ROOT}" "${_log:--}" "${_audit:+ $_audit}" >>"$_history" 2>/dev/null || return 0
    if [ "$(wc -l <"$_history" 2>/dev/null || echo 0)" -gt 400 ]; then
        tail -n 200 "$_history" >"$_history.new" 2>/dev/null && mv -f "$_history.new" "$_history" 2>/dev/null || true
    fi
    return 0
}
# The branch put back as it was pushed, if the gate tested it merged with
# main (below). The merge never commits, so aborting it is all it takes.
_unmerge() {
    [ -n "${_merged_from:-}" ] || return 0
    git merge --abort >/dev/null 2>&1 || git reset --hard --quiet "$_merged_from" >/dev/null 2>&1 || true
    _merged_from=
}
trap '_s=$?; _unmerge; _record "$_s"; _leave_queue; exit "$_s"' EXIT

# Where the gate builds (#871). It built in the worktree being pushed,
# and every worktree started cold: a copy of the main checkout's build
# with all its files touched (below), so all ~40 crates recompiled even
# for a one-line change, and each worktree grew its own ~90 GB build. Now
# it builds in one checkout of its own beside the main one, used only
# under the gate lock: each run checks the pushed commit out there, git
# rewrites only the files that differ, and cargo rebuilds only their
# crates and what depends on them. That is cargo's own model, one tree
# and one build folder, so nothing is borrowed from another tree after
# its first build, which starts from the main checkout's as a worktree's
# did. The pushing worktree is left as it is. When the checkout cannot be
# made or used, or KLIPP_ALLOW_DIRTY asks for the files on disk to be
# tested, or a CARGO_TARGET_DIR is set, the gate builds in place as
# before, and says so. KLIPP_GATE_IN_PLACE=1 asks for that too, and
# KLIPP_GATE_DIR moves the checkout.
_pushed_from=$ROOT
_check_push_refs
if [ -n "$_queue" ] && [ -z "${CARGO_TARGET_DIR:-}" ] && [ "${KLIPP_GATE_IN_PLACE:-}" != "1" ] && [ "$KLIPP_ALLOW_DIRTY" != "1" ]; then
    _step=checkout
    _main=$(dirname "$_common")
    _gate_dir=${KLIPP_GATE_DIR:-$(dirname "$_main")/$(basename "$_main")-gate}
    _pushed=$(git rev-parse HEAD)
    # A hook may be run with these set for the pushing tree; the gate
    # checkout is another tree.
    unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE
    if [ ! -e "$_gate_dir/.git" ]; then
        git worktree prune >/dev/null 2>&1 || true
        if git worktree add --quiet --detach "$_gate_dir" "$_pushed" >/dev/null 2>&1; then
            printf '  pre-push: made the gate checkout %s; its first build is a full one\n' "$_gate_dir"
        fi
    fi
    if [ -e "$_gate_dir/.git" ] && (
        cd "$_gate_dir" &&
        { git merge --abort >/dev/null 2>&1 || true; } &&
        # Files whose times moved but not their content (the first build's
        # touch, below) are matched by content here; otherwise the reset
        # rewrites every one of them and every run rebuilds everything.
        { git update-index -q --refresh >/dev/null 2>&1 || true; } &&
        git reset --quiet --hard &&
        git checkout --quiet --detach --force "$_pushed" &&
        git clean -fdq
    ) >/dev/null 2>&1; then
        ROOT=$_gate_dir
        cd "$ROOT"
        printf '  pre-push: building %s in the gate checkout %s; only what changed rebuilds\n' "$(git rev-parse --short HEAD)" "$ROOT"
        # Its build keeps every crate version it has made, so it grows. Once
        # a day the space it holds of its own is looked at, and past
        # KLIPP_GATE_MAX_GB (250) it is let go, to start again from the
        # main checkout's below. Its own: it began as a copy-on-write copy
        # of the main checkout's, whose blocks it shares, and du counts
        # those in full (608 GB for a build holding 79 GB of its own, which
        # let it go every day). btrfs says what is exclusive; without it
        # nothing is let go.
        _size_mark="$_common/klipp-pre-push.gate-size"
        if command -v btrfs >/dev/null 2>&1 && { [ ! -e "$_size_mark" ] || [ -n "$(find "$_size_mark" -mmin +1440 2>/dev/null)" ]; }; then
            touch "$_size_mark" 2>/dev/null || true
            _gb=$(timeout 300 btrfs filesystem du -s --raw "$ROOT/target" 2>/dev/null | awk 'NR == 2 { printf "%d", $2 / 1073741824 }')
            case "$_gb" in ''|*[!0-9]*) _gb=0 ;; esac
            if [ "$_gb" -gt "${KLIPP_GATE_MAX_GB:-250}" ]; then
                rm -rf "$ROOT/target"
                printf '  pre-push: the gate checkout'"'"'s build held %s GB of its own; it starts again from the main checkout'"'"'s\n' "$_gb"
            fi
        fi
    else
        printf '  pre-push: the gate checkout %s could not be used; building here, as before\n' "$_gate_dir" >&2
    fi
fi

# What the merge will be, not the branch alone (#866). GitHub squashes a
# branch onto main as main is when it merges, so a branch based on an older
# main was tested on code that never ships, and one that could no longer
# merge cleanly still built and ran everything before anybody found out:
# three branches went through the gate three times in a night that way.
# So when the gate's turn comes, main is fetched. A branch that cannot
# merge onto it is refused here, before anything is built. One that is
# behind it is tested merged with it (`git merge --no-commit`, so nothing
# is committed and the push still sends the branch as it is), and put back
# by the exit trap whatever ends the gate. A branch already on main, a
# fetch that fails, or a tree that cannot take the merge (KLIPP_ALLOW_DIRTY,
# untracked files in the way) is tested as it is, as before, and said.
# Git writes each file it changes as a new file, so this script, running,
# is never rewritten under itself.
_merged_from=
if [ -n "$_queue" ] && [ "${KLIPP_GATE_AS_IS:-}" != "1" ] && git remote get-url origin >/dev/null 2>&1; then
    _step=merge
    if timeout 60 git fetch --quiet origin main 9>&- 2>/dev/null && _main_head=$(git rev-parse --verify --quiet origin/main); then
        if ! git merge-base --is-ancestor "$_main_head" HEAD 2>/dev/null; then
            # merge-tree says 1 for a conflict; anything else is git failing
            # to answer (an older git without --write-tree), which must not
            # refuse every push, so the branch is then tested as it is.
            _mt_status=0
            git merge-tree --write-tree HEAD "$_main_head" >/dev/null 2>&1 || _mt_status=$?
            if [ "$_mt_status" -ne 0 ] && [ "$_mt_status" -ne 1 ]; then
                printf '  pre-push: git could not say whether %s merges onto main (merge-tree exit %s); testing the branch as it is.\n' "${_branch:-this branch}" "$_mt_status" >&2
            elif [ "$_mt_status" -eq 1 ]; then
                printf '\n  pre-push: this branch no longer merges cleanly onto main — push refused.\n' >&2
                printf '            Nothing was built. Rebase it and push again:\n\n' >&2
                printf '              git fetch origin && git rebase origin/main\n\n' >&2
                # The files in conflict: the lines after the tree id, up to the blank line.
                git merge-tree --name-only --write-tree HEAD "$_main_head" 2>/dev/null | awk 'NR > 1 && /^$/ { exit } NR > 1 { print "              conflict: " $0 }' >&2 || true
                exit 1
            elif [ "$KLIPP_ALLOW_DIRTY" != "1" ] && _pushed=$(git rev-parse HEAD) && git merge --no-commit --no-ff --quiet "$_main_head" >/dev/null 2>&1; then
                _merged_from=$_pushed
                printf '  pre-push: %s is behind main; testing it merged with main (%s), as the squash merge will be. The branch is put back after.\n' \
                    "${_branch:-this branch}" "$(git rev-parse --short "$_main_head")"
            else
                git merge --abort >/dev/null 2>&1 || true
                printf '  pre-push: %s is behind main, and the merge could not be laid out here; testing the branch as it is.\n' "${_branch:-this branch}" >&2
            fi
        fi
    else
        printf '  pre-push: main could not be fetched; testing the branch as it is.\n' >&2
    fi
fi

# A worktree's first gate starts from the main checkout's build, not an
# empty target/. Each push built the whole workspace from nothing, all
# cores for half an hour and some 90 GB written, and a day of pushes from
# three sessions left fourteen such folders and the machine unusable.
# The copy is copy-on-write (reflink), so it costs next to no disk until
# something is rebuilt. Sharing one target/ across worktrees instead is
# not sound: cargo names a workspace crate by its path within the
# workspace, the same in every worktree, and judges it fresh by file
# times, so one tree's build linked another tree's crates. Touching
# every tracked file here makes all of this tree's own crates rebuild
# from its own sources; only the dependencies come from the copy. A
# deliberate CARGO_TARGET_DIR still wins, and without reflink support
# the target starts empty as before. The gate checkout's first build
# (above) starts this way too, once.
if [ -z "${CARGO_TARGET_DIR:-}" ] && [ -n "$_queue" ]; then
    _main=$(dirname "$_common")
    if [ "$_main" != "$ROOT" ] && [ ! -d "$ROOT/target/debug" ] && [ -d "$_main/target/debug" ]; then
        mkdir -p "$ROOT/target"
        if cp -a --reflink=always "$_main/target/debug" "$ROOT/target/" 2>/dev/null; then
            git ls-files -z | xargs -0 touch -c --
            printf '  pre-push: build started from the main checkout'"'"'s (copy-on-write); this tree'"'"'s crates rebuild\n'
        else
            rm -rf "$ROOT/target/debug"
        fi
    fi
fi

# The gate yields to whoever is at the machine: the build takes every
# core and writes gigabytes, and at normal priority it made the desktop
# and the app crawl for the half hour it ran. At the lowest CPU priority
# and the lowest best-effort disk priority it uses what is spare; cargo,
# the compilers and the tests inherit both.
renice -n 19 -p $$ >/dev/null 2>&1 || true
ionice -c 2 -n 7 -p $$ >/dev/null 2>&1 || true

# Match klipp_testing::scratch, and keep compiler/linker temporary files
# off a small system tmpfs too. A deliberate TMPDIR always wins.
TMPDIR=${TMPDIR:-${CARGO_TARGET_DIR:-$ROOT/target}/test-scratch}
if ! mkdir -p "$TMPDIR"; then
    printf '\n  pre-push: cannot create scratch directory %s — push refused.\n' "$TMPDIR" >&2
    exit 1
fi
# Relative CARGO_TARGET_DIR/TMPDIR paths are relative to this workspace;
# child tools may change directory, so pass them an absolute location.
TMPDIR=$(cd "$TMPDIR" && pwd)
export TMPDIR
printf '  pre-push: scratch directory: %s\n' "$TMPDIR"

scratch_space() {
    df -Pk "$TMPDIR" >&2 || true
}
# This is headroom for generated media, not a guarantee: quotas and
# simultaneous builds can exhaust storage even when df reports room.
_available=$(df -Pk "$TMPDIR" 2>/dev/null | awk 'NR == 2 {print $4}')
case "$_available" in
    ''|*[!0-9]*) ;;
    *) if [ "$_available" -lt 2097152 ]; then
        printf '\n  pre-push: scratch has less than 2 GiB free — push refused.\n' >&2
        scratch_space
        printf '            Free space there or set TMPDIR to a disk-backed directory.\n\n' >&2
        exit 1
    fi ;;
esac

# Advisories in the dependencies (#825, #841, #928). One that deny.toml
# does not record refuses the push until it is fixed, or recorded there
# with its reason; the ones recorded pass. An entry in deny.toml that no
# longer matches anything is pointed at, to be dropped.
#
# The check has to run for the push to go: without cargo-deny, or when it
# cannot finish, the push is refused, with how to install it. The list of
# advisories is refreshed first; offline, the copy on disk is used, said
# when it is over a week old, and refused once it is over 30 days old,
# since an old list passes what a current one would not. A machine that
# cannot check sets KLIPP_SKIP_AUDIT=1: the push goes on, says so, and its
# line in the gate history is marked `unaudited` for everyone to see.
_step=advisories
_audit=
_audit_refused() {
    printf '            KLIPP_SKIP_AUDIT=1 git push   pushes without the check; the gate\n' >&2
    printf '            history marks it unaudited.\n\n' >&2
    exit 1
}
if [ "${KLIPP_SKIP_AUDIT:-}" = "1" ]; then
    printf '\n  pre-push: KLIPP_SKIP_AUDIT=1: the dependencies are not checked for advisories;\n' >&2
    printf '            this push is marked unaudited in the gate history.\n\n' >&2
    _audit=unaudited
elif command -v cargo-deny >/dev/null 2>&1; then
    if ! timeout 60 cargo deny fetch db >/dev/null 2>&1 9>&-; then
        printf '\n  pre-push: the advisory list could not be refreshed; checking against the one on disk.\n' >&2
    fi
    # How old the list on disk is: its last fetch, or failing that the
    # folder itself. The folder is deny.toml's db-path, so moving it there
    # moves the check with it (#949 review); cargo-deny's own default when
    # deny.toml names none.
    _db=${KLIPP_ADVISORY_DB:-$(sed -n 's/^[[:space:]]*db-path[[:space:]]*=[[:space:]]*"\(.*\)".*/\1/p' deny.toml 2>/dev/null | head -n 1)}
    _db=${_db:-${CARGO_HOME:-$HOME/.cargo}/advisory-dbs}
    case "$_db" in "~"/*) _db="$HOME/${_db#\~/}" ;; esac
    _db_time=$(find "$_db" -maxdepth 3 \( -name FETCH_HEAD -o -name HEAD \) -path '*/.git/*' -printf '%T@\n' 2>/dev/null | sort -n | tail -n 1)
    [ -n "$_db_time" ] || _db_time=$(find "$_db" -maxdepth 1 -mindepth 1 -type d -printf '%T@\n' 2>/dev/null | sort -n | tail -n 1)
    _db_days=
    if [ -n "$_db_time" ]; then
        _db_days=$(( ($(date +%s) - ${_db_time%.*}) / 86400 ))
    else
        printf '\n  pre-push: no advisory list found in %s, so its age is not known.\n' "$_db" >&2
    fi
    # Under set -e a failing $(...) would end the gate unsaid: keep its status.
    _deny_status=0
    _deny_out=$(timeout 120 cargo deny --offline check advisories 2>&1 9>&-) || _deny_status=$?
    # Each advisory found as "ID  crate version: title"; each stale
    # entry in deny.toml by its ID.
    _found=$(printf '%s\n' "$_deny_out" | awk '
        /^error\[(vulnerability|unsound|unmaintained|notice)\]/ { title = $0; sub(/^error\[[a-z]+\]: /, "", title); crate = ""; want = 1; next }
        want && crate == "" && / registry\+/ { line = $0; sub(/^ *[0-9]+ [^ ]+ /, "", line); split(line, f, " "); crate = f[1] " " f[2] }
        want && /ID: RUSTSEC/ { id = $0; sub(/.*ID: /, "", id); print id "  " crate ": " title; want = 0 }' | sort -u)
    _stale=$(printf '%s\n' "$_deny_out" | awk '/advisory-not-detected/ { s = 1; next } s && match($0, /RUSTSEC-[0-9]+-[0-9]+/) { print substr($0, RSTART, RLENGTH); s = 0 }' | sort -u)
    if [ -n "$_stale" ]; then
        printf '\n  pre-push: deny.toml ignores advisories nothing has any more; drop them:\n' >&2
        printf '%s\n' "$_stale" | sed 's/^/            /' >&2
    fi
    if [ -n "$_found" ]; then
        printf '\n  pre-push: a dependency has an advisory deny.toml does not record — push refused.\n' >&2
        printf '%s\n' "$_found" | sed 's/^/            /' >&2
        printf '            Update the crate, or if it cannot reach Klipp or has no fix yet,\n' >&2
        printf '            add it to [advisories] ignore in deny.toml with the reason and\n' >&2
        printf '            what would let it go. cargo deny check advisories shows the detail.\n\n' >&2
        exit 1
    elif [ "$_deny_status" -ne 0 ]; then
        printf '\n  pre-push: cargo deny could not check the advisories — push refused:\n' >&2
        printf '%s\n' "$_deny_out" | tail -n 3 | sed 's/^/            /' >&2
        printf '            Run cargo deny check advisories to see why.\n' >&2
        _audit_refused
    elif [ -n "$_db_days" ] && [ "$_db_days" -gt 30 ]; then
        printf '\n  pre-push: the advisory list on disk is %s days old — push refused:\n' "$_db_days" >&2
        printf '            it passes what a current one would not. Refresh it online with\n' >&2
        printf '            cargo deny fetch db.\n' >&2
        _audit_refused
    elif [ -n "$_db_days" ] && [ "$_db_days" -gt 7 ]; then
        printf '\n  pre-push: the advisory list on disk is %s days old; refresh it online with\n' "$_db_days" >&2
        printf '            cargo deny fetch db (over 30 days, the push is refused).\n\n' >&2
    fi
else
    printf '\n  pre-push: cargo-deny is not installed, so the dependencies cannot be checked\n' >&2
    printf '            for advisories — push refused. Install it:\n' >&2
    printf '            cargo install --locked cargo-deny\n' >&2
    _audit_refused
fi

# Which crates' tests run (#873). The whole suite ran for every push,
# whatever it touched: a change to the app's UI ran the colour science's
# tests too. Now the changed files (this branch against the main it
# leaves from) pick the crates: those changed, every crate built on them,
# and every crate whose tests use one of those, decided by
# scripts/affected_crates.py from cargo's own metadata. A change to the
# root Cargo.toml or Cargo.lock is read for what it touches (#1124); one
# to a file the whole workspace shares (assets/, vendor/, this script),
# or to a path it does not know, runs everything; documentation
# alone runs no tests. rustdoc documents the same crates; clippy always
# covers the whole workspace. And the whole suite runs anyway when it has not passed in
# KLIPP_GATE_FULL_HOURS (12) hours, so what a narrower run could miss is
# caught the same day, with its merge; KLIPP_GATE_FULL=1 asks for it.
_scope_args="--workspace"
_scope="full"
_scope_why="KLIPP_GATE_FULL is set"
if [ "${KLIPP_GATE_FULL:-}" != "1" ]; then
    _scope_why="the test scope could not be worked out"
    _full_mark=${_common:+$_common/klipp-pre-push.full}
    _last_full=$(cat "$_full_mark" 2>/dev/null || echo 0)
    case "$_last_full" in ''|*[!0-9]*) _last_full=0 ;; esac
    _base=$(git rev-parse --verify --quiet origin/main 2>/dev/null || true)
    if [ -z "$_full_mark" ]; then
        :
    elif [ $(( $(date +%s) - _last_full )) -ge $(( ${KLIPP_GATE_FULL_HOURS:-12} * 3600 )) ]; then
        _scope_why="the whole suite has not passed in ${KLIPP_GATE_FULL_HOURS:-12} hours"
    elif [ -n "$_base" ] && command -v python3 >/dev/null 2>&1 \
        && _fork=$(git merge-base "$_base" HEAD 2>/dev/null) \
        && _meta=$(cargo metadata --no-deps --format-version 1 --offline 2>/dev/null 9>&-) \
        && _metafile=$(mktemp) && printf '%s' "$_meta" >"$_metafile" \
        && _changed=$(git diff --name-only "$_fork" HEAD) \
        && _decision=$(printf '%s\n' "$_changed" | python3 "$ROOT/scripts/affected_crates.py" "$_metafile" --git "$_fork" HEAD 2>/dev/null); then
        rm -f "$_metafile"
        case "$_decision" in
            packages\ *)
                _scope=packages
                _scope_args=$(printf '%s' "${_decision#packages }" | sed 's/[^ ][^ ]*/-p &/g')
                _scope_why="${_decision#packages }" ;;
            none\ *) _scope=none; _scope_why="${_decision#none }" ;;
            full\ *) _scope_why="${_decision#full }" ;;
        esac
    fi
    [ -n "${_metafile:-}" ] && rm -f "$_metafile"
fi

# How long each step took, in the log: what to read when the gate is
# slow, and what a change to it is measured by.
_took() {
    printf '  pre-push: %s took %ss\n' "$1" $(( $(date +%s) - $2 ))
}

# Line tables instead of full debug info for everything the gate builds.
# The test binaries are linked with the dev profile's debug info, about
# 2 GB each, which made linking most of the test build (8 of a full run's
# 16 minutes) and set the gate's job count by memory, not by cores
# (gate-jobs.sh). Line tables keep file and line in a failing test's
# backtrace. The gate builds in its own target, so this rebuilds nothing
# anyone else uses.
export CARGO_PROFILE_DEV_DEBUG=line-tables-only

_step=lint
_t=$(date +%s)
if ! _logged cargo clippy --workspace --all-targets -- -D warnings 9>&-; then
    printf '\n  pre-push: clippy warned — push refused.\n' >&2
    printf '            Fix it, or if the lint is wrong for this code, decline\n' >&2
    printf '            it with a reason: in place with #[allow], or for the\n' >&2
    printf '            whole workspace under [workspace.lints.clippy].\n\n' >&2
    exit 1
fi

_took clippy "$_t"

# rustdoc documents the crates whose tests run (#873's scope): those a
# change touches and those built on them. It documented the whole
# workspace for every push, about 4 of a full run's 16 minutes, though a
# crate nothing changed in cannot have gained a broken link. A change
# with no tests to run (documentation) is documented whole, being rare.
_step=docs
_t=$(date +%s)
_doc_args=$_scope_args
[ "$_scope" = none ] && _doc_args=--workspace
printf '  pre-push: documenting with %s job(s).\n' "$_doc_jobs"
# shellcheck disable=SC2086 # the crates, one -p each
if ! _logged env RUSTDOCFLAGS="-D warnings" cargo doc -j "$_doc_jobs" $_doc_args --no-deps 9>&-; then
    printf '\n  pre-push: rustdoc warned — push refused.\n' >&2
    printf '            Usually a [`link`] to something renamed or private, or a\n' >&2
    printf '            bare [0,1] or Vec<T> in prose that needs backticks.\n\n' >&2
    exit 1
fi
_took rustdoc "$_t"

case "$_scope" in
    full) printf '  pre-push: running the whole test suite: %s\n' "$_scope_why" ;;
    packages) printf '  pre-push: running the tests of the crates this touches: %s\n' "$_scope_why" ;;
    none) printf '  pre-push: no tests to run: %s\n' "$_scope_why" ;;
esac

# The scripts' own tests (the gate's, the hook installer's, packaging's)
# when a change touches them: cargo never ran them, and two went stale
# for days unseen. Without a scope (the whole suite, or one that could
# not be worked out) they run too, being quick.
_step=scripts
if command -v python3 >/dev/null 2>&1 && [ -d "$ROOT/scripts/tests" ] \
    && { [ "$_scope" = full ] || printf '%s\n' "${_changed:-}" | grep -q '^\(scripts\|packaging\)/'; }; then
    if ! _logged python3 -m unittest discover -s "$ROOT/scripts/tests" -p 'test_*.py' </dev/null 9>&-; then
        printf '\n  pre-push: the scripts'"'"' own tests failed — push refused.\n' >&2
        printf '            python3 -m unittest discover -s scripts/tests -p '"'"'test_*.py'"'"'\n\n' >&2
        exit 1
    fi
fi

# Built first and separately, so a compile error is reported as one
# rather than as "tests FAILED" — which sends you reading test names
# for a problem that is not in any of them.
_step=build
if [ "$_scope" = none ]; then
    _check_push_refs
    printf '\n  pre-push: clippy and rustdoc pass; nothing else to run.\n\n'
    exit 0
fi
_t=$(date +%s)
# shellcheck disable=SC2086 # the crates, one -p each
if ! _logged cargo test $_scope_args --no-run 9>&-; then
    printf '\n  pre-push: the tests did not BUILD — push refused.\n' >&2
    printf '            Nothing ran. This is a compile or link error, not a\n' >&2
    printf '            failing assertion.\n\n' >&2
    printf '            `undefined hidden symbol: anon.*.llvm.*` in a crate you\n' >&2
    printf '            did not touch is a truncated object from an earlier\n' >&2
    printf '            interrupted build, not your change:\n\n' >&2
    printf '              cargo clean -p <that crate>\n\n' >&2
    exit 1
fi

_took "the test build" "$_t"

_step=tests
_t=$(date +%s)
# shellcheck disable=SC2086 # the crates, one -p each
if _logged cargo test $_scope_args 9>&-; then
    _took "the tests" "$_t"
    _check_push_refs
    printf '\n  pre-push: tests pass.\n\n'
    # When the whole suite last passed, for the next gate's scope.
    [ "$_scope" = full ] && [ -n "${_common:-}" ] && date +%s >"$_common/klipp-pre-push.full" 2>/dev/null
    exit 0
fi

printf '\n  pre-push: tests FAILED — push refused.\n' >&2
printf '            The test run failed; inspect the error above for its cause.\n' >&2
printf '            Disk quota exceeded / No space left on device indicate storage,\n' >&2
printf '            not an assertion failure. Scratch directory: %s\n' "$TMPDIR" >&2
scratch_space
printf '            Free space or choose a disk-backed TMPDIR and rerun.\n' >&2
if command -v pgrep >/dev/null 2>&1 && pgrep -x klipp-app >/dev/null 2>&1; then
    printf '\n            Klipp is running, and the GPU tests contend with\n' >&2
    printf '            it. Close it and push again before reading further:\n' >&2
    printf '            this is the likeliest explanation, not your change.\n' >&2
fi
printf '            If gpu_pipelines_build is among them, a shader stopped\n' >&2
printf '            compiling and the rest of the GPU suite is skipping\n' >&2
printf '            rather than failing. Fix that one first.\n\n' >&2
exit 1
