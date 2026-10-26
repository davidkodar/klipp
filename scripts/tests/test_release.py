"""Release validation and updater execution, isolated from installed software."""
import argparse
import functools
import hashlib
import http.server
import importlib.util
import os
from pathlib import Path
import re
import shutil
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

    def test_the_test_key_counts_only_on_the_loopback_test_address(self):
        """release_key() itself, run from the shipped script without the
        network: the shipped key unless the test address is a loopback one,
        and only then the test's key."""
        script = (ROOT / "packaging/klipp-update").read_text()
        functions = "\n".join(
            block.group(0) for block in re.finditer(r"^RELEASE_KEY=[^\n]*$|^(?:test_base|release_key)\(\) \{\n.*?^\}$", script, re.M | re.S)
        )
        shipped = release.release_key()

        def key_for(base):
            env = {"PATH": os.environ["PATH"], "KLIPP_UPDATE_TEST_PUBLIC_KEY": "RWQtestkey"}
            if base is not None:
                env["KLIPP_UPDATE_TEST_BASE_URL"] = base
            return subprocess.run(["bash", "-c", functions + "\nrelease_key"], env=env,
                                  capture_output=True, text=True, check=True).stdout.strip()

        for base in (None, "", "http://example.invalid/releases", "https://127.0.0.1:8000", "http://127.0.0.1.example.com"):
            self.assertEqual(key_for(base), shipped, f"base {base!r}")
        for base in ("http://127.0.0.1:8000", "http://localhost:8000/x"):
            self.assertEqual(key_for(base), "RWQtestkey", f"base {base!r}")

    def test_failure_does_not_claim_update_succeeded(self):
        result, _ = self.updater("y\n\n", 1)
        self.assertEqual(result.returncode, 1)
        self.assertIn("failed or was cancelled", result.stdout)
        self.assertNotIn("Update completed", result.stdout)


MINISIGN = shutil.which("minisign")


def minisign_key(directory, name):
    """A throwaway minisign key pair with no passphrase, made for the test:
    the secret key's path and the public key line."""
    secret, public = directory / f"{name}.key", directory / f"{name}.pub"
    subprocess.run([MINISIGN, "-G", "-W", "-s", str(secret), "-p", str(public)], check=True, capture_output=True)
    return secret, public.read_text().splitlines()[-1]


def minisign_sign(secret, path, comment):
    subprocess.run([MINISIGN, "-S", "-s", str(secret), "-m", str(path), "-t", comment], check=True, capture_output=True)


@unittest.skipUnless(MINISIGN, "minisign is not installed")
class SignTests(unittest.TestCase):
    """release.py sign: the release's SHA256SUMS signed, for its version, by
    the key klipp-update checks with."""

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.out = self.root / "release"
        self.out.mkdir()
        (self.out / "SHA256SUMS").write_text("ab" * 32 + "  klipp-0.1.0-1-x86_64.pkg.tar.zst\n")
        (self.out / "release.json").write_text('{"version": "0.1.0"}')
        self.secret, self.public = minisign_key(self.root, "release")
        self.shipped = release.release_key

    def tearDown(self):
        release.release_key = self.shipped
        self.temp.cleanup()

    def sign(self, public):
        release.release_key = lambda: public
        release.sign(argparse.Namespace(release=self.out, key=str(self.secret)))

    def test_the_sums_are_signed_for_their_version(self):
        self.sign(self.public)
        verified = subprocess.run([MINISIGN, "-V", "-P", self.public, "-m", str(self.out / "SHA256SUMS")],
                                  check=True, capture_output=True, text=True).stdout
        self.assertIn("Trusted comment: klipp 0.1.0", verified.splitlines())

    def test_a_key_the_updater_does_not_check_with_is_refused(self):
        _, other = minisign_key(self.root, "other")
        with self.assertRaisesRegex(ValueError, "RELEASE_KEY"):
            self.sign(other)
        self.assertFalse((self.out / "SHA256SUMS.minisig").exists(), "no signature left to upload")

    def test_no_release_key_signs_nothing(self):
        with self.assertRaisesRegex(ValueError, "no RELEASE_KEY"):
            self.sign("")
        self.assertFalse((self.out / "SHA256SUMS.minisig").exists())

    def test_the_release_key_is_read_from_the_updater(self):
        updater = self.root / "klipp-update"
        updater.write_text(f"#!/bin/bash\nRELEASE_KEY='{self.public}'\n")
        self.assertEqual(self.shipped(updater), self.public)
        shipped = self.shipped(ROOT / "packaging/klipp-update")
        self.assertRegex(shipped, r"^RW[A-Za-z0-9+/]{54}$", "the updater ships a minisign Ed25519 public key")


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args):
        pass


