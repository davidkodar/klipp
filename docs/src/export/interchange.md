# Timeline Interchange

Klipp reads and writes timelines in the common interchange formats. Each carries the edit (which clips, where, and which part of the source) and loses most of what Klipp does on top of it: grades, most effects and most titles stay behind.

| Format | Import | Export |
|---|---|---|
| OpenTimelineIO (`.otio`) | yes | yes |
| Kdenlive / MLT (`.kdenlive`, `.mlt`) | yes | yes |
| FCPXML 1.9 (`.fcpxml`) | yes | yes |
| Final Cut Pro 7 XML (xmeml, `.xml`) | yes | no |
| CMX 3600 EDL (`.edl`) | yes | yes |
| AAF (`.aaf`), through OpenTimelineIO | yes, with OpenTimelineIO's AAF adapter | yes, with the same |

**Opening:** **File → Open Project from Another Editor...** opens any of these as a new project, asking about unsaved changes first; a file dropped on the window does the same. The format is read from the content, so a misnamed extension still works. **File → Import Timeline** adds one to the current project as a new sequence instead. The media is added to the media pool, and media that is not found can be relinked at once.

**Import report:** after an import Klipp shows what it brought in, grouped warnings for what it changed or left out, and the media it could not find, with a **Relink** button. The report is kept with the project; **File → Last Import Report…** opens it again.

**Export:** **File → Export Timeline** has FCPXML, Kdenlive Project, EDL, OpenTimelineIO and AAF (via OpenTimelineIO); each is in the command palette too. Every export writes the active sequence, and what it could not write is listed in `<name>.export_warnings.txt` beside the exported file.

All formats refer to media by its file path. The receiving system needs the same files at the same paths, or has to relink them.

## OpenTimelineIO

OTIO has adapters to many other applications, so an `.otio` file is the most general way out of Klipp. Only plain `.otio` JSON is read and written, not `.otiod` bundles or `.otioz` archives.

**Export** writes:

- every video and audio track, in order, with gaps where the track is empty
- media clips with their source range, enabled state and clip markers
- sequence markers, with their colour and duration
- an offline clip as a missing reference, so the receiving application knows the file is gone

Titles, generators, adjustment layers and compound clips become gaps of the same length, so everything after them stays in place. Subtitle and text tracks are not exported. Transitions, grades and effects are not written. Clips that overlap on one track cannot be expressed in OTIO, where a track is a sequence of items; the later clip is moved to follow the earlier one. Everything dropped or moved is listed in `<name>.export_warnings.txt` beside the exported file.

**Import** reads video and audio tracks, clips with their media (a missing or unlinked reference becomes an offline item), gaps, transitions, constant speed changes, and timeline and clip markers. Other effects are named in the import report.

## AAF

Klipp reads and writes AAF by way of OpenTimelineIO's converter, `otioconvert`, with its AAF adapter. Opening an AAF without them, Klipp says what is missing and offers to install them into a Python environment of its own, showing the exact commands first; with your consent it runs them, shows their output, and opens the file when they finish. To install them yourself:

```sh
pip install opentimelineio otio-aaf-adapter
```

**AAF (via OpenTimelineIO)…** exports the sequence as OTIO and converts it. What reaches the AAF file is what reaches the OTIO one (see above), and an AAF import carries what the OTIO import does.

## Kdenlive (MLT)

**Export** writes a `.kdenlive` project that Kdenlive and `melt` open: every video and audio track, and each media clip's position and source range. Titles, generators, adjustment layers and compound clips become blanks of the same length. Transitions, effects, keyframes, grades and markers are not written.

**Import** reads:

- tracks, media clips and blanks
- solid-colour clips, as generators, and Kdenlive titles, as text
- dissolve-type transitions (`luma`, `mix`, `composite`, `frei0r.cairoblend`), as cross dissolves; any other transition becomes a cut
- position, scale and opacity keyframes from the `qtblend` and `affine` filters, rotation from `qtblend`, and volume keyframes from the `volume` filter

Other filters are dropped with a warning.

## FCPXML 1.9

**Export** walks the **first video track only**. It writes:

- media clips on that track, with their position, length, source in point and enabled state
- the clips' markers
- titles on that track, as FCPXML titles carrying their text
- sequence markers, with their duration and comment

Gaps on that track stay gaps; compound clips, generators and adjustment layers become named gaps. Other video tracks, audio tracks, transitions, audio levels, grades and effects are not exported.

**Import** reads the primary storyline (the `spine`) onto a video track, and connected clips onto lane tracks above and below it: asset clips and clips with their in points and enabled state, gaps, transitions joined to their clips, titles as editable titles, timeMap speed, opacity and volume with their keyframes, and markers with their notes and colours, chapters kept as chapters. Transforms, effects, compound clips, multicam clips, synchronised clips and auditions are named in the import report. An FCPXML saved as `.xml` is read too.

## Final Cut Pro 7 XML (xmeml)

**Import** reads the first sequence, with its rate (NTSC and drop frame) and frame size: its video and audio tracks, their clips with their files, enabled or not, linked picture and sound; gaps; constant speed and reverse; opacity and audio level with their keyframes; cross dissolves and dips between two clips; and markers on the sequence and on clips. Generators such as titles, other filters and nested sequences are named in the import report.

## CMX 3600 EDL

**Export** lists every enabled clip on every video and audio track, one event per clip:

- source in and out, and record in and out, in timecode
- the channel: `V` for video, `A1` to `A4` for the first four audio tracks, and `A` after that
- a reel name of up to eight characters, taken from the clip's logged reel, then the reel the camera recorded, then the file name; two sources that shorten to the same name are numbered apart, with a comment saying so
- `* FROM CLIP NAME:` with the file name
- transitions on the incoming clip: horizontal and vertical wipes as SMPTE wipes `W001` and `W002`, everything else as a dissolve `D`, with its length in frames and a `* EFFECT NAME:` comment

The header says `DROP FRAME` at 29.97 and 59.94 fps and `NON-DROP FRAME` otherwise, and every timecode is counted the same way. Effects, grades, levels and markers are not written.

**Import** reads the events as cuts, matching each to media already in the pool by its `* FROM CLIP NAME` comment, else by its reel. Source timecodes are taken from the file's start timecode when they lie past it (a conform EDL), and from zero otherwise. A dissolve or wipe comes in as a cut, and an event whose media is not in the pool is listed in the report.

## Which to Use

| Need | Use |
|---|---|
| Hand a multitrack edit to another application | OpenTimelineIO, or Kdenlive for Kdenlive itself |
| Send a conform list against camera reels | EDL |
| Hand a single-track cut to an FCPXML reader | FCPXML |
| Keep everything, including grades and effects | Klipp's own project file |
