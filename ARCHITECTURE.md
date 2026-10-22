# Architecture

This document is the developer-facing map of Klipp. It's intended as the
first thing a new contributor reads after `README.md` — enough to
understand where things live, how they fit together, and which
invariants matter. For a full API reference run `cargo doc --open` once;
every crate now carries a `//!` overview.

---

## Workspace

Klipp is a 42-crate Cargo workspace. Dependencies flow strictly
bottom-up — no cycles — and every crate depends on `klipp-core`.

```
klipp-app ── the binary; wires everything.
    └── klipp-ui ── egui desktop editor
            ├── klipp-control ── socket control surface for a running app
            │       └── klipp-mcp ── Model Context Protocol server
            ├── klipp-document ── undoable edit history + command apply
            ├── klipp-analysis ── shots, audio, motion, transcript, framing
            └── klipp-engine ── real-time playback
                    ├── klipp-compose ── the one definition of a rendered frame
                    ├── klipp-render ── offline export
                    │       └── klipp-export ── render-queue model
                    ├── klipp-hdr-out ── HDR reference output (viewer + export)
                    ├── klipp-effects ── GPU effects + scope pipeline (wgpu)
                    ├── klipp-color ── colour grading, LUTs, scopes, log decoders
                    │       └── klipp-grade-model ── primary-grade maths (BT.1886)
                    ├── klipp-audio ── DSP, LUFS, mixer
                    ├── klipp-nodes ── colour-grading node graph
                    ├── klipp-plugin ── OFX 1.5 host (C ABI, dlopen)
                    ├── klipp-frei0r ── frei0r 1.x host
                    ├── klipp-title ── title / text rasteriser
                    ├── klipp-transitions ── transition kernels
                    ├── klipp-ml ── model manifest + registry
                    │       ├── klipp-ml-ort ── ONNX (ort) inference
                    │       └── klipp-ml-candle ── Candle inference
                    ├── klipp-timeline ── Sequence / Track / Clip model
                    │       └── klipp-validation ── invariant checks
                    ├── klipp-project ── .klipp save/load + importers
                    └── klipp-media ── MediaPool + on-disk caches
                            └── klipp-core ── IDs, time, events, config
```

`klipp-jobs` (background scheduling), `klipp-perf`, `klipp-smoke` and
`klipp-testing` (perf / headless / snapshot harnesses) sit off to the
side of the editor layers.

Structural rule: if two crates both need a shared type, it goes down to
`klipp-core` (or, for timeline-model types, `klipp-timeline`). Crates
never import from peers at the same layer.

---

## Threading model

Klipp is single-process with a small handful of long-lived threads.

| Thread | Crate | Role |
|---|---|---|
| UI thread | `klipp-ui` | egui rendering, user input, state mutations. **Must not block.** |
| Engine thread | `klipp-engine` | Owns `PlaybackEngine`, drains `AppCommand`s, advances the playhead, composites frames. |
| Video decode threads | `klipp-engine` | One per open `VideoDecoder`. ffmpeg-next software / CUDA / VAAPI. |
| Audio decode threads | `klipp-engine` | One per active `AudioDecoder`. Feeds a lock-free ringbuf. |
| Mix thread | `klipp-engine` | Drains per-track audio stagings, sums, applies per-clip FX, writes to the cpal output ring. FTZ+DAZ enabled to avoid denormal stalls. |
| cpal callback | `klipp-engine` | OS-owned. Pulls from the mix ring and drives the output device. |
| Thumbnail / filmstrip worker | `klipp-ui` | On-demand rendering of browser thumbs and timeline filmstrips. |
| Export thread | `klipp-render` | One per active `ExportJob`. Bounded pool via `klipp-export::RenderQueue`. |
| Scene detect / stabilise / analyse | various | Short-lived; fire-and-join per job. |

Communication is exclusively via crossbeam channels (`AppCommand` /
`AppEvent` in `klipp-core`). There is no direct cross-thread method call
between the UI and the engine. Shared state (the current `Sequence`, the
`MediaPool`, the `FrameBuffer`) lives behind `Arc<Mutex<_>>` /
`Arc<RwLock<_>>` and is recovered from lock poisoning via
`.unwrap_or_else(|e| e.into_inner())` everywhere — a single worker-thread
panic does not cascade into a UI freeze.

---

## Key subsystems

### Timing

