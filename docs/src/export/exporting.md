# Exporting Projects

Open the export settings with **File → Export...** (**Ctrl+E** by default). The same settings also sit in the right-hand column of the Export workspace.

## Export Settings

### Presets

The **Preset** menu fills in the settings for a delivery. Pick **Custom** to set everything by hand. The built-in presets:

| Preset | Container | Video | Size | Audio | Loudness |
|---|---|---|---|---|---|
| YouTube 1080p | MP4 | H.264, CRF 18 | 1920×1080 | AAC 192 kbps | — |
| YouTube 4K | MP4 | H.265, CRF 20 | 3840×2160 | AAC 256 kbps | — |
| Vimeo 1080p | MP4 | H.264, CRF 16 | 1920×1080 | AAC 256 kbps | — |
| Twitter/X | MP4 | H.264, CRF 23 | 1280×720 | AAC 128 kbps | — |
| Instagram Square | MP4 | H.264, CRF 20 | 1080×1080 | AAC 192 kbps | — |
| Shorts, Reels, TikTok 9:16 | MP4 | H.264, CRF 20 | 1080×1920 | AAC 192 kbps | — |
| ProRes 422 | MOV | ProRes 422 | Sequence | PCM 24-bit | — |
| HEVC Main10 master | MP4 | H.265 10-bit, CRF 16, Slow | Sequence | AAC 320 kbps | — |
| DNxHD 115 | MOV | DNxHD 115 Mbps | 1920×1080 | PCM 24-bit | — |
| H.265 4K | MP4 | H.265, CRF 22 | 3840×2160 | AAC 256 kbps | — |
| H.265 HQ (GPU) | MP4 | H.265, CRF 16, Slow | Sequence | AAC 256 kbps | — |
| Audio Only MP3 | MP3 | none | — | MP3 320 kbps | — |
| Broadcast master (EBU R128) | MOV | ProRes 422 | Sequence | PCM 24-bit | −23 LUFS |
| Web 1080p (-14 LUFS) | MP4 | H.264, CRF 18 | 1920×1080 | AAC 192 kbps | −14 LUFS |
| Animated GIF (480p, 15fps) | GIF | GIF | 854×480, 15 fps | none | — |

**Save as Preset** stores the current settings under a name of your choosing, and **Manage...** renames, duplicates, deletes, imports and exports your own presets. Your presets are kept in `~/.config/klipp/preferences.json` and are listed after the built-ins with a ⚙ mark.

### Output

- **Output file** — where the file is written. Changing the format changes the file extension to match.
- **Format** — the container: MP4, MKV, MOV, WebM or GIF, or for the sound alone MP3, WAV or M4A (the picture settings are then not used). Changing it swaps out a video or audio codec the container cannot hold: ProRes and DNxHD go in MOV or MKV, WebM holds only VP9 or AV1 with Opus, MOV holds neither AV1, VP9, Opus nor FLAC, and GIF holds only the GIF codec.
- **Video codec** — H.264, H.265, AV1 or VP9, plus ProRes 422 / 422 HQ / 4444 and DNxHD 115 / 145 when the format is MOV or MKV. The GIF codec is chosen by choosing the GIF format. See [Export Formats](./formats.md) for what each one is for.

### Video

- **Bit depth** — 8-bit or 10-bit, for H.264, H.265 and AV1. ProRes is always 10-bit and DNxHD always 8-bit.
- **HDR** — SDR, HLG or HDR10 (PQ). Offered for ProRes, and for H.264, H.265 and AV1 at 10 bits. Choosing H.264 for an HDR delivery shows a warning and a button to switch to H.265: 10-bit H.264 is the High 10 profile, which no hardware encoder writes and many players cannot decode.
- **Mastering peak** — for a PQ delivery, the display the master is graded for: 1000, 2000 or 4000 nits. HLG is fixed at 1000 nits. An H.265 PQ export also carries HDR10+ dynamic metadata, measured from its frames.
- **Banding** — where an 8-bit export of a smooth gradient will band, the settings suggest 10 bits or a fine grain.
- **Tone map** — for SDR deliveries: None, Reinhard, Hable (the default) or ACES.
- **Output transform** — see below.
- **Rate control** — CRF (quality), CBR or VBR, for H.264, H.265, AV1 and VP9. CRF takes a quality value from 0 to 51; CBR and VBR take a bitrate from 500 to 100,000 kbps.
- **Encoder speed** — Fast, Medium, Slow or Very Slow, for H.264, H.265 and AV1.

