#!/usr/bin/env python3
"""Install the hook into a scratch repository and run it, without cargo."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import unittest

REPO = Path(__file__).resolve().parents[2]

LFS_HOOK = "#!/bin/sh\necho lfs-ran >> \"$(git rev-parse --show-toplevel)/calls\"\n"

# The block as scripts/install-hooks.sh appended it before it queued
# stale gates: what an upgrade has to find and replace.
V1_BLOCK = """
# --- Klipp test gate (scripts/pre-push.sh) ---------------------------
# Appended by scripts/install-hooks.sh. Delete this block to remove the
# gate; the lines above it are git-lfs's, not ours.
klipp_lfs_status=$?
"$(git rev-parse --show-toplevel)/scripts/pre-push.sh" || exit 1
exit $klipp_lfs_status
"""


class InstallHooksTests(unittest.TestCase):
    def setUp(self):
        (REPO / "target").mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=REPO / "target")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "repo"
        self.root.mkdir()
        self.git("init", "-q", "-b", "fix/stale")
        (self.root / "scripts").mkdir()
        shutil.copy(REPO / "scripts/install-hooks.sh", self.root / "scripts/install-hooks.sh")
        self.hook = self.root / ".git/hooks/pre-push"
        self.hook.write_text(LFS_HOOK)
        self.hook.chmod(0o755)
        self.env = {k: v for k, v in os.environ.items() if not k.startswith("KLIPP_")}

    def git(self, *args):
        subprocess.run(["git", *args], cwd=self.root, check=True, capture_output=True)

    def gate(self, body):
        path = self.root / "scripts/pre-push.sh"
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def install(self, check=True):
        return subprocess.run(["sh", "scripts/install-hooks.sh"], cwd=self.root, env=self.env, text=True, capture_output=True, check=check)

    def run_hook(self, timeout=10):
        return subprocess.run(["sh", str(self.hook)], cwd=self.root, env=self.env, text=True, capture_output=True, timeout=timeout)

    def calls(self):
        path = self.root / "calls"
        return path.read_text().split() if path.exists() else []

    def test_installs_once_and_keeps_the_lfs_lines(self):
        self.install()
        again = self.install()
        self.assertIn("nothing to do", again.stdout)
        text = self.hook.read_text()
        self.assertEqual(text.count("Klipp test gate"), 1)
        # Since v6 (#603) git-lfs's lines live beside the hook, run from it.
        self.assertEqual((self.hook.parent / "pre-push.lfs").read_text(), LFS_HOOK)

    def test_an_older_block_is_replaced(self):
        self.hook.write_text(LFS_HOOK + V1_BLOCK)
        result = self.install()
        self.assertIn("replacing an older", result.stdout)
        text = self.hook.read_text()
        self.assertEqual(text.count("Klipp test gate"), 1)
        self.assertIn("klipp-gate-block v6", text)
        # The lines above the old block are git-lfs's, kept beside the hook.
        self.assertTrue((self.hook.parent / "pre-push.lfs").read_text().startswith(LFS_HOOK))
        self.assertEqual((self.root / ".git/hooks/pre-push.klipp-backup").read_text(), LFS_HOOK + V1_BLOCK)

    def test_a_stale_gate_waits_for_the_lock_and_names_itself(self):
        self.install()
        self.gate('echo gate-ran >> "$(git rev-parse --show-toplevel)/calls"')
        lock = self.root / ".git/klipp-pre-push.lock"
        holder = subprocess.Popen(["flock", "-o", str(lock), "sleep", "30"])
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        for _ in range(100):
            if subprocess.run(["flock", "-n", str(lock), "true"]).returncode != 0:
                break
            time.sleep(0.02)
        with self.assertRaises(subprocess.TimeoutExpired):
            self.run_hook(timeout=1)
        self.assertNotIn("gate-ran", self.calls())
        holder.kill()
        holder.wait()
        result = self.run_hook()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("predates the gate queue", result.stderr)
        self.assertIn("gate-ran", self.calls())
        self.assertEqual((self.root / ".git/klipp-pre-push.holder").read_text().split()[1], "fix/stale")

    def test_the_stale_gate_does_not_hold_the_lock(self):
        # An orphaned child of a killed gate must not keep the queue.
        self.install()
        self.gate('for f in /proc/$$/fd/*; do readlink "$f" || true; done > "$(git rev-parse --show-toplevel)/fds"')
        self.assertEqual(self.run_hook().returncode, 0)
        open_files = (self.root / "fds").read_text().splitlines()
        self.assertFalse([f for f in open_files if f.endswith("klipp-pre-push.lock")], open_files)

    def test_a_gate_that_queues_itself_is_not_wrapped(self):
        self.install()
        self.gate('# takes klipp-pre-push.lock itself\necho gate-ran >> "$(git rev-parse --show-toplevel)/calls"')
        lock = self.root / ".git/klipp-pre-push.lock"
        holder = subprocess.Popen(["flock", "-o", str(lock), "sleep", "30"])
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        time.sleep(0.2)
        result = self.run_hook(timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("gate-ran", self.calls())
        self.assertNotIn("predates", result.stderr)

    def test_a_refused_gate_refuses_the_push_and_lfs_still_runs(self):
        self.install()
        self.gate("exit 1")
        self.assertEqual(self.run_hook().returncode, 1)
        self.assertIn("lfs-ran", self.calls())

    def test_skip_tests_does_not_queue(self):
        self.install()
        self.gate('echo gate-ran >> "$(git rev-parse --show-toplevel)/calls"')
        lock = self.root / ".git/klipp-pre-push.lock"
        holder = subprocess.Popen(["flock", "-o", str(lock), "sleep", "30"])
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        time.sleep(0.2)
        self.env["KLIPP_SKIP_TESTS"] = "1"
        self.assertEqual(self.run_hook(timeout=5).returncode, 0)


    def test_the_hook_is_replaced_not_rewritten_in_place(self):
        # A running hook reads its file by offset; rewriting it in place
        # spliced old offsets into new text.
        self.hook.write_text(LFS_HOOK + V1_BLOCK)
        before = self.hook.stat().st_ino
        self.install()
        self.assertNotEqual(self.hook.stat().st_ino, before)

    def test_an_edited_block_header_is_left_alone(self):
        edited = LFS_HOOK + V1_BLOCK.replace("# --- Klipp test gate", "# --- my gate")
        self.hook.write_text(edited)
        result = self.install(check=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("edited", result.stderr)
        self.assertEqual(self.hook.read_text(), edited)

    def test_a_missing_gate_refuses_at_once(self):
        self.install()
        result = self.run_hook(timeout=5)
        self.assertEqual(result.returncode, 1)
        self.assertIn("is missing", result.stderr)

    def test_a_branch_rebased_while_it_waits_queues_itself(self):
        # Stale when the push began, queue-aware by the time the lock is
        # free: it must not wait on the lock its own hook holds.
        self.install()
        self.gate('echo gate-ran >> "$(git rev-parse --show-toplevel)/calls"')
        lock = self.root / ".git/klipp-pre-push.lock"
        holder = subprocess.Popen(["flock", "-o", str(lock), "sleep", "30"])
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        time.sleep(0.2)
        hook = subprocess.Popen(["sh", str(self.hook)], cwd=self.root, env=self.env, text=True,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(hook.kill)
        time.sleep(0.5)
        self.gate(f'# takes klipp-pre-push.lock itself\nexec 9>>"{lock}"\nflock -w 5 9 || exit 3\n'
                  'echo gate-ran >> "$(git rev-parse --show-toplevel)/calls"')
        holder.kill()
        holder.wait()
        _, err = hook.communicate(timeout=15)
        self.assertEqual(hook.returncode, 0, err)
        self.assertIn("gate-ran", self.calls())

    def gate_that_lingers(self):
        # Starts a child that would outlive a killed push, and says where.
        self.gate('sleep 60 &\necho $! > "$(git rev-parse --show-toplevel)/child"\nwait')

    def alive(self, pid):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return False
        with open(f"/proc/{pid}/stat") as f:
            return f.read().split()[2] != "Z"

    def wait_for_child(self):
        path = self.root / "child"
        for _ in range(200):
            if path.exists() and path.read_text().strip():
                return int(path.read_text())
            time.sleep(0.02)
        self.fail("the gate never started its child")

    def test_a_killed_push_takes_its_gate_with_it(self):
        self.install()
        self.gate_that_lingers()
        # Stands in for git: the hook's parent, killed by pid.
        push = subprocess.Popen(["sh", "-c", f'sh "{self.hook}"; echo after'], cwd=self.root, env=self.env)
        self.addCleanup(push.wait)
        child = self.wait_for_child()
        self.addCleanup(lambda: self.alive(child) and os.kill(child, 9))
        push.kill()
        for _ in range(100):
            if not self.alive(child):
                break
            time.sleep(0.05)
        self.assertFalse(self.alive(child), "the gate's work outlived its push")

    def test_interrupting_the_hook_ends_the_gate(self):
        self.install()
        self.gate_that_lingers()
        hook = subprocess.Popen(["sh", str(self.hook)], cwd=self.root, env=self.env)
        self.addCleanup(hook.wait)
        child = self.wait_for_child()
        self.addCleanup(lambda: self.alive(child) and os.kill(child, 9))
        hook.send_signal(2)
        hook.wait(timeout=5)
        for _ in range(60):
            if not self.alive(child):
                break
            time.sleep(0.05)
        self.assertFalse(self.alive(child), "Ctrl-C left the gate running")

    def test_the_gate_still_reads_the_refs_git_sends(self):
        self.install()
        self.gate('cat > "$(git rev-parse --show-toplevel)/stdin"')
        result = subprocess.run(["sh", str(self.hook)], cwd=self.root, env=self.env, text=True,
                                input="refs/heads/x abc refs/heads/x def\n", capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.root / "stdin").read_text(), "refs/heads/x abc refs/heads/x def\n")

    def test_a_killed_hook_takes_its_gate_with_it(self):
        self.install()
        self.gate_that_lingers()
        hook = subprocess.Popen(["sh", str(self.hook)], cwd=self.root, env=self.env)
        self.addCleanup(hook.wait)
        child = self.wait_for_child()
        self.addCleanup(lambda: self.alive(child) and os.kill(child, 9))
        hook.kill()
        for _ in range(100):
            if not self.alive(child):
                break
            time.sleep(0.05)
        self.assertFalse(self.alive(child), "the gate outlived its killed hook")

    def test_the_watcher_does_not_hold_the_lock(self):
        # A stale gate: the hook holds the queue's lock on fd 9 while it
        # runs. The watcher must not keep a copy, or a killed hook would
        # leave the queue locked.
        self.install()
        self.gate_that_lingers()
        hook = subprocess.Popen(["sh", str(self.hook)], cwd=self.root, env=self.env)
        self.addCleanup(hook.wait)
        self.addCleanup(hook.kill)
        child = self.wait_for_child()
        self.addCleanup(lambda: self.alive(child) and os.kill(child, 9))
        lock = str(self.root / ".git/klipp-pre-push.lock")
        watchers = []
        for pid in os.listdir("/proc"):
            if not pid.isdigit():
                continue
            try:
                cmd = open(f"/proc/{pid}/cmdline", "rb").read().split(b"\0")
            except OSError:
                continue
            if b"klipp-gate-watcher" in cmd and str(hook.pid).encode() in cmd:
                watchers.append(pid)
        self.assertEqual(len(watchers), 1, watchers)
        held = []
        for fd in os.listdir(f"/proc/{watchers[0]}/fd"):
            try:
                if os.readlink(f"/proc/{watchers[0]}/fd/{fd}") == lock:
                    held.append(fd)
            except OSError:
                pass
        self.assertEqual(held, [], "the watcher holds the queue's lock")

    def test_a_refused_gate_refuses_with_job_control_on(self):
        self.install()
        self.gate("exit 1")
        self.env["SHELLOPTS"] = "monitor"
        self.assertEqual(self.run_hook().returncode, 1)

    def path_without(self, tool):
        """PATH with the system's tools but not `tool`."""
        tools = Path(self.temp.name) / "tools"
        tools.mkdir(exist_ok=True)
        seen = {tool}
        for system in ("/usr/bin", "/bin"):
            for entry in os.scandir(system):
                if entry.name not in seen:
                    seen.add(entry.name)
                    (tools / entry.name).symlink_to(entry.path)
        return str(tools)

    def test_installing_without_cargo_deny_says_how_to_get_it(self):
        # #928: the gate refuses a push it cannot check for advisories.
        self.gate("exit 0")
        self.env["PATH"] = self.path_without("cargo-deny")
        result = self.install()
        self.assertIn("cargo install --locked cargo-deny", result.stdout)
        result = self.install()
        self.assertIn("nothing to do", result.stdout)
        self.assertIn("cargo install --locked cargo-deny", result.stdout, "said on a re-run too")


if __name__ == "__main__":
    unittest.main()
