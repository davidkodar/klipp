# Introduction

Klipp is an in-progress non-linear video editor for Linux, written in Rust with an egui/eframe interface. This documentation describes what is currently wired up — not what works reliably, not what is feature-complete, and not what should be relied on for real work.

## Status

🚧 **Early development.**

Many subsystems are stubbed, half-built, or actively changing. There is no stability commitment, no release schedule, no pre-built binaries, and no support. Building from source is currently the only way to run Klipp, and you should expect to hit rough edges, breakage, and incomplete features.

If you need a Linux NLE you can rely on for actual work today, use [Kdenlive](https://kdenlive.org/), [DaVinci Resolve](https://www.blackmagicdesign.com/products/davinciresolve), or [Olive](https://www.olivevideoeditor.org/). Klipp is not a replacement for any of them.

## What's Wired Up

The list below describes which subsystems currently produce output. None of these are claims of quality, completeness, or production readiness — they're just things you can poke at if you build Klipp from source.

### Timeline editing
- Multi-track video and audio
- Magnetic-timeline behaviours (insert / overwrite, ripple, snap)
- Filmstrip thumbnails on video clips
- Undo / redo with drag coalescing

### Colour pipeline
- Lift / Gamma / Gain / Offset wheels and a curves editor
- Working colourspaces: Rec.709, Rec.2020, ACEScg
- Auto-detection for common camera log formats (Sony SLog3, Canon Log, Panasonic V-Log, Apple Log, ARRI Log-C, RED Log3G10) — not validated against reference workflows
- Output Display Transforms (sRGB, Rec.709, Rec.2020, P3-D65)
- A per-clip node graph for mattes and correctors
- Keyframable parameters with several interpolation curves

### GPU effects
- Opacity, Gaussian Blur, Sharpen, Crop, Transform, Chroma Key
- Vulkan compute shaders via wgpu, with CPU fallback

### AI tools
- Remove Background (BiRefNet, MIT)
- Isolate Object (MobileSAM, Apache-2.0)
- Detect Objects (RF-DETR-Seg, Apache-2.0)
- All inference runs on-device. NVIDIA CUDA acceleration when the runtime libraries are present; CPU fallback otherwise. AMD and Intel GPU acceleration is not implemented yet.

### Audio
- Per-clip volume, pan, EQ, fade curves
- EBU R128 loudness metering
- Keyframable parameters
- Output through cpal — PipeWire / ALSA / JACK

### Decode
- CUDA NVDEC (cuvid path) on NVIDIA hardware
- Software decode fallback
- A direct path for 10-bit camera-log YUV that bypasses libswscale's broken RGBA64LE output

### Export
- H.264 / H.265 / ProRes / DNxHD into MP4 / MKV / MOV
- FCPXML and CMX 3600 EDL interchange
- Per-clip ODT override at export time

## What's Missing

A non-exhaustive list of fundamentals that are either stubbed, unfinished, or not yet started:

- Reliability — no extended soak-testing has been done
- AMD ROCm and Intel OpenVINO for AI inference
- Pre-built binaries / installers
- Documentation accuracy under continued churn (this doc may drift)
- Plugin host (no OFX, no LV2 yet)
- Auto-captions / speech-to-text
- Many polish items across audio, effects, and the timeline

## Author and Licence

Klipp is developed by David Bolin and released under the GNU General Public Licence v3 (GPL-3.0). GPL-3.0 is compatible with code under permissive licences (MIT, Apache-2.0); bundled assets such as Phosphor icons (MIT) and the Film Emulation LUT collection (MIT) are redistributed under their original terms.
