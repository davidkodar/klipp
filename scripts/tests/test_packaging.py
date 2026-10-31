#!/usr/bin/env python3
"""Run the Arch package functions against empty, isolated staging directories."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REPO = Path(__file__).resolve().parents[2]


class ArchPackagingTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="klipp package ")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "checkout"
        self.stage = Path(self.temp.name) / "package"
        (self.root / "packaging").mkdir(parents=True)
        self.stage.mkdir()
        shutil.copy(REPO / "packaging/PKGBUILD", self.root / "packaging/PKGBUILD")
        shutil.copy(REPO / "packaging/klipp-update", self.root / "packaging/klipp-update")
        assets = self.root / "assets"
        (assets / "luts/film").mkdir(parents=True)
        (assets / "luts/film/sample.cube").write_text("LUT_3D_SIZE 2\n")
        shutil.copy(REPO / "assets/luts/CREDITS.md", assets / "luts/CREDITS.md")
        shutil.copy(REPO / "assets/klipp.desktop", assets / "klipp.desktop")
        shutil.copytree(REPO / "assets/branding/klipp-refined", assets / "branding/klipp-refined")
        release = self.root / "target/release"
        release.mkdir(parents=True)
        for name in ("klipp-app", "klipp-hdr-out", "klipp-mcp"):
            (release / name).write_text(f"fixture {name}\n")
        self.env = dict(os.environ, startdir=str(self.root / "packaging"), pkgdir=str(self.stage))
        self.bin_dir = self.root / "fake-bin"
        self.bin_dir.mkdir()
        git = self.bin_dir / "git"
        git.write_text('#!/bin/sh\ncase "$*" in\n"rev-list --count HEAD") echo 123;;\n"rev-parse --short HEAD") echo abcdef12;;\n*) exit 1;;\nesac\n')
        git.chmod(0o755)
        self.env["PATH"] = f"{self.bin_dir}:{os.environ['PATH']}"

    def run_function(self, name):
        return subprocess.run(
            ["bash", "-ec", f'source "$startdir/PKGBUILD"; {name}'],
            env=self.env, text=True, capture_output=True,
        )

    def test_package_into_empty_directory_has_all_runtime_assets(self):
        result = self.run_function("package")
        self.assertEqual(result.returncode, 0, result.stderr)
        for command, binary in (("klipp", "klipp-app"), ("klipp-mcp", "klipp-mcp")):
            link = self.stage / "usr/bin" / command
            # Relative, so the link holds wherever the package root is.
            self.assertEqual(os.readlink(link), f"../lib/klipp/{command}")
            installed = (link.parent / link.readlink()).resolve()
            self.assertEqual(installed, (self.stage / f"usr/lib/klipp/{command}").resolve())
            self.assertEqual(installed.read_bytes(), (self.root / "target/release" / binary).read_bytes())
            self.assertTrue(os.access(installed, os.X_OK))
        self.assertTrue(os.access(self.stage / "usr/lib/klipp/klipp-hdr-out", os.X_OK))
        self.assertEqual((self.stage / "usr/lib/klipp/assets/luts/film/sample.cube").read_text(), "LUT_3D_SIZE 2\n")
        self.assertTrue((self.stage / "usr/lib/klipp/assets/luts/CREDITS.md").is_file())
        self.assertEqual((self.stage / "usr/share/applications/klipp.desktop").read_bytes(), (REPO / "assets/klipp.desktop").read_bytes())
        icons = self.stage / "usr/share/icons/hicolor"
        for size in (16, 22, 24, 32, 48, 64, 128, 256, 512, 1024):
            self.assertEqual(
                (icons / f"{size}x{size}/apps/klipp.png").read_bytes(),
                (REPO / f"assets/branding/klipp-refined/klipp-icon-{size}.png").read_bytes(),
            )

    def test_release_package_uses_semver_and_installs_update_helper(self):
        self.env["KLIPP_RELEASE_VERSION"] = "0.20.0-rc.1"
        result = self.run_function('printf "%s %s\\n" "$pkgname" "$pkgver"; package')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "klipp 0.20.0rc.1")
        self.assertTrue(os.access(self.stage / "usr/bin/klipp-update", os.X_OK))
        self.assertEqual(subprocess.check_output(["vercmp", "0.20.0rc.1", "0.20.0"], text=True).strip(), "-1")

    def release_build_env(self, carch):
        """The environment a release build on `carch` hands cargo, as
        makepkg runs it: CARCH set, machine-specific flags inherited."""
        self.env["KLIPP_RELEASE_VERSION"] = "0.20.0"
        self.env["CARCH"] = carch
        self.env["CARGO_ENCODED_RUSTFLAGS"] = "-Ctarget-cpu=native"
        self.env["GGML_NATIVE"] = "ON"
        self.test_build_uses_the_directory_that_package_reads()
        cargo = self.bin_dir / "cargo"
        cargo.write_text('#!/bin/sh\nenv > "$startdir/build-env"\n')
        result = self.run_function("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        env = (self.root / "packaging/build-env").read_text().splitlines()
        rustflags = next(line for line in env if line.startswith("CARGO_ENCODED_RUSTFLAGS=")).split("=", 1)[1].split("\x1f")
        return "\n".join(env), rustflags

    def test_release_overrides_machine_specific_rust_and_native_flags(self):
        env, rustflags = self.release_build_env("x86_64")
        self.assertEqual(rustflags, ["-C", "link-arg=-fuse-ld=lld", "-C", "target-cpu=x86-64"])
        self.assertNotIn("target-cpu=native", env)
        self.assertIn("GGML_NATIVE=OFF", env)
        self.assertIn("GGML_AVX2=OFF", env)
        self.assertIn("CFLAGS=-O2 -march=x86-64 -mtune=generic", env)

    def test_aarch64_release_links_with_lld_for_a_generic_cpu(self):
        env, rustflags = self.release_build_env("aarch64")
        self.assertEqual(rustflags, ["-C", "link-arg=-fuse-ld=lld", "-C", "target-cpu=generic"])
        self.assertIn("GGML_NATIVE=OFF", env)
        self.assertIn("CFLAGS=-O2 -march=armv8-a", env)

    def test_missing_required_icon_fails_packaging(self):
        (self.root / "assets/branding/klipp-refined/klipp-icon-32.png").unlink()
        result = self.run_function("package")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("klipp-icon-32.png", result.stderr)

    def test_initial_version_matches_pkgver_without_rewriting_recipe(self):
        recipe = self.root / "packaging/PKGBUILD"
        before = recipe.read_bytes()
        result = subprocess.run(
            ["bash", "-ec", 'source "$startdir/PKGBUILD"; printf "%s\\n" "$pkgver"; pkgver'],
            env=self.env, text=True, capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ["r123.abcdef12", "r123.abcdef12"])
        self.assertEqual(recipe.read_bytes(), before)

    def test_build_uses_the_directory_that_package_reads(self):
        scripts = self.root / "scripts"
        scripts.mkdir()
        sdk = scripts / "vulkan-sdk.sh"
        sdk.write_text('#!/bin/sh\ntouch "$startdir/sdk-ran"\n')
        sdk.chmod(0o755)
        cargo = self.bin_dir / "cargo"
        cargo.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$startdir/cargo-args"\n')
        cargo.chmod(0o755)
        self.env["CARGO_TARGET_DIR"] = "/unrelated/build-output"
        result = self.run_function("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((self.root / "packaging/sdk-ran").is_file())
        args = (self.root / "packaging/cargo-args").read_text().splitlines()
        self.assertIn("--locked", args)
        self.assertIn("--release", args)
        self.assertEqual(Path(args[args.index("--target-dir") + 1]).resolve(), self.root / "target")
        self.assertEqual([args[i + 1] for i, arg in enumerate(args) if arg == "-p"], ["klipp-app", "klipp-hdr-out", "klipp-mcp"])


if __name__ == "__main__":
    unittest.main()
