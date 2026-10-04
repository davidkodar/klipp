#!/usr/bin/env python3
"""Which workspace crates a change can affect, for the pre-push gate (#873).

Reads the changed paths (one per line, relative to the workspace root) on
stdin and `cargo metadata --no-deps --format-version 1` from the file named
on the command line, and prints one line:

    full <reason>        run the whole test suite
    none <reason>        nothing a test reads changed (documentation)
    packages a b c ...   the crates changed, every crate built on them,
                         and every crate whose tests use one of those

A path the rules below do not know asks for the full suite: the cheap
answer is only given where it is certainly enough.
"""

import json
import sys
from pathlib import PurePosixPath

# Anything here can change what every crate builds or tests against.
FULL = ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml", "rust-toolchain")
FULL_DIRS = (".cargo/", "assets/", "vendor/", "test-luts/", "tools/", "scripts/")
# Read by people, not by a build or a test. A crate's own README or docs
# are under crates/ and count as that crate's change.
NONE_DIRS = ("docs/", "packaging/", ".github/")
NONE_SUFFIXES = (".md",)
NONE_FILES = ("LICENSE", "rustfmt.toml", "deny.toml", ".gitignore")


def decide(changed, metadata):
    root = PurePosixPath(metadata["workspace_root"])
    packages = metadata["packages"]
    member_dirs = {}
    for p in packages:
        rel = PurePosixPath(p["manifest_path"]).parent.relative_to(root)
        member_dirs[str(rel)] = p["name"]
    # Workspace crates by name, each with the workspace crates it builds on
    # (normal and build dependencies) and those only its tests use.
    names = {p["name"] for p in packages}
    local = lambda p, dev: {d["name"] for d in p["dependencies"] if d["name"] in names and d.get("path") and (d.get("kind") == "dev") == dev}
    builds_on = {p["name"]: local(p, False) for p in packages}
    tests_use = {p["name"]: local(p, True) for p in packages}

    hit = set()
    for path in (line.strip() for line in changed):
        if not path:
            continue
        if path in FULL or path.startswith(FULL_DIRS):
            return f"full {path} is shared by the whole workspace"
        owner = next((name for d, name in sorted(member_dirs.items(), key=lambda kv: -len(kv[0])) if path.startswith(d + "/")), None)
        if owner:
            hit.add(owner)
            continue
        top = PurePosixPath(path)
        if path.startswith(NONE_DIRS) or (len(top.parts) == 1 and (path.endswith(NONE_SUFFIXES) or path in NONE_FILES)):
            continue
        return f"full {path} is outside every crate, and the gate cannot tell what reads it"

    if not hit:
        return "none only documentation changed"
    # Every crate built on one hit, at any remove; then every crate whose
    # tests use one of those. A test-only use stops there: the crate's own
    # code is unchanged, so what is built on it is too (klipp-core's tests
    # use klipp-timeline, and a timeline change is not a core change).
    affected = set(hit)
    grew = True
    while grew:
        grew = False
        for name, deps in builds_on.items():
            if name not in affected and deps & affected:
                affected.add(name)
                grew = True
    affected |= {name for name, deps in tests_use.items() if deps & affected}
    return "packages " + " ".join(sorted(affected))


def main():
    with open(sys.argv[1]) as f:
        metadata = json.load(f)
    print(decide(sys.stdin.read().splitlines(), metadata))


if __name__ == "__main__":
    main()
