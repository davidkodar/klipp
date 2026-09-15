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

### Opacity
Controls transparency. Uses premultiplied alpha — opacity scales all four RGBA channels for correct compositing with underlying layers. Range 0–1.

### Gaussian Blur
Smooth Gaussian blur. Radius controls blur amount. Useful for background defocus, glow primitives, skin softening.

### Sharpen
Enhances edge detail. Use sparingly — over-sharpening introduces visible halos.

### Crop
Removes portions of the frame from any edge. Parameters: left / right / top / bottom in pixels or percentage. Cropped area becomes transparent, revealing layers below.

### Transform
GPU-accelerated position, scale, rotation. Same controls as the inspector's Transform section but executed in the effects chain so subsequent effects see the transformed frame.

### Chroma Key
Removes a specific colour (green screen / blue screen):

- **Key colour** — pick from the viewer or enter manually
- **Tolerance** — how far from the key colour to include
- **Edge softness** — feathering at the key boundary

## Additional Effects

- **Bloom** — light bleed from bright areas; threshold, intensity, radius
- **Light Rays** — volumetric rays from bright areas
- **Lens Flare** — simulated optical flare; position, intensity, flare elements
- **Film Halation** — red bloom around highlights, the way film stock glows
- **Noise** — static per-pixel noise; amount, mono/colour, seed

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

Every GPU effect has a CPU oracle implementation in `klipp_effects/tests/` that produces bit-identical (or float-identical-within-tolerance) output. The parity tests are part of the test suite; any GPU shader change must keep the oracle test green.

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
