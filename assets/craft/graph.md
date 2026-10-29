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

A merge lays the end of a branch beside the chain over the clip: at the clip's
top level, `add_beside` a source (a media item, a generator, the clip below),
then `add_beside` each effect after it, connecting each one's `in` to the
node before; connect the last one's `out` to the merge's `fg`. A green screen
is a media item, a chroma key and a transform, merged over the clip. A node
beside the chain reads the clip's source, its grade's picture or another node
beside the chain, never the chain past the grade (refused, named).

## Publish a group output

Expose a direct child's output on its enclosing clip group, then connect from
that group to a sibling effect. The output type is inferred from the child;
read the resulting `ports` entries (`key`, `direction`, `ty`, `binding`) on the
group in `klipp_graph_read`, or read the group path itself. `klipp_node_types`
also describes these operations under `publication`.

```json
{"ops":[
  {"op":"publish","path":{"root":{"clip":"CLIP_ID"},"nodes":["GROUP_ID"]},"from":{"root":{"clip":"CLIP_ID"},"nodes":["GROUP_ID","MASK_ID"]},"from_port":"out","key":"window_mask","label":"Window"},
  {"op":"connect","from":{"root":{"clip":"CLIP_ID"},"nodes":["GROUP_ID"]},"from_port":"window_mask","to":{"root":{"clip":"CLIP_ID"},"nodes":["BLUR_ID"]},"to_port":"mask"}
]}
```

`from` can use `{new:k}` for a direct child added earlier in the same batch.
To remove an output, disconnect its readers and then send
`{"op":"unpublish","path":GROUP_PATH,"key":"window_mask"}` in the same batch.
Missing children/outputs, duplicate keys and invalid connections refuse the
whole batch without changing the graph. The child node remains after unpublish.

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
Clip groups keep published outputs through save/reopen. Other roots still
refuse interfaces they cannot preserve. Do not replace a refused operation with an approximation and report success.

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

To see what one node does, compare its input with its output: read
`klipp_frame_at`, switch the node off with `klipp_toggle_node`, read it again,
and switch it back on. In the app, a node's menu has Compare input and output
(A/B), which wipes the viewer between the two. `frame_at`'s `channel: "alpha"`
shows a key's or a merge's coverage directly.

## The rules of klipp_graph, in short

Edit one clip graph atomically as one undo step; send alone. modify(path,key,modifiers) sets an effect parameter's behaviours, its keys kept: a list like [{"type":"wiggle","amplitude":20,"frequency":3}] (also oscillate, throw, ramp, loop, ping_pong, clamp), at most 8, numbers finite; [] takes them off; only a parameter that moves over time takes them (a Picture Transform's position does, a Transform's placement doesn't). keys(path,key,keys) sets an effect parameter's keys, its behaviours kept: a list like [{"time_secs":0.5,"value":0},{"time_secs":1.5,"value":20,"interpolation":"Step"}] in seconds into the clip, linear to the next key unless it says Step or Bezier, or a key's "ease" (in, out, in_out, expo_out, back_out…) eases it into the next, at most 256, numbers finite; [] takes them off; on the clip's placement node it keys pos_x, pos_y, scale, scale_y, rotation, tilt_x or tilt_y, as the clip's own keyframes. Read graph_read and node_types first. Paths contain a root and node IDs; {new:k} refers to the node made by earlier op k. Values follow catalog kinds; choices use labels; a Merge's `mask` port limits where it lays its `fg` (a window or a key beside the chain); a qualifier beside the chain (`klipp.grade.qualifier`) keys the picture by colour, for a Merge's or an effect's mask: set `qual_enabled` true and its `qual_hue_center` and `qual_hue_width` in degrees, and the `qual_sat_*` and `qual_lum_*` ranges; a `klipp.fx.title_source` beside the chain draws a title (its `title`, a title as the title op makes it, JSON) as a picture a Merge lays over, as a title clip above would show it; a `klipp.fx.surface_pin` beside the chain lays what it reads onto a surface tracked in the clip (track_surface first; its `surface` is the surface's name or id, its `inset` a fraction kept inside each edge), following the track frame by frame, so a Merge puts a picture on a sign or screen; on a surface the clip lacks it draws nothing; `klipp.fx.gradient_map` lays the picture's brightness along a ramp of three colours, `shadow`, `mid` and `highlight` ([r,g,b] from 0 to 1 or "#rrggbb"), `mix` of it over the picture; `klipp.fx.warp` bends the picture by one shape, its `mode`: 0 a wave (`amplitude` in pixels of a 1080-line frame, `wavelength` a fraction of the frame, `speed`, `direction` across, up and down or both), 1 a twirl (`angle` in degrees, `radius` a fraction of the frame's height, `center_x`/`center_y`), 2 a bulge or pinch (`strength` above or below 0, `radius`, centre), 3 ripples (`amplitude`, `wavelength`, `speed`, centre), 4 torn rows (`strength`, `band` a fraction of the frame's height, `speed`); it moves with the clip's time; a difference key beside the chain (`klipp.grade.difference_key`) takes its clean plate from the clip's own frames: set `difference_key.plate_frame` to the source seconds of a frame without the subject (the media's own time, so a trim leaves it); it is read once and kept, takes the place of a plate file, and a time outside the media gives no plate, said in the log; an effect's colour is [r,g,b] from 0 to 1 or "#rrggbb", and its text (a media source's media id) a string. Created nodes return full paths. A clip's graph keeps an effect's mix, its mask (channel and connection) and its label; the timeline grade and a grade group's stages keep a correction's mix and mask channel, and a track's grade refuses them. Placed kinds are refused; clip groups support publish/unpublish; unsupported parameter shapes are refused with the failing op_index. Revision and project identity guard commit. add_beside inserts unwired; connect required inputs in the same batch. Mask shapes use the masks JSON described by node_types.
