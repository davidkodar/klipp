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
        # Cargo.toml and Cargo.lock too, when their texts aren't given.
        for path in ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml", ".cargo/config.toml", "assets/craft/guide.md", "vendor/winit/src/lib.rs", "scripts/pre-push.sh", "test-luts/a.cube"):
            self.assertTrue(scope("crates/klipp-ui/src/app.rs", path).startswith("full "), path)

    def test_an_unknown_path_asks_for_the_full_suite(self):
        self.assertTrue(scope("somewhere/new.rs").startswith("full "))
        self.assertTrue(scope("build.rs").startswith("full "))

    def test_documentation_alone_needs_no_tests(self):
        self.assertTrue(scope("CONTRIBUTING.md", "docs/a.md", "packaging/PKGBUILD").startswith("none "))
        # A crate's own docs are that crate's: its doc tests read them.
        self.assertEqual(scope("crates/klipp-media/README.md"), "packages klipp-media klipp-ui")



# The lockfile of METADATA's workspace: serde is built by timeline, and
# its dependency serde_derive by nothing else; media and ui use nothing.
LOCK = """version = 4

[[package]]
name = "klipp-core"
version = "0.1.0"
dependencies = ["klipp-timeline"]

[[package]]
name = "klipp-timeline"
version = "0.1.0"
dependencies = ["klipp-core", "serde"]

[[package]]
name = "klipp-document"
version = "0.1.0"
dependencies = ["klipp-timeline"]

[[package]]
name = "klipp-media"
version = "0.1.0"

[[package]]
name = "klipp-ui"
version = "0.1.0"
dependencies = ["klipp-document", "klipp-media"]

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = ["serde_derive"]

[[package]]
name = "serde_derive"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
"""

ROOT = """[workspace]
members = ["crates/klipp-core", "crates/klipp-timeline", "crates/klipp-document", "crates/klipp-media", "crates/klipp-ui"]
resolver = "2"

[workspace.dependencies]
klipp-core = { path = "crates/klipp-core" }
serde = { version = "1", features = ["derive"] }

[profile.dev]
opt-level = 1
"""


def with_manifests(lock=None, root=None, *paths):
    manifests = {}
    if lock is not None:
        manifests["Cargo.lock"] = (LOCK, lock)
    if root is not None:
        manifests["Cargo.toml"] = (ROOT, root)
    return decide(list(paths) + list(manifests), METADATA, manifests)


