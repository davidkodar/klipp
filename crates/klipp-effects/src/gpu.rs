//! GPU-accelerated effects via wgpu compute shaders.
//!
//! Each effect runs as a compute shader dispatch on the GPU.
//! Frames are uploaded, processed through the effect chain, and downloaded.
//! Falls back to CPU if GPU is unavailable.

/// GPU power preference set from application preferences.
static GPU_POWER_PREF: std::sync::Mutex<Option<wgpu::PowerPreference>> = std::sync::Mutex::new(None);

/// Set the GPU power preference before pipeline initialization.
/// Options: HighPerformance (discrete), LowPower (integrated).
/// None uses HighPerformance as default.
pub fn set_gpu_power_preference(pref: Option<wgpu::PowerPreference>) {
    *GPU_POWER_PREF.lock().unwrap_or_else(|e| e.into_inner()) = pref;
}

/// A shader from `src/gpu/shaders/`, by its path there, as the build
/// script assembled it (#2890): its `//#include` lines replaced by the
/// shared pieces in `shaders/include/`. Compile time, like `include_str!`.
macro_rules! gpu_shader {
    ($path:literal) => {
        include_str!(concat!(env!("OUT_DIR"), "/shaders/", $path))
    };
}

use crate::BuiltinEffect;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info};

mod readbacks;
mod alpha_masks;
mod title_styles;
pub use title_styles::{TitleStyle,TitleFace,TitleFill,TitleMetal,TitleTexture,TitleGlyphPose,TitlePath};
pub use readbacks::{readback_stats, thread_readback_stats, ReadbackStats};
mod params;
mod slots;
pub use slots::{OverBudget, SlotId, SlotKey, SlotPool, SlotUsage};
pub use params::ParamSlots;
mod record;
pub use record::{converts_inside, slots_needed, EffectRecording, Recorded};
mod thumbs;
pub use thumbs::{ChainThumbs, MAX_CHAIN_THUMBS};

