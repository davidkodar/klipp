#!/usr/bin/env python3
"""Exercise the gate's scratch policy and its queue without compiling or pushing anything."""

import fcntl
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
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
        self.script(self.bin / "git", 'case "$*" in\n "rev-parse --show-toplevel") printf "%s\\n" "$TEST_ROOT";;\n "rev-parse --path-format=absolute --git-common-dir") [ -n "$TEST_GITDIR" ] && printf "%s\\n" "$TEST_GITDIR";;\n "rev-parse --short HEAD") echo abc1234;;\n "symbolic-ref --short -q HEAD") echo fix/scratch;;\nesac')
        self.gitdir = self.root / "gitdir"
        self.gitdir.mkdir()
        self.script(self.bin / "pgrep", "exit 1")
        self.script(self.bin / "df", 'printf "Filesystem 1024-blocks Used Available Capacity Mounted on\\nfixture 9000000 1 %s 1%% /scratch\\n" "$TEST_FREE"')
        self.script(self.bin / "cargo", 'printf "%s|%s\\n" "$*" "$TMPDIR" >> "$TEST_ROOT/cargo-calls"\nfor fd in 8 9; do [ -e "/proc/$$/fd/$fd" ] && echo "$fd $*" >> "$TEST_ROOT/cargo-held-fd"; done\nif [ "$1" = clippy ]; then\n printf "%s\\n" "${TEST_TAG:-gate}" >> "$TEST_ROOT/gate-order"\n [ -n "$TEST_HOLD" ] && sleep "$TEST_HOLD"\nfi\nif [ "$*" = "test --workspace" ]; then exit "${TEST_EXIT:-0}"; fi')
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("KLIPP_", "CARGO_")) and k != "TMPDIR"}
        self.env.update(PATH=f"{self.bin}:{os.environ['PATH']}", TEST_ROOT=str(self.root), TEST_FREE="8000000", TEST_GITDIR=str(self.gitdir))

    def script(self, path, body):
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def run_gate(self):
        return subprocess.run(["sh", str(self.root / "scripts/pre-push.sh")], env=self.env, text=True, capture_output=True)

    # The gate under test, started in the background so several of them
    # can be queued at once. Its pid is the pid the script sees as $$, and
    # its diagnostics go to a file so they can be read while it waits.
    def start_gate(self, tag, **extra):
        errors = self.root / f"stderr-{tag}"
        handle = errors.open("w")
        self.addCleanup(handle.close)
        gate = subprocess.Popen(["sh", str(self.root / "scripts/pre-push.sh")],
                                env=dict(self.env, TEST_TAG=tag, **extra),
                                stdout=subprocess.DEVNULL, stderr=handle)
        self.addCleanup(gate.kill)
        gate.said = lambda: errors.read_text()
        return gate

    # Read the queue the way the gate does: under the meta lock, so a
    # rewrite in progress is never read half-done.
    def queue_text(self):
        queue = self.gitdir / "klipp-pre-push.queue"
        if not queue.exists():
            return ""
        return subprocess.run(["flock", str(self.gitdir / "klipp-pre-push.queue.lock"), "cat", str(queue)],
                              text=True, capture_output=True).stdout

    def queued_pids(self):
        return [line.split()[0] for line in self.queue_text().splitlines() if line.strip()]

    # Stand in for a gate that holds the lock with no ticket in the queue:
    # a branch predating #432, which the installed hook runs under flock on
    # this same lock, writing this same holder file. Returns its release.
    def hold_the_gate_lock(self, holder="4242 fix/other /elsewhere 0\n"):
        if holder is not None:
            (self.gitdir / "klipp-pre-push.holder").write_text(holder)
        fd = os.open(str(self.gitdir / "klipp-pre-push.lock"), os.O_WRONLY | os.O_CREAT, 0o644)
        fcntl.flock(fd, fcntl.LOCK_EX)
        held = [fd]

        def release():
            if held:
                fcntl.flock(held[0], fcntl.LOCK_UN)
                os.close(held.pop())
        self.addCleanup(release)
        return release

    def until(self, ready, what, timeout=30):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if ready():
                return
            time.sleep(0.02)
        self.fail(f"timed out waiting for {what}; queue is {self.queue_text()!r}")

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


    # cargo-deny present, answering `cargo deny` with TEST_DENY_OUT (a file)
    # and TEST_DENY_EXIT, its fetch with TEST_FETCH_EXIT (#841).
    def with_cargo_deny(self, out="", status=0, fetch=0):
        self.script(self.bin / "cargo-deny", "exit 0")
        fake = self.root / "deny-out.txt"
        fake.write_text(out)
        cargo = (self.bin / "cargo").read_text().replace(
            "#!/bin/sh\n",
            '#!/bin/sh\nif [ "$1 $2" = "deny fetch" ]; then exit "$TEST_FETCH_EXIT"; fi\n'
            'if [ "$1" = deny ]; then cat "$TEST_DENY_OUT"; exit "$TEST_DENY_EXIT"; fi\n', 1)
        (self.bin / "cargo").write_text(cargo)
        self.env.update(TEST_DENY_OUT=str(fake), TEST_DENY_EXIT=str(status), TEST_FETCH_EXIT=str(fetch))

    ADVISORY = (
        "error[vulnerability]: Double free in `Consumer::skip`\n"
        "    ┌─ /w/Cargo.lock:470:1\n"
        "470 │ ringbuf 0.3.3 registry+https://github.com/rust-lang/crates.io-index\n"
        "    ├ ID: RUSTSEC-2026-0293\n"
    )
    STALE = (
        "warning[advisory-not-detected]: advisory was not encountered\n"
        "   ┌─ /w/deny.toml:37:11\n"
        '37 │   { id = "RUSTSEC-2020-0001", reason = "gone" },\n'
    )

    def test_an_unrecorded_advisory_refuses_the_push(self):
        self.with_cargo_deny(self.ADVISORY, status=1)
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("push refused", result.stderr)
        self.assertIn("RUSTSEC-2026-0293  ringbuf 0.3.3: Double free", result.stderr)
        self.assertFalse((self.root / "gate-order").exists(), "refused before the build")

    def test_recorded_advisories_pass(self):
        self.with_cargo_deny("advisories ok\n")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("advisor", result.stderr)

    def test_a_stale_exception_is_pointed_at_not_refused(self):
        self.with_cargo_deny(self.STALE + "advisories ok\n")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("RUSTSEC-2020-0001", result.stderr)
        self.assertIn("drop them", result.stderr)

    def test_an_audit_that_cannot_run_is_reported_not_refused(self):
        self.with_cargo_deny("error: failed to open advisory database\n", status=1, fetch=1)
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("could not be refreshed", result.stderr)
        self.assertIn("could not check", result.stderr)

    def test_without_cargo_deny_the_gate_says_how_to_get_it(self):
        # Only the system's own tools, so an installed cargo-deny is not found.
        self.env["PATH"] = f"{self.bin}:/usr/bin:/bin"
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("cargo install --locked cargo-deny", result.stderr)

    def test_a_held_gate_is_waited_for_or_refused(self):
        # Another gate holds the lock: with KLIPP_GATE_NO_WAIT the push is
        # refused, naming the holder, before anything compiles — and
        # without taking a place in the queue on the way past.
        self.hold_the_gate_lock()
        self.env["KLIPP_GATE_NO_WAIT"] = "1"
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("another gate is running: 4242 fix/other /elsewhere", result.stderr)
        self.assertFalse((self.root / "cargo-calls").exists())
        # It read the queue to report who is behind the holder, and left it
        # exactly as it found it.
        self.assertTrue((self.gitdir / "klipp-pre-push.queue").exists())
        self.assertEqual(self.queued_pids(), [])

    def test_the_gate_names_itself_and_cargo_does_not_hold_the_lock(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.gitdir / "klipp-pre-push.holder").read_text().split()[1] == "fix/scratch")
        # A cargo orphaned by a killed hook must not keep the queue locked:
        # neither the gate lock on fd 9 nor the meta lock on fd 8.
        held = self.root / "cargo-held-fd"
        self.assertFalse(held.exists(), held.read_text() if held.exists() else "")
        # It queued for its turn, and left the queue on the way out.
        self.assertTrue((self.gitdir / "klipp-pre-push.queue").exists())
        self.assertEqual(self.queued_pids(), [])

    def test_no_git_common_dir_runs_unqueued(self):
        self.env["TEST_GITDIR"] = ""
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "cargo-calls").exists())
        self.assertFalse((self.gitdir / "klipp-pre-push.queue").exists())

    def test_staggered_gates_run_in_the_order_they_arrived(self):
        order = self.root / "gate-order"
        first = self.start_gate("first", TEST_HOLD="4")
        self.until(lambda: order.exists() and "first" in order.read_text(), "the first gate to start its checks")
        second = self.start_gate("second")
        self.until(lambda: str(second.pid) in self.queued_pids(), "the second gate to join the queue")
        third = self.start_gate("third")
        self.until(lambda: str(third.pid) in self.queued_pids(), "the third gate to join the queue")
        # The queue is arrival order, the running gate still at its head.
        self.assertEqual(self.queued_pids(), [str(first.pid), str(second.pid), str(third.pid)])
        for gate in (first, second, third):
            self.assertEqual(gate.wait(timeout=90), 0, gate.said())
        # Each waiter said where it stood, and each ran in turn.
        self.assertIn(f"2nd in the queue, behind fix/scratch on {self.root}", second.said())
        self.assertIn(f"3rd in the queue, behind fix/scratch on {self.root}", third.said())
        self.assertEqual(order.read_text().split(), ["first", "second", "third"])
        self.assertEqual(self.queued_pids(), [])

    def test_a_killed_waiter_leaves_no_entry_in_the_queue(self):
        self.hold_the_gate_lock()
        waiter = self.start_gate("waiter")
        self.until(lambda: "in the queue" in waiter.said(), "the gate to report its position")
        self.assertEqual(self.queued_pids(), [str(waiter.pid)])
        waiter.terminate()
        self.assertEqual(waiter.wait(timeout=30), 143, waiter.said())
        # The holder record names a pid that is gone, and is reported as a
        # record rather than as a fact about who holds the lock.
        self.assertIn("1st in the queue, waiting for a gate outside the queue: "
                      "4242 fix/other /elsewhere", waiter.said())
        self.assertIn("that pid is gone, so the record may be stale", waiter.said())
        self.assertEqual(self.queued_pids(), [])
        self.assertFalse((self.root / "cargo-calls").exists())

    def test_a_gate_outside_the_queue_is_waited_for_at_the_head(self):
        # The queue decides the order among gates that have tickets; the
        # lock is still what grants exclusivity, so being first in the
        # queue is not enough to run past a gate that has no ticket.
        release = self.hold_the_gate_lock(f"{os.getpid()} fix/legacy /legacy 0\n")
        gate = self.start_gate("queued")
        self.until(lambda: "outside the queue" in gate.said(), "the gate to name what it is waiting for")
        self.assertEqual(self.queued_pids(), [str(gate.pid)])
        self.assertIsNone(gate.poll())
        self.assertFalse((self.root / "cargo-calls").exists())
        self.assertIn(f"waiting for a gate outside the queue: {os.getpid()} fix/legacy /legacy, held for",
                      gate.said())
        release()
        self.assertEqual(gate.wait(timeout=60), 0, gate.said())
        self.assertTrue((self.root / "cargo-calls").exists())
        self.assertEqual(self.queued_pids(), [])

    def test_the_queue_moves_when_a_gate_outside_it_lets_go(self):
        # Nothing is recorded about this holder, which is said plainly
        # rather than filled in, and three tickets wait behind it.
        release = self.hold_the_gate_lock(holder=None)
        gates = []
        for tag in ("one", "two", "three"):
            gate = self.start_gate(tag)
            self.until(lambda g=gate: str(g.pid) in self.queued_pids(), f"gate {tag} to join the queue")
            gates.append(gate)
        self.assertEqual(self.queued_pids(), [str(gate.pid) for gate in gates])
        self.until(lambda: "nothing recorded about it" in gates[0].said(),
                   "the head of the queue to report the holder it cannot name")
        release()
        for gate in gates:
            self.assertEqual(gate.wait(timeout=90), 0, gate.said())
        self.assertEqual((self.root / "gate-order").read_text().split(), ["one", "two", "three"])
        self.assertEqual(self.queued_pids(), [])

    def test_a_stale_pid_in_the_queue_does_not_block(self):
        # pid_max is one past the largest pid the kernel will hand out, so
        # no process can be wearing it.
        stale = Path("/proc/sys/kernel/pid_max").read_text().strip()
        (self.gitdir / "klipp-pre-push.queue").write_text(f"{stale} fix/dead /gone 0\n")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("in the queue", result.stderr)
        self.assertEqual(self.queued_pids(), [])
        self.assertTrue((self.root / "cargo-calls").exists())

    def test_no_wait_refuses_rather_than_jump_the_queue(self):
        # The lock is free, but a live session is waiting its turn for it.
        ahead = subprocess.Popen(["sleep", "30"])
        self.addCleanup(ahead.wait)
        self.addCleanup(ahead.kill)
        (self.gitdir / "klipp-pre-push.queue").write_text(f"{ahead.pid} fix/other /elsewhere 0\n")
        self.env["KLIPP_GATE_NO_WAIT"] = "1"
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("1 session(s) are already queued for the gate", result.stderr)
        self.assertFalse((self.root / "cargo-calls").exists())
        self.assertEqual(self.queued_pids(), [str(ahead.pid)])

