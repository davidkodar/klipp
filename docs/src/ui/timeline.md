# Timeline

The timeline is the primary editing surface where you arrange, trim, and organize your clips.

## Toolbar

The timeline's own toolbar sits above the sequence tabs and holds every control that changes how an edit behaves: the edit tools, the INS / OVR edit mode, snap and skimming, the marker button and the zoom. See [Toolbar](toolbar.md#timeline-toolbar).

## Tracks

The timeline supports multiple video and audio tracks:

- **Video tracks** stack from bottom to top. Higher tracks composite over lower tracks.
- **Audio tracks** mix together. Each track has independent volume and pan.
- Add tracks from **Timeline → Add Video Track** or **Timeline → Add Audio Track**.
- Tracks display a header on the left with name, lock, mute/solo, and visibility toggles.

## Clips

Clips on the timeline represent segments of imported media. They display:

- **Filmstrip thumbnails** on video clips, showing frames at regular intervals
- **Audio waveforms** on audio clips and linked audio
- **Colour tint bar** at the top edge for visual identification
- Clip name overlay

## Playhead

The playhead is a vertical line indicating the current position. Click anywhere on the ruler or track area to move the playhead. During playback, the playhead moves smoothly using wall-clock interpolation against the master clock — sub-frame timing is honest at any monitor refresh rate.

The playhead position is stored as a raw `f64` seconds value, not snapped to any frame rate, so seeking and trimming have no quantisation drift.

## Magnetic Timeline

Klipp uses a magnetic timeline model inspired by Final Cut Pro. The full set of FCP-parity behaviours (insert, overwrite, ripple, slip, slide, replace, append, swap, lift, gap-fill) is documented in [Magnetic Timeline](../editing/magnetic-timeline.md). Headlines:

- Clips automatically snap together, eliminating gaps
- Moving a clip ripples adjacent clips to maintain continuity
- Deleting a clip closes the gap automatically (ripple delete)
- Override magnetic behaviour by holding a modifier key while dragging

## Snapping

Toggle snapping with **N**. When enabled, clips snap to:

- The playhead
- Other clip boundaries
- In/out points
- Markers

A visual indicator appears when a snap occurs.

## Filmstrip Thumbnails

Video clips display filmstrip thumbnails that update as you zoom:

- Thumbnails are generated incrementally (~60 frames per clip, 100 ms throttle, streamed via channel)
- Progressive refinement: zooming in queues higher-density thumbnails for visible tiles
- Only viewport-visible tiles are rendered for performance
- Dark separators mark distinct frame boundaries
- A coloured tint bar at the top edge identifies the clip across cuts

## Waveforms

Audio tracks and linked audio display waveforms showing amplitude over time. Waveforms cache to disk so re-renders on zoom are sub-millisecond.

## Zoom and Navigation

- **=** / **-** — Zoom in/out
- **Z** — Zoom to fit the whole timeline; **Shift+Z** zooms to the selection
- **Scroll wheel** — Horizontal scroll (or zoom with Ctrl held)
- **Home** / **End** — Jump to timeline start/end

## Status Bar

An 18-pixel status bar below the timeline tracks shows:

- **Left side** — Clip count, selected clip name, and duration
- **Right side** — Filmstrip visible/total count and zoom level (pixels per second)

## Track Headers

Each track header shows:

- Name (double-click to rename)
- Lock toggle (prevents edits)
- Mute / Solo (audio)
- Visibility (video)
- Output target indicator (which audio bus the track sends to)

## Camera Lane

The sequence camera's move — where it looks, how far in, its roll — is keyed on the sequence, not on a clip, so its keys have a lane of their own between the ruler and the tracks. The diamond at the right of the track header column's ruler spacer shows or hides it; the diamond is lit when the camera carries keys, so a move written over the control socket is not invisible until someone thinks to look.

The lane has one row per value: **X**, **Y**, **Zoom** and **Roll**, each in its own colour, with every key drawn at the moment it is stored. The pointer finds a key by the same rule the inspector's CAMERA rows use: the nearest key within the displayed frame, and it is edited at its own timestamp, never moved by being looked at.

- **Click** a key to select it; the curve editor follows the selection.
- **Drag** a key along the lane to move it in time. It snaps to the playhead and to other camera keys; its value is left alone.
- **Alt-click** a row to add a key at the playhead's frame, holding the value the curve has there.
- **Double-click** a key to remove it.
- **Right-click** a key for the curve out of it (Linear, Hold, Bezier), a named ease into it, or removal.

A drag is one undo step, however far the key travelled.

With no clip selected, the curve editor shows the sequence camera's four curves over the sequence's time instead of a clip's. Keys are added by double-click at the exact moment under the pointer, dragged in time and value, and given Bezier handles, as a clip's keyframes are.
