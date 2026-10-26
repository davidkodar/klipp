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
        # Under target/, which a fresh checkout does not have yet.
        target = Path(__file__).resolve().parents[2] / "target"
        target.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=target)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.root / "scripts").mkdir()
        shutil.copy(Path(__file__).resolve().parents[1] / "pre-push.sh", self.root / "scripts/pre-push.sh")
        self.script(self.root / "scripts/check-signoff.sh", "exit 0")
        self.script(self.bin / "git", 'case "$*" in\n "rev-parse --show-toplevel") printf "%s\\n" "$TEST_ROOT";;\n "rev-parse --path-format=absolute --git-common-dir") [ -n "$TEST_GITDIR" ] && printf "%s\\n" "$TEST_GITDIR";;\n "rev-parse --verify HEAD") echo abc1234;;\n "rev-parse --short HEAD") echo abc1234;;\n "symbolic-ref --short -q HEAD") echo fix/scratch;;\nesac')
        self.gitdir = self.root / "gitdir"
        self.gitdir.mkdir()
        self.script(self.bin / "pgrep", "exit 1")
        self.script(self.bin / "df", 'printf "Filesystem 1024-blocks Used Available Capacity Mounted on\\nfixture 9000000 1 %s 1%% /scratch\\n" "$TEST_FREE"')
        self.script(self.bin / "cargo", 'printf "%s|%s\\n" "$*" "$TMPDIR" >> "$TEST_ROOT/cargo-calls"\nfor fd in 8 9; do [ -e "/proc/$$/fd/$fd" ] && echo "$fd $*" >> "$TEST_ROOT/cargo-held-fd"; done\nif [ "$1" = clippy ]; then\n printf "%s\\n" "${TEST_TAG:-gate}" >> "$TEST_ROOT/gate-order"\n [ -n "$TEST_HOLD" ] && sleep "$TEST_HOLD"\nfi\nif [ "$*" = "test --workspace" ]; then exit "${TEST_EXIT:-0}"; fi')
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("KLIPP_", "CARGO_")) and k != "TMPDIR"}
        self.env.update(PATH=f"{self.bin}:{os.environ['PATH']}", TEST_ROOT=str(self.root), TEST_FREE="8000000", TEST_GITDIR=str(self.gitdir),
                        # The local pre-gate stamp (#1341) has tests of its own below.
                        KLIPP_SKIP_PRE_GATE="the gate's own tests")
        # cargo-deny is there and its advisory list was fetched just now,
        # so a gate that does not test the audit passes it (#928): the
        # fake cargo answers `cargo deny` with nothing and success.
        self.script(self.bin / "cargo-deny", "exit 0")
        self.advisory_db(age_days=0)

    def advisory_db(self, age_days):
        """The advisory list on disk, last fetched `age_days` ago."""
        fetched = self.root / "advisory-db" / "advisory-db-1" / ".git" / "FETCH_HEAD"
        fetched.parent.mkdir(parents=True, exist_ok=True)
        fetched.write_text("")
        when = time.time() - age_days * 86400
        os.utime(fetched, (when, when))
        self.env["KLIPP_ADVISORY_DB"] = str(self.root / "advisory-db")

    def script(self, path, body):
        path.write_text("#!/bin/sh\n" + body + "\n")
        path.chmod(0o755)

    def run_gate(self):
        # No stdin: the gate reads the pushed refs from it when it is not a
        # terminal, and an inherited open pipe would keep it waiting.
        return subprocess.run(["sh", str(self.root / "scripts/pre-push.sh")], env=self.env, text=True, capture_output=True, stdin=subprocess.DEVNULL)

    # The gate under test, started in the background so several of them
    # can be queued at once. Its pid is the pid the script sees as $$, and
    # its diagnostics go to a file so they can be read while it waits.
    def start_gate(self, tag, **extra):
        errors = self.root / f"stderr-{tag}"
        handle = errors.open("w")
        self.addCleanup(handle.close)
        gate = subprocess.Popen(["sh", str(self.root / "scripts/pre-push.sh")],
                                env=dict(self.env, TEST_TAG=tag, **extra),
                                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=handle)
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

    def test_an_audit_that_cannot_run_refuses_the_push(self):
        # #928: a check that could not finish passed nothing, so the push
        # does not go on as if it had.
        self.with_cargo_deny("error: failed to open advisory database\n", status=1, fetch=1)
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("could not be refreshed", result.stderr)
        self.assertIn("could not check the advisories — push refused", result.stderr)
        self.assertIn("KLIPP_SKIP_AUDIT=1", result.stderr)
        self.assertFalse((self.root / "gate-order").exists(), "refused before the build")

    def test_offline_the_list_on_disk_is_used_and_its_age_said(self):
        self.with_cargo_deny("advisories ok\n", fetch=1)
        self.advisory_db(age_days=10)
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("could not be refreshed", result.stderr)
        self.assertIn("10 days old", result.stderr)

    def test_a_list_over_a_month_old_refuses_the_push(self):
        self.with_cargo_deny("advisories ok\n", fetch=1)
        self.advisory_db(age_days=40)
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("40 days old — push refused", result.stderr)
        self.assertIn("cargo deny fetch db", result.stderr)

    def test_the_list_is_found_where_deny_toml_keeps_it(self):
        # The age is read from deny.toml's db-path, not a fixed folder: an
        # old list there is refused (#949 review).
        self.with_cargo_deny("advisories ok\n", fetch=1)
        self.advisory_db(age_days=40)
        kept = self.env.pop("KLIPP_ADVISORY_DB")
        (self.root / "deny.toml").write_text(f'[advisories]\ndb-path = "{kept}"\n')
        result = self.run_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("40 days old", result.stderr)

    def test_a_missing_list_is_said(self):
        self.with_cargo_deny("advisories ok\n")
        self.env["KLIPP_ADVISORY_DB"] = str(self.root / "nowhere")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("age is not known", result.stderr)

    def test_skipping_the_audit_is_said_and_recorded(self):
        # The opt-out for a machine that cannot check: the push goes on, and
        # its line in the gate history says it was not checked.
        self.with_cargo_deny(self.ADVISORY, status=1)
        self.env["KLIPP_SKIP_AUDIT"] = "1"
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("not checked for advisories", result.stderr)
        lines = (self.gitdir / "klipp-pre-push.history").read_text().splitlines()
        self.assertEqual(lines[-1].split()[2], "pass")
        self.assertEqual(lines[-1].split()[-1], "unaudited")

    def test_an_audited_push_has_no_mark_in_the_history(self):
        self.with_cargo_deny("advisories ok\n")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        line = (self.gitdir / "klipp-pre-push.history").read_text().splitlines()[-1]
        self.assertEqual(len(line.split()), 7, line)

    def test_without_cargo_deny_the_push_is_refused_and_says_how_to_get_it(self):
        # The system's own tools without cargo-deny, so one installed by
        # the distribution in /usr/bin is not found either.
        tools = self.root / "system-tools"
        tools.mkdir()
        linked = {"cargo-deny"}
        for system in ("/usr/bin", "/bin"):
            for entry in os.scandir(system):
                if entry.name not in linked:
                    linked.add(entry.name)
                    (tools / entry.name).symlink_to(entry.path)
        (self.bin / "cargo-deny").unlink()
        self.env["PATH"] = f"{self.bin}:{tools}"
        result = self.run_gate()
        self.assertEqual(result.returncode, 1, "#928: without the checker the push is refused")
        self.assertIn("cargo install --locked cargo-deny", result.stderr)
        self.assertIn("KLIPP_SKIP_AUDIT=1", result.stderr)

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
        # A pid known to be gone: a process started and waited for. A
        # fixed one may be a live process on the machine running this.
        ended = subprocess.Popen(["true"])
        ended.wait()
        gone = ended.pid
        self.hold_the_gate_lock(f"{gone} fix/other /elsewhere 0\n")
        waiter = self.start_gate("waiter")
        self.until(lambda: "in the queue" in waiter.said(), "the gate to report its position")
        self.assertEqual(self.queued_pids(), [str(waiter.pid)])
        waiter.terminate()
        self.assertEqual(waiter.wait(timeout=30), 143, waiter.said())
        # The holder record names a pid that is gone, and is reported as a
        # record rather than as a fact about who holds the lock.
        self.assertIn("1st in the queue, waiting for a gate outside the queue: "
                      f"{gone} fix/other /elsewhere", waiter.said())
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



