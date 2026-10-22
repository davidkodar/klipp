# Source Viewer

The source viewer is a second viewer for reviewing source media without affecting the program viewer or the timeline: review a clip in one viewer, see the program in the other, and insert or overwrite to move material between them.

It is shown beside the program viewer while **View → Dual Viewers** is on; with a single viewer, the Source / Program switch under the viewer brings it up.

## Workflow

1. Click a clip in the media browser. It loads into the source viewer.
2. Scrub or play it to find the section you want.
3. Press **I** to mark an in point, **O** for an out point.
4. Park the timeline's playhead where the section should land, then press **,** (insert) or **.** (overwrite), or use the viewer's buttons. **Place on Top** (F12), **Replace** (Shift+F12) and **Fit to Fill** (Shift+F11) work from the same marks.
5. Or drag the marked section from the viewer onto a track.

The source viewer keeps its clip, marks and playhead until you load another; they are independent of the timeline's.

## Logging in the Source Viewer

- **M** adds a source marker on the media.
- **Shift+F** marks the range as a favourite and **Shift+R** as rejected.
- **Subclip** turns the marked range into a media item of its own.
- **Match Frame** (F) loads the source of the frame under the timeline's playhead; **Shift+F** in the program viewer goes back to the timeline.

## Multicam Angles

**View → Multicam Angles** (Shift+Alt+M) shows a multicam clip's angles as a grid in the source viewer. Click an angle, or press **1**–**9**, to cut to it while playing.

## Skim and Playback

Scrubbing the source viewer uses Klipp's skim decode path: sparse decodes and coalesced seeks, so rapid scrubbing keeps up on 4K sources. Pressing **Space** plays it through the full playback path.

## Display

The source viewer takes the same overlays as the program viewer (safe areas, grid, zebras, false colour). It shows the source as decoded, without the clip's effects or grade: it is for judging the material, not the graded output.
