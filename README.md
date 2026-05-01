# Klipp

**Klipp** is a free, open-source non-linear video editor for Linux.

> *Klipp* is Swedish for "cut" — the fundamental act of film editing.

---

## Direction

Things being explored — none finished:

- A native NLE for Linux with a magnetic timeline
- A linear-light colour pipeline with ACEScg working space and per-clip source-colourspace detection
- GPU-accelerated playback, effects, and ML inference
- On-device AI assist (masking, isolation, segmentation) — no cloud calls
- Permissive bundled assets so the source stays redistributable

## Status

🚧 **Early development.**

Many subsystems are stubbed, half-built, or actively changing. There is no stability commitment, no release schedule, no pre-built binaries, and no support — if you build it from source, expect to hit rough edges, breakage, and incomplete features. A long list of fundamentals (reliability, soak-testing, AMD/Intel GPU support, large parts of the audio and effects surface) is still ahead of any "usable for real work" milestone.

If you're looking for a Linux NLE you can rely on today, use Kdenlive, DaVinci Resolve, or Olive. Klipp is not a replacement for any of them yet.

## Building

### Prerequisites

```bash
# CachyOS / Arch Linux
sudo pacman -S rust ffmpeg clang lld pkg-config vulkan-icd-loader vulkan-headers

# Ubuntu / Debian
sudo apt install rustup ffmpeg libavcodec-dev libavformat-dev libavutil-dev \
    libavfilter-dev libswscale-dev libswresample-dev libavdevice-dev \
    libclang-dev lld pkg-config libvulkan-dev libasound2-dev \
    libpipewire-0.3-dev
```

For NVIDIA GPU acceleration of AI inference (recommended; not strictly required — Klipp falls back to CPU):

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
cargo r          # debug build
cargo rr         # release build
```

AI features ship enabled by default and run on CPU out of the box. For NVIDIA GPU acceleration:

```bash
cargo r-ort-cuda    # GPU AI for BiRefNet + RF-DETR-Seg (no nvcc needed)
cargo r-cuda        # full GPU AI including MobileSAM (requires CUDA Toolkit)
```

For minimal builds without AI dependencies:

```bash
cargo run -p klipp-app --no-default-features
```

See [docs/src/getting-started/installation.md](docs/src/getting-started/installation.md) for the build-time vs runtime CUDA distinction and full instructions.

## Architecture

Klipp is a Cargo workspace with focused crates organised into layers. Dependencies flow in one direction with no cycles.

| Crate | Responsibility |
|---|---|
| `klipp-core` | Foundation types: IDs, RationalTime, events, errors |
| `klipp-project` | `.klipp` directory format, serialisation, versioning |
| `klipp-timeline` | Sequence/Track/Clip/Transition data model, magnetic timeline |
| `klipp-media` | MediaPool, thumbnails, filmstrip generation |
| `klipp-nodes` | Per-clip node graph (mattes, correctors, mask combine) |
| `klipp-engine` | FFmpeg decode (CUDA NVDEC + software), cpal audio, MasterClock |
| `klipp-effects` | GPU compute-shader effects via wgpu (Vulkan) |
| `klipp-stab` | Motion stabilisation (sequential frame decoder) |
| `klipp-scenedetect` | Scene-change detection |
| `klipp-title` | Title generator (cosmic-text + GPU rasterisation) |
| `klipp-ml` | Model manifest, registry, downloader, telemetry |
| `klipp-ml-ort` | ONNX inference via `ort` (BiRefNet, RF-DETR-Seg) |
| `klipp-ml-candle` | Pure-Rust inference via `candle` (MobileSAM) |
| `klipp-jobs` | Background job queue (export, stabilise, scene detect, AI) |
| `klipp-ui` | egui/eframe panels, command palette, theme registry |
| `klipp-app` | Binary entry point |
| `klipp-smoke` | End-to-end smoke tests |
| `klipp-testing` | Snapshot/parity test scaffolding |

## Tech Stack

The libraries doing most of the heavy lifting:

- **UI** — [egui](https://github.com/emilk/egui)
- **GPU** — [wgpu](https://wgpu.rs/) (Vulkan backend)
- **Decode / Encode** — [ffmpeg-next](https://crates.io/crates/ffmpeg-next)
- **Audio** — [cpal](https://crates.io/crates/cpal) with PipeWire / ALSA / JACK
- **AI inference** — [ort](https://crates.io/crates/ort) and [candle](https://crates.io/crates/candle-core)
- **Colour** — in-house ACEScg pipeline with source-colourspace auto-detection for common camera log formats (Sony / Canon / Panasonic / ARRI / Apple / RED). Not yet validated against reference workflows.

## Documentation

There's an mdbook under [`docs/`](docs/) that documents what's currently built:

```bash
mdbook serve docs --open
```

Useful entry points: [Installation](docs/src/getting-started/installation.md), [Quick Start](docs/src/getting-started/quick-start.md), [Architecture Reference](docs/src/reference/architecture.md).

## AI Models

Models are downloaded on demand, verified by SHA-256 against the bundled manifest, and stored in `~/.cache/klipp/models/`. Klipp does not redistribute weights — the manifest points at upstream HuggingFace mirrors.

Currently wired up:

| Model | Size | Licence |
|---|---|---|
| BiRefNet Lite | 224 MB | MIT |
| MobileSAM | 40 MB | Apache-2.0 |
| RF-DETR-Seg (Preview / Medium) | 134 / 139 MB | Apache-2.0 |

See [docs/src/ai-tools/models.md](docs/src/ai-tools/models.md) for the full picture, including licence audit notes.

## Bundled Assets & Third-Party Credits

### LUTs (Colour Look-Up Tables)

Klipp ships ~300 film emulation and cinematic LUTs for colour grading:

- **Film Emulation LUTs** — derived from [G'MIC](https://gmic.eu/) film simulation presets, packaged by [YahiaAngelo/Film-Luts](https://github.com/YahiaAngelo/Film-Luts). Includes Kodak Portra, Tri-X, Fuji Velvia, Superia, Provia, Agfa, Polaroid, and many more. Licensed under **MIT**.
- **Cinematic Blockbuster LUTs** — by [IRCGraphic](https://github.com/IRCGraphic/D-Cinelike-and-Normal-Blockbuster-LUTs). Licensed under **CC0 1.0** (public domain).

### Icons

- **Phosphor Icons** — [phosphoricons.com](https://phosphoricons.com/). Licensed under **MIT**.

### Models

See the AI Models table above; each model retains its upstream attribution and licence.

## License

GPL-3.0.
