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
VERSION='klipp-gate-block v2'
BLOCK_START='# --- Klipp test gate (scripts/pre-push.sh) ---'

mkdir -p "$HOOKS"

if [ ! -f "$TARGET" ]; then
    printf '#!/bin/sh\n' > "$TARGET"
    chmod +x "$TARGET"
    printf '  created %s\n' "$TARGET"
fi

if grep -q "$VERSION" "$TARGET" 2>/dev/null; then
    printf '  already installed in %s — nothing to do\n' "$TARGET"
    exit 0
fi

# An older block: ours runs from its first line to the end of the file,
# since it was appended there. Cut it and append the current one.
if grep -q "$MARKER" "$TARGET" 2>/dev/null; then
    tmp="$TARGET.klipp-upgrade"
    awk -v start="$BLOCK_START" 'index($0, start) == 1 { exit } { print }' "$TARGET" > "$tmp"
    cat "$tmp" > "$TARGET"
    rm -f "$tmp"
    printf '  replacing an older test gate block in %s\n' "$TARGET"
fi

# Appended, so git-lfs's own lines keep running first. The existing hook
# has no `set -e` and ends with `git lfs pre-push`, whose exit status is
# therefore the hook's — so capture it before running the gate and hand
# it back afterwards. Otherwise installing this would silently turn an
# LFS refusal into a pass.
cat >> "$TARGET" <<'HOOK'

# --- Klipp test gate (scripts/pre-push.sh) ---------------------------
# Appended by scripts/install-hooks.sh. Delete this block to remove the
# gate; the lines above it are git-lfs's, not ours.
#
# `$?` here is still git-lfs's status: comments and blank lines do not
# change it. Preserved so an LFS refusal above is not overridden by the
# tests passing.
# klipp-gate-block v2
klipp_lfs_status=$?
klipp_gate="$(git rev-parse --show-toplevel)/scripts/pre-push.sh"
# Git runs the worktree's own copy of the gate, and a branch from before
# the gate queued itself (#432) carries a copy that never queues: several
# of those at once ran the machine out of memory. Such a gate is queued
# here, on the same lock, recording who holds it as a queued gate does;
# -o keeps the lock out of the gate's children, so an orphaned cargo
# cannot hold the queue. A gate that queues itself runs as it is.
if [ "$KLIPP_SKIP_TESTS" != "1" ] && ! grep -q klipp-pre-push.lock "$klipp_gate" 2>/dev/null \
   && command -v flock >/dev/null 2>&1 \
   && klipp_common=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) \
   && [ -d "$klipp_common" ]; then
    printf '\n  pre-push: this branch predates the gate queue (#432); queueing it here.\n\n' >&2
    flock -o "$klipp_common/klipp-pre-push.lock" sh -c \
        'printf "%s %s %s %s\n" "$$" "$1" "$2" "$(date +%s)" > "$3"; exec "$4"' klipp-gate \
        "$(git symbolic-ref --short -q HEAD || git rev-parse --short HEAD)" \
        "$(git rev-parse --show-toplevel)" "$klipp_common/klipp-pre-push.holder" "$klipp_gate" || exit 1
else
    "$klipp_gate" || exit 1
fi
exit $klipp_lfs_status
HOOK

chmod +x "$TARGET"
printf '  installed the test gate into %s\n' "$TARGET"
printf '  bypass a single push with:  KLIPP_SKIP_TESTS=1 git push\n'
