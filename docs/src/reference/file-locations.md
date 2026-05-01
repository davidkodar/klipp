# File Locations

Quick reference for where Klipp stores its data on disk.

## User Configuration

`~/.config/klipp/`

| File | Purpose |
|---|---|
| `preferences.json` | All user preferences from **Edit → Preferences** |
| `keybindings.json` | Custom keyboard shortcut overrides |
| `themes/*.toml` | User-added theme files |
| `models.toml` | Optional override for the bundled AI model manifest |
| `title-presets/*.json` | User-saved title presets |
| `export-presets/*.json` | User-saved export presets |
| `luts/*.cube` | User LUTs (auto-discovered) |

`preferences.json` survives Klipp updates. If you delete it, Klipp regenerates it with default values on next launch.

## User Cache

`~/.cache/klipp/`

| Directory / File | Purpose | Default cap |
|---|---|---|
| `frames/` | Frame cache for playback | 10 GB |
| `thumbnails/` | Browser thumbnails | 2 GB |
| `ai_masks/` | AI Tools mask outputs (PNG, atomic-written) | 2 GB (LRU-evicted) |
| `models/` | Downloaded AI model weights | uncapped |
| `font_registry.json` | Cached system font index | small (< 1 MB) |

The frame and thumbnail caches survive Klipp updates and project changes — clearing them just forces re-decode on next play.

`models/` is intentionally uncapped — model weights are large (40 MB to 224 MB each) and the AI Models dialog provides explicit uninstall. Letting cache eviction silently delete a model would be surprising.

## Logs

`klipp-debug.log` — written to the working directory where Klipp was launched (typically the project root). Rotates if it exceeds 50 MB.

The log is a plain text file with `tracing` output: timestamps, log levels, target crates, structured fields. Filter by crate or level with `RUST_LOG`:

```bash
RUST_LOG=klipp_engine=debug,info cargo r
```

## Project Data

Inside each `.klipp` directory — see [Project Format](./project-format.md) for the full layout. The TL;DR:

- `project.json` — top-level state
- `media.json` — media pool
- `thumbnails/`, `filmstrips/`, `audio_cache/` — lazy caches
- `analysis/` — stabilisation + scene detect output
- `grade_stills/` — comparison stills

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
| AI inference on CPU | `~/.cache/klipp/models/<model>/.telemetry.json`; AI Models dialog footer; `klipp-debug.log` for `klipp_ml_ort` lines |
| Slow startup | `~/.cache/klipp/font_registry.json` — if missing, font scan re-runs (~16 s) |
| Crash on save | Check `pid`-stamped `*.tmp` files in `.klipp/` — orphaned scratch files from a failed atomic write |
| GPU effects not active | `klipp-debug.log` for `wgpu` adapter selection lines |
| Project won't open | `klipp-debug.log` for the deserialisation error; `project.json` is the canonical source |
