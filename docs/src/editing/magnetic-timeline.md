# Magnetic Timeline

Klipp's timeline is *magnetic* by default — clips snap together, gaps close automatically, and edits ripple by default. This is the same behaviour Final Cut Pro popularised; Klipp implements 64 of the FCP-parity behaviours across edit, trim, move, and delete operations.

Toggle magnetic behaviour from **Timeline → Toggle Magnetic** if you want classic gap-permitting editing.

## Edit Operations

| Operation | Shortcut | Behaviour |
|---|---|---|
| **Insert** | **W** | Place clip at playhead; ripple subsequent clips forward |
| **Overwrite** | **D** | Place clip at playhead; replace whatever overlaps |
| **Append** | — | Place clip at end of primary track |
| **Backtimed insert** | — | Place clip ending at playhead; ripple |
| **Backtimed overwrite** | — | Place clip ending at playhead; overwrite |
| **Replace** | — | Swap clip; preserve timeline span and effects |
| **Source overwrite** | — | Use marked source range over marked timeline range |

## Trim Operations

| Operation | How | Behaviour |
|---|---|---|
| **Ripple trim** | Drag clip edge | Trim, ripple neighbours to close gap (default) |
| **Roll trim** | Drag edit point with both edges | Move boundary between two adjacent clips |
| **Slip** | Alt + drag inside clip | Change clip in/out without moving timeline position |
| **Slide** | Cmd + drag inside clip | Move clip and ripple both sides |
| **Extend edit** | E | Trim selected edge to playhead |

## Move Operations

| Operation | How | Behaviour |
|---|---|---|
| **Drag move** | Drag clip body | Magnetic-snap to neighbours and gaps |
| **Swap** | Drag clip onto another | Exchange the two clips' positions |
| **Lift** | Shift + Del | Remove clip; leave a gap |
| **Ripple delete** | Del | Remove clip; close gap |

## Delete Operations

| Operation | Shortcut | Behaviour |
|---|---|---|
| **Ripple delete** | **Del** | Remove clip; close gap (default) |
| **Lift delete** | **Shift+Del** | Remove clip; leave a gap |
| **Delete attributes** | — | Strip effects from a clip without removing it |

## Snapping

Toggle snapping with **N**. When enabled, clips snap to:

- The playhead
- Other clip boundaries
- In / out points
- Markers
- Clip edges across tracks

A visual snap-line appears when a snap occurs. The snap window is configurable in **Edit → Preferences → System & Performance**.

## Modifier Overrides

Hold a modifier while dragging to override magnetic behaviour:

| Modifier | Effect |
|---|---|
| **Shift** | Disable snapping for this drag only |
| **Ctrl** | Disable magnetic ripple — drag like a non-magnetic timeline |
| **Alt** | Slip mode — adjust clip in/out instead of position |
| **Cmd** | Slide mode — move clip and ripple both sides |

## Drop Targets

When dragging a clip onto the timeline, the drop indicator shows what will happen:

- **Vertical line between clips** — Insert (ripple)
- **Highlighted clip** — Overwrite that clip
- **Empty track area** — Place at that position; the magnetic timeline closes any resulting gap

A modal appears when the intent is ambiguous (dropping mid-clip): choose **Insert** or **Overwrite**.

## Ripple Across Tracks

When the magnetic timeline ripples, it considers every track simultaneously. A clip moved on track V1 will ripple linked audio on its own audio track and any clips on V2 / V3 that started after the moved clip.

To restrict rippling to a single track, use **Lock** on tracks you want to leave alone.

## Why Magnetic?

The magnetic model is opinionated: it prioritises *intent* over *coordinates*. When you delete a clip, you almost certainly mean "close the gap"; when you move a clip forward, you almost certainly mean "ripple later clips backward". Magnetic timelines turn those intents into one-step operations.

For workflows where you need exact timecode control (multi-cam sync, music videos with hard cuts on the beat), turn magnetic off — the timeline becomes a fixed-coordinate grid where deletes leave gaps and moves overwrite.
