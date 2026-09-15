# Titles

Klipp's titles are text layers rendered by the `klipp-title` engine and composited like any other clip. A title lives on a text track as a clip: trim it, move it, copy it, grade it.

## The Library

**Library → Titles** holds a curated set of twenty-five designs, one job each, in six groups: Lower Thirds, Openers, Captions, Credits, Social and Utility. Every card is rendered by the real title engine over a frame of your own footage, with the template's fonts, shadows and motion at its resting pose. Hover a card to see its motion play. The swatches above the grid switch the backdrop to black, white or a colour if you want to judge a title without footage, and the text field renders your own words into every card.

## Adding a Title

Double-click a card, or drag it onto the timeline. The title lands on a text track at the playhead with a five-second duration, becomes the selection, the inspector opens on its Title tab with the text field focused, and the playhead parks a little way into the clip so the viewer shows the title at rest rather than mid-entrance. Type to replace the placeholder text.

## Fonts

Four families ship inside Klipp, under the SIL Open Font License, so a title looks the same on every machine:

| Family | For |
|---|---|
| Inter | Broadcast, social and interface text |
| Bebas Neue | Impact: condensed, all caps |
| Libre Baskerville | Cinematic and editorial titles |
| JetBrains Mono | Slates, timecode and data |

They appear at the top of the font picker with any fonts found on the system. New text starts in Inter.

## Motion

Each text layer has an entrance and an exit, set in the inspector's **Motion** group:

- **Kind** — Fade, Rise, Drop, Slide from left or right, Grow, Settle, Focus, Mask rise, Mask drop, Typewriter. The mask kinds clip each glyph to its resting box so it appears out of an invisible edge.
- **Duration** and **Delay** — the exit's delay counts back from the end of the clip, so it always finishes as the clip does.
- **Easing** — ease out in three strengths, overshoot, ease in and out, ease in, or linear.
- **Stagger** — all at once, or by line, word or character, with the gap between units.
- **Travel** — how far the moving kinds move, as a fraction of the font size.

The templates are built on this model, so a template's motion is fully editable after it is added. The older whole-title animations (scroll, crawl, flash, counter) and the per-character animation remain available in their own groups.

## Text Properties

Font family and style, size in pixels at 1080p, colour, alignment, tracking, line spacing, case, shadows, outlines, glows, gradients, inner shadows, pattern and texture fills, extrusion, text on a path and rich-text style runs are all in the Title tab. A title can hold several text and shape layers; select a layer in the viewer or the Layers list to edit it.

## Font Discovery

Klipp scans the system font directories on first launch and caches the result in `~/.cache/klipp/font_registry.json`. Later launches read the cache. Run `Reload Font Registry` from the command palette to force a rescan.

## Performance

Glyphs are rasterised with `fontdue`; shadows, glows and blurs run on the GPU when one is available. A 1080p title with shadow and outline renders in well under a millisecond per frame.
