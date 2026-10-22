# Generators

Generators are clips that Klipp draws rather than decodes: backgrounds, test signals, timing displays, slates and shapes. They live on a video track like any other clip.

## The Library

**Library → Generators** holds fifteen generators in five groups. Every card is rendered by the generator renderer itself, so a slate card is the slate you get and a countdown card counts while you hover it.

| Group | Generators |
|---|---|
| Backgrounds | Solid Colour, Gradient, Four-Corner, Living Gradient, Grain |
| Test | SMPTE 75% bars, SMPTE 100% bars |
| Timing | Countdown, Timer Up, Timer Down |
| Slate | Slate, Placeholder |
| Shape | Polygon, Circle, Star |

There is one generator per job. Its colours, counts and sizes are parameters in the inspector, so a red matte is the Solid Colour with its colour changed, and a triangle is the Polygon with three sides.

## Adding a Generator

Double-click a card, or drag it onto the timeline; **Timeline → Add Generator** adds a solid colour, colour bars or black directly. The generator lands on a video track at the playhead with a five-second duration, becomes the selection, the inspector opens on its parameters, and the playhead moves a little way in so the viewer shows it.

## Living Backgrounds

A still gradient behind a title looks like a slide; a moving one looks designed. **Living Gradient** turns its angle slowly and drifts a soft light across the frame; the drift speed and the light's strength are parameters. **Grain** is film grain on a colour, refreshed at a chosen rate, for texture behind titles or as a plate to mix over footage.

## Placeholder

A hatched card with a label in the centre, for a shot that is not cut yet. Set the label to what the shot will be, so the timeline reads correctly before the footage arrives.
