"""Release validation and updater execution, isolated from installed software."""
import functools
import hashlib
import http.server
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("release", ROOT / "scripts/release.py")
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class ReleaseTests(unittest.TestCase):
    def test_tag_must_match_actual_version(self):
        manifest = '[workspace.package]\nversion="0.20.0-rc.1"\n'
        self.assertEqual(release.release_version("v0.20.0-rc.1", manifest), "0.20.0-rc.1")
        for tag in ("v0.20.0", "v1.0.0", "--bad"):
            with self.assertRaises(ValueError):
                release.release_version(tag, manifest)
        for version in ("01.0.0", "1.0.0-rc.01", "1.0.0+local", "1.0.0-", "1.0.0-rc..1"):
            with self.assertRaises(ValueError):
                release.release_version("v" + version, f'[workspace.package]\nversion="{version}"')

    def updater(self, answer, exit_code=0):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            sudo = root / "sudo"
            sudo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$CALLS"\nexit "$RESULT"\n')
            sudo.chmod(0o755)
            calls = root / "calls"
            env = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}", CALLS=str(calls), RESULT=str(exit_code))
            result = subprocess.run(["bash", str(ROOT / "packaging/klipp-update"), "--terminal"],
                                    input=answer, capture_output=True, text=True, env=env)
            return result, calls.read_text().splitlines() if calls.exists() else []

    def test_update_requires_confirmation(self):
        result, calls = self.updater("n\n")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(calls, [])

    def test_update_is_full_pacman_transaction_with_normal_confirmation(self):
        result, calls = self.updater("y\n\n")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(calls, ["pacman", "-Syu", "klipp"])
        self.assertIn("Close and reopen", result.stdout)

    def test_failure_does_not_claim_update_succeeded(self):
        result, _ = self.updater("y\n\n", 1)
        self.assertEqual(result.returncode, 1)
        self.assertIn("failed or was cancelled", result.stdout)
        self.assertNotIn("Update completed", result.stdout)


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


class PackageUpdateTests(unittest.TestCase):
    """klipp-update VERSION against a local fixture release, with pacman and
    sudo stubbed: what it downloads, checks and hands to pacman."""

    VERSION = "0.1.0-rc.9"
    FILE = "klipp-0.1.0rc.9-1-x86_64.pkg.tar.zst"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.release = self.root / "site" / f"v{self.VERSION}"
        self.release.mkdir(parents=True)
        self.package = b"not really a package, pacman is stubbed"
        (self.release / self.FILE).write_bytes(self.package)
        self.sums(hashlib.sha256(self.package).hexdigest())
        handler = functools.partial(QuietHandler, directory=str(self.root / "site"))
        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        bin_dir = self.root / "bin"
        bin_dir.mkdir()
        # pacman -Qp reports what $PACKAGE says the package holds.
        (bin_dir / "pacman").write_text('#!/bin/sh\n[ "$1" = -Qp ] && { printf "%s\\n" "$PACKAGE"; exit 0; }\nexit 1\n')
        (bin_dir / "sudo").write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$CALLS"\n[ -f "$4" ] && echo present >> "$CALLS"\nexit "$RESULT"\n')
        for stub in ("pacman", "sudo"):
            (bin_dir / stub).chmod(0o755)
        self.env = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", CALLS=str(self.root / "calls"),
                        RESULT="0", PACKAGE="klipp 0.1.0rc.9-1", TMPDIR=str(self.root),
                        KLIPP_UPDATE_TEST_BASE_URL=f"http://127.0.0.1:{self.server.server_port}")

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.temp.cleanup()

    def sums(self, digest):
        (self.release / "SHA256SUMS").write_text(f"{digest}  {self.FILE}\nabc  klipp-0.1.0-rc.9-source.tar.gz\n")

    def run_update(self, answer="y\n\n", version=None):
        result = subprocess.run(["bash", str(ROOT / "packaging/klipp-update"), "--terminal", version or self.VERSION],
                                input=answer, capture_output=True, text=True, env=self.env, timeout=60)
        calls = self.root / "calls"
        return result, calls.read_text().splitlines() if calls.exists() else []

    def assert_nothing_left(self):
        self.assertEqual(list(self.root.glob("klipp-update.*")), [], "the temporary directory is removed")

    def test_a_verified_package_is_installed_with_pacman_after_yes(self):
        result, calls = self.run_update()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls[:3], ["pacman", "-U", "--"])
        self.assertTrue(calls[3].endswith("/" + self.FILE))
        self.assertEqual(calls[4], "present", "pacman is handed the downloaded file")
        for shown in (self.VERSION, self.FILE, hashlib.sha256(self.package).hexdigest(), "Install with pacman? [y/N]"):
            self.assertIn(shown, result.stdout)
        self.assertIn("Update completed", result.stdout)
        self.assert_nothing_left()

    def test_no_installs_nothing(self):
        result, calls = self.run_update("n\n\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("Not installed", result.stdout)
        self.assert_nothing_left()

    def test_a_wrong_checksum_is_refused(self):
        self.sums("0" * 64)
        result, calls = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("Checksum mismatch", result.stderr)
        self.assertNotIn("Install with pacman", result.stdout)
        self.assert_nothing_left()

    def test_a_missing_checksum_is_refused(self):
        (self.release / "SHA256SUMS").write_text("abc  something-else\n")
        result, calls = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("no checksum", result.stderr)

    def test_a_package_of_another_version_or_name_is_refused(self):
        for holds in ("klipp 0.1.0rc.8-1", "klipp-git 0.1.0rc.9-1"):
            self.env["PACKAGE"] = holds
            result, calls = self.run_update()
            self.assertNotEqual(result.returncode, 0, holds)
            self.assertEqual(calls, [], holds)
            self.assertIn(f'holds "{holds}"', result.stderr)
        self.assert_nothing_left()

    def test_a_release_without_the_package_fails_cleanly(self):
        result, calls = self.run_update(version="0.1.0-rc.8")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("Could not download", result.stderr)
        self.assert_nothing_left()

    def test_a_pacman_failure_is_reported(self):
        self.env["RESULT"] = "1"
        result, _ = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Pacman did not install", result.stderr)
        self.assertNotIn("Update completed", result.stdout)

    def test_only_loopback_test_hosts_and_release_versions(self):
        self.env["KLIPP_UPDATE_TEST_BASE_URL"] = "http://example.com"
        result, calls = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("loopback", result.stderr)
        self.env["KLIPP_UPDATE_TEST_BASE_URL"] = f"http://127.0.0.1:{self.server.server_port}"
        for bad in ("../../etc", "0.1.0-rc.9/../x", "1.0"):
            result, calls = self.run_update(version=bad)
            self.assertEqual(result.returncode, 2, bad)
            self.assertEqual(calls, [])

    def test_the_shipped_url_is_github_over_https(self):
        script = (ROOT / "packaging/klipp-update").read_text()
        self.assertIn("RELEASES=https://github.com/davidkodar/klipp/releases/download\n", script)
        self.assertIn("--proto '=https'", script)


if __name__ == "__main__":
    unittest.main()
