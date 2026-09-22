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
├── project.json          ← top-level project state (sequences, tracks, clips, grades)
├── media.json            ← media pool manifest (MediaItems, bins)
├── thumbnails/           ← per-MediaId thumbnail caches (lazy)
├── filmstrips/           ← per-MediaId filmstrip strips (lazy)
├── audio_cache/          ← per-MediaId decoded f32 PCM (lazy)
├── proxies/              ← per-MediaId proxy renders (optional)
├── grade_stills/         ← compare-still PNGs + index
│   ├── stills.json       ← metadata for each still
│   └── *.png             ← per-still RGBA8 PNGs
└── render_cache/         ← grade-cache LZ4 frames (optional, transient)
```

`project.json` and `media.json` are the only files that are
**required** to exist for a `.klipp` directory to load. Everything
else is regenerated lazily from the source media.

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

## `media.json` — media pool manifest

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

## `grade_stills/stills.json` — compare-stills index

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
