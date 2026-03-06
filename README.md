# Klipp

**Klipp** is a free, open-source professional non-linear video editor for Linux.

> *Klipp* is Swedish for "cut" — the fundamental act of film editing.

---

## Goals

- Best free NLE on Linux — targeting ~85% of Final Cut Pro / DaVinci Resolve capability
- GPU-accelerated compositing via `wgpu` (no more CPU-only editing)
- Full codec support via FFmpeg and libmpv
- OpenFX plugin support (Boris FX, Sapphire, Red Giant, Neat Video, etc.)
- OpenColorIO color management (ACES, LUTs, wide gamut)
- On-device auto-captions via Whisper
- LV2 audio plugins (Linux-native, huge free library)

## Status

🚧 **Early development — not yet usable**

## Building

### Prerequisites

```bash
# CachyOS / Arch Linux
sudo pacman -S rust ffmpeg libmpv mold

# Ubuntu / Debian
sudo apt install rustup ffmpeg libmpv-dev
```

### Run

```bash
git clone https://github.com/davidkodar/klipp
cd klipp
cargo run-dev
```

## Architecture

Klipp is a Cargo workspace with focused crates:

| Crate | Responsibility |
|---|---|
| `klipp-core` | Shared types, IDs, errors, events |
| `klipp-project` | `.klipp` project format, serialization, versioning |
| `klipp-timeline` | Timeline data model, editing operations, undo/redo |
| `klipp-media` | Media pool, decode, proxies, thumbnails |
| `klipp-audio` | Audio mixer, monitoring, LV2 plugins, captions |
| `klipp-color` | Color grading nodes, LUTs, scopes |
| `klipp-engine` | GPU compositor (wgpu), frame scheduling |
| `klipp-plugin` | OpenFX plugin host |
| `klipp-export` | Offline render and transcode (ffmpeg) |
| `klipp-ui` | All egui panels and custom widgets |
| `klipp-app` | Binary entry point |

### Dependency graph

```
klipp-app
  └── klipp-ui
        ├── klipp-engine    ← wgpu compositor
        ├── klipp-timeline  ← editing model
        ├── klipp-media     ← decode + thumbnails
        ├── klipp-audio     ← mixer
        ├── klipp-color     ← grading
        ├── klipp-plugin    ← OpenFX host
        ├── klipp-export    ← transcode
        └── klipp-project   ← file format
              └── klipp-core  ← shared types (no klipp-* deps)
```

## Tech Stack

- **UI**: [egui](https://github.com/emilk/egui) — immediate mode, custom timeline widget
- **GPU**: [wgpu](https://wgpu.rs/) — Vulkan backend, custom WGSL shaders
- **Decode**: [libmpv2](https://crates.io/crates/libmpv2) — all codecs via FFmpeg
- **Export**: [ffmpeg-next](https://crates.io/crates/ffmpeg-next)
- **Audio**: [cpal](https://crates.io/crates/cpal) + [dasp](https://crates.io/crates/dasp) + LV2
- **Waveforms**: [symphonia](https://crates.io/crates/symphonia)
- **Color**: OpenColorIO (via FFI)
- **Plugins**: OpenFX 1.5
- **Captions**: [whisper-rs](https://crates.io/crates/whisper-rs)

## License

TBD
