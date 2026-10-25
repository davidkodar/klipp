# Downloadable releases and updates

The release tooling is implemented, but no public package repository or signing
identity has been configured yet. Do not advertise automatic package updates
until the hosting steps below have been completed. Until a release is published,
the in-app release check reports that no releases are available.
The app never reads a user's GitHub credentials.

## Maintainer: build a release

Follow CONTRIBUTING.md: issue, branch, reviewed PR, signed-off commits, squash
merge. Test the release candidate on a GPU before tagging. Releases come from tags on tested commits; this
script does not replace that gate and does not publish or create tags.

1. Set `[workspace.package].version` and update Cargo.lock in the release PR.
   Keep the real SemVer prerelease suffix, e.g. `0.20.0-rc.1`. Do not call an
   untested candidate a stable release. The tag must be `v` plus that exact value.
2. After merging and testing, create and push the annotated release tag.
3. On an up-to-date x86_64 Arch build host with the dependencies from PKGBUILD,
   Python 3.11+, Git and makepkg installed, run as a normal user:

   ```bash
   python3 scripts/release.py build v0.20.0-rc.1 --output target/releases/0.20.0-rc.1
   ```

   Substitute the actual tag. The script clones its exact commit into a new
   build directory, requires a clean tracked tree, checks the version, builds
   `klipp` (not `klipp-git`), and produces the package, matching source archive,
   SHA256SUMS and release.json. It does not install dependencies or the package.
   `makepkg` reports missing dependencies before building; install these first.
   Builds can take significant time and disk space. Set CARGO_BUILD_JOBS to limit
   compiler concurrency. The script uses disk-backed compiler scratch.

   Rust and Whisper's native C/C++ backend target baseline x86-64, with host
   AVX/AVX2/FMA optimizations disabled. Vulkan handles GPU-specific execution.
   This is an Arch package using Arch runtime libraries, not a universal Linux
   binary. Do not use CachyOS-specific or CPU-v3/v4 libraries as the public build
   environment. Rebuild and retest when Arch changes required library ABIs.

4. Install the candidate on test systems. Verify launch, import, audio/video
   playback, save/reopen and export. Test AMD and NVIDIA hardware before claiming
   both are validated; a portable CPU build alone does not establish GPU support.
5. Create a **draft** GitHub Release from the existing tag, attach the package,
   source archive, checksums and signatures, and write notes including supported
   systems and known limitations. Mark prereleases as such. Do not upload a
   machine-optimized developer build as a release asset.

   ```bash
   gh release create v0.20.0-rc.1 --verify-tag --draft --prerelease \
     --title 'Klipp 0.20.0-rc.1' --notes-file release-notes.md \
     target/releases/0.20.0-rc.1/*.pkg.tar.zst \
     target/releases/0.20.0-rc.1/*-source.tar.gz \
     target/releases/0.20.0-rc.1/SHA256SUMS
   ```

   These are examples, not a command to release that particular version. Sign
   downloadable packages with `gpg --local-user FULL_FINGERPRINT --detach-sign
   PACKAGE` and attach `PACKAGE.sig` too. Publish only after validation; for stable
   versions, publish the repository package before the release announcement.
   Keep old release assets and their matching source archives available.

## Maintainer: signed Arch repository

Choose an HTTPS host for static files and a maintained OpenPGP signing key.
Keep its private key outside the repository, back it up securely, and publish the
full public fingerprint through a trusted project page. Do not commit secrets or
invent a production URL. Key creation, hosting configuration, public access and
publication are separate deployment decisions.

For a tested **stable** package:

```bash
python3 scripts/release.py repository \
  target/releases/0.20.0/klipp-0.20.0-1-x86_64.pkg.tar.zst \
  --key FULL_SIGNING_KEY_FINGERPRINT --output target/repository/0.20.0
```

This signs and verifies the package, creates signed `klipp.db` and `klipp.files`
(and their compressed forms), and exports the public key. Use a new empty staging
directory each time. It deliberately rejects preview and development packages.
Upload the package and signature first, then publish the database and database
signature together (ideally by switching an atomic directory/symlink on the
server). Retain old package files so clients using a cached database still work.
Serve the key and databases without authentication for public users. Never put
credentials in a pacman Server URL.

The repository section is named **klipp**, which the app detects:

```ini
[klipp]
SigLevel = Required
Server = https://YOUR-HOST/klipp/x86_64
```

Replace the placeholder with the actual HTTPS address before publishing install
instructions. Require package **and database** signatures; do not recommend
TrustAll or disabling verification. Users verify the published fingerprint,
import the public key with `sudo pacman-key --add klipp-signing-key.asc`, locally
trust that exact fingerprint with `sudo pacman-key --lsign-key FINGERPRINT`, and
add the repository section to `/etc/pacman.conf`. Then:

