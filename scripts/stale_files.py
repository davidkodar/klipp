#!/usr/bin/env python3
"""Files a branch puts back to an older main version.

A squash made with `git reset --soft origin/main` after a fetch moved
origin/main records the branch's tree on top of commits it never saw, and
every file those commits changed goes back to the copy before them: the
later main commit is reverted, its tests with it, and the tree still builds
and passes. This finds such files: for each file the branch changes
against `fork`, whether the branch's copy is exactly the one some recent
main commit replaced (or the absence of one it added).

Usage: stale_files.py FORK HEAD < changed-paths
Prints one line per stale file, `path<TAB>commit<TAB>subject`, and exits 1
when there is any.
"""

import subprocess
import sys

# How far back along each file's main history to look. A stale squash
# reverts the newest commits, so a short window finds it.
DEPTH = 20


def git(*args, ok=(0,)):
    r = subprocess.run(["git", *args], capture_output=True, text=True)
    if r.returncode not in ok:
        raise RuntimeError(f"git {' '.join(args)}: {r.stderr.strip()}")
    return r


def blob(rev, path):
    """The object id of `path` at `rev`, or None when it isn't there."""
    r = git("rev-parse", "--verify", "--quiet", f"{rev}:{path}", ok=(0, 1, 128))
    return r.stdout.strip() if r.returncode == 0 else None


def stale(fork, head, paths):
    """(path, commit, subject) for each path whose copy at `head` is the
    one a commit in `fork`'s recent history of it replaced."""
    found = []
    for path in paths:
        mine = blob(head, path)
        if mine == blob(fork, path):
            continue
        log = git("log", f"-n{DEPTH}", "--format=%H%x09%s", fork, "--", path).stdout.splitlines()
        for line in log:
            commit, _, subject = line.partition("\t")
            parents = git("rev-list", "--parents", "-n1", commit).stdout.split()[1:]
            before = blob(parents[0], path) if parents else None
            if before == mine and blob(commit, path) != mine:
                found.append((path, commit, subject))
                break
    return found


def main():
    fork, head = sys.argv[1], sys.argv[2]
    paths = [p.strip() for p in sys.stdin if p.strip()]
    found = stale(fork, head, paths)
    for path, commit, subject in found:
        print(f"{path}\t{commit[:8]}\t{subject}")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
