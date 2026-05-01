# Color Grading

Klipp provides a complete colour grading toolset for adjusting the look and exposure of your footage. Access colour controls from the inspector's effects chain or switch to the Color Grading workspace for a dedicated layout.

The pipeline runs in linear light with **ACEScg** as the default working colour space — see [Working Colorspaces](./working-colorspaces.md).

## Color Wheels

Four colour wheels control different tonal ranges:

### Lift
Adjusts the darkest parts of the image (shadows). Drag the centre point to shift shadow colour. The outer ring controls shadow brightness.

### Gamma
Adjusts the midtones. Most of the visible image falls in this range, so gamma changes have the most visible impact on overall colour balance.

### Gain
Adjusts the brightest parts of the image (highlights). Use gain to correct white balance in highlights or add warmth/coolness to bright areas.

### Offset
Applies a uniform shift across the entire tonal range. Useful for overall colour-cast correction.

## Curves Editor

The curves editor provides precise control over luminance and individual colour channels:

- **Master curve** — overall luminance
- **Red / Green / Blue curves** — per-channel control
- **Hue vs Hue / Hue vs Sat / Hue vs Lum** — secondary curves for hue-based qualifying

Click on a curve to add a control point. Drag to adjust. Double-click to remove.

Common patterns:

- S-curve on the master channel increases contrast
- Lifting the black point creates a faded matte look
- Per-channel curves correct colour casts precisely
- Hue vs Sat is the cleanest way to desaturate a single colour without touching others

## HSL Qualifier

Pick a colour with the eyedropper, then adjust:

- Hue range
- Saturation range
- Luminance range
- Softness (feathering)

The qualifier output drives a mask that can be wired into a Corrector node — see [Node Graph](./node-graph.md).

## Temperature and Tint

- **Temperature** — Shift between warm (amber) and cool (blue) tones. Approximate Kelvin equivalents.
- **Tint** — Shift between green and magenta to correct for fluorescent or mixed lighting.

These controls operate in CIE xy chromaticity space and are colourspace-aware — temperature in Rec.709 and ACEScg work the same way perceptually.

## Saturation

- **Saturation** — Linear intensity scale (0 = monochrome, 1 = normal, 2 = oversaturated)
- **Vibrance** — Saturation that protects skin tones and already-saturated colours

## LUTs

Klipp ships ~300 film emulation LUTs (Kodak Portra, Tri-X, Fuji Velvia, Agfa, etc.) under MIT, plus ~50 cinematic LUTs under CC0. Apply a LUT from the Color Panel's LUT picker. LUTs operate in their native domain (most are display-referred); Klipp converts to/from ACEScg around the LUT call so the rest of the chain stays linear.

User LUTs land in `~/.config/klipp/luts/` and appear automatically.

## Viewer LUT

Apply a display LUT to the viewer for monitoring. The viewer LUT affects only the preview — it does not modify exported output unless explicitly included in render settings. Useful for previewing a film look or simulating a specific display output while grading.

## Output Display Transforms

At export time, pick an Output Display Transform (ODT) to convert from the working space to the delivery space:

- sRGB
- Rec.709
- Rec.2020
- P3-D65

ODT is per-export — the same project can be rendered to multiple deliveries without re-grading. See [Working Colorspaces](./working-colorspaces.md) for the full chain.

## Order of Operations

The colour pipeline applies in this order:

1. Source decode (with camera-log inverse if applicable)
2. Source colourspace → ACEScg
3. Effects chain (blur, sharpen, etc.) in ACEScg linear
4. Colour Corrector chain (wheels, curves, qualifiers)
5. ACEScg → Output Display Transform
6. Final encode

Working in linear ACEScg between steps 2 and 5 means colour operations behave as light operations — combining halves a value rather than producing the gamma-confused mid-grey of an sRGB-domain mix.
