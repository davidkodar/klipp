# Working with Clips

Clips are the building blocks of your timeline. Each clip represents a segment of imported media placed at a specific position and duration.

## Adding Clips to the Timeline

### Append
Drag a clip from the media browser to the end of a track, or select it and the clip is placed after the last clip on the primary track.

### Insert Edit (W)
Places the clip at the playhead position and pushes all subsequent clips forward. This preserves the relative timing of everything after the insertion point.

### Overwrite Edit (D)
Places the clip at the playhead position, replacing whatever was there. Clips that overlap are trimmed or replaced. No rippling occurs.

### Drag and Drop
Drag clips from the media browser directly onto any track. The magnetic timeline snaps the clip into position.

## Trimming

Drag the left or right edge of a clip to trim it:

- **Ripple trim** -- Trims the clip and shifts subsequent clips to close the gap (default with magnetic timeline)
- **Roll trim** -- Adjusts the edit point between two adjacent clips without moving other clips

The viewer shows the trim frame in real time as you drag.

## Splitting (Blade Tool)

1. Press **B** to activate the blade tool
2. Click on a clip at the desired split point
3. The clip is split into two independent clips at that position

You can also split at the playhead with **Ctrl+B** while using the select tool.

## Ripple Delete

Select a clip and press **Del** to delete it. The magnetic timeline automatically closes the gap left behind.

## Speed Changes

Adjust clip playback speed from the **Clip > Speed** menu or the inspector:

- **Constant speed** -- 25%, 50%, 100%, 200%, 400%, or custom value
- Speed changes adjust the clip duration proportionally

## Enable/Disable (E)

Press **E** to toggle a clip between enabled and disabled. Disabled clips remain on the timeline but are excluded from playback and export. Disabled clips appear dimmed.

## Solo (S)

Press **S** to solo the selected clip. When soloed, only that clip plays back; all other clips on all tracks are temporarily muted. Press **S** again to unsolo.

## Copy/Paste Attributes

Copy effects, color grades, or transform settings from one clip to others:

1. Select the source clip
2. Go to **Clip > Copy Attributes**
3. Select the target clip(s)
4. Paste with **Ctrl+V**

Choose which attributes to paste: effects, color, transform, audio, or all.

## Compound Clips

Group multiple clips into a compound clip that behaves as a single unit:

1. Select multiple clips on the timeline
2. Right-click and choose **Create Compound Clip**

Compound clips can be opened (double-click) to edit their internal timeline. Changes inside a compound clip are reflected everywhere it is used.

## Clip Properties

Select a clip and view its properties in the inspector's Info tab: source file, duration, resolution, codec, and timeline position.
