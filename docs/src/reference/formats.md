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
| HEIC / HEIF, AVIF, JPEG XL | Formats phones and the web produce |
| Camera RAW stills | See [Camera RAW](#camera-raw) below |

## Import: Subtitles

| Format | Notes |
|--------|-------|
| SRT | SubRip text, widely supported |
| VTT | WebVTT, web standard |
| ASS / SSA | Styled subtitles |
| TTML, DFXP, IMSC, iTT | XML timed text |
| SCC | Scenarist, CEA-608 captions |
| EBU-STL | Broadcast subtitles |

Exports write the same formats, TTML as IMSC 1.1 Text.

## Import: Camera Log Encodings

Klipp decodes these camera log formats, each with the gamut it is decoded in:

| Format | Gamut |
|--------|-------|
| ARRI LogC3 | ARRI Wide Gamut 3 |
| ARRI LogC4 | ARRI Wide Gamut 4 |
| Sony S-Log2 | S-Gamut3 |
| Sony S-Log3 | S-Gamut3.Cine |
| Canon Log | Cinema Gamut |
| Canon Log 3 | Cinema Gamut |
| Panasonic V-Log | V-Gamut |
| Nikon N-Log | BT.2020 |
| Fuji F-Log | BT.2020 |
| DJI D-Log | BT.709 |
| GoPro ProTune Flat | BT.709 |
| Apple Log | BT.2020 |
| Leica L-Log | BT.2020 (BT.709 for footage from the SL, Typ 601, which records it) |
| RED Log3G10 | REDWideGamutRGB |
| BMD Film Gen 5 | Blackmagic Wide Gamut |

HLG and PQ sources are read from the file's own tags. See [Source Colourspace & Log](../color/source-colorspace.md) for detection and the per-clip override.

## Camera RAW

Klipp decodes video through FFmpeg, and does not support proprietary camera RAW video: REDCODE (`.r3d`), ARRIRAW (`.ari`, or ARRIRAW in MXF) and Blackmagic RAW (`.braw`). Each needs its maker's closed SDK to decode and its own colour science to develop, and Klipp does not link those SDKs. `.r3d`, `.ari` and `.braw` are not in Klipp's list of media files. This is a deliberate choice, not a planned feature.

Transcode camera RAW to an editing codec first, in the maker's free tool — REDCINE-X PRO for RED, ARRI Reference Tool for ARRI, Blackmagic's own free software for Blackmagic RAW — to ProRes (422 HQ or 4444) or DNxHR, and import the result. Keep a log encoding if the tool offers one, so the latitude is there to grade in Klipp, and set the clip's log format if Klipp does not detect it. The professional codecs cameras record directly (ProRes, DNxHD / DNxHR, XAVC and XDCAM in MXF, H.264 and H.265) are decoded by FFmpeg as usual.

Still-image RAW files (`.arw`, `.cr2`, `.cr3`, `.nef`, `.raf`, `.orf`, `.rw2`, `.dng`, `.pef`, `.srw`, `.rwl`) import as stills, but Klipp does not develop the sensor data. The file browser's thumbnail is the JPEG preview the camera embedded in the file, with the camera's white balance and processing baked in. The picture on the timeline is decoded by FFmpeg like any other still, and FFmpeg cannot decode the compression many cameras use. For dependable results, convert RAW photos to TIFF or JPEG in a RAW developer first.

## Export: Video Codecs

| Codec | Containers | Hardware encode | Notes |
|-------|-----------|------|----------------|
| H.264 / AVC | MP4, MKV, MOV | NVENC, AMF, VAAPI (8-bit) | 8- or 10-bit; CRF, CBR or VBR |
| H.265 / HEVC | MP4, MKV, MOV | NVENC, AMF, VAAPI | 8- or 10-bit; HLG and HDR10 with mastering metadata |
| AV1 | MP4, MKV, WebM | NVENC, AMF, VAAPI | SVT-AV1 in software |
| VP9 | WebM, MKV, MP4 | no | libvpx-vp9 |
| ProRes 422 / 422 HQ / 4444 | MOV, MKV | no | 10-bit; HLG and HDR10 |
| DNxHD 115 / 145 | MOV, MKV | no | 8-bit 4:2:2 |
| GIF | GIF | no | 256-colour palette, no audio |

## Export: Audio Codecs

AAC, MP3, Opus (128–320 kbps), FLAC, PCM 16-bit and PCM 24-bit, or no audio; MOV holds no Opus or FLAC, and WebM only Opus. The mix is stereo at 48 kHz. The sound alone exports as MP3, WAV or M4A. See [Export Formats](../export/formats.md).

## Export: Output Transforms

Chosen in the export settings:

- Match Viewer (default)
- Rec.709 SDR
- Rec.2020 SDR
- Rec.2100 PQ (1000 nit)
- Rec.2100 PQ (4000 nit)
- DCI-P3 D65 (48 nit)
- sRGB curve only (legacy)

HDR delivery itself (HLG or HDR10) is chosen separately; see [Exporting Projects](../export/exporting.md#output-transform).

## Export: Interchange

| Format | Import | Export |
|--------|--------|--------|
| OpenTimelineIO (`.otio`) | yes | yes |
| Kdenlive / MLT (`.kdenlive`) | yes | yes |
| FCPXML 1.9 | yes (storyline and connected clips) | yes (first video track only) |
| Final Cut Pro 7 XML (xmeml) | yes | no |
| CMX 3600 EDL | yes (cuts) | yes (all video and audio tracks) |
| AAF, through OpenTimelineIO | yes | yes |

See [Timeline Interchange](../export/interchange.md) for what each carries.

## Not Supported

- **DCP** — export a master and wrap it with DCP-o-matic; see [Cinema (DCP) Delivery](../export/exporting.md#cinema-dcp-delivery).
- **IMF** — not made by Klipp; see [IMF Packages](../export/exporting.md#imf-packages).
- **Camera RAW video** — see [Camera RAW](#camera-raw).

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
