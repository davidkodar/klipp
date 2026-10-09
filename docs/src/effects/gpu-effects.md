# GPU Effects

Klipp uses `wgpu` compute shaders on the Vulkan backend for GPU-accelerated visual effects. On systems without Vulkan, effects fall back to CPU processing — same code path, same results, just slower.

## Performance

GPU effects run significantly faster than their CPU equivalents. Reference numbers from an RTX 4080 at 1080p:

| Effect | CPU | GPU | Speedup |
|---|---|---|---|
| Gaussian Blur | 60–100 ms | 4–7 ms | ~15× |
| Sharpen | ~30 ms | ~2 ms | ~15× |
| Chroma Key | ~12 ms | <1 ms | ~12× |
| Opacity | 1–2 ms | <0.5 ms | small effect, small win |

This makes real-time playback with multiple stacked effects practical at 1080p, and possible at 4K with judicious effect counts.

## Built-in Effects

The built-in effects are the variants of `BuiltinEffect` in `crates/klipp-effects/src/lib.rs`. Parameters are listed by their saved names. Positions and centres are fractions of the frame (0–1); a radius, length or distance in pixels is measured at the sequence's size and scaled with the frame when a preview or proxy renders smaller.

### Geometry and layer

| Effect | Parameters | What it does |
|---|---|---|
| Crop | `left`, `right`, `top`, `bottom` (fractions of the frame), `feather` (px) | Removes edges of the picture, with an optional soft edge |
| Transform | `position_x`, `position_y` (px from centre), `scale_x`, `scale_y`, `rotation` (degrees), `anchor_x`, `anchor_y`, `rotation_x`, `rotation_y`, `perspective`, `focal_ref` | Position, scale and rotation, including 3D tilt with perspective; `focal_ref` can pin the anchor to a title glyph or a tracker |
| Corner Pin | `tl`, `tr`, `br`, `bl` (each `[x, y]`) | Pins the picture's corners to four points; see below |
| Opacity | `value` (0–1) | Transparency, applied to all four premultiplied channels |
| Colour Fade | `color` (RGBA), `duration_secs`, `mode` (`in` / `out`) | Fades from a colour at the clip's start, or to it at the end |
| Rounded Corners | `radius`, `feather` (fractions of the shorter side) | Cuts rounded corners out of the layer's alpha |
| Drop Shadow | `offset_x`, `offset_y`, `blur` (fractions of the width), `color`, `opacity` | A shadow of the layer's opaque parts, drawn behind it |
| Outer Glow | `radius`, `intensity`, `color` | Light spilling out past the layer's edge |
| Mirror | `horizontal`, `vertical` (0 or 1) | Flips the picture |
| Lens Distortion | `distortion` (−1 barrel to 1 pincushion), `scale` | Barrel or pincushion distortion |
| Letterbox | `aspect_ratio`, `bar_color` (0 black, 1 white), `opacity` | Bars that matte the frame to an aspect ratio |

### Blur, sharpen and noise

| Effect | Parameters | What it does |
|---|---|---|
| Gaussian Blur | `radius` (px) | Smooth Gaussian blur |
| Directional Blur | `angle`, `distance`, `samples` | Motion blur along an angle |
| Radial Blur | `center_x`, `center_y`, `amount`, `samples`, `mode` (0 zoom, 1 spin) | Zoom or spin blur about a point |
| Tilt-Shift | `center`, `width`, `softness`, `radius` | A sharp band across the frame, blurred beyond it; see below |
| Sharpen | `amount` (0–2) | Edge sharpening; over-sharpening shows halos |
| Denoise | `spatial_sigma`, `color_sigma`, `iterations` | Edge-preserving bilateral filter |
| Median Filter | `radius` (1–5; 2 at most on the GPU) | Median of the neighbourhood, for impulse noise |
| Temporal Noise Reduction | `strength`, `threshold` | Averages each pixel with the neighbouring frames, keeping moved pixels as they are. Runs at the head of the clip's pipeline, before the grade, wherever it sits in the list. Display-referred sources only for now |
| Noise | `amount`, `monochrome` (0 colour, 1 grey), `seed` | Static per-pixel noise |
| Dilate | `radius` (1–10) | Morphological dilation: bright areas grow |
| Erode | `radius` (1–10) | Morphological erosion: dark areas grow |

### Light and lens

