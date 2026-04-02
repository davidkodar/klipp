# Supported Formats

Complete reference of all media formats supported by Klipp for import and export.

## Import: Video

Klipp uses ffmpeg-next for decoding and supports all major video formats:

| Format | Container | Notes |
|--------|-----------|-------|
| H.264 / AVC | MP4, MKV, MOV, AVI | Hardware decode via NVDEC |
| H.265 / HEVC | MP4, MKV, MOV | Hardware decode via NVDEC |
| ProRes | MOV | All profiles (422, 422HQ, 4444) |
| DNxHD / DNxHR | MKV, MOV | All profiles |
| VP9 | WebM, MKV | Software decode recommended on NVIDIA |
| AV1 | MP4, MKV, WebM | Software decode recommended on NVIDIA |
| MPEG-2 | MPEG-TS, MPG, VOB | Hardware decode via NVDEC |
| MPEG-4 Part 2 | AVI, MP4 | Legacy format |
| MJPEG | AVI, MOV | Common in cameras |
| FFV1 | MKV | Lossless archival |

## Import: Audio

| Format | Container | Notes |
|--------|-----------|-------|
| PCM | WAV, AIFF | 16/24/32-bit, up to 96kHz |
| FLAC | FLAC | Lossless |
| AAC | MP4, M4A | Common compressed format |
| MP3 | MP3 | Legacy compressed |
| Vorbis | OGG | Open compressed format |
| Opus | OGG, WebM | Modern compressed format |

## Import: Images

| Format | Notes |
|--------|-------|
| PNG | With alpha channel support |
| JPEG | Standard photographic format |
| TIFF | Including 16-bit |
| BMP | Uncompressed bitmap |
| WebP | Google web image format |
| OpenEXR | HDR floating-point images |

## Import: Subtitles

| Format | Notes |
|--------|-------|
| SRT | SubRip text, widely supported |
| VTT | WebVTT, web standard |

## Export: Video Codecs

| Codec | Containers | Profiles/Notes |
|-------|-----------|----------------|
| H.264 / AVC | MP4, MKV, MOV | Baseline, Main, High profiles; CRF or CBR |
| H.265 / HEVC | MP4, MKV, MOV | Main, Main10 profiles |
| ProRes 422 | MOV | Standard quality intermediate |
| ProRes 422 HQ | MOV | High bitrate intermediate |
| ProRes 4444 | MOV | With alpha channel |
| DNxHD | MOV, MKV | Avid-compatible intermediate |

## Export: Audio Codecs

| Codec | Containers | Notes |
|-------|-----------|-------|
| AAC | MP4, MOV, MKV | 128-320 kbps |
| MP3 | MP4, MKV | 128-320 kbps |
| FLAC | MKV | Lossless |

## Export: Interchange

| Format | Notes |
|--------|-------|
| FCPXML 1.9 | Final Cut Pro XML, timeline structure and clips |
| CMX 3600 EDL | Industry-standard edit decision list, single track |

## Hardware Decode Support

| Codec | CUDA NVDEC | VAAPI | Software |
|-------|-----------|-------|----------|
| H.264 | Yes | Yes | Yes |
| H.265 | Yes | Yes | Yes |
| VP9 | Yes | Broken on NVIDIA | Yes |
| AV1 | Yes (RTX 40+) | Broken on NVIDIA | Yes |
| MPEG-2 | Yes | Yes | Yes |
| ProRes | No | No | Yes |

VAAPI with nvidia-vaapi-driver produces all-zero frames for AV1 and VP9. Use CUDA NVDEC or software decode for these codecs on NVIDIA hardware.
