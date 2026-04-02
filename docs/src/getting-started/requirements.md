# System Requirements

## Minimum Requirements

| Component | Requirement |
|-----------|-------------|
| OS | Linux x86_64 (kernel 5.15+) |
| CPU | 4-core x86_64 processor |
| RAM | 8 GB |
| GPU | Any GPU with Vulkan 1.1 support |
| Storage | SSD recommended for media projects |
| Display | Wayland or X11 |

With minimum specs, Klipp operates using software decode and CPU-based effects. Editing 1080p content is practical; 4K may require proxy workflows.

## Recommended Requirements

| Component | Requirement |
|-----------|-------------|
| OS | Linux x86_64 (kernel 6.1+) |
| CPU | 8+ core processor (AMD Ryzen 7 / Intel i7 or better) |
| RAM | 32 GB |
| GPU | NVIDIA RTX series (CUDA/NVDEC support) |
| Storage | NVMe SSD |
| Display | Wayland compositor (sway, KDE Plasma, GNOME) |

With recommended specs, Klipp provides real-time 4K playback via NVDEC hardware decode, GPU-accelerated effects through Vulkan compute shaders, and smooth multi-track editing.

## Supported Display Servers

- **Wayland** -- Full support. Tested with KDE Plasma, GNOME, sway, and Hyprland.
- **X11** -- Full support.

## GPU Support Details

### NVIDIA (Recommended)
- CUDA NVDEC for hardware video decode (GTX 900 series and later)
- Vulkan compute for GPU effects
- Klipp auto-detects CUDA availability at startup

### AMD
- Vulkan compute for GPU effects via RADV or AMDVLK
- Video decode via software fallback (VAAPI support planned)

### Intel
- Vulkan compute for GPU effects via ANV
- Video decode via software fallback

## Audio Backend

Klipp uses cpal for audio output and supports:
- **PipeWire** (recommended, default on most modern distributions)
- **ALSA** (direct hardware access)
