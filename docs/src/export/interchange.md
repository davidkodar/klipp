# Interchange Formats

Klipp supports exporting project timelines in interchange formats for use in other editing applications.

## FCPXML 1.9

Export your timeline as FCPXML (Final Cut Pro XML) version 1.9 for import into Final Cut Pro and other compatible NLEs.

### What Is Exported

- Timeline structure (sequence, tracks)
- Clip positions and durations
- In/out points
- Basic transitions (cross dissolves)
- Audio levels
- Markers

### What Is Not Exported

- GPU effects and color grades (application-specific)
- Compound clip internal structure
- Audio effects (EQ, compressor)
- Keyframe animation

### How to Export

1. Go to **File > Export**
2. In the export dialog, select the **Interchange** tab
3. Choose **FCPXML 1.9**
4. Select the output file location
5. Click **Export**

### Notes

FCPXML references media files by their original file paths. Ensure the receiving system has access to the same media files, or relink media after import.

## CMX 3600 EDL

Export an Edit Decision List in CMX 3600 format, the industry-standard text-based interchange format.

### What Is Exported

- Cut edit points (in/out timecodes for source and record)
- Dissolve transitions with duration
- Source file references by reel name

### What Is Not Exported

- Multiple video tracks (EDL is single-track)
- Effects, color grades, audio mixing
- Markers

### How to Export

1. Go to **File > Export**
2. In the export dialog, select the **Interchange** tab
3. Choose **CMX 3600 EDL**
4. Configure the frame rate to match your timeline
5. Select the output file location
6. Click **Export**

### EDL Format

The exported EDL follows standard CMX 3600 conventions:

```
TITLE: My Project
FCM: NON-DROP FRAME

001  AX       V     C        01:00:00:00 01:00:05:12 01:00:00:00 01:00:05:12
* FROM CLIP NAME: interview_01.mp4

002  AX       V     D    030 01:00:10:00 01:00:15:00 01:00:05:12 01:00:10:12
* FROM CLIP NAME: broll_02.mp4
```

### Compatibility

CMX 3600 EDL is supported by virtually all major NLEs including DaVinci Resolve, Avid Media Composer, Adobe Premiere Pro, and Final Cut Pro. It is the most reliable interchange format for simple timelines.
