#!/bin/sh
# Install the repo's git hooks, preserving whatever is already there.
#
# Idempotent: run it as often as you like. What the hook held before it —
# git-lfs's lines — is kept, moved to `pre-push.lfs` beside it and run
# first, because replacing it would quietly stop LFS working if this
# repo ever starts using it.
#
# Git hands a pre-push hook the list of refs being pushed on stdin, and
# git-lfs reads all of it. The hook reads the list once and gives each
# of the two its own copy: appended after git-lfs's lines, the gate got
# nothing and could not tell a branch delete from a push of code.

set -e

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

HOOKS=$(git config --get core.hooksPath || echo "$ROOT/.git/hooks")
TARGET="$HOOKS/pre-push"
MARKER='scripts/pre-push.sh'
# Bumped whenever the block below changes, so a re-run replaces an older
# block instead of reporting it installed.
VERSION='klipp-gate-block v6'
BLOCK_START='# --- Klipp test gate (scripts/pre-push.sh) ---'

mkdir -p "$HOOKS"

if grep -q "$VERSION" "$TARGET" 2>/dev/null; then
    printf '  already installed in %s — nothing to do\n' "$TARGET"
    exit 0
fi

# The new hook is written beside the old one and moved over it. A hook
# that is running reads its script as it goes, by byte offset: rewriting
# the file in place under it spliced old offsets into new text, which
# fabricated verdicts and exec'd an empty command. A moved-in file is a
# new inode, and running shells go on reading the old one.
NEW="$TARGET.klipp-new"
LFS="$HOOKS/pre-push.lfs"
LFS_NEW="$LFS.klipp-new"
# What ran before the gate, to be kept as `pre-push.lfs`: the whole hook
# if it is not ours, what sits above our block in an older one (v5 and
# before appended the block to git-lfs's lines), nothing new if it is a
# v6 hook already and `pre-push.lfs` holds them.
if [ -f "$TARGET" ] && grep -q "$MARKER" "$TARGET"; then
    if ! grep -q "^$BLOCK_START" "$TARGET"; then
        printf '  %s runs %s but its block header was edited;\n' "$TARGET" "$MARKER" >&2
        printf '  leaving it alone. Remove our block by hand and run this again.\n' >&2
        exit 1
    fi
    if ! grep -q '^klipp_refs=' "$TARGET"; then
        awk -v start="$BLOCK_START" 'index($0, start) == 1 { exit } { print }' "$TARGET" > "$LFS_NEW"
    fi
    printf '  replacing an older test gate block in %s\n' "$TARGET"
elif [ -f "$TARGET" ]; then
    cat "$TARGET" > "$LFS_NEW"
fi
# Kept only if it runs anything: a bare shebang is not worth a file.
if [ -f "$LFS_NEW" ]; then
    if grep -v '^#' "$LFS_NEW" | grep -q '[^[:space:]]'; then
        chmod +x "$LFS_NEW"
        mv "$LFS_NEW" "$LFS"
    else
        rm -f "$LFS_NEW"
    fi
fi
[ -f "$TARGET" ] && cp "$TARGET" "$TARGET.klipp-backup"
cat > "$NEW" <<'HOOK'
#!/bin/sh
# Written by scripts/install-hooks.sh. Git's list of the refs being
# pushed arrives on stdin once; each of the two below gets its own copy.
klipp_refs=$(cat)
# What ran here before the gate (git-lfs's lines), kept beside this file.
# Its refusal is the push's, and there is no gate to run after one.
klipp_lfs="$(dirname "$0")/pre-push.lfs"
if [ -x "$klipp_lfs" ]; then
    { [ -n "$klipp_refs" ] && printf '%s\n' "$klipp_refs"; } | "$klipp_lfs" "$@"
    klipp_lfs_status=$?
    [ "$klipp_lfs_status" -eq 0 ] || exit "$klipp_lfs_status"
fi
HOOK

cat >> "$NEW" <<'HOOK'

# --- Klipp test gate (scripts/pre-push.sh) ---------------------------
# Written by scripts/install-hooks.sh. Delete this file and rename
# pre-push.lfs back to pre-push to remove the gate.
# klipp-gate-block v6
klipp_gate="$(git rev-parse --show-toplevel)/scripts/pre-push.sh"
if [ ! -x "$klipp_gate" ]; then
    printf '\n  pre-push: %s is missing — push refused.\n\n' "$klipp_gate" >&2
    exit 1
