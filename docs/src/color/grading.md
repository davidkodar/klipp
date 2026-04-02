# Color Grading

Klipp provides a complete color grading toolset for adjusting the look and exposure of your footage. Access color controls from the inspector's Color tab or switch to the Color Grading workspace for a dedicated layout.

## Color Wheels

Four color wheels control different tonal ranges:

### Lift
Adjusts the darkest parts of the image (shadows). Drag the center point to shift the shadow color. The outer ring controls brightness of the shadows.

### Gamma
Adjusts the midtones. Most of the visible image falls in this range, so gamma changes have the most visible impact on overall color balance.

### Gain
Adjusts the brightest parts of the image (highlights). Use gain to correct white balance in highlights or add warmth/coolness to bright areas.

### Offset
Applies a uniform shift across the entire tonal range. Useful for overall color cast correction or creating a consistent look.

## Curves Editor

The curves editor provides precise control over luminance and individual color channels:

- **Master curve (white)** -- Controls overall luminance
- **Red curve** -- Controls the red channel
- **Green curve** -- Controls the green channel
- **Blue curve** -- Controls the blue channel

Click on the curve to add control points. Drag points to adjust. Double-click a point to remove it.

Common adjustments:
- **S-curve** on the master channel increases contrast
- **Lifting the black point** creates a faded/matte look
- **Individual channel curves** correct color casts precisely

## Hue / Saturation

- **Hue** -- Rotate all colors around the color wheel (0-360 degrees)
- **Saturation** -- Intensity of colors (0 = monochrome, 100 = normal, 200 = oversaturated)
- **Vibrance** -- Intelligent saturation that protects skin tones and already-saturated colors

## Temperature and Tint

- **Temperature** -- Shift between warm (amber) and cool (blue) tones. Measured in approximate Kelvin equivalents.
- **Tint** -- Shift between green and magenta to correct for fluorescent or mixed lighting.

## Working Color Spaces

Klipp supports multiple working color spaces for different production requirements:

- **Rec.709** -- Standard for HD video. Default working space.
- **Rec.2020** -- Wide gamut for HDR and UHD content.
- **ACEScg** -- Academy Color Encoding System. Linear, scene-referred. Ideal for VFX work and maximum color fidelity.

Set the working color space in project settings. All color operations are performed in the selected space.

## Viewer LUT

Apply a display LUT (Look-Up Table) to the viewer for monitoring purposes. The LUT affects only the preview -- it does not modify the exported output unless explicitly included in the render settings. Useful for previewing a film look or simulating a specific display output while grading.
