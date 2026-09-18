# Klipp

**Klipp** is a free, open-source, GPU-accelerated non-linear video editor for Linux, with a linear-light colour pipeline and an on-device AI assistant an agent can drive.

> *Klipp* is Swedish for "cut" — the fundamental act of film editing.

---

## What it is

Klipp is a native Linux NLE built around three ideas:

- **A real editor.** A magnetic timeline with video, audio, title and subtitle tracks; real-time playback that stays on the GPU from decode to display; import of OTIO, FCPXML and MLT projects; and a `.klipp` project format of its own.
- **A colourist's pipeline.** Grading happens in scene-linear light with an ACEScg working space and camera-log decoders (Sony, Canon, Panasonic, ARRI, Apple, RED). Node-graph grading covers primaries, HSL secondaries, curves, colour wheels and a colour warper; there are ~300 bundled 3D LUTs, GPU scopes, and real HDR (PQ / HLG) grading and export.
- **An AI-first surface.** A control socket and a Model Context Protocol server let an agent drive a running Klipp: analyse the footage, assemble selects on clean cut points, grade to the scopes, match shots for continuity, clean and sync the sound, caption, quality-check and export — all as undoable operations, with no cloud calls.

## What it can do today

- **Editing** — magnetic multi-track timeline; insert and ripple edits, splits and ripple deletes; J and L cuts; markers; per-clip retime and reframe.
- **Colour** — scene-linear node-graph grading (primary + HSL secondary qualifiers, curves, wheels, warper), 3D-LUT looks, shot-to-shot colour match, BT.1886 and HDR grade encodings, GPU waveform / parade / vectorscope / histogram, and per-frame scope readings.
- **Audio** — multi-track mixer with one pan law for playback and export; per-clip DSP (noise reduction, mains-hum removal, de-esser, gate, compressor, EQ, limiter); EBU R128 loudness metering and normalisation; dual-system sound sync by cross-correlation.
- **Titles, transitions, effects** — a text/title engine with styled runs; over 60 transitions; built-in GPU effects (vignette, film grain, glow, blur, sharpen, stabilisation, transform).
- **Media management** — bins and rule-based smart bins, markers, shot logging, metadata (EXIF stills, Sony/Canon camera XML sidecars), and CSV/ALE sheets.
- **On-device AI** — subject masking and segmentation (BiRefNet, MobileSAM, RF-DETR-Seg) and speech transcription (Whisper Large v3 Turbo on the GPU via Vulkan), all local.
- **Plugins** — OFX 1.5 and frei0r hosts.
- **Delivery** — export with CPU/GPU render parity and real HDR output; burnt-in or sidecar captions.

## Status

🚧 **Pre-release, single-developer, moving fast.**

Large parts of the editor are built and usable, but Klipp makes no stability commitment: there is no release schedule, no pre-built binaries, and no support. Building from source, you should still expect rough edges, breaking changes, and areas that are further along than others (reliability hardening, soak-testing, and broad AMD/Intel GPU validation are ongoing). If you need a Linux NLE to rely on today, use Kdenlive, DaVinci Resolve, or Olive.

