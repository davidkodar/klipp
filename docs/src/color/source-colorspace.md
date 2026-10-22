# Source Colorspace & Log

Modern cinema cameras record in one of several *log* (logarithmic) encodings designed to preserve highlight and shadow detail in 10-bit YUV. To grade these correctly, Klipp needs to know which log curve and gamut a clip was shot in — and apply the inverse log curve before any other operation.

Klipp auto-detects source colourspace from clip metadata using a four-layer chain. When detection fails, you can override per clip from the inspector.

## Supported Log Encodings

Klipp decodes fifteen camera log formats, from ARRI LogC3 and LogC4, Sony S-Log2 and S-Log3, Canon Log and Canon Log 3, and Panasonic V-Log to RED Log3G10, Blackmagic Film Gen 5, DJI D-Log, Fujifilm F-Log, Apple Log, Nikon N-Log, GoPro Protune Flat and Leica L-Log. [Supported Formats](../reference/formats.md) lists each with the gamut it is decoded in.

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
2. Inspector → Colour → **Input Colour Space**
3. Pick the right encoding: one of the log curves listed above, or sRGB / Rec.709 / Rec.2020.

The override is per clip, saved in the project, and one undo step. New clips take the project's input format and input LUT from the Colour Management page.

## Why YUV420P10LE Bypasses libswscale

Camera log sources almost always record as `yuv420p10le` — 10-bit YUV with chroma subsampled. Klipp normally uses libswscale to convert codec output to RGBA for the GPU pipeline, but **libswscale's RGBA64LE output for 10-bit log YUV is channel-clamped junk** — the high bits get truncated to 8-bit before scaling, regardless of `sws_setColorspaceDetails`.

Klipp bypasses libswscale entirely for `yuv420p10le` sources via a direct decoder that:

1. Reads YUV planes at full 10-bit precision (`& 0x3FF` to mask the lower 10 bits — *not* `>> 6`, which is for P010LE)
2. Decodes Y, U, V to floating-point in the source colourspace
3. Applies the source colourspace inverse to produce linear light in the working space directly

This bypass is what makes SLog3, V-Log, and the rest grade correctly — without it, the working space sees a clipped, posterised version of the original 10 bits.

## Environment Variable Override

For testing or stand-in use:

```bash
KLIPP_FORCE_LOG_FORMAT=slog3 cargo r
```

Forces every clip to use the named log curve regardless of metadata. Available values: `slog3`, `slog2`, `vlog`, `clog`, `clog3`, `logc3`, `logc4`, `redlog3g10`, `bmdfilmgen5`, `dlog`, `flog`, `applelog`, `nlog`, `goproflat` and `llog` (or `leicallog`); any other value is ignored. Useful when developing or when batch-processing footage from a camera Klipp doesn't auto-detect yet.

The per-clip Inspector dropdown is the canonical way to do this; the env var is a developer-side stand-in.

## What if I Get It Wrong?

If a clip is decoded with the wrong source colourspace:

- **Log decoded as sRGB** — Image looks faded and low-contrast (the log curve's linear range is interpreted as already-gamma-curved values)
- **sRGB decoded as log** — Image looks crushed and over-saturated (the inverse log curve is applied to already-linear values)

Both are visible at a glance. Change the clip's Input Colour Space until the image looks like the camera intended. If many clips from one camera need the same override, set the project's input format instead, and report the case so detection can learn it. Clip metadata also helps tell a hazy picture from camera log.
