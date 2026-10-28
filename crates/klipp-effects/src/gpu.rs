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
    /// gone past `outer`, as [`klipp_core::MaskShape::RadialGradient`].
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

pub struct GpuEffectsPipeline {
    readbacks: readbacks::Counters,
    // Arc-wrapped so the effects pipeline can share its device with
    // klipp-effects' NV12/P010 converters (and any future GPU-using
    // type) through `GpuContext`. Internal call sites deref
    // transparently via Arc's Deref<Target = wgpu::Device>.
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    // Compute pipelines — effects (2-buffer: input, output, params)
    opacity_pipeline: wgpu::ComputePipeline,
    color_fade_pipeline: wgpu::ComputePipeline,
    layer_gpu: layer::LayerGpu,
    mask_mix_gpu: mask_mix::MaskMixGpu,
    /// Optical-flow interpolation's pipelines and kept pairs (#2215),
    /// made on first use.
    flow_gpu: Option<flow::FlowGpu>,
    blur_h_pipeline: wgpu::ComputePipeline,
    blur_v_pipeline: wgpu::ComputePipeline,
    sharpen_pipeline: wgpu::ComputePipeline,
    crop_pipeline: wgpu::ComputePipeline,
    transform_pipeline: wgpu::ComputePipeline,
    corner_pin_pipeline: wgpu::ComputePipeline,
    chroma_key_pipeline: wgpu::ComputePipeline,
    // Compute pipelines — light effects
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
    // Compute pipelines — new effects
    invert_pipeline: wgpu::ComputePipeline,
    posterize_pipeline: wgpu::ComputePipeline,
    /// The colour-space transform effect's pass (#1746).
    cst_pipeline: wgpu::ComputePipeline,
    /// The LUT effects' tables on the GPU, the latest last, each kept with
    /// the preparation it was uploaded from so a table is uploaded once.
    effect_luts: std::cell::RefCell<Vec<(Arc<crate::colour_transform::LutTable>, Arc<wgpu::Buffer>)>>,
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
    // Compute pipeline — rolling shutter correction (uses huesat 4-binding layout)
    rsc_pipeline: wgpu::ComputePipeline,
    rsc_row_buf: Option<wgpu::Buffer>,
    rsc_row_buf_size: u64,
    // Compute pipeline — video stabilization (uses huesat 4-binding layout)
    stabilization_pipeline: wgpu::ComputePipeline,
    stabilization_buf: Option<wgpu::Buffer>,
    stabilization_buf_size: u64,
    // Compute pipelines — color grading (standalone, not part of effect chain)
    color_grade_pipeline: wgpu::ComputePipeline,
    huesat_pipeline: wgpu::ComputePipeline,
    huesat_bgl: wgpu::BindGroupLayout,
    /// Five bindings, for the one node that needs three pictures at
    /// once: input (the graded frame), output, params, the kept
    /// pre-secondary frame, and the matte.
    apply_matte_bgl: wgpu::BindGroupLayout,
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
    lut3d_pipeline: wgpu::ComputePipeline,
    lut3d_buf: Option<wgpu::Buffer>,
    lut3d_buf_size: u64,
    // Compute pipeline — color balance (temperature/tint/hue)
    color_balance_pipeline: wgpu::ComputePipeline,
    hdr_zones_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — wheel sat/bri (per-wheel saturation & brightness)
    wheel_sat_bri_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — RGB curves (LUT-based, reuses huesat_bgl 4-binding layout)
    rgb_curves_pipeline: wgpu::ComputePipeline,
    rgb_curves_lut_buf: wgpu::Buffer,
    // Compute pipeline — vibrance
    vibrance_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — a scene-referred clip's display step (step 11c)
    scene_to_display_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — color warper (12-sector hue/sat)
    color_warper_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — input transform (log-to-sRGB via 1D LUT, reuses huesat_bgl)
    // Compute pipelines — clarity (3-pass: blur_h, blur_v, apply)
    clarity_blur_h_pipeline: wgpu::ComputePipeline,
    clarity_blur_v_pipeline: wgpu::ComputePipeline,
    clarity_apply_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — mask matte generation (SDF power windows)
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
    /// The picture a Corrector's mix over all its stages blends against
    /// (its Keep/Mix stage pair, within the one node). A grade
    /// secondary's kept picture and a composite's second picture are
    /// pool slots ([`KeptPicture`], M3 step 17b).
    grade_mix_src: Option<wgpu::Buffer>,
    // Compute pipeline — qualifier (secondary color correction)
    qualifier_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — standalone luminance-threshold key (matte
    // producer, simpler than qualifier — luma in, matte out).
    luma_key_pipeline: wgpu::ComputePipeline,
    difference_key_pipeline: wgpu::ComputePipeline,
    difference_key_bgl: wgpu::BindGroupLayout,
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
    /// Temporal noise reduction on the chain (#1988): the chain's picture
    /// against up to two neighbour frames in buffers of their own.
    temporal_denoise_pipeline: wgpu::ComputePipeline,
    temporal_denoise_bgl: wgpu::BindGroupLayout,
    // Compute pipeline — standalone soft-clip / hard-clip transform
    // (per-channel Reinhard rolloff above threshold). Value
    // transform, not a matte producer.
    soft_clip_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — film grain overlay
    grain_pipeline: wgpu::ComputePipeline,
    // Compute pipelines — alpha mask operations (for title renderer)
    alpha_blur_h_pipeline: wgpu::ComputePipeline,
    alpha_blur_v_pipeline: wgpu::ComputePipeline,
    alpha_dilate_pipeline: wgpu::ComputePipeline,
    title_style_pipeline: wgpu::ComputePipeline,
    title_style_scratch: Option<title_styles::StyleScratch>,
    /// Metal titles that reflect the shot behind them, by stack slot,
    /// waiting for the layers below to be laid (#2706); emptied as a
    /// frame's stack begins.
    title_metal_shots: std::collections::HashMap<usize, title_styles::MetalShot>,
    title_metal_shot_pipeline: wgpu::ComputePipeline,
    title_metal_shot_scratch: Option<(wgpu::Buffer, u64)>,
    /// Contrast protection for titles (#2734) and its two small grids
    /// (measure, blur) with their cell count.
    contrast_protect_pipeline: wgpu::ComputePipeline,
    contrast_protect_scratch: Option<(wgpu::Buffer, wgpu::Buffer, u64)>,
    /// A pinned layer taking its surface's light and texture (#2751):
    /// its grid and blur, its row sums, and a frame of factors, with the
    /// cell and pixel counts they were made for.
    surface_light_pipeline: wgpu::ComputePipeline,
    surface_light_scratch: Option<surface_light::Scratch>,
    title_post_scratch: Option<wgpu::Buffer>,
    title_scene_bounds: Option<wgpu::Buffer>,
    title_outlines: Vec<(u64,wgpu::Buffer)>,
    // Compute pipelines — transitions (3-buffer: from, to, output, params)
    transition_pipeline: wgpu::ComputePipeline,
    /// Posed piece transitions (#985): `SHADER_PIECES`.
    pieces_pipeline: wgpu::ComputePipeline,
    /// A layer smeared across the shutter, in one pass (#1923).
    shutter_pipeline: wgpu::ComputePipeline,
    // ── linear-f32 → sRGB-u8 pixel encode pipeline ─────────────────
    // GPU replacement for the CPU `linear_rgba_to_u8` LUT pass that
    // costs 40-90 ms per 4K frame. Reads `array<vec4<f32>>` linear,
    // writes `array<u32>` packed sRGB-u8 RGBA. Output buffer is 4×
    // smaller than the input, which matters because the readback
    // dominates wall-time on this op (PCIe-bound, sub-ms compute).
    bridge_u8_pipeline: wgpu::ComputePipeline,
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
    /// The matrices and tone curves that shader reads, uploaded once.
    aces_rec709_data: Option<CachedTables>,
    /// Bound in that slot by a delivery that is not ACES, which never
    /// reads it.
    no_display_tables: wgpu::Buffer,
    no_light: wgpu::Buffer,
    /// BT.2020's reach tables, cusps and matrices — 148 KB, uploaded
    /// once.
    bt2020_gamut_data: Option<CachedTables>,
    hdr_encode_data: Option<CachedTables>,
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
    hdr_nit_buf: Option<wgpu::Buffer>,
    hdr_nit_staging: Option<wgpu::Buffer>,
    /// Map-readable copy of `bridge_u8_out_buf` for the display readback.
    display_u8_staging: Option<wgpu::Buffer>,
    /// `bridge_u8_out_buf` into a storage texture, one texel per
    /// invocation: the display bytes land in the viewer's texture
    /// without a copy, so no row-pitch alignment applies.
    u8_to_texture_pipeline: wgpu::ComputePipeline,
    u8_to_texture_bgl: wgpu::BindGroupLayout,
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
    sprite_place_pipeline: wgpu::ComputePipeline,
    /// The per-glyph sprite pass: sprite, output, params, glyph map,
    /// glyph table.
    sprite_glyphs_bgl: wgpu::BindGroupLayout,
    sprite_glyphs_pipeline: wgpu::ComputePipeline,
    /// The glyph table of the frame being placed (grows as needed).
    sprite_glyph_table: Option<wgpu::Buffer>,
    stack_buf_size: u64,
    /// Whether anything has been blended into the accumulator yet.
    stack_acc_touched: bool,
    stack_dims: (u32, u32),
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
    // Bind group layouts
    bind_group_layout: wgpu::BindGroupLayout,         // effects: input, output, params
    transition_bgl: wgpu::BindGroupLayout,             // transitions: from, to, output, params
    pieces_bgl: wgpu::BindGroupLayout,                 // posed pieces: from, to, output, params, pieces, matte
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

    fn build_from_device_queue(
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
        device_lost: Arc<AtomicBool>,
    ) -> Option<Self> {
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effects_bgl"),
            entries: &[
                // binding 0: input (read-only storage)
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 1: output (read-write storage)
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // binding 2: params (uniform)
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        // Transition bind group layout: from (read), to (read), output (write), params (uniform)
        let transition_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("transition_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let transition_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("transition_pl"),
            bind_group_layouts: &[&transition_bgl],
            push_constant_ranges: &[],
        });

        // Posed pieces (#985): the transition's four bindings and the plan.
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let pieces_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pieces_bgl"),
            entries: &[
                storage(0, true),
                storage(1, true),
                storage(2, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage(4, true),
                // The subject's matte (#985 phase 3).
                storage(5, true),
            ],
        });
        let pieces_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pieces_pl"),
            bind_group_layouts: &[&pieces_bgl],
            push_constant_ranges: &[],
        });

        // Push error handler to capture shader validation errors
        device.push_error_scope(wgpu::ErrorFilter::Validation);

        let pipeline_compile_start = std::time::Instant::now();

        let create_pipeline = |dev: &wgpu::Device, layout: &wgpu::PipelineLayout, label: &str, shader_src: &str| -> wgpu::ComputePipeline {
            let module = dev.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(shader_src.into()),
            });
            dev.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                module: &module,
                entry_point: "main",
                compilation_options: Default::default(),
                cache: None,
            })
        };

        let opacity_pipeline = create_pipeline(&device, &pipeline_layout, "opacity", SHADER_OPACITY);
        let color_fade_pipeline = create_pipeline(&device, &pipeline_layout, "color_fade", SHADER_COLOR_FADE);
        let blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "blur_h", SHADER_BLUR_H);
        let blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "blur_v", SHADER_BLUR_V);
        let sharpen_pipeline = create_pipeline(&device, &pipeline_layout, "sharpen", SHADER_SHARPEN);
        let crop_pipeline = create_pipeline(&device, &pipeline_layout, "crop", SHADER_CROP);
        let transform_pipeline = create_pipeline(&device, &pipeline_layout, "transform", SHADER_TRANSFORM);
        let corner_pin_pipeline = create_pipeline(&device, &pipeline_layout, "corner_pin", SHADER_CORNER_PIN);
        let chroma_key_pipeline = create_pipeline(&device, &pipeline_layout, "chroma_key", SHADER_CHROMA_KEY);
        let bloom_threshold_pipeline = create_pipeline(&device, &pipeline_layout, "bloom_threshold", SHADER_BLOOM_THRESHOLD);
        // Bloom/glow blend uses 4-binding layout (input, bright/blurred, output, params) — same as transitions
        let bloom_blend_pipeline = create_pipeline(&device, &transition_pl, "bloom_blend", SHADER_BLOOM_BLEND);
        let glow_blend_pipeline = create_pipeline(&device, &transition_pl, "glow_blend", SHADER_GLOW_BLEND);
        let light_rays_pipeline = create_pipeline(&device, &pipeline_layout, "light_rays", SHADER_LIGHT_RAYS);
        let vignette_pipeline = create_pipeline(&device, &pipeline_layout, "vignette", SHADER_VIGNETTE);
        let living_gradient_pipeline = create_pipeline(&device, &pipeline_layout, "living_gradient", SHADER_LIVING_GRADIENT);
        let grain_generator_pipeline = create_pipeline(&device, &pipeline_layout, "grain_generator", SHADER_GRAIN_GENERATOR);
        let segments_pipeline = create_pipeline(&device, &pipeline_layout, "segments", SHADER_SEGMENTS);
        let generator_pipeline = create_pipeline(&device, &pipeline_layout, "generator", SHADER_GENERATOR);
        let film_grain_pipeline = create_pipeline(&device, &pipeline_layout, "film_grain", SHADER_FILM_GRAIN);
        let tilt_shift_blend_pipeline = create_pipeline(&device, &transition_pl, "tilt_shift_blend", SHADER_TILT_SHIFT_BLEND);
        let focus_blur_blend_pipeline = create_pipeline(&device, &transition_pl, "focus_blur_blend", SHADER_FOCUS_BLUR_BLEND);
        let focus_blur_premultiply_pipeline = create_pipeline(&device, &pipeline_layout, "focus_blur_premultiply", SHADER_FOCUS_BLUR_PREMULTIPLY);
        let old_film_pipeline = create_pipeline(&device, &pipeline_layout, "old_film", SHADER_OLD_FILM);
        let vhs_pipeline = create_pipeline(&device, &pipeline_layout, "vhs", SHADER_VHS);
        let lens_flare_pipeline = create_pipeline(&device, &pipeline_layout, "lens_flare", SHADER_LENS_FLARE);
        let anamorphic_streak_pipeline = create_pipeline(&device, &pipeline_layout, "anamorphic_streak", SHADER_ANAMORPHIC_STREAK);
        let film_halation_blend_pipeline = create_pipeline(&device, &transition_pl, "film_halation_blend", SHADER_FILM_HALATION);
        let invert_pipeline = create_pipeline(&device, &pipeline_layout, "invert", SHADER_INVERT);
        let mosaic_pipeline = create_pipeline(&device, &pipeline_layout, "mosaic", SHADER_MOSAIC);
        let edge_detect_pipeline = create_pipeline(&device, &pipeline_layout, "edge_detect", SHADER_EDGE_DETECT);
        let channel_mixer_pipeline = create_pipeline(&device, &pipeline_layout, "channel_mixer", SHADER_CHANNEL_MIXER);
        let directional_blur_pipeline = create_pipeline(&device, &pipeline_layout, "directional_blur", SHADER_DIRECTIONAL_BLUR);
        let emboss_pipeline = create_pipeline(&device, &pipeline_layout, "emboss", SHADER_EMBOSS);
        let mirror_pipeline = create_pipeline(&device, &pipeline_layout, "mirror", SHADER_MIRROR);
        let lens_distortion_pipeline = create_pipeline(&device, &pipeline_layout, "lens_distortion", SHADER_LENS_DISTORTION);
        let displace_pipeline = create_pipeline(&device, &pipeline_layout, "displace", SHADER_DISPLACE);
        let warp_pipeline = create_pipeline(&device, &pipeline_layout, "warp", SHADER_WARP);
        let gradient_map_pipeline = create_pipeline(&device, &pipeline_layout, "gradient_map", SHADER_GRADIENT_MAP);
        let reflection_pipeline = create_pipeline(&device, &pipeline_layout, "reflection", SHADER_REFLECTION);
        let dilate_pipeline = create_pipeline(&device, &pipeline_layout, "dilate", SHADER_DILATE);
        let erode_pipeline = create_pipeline(&device, &pipeline_layout, "erode", SHADER_ERODE);
        let denoise_pipeline = create_pipeline(&device, &pipeline_layout, "denoise", SHADER_DENOISE);
        let gamma_pipeline = create_pipeline(&device, &pipeline_layout, "gamma", SHADER_GAMMA);
        let hsv_adjust_pipeline = create_pipeline(&device, &pipeline_layout, "hsv_adjust", SHADER_HSV_ADJUST);
        let noise_generator_pipeline = create_pipeline(&device, &pipeline_layout, "noise_generator", SHADER_NOISE_GENERATOR);
        let median_filter_pipeline = create_pipeline(&device, &pipeline_layout, "median_filter", SHADER_MEDIAN_FILTER);
        let radial_blur_pipeline = create_pipeline(&device, &pipeline_layout, "radial_blur", SHADER_RADIAL_BLUR);
        let levels_pipeline = create_pipeline(&device, &pipeline_layout, "levels", SHADER_LEVELS);
        let letterbox_pipeline = create_pipeline(&device, &pipeline_layout, "letterbox", SHADER_LETTERBOX);
        let chromatic_aberration_pipeline = create_pipeline(&device, &pipeline_layout, "chromatic_aberration", SHADER_CHROMATIC_ABERRATION);
        let binocular_vignette_pipeline = create_pipeline(&device, &pipeline_layout, "binocular_vignette", SHADER_BINOCULAR_VIGNETTE);
        let particles_pipeline = create_pipeline(&device, &pipeline_layout, "particles", SHADER_PARTICLES);
        let color_grade_pipeline = create_pipeline(&device, &pipeline_layout, "color_grade", SHADER_COLOR_GRADE);

        // Hue/Sat pipeline: 4 bindings (input, output, params, LUT storage)
        let huesat_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("huesat_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let huesat_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("huesat_pl"),
            bind_group_layouts: &[&huesat_bgl],
            push_constant_ranges: &[],
        });

        // ApplyMatte needs the graded frame, the frame before the
        // secondary, and the matte, all three at once — one binding
        // more than any other chain op.
        let storage = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let apply_matte_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("apply_matte_bgl"),
            entries: &[
                storage(0, true),
                storage(1, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(3, true),
                storage(4, true),
            ],
        });
        let apply_matte_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("apply_matte_pl"),
            bind_group_layouts: &[&apply_matte_bgl],
            push_constant_ranges: &[],
        });

        // Difference key: the effects layout plus the clean plate.
        //   0: input  (storage, read)  — the kept picture, linear
        //   1: output (storage, write) — the matte
        //   2: params (uniform)
        //   3: plate  (storage, read)  — the plate, already resized to
        //                                the frame on the host and
        //                                linear like the input
        let difference_key_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("difference_key_bgl"),
            entries: &[
                storage(0, true),
                storage(1, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(3, true),
            ],
        });
        let difference_key_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("difference_key_pl"),
            bind_group_layouts: &[&difference_key_bgl],
            push_constant_ranges: &[],
        });
        let difference_key_pipeline = create_pipeline(
            &device,
            &difference_key_pl,
            "difference_key",
            SHADER_DIFFERENCE_KEY,
        );

        // Temporal noise reduction (#1988): the effects layout plus the
        // two neighbour frames, linear working-space like the input.
        let temporal_denoise_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("temporal_denoise_bgl"),
            entries: &[
                storage(0, true),
                storage(1, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(3, true),
                storage(4, true),
            ],
        });
        let temporal_denoise_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("temporal_denoise_pl"),
            bind_group_layouts: &[&temporal_denoise_bgl],
            push_constant_ranges: &[],
        });
        let temporal_denoise_pipeline = create_pipeline(&device, &temporal_denoise_pl, "temporal_denoise", SHADER_TEMPORAL_DENOISE);

        // Mask v2 bind group layout — purpose-built for the
        // storage-buffer mask architecture. 4 bindings:
        //   0: output (storage, write) — the matte vec4<f32> buffer
        //   1: params (uniform) — frame constants only (width,
        //                          height, mask_count, _pad)
        //   2: records (storage, read) — array<MaskRecord> with
        //                                 per-mask shape data + bezier
        //                                 offsets into binding 3
        //   3: points  (storage, read) — pooled bezier control points
        //                                 (vec2<f32>) across all masks
        // No "input" buffer (the original mask shader reused the
        // 3-binding effects layout and ignored its input slot — we
        // drop the slot entirely here for clarity).
        let mask_v2_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("mask_v2_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false, min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let mask_v2_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mask_v2_pl"),
            bind_group_layouts: &[&mask_v2_bgl],
            push_constant_ranges: &[],
        });
        let huesat_pipeline = create_pipeline(&device, &huesat_pl, "huesat", SHADER_HUESAT);
        let lut3d_pipeline = create_pipeline(&device, &huesat_pl, "lut3d", SHADER_LUT3D);
        // Posterize reads its ladder's band edges from a table (#1443).
        let posterize_pipeline = create_pipeline(&device, &huesat_pl, "posterize", SHADER_POSTERIZE);
        let cst_pipeline = create_pipeline(&device, &huesat_pl, "colour_space_transform", SHADER_CST);
        let equalize_pipeline = create_pipeline(&device, &huesat_pl, "equalize", SHADER_EQUALIZE);
        let equalize_histogram_pipeline = create_pipeline(&device, &huesat_pl, "equalize_histogram", SHADER_EQUALIZE_HISTOGRAM);
        let equalize_cdf_pipeline = create_pipeline(&device, &huesat_pl, "equalize_cdf", SHADER_EQUALIZE_CDF);
        let rsc_pipeline = create_pipeline(&device, &huesat_pl, "rolling_shutter", SHADER_ROLLING_SHUTTER);
        let stabilization_pipeline = create_pipeline(&device, &huesat_pl, "stabilization", SHADER_STABILIZATION);
        let vibrance_pipeline = create_pipeline(&device, &pipeline_layout, "vibrance", SHADER_VIBRANCE);
        let scene_to_display_pipeline = create_pipeline(&device, &pipeline_layout, "scene_to_display", shader_scene_to_display());
        let color_warper_pipeline = create_pipeline(&device, &pipeline_layout, "color_warper", SHADER_COLOR_WARPER);
        let clarity_blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "clarity_blur_h", SHADER_CLARITY_BLUR_H);
        let clarity_blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "clarity_blur_v", SHADER_CLARITY_BLUR_V);
        let clarity_apply_pipeline = create_pipeline(&device, &huesat_pl, "clarity_apply", SHADER_CLARITY_APPLY);
        let mask_cutout_pipeline = create_pipeline(&device, &huesat_pl, "mask_cutout", SHADER_MASK_CUTOUT);
        let grade_mix_pipeline = create_pipeline(&device, &huesat_pl, "grade_mix", SHADER_GRADE_MIX);
        let apply_matte_pipeline = create_pipeline(&device, &apply_matte_pl, "apply_matte", SHADER_APPLY_MATTE_CHAIN);
        let deflicker_pipeline = create_pipeline(&device, &pipeline_layout, "deflicker", SHADER_DEFLICKER);
        let blend_mix_pipeline = create_pipeline(&device, &huesat_pl, "blend_mix", SHADER_BLEND_MIX);
        let merge_over_pipeline = create_pipeline(&device, &huesat_pl, "merge_over", SHADER_MERGE_OVER);
        let layer_over_pipeline = create_pipeline(&device, &huesat_pl, "layer_over", SHADER_LAYER_OVER);
        let mask_pipeline = create_pipeline(&device, &mask_v2_pl, "mask_v2", SHADER_MASK_V2);
        let color_balance_pipeline = create_pipeline(&device, &pipeline_layout, "color_balance", SHADER_COLOR_BALANCE);
        let hdr_zones_pipeline = create_pipeline(&device, &pipeline_layout, "hdr_zones", SHADER_HDR_ZONES);
        let wheel_sat_bri_pipeline = create_pipeline(&device, &pipeline_layout, "wheel_sat_bri", SHADER_WHEEL_SAT_BRI);
        let rgb_curves_pipeline = create_pipeline(&device, &huesat_pl, "rgb_curves", SHADER_RGB_CURVES);
        let qualifier_pipeline = create_pipeline(&device, &pipeline_layout, "qualifier", SHADER_QUALIFIER);
        let luma_key_pipeline = create_pipeline(&device, &pipeline_layout, "luma_key", SHADER_LUMA_KEY);
        let soft_clip_pipeline = create_pipeline(&device, &pipeline_layout, "soft_clip", SHADER_SOFT_CLIP);
        let grain_pipeline = create_pipeline(&device, &pipeline_layout, "grain", SHADER_GRAIN);
        let alpha_blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_blur_h", SHADER_ALPHA_BLUR_H);
        let alpha_blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_blur_v", SHADER_ALPHA_BLUR_V);
        let alpha_dilate_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_dilate", SHADER_ALPHA_DILATE);
        let title_style_pipeline = create_pipeline(&device, &pipeline_layout, "title_styles", gpu_shader!("title/title_styles.wgsl"));
        let title_metal_shot_pipeline = create_pipeline(&device, &pipeline_layout, "title_metal_shot", SHADER_TITLE_METAL_SHOT);
        let contrast_protect_pipeline = create_pipeline(&device, &pipeline_layout, "contrast_protect", contrast_protect::SHADER_CONTRAST_PROTECT);
        let surface_light_pipeline = create_pipeline(&device, &pipeline_layout, "surface_light", surface_light::SHADER_SURFACE_LIGHT);

        // LUT buffer, sized from the same constant the uploads are.
        //
        // A buffer left a curve short would not fail to compile: the
        // binding is a runtime-sized `array<f32>`. It would fail at the
        // upload instead — `queue.write_buffer` validates the write and
        // an oversized one reaches the uncaptured-error handler — so
        // the symptom is a logged validation error and a LUT that never
        // arrives, not a silent out-of-bounds read.
        let huesat_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("huesat_lut"),
            size: HUESAT_LUT_LEN as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // The equalize CDF: 256 f32. Its own buffer — see the field.
        let equalize_cdf_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("equalize_cdf"),
            size: EQUALIZE_CDF_LEN as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let equalize_hist_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("equalize_histogram"),
            size: EQUALIZE_CDF_LEN as u64 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        // RGB curves LUT buffer: 4 channels × 256 entries × 4 bytes = 4096 bytes
        let rgb_curves_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rgb_curves_lut"),
            size: 4 * 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let transition_pipeline = create_pipeline(&device, &transition_pl, "transition", SHADER_TRANSITION);
        let pieces_pipeline = create_pipeline(&device, &pieces_pl, "pieces", SHADER_PIECES);
        let shutter_pipeline = create_pipeline(&device, &transition_pl, "shutter", SHADER_SHUTTER);
        // Linear-to-u8 bridge — uses the same 3-binding effects BGL.
        // Input: array<vec4<f32>>. Output: array<u32> (packed u8 RGBA).
        // Output buffer is 4× smaller than input; readback dominates
        // wall time on this op so the smaller output is the win.
        let bridge_u8_pipeline = create_pipeline(&device, &pipeline_layout, "bridge_u8", SHADER_LINEAR_TO_U8);
        // The 4-binding layout (input, output, params, then the 8-bit
        // codes' decode table), shader flipped to read u8 and write linear.
        let bridge_u8_to_linear_pipeline = create_pipeline(
            &device, &huesat_pl, "bridge_u8_to_linear", SHADER_U8_TO_LINEAR,
        );
        let bridge_u8_fit_pipeline = create_pipeline(
            &device, &huesat_pl, "bridge_u8_fit_to_linear", SHADER_U8_FIT_TO_LINEAR,
        );
        // Each 8-bit code's linear value, decoded once here as every CPU
        // route decodes it (`srgb8_to_linear`), so a
        // frame put on the GPU as 8-bit codes is the CPU's bit for bit.
        // The shaders' `pow` only approximates it, and a step in the chain
        // (Posterize's band edge, at a code that sits on one) then went
        // one way on the GPU and the other on the CPU.
        let srgb8_lut = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("srgb8_to_linear"),
            size: 512 * 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        srgb8_lut.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(&srgb8_to_linear()));
        srgb8_lut.unmap();
        // Same again at 16 bits a sample, for the export's software-decoded
        // frames — two u32 of input per pixel instead of one — with the
        // CPU's own 16-bit decode table (512 KiB), for the same reason
        // (#1451).
        let bridge_rgba64_to_linear_pipeline = create_pipeline(
            &device, &huesat_pl, "bridge_rgba64_to_linear", SHADER_RGBA64_TO_LINEAR,
        );
        let srgb16_table = srgb16_decode_table();
        let srgb16_lut = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("srgb16_decode"),
            size: std::mem::size_of_val(srgb16_table) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        srgb16_lut.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(srgb16_table));
        srgb16_lut.unmap();
        // A display transform whose numbers are baked on the CPU — ACES'
        // matrices and tone curves, BT.2020's gamut reach tables — reads
        // them from a fourth binding. All three pipelines below share
        // this layout because each wants that same slot, and the WGSL
        // calls it `tables` rather than naming one occupant.
        let display_tables_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("display_tables_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Per-workgroup light, for a PQ delivery's MaxCLL and
                // MaxFALL. Only the Y'CbCr writer declares it; the other
                // two pipelines bind the dummy and never read it, which
                // a layout permits.
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let display_tables_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("display_tables_pl"),
            bind_group_layouts: &[&display_tables_bgl],
            push_constant_ranges: &[],
        });
        let aces_rec709_pipeline =
            create_pipeline(&device, &display_tables_pl, "aces_rec709", SHADER_ACES_REC709_U8);
        // The export's last step: linear frame in, packed Y/Cb/Cr out,
        // on the same layout so it can reach whichever tables its
        // delivery's display transform needs.
        let linear_to_yuv420_pipeline = create_pipeline(
            &device, &display_tables_pl, "linear_to_yuv420", SHADER_LINEAR_TO_YUV420,
        );
        let bt2020_display_pipeline = create_pipeline(
            &device, &display_tables_pl, "bt2020_display", SHADER_BT2020_DISPLAY_U8,
        );
        // Bound at slot 3 when the delivery's transform needs no baked
        // tables. Never read; a bind group still has to be complete.
        let no_display_tables = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("no_display_tables"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        // The same for slot 4, which measures light. A *separate* buffer
        // and not the one above: slot 3 is read-only and slot 4 writable,
        // and one buffer bound as both in a single bind group is an
        // aliasing violation — which wgpu reports as the encode simply
        // failing, so it looks like "this frame cannot take the GPU path".
        let no_light = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("no_light"),
            size: 4,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        // Both HDR kernels share the colourspace prelude with the P010
        // decoder, so the PQ/HLG curves on either side are one definition.
        let bridge_hdr10_pipeline = create_pipeline(
            &device, &pipeline_layout, "bridge_hdr10_to_linear", shader_hdr10_to_linear(),
        );
        let bridge_hdr16_pipeline = create_pipeline(
            &device, &pipeline_layout, "bridge_hdr16_to_linear", shader_hdr16_to_linear(),
        );
        let display_u8_pipeline = create_pipeline(
            &device, &pipeline_layout, "linear_to_display_u8", SHADER_LINEAR_TO_DISPLAY_U8,
        );
        let u8_to_texture_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("u8_to_texture_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let u8_to_texture_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("u8_to_texture_pl"),
            bind_group_layouts: &[&u8_to_texture_bgl],
            push_constant_ranges: &[],
        });
        let u8_to_texture_pipeline = create_pipeline(&device, &u8_to_texture_pl, "u8_to_texture", SHADER_U8_TO_TEXTURE);
        let blend_layer_pipeline = create_pipeline(&device, &pipeline_layout, "blend_layer", SHADER_BLEND_LAYER);
        let premultiply_pipeline = create_pipeline(&device, &pipeline_layout, "premultiply", SHADER_PREMULTIPLY);
        let view_channel_pipeline = create_pipeline(&device, &pipeline_layout, "view_channel", SHADER_VIEW_CHANNEL);
        let alpha_form_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_form", SHADER_ALPHA_FORM);
        let sprite_rows_pipeline = create_pipeline(&device, &pipeline_layout, "sprite_rows", SHADER_SPRITE_ROWS);
        let sprite_rows_f16_pipeline = create_pipeline(&device, &pipeline_layout, "sprite_rows_f16", SHADER_SPRITE_ROWS_F16);
        let sprite_box_f16_pipeline = create_pipeline(&device, &pipeline_layout, "sprite_box_f16", SHADER_SPRITE_BOX_F16);
        let sprite_place_pipeline = create_pipeline(&device, &pipeline_layout, "sprite_place", SHADER_SPRITE_PLACE);
        let storage_entry = |binding: u32, read_only: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let sprite_glyphs_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("sprite_glyphs_bgl"),
            entries: &[
                storage_entry(0, true),
                storage_entry(1, false),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage_entry(3, true),
                storage_entry(4, true),
            ],
        });
        let sprite_glyphs_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sprite_glyphs_pl"),
            bind_group_layouts: &[&sprite_glyphs_bgl],
            push_constant_ranges: &[],
        });
        let sprite_glyphs_pipeline = create_pipeline(&device, &sprite_glyphs_pl, "sprite_glyphs", SHADER_SPRITE_GLYPHS);
        let matte_extract_pipeline = create_pipeline(&device, &pipeline_layout, "matte_extract", SHADER_MATTE_EXTRACT);
        let mask_min_h_pipeline = create_pipeline(&device, &pipeline_layout, "mask_min_h", SHADER_MASK_MIN_H);
        let mask_min_v_pipeline = create_pipeline(&device, &pipeline_layout, "mask_min_v", SHADER_MASK_MIN_V);
        let matte_apply_pipeline = create_pipeline(&device, &pipeline_layout, "matte_apply", SHADER_MATTE_APPLY);
        let scale_alpha_pipeline = create_pipeline(&device, &pipeline_layout, "scale_alpha", SHADER_SCALE_ALPHA);
        // Same three-binding shape as every effect — in, out, params —
        // so it needs no layout of its own; its `out` is f32 rather than
        // packed bytes, which a bind group layout does not distinguish.
        let hdr_nit_map_pipeline = create_pipeline(
            &device, &pipeline_layout, "hdr_nit_map", SHADER_HDR_NIT_MAP,
        );

        let mask_mix_gpu = mask_mix::MaskMixGpu::new(&device);
        let layer_gpu = layer::LayerGpu::new(&device, &pipeline_layout, &transition_pl);
        let pipeline_count = 68;
        info!(
            "GPU effects: compiled {} pipelines in {:.0}ms",
            pipeline_count,
            pipeline_compile_start.elapsed().as_secs_f64() * 1000.0,
        );

        // Check if any shader compilation failed
        if let Some(err) = pollster::block_on(device.pop_error_scope()) {
            crate::shader_check::shader_failed("effects", &err.to_string());
            return None;
        }

        let params_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("params"),
            size: PARAMS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Ring of per-dispatch params buffers for the chain path. Each
        // chain dispatch claims its own slot so the params write lands
        // in a buffer no later dispatch in the same submit reads from.
        let stack_params_ring: Vec<wgpu::Buffer> = (0..STACK_PARAMS_RING_SIZE)
            .map(|i| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("stack_params_{i}")),
                    size: PARAMS_SIZE,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let chain_params = ParamSlots::new(&device, CHAIN_PARAMS_RING_SIZE, PARAMS_SIZE, "chain_params");
        let effect_params = Some(ParamSlots::new(&device, EFFECT_PARAMS_RING_SIZE, PARAMS_SIZE, "effect_params"));

        info!("GPU effects pipeline initialized");

        Some(Self {
            readbacks: readbacks::Counters::default(),
            layer_gpu,
            mask_mix_gpu,
            flow_gpu: None,
            device,
            queue,
            opacity_pipeline,
            color_fade_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            sharpen_pipeline,
            crop_pipeline,
            transform_pipeline,
            corner_pin_pipeline,
            chroma_key_pipeline,
            bloom_threshold_pipeline,
            bloom_blend_pipeline,
            glow_blend_pipeline,
            light_rays_pipeline,
            vignette_pipeline,
            generator_pipeline,
            living_gradient_pipeline,
            grain_generator_pipeline,
            segments_pipeline,
            film_grain_pipeline,
            tilt_shift_blend_pipeline,
            focus_blur_blend_pipeline,
            focus_blur_premultiply_pipeline,
            old_film_pipeline,
            vhs_pipeline,
            lens_flare_pipeline,
            anamorphic_streak_pipeline,
            film_halation_blend_pipeline,
            invert_pipeline,
            posterize_pipeline,
            cst_pipeline,
            effect_luts: std::cell::RefCell::new(Vec::new()),
            mosaic_pipeline,
            edge_detect_pipeline,
            channel_mixer_pipeline,
            directional_blur_pipeline,
            emboss_pipeline,
            mirror_pipeline,
            lens_distortion_pipeline,
            displace_pipeline,
            warp_pipeline,
            gradient_map_pipeline,
            reflection_pipeline,
            dilate_pipeline,
            erode_pipeline,
            denoise_pipeline,
            gamma_pipeline,
            hsv_adjust_pipeline,
            noise_generator_pipeline,
            median_filter_pipeline,
            radial_blur_pipeline,
            levels_pipeline,
            equalize_pipeline,
            equalize_histogram_pipeline,
            equalize_cdf_pipeline,
            letterbox_pipeline,
            chromatic_aberration_pipeline,
            binocular_vignette_pipeline,
            particles_pipeline,
            rsc_pipeline,
            rsc_row_buf: None,
            rsc_row_buf_size: 0,
            stabilization_pipeline,
            stabilization_buf: None,
            stabilization_buf_size: 0,
            color_grade_pipeline,
            huesat_pipeline,
            huesat_bgl,
            apply_matte_bgl,
            huesat_lut_buf,
            equalize_cdf_buf,
            equalize_hist_buf,
            lut3d_pipeline,
            lut3d_buf: None,
            lut3d_buf_size: 0,
            vibrance_pipeline,
            scene_to_display_pipeline,
            color_warper_pipeline,
            clarity_blur_h_pipeline,
            clarity_blur_v_pipeline,
            clarity_apply_pipeline,
            mask_cutout_pipeline,
            grade_mix_pipeline,
            apply_matte_pipeline,
            deflicker_pipeline,
            blend_mix_pipeline,
            merge_over_pipeline,
            layer_over_pipeline,
            grade_mix_src: None,
            mask_pipeline,
            color_balance_pipeline,
            hdr_zones_pipeline,
            wheel_sat_bri_pipeline,
            rgb_curves_pipeline,
            rgb_curves_lut_buf,
            qualifier_pipeline,
            luma_key_pipeline,
            difference_key_pipeline,
            difference_key_bgl,
            plate_buf: None,
            plate_key: None,
            plate_read_in_chain: false,
            temporal_denoise_pipeline,
            temporal_denoise_bgl,
            soft_clip_pipeline,
            grain_pipeline,
            alpha_blur_h_pipeline,
            alpha_blur_v_pipeline,
            alpha_dilate_pipeline,
            title_style_pipeline,
            title_style_scratch: None,
            title_metal_shots: std::collections::HashMap::new(),
            title_metal_shot_pipeline,
            title_metal_shot_scratch: None,
            contrast_protect_pipeline,
            contrast_protect_scratch: None,
            surface_light_pipeline,
            surface_light_scratch: None,
            title_post_scratch: None,
            title_scene_bounds: None,
            title_outlines: Vec::new(),
            transition_pipeline,
            pieces_pipeline,
            shutter_pipeline,
            bridge_u8_pipeline,
            bridge_u8_to_linear_pipeline,
            bridge_u8_fit_pipeline,
            srgb8_lut,
            bridge_rgba64_to_linear_pipeline,
            srgb16_lut,
            linear_to_yuv420_pipeline,
            aces_rec709_pipeline,
            display_tables_bgl,
            aces_rec709_data: None,
            no_display_tables,
            no_light,
            bt2020_gamut_data: None,
            hdr_encode_data: None,
            bt2020_display_pipeline,
            bridge_hdr10_pipeline,
            bridge_hdr16_pipeline,
            display_u8_pipeline,
            hdr_nit_map_pipeline,
            hdr_nit_buf: None,
            hdr_nit_staging: None,
            display_u8_staging: None,
            stack_layers: Vec::new(),
            stack_title_slot: 0,
            stack_acc: None,
            stack_sprite_u8: None,
            stack_sprite_f16: None,
            stack_rects: None,
            stack_sprites: Vec::new(),
            stack_sprite_tick: 0,
            sprite_place_pipeline,
            sprite_glyphs_bgl,
            sprite_glyphs_pipeline,
            sprite_glyph_table: None,
            stack_buf_size: 0,
            stack_acc_touched: false,
            stack_dims: (0, 0),
            blend_layer_pipeline,
            premultiply_pipeline,
            view_channel_pipeline,
            alpha_form_pipeline,
            sprite_rows_pipeline,
            sprite_rows_f16_pipeline,
            sprite_box_f16_pipeline,
            matte_extract_pipeline,
            mask_min_h_pipeline,
            mask_min_v_pipeline,
            matte_apply_pipeline,
            scale_alpha_pipeline,
            u8_to_texture_pipeline,
            u8_to_texture_bgl,
            bridge_u8_out_buf: None,
            bridge_u8_out_size: 0,
            bridge_u8_staging_buf: None,
            bridge_u8_staging_size: 0,
            bind_group_layout,
            transition_bgl,
            pieces_bgl,
            buf_a: None,
            buf_b: None,
            buf_c: None,
            shutter_sum: None,
            matte_buf: None,
            mask_records_buf: None,
            mask_points_buf: None,
            staging_read: None,
            params_buf,
            chain_params,
            effect_params,
            effects_incomplete: std::cell::Cell::new(false),
            slot_pool: SlotPool::new(Self::MAX_SLOTS),
            refuse_recording: false,
            // Start empty; populated by `rebuild_chain_bind_group_pools`
            // after the first `ensure_buffers` call (buf_a/b/c don't
            // exist until then).
            chain_bg_3binding: (0..CHAIN_PARAMS_RING_SIZE * 4).map(|_| None).collect(),
            chain_bg_huesat: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_rgb_curves: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_clarity_apply: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_mask_cutout: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_build_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            chain_encoder: None,
            current_buf_size: 0,
            device_lost,
            submit_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            frame_upload_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            batch: std::cell::RefCell::new(None),
            batching: std::cell::Cell::new(false),
            stack_params_ring,
            stack_params_slot: std::cell::Cell::new(0),
            scopes: None,
            luma_coefs: [0.2126, 0.7152, 0.0722],
            authored_to_working: [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]],
            grade_encoding: 0,
        })
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
                    layout: &self.bind_group_layout,
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
                    layout: &self.huesat_bgl,
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
                    layout: &self.huesat_bgl,
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
                    layout: &self.huesat_bgl,
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
                    layout: &self.huesat_bgl,
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
            layout: &self.bind_group_layout,
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

    /// Pick (input, output) buffer refs from the current ping-pong slot.
    fn chain_in_out(&self, in_is_a: bool) -> (&wgpu::Buffer, &wgpu::Buffer) {
        let buf_a = self.buf_a.as_ref().expect("ensure_chain_buffers must be called first");
        let buf_b = self.buf_b.as_ref().expect("ensure_chain_buffers must be called first");
        if in_is_a { (buf_a, buf_b) } else { (buf_b, buf_a) }
    }

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

    /// Start a new chain segment. Resets the slot ring and opens a fresh
    /// command encoder. If an encoder is already open (should not happen
    /// — executor pairs begin/submit in lock-step) we submit+drop it
    /// first so no recorded work is lost.
    pub fn begin_chain(&mut self) {
        if let Some(enc) = self.chain_encoder.take() {
            // Recover from a missed submit_chain. Should be unreachable
            // in well-formed callers.
            self.submit(enc);
        }
        self.chain_params.reset();
        self.plate_read_in_chain = false;
        // A stack frame's passes so far go first: the chain reads what
        // they wrote, and its own ring starts over.
        self.flush();
        let enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("chain_encoder") });
        self.chain_encoder = Some(enc);
    }

    /// Finalise the current chain segment: submit the batched encoder if
    /// one is live. No-op when called without a prior `begin_chain`
    /// (covers short-circuit / device-lost paths).
    pub fn submit_chain(&mut self) {
        if let Some(enc) = self.chain_encoder.take() {
            self.submit(enc);
            self.plate_read_in_chain = false;
            // Effects recorded into the chain (`chain_run_effects`) read
            // their uniforms from these slots; submitted, they are free.
            if let Some(slots) = self.effect_params.as_mut() {
                slots.reset();
            }
            // A slot written from the CPU was held for this encoder's readers.
            self.slot_pool.submitted();
            // Non-blocking nudge to the driver to reclaim deferred
            // resources (command buffers, staging allocations) between
            // chain segments. On NVIDIA Vulkan these otherwise
            // accumulate across long sessions — paired with the
            // pre-allocated bind group pools above, this is the second
            // belt against the observed `device lost (Dropped)` crash.
            // `Maintain::Poll` returns immediately — it's a hint, not a
            // fence.
            self.device.poll(wgpu::Maintain::Poll);
        }
    }

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

    /// Claim the next ring slot for a chain dispatch. Panics if the
    /// ring is exhausted — a 32-slot ring covers every realistic grade
    /// (full legacy chain is 13 nodes; multi-pass nodes claim 1 slot
    /// per pass). If this ever fires in the wild, bump
    /// `CHAIN_PARAMS_RING_SIZE` rather than masking with wraparound.
    fn next_params_slot(&mut self) -> usize {
        self.chain_params.claim_index().unwrap_or_else(|| {
            panic!("chain params ring exhausted ({CHAIN_PARAMS_RING_SIZE} slots); bump CHAIN_PARAMS_RING_SIZE")
        })
    }

    /// Write `data` to the params-ring slot at `slot_idx`. Paired with
    /// `next_params_slot` by every `*_chain` entry.
    fn write_chain_params<T: bytemuck::Pod>(&self, slot_idx: usize, data: &T) {
        let bytes = bytemuck::bytes_of(data);
        self.queue.write_buffer(self.chain_params.buffer(slot_idx), 0, bytes);
    }

    /// Record a single compute dispatch into the supplied encoder.
    /// Pulled out so each `*_chain` entry doesn't duplicate the
    /// `begin_compute_pass` boilerplate. Takes the encoder separately
    /// from `&self` so callers can hold an immutable borrow on other
    /// fields (pipelines, LUT buffers) while recording.
    fn record_compute_dispatch(
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::ComputePipeline,
        bind_group: &wgpu::BindGroup,
        workgroups: u32,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: None,
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        dispatch_flat(&mut pass, workgroups);
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

    /// Buffer-pair configuration indexing for the pre-allocated chain
    /// bind group pools. The 3-binding pool stores all four variants
    /// (AtoB=0, BtoA=1, AtoC=2, BtoC=3); the 4-binding pools only need
    /// AtoB=0 / BtoA=1.
    #[inline]
    fn chain_cfg_3b_ab(in_is_a: bool) -> usize { if in_is_a { 0 } else { 1 } }
    #[inline]
    fn chain_cfg_3b_ac(in_is_a: bool) -> usize { if in_is_a { 2 } else { 3 } }
    #[inline]
    fn chain_cfg_4b(in_is_a: bool) -> usize { if in_is_a { 0 } else { 1 } }


    /// A slowed clip's in-between frame on the chain by optical flow
    /// (#2854): from the chain's picture, or `from` in its place, toward
    /// `toward` (linear RGBA,
    /// `width` × `height`, the media's next frame on the GPU) at `t`, the
    /// pair `key` names estimated once and kept. `None` when it couldn't
    /// be recorded; else the share of the pair that couldn't be followed,
    /// once it is known (the draw cross-fades such a frame itself).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_flow_chain(&mut self, in_is_a: bool, width: u32, height: u32, from: Option<&wgpu::Buffer>, toward: &wgpu::Buffer, key: FlowKey, t: f32, luma: [f32; 3]) -> Option<Option<f32>> {
        let (a, b) = (self.buf_a.take(), self.buf_b.take());
        let made = match (&a, &b) {
            (Some(a), Some(b)) if in_is_a => self.record_flow_on_chain(key, from.unwrap_or(a), toward, b, width, height, t, luma),
            (Some(a), Some(b)) => self.record_flow_on_chain(key, from.unwrap_or(b), toward, a, width, height, t, luma),
            _ => None,
        };
        (self.buf_a, self.buf_b) = (a, b);
        made
    }

    // ─── Chain plumbing helpers ────────────────────────────────────────────

    /// Resize `buf_a` / `buf_b` / `buf_c` to hold at least `byte_size` bytes.
    /// Public entry point for the node executor's GPU-resident chain path.
    pub fn ensure_chain_buffers(&mut self, byte_size: u64) {
        self.ensure_buffers(byte_size);
    }

    /// Upload a linear-f32 RGBA frame into `buf_a`. Used by the node
    /// executor to seed the ping-pong chain.
    pub fn upload_chain_input_linear(&mut self, linear_rgba: &[f32]) {
        if self.is_device_lost() {
            return;
        }
        let buf_a = self.buf_a.as_ref().expect("ensure_chain_buffers must be called first");
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
    }

    /// Download the final linear-f32 RGBA frame. `slot_a` selects `buf_a`
    /// when true, `buf_b` when false — the caller's ping-pong parity.
    /// Read `matte_buf` back to the CPU.
    ///
    /// The slow way to get at a matte, and only correct once the
    /// dispatch that wrote it has been submitted — after a chain flush,
    /// in other words. It exists so a matte left in GPU memory by a node
    /// whose consumer then fell off the chain can still be blended
    /// correctly: the alternative is a silently un-applied secondary,
    /// which is a wrong picture rather than a slow one.
    pub fn read_matte_buf_linear(&self, pixel_count: usize) -> Vec<f32> {
        if self.is_device_lost() {
            return Vec::new();
        }
        match self.matte_buf.as_ref() {
            Some(buf) => self.download_linear_rgba_f32(buf, pixel_count),
            None => Vec::new(),
        }
    }

    pub fn download_chain_output_linear(&self, slot_a: bool, pixel_count: usize) -> Vec<f32> {
        let buf = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
        let buf = buf.expect("ensure_chain_buffers must be called first");
        self.download_linear_rgba_f32(buf, pixel_count)
    }

    /// Take ownership of the open chain encoder (if any) and finish the
    /// chain by appending a readback copy + map_async + sync fence in
    /// the SAME submit. Halves the driver-roundtrip latency vs the
    /// two-submit pattern (`submit_chain()` then `download_*`).
    ///
    /// On NVIDIA Vulkan each `queue.submit` costs 5-15 ms of driver
    /// overhead beyond the actual GPU work. Merging the submits is a
    /// pure win — the chain dispatch was always going to fence on the
    /// readback's `device.poll(Wait)` anyway.
    pub fn flush_chain_and_download_linear(&mut self, slot_a: bool, pixel_count: usize) -> Vec<f32> {
        if self.is_device_lost() {
            return vec![0.0f32; pixel_count * 4];
        }
        let byte_size = (pixel_count as u64) * 16;
        let staging = self.staging_read.as_ref().expect("staging buffer must exist");
        let source_buf = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
        let source_buf = source_buf.expect("chain buffers must be initialized");

        // Pull the chain encoder if one is open; otherwise create a
        // fresh encoder for the readback copy.
        let mut encoder = self.chain_encoder.take().unwrap_or_else(|| {
            self.encoder(Some("flush_chain_dl"))
        });
        let mut readback = self.readbacks.begin(byte_size);
        encoder.copy_buffer_to_buffer(source_buf, 0, staging, 0, byte_size);
        self.submit(encoder);

        let t_submit = std::time::Instant::now();
        let slice = staging.slice(..byte_size);
        let (tx, rx) = std::sync::mpsc::channel();
        self.flush();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });

        mark_gpu_stage(GpuStage::FlushChain);
        let result = match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let t_mapped = t_submit.elapsed();
                let data = slice.get_mapped_range();
                let result = mapped_bytes_to_f32(&data, pixel_count * 4);
                tracing::debug!(
                    "chain flush {} MB: gpu+transfer {:.1}ms, copy out {:.1}ms",
                    byte_size / 1_000_000,
                    t_mapped.as_secs_f64() * 1000.0,
                    (t_submit.elapsed() - t_mapped).as_secs_f64() * 1000.0,
                );
                drop(data);
                staging.unmap();
                readback.succeeded = true;
                result
            }
            other => {
                self.mark_device_lost(&format!("flush_chain map_async failed: {other:?}"));
                vec![0.0f32; pixel_count * 4]
            }
        };
        mark_gpu_stage(GpuStage::Idle);
        result
    }

    /// Borrow the chain buffers by their ping-pong slot. `slot_a` true
    /// returns `buf_a`; false returns `buf_b`. Intended for callers that
    /// want to pass the buffer refs to `*_chain` dispatch entries.
    #[allow(dead_code)]
    pub fn chain_buffer(&self, slot_a: bool) -> &wgpu::Buffer {
        if slot_a {
            self.buf_a.as_ref().expect("ensure_chain_buffers must be called first")
        } else {
            self.buf_b.as_ref().expect("ensure_chain_buffers must be called first")
        }
    }

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

    /// Ready the stack for a frame of `width` × `height` with room for
    /// `layers` layers. Buffers grow to the largest request and stay.
    pub fn stack_begin(&mut self, width: u32, height: u32, layers: usize) -> bool {
        // Whatever the last frame left goes to the queue; this frame's
        // passes record into one encoder from here.
        self.stack_end();
        self.title_metal_shots.clear();
        self.batching.set(true);
        self.stack_params_slot.set(0);
        if self.is_device_lost() || width == 0 || height == 0 {
            return false;
        }
        let byte_size = (width as u64) * (height as u64) * 16;
        if byte_size > self.stack_buf_size {
            self.stack_layers.clear();
            self.stack_acc = None;
            self.stack_buf_size = byte_size;
        }
        // Every buffer in the pool is the pool's size, never this frame's:
        // a layer first wanted by a smaller frame (a 2128-row clip after a
        // 2132-row one) would otherwise be short for the next larger
        // frame, whose copies then overrun it — a validation error that
        // took the GPU away for the rest of the session.
        let pool_size = self.stack_buf_size;
        let make = |device: &wgpu::Device, label: &str| {
            std::sync::Arc::new(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: pool_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }))
        };
        while self.stack_layers.len() < layers {
            let label = format!("stack_layer_{}", self.stack_layers.len());
            self.stack_layers.push(make(&self.device, &label));
        }
        if self.stack_acc.is_none() {
            self.stack_acc = Some(make(&self.device, "stack_acc"));
        }
        // The chain buffers must exist at this size too: layers copy out
        // of them, effects run in them, the finish copies into them.
        self.ensure_buffers(byte_size);
        if self.stack_dims != (width, height) {
            self.title_style_scratch = None;
            self.title_post_scratch = None;
            self.title_scene_bounds = None;
            self.stack_sprites.clear();
        }
        self.stack_title_slot = layers;
        self.stack_dims = (width, height);
        self.stack_acc_touched = false;
        true
    }

    fn stack_pixel_count(&self) -> u64 {
        (self.stack_dims.0 as u64) * (self.stack_dims.1 as u64)
    }

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

    /// A stack frame is over: its passes go to the queue and passes
    /// submit themselves again until the next [`Self::stack_begin`].
    pub fn stack_end(&self) {
        self.flush();
        self.batching.set(false);
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

    /// One compute pass over `workgroups`, bound as (read, read_write,
    /// params) on the effects layout.
    fn stack_pass(&self, pipeline: &wgpu::ComputePipeline, read: &wgpu::Buffer, rw: &wgpu::Buffer, workgroups: u32, label: &str) {
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: read.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: rw.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let mut enc = self.encoder(Some(label));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg, &[]);
            dispatch_flat(&mut pass, workgroups);
        }
        self.submit(enc);
    }

    /// A whole-frame copy between two of the stack's buffers. `false`,
    /// and nothing recorded, when either is too small for the frame:
    /// an overrunning copy is a validation error, and the frame is
    /// better declined than the device marked bad.
    fn stack_copy(&self, from: &wgpu::Buffer, to: &wgpu::Buffer, label: &str) -> bool {
        let bytes = self.stack_pixel_count() * 16;
        if from.size() < bytes || to.size() < bytes {
            error!(
                "GPU stack: {label} needs {bytes} bytes but has {} to copy from and {} to copy into; the frame is declined",
                from.size(),
                to.size()
            );
            return false;
        }
        let mut enc = self.encoder(Some(label));
        enc.copy_buffer_to_buffer(from, 0, to, 0, bytes);
        self.submit(enc);
        true
    }

    /// The accumulator, a canvas if nothing has been laid on it yet,
    /// for a chain to seed from (an adjustment layer).
    pub fn stack_acc_buffer(&mut self) -> Option<std::sync::Arc<wgpu::Buffer>> {
        if !self.stack_ensure_acc() {
            return None;
        }
        self.flush();
        self.stack_acc.clone()
    }

    /// Layer `i` cleared to transparent black.
    pub fn stack_layer_clear(&mut self, i: usize) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let mut enc = self.encoder(Some("stack_layer_clear"));
        enc.clear_buffer(&self.stack_layers[i], 0, Some(self.stack_pixel_count() * 16));
        self.submit(enc);
        true
    }

    /// Layer `dst` becomes a copy of layer `src`, every pixel.
    pub fn stack_layer_copy(&mut self, src: usize, dst: usize) -> bool {
        if self.is_device_lost() || src >= self.stack_layers.len() || dst >= self.stack_layers.len() || src == dst {
            return false;
        }
        let mut enc = self.encoder(Some("stack_layer_copy"));
        enc.copy_buffer_to_buffer(&self.stack_layers[src], 0, &self.stack_layers[dst], 0, self.stack_pixel_count() * 16);
        self.submit(enc);
        true
    }

    /// Layer `i`'s buffer, for a chain to seed from: a layer whose effects
    /// run as a plan of its own after it was placed (a title's sprite, a
    /// generator; phase 2b M2 step 11d), its result copied back with
    /// [`Self::stack_layer_from_chain`].
    pub fn stack_layer_buffer(&self, i: usize) -> Option<std::sync::Arc<wgpu::Buffer>> {
        self.stack_layers.get(i).cloned()
    }

    /// Layer `i` takes the chain's output slot (a grade's result, or a
    /// seeded frame), as an on-GPU copy.
    pub fn stack_layer_from_chain(&mut self, i: usize, slot_a: bool) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let src = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
        let Some(src) = src else { return false };
        self.stack_copy(src, &self.stack_layers[i], "stack_layer_from_chain")
    }

    /// A linear frame sent up once and kept on the GPU (#2092): a still
    /// image's picture, seeded from on later frames instead of uploaded
    /// again. `None` when the device is lost.
    pub fn kept_linear(&mut self, linear_rgba: &[f32]) -> Option<std::sync::Arc<wgpu::Buffer>> {
        if self.is_device_lost() || linear_rgba.is_empty() {
            return None;
        }
        let kept = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kept_still"),
            size: std::mem::size_of_val(linear_rgba) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.upload(&kept, 0, bytemuck::cast_slice(linear_rgba));
        Some(std::sync::Arc::new(kept))
    }

    /// Layer `i` takes a linear working-space frame from the CPU.
    pub fn stack_layer_from_linear(&mut self, i: usize, linear_rgba: &[f32]) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        if linear_rgba.len() as u64 != self.stack_pixel_count() * 4 {
            return false;
        }
        self.upload(&self.stack_layers[i], 0, bytemuck::cast_slice(linear_rgba));
        true
    }

    /// The frame's canvas, clear where nothing has been laid on it: an
    /// empty frame (a gap, a strobe's dark pieces) is this canvas, graded
    /// and finished as any composite is (#2377).
    pub fn stack_clear_canvas(&mut self) -> bool {
        !self.is_device_lost() && self.stack_ensure_acc()
    }

    fn stack_ensure_acc(&mut self) -> bool {
        let Some(acc) = self.stack_acc.clone() else { return false };
        if !self.stack_acc_touched {
            let mut enc = self.encoder(Some("stack_clear"));
            enc.clear_buffer(&acc, 0, Some(self.stack_pixel_count() * 16));
            self.submit(enc);
            self.stack_acc_touched = true;
        }
        true
    }

    /// The accumulator into chain slot A, for an adjustment layer's
    /// chain to run on.
    pub fn stack_acc_to_chain(&mut self) -> bool {
        if !self.stack_ensure_acc() {
            return false;
        }
        let acc = self.stack_acc.clone().unwrap();
        self.stack_copy(&acc, self.buf_a.as_ref().unwrap(), "stack_acc_to_chain")
    }

    /// Chain slot B into slot A, so an effects run can start from A.
    pub fn stack_chain_to_slot_a(&mut self, slot_a: bool) {
        if !slot_a {
            self.stack_copy(self.buf_b.as_ref().unwrap(), self.buf_a.as_ref().unwrap(), "stack_chain_b_to_a");
        }
    }

    /// Mix the chain's slot back into the accumulator: acc = acc + (chain
    /// - acc) × opacity, every channel — an adjustment layer's opacity.
    pub fn stack_mix_from_chain(&mut self, slot_a: bool, opacity: f32) -> bool {
        if !self.stack_ensure_acc() {
            return false;
        }
        let (width, height) = self.stack_dims;
        self.write_params(&BlendLayerParams { width, height, opacity, mode: 13, y0: 0, y1: height, _p0: 0, _p1: 0 });
        let src = if slot_a { self.buf_a.as_ref().unwrap() } else { self.buf_b.as_ref().unwrap() };
        let acc = self.stack_acc.clone().unwrap();
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        self.stack_pass(&self.blend_layer_pipeline, src, &acc, wg, "stack_mix");
        true
    }

    /// Close the stack: the accumulator (premultiplied unless
    /// `keep_alpha`) moves into chain slot A, where every encode and
    /// the LUT can take it. Returns false when nothing was blended.
    pub fn stack_finish_into_chain(&mut self, keep_alpha: bool) -> bool {
        if self.is_device_lost() || !self.stack_acc_touched {
            return false;
        }
        let (width, height) = self.stack_dims;
        let acc = self.stack_acc.clone().unwrap();
        if keep_alpha {
            return self.stack_copy(&acc, self.buf_a.as_ref().unwrap(), "stack_finish_copy");
        }
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        self.stack_pass(&self.premultiply_pipeline, &acc, self.buf_a.as_ref().unwrap(), wg, "stack_premultiply");
        true
    }

    /// The stack finished for the viewer's channel view (#2758):
    /// `stack_finish_into_chain(false)` with `channel` (as
    /// `composite::view_channel` takes it) shown in grey.
    pub fn stack_finish_viewing(&mut self, channel: u32, luma: [f32; 3]) -> bool {
        if self.is_device_lost() || !self.stack_acc_touched {
            return false;
        }
        let (width, height) = self.stack_dims;
        let acc = self.stack_acc.clone().unwrap();
        self.write_params(&ViewChannelParams { width, height, channel, _pad0: 0, luma: [luma[0], luma[1], luma[2], 0.0] });
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        self.stack_pass(&self.view_channel_pipeline, &acc, self.buf_a.as_ref().unwrap(), wg, "stack_view_channel");
        true
    }

    /// The finished stack (`stack_finish_into_chain` with `keep_alpha`)
    /// kept in a buffer of its own (#2757): a precomposition's scene,
    /// composed before its parent's stack, which seeds the parent's layer
    /// from it. `reuse` is the buffer kept for the clip last frame, taken
    /// again when it is the frame's size. Recorded in the frame's encoder,
    /// so it lands before the next stack's passes.
    pub fn stack_keep_chain(&mut self, reuse: Option<std::sync::Arc<wgpu::Buffer>>) -> Option<std::sync::Arc<wgpu::Buffer>> {
        if self.is_device_lost() {
            return None;
        }
        let bytes = self.stack_pixel_count() * 16;
        let kept = match reuse.filter(|b| b.size() == bytes) {
            Some(b) => b,
            None => std::sync::Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("kept_scene"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            })),
        };
        self.stack_copy(self.buf_a.as_ref()?, &kept, "stack_keep_scene").then_some(kept)
    }

    /// The finished stack as linear working-space pixels, for tests and
    /// the CPU twin comparison. Call after `stack_finish_into_chain`.
    pub fn stack_download_linear(&self) -> Vec<f32> {
        self.download_chain_output_linear(true, self.stack_pixel_count() as usize)
    }

    /// Apply a chain of effects on the GPU and read the picture back: the
    /// effects as the plan route runs them ([`Self::chain_run_effects`]:
    /// one encoder, a pass of their own only where one isn't recorded),
    /// with an upload before and a download after. Kept for the CPU/GPU
    /// parity tests and examples, which compare the GPU's effects with the
    /// CPU kernels' on plain frames; the renderer runs effects as plan
    /// nodes (phase 2b M2, step 11d). An effect left out (a lost device)
    /// gives the CPU kernels' picture instead.
    #[doc(hidden)]
    pub fn apply_effects(
        &mut self,
        effects: &[BuiltinEffect],
        linear_rgba: &[f32],
        width: u32,
        height: u32,
    ) -> Vec<f32> {
        if self.is_device_lost() || effects.is_empty() {
            return linear_rgba.to_vec();
        }
        self.ensure_recording_buffers(width, height);
        self.upload(self.buf_a.as_ref().unwrap(), 0, bytemuck::cast_slice(linear_rgba));
        self.begin_chain();
        let run = self.chain_run_effects(effects, true, width, height);
        self.submit_chain();
        if !run.complete {
            return crate::apply_effects(effects, linear_rgba.to_vec(), width, height);
        }
        let final_buf = if run.in_a { self.buf_a.as_ref().unwrap() } else { self.buf_b.as_ref().unwrap() };
        self.download_linear_rgba_f32(final_buf, (width * height) as usize)
    }

    /// One break of a chain ([`Self::chain_run_effects`]): an effect, or a
    /// run of spills, the chain couldn't record, run in passes of its own
    /// on the chain buffers, on the frame in slot A (`in_a`) or B. Returns
    /// whether the result sits in slot A. Equalize comes here only when
    /// recording is refused (#1215), and builds its histogram on the CPU;
    /// a run of spills too long for the slots runs unrecorded. Any other
    /// effect is only ever recorded (the chain makes the layer passes'
    /// buffers before it records), so one that couldn't be is missing from
    /// the picture, and [`Self::last_effects_complete`] says so.
    fn run_break_pass(&mut self, part: &[BuiltinEffect], in_a: bool, width: u32, height: u32) -> bool {
        // A lost device runs none of them, so every effect is missing;
        // set before the early return, or a caller would read the last
        // run's answer.
        self.effects_incomplete.set(self.is_device_lost() && !part.is_empty());
        if self.is_device_lost() || part.is_empty() {
            return in_a;
        }
        self.ensure_recording_buffers(width, height);
        let byte_size = u64::from(width) * u64::from(height) * 16;
        let (buf_a, buf_b) = (self.buf_a.as_ref().unwrap(), self.buf_b.as_ref().unwrap());
        let (input, output) = if in_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
        let current_in_a = in_a;
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        if crate::layer::Spill::of(&part[0]).is_some() {
            let spills: Vec<_> = part.iter().filter_map(crate::layer::Spill::of).collect();
            return if self.layer_spills(input, output, width, height, &spills) { !in_a } else { in_a };
        }
        match &part[0] {
                BuiltinEffect::Equalize { strength } => {
                    // Hybrid GPU: build histogram/CDF on CPU (fast), remap on GPU (parallel).
                    // Step 1: Download current frame to compute histogram.
                    // The CDF of display-encoded luma, built as the CPU
                    // path builds it.
                    let current_buf = if current_in_a { buf_a } else { buf_b };
                    let frame_bytes = self.download(current_buf, byte_size);
                    let frame_data: &[f32] = bytemuck::cast_slice(&frame_bytes);

                    let cdf = crate::equalize_cdf(frame_data, self.luma_coefs);

                    // Step 4: Upload CDF as storage buffer and dispatch GPU remap shader
                    let cdf_bytes = bytemuck::cast_slice(&cdf);
                    debug_assert_eq!(cdf.len(), EQUALIZE_CDF_LEN);
                    self.upload(&self.equalize_cdf_buf, 0, cdf_bytes);

                    self.write_params(&EqualizeParams {
                        width,
                        height,
                        strength: *strength,
                        _pad0: 0,
                        luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0],
                    });

                    let input = if current_in_a { buf_a } else { buf_b };
                    let output = if current_in_a { buf_b } else { buf_a };

                    // Create bind group with 4 bindings (like huesat)
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("equalize_bg"),
                        layout: &self.huesat_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: self.equalize_cdf_buf.as_entire_binding() },
                        ],
                    });

                    let mut encoder = self.encoder(None);
                    {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                        pass.set_pipeline(&self.equalize_pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        dispatch_flat(&mut pass, workgroups);
                    }
                    self.submit(encoder);
                    // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
                    // queue drains between submits. See submit_chain for context.
                    self.device.poll(wgpu::Maintain::Poll);

                    !current_in_a
                }
                _ => {
                    self.effects_incomplete.set(true);
                    in_a
                }
            }
    }

    /// Make the buffers recorded effects use at `width` × `height`: the
    /// chain's frames (`buf_c` is a run's scratch) and the layer passes'
    /// own. A caller that records effects itself calls this once per
    /// output size, so whether a layer pass records never depends on
    /// whether a chain happened to run at that size before.
    #[doc(hidden)]
    pub fn ensure_recording_buffers(&mut self, width: u32, height: u32) {
        let byte_size = u64::from(width) * u64::from(height) * 16;
        self.ensure_buffers(byte_size);
        self.layer_gpu.ensure_buffers(&self.device, byte_size);
    }

    /// Run `effects` as nodes of the open chain (`begin_chain`), on the
    /// picture in slot A (`in_is_a`) or B (#1089, phase 2b M2, step 10).
    /// A recorded effect goes into the chain's own encoder, so a grade
    /// and the effects after it are one submit. One that isn't recorded
    /// is a break: the chain so far is submitted, the effect runs in its
    /// own pass on the same buffers, and a new chain segment opens; the
    /// picture never leaves the GPU. `buf_c` is the multi-pass scratch,
    /// so effects run after any secondary that keeps a picture there.
    #[doc(hidden)]
    pub fn chain_run_effects(&mut self, effects: &[BuiltinEffect], in_is_a: bool, width: u32, height: u32) -> ChainEffects {
        let mut out = ChainEffects { in_a: in_is_a, breaks: 0, complete: true };
        // This run's answer for `last_effects_complete`: a break that
        // leaves an effect out says so below.
        self.effects_incomplete.set(false);
        if self.is_device_lost() {
            out.complete = effects.is_empty();
            self.effects_incomplete.set(!out.complete);
            return out;
        }
        // Recording reads the layer passes' buffers too: made now, so
        // whether an effect records never depends on an earlier run.
        self.ensure_recording_buffers(width, height);
        let spills = |e: &BuiltinEffect| crate::layer::Spill::of(e).is_some();
        let mut rest = effects;
        while !rest.is_empty() {
            // Effects that spill past the picture are one layer: each is
            // cast from the same picture and stacked under it, so a run of
            // them goes as one, as `apply_effects` groups them.
            let n = if spills(&rest[0]) { rest.iter().take_while(|e| spills(e)).count() } else { 1 };
            let (part, after) = rest.split_at(n);
            rest = after;
            let mut outcome = self.record_in_chain(part, out.in_a, width, height);
            if outcome == Recorded::OutOfSlots {
                // The chain's encoder holds the slots' readers: submit it,
                // then they are free, and the chain goes on in a new one.
                self.submit_chain();
                self.begin_chain();
                outcome = self.record_in_chain(part, out.in_a, width, height);
            }
            match outcome {
                Recorded::Done => out.in_a = !out.in_a,
                Recorded::Identity => {}
                Recorded::NotRecordable | Recorded::OutOfSlots => {
                    for fx in part {
                        crate::residency::record(crate::residency::effect_id(fx), "GPU recorder declined; chain break", crate::residency::Work::Fallback);
                    }
                    out.breaks += 1;
                    self.submit_chain();
                    out.in_a = self.run_break_pass(part, out.in_a, width, height);
                    out.complete &= self.last_effects_complete();
                    self.begin_chain();
                }
            }
        }
        // A later clean break must not hide an earlier one that left an
        // effect out: the run's answer is all of them.
        self.effects_incomplete.set(!out.complete);
        out
    }

    /// Slots the pool may hold at once, of all sizes (9b-1's budget).
    pub const MAX_SLOTS: usize = 8;

    /// The stack's composite so far in a pool slot of its own (#2758):
    /// what is below a layer that reads it, copied out before that layer is
    /// placed, for its run to read as its side picture. Held until that
    /// run's slots are given back. `None` without a stack or a slot.
    pub fn stack_acc_as_picture(&mut self) -> Option<KeptPicture> {
        if self.is_device_lost() || !self.stack_ensure_acc() {
            return None;
        }
        let (width, height) = self.stack_dims;
        let acc = self.stack_acc.clone()?;
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Image }, |k| slot_buffer(device, k)).ok()?;
        let mut enc = self.encoder(Some("stack_acc_as_picture"));
        enc.copy_buffer_to_buffer(&acc, 0, self.slot_pool.buffer(id), 0, u64::from(width) * u64::from(height) * 16);
        self.submit(enc);
        Some(KeptPicture(id))
    }

    /// Layer `slot` of the stack in a pool slot of its own (#2758): a title
    /// drawn there as a layer is, handed to a clip's run as its side
    /// picture. `None` without that layer or a slot.
    pub fn stack_layer_as_picture(&mut self, slot: usize) -> Option<KeptPicture> {
        if self.is_device_lost() {
            return None;
        }
        let (width, height) = self.stack_dims;
        let layer = self.stack_layers.get(slot)?.clone();
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Image }, |k| slot_buffer(device, k)).ok()?;
        let mut enc = self.encoder(Some("stack_layer_as_picture"));
        enc.copy_buffer_to_buffer(&layer, 0, self.slot_pool.buffer(id), 0, u64::from(width) * u64::from(height) * 16);
        self.submit(enc);
        Some(KeptPicture(id))
    }

    /// Keep the chain's picture (in `buf_a` when `in_is_a`) in a pool slot
    /// of its own, copied in the chain's encoder, so it is ordered against
    /// the passes around it. `None` when there is no chain or the pool's
    /// budget refuses.
    pub fn keep_picture(&mut self, in_is_a: bool, width: u32, height: u32) -> Option<KeptPicture> {
        if self.is_device_lost() || self.chain_encoder.is_none() {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Image }, |k| slot_buffer(device, k)).ok()?;
        let size = u64::from(width) * u64::from(height) * 16;
        let src = if in_is_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }.expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        enc.copy_buffer_to_buffer(src, 0, self.slot_pool.buffer(id), 0, size);
        Some(KeptPicture(id))
    }

    /// `px`, a picture from the CPU (a composite's second picture, read
    /// back or the source frame), in a pool slot of its own. Uploaded, so
    /// it lands before the open encoder: the slot is one no pass of it has
    /// used, held after its return until that encoder is submitted. `None`
    /// when the size is wrong or the budget refuses.
    pub fn upload_picture(&mut self, px: &[f32], width: u32, height: u32) -> Option<KeptPicture> {
        if self.is_device_lost() || (px.len() * 4) as u64 != u64::from(width) * u64::from(height) * 16 {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease_for_upload(SlotKey { width, height, usage: SlotUsage::Image }, |k| slot_buffer(device, k)).ok()?;
        self.upload_frame(self.slot_pool.buffer(id), bytemuck::cast_slice(px));
        self.slot_pool.mark_cpu_written(id);
        Some(KeptPicture(id))
    }

    /// A kept picture's pixels, read back for a reader off the chain.
    /// `None` while the chain that keeps it is still being recorded.
    pub fn read_picture(&self, picture: &KeptPicture, pixel_count: usize) -> Option<Vec<f32>> {
        if self.is_device_lost() || self.chain_encoder.is_some() {
            return None;
        }
        Some(self.download_linear_rgba_f32(self.slot_pool.buffer(picture.0), pixel_count)).filter(|v| v.len() == pixel_count * 4)
    }

    /// Give a kept picture's slot back: its last reader is recorded.
    pub fn release_picture(&mut self, picture: KeptPicture) {
        self.slot_pool.release(picture.0);
    }

    /// A window's shapes as the buffers the window mix reads (#1252): its
    /// shape records and its bezier points. Each call gets its own buffers:
    /// several masked effects in one encoder would otherwise share one
    /// upload, and the queue writes all land before the encoder runs.
    fn window_buffers(&self, masks: &[GpuMaskDef]) -> (wgpu::Buffer, wgpu::Buffer) {
        let (records, points) = Self::build_mask_storage(masks);
        let records: &[u8] = if records.is_empty() {
            &[0u8; std::mem::size_of::<MaskRecord>()]
        } else {
            bytemuck::cast_slice(&records)
        };
        let points: &[u8] = if points.is_empty() { &[0u8; 8] } else { bytemuck::cast_slice(&points) };
        let make = |label: &str, bytes: &[u8]| {
            let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: bytes.len() as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&buf, 0, bytes);
            buf
        };
        (make("window_records", records), make("window_points", points))
    }

    /// [`Self::chain_run_effects`], mixed back over the picture it started
    /// from (phase 2b M2, step 9c): by `mix`, and by the coverage `mask`
    /// gives (straight RGBA of the frame's size, in a pool slot; a window's
    /// shapes, worked out inside the mix with no slot, #1252; none is
    /// white). The input is kept in a pool slot before the effects
    /// run, and the mix writes the other chain buffer, so nothing aliases.
    /// `None` when the pool's budget refuses, before anything is recorded:
    /// the caller runs the node another way.
    #[doc(hidden)]
    pub fn chain_run_effects_mixed(
        &mut self,
        effects: &[BuiltinEffect],
        in_is_a: bool,
        width: u32,
        height: u32,
        mix: crate::mask_mix::MaskMix,
        mask: Option<EffectMatte<'_>>,
    ) -> Option<ChainEffects> {
        // One effect recorded as a single dispatch writes the other chain
        // buffer and leaves its input where it is: the mix reads it there
        // and writes over it (#1254). Several effects, or one of several
        // passes, may write over the input, and a copy of it is kept.
        let in_place = matches!(effects, [one] if Self::single_dispatch(one));
        self.chain_run_mixed(in_place, in_is_a, width, height, mix, mask, |gpu| gpu.chain_run_effects(effects, in_is_a, width, height))
    }

    /// A merge on the chain (`apply_merge_chain`) mixed back over the
    /// picture it lays over by `mix` and the coverage `mask` gives
    /// (#2758): the merge's picture laid only where the mask is, as a
    /// masked effect's result is. The merge is one dispatch into the other
    /// chain buffer, its input left where it is.
    #[allow(clippy::too_many_arguments)]
    pub fn chain_merge_mixed(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        opacity: f32,
        mode: u32,
        other: &KeptPicture,
        mix: crate::mask_mix::MaskMix,
        mask: Option<EffectMatte<'_>>,
    ) -> Option<ChainEffects> {
        self.chain_run_mixed(true, in_is_a, width, height, mix, mask, |gpu| {
            gpu.apply_merge_chain(in_is_a, width, height, opacity, mode, other);
            ChainEffects { in_a: !in_is_a, breaks: 0, complete: !gpu.is_device_lost() }
        })
    }

    /// `body` run on the chain, mixed back over the picture it started
    /// from by `mix` and `mask`, as [`Self::chain_run_effects_mixed`]
    /// says; `in_place` when `body` is one dispatch that leaves its input
    /// where it is, so no copy of it is kept.
    #[allow(clippy::too_many_arguments)]
    fn chain_run_mixed(
        &mut self,
        in_place: bool,
        in_is_a: bool,
        width: u32,
        height: u32,
        mix: crate::mask_mix::MaskMix,
        mask: Option<EffectMatte<'_>>,
        body: impl FnOnce(&mut Self) -> ChainEffects,
    ) -> Option<ChainEffects> {
        if self.is_device_lost() || self.chain_encoder.is_none() {
            return None;
        }
        let bytes = u64::from(width) * u64::from(height) * 16;
        let image = SlotKey { width, height, usage: SlotUsage::Image };
        let make = |device: &wgpu::Device, k: SlotKey| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(match k.usage { SlotUsage::Image => "slot_image", SlotUsage::Matte => "slot_matte" }),
                size: u64::from(k.width) * u64::from(k.height) * 16,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let device = &self.device;
        let kept = if in_place { None } else { Some(self.slot_pool.lease(image, |k| make(device, k)).ok()?) };
        let keyed = matches!(mask, Some(EffectMatte::Keyed(_)));
        // A window is drawn inside the mix itself: its shapes, no matte.
        let window = match mask {
            Some(EffectMatte::Shapes(masks)) => Some((self.window_buffers(masks), masks.len() as u32)),
            _ => None,
        };
        let matte = match mask {
            Some(EffectMatte::Keyed(m)) => Some(m.0),
            Some(EffectMatte::Shapes(_)) | None => None,
            Some(source) => {
                let device = &self.device;
                // Uploaded, so it lands before the open encoder: a slot no
                // pass of it has used (a keyed matte read earlier in it).
                match self.slot_pool.lease_for_upload(SlotKey { usage: SlotUsage::Matte, ..image }, |k| make(device, k)) {
                    Ok(id) => {
                        match source {
                            EffectMatte::Pixels(data) => {
                                self.queue.write_buffer(self.slot_pool.buffer(id), 0, bytemuck::cast_slice(data));
                                self.slot_pool.mark_cpu_written(id);
                            }
                            EffectMatte::Shapes(_) | EffectMatte::Keyed(_) => unreachable!("taken above"),
                        }
                        Some(id)
                    }
                    Err(_) => {
                        if let Some(kept) = kept {
                            self.slot_pool.release(kept);
                        }
                        return None;
                    }
                }
            }
        };
        // The input, kept before the effects can write over it.
        if let Some(kept) = kept {
            let src = if in_is_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
            let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
            enc.copy_buffer_to_buffer(src.expect("chain buffers"), 0, self.slot_pool.buffer(kept), 0, bytes);
        }
        let mut run = body(self);
        // A break runs the effect in passes of its own, which this didn't
        // keep its input from: the run is said incomplete, and the caller
        // takes another route, as for a mix that wasn't recorded.
        if in_place && run.breaks > 0 {
            run.complete = false;
            if let Some(m) = matte.filter(|_| !keyed) {
                self.slot_pool.release(m);
            }
            return Some(run);
        }
        // The mix's uniforms are claimed once the effects are recorded: an
        // effect that breaks the chain resets the ring. A full ring is
        // submitted and the chain goes on in a new one.
        let claim = |gpu: &mut Self| match &window {
            Some((_, count)) => gpu.claim_mask_mix_window(width, height, mix, *count),
            None => gpu.claim_mask_mix(width, height, mix, matte.is_some(), false),
        };
        let mut params = claim(self);
        if params == Err(crate::mask_mix::MaskMixError::ParamsExhausted) {
            self.submit_chain();
            self.begin_chain();
            params = claim(self);
        }
        let recorded = match params {
            Ok(Some(slot)) => {
                let mut encoder = self.chain_encoder.take().expect(Self::CHAIN_ENCODER_EXPECT);
                let (a, b) = (self.buf_a.as_ref().expect("chain buffers"), self.buf_b.as_ref().expect("chain buffers"));
                let (effect, out) = if run.in_a { (a, b) } else { (b, a) };
                // The picture under the effect: its kept copy; in place, the
                // buffer the effect read, which the mix writes (`out`); or,
                // when the effect wrote nothing, the effect's buffer itself.
                let input = match kept {
                    Some(kept) => Some(self.slot_pool.buffer(kept)),
                    None if run.in_a == in_is_a => Some(effect),
                    None => None,
                };
                let r = match &window {
                    Some(((records, points), _)) => self.record_mask_mix_window_in(&mut encoder, input, effect, records, points, out, width, height, slot),
                    None => self.record_mask_mix_in(&mut encoder, input, effect, matte.map(|m| self.slot_pool.buffer(m)), out, width, height, slot),
                };
                self.chain_encoder = Some(encoder);
                r
            }
            Ok(None) => Ok(()),
            Err(e) => Err(e),
        };
        // Their readers are recorded: returned (a CPU-written matte is held
        // until the encoder is submitted).
        if let Some(kept) = kept {
            self.slot_pool.release(kept);
        }
        // A keyed matte is its key's, for its other readers.
        if let Some(m) = matte.filter(|_| !keyed) {
            self.slot_pool.release(m);
        }
        match recorded {
            Ok(()) => run.in_a = !run.in_a,
            // The effects ran, the mix didn't: the picture lacks the mix,
            // and the run says so (the caller takes another route).
            Err(e) => {
                tracing::warn!("an effect's mix wasn't recorded: {e}");
                run.complete = false;
            }
        }
        Some(run)
    }

    /// Whether `effect` is recorded as one dispatch from its input into
    /// the other chain buffer, leaving its input as it was (#1254): the
    /// one-slot effects, but for those that run passes of their own
    /// (a denoise's iterations, a card's rounded corners).
    fn single_dispatch(effect: &BuiltinEffect) -> bool {
        slots_needed(effect) == Some(1) && !matches!(effect, BuiltinEffect::Denoise { .. } | BuiltinEffect::RoundedCorners { .. })
    }

    /// The run is over: every pool slot is free again, and a size no run
    /// has used for a while is dropped. Gives back the most slots leased
    /// at once.
    #[doc(hidden)]
    pub fn end_slot_run(&mut self) -> usize {
        self.slot_pool.end_run()
    }

    /// Record one effect, or one run of spills, into the open chain
    /// encoder.
    fn record_in_chain(&mut self, part: &[BuiltinEffect], in_is_a: bool, width: u32, height: u32) -> Recorded {
        let (Some(mut encoder), Some(mut slots)) = (self.chain_encoder.take(), self.effect_params.take()) else {
            return Recorded::NotRecordable;
        };
        let (a, b) = (self.buf_a.as_ref().expect("chain buffers"), self.buf_b.as_ref().expect("chain buffers"));
        let (src, dst) = if in_is_a { (a, b) } else { (b, a) };
        let rec = &mut EffectRecording { encoder: &mut encoder, slots: &mut slots };
        let outcome = match part {
            [one] if crate::layer::Spill::of(one).is_none() => self.record_effect(one, src, dst, self.buf_c.as_ref(), width, height, rec),
            spills => self.record_spill_run(spills, src, dst, self.buf_c.as_ref(), width, height, rec),
        };
        self.chain_encoder = Some(encoder);
        self.effect_params = Some(slots);
        outcome
    }

    /// Whether the last chain's effects ([`Self::chain_run_effects`],
    /// its breaks included) were all applied. `false` when one that is only ever recorded could not be
    /// (a lost device): its picture is missing that effect, and the caller
    /// takes another path or declines the frame, never shows it.
    pub fn last_effects_complete(&self) -> bool {
        !self.effects_incomplete.get()
    }

    /// For tests: make every `record_effect` answer `NotRecordable`, as a
    /// lost device does, to prove a run reports what it left out.
    #[doc(hidden)]
    pub fn refuse_recording(&mut self, refuse: bool) {
        self.refuse_recording = refuse;
    }

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CropParams {
    width: u32,
    height: u32,
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    feather: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransformParams {
    width: u32,
    height: u32,
    anchor_x: f32,
    anchor_y: f32,
    position_x: f32,
    position_y: f32,
    cos_r: f32,
    sin_r: f32,
    inv_sx: f32,
    inv_sy: f32,
    cos_rx: f32,
    sin_rx: f32,
    cos_ry: f32,
    sin_ry: f32,
    focal: f32,
    _pad: [u32; 3],
}

impl TransformParams {
    /// The Transform effect's inverse map at `width` × `height`: what its
    /// pass reads, and what the shutter's pass reads for each sample
    /// (#1923), so the two place a picture alike. `None` for any other
    /// effect.
    pub(crate) fn of(effect: &BuiltinEffect, width: u32, height: u32) -> Option<Self> {
        let BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, rotation_x, rotation_y, perspective, .. } = effect else {
            return None;
        };
        let rot = rotation.to_radians();
        let inverse = |s: f32| if s.abs() > f32::EPSILON { 1.0 / s } else { 1.0 };
        let (rx, ry) = (rotation_x.to_radians(), rotation_y.to_radians());
        Some(Self {
            width,
            height,
            anchor_x: *anchor_x * width as f32,
            anchor_y: *anchor_y * height as f32,
            position_x: *position_x,
            position_y: *position_y,
            cos_r: rot.cos(),
            sin_r: rot.sin(),
            inv_sx: inverse(*scale_x),
            inv_sy: inverse(*scale_y),
            cos_rx: rx.cos(),
            sin_rx: rx.sin(),
            cos_ry: ry.cos(),
            sin_ry: ry.sin(),
            focal: if *perspective > 0.001 { width as f32 / *perspective } else { 0.0 },
            _pad: [0; 3],
        })
    }

    /// A sample's row for the shutter's pass: four vec4s.
    fn shutter_row(&self) -> [f32; 16] {
        [
            self.anchor_x, self.anchor_y, self.position_x, self.position_y,
            self.cos_r, self.sin_r, self.inv_sx, self.inv_sy,
            self.cos_rx, self.sin_rx, self.cos_ry, self.sin_ry,
            self.focal, 0.0, 0.0, 0.0,
        ]
    }
}

