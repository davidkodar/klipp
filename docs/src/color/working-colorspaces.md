# Working Colorspaces

Klipp's colour pipeline runs in linear light. A project works in **Rec.709 linear** unless you change it, and **ACEScg** is a choice away. This page explains what that means, what you can choose between, and how the chain composes from source decode to final delivery.

## What "Working Colourspace" Means

Every colour operation Klipp performs — opacity blending, blur, sharpen, colour wheels, curves — is mathematically defined in *some* colour space. If that space is sRGB-encoded (gamma-curved, display-referred), then halving a value gives you a perceptually-grey mid-tone, not an actual half-light value. Light doesn't work that way.

Klipp performs all colour maths in a *linear* (gamma-1.0) space whose primaries describe a wide gamut. ACEScg is the standard scene-referred linear space used by the visual-effects industry. Halving a value really halves the light; mixing two colours produces the colour you'd get if the two lights were physically combined.

## The Chain

```
Source clip
    │
    ▼
1. Decode (CUDA NVDEC / software ffmpeg)
    │
    ▼
2. Source colourspace inverse  ← per-clip auto-detected (Sony SLog3,
    │                            Canon Log, sRGB, Rec.709, …)
    ▼
3. Working colourspace (Rec.709 linear by default)
    │
    ├─► 4a. Effects chain (blur, sharpen, transform, …)
    │
    ├─► 4b. Corrector chain (wheels, curves, qualifiers, mattes)
    │
    ▼
5. Output Display Transform (ODT)  ← chosen at export time
    │
    ▼
Final delivery (sRGB, Rec.709, Rec.2020, P3-D65, Rec.2100 PQ / HLG)
```

Stages 1–3 are *decode*. Stage 4 is *grading*. Stage 5 is *delivery*.

## Available Working Colourspaces

| Working space | Use when |
|---|---|
| **Rec.709 (linear)** (default) | HD and web delivery; the safe choice, and what a project gets if you never touch this |
| **ACEScg** | VFX work, wide-gamut sources, stacking many effects, an ACES delivery |
| **Rec.2020 (linear)** | HDR and UHD wide-gamut grades without the ACES tone curve |
| **ACEScct** / **ACEScc** | Grading in a log space, for wheels and curves that respond as on film |

Set it under **Grade in** on the Colour Management page.

## Colour Management

**Colour Management…** (from Project Properties or the command palette) holds the project's colour settings on one page, in the order the picture goes through them:

- **Input** — the input format new clips take (or **Detect from the file**), and an input LUT.
- **Grade in** — the working space.
- **Output** — the output transform (including Rec.2100 PQ and HLG), and an output LUT, applied or not.
- **Monitoring** — a viewing LUT and a calibration LUT for the viewer only, the display gamma (BT.1886), **Match Viewer**, and whether the scopes read before or after the viewing LUT.

A colour change made here is one undo step, with any grades converted alongside it.

Switching it re-bases the whole grade chain. The dropdown alone is a
naked switch: the numbers stay, so the picture changes. The button
beside it wraps every clip's chain in primaries-conversion nodes
instead, which keeps the look across the switch — use that unless you
mean to re-grade.

## Why ACEScg When You Want It

ACEScg has three useful properties:

1. **AP1 primaries** — wider than Rec.2020. No clipping when grading a wide-gamut camera through to Rec.2020 delivery.
2. **Linear** — every effect behaves like light.
3. **Industry-standard** — round-tripping through any other ACES-aware tool is lossless.

The cost is that ACEScg values can exceed 1.0 (super-white highlights from log sources) and go below 0.0 (chromatic excursions). All Klipp's effects are floating-point throughout to handle this.

## Output Display Transforms

At export, an ODT converts from the working space to the delivery space. ODT is per-export, not per-project — you can render the same edit to sRGB, Rec.709, and Rec.2020 without changing any grades.

| ODT | Use for |
|---|---|
| sRGB | YouTube, web, most monitors |
| Rec.709 | Broadcast HD, conservative streaming |
| Rec.2020 | Wide-gamut SDR deliveries |
| Rec.2100 PQ / HLG | HDR streaming and broadcast |
| P3-D65 | Cinema, Apple displays |

Pick the ODT in the export settings, or set the project's on the Colour Management page. It applies to the whole export; there is no per-clip override.

## Per-Clip Source Override

Klipp auto-detects source colourspace from clip metadata, but the detection is overridable per clip via the inspector. See [Source Colourspace & Log](./source-colorspace.md) for the full table of supported log curves and how to override.

## Numeric Precision

The pipeline is `f16` on the GPU (storage) and `f32` in compute, end to end. There is no point at which Klipp falls back to 8-bit colour internally; even the final encode happens after dithered quantisation from 16-bit float to the target bit depth.

## Why This Matters for Compositing

When you stack a 50% opacity clip over a background in ACEScg, the result is what you'd see if you'd shot two physical lights at half power against the scene. When you do the same in sRGB-encoded space, you get a darker, muddier composite that requires extra exposure compensation to look right.

