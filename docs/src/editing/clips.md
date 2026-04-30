# Working with Clips

Clips are the building blocks of your timeline. Each clip references a segment of imported media placed at a specific position and duration.

## Adding Clips to the Timeline

### Append
Drag a clip from the media browser to the end of a track, or select it and press **Append** — placed after the last clip on the primary track.

### Insert Edit (W)
Places the clip at the playhead and pushes all subsequent clips forward. Preserves the relative timing of everything after the insertion point.

### Overwrite Edit (D)
Places the clip at the playhead, replacing whatever was there. Clips that overlap are trimmed or replaced. No rippling.

### Drag and Drop
Drag clips from the media browser directly onto any track. The magnetic timeline snaps the clip into position; drop on top of an existing clip and Klipp prompts whether to insert (ripple) or overwrite.

## Trimming

Drag the left or right edge of a clip to trim it:

- **Ripple trim** — Trims the clip and shifts subsequent clips to close the gap (default with magnetic timeline)
- **Roll trim** — Adjusts the edit point between two adjacent clips without moving other clips
- **Slip** — Adjusts the in/out points of a clip without moving its position on the timeline (alt-drag inside the clip)
- **Slide** — Moves a clip without changing its in/out, rippling neighbours

The viewer shows the trim frame in real time as you drag. The full FCP-parity trim taxonomy is documented in [Magnetic Timeline](./magnetic-timeline.md).

## Splitting (Blade Tool)

1. Press **B** to activate the blade tool
2. Click on a clip at the desired split point
3. The clip is split into two independent clips at that position

You can also split at the playhead with **Ctrl+B** while using the select tool.

## Ripple Delete

Select a clip and press **Del** to delete it. The magnetic timeline automatically closes the gap left behind. Use **Shift+Del** for a *lift* delete (leaves a gap).

## Speed Changes

Adjust clip playback speed from the **Clip → Speed** menu or the inspector:

- **Constant speed** — 25%, 50%, 100%, 200%, 400%, or custom
- Speed changes adjust the clip duration proportionally
- Source clip duration and rate are preserved; only the timeline span changes

## Replace

To swap one clip for another while keeping the same timeline span and effects:

1. Select the target clip on the timeline
2. Select the replacement clip in the browser
3. Right-click → **Replace** (or use the command palette)

The replacement inherits the original's timeline position, duration, in/out, and applied effects.

## Enable / Disable (E)

Press **E** to toggle a clip between enabled and disabled. Disabled clips remain on the timeline but are excluded from playback and export. Disabled clips appear dimmed.

## Solo (S)

Press **S** to solo the selected clip. When soloed, only that clip plays back; all other clips are temporarily muted. Press **S** again to unsolo.

## Stabilisation

Run motion stabilisation on a clip from **Clip → Stabilize**. Analysis takes ~18 seconds on 4K via the SequentialFrameDecoder; the result is cached, so adjusting smoothness does not require re-analysing. See [Stabilization](./stabilization.md).

## Scene Detection

Run scene-change detection on a clip from **Clip → Detect Scenes**. The result is a list of detected cuts, which can be applied as edits to split the clip at the detected boundaries. See [Scene Detection](./scene-detection.md).

## Copy / Paste Attributes

Copy effects, colour grades, or transform settings from one clip to others:

1. Select the source clip
2. **Clip → Copy Attributes**
3. Select the target clip(s)
4. Paste with **Ctrl+V**

Choose which attributes to paste: effects, colour, transform, audio, or all.

## Compound Clips

Group multiple clips into a compound clip that behaves as a single unit:

1. Select multiple clips on the timeline
2. Right-click → **Create Compound Clip**

Compound clips can be opened (double-click) to edit their internal timeline. Changes inside a compound clip are reflected everywhere it is used.

## Clip Properties

Select a clip and view its properties in the inspector's Info tab: source file, duration, resolution, codec, pixel format, detected colourspace, and timeline position.