if __name__ == "__main__":
    (Path(__file__).resolve().parents[2] / "target").mkdir(exist_ok=True)
    unittest.main()


class MergeWithMainTests(unittest.TestCase):
    """The gate tests a branch merged with current main, and refuses one that
    cannot merge before building anything (#866). Real git, a fake cargo."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=Path(__file__).resolve().parents[2] / "target")
        self.addCleanup(self.temp.cleanup)
        top = Path(self.temp.name)
        self.bin = top / "bin"
        self.bin.mkdir()
        self.origin = top / "origin.git"
        self.root = top / "work"
        self.other = top / "other"
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("KLIPP_", "CARGO_", "GIT_")) and k != "TMPDIR"}
        self.env.update(PATH=f"{self.bin}:{os.environ['PATH']}", TEST_ROOT=str(top), GIT_AUTHOR_NAME="T", GIT_AUTHOR_EMAIL="t@t",
                        GIT_COMMITTER_NAME="T", GIT_COMMITTER_EMAIL="t@t", GIT_CONFIG_NOSYSTEM="1", HOME=str(top))
        # cargo records what the tree it builds holds: the shared file's
        # text and whether main's new file is there.
        self.script(self.bin / "cargo", 'if [ "$1" = clippy ]; then printf "%s|%s\\n" "$(tr "\\n" , < shared.txt)" "$( [ -e from-main.txt ] && echo main || echo branch)" >> "$TEST_ROOT/built"; fi\nexit 0')
        self.script(self.bin / "pgrep", "exit 1")
        self.script(self.bin / "df", 'printf "Filesystem 1024-blocks Used Available Capacity Mounted on\\nfixture 9000000 1 8000000 1%% /scratch\\n"')
        self.git(top, "init", "-q", "--bare", "-b", "main", str(self.origin))
        self.git(top, "clone", "-q", str(self.origin), str(self.root))
        (self.root / "scripts").mkdir()
        shutil.copy(Path(__file__).resolve().parents[1] / "pre-push.sh", self.root / "scripts/pre-push.sh")
        self.script(self.root / "scripts/check-signoff.sh", "exit 0")
        (self.root / "shared.txt").write_text("one\n")
        (self.root / ".gitignore").write_text("target/\n")
        self.commit(self.root, "start")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        self.git(self.root, "checkout", "-q", "-b", "fix/branch")
        self.git(top, "clone", "-q", str(self.origin), str(self.other))

    def git(self, cwd, *args):
        return subprocess.run(["git", *args], cwd=cwd, env=self.env, check=True, capture_output=True, text=True).stdout

    def commit(self, cwd, message):
        self.git(cwd, "add", "-A")
        self.git(cwd, "commit", "-q", "-m", message)

    def script(self, path, body):
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def main_moves_on(self, name="from-main.txt", text="main\n"):
        (self.other / name).write_text(text)
        self.commit(self.other, "main moves on")
        self.git(self.other, "push", "-q", "origin", "HEAD:main")

    def run_gate(self):
        return subprocess.run(["sh", str(self.root / "scripts/pre-push.sh")], cwd=self.root, env=self.env, text=True, capture_output=True, stdin=subprocess.DEVNULL)

    def built(self):
        path = Path(self.temp.name) / "built"
        return path.read_text().splitlines() if path.exists() else []

    def test_a_branch_behind_main_is_tested_merged_with_it_and_put_back(self):
        (self.root / "shared.txt").write_text("one\nbranch\n")
        self.commit(self.root, "the branch's change")
        pushed = self.git(self.root, "rev-parse", "HEAD")
        self.main_moves_on()
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.built(), ["one,branch,|main"], "built with main's new file and the branch's change")
        self.assertIn("testing it merged with main", result.stdout)
        self.assertEqual(self.git(self.root, "rev-parse", "HEAD"), pushed, "the branch as it was pushed")
        self.assertEqual(self.git(self.root, "status", "--porcelain"), "", "nothing left of the merge")
        self.assertFalse((self.root / "from-main.txt").exists())

    def test_a_branch_that_cannot_merge_is_refused_before_building(self):
        (self.root / "shared.txt").write_text("branch\n")
        self.commit(self.root, "the branch's change")
        self.main_moves_on("shared.txt", "main\n")
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("no longer merges cleanly onto main", result.stderr)
        self.assertIn("conflict: shared.txt", result.stderr)
        self.assertEqual(self.built(), [], "nothing was built")
        self.assertEqual(self.git(self.root, "status", "--porcelain"), "")

    def test_a_branch_on_main_and_a_failing_gate_are_left_as_they_are(self):
        (self.root / "shared.txt").write_text("one\nbranch\n")
        self.commit(self.root, "the branch's change")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.built(), ["one,branch,|branch"], "on main already: tested as it is")
        self.assertNotIn("merged with main", result.stdout)
        # A gate that fails, tested merged: the branch is still put back.
        self.main_moves_on()
        self.script(self.bin / "cargo", 'if [ "$1" = clippy ]; then exit 1; fi\nexit 0')
        pushed = self.git(self.root, "rev-parse", "HEAD")
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(self.git(self.root, "rev-parse", "HEAD"), pushed)
        self.assertEqual(self.git(self.root, "status", "--porcelain"), "")
        self.assertFalse((self.root / "from-main.txt").exists())
