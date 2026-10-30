# `.klipp` on-disk project format

This document describes the layout and shape of a Klipp project as it
lives on disk. The authoritative serde structs are in
`crates/klipp-project/src/format.rs`; this document is the
human-readable companion.

## Directory layout

A `.klipp` project is a **directory**, not a single file. The
extension exists for OS-level association (file managers show a
custom icon, double-click opens Klipp), but the entry point is a
directory the user names with the `.klipp` suffix.

```
MyShortFilm.klipp/
├── project.json          ← the whole project: sequences, tracks, clips, grades
├── project.json.bak.1    ← the last few saves, rotated (`.bak.1` is newest)
├── media/
│   ├── manifest.json     ← the media pool: items and bins, paths relative to the project
│   ├── stills.json       ← index of the compare stills
│   └── stills/           ← the stills themselves, RGBA8 PNGs at preview resolution
└── looks/                ← looks saved from this project
```

`project.json` and `media/manifest.json` are the two files a `.klipp`
directory needs to load.

Nothing else lives here. Thumbnails, filmstrips, decoded audio,
analysis results and recordings are keyed by the media file rather
than by the project, so they sit in `~/.cache/klipp/` where two
projects cutting the same footage share one copy; models are in
`~/.local/share/klipp/models/`. Deleting either costs rebuild time and
nothing else.

## Atomic writes

All `.json` files (and the `stills.json` index) are written via
`klipp_project::path_util::atomic_write` — temp + rename, with the
temp filename including `pid` + `nanos` so concurrent saves don't
clobber each other's scratch files.

A power-cut mid-save leaves the previous valid `project.json` in
place; the half-written temp file is orphaned (next save overwrites
it).

## `project.json` — top-level shape

```jsonc
{
  // Format version. Bumped when an incompatible schema change ships.
  "version": "1",

  // Lowest Klipp build that can read this file. Set on save by the
  // current Klipp version and used by load() to refuse projects from
  // a future Klipp build that the running build can't parse.
  "klipp_version_min": "0.1.0",

  // Optional metadata.
  "klipp_version": "0.1.0+abc123",
  "saved_at":      "2026-04-27T20:11:41Z",
  "created_at":    "2026-04-15T10:23:00Z",
  "platform":      "linux-x86_64",

  // The project's sequences. Always at least one. Each sequence is
  // independently loadable as a timeline with its own fps, resolution,
  // tracks, and grade state.
  "sequences": [ /* SequenceJson */ ],

  // The sequence that was active when the project was last saved.
  // Restored on load. None → first sequence by storage order.
  "active_sequence_id": "uuid-string",

  // Free-form project notes (shot list, delivery specs, etc.).
  "notes": "",

  // Last UI state — restored on load so the workspace looks the same.
  "timeline_zoom":     100.0,
  "timeline_scroll_x": 0.0
}
```

### `SequenceJson`

```jsonc
{
  "id":         "uuid-string",
  "name":       "Main Sequence",
  "fps":        { "num": 24, "den": 1 },
  "resolution": { "width": 1920, "height": 1080 },
  "tracks":     [ /* TrackJson */ ],

  // Optional in/out range.
  "in_point":  { "value": 0,    "rate": 24 },
  "out_point": { "value": 5760, "rate": 24 },

  // Sequence-level color state.
  "global_lut_path":    "/abs/path/to/look.cube",
  "global_lut_opacity": 1.0,
  "global_lut_enabled": true,

  // Sequence markers.
  "markers": [ /* MarkerJson */ ],

  // UI state.
  "track_header_width": 160.0,
  "last_playhead":      { "value": 1234, "rate": 24 },

  // Per-project overrides for the global filmstrip / skim prefs.
  "auto_generate_filmstrips": null,
  "auto_generate_skim":       null
}
```

### `TrackJson`

```jsonc
{
  "id":   "uuid-string",
  "name": "Video 1",
  "kind": "Video",          // "Video" | "Audio" | "Text" | "Subtitle"
  "clips":       [ /* ClipJson */ ],
  "transitions": [ /* TransitionJson */ ],

  // Track-level mix state.
  "opacity":       1.0,
  "volume":        1.0,
  "muted":         false,
  "solo":          false,
  "visible":       true,
  "pan":           0.0,
  "locked":        false,
  "custom_height": null     // null = use kind-specific default
}
```

### `ClipJson`

The biggest single record on disk. A clip carries:

