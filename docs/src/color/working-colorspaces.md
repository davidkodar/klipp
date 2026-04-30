# Working Colorspaces

Klipp's colour pipeline runs in linear light, with **ACEScg** as the default working colour space. This page explains what that means, what you can choose between, and how the chain composes from source decode to final delivery.

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
3. Working colourspace (ACEScg by default)
    │
    ├─► 4a. Effects chain (blur, sharpen, transform, …)
    │
    ├─► 4b. Corrector chain (wheels, curves, qualifiers, mattes)
    │
    ▼
5. Output Display Transform (ODT)  ← chosen at export time
    │
    ▼
Final delivery (sRGB, Rec.709, Rec.2020, P3-D65)
```

Stages 1–3 are *decode*. Stage 4 is *grading*. Stage 5 is *delivery*.

## Available Working Colourspaces

| Working space | Use when |
|---|---|
| **ACEScg** (default) | VFX work, maximum colour fidelity, stacking many effects |
| **Rec.709 (linear)** | Conservative grade for HD broadcast / web |
| **Rec.2020 (linear)** | HDR and UHD wide-gamut grades |
| **sRGB (encoded)** | Compatibility mode for legacy projects; effects do not behave linearly |

Set the working space in **Project → Settings → Colour**. Changing the working space mid-project re-evaluates all clips through the new chain — your grades stay numerically consistent because they're parameter changes, not pixel changes.

## Why ACEScg Specifically

ACEScg has three useful properties:

1. **AP1 primaries** — wider than Rec.2020. No clipping when grading a wide-gamut camera through to Rec.2020 delivery.
2. **Linear** — every effect behaves like light.
3. **Industry-standard** — round-tripping through Resolve, Nuke, or any other ACES-aware tool is lossless.

The cost is that ACEScg values can exceed 1.0 (super-white highlights from log sources) and go below 0.0 (chromatic excursions). All Klipp's effects are floating-point throughout to handle this.

## Output Display Transforms

At export, an ODT converts from the working space to the delivery space. ODT is per-export, not per-project — you can render the same edit to sRGB, Rec.709, and Rec.2020 without changing any grades.

| ODT | Use for |
|---|---|
| sRGB | YouTube, web, most monitors |
| Rec.709 | Broadcast HD, conservative streaming |
| Rec.2020 | HDR streaming, Blu-ray UHD |
| P3-D65 | Cinema, Apple displays |

Pick the ODT in the export dialog. Per-clip ODT overrides are also possible for split-delivery workflows.

## Per-Clip Source Override

Klipp auto-detects source colourspace from clip metadata, but the detection is overridable per clip via the inspector. See [Source Colourspace & Log](./source-colorspace.md) for the full table of supported log curves and how to override.

## Numeric Precision

The pipeline is `f16` on the GPU (storage) and `f32` in compute, end to end. There is no point at which Klipp falls back to 8-bit colour internally; even the final encode happens after dithered quantisation from 16-bit float to the target bit depth.

## Why This Matters for Compositing

When you stack a 50% opacity clip over a background in ACEScg, the result is what you'd see if you'd shot two physical lights at half power against the scene. When you do the same in sRGB-encoded space, you get a darker, muddier composite that requires extra exposure compensation to look right.

This single change — linear-light everywhere — is the largest visible quality difference between Klipp's pipeline and a typical web-video editor.
