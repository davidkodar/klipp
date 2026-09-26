# Export Formats

Reference for the containers, video codecs and audio codecs Klipp exports. Klipp encodes through FFmpeg, so the encoders named below have to be in the local FFmpeg build.

## Containers

| Container | Extension | Video codecs | Audio codecs | Notes |
|---|---|---|---|---|
| MP4 | `.mp4` | H.264, H.265, AV1 | Any | The usual choice for web and general delivery. Chapters and soft subtitles (`mov_text`). |
| MKV | `.mkv` | H.264, H.265, AV1, VP9 | Any | Chapters and soft subtitles (SubRip). |
| MOV | `.mov` | H.264, H.265, ProRes, DNxHD | AAC, MP3, PCM | Required for ProRes and DNxHD. Chapters and soft subtitles (`mov_text`). |
| WebM | `.webm` | VP9, AV1 | Opus | Soft subtitles (WebVTT). No chapters. |
| GIF | `.gif` | GIF | — | Picture only: no audio, subtitles, chapters or HDR. |

The export dialog only offers pairs the container can hold. Changing the format replaces a codec it cannot hold with its default (H.264 and AAC, or VP9 and Opus for WebM), and picking a codec the format cannot hold moves the export to that codec's container: MOV for ProRes and DNxHD, WebM for VP9, MP4 for AV1.

## Video Codecs

| Codec | Encoder | Rate control | Bit depth | Hardware |
|---|---|---|---|---|
| H.264 | libx264 | CRF, CBR, VBR | 8 or 10 | NVENC, AMF (8-bit only) |
| H.265 | libx265 | CRF, CBR, VBR | 8 or 10 | NVENC, AMF |
| AV1 | SVT-AV1 (`libsvtav1`) | CRF, CBR, VBR | 8 | NVENC |
| VP9 | `libvpx-vp9` | CRF, CBR, VBR | 8 | — |
| ProRes 422 | `prores_ks`, profile 2 | fixed | 10, 4:2:2 | — |
| ProRes 422 HQ | `prores_ks`, profile 3 | fixed | 10, 4:2:2 | — |
| ProRes 4444 | `prores_ks`, profile 4 | fixed | 10, 4:4:4 | — |
| DNxHD 115 / 145 | `dnxhd` at 115 / 145 Mbps | fixed | 8, 4:2:2 | — |
| GIF | `gif` | — | 256-colour palette | — |

- **H.264** is the most widely playable. At 10 bits it becomes the High 10 profile, which only libx264 writes and many players cannot decode; use H.265 for HDR.
- **H.265** is what HDR10 is specified on, and encodes on the GPU at 10 bits.
- **AV1** compresses best and is slow in software.
- **VP9** is for web video in WebM. VP9 HDR has thin player support, so the export dialog suggests AV1 for an HDR VP9 delivery.
- **ProRes** is the mastering and interchange format. ProRes 4444 is written as 4:4:4 Y'CbCr; Klipp writes no alpha channel.
- **DNxHD** at these two bitrates is 8-bit and cannot carry HDR; ProRes 422 HQ is the 10-bit equivalent.
- **GIF** is made in two passes: a palette is built from the whole clip, then each frame is mapped to it. It loops forever.

See [Exporting Projects](./exporting.md#hardware-encoding) for how the hardware encoder is chosen and what happens when it cannot open.

## Audio Codecs

| Codec | Encoder | Bitrate |
|---|---|---|
| AAC | `aac` | 128–320 kbps |
| MP3 | `libmp3lame` | 128–320 kbps |
| Opus | `libopus` | 128–320 kbps |
| FLAC | `flac` | lossless |
| PCM 16-bit | `pcm_s16le` | uncompressed |
| PCM 24-bit | `pcm_s24le` | uncompressed |
| No audio | — | no audio stream is written |

The mix is written in stereo at 48 kHz.

## Colour Tags

Every export except GIF is tagged limited range, and its colour tags follow the delivery and, for SDR, the output transform:

| Delivery | Primaries | Transfer | Matrix |
|---|---|---|---|
| SDR | BT.709 | BT.709 | BT.709 |
| SDR, sRGB curve only | BT.709 | sRGB (IEC 61966-2-1) | BT.709 |
| SDR, DCI-P3 D65 | P3 D65 (SMPTE EG 432-1) | unspecified | BT.709 |
| HLG | BT.2020 | ARIB STD-B67 (HLG) | BT.2020 non-constant |
| HDR10 (PQ) | BT.2020 | SMPTE ST 2084 (PQ) | BT.2020 non-constant |

A PQ file also carries the mastering-display metadata and MaxCLL / MaxFALL, measured on the frames written.

The DCI-P3 D65 transform writes a pure 2.6 gamma, which has no transfer code (the SMPTE 428 code is for the XYZ encoding a DCP holds), so its transfer is left unspecified. Tell the receiving application the gamma when it asks.
