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

The root `Cargo.toml` and `Cargo.lock` are read rather than treated as
opaque (#1124), when the gate passes the commits to compare
(`--git <base> <head>`). A new crate, or a new dev-dependency, then tests
what it touches instead of everything; each was a 25-minute run.

- **`Cargo.lock`:** each package entry added, removed or changed, then
  every workspace crate that reaches it through the lockfile's own graph
  (which holds dev-dependencies too), then their dependents as for any
  change. An entry that reaches no workspace crate runs everything.
- **The root `Cargo.toml`:** a change to `[workspace] members` is that
  crate's own files. One more `[workspace.dependencies]` entry that is a
  path to a workspace member is the new crate, whose users' manifests
  change too. Any other change runs everything: a changed or removed
  dependency entry, `[patch]`, a profile, the lints, anything else.
- **A crate's own `Cargo.toml`** that changed only in its
  `[dev-dependencies]` tests that crate alone: nothing built on it sees
  its dev-dependencies. Its lockfile entry's change is the same change.
- **A crate's `tests/`, `benches/` or `examples/`** tests that crate
  alone, unless its own code includes from that folder, as a fixture
  `include_str!` does. Any other crate whose files name the folder
  (`klipp-project/tests/`) is held the same way: its tests if they name
  it, the crate and what's built on it if its code does.
- A path crate outside the workspace (`vendor/`) in the lockfile is
  walked through like any package; a change to `vendor/` itself still
  runs everything.
- Anything that can't be read or compared runs everything.
"""

import json
import subprocess
import sys
import tomllib
from pathlib import PurePosixPath

# Read when the gate passes the commits to compare (#1124); otherwise,
# like the rest of FULL, a change to either runs everything.
MANIFESTS = ("Cargo.toml", "Cargo.lock")
# Anything here can change what every crate builds or tests against.
FULL = ("rust-toolchain.toml", "rust-toolchain")
FULL_DIRS = (".cargo/", "assets/", "vendor/", "test-luts/", "tools/", "scripts/")
# Read by people, not by a build or a test. A crate's own README or docs
# are under crates/ and count as that crate's change.
NONE_DIRS = ("docs/", "packaging/", ".github/")
NONE_SUFFIXES = (".md",)
NONE_FILES = ("LICENSE", "rustfmt.toml", "deny.toml", ".gitignore")


class Unattributable(Exception):
    """A manifest change the narrowing can't account for: run everything."""


def _lock_entries(text):
    """`Cargo.lock`'s packages, keyed by (name, version, source), each with
    the keys of what it depends on."""
    lock = tomllib.loads(text)
    pkgs = lock.get("package", [])
    by_name = {}
    for p in pkgs:
        by_name.setdefault(p["name"], []).append(p)
    key = lambda p: (p["name"], p["version"], p.get("source"))

    def resolve(dep):
        # "name", "name version" or "name version (source)".
        parts = dep.split(" ", 2)
        cands = by_name.get(parts[0], [])
        if len(parts) > 1:
            cands = [c for c in cands if c["version"] == parts[1]]
        if len(parts) > 2:
            cands = [c for c in cands if f"({c.get('source')})" == parts[2]]
        if len(cands) != 1:
            raise Unattributable(f"Cargo.lock names {dep!r} ambiguously")
        return key(cands[0])

    rest = {k: v for k, v in lock.items() if k != "package"}
    return rest, {key(p): sorted(resolve(d) for d in p.get("dependencies", [])) for p in pkgs}


def lock_hits(old, new, names, tests_only=frozenset()):
    """The workspace crates a `Cargo.lock` change reaches. A changed entry
    of a crate in `tests_only` is its dev-dependencies, already counted."""
    old_rest, old_pkgs = _lock_entries(old)
    new_rest, new_pkgs = _lock_entries(new)
    if old_rest != new_rest:
        raise Unattributable("Cargo.lock's own fields changed")
    changed = {k for k in old_pkgs.keys() | new_pkgs.keys() if old_pkgs.get(k) != new_pkgs.get(k)}
    users = {}
    for pkgs in (old_pkgs, new_pkgs):
        for k, deps in pkgs.items():
            for d in deps:
                users.setdefault(d, set()).add(k)
    hits = set()
    for start in changed:
        if start[2] is None and start[0] in tests_only:
            continue
        seen, todo, reached = {start}, [start], set()
        while todo:
            k = todo.pop()
            if k[2] is None and k[0] in names:  # a workspace crate
                reached.add(k[0])
            for u in users.get(k, ()):
                if u not in seen:
                    seen.add(u)
                    todo.append(u)
        if not reached:
            raise Unattributable(f"Cargo.lock's {start[0]} {start[1]} reaches no workspace crate")
        hits |= reached
    return hits


def root_manifest_ok(old, new, member_dirs):
    """Whether a root `Cargo.toml` change is only members and new path
    entries for workspace crates; raises when it isn't."""
    old, new = tomllib.loads(old), tomllib.loads(new)
    pop = lambda t: (t.get("workspace", {}).pop("members", None), t.get("workspace", {}).pop("dependencies", {}))
    (_, old_deps), (_, new_deps) = pop(old), pop(new)
    if old != new:
        raise Unattributable("the root Cargo.toml changed outside [workspace] members and dependencies")
    for name in old_deps.keys() | new_deps.keys():
        if old_deps.get(name) == new_deps.get(name):
            continue
        entry = new_deps.get(name)
        if name in old_deps or not isinstance(entry, dict) or entry.get("path") not in member_dirs:
            raise Unattributable(f"the root Cargo.toml's [workspace.dependencies] {name} changed")
    return True


def dev_dependencies_only(old, new):
    """Whether a crate manifest changed only in its dev-dependencies."""
    def rest(text):
        t = tomllib.loads(text)
        t.pop("dev-dependencies", None)
        t.pop("dev_dependencies", None)
        for target in t.get("target", {}).values():
            target.pop("dev-dependencies", None)
            target.pop("dev_dependencies", None)
        return t
    return rest(old) == rest(new)


TEST_DIRS = ("tests", "benches", "examples")


def is_code(rel):
    """Whether a path inside a crate is built into the crate itself."""
    return rel.startswith("src/") or rel == "build.rs"


def _test_folder_readers(crate_dir, folder, member_dirs, mentions):
    """The crates that read `<crate_dir>/<folder>/`, each with whether its
    own code does; None when a reader is outside every crate."""
    owner = member_dirs[crate_dir]
    held = {owner: False}
    crate_name = crate_dir.rsplit("/", 1)[-1]
    # A comment reads nothing: only lines of code naming the folder count.
    code = lambda hits: {f for f, line in hits if not line.lstrip().startswith(("//", "#", "*", "/*"))}
    found = [(f, False) for f in code(mentions(f"{folder}/")) if f.startswith(crate_dir + "/")]
    found += [(f, True) for f in code(mentions(f"{crate_name}/{folder}/"))]
    for f, other in sorted(found):
        reader = next((n for d, n in sorted(member_dirs.items(), key=lambda kv: -len(kv[0])) if f.startswith(d + "/")), None)
        if reader is None:
            return None
        rel = f[len(next(d for d, n in member_dirs.items() if n == reader)) + 1:]
        if reader == owner and not other:
            # The crate's own files naming the folder: only its code counts.
            held[owner] = held[owner] or is_code(rel)
        else:
            held[reader] = held.get(reader, False) or is_code(rel)
    return held


def is_manifest(path):
    return path == "Cargo.lock" or path == "Cargo.toml" or path.endswith("/Cargo.toml")


def decide(changed, metadata, manifests=None, mentions=None):
    """`manifests`, when given, maps `Cargo.toml` and `Cargo.lock` to their
    (old, new) texts, None for a side without the file. `mentions(text)`
    lists the repository's lines containing `text`, as (file, line) pairs; without it, a test
    folder's change counts as its crate's, as before."""
    try:
        return _decide(changed, metadata, manifests or {}, mentions)
    except Unattributable as e:
        return f"full {e}"
    except (tomllib.TOMLDecodeError, KeyError, TypeError, AttributeError) as e:
        return f"full a manifest could not be read ({type(e).__name__})"


def _decide(changed, metadata, manifests, mentions):
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

    # Crates whose only manifest change is their dev-dependencies (#1124).
    tests_only = set()
    for path, (old, new) in manifests.items():
        owner = member_dirs.get(str(PurePosixPath(path).parent))
        if path.endswith("/Cargo.toml") and owner and old is not None and new is not None and dev_dependencies_only(old, new):
            tests_only.add(owner)

    hit = set()
    for path in (line.strip() for line in changed):
        if not path:
            continue
        if path in MANIFESTS:
            old, new = manifests.get(path, (None, None))
            if old is None or new is None:
                return f"full {path} is shared by the whole workspace"
            if path == "Cargo.lock":
                hit |= lock_hits(old, new, names, tests_only)
            else:
                root_manifest_ok(old, new, set(member_dirs))
            continue
        if path in FULL or path.startswith(FULL_DIRS):
            return f"full {path} is shared by the whole workspace"
        owner = next((name for d, name in sorted(member_dirs.items(), key=lambda kv: -len(kv[0])) if path.startswith(d + "/")), None)
        if owner and path == f"{next(d for d, n in member_dirs.items() if n == owner)}/Cargo.toml" and owner in tests_only:
            continue
        if owner and mentions is not None:
            crate_dir = next(d for d, n in member_dirs.items() if n == owner)
            rel = path[len(crate_dir) + 1:]
            folder = rel.split("/", 1)[0]
            if folder in TEST_DIRS and "/" in rel:
                held = _test_folder_readers(crate_dir, folder, member_dirs, mentions)
                if held is not None:
                    for name, code in held.items():
                        (hit if code else tests_only).add(name)
                    continue
        if owner:
            hit.add(owner)
            continue
        top = PurePosixPath(path)
        if path.startswith(NONE_DIRS) or (len(top.parts) == 1 and (path.endswith(NONE_SUFFIXES) or path in NONE_FILES)):
            continue
        return f"full {path} is outside every crate, and the gate cannot tell what reads it"

    if not hit and not tests_only:
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
    affected |= tests_only
    return "packages " + " ".join(sorted(affected))


def main():
    with open(sys.argv[1]) as f:
        metadata = json.load(f)
    changed = sys.stdin.read().splitlines()
    manifests = {}
    if len(sys.argv) == 5 and sys.argv[2] == "--git":
        base, head = sys.argv[3], sys.argv[4]
        show = lambda rev, path: subprocess.run(["git", "show", f"{rev}:{path}"], capture_output=True, text=True).stdout if subprocess.run(["git", "cat-file", "-e", f"{rev}:{path}"], capture_output=True).returncode == 0 else None
        manifests = {path: (show(base, path), show(head, path)) for path in changed if is_manifest(path)}
    mentions = None
    if manifests is not None and len(sys.argv) == 5:
        def mentions(text):
            out = subprocess.run(["git", "grep", "-z", "-n", "-F", "-e", text], capture_output=True, text=True)
            if out.returncode not in (0, 1):
                raise Unattributable(f"git grep failed for {text!r}")
            hits = []
            for record in out.stdout.splitlines():
                # file NUL line NUL text
                parts = record.split("\0", 2)
                if len(parts) != 3:
                    raise Unattributable(f"git grep printed {record[:80]!r}")
                hits.append((parts[0], parts[2]))
            return hits
    print(decide(changed, metadata, manifests, mentions))


if __name__ == "__main__":
    main()
