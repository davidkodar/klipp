# Project Format

Klipp stores projects as `.klipp` *directories*, not single files. The directory contains JSON metadata plus lazy caches (thumbnails, filmstrips, audio decode, etc.). The full schema lives in `crates/klipp-project/src/format.rs`; this page is the user-facing summary.

## Directory Layout

```
MyProject.klipp/
├── project.json          ← the whole project: sequences, tracks, clips, grades
├── project.json.bak.1    ← the last few saves, rotated
├── media/
│   ├── manifest.json     ← the media pool: items, bins, paths relative to here
│   ├── stills.json       ← index of the compare stills
│   └── stills/           ← the compare stills themselves, as PNGs
├── looks/                ← looks saved from this project
├── fonts.json            ← the fonts a consolidated project carries
└── fonts/                ← their files and licences, a folder per family
```

`project.json` and `media/manifest.json` are the two files a project
needs to load.

## What is *not* in the project

Everything that can be rebuilt from the source media lives in
`~/.cache/klipp/`, keyed by the media file rather than by the project,
so two projects using the same footage share one copy and deleting the
cache costs only time:

| `~/.cache/klipp/` | What |
|---|---|
| `filmstrips/` | timeline filmstrips |
| `audio/`, `audio-cache/` | decoded PCM and waveform peaks |
| `analysis/` | the analysers' results, one JSON per analyser and schema version |
| `recordings/` | screen recordings made from inside Klipp |
| `*_registry.json` | what the plugin and font scans found |

Models are separate again, in `~/.local/share/klipp/models/`.

## Why a Directory?

- **Plain-JSON state** is version-controllable and diffable
- **The human-editable state stays free of binary blobs**
- **A Klipp project is portable** — copy the `.klipp` directory and you
  have the edit; the caches rebuild themselves from the media

The `.klipp` extension exists so file managers can show a custom icon; under the hood it's just a directory.

## Atomic Writes

All `.json` files are written via the temp-file + rename pattern. The temp filename includes `pid` + nanosecond timestamp so concurrent saves don't clobber each other's scratch files.

A power-cut mid-save leaves the previous valid `project.json` in place; the half-written temp file is orphaned and the next save overwrites it.

## Versioning

Each save records:

- `format_revision` — the project format's revision, currently **4**
- `version` — the schema version string
- `klipp_version` — the writing build
- `saved_at` / `created_at` — ISO 8601 timestamps
- `platform` — e.g. `linux-x86_64`

The revision goes up only when a change would be *misread* by a build that knows the previous one (data moved or removed, not merely added). A build refuses a project from a newer revision, and a project from before revision 4 is refused with the name of the tool that converts it. A project whose grade uses node kinds this build does not know is refused rather than opened without them.

## Compatibility

- New optional fields are added with defaults, so a project without them loads cleanly.
- Opening a project and only playing it leaves it saved; a project whose files are unchanged stays saved.

## Project Settings

`project.json` carries:

- Active sequence ID (restored on load)
- Per-sequence colour state (working colourspace, ODT, Timeline LUT)
- Per-sequence track header width, scroll positions
- Project notes
- Last UI state (timeline zoom, scroll)

## Per-Sequence

Each sequence has its own:

- Frame rate (rational: `{ "num": 24, "den": 1 }`)
- Resolution
- Tracks (video / audio / text / subtitle)
- In/out range
- Markers
- Track header width
- Last playhead position

## Per-Clip

The biggest record on disk. A clip carries:

- **Identity & timing** — IDs, kind (Media / Title / Gap / Sequence / Generator / AdjustmentLayer and others), `timeline_in/out` and `source_in/out` as exact times at one rate that holds them all
- **Picture** — the clip's grade, effects and plug-ins as one node graph, each effect keeping its node id across saves (see [Node Graph](../color/node-graph.md))
- **Linkage** — audio↔video pair, multi-clip groups, multicam
- **Audio mix** — gain, volume, pan, fade in/out with curve type, channel config
- **Keyframes** — per-parameter keyframe tracks (volume, pan, opacity, transform, effects, speed)
- **Modifier stacks** — per-parameter (Oscillate, Wiggle, etc.)
- **Speed** — constant or variable (keyframed)
- **Role / state** — colour-coded label, enabled, solo, source rotation, track points
- **Grade versions** — multiple complete `NodeGraph` snapshots with one active
- **Per-clip markers**

Every optional field has a default, so a project without it loads.

## Source Colourspace Persistence

When Klipp auto-detects log encoding from camera metadata, the detected `colour_space` and `log_format` are persisted in `media/manifest.json` per `MediaItem`. This means:

- Reopening a project does not re-probe — detection is one-time at import
- A user override stored against a clip survives saves
- If you move a project to a machine where the source files are missing, the colourspace metadata is still in `media/manifest.json` and the relink dialog shows the right metadata

## Media References

Klipp references media files by path, not by copy (paths in `media/manifest.json` are relative to it). Missing media opens the relink flow. If you need a self-contained project (for archival, hand-off), use **File → Consolidate Project...** to copy the media into one place, optionally only what the cut uses. It also copies the font files its titles use, each with the licence beside it, into `fonts/`, listed in `fonts.json` by paths inside the project folder; opened on a machine without one of those families, the titles are drawn in the project's own copy. A font with no licence file beside it, or whose licence does not allow embedding, is left out and named in the report.

## See Also

The authoritative schema reference is `crates/klipp-project/src/format.rs`. When format structs change, this page should change in the same commit.

Titles containing editable cubic paths use the explicit `vector_title` clip-kind
tag and typed `title_data`. Older readers refuse this unknown kind by name,
rather than falling back to a flat text title and losing the path. Ordinary
text/rectangle/image titles retain their existing representation. Vector path
geometry, paint and topology are validated before legacy sequence recovery.

A title whose layers carry motion or effect stacks of their own (`fx` on a
`TitleObject`: motion, loops, glows, shadows, outlines, inner shadows, motion
blur — for text, shapes and images alike) uses the `title_v2` clip-kind tag.
Older readers refuse it by name rather than reading the title as still and
bare. A title whose layers all have an empty stack keeps the `title` or
`vector_title` tag an older reader knows. On load, the old per-text-box
effect fields are hoisted onto their object once; a text box that carries
them beside an explicit stack loses them to the stack, and a save writes
the stack only.
