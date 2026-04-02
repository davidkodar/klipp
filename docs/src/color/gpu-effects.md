# GPU Effects

Klipp uses wgpu compute shaders running on the Vulkan backend for GPU-accelerated visual effects. On systems without Vulkan support, effects automatically fall back to CPU processing.

## Performance

GPU effects run significantly faster than their CPU equivalents. For example, Gaussian Blur processes in 4-7ms on GPU (NVIDIA RTX 4080) compared to 60-100ms on CPU -- a 15x speedup. This makes real-time playback with multiple effects practical.

## Built-in Effects

### Opacity
Controls the transparency of the clip. Uses premultiplied alpha for correct compositing with underlying layers. Range: 0% (invisible) to 100% (fully opaque).

### Gaussian Blur
Smoothly blurs the image. Adjustable radius controls the blur amount. Higher radius values produce a stronger blur effect. Useful for background defocus, glow effects, or softening skin.

### Sharpen
Enhances edge detail in the image. The amount parameter controls sharpening intensity. Use sparingly -- over-sharpening introduces visible halos around edges.

### Crop
Removes portions of the frame from any edge. Parameters for left, right, top, and bottom crop in pixels or percentage. The cropped area becomes transparent, revealing layers below.

### Transform
GPU-accelerated position, scale, and rotation. Provides the same controls as the inspector's Transform section but processed entirely on the GPU for better performance in the effects chain.

### Chroma Key
Removes a specific color from the image (green screen / blue screen keying). Parameters:

- **Key color** -- The color to remove (pick from the viewer or enter manually)
- **Tolerance** -- How far from the key color to include in the key
- **Edge softness** -- Feathering at the key boundary to reduce hard edges

## Additional Effects

### Bloom
Simulates light bleeding from bright areas. Creates a soft glow around highlights. Controls for threshold, intensity, and radius.

### Light Rays
Generates volumetric light ray effects from bright areas in the frame.

### Lens Flare
Simulates optical lens flare artifacts. Position, intensity, and flare element controls.

### Film Grain
Adds photographic film grain texture to the image. Controls for grain size, intensity, and color/monochrome grain type.

## Effect Pipeline

Effects are processed using a ping-pong buffer pattern -- the output of one effect becomes the input of the next. This allows chaining multiple effects without intermediate copies.

Effects are processed in the order listed in the inspector. Drag to reorder effects in the chain.

## GPU Requirements

- Vulkan 1.1 or later
- NVIDIA: nvidia-utils with Vulkan ICD
- AMD: RADV (Mesa) or AMDVLK
- Intel: ANV (Mesa)

On CachyOS/Arch with NVIDIA, Klipp automatically locates the Vulkan ICD at `/usr/share/vulkan/icd.d/nvidia_icd.json`.

## CPU Fallback

When GPU is unavailable, Klipp processes all effects on the CPU. Performance is lower but all effects produce identical results. The performance overlay (**P**) shows whether GPU effects are active.