The editor's time primitive is [`klipp_core::RationalTime`]: a `(value:
i64, rate: u32)` pair. Arithmetic on two times rescales the right-hand
side to the left-hand side's rate.

**The `RationalTime::ZERO` gotcha** — the constant has `rate = 1`. Using
it as the LHS of subtraction against a higher-rate time (typical clip
timeline: rate 24 or 48000) silently rescales the RHS down to whole
seconds, losing sub-second precision. Use
`RationalTime::from_frames(0, rate)` when you need a rate-preserving
zero. This has bitten us three times during audit; the magnetic timeline
especially is allergic to ZERO.

### Magnetic timeline

`klipp-timeline/src/magnetic.rs` implements FCP-parity editing. Every
mutation goes through `MagneticEditor::<op>` which returns a
`MagneticEditCommand` carrying its own inverse — the UI's history stack
can round-trip without snapshotting the full sequence. New editing
features should add a variant to `MagneticEditCommand` and both halves
(apply + invert) to `apply_forward` / `apply_inverse`.

There is a parallel path (`Track::blade_at`, `Track::ripple_trim_left`,
`Track::ripple_trim_right`, `Track::ripple_remove` in `sequence.rs`)
that mutates state without emitting commands and therefore bypasses
undo. Prefer `MagneticEditor::` for any new call.

### Playback engine

`klipp-engine::PlaybackEngine` runs on its own thread and owns
everything to do with live playback: the `Sequence` (shared), the
`MediaPool` (shared), the video and audio decoders, the mix thread, the
audio output, the render cache, and the frame buffer the UI reads from.

The single source of truth for "what time is it" is
`sync::MasterClock` — video and audio both slave to it. See
`sync.rs` for the `seek`/`pause`/`resume` state machine and the
generation counter used by callers to detect stale state.

**Decode-pipeline invariant — two-phase decode.** Always drain
`receive_frame` before feeding new packets with `send_packet`. Mixing
the order causes EAGAIN packet loss on high-latency codecs (AV1, HEVC
with B-frames, VP9). On EAGAIN from `send_packet`, drain output frames
then retry once — all helpers in this crate follow the rule; new code
must too.

### Offline render

`klipp-render` is the export pipeline. The live `Sequence` is frozen
into a `SequenceSnapshot` and handed to an `ExportJob`:

```
snapshot → decode → colour grade → OFX effects → builtin effects
        → compositor → encoder (ffmpeg / NVENC)
        ↘ audio mixdown → encoder audio
