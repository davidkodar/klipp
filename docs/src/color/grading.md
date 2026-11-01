# Color Grading

Switch to the **Colour** workspace (the switcher at the top, or **C** for the colour panel) to grade: the program viewer, a strip of graded thumbnails for moving between shots, the scope dock, and the grade of the selected clip with its node graph.

The pipeline runs in linear light, in **Rec.709 linear** unless the project says otherwise — see [Working Colorspaces](./working-colorspaces.md).

## Where a Grade Lives

- **A clip's grade** — its own node graph, the usual place.
- **A track's grade** — over every clip on the track.
- **A grade group's stages** — shared by the clips in the group: pre-clip, before their own grades, and post-clip, after.
- **The timeline grade** — over the whole picture.

Each is a node graph of correctors, keys, masks, LUTs and effects, and each is edited the same way. See [Node Graph](./node-graph.md).

## Colour Wheels

The Corrector's wheels control tonal ranges:

- **Lift** — the darkest parts of the image
- **Gamma** — the midtones
- **Gain** — the brightest parts
- **Offset** — a uniform shift across the whole range

Each wheel has a colour puck and a level. **Log** wheels (shadows, midtones, highlights with adjustable ranges) suit log and HDR footage, and a luma mix sets how much the wheels move brightness. Exposure, contrast with its pivot, saturation, temperature and tint, highlights and shadows, and an ASC CDL view sit beside them.

**HDR Zones** divides the range into tonal zones measured in stops, each with its own wheel, with presets.

## Printer Lights

Step the red, green, blue or all three up and down by printer points: **Ctrl+Alt+7/4** (red), **8/5** (green), **9/6** (blue), **3/1** (master).

## Curves

- **Master** and **Red / Green / Blue** curves
- **Hue vs Hue**, **Hue vs Sat**, **Hue vs Lum**, **Lum vs Sat** and more

Click a curve to add a point, drag to adjust, double-click to remove. Hue vs Sat is the cleanest way to desaturate one colour without touching others.

## Colour Warper

A grid over hue and saturation (or chroma and luma): drag points to move colours toward others, the rest following smoothly.

## Qualifiers

A qualifier keys the picture by colour, so a correction applies only there:

- **HSL** — hue, saturation and luminance ranges with softness
- **RGB** — red, green and blue ranges
- **3D** — strokes across the colours to keep and the colours to take out

Pick with the eyedropper on the viewer. **Matte Finesse** cleans the key: clean black and clean white, shrink / grow, blur, and a denoise that clears specks and keeps the edge.

## Masks

Circle, linear gradient and freeform masks limit a correction to part of the frame. Draw freeform masks with the Mask tool (**P**) in the viewer: curves, an inner feather and a combine mode (add, subtract, intersect). A mask's shape can be keyed over time or follow a tracked subject.

## Matching and Balance

- **Auto WB** balances from a neutral you pick.
- **Match** captures a reference frame and matches the clip's colour to it.
- Scenes can be matched to a hero shot, and looks derived from a reference; see [Colour Check](#colour-check).

## Split Shots

A shot split in pieces is still one shot. When you've graded one piece, right-click it and choose **Colour → Apply Grade to All Pieces of This Shot**. Its grade goes on the other pieces that have no grade of their own, as one undo step.

Pieces count as one shot when they sit side by side on the same track, from the same file, at the same speed, with the file's range carrying straight on. A piece you graded yourself keeps its grade.

To have the Colour page offer this under its header whenever the graded clip has ungraded pieces, turn on **Preferences → Colour → Offer grade for split shots**. It's off by default.

## LUTs

Klipp ships 304 LUTs: film emulation (negative, print, slide, instant, black and white, Fuji X-Trans) under MIT, and a set of cinematic looks. Apply one to a clip from the **LUTs** category of the Effects browser (hover a card to see it on the clip first) or the Colour tab's LUT presets: it becomes the clip's look, a 3D LUT in its grade, and another preset replaces it, one undo step each. A LUT node in the grade does the same by hand. 3D `.cube` LUTs and 1D tables and shapers (`.cube` 1D, CSP) load. User LUTs in `~/.config/klipp/luts/` appear automatically.

**Bake Grade as 3D LUT (.cube)** (command palette, or **Bake Grade** in the Colour tab) writes the clip's grade as a LUT.

## Timeline LUT

The Colour tab's **Timeline LUT** is one LUT file over the whole picture, after every clip's grade, with its own opacity and on/off switch. It is part of the project and every export applies it.

## Viewing LUT

A project's viewing LUT, set in **Colour Management**, and the calibration LUT in Preferences are applied to the viewer only, never to an export. The scopes can read before or after the viewing LUT.

## Grade Library, Versions and Flags

- **Grade versions** keep alternative grades on a clip to switch between.
- The **grade library** keeps grades across projects: save one, browse with thumbnails, apply it.
- **Copy Colour Nodes** / **Paste Colour Nodes** (Ctrl+C / Ctrl+V in the node graph) carry a grade between clips.
- **Grade flags** mark clips as needs work, review or approved, shown on the thumbnail strip.
- Node colour labels travel with a pasted, library or still grade.

## Colour Check

**Window → Colour Check** measures the grade: a chart self-check, skin tones against their band, white balance, black floor and clipping, banding, and how shots match each other. Its tools match scenes to a hero shot, grade a clip toward a measured target, and derive a look from a reference. Findings can be reviewed and saved with a reason.

## Output Display Transforms

At export time, an Output Display Transform (ODT) converts from the working space to the delivery space: sRGB, Rec.709, Rec.2020 or P3-D65, and HDR PQ or HLG. The same project can be rendered to several deliveries without re-grading. See [Working Colorspaces](./working-colorspaces.md).

## Order of Operations

1. Source decode, with the camera-log inverse if applicable
2. Source colourspace → working space (linear)
3. The clip's effects and grade, in the order of its node graph
4. The track's grade, the grade group's stages, then the timeline grade
5. Working space → Output Display Transform
6. Encode

Working in linear light means colour operations behave as light: mixing two values halves them rather than producing the gamma-confused mid-grey of an sRGB-domain mix.
