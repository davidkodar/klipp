# Export Formats

Detailed reference for all supported export containers, video codecs, and audio codecs.

## Containers

### MP4 (MPEG-4 Part 14)
The most widely supported container. Compatible with virtually all media players, web browsers, and devices. Supports H.264, H.265, and AAC audio. Use MP4 for web delivery, YouTube, and general distribution.

### MKV (Matroska)
A flexible open-source container that supports nearly all video and audio codecs. MKV handles H.264, H.265, ProRes, DNxHD, FLAC, and many more. Ideal for archival and when codec flexibility is needed. Some devices and platforms have limited MKV support.

### MOV (QuickTime)
Apple's container format, required for ProRes output. Also supports H.264, H.265, and DNxHD. Use MOV when delivering ProRes masters or working with Apple-based post-production pipelines.

## Video Codecs

| Codec | Use case | Quality | Speed | Size | Container |
|---|---|---|---|---|---|
| H.264 / AVC | Universal delivery, streaming, web | Good at moderate bitrates | Fast (NVENC: very fast) | Moderate | MP4, MKV, MOV |
| H.265 / HEVC | 4K delivery, bandwidth-constrained | Better than H.264 (~30–50% smaller) | Slower than H.264 (NVENC: still fast) | Small | MP4, MKV, MOV |
| ProRes 422 | Post-production intermediate | Visually lossless | Fast | Large (~10× H.264) | MOV |
| ProRes 422 HQ | Broadcast delivery | Higher bitrate than 422 | Fast | Very large | MOV |
| ProRes 4444 | VFX, alpha workflows | Near-lossless + alpha | Fast | Very large | MOV |
| DNxHD | Avid interchange, broadcast | Visually lossless | Fast | Large | MOV, MKV |

### NVENC vs Software

For H.264 and H.265, Klipp picks NVENC over libx264/libx265 when an NVIDIA GPU is present and `nvenc` is available in the local FFmpeg build. NVENC trade-offs:

- **Faster** — typically 5–10× the encoding speed of CPU encoders
- **Lower CPU use** — lets you do other work while the export runs
- **Slightly lower compression efficiency** — at the same target quality, NVENC files are typically 5–15% larger than libx264/libx265 equivalents

If file size matters more than speed (archival, bandwidth-capped delivery), tick **Force software encoder** in the export dialog.

## Audio Codecs

| Codec | Use case | Quality | Compatibility | Container |
|---|---|---|---|---|
| AAC | Standard delivery | Good at 128–256 kbps | Universal | MP4, MOV, MKV |
| MP3 | Legacy compatibility | Acceptable at 192–320 kbps | Universal | MP4, MKV |
| FLAC | Archival, lossless | Perfect (lossless) | Modern players | MKV |
| PCM | Uncompressed | Bit-perfect | Universal | MOV, WAV |

## Bit Depth and Colour

| Codec | Bit depth | Colour space awareness |
|---|---|---|
| H.264 | 8-bit (10-bit possible with libx264 high10 profile) | Tagged in stream header |
| H.265 | 8-bit or 10-bit | Tagged + HDR metadata for HDR10 / HLG |
| ProRes 422 | 10-bit | YCbCr 4:2:2 |
| ProRes 4444 | 12-bit | RGBA |
| DNxHD | 8-bit (DNxHR HQX 10-bit) | YCbCr 4:2:2 |

Klipp tags exports with the configured ODT colour primaries and transfer characteristics so receiving applications interpret them correctly.

## Frame Rate Round-Trip

When the export frame rate differs from the timeline frame rate, Klipp uses rational-time interpolation rather than nearest-frame snapping. A 23.976 timeline rendered to 30 fps is doubled-and-decimated using exact rational arithmetic, so cumulative drift over a long export is zero.
