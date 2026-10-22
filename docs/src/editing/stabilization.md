# Stabilization

Klipp ships motion stabilisation for handheld and gimbal-residual footage, and two related clean-ups: rolling shutter correction and deflicker. Each is an effect that analyses the clip's motion or brightness first, then corrects each frame from that analysis on the GPU, in the viewer and the export.

## Workflow

1. Select a clip on the timeline.
2. Add **Stabilisation** from **Library → Video FX** (drag it onto the clip, or double-click it).
3. The analysis starts at once and runs in the background, with its progress shown.
4. When it finishes, the clip plays stabilised, and the effect's card in the Effects tab takes its settings.

The analysis runs once per clip. Changing the settings afterwards re-warps from the stored motion; nothing is analysed again.

## Controls

| Control | Default | What it does |
|---|---|---|
| **Smoothing** | 0.5 | 0 follows the camera's own path; 1 smooths it the most |
| **Crop** | 0.05 | How much of the edge is given up to hide the borders the warp reveals (0–0.2) |
| **Zoom to Fill** | on | Scale up so the frame is always filled; off shows the warped frame with black where borders show |

Like any effect, it can be turned off with its eye to compare, and mixed or masked.

## Rolling Shutter Correction

A CMOS sensor reads its rows one after another, so fast pans lean and wobble. **Rolling Shutter Correction** straightens them from the clip's motion; **Readout** is the share of the frame time the sensor takes to read, and **Strength** how much is corrected.

## Deflicker

**Deflicker** evens out brightness that pulses from frame to frame — timelapses, lights on mains frequency. **Strength** sets how much, over a **Window** of 0.2 to 4 seconds.

## When to Stabilise

Good candidates:

- Handheld footage with general camera shake
- Gimbal residue (low-frequency drift the gimbal didn't catch)
- Vehicle-mounted shots and walking shots

Bad candidates:

- Locked-off shots (nothing to stabilise; tracking noise can add a subtle warp)
- Heavy zoom-ins (tracking degrades when the frame content changes quickly)
- A moving subject filling the frame (the stabiliser may lock to the subject and move the background)

## Order with Other Effects

The analysis lands on the clip that was analysed, and the correction runs as a node in the clip's effect stack, so effects after it see the stabilised frame.
