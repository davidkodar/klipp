# Importing Media

Klipp supports importing video, audio, and image files into your project's media pool.

## Import Methods

### File Menu
Go to **File > Import Media** (or press **Ctrl+I**) to open the native file picker. Select one or more files to import. You can select multiple files by holding Shift or Ctrl.

### Drag and Drop
Drag files from your file manager directly into the media browser panel. Klipp accepts files dropped anywhere in the browser area.

## Supported Video Formats

Klipp uses ffmpeg-next for decoding and supports all formats that FFmpeg handles:

- **Containers**: MP4, MKV, MOV, AVI, WebM, MPEG-TS
- **Video codecs**: H.264/AVC, H.265/HEVC, ProRes, DNxHD, VP9, AV1, MPEG-2, MJPEG
- **Frame rates**: Any frame rate from 23.976 to 120 fps

Hardware-accelerated decode via CUDA NVDEC is used automatically for supported codecs on NVIDIA GPUs. VAAPI decode is available as a fallback (note: VAAPI is unreliable for AV1 and VP9 on NVIDIA with the nvidia-vaapi-driver).

## Supported Audio Formats

- **Containers**: WAV, FLAC, MP3, AAC, OGG, AIFF
- **Codecs**: PCM (16/24/32-bit), FLAC, MP3, AAC, Vorbis, Opus
- **Sample rates**: 44.1 kHz, 48 kHz, 96 kHz, and others

## Supported Image Formats

- PNG, JPEG, TIFF, BMP, WebP, OpenEXR
- Images are imported as still clips with a configurable default duration

## Image Sequences

Import numbered image sequences (e.g., `frame_0001.png`, `frame_0002.png`) as a single video clip. Select the first image in the sequence, and Klipp detects the remaining frames automatically based on the numbering pattern.

## Subtitles

Klipp can import subtitle files:

- **SRT** (SubRip) -- Standard timed text format
- **VTT** (WebVTT) -- Web-compatible subtitle format

Imported subtitles appear as clips on a subtitle track.

## Media Management

Klipp references media files by their original file paths. The project does not copy media files into the project directory. If you move or rename source files, Klipp will prompt you to relink them.

## After Import

Imported media appears in the media browser. From there you can:

- Hover to skim through clips
- Double-click to preview in the viewer
- Drag to the timeline or use **W**/**D** to insert/overwrite
- Organize into bins
