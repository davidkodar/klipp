# Exporting

Export your finished project from **File > Export** (or **Ctrl+E**) to open the export dialog.

## Export Dialog

The export dialog presents all rendering options in a single window:

### Preset Selection
Choose from built-in presets for common delivery targets:

- **YouTube 1080p** -- H.264, 1920x1080, AAC audio
- **YouTube 4K** -- H.264, 3840x2160, AAC audio
- **Broadcast ProRes** -- ProRes 422, source resolution, PCM audio
- **Archive Master** -- ProRes 4444, source resolution, FLAC audio
- **Web MP4** -- H.264, optimized for small file size
- **Custom** -- Configure all settings manually

### Resolution
- Match source resolution (default)
- Standard resolutions: 3840x2160, 1920x1080, 1280x720
- Custom width and height with locked or unlocked aspect ratio

### Frame Rate
- Match source frame rate (default)
- Standard rates: 23.976, 24, 25, 29.97, 30, 50, 59.94, 60 fps

### Video Codec
Select the output video codec:

- **H.264** -- Universal compatibility, good compression
- **H.265/HEVC** -- Better compression than H.264 at the same quality, slower to encode
- **ProRes 422** -- High quality, large files, industry standard for post-production
- **ProRes 422 HQ** -- Higher bitrate ProRes for demanding workflows
- **ProRes 4444** -- Includes alpha channel support
- **DNxHD** -- Avid-compatible intermediate codec

### Quality
- **Bitrate mode**: Constant bitrate (CBR) or variable bitrate (VBR)
- **Quality slider**: CRF/CQP value for rate-controlled encoding
- **Bitrate**: Target bitrate in Mbps for CBR mode

### Audio Codec
- **AAC** -- Standard compressed audio, good compatibility
- **MP3** -- Legacy compressed audio
- **FLAC** -- Lossless compression, larger files

### Container
- **MP4** -- Universal playback support
- **MKV** -- Flexible container, supports all codecs
- **MOV** -- Apple ecosystem, required for ProRes

## Render Queue

For batch exports, add multiple export jobs to the render queue. Each job can have different settings. The queue processes jobs sequentially.

## Export Progress

During export, a progress bar shows:
- Current frame / total frames
- Elapsed time
- Estimated time remaining
- Cancel button to abort the export
