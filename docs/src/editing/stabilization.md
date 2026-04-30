# Stabilization

Klipp ships motion stabilisation for handheld and gimbal-residual footage. The stabiliser tracks features across frames, builds a motion trajectory, smooths it, and warps each frame to match the smoothed trajectory.

## Workflow

1. Select a clip on the timeline
2. **Clip → Stabilize** (or *Stabilize Selected Clip* in the command palette)
3. Klipp analyses the clip — a progress bar shows percent complete
4. When complete, the inspector's **Stabilization** section becomes interactive

Analysis is one-time per clip. Adjusting smoothness afterwards is instant — Klipp re-warps from the cached motion data, no re-analysis needed.

## Performance

The stabiliser runs on the GPU through the `SequentialFrameDecoder`, which keeps frames resident on the GPU between analysis steps. On an RTX 4080:

- 4K HEVC, 60 seconds: ~18 seconds analysis
- 1080p H.264, 60 seconds: ~6 seconds analysis

A previous implementation took 10–15 minutes for the same 4K clip; the current decoder + GPU pipeline is roughly 30× faster.

## Controls

After analysis, the inspector exposes:

| Control | Default | Range |
|---|---|---|
| **Bypass** | off | toggle — when on, the stabilisation is computed but not applied (useful to A/B compare) |
| **Smoothness** | 0.6 | 0.0 (no smoothing, just tracking) to 1.0 (maximum smoothing) |
| **Crop strategy** | Auto | Auto / Fixed crop / Black borders |
| **Show jitter %** | off | UI overlay showing residual jitter per frame |
| **Trajectory graph** | off | UI overlay showing X/Y/rotation trajectory |

The jitter % indicator is meant to build trust — it shows how much motion remains after stabilisation, so you can judge whether the result is over- or under-smoothed without watching the clip end-to-end.

## Crop Strategies

Stabilisation needs to crop the frame because warping reveals the borders. Three strategies:

- **Auto** — Klipp picks a crop that hides borders for the analysed trajectory
- **Fixed crop** — apply a constant percentage crop regardless of trajectory
- **Black borders** — show the warped frame at full extent with black where borders are revealed; useful for VFX workflows where the borders will be replaced

## When to Stabilise

Good candidates:

- Handheld footage with general camera shake
- Gimbal residue (low-frequency drift the gimbal didn't catch)
- Vehicle-mounted shots
- Walking shots in narrative work

Bad candidates:

- Locked-off shots (no motion to stabilise; will produce subtle warp artefacts from feature mis-tracking)
- Heavy zoom-in shots (feature tracking degrades when frame content changes rapidly)
- Footage with a moving subject filling the frame (the stabiliser may lock to the subject and create motion in the background)

## Cache

Motion data is cached in the project under `analysis/stabilization/<clip_id>.json`. The cache survives project save / load. Re-analysing replaces the cache.

To clear the cache and re-analyse: select the clip → **Clip → Re-analyze Stabilization** from the command palette.

## Combine with Optical Flow Effects

Stabilisation runs *before* the effect chain, so any blur, sharpen, or transform you stack on top sees the stabilised frame. This is the order most editors expect — the effect chain operates on what looks like a tripod-mounted shot.
