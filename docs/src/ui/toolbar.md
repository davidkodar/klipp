# Toolbar

Klipp has two toolbars, and they never overlap in purpose.

## App bar

The bar across the top of the window holds what applies to the whole application:

1. **Klipp** — Brand mark.
2. **Menu bar** — File, Edit, Clip, Timeline, Mark, View, Window, Tools, Help.
3. **Workspace switch** — Edit, Colour, Export. Centred.
4. **Render Queue** and **Export** — The two global actions, at the right.

## Timeline toolbar

The row above the tracks holds everything that changes how an edit on the timeline behaves, left to right:

1. **Edit tools** — Select (V), Trim (T), Blade (B), Range (R), Text (Y), Mask (P).
2. **INS / OVR** — The edit mode for insert and overwrite edits (Shift+W toggles).
3. **RIPPLE** — On, deleting and trimming close the gap; off, they leave one.
4. **Snap** (N; right-click to snap to clips, the playhead, markers, in/out or the grid) and **Skimming** (S).
5. **Timeline options** — Follow the playhead, play from proxies, lower quality for faster decode, appended clips bring their audio, and Find in Timeline (Ctrl+F).
6. **LINKED** — Picture and sound trim and move together.
7. **Add Marker** (M) and **Add Title** (Ctrl+T).
8. **Zoom** — Slider, plus and minus, and Fit (Z), at the right.

With a clip selected, the row also offers Split, Trim in, Trim out, Duplicate, Delete, Link / Unlink and Transition.

## Editing Tools

### Select Tool (V)
The default tool. Click clips to select, drag to move, drag clip edges to trim. Hold Shift to select multiple clips.

### Trim Tool (T)
Drag a clip's body to slip it and a junction to roll it; Shift+drag on the body slides it. See [Magnetic Timeline](../editing/magnetic-timeline.md).

### Blade Tool (B)
Click on a clip to split it at that position. With snapping on, the blade snaps to the playhead and clip boundaries.

### Range Tool (R)
Drag to select a range across one or more tracks. The range can be deleted, copied, or used as in and out points.

### Text Tool (Y)
Click in the viewer to start a title there, or drag to draw its text box, and type. On the timeline it acts as Select.

### Mask Tool (P)
Draw a freeform mask on the selected clip in the viewer — click for corners, drag for curves — and edit its points and handles. Enter closes a shape of three or more points; Delete removes the last point drawn, or the selected points; Escape drops the shape being drawn, or the selection. Alt-drag breaks a smooth handle. On the timeline it acts as Select.

## Transport Controls

The transport sits below each viewer:

| Control | Shortcut | Action |
|---------|----------|--------|
| Play / Pause | Space | Toggle playback |
| Reverse | J | Play in reverse; each press goes faster |
| Stop | K | Stop playback |
| Forward | L | Play forward; each press goes faster |
| Step Back / Forward | ← / → | One frame |
| Step Back / Forward more | Shift+← / Shift+→ | One second, or the frames set in Preferences |
| Go to Start / End | Home / End | |
| Loop | Ctrl+L | Loop playback |

### JKL Shuttle

- **L** plays forward at normal speed; each further press multiplies the speed by the JKL step (Preferences → Playback; 2 gives 2×, 4×, 8×).
- **J** does the same in reverse.
- **K** stops.
- **K+L** or **K+J** held together plays slowly (0.5×).

## Timecode Display

The transport shows the playhead position. Click it to type a timecode to go to.

## Jog Wheel

A jog wheel under the transport scrubs the playhead finely: drag it left or right, and the speed of the drag sets the speed of the playhead.