/// The corner pin's inverse map, nine coefficients row-major, from
/// `corner_pin::inverse_map`: the same numbers the CPU samples with.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CornerPinParams {
    width: u32,
    height: u32,
    inv: [f32; 9],
    _pad: u32,
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuInputTransformParams {
    width: u32, height: u32, _pad0: u32, _pad1: u32,
}

const _: () = assert!(std::mem::size_of::<GpuApplyMatteParams>() == 32, "the WGSL Params is 32 bytes");

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuMaskCutoutParams {
    width: u32, height: u32, _pad0: u32, _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuScopeAccumParams {
    width: u32, height: u32, display_columns: u32, step: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuScopeRenderParams {
    out_w: u32, out_h: u32, num_cols: u32, max_count: u32,
}

/// Mask-pipeline frame constants. Per-mask shape data is in the
/// `MaskRecord` storage buffer (binding 2); per-bezier points are
/// pooled in another storage buffer (binding 3). The uniform only
/// holds frame-wide knobs.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuMaskParams {
    width: u32,
    height: u32,
    mask_count: u32,
    _pad: u32,
}
// 4 fields × 4 bytes = 16 bytes — the smallest legal uniform.

/// One per-mask record in the storage buffer. Layout matches the
/// WGSL `MaskRecord` struct in [`SHADER_MASK_V2`] one-for-one (12
/// `u32`/`f32` slots, then the warp's three rows as `vec4`s at 16-byte
/// offsets = 96 bytes, 16-byte aligned for `array<MaskRecord>`).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskRecord {
    shape_type: u32,     // 0=circle, 1=rect, 2=gradient, 3=bezier, 4=radial gradient
    flags: u32,          // bit 0 invert, bit 1 warp, bits 2-3 combine, bits 8-15 opacity (0-255)
    bezier_offset: u32,  // index into points buffer (bezier only)
    bezier_count: u32,   // number of points (bezier only)
    cx: f32, cy: f32, p1: f32, p2: f32,
    p3: f32, feather: f32, feather_inner: f32, _pad1: u32,
    w0: [f32; 4], w1: [f32; 4], w2: [f32; 4],  // warp rows (flag bit 1)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuLut3dParams {
    width: u32,
    height: u32,
    lut_size: u32,
    opacity: f32,
    /// Non-zero: tetrahedral interpolation instead of trilinear.
    tetrahedral: u32,
    /// Bit 0: `pre` applies, bit 1: `post` (see `SHADER_LUT3D`).
    bracket: u32,
    _pad0: u32,
    _pad1: u32,
    /// Rows of the working space → LUT input primaries matrix.
    pre: [[f32; 4]; 3],
    /// Rows of the LUT output → working space primaries matrix.
    post: [[f32; 4]; 3],
}
// 8 × 4 bytes + 2 × 48 = 128 bytes, inside PARAMS_SIZE

