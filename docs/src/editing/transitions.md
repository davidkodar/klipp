# Transitions

Transitions provide smooth visual changes between adjacent clips on the timeline.

## The Library

**Library → Transitions** shows every transition as a card rendered by the same code the timeline uses, between two frames of your own footage: the posters of the first two clips in the media pool, or one poster and a darker copy of it when the project has a single clip. A card at rest shows the midpoint of the transition; hover it to play the whole move. Slides and wipes keep a small arrow for their direction.

## Adding Transitions

- Drag a transition from **Library → Transitions** onto the cut between two clips.
- Select a clip next to a cut and press **Shift+T** for the default transition (a cross dissolve), or use the **Transition** button in the timeline toolbar.
- **Alt+T** adds an audio crossfade.

A transition needs *handle media* — frames beyond the visible trim points — on both sides of the cut. A dissolve into or out of a generator or a title needs none. A transition stays with the clips it joins when they move.

## Smooth Cut

A smooth cut hides a jump cut between two takes of one framing, such as an interview where a few words were cut out. It holds the last frame before the cut and morphs it into the first frame after it by following the motion between the two, so a head that moved a little slides into place instead of jumping.

- The picture holds still while it morphs, so a smooth cut is kept short: at most a third of a second when added from the Library.
- It needs no footage past the cut.
- When both clips have framing analysis, a smooth cut is refused where the faces differ in number, move or change size too much between the two frames. Without the analysis it is added, and says it wasn't checked.

## Transition Duration

The default duration is 24 frames. Adjust it by dragging the transition's edges on the timeline or in the inspector; change the default in **Preferences → Editing**. Where a clip has too little handle media for the duration, the transition is shortened to fit.

## Removing Transitions

Select a transition on the timeline and press **Del** to remove it. Clips return to a straight cut at the original edit point.

## Catalogue

Seventy-three transitions ship, grouped as dissolves, wipes, slides, zooms, cinematic and effect transitions. Among them:

- Cross dissolve, and dip to colour (black, white or any colour)
- Slides, pushes and wipes (linear, radial, clock)
- Zooms and cross zoom
- **Piece transitions** — the picture breaks into strips, panels, halves or shards that slide, tilt, turn, fall or shrink away, in an order and with an easing you choose. A piece transition can also cut round the subject, by its tracked matte, so the subject moves as a piece of its own.

All are drawn on the GPU.
