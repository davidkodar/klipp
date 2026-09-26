# Timeline Interchange

Klipp reads and writes timelines in four interchange formats. Each one carries the edit (which clips, where, and which part of the source) and loses most of what Klipp does on top of it: grades, effects and most titles stay behind.

| Format | Import | Export |
|---|---|---|
| OpenTimelineIO (`.otio`) | yes | yes |
| Kdenlive / MLT (`.kdenlive`) | yes | yes |
| FCPXML 1.9 (`.fcpxml`) | yes | yes |
| CMX 3600 EDL (`.edl`) | no | yes |

**Import:** **File → Import Timeline** has Kdenlive Project, OpenTimelineIO and Final Cut Pro XML. An import adds a new sequence to the current project and switches to it; its media is added to the media pool. What the importer could not bring in is listed in a file beside the one you imported, named `<name>.import_warnings.txt`, and a message says how many there were.

**Export:** **File → Export Timeline** has FCPXML, Kdenlive Project and EDL. OpenTimelineIO export is in the command palette, as **Export OpenTimelineIO (.otio)…**. Every export writes the active sequence.

All four formats refer to media by its file path. The receiving system needs the same files at the same paths, or has to relink them.

## OpenTimelineIO

OTIO has adapters to many other applications, so an `.otio` file is the most general way out of Klipp. Only plain `.otio` JSON is read and written, not `.otiod` bundles or `.otioz` archives.

**Export** writes:

- every video and audio track, in order, with gaps where the track is empty
- media clips with their source range, enabled state and clip markers
- sequence markers, with their colour and duration
- an offline clip as a missing reference, so the receiving application knows the file is gone

Titles, generators, adjustment layers and compound clips become gaps of the same length, so everything after them stays in place. Subtitle and text tracks are not exported. Transitions, grades and effects are not written. Clips that overlap on one track cannot be expressed in OTIO, where a track is a sequence of items; the later clip is moved to follow the earlier one. Everything dropped or moved is listed in `<name>.export_warnings.txt` beside the exported file.

**Import** reads video and audio tracks, clips with their media (a missing or unlinked reference becomes an offline item), gaps, and timeline and clip markers. Transitions and effects, including speed changes, are skipped with a warning.

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

**Import** reads the primary storyline (the `spine`) onto one video track: asset clips and clips with their in points and enabled state, gaps, clip markers and sequence markers. Titles, transitions, compound clips, multicam clips, synchronised clips, auditions, connected clips and effects are skipped with a warning.

## CMX 3600 EDL

**Export** lists every enabled clip on every video and audio track, one event per clip:

- source in and out, and record in and out, in timecode
- the channel: `V` for video, `A1` to `A4` for the first four audio tracks, and `A` after that
- a reel name of up to eight characters, taken from the clip's logged reel, then the reel the camera recorded, then the file name; two sources that shorten to the same name are numbered apart, with a comment saying so
- `* FROM CLIP NAME:` with the file name
- transitions on the incoming clip: horizontal and vertical wipes as SMPTE wipes `W001` and `W002`, everything else as a dissolve `D`, with its length in frames and a `* EFFECT NAME:` comment

The header says `DROP FRAME` at 29.97 and 59.94 fps and `NON-DROP FRAME` otherwise, and every timecode is counted the same way. Effects, grades, levels and markers are not written.

## Which to Use

| Need | Use |
|---|---|
| Hand a multitrack edit to another application | OpenTimelineIO, or Kdenlive for Kdenlive itself |
| Send a conform list against camera reels | EDL |
| Hand a single-track cut to Final Cut Pro | FCPXML |
| Keep everything, including grades and effects | Klipp's own project file |
