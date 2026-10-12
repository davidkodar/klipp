# Generated project corpus

Run `scripts/corpus.sh all` to generate the inputs, check loading and repeated
saves, and compare rendered frames. FFmpeg, FFprobe, a C compiler and a working
GPU are required. Nothing is downloaded; native plugin fixtures are compiled
from the repository's test sources. Installed plugins are not used.

Outputs go in a fresh directory below `target/klipp-corpus/generated`;
`--out DIRECTORY` changes the parent directory. Generated media, saved projects,
plugin binaries and analysis caches stay there, outside version control.

The small 640×360, 24 fps projects cover edits, blur/sharpen (including a disabled
effect), a grade followed by an effect, OFX, frei0r, temporal denoise and deflicker.
Moving charts, frame-varying grain, flicker and stereo 48 kHz sync audio supply
their inputs. The audio check pins PCM samples; the pixel check covers pictures.

## Checks

- `generate` verifies stream metadata, decoded video frames, decoded PCM and
  canonical saved-project bytes against `generated.json`. Container hashes are
  excluded because FFmpeg versions can change them without changing samples.
- `open` verifies that strict loading changes no project file, file size or
  modification time. This exercises the loader, not the application's inspectors.
- `reopen` saves to a fresh copy, loads and saves again, and reports the first
  changed JSON path. Only save-owned stamps, equivalent relative file paths and
  media-pool ordering are normalized. Authored values, creation time and active
  sequence state remain checked.
- `pixels` reads the generated saved projects through the shared
  `klipp-render::pixel_check` routes: CPU, GPU, viewer stack and export. It holds
  each frame to its reference within one 8-bit level, including alpha, and checks
  frame coverage, route choices, YUV layout and declines. Each effect-family
  fixture must visibly differ from its bypassed control. Missing references fail.
- `all` runs these checks together. It never updates a reference.

The references are a compact PNG atlas and JSON index under
`references/generated/`, limited to 2 MiB together. They contain only generated
charts. They are not application screenshots or recordings of private projects.
This first atlas samples frame 12 at 256 pixels wide in each family on every
available route, except temporal denoise, which keeps frames 0 and 24 to cover
the media edge and a cross-second sample. Its active effect witness uses frame
24. All sampled frames must be non-flat and have no legacy-plan fallbacks;
only temporal's known missing-neighbour decline at frame 0 is accepted. Native plugins and temporal
effects can decline the stack route and use CPU export; those route choices and
CPU-kernel counts are pinned. The compact sampling does not establish exhaustive
cut, disabled-effect or frame-rate coverage.

## Deliberate reference updates

```
scripts/corpus.sh generate --record "reason for changing inputs"
scripts/corpus.sh pixels --record "reason for changing rendered output"
scripts/corpus.sh pixels --record "reason for changing one case" --case grade
```

Input recording generates twice in independent directories and refuses different
inputs. Pixel recording also renders twice and refuses changed coverage or
pictures beyond the one-level bar. It records the reason and input-contract hash.
Review reference changes with the source change. If references exceed the size
budget, reduce the sampled frames; do not relax the tolerance.

With `--case`, all cases still render and are checked. Only the named case's
reference samples change; other cases keep their exact stored samples, and a
regression in any of them prevents the update. Existing references are required.

`cargo test -p klipp-corpus` runs the full generated checkpoint. The gate
selects it for changes in its rendering and project dependencies.

The checks include planted writes, lost fields and a two-level frame change, so
those failures must be detected. This corpus does not yet establish full project
format, UI interaction, HDR, audio-render or real-footage coverage.