/// sRGB → linear-light (IEC 61966-2-1).
#[inline]
fn srgb_to_linear_scalar(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Linear-light → sRGB (IEC 61966-2-1).
#[inline]
fn linear_to_srgb_scalar(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// sRGB-u8 RGBA → linear-f32 RGBA (interleaved). Used at the u8 boundary
/// of the GPU pipeline now that storage buffers are native linear f32.
fn u8_rgba_to_linear_f32(rgba: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(rgba.len());
    for chunk in rgba.chunks_exact(4) {
        out.push(srgb_to_linear_scalar(chunk[0] as f32 / 255.0));
        out.push(srgb_to_linear_scalar(chunk[1] as f32 / 255.0));
        out.push(srgb_to_linear_scalar(chunk[2] as f32 / 255.0));
        out.push(chunk[3] as f32 / 255.0);
    }
    out
}

/// linear-f32 RGBA → sRGB-u8 RGBA (interleaved). Inverse of
/// [`u8_rgba_to_linear_f32`]; clamp happens at the u8 boundary.
fn linear_f32_rgba_to_u8(linear: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(linear.len());
    for chunk in linear.chunks_exact(4) {
        out.push((linear_to_srgb_scalar(chunk[0]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.push((linear_to_srgb_scalar(chunk[1]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.push((linear_to_srgb_scalar(chunk[2]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.push((chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    }
    out
}

/// Definition of a single mask for GPU dispatch.
///
/// All shape variants share the flat-field representation
/// (`shape_type` + cx/cy/p1/p2/p3 + feather + flags); the bezier
/// variant additionally carries a normalised-coord point list. The
/// pipeline pools every mask's points into a single storage buffer
/// at dispatch time, with `bezier_offset`/`bezier_count` per record
/// recording the slice — so there is no per-mask cap and no per-
/// point-count cap, just whatever the GPU storage-buffer size limit
/// allows (hundreds of thousands of points in practice).
pub struct GpuMaskDef {
    /// 0=circle, 1=rectangle, 2=linear_gradient, 3=bezier,
    /// 4=radial_gradient
    pub shape_type: u32,
    pub cx: f32,
    pub cy: f32,
    pub p1: f32,  // circle: radius, rect: width, grad: end_x, radial: inner
    pub p2: f32,  // rect: height, grad: end_y, radial: outer
    pub p3: f32,  // rect: rotation (degrees)
    pub feather: f32,
    /// bit 0 = invert, bit 1 = `warp` applies, bits 2-3 = how it joins
    /// the masks before it (`klipp_core::MaskCombine::code`), bits 8-15 =
    /// opacity (0-255)
    pub flags: u32,
    /// The soft edge's width inside the line (#74); `feather` is outside.
    pub feather_inner: f32,
    /// Closed-polygon control points for bezier shapes. Empty for
    /// every other shape. Coords are normalised (0..1) on width.
    pub bezier_points: Vec<(f32, f32)>,
    /// A window tracked in perspective (#238): the plane map, row by
    /// row, from a pixel's frame position now to where the shape is
    /// measured. See [`Self::with_warp`].
    pub warp: [f32; 9],
}

impl GpuMaskDef {
    pub fn circle(cx: f32, cy: f32, radius: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self {
            shape_type: 0, cx, cy, p1: radius, p2: 0.0, p3: 0.0, feather,
            flags: Self::make_flags(invert, opacity),
            bezier_points: Vec::new(),
            warp: [0.0; 9],
            feather_inner: 0.0,
        }
    }
    pub fn rectangle(cx: f32, cy: f32, w: f32, h: f32, rotation: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self {
            shape_type: 1, cx, cy, p1: w, p2: h, p3: rotation, feather,
            flags: Self::make_flags(invert, opacity),
            bezier_points: Vec::new(),
            warp: [0.0; 9],
            feather_inner: 0.0,
        }
    }
    pub fn linear_gradient(sx: f32, sy: f32, ex: f32, ey: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self {
            shape_type: 2, cx: sx, cy: sy, p1: ex, p2: ey, p3: 0.0, feather,
            flags: Self::make_flags(invert, opacity),
            bezier_points: Vec::new(),
            warp: [0.0; 9],
            feather_inner: 0.0,
        }
    }
    /// A round gradient (#2704): whole inside `inner` of the centre,
    /// gone past `outer`, as `klipp_core::MaskShape::RadialGradient`.
    pub fn radial_gradient(cx: f32, cy: f32, inner: f32, outer: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self {
            shape_type: 4, cx, cy, p1: inner, p2: outer, p3: 0.0, feather,
            flags: Self::make_flags(invert, opacity),
            bezier_points: Vec::new(),
            warp: [0.0; 9],
            feather_inner: 0.0,
        }
    }
    /// `points` are closed-polygon control vertices in normalised
    /// (0..1) width-relative coords. The polygon is closed by the
    /// shader (last → first edge added implicitly). Fewer than 3
    /// points produce an empty matte (CPU rasteriser does the same).
    pub fn bezier(points: Vec<(f32, f32)>, feather: f32, invert: bool, opacity: f32) -> Self {
        Self {
            shape_type: 3, cx: 0.0, cy: 0.0, p1: 0.0, p2: 0.0, p3: 0.0, feather,
            flags: Self::make_flags(invert, opacity),
            bezier_points: points,
            warp: [0.0; 9],
            feather_inner: 0.0,
        }
    }
    /// This window measured through `warp`, a plane map row by row (an
    /// evaluated window's `keyframes.warp` in klipp-core): a pixel is
    /// looked up where the map sends it, and one past its horizon is
    /// outside.
    pub fn with_warp(mut self, warp: Option<[f32; 9]>) -> Self {
        if let Some(w) = warp {
            self.warp = w;
            self.flags |= 2;
        }
        self
    }
    /// This window's soft edge reaching `inner` inside its line as well
    /// (#74).
    pub fn with_feather_inner(mut self, inner: f32) -> Self {
        self.feather_inner = inner.max(0.0);
        self
    }
    /// This window joining the ones before it as `code` says
    /// (`klipp_core::MaskCombine::code`: 0 add, 1 subtract, 2 intersect).
    pub fn with_combine(mut self, code: u32) -> Self {
        self.flags = (self.flags & !0b1100) | ((code & 3) << 2);
        self
    }
    fn make_flags(invert: bool, opacity: f32) -> u32 {
        let opacity_byte = (opacity.clamp(0.0, 1.0) * 255.0) as u32;
        (if invert { 1 } else { 0 }) | (opacity_byte << 8)
    }
}


/// GPU effects pipeline — holds device, queue, compute pipelines, and reusable buffers.
use crate::cpu::EQUALIZE_CDF_LEN;
use crate::cpu_grade::HUESAT_LUT_LEN;

/// The compute pipelines, their bind group layouts and the constant
/// tables they read (#2688): made once a device, compiled on its first
/// renderer and shared by every later one, so a new engine skips the
/// compile. Nothing in it is written after it is built.
pub(super) struct EffectPipelines {
    opacity_pipeline: wgpu::ComputePipeline,
    color_fade_pipeline: wgpu::ComputePipeline,
    mask_mix_gpu: mask_mix::MaskMixGpu,
    blur_h_pipeline: wgpu::ComputePipeline,
    blur_v_pipeline: wgpu::ComputePipeline,
    sharpen_pipeline: wgpu::ComputePipeline,
    crop_pipeline: wgpu::ComputePipeline,
    transform_pipeline: wgpu::ComputePipeline,
    corner_pin_pipeline: wgpu::ComputePipeline,
    chroma_key_pipeline: wgpu::ComputePipeline,
    bloom_threshold_pipeline: wgpu::ComputePipeline,
    bloom_blend_pipeline: wgpu::ComputePipeline,
    glow_blend_pipeline: wgpu::ComputePipeline,
    light_rays_pipeline: wgpu::ComputePipeline,
    vignette_pipeline: wgpu::ComputePipeline,
    /// The generator effect's pass (#1746).
    generator_pipeline: wgpu::ComputePipeline,
    living_gradient_pipeline: wgpu::ComputePipeline,
    /// The native Grain, Countdown and Timer generators in the stack
    /// (#2067).
    grain_generator_pipeline: wgpu::ComputePipeline,
    segments_pipeline: wgpu::ComputePipeline,
    film_grain_pipeline: wgpu::ComputePipeline,
    tilt_shift_blend_pipeline: wgpu::ComputePipeline,
    focus_blur_blend_pipeline: wgpu::ComputePipeline,
    focus_blur_premultiply_pipeline: wgpu::ComputePipeline,
    old_film_pipeline: wgpu::ComputePipeline,
    vhs_pipeline: wgpu::ComputePipeline,
    lens_flare_pipeline: wgpu::ComputePipeline,
    anamorphic_streak_pipeline: wgpu::ComputePipeline,
    film_halation_blend_pipeline: wgpu::ComputePipeline,
    invert_pipeline: wgpu::ComputePipeline,
    posterize_pipeline: wgpu::ComputePipeline,
    /// The colour-space transform effect's pass (#1746).
    cst_pipeline: wgpu::ComputePipeline,
    mosaic_pipeline: wgpu::ComputePipeline,
    edge_detect_pipeline: wgpu::ComputePipeline,
    channel_mixer_pipeline: wgpu::ComputePipeline,
    directional_blur_pipeline: wgpu::ComputePipeline,
    emboss_pipeline: wgpu::ComputePipeline,
    mirror_pipeline: wgpu::ComputePipeline,
    lens_distortion_pipeline: wgpu::ComputePipeline,
    displace_pipeline: wgpu::ComputePipeline,
    warp_pipeline: wgpu::ComputePipeline,
    gradient_map_pipeline: wgpu::ComputePipeline,
    reflection_pipeline: wgpu::ComputePipeline,
    dilate_pipeline: wgpu::ComputePipeline,
    erode_pipeline: wgpu::ComputePipeline,
    denoise_pipeline: wgpu::ComputePipeline,
    gamma_pipeline: wgpu::ComputePipeline,
    hsv_adjust_pipeline: wgpu::ComputePipeline,
    noise_generator_pipeline: wgpu::ComputePipeline,
    median_filter_pipeline: wgpu::ComputePipeline,
    radial_blur_pipeline: wgpu::ComputePipeline,
    levels_pipeline: wgpu::ComputePipeline,
    equalize_pipeline: wgpu::ComputePipeline,
    /// Equalize's histogram and CDF, built on the GPU in the chain
    /// (#1215): the frame's display-luma counts into `equalize_hist_buf`,
    /// then the CDF from them into `equalize_cdf_buf`, which the map pass
    /// reads.
    equalize_histogram_pipeline: wgpu::ComputePipeline,
    equalize_cdf_pipeline: wgpu::ComputePipeline,
    letterbox_pipeline: wgpu::ComputePipeline,
    chromatic_aberration_pipeline: wgpu::ComputePipeline,
    binocular_vignette_pipeline: wgpu::ComputePipeline,
    particles_pipeline: wgpu::ComputePipeline,
    rsc_pipeline: wgpu::ComputePipeline,
    stabilization_pipeline: wgpu::ComputePipeline,
    color_grade_pipeline: wgpu::ComputePipeline,
    huesat_pipeline: wgpu::ComputePipeline,
    huesat_bgl: wgpu::BindGroupLayout,
    /// Five bindings, for the one node that needs three pictures at
    /// once: input (the graded frame), output, params, the kept
    /// pre-secondary frame, and the matte.
    apply_matte_bgl: wgpu::BindGroupLayout,
    lut3d_pipeline: wgpu::ComputePipeline,
    color_balance_pipeline: wgpu::ComputePipeline,
    hdr_zones_pipeline: wgpu::ComputePipeline,
    wheel_sat_bri_pipeline: wgpu::ComputePipeline,
    rgb_curves_pipeline: wgpu::ComputePipeline,
    vibrance_pipeline: wgpu::ComputePipeline,
    scene_to_display_pipeline: wgpu::ComputePipeline,
    color_warper_pipeline: wgpu::ComputePipeline,
    clarity_blur_h_pipeline: wgpu::ComputePipeline,
    clarity_blur_v_pipeline: wgpu::ComputePipeline,
    clarity_apply_pipeline: wgpu::ComputePipeline,
    mask_pipeline: wgpu::ComputePipeline,
    /// Compute pipeline — mask cutout (chain frame × matte). 4-binding
    /// layout: input chain buffer, output chain buffer, params,
    /// matte_buf as the matte source. Lets the Source → Corrector →
    /// MaskCutout → Output chain stay GPU-resident through the cutout
    /// instead of bouncing to CPU for the multiply.
    mask_cutout_pipeline: wgpu::ComputePipeline,
    /// A grade's result mixed with the picture it started from (the
    /// Corrector's mix over all its stages), and that picture's copy.
    grade_mix_pipeline: wgpu::ComputePipeline,
    apply_matte_pipeline: wgpu::ComputePipeline,
    deflicker_pipeline: wgpu::ComputePipeline,
    blend_mix_pipeline: wgpu::ComputePipeline,
    merge_over_pipeline: wgpu::ComputePipeline,
    layer_over_pipeline: wgpu::ComputePipeline,
    qualifier_pipeline: wgpu::ComputePipeline,
    luma_key_pipeline: wgpu::ComputePipeline,
    difference_key_pipeline: wgpu::ComputePipeline,
    difference_key_bgl: wgpu::BindGroupLayout,
    /// Temporal noise reduction on the chain (#1988): the chain's picture
    /// against up to two neighbour frames in buffers of their own.
    temporal_denoise_pipeline: wgpu::ComputePipeline,
    temporal_denoise_bgl: wgpu::BindGroupLayout,
    soft_clip_pipeline: wgpu::ComputePipeline,
    grain_pipeline: wgpu::ComputePipeline,
    alpha_blur_h_pipeline: wgpu::ComputePipeline,
    alpha_blur_v_pipeline: wgpu::ComputePipeline,
    alpha_dilate_pipeline: wgpu::ComputePipeline,
    title_style_pipeline: wgpu::ComputePipeline,
    title_metal_shot_pipeline: wgpu::ComputePipeline,
    /// Contrast protection for titles (#2734) and its two small grids
    /// (measure, blur) with their cell count.
    contrast_protect_pipeline: wgpu::ComputePipeline,
    /// A pinned layer taking its surface's light and texture (#2751):
    /// its grid and blur, its row sums, and a frame of factors, with the
    /// cell and pixel counts they were made for.
    surface_light_pipeline: wgpu::ComputePipeline,
    transition_pipeline: wgpu::ComputePipeline,
    /// Posed piece transitions (#985): `SHADER_PIECES`.
    pieces_pipeline: wgpu::ComputePipeline,
    /// A layer smeared across the shutter, in one pass (#1923).
    shutter_pipeline: wgpu::ComputePipeline,
    bridge_u8_pipeline: wgpu::ComputePipeline,
    bridge_u8_to_linear_pipeline: wgpu::ComputePipeline,
    /// [`Self::bridge_u8_fit_to_linear_into_chain`]: the same decode, from
    /// a frame of another size, fitted into the chain's.
    bridge_u8_fit_pipeline: wgpu::ComputePipeline,
    /// The 8-bit bridges' decode table (`srgb8_to_linear`), alpha's after.
    srgb8_lut: wgpu::Buffer,
    /// The same bridge at 16 bits a sample, with the source primaries
    /// folded in: sRGB-encoded RGBA64 → linear working-space f32. This
    /// is how a software-decoded export frame reaches the chain, since
    /// its scaler's output does not fit in 8 bits.
    bridge_rgba64_to_linear_pipeline: wgpu::ComputePipeline,
    /// The 16-bit bridge's decode table (`srgb16_decode_table`), alpha's
    /// after.
    srgb16_lut: wgpu::Buffer,
    /// Graded linear frame → the encoder's Y'CbCr planes: display
    /// transform, colour matrix and chroma subsampling in one pass, so
    /// only the planes cross the bus.
    linear_to_yuv420_pipeline: wgpu::ComputePipeline,
    /// AcesCG → display-encoded Rec.709 SDR u8, the ACES output
    /// transform on the GPU rather than per-pixel on one core.
    aces_rec709_pipeline: wgpu::ComputePipeline,
    display_tables_bgl: wgpu::BindGroupLayout,
    /// Bound in that slot by a delivery that is not ACES, which never
    /// reads it.
    no_display_tables: wgpu::Buffer,
    no_light: wgpu::Buffer,
    bt2020_display_pipeline: wgpu::ComputePipeline,
    /// Packed RGB10 HDR code values → scene-linear working-space f32,
    /// written into the chain's `buf_a`. The GPU half of
    /// `klipp_color::hdr::hdr10_codes_to_scene_linear_rgba`.
    bridge_hdr10_pipeline: wgpu::ComputePipeline,
    /// The same for 16-bit codes, a source deeper than 10 bits (#1226).
    bridge_hdr16_pipeline: wgpu::ComputePipeline,
    /// Chain output (linear f32) → display sRGB u8, with the HDR display
    /// tone map applied first when asked. Lets a graded frame leave the
    /// GPU as 4 bytes a pixel instead of 16.
    display_u8_pipeline: wgpu::ComputePipeline,
    hdr_nit_map_pipeline: wgpu::ComputePipeline,
    /// `bridge_u8_out_buf` into a storage texture, one texel per
    /// invocation: the display bytes land in the viewer's texture
    /// without a copy, so no row-pitch alignment applies.
    u8_to_texture_pipeline: wgpu::ComputePipeline,
    u8_to_texture_bgl: wgpu::BindGroupLayout,
    sprite_place_pipeline: wgpu::ComputePipeline,
    /// The per-glyph sprite pass: sprite, output, params, glyph map,
    /// glyph table.
    sprite_glyphs_bgl: wgpu::BindGroupLayout,
    sprite_glyphs_pipeline: wgpu::ComputePipeline,
    blend_layer_pipeline: wgpu::ComputePipeline,
    premultiply_pipeline: wgpu::ComputePipeline,
    /// The viewer's channel view of the finished stack (#2758).
    view_channel_pipeline: wgpu::ComputePipeline,
    alpha_form_pipeline: wgpu::ComputePipeline,
    sprite_rows_pipeline: wgpu::ComputePipeline,
    sprite_rows_f16_pipeline: wgpu::ComputePipeline,
    sprite_box_f16_pipeline: wgpu::ComputePipeline,
    matte_extract_pipeline: wgpu::ComputePipeline,
    mask_min_h_pipeline: wgpu::ComputePipeline,
    mask_min_v_pipeline: wgpu::ComputePipeline,
    matte_apply_pipeline: wgpu::ComputePipeline,
    scale_alpha_pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,         // effects: input, output, params
    transition_bgl: wgpu::BindGroupLayout,             // transitions: from, to, output, params
    pieces_bgl: wgpu::BindGroupLayout,                 // posed pieces: from, to, output, params, pieces, matte
    /// The layer passes' pipelines.
    layer: Arc<layer::LayerPipelines>,
}

impl EffectPipelines {
    /// `device`'s pipelines: the ones an earlier renderer on it compiled,
    /// or compiled now and kept for the next. Each device's are kept while
    /// the device is: one replaced after a loss gets its own, and the old
    /// one's go with it. The compile takes over a second with the driver's
    /// shader cache cold, longer than a strobe's first gap (#2688).
    fn shared(device: &Arc<wgpu::Device>) -> Option<Arc<Self>> {
        static KEPT: std::sync::Mutex<Vec<(std::sync::Weak<wgpu::Device>, Arc<EffectPipelines>)>> = std::sync::Mutex::new(Vec::new());
        // Held while compiling, so two renderers made at once compile once.
        let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
        kept.retain(|(d, _)| d.strong_count() > 0);
        if let Some((_, p)) = kept.iter().find(|(d, _)| std::ptr::eq(d.as_ptr(), Arc::as_ptr(device))) {
            return Some(Arc::clone(p));
        }
        let built = Arc::new(Self::build(device)?);
        kept.push((Arc::downgrade(device), Arc::clone(&built)));
        Some(built)
    }
}

pub struct GpuEffectsPipeline {
    /// Shared with every renderer on the device (#2688).
    pipelines: Arc<EffectPipelines>,
    readbacks: readbacks::Counters,
    // Arc-wrapped so the effects pipeline can share its device with
    // klipp-effects' NV12/P010 converters (and any future GPU-using
    // type) through `GpuContext`. Internal call sites deref
    // transparently via Arc's Deref<Target = wgpu::Device>.
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    // Compute pipelines — effects (2-buffer: input, output, params)
    layer_gpu: layer::LayerGpu,
    /// Optical-flow interpolation's pipelines and kept pairs (#2215),
    /// made on first use.
    flow_gpu: Option<flow::FlowGpu>,
    // Compute pipelines — light effects
    // Compute pipelines — new effects
    /// The LUT effects' tables on the GPU, the latest last, each kept with
    /// the preparation it was uploaded from so a table is uploaded once.
    effect_luts: std::cell::RefCell<Vec<(Arc<crate::colour_transform::LutTable>, Arc<wgpu::Buffer>)>>,
    // Compute pipeline — rolling shutter correction (uses huesat 4-binding layout)
    rsc_row_buf: Option<wgpu::Buffer>,
    rsc_row_buf_size: u64,
    // Compute pipeline — video stabilization (uses huesat 4-binding layout)
    stabilization_buf: Option<wgpu::Buffer>,
    stabilization_buf_size: u64,
    // Compute pipelines — color grading (standalone, not part of effect chain)
    huesat_lut_buf: wgpu::Buffer,
    /// The equalize pass's 256-entry CDF.
    ///
    /// Its own buffer, deliberately. It used to be written over the
    /// start of `huesat_lut_buf`, which worked only because `upload`
    /// flushes before every write — so a pending hue/sat dispatch was
    /// always submitted before the CDF landed on top of the LUT it
    /// reads. Nothing at either end had to say so for the code to run,
    /// the two sites are thousands of lines apart, and the invariant
    /// lived in a private helper neither mentioned. Batching or
    /// deferring uploads would have broken grades, but only on a clip
    /// carrying both hue/sat curves and an Equalize effect.
    ///
    /// 1 KiB is not worth that.
    equalize_cdf_buf: wgpu::Buffer,
    /// Equalize's histogram, 256 counts, cleared in the encoder before
    /// each Equalize records its count into it (#1215).
    equalize_hist_buf: wgpu::Buffer,
    // Compute pipeline — 3D LUT (reuses huesat_bgl: input, output, params, lut storage)
    lut3d_buf: Option<wgpu::Buffer>,
    lut3d_buf_size: u64,
    // Compute pipeline — color balance (temperature/tint/hue)
    // Compute pipeline — wheel sat/bri (per-wheel saturation & brightness)
    // Compute pipeline — RGB curves (LUT-based, reuses huesat_bgl 4-binding layout)
    rgb_curves_lut_buf: wgpu::Buffer,
    // Compute pipeline — vibrance
    // Compute pipeline — a scene-referred clip's display step (step 11c)
    // Compute pipeline — color warper (12-sector hue/sat)
    // Compute pipeline — input transform (log-to-sRGB via 1D LUT, reuses huesat_bgl)
    // Compute pipelines — clarity (3-pass: blur_h, blur_v, apply)
    // Compute pipeline — mask matte generation (SDF power windows)
    /// The picture a Corrector's mix over all its stages blends against
    /// (its Keep/Mix stage pair, within the one node). A grade
    /// secondary's kept picture and a composite's second picture are
    /// pool slots ([`KeptPicture`], M3 step 17b).
    grade_mix_src: Option<wgpu::Buffer>,
    // Compute pipeline — qualifier (secondary color correction)
    // Compute pipeline — standalone luminance-threshold key (matte
    // producer, simpler than qualifier — luma in, matte out).
    /// The difference key's clean plate, resized to the frame on the
    /// host and uploaded once. Keyed by the host's own cache generation
    /// rather than by contents: comparing 33 MB to decide whether to
    /// upload 33 MB would defeat the purpose.
    plate_buf: Option<wgpu::Buffer>,
    plate_key: Option<(u64, u32, u32)>,
    /// A difference key recorded in the open chain reads `plate_buf`
    /// (#1266): a different plate can't go in until that chain is
    /// submitted, since an upload lands before the whole encoder runs.
    plate_read_in_chain: bool,
    // Compute pipeline — standalone soft-clip / hard-clip transform
    // (per-channel Reinhard rolloff above threshold). Value
    // transform, not a matte producer.
    // Compute pipeline — film grain overlay
    // Compute pipelines — alpha mask operations (for title renderer)
    title_style_scratch: Option<title_styles::StyleScratch>,
    /// Metal titles that reflect the shot behind them, by stack slot,
    /// waiting for the layers below to be laid (#2706); emptied as a
    /// frame's stack begins.
    title_metal_shots: std::collections::HashMap<usize, title_styles::MetalShot>,
    title_metal_shot_scratch: Option<(wgpu::Buffer, u64)>,
    contrast_protect_scratch: Option<(wgpu::Buffer, wgpu::Buffer, u64)>,
    surface_light_scratch: Option<surface_light::Scratch>,
    title_post_scratch: Option<wgpu::Buffer>,
    title_scene_bounds: Option<wgpu::Buffer>,
    title_outlines: Vec<(u64,wgpu::Buffer)>,
    // Compute pipelines — transitions (3-buffer: from, to, output, params)
    // ── linear-f32 → sRGB-u8 pixel encode pipeline ─────────────────
    // GPU replacement for the CPU `linear_rgba_to_u8` LUT pass that
    // costs 40-90 ms per 4K frame. Reads `array<vec4<f32>>` linear,
    // writes `array<u32>` packed sRGB-u8 RGBA. Output buffer is 4×
    // smaller than the input, which matters because the readback
    // dominates wall-time on this op (PCIe-bound, sub-ms compute).
    bridge_u8_out_buf: Option<wgpu::Buffer>,
    bridge_u8_out_size: u64,
    bridge_u8_staging_buf: Option<wgpu::Buffer>,
    bridge_u8_staging_size: u64,
    // ── sRGB-u8 → linear-f32 pixel decode pipeline ─────────────────
    // The opposite of `bridge_u8_pipeline`: takes packed u8 RGBA
    // (from a decoder's NV12/P010 output buffer for example) and
    // writes linear-light vec4<f32> RGBA into the grade chain's input
    // slot. Enables phase 4d's GPU-resident decode → grade path —
    // without this bridge the grade chain can only consume CPU data.
    /// The matrices and tone curves that shader reads, uploaded once.
    aces_rec709_data: Option<CachedTables>,
    /// BT.2020's reach tables, cusps and matrices — 148 KB, uploaded
    /// once.
    bt2020_gamut_data: Option<CachedTables>,
    hdr_encode_data: Option<CachedTables>,
    hdr_nit_buf: Option<wgpu::Buffer>,
    hdr_nit_staging: Option<wgpu::Buffer>,
    /// Map-readable copy of `bridge_u8_out_buf` for the display readback.
    display_u8_staging: Option<wgpu::Buffer>,
    /// The layer stack: one linear buffer per layer, two for the
    /// accumulator (ping-pong), an RGBA8 upload buffer for sprites.
    /// Sized by `stack_begin`, never shrunk.
    stack_layers: Vec<std::sync::Arc<wgpu::Buffer>>,
    stack_title_slot: usize,
    stack_acc: Option<std::sync::Arc<wgpu::Buffer>>,
    stack_sprite_u8: Option<wgpu::Buffer>,
    stack_sprite_f16: Option<wgpu::Buffer>,
    /// A countdown's or a timer's rects, `[x0, y0, x1, y1)` each (#2067).
    stack_rects: Option<wgpu::Buffer>,
    /// Title sprites kept on the GPU by the id their renderer gave
    /// them, so a title that only moves or fades from frame to frame is
    /// uploaded once and placed by a pass. Frame-sized RGBA8; dropped
    /// when the stack's size changes.
    stack_sprites: Vec<ResidentSprite>,
    stack_sprite_tick: u64,
    /// The glyph table of the frame being placed (grows as needed).
    sprite_glyph_table: Option<wgpu::Buffer>,
    stack_buf_size: u64,
    /// What its caches hold, in the GPU memory report (#3174).
    memory_gauges: MemoryGauges,
    /// Neighbour frames decoded to linear for a blend or a denoise, kept
    /// for reuse (#3174): at the chain's size each is a frame's worth.
    neighbour_pool: std::sync::Mutex<crate::nv12_gpu::ResidentPool>,
    /// Neighbour decodes submitted and not yet done on the GPU (#3174).
    neighbours_in_flight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// The memory pressure its caches last trimmed at (#3174).
    pressure_seen: u64,
    /// Whether anything has been blended into the accumulator yet.
    stack_acc_touched: bool,
    stack_dims: (u32, u32),
    // Bind group layouts
    // Reusable buffers — resized when frame dimensions change
    buf_a: Option<wgpu::Buffer>,
    buf_b: Option<wgpu::Buffer>,
    buf_c: Option<wgpu::Buffer>,  // third buffer for transition "to" frame
    /// The running premultiplied sum of a redrawn picture's shutter samples.
    shutter_sum: Option<wgpu::Buffer>,
    /// Dedicated buffer for GPU-resident mask matte. Written by
    /// `generate_mask_gpu_to_matte_buf`, read by
    /// `apply_mask_cutout_chain`. Independent of buf_a/buf_b/buf_c so a
    /// MaskCutout dispatch can run mid-chain without colliding with
    /// the ping-pong slots or with clarity / transitions.
    matte_buf: Option<wgpu::Buffer>,
    /// Storage buffer holding the per-mask `MaskRecord` array for
    /// the v2 mask pipeline. Sized lazily to the largest stack
    /// seen so far; small (48 B per mask) so we keep the slack.
    mask_records_buf: Option<wgpu::Buffer>,
    /// Pooled bezier control points for the v2 mask pipeline. All
    /// bezier shapes' points concatenate into this buffer; each
    /// MaskRecord carries `bezier_offset` + `bezier_count` to slice
    /// its own range. 8 B per point.
    mask_points_buf: Option<wgpu::Buffer>,
    staging_read: Option<wgpu::Buffer>,
    params_buf: wgpu::Buffer,
    /// Ring of uniform buffers used by chain dispatches. Every `*_chain`
    /// entry claims a fresh slot via `next_params_slot` so the params
    /// write in `queue.write_buffer` can't race against a later dispatch
    /// in the same submit — wgpu orders `write_buffer` calls against the
    /// NEXT submit, so naively batching N dispatches against the single
    /// `params_buf` would have all N read whichever write landed last.
    chain_params: ParamSlots,
    /// The uniform slots of the effects a chain records
    /// (`chain_run_effects`), apart from the chain's own. Taken out while
    /// one records (`record_effect` borrows the pipeline), so `None` only
    /// inside it.
    effect_params: Option<ParamSlots>,
    /// Set when the last chain's effects left one out: one
    /// that is only ever recorded, and could not be (a lost device). See
    /// [`Self::last_effects_complete`].
    effects_incomplete: std::cell::Cell<bool>,
    /// Frame-sized slots a run leases for the values a mixed effect needs
    /// (its input before it, the matte its mask reads): step 9c's first
    /// users of the pool (9b-1).
    slot_pool: SlotPool<wgpu::Buffer>,
    /// A test's switch: every `record_effect` answers `NotRecordable`.
    refuse_recording: bool,
    /// Pre-allocated bind groups for the 3-binding chain layout
    /// (`bind_group_layout`). Indexed by `slot * 4 + config_idx` where
    /// `config_idx` picks one of the four (input, output) buffer pairs
    /// the chain dispatch entries use: AtoB, BtoA, AtoC, BtoC.
    /// Size = `CHAIN_PARAMS_RING_SIZE * 4` = 128.
    ///
    /// Created at pipeline init with `buf_*` all empty so every entry is
    /// `None`; the first `ensure_buffers` call after a frame size change
    /// rebuilds it. We index-and-borrow instead of creating a fresh bind
    /// group per dispatch — on NVIDIA Vulkan wgpu's deferred
    /// descriptor-pool frees accumulate across long sessions and
    /// eventually produce `device lost (Dropped)`, plus ~0.3 ms per
    /// dispatch of driver overhead in the steady state.
    chain_bg_3binding: Vec<Option<wgpu::BindGroup>>,
    /// Pre-allocated bind groups for the huesat 4-binding layout with
    /// `huesat_lut_buf` as the extra buffer. Used by `apply_huesat_chain`.
    /// Size = `CHAIN_PARAMS_RING_SIZE * 2` = 64 (AtoB, BtoA only).
    chain_bg_huesat: Vec<Option<wgpu::BindGroup>>,
    /// Pre-allocated bind groups for the huesat layout with
    /// `rgb_curves_lut_buf` as binding 3. Used by `apply_rgb_curves_chain`.
    chain_bg_rgb_curves: Vec<Option<wgpu::BindGroup>>,
    /// Pre-allocated bind groups for the huesat layout with
    /// Pre-allocated bind groups for the huesat layout with `buf_c`
    /// as binding 3. Used by `apply_clarity_chain` pass 3.
    chain_bg_clarity_apply: Vec<Option<wgpu::BindGroup>>,
    /// Pre-allocated bind groups for the huesat layout with `matte_buf`
    /// as binding 3. Used by `apply_mask_cutout_chain` to multiply the
    /// chain frame by a GPU-resident matte without ever leaving the GPU.
    chain_bg_mask_cutout: Vec<Option<wgpu::BindGroup>>,
    /// Count of `create_bind_group` calls spent building the chain bind
    /// group pools. Used by `chain_reuses_bind_groups` to verify no new
    /// bind groups are created during steady-state dispatch.
    chain_bg_build_count: Arc<std::sync::atomic::AtomicU64>,
    /// Single `CommandEncoder` shared by an entire chain segment. All
    /// `*_chain` entries record into this when it's `Some`; the
    /// executor calls `begin_chain` / `submit_chain` around a chain
    /// segment so one `queue.submit` covers every dispatch in the
    /// segment. ~2-5 ms of NVIDIA Vulkan submit overhead per node
    /// otherwise.
    chain_encoder: Option<wgpu::CommandEncoder>,
    current_buf_size: u64,
    /// Set by wgpu callbacks when the device is lost or an uncaptured
    /// error fires. Every `apply_*` / `render_*` entry short-circuits
    /// to a safe passthrough so a flaky driver turns effects into a
    /// no-op instead of a process-wide panic. Reset only by tearing
    /// down and recreating the pipeline (i.e. app restart).
    device_lost: Arc<AtomicBool>,
    /// Count of queue submits issued since construction. Used by the
    /// `chain_single_submit_per_segment` test (in `klipp-nodes`) to
    /// verify the chain batching actually collapses per-node submits
    /// into one. Kept unconditional (rather than `#[cfg(test)]`) so
    /// downstream-crate tests can observe it without coupling to this
    /// crate's test harness — an atomic counter is ~free in release.
    submit_count: Arc<std::sync::atomic::AtomicU64>,
    /// Frames uploaded from the CPU (see [`Self::frame_upload_count`]).
    frame_upload_count: Arc<std::sync::atomic::AtomicU64>,
    /// A stack frame's passes, recorded into one encoder and handed to
    /// the queue once ([`Self::encoder`] / [`Self::submit`]); `batching`
    /// while a stack frame is open. `stack_params_ring` gives each pass
    /// its own params slot, since a `write_buffer` lands before the next
    /// submit whatever was recorded in between (the chain has its own
    /// ring for the same reason).
    batch: std::cell::RefCell<Option<wgpu::CommandEncoder>>,
    batching: std::cell::Cell<bool>,
    stack_params_ring: Vec<wgpu::Buffer>,
    stack_params_slot: std::cell::Cell<usize>,
    /// The waveform and parade scopes, made on first use: they share
    /// the device so a frame the chain graded can be measured in place.
    scopes: Option<crate::gpu_scopes::ScopeRenderer>,

    /// Working-space luma coefficients pushed into every grade-shader
    /// uniform (saturation, tone-controls keying, lift/gamma/gain
    /// banding). Default `(0.2126, 0.7152, 0.0722)` keeps the pre-
    /// Phase-E behaviour byte-identical for any caller that hasn't
    /// installed a working-space-aware setter (notably the existing
    /// integration tests). The engine flips this to AcesCG / etc. via
    /// [`Self::set_luma_coefs`] when the project's working colorspace
    /// changes.
    luma_coefs: [f32; 3],
    /// See [`Self::set_authored_to_working`].
    authored_to_working: [[f32; 4]; 3],
    /// The grade encoding the colour grade's controls act in
    /// (`klipp_grade_model::Encoding::as_u8`, #467): 0, BT.1886, unless
    /// the project grades in ACEScct or ACEScc under ACEScg.
    grade_encoding: u32,
}

/// Maximum params size (padded to 256 bytes for alignment)
const PARAMS_SIZE: u64 = 256;
/// Number of uniform-buffer slots in the chain params ring. A maximal
/// grade (~13 active nodes in the default graph) plus multi-pass nodes
/// like clarity (3 slots) fits comfortably. Bump if a future graph ever
/// asserts in `next_params_slot`.
/// What [`GpuEffectsPipeline::chain_run_effects`] did.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChainEffects {
    /// Whether the picture is in slot A now.
    pub in_a: bool,
    /// Effects that broke the chain: run in a pass of their own because
    /// they aren't recorded (yet).
    pub breaks: u32,
    /// Every effect was applied (`false` on a lost device).
    pub complete: bool,
}

const CHAIN_PARAMS_RING_SIZE: usize = 32;
/// Uniform slots of one recorded run of effects: one per pass, so a run
/// longer than this is submitted in parts.
const EFFECT_PARAMS_RING_SIZE: usize = 64;
/// Params slots a stack frame may use before its passes so far are
/// handed to the queue and the ring starts over.
const STACK_PARAMS_RING_SIZE: usize = 64;
const WORKGROUP_SIZE: u32 = 256;

/// The most workgroups a dispatch may take along one dimension: wgpu's
/// default limit, which every adapter offers.
const MAX_WORKGROUPS_PER_DIMENSION: u32 = 65_535;

thread_local! {
    /// [`MAX_WORKGROUPS_PER_DIMENSION`], lowered on one thread only by
    /// [`WorkgroupCapForTest`].
    static WORKGROUP_CAP: std::cell::Cell<u32> = const { std::cell::Cell::new(MAX_WORKGROUPS_PER_DIMENSION) };
}

/// `n` workgroups over x and, past the per-dimension limit, y (#2341): an
/// 8K frame is 129,600 workgroups of 256 pixels. Every flat-indexed
/// shader rebuilds its index from both, `gid.x + gid.y · nwg.x · size`,
/// and its bounds check takes the few past the end.
pub fn split_workgroups(n: u32) -> (u32, u32) {
    let cap = WORKGROUP_CAP.with(std::cell::Cell::get).max(1);
    if n <= cap {
        return (n, 1);
    }
    let y = n.div_ceil(cap);
    (n.div_ceil(y), y)
}

/// Dispatch `n` workgroups of a flat-indexed shader, split by
/// [`split_workgroups`].
pub fn dispatch_flat(pass: &mut wgpu::ComputePass<'_>, n: u32) {
    let (x, y) = split_workgroups(n);
    pass.dispatch_workgroups(x, y, 1);
}

/// The per-dimension limit lowered to `cap` on this thread while held, so
/// a test sees small frames dispatched in 2D; other threads keep the real
/// one, and it is restored on drop.
#[doc(hidden)]
pub struct WorkgroupCapForTest(u32);

impl WorkgroupCapForTest {
    pub fn new(cap: u32) -> Self {
        Self(WORKGROUP_CAP.with(|c| c.replace(cap)))
    }
}

impl Drop for WorkgroupCapForTest {
    fn drop(&mut self) {
        WORKGROUP_CAP.with(|c| c.set(self.0));
    }
}

/// Maximum time to spin waiting for a `map_async` callback before
/// giving up. 2 s is far longer than any real GPU operation needs,
/// but short enough that a genuine hang gets flagged before the
/// engine watchdog notices.
pub const MAP_POLL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Fine-grained "inside which GPU call" marker, parallel to the
/// engine's `ENGINE_STAGE` waypoint. The grade-path stall we chase
/// always shows up as `ENGINE_STAGE=Prerender` (or PausedRegrade,
/// PostProcessInner), which tells us which engine *phase* is stuck
/// but not which underlying GPU operation inside that phase. This
/// atomic narrows it to the specific blocking call — map_async
/// wait, buffer drain, etc.
///
/// Written with `Release`, read by the watchdog with `Acquire`.
pub static GPU_STAGE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[repr(u64)]
#[derive(Copy, Clone, Debug)]
pub enum GpuStage {
    Idle = 0,
    /// `bridge_u8_to_linear_into_chain` — u8 sRGB → linear f32
    /// compute shader + GPU→CPU readback of the linear buffer.
    /// The readback is the usual hang site.
    BridgeU8 = 1,
    /// `flush_chain_to_cpu` — drains the current grade-chain ping-
    /// pong buffer back to CPU linear RGBA. Blocking readback.
    FlushChain = 2,
    /// `linear_rgba_to_u8_gpu` — linear f32 → sRGB u8 compute +
    /// blocking readback. Called at the end of post_process_rgba
    /// for cache writes and scope input.
    LinearRgbaToU8 = 3,
    // Discriminants 4–8 were the stage labels of a scope renderer since
    // removed; the GPU scopes are timed by the engine as one pass. The
    // numeric gap is intentional so any persisted instrumentation logs
    // still resolve.
    /// `Frame::into_cpu` in klipp-engine — reads back a
    /// decode-thread-produced GpuFrame to CPU.
    FrameIntoCpu = 9,
    /// `P010Converter::convert_to_rgba` — HDR YUV decode path
    /// readback.
    P010Convert = 10,
    /// `Nv12Converter::convert_to_rgba` — SDR YUV decode path
    /// readback.
    Nv12Convert = 11,
}

pub fn gpu_stage_name(v: u64) -> &'static str {
    match v {
        0 => "Idle",
        1 => "BridgeU8",
        2 => "FlushChain",
        3 => "LinearRgbaToU8",
        9 => "FrameIntoCpu",
        10 => "P010Convert",
        11 => "Nv12Convert",
        _ => "Unknown",
    }
}

#[inline]
pub fn mark_gpu_stage(s: GpuStage) {
    GPU_STAGE.store(s as u64, std::sync::atomic::Ordering::Release);
}

/// Poll the device until a `map_async` callback has delivered a
/// result on the provided channel, or the 2-second timeout expires.
///
/// Replaces the classic `submit → map_async → device.poll(Wait) →
/// rx.recv()` pattern. On a UI-shared device `Maintain::Wait` never
/// returns (the UI's egui paint + present submits keep the queue
/// non-empty), and even `WaitForSubmissionIndex` doesn't reliably
/// drive the mapping callback past the submission's completion on
/// the wgpu-22 Vulkan backend. Polling with `Maintain::Poll`
/// explicitly processes ready callbacks without blocking, so the
/// engine thread can't get starved by UI traffic.
///
/// Returns the channel's result on success, or
/// `Err(RecvError)`-style substitute when the timeout expires so
/// callers can treat it as a device-lost-equivalent and fall back.
pub fn poll_until_mapped(
    device: &wgpu::Device,
    rx: &std::sync::mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
) -> Result<Result<(), wgpu::BufferAsyncError>, std::sync::mpsc::RecvTimeoutError> {
    let start = std::time::Instant::now();
    loop {
        // Non-blocking poll. On wgpu-22, `Maintain::Poll` drives
        // completed submissions' map callbacks — critical because
        // without it the callback never fires even though the GPU
        // work is done.
        device.poll(wgpu::Maintain::Poll);
        match rx.try_recv() {
            Ok(v) => return Ok(v),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(std::sync::mpsc::RecvTimeoutError::Disconnected);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if start.elapsed() >= MAP_POLL_TIMEOUT {
                    return Err(std::sync::mpsc::RecvTimeoutError::Timeout);
                }
                // Yield the thread briefly so we're not 100%-CPU
                // while waiting. 100 µs is fine-grained enough to
                // catch sub-millisecond readbacks yet cheap enough
                // not to pile up syscalls.
                std::thread::sleep(std::time::Duration::from_micros(100));
            }
        }
    }
}


/// Where a mixed effect's matte comes from (#1242): pixels made on the
/// CPU, uploaded into its pool slot; or a window's shapes, worked out per
/// pixel inside the mix on the GPU, with no matte made anywhere (#1252).
#[derive(Clone, Copy)]
pub enum EffectMatte<'a> {
    Pixels(&'a [f32]),
    Shapes(&'a [GpuMaskDef]),
    /// A key already drawn on the GPU into its own slot
    /// ([`GpuEffectsPipeline::key_into_matte_slot`], #1266): read as it is,
    /// and left leased for the key's other readers.
    Keyed(&'a KeyedMatte),
}

/// A key whose matte is drawn on the GPU for effects' masks (#1266), with
/// the settings its kernel takes. Hue values are degrees, as the caller
/// sees them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GpuKey {
    /// `rgb`, in RGB mode (#895): red, green and blue low and high, and
    /// the soft edge; `None` keys by hue, saturation and luminance.
    /// `volume`, in 3D mode: the keep and remove volumes and the reach.
    Qualifier { hue_center: f32, hue_width: f32, hue_soft: f32, sat_low: f32, sat_high: f32, sat_soft: f32, lum_low: f32, lum_high: f32, lum_soft: f32, invert: bool, rgb: Option<[f32; 7]>, volume: Option<QualifierVolume> },
    LumaKey { low: f32, high: f32, soft: f32, invert: bool },
    /// Against the plate [`GpuEffectsPipeline::upload_difference_key_plate`]
    /// put on the GPU.
    DifferenceKey { tolerance: f32, soft: f32, invert: bool, luminance: bool },
}

/// The 3D qualifier's key (#895): the keep and remove volumes, and how
/// far (and how softly) the keep volume reaches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QualifierVolume {
    pub keep: Option<crate::cpu_grade::KeyVolume>,
    pub remove: Option<crate::cpu_grade::KeyVolume>,
    pub radius: f32,
    pub soft: f32,
}

/// A key's matte in a pool slot of its own (#1266), drawn from a chain
/// picture by [`GpuEffectsPipeline::key_into_matte_slot`]. Each mixed
/// effect it masks reads it ([`EffectMatte::Keyed`]); the caller gives it
/// back with [`GpuEffectsPipeline::release_keyed_matte`] once the last one
/// is recorded (the run's end frees it too).
#[derive(Debug, PartialEq, Eq)]
pub struct KeyedMatte(slots::SlotId);

/// A picture the chain keeps beside its pair, in a pool slot of its own
/// (M3 step 17b): a grade secondary's ungraded picture, which its key
/// reads and its gate blends against, or a composite's second picture.
/// Kept by [`GpuEffectsPipeline::keep_picture`] or uploaded by
/// [`GpuEffectsPipeline::upload_picture`]; the caller gives it back with
/// [`GpuEffectsPipeline::release_picture`] once its last reader is
/// recorded (the run's end frees it too).
#[derive(Debug, PartialEq, Eq)]
pub struct KeptPicture(slots::SlotId);

/// Where a key reads its picture and writes its matte.
#[derive(Clone, Copy)]
enum KeyRoute {
    /// A kept picture, into a slot: a grade secondary's.
    KeptToSlot(slots::SlotId, slots::SlotId),
    /// The chain's picture in `buf_a` (`true`) or `buf_b`, into a slot.
    ChainToSlot(bool, slots::SlotId),
}

/// The pipeline's shares of the GPU memory report (#3174), one a cache.
#[derive(Debug)]
struct MemoryGauges {
    chain: crate::gpu_memory::Gauge,
    slots: crate::gpu_memory::Gauge,
    stack: crate::gpu_memory::Gauge,
    sprites: crate::gpu_memory::Gauge,
}

impl Default for MemoryGauges {
    fn default() -> Self {
        use crate::gpu_memory::{Gauge, Held};
        Self { chain: Gauge::new(Held::ChainBuffers), slots: Gauge::new(Held::Slots), stack: Gauge::new(Held::StackLayers), sprites: Gauge::new(Held::Sprites) }
    }
}

/// A pool slot's buffer, of the frame's size.
fn slot_buffer(device: &wgpu::Device, k: SlotKey) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(match k.usage { SlotUsage::Image => "slot_image", SlotUsage::Matte => "slot_matte" }),
        size: u64::from(k.width) * u64::from(k.height) * 16,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
}

impl GpuEffectsPipeline {
    /// Create a new GPU effects pipeline, sharing the process-wide
    /// `GpuContext` with the NV12/P010 converters. Returns `None` if
    /// no suitable GPU is found.
    ///
    /// The `GPU_POWER_PREF` override is honoured via
    /// `GpuContext::shared`'s policy (HighPerformance by default) —
    /// the unification means every GPU-using type gets the same
    /// adapter, which is the prerequisite for cross-type buffer
    /// handoff in phase 4's grade chain.
    pub fn new() -> Option<Self> {
        let ctx = crate::gpu_context::GpuContext::shared()?;
        Self::with_context(ctx)
    }

    /// Create a new GPU effects pipeline that shares an existing
    /// `GpuContext`. Enables device-local handoff from the decoder
    /// converters into the grade chain without a CPU round-trip.
    pub fn with_context(ctx: &crate::gpu_context::GpuContext) -> Option<Self> {
        // The context owns the device's error/loss callbacks and the
        // flag they flip (one callback slot per device in wgpu —
        // registering per-pipeline raced under concurrency, see
        // `gpu_context::register_device_callbacks`). Reset it here so
        // a pipeline rebuilt after a transient error gets a fresh
        // start; a genuinely dead device re-flags on the next error.
        let device_lost = ctx.device_lost_flag();
        device_lost.store(false, Ordering::SeqCst);
        Self::build_from_device_queue(ctx.device(), ctx.queue(), device_lost)
    }

    /// Set the working-space luma coefficients used by subsequent
    /// grade-shader dispatches. Called by the engine when the loaded
    /// project's working colorspace changes; the values are the
    /// second row of the working space's RGB → CIE XYZ matrix
    /// (`(0.2126, 0.7152, 0.0722)` for Rec.709,
    /// `(0.2722, 0.6741, 0.0537)` for AcesCG, etc.). Default is
    /// Rec.709 so any caller that doesn't install one keeps the
    /// pre-Phase-E behaviour byte-identical.
    #[inline]
    pub fn set_luma_coefs(&mut self, coefs: [f32; 3]) {
        self.luma_coefs = coefs;
    }

    /// The BT.709 → working-space matrix a placed title, glyph run or
    /// generator layer is converted by, as the CPU's
    /// `authored_to_working` converts it (#1279); identity, the default,
    /// in a Rec.709 project.
    pub fn set_authored_to_working(&mut self, m: [[f32; 3]; 3]) {
        self.authored_to_working = m.map(|r| [r[0], r[1], r[2], 0.0]);
    }

    /// The encoding the colour grade's controls act in (#467), as
    /// `klipp_grade_model::Encoding::as_u8` gives it.
    pub fn set_grade_encoding(&mut self, tag: u8) {
        self.grade_encoding = u32::from(tag);
    }

    /// Read the active luma coefficients. Mostly useful for tests.
    #[inline]
    pub fn luma_coefs(&self) -> [f32; 3] {
        self.luma_coefs
    }

    /// True once wgpu has reported a device-lost or uncaptured error.
    /// After this flips, every frame-processing entry passes the input
    /// through unchanged so compositing doesn't stall on a dead GPU.
    pub fn is_device_lost(&self) -> bool {
        self.device_lost.load(Ordering::SeqCst)
    }

    /// Mark the pipeline dead. Intended for internal call sites that
    /// observe a sync failure (e.g. `download()` when `map_async`
    /// returns Err) that wgpu's async callbacks may not have flagged
    /// yet.
    fn mark_device_lost(&self, ctx: &str) {
        if !self.device_lost.swap(true, Ordering::SeqCst) {
            error!("GPU effects: {ctx} — disabling GPU effects");
        }
    }

    /// Ensure GPU buffers are large enough for the frame.
    fn ensure_buffers(&mut self, byte_size: u64) {
        if byte_size <= self.current_buf_size {
            return;
        }
        self.flush();
        debug!("GPU effects: resizing buffers to {byte_size} bytes");
        self.buf_a = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_a"),
            size: byte_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.buf_b = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_b"),
            size: byte_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.buf_c = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_c"),
            size: byte_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.matte_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_matte"),
            size: byte_size,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }));
        self.staging_read = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("fx_staging_read"),
            size: byte_size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
        self.current_buf_size = byte_size;
        // The pre-allocated chain bind group pools reference buf_a /
        // buf_b / buf_c by Buffer handle; they're now stale. Rebuild.
        self.rebuild_chain_bind_group_pools();
    }

    /// Populate the pre-allocated chain bind-group pools against the
    /// current `buf_a` / `buf_b` / `buf_c` handles. Called after any
    /// buffer recreation (`ensure_buffers`, clarity's on-demand `buf_c`
    /// create). Every slot × every buffer configuration is built once
    /// here; dispatch sites then index into the pool instead of issuing
    /// `create_bind_group` per frame.
    ///
    /// Pool counts:
    ///  - `chain_bg_3binding`: 32 slots × 4 configs = 128
    ///  - each 4-binding pool: 32 slots × 2 configs = 64
    ///
    /// Configuration indexing (3-binding): 0=AtoB, 1=BtoA, 2=AtoC, 3=BtoC.
    /// Configuration indexing (4-binding): 0=AtoB, 1=BtoA.
    fn rebuild_chain_bind_group_pools(&mut self) {
        let (buf_a, buf_b, buf_c, matte_buf) = match (
            self.buf_a.as_ref(),
            self.buf_b.as_ref(),
            self.buf_c.as_ref(),
            self.matte_buf.as_ref(),
        ) {
            (Some(a), Some(b), Some(c), Some(m)) => (a, b, c, m),
            _ => {
                // Buffers not yet allocated — pools stay empty and
                // `ensure_buffers` will call us again after it creates
                // buf_a/b/c/matte_buf.
                return;
            }
        };

        let configs_3: [(&wgpu::Buffer, &wgpu::Buffer); 4] = [
            (buf_a, buf_b), // AtoB = 0
            (buf_b, buf_a), // BtoA = 1
            (buf_a, buf_c), // AtoC = 2
            (buf_b, buf_c), // BtoC = 3
        ];
        let configs_4: [(&wgpu::Buffer, &wgpu::Buffer); 2] = [
            (buf_a, buf_b), // AtoB = 0
            (buf_b, buf_a), // BtoA = 1
        ];

        for slot in 0..CHAIN_PARAMS_RING_SIZE {
            for (cfg_idx, (input, output)) in configs_3.iter().enumerate() {
                let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("chain_bg_3b"),
                    layout: &self.pipelines.bind_group_layout,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: self.chain_params.buffer(slot).as_entire_binding(),
                        },
                    ],
                });
                self.chain_bg_build_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.chain_bg_3binding[slot * 4 + cfg_idx] = Some(bg);
            }

            for (cfg_idx, (input, output)) in configs_4.iter().enumerate() {
                let idx = slot * 2 + cfg_idx;

                let bg_huesat = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("chain_bg_huesat"),
                    layout: &self.pipelines.huesat_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: self.huesat_lut_buf.as_entire_binding() },
                    ],
                });
                self.chain_bg_build_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.chain_bg_huesat[idx] = Some(bg_huesat);

                let bg_rgb_curves = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("chain_bg_rgb_curves"),
                    layout: &self.pipelines.huesat_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: self.rgb_curves_lut_buf.as_entire_binding() },
                    ],
                });
                self.chain_bg_build_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.chain_bg_rgb_curves[idx] = Some(bg_rgb_curves);

                let bg_clarity_apply = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("chain_bg_clarity_apply"),
                    layout: &self.pipelines.huesat_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: buf_c.as_entire_binding() },
                    ],
                });
                self.chain_bg_build_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.chain_bg_clarity_apply[idx] = Some(bg_clarity_apply);

                let bg_mask_cutout = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("chain_bg_mask_cutout"),
                    layout: &self.pipelines.huesat_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: matte_buf.as_entire_binding() },
                    ],
                });
                self.chain_bg_build_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.chain_bg_mask_cutout[idx] = Some(bg_mask_cutout);
            }
        }
    }

    /// Create a bind group with the given input/output buffers.
    fn bind_group(&self, input: &wgpu::Buffer, output: &wgpu::Buffer) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.pipelines.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: input.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: output.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params_binding(),
                },
            ],
        })
    }

    /// Clone the Arc-wrapped wgpu device. Pair with
    /// `FrameBuffer::ensure_gpu_output` when routing the final
    /// graded frame into a viewer-bindable texture on the shared
    /// device.
    /// The scope renderer on this device, made on first use.
    pub fn scopes(&mut self) -> &mut crate::gpu_scopes::ScopeRenderer {
        if self.scopes.is_none() {
            self.scopes = Some(crate::gpu_scopes::ScopeRenderer::new(self.device.clone(), self.queue.clone()));
        }
        self.scopes.as_mut().expect("just made")
    }

    /// The scopes measured from the frame the last display encode
    /// ([`Self::encode_chain_to_display`]) left on the device, `width ×
    /// height` packed RGBA8, without reading it back. `None` when there
    /// is no such frame; call it before anything else encodes one.
    pub fn render_scopes_from_display(
        &mut self,
        width: u32,
        height: u32,
        luma: [f32; 3],
        step: u32,
        jobs: &[crate::gpu_scopes::ScopeJob],
    ) -> Option<Vec<Option<Vec<u8>>>> {
        if self.is_device_lost() {
            return None;
        }
        if self.scopes.is_none() {
            self.scopes = Some(crate::gpu_scopes::ScopeRenderer::new(self.device.clone(), self.queue.clone()));
        }
        let buffer = self.bridge_u8_out_buf.as_ref()?;
        let scopes = self.scopes.as_mut().expect("just made");
        let input = crate::gpu_scopes::ScopeInput::DisplayOnGpu { buffer, width, height, luma };
        Some(scopes.render(&input, step, jobs))
    }

    pub fn device(&self) -> std::sync::Arc<wgpu::Device> {
        self.flush();
        std::sync::Arc::clone(&self.device)
    }

    /// Clone the Arc-wrapped queue, for writing into buffers made on
    /// [`Self::device`].
    pub fn queue(&self) -> std::sync::Arc<wgpu::Queue> {
        self.flush();
        std::sync::Arc::clone(&self.queue)
    }


    // ═══════════════════════════════════════════════════════════════════════════
    // Dispatch-only entry points — GPU-resident chain path
    //
    // Each `*_chain` method performs the same compute-shader dispatch as its
    // `apply_*_gpu` counterpart, but reads from one of `buf_a`/`buf_b`
    // (selected by `in_is_a`) and writes to the opposite slot. The result
    // stays on the GPU. Callers (see `GraphExecutor::execute_linear`) plumb
    // the ping-pong parity through a whole run of single-input filter nodes
    // so only the head of the chain uploads and only the tail downloads.
    //
    // `in_is_a` is preferred over raw `&wgpu::Buffer` references because
    // `wgpu::Buffer` is `!Clone` and we need to re-borrow `&mut self` across
    // dispatches (for params writes, bind group creation, LUT uploads, …).
    // ═══════════════════════════════════════════════════════════════════════════

    // ─── Chain-encoder lifecycle ──────────────────────────────────────────
    //
    // Every `*_chain` entry records into a single shared `CommandEncoder`
    // so all dispatches in a chain segment flush in ONE `queue.submit`.
    // NVIDIA Vulkan spends ~2-5 ms per submit on fence setup and barrier
    // tracking; for a 5-node chain that's 10-20 ms that disappears with
    // this batching.
    //
    // The wrinkle that forces the params ring: wgpu orders
    // `queue.write_buffer` writes before the NEXT submit. If we recorded
    // `write_buffer(params_A); pass_A; write_buffer(params_B); pass_B`
    // into one encoder and submitted once, both passes would observe
    // `params_B`. Each `*_chain` therefore claims its own slot from
    // `chain_params` via `next_params_slot`, and the pass binds THAT slot
    // rather than the shared `params_buf`.

    /// Blocking downloads by this pipeline; see [`readback_stats`] for scope.
    pub fn readback_stats(&self) -> ReadbackStats { self.readbacks.snapshot() }

    /// Count of `queue.submit` calls since pipeline creation. Used by
    /// `chain_single_submit_per_segment` (klipp-nodes tests) to verify
    /// the chain path collapses per-node submits into one.
    pub fn submit_count(&self) -> u64 {
        self.submit_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Count of whole frames uploaded from the CPU since pipeline
    /// creation: a chain's input, a single-node pass's input, a key's
    /// plate. Its change over a graph run is
    /// `ChainStats::uploads`, so a test can tell a CPU-made value that was
    /// uploaded into the chain from one that made the chain decline.
    pub fn frame_upload_count(&self) -> u64 {
        self.frame_upload_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Count of chain-bind-group `create_bind_group` calls since
    /// pipeline creation. Every bind group is built once by
    /// `rebuild_chain_bind_group_pools` (at init time and whenever
    /// buffers are resized); steady-state dispatch never builds more.
    /// Used by `chain_reuses_bind_groups` to verify the pool path.
    pub fn chain_bind_group_build_count(&self) -> u64 {
        self.chain_bg_build_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Error message for inline `chain_encoder.as_mut().expect(...)` calls
    /// at each `*_chain` dispatch site. We inline rather than wrap in a
    /// method so callers keep the disjoint-field borrow (pipeline refs
    /// live alongside the encoder on `self`).
    const CHAIN_ENCODER_EXPECT: &'static str =
        "chain encoder not open — begin_chain must be called before *_chain dispatches";

    /// Error for pool-lookup panics: the pool is rebuilt from
    /// `ensure_buffers` whenever buffers are (re)created, so every
    /// dispatch site should find a populated slot. If this ever fires
    /// someone skipped the `ensure_chain_buffers` call ahead of the
    /// chain, or `buf_c` wasn't created when clarity was the first
    /// chain node.
    const CHAIN_BG_EXPECT: &'static str =
        "chain bind group pool not populated — ensure_chain_buffers must be called before *_chain dispatches";


    // ─── Chain plumbing helpers ────────────────────────────────────────────

    // ─── The layer stack ─────────────────────────────────────────────
    //
    // A frame's layers composited on the GPU, in the working linear
    // space, the way `klipp_compose::Renderer::render_stack` does it on
    // the CPU: each layer lands in its own buffer (from a chain output,
    // a linear upload or an sRGB sprite), may have effects run on it in
    // place, a matte multiplied into its alpha, and is blended onto the
    // accumulator in place with its opacity and blend mode; a transition
    // is a two-input pass between two layers; an adjustment layer runs
    // its chain on a copy of the accumulator and mixes back; the
    // accumulator is premultiplied and encoded at the end. The CPU stack
    // is the twin; `gpu_stack_parity.rs` and `gpu_composite.rs` hold it.

    /// The encoder a pass records into: while a stack frame is open, the
    /// frame's one encoder (taken out here, given back by
    /// [`Self::submit`]); otherwise a new one for this pass alone.
    fn encoder(&self, label: Option<&str>) -> wgpu::CommandEncoder {
        if self.batching.get() {
            if let Some(enc) = self.batch.borrow_mut().take() {
                return enc;
            }
        }
        self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label })
    }

    /// A pass is recorded: hand it to the queue, or while a stack frame
    /// is open keep it as the frame's encoder for the next pass to
    /// continue. An encoder already waiting there was recorded while
    /// `enc` was open (a pass inside another's recording) and goes to
    /// the queue first, the order every pass had when each submitted
    /// itself.
    fn submit(&self, enc: wgpu::CommandEncoder) {
        if !self.batching.get() {
            self.submit_now(enc);
            return;
        }
        if let Some(prev) = self.batch.borrow_mut().take() {
            self.submit_now(prev);
        }
        *self.batch.borrow_mut() = Some(enc);
    }

    /// Every command buffer this pipeline hands the queue goes through
    /// here, so the count in [`Self::submit_count`] is every submit.
    fn submit_now(&self, enc: wgpu::CommandEncoder) {
        self.queue.submit(std::iter::once(enc.finish()));
        self.submit_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Wait until the GPU has done all the work handed to it so far, at
    /// most two seconds, by polling (a UI-shared device's `Wait` never
    /// returns): how long that took, in ms. A diagnostic for where a
    /// chain start's time goes (#2107), never on a normal frame's path.
    #[doc(hidden)]
    pub fn wait_for_submitted_work(&self) -> f64 {
        let t = std::time::Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        self.queue.on_submitted_work_done(move || {
            let _ = tx.send(());
        });
        while t.elapsed() < std::time::Duration::from_secs(2) {
            self.device.poll(wgpu::Maintain::Poll);
            if rx.try_recv().is_ok() {
                break;
            }
            std::thread::yield_now();
        }
        t.elapsed().as_secs_f64() * 1000.0
    }

    /// Hand the queue what a stack frame has recorded so far. Before
    /// anything that must see those passes done or ordered: an upload
    /// into a buffer they read, a readback, a chain segment, a buffer
    /// handed to code outside this pipeline.
    pub fn flush(&self) {
        if let Some(enc) = self.batch.borrow_mut().take() {
            self.submit_now(enc);
        }
    }

    /// `queue.write_buffer` into a buffer passes may read: the passes
    /// recorded so far go first, since the write lands before the next
    /// submit whatever was recorded ahead of it.
    fn upload(&self, buffer: &wgpu::Buffer, offset: u64, data: &[u8]) {
        self.flush();
        self.queue.write_buffer(buffer, offset, data);
    }

    /// [`Self::upload`] of a whole frame of pixels from the CPU, counted
    /// in [`Self::frame_upload_count`]; tables and parameters are not.
    fn upload_frame(&self, buffer: &wgpu::Buffer, data: &[u8]) {
        self.upload(buffer, 0, data);
        self.frame_upload_count.fetch_add(1, Ordering::Relaxed);
    }

    /// The uniform the next pass binds as its params: the slot
    /// [`Self::write_params`] last filled while a stack frame is open,
    /// else the one shared params buffer.
    fn params_binding(&self) -> wgpu::BindingResource<'_> {
        if self.batching.get() {
            if let Some(i) = self.stack_params_slot.get().checked_sub(1) {
                return self.stack_params_ring[i].as_entire_binding();
            }
        }
        self.params_buf.as_entire_binding()
    }

    /// Slots the pool may hold at once, of all sizes (9b-1's budget).
    pub const MAX_SLOTS: usize = 8;

    fn write_params<T: bytemuck::Pod>(&self, data: &T) {
        let bytes = bytemuck::bytes_of(data);
        if !self.batching.get() {
            self.queue.write_buffer(&self.params_buf, 0, bytes);
            return;
        }
        let mut slot = self.stack_params_slot.get();
        if slot >= STACK_PARAMS_RING_SIZE {
            self.flush();
            slot = 0;
        }
        self.queue.write_buffer(&self.stack_params_ring[slot], 0, bytes);
        self.stack_params_slot.set(slot + 1);
    }

    fn dispatch(
        &self,
        pipeline: &wgpu::ComputePipeline,
        input: &wgpu::Buffer,
        output: &wgpu::Buffer,
        workgroups: u32,
    ) {
        let bind_group = self.bind_group(input, output);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(encoder);
    }

    /// Download `byte_size` raw bytes from a storage buffer. Kept for
    /// buffers that legitimately store bytes (for example the u32
    /// scope image buffer) — pixel buffers now hold linear-f32 vec4s
    /// and callers should use [`download_linear_rgba_as_u8`] or
    /// [`download_linear_rgba`] instead.
    /// Map `source` and hand the bytes to `read` once, then unmap.
    /// `None` when the device is lost or the map failed.
    ///
    /// The mapped range cannot outlive the unmap, so a caller has to copy
    /// what it wants out of it — and this exists so that is the *only*
    /// copy. Reading a frame as `Vec<u8>` and re-interpreting it into a
    /// `Vec<f32>` afterwards costs two, and at 4K each one is a 133 MB
    /// memcpy: together they were the larger half of a 45.7 ms chain
    /// readback in the export.
    fn with_downloaded<T>(
        &self,
        source: &wgpu::Buffer,
        byte_size: u64,
        read: impl FnOnce(&[u8]) -> T,
    ) -> Option<T> {
        if self.is_device_lost() {
            return None;
        }
        let staging = self.staging_read.as_ref().unwrap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let mut readback = self.readbacks.begin(byte_size);
        encoder.copy_buffer_to_buffer(source, 0, staging, 0, byte_size);
        self.submit(encoder);

        let buffer_slice = staging.slice(..byte_size);
        let (tx, rx) = std::sync::mpsc::channel();
        self.flush();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });

        mark_gpu_stage(GpuStage::FlushChain);
        let result = match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let data = buffer_slice.get_mapped_range();
                let result = read(&data);
                drop(data);
                staging.unmap();
                readback.succeeded = true;
                Some(result)
            }
            other => {
                // `map_async` signalled an error (or the channel dropped
                // without delivering one). That means the GPU state is
                // suspect: either the buffer wasn't mapped or the device
                // is on its way out. Flip the lost flag so subsequent
                // calls short-circuit before they touch the device.
                self.mark_device_lost(&format!("buffer map_async failed: {other:?}"));
                None
            }
        };
        mark_gpu_stage(GpuStage::Idle);
        result
    }

    fn download(&self, source: &wgpu::Buffer, byte_size: u64) -> Vec<u8> {
        self.with_downloaded(source, byte_size, |bytes| bytes.to_vec())
            .unwrap_or_else(|| vec![0u8; byte_size as usize])
    }

    /// Download `pixel_count` linear-f32 RGBA vec4s from a pixel
    /// storage buffer as-is. This is the native entry point for the
    /// linear-edge-to-edge pipeline — callers that already speak
    /// linear (the engine compositor) skip the sRGB u8 boundary
    /// entirely.
    fn download_linear_rgba_f32(
        &self,
        source: &wgpu::Buffer,
        pixel_count: usize,
    ) -> Vec<f32> {
        let byte_size = (pixel_count * 16) as u64;
        // Straight from the mapped range into floats — one copy, in
        // parallel, into uninitialised capacity. Going via `download`'s
        // `Vec<u8>` and casting afterwards costs a second full copy of the
        // frame, which at 4K is 133 MB of pure memory traffic.
        self.with_downloaded(source, byte_size, |bytes| {
            mapped_bytes_to_f32(bytes, pixel_count * 4)
        })
        .filter(|linear| linear.len() >= pixel_count * 4)
        // Device-lost fallback: the map returned nothing usable.
        .unwrap_or_else(|| vec![0.0f32; pixel_count * 4])
    }

}