### Output Transform

The output transform decides the colour space the pixels are written in. The choices:

- **Match Viewer** (default) — the file looks the way the viewer has been showing the picture while you edit. It works for SDR, HLG and PQ deliveries.
- **Rec.709 SDR** — the ACES Rec.709 output transform, whatever the project's working space.
- **Rec.2020 SDR** — the ACES video transform on BT.2020 primaries, tagged and matrixed BT.2020.
- **Rec.2100 PQ (1000 nit)** and **Rec.2100 PQ (4000 nit)** — the ACES Rec.2100 PQ output transforms. Only offered for a PQ delivery. They set the mastering peak themselves.
- **DCI-P3 D65 (48 nit)** — the ACES DCI-P3 D65 output transform, for cinema projection. The pixels are RGB with a 2.6 gamma; this is not the DCI XYZ encoding a DCP holds.
- **sRGB curve only (legacy)** — an sRGB encode with no ACES output transform. Only offered in a Rec.709 working space.

The list only shows transforms that suit the delivery and the project's working space. The transform applies to the whole export: there is no per-clip override. See [Working Colorspaces](../color/working-colorspaces.md) for the full chain.

### Audio

- **Audio codec** — AAC, MP3, Opus, FLAC, PCM 16-bit, PCM 24-bit, or **No audio**. Klipp writes a stereo mix.
- **Audio bitrate** — 128, 192, 256 or 320 kbps, for AAC, MP3 and Opus.
- **Loudness** — tick **Normalize** and pick a target: YouTube / Spotify (−14 LUFS), Podcast (−16), EBU R128 (−23), ATSC A/85 (−24), or a custom value from −30 to −5 LUFS. The render measures the mix first, sets the gain to reach the target, and holds the true peak at −1 dBTP (−2 dBTP for ATSC A/85) with a limiter when the gain would push peaks past it; when the limiter holds the level down, the gain is fitted again by the loudness measured in the file. The render queue shows the loudness measured in the written file. Playback is heard at the loudness the export will deliver.

GIF files carry no audio, so these controls are disabled for GIF.

### Size and Rate

- **Resolution** — the sequence's size, 720p, 1080p, 4K, or a custom size from 128×128 up to 7680×4320. When the size differs from the sequence's, the picture is scaled to fit inside it without distortion and padded.
- **Frame rate** — the sequence's rate, or 24, 25, 29.97, 30 or 60 fps. At a different rate, output frame *n* shows the timeline at time *n* ÷ rate, so a 24 fps sequence exported at 12 fps keeps its length and plays every other frame. Frames are not blended.

### Range

- **Full timeline**
- **In/Out range** — when the sequence has both an in and an out point.
- **Selected clips** — when clips are selected.
- **Between markers** — one file for each stretch between markers, with options to include the stretch before the first marker and after the last.

When the sequence has subtitle tracks, you can burn the subtitles into the picture, embed them as a soft subtitle track (not in GIF), carry them as **closed captions in the video (CEA-608)** for H.264 and H.265, or a mix of these. MP4, MKV and MOV exports carry chapters taken from markers whose kind is **Chapter**.

Before exporting, **Window → Delivery Check** runs the delivery check over the cut: the craft's rules, the sequence's brief, and on request the colour of every shot and the mix's loudness. Each finding names what to fix; one you accept is kept with your reason, as one undo step.

## Delivery Specs

A **delivery spec** is what a platform or a broadcaster asks a finished piece to be: container, codec, frame size and rate, bit depth, colour (SDR or HDR), audio, loudness and true peak, captions. In the **Delivery Spec** section of the export settings, pick one and the export is set from it.

The built-in specs take their numbers from each platform's or standards body's public specification, named with the spec: YouTube (SDR and HDR), Vimeo, Instagram Reels and TikTok (vertical), EBU R 128 and ATSC A/85 broadcast loudness, and a streaming master. Where a platform only recommends a setting, missing it is a warning, not a failure.

