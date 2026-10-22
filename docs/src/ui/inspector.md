# Inspector

The inspector panel on the right shows the properties of the selected clip, in tabs that depend on what it is:

| Selection | Tabs |
|---|---|
| A media clip | Video, Audio, Colour, Effects, Info |
| A title | Title, Clip Video, Effects, Info |
| A generator | Generator, Video, Effects, Info |
| Nothing | The sequence: its settings, or plain facts about it |

A tab with keyed parameters is marked ◆, and its tooltip names what is animated there. **◆ Keyframes** at the right of the tab row shows each parameter's keys on a strip beside it, when the inspector is wide enough.

## Video Tab

- **Transform** — position, scale (with a separate height when keyed apart), rotation and anchor, keyframable; reset and clear-keys buttons.
- **Compositing** — opacity, blend mode and a matte source (alpha, luma, or another clip's picture).
- **Placement** — **Auto** follows the effects' order; **Fixed** keeps the transform where its row is.
- **Behaviours** — Ken Burns, Frame Glide (frame the clip's start and end over the picture), Hyperzoom and colour fades.
- **Speed** — constant speed, speed keys and ramps, with an option to keep the pitch.
- **Source colourspace** — override the detected input colour space or camera log curve; see [Source Colourspace & Log](../color/source-colorspace.md).

Opacity is premultiplied: it scales all four channels. Every enabled Opacity — the clip's, an effect's, its track's — multiplies.

## Audio Tab

- **Volume** and **Pan**, keyframable
- **Channels** — stereo, left only, right only, swapped, or a mono downmix
- **Fade In / Out** — duration and curve (linear, logarithmic, exponential, S-curve, constant power)
- **Role** — dialogue, music, effects and the like, used by the mix and the delivery check
- **Audio effects** — the clip's audio effect chain, EQ among them
- **Auto Duck** — lower this clip under a sidechain by a duck level, threshold, attack and release
- **Loudness meter** and waveform

Click the diamond next to a parameter to add a key at the playhead.

## Colour Tab

The clip's input colour space and LUTs, a **Bake Grade** action, and **Open Colour Panel** (C) for the full grade. See [Color Grading](../color/grading.md).

## Effects Tab

The clip's effect stack, as cards in the order they run. Each card has:

- an eye to turn it on or off, and **Remove**
- its parameters, each with a key diamond
- **Mix**, to blend the result back over its input, and a **mask channel** to limit it to a matte
- **Move up** / **Move down**, and **Open in graph** to see it as a node

Add effects from the Library tab, by drag or double-click. Built-in effects run as GPU compute nodes — see [GPU Effects](../effects/gpu-effects.md).

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

The inspector adapts to the available space and keeps its rows inside the column at any width. Sliders use scrubby input — click and drag horizontally on any numeric value to adjust it smoothly. The cursor wraps at the screen edge so long drags don't run out of room.

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
