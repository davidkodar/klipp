# Introduction

Klipp is a non-linear video editor built for Linux. Written in Rust with an egui/eframe interface, it delivers GPU-accelerated editing, color grading, and effects processing without leaving the Linux desktop.

## Why Klipp?

Linux has long lacked a native, fully-featured NLE that takes full advantage of modern hardware. Klipp fills that gap with a ground-up Rust implementation that provides:

- **Hardware-accelerated decode** via CUDA NVDEC, VAAPI, or optimized software fallback
- **GPU compute effects** through wgpu/Vulkan compute shaders
- **Low-latency audio** with cpal and PipeWire/ALSA support
- **Magnetic timeline** editing inspired by Final Cut Pro
- **Color grading** with wheels, curves, scopes, and multiple color space support

## Key Features

- **Timeline Editing** -- Multi-track video and audio with magnetic snapping, ripple editing, insert/overwrite modes, and filmstrip thumbnails.
- **Color Grading** -- Lift/Gamma/Gain/Offset wheels, curves editor, hue/saturation controls, and working color spaces including Rec.709, Rec.2020, and ACEScg.
- **GPU Effects** -- Opacity, Gaussian Blur, Sharpen, Crop, Transform, Chroma Key, bloom, film grain, and more, all running on Vulkan compute shaders with CPU fallback.
- **Audio Mixing** -- Per-clip volume, pan, and EQ with loudness metering (EBU R128), keyframeable parameters, and multiple fade curve types.
- **Hardware Decode** -- CUDA NVDEC for NVIDIA GPUs provides real-time 4K playback with automatic downscaling.
- **Export** -- H.264, H.265, ProRes, DNxHD output in MP4/MKV/MOV containers, plus FCPXML and EDL interchange.

## Who Is Klipp For?

Klipp is designed for video editors who work on Linux and want capable editing tools without relying on compatibility layers or web-based apps. Whether you are cutting short films, producing YouTube content, or assembling corporate video, Klipp provides the tools you need with native Linux performance.

## Author and License

Klipp is developed by David Bolin and released under the GNU General Public License v3 (GPL-3.0).
