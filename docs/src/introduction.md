# Introduction

Klipp is a non-linear video editor built for Linux. Written in Rust with an egui/eframe interface, it delivers GPU-accelerated editing, color grading, AI-assisted masking, and effects processing without leaving the Linux desktop.

## Why Klipp?

Linux has long lacked a native, fully-featured NLE that takes full advantage of modern hardware. Klipp fills that gap with a ground-up Rust implementation that provides:

- **Hardware-accelerated decode** via CUDA NVDEC with software fallback
- **GPU compute effects** through wgpu/Vulkan compute shaders
- **Linear-light colour pipeline** with ACEScg working space and per-clip source colourspace detection
- **Magnetic timeline** editing inspired by Final Cut Pro
- **AI-assisted masking** via on-device ONNX models (BiRefNet, MobileSAM, RF-DETR-Seg)
- **Low-latency audio** with cpal and PipeWire/ALSA support

## Key Features

### Timeline Editing
- Multi-track video and audio with magnetic snapping and ripple editing
- Filmstrip thumbnails with progressive refinement on zoom
- Insert/overwrite modes, range selection, blade tool
- Undo/redo with drag coalescing

### Colour Pipeline
- Lift/Gamma/Gain/Offset wheels and curves editor
- Working colourspaces: Rec.709, Rec.2020, ACEScg
- Per-clip source colourspace with auto-detection for Sony SLog3, Canon Log, Panasonic V-Log, Apple Log, ARRI Log-C, RED Log3G10
- Output Display Transforms (sRGB, Rec.709, Rec.2020, P3-D65)
- Node graph compositing for masks, mattes, and chained correctors
- Keyframeable parameters with multiple interpolation curves

### GPU Effects
- Built-in: Opacity, Gaussian Blur, Sharpen, Crop, Transform, Chroma Key
- Vulkan compute shaders via wgpu, with CPU fallback
- Frame data resident on GPU between effects (no round-trips)

### AI Tools
- **Remove Background** via BiRefNet (224 MB ONNX, MIT license)
- **Isolate Object** via MobileSAM click-to-mask (40 MB, Apache-2.0)
- **Detect Objects** via RF-DETR-Seg multi-class instance segmentation (Apache-2.0)
- All models run on-device through ort or candle; no cloud calls
- NVIDIA CUDA acceleration when available, CPU fallback otherwise

### Audio Mixing
- Per-clip volume, pan, and EQ
- Loudness metering (EBU R128)
- Keyframeable parameters and multiple fade curve types
- PipeWire and ALSA support via cpal

### Hardware Decode
- CUDA NVDEC for NVIDIA GPUs (real-time 4K via auto-downscale to 1080p)
- Software decode fallback with thread-count auto-detection
- Direct YUV420P10LE bypass for 10-bit camera-log sources where libswscale fails

### Export
- H.264, H.265, ProRes, DNxHD output in MP4/MKV/MOV containers
- FCPXML and EDL interchange
- Per-clip ODT override at export time

## Who Is Klipp For?

Klipp is designed for video editors who work on Linux and want capable editing tools without relying on compatibility layers or web-based apps. Whether you are cutting short films, producing YouTube content, or assembling corporate video, Klipp provides the tools you need with native Linux performance.

## Status

Klipp is in active development. Core editing, colour grading, GPU effects, and AI tools are functional. Pre-built binaries are not yet released — Klipp is currently built from source. See [Installation](./getting-started/installation.md) to get started.

## Author and License

Klipp is developed by David Bolin and released under the GNU General Public License v3 (GPL-3.0). GPL-3.0 is compatible with code under permissive licenses (MIT, Apache-2.0) — bundled assets such as Phosphor icons (MIT) and the Film Emulation LUT collection (MIT) are redistributed under their original terms.
