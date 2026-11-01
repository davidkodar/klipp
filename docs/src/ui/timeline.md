# Timeline

The timeline is the primary editing surface where you arrange, trim, and organize your clips.

## Toolbar

The timeline's own toolbar sits above the sequence tabs and holds every control that changes how an edit behaves: the edit tools, the INS / OVR edit mode, ripple, snap and skimming, the marker and title buttons and the zoom. See [Toolbar](toolbar.md#timeline-toolbar).

## Tracks

The timeline holds video, audio and subtitle tracks:

- **Video tracks** stack from bottom to top. Higher tracks composite over lower tracks.
- **Audio tracks** mix together. Each track has its own volume and pan, and can carry automation for both.
- **Subtitle tracks** hold captions, with a language and a caption style each.
- Add tracks from **Timeline → Add Video Track** / **Add Audio Track**, or from a track header's menu.

## Clips

Clips on the timeline display:

- **Filmstrip thumbnails** on video clips
- **Audio waveforms** on audio clips and linked audio
- A **colour label** for visual identification
- The clip name, and badges for speed, effects and keys

A clip's menu offers **Show Keys on Clip**: any keyed parameter of its effects, grade or title layers drawn as a line on the clip, where keys are clicked, Alt+clicked to add, dragged in time and value, and deleted with Delete.

## Playhead

The playhead is a vertical line marking the current position. Click the ruler or the track area to move it. During playback it moves smoothly against the master clock.

## Render Status

A thin strip along the top of the ruler shows where the timeline stands in the render cache: **green** where frames are rendered and play from the cache, **amber** where they are heavy to play and not rendered, and **dashed** where something rendered has changed since and plays live until it is rendered again. Light frames that aren't rendered show nothing.

While you pause, Klipp renders the heavy parts ahead on its own (Preferences > Playback, **Render heavy parts while idle**), and **Render In to Out** renders a range now. Exports always render live, from the media.

## Magnetic Timeline

Clips in Klipp's timeline close up by default: moving or deleting a clip ripples the clips after it, and insert, overwrite, slip, slide, roll and replace edits keep the edit continuous. The **RIPPLE** button turns gap-closing off. See [Magnetic Timeline](../editing/magnetic-timeline.md).

## Snapping

Toggle snapping with **N**. When enabled, every drag snaps the same way: moving and trimming clips, rolling and sliding edits, the playhead, the range tool, the blade and drops from the browser. An edge snaps to the playhead, other clips' edges on any track, in/out points, markers and the sequence start, and to the grid when one is chosen. The clips that move with the dragged one (its linked sound, its group) and the selected clips are never targets.

A snap shows at once: the clip jumps to the point and the snap line appears with the target's name. It then holds until the pointer is half as far again past the snap distance, so a small movement of the hand doesn't let it go, and the drop lands exactly where it showed. Hold **Shift** to drag free, on the nearest frame; in a slide, which Shift starts, hold **Ctrl** instead.

Right-click the snap button to choose the targets and the grid: Off, Ruler ticks, Frames, 0.5 s, 1 s, 2 s or 5 s. The snap threshold is in Preferences, in points on screen.

## Waveforms

Audio tracks and linked audio display waveforms. **Show Channels Separately** on an audio track's header draws each clip's left channel above its right.

## Zoom and Navigation

- **=** / **-** — Zoom in/out
- **Z** — Fit the whole timeline; **Shift+Z** zooms to the selection
- **Scroll wheel** — Horizontal scroll (or zoom with Ctrl held)
- **Home** / **End** — Jump to the start / end
- **↑** / **↓** — Previous / next edit point

## Track Headers

Each track header shows its name (double-click to rename), lock, mute and solo, and, on audio, the arm button for recording. Its menu adds:

- Track colour and height (Small, Medium, Large, Expanded)
- Add a track above or below, move it, remove it (with or without its clips)
- **Show automation lane** — the track's volume and pan keys, drawn and edited in a lane under it
- **Set as Primary** — the track the cut is built on
- On subtitle tracks, the **Language**, **Translate Captions**, and show or hide
- On video tracks, **Bounce to Media File…** (see below)

Drag a header to reorder tracks.

### Bounce to Media File

**Bounce to Media File…** draws a video track alone into a file and puts a clip of it in the track's place. A heavy track becomes one clip, which plays like any other and opens in other editors.
- **What goes in:** the track from its first clip to its last, at the sequence's full size, with its transparency. Gaps between clips stay transparent and silent.
- **Baked in:** the track's effects and each clip's opacity.
- **Kept editable:** the track's opacity and the clips' blend mode, which carry over to the new track and clip.
- **Sound:** the sound linked to its clips goes in too, unless you turn it off in the dialog.

Choose **ProRes 4444** (the default: plays in other editors, up to about 2 GB a minute at 1080p) or **FFV1** (lossless and larger). The file is kept in the project's `media/bounces` folder, so save the project first.

When it is written, the clip goes on a new track above, with its sound on a new track linked to it; the original track is hidden and its linked sound silenced. One undo takes all of it back and leaves the file.

**Highlights above white are clipped; grade before bouncing.** The file is display-referred, as an export is.

Bouncing is SDR only for now: an HDR or wide-gamut project says why it can't. Also refused, saying why:
- a track whose frames read other tracks (an adjustment layer, a matte, captions);
- clips that blend in different ways;
- not enough free disk for the file.

## Track Automation

An audio track's volume and pan can be keyed over time, played and exported. The automation lane shows the keys on a fader law in dB, down to silence. Each track's automation mode, clicked through in the Audio Mixer, is **Off** (the controls as set), **Read** (play the keys), **Touch** (record while a control is held), **Latch** (record from its first touch until playback stops) or **Write** (record the whole pass). Each recorded pass is one undo step.

## Compound Clips

Double-click a compound clip to edit inside it; a breadcrumb over the timeline shows where you are, and **Ctrl+↑** leaves it.

## Stacked Selects

A sequence tab's menu offers **Open as Selects**: that sequence is shown as a strip above the cut. Mark a range or select clips in it and drag them into the cut, or insert or overwrite them. Click **Edit** on the strip to work in it as a full timeline stacked above the cut; **Alt+`** swaps which of the two you are working in, and a mark along the top shows which one it is. Clips move between the two only with a clip-moving tool, or with Ctrl+drag.

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
