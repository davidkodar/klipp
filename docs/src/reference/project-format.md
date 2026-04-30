# Project Format

Klipp stores projects as `.klipp` *directories*, not single files. The directory contains JSON metadata plus lazy caches (thumbnails, filmstrips, audio decode, etc.). The full schema lives in `crates/klipp-project/src/format.rs`; this page is the user-facing summary.

## Directory Layout

```
MyProject.klipp/
├── project.json          ← top-level project state (sequences, clips, grades)
├── media.json            ← media pool manifest (MediaItems, bins)
├── thumbnails/           ← per-MediaId thumbnail caches (lazy)
├── filmstrips/           ← per-MediaId filmstrip strips (lazy)
├── audio_cache/          ← per-MediaId decoded f32 PCM (lazy)
├── proxies/              ← per-MediaId proxy renders (optional)
├── grade_stills/         ← compare-still PNGs + index
├── analysis/
│   ├── stabilization/    ← per-clip motion data
│   └── scenedetect/      ← per-clip scene boundaries
└── render_cache/         ← grade-cache LZ4 frames (optional, transient)
```

`project.json` and `media.json` are the only files **required** to exist for a project to load. Everything else is regenerated lazily from the source media.

## Why a Directory?

- **Plain-JSON state** is version-controllable and diffable
- **Lazy caches stay separate** so the human-editable state doesn't get mingled with binary blobs
- **Proxies and analysis live alongside the project** so they survive a project move
- **A Klipp project is portable** — copy the `.klipp` directory and you have everything

The `.klipp` extension exists so file managers can show a custom icon and double-click can open Klipp; under the hood it's just a directory.

## Atomic Writes

All `.json` files are written via the temp-file + rename pattern. The temp filename includes `pid` + nanosecond timestamp so concurrent saves don't clobber each other's scratch files.

A power-cut mid-save leaves the previous valid `project.json` in place; the half-written temp file is orphaned and the next save overwrites it.

## Versioning

Format version is currently `"1"`. Each save records:

- `version` — schema version
- `klipp_version_min` — lowest Klipp build that can read this file
- `klipp_version` — the writing build
- `saved_at` / `created_at` — ISO 8601 timestamps
- `platform` — e.g. `linux-x86_64`

`load()` refuses projects whose `klipp_version_min` exceeds the running build — prevents silent forward-compatibility breakage when a newer Klipp introduces a schema field.

## Forward Compatibility

- New optional fields are added via `#[serde(default)]` — older projects load cleanly
- Unknown keys are silently dropped on load — newer Klipp builds can read projects that include fields removed in later versions

## Project Settings

`project.json` carries:

- Active sequence ID (restored on load)
- Per-sequence colour state (working colourspace, ODT, global LUT)
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

- **Identity & timing** — IDs, kind (Media / Title / Gap / Sequence / Generator / AdjustmentLayer), `timeline_in/out` and `source_in/out` as `i64` frames at the clip's native rate
- **Colour graph** — per-clip `NodeGraph` (see [Node Graph](../color/node-graph.md))
- **Effects** — OFX plugin chain plus built-in effect node references
- **Linkage** — audio↔video pair, multi-clip groups, multicam
- **Audio mix** — gain, volume, pan, fade in/out with curve type, channel config
- **Keyframes** — per-parameter keyframe tracks (volume, pan, opacity, transform, effects, speed)
- **Modifier stacks** — per-parameter (Oscillate, Wiggle, etc.)
- **Speed** — constant or variable (keyframed)
- **Role / state** — colour-coded label, enabled, solo, source rotation, track points
- **Grade versions** — multiple complete `NodeGraph` snapshots with one active
- **Per-clip markers**

Every optional field uses `#[serde(default)]` for forward / backward compatibility.

## Source Colourspace Persistence

When Klipp auto-detects log encoding from camera metadata, the detected `colour_space` and `log_format` are persisted in `media.json` per `MediaItem`. This means:

- Reopening a project does not re-probe — detection is one-time at import
- A user override stored against a clip survives saves
- If you move a project to a machine where the source files are missing, the colourspace metadata is still in `media.json` and the relink dialog shows the right metadata

## Media References

Klipp references media files by absolute path, not by copy. Moving or renaming source files triggers a relink dialog on next open. If you need a self-contained project (for archival, hand-off), use **File → Consolidate Media** to copy referenced files into the project's `media/` subdirectory.

## See Also

The authoritative schema reference is `crates/klipp-project/src/format.rs`. When format structs change, this page should change in the same commit.
