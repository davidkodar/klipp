# Transitions

Transitions provide smooth visual changes between adjacent clips on the timeline.

## Adding Transitions

To add a transition between two clips:

1. Position two clips adjacent to each other on the same track
2. Select the edit point (the boundary between the clips)
3. Apply a transition from the effects browser, or use **Ctrl+T** for the default cross dissolve

Both clips must have sufficient *handle media* — extra frames beyond the visible trim points — to create the transition overlap.

## Cross Dissolve

The cross dissolve is the default transition. It blends the outgoing clip into the incoming clip over the transition duration. The most commonly used transition in video editing.

- Select an edit point between two clips
- Press **Ctrl+T** or right-click → **Add Cross Dissolve**

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

Beyond cross dissolve, the effects browser ships:

- Dip to colour (configurable colour, default black)
- Slide
- Wipe (linear, radial, clock)
- Push
- Whip pan
- Cross zoom

All are GPU-rendered through the same compute shaders that drive the effects pipeline — no CPU fallback needed at typical preview resolutions.
