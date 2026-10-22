#!/bin/sh
# Who signed these commits, and is it a person?
#
# Three rules from CONTRIBUTING.md, checked here rather than in CI,
# where it would cost a runner and this costs nothing:
#
#   1. every commit carries a Signed-off-by (the DCO);
#   2. no AI agent is the author or the committer;
#   3. no Co-Authored-By trailers, for anyone.
#
# Rule 2 is the point and rule 1 is what makes it enforceable: signing
# off is a statement a person makes about code they are answerable for,
# and an agent cannot make it. A repository where the agent is named as
# author has no one to ask "why is this here?".
#
# Runs from the pre-push hook, and by hand over any range:
#
#     scripts/check-signoff.sh                 # origin/main..HEAD
#     scripts/check-signoff.sh main..my-branch
#     scripts/check-signoff.sh HEAD~5..HEAD
#
# This is the only thing enforcing them. There is no CI — see
# CONTRIBUTING.md. Everyone who pushes here has this hook, but a fork
# does not, so the maintainer runs this against a fork's branch by hand
# before merging it.

set -e

ROOT=$(git rev-parse --show-toplevel)
cd "$ROOT"

range="$1"
if [ -z "$range" ]; then
    if git rev-parse --verify --quiet origin/main >/dev/null 2>&1; then
        range="origin/main..HEAD"
    else
        range="HEAD"
    fi
fi

# An unresolvable range must not read as "nothing to check, all good".
# That is the same shape as the GPU tests that skip when a constructor
# returns None and report ok — a check that passes because it checked
# nothing is worse than no check, because it is trusted.
if ! commits=$(git rev-list "$range" 2>&1); then
    printf '\n  check-signoff: cannot resolve the range "%s":\n    %s\n' "$range" "$commits" >&2
    printf '  Refusing rather than reporting a pass on nothing.\n\n' >&2
    exit 1
fi

[ -z "$commits" ] && exit 0

fail=0

for sha in $commits; do
    subject=$(git log -1 --format=%s "$sha")
    short=$(git rev-parse --short "$sha")

    if ! git log -1 --format=%B "$sha" | grep -qi '^Signed-off-by: .\+ <.\+@.\+>'; then
        printf '\n  %s  %s\n' "$short" "$subject" >&2
        printf '    no Signed-off-by line. Sign your work: git commit -s\n' >&2
        printf '    (to fix the last commit: git commit --amend -s --no-edit)\n' >&2
        fail=1
    fi

    # Anchored deliberately. A loose pattern would reject a contributor
    # called Abbott for containing "bot", and a name is not a thing to
    # be clever about.
    if git log -1 --format='%an%n%cn' "$sha" \
         | grep -qiE '^(claude|copilot|chatgpt|codex|cursor|devin|gpt-|openai|anthropic|github-actions)\b' \
       || git log -1 --format='%ae%n%ce' "$sha" \
         | grep -qiE '@(anthropic|openai|users\.noreply\.anthropic)\.com$'; then
        printf '\n  %s  %s\n' "$short" "$subject" >&2
        printf '    authored or committed by what looks like an AI agent:\n' >&2
        printf '      author    %s\n' "$(git log -1 --format='%an <%ae>' "$sha")" >&2
        printf '      committer %s\n' "$(git log -1 --format='%cn <%ce>' "$sha")" >&2
        printf '    Commit under your own name and email. The author of a commit\n' >&2
        printf '    is whoever is answerable for the code in it.\n' >&2
        fail=1
    fi

    if git log -1 --format=%B "$sha" | grep -qi '^Co-Authored-By:'; then
        printf '\n  %s  %s\n' "$short" "$subject" >&2
        printf '    carries a Co-Authored-By trailer. Klipp does not use them —\n' >&2
        printf '    say who did what in the body, in prose.\n' >&2
        fail=1
    fi
done

if [ "$fail" = 1 ]; then
    printf '\n  Authorship check failed. See "Who may be the author of a commit"\n' >&2
    printf '  in CONTRIBUTING.md.\n\n' >&2
    exit 1
fi

exit 0
