# Architecture

Klipp is built as a Rust workspace with focused crates, each responsible for a specific domain. Dependencies flow in one direction with no cycles.

## Crate Overview

### Foundation
- **klipp-core** — Foundation types: IDs, `RationalTime`, events, errors, `ExportSettings`. No klipp-* dependencies.
- **klipp-project** — `.klipp` directory format, serialisation, versioning.

### Data Model
- **klipp-timeline** — `Sequence`, `Track`, `Clip`, `Transition`. Magnetic timeline behaviour, ripple, snapping.
- **klipp-media** — `MediaPool`, `MediaItem`, `ThumbnailCache`, `FilmstripFrame`.
- **klipp-nodes** — Per-clip node graph (`NodeKind`, `ExternalMatte`, `Corrector`, `MaskCombine`).

### Engine
- **klipp-engine** — ffmpeg-next decode (CUDA NVDEC / software), cpal audio, `MasterClock` A/V sync, frame queue.
- **klipp-effects** — GPU compute-shader effects (wgpu/Vulkan): Opacity, Blur, Sharpen, Crop, Transform, ChromaKey. CPU fallback.
- **klipp-stab** — Motion stabilisation. `SequentialFrameDecoder` for ~18 s 4K analysis.
- **klipp-scenedetect** — Scene-change detection (content + threshold algorithms).
- **klipp-title** — Title generator: text shaping (cosmic-text), GPU rasterisation, animation curves.

### ML
- **klipp-ml** — Manifest schema, model registry, download/verify pipeline, telemetry.
- **klipp-ml-ort** — `ort`-backed ONNX inference (BiRefNet, RF-DETR-Seg). Load-dynamic, runtime resolution, GPU vendor probe.
- **klipp-ml-candle** — `candle`-backed pure-Rust inference (MobileSAM).

### Test Infra
- **klipp-smoke** — End-to-end smoke tests covering import → edit → export.
- **klipp-testing** — Snapshot-test scaffolding for parity / regression checks.

### UI
- **klipp-ui** — egui/eframe interface. All panels (viewer, timeline, browser, inspector, AI tools), history (undo/redo), export dialog, theme registry, command palette, keybindings.
- **klipp-jobs** — Background job runner used by export, stabilisation, scene detect, AI inference.
- **klipp-app** — Binary entry point. Tracing setup (file output to `klipp-debug.log`), eframe window, env-var auto-config.

## Dependency Graph

```
klipp-app
  └── klipp-ui
        ├── klipp-engine    ← ffmpeg, cpal, MasterClock
        ├── klipp-effects   ← wgpu compute shaders
        ├── klipp-stab      ← stabilisation
        ├── klipp-scenedetect
        ├── klipp-title
        ├── klipp-jobs      ← background job queue
        ├── klipp-timeline  ← editing model
        ├── klipp-media     ← decode + thumbnails
        ├── klipp-nodes     ← per-clip node graph
        ├── klipp-ml-ort    ← ort inference (feature-gated)
        ├── klipp-ml-candle ← candle inference (feature-gated)
        ├── klipp-ml        ← manifest, registry
        └── klipp-project   ← file format
              └── klipp-core ← shared types
```

## Decode Pipeline

The video decode pipeline:

1. **CUDA NVDEC (cuvid)** — Auto-attached `*_mp4toannexb` BSFs, GPU-side resize cap at 1080p for playback. Best performance on NVIDIA.
2. **Software** — FFmpeg software decode with `thread_count=0` (auto multi-threading).

VAAPI is currently disabled by default — the `nvidia-vaapi-driver` returns all-zero frames for AV1 and VP9.

The decode loop uses a strict two-phase pattern:

1. Drain all frames from the decoder (`receive_frame`) before sending new packets (`send_packet`)
2. Save and retry packets on EAGAIN; never silently drop

For 10-bit log YUV (`yuv420p10le`), the pipeline bypasses libswscale entirely — libswscale's RGBA64LE output for log YUV is channel-clamped junk regardless of `sws_setColorspaceDetails`. A direct decoder reads YUV planes at full 10-bit precision and applies the source colourspace inverse to ACEScg directly.

## A/V Synchronisation

The `MasterClock` in klipp-engine is the single source of truth for playback timing:

- Anchor PTS (microseconds)
- Wall-clock anchor
- Speed
- Generation counter (rejects stale frames after a seek)

Both audio and video slave to this clock. Seek operations:

1. Pause the clock
2. Flush audio ring buffer
3. Reposition decoders
4. Set new anchor
5. Flush video frame queue
6. Resume

The UI playhead interpolates `anchor_secs + wall_elapsed * speed` at monitor refresh rate, decoupling display from frame rate.

## GPU Effects Pipeline

Effects run via wgpu compute shaders on Vulkan:

- WGSL shaders compiled at startup
- Ping-pong storage buffers for chaining
- `f16` storage / `f32` compute for HDR-safe maths
- Premultiplied alpha for correct compositing
- CPU fallback produces float-identical output

Engine holds `Option<GpuEffectsPipeline>`. `post_process_rgba` picks GPU or CPU based on availability.

## ML Inference Stack

Two runtimes:

- **ort** (load-dynamic) — ONNX Runtime via the `ort` crate. CUDA EP at runtime if the libs are present. Used by BiRefNet and RF-DETR-Seg.
- **candle** — Pure-Rust ML. CUDA via cudarc (build-time nvcc). Used by MobileSAM.

`klipp_ml_ort::find_libonnxruntime()` probes a path list at startup and sets `ORT_DYLIB_PATH`. `klipp_ml_ort::detect_gpu_vendor()` runs `lspci` + `/sys/module/*` fallback. The combined runtime status drives the AI Tools panel and AI Models dialog footer.

## Audio Pipeline

- **Output** — cpal with PipeWire / ALSA / JACK backend selection
- **Buffering** — Ring buffer (~2 s, ringbuf 0.4) plus overflow buffer for excess samples on full ring
- **Sync** — Audio slaves to `MasterClock`
- **Resampling** — Recreated on seek to avoid stale-buffer artefacts

The audio frame linesize trap: `resampled.data(0).len()` includes padding. Real data length is `resampled.samples() * channels * sizeof(f32)`. Reading padding bytes produces loud artefacts.

## Concurrency Model

- The UI thread owns `AppState` and `KlippApp`. It does not block.
- Heavy work (decode, thumbnails, filmstrips, export, scene detect, stabilise) runs on background threads.
- All cross-thread communication flows through crossbeam channels.
- Shared mutable state uses `Arc<Mutex<_>>` or `Arc<RwLock<_>>` with `unwrap_or_else(|e| e.into_inner())` to recover from poison.
- The engine never knows about egui — communication is `AppCommand` / `AppEvent`.

## Logging

Klipp writes structured logs to `klipp-debug.log` (file, not stderr) via `tracing_subscriber`. Log targets follow standard crate names; do **not** use `tracing::info!(target: "...")` with custom targets — the `EnvFilter` whitelist won't match and events are silently dropped.