// ─── Param structs (must be Pod for bytemuck) ────────────────────────────────

/// A bitmap glyph's pose on the title stack (#2203): its alpha, and the
/// frame rows it may cover.
#[derive(Clone, Copy, Debug)]
pub struct GlyphPlace {
    pub alpha: f32,
    pub clip_y: [i32; 2],
}

impl Default for GlyphPlace {
    fn default() -> Self {
        Self { alpha: 1.0, clip_y: [i32::MIN, i32::MAX] }
    }
}

/// How a placed sprite fades with where it is on the frame (#592), as
/// the title crate's `ScreenFade` gives it: an edge fade zone at each
/// edge, and a focus band down (`Some(true)`) or across (`Some(false)`)
/// the frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SpriteShape {
    pub top: f32,
    pub bottom: f32,
    pub left: f32,
    pub right: f32,
    pub focus_vertical: Option<bool>,
    pub band: f32,
    pub edge_opacity: f32,
}

/// The box of the frame a title sprite's half floats cover (#190):
/// `w * h` pixels from `(x0, y0)`, two `u32` each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SpriteBox {
    pub x0: u32,
    pub y0: u32,
    pub w: u32,
    pub h: u32,
}

impl SpriteBox {
    /// Whether `texels` are this box's and the box lies in the frame.
    pub fn fits(&self, texels: &[u32], width: u32, height: u32) -> bool {
        texels.len() as u64 == (self.w as u64) * (self.h as u64) * 2 && self.x0 + self.w <= width && self.y0 + self.h <= height
    }
}


