# System Preferences

**Preferences → System** controls the GPU, hardware decode and decoding threads. Some changes need a restart, and the page says so.

## GPU

- **GPU preference** — **Auto** (recommended), **Prefer discrete**, **Prefer integrated**, or **CPU only**. It affects effects, colour grading and decode. A GPU is required for the editor; **CPU only** is for diagnosis.
- **Hardware video decode** — NVDEC / VAAPI on or off. When on, Klipp tries CUDA NVDEC, then VAAPI, then software, and moves a stream to software when the GPU's decoder is full or refuses it.

## Threading

- **Decode threads** — the thread count per clip decoder; **Auto** lets FFmpeg choose.
- **Frame queue size** — decoded frames buffered ahead of playback. Larger is smoother and uses more memory. Klipp's decoders also keep to a budget of free memory and GPU memory, opening shorter queues when either is short.

## Playback Resolution

- **Max playback resolution** — the largest size frames are decoded to while playing. Exports always render at full resolution.

The playback quality (full, half, quarter) and background rendering are on the [Playback](./pages.md#playback) page; the caches on [Media & Cache](./pages.md#media--cache).