class RealGitGate(unittest.TestCase):
    """A clone of a scratch origin, on a branch, with the gate copied in:
    real git, a fake cargo."""

    def setUp(self):
        # Under target/, which a fresh checkout does not have yet.
        target = Path(__file__).resolve().parents[2] / "target"
        target.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=target)
        self.addCleanup(self.temp.cleanup)
        top = Path(self.temp.name)
        self.bin = top / "bin"
        self.bin.mkdir()
        self.origin = top / "origin.git"
        self.root = top / "work"
        self.other = top / "other"
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("KLIPP_", "CARGO_", "GIT_")) and k != "TMPDIR"}
        self.env.update(PATH=f"{self.bin}:{os.environ['PATH']}", TEST_ROOT=str(top), KLIPP_SKIP_PRE_GATE="the gate's own tests", GIT_AUTHOR_NAME="T", GIT_AUTHOR_EMAIL="t@t",
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
        shutil.copy(Path(__file__).resolve().parents[1] / "affected_crates.py", self.root / "scripts/affected_crates.py")
        shutil.copy(Path(__file__).resolve().parents[1] / "stale_files.py", self.root / "scripts/stale_files.py")
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


class PushedCommitIsCheckedOutTests(RealGitGate):
    """The gate tests what is checked out, so a branch pushed from a checkout
    that has another one out would go up untested: it is refused (#1118)."""

    def run_gate_with(self, refs):
        return subprocess.run(["sh", str(self.root / "scripts/pre-push.sh")], cwd=self.root, env=self.env, text=True, capture_output=True, input=refs)

    def test_a_branch_that_is_not_checked_out_is_refused_before_anything_is_built(self):
        (self.root / "shared.txt").write_text("one\nbranch\n")
        self.commit(self.root, "the branch's change")
        main = self.git(self.root, "rev-parse", "origin/main").strip()
        result = self.run_gate_with(f"refs/heads/main {main} refs/heads/main {'0' * 40}\n")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("refs/heads/main is not the commit checked out here", result.stderr)
        self.assertIn("#1118", result.stderr)
        self.assertEqual(self.built(), [], "nothing built")

    def test_the_checked_out_branch_goes_through(self):
        (self.root / "shared.txt").write_text("one\nbranch\n")
        self.commit(self.root, "the branch's change")
        head = self.git(self.root, "rev-parse", "HEAD").strip()
        result = self.run_gate_with(f"refs/heads/fix/branch {head} refs/heads/fix/branch {'0' * 40}\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(self.built()), 1, "built once")

    def test_a_tag_is_not_held_to_the_checkout(self):
        main = self.git(self.root, "rev-parse", "origin/main").strip()
        (self.root / "shared.txt").write_text("one\nbranch\n")
        self.commit(self.root, "the branch's change")
        result = self.run_gate_with(f"refs/tags/v0 {main} refs/tags/v0 {'0' * 40}\n")
        self.assertNotIn("#1118", result.stderr)

    def test_a_tag_pushed_onto_a_branch_is_held_to_the_checkout(self):
        # The remote ref decides: an old tag pushed to a branch brings its
        # untested commit into that branch.
        main = self.git(self.root, "rev-parse", "origin/main").strip()
        (self.root / "shared.txt").write_text("one\nbranch\n")
        self.commit(self.root, "the branch's change")
        result = self.run_gate_with(f"refs/tags/old {main} refs/heads/release {'0' * 40}\n")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("#1118", result.stderr)
        self.assertEqual(self.built(), [], "nothing built")

    def test_a_branch_pushed_as_a_tag_is_not_held_to_the_checkout(self):
        main = self.git(self.root, "rev-parse", "origin/main").strip()
        (self.root / "shared.txt").write_text("one\nbranch\n")
        self.commit(self.root, "the branch's change")
        result = self.run_gate_with(f"refs/heads/main {main} refs/tags/snapshot {'0' * 40}\n")
        self.assertNotIn("#1118", result.stderr)


class RefStabilityTests(RealGitGate):
    """The original checkout, not the gate checkout, must stay at its head."""

    run_gate_with = PushedCommitIsCheckedOutTests.run_gate_with

    def mutate_during_gate(self, command):
        self.env["TEST_PUSH_ROOT"] = str(self.root)
        self.script(self.bin / "cargo",
                    'if [ "$1" = clippy ]; then\n'
                    '  cd "$TEST_PUSH_ROOT" || exit 1\n'
                    + command + '\nfi\nexit 0')

    def test_commit_during_gate_refuses_push(self):
        head = self.git(self.root, "rev-parse", "HEAD").strip()
        self.mutate_during_gate('git commit -q --allow-empty -m "during gate" || exit 1')
        result = self.run_gate_with(f"refs/heads/fix/branch {head} refs/heads/fix/branch {'0' * 40}\n")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("HEAD moved during the gate", result.stderr)
        self.assertNotEqual(self.git(self.root, "rev-parse", "HEAD").strip(), head)

    def test_second_ref_moving_is_refused_even_when_head_stays(self):
        head = self.git(self.root, "rev-parse", "HEAD").strip()
        self.git(self.root, "branch", "also-pushed", head)
        self.mutate_during_gate('new=$(git commit-tree HEAD^{tree} -p HEAD -m "other ref") || exit 1\n'
                                'git update-ref refs/heads/also-pushed "$new" || exit 1')
        refs = ''.join(f"refs/heads/{name} {head} refs/heads/{name} {'0' * 40}\n"
                       for name in ("fix/branch", "also-pushed"))
        result = self.run_gate_with(refs)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("refs/heads/also-pushed moved during the gate", result.stderr)
        self.assertEqual(self.git(self.root, "rev-parse", "HEAD").strip(), head)

    def test_deleted_pushed_ref_is_refused(self):
        head = self.git(self.root, "rev-parse", "HEAD").strip()
        self.git(self.root, "branch", "also-pushed", head)
        self.mutate_during_gate('git update-ref -d refs/heads/also-pushed || exit 1')
        result = self.run_gate_with(f"refs/heads/also-pushed {head} refs/heads/also-pushed {'0' * 40}\n")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("refs/heads/also-pushed moved during the gate", result.stderr)

    def test_manual_gate_checks_original_head_too(self):
        self.mutate_during_gate('git commit -q --allow-empty -m "during manual gate" || exit 1')
        result = self.run_gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("HEAD moved during the gate", result.stderr)


class MergeWithMainTests(RealGitGate):
    """The gate tests a branch merged with current main, and refuses one that
    cannot merge before building anything (#866)."""

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

    def test_a_git_that_cannot_answer_tests_the_branch_as_it_is(self):
        # An older git without merge-tree --write-tree exits 129: that is
        # not a conflict, and must not refuse every push.
        real = shutil.which("git", path=os.environ["PATH"])
        self.script(self.bin / "git", f'if [ "$1" = merge-tree ]; then exit 129; fi\nexec "{real}" "$@"')
        (self.root / "shared.txt").write_text("one\nbranch\n")
        self.commit(self.root, "the branch's change")
        self.main_moves_on()
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("could not say whether", result.stderr)
        self.assertEqual(self.built(), ["one,branch,|branch"], "tested as it is")


class GateCheckoutTests(RealGitGate):
    """The gate builds the pushed commit in a checkout of its own, so only
    what changed rebuilds, and leaves the pushing worktree alone (#871)."""

    def setUp(self):
        super().setUp()
        self.gate_dir = Path(self.temp.name) / "work-gate"
        # cargo records where it builds and what the tree holds there.
        self.script(self.bin / "cargo", 'if [ "$1" = clippy ]; then printf "%s|%s\\n" "$(pwd)" "$(tr "\\n" , < shared.txt)" >> "$TEST_ROOT/where"; fi\nexit 0')
        (self.root / "other.txt").write_text("untouched\n")
        self.commit(self.root, "another file")

    def where(self):
        path = Path(self.temp.name) / "where"
        return path.read_text().splitlines() if path.exists() else []

    def test_the_push_is_built_in_the_gate_checkout_and_the_worktree_is_left_alone(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.where(), [f"{self.gate_dir}|one,"], "built in the gate checkout")
        self.assertIn("in the gate checkout", result.stdout)
        self.assertFalse((self.root / "target").exists(), "nothing built in the pushing worktree")
        self.assertEqual(self.git(self.gate_dir, "rev-parse", "HEAD"), self.git(self.root, "rev-parse", "HEAD"))
        history = (self.root / ".git/klipp-pre-push.history").read_text()
        self.assertIn(str(self.root), history, "the history names the worktree pushed from")

    def test_the_next_push_rewrites_only_what_changed(self):
        self.assertEqual(self.run_gate().returncode, 0)
        other = self.gate_dir / "other.txt"
        old = time.time() - 3600
        os.utime(other, (old, old))
        (self.root / "shared.txt").write_text("one\ntwo\n")
        self.commit(self.root, "a change")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.where()[-1], f"{self.gate_dir}|one,two,")
        self.assertAlmostEqual(other.stat().st_mtime, old, delta=1, msg="an unchanged file keeps its time, so cargo keeps its build")

    def test_in_place_when_asked_or_when_the_checkout_cannot_be_used(self):
        self.env["KLIPP_GATE_IN_PLACE"] = "1"
        self.assertEqual(self.run_gate().returncode, 0)
        self.assertEqual(self.where(), [f"{self.root}|one,"])
        del self.env["KLIPP_GATE_IN_PLACE"]
        blocked = Path(self.temp.name) / "a-file"
        blocked.write_text("not a checkout")
        self.env["KLIPP_GATE_DIR"] = str(blocked)
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.where()[-1], f"{self.root}|one,")
        self.assertIn("could not be used", result.stderr)


