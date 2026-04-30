# Exporting Projects

Export your finished project from **File → Export** (**Ctrl+E**) to open the export dialog.

## Export Dialog

The export dialog presents all rendering options in a single window:

### Preset Selection
Built-in presets for common delivery targets:

- **YouTube 1080p** — H.264, 1920×1080, AAC audio
- **YouTube 4K** — H.265, 3840×2160, AAC audio
- **Broadcast ProRes** — ProRes 422, source resolution, PCM audio
- **Archive Master** — ProRes 4444, source resolution, FLAC audio
- **Web MP4** — H.264, optimised for small file size
- **Custom** — Configure all settings manually

User-defined presets land in `~/.config/klipp/export-presets/` and appear alongside the built-ins.

### Resolution
- Match source resolution (default)
- Standard resolutions: 3840×2160, 1920×1080, 1280×720
- Custom width and height with locked or unlocked aspect ratio

### Frame Rate
- Match source frame rate (default)
- Standard rates: 23.976, 24, 25, 29.97, 30, 50, 59.94, 60 fps

### Video Codec
Select the output video codec — see [Export Formats](./formats.md) for full details:

- **H.264** — Universal compatibility
- **H.265 / HEVC** — Better compression than H.264 at the same quality, slower
- **ProRes 422** / **422 HQ** / **4444** — Post-production intermediate
- **DNxHD** — Avid-compatible

### Quality
- **Bitrate mode** — Constant bitrate (CBR) or variable bitrate (VBR)
- **Quality slider** — CRF / CQP value for rate-controlled encoding
- **Bitrate** — Target bitrate in Mbps for CBR mode

### Audio Codec
- **AAC** — Standard compressed audio
- **MP3** — Legacy compressed audio
- **FLAC** — Lossless compression
- **PCM** — Uncompressed (MOV / WAV)

### Container
- **MP4** — Universal playback support
- **MKV** — Flexible container, supports all codecs
- **MOV** — Apple ecosystem, required for ProRes

### Output Display Transform (ODT)

Pick the colour space of the delivery file:

- **sRGB** — Web, YouTube, most monitors (default)
- **Rec.709** — Broadcast HD
- **Rec.2020** — HDR streaming, UHD
- **P3-D65** — Cinema, Apple displays

ODT is per-export — the same project can be rendered to multiple deliveries without re-grading. See [Working Colorspaces](../color/working-colorspaces.md) for the full chain.

### Per-Clip ODT Override

For split-delivery workflows, individual clips can override the project's ODT — useful when a clip should retain its source colourspace (a UI screencap inside a film-grade timeline, for instance). The override is set in the inspector's Video tab.

## Render Queue

For batch exports, add multiple export jobs to the render queue. Each job can have different settings. The queue processes sequentially.

## Export Progress

During export, a progress bar shows:

- Current frame / total frames
- Elapsed time
- Estimated time remaining
- A cancel button to abort

Export runs on a background job — you can keep editing while a render is in progress.

## Hardware Acceleration

For H.264 and H.265 encoding, Klipp uses NVENC on NVIDIA GPUs when available. NVENC encoding is roughly 5–10× faster than the libx264 / libx265 software encoders at comparable quality. The export dialog labels the codec choice with `(NVENC)` when GPU encoding is in use.

For ProRes and DNxHD, only software encoding is available — these codecs have no GPU-accelerated encoder in the FFmpeg ecosystem.
