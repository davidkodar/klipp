# Making Your Own Presets

Many of Klipp's lists come from plain text files: the generators, the project formats, the HDR Zones starting points, the delivery formats, the false-colour palettes, the "Animate letters" presets and the audio effect presets. Each list can take entries of your own, written in the same form as Klipp's.

## Where they go

Each kind has its own folder under `~/.config/klipp/presets/` (or `$XDG_CONFIG_HOME/klipp/presets/`). Any `.toml` file in that folder is read.

| Kind | Folder | Shown in |
|---|---|---|
| Generators | `generators/` | Library → Generators |
| Project formats and destinations | `project_formats/` | New Project, the Welcome screen, Sequence Settings |
| HDR Zones presets | `hdr_zones/` | Colour → HDR Zones |
| Delivery formats | `delivery_templates/` | Export → deliver in several formats |
| False-colour palettes | `false_colour/` | the viewer's False Colour menu, Preferences |
| "Animate letters" presets | `animators/` | Title tab → Animate letters |
| Audio effect presets | `audio_effects/` | the audio effect's presets in the inspector |

Every file starts with two lines that say what it holds: `kind` and `format_version`. Each entry follows as a `[[…]]` table.

**How they sit beside Klipp's:**
- Your entries are listed after Klipp's own. They never replace one: an entry with the name of one of Klipp's is skipped.
- Klipp reads the folder the first time it needs the list. To see a change, restart Klipp; installing or removing a template pack also re-reads it.

**What gets skipped:** a file that can't be used is skipped with a reason, and the rest are still read. Klipp's log says what was skipped, and `klipp_project_templates` lists it for the project formats. Klipp skips:
- a file that isn't plain (a link), or is over 1 MB;
- a file of another `kind`, or a `format_version` newer than this Klipp reads;
- an unknown field, or a value out of range.

**Sharing:** to share your presets, send the `.toml` file; the other person puts it in the same folder. To share several kinds at once, a template pack carries them in its own `presets/<folder>/`. Klipp reads a file someone else made the same careful way as your own: limited in size, checked field by field, never followed through a link.

## Generators

A name, the group it's listed under in the Generators tab, and the generator clip it adds, in the form a project saves it. Colours are RGBA, 0 to 255.

```toml
{{#include preset-examples/generators.toml}}
```

## Project formats and destinations

A name, the picture in pixels, and the frame rates it is made at, the first one being the usual. Add `hdr = true` for HLG or PQ delivery. A destination also names the platform, its loudness target and whether its pieces are captioned. A project made from a destination puts these in its sequence's brief, which the Delivery Check and the export read. A destination can also list the viewer overlays, by id, for where that platform's buttons cover the picture.

```toml
{{#include preset-examples/project_formats.toml}}
```

## HDR Zones presets

A name, a line saying what it does, and its changes from neutral, by zone (`global`, `black`, `dark`, `shadow`, `light`, `highlight`, `specular`):
- `exposure` is in stops;
- `saturation` 1 is no change;
- `tint` is a hue on the wheel (0 red, 30 orange, 210 a cool blue) and an amount, where 1 is the wheel's rim.

```toml
{{#include preset-examples/hdr_zones.toml}}
```

## Delivery formats

What the Export page can deliver in one go:
- a name;
- its aspect, as width and height parts;
- the suffix its file takes (letters, digits, `-` and `_`);
- the export preset it is written with, by id.

```toml
{{#include preset-examples/delivery_templates.toml}}
```

## False-colour palettes

A name and its stops. Each stop is a band of 8-bit luma (`from` and `to`, both included), the colour it's painted (RGB, 0 to 255) and a label saying what the band means.

```toml
{{#include preset-examples/false_colour.toml}}
```

## "Animate letters" presets

- An `id`, which edit operations and the assistant use.
- A `name`, shown in the Title tab.
- The part of the text it picks (`selector`) and what it does to it (`props`).
- Its keys:
  - each placed at the clip's `start`, at the `entrance` (1.2 s in, or the clip's end on a shorter clip) or at the clip's `end`;
  - on the range's `start`, `end` or `offset`.

Offsets and blur are in font sizes, and rotation is in degrees clockwise.

```toml
{{#include preset-examples/animators.toml}}
```

## Audio effect presets

The effect it's for, a name, a line saying what it does, and the effect's settings by name.

```toml
{{#include preset-examples/audio_effects.toml}}
```

Every example on this page is checked by a test that reads it as your own file would be read.
