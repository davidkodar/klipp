# Scene Detection

Klipp's scene detector (in the `klipp-scenedetect` crate) identifies cuts and dissolves in long-form footage and produces a list of scene boundaries you can apply as edits.

Useful for:

- Splitting raw recordings of multi-cam events
- Pre-editing screen recordings that have natural cut points
- Importing legacy footage where the source file has many scenes welded together
- Quick assembly cuts on archival material

## Workflow

1. Select a clip on the timeline (or in the browser)
2. **Clip → Detect Scenes** (or *Detect Scenes on Selected Clip* in the command palette)
3. Klipp analyses the clip; a progress dialog shows percent complete
4. When complete, a results panel lists detected scenes — one row per scene with thumbnail, in / out timecodes, and a confidence score
5. Pick which scenes to apply as edits, then click **Apply** to split the clip at every selected boundary

## Detection Methods

Two algorithms run in parallel and their results are combined:

- **Content detector** — colour histogram delta between adjacent frames. Good at hard cuts; misses dissolves.
- **Threshold detector** — luminance threshold crossings. Catches fade-to-black and fade-from-black.

Each detected boundary is tagged with which detector(s) flagged it and a confidence score.

## Tuning

The results panel exposes:

| Control | Default | Effect |
|---|---|---|
| **Sensitivity** | 0.6 | Lower → fewer false positives, may miss soft cuts. Higher → catches more but introduces noise. |
| **Min scene length** | 1.0 s | Skip detections shorter than this — suppresses jitter on rapid cuts |
| **Show below confidence** | off | When on, shows all detections, even low-confidence ones, for manual review |

## Apply as Edits

When you click **Apply**:

- The original clip is split at each selected boundary
- Resulting sub-clips inherit the source clip's effects, colour grade, and audio
- Track selection is preserved — the clip stays on its original track

To re-merge sub-clips: select adjacent ones and **Clip → Merge** from the command palette.

## Performance

Detection runs on the GPU via the same `SequentialFrameDecoder` used by stabilisation. On an RTX 4080:

- 4K HEVC, 60 minutes: ~3 minutes
- 1080p H.264, 60 minutes: ~50 seconds

CPU fallback is roughly 10× slower but functional.

## Cache

Scene-detection results are cached under `analysis/scenedetect/<clip_id>.json` in the project. Re-running with the same clip + parameters reads from the cache; changing parameters triggers re-analysis.

## Limitations

- Cross-dissolves are detected as a region of low confidence rather than a single cut — pick the midpoint manually if precision matters
- Static scenes with rapid camera motion can produce false positives; raise the **Min scene length** to suppress
- Animated content with hard colour palette swaps can confuse the detector — animation often shows large per-frame deltas without an actual cut
