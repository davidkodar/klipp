#!/usr/bin/env python3
"""What the gate's test scope decides from a change (#873), against a small
made-up workspace."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from affected_crates import decide  # noqa: E402

# core <- timeline <- document <- ui; media stands alone; ui also needs media.
METADATA = {
    "workspace_root": "/w",
    "packages": [
        # core's tests use timeline, which is built on core.
        {"name": "klipp-core", "manifest_path": "/w/crates/klipp-core/Cargo.toml",
         "dependencies": [{"name": "klipp-timeline", "path": "/w/crates/klipp-timeline", "kind": "dev"}]},
        {"name": "klipp-timeline", "manifest_path": "/w/crates/klipp-timeline/Cargo.toml",
         "dependencies": [{"name": "klipp-core", "path": "/w/crates/klipp-core"}, {"name": "serde"}]},
        {"name": "klipp-document", "manifest_path": "/w/crates/klipp-document/Cargo.toml",
         "dependencies": [{"name": "klipp-timeline", "path": "/w/crates/klipp-timeline"}]},
        {"name": "klipp-media", "manifest_path": "/w/crates/klipp-media/Cargo.toml", "dependencies": []},
        {"name": "klipp-ui", "manifest_path": "/w/crates/klipp-ui/Cargo.toml",
         "dependencies": [{"name": "klipp-document", "path": "/w/crates/klipp-document"},
                          {"name": "klipp-media", "path": "/w/crates/klipp-media", "kind": "dev"}]},
    ],
}


def scope(*paths):
    return decide(list(paths), METADATA)


class AffectedCratesTests(unittest.TestCase):
    def test_a_crate_and_everything_that_depends_on_it(self):
        # core's tests use timeline, so they run; nothing spreads further
        # from core, its own code being unchanged.
        self.assertEqual(scope("crates/klipp-timeline/src/lib.rs"), "packages klipp-core klipp-document klipp-timeline klipp-ui")
        self.assertEqual(scope("crates/klipp-core/src/x.rs"), "packages klipp-core klipp-document klipp-timeline klipp-ui")

    def test_a_leaf_is_itself_and_a_dev_dependency_counts(self):
        self.assertEqual(scope("crates/klipp-ui/src/app.rs"), "packages klipp-ui")
        self.assertEqual(scope("crates/klipp-media/src/lib.rs"), "packages klipp-media klipp-ui")

    def test_shared_files_ask_for_the_full_suite(self):
        for path in ("Cargo.lock", "Cargo.toml", "assets/craft/guide.md", "vendor/winit/src/lib.rs", "scripts/pre-push.sh", "test-luts/a.cube"):
            self.assertTrue(scope("crates/klipp-ui/src/app.rs", path).startswith("full "), path)

    def test_an_unknown_path_asks_for_the_full_suite(self):
        self.assertTrue(scope("somewhere/new.rs").startswith("full "))
        self.assertTrue(scope("build.rs").startswith("full "))

    def test_documentation_alone_needs_no_tests(self):
        self.assertTrue(scope("CONTRIBUTING.md", "docs/a.md", "packaging/PKGBUILD").startswith("none "))
        # A crate's own docs are that crate's: its doc tests read them.
        self.assertEqual(scope("crates/klipp-media/README.md"), "packages klipp-media klipp-ui")


if __name__ == "__main__":
    unittest.main()
