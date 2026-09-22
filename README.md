# Klipp

**Klipp** is a free, open-source, GPU-accelerated non-linear video editor for Linux, with a scene-linear colour pipeline, colour judged by measurement rather than by eye, and an on-device AI assistant an agent can drive end to end.

> *Klipp* is Swedish for "cut" — the fundamental act of film editing.

---

## What it is

Klipp is a native Linux NLE built around three ideas:

- **A real editor.** A magnetic timeline with video, audio, title and subtitle tracks; real-time playback that stays on the GPU from decode to display; import of OTIO, FCPXML and MLT projects; and a `.klipp` project format of its own.
- **A colourist's pipeline, measured.** Grading happens in scene-linear light with an ACEScg working space and camera-log decoders (Sony, Canon, Panasonic, ARRI, Apple, RED). Node-graph grading covers primaries, HSL secondaries, curves, colour wheels and a colour warper; there are ~300 bundled 3D LUTs, GPU scopes, false colour, and real HDR (PQ / HLG) grading and export. On top of that, every colour judgement Klipp makes is a **number against a published target** — CIELAB readings, skin held to a band derived from photographed faces, and a synthesised ColorChecker sent through the render path to prove the pipeline itself isn't moving colour.
- **An AI-first surface.** A control socket and a Model Context Protocol server expose a running Klipp as **84 tools**, so an agent can do the whole job: read the footage, propose a paper edit a person approves, assemble it, grade it to the numbers, design the sound, caption it, quality-check it and export — every step one undo. Every model runs on the machine.

## What it can do today

### Editing

Magnetic multi-track timeline; insert and ripple edits, splits and ripple deletes; J and L cuts; markers; per-clip retime and reframe. Clips can be moved by motion rather than by hand: `zoom`, `pan`, `follow` (the frame tracks a subject), `highlight` and `ramp` are first-class operations on the timeline, not keyframes you draw.

### Colour

- **Grading** — scene-linear node-graph grading (primary + HSL secondary qualifiers, curves, wheels, warper), 3D-LUT looks, BT.1886 and HDR grade encodings.
- **Scopes** — GPU waveform, parade, vectorscope and histogram at native size, plus **false colour** whose skin band comes from the look the piece is being held to, not a fixed IRE window — so it agrees with the grading loop instead of offering a second opinion.
- **Measurement** — a **Colour Check** window reads the frame in the viewer and names the fault and the knob to turn: exposure metered off the face, casts on the neutrals, noise floor, banding, gamut, and whether the grade's nodes are in an order that can settle. A trend strip beneath it fills as you watch the piece, so drift across a sequence shows up on one axis.
- **Matching** — shots grouped into scenes by how they were lit, a hero picked per scene, the rest graded onto it, and the remaining distance reported. Matching compares the light, not the people.
- **Looks from a reference** — point at a picture and say "like this": the look's numbers are taken off a reference frame or the timeline's best-graded shot and become the target everything else is held to. The named presets are the fallback, not the normal path.
- **Faces** — a face detector, the cheek band read in CIELAB, and a skin secondary held inside a window tracked on the face, so a red jumper in shot doesn't turn with the skin.

### Audio

Multi-track mixer with one pan law for playback and export; per-clip DSP (noise reduction, mains-hum removal, de-esser, gate, compressor, EQ, limiter); EBU R128 loudness metering and normalisation; dual-system sound sync by cross-correlation. A **Sound Over Time** window shows a whole file as frequency against time — where a hum starts, when a radio mic began to hiss, which two minutes have the air conditioning in them — with the findings named.

### Titles, transitions, effects

A text/title engine with styled runs, named type styles, glow, gradients, entrances and staggered per-glyph motions composited on the GPU; 67 transitions; built-in GPU effects (vignette, film grain, glow, blur, sharpen, stabilisation, transform).

### Screen recordings and demos

Screencasts defeat camera-tuned analysis — a four-minute demo reads as one frozen shot. Klipp analyses them on their own terms: a **beat sheet** built from stillness (where the screen settles, and the reveal that is the best moment to cut to), on-screen text read by OCR, and a `redact` operation that hides a patch of picture for a stretch. `auto_redact` covers every sensitive string the OCR found — emails, keys, passwords, card numbers — wherever the timeline shows it. The timeline find bar and the media pool search read that screen text too, so you can search a recording for what was on screen.

### The paper edit

An agent proposes an `EditPlan` — segments, joins, music, titles — and the **Edit Plan window** in the app is where the person whose word it waits on reads it and hits Approve, Build or Discard. Build lowers the plan to ops and applies them as one undo step, along exactly the path the socket takes, so what an agent would build is what the button builds.

