#!/usr/bin/env python3
"""Build a tagged Arch release, sign its checksums, or stage a signed pacman
repository. Never uploads."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z]+(?:\.[0-9A-Za-z]+)*)?\Z")
# The release packages, by arch, with the extension each arch's makepkg
# writes: Arch's default on x86_64, Arch Linux ARM's PKGEXT on aarch64.
# x86_64 is built here and always there; aarch64 is built on the Mac mini
# and copied into the output directory.
PACKAGE_EXT = {"x86_64": "zst", "aarch64": "xz"}
PLATFORM = {"x86_64": "Arch Linux x86_64", "aarch64": "Arch Linux ARM aarch64"}
# Klipp hosts Veylvik's engine from its own repository. A release names the
# Veylvik release it was tested with: a tag, never a bare revision or branch.
VEYLVIK_URL = "https://github.com/davidkodar/veylvik"
VEYLVIK_MANIFEST = "crates/klipp-veylvik/Cargo.toml"


def run(*args, cwd=ROOT, **kwargs):
    return subprocess.run(args, cwd=cwd, check=True, text=True, **kwargs)


def output(*args, cwd=ROOT):
    return run(*args, cwd=cwd, stdout=subprocess.PIPE).stdout.strip()


def release_version(tag, manifest):
    version = tomllib.loads(manifest)["workspace"]["package"]["version"]
    if not VERSION.fullmatch(version) or tag != f"v{version}":
        raise ValueError("Release tag must exactly match the workspace version; build metadata is not supported.")
    # SemVer forbids leading zeroes in numeric components, including prereleases.
    core, _, pre = version.partition("-")
    if any(len(v) > 1 and v.startswith("0") for v in core.split(".") + [p for p in pre.split(".") if p.isdigit()]):
        raise ValueError("Version contains a numeric component with a leading zero.")
    return version


def version_key(tag):
    """A release tag's order: numbers first, and a prerelease before its release."""
    core, _, pre = tag[1:].partition("-")
    parts = tuple(int(part) if part.isdigit() else part for part in pre.split(".")) if pre else ()
    return tuple(int(part) for part in core.split(".")), not pre, tuple((isinstance(p, str), p) for p in parts)


def veylvik_pin(manifest, lock, allow_rev=False):
    """The Veylvik tag and commit a Klipp manifest and lock file build with.
    Every Veylvik crate must come from Veylvik's repository at one release
    tag, and the lock file must hold that same tag at one commit. With
    allow_rev, a revision pin is let through and its revision given as the tag."""
    dependencies = tomllib.loads(manifest).get("dependencies", {})
    pins = {name: spec for name, spec in dependencies.items() if name.startswith("veylvik-")}
    if not pins:
        raise ValueError(f"{VEYLVIK_MANIFEST} names no Veylvik crate.")
    found = set()
    for name, spec in pins.items():
        if not isinstance(spec, dict) or spec.get("git") != VEYLVIK_URL:
            raise ValueError(f"{name} must come from {VEYLVIK_URL}.")
        if "branch" in spec:
            raise ValueError(f"{name} follows a branch; pin a Veylvik release tag.")
        if "tag" in spec:
            if not (spec["tag"].startswith("v") and VERSION.fullmatch(spec["tag"][1:])) or "rev" in spec:
                raise ValueError(f"{name}'s tag {spec['tag']!r} isn't a Veylvik release tag such as v0.1.0.")
            found.add(("tag", spec["tag"]))
        elif "rev" in spec and allow_rev:
            found.add(("rev", spec["rev"]))
        else:
            raise ValueError(f"{name} is pinned to revision {spec.get('rev')!r}, not a Veylvik release tag.")
    if len(found) != 1:
        raise ValueError(f"The Veylvik crates are pinned to different releases: {sorted(f[1] for f in found)}.")
    (kind, pin), = found
    expected = re.compile(rf"git\+{re.escape(VEYLVIK_URL)}\?{kind}={re.escape(pin)}#([0-9a-f]{{40}})\Z")
    locked = {package["name"]: expected.match(package.get("source", ""))
              for package in tomllib.loads(lock).get("package", []) if package["name"] in pins}
    commits = {m.group(1) for m in locked.values() if m}
    if set(locked) != set(pins) or not all(locked.values()) or len(commits) != 1:
        raise ValueError(f"Cargo.lock doesn't hold every Veylvik crate at {pin}; run cargo update -p veylvik-document.")
    return pin, commits.pop()


