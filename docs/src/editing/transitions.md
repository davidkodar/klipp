# Transitions

Transitions provide smooth visual changes between adjacent clips on the timeline.

## Adding Transitions

To add a transition between two clips:

1. Position two clips adjacent to each other on the same track
2. Select the edit point (the boundary between the clips)
3. Apply a transition from the effects browser or use the default transition shortcut

Both clips must have sufficient handle media (extra frames beyond the visible trim points) to create the transition overlap.

## Cross Dissolve

The cross dissolve is the default transition. It gradually blends the outgoing clip into the incoming clip over the transition duration. This is the most commonly used transition in professional editing.

To apply the default cross dissolve:
- Select an edit point between two clips
- Press **Ctrl+T** or right-click and choose **Add Cross Dissolve**

## Transition Duration

The default transition duration is 30 frames (approximately 1 second at 30fps). You can adjust the duration by:

- Dragging the edges of the transition on the timeline
- Selecting the transition and entering a duration in the inspector
- Changing the default duration in **Edit > Preferences > General**

## Transition Behavior

Transitions overlap the outgoing and incoming clips. The overlap region uses frames from each clip's handle (the media beyond the visible in/out points).

If a clip does not have enough handle media for the requested transition duration, Klipp will:
- Shorten the transition to fit available media
- Display a warning indicator on the transition

## Removing Transitions

Select a transition on the timeline and press **Del** to remove it. The clips return to a straight cut at the original edit point.
