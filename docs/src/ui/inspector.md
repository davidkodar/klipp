# Inspector

The inspector panel appears on the right side of the interface and displays context-sensitive properties for the selected clip. It contains four tabs.

## Video Tab

The video tab shows spatial and visual properties:

### Transform
- **Position** -- X and Y offset from center
- **Scale** -- Uniform or independent X/Y scaling
- **Rotation** -- Rotation in degrees
- **Anchor Point** -- Center of transformation

### Opacity
Adjust clip opacity from 0% (transparent) to 100% (fully opaque). Opacity is GPU-accelerated and uses premultiplied alpha for correct compositing.

### Effects
Lists all effects applied to the clip. Each effect has:
- Enable/disable toggle
- Parameter controls (sliders, color pickers, etc.)
- Drag to reorder in the effect chain
- Delete button to remove

Add effects by clicking the **+** button or dragging from the effects browser. Available GPU effects include Gaussian Blur, Sharpen, Crop, Transform, Chroma Key, bloom, and more.

### Keyframes
Parameters with a diamond icon can be keyframed. Click the diamond to add a keyframe at the current playhead position. Keyframe interpolation supports linear, ease-in, ease-out, and bezier curves.

## Audio Tab

The audio tab shows audio properties for the selected clip:

- **Volume** -- Clip volume in dB with scrubby slider
- **Pan** -- Stereo pan position (-100 to +100)
- **EQ** -- Parametric equalizer with adjustable bands
- **Audio Meters** -- Real-time level meters showing peak and RMS levels

All audio parameters support keyframe animation for dynamic changes over time.

## Info Tab

The info tab displays read-only metadata about the selected clip:

- File name and path
- Duration
- Resolution and aspect ratio
- Frame rate
- Video codec
- Audio codec, sample rate, and channels
- File size
- Date created/modified

## Color Tab

The color tab provides quick access to color grading controls for the selected clip:

- Color wheels (lift, gamma, gain, offset)
- Temperature and tint
- Saturation

For full color grading, switch to the Color Grading workspace.

## Adaptive Layout

The inspector adapts to the available space. When the panel is narrow, controls stack vertically. Sliders use scrubby input -- click and drag horizontally on any numeric value to adjust it smoothly.
