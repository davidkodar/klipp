# Titles

Klipp's title generator (in the `klipp-title` crate) produces text overlays — lower thirds, full-screen titles, end cards, captions — rendered through the GPU effects pipeline so they composite at full quality with no separate render path.

Titles live on a normal video track as clips. They behave like any other clip for trim, move, copy, and effect application.

## Inserting a Title

From **Timeline → Generators → Title** or via the title browser:

1. Pick a preset (Lower Third, Centred Title, Three-Line Caption, End Card, etc.)
2. The title is inserted at the playhead with a default duration of 5 seconds
3. The inspector opens the title's text and style properties

## Text Properties

| Property | Notes |
|---|---|
| Text | Multi-line; supports basic Unicode |
| Font family | Drawn from the Klipp font registry — auto-cached at startup, see [File Locations](../reference/file-locations.md) |
| Font weight | Regular, Medium, Bold, Black (where the family supports them) |
| Size | In pixels at 1080p; scales with output resolution |
| Colour | RGBA picker |
| Background | None / solid / gradient |
| Outline | Thickness + colour |
| Drop shadow | Offset + blur + colour |

## Animation

Each title supports keyframable in/out animations:

- **Fade** — opacity from 0 to 1 over the in duration
- **Slide** — from any cardinal direction
- **Scale** — from 0 to 1 (with bounce easing if selected)
- **Typewriter** — characters appear one at a time

The in / out durations are independent. The middle is held static.

## Layout Presets

The bundled presets are starting points. After insertion the title is just a clip with parameters; tweak any property and the change is local to that clip.

To save a customised title as a new preset: **Inspector → Title → Save as preset…**. User presets land in `~/.config/klipp/title-presets/` and appear in the title browser alongside the bundled set.

## Font Discovery

Klipp scans the system font directories on first launch and caches the result in `~/.cache/klipp/font_registry.json`. Subsequent launches read the cache (~5 ms) instead of re-scanning. A background thread refreshes the cache when the system font configuration changes.

If a font does not appear in the picker, run `Reload Font Registry` from the command palette to force a rescan.

## Performance

Title rendering uses `cosmic-text` for shaping and `wgpu` for rasterisation. A 1080p title with shadow + outline renders in well under 1 ms per frame.

## Limitations Today

- No animated paths (text-along-curve)
- No 3D extrusion
- No timeline-based per-character keyframes (only whole-title in / out)

These are tracked as future work; for now, animate the whole title and split into multiple titles where you need character-level control.
