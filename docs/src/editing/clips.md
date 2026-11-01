# Working with Clips

Clips are the building blocks of your timeline. Each clip references a segment of imported media placed at a specific position and duration.

## Adding Clips to the Timeline

### Insert Edit (, or F10)
Places the source range at the playhead and pushes all later clips forward.

### Overwrite Edit (.)
Places the source range at the playhead, replacing whatever was there. No rippling.

### Fit to Fill, Place on Top, Replace
- **Fit to Fill** (Shift+F11) retimes the source range to fill the timeline's in–out range.
- **Place on Top** (F12) puts it on the first free track above the playhead.
- **Replace** (Shift+F12) swaps the clip under the playhead for it, keeping its span.

With an out point set and no in point, an edit is backtimed: it ends at the out point. See [Magnetic Timeline](./magnetic-timeline.md#three--and-four-point-editing).

### Drag and Drop
Drag clips from the media browser or the source viewer onto any track. The drop follows the timeline's INS / OVR mode.

## Trimming

Drag the left or right edge of a clip to trim it:

- **Ripple trim** — trims the clip and moves later clips to close the gap (with RIPPLE on)
- **Roll** — moves the edit point between two adjacent clips without moving other clips
- **Slip** — changes a clip's in and out points without moving it (Trim tool on the clip's body, or Alt-drag)
- **Slide** — moves a clip between its neighbours, keeping its in and out (Trim tool, Shift-drag)

**Q** and **W** trim the selected clip's tail or head to the playhead. **U** selects the edit point nearest the playhead; **[** and **]** then trim it a frame at a time (Shift for more), and **E** extends it to the playhead. The viewer shows the trim frame as you drag. See [Magnetic Timeline](./magnetic-timeline.md).

## Splitting

Press **B** for the Blade tool and click a clip to split it there, or press **Ctrl+B** to split the selected clips at the playhead. **Join Through Edit at Playhead** (Timeline menu) rejoins a split.

## Deleting

**Delete** removes the selected clips and closes the gap (with RIPPLE on). **Shift+Delete** lifts them, leaving a gap. **;** and **'** lift and extract the in–out range.

## Moving

- **Alt+,** / **Alt+.** nudge the selection a frame (Shift+Alt for more).
- **Alt+↑** / **Alt+↓** move it a track up or down.
- **Swap Selected Clips** (command palette) exchanges two clips.

## Connected Clips, Groups and Storylines

- **G** connects a clip to the clip under it on the primary track, so it moves with it; **Shift+G** disconnects it.
- **Ctrl+G** groups clips to select and move together; **Ctrl+Shift+G** ungroups.
- A clip's menu promotes connected clips to a storyline, or breaks one apart.

## Speed Changes

**Clip → Speed...** sets the clip's speed and reverse:

- Constant speed — preset percentages or a custom value; the clip's length changes to match
- Speed keys, in the Video tab's speed rows
- **Keep pitch** for the sound

The **Speed Ramp** effect blends the in-between frames of a slowed clip by nearest frame, a cross-fade, or **optical flow**, which follows the motion between source frames on the GPU. A slowed clip plays at the timeline's rate.

## Freeze Frame

**Alt+H**, or **Freeze Frame Here** in a clip's menu, holds the frame under the playhead.

## Enable / Disable and Solo

**Clip → Enable / Disable** switches a clip off without removing it; disabled clips are dimmed and excluded from playback and export. **Clip → Solo** plays only that clip.

## Matching a Clip to the One Below

A title, effect layer or sound's menu offers **Match Clip Below**: it takes the start and length of the clip under it, in one undo step. **Keep Matched to Clip Below** keeps it so as that clip is moved or trimmed; **Stop Keeping Matched** ends that.

## Stabilisation

Add the **Stabilisation** effect to a clip; its analysis starts at once. See [Stabilization](./stabilization.md).

## Scene Detection

**Timeline → Detect Scene Cuts** splits the selected clip where its picture cuts. See [Scene Detection](./scene-detection.md).

## Copy / Paste Attributes

Copy effects, colour grades, or transform settings from one clip to others:

1. Select the source clip and press **Ctrl+Shift+C** (Edit → Copy Attributes).
2. Select the target clip(s) and press **Ctrl+Shift+V** (Edit → Paste Attributes...).
3. Choose which attributes to paste.

**Copy Title Style** / **Paste Title Style** do the same for a title's look.

## Compound Clips

Select clips and choose **Timeline → Create Compound Clip**. Double-click a compound clip to edit inside it; **Ctrl+↑** leaves it. Changes inside are reflected everywhere it is used.

## Auditions

An audition holds several takes in one clip's place: other shots, other ranges of a file, or copies of the clip with a different grade, effect or title text. Right-click a clip and choose **Link & Group → Audition → Create Audition**: the clip becomes its first take and a copy of it the second. **Add Take From Bin** adds a file, and a file dragged from the media pool onto a clip with **Alt** held goes into it as a take.

An audition shows its count of takes on a badge, and the name of the take on the timeline after its own. Double-click it, or choose **Link & Group → Audition → Takes…**, to see its takes: click one to put it in, rename them, remove the ones you don't want, or **Finalize** to keep the take on the timeline as a plain clip. On the main storyline the clips after it move to fit a longer or shorter take; elsewhere they move only with Ripple on. To compare takes in place, click a take's **▶** in the picker, or **Preview All**: the take plays in its slot with its own sound, from 2 s before it to 2 s after, the clips after it moved as they would be. **←** and **→** play the previous or next take, **Enter** puts the one playing in (one undo step), and **Esc** stops and goes back to where you were. Nothing in the project changes while you compare; any edit, or Undo, ends the preview first. Each change is one undo step. The Final Cut Pro keyboard layout gives these its keys (Y, Ctrl+Y, Alt+Y, Shift+Alt+Y); in Klipp's own layout they have none until you set them.

## Multicam Clips

Select synced angles in the browser and choose **Create Multicam Clip** (Alt+C). Cut between angles with **1**–**9** while playing, or from **Clip → Switch Angle**; **Clip → Multicam Sound** picks where the sound comes from. See [Source Viewer](../ui/source-viewer.md#multicam-angles).

## Clip Properties

Select a clip and view its properties in the inspector's Info tab: source file, duration, resolution, codec, pixel format, detected colourspace, and timeline position.
