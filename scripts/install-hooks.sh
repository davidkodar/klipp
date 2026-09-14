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

mkdir -p "$HOOKS"

if [ ! -f "$TARGET" ]; then
    printf '#!/bin/sh\n' > "$TARGET"
    chmod +x "$TARGET"
    printf '  created %s\n' "$TARGET"
fi

if grep -q "$MARKER" "$TARGET" 2>/dev/null; then
    printf '  already installed in %s — nothing to do\n' "$TARGET"
    exit 0
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
klipp_lfs_status=$?
"$(git rev-parse --show-toplevel)/scripts/pre-push.sh" || exit 1
exit $klipp_lfs_status
HOOK

chmod +x "$TARGET"
printf '  installed the test gate into %s\n' "$TARGET"
printf '  bypass a single push with:  KLIPP_SKIP_TESTS=1 git push\n'