class TestScopeTests(RealGitGate):
    """The gate runs the tests of the crates a branch touches and of those
    built on them, and the whole suite when it must (#873)."""

    def setUp(self):
        super().setUp()
        # Crate b is built on crate a. cargo answers metadata for the tree
        # it runs in and records each test command.
        for name in ("a", "b"):
            (self.root / "crates" / name / "src").mkdir(parents=True)
            (self.root / "crates" / name / "src/lib.rs").write_text("\n")
        self.commit(self.root, "two crates")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        meta = ('{"workspace_root":"%s","packages":['
                '{"name":"a","manifest_path":"%s/crates/a/Cargo.toml","dependencies":[]},'
                '{"name":"b","manifest_path":"%s/crates/b/Cargo.toml","dependencies":[{"name":"a","path":"%s/crates/a"}]}]}')
        self.script(self.bin / "cargo", f'case "$1" in\n metadata) printf \'{meta}\' "$PWD" "$PWD" "$PWD" "$PWD";;\n test) echo "$*" >> "$TEST_ROOT/tests";;\n doc) echo "$*" >> "$TEST_ROOT/docs";;\nesac\necho "$1 $CARGO_PROFILE_DEV_DEBUG" >> "$TEST_ROOT/debuginfo"\nexit 0')
        self.full_mark = self.root / ".git/klipp-pre-push.full"

    def tests_run(self):
        path = Path(self.temp.name) / "tests"
        lines = path.read_text().splitlines() if path.exists() else []
        path.unlink(missing_ok=True)
        return lines

    def change(self, path, text="changed\n"):
        (self.root / path).write_text(text)
        self.commit(self.root, f"change {path}")

    def test_the_first_gate_runs_everything_and_the_next_only_what_a_change_reaches(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.tests_run(), ["test --workspace --no-run", "test --workspace"])
        self.assertIn("has not passed in 12 hours", result.stdout)
        self.assertTrue(self.full_mark.exists(), "the whole suite's pass is noted")
        self.change("crates/a/src/lib.rs")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.tests_run(), ["test -p a -p b --no-run", "test -p a -p b"], "a and b, which is built on a")
        self.change("crates/b/src/lib.rs")
        self.run_gate()
        self.assertEqual(self.tests_run(), ["test -p a -p b --no-run", "test -p a -p b"], "the branch's changes: a and b")

    def test_documentation_runs_no_tests_and_a_shared_file_runs_them_all(self):
        self.full_mark.write_text(str(int(time.time())))
        self.change("README.md")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.tests_run(), [])
        self.assertIn("no tests to run", result.stdout)
        self.change("Cargo.lock")
        result = self.run_gate()
        self.assertEqual(self.tests_run(), ["test --workspace --no-run", "test --workspace"])
        self.assertIn("Cargo.lock is shared", result.stdout)

    def test_documentation_gate_rechecks_head_before_success(self):
        self.full_mark.write_text(str(int(time.time())))
        self.change("README.md")
        self.env["TEST_PUSH_ROOT"] = str(self.root)
        cargo = self.bin / "cargo"
        cargo.write_text(cargo.read_text().replace(
            ' doc) echo',
            ' doc) git -C "$TEST_PUSH_ROOT" commit -q --allow-empty -m "during docs" || exit 1; echo'))
        result = self.run_gate()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("no tests to run", result.stdout)
        self.assertIn("HEAD moved during the gate", result.stderr)
        self.assertEqual(self.tests_run(), [])

    def logged(self, name):
        path = Path(self.temp.name) / name
        lines = path.read_text().splitlines() if path.exists() else []
        path.unlink(missing_ok=True)
        return lines

    def test_rustdoc_documents_the_crates_whose_tests_run(self):
        # The scope is the branch's changes against main, so each step
        # adds to what the one before it changed.
        # A larger compile-job budget must not raise rustdoc concurrency.
        self.env["CARGO_BUILD_JOBS"] = "8"
        self.full_mark.write_text(str(int(time.time())))
        self.change("README.md")
        self.run_gate()
        self.assertEqual(self.logged("docs"), ["doc -j 1 --workspace --no-deps"], "no tests to run: documented whole")
        self.change("crates/b/src/lib.rs")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.logged("docs"), ["doc -j 1 -p b --no-deps"], "b: nothing is built on it")
        self.assertIn("rustdoc took", result.stdout)
        self.change("Cargo.lock")
        self.run_gate()
        self.assertEqual(self.logged("docs"), ["doc -j 1 --workspace --no-deps"], "a shared file: everything")

    def test_rustdoc_has_its_own_explicit_job_override(self):
        self.env.update(CARGO_BUILD_JOBS="8", KLIPP_GATE_DOC_JOBS="3")
        self.full_mark.write_text(str(int(time.time())))
        self.change("README.md")
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.logged("docs"), ["doc -j 3 --workspace --no-deps"])

    def test_rustdoc_refuses_an_invalid_job_override(self):
        self.full_mark.write_text(str(int(time.time())))
        self.change("README.md")
        for jobs in ("0", "-1", "many"):
            with self.subTest(jobs=jobs):
                self.env["KLIPP_GATE_DOC_JOBS"] = jobs
                result = self.run_gate()
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn("KLIPP_GATE_DOC_JOBS must be a positive integer", result.stderr)
                self.assertEqual(self.logged("docs"), [])
                self.assertEqual(self.logged("debuginfo"), [], "no cargo command before validation")

    def test_everything_the_gate_builds_has_line_tables_only(self):
        self.full_mark.write_text(str(int(time.time())))
        self.change("crates/a/src/lib.rs")
        self.run_gate()
        calls = [c for c in self.logged("debuginfo") if c.split()[0] in ("clippy", "doc", "test")]
        self.assertTrue(calls, "cargo was called")
        for call in calls:
            self.assertEqual(call.split()[1:], ["line-tables-only"], call)

    def test_a_stale_full_run_or_klipp_gate_full_runs_everything(self):
        self.full_mark.write_text(str(int(time.time()) - 13 * 3600))
        self.change("crates/b/src/lib.rs")
        self.run_gate()
        self.assertEqual(self.tests_run()[-1], "test --workspace", "no full pass in 12 hours")
        self.env["KLIPP_GATE_FULL"] = "1"
        self.run_gate()
        self.assertEqual(self.tests_run()[-1], "test --workspace", "asked for")