/// Where a `w` by `h` picture sits when fitted into `ow` by `oh`: its
/// size there and its offset, in whole output pixels. One factor, so
/// the shape is kept; centred. The CPU compositor's arithmetic, which
/// the GPU fit has to land on to the pixel.
pub fn fit_rect(w: u32, h: u32, ow: u32, oh: u32) -> (u32, u32, u32, u32) {
    let scale = (ow as f32 / w as f32).min(oh as f32 / h as f32);
    let dw = ((w as f32 * scale).round() as u32).clamp(1, ow);
    let dh = ((h as f32 * scale).round() as u32).clamp(1, oh);
    (dw, dh, (ow - dw) / 2, (oh - dh) / 2)
}

/// How a frame has to be laid out for the encoder that will take it.
///
/// One value rather than five arguments: it travels from the encoder
/// that knows the answer, through the renderer, to the shader, and a
/// five-wide tuple through three layers is five chances to transpose
/// two of them — `semi_planar` and the depth especially, since both are
/// small integers that look alike at a call site.
#[derive(Clone, Copy, Debug)]
pub struct Yuv420Target {
    /// 0 none, 1 Reinhard, 2 Hable. The host has already established
    /// the display transform is a tone map and an sRGB encode.
    pub tone_map_op: u32,
    /// Which output transform the blob describes, when `tone_map_op` is
    /// 3 — `klipp_color::aces::GpuOdt`'s wire tag. This layer does not
    /// interpret it: it names the blob the caller must supply, and 0 (the
    /// ACES Rec.709 SDR transform) is what an AcesCG project's viewer
    /// match gives.
    pub odt_tag: u32,
    /// 0 an SDR delivery, 1 PQ, 2 HLG. An HDR delivery replaces the tone
    /// map and the sRGB encode entirely: the frame is display light
    /// (1.0 = 203 cd/m²), and the shader takes it to BT.2020 and the
    /// delivery's transfer. Exclusive with the two fields above, which
    /// read the same binding.
    pub hdr_op: u32,
    /// The master's peak, cd/m². PQ only — HLG's is the nominal 1000.
    pub peak_nits: f32,
    /// A scene-referred source's own last step, which runs *before* the
    /// delivery's transform: 0 none, 1 the SDR display tone map, 2 HDR
    /// display light. `None` on the field set below for a
    /// display-referred source, which needs neither.
    pub scene: Option<SceneReferredEnd>,
    /// 1 brings a BT.2020 frame into BT.709 first, gamut-compressing
    /// what BT.709 cannot hold; 0 leaves the basis alone. Never 1
    /// alongside `tone_map_op == 3`: an ACES ODT maps its own gamut, and
    /// both want the same binding for their tables.
    pub gamut_op: u32,
    /// The output's luma coefficients — BT.709 or BT.2020.
    pub luma_coefs: [f32; 3],
    /// Interleaved chroma (NV12, P010) rather than separate planes.
    pub semi_planar: bool,
    /// 8 or 10.
    pub bits: u32,
    /// Left shift on each stored sample: 6 for P010, 0 otherwise.
    pub sample_shift: u32,
}

