# Keyframes

Keyframes animate parameter values over time. Almost every parameter in Klipp — opacity, transform, colour wheels, grade node values, title layer properties, masks, audio volume, effect parameters — can be keyed.

## Adding a Keyframe

Click the diamond next to any parameter in the inspector, or press **Alt+K** for the parameter in focus: the current value at the playhead becomes a key. Move the playhead, change the value, and the parameter now moves between the two. A switch or a choice keys as a step. **Alt+←** / **Alt+→** go to the previous / next key.

Keys show in several places, all of them editable:

- The inspector's **◆ Keyframes** column, a strip beside each parameter
- The **Curve Editor** and its **Dope Sheet** (Shift+K)
- On the clip itself, with **Show Keys on Clip** in the clip's menu
- A clip's position keys as a **motion path** in the viewer
- A track's automation lane, and the sequence camera's lane

## Interpolation and Easing

Each key's curve to the next is **Linear** (the default), **Step (Hold)** — holding the value until the next key — or **Bezier**, shaped by handles. Before the first key and after the last, the value holds.

Easing presets set a key to Bezier with a named shape: **ease in**, **ease out**, **ease in-out**, and exponential, back (a small overshoot), elastic and bounce, each in, out or in-out. **Ctrl+Alt+I**, **Ctrl+Alt+O**, **Ctrl+Alt+E** and **Ctrl+Alt+L** set the selected keys to ease in, ease out, ease in-out and linear.

Each key also has a **handle kind**: **auto** (smooth, chosen for you), **clamped** (smooth without overshooting its neighbours), **aligned** (the two handles move together), **broken** (each handle on its own), **flat**, or **linear in / out**. The kind is saved with the key.

## The Curve Editor

**Window → Curve Editor** (Shift+K) shows the selected clip's keyed parameters on one time axis. With no clip selected it shows the sequence camera.

- **Several curves at once** — the list of shown curves on the left; **Draw this curve** adds one, **Stop showing this curve** removes it. Position X and Y can be shown as one.
- **Edit keys** — click to select, drag in time and value, Shift-click or drag a box to select several and edit them together. Dragged keys snap to frames, the playhead and other keys. Type a selected key's exact time and value.
- **Handles** — drag Bezier handles to shape the curve; in the **Speed** view, drag a key's influence handles to set how it eases.
- **Key tools** — previous / next key, key at the playhead, copy, paste and scale the selected keys.
- **Dope Sheet** — every animated parameter of the clip as a row of keys over time, to move keys and see timing at a glance.

The Curve Editor and the inspector share the parameter in focus: selecting a row in one shows it in the other.

## Copying and Reversing Keys

A key diamond's menu copies, pastes and reverses an effect parameter's keys. **Clear all keyframes** turns the parameter back into a single value: its value at the playhead.

## Behaviours (Modifiers)

A parameter's value at a given time is worked out in three steps: time modifiers remap the time, the keyframes are evaluated at that time, and value modifiers then change the result in the order they are stacked. The modifiers are `Modifier` in `crates/klipp-core/src/modifier.rs`, and the inspector lists them under **Behaviors**, on the video tab for Opacity, Position X/Y, Scale and Rotation and on the audio tab for Volume, Pan and Gain. The model can also carry a stack for an effect parameter.

Each modifier can be switched off without removing it. The ones that produce a value have a **blend** that says how it meets the value below it: *Add* (the default), *Multiply* or *Replace*.

| Behaviour | Parameters | What it does |
|---|---|---|
| **Oscillate** | `amplitude`, `frequency` (Hz), `phase` (fraction of a cycle), `waveform` (Sine, Square, Triangle, Sawtooth), blend | A periodic wave of the given amplitude |
| **Wiggle** | `amplitude`, `frequency` (Hz), blend | Smooth random motion: a new random value `frequency` times a second, interpolated between. The pattern is seeded by the clip, so it is the same every time the clip plays |
| **Throw** | `velocity`, `acceleration`, blend | `velocity × t + ½ × acceleration × t²` from the clip's start: a steady drift, or one that speeds up or slows down |
| **Ramp** | `start_value`, `end_value`, `start_norm`, `end_norm`, blend (a new Ramp starts as Replace; one saved without a blend loads as Add) | Goes from the start value to the end value between two points given as fractions of the clip's length (0 is its start, 1 its end), holding outside them |
| **Loop** | `period_secs` | Time modifier: repeats the keyframes' first `period_secs` seconds |
| **Ping Pong** | `period_secs` | Time modifier: plays the first `period_secs` seconds forward, then backward, and so on |
| **Clamp** | `min`, `max` | Keeps the value between two limits |

Time runs from the clip's start on the timeline. Keyframes and behaviours combine: a clip's opacity can be keyframed *and* oscillate around the keyframed value.

## Common Patterns

### Fade in / out
Two keyframes on opacity: 0 → 1 over the in duration, 1 → 0 over the out duration. Use the **ease in-out** preset for a natural feel.

### Ken Burns
Two keyframes on transform position + scale at clip start and end. **Linear** interpolation looks deliberate; **ease in-out** looks photographic. **Clip → Motion Presets** sets up common ones.

### Music ducking
Keys on the music track's volume around a voiceover: −6 dB before, −18 dB during, −6 dB after, eased in-out — or let **Auto Duck** do it.

### Chromakey edge clean-up
Keyframe the chroma-key tolerance over a tricky shot — relax tolerance during a fast-moving section, tighten during a static one.

## Performance

Keyframe evaluation is sub-microsecond per parameter. Klipp samples every animated parameter at the playhead per frame; with hundreds of keyframes across dozens of parameters, evaluation cost is negligible against the rest of the render pipeline.