def remote_tags(listing):
    """Release tags and their commits from git ls-remote --tags, peeled."""
    tags, peeled = {}, {}
    for line in listing.splitlines():
        sha, _, ref = line.partition("\t")
        name = ref.removeprefix("refs/tags/")
        target = peeled if name.endswith("^{}") else tags
        name = name.removesuffix("^{}")
        if name.startswith("v") and VERSION.fullmatch(name[1:]):
            target[name] = sha
    # An annotated tag's own object isn't the commit; its peeled line is.
    return tags | peeled


def check_veylvik(manifest, lock, listing, allow_rev=False):
    """Refuse a Veylvik pin its repository doesn't hold as that release, and
    give the newer Veylvik releases there are, for a note."""
    pin, commit = veylvik_pin(manifest, lock, allow_rev)
    tags = remote_tags(listing)
    if not pin.startswith("v"):
        return pin, sorted(tags, key=version_key)[-1:]
    if pin not in tags:
        raise ValueError(f"Veylvik has no release {pin}.")
    if tags[pin] != commit:
        raise ValueError(f"Veylvik's {pin} is now {tags[pin][:12]}, but Cargo.lock builds {commit[:12]}; the tag moved.")
    return pin, sorted((t for t in tags if version_key(t) > version_key(pin)), key=version_key)


def package_name(version, arch):
    """The release package's file name: Arch versions can't hold a hyphen,
    so 0.1.0-rc.3 is packaged as 0.1.0rc.3."""
    return f"klipp-{version.replace('-', '')}-1-{arch}.pkg.tar.{PACKAGE_EXT[arch]}"


def package_info(package):
    """The .PKGINFO fields of a pacman package, read with bsdtar, which
    reads both zstd and xz; nothing in it is run."""
    text = output("bsdtar", "-xOqf", str(package), ".PKGINFO")
    fields = {}
    for line in text.splitlines():
        key, sep, value = line.partition(" = ")
        if sep and not key.startswith("#"):
            fields.setdefault(key, value)
    return fields


def check_package(package, version, arch):
    """`package` is this release's klipp package for `arch`: by its name,
    and by the name, version and arch it holds."""
    if package.name != package_name(version, arch):
        raise ValueError(f"{package.name} is not named as Klipp {version}'s {arch} package ({package_name(version, arch)}).")
    info = package_info(package)
    want = {"pkgname": "klipp", "pkgver": f"{version.replace('-', '')}-1", "arch": arch}
    found = {key: info.get(key) for key in want}
    if found != want:
        raise ValueError(f"{package.name} holds {found}, not {want}.")


def release_packages(destination, version):
    """The release's packages in `destination`, x86_64 first: exactly one
    x86_64 package, at most one aarch64, each checked. Any other klipp
    package there (another version, arch or extension, klipp-git) is
    refused rather than left out unseen."""
    found = {}
    for path in sorted(destination.glob("klipp*.pkg.tar*")):
        arch = next((a for a in PACKAGE_EXT if path.name == package_name(version, a)), None)
        if arch is None:
            raise ValueError(f"{path.name} in {destination} is not one of Klipp {version}'s packages "
                             f"({', '.join(package_name(version, a) for a in PACKAGE_EXT)}); remove it.")
        check_package(path, version, arch)
        found[arch] = path
    if "x86_64" not in found:
        raise ValueError(f"Expected the x86_64 package {package_name(version, 'x86_64')} (use the default PKGEXT).")
    return [found[a] for a in PACKAGE_EXT if a in found]


def write_sums(destination, release):
    """SHA256SUMS over the release's packages and source archive, and
    release.json, from `release` (version, tag, commit). What is listed is
    what the signature will cover. Says when there is no aarch64 package."""
    version = release["version"]
    packages = release_packages(destination, version)
    source = destination / f"klipp-{version}-source.tar.gz"
    if not source.is_file():
        raise ValueError(f"The source archive {source.name} is missing from {destination}.")
    checksums = []
    for artifact in packages + [source]:
        with artifact.open("rb") as stream:
            checksums.append(f"{hashlib.file_digest(stream, 'sha256').hexdigest()}  {artifact.name}\n")
    (destination / "SHA256SUMS").write_text("".join(checksums))
    arches = [a for a in PACKAGE_EXT if destination / package_name(version, a) in packages]
    (destination / "release.json").write_text(json.dumps({"version": version, "tag": release["tag"], "commit": release["commit"],
        "arches": arches, "platforms": [PLATFORM[a] for a in arches], "prerelease": "-" in version}, indent=2) + "\n")
    if "aarch64" not in arches:
        print(f"No aarch64 package in {destination}: this release is x86_64 only. To add it, copy the Mac mini's "
              f"{package_name(version, 'aarch64')} here and run: python3 scripts/release.py sums {destination}")
    return arches


