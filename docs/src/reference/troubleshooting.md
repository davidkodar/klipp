# Troubleshooting

Common issues and solutions for Klipp.

## Log File

Klipp writes diagnostic logs to `klipp-debug.log` in the working directory. This file contains detailed information about decode pipeline selection, GPU initialization, audio device setup, and any errors encountered during operation.

When reporting issues, include the relevant sections of this log file.

## Performance Overlay

Press **P** to toggle the performance overlay. It displays:

- UI FPS, decode backend (NVDEC/SW), playback state
- Frame queue depth, dropped frames
- CPU and memory usage

This is the fastest way to diagnose playback and performance problems.

## Common Issues

### VAAPI Produces Black Frames (NVIDIA)

**Symptom**: Video plays but shows all-black or all-zero frames when using VAAPI decode on NVIDIA GPUs.

**Cause**: The nvidia-vaapi-driver has known issues with AV1 and VP9 codecs, producing all-zero frame data.

**Solution**: Set decode preference to CUDA NVDEC in **Edit > Preferences > System > Hardware Decode**. NVDEC handles all codecs correctly on NVIDIA hardware.

### Vulkan Not Detected (CachyOS/Arch)

**Symptom**: GPU effects fall back to CPU. Performance overlay shows no GPU effects.

**Cause**: On CachyOS and some Arch-based distributions, the NVIDIA Vulkan ICD file is located at `/usr/share/vulkan/icd.d/nvidia_icd.json` instead of the standard `/etc/vulkan/icd.d/` path.

**Solution**: Klipp sets the `VK_DRIVER_FILES` environment variable automatically. If issues persist, set it manually:

```bash
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/nvidia_icd.json
cargo rr
```

### Choppy 4K Playback

**Symptom**: 4K footage plays with dropped frames or stuttering.

**Solutions**:
1. Enable CUDA NVDEC in preferences (if using NVIDIA GPU)
2. Set playback resolution to **Half** in **Edit > Preferences > System**
3. Increase frame queue size (default 16, try 24 or 32)
4. Close other GPU-intensive applications
5. Use proxy media for editing, then switch to full resolution for export

### Audio Crackling or Dropouts

**Symptom**: Audio plays with pops, clicks, or gaps.

**Solutions**:
1. Increase audio buffer size in **Edit > Preferences > Audio** (try 1024 or 2048 samples)
2. Check that PipeWire or ALSA is running correctly
3. Ensure no other application has exclusive access to the audio device
4. Verify CPU usage is not at 100% (heavy effects can starve the audio thread)

### No Audio Output

**Symptom**: Video plays but no sound is heard.

**Solutions**:
1. Check the correct output device is selected in **Edit > Preferences > Audio**
2. Verify system volume and application volume are not muted
3. Check that the clip has audio (some video files are video-only)
4. Try selecting a different output device or the system default

### High Memory Usage

**Symptom**: Klipp uses excessive RAM during long editing sessions.

**Solutions**:
1. Reduce frame queue size in **Edit > Preferences > System**
2. Clear the cache in **Edit > Preferences > System > Clear Cache**
3. Reduce undo history depth in **Edit > Preferences > General**
4. Close and reopen large projects periodically

### Slow Startup

**Symptom**: Klipp takes a long time to start.

**Cause**: GPU initialization (Vulkan device creation, shader compilation) and CUDA detection happen at startup.

**Solution**: This is normal on first launch. Subsequent launches may be faster due to driver shader caching. If startup consistently exceeds 10 seconds, check the log file for errors during GPU initialization.

## Performance Tips

- Use CUDA NVDEC for all video decode on NVIDIA GPUs
- Set playback resolution to Half for 4K projects
- Keep frame queue at 16 (default) unless experiencing issues
- Use the Color Grading workspace only when grading (scopes add GPU load)
- Close unused panels to reduce UI rendering overhead
- Use background rendering for effects-heavy timelines