fi
# The gate runs in a session of its own, watched: when git, this hook's
# parent, is gone — a push killed by its pid — the watcher ends the
# gate's whole group, cargo and test binaries with it. Otherwise a killed
# push left its gate building for nobody, holding the queue meanwhile.
# Ctrl-C and a kill of this hook end the group too.
klipp_git=$PPID
klipp_gate_pid=
klipp_run_gate() {
    if ! command -v setsid >/dev/null 2>&1; then
        "$klipp_gate"
        return
    fi
    # Without job control a background job is not a group leader, so
    # setsid makes the new session in place and $! is the gate's pid and
    # group; with it on, setsid would fork and $! be a wrapper whose exit
    # status says nothing about the gate's.
    set +m
    # And a background job's stdin is /dev/null, so git's list of refs is
    # handed over on another descriptor first.
    exec 3<&0
    setsid "$klipp_gate" <&3 3<&- &
    klipp_gate_pid=$!
    exec 3<&-
    # The watcher is a process of its own, in a session of its own: a
    # forked subshell would keep this shell's saved copy of the lock's fd
    # 9, and die with the push's group when that is killed whole. Git
    # gone shows as this hook being handed to another parent — a dead git
    # lingers as a zombie until its own parent reaps it, and a zombie
    # still answers kill -0 — and this hook gone as its /proc entry going,
    # or showing it a zombie.
    setsid sh -c '
        trap "" INT
        while kill -0 "$3" 2>/dev/null; do
            read -r _ _ state parent _ < "/proc/$1/stat" 2>/dev/null || break
            [ "$state" != Z ] && [ "$parent" = "$2" ] || break
            sleep 1
        done
        kill -0 "$3" 2>/dev/null && kill -TERM -- "-$3" 2>/dev/null
    ' klipp-gate-watcher "$$" "$klipp_git" "$klipp_gate_pid" </dev/null 9>&- &
    klipp_watch_pid=$!
    wait "$klipp_gate_pid"
    klipp_status=$?
    kill "$klipp_watch_pid" 2>/dev/null
    klipp_gate_pid=
    return "$klipp_status"
}
trap '[ -n "$klipp_gate_pid" ] && kill -TERM -- "-$klipp_gate_pid" 2>/dev/null; exit 130' INT TERM HUP
# The gate with git's list of refs on its stdin. A here-document, not a
# pipe: a function at the end of a pipe runs in a subshell, where the
# trap above could not see the gate's pid.
klipp_run_gate_refs() {
    klipp_run_gate <<KLIPP_REFS
$klipp_refs
KLIPP_REFS
}

# Git runs the worktree's own copy of the gate, and a branch from before
# the gate queued itself (#432) carries one that never queues: several of
# those at once ran the machine out of memory. Such a gate is queued
# here, on the same lock, held on this shell's fd 9 and never by the
# gate's children, so an orphaned cargo cannot hold the queue. Once the
# lock is ours the gate is looked at again: a branch rebased while it
# waited now queues itself, and would wait for ever on the lock this
# shell holds, so the lock is let go and the gate left to queue.
klipp_common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || klipp_common=
if [ "$KLIPP_SKIP_TESTS" != "1" ] && [ -n "$klipp_common" ] && [ -d "$klipp_common" ] \
   && command -v flock >/dev/null 2>&1 && ! grep -q klipp-pre-push.lock "$klipp_gate"; then
    printf '\n  pre-push: this branch predates the gate queue (#432); queueing it here.\n\n' >&2
    exec 9>>"$klipp_common/klipp-pre-push.lock"
    flock 9
    if grep -q klipp-pre-push.lock "$klipp_gate"; then
        exec 9>&-
        klipp_run_gate_refs || exit 1
    else
        printf '%s %s %s %s\n' "$$" "$(git symbolic-ref --short -q HEAD || git rev-parse --short HEAD)" \
            "$(git rev-parse --show-toplevel)" "$(date +%s)" > "$klipp_common/klipp-pre-push.holder"
        klipp_run_gate_refs 9>&- || exit 1
    fi
else
    klipp_run_gate_refs || exit 1
fi
exit 0
HOOK

chmod +x "$NEW"
mv "$NEW" "$TARGET"
printf '  installed the test gate into %s\n' "$TARGET"
printf '  bypass a single push with:  KLIPP_SKIP_TESTS=1 git push\n'
