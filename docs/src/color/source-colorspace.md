# Source Colorspace & Log

Modern cinema cameras record in one of several *log* (logarithmic) encodings designed to preserve highlight and shadow detail in 10-bit YUV. To grade these correctly, Klipp needs to know which log curve and gamut a clip was shot in — and apply the inverse log curve before any other operation.

Klipp auto-detects source colourspace from clip metadata using a four-layer chain. When detection fails, you can override per clip from the inspector.

## Supported Log Encodings

| Curve | Camera | Gamut |
|---|---|---|
| **Sony S-Log3** | Sony FX/A7S/Venice | S-Gamut3.Cine |
| **Canon Log 3** | Canon C70/C300/R5 | Cinema Gamut |
| **Panasonic V-Log** | Panasonic GH/S/EVA1 | V-Gamut |
| **ARRI Log-C 3** | ARRI Alexa | Wide Gamut |
| **Apple Log** | iPhone 15 Pro+ ProRes | Wide-DCI |
| **RED Log3G10** | RED cameras | RED WideGamut RGB |
| **DJI D-Log** | DJI Inspire / Mavic Pro | DJI gamut |
| **Z-CAM Z-Log2** | Z-CAM E2 / F6 | Z-CAM gamut |

Plus standard non-log spaces: sRGB, Rec.709, Rec.2020 (linear and gamma).

## Auto-Detection

Detection runs at clip import time. Four signal layers are queried in order:

1. **Stream-level metadata** — `colour_primaries`, `transfer_characteristics`, `matrix_coefficients` from the codec's stream header. Most reliable for cameras that mark these fields correctly.
2. **Encoder string heuristics** — Sony writes `encoder = "HEVC Coding"` on the *stream* (not the format) when shooting SLog3. The heuristic catches tagless variants by encoder name and codec parameters.
3. **IDT splice** — when the default colour graph for a clip lacks an Input Device Transform, Klipp inserts one based on the detected curve.
4. **User override** — set explicitly in the inspector; persists across project save/load.

Each detection produces a confidence score; sources at low confidence default to "Unknown — sRGB assumed" and produce a banner in the inspector inviting override.

## Override

To override:

1. Select the clip
2. Inspector → Video → Source Colourspace
3. Pick from the dropdown:
   - Auto (use detected) — default
   - sRGB / Rec.709 / Rec.2020
   - One of the log curves listed above

The override is per-clip and saved in the project. Re-importing the same media does not lose the override.

## Why YUV420P10LE Bypasses libswscale

Camera log sources almost always record as `yuv420p10le` — 10-bit YUV with chroma subsampled. Klipp normally uses libswscale to convert codec output to RGBA for the GPU pipeline, but **libswscale's RGBA64LE output for 10-bit log YUV is channel-clamped junk** — the high bits get truncated to 8-bit before scaling, regardless of `sws_setColorspaceDetails`.

Klipp bypasses libswscale entirely for `yuv420p10le` sources via a direct decoder that:

1. Reads YUV planes at full 10-bit precision (`& 0x3FF` to mask the lower 10 bits — *not* `>> 6`, which is for P010LE)
2. Decodes Y, U, V to floating-point in the source colourspace
3. Applies the source colourspace inverse to produce linear ACEScg directly

This bypass is what makes SLog3, V-Log, and the rest grade correctly — without it, the working space sees a clipped, posterised version of the original 10 bits.

## Environment Variable Override

For testing or stand-in use:

```bash
KLIPP_FORCE_LOG_FORMAT=slog3 cargo r
```

Forces every clip to use the named log curve regardless of metadata. Available values: `slog3`, `clog3`, `vlog`, `logc`, `applelog`, `redlog3g10`, `dlog`, `zlog2`. Useful when developing or when batch-processing footage from a camera Klipp doesn't auto-detect yet.

The per-clip Inspector dropdown is the canonical way to do this; the env var is a developer-side stand-in.

## What if I Get It Wrong?

If a clip is decoded with the wrong source colourspace:

- **Log decoded as sRGB** — Image looks faded and low-contrast (the log curve's linear range is interpreted as already-gamma-curved values)
- **sRGB decoded as log** — Image looks crushed and over-saturated (the inverse log curve is applied to already-linear values)

Both are visible at a glance. Toggle the inspector dropdown until the image looks like the camera intended; then either keep the override or, if many clips from the same camera need the same override, file a bug — the auto-detect is missing a case.
