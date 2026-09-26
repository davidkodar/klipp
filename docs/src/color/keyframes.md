# Keyframes

Keyframes animate parameter values over time. Almost every parameter in Klipp — opacity, transform, colour wheels, audio volume, EQ bands, effect parameters — is keyframable.

## Adding a Keyframe

Click the diamond icon next to any parameter in the inspector. The current value at the playhead becomes a keyframe. Move the playhead, change the value, and click the diamond again to add a second keyframe — the parameter now interpolates between the two.

## Interpolation Curves

Each keyframe has an *interpolation* setting that controls how the value transitions to the next keyframe. There are three (`Interpolation` in `crates/klipp-core/src/keyframe.rs`):

| Interpolation | Shape | Use for |
|---|---|---|
| **Linear** | Straight line; the default | Steady changes, no acceleration |
| **Step** | Holds the value until the next keyframe (shown as *Step (Hold)* or *Hold*) | On/off toggles, discrete state |
| **Bezier** | Cubic curve shaped by the keyframes' in and out tangents | Eases and custom shapes |

Interpolation is set on the *outgoing* keyframe — i.e. the keyframe whose curve takes you to the next one. Before the first keyframe and after the last, the value holds.

### Ease presets

Ease In, Ease Out and Ease Both (Ease In-Out) are not interpolation types of their own. Choosing one from a keyframe's right-click menu sets the keyframe to **Bezier** and writes its tangents, scaled to the length of the segment, using the CSS `cubic-bezier` curves:

| Preset | Curve | Shape |
|---|---|---|
| **Ease In** | (0.42, 0, 1, 1) | Starts slowly and arrives at speed |
| **Ease Out** | (0, 0, 0.58, 1) | Leaves at speed and settles |
| **Ease Both** | (0.42, 0, 0.58, 1) | Slow at both ends |

The camera lane and the assistant's motion tools use the same model (`Ease` in `keyframe.rs`), which adds two more curves: *expo out* (0.16, 1, 0.3, 1), a fast arrival that settles long, and *back out* (0.34, 1.56, 0.64, 1), which overshoots a little and settles back. *Bezier (Custom)* keeps whatever tangents you drag in the keyframe editor.

## Keyframe Editor

Open the per-parameter keyframe graph by clicking the small graph icon next to the parameter. The editor shows time on the X-axis, value on the Y-axis, and the keyframe sequence as control points connected by their interpolation curve.

Drag points to move them. Drag bezier handles to reshape the curve. Right-click a point to change interpolation type or delete.

## Behaviours (Modifiers)

A parameter's value at a given time is worked out in three steps: time modifiers remap the time, the keyframes are evaluated at that time, and value modifiers then change the result in the order they are stacked. The modifiers are `Modifier` in `crates/klipp-core/src/modifier.rs`, and the inspector lists them under **Behaviors**, on the video tab for Opacity, Position X/Y, Scale and Rotation and on the audio tab for Volume, Pan and Gain. The model can also carry a stack for an effect parameter.

Each modifier can be switched off without removing it. The ones that produce a value have a **blend** that says how it meets the value below it: *Add* (the default), *Multiply* or *Replace*.

| Behaviour | Parameters | What it does |
|---|---|---|
| **Oscillate** | `amplitude`, `frequency` (Hz), `phase` (fraction of a cycle), `waveform` (Sine, Square, Triangle, Sawtooth), blend | A periodic wave of the given amplitude |
| **Wiggle** | `amplitude`, `frequency` (Hz), blend | Smooth random motion: a new random value `frequency` times a second, interpolated between. The pattern is seeded by the clip, so it is the same every time the clip plays |
| **Throw** | `velocity`, `acceleration`, blend | `velocity × t + ½ × acceleration × t²` from the clip's start: a steady drift, or one that speeds up or slows down |
| **Ramp** | `start_value`, `end_value`, `start_norm`, `end_norm`, blend (Replace by default) | Goes from the start value to the end value between two points given as fractions of the clip's length (0 is its start, 1 its end), holding outside them |
| **Loop** | `period_secs` | Time modifier: repeats the keyframes' first `period_secs` seconds |
| **Ping Pong** | `period_secs` | Time modifier: plays the first `period_secs` seconds forward, then backward, and so on |
| **Clamp** | `min`, `max` | Keeps the value between two limits |

Time runs from the clip's start on the timeline. Keyframes and behaviours combine: a clip's opacity can be keyframed *and* oscillate around the keyframed value.

## Common Patterns

### Fade in / out
Two keyframes on opacity: 0 → 1 over the in duration, 1 → 0 over the out duration. Use the **Ease Both** preset for a natural feel.

### Ken Burns
Two keyframes on transform position + scale at clip start and end. **Linear** interpolation looks deliberate; **Ease Both** looks photographic.

### Music ducking
Two keyframes on the music track's volume around a voiceover: -6 dB before VO, -18 dB during, -6 dB after. **Ease Both** for a smooth duck.

### Chromakey edge clean-up
Keyframe the chroma-key tolerance over a tricky shot — relax tolerance during a fast-moving section, tighten during a static one.

## Performance

Keyframe evaluation is sub-microsecond per parameter. Klipp samples every animated parameter at the playhead per frame; with hundreds of keyframes across dozens of parameters, evaluation cost is negligible against the rest of the render pipeline.

## Removing All Keyframes

Right-click the diamond icon → **Remove all keyframes** to convert the parameter back to a single constant value (the value at the playhead at the time of removal).
