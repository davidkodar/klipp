# System Requirements

## Minimum Requirements

| Component | Requirement |
|-----------|-------------|
| OS | Linux x86_64 (kernel 5.15+) |
| CPU | 4-core x86_64 processor |
| RAM | 8 GB |
| GPU | Any GPU with Vulkan 1.1 support (integrated is fine) |
| Storage | 10 GB for app + cache; SSD recommended |
| Display | Wayland or X11 |

A working Vulkan GPU is required: the viewer, effects, titles and scopes run on it. With minimum specs, decode may be software and AI inference runs on the CPU. Editing 1080p content is practical; 4K may require proxy workflows. AI tools work but are slower (BiRefNet on CPU: ~5–8 seconds per frame on a 4-core CPU).

## Recommended Requirements

| Component | Requirement |
|-----------|-------------|
| OS | Linux x86_64 (kernel 6.1+) |
| CPU | 8+ core processor (AMD Ryzen 7 / Intel i7 or better) |
| RAM | 32 GB |
| GPU | NVIDIA RTX series (CUDA/NVDEC + ML inference) |
| Storage | NVMe SSD with 50+ GB free for media + model cache |
| Display | Wayland compositor (KDE Plasma, GNOME, sway, Hyprland) |

With recommended specs, Klipp provides:
- Real-time 4K playback via NVDEC hardware decode
- GPU-accelerated effects through Vulkan compute shaders
- Sub-second AI inference (BiRefNet on RTX 4080: ~250 ms per frame)
- Smooth multi-track editing

## Supported Display Servers

- **Wayland** — Full support. Tested with KDE Plasma, GNOME, sway, and Hyprland.
- **X11** — Full support.

## GPU Support by Subsystem

Klipp's GPU acceleration is split across three independent subsystems. Each has its own backend story.

| Subsystem | NVIDIA | AMD | Intel | CPU fallback |
|---|---|---|---|---|
| Video decode | NVDEC (cuvid) | VAAPI | VAAPI | ✓ (FFmpeg threaded) |
| GPU effects | Vulkan (wgpu) | Vulkan (wgpu) | Vulkan (wgpu) | ✓ |
| AI inference (ort) | CUDA EP | CPU | CPU | ✓ |
| AI inference (candle) | CUDA | CPU | CPU | ✓ |

A stream moves to software decode when the GPU's decoder is full or refuses it.

**Effects parity is full across vendors** — RADV and AMDVLK on AMD, ANV on Intel, and the proprietary NVIDIA driver all work for Klipp's compute shaders.

**AI inference currently accelerates only on NVIDIA.** AMD and Intel users get CPU AI, which is functional but several times slower for the heavier models.

## NVIDIA-Specific Notes

For full GPU acceleration:

1. Install the proprietary NVIDIA driver (your distribution's `nvidia-utils` / `nvidia-driver` package).
2. Install the cuDNN runtime library (`pacman -S cudnn`, `apt install libcudnn9-cuda-12`, or equivalent). This pulls in `libcudart`, `libcublas`, etc. — roughly 600 MB.
3. Run Klipp with the CUDA build alias: `cargo r-ort-cuda` for ONNX models, `cargo r-cuda` for full coverage including MobileSAM.

GTX 900 series and later are supported for NVDEC. RTX 20-series and later are recommended for AI workloads.

## Disk Space

| What | Size |
|---|---|
| Klipp source + dev build | well over 100 GB of build output in `target/` |
| Klipp release build | ~250 MB |
| AI model cache (`~/.local/share/klipp/models/`) | about 1.4 GB with every model installed; each downloads only when first asked for |
| AI mask cache (`~/.cache/klipp/ai_masks/`) | capped at 2 GB, LRU-evicted |
| Font registry cache | < 1 MB |

Models are downloaded on demand via the AI Models dialog and verified by SHA-256 against the bundled manifest.

## Audio Backend

Klipp uses cpal for audio output and supports:
- **PipeWire** (recommended; default on most modern distributions)
- **ALSA** (direct hardware access)

JACK is supported via PipeWire's JACK compatibility layer.
