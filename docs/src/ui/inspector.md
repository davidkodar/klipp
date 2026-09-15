# Inspector

The inspector panel appears on the right side of the interface and displays context-sensitive properties for the selected clip. It contains four tabs.

## Video Tab

The video tab shows spatial and visual properties:

### Transform
- **Position** — X and Y offset from centre
- **Scale** — Uniform or independent X / Y scaling
- **Rotation** — Rotation in degrees
- **Anchor Point** — Centre of transformation

### Opacity
Adjust clip opacity from 0% (transparent) to 100% (fully opaque). Opacity is GPU-accelerated and uses premultiplied alpha for correct compositing — opacity scales all four RGBA channels.

### Source Colourspace
Override Klipp's auto-detected source colourspace. Useful when:

- Camera log encoding is mis-detected (rare; auto-detect is reliable for Sony / Canon / Panasonic / ARRI / Apple / RED)
- The clip was already de-logged externally
- You want to force a specific decode path

Available choices include Rec.709, Rec.2020, sRGB, ACEScg, plus the camera log curves. See [Source Colourspace & Log](../color/source-colorspace.md) for details.

### Stabilisation
Per-clip stabilisation is applied here. **Analyze** runs motion analysis (~18 s on 4K with the SequentialFrameDecoder); afterwards a **Smoothness** slider, **Bypass** toggle, and **Show jitter %** indicator let you adjust without re-analysing. See [Stabilization](../editing/stabilization.md).

## Audio Tab

The audio tab shows audio properties for the selected clip:

- **Volume** — Clip volume in dB with scrubby slider
- **Pan** — Stereo pan position
- **EQ** — Multi-band parametric equaliser
- **Fade In / Out** — Curve type and duration
- **Audio Meters** — Real-time level meters with peak hold

All audio parameters support keyframe animation for dynamic changes over time. Click the diamond icon next to any parameter to add a keyframe at the playhead.

## Effects Tab

Lists all effects applied to the clip. Each row shows:

- Enable/disable toggle
- Parameter controls (sliders, colour pickers, etc.)
- Drag handle to reorder in the effect chain
- Delete button

Add effects with the **+** button or by dragging from the effects browser. Effects ship as GPU compute shaders with CPU fallback — see [GPU Effects](../effects/gpu-effects.md).

## Info Tab

The info tab displays read-only metadata about the selected clip:

- File name and path
- Duration and frame count
- Resolution and aspect ratio
- Frame rate
- Video codec, pixel format, bit depth
- Detected source colourspace and gamut
- Audio codec, sample rate, channels
- File size
- Date created / modified
- Camera make and model, encoder
- Lens, exposure (ISO, shutter, aperture, focal length), white balance
- The gamma and gamut the camera recorded in, such as S-Log3 / S-Gamut3.Cine
- Start timecode and reel
- **All metadata**: every tag the file carried, as written, folded under a header

See [Importing Media](../editing/importing.md#what-klipp-reads-about-a-shot) for where each of these comes from.

## Adaptive Layout

The inspector adapts to the available space. When the panel is narrow, controls reflow to a single column. Sliders use scrubby input — click and drag horizontally on any numeric value to adjust it smoothly. The cursor wraps at the screen edge so long drags don't run out of room.

Hover any parameter for a tooltip with the parameter name, current value, and (where applicable) the range of valid values.
