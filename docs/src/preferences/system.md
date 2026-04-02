# System Preferences

Access system preferences from **Edit > Preferences > System**. These settings control hardware acceleration, decode pipeline, and performance tuning.

## GPU Preference

Select the preferred GPU for effects processing:

- **Auto** -- Klipp selects the best available GPU (default)
- **Specific GPU** -- Choose from detected Vulkan-capable GPUs

If no Vulkan GPU is available, effects fall back to CPU processing automatically.

## Hardware Decode

Configure the video decode pipeline:

- **Auto** -- Tries CUDA NVDEC first, then VAAPI, then software (default)
- **CUDA NVDEC** -- Force NVIDIA hardware decode (requires CUDA toolkit)
- **VAAPI** -- Force VA-API hardware decode
- **Software** -- Force CPU-only decode (uses all available cores)

CUDA NVDEC is recommended for NVIDIA GPU users. It provides the best 4K playback performance. Software decode uses `thread_count=0` for automatic multi-threading.

## Threading

- **Decode threads** -- Number of threads for software video decode (default: auto, uses all cores)
- **Background render threads** -- Number of threads for background rendering tasks

## Frame Queue

- **Frame queue size** -- Number of decoded frames buffered ahead of playback (default: 16)

A larger queue provides smoother playback at the cost of higher memory usage and slightly increased seek latency. Reduce the queue size if memory is constrained.

## Playback Resolution

- **Full** -- Decode at source resolution
- **Half** -- Decode at half resolution (default for 4K content)
- **Quarter** -- Decode at quarter resolution

Lower playback resolution improves performance when editing high-resolution content. Exports always render at full resolution regardless of this setting.

## Playback Quality

- **High** -- Best quality scaling and color processing
- **Performance** -- Faster processing with minor quality trade-offs (default)

## Background Render

- **Enable background render** -- Pre-render complex sections during idle time
- **Start after idle** -- Seconds of inactivity before background rendering begins (default: 5)

Background rendering pre-computes effects-heavy sections so they play back smoothly without real-time processing.

## Cache Settings

- **Cache location** -- Directory for decoded frame cache and rendered previews
- **Maximum cache size** -- Disk space limit for cache files (default: 10 GB)
- **Clear cache** -- Delete all cached data to free disk space