| Effect | Parameters | What it does |
|---|---|---|
| Bloom | `threshold`, `radius`, `intensity` | Bright areas blurred and added back |
| Glow | `radius`, `intensity` | Soft light wrapped around bright areas |
| Diffusion | `amount`, `radius`, `threshold`, `desaturation` | Pro-Mist style softening of the highlights |
| Light Rays | `center_x`, `center_y`, `threshold`, `samples`, `decay`, `intensity` | Rays from bright areas, radiating from a point |
| Lens Flare | `position_x`, `position_y`, `brightness`, `flare_size`, `ghost_count`, `ghost_intensity`, `chromatic_aberration` | A simulated flare with ghosts |
| Anamorphic Streak | `threshold`, `length` (px), `intensity`, `tint` (0 amber, 1 blue) | Horizontal streaks from bright areas |
| Film Halation | `threshold`, `radius`, `intensity`, `hue` | Red-orange bloom around highlights, as film stock glows |
| Light Wrap | `width` (px), `intensity`, `softness` | Background light bled onto the foreground's edges |
| Vignette | `amount` (−1 black to 1 white), `size`, `softness`, `roundness` | Darkens or lightens the edges and corners |
| Chromatic Aberration | `red_shift`, `blue_shift` (px), `center_x`, `center_y` | Radial separation of the red and blue channels |

### Colour and tone

| Effect | Parameters | What it does |
|---|---|---|
| Levels | `in_black`, `in_white`, `gamma`, `out_black`, `out_white` | Input and output range remap with gamma |
| Gamma | `gamma` (0.1–5) | Gamma correction per channel |
| HSV Adjust | `hue_shift` (degrees), `saturation`, `value` | Hue rotation and saturation and value multipliers |
| Vibrance | `amount` (−1 to 2) | Saturation that spares already-saturated colours |
| Channel Mixer | `red_red` … `blue_blue` (nine gains) | A 3×3 matrix on R, G and B |
| Equalize | `strength` | Histogram equalisation |
| Invert | `mix` | Inverts RGB, mixed with the original |
| Posterize | `levels` (2–256) | Reduces each channel to a number of levels |
| Chroma Key | `key_color`, `tolerance`, `soft_edge`, `spill_method`, `spill_strength` | Keys out a colour; see below |

### Stylise

| Effect | Parameters | What it does |
|---|---|---|
| Mosaic | `block_size` (px) | Pixelation in square blocks |
| Edge Detect | `strength`, `mix` | Sobel edges, mixed with the original |
| Emboss | `angle`, `strength`, `mix` | A relief lit from an angle |
| Film Grain | `amount`, `size`, `color` (0 mono, 1 per channel), `shadows`, `seed` | Animated emulsion grain; see below |
| Old Film | `flicker`, `scratches`, `dust`, `weave`, `sepia`, `seed` | Flicker, scratches, dust, gate weave and sepia; see below |
| VHS | `tracking`, `bleed`, `noise`, `scanlines`, `seed` | Tracking error, chroma bleed, tape noise and scanlines; see below |

### Time and camera

| Effect | Parameters | What it does |
|---|---|---|
| Video Stabilization | `smoothing`, `crop_ratio`, `zoom` | Smooths camera motion. Needs the clip analysed first (*AnalyzeStabilization*) |
| Rolling Shutter Correction | `strength`, `readout_ratio` | Removes CMOS wobble |
| Deflicker | `strength`, `window_secs`, `gain` | Undoes whole-frame brightness pumping. Needs the clip analysed first (*AnalyzeDeflicker*); the compositor sets `gain` per frame, and 1.0 (no correction) is what an unanalysed clip gets |
| Speed Ramp | `speed`, `blend_mode` (0 nearest, 1 linear) | Retiming with optional frame blending; there is no optical-flow mode |

### Plugins

| Effect | Parameters | What it does |
|---|---|---|
| frei0r plugin | `type_id`, `params` (one value per plugin parameter) | Runs an installed frei0r 1.x plugin; shown under the plugin's own name. CPU only: a chain with one does not run on the GPU. Above 2048 px a spatial plugin renders in tiles and a temporal one passes the frame through |

### Corner Pin
Pins the picture to four corners — where its top-left, top-right, bottom-right and bottom-left land, as fractions of the frame — so it sits on a screen, a sign or a wall in the shot with that surface's perspective. Straight lines stay straight. Outside the four corners is transparent, and corners that make no quadrilateral draw nothing. Each coordinate is keyframable from its row in the inspector. With on-canvas controls on, the four corners are handles in the viewer: drag one to move it, and a drag is one undo step. The pin runs before the clip's transform, with the picture, so it scales and moves with the layer.

