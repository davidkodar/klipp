# Contributing to Klipp

Thanks for taking a look. This is a short, practical guide — read
`ARCHITECTURE.md` alongside for the system map, and
`CODE_OF_CONDUCT.md` for how we treat each other.

Klipp is pre-release and moves fast. That makes some kinds of
contribution much easier to accept than others, so this document leads
with the process rather than the build.

---

## Before you write code

**Open an issue first.** For anything beyond a typo or an obvious
one-line fix, describe the problem before you write the fix. Klipp has
a lot of load-bearing invariants that are not visible from the call
site — the colour pipeline is linear-light end to end, the decode loop
is two-phase, GPU and CPU render paths are held to each other frame for
frame — and a patch that violates one of them is a patch nobody can
merge, however good the code is. Ten minutes in an issue saves you an
afternoon.

There are two issue forms:

* **Bug report** — what you did, what happened, what you expected.
  Include your distribution, GPU and driver, `klipp --version`, and the
  relevant part of `klipp-debug.log`. A bug we cannot reproduce is a
  bug we cannot fix.
* **Feature request** — the problem you have, not the solution you have
  in mind. "I cannot tell which take is sharpest" is a better request
  than "add a focus-peaking overlay", because the first admits more
  answers.

Good things to pick up: anything labelled `good first issue`, missing
or wrong attribution in the credits, distribution packaging beyond
Arch, and failures on GPUs we do not have — AMD and Intel coverage is
genuinely thin and reports from those machines are valuable.

Things to discuss before building: a new crate, a new dependency, a new
model in the manifest, anything that changes the `.klipp` format, and
anything that touches the colour pipeline's numerical output.

---

## How work gets merged

`main` is the only long-lived branch. It is always buildable and always
green, and nothing is pushed to it directly — not by contributors, not
by the maintainer. Everything arrives as a pull request.

GitHub enforces this: a ruleset on `main` accepts changes only through
pull requests, merged as a squash, with a linear history. The pre-push
hook also refuses a direct push to `main`, so the mistake is caught on
your machine before GitHub has to.

```
main  ──●──●──●──●──●──●──●──▶   protected, always green
         ╲        ╱      ╲  ╱
          ●──●──●'        ●─'    short-lived branches, one PR each

            ▲                ▲
        v0.1.0-rc.1      v0.1.0   ← releases are tags, not branches
```

The release version is declared once in the root `Cargo.toml`, under
`[workspace.package].version`. Every Klipp crate inherits it, and the app
displays the full value, including prerelease identifiers such as
`0.20.0-rc.1` or `1.0.0-beta.1`. Update that declaration and regenerate
`Cargo.lock` before tagging a release; the tag must match the version
(with a `v` prefix). Rebuild to embed the new version in the executable.

Releases are **tags**, because being released is a property of a
commit, not of a branch. A release candidate is `v0.1.0-rc.1`; the
release is `v0.1.0`. If a shipped version ever needs a fix while `main`
has moved past it, a `release/0.1.x` branch is cut from the tag at that
moment — there is no permanently-maintained stable or dev tree, and
adding one before there is something to maintain would just be a
promotion chore.

The flow:

0. Run `scripts/install-hooks.sh` if you have not already.
1. Fork, or branch if you have push access. Name the branch for what it
   does: `fix/decode-eagain-retry`, `feat/focus-peaking`.
2. Keep it short-lived. A branch that lives a week is a branch that
   conflicts.
3. Open a PR against `main`. Fill in the template; link the issue.
4. It gets reviewed. Expect questions about invariants more than about
   style.
