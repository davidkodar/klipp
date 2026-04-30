# Remove Background

Single-button background removal. Klipp runs the entire frame through BiRefNet Lite and produces a foreground/background mask with no clicks required.

## Workflow

1. Park the playhead on the frame you want to mask.
2. **Alt+A** to open the AI Tools panel.
3. Click the **Remove Background** tile.
4. Click **Run on current frame**.
5. When complete, click **Apply to clip**.

That's it. The mask is written to the cache and an `ExternalMatte` node is wired into the clip's first Corrector — see [Apply to Clip](./overview.md#apply-to-clip) for the full chain.

## Default Model

**BiRefNet Lite** — Swin-Tiny backbone, 224 MB, MIT licence, single-input single-output mask. Trained on diverse subjects (people, animals, products, vehicles) so it generalises well across content types.

To override, expand the **Pick model** dropdown on the model card. Any model with the `mask_generator` kind and an empty `prompts` field is eligible.

## Inference Time

| Backend | RTX 4080 | RTX 3070 | 8-core CPU (no GPU) |
|---|---|---|---|
| ort + CUDA EP | ~250 ms | ~420 ms | n/a |
| ort + CPU EP | n/a | n/a | ~5–8 s |

The first run after launch includes a one-time warm-up of around 1.5 s for graph optimisation; subsequent inferences hit the cached graph.

## Tuning

The **Advanced** dropdown exposes:

- **Mask threshold** — the binarisation cut-off. Default 0.5. Lower values keep more of the foreground (including soft edges); higher values produce a tighter, more confident mask.
- **Edge feathering** — Gaussian blur applied to the mask before applying. Default 0 px. Use 1–3 px for a softer composite, especially against busy backgrounds.
- **Output format** — *Alpha matte* (default; matte channel = luminance) or *Inverted matte* (mask the background instead of the foreground).

## When to Use

Remove Background is best for:

- Talking-head footage on a clean or moderately busy background
- Product shots
- B-roll where you want the subject on a transparent track for compositing

It is less well suited for:

- Hair detail or transparent objects (glass, fluids) — falls back to a hard edge
- Multi-subject scenes where you only want one person — use **Isolate Object** instead
- Heavy motion blur — re-run per frame or use a video-aware model when SAM 2 is available

## Tips

- **Apply once, refine in colour** — the inserted matte node feeds the first Corrector. Adjust the corrector's qualifier softness or matte blur in the colour panel without re-running inference.
- **Re-run before applying** — the mask preview heatmap lets you flip between Overlay / Mask only / Cutout to judge quality before committing. If the mask misses an arm or grabs background, tweak the threshold and re-run.
- **Per-frame or per-clip** — Remove Background runs frame-by-frame. For a moving subject, mask each keyframe and rely on the colour Corrector chain to interpolate.
