# Detect Objects

Multi-class instance segmentation. Klipp runs the entire frame through RF-DETR-Seg and produces one mask per detected object, each tagged with a class label and confidence score.

## Workflow

1. Park the playhead on the frame.
2. **Alt+A** to open the AI Tools panel.
3. Click the **Detect Objects** tile.
4. Click **Run on current frame**.
5. The Complete state shows a list of detections, one row per object: class name, confidence percentage, and a thumbnail.
6. Pick the detection(s) you want to keep — click rows to toggle. Selected detections are unioned into the final mask.
7. Click **Apply to clip**.

## Default Model

**RF-DETR-Seg (Medium)** — transformer-based detector with per-instance masks. Apache-2.0 licence. ~140 MB. Trained on the COCO-91 class set (people, vehicles, animals, common objects).

A faster but noisier **Preview** tier is also available — same architecture, different weights. Use it when you want to scan many frames quickly and don't mind a higher false-positive rate.

## Class Set (COCO-91)

The model knows 80 object classes plus 11 stuff classes. Most-used at video editing time:

- **People & animals** — person, dog, cat, horse, sheep, cow, elephant, bear, zebra, giraffe, bird
- **Vehicles** — car, motorcycle, airplane, bus, train, truck, boat, bicycle
- **Indoor** — chair, couch, bed, dining table, toilet, tv, laptop, microwave, oven, refrigerator
- **Sports** — frisbee, skis, snowboard, sports ball, kite, surfboard, tennis racket
- **Personal items** — backpack, umbrella, handbag, tie, suitcase

The full list is shown in the detection picker when you click **Filter classes…**.

## Inference Time

| Backend | RTX 4080 | RTX 3070 | 8-core CPU (no GPU) |
|---|---|---|---|
| ort + CUDA EP | ~350 ms | ~600 ms | n/a |
| ort + CPU EP | n/a | n/a | ~3–5 s |

Detection time is largely independent of the number of objects in the frame — RF-DETR runs the same forward pass regardless.

## Tuning

- **Confidence threshold** — minimum score for a detection to be reported. Default 0.5. Raise to 0.7+ to suppress low-confidence false positives; lower to 0.3 to catch occluded or small objects.
- **Class filter** — restrict detections to a subset of the 91 classes. Useful when you know you only want people, or only vehicles, and want to suppress noisy detections of background objects.
- **Max detections** — caps the number of returned objects (default 100). Lower if the frame is busy and you want only the most prominent.

## When to Use

- Editorial workflows where you want to mask "all the people" or "all the cars" in a shot
- Privacy redaction (mask all faces / number plates)
- Effect application restricted to a class — e.g. apply colour grade only to people, leave the background untouched
- Quick scene analysis — count objects per frame for a rough scene description

Less well suited for:

- Single-object focus where the box / point prompt of [Isolate Object](./isolate-object.md) is faster
- Custom or unusual classes — RF-DETR-Seg is closed to COCO-91. Custom classes require fine-tuning and re-export to ONNX
- Sub-30 px objects — the detector downsamples its input; tiny objects fall below detection threshold

## Preview vs Medium Tier

Both tiers are bundled and selectable from the AI Models dialog:

| Tier | Size | Speed | Accuracy | Use when |
|---|---|---|---|---|
| Preview | 134 MB | ≈ Medium | Lower (more false positives, especially out-of-distribution scenes) | Quickly scanning many frames, classifier confidence not critical |
| Medium | 140 MB | ≈ Preview | Genuinely accurate | Default — recommended for production work |

The Preview tier is included as a hedge against the Medium tier missing detections in unusual content; in practice, Medium is the better choice for most editing workflows.

## Tips

- **Filter early** — if you only care about people, set the class filter before running. Klipp still runs the full forward pass (the model is class-agnostic at the detector head), but the UI only surfaces matching detections, which makes the picker much easier to navigate.
- **Union, don't repeat** — selecting multiple detections in the Complete-state picker unions them into a single mask. There is no need to run inference multiple times to get a multi-object mask.
- **Mask quality follows detection quality** — a low-confidence detection will produce a noisier mask. If a class you want is consistently below threshold, lower the confidence threshold rather than masking and cleaning up by hand.
