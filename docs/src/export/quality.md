# Export Quality

Every built-in export preset's output is measured, not assumed. This page is the latest measurement: 2026-10-13, ffmpeg n9.0.1, VMAF model v0.6.1 (the 4K model at 2160p).

## What is measured

A fixed test project, made from generated sources with no footage: a moving test chart, a one-second cross-dissolve into a 16-bit horizontal ramp, and a title over both, 8 seconds at 1080p25. It's exported through every built-in preset, and through the delivery choices no preset covers (10-bit H.264, H.265 and AV1, ProRes 422 HQ and 4444, PQ and HLG). Each file is checked against the points below.

- **Quality**: VMAF (mean and 1st percentile over frames) and luma PSNR against a master of the same project at the file's size. The master is Klipp's own export as H.264 at crf 0: lossless at 8 bits, and near-lossless at 10 bits for the 10-bit files. Both are compared as 10-bit 4:2:0. The score is the codec's loss on Klipp's pictures; the checks below catch errors the master would share.
- **Floor**: the VMAF mean and 1st percentile each delivery must clear, set before measuring: masters 98 / 95, high-rate web 95 / 90, standard web 93 / 85, social and the extra cases 90 / 80.
- **Ramp levels**: distinct luma values across the ramp. About 220 at 8 bits and about 870 at 10 bits; an 8-bit step anywhere in a 10-bit file would cut it to about a quarter.
- **Chroma, range and tags**: the pixel format's subsampling, and the primaries, transfer and matrix the stream carries. These agree with the picture's own frame tags; ProRes carries them in every frame header.
- **PQ light**: a second project of four patches at 5, 100, 203 and 1000 cd/m², read back from the file's codes through the PQ curve.

The **HEVC Main10 master** preset was added after this measurement and is not in the table; the H.265 10-bit row below is the same codec and depth.

The Animated GIF preset is a palette at its own rate (15 fps), so it is checked for its size, rate and frame count rather than scored. "Audio Only MP3" writes the sound alone: it is checked for one MP3 stream and no video stream, and its loudness is held by the audio export's tests.

## Results

| Export | Codec | Size | Pixel format | Chroma | Range | Primaries / transfer / matrix | Ramp levels | VMAF mean / p1 | PSNR Y | Floor | |
|---|---|---|---|---|---|---|---|---|---|---|---|
| YouTube 1080p | h264 | 1920x1080 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 222 | 98.5 / 97.3 | 57.3 | 93.0 / 85.0 | pass |
| YouTube 4K | hevc | 3840x2160 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 218 | 99.7 / 98.2 | 51.9 | 93.0 / 85.0 | pass |
| Vimeo 1080p | h264 | 1920x1080 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 218 | 98.6 / 97.3 | 59.0 | 95.0 / 90.0 | pass |
| Twitter/X | h264 | 1280x720 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 220 | 98.3 / 96.9 | 52.0 | 90.0 / 80.0 | pass |
| Instagram Square | h264 | 1080x1080 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 222 | 98.2 / 97.3 | 58.6 | 90.0 / 80.0 | pass |
| Instagram Vertical | h264 | 1080x1920 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 220 | 97.9 / 97.3 | 62.0 | 90.0 / 80.0 | pass |
| ProRes 422 | prores | 1920x1080 | yuv422p10le | 4:2:2 | tv | bt709 / bt709 / bt709 | 873 | 98.3 / 97.3 | 55.2 | 98.0 / 95.0 | pass |
| DNxHD 115 | dnxhd | 1920x1080 | yuv422p | 4:2:2 | tv | bt709 / bt709 / bt709 | 218 | 98.7 / 97.3 | 57.6 | 98.0 / 95.0 | pass |
| H.265 4K | hevc | 3840x2160 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 218 | 99.7 / 98.4 | 52.0 | 93.0 / 85.0 | pass |
| H.265 HQ (GPU) | hevc | 1920x1080 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 218 | 98.3 / 97.1 | 54.3 | 95.0 / 90.0 | pass |
| Audio Only MP3 | mp3 | – | – | – | – | – | – | – | – | – | pass (audio only, no video stream) |
| Broadcast master (EBU R128) | prores | 1920x1080 | yuv422p10le | 4:2:2 | tv | bt709 / bt709 / bt709 | 873 | 98.3 / 97.3 | 55.2 | 98.0 / 95.0 | pass |
| Web 1080p (-14 LUFS) | h264 | 1920x1080 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 222 | 98.5 / 97.3 | 57.3 | 93.0 / 85.0 | pass |
| Animated GIF (480p, 15fps) | gif | 854x480 | bgra | ? | – | – / – / – | – | – | – | – | pass (120 frames at 15/1) |
| H.264 10-bit | h264 | 1920x1080 | yuv420p10le | 4:2:0 | tv | bt709 / bt709 / bt709 | 873 | 98.4 / 97.2 | 54.4 | 90.0 / 80.0 | pass |
| H.265 10-bit | hevc | 1920x1080 | yuv420p10le | 4:2:0 | tv | bt709 / bt709 / bt709 | 858 | 98.3 / 96.9 | 53.0 | 90.0 / 80.0 | pass |
| AV1 10-bit | av1 | 1920x1080 | yuv420p10le | 4:2:0 | tv | bt709 / bt709 / bt709 | 878 | 98.8 / 97.2 | 63.6 | 90.0 / 80.0 | pass |
| ProRes 422 HQ | prores | 1920x1080 | yuv422p10le | 4:2:2 | tv | bt709 / bt709 / bt709 | 873 | 98.3 / 97.3 | 55.2 | 98.0 / 95.0 | pass |
| ProRes 4444 | prores | 1920x1080 | yuv444p12le | 4:4:4 | tv | bt709 / bt709 / bt709 | 1340 | 98.3 / 97.3 | 53.8 | 98.0 / 95.0 | pass |
| H.264 8-bit (control) | h264 | 1920x1080 | yuv420p | 4:2:0 | tv | bt709 / bt709 / bt709 | 218 | 98.6 / 97.3 | 59.0 | 90.0 / 80.0 | pass |
| H.265 PQ | hevc | 1920x1080 | yuv420p10le | 4:2:0 | tv | bt2020 / smpte2084 / bt2020nc | – | – | – | – | PQ patches 5, 100, 204, 1004 cd/m² |
| H.265 HLG | hevc | 1920x1080 | yuv420p10le | 4:2:0 | tv | bt2020 / arib-std-b67 / bt2020nc | – | – | – | – |  |
| AV1 PQ | av1 | 1920x1080 | yuv420p10le | 4:2:0 | tv | bt2020 / smpte2084 / bt2020nc | – | – | – | – | PQ patches 5, 100, 204, 1004 cd/m² |
| ProRes 4444 PQ | prores | 1920x1080 | yuv444p12le | 4:4:4 | tv | bt2020 / smpte2084 / bt2020nc | – | – | – | – | PQ patches 5, 100, 204, 1004 cd/m² |

## Running it again

```text
cargo run --release -p klipp-render --example preset_quality -- <out dir> [<table.md>]
```

It writes every export, the masters, `results.json` and the table to `<out dir>` (several GB at 4K), and needs ffmpeg with libvmaf and the VMAF models in `/usr/share/model`.
