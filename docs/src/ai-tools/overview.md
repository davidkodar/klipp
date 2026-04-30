# AI Tools Overview

Klipp ships on-device AI tools for masking, object isolation, and instance segmentation. All inference runs locally — no frames ever leave your machine — using ONNX models loaded through `ort` (with CUDA acceleration when available) or pure-Rust models loaded through `candle`.

The AI Tools panel is opened with **Alt+A**, or via **Window → AI Tools**.

## Three Tasks

| Task | Use case | Recommended model | Prompt type |
|---|---|---|---|
| **Remove Background** | Cut out the foreground subject as a single mask | BiRefNet Lite | none — automatic |
| **Isolate Object** | Click-to-mask a specific object or region | MobileSAM | point / box / scribble |
| **Detect Objects** | Multi-class segmentation, one mask per detection | RF-DETR-Seg (Medium) | none — automatic |

A fourth task, **Track Object** (temporal mask propagation), is reserved for the SAM 2.1 video backend and ships disabled until that work lands.

## The Three-State Workflow

The panel follows a deliberate three-state machine designed to get out of your way:

### Idle
- Numbered steps (1–4) walk you through the workflow visually
- A 2×2 task tile grid shows the four task types with Phosphor icons
- The model card shows the active model with an inline `˅ Pick model` dropdown — no need to leave the panel
- Scope chips choose between *Current frame*, *Selection*, and *Full clip*
- The **Advanced** dropdown exposes mask threshold, edge feathering, and output format

### Running
- Tiles compress to 56 px to free vertical space for progress
- A spinner glyph replaces the active task's icon
- A fake-but-honest progress bar advances based on the model's average inference time from prior runs (capped at 95% so completion always feels like real progress, not a timer expiring)
- The static prompt overlay stays visible — point markers (green = positive, red = negative) and dashed bounding boxes do not animate during inference

### Complete
- A green success banner shows the frame number and total inference time
- The mask preview heatmap supports three display modes:
  - **Overlay** — mask alpha blended over the source frame
  - **Mask only** — the raw mask on a flat background
  - **Cutout** — the source frame with the mask alpha applied, against a checkerboard
- **Re-run** repeats with the same parameters; **Apply to clip** writes the mask to disk and inserts a node into the clip's colour graph (see *Apply to Clip* below)
- The Recent panel keeps the last 20 results in memory with thumbnails for quick comparison

## Apply to Clip

When you click **Apply to clip**, Klipp:

1. Finds the target clip — selected clip if any, otherwise the topmost track at the playhead
2. Writes the mask as a PNG to `~/.cache/klipp/ai_masks/<clip_id>_<frame>_<model_hash>_<timestamp>.png` using an atomic write (`.tmp` → rename)
3. Inserts an `ExternalMatte` node into the clip's `NodeGraph`, with `matte_image_path` pointing at the cached PNG and `matte_channel` set to `Luminance`
4. Wires the node into the first `Corrector`'s matte input port. If a matte was already wired, the old connection is replaced
5. Toasts `Mask applied to {track_name} · {clip_short}`

The mask cache is bounded — `~/.cache/klipp/ai_masks/` is LRU-pruned to 2 GB on app start, oldest first.

## Auto-Pick by Task

Switching tasks auto-selects the recommended model for that task using a three-tier fallback:

1. The user's last explicit pick for that task
2. The task's recommended default (per the table above)
3. The first capable model installed on disk

If no capable model is installed, the panel routes you to the AI Models dialog with the recommended model pre-selected for download.

## Footer Status

The panel's footer shows the resolved inference path:

| Indicator | Meaning |
|---|---|
| ● green — `NVIDIA CUDA` | GPU acceleration active |
| ● amber — `GPU detected — runtime libs missing` | NVIDIA card present but `cudnn` / `libcudart` not installed |
| ● amber — `CPU only` | No NVIDIA GPU, or `ai-ort-cuda` was not enabled at build time |
| ● grey — `CPU only — feature flag off` | Klipp was built with `--no-default-features` |

Hover any of these for a tooltip explaining how to upgrade.

## See Also

- [Models & Manager](./models.md) — install, verify, uninstall, switch versions
- [Remove Background](./remove-background.md) — workflow and tips
- [Isolate Object](./isolate-object.md) — point/box/scribble prompting
- [Detect Objects](./detect-objects.md) — class filtering and multi-instance handling
- [GPU Runtime](./gpu-runtime.md) — CUDA setup, build-time vs runtime