```

Stage order is fixed (colour grade → OFX → builtin). Every failure
becomes a `RenderError` and aborts the job — partial or drifted output
files are never preferred over a loud failure. Export is currently
**not atomic** (writes directly to the target path); a
`.tmp` + rename refactor is tracked.

### Colour pipeline

The colour pipeline is linear-light end-to-end: decoders emit f32
RGBA in the project working space, the grade chain operates on those
linear values, and a single display transform at the end of the chain
encodes for the viewer or the export. Working space is a per-project
choice — `klipp_core::WorkingColorspace` selects between
`Rec709Linear` (default), `AcesCG` (AP1 primaries), and `Bt2020Linear`
(stub today, aliased to Rec.709 in display).

`klipp-color` houses the maths: the ACES 1.3 RRT line-for-line, four
Output Display Transforms (Rec.709 SDR cinema, Rec.2100 PQ at 1000 /
4000 nit, DCI-P3 D65 48 nit), primaries matrices between every
working-space pair (numerical f64 inverses so round-trips are exact),
and per-working-space luma coefficients for all gamut-aware grade ops
(saturation, vibrance, tone controls, lift/gamma/gain, log wheels,
etc.) Each op has a `_ws` variant; the `_bt709` form is a thin
wrapper kept for byte-identity with pre-Phase-E code.

Camera-log decoders (`klipp-color::transforms`) are whitepaper-correct
where verified — Apple Log, LogC3/4, SLog2/3, PQ, HLG. Nikon N-Log
and a few others are still pending verification against reference
material. The IDT lifts log-encoded YUV / RGB straight to the
project working space — no sRGB re-encode round-trip.

The grade graph (`klipp-nodes`) carries an optional per-chain
`working_space_override`. When set, the executor swaps its active
working colorspace + GPU luma uniform for the duration of that
chain's execution and restores on every exit. The migration node
`PrimariesMatrix` applies a fixed 3×3 matrix between any two working
spaces; the project-info "Convert grades" button wraps each clip's
chain with `Source → PM(new→old) → ... → PM(old→new) → Output` and
pins the override so an existing grade keeps producing the same
pixels after a working-space switch.

Export (`klipp-render`) carries a delivery-target ODT choice
(`klipp_core::OdtChoice`) on `ExportSettings`. The default
`MatchViewer` mirrors the viewer's display path; explicit Rec.709 SDR
/ HDR PQ / DCI-P3 D65 ODTs route through the AcesCG-basis chain (with
a BT.709 → AP1 walk when the project is Rec.709-linear so the ODT
sees the right basis).

LUTs in `klipp-color::lut` carry input/output primaries tags on
`LutData`. The lookup itself is always on the sRGB encoding (as a
`.cube` look is built); a tag says which primaries it was built in, and
`lut_bracket` gives the matrices that take the working space into the
LUT's input primaries before the lookup and its output primaries back
after, so a Rec.709-authored .cube applied to an AcesCG project doesn't
silently shift gamut. The CPU (`klipp_effects::apply_3d_lut_bracketed`,
the one lookup shared with the global LUT) and the GPU LUT pass apply
the same pair. The Lut3D inspector node exposes both tags as dropdowns,
and the node's choice wins over the file's.

### OFX plugin host

`klipp-plugin` is the workspace's only C-ABI FFI crate and the
highest-unsafe-density target. It dlopens third-party `.ofx.bundle`
effects plugins, exposes the OFX C API through suite function tables,
and bridges the plugin's GPU context to our wgpu / CUDA / OpenCL /
OpenGL runtimes.

Every `unsafe extern "C" fn` a plugin can call goes through two guards:

* `check_non_null!(a, b, …)` — early-returns `K_OFX_STAT_ERR_VALUE` for
  any null pointer; prevents a buggy plugin from turning us into a
  SIGSEGV via `*out = value`.
* `ofx_guard!({ body })` — wraps the body in `std::panic::catch_unwind`,
  converts a Rust panic into `K_OFX_STAT_ERR_FATAL`. Panicking across
  `extern "C"` is UB on Rust 2021 and aborts on 2024.

Two known latent issues still need a dedicated refactor pass:

1. `paramGetValue` / `paramSetValue` are variadic in the OFX spec but
   implemented as non-variadic and `mem::transmute`'d into the suite
   table. Works today by coincidence on x86-64 SysV ABI with the
   OpenFX C++ support lib's call convention, but is formally UB.
2. `prop_get_string_fn` returns a raw `*const c_char` that borrows
   from a `CString` inside a Mutex-protected store. The Mutex drops
   before the plugin sees the pointer; a subsequent `remove_propset`
   or `set_string` on the same propset frees the underlying `CString`.

### File I/O trust boundary

`klipp-project` draws a hard line between trusted and untrusted input.

| Source | Trust | Path resolver |
|---|---|---|
| Our own `.klipp` load | trusted (we wrote it via `save::save_project`) | `path_util::resolve_media_path` — no traversal check |
| MLT / Kdenlive import | untrusted | `path_util::safe_resolve` — rejects `..` escape |
| FCPXML import | untrusted | `path_util::safe_resolve` |
| OTIO import | untrusted | `path_util::safe_resolve` |
| Media consolidation | untrusted outputs | symlink rejection on every copy source |

All file reads go through `read_to_string_capped` / `read_capped` (64
MiB default cap); all XML through `parse_xml_safe` (roxmltree with
`allow_dtd: false`, 1M-node limit). All atomic writes go through
`path_util::atomic_write` with a collision-proof
`.<file>.tmp.<pid>.<nanos>` scratch name.

### Media pool

`klipp-media::MediaPool` is the flat owner of every imported item plus
a bin tree for folder-like grouping. Items reference their underlying
source file by absolute path (converted to project-relative on save,
back to absolute on load). Offline media is represented by
`MediaItem::offline = true`, never by removal — the UI and engine both
check the flag before opening the file.

Three on-disk caches sit alongside the pool:

* **Audio cache** (`audio_cache`) — conformed PCM keyed by media
  fingerprint. Lets the engine skim / scrub at zero decode cost.
* **Thumbnail cache** (`thumbnail`) — ThumbnailCache + FilmstripFrame.
* **Waveform** (`waveform`) — mono peak buckets for the timeline's
  audio lanes.

### Control surface & the AI assistant

Four crates let an agent drive a running Klipp through tools alone,
with every edit going through the normal history so it undoes like a
hand edit.

* **`klipp-document`** owns the edit history off the UI. `Op`
  (`ops.rs`) is the agent-facing intent — place, trim, split, move,
  grade, `qualify` (HSL secondary), `look` (LUT), `match_grade`,
  `clean_audio`, `reframe`, `compose` (place a picture smaller than
  the frame, which `corners`, `shadow` and `outer_glow` then treat as a
  layer), `retime`, `transition`, `effect`, `captions`, and so on. `resolve` simulates a batch of ops on a clone
  and returns a `Plan` of `EditCommand`s (or a `Refusal` naming the
  step, the reason, and the fix); the UI applies the plan as one
  history batch. `qc::check` is the delivery quality-check: black and
  flash frames, jump cuts, titles across a cut, sound cut inside a
  word, music under speech, loudness, the brief.

* **`klipp-analysis`** turns footage into facts, each analyser an
  ffmpeg or on-device-model pass cached on disk: shot boundaries,
  the audio envelope (silence, EBU R128 loudness, and the spectral
  bands that tell wind / rumble / hum / hiss apart), motion, the
  transcript (Whisper, with word times), and framing (RF-DETR).
  `audio_sync` cross-correlates two clips' sound for dual-system sync.

* **`klipp-control`** exposes a running instance over a Unix-socket
  JSON protocol (`ControlCommand` in `protocol.rs`): read the state,
  run analysers, `plan` / `apply` document ops, grab a frame's scope
  readings (`frame_stats`) or a scope image (`scope`), find clean cut
  points, check, export. Writes carry a revision so a stale batch is
  refused rather than misapplied.

* **`klipp-mcp`** wraps that socket as a Model Context Protocol
  server over stdio, with a tool per command and text views written
  for an agent to read. The craft guidance it serves lives in
  `assets/craft/`.

The control layer runs inside the app process and talks to the engine
through the same `AppCommand` / `AppEvent` channels as the UI; it never
reaches into engine state directly.

---

## Build / test / run

```bash
cargo rr                 # release build + run
cargo c                  # workspace clippy (aliases in .cargo/config.toml)
cargo test --workspace   # all tests
cargo test -p klipp-engine --lib    # single-crate
```

The workspace uses `lld` as the linker (`.cargo/config.toml`). `mold`
is not currently wired in.

Logs land in `klipp-debug.log` in the workspace root — tail it while
running to watch engine events.

---

## Conventions

* **Error handling is a patchwork.** New code should prefer
  per-crate `thiserror` enums (`klipp-render::RenderError`,
  `klipp-project::MltImportError`, etc.) and either propagate via `?`
  or convert with explicit `map_err`. Avoid `Result<_, String>` — it
  loses source chains and isn't pattern-matchable. `klipp-core` has a
  `KlippError` that is under-adopted and will be expanded rather than
  replaced.
* **Every `.lock()` / `.read()` / `.write()` on shared state** uses
  `.unwrap_or_else(|e| e.into_inner())` to recover from poisoning. A
  worker-thread panic should surface as a UI toast, not a freeze.
* **Unsafe** blocks in new code carry a `// SAFETY:` comment explaining
  preconditions. The existing surface is audit-in-progress.
* **Commits** are single-purpose. Commit messages focus on the *why*.
  No `Co-Authored-By` trailers.

---

## Where things tend to break

A small honest-map of the landmines we've hit:

* **Audio on multi-clip timelines** — the preroll-hit path in
  `advance_to_next_clip` must reopen audio decoders when the next clip
  uses different media, not just seek. If only clip 1's audio plays
  through the timeline, `seek_all_audio_to_timecode`'s media-mismatch
  check regressed.
* **RationalTime::ZERO in magnetic ops** — see above.
* **Single-variant enums after deletion** — when removing a variant,
  consider flattening the enum if only one remains (`FrameData::Nv12`
  was deleted; `FrameData` became `Vec<u8>` directly).
* **Lock-poison cascade** — any new `.lock().unwrap()` on shared state
  re-opens the whole-app-freeze-on-worker-panic class of bugs.
* **WGSL uniform arrays** — `array<u32, N>` fails naga validation
  silently. Always use individual scalar `_padN: u32` fields on the
  WGSL side, regardless of what the Rust `#[repr(C)]` struct looks
  like.
