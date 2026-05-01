# Isolate Object

Click-to-mask a specific object or region. Klipp uses MobileSAM under the hood — a tiny ViT image encoder (~10 MB) plus the SAM prompt encoder and mask decoder. Three prompt types are supported.

## Workflow

1. Park the playhead on the frame.
2. **Alt+A** to open the AI Tools panel.
3. Click the **Isolate Object** tile.
4. Choose a prompt mode in the prompt tabs:
   - **Point** — single click for positive ("include"), Alt+click for negative ("exclude")
   - **Box** — drag a bounding box around the object
   - **Scribble** — draw a freehand region
5. Place your prompts in the viewer.
6. Click **Run on current frame** — the model returns a mask shaped to your prompts.
7. Tweak prompts and re-run, or click **Apply to clip**.

## Default Model

**MobileSAM** — TinyViT image encoder + SAM prompt encoder + mask decoder. Apache-2.0 licence. Roughly 40 MB on disk. Loaded through `candle` (pure Rust, no ONNX).

Because MobileSAM is a candle model, NVIDIA GPU acceleration requires the build to include the `ai-candle-cuda` feature (see [Installation](../getting-started/installation.md#cargo-aliases)). On a CPU build, MobileSAM still works — it is small enough that single-frame inference completes in ~1 second.

## Prompt Modes

### Point
The most common workflow. One positive click is often enough. Add negative clicks (Alt+click) to exclude background that the first click accidentally included.

```
+   positive (include in mask)
×   negative (exclude from mask)
```

The mask updates *after* you click **Run** — Klipp does not re-infer on every click, because that would lock up the UI on weaker hardware. To preview different click placements, place your clicks, run, evaluate, then refine.

### Box
Drag a bounding box around the object. The mask conforms to the largest connected region inside the box. Useful when the object has many internal edges that confuse point prompts (e.g. a bicycle, a lattice fence).

### Scribble
Freehand stroke through the object. Useful for thin or curved subjects (a guitar neck, a tail) where a box would include too much background.

## Inference Time

| Backend | RTX 4080 | RTX 3070 | 8-core CPU (no GPU) |
|---|---|---|---|
| candle + CUDA | ~80 ms | ~140 ms | n/a |
| candle + CPU | n/a | n/a | ~1–2 s |

The image encoder runs once per frame and is cached. Subsequent prompt changes on the same frame are sub-100 ms even on CPU because only the prompt encoder + mask decoder re-run.

## Tuning

- **Mask threshold** — same control as Remove Background. SAM produces a logit map; threshold 0.5 binarises it. Lower for soft edges, higher for tight cores.
- **Edge feathering** — Gaussian blur on the mask. Particularly useful when the SAM mask has a hard step at a soft real-world edge (hair, fur).

## When to Use

- A specific subject in a multi-subject scene (one person in a crowd)
- Picking out an object whose colour and shape make it hard for chroma-key or qualifier-based masking
- Iterating on the mask shape interactively — point/box/scribble prompts are far faster than rotoscoping

Less well suited for:

- Multi-frame tracking — MobileSAM has no temporal awareness. For motion, mask keyframes and let the Corrector interpolate, or wait for SAM 2.
- Very small objects (< 32 px) — the image encoder downsamples; sub-pixel detail is lost.

## Tips

- **One positive + one or two negative** clicks usually beats three positives. The model interprets multiple positives as "the union of these regions", which can pull in unwanted territory.
- **Box prompt for hard edges** — boxes are decisive. Use them when an object has a clear silhouette and you don't want SAM to wander into background.
- **Scribble for thin shapes** — a 1-px stroke down a power line or guitar string snaps to the line; a point click would capture too little context.
