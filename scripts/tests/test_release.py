"""Release validation and updater execution, isolated from installed software."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
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


if __name__ == "__main__":
    unittest.main()
