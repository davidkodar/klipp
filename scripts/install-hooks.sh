#!/bin/sh
# Install the repo's git hooks, preserving whatever is already there.
#
# Idempotent: run it as often as you like. It appends a delegation line to
# `.git/hooks/pre-push` rather than overwriting the file, because the
# existing hook is git-lfs's and replacing it would quietly stop LFS
# working if this repo ever starts using it.

set -e

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

HOOKS=$(git config --get core.hooksPath || echo "$ROOT/.git/hooks")
TARGET="$HOOKS/pre-push"
MARKER='scripts/pre-push.sh'
# Bumped whenever the block below changes, so a re-run replaces an older
# block instead of reporting it installed.
VERSION='klipp-gate-block v3'
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
if [ ! -f "$TARGET" ]; then
    printf '#!/bin/sh\n' > "$NEW"
elif grep -q "$MARKER" "$TARGET"; then
    # An older block of ours, appended at the end: keep what is above it.
    if ! grep -q "^$BLOCK_START" "$TARGET"; then
        printf '  %s runs %s but its block header was edited;\n' "$TARGET" "$MARKER" >&2
        printf '  leaving it alone. Remove our block by hand and run this again.\n' >&2
        exit 1
    fi
    awk -v start="$BLOCK_START" 'index($0, start) == 1 { exit } { print }' "$TARGET" > "$NEW"
    printf '  replacing an older test gate block in %s\n' "$TARGET"
else
    cat "$TARGET" > "$NEW"
fi
[ -f "$TARGET" ] && cp "$TARGET" "$TARGET.klipp-backup"

# Appended, so git-lfs's own lines keep running first. The existing hook
# has no `set -e` and ends with `git lfs pre-push`, whose exit status is
# therefore the hook's — so capture it before running the gate and hand
# it back afterwards. Otherwise installing this would silently turn an
# LFS refusal into a pass.
cat >> "$NEW" <<'HOOK'

# --- Klipp test gate (scripts/pre-push.sh) ---------------------------
# Appended by scripts/install-hooks.sh. Delete this block to remove the
# gate; the lines above it are git-lfs's, not ours.
#
# `$?` here is still git-lfs's status: comments and blank lines do not
# change it. Preserved so an LFS refusal above is not overridden by the
# tests passing.
# klipp-gate-block v3
klipp_lfs_status=$?
klipp_gate="$(git rev-parse --show-toplevel)/scripts/pre-push.sh"
if [ ! -x "$klipp_gate" ]; then
    printf '\n  pre-push: %s is missing — push refused.\n\n' "$klipp_gate" >&2
    exit 1
fi
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
        "$klipp_gate" || exit 1
    else
        printf '%s %s %s %s\n' "$$" "$(git symbolic-ref --short -q HEAD || git rev-parse --short HEAD)" \
            "$(git rev-parse --show-toplevel)" "$(date +%s)" > "$klipp_common/klipp-pre-push.holder"
        "$klipp_gate" 9>&- || exit 1
    fi
else
    "$klipp_gate" || exit 1
fi
exit $klipp_lfs_status
HOOK

chmod +x "$NEW"
mv "$NEW" "$TARGET"
printf '  installed the test gate into %s\n' "$TARGET"
printf '  bypass a single push with:  KLIPP_SKIP_TESTS=1 git push\n'