def build(args):
    if not args.tag.startswith("v") or not VERSION.fullmatch(args.tag[1:]):
        raise ValueError("Expected a release tag such as v0.1.0-rc.1")
    ref = f"refs/tags/{args.tag}"
    host = platform.machine()
    if host not in PACKAGE_EXT:
        raise ValueError(f"Release packages are built on x86_64 or aarch64, not {host}.")
    sha = output("git", "rev-parse", "--verify", f"{ref}^{{commit}}")
    manifest = output("git", "show", f"{ref}:Cargo.toml")
    version = release_version(args.tag, manifest)
    if output("git", "status", "--porcelain", "--untracked-files=no"):
        raise ValueError("Commit tracked changes before building a release.")
    veylvik, newer = check_veylvik(output("git", "show", f"{ref}:{VEYLVIK_MANIFEST}"), output("git", "show", f"{ref}:Cargo.lock"),
                                   output("git", "ls-remote", "--tags", VEYLVIK_URL), args.allow_veylvik_rev)
    if not veylvik.startswith("v"):
        print(f"Note: this release builds Veylvik at revision {veylvik}, not a Veylvik release.")
    if newer:
        print(f"Note: Veylvik has newer releases than {veylvik}: {', '.join(newer)}. This release keeps {veylvik}.")
    destination = args.output.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    # Keep compiler scratch on the output filesystem, not a small /tmp tmpfs.
    with tempfile.TemporaryDirectory(prefix="build-", dir=destination) as temp:
        checkout = Path(temp) / "source"
        run("git", "clone", "--shared", "--no-checkout", str(ROOT), str(checkout))
        run("git", "checkout", "--detach", sha, cwd=checkout)
        env = dict(os.environ, KLIPP_RELEASE_VERSION=version, PKGDEST=str(destination),
                   TMPDIR=temp, SOURCE_DATE_EPOCH=output("git", "show", "-s", "--format=%ct", sha))
        # Do not inherit machine-specific C toolchain overrides into a release.
        for key in list(env):
            if key.startswith(("CFLAGS", "CXXFLAGS", "CPPFLAGS", "CMAKE_", "GGML_", "CARGO_TARGET_")):
                del env[key]
        env.pop("RUSTFLAGS", None)
        env.pop("CARGO_ENCODED_RUSTFLAGS", None)
        run("makepkg", "--cleanbuild", "--force", cwd=checkout / "packaging", env=env)
        if output("git", "status", "--porcelain", "--untracked-files=no", cwd=checkout):
            raise ValueError("Release build modified tracked source files.")
        # The source archive contains the exact tag used, including vendored code.
        source = destination / f"klipp-{version}-source.tar.gz"
        run("git", "archive", "--format=tar.gz", f"--prefix=klipp-{version}/", "-o", str(source), sha)
    if host == "aarch64":
        package = destination / package_name(version, "aarch64")
        if not package.is_file():
            raise ValueError(f"Expected the aarch64 package {package.name} (Arch Linux ARM's PKGEXT is .pkg.tar.xz).")
        check_package(package, version, "aarch64")
        source.unlink(missing_ok=True)
        print(f"Built {args.tag} at {sha} for aarch64: {package}")
        print("Test it here, then copy it into the x86_64 build's output directory and run "
              "python3 scripts/release.py sums DIRECTORY there before signing.")
        return
    write_sums(destination, {"version": version, "tag": args.tag, "commit": sha})
    print(f"Built {args.tag} at {sha}. Test the packages before signing or publishing: {destination}")
    print(f"Then sign its checksums: python3 scripts/release.py sign {destination} --key PATH")


def sums(args):
    """SHA256SUMS and release.json made again for a built release, after the
    aarch64 package was copied in. Refused once signed: the signature would
    no longer cover what is listed."""
    destination = args.release.resolve()
    if (destination / "SHA256SUMS.minisig").exists():
        raise ValueError("This release is signed already; remove SHA256SUMS.minisig, then sign again after this.")
    release = json.loads((destination / "release.json").read_text())
    arches = write_sums(destination, release)
    print(f"Listed {', '.join(arches)} in {destination / 'SHA256SUMS'}. Sign it next.")


