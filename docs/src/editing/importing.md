# Importing Media

Klipp supports importing video, audio, and image files into your project's media pool.

## Import Methods

### File Menu
Go to **File → Import Media...** (or press **Ctrl+I**) to open the file picker. Select one or more files. An import is one undo step.

### Drag and Drop
Drag files from your file manager directly into the media browser panel. Klipp accepts files dropped anywhere in the browser area. Dropping a folder imports recursively.

## Supported Video Formats

Klipp uses ffmpeg-next for decoding and supports all formats that FFmpeg handles:

- **Containers** — MP4, MKV, MOV, AVI, WebM, MPEG-TS, MXF
- **Video codecs** — H.264/AVC, H.265/HEVC, ProRes, DNxHD, VP9, AV1, MPEG-2, MJPEG
- **Frame rates** — Any frame rate; rational-time tracking preserves drop-frame and fractional rates exactly

Decoding tries CUDA NVDEC first, then VAAPI, then software. During playback frames are scaled to the maximum playback resolution set in Preferences. AV1 and VP9 skip VAAPI on NVIDIA, whose VAAPI driver returns empty frames for them. A stream moves to software decode when the GPU's decoder is full or refuses its frames.

## Supported Audio Formats

- **Containers** — WAV, FLAC, MP3, AAC, OGG, AIFF
- **Codecs** — PCM (16/24/32-bit), FLAC, MP3, AAC, Vorbis, Opus
- **Sample rates** — 44.1 kHz, 48 kHz, 96 kHz, and others

Audio is resampled to the project sample rate on the fly with a recreate-on-seek pattern that avoids stale-buffer artefacts.

## Supported Image Formats

- PNG, JPEG, TIFF, BMP, WebP, OpenEXR, HEIC / HEIF, AVIF, JPEG XL
- Images are imported as still clips with a configurable default duration

## Camera RAW

Klipp does not support proprietary camera RAW video: REDCODE (`.r3d`), ARRIRAW and Blackmagic RAW (`.braw`). Decoding them needs each maker's closed SDK, and Klipp decodes video through FFmpeg. This is a deliberate choice, not a planned feature. Transcode the footage first in the maker's free tool (REDCINE-X PRO, ARRI Reference Tool, or Blackmagic's own software for Blackmagic RAW) to ProRes or DNxHR, and import that.

Still-image RAW files (`.cr2`, `.nef`, `.arw`, `.dng` and others) import as stills, without developing the sensor data. See [Camera RAW](../reference/formats.md#camera-raw) for the details and the limits.

## Image Sequences

Import numbered image sequences (e.g., `frame_0001.png`, `frame_0002.png`) as a single video clip. Select the first image in the sequence; Klipp detects the rest automatically based on the numbering pattern.

## Camera Log Footage

Klipp auto-detects log-encoded source material at import time using a four-layer chain:

1. Stream-level metadata (Sony writes the `encoder` tag on the stream, not the format)
2. Encoder string heuristics for tagless files (`HEVC Coding` → SLog3, etc.)
3. IDT colour transform splice
4. User override stored in the project on save

Klipp decodes ARRI LogC3 and LogC4, Sony S-Log2 and S-Log3, Canon Log and Canon Log 3, Panasonic V-Log, Nikon N-Log, Fuji F-Log, DJI D-Log, GoPro ProTune Flat, Apple Log, Leica L-Log, RED Log3G10 and BMD Film Gen 5. See [Supported Formats](../reference/formats.md#import-camera-log-encodings) for the gamut of each, and [Source Colourspace & Log](../color/source-colorspace.md) for detection and the override.

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

**File → Import Subtitles...** reads SRT, WebVTT, ASS / SSA, TTML (including DFXP, IMSC and iTT), Scenarist SCC (CEA-608) and EBU-STL. The captions land on a subtitle track, keeping their inline styling. See [Subtitles & Captions](./subtitles.md).

## Timelines from Other Editors

**File → Open Project from Another Editor...** opens an FCPXML, Final Cut Pro 7 XML, EDL, OpenTimelineIO, AAF or Kdenlive timeline as a new project; **File → Import Timeline** brings one into the open project. Each import keeps a report of what it carried, changed or left out, under **File → Last Import Report…**. See [Timeline Interchange](../export/interchange.md).

## Media Management

Klipp references media files by their original file paths; the project does not copy them. If an import or an opened project has missing media, the relink flow opens at once to point it at its files (**File → Relink Media...** opens it any time). Undoing a relink gives each item its old file's details back. **File → Consolidate Project...** copies the media into one place, optionally only what the cut uses, trimmed with handles. Music and sound effects downloaded from the Library go into the saved project with their credits.

## After Import

Imported media appears in the media browser. From there you can:

- Hover to skim through clips
- Click to load into the source viewer for marking up
- Drag to the timeline or use **,** / **.** to insert / overwrite
- Organise into bins and smart bins
- Transcribe, analyse and tag them; see [AI Tools](../ai-tools/overview.md)
