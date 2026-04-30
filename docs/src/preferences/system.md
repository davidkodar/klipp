# System Preferences

Access system preferences from **Edit → Preferences → System**. These settings control hardware acceleration, decode pipeline, and performance tuning.

## GPU Preference

Select the preferred GPU for effects processing:

- **Auto** — Klipp picks the best available GPU (default)
- **Specific GPU** — choose from detected Vulkan-capable GPUs

If no Vulkan GPU is available, effects fall back to CPU processing automatically.

## Hardware Decode

Configure the video decode pipeline:

- **Auto** — tries CUDA NVDEC first, then software (default)
- **CUDA NVDEC (cuvid)** — force NVIDIA hardware decode
- **Software** — force CPU-only decode (uses all available cores via `thread_count=0`)

CUDA NVDEC uses the cuvid path with auto-attached `*_mp4toannexb` BSFs and a 1080p resize cap on the GPU side, the combination that gives smooth 4K playback on NVIDIA. VAAPI is currently disabled by default — `nvidia-vaapi-driver` returns all-zero frames for AV1 and VP9.

## ML Inference

Klipp's AI inference path is configured by the build features at compile time (see [Installation](../getting-started/installation.md)). At runtime, the AI Models dialog footer shows the resolved path; this preferences page surfaces the same information for visibility.

- **ort runtime path** — auto-resolved at startup; override via `ORT_DYLIB_PATH`
- **GPU vendor** — auto-detected; informational
- **CUDA EP status** — green / amber / off

## Threading

- **Decode threads** — Number of threads for software video decode (default: auto, all cores)
- **Background render threads** — Threads for background rendering tasks
- **Thumbnail threads** — Threads for filmstrip generation (default: 2)

## Frame Queue

- **Frame queue size** — Number of decoded frames buffered ahead of playback (default: 16)

A larger queue provides smoother playback at the cost of higher memory use and slightly increased seek latency. Reduce if memory is constrained.

## Playback Resolution

- **Full** — Decode at source resolution
- **Half** — Decode at half resolution (default for 4K content)
- **Quarter** — Decode at quarter resolution

Lower playback resolution improves performance when editing high-resolution content. Exports always render at full resolution regardless of this setting.

## Playback Quality

- **High** — Best quality scaling and colour processing
- **Performance** — Faster processing with minor quality trade-offs (default)

## Background Render

- **Enable background render** — pre-render complex sections during idle time
- **Start after idle** — seconds of inactivity before background rendering begins (default: 5)

Background rendering pre-computes effects-heavy sections so they play back smoothly without real-time processing.

## Cache Settings

| Cache | Default location | Default size cap |
|---|---|---|
| Frame cache | `~/.cache/klipp/frames/` | 10 GB |
| Thumbnails | `~/.cache/klipp/thumbnails/` | 2 GB |
| AI mask cache | `~/.cache/klipp/ai_masks/` | 2 GB (LRU) |
| AI models | `~/.cache/klipp/models/` | uncapped (model size driven) |
| Font registry | `~/.cache/klipp/font_registry.json` | small |

**Clear cache** — delete cached data to free disk space. Models persist (they have their own management in the AI Models dialog).
