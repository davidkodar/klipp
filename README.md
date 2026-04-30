# Klipp

**Klipp** is a free, open-source non-linear video editor for Linux.

> *Klipp* is Swedish for "cut" — the fundamental act of film editing.

---

## Goals

- A capable, native NLE for Linux — magnetic timeline, professional colour pipeline, on-device AI assist
- GPU-accelerated playback (NVDEC), effects (Vulkan compute), and ML inference (CUDA EP)
- Linear-light colour pipeline with ACEScg working space and per-clip source colourspace detection
- On-device AI tools — masking, isolation, instance segmentation — no cloud calls
- Direct codec support via FFmpeg
- Permissive bundled assets so the distribution stays redistributable

## Status

🚧 **Early development.** Core editing, colour grading, GPU effects, and AI tools are functional, but expect rough edges, missing polish, and breaking changes. Pre-built binaries are not yet released — Klipp is currently built from source.

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

- **UI** — [egui](https://github.com/emilk/egui) — immediate mode, custom timeline + scope widgets
- **GPU** — [wgpu](https://wgpu.rs/) — Vulkan backend, custom WGSL compute shaders
- **Decode + Encode** — [ffmpeg-next](https://crates.io/crates/ffmpeg-next) — FFmpeg bindings, CUDA NVDEC + NVENC
- **Audio** — [cpal](https://crates.io/crates/cpal) with PipeWire / ALSA / JACK
- **AI inference** — [ort](https://crates.io/crates/ort) (ONNX Runtime, load-dynamic) and [candle](https://crates.io/crates/candle-core) (pure-Rust ML)
- **Colour** — Bespoke ACEScg pipeline with auto-detected source colourspaces (Sony / Canon / Panasonic / ARRI / Apple / RED)

## Documentation

The full documentation is an mdbook under [`docs/`](docs/). Build and view locally:

```bash
mdbook serve docs --open
```

Highlights:

- [Getting Started → Installation](docs/src/getting-started/installation.md)
- [Quick Start](docs/src/getting-started/quick-start.md)
- [AI Tools Overview](docs/src/ai-tools/overview.md)
- [Working Colourspaces (ACEScg)](docs/src/color/working-colorspaces.md)
- [Magnetic Timeline](docs/src/editing/magnetic-timeline.md)
- [Architecture Reference](docs/src/reference/architecture.md)

## AI Models

Klipp ships an AI model catalogue with on-device inference. Models are downloaded on demand, verified by SHA-256, and stored in `~/.cache/klipp/models/`.

| Model | Size | Licence | Purpose |
|---|---|---|---|
| BiRefNet Lite | 224 MB | MIT | Remove background |
| MobileSAM | 40 MB | Apache-2.0 | Click-to-mask isolation |
| RF-DETR-Seg (Preview) | 134 MB | Apache-2.0 | Multi-class instance segmentation, fast tier |
| RF-DETR-Seg (Medium) | 139 MB | Apache-2.0 | Multi-class instance segmentation, recommended |

Klipp does not redistribute weights — the bundled manifest points at upstream HuggingFace mirrors and verifies bytes via SHA-256. See [docs/src/ai-tools/models.md](docs/src/ai-tools/models.md).

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

GPL-3.0. Bundled assets retain their original licences (MIT, CC0, Apache-2.0) and remain redistributable under their respective terms; GPL-3.0 is compatible with all of them as a downstream licence.

## Author

David Bolin ([@davidkodar](https://github.com/davidkodar) on GitHub).
