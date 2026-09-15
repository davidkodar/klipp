# Importing Media

Klipp supports importing video, audio, and image files into your project's media pool.

## Import Methods

### File Menu
Go to **File → Import Media** (or press **Ctrl+I**) to open the native file picker (`rfd`-backed). Select one or more files. Hold **Shift** or **Ctrl** to select multiple.

### Drag and Drop
Drag files from your file manager directly into the media browser panel. Klipp accepts files dropped anywhere in the browser area. Dropping a folder imports recursively.

## Supported Video Formats

Klipp uses ffmpeg-next for decoding and supports all formats that FFmpeg handles:

- **Containers** — MP4, MKV, MOV, AVI, WebM, MPEG-TS, MXF
- **Video codecs** — H.264/AVC, H.265/HEVC, ProRes, DNxHD, VP9, AV1, MPEG-2, MJPEG
- **Frame rates** — Any frame rate; rational-time tracking preserves drop-frame and fractional rates exactly

Hardware-accelerated decode via CUDA NVDEC (cuvid path) is used automatically for supported codecs on NVIDIA GPUs. NVDEC pulls Annex B bitstreams via auto-attached `*_mp4toannexb` BSFs and resizes on the GPU side to a 1080p cap during playback. Software fallback uses thread-count auto-detection.

VAAPI is currently disabled by default — `nvidia-vaapi-driver` returns all-zero frames for AV1 and VP9, which is unrecoverable.

## Supported Audio Formats

- **Containers** — WAV, FLAC, MP3, AAC, OGG, AIFF
- **Codecs** — PCM (16/24/32-bit), FLAC, MP3, AAC, Vorbis, Opus
- **Sample rates** — 44.1 kHz, 48 kHz, 96 kHz, and others

Audio is resampled to the project sample rate on the fly with a recreate-on-seek pattern that avoids stale-buffer artefacts.

## Supported Image Formats

- PNG, JPEG, TIFF, BMP, WebP, OpenEXR
- Images are imported as still clips with a configurable default duration

## Image Sequences

Import numbered image sequences (e.g., `frame_0001.png`, `frame_0002.png`) as a single video clip. Select the first image in the sequence; Klipp detects the rest automatically based on the numbering pattern.

## Camera Log Footage

Klipp auto-detects log-encoded source material at import time using a four-layer chain:

1. Stream-level metadata (Sony writes the `encoder` tag on the stream, not the format)
2. Encoder string heuristics for tagless files (`HEVC Coding` → SLog3, etc.)
3. IDT colour transform splice
4. User override stored in the project on save

Detected log curves include Sony S-Log3, Canon Log, Panasonic V-Log, ARRI Log-C, Apple Log, RED Log3G10. See [Source Colourspace & Log](../color/source-colorspace.md) for the full table and override mechanics.

## What Klipp reads about a shot

Import records what the camera wrote about the shot, and the Info tab and clip properties show it. Three sources are read, and a later one fills only what an earlier one left blank:

1. **The container and its streams.** Every tag is kept, as written, under the **All metadata** header, and among them the camera make and model, the encoder, the creation date, the **start timecode** (a QuickTime timecode track, an MXF material package, a Matroska tag) and the **reel name**.
2. **A still's EXIF.** Photos show their camera, lens, ISO, shutter, aperture, focal length, white balance and the time they were taken.
3. **The camera's sidecar.** Sony XAVC clips come with a `C0001M01.XML` beside `C0001.MP4`, and Canon XF clips with a `.XML` of the same name; both hold what the container does not: the lens and exposure, the **gamma and gamut** the clip was recorded in (`s-log3-cine` / `s-gamut3-cine`), and the start timecode. Keep the sidecar next to the clip when you copy a card and Klipp reads it at import.

Cameras that write exposure only into their own container boxes, or into proprietary raw headers, are not read yet: their clips show what the container tags carry.

A project saved before Klipp kept this metadata reads it the first time the project opens, one file at a time in the background, and saves it with the project. **Refresh Metadata** in a clip's right-click menu reads a file again at any time, for instance after copying its sidecar next to it.

## Camera cards

Importing a folder that is a camera card — Sony XAVC or XDCAM, AVCHD, Canon XF, Panasonic P2, or a DCIM card — brings in the clips and leaves the proxies, thumbnails and index files beside them where they are. A take the camera split at its file-size limit is recognised from its numbered parts, where each part's timecode picks up where the last ended or the earlier part is a full-size file; each part then says which it is of how many, and **Add All Parts to Timeline** in its right-click menu lays them back to back.

## Metadata sheets

**File > Export Metadata Sheet** writes every clip's logged fields and what the camera recorded as CSV, for a spreadsheet, or ALE, for tools that read Avid Log Exchange. **File > Import Metadata Sheet** reads either back: rows are matched to clips by file name, in any case and with or without the extension, and the editor's fields are taken — scene, shot, take, description, comments, keywords, good take, reel, rating and tags. A column the sheet does not have is left alone.

## Files cut short

A copy interrupted mid-transfer keeps its header, which describes the whole recording, while its picture data stops early. Import checks for this: a clip that holds less than its header claims is imported at the length it really holds, a message says so, and the Info tab shows **Cut short** with both figures. Copy the file from the camera again and use **Relink** or **Refresh Metadata** to bring in the whole clip.

## Subtitles

- **SRT** (SubRip) — standard timed text format
- **VTT** (WebVTT) — web-compatible subtitle format

Imported subtitles appear as clips on a subtitle track.

## Media Management

Klipp references media files by their original file paths. The project does not copy media files into the project directory. If you move or rename source files, Klipp prompts you to relink them on next open.

## After Import

Imported media appears in the media browser. From there you can:

- Hover to skim through clips
- Double-click to load into the source viewer for marking up
- Drag to the timeline or use **W** / **D** to insert / overwrite
- Organise into bins
