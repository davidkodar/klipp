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
- **Logging**: scene, shot, take, reel, keywords, description, comments and good take, edited in place
- **Markers**: the notes, favourite and rejected stretches marked on the clip
- Synced audio and split-take part, when the clip has them

See [Importing Media](../editing/importing.md#what-klipp-reads-about-a-shot) for where each of these comes from.

## Adaptive Layout

The inspector adapts to the available space. When the panel is narrow, controls reflow to a single column. Sliders use scrubby input — click and drag horizontally on any numeric value to adjust it smoothly. The cursor wraps at the screen edge so long drags don't run out of room.

Hover any parameter for a tooltip with the parameter name, current value, and (where applicable) the range of valid values.

## Title Tab

Selected title clips get a Title tab in three tiers: the text, font and position and the Motion and Style cards always open, with every choice shown as a chip rendered on the clip's own text; the full effect stacks and the rest under Advanced. With nothing selected the tab offers **Add Title**. See [Titles](../editing/titles.md).

### Editable vector paths

Every layer of a title — text, shape or picture — carries its own motion and
effect stack. Select the layer and the inspector shows its Motion card (an
entrance, an exit and the loops, previewed on the layer itself), its Effects
group (glow, outline, drop shadow, inner shadow and their secondary layers)
and its Motion Blur; a text layer also has the fills, which a shape or a
picture has no use for. A backing pill can rise with its words, a logo can
fade in under a glow, and a row of cards can arrive one after another by
each card's own delay.

Select a title's shape layer and choose **Start editable path** to begin with a
square. The path editor exposes vertices, absolute cubic handles, contours,
fill RGBA, stroke RGBA and stroke width (pixels at 1080p). Drag the points or
edit their coordinates; normalized `(0,0)` and `(1,1)` span the layer box.
JSON import/export uses the same `VectorPath` structure as the `vector_path`
agent operation. SVG import is not implemented.

**Key pose** stores geometry and paint at the clip-local playhead. Editing near
an existing key changes that key's stored pose and timestamp; the easing menu
controls interpolation toward the next key. The first/last pose is held outside
the keyed range. Clear keys before changing contour count, vertex count/order
or closed flags: mismatched topology is refused rather than guessed. Contour
and vertex order establish morph correspondence. Up to 16 contours, 256 total
vertices and 256 pose keys are supported.

Closed contours use even-odd fill (including holes); open contours are
stroke-only. Strokes have round caps and joins. Cubics are flattened at output
resolution and antialiased; no fixed-resolution bitmap is stored in the project.
Path animation is evaluated in both the viewer and export title pipeline.

### Stroke trim and glow

Enable **Trim stroke** in an editable path's inspector. **Start** and **End**
select an interval of each contour's length; **Offset** shifts it in turns.
Start must not exceed End. Open contours clip shifted intervals to their ends;
closed contours wrap across the seam. A full-length closed stroke stays full
at any offset. Each contour reveals independently over its own length. Reveals
use arc length measured on output-resolution cubic subdivisions, so unequal
curve segments do not receive equal time.

Trim has its own key button and easing, independent of morph/paint keys. Key
End from 0 to 1 to draw, hold that range, then key Start from 0 to 1 to retract.
Round caps/joins apply at reveal endpoints. Fill remains unchanged. **Stroke
glow** (independent of Trim stroke) adds a soft halo in the stroke's color from only the currently revealed
stroke; radius scales from 1080p and strength is 0..1. Glow radius and strength are pose paint and can be keyed with the morph/paint keys.
The same optional `path.trim` object is accepted by the `vector_path` operation
and JSON import. Readers supporting paths but not trim refuse the unknown
`trim` field; paths without trim retain the prior representation.