class ManifestScopeTests(unittest.TestCase):
    """#1124: Cargo.toml and Cargo.lock are read for what they touch."""

    def test_a_new_dev_dependency_tests_its_crate_alone(self):
        # media's tests start using klipp-core: media's lockfile entry and its
        # manifest's [dev-dependencies] change. ui, built on media, sees none
        # of it.
        lock = LOCK.replace('name = "klipp-media"\nversion = "0.1.0"\n', 'name = "klipp-media"\nversion = "0.1.0"\ndependencies = ["klipp-core"]\n')
        old = '[package]\nname = "klipp-media"\n\n[dependencies]\n'
        new = old + '\n[dev-dependencies]\nklipp-core = { workspace = true }\n'
        manifests = {"Cargo.lock": (LOCK, lock), "crates/klipp-media/Cargo.toml": (old, new)}
        self.assertEqual(decide(list(manifests), METADATA, manifests), "packages klipp-media")

    def test_a_crates_new_dependency_reaches_what_is_built_on_it(self):
        lock = LOCK.replace('name = "klipp-media"\nversion = "0.1.0"\n', 'name = "klipp-media"\nversion = "0.1.0"\ndependencies = ["klipp-core"]\n')
        old = '[package]\nname = "klipp-media"\n\n[dependencies]\n'
        new = old + 'klipp-core = { workspace = true }\n'
        manifests = {"Cargo.lock": (LOCK, lock), "crates/klipp-media/Cargo.toml": (old, new)}
        # media's code now builds on core: media and ui, built on media.
        # Core itself is unchanged.
        self.assertEqual(decide(list(manifests), METADATA, manifests), "packages klipp-media klipp-ui")

    def test_a_vendored_path_crate_is_walked_through(self):
        # A path crate outside the workspace, as vendor/ holds: a change to
        # what it uses reaches the workspace crates that use it.
        lock = LOCK.replace('dependencies = ["klipp-document", "klipp-media"]', 'dependencies = ["klipp-document", "klipp-media", "winit"]') + '\n[[package]]\nname = "winit"\nversion = "0.30.0"\ndependencies = ["serde_derive"]\n'
        base = decide(["Cargo.lock"], METADATA, {"Cargo.lock": (LOCK, lock)})
        self.assertEqual(base, "packages klipp-ui")
        bumped = lock.replace('name = "serde_derive"\nversion = "1.0.0"', 'name = "serde_derive"\nversion = "1.0.1"')
        self.assertEqual(decide(["Cargo.lock"], METADATA, {"Cargo.lock": (lock, bumped)}), "packages klipp-core klipp-document klipp-timeline klipp-ui")

    def test_a_new_crate_tests_it_and_its_users(self):
        # A new member and its path entry; media, the only user, changes too.
        meta = {"workspace_root": "/w", "packages": METADATA["packages"] + [
            {"name": "klipp-new", "manifest_path": "/w/crates/klipp-new/Cargo.toml", "dependencies": []}]}
        meta["packages"] = [dict(p, dependencies=p["dependencies"] + [{"name": "klipp-new", "path": "/w/crates/klipp-new"}]) if p["name"] == "klipp-media" else p for p in meta["packages"]]
        root = ROOT.replace('"crates/klipp-ui"]', '"crates/klipp-ui", "crates/klipp-new"]').replace(
            'klipp-core = { path = "crates/klipp-core" }', 'klipp-core = { path = "crates/klipp-core" }\nklipp-new = { path = "crates/klipp-new" }')
        lock = LOCK.replace('name = "klipp-media"\nversion = "0.1.0"\n', 'name = "klipp-media"\nversion = "0.1.0"\ndependencies = ["klipp-new"]\n') + '\n[[package]]\nname = "klipp-new"\nversion = "0.1.0"\n'
        manifests = {"Cargo.toml": (ROOT, root), "Cargo.lock": (LOCK, lock)}
        got = decide(["Cargo.toml", "Cargo.lock", "crates/klipp-new/src/lib.rs", "crates/klipp-new/Cargo.toml", "crates/klipp-media/Cargo.toml"], meta, manifests)
        self.assertEqual(got, "packages klipp-media klipp-new klipp-ui")

    def test_an_outside_package_reaches_every_crate_built_on_it(self):
        # serde_derive is only reached through serde, which timeline uses:
        # the walk climbs the lockfile, then spreads as for a timeline change.
        lock = LOCK.replace('name = "serde_derive"\nversion = "1.0.0"', 'name = "serde_derive"\nversion = "1.0.1"').replace('dependencies = ["serde_derive"]', 'dependencies = ["serde_derive"]')
        self.assertEqual(with_manifests(lock), "packages klipp-core klipp-document klipp-timeline klipp-ui")

    def test_a_package_that_reaches_no_crate_runs_everything(self):
        self.assertTrue(with_manifests(LOCK + '\n[[package]]\nname = "orphan"\nversion = "1.0.0"\nsource = "registry+x"\n').startswith("full "))

    def test_an_ambiguous_lockfile_name_runs_everything(self):
        lock = LOCK + '\n[[package]]\nname = "serde"\nversion = "2.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n'
        self.assertTrue(with_manifests(lock).startswith("full "))

    def test_the_lockfiles_own_fields_run_everything(self):
        self.assertTrue(with_manifests(LOCK.replace("version = 4", "version = 3")).startswith("full "))

    def test_an_unreadable_manifest_runs_everything(self):
        self.assertTrue(with_manifests("not [ toml").startswith("full "))
        self.assertTrue(with_manifests(None, "[workspace").startswith("full "))

    def test_a_manifest_without_its_texts_runs_everything(self):
        self.assertTrue(decide(["Cargo.lock"], METADATA, {}).startswith("full "))
        self.assertTrue(decide(["Cargo.lock"], METADATA, {"Cargo.lock": (None, LOCK)}).startswith("full "))

    def test_root_manifest_changes_outside_members_and_new_paths_run_everything(self):
        cases = {
            "a changed dependency entry": ROOT.replace('features = ["derive"]', 'features = ["derive", "rc"]'),
            "a removed dependency entry": ROOT.replace('serde = { version = "1", features = ["derive"] }\n', ""),
            "a new outside dependency": ROOT.replace("[profile.dev]", 'anyhow = "1"\n\n[profile.dev]'),
            "a new path that isn't a member": ROOT.replace("[profile.dev]", 'x = { path = "crates/x" }\n\n[profile.dev]'),
            "a profile": ROOT.replace("opt-level = 1", "opt-level = 2"),
            "a patch": ROOT + '\n[patch.crates-io]\nserde = { path = "vendor/serde" }\n',
            "the lints": ROOT + '\n[workspace.lints.clippy]\nall = "warn"\n',
            "the resolver": ROOT.replace('resolver = "2"', 'resolver = "3"'),
        }
        for why, root in cases.items():
            self.assertTrue(with_manifests(None, root).startswith("full "), why)

    def test_a_member_added_in_the_manifest_alone_tests_that_crate(self):
        # Review of #1125: only Cargo.toml in the change list must not read
        # as documentation.
        root = ROOT.replace('"crates/klipp-ui"]', '"crates/klipp-ui", "crates/klipp-new"]')
        meta = {"workspace_root": "/w", "packages": METADATA["packages"] + [
            {"name": "klipp-new", "manifest_path": "/w/crates/klipp-new/Cargo.toml", "dependencies": []}]}
        self.assertEqual(decide(["Cargo.toml"], meta, {"Cargo.toml": (ROOT, root)}), "packages klipp-new")

    def test_a_member_cargo_doesnt_list_runs_everything(self):
        root = ROOT.replace('"crates/klipp-ui"]', '"crates/klipp-ui", "crates/klipp-ghost"]')
        self.assertTrue(with_manifests(None, root).startswith("full "))

    def test_a_member_removed_runs_everything(self):
        root = ROOT.replace(', "crates/klipp-ui"]', "]")
        self.assertTrue(with_manifests(None, root).startswith("full "))

    def test_a_checksum_change_reaches_the_crates_built_on_it(self):
        # Review of #1125: fields beside the dependencies were ignored.
        lock = LOCK.replace('name = "serde_derive"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n',
                            'name = "serde_derive"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "abc"\n')
        self.assertEqual(with_manifests(lock), "packages klipp-core klipp-document klipp-timeline klipp-ui")

    def test_a_reordered_member_list_changes_nothing(self):
        root = ROOT.replace('"crates/klipp-core", "crates/klipp-timeline"', '"crates/klipp-timeline", "crates/klipp-core"')
        self.assertTrue(with_manifests(None, root).startswith("none "))



