# Toolbar

The toolbar sits within the menu bar area and provides quick access to editing tools and transport controls.

## Layout

From left to right, the toolbar contains:

1. **Klipp logo** -- Application identity in the top-left corner.
2. **Menu bar** -- File, Edit, Clip, Timeline, Mark, View, Window, Help menus.
3. **Tool buttons** -- Editing tool selection.
4. **Transport controls** -- Playback controls centered below the viewer.

## Editing Tools

Three primary tools are available, switchable with a single keypress:

### Select Tool (V)
The default tool. Click clips to select them, drag to move, and drag clip edges to trim. Hold Shift to select multiple clips. This is the tool you will use most of the time.

### Blade Tool (B)
Click on a clip to split it at that position. The blade follows the mouse cursor with a vertical indicator showing where the cut will land. With snapping enabled, the blade snaps to the playhead and other clip boundaries.

### Range Tool (R)
Click and drag to select a range across one or more tracks. The selected range can be deleted, copied, or used to set in/out points. Useful for selecting portions of clips without selecting entire clip objects.

## Transport Controls

The transport controls appear below the viewer and provide playback management:

| Control | Shortcut | Action |
|---------|----------|--------|
| Play/Pause | Space | Toggle playback |
| Reverse | J | Play in reverse (press multiple times to increase speed) |
| Stop | K | Stop playback |
| Forward | L | Play forward (press multiple times to increase speed) |
| Step Back | Left arrow | Move one frame backward |
| Step Forward | Right arrow | Move one frame forward |
| Go to Start | Home | Jump to timeline start |
| Go to End | End | Jump to timeline end |

### JKL Shuttle

The J, K, and L keys provide variable-speed shuttling:

- **L** once = 1x forward, twice = 2x, three times = 4x, four times = 8x
- **J** once = 1x reverse, twice = 2x, three times = 4x, four times = 8x
- **K** = stop
- **K+L** held together = slow forward (0.5x)
- **K+J** held together = slow reverse (0.5x)

## Timecode Display

The transport area shows the current playhead position in **HH:MM:SS.mmm** format, providing millisecond-accurate timecode readout that updates smoothly during playback.
