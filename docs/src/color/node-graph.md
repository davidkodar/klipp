# Node Graph

Each clip in Klipp has a *node graph* that defines how the source frame becomes the picture the timeline composites: its grade, its effects, its keys and masks, as connected nodes. A track, a grade group's stages and the timeline grade have graphs of their own.

Most users never need to open the graph — the inspector's controls map onto its nodes — but it exposes the full surface: every effect in the clip's stack is a node in its picture graph too, and OFX and frei0r plug-ins run as nodes of their own.

## Default Graph

A new clip starts with this graph:

```
Source ── Corrector ── Output
```

- **Source** — the decoded frame in the working colour space
- **Corrector** — the bundled primary corrector (wheels, curves, balance, warper and the rest); the inspector's colour panel writes here
- **Output** — feeds the timeline composite

## Node Kinds

The kinds a grade graph is built from are below. Ports are listed as inputs → outputs; a matte output is a single-channel selection that feeds another node's matte input.

### Structure

| Node | Ports | Purpose |
|---|---|---|
| `Source` | 0 → 1 | The clip's decoded frame. The root; one per graph. |
| `Output` | 1 → 0 | The graded frame. The sink; one per graph. |
| `Compound` | 1 → 1 | Holds a nested graph in `params.compound_subgraph` and runs it recursively; its input feeds the sub-graph's Source. A passthrough when empty. |
| `Reroute` | 1 → 1 | Passes its input through unchanged. Only for bending wires in a dense graph. |
| `Unknown` | — | Loaded in place of a kind this build does not know, so newer projects still open. |

### Primary grade

| Node | Ports | Purpose |
|---|---|---|
| `InputTransform` | 1 → 1 | Camera log to the working space: undoes the camera's encoding before anything else runs. |
| `Corrector` | 1 → 1 | The bundled primary corrector: exposure, contrast, saturation, vibrance, tone, wheels, wheel sat/bri, colour balance, RGB curves, hue/sat curves and the colour warper on one node. The default graph's grade node and where the inspector writes. Log-mode wheels need a separate `ColorWheels` node. |
| `Exposure` | 1 → 1 | Exposure in stops. |
| `Contrast` | 1 → 1 | Contrast. |
| `Saturation` | 1 → 1 | Saturation. |
| `ToneControls` | 1 → 1 | Highlights, Shadows, Whites, Blacks. |
| `ColorWheels` | 1 → 1 | Lift / Gamma / Gain / Offset wheels. |
| `WheelSatBri` | 1 → 1 | Saturation and brightness per wheel. |
| `RGBCurves` | 1 → 1 | Master and R/G/B curves, with preserve-luma and mix. |
| `HueSatCurves` | 1 → 1 | The six hue/saturation curves. |
| `ColorBalance` | 1 → 1 | Temperature, tint, hue rotation and a global mix. |
| `HdrZones` | 1 → 1 | Exposure, colour and saturation by tonal zone, each zone's reach set in stops about mid grey. |
| `ColorWarper` | 1 → 1 | Hue/saturation vector grading in 12 sectors. |
| `ColorMatch` | 2 → 1 | Histogram matching of the input (port 0) to a reference frame (port 1). |
| `Denoise` | 1 → 1 | Spatial noise reduction (bilateral filter); shown as *Noise Reduction*. |

### Secondaries and mattes