def mentions_in(files):
    """A `mentions` over made-up file contents."""
    return lambda text: [(f, line) for f, body in sorted(files.items()) for line in body.splitlines() if text in line]


class TestFolderScopeTests(unittest.TestCase):
    """#1124: a crate's tests, benches and examples test that crate, and
    whatever reads them."""

    def test_a_test_file_tests_its_crate_alone(self):
        # timeline is built on by document and ui, but a test file is
        # timeline's own.
        got = decide(["crates/klipp-timeline/tests/a.rs"], METADATA, {}, mentions_in({}))
        self.assertEqual(got, "packages klipp-timeline")

    def test_a_fixture_read_by_another_crates_tests_tests_both(self):
        files = {"crates/klipp-ui/tests/load.rs": 'join("../klipp-timeline/tests/fixtures/a")'}
        got = decide(["crates/klipp-timeline/tests/fixtures/a/project.json"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-timeline klipp-ui")

    def test_a_fixture_another_crates_code_reads_reaches_what_is_built_on_it(self):
        files = {"crates/klipp-document/src/x.rs": 'include_str!("../../klipp-timeline/tests/fixtures/a")'}
        got = decide(["crates/klipp-timeline/tests/fixtures/a"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-document klipp-timeline klipp-ui")

    def test_a_fixture_its_own_crates_code_includes_is_a_change_to_the_crate(self):
        files = {"crates/klipp-timeline/src/import.rs": 'include_str!("../tests/fixtures/a.mlt")'}
        got = decide(["crates/klipp-timeline/tests/fixtures/a.mlt"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-core klipp-document klipp-timeline klipp-ui")

    def test_a_comment_naming_the_folder_reads_nothing(self):
        files = {"crates/klipp-document/src/x.rs": "//! checked by `klipp-timeline/tests/a.rs`\n",
                 "crates/klipp-timeline/src/lib.rs": "/// see tests/a.rs\n"}
        got = decide(["crates/klipp-timeline/tests/a.rs"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-timeline")

    def test_a_reader_outside_every_crate_runs_everything(self):
        files = {"tools/check.py": 'open("crates/klipp-timeline/tests/a.rs")'}
        got = decide(["crates/klipp-timeline/tests/a.rs"], METADATA, {}, mentions_in(files))
        self.assertTrue(got.startswith("full "), got)

    def test_documentation_and_the_gates_scripts_read_nothing(self):
        files = {"docs/src/effects.md": "see `crates/klipp-timeline/tests/a.rs`",
                 "crates/klipp-timeline/README.md": "klipp-timeline/tests/a.rs",
                 "scripts/affected_crates.py": '"""like `klipp-timeline/tests/`"""'}
        got = decide(["crates/klipp-timeline/tests/a.rs"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-timeline")

    def test_a_path_attribute_is_code_not_a_comment(self):
        # Review of #1125: `#[path]` starts with `#` and was read as a comment.
        files = {"crates/klipp-media/tests/shared.rs": '#[path = "../../klipp-timeline/tests/a.rs"]\nmod a;'}
        got = decide(["crates/klipp-timeline/tests/a.rs"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-media klipp-timeline")

    def test_without_a_lookup_a_test_file_counts_as_its_crate(self):
        self.assertEqual(decide(["crates/klipp-timeline/tests/a.rs"], METADATA), "packages klipp-core klipp-document klipp-timeline klipp-ui")

    def test_a_failing_lookup_runs_everything(self):
        from affected_crates import Unattributable

        def broken(text):
            raise Unattributable("git grep failed")
        self.assertTrue(decide(["crates/klipp-timeline/tests/a.rs"], METADATA, {}, broken).startswith("full "))


SCAN = 'let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();'


class WorkspaceScanTests(unittest.TestCase):
    """A test that scans every crate's sources runs on any crate's code
    change: a new reader in one crate turned another crate's scan red on
    main, unseen, while the gate tested only the crates the change reached."""

    def test_a_code_change_anywhere_runs_every_workspace_scan(self):
        # media reaches only itself and ui; core's scan reads it too.
        files = {"crates/klipp-core/tests/readers.rs": SCAN}
        got = decide(["crates/klipp-media/src/lib.rs"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-core klipp-media klipp-ui")

    def test_a_scan_in_a_crates_own_code_counts_too(self):
        files = {"crates/klipp-document/src/scan_tests.rs": SCAN}
        got = decide(["crates/klipp-media/src/lib.rs"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-document klipp-media klipp-ui")

    def test_a_scan_runs_its_crate_alone_not_what_is_built_on_it(self):
        # timeline's scan: document and ui are built on timeline, but its
        # code is unchanged, so they don't run.
        files = {"crates/klipp-timeline/tests/readers.rs": SCAN}
        got = decide(["crates/klipp-media/src/lib.rs"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-media klipp-timeline klipp-ui")

    def test_a_test_or_documentation_change_runs_no_scan(self):
        # The scans read `src/`: a test file or a README is not in it.
        files = {"crates/klipp-core/tests/readers.rs": SCAN}
        self.assertEqual(decide(["crates/klipp-media/tests/a.rs"], METADATA, {}, mentions_in(files)), "packages klipp-media")
        self.assertTrue(decide(["README.md"], METADATA, {}, mentions_in(files)).startswith("none "))

    def test_a_comment_naming_the_scan_scans_nothing(self):
        files = {"crates/klipp-core/src/lib.rs": "// " + SCAN, "crates/klipp-timeline/src/lib.rs": "/* " + SCAN}
        got = decide(["crates/klipp-media/src/lib.rs"], METADATA, {}, mentions_in(files))
        self.assertEqual(got, "packages klipp-media klipp-ui")

    def test_the_real_scans_use_the_form_the_gate_looks_for(self):
        # If a scan finds the crates folder another way, the gate misses
        # it: this holds the two scans that exist to the form.
        root = Path(__file__).resolve().parents[2] / "crates"
        for scan in ("klipp-graph-lift/tests/effect_entry_readers.rs", "klipp-compose/tests/legacy_route_gone.rs"):
            self.assertIn('env!("CARGO_MANIFEST_DIR")).parent()', (root / scan).read_text(), scan)


if __name__ == "__main__":
    unittest.main()
