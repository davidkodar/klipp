# Installation

Klipp is built from source using the Rust toolchain. There are no pre-built binaries at this time.

## Prerequisites

Install the Rust toolchain via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Klipp requires the stable Rust toolchain (1.75 or later).

## System Dependencies

### CachyOS / Arch Linux

```bash
sudo pacman -S ffmpeg clang pkg-config vulkan-icd-loader vulkan-headers
# For NVIDIA CUDA/NVDEC support:
sudo pacman -S cuda nvidia-utils
```

### Ubuntu / Debian

```bash
sudo apt install libavcodec-dev libavformat-dev libavutil-dev libavfilter-dev \
    libswscale-dev libswresample-dev libavdevice-dev \
    libclang-dev pkg-config libvulkan-dev \
    libasound2-dev libpipewire-0.3-dev
# For NVIDIA CUDA/NVDEC support:
sudo apt install nvidia-cuda-toolkit
```

### Fedora

```bash
sudo dnf install ffmpeg-free-devel clang-devel pkg-config vulkan-loader-devel \
    vulkan-headers alsa-lib-devel pipewire-devel
# For NVIDIA CUDA/NVDEC support:
sudo dnf install cuda
```

## Building

Clone the repository and build in release mode:

```bash
git clone https://github.com/davidbolin/klipp.git
cd klipp
cargo rr
```

The `cargo rr` alias builds and runs Klipp in release mode with optimizations. For development builds, use `cargo r` (debug mode) or `cargo c` for clippy checks.

## Optional: CUDA Toolkit

CUDA is optional but strongly recommended for NVIDIA GPU users. Without it, Klipp falls back to VAAPI or software decode. The CUDA toolkit provides NVDEC hardware decoding which enables real-time 4K playback.

## Optional: Vulkan Drivers

Vulkan is required for GPU-accelerated effects. Klipp falls back to CPU processing if Vulkan is unavailable.

- **NVIDIA**: Install `nvidia-utils` (Arch) or `nvidia-driver` (Debian/Ubuntu).
- **AMD**: Install `vulkan-radeon` (RADV) or `amdvlk`.
- **Intel**: Install `vulkan-intel`.

## Verifying the Installation

Launch Klipp and press **P** to open the performance overlay. It displays the active decode backend (NVDEC, VAAPI, or Software) and whether GPU effects are enabled.
