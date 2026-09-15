# Toolbar

Klipp has two toolbars, and they never overlap in purpose.

## App bar

The bar across the top of the window holds what applies to the whole application:

1. **Klipp** — Brand mark.
2. **Menu bar** — File, Edit, Clip, Timeline, Mark, View, Window, Tools, Help.
3. **Workspace switch** — Edit, Color, Export. Centred.
4. **Render Queue** and **Export** — The two global actions, at the right.

## Timeline toolbar

The row above the tracks holds everything that changes how an edit on the timeline behaves, left to right:

1. **Edit tools** — Select (V), Trim (T), Blade (B), Range (R).
2. **INS / OVR** — The edit mode for insert and overwrite edits (W toggles).
3. **Snap** (N; right-click for the per-target options) and **Skimming** (S).
4. **Add Marker** (M).
5. **Zoom** — Slider, plus and minus, and Fit (Shift+Z), at the right.

## Editing Tools

Four tools, switchable with a single keypress:

### Select Tool (V)
The default tool. Click clips to select, drag to move, drag clip edges to trim. Hold Shift to select multiple clips. The tool you will use most of the time.

### Trim Tool (T)
Drag clip edges to trim with the ripple, roll, slip and slide behaviours of the magnetic timeline.

### Blade Tool (B)
Click on a clip to split it at that position. The blade follows the cursor with a vertical indicator showing where the cut will land. With snapping enabled, the blade snaps to the playhead and clip boundaries.

### Range Tool (R)
Click and drag to select a range across one or more tracks. The selected range can be deleted, copied, or used to set in/out points. Useful for selecting portions of clips without selecting whole clip objects.

## Transport Controls

The transport sits below the viewer:

| Control | Shortcut | Action |
|---------|----------|--------|
| Play / Pause | Space | Toggle playback |
| Reverse | J | Play in reverse (multi-press for higher speed) |
| Stop | K | Stop playback |
| Forward | L | Play forward (multi-press for higher speed) |
| Step Back | ← | Move one frame backward |
| Step Forward | → | Move one frame forward |
| Step Back ×10 | Shift+← | Move ten frames backward |
| Step Forward ×10 | Shift+→ | Move ten frames forward |
| Go to Start | Home | Jump to timeline start |
| Go to End | End | Jump to timeline end |

### JKL Shuttle

The J, K, and L keys provide variable-speed shuttling:

- **L** once = 1× forward, twice = 2×, three times = 4×, four times = 8×
- **J** once = 1× reverse, twice = 2×, three times = 4×, four times = 8×
- **K** = stop
- **K+L** held together = slow forward (0.5×)
- **K+J** held together = slow reverse (0.5×)

## Timecode Display

The transport shows the playhead position in **HH:MM:SS.mmm** format. During playback, the display interpolates with the master clock at the monitor's refresh rate, so the readout is millisecond-accurate and free of jitter even when the underlying frame rate does not match the display rate.

## Jog Wheel

A jog wheel widget under the transport allows fine playhead scrubbing. Drag the wheel left or right; the speed of rotation maps to the speed of playhead movement. Useful when you need single-frame precision without leaving the viewer.