impl GpuLut3dParams {
    fn new(width: u32, height: u32, lut_size: u32, opacity: f32, tetrahedral: bool, bracket: LutBracket) -> Self {
        let rows = |m: Option<[[f32; 3]; 3]>| m.map_or([[0.0; 4]; 3], |m| m.map(|r| [r[0], r[1], r[2], 0.0]));
        Self {
            width, height, lut_size, opacity,
            tetrahedral: tetrahedral as u32,
            bracket: bracket.0.is_some() as u32 | (bracket.1.is_some() as u32) << 1,
            _pad0: 0, _pad1: 0,
            pre: rows(bracket.0),
            post: rows(bracket.1),
        }
    }
}

/// The primaries matrices a LUT lookup is bracketed with: working space
/// into the LUT's input primaries before, its output primaries back
/// after, rows first; `None` where the two agree. `klipp_color::lut_bracket`
/// makes them from a LUT's tags.
pub type LutBracket = (Option<[[f32; 3]; 3]>, Option<[[f32; 3]; 3]>);

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuQualifierParams {
    width: u32,
    height: u32,
    // Qualifier selection
    hue_center: f32,     // 0-1 (normalized from 0-360°)
    hue_width: f32,      // 0-1
    hue_soft: f32,       // 0-1
    sat_low: f32,
    sat_high: f32,
    sat_soft: f32,
    lum_low: f32,
    lum_high: f32,
    lum_soft: f32,
    invert: u32,         // 0 or 1
    // Secondary grade
    sec_exposure: f32,
    sec_saturation: f32,
    sec_hue_shift: f32,  // radians
    sec_temperature: f32,
    // Matte preview
    matte_preview: u32,
    /// 0 HSL, 1 RGB (#895).
    mode: u32,
    _pad1: u32,
    _pad2: u32,
    /// The RGB mode's ranges: red, green, blue low and high, the soft edge.
    rgb: [f32; 7],
    _pad3: f32,
    /// The 3D mode's volumes (#895): each centre (its `w` 1 when there is
    /// one) and inverse spread by rows, and the reach and its soft edge.
    keep_mean: [f32; 4],
    keep_inv: [[f32; 4]; 3],
    remove_mean: [f32; 4],
    remove_inv: [[f32; 4]; 3],
    reach: [f32; 4],
}
// 64 fields × 4 bytes = 256 bytes: the whole PARAMS_SIZE

