#!/usr/bin/env python3
"""Exercise the gate's scratch policy without compiling or pushing anything."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class ScratchGateTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parents[2] / "target")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.root / "scripts").mkdir()
        shutil.copy(Path(__file__).resolve().parents[1] / "pre-push.sh", self.root / "scripts/pre-push.sh")
        self.script(self.root / "scripts/check-signoff.sh", "exit 0")
        self.script(self.bin / "git", 'case "$*" in\n "rev-parse --show-toplevel") printf "%s\\n" "$TEST_ROOT";;\n "rev-parse --path-format=absolute --git-common-dir") [ -n "$TEST_GITDIR" ] && printf "%s\\n" "$TEST_GITDIR";;\n "symbolic-ref --short -q HEAD") echo fix/scratch;;\nesac')
        self.gitdir = self.root / "gitdir"
        self.gitdir.mkdir()
        self.script(self.bin / "pgrep", "exit 1")
        self.script(self.bin / "df", 'printf "Filesystem 1024-blocks Used Available Capacity Mounted on\\nfixture 9000000 1 %s 1%% /scratch\\n" "$TEST_FREE"')
        self.script(self.bin / "cargo", 'printf "%s|%s\\n" "$*" "$TMPDIR" >> "$TEST_ROOT/cargo-calls"\n[ -e /proc/$$/fd/9 ] && echo "$*" >> "$TEST_ROOT/cargo-held-fd9"\nif [ "$*" = "test --workspace" ]; then exit "${TEST_EXIT:-0}"; fi')
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("KLIPP_", "CARGO_")) and k != "TMPDIR"}
        self.env.update(PATH=f"{self.bin}:{os.environ['PATH']}", TEST_ROOT=str(self.root), TEST_FREE="8000000", TEST_GITDIR=str(self.gitdir))

    def script(self, path, body):
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def run_gate(self):
        return subprocess.run(["sh", str(self.root / "scripts/pre-push.sh")], env=self.env, text=True, capture_output=True)

    def test_default_is_workspace_target(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"|{self.root}/target/test-scratch", (self.root / "cargo-calls").read_text())

    def test_relative_target_is_made_absolute(self):
        self.env["CARGO_TARGET_DIR"] = "shared-build"
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"|{self.root}/shared-build/test-scratch", (self.root / "cargo-calls").read_text())

    def test_explicit_tmpdir_wins(self):
        self.env.update(TMPDIR=str(self.root / "chosen scratch"), CARGO_TARGET_DIR="other-target")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"|{self.root}/chosen scratch", (self.root / "cargo-calls").read_text())

    def test_low_space_refuses_before_compiling(self):
        self.env["TEST_FREE"] = "400000"
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("less than 2 GiB", result.stderr)
        self.assertIn("TMPDIR", result.stderr)
        self.assertFalse((self.root / "cargo-calls").exists())

    def test_runtime_failure_mentions_storage(self):
        self.env["TEST_EXIT"] = "101"
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("Disk quota exceeded", result.stderr)
        self.assertIn(str(self.root / "target/test-scratch"), result.stderr)
        self.assertNotIn("these are assertions", result.stderr)


    def test_a_held_gate_is_waited_for_or_refused(self):
        # Another gate holds the lock: with KLIPP_GATE_NO_WAIT the push is
        # refused, naming the holder, before anything compiles.
        (self.gitdir / "klipp-pre-push.holder").write_text("4242 fix/other /elsewhere 0\n")
        holder = subprocess.Popen(["flock", str(self.gitdir / "klipp-pre-push.lock"), "sleep", "30"])
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        for _ in range(100):
            if subprocess.run(["flock", "-n", str(self.gitdir / "klipp-pre-push.lock"), "true"]).returncode != 0:
                break
            subprocess.run(["sleep", "0.02"])
        self.env["KLIPP_GATE_NO_WAIT"] = "1"
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("another gate is running: 4242 fix/other /elsewhere", result.stderr)
        self.assertFalse((self.root / "cargo-calls").exists())

    def test_the_gate_names_itself_and_cargo_does_not_hold_the_lock(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.gitdir / "klipp-pre-push.holder").read_text().split()[1] == "fix/scratch")
        # A cargo orphaned by a killed hook must not keep the queue locked.
        self.assertFalse((self.root / "cargo-held-fd9").exists(), (self.root / "cargo-held-fd9").read_text() if (self.root / "cargo-held-fd9").exists() else "")

    def test_no_git_common_dir_runs_unqueued(self):
        self.env["TEST_GITDIR"] = ""
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "cargo-calls").exists())

if __name__ == "__main__":
    (Path(__file__).resolve().parents[2] / "target").mkdir(exist_ok=True)
    unittest.main()
