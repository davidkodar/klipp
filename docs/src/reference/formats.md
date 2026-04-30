# Supported Formats

Complete reference of all media formats supported by Klipp for import and export.

## Import: Video

Klipp uses ffmpeg-next for decoding and supports all major video formats:

| Format | Container | Notes |
|--------|-----------|-------|
| H.264 / AVC | MP4, MKV, MOV, AVI, MXF | Hardware decode via NVDEC (cuvid) |
| H.265 / HEVC | MP4, MKV, MOV, MXF | Hardware decode via NVDEC; supports HDR10 / HLG metadata |
| ProRes | MOV | All profiles (422, 422HQ, 4444); software decode |
| DNxHD / DNxHR | MKV, MOV | All profiles; software decode |
| VP9 | WebM, MKV | Software decode recommended on NVIDIA |
| AV1 | MP4, MKV, WebM | Software decode recommended on NVIDIA |
| MPEG-2 | MPEG-TS, MPG, VOB | NVDEC supported |
| MPEG-4 Part 2 | AVI, MP4 | Legacy |
| MJPEG | AVI, MOV | Common in cameras |
| FFV1 | MKV | Lossless archival |

## Import: Audio

| Format | Container | Notes |
|--------|-----------|-------|
| PCM | WAV, AIFF | 16/24/32-bit, up to 96 kHz |
| FLAC | FLAC, MKV | Lossless |
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

## Import: Camera Log Encodings

Auto-detected; user-overridable per clip:

- Sony S-Log3 (S-Gamut3.Cine)
- Canon Log 3 (Cinema Gamut)
- Panasonic V-Log (V-Gamut)
- ARRI Log-C 3 (Wide Gamut)
- Apple Log (Wide-DCI)
- RED Log3G10 (RED WideGamut RGB)
- DJI D-Log (DJI gamut)
- Z-CAM Z-Log2 (Z-CAM gamut)

See [Source Colourspace & Log](../color/source-colorspace.md) for the detection chain.

## Export: Video Codecs

| Codec | Containers | NVENC | Notes |
|-------|-----------|------|----------------|
| H.264 / AVC | MP4, MKV, MOV | yes | Baseline, Main, High profiles; CRF or CBR |
| H.265 / HEVC | MP4, MKV, MOV | yes | Main, Main10 profiles; HDR10 metadata supported |
| ProRes 422 | MOV | no | Standard quality intermediate |
| ProRes 422 HQ | MOV | no | High bitrate intermediate |
| ProRes 4444 | MOV | no | With alpha channel |
| DNxHD | MOV, MKV | no | Avid-compatible intermediate |

## Export: Audio Codecs

| Codec | Containers | Notes |
|-------|-----------|-------|
| AAC | MP4, MOV, MKV | 128–320 kbps |
| MP3 | MP4, MKV | 128–320 kbps |
| FLAC | MKV | Lossless |
| PCM | MOV, WAV | Uncompressed |

## Export: Output Display Transforms

ODT applied at export — converts working ACEScg to the chosen delivery space:

- sRGB
- Rec.709
- Rec.2020 (with HDR10 / HLG metadata)
- P3-D65

## Export: Interchange

| Format | Notes |
|--------|-------|
| FCPXML 1.9 | Final Cut Pro XML; timeline structure, clips, basic transitions, markers |
| CMX 3600 EDL | Industry-standard edit decision list, single track |

## Hardware Decode Support Matrix

| Codec | CUDA NVDEC | VAAPI | Software |
|-------|-----------|-------|----------|
| H.264 | Yes | Yes | Yes |
| H.265 | Yes | Yes | Yes |
| VP9 | Yes | **Broken on nvidia-vaapi-driver** | Yes |
| AV1 | Yes (RTX 40+) | **Broken on nvidia-vaapi-driver** | Yes |
| MPEG-2 | Yes | Yes | Yes |
| ProRes | No (intra-only, limited NVDEC support) | No | Yes |
| DNxHD | No | No | Yes |

VAAPI with `nvidia-vaapi-driver` produces all-zero frames for AV1 and VP9. Use CUDA NVDEC or software decode for these codecs on NVIDIA hardware.
