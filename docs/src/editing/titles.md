# Titles

Klipp's titles are text layers rendered by the `klipp-title` engine and composited like any other clip. A title lives on a text track as a clip: trim it, move it, copy it, grade it.

## The Library

**Library → Titles** holds a curated set of twenty-five designs, one job each, in six groups: Lower Thirds, Openers, Captions, Credits, Social and Utility. Every card is rendered by the real title engine over a frame of your own footage, with the template's fonts, shadows and motion at its resting pose. Hover a card to see its motion play. The swatches above the grid switch the backdrop to black, white or a colour if you want to judge a title without footage, and the text field renders your own words into every card.

## Adding a Title

Every way in lands the same: the title becomes the selection, the inspector opens on its Title tab with the text focused, and the playhead parks a little way into the clip so the viewer shows the title at rest rather than mid-entrance. Type to replace the placeholder text.

- **Ctrl+T**, the **T** button in the timeline toolbar, **Add Title** in the command palette, or the **Add Title** button the Title tab shows when nothing is selected: a fresh five-second title on a free text track at the playhead.
- **Right-click a video clip › Add Title Over Clip**: a title spanning exactly that clip.
- **Library › Titles**: double-click a design, or drag it onto the timeline, for one of the curated designs.

A new text track is made on top when no existing one has room.

## Editing on the Picture

Double-click a title in the viewer to edit its text where it is. The caret and the selection are drawn on the engine's own render, so what you see while typing is the title itself: the face, the shadows, the fill. Click or drag on the text to place the caret or select; Esc, or a click elsewhere in the viewer, finishes. A double-click beside the text edits the first text layer.

Drag a layer to move it. It snaps to the frame's centre lines, the title-safe edges and the other layers' centres, with guide lines while it does and an x/y readout in frame pixels beside the pointer. Hold Alt to drag free.

## The Title Tab

The tab has three tiers. The first is always open: the text, the font, style, size, colour and alignment, the position, then **Motion** and **Style**.

**Motion** has an **In** row and an **Out** row. Each shows a chip rendered by the title engine on the clip's own text; hover it and the motion plays. Click the chip or the name to open a gallery of every kind, each rendered on your text. The duration sits beside it, and **Details** opens the delay, easing, stagger and travel. Below them are the **Loops**, added from a chip gallery of their own.

**Style** shows the presets as chips on your text (Neon, Classic, Bold Shadow, Outlined, VHS) and then one row each for **Fill** (with an optional gradient), **Stroke**, **Shadow** and **Glow**.

**Advanced**, collapsed by default, holds everything else: the full effect stacks (several shadows, outlines and glows, inner shadows, pattern and texture fills), typography, background and layer opacity, motion blur, 3D extrusion and distortion, and text on a path.

## Layers

A title is a stack of layers: text, shapes and images. The **Layers** list at the top of the tab shows them back to front with an eye, a lock and reorder arrows; select one to edit it, in the list or by clicking it in the viewer. **+ Text**, **+ Shape** and **+ Image** add a layer. An image layer takes a PNG, JPEG or WebP from disk, a logo or a still, sized to a third of the frame at the picture's aspect; its editor offers **Fit**, **Fill** or **Stretch** inside its box and a **Replace…** button. Image paths are saved relative to the project.

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

Below the entrance and exit sit the layer's **Loops**: what it keeps doing while it is on screen. Add as many as you like from the **+ Add** list, each with its own timing, and they all run at once on top of the entrance and exit, so a credits roll can fade in, scroll and fade out, and an alert can blink while it slides in:

| Loop | What it does |
|---|---|
| Scroll | The layer travels up or down through the frame. A speed of 0 fits the travel to the clip. |
| Crawl | The layer travels across the frame, left or right, optionally repeating. |
| Blink | Hard on and off at a rate, visible for a fraction of each period. |
| Pulse | A smooth swell between a minimum opacity and full. |
| Shake | The whole layer jitters, in font sizes. |
| Wave | Each glyph bobs on a travelling sine. |
| Colour cycle | The fill hue rotates. |
| Colour blend | The fill swings between two colours. |
| Counter | The text becomes a number counting from one value to another. |

The templates are built on this model, so a template's motion is fully editable after it is added. Projects saved by earlier versions, which had a single whole-title animation and a separate per-character animation, are converted to entrances, exits and loops when they open.

## Text Properties

Font family and style, size in pixels at 1080p, colour, alignment, tracking, line spacing, case, shadows, outlines, glows, gradients, inner shadows, pattern and texture fills, extrusion, text on a path and rich-text style runs are all in the Title tab, the everyday ones in the first tier and the rest under Advanced.

## Font Discovery

Klipp scans the system font directories on first launch and caches the result in `~/.cache/klipp/font_registry.json`. Later launches read the cache. Run `Reload Font Registry` from the command palette to force a rescan.

## Performance

Glyphs are rasterised with `fontdue`; shadows, glows and blurs run on the GPU when one is available. A 1080p title with shadow and outline renders in well under a millisecond per frame.
