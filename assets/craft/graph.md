# Editing a clip graph

Read `klipp_graph_read` and `klipp_node_types` before calling `klipp_graph`.
Start with `{"path":{"root":{"clip":"CLIP_ID"},"nodes":[]}}`, then
read the grade group's path to see its nodes and edges. Replace the capitalised
IDs in these examples with IDs from those reads. A path lists every enclosing
group before the target node; names and labels are not IDs.

Use the catalog's type IDs, parameter keys, choice labels and port keys. The
catalog describes types; it does not promise every operation can currently be
saved in every context. If a read reports omissions, read a narrower path. Missing
parameters can mean defaults; omitted details do not mean an empty node.

## Chain or branch

`add` inserts a node after an existing node in the picture chain. For example,
the `ops` entry below inserts a grade node after the named sibling:

```json
{"op":"add","after":{"root":{"clip":"CLIP_ID"},"nodes":["GROUP_ID","NODE_ID"]},"type":"klipp.grade.corrector","params":{"exposure":0.5}}
```

`add_beside` creates a sibling without splicing it into that chain. Use it for
a mask branch, and connect its required inputs in the same batch. Connections
must stay inside the same parent graph. `{"new":0}` means the node created by
operation 0 of this batch, not the first node in a read; it is only valid after
that operation and under the same root and parent. The result's `created`
entries contain full paths to use in later calls.

This `klipp_graph` call creates a rectangular window beside an existing
Corrector and connects it to the Corrector's mask input:

```json
{
  "ops": [
    {
      "op": "add_beside",
      "at": {"root":{"clip":"CLIP_ID"},"nodes":["GROUP_ID","CORRECTOR_ID"]},
      "type": "klipp.grade.mask",
      "label": "Window",
      "params": {"masks":{"masks":[{
        "shape":{"Rectangle":{"center_x":0.5,"center_y":0.5,"width":0.4,"height":0.4,"rotation":0}},
        "feather":0,"opacity":1,"invert":false,"enabled":true
      }]}}
    },
    {
      "op": "connect",
      "from": {"root":{"clip":"CLIP_ID"},"nodes":["GROUP_ID",{"new":0}]},
      "from_port": "out",
      "to": {"root":{"clip":"CLIP_ID"},"nodes":["GROUP_ID","CORRECTOR_ID"]},
      "to_port": "mask"
    }
  ]
}
```

Add `expected_revision` and `expected_project_identity` from the latest read to
guard the call. Send it alone, not alongside another write. One successful batch
is one undo step, including its initial parameters and connections.

## Shape values

For an existing Mask, use `set` with its path, `key:"masks"` and the stack above
as `value`. `{"masks":[]}` clears shapes. Read `value_format` on the catalog's
Mask `masks` parameter for Circle, Rectangle, LinearGradient and Bezier formats.
At most 16 shapes and 3–256 points per Bezier are accepted. Coordinates, sizes,
feather and opacity are 0..1; rotation is −180..180 degrees. Values must be
finite. Unknown fields, non-empty keyframes and tracking are refused; this
operation currently handles static shapes.

## Current limits and refusals

Graph writes take every root a read takes: a clip, `{"timeline_grade":SEQ}`,
`{"group_stage":GROUP,"stage":"pre"}` (or `"post"`), and
`{"track":TRACK,"part":"grade"}` or `"part":"effects"`. Each batch is one undo
step of that root. A grade the sequence doesn't have yet has no node to name and
is refused; make it in the Colour workspace first. A track's effects keep no
keyframes, so key operations there are refused. A track's effects are named by
their place in its list: after an add, remove or move, use the `created` paths
and read the track again before naming its other effects. Placed kinds are
owned by the clip's placement model and cannot be edited as ordinary nodes.
On a clip, an effect node's mix, its mask connection and channel, and a custom
label are kept: they read back, survive saving and reopening, and undo as one
step. The timeline grade and a grade group's stages keep a correction's mix and
mask channel; a track's grade has no place for them and refuses them.
Publishing group ports is unavailable until those interfaces survive saving and
reopening. Do not replace
a refused operation with an approximation and report success.

A refused batch changes nothing. `op_index` is the zero-based index in your
original `ops`, including when an add's initial parameter failed. Read the
reason, correct the operation, refresh revision/project guards and resubmit
the whole intended batch. `op_index:null` means the failure is not attributed
to one operation; inspect the batch and its context. An unwired new node can
point to its add: supply the missing connection in that same batch. A lost reply
is different from a refusal: check the operation outcome or retry the identical
write with the same idempotency key before sending another write.

Read back the changed nodes and edges, then inspect `klipp_frame_at` at a useful
time. For a mask, verify both inside and outside the window. Saved values and a
successful write do not by themselves prove the rendered picture is correct.

## The rules of klipp_graph, in short

Edit one clip graph atomically as one undo step; send alone. Read graph_read and node_types first. Paths contain a root and node IDs; {new:k} refers to the node made by earlier op k. Values follow catalog kinds; choices use labels. Created nodes return full paths. A clip's graph keeps an effect's mix, its mask (channel and connection) and its label; the timeline grade and a grade group's stages keep a correction's mix and mask channel, and a track's grade refuses them. Placed kinds and publication are refused; unsupported parameter shapes are refused with the failing op_index. Revision and project identity guard commit. add_beside inserts unwired; connect required inputs in the same batch. Mask shapes use the masks JSON described by node_types.