5. The maintainer runs the full suite locally on a machine with a GPU.
6. It lands **squash-merged**, so `main` keeps one commit per logical
   change. Write your PR title as the commit subject you want — see
   [Commit style](#commit-style).

---

## Authorship and AI-assisted contributions

Klipp welcomes contributions developed with or without AI assistance.

Every contribution must be submitted and signed off by a person, under
their own name and a reachable email, who takes responsibility for the
change: its correctness, its licensing, and its compliance with these
contribution requirements.

AI tools may be used to write, change, review or test code. An AI
system is never listed as the author, committer or co-author of a
commit. No `Co-Authored-By` trailers at all: if two people worked on a
change, say so in the body.

Every commit must comply with the Developer Certificate of Origin, below.

### Sign your work

Klipp uses the [Developer Certificate of Origin](https://developercertificate.org/)
(DCO) — the same one-line attestation the Linux kernel uses. There is
no CLA and you keep your copyright.

Add a `Signed-off-by` line to each commit:

```bash
git commit -s -m "Subject line"
```

which appends:

```
Signed-off-by: Your Name <your.email@example.com>
```

By doing that you certify the terms at the link above: in short, that
you wrote the patch or have the right to submit it under the project's
licence, and that you are content for it to be distributed under that
licence forever.

The name in `Signed-off-by` must be a real name and must match the
commit author. This is the mechanism that makes the rule above
enforceable rather than decorative: an agent cannot sign off, because
signing off is a statement a person makes.

---

## Licence

Klipp is **GPL-3.0**, and contributions are accepted under GPL-3.0.
See `LICENSE` for the full text.

This is not an arbitrary choice: Klipp links against an FFmpeg built
with `--enable-gpl --enable-version3` (x264 and x265 are GPL), and
hosts frei0r plugins, which are GPL too. A permissively-licensed Klipp
would be a permissive label on source that can only be built into a
GPL binary — which helps nobody and misleads the reader.

If you add a dependency, check its licence is compatible and add it to
the credits section of `README.md`. `cargo deny check licenses` is the
mechanical version of that check.

---

## Build prerequisites

Klipp targets Linux (tested on CachyOS / Arch / Debian derivatives).
You need:

* **Rust** — pinned by `rust-toolchain.toml`. `rustup show` will
  install the right version.
* **ffmpeg development headers** — `ffmpeg-next` links against system
  ffmpeg. On Arch: `pacman -S ffmpeg`. On Debian: `apt install
  ffmpeg libavcodec-dev libavformat-dev libavutil-dev libswscale-dev
  libswresample-dev`.
* **lld** — the workspace is configured to link with `lld` via
  `.cargo/config.toml`. `pacman -S lld` or `apt install lld`.
* **Vulkan** — wgpu needs a working Vulkan ICD at runtime. Nvidia
  users: note that CachyOS puts `nvidia_icd.json` under
  `/usr/share/vulkan/icd.d/` rather than `/etc/vulkan/icd.d/`, so the
  binary sets `VK_DRIVER_FILES` at startup if it finds the file there.
* **Vulkan headers + `glslc` / `shaderc`** — build-time only. The
  speech-transcription model (Whisper via `whisper-rs`) runs on the GPU
  through Vulkan and compiles its shaders into the binary. Run
  `./scripts/vulkan-sdk.sh` once to lay out the SDK directory the build
  expects.
* **PipeWire** (recommended) / ALSA — cpal uses whichever is available.

## Running

```bash
cargo rr                 # cargo run --release (aliased in .cargo/config.toml)
cargo r                  # cargo run (debug, slower but faster to compile)
cargo c                  # cargo clippy --workspace --all-targets
cargo test --workspace   # every test
cargo test -p klipp-engine --lib   # a single crate's lib tests
```

The view-parity tests in `klipp-ui` draw a real frei0r plug-in, so
`cargo test -p klipp-ui` needs `frei0r-plugins` installed (an optional
dependency of the package); without it the frei0r case fails and says so.
`cargo test -p klipp-frei0r` probes every installed frei0r filter for frame
history (`tests/temporal_probe.rs`, one child process per plug-in); without
the package it says so and skips.

The plug-in fixtures that `scripts/plugin-tests.py` runs (`--format ofx`,
`--format frei0r`) are C sources under `crates/*/tests/fixtures/`, compiled
by the script into its artifact folder and never by `cargo build`. They need
a C compiler: `cc`, or the one `CC` names (`gcc` from the `base-devel`
group on Arch). Without one the script stops and says what to install.

Logs land in `klipp-debug.log` at the workspace root — tail it while
running. Level is controlled by the `RUST_LOG` env var; default is
`info`.

---

## Tooling

Install these once; the repo assumes they're available:

```bash
cargo install cargo-audit cargo-deny cargo-outdated cargo-udeps cargo-nextest tokei
```

* **cargo audit** — RustSec CVE scan. Run before bumping deps.
* **cargo deny check** — advisories, bans, licences.
* **cargo outdated --workspace --root-deps-only** — see what's stale.
* **cargo +nightly udeps --workspace --all-targets** — find unused
  deps. Requires nightly.
* **cargo nextest run** — faster test runner with better reporting.
* **tokei crates/** — LOC / comments / blanks per crate.

### The gate runs on your machine

**Install the hooks. This is step one, before you write anything:**

```bash
scripts/install-hooks.sh     # once, after cloning
```

Deterministic saved-project inputs are generated with `scripts/corpus.sh generate`.
See [the corpus instructions](corpus/README.md) for the input checks and deliberate
reference updates.

After that every `git push` runs the authorship check, clippy with
`-D warnings`, a rustdoc pass, and the tests the change can reach (see
[What each step runs](#what-each-step-runs)), and refuses the push on
any failure. On a warm build a gate run takes a few minutes, about 7 as
a typical figure, and up to about 16 when the whole suite runs.

Before you push, run the **pre-gate** in your branch's checkout, with
everything committed:

```bash
scripts/pre-gate.sh
```

It does the cheap checks that catch most gate failures, in your own
checkout, before the gate spends a run on them:

1. a merge check against the newest `main`, which names any file that
   conflicts, before anything is built;
2. `cargo check --all-targets` on every crate the change can affect;
3. every test target (unit, integration, doc) of the crates the gate will
   test: those the change touches, every crate built on them and every
   crate whose tests use them. Plus the repository's source scans when an
   effect or grade crate changed, and, if the gate's scope leaves them
   out, the render references (`klipp-compose`'s `golden_corpus`) and
   the conversion's render inputs (`klipp-convert`'s `render_inputs`)
   when a crate they read changed;
4. clippy and rustdoc with `-D warnings` on those crates;
5. the scripts' own tests, when `scripts/` changed.

On a pass it stamps the commit's **tree**, and the pre-push hook
refuses any push whose tree has no stamp:

```
pre-push: 1a2b3c4d has not passed scripts/pre-gate.sh — push refused.
```

Because the stamp is the tree, rewording a commit message keeps it; a
code change, or a rebase that changes the code, needs a new run. Warm,
it takes ten to twenty minutes, most of it the tests; its log is kept
in `.git/klipp-pre-gate/logs/`.

Its overrides each take a **reason**, not a flag, and the reason is kept
in `.git/klipp-pre-gate/skips.log`:

* `KLIPP_SKIP_PRE_GATE="<why>" git push` pushes without a stamp (a
  documentation-only branch, a mid-bisect push).
* `KLIPP_PRE_GATE_FORCE="<why>" scripts/pre-gate.sh` starts it although
  two other cargo jobs are already running on the machine. It refuses
  to start a third by default, since a third build makes all three slow
  and can run the machine out of memory.
* `KLIPP_PRE_GATE_ALLOW_REVERT="<why>" scripts/pre-gate.sh` lets through
  a branch that puts a file back exactly as it was before a recent
  `main` commit. That is what a squash onto a moved `origin/main` does
  by accident, so it is refused unless you say the revert is intended.
  Squash onto the commit you rebased on:
  `git reset --soft $(git merge-base HEAD origin/main)`.

Klipp's tests run locally rather than in CI, and this is a deliberate
choice rather than a gap we have not got round to.

The reason is that **the tests need a GPU and a cloud runner does not
have one**. Klipp's correctness argument for the colour pipeline is
that its CPU and GPU paths agree, and around ninety of the tests that
check this are written to skip when a GPU constructor returns `None`.
That is right on a machine with no GPU — and it means a cloud runner
reports a confident green on a build where a shader stopped compiling.
Your laptop is a *better* gate than GitHub's runner, not a worse one.

**There is no CI, and that is the current answer rather than a gap.**
A workflow was written and then deleted unrun, which is worth recording
so it is not rediscovered as an oversight:

* A cold build of this workspace — ffmpeg and whisper.cpp both build C —
  takes long enough on a hosted runner to be a poor fit for every push.
* Whatever it reported would be **advisory** for the GPU reason above,
  and a check that cannot be trusted must not be able to block a merge.
* A check nobody trusts is worse than no check. A red mark that is
  routinely correct to ignore teaches everyone to ignore red marks.

If CI is ever added it should be because someone wants a specific
signal, and it should be got green before it is wired to anything. Two
notes from the attempt, so the next person starts further along: the
Ubuntu package list in the [Prerequisites](#build-prerequisites) works
on a runner, and installing `mesa-vulkan-drivers` gives it **lavapipe**,
a software Vulkan driver — a real adapter, so shaders genuinely compile
and `klipp-effects`' `gpu_pipelines_build` canary is not a no-op. That
closes the largest hole a GPU-less runner would otherwise leave, though
it does not make the parity tests meaningful.

Generated-media tests use `target/test-scratch/` (under `CARGO_TARGET_DIR`
when set), sweep finished runs, and honor an explicit `TMPDIR`. The hook
also puts compiler scratch there by default and checks for at least 2 GiB
of free headroom before building. This avoids filling a small `/tmp`
tmpfs. If storage or quota errors occur, free space on the reported
filesystem or set `TMPDIR` to a disk-backed directory; `df` cannot detect
every per-user quota. Worktrees on tmpfs should point `CARGO_TARGET_DIR`
at a disk-backed build directory too.

The gate builds in a checkout of its own beside the main one
(`<repo>-gate`, or `KLIPP_GATE_DIR`), not in the worktree being
pushed. Each run checks the pushed commit out there; git rewrites only
the files that differ, and cargo rebuilds only their crates and what
depends on them. The pushing worktree is not built in or changed.
With `KLIPP_ALLOW_DIRTY=1`, a `CARGO_TARGET_DIR`,
`KLIPP_GATE_IN_PLACE=1`, or a gate checkout that cannot be used, it
builds in place as before. The gate checkout shows in
`git worktree list`: leave it there when tidying merged worktrees,
since removing it costs the next push a full build. Its build is let go
once the space it holds of its own (not what it shares with the main
checkout's build) passes `KLIPP_GATE_MAX_GB` (250), checked once a day
on btrfs.

#### What each step runs

| Step | Tests | clippy and rustdoc |
|---|---|---|
| `scripts/pre-gate.sh` | the crates the change **touches**, every target | the touched crates |
| the gate (`git push`) | the touched crates, every crate built on them, and every crate whose tests use one of those | the whole workspace |
| the gate, every 12 h, or `KLIPP_GATE_FULL=1 git push` | the whole workspace | the whole workspace |

`scripts/affected_crates.py` works out the affected crates from cargo's
metadata, for both steps. A change to a file the whole workspace shares
(`Cargo.lock`, `assets/`, `vendor/`, `scripts/`) or to a path it does
not know runs the whole suite, and documentation alone runs none. The
whole suite also runs when it has not passed for
`KLIPP_GATE_FULL_HOURS` (12) hours, so whatever a narrower run misses
is caught the same day.

You don't need to run `cargo test --workspace` yourself before a push;
the gate does what the change can reach. It's still the command for
checking everything at once.

A build folder's first build (the gate checkout's, or a worktree's built
in place) does not start from an empty `target/`: it takes a
copy-on-write copy of the main checkout's `target/debug` (on btrfs or
any filesystem with reflinks) and rebuilds the tree's own crates from
its sources, so only the dependencies come from the copy. Do not point
several worktrees at one `CARGO_TARGET_DIR`: cargo names a workspace
crate by its path within the workspace, the same in every worktree, and
judges freshness by file times, so one tree's build can link another
tree's crates. A worktree's `target/` can be deleted once its branch is
merged; the gate does not need it. The gate runs at the lowest CPU and disk priority, so the machine
stays usable while it builds; it takes longer when you are using it, not
the other way round.

Each gate that runs adds a line to `.git/klipp-pre-push.history` (start,
end, pass/fail/stopped, the step it ended in, branch, worktree; the last
200 kept), so what passed and what failed can be read after the queue
has moved on, and how long a gate takes. The checks' output is also kept,
in `.git/klipp-pre-push.logs/` (the last 50 runs), and the history line
names its log, so a failure can be read after the push that hit it has
gone.

When its turn comes, the gate fetches `main`. A branch that no longer
merges cleanly onto it is refused at once, before anything is built:
rebase it and push again. A branch that is behind `main` is tested merged
with it, which is what the squash merge will put on `main`, and then put
back exactly as it was pushed. (`KLIPP_GATE_AS_IS=1 git push` tests the
branch alone.) The routine that goes with it: the reviewer reviews while
the gate runs, and merges as soon as it passes and GitHub shows the PR
mergeable, so the next branch is tested on a `main` that already has it.

The gate also runs `cargo deny check advisories`, and needs cargo-deny
for it (`cargo install --locked cargo-deny`; `scripts/install-hooks.sh`
says so when it is missing). An advisory that `deny.toml` does not
record refuses the push until it is fixed by updating, or recorded there
with the reason it cannot reach Klipp.

The check has to run for a push to go. Without cargo-deny, or when it
cannot finish, the push is refused. Offline, it checks against the list
of advisories already on disk: past a week old it says so, and past 30
days it refuses, since an old list passes what a current one would not
(`cargo deny fetch db` refreshes it). A machine that cannot check pushes
with `KLIPP_SKIP_AUDIT=1 git push`: the gate says so, and the push's
line in the gate history is marked `unaudited`.

`KLIPP_SKIP_TESTS=1 git push` bypasses the hook for a genuine reason (a
work-in-progress branch, a mid-bisect push, a machine with no GPU). It
is not for getting a red suite past the gate.

**If you have no GPU**, say so in the PR. Your run will have skipped
the parity tests without telling you, and the maintainer will run the
full suite on real hardware before merging. That is the backstop, and
it is why an unverifiable "I ran the tests" costs nobody anything: the
claim is a courtesy that saves a round-trip, not the thing being
trusted.

### Checking authorship without pushing

The sign-off and authorship rules are checked by the same hook, and you
can run them over any range yourself:

```bash
scripts/check-signoff.sh                 # origin/main..HEAD
scripts/check-signoff.sh main..my-branch
```

It is instant, and it runs before clippy so a commit that needs
amending does not cost you a build first.

If you do not have a GPU, say so in the PR. The maintainer will run the
full suite locally before merging.

---

## Conventions that matter

1. **Every `.lock()` / `.read()` / `.write()` on shared state** uses
   `.unwrap_or_else(|e| e.into_inner())`. A worker-thread panic must
   not poison a mutex and freeze the UI. If you're introducing a new
   shared primitive, hold the same line.

2. **Two-phase decode loop.** In any ffmpeg code:
   drain `receive_frame` before feeding new `send_packet`. On EAGAIN
   from `send_packet`, drain output then retry once. `let _ =
   decoder.send_packet(…)` is a correctness bug — it silently loses
   packets on high-latency codecs.

3. **RationalTime::ZERO has rate = 1.** Do not use it as the LHS of
   arithmetic against a higher-rate time. Use
   `RationalTime::from_frames(0, rate)` to preserve rate.

4. **OFX callbacks** wrap their body in `ofx_guard!({ … })` and
   open with `check_non_null!(a, b, …)` for every pointer the plugin
   passes in. Never dereference a plugin-supplied pointer without
   null-checking it first.

5. **Trust boundary at import.** Untrusted-file paths (MLT, OTIO,
   FCPXML) route through `klipp_project::path_util::safe_resolve`,
   XML through `parse_xml_safe`, reads through `read_to_string_capped`.
   Our own `.klipp` format is trusted because `save::save_project`
   writes it.

6. **Colour math is linear-light f32.** Decoders emit f32 RGBA in the
   project working space, the grade chain operates on those linear
   values, and a single display transform at the end encodes for the
   viewer or the export. A new effect operates on linear f32 RGBA, and
   its CPU and GPU implementations must produce identical output.
   Gamut-aware ops carry a `_ws` variant that takes the working space's
   luma coefficients; the `_bt709` form is a thin wrapper kept for
   byte-identity with pre-migration code. The ACES output transforms are
   checked against OCIO — see the colour-pipeline section of
   `ARCHITECTURE.md`.

7. **WGSL uniform padding** uses individual scalar `_pad0: u32,
   _pad1: u32, …` fields. Never `array<u32, N>` on the WGSL side —
   naga validation fails silently and the effect breaks at runtime.
   The Rust `#[repr(C)]` struct can still use `[u32; N]`.

8. **Nothing lands on `main` except through a PR** — see
   [How work gets merged](#how-work-gets-merged). `main` is protected,
   so this is enforced rather than merely asked for.

---

## Commit style

* One logical change per commit. Bundle aggressively within a single
  file (e.g. three related one-liners in `history.rs`); split
  aggressively across files (renaming a type is not the same commit
  as a bugfix).
* Messages explain **why**, not **what** — the diff shows the what.
* Short subject line (≤ 70 chars), imperative mood, no trailing full
  stop. Blank line. Body wrapped at ~72.
* No `Co-Authored-By` trailers, and no AI system as author or
  committer — see [Authorship and AI-assisted contributions](#authorship-and-ai-assisted-contributions).
* Sign off with `git commit -s`.
* No references to audit-internal item IDs or internal planning
  documents — commit messages are the public-facing record of the
  code's history.

Example:

```
Recover from poisoned locks instead of cascading panics

Every .lock().unwrap() … was .lock().unwrap_or_else(|e| e.into_inner()).

Previously, a single panic on any worker thread would poison every
shared Mutex it held and then any subsequent access from another
thread would panic again in an unrecoverable cascade.

The replacement recovers by extracting the guard from the
PoisonError. The underlying data may be in a partially-updated
state, but for the data behind these locks a stale or partial value
is always preferable to a crash.
```

---

## Pull request checklist

Before you open a PR:

0. Every commit is signed off (`git commit -s`), authored under your
   own name and email, with no agent as author, committer or
   co-author.
1. `scripts/pre-gate.sh` passed on the commit you push: the pre-push
   hook refuses a push without its stamp. It runs `clippy -D warnings`,
   rustdoc and the tests of the crates you touched.
2. The gate passed on `git push`: clippy and rustdoc over the whole
   workspace, and the tests the change can reach ([What each step
   runs](#what-each-step-runs)).
3. `cargo +nightly check -p klipp-ui` produces no new
   f64→f32-fallback warnings (rust-lang/rust#154024). Run
   `cargo +nightly fix --lib -p klipp-ui` if needed.
4. If you touched an `unsafe` block, it carries a `// SAFETY:`
   comment enumerating the preconditions.
5. If you added a crate-level dependency: `cargo +nightly udeps
   --workspace --all-targets` afterwards confirms it's used.
6. If you touched a call path that can panic, consider: can this
   panic now poison a Mutex that another thread holds? If yes, wrap
   the thread's entry point in `catch_unwind` instead.

---

## Pull requests from forks

Contributors outside the repository work from a fork, which has no
pre-push hook, so nothing has checked their commits. Before merging,
check the sign-offs by hand: `gh pr checkout <number>`, then
`scripts/check-signoff.sh main..HEAD`. There is no CI to catch it.

## Releasing

Follow [the package release guide](packaging/RELEASING.md) to build portable
Arch packages from the exact version tag, test them, sign them and stage the
package repository. A local build is not a published release.

For the maintainer, and written down so it is the same every time.

1. `main` is green and the working tree is clean.
2. Tag a candidate: `git tag -a v0.1.0-rc.1 -m "v0.1.0-rc.1"` and push
   the tag. Live with it for a few days on real work.
3. If it holds, tag the release: `git tag -a v0.1.0 -m "v0.1.0"`, push,
   and cut a GitHub Release from the tag with notes written from the
   commits since the last one.
4. Only if a shipped release later needs a fix while `main` has moved
   on: `git switch -c release/0.1.x v0.1.0`, fix there, tag `v0.1.1`,
   and cherry-pick the fix onto `main`. Do not create that branch in
   advance.

---

## Security

Do not open a public issue for a security problem. `SECURITY.md` says
where to send it.

---

## Where to ask questions

`ARCHITECTURE.md` answers most structural questions. For specific
invariants, the relevant crate's `//!` docstring is the fastest
lookup (`cargo doc --open`, navigate to the crate).

Beyond that: open an issue. A question that took you an hour to answer
from the source is a documentation bug, and saying so is a
contribution.

## GPU-first picture processing

New picture effects, transitions, generators and title animations must prefer a
usable GPU. Include GPU/CPU image-parity tests, residency classification and an
active playback fixture. CPU twins remain for fallback and reference rendering;
bounded cache preparation, decode, audio and offline analysis are distinct.

Report CPU picture work through the shared residency diagnostics even when the
final composite stays on GPU. Nonresident routes require a named tracking issue
and an explicitly reviewed exception; the gate pins route identities as well as
classifications. See [the residency policy and tests](docs/rendering-residency.md#gpu-first-enforcement-2077).
