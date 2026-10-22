# Troubleshooting

Common issues and solutions.

## Log File

Klipp writes diagnostic logs to `klipp-debug.log` in the working directory. The file contains decode pipeline selection, GPU initialisation, audio device setup, ML runtime resolution, and any errors.

When reporting issues, include the relevant sections.

## Performance Overlay

Press **Ctrl+Shift+P** to toggle the performance overlay. It displays:

- UI FPS, decode backend (NVDEC / VAAPI / Software)
- Playback state, frame queue depth, dropped frames
- CPU usage (process-specific, EMA-smoothed) and memory in MB

The fastest way to diagnose playback and performance problems.

## Common Issues

### VAAPI Produces Black Frames (NVIDIA)

**Symptom** — Video plays but shows all-black or all-zero frames when using VAAPI on NVIDIA.

**Cause** — `nvidia-vaapi-driver` has known issues with AV1 and VP9, producing all-zero frame data.

**Fix** — None needed: Klipp tries CUDA NVDEC before VAAPI, and AV1 and VP9 skip VAAPI on NVIDIA.

### Vulkan Not Detected (CachyOS / Arch)

**Symptom** — Klipp reports no usable GPU, or the performance overlay shows no GPU.

**Cause** — On CachyOS, the NVIDIA Vulkan ICD lives at `/usr/share/vulkan/icd.d/nvidia_icd.json` rather than `/etc/vulkan/icd.d/`.

**Fix** — Klipp sets `VK_DRIVER_FILES` automatically. If issues persist, set it manually:

```bash
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/nvidia_icd.json
cargo r
```

### Choppy 4K Playback

**Symptom** — 4K footage plays with dropped frames or stuttering.

**Fixes**

1. Ensure CUDA NVDEC is the active decode backend (check the performance overlay)
2. Set **Playback quality** to **Half** in **Preferences → Playback**, or lower **Max playback resolution** in **Preferences → System**
3. Increase **Frame queue size** in **Preferences → System**
4. Close other GPU-intensive applications
5. Use proxy media for editing, switch to full resolution for export

### Audio Crackling or Dropouts

**Symptom** — Audio plays with pops, clicks, or gaps.

**Fixes**

1. Increase the buffer size in **Preferences → Audio Hardware** (try 1024 or 2048 samples)
2. Check that PipeWire or ALSA is running (`systemctl --user status pipewire`)
3. Ensure no other application has exclusive access to the audio device
4. Verify CPU usage is not at 100% — heavy effects can starve the audio thread

### No Audio Output

**Symptom** — Video plays but no sound.

**Fixes**

1. Check the correct output device is selected in **Preferences → Audio Hardware**
2. Verify system and application volume are not muted
3. Check that the clip has audio (some files are video-only)
4. Try a different output device or the system default

### High Memory Usage

**Symptom** — Klipp uses excessive RAM during long editing sessions.

**Fixes**

1. Reduce **Frame queue size** in **Preferences → System**
2. Clear caches in **Preferences → Media & Cache**, or with **Tools → Cache Manager…**
4. Close and reopen large projects periodically

### Slow Startup

**Symptom** — Klipp takes a long time to launch.

**Cause** — On a clean install, the first launch builds the font registry by scanning system fonts (typically 16 s on a fully-populated system).

**Fix** — Subsequent launches should be ~175 ms because the font registry is cached at `~/.cache/klipp/font_registry.json`. If startup consistently exceeds a few seconds after the first launch, the cache may be invalidated — check that the cache file exists and is readable.

### AI Inference on CPU Even Though I Have an NVIDIA GPU

**Symptom** — AI Tools panel footer shows "GPU detected — runtime libs missing" or "CPU only".

**Cause** — `cudnn` runtime libraries are not installed.

**Fix** — Install `cudnn` for your distribution:

```bash
sudo pacman -S cudnn          # Arch / CachyOS
sudo apt install libcudnn9-cuda-12   # Ubuntu / Debian
sudo dnf install libcudnn9    # Fedora
```

See [GPU Runtime](../ai-tools/gpu-runtime.md) for the full chain.

### libonnxruntime.so Not Found

**Symptom** — AI Models dialog shows "ort runtime not found".

**Fix** — Install `onnxruntime` from your distro, or download a prebuilt from the upstream releases page and extract to `~/.local/lib/onnxruntime/`. Klipp probes `~/.local/lib/onnxruntime*`, `/usr/lib/`, `/usr/local/lib/`, and `/opt/onnxruntime/lib/` automatically.

### Build Fails: "Failed to execute nvcc"

**Symptom** — `cargo r-cuda` or `cargo build --features ai-candle-cuda` fails with a `cudarc` error mentioning `nvcc`.

**Cause** — `cudarc` requires the CUDA Toolkit at build time.

**Fix** — Install CUDA Toolkit, or use `cargo r-ort-cuda` (no nvcc required, covers BiRefNet + RF-DETR-Seg but not MobileSAM).

```bash
sudo pacman -S cuda           # CachyOS / Arch — provides /opt/cuda/bin/nvcc
sudo apt install nvidia-cuda-toolkit  # Ubuntu / Debian
```

### Mask Was Applied but the Clip Looks Unchanged

**Symptom** — Apply mask from AI Tools succeeded (toast appeared), but the clip's appearance hasn't changed.

**Cause** — The mask is wired to the clip's first Corrector, but if the Corrector has no active grade, the matte has nothing to gate. The matte itself doesn't change the image — it gates a colour operation.

**Fix** — Open the inspector's Colour tab or the Colour workspace and apply some grade with the wheels or curves. The mask now restricts that grade to its area.

## Performance Tips

- Use CUDA NVDEC for all video decode on NVIDIA GPUs
- Set playback resolution to **Half** for 4K projects
- Keep frame queue at 16 (default) unless experiencing issues
- Show the scopes only when grading (they add GPU load)
- Close unused panels to reduce UI rendering overhead
- Use background rendering for effects-heavy timelines
- Pre-cache thumbnails by pre-loading bins in the browser before editing

## Reporting Bugs

Bugs go to the issue tracker on GitHub — **Help → Report a Bug** opens
it, and there is a form for a bug and one for a feature request. What
a report needs:

- What you did, what happened, and what you expected
- Your distribution and kernel (`uname -r`)
- GPU model and driver version (`vulkaninfo --summary | head -30`)
- The build you are on, from **Help → About**
- The relevant part of `klipp-debug.log`

A security problem does not go here — `SECURITY.md` says where to send
it privately.
