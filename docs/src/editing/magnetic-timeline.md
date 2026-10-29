# Magnetic Timeline

Klipp's timeline is *magnetic* by default: clips close up, deletes and trims ripple, and inserts push later clips along. The **RIPPLE** button in the timeline toolbar turns gap-closing off, so deletes and trims leave gaps; **INS / OVR** (Shift+W) chooses whether edits and drops insert or overwrite.

## Edit Operations

| Operation | Shortcut | Behaviour |
|---|---|---|
| **Insert** | **,** or **F10** | Place the source range at the playhead; ripple later clips forward |
| **Overwrite** | **.** | Place it at the playhead; replace whatever overlaps |
| **Ripple Overwrite** | — | Overwrite, then ripple to the new length |
| **Fit to Fill** | **Shift+F11** | Retime the source range to fill the timeline's in–out range |
| **Place on Top** | **F12** | Put it on the first free track above |
| **Replace** | **Shift+F12** | Swap the clip under the playhead, keeping its span |
| **Lift** / **Extract** | **;** / **'** | Remove the in–out range, leaving or closing the gap |

## Three- and Four-Point Editing

An edit uses whichever of the four points are set: the source's in and out, and the timeline's in and out.

- The clip starts at the timeline's in point, or at the playhead when there is none.
- A timeline out point alone places the clip by its end: the edit is **backtimed**.
- With both timeline points, their span is the length, taken from the source in point, or back from a source out point set without an in.
- With all four points, **Fit to Fill** retimes the source range to the timeline range.

## Trim Operations

| Operation | How | Behaviour |
|---|---|---|
| **Ripple trim** | Drag a clip edge | Trim, moving later clips to close the gap (RIPPLE on) |
| **Roll** | Drag the junction between two clips (Trim tool) | Move the cut between them |
| **Slip** | Trim tool on the clip's body, or Alt-drag with Select | Change in and out without moving the clip |
| **Slide** | Trim tool + Shift-drag, or Alt+Shift-drag with Select | Move the clip between its neighbours, keeping its in and out |
| **Trim to playhead** | **Q** / **W** | Ripple-trim the tail / head to the playhead |
| **Extend edit** | **E** | Move the selected edit point to the playhead |
| **Trim a frame** | **[** / **]** (Shift for more) | Move the selected edit point (**U** selects the nearest) |

## Move Operations

| Operation | How | Behaviour |
|---|---|---|
| **Drag move** | Drag a clip's body | Snaps to neighbours and gaps |
| **Nudge** | **Alt+,** / **Alt+.** (Shift for more) | Move the selection by frames |
| **Change track** | **Alt+↑** / **Alt+↓** | Move the selection a track up or down |
| **Swap** | Swap Selected Clips (palette) | Exchange two clips' places |

Picture and sound move together while **Linked Selection** is on; hold **Alt** at the start of a drag to move one alone.

## Delete Operations

| Operation | Shortcut | Behaviour |
|---|---|---|
| **Ripple delete** | **Delete** | Remove the clip; close the gap |
| **Lift delete** | **Shift+Delete** | Remove the clip; leave a gap |
| **Close Gap** | Clip menu | Close the gap before a clip |

With several clips selected, Delete and Shift+Delete take every one of them, on any track, and one undo puts them all back. Dragging any of them moves them all by the same time, each drawn where it will land; dragged to another track, each goes the same number of tracks of its kind up or down, and the whole drag is refused when one would land on a locked or missing track. Selected clips on the primary storyline move as one block, rippled into the place the drag shows (or overwriting there with Alt or OVR), while the rest of the selection moves by the same time on its own tracks. Off the magnetic tracks a removed clip always leaves its gap.

## Snapping

Toggle snapping with **N**. Clips snap to the playhead, other clip edges on any track, in and out points and markers; right-click the snap button to choose the targets or a grid. A snap holds until the pointer is half as far again past the snap distance, and the clip lands where the snap line showed. Hold **Shift** while dragging to skip snapping for that drag (**Ctrl** in a slide). An edit made at the playhead or an In point, such as a gap or a source insert, goes in exactly there, not on a nearby cut. See [Snapping](../ui/timeline.md#snapping).

## Ripple Across Tracks

A ripple moves every track: a clip moved on V1 moves its linked sound and the clips on other tracks that come after it. **Lock** a track to leave it alone.