impl Yuv420Target {
    /// The bytes a `width` x `height` frame occupies in this layout.
    pub fn byte_len(&self, width: u32, height: u32) -> usize {
        let px = (width as usize) * (height as usize);
        let sample = if self.bits == 8 { 1 } else { 2 };
        // Luma plus two chroma planes at a quarter the samples each.
        (px + px / 2) * sample
    }
}

/// How a scene-referred clip finishes: the step `Renderer::display_render`
/// runs on the CPU frame after the grade, when there is no GPU path.
///
/// `kind` is `klipp_color::hdr::HdrTransfer::display_light_kind`, `scale`
/// its `scene_scale` and `exposure` its `display_exposure`. This layer
/// does not interpret them beyond passing them to the shader.
#[derive(Clone, Copy, Debug)]
pub struct SceneReferredEnd {
    /// The SDR display tone map (false) or HDR display light (true).
    pub display_light: bool,
    pub kind: u32,
    pub scale: f32,
    pub exposure: f32,
}

/// A frame's light levels, in cd/m², as the GPU measured them while it
/// encoded: what HDR10's MaxCLL (the brightest channel anywhere) and
/// MaxFALL (the frame's average) are made of.
///
/// `sum_nits` is a sum rather than a mean so a caller can weight frames
/// the way `klipp_color::hdr::FrameLight` does.
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuFrameLight {
    pub max_nits: f32,
    pub sum_nits: f64,
    pub pixels: u64,
}