### Chroma Key
Removes a specific colour (green screen / blue screen):

- **Key colour** — pick from the viewer or enter manually
- **Tolerance** — the CbCr distance from the key colour inside which a pixel is keyed out completely
- **Soft edge** — the band beyond the tolerance over which alpha ramps from 0 to 1 (saved projects may call it `spill_suppression`; it only ever shaped the matte)
- **Spill method** — how key-colour spill is removed from the foreground after keying: *None*, *Channel Clamp* (the key channel clamped towards the larger of the other two; conservative) or *Hawk* (clamped towards their average; stronger, can dull foreground colours near the key hue)
- **Spill strength** — 0 leaves the foreground alone, 1 removes spill fully. New keys start with Channel Clamp at 0.6

## Film Effects

The animated film effects take a new pattern every frame: the compositor
sets their seed to the timeline frame number, so a scrub, playback and an
export all land on the same grain for a given frame.

- **Film Grain** — emulsion grain in cells sized in pixels at 1080p and scaled
  with the frame; **Shadows** weights it away from the highlights, **Colour
  grain** makes it per channel
- **Old Film** — flicker, vertical scratches that live for a few frames,
  dust specks, gate weave (the frame drifts by a few pixels) and a sepia wash,
  under a soft vignette
- **VHS** — per-row tracking jitter with a wandering tear band, chroma bleed
  (luma stays put, chroma is taken a few pixels to the left), tape noise and
  scanlines
- **Tilt-Shift** — a sharp band across the frame (**Focus** and **Band**,
  as fractions of the height) blurred beyond it by **Blur** with a
  **Softness** ramp; the blur runs into a scratch buffer and a blend pass
  mixes the original back in

## Pipeline Architecture

Effects use a ping-pong storage buffer pattern — the output of one effect becomes the input of the next, with two buffers swapping roles. No intermediate copies, no GPU↔CPU round-trips between effects.

The pipeline lives in `klipp_effects::GpuEffectsPipeline` and is held by `klipp-engine`. On a frame:

1. Source RGBA → GPU buffer A
2. Effect 1 reads A, writes B
3. Effect 2 reads B, writes A
4. … alternating until the chain is complete
5. Final buffer copies back to CPU for the egui texture

## CPU Parity

Every GPU effect has a CPU twin in `crates/klipp-effects/src/cpu.rs`, a transcription of its shader that works on the same linear-light frame. The grade shaders' twins are in `cpu_grade.rs` and the transitions' in `cpu_transition.rs`. The parity tests in `crates/klipp-effects/tests/` (`cpu_gpu_effect_parity.rs`, `cpu_gpu_transition_parity.rs` and others) render each case both ways and compare the results; a shader change needs the matching change to its twin to keep them green.

## Order in the Effects Chain

Effects are processed top-to-bottom in the inspector list. Drag to reorder. Some orderings matter:

- Blur after Chroma Key — softens key edges
- Blur before Sharpen — smooths first, then re-defines edges
- Transform before any pixel-domain effect — rotation+blur stacks correctly when rotation is first

## GPU Requirements

- Vulkan 1.1 or later
- **NVIDIA** — `nvidia-utils` with the Vulkan ICD installed
- **AMD** — RADV (Mesa, recommended) or AMDVLK
- **Intel** — ANV (Mesa)

On CachyOS / Arch, the NVIDIA ICD lives at `/usr/share/vulkan/icd.d/nvidia_icd.json` (not `/etc/vulkan/icd.d/`). Klipp sets `VK_DRIVER_FILES` automatically to find it.

## CPU Fallback

When the GPU is unavailable, Klipp processes effects on the CPU. The performance overlay (**P**) shows whether GPU effects are active. The fallback is real and tested — Klipp ships with a CPU-only mode for headless / CI testing.

## Premultiplied Alpha

All Klipp effects work in premultiplied alpha. This is what egui expects for `Color32`, and it's what gives correct results when blurring across an alpha edge. If you write a custom effect, scale all four channels by alpha — failing to do so on opacity in particular causes a "halo" around edges that's surprisingly visible against busy backgrounds.

## WGSL Uniform Layout Gotcha

`array<u32, N>` in WGSL uniform blocks fails naga validation. When defining effect parameters, use individual `_pad0: u32` fields instead. This is a one-time gotcha; once you know it, the rest of the shader pipeline is straightforward.