@unittest.skipUnless(MINISIGN, "minisign is not installed")
class PackageUpdateTests(unittest.TestCase):
    """klipp-update VERSION against a local fixture release, with pacman and
    sudo stubbed: what it downloads, checks and hands to pacman. The release
    is signed with a key made for the test, swapped in through the loopback
    test path."""

    VERSION = "0.1.0-rc.9"
    FILE = "klipp-0.1.0rc.9-1-x86_64.pkg.tar.zst"

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.release = self.root / "site" / f"v{self.VERSION}"
        self.release.mkdir(parents=True)
        self.package = b"not really a package, pacman is stubbed"
        (self.release / self.FILE).write_bytes(self.package)
        self.secret, self.public = minisign_key(self.root, "release")
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
                        KLIPP_UPDATE_TEST_BASE_URL=f"http://127.0.0.1:{self.server.server_port}",
                        KLIPP_UPDATE_TEST_PUBLIC_KEY=self.public)

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.temp.cleanup()

    def sums(self, digest, signed_by=None, comment=None):
        """The release's sums for a package of `digest`, signed by the test's
        key or `signed_by`, its trusted comment naming the version."""
        sums = self.release / "SHA256SUMS"
        sums.write_text(f"{digest}  {self.FILE}\nabc  klipp-0.1.0-rc.9-source.tar.gz\n")
        minisign_sign(signed_by or self.secret, sums, comment or f"klipp {self.VERSION}")

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

    def test_the_signature_is_checked_and_shown(self):
        result, _ = self.run_update()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("Signed:   SHA256SUMS by Klipp's release key", result.stdout)

    def test_tampered_sums_are_refused(self):
        sums = self.release / "SHA256SUMS"
        sums.write_text(sums.read_text() + "0" * 64 + "  extra\n")
        result, calls = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("not signed by Klipp's release key", result.stderr)
        self.assertNotIn("Install with pacman", result.stdout)
        self.assert_nothing_left()

    def test_a_missing_signature_is_refused(self):
        (self.release / "SHA256SUMS.minisig").unlink()
        result, calls = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("has no signature", result.stderr)
        self.assert_nothing_left()

    def test_a_signature_by_another_key_is_refused(self):
        other, _ = minisign_key(self.root, "other")
        self.sums(hashlib.sha256(self.package).hexdigest(), signed_by=other)
        result, calls = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("not signed by Klipp's release key", result.stderr)

    def test_a_signature_for_another_version_is_refused(self):
        self.sums(hashlib.sha256(self.package).hexdigest(), comment="klipp 0.1.0-rc.8")
        result, calls = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("not for Klipp 0.1.0-rc.9", result.stderr)

    def test_a_test_key_counts_only_on_the_loopback_test_path(self):
        # Off the loopback address the test key is never read, and nothing
        # is downloaded from the address given.
        self.env["KLIPP_UPDATE_TEST_BASE_URL"] = "http://example.invalid/releases"
        result, calls = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("must be a loopback", result.stderr)
        self.assertNotIn("test's key", result.stderr)

    def test_a_wrong_checksum_is_refused(self):
        self.sums("0" * 64)
        result, calls = self.run_update()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertIn("Checksum mismatch", result.stderr)
        self.assertNotIn("Install with pacman", result.stdout)
        self.assert_nothing_left()

    def test_a_missing_checksum_is_refused(self):
        # Signed, so it is the missing line that stops it.
        (self.release / "SHA256SUMS").write_text("abc  something-else\n")
        minisign_sign(self.secret, self.release / "SHA256SUMS", f"klipp {self.VERSION}")
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
        # The test key is read only inside the loopback test path.
        self.assertEqual(script.count("KLIPP_UPDATE_TEST_PUBLIC_KEY"), 3, "named in the comment, the test and the echo only")
        self.assertIn("RELEASES=https://github.com/davidkodar/klipp/releases/download\n", script)
        self.assertIn("--proto '=https'", script)


if __name__ == "__main__":
    unittest.main()
