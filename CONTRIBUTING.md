# Contributing to Klipp

Thanks for taking a look. This is a short, practical guide — read
`ARCHITECTURE.md` alongside for the system map.

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
* **Vulkan** — wgpu needs a working Vulkan ICD. Nvidia users: note
  that CachyOS puts `nvidia_icd.json` under `/usr/share/vulkan/icd.d/`
  rather than `/etc/vulkan/icd.d/`, so the binary sets `VK_DRIVER_FILES`
  at startup if it finds the file there.
* **PipeWire** (recommended) / ALSA — cpal uses whichever is available.

## Running

```bash
cargo rr                 # cargo run --release (aliased in .cargo/config.toml)
cargo r                  # cargo run (debug, slower but faster to compile)
cargo c                  # cargo clippy --workspace --all-targets
cargo test --workspace   # every test
cargo test -p klipp-engine --lib   # a single crate's lib tests
```

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

None of these are wired into CI yet. Contributions welcome.

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

6. **Colour math is currently 8-bit sRGB-byte.** If you're adding a
   new effect, match the convention (the CPU / GPU pair must produce
   identical output). When the linear-light pipeline lands, every
   effect will flip in one coordinated change.

7. **WGSL uniform padding** uses individual scalar `_pad0: u32,
   _pad1: u32, …` fields. Never `array<u32, N>` on the WGSL side —
   naga validation fails silently and the effect breaks at runtime.
   The Rust `#[repr(C)]` struct can still use `[u32; N]`.

8. **No commits without explicit OK.** Direct-to-main is discouraged;
   unsaved user work on a broken change is expensive.

---

## Commit style

* One logical change per commit. Bundle aggressively within a single
  file (e.g. three related one-liners in `history.rs`); split
  aggressively across files (renaming a type is not the same commit
  as a bugfix).
* Messages explain **why**, not **what** — the diff shows the what.
* Short subject line (≤ 70 chars), imperative mood, no trailing full
  stop. Blank line. Body wrapped at ~72.
* No `Co-Authored-By` trailers. No references to audit-internal item
  IDs or internal planning documents — commit messages are the
  public-facing record of the code's history.

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

Before you open a PR (or before you ask me to review, if we're
pair-shipping):

1. `cargo c` clean — zero new clippy warnings.
2. `cargo test --workspace` green.
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

## Where to ask questions

`ARCHITECTURE.md` answers most structural questions. For specific
invariants, the relevant crate's `//!` docstring is the fastest
lookup (`cargo doc --open`, navigate to the crate).
