# Picture residency coverage

The executable inventory is `klipp_readiness::residency`. Run its catalogue
probes with:

```sh
KLIPP_TEST_REQUIRE_GPU=1 cargo test -p klipp-readiness --test residency -- --nocapture
```

The classification is for an operation on the supported GPU composition path.
A GPU final composite does not establish that its inputs were generated on the
GPU. Audio DSP, file I/O, font discovery, geometry setup, decode and offline
analysis are separate concerns.

| Classification | Meaning |
| --- | --- |
| GPU | This operation keeps intermediate picture data in GPU buffers. |
| CPU cached | CPU shaping/raster/setup is reused for unchanged inputs; upload or GPU placement follows. |
| CPU per frame | Time-dependent pixels are produced on CPU and uploaded. |
| Round trip | GPU picture data returns to CPU before continuing on GPU. |
| Conditional | Another execution family or composition context determines the route. Read the reason. |

The inventory uses exhaustive matches for built-in effects, native generators,
transitions, title motions and loops. Tests account for fields of the title
text, style, transform, shape and image structures. Additions must be classified
explicitly. Nonresident and conditional paths name their reason and any open follow-up
issue; these exceptions are not claims that the CPU work has been fixed.

## What the probes establish

The catalogue test runs each GPU-classified built-in at its registered settings,
activates zero-radius blur, checks completion and chain breaks, and rejects intermediate readbacks. It also
runs every registered transition and checks intermediate transfers. The final
verification download is outside the measured operation. Equalize has a separate
positive control that must break the chain and increase readback counters.

These probes supplement the existing non-default CPU/GPU image-parity tests,
recorded-effect tests and M4 readiness families. Default parameters can be
identity operations; these probes alone do not prove every parameter range or
composition. M4 covers contextual source boundaries, plugins and analysis
nodes; do not treat a recorder's no-op/delegated arm as proof of residency.

A missing GPU is reported as unverified; setting `KLIPP_TEST_REQUIRE_GPU=1`
makes that a failure. Performance timings must be run separately from competing
builds, with GPU completion included. The title composition probe in
`klipp-compose` (`tracking_4k_cached_and_raster_composition_timing`) exercises 4K
tracking; it excludes decode, audio and UI presentation, so it cannot stand in
for full-project playback profiling.

## Known distinctions

- Equalize still breaks the GPU chain for histogram/CDF work (#1215).
- Frei0r's CPU pixel ABI is an explicit local CPU segment (#1465). Temporal
  denoise must be checked at its source-neighbour boundary (#1988).
- Native LivingGradient has a GPU stack path. Several other native generators
  cache CPU pixels; grain and timers rasterize with time (#2067). StillFrame rereads its
  file and is not a cached still-generator path (#2068). The Generator effect node has
  its own GPU kernel and must not be confused with native generators.
- Static title rasters can cache. Keyed typography and unsupported
  motion/style combinations can rerasterize on CPU (#2069). Eligible
  animated tracking reuses cached glyph coverage with GPU placement (#2062).
- Eligible animated single-text and 3D title styles compose on GPU (#2063).
  A bounded face cache avoids repeated raster/upload when only styles animate;
  eligible rotating 3D uses cached GPU geometry (#2061). Unsupported combinations
  retain the CPU composition path (#2069), with GPU blur/dilate callbacks and
  downloads above 250,000 mask pixels. Static finished title sprites still cache.
  CPU and GPU title masks use the same box-blur and fractional outline-radius
  rules. Clip glow/blur kernels remain a separate path.
- Title glitch changes pixels on CPU each frame (#2069). Static title chromatic
  treatment can cache; the similarly named clip effect has a GPU kernel.
- Unsupported composition or an unavailable/lost GPU can invoke CPU fallback.
  CPU twins remain necessary for fallback and parity; their existence alone is
  not evidence that normal GPU playback uses them.

Eligible plain animated 3D text uses a four-entry, 64 MiB geometry cache on
each of CPU and GPU. Camera, lighting and glyph poses update frame parameters;
text, layout, size, depth and bevel edits rebuild geometry. GPU rasterization
uses a 2×2 sample grid and bounded frame-sized colour/depth targets. It performs
no intermediate downloads. Hardware subpixel coverage can differ from the CPU
scan converter at triangle seams; tests bound those differences separately from
interior shading and verify viewer/export consistency. Bitmap glyphs, rich
fills, text paths, mixed objects and unsupported transforms retain CPU fallback.
