#!/usr/bin/env python3
"""A branch that puts a file back to the copy a later main commit replaced
is found (stale_files.py), in a throwaway repository."""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "stale_files.py"


def git(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True, text=True).stdout.strip()


def commit(cwd, files, message):
    for name, text in files.items():
        path = Path(cwd) / name
        if text is None:
            path.unlink()
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
    git(cwd, "add", "-A")
    git(cwd, "commit", "-q", "-m", message)
    return git(cwd, "rev-parse", "HEAD")


def run(cwd, fork, head):
    changed = git(cwd, "diff", "--name-only", fork, head)
    r = subprocess.run([sys.executable, str(SCRIPT), fork, head], cwd=cwd, input=changed, capture_output=True, text=True)
    return r.returncode, r.stdout


class StaleFilesTests(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.repo = self.dir.name
        env = {"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@t", "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@t"}
        os.environ.update(env)
        git(self.repo, "init", "-q", "-b", "main")
        self.base = commit(self.repo, {"a.rs": "a1\n", "b.rs": "b1\n"}, "base")

    def tearDown(self):
        self.dir.cleanup()

    def test_a_squash_onto_a_moved_main_is_found(self):
        # main moves on (b.rs changes, c.rs is added) while the branch,
        # cut at base, edits a.rs only.
        later = commit(self.repo, {"b.rs": "b2\n", "c.rs": "c1\n"}, "later main work")
        git(self.repo, "checkout", "-q", "-b", "branch", self.base)
        mine = commit(self.repo, {"a.rs": "a2\n"}, "branch work")
        # The bad squash: the branch's tree recorded on top of `later`.
        tree = git(self.repo, "rev-parse", f"{mine}^{{tree}}")
        squash = git(self.repo, "commit-tree", tree, "-p", later, "-m", "squash")
        code, out = run(self.repo, later, squash)
        self.assertEqual(code, 1, out)
        self.assertIn("b.rs", out)
        self.assertIn("c.rs", out, "a file main added and the branch drops")
        self.assertNotIn("a.rs", out, "the branch's own edit isn't stale")

    def test_a_branch_rebased_properly_is_clean(self):
        later = commit(self.repo, {"b.rs": "b2\n"}, "later main work")
        git(self.repo, "checkout", "-q", "-b", "branch", later)
        mine = commit(self.repo, {"a.rs": "a2\n", "b.rs": "b3\n"}, "branch work")
        self.assertEqual(run(self.repo, later, mine), (0, ""))

    def test_an_edit_back_to_an_older_text_by_hand_is_still_named(self):
        # Putting b.rs back to its text before `later` is a revert of
        # `later`, however it was made: named, for the author to confirm.
        later = commit(self.repo, {"b.rs": "b2\n"}, "later main work")
        git(self.repo, "checkout", "-q", "-b", "branch", later)
        mine = commit(self.repo, {"b.rs": "b1\n"}, "branch work")
        code, out = run(self.repo, later, mine)
        self.assertEqual(code, 1)
        self.assertIn("later main work", out)


if __name__ == "__main__":
    unittest.main()
