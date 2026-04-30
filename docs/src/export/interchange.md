# FCPXML & EDL

Klipp supports exporting and importing project timelines in interchange formats for use in other editing applications.

## FCPXML 1.9

Final Cut Pro XML version 1.9 — supported by Final Cut Pro and DaVinci Resolve, partially by Premiere Pro.

### What is exported
- Timeline structure (sequence, tracks)
- Clip positions and durations
- In / out points
- Basic transitions (cross dissolves)
- Audio levels
- Markers
- Clip metadata (resolution, frame rate)

### What is not exported
- GPU effects and colour grades (application-specific representations)
- Compound clip internal structure
- Audio effects (EQ, compressor)
- Keyframe animation outside of basic opacity / transform
- AI-generated mattes (the matte PNGs are external; references would not survive in another NLE)

### Exporting

1. **File → Export**
2. Select the **Interchange** tab
3. Choose **FCPXML 1.9**
4. Pick the output file location
5. **Export**

### Importing

1. **File → Import FCPXML**
2. Pick the FCPXML file
3. Klipp creates a new sequence in the current project mirroring the FCPXML structure

Klipp's importer is verified against real exports from Final Cut Pro and DaVinci Resolve — not just spec / docs. This matters because in practice both applications take liberties with the FCPXML schema; matching their actual output is more useful than matching the spec.

### Notes

FCPXML references media files by their original file paths. Ensure the receiving system has access to the same media files, or relink media after import.

## CMX 3600 EDL

Industry-standard text-based interchange format. Single-track only.

### What is exported
- Cut edit points (in / out timecodes for source and record)
- Dissolve transitions with duration
- Source file references by reel name

### What is not exported
- Multiple video tracks (EDL is single-track by definition)
- Effects, colour grades, audio mixing
- Markers

### Exporting

1. **File → Export**
2. Select the **Interchange** tab
3. Choose **CMX 3600 EDL**
4. Configure the timecode frame rate (drop-frame / non-drop-frame)
5. Pick the output file
6. **Export**

### EDL Format

Output follows standard CMX 3600 conventions:

```
TITLE: My Project
FCM: NON-DROP FRAME

001  AX       V     C        01:00:00:00 01:00:05:12 01:00:00:00 01:00:05:12
* FROM CLIP NAME: interview_01.mp4

002  AX       V     D    030 01:00:10:00 01:00:15:00 01:00:05:12 01:00:10:12
* FROM CLIP NAME: broll_02.mp4
```

### Compatibility

CMX 3600 EDL is supported by virtually all major NLEs — DaVinci Resolve, Avid Media Composer, Premiere Pro, Final Cut Pro. The most reliable interchange format for simple, single-track timelines.

## When to Use Which

| Need | Use |
|---|---|
| Round-trip a multi-track edit to FCP / Resolve | FCPXML |
| Send a colourist a cut list against camera reels | EDL |
| Archive a timeline with effect parameters | Klipp's native `.klipp` directory format |
| Hand off to a sound engineer | Export OMF (planned) or stems |