class PreGateTests(RealGitGate):
    """A push needs its tree to have passed scripts/pre-gate.sh, the cheap
    local checks run first (#1341): a merge check, a compile of what the
    change can affect, the touched crates' tests to the end and their
    clippy. Only a pass of a committed tree is stamped."""

    run_gate_with = PushedCommitIsCheckedOutTests.run_gate_with

    def setUp(self):
        super().setUp()
        del self.env["KLIPP_SKIP_PRE_GATE"]
        shutil.copy(Path(__file__).resolve().parents[1] / "pre-gate.sh", self.root / "scripts/pre-gate.sh")
        # Crate b is built on crate a; klipp-graph-lift holds a scan.
        for name in ("a", "b", "klipp-graph-lift"):
            (self.root / "crates" / name / "src").mkdir(parents=True)
            (self.root / "crates" / name / "src/lib.rs").write_text("\n")
            (self.root / "crates" / name / "Cargo.toml").write_text(f'[package]\nname = "{name}"\n')
        (self.root / "crates/klipp-graph-lift/tests").mkdir()
        (self.root / "crates/klipp-graph-lift/tests/effect_entry_readers.rs").write_text("\n")
        self.commit(self.root, "crates")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        self.git(self.other, "pull", "-q", "origin", "main")
        meta = ('{"workspace_root":"%s","packages":['
                '{"name":"a","manifest_path":"%s/crates/a/Cargo.toml","dependencies":[]},'
                '{"name":"b","manifest_path":"%s/crates/b/Cargo.toml","dependencies":[{"name":"a","path":"%s/crates/a"}]},'
                '{"name":"klipp-graph-lift","manifest_path":"%s/crates/klipp-graph-lift/Cargo.toml","dependencies":[]}]}')
        self.script(self.bin / "cargo", f'case "$1" in\n metadata) [ -n "$TEST_NO_METADATA" ] && exit 1; printf \'{meta}\' "$PWD" "$PWD" "$PWD" "$PWD" "$PWD";;\n *) echo "$*" >> "$TEST_ROOT/cargo-calls";;\nesac\n'
                    'if [ "$1" = test ] && [ -n "$TEST_FAIL" ]; then echo "test broken ... FAILED"; exit 101; fi\nexit 0')
        self.change("crates/a/src/lib.rs")

    def change(self, path, text="changed\n"):
        (self.root / path).write_text(text)
        self.commit(self.root, f"change {path}")

    def calls(self):
        path = Path(self.temp.name) / "cargo-calls"
        return path.read_text().splitlines() if path.exists() else []

    def push_refs(self):
        head = self.git(self.root, "rev-parse", "HEAD").strip()
        return f"refs/heads/fix/branch {head} refs/heads/fix/branch {'0' * 40}\n"

    def pre_gate(self):
        return subprocess.run(["sh", str(self.root / "scripts/pre-gate.sh")], cwd=self.root, env=self.env, text=True, capture_output=True, stdin=subprocess.DEVNULL)

    def stamp(self):
        tree = self.git(self.root, "rev-parse", "HEAD^{tree}").strip()
        return self.root / ".git" / "klipp-pre-gate" / tree

    def test_a_push_without_a_stamp_is_refused_before_anything_is_built(self):
        result = self.run_gate_with(self.push_refs())
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("has not passed scripts/pre-gate.sh", result.stderr)
        self.assertEqual(self.calls(), [], "nothing built")

    def test_a_run_by_hand_with_no_refs_needs_no_stamp(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("pre-gate.sh", result.stderr)
        self.assertIn("clippy --workspace --all-targets -- -D warnings", self.calls(), "the checks ran")

    def test_a_stamped_head_does_not_let_another_unstamped_commit_through(self):
        self.assertEqual(self.pre_gate().returncode, 0)
        stamped = self.git(self.root, "rev-parse", "HEAD").strip()
        self.git(self.root, "checkout", "-q", "-b", "other", "HEAD~1")
        (self.root / "crates/b/src/lib.rs").write_text("unstamped\n")
        self.commit(self.root, "an unstamped commit")
        unstamped = self.git(self.root, "rev-parse", "HEAD").strip()
        self.git(self.root, "checkout", "-q", "fix/branch")
        self.assertEqual(self.git(self.root, "rev-parse", "HEAD").strip(), stamped)
        (Path(self.temp.name) / "cargo-calls").unlink(missing_ok=True)
        result = self.run_gate_with(f"refs/heads/other {unstamped} refs/heads/other {'0' * 40}\n")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(self.calls(), [], "nothing built")

    def test_a_third_heavy_job_needs_a_reason_and_is_kept(self):
        # Two other cargo jobs, as pgrep and /proc show them.
        sleepers = [subprocess.Popen(["sleep", "30"], cwd=d) for d in (self.other, Path(self.temp.name))]
        self.addCleanup(lambda: [p.kill() for p in sleepers])
        pids = " ".join(str(p.pid) for p in sleepers)
        self.script(self.bin / "pgrep", f'[ "$1 $2" = "-x cargo" ] && echo "{pids}" | tr " " "\\n"')
        result = self.pre_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("two cargo jobs are already running", result.stderr)
        self.env["KLIPP_PRE_GATE_FORCE"] = "1"
        self.assertIn("give the reason", self.pre_gate().stderr)
        self.env["KLIPP_PRE_GATE_FORCE"] = "the lead asked for it now"
        result = self.pre_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("starting a THIRD heavy job: the lead asked for it now", result.stdout)
        self.assertIn("forced a third job: the lead asked for it now", (self.root / ".git/klipp-pre-gate/skips.log").read_text())

    def test_a_deletion_needs_no_stamp(self):
        result = self.run_gate_with(f"(delete) {'0' * 40} refs/heads/old {'1' * 40}\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("pre-gate.sh", result.stderr)

    def test_a_skip_needs_a_reason_and_the_reason_is_kept(self):
        self.env["KLIPP_SKIP_PRE_GATE"] = "1"
        result = self.run_gate_with(self.push_refs())
        self.assertEqual(result.returncode, 1)
        self.assertIn("needs the reason", result.stderr)
        self.env["KLIPP_SKIP_PRE_GATE"] = "a one-line doc fix"
        result = self.run_gate_with(self.push_refs())
        self.assertIn("pushed WITHOUT scripts/pre-gate.sh: a one-line doc fix", result.stderr)
        self.assertIn("a one-line doc fix", (self.root / ".git/klipp-pre-gate/skips.log").read_text())
        self.assertNotEqual(self.calls(), [], "the gate ran")

    def test_a_pass_stamps_the_tree_and_the_push_goes_on_to_the_gate(self):
        result = self.pre_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("PASSED", result.stdout)
        self.assertIn(self.git(self.root, "rev-parse", "HEAD").strip(), self.stamp().read_text())
        result = self.run_gate_with(self.push_refs())
        self.assertNotIn("has not passed scripts/pre-gate.sh", result.stderr)
        self.assertIn("clippy --workspace --all-targets -- -D warnings", self.calls(), "the gate ran")

    def test_it_tests_what_the_gate_will_test(self):
        # The gate tests a and every crate built on it: testing a alone
        # let b's tests fail first in the gate, after its whole build.
        self.assertEqual(self.pre_gate().returncode, 0)
        calls = self.calls()
        self.assertIn("check --all-targets -p a -p b", calls, "b is built on a: compiled")
        self.assertIn("test --no-fail-fast -p a -p b", calls, "the gate's crates' tests, to the end")
        self.assertIn("clippy --all-targets -p a -- -D warnings", calls)
        self.assertIn("doc -j 1 --no-deps -p a", calls, "rustdoc on the touched crate, one job")
        self.assertFalse(any(c.startswith("doc") and "-p b" in c or "--workspace" in c for c in calls), "rustdoc of what it reaches and workspace clippy are the gate's")
        self.assertFalse(any("effect_entry_readers" in c for c in calls), "a is not an effect or grade crate")

    def test_an_effect_crate_change_runs_the_scans_it_could_break(self):
        for name in ("klipp-effects",):
            (self.root / "crates" / name / "src").mkdir(parents=True)
            (self.root / "crates" / name / "Cargo.toml").write_text(f'[package]\nname = "{name}"\n')
        self.change("crates/klipp-effects/src/lib.rs")
        # Include the new crate in metadata so the scope stays narrow.
        cargo = self.bin / "cargo"
        package = '{"name":"klipp-effects","manifest_path":"' + str(self.root / "crates/klipp-effects/Cargo.toml") + '","dependencies":[]},'
        cargo.write_text(cargo.read_text().replace('"packages":[', '"packages":[' + package))
        self.assertEqual(self.pre_gate().returncode, 0)
        self.assertIn("test -p klipp-graph-lift --test effect_entry_readers", self.calls())

    def test_a_bundled_scan_uses_its_registered_target(self):
        (self.root / "crates/klipp-compose/tests").mkdir(parents=True)
        (self.root / "crates/klipp-compose/tests/legacy_route_gone.rs").write_text("\n")
        self.commit(self.root, "baseline bundled scan")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        (self.root / "crates/klipp-effects/src").mkdir(parents=True)
        (self.root / "crates/klipp-effects/Cargo.toml").write_text('[package]\nname = "klipp-effects"\n')
        self.change("crates/klipp-effects/src/lib.rs")
        # Include the new crate in metadata so the scope stays narrow.
        cargo = self.bin / "cargo"
        package = '{"name":"klipp-effects","manifest_path":"' + str(self.root / "crates/klipp-effects/Cargo.toml") + '","dependencies":[]},'
        cargo.write_text(cargo.read_text().replace('"packages":[', '"packages":[' + package))
        result = self.pre_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("test -p klipp-compose --test all legacy_route_gone::", self.calls())
        self.assertNotIn("test -p klipp-compose --test legacy_route_gone", self.calls())

    def test_workspace_suite_does_not_repeat_source_scans(self):
        (self.root / "crates/klipp-effects/src").mkdir(parents=True)
        (self.root / "crates/klipp-effects/Cargo.toml").write_text('[package]\nname = "klipp-effects"\n')
        self.change("crates/klipp-effects/src/lib.rs")
        self.change("Cargo.toml", "# shared workspace change\n")
        result = self.pre_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("test --no-fail-fast --workspace", self.calls())
        self.assertFalse(any("effect_entry_readers" in c for c in self.calls()))

    def test_a_pixel_crate_change_checks_the_render_references(self):
        # golden_corpus lives in klipp-compose, downstream of the effect
        # crates: a changed effect passes its own crate and fails only it.
        for name in ("klipp-effects", "klipp-title"):
            (self.root / f"crates/{name}/src").mkdir(parents=True)
            (self.root / f"crates/{name}/Cargo.toml").write_text(f'[package]\nname = "{name}"\n')
        (self.root / "crates/klipp-compose/tests").mkdir(parents=True)
        (self.root / "crates/klipp-compose/tests/golden_corpus.rs").write_text("\n")
        # Compose's manifest names its klipp dependencies; it isn't a crate
        # this test touches (no src), so only the dependency is "touched".
        (self.root / "crates/klipp-compose/Cargo.toml").write_text('[package]\nname = "klipp-compose"\n[dependencies]\nklipp-effects = { path = "../klipp-effects" }\nklipp-title = { path = "../klipp-title" }\n')
        # On main already, so the change touches only the dependency.
        self.commit(self.root, "compose and its dependencies")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        for changed in ("crates/klipp-effects/src/lib.rs", "crates/klipp-title/src/lib.rs"):
            self.change(changed, f"// {changed}\n")
            self.assertEqual(self.pre_gate().returncode, 0)
            calls = self.calls()
            # Run on their own, or within the gate's scope when that
            # includes compose (here the whole workspace), never twice.
            ran = [c for c in calls if c == "test -p klipp-compose --test golden_corpus" or c.startswith("test --no-fail-fast") and ("--workspace" in c or "-p klipp-compose" in c)]
            self.assertTrue(ran, (changed, calls))
            if any("--workspace" in c and c.startswith("test --no-fail-fast") for c in calls):
                self.assertNotIn("test -p klipp-compose --test golden_corpus", calls, "already in the workspace's tests")

    def test_an_unreadable_compose_manifest_still_runs_the_render_references(self):
        # Fail closed: when compose's dependencies can't be read, the
        # references run rather than being skipped without a word.
        (self.root / "crates/klipp-compose/tests").mkdir(parents=True)
        (self.root / "crates/klipp-compose/tests/golden_corpus.rs").write_text("\n")
        (self.root / "crates/klipp-compose/Cargo.toml").write_text("this is [not toml\n")
        self.commit(self.root, "a broken compose manifest")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        self.change("crates/a/src/lib.rs", "changed for the manifest test\n")
        result = self.pre_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("test -p klipp-compose --test golden_corpus", self.calls())
        self.assertIn("could not be read", result.stdout)

    def test_a_change_the_conversion_reads_runs_its_render_inputs(self):
        # render_inputs lives in klipp-convert and reads the lift (a
        # dependency) and the timeline's live eval (a dev-dependency): a
        # change in either passes its own crate and fails only it.
        for name in ("klipp-graph-lift", "klipp-timeline"):
            (self.root / f"crates/{name}/src").mkdir(parents=True, exist_ok=True)
            (self.root / f"crates/{name}/Cargo.toml").write_text(f'[package]\nname = "{name}"\n')
        (self.root / "crates/klipp-convert/tests").mkdir(parents=True)
        (self.root / "crates/klipp-convert/tests/render_inputs.rs").write_text("\n")
        (self.root / "crates/klipp-convert/Cargo.toml").write_text('[package]\nname = "klipp-convert"\n[dependencies]\nklipp-graph-lift = { path = "../klipp-graph-lift" }\n[dev-dependencies]\nklipp-timeline = { path = "../klipp-timeline" }\n')
        self.commit(self.root, "convert and what it reads")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        for changed in ("crates/klipp-graph-lift/src/lib.rs", "crates/klipp-timeline/src/lib.rs"):
            self.change(changed, f"// {changed}\n")
            self.assertEqual(self.pre_gate().returncode, 0)
            self.assertIn("test -p klipp-convert --test render_inputs", self.calls(), changed)

    def test_a_change_the_conversion_doesnt_read_skips_its_render_inputs(self):
        (self.root / "crates/klipp-convert/tests").mkdir(parents=True)
        (self.root / "crates/klipp-convert/tests/render_inputs.rs").write_text("\n")
        (self.root / "crates/klipp-convert/Cargo.toml").write_text('[package]\nname = "klipp-convert"\n[dependencies]\nklipp-graph-lift = { path = "../klipp-graph-lift" }\n')
        self.commit(self.root, "convert, built on the lift only")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        self.change("crates/a/src/lib.rs", "changed elsewhere for convert\n")
        self.assertEqual(self.pre_gate().returncode, 0)
        self.assertFalse(any("render_inputs" in c for c in self.calls()))

    def test_a_change_elsewhere_does_not_run_the_render_references(self):
        (self.root / "crates/klipp-compose/tests").mkdir(parents=True)
        (self.root / "crates/klipp-compose/tests/golden_corpus.rs").write_text("\n")
        (self.root / "crates/klipp-compose/Cargo.toml").write_text('[package]\nname = "klipp-compose"\n[dependencies]\nklipp-effects = { path = "../klipp-effects" }\n')
        self.commit(self.root, "compose, built on effects only")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        self.change("crates/a/src/lib.rs", "changed elsewhere\n")
        self.assertEqual(self.pre_gate().returncode, 0)
        self.assertFalse(any("golden_corpus" in c for c in self.calls()))

    def test_a_scripts_only_change_builds_nothing_and_runs_the_scripts_tests(self):
        (self.root / "scripts/tests").mkdir()
        (self.root / "scripts/tests/test_nothing.py").write_text("import unittest\nclass T(unittest.TestCase):\n    def test_it(self): pass\n")
        self.commit(self.root, "a scripts change")
        self.git(self.root, "push", "-q", "origin", "HEAD:main")
        self.change("scripts/check-signoff.sh", "#!/bin/sh\n# changed\nexit 0\n")
        result = self.pre_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c for c in self.calls() if not c.startswith("metadata")], [], "no cargo")
        self.assertIn("the scripts' tests passed", result.stdout)

    def test_a_new_message_keeps_the_stamp_and_a_code_change_needs_a_new_run(self):
        self.assertEqual(self.pre_gate().returncode, 0)
        self.git(self.root, "commit", "-q", "--amend", "-m", "reworded")
        self.assertTrue(self.stamp().exists(), "the same tree")
        self.change("crates/a/src/lib.rs", "changed again\n")
        self.assertFalse(self.stamp().exists())
        self.assertIn("has not passed", self.run_gate_with(self.push_refs()).stderr)

    def test_failing_tests_stamp_nothing_and_are_shown(self):
        self.env["TEST_FAIL"] = "1"
        result = self.pre_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("FAILED — nothing stamped", result.stderr)
        self.assertIn("test broken ... FAILED", result.stderr)
        self.assertFalse(self.stamp().exists())

    def test_uncommitted_changes_are_refused(self):
        (self.root / "crates/a/src/lib.rs").write_text("not committed\n")
        result = self.pre_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("uncommitted or untracked", result.stderr)
        self.assertEqual(self.calls(), [])

    def test_behind_main_but_merging_cleanly_is_checked(self):
        self.main_moves_on()
        result = self.pre_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("merges cleanly", result.stdout)

    def test_behind_main_in_a_crate_the_gate_tests_needs_a_rebase(self):
        # b is built on a, so the gate tests b: a main commit in b, untested
        # here, could fail the gate after its whole build.
        self.main_moves_on(name="crates/b/src/other.rs", text="main's b\n")
        result = self.pre_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("main has moved in crates the gate will test ( b )", result.stderr)
        self.assertIn("git rebase origin/main", result.stderr)
        self.assertFalse(any(c.startswith("test") for c in self.calls()), "nothing tested")
        self.assertFalse(self.stamp().exists())

    def test_behind_main_with_an_unknown_scope_needs_a_rebase(self):
        # Without cargo's metadata the gate tests the whole workspace, so a
        # main commit in any crate is untested here: never skipped silently.
        self.env["TEST_NO_METADATA"] = "1"
        self.main_moves_on(name="crates/klipp-graph-lift/src/other.rs", text="main's lift\n")
        result = self.pre_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("main has moved in crates the gate will test", result.stderr)

    def test_behind_main_outside_the_gates_scope_is_checked(self):
        self.main_moves_on(name="crates/klipp-graph-lift/src/other.rs", text="main's lift\n")
        result = self.pre_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("merges cleanly", result.stdout)

    def test_a_conflict_with_main_is_refused_and_named(self):
        self.main_moves_on(name="crates/a/src/lib.rs", text="main's line\n")
        result = self.pre_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("conflict: crates/a/src/lib.rs", result.stderr)
        self.assertIn("git rebase origin/main", result.stderr)
        self.assertEqual(self.calls(), [], "nothing built")
        self.assertFalse(self.stamp().exists())

    def test_a_squash_onto_a_moved_main_that_reverts_it_is_refused(self):
        # The branch's tree recorded on top of a main it never saw: main's
        # newer file goes missing, a revert that builds and passes.
        self.main_moves_on()
        self.git(self.root, "fetch", "-q", "origin")
        tree = self.git(self.root, "rev-parse", "HEAD^{tree}").strip()
        squash = self.git(self.root, "commit-tree", tree, "-p", "origin/main", "-m", "squash").strip()
        self.git(self.root, "reset", "-q", "--hard", squash)
        result = self.pre_gate()
        self.assertEqual(result.returncode, 1)
        self.assertIn("from-main.txt", result.stderr)
        self.assertIn("reverting it", result.stderr)
        self.assertEqual(self.calls(), [], "nothing built")
        self.env["KLIPP_PRE_GATE_ALLOW_REVERT"] = "taking main's file out on purpose"
        result = self.pre_gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("allowed a revert: taking main's file out on purpose", (self.root / ".git/klipp-pre-gate/skips.log").read_text())

if __name__ == "__main__":
    (Path(__file__).resolve().parents[2] / "target").mkdir(exist_ok=True)
    unittest.main()
