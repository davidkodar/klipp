# Architecture

Klipp is built as a Rust workspace with 11 crates, each responsible for a specific domain. Dependencies flow in one direction with no cycles.

## Crate Overview

### klipp-core
Foundation types shared across all crates: unique IDs, `RationalTime` for frame-accurate timing, event system, error types, and `ExportSettings`.

### klipp-engine
Media decode and playback engine. Wraps ffmpeg-next for video/audio decoding with hardware acceleration (CUDA NVDEC, VAAPI, software fallback). Manages the `MasterClock` for A/V sync, cpal audio output with ring buffer, and the frame queue.

### klipp-ui
The egui/eframe user interface. Contains all panels (viewer, timeline, browser, inspector, preferences), the history system (undo/redo), export dialog, and theme definitions.

### klipp-timeline
Timeline data model: `Sequence`, `Track`, `Clip`, and `Transition` types. Implements the magnetic timeline behavior, ripple editing, and snapping logic.

### klipp-media
Media management: `MediaPool` for tracking imported files, `MediaItem` metadata, `ThumbnailCache` for browser thumbnails, and `FilmstripFrame` generation for timeline filmstrip display.

### klipp-project
Project persistence. Saves and loads `.klipp` directory format containing timeline data, media references, effect parameters, and project settings.

### klipp-effects
GPU effects pipeline using wgpu compute shaders (Vulkan backend). Includes 6 built-in effects: Opacity, Gaussian Blur, Sharpen, Crop, Transform, and Chroma Key. Uses ping-pong storage buffers with uniform parameters. Includes CPU fallback path.

### klipp-color
Color grading and color science. Color wheels, curves, hue/saturation, color space conversions (Rec.709, Rec.2020, ACEScg), and LUT support.

### klipp-audio
Audio processing: EQ, compressor, multiband compressor, fade curves, and loudness metering.

### klipp-export
Export pipeline: encoding via ffmpeg-next, render queue management, and interchange format generation (FCPXML, EDL).

### klipp-app
Binary entry point. Initializes logging (tracing_subscriber to `klipp-debug.log`), creates the eframe window, and wires all subsystems together.

## Decode Pipeline

The video decode pipeline follows a priority chain:

1. **CUDA NVDEC** -- NVIDIA hardware decode. Best performance for all supported codecs.
2. **VAAPI** -- VA-API hardware decode. Note: broken for AV1 and VP9 on nvidia-vaapi-driver (produces all-zero frames).
3. **Software** -- FFmpeg software decode with `thread_count=0` (auto multi-threading).

The decode loop uses a two-phase pattern: drain all frames from the decoder before sending new packets. This prevents EAGAIN-related packet loss.

## GPU Effects Pipeline

Effects are processed via wgpu compute shaders on the Vulkan backend:

- WGSL shaders compiled at startup
- Ping-pong buffer pattern for effect chaining
- Uniform structs pass parameters to shaders
- Premultiplied alpha for correct compositing
- CPU fallback produces identical output when GPU is unavailable

## Audio Pipeline

- **Output**: cpal with PipeWire or ALSA backend
- **Buffering**: Ring buffer (~2 second capacity) with overflow buffer for excess samples
- **Sync**: All audio slaves to the `MasterClock` (external monotonic clock)
- **Resampling**: Automatic resampling to match output device sample rate, recreated on seek

## A/V Synchronization

The `MasterClock` in klipp-engine is the single source of truth for playback timing. It tracks an anchor PTS, wall-clock anchor, speed, and generation counter. Both audio and video slave to this clock. Seek operations pause the clock, flush buffers, reposition decoders, and resume.
