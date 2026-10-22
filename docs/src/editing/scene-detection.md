# Scene Detection

A long recording is often several shots welded into one file — a
multi-camera event, a tape transfer, a screen capture. Scene detection
finds where the picture changes and marks those points on the clip, so
you can cut the file into its shots without scrubbing for them.

## Using it

1. Right-click a clip on the timeline (or select it and use **Timeline → Detect Scene Cuts**).
2. Choose **Detect Scenes…**. The menu item is replaced by a progress
   readout while it runs, with a **Cancel** beside it.
3. Each cut it finds becomes a clip marker, in the colour set in
   Preferences.
4. Right-click the clip again and choose **Split at Scene Markers (N
   cuts)** to blade it at every marker. That is one undo step, so one
   Ctrl+Z puts the clip back together.

The analysis runs in the background: the timeline stays live, and
several clips can be detecting at once.

## How it decides

Klipp builds a 64-bin luminance histogram for each frame and compares
it with the previous frame's, using Rec.601 weights and an L1
distance. A cut is declared where the distance crosses a threshold
scaled by the sensitivity setting. It runs on the CPU, over frames
decoded for the purpose.

This finds hard cuts well. A dissolve, a fade or a whip pan is a
gradual change rather than a jump, so expect those to be missed or to
land a few frames late.

## Settings

**Preferences → Editing**:

| Setting | Default | What it does |
|---|---|---|
| Sensitivity | 0.4 | Lower finds more cuts, including false ones; higher only finds obvious ones |
| Minimum gap | 0.5 s | How close two cuts may be, which stops a flickering shot producing a run of markers |
| Marker colour | Yellow | The colour the scene markers take |

## Related

- [Markers](./markers.md) — what else clip markers are used for
- The assistant's `find_shots` reads the same boundaries as data, along
  with what is in each shot.