/// Log-mode wheels (low, mid, high) as nine floats; zero when off.
fn flat_wheels(w: Option<[(f32, f32, f32); 3]>) -> [f32; 9] {
    let [l, m, h] = w.unwrap_or([(0.0, 0.0, 0.0); 3]);
    [l.0, l.1, l.2, m.0, m.1, m.2, h.0, h.1, h.2]
}

/// A 3×3 matrix as nine row-major floats, the layout the shaders read.
fn flat3(m: [[f32; 3]; 3]) -> [f32; 9] {
    [m[0][0], m[0][1], m[0][2], m[1][0], m[1][1], m[1][2], m[2][0], m[2][1], m[2][2]]
}

// 50 words = 200 bytes (still under 256-byte PARAMS_SIZE)

// 4 fields × 4 bytes = 16 bytes

const _: () = assert!(std::mem::size_of::<GpuHdrZonesParams>() as u64 <= PARAMS_SIZE);

const _: () = assert!(std::mem::size_of::<GpuApplyMatteParams>() == 32, "the WGSL Params is 32 bytes");

// 4 fields × 4 bytes = 16 bytes — the smallest legal uniform.

// 8 × 4 bytes + 2 × 48 = 128 bytes, inside PARAMS_SIZE

/// The primaries matrices a LUT lookup is bracketed with: working space
/// into the LUT's input primaries before, its output primaries back
/// after, rows first; `None` where the two agree. `klipp_color::lut_bracket`
/// makes them from a LUT's tags.
pub type LutBracket = (Option<[[f32; 3]; 3]>, Option<[[f32; 3]; 3]>);

// 64 fields × 4 bytes = 256 bytes: the whole PARAMS_SIZE

// 8 u32 + vec4 = 48 bytes (well under 256-byte PARAMS_SIZE)

// 8 fields × 4 bytes = 32 bytes

// 8 fields × 4 bytes = 32 bytes

// 12 fields × 4 bytes = 48 bytes (multiple of 16)

// 8 fields × 4 bytes = 32 bytes

// 8 fields x 4 bytes = 32 bytes

// 4 fields x 4 bytes = 16 bytes

// 4 fields x 4 bytes = 16 bytes

/// [`crate::displace`]'s pass (#2702).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct DisplaceParams {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) amount: f32,
    pub(crate) size: f32,
    pub(crate) speed: f32,
    pub(crate) direction: u32,
    pub(crate) seed: u32,
    pub(crate) time: f32,
}
// 8 fields x 4 bytes = 32 bytes

/// [`crate::warp`]'s pass (#2758), in the shader's order.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct WarpParams {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) mode: u32,
    pub(crate) direction: u32,
    pub(crate) amplitude: f32,
    pub(crate) wavelength: f32,
    pub(crate) speed: f32,
    pub(crate) angle: f32,
    pub(crate) strength: f32,
    pub(crate) radius: f32,
    pub(crate) center_x: f32,
    pub(crate) center_y: f32,
    pub(crate) band: f32,
    pub(crate) time: f32,
    pub(crate) _pad0: u32,
    pub(crate) _pad1: u32,
}
// 16 fields x 4 bytes = 64 bytes

/// [`crate::gradient_map`]'s pass (#2758), in the shader's order.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct GradientMapParams {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) shadow_r: f32,
    pub(crate) shadow_g: f32,
    pub(crate) shadow_b: f32,
    pub(crate) mid_r: f32,
    pub(crate) mid_g: f32,
    pub(crate) mid_b: f32,
    pub(crate) highlight_r: f32,
    pub(crate) highlight_g: f32,
    pub(crate) highlight_b: f32,
    pub(crate) mix: f32,
    pub(crate) luma_yr: f32,
    pub(crate) luma_yg: f32,
    pub(crate) luma_yb: f32,
    pub(crate) _pad: u32,
}
// 16 fields x 4 bytes = 64 bytes

// 4 fields x 4 bytes = 16 bytes

// 12 fields x 4 bytes = 48 bytes, a multiple of 16 as a uniform needs

// 4 fields x 4 bytes = 16 bytes

// 8 fields x 4 bytes = 32 bytes

// 8 fields x 4 bytes = 32 bytes

// 4 fields x 4 bytes = 16 bytes

// 8 fields x 4 bytes = 32 bytes

// 8 fields x 4 bytes = 32 bytes

// 8 fields x 4 bytes = 32 bytes

// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct BinocularVignetteParams {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) amount: f32,
    pub(crate) spacing: f32,
    pub(crate) eye_w: f32,
    pub(crate) eye_h: f32,
    pub(crate) softness: f32,
    pub(crate) fringe: f32,
}
// 8 fields x 4 bytes = 32 bytes

// ─── Box blur Gaussian approximation (same as CPU version) ───────────────────

// ─── WGSL Compute Shaders ────────────────────────────────────────────────────

/// WGSL helper functions that decode sRGB u8 storage to linear f32
/// on load and re-encode on store. Shared by every effect shader
/// whose math requires linear-light values for physical correctness
/// (averaging — blur, bloom, denoise, light rays, film halation;
/// spatial weighting — sharpen, clarity, chromatic aberration;
/// compositing — transitions, bloom blend).
///
/// The `pack` half clamps to `[0, 1]` only at the u8 storage
/// boundary — intermediate values may exceed 1.0 throughout the
/// shader. When the full linear f32 storage path lands in Phase 10
/// this entire helper goes away (storage will already be linear).
///
/// Alpha channel stays linear per sRGB spec — gamma applies only to
/// the three colour channels.
///
/// Exposed as a `macro_rules!` so it can be spliced into `concat!()`
/// shader definitions (which only accept literals, not `const` refs).
// Storage buffers are now native linear-f32 `array<vec4<f32>>`. The
// historical `unpack` / `pack` helpers used to decode/encode sRGB-u8 at
// the storage boundary; now they are identity passes on `vec4<f32>`,
// preserved as a no-op so the ~36 shaders that still reference them
// compile unchanged. The sRGB↔linear component helpers remain for
// shaders that must linearise user-supplied sRGB colour parameters
// (for example `SHADER_TRANSITION`).
/// How a pixel is stored, as the shaders see it.
///
/// Identity today: a frame is `vec4<f32>` in the buffer and in the
/// registers. The pair exists so the *storage* form can change — half
/// floats at half the bandwidth being the obvious candidate — without
/// touching any shader's arithmetic. Every shader that indexes a pixel
/// buffer includes this and goes through it, which the
/// `pixel_access_goes_through_pack` test enforces on the source.
macro_rules! shader_pixel_storage_wgsl {
    () => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/gpu/shaders/include/pixel_storage.wgsl"))
    };
}

// The shared piece `include/display_dither.wgsl` (#2890):
// [`crate::dither`] as WGSL: the display encodes' dither and rounding,
// the export's own pattern (#2072). The literals are
// `DITHER_HASH_MUL_A`, `DITHER_HASH_MUL_B`, `DITHER_SECOND_DRAW` and
// `ON_CODE_TOLERANCE`, which `display_dither_wgsl_carries_the_rust_constants`
// holds equal,
// and `4294967296.0` is `u32::MAX as f32`. Computed in the encode's own
// pass from the pixel's index alone: no buffer, no extra pass, and the
// same pixels every time a frame is drawn.


/// [`crate::UNPREMULTIPLY_MIN_ALPHA`] as WGSL: the same literal, which a
/// test holds equal to the constant.
macro_rules! unpremultiply_min_alpha_wgsl {
    () => {
        "1e-6"
    };
}

/// A mapped GPU buffer copied into `Vec<f32>`, in parallel, without
/// zeroing the destination first.
///
/// This is the largest single cost in a 4K export, and none of it is
/// arithmetic. Reading 132 MB out of a mapped staging buffer took 30 ms
/// the obvious way — `cast_slice(&data).to_vec()` — because that is one
/// core, reading write-combined memory the GPU driver did not intend to
/// be read fast, into a freshly allocated buffer whose pages the kernel
/// faults in as they are first touched, having already zeroed them.
///
/// So: the capacity is allocated but never initialised, the copy writes
/// every byte of it in parallel bands, and `set_len` publishes it
/// afterwards. Any four bytes are a valid `f32` — there is no invalid
/// bit pattern to land on — so the only requirement is that the whole
/// length is written, which `copy_from_slice` on exact-size chunks
/// guarantees or panics.
///
/// Bands of 1 MB rather than `samples / threads`: the point is to have
/// several loads outstanding against slow memory, and a fixed band keeps
/// that true whatever the frame size.
fn mapped_bytes_to_f32(data: &[u8], samples: usize) -> Vec<f32> {
    use rayon::prelude::*;
    const BAND: usize = 1 << 20;

    let bytes = samples * 4;
    if data.len() < bytes {
        return vec![0.0f32; samples];
    }
    let mut out: Vec<f32> = Vec::with_capacity(samples);
    {
        let spare = out.spare_capacity_mut();
        // SAFETY: `spare` is `samples` f32s of uninitialised capacity, so
        // it is `bytes` addressable bytes; nothing reads them before the
        // copy below writes all of them.
        let dst = unsafe {
            std::slice::from_raw_parts_mut(spare.as_mut_ptr() as *mut u8, bytes)
        };
        dst.par_chunks_mut(BAND)
            .zip(data[..bytes].par_chunks(BAND))
            .for_each(|(d, s)| d.copy_from_slice(s));
    }
    // SAFETY: every byte of the first `samples` f32s was just written.
    unsafe { out.set_len(samples) };
    out
}

// The shared piece `include/hdr_display_light.wgsl` (#2890):
// A scene-referred clip's display-light pass, as WGSL.
//
// `klipp_color::hdr::hdr_display_light`: a graded scene-linear pixel to
// HDR display light, 1.0 = 203 cd/m². Its own macro because two shaders
// need it — the export's fused Y'CbCr writer and the viewer's display
// encode, for HDR monitoring — and two hand-written copies of a
// three-branch derivation with a highlight roll-off is how they drift.
//
// The includer must supply, as its own function:
//
// ```wgsl
// fn h_working_luma() -> vec3<f32>   // the working space's luma weights
// ```
//
// which the export reads out of its baked table and the viewer out of its
// params. Everything else here is closed form.

/// The pixel storage hook as a string, for a shader assembled at runtime
/// rather than by `concat!` — `nv12_gpu`'s, which is built from the
/// shared colourspace prelude plus its own body and now writes into a
/// chain-format frame buffer.
pub(crate) fn shader_pixel_storage_source() -> &'static str {
    shader_pixel_storage_wgsl!()
}

// `include/mask_sdf.wgsl`, a window's shapes as WGSL: the `MaskRecord`
// layout and `eval_record`, the coverage of one shape at a pixel (signed
// distance, feather, invert, opacity). The including shader binds
// `points: array<vec2<f32>>`, which `bezier_sdf` reads. One copy for the
// window drawn into a matte (`SHADER_MASK_V2`) and the window drawn
// inside an effect's mix (#1252), so the two can't drift apart.

// Opacity — multiply linear RGBA by a scalar. In linear light,
// opacity 0.5 means "half the photons" (matching DaVinci / Nuke);
// the sRGB u8 version multiplied gamma-encoded bytes, which
// produced visibly darker-than-half output.
mod layer;
mod contrast_protect;
mod surface_light;
mod flow;
mod transitions;
use transitions::*;
mod composite;
use composite::*;
mod mattes;
use mattes::*;
mod colour;
use colour::*;
mod blur;
use blur::*;
mod light;
use light::*;
mod generators;
use generators::*;
mod sprites;
use sprites::*;
mod temporal;
use temporal::*;
#[cfg(feature = "test-hooks")]
pub use temporal::pin_neighbours_in_flight;
pub use temporal::neighbours_skipped;
mod output;
use output::*;
mod build;
mod stack;
mod chain;
mod effect_params;
use effect_params::*;
#[cfg(test)]
mod shader_files;
pub mod planar;
mod planar_wgsl;
pub use flow::FlowKey;
mod mask_mix;

const SHADER_CROP: &str = gpu_shader!("geometry/crop.wgsl");

const SHADER_TRANSFORM: &str = gpu_shader!("geometry/transform.wgsl");

// Chroma-key math lives in sRGB YCbCr space (that's where `key_cb` /
// `key_cr` are defined — they're derived from the user-picked sRGB
// colour). Storage buffers are now linear-f32, so we re-encode to
// sRGB inside the shader for the YCbCr distance test, then write back
// the (possibly alpha-faded) linear pixel.
/// The picture resampled through the corner pin's inverse map: each
/// output pixel finds the picture pixel that lands on it and reads it
/// bilinearly; outside the picture, and behind the eye (a denominator
/// at or below zero), it is transparent. Mirrors the CPU arm in
/// `cpu.rs`, coefficient for coefficient.
const SHADER_CORNER_PIN: &str = gpu_shader!("geometry/corner_pin.wgsl");

/// The HDR10 bridge as it is compiled: the colourspace prelude, shared
/// with the P010 decoder so the PQ/HLG curves on either side are one
/// definition, then [`SHADER_HDR10_TO_LINEAR_BODY`]. Built once, so the
/// pipeline and [`shader_sources`]' checks read the same text.
fn shader_hdr10_to_linear() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(|| format!("{}{}{}", crate::color_wgsl::WGSL_PRELUDE, crate::color_wgsl::WGSL_HELPERS, SHADER_HDR10_TO_LINEAR_BODY))
}

/// [`shader_hdr10_to_linear`] for 16-bit codes: [`SHADER_HDR16_TO_LINEAR_BODY`].
fn shader_hdr16_to_linear() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(|| format!("{}{}{}", crate::color_wgsl::WGSL_PRELUDE, crate::color_wgsl::WGSL_HELPERS, SHADER_HDR16_TO_LINEAR_BODY))
}

/// [`SHADER_SCENE_TO_DISPLAY_BODY`] as compiled, after the colour helpers
/// that hold the tone map. Built once, like [`shader_hdr10_to_linear`].
fn shader_scene_to_display() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(|| format!("{}{}{}", crate::color_wgsl::WGSL_PRELUDE, crate::color_wgsl::WGSL_HELPERS, SHADER_SCENE_TO_DISPLAY_BODY))
}



// An sRGB RGBA8 sprite, rows y0..y1, decoded to linear with its alpha
// kept straight.

/// Each 16-bit sRGB code's linear value (the code over 65535, decoded),
/// then each code over 65535, alpha's value: the one decode of a 16-bit
/// frame, on the CPU (`klipp-render`'s `rgba64_to_linear`) and on the
/// GPU's bridge alike, so a frame seeded on either is the same to the bit
/// (#1451). Built once.
pub fn srgb16_decode_table() -> &'static [f32] {
    static TABLE: std::sync::OnceLock<Vec<f32>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let codes = 0..=u16::MAX;
        codes.clone().map(|v| srgb_to_linear_scalar(v as f32 / 65535.0)).chain(codes.map(|v| v as f32 / 65535.0)).collect()
    })
}