| Node | Ports | Purpose |
|---|---|---|
| `Qualifier` | 1 → matte | A matte from the input frame's colours: HSL, RGB or 3D (strokes across the colours to keep and to take out), with Matte Finesse. |
| `LumaKey` | 1 → matte | A matte of pixels whose luma lies between a low and a high threshold, with a soft falloff. |
| `DifferenceKey` | 1 → matte | A matte of pixels that differ from a clean-plate image loaded from disk by more than a tolerance. |
| `Mask` | 1 → matte | Spatial masks: circle, rectangle, gradient, and freeform paths with curves, an inner feather and a combine mode; shapes can be keyed or tracked. |
| `ExternalMatte` | 0 → matte | An image file, or a per-frame matte sequence (such as Track Subject's), read as a matte: its alpha, luminance, red, green or blue channel, optionally inverted. |
| `KeyMixer` | 2+ mattes → matte | Mixes any number of mattes into one, each with a gain (negative subtracts), then an output gain and offset. |
| `CombineMattes` | 2 mattes → matte | Combines two mattes by `matte_combine_op`: intersect (the default), union or A minus B, in linear light. |
| `ApplyMatte` | 3 → 1 | Uses the graded frame (port 0) where the matte (port 1) selects and the ungraded frame (port 2) elsewhere. This is how a secondary is gated. |
| `MaskCutout` | 2 → 1 | The source (port 0) multiplied by the matte (port 1): the picture where the matte is, black outside. Alpha stays opaque. |

### LUT, colour space and look

| Node | Ports | Purpose |
|---|---|---|
| `Lut3D` | 1 → 1 | A 3D LUT (`.cube`, `.3dl`, `.csp`). |
| `PrimariesMatrix` | 1 → 1 | A fixed 3×3 matrix in linear light between two working-space primaries (for example BT.709 and ACEScg AP1). A passthrough when either side is unset or the pair has no matrix. The working-colourspace migration wraps grades in a pair of these. |
| `SoftClip` | 1 → 1 | Rolls highlights off below 1.0 per channel; a knee of 0 is a hard clip. |
| `FilmGrain` | 1 → 1 | Film grain overlay. |

### Compositing and channels

| Node | Ports | Purpose |
|---|---|---|
| `Layer` | 2 → 1 | Alpha composite: foreground (port 0) over background (port 1). |
| `Blend` | 2 → 1 | Mixes two inputs by a scalar. |
| `LayerMixer` | 2+ → 1 | N-input compositor, port 0 on top, each input with its own opacity and blend mode. Gains a port as wires are dropped on it. |
| `RGBSplitter` | 1 → 3 | Sends the frame to three outputs labelled R, G and B, so each branch can be graded for its channel. |
| `RGBCombiner` | 3 → 1 | Takes R from input 0, G from input 1 and B from input 2 (alpha from input 0). Pairs with `RGBSplitter`. |

Every kind also has a place in the grading order (`NodeKind::grade_stage`): input transform, balance, secondary, look or output, with plumbing nodes outside the order.

## When the Graph Matters

Most users only see the graph when:

- **Applying an AI mask** — *Apply to clip* in the AI Tools panel inserts an `ExternalMatte` node labelled *AI mask* and wires it to the first Corrector's matte input. See [AI Tools Overview](../ai-tools/overview.md#apply-to-clip).
- **Masking or mixing one correction** — a correction takes a **mix** and a **mask channel**, set in the inspector; a grade's Output can take a matte as the clip's alpha.
- **Stacking multiple correctors** — when the inspector adds a second corrector for secondary grading, a new `Corrector` node is appended downstream.
- **Combining multiple masks** — e.g. an HSL qualifier *and* an external matte should select the same region. Wire both into a `CombineMattes` node and its output into the matte port of an `ApplyMatte`.

## Editing the Graph

**Window → Nodes** shows the graph dock, in the Edit and Colour workspaces. In the Colour workspace, the arrow beside **Nodes** offers **Below viewer** and **Right of viewer**; drag the divider to resize, and the placement is remembered. **Open in graph** on an effect card in the inspector shows that effect's node.

The **Picture** view shows the clip's whole picture — source, effects, grade and output — and breadcrumbs lead from a grade back to its clip, or into a compound. Drag from an output port to an input port to wire; a reroute bends a wire through a point. Right-click a node to rename, label with a colour, delete or replace it; **Node graph: Auto-layout** tidies the canvas. Each node has a thumbnail, a solo to show its output alone, and a scope tap to feed the scopes from it. Node parameters appear in the inspector when the node is selected.

**[** and **]** step between colour nodes, **Alt+D** bypasses the current one, and **Ctrl+Shift+R** resets it.

## Mask Routing Conventions

- A matte gates a grade through `ApplyMatte`: the graded frame on port 0, the matte on port 1, and the ungraded frame on port 2. Where the matte is 0 the ungraded frame shows through.
- Matte producers (`Qualifier`, `LumaKey`, `DifferenceKey`, `Mask`, `ExternalMatte`, `CombineMattes`, `KeyMixer`) output a matte, and the editor colours their wires as mattes.
- To use two mattes at once, route both through a `CombineMattes` node first and pick its operator: intersect, union or subtract.

## File Format

A clip's graph is saved with the clip in the project file and round-trips across save and load. See [Project Format](../reference/project-format.md).
