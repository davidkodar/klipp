# Architecture

This document is the developer-facing map of Klipp. It's intended as the
first thing a new contributor reads after `README.md` — enough to
understand where things live, how they fit together, and which
invariants matter. For a full API reference run `cargo doc --open` once;
every crate now carries a `//!` overview.

---

## Workspace

Klipp is a 42-crate Cargo workspace with no dependency cycles (Cargo
enforces that). Most crates depend on `klipp-core`; the 15 that don't
are self-contained: file formats, maths, vendored code, the ML
runtimes, the job scheduler, and two of the binaries. Three binaries ship: `klipp-app` (the editor),
`klipp-mcp` (the MCP server an assistant starts) and `klipp-hdr-out`
(the HDR reference presenter the viewer starts). The converter and the
harnesses have binaries of their own for development.

| Group | Crates |
|---|---|
| App and surfaces | `klipp-app` (the binary; wires everything), `klipp-ui` (the egui editor), `klipp-control` (socket control of a running app), `klipp-mcp` (Model Context Protocol server over that socket) |
| Editing model | `klipp-timeline` (Sequence / Track / Clip, the magnetic editor), `klipp-document` (undoable edits, agent ops, plans, quality check), `klipp-project` (`.klipp` save/load, importers and exporters), `klipp-validation` (pre-flight checks), `klipp-graph-format` (how a clip's picture graph is written), `klipp-graph-lift` (the picture lifted to the graph and back), `klipp-convert` (the one-time project conversions) |
| Picture | `klipp-compose` (the one definition of a rendered frame), `klipp-nodes` (the per-clip node graph and its executor), `klipp-effects` (GPU effects and scopes on wgpu, with their CPU references), `klipp-color` (colour science, ACES, LUTs, log decoders), `klipp-grade-model` (primary-grade maths), `klipp-title` (titles and text), `klipp-linebreak` (vendored line breaking for titles), `klipp-transitions` (transition kernels), `klipp-plugin` (OFX 1.5 host), `klipp-frei0r` (frei0r host) |
| Playback and delivery | `klipp-engine` (real-time playback), `klipp-render` (export), `klipp-export` (render-queue model), `klipp-hdr-out` (HDR reference output, a separate process) |
| Media and sound | `klipp-media` (MediaPool, on-disk caches), `klipp-audio` (DSP, LUFS, the mixer), `klipp-captions` (subtitle file formats), `klipp-audio-search` (music and effects search), `klipp-deepfilter` (vendored DeepFilterNet3 voice isolation) |
| Analysis and ML | `klipp-analysis` (analysers that turn footage into facts), `klipp-ml` (model manifest and registry), `klipp-ml-ort` (ONNX Runtime, loaded at run time), `klipp-ml-candle` (Candle inference) |
| Foundations | `klipp-core` (IDs, time, events, settings), `klipp-jobs` (background scheduling that yields to playback), `klipp-load-lock` (one lock around loading a library and starting a GPU) |
| Harnesses | `klipp-perf`, `klipp-smoke`, `klipp-testing`, `klipp-corpus` (reproducible test media and projects), `klipp-readiness` (the readiness and residency inventories) |

Structural rule: if two crates both need a shared type, it goes down to
`klipp-core` (or, for timeline-model types, `klipp-timeline`) rather
than one crate reaching up into another.

---

## Threading model

The editor is one process. Two companions run as processes of their
own: `klipp-mcp`, started by an assistant and talking to the editor's
control socket, and `klipp-hdr-out`, started by the viewer to show the
HDR picture on a surface that can carry it. Inside the editor a few
threads live as long as the app, and named workers come and go.

| Thread | Crate | Role |
|---|---|---|
| UI thread | `klipp-ui` | egui rendering, user input, state mutations. **Must not block.** |
| Engine thread | `klipp-engine` | Owns `PlaybackEngine`, drains `AppCommand`s, advances the playhead, composites frames. |
| Video decode threads | `klipp-engine` | One per open `VideoDecoder`: NVDEC (cuvid) / CUDA / VAAPI, or software. A stream whose GPU decoder refuses work goes on in software. |
| Audio decode threads | `klipp-engine` | One per active `AudioDecoder`. Feeds a lock-free ringbuf. |
| Mix thread | `klipp-engine` | Drains per-track audio stagings, sums, applies per-clip FX, writes to the cpal output ring. FTZ+DAZ enabled to avoid denormal stalls. |
| cpal callback | `klipp-engine` | OS-owned. Pulls from the mix ring and drives the output device. |
| Thumbnail / filmstrip worker | `klipp-ui` | On-demand rendering of browser thumbs and timeline filmstrips. |
| Export thread | `klipp-render` | One per active `ExportJob`, queued by `klipp-export::RenderQueue`. |
| Control socket | `klipp-control` | A listener, and a thread per connection (an MCP server, a script). |
| Background jobs | `klipp-jobs` | A scheduler for skim files, filmstrips and audio conform that yields to playback. |
| Workers | various | Named, on demand: analysis, model downloads and inference, translation, narration, multicam sync, scene detect, stabilise. |

The UI and the engine talk through crossbeam channels (`AppCommand` /
`AppEvent` in `klipp-core`); there is no direct cross-thread method call
between them. Shared state (the current `Sequence`, the
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

**`RationalTime::ZERO`** has `rate = 1` and takes the other side's rate
in arithmetic, whichever side it is on: `ZERO + t` and `ZERO - t` are
exact at `t`'s rate (#2450). Ordering (`<`, `cmp`) compares values
across rates. Equality does not: `==` is structural, so `ZERO` is not
`==` a zero at rate 24. Compare times for "the same moment" with `cmp`,
or `== RationalTime::from_frames(0, rate)` at the rate you mean.

### Magnetic timeline

`klipp-timeline/src/magnetic.rs` implements FCP-parity editing. Every
mutation goes through `MagneticEditor::<op>` which returns a
`MagneticEditCommand` carrying its own inverse — the UI's history stack
can round-trip without snapshotting the full sequence. New editing
features should add a variant to `MagneticEditCommand` and both halves
(apply + invert) to `apply_forward` / `apply_inverse`.

`Track::blade_at`, `Track::ripple_trim_left`, `Track::ripple_trim_right`
and `Track::ripple_remove` in `sequence.rs` change a track without
emitting a command of their own. Their production callers all run inside
a recorded history step: the stabilisation scene split (recorded as
`LegacySplitAtSceneMarkers`), the document blade on a non-magnetic track
(`LegacyBladeClip`), the `apply` of `LegacyRippleRemove` and
`LegacyCutClip`, and the planner's blade on a scratch copy. The two
trims are left to tests. Prefer `MagneticEditor::` for any new call.

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

### Rendering: viewer and export

A clip's picture is a node graph. Each clip carries its grade, effects
and picture animation; lifted to the graph format (`klipp-graph-lift`,
written as `klipp-graph-format`), they are one chain in render order:
the grade nodes, the display conversion, OFX plug-ins, the built-in
effects, and the placement where the clip's transform goes. A
picture-truth clip, whose graph holds what the older fields cannot (an
effect's mix or mask, a published grade port), keeps the graph itself
as the truth. Project files are at format revision 4.

`klipp-compose` is the one definition of a rendered frame, and the
viewer (`klipp-engine`) and export (`klipp-render`) both call its
`Renderer`. Per frame it renders each visible track's clip as its own
layer, running the clip's graph through `klipp-nodes`' `GraphExecutor`
at clip-relative time, stacks the layers bottom to top with clip ×
track opacity applied once at the blend, renders a transition from both
of its clips as full layers, and applies the global LUT to the
composite. Its rules are listed at the top of `render.rs`. On a usable
GPU the work runs there (wgpu, with decoded NV12 / P010 frames converted
by shaders and kept resident); the CPU path is the fallback and the
reference the GPU is tested against. `docs/rendering-residency.md`
lists what stays on the GPU. The effects' compute shaders are `.wgsl`
files under `crates/klipp-effects/src/gpu/shaders/`, one per shader, the
pieces several share under `shaders/include/`; the crate's build script
assembles them (`//#include <name>`) for `gpu_shader!` to take in.

Export freezes the live `Sequence` into a `SequenceSnapshot` and hands
it to an `ExportJob`. `export_frames` makes each frame with the same
renderer, converting to the encoder's YUV planes on the GPU when the
delivery allows; the audio is mixed down beside it with the playback
mix's rules. Every failure becomes a `RenderError` and aborts the job.
Export is atomic: the encode goes to a `.partial` sibling of the target
and is renamed into place only when it is complete (`export_job.rs`),
so a failed or cancelled export never leaves a file at the target path.

### Colour pipeline

The colour pipeline is linear-light end-to-end: decoders emit f32
RGBA in the project working space, the grade chain operates on those
linear values, and a single display transform at the end of the chain
encodes for the viewer or the export. Working space is a per-project
choice — `klipp_core::WorkingColorspace` selects between
`Rec709Linear` (default), `AcesCG` (AP1 primaries), and `Bt2020Linear`
(BT.2020 primaries; converted and gamut-mapped to BT.709 for an SDR
display). The ACEScg path is the most tested.

`klipp-color` houses the maths: the ACES 1.3 RRT line-for-line, five
Output Display Transforms (Rec.709 SDR, Rec.2100 PQ at 1000 / 4000
nit, DCI-P3 D65 48 nit, Rec.2020 SDR), each checked against OpenColorIO
(`tests/ocio_parity.rs`), primaries matrices between every
working-space pair (numerical f64 inverses so round-trips are exact),
and per-working-space luma coefficients for all gamut-aware grade ops
(saturation, vibrance, tone controls, lift/gamma/gain, log wheels,
etc.) Each op has a `_ws` variant; the `_bt709` form is a thin
wrapper kept for byte-identity with pre-Phase-E code.

Camera-log decoders (`klipp-color::camera_log`) are whitepaper-correct
where verified — Apple Log, LogC3/4, SLog2/3, PQ, HLG — and the curves
are checked across all 1024 10-bit code values against OpenColorIO and
colour-science (`tests/camera_log_parity.rs`). Nikon N-Log and one or
two others still want a reference round-trip. The IDT lifts log-encoded YUV / RGB straight to the
project working space — no sRGB re-encode round-trip.

`klipp-nodes` is the picture graph, not only a grade graph: besides
the grading nodes it has keys and mattes, layer and blend nodes, the
effect, OFX and frei0r nodes, denoise, film grain, compounds and the
display conversion. A graph carries an optional per-chain
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
`MatchViewer` mirrors the viewer's display path; the explicit ODTs
route through the AcesCG-basis chain (with a walk into AP1 when the
project is in another working space, so the ODT sees the right basis),
and `Srgb` is the legacy encode without the RRT.

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

A string property handed to a plugin (`prop_get_string_fn`,
`prop_get_string_n_fn`) is a `*const c_char` into a `CString` its
property set owns. A string that is replaced or reset is kept in the
set's `retired` list rather than freed, so the pointer a plugin was given
reads the same bytes until `remove_propset` destroys the set, as the OFX
spec expects (#2449).

`paramGetValue` / `paramSetValue` and the `…AtTime`, derivative and
integral getters are variadic in the OFX spec, and are C functions in
`klipp-plugin/src/ofx_param_shim.c`, built by `build.rs` with the `cc`
crate (#2455): only C reads a `va_list`. Each asks Rust what the
parameter holds (`klipp_ofx_param_shape`: double, int or string, and how
many components), reads that many arguments, a pointer per component to
get into or each value as C promotes it to set, and calls a typed Rust
function with a plain array, inside the same `ofx_guard!`.

### File I/O trust boundary

`klipp-project` draws a hard line between trusted and untrusted input.

**A file from elsewhere is untrusted, even in Klipp's own format.** A
`.klipp` project, a project or title template, a composition template,
a recipe or a `.klipppack` may have been written by anyone, so what it
names is checked like any import:

* **File names** taken from it are joined only through
  `path_util::inside(root, parts)` (each part one plain path component)
  or made with `klipp_core::safe_name`, and only regular files are
  copied, never links.
* **Sizes**: every archive entry is read through a bounded reader
  against a per-entry and a total cap, and an archive's file count is
  capped; text files are read with `read_to_string_capped`.
* **Plug-ins** (LADSPA, frei0r, CLAP, LV2) are loaded only from the
  lists their scans of the plug-in folders found: an effect id names a
  plug-in, it never chooses a library.
* **Paths Klipp derives** (proxies, analysis and disk caches) are made
  from ids and hashes. A stored one is checked against the shape Klipp
  makes before anything is copied from it, and Klipp deletes or
  overwrites only the files it would make itself.
* **Times and counts** read from a file (subtitle cues, EDL channels)
  are checked to be finite and in range before they become clips or
  tracks.

| Source | Trust | Path resolver |
|---|---|---|
| A `.klipp` project | untrusted (the format is ours, the file may not be) | `path_util::resolve_media_path` for media paths, which may be absolute; derived paths as above |
| MLT / Kdenlive import | untrusted | `path_util::safe_resolve` — rejects a relative path whose `..` escapes the project folder |
| OTIO import (and AAF, through OTIO) | untrusted | `path_util::safe_resolve` |
| FCPXML import | untrusted | `file://` URL decoded, then `path_util::normalize_path` (the format's paths are absolute) |
| FCP7 XML (xmeml) import | untrusted | `file://` URL percent-decoded to an absolute path |
| EDL import | untrusted | no paths: events are matched to media already in the pool |
| Media consolidation | untrusted outputs | symlink rejection on every copy source |

Absolute paths are allowed everywhere, because media legitimately lives
outside the project folder; a path that resolves to nothing becomes
offline media, never an ffmpeg input.

Every importer reads its file through `read_to_string_capped` /
`read_capped` (64 MiB cap), and every XML format through
`parse_xml_safe` (roxmltree with `allow_dtd: false`, 1M-node limit).
All atomic writes go through
`path_util::atomic_write` with a collision-proof
`.<file>.tmp.<pid>.<nanos>` scratch name.

### Media pool

`klipp-media::MediaPool` is the flat owner of every imported item plus
a bin tree for folder-like grouping. Items reference their underlying
source file by absolute path (converted to project-relative on save,
back to absolute on load). Offline media is represented by
`MediaItem::offline = true`, never by removal — the UI and engine both
check the flag before opening the file.

Smart bins (rules over logged fields), relink and camera-card import
live beside it. Three on-disk caches sit alongside the pool:

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
  transcript (Whisper, with word times), framing (RF-DETR), music beats
  and structure, a screen recording's beat sheet and on-screen text
  (OCR), what the picture shows (CLIP embeddings, for search), what a
  file's sound is (speech, music, effects, ambience) and subject
  mattes. `audio_sync` cross-correlates two clips' sound for
  dual-system sync.

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

The control layer runs inside the app process. It reads the shared
`Sequence`, `MediaPool` and frame buffer as the UI does, and sends every
write to the UI's command router (not the engine's channel), so an
agent's edit is applied, recorded and undone exactly as a hand edit is.

### Picture graph, templates and recipes

One picture model serves the edit, the Composite page, templates and
recipes.

* **The clip's picture graph** (`klipp-nodes`) is what every picture
  clip renders through: source, corrections, effects and keys as
  nodes, evaluated on the GPU stack with the CPU as reference. The
  **Composite page** (#2758, a workspace in `klipp-ui`) is a wide
  view of that same graph; it adds no second model.

* **Precompositions** are sequences placed as one clip. The renderer
  composes the inner sequence on the GPU stack and seeds the clip's
  layer from it (`precomp_scenes` in `klipp-compose/src/render.rs`),
  with no CPU fallback.

* **Composition templates** (`klipp-timeline/src/composition_template.rs`)
  are a precomposition plus what the user may change: parameters,
  slots (media to fill) and bindings from each to a property path
  (`object:<i>.<prop>`, `clip:<n>.media`). A placed template is a
  `TemplatePrecomposition` clip carrying a `TemplateInstance`;
  `flatten::apply_template_instance` writes its values into a copy of
  the sequence when the timeline is flattened, so the renderer only
  ever sees ordinary clips. `flatten::binding_parts` is the one parser
  of property paths. **Macro controls** are bindings with a link: a
  range, a curve and, on the placed instance, an offset and a linked
  flag, so a hand edit is kept as an offset (`TemplateLink` in
  `klipp-core/src/template.rs`).

* **Recipes** are the same file format with `kind: recipe`: instead of
  a sequence they carry `graph.ops`, the graph ops `klipp_graph` takes,
  addressed from named anchors (`source`, `grade`, `display`, `end`).
  The `recipe` op builds them into a clip's own graph as one undo step
  (`klipp-control/src/recipes.rs`), its controls bound to node
  parameters (`node:<op>.<param>`). Built-in recipes are files in
  `assets/recipes/`.

**The rule: compound effects are data.** A look, a key-and-glow, a
picture-in-picture or a motion card is a recipe or a template file,
never a new op or a per-effect code path. Code is written only for a
building block the graph lacks, a node, added once, on the GPU first
(with its CPU reference and parity test), and then usable by every
recipe. So: a new *look or arrangement* is a new file in
`assets/recipes/` (or a template); a new *kind of pixel operation* is a
new node in `klipp-nodes` / `klipp-effects`. The recipe harness
(`klipp-control/tests/recipes_render.rs`) places every built-in recipe
on a test shot through the `recipe` op's route, draws it on the GPU
stack with no fallback, compares it with the CPU and runs the checks
the file itself declares (`graph.checks`): a new recipe needs no new
test code.

**Planned, not done.** The same rule is to reach what is still code:
compound one-click features move to recipe files (#2886); named presets
and the built-in delivery specs move to files under
`assets/presets/<kind>/*.toml`, with a user folder beside them (#2887);
effects, transitions and nodes are declared once, with menus and MCP
lists generated from that (#2888); shaders move to `.wgsl` files and
`gpu.rs` is split by area (#2890, done: one module per area under
`crates/klipp-effects/src/gpu/`). Each conversion names what it fixes
or enables; none is done for its own sake.

---

## Build / test / run

```bash
cargo rr                 # release build + run
cargo c                  # workspace clippy (aliases in .cargo/config.toml)
cargo test -p klipp-engine          # one crate, the usual way to test
CARGO_BUILD_JOBS=4 cargo test -p klipp-compose   # fewer parallel links
```

The big crates compile their integration tests into one `tests/all`
binary each, to keep link time down. GPU tests skip quietly without a
usable GPU; `KLIPP_TEST_REQUIRE_GPU=1` turns a skip into a failure.

The workspace uses `lld` as the linker (`.cargo/config.toml`). `mold`
is not currently wired in.

Logs land in `klipp-debug.log` in the working directory the app was
started from (the previous run's is kept as `klipp-debug.log.1`) — tail
it while running to watch engine events.

---

## Conventions

* **Error handling is per crate.** There is no workspace-wide error
  type. Library crates use `thiserror` enums (`klipp-render::RenderError`,
  `klipp-project::MltImportError`, etc.) and propagate with `?` or an
  explicit `map_err`. The agent-facing layers (`klipp-control`,
  `klipp-document`) answer with messages written for the reader, a
  `Refusal` naming the step, the reason and the fix, rather than error
  enums.
* **Every `.lock()` / `.read()` / `.write()` on shared state** uses
  `.unwrap_or_else(|e| e.into_inner())` to recover from poisoning. A
  worker-thread panic should surface as a UI toast, not a freeze.
* **Unsafe** is kept to where it is needed: the OFX and frei0r hosts,
  ffmpeg and hardware-decode FFI, a few libc calls (socket peer
  credentials, process groups), environment writes that Rust 2024 marks
  unsafe, and a few buffer hand-offs. Nine crates forbid it outright,
  and `klipp-plugin` holds most of it. Review asks every touched unsafe
  block for a `// SAFETY:` comment explaining its preconditions.
* **Commits** are single-purpose, signed off (DCO; see
  `CONTRIBUTING.md`), and the message focuses on the *why*. No
  `Co-Authored-By` trailers.

---

## Where things tend to break

A small honest-map of the landmines we've hit:

* **Audio on multi-clip timelines** — the preroll-hit path in
  `advance_to_next_clip` must reopen audio decoders when the next clip
  uses different media, not just seek. If only clip 1's audio plays
  through the timeline, `seek_all_audio_to_timecode`'s media-mismatch
  check regressed.
* **`==` on times at different rates** — structural, so `ZERO` is not
  `==` a zero at another rate; use `cmp` or a rate-matched zero (see
  Timing).
* **Single-variant enums after deletion** — when removing a variant,
  consider flattening the enum if only one remains (`FrameData::Nv12`
  was deleted; `FrameData` became `Vec<u8>` directly).
* **Lock-poison cascade** — any new `.lock().unwrap()` on shared state
  re-opens the whole-app-freeze-on-worker-panic class of bugs.
* **WGSL uniform arrays** — `array<u32, N>` fails naga validation
  silently. Always use individual scalar `_padN: u32` fields on the
  WGSL side, regardless of what the Rust `#[repr(C)]` struct looks
  like.