def release_key(updater=ROOT / "packaging/klipp-update"):
    """The release public key klipp-update checks downloads with: its
    RELEASE_KEY line, empty until the key is made."""
    found = re.search(r"^RELEASE_KEY='([^']*)'$", updater.read_text(), re.M)
    return found.group(1) if found else ""


def sign(args):
    """Sign a built release's SHA256SUMS with minisign, which covers every
    asset it lists, and check the signature the way klipp-update will."""
    destination = args.release.resolve()
    sums = destination / "SHA256SUMS"
    version = json.loads((destination / "release.json").read_text())["version"]
    key = args.key or os.environ.get("KLIPP_MINISIGN_KEY")
    if not key:
        raise ValueError("Give the release's minisign secret key with --key or KLIPP_MINISIGN_KEY.")
    public = release_key()
    if not public:
        raise ValueError("packaging/klipp-update has no RELEASE_KEY: put the release's public key there first.")
    signature = destination / "SHA256SUMS.minisig"
    # minisign asks for the key's passphrase itself; nothing here keeps it.
    # The trusted comment names the version, which klipp-update checks.
    run("minisign", "-S", "-s", str(Path(key).expanduser()), "-m", str(sums), "-x", str(signature), "-t", f"klipp {version}")
    try:
        verified = output("minisign", "-V", "-P", public, "-m", str(sums), "-x", str(signature))
    except subprocess.CalledProcessError:
        signature.unlink(missing_ok=True)
        raise ValueError("The signature doesn't verify with the RELEASE_KEY in packaging/klipp-update; signed with another key?")
    if f"Trusted comment: klipp {version}" not in verified.splitlines():
        signature.unlink(missing_ok=True)
        raise ValueError(f"The signature's trusted comment is not \"klipp {version}\".")
    print(f"Signed {sums} for Klipp {version}: attach {signature.name} with the other assets.")


def repository(args):
    package = args.package.resolve()
    if not re.fullmatch(r"klipp-[0-9]+\.[0-9]+\.[0-9]+-[0-9]+-x86_64\.pkg\.tar\.zst", package.name):
        raise ValueError("The stable repository accepts only stable klipp release packages, not previews or klipp-git.")
    if not re.fullmatch(r"[A-Fa-f0-9]{40}|[A-Fa-f0-9]{64}", args.key):
        raise ValueError("Use the full signing-key fingerprint.")
    destination = args.output.resolve()
    if destination.exists() and any(destination.iterdir()):
        raise ValueError("Use an empty output directory; never modify a live repository in place.")
    destination.mkdir(parents=True, exist_ok=True)
    target = destination / package.name
    shutil.copy2(package, target)
    run("gpg", "--local-user", args.key, "--detach-sign", str(target))
    run("gpg", "--verify", str(target) + ".sig", str(target))
    run("repo-add", "--sign", "--key", args.key, "--verify", "klipp.db.tar.gz", target.name, cwd=destination)
    # Static hosts may not preserve symlinks; serve concrete database files.
    for name in ("klipp.db", "klipp.db.sig", "klipp.files", "klipp.files.sig"):
        path = destination / name
        if path.is_symlink():
            data = path.read_bytes()
            path.unlink()
            path.write_bytes(data)
    run("gpg", "--verify", str(destination / "klipp.db.sig"), str(destination / "klipp.db"))
    with (destination / "klipp-signing-key.asc").open("w") as key_file:
        run("gpg", "--armor", "--export", args.key, stdout=key_file)
    print(f"Signed repository staged at {destination}. Upload packages first and the database last.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    b = commands.add_parser("build")
    b.add_argument("tag")
    b.add_argument("--output", type=Path, required=True)
    b.add_argument("--allow-veylvik-rev", action="store_true", help="build with Veylvik pinned to a revision, not a release tag")
    b.set_defaults(func=build)
    m = commands.add_parser("sums", help="list the packages again, after copying in the aarch64 one")
    m.add_argument("release", type=Path, help="the x86_64 build's output directory")
    m.set_defaults(func=sums)
    s = commands.add_parser("sign")
    s.add_argument("release", type=Path, help="the build's output directory")
    s.add_argument("--key", help="the minisign secret key (or KLIPP_MINISIGN_KEY)")
    s.set_defaults(func=sign)
    r = commands.add_parser("repository")
    r.add_argument("package", type=Path)
    r.add_argument("--key", required=True)
    r.add_argument("--output", type=Path, required=True)
    r.set_defaults(func=repository)
    args = parser.parse_args()
    try:
        args.func(args)
    except (ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Release failed: {error}\n")


if __name__ == "__main__":
    main()
