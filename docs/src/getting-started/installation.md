# Installation

Klipp is built from source using the Rust toolchain. Pre-built binaries are not yet available.

## Prerequisites

Install the Rust toolchain via [rustup](https://rustup.rs/):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Klipp builds on stable Rust — `rust-toolchain.toml` pins the channel, so rustup fetches the right one. Anything recent works; the code uses standard-library items from 1.80.

## System Dependencies

### CachyOS / Arch Linux

```bash
sudo pacman -S rust ffmpeg clang lld cmake pkg-config \
    vulkan-icd-loader vulkan-headers shaderc lilv
```

For NVIDIA GPU acceleration of AI inference (recommended):

```bash
# Runtime libraries (for ort CUDA Execution Provider — no nvcc needed)
sudo pacman -S cudnn

# Optional: full CUDA Toolkit (only if building with --features ai-candle-cuda)
sudo pacman -S cuda
```

### Ubuntu / Debian

```bash
sudo apt install rustup ffmpeg libavcodec-dev libavformat-dev libavutil-dev \
    libavfilter-dev libswscale-dev libswresample-dev libavdevice-dev \
    libclang-dev lld cmake pkg-config libvulkan-dev glslc \
    libasound2-dev libpipewire-0.3-dev liblilv-dev

# Runtime CUDA libraries for AI inference
sudo apt install libcudnn9-cuda-12

# Optional: full CUDA Toolkit
sudo apt install nvidia-cuda-toolkit
```

### Fedora

```bash
sudo dnf install ffmpeg-free-devel clang-devel lld cmake pkg-config \
    vulkan-loader-devel vulkan-headers glslc alsa-lib-devel pipewire-devel lilv-devel

# Runtime CUDA libraries
sudo dnf install libcudnn9
```

## Building

Clone the repository and build:

```bash
git clone https://github.com/davidkodar/klipp.git
cd klipp
./scripts/vulkan-sdk.sh   # once: lays out the Vulkan SDK directory the
                          # speech model's build expects
cargo r                   # debug build, run
```

`scripts/vulkan-sdk.sh` is not optional on a first build:
`.cargo/config.toml` points `VULKAN_SDK` at `vendor/vulkan-sdk`, and
whisper.cpp's Vulkan backend compiles its shaders against it. `cmake`
and a C++ compiler are needed for the same reason.

The first build downloads dependencies and compiles ~400 crates — expect 5–10 minutes. Incremental rebuilds are 1–10 seconds. AI features (BiRefNet, MobileSAM, RF-DETR-Seg) ship enabled by default and run on CPU out of the box.

## Cargo Aliases

Klipp's `.cargo/config.toml` provides shortcuts:

| Alias | Description |
|---|---|
| `cargo r` | Debug build and run |
| `cargo rr` | Release build and run |
| `cargo b` | Build only |
| `cargo c` | Clippy over the workspace |
| `cargo r-cuda` / `cargo rr-cuda` | Adds MobileSAM on the GPU; needs the CUDA Toolkit (nvcc) at build time |
| `cargo b-cuda` | Build only, full GPU AI |

The default build already includes `ai-ort`, `ai-ort-cuda` and
`ai-candle`, so a plain `cargo r` can use NVIDIA's execution provider
for BiRefNet and RF-DETR-Seg when the runtime libraries are installed,
and falls back to CPU when they are not. The `-cuda` aliases exist for
the one model that needs a build-time toolchain.

To build without the ONNX and Candle backends:

```bash
cargo run -p klipp-app --no-default-features
```

That drops subject masking and detection. It is not an ML-free build:
the analysers in `klipp-analysis` — speech transcription, framing and
on-screen text — are on by default and come in regardless.

## CUDA: Build-Time vs Runtime

There is a common point of confusion worth clearing up:

| | Needed at build | Needed at runtime |
|---|---|---|
| `nvcc` (CUDA compiler) | for `ai-candle-cuda` only | never |
| `libcudart.so.12`, `libcublasLt.so.12` | yes | yes |
| `libcudnn.so.9` | yes | yes |
| `libcuda.so.1` | yes | yes (ships with NVIDIA driver) |

**End users** running a binary never need the CUDA Toolkit (`nvcc`). They only need the runtime libraries — typically supplied by the `cudnn` package on most distributions, or bundled inside Klipp release packages.

**Building from source with full GPU AI** (`cargo r-cuda`) requires the full CUDA Toolkit because the `cudarc` crate compiles CUDA kernels with `nvcc` during the build. If you are unsure or do not need GPU acceleration for MobileSAM specifically, prefer `cargo r-ort-cuda` — it gives you GPU acceleration for BiRefNet and RF-DETR-Seg without the toolkit hassle.

## Vulkan Drivers

Vulkan is required for GPU-accelerated effects. Klipp falls back to CPU processing if Vulkan is unavailable.

- **NVIDIA**: Install `nvidia-utils` (Arch) or `nvidia-driver-XXX` (Debian/Ubuntu).
- **AMD**: Install `vulkan-radeon` (RADV, recommended) or `amdvlk`.
- **Intel**: Install `vulkan-intel`.

## Verifying the Installation

Launch Klipp:

```bash
cargo r
```

Then verify each subsystem:

- **Decode backend** — Press **P** to toggle the performance overlay. The "Decode" line shows `NVDEC`, `VAAPI`, or `Software`.
- **GPU effects** — The same overlay shows whether Vulkan compute is active.
- **AI runtime** — Open the AI Tools panel with **Alt+A**, then click "Manage AI models" in the panel footer. The dialog footer shows the resolved ONNX runtime and detected GPU vendor (e.g. `NVIDIA CUDA`, `CPU only`, or `GPU detected — runtime libs missing`).

If the AI dialog reports "GPU detected — runtime libs missing", install your distribution's `cudnn` package and relaunch Klipp.

## Troubleshooting

For common build and runtime issues, see [Troubleshooting](../reference/troubleshooting.md).
