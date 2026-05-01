# Keyframes

Keyframes animate parameter values over time. Almost every parameter in Klipp — opacity, transform, colour wheels, audio volume, EQ bands, effect parameters — is keyframable.

## Adding a Keyframe

Click the diamond icon next to any parameter in the inspector. The current value at the playhead becomes a keyframe. Move the playhead, change the value, and click the diamond again to add a second keyframe — the parameter now interpolates between the two.

## Interpolation Curves

Each keyframe has an *interpolation* setting that controls how the value transitions to the next keyframe:

| Curve | Shape | Use for |
|---|---|---|
| **Constant** | Step (no interpolation) | On/off toggles, discrete state |
| **Linear** | Straight line | Steady changes, no acceleration |
| **Ease In** | S-curve (slow start) | Soft entrances |
| **Ease Out** | S-curve (slow end) | Soft exits |
| **Ease In/Out** | S-curve both ends | Natural acceleration + deceleration |
| **Bezier** | User-tweaked control points | Custom shapes |

Interpolation is set on the *outgoing* keyframe — i.e. the keyframe whose curve takes you to the next one.

## Keyframe Editor

Open the per-parameter keyframe graph by clicking the small graph icon next to the parameter. The editor shows time on the X-axis, value on the Y-axis, and the keyframe sequence as control points connected by their interpolation curve.

Drag points to move them. Drag bezier handles to reshape the curve. Right-click a point to change interpolation type or delete.

## Modifier and Behaviour Architecture

Keyframes are the leaf nodes of a wider *modifier* / *behaviour* architecture. A modifier is anything that mutates a parameter value at a given time:

- **Constant** — a fixed value
- **Keyframed** — interpolated between keyframes
- **Expression** — driven by a JS-like expression (e.g. `time * 30`)
- **Linked** — copies another parameter's value (e.g. clip B's opacity follows clip A's)

A behaviour is a modifier that depends on engine state (e.g. *follow audio level*). Behaviours and modifiers stack — a clip's opacity could be keyframed *and* multiplied by a behaviour that responds to scene cuts.

Today, only Constant, Keyframed, and a small set of behaviours (Wiggle, Audio Follow) are exposed in the UI. The rest is plumbing for future expression / linking work.

## Common Patterns

### Fade in / out
Two keyframes on opacity: 0 → 1 over the in duration, 1 → 0 over the out duration. Use **Ease In/Out** for natural feel.

### Ken Burns
Two keyframes on transform position + scale at clip start and end. **Linear** interpolation looks deliberate; **Ease In/Out** looks photographic.

### Music ducking
Two keyframes on the music track's volume around a voiceover: -6 dB before VO, -18 dB during, -6 dB after. **Ease In/Out** for smooth duck.

### Chromakey edge clean-up
Keyframe the chroma-key tolerance over a tricky shot — relax tolerance during a fast-moving section, tighten during a static one.

## Performance

Keyframe evaluation is sub-microsecond per parameter. Klipp samples every animated parameter at the playhead per frame; with hundreds of keyframes across dozens of parameters, evaluation cost is negligible against the rest of the render pipeline.

## Removing All Keyframes

Right-click the diamond icon → **Remove all keyframes** to convert the parameter back to a single constant value (the value at the playhead at the time of removal).
