# Toolbar

The toolbar sits within the menu bar area and provides quick access to editing tools and transport controls.

## Layout

From left to right, the toolbar contains:

1. **Klipp logo** — Application identity in the top-left corner.
2. **Menu bar** — File, Edit, Clip, Timeline, Mark, View, Window, Help.
3. **Tool buttons** — Editing tool selection.
4. **Transport controls** — Playback controls centred below the viewer.

## Editing Tools

Three primary tools, switchable with a single keypress:

### Select Tool (V)
The default tool. Click clips to select, drag to move, drag clip edges to trim. Hold Shift to select multiple clips. The tool you will use most of the time.

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