/// Each 8-bit sRGB code's linear value, as every CPU route decodes one
/// (this crate's `srgb_u8_to_linear_f32_rgba` and
/// `klipp_color::transfer::srgb_to_linear` of the code over 255, the same
/// arithmetic): what the 8-bit bridges look a code up in, so a frame put
/// on the GPU is the CPU's to the bit.
///
/// Then each code over 255, alpha's value, as the CPU divides it: the
/// shader's division is good only to 2.5 ULP.
fn srgb8_to_linear() -> [f32; 512] {
    std::array::from_fn(|i| if i < 256 { srgb_to_linear_scalar(i as f32 / 255.0) } else { (i - 256) as f32 / 255.0 })
}

// The ACES Rec.709 SDR output transform, on the GPU.
//
// Why this exists: the same 4K timeline exports at 51 fps in a Rec.709
// project and 8 in an AcesCG one, and essentially all of the difference
// is this transform running per-pixel on one core. The colour-managed
// pipeline being the slow one is the wrong way round.
//
// Why it is shaped like this: the transform is *not* a 3D LUT. Measured
// — a shaper plus a 3D LUT baked from `klipp_color::aces` is out by 51
// code values at 33³ and 22 at 97³/14.6 MB, because the red modifier,
// the glow and the gamut limit change too fast in saturated mid-tones
// for any practical grid. But it decomposes: closed-form arithmetic
// either side of two *per-channel* tone curves. Those curves carry the
// AMPAS coefficient tables and a knot search, so they arrive as tables
// baked from the Rust (8 KB, 0.041 levels); the arithmetic here is
// short enough to read. The derived matrices arrive as data too, since
// transcribing a computed number is how two implementations drift.
//
// `aces_rec709_matches_the_cpu` is what keeps this honest — the last
// WGSL copy of ACES in this codebase was removed, and a copy without a
// parity test is exactly what should be.
// The shared piece `include/bt2020_gamut.wgsl` (#2890):
// The BT.2020 -> BT.709 display mapping, as WGSL.
//
// A BT.2020 project's display transform is a basis change and then the
// BT.2407-style gamut compression from `klipp_color::gamut` — ~15 ms a
// 4K frame on the CPU for ordinary colour and ~32 for wholly
// saturated, which is why the viewer's fast path declined the whole
// project.
//
// Same division as ACES: the reach tables and cusps arrive as data,
// built by bisecting each gamut's boundary 24 times per grid point,
// and only the arithmetic that reads them lives here. The includer
// must declare `gamut: array<f32>` as a storage binding.

// The shared piece `include/hdr_encode.wgsl` (#2890):
// A PQ or HLG delivery's encode, as WGSL.
//
// `ExportDisplay::encode` on `ExportPath::Pq` / `Hlg`: HDR display light
// in the working basis (1.0 = 203 cd/m², BT.2408) → the delivery's
// signal. The basis matrix, the working space's luma weights and the PQ
// table come from `klipp_color::hdr::hdr_encode_gpu_data` in the
// `tables` binding; everything else here is closed form.
//
// The PQ curve is read from the shipped table rather than evaluated,
// because the CPU reads a table too — evaluating ST-2084 properly here
// would put the shader 3e-5 away from the twin by being *more* right,
// which is not what a parity test can hold.

// The shared piece `include/aces_rec709.wgsl` (#2890):
// The ACES Rec.709 SDR output transform, as WGSL for any shader that
// needs it.
//
// A macro rather than a second copy: the export's fused Y'CbCr writer
// needs the same transform as the viewer's display encode, and two
// hand-written ACES chains is precisely the situation that got the
// last one deleted.
//
// The includer must declare these bindings, and a `params` with
// nothing required of it:
//
// ```wgsl
// @group(0) @binding(3) var<storage, read> tables: array<f32>;
// ```
//
// `aces` is `klipp_color::aces::aces_rec709_gpu_data` — see there for
// why the curves and matrices are data and the rest is code.


// ─── Qualifier (Secondary Color Correction) GPU Shader ──────────────────────

// ─── Film Grain GPU Shader ──────────────────────────────────────────────────

// ─── Alpha Mask GPU Shaders ─────────────────────────────────────────────────

// ─── Light effect shaders ──────────────────────────────────────────────────────

// ─── New effect shaders ─────────────────────────────────────────────────────

const SHADER_MOSAIC: &str = gpu_shader!("stylise/mosaic.wgsl");

const SHADER_MIRROR: &str = gpu_shader!("geometry/mirror.wgsl");

const SHADER_DISPLACE: &str = gpu_shader!("stylise/displace.wgsl");

/// [`crate::warp`]'s pass (#2758): the shared warps, the Warp's own
/// choice among them, and the displace sampler.
/// [`crate::gradient_map`]'s pass (#2758).
const SHADER_GRADIENT_MAP: &str = gpu_shader!("stylise/gradient_map.wgsl");

const SHADER_WARP: &str = gpu_shader!("stylise/warp.wgsl");

const SHADER_REFLECTION: &str = gpu_shader!("stylise/reflection.wgsl");

const SHADER_LENS_DISTORTION: &str = gpu_shader!("geometry/lens_distortion.wgsl");

const SHADER_LETTERBOX: &str = gpu_shader!("geometry/letterbox.wgsl");


// ══════════════════════════════════════════════════════════════════════════════
// GPU Scope Rendering Shaders
// ══════════════════════════════════════════════════════════════════════════════

// ── Waveform bin accumulation (atomic histogram per column) ───────────────────
// Input is linear-f32 scene light; bins are sRGB code values. Video scopes are
// always plotted in the display-encoded domain — 0 / 100 IRE map to sRGB 0 /
// 255, NOT to linear 0.0 / 1.0. Before binning we gamma-encode each channel
// via the sRGB OETF so a mid-grey (linear 0.18) lands at bin ~118 instead of
// bin ~46, restoring the FCP / DaVinci trace distribution.
#[cfg(test)]
mod unpremultiply_rule_tests {
    /// The shaders unpremultiply by the rule the CPU does (#1923): the
    /// literal they're written with is the constant.
    #[test]
    fn the_shaders_and_the_cpu_share_one_alpha_rule() {
        assert_eq!(unpremultiply_min_alpha_wgsl!().parse::<f32>().unwrap(), crate::UNPREMULTIPLY_MIN_ALPHA);
        for shader in [super::SHADER_SHUTTER, super::SHADER_ALPHA_FORM] {
            assert!(shader.contains(concat!("> ", unpremultiply_min_alpha_wgsl!(), " {")), "the rule is in the shader");
        }
    }
}

#[cfg(test)]
mod display_dither_rule_tests {
    use crate::dither::{DITHER_HASH_MUL_A, DITHER_HASH_MUL_B, DITHER_SECOND_DRAW};

    /// The display encodes dither with the export's pattern (#2072): the
    /// literals the WGSL is written with are the Rust constants, and every
    /// 8-bit display encode takes it.
    #[test]
    fn display_dither_wgsl_carries_the_rust_constants() {
        let wgsl = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/gpu/shaders/include/display_dither.wgsl"));
        for (name, k) in [("A", DITHER_HASH_MUL_A), ("B", DITHER_HASH_MUL_B), ("second draw", DITHER_SECOND_DRAW)] {
            assert!(wgsl.contains(&format!("0x{k:08x}u")), "{name} is not the Rust constant");
        }
        assert_eq!(4294967296.0f32, u32::MAX as f32, "the WGSL's divisor is the CPU's");
        assert_eq!(crate::dither::ON_CODE_TOLERANCE, 1e-3, "the WGSL's on-code tolerance is the CPU's");
        assert!(wgsl.contains("abs(x - round(x)) < 1e-3"), "the WGSL leaves on-code values undithered");
        for (name, shader) in [
            ("SHADER_LINEAR_TO_DISPLAY_U8", super::SHADER_LINEAR_TO_DISPLAY_U8),
            ("SHADER_ACES_REC709_U8", super::SHADER_ACES_REC709_U8),
            ("SHADER_BT2020_DISPLAY_U8", super::SHADER_BT2020_DISPLAY_U8),
        ] {
            assert!(shader.contains(wgsl), "{name} lacks the dither");
            assert!(shader.contains("let d = dither_at(idx);"), "{name} does not dither its pixels");
        }
    }
}

#[cfg(test)]
mod generator_grain_rule_tests {
    use crate::dither::{DITHER_HASH_MUL_A, DITHER_HASH_MUL_B};
    use crate::generator_grain::{GRAIN_SALTS, GRAIN_TAPER_CODES};

    /// The living gradient's pass carries the CPU's grain (#2073): the
    /// literals the WGSL piece is written with (`include/generator_grain.wgsl`)
    /// are the Rust constants: `DITHER_HASH_MUL_A`, `DITHER_HASH_MUL_B` (the
    /// hash both share), `GRAIN_SALTS` and `GRAIN_TAPER_CODES` (`63.75` is
    /// `255 / 4`). The grain is computed in the generator's own pass from
    /// the pixel's index alone.
    #[test]
    fn generator_grain_wgsl_carries_the_rust_constants() {
        let wgsl = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/gpu/shaders/include/generator_grain.wgsl"));
        for (name, k) in [("A", DITHER_HASH_MUL_A), ("B", DITHER_HASH_MUL_B)] {
            assert!(wgsl.contains(&format!("x * 0x{k:08x}u")), "hash multiplier {name} is not the Rust constant");
        }
        for (n, salt) in GRAIN_SALTS.iter().enumerate() {
            assert!(wgsl.contains(&format!("grain_hash(index ^ 0x{salt:08x}u) >> 8u) / 16777216.0")), "salt {n} is not the Rust constant, or its draw isn't the CPU's 24 bits");
        }
        assert_eq!(255.0 / GRAIN_TAPER_CODES, 63.75, "the WGSL's taper is the CPU's");
        assert!(wgsl.contains("min(v, 1.0 - v) * 63.75"), "the WGSL tapers as the CPU does");
        assert!(wgsl.contains("(a + b + c - 1.5) * 2.0"), "the WGSL scales as the CPU does");
        assert!(super::SHADER_LIVING_GRADIENT.contains(wgsl), "the living gradient lacks the grain");
        assert!(super::SHADER_LIVING_GRADIENT.contains("let n = grain_noise(i);"), "the living gradient does not grain its pixels");
    }
}

#[cfg(test)]
mod bridge_u8_tests {
    use super::*;

