# Node Graph

Each clip in Klipp has a per-clip *node graph* (in the `klipp-nodes` crate) that defines how the source frame is transformed into the final composited output. The graph is a directed acyclic graph where nodes produce or consume frames, masks, or parameter values.

Most users never need to open the node graph — the inspector controls map onto graph nodes automatically — but for power users it exposes the full compositing surface.

## Default Graph

A new clip starts with this graph:

```
Source ── Corrector ── Output
```

- **Source** — the decoded frame in the working colour space
- **Corrector** — colour wheels, curves, qualifiers; the inspector's colour panel writes here
- **Output** — feeds the timeline composite

## Node Kinds

| Node | Inputs | Output | Purpose |
|---|---|---|---|
| `Source` | — | frame | Decoded source clip |
| `Corrector` | frame, matte? | frame | Wheels / curves / qualifiers; matte gates the operation |
| `ExternalMatte` | path | mask | Loads a PNG/EXR matte from disk |
| `MaskCombine` | mask, mask | mask | Union / intersection / subtraction of two masks |
| `MaskInvert` | mask | mask | Inverts a mask |
| `MaskBlur` | mask | mask | Gaussian blur on the mask only |
| `Effect` | frame | frame | Wraps a `klipp-effects` operator (Blur, Sharpen, etc.) |
| `Transform` | frame | frame | Position / scale / rotation |
| `Output` | frame | — | Terminal — feeds the timeline composite |

The `MaskChannel` enum (Luminance / RGB-X) selects which channel of an external matte image is used as the alpha source.

## When the Graph Matters

Most users only see the graph when:

- **Applying an AI mask** — *Apply to clip* in the AI Tools panel inserts an `ExternalMatte` node and wires it to the first Corrector. See [AI Tools Overview](../ai-tools/overview.md#apply-to-clip).
- **Stacking multiple correctors** — when the inspector adds a second corrector for secondary grading, a new `Corrector` node is appended downstream.
- **Combining multiple masks** — e.g. an HSL qualifier *and* an external matte should mask the same correction. The graph editor wires both into a `MaskCombine` node and the result into the corrector's matte input.

## Editing the Graph

Open the graph from the inspector's **Effects** tab → **Show node graph…**

The graph editor shows nodes as rounded rectangles with input ports on the left and output ports on the right. Drag from an output port to an input port to wire. Right-click a node to delete or replace it.

Node parameters appear in the inspector when the node is selected.

## Mask Routing Conventions

- **Corrector port 1** is the matte input. A wired matte gates the colour operation; an unwired matte means the corrector applies to the entire frame.
- A new external matte connected to a corrector that already has a matte *replaces* the previous edge — the user almost always means "use this new matte instead of the old one". To union the two, route both through a `MaskCombine` node first.

## Why Direct UI Mutation Is Discouraged

Direct mutation of the graph from UI code (e.g. `vec.remove(i)` against the node list) bypasses the centralised handler that maintains cross-field invariants. Klipp routes every graph change through `AppCommand` so that orphaned `TrackPoints`, dangling keyframes, or stale matte edges can be detected and cleaned up consistently.

When extending the editor, route new operations through `AppCommand::NodeGraph(*)` rather than touching the graph directly.

## File Format

Node graphs are serialised inside the project's per-clip JSON. They round-trip across save / load and across version control diffs. A typical graph for a clip with one AI mask looks like:

```json
{
  "nodes": [
    {"id": 1, "kind": "Source"},
    {"id": 2, "kind": "ExternalMatte",
     "params": {"matte_image_path": "ai_masks/abc_123.png",
                "matte_channel": "Luminance"}},
    {"id": 3, "kind": "Corrector",
     "params": {"lift": [0.0,0.0,0.0,0.0], "...": "..."}},
    {"id": 4, "kind": "Output"}
  ],
  "edges": [
    {"from": [1, 0], "to": [3, 0]},
    {"from": [2, 0], "to": [3, 1]},
    {"from": [3, 0], "to": [4, 0]}
  ]
}
```