impl GpuQualifierParams {
    /// The mode and RGB ranges `rgb` gives (#895): HSL's, with the RGB
    /// ranges unused, for `None`.
    fn rgb_mode(rgb: Option<[f32; 7]>) -> (u32, [f32; 7]) {
        match rgb {
            Some(r) => (1, r),
            None => (0, [0.0, 1.0, 0.0, 1.0, 0.0, 1.0, 0.1]),
        }
    }

    /// The mode, ranges and volumes a key's shape gives: 3D for a
    /// volume, else RGB's or HSL's.
    fn shape(rgb: Option<[f32; 7]>, volume: Option<QualifierVolume>) -> (u32, [f32; 7], [[f32; 4]; 9]) {
        let (mut mode, ranges) = Self::rgb_mode(rgb);
        let mut v = [[0.0f32; 4]; 9];
        if let Some(q) = volume {
            mode = 2;
            let mut put = |at: usize, k: Option<crate::cpu_grade::KeyVolume>| {
                if let Some(k) = k {
                    v[at] = [k.mean[0], k.mean[1], k.mean[2], 1.0];
                    for r in 0..3 {
                        v[at + 1 + r] = [k.inv[r][0], k.inv[r][1], k.inv[r][2], 0.0];
                    }
                }
            };
            put(0, q.keep);
            put(4, q.remove);
            v[8] = [q.radius, q.soft, 0.0, 0.0];
        }
        (mode, ranges, v)
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuLumaKeyParams {
    width: u32,
    height: u32,
    low: f32,
    high: f32,
    soft: f32,
    invert: u32,
    /// 1: the matte multiplies the picture's alpha, the colour kept (the
    /// Luma Key clip effect, #1638); 0: the greyscale matte (the node).
    into_alpha: u32,
    _pad1: u32,
    /// Working-space luma coefficients, `klipp_color::linear::luma_coefs`.
    /// A vec4 because WGSL aligns vec3 to 16 bytes anyway; `.w` unused.
    luma: [f32; 4],
}
// 8 u32 + vec4 = 48 bytes (well under 256-byte PARAMS_SIZE)

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuTemporalDenoiseParams {
    width: u32,
    height: u32,
    /// Neighbours bound (0 to 2); a slot past it is bound but not read.
    count: u32,
    _pad0: u32,
    /// `temporal_denoise::denominator(threshold)`, worked out on the host.
    denom: f32,
    strength: f32,
    _pad1: u32,
    _pad2: u32,
    /// The guard's luma weights, as the caller passes them; `.w` unused.
    luma: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuDifferenceKeyParams {
    width: u32,
    height: u32,
    tolerance: f32,
    soft: f32,
    invert: u32,
    /// 0 = RGB (Chebyshev), 1 = weighted luminance. Matches
    /// `klipp_core::DifferenceKeyMatchSpace`'s order; the host converts.
    match_space: u32,
    _pad0: u32,
    _pad1: u32,
    /// Working-space luminance row; `.w` unused. Both sides of the
    /// comparison are linear working-space values, so Rec.709's row
    /// would mis-weight an ACEScg project.
    luma: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuSoftClipParams {
    width: u32,
    height: u32,
    threshold: f32,
    knee: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    _pad3: u32,
}
// 8 fields × 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuGrainParams {
    width: u32,
    height: u32,
    amount: f32,
    size: f32,
    shadows: f32,
    highlights: f32,
    frame_seed: u32,
    monochrome: u32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad_luma: u32,
}
// 8 fields × 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AlphaBlurParams {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AlphaDilateParams {
    width: u32,
    height: u32,
    radius: u32,
    radius_sq: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InvertParams {
    width: u32,
    height: u32,
    mix: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CstParams {
    width: u32,
    height: u32,
    /// Bit 0: read through the input curve, 1: write through the output
    /// curve, 2: convert by the matrix.
    flags: u32,
    _pad: u32,
    /// The matrix's rows.
    m: [[f32; 4]; 3],
}

impl CstParams {
    fn new(width: u32, height: u32, t: &crate::colour_transform::CstTables) -> Self {
        let m = t.matrix.unwrap_or([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        Self {
            width,
            height,
            flags: t.decode.is_some() as u32 | (t.encode.is_some() as u32) << 1 | (t.matrix.is_some() as u32) << 2,
            _pad: 0,
            m: m.map(|r| [r[0], r[1], r[2], 0.0]),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PosterizeParams {
    width: u32,
    height: u32,
    /// Clamped to [2, 256] (`PosterizeLadder`).
    levels: f32,
    /// How many of the table's above-white edges are edges.
    above_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MosaicParams {
    width: u32,
    height: u32,
    block_size: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EdgeDetectParams {
    width: u32,
    height: u32,
    strength: f32,
    mix: f32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad_luma: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChannelMixerParams {
    width: u32,
    height: u32,
    rr: f32,
    rg: f32,
    rb: f32,
    gr: f32,
    gg: f32,
    gb: f32,
    br: f32,
    bg: f32,
    bb: f32,
    _pad: u32,
}
// 12 fields × 4 bytes = 48 bytes (multiple of 16)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DirectionalBlurParams {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    samples: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}
// 8 fields × 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EmbossParams {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    strength: f32,
    mix: f32,
    _pad0: u32,
    _pad1: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MirrorParams {
    width: u32,
    height: u32,
    horizontal: u32,
    vertical: u32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LensDistortionParams {
    width: u32,
    height: u32,
    distortion: f32,
    scale: f32,
}
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DilateErodeParams {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DenoiseParams {
    width: u32,
    height: u32,
    spatial_sigma: f32,
    /// The range sigma, on the luma difference, for smoothing the luma:
    /// the Luma slider. 0 leaves the luma as it is (#1039).
    luma_sigma: f32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    /// The largest kernel radius, in pixels of the buffer: 10 at the
    /// sequence's size, scaled with the render size for the grade node.
    max_kernel: u32,
    /// The range sigma, also on the luma difference, for smoothing the
    /// chroma `rgb − Y`: the Chroma slider. 0 leaves the chroma as it
    /// is (#1039). Before, the two were averaged into one sigma.
    chroma_sigma: f32,
    _pad: [u32; 3],
}
// 12 fields x 4 bytes = 48 bytes, a multiple of 16 as a uniform needs

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GammaParams {
    width: u32,
    height: u32,
    inv_gamma: f32,
    _pad: u32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct HSVAdjustParams {
    width: u32,
    height: u32,
    hue_shift: f32,
    saturation: f32,
    value: f32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct NoiseGeneratorParams {
    width: u32,
    height: u32,
    amount: f32,
    monochrome: u32,
    seed: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MedianFilterParams {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}
// 4 fields x 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RadialBlurParams {
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    amount: f32,
    samples: u32,
    mode: u32,
    _pad: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LevelsParams {
    width: u32,
    height: u32,
    in_black: f32,
    in_white: f32,
    inv_gamma: f32,
    out_black: f32,
    out_white: f32,
    _pad: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct EqualizeParams {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
    /// The working space's luma weights, and a pad.
    luma: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LetterboxParams {
    width: u32,
    height: u32,
    aspect_ratio: f32,
    bar_color: u32,
    opacity: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}
// 8 fields x 4 bytes = 32 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RscParams {
    width: u32,
    height: u32,
    strength: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct StabilizationParams {
    width: u32,
    height: u32,
    crop_zoom: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaticAberrationParams {
    width: u32,
    height: u32,
    red_shift: f32,
    blue_shift: f32,
    center_x: f32,
    center_y: f32,
    _pad0: u32,
    _pad1: u32,
}
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

macro_rules! shader_linear_helpers_wgsl {
    () => {
        concat!(
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/gpu/shaders/include/srgb_components.wgsl")),
            shader_pixel_storage_wgsl!(),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/gpu/shaders/include/linear_helpers.wgsl")),
        )
    };
}

/// A window's shapes as WGSL: the `MaskRecord` layout and `eval_record`,
/// the coverage of one shape at a pixel (signed distance, feather, invert,
/// opacity). The including shader binds `points: array<vec2<f32>>`, which
/// `bezier_sdf` reads. One copy for the window drawn into a matte
/// (`SHADER_MASK_V2`) and the window drawn inside an effect's mix (#1252),
/// so the two can't drift apart.
macro_rules! mask_sdf_wgsl {
    () => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/gpu/shaders/include/mask_sdf.wgsl"))
    };
}

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
mod output;
use output::*;
#[cfg(test)]
mod shader_files;
pub mod planar;
mod planar_wgsl;
pub use flow::FlowKey;
mod mask_mix;

const SHADER_CROP: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    crop_left: f32,
    crop_right: f32,
    crop_top: f32,
    crop_bottom: f32,
    feather: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let xf = f32(x);
    let yf = f32(y);

    if xf < params.crop_left || xf >= params.crop_right || yf < params.crop_top || yf >= params.crop_bottom {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }

    var pixel = unpack(input[idx]);

    if params.feather > 0.0 {
        let dist = min(
            min(xf - params.crop_left, params.crop_right - 1.0 - xf),
            min(yf - params.crop_top, params.crop_bottom - 1.0 - yf),
        );
        if dist < params.feather {
            // Clamped: on the last row and column `dist` goes slightly
            // negative, and the alpha came out negative with it.
            let factor = clamp(dist / params.feather, 0.0, 1.0);
            pixel.w = pixel.w * factor;
        }
    }

    output[idx] = pack(pixel);
}
"
);

const SHADER_TRANSFORM: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    anchor_x: f32,
    anchor_y: f32,
    position_x: f32,
    position_y: f32,
    cos_r: f32,
    sin_r: f32,
    inv_sx: f32,
    inv_sy: f32,
    cos_rx: f32,
    sin_rx: f32,
    cos_ry: f32,
    sin_ry: f32,
    focal: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let dx = f32(x) - params.anchor_x - params.position_x;
    let dy = f32(y) - params.anchor_y - params.position_y;

    // Inverse Z-rotation (2D)
    let rx = dx * params.cos_r + dy * params.sin_r;
    let ry = -dx * params.sin_r + dy * params.cos_r;

    // Inverse scale
    var sx = rx * params.inv_sx;
    var sy = ry * params.inv_sy;

    // 3D tilt: the point on the tilted picture this pixel sees — the
    // ray through the pinhole meeting the picture's plane. Mirrors
    // `transform::unproject` on the CPU; see there for the maths.
    if params.cos_rx != 1.0 || params.cos_ry != 1.0 {
        let a = params.sin_rx;
        let b = params.sin_ry;
        let f = params.focal;
        var u = 0.0;
        var v = 0.0;
        var seen = true;
        if f > 0.0 {
            let a11 = sx * b - f * params.cos_ry;
            let a12 = -sx * a * params.cos_ry - f * a * b;
            let a21 = sy * b;
            let a22 = -sy * a * params.cos_ry - f * params.cos_rx;
            let det = a11 * a22 - a12 * a21;
            if abs(det) < 1e-9 {
                seen = false;
            } else {
                let b1 = -sx * f;
                let b2 = -sy * f;
                u = (b1 * a22 - a12 * b2) / det;
                v = (a11 * b2 - b1 * a21) / det;
                seen = f + (u * b - v * a * params.cos_ry) > 0.0;
            }
        } else if abs(params.cos_rx) < 1e-6 || abs(params.cos_ry) < 1e-6 {
            seen = false;
        } else {
            v = sy / params.cos_rx;
            u = (sx - v * a * b) / params.cos_ry;
        }
        if seen {
            sx = u;
            sy = v;
        } else {
            sx = -1.0e9;
            sy = -1.0e9;
        }
    }

    sx = sx + params.anchor_x;
    sy = sy + params.anchor_y;

    // Bilinear sample — same reasoning as the CPU path in
    // klipp-effects/src/transform.rs: nearest-neighbour quantises
    // sub-pixel motion to integer steps and produces a low-rate
    // stutter on slow scrolls (credits, slides, ticker crawls).
    // Storage buffers are linear-f32 here, so the lerp is in
    // linear light — cleaner than the CPU path's sRGB approximation.
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);

    let p00 = sample_pixel(x0, y0);
    let p10 = sample_pixel(x1, y0);
    let p01 = sample_pixel(x0, y1);
    let p11 = sample_pixel(x1, y1);

    let top = mix(p00, p10, fx);
    let bot = mix(p01, p11, fx);
    output[idx] = pack(mix(top, bot, fy));
}

fn sample_pixel(x: i32, y: i32) -> vec4<f32> {
    if x < 0 || y < 0 || x >= i32(params.width) || y >= i32(params.height) {
        return vec4<f32>(0.0);
    }
    let src_idx = u32(y) * params.width + u32(x);
    return unpack(input[src_idx]);
}
"
);

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
const SHADER_CORNER_PIN: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    h0: f32,
    h1: f32,
    h2: f32,
    h3: f32,
    h4: f32,
    h5: f32,
    h6: f32,
    h7: f32,
    h8: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let fx0 = f32(x);
    let fy0 = f32(y);
    let w = params.h6 * fx0 + params.h7 * fy0 + params.h8;
    if w <= 1e-6 {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }
    let sx = (params.h0 * fx0 + params.h1 * fy0 + params.h2) / w;
    let sy = (params.h3 * fx0 + params.h4 * fy0 + params.h5) / w;

    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);

    let p00 = sample_pixel(x0, y0);
    let p10 = sample_pixel(x1, y0);
    let p01 = sample_pixel(x0, y1);
    let p11 = sample_pixel(x1, y1);

    let top = mix(p00, p10, fx);
    let bot = mix(p01, p11, fx);
    output[idx] = pack(mix(top, bot, fy));
}

fn sample_pixel(x: i32, y: i32) -> vec4<f32> {
    if x < 0 || y < 0 || x >= i32(params.width) || y >= i32(params.height) {
        return vec4<f32>(0.0);
    }
    let src_idx = u32(y) * params.width + u32(x);
    return unpack(input[src_idx]);
}
"
);

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MetalShotParams {
    width: u32,
    height: u32,
    op: u32,
    scale: u32,
    roi_x: u32,
    roi_y: u32,
    roi_w: u32,
    roi_h: u32,
    small_w: u32,
    small_h: u32,
    base: u32,
    radius: u32,
    tint_shot: [f32; 4],
}

// ─── New effect shaders ─────────────────────────────────────────────────────

const SHADER_MOSAIC: &str = "
struct Params {
    width: u32,
    height: u32,
    block_size: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let bx = u32(floor(f32(x) / params.block_size) * params.block_size);
    let by = u32(floor(f32(y) / params.block_size) * params.block_size);
    let cbx = min(bx, params.width - 1u);
    let cby = min(by, params.height - 1u);
    let src_idx = cby * params.width + cbx;
    output[idx] = input[src_idx];
}
";

const SHADER_MIRROR: &str = "
struct Params {
    width: u32,
    height: u32,
    horizontal: u32,
    vertical: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    var sx = x;
    var sy = y;
    if params.horizontal != 0u {
        sx = params.width - 1u - x;
    }
    if params.vertical != 0u {
        sy = params.height - 1u - y;
    }
    output[idx] = input[sy * params.width + sx];
}
";

const SHADER_DISPLACE: &str = concat!(shader_pixel_storage_wgsl!(), crate::displace::noise_wgsl!(), crate::displace::shader_wgsl!(), crate::displace::sample_wgsl!());

/// [`crate::warp`]'s pass (#2758): the shared warps, the Warp's own
/// choice among them, and the displace sampler.
/// [`crate::gradient_map`]'s pass (#2758).
const SHADER_GRADIENT_MAP: &str = concat!(shader_pixel_storage_wgsl!(), crate::gradient_map::shader_wgsl!());

const SHADER_WARP: &str = concat!(shader_pixel_storage_wgsl!(), crate::warp::functions_wgsl!(), crate::warp::shader_wgsl!(), crate::displace::sample_wgsl!());

const SHADER_REFLECTION: &str = concat!(shader_pixel_storage_wgsl!(), crate::displace::noise_wgsl!(), crate::reflection::shader_wgsl!());

const SHADER_LENS_DISTORTION: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    distortion: f32,
    scale: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn sample_pixel(sx: f32, sy: f32, w: u32, h: u32) -> vec4<f32> {
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);
    let cx0 = u32(clamp(x0, 0, i32(w) - 1));
    let cy0 = u32(clamp(y0, 0, i32(h) - 1));
    let cx1 = u32(clamp(x1, 0, i32(w) - 1));
    let cy1 = u32(clamp(y1, 0, i32(h) - 1));
    let c00 = unpack(input[cy0 * w + cx0]);
    let c10 = unpack(input[cy0 * w + cx1]);
    let c01 = unpack(input[cy1 * w + cx0]);
    let c11 = unpack(input[cy1 * w + cx1]);
    let top = mix(c00, c10, fx);
    let bot = mix(c01, c11, fx);
    return mix(top, bot, fy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = idx % w;
    let y = idx / w;
    let cx = f32(w) / 2.0;
    let cy = f32(h) / 2.0;
    let nx = (f32(x) - cx) / cx;
    let ny = (f32(y) - cy) / cy;
    let r2 = nx * nx + ny * ny;
    let factor = 1.0 + params.distortion * r2;
    let sx = (nx * factor / params.scale) * cx + cx;
    let sy = (ny * factor / params.scale) * cy + cy;
    output[idx] = pack(sample_pixel(sx, sy, w, h));
}
"
);

const SHADER_LETTERBOX: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    aspect_ratio: f32,
    bar_color: u32,
    opacity: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid_flat: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let gid = vec3<u32>(gid_flat.x + gid_flat.y * nwg.x * 256u, 0u, gid_flat.z);
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = idx % w;
    let y = idx / w;

    let frame_aspect = f32(w) / f32(h);
    let target_aspect = params.aspect_ratio;

    var bar_height = 0.0;
    var bar_width = 0.0;
    if frame_aspect > target_aspect {
        let visible_w = f32(h) * target_aspect;
        bar_width = (f32(w) - visible_w) / 2.0;
    } else {
        let visible_h = f32(w) / target_aspect;
        bar_height = (f32(h) - visible_h) / 2.0;
    }

    let in_bar = f32(y) < bar_height || f32(y) >= f32(h) - bar_height
              || f32(x) < bar_width || f32(x) >= f32(w) - bar_width;

    let v = unpack(input[idx]);
    if in_bar {
        // bar_color 0 = black, non-zero = white. Linear black = 0.0,
        // linear white = 1.0 (same as sRGB 0/1 endpoints).
        let bv = select(0.0, 1.0, params.bar_color != 0u);
        let op = params.opacity;
        let orig_rgb = vec3<f32>(v.x, v.y, v.z);
        let bar_rgb = vec3<f32>(bv);
        let mixed = orig_rgb * (1.0 - op) + bar_rgb * op;
        let na = v.w * (1.0 - op) + op;
        output[idx] = pack(vec4<f32>(mixed.x, mixed.y, mixed.z, na));
    } else {
        output[idx] = pack(v);
    }
}
"
);


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