Klipp is developed and tested day-to-day on [Omarchy](https://omarchy.org/) 4.0.4 (DHH's Arch- and Hyprland-based Linux). Any recent Arch or Arch-derived system should behave the same; other distributions are untested.

Progress is fast right now, most of all on the AI framework: the control socket, the MCP server, the footage analysers, and the professional-finishing operations an agent uses are landing quickly, so the assistant surface changes week to week.

### Try it on Omarchy / Arch

There's a `makepkg` recipe under [`packaging/`](packaging/) that builds Klipp from source and installs it as a normal pacman package, with a launcher entry that shows up in the Omarchy app menu:

```bash
git clone https://github.com/davidkodar/klipp
cd klipp/packaging
makepkg -si          # builds from source, then installs klipp-git via pacman
```

`makepkg` pulls the build tools it needs, runs the release build (this takes a while the first time), and installs the `klipp` and `klipp-mcp` commands plus the desktop entry and icons. Everything built-in works out of the box — GPU playback, the bundled effects, LUTs and fonts, and the MCP tools; the AI models (speech and vision) download themselves on first use, verified against the bundled manifest. Remove it later with `pacman -R klipp-git`. The [Building](#building) section below covers the plain `cargo` workflow for development.

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

For NVIDIA GPU acceleration of the ONNX / Candle AI models (recommended, not required — Klipp falls back to CPU):

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

AI features ship enabled by default and run on CPU out of the box. For NVIDIA GPU acceleration of the ONNX / Candle models:

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

Klipp exposes a running instance to an agent so an edit can be built through tools alone. Two crates carry it:

- **`klipp-control`** opens a Unix-socket JSON protocol on a running app: read the state, run analysers, apply undoable edit operations, grab frames and scopes, export.
- **`klipp-mcp`** wraps that socket as a [Model Context Protocol](https://modelcontextprotocol.io) server, so any MCP client can call the tools. The package installs it as the `klipp-mcp` command; point your MCP client at it while Klipp is running, and the tools appear.

Between them an agent can orient itself, import and analyse footage (shot boundaries, silence and loudness, motion, transcript, framing), find clean cut points and cut a long take down to its selects, place clips, grade to the scope numbers and match shots for continuity, pull HSL secondaries, apply LUT looks, clean wind / hum / hiss and sync dual-system sound, caption from the transcript, run a delivery quality-check (picture **and** sound), and export — each step one undo. The craft guidance the assistant follows lives in [`assets/craft/`](assets/craft/).

None of it needs a particular GPU vendor: the AI runs on CPU (the ONNX and Candle vision models) and Vulkan (Whisper speech, and all playback, effects and scopes), and video decode / encode fall back to software. CUDA is an optional accelerator only — nothing requires it.

All of the on-device AI is **opt-in-by-default and easy to turn off**: **Preferences → AI** has the master switch (off means transcription and subject detection never run), the "AI ready / MCP ready" status, a CPU-vs-GPU choice for transcription, and the speech (Whisper) models with their sizes, with a link to the full model manager for downloading and switching every model.

## Architecture

Klipp is a 32-crate Cargo workspace organised into layers, with dependencies flowing one way and no cycles. Every crate carries a `//!` overview; `ARCHITECTURE.md` is the system map.

| Crate | Responsibility |
|---|---|
| `klipp-core` | Foundation types: IDs, rational time, events, config, errors |
| `klipp-project` | `.klipp` project format, serialisation, OTIO / FCPXML / MLT import |
| `klipp-timeline` | Sequence / track / clip model and the magnetic-timeline editor |
| `klipp-media` | Media pool, bins, thumbnails, waveforms, on-disk caches |
| `klipp-audio` | DSP effects, LUFS metering, multi-track mixer |
| `klipp-color` | Colour grading, LUTs, scopes, camera-log decoders |
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
| `klipp-analysis` | Footage analysers: shots, audio, motion, transcript, framing |
| `klipp-document` | The edit history and the application of its commands |
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
- **AI inference** — [ort](https://crates.io/crates/ort) and [candle](https://crates.io/crates/candle-core) for vision models; [whisper-rs](https://crates.io/crates/whisper-rs) (Vulkan) for speech
- **Colour** — an in-house scene-linear ACEScg pipeline with camera-log auto-detection; the ACES output transforms are checked against OCIO

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
| BiRefNet Lite | Subject masking | 224 MB | MIT |
| MobileSAM | Promptable segmentation | 40 MB | Apache-2.0 |
| RF-DETR-Seg (Preview / Medium) | Detection + segmentation | 134 / 139 MB | Apache-2.0 |

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

### Models

See the AI Models table above; each model retains its upstream attribution and licence.

## Acknowledgements

Klipp stands on a lot of open-source work. Beyond the crates in the tech stack, it links or builds against, and is grateful to, [FFmpeg](https://ffmpeg.org/) (decode / encode / analysis), the [Vulkan](https://www.vulkan.org/) loader and [wgpu](https://wgpu.rs/) (all GPU work), [whisper.cpp / ggml](https://github.com/ggerganov/whisper.cpp) (speech, via [whisper-rs](https://crates.io/crates/whisper-rs)), [egui](https://github.com/emilk/egui) (UI), [cpal](https://crates.io/crates/cpal) and the [LV2](https://lv2plug.in/) ecosystem via [lilv](https://gitlab.com/lv2/lilv) (audio), and [frei0r](https://frei0r.dyne.org/) (video plugins). Each is used under its own licence. Contributions of missing or incorrect attribution are welcome — open an issue.

## License

GPL-3.0.
