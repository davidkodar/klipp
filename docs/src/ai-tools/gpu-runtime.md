# GPU Runtime

This page explains how Klipp's AI inference picks a backend at runtime, what you need on your system to get GPU acceleration, and how to diagnose the most common "why is it on CPU" issues.

## The Backend Stack

Two ML runtimes ship with Klipp:

- **`ort`** (ONNX Runtime via the `ort` crate) — used for BiRefNet and RF-DETR-Seg. ONNX models load through `ort` with the CUDA Execution Provider (EP) when available, CPU EP otherwise.
- **`candle`** (pure-Rust ML via the `candle-core` crate) — used for MobileSAM. SafeTensors models load through `candle` with the CUDA backend when available, CPU backend otherwise.

Both runtimes are independent: `ort` can be on GPU while `candle` is on CPU, or vice versa, depending on how Klipp was built.

## Build-Time vs Runtime

The single most common point of confusion. Two things are needed for full GPU AI:

| Thing | Provided by | Needed at build | Needed at runtime |
|---|---|---|---|
| `nvcc` (CUDA compiler) | CUDA Toolkit | only for `ai-candle-cuda` | never |
| `libcudart.so.12`, `libcublasLt.so.12` | CUDA runtime libraries | yes (linker) | yes |
| `libcudnn.so.9` | cuDNN package | yes | yes |
| `libcuda.so.1` | NVIDIA driver | yes | yes (driver) |
| `libonnxruntime.so` | ort prebuilt or distro `onnxruntime` | resolved at runtime via dynamic loading | yes |

**Bottom line for end users:** never need the CUDA Toolkit (`nvcc`). Just need the runtime libraries — your distribution's `cudnn` package pulls them all in.

**Bottom line for source builds:** `cargo r-ort-cuda` works without `nvcc`; `cargo r-cuda` (which adds `ai-candle-cuda`) requires `nvcc` because `cudarc` compiles CUDA kernels during the Rust build.

## Resolving libonnxruntime

Klipp uses `ort` in *load-dynamic* mode — it does not link against `libonnxruntime.so` at build time. Instead, on startup, `klipp_ml_ort::find_libonnxruntime()` probes a list of candidate paths in order:

1. `$ORT_DYLIB_PATH` if set (user override)
2. `~/.local/lib/onnxruntime/lib/libonnxruntime.so`
3. `~/.local/lib/libonnxruntime.so`
4. `/usr/lib/libonnxruntime.so`
5. `/usr/local/lib/libonnxruntime.so`
6. `/opt/onnxruntime/lib/libonnxruntime.so`

The first path whose file exists is set as `ORT_DYLIB_PATH` for the rest of the process. Klipp does this **before** any thread that might touch ort spawns, so the env mutation is safe.

If you have ort installed at a non-standard location, set `ORT_DYLIB_PATH` before launching Klipp:

```bash
export ORT_DYLIB_PATH=/path/to/libonnxruntime.so
cargo r
```

## GPU Vendor Detection

`klipp_ml_ort::detect_gpu_vendor()` runs at startup and probes:

1. `lspci -d ::0300` — looks for VGA and 3D controllers
2. `/sys/module/nvidia` — fallback marker for the proprietary NVIDIA driver
3. `/sys/module/amdgpu` — fallback marker for AMD

The detected vendor combined with the loaded ort runtime determines what the AI Tools panel and AI Models dialog footer show:

| Vendor | ort | Status |
|---|---|---|
| NVIDIA | found, with CUDA EP | `NVIDIA CUDA — active` (green) |
| NVIDIA | found, CUDA EP missing or libs absent | `GPU detected — runtime libs missing` (amber) |
| NVIDIA | not found | `ort runtime not found` (amber) |
| AMD / Intel | found | `CPU only — vendor unsupported` (amber) |
| None / Unknown | found | `CPU only — no GPU detected` (grey) |

## What "Runtime Libs Missing" Means

You will see this if the NVIDIA driver is installed (so `libcuda.so.1` is present) but the cuDNN runtime libraries (`libcudart.so.12`, `libcublas.so.12`, `libcudnn.so.9`) are not.

Fix:

```bash
# Arch / CachyOS
sudo pacman -S cudnn

# Ubuntu / Debian (CUDA 12 line)
sudo apt install libcudnn9-cuda-12

# Fedora
sudo dnf install libcudnn9
```

Relaunch Klipp. The footer should now show `NVIDIA CUDA — active`.

## AMD and Intel

AI inference on AMD and Intel GPUs runs on the CPU: Klipp's `ort` build registers only NVIDIA's CUDA execution provider. **Tools → System Probe…** reports which execution provider each model actually ran on.

## Auto-Configured Environment

Klipp's `main()` sets two environment variables at startup if they are not already set, so you do not need shell prefixes:

- `RUST_MIN_STACK=1073741824` (1 GiB) — needed for the release-profile LTO pass on `klipp-app`
- `ORT_DYLIB_PATH` — set to the path resolved by `find_libonnxruntime()` above

If you have specific reasons to override these (e.g. a custom ort build), set them in your shell before launching and Klipp will leave them alone.

## Troubleshooting

**"My RTX is detected but inference is on CPU"**
Almost always cuDNN runtime libraries missing — install `cudnn` per above.

**"libonnxruntime.so not found"**
Either install `onnxruntime` from your distro, or download a prebuilt from [github.com/microsoft/onnxruntime/releases](https://github.com/microsoft/onnxruntime/releases) and extract to `~/.local/lib/onnxruntime/`.

**"MobileSAM is on CPU even though everything else is on GPU"**
You built without `ai-candle-cuda`. MobileSAM uses candle, not ort, and candle has its own CUDA build flag. Use `cargo r-cuda` instead of `cargo r-ort-cuda`. This requires `nvcc` (CUDA Toolkit) at build time.

**"I see `RUNTIME_LIBS_MISSING` but I have cuDNN installed"**
Check `LD_LIBRARY_PATH` and that `libcudnn.so.9` (not `.so.8`) is on it. Klipp targets the CUDA 12 line; CUDA 11 cuDNN will not be picked up.