* **Identity & timing** — `id`, `kind` (Media / Title / Gap /
  Sequence / Generator / AdjustmentLayer), `timeline_in/out` and
  `source_in/out` as `i64` frames, `rate` (the clip's native fps,
  e.g. 24 for a 24p source).

* **Color graph** — `node_graph: Option<NodeGraph>` is the new format.
  `color_grade` is a legacy field kept only so old project files
  still parse; it's ignored on load.

* **Effects** — `effects: Vec<EffectJson>` (OFX plugins) and
  `builtin_effects` (legacy serde::Value blob — superseded by
  `node_graph`'s built-in node kinds).

* **Linkage** — `linked_clip_id` (audio↔video pair),
  `group_id` (multi-clip selection group),
  `multicam_group` + `multicam_angle` + `multicam_cuts`.

* **Audio mix** — `gain`, `volume`, `pan`, `fade_in_secs/curve`,
  `fade_out_secs/curve`, `channel_config`, `audio_effects`.

* **Per-parameter keyframe tracks** — `volume_keyframes`,
  `pan_keyframes`, `gain_keyframes`, `opacity_keyframes`, four
  `transform_*_keyframes`, `effect_keyframes` (string-keyed),
  `speed_keyframes`.

* **Per-parameter modifier stacks** — `param_modifiers`
  (Oscillate / Wiggle / etc. — see `klipp_core::ModifierStack`).

* **Speed** — `speed: f64` (constant) or `speed_keyframes` (variable).

* **Role / state** — `role` (color-coded label),
  `enabled` (skip during render but still visible on the timeline),
  `solo`, `source_rotation` (0/90/180/270),
  `track_points` (per-clip motion-tracker anchors).

* **Per-clip grades** — `grade_versions: Vec<GradeVersionJson>` plus
  `active_grade_version: Option<usize>`. Each version is a complete
  `NodeGraph` snapshot.

* **Per-clip markers** — `clip_markers`.

The serde struct uses `#[serde(default)]` on every optional field, so
older project files load cleanly (missing fields → defaults) and new
fields are forward-compatible.

### `TransitionJson`

```jsonc
{
  "id":        "uuid-string",
  "kind":      "CrossDissolve",   // see TransitionKind enum
  "duration":  { "value": 24, "rate": 24 },   // 1 second @24fps
  "clip_a_id": "uuid-string",
  "clip_b_id": "uuid-string"
}
```

The `kind` field is matched against the `TRANSITION_SPECS` registry
in `klipp-core::transition_registry`. A missing variant on load
falls back to `CrossDissolve` and emits a `tracing::error!` rather
than crashing.

### `MarkerJson`

```jsonc
{
  "id":       "uuid-string",
  "position": { "value": 240, "rate": 24 },   // 10s @24fps
  "label":    "Act 1",
  "color":    "Blue",                          // see MarkerColor enum
  "duration": { "value": 120, "rate": 24 }     // optional range marker
}
```

A point marker omits `duration`; a range marker carries it. Legacy
files without the field deserialize cleanly via serde defaults.

### `OfxValueJson`

OFX plugin parameter values are stored in **tagged form** so type is
unambiguous on round-trip:

```jsonc
{ "t": "Double",  "v": 1.0 }
{ "t": "Double2", "v": [0.5, 0.5] }
{ "t": "Int",     "v": 42 }
{ "t": "Bool",    "v": true }
{ "t": "Choice",  "v": 2 }
{ "t": "String",  "v": "hello" }
{ "t": "Rgba",    "v": [1.0, 0.0, 0.5, 1.0] }
```

Old projects used an untagged form (bare JSON values). The custom
`Deserialize` impl in `format.rs:348` falls back to that legacy
shape so old projects still load.

## `media/manifest.json` — media pool manifest

```jsonc
{
  "version": "1",
  "items":   [ /* MediaItemJson */ ],
  "bins":    [ /* BinJson */ ]
}
```

### `MediaItemJson` — one row per imported media file

* **Identity** — `id` (UUID, stable across saves), `path` (absolute
  on disk), `filename` (display).

* **Source metadata** — `duration_frames` + `duration_rate`, `fps`,
  `resolution`, `video_codec`, `audio_codec`, `has_audio`,
  `file_size_bytes`, `sample_rate`, `audio_channels`, `is_still`.

* **Probed metadata** — `camera_make`, `camera_model`, `encoder`,
  `bit_depth`, `video_bitrate_kbps`, `audio_bitrate_kbps`,
  `color_primaries`, `color_transfer`, `pixel_format`. Preserved
  across save/load so the log-format auto-detect heuristic still
  works on reload (without these, every reopen would forget the
  camera tags and fall back to sRGB on log-encoded sources).

* **User overrides** — `default_source_in/out` (per-media trim
  defaults), `rating` (0–5), `tags`, `user_notes`, `color_label`,
  `colour_space`, `source_rotation`, `log_format`.

* **Cache pointers** — `proxy_path`, `proxy_ready`.

* **Subclips** — `parent_media_id` is set on subclip MediaItems
  pointing at the parent. None on normal items.

* **Bin membership** — `bin_id` (UUID into `bins`) or legacy `bin`
  (string name).

* **Ingest timestamps** — `added_at`, `file_modified_at`,
  `media_created_at`.

### `BinJson`

```jsonc
{
  "id":        "uuid-string",
  "name":      "B-roll",
  "parent_id": "uuid-of-parent-bin"   // None for top-level
}
```

## `media/stills.json` — compare-stills index

A small JSON file pointing at the per-still PNGs:

```jsonc
[
  {
    "id":       "uuid-string",
    "filename": "<id>.png",
    "captured_at_secs": 12.500,
    "clip_id":  "uuid-string",
    "thumbnail_only": false
  }
]
```

The PNGs themselves are RGBA8 at the project's preview resolution.
`load_grade_stills` caps the number of entries at `MAX_STILLS` so a
malformed `stills.json` can't DoS the project loader.

## Template files

Templates live outside projects, in the user's config folder, one JSON
file each. The structs are in `klipp-timeline` and `klipp-core`.

### `.klipptitle` — a title template

`{name, category, written_by, format: 1, data}`, `data` being the
title (`TitleData`) with its published fields. A file of a newer
`format` is left out, not guessed at.

Format 2 adds `design`: `{anchor: [x, y], grow, fields, accent,
preview}`. `data` is then the layers as designed on a wide frame, and
Klipp makes the title as it makes a built-in: laid out again for a tall
or square frame around `anchor` (a centred one growing into the room
there when `grow`), its texts named by `fields` in stacking order, the
layer `accent` published as its accent colour, then Size and Speed.
`preview` is the browser card's text. A file is refused when `anchor`
is outside the frame, `fields` names more texts than it has or more
than 16, a name is blank or over 64 characters, `accent` is past the
layers, `preview` is over 128 characters, it has over 256 layers, or a
design comes in format 1; any `.klipptitle` over 16 MB is not read.
Klipp's built-in title templates are such files, in `assets/titles`
(#2886). A save still writes format 1.

A design may also carry (#2886):

* `optional` — `[{field, edits}]`: fields that may be left empty. An
  empty one gets its edits and its layer is taken out.
* `variants` — `[{name, options: [{name, edits}]}]`: named choices; the
  first option is taken when none is chosen.
* `place_by` — the layer the title is placed by: put at a point, or the
  same point of a box (a shot's picture), it moves there and every other
  layer keeps its distance from it.

An edit is `{pointer, value}`: the value at `pointer`, a JSON pointer
into `data`, replaced. It may reach only the layers (`/objects/...`) and
the title's `/background` and `/protect`, and the value must be there
already. Edits are made in this order: each variant's chosen option, in
the order the variants are listed (a later variant's edit of the same
value wins); then the field texts; then each empty optional field's
edits, in the order `optional` lists them, on the layers as they stand
(so they come after every variant); last, the empty fields' layers are
taken out. Then, as what makes it needs, the title is placed, laid out
for the frame and published. A file is refused when an edit reaches
anything else or a value not there, when an option's or optional field's
edits alone leave no title or change the number of layers, with over 64
edits in one option, over 8 variants or 8 options in one, a blank, long
or repeated variant or option name, an optional field that is not one of
`fields`, or a `place_by` past the layers. Klipp's own lower third is
such a file, `assets/titles/ops/lower_third.klipptitle`, which only the
`lower_third` op reads.

### `.klipptemplate` — a composition template or a recipe

`CompositionTemplate`, `format_version: 2` (format 1 only named its
composition and is refused with a hint to save it again):

* `id`, `name`, `category`, `written_by`.
* `kind` — `composition` (the default, left out) or `recipe`.
* `composition_id` and `sequences` — a composition carries its
  sequence and every sequence it nests, so it inserts into any project.
* `graph` — a recipe's `{ops, checks, check_with}` instead: graph ops
  addressed from anchors or `{"new": k}` (`modify` among them, putting
  behaviours on a node's parameter, and `keys`, its keys, at most 256,
  each a finite `time_secs`, seconds into the clip, and `value`, and an
  `ease` into the next if it has one; `{"anchor": "placement"}` is the
  clip's placement, which only a `keys` op names, keying its position,
  scale, rotation or tilt as the clip's own keyframes; an `add` may carry `entry`, the
  node's whole stored entry, as a recipe saved from nodes does, refused
  unless it reads as that node's), the harness's checks, and the
  control values the checks run with where the test shot needs other
  values than the defaults.
* `parameters` — by name: `{label, default, range?, choices}`, the
  default a `TemplateValue` (`Text`, `Color`, `Number`, `Bool`, `Enum`).
* `slots` — by name: `{label, kind}`, kind `media`, `clip`, `title`,
  `subject_matte`, `colour` or `surface` (a surface tracked in the clip,
  by name or id).
* `bindings` — `{parameter, property, link?}`: a parameter or slot to a
  property path (`object:<i>.text|color|font|picture|size|opacity|…`,
  `clip:<n>.media`, `node:<op>.<param>`, or `node:<op>.<param>~<field>`
  for a field of the first behaviour a recipe's `modify` op puts on that
  parameter, as a wiggle's `amplitude`, or `node:<op>.<param>/<pointer>`
  for one value inside that parameter by JSON pointer, as a mask
  rectangle's `masks/masks/0/shape/Rectangle/center_x`, set where the
  recipe is placed, or `node:<op>.<param>#<i>.time_secs|value` for the
  time or value of key `i` a recipe's `keys` op puts on that parameter,
  or `keys:<op>#<i>.time_secs|value|ease` for key `i` of the `keys` op
  `op` itself, the placement's among them);
  `take: "hue"` sets it to a
  colour control's hue in degrees, as a qualifier reads that colour
  picked on the picture. With `link: {to: [a, b],
  curve}` the parameter is a macro control: its range maps onto `to`
  on `linear`, `ease_in`, `ease_out` or `ease_in_out`. A recipe's link
  may say what `to` is measured against, `per`: `width` or `height`
  (fractions of the frame, made pixels of the sequence), or `source`
  (seconds into the clip, made its media's own seconds, so a picked
  frame stays on its frame through a trim; a time past the clip's end is
  refused).

### The placed instance

A placed composition is a clip of kind `TemplatePrecomposition
{sequence_id, template_instance}`; a placed recipe keeps a
`RecipeInstance {name, instance, nodes}` on the node frame round its
nodes. `TemplateInstance` is:

* `template_id`, `bindings` (plain ones, by parameter), `overrides`
  (each parameter's value; a control's value is a `Number`),
  `media_slots` (slot name → media id).
* `links` — one per controlled property: `{parameter, property, from,
  to, curve, offset, linked, frozen}`. While `linked`, the property is
  the control's value mapped from `from` to `to` on the curve, plus
  `offset` (what a hand edit left). Unlinked, it is `frozen`, the
  value it had. Relinking sets the offset that keeps that value.

### `.klipppack` — a template pack

A zip (stored):

* `pack.json` — `{format: 1, name, written_by, titles, templates,
  fonts, pictures}`: `titles` are `.klipptitle` contents, `templates`
  `.klipptemplate` contents (compositions and recipes), whole. `fonts`
  lists each family used: `{family, catalogue: true}`, fetched by name
  where the pack is used, or `{family, files, licence}` carried in it.
  `pictures` lists `{file, licence}`.
* `assets/pictures/<n>-<name>` and `<file>.licence.txt` — each picture
  a template draws; the templates' image paths point here.
* `assets/fonts/<family>/` — a carried font's files and `LICENCE.txt`.

Import writes the templates to the user's folders, the pictures to
`template_assets/<pack>/` (repointing the templates at them) and
carried fonts to `<fonts>/packs/<family>/`. Code:
`crates/klipp-project/src/template_pack.rs`.

## Versioning & forward-compatibility

* `version` is a single string ("1" today). Bumped on incompatible
  schema changes; load() refuses to open a project whose
  `klipp_version_min` exceeds the running build.

* New optional fields are added via `#[serde(default)]` so older
  projects load cleanly.

* Removed fields are tolerated by `#[serde(deny_unknown_fields)]`
  being **off** at the top level — serde silently drops unknown
  keys. This is deliberate: it lets us prune fields without breaking
  newer Klipp versions that try to load older projects.

## Reading the source

The single source of truth for the schema is
`crates/klipp-project/src/format.rs`. The serde struct definitions
in that file are the spec; this document is a guide to interpreting
them. When the structs change, this document should change with them
in the same commit.