### Sound design and music

`auto_sfx` places a whoosh on every cut, a click on every reveal the beat sheet found and a riser into the last cut, as one undo step. Beat detection lets a plan's segments snap to the music's bars. Royalty-free music and sound effects can be searched and previewed from inside Klipp (Jamendo and Freesound — see [Online sound libraries](#online-sound-libraries)).

### Media management

Bins and rule-based smart bins, markers, shot logging, metadata (EXIF stills, Sony/Canon camera XML sidecars), and CSV/ALE sheets.

### On-device AI

Subject masking and segmentation (BiRefNet, MobileSAM, RF-DETR-Seg), speech transcription (Whisper Large v3 Turbo on the GPU via Vulkan), face detection (YuNet), on-screen text (PP-OCR) and a scratch narration voice (Piper) — all local, all downloaded on demand and verified against a bundled manifest.

### Delivery

Export with CPU/GPU render parity and real HDR output; burnt-in or sidecar captions; and a delivery check that covers picture **and** sound against the project's brief.

### Plugins

OFX 1.5 and frei0r hosts.

## Status

🚧 **Pre-release, single-developer, moving fast.**

Large parts of the editor are built and usable, but Klipp makes no stability commitment: there is no release schedule, no pre-built binaries, and no support. Building from source, you should still expect rough edges, breaking changes, and areas that are further along than others (reliability hardening, soak-testing, and broad AMD/Intel GPU validation are ongoing).

Klipp is developed and tested day-to-day on [Omarchy](https://omarchy.org/) 4.0.4 (DHH's Arch- and Hyprland-based Linux). Any recent Arch or Arch-derived system should behave the same; other distributions are untested. Klipp follows the Omarchy theme if you ask it to, in Preferences → Appearance.

A GPU is a hard requirement — any GPU with Vulkan 1.1, an integrated one included. Playback, the built-in effects, the scopes and the speech model all run through Vulkan, and the scopes deliberately have no CPU fallback. (The offline render path does have one, held against the GPU path frame for frame, so an export is the same picture either way.)

Progress is fast right now, most of all on the assistant: the control socket, the MCP server, the footage analysers, and the professional-finishing operations an agent uses are landing weekly, so that surface changes faster than the rest.

### Try it on Omarchy / Arch

There's a `makepkg` recipe under [`packaging/`](packaging/) that builds Klipp from source and installs it as a normal pacman package, with a launcher entry that shows up in the Omarchy app menu:

```bash
git clone https://github.com/davidkodar/klipp
cd klipp/packaging
makepkg -si          # builds from source, then installs klipp-git via pacman
```

`makepkg` pulls the build tools it needs, runs the release build (this takes a while the first time), and installs the `klipp` and `klipp-mcp` commands plus the desktop entry and icons. Everything built-in works out of the box — GPU playback, the bundled effects, LUTs and fonts, and the MCP tools; the AI models (speech, vision, OCR and voice) download themselves on first use, verified against the bundled manifest. Remove it later with `pacman -R klipp-git`. The [Building](#building) section below covers the plain `cargo` workflow for development.

## Building

### Prerequisites

```bash
# Omarchy / Arch / CachyOS (the primary target)
sudo pacman -S rust ffmpeg clang lld cmake pkg-config vulkan-icd-loader vulkan-headers shaderc lilv

# Ubuntu / Debian
sudo apt install rustup ffmpeg libavcodec-dev libavformat-dev libavutil-dev \
    libavfilter-dev libswscale-dev libswresample-dev libavdevice-dev \
    libclang-dev lld cmake pkg-config libvulkan-dev glslc libasound2-dev \
    libpipewire-0.3-dev liblilv-dev
```

Omarchy is Arch-based, so the `pacman` line covers it directly. On Omarchy or Arch you can skip the manual step entirely and use the [`packaging/`](packaging/) recipe above, which pulls these itself.

The Vulkan headers and `glslc` are build-time only: the speech model runs on the GPU through Vulkan (NVIDIA, AMD or Intel) and its shaders are compiled into the binary. A built Klipp needs nothing beyond the GPU driver it already requires.

Two assistant tools shell out to a program if you want them: `narrate` needs `piper-tts`, and `record` needs `gpu-screen-recorder` or `wf-recorder`. Both are optional and everything else works without them.

For NVIDIA GPU acceleration of the ONNX / Candle AI models (recommended, not required — these fall back to CPU):

```bash
# Arch / CachyOS
sudo pacman -S cudnn

# Ubuntu / Debian
sudo apt install libcudnn9-cuda-12
```

### Run

```bash
git clone https://github.com/davidkodar/klipp
cd klipp
./scripts/vulkan-sdk.sh   # once: lays out the Vulkan SDK directory the speech model's build expects
cargo r                   # debug build (aliased: run -p klipp-app)
cargo rr                  # release build
```

AI features ship enabled by default. The vision models run on CPU out of the box; for NVIDIA GPU acceleration of the ONNX / Candle ones:

```bash
cargo r-ort-cuda    # GPU AI for BiRefNet + RF-DETR-Seg (no nvcc needed)
cargo r-cuda        # full GPU AI including MobileSAM (requires CUDA Toolkit)
```

For minimal builds without AI dependencies:

```bash
cargo run -p klipp-app --no-default-features
```

See [docs/src/getting-started/installation.md](docs/src/getting-started/installation.md) for the build-time vs runtime CUDA distinction and full instructions.

## The AI assistant

Klipp exposes a running instance to an agent so an entire edit can be built through tools alone. Two crates carry it:

- **`klipp-control`** opens a Unix-socket JSON protocol on a running app: read the state, run analysers, apply undoable edit operations, grab frames and scopes, export.
- **`klipp-mcp`** wraps that socket as a [Model Context Protocol](https://modelcontextprotocol.io) server, so any MCP client can call the tools. The package installs it as the `klipp-mcp` command; point your MCP client at it while Klipp is running, and the tools appear.

An agent never clicks and never does frame arithmetic: it names clips and media by id and places by anchor, and Klipp resolves the frames. The loop it works in:

1. **Orient** — where it is in one call: the build, the project, the brief, the footage, the sequence, and what to do next.
2. **The brief** — what the piece is for: its length, platform, loudness, whether it is captioned, the cast and their names, and the named type styles its titles use. The check holds the piece to it and the export takes its size and loudness from it.
3. **Read the footage** — shot boundaries, loudness with the spectral bands that reveal wind, rumble, hum and hiss, silences, motion, transcript, who is in shot and how large, on-screen text, tempo and beat grid, and a beat sheet for screen recordings. `style_profile` reads a reference video's whole editing style — its cut cadence, whether it cuts on the beat, its look, its motion — so you can build in that feel from your own footage and your own music, never the reference's track.
4. **Find the cut** — clean cut points ranked (pauses first, then sentence ends and shot changes, then phrase breaks), a long take split into its keeps and drops, shots found by framing, people, objects, motion, speech or length.
5. **Plan** — a paper edit a person approves in the Edit Plan window before anything is built.
6. **Build** — place clips, joins, titles, music; motion; sound design; captions from the transcript. Each step is one undo.
7. **Finish** — check the render path against a chart first, adopt a look off a reference, match scenes to their hero, pull skin onto the band, clean wind / hum / hiss, sync dual-system sound.
8. **Look at it** — `review` renders a draft of the whole timeline, tiles it into a contact sheet, and runs the check: the agent sees the rhythm, the titles and the cards, rather than assuming.
9. **Deliver** — a quality check over picture and sound, then export.

The craft guidance the assistant follows lives in [`assets/craft/`](assets/craft/) — the guide, the recipes and the reference are read by the model, not by the code, so they can be argued with and improved without a rebuild.

It can also make its own footage: `record` captures the screen (the focused window, a monitor or a region) and imports it, and `narrate` speaks a scratch voice track to cut to and replace later.

### Turning it off

All of the on-device AI is **easy to turn off, one piece at a time**: **Preferences → AI** has a separate switch for speech transcription, for subject detection, and for the AI assistant (the MCP server), so you can, say, keep transcription and turn the assistant off entirely. It also shows the "ready / off" status of each, a CPU-vs-GPU choice for transcription, and the models with their sizes and a link to the full model manager. A clip's right-click menu has **Transcribe Clip** to run speech-to-text on one clip on demand, even with automatic transcription off.

### Online sound libraries

Two things in Klipp reach the network, and only when you ask them to: the music and sound-effect search, which queries [Jamendo](https://www.jamendo.com/) and [Freesound](https://freesound.org/) for Creative-Commons audio. They need your own API keys, which live in `~/.config/klipp/keys.toml` (or the `KLIPP_JAMENDO_CLIENT_ID` / `KLIPP_FREESOUND_TOKEN` environment variables) and never in the source tree. Without keys the feature is simply absent; nothing else in Klipp calls out, and no AI model does.

## Architecture

Klipp is a 33-crate Cargo workspace organised into layers, with dependencies flowing one way and no cycles. Every crate carries a `//!` overview; `ARCHITECTURE.md` is the system map.

| Crate | Responsibility |
|---|---|
| `klipp-core` | Foundation types: IDs, rational time, events, config, errors |
| `klipp-project` | `.klipp` project format, serialisation, OTIO / FCPXML / MLT import |
| `klipp-timeline` | Sequence / track / clip model, the magnetic-timeline editor, the brief and the edit plan |
| `klipp-media` | Media pool, bins, thumbnails, waveforms, on-disk caches |
| `klipp-audio` | DSP effects, LUFS metering, multi-track mixer |
| `klipp-audio-search` | Royalty-free music and SFX search (Jamendo, Freesound) |
| `klipp-color` | Colour grading, LUTs, scopes, camera-log decoders, CIELAB measurement, false colour, the chart self-check |
| `klipp-grade-model` | The primary grade's maths (lift/gamma/gain/offset, contrast, BT.1886) |
| `klipp-nodes` | Per-clip colour node-graph model and executor |
| `klipp-engine` | Real-time playback: decode, composite, mix, output |
| `klipp-compose` | The one definition of how a timeline frame is rendered |
| `klipp-effects` | Built-in GPU effects and the shared scope pipeline (wgpu / Vulkan) |
| `klipp-transitions` | Transition pixel-math |
| `klipp-title` | Title / text rasteriser: layout, glyphs, style runs |
| `klipp-plugin` | OFX 1.5 plugin host (C ABI, dlopen) |
| `klipp-frei0r` | frei0r 1.x plugin host |
| `klipp-render` / `klipp-export` | Offline render pipeline and the export-queue model |
| `klipp-hdr-out` | HDR reference output shared by viewer and export |
| `klipp-validation` | Project validation and pre-flight checks |
| `klipp-ml` / `klipp-ml-ort` / `klipp-ml-candle` | Model manifest + ONNX (`ort`) and Candle inference backends |
| `klipp-analysis` | Footage analysers: shots, audio, motion, transcript, framing, beats, beat sheet, spectrogram, screen text |
| `klipp-document` | The edit history, the ops, and the lowering of an edit plan into them |
| `klipp-control` | Control surface for a running instance (socket protocol) |
| `klipp-mcp` | Model Context Protocol server |
| `klipp-jobs` | Background job scheduling primitives |
| `klipp-ui` | The egui / eframe desktop editor surface |
| `klipp-app` | Binary entry point |
| `klipp-perf` / `klipp-smoke` / `klipp-testing` | Perf harness, headless engine harness, test scaffolding |

## Tech Stack

- **UI** — [egui](https://github.com/emilk/egui)
- **GPU** — [wgpu](https://wgpu.rs/) (Vulkan backend)
- **Decode / Encode** — [ffmpeg-next](https://crates.io/crates/ffmpeg-next)
- **Audio** — [cpal](https://crates.io/crates/cpal) with PipeWire / ALSA / JACK
- **AI inference** — [ort](https://crates.io/crates/ort) and [candle](https://crates.io/crates/candle-core) for vision, OCR and face models; [whisper-rs](https://crates.io/crates/whisper-rs) (Vulkan) for speech; [Piper](https://github.com/rhasspy/piper) for the scratch voice
- **Colour** — an in-house scene-linear ACEScg pipeline with camera-log auto-detection; the ACES output transforms are checked against OCIO, and the render path against a published ColorChecker

## Documentation

There's an mdbook under [`docs/`](docs/):

```bash
mdbook serve docs --open
```

Useful entry points: [Installation](docs/src/getting-started/installation.md), [Quick Start](docs/src/getting-started/quick-start.md), [Architecture Reference](docs/src/reference/architecture.md).

## AI Models

Models are downloaded on demand, verified by SHA-256 against the bundled manifest, and stored in `~/.cache/klipp/models/`. Klipp does not redistribute weights — the manifest points at upstream HuggingFace mirrors.

| Model | Role | Size | Licence |
|---|---|---|---|
| Whisper Large v3 Turbo (q5) | Speech transcription | ~574 MB | MIT (whisper.cpp / ggml) |
| Whisper Base | Small and quick, less accurate — the fallback on a slow machine | ~148 MB | MIT (whisper.cpp / ggml) |
| BiRefNet Lite | Subject masking | 224 MB | MIT |
| MobileSAM | Promptable segmentation | 40 MB | Apache-2.0 |
| RF-DETR-Seg (Preview / Medium) | Detection + segmentation, framing analysis | 134 / 139 MB | Apache-2.0 |
| YuNet | Face detection for the colour measurements | 233 KB | MIT |
| PP-OCRv3 det + PP-OCRv4 rec | On-screen text in screen recordings | ~2.4 MB + ~7.7 MB | Apache-2.0 |
| Piper en_US Lessac (medium) | Scratch narration voice | 63 MB | MIT (Piper); voice public domain |

See [docs/src/ai-tools/models.md](docs/src/ai-tools/models.md) for the full picture, including licence audit notes.

## Bundled Assets & Third-Party Credits

### LUTs (Colour Look-Up Tables)

Klipp ships ~300 film-emulation and cinematic LUTs for colour grading:

- **Film Emulation LUTs** — derived from [G'MIC](https://gmic.eu/) film simulation presets, packaged by [YahiaAngelo/Film-Luts](https://github.com/YahiaAngelo/Film-Luts). Includes Kodak Portra, Tri-X, Fuji Velvia, Superia, Provia, Agfa, Polaroid, and many more. Licensed under **MIT**.
- **Cinematic Blockbuster LUTs** — by [IRCGraphic](https://github.com/IRCGraphic/D-Cinelike-and-Normal-Blockbuster-LUTs). Licensed under **CC0 1.0** (public domain).

### Fonts

Klipp bundles these typefaces for its title tool, each under the **SIL Open Font License 1.1**:

- **Inter** — [Rasmus Andersson](https://rsms.me/inter/)
- **Bebas Neue** — [Ryoichi Tsunekawa / Dharma Type](https://www.dafont.com/bebas-neue.font)
- **Libre Baskerville** — [Impallari Type](https://github.com/impallari/Libre-Baskerville)
- **JetBrains Mono** — [JetBrains](https://www.jetbrains.com/lp/mono/)

### Icons

- **Phosphor Icons** — [phosphoricons.com](https://phosphoricons.com/). Licensed under **MIT**.

### Colour reference data

The ColorChecker patch values the render-path self-check uses are the **BabelColor averages of the X-Rite ColorChecker Classic** — published figures, quoted rather than re-derived, which is the whole point of an external reference.

### Models

See the AI Models table above; each model retains its upstream attribution and licence.

## Acknowledgements

Klipp stands on a lot of open-source work. Beyond the crates in the tech stack, it links or builds against, and is grateful to, [FFmpeg](https://ffmpeg.org/) (decode / encode / analysis), the [Vulkan](https://www.vulkan.org/) loader and [wgpu](https://wgpu.rs/) (all GPU work), [whisper.cpp / ggml](https://github.com/ggerganov/whisper.cpp) (speech, via [whisper-rs](https://crates.io/crates/whisper-rs)), [PaddleOCR](https://github.com/PaddlePaddle/PaddleOCR) (on-screen text), [Piper](https://github.com/rhasspy/piper) (the scratch voice), [libfacedetection / YuNet](https://github.com/ShiqiYu/libfacedetection) (faces), [egui](https://github.com/emilk/egui) (UI), [cpal](https://crates.io/crates/cpal) and the [LV2](https://lv2plug.in/) ecosystem via [lilv](https://gitlab.com/lv2/lilv) (audio), [frei0r](https://frei0r.dyne.org/) (video plugins), and [Jamendo](https://www.jamendo.com/) and [Freesound](https://freesound.org/) (the music and sound-effect libraries). Each is used under its own licence. Contributions of missing or incorrect attribution are welcome — open an issue.

## Contributing

Issues and pull requests are welcome — read
[CONTRIBUTING.md](CONTRIBUTING.md) first, and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) alongside it. Two rules are
worth knowing before you start: open an issue before writing anything
non-trivial, and **commit under your own name** — contributions are
signed off under the [DCO](https://developercertificate.org/), and an
AI agent is never the author, committer or co-author of a commit.
Security problems go through [SECURITY.md](SECURITY.md), not the issue
tracker.

Klipp's tests run on your machine rather than in CI — `scripts/install-hooks.sh`
after cloning, and every `git push` then runs the suite. That is not a
gap: the colour pipeline's correctness argument is that its CPU and GPU
paths agree, and a cloud runner has no GPU to check it with.

## License

**GPL-3.0** — see [LICENSE](LICENSE) for the full text.

Klipp links against an FFmpeg built with `--enable-gpl
--enable-version3` (x264 and x265 are GPL) and hosts frei0r plugins,
which are GPL as well, so the editor you build from this source is a
GPL work whatever label the repository carried. The licence says what
is already true rather than choosing against the grain of the
dependencies.
