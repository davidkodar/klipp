# Transitions

Transitions provide smooth visual changes between adjacent clips on the timeline.

## The Library

**Library → Transitions** shows every transition as a card rendered by the same code the timeline uses, between two frames of your own footage: the posters of the first two clips in the media pool, or one poster and a darker copy of it when the project has a single clip. A card at rest shows the midpoint of the transition; hover it to play the whole move. Slides and wipes keep a small arrow for their direction.

## Adding Transitions

To add a transition between two clips:

1. Position two clips adjacent to each other on the same track
2. Select the edit point (the boundary between the clips)
3. Drag a transition from the Transitions tab of the browser onto the edit point

Both clips must have sufficient *handle media* — extra frames beyond the visible trim points — to create the transition overlap.

## Cross Dissolve

The cross dissolve is the default transition. It blends the outgoing clip into the incoming clip over the transition duration. The most commonly used transition in video editing.

- Select an edit point between two clips
- Press **Ctrl+T** or right-click → **Add Cross Dissolve**

## Smooth Cut

A smooth cut hides a jump cut between two takes of one framing, such as an interview where a few words were cut out. It holds the last frame before the cut and morphs it into the first frame after it by following the motion between the two, so a head that moved a little slides into place instead of jumping.

- The picture holds still while it morphs, so a smooth cut is kept short: at most a third of a second when added from the Library.
- It needs no footage past the cut.
- When both clips have framing analysis, a smooth cut is refused where the faces differ in number, move or change size too much between the two frames. Without the analysis it is added, and says it wasn't checked.

## Transition Duration

The default duration is 30 frames (≈ 1 second at 30 fps). Adjust by:

- Dragging the edges of the transition on the timeline
- Selecting the transition and entering a duration in the inspector
- Changing the default in **Edit → Preferences → General**

## Behaviour Under Limited Handles

If a clip does not have enough handle media for the requested duration, Klipp will:

- Shorten the transition to fit available media
- Display a warning indicator on the transition

This avoids the alternative — silently freezing on the last available frame — which produces invisible bugs in finished cuts.

## Removing Transitions

Select a transition on the timeline and press **Del** to remove it. Clips return to a straight cut at the original edit point.

## Per-Type Catalogue

Sixty-seven transitions ship, in the Transitions tab of the browser.
The ones reached for most:

- Dip to colour (configurable colour, default black)
- Slide
- Wipe (linear, radial, clock)
- Push
- Cross zoom

All are GPU-rendered through the same compute shaders that drive the effects pipeline — no CPU fallback needed at typical preview resolutions.
