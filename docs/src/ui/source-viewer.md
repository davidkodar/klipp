# Source Viewer

The source viewer is an optional second viewer for browsing source media without affecting the program viewer or timeline. It is a familiar pattern from professional NLEs (Avid, Premiere) — review your source clip in one viewer, see the program output in the other, and use insert/overwrite to move material between them.

Toggle the source viewer from **View → Toggle Source Viewer**.

## Workflow

1. Double-click a clip in the media browser. The clip loads into the source viewer.
2. Scrub or play the source viewer to find the section you want.
3. Press **I** in the source viewer to mark an in point, **O** for an out point.
4. Switch focus to the timeline, park the playhead where you want the section to land, then press **W** (insert) or **D** (overwrite).
5. Klipp adds the marked section of the source clip to the timeline at the playhead.

The source viewer stays loaded with that clip until you double-click a different one. Marks and playhead are independent of the timeline.

## Why It's Separate from the Browser Skim

The browser provides *skimming* — hover over a clip thumbnail to preview without committing. Skimming is fast and casual; the source viewer is for committed review with marks.

These two flows are deliberately separate so you can:
- Skim through a bin of dailies in the browser
- Promote a candidate to the source viewer when you find one worth marking up
- Set in/out points there
- Keep the browser free for browsing other candidates without losing your marked section

## Performance: Scrub vs Source

The source viewer uses Klipp's *skim* decode path, not the full timeline decode path. This is deliberate:

- **Skim path** — sparse keyframe decode, ~30 Hz UI throttle, coalesced seeks. Designed to keep up with rapid scrubbing on 4K source.
- **Timeline path** — full smooth playback with master clock sync. Designed for review at 1× or shuttle speeds.

If you need frame-accurate review at 1×, mark in/out and play the source — the source viewer switches to the playback path automatically when **Space** is pressed.

## Coalesced Seeks

The source viewer coalesces seek requests at three layers:

1. **UI throttle** — at most one seek per 33 ms (30 Hz)
2. **Engine coalesce** — pending seeks are dropped if a newer one arrives before the decoder consumes the previous
3. **Decoder coalesce** — the decoder discards in-flight frames whose presentation timestamp is past the current target

This three-layer coalescing is what makes 4K scrubbing feel professional rather than slideshow-stuttery.

## Display Modes

The source viewer respects the same overlays as the program viewer (safe areas, grid, zebra, false colour). It does **not** show the timeline-level effect chain — only the raw decoded source. This is by design: the source viewer is for evaluating source material, not for previewing graded output.
