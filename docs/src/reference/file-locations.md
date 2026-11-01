# File Locations

Quick reference for where Klipp stores its data on disk.

## User Configuration

`~/.config/klipp/`

| File | Purpose |
|---|---|
| `preferences.json` | Everything in **Preferences** |
| `keybindings.json` | The keyboard layout and your own shortcut changes |
| `layout.json`, `workspaces.json` | Panel layout and saved workspaces |
| `keys.toml` | Your Jamendo / Freesound API keys for the music and SFX search |
| `ai_tools.toml` | AI-tool settings and which models have run |
| `models.toml` | Optional override for the bundled AI model manifest |
| `themes/*.toml` | User-added themes |
| `presets/color/*` | Saved grades |
| `effect_presets/`, `hyperzoom_presets/` | Saved effect and hyperzoom presets |
| `presets/<kind>/*.toml` | Your own entries in Klipp's lists: see [Making Your Own Presets](your-own-presets.md) |
| `project_templates/` | Project templates, from **Save Project as Template...** |
| `shared_library/` | The Shared Library: collections of files for every project |

## User Data

`~/.local/share/klipp/` (or `$KLIPP_DATA_DIR`)

| Folder | Purpose |
|---|---|
| `title_templates/*.klipptitle` | Title templates you saved |
| `templates/*.klipptemplate` | Motion-graphics templates and recipes you saved |
| `template_assets/` | Pictures your templates use |
| `packs/<name>/` | Installed template packs |

See [Making Your Own Templates and Recipes](your-own-templates.md).

`preferences.json` survives Klipp updates. If you delete it, Klipp regenerates it with default values on next launch.

## User Cache

`~/.cache/klipp/`

| Directory / File | Purpose |
|---|---|
| `filmstrips/` | Timeline filmstrips |
| `thumbnails/` | Browser thumbnails |
| `audio/`, `audio-cache/` | Decoded PCM and waveform peaks |
| `analysis/` | Analyser results, one JSON per analyser and schema version |
| `ai_masks/` | AI Tools mask outputs |
| `proxies/` | Proxy renders |
| `recordings/` | Screen recordings made from inside Klipp |
| `*_registry.json` | What the font and plugin scans found (LV2, LADSPA, CLAP, frei0r) |

Model weights are not here: they live in `~/.local/share/klipp/models/`,
uncapped, because they are large and the AI Models dialog is where they
are removed. Letting a cache sweep delete one silently would be
surprising.

The frame and thumbnail caches survive Klipp updates and project changes — clearing them just forces re-decode on next play.

## Logs

`klipp-debug.log` — written to the working directory where Klipp was launched (typically the project root). Rotates if it exceeds 50 MB.

The log is a plain text file with `tracing` output: timestamps, log levels, target crates, structured fields. Filter by crate or level with `RUST_LOG`:

```bash
RUST_LOG=klipp_engine=debug,info cargo r
```

## Project Data

Inside each `.klipp` directory — see [Project Format](./project-format.md) for the full layout. The TL;DR:

- `project.json` — the whole project, plus rotated `.bak` copies
- `media/manifest.json` — the media pool
- `media/stills.json` and `media/stills/` — the compare stills
- `looks/` — looks saved from this project

Caches are not in the project; they are in `~/.cache/klipp/`, keyed by
the media file.

## System Library Probes

At startup, Klipp probes a list of paths for runtime libraries:

### libonnxruntime.so

In order:

1. `$ORT_DYLIB_PATH` (user override)
2. `~/.local/lib/onnxruntime/lib/libonnxruntime.so`
3. `~/.local/lib/libonnxruntime.so`
4. `/usr/lib/libonnxruntime.so`
5. `/usr/local/lib/libonnxruntime.so`
6. `/opt/onnxruntime/lib/libonnxruntime.so`

The first match is set as `ORT_DYLIB_PATH` for the rest of the process.

### Vulkan ICD

Klipp sets `VK_DRIVER_FILES=/usr/share/vulkan/icd.d/nvidia_icd.json` automatically on CachyOS / Arch where the NVIDIA ICD lives in a non-standard location.

## Auto-Set Environment Variables

Klipp's `main()` sets these at startup if not already configured:

| Variable | Value | Why |
|---|---|---|
| `RUST_MIN_STACK` | 1 GiB | Required for the release-profile LTO pass on klipp-app |
| `ORT_DYLIB_PATH` | resolved per the probe list above | So plain `cargo r` finds ort without a shell prefix |
| `VK_DRIVER_FILES` | resolved Vulkan ICD path | Vulkan loading on CachyOS |

If any of these are already set when Klipp launches, the existing value is respected — no overwriting.

## Where to Look When Things Go Wrong

| Symptom | Look at |
|---|---|
| AI inference on CPU | The AI Models dialog's footer; `klipp-debug.log` for `klipp_ml_ort` lines |
| Slow startup | `~/.cache/klipp/font_registry.json` — if missing, font scan re-runs (~16 s) |
| Crash on save | Check `pid`-stamped `*.tmp` files in `.klipp/` — orphaned scratch files from a failed atomic write |
| GPU effects not active | `klipp-debug.log` for `wgpu` adapter selection lines |
| Project won't open | `klipp-debug.log` for the deserialisation error; `project.json` is the canonical source |