```bash
sudo pacman -Syu klipp
```

Pacman will ask to replace a conflicting `klipp-git` package if one is installed.
Do not run both installation methods over the same files. Normal system updates
now include Klipp. Follow Arch's full-upgrade policy; do not use `pacman -Sy klipp`.

For a standalone download before configuring the repository: verify the detached
signature against the independently verified public key, fully update Arch, then
install the downloaded package using `sudo pacman -U ./klipp-VERSION-1-x86_64.pkg.tar.zst`.
Installing a file does **not** automatically configure a repository.

## In the app

- Automatic checks are **off by default**. After the user explicitly enables them
  in Preferences → General → Updates, a background check starts after 30 seconds,
  at most once a day across restarts.
  It queries the most recent 100 GitHub releases over HTTPS with a timeout and
  response-size limit. It sends no project information, and never downloads or
  installs software automatically. As with any request, GitHub sees the network
  address. Disable checks in Preferences → General → Updates. The previous
  default-on setting is reset to off because a saved default does not establish
  consent; explicitly enabling the new setting is remembered.
- A newer release produces a dismissible notice. Offline/private/rate-limit errors
  are quiet in the background and explained in About.
- About → Check for updates checks in place and shows the result there;
  Preferences → General → Updates opens About to check.
  Preview releases are opt-in for discovery; enabling them does not change your
  pacman repositories or install a beta automatically.
- Only `/usr/lib/klipp/klipp` owned by pacman's `klipp` package can offer the
  package updater, and only with a configured `[klipp]` repository. The fixed,
  package-owned helper opens a terminal, asks the user to save/finish work and
  confirm, then runs `sudo pacman -Syu klipp`. Pacman shows the transaction and
  verifies signatures. Passwords stay in the terminal. The app does not overwrite
  its own executable or claim the update succeeded before pacman completes.
- Restart Klipp after a successful installation. Never force-close a project to
  update. The running process keeps reporting its original embedded version.
- `klipp-git` users get source-update instructions: pull, then rebuild/install
  with makepkg. Unmanaged/development builds get rebuild instructions. Preview
  packages are manual downloads; the stable repository never ships them.

## Testing the update check

Two environment variables let the update check run against the real GitHub
releases as if Klipp were an older version or installed another way. They are
read once at start and logged at info level; without them nothing changes.

- `KLIPP_UPDATE_TEST_VERSION=<semver>`: the check treats this as the installed
  version. About's Version row still shows the real one; the status row says
  "(testing as <version>)".
- `KLIPP_UPDATE_TEST_INSTALL=package|repository|klipp-git|source`: replaces the
  installation the check finds.
  - `package`: the release package without its repository, so the release page
    download is offered.
  - `repository`: the release package with the `[klipp]` repository configured,
    so Install update… shows, previews included. The `klipp-update` helper still
    asks before doing anything, and refuses when the real installation isn't
    the release package; About then says the updater ended without updating.
  - `klipp-git`: the makepkg steps with Copy.
  - `source`: the cargo rebuild steps with Copy.

Values other than these are ignored with a warning. Run a build from the
repository root, then use About → Check for updates:

```bash
# A newer release is available (the latest published release, rc.2 at the time of writing):
KLIPP_UPDATE_TEST_VERSION=0.1.0-rc.1 KLIPP_UPDATE_TEST_INSTALL=package ./target/release/klipp-app
KLIPP_UPDATE_TEST_VERSION=0.1.0-rc.1 KLIPP_UPDATE_TEST_INSTALL=repository ./target/release/klipp-app
KLIPP_UPDATE_TEST_VERSION=0.1.0-rc.1 KLIPP_UPDATE_TEST_INSTALL=klipp-git ./target/release/klipp-app
KLIPP_UPDATE_TEST_VERSION=0.1.0-rc.1 KLIPP_UPDATE_TEST_INSTALL=source ./target/release/klipp-app
# Up to date: a version past every release.
KLIPP_UPDATE_TEST_VERSION=99.0.0 ./target/release/klipp-app
```

Preview releases are included by default when the build itself is a preview
(Preferences → General → Updates → Include preview releases). With it off, a
test version older than the latest stable release is needed to see one. To see
a failure, check with the network off.

## Tests

```bash
python3 -m unittest discover -s scripts/tests -p 'test_*.py'
cargo test -p klipp-ui updates::tests
```

Run the full repository gate and nightly UI checks before merging, in addition to
these targeted tests. Test repository signing using a disposable key and isolated
GNUPGHOME; never import a test key into the system pacman keyring.