- **Check the Export** checks the export as the settings would make it, against the spec.
- **Check a File…** checks a finished file: it is read with `ffprobe`, and its loudness and true peak are measured with Klipp's own meter (a multichannel mix is measured as its stereo downmix).

Each requirement is listed as passing, a warning or failing, with what was found, what is wanted and what to change. A loudness target the spec measures on dialogue only (ATSC A/85, the streaming master) is read over the whole mix, so it is a guide and warns rather than fails.

**Save as My Spec…** keeps the spec's requirements with your export settings under a name of your own. Your specs are in `delivery-specs.json` in Klipp's settings folder (`~/.config/klipp/`), where their requirements can be edited.

An assistant can list the specs and check the timeline or a file against one too.

## Render Queue

Both **Export** and **Add to Queue** put the render in the render queue. **Export** also closes the settings and opens the queue, and asks before overwriting an existing file. The queue runs two jobs at once by default. Change this with **Parallel render jobs** (1 to 8) in the export preferences.

For each job the queue shows progress, the estimated time remaining, render speed, the hardware used to decode and encode, and a **Cancel** button. Rendering runs in the background, so you can keep editing. The file is written under a temporary `.partial` name, and a cancelled render removes it.

## Hardware Encoding

Klipp tries every hardware encoder FFmpeg has for the codec, in turn, before the software one:

- **H.264 and H.265** — NVENC (NVIDIA), then AMF (AMD), then VAAPI (AMD and Intel), then libx264 / libx265.
- **AV1** — NVENC, then AMF, then VAAPI, then SVT-AV1.
- **VAAPI** is tried on each AMD or Intel GPU's render node (`/dev/dri/renderD*`), and again there in low-power mode, which is the only one some Intel GPUs have for HEVC and AV1. It is not tried on NVIDIA's, whose VAAPI driver only decodes.
- **10-bit H.264** always uses libx264, because no hardware H.264 encoder writes ten bits.
- A sequence smaller than 128×128 always encodes in software.

ProRes, DNxHD, VP9 and GIF always encode in software.

A distribution's FFmpeg usually has all three families whatever GPU the machine has, so being listed says nothing: a hardware encoder can refuse to open because the GPU is another vendor's, or because every NVENC session on the card is taken by other programs or other exports. When one refuses, Klipp goes on to the next, and in the end to the software encoder for the same codec. The job's hardware line then reads **Software (CPU): NVENC unavailable** on an NVIDIA machine, and **Software (CPU): hardware encoder unavailable** otherwise. If the software encoder fails too, the export fails with every encoder's error. Only an encoder that refuses to open falls back: one that opens and then fails on its first frame still fails the export.

## Cinema (DCP) Delivery

Klipp does not make DCPs. The supported route is to export a high-quality master and wrap it with a dedicated tool: [DCP-o-matic](https://dcpomatic.com) (free) or easyDCP.

- Export ProRes 4444 or ProRes 422 HQ in MOV, with PCM 24-bit audio, at the frame rate the DCP will play (usually 24 fps).
- Klipp has no DCI XYZ output transform, so the master is RGB. DCP-o-matic converts it to XYZ; tell it the master's colour space when you add the file. The DCI-P3 D65 transform writes P3 pixels with a 2.6 gamma; the file is tagged P3 D65 with the transfer unspecified, so give DCP-o-matic the gamma by hand if you use it.
- Klipp's export writes a stereo mix. A 5.1 or 7.1 DCP needs its sound prepared elsewhere.

Native DCP export would need a DCI XYZ output transform, JPEG 2000 encoding to the DCI profile, MXF wrapping of picture and sound, and the CPL, PKL and ASSETMAP files that describe the package (usually done with asdcplib).

## IMF Packages

Klipp does not make IMF (Interoperable Master Format) packages. IMF delivery is mastering and conformance work: a platform checks the package against its own, regularly revised delivery specification, and that job is done by dedicated mastering tools. IMF's supplemental packages describe what changed since an earlier delivery, which needs a record of versioned deliveries that Klipp does not keep.

What Klipp offers instead is a clean mastering export for a facility or a mastering tool to work from: ProRes 422 HQ or 4444, PCM 24-bit audio, loudness normalised to a target with a true-peak ceiling, and for HDR, PQ or HLG with BT.2020 colour tags and, for PQ, the mastering-display and MaxCLL / MaxFALL metadata.
