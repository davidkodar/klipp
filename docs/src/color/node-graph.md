# Node Graph

Each clip in Klipp has a per-clip *node graph* (in the `klipp-nodes` crate) that defines how the source frame is transformed into the final composited output. The graph is a directed acyclic graph where nodes produce or consume frames, masks, or parameter values.

Most users never need to open the node graph — the inspector controls map onto graph nodes automatically — but for power users it exposes the full compositing surface.

## Default Graph

A new clip starts with this graph:

```
Source ── Corrector ── Output
```

- **Source** — the decoded frame in the working colour space
- **Corrector** — the bundled primary corrector (wheels, curves, balance, warper and the rest); the inspector's colour panel writes here
- **Output** — feeds the timeline composite

## Node Kinds

The `NodeKind` enum in `crates/klipp-nodes/src/kinds.rs` has 35 kinds plus `Unknown`. Ports are listed as inputs → outputs; a matte output is a single-channel selection that feeds another node's matte input.

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
| `ColorWarper` | 1 → 1 | Hue/saturation vector grading in 12 sectors. |
| `ColorMatch` | 2 → 1 | Histogram matching of the input (port 0) to a reference frame (port 1). |
| `Denoise` | 1 → 1 | Spatial noise reduction (bilateral filter); shown as *Noise Reduction*. |

### Secondaries and mattes

| Node | Ports | Purpose |
|---|---|---|
| `Qualifier` | 1 → matte | HSL qualifier: a matte from the input frame. |
| `LumaKey` | 1 → matte | A matte of pixels whose luma lies between a low and a high threshold, with a soft falloff. |
| `DifferenceKey` | 1 → matte | A matte of pixels that differ from a clean-plate image loaded from disk by more than a tolerance. |
| `Mask` | 1 → matte | Spatial masks (circle, rectangle, gradient). |
| `ExternalMatte` | 0 → matte | An image file read as a matte: its alpha, luminance, red, green or blue channel (`MatteChannel`), optionally inverted. |
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

- **Applying an AI mask** — *Apply to clip* in the AI Tools panel inserts an `ExternalMatte` node labelled *AI mask* and wires it to port 1 of the first Corrector, replacing any edge already there. See [AI Tools Overview](../ai-tools/overview.md#apply-to-clip).
- **Stacking multiple correctors** — when the inspector adds a second corrector for secondary grading, a new `Corrector` node is appended downstream.
- **Combining multiple masks** — e.g. an HSL qualifier *and* an external matte should select the same region. Wire both into a `CombineMattes` node and its output into the matte port of an `ApplyMatte`.

## Editing the Graph

Open the graph from the inspector's **Effects** tab → **Show node graph…**

The graph editor shows nodes as rounded rectangles with input ports on the left and output ports on the right. Drag from an output port to an input port to wire. Right-click a node to delete or replace it.

Node parameters appear in the inspector when the node is selected.

## Mask Routing Conventions

- A matte gates a grade through `ApplyMatte`: the graded frame on port 0, the matte on port 1, and the ungraded frame on port 2. Where the matte is 0 the ungraded frame shows through.
- Matte producers (`Qualifier`, `LumaKey`, `DifferenceKey`, `Mask`, `ExternalMatte`, `CombineMattes`) output a matte, and the editor colours their wires as mattes.
- To use two mattes at once, route both through a `CombineMattes` node first and pick its operator: intersect, union or subtract.

## Why Direct UI Mutation Is Discouraged

Direct mutation of the graph from UI code (e.g. `vec.remove(i)` against the node list) bypasses the centralised handler that maintains cross-field invariants. Klipp routes every graph change through `AppCommand` so that orphaned `TrackPoints`, dangling keyframes, or stale matte edges can be detected and cleaned up consistently.

When extending the editor, send the existing `AppCommand` variants rather than touching the graph directly. They are defined in `crates/klipp-core/src/events.rs` and take the clip's id, except the viewing ones (`SetNodeSolo`, `SetNodeViewBookmark`, `JumpNodeViewBookmark`), which act on the graph shown:

- **Structure** — `AddNode` (kind by name, e.g. `"Qualifier"`, and a position), `RemoveNode`, `ConnectNodes` (from node and port, to node and port), `DisconnectEdge` (by target node and port), `RenameNode`, `ToggleNodeEnabled`, `MoveNode`, `AutoLayoutNodeGraph`
- **Wireless links** — `SetNodeOutputLink`, `SetNodeInputLink`: connect ports by label instead of a drawn edge
- **Compounds** — `GroupIntoCompound`, `UngroupCompound`, `DescendIntoCompound`
- **Parameters** — `SetNodeParam` (one named scalar), bracketed by `BeginNodeParamEdit` / `EndNodeParamEdit` for a drag, and typed setters for structured parameters: `SetNodeCurves`, `SetNodeHueSatCurves`, `SetNodeQualifier`, `SetNodeLumaKey`, `SetNodeDifferenceKey`, `SetNodeDifferenceKeyPlate`, `SetNodeSoftClip`, `SetNodeMasks`, `SetNodeColorMatch`, `SetNodeLayerMixerInput`
- **Clipboard** — `CopyColorNodes`, `PasteColorNodes` (pasted nodes get new ids)
- **Annotation** — frames (`CreateNodeFrame`, `RenameNodeFrame`, `SetNodeFrameDescription`, `RecolorNodeFrame`, `AddToNodeFrame`, `RemoveFromNodeFrame`, `DeleteNodeFrame`) and sticky notes (`CreateStickyNote`, `SetStickyNoteText`, `MoveStickyNote`, `DeleteStickyNote`)
- **Viewing** — `SetNodeSolo` (show one node's output as the final image), `SetScopeTapNode`, `SetNodeViewBookmark` / `JumpNodeViewBookmark`

## File Format

A clip's graph is saved in its `node_graph` field in the project file, and round-trips across save and load. Node ids are UUIDs. An edge is four flat fields: the output node and port it leaves, and the input node and port it enters. `output_node_id` names the node whose output is the graded frame, and `node_positions` maps node ids to editor positions. The default graph looks like this (ids shortened here for reading; real ones are full UUIDs):

```json
{
  "nodes": [
    {"id": "5c1e…", "kind": "Source", "label": "Source", "enabled": true,
     "params": {}, "keyframes": {"tracks": []},
     "output_link": null, "input_links": []},
    {"id": "a07b…", "kind": "Corrector", "label": "Corrector", "enabled": true,
     "params": {}, "keyframes": {"tracks": []},
     "output_link": null, "input_links": []},
    {"id": "e9d2…", "kind": "Output", "label": "Output", "enabled": true,
     "params": {}, "keyframes": {"tracks": []},
     "output_link": null, "input_links": []}
  ],
  "edges": [
    {"from_node": "5c1e…", "from_port": 0, "to_node": "a07b…", "to_port": 0},
    {"from_node": "a07b…", "from_port": 0, "to_node": "e9d2…", "to_port": 0}
  ],
  "output_node_id": "e9d2…",
  "node_positions": {
    "5c1e…": {"x": 0.0, "y": 0.0},
    "a07b…": {"x": 220.0, "y": 0.0},
    "e9d2…": {"x": 440.0, "y": 0.0}
  }
}
```

`id`, `kind`, `label` and `enabled` are required on every node; `params`, `keyframes`, `output_link` and `input_links` may be left out when loading. Only the parameters that are set are written into `params`. The graph itself may also carry `working_space_override`, `frames` and `sticky_notes`, which are written only when set or non-empty, and `node_positions` defaults to empty when missing.
