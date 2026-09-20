#!/bin/sh
# Turn on server-side protection for main. Run this once, on the day the
# repository goes public.
#
# Why it is a script and not a line in a runbook: GitHub gates branch
# protection *and* rulesets behind GitHub Pro on private repositories,
# so this cannot be done in advance. It has to happen at the moment the
# repo flips to public, which is a moment where a forgotten step is
# expensive — an unprotected default branch on a public repo is exactly
# when the rule matters most.
#
# Until then the pre-push hook refuses direct pushes to main, which
# covers everyone who can push while the repo is private. This replaces
# that local-only arrangement with one GitHub enforces for everybody,
# including people who never install a hook.
#
# Idempotent: run it again and it updates the ruleset in place.
#
#     scripts/protect-main.sh              # act on the repo `origin` points at
#     scripts/protect-main.sh owner/name   # or name it explicitly

set -e

repo="${1:-}"
if [ -z "$repo" ]; then
    repo=$(gh repo view --json nameWithOwner -q .nameWithOwner)
fi

printf '  repository: %s\n' "$repo"

visibility=$(gh repo view "$repo" --json visibility -q .visibility)
printf '  visibility: %s\n' "$visibility"

if [ "$visibility" != "PUBLIC" ]; then
    printf '\n  This repository is %s. GitHub gates branch protection and rulesets\n' "$visibility" >&2
    printf '  behind GitHub Pro on anything that is not public, so this will be\n' >&2
    printf '  refused with a 403.\n\n' >&2
    printf '  Make it public first, or upgrade the account. Meanwhile the\n' >&2
    printf '  pre-push hook (scripts/pre-push.sh) refuses direct pushes to main,\n' >&2
    printf '  which is the same rule enforced one step earlier.\n\n' >&2
    exit 1
fi

# required_approving_review_count is 0 deliberately: a solo maintainer
# cannot approve their own pull request, and a rule nobody can satisfy
# is a rule that gets switched off. The PR itself is the gate — the
# branch cannot be written to directly, the history stays linear, and
# every merge is a squash with a reviewable diff. Raise this the day
# there is a second person who can press approve.
#
# No required_status_checks either: there is no CI to require. If that
# ever changes, add it here and nowhere else — and only once the check
# is green and trusted, never as a way of finding out whether it works.
cat > /tmp/klipp-ruleset.$$ <<'JSON'
{
  "name": "main: pull requests only",
  "target": "branch",
  "enforcement": "active",
  "conditions": { "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] } },
  "rules": [
    { "type": "deletion" },
    { "type": "non_fast_forward" },
    { "type": "required_linear_history" },
    { "type": "pull_request",
      "parameters": {
        "required_approving_review_count": 0,
        "dismiss_stale_reviews_on_push": true,
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_review_thread_resolution": true,
        "allowed_merge_methods": ["squash"]
      }
    }
  ]
}
JSON

existing=$(gh api "repos/$repo/rulesets" --jq '.[] | select(.name == "main: pull requests only") | .id' 2>/dev/null || true)

if [ -n "$existing" ]; then
    printf '  updating ruleset %s\n' "$existing"
    gh api -X PUT "repos/$repo/rulesets/$existing" --input "/tmp/klipp-ruleset.$$" > /dev/null
else
    printf '  creating the ruleset\n'
    gh api -X POST "repos/$repo/rulesets" --input "/tmp/klipp-ruleset.$$" > /dev/null
fi

rm -f "/tmp/klipp-ruleset.$$"

printf '\n  main is protected: pull requests only, squash merges, linear history,\n'
printf '  no force pushes, no deletion.\n\n'
printf '  There is no CI to switch on. A fork PR has had nothing check it, so\n'
printf '  run this against one before merging it:\n\n'
printf '    gh pr checkout <n> && scripts/check-signoff.sh main..HEAD\n\n'