    /// Build a synthetic linear-f32 RGBA frame: R = x/(w-1), G = y/(h-1),
    /// B = (x+y)/(w+h-2), A = 1.0. Deterministic and exercises a
    /// range of input values per channel.
    fn gradient_linear_frame(w: u32, h: u32) -> Vec<f32> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let r = x as f32 / (w.max(2) - 1) as f32;
                let g = y as f32 / (h.max(2) - 1) as f32;
                let b = (x + y) as f32 / ((w + h).max(2) - 2) as f32;
                out.push(r);
                out.push(g);
                out.push(b);
                out.push(1.0);
            }
        }
        out
    }

    /// CPU reference encode using the exact transfer formula the
    /// shader uses. Match within ±1 LSB tolerance because GPU floats
    /// (especially `pow`) and the LUT can disagree on one quantum at
    /// the edges of the curve.
    fn cpu_linear_to_u8(linear: &[f32]) -> Vec<u8> {
        fn enc(c: f32) -> u8 {
            let x = c.clamp(0.0, 1.0);
            let y = if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
            (y.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
        }
        let mut out = Vec::with_capacity(linear.len());
        for chunk in linear.chunks_exact(4) {
            out.push(enc(chunk[0]));
            out.push(enc(chunk[1]));
            out.push(enc(chunk[2]));
            out.push((chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
        out
    }

    #[test]
    fn bridge_u8_matches_cpu_within_one_lsb() {
        let Some(mut gpu) = GpuEffectsPipeline::new() else {
            eprintln!("skipping bridge_u8_matches_cpu_within_one_lsb: no GPU");
            return;
        };
        let (w, h) = (32u32, 32u32);
        let linear = gradient_linear_frame(w, h);
        let cpu = cpu_linear_to_u8(&linear);
        let gpu_out = gpu
            .linear_rgba_to_u8_gpu(&linear, w, h)
            .expect("GPU bridge must succeed when device is alive");

        assert_eq!(cpu.len(), gpu_out.len(), "output length must match");

        // Allow ±1 LSB to account for sRGB pow vs LUT rounding. In
        // practice we usually see exact equality but the looser bound
        // protects against future GPU driver-rounding differences.
        let mut max_delta = 0i32;
        let mut delta_pixels = 0usize;
        for (i, (c, g)) in cpu.iter().zip(gpu_out.iter()).enumerate() {
            let d = (*c as i32 - *g as i32).abs();
            if d > max_delta { max_delta = d; }
            if d > 0 { delta_pixels += 1; }
            assert!(
                d <= 1,
                "byte {i}: CPU={c} GPU={g} (delta {d} > 1 LSB)",
            );
        }
        // Print stats for debugging perf regressions.
        eprintln!("bridge_u8: max_delta={max_delta}, delta_pixels={delta_pixels} of {}", cpu.len());
    }

    /// An out-of-memory error is counted, and the next frame is still
    /// graded on the GPU: it used to switch GPU effects off, every later
    /// frame passed through ungraded, in an export too (#2394).
    #[test]
    fn a_frame_after_out_of_memory_is_still_graded() {
        let Some(mut gpu) = GpuEffectsPipeline::new() else {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
            eprintln!("skipping a_frame_after_out_of_memory_is_still_graded: no GPU");
            return;
        };
        let (w, h) = (32u32, 32u32);
        let linear = gradient_linear_frame(w, h);
        let before = crate::gpu_context::out_of_memory_errors();
        crate::gpu_context::note_out_of_memory();
        assert_eq!(crate::gpu_context::out_of_memory_errors(), before + 1, "the error is counted");
        assert!(!gpu.is_device_lost(), "out of memory does not take the GPU away");
        let graded = gpu.apply_vibrance_gpu(&linear, w, h, 0.8);
        assert_eq!(graded.len(), linear.len());
        assert!(graded != linear, "the frame after out of memory came out ungraded");
    }

    #[test]
    fn bridge_u8_handles_clamp_at_boundaries() {
        let Some(mut gpu) = GpuEffectsPipeline::new() else {
            eprintln!("skipping bridge_u8_handles_clamp_at_boundaries: no GPU");
            return;
        };
        // Pixels with values outside [0, 1] must clamp at the byte
        // boundary, not earlier — matches CPU `linear_rgba_to_u8`.
        let linear: Vec<f32> = vec![
            -0.5,  0.0,  1.0,  1.0,  // R clamps to 0 → byte 0
             1.5,  2.0,  0.5,  1.0,  // R/G clamp to 255
             0.5,  0.5,  0.5, -1.0,  // alpha clamps to 0
             0.5,  0.5,  0.5,  2.0,  // alpha clamps to 255
        ];
        let out = gpu.linear_rgba_to_u8_gpu(&linear, 4, 1).expect("GPU bridge must succeed");
        assert_eq!(out.len(), 16);
        // Pixel 0: R = clamp(-0.5) → 0
        assert_eq!(out[0], 0, "negative R must clamp to 0");
        // Pixel 1: R = clamp(1.5) → 255, G = clamp(2.0) → 255
        assert_eq!(out[4], 255, "R > 1 must clamp to 255");
        assert_eq!(out[5], 255, "G > 1 must clamp to 255");
        // Pixel 2: alpha = clamp(-1.0) → 0
        assert_eq!(out[11], 0, "negative alpha must clamp to 0");
        // Pixel 3: alpha = clamp(2.0) → 255
        assert_eq!(out[15], 255, "alpha > 1 must clamp to 255");
    }

    #[test]
    fn bridge_u8_zero_pixels_returns_empty() {
        let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
        let out = gpu.linear_rgba_to_u8_gpu(&[], 0, 0).expect("zero-pixel call must succeed");
        assert!(out.is_empty());
    }

    #[test]
    fn bridge_u8_short_input_returns_none() {
        let Some(mut gpu) = GpuEffectsPipeline::new() else { return };
        // Asking for 4×4 frame but only providing 8 floats (2 pixels).
        // GPU bridge should refuse rather than read garbage.
        let short = vec![0.5f32; 8];
        let out = gpu.linear_rgba_to_u8_gpu(&short, 4, 4);
        assert!(out.is_none(), "short input must return None for caller fallback");
    }
}

/// Every compute shader parses and validates, without a GPU. A shader that
/// does not compile makes `GpuEffectsPipeline::new` return `None`, which the
/// GPU tests take for "no GPU here" and skip: a broken shader would pass.
/// Every compute shader in this module, by name.
///
/// One list, two consumers: the naga test that parses them all, and the
/// source lint that checks every pixel access goes through the storage
/// hook. A shader missing from here is a shader neither protects, so
/// add new ones as they are written.
pub fn shader_sources() -> Vec<(&'static str, &'static str)> {
    vec![
        ("SHADER_MASK_MIX", mask_mix::SHADER_MASK_MIX),
        ("SHADER_MASK_MIX_WINDOW", mask_mix::SHADER_MASK_MIX_WINDOW),
        ("SHADER_MASK_MIX_IN_PLACE", mask_mix::SHADER_MASK_MIX_IN_PLACE),
        ("SHADER_MASK_MIX_WINDOW_IN_PLACE", mask_mix::SHADER_MASK_MIX_WINDOW_IN_PLACE),
        ("LAYER_ROWS", layer::ROWS),
        ("LAYER_BOUNDS", layer::BOUNDS),
        ("LAYER_CAST", layer::CAST),
        ("LAYER_CORNERS", layer::CORNERS),
        ("LAYER_OVER", layer::OVER),
        ("LAYER_FILL_SMALL", layer::FILL_SMALL),
        ("LAYER_FILL_OVER", layer::FILL_OVER),
        ("SHADER_OPACITY", SHADER_OPACITY),
        ("SHADER_COLOR_FADE", SHADER_COLOR_FADE),
        ("SHADER_BLUR_H", SHADER_BLUR_H),
        ("SHADER_BLUR_V", SHADER_BLUR_V),
        ("SHADER_SHARPEN", SHADER_SHARPEN),
        ("SHADER_CROP", SHADER_CROP),
        ("SHADER_TRANSFORM", SHADER_TRANSFORM),
        ("SHADER_CORNER_PIN", SHADER_CORNER_PIN),
        ("SHADER_CHROMA_KEY", SHADER_CHROMA_KEY),
        ("SHADER_LINEAR_TO_DISPLAY_U8", SHADER_LINEAR_TO_DISPLAY_U8),
        ("SHADER_HDR_NIT_MAP", SHADER_HDR_NIT_MAP),
        ("SHADER_LINEAR_TO_U8", SHADER_LINEAR_TO_U8),
        ("SHADER_U8_TO_LINEAR", SHADER_U8_TO_LINEAR),
        ("SHADER_RGBA64_TO_LINEAR", SHADER_RGBA64_TO_LINEAR),
        ("SHADER_LINEAR_TO_YUV420", SHADER_LINEAR_TO_YUV420),
        ("SHADER_ACES_REC709_U8", SHADER_ACES_REC709_U8),
        ("SHADER_BT2020_DISPLAY_U8", SHADER_BT2020_DISPLAY_U8),
        ("SHADER_TRANSITION", SHADER_TRANSITION),
        ("SHADER_PIECES", SHADER_PIECES),
        ("SHADER_SHUTTER", SHADER_SHUTTER),
        ("SHADER_COLOR_GRADE", SHADER_COLOR_GRADE),
        ("SHADER_HUESAT", SHADER_HUESAT),
        ("SHADER_QUALIFIER", SHADER_QUALIFIER),
        ("SHADER_LUMA_KEY", SHADER_LUMA_KEY),
        ("SHADER_SOFT_CLIP", SHADER_SOFT_CLIP),
        ("SHADER_GRAIN", SHADER_GRAIN),
        ("SHADER_ALPHA_BLUR_H", SHADER_ALPHA_BLUR_H),
        ("SHADER_ALPHA_BLUR_V", SHADER_ALPHA_BLUR_V),
        ("SHADER_ALPHA_DILATE", SHADER_ALPHA_DILATE),
        ("TITLE_STYLES", gpu_shader!("title/title_styles.wgsl")),
        ("SHADER_BLOOM_THRESHOLD", SHADER_BLOOM_THRESHOLD),
        ("SHADER_BLOOM_BLEND", SHADER_BLOOM_BLEND),
        ("SHADER_GLOW_BLEND", SHADER_GLOW_BLEND),
        ("SHADER_TITLE_METAL_SHOT", SHADER_TITLE_METAL_SHOT),
        ("SHADER_CONTRAST_PROTECT", contrast_protect::SHADER_CONTRAST_PROTECT),
        ("SHADER_SURFACE_LIGHT", surface_light::SHADER_SURFACE_LIGHT),
        ("SHADER_LIGHT_RAYS", SHADER_LIGHT_RAYS),
        ("SHADER_VIGNETTE", SHADER_VIGNETTE),
        ("SHADER_LENS_FLARE", SHADER_LENS_FLARE),
        ("SHADER_ANAMORPHIC_STREAK", SHADER_ANAMORPHIC_STREAK),
        ("SHADER_FILM_HALATION", SHADER_FILM_HALATION),
        ("SHADER_LUT3D", SHADER_LUT3D),
        ("SHADER_INVERT", SHADER_INVERT),
        ("SHADER_POSTERIZE", SHADER_POSTERIZE),
        ("SHADER_CST", SHADER_CST),
        ("SHADER_GENERATOR", SHADER_GENERATOR),
        ("SHADER_LIVING_GRADIENT", SHADER_LIVING_GRADIENT),
        ("SHADER_GRAIN_GENERATOR", SHADER_GRAIN_GENERATOR),
        ("SHADER_SEGMENTS", SHADER_SEGMENTS),
        ("SHADER_MOSAIC", SHADER_MOSAIC),
        ("SHADER_EDGE_DETECT", SHADER_EDGE_DETECT),
        ("SHADER_CHANNEL_MIXER", SHADER_CHANNEL_MIXER),
        ("SHADER_DIRECTIONAL_BLUR", SHADER_DIRECTIONAL_BLUR),
        ("SHADER_EMBOSS", SHADER_EMBOSS),
        ("SHADER_MIRROR", SHADER_MIRROR),
        ("SHADER_LENS_DISTORTION", SHADER_LENS_DISTORTION),
        ("SHADER_DISPLACE", SHADER_DISPLACE),
        ("SHADER_WARP", SHADER_WARP),
        ("SHADER_GRADIENT_MAP", SHADER_GRADIENT_MAP),
        ("SHADER_REFLECTION", SHADER_REFLECTION),
        ("SHADER_DILATE", SHADER_DILATE),
        ("SHADER_ERODE", SHADER_ERODE),
        ("SHADER_DENOISE", SHADER_DENOISE),
        ("SHADER_GAMMA", SHADER_GAMMA),
        ("SHADER_HSV_ADJUST", SHADER_HSV_ADJUST),
        ("SHADER_NOISE_GENERATOR", SHADER_NOISE_GENERATOR),
        ("SHADER_MEDIAN_FILTER", SHADER_MEDIAN_FILTER),
        ("SHADER_RADIAL_BLUR", SHADER_RADIAL_BLUR),
        ("SHADER_LEVELS", SHADER_LEVELS),
        ("SHADER_EQUALIZE", SHADER_EQUALIZE),
        ("SHADER_EQUALIZE_HISTOGRAM", SHADER_EQUALIZE_HISTOGRAM),
        ("SHADER_EQUALIZE_CDF", SHADER_EQUALIZE_CDF),
        ("SHADER_LETTERBOX", SHADER_LETTERBOX),
        ("SHADER_CHROMATIC_ABERRATION", SHADER_CHROMATIC_ABERRATION),
        ("SHADER_BINOCULAR_VIGNETTE", SHADER_BINOCULAR_VIGNETTE),
        ("SHADER_PARTICLES", SHADER_PARTICLES),
        ("SHADER_ROLLING_SHUTTER", SHADER_ROLLING_SHUTTER),
        ("SHADER_STABILIZATION", SHADER_STABILIZATION),
        ("SHADER_COLOR_BALANCE", SHADER_COLOR_BALANCE),
        ("SHADER_HDR_ZONES", SHADER_HDR_ZONES),
        ("SHADER_WHEEL_SAT_BRI", SHADER_WHEEL_SAT_BRI),
        ("SHADER_RGB_CURVES", SHADER_RGB_CURVES),
        ("SHADER_MASK_V2", SHADER_MASK_V2),
        ("SHADER_VIBRANCE", SHADER_VIBRANCE),
        ("SHADER_COLOR_WARPER", SHADER_COLOR_WARPER),
        ("SHADER_CLARITY_BLUR_H", SHADER_CLARITY_BLUR_H),
        ("SHADER_CLARITY_BLUR_V", SHADER_CLARITY_BLUR_V),
        ("SHADER_MASK_CUTOUT", SHADER_MASK_CUTOUT),
        ("SHADER_GRADE_MIX", SHADER_GRADE_MIX),
        ("SHADER_APPLY_MATTE_CHAIN", SHADER_APPLY_MATTE_CHAIN),
        ("SHADER_CLARITY_APPLY", SHADER_CLARITY_APPLY),
        ("SHADER_BLEND_LAYER", SHADER_BLEND_LAYER),
        ("SHADER_MERGE_OVER", SHADER_MERGE_OVER),
        ("SHADER_PREMULTIPLY", SHADER_PREMULTIPLY),
        ("SHADER_VIEW_CHANNEL", SHADER_VIEW_CHANNEL),
        ("SHADER_ALPHA_FORM", SHADER_ALPHA_FORM),
        ("SHADER_SPRITE_ROWS", SHADER_SPRITE_ROWS),
        ("SHADER_MATTE_EXTRACT", SHADER_MATTE_EXTRACT),
        ("SHADER_MASK_MIN_H", SHADER_MASK_MIN_H),
        ("SHADER_MASK_MIN_V", SHADER_MASK_MIN_V),
        ("SHADER_MATTE_APPLY", SHADER_MATTE_APPLY),
        ("SHADER_SCALE_ALPHA", SHADER_SCALE_ALPHA),
        ("SHADER_U8_TO_TEXTURE", SHADER_U8_TO_TEXTURE),
        ("SHADER_SPRITE_PLACE", SHADER_SPRITE_PLACE),
        ("SHADER_SPRITE_GLYPHS", SHADER_SPRITE_GLYPHS),
        ("SHADER_U8_FIT_TO_LINEAR", SHADER_U8_FIT_TO_LINEAR),
        // Compiled into pipelines but missing from this list until #692,
        // which kept them out of the WGSL validation and pack lints.
        ("SHADER_HDR10_TO_LINEAR", shader_hdr10_to_linear()),
        ("SHADER_HDR16_TO_LINEAR", shader_hdr16_to_linear()),
        ("SHADER_SCENE_TO_DISPLAY", shader_scene_to_display()),
        ("SHADER_SPRITE_ROWS_F16", SHADER_SPRITE_ROWS_F16),
        ("SHADER_SPRITE_BOX_F16", SHADER_SPRITE_BOX_F16),
        ("SHADER_DIFFERENCE_KEY", SHADER_DIFFERENCE_KEY),
        ("SHADER_TEMPORAL_DENOISE", SHADER_TEMPORAL_DENOISE),
        ("SHADER_FILM_GRAIN", SHADER_FILM_GRAIN),
        ("SHADER_FOCUS_BLUR_BLEND", SHADER_FOCUS_BLUR_BLEND),
        ("SHADER_FOCUS_BLUR_PREMULTIPLY", SHADER_FOCUS_BLUR_PREMULTIPLY),
        ("SHADER_TILT_SHIFT_BLEND", SHADER_TILT_SHIFT_BLEND),
        ("SHADER_OLD_FILM", SHADER_OLD_FILM),
        ("SHADER_VHS", SHADER_VHS),
        ("SHADER_DEFLICKER", SHADER_DEFLICKER),
        ("SHADER_BLEND_MIX", SHADER_BLEND_MIX),
        ("SHADER_LAYER_OVER", SHADER_LAYER_OVER),
        // Assembled at runtime from the shared prelude and a body.
        ("SHADER_CHAIN_THUMB", thumbs::shader_chain_thumb()),
        ("SHADER_NV12", crate::nv12_gpu::nv12_shader()),
        ("SHADER_P010", crate::p010_gpu::p010_shader()),
    ]
    .into_iter()
    .chain(layer::SPILL.iter().map(|(name, source, _)| (*name, *source)))
    .collect()
}

#[cfg(test)]
mod shader_validation_tests {

    #[test]
    fn every_shader_compiles() {
        // The registered shaders, and the flow's, assembled per pipeline.
        let flow = super::flow::sources();
        // The planar tracker's too (#2754).
        let planar = super::planar_wgsl::sources();
        let shaders = super::shader_sources().into_iter().chain(flow.iter().map(|(n, s)| (*n, s.as_str()))).chain(planar.iter().map(|(n, s)| (*n, s.as_str())));
        let mut failures = Vec::new();
        for (name, src) in shaders {
            let module = match naga::front::wgsl::parse_str(src) {
                Ok(m) => m,
                Err(e) => {
                    failures.push(format!("{name}: {}", e.emit_to_string(src)));
                    continue;
                }
            };
            let mut validator = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all());
            if let Err(e) = validator.validate(&module) {
                failures.push(format!("{name}: {e:?}"));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}

#[cfg(test)]
mod shared_pipeline_tests {
    use super::*;

    fn bits(v: &[f32]) -> Vec<u32> {
        v.iter().map(|x| x.to_bits()).collect()
    }

    /// #2688: a renderer made after the first on a device reuses its
    /// pipelines instead of compiling them again, and draws the same.
    #[test]
    fn a_second_renderer_reuses_the_devices_pipelines() {
        let Some(ctx) = crate::gpu_context::GpuContext::shared() else {
            assert!(std::env::var_os("KLIPP_TEST_REQUIRE_GPU").is_none(), "GPU required");
            return;
        };
        let t = std::time::Instant::now();
        let mut first = GpuEffectsPipeline::with_context(ctx).expect("a renderer");
        let first_ms = t.elapsed().as_secs_f64() * 1000.0;
        let t = std::time::Instant::now();
        let mut second = GpuEffectsPipeline::with_context(ctx).expect("another");
        let second_ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("first renderer {first_ms:.0} ms, second {second_ms:.0} ms");
        assert!(Arc::ptr_eq(&first.pipelines, &second.pipelines), "the second renderer compiled its own");
        let frame: Vec<f32> = (0..16 * 16 * 4).map(|i| (i % 97) as f32 / 97.0).collect();
        let fx = [crate::BuiltinEffect::GaussianBlur { radius: 2.0 }, crate::BuiltinEffect::Sharpen { amount: 0.8 }];
        let a = first.apply_effects(&fx, &frame, 16, 16);
        let b = second.apply_effects(&fx, &frame, 16, 16);
        assert_ne!(a, frame, "the effects changed the picture");
        assert_eq!(bits(&a), bits(&b), "both draw the same");
    }
}
