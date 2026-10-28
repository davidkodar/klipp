//! Light on the GPU (#2890): bloom, glow, light rays, vignettes, lens
//! flare, anamorphic streaks, film halation and chromatic aberration, with
//! their params and shaders.

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct BloomThresholdParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) threshold: f32,
    /// 1: no threshold, the picture premultiplied by its alpha: a
    /// layer's light as it is seen, for a glow that spreads past its
    /// edges (#2703).
    pub(super) premultiply: u32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad_luma: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct BloomBlendParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) intensity: f32,
    /// Desaturation of the blended layer (screen blend only; Diffusion).
    pub(super) desat: f32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    /// 1: the light spreads past the layer's edges, raising its alpha
    /// where it lands (Glow, #2703).
    pub(super) lift_alpha: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct LightRaysParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) center_x: f32,
    pub(super) center_y: f32,
    pub(super) threshold: f32,
    pub(super) samples: u32,
    pub(super) decay: f32,
    pub(super) intensity: f32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad_luma: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct VignetteParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) amount: f32,
    pub(super) size: f32,
    pub(super) softness: f32,
    pub(super) roundness: f32,
    pub(super) _pad: u32,
    pub(super) _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct LensFlareParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pos_x: f32,
    pub(super) pos_y: f32,
    pub(super) brightness: f32,
    pub(super) flare_size: f32,
    pub(super) ghost_count: u32,
    pub(super) ghost_intensity: f32,
    pub(super) chromatic_aberration: f32,
    pub(super) _pad: u32,
    pub(super) _pad2: u32,
    pub(super) _pad3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct AnamorphicStreakParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) threshold: f32,
    pub(super) half_len: u32,
    pub(super) intensity: f32,
    pub(super) tint: f32,
    pub(super) _pad: u32,
    pub(super) _pad2: u32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad_luma: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(super) struct FilmHalationBlendParams {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) threshold: f32,
    pub(super) intensity: f32,
    pub(super) hal_r: f32,
    pub(super) hal_g: f32,
    pub(super) hal_b: f32,
    pub(super) _pad: u32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    pub(super) luma_yr: f32,
    pub(super) luma_yg: f32,
    pub(super) luma_yb: f32,
    pub(super) _pad_luma: u32,
}

// Bloom threshold — extract pixels above a luma threshold and
// scale them by `(luma − threshold) / (1 − threshold)`. Linear
// luma (BT.709) matches the coefficients' definition; thresholding
// in linear gives a physically-meaningful "light above N nits".
pub(super) const SHADER_BLOOM_THRESHOLD: &str = gpu_shader!("light/bloom_threshold.wgsl");

// Bloom blend — additive blend of the bright-extracted buffer onto
// the original. Linear add is physically correct for "combining
// light sources"; the sRGB u8 version clipped at 255 and produced
// the wrong midtone mix for HDR highlights.
pub(super) const SHADER_BLOOM_BLEND: &str = gpu_shader!("light/bloom_blend.wgsl");

// Glow blend — Screen blend between original and blurred layer.
// Screen formula 1 − (1−a)(1−b) is algebraically identical in any
// linear [0, 1] space; running it in linear makes the "lighten"
// curve match how real light stacks rather than how sRGB bytes add.
pub(super) const SHADER_GLOW_BLEND: &str = gpu_shader!("light/glow_blend.wgsl");

// Light rays (god rays) — radial sampling from a virtual light source,
// accumulates bright pixels (above threshold) along each ray, decays
// with distance. Linear luma matches the BT.709 coefficient domain,
// linear summation avoids gamma-darkening along the trail.
pub(super) const SHADER_LIGHT_RAYS: &str = gpu_shader!("light/light_rays.wgsl");

pub(super) const SHADER_VIGNETTE: &str = gpu_shader!("light/vignette.wgsl");

// Lens flare — synthetic additive disc glow + chromatic-abb'd
// ghost discs along the flare-to-centre axis. Adding synthetic
// light to linear RGB is physically correct; the u8 version added
// gamma-encoded bytes, which visibly under-weighted the glow
// (flares looked thin and under-saturated).
pub(super) const SHADER_LENS_FLARE: &str = gpu_shader!("light/lens_flare.wgsl");

// Anamorphic streak — horizontal threshold-accumulate-and-tint on
// bright pixels. Linear summation of light sources, then additive
// blend onto the original. Both benefit from linear: bright streaks
// don't dim-out through gamma averaging, and additive compositing
// is physically what anamorphic lenses produce.
pub(super) const SHADER_ANAMORPHIC_STREAK: &str = gpu_shader!("light/anamorphic_streak.wgsl");

// Film halation — Screen-blend the pre-blurred highlights onto the
// original, in the halation's colour (deep red to warm yellow, from
// `hue`) whatever colour the highlight was.
// Screen blend in linear matches how light spreads on actual film
// emulsion — additive glow that asymptotes at diffuse white.
pub(super) const SHADER_FILM_HALATION: &str = gpu_shader!("light/film_halation.wgsl");

pub(super) const SHADER_BINOCULAR_VIGNETTE: &str = gpu_shader!("light/binocular_vignette.wgsl");

pub(super) const SHADER_CHROMATIC_ABERRATION: &str = gpu_shader!("light/chromatic_aberration.wgsl");
