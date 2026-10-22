# Export Formats

Reference for the containers, video codecs and audio codecs Klipp exports. Klipp encodes through FFmpeg, so the encoders named below have to be in the local FFmpeg build.

## Containers

| Container | Extension | Video codecs | Audio codecs | Notes |
|---|---|---|---|---|
| MP4 | `.mp4` | H.264, H.265, AV1, VP9 | Any | The usual choice for web and general delivery. Chapters and soft subtitles (`mov_text`). |
| MKV | `.mkv` | H.264, H.265, AV1, VP9, ProRes, DNxHD | Any | Chapters and soft subtitles (SubRip). |
| MOV | `.mov` | H.264, H.265, ProRes, DNxHD | AAC, MP3, PCM | The usual home of ProRes and DNxHD. Chapters and soft subtitles (`mov_text`). |
| WebM | `.webm` | VP9, AV1 | Opus | Soft subtitles (WebVTT). No chapters. |
| GIF | `.gif` | GIF | — | Picture only: no audio, subtitles, chapters or HDR. |
| MP3 | `.mp3` | — | MP3 | The sound alone. |
| WAV | `.wav` | — | PCM | The sound alone, uncompressed. |
| M4A | `.m4a` | — | AAC | The sound alone. |

H.264 and H.265 exports can also carry the subtitle track as CEA-608 closed captions inside the video stream.

Every pair in this table is one FFmpeg writes. The audio menu lists only what the container holds. Changing the format replaces a codec it cannot hold with its default (H.264 and AAC, or VP9 and Opus for WebM), and picking a video codec the format cannot hold moves the export to that codec's container: MOV for ProRes and DNxHD, WebM for VP9, MP4 for AV1. A saved preset is fitted the same way when it is loaded, and a pair that still does not fit is named above the Export button, which stays disabled until it is changed.

## Video Codecs

| Codec | Encoder | Rate control | Bit depth | Hardware |
|---|---|---|---|---|
| H.264 | libx264 | CRF, CBR, VBR | 8 or 10 | NVENC, AMF, VAAPI (8-bit only) |
| H.265 | libx265 | CRF, CBR, VBR | 8 or 10 | NVENC, AMF, VAAPI |
| AV1 | SVT-AV1 (`libsvtav1`) | CRF, CBR, VBR | 8 or 10 | NVENC, AMF, VAAPI |
| VP9 | `libvpx-vp9` | CRF, CBR, VBR | 8 | — |
| ProRes 422 | `prores_ks`, profile 2 | fixed | 10, 4:2:2 | — |
| ProRes 422 HQ | `prores_ks`, profile 3 | fixed | 10, 4:2:2 | — |
| ProRes 4444 | `prores_ks`, profile 4 | fixed | 10, 4:4:4 | — |
| DNxHD 115 / 145 | `dnxhd` at 115 / 145 Mbps | fixed | 8, 4:2:2 | — |
| GIF | `gif` | — | 256-colour palette | — |

- **H.264** is the most widely playable. At 10 bits it becomes the High 10 profile, which only libx264 writes and many players cannot decode; use H.265 for HDR.
- **H.265** is what HDR10 is specified on, and encodes on the GPU at 10 bits.
- **AV1** compresses best, and takes 8 or 10 bits and HLG or HDR10 like H.265. It is slow in software; NVENC on RTX 40 cards and newer, and recent AMD and Intel GPUs, encode it in hardware.
- **VP9** is for web video in WebM. Klipp writes it in SDR only, at 8 bits.
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

Every export except GIF is tagged limited range, and its colour tags follow the delivery; an SDR export through the DCI-P3 D65 transform is tagged P3:

| Delivery | Primaries | Transfer | Matrix |
|---|---|---|---|
| SDR | BT.709 | BT.709 | BT.709 |
| SDR, DCI-P3 D65 | P3 D65 (SMPTE EG 432-1) | unspecified | BT.709 |
| SDR, Rec.2020 | BT.2020 | BT.709 | BT.2020 non-constant |
| HLG | BT.2020 | ARIB STD-B67 (HLG) | BT.2020 non-constant |
| HDR10 (PQ) | BT.2020 | SMPTE ST 2084 (PQ) | BT.2020 non-constant |

A PQ file also carries the mastering-display metadata and MaxCLL / MaxFALL, measured on the frames written; an H.265 PQ file carries HDR10+ dynamic metadata as well. An MP4 or M4A puts its index before its media, so it starts playing before it has downloaded.

The DCI-P3 D65 transform writes a pure 2.6 gamma, which has no transfer code (the SMPTE 428 code is for the XYZ encoding a DCP holds), so its transfer is left unspecified. Tell the receiving application the gamma when it asks.
