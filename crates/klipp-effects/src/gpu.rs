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

use crate::BuiltinEffect;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tracing::{debug, error, info};

mod readbacks;
mod alpha_masks;
mod title_styles;
pub use title_styles::{TitleStyle,TitleFace,TitleFill,TitleTexture,TitleGlyphPose,TitlePath};
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
    /// 0=circle, 1=rectangle, 2=linear_gradient, 3=bezier
    pub shape_type: u32,
    pub cx: f32,
    pub cy: f32,
    pub p1: f32,  // circle: radius, rect: width, grad: end_x
    pub p2: f32,  // rect: height, grad: end_y
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
        let title_style_pipeline = create_pipeline(&device, &pipeline_layout, "title_styles", include_str!("gpu/title_styles.wgsl"));

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

    /// Apply per-clip color grading on the GPU. Returns the processed RGBA data.
    /// Accepts the pre-converted lift/gamma/gain RGB tuples from wheel_offset_to_rgb,
    /// and temperature / tint as a white-balance matrix
    /// (`klipp_color::white_balance::white_balance_matrix`; identity for none).
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    pub fn apply_color_grade(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        exposure: f32,
        contrast: f32,
        // Already grade-encoded: `klipp_grade_model::encode_contrast_pivot`.
        contrast_pivot_encoded: f32,
        saturation: f32,
        white_balance: [[f32; 3]; 3],
        highlights: f32,
        shadows: f32,
        whites: f32,
        blacks: f32,
        lift: (f32, f32, f32),
        gamma: (f32, f32, f32),
        gain: (f32, f32, f32),
        offset: (f32, f32, f32),
        log_wheels: Option<[(f32, f32, f32); 3]>,
        wheel_shape: klipp_grade_model::WheelShape,
        hue: f32,
        global_mix: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuColorGradeParams {
            width,
            height,
            exposure,
            contrast,
            saturation,
            wb: flat3(white_balance),
            highlights,
            shadows,
            whites,
            blacks,
            lift_r: lift.0,
            lift_g: lift.1,
            lift_b: lift.2,
            gamma_r: gamma.0,
            gamma_g: gamma.1,
            gamma_b: gamma.2,
            gain_r: gain.0,
            gain_g: gain.1,
            gain_b: gain.2,
            offset_r: offset.0,
            offset_g: offset.1,
            offset_b: offset.2,
            hue,
            global_mix,
            contrast_pivot_encoded,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            encoding: self.grade_encoding,
            log_mode: log_wheels.is_some() as u32,
            log_wheels: flat_wheels(log_wheels),
            log_low_range: wheel_shape.low_range,
            log_high_range: wheel_shape.high_range,
            luma_mix: wheel_shape.luma_mix,
        });
        self.dispatch(&self.color_grade_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply color balance (white balance, hue rotation, global mix) on GPU.
    /// `white_balance` is temperature / tint as a matrix on linear light
    /// (`klipp_color::white_balance::white_balance_matrix`).
    pub fn apply_color_balance_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        white_balance: [[f32; 3]; 3],
        hue: f32,
        global_mix: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuColorBalanceParams {
            width, height, wb: flat3(white_balance), hue, global_mix,
            _pad0: 0, _pad1: 0,
        });
        self.dispatch(&self.color_balance_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply the HDR zones (#240) on the GPU, as
    /// [`crate::cpu_grade::hdr_zones`] does on the CPU.
    pub fn apply_hdr_zones_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32, terms: &klipp_grade_model::hdr_zones::Terms) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuHdrZonesParams::new(width, height, terms));
        self.dispatch(&self.hdr_zones_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply per-wheel saturation & brightness adjustments on GPU.
    pub fn apply_wheel_sat_bri_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        lift_sat: f32, lift_bri: f32,
        gamma_sat: f32, gamma_bri: f32,
        gain_sat: f32, gain_bri: f32,
        offset_sat: f32, offset_bri: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuWheelSatBriParams {
            width, height,
            lift_sat, lift_bri, gamma_sat, gamma_bri,
            gain_sat, gain_bri, offset_sat, offset_bri,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad0: 0,
        });
        self.dispatch(&self.wheel_sat_bri_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply RGB curves on GPU using pre-computed LUTs.
    /// `lut_data` must be 4×256 f32 values (master, red, green, blue).
    pub fn apply_rgb_curves_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        lut_data: &[f32; 1024], // 4 × 256
        preserve_luma: bool,
        mix: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        // Upload curve LUT data
        self.upload(&self.rgb_curves_lut_buf, 0, bytemuck::cast_slice(lut_data));

        self.write_params(&GpuRgbCurvesParams {
            width, height,
            preserve_luma: if preserve_luma { 1 } else { 0 },
            mix,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad0: 0,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        // Create bind group with 4 bindings (reuse huesat_bgl layout)
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("rgb_curves_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.rgb_curves_lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.rgb_curves_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    pub fn linear_rgba_to_u8_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32) -> Option<Vec<u8>> {
        if self.is_device_lost() { return None; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return Some(Vec::new()); }
        let in_bytes = pixel_count * 16;     // vec4<f32> per pixel
        let out_bytes = pixel_count * 4;     // u8 RGBA per pixel
        if linear_rgba.len() < (pixel_count * 4) as usize {
            // Caller passed a short buffer — fall through to CPU.
            return None;
        }

        // Reuse the standard chain input buffer for the linear data.
        // It's already the right size for the effects pipeline; if a
        // chain is mid-flight we'd clobber it, but this method is only
        // called from the engine's wrap path which runs serially with
        // the executor's flush_chain.
        self.ensure_buffers(in_bytes);

        // Resize the dedicated u8 output + staging buffers as needed.
        if self.bridge_u8_out_size < out_bytes {
            self.bridge_u8_out_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bridge_u8_out"),
                size: out_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            self.bridge_u8_out_size = out_bytes;
        }
        if self.bridge_u8_staging_size < out_bytes {
            self.bridge_u8_staging_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bridge_u8_staging"),
                size: out_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.bridge_u8_staging_size = out_bytes;
        }

        // Take refs after potential mutation above so the borrow
        // checker is happy.
        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        let out_buf = self.bridge_u8_out_buf.as_ref().expect("bridge_u8_out_buf just created");
        let staging = self.bridge_u8_staging_buf.as_ref().expect("bridge_u8_staging_buf just created");

        // Upload input + write params.
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        // Bind group: input(buf_a, read), output(out_buf, write), params(uniform).
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_u8_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });

        // Single submit: dispatch compute + copy_buffer_to_buffer for
        // readback. Same one-submit pattern as flush_chain — halves
        // NVIDIA Vulkan driver overhead vs two separate submits.
        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("bridge_u8_enc"));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("bridge_u8_pass"), timestamp_writes: None,
            });
            pass.set_pipeline(&self.bridge_u8_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        let mut readback = self.readbacks.begin(out_bytes);
        encoder.copy_buffer_to_buffer(out_buf, 0, staging, 0, out_bytes);
        self.submit(encoder);

        mark_gpu_stage(GpuStage::LinearRgbaToU8);
        let slice = staging.slice(..out_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        self.flush();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        // Non-blocking poll loop instead of `Maintain::Wait` — on a
        // UI-shared device, Wait never returns because eframe-wgpu
        // keeps the queue non-empty with continuous egui submits.
        // `poll_until_mapped` drives the map callback via Poll until
        // it fires (or times out at 2 s).
        let result = match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let data = slice.get_mapped_range();
                let result = data.to_vec();
                drop(data);
                staging.unmap();
                readback.succeeded = true;
                Some(result)
            }
            other => {
                self.mark_device_lost(&format!("bridge_u8 map_async failed: {other:?}"));
                None
            }
        };
        mark_gpu_stage(GpuStage::Idle);
        result
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

    /// Encode an externally-provided linear-f32 RGBA buffer directly
    /// into a `wgpu::Texture`. Same output as
    /// `encode_grade_to_texture` but input is a CPU slice uploaded
    /// to `buf_a` here — useful at the legacy post_process_rgba
    /// boundary that still downloads the grade chain's result as
    /// `Vec<f32>`. Later: the chain skips the download entirely and
    /// `encode_grade_to_texture` reads the slot directly.
    ///
    /// Same target constraints as `encode_grade_to_texture`:
    /// `Rgba8Unorm`, matching size, `width * 4 % 256 == 0`.
    pub fn linear_rgba_to_texture_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        target: &wgpu::Texture,
    ) -> bool {
        let pixel_count = (width as u64) * (height as u64);
        if linear_rgba.len() < (pixel_count * 4) as usize {
            return false;
        }
        let byte_size = pixel_count * 16;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        self.encode_grade_to_texture(true, width, height, target)
    }

    /// Encode the grade chain's final linear-f32 output (currently in
    /// `buf_a` when `slot_a = true`, `buf_b` otherwise) to sRGB-u8
    /// RGBA, then copy the result into the caller-supplied
    /// `wgpu::Texture`. No CPU readback — the texture is immediately
    /// bindable by another consumer on the same device
    /// (egui-wgpu's viewer, for example).
    ///
    /// The target texture must be `Rgba8Unorm` (not sRGB — our
    /// shader output bytes are already sRGB-encoded, we don't want a
    /// second decode-on-sample) with `TEXTURE_BINDING | COPY_DST`
    /// usage and dimensions `width × height`. Misconfigured targets
    /// return `false` without dispatching.
    ///
    /// Returns `false` on device-lost / misconfigured target; the
    /// caller's CPU-download + egui-upload fallback should kick in.
    pub fn encode_grade_to_texture(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        target: &wgpu::Texture,
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        let out_bytes = pixel_count * 4;

        // Target validation — surface misconfigurations loudly
        // instead of corrupting the display.
        let size = target.size();
        if size.width != width || size.height != height {
            tracing::warn!(
                "encode_grade_to_texture: target size {}x{} != requested {width}x{height}",
                size.width, size.height,
            );
            return false;
        }
        if target.format() != wgpu::TextureFormat::Rgba8Unorm {
            tracing::warn!(
                "encode_grade_to_texture: target format {:?} must be Rgba8Unorm",
                target.format(),
            );
            return false;
        }
        // wgpu's `copy_buffer_to_texture` requires `bytes_per_row` to
        // be a multiple of `COPY_BYTES_PER_ROW_ALIGNMENT` (256).
        // Our output buffer is tightly packed (width * 4 bytes/row),
        // so any width whose byte pitch isn't a multiple of 256
        // would silently corrupt the copy under the release-build
        // validation disable. Reject up front.
        let bytes_per_row = width * 4;
        if !bytes_per_row.is_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) {
            tracing::warn!(
                "encode_grade_to_texture: width {width} (pitch {bytes_per_row} B) \
                 not a multiple of COPY_BYTES_PER_ROW_ALIGNMENT={}. \
                 Real video frames ≥ 64 px wide always meet this.",
                wgpu::COPY_BYTES_PER_ROW_ALIGNMENT,
            );
            return false;
        }

        // Need our reusable u8 output buffer at the right size.
        if self.bridge_u8_out_size < out_bytes {
            self.bridge_u8_out_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bridge_u8_out"),
                size: out_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            self.bridge_u8_out_size = out_bytes;
        }

        let src_buf = if slot_a {
            self.buf_a.as_ref()
        } else {
            self.buf_b.as_ref()
        }.expect("chain buffers must be initialised before encode_grade_to_texture");
        let out_buf = self.bridge_u8_out_buf.as_ref().unwrap();

        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("encode_grade_to_texture_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("encode_grade_to_texture"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.bridge_u8_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        // Copy the packed-u32 buffer straight into the texture. The
        // buffer layout matches `Rgba8Unorm`'s row-major RGBA bytes
        // byte-for-byte (R in bits 0-7 of each u32, then G, B, A).
        encoder.copy_buffer_to_texture(
            wgpu::ImageCopyBuffer {
                buffer: out_buf,
                layout: wgpu::ImageDataLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: Some(height),
                },
            },
            wgpu::ImageCopyTexture {
                texture: target,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        );
        self.submit(encoder);
        true
    }

    /// GPU bridge: packed sRGB-u8 RGBA (`array<u32>`) → linear-f32
    /// RGBA (`array<vec4<f32>>`). Reads an *external* input buffer
    /// (typically the output of `convert_*_to_rgba_on_gpu`) and writes
    /// the decoded linear pixels into the pipeline's chain slot
    /// `buf_a`, leaving the chain primed so downstream grade-node
    /// dispatches can consume it.
    ///
    /// Fully GPU-resident — no CPU bytes touched, no
    /// `queue.write_buffer` round-trip. This is the operation that
    /// eliminates the ~5 ms PCIe upload the CPU path paid on every
    /// frame.
    ///
    /// Returns `false` if the GPU device is lost or pre-conditions
    /// aren't met (chain buffers not allocated, input buffer undersized
    /// for the declared dimensions). Callers should fall back to the
    /// CPU `u8_rgba_to_linear_f32` → `upload_chain_input_linear`
    /// path in that case.
    pub fn bridge_u8_to_linear_into_chain(
        &mut self,
        input_u8: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        let linear_byte_size = pixel_count * 16;
        self.ensure_buffers(linear_byte_size);

        // Input must have at least 4 bytes per pixel.
        if input_u8.size() < pixel_count * 4 { return false; }

        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_u8_to_linear_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                // binding 0: input u8 buffer (external, read-only)
                wgpu::BindGroupEntry { binding: 0, resource: input_u8.as_entire_binding() },
                // binding 1: output linear buffer (chain's buf_a, read-write)
                wgpu::BindGroupEntry { binding: 1, resource: buf_a.as_entire_binding() },
                // binding 2: params uniform
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                // binding 3: the codes' decode table
                wgpu::BindGroupEntry { binding: 3, resource: self.srgb8_lut.as_entire_binding() },
            ],
        });

        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("bridge_u8_to_linear"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.bridge_u8_to_linear_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        true
    }

    /// [`Self::bridge_u8_to_linear_into_chain`] from a frame of another
    /// size: `src_w` by `src_h` sRGB bytes fitted into the chain's
    /// `width` by `height` the way the CPU compositor fits a source —
    /// one scale factor, centred, the rest transparent; an area average
    /// when shrinking by two or more, bilinear otherwise, both in linear
    /// light.
    ///
    /// A 1440p overlay on a 1080p timeline used to send every frame it
    /// was on down to the CPU path, a readback and an upload each: 24 ms
    /// a frame against one.
    pub fn bridge_u8_fit_to_linear_into_chain(
        &mut self,
        input_u8: &wgpu::Buffer,
        src_w: u32,
        src_h: u32,
        width: u32,
        height: u32,
    ) -> bool {
        if self.is_device_lost() || src_w == 0 || src_h == 0 || width == 0 || height == 0 {
            return false;
        }
        if input_u8.size() < (src_w as u64) * (src_h as u64) * 4 {
            return false;
        }
        let pixel_count = (width as u64) * (height as u64);
        self.ensure_buffers(pixel_count * 16);
        let (dst_w, dst_h, x_off, y_off) = fit_rect(src_w, src_h, width, height);
        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.write_params(&BridgeU8FitParams { src_w, src_h, out_w: width, out_h: height, dst_w, dst_h, x_off, y_off });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_u8_fit_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_u8.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.srgb8_lut.as_entire_binding() },
            ],
        });
        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("bridge_u8_fit_to_linear"));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.bridge_u8_fit_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        true
    }

    /// The chain's graded frame straight to 8-bit 4:2:0 planes: Y, then
    /// Cb, then Cr, tightly packed, ready for the encoder.
    ///
    /// The whole point is what does *not* happen — the linear frame never
    /// comes down. At 4K that is 132 MB replaced by 12.4 MB, and the
    /// per-pixel CPU display-and-convert pass replaced by nothing.
    ///
    /// `None` when this is not the right route and the caller must use
    /// `encode_linear_direct`: no GPU, a device lost, dimensions the
    /// block layout cannot tile (width not a multiple of 8, height odd),
    /// or a transform the shader does not implement. The `tone_map_op`
    /// encoding is 0 none, 1 Reinhard, 2 Hable, 3 the whole ACES Rec.709
    /// output transform; the caller has already established which of
    /// those its delivery is.
    ///
    /// `display_tables` is whichever baked table set `target` asks for —
    /// `klipp_color::aces::aces_rec709_gpu_data` for `tone_map_op == 3`,
    /// `klipp_color::gamut::bt2020_to_bt709_gpu_data` for `gamut_op == 1`
    /// — and empty when the transform needs none. Passed in rather than
    /// built here because this crate deliberately does not depend on
    /// `klipp-color` outside its tests.
    pub fn encode_chain_to_yuv420(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        target: Yuv420Target,
        display_tables: &[f32],
    ) -> Option<Vec<u8>> {
        self.encode_chain_to_yuv420_inner(slot_a, width, height, target, display_tables)
            .map(|(bytes, _)| bytes)
    }

    /// [`Self::encode_chain_to_yuv420`], and the frame's light levels —
    /// what HDR10's MaxCLL and MaxFALL are made of.
    ///
    /// A PQ delivery needs them, and they used to be the reason it could
    /// not take this path at all: the twin measures light per pixel while
    /// it encodes, off the linear frame this path exists not to download.
    /// The shader measures the same thing where the pixels are, reducing
    /// to one `(max, sum)` a workgroup, and the host finishes the
    /// reduction over those few thousand pairs.
    ///
    /// `None` for the same reasons as the unmeasured form, and also when
    /// the target is not a PQ delivery — the shader writes no light on
    /// any other path, so a caller asking for it has the wrong one.
    pub fn encode_chain_to_yuv420_measured(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        target: Yuv420Target,
        display_tables: &[f32],
    ) -> Option<(Vec<u8>, GpuFrameLight)> {
        if target.hdr_op != 1 {
            return None;
        }
        let (bytes, light) =
            self.encode_chain_to_yuv420_inner(slot_a, width, height, target, display_tables)?;
        Some((bytes, light?))
    }

    fn encode_chain_to_yuv420_inner(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        target: Yuv420Target,
        display_tables: &[f32],
    ) -> Option<(Vec<u8>, Option<GpuFrameLight>)> {
        let Yuv420Target {
            tone_map_op,
            // Names which ODT blob the caller built; nothing here reads
            // it, and the shader takes the transform from the blob.
            odt_tag: _,
            gamut_op,
            hdr_op,
            peak_nits,
            scene,
            luma_coefs,
            semi_planar,
            bits,
            sample_shift,
        } = target;
        let (scene_op, scene_kind, scene_scale, scene_exposure) = match scene {
            Some(s) => (if s.display_light { 2 } else { 1 }, s.kind, s.scale, s.exposure),
            None => (0, 0, 1.0, 1.0),
        };
        if self.is_device_lost()
            || !width.is_multiple_of(8)
            || !height.is_multiple_of(2)
            || tone_map_op > 3
            || gamut_op > 1
            || hdr_op > 2
            // All three read binding 3, so one transform at a time.
            || (gamut_op == 1 && tone_map_op == 3)
            || (hdr_op != 0 && (gamut_op == 1 || tone_map_op == 3))
            || !matches!(bits, 8 | 10)
        {
            return None;
        }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 {
            return None;
        }
        // Two bytes a sample at ten bits, one at eight.
        let sample_bytes = if bits == 8 { 1 } else { 2 };
        let luma_bytes = pixel_count * sample_bytes;
        let chroma_bytes = (pixel_count / 4) * sample_bytes;
        let total = luma_bytes + 2 * chroma_bytes;

        // Before `source` borrows self: uploading to the fourth binding
        // needs &mut, and every table set is cached for the export's life
        // because none changes frame to frame.
        if tone_map_op == 3 {
            if display_tables.is_empty() {
                return None;
            }
            self.ensure_aces_data(display_tables);
        } else if gamut_op == 1 && !self.ensure_bt2020_gamut_data(display_tables) {
            return None;
        } else if (hdr_op != 0 || scene_op == 2) && !self.ensure_hdr_encode_data(display_tables) {
            // The display-light branch reads the working space's luma
            // weights out of the same blob (the HLG OOTF needs them), so
            // it wants the blob even when the delivery is SDR.
            return None;
        }

        // One `(max, sum)` pair a workgroup, which only a PQ delivery
        // writes. The dispatch shape below has to match this exactly, so
        // both read the same expression.
        let (wg_x, wg_y) = ((width / 8).div_ceil(8), (height / 2).div_ceil(8));
        let measuring = hdr_op == 1;
        let light_floats = (wg_x as u64) * (wg_y as u64) * 2;
        let light_buf = measuring.then(|| {
            self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("yuv420_light"),
                size: light_floats * 4,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        });

        let source = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() };
        let source = source?;
        // The planes need their own buffer: `buf_c` is a chain slot and
        // may hold a frame the graph still wants.
        let planes = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuv420_planes"),
            size: total,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        self.write_params(&Yuv420Params {
            width,
            height,
            tone_map_op,
            chroma_word_offset: (luma_bytes / 4) as u32,
            kr: luma_coefs[0],
            kg: luma_coefs[1],
            kb: luma_coefs[2],
            semi_planar: semi_planar as u32,
            bits,
            sample_shift,
            gamut_op,
            hdr_op,
            peak_nits,
            scene_op,
            scene_kind,
            scene_scale,
            scene_exposure,
            _pad2: 0,
            _pad3: 0,
        });
        let tables: &wgpu::Buffer = if tone_map_op == 3 {
            &self.aces_rec709_data.as_ref()?.buf
        } else if gamut_op == 1 {
            &self.bt2020_gamut_data.as_ref()?.buf
        } else if hdr_op != 0 || scene_op == 2 {
            &self.hdr_encode_data.as_ref()?.buf
        } else {
            &self.no_display_tables
        };
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("linear_to_yuv420_bg"),
            layout: &self.display_tables_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: source.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: planes.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: tables.as_entire_binding() },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: light_buf
                        .as_ref()
                        .unwrap_or(&self.no_light)
                        .as_entire_binding(),
                },
            ],
        });

        // Ride the open chain encoder when there is one, so the whole
        // frame is a single submit; NVIDIA's driver charges 5-15 ms for
        // each one.
        let mut encoder = self.chain_encoder.take().unwrap_or_else(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("yuv420") })
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.linear_to_yuv420_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(wg_x, wg_y, 1);
        }
        self.submit(encoder);

        // `staging_read` is sized for a whole linear frame, which is ten
        // times these planes, so the chain having run means it fits.
        let bytes = self.with_downloaded(&planes, total, |bytes| bytes.to_vec())?;
        let light = match light_buf {
            // The per-workgroup pairs, finished on the CPU. Summed in f64
            // the way the twin sums its per-pixel nits, over a few
            // thousand partials rather than eight million pixels.
            Some(buf) => self.with_downloaded(&buf, light_floats * 4, |raw| {
                let pairs: &[f32] = bytemuck::cast_slice(&raw[..(light_floats as usize) * 4]);
                let mut max_nits = 0.0f32;
                let mut sum_nits = 0.0f64;
                for p in pairs.chunks_exact(2) {
                    max_nits = max_nits.max(p[0]);
                    sum_nits += p[1] as f64;
                }
                GpuFrameLight { max_nits, sum_nits, pixels: pixel_count }
            }),
            None => None,
        };
        Some((bytes, light))
    }

    /// The chain's graded frame → HDR monitoring's light map: the
    /// brightest channel of every `step`-th pixel, in cd/m². Row-major,
    /// `width.div_ceil(step)` by `height.div_ceil(step)`.
    ///
    /// `to_2020` is the working space's basis into BT.2020, packed by
    /// `color_wgsl::pack_primaries_matrix` — a delivery is BT.2020
    /// whatever the project works in, and measured in a narrower basis the
    /// same BT.2020 red reads 67% high.
    ///
    /// This is the piece that lets HDR monitoring keep the fast path: the
    /// readout needs the light, and the light needs the linear frame, so
    /// without it the frame has to come down. A quarter-resolution map is
    /// 2 MB at 4K against the frame's 132.
    pub fn nit_map_of_chain(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        step: u32,
        to_2020: [[f32; 4]; 3],
    ) -> Option<(Vec<f32>, u32, u32)> {
        if self.is_device_lost() || width == 0 || height == 0 {
            return None;
        }
        let step = step.max(1);
        let (mw, mh) = (width.div_ceil(step), height.div_ceil(step));
        let bytes = (mw as u64) * (mh as u64) * 4;
        if self.hdr_nit_buf.as_ref().is_none_or(|b| b.size() < bytes) {
            self.hdr_nit_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hdr_nit_map"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            self.hdr_nit_staging = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("hdr_nit_staging"),
                size: bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        self.write_params(&NitMapParams { width, height, step, map_width: mw, to_2020 });
        let src = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }?;
        let out = self.hdr_nit_buf.as_ref()?;
        let staging = self.hdr_nit_staging.as_ref()?;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hdr_nit_map_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("hdr_nit_map") });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.hdr_nit_map_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((mw * mh).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        let mut readback = self.readbacks.begin(bytes);
        enc.copy_buffer_to_buffer(out, 0, staging, 0, bytes);
        self.submit(enc);

        let slice = staging.slice(..bytes);
        self.flush();
        slice.map_async(wgpu::MapMode::Read, |_| {});
        self.device.poll(wgpu::Maintain::Wait);
        let data = slice.get_mapped_range();
        let map: Vec<f32> = bytemuck::cast_slice(&data).to_vec();
        drop(data);
        staging.unmap();
        readback.succeeded = true;
        Some((map, mw, mh))
    }

    /// The chain's graded AcesCG frame → display-encoded Rec.709 SDR
    /// bytes, the ACES output transform run on the GPU.
    ///
    /// Replaces `klipp_color::aces::acescg_to_rec709_sdr` per pixel on
    /// the CPU, which is 88 ms of a 4K frame — the reason an AcesCG
    /// project exported six times slower than a Rec.709 one.
    ///
    /// `aces_data` is `klipp_color::aces::aces_rec709_gpu_data` — the
    /// matrices and tone curves. It is passed in rather than built here
    /// because this crate deliberately does not depend on
    /// `klipp-color` outside its tests: the colour science lives there,
    /// and this is the layer that runs things on a GPU. Uploaded once;
    /// the data is fixed for this ODT.
    ///
    /// `None` when there is no GPU or the device is lost; callers keep
    /// the CPU transform.
    pub fn encode_chain_to_aces_rec709_u8(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        aces_data: &[f32],
    ) -> Option<Vec<u8>> {
        if self.is_device_lost() {
            return None;
        }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 {
            return None;
        }
        self.ensure_aces_data(aces_data);
        let out_bytes = pixel_count * 4;

        let source = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }?;
        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("aces_rec709_out"),
            size: out_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        let data = &self.aces_rec709_data.as_ref()?.buf;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("aces_rec709_bg"),
            layout: &self.display_tables_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: source.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: data.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: self.no_light.as_entire_binding() },
            ],
        });

        let mut encoder = self.chain_encoder.take().unwrap_or_else(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("aces") })
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.aces_rec709_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((pixel_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.submit(encoder);

        self.with_downloaded(&out, out_bytes, |bytes| bytes.to_vec())
    }

    /// Upload the ACES matrices and tone curves, once.
    fn ensure_aces_data(&mut self, data: &[f32]) {
        Self::ensure_tables(
            &self.device,
            &self.queue,
            &mut self.aces_rec709_data,
            "aces_odt_data",
            data,
        );
    }

    /// Upload `data` to `slot`, reusing what is there when the contents
    /// are the same.
    ///
    /// Keyed on the contents and not merely on "is something uploaded",
    /// which is how it started: an output transform's numbers are fixed
    /// for a *deliverable*, not for a pipeline, and the four ACES ODTs
    /// share one slot. Skipping the second upload rendered the second
    /// deliverable with the first one's matrices and gammas — the same
    /// picture, silently, which is how
    /// `every_acescg_odt_matches_the_cpu` found this. Comparing a few
    /// thousand floats is nothing beside a frame.
    fn ensure_tables(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        slot: &mut Option<CachedTables>,
        label: &'static str,
        data: &[f32],
    ) {
        if let Some(c) = slot {
            if c.data == data {
                return;
            }
        }
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (data.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buf, 0, bytemuck::cast_slice(data));
        *slot = Some(CachedTables { buf, data: data.to_vec() });
    }

    /// Upload `klipp_color::gamut::bt2020_to_bt709_gpu_data` once. The
    /// tables are fixed for this gamut pair, so later calls are free.
    ///
    /// `false` for a buffer the shader would read past the end of. The
    /// WGSL takes its grid size from the first two entries and indexes
    /// the reach and cusp tables off it, so a short or truncated buffer
    /// is not merely wrong pixels — the length the header implies is
    /// recomputed here, the one place the host can check it. That header
    /// arithmetic mirrors `gamut::gpu_layout`, which
    /// `gpu_gamut_data_length_is_what_the_header_says` in `klipp-color`
    /// holds the two to.
    fn ensure_bt2020_gamut_data(&mut self, data: &[f32]) -> bool {
        // HEADER_LEN 2 + MATRICES_LEN 27 + WHITE_LEN 3 = 32 before the
        // first table; then two reach tables and the cusps.
        let (l_steps, h_steps) = match (data.first(), data.get(1)) {
            (Some(&l), Some(&h)) if l >= 1.0 && h >= 1.0 && l < 4096.0 && h < 4096.0 => {
                (l as usize, h as usize)
            }
            _ => return false,
        };
        let reach = l_steps * h_steps + l_steps;
        if data.len() < 32 + 2 * reach + h_steps * 2 {
            return false;
        }
        Self::ensure_tables(
            &self.device,
            &self.queue,
            &mut self.bt2020_gamut_data,
            "bt2020_gamut_data",
            data,
        );
        true
    }

    /// Upload `klipp_color::hdr::hdr_encode_gpu_data` once: the basis
    /// matrix, the working space's luma weights and the PQ table.
    ///
    /// `false` for a buffer the shader would read past the end of. The
    /// WGSL takes the PQ table's length from the first entry and indexes
    /// off it, so the length that header implies is checked here — the
    /// one place the host can. `gpu_hdr_data_length_is_what_the_header_says`
    /// in `klipp-color` holds the two layouts together, since this crate
    /// cannot see `hdr::gpu_layout`.
    fn ensure_hdr_encode_data(&mut self, data: &[f32]) -> bool {
        // 1 header + 9 matrix + 3 luma = 13 before the table.
        let entries = match data.first() {
            Some(&n) if (2.0..65536.0).contains(&n) => n as usize,
            _ => return false,
        };
        if data.len() < 13 + entries {
            return false;
        }
        Self::ensure_tables(
            &self.device,
            &self.queue,
            &mut self.hdr_encode_data,
            "hdr_encode_data",
            data,
        );
        true
    }

    /// The chain's graded BT.2020 frame → display bytes: the basis
    /// change, the BT.2407-style gamut compression and the sRGB encode.
    ///
    /// Replaces `display_transform_ws(.., Bt2020Linear)` on the CPU: 51 ms
    /// of a saturated 4K frame, plus 14 ms to bring the floats down, for
    /// 4.1 ms here. No tone-map curve: the caller only takes this with
    /// `ToneMapOp::None`, as it does for BT.709.
    ///
    /// `gamut_data` is `klipp_color::gamut::bt2020_to_bt709_gpu_data`,
    /// passed in for the same reason the ACES data is — this crate does
    /// not depend on `klipp-color` outside its tests.
    pub fn encode_chain_to_bt2020_display_u8(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        gamut_data: &[f32],
    ) -> Option<Vec<u8>> {
        if self.is_device_lost() {
            return None;
        }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 || !self.ensure_bt2020_gamut_data(gamut_data) {
            return None;
        }

        let out_bytes = pixel_count * 4;
        let source = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }?;
        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("bt2020_display_out"),
            size: out_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        let data = &self.bt2020_gamut_data.as_ref()?.buf;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bt2020_display_bg"),
            layout: &self.display_tables_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: source.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: data.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: self.no_light.as_entire_binding() },
            ],
        });

        let mut encoder = self.chain_encoder.take().unwrap_or_else(|| {
            self.device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("bt2020") })
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.bt2020_display_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((pixel_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.submit(encoder);

        self.with_downloaded(&out, out_bytes, |bytes| bytes.to_vec())
    }

    /// Put a frame that is already linear working-space f32 on the GPU
    /// into the chain's input slot. A buffer copy, not a conversion —
    /// there is nothing to expand.
    ///
    /// GPU-local, so it is not the kind of copy the bridges exist to
    /// avoid: 133 MB at 4K moves in well under a millisecond on device
    /// memory, against ~10 ms for the same bytes over PCIe.
    ///
    /// `false` when the device is lost or `input` is too small; callers
    /// fall back to handing the pixels over on the CPU.
    pub fn copy_linear_into_chain(
        &mut self,
        input: &wgpu::Buffer,
        width: u32,
        height: u32,
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        let byte_size = pixel_count * 16;
        self.ensure_buffers(byte_size);
        if input.size() < byte_size { return false; }

        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        let mut encoder = self.encoder(Some("copy_linear_into_chain"));
        encoder.copy_buffer_to_buffer(input, 0, buf_a, 0, byte_size);
        self.submit(encoder);
        true
    }

    /// [`Self::bridge_u8_to_linear_into_chain`] one sample wider:
    /// sRGB-encoded RGBA64 → linear working-space f32 in `buf_a`, the
    /// source primaries folded in.
    ///
    /// This is the export's input transform. Its scaler produces RGBA64
    /// even from an 8-bit source — YUV→RGB is a matrix, and its output
    /// genuinely needs more than 8 bits — so narrowing on the way here
    /// would quantise the delivered frame. Instead the scaler's own
    /// buffer is uploaded as it stands and expanded in place: half the
    /// bus traffic of the linear f32 buffer it replaces, and none of the
    /// per-pixel CPU pass that used to build it.
    ///
    /// `false` when the device is lost or `input` is too small; callers
    /// fall back to expanding on the CPU.
    pub fn bridge_rgba64_to_linear_into_chain(
        &mut self,
        input_rgba64: &wgpu::Buffer,
        width: u32,
        height: u32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        self.ensure_buffers(pixel_count * 16);

        // Four u16 per pixel — twice the u8 bridge's input stride.
        if input_rgba64.size() < pixel_count * 8 { return false; }

        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.write_params(&Rgba64BridgeParams {
            width,
            height,
            _pad0: 0,
            _pad1: 0,
            source_to_working,
        });

        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_rgba64_to_linear_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_rgba64.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.srgb16_lut.as_entire_binding() },
            ],
        });

        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.encoder(Some("bridge_rgba64_to_linear"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.bridge_rgba64_to_linear_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        true
    }

    /// GPU input transform for HDR: packed RGB10 code values (the P010
    /// decoder's `hdr_codes`) → scene-linear working-space f32 in `buf_a`,
    /// leaving the chain primed exactly as `bridge_u8_to_linear_into_chain`
    /// does for SDR. `transfer_id` is a `klipp_core::color_ids` PQ/HLG id.
    ///
    /// Returns `false` when the device is lost or `codes` is too small;
    /// callers fall back to the CPU input transform.
    pub fn bridge_hdr10_to_linear_into_chain(
        &mut self,
        codes: &wgpu::Buffer,
        width: u32,
        height: u32,
        transfer_id: u32,
        scene_scale: f32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        self.bridge_scene_codes_into_chain(codes, false, width, height, transfer_id, scene_scale, source_to_working)
    }

    /// [`Self::bridge_hdr10_to_linear_into_chain`] for 16-bit codes
    /// (`klipp_color::hdr::pack_rgb16`, eight bytes a pixel): a source
    /// deeper than 10 bits, at its own precision (#1226).
    pub fn bridge_hdr16_to_linear_into_chain(
        &mut self,
        codes: &wgpu::Buffer,
        width: u32,
        height: u32,
        transfer_id: u32,
        scene_scale: f32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        self.bridge_scene_codes_into_chain(codes, true, width, height, transfer_id, scene_scale, source_to_working)
    }

    #[allow(clippy::too_many_arguments)]
    fn bridge_scene_codes_into_chain(
        &mut self,
        codes: &wgpu::Buffer,
        deep: bool,
        width: u32,
        height: u32,
        transfer_id: u32,
        scene_scale: f32,
        source_to_working: [[f32; 4]; 3],
    ) -> bool {
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        self.ensure_buffers(pixel_count * 16);
        if codes.size() < pixel_count * if deep { 8 } else { 4 } { return false; }

        let buf_a = self.buf_a.as_ref().expect("ensure_buffers established buf_a");
        self.write_params(&Hdr10BridgeParams {
            width, height, transfer_id, scene_scale, source_to_working,
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_hdr10_to_linear_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: codes.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let mut encoder = self.encoder(Some("bridge_hdr10_to_linear"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(if deep { &self.bridge_hdr16_pipeline } else { &self.bridge_hdr10_pipeline });
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((pixel_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.submit(encoder);
        true
    }

    /// Encode the chain's final linear output (`buf_a` when `slot_a`,
    /// else `buf_b`) to display sRGB u8 on the GPU and read back only
    /// that: 4 bytes a pixel instead of the 16 a linear download costs —
    /// 33 MB at 4K rather than 132. `hdr_tonemap` applies the HDR display
    /// tone map first, making this an HDR clip's last step.
    ///
    /// Only valid where the CPU display path would be
    /// `display_transform_ws(ToneMapOp::None, Rec709/BT2020 linear)`; the
    /// caller gates on that. `None` on device loss or a failed readback.
    pub fn encode_chain_to_display_u8(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
    ) -> Option<Vec<u8>> {
        self.encode_chain_to_display_u8_view(slot_a, width, height, tonemap_exposure, false)
    }

    /// [`Self::encode_chain_to_display_u8`], and for `hdr_preview` the
    /// frame is HDR display light rather than SDR linear: the light above
    /// reference white rolls into the top so its structure survives a
    /// screen that cannot show it.
    ///
    /// That is HDR monitoring's `Preview` mode, and it exists here so
    /// turning monitoring on does not cost the frame its place on the GPU.
    /// `klipp_color::hdr_view::preview_frame` is the twin.
    pub fn encode_chain_to_display_u8_view(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
        hdr_preview: bool,
    ) -> Option<Vec<u8>> {
        self.encode_chain_to_display_u8_monitor(
            slot_a, width, height, tonemap_exposure, hdr_preview, None,
        )
    }

    /// [`Self::encode_chain_to_display_u8_view`], and for a
    /// scene-referred clip the display-light pass it ends with — which is
    /// what HDR monitoring wants to see rather than the SDR tone map.
    ///
    /// `scene` is `(HdrTransfer::display_light_kind, scene_scale,
    /// peak_nits, working luma weights)`; `None` for a display-referred
    /// clip, which has no such pass.
    pub fn encode_chain_to_display_u8_monitor(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
        hdr_preview: bool,
        scene: Option<(u32, f32, f32, [f32; 3])>,
    ) -> Option<Vec<u8>> {
        self.encode_chain_to_display(slot_a, width, height, tonemap_exposure, hdr_preview, scene, None, true)
            .map(Option::unwrap_or_default)
    }

    /// The display encode of [`Self::encode_chain_to_display_u8_monitor`],
    /// delivered where the caller wants it: into `target` — an
    /// `Rgba8Unorm` texture of the frame's size with `STORAGE_BINDING`,
    /// the viewer's own — and, when `want_bytes`, to the CPU as well.
    /// Neither costs the other anything but the texel store; with no
    /// bytes wanted the frame never leaves the GPU, which is the point.
    ///
    /// `None` on a lost device or a target of the wrong shape; otherwise
    /// the bytes when they were wanted.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_chain_to_display(
        &mut self,
        slot_a: bool,
        width: u32,
        height: u32,
        tonemap_exposure: Option<f32>,
        hdr_preview: bool,
        scene: Option<(u32, f32, f32, [f32; 3])>,
        target: Option<&wgpu::Texture>,
        want_bytes: bool,
    ) -> Option<Option<Vec<u8>>> {
        if self.is_device_lost() { return None; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return Some(want_bytes.then(Vec::new)); }
        if let Some(t) = target {
            let size = t.size();
            if size.width != width || size.height != height || t.format() != wgpu::TextureFormat::Rgba8Unorm
                || !t.usage().contains(wgpu::TextureUsages::STORAGE_BINDING)
            {
                tracing::warn!(
                    "encode_chain_to_display: target {}x{} {:?} {:?} cannot take a {width}x{height} frame",
                    size.width, size.height, t.format(), t.usage(),
                );
                return None;
            }
        }
        let out_bytes = pixel_count * 4;
        if self.bridge_u8_out_size < out_bytes {
            self.bridge_u8_out_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("bridge_u8_out"),
                size: out_bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }));
            self.bridge_u8_out_size = out_bytes;
        }
        if self.display_u8_staging.as_ref().is_none_or(|b| b.size() < out_bytes) {
            self.display_u8_staging = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("display_u8_staging"),
                size: out_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let src = if slot_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }?;
        let out = self.bridge_u8_out_buf.as_ref().unwrap();
        let staging = self.display_u8_staging.as_ref().unwrap();
        self.write_params(&DisplayU8Params {
            width,
            height,
            hdr_tonemap: tonemap_exposure.is_some() as u32,
            exposure: tonemap_exposure.unwrap_or(0.0),
            view_op: hdr_preview as u32,
            // 3 means "no display-light pass"; the kinds are 0, 1, 2.
            scene_kind: scene.map_or(3, |s| s.0),
            scene_scale: scene.map_or(1.0, |s| s.1),
            peak_nits: scene.map_or(0.0, |s| s.2),
            luma: scene.map_or([0.2126, 0.7152, 0.0722, 0.0], |s| {
                [s.3[0], s.3[1], s.3[2], 0.0]
            }),
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("linear_to_display_u8_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
            ],
        });
        let mut encoder = self.encoder(Some("linear_to_display_u8"));
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.display_u8_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((pixel_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        if let Some(t) = target {
            // Its own params buffer: the shared one is read by the encode
            // pass ahead of this in the same encoder.
            let view = t.create_view(&wgpu::TextureViewDescriptor::default());
            let tex_params = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("u8_to_texture_params"),
                size: std::mem::size_of::<BridgeU8Params>() as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.queue.write_buffer(&tex_params, 0, bytemuck::bytes_of(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 }));
            let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("u8_to_texture_bg"),
                layout: &self.u8_to_texture_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: out.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 2, resource: tex_params.as_entire_binding() },
                ],
            });
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: Some("u8_to_texture"), timestamp_writes: None },
            );
            pass.set_pipeline(&self.u8_to_texture_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(width.div_ceil(16), height.div_ceil(16), 1);
        }
        let mut readback = want_bytes.then(|| self.readbacks.begin(out_bytes));
        if want_bytes {
            encoder.copy_buffer_to_buffer(out, 0, staging, 0, out_bytes);
        }
        self.submit(encoder);
        // The frame is encoded: everything recorded goes to the queue.
        self.stack_end();
        if !want_bytes {
            return Some(None);
        }

        let slice = staging.slice(..out_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        self.flush();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let bytes = slice.get_mapped_range().to_vec();
                staging.unmap();
                if let Some(attempt) = &mut readback { attempt.succeeded = true; }
                Some(Some(bytes))
            }
            other => {
                self.mark_device_lost(&format!("display_u8 map_async failed: {other:?}"));
                None
            }
        }
    }

    pub fn apply_vibrance_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32, amount: f32) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuVibranceParams { width, height, amount, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        self.dispatch(&self.vibrance_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply 12-sector color warper on GPU.
    pub fn apply_color_warper_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32, hue_shifts: &[f32; 12], sat_shifts: &[f32; 12]) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuColorWarperParams {
            width, height, _pad0: 0, _pad1: 0,
            hue_shifts: *hue_shifts, sat_shifts: *sat_shifts,
        });
        self.dispatch(&self.color_warper_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }


    /// Apply clarity (midtone unsharp mask) on GPU using 3-pass approach.
    pub fn apply_clarity_gpu(&mut self, linear_rgba: &[f32], width: u32, height: u32, amount: f32, radius: u32) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        let _luma_size = (width * height * 4) as u64; // store luma as u32 per pixel
        self.ensure_buffers(byte_size);
        // Need a third buffer for blur intermediate
        if self.buf_c.is_none() || self.current_buf_size < byte_size {
            self.buf_c = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("buf_c"), size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        let buf_c = self.buf_c.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let radius = radius.max(1);

        // Pass 1: horizontal blur (input RGBA → buf_b luma)
        self.write_params(&GpuClarityBlurParams { width, height, radius, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        self.dispatch(&self.clarity_blur_h_pipeline, buf_a, buf_b, workgroups);

        // Pass 2: vertical blur (buf_b luma → buf_c blurred luma)
        self.dispatch(&self.clarity_blur_v_pipeline, buf_b, buf_c, workgroups);

        // Pass 3: apply unsharp mask (input RGBA + blurred luma → output RGBA)
        self.write_params(&GpuClarityApplyParams { width, height, amount, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clarity_apply_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: buf_c.as_entire_binding() },
            ],
        });
        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.clarity_apply_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply bilateral denoise on GPU. Used by the color grading Denoise node.
    pub fn apply_denoise_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        spatial_sigma: f32,
        luma_sigma: f32,
        chroma_sigma: f32,
        max_kernel: u32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&DenoiseParams {
            width, height, spatial_sigma, luma_sigma,
            luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], max_kernel,
            chroma_sigma, _pad: [0; 3],
        });
        self.dispatch(&self.denoise_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Generate a mask matte on GPU. Supports an unbounded number
    /// of masks of any shape (circle / rectangle / linear gradient
    /// / bezier polygon) — per-mask shape data lands in a storage
    /// buffer rather than the previous fixed-size uniform, and
    /// pooled bezier control points live in a second storage
    /// buffer keyed by per-record `bezier_offset` / `bezier_count`.
    ///
    /// Returns greyscale RGBA8 (R=G=B=matte, A=255).
    pub fn generate_mask_gpu(
        &mut self,
        width: u32,
        height: u32,
        masks: &[GpuMaskDef],
    ) -> Vec<u8> {
        if self.is_device_lost() {
            let _ = masks;
            return vec![0u8; (width * height * 4) as usize];
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        // Pre-allocate everything `&mut self` needs BEFORE we
        // start handing out `&` borrows of self.buf_b — wgpu::Buffer
        // is not Clone in 22.x so we can't make an owned copy to
        // hand to a `&mut self` dispatch. Instead we make
        // `dispatch_mask_v2` `&self`-only and build buffers up
        // front.
        let (records, points) = Self::build_mask_storage(masks);
        let records_bytes = (records.len() * std::mem::size_of::<MaskRecord>()) as u64;
        let points_bytes = (points.len() * std::mem::size_of::<[f32; 2]>()) as u64;
        self.ensure_mask_storage_buffers(records_bytes, points_bytes);

        let dest = self.buf_b.as_ref().expect("ensure_buffers populated buf_b");
        self.dispatch_mask_v2(width, height, &records, &points, masks.len() as u32, dest, workgroups);
        self.download_raw_rgba_as_u8(dest, (width * height) as usize)
    }

    /// Build the per-mask records and pooled bezier-points arrays
    /// from a slice of [`GpuMaskDef`]. Returns the records (one
    /// per input mask, with `bezier_offset` / `bezier_count` set
    /// for bezier shapes) and the flat point pool (concatenation
    /// of every bezier mask's points in stack order).
    fn build_mask_storage(masks: &[GpuMaskDef]) -> (Vec<MaskRecord>, Vec<[f32; 2]>) {
        let mut records: Vec<MaskRecord> = Vec::with_capacity(masks.len());
        let mut points: Vec<[f32; 2]> = Vec::new();
        for m in masks {
            let (offset, count) = if m.shape_type == 3 {
                let off = points.len() as u32;
                let n = m.bezier_points.len() as u32;
                points.extend(m.bezier_points.iter().map(|&(x, y)| [x, y]));
                (off, n)
            } else {
                (0, 0)
            };
            records.push(MaskRecord {
                shape_type: m.shape_type,
                flags: m.flags,
                bezier_offset: offset,
                bezier_count: count,
                cx: m.cx, cy: m.cy, p1: m.p1, p2: m.p2,
                p3: m.p3, feather: m.feather, feather_inner: m.feather_inner, _pad1: 0,
                w0: [m.warp[0], m.warp[1], m.warp[2], 0.0],
                w1: [m.warp[3], m.warp[4], m.warp[5], 0.0],
                w2: [m.warp[6], m.warp[7], m.warp[8], 0.0],
            });
        }
        (records, points)
    }

    /// Ensure the records + points storage buffers are at least
    /// the requested sizes. Storage buffers can't be size 0 in
    /// wgpu (it'd fail validation), so we floor each at 16 bytes
    /// of placeholder data — empty-bezier-stack draws still need
    /// valid bind-group entries.
    fn ensure_mask_storage_buffers(&mut self, records_bytes: u64, points_bytes: u64) {
        let records_bytes = records_bytes.max(96);  // one MaskRecord
        let points_bytes = points_bytes.max(8);     // one vec2<f32>
        let need_records = self.mask_records_buf.as_ref()
            .is_none_or(|b| b.size() < records_bytes);
        if need_records {
            self.mask_records_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mask_records_buf"),
                size: records_bytes.next_power_of_two().max(96),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let need_points = self.mask_points_buf.as_ref()
            .is_none_or(|b| b.size() < points_bytes);
        if need_points {
            self.mask_points_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mask_points_buf"),
                size: points_bytes.next_power_of_two().max(8),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
    }

    /// Write the storage buffers, build a 4-binding bind group on
    /// the v2 layout, and dispatch the mask pipeline writing to
    /// `dest` (which is `buf_b` for the CPU-download path and
    /// `matte_buf` for the GPU-resident chain path). Shared
    /// helper so the two callers stay in sync.
    ///
    /// Caller is responsible for `ensure_buffers` (the dest buffer)
    /// AND `ensure_mask_storage_buffers` (records + points buffers)
    /// BEFORE calling — those need `&mut self`, this dispatch only
    /// needs `&self` so we can hold a `&Buffer` to `dest` from
    /// inside `self.buf_b` / `self.matte_buf` while calling.
    fn dispatch_mask_v2(
        &self,
        width: u32,
        height: u32,
        records: &[MaskRecord],
        points: &[[f32; 2]],
        mask_count: u32,
        dest: &wgpu::Buffer,
        workgroups: u32,
    ) {
        // Empty-stack guard: write a placeholder all-zero record
        // and point so the bind-group's storage buffers are
        // non-empty (wgpu validation rejects 0-byte storage
        // buffers in a binding). The shader's `mask_count` gate
        // means the placeholders never execute.
        let placeholder_record = [MaskRecord {
            shape_type: 0, flags: 0, bezier_offset: 0, bezier_count: 0,
            cx: 0.0, cy: 0.0, p1: 0.0, p2: 0.0,
            p3: 0.0, feather: 0.0, feather_inner: 0.0, _pad1: 0,
            w0: [0.0; 4], w1: [0.0; 4], w2: [0.0; 4],
        }];
        let placeholder_point = [[0.0_f32, 0.0_f32]];
        let records_bytes_to_write: &[u8] = if records.is_empty() {
            bytemuck::cast_slice(&placeholder_record)
        } else {
            bytemuck::cast_slice(records)
        };
        let points_bytes_to_write: &[u8] = if points.is_empty() {
            bytemuck::cast_slice(&placeholder_point)
        } else {
            bytemuck::cast_slice(points)
        };

        let records_buf = self.mask_records_buf.as_ref()
            .expect("ensure_mask_storage_buffers must run before dispatch_mask_v2");
        let points_buf = self.mask_points_buf.as_ref()
            .expect("ensure_mask_storage_buffers must run before dispatch_mask_v2");
        self.upload(records_buf, 0, records_bytes_to_write);
        self.upload(points_buf, 0, points_bytes_to_write);

        // write_params is &mut self elsewhere; inline the small
        // uniform write here so this helper stays &self.
        let params = GpuMaskParams {
            width, height,
            mask_count,
            _pad: 0,
        };
        self.write_params(&params);

        // Pull the bind group layout straight off the pipeline so
        // we don't have to keep a second handle around in the
        // pipeline struct for every dispatch.
        let bgl = self.mask_pipeline.get_bind_group_layout(0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask_v2_bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: dest.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: records_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: points_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.encoder(Some("mask_v2_enc"));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mask_v2_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.mask_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        self.device.poll(wgpu::Maintain::Poll);
    }

    /// Apply hue/sat curves on the GPU using pre-computed LUTs.
    /// `lut_data` must be [`HUESAT_LUT_LEN`] f32 values — seven curves,
    /// 256 entries each.
    pub fn apply_huesat_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        mix: f32,
        lut_data: &[f32; HUESAT_LUT_LEN],
        active: u32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        // Upload LUT data
        self.upload(&self.huesat_lut_buf, 0, bytemuck::cast_slice(lut_data));

        // Write params
        self.write_params(&GpuHueSatParams {
            width,
            height,
            mix,
            active,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        // Create bind group with 4 bindings
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("huesat_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.huesat_lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.huesat_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply a 3D LUT with trilinear interpolation on GPU.
    /// `lut_data` is RGB triples in B-major order, `lut_size` is the grid dimension (e.g. 33).
    /// `bracket` carries the LUT's primaries around the lookup.
    pub fn apply_3d_lut_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        lut_data: &[f32],
        lut_size: u32,
        opacity: f32,
        tetrahedral: bool,
        bracket: LutBracket,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        if opacity <= 0.0 {
            return linear_rgba.to_vec();
        }

        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        // (Re-)create LUT buffer if needed
        let lut_byte_size = (lut_data.len() * 4) as u64;
        if self.lut3d_buf.is_none() || self.lut3d_buf_size < lut_byte_size {
            self.lut3d_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lut3d_data"),
                size: lut_byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.lut3d_buf_size = lut_byte_size;
        }
        let lut_buf = self.lut3d_buf.as_ref().unwrap();
        self.upload(lut_buf, 0, bytemuck::cast_slice(lut_data));

        self.write_params(&GpuLut3dParams::new(width, height, lut_size, opacity, tetrahedral, bracket));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut3d_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.lut3d_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply qualifier (secondary color correction) on GPU.
    /// Generates HSL matte and applies secondary grade in a single pass.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_qualifier_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        hue_center: f32, hue_width: f32, hue_soft: f32,
        sat_low: f32, sat_high: f32, sat_soft: f32,
        lum_low: f32, lum_high: f32, lum_soft: f32,
        invert: bool,
        sec_exposure: f32, sec_saturation: f32, sec_hue_shift: f32, sec_temperature: f32,
        matte_preview: bool,
        rgb: Option<[f32; 7]>,
        volume: Option<QualifierVolume>,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        let (mode, ranges, v) = GpuQualifierParams::shape(rgb, volume);
        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        self.write_params(&GpuQualifierParams {
            width,
            height,
            hue_center: hue_center / 360.0,
            hue_width: hue_width / 360.0,
            hue_soft: hue_soft / 360.0,
            sat_low,
            sat_high,
            sat_soft,
            lum_low,
            lum_high,
            lum_soft,
            invert: if invert { 1 } else { 0 },
            sec_exposure,
            sec_saturation,
            sec_hue_shift: sec_hue_shift.to_radians(),
            sec_temperature: sec_temperature / 100.0 * 0.1,
            matte_preview: if matte_preview { 1 } else { 0 },
            mode,
            _pad1: 0,
            _pad2: 0,
            rgb: ranges,
            _pad3: 0.0,
            keep_mean: v[0],
            keep_inv: [v[1], v[2], v[3]],
            remove_mean: v[4],
            remove_inv: [v[5], v[6], v[7]],
            reach: v[8],
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.dispatch(&self.qualifier_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply standalone luminance-threshold key on GPU. Mirrors
    /// [`Self::apply_qualifier_gpu`] but evaluates Rec.709 luma
    /// alone — no HSL, no secondary grade. Output is a greyscale
    /// matte (R=G=B=matte, A copied from input alpha) ready for
    /// downstream `MaskCutout` / `CombineMattes` / `ApplyMatte`
    /// consumption.
    pub fn apply_luma_key_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        low: f32,
        high: f32,
        soft: f32,
        invert: bool,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        self.write_params(&GpuLumaKeyParams {
            width,
            height,
            low,
            high,
            soft,
            invert: if invert { 1 } else { 0 },
            into_alpha: 0,
            _pad1: 0,
            luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0],
        });

        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.dispatch(&self.luma_key_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply standalone soft-clip / hard-clip transform on GPU.
    /// Per-channel Reinhard-style rolloff above `threshold`. With
    /// `knee == 0` this becomes a hard clip at `threshold`; larger
    /// `knee` gives smoother highlights asymptotic to 1.0. Value
    /// transform — alpha is passed through; same pixel format and
    /// buffer layout as the input.
    pub fn apply_soft_clip_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        threshold: f32,
        knee: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        self.write_params(&GpuSoftClipParams {
            width,
            height,
            threshold,
            knee,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
            _pad3: 0,
        });

        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.dispatch(&self.soft_clip_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply film grain overlay on GPU.
    pub fn apply_grain_gpu(
        &mut self,
        linear_rgba: &[f32],
        width: u32,
        height: u32,
        amount: f32,
        size: f32,
        shadows: f32,
        highlights: f32,
        monochrome: bool,
        frame_seed: u32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload_frame(buf_a, bytemuck::cast_slice(linear_rgba));

        self.write_params(&GpuGrainParams {
            width,
            height,
            amount,
            size,
            shadows,
            highlights,
            frame_seed,
            monochrome: if monochrome { 1 } else { 0 },
            luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad_luma: 0,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.dispatch(&self.grain_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
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
        pass.dispatch_workgroups(workgroups, 1, 1);
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

    /// Dispatch-only colour-grade pass. Reads `in_is_a`-slot, writes the other.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_color_grade_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        exposure: f32,
        contrast: f32,
        // Already grade-encoded: `klipp_grade_model::encode_contrast_pivot`.
        contrast_pivot_encoded: f32,
        saturation: f32,
        white_balance: [[f32; 3]; 3],
        highlights: f32,
        shadows: f32,
        whites: f32,
        blacks: f32,
        lift: (f32, f32, f32),
        gamma: (f32, f32, f32),
        gain: (f32, f32, f32),
        offset: (f32, f32, f32),
        log_wheels: Option<[(f32, f32, f32); 3]>,
        wheel_shape: klipp_grade_model::WheelShape,
        hue: f32,
        global_mix: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuColorGradeParams {
            width, height, exposure, contrast, saturation, wb: flat3(white_balance),
            highlights, shadows, whites, blacks,
            lift_r: lift.0, lift_g: lift.1, lift_b: lift.2,
            gamma_r: gamma.0, gamma_g: gamma.1, gamma_b: gamma.2,
            gain_r: gain.0, gain_g: gain.1, gain_b: gain.2,
            offset_r: offset.0, offset_g: offset.1, offset_b: offset.2,
            hue, global_mix, contrast_pivot_encoded,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            encoding: self.grade_encoding,
            log_mode: log_wheels.is_some() as u32,
            log_wheels: flat_wheels(log_wheels),
            log_low_range: wheel_shape.low_range,
            log_high_range: wheel_shape.high_range,
            luma_mix: wheel_shape.luma_mix,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        // Disjoint-field borrow: read from chain_bg_3binding while
        // taking &mut on chain_encoder. Pool must be pre-populated.
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.color_grade_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only colour-balance pass.
    pub fn apply_color_balance_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        white_balance: [[f32; 3]; 3],
        hue: f32,
        global_mix: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuColorBalanceParams {
            width, height, wb: flat3(white_balance), hue, global_mix,
            _pad0: 0, _pad1: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.color_balance_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only HDR zones pass (#240).
    pub fn apply_hdr_zones_chain(&mut self, in_is_a: bool, width: u32, height: u32, terms: &klipp_grade_model::hdr_zones::Terms) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuHdrZonesParams::new(width, height, terms));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.hdr_zones_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only wheel sat/bri pass.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_wheel_sat_bri_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        lift_sat: f32, lift_bri: f32,
        gamma_sat: f32, gamma_bri: f32,
        gain_sat: f32, gain_bri: f32,
        offset_sat: f32, offset_bri: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuWheelSatBriParams {
            width, height,
            lift_sat, lift_bri, gamma_sat, gamma_bri,
            gain_sat, gain_bri, offset_sat, offset_bri,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad0: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.wheel_sat_bri_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only RGB curves pass. Uploads LUT, writes params, dispatches.
    pub fn apply_rgb_curves_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        lut_data: &[f32; 1024],
        preserve_luma: bool,
        mix: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        self.upload(&self.rgb_curves_lut_buf, 0, bytemuck::cast_slice(lut_data));
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuRgbCurvesParams {
            width, height,
            preserve_luma: if preserve_luma { 1 } else { 0 },
            mix,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad0: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_rgb_curves[slot * 2 + Self::chain_cfg_4b(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.rgb_curves_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only vibrance pass.
    pub fn apply_vibrance_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        amount: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuVibranceParams { width, height, amount, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.vibrance_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only display step of a scene-referred clip (phase 2b M2,
    /// step 11c): `klipp_color::hdr::SceneToDisplay::apply` on the chain,
    /// so the effects that follow take the picture where it sits. `kind`,
    /// `scene_scale` and `exposure` are the source transfer's
    /// (`display_light_kind`, `scene_scale`, `display_exposure`); a `peak`
    /// is display light, none the SDR tone map; `luma` is the working
    /// space's luminance weights.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_scene_to_display_chain(&mut self, in_is_a: bool, width: u32, height: u32, kind: u32, scene_scale: f32, exposure: f32, peak: Option<f32>, luma: [f32; 3]) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuSceneToDisplayParams {
            width,
            height,
            light: u32::from(peak.is_some()),
            kind,
            scene_scale,
            exposure,
            peak: peak.unwrap_or(0.0),
            _pad: 0,
            luma_r: luma[0],
            luma_g: luma[1],
            luma_b: luma[2],
            _pad1: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.scene_to_display_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only 12-sector colour warper pass.
    pub fn apply_color_warper_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        hue_shifts: &[f32; 12],
        sat_shifts: &[f32; 12],
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuColorWarperParams {
            width, height, _pad0: 0, _pad1: 0,
            hue_shifts: *hue_shifts, sat_shifts: *sat_shifts,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.color_warper_pipeline, bind_group, workgroups);
    }


    /// Dispatch-only clarity (midtone unsharp-mask) pass. Uses `buf_c` as
    /// scratch for the horizontal/vertical luma blur. The caller must have
    /// already called `ensure_buffers` for a frame that fits in `buf_c` too.
    pub fn apply_clarity_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        amount: f32,
        radius: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let byte_size = (width * height * 16) as u64;
        // buf_c must exist and match the current size. Normally
        // `ensure_chain_buffers` has already built it at the current
        // size via `ensure_buffers`; this branch is defensive for the
        // case where clarity runs outside the chain (shouldn't happen).
        // If we do recreate buf_c, rebuild the clarity/3-binding pools
        // that reference it.
        if self.buf_c.is_none() || self.current_buf_size < byte_size {
            self.buf_c = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("buf_c"), size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.rebuild_chain_bind_group_pools();
        }
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let radius = radius.max(1);

        // Clarity is 3 passes (blur_h, blur_v, apply). Two distinct
        // params structs (`GpuClarityBlurParams` for the blurs,
        // `GpuClarityApplyParams` for the unsharp-mask combine) — two
        // ring slots are sufficient since the two blur passes share the
        // same params struct. We still claim two slots to keep each
        // logical params write in its own backing buffer; blur_h and
        // blur_v happen to hold identical values, so we could share,
        // but keeping them separate simplifies reasoning and leaves
        // room if the blur params ever diverge.
        let blur_slot = self.next_params_slot();
        self.write_chain_params(blur_slot, &GpuClarityBlurParams { width, height, radius, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });
        let apply_slot = self.next_params_slot();
        self.write_chain_params(apply_slot, &GpuClarityApplyParams { width, height, amount, _pad: 0, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad1: 0 });

        // Pass 1: horizontal blur (in RGBA → out luma scratch).
        // 3-binding pool AtoB/BtoA at `blur_slot`.
        {
            let bind_group = self.chain_bg_3binding[blur_slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
                .as_ref()
                .expect(Self::CHAIN_BG_EXPECT);
            let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
            Self::record_compute_dispatch(enc, &self.clarity_blur_h_pipeline, bind_group, workgroups);
        }

        // Pass 2: vertical blur (out luma → buf_c blurred luma).
        // The "input" here is what Pass 1 wrote to, which is buf_b when
        // in_is_a or buf_a when !in_is_a. That's (buf_b, buf_c) or
        // (buf_a, buf_c) respectively — AtoC/BtoC configs in the pool.
        // Note: `!in_is_a` inverts parity because the vertical pass
        // reads what the horizontal pass just wrote.
        {
            let bind_group = self.chain_bg_3binding[blur_slot * 4 + Self::chain_cfg_3b_ac(!in_is_a)]
                .as_ref()
                .expect(Self::CHAIN_BG_EXPECT);
            let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
            Self::record_compute_dispatch(enc, &self.clarity_blur_v_pipeline, bind_group, workgroups);
        }

        // Pass 3: unsharp mask (in RGBA + buf_c blurred luma → out RGBA).
        // 4-binding clarity_apply pool keyed on (in_is_a) × (buf_c as
        // binding 3).
        let bind_group = self.chain_bg_clarity_apply[apply_slot * 2 + Self::chain_cfg_4b(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.clarity_apply_pipeline, bind_group, workgroups);
    }

    /// Rasterize a mask matte into the dedicated `matte_buf` (instead
    /// of buf_b → CPU download as `generate_mask_gpu` does). Used by
    /// the GPU-resident `Source → Corrector → MaskCutout` chain so the
    /// matte never leaves the GPU. Caller is responsible for matching
    /// the matte_buf's lifetime to the chain's submit window — i.e.
    /// `matte_buf` write happens BEFORE `apply_mask_cutout_chain` in
    /// the same `begin_chain` / `submit_chain` pair.
    ///
    /// All shape variants are supported, including bezier polygons
    /// (per-mask shape data is in a storage buffer, not the
    /// uniform — there is no shape cap).
    pub fn generate_mask_gpu_to_matte_buf(
        &mut self,
        width: u32,
        height: u32,
        masks: &[GpuMaskDef],
    ) {
        if self.is_device_lost() {
            return;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let (records, points) = Self::build_mask_storage(masks);
        let records_bytes = (records.len() * std::mem::size_of::<MaskRecord>()) as u64;
        let points_bytes = (points.len() * std::mem::size_of::<[f32; 2]>()) as u64;
        self.ensure_mask_storage_buffers(records_bytes, points_bytes);

        let matte = self.matte_buf.as_ref().expect("matte_buf populated by ensure_buffers");
        self.dispatch_mask_v2(width, height, &records, &points, masks.len() as u32, matte, workgroups);
    }

    /// Dispatch-only mask cutout — multiplies the chain frame by
    /// `matte_buf`'s R channel and forces alpha=1.0. Caller must have
    /// written the matte to `matte_buf` (e.g. via
    /// `generate_mask_gpu_to_matte_buf`) before calling this. Standard
    /// chain conventions: reads from in_is_a slot, writes to opposite.
    pub fn apply_mask_cutout_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuMaskCutoutParams { width, height, _pad0: 0, _pad1: 0 });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_mask_cutout[slot * 2 + Self::chain_cfg_4b(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.mask_cutout_pipeline, bind_group, workgroups);
    }

    /// Keep a copy of the chain's current picture (in the slot `in_is_a`
    /// names) for [`Self::apply_mix_with_kept_chain`]: a node that mixes
    /// its result with its input over several dispatches.
    pub fn keep_chain_input(&mut self, in_is_a: bool, width: u32, height: u32) {
        if self.is_device_lost() {
            return;
        }
        let size = (width * height * 16) as u64;
        if self.grade_mix_src.as_ref().is_none_or(|b| b.size() < size) {
            self.grade_mix_src = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("grade_mix_src"),
                size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let src = if in_is_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }.expect(Self::CHAIN_BG_EXPECT);
        let dst = self.grade_mix_src.as_ref().unwrap();
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        enc.copy_buffer_to_buffer(src, 0, dst, 0, size);
    }

    /// Dispatch-only mix of the chain's picture with the one
    /// [`Self::keep_chain_input`] kept: `kept + (current - kept) * mix`.
    pub fn apply_mix_with_kept_chain(&mut self, in_is_a: bool, width: u32, height: u32, mix: f32) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuGradeMixParams { width, height, mix, _pad: 0 });
        let (a, b) = (self.buf_a.as_ref().expect(Self::CHAIN_BG_EXPECT), self.buf_b.as_ref().expect(Self::CHAIN_BG_EXPECT));
        let (input, output) = if in_is_a { (a, b) } else { (b, a) };
        let kept = self.grade_mix_src.as_ref().expect("keep_chain_input first");
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_grade_mix"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: kept.as_entire_binding() },
            ],
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.grade_mix_pipeline, &bind_group, workgroups);
    }

    /// Dispatch-only Blend: `a + (b - a) * mix`, `a` the chain's
    /// picture and `b` the kept picture `other` (M3 step 17b). Alpha
    /// mixes with the rest.
    pub fn apply_blend_chain(&mut self, in_is_a: bool, width: u32, height: u32, mix: f32, other: &KeptPicture) {
        self.dispatch_composite(in_is_a, width, height, mix, /* over = */ false, other);
    }

    /// Dispatch-only Layer: the over operator, foreground from the
    /// chain and background the kept picture `other`.
    pub fn apply_over_chain(&mut self, in_is_a: bool, width: u32, height: u32, other: &KeptPicture) {
        self.dispatch_composite(in_is_a, width, height, 1.0, /* over = */ true, other);
    }

    fn dispatch_composite(&mut self, in_is_a: bool, width: u32, height: u32, mix: f32, over: bool, other: &KeptPicture) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuGradeMixParams { width, height, mix, _pad: 0 });
        let (a, b) = (
            self.buf_a.as_ref().expect(Self::CHAIN_BG_EXPECT),
            self.buf_b.as_ref().expect(Self::CHAIN_BG_EXPECT),
        );
        let (input, output) = if in_is_a { (a, b) } else { (b, a) };
        let other = self.slot_pool.buffer(other.0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_composite"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: other.as_entire_binding() },
            ],
        });
        let pipeline = if over { &self.layer_over_pipeline } else { &self.blend_mix_pipeline };
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, pipeline, &bind_group, workgroups);
    }

    /// Upload the difference key's clean plate, already resized to the
    /// frame and in linear light, unless the same one is already there.
    ///
    /// `key` identifies the plate to the *host*: its resize-cache
    /// generation plus the frame size. Contents are not compared, since
    /// reading 33 MB to decide whether to write 33 MB is the cost this
    /// avoids. A host that reuses a generation for different pixels
    /// would show a stale plate, so the executor derives it from the
    /// plate path and dimensions it already keys its own cache on.
    pub fn upload_difference_key_plate(
        &mut self,
        plate_linear: &[f32],
        key: (u64, u32, u32),
    ) {
        if self.is_device_lost() {
            return;
        }
        // Another plate while a key recorded in the open chain still reads
        // this one (#1266): the chain goes to the queue first and goes on
        // in a new encoder, as when its slots run out. The upload then
        // lands after that submit and before the next.
        if self.plate_key != Some(key) && self.plate_read_in_chain && self.chain_encoder.is_some() {
            self.submit_chain();
            self.begin_chain();
        }
        let bytes = std::mem::size_of_val(plate_linear) as u64;
        let needs_alloc = self.plate_buf.as_ref().is_none_or(|b| b.size() < bytes);
        if needs_alloc {
            self.plate_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("difference_key_plate"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            // A fresh buffer holds nothing, so whatever was keyed before
            // is no longer in it.
            self.plate_key = None;
        }
        if self.plate_key == Some(key) {
            return;
        }
        if let Some(buf) = self.plate_buf.as_ref() {
            self.upload_frame(buf, bytemuck::cast_slice(plate_linear));
            self.plate_key = Some(key);
        }
    }

    /// Whether a plate matching `key` is already resident, so the
    /// executor can skip building the resized plate at all.
    pub fn has_difference_key_plate(&self, key: (u64, u32, u32)) -> bool {
        self.plate_buf.is_some() && self.plate_key == Some(key)
    }

    /// A neighbour frame for [`Self::apply_temporal_denoise_chain`], from
    /// linear working-space pixels on the CPU: a buffer of its own, never
    /// the chain's or the stack's, which the caller holds (an `Arc`) until
    /// the chain that reads it is submitted. `None` without a device or
    /// when the buffer can't be made.
    pub fn upload_neighbour(&self, linear: &[f32]) -> Option<std::sync::Arc<wgpu::Buffer>> {
        if self.is_device_lost() || linear.is_empty() {
            return None;
        }
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal_neighbour"),
            size: std::mem::size_of_val(linear) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.upload_frame(&buf, bytemuck::cast_slice(linear));
        Some(std::sync::Arc::new(buf))
    }

    /// A neighbour frame the GPU holds as sRGB bytes (a viewer stream's
    /// decoded frame), decoded to linear into a buffer of its own for a
    /// temporal denoise to bind (#1988): nothing read back or uploaded.
    /// Never the chain's buffers or its parameters: the pass has a uniform
    /// of its own, and is submitted at once, ahead of the chain that reads
    /// it. `None` on a lost device or a buffer too small.
    pub fn neighbour_from_u8(&self, input_u8: &wgpu::Buffer, width: u32, height: u32) -> Option<std::sync::Arc<wgpu::Buffer>> {
        let pixels = u64::from(width) * u64::from(height);
        if self.is_device_lost() || pixels == 0 || input_u8.size() < pixels * 4 {
            return None;
        }
        let out = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal_neighbour_from_u8"),
            size: pixels * 16,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let params = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal_neighbour_from_u8_params"),
            size: std::mem::size_of::<BridgeU8Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&params, 0, bytemuck::bytes_of(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 }));
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("temporal_neighbour_from_u8_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input_u8.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.srgb8_lut.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("temporal_neighbour_from_u8") });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.bridge_u8_to_linear_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((pixels as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        Some(std::sync::Arc::new(out))
    }

    /// Dispatch-only temporal noise reduction (#1988): the chain's picture
    /// (in `buf_a` when `in_is_a`) against `neighbours` (none to two, each
    /// a frame of the picture's size), written to the other slot, as
    /// `temporal_denoise::denoise_frame` draws it. `false`, nothing
    /// recorded, without a chain, with more than two neighbours or one of
    /// the wrong size; the caller takes the CPU route then. `luma` is the
    /// guard's brightness weights, the working space's
    /// (`klipp_color::linear::luma_coefs`), as the CPU takes them (#2019).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_temporal_denoise_chain(&mut self, in_is_a: bool, width: u32, height: u32, neighbours: &[&wgpu::Buffer], threshold: f32, strength: f32, luma: [f32; 3]) -> bool {
        let bytes = u64::from(width) * u64::from(height) * 16;
        if self.is_device_lost() || self.chain_encoder.is_none() || neighbours.len() > 2 || neighbours.iter().any(|n| n.size() < bytes) {
            return false;
        }
        if self.buf_a.is_none() || self.buf_b.is_none() {
            return false;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuTemporalDenoiseParams {
            width,
            height,
            count: neighbours.len() as u32,
            _pad0: 0,
            denom: crate::temporal_denoise::denominator(threshold),
            strength: strength.clamp(0.0, 1.0),
            _pad1: 0,
            _pad2: 0,
            luma: [luma[0], luma[1], luma[2], 0.0],
        });
        let (input, output) = match (self.buf_a.as_ref(), self.buf_b.as_ref()) {
            (Some(a), Some(b)) if in_is_a => (a, b),
            (Some(a), Some(b)) => (b, a),
            _ => return false,
        };
        // An unread slot still needs a buffer bound: the input, read only.
        let before = neighbours.first().copied().unwrap_or(input);
        let after = neighbours.get(1).copied().unwrap_or(input);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_temporal_denoise"),
            layout: &self.temporal_denoise_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: before.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: after.as_entire_binding() },
            ],
        });
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.temporal_denoise_pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
        true
    }

    /// Temporal noise reduction of `input` (linear RGBA, `width` ×
    /// `height`) against `neighbours`, off the chain (#2266): the chain's
    /// kernel and parameters, so a frame is denoised to the same picture
    /// whether the chain draws it or this does. For a node whose output
    /// must be pixels (a parked frame holds it): the result read back,
    /// and the buffer it is in, for the holder to seed later chains from.
    /// Its own buffers and uniform, never the chain's. `None` on a lost
    /// device, a size that doesn't fit, more than two neighbours, or a
    /// failed read; the caller draws it on the CPU then.
    #[allow(clippy::too_many_arguments)]
    pub fn temporal_denoise_pixels(&mut self, input: &[f32], width: u32, height: u32, neighbours: &[&wgpu::Buffer], threshold: f32, strength: f32, luma: [f32; 3]) -> Option<(Vec<f32>, std::sync::Arc<wgpu::Buffer>)> {
        let pixels = (width as usize) * (height as usize);
        let bytes = (pixels * 16) as u64;
        if self.is_device_lost() || pixels == 0 || input.len() != pixels * 4 || neighbours.len() > 2 || neighbours.iter().any(|n| n.size() < bytes) {
            return None;
        }
        // The read-back's staging buffer, at the frame's size.
        self.ensure_buffers(bytes);
        let storage = |label| self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        let (inp, out) = (storage("temporal_denoise_in"), storage("temporal_denoise_out"));
        self.queue.write_buffer(&inp, 0, bytemuck::cast_slice(input));
        let params = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("temporal_denoise_params"),
            size: std::mem::size_of::<GpuTemporalDenoiseParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue.write_buffer(&params, 0, bytemuck::bytes_of(&GpuTemporalDenoiseParams {
            width,
            height,
            count: neighbours.len() as u32,
            _pad0: 0,
            denom: crate::temporal_denoise::denominator(threshold),
            strength: strength.clamp(0.0, 1.0),
            _pad1: 0,
            _pad2: 0,
            luma: [luma[0], luma[1], luma[2], 0.0],
        }));
        let before = neighbours.first().copied().unwrap_or(&inp);
        let after = neighbours.get(1).copied().unwrap_or(&inp);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("temporal_denoise_pixels_bg"),
            layout: &self.temporal_denoise_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: inp.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: before.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: after.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("temporal_denoise_pixels") });
        Self::record_compute_dispatch(&mut encoder, &self.temporal_denoise_pipeline, &bind_group, (pixels as u32).div_ceil(WORKGROUP_SIZE));
        self.queue.submit(std::iter::once(encoder.finish()));
        let read = self.with_downloaded(&out, bytes, |b| mapped_bytes_to_f32(b, pixels * 4)).filter(|v| v.len() == pixels * 4)?;
        Some((read, std::sync::Arc::new(out)))
    }

    /// Dispatch-only ApplyMatte: blend the chain's picture (graded) with
    /// `kept` (ungraded), through `matte` (M3 step 17b). Standard chain
    /// conventions — reads the `in_is_a` slot, writes the opposite.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_matte_chain(&mut self, in_is_a: bool, width: u32, height: u32, mix: f32, channel: crate::mask_mix::MaskChannel, invert: bool, kept: &KeptPicture, matte: &KeyedMatte) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        let [luma_yr, luma_yg, luma_yb] = self.luma_coefs;
        self.write_chain_params(slot, &GpuApplyMatteParams { width, height, mix, channel: channel as u32, luma_yr, luma_yg, luma_yb, invert: invert as u32 });
        let (a, b) = (
            self.buf_a.as_ref().expect(Self::CHAIN_BG_EXPECT),
            self.buf_b.as_ref().expect(Self::CHAIN_BG_EXPECT),
        );
        let (input, output) = if in_is_a { (a, b) } else { (b, a) };
        let (kept, matte) = (self.slot_pool.buffer(kept.0), self.slot_pool.buffer(matte.0));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chain_bg_apply_matte"),
            layout: &self.apply_matte_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: kept.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: matte.as_entire_binding() },
            ],
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.apply_matte_pipeline, &bind_group, workgroups);
    }

    /// Dispatch-only bilateral denoise pass.
    pub fn apply_denoise_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        spatial_sigma: f32,
        luma_sigma: f32,
        chroma_sigma: f32,
        max_kernel: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &DenoiseParams { width, height, spatial_sigma, luma_sigma, luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], max_kernel, chroma_sigma, _pad: [0; 3] });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.denoise_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only hue/sat curves pass. Uploads the 6×256 LUT, writes
    /// params, and dispatches.
    pub fn apply_huesat_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        mix: f32,
        lut_data: &[f32; HUESAT_LUT_LEN],
        active: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        self.upload(&self.huesat_lut_buf, 0, bytemuck::cast_slice(lut_data));
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuHueSatParams { width, height, mix, active });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_huesat[slot * 2 + Self::chain_cfg_4b(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.huesat_pipeline, bind_group, workgroups);
    }

    /// Dispatch-only 3D LUT pass with trilinear interpolation.
    pub fn apply_3d_lut_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        lut_data: &[f32],
        lut_size: u32,
        opacity: f32,
        tetrahedral: bool,
        bracket: LutBracket,
    ) {
        if self.is_device_lost() {
            return;
        }
        if opacity <= 0.0 {
            // No-op: copy in → out into the chain encoder. The batched
            // submit runs it alongside the surrounding dispatches.
            let byte_size = (width * height * 16) as u64;
            // Split the shared `self` borrow: encoder is on self, buf_a/buf_b
            // are on self. `copy_buffer_to_buffer` takes `&Buffer`, so we
            // need to hold the buffer refs separately from the &mut encoder.
            // wgpu::Buffer isn't Clone, but copy_buffer_to_buffer doesn't
            // retain them, so we can re-extract each borrow in sequence by
            // matching on the Options directly.
            let buf_a = self.buf_a.as_ref().expect("ensure_chain_buffers must be called first");
            let buf_b = self.buf_b.as_ref().expect("ensure_chain_buffers must be called first");
            let (input, output) = if in_is_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
            let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
            enc.copy_buffer_to_buffer(input, 0, output, 0, byte_size);
            return;
        }
        let lut_byte_size = (lut_data.len() * 4) as u64;
        if self.lut3d_buf.is_none() || self.lut3d_buf_size < lut_byte_size {
            self.lut3d_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lut3d_data"),
                size: lut_byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.lut3d_buf_size = lut_byte_size;
        }
        // Claim the params slot first so subsequent `&self` borrows
        // (lut_buf, chain_in_out) don't clash with the `&mut self` that
        // `next_params_slot` needs.
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuLut3dParams::new(width, height, lut_size, opacity, tetrahedral, bracket));
        let lut_buf = self.lut3d_buf.as_ref().unwrap();
        self.upload(lut_buf, 0, bytemuck::cast_slice(lut_data));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let (input, output) = self.chain_in_out(in_is_a);
        // NOTE: lut3d_buf is dynamically sized (different LUT dimensions
        // produce different buffer sizes), so it's not pooled — a fresh
        // bind group is created per dispatch. This path runs at most
        // once per chain (≤1 3D LUT node per grade) so the cost is bounded.
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut3d_chain_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: lut_buf.as_entire_binding() },
            ],
        });
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.lut3d_pipeline, &bind_group, workgroups);
    }

    /// Dispatch-only film grain overlay pass.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_grain_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        amount: f32,
        size: f32,
        shadows: f32,
        highlights: f32,
        monochrome: bool,
        frame_seed: u32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuGrainParams {
            width, height, amount, size, shadows, highlights, frame_seed,
            monochrome: if monochrome { 1 } else { 0 },
            luma_yr: self.luma_coefs[0], luma_yg: self.luma_coefs[1], luma_yb: self.luma_coefs[2], _pad_luma: 0,
        });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.grain_pipeline, bind_group, workgroups);
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

    /// Reserve a temporary title object slot after the frame's timeline slots.
    /// The caller reuses the returned index for every object in this title.
    pub fn stack_title_object_slot(&mut self, offset: usize) -> Option<usize> {
        if self.stack_buf_size == 0 || self.is_device_lost() { return None; }
        if offset > 3 { return None; }
        let index = self.stack_title_slot + offset;
        while self.stack_layers.len() <= index {
            self.stack_layers.push(std::sync::Arc::new(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("title object"), size: self.stack_buf_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })));
        }
        Some(index)
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
            pass.dispatch_workgroups(workgroups, 1, 1);
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

    /// Source-local title effects, with no CPU break route. The caller orders
    /// these before title post-processing and the clip grade.
    pub fn stack_title_source_effects(&mut self, slot: usize, effects: &[BuiltinEffect]) -> bool {
        if effects.is_empty() { return true; }
        if self.is_device_lost() || slot >= self.stack_layers.len() { return false; }
        let (w,h) = self.stack_dims;
        self.ensure_recording_buffers(w,h);
        let Some(a) = self.buf_a.as_ref() else { return false };
        if !self.stack_copy(&self.stack_layers[slot],a,"title effect seed") { return false; }
        self.begin_chain();
        let mut in_a=true;
        let mut ok=true;
        for effect in effects {
            match self.record_in_chain(std::slice::from_ref(effect),in_a,w,h) {
                Recorded::Done => in_a = !in_a,
                Recorded::Identity => {},
                _ => { ok=false; break; }
            }
        }
        self.submit_chain();
        ok && self.stack_layer_from_chain(slot,in_a)
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

    /// Draw the native living gradient directly in a stack layer. Only
    /// parameters cross from the CPU; pixels remain on the GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_living_gradient(&mut self, i: usize, a: [u8; 4], b: [u8; 4], angle_deg: f32, drift: f32, glow: f32, grain: f32, secs: f64) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() { return false; }
        let (width, height) = self.stack_dims;
        let t = secs as f32;
        let theta = (angle_deg + drift * t).to_radians();
        self.write_params(&LivingGradientParams {
            width, height, glow, grain,
            a: a.map(f32::from), b: b.map(f32::from),
            dir: [theta.sin(), theta.cos()],
            centre: [width as f32 * (0.5 + 0.28 * (t * 0.33).cos()), height as f32 * (0.5 + 0.22 * (t * 0.21).sin())],
            to_working: self.authored_to_working,
        });
        self.stack_pass(&self.living_gradient_pipeline, self.stack_acc.as_ref().unwrap(), &self.stack_layers[i], (width * height).div_ceil(WORKGROUP_SIZE), "stack_living_gradient");
        true
    }

    /// Layer `i` as the native Grain generator draws it (#2067): `color`
    /// with monochrome noise `amount` deep in cells `size_px` wide at
    /// 1080p, a new grain `speed` times a second. The CPU's
    /// `render_grain` is its reference: the same hash, cell and depth.
    pub fn stack_layer_grain(&mut self, i: usize, color: [u8; 4], amount: f32, size_px: f32, speed: f32, secs: f64) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() { return false; }
        let (width, height) = self.stack_dims;
        let scale = (height as f32 / 1080.0).max(0.1);
        self.write_params(&GrainGeneratorParams {
            width, height,
            frame: (secs as f32 * speed.max(0.0)).floor() as i64 as u32,
            _pad: 0,
            color: color.map(f32::from),
            cell: (size_px * scale).max(1.0),
            depth: amount.clamp(0.0, 1.0) * 110.0,
            _p: [0.0; 2],
            to_working: self.authored_to_working,
        });
        self.stack_pass(&self.grain_generator_pipeline, self.stack_acc.as_ref().unwrap(), &self.stack_layers[i], (width * height).div_ceil(WORKGROUP_SIZE), "stack_grain_generator");
        true
    }

    /// Layer `i` as flat rects over a background (#2067): a countdown's or
    /// a timer's digits, laid out by `klipp_core::Segments` for the CPU and
    /// here alike. `rects` are `[x0, y0, x1, y1)` in the layer's pixels.
    pub fn stack_layer_segments(&mut self, i: usize, background: [u8; 4], color: [u8; 4], rects: &[[u32; 4]]) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() { return false; }
        let (width, height) = self.stack_dims;
        let bytes = (rects.len().max(1) * 16) as u64;
        if self.stack_rects.as_ref().is_none_or(|b| b.size() < bytes) {
            self.stack_rects = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_rects"),
                size: bytes.max(256),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let buf = self.stack_rects.as_ref().unwrap();
        if !rects.is_empty() {
            self.upload(buf, 0, bytemuck::cast_slice(rects));
        }
        self.write_params(&SegmentsParams {
            width, height, count: rects.len() as u32, _pad: 0,
            background: background.map(f32::from),
            color: color.map(f32::from),
            to_working: self.authored_to_working,
        });
        self.stack_pass(&self.segments_pipeline, buf, &self.stack_layers[i], (width * height).div_ceil(WORKGROUP_SIZE), "stack_segments");
        true
    }

    /// Layer `i` takes an sRGB RGBA8 straight-alpha sprite from the CPU
    /// (a title, a generator): decoded to linear on the GPU, alpha kept.
    /// Layer `i` from sRGB-encoded half floats, decoded on the way in.
    ///
    /// The sibling of [`Self::stack_layer_from_srgb_u8`], for a source
    /// with more than eight bits to give. The decode stays here, where
    /// the u8 path's decode is: sRGB is a power curve, and a `powf` a
    /// channel across a 1080p frame is 6.2 million of them, which on
    /// the CPU cost more than the whole generator did — 88 ms a frame
    /// against 35 — and on the GPU costs nothing anyone can measure.
    ///
    /// Half floats rather than whole ones because the source is
    /// display-referred: eleven bits of mantissa against sRGB's eight
    /// is already far more than the picture can hold, and it is eight
    /// bytes a pixel instead of sixteen. Two halves ride in each u32,
    /// unpacked with `unpack2x16float`, so it asks nothing of the
    /// device that the eight-bit path does not.
    pub fn stack_layer_from_srgb_f16(&mut self, i: usize, packed: &[u32]) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let pixel_count = self.stack_pixel_count();
        if packed.len() as u64 != pixel_count * 2 {
            return false;
        }
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        let bytes = pixel_count * 8;
        if self.stack_sprite_f16.as_ref().is_none_or(|b| b.size() < bytes) {
            self.stack_sprite_f16 = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_f16"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let sprite = self.stack_sprite_f16.as_ref().unwrap();
        self.upload(sprite, 0, bytemuck::cast_slice(packed));
        self.write_params(&SpriteRowsParams { width, height, y0: 0, y1: height, to_working: self.authored_to_working });
        let px = width * height;
        self.stack_pass(&self.sprite_rows_f16_pipeline, sprite, &layer, px.div_ceil(WORKGROUP_SIZE), "stack_sprite_f16");
        true
    }

    pub fn stack_layer_from_srgb_u8(&mut self, i: usize, rgba: &[u8]) -> bool {
        let h = self.stack_dims.1;
        self.stack_layer_from_srgb_u8_rows(i, rgba, 0, h)
    }

    /// `stack_layer_from_srgb_u8` for a sprite that only covers rows
    /// `y0..y1` of the full-frame `rgba`: only those rows go up and are
    /// decoded; the rest of the layer is cleared.
    pub fn stack_layer_from_srgb_u8_rows(&mut self, i: usize, rgba: &[u8], y0: u32, y1: u32) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let pixel_count = self.stack_pixel_count();
        if rgba.len() as u64 != pixel_count * 4 {
            return false;
        }
        let y0 = y0.min(height);
        let y1 = y1.min(height);
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        let mut enc = self.encoder(Some("stack_sprite_clear"));
        enc.clear_buffer(&layer, 0, Some(pixel_count * 16));
        self.submit(enc);
        if y1 <= y0 {
            return true;
        }
        if self.stack_sprite_u8.as_ref().is_none_or(|b| b.size() < pixel_count * 4) {
            self.stack_sprite_u8 = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_u8"),
                size: pixel_count * 4,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let sprite = self.stack_sprite_u8.as_ref().unwrap();
        let row_bytes = (width as u64) * 4;
        let (b0, b1) = ((y0 as u64 * row_bytes) as usize, (y1 as u64 * row_bytes) as usize);
        self.upload(sprite, b0 as u64, &rgba[b0..b1]);
        self.write_params(&SpriteRowsParams { width, height, y0, y1, to_working: self.authored_to_working });
        let rows_px = (y1 - y0) * width;
        self.stack_pass(&self.sprite_rows_pipeline, sprite, &layer, rows_px.div_ceil(WORKGROUP_SIZE), "stack_sprite_rows");
        true
    }

    /// Layer `i` is the title sprite `id` moved by `(dx, dy)` with its
    /// alpha scaled by `opacity` — `klipp_title`'s `shift_and_fade`,
    /// done where the layer is. `pixels` is the sprite, uploaded only
    /// when `id` is not resident; rows `y0..y1` are the destination rows
    /// it covers, the rest of the layer is cleared.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_place_sprite(&mut self, i: usize, id: u64, texels: &[u32], bx: SpriteBox, dx: i32, dy: i32, opacity: f32, y0: u32, y1: u32) -> bool {
        self.stack_layer_place_sprite_posed(i, id, texels, bx, dx, dy, opacity, 1.0, (0.0, 0.0), None, None, y0, y1)
    }

    /// [`Self::stack_layer_place_sprite`] with the whole pose: the
    /// sprite scaled by `scale` about `anchor` (bilinear, premultiplied,
    /// in the sprite's sRGB-encoded units) and laid over a full-frame
    /// `under` colour (sRGB8, straight alpha) when there is one, its
    /// alpha shaped by where it is on the frame when `shape` is given.
    /// The twin of `klipp_title::pose_sprite`.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_place_sprite_posed(
        &mut self,
        i: usize,
        id: u64,
        texels: &[u32],
        bx: SpriteBox,
        dx: i32,
        dy: i32,
        opacity: f32,
        scale: f32,
        anchor: (f32, f32),
        under: Option<[u8; 4]>,
        shape: Option<SpriteShape>,
        y0: u32,
        y1: u32,
    ) -> bool {
        self.place_sprite(i, id, texels, bx, dx, dy, opacity, scale, anchor, under, shape, y0, y1, 0, None)
    }

    /// Layer `i` takes the resident sprite `id`, as it rests, with its
    /// alpha scaled by `opacity`, laid over what the layer already holds
    /// as the title renderer lays one text layer over another (#2069):
    /// straight alpha, in sRGB-encoded units. A title's text layers each
    /// fading are placed so, one after another, the first with
    /// [`Self::stack_layer_place_sprite_posed`]. Only rows `y0..y1` are
    /// touched. Existing pixels are converted back to authored light for
    /// blending, then returned to the project working space on the GPU.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_place_sprite_over(&mut self, i: usize, id: u64, texels: &[u32], bx: SpriteBox, opacity: f32, y0: u32, y1: u32) -> bool {
        self.place_sprite(i, id, texels, bx, 0, 0, opacity, 1.0, (0.0, 0.0), None, None, y0, y1, 1, None)
    }

    /// Assemble cached glyph coverage with the CPU mask's max operation.
    /// Fill is a frame parameter; changing colour does not replace the source.
    pub fn stack_title_glyph(&mut self, slot: usize, id: u64, texels: &[u32], size: [u32;2], origin: [i32;2], color: [u8;4], place: GlyphPlace) -> bool {
        let (y0,y1)=self.glyph_rows(origin,size[1],place);
        self.place_sprite(slot,id,texels,SpriteBox{x0:0,y0:0,w:size[0],h:size[1]},origin[0],origin[1],place.alpha,1.0,(0.0,0.0),Some(color),None,y0,y1,2,None)
    }

    /// Subtract native bitmap alpha from the shared glyph face mask, as
    /// colour_pixels does before the CPU paints the remaining text fill.
    pub fn stack_title_colour_coverage_subtract(&mut self,slot:usize,id:u64,texels:&[u32],size:[u32;2],origin:[i32;2],place:GlyphPlace)->bool {
        let (y0,y1)=self.glyph_rows(origin,size[1],place);
        self.place_sprite(slot,id,texels,SpriteBox{x0:0,y0:0,w:size[0],h:size[1]},origin[0],origin[1],place.alpha,1.0,(0.0,0.0),None,None,y0,y1,3,None)
    }

    /// Place a cached native-colour glyph without applying the text fill.
    pub fn stack_title_colour_glyph(&mut self,slot:usize,id:u64,texels:&[u32],size:[u32;2],origin:[i32;2],place:GlyphPlace)->bool {
        let (y0,y1)=self.glyph_rows(origin,size[1],place);
        self.place_sprite(slot,id,texels,SpriteBox{x0:0,y0:0,w:size[0],h:size[1]},origin[0],origin[1],place.alpha,1.0,(0.0,0.0),None,None,y0,y1,4,None)
    }

    /// Resize a retained byte-premultiplied font strike with the CPU triangle
    /// filter. mode: 2=max coverage, 3=subtract coverage, 1=native RGBA over.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_title_resized_colour_glyph(&mut self,slot:usize,id:u64,texels:&[u32],size:[u32;2],target:[u32;2],origin:[i32;2],color:[u8;4],mode:u32,place:GlyphPlace)->bool {
        if target.contains(&0) || size.contains(&0) || !matches!(mode,1..=3) {return false;}
        let (y0,y1)=self.glyph_rows(origin,target[1],place);
        // Native RGBA over takes a posed glyph's alpha rounded, as the glyph modes do.
        self.place_sprite(slot,id,texels,SpriteBox{x0:0,y0:0,w:size[0],h:size[1]},origin[0],origin[1],place.alpha,1.0,(0.0,0.0),Some(color),None,y0,y1,if mode==1 {4} else {mode},Some(target))
    }

    /// The frame rows a glyph `height` tall at `origin` covers, inside its pose's clip.
    fn glyph_rows(&self,origin:[i32;2],height:u32,place:GlyphPlace)->(u32,u32) {
        let (_,h)=self.stack_dims;
        let top=origin[1].max(place.clip_y[0]);
        let bottom=(i64::from(origin[1])+i64::from(height)).min(i64::from(place.clip_y[1]));
        (top.clamp(0,h as i32) as u32,bottom.clamp(0,i64::from(h)) as u32)
    }

    #[allow(clippy::too_many_arguments)]
    fn place_sprite(
        &mut self,
        i: usize,
        id: u64,
        texels: &[u32],
        bx: SpriteBox,
        dx: i32,
        dy: i32,
        opacity: f32,
        scale: f32,
        anchor: (f32, f32),
        under: Option<[u8; 4]>,
        shape: Option<SpriteShape>,
        y0: u32,
        y1: u32,
        over: u32,
        native_size: Option<[u32;2]>,
    ) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let Some(to_authored) = title_styles::inverse(self.authored_to_working) else { return false };
        let (width, height) = self.stack_dims;
        let pixel_count = self.stack_pixel_count();
        if (native_size.is_none() && over != 2 && !bx.fits(texels,width,height))
            || texels.len() as u64 != u64::from(bx.w)*u64::from(bx.h)*2
        { return false; }
        let y0 = y0.min(height);
        let y1 = y1.min(height);
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        if over == 0 {
            let mut enc = self.encoder(Some("stack_sprite_clear"));
            enc.clear_buffer(&layer, 0, Some(pixel_count * 16));
            self.submit(enc);
        }
        if y1 <= y0 || (opacity <= 0.001 && under.is_none()) {
            return true;
        }
        let srgb = |v: u8| {
            let c = v as f32 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let under = under.map(|c| [srgb(c[0]), srgb(c[1]), srgb(c[2]), c[3] as f32 / 255.0]).unwrap_or([0.0; 4]);
        let pos = self.resident_sprite(id, texels);
        self.write_params(&SpritePlaceParams {
            width,
            height,
            y0,
            y1,
            dx,
            dy,
            opacity,
            scale: if (scale - 1.0).abs() < 1e-6 { 1.0 } else { scale },
            anchor_x: anchor.0,
            anchor_y: anchor.1,
            under_r: under[0],
            under_g: under[1],
            under_b: under[2],
            under_a: under[3],
            focus_axis: match shape.and_then(|s| s.focus_vertical) {
                None => 0,
                Some(true) => 1,
                Some(false) => 2,
            },
            band: shape.map_or(0.5, |s| s.band),
            edge_opacity: shape.map_or(1.0, |s| s.edge_opacity),
            fade_top: shape.map_or(0.0, |s| s.top),
            fade_bottom: shape.map_or(0.0, |s| s.bottom),
            fade_left: shape.map_or(0.0, |s| s.left),
            fade_right: shape.map_or(0.0, |s| s.right),
            bx0: bx.x0,
            by0: bx.y0,
            bw: bx.w,
            bh: bx.h,
            over,
            _pad1: native_size.map_or(0,|s|s[0]),
            _pad2: native_size.map_or(0,|s|s[1]),
            to_working: self.authored_to_working,
            to_authored,
        });
        let rows_px = (y1 - y0) * width;
        let sprite = &self.stack_sprites[pos].buffer;
        self.stack_pass(&self.sprite_place_pipeline, sprite, &layer, rows_px.div_ceil(WORKGROUP_SIZE), "stack_sprite_place");
        true
    }

    /// Whether title sprite `id` is on the GPU.
    /// Layer `i` takes the resident sprite `id`'s glyphs each placed on
    /// its own: `units` is the sprite's glyph map over `(x0, y0, w, h)`
    /// (`u16::MAX` for no glyph), `glyphs` one sixteen-float record per
    /// glyph as `klipp_title::GlyphPose::packed` lays it out (moved box,
    /// dx dy scale alpha, anchor and clip rows). The twin of
    /// `klipp_title::pose_sprite_glyphs`; only rows `y0..y1` are written.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_layer_place_glyphs(
        &mut self,
        i: usize,
        id: u64,
        texels: &[u32],
        bx: SpriteBox,
        units: (u32, u32, u32, u32, &[u16]),
        glyphs: &[[f32; 16]],
        y0: u32,
        y1: u32,
    ) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let pixel_count = self.stack_pixel_count();
        let (ux0, uy0, uw, uh, index) = units;
        if !bx.fits(texels, width, height) || index.len() as u64 != (uw as u64) * (uh as u64) || glyphs.is_empty() {
            return false;
        }
        let y0 = y0.min(height);
        let y1 = y1.min(height);
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        let mut enc = self.encoder(Some("stack_sprite_clear"));
        enc.clear_buffer(&layer, 0, Some(pixel_count * 16));
        self.submit(enc);
        if y1 <= y0 {
            return true;
        }
        let pos = self.resident_sprite(id, texels);
        if self.stack_sprites[pos].units.as_ref().is_none_or(|u| (u.0, u.1, u.2, u.3) != (ux0, uy0, uw, uh)) {
            let mut packed: Vec<u32> = Vec::with_capacity(index.len().div_ceil(2));
            for pair in index.chunks(2) {
                packed.push(pair[0] as u32 | ((pair.get(1).copied().unwrap_or(0) as u32) << 16));
            }
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_units"),
                size: ((packed.len().max(1)) * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.upload(&buffer, 0, bytemuck::cast_slice(&packed));
            self.stack_sprites[pos].units = Some((ux0, uy0, uw, uh, buffer));
        }
        let table_bytes = (glyphs.len() * 64) as u64;
        if self.sprite_glyph_table.as_ref().is_none_or(|b| b.size() < table_bytes) {
            self.sprite_glyph_table = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_glyphs"),
                size: table_bytes.max(64),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let flat: Vec<f32> = glyphs.iter().flatten().copied().collect();
        let table = self.sprite_glyph_table.as_ref().unwrap();
        self.upload(table, 0, bytemuck::cast_slice(&flat));
        self.write_params(&SpriteGlyphsParams { width, height, y0, y1, n: glyphs.len() as u32, ux0, uy0, uw, uh, bx0: bx.x0, by0: bx.y0, bw: bx.w, bh: bx.h, _p0: 0, _p1: 0, _p2: 0, to_working: self.authored_to_working });
        let sprite = &self.stack_sprites[pos];
        let units_buf = &sprite.units.as_ref().unwrap().4;
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stack_sprite_glyphs"),
            layout: &self.sprite_glyphs_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: sprite.buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: layer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: units_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: table.as_entire_binding() },
            ],
        });
        let rows_px = (y1 - y0) * width;
        let mut enc = self.encoder(Some("stack_sprite_glyphs"));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.sprite_glyphs_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(rows_px.div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.submit(enc);
        true
    }

    /// The resident sprite `id`, put on the GPU from `texels` when it is
    /// not there yet: the oldest go first when the count or the bytes
    /// would pass their caps (#190). Its index in `stack_sprites`.
    fn resident_sprite(&mut self, id: u64, texels: &[u32]) -> usize {
        self.stack_sprite_tick += 1;
        let tick = self.stack_sprite_tick;
        if let Some(pos) = self.stack_sprites.iter().position(|r| r.id == id) {
            self.stack_sprites[pos].used = tick;
            return pos;
        }
        let size = ((texels.len().max(2)) * 4) as u64;
        while !self.stack_sprites.is_empty()
            && (self.stack_sprites.len() >= STACK_SPRITE_CAP || self.stack_sprites.iter().map(|r| r.buffer.size()).sum::<u64>() + size > STACK_SPRITE_BYTES)
        {
            let oldest = self.stack_sprites.iter().enumerate().min_by_key(|(_, r)| r.used).map(|(i, _)| i).unwrap();
            self.stack_sprites.remove(oldest);
        }
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stack_resident_sprite"),
            size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        if !texels.is_empty() {
            self.upload(&buffer, 0, bytemuck::cast_slice(texels));
        }
        self.stack_sprites.push(ResidentSprite { id, buffer, used: tick, units: None });
        self.stack_sprites.len() - 1
    }

    /// The bytes the resident sprites take on the GPU.
    pub fn stack_sprite_bytes(&self) -> u64 {
        self.stack_sprites.iter().map(|r| r.buffer.size()).sum()
    }

    /// Layer `i` is a title's box of packed sRGB half floats (`bx`), the
    /// rest of the layer cleared (#190): only the box goes up.
    pub fn stack_layer_from_srgb_f16_box(&mut self, i: usize, texels: &[u32], bx: SpriteBox) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        if !bx.fits(texels, width, height) {
            return false;
        }
        let pixel_count = self.stack_pixel_count();
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        let mut enc = self.encoder(Some("stack_sprite_clear"));
        enc.clear_buffer(&layer, 0, Some(pixel_count * 16));
        self.submit(enc);
        if bx.w == 0 || bx.h == 0 {
            return true;
        }
        let bytes = (texels.len() * 4) as u64;
        if self.stack_sprite_f16.as_ref().is_none_or(|b| b.size() < bytes) {
            self.stack_sprite_f16 = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stack_sprite_f16"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let sprite = self.stack_sprite_f16.as_ref().unwrap();
        self.upload(sprite, 0, bytemuck::cast_slice(texels));
        self.write_params(&SpriteBoxParams { width, height, bx0: bx.x0, by0: bx.y0, bw: bx.w, bh: bx.h, _p0: 0, _p1: 0, to_working: self.authored_to_working });
        self.stack_pass(&self.sprite_box_f16_pipeline, sprite, &layer, (bx.w * bx.h).div_ceil(WORKGROUP_SIZE), "stack_sprite_box_f16");
        true
    }

    pub fn stack_sprite_resident(&self, id: u64) -> bool {
        self.stack_sprites.iter().any(|r| r.id == id)
    }

    /// Multiply layer `i`'s alpha by `factor` (a transition side's
    /// opacity, a missing matte source).
    pub fn stack_scale_alpha(&mut self, i: usize, factor: f32) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        self.write_params(&ScaleAlphaParams { width, height, factor, _pad: 0 });
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        self.stack_pass(&self.scale_alpha_pipeline, self.matte_buf.as_ref().unwrap(), &layer, wg, "stack_scale_alpha");
        true
    }

    /// Mask layer `consumer`'s alpha by a matte taken from layer
    /// `source`: `channel` (0 alpha, 1 luminance, 2 red, 3 green, 4
    /// blue), inverted or not, choked by `choke_px` (a square minimum),
    /// feathered by `feather_px` (the same box-Gaussian the CPU uses).
    #[allow(clippy::too_many_arguments)]
    pub fn stack_matte(&mut self, consumer: usize, source: usize, channel: u32, invert: bool, feather_px: f32, choke_px: f32) -> bool {
        if self.is_device_lost() || consumer >= self.stack_layers.len() || source >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        let luma = self.luma_coefs;
        let source_buf = std::sync::Arc::clone(&self.stack_layers[source]);
        let consumer_buf = std::sync::Arc::clone(&self.stack_layers[consumer]);
        self.write_params(&MatteExtractParams { width, height, channel, invert: invert as u32, luma_r: luma[0], luma_g: luma[1], luma_b: luma[2], _pad: 0 });
        self.stack_pass(&self.matte_extract_pipeline, &source_buf, self.matte_buf.as_ref().unwrap(), wg, "stack_matte_extract");
        if choke_px >= 0.5 {
            let radius = (choke_px.round() as u32).max(1);
            self.write_params(&MaskPoolParams { width, height, radius, _pad: 0 });
            self.stack_pass(&self.mask_min_h_pipeline, self.matte_buf.as_ref().unwrap(), self.buf_c.as_ref().unwrap(), wg, "stack_mask_min_h");
            self.write_params(&MaskPoolParams { width, height, radius, _pad: 0 });
            self.stack_pass(&self.mask_min_v_pipeline, self.buf_c.as_ref().unwrap(), self.matte_buf.as_ref().unwrap(), wg, "stack_mask_min_v");
        }
        if feather_px >= 0.25 {
            for r in crate::blur::boxes_for_gaussian(feather_px) {
                self.write_params(&BlurParams { width, height, radius: r as i32, _pad: 0 });
                self.stack_pass(&self.blur_h_pipeline, self.matte_buf.as_ref().unwrap(), self.buf_c.as_ref().unwrap(), wg, "stack_feather_h");
                self.write_params(&BlurParams { width, height, radius: r as i32, _pad: 0 });
                self.stack_pass(&self.blur_v_pipeline, self.buf_c.as_ref().unwrap(), self.matte_buf.as_ref().unwrap(), wg, "stack_feather_v");
            }
        }
        self.write_params(&ScaleAlphaParams { width, height, factor: 1.0, _pad: 0 });
        self.stack_pass(&self.matte_apply_pipeline, self.matte_buf.as_ref().unwrap(), &consumer_buf, wg, "stack_matte_apply");
        true
    }

    /// A transition between layers `from` and `to` into layer `out`. The
    /// sides are mixed premultiplied and the result divided back to
    /// straight, as the CPU mixes them: a side that is partly transparent
    /// (scaled down, matted, faded) must lend its colour by its alpha, not
    /// by the blend's weight alone (#718, #1026). With both sides opaque
    /// it is the plain mix.
    #[allow(clippy::too_many_arguments)]
    pub fn stack_transition(&mut self, from: usize, to: usize, out: usize, kind_id: u32, progress: f32, p: [f32; 4], color: [f32; 4]) -> bool {
        if crate::pieces::is_posed(kind_id, p[3]) {
            let (width, height) = self.stack_dims;
            return self.stack_pieces(from, to, out, &crate::pieces::plan(kind_id, progress, width, height, p, 0.0, 1));
        }
        if self.is_device_lost() || from >= self.stack_layers.len() || to >= self.stack_layers.len() || out >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        let (Some(from_pm), Some(to_pm)) = (self.buf_a.as_ref(), self.buf_b.as_ref()) else { return false };
        self.write_params(&AlphaFormParams { width, height, unpremultiply: 0, _pad: 0 });
        let (from_layer, to_layer) = (std::sync::Arc::clone(&self.stack_layers[from]), std::sync::Arc::clone(&self.stack_layers[to]));
        self.stack_pass(&self.alpha_form_pipeline, &from_layer, from_pm, wg, "stack_transition_premultiply_from");
        self.stack_pass(&self.alpha_form_pipeline, &to_layer, to_pm, wg, "stack_transition_premultiply_to");
        self.write_params(&TransitionParams {
            width, height, progress, kind_id,
            param0: p[0], param1: p[1], param2: p[2], param3: p[3],
            color_r: color[0], color_g: color[1], color_b: color[2], color_a: color[3],
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stack_transition_bg"),
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: from_pm.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: to_pm.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.buf_c.as_ref().unwrap().as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });
        let mut enc = self.encoder(Some("stack_transition"));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.transition_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(wg, 1, 1);
        }
        self.submit(enc);
        self.write_params(&AlphaFormParams { width, height, unpremultiply: 1, _pad: 0 });
        let out_layer = std::sync::Arc::clone(&self.stack_layers[out]);
        self.stack_pass(&self.alpha_form_pipeline, self.buf_c.as_ref().unwrap(), &out_layer, wg, "stack_transition_unpremultiply");
        true
    }

    /// The accumulator exists and is a canvas if nothing has been laid
    /// on it yet.
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

    /// Blend layer `i` onto the accumulator with `opacity` and `mode`
    /// (`BlendMode as u32`, the order of `klipp_effects::BlendMode`; 13
    /// is a plain lerp). The first Normal layer at full opacity is
    /// copied in, as the CPU stack moves it in.
    pub fn stack_blend(&mut self, i: usize, opacity: f32, mode: u32) -> bool {
        let h = self.stack_dims.1;
        self.stack_blend_rows(i, opacity, mode, 0, h)
    }

    /// `stack_blend` over rows `y0..y1` only.
    pub fn stack_blend_rows(&mut self, i: usize, opacity: f32, mode: u32, y0: u32, y1: u32) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let (y0, y1) = (y0.min(height), y1.min(height));
        if !self.stack_acc_touched && mode == 0 && opacity >= 1.0 && y0 == 0 && y1 == height {
            let acc = self.stack_acc.clone().unwrap();
            if !self.stack_copy(&self.stack_layers[i], &acc, "stack_move_in") {
                return false;
            }
            self.stack_acc_touched = true;
            return true;
        }
        if !self.stack_ensure_acc() || y1 <= y0 {
            return self.stack_acc.is_some();
        }
        self.write_params(&BlendLayerParams { width, height, opacity, mode, y0, y1, _p0: 0, _p1: 0 });
        let acc = self.stack_acc.clone().unwrap();
        let rows_px = (y1 - y0) * width;
        let layer = std::sync::Arc::clone(&self.stack_layers[i]);
        self.stack_pass(&self.blend_layer_pipeline, &layer, &acc, rows_px.div_ceil(WORKGROUP_SIZE), "stack_blend");
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

    /// Equalize's histogram and CDF of `linear` (straight RGBA, `width` ×
    /// `height`) as the chain builds them on the GPU (#1215), read back:
    /// for the tests that hold them to `crate::equalize_cdf`. `None` on a
    /// lost device.
    #[doc(hidden)]
    pub fn equalize_histogram_and_cdf(&mut self, linear: &[f32], width: u32, height: u32) -> Option<([u32; EQUALIZE_CDF_LEN], [f32; EQUALIZE_CDF_LEN])> {
        if self.is_device_lost() {
            return None;
        }
        self.ensure_recording_buffers(width, height);
        let frame = self.buf_a.as_ref()?;
        self.upload(frame, 0, bytemuck::cast_slice(linear));
        self.write_params(&EqualizeParams { width, height, strength: 1.0, _pad0: 0, luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0] });
        let (hist, cdf) = (&self.equalize_hist_buf, &self.equalize_cdf_buf);
        let bind = |a: &wgpu::Buffer, b: &wgpu::Buffer, c: &wgpu::Buffer| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("equalize_probe"),
                layout: &self.huesat_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: a.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: b.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: self.params_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: c.as_entire_binding() },
                ],
            })
        };
        let mut encoder = self.encoder(None);
        encoder.clear_buffer(hist, 0, None);
        Self::record_compute_dispatch(&mut encoder, &self.equalize_histogram_pipeline, &bind(frame, hist, cdf), (width * height).div_ceil(WORKGROUP_SIZE));
        Self::record_compute_dispatch(&mut encoder, &self.equalize_cdf_pipeline, &bind(hist, cdf, hist), 1);
        self.submit(encoder);
        let counts: Vec<u32> = bytemuck::cast_slice(&self.download(hist, EQUALIZE_CDF_LEN as u64 * 4)).to_vec();
        let ramp: Vec<f32> = bytemuck::cast_slice(&self.download(cdf, EQUALIZE_CDF_LEN as u64 * 4)).to_vec();
        Some((counts.try_into().ok()?, ramp.try_into().ok()?))
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
                        pass.dispatch_workgroups(workgroups, 1, 1);
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

    /// Draw `key`'s matte from the chain's picture (in `buf_a` when
    /// `in_is_a`) into a pool slot of its own, recorded in the chain's
    /// encoder: the picture isn't touched, and nothing is read back.
    /// `None` when there is no chain, the pool's budget refuses, or a
    /// difference key has no plate; the caller keys the CPU way then.
    pub fn key_into_matte_slot(&mut self, key: &GpuKey, in_is_a: bool, width: u32, height: u32) -> Option<KeyedMatte> {
        if self.is_device_lost() || self.chain_encoder.is_none() {
            return None;
        }
        if matches!(key, GpuKey::DifferenceKey { .. }) && self.plate_buf.is_none() {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Matte }, |k| slot_buffer(device, k)).ok()?;
        self.record_key(key, KeyRoute::ChainToSlot(in_is_a, id), width, height);
        Some(KeyedMatte(id))
    }

    /// Draw `key`'s matte from `from`, a kept picture, into a pool slot of
    /// its own, recorded in the chain's encoder: a grade secondary's key,
    /// drawn just before its gate reads it (M3 step 17b). `None` as
    /// [`Self::key_into_matte_slot`].
    pub fn key_kept_into_matte_slot(&mut self, key: &GpuKey, from: &KeptPicture, width: u32, height: u32) -> Option<KeyedMatte> {
        if self.is_device_lost() || self.chain_encoder.is_none() {
            return None;
        }
        if matches!(key, GpuKey::DifferenceKey { .. }) && self.plate_buf.is_none() {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease(SlotKey { width, height, usage: SlotUsage::Matte }, |k| slot_buffer(device, k)).ok()?;
        self.record_key(key, KeyRoute::KeptToSlot(from.0, id), width, height);
        Some(KeyedMatte(id))
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

    /// A window's matte (`masks`, as drawn now) in a pool slot of its own,
    /// for a grade secondary's gate (M3 step 17b). Drawn by a pass
    /// submitted on its own, ahead of the open encoder, so the slot is one
    /// no pass of that encoder has used. `None` when the budget refuses.
    pub fn mask_into_matte_slot(&mut self, width: u32, height: u32, masks: &[GpuMaskDef]) -> Option<KeyedMatte> {
        if self.is_device_lost() {
            return None;
        }
        let device = &self.device;
        let id = self.slot_pool.lease_for_upload(SlotKey { width, height, usage: SlotUsage::Matte }, |k| slot_buffer(device, k)).ok()?;
        self.slot_pool.mark_cpu_written(id);
        let (records, points) = Self::build_mask_storage(masks);
        let records_bytes = (records.len() * std::mem::size_of::<MaskRecord>()) as u64;
        let points_bytes = (points.len() * std::mem::size_of::<[f32; 2]>()) as u64;
        self.ensure_mask_storage_buffers(records_bytes, points_bytes);
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.dispatch_mask_v2(width, height, &records, &points, masks.len() as u32, self.slot_pool.buffer(id), workgroups);
        Some(KeyedMatte(id))
    }

    /// A keyed matte's pixels, read back for a reader off the chain.
    /// `None` while the chain that draws it is still being recorded.
    pub fn read_keyed_matte(&self, matte: &KeyedMatte, pixel_count: usize) -> Option<Vec<f32>> {
        if self.is_device_lost() || self.chain_encoder.is_some() {
            return None;
        }
        Some(self.download_linear_rgba_f32(self.slot_pool.buffer(matte.0), pixel_count)).filter(|v| v.len() == pixel_count * 4)
    }

    /// Give a keyed matte's slot back: its last reader is recorded.
    pub fn release_keyed_matte(&mut self, matte: KeyedMatte) {
        self.slot_pool.release(matte.0);
    }

    /// Record `key`'s kernel along `route`.
    fn record_key(&mut self, key: &GpuKey, route: KeyRoute, width: u32, height: u32) {
        let slot = self.next_params_slot();
        let pipeline = match *key {
            GpuKey::Qualifier { hue_center, hue_width, hue_soft, sat_low, sat_high, sat_soft, lum_low, lum_high, lum_soft, invert, rgb, volume } => {
                let (mode, rgb, v) = GpuQualifierParams::shape(rgb, volume);
                self.write_chain_params(slot, &GpuQualifierParams {
                    width,
                    height,
                    hue_center: hue_center / 360.0,
                    hue_width: hue_width / 360.0,
                    hue_soft: hue_soft / 360.0,
                    sat_low,
                    sat_high,
                    sat_soft,
                    lum_low,
                    lum_high,
                    lum_soft,
                    invert: invert as u32,
                    sec_exposure: 0.0,
                    sec_saturation: 1.0,
                    sec_hue_shift: 0.0,
                    sec_temperature: 0.0,
                    matte_preview: 1,
                    mode,
                    _pad1: 0,
                    _pad2: 0,
                    rgb,
                    _pad3: 0.0,
                    keep_mean: v[0],
                    keep_inv: [v[1], v[2], v[3]],
                    remove_mean: v[4],
                    remove_inv: [v[5], v[6], v[7]],
                    reach: v[8],
                });
                &self.qualifier_pipeline
            }
            GpuKey::LumaKey { low, high, soft, invert } => {
                self.write_chain_params(slot, &GpuLumaKeyParams {
                    width, height, low, high, soft,
                    invert: invert as u32,
                    into_alpha: 0,
                    _pad1: 0,
                    luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0],
                });
                &self.luma_key_pipeline
            }
            GpuKey::DifferenceKey { tolerance, soft, invert, luminance } => {
                self.write_chain_params(slot, &GpuDifferenceKeyParams {
                    width,
                    height,
                    tolerance,
                    soft,
                    invert: invert as u32,
                    match_space: luminance as u32,
                    _pad0: 0,
                    _pad1: 0,
                    luma: [self.luma_coefs[0], self.luma_coefs[1], self.luma_coefs[2], 0.0],
                });
                &self.difference_key_pipeline
            }
        };
        let (input, output) = match route {
            KeyRoute::KeptToSlot(kept, id) => (self.slot_pool.buffer(kept), self.slot_pool.buffer(id)),
            KeyRoute::ChainToSlot(in_is_a, id) => (
                if in_is_a { self.buf_a.as_ref() } else { self.buf_b.as_ref() }.expect("chain buffers"),
                self.slot_pool.buffer(id),
            ),
        };
        let mut plate_read = false;
        let mut entries = vec![
            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 2, resource: self.chain_params.buffer(slot).as_entire_binding() },
        ];
        let layout = match key {
            GpuKey::DifferenceKey { .. } => {
                entries.push(wgpu::BindGroupEntry { binding: 3, resource: self.plate_buf.as_ref().expect("checked by the caller").as_entire_binding() });
                plate_read = true;
                &self.difference_key_bgl
            }
            _ => &self.bind_group_layout,
        };
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("chain_bg_key"), layout, entries: &entries });
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, pipeline, &bind_group, (width * height).div_ceil(WORKGROUP_SIZE));
        self.plate_read_in_chain |= plate_read;
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
        // One effect recorded as a single dispatch writes the other chain
        // buffer and leaves its input where it is: the mix reads it there
        // and writes over it (#1254). Several effects, or one of several
        // passes, may write over the input, and a copy of it is kept.
        let in_place = matches!(effects, [one] if Self::single_dispatch(one));
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
        let mut run = self.chain_run_effects(effects, in_is_a, width, height);
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

    /// Blur an alpha mask on the GPU. Input/output are single-channel u8 buffers.
    /// Uses three separable box passes at round(radius), matching title CPU masks.
    pub fn blur_alpha_gpu(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        let pixel_count = (w * h) as u64;
        // Each alpha value stored as one u32
        let byte_size = pixel_count * 4;
        self.ensure_buffers(byte_size);

        // Convert u8 alpha -> u32 for GPU
        let input_u32: Vec<u32> = mask.iter().map(|&a| a as u32).collect();

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload(buf_a, 0, bytemuck::cast_slice(&input_u32));

        let workgroups = (w * h).div_ceil(WORKGROUP_SIZE);

        // 3-pass box blur for gaussian approximation
        let boxes = [radius.max(0.0).round() as usize; 3];
        let mut current_in_a = true;

        for &box_r in &boxes {
            if box_r == 0 { continue; }

            // Horizontal pass
            let (inp, out) = if current_in_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
            self.write_params(&AlphaBlurParams { width: w, height: h, radius: box_r as u32, _pad: 0 });
            self.dispatch(&self.alpha_blur_h_pipeline, inp, out, workgroups);
            current_in_a = !current_in_a;

            // Vertical pass
            let (inp, out) = if current_in_a { (buf_a, buf_b) } else { (buf_b, buf_a) };
            self.dispatch(&self.alpha_blur_v_pipeline, inp, out, workgroups);
            current_in_a = !current_in_a;
        }

        let result_buf = if current_in_a { buf_a } else { buf_b };
        let result_bytes = self.download(result_buf, byte_size);

        // Convert u32 back to u8
        let result_u32: &[u32] = bytemuck::cast_slice(&result_bytes);
        result_u32.iter().map(|&v| v.min(255) as u8).collect()
    }

    /// Dilate an alpha mask on the GPU using circular distance.
    pub fn dilate_alpha_gpu(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        let r = radius.ceil() as u32;
        if r == 0 { return mask.to_vec(); }

        let pixel_count = (w * h) as u64;
        let byte_size = pixel_count * 4;
        self.ensure_buffers(byte_size);

        let input_u32: Vec<u32> = mask.iter().map(|&a| a as u32).collect();

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.upload(buf_a, 0, bytemuck::cast_slice(&input_u32));

        let workgroups = (w * h).div_ceil(WORKGROUP_SIZE);

        self.write_params(&AlphaDilateParams { width: w, height: h, radius: r, radius_sq: (radius.max(0.0) * radius.max(0.0)) as u32 });
        self.dispatch(&self.alpha_dilate_pipeline, buf_a, buf_b, workgroups);

        let result_bytes = self.download(buf_b, byte_size);
        let result_u32: &[u32] = bytemuck::cast_slice(&result_bytes);
        result_u32.iter().map(|&v| v.min(255) as u8).collect()
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
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
    }

    pub fn upload_rsc_row_transforms(&mut self, row_transforms: &[crate::rolling_shutter::RowTransform]) {
        let byte_size = std::mem::size_of_val(row_transforms) as u64;
        if self.rsc_row_buf.as_ref().is_none_or(|_| self.rsc_row_buf_size < byte_size) {
            self.rsc_row_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rsc_row_transforms"),
                size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.rsc_row_buf_size = byte_size;
        }
        self.upload(self.rsc_row_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(row_transforms));
    }

    pub fn has_rsc_data(&self) -> bool {
        self.rsc_row_buf.is_some() && self.rsc_row_buf_size > 0
    }

    pub fn upload_stabilization_homography(&mut self, h: &[f32; 12]) {
        let byte_size = 48u64; // 12 x f32
        if self.stabilization_buf.is_none() {
            self.stabilization_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("stabilization_homography"),
                size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.stabilization_buf_size = byte_size;
        }
        self.upload(self.stabilization_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(h));
    }

    /// Forget the stabilisation correction, so the effect passes the
    /// frame through until another is uploaded: a clip without analysis
    /// must not be warped by the last clip's (#698).
    pub fn clear_stabilization_homography(&mut self) {
        self.stabilization_buf = None;
    }

    /// Forget the rolling-shutter row transforms, as
    /// [`Self::clear_stabilization_homography`] does.
    pub fn clear_rsc_row_transforms(&mut self) {
        self.rsc_row_buf = None;
        self.rsc_row_buf_size = 0;
    }

    pub fn has_stabilization_data(&self) -> bool {
        self.stabilization_buf.is_some()
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

    /// Upload an sRGB u8 RGBA frame to a pixel storage buffer as
    /// native linear-f32 vec4s (16 bytes/pixel). Callers pass `Vec<u8>`
    /// at the boundary; this method handles the sRGB→linear decode +
    /// GPU upload in one shot.
    fn upload_u8_rgba_as_linear(&self, buf: &wgpu::Buffer, rgba: &[u8]) {
        let linear = u8_rgba_to_linear_f32(rgba);
        self.queue
            .write_buffer(buf, 0, bytemuck::cast_slice(&linear));
    }

    /// Download `pixel_count` linear-f32 RGBA vec4s from a pixel
    /// storage buffer and convert to sRGB u8 RGBA at the boundary.
    /// Retained for the u8-boundary paths (scopes, transition helpers,
    /// 3D LUT) that still speak u8.
    #[allow(dead_code)]
    fn download_linear_rgba_as_u8(
        &self,
        source: &wgpu::Buffer,
        pixel_count: usize,
    ) -> Vec<u8> {
        let byte_size = (pixel_count * 16) as u64;
        let bytes = self.download(source, byte_size);
        if bytes.len() < pixel_count * 16 {
            // Device-lost fallback: `download` returned zero bytes.
            return vec![0u8; pixel_count * 4];
        }
        let linear: &[f32] = bytemuck::cast_slice(&bytes);
        linear_f32_rgba_to_u8(linear)
    }

    /// Same as [`download_linear_rgba_as_u8`] but treats the buffer
    /// as already-perceptual / alpha data — no sRGB encoding applied
    /// on the way out. Used by mask matte downloads where the
    /// shader writes a 0..1 coverage value (not a linear-light
    /// luminance) to all RGB channels and the consumer (the test
    /// harness, an image viewer, the CPU compositor) expects that
    /// value to land at byte position `(matte * 255 + 0.5) as u8`
    /// without any gamma curve. Without this path, mask matte 0.5
    /// gets gamma-encoded to ~187/255 instead of the expected
    /// 128/255 — a 60-LSB drift that hid in production until the
    /// CPU-vs-GPU parity test caught it.
    fn download_raw_rgba_as_u8(
        &self,
        source: &wgpu::Buffer,
        pixel_count: usize,
    ) -> Vec<u8> {
        let byte_size = (pixel_count * 16) as u64;
        let bytes = self.download(source, byte_size);
        if bytes.len() < pixel_count * 16 {
            return vec![0u8; pixel_count * 4];
        }
        let f32s: &[f32] = bytemuck::cast_slice(&bytes);
        let mut out = Vec::with_capacity(pixel_count * 4);
        for chunk in f32s.chunks_exact(4) {
            out.push((chunk[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            out.push((chunk[1].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            out.push((chunk[2].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
            out.push((chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        }
        out
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

    /// Render a transition between two frames on the GPU.
    /// `kind_id`: 0=CrossDissolve, 1=WipeLeft, 2=WipeRight, 3=WipeUp, 4=WipeDown, 5=DipToBlack, 6=DipToWhite
    pub fn render_transition(
        &mut self,
        from_rgba: &[u8],
        to_rgba: &[u8],
        kind_id: u32,
        progress: f32,
        width: u32,
        height: u32,
        user_param0: f32,
        user_param1: f32,
        user_param2: f32,
        user_param3: f32,
        user_color: [f32; 4],
    ) -> Vec<u8> {
        if self.is_device_lost() {
            let _ = to_rgba;
            return from_rgba.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        let buf_c = self.buf_c.as_ref().unwrap();

        // Upload: buf_a = from, buf_c = to, buf_b = output.
        // u8 sRGB → linear f32 at the boundary.
        self.upload_u8_rgba_as_linear(buf_a, from_rgba);
        self.upload_u8_rgba_as_linear(buf_c, to_rgba);

        self.write_params(&TransitionParams {
            width,
            height,
            progress,
            kind_id,
            param0: user_param0,
            param1: user_param1,
            param2: user_param2,
            param3: user_param3,
            color_r: user_color[0],
            color_g: user_color[1],
            color_b: user_color[2],
            color_a: user_color[3],
        });

        // Create transition bind group (from, to, output, params)
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_c.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let mut encoder = self.encoder(None);
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.transition_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_as_u8(buf_b, (width * height) as usize)
    }

    /// Linear-f32 input/output transition. Identical shader work to
    /// [`Self::render_transition`] but skips the sRGB encode on the output
    /// path *and* the linear decode on the input path — callers
    /// already speaking linear (the engine compositor + display
    /// chain) hand off and pick up linear-f32 buffers directly,
    /// shaving the two CPU pow loops that the u8 boundary version
    /// pays per frame.
    ///
    /// The compute shader itself doesn't care about the colour
    /// space — it just lerps RGBA. By keeping the boundary in
    /// linear f32 we avoid round-tripping a colour-space encode
    /// purely to satisfy a u8 storage format.
    #[allow(clippy::too_many_arguments)]
    pub fn render_transition_linear(
        &mut self,
        from_linear: &[f32],
        to_linear: &[f32],
        kind_id: u32,
        progress: f32,
        width: u32,
        height: u32,
        user_param0: f32,
        user_param1: f32,
        user_param2: f32,
        user_param3: f32,
        user_color: [f32; 4],
    ) -> Vec<f32> {
        if crate::pieces::is_posed(kind_id, user_param3) {
            let plan = crate::pieces::plan(kind_id, progress, width, height, [user_param0, user_param1, user_param2, user_param3], 0.0, 1);
            return self.render_pieces_linear(from_linear, to_linear, &plan);
        }
        if self.is_device_lost() {
            let _ = to_linear;
            return from_linear.to_vec();
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        let buf_c = self.buf_c.as_ref().unwrap();

        // Upload linear-f32 directly — no sRGB conversion needed.
        self.upload(buf_a, 0, bytemuck::cast_slice(from_linear));
        self.upload(buf_c, 0, bytemuck::cast_slice(to_linear));

        self.write_params(&TransitionParams {
            width,
            height,
            progress,
            kind_id,
            param0: user_param0,
            param1: user_param1,
            param2: user_param2,
            param3: user_param3,
            color_r: user_color[0],
            color_g: user_color[1],
            color_b: user_color[2],
            color_a: user_color[3],
        });

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("render_transition_linear_bg"),
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_c.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let mut encoder = self.encoder(Some("render_transition_linear"));
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.transition_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(encoder);
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// A posed-pieces plan in a buffer of its own (#985): a batched stack
    /// may not have read the last one yet.
    fn pieces_buffer(&self, plan: &crate::pieces::PiecePlan) -> wgpu::Buffer {
        let mut data = plan.data.clone();
        if data.is_empty() {
            data.resize(crate::pieces::STRIDE, 0.0);
        }
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pieces"),
            size: (data.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        buf.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(&data));
        buf.unmap();
        buf
    }

    /// `SHADER_PIECES` over `from` and `to` into `out`, with `plan`.
    fn dispatch_pieces(&self, from: &wgpu::Buffer, to: &wgpu::Buffer, out: &wgpu::Buffer, plan: &crate::pieces::PiecePlan, label: &str) {
        let full = (plan.width as usize) * (plan.height as usize);
        let matte = plan.matte.as_deref().filter(|m| plan.subject != 0 && m.len() >= full);
        let subject = if plan.subject == 0 { 0 } else if matte.is_some() { 2 } else { 1 };
        self.write_params(&PiecesParams { width: plan.width, height: plan.height, count: plan.count, samples: plan.samples, background_to: plan.background_to as u32, subject, _pad1: 0, _pad2: 0 });
        let pieces = self.pieces_buffer(plan);
        // A buffer of its own, as the plan's: one value when there's none.
        let matte_data: &[f32] = matte.map_or(&[0.0][..], |m| &m[..full]);
        let matte_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pieces_matte"),
            size: (matte_data.len() * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        matte_buf.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(matte_data));
        matte_buf.unmap();
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &self.pieces_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: from.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: to.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: pieces.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: matte_buf.as_entire_binding() },
            ],
        });
        let mut enc = self.encoder(Some(label));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.pieces_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((plan.width * plan.height).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.submit(enc);
    }

    /// Posed piece transitions (#985) between two linear-light frames:
    /// what `pieces::render` draws on the CPU.
    pub fn render_pieces_linear(&mut self, from_linear: &[f32], to_linear: &[f32], plan: &crate::pieces::PiecePlan) -> Vec<f32> {
        if self.is_device_lost() {
            return crate::pieces::render(from_linear, to_linear, plan);
        }
        self.ensure_buffers((plan.width * plan.height * 16) as u64);
        let (buf_a, buf_b, buf_c) = (self.buf_a.as_ref().unwrap(), self.buf_b.as_ref().unwrap(), self.buf_c.as_ref().unwrap());
        self.upload(buf_a, 0, bytemuck::cast_slice(from_linear));
        self.upload(buf_c, 0, bytemuck::cast_slice(to_linear));
        self.dispatch_pieces(buf_a, buf_c, buf_b, plan, "render_pieces_linear");
        self.device.poll(wgpu::Maintain::Poll);
        self.download_linear_rgba_f32(buf_b, (plan.width * plan.height) as usize)
    }

    /// Layer `i` smeared across the camera's shutter (#1923): placed by
    /// each of `placements` (one Transform a shutter sample) and averaged
    /// premultiplied, given back straight, as the CPU's `shuttered`
    /// averages (#2017). One pass over the whole frame into `buf_c`, then
    /// back into the layer: a placed sample can reach past where the
    /// picture was. `false` for anything but Transforms, or no layer.
    pub fn stack_shutter(&mut self, i: usize, placements: &[BuiltinEffect]) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() || placements.is_empty() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let rows: Option<Vec<[f32; 16]>> = placements.iter().map(|e| TransformParams::of(e, width, height).map(|p| p.shutter_row())).collect();
        let (Some(rows), Some(out)) = (rows, self.buf_c.as_ref()) else { return false };
        let layer = &self.stack_layers[i];
        let mut enc = self.shutter_pass(layer, &rows, out, rows.len() as u32, 0);
        enc.copy_buffer_to_buffer(out, 0, layer, 0, self.stack_pixel_count() * 16);
        self.submit(enc);
        true
    }

    /// Layer `i` as `placement` (a Transform) places it, added premultiplied
    /// to the shutter's running sum, which `first` starts afresh: one sample
    /// of a picture that is drawn anew at each instant the shutter is open,
    /// as the CPU's `motion_blurred` adds a title redrawn at each.
    /// [`Self::stack_shutter_finish`] gives the average back.
    pub fn stack_shutter_add(&mut self, i: usize, placement: &BuiltinEffect, first: bool) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        let Some(row) = TransformParams::of(placement, width, height).map(|p| p.shutter_row()) else { return false };
        let size = self.stack_pixel_count() * 16;
        if self.shutter_sum.as_ref().is_none_or(|b| b.size() != size) {
            self.shutter_sum = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("shutter_sum"),
                size,
                usage: wgpu::BufferUsages::STORAGE,
                mapped_at_creation: false,
            }));
        }
        let Some(sum) = self.shutter_sum.as_ref() else { return false };
        let enc = self.shutter_pass(&self.stack_layers[i], &[row], sum, 1, if first { 1 } else { 3 });
        self.submit(enc);
        true
    }

    /// The shutter's running sum of `n` samples into layer `i`, averaged
    /// premultiplied and given back straight, as `stack_shutter` gives its
    /// own.
    pub fn stack_shutter_finish(&mut self, i: usize, n: u32) -> bool {
        if self.is_device_lost() || i >= self.stack_layers.len() || n == 0 {
            return false;
        }
        let Some(sum) = self.shutter_sum.as_ref() else { return false };
        // The pass reads no placement in this mode, but its binding wants one.
        let enc = self.shutter_pass(sum, &[[0.0; 16]], &self.stack_layers[i], n, 2);
        self.submit(enc);
        true
    }

    /// One shutter pass over the frame, `input` to `output`, in `mode`
    /// (the shader's), with `rows` as its samples: the encoder, unsubmitted.
    fn shutter_pass(&self, input: &wgpu::Buffer, rows: &[[f32; 16]], output: &wgpu::Buffer, n: u32, mode: u32) -> wgpu::CommandEncoder {
        let (width, height) = self.stack_dims;
        let samples = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shutter_samples"),
            size: (rows.len() * 64) as u64,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: true,
        });
        samples.slice(..).get_mapped_range_mut().copy_from_slice(bytemuck::cast_slice(rows));
        samples.unmap();
        self.write_params(&ShutterParams { width, height, n, mode });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("stack_shutter"),
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: samples.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });
        let mut enc = self.encoder(Some("stack_shutter"));
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.shutter_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups((self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        enc
    }

    /// Posed piece transitions (#985) on the stack: layers `from` and `to`
    /// into `out`, mixed premultiplied as `stack_transition` mixes.
    pub fn stack_pieces(&mut self, from: usize, to: usize, out: usize, plan: &crate::pieces::PiecePlan) -> bool {
        if self.is_device_lost() || from >= self.stack_layers.len() || to >= self.stack_layers.len() || out >= self.stack_layers.len() {
            return false;
        }
        let (width, height) = self.stack_dims;
        if (plan.width, plan.height) != (width, height) {
            return false;
        }
        let wg = (self.stack_pixel_count() as u32).div_ceil(WORKGROUP_SIZE);
        let (Some(from_pm), Some(to_pm)) = (self.buf_a.as_ref(), self.buf_b.as_ref()) else { return false };
        self.write_params(&AlphaFormParams { width, height, unpremultiply: 0, _pad: 0 });
        let (from_layer, to_layer) = (std::sync::Arc::clone(&self.stack_layers[from]), std::sync::Arc::clone(&self.stack_layers[to]));
        self.stack_pass(&self.alpha_form_pipeline, &from_layer, from_pm, wg, "stack_pieces_premultiply_from");
        self.stack_pass(&self.alpha_form_pipeline, &to_layer, to_pm, wg, "stack_pieces_premultiply_to");
        self.dispatch_pieces(from_pm, to_pm, self.buf_c.as_ref().unwrap(), plan, "stack_pieces");
        self.write_params(&AlphaFormParams { width, height, unpremultiply: 1, _pad: 0 });
        let out_layer = std::sync::Arc::clone(&self.stack_layers[out]);
        self.stack_pass(&self.alpha_form_pipeline, self.buf_c.as_ref().unwrap(), &out_layer, wg, "stack_pieces_unpremultiply");
        true
    }

    /// One pass over two inputs (bindings 0 and 1) into `output`, with
    /// the params already written: the layout the blend shaders share.
    fn dispatch_pair(&self, pipeline: &wgpu::ComputePipeline, a: &wgpu::Buffer, b: &wgpu::Buffer, output: &wgpu::Buffer, workgroups: u32) {
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pair_bg"),
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_binding() },
            ],
        });
        let mut enc = self.encoder(None);
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.submit(enc);
        self.device.poll(wgpu::Maintain::Poll);
    }
}

// ─── Param structs (must be Pod for bytemuck) ────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlendLayerParams {
    width: u32,
    height: u32,
    opacity: f32,
    mode: u32,
    y0: u32,
    y1: u32,
    _p0: u32,
    _p1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpriteGlyphsParams {
    width: u32,
    height: u32,
    y0: u32,
    y1: u32,
    n: u32,
    ux0: u32,
    uy0: u32,
    uw: u32,
    uh: u32,
    /// The box of the frame the sprite's half floats cover (#190).
    bx0: u32,
    by0: u32,
    bw: u32,
    bh: u32,
    _p0: u32,
    _p1: u32,
    _p2: u32,
    /// BT.709 → working space, rows padded to four (#1279).
    to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpriteBoxParams {
    width: u32,
    height: u32,
    bx0: u32,
    by0: u32,
    bw: u32,
    bh: u32,
    _p0: u32,
    _p1: u32,
    /// BT.709 → working space, rows padded to four (#1279).
    to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpriteRowsParams {
    width: u32,
    height: u32,
    y0: u32,
    y1: u32,
    /// BT.709 → working space, rows padded to four (#1279).
    to_working: [[f32; 4]; 3],
}

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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SpritePlaceParams {
    width: u32,
    height: u32,
    y0: u32,
    y1: u32,
    dx: i32,
    dy: i32,
    opacity: f32,
    scale: f32,
    anchor_x: f32,
    anchor_y: f32,
    /// The full-frame colour under the sprite, linear, straight alpha;
    /// alpha 0 means none.
    under_r: f32,
    under_g: f32,
    under_b: f32,
    under_a: f32,
    /// [`SpriteShape`]'s focus band: axis 0 none, 1 down, 2 across.
    focus_axis: u32,
    band: f32,
    edge_opacity: f32,
    /// [`SpriteShape`]'s edge fade zones, fractions of the frame.
    fade_top: f32,
    fade_bottom: f32,
    fade_left: f32,
    fade_right: f32,
    /// The box of the frame the sprite's half floats cover (#190).
    bx0: u32,
    by0: u32,
    bw: u32,
    bh: u32,
    /// 1: laid over what the layer holds, as the title renderer lays one
    /// text layer over another (#2069); 0: the layer is the sprite. A
    /// glyph's: 2 its coverage into the face mask, 3 taken out of it, 4 its
    /// native colour laid over; a posed one's alpha rounds to a byte (#2203).
    over: u32,
    _pad1: u32,
    _pad2: u32,
    /// BT.709 → working space, rows padded to four (#1279).
    to_working: [[f32; 4]; 3],
    to_authored: [[f32; 4]; 3],
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

/// A title's resting render on the GPU, the box it covers as packed
/// sRGB half floats (#190); see `stack_sprites`.
struct ResidentSprite {
    id: u64,
    buffer: wgpu::Buffer,
    used: u64,
    /// The sprite's glyph map, `(x0, y0, w, h, two u16 per u32)`, once a
    /// staggered motion has asked for it.
    units: Option<(u32, u32, u32, u32, wgpu::Buffer)>,
}

/// Sprites kept on the GPU at once: the box each covers, eight bytes a
/// pixel.
const STACK_SPRITE_CAP: usize = 512;
/// The most the resident sprites take together (#190): a guard, since a
/// title's box is usually far smaller than its frame; the sprite being
/// placed is always kept.
const STACK_SPRITE_BYTES: u64 = 200 << 20;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MatteExtractParams {
    width: u32,
    height: u32,
    channel: u32,
    invert: u32,
    luma_r: f32,
    luma_g: f32,
    luma_b: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskPoolParams {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AlphaFormParams {
    width: u32,
    height: u32,
    /// 0 premultiplies, 1 divides back to straight.
    unpremultiply: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ScaleAlphaParams {
    width: u32,
    height: u32,
    factor: f32,
    _pad: u32,
}


#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FilmGrainParams {
    width: u32,
    height: u32,
    amount: f32,
    /// Grain cells per pixel (`cpu::grain_cells_per_px`), not the size:
    /// the shader multiplies by it, as the CPU does (#1441).
    per_px: f32,
    color: u32,
    shadows: f32,
    seed: u32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FocusBlurParams {
    width: u32,
    height: u32,
    vertical: u32,
    band: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TiltShiftParams {
    width: u32,
    height: u32,
    center: f32,
    band: f32,
    softness: f32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OldFilmParams {
    width: u32,
    height: u32,
    flicker: f32,
    scratches: f32,
    dust: f32,
    weave: f32,
    sepia: f32,
    seed: u32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VhsParams {
    width: u32,
    height: u32,
    tracking: f32,
    bleed: f32,
    noise: f32,
    scanlines: f32,
    seed: u32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OpacityParams {
    width: u32,
    height: u32,
    value: f32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ColorFadeParams {
    width: u32,
    height: u32,
    progress: f32,
    _pad: u32,
    /// Linear RGB and alpha.
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BridgeU8Params {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BridgeU8FitParams {
    src_w: u32,
    src_h: u32,
    out_w: u32,
    out_h: u32,
    dst_w: u32,
    dst_h: u32,
    x_off: u32,
    y_off: u32,
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

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Yuv420Params {
    width: u32,
    height: u32,
    tone_map_op: u32,
    chroma_word_offset: u32,
    kr: f32,
    kg: f32,
    kb: f32,
    semi_planar: u32,
    bits: u32,
    sample_shift: u32,
    gamut_op: u32,
    hdr_op: u32,
    peak_nits: f32,
    scene_op: u32,
    scene_kind: u32,
    scene_scale: f32,
    scene_exposure: f32,
    _pad2: u32,
    _pad3: u32,
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

/// A table set uploaded to the display-transform binding, with the
/// floats it was built from so a different set replaces it.
struct CachedTables {
    buf: wgpu::Buffer,
    data: Vec<f32>,
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
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Rgba64BridgeParams {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
    /// Source primaries → working space, 3 rows of vec4 (w unused).
    source_to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct Hdr10BridgeParams {
    width: u32,
    height: u32,
    transfer_id: u32,
    /// Curve output -> scene-linear: 100 for PQ/HLG, 1 for camera logs.
    scene_scale: f32,
    /// Source primaries → working space, 3 rows of vec4 (w unused).
    source_to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct DisplayU8Params {
    width: u32,
    height: u32,
    hdr_tonemap: u32,
    /// Exposure ahead of the tone map (`HdrTransfer::display_exposure`).
    exposure: f32,
    /// 0 the ordinary SDR encode, 1 HDR monitoring's preview roll-off.
    view_op: u32,
    /// `HdrTransfer::display_light_kind` for a scene-referred clip, whose
    /// own last step runs before the preview roll-off; 3 or more for a
    /// display-referred one, which has none.
    scene_kind: u32,
    scene_scale: f32,
    peak_nits: f32,
    /// The working space's luma weights, for the HLG branch of the
    /// display-light pass. `w` is padding: a uniform `vec4` needs it.
    luma: [f32; 4],
}

#[repr(C)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct NitMapParams {
    width: u32,
    height: u32,
    step: u32,
    map_width: u32,
    to_2020: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SharpenParams {
    width: u32,
    height: u32,
    amount: f32,
    _pad: u32,
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

/// The shutter pass's size and sample count (#1923).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ShutterParams {
    width: u32,
    height: u32,
    n: u32,
    mode: u32,
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ChromaKeyParams {
    width: u32,
    height: u32,
    key_cb: f32,
    key_cr: f32,
    tolerance: f32,
    soft_edge: f32,
    /// Spill method as `u32` (0 = None, 1 = ChannelClamp, 2 = Hawk).
    /// WGSL uniforms can't carry Rust enums directly so we encode here.
    spill_method: u32,
    spill_strength: f32,
    /// Index of the dominant key channel (0 = R, 1 = G, 2 = B).
    /// Pre-computed CPU-side from `key_color` so the shader avoids a
    /// per-pixel argmax. Tied to `dominant_channel` in chroma_key.rs.
    spill_key_idx: u32,
    /// Round to 16-byte alignment — WGSL uniforms require it.
    _pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomThresholdParams {
    width: u32,
    height: u32,
    threshold: f32,
    _pad: u32,
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
struct BloomBlendParams {
    width: u32,
    height: u32,
    intensity: f32,
    /// Desaturation of the blended layer (screen blend only; Diffusion).
    desat: f32,
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
struct LightRaysParams {
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    threshold: f32,
    samples: u32,
    decay: f32,
    intensity: f32,
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
struct LivingGradientParams {
    width: u32,
    height: u32,
    glow: f32,
    grain: f32,
    a: [f32; 4],
    b: [f32; 4],
    dir: [f32; 2],
    centre: [f32; 2],
    to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GrainGeneratorParams {
    width: u32,
    height: u32,
    frame: u32,
    _pad: u32,
    color: [f32; 4],
    cell: f32,
    depth: f32,
    _p: [f32; 2],
    to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SegmentsParams {
    width: u32,
    height: u32,
    count: u32,
    _pad: u32,
    background: [f32; 4],
    color: [f32; 4],
    to_working: [[f32; 4]; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GeneratorParams {
    width: u32,
    height: u32,
    shape: u32,
    _pad: u32,
    a: [f32; 4],
    b: [f32; 4],
    dir: [f32; 2],
    centre: [f32; 2],
    radius: f32,
    _pad2: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct VignetteParams {
    width: u32,
    height: u32,
    amount: f32,
    size: f32,
    softness: f32,
    roundness: f32,
    _pad: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LensFlareParams {
    width: u32,
    height: u32,
    pos_x: f32,
    pos_y: f32,
    brightness: f32,
    flare_size: f32,
    ghost_count: u32,
    ghost_intensity: f32,
    chromatic_aberration: f32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct AnamorphicStreakParams {
    width: u32,
    height: u32,
    threshold: f32,
    half_len: u32,
    intensity: f32,
    tint: f32,
    _pad: u32,
    _pad2: u32,
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
struct FilmHalationBlendParams {
    width: u32,
    height: u32,
    threshold: f32,
    intensity: f32,
    hal_r: f32,
    hal_g: f32,
    hal_b: f32,
    _pad: u32,
    /// Working-space luminance weights, for the brightness this effect
    /// gates on. `GpuEffectsPipeline::set_luma_coefs` installs them; the
    /// CPU twin reads the same numbers.
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad_luma: u32,
}

/// `SHADER_PIECES`' uniform (#985): the frame, and how many posed pieces
/// an instant and instants the plan holds.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PiecesParams {
    width: u32,
    height: u32,
    count: u32,
    samples: u32,
    background_to: u32,
    /// 1 when the subject is cut out (#985 phase 3), 2 with its matte.
    subject: u32,
    _pad1: u32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TransitionParams {
    width: u32,
    height: u32,
    progress: f32,
    kind_id: u32, // 0=CrossDissolve, 1=WipeLeft, 2=WipeRight, 3=WipeUp, 4=WipeDown, 5=DipToBlack, 6=DipToWhite
    // Generic per-kind user params. Meaning is a per-kind contract — see the
    // WGSL shader branches. Keep field layout identical to the WGSL struct.
    param0: f32,
    param1: f32,
    param2: f32,
    param3: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
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

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuColorGradeParams {
    width: u32,
    height: u32,
    exposure: f32,
    contrast: f32,
    saturation: f32,
    /// Temperature / tint as a white-balance matrix, row-major
    /// (`klipp_color::white_balance`).
    wb: [f32; 9],
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    lift_r: f32,
    lift_g: f32,
    lift_b: f32,
    gamma_r: f32,
    gamma_g: f32,
    gamma_b: f32,
    gain_r: f32,
    gain_g: f32,
    gain_b: f32,
    offset_r: f32,
    offset_g: f32,
    offset_b: f32,
    hue: f32,
    global_mix: f32,
    /// Where contrast pivots, already grade-encoded.
    ///
    /// Sits in what was `_pad0`, so the uniform's size and every field
    /// offset after it are unchanged. Encoded on this side rather than
    /// in the shader: one `powf` per frame instead of one per sample,
    /// and the CPU twin is handed the identical number rather than
    /// deriving its own, which is a way parity drifts.
    contrast_pivot_encoded: f32,
    /// Working-space luma coefficients used by the saturation /
    /// tone-controls / lift-gamma-gain stages of `SHADER_COLOR_GRADE`.
    /// `(0.2126, 0.7152, 0.0722)` for Rec709Linear,
    /// `(0.2722, 0.6741, 0.0537)` for AcesCG (AP1), etc. Populated by
    /// the engine via `GpuEffectsPipeline::set_luma_coefs`.
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    /// The grade encoding, `klipp_grade_model::Encoding::as_u8` (#467).
    encoding: u32,
    /// Non-zero: log-mode wheels (`log_wheels` = low, mid, high RGB)
    /// replace lift/gamma/gain/offset.
    log_mode: u32,
    log_wheels: [f32; 9],
    /// The log wheels' low and high band centres, and the wheels' luma
    /// mix (#900): `klipp_grade_model::WheelShape`.
    log_low_range: f32,
    log_high_range: f32,
    luma_mix: f32,
}
// 50 words = 200 bytes (still under 256-byte PARAMS_SIZE)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuHueSatParams {
    width: u32,
    height: u32,
    mix: f32,
    /// Bit `c` set: curve `c` was edited and applies.
    active: u32,
}
// 4 fields × 4 bytes = 16 bytes

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuColorBalanceParams {
    width: u32,
    height: u32,
    /// Temperature / tint as a white-balance matrix, row-major.
    wb: [f32; 9],
    hue: f32,
    global_mix: f32,
    _pad0: u32,
    _pad1: u32,
}

/// `SHADER_HDR_ZONES`'s uniform: each zone's gain in stops per channel
/// and its saturation, then its boundary and falloff. 256 bytes, the
/// whole params slot.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuHdrZonesParams {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
    luma: [f32; 4],
    gain_sat: [[f32; 4]; klipp_grade_model::hdr_zones::ZONES],
    edge: [[f32; 4]; klipp_grade_model::hdr_zones::ZONES],
}

impl GpuHdrZonesParams {
    fn new(width: u32, height: u32, t: &klipp_grade_model::hdr_zones::Terms) -> Self {
        let mut gain_sat = [[0.0; 4]; klipp_grade_model::hdr_zones::ZONES];
        let mut edge = [[0.0; 4]; klipp_grade_model::hdr_zones::ZONES];
        for i in 0..klipp_grade_model::hdr_zones::ZONES {
            let g = t.gain_stops[i];
            gain_sat[i] = [g[0], g[1], g[2], t.saturation[i]];
            edge[i] = [t.boundary[i], t.falloff[i], 0.0, 0.0];
        }
        Self { width, height, _pad0: 0, _pad1: 0, luma: [t.luma[0], t.luma[1], t.luma[2], 0.0], gain_sat, edge }
    }
}

const _: () = assert!(std::mem::size_of::<GpuHdrZonesParams>() as u64 <= PARAMS_SIZE);

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuWheelSatBriParams {
    width: u32,
    height: u32,
    lift_sat: f32,
    lift_bri: f32,
    gamma_sat: f32,
    gamma_bri: f32,
    gain_sat: f32,
    gain_bri: f32,
    offset_sat: f32,
    offset_bri: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuRgbCurvesParams {
    width: u32,
    height: u32,
    preserve_luma: u32,
    mix: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad0: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct DeflickerParams {
    width: u32, height: u32, gain: f32, strength: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVibranceParams {
    width: u32, height: u32, amount: f32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
}

/// [`shader_scene_to_display`]'s uniform: `klipp_color::hdr::SceneToDisplay`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuSceneToDisplayParams {
    width: u32, height: u32,
    /// 1 for display light (a peak is set), 0 for the SDR tone map.
    light: u32,
    /// `HdrTransfer::display_light_kind`.
    kind: u32,
    scene_scale: f32, exposure: f32, peak: f32, _pad: u32,
    luma_r: f32, luma_g: f32, luma_b: f32, _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuColorWarperParams {
    width: u32, height: u32, _pad0: u32, _pad1: u32,
    hue_shifts: [f32; 12],
    sat_shifts: [f32; 12],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuInputTransformParams {
    width: u32, height: u32, _pad0: u32, _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuClarityBlurParams {
    width: u32, height: u32, radius: u32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuClarityApplyParams {
    width: u32, height: u32, amount: f32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuGradeMixParams {
    width: u32,
    height: u32,
    mix: f32,
    _pad: u32,
}

/// A gate's blend: its mix, and which channel of its matte is the
/// coverage (`MaskChannel` as numbered: red, luminance, alpha), with the
/// working space's luma weights for the second.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuApplyMatteParams {
    width: u32,
    height: u32,
    mix: f32,
    channel: u32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    /// 1: the matte taken inverted, an outside node's key (#898). The
    /// word that padded the struct to 32 bytes; the size is unchanged.
    invert: u32,
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
    shape_type: u32,     // 0=circle, 1=rect, 2=gradient, 3=bezier
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
        "
fn unpack(v: vec4<f32>) -> vec4<f32> { return v; }
fn pack(v: vec4<f32>) -> vec4<f32> { return v; }
"
    };
}

/// [`crate::dither`] as WGSL: the display encodes' dither and rounding,
/// the export's own pattern (#2072). The literals are
/// `DITHER_HASH_MUL_A`, `DITHER_HASH_MUL_B`, `DITHER_SECOND_DRAW` and
/// `ON_CODE_TOLERANCE`, which `display_dither_wgsl_carries_the_rust_constants`
/// holds equal,
/// and `4294967296.0` is `u32::MAX as f32`. Computed in the encode's own
/// pass from the pixel's index alone: no buffer, no extra pass, and the
/// same pixels every time a frame is drawn.
macro_rules! shader_display_dither_wgsl {
    () => {
        "
fn dither_hash(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

/// `klipp_effects::dither::dither_at`: triangular, in code values, in
/// [-1, 1], from the pixel's index `y * width + x`.
fn dither_at(index: u32) -> f32 {
    let a = f32(dither_hash(index)) / 4294967296.0;
    let b = f32(dither_hash(index ^ 0x9e3779b9u)) / 4294967296.0;
    return a + b - 1.0;
}

/// `klipp_effects::dither::quantise` at 8 bits: a value on 0 or 1, or
/// already on a code value (within `ON_CODE_TOLERANCE`), is not dithered.
fn quantise_u8(v: f32, d: f32) -> u32 {
    let c = clamp(v, 0.0, 1.0);
    let x = c * 255.0;
    var n = d;
    if (c <= 0.0 || c >= 1.0 || abs(x - round(x)) < 1e-3) { n = 0.0; }
    return u32(clamp(x + 0.5 + n, 0.0, 255.0));
}
"
    };
}

/// [`crate::generator_grain`] as WGSL: the smooth generators' grain
/// (#2073), the CPU's to the bit. The literals are `DITHER_HASH_MUL_A`,
/// `DITHER_HASH_MUL_B` (the hash both share), `GRAIN_SALTS` and
/// `GRAIN_TAPER_CODES` (`63.75` is `255 / 4`), which
/// `generator_grain_wgsl_carries_the_rust_constants` holds equal.
/// Computed in the generator's own pass from the pixel's index alone.
macro_rules! shader_generator_grain_wgsl {
    () => {
        "
fn grain_hash(v: u32) -> u32 {
    var x = v;
    x = x ^ (x >> 16u);
    x = x * 0x7feb352du;
    x = x ^ (x >> 15u);
    x = x * 0x846ca68bu;
    x = x ^ (x >> 16u);
    return x;
}

/// `klipp_effects::generator_grain::grain_noise`.
fn grain_noise(index: u32) -> f32 {
    let a = f32(grain_hash(index ^ 0x85ebca6bu) >> 8u) / 16777216.0;
    let b = f32(grain_hash(index ^ 0xc2b2ae35u) >> 8u) / 16777216.0;
    let c = f32(grain_hash(index ^ 0x27d4eb2fu) >> 8u) / 16777216.0;
    return (a + b + c - 1.5) * 2.0;
}

/// `klipp_effects::generator_grain::grain_channel`.
fn grain_channel(v: f32, n: f32, s: f32) -> f32 {
    let taper = clamp(min(v, 1.0 - v) * 63.75, 0.0, 1.0);
    return clamp(v + n * s * taper / 255.0, 0.0, 1.0);
}
"
    };
}

/// [`crate::UNPREMULTIPLY_MIN_ALPHA`] as WGSL: the same literal, which a
/// test holds equal to the constant.
macro_rules! unpremultiply_min_alpha_wgsl {
    () => {
        "1e-6"
    };
}

/// A layer smeared across the camera's shutter (#1923), in one pass: each
/// pixel is the mean of the layer as each sample's transform places it,
/// the Transform's own inverse map and bilinear sample (transparent
/// outside the picture), averaged premultiplied and given back straight,
/// as the CPU's `shuttered` averages (#2017). `samples` holds a
/// `Sample` of four vec4s each: anchor and position; rotation and inverse
/// scale; tilt; focal length.
const SHADER_SHUTTER: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
// `mode`: 0 averages all `n` samples of `input`; 1 and 3 place sample 0
// of `input` and add it, premultiplied, to `output` (1 starting it afresh);
// 2 gives `input`, such a sum of `n` samples, back averaged and straight.
struct Params { width: u32, height: u32, n: u32, mode: u32 }
// A sample's placement: anchor and position; rotation and inverse scale;
// tilt; focal length. Numbers, not pixels.
struct Sample { place: vec4<f32>, turn: vec4<f32>, tilt: vec4<f32>, lens: vec4<f32> }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> samples: array<Sample>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;

fn texel(x: i32, y: i32) -> vec4<f32> {
    if x < 0 || y < 0 || x >= i32(params.width) || y >= i32(params.height) {
        return vec4<f32>(0.0);
    }
    return unpack(input[u32(y) * params.width + u32(x)]);
}

// The picture as sample `k` places it, at pixel (x, y): the Transform
// shader's inverse map and bilinear read, line for line.
fn placed(k: u32, x: f32, y: f32) -> vec4<f32> {
    let a = samples[k].place;
    let r = samples[k].turn;
    let t = samples[k].tilt;
    let focal = samples[k].lens.x;
    let dx = x - a.x - a.z;
    let dy = y - a.y - a.w;
    let rx = dx * r.x + dy * r.y;
    let ry = -dx * r.y + dy * r.x;
    var sx = rx * r.z;
    var sy = ry * r.w;
    let cos_rx = t.x;
    let sin_rx = t.y;
    let cos_ry = t.z;
    let sin_ry = t.w;
    if cos_rx != 1.0 || cos_ry != 1.0 {
        var u = 0.0;
        var v = 0.0;
        var seen = true;
        if focal > 0.0 {
            let a11 = sx * sin_ry - focal * cos_ry;
            let a12 = -sx * sin_rx * cos_ry - focal * sin_rx * sin_ry;
            let a21 = sy * sin_ry;
            let a22 = -sy * sin_rx * cos_ry - focal * cos_rx;
            let det = a11 * a22 - a12 * a21;
            if abs(det) < 1e-9 {
                seen = false;
            } else {
                let b1 = -sx * focal;
                let b2 = -sy * focal;
                u = (b1 * a22 - a12 * b2) / det;
                v = (a11 * b2 - b1 * a21) / det;
                seen = focal + (u * sin_ry - v * sin_rx * cos_ry) > 0.0;
            }
        } else if abs(cos_rx) < 1e-6 || abs(cos_ry) < 1e-6 {
            seen = false;
        } else {
            v = sy / cos_rx;
            u = (sx - v * sin_rx * sin_ry) / cos_ry;
        }
        if seen {
            sx = u;
            sy = v;
        } else {
            sx = -1.0e9;
            sy = -1.0e9;
        }
    }
    sx = sx + a.x;
    sy = sy + a.y;
    let x0 = i32(floor(sx));
    let y0 = i32(floor(sy));
    let fx = sx - f32(x0);
    let fy = sy - f32(y0);
    let top = mix(texel(x0, y0), texel(x0 + 1, y0), fx);
    let bot = mix(texel(x0, y0 + 1), texel(x0 + 1, y0 + 1), fx);
    return mix(top, bot, fy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    if params.mode == 1u || params.mode == 3u {
        let p = placed(0u, x, y);
        var sum = vec4<f32>(0.0);
        if params.mode == 3u { sum = unpack(output[idx]); }
        output[idx] = pack(sum + vec4<f32>(p.xyz * p.w, p.w));
        return;
    }
    var rgb = vec3<f32>(0.0);
    var covered = 0.0;
    if params.mode == 2u {
        let sum = unpack(input[idx]);
        rgb = sum.xyz;
        covered = sum.w;
    } else {
        for (var k = 0u; k < params.n; k = k + 1u) {
            let p = placed(k, x, y);
            rgb = rgb + p.xyz * p.w;
            covered = covered + p.w;
        }
    }
    var colour = vec3<f32>(0.0);
    if covered > ", unpremultiply_min_alpha_wgsl!(), " { colour = rgb / covered; }
    output[idx] = pack(vec4<f32>(colour, covered / f32(max(params.n, 1u))));
}
"
);

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

/// A scene-referred clip's display-light pass, as WGSL.
///
/// `klipp_color::hdr::hdr_display_light`: a graded scene-linear pixel to
/// HDR display light, 1.0 = 203 cd/m². Its own macro because two shaders
/// need it — the export's fused Y'CbCr writer and the viewer's display
/// encode, for HDR monitoring — and two hand-written copies of a
/// three-branch derivation with a highlight roll-off is how they drift.
///
/// The includer must supply, as its own function:
///
/// ```wgsl
/// fn h_working_luma() -> vec3<f32>   // the working space's luma weights
/// ```
///
/// which the export reads out of its baked table and the viewer out of its
/// params. Everything else here is closed form.
macro_rules! shader_hdr_display_light_wgsl {
    () => {
        "
/// `HDR_REFERENCE_WHITE_NITS`: what display light 1.0 means.
const H_REF_WHITE_NITS: f32 = 203.0;
/// `HLG_NOMINAL_PEAK_NITS` and `HLG_GAMMA`.
const H_HLG_PEAK_NITS: f32 = 1000.0;
const H_HLG_GAMMA: f32 = 1.2;

/// `NITS_PER_LINEAR`: what scene-linear 1.0 means for a PQ source.
const H_NITS_PER_LINEAR: f32 = 100.0;
/// `LOG_HDR_EXPOSURE` and `LOG_HDR_KNEE`.
const H_LOG_HDR_EXPOSURE: f32 = 0.03102416;
const H_LOG_HDR_KNEE: f32 = 0.5;

/// `pq_reference_ootf`: BT.2100's reference OOTF — scene light (1.0 =
/// its 10 000-nit top) to display light, cd/m². The BT.709 OETF followed
/// by the BT.1886 EOTF.
fn h_pq_reference_ootf(e: f32) -> f32 {
    let x = 59.5208 * max(e, 0.0);
    var ep = 0.0;
    if x <= 0.018 {
        ep = 4.5 * x;
    } else {
        ep = 1.099 * pow(x, 0.45) - 0.099;
    }
    return 100.0 * pow(ep, 2.4);
}

/// The OOTF applied odd-symmetrically, so a BT.2020 colour outside the
/// working gamut arrives negative and survives the trip.
fn h_signed_ootf(c: f32) -> f32 {
    let m = h_pq_reference_ootf(abs(c) * H_LOG_HDR_EXPOSURE);
    return select(-m, m, c >= 0.0);
}

/// `roll_off`: compress above `knee` so the curve approaches `peak`
/// without reaching it, with slope 1 at the knee so there is no corner.
fn h_roll_off(x: f32, knee: f32, peak: f32) -> f32 {
    if x <= knee {
        return x;
    }
    let t = (x - knee) / (peak - knee);
    return knee + (peak - knee) * t / (1.0 + t);
}

/// `hdr_display_light`: a graded scene-linear pixel to HDR display light
/// (1.0 = 203 cd/m²). `kind` is `HdrTransfer::display_light_kind` — 0 PQ,
/// 1 HLG, 2 camera log.
///
/// A scene-referred clip's last step of its own, which used to happen on
/// the CPU frame after the grade and is why a real HDR export — one whose
/// *source* is PQ, HLG or log — could not take this path at all.
fn hdr_display_light(rgb: vec3<f32>, kind: u32, scene_scale: f32, peak: f32) -> vec3<f32> {
    var nits = vec3<f32>(0.0);
    if kind == 0u {
        nits = rgb * H_NITS_PER_LINEAR;
    } else if kind == 1u {
        // Undo the decoder's PQ-sized scale to get HLG scene light, then
        // the HLG OOTF. Channels stay signed for the same reason as below.
        let e = rgb / scene_scale;
        let ys = dot(h_working_luma(), e);
        var gain = 0.0;
        if ys > 0.0 {
            gain = H_HLG_PEAK_NITS * pow(ys, H_HLG_GAMMA - 1.0);
        }
        nits = e * gain;
    } else {
        let n = vec3<f32>(h_signed_ootf(rgb.x), h_signed_ootf(rgb.y), h_signed_ootf(rgb.z));
        // Roll off the brightest channel and scale the others with it, so
        // a highlight keeps its hue as it compresses.
        let m = max(n.x, max(n.y, n.z));
        let rolled = h_roll_off(m, H_LOG_HDR_KNEE * peak, peak);
        var k = 1.0;
        if m > 0.0 {
            k = rolled / m;
        }
        nits = n * k;
    }
    // PQ and HLG sources are display light already and are not rolled
    // off: the encoder holds the peak.
    return nits / H_REF_WHITE_NITS;
}
"
    };
}

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
        "
fn srgb_to_linear_component(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    } else {
        return pow((c + 0.055) / 1.055, 2.4);
    }
}

fn linear_to_srgb_component(c: f32) -> f32 {
    if (c <= 0.0031308) {
        return c * 12.92;
    } else {
        return 1.055 * pow(c, 1.0/2.4) - 0.055;
    }
}

",
        shader_pixel_storage_wgsl!(),
        "

fn enc3(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(linear_to_srgb_component(v.x), linear_to_srgb_component(v.y), linear_to_srgb_component(v.z));
}
fn dec3(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(srgb_to_linear_component(v.x), srgb_to_linear_component(v.y), srgb_to_linear_component(v.z));
}

// Screen blend — light added on top — at the picture's own ceiling.
// 1 - (1-o)(1-v) is only a blend while o and v stay under 1: on a
// highlight of 4.0 both factors go negative and their product came back
// as a negative pixel, a black halo where the glow should be. Written as
// o + v - o*v/W with W = max(1, o, v) it is the same formula under 1 and
// keeps adding light above it.
fn screen_at_ceiling(o: f32, v: f32) -> f32 {
    let w = max(max(o, v), 1.0);
    return o + v - o * v / w;
}

fn screen3_at_ceiling(o: vec3<f32>, v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(screen_at_ceiling(o.x, v.x), screen_at_ceiling(o.y, v.y), screen_at_ceiling(o.z, v.z));
}

// sRGB encode / decode that keep the sign, so a negative channel — a
// colour outside the working space's gamut — is not lost to a NaN or a
// clamp.
fn enc_signed(c: f32) -> f32 {
    let e = linear_to_srgb_component(abs(c));
    return select(e, -e, c < 0.0);
}
fn dec_signed(c: f32) -> f32 {
    let d = srgb_to_linear_component(abs(c));
    return select(d, -d, c < 0.0);
}

// The HSL the hue/sat tools work in: the picture as displayed (sRGB
// encoded, where their axes live), and scaled into [0, 1] when the pixel
// is brighter than white, because HSL's saturation is only defined
// there — feeding it a highlight of 4.0 gave a negative saturation that
// clamped to zero, which turned every bright colour grey. `magnitude`
// carries the scale back out.
struct PictureHsl {
    h: f32,
    /// Saturation as a fraction of the brightest channel,
    /// `(max - min) / max` — the HSV/HSB definition, and what the
    /// Hue/Sat curves, the Colour Warper and the Qualifier all read.
    ///
    /// It used to be the HSL one, `(max - min) / (1 - |2l - 1|)`, which
    /// is wrong for these three tools in two ways. A near-white pastel
    /// came back fully saturated — `(1.0, 0.95, 0.95)`, almost no colour
    /// in it, read 1.000, the same as a vivid red highlight — so the
    /// qualifier could not tell rich from pale up there and a
    /// saturation boost had nowhere to go. And it was wrong in range
    /// too: light pastel pink read 0.497 against a mid saturated red's
    /// 0.457, which no colourist would agree with. User's choice,
    /// 2026-09-12.
    s: f32,
    /// Lightness, `(max + min) / 2`. Kept for `picture_lum_axis` — the
    /// luminance the lum-driven lookups and the qualifier's luminance
    /// window use — and *not* what a saturation change holds fixed.
    l: f32,
    /// The brightest channel, normalised. This is what a saturation
    /// change holds fixed, so making a colour more saturated deepens it
    /// rather than brightening it, which is what a saturation control is
    /// expected to do.
    v: f32,
    magnitude: f32,
}

fn picture_hsl(c: vec3<f32>) -> PictureHsl {
    let e = vec3<f32>(enc_signed(c.x), enc_signed(c.y), enc_signed(c.z));
    let magnitude = max(max(max(e.x, e.y), e.z), 1.0);
    let n = e / magnitude;
    let mx = max(max(n.x, n.y), n.z);
    let mn = min(min(n.x, n.y), n.z);
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    var s = 0.0;
    var h = 0.0;
    if delta > 0.0001 {
        s = clamp(delta / max(mx, 0.0001), 0.0, 1.0);
        if mx == n.x {
            h = ((n.y - n.z) / delta) % 6.0;
            if h < 0.0 { h = h + 6.0; }
            h = h / 6.0;
        } else if mx == n.y {
            h = ((n.z - n.x) / delta + 2.0) / 6.0;
        } else {
            h = ((n.x - n.y) / delta + 4.0) / 6.0;
        }
    }
    return PictureHsl(h, s, l, mx, magnitude);
}

// Hue, max-normalised saturation and value back to a normalised encoded
// RGB. The partner of `picture_hsl`'s `s`: chroma is `s * v`, so the
// brightest channel stays where it was and only the others move.
fn hsv_rgb_norm(h: f32, s: f32, v: f32) -> vec3<f32> {
    if s < 0.0001 { return vec3<f32>(v, v, v); }
    let c = s * v;
    let x = c * (1.0 - abs((h * 6.0) % 2.0 - 1.0));
    let m = v - c;
    var rgb: vec3<f32>;
    switch u32(h * 6.0) {
        case 0u: { rgb = vec3<f32>(c, x, 0.0); }
        case 1u: { rgb = vec3<f32>(x, c, 0.0); }
        case 2u: { rgb = vec3<f32>(0.0, c, x); }
        case 3u: { rgb = vec3<f32>(0.0, x, c); }
        case 4u: { rgb = vec3<f32>(x, 0.0, c); }
        default: { rgb = vec3<f32>(c, 0.0, x); }
    }
    return rgb + vec3<f32>(m, m, m);
}

// The inverse of `picture_hsl`: hue, saturation and value back to
// linear light. `magnitude` puts a pixel that was brighter than white
// back where it was.
fn picture_rgb(h: f32, s: f32, v: f32, magnitude: f32) -> vec3<f32> {
    let e = hsv_rgb_norm(h, s, v) * magnitude;
    return vec3<f32>(dec_signed(e.x), dec_signed(e.y), dec_signed(e.z));
}

// The luminance axis those tools look up on: the encoded lightness, with
// everything above white at the top of the axis.
fn picture_lum_axis(p: PictureHsl) -> f32 {
    return clamp(p.l * p.magnitude, 0.0, 1.0);
}

// Hue rotation by `degrees` in HSL, the hue slider's meaning: a colour's
// HSL hue moves by exactly that much. HSL needs bounded input, so a pixel
// brighter than 1 is scaled into range and back. Greys are left alone.
fn hue_rotate_hsl(c: vec3<f32>, degrees: f32) -> vec3<f32> {
    let magnitude = max(max(max(c.x, c.y), c.z), 1.0);
    let r = c.x / magnitude;
    let g = c.y / magnitude;
    let b = c.z / magnitude;
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    if delta < 0.0001 { return c; }
    let s = delta / (1.0 - abs(2.0 * l - 1.0));
    var h: f32;
    if mx == r {
        h = ((g - b) / delta) % 6.0;
        if h < 0.0 { h = h + 6.0; }
        h = h / 6.0;
    } else if mx == g {
        h = ((b - r) / delta + 2.0) / 6.0;
    } else {
        h = ((r - g) / delta + 4.0) / 6.0;
    }
    h = (h + degrees / 360.0) % 1.0;
    if h < 0.0 { h = h + 1.0; }
    let cc = (1.0 - abs(2.0 * l - 1.0)) * s;
    let x = cc * (1.0 - abs((h * 6.0) % 2.0 - 1.0));
    let m = l - cc * 0.5;
    var o: vec3<f32>;
    switch u32(h * 6.0) {
        case 0u: { o = vec3<f32>(cc, x, 0.0); }
        case 1u: { o = vec3<f32>(x, cc, 0.0); }
        case 2u: { o = vec3<f32>(0.0, cc, x); }
        case 3u: { o = vec3<f32>(0.0, x, cc); }
        case 4u: { o = vec3<f32>(x, 0.0, cc); }
        default: { o = vec3<f32>(cc, 0.0, x); }
    }
    return (o + vec3<f32>(m, m, m)) * magnitude;
}
"
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
        "
struct MaskRecord {
    shape_type: u32,
    flags: u32,
    bezier_offset: u32,
    bezier_count: u32,
    cx: f32, cy: f32, p1: f32, p2: f32,
    p3: f32, feather: f32, feather_inner: f32, _pad1: u32,
    w0: vec4<f32>, w1: vec4<f32>, w2: vec4<f32>,
}


// As klipp_core::soft_edge: covered inside `inner` past the line, falling
// off to `outer` outside it.
fn soft_edge_sdf(dist: f32, inner_in: f32, outer_in: f32) -> f32 {
    let inner = max(inner_in, 0.0);
    let outer = max(outer_in, 0.0);
    let span = inner + outer;
    if dist <= -inner { return 1.0; }
    if span > 0.0001 && dist < outer {
        let t = (dist + inner) / span;
        return 1.0 - t * t * (3.0 - 2.0 * t);
    }
    return 0.0;
}

// Every record joined in order, as klipp_core::MaskCombine::apply: the
// matte starts empty, or whole when the first record doesn't add (#74).
fn window_matte(nx: f32, ny: f32, aspect: f32, count: u32) -> f32 {
    var matte: f32 = 0.0;
    if count > 0u && ((records[0].flags >> 2u) & 3u) != 0u { matte = 1.0; }
    for (var i: u32 = 0u; i < count; i = i + 1u) {
        let m = eval_record(records[i], nx, ny, aspect);
        let c = (records[i].flags >> 2u) & 3u;
        if c == 1u {
            matte = min(matte, 1.0 - m);
        } else if c == 2u {
            matte = min(matte, m);
        } else {
            matte = max(matte, m);
        }
    }
    return matte;
}

// Signed-distance to a closed polygon using the same winding-
// number + min edge distance algorithm as the CPU rasteriser at
// klipp-color/src/masks.rs:80-115. Returns negative inside,
// positive outside, in normalised width-relative units.
fn bezier_sdf(nx: f32, ny: f32, aspect: f32, offset: u32, count: u32) -> f32 {
    if count < 3u { return 1.0; }
    let px = nx;
    let py = ny * aspect;
    var min_dist_sq: f32 = 1.0e30;
    var winding: i32 = 0;
    for (var i: u32 = 0u; i < count; i = i + 1u) {
        let a_raw = points[offset + i];
        let b_raw = points[offset + ((i + 1u) % count)];
        let ax = a_raw.x;
        let ay = a_raw.y * aspect;
        let bx = b_raw.x;
        let by = b_raw.y * aspect;
        let ex = bx - ax;
        let ey = by - ay;
        let dx = px - ax;
        let dy = py - ay;
        let denom = ex * ex + ey * ey;
        var t: f32 = 0.0;
        if denom > 0.0 {
            t = clamp((dx * ex + dy * ey) / denom, 0.0, 1.0);
        }
        let cx = ax + t * ex - px;
        let cy = ay + t * ey - py;
        let d2 = cx * cx + cy * cy;
        if d2 < min_dist_sq { min_dist_sq = d2; }
        // Winding-number contribution. CPU uses the classic
        // ray-casting algorithm — match the branches exactly so
        // the parity test stays clean.
        if ay <= py {
            if by > py {
                let cross = ex * (py - ay) - ey * (px - ax);
                if cross > 0.0 { winding = winding + 1; }
            }
        } else {
            if by <= py {
                let cross = ex * (py - ay) - ey * (px - ax);
                if cross < 0.0 { winding = winding - 1; }
            }
        }
    }
    let dist = sqrt(min_dist_sq);
    if winding != 0 { return -dist; }
    return dist;
}

fn eval_record(rec: MaskRecord, px: f32, py: f32, aspect: f32) -> f32 {
    // A window tracked in perspective is measured where the pixel was
    // on the frame it was drawn on; past the horizon is outside. As the
    // CPU rasteriser's PlaneMap::apply.
    var nx = px;
    var ny = py;
    var behind = false;
    if (rec.flags & 2u) != 0u {
        let h = vec3<f32>(px, py, 1.0);
        let w = dot(rec.w2.xyz, h);
        if !(w > 1.0e-9) {
            behind = true;
        } else {
            nx = dot(rec.w0.xyz, h) / w;
            ny = dot(rec.w1.xyz, h) / w;
        }
    }
    var dist: f32;
    if behind { dist = 3.0e38; } else {
    switch rec.shape_type {
        case 0u: {
            // circle
            let dx = nx - rec.cx;
            let dy = (ny - rec.cy) * aspect;
            dist = sqrt(dx * dx + dy * dy) - rec.p1;
        }
        case 1u: {
            // rectangle
            let rad = rec.p3 * 3.14159265 / 180.0;
            let cos_r = cos(rad);
            let sin_r = sin(rad);
            let dx = nx - rec.cx;
            let dy = (ny - rec.cy) * aspect;
            let lx = abs(dx * cos_r + dy * sin_r);
            let ly = abs(-dx * sin_r + dy * cos_r);
            let hw = rec.p1 / 2.0;
            let hh = rec.p2 / 2.0;
            let ox = lx - hw;
            let oy = ly - hh;
            let outside = sqrt(max(ox, 0.0) * max(ox, 0.0) + max(oy, 0.0) * max(oy, 0.0));
            let inside = min(max(ox, oy), 0.0);
            dist = outside + inside;
        }
        case 2u: {
            // linear gradient
            let gx = rec.p1 - rec.cx;
            let gy = (rec.p2 - rec.cy) * aspect;
            let glen2 = gx * gx + gy * gy;
            if glen2 < 1.0e-8 {
                dist = 0.0;
            } else {
                let dx = nx - rec.cx;
                let dy = (ny - rec.cy) * aspect;
                let t = (dx * gx + dy * gy) / glen2;
                dist = t - 0.5;
            }
        }
        case 3u: {
            // bezier polygon
            dist = bezier_sdf(nx, ny, aspect, rec.bezier_offset, rec.bezier_count);
        }
        default: { dist = 1.0; }
    }
    }
    var m = soft_edge_sdf(dist, rec.feather_inner, rec.feather);
    if (rec.flags & 1u) != 0u { m = 1.0 - m; }
    let opacity = f32((rec.flags >> 8u) & 0xFFu) / 255.0;
    return m * opacity;
}

"
    };
}

// Opacity — multiply linear RGBA by a scalar. In linear light,
// opacity 0.5 means "half the photons" (matching DaVinci / Nuke);
// the sRGB u8 version multiplied gamma-encoded bytes, which
// produced visibly darker-than-half output.
mod layer;
mod flow;
pub use flow::FlowKey;
mod mask_mix;

const SHADER_OPACITY: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    value: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = unpack(input[idx]);
    output[idx] = pack(pixel * params.value);
}
"
);

// Colour fade — from a solid colour into the picture (or out to it):
// the pixel mixed with the colour by `progress`, in linear light, as a
// dissolve mixes.
const SHADER_COLOR_FADE: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    progress: f32,
    _pad: u32,
    color: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    output[idx] = pack(mix(params.color, unpack(input[idx]), params.progress));
}
"
);

// Horizontal blur. Averages linear-light RGB — the gamma-encoded
// version underestimated midtone brightness and made edges look
// unnaturally dark. unpack decodes sRGB u8 → linear f32 [0,1];
// pack encodes linear f32 → sRGB u8 at the 8-bit storage boundary.
// Alpha stays linear per sRGB convention.
const SHADER_BLUR_H: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    var sum = vec4<f32>(0.0);
    let diam = 2 * params.radius + 1;
    let inv = 1.0 / f32(diam);
    let w_i32 = i32(params.width);

    for (var dx = -params.radius; dx <= params.radius; dx++) {
        let sx = clamp(i32(x) + dx, 0, w_i32 - 1);
        sum += unpack(input[y * params.width + u32(sx)]);
    }
    output[idx] = pack(sum * inv);
}
"
);

const SHADER_BLUR_V: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    radius: i32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    var sum = vec4<f32>(0.0);
    let diam = 2 * params.radius + 1;
    let inv = 1.0 / f32(diam);
    let h_i32 = i32(params.height);

    for (var dy = -params.radius; dy <= params.radius; dy++) {
        let sy = clamp(i32(y) + dy, 0, h_i32 - 1);
        sum += unpack(input[u32(sy) * params.width + x]);
    }
    output[idx] = pack(sum * inv);
}
"
);

const SHADER_SHARPEN: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let y = idx / params.width;
    let x = idx % params.width;
    if y >= params.height { return; }

    let orig = unpack(input[idx]);

    // 3x3 average of neighbors (excluding center)
    var avg = vec4<f32>(0.0);
    let w_i32 = i32(params.width);
    let h_i32 = i32(params.height);
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let sx = clamp(i32(x) + dx, 0, w_i32 - 1);
            let sy = clamp(i32(y) + dy, 0, h_i32 - 1);
            avg += unpack(input[u32(sy) * params.width + u32(sx)]);
        }
    }
    avg = avg / 9.0;

    // Unsharp mask: original + amount * (original - blurred)
    let sharpened = vec4<f32>(
        orig.x + params.amount * (orig.x - avg.x),
        orig.y + params.amount * (orig.y - avg.y),
        orig.z + params.amount * (orig.z - avg.z),
        orig.w,  // preserve alpha
    );
    output[idx] = pack(sharpened);
}
"
);

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
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
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
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
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
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
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

const SHADER_CHROMA_KEY: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    key_cb: f32,
    key_cr: f32,
    tolerance: f32,
    soft_edge: f32,
    spill_method: u32,
    spill_strength: f32,
    spill_key_idx: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn linear_to_srgb_component(c: f32) -> f32 {
    if (c <= 0.0031308) {
        return c * 12.92;
    } else {
        return 1.055 * pow(c, 1.0/2.4) - 0.055;
    }
}

fn srgb_to_linear_component(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    } else {
        return pow((c + 0.055) / 1.055, 2.4);
    }
}

fn rgb_to_cbcr(r: f32, g: f32, b: f32) -> vec2<f32> {
    let cb = 128.0 + (-0.168736 * r - 0.331264 * g + 0.5 * b);
    let cr = 128.0 + (0.5 * r - 0.418688 * g - 0.081312 * b);
    return vec2<f32>(cb, cr);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    var pixel = unpack(input[idx]);
    // Re-encode to sRGB 0-255 for YCbCr math (matches CPU path + user input).
    let srgb_r = clamp(linear_to_srgb_component(pixel.x), 0.0, 1.0) * 255.0;
    let srgb_g = clamp(linear_to_srgb_component(pixel.y), 0.0, 1.0) * 255.0;
    let srgb_b = clamp(linear_to_srgb_component(pixel.z), 0.0, 1.0) * 255.0;
    let cbcr = rgb_to_cbcr(srgb_r, srgb_g, srgb_b);

    let dcb = (cbcr.x - params.key_cb) / 255.0;
    let dcr = (cbcr.y - params.key_cr) / 255.0;
    let dist = sqrt(dcb * dcb + dcr * dcr);

    if dist < params.tolerance {
        // Fully keyed out — no spill stage needed.
        output[idx] = pack(vec4<f32>(0.0));
        return;
    } else if dist < params.tolerance + params.soft_edge {
        let factor = (dist - params.tolerance) / params.soft_edge;
        pixel.w = pixel.w * factor;
    }

    // Spill suppression on whatever's still visible. Mirrors the CPU
    // `apply_spill` shape: pick a target value the key channel drops
    // toward when it exceeds that target, blend by spill_strength.
    if params.spill_method != 0u && params.spill_strength > 0.0 && pixel.w > 0.0 {
        // On the pixel's own values, unclamped, and only the key channel
        // changes, as on the CPU: above white and below black survive
        // (#1020).
        let srgb = vec3<f32>(linear_to_srgb_component(pixel.x), linear_to_srgb_component(pixel.y), linear_to_srgb_component(pixel.z)) * 255.0;
        let key = srgb[params.spill_key_idx];
        let other_a_idx = (params.spill_key_idx + 1u) % 3u;
        let other_b_idx = (params.spill_key_idx + 2u) % 3u;
        let other_a = srgb[other_a_idx];
        let other_b = srgb[other_b_idx];
        var spill_target: f32 = key;
        if params.spill_method == 1u {
            spill_target = max(other_a, other_b);
        } else if params.spill_method == 2u {
            spill_target = 0.5 * (other_a + other_b);
        }
        if key > spill_target {
            let new_key = key + params.spill_strength * (spill_target - key);
            pixel[params.spill_key_idx] = srgb_to_linear_component(new_key / 255.0);
        }
    }

    output[idx] = pack(pixel);
}
"
);

/// HDR monitoring's light measurement: display light → the brightest
/// channel's level in cd/m², on a coarse grid.
///
/// `klipp_color::hdr_view::HdrMeasure::nit_map`'s twin. The brightest
/// *channel* and not the luminance, measured in BT.2020 whatever the
/// project works in — a saturated highlight runs out of range channel by
/// channel, and measured in a narrower basis the same BT.2020 red reads
/// 67% high. The matrix comes in as data for that reason.
///
/// Coarse on purpose: the readout under the pointer does not need a value
/// per pixel, and a whole-frame map is 33 MB at 4K where a
/// quarter-resolution one is 2.
const SHADER_HDR_NIT_MAP: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, step: u32, map_width: u32,
    to_2020_row0: vec4<f32>,
    to_2020_row1: vec4<f32>,
    to_2020_row2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> nits: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

/// `HDR_REFERENCE_WHITE_NITS`: what display light 1.0 means.
const REF_WHITE_NITS: f32 = 203.0;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let map_height = (params.height + params.step - 1u) / params.step;
    let idx = gid.x;
    if idx >= params.map_width * map_height { return; }
    let mx = idx % params.map_width;
    let my = idx / params.map_width;
    // The same sample the twin takes: the top-left pixel of each cell,
    // clamped to the frame, not an average of it.
    let x = min(mx * params.step, params.width - 1u);
    let y = min(my * params.step, params.height - 1u);
    let px = unpack(input[y * params.width + x]).xyz;
    let c = vec3<f32>(
        dot(params.to_2020_row0.xyz, px),
        dot(params.to_2020_row1.xyz, px),
        dot(params.to_2020_row2.xyz, px),
    );
    nits[idx] = max(c.x, max(c.y, c.z)) * REF_WHITE_NITS;
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

/// A scene-referred clip's display step as a chain pass (phase 2b M2, step
/// 11c): `klipp_color::hdr::SceneToDisplay::apply`, the SDR tone map or
/// display light, from the two shared definitions the decoders' previews
/// and the export's writer use. Signed like the CPU: unlike the viewer's
/// monitor encode it doesn't clamp first, since effects still follow.
const SHADER_SCENE_TO_DISPLAY_BODY: &str = concat!(
    "
struct Params {
    width: u32, height: u32, light: u32, kind: u32,
    scene_scale: f32, exposure: f32, peak: f32, _pad: u32,
    luma_r: f32, luma_g: f32, luma_b: f32, _pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn h_working_luma() -> vec3<f32> {
    return vec3<f32>(params.luma_r, params.luma_g, params.luma_b);
}
",
    shader_pixel_storage_wgsl!(),
    shader_hdr_display_light_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    var rgb = vec3<f32>(0.0);
    if params.light == 1u {
        rgb = hdr_display_light(p.xyz, params.kind, params.scene_scale, params.peak);
    } else {
        rgb = tone_map_hdr_vec(p.xyz, params.exposure);
    }
    output[idx] = pack(vec4<f32>(rgb, p.w));
}
"
);

/// [`SHADER_SCENE_TO_DISPLAY_BODY`] as compiled, after the colour helpers
/// that hold the tone map. Built once, like [`shader_hdr10_to_linear`].
fn shader_scene_to_display() -> &'static str {
    static SRC: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SRC.get_or_init(|| format!("{}{}{}", crate::color_wgsl::WGSL_PRELUDE, crate::color_wgsl::WGSL_HELPERS, SHADER_SCENE_TO_DISPLAY_BODY))
}

// Linear-f32 RGBA → sRGB-u8 RGBA encode. One workgroup-thread per
// output pixel. Output buffer is `array<u32>` where each u32 packs
// RGBA bytes in little-endian order (R = byte 0, A = byte 3),
// matching the `Vec<u8>` format the rest of the engine consumes.
//
// Math is the standard sRGB transfer function (12.92 / 1.055·x^(1/2.4)
// with the linear toe at 0.0031308). Bit-identical to the CPU
// `linear_to_srgb` reference within rounding to u8. The CPU LUT we
// replace was also a quantised approximation (4096 input bins → u8
// output) so the GPU result here is at least as accurate.
// Packed RGB10 HDR code values (source primaries, PQ/HLG-encoded, as the
// P010 decoder writes them) → scene-linear working-space f32 RGBA in the
// chain's input slot. Concatenated after the colourspace prelude, which
// supplies `display_to_linear` and the TRANSFER_* ids. Must match
// `klipp_color::hdr::hdr10_codes_to_scene_linear_rgba`: curve first, then
// the gamut matrix, 1.0 = 100 nits.
const SHADER_HDR10_TO_LINEAR_BODY: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, transfer_id: u32, scene_scale: f32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = input[idx];
    let code = vec3<f32>(f32(p & 1023u), f32((p >> 10u) & 1023u), f32((p >> 20u) & 1023u)) / 1023.0;
    // PQ/HLG return 1.0 = 10 000 nits (scale 100 to 1.0 = 100 nits);
    // camera logs already return reflectance (scale 1).
    let scene = display_to_linear(code, params.transfer_id) * params.scene_scale;
    output[idx] = pack(vec4<f32>( dot(params.m0.xyz, scene), dot(params.m1.xyz, scene), dot(params.m2.xyz, scene), 1.0, ));
}
"
);

// SHADER_HDR10_TO_LINEAR_BODY for 16-bit codes, a source deeper than 10
// bits (#1226): `klipp_color::hdr::pack_rgb16`, R and G in a pixel's
// first word, B in the low half of its second. Must match
// `klipp_color::hdr::hdr16_codes_to_scene_linear_rgba`.
const SHADER_HDR16_TO_LINEAR_BODY: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, transfer_id: u32, scene_scale: f32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let rg = input[idx * 2u];
    let b = input[idx * 2u + 1u];
    let code = vec3<f32>(f32(rg & 65535u), f32(rg >> 16u), f32(b & 65535u)) / 65535.0;
    let scene = display_to_linear(code, params.transfer_id) * params.scene_scale;
    output[idx] = pack(vec4<f32>( dot(params.m0.xyz, scene), dot(params.m1.xyz, scene), dot(params.m2.xyz, scene), 1.0, ));
}
"
);

// Chain output (linear f32 RGBA) → display sRGB u8, for a graded frame
// that nothing else needs on the CPU. With `hdr_tonemap` set it applies
// the HDR display tone map first — the GPU half of
// `klipp_color::hdr::apply_hdr_display_tonemap` — so an HDR clip's last
// step runs here rather than on a 132 MB CPU copy. Without it this is the
// SDR display encode (`ToneMapOp::None`: clamp, sRGB). Same layout as
// SHADER_LINEAR_TO_U8.
const SHADER_LINEAR_TO_DISPLAY_U8: &str = concat!(
    shader_pixel_storage_wgsl!(),
    shader_hdr_display_light_wgsl!(),
    shader_display_dither_wgsl!(),
    "
// `view_op` 1 is HDR monitoring's preview: the frame is display light
// (1.0 = 203 cd/m²) rather than SDR linear, and the light above reference
// white rolls into the top so its structure survives a screen that cannot
// show it. `klipp_color::hdr_view::hdr_to_sdr_preview` is the twin.
struct Params {
    width: u32, height: u32, hdr_tonemap: u32, exposure: f32,
    view_op: u32, scene_kind: u32, scene_scale: f32, peak_nits: f32,
    luma: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

/// The hook `shader_hdr_display_light_wgsl` asks for. Here the weights
/// travel in the params rather than a baked table: this shader's layout
/// has three bindings and every effect shares it.
fn h_working_luma() -> vec3<f32> {
    return params.luma.xyz;
}

/// `hdr_to_sdr_preview`: identity to the knee, then a shoulder that
/// approaches 1 without reaching it.
fn hdr_to_sdr_preview(display_light: f32) -> f32 {
    let knee = 0.75;
    let v = max(display_light, 0.0);
    if v <= knee {
        return v;
    }
    let t = (v - knee) / (1.0 - knee);
    return knee + (1.0 - knee) * (t / (1.0 + t));
}

fn linear_to_srgb_c(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    if (x <= 0.0031308) { return x * 12.92; }
    return 1.055 * pow(x, 1.0/2.4) - 0.055;
}

// Narkowicz ACES fit; constants shared with klipp_color::hdr::filmic.
fn filmic(x: f32) -> f32 {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = unpack(input[idx]);
    var rgb = max(px.xyz, vec3<f32>(0.0));
    if (params.hdr_tonemap != 0u) {
        rgb = vec3<f32>(filmic(rgb.x * params.exposure), filmic(rgb.y * params.exposure), filmic(rgb.z * params.exposure));
    }
    if (params.view_op == 1u) {
        // A scene-referred clip's own last step first, where the ordinary
        // SDR path would have tone-mapped it: monitoring wants the HDR
        // display light the export writes, not the SDR picture.
        if (params.scene_kind < 3u) {
            rgb = hdr_display_light(
                rgb, params.scene_kind, params.scene_scale, params.peak_nits,
            );
        }
        rgb = vec3<f32>(
            hdr_to_sdr_preview(rgb.x),
            hdr_to_sdr_preview(rgb.y),
            hdr_to_sdr_preview(rgb.z),
        );
    }
    // Dithered as the export rounds (#2072); alpha exact.
    let d = dither_at(idx);
    let r = quantise_u8(linear_to_srgb_c(rgb.x), d);
    let g = quantise_u8(linear_to_srgb_c(rgb.y), d);
    let b = quantise_u8(linear_to_srgb_c(rgb.z), d);
    let a = u32(clamp(px.w * 255.0 + 0.5, 0.0, 255.0));
    output[idx] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
"
);

const SHADER_LINEAR_TO_U8: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn linear_to_srgb_c(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    if (x <= 0.0031308) { return x * 12.92; }
    return 1.055 * pow(x, 1.0/2.4) - 0.055;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = unpack(input[idx]);
    // Encode RGB through sRGB transfer, alpha is linear (no gamma).
    let r = u32(clamp(linear_to_srgb_c(pixel.x) * 255.0 + 0.5, 0.0, 255.0));
    let g = u32(clamp(linear_to_srgb_c(pixel.y) * 255.0 + 0.5, 0.0, 255.0));
    let b = u32(clamp(linear_to_srgb_c(pixel.z) * 255.0 + 0.5, 0.0, 255.0));
    let a = u32(clamp(pixel.w * 255.0 + 0.5, 0.0, 255.0));
    // Pack RGBA into a single u32, little-endian (R in byte 0).
    output[idx] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
"
);

// Opposite direction: packed sRGB-u8 RGBA (4 bytes per pixel, one u32)
// → linear-f32 vec4 RGBA. Reads from `input: array<u32>`, writes to
// `output: array<vec4<f32>>`. Same binding layout as SHADER_LINEAR_TO_U8
// so it reuses the pipeline's 3-binding bind_group_layout. Powers the
// phase-4d GPU-resident decode → grade path: the decoder's output
// buffer is plugged in as `input`, the grade chain's `buf_a` is the
// `output`.
// One layer onto the accumulator, in place: `klipp_effects::composite::blend_layer_linear`
// transcribed, mode by mode, in the same arithmetic order, so the stack
// on the GPU is the CPU stack to the level. Mode 13 is a plain lerp of
// every channel, an adjustment layer's opacity. Rows `y0..y1` only.
const SHADER_BLEND_LAYER: &str = concat!(
    "
struct Params { width: u32, height: u32, opacity: f32, mode: u32, y0: u32, y1: u32, _p0: u32, _p1: u32 }
@group(0) @binding(0) var<storage, read> layer: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> acc: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
fn overlay_ch(base: f32, blend: f32) -> f32 {
    if base < 0.5 { return 2.0 * base * blend; }
    return 1.0 - 2.0 * (1.0 - base) * (1.0 - blend);
}
fn soft_light_ch(base: f32, blend: f32) -> f32 {
    if blend < 0.5 { return base - (1.0 - 2.0 * blend) * base * (1.0 - base); }
    var d: f32;
    if base < 0.25 { d = ((16.0 * base - 12.0) * base + 4.0) * base; } else { d = sqrt(base); }
    return base + (2.0 * blend - 1.0) * (d - base);
}
fn dodge_ch(base: f32, blend: f32) -> f32 {
    if blend >= 1.0 { return 1.0; }
    return min(base / (1.0 - blend), 1.0);
}
fn burn_ch(base: f32, blend: f32) -> f32 {
    if blend <= 0.0 { return 0.0; }
    return max(1.0 - (1.0 - base) / blend, 0.0);
}
fn blend3(mode: u32, d: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: { return d + s; }
        case 2u: { return d * s; }
        case 3u: { return vec3<f32>(1.0) - (vec3<f32>(1.0) - d) * (vec3<f32>(1.0) - s); }
        case 4u: { return vec3<f32>(overlay_ch(d.x, s.x), overlay_ch(d.y, s.y), overlay_ch(d.z, s.z)); }
        case 5u: { return vec3<f32>(soft_light_ch(d.x, s.x), soft_light_ch(d.y, s.y), soft_light_ch(d.z, s.z)); }
        case 6u: { return vec3<f32>(overlay_ch(s.x, d.x), overlay_ch(s.y, d.y), overlay_ch(s.z, d.z)); }
        case 7u: { return abs(d - s); }
        case 8u: { return d + s - 2.0 * d * s; }
        case 9u: { return vec3<f32>(dodge_ch(d.x, s.x), dodge_ch(d.y, s.y), dodge_ch(d.z, s.z)); }
        case 10u: { return vec3<f32>(burn_ch(d.x, s.x), burn_ch(d.y, s.y), burn_ch(d.z, s.z)); }
        case 11u: { return min(d, s); }
        case 12u: { return max(d, s); }
        default: { return s; }
    }
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let d = unpack(acc[idx]);
    let s = unpack(layer[idx]);
    if params.mode == 13u {
        acc[idx] = pack(d + (s - d) * params.opacity);
        return;
    }
    let sa = s.w * params.opacity;
    if sa < 0.001 {
        return;
    }
    let da = d.w;
    let b = blend3(params.mode, d.xyz, s.xyz);
    let out_a = sa + da * (1.0 - sa);
    if out_a > 0.0 {
        let c = (b * sa + d.xyz * da * (1.0 - sa)) / out_a;
        acc[idx] = pack(vec4<f32>(c, out_a));
    }
}
"
);

// An sRGB RGBA8 sprite, rows y0..y1, decoded to linear with its alpha
// kept straight.

// A generator's frame as sRGB-encoded half floats, decoded to linear
// where every other layer is decoded — on the GPU. Two halves to a
// u32, so it needs no f16 shader feature, and eight bytes a pixel
// against the sixteen a linear f32 upload costs.
const SHADER_SPRITE_ROWS_F16: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, y0: u32, y1: u32, m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32> }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// The authored BT.709 picture in the working space (#1279): the CPU's
// `authored_to_working`; identity in a Rec.709 project.
fn to_ws(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
}

fn srgb_to_linear_c(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let rg = unpack2x16float(input[idx * 2u]);
    let ba = unpack2x16float(input[idx * 2u + 1u]);
    output[idx] = pack(vec4<f32>(to_ws(vec3<f32>(srgb_to_linear_c(rg.x), srgb_to_linear_c(rg.y), srgb_to_linear_c(ba.x))), ba.y));
}
"
);

// A title's box of packed sRGB half floats into its place in a cleared
// layer (#190): one invocation a box pixel.
const SHADER_SPRITE_BOX_F16: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, bx0: u32, by0: u32, bw: u32, bh: u32, _p0: u32, _p1: u32, m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32> }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
fn to_ws(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
}

fn srgb_to_linear_c(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.bw * params.bh { return; }
    let x = params.bx0 + i % params.bw;
    let y = params.by0 + i / params.bw;
    if x >= params.width || y >= params.height { return; }
    let rg = unpack2x16float(input[i * 2u]);
    let ba = unpack2x16float(input[i * 2u + 1u]);
    output[y * params.width + x] = pack(vec4<f32>(to_ws(vec3<f32>(srgb_to_linear_c(rg.x), srgb_to_linear_c(rg.y), srgb_to_linear_c(ba.x))), ba.y));
}
"
);

const SHADER_SPRITE_ROWS: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, y0: u32, y1: u32, m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32> }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// The authored BT.709 picture in the working space (#1279): the CPU's
// `authored_to_working`; identity in a Rec.709 project.
fn to_ws(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
}

fn srgb_to_linear_c(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let packed = input[idx];
    let r = f32((packed >> 0u) & 0xFFu) / 255.0;
    let g = f32((packed >> 8u) & 0xFFu) / 255.0;
    let b = f32((packed >> 16u) & 0xFFu) / 255.0;
    let a = f32((packed >> 24u) & 0xFFu) / 255.0;
    output[idx] = pack(vec4<f32>(to_ws(vec3<f32>(srgb_to_linear_c(r), srgb_to_linear_c(g), srgb_to_linear_c(b))), a));
}
"
);

// A resident sRGB RGBA8 sprite moved by (dx, dy) and faded, decoded to
// linear with its alpha kept straight, over destination rows y0..y1:
// `klipp_title`'s `shift_and_fade` then the sprite decode, in one. The
// alpha rounds as the CPU rounds it (half away from zero, then a byte).
const SHADER_SPRITE_PLACE: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, y0: u32, y1: u32,
    dx: i32, dy: i32, opacity: f32, scale: f32,
    anchor_x: f32, anchor_y: f32, under_r: f32, under_g: f32,
    under_b: f32, under_a: f32, focus_axis: u32, band: f32,
    edge_opacity: f32, fade_top: f32, fade_bottom: f32, fade_left: f32,
    fade_right: f32, bx0: u32, by0: u32, bw: u32,
    bh: u32, over: u32, _pad1: u32, _pad2: u32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
    a0: vec4<f32>, a1: vec4<f32>, a2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> sprite: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// The authored BT.709 picture in the working space (#1279): the CPU's
// `authored_to_working`; identity in a Rec.709 project.
fn to_ws(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
}

fn srgb_to_linear_c(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb_c(c: f32) -> f32 {
    if (c <= 0.0031308) { return c * 12.92; }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

// The layer's fade by where it is on the frame (#592):
// `klipp_core::title_motion::ScreenFade`, its factor down the frame
// (`along_y`) times its factor across it (`along_x`). A focus band runs
// down the frame on axis 1, across it on axis 2; 0 is none.
fn focus_factor(u: f32) -> f32 {
    let d = abs(u - 0.5) * 2.0;
    return 1.0 - smoothstep(params.band, 1.0, d) * (1.0 - params.edge_opacity);
}
fn shape_alpha(x: i32, y: i32) -> f32 {
    let uy = (f32(y) + 0.5) / f32(params.height);
    let ux = (f32(x) + 0.5) / f32(params.width);
    var ay = 1.0;
    if params.fade_top > 0.0 { ay = ay * smoothstep(0.0, params.fade_top, uy); }
    if params.fade_bottom > 0.0 { ay = ay * smoothstep(0.0, params.fade_bottom, 1.0 - uy); }
    if params.focus_axis == 1u { ay = ay * focus_factor(uy); }
    var ax = 1.0;
    if params.fade_left > 0.0 { ax = ax * smoothstep(0.0, params.fade_left, ux); }
    if params.fade_right > 0.0 { ax = ax * smoothstep(0.0, params.fade_right, 1.0 - ux); }
    if params.focus_axis == 2u { ax = ax * focus_factor(ux); }
    return ay * ax;
}

// The sprite at frame pixel (x, y), straight: colour in sRGB-encoded
// 0..255 units, alpha 0..1; nothing outside its box (#190).
fn texel(x: i32, y: i32) -> vec4<f32> {
    let bx = x - i32(params.bx0);
    let by = y - i32(params.by0);
    if bx < 0 || by < 0 || bx >= i32(params.bw) || by >= i32(params.bh) {
        return vec4<f32>(0.0);
    }
    let i = u32(by) * params.bw + u32(bx);
    let rg = unpack2x16float(sprite[i * 2u]);
    let ba = unpack2x16float(sprite[i * 2u + 1u]);
    return vec4<f32>(rg.x * 255.0, rg.y * 255.0, ba.x * 255.0, ba.y);
}

// A tap of the sprite, premultiplied, in its sRGB-encoded 0..255 units
// (alpha 0..1).
fn tap(x: i32, y: i32) -> vec4<f32> {
    let t = texel(x, y);
    return vec4<f32>(t.xyz * t.w, t.w);
}

// CPU image::resize Triangle: vertical float pass, horizontal pass rounded
// to byte premultiplied RGBA, then integer unpremultiplication.
fn resized_colour(x:i32,y:i32)->vec4<f32> {
    if x<0 || y<0 || x>=i32(params._pad1) || y>=i32(params._pad2) {return vec4<f32>(0.0);}
    let ratio=vec2<f32>(f32(params.bw)/f32(params._pad1),f32(params.bh)/f32(params._pad2));
    let support=max(ratio,vec2<f32>(1.0));
    let centre=(vec2<f32>(f32(x),f32(y))+0.5)*ratio;
    let left=clamp(vec2<i32>(floor(centre-support)),vec2<i32>(0),vec2<i32>(i32(params.bw)-1,i32(params.bh)-1));
    let right=clamp(vec2<i32>(ceil(centre+support)),left+1,vec2<i32>(i32(params.bw),i32(params.bh)));
    var wx=0.0;var wy=0.0;
    for(var sx=left.x;sx<right.x;sx=sx+1) {wx+=max(1.0-abs((f32(sx)-(centre.x-0.5))/support.x),0.0);}
    for(var sy=left.y;sy<right.y;sy=sy+1) {wy+=max(1.0-abs((f32(sy)-(centre.y-0.5))/support.y),0.0);}
    var total=vec4<f32>(0.0);
    for(var sx=left.x;sx<right.x;sx=sx+1) {
        var vertical=vec4<f32>(0.0);
        for(var sy=left.y;sy<right.y;sy=sy+1) {
            let t=texel(sx,sy);
            let bytes=round(vec4<f32>(t.rgb,t.a*255.0));
            let weight=max(1.0-abs((f32(sy)-(centre.y-0.5))/support.y),0.0)/wy;
            vertical+=bytes*weight;
        }
        let weight=max(1.0-abs((f32(sx)-(centre.x-0.5))/support.x),0.0)/wx;
        total+=vertical*weight;
    }
    let pre=floor(clamp(total,vec4<f32>(0.0),vec4<f32>(255.0))+0.5);
    var rgb=pre.rgb;
    if pre.a>0.0 {rgb=min(floor((pre.rgb*255.0+floor(pre.a/2.0))/pre.a),vec3<f32>(255.0));}
    return vec4<f32>(rgb,pre.a/255.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);
    var rgb = vec3<f32>(0.0);
    var a8 = 0.0;
    if params._pad1>0u && params._pad2>0u {
        let t=resized_colour(x-params.dx,y-params.dy);
        rgb=t.rgb;a8=t.a*255.0;
    } else if params.scale == 1.0 {
        // Unscaled: the pixel as it is.
        let t = texel(x - params.dx, y - params.dy);
        rgb = t.xyz;
        a8 = t.w * 255.0;
    } else {
        // The sprite scaled about the anchor: the source position of
        // this pixel's centre, sampled bilinearly, as the CPU twin does.
        let fx = (f32(x) + 0.5 - f32(params.dx) - params.anchor_x) / params.scale + params.anchor_x - 0.5;
        let fy = (f32(y) + 0.5 - f32(params.dy) - params.anchor_y) / params.scale + params.anchor_y - 0.5;
        let x0f = floor(fx);
        let y0f = floor(fy);
        let tx = fx - x0f;
        let ty = fy - y0f;
        let x0 = i32(x0f);
        let y0 = i32(y0f);
        let s = tap(x0, y0) * ((1.0 - tx) * (1.0 - ty))
          + tap(x0 + 1, y0) * (tx * (1.0 - ty))
          + tap(x0, y0 + 1) * ((1.0 - tx) * ty)
          + tap(x0 + 1, y0 + 1) * (tx * ty);
        if s.w > 0.0 {
            rgb = s.xyz / s.w;
        }
        a8 = s.w * 255.0;
    }
    // A posed glyph's byte coverage scaled by its alpha and rounded half
    // away from zero, as the CPU rasteriser scales it (#2203): the byte
    // recovered first, so 4/255 read back as 3.9999998 still scales from 4.
    let posed = params.over >= 2u && params.opacity < 1.0;
    if posed {
        a8 = round(a8);
    }
    // Unrounded (#190), as the CPU twin: the opacity and the fade
    // applied; scaled, no colour under an alpha that rounds to nothing.
    a8 = clamp(a8 * params.opacity * shape_alpha(x, y), 0.0, 255.0);
    if posed {
        let whole = floor(a8);
        a8 = whole + select(0.0, 1.0, a8 - whole >= 0.5);
    }
    if params.scale != 1.0 && a8 < 0.5 {
        rgb = vec3<f32>(0.0);
    }
    if params.over == 2u {
        let old=unpack(output[idx]);
        let alpha=max(old.a,a8/255.0);
        output[idx]=pack(vec4<f32>(to_ws(vec3<f32>(params.under_r,params.under_g,params.under_b)),alpha));
        return;
    }
    if params.over == 3u {
        // Recover byte coverage from half-float sprite storage before the
        // saturating subtraction. Do not multiply by the text fill alpha.
        let old=unpack(output[idx]);
        let alpha=max(round(old.a*255.0)-round(a8),0.0)/255.0;
        output[idx]=pack(vec4<f32>(old.rgb,alpha));
        return;
    }
    if params.over == 1u || params.over == 4u {
        // Over what the layer holds, as the title renderer lays its
        // layers (`over_blend`): straight alpha, in sRGB-encoded units.
        // Undo the working-space matrix before authored-space blending.
        let sa = a8 / 255.0;
        if sa <= 0.0 { return; }
        let d = unpack(output[idx]);
        let da = clamp(d.w, 0.0, 1.0);
        let out_a = sa + da * (1.0 - sa);
        let authored = vec3<f32>(dot(params.a0.xyz, d.xyz), dot(params.a1.xyz, d.xyz), dot(params.a2.xyz, d.xyz));
        let dc = vec3<f32>(linear_to_srgb_c(max(authored.x, 0.0)), linear_to_srgb_c(max(authored.y, 0.0)), linear_to_srgb_c(max(authored.z, 0.0))) * 255.0;
        let c = (rgb * sa + dc * da * (1.0 - sa)) / out_a;
        let lc = vec3<f32>(srgb_to_linear_c(c.x / 255.0), srgb_to_linear_c(c.y / 255.0), srgb_to_linear_c(c.z / 255.0));
        output[idx] = pack(vec4<f32>(to_ws(lc), out_a));
        return;
    }
    let lin = vec3<f32>(srgb_to_linear_c(rgb.x / 255.0), srgb_to_linear_c(rgb.y / 255.0), srgb_to_linear_c(rgb.z / 255.0));
    if params.under_a <= 0.0 {
        output[idx] = pack(vec4<f32>(to_ws(lin), a8 / 255.0));
        return;
    }
    // Over the full-frame colour beneath, in linear light.
    let sa = a8 / 255.0;
    let ua = params.under_a * (1.0 - sa);
    let oa = sa + ua;
    var o = vec3<f32>(0.0);
    if oa > 0.0 {
        o = (lin * sa + vec3<f32>(params.under_r, params.under_g, params.under_b) * ua) / oa;
    }
    output[idx] = pack(vec4<f32>(to_ws(o), oa));
}
"
);

// A resident sprite's glyphs each placed on its own — a motion that
// staggers by character, word or line: `klipp_title`'s
// `pose_sprite_glyphs`, then the sprite decode, in one. Every pixel in
// the rows asks every glyph whose moved box holds it for its bilinear,
// glyph-masked sample, and lays the glyphs over one another in order.
// A glyph an animator turns is sampled back through its turn.
const SHADER_SPRITE_GLYPHS: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, y0: u32, y1: u32, n: u32, ux0: u32, uy0: u32, uw: u32, uh: u32, bx0: u32, by0: u32, bw: u32, bh: u32, _p0: u32, _p1: u32, _p2: u32, m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32> }
@group(0) @binding(0) var<storage, read> sprite: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> units: array<u32>;
@group(0) @binding(4) var<storage, read> glyphs: array<vec4<f32>>; // not-pixels: glyph poses (moved rect, pose, anchor), four vec4 each

// The authored BT.709 picture in the working space (#1279): the CPU's
// `authored_to_working`; identity in a Rec.709 project.
fn to_ws(c: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
}

fn srgb_to_linear_c(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn unit_at(x: i32, y: i32) -> u32 {
    let mx = x - i32(params.ux0);
    let my = y - i32(params.uy0);
    if mx < 0 || my < 0 || mx >= i32(params.uw) || my >= i32(params.uh) { return 0xFFFFu; }
    let i = u32(my) * params.uw + u32(mx);
    let word = units[i >> 1u];
    if (i & 1u) == 0u { return word & 0xFFFFu; }
    return (word >> 16u) & 0xFFFFu;
}

// Glyph `g`'s pixel at (x, y), premultiplied, colour in sRGB-encoded
// 0..255 units; nothing outside the sprite's box or the glyph (#190).
fn tap(x: i32, y: i32, g: u32) -> vec4<f32> {
    let bx = x - i32(params.bx0);
    let by = y - i32(params.by0);
    if bx < 0 || by < 0 || bx >= i32(params.bw) || by >= i32(params.bh) || unit_at(x, y) != g {
        return vec4<f32>(0.0);
    }
    let i = u32(by) * params.bw + u32(bx);
    let rg = unpack2x16float(sprite[i * 2u]);
    let ba = unpack2x16float(sprite[i * 2u + 1u]);
    let a = ba.y;
    return vec4<f32>(rg.x * 255.0 * a, rg.y * 255.0 * a, ba.x * 255.0 * a, a);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = params.y0 * params.width + gid.x;
    if idx >= params.y1 * params.width { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);
    var acc = vec4<f32>(0.0);
    for (var g = 0u; g < params.n; g = g + 1u) {
        let moved = glyphs[g * 4u];
        if f32(x) < moved.x || f32(x) >= moved.z || f32(y) < moved.y || f32(y) >= moved.w { continue; }
        let pose = glyphs[g * 4u + 1u];
        let anch = glyphs[g * 4u + 2u];
        // A turn, clockwise radians about the anchor (an animator's).
        let turn = glyphs[g * 4u + 3u].x;
        var u = f32(x) + 0.5 - pose.x - anch.x;
        var v = f32(y) + 0.5 - pose.y - anch.y;
        if turn != 0.0 {
            let c = cos(turn);
            let sn = sin(turn);
            let ur = u * c + v * sn;
            v = v * c - u * sn;
            u = ur;
        }
        let fx = u / pose.z + anch.x - 0.5;
        let fy = v / pose.z + anch.y - 0.5;
        let x0f = floor(fx);
        let y0f = floor(fy);
        let tx = fx - x0f;
        let ty = fy - y0f;
        let x0 = i32(x0f);
        let y0 = i32(y0f);
        let s = tap(x0, y0, g) * ((1.0 - tx) * (1.0 - ty))
              + tap(x0 + 1, y0, g) * (tx * (1.0 - ty))
              + tap(x0, y0 + 1, g) * ((1.0 - tx) * ty)
              + tap(x0 + 1, y0 + 1, g) * (tx * ty);
        if s.w <= 0.0 { continue; }
        // Unrounded (#190), as the CPU twin.
        let a = clamp(s.w * pose.w, 0.0, 1.0);
        if a * 255.0 < 0.5 { continue; }
        let rgb = s.xyz / s.w;
        acc = vec4<f32>(rgb * a + acc.xyz * (1.0 - a), a + acc.w * (1.0 - a));
    }
    if acc.w <= 0.0 {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }
    let rgb = acc.xyz / acc.w;
    output[idx] = pack(vec4<f32>(to_ws(vec3<f32>(srgb_to_linear_c(rgb.x / 255.0), srgb_to_linear_c(rgb.y / 255.0), srgb_to_linear_c(rgb.z / 255.0))), acc.w));
}
"
);

// A matte from a layer: `track_matte::extract_mask_linear` — the source
// is premultiplied, so colour channels are divided by alpha first;
// channel 0 alpha, 1 luminance, 2 red, 3 green, 4 blue; then inverted.
// Written to every channel of the matte buffer.
const SHADER_MATTE_EXTRACT: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, channel: u32, invert: u32, luma_r: f32, luma_g: f32, luma_b: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> source: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> matte: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(source[idx]);
    var rgb = p.xyz;
    if params.channel != 0u && p.w > 0.0001 {
        rgb = clamp(rgb / p.w, vec3<f32>(0.0), vec3<f32>(1.0));
    }
    var raw: f32;
    switch params.channel {
        case 0u: { raw = p.w; }
        case 1u: { raw = params.luma_r * rgb.x + params.luma_g * rgb.y + params.luma_b * rgb.z; }
        case 2u: { raw = rgb.x; }
        case 3u: { raw = rgb.y; }
        default: { raw = rgb.z; }
    }
    var m = clamp(raw, 0.0, 1.0);
    if params.invert != 0u { m = 1.0 - m; }
    matte[idx] = pack(vec4<f32>(m, m, m, m));
}
"
);

// A choke: the minimum over a (2r+1)-wide window, one axis per pass.
const SHADER_MASK_MIN_H: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, radius: u32, _pad: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let x = i32(idx % params.width);
    let r = i32(params.radius);
    var m = 1.0e30;
    for (var dx = -r; dx <= r; dx++) {
        let sx = x + dx;
        if sx < 0 || sx >= i32(params.width) { continue; }
        m = min(m, unpack(input[y * params.width + u32(sx)]).x);
    }
    output[idx] = pack(vec4<f32>(m, m, m, m));
}
"
);

const SHADER_MASK_MIN_V: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, radius: u32, _pad: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = i32(idx / params.width);
    let x = idx % params.width;
    let r = i32(params.radius);
    var m = 1.0e30;
    for (var dy = -r; dy <= r; dy++) {
        let sy = y + dy;
        if sy < 0 || sy >= i32(params.height) { continue; }
        m = min(m, unpack(input[u32(sy) * params.width + x]).x);
    }
    output[idx] = pack(vec4<f32>(m, m, m, m));
}
"
);

// The matte into a layer's alpha, in place.
const SHADER_MATTE_APPLY: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, factor: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> matte: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> layer: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(layer[idx]);
    let m = clamp(unpack(matte[idx]).x, 0.0, 1.0);
    layer[idx] = pack(vec4<f32>(p.xyz, p.w * m));
}
"
);

// Packed sRGB RGBA8 bytes into a storage texture, a texel per
// invocation. A `copy_buffer_to_texture` wants rows padded to 256
// bytes, which a tightly packed frame of most widths is not; a store
// has no such condition.
const SHADER_U8_TO_TEXTURE: &str = "
struct Params { width: u32, height: u32, _p0: u32, _p1: u32 }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var out_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if gid.x >= params.width || gid.y >= params.height { return; }
    let packed = input[gid.y * params.width + gid.x];
    textureStore(out_tex, vec2<i32>(i32(gid.x), i32(gid.y)), unpack4x8unorm(packed));
}
";

// A layer's alpha times a factor, in place. Binding 0 is unused.
const SHADER_SCALE_ALPHA: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, factor: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> unused: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> layer: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(layer[idx]);
    if idx == 0u && arrayLength(&unused) == 0u { return; }
    layer[idx] = pack(vec4<f32>(p.xyz, p.w * params.factor));
}
"
);

// The composite's last step: colour times alpha, alpha to one.
const SHADER_PREMULTIPLY: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let a = clamp(p.w, 0.0, 1.0);
    output[idx] = pack(vec4<f32>(p.xyz * a, 1.0));
}
"
);

// A layer premultiplied keeping its alpha, or divided back to straight:
// what a transition mixes in, as the CPU mixes it (#1026).
const SHADER_ALPHA_FORM: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, unpremultiply: u32, _pad: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    var rgb = p.xyz * p.w;
    if params.unpremultiply != 0u {
        rgb = p.xyz;
        if p.w > ", unpremultiply_min_alpha_wgsl!(), " { rgb = p.xyz / p.w; }
    }
    output[idx] = pack(vec4<f32>(rgb, p.w));
}
"
);

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

const SHADER_U8_TO_LINEAR: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// Each 8-bit code's linear value, as the CPU decodes it, then each code
// over 255, alpha's value, as the CPU divides it (`srgb8_to_linear`).
@group(0) @binding(3) var<storage, read> srgb8: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let packed = input[idx];
    let a = srgb8[256u + ((packed >> 24u) & 0xFFu)];
    // sRGB transfer on RGB, looked up: alpha stays linear per sRGB spec.
    output[idx] = pack(vec4<f32>(srgb8[packed & 0xFFu], srgb8[(packed >> 8u) & 0xFFu], srgb8[(packed >> 16u) & 0xFFu], a));
}
"
);

// A frame of another size fitted into the chain's, as the CPU
// compositor's `fit` does: outside the picture transparent; inside, an
// area average of the source pixels an output pixel covers when the
// source is at least twice the size both ways, bilinear otherwise, on
// linear values with straight alpha.
const SHADER_U8_FIT_TO_LINEAR: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    src_w: u32, src_h: u32, out_w: u32, out_h: u32,
    dst_w: u32, dst_h: u32, x_off: u32, y_off: u32,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// Each 8-bit code's linear value, as the CPU decodes it, then each code
// over 255, alpha's value, as the CPU divides it (`srgb8_to_linear`).
@group(0) @binding(3) var<storage, read> srgb8: array<f32>;

fn tap(x: u32, y: u32) -> vec4<f32> {
    let packed = input[y * params.src_w + x];
    let a = srgb8[256u + ((packed >> 24u) & 0xFFu)];
    return vec4<f32>(srgb8[packed & 0xFFu], srgb8[(packed >> 8u) & 0xFFu], srgb8[(packed >> 16u) & 0xFFu], a);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.out_w * params.out_h { return; }
    let ox = idx % params.out_w;
    let oy = idx / params.out_w;
    if ox < params.x_off || ox >= params.x_off + params.dst_w || oy < params.y_off || oy >= params.y_off + params.dst_h {
        output[idx] = pack(vec4<f32>(0.0));
        return;
    }
    let x = ox - params.x_off;
    let y = oy - params.y_off;
    let sx = f32(params.src_w) / f32(params.dst_w);
    let sy = f32(params.src_h) / f32(params.dst_h);
    if sx >= 2.0 && sy >= 2.0 {
        let x0 = min(u32(f32(x) * sx), params.src_w - 1u);
        let x1 = clamp(u32(f32(x + 1u) * sx), x0 + 1u, params.src_w);
        let y0 = min(u32(f32(y) * sy), params.src_h - 1u);
        let y1 = clamp(u32(f32(y + 1u) * sy), y0 + 1u, params.src_h);
        var acc = vec4<f32>(0.0);
        for (var yy = y0; yy < y1; yy = yy + 1u) {
            for (var xx = x0; xx < x1; xx = xx + 1u) {
                acc = acc + tap(xx, yy);
            }
        }
        output[idx] = pack(acc / f32((y1 - y0) * (x1 - x0)));
        return;
    }
    let fx = clamp((f32(x) + 0.5) * sx - 0.5, 0.0, f32(params.src_w - 1u));
    let fy = clamp((f32(y) + 0.5) * sy - 0.5, 0.0, f32(params.src_h - 1u));
    let xa = u32(floor(fx));
    let ya = u32(floor(fy));
    let xb = min(xa + 1u, params.src_w - 1u);
    let yb = min(ya + 1u, params.src_h - 1u);
    let tx = fract(fx);
    let ty = fract(fy);
    let top = mix(tap(xa, ya), tap(xb, ya), tx);
    let bot = mix(tap(xa, yb), tap(xb, yb), tx);
    output[idx] = pack(mix(top, bot, ty));
}
"
);

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
/// The BT.2020 -> BT.709 display mapping, as WGSL.
///
/// A BT.2020 project's display transform is a basis change and then the
/// BT.2407-style gamut compression from `klipp_color::gamut` — ~15 ms a
/// 4K frame on the CPU for ordinary colour and ~32 for wholly
/// saturated, which is why the viewer's fast path declined the whole
/// project.
///
/// Same division as ACES: the reach tables and cusps arrive as data,
/// built by bisecting each gamut's boundary 24 times per grid point,
/// and only the arithmetic that reads them lives here. The includer
/// must declare `gamut: array<f32>` as a storage binding.
macro_rules! shader_bt2020_gamut_wgsl {
    () => {
        "
// Must match `klipp_color::gamut::gpu_layout`.
const G_MATRICES: u32 = 2u;
const G_WHITE: u32 = 29u;
const G_TARGET: u32 = 32u;

const KNEE: f32 = 0.8;
const CHROMA_FLOOR: f32 = 0.5;
const MAX_LIGHTNESS_TRADED: f32 = 5.0;

fn g_l_steps() -> u32 { return u32(tables[0]); }
fn g_h_steps() -> u32 { return u32(tables[1]); }
fn g_reach_len() -> u32 { return g_l_steps() * g_h_steps() + g_l_steps(); }
fn g_source() -> u32 { return G_TARGET + g_reach_len(); }
fn g_cusps() -> u32 { return g_source() + g_reach_len(); }

fn g_mat(m: u32, v: vec3<f32>) -> vec3<f32> {
    let o = G_MATRICES + m * 9u;
    return vec3<f32>(
        tables[o + 0u] * v.x + tables[o + 1u] * v.y + tables[o + 2u] * v.z,
        tables[o + 3u] * v.x + tables[o + 4u] * v.y + tables[o + 5u] * v.z,
        tables[o + 6u] * v.x + tables[o + 7u] * v.y + tables[o + 8u] * v.z,
    );
}

fn g_white() -> vec3<f32> {
    return vec3<f32>(tables[G_WHITE], tables[G_WHITE + 1u], tables[G_WHITE + 2u]);
}

fn lab_f(t: f32) -> f32 {
    if t > 0.008856 { return pow(t, 1.0 / 3.0); }
    return 7.787 * t + 16.0 / 116.0;
}

fn lab_g(t: f32) -> f32 {
    let cube = t * t * t;
    if cube > 0.008856 { return cube; }
    return (t - 16.0 / 116.0) / 7.787;
}

fn xyz_to_lab(xyz: vec3<f32>) -> vec3<f32> {
    let w = g_white();
    let fx = lab_f(xyz.x / w.x);
    let fy = lab_f(xyz.y / w.y);
    let fz = lab_f(xyz.z / w.z);
    return vec3<f32>(116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz));
}

fn lab_to_xyz(lab: vec3<f32>) -> vec3<f32> {
    let fy = (lab.x + 16.0) / 116.0;
    let fx = fy + lab.y / 500.0;
    let fz = fy - lab.z / 200.0;
    let w = g_white();
    return vec3<f32>(lab_g(fx) * w.x, lab_g(fy) * w.y, lab_g(fz) * w.z);
}

fn hue_cell(h: f32) -> vec3<f32> {
    let n = f32(g_h_steps());
    let hf = (h - floor(h / 360.0) * 360.0) / 360.0 * n;
    let h0 = floor(hf);
    return vec3<f32>(h0 - floor(h0 / n) * n, (h0 + 1.0) - floor((h0 + 1.0) / n) * n, hf - h0);
}

fn lightness_cell(l: f32) -> vec2<f32> {
    let lf = (clamp(l, 0.0, 100.0) / 100.0) * f32(g_l_steps() - 1u);
    let l0 = min(floor(lf), f32(g_l_steps() - 2u));
    return vec2<f32>(l0, lf - l0);
}

fn reach_at(base: u32, l: f32, h: f32) -> f32 {
    let lc = lightness_cell(l);
    let hc = hue_cell(h);
    let hs = g_h_steps();
    let r0 = base + u32(lc.x) * hs;
    let r1 = base + (u32(lc.x) + 1u) * hs;
    let a = mix(tables[r0 + u32(hc.x)], tables[r0 + u32(hc.y)], hc.z);
    let b = mix(tables[r1 + u32(hc.x)], tables[r1 + u32(hc.y)], hc.z);
    return mix(a, b, lc.y);
}

fn reach_least_at(base: u32, l: f32) -> f32 {
    let mins = base + g_l_steps() * g_h_steps();
    let lc = lightness_cell(l);
    return min(tables[mins + u32(lc.x)], tables[mins + u32(lc.x) + 1u]);
}

fn cusp_at(h: f32) -> vec2<f32> {
    let hc = hue_cell(h);
    let base = g_cusps();
    let a = vec2<f32>(tables[base + u32(hc.x) * 2u], tables[base + u32(hc.x) * 2u + 1u]);
    let b = vec2<f32>(tables[base + u32(hc.y) * 2u], tables[base + u32(hc.y) * 2u + 1u]);
    return mix(a, b, hc.z);
}

fn compress_to_bt709(rgb_in: vec3<f32>) -> vec3<f32> {
    let scale = max(max(max(rgb_in.x, rgb_in.y), rgb_in.z), 1.0);
    var n = rgb_in;
    if scale > 1.0 { n = rgb_in / scale; }
    let lab = xyz_to_lab(g_mat(0u, n));
    let l = lab.x;
    let a = lab.y;
    let b = lab.z;
    let c = sqrt(a * a + b * b);
    if c <= CHROMA_FLOOR { return rgb_in; }
    if c <= reach_least_at(G_TARGET, l) * KNEE { return rgb_in; }
    let h = degrees(atan2(b, a));
    let held = reach_at(G_TARGET, l, h);
    if c <= held * KNEE { return rgb_in; }
    let cusp = cusp_at(h);
    var lightness = l;
    if c > held && l > cusp.x {
        let want = min(c, cusp.y);
        let fl = max(cusp.x, l - MAX_LIGHTNESS_TRADED);
        var lo = fl;
        var hi = l;
        for (var i = 0; i < 8; i = i + 1) {
            let mid = 0.5 * (lo + hi);
            if reach_at(G_TARGET, mid, h) >= want { lo = mid; } else { hi = mid; }
        }
        lightness = lo;
    }
    let t = reach_at(G_TARGET, lightness, h);
    let knee = t * KNEE;
    let sm = reach_at(g_source(), lightness, h);
    if sm <= t * (1.0 + 1e-4) && lightness == l { return rgb_in; }
    var mapped_c: f32;
    if sm > knee {
        mapped_c = min(knee + (c - knee) * ((t - knee) / (sm - knee)), t);
    } else {
        mapped_c = min(c, t);
    }
    let k = max(mapped_c, 0.0) / c;
    let mapped = g_mat(1u, lab_to_xyz(vec3<f32>(lightness, a * k, b * k)));
    return mapped * scale;
}

fn bt2020_to_bt709_display(rgb: vec3<f32>) -> vec3<f32> {
    return max(compress_to_bt709(g_mat(2u, rgb)), vec3<f32>(0.0));
}
"
    };
}

/// A PQ or HLG delivery's encode, as WGSL.
///
/// `ExportDisplay::encode` on `ExportPath::Pq` / `Hlg`: HDR display light
/// in the working basis (1.0 = 203 cd/m², BT.2408) → the delivery's
/// signal. The basis matrix, the working space's luma weights and the PQ
/// table come from `klipp_color::hdr::hdr_encode_gpu_data` in the
/// `tables` binding; everything else here is closed form.
///
/// The PQ curve is read from the shipped table rather than evaluated,
/// because the CPU reads a table too — evaluating ST-2084 properly here
/// would put the shader 3e-5 away from the twin by being *more* right,
/// which is not what a parity test can hold.
macro_rules! shader_hdr_encode_wgsl {
    () => {
        "
// Must match `klipp_color::hdr::gpu_layout`.
const H_TO_2020: u32 = 1u;
const H_LUMA: u32 = 10u;
const H_PQ_TABLE: u32 = 13u;

fn h_to_bt2020(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(
        tables[H_TO_2020 + 0u] * v.x + tables[H_TO_2020 + 1u] * v.y + tables[H_TO_2020 + 2u] * v.z,
        tables[H_TO_2020 + 3u] * v.x + tables[H_TO_2020 + 4u] * v.y + tables[H_TO_2020 + 5u] * v.z,
        tables[H_TO_2020 + 6u] * v.x + tables[H_TO_2020 + 7u] * v.y + tables[H_TO_2020 + 8u] * v.z,
    );
}

/// `display_light_to_nits`. Over the peak the pixel scales down as a
/// whole, so a highlight too bright for the master keeps its hue instead
/// of turning towards white channel by channel.
fn h_display_light_to_nits(rgb2020: vec3<f32>, peak: f32) -> vec3<f32> {
    let n = max(rgb2020 * H_REF_WHITE_NITS, vec3<f32>(0.0));
    let m = max(n.x, max(n.y, n.z));
    if m > peak {
        return n * (peak / m);
    }
    return n;
}

/// `pq_encode_nits`, off the shipped table with the same indexing: the
/// curve sampled at the eighth root of luminance, where it is nearly
/// straight, and interpolated.
fn h_pq_encode_nits(nits: f32) -> f32 {
    // The header holds the entry count; the CPU's `PQ_TABLE_SIZE` is one
    // less, because the table carries both ends.
    let n = u32(tables[0]) - 1u;
    let x = clamp(nits / 10000.0, 0.0, 1.0);
    // Three square roots rather than `pow(x, 0.125)`, to land on the
    // same bits the twin's `.sqrt().sqrt().sqrt()` does.
    let t = sqrt(sqrt(sqrt(x))) * f32(n);
    let i = min(u32(t), n - 1u);
    let f = t - f32(i);
    let a = tables[H_PQ_TABLE + i];
    let b = tables[H_PQ_TABLE + i + 1u];
    return a + (b - a) * f;
}

/// `hlg_oetf`: scene light [0,1] → signal [0,1].
fn h_hlg_oetf(e: f32) -> f32 {
    let a = 0.17883277; let b = 0.28466892; let c = 0.55991073;
    let x = clamp(e, 0.0, 1.0);
    if x <= 1.0 / 12.0 {
        return sqrt(3.0 * x);
    }
    return a * log(12.0 * x - b) + c;
}

/// `hlg_encode_nits`: the inverse HLG OOTF on the nominal display, then
/// the OETF, so SDR white at 203 nits lands on BT.2408's 75% signal.
fn h_hlg_encode_nits(nits2020: vec3<f32>) -> vec3<f32> {
    let f = clamp(nits2020, vec3<f32>(0.0), vec3<f32>(H_HLG_PEAK_NITS)) / H_HLG_PEAK_NITS;
    let yd = 0.2627 * f.x + 0.6780 * f.y + 0.0593 * f.z;
    if yd <= 0.0 {
        return vec3<f32>(0.0);
    }
    // Fd = Ys^(g-1) * E with Yd = Ys^g, so E = Fd * Yd^((1-g)/g).
    let k = pow(yd, (1.0 - H_HLG_GAMMA) / H_HLG_GAMMA);
    return vec3<f32>(h_hlg_oetf(f.x * k), h_hlg_oetf(f.y * k), h_hlg_oetf(f.z * k));
}

/// An HDR delivery's signal, and the pixel's light in cd/m² for MaxCLL.
/// `op` is 1 for PQ, 2 for HLG.
///
/// The light is 0 for HLG because the twin measures none there: MaxCLL
/// and MaxFALL are HDR10 metadata, and an HLG file carries neither.
fn hdr_encode(px: vec3<f32>, peak: f32, op: u32) -> vec4<f32> {
    let rgb2020 = h_to_bt2020(px);
    if op == 2u {
        let nits = h_display_light_to_nits(rgb2020, H_HLG_PEAK_NITS);
        return vec4<f32>(h_hlg_encode_nits(nits), 0.0);
    }
    let nits = h_display_light_to_nits(rgb2020, peak);
    return vec4<f32>(
        h_pq_encode_nits(nits.x),
        h_pq_encode_nits(nits.y),
        h_pq_encode_nits(nits.z),
        max(nits.x, max(nits.y, nits.z)),
    );
}
"
    };
}

/// The ACES Rec.709 SDR output transform, as WGSL for any shader that
/// needs it.
///
/// A macro rather than a second copy: the export's fused Y'CbCr writer
/// needs the same transform as the viewer's display encode, and two
/// hand-written ACES chains is precisely the situation that got the
/// last one deleted.
///
/// The includer must declare these bindings, and a `params` with
/// nothing required of it:
///
/// ```wgsl
/// @group(0) @binding(3) var<storage, read> tables: array<f32>;
/// ```
///
/// `aces` is `klipp_color::aces::aces_rec709_gpu_data` — see there for
/// why the curves and matrices are data and the rest is code.
macro_rules! shader_aces_rec709_wgsl {
    () => {
        "
// Must match `klipp_color::aces::gpu_layout`.
const HEADER_LEN: u32 = 12u;
const A_VARIANT: u32 = 6u;
const A_DISP_GAMMA: u32 = 7u;
const A_DIM_SURROUND: u32 = 8u;
const A_Y_WHITE: u32 = 9u;
const A_Y_BLACK: u32 = 10u;
const MATRICES: u32 = 12u;
// Derived, not written out: the offsets below shift whenever a matrix
// slot is added, and two of them written by hand is two chances to
// forget one. (That mistake reads every curve as a matrix row and turns
// the picture solid.)
const MATRIX_COUNT: u32 = 12u;
const SSTS: u32 = MATRICES + MATRIX_COUNT * 9u;
const SSTS_LEN: u32 = 21u;
const CURVES: u32 = SSTS + SSTS_LEN;
const HALF_MIN: f32 = 6.103515625e-5;
const HALF_MAX: f32 = 65504.0;
const RRT_GLOW_GAIN: f32 = 0.05;
const RRT_GLOW_MID: f32 = 0.08;
const RRT_RED_SCALE: f32 = 0.82;
const RRT_RED_PIVOT: f32 = 0.03;
const RRT_RED_HUE: f32 = 0.0;
const RRT_RED_WIDTH: f32 = 135.0;
const CINEMA_WHITE: f32 = 48.0;
const CINEMA_BLACK: f32 = 0.02;
const SDR_DISP_GAMMA: f32 = 2.4;
const DIM_SURROUND_GAMMA: f32 = 0.9811;
const YC_RADIUS_WEIGHT: f32 = 1.75;
const TINY: f32 = 1e-10;

fn mat_mul(m: u32, v: vec3<f32>) -> vec3<f32> {
    let o = MATRICES + m * 9u;
    return vec3<f32>(
        tables[o + 0u] * v.x + tables[o + 1u] * v.y + tables[o + 2u] * v.z,
        tables[o + 3u] * v.x + tables[o + 4u] * v.y + tables[o + 5u] * v.z,
        tables[o + 6u] * v.x + tables[o + 7u] * v.y + tables[o + 8u] * v.z,
    );
}

/// A tone curve from its table: uniform in log10(x), storing log10(y),
/// clamped at the ends — which is exact, because both curves really are
/// constant outside their knots.
fn tone_curve(which: u32, x: f32) -> f32 {
    let base = select(CURVES, CURVES + u32(tables[2]), which == 1u);
    let lo = tables[select(0u, 3u, which == 1u)];
    let hi = tables[select(1u, 4u, which == 1u)];
    let n = u32(tables[select(2u, 5u, which == 1u)]);

    let logx = log(max(x, HALF_MIN)) / log(10.0);
    let t = clamp((logx - lo) / (hi - lo), 0.0, 1.0) * f32(n - 1u);
    let i = min(u32(floor(t)), n - 2u);
    let f = t - floor(t);
    let logy = mix(tables[base + i], tables[base + i + 1u], f);
    return pow(10.0, logy);
}

fn rgb_2_saturation(rgb: vec3<f32>) -> f32 {
    let mx = max(max(rgb.x, rgb.y), rgb.z);
    let mn = min(min(rgb.x, rgb.y), rgb.z);
    return (max(mx, TINY) - max(mn, TINY)) / max(mx, 1.0e-2);
}

fn rgb_2_yc(rgb: vec3<f32>) -> f32 {
    let r = rgb.x; let g = rgb.y; let b = rgb.z;
    let chroma = sqrt(max(b * (b - g) + g * (g - r) + r * (r - b), 0.0));
    return (b + g + r + YC_RADIUS_WEIGHT * chroma) / 3.0;
}

fn rgb_2_hue(rgb: vec3<f32>) -> f32 {
    if rgb.x == rgb.y && rgb.y == rgb.z { return 0.0; }
    let y = sqrt(3.0) * (rgb.y - rgb.z);
    let x = 2.0 * rgb.x - rgb.y - rgb.z;
    var hue = degrees(atan2(y, x));
    if hue < 0.0 { hue = hue + 360.0; }
    return hue;
}

fn center_hue(hue: f32, center_h: f32) -> f32 {
    var h = hue - center_h;
    if h < -180.0 { h = h + 360.0; } else if h > 180.0 { h = h - 360.0; }
    return h;
}

fn sigmoid_shaper(x: f32) -> f32 {
    let t = max(1.0 - abs(x / 2.0), 0.0);
    return (1.0 + sign(x) * (1.0 - t * t)) / 2.0;
}

fn glow_fwd(yc_in: f32, glow_gain_in: f32, glow_mid: f32) -> f32 {
    if yc_in <= (2.0 / 3.0) * glow_mid { return glow_gain_in; }
    if yc_in >= 2.0 * glow_mid { return 0.0; }
    return glow_gain_in * (glow_mid / yc_in - 0.5);
}

/// AMPAS `cubic_basis_shaper`, width `w`, height ~1.5 at zero.
fn cubic_basis_shaper(x: f32, w: f32) -> f32 {
    let k0 = -w / 2.0;
    let k4 = w / 2.0;
    if !(x > k0 && x < k4) { return 0.0; }
    let knot_coord = (x - k0) / (k4 - k0) * 4.0;
    let j = i32(floor(knot_coord));
    let t = knot_coord - floor(knot_coord);
    let m = vec4<f32>(t * t * t, t * t, t, 1.0);
    // The basis columns, reversed as the CTL indexes them.
    var c: vec4<f32>;
    if j == 0 { c = vec4<f32>(1.0 / 6.0, 0.0, 0.0, 0.0); }
    else if j == 1 { c = vec4<f32>(-3.0 / 6.0, 3.0 / 6.0, 3.0 / 6.0, 1.0 / 6.0); }
    else if j == 2 { c = vec4<f32>(3.0 / 6.0, -6.0 / 6.0, 0.0, 4.0 / 6.0); }
    else if j == 3 { c = vec4<f32>(-1.0 / 6.0, 3.0 / 6.0, -3.0 / 6.0, 1.0 / 6.0); }
    else { return 0.0; }
    return dot(c, m) * 1.5;
}

fn rrt_sweeteners(aces_in: vec3<f32>) -> vec3<f32> {
    let saturation = rgb_2_saturation(aces_in);
    let yc_in = rgb_2_yc(aces_in);
    let s = sigmoid_shaper((saturation - 0.4) / 0.2);
    let added_glow = 1.0 + glow_fwd(yc_in, RRT_GLOW_GAIN * s, RRT_GLOW_MID);
    var a = aces_in * added_glow;

    let centered_hue = center_hue(rgb_2_hue(a), RRT_RED_HUE);
    let hue_weight = cubic_basis_shaper(centered_hue, RRT_RED_WIDTH);
    a.x = a.x + hue_weight * saturation * (RRT_RED_PIVOT - a.x) * (1.0 - RRT_RED_SCALE);

    // AP0 negatives clipped before the matrix, or a negative channel is
    // redistributed into the other two and pure reds go green.
    a = max(a, vec3<f32>(0.0));
    var ap1 = mat_mul(1u, a);
    ap1 = clamp(ap1, vec3<f32>(0.0), vec3<f32>(HALF_MAX));
    return mat_mul(2u, ap1);
}

fn dark_surround_to_dim_surround(rec709_lin: vec3<f32>) -> vec3<f32> {
    let xyz = mat_mul(5u, rec709_lin);
    let sum = xyz.x + xyz.y + xyz.z;
    if sum < 1e-12 { return rec709_lin; }
    let x_chrom = xyz.x / sum;
    let y_chrom = xyz.y / sum;
    let big_y = pow(max(xyz.y, 0.0), DIM_SURROUND_GAMMA);
    var back: vec3<f32>;
    if y_chrom < 1e-12 {
        back = vec3<f32>(0.0, big_y, 0.0);
    } else {
        let inv_y = 1.0 / y_chrom;
        back = vec3<f32>(x_chrom * big_y * inv_y, big_y, (1.0 - x_chrom - y_chrom) * big_y * inv_y);
    }
    return mat_mul(6u, back);
}

/// AcesCG (AP1) → display-encoded Rec.709, the CPU chain's
/// `acescg_to_rec709_sdr`.
/// `camera_log::linear_to_pq` — ST-2084, evaluated exactly.
///
/// Deliberately *not* the table `klipp_color::hdr::pq_encode_nits`
/// interpolates: that one serves the non-ACES PQ delivery, whose CPU
/// twin reads the same table. The ACES ODT's `Y_2_ST2084` calls the real
/// curve, so this does too, and the two PQ paths agree with their own
/// twins rather than with each other.
fn aces_linear_to_pq(l_norm: f32) -> f32 {
    let m1 = 0.15930176;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let yp = pow(max(l_norm, 0.0), m1);
    return pow((c1 + c2 * yp) / (1.0 + c3 * yp), m2);
}

/// `eval_spline_segment`: the quadratic basis transform from AMPAS
/// `Tonecurves.h`, written out. M_QUAD_BASIS is
/// `[[0.5,-1,0.5], [-1,1,0.5], [0.5,0,0]]` and this is its product with
/// three consecutive coefficients, evaluated at `t`.
fn quad_spline_segment(cf: vec3<f32>, t: f32) -> f32 {
    let a = 0.5 * cf.x - cf.y + 0.5 * cf.z;
    let b = -cf.x + cf.y;
    let c = 0.5 * cf.x + 0.5 * cf.y;
    return a * t * t + b * t + c;
}

/// `ssts`, the single-stage HDR tone scale: an AP1 channel value to
/// absolute display luminance in cd/m². Three quadratic segments per
/// side in log-log space, log-linear extrapolation outside.
///
/// Evaluated rather than read from a table, unlike the two SDR curves.
/// Its output feeds `Y_2_linCV`, which subtracts `Y_MIN` from values
/// that *are* `Y_MIN` near black, so a table's own log round-trip is all
/// that survives the cancellation — and PQ's slope there turns it into
/// about two 10-bit code values of lifted black. The numbers that could
/// drift are `init_TsParams`', and those are what the blob ships.
fn ssts(x: f32) -> f32 {
    let min_x = tables[SSTS + 0u];
    let min_y = tables[SSTS + 1u];
    let min_slope = tables[SSTS + 2u];
    let mid_x = tables[SSTS + 3u];
    let max_x = tables[SSTS + 6u];
    let max_y = tables[SSTS + 7u];
    let max_slope = tables[SSTS + 8u];

    // Floored at TINY rather than HALF_MIN, deliberately, as the twin
    // is: these curves put their min anchor below HALF_MIN, so that
    // floor would make the low extrapolation unreachable and lift black.
    let logx = log(max(x, TINY)) / log(10.0);
    let log_min_x = log(min_x) / log(10.0);
    let log_mid_x = log(mid_x) / log(10.0);
    let log_max_x = log(max_x) / log(10.0);

    var logy = 0.0;
    if logx <= log_min_x {
        logy = logx * min_slope + (log(min_y) / log(10.0) - min_slope * log_min_x);
    } else if logx < log_mid_x {
        let knot = 3.0 * (logx - log_min_x) / (log_mid_x - log_min_x);
        // Capped at the last segment: float rounding can push `knot` to
        // exactly 3 at the boundary, where segment 2 at t = 1 is
        // precisely the mid anchor.
        let j = min(u32(floor(knot)), 2u);
        let t = knot - f32(j);
        let cf = vec3<f32>(
            tables[SSTS + 9u + j],
            tables[SSTS + 9u + j + 1u],
            tables[SSTS + 9u + j + 2u],
        );
        logy = quad_spline_segment(cf, t);
    } else if logx < log_max_x {
        let knot = 3.0 * (logx - log_mid_x) / (log_max_x - log_mid_x);
        let j = min(u32(floor(knot)), 2u);
        let t = knot - f32(j);
        let cf = vec3<f32>(
            tables[SSTS + 15u + j],
            tables[SSTS + 15u + j + 1u],
            tables[SSTS + 15u + j + 2u],
        );
        logy = quad_spline_segment(cf, t);
    } else {
        logy = logx * max_slope + (log(max_y) / log(10.0) - max_slope * log_max_x);
    }
    return pow(10.0, logy);
}

/// The ACES 1.1 single-stage output transform for the Rec.2100 PQ
/// deliverables: sweeteners, the SSTS, then absolute luminance to a PQ
/// signal via the limiting primaries.
/// Called with AP1 values: `acescg_to_display` has already walked the
/// project's basis there.
fn acescg_to_rec2100_pq(acescg: vec3<f32>) -> vec3<f32> {
    let y_max = tables[A_Y_WHITE];
    let y_min = tables[A_Y_BLACK];

    // Sweeteners, shared with the SDR path so the two cannot drift.
    let pre = rrt_sweeteners(mat_mul(0u, acescg));
    let post = vec3<f32>(ssts(pre.x), ssts(pre.y), ssts(pre.z));
    let lin_cv = (post - vec3<f32>(y_min)) / (y_max - y_min);

    // AP1 -> XYZ, still D60-adapted, then gamut-limit to Rec.2020 by
    // round-tripping through its primaries with a clip on the way.
    //
    // The clip happens in a D65-primaries space applied to XYZ that is
    // still D60-adapted, because the CAT only runs afterwards. That
    // white-point mismatch is what the reference does; matching it is
    // the point.
    var xyz = mat_mul(7u, lin_cv);
    let limited = clamp(mat_mul(8u, xyz), vec3<f32>(0.0), vec3<f32>(1.0));
    xyz = mat_mul(9u, limited);
    xyz = mat_mul(10u, xyz);
    let rgb = max(mat_mul(8u, xyz), vec3<f32>(0.0));

    // STRETCH_BLACK: the trip back to absolute luminance uses Ymin = 0,
    // so the darkest values quantise to PQ 0 instead of floating above.
    let nits = max(rgb * y_max, vec3<f32>(0.0));
    return vec3<f32>(
        aces_linear_to_pq(nits.x / 10000.0),
        aces_linear_to_pq(nits.y / 10000.0),
        aces_linear_to_pq(nits.z / 10000.0),
    );
}

/// An AcesCG frame through whichever output transform the blob
/// describes: the two-stage SDR chain (RRT then the C9 ODT) or the
/// single-stage HDR one.
///
/// The SDR deliverables — Rec.709 video and DCI-P3 D65 cinema — are one
/// chain with three numbers changed: the output matrix, the display
/// gamma, and whether the dim-surround compensation runs. A cinema is a
/// dark surround, not a dim one, so the cinema ODT applies neither that
/// nor its saturation partner, and the blob carries an identity in the
/// saturation slot for it.
fn acescg_to_display(working: vec3<f32>) -> vec3<f32> {
    // The project's basis into AP1, which the ODTs are defined on. Its
    // own slot rather than folded into ap1->ap0, which the chain applies
    // a second time below to the post-curve values.
    let acescg = mat_mul(11u, working);
    if u32(tables[A_VARIANT]) == 1u {
        return acescg_to_rec2100_pq(acescg);
    }
    // RRT: to AP0, sweeteners, tone curve, back to AP0 as OCES.
    let ap1_desat = rrt_sweeteners(mat_mul(0u, acescg));
    let rgb_post = vec3<f32>(
        tone_curve(0u, ap1_desat.x),
        tone_curve(0u, ap1_desat.y),
        tone_curve(0u, ap1_desat.z),
    );
    let oces = mat_mul(0u, rgb_post);

    // ODT: to AP1, tone curve to display cd/m2, normalise, desaturate,
    // to the display basis, dim surround, clamp, inverse OETF.
    let pre = mat_mul(1u, oces);
    let post = vec3<f32>(
        tone_curve(1u, pre.x),
        tone_curve(1u, pre.y),
        tone_curve(1u, pre.z),
    );
    let y_white = tables[A_Y_WHITE];
    let y_black = tables[A_Y_BLACK];
    let lin_cv = (post - vec3<f32>(y_black)) / (y_white - y_black);
    let desat = mat_mul(3u, lin_cv);
    var display_lin = mat_mul(4u, desat);
    if tables[A_DIM_SURROUND] != 0.0 {
        display_lin = dark_surround_to_dim_surround(display_lin);
    }
    let clamped = clamp(display_lin, vec3<f32>(0.0), vec3<f32>(1.0));
    // BT.1886 inverse at L_W = 1, L_B = 0 collapses to pow(L, 1/gamma),
    // which is also the DCI gamma-2.6 encode.
    return pow(clamped, vec3<f32>(1.0 / tables[A_DISP_GAMMA]));
}

/// The Rec.709 SDR transform by name, for callers that only ever want
/// that one and whose blob always describes it.
fn acescg_to_rec709_sdr(acescg: vec3<f32>) -> vec3<f32> {
    return acescg_to_display(acescg);
}

"
    };
}
// A BT.2020 project's viewer display transform, on the GPU.
//
// `display_transform_ws(.., Bt2020Linear)` is the basis change, the
// BT.2407-style gamut compression, then the sRGB encode — ~15 ms a 4K
// frame for ordinary colour, ~32 for wholly saturated. The viewer's
// fast path declined the whole project for want of this.
//
// No tone-map curve here: the fast path only engages with
// `ToneMapOp::None`, exactly as it does for BT.709, where the operator
// would otherwise sit between the mapping and the encode. With one
// active the frame takes the CPU transform.
// `shader_linear_helpers_wgsl` already carries the pixel storage hook.
const SHADER_BT2020_DISPLAY_U8: &str = concat!(
    shader_linear_helpers_wgsl!(),
    shader_bt2020_gamut_wgsl!(),
    shader_display_dither_wgsl!(),
    "
struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> tables: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = unpack(input[idx]);
    // The CPU's order: to BT.709 and compressed, negatives clamped,
    // (no tone map), sRGB encode, clamp, quantise.
    let mapped = max(bt2020_to_bt709_display(px.xyz), vec3<f32>(0.0));
    let e = vec3<f32>(
        linear_to_srgb_component(mapped.x),
        linear_to_srgb_component(mapped.y),
        linear_to_srgb_component(mapped.z),
    );
    // Dithered as the export rounds (#2072); alpha exact.
    let d = dither_at(idx);
    let a = u32(clamp(px.w, 0.0, 1.0) * 255.0 + 0.5);
    output[idx] = quantise_u8(e.x, d) | (quantise_u8(e.y, d) << 8u) | (quantise_u8(e.z, d) << 16u) | (a << 24u);
}
"
);


const SHADER_ACES_REC709_U8: &str = concat!(
    shader_pixel_storage_wgsl!(),
    shader_display_dither_wgsl!(),
    "
struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> tables: array<f32>;

",
    shader_aces_rec709_wgsl!(),
    "@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = unpack(input[idx]);
    let e = acescg_to_rec709_sdr(px.xyz);
    // Dithered as the export rounds (#2072); alpha exact.
    let d = dither_at(idx);
    let r = quantise_u8(e.x, d);
    let g = quantise_u8(e.y, d);
    let b = quantise_u8(e.z, d);
    let a = u32(clamp(px.w * 255.0 + 0.5, 0.0, 255.0));
    output[idx] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
"
);

// The end of the export: a graded linear frame straight to the encoder's
// Y'CbCr planes, tone map, display encode, colour matrix and chroma
// subsampling all in the one pass.
//
// This is what stops 132 MB crossing the bus. The frame used to come back
// as linear f32 so the CPU could apply the display transform and convert
// to YUV; doing both here means only the planes come down — 12.4 MB for
// 4:2:0 at 4K, a tenth of the traffic — and the per-pixel CPU pass is
// gone with it.
//
// It must agree with `encode_linear_direct` exactly, which is the CPU
// twin and the authority; `gpu_yuv_matches_cpu` holds them to a level.
// That means the same order of operations: negatives clamped, tone map,
// sRGB encode, clamp to [0,1], luma from the *display-encoded* RGB, and
// chroma from the mean of the display-encoded RGB over the block — not
// the mean of the chroma, which is a different number.
//
// One invocation per 8x2 pixel block, because WGSL stores `u32` and not
// bytes: eight pixels wide is two whole words of luma per row, and the
// four chroma samples they reduce to are one whole word each of Cb and
// Cr. Every write is word-aligned with no read-modify-write and no
// atomics. The caller guarantees width % 8 == 0 and height % 2 == 0.
// `shader_linear_helpers_wgsl` already carries the pixel storage hook.
const SHADER_LINEAR_TO_YUV420: &str = concat!(
    shader_linear_helpers_wgsl!(),
    shader_aces_rec709_wgsl!(),
    shader_bt2020_gamut_wgsl!(),
    // The hook `shader_hdr_display_light_wgsl` asks for: this shader has
    // the working space's luma weights in its baked table.
    "
fn h_working_luma() -> vec3<f32> {
    return vec3<f32>(tables[10u], tables[11u], tables[12u]);
}
",
    shader_hdr_display_light_wgsl!(),
    shader_hdr_encode_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    // 0 none, 1 Reinhard, 2 Hable, and 3 means the ACES Rec.709
    // output transform instead of a tone map and an sRGB encode —
    // see `display_encode`. Anything else is refused by the host.
    tone_map_op: u32,
    // Word offset of the chroma. Planar: the Cb plane, with Cr one plane
    // further on. Semi-planar: the single interleaved UV plane.
    chroma_word_offset: u32,
    kr: f32,
    kg: f32,
    kb: f32,
    // 1 for NV12/P010's interleaved UV, 0 for separate Cb and Cr planes.
    semi_planar: u32,
    // 8 or 10. At 10 every sample is a u16 rather than a byte, so half
    // as many fit in a word.
    bits: u32,
    // Left shift on each stored sample: 6 for P010, which carries its
    // ten bits at the top of the u16, 0 everywhere else.
    sample_shift: u32,
    // 1 compresses BT.2020 into BT.709 before the tone map, reading the
    // reach tables at binding 3; 0 leaves the basis alone. Never 1 at
    // the same time as `tone_map_op == 3` — an ACES ODT maps its own
    // gamut, and the two would want the same binding.
    gamut_op: u32,
    // 0 an SDR delivery (the tone map and sRGB encode above), 1 PQ,
    // 2 HLG. An HDR delivery replaces the whole tail: the frame is
    // display light already, and `hdr_encode` takes it to the
    // delivery's signal. Reads binding 3, so likewise exclusive with
    // the other two.
    hdr_op: u32,
    // The master's peak, cd/m². PQ only; HLG's is the nominal 1000.
    peak_nits: f32,
    // A scene-referred source's own last step, before any of the above:
    // 0 none (the frame is already display-referred), 1 the SDR display
    // tone map, 2 HDR display light. `scene_kind` is
    // `HdrTransfer::display_light_kind` and `scene_scale` its
    // `scene_scale`; `scene_exposure` is `display_exposure`, for the
    // tone map.
    scene_op: u32,
    scene_kind: u32,
    scene_scale: f32,
    scene_exposure: f32,
    _pad2: u32,
    _pad3: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
// Whichever tables this delivery's display transform needs: ACES' curves
// and matrices for `tone_map_op == 3`, BT.2020's gamut reach and cusps
// for `gamut_op == 1`. A small dummy when it needs neither.
@group(0) @binding(3) var<storage, read> tables: array<f32>;
// One `(max, sum)` pair of cd/m² per workgroup, for a PQ delivery's
// MaxCLL and MaxFALL. The host reduces the pairs; a small dummy is bound
// when the delivery measures no light.
@group(0) @binding(4) var<storage, read_write> light: array<f32>;

/// Narkowicz ACES fit; constants shared with `klipp_color::hdr::filmic`.
/// The SDR display tone map a scene-referred clip ends with.
fn filmic(x: f32) -> f32 {
    return clamp((x * (2.51 * x + 0.03)) / (x * (2.43 * x + 0.59) + 0.14), 0.0, 1.0);
}

fn hable_curve(x: f32) -> f32 {
    let a = 0.15; let b = 0.50; let c = 0.10;
    let d = 0.20; let e = 0.02; let f = 0.30;
    return ((x * (a * x + c * b) + d * e) / (x * (a * x + b) + d * f)) - e / f;
}

fn tone_map_c(v: f32) -> f32 {
    if params.tone_map_op == 1u { return v / (1.0 + v); }
    if params.tone_map_op == 2u {
        // hable_normalised: the exposure bias and the white point are
        // part of the curve, not the caller's business.
        return hable_curve(v * 2.0) / hable_curve(11.2);
    }
    return v;
}

/// One pixel through the display transform: the CPU twin's
/// `ExportDisplay::encode`, on whichever path the host selected.
///
/// `w` is the pixel's light in cd/m², which only a PQ delivery measures
/// (HDR10's MaxCLL and MaxFALL are made of it); 0 on every other path.
fn display_encode(px_in: vec3<f32>) -> vec4<f32> {
    // A scene-referred clip's own last step, ahead of the delivery's
    // transform: `Renderer::display_render`, which for an HDR delivery is
    // the display light and for an SDR one the filmic tone map.
    var px = px_in;
    if params.scene_op == 2u {
        px = hdr_display_light(px, params.scene_kind, params.scene_scale, params.peak_nits);
    } else if params.scene_op == 1u {
        // Negative first goes to 0, as the CPU's `hdr_display_tonemap`
        // and the viewer's tone map take it: a wide-gamut source's colours
        // outside the working space arrive negative, and this curve maps a
        // small negative to a positive (-0.1 to 0.21), a cast the CPU
        // never shows (#1280).
        let e = max(px * params.scene_exposure, vec3<f32>(0.0));
        px = vec3<f32>(filmic(e.x), filmic(e.y), filmic(e.z));
    }
    // An HDR delivery is a different tail entirely — the frame is display
    // light, not scene light, and there is no tone map or sRGB encode.
    if params.hdr_op != 0u {
        let e = hdr_encode(px, params.peak_nits, params.hdr_op);
        return vec4<f32>(clamp(e.xyz, vec3<f32>(0.0), vec3<f32>(1.0)), e.w);
    }
    // The ACES path is a whole output transform and ends
    // display-encoded already, so it returns before the sRGB encode.
    if params.tone_map_op == 3u {
        return vec4<f32>(acescg_to_rec709_sdr(px), 0.0);
    }
    // A wide-gamut basis comes into BT.709 first, gamut-compressing what
    // BT.709 cannot hold — `ToBt709::apply` in the twin, which likewise
    // runs before the negatives are clamped.
    var v = px;
    if params.gamut_op == 1u {
        v = bt2020_to_bt709_display(v);
    }
    // Negatives clamped before the tone map, as the twin does.
    let lin = max(v, vec3<f32>(0.0));
    let m = vec3<f32>(tone_map_c(lin.x), tone_map_c(lin.y), tone_map_c(lin.z));
    let e = vec3<f32>(
        linear_to_srgb_component(m.x),
        linear_to_srgb_component(m.y),
        linear_to_srgb_component(m.z),
    );
    return vec4<f32>(clamp(e, vec3<f32>(0.0), vec3<f32>(1.0)), 0.0);
}

/// `quant` from the CPU twin: offset and range are given in 8-bit
/// units and scaled to the output depth, rounded half up, clamped.
/// Then shifted into place — P010 puts its ten bits at the top of the
/// u16, which is what `sample_shift` carries.
fn quant(v: f32, off: f32, range: f32) -> u32 {
    let scale = f32(1u << (params.bits - 8u));
    let top = f32((1u << params.bits) - 1u);
    let q = u32(clamp((off + v * range) * scale + 0.5, 0.0, top));
    return q << params.sample_shift;
}

fn luma(e: vec3<f32>) -> f32 {
    return params.kr * e.x + params.kg * e.y + params.kb * e.z;
}

// A workgroup's partial `(max, sum)` of pixel light, one slot per
// invocation, reduced below.
var<workgroup> wg_max: array<f32, 64>;
var<workgroup> wg_sum: array<f32, 64>;

@compute @workgroup_size(8, 8)
fn main(
    @builtin(global_invocation_id) gid: vec3<u32>,
    @builtin(workgroup_id) wid: vec3<u32>,
    @builtin(num_workgroups) nwg: vec3<u32>,
    @builtin(local_invocation_index) lidx: u32,
) {
    let bx = gid.x;          // 8-pixel column block
    let by = gid.y;          // 2-pixel row block
    // Deliberately *not* an early return. The light reduction below
    // ends in `workgroupBarrier`, which every invocation of the
    // workgroup has to reach — a barrier behind a bounds check is
    // undefined in WGSL, and the last workgroup of a row is partly out
    // of range whenever `width / 8` is not a multiple of 8. So the
    // out-of-range invocations run, contribute nothing, and skip only
    // the writes.
    let in_frame = bx * 8u < params.width && by * 2u < params.height;

    let x0 = bx * 8u;
    let y0 = by * 2u;

    // The sixteen pixels of the block, display-encoded once and reused
    // for both luma and chroma.
    var e: array<vec3<f32>, 16>;
    var lmax = 0.0;
    var lsum = 0.0;
    if in_frame {
        for (var r = 0u; r < 2u; r = r + 1u) {
            for (var c = 0u; c < 8u; c = c + 1u) {
                let p = unpack(input[(y0 + r) * params.width + x0 + c]);
                let d = display_encode(p.xyz);
                e[r * 8u + c] = d.xyz;
                lmax = max(lmax, d.w);
                lsum = lsum + d.w;
            }
        }
    }

    // Reduce the block partials to one pair a workgroup. Gated on a
    // uniform value, so the barriers inside stay in uniform control
    // flow, and skipped entirely for a delivery that measures nothing.
    if params.hdr_op == 1u {
        wg_max[lidx] = lmax;
        wg_sum[lidx] = lsum;
        workgroupBarrier();
        for (var stride = 32u; stride > 0u; stride = stride >> 1u) {
            if lidx < stride {
                wg_max[lidx] = max(wg_max[lidx], wg_max[lidx + stride]);
                wg_sum[lidx] = wg_sum[lidx] + wg_sum[lidx + stride];
            }
            workgroupBarrier();
        }
        if lidx == 0u {
            let slot = (wid.y * nwg.x + wid.x) * 2u;
            light[slot] = wg_max[0];
            light[slot + 1u] = wg_sum[0];
        }
    }

    // Past the last barrier, so leaving now is safe.
    if !in_frame { return; }

    // The block's sixteen luma samples and the four chroma sites its
    // 2x2 quads reduce to, quantised. Chroma is the mean of the
    // *display-encoded RGB* over each quad, then the difference — not
    // the mean of the chroma, which is a different number.
    var ys: array<u32, 16>;
    for (var i = 0u; i < 16u; i = i + 1u) {
        ys[i] = quant(luma(e[i]), 16.0, 219.0);
    }
    var cb: array<u32, 4>;
    var cr: array<u32, 4>;
    for (var q = 0u; q < 4u; q = q + 1u) {
        let acc = (e[q * 2u] + e[q * 2u + 1u] + e[8u + q * 2u] + e[8u + q * 2u + 1u]) * 0.25;
        let l = luma(acc);
        cb[q] = quant((acc.z - l) / (2.0 * (1.0 - params.kb)), 128.0, 224.0);
        cr[q] = quant((acc.x - l) / (2.0 * (1.0 - params.kr)), 128.0, 224.0);
    }

    // Packing, which is the only part the depth changes: a word holds
    // four 8-bit samples or two 16-bit ones. Every offset below is in
    // words and lands on a boundary because the caller guarantees the
    // width is a multiple of eight.
    if params.bits == 8u {
        for (var r = 0u; r < 2u; r = r + 1u) {
            let row_word = ((y0 + r) * params.width + x0) / 4u;
            for (var half = 0u; half < 2u; half = half + 1u) {
                var w = 0u;
                for (var k = 0u; k < 4u; k = k + 1u) {
                    w = w | (ys[r * 8u + half * 4u + k] << (k * 8u));
                }
                output[row_word + half] = w;
            }
        }
        if params.semi_planar == 1u {
            // NV12: one plane of Cb,Cr pairs, eight bytes for the four.
            let uv_word = (by * params.width + x0) / 4u;
            output[params.chroma_word_offset + uv_word] =
                cb[0] | (cr[0] << 8u) | (cb[1] << 16u) | (cr[1] << 24u);
            output[params.chroma_word_offset + uv_word + 1u] =
                cb[2] | (cr[2] << 8u) | (cb[3] << 16u) | (cr[3] << 24u);
        } else {
            // Separate planes, half as wide and half as tall: the four
            // samples are exactly one word.
            let c_word = (by * (params.width / 2u) + x0 / 2u) / 4u;
            let plane_words = (params.width / 2u) * (params.height / 2u) / 4u;
            output[params.chroma_word_offset + c_word] =
                cb[0] | (cb[1] << 8u) | (cb[2] << 16u) | (cb[3] << 24u);
            output[params.chroma_word_offset + plane_words + c_word] =
                cr[0] | (cr[1] << 8u) | (cr[2] << 16u) | (cr[3] << 24u);
        }
    } else {
        // Ten bits: u16 samples, so two to a word and twice the stride.
        for (var r = 0u; r < 2u; r = r + 1u) {
            let row_word = ((y0 + r) * params.width + x0) / 2u;
            for (var k = 0u; k < 4u; k = k + 1u) {
                let a = ys[r * 8u + k * 2u];
                let b = ys[r * 8u + k * 2u + 1u];
                output[row_word + k] = a | (b << 16u);
            }
        }
        if params.semi_planar == 1u {
            // P010: Cb,Cr pairs as u16, so a word per pair.
            let uv_word = (by * params.width + x0) / 2u;
            for (var q = 0u; q < 4u; q = q + 1u) {
                output[params.chroma_word_offset + uv_word + q] = cb[q] | (cr[q] << 16u);
            }
        } else {
            let c_word = (by * (params.width / 2u) + x0 / 2u) / 2u;
            let plane_words = (params.width / 2u) * (params.height / 2u) / 2u;
            output[params.chroma_word_offset + c_word] = cb[0] | (cb[1] << 16u);
            output[params.chroma_word_offset + c_word + 1u] = cb[2] | (cb[3] << 16u);
            output[params.chroma_word_offset + plane_words + c_word] = cr[0] | (cr[1] << 16u);
            output[params.chroma_word_offset + plane_words + c_word + 1u] = cr[2] | (cr[3] << 16u);
        }
    }
}
"
);

// The same bridge one sample wider: sRGB-encoded RGBA64 (four u16 per
// pixel, two per u32) → linear f32, with the source primaries folded in.
//
// This is the export's input transform. A software decoder's scaler
// hands back RGBA64 even for an 8-bit source, because YUV→RGB is a
// matrix and its output does not fit in 8 bits — measured on real
// footage, only 27% of samples come back byte-replicated. So the bytes
// cannot be narrowed on the way to the GPU without quantising the
// deliverable; they can only be *moved* there unexpanded, which is what
// this does. Half the traffic of the f32 buffer it replaces, and the
// per-pixel CPU pass that built it disappears.
//
// `source_to_working` is applied in linear light after the curve, so a
// project whose working space is not the source's still takes this path
// — the matrix rows are the identity when there is nothing to change.
const SHADER_RGBA64_TO_LINEAR: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, _pad0: u32, _pad1: u32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// Each 16-bit code's linear value, as the CPU decodes it, then each code
// over 65535, alpha's value (`srgb16_decode_table`): the shader's `pow`
// and division only approximate them.
@group(0) @binding(3) var<storage, read> srgb16: array<f32>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    // RGBA64LE: R,G in the first u32 (low half first), B,A in the second.
    let lo = input[idx * 2u];
    let hi = input[idx * 2u + 1u];
    let a = srgb16[65536u + ((hi >> 16u) & 0xFFFFu)];
    let lin = vec3<f32>(srgb16[lo & 0xFFFFu], srgb16[(lo >> 16u) & 0xFFFFu], srgb16[hi & 0xFFFFu]);
    // Alpha stays linear, per the sRGB spec.
    output[idx] = pack(vec4<f32>(
        dot(params.m0.xyz, lin),
        dot(params.m1.xyz, lin),
        dot(params.m2.xyz, lin),
        a,
    ));
}
"
);

const SHADER_TRANSITION: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    progress: f32,
    kind_id: u32,
    param0: f32,
    param1: f32,
    param2: f32,
    param3: f32,
    color_r: f32,
    color_g: f32,
    color_b: f32,
    color_a: f32,
}

@group(0) @binding(0) var<storage, read> from_buf: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> to_buf: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;

fn srgb_to_linear_component(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    } else {
        return pow((c + 0.055) / 1.055, 2.4);
    }
}

// Storage is native linear-f32 vec4, but the transition body below
// was written against 0-255-range floats (linear or otherwise). To
// keep the large body unchanged, the local `unpack` scales linear
// [0,1] → 0-255, and `pack` divides by 255 back to linear. Alpha
// stays in 0-255 internally the same way (max alpha = 255.0 ⇒ 1.0).
// 0-1 <-> 0-255, this shader's own convention. Named away from
// pack/unpack, which mean how a pixel is *stored*.
fn to_bytes(v: vec4<f32>) -> vec4<f32> { return v * 255.0; }
fn from_bytes(v: vec4<f32>) -> vec4<f32> { return v / 255.0; }

fn lerp_pixel(a: vec4<f32>, b: vec4<f32>, t: f32) -> vec4<f32> {
    return a * (1.0 - t) + b * t;
}

// Noise for the grain, static and glitch kinds: an integer hash of
// integer inputs (pixel or block coordinates, progress's bits), so the
// pattern is the same on every GPU and on the CPU. A `fract(sin(…))`
// hash depended on each GPU's approximation of `sin` at large arguments.
fn hash_u(v: u32) -> u32 {
    var x = v;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    return (x >> 16u) ^ x;
}

fn rand3(a: u32, b: u32, c: u32) -> f32 {
    return f32(hash_u(a ^ hash_u(b ^ hash_u(c)))) / 4294967295.0;
}

// Smooth value noise for the film burn's edge: `rand3` at the corners of
// a unit grid, blended with a smoothstep. Integer-hashed, so the same on
// every GPU and on the CPU.
fn burn_noise(px: f32, py: f32) -> f32 {
    let ix = floor(px);
    let iy = floor(py);
    let tx = px - ix;
    let ty = py - iy;
    let ux = tx * tx * (3.0 - 2.0 * tx);
    let uy = ty * ty * (3.0 - 2.0 * ty);
    let x0 = u32(ix);
    let y0 = u32(iy);
    let a = rand3(x0, y0, 7u);
    let b = rand3(x0 + 1u, y0, 7u);
    let c = rand3(x0, y0 + 1u, 7u);
    let d = rand3(x0 + 1u, y0 + 1u, 7u);
    return mix(mix(a, b, ux), mix(c, d, ux), uy);
}

// Bilinear samples for the kinds that move pixels by fractional amounts
// (spin, wave, zoom): smooth motion, where nearest-pixel sampling
// stair-stepped — and picked a different pixel on each GPU wherever a
// coordinate fell on a boundary. Clamped to the frame; unpacked.
fn bilinear(buf_is_to: bool, sx: f32, sy: f32) -> vec4<f32> {
    let fx = clamp(sx, 0.0, f32(params.width) - 1.0);
    let fy = clamp(sy, 0.0, f32(params.height) - 1.0);
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, params.width - 1u);
    let y1 = min(y0 + 1u, params.height - 1u);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    var c00: vec4<f32>;
    var c10: vec4<f32>;
    var c01: vec4<f32>;
    var c11: vec4<f32>;
    if buf_is_to {
        c00 = unpack(to_buf[y0 * params.width + x0]);
        c10 = unpack(to_buf[y0 * params.width + x1]);
        c01 = unpack(to_buf[y1 * params.width + x0]);
        c11 = unpack(to_buf[y1 * params.width + x1]);
    } else {
        c00 = unpack(from_buf[y0 * params.width + x0]);
        c10 = unpack(from_buf[y0 * params.width + x1]);
        c01 = unpack(from_buf[y1 * params.width + x0]);
        c11 = unpack(from_buf[y1 * params.width + x1]);
    }
    return to_bytes(mix(mix(c00, c10, tx), mix(c01, c11, tx), ty));
}

// Piece transitions (#985): the settings `PieceParams` packs into param3.
// Bits 0-2 the order, 3-4 the easing, 5 assemble, 6 alternate, 7-11 the
// gap in pixels.
fn piece_bits() -> u32 {
    return u32(max(params.param3, 0.0) + 0.5);
}

// A piece's place in the order, 0 moving first and 1 last: `along` is
// its place along the move (0 nearest the way out), `centre` its
// distance from the middle (0 to 1), `id` what shuffles it.
fn piece_key(id: u32, along: f32, centre: f32) -> f32 {
    let order = piece_bits() & 7u;
    if order == 1u {
        return 1.0 - along;
    } else if order == 2u {
        return centre;
    } else if order == 3u {
        return 1.0 - centre;
    } else if order == 4u {
        return rand3(id, 985u, 3u);
    } else if order == 5u {
        return 0.0;
    }
    return along;
}

// How far a piece with key `k` has gone, eased: 0 at rest, 1 gone. Each
// piece's start is put off by the stagger times its key.
fn piece_travel(k: f32) -> f32 {
    let s = clamp(params.param2, 0.0, 0.9);
    let t = clamp((params.progress - s * k) / (1.0 - s), 0.0, 1.0);
    let ease = (piece_bits() >> 3u) & 3u;
    if ease == 1u {
        return t;
    } else if ease == 2u {
        return t * t;
    } else if ease == 3u {
        let u = t - 1.0;
        return 1.0 + 2.70158 * u * u * u + 1.70158 * u * u;
    }
    return t * t * (3.0 - 2.0 * t);
}

// Half the gap open around a piece that has gone `e`: none at rest or
// gone, all of it on the way.
fn piece_gap(e: f32) -> f32 {
    return f32((piece_bits() >> 7u) & 31u) * 0.5 * clamp(8.0 * e * (1.0 - e), 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let y = idx / params.width;
    let x = idx % params.width;
    let p = params.progress;

    let from_px = to_bytes(unpack(from_buf[idx]));
    let to_px = to_bytes(unpack(to_buf[idx]));

    var result: vec4<f32>;

    if params.kind_id == 0u {
        // Cross Dissolve
        result = lerp_pixel(from_px, to_px, p);
    } else if params.kind_id == 1u {
        // Wipe Left (reveals to-frame from left to right)
        // param0 = feather radius in pixels (0 = hard edge)
        // threshold expanded by ±feather so endpoints fully clear/cover the frame
        let feather = max(params.param0, 0.001);
        let threshold = p * (f32(params.width) + 2.0 * feather) - feather;
        let t = 1.0 - smoothstep(threshold - feather, threshold + feather, f32(x));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 2u {
        // Wipe Right (reveals to-frame from right to left)
        let feather = max(params.param0, 0.001);
        let threshold = (1.0 - p) * (f32(params.width) + 2.0 * feather) - feather;
        let t = smoothstep(threshold - feather, threshold + feather, f32(x));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 3u {
        // Wipe Up (reveals to-frame from top to bottom)
        let feather = max(params.param0, 0.001);
        let threshold = p * (f32(params.height) + 2.0 * feather) - feather;
        let t = 1.0 - smoothstep(threshold - feather, threshold + feather, f32(y));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 4u {
        // Wipe Down (reveals to-frame from bottom to top)
        let feather = max(params.param0, 0.001);
        let threshold = (1.0 - p) * (f32(params.height) + 2.0 * feather) - feather;
        let t = smoothstep(threshold - feather, threshold + feather, f32(y));
        result = mix(from_px, to_px, t);
    } else if params.kind_id == 5u || params.kind_id == 6u {
        // Dip to colour. param0 = hold duration (0..0.45) at the midpoint.
        // colour = dip colour (default black for kind 5, white for kind 6).
        // `params.color_*` are already in 0-1 sRGB — decode to linear
        // so the dip crossfade works in physical light. Scale to the
        // 0-255 local convention this shader body uses.
        let dip_color = vec4<f32>(
            srgb_to_linear_component(params.color_r) * 255.0,
            srgb_to_linear_component(params.color_g) * 255.0,
            srgb_to_linear_component(params.color_b) * 255.0,
            255.0,
        );
        let hold = clamp(params.param0, 0.0, 0.45);
        let mid_start = 0.5 - hold * 0.5;
        let mid_end = 0.5 + hold * 0.5;
        if p <= mid_start {
            let t = p / max(mid_start, 0.001);
            result = lerp_pixel(from_px, dip_color, t);
        } else if p >= mid_end {
            let t = (p - mid_end) / max(1.0 - mid_end, 0.001);
            result = lerp_pixel(dip_color, to_px, t);
        } else {
            result = dip_color;
        }
    } else if params.kind_id == 7u {
        // Push: outgoing slides off, incoming slides on. Hard slide, no blend.
        // param0 = direction (0=Left, 1=Right, 2=Up, 3=Down)
        let dir = u32(params.param0 + 0.5);
        if dir == 1u {
            // Push Right: outgoing slides right, incoming enters from left
            let offset = i32(p * f32(params.width));
            let src_x = i32(x) - offset;
            if src_x >= 0 {
                result = to_bytes(unpack(from_buf[y * params.width + u32(src_x)]));
            } else {
                let in_x = u32(src_x + i32(params.width));
                result = to_bytes(unpack(to_buf[y * params.width + in_x]));
            }
        } else if dir == 2u {
            // Push Up: outgoing slides up, incoming enters from bottom
            let offset = u32(p * f32(params.height));
            let src_y = y + offset;
            if src_y < params.height {
                result = to_bytes(unpack(from_buf[src_y * params.width + x]));
            } else {
                let in_y = src_y - params.height;
                result = to_bytes(unpack(to_buf[in_y * params.width + x]));
            }
        } else if dir == 3u {
            // Push Down: outgoing slides down, incoming enters from top
            let offset = i32(p * f32(params.height));
            let src_y = i32(y) - offset;
            if src_y >= 0 {
                result = to_bytes(unpack(from_buf[u32(src_y) * params.width + x]));
            } else {
                let in_y = u32(src_y + i32(params.height));
                result = to_bytes(unpack(to_buf[in_y * params.width + x]));
            }
        } else {
            // Push Left (default): outgoing slides left, incoming enters from right
            let offset = u32(p * f32(params.width));
            let src_x = x + offset;
            if src_x < params.width {
                result = to_bytes(unpack(from_buf[y * params.width + src_x]));
            } else {
                let in_x = src_x - params.width;
                result = to_bytes(unpack(to_buf[y * params.width + in_x]));
            }
        }
    } else if params.kind_id == 8u {
        // Film Dissolve: square-root blending that emulates the
        // density-stacking behaviour of film emulsions. In the
        // linear pipeline `from_px` / `to_px` are already linear,
        // so we drop the /255 and *255 scaling — the sqrt curve
        // still produces the characteristic over-bright midpoint
        // that distinguishes film dissolve from cross dissolve.
        let blended = sqrt(from_px * from_px * (1.0 - p) + to_px * to_px * p);
        result = blended;
        result.w = from_px.w * (1.0 - p) + to_px.w * p;
    } else if params.kind_id == 9u {
        // Blur Dissolve: dissolve + horizontal box blur peaking at midpoint
        // param0 = max blur radius in pixels
        let max_radius = max(params.param0, 1.0);
        let blur_r = i32(p * (1.0 - p) * 4.0 * max_radius);
        var acc = vec4<f32>(0.0);
        var count = 0.0;
        let ix = i32(x);
        let step_sz = max(blur_r / 4, 1);
        for (var dx = -blur_r; dx <= blur_r; dx += step_sz) {
            let sx = clamp(ix + dx, 0, i32(params.width) - 1);
            let si = y * params.width + u32(sx);
            let fpx = to_bytes(unpack(from_buf[si]));
            let tpx = to_bytes(unpack(to_buf[si]));
            acc += lerp_pixel(fpx, tpx, p);
            count += 1.0;
        }
        if count > 0.0 {
            result = acc / count;
        } else {
            result = lerp_pixel(from_px, to_px, p);
        }
    } else if params.kind_id == 10u {
        // Flash: bright colour flash with parameterized peak.
        // param0 = intensity, param1 = peak position, color = flash colour.
        // Decode user-supplied sRGB colour to linear, scaled to the
        // 0-255 local convention this shader body uses.
        let flash_color = vec4<f32>(
            srgb_to_linear_component(params.color_r) * 255.0,
            srgb_to_linear_component(params.color_g) * 255.0,
            srgb_to_linear_component(params.color_b) * 255.0,
            255.0,
        );
        let intensity = clamp(params.param0, 0.0, 2.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        var flash_t: f32;
        if p < ramp_up {
            flash_t = p / max(ramp_up, 0.001);
        } else if p < peak {
            flash_t = 1.0;
        } else if p < ramp_down {
            flash_t = 1.0 - (p - peak) / max(ramp_down - peak, 0.001);
        } else {
            flash_t = 0.0;
        }
        flash_t = clamp(flash_t * intensity, 0.0, 1.0);
        let dissolve_start = peak * 0.8;
        let dissolve_t = clamp((p - dissolve_start) / max(1.0 - dissolve_start, 0.001), 0.0, 1.0);
        let base = lerp_pixel(from_px, to_px, dissolve_t);
        // No 255 clamp — HDR flash may legitimately exceed 1.0 linear;
        // the display boundary clamps at the u8 encode step.
        result = vec4<f32>(
            base.x * (1.0 - flash_t) + flash_color.x * flash_t,
            base.y * (1.0 - flash_t) + flash_color.y * flash_t,
            base.z * (1.0 - flash_t) + flash_color.z * flash_t,
            base.w,
        );
    } else if params.kind_id == 11u {
        // Spin: rotation dissolve around center
        // param0 = rotations (1.0 = one full turn), param1 = zoom bloom amount
        let rotations = max(params.param0, 0.05);
        let zoom_amount = clamp(params.param1, 0.0, 1.0);
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let angle = p * rotations * 6.283185; // 2*PI
        let scale = 1.0 + p * (1.0 - p) * 4.0 * zoom_amount;
        let inv_s = 1.0 / scale;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let rx = (dx * cos(angle) + dy * sin(angle)) * inv_s + cx;
        let ry = (-dx * sin(angle) + dy * cos(angle)) * inv_s + cy;
        let rotated_from = bilinear(false, rx, ry);
        result = lerp_pixel(rotated_from, to_px, p);
    } else if params.kind_id == 12u {
        // Light Leak: additive coloured glow peaking at midpoint.
        // param0 = intensity, color = leak colour (default warm amber).
        let leak_color = vec3<f32>(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let intensity = clamp(params.param0, 0.0, 2.0);
        let leak_t = pow(p * (1.0 - p) * 4.0, 0.7) * intensity;
        let base = lerp_pixel(from_px, to_px, p);
        // Pure additive — leak colour added uniformly so non-amber leaks look right.
        let add = leak_color * leak_t;
        result = vec4<f32>(
            min(base.x + add.x, 255.0),
            min(base.y + add.y, 255.0),
            min(base.z + add.z, 255.0),
            base.w,
        );
    } else if params.kind_id == 13u {
        // Luminance Dissolve: bright pixels of `from` dissolve first, dark pixels last.
        // param0 = softness (default 1.0).
        let from_norm = from_px.rgb / 255.0;
        let luma = dot(from_norm, vec3<f32>(0.2126, 0.7152, 0.0722));
        let softness = max(params.param0 * 0.15, 0.01);
        let t = smoothstep(p - softness, p + softness, luma);
        result = lerp_pixel(from_px, to_px, t);
    } else if params.kind_id == 14u || params.kind_id == 15u {
        // Circle Wipe In (14) expands from center; Out (15) contracts to center.
        // param0 = softness, param1 = cx (0..1), param2 = cy (0..1)
        let cx = params.param1 * f32(params.width);
        let cy = params.param2 * f32(params.height);
        let mx = max(cx, f32(params.width) - cx);
        let my = max(cy, f32(params.height) - cy);
        let max_r = max(length(vec2<f32>(mx, my)), 1.0);
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let dist = length(vec2<f32>(dx, dy)) / max_r;
        let softness = max(params.param0 * 0.04, 0.001);
        var radius: f32;
        if params.kind_id == 14u {
            radius = p * (1.0 + 2.0 * softness) - softness;
        } else {
            radius = (1.0 - p) * (1.0 + 2.0 * softness) - softness;
        }
        let s_val = smoothstep(radius - softness, radius + softness, dist);
        var t: f32;
        if params.kind_id == 14u {
            t = 1.0 - s_val;
        } else {
            t = s_val;
        }
        result = lerp_pixel(from_px, to_px, t);
    } else if params.kind_id == 16u {
        // Clock Wipe: angular sweep around frame center.
        // param0=softness, param1=start angle deg, param2=direction (0=CW, 1=CCW)
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let start_rad = params.param1 * 3.14159265 / 180.0;
        let two_pi = 6.28318530;
        var angle = atan2(dx, -dy) - start_rad;
        angle = ((angle - floor(angle / two_pi) * two_pi) + two_pi);
        angle = angle - floor(angle / two_pi) * two_pi;
        if params.param2 > 0.5 {
            angle = two_pi - angle;
        }
        let norm_angle = angle / two_pi;
        let softness = max(params.param0 * 0.02, 0.001);
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_val = smoothstep(threshold - softness, threshold + softness, norm_angle);
        result = lerp_pixel(from_px, to_px, 1.0 - s_val);
    } else if params.kind_id == 17u || params.kind_id == 18u {
        // Barn Door Open (17) splits from center outward; Close (18) closes inward.
        // param0 = softness, param1 = orientation (0=horizontal, 1=vertical)
        let softness = max(params.param0 * 0.03, 0.001);
        var dist: f32;
        if params.param1 > 0.5 {
            let cy = f32(params.height) * 0.5;
            dist = abs(f32(y) - cy) / max(cy, 1.0);
        } else {
            let cx = f32(params.width) * 0.5;
            dist = abs(f32(x) - cx) / max(cx, 1.0);
        }
        var t_bd: f32;
        if params.kind_id == 17u {
            let threshold = p * (1.0 + 2.0 * softness) - softness;
            t_bd = 1.0 - smoothstep(threshold - softness, threshold + softness, dist);
        } else {
            let threshold = (1.0 - p) * (1.0 + 2.0 * softness) - softness;
            t_bd = smoothstep(threshold - softness, threshold + softness, dist);
        }
        result = lerp_pixel(from_px, to_px, t_bd);
    } else if params.kind_id == 19u {
        // Pixelate: block size peaks at midpoint.
        // param0 = max block size, param1 = mode (0 = pixelate+dissolve, 1 = cross)
        let max_block = max(params.param0, 2.0);
        let triangle = 1.0 - abs(p * 2.0 - 1.0);
        let block_f = max(max_block * triangle, 1.0);
        let block = max(u32(block_f), 1u);
        let bx = min((x / block) * block + block / 2u, params.width - 1u);
        let by = min((y / block) * block + block / 2u, params.height - 1u);
        let block_idx = by * params.width + bx;
        let from_block = to_bytes(unpack(from_buf[block_idx]));
        if params.param1 > 0.5 {
            let to_block = to_bytes(unpack(to_buf[block_idx]));
            result = lerp_pixel(from_block, to_block, p);
        } else if p < 0.5 {
            result = from_block;
        } else {
            let t_px = (p - 0.5) * 2.0;
            result = lerp_pixel(from_block, to_px, t_px);
        }
    } else if params.kind_id == 20u {
        // Ripple: radial sine displacement, envelope peaks at midpoint.
        // param0=amplitude, param1=frequency, param2=speed
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let dist = length(vec2<f32>(dx, dy));
        let amplitude = params.param0;
        let frequency = params.param1;
        let speed = params.param2;
        let envelope = sin(p * 3.14159265) * amplitude;
        let phase = dist * frequency * 0.1 - p * speed * 10.0;
        let ripple = sin(phase) * envelope;
        var nx: f32 = 0.0;
        var ny: f32 = 0.0;
        if dist > 0.1 {
            nx = dx / dist;
            ny = dy / dist;
        }
        let sx = clamp(f32(x) + nx * ripple, 0.0, f32(params.width) - 1.0);
        let sy = clamp(f32(y) + ny * ripple, 0.0, f32(params.height) - 1.0);
        let sample_idx = u32(sy) * params.width + u32(sx);
        let from_distorted = to_bytes(unpack(from_buf[sample_idx]));
        result = lerp_pixel(from_distorted, to_px, p);
    } else if params.kind_id == 21u || params.kind_id == 22u {
        // Page Turn (21) / Page Turn Reverse (22): peel from one side with fold shadow.
        // param0 = shadow intensity, param1 = direction (0=RTL, 1=LTR) for PageTurn.
        let shadow_intensity = params.param0;
        var fx: f32 = f32(x);
        if params.kind_id == 22u || params.param1 > 0.5 {
            fx = f32(params.width) - 1.0 - fx;
        }
        let norm_x = fx / f32(params.width);
        let fold_x = 1.0 - p;
        let denom = max(p * 0.3, 0.01);
        if norm_x > fold_x {
            let shadow_dist = (norm_x - fold_x) / denom;
            let shadow = shadow_intensity * exp(-shadow_dist * 3.0);
            let t = clamp(1.0 - shadow, 0.0, 1.0);
            result = vec4<f32>(to_px.x * t, to_px.y * t, to_px.z * t, 255.0);
        } else {
            let shadow_dist = (fold_x - norm_x) / denom;
            let shadow = shadow_intensity * 0.5 * exp(-shadow_dist * 2.0);
            let t = clamp(1.0 - shadow, 0.0, 1.0);
            result = vec4<f32>(from_px.x * t, from_px.y * t, from_px.z * t, 255.0);
        }
    } else if params.kind_id == 23u {
        // Chromatic Aberration: R/B channels sampled with offset, envelope peaks at midpoint.
        // param0 = max offset px, param1 = direction (0=H, 1=V, 2=diag)
        let max_offset = params.param0;
        let dir = u32(params.param1 + 0.5);
        let envelope = sin(p * 3.14159265) * max_offset;
        var r_ox: f32 = 0.0;
        var r_oy: f32 = 0.0;
        var b_ox: f32 = 0.0;
        var b_oy: f32 = 0.0;
        if dir == 1u {
            r_oy = -envelope;
            b_oy = envelope;
        } else if dir == 2u {
            r_ox = -envelope * 0.707;
            r_oy = -envelope * 0.707;
            b_ox = envelope * 0.707;
            b_oy = envelope * 0.707;
        } else {
            r_ox = -envelope;
            b_ox = envelope;
        }
        let base_g = lerp_pixel(from_px, to_px, p).y;
        let rx = clamp(f32(x) + r_ox, 0.0, f32(params.width) - 1.0);
        let ry = clamp(f32(y) + r_oy, 0.0, f32(params.height) - 1.0);
        let r_idx = u32(ry) * params.width + u32(rx);
        let r_val = lerp_pixel(to_bytes(unpack(from_buf[r_idx])), to_bytes(unpack(to_buf[r_idx])), p).x;
        let bx = clamp(f32(x) + b_ox, 0.0, f32(params.width) - 1.0);
        let by = clamp(f32(y) + b_oy, 0.0, f32(params.height) - 1.0);
        let b_idx = u32(by) * params.width + u32(bx);
        let b_val = lerp_pixel(to_bytes(unpack(from_buf[b_idx])), to_bytes(unpack(to_buf[b_idx])), p).z;
        result = vec4<f32>(r_val, base_g, b_val, 255.0);
    } else if params.kind_id == 24u {
        // Film Burn: a hot spot that blooms in from the right edge and
        // sweeps across, warm at its rim and near white at its core, laid
        // over the pictures so both show through, the cut hidden under it.
        // Its edge is ragged with low-frequency noise. Each pixel drew its
        // own random threshold before, which made a flat grain field with
        // no shape at all (#511).
        // param0 = temperature (0 white, 1 amber), param1 = sweep speed,
        // param2 = grain
        let temp = clamp(params.param0, 0.0, 1.0);
        let speed = max(params.param1, 0.1);
        let grain = params.param2;
        let pc = clamp(p, 0.0, 1.0);
        let env = max(sin(pc * 3.14159265), 0.0);
        let w = f32(params.width);
        let h = f32(params.height);
        let short_side = min(w, h);
        let travel = clamp(pc * speed, 0.0, 1.0);
        let cx = w * (1.1 - 0.7 * travel);
        let cy = h * (0.3 + 0.1 * travel);
        let radius = short_side * (0.3 + 0.85 * env);
        let fx = f32(x);
        let fy = f32(y);
        let n = burn_noise(fx / (short_side * 0.3), fy / (short_side * 0.3));
        let d = length(vec2<f32>(fx - cx, fy - cy)) / radius + (n - 0.5) * 0.3;
        let heat = pow(clamp(1.0 - d, 0.0, 1.0), 1.4) * env;
        let warm = mix(vec3<f32>(1.0, 0.9, 0.75), vec3<f32>(1.0, 0.3, 0.05), temp);
        let col = mix(warm, vec3<f32>(1.0, 0.95, 0.85), smoothstep(0.7, 1.0, heat));
        let cut_t = smoothstep(0.35, 0.65, pc);
        // The picture under the burn is warmed as well as lit.
        let base = mix(from_px.xyz, to_px.xyz, cut_t) / 255.0 * mix(vec3<f32>(1.0, 1.0, 1.0), warm, min(heat * 1.5, 1.0));
        let g = (rand3(x, y, u32(pc * 1000.0)) - 0.5) * grain * 0.08 * heat;
        let burned = clamp(base + col * (heat * 1.2) + vec3<f32>(g, g, g), vec3<f32>(0.0, 0.0, 0.0), vec3<f32>(1.0, 1.0, 1.0));
        result = vec4<f32>(burned.x * 255.0, burned.y * 255.0, burned.z * 255.0, 255.0);
    } else if params.kind_id == 25u {
        // Star Wipe: N-pointed star expanding from center.
        // param0=softness, param1=point count, param2=inner radius ratio
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let points = max(params.param1, 3.0);
        let inner_ratio = clamp(params.param2, 0.2, 0.8);
        let softness = max(params.param0 * 0.03, 0.001);
        let dist = length(vec2<f32>(dx, dy));
        let angle = atan2(dy, dx);
        let sector = 3.14159265 / points;
        let two_sector = 2.0 * sector;
        // WGSL has no float %, use floor-based modulo. Apply twice to normalize negatives.
        var sector_angle = angle - floor(angle / two_sector) * two_sector;
        sector_angle = sector_angle - floor(sector_angle / two_sector) * two_sector;
        let star_r_norm = mix(
            inner_ratio,
            1.0,
            smoothstep(0.0, sector, abs(sector_angle - sector)),
        );
        let max_r = max(length(vec2<f32>(cx, cy)), 1.0);
        let norm_dist = dist / (max_r * max(star_r_norm, 0.01));
        let max_norm = 1.0 / inner_ratio;
        let threshold = p * (max_norm + 2.0 * softness) - softness;
        let s_val = smoothstep(threshold - softness, threshold + softness, norm_dist);
        result = lerp_pixel(from_px, to_px, 1.0 - s_val);
    } else if params.kind_id == 26u || params.kind_id == 27u {
        // Split Wipe H (26) / V (27): reveal `to` expanding from the center line outward.
        let softness = max(params.param0 * 0.03, 0.001);
        var dist: f32;
        if params.kind_id == 26u {
            let cx = f32(params.width) * 0.5;
            dist = abs(f32(x) - cx) / max(cx, 1.0);
        } else {
            let cy = f32(params.height) * 0.5;
            dist = abs(f32(y) - cy) / max(cy, 1.0);
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_sp = smoothstep(threshold - softness, threshold + softness, dist);
        result = lerp_pixel(from_px, to_px, 1.0 - s_sp);
    } else if params.kind_id == 28u || params.kind_id == 29u {
        // Venetian Blind H (28) / V (29): N parallel strips wipe simultaneously.
        let softness = max(params.param0 * 0.04, 0.001);
        let blinds = max(params.param1, 1.0);
        var strip_pos: f32;
        if params.kind_id == 28u {
            strip_pos = fract(f32(y) / f32(params.height) * blinds);
        } else {
            strip_pos = fract(f32(x) / f32(params.width) * blinds);
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_vb = smoothstep(threshold - softness, threshold + softness, strip_pos);
        result = lerp_pixel(from_px, to_px, 1.0 - s_vb);
    } else if params.kind_id == 30u {
        // Checkerboard Wipe: alternating squares wipe LTR / RTL.
        // param0 = softness, param1 = grid size (squares per axis)
        let softness = max(params.param0 * 0.04, 0.001);
        let grid = max(params.param1, 1.0);
        let fx = f32(x) / f32(params.width) * grid;
        let fy = f32(y) / f32(params.height) * grid;
        let col = u32(fx);
        let row = u32(fy);
        let local_x = fract(fx);
        var local_pos: f32;
        if (col + row) % 2u == 0u {
            local_pos = local_x;
        } else {
            local_pos = 1.0 - local_x;
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_cb = smoothstep(threshold - softness, threshold + softness, local_pos);
        result = lerp_pixel(from_px, to_px, 1.0 - s_cb);
    } else if params.kind_id == 31u || params.kind_id == 32u {
        // Diagonal wipe TL-BR (31) / TR-BL (32).
        let softness = max(params.param0 * 0.03, 0.001);
        let nx = f32(x) / f32(params.width);
        let ny = f32(y) / f32(params.height);
        var diag: f32;
        if params.kind_id == 31u {
            diag = (nx + ny) * 0.5;
        } else {
            diag = ((1.0 - nx) + ny) * 0.5;
        }
        let threshold = p * (1.0 + 2.0 * softness) - softness;
        let s_diag = smoothstep(threshold - softness, threshold + softness, diag);
        result = lerp_pixel(from_px, to_px, 1.0 - s_diag);
    } else if params.kind_id == 33u || params.kind_id == 34u {
        // Box Wipe In (33) / Out (34): rectangle expanding from / contracting to center.
        // param0 = softness, param1 = square blend (0 = frame aspect, 1 = square)
        let softness = max(params.param0 * 0.03, 0.001);
        let square = clamp(params.param1, 0.0, 1.0);
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = abs(f32(x) - cx) / max(cx, 1.0);
        let dy = abs(f32(y) - cy) / max(cy, 1.0);
        let box_dist = max(dx, dy);
        let sq_dist = max(dx * cx / max(cy, 1.0), dy);
        let dist = box_dist + (sq_dist - box_dist) * square;
        let aspect = max(cx / max(cy, 1.0), 1.0);
        let max_norm = 1.0 + (aspect - 1.0) * square;
        var threshold: f32;
        if params.kind_id == 33u {
            threshold = p * (max_norm + 2.0 * softness) - softness;
        } else {
            threshold = (1.0 - p) * (max_norm + 2.0 * softness) - softness;
        }
        let s_box = smoothstep(threshold - softness, threshold + softness, dist);
        var t_box: f32;
        if params.kind_id == 33u {
            t_box = 1.0 - s_box;
        } else {
            t_box = s_box;
        }
        result = lerp_pixel(from_px, to_px, t_box);
    } else if params.kind_id >= 35u && params.kind_id <= 44u {
        // Slide / Cover / Uncover / Squeeze family — pixel-offset sampling.
        let wf = f32(params.width);
        let hf = f32(params.height);
        let wi = i32(params.width);
        let hi = i32(params.height);
        let offset_w = i32(p * wf);
        let offset_h = i32(p * hf);
        var sx: i32 = i32(x);
        var sy: i32 = i32(y);
        var use_to: bool = false;
        if params.kind_id == 35u {
            let to_x = i32(x) + wi - offset_w;
            if to_x < wi {
                use_to = true;
                sx = max(to_x, 0);
            } else {
                sx = clamp(i32(x) - offset_w, 0, wi - 1);
            }
        } else if params.kind_id == 36u {
            let to_x = i32(x) - wi + offset_w;
            if to_x >= 0 {
                use_to = true;
                sx = min(to_x, wi - 1);
            } else {
                sx = min(i32(x) + offset_w, wi - 1);
            }
        } else if params.kind_id == 37u {
            let to_y = i32(y) + hi - offset_h;
            if to_y < hi {
                use_to = true;
                sy = max(to_y, 0);
            } else {
                sy = clamp(i32(y) - offset_h, 0, hi - 1);
            }
        } else if params.kind_id == 38u {
            let to_y = i32(y) - hi + offset_h;
            if to_y >= 0 {
                use_to = true;
                sy = min(to_y, hi - 1);
            } else {
                sy = min(i32(y) + offset_h, hi - 1);
            }
        } else if params.kind_id == 39u {
            let to_x = i32(x) + wi - offset_w;
            if to_x < wi {
                use_to = true;
                sx = max(to_x, 0);
            }
        } else if params.kind_id == 40u {
            let to_x = i32(x) - wi + offset_w;
            if to_x >= 0 {
                use_to = true;
                sx = min(to_x, wi - 1);
            }
        } else if params.kind_id == 41u {
            let from_x = i32(x) + offset_w;
            if from_x < wi {
                sx = from_x;
            } else {
                use_to = true;
            }
        } else if params.kind_id == 42u {
            let from_x = i32(x) - offset_w;
            if from_x >= 0 {
                sx = from_x;
            } else {
                use_to = true;
            }
        } else if params.kind_id == 43u {
            // Squeeze H: proper collapse/expand with black outside.
            let half = wf * 0.5;
            let one_px = 1.0 / wf;
            var out_color: vec4<f32>;
            if p <= 0.5 {
                let scale = 1.0 - 2.0 * p;
                if scale < one_px {
                    out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_left = half - half * scale;
                    let vis_right = half + half * scale;
                    if f32(x) < vis_left || f32(x) >= vis_right {
                        out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_x = clamp((f32(x) - half) / scale + half, 0.0, wf - 1.0);
                        out_color = to_bytes(unpack(from_buf[u32(y) * params.width + u32(src_x)]));
                    }
                }
            } else {
                let scale = 2.0 * p - 1.0;
                if scale < one_px {
                    out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_left = half - half * scale;
                    let vis_right = half + half * scale;
                    if f32(x) < vis_left || f32(x) >= vis_right {
                        out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_x = clamp((f32(x) - half) / scale + half, 0.0, wf - 1.0);
                        out_color = to_bytes(unpack(to_buf[u32(y) * params.width + u32(src_x)]));
                    }
                }
            }
            result = out_color;
        } else if params.kind_id == 44u {
            // Squeeze V: proper collapse/expand with black outside.
            let half = hf * 0.5;
            let one_px = 1.0 / hf;
            var out_color: vec4<f32>;
            if p <= 0.5 {
                let scale = 1.0 - 2.0 * p;
                if scale < one_px {
                    out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_top = half - half * scale;
                    let vis_bottom = half + half * scale;
                    if f32(y) < vis_top || f32(y) >= vis_bottom {
                        out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_y = clamp((f32(y) - half) / scale + half, 0.0, hf - 1.0);
                        out_color = to_bytes(unpack(from_buf[u32(src_y) * params.width + u32(x)]));
                    }
                }
            } else {
                let scale = 2.0 * p - 1.0;
                if scale < one_px {
                    out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                } else {
                    let vis_top = half - half * scale;
                    let vis_bottom = half + half * scale;
                    if f32(y) < vis_top || f32(y) >= vis_bottom {
                        out_color = vec4<f32>(0.0, 0.0, 0.0, 255.0);
                    } else {
                        let src_y = clamp((f32(y) - half) / scale + half, 0.0, hf - 1.0);
                        out_color = to_bytes(unpack(to_buf[u32(src_y) * params.width + u32(x)]));
                    }
                }
            }
            result = out_color;
        }
        // Shared lookup for slide/cover/uncover (35–42).
        // Squeeze (43–44) already set result directly above.
        if params.kind_id <= 42u {
            let sidx = u32(sy) * params.width + u32(sx);
            if use_to {
                result = to_bytes(unpack(to_buf[sidx]));
            } else {
                result = to_bytes(unpack(from_buf[sidx]));
            }
        }
    } else if params.kind_id == 66u {
        // Water Drop: a ring spreads from the centre; the incoming
        // picture is revealed inside it, refracted at the crest, with
        // ripples trailing behind the front.
        // param0 = ring width (fraction of the half diagonal)
        // param1 = refraction at the crest, px
        // param2 = trailing ripples
        // param3 = crest highlight
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let dist = length(vec2<f32>(dx, dy));
        let half_diag = length(vec2<f32>(cx, cy));
        let band = max(params.param0, 0.02) * half_diag;
        let front = -band * 0.5 + p * (half_diag * 1.05 + band);
        let s = dist - front;
        let env = sin(p * 3.14159265);
        let crest = exp(-(s * s) / (band * band * 0.18));
        var trail: f32 = 0.0;
        if s < 0.0 {
            trail = sin(s / band * 6.2831853 * max(params.param2, 0.0)) * exp(s / band);
        }
        let disp = (crest + trail * 0.5) * params.param1 * env;
        var nx: f32 = 0.0;
        var ny: f32 = 0.0;
        if dist > 0.1 {
            nx = dx / dist;
            ny = dy / dist;
        }
        let sx = clamp(f32(x) + nx * disp, 0.0, f32(params.width) - 1.0);
        let sy = clamp(f32(y) + ny * disp, 0.0, f32(params.height) - 1.0);
        let from_s = bilinear(false, sx, sy);
        let to_s = bilinear(true, sx, sy);
        let reveal = 1.0 - smoothstep(-band * 0.5, band * 0.5, s);
        let col = lerp_pixel(from_s, to_s, reveal);
        let glint = crest * params.param3 * env * 255.0;
        result = vec4<f32>(min(col.x + glint, 255.0), min(col.y + glint, 255.0), min(col.z + glint, 255.0), col.w);
    } else if params.kind_id == 67u {
        // Whip Pan: the outgoing picture is flung off and the incoming one
        // follows, fastest at the cut, smeared along the move by a blur
        // that follows the speed (#512). The two pictures lie side by side
        // on a strip, and the frame slides along it.
        // param0 = direction (0 left, 1 right, 2 up, 3 down), param1 = blur
        let dir = u32(params.param0 + 0.5);
        let horizontal = dir < 2u;
        var len_axis: f32 = f32(params.height);
        var a: f32 = f32(y);
        if horizontal {
            len_axis = f32(params.width);
            a = f32(x);
        }
        let s = p * p * (3.0 - 2.0 * p);
        let speed = 6.0 * p * (1.0 - p);
        let blur = max(params.param1, 0.0) * speed * len_axis * 0.12;
        var sgn: f32 = 1.0;
        if dir == 1u || dir == 3u {
            sgn = -1.0;
        }
        // 24 taps, each pixel's set shifted by its own hash, so the
        // spacing of the taps shows as fine noise and not as ghost copies.
        let jitter = rand3(x, y, 11u);
        var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        for (var k: u32 = 0u; k < 24u; k = k + 1u) {
            let off = ((f32(k) + jitter) / 24.0 - 0.5) * blur;
            let u = a + sgn * s * len_axis + off;
            var to_side = false;
            var v: f32 = u;
            if sgn > 0.0 {
                if u >= len_axis {
                    to_side = true;
                    v = u - len_axis;
                }
            } else {
                if u < 0.0 {
                    to_side = true;
                    v = u + len_axis;
                }
            }
            v = clamp(v, 0.0, len_axis - 1.0);
            var sx: f32 = f32(x);
            var sy: f32 = f32(y);
            if horizontal {
                sx = v;
            } else {
                sy = v;
            }
            acc = acc + bilinear(to_side, sx, sy);
        }
        result = acc / 24.0;
    } else if params.kind_id == 68u {
        // Slide Strips (#985): strips across the move slide away along
        // their length, each in its turn, uncovering the incoming picture
        // (or the incoming strips slide in over the outgoing). Each output
        // pixel finds its strip and where that strip's picture came from.
        // param0 = strips, param1 = direction in degrees, param2 = stagger.
        let bits = piece_bits();
        let w = f32(params.width);
        let h = f32(params.height);
        let n = clamp(round(params.param0), 1.0, 64.0);
        let ang = params.param1 * 0.017453292;
        let mx = cos(ang);
        let my = sin(ang);
        let qx = f32(x) + 0.5 - w * 0.5;
        let qy = f32(y) + 0.5 - h * 0.5;
        let u = qx * mx + qy * my;
        let v = qy * mx - qx * my;
        // Which strip, and the gap, are judged a hair off the pixel's
        // centre: a centre exactly on an edge would fall either way with
        // the last bit of rounding, and differently on each GPU.
        let uc = (qx + 0.001953125) * mx + (qy + 0.0029296875) * my;
        let vc = (qy + 0.0029296875) * mx - (qx + 0.001953125) * my;
        let ln = abs(w * my) + abs(h * mx);
        let lm = abs(w * mx) + abs(h * my);
        let bw = ln / n;
        let fi = clamp(floor((vc + ln * 0.5) / bw), 0.0, n - 1.0);
        var sgn: f32 = 1.0;
        if (bits & 64u) != 0u && (u32(fi) & 1u) == 1u {
            sgn = -1.0;
        }
        let mid = (n - 1.0) * 0.5;
        let e = piece_travel(piece_key(u32(fi), fi / max(n - 1.0, 1.0), abs(fi - mid) / max(mid, 0.5)));
        let assemble = (bits & 32u) != 0u;
        var t: f32 = e;
        if assemble {
            t = 1.0 - e;
        }
        let su = u - sgn * t * lm;
        let gh = piece_gap(e);
        let fv = vc + ln * 0.5 - fi * bw;
        let sx = su * mx - v * my + w * 0.5;
        let sy = su * my + v * mx + h * 0.5;
        let suc = uc - sgn * t * lm;
        let sxc = suc * mx - vc * my + w * 0.5;
        let syc = suc * my + vc * mx + h * 0.5;
        // The gap is only tested once open: at rest, a pixel's own strip
        // always holds it, whatever the rounding at a strip's edge.
        let in_strip = gh <= 0.0 || (fv >= gh && fv <= bw - gh);
        if in_strip && sxc >= 0.0 && sxc < w && syc >= 0.0 && syc < h {
            result = bilinear(assemble, sx - 0.5, sy - 0.5);
        } else if assemble {
            result = from_px;
        } else {
            result = to_px;
        }
    } else if params.kind_id == 69u {
        // Slide Panels (#985): a grid of square panels slides away along
        // the nearest of right, down, left and up. A pixel's row (across
        // the move) is fixed, so it looks along the row for the panel
        // over it now, the one that has moved furthest on top.
        // param0 = rows, param1 = direction in degrees, param2 = stagger.
        let bits = piece_bits();
        let w = f32(params.width);
        let h = f32(params.height);
        let rows = clamp(round(params.param0), 1.0, 16.0);
        let cols = clamp(round(rows * w / h), 1.0, 32.0);
        let q4 = round(params.param1 / 90.0);
        let quarter = u32(q4 - 4.0 * floor(q4 / 4.0));
        let vertical = quarter == 1u || quarter == 3u;
        var dir: f32 = 1.0;
        if quarter >= 2u {
            dir = -1.0;
        }
        var a: f32 = f32(x) + 0.5;
        var b: f32 = f32(y) + 0.5;
        var la: f32 = w;
        var lb: f32 = h;
        var na: f32 = cols;
        var nb: f32 = rows;
        if vertical {
            a = f32(y) + 0.5;
            b = f32(x) + 0.5;
            la = h;
            lb = w;
            na = rows;
            nb = cols;
        }
        let pa = la / na;
        let pb = lb / nb;
        // Judged a hair off the pixel's centre, as the strips are.
        let ac = a + 0.001953125;
        let bc = b + 0.0029296875;
        let fr = clamp(floor(bc / pb), 0.0, nb - 1.0);
        let fb = bc - fr * pb;
        var sgn: f32 = dir;
        if (bits & 64u) != 0u && (u32(fr) & 1u) == 1u {
            sgn = -dir;
        }
        let assemble = (bits & 32u) != 0u;
        let across = fr / max(nb - 1.0, 1.0);
        var best: f32 = -1.0;
        var best_a: f32 = 0.0;
        for (var j: u32 = 0u; j < 32u; j = j + 1u) {
            let fj = f32(j);
            if fj >= na {
                break;
            }
            var along = fj / max(na - 1.0, 1.0);
            if sgn > 0.0 {
                along = 1.0 - along;
            }
            let ca = (fj + 0.5) / na - 0.5;
            let cb = (fr + 0.5) / nb - 0.5;
            let centre = clamp(sqrt(ca * ca + cb * cb) / 0.7072, 0.0, 1.0);
            let e = piece_travel(piece_key(u32(fr) * 32u + j, (along + across) * 0.5, centre));
            var t: f32 = e;
            if assemble {
                t = 1.0 - e;
            }
            let d = sgn * t * la;
            let gh = piece_gap(e);
            let sa = a - d;
            let sac = ac - d;
            let lo = fj * pa;
            // Its own panel by index, as the pixel's row was found, so at
            // rest every pixel is in exactly one; the gap once open.
            let in_panel = floor(sac / pa) == fj && (gh <= 0.0 || (sac >= lo + gh && sac < lo + pa - gh && fb >= gh && fb < pb - gh));
            if in_panel && abs(d) >= best {
                best = abs(d);
                best_a = sa;
            }
        }
        if best >= 0.0 {
            var sx: f32 = best_a;
            var sy: f32 = b;
            if vertical {
                sx = b;
                sy = best_a;
            }
            result = bilinear(assemble, sx - 0.5, sy - 0.5);
        } else if assemble {
            result = from_px;
        } else {
            result = to_px;
        }
    } else if params.kind_id == 70u {
        // Part Halves (#985): the picture splits down a line through the
        // middle, and the halves part across it (or slide along it, the
        // ways apart). A half only ever covers its own side of the line.
        // param0 = 0 part / 1 slide along, param1 = the line's angle in
        // degrees, param2 = stagger.
        let bits = piece_bits();
        let w = f32(params.width);
        let h = f32(params.height);
        let ang = params.param1 * 0.017453292;
        let lx = cos(ang);
        let ly = sin(ang);
        let qx = f32(x) + 0.5 - w * 0.5;
        let qy = f32(y) + 0.5 - h * 0.5;
        // The side, judged a hair off the pixel's centre, as the strips are.
        let cx = qx + 0.001953125;
        let cy = qy + 0.0029296875;
        let v = cy * lx - cx * ly;
        var side: f32 = 1.0;
        var i: u32 = 1u;
        if v < 0.0 {
            side = -1.0;
            i = 0u;
        }
        var dx: f32 = -ly * side;
        var dy: f32 = lx * side;
        var dist: f32 = (abs(w * ly) + abs(h * lx)) * 0.5 + 1.0;
        if params.param0 > 0.5 {
            dx = lx * side;
            dy = ly * side;
            dist = abs(w * lx) + abs(h * ly);
        }
        let e = piece_travel(piece_key(i, f32(i), 0.0));
        let assemble = (bits & 32u) != 0u;
        var t: f32 = e;
        if assemble {
            t = 1.0 - e;
        }
        let spx = qx - dx * t * dist;
        let spy = qy - dy * t * dist;
        let sv = ((cy - dy * t * dist) * lx - (cx - dx * t * dist) * ly) * side;
        let sx = spx + w * 0.5;
        let sy = spy + h * 0.5;
        let sxc = cx - dx * t * dist + w * 0.5;
        let syc = cy - dy * t * dist + h * 0.5;
        if sv >= piece_gap(e) && sxc >= 0.0 && sxc < w && syc >= 0.0 && syc < h {
            result = bilinear(assemble, sx - 0.5, sy - 0.5);
        } else if assemble {
            result = from_px;
        } else {
            result = to_px;
        }
    } else if params.kind_id == 63u || params.kind_id == 64u || params.kind_id == 65u {
        // Zoom family: scale from/to around frame center.
        let zoom_amount = max(params.param0, 1.001);
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        var ep: f32 = p;
        if params.kind_id == 65u && params.param1 > 0.5 {
            ep = p * p * (3.0 - 2.0 * p);
        }
        var from_sample: vec4<f32>;
        var to_sample: vec4<f32>;
        if params.kind_id == 63u {
            let scale_a = 1.0 + (zoom_amount - 1.0) * ep;
            let from_sx = clamp(dx / scale_a + cx, 0.0, f32(params.width) - 1.0);
            let from_sy = clamp(dy / scale_a + cy, 0.0, f32(params.height) - 1.0);
            from_sample = bilinear(false, from_sx, from_sy);
            to_sample = to_px;
        } else if params.kind_id == 64u {
            let scale_a = zoom_amount - (zoom_amount - 1.0) * ep;
            let from_sx = clamp(dx / scale_a + cx, 0.0, f32(params.width) - 1.0);
            let from_sy = clamp(dy / scale_a + cy, 0.0, f32(params.height) - 1.0);
            from_sample = bilinear(false, from_sx, from_sy);
            to_sample = to_px;
        } else {
            let scale_a = 1.0 + (zoom_amount - 1.0) * ep;
            let scale_b = zoom_amount - (zoom_amount - 1.0) * ep;
            let from_sx = clamp(dx / scale_a + cx, 0.0, f32(params.width) - 1.0);
            let from_sy = clamp(dy / scale_a + cy, 0.0, f32(params.height) - 1.0);
            let to_sx = clamp(dx / scale_b + cx, 0.0, f32(params.width) - 1.0);
            let to_sy = clamp(dy / scale_b + cy, 0.0, f32(params.height) - 1.0);
            from_sample = bilinear(false, from_sx, from_sy);
            to_sample = bilinear(true, to_sx, to_sy);
        }
        result = lerp_pixel(from_sample, to_sample, ep);
    } else if params.kind_id == 62u {
        // Bokeh Dissolve: screen-blend bloom + chromatic fringe.
        let bokeh_size = params.param0;
        let brightness_boost = params.param1;
        let envelope = sin(p * 3.14159265);
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722)) / 255.0;
        let bloom_strength = luma * bokeh_size * envelope * brightness_boost * 2.0;
        let bloom_val = bloom_strength * 255.0;
        let screen_r = 255.0 - (255.0 - base.x) * (255.0 - bloom_val) / 255.0;
        let screen_g = 255.0 - (255.0 - base.y) * (255.0 - bloom_val) / 255.0;
        let screen_b = 255.0 - (255.0 - base.z) * (255.0 - bloom_val) / 255.0;
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = (f32(x) - cx) / max(cx, 1.0);
        let dy = (f32(y) - cy) / max(cy, 1.0);
        let radial = length(vec2<f32>(dx, dy));
        let fringe = radial * bokeh_size * envelope * 8.0;
        result = vec4<f32>(
            clamp(screen_r + fringe, 0.0, 255.0),
            clamp(screen_g, 0.0, 255.0),
            clamp(screen_b - fringe, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 60u {
        // Anamorphic Flare: horizontal streak across frame center.
        let intensity = params.param0;
        let streak_width = params.param1;
        let flare_color = vec3<f32>(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let envelope = sin(p * 3.14159265) * intensity;
        let base = lerp_pixel(from_px, to_px, p);
        let ny = abs(f32(y) / f32(params.height) - 0.5) * 2.0;
        let streak_t = max(1.0 - ny / max(streak_width, 0.001), 0.0);
        let streak_t2 = streak_t * streak_t;
        let shimmer = 0.7 + 0.3 * sin(f32(x) / f32(params.width) * 3.14159265 * 8.0);
        let flare_strength = streak_t2 * envelope * shimmer;
        let flare = flare_color * flare_strength;
        result = vec4<f32>(
            clamp(base.x + flare.x, 0.0, 255.0),
            clamp(base.y + flare.y, 0.0, 255.0),
            clamp(base.z + flare.z, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 61u {
        // Dream Sequence: bloom + warm glow + overexposure.
        let bloom_intensity = params.param0;
        let warmth = params.param1;
        let envelope = sin(p * 3.14159265);
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722)) / 255.0;
        let bloom = luma * bloom_intensity * envelope * 60.0;
        let warm_r = warmth * envelope * 30.0;
        let warm_g = warmth * envelope * 15.0;
        let warm_b = -warmth * envelope * 10.0;
        let lift = envelope * bloom_intensity * 15.0;
        result = vec4<f32>(
            clamp(base.x + bloom + warm_r + lift, 0.0, 255.0),
            clamp(base.y + bloom + warm_g + lift, 0.0, 255.0),
            clamp(base.z + bloom + warm_b + lift, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 58u {
        // Projector Flicker: brightness flicker + gate weave + frame jump.
        let flicker_intensity = params.param0;
        let weave_amount = params.param1;
        let envelope = sin(p * 3.14159265) * flicker_intensity;
        let frame_hash = rand3(bitcast<u32>(p), 0u, 3u);
        let frame_hash2 = rand3(bitcast<u32>(p), 0u, 4u);
        let frame_hash3 = rand3(bitcast<u32>(p), 0u, 5u);
        let brightness = 1.0 + (frame_hash - 0.5) * envelope * 0.6;
        let weave = (frame_hash2 - 0.5) * weave_amount * envelope;
        let sx = clamp(f32(x) + weave, 0.0, f32(params.width) - 1.0);
        var jump: f32 = 0.0;
        if frame_hash3 < envelope * 0.1 {
            jump = (frame_hash3 - 0.5) * 8.0;
        }
        let sy = clamp(f32(y) + jump, 0.0, f32(params.height) - 1.0);
        let sample_idx = u32(sy) * params.width + u32(sx);
        let from_s = to_bytes(unpack(from_buf[sample_idx]));
        let to_s = to_bytes(unpack(to_buf[sample_idx]));
        let base = lerp_pixel(from_s, to_s, p);
        result = vec4<f32>(
            clamp(base.x * brightness, 0.0, 255.0),
            clamp(base.y * brightness, 0.0, 255.0),
            clamp(base.z * brightness, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 59u {
        // Halation: per-pixel brightness-threshold additive glow. O(1) per pixel.
        let threshold = params.param0;
        let spread = params.param1;
        let halo_color = vec3<f32>(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let envelope = sin(p * 3.14159265);
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722)) / 255.0;
        let excess = max(luma - threshold, 0.0) / max(1.0 - threshold, 0.001);
        let halo_strength = excess * spread * envelope * 3.0;
        let halo = halo_color * halo_strength;
        result = vec4<f32>(
            clamp(base.x + halo.x, 0.0, 255.0),
            clamp(base.y + halo.y, 0.0, 255.0),
            clamp(base.z + halo.z, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 57u {
        // Glitch RGB Split: per-block RGB channel separation with blackouts.
        let intensity = params.param0;
        let block_size = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let block_x = u32(f32(x) / block_size);
        let block_y = u32(f32(y) / block_size);
        let hash_r = rand3(block_x + block_y * 65536u, bitcast<u32>(p), 6u);
        let hash_g = rand3(block_x + block_y * 65536u, bitcast<u32>(p), 7u);
        let hash_b = rand3(block_x + block_y * 65536u, bitcast<u32>(p), 8u);
        let r_shift = (hash_r - 0.5) * envelope * 80.0;
        let g_shift = (hash_g - 0.5) * envelope * 60.0;
        let b_shift = (hash_b - 0.5) * envelope * 80.0;
        let rx = clamp(f32(x) + r_shift, 0.0, f32(params.width) - 1.0);
        let gx = clamp(f32(x) + g_shift, 0.0, f32(params.width) - 1.0);
        let bx = clamp(f32(x) + b_shift, 0.0, f32(params.width) - 1.0);
        let r_idx = u32(y) * params.width + u32(rx);
        let g_idx = u32(y) * params.width + u32(gx);
        let b_idx = u32(y) * params.width + u32(bx);
        let r_val = lerp_pixel(to_bytes(unpack(from_buf[r_idx])), to_bytes(unpack(to_buf[r_idx])), p).x;
        let g_val = lerp_pixel(to_bytes(unpack(from_buf[g_idx])), to_bytes(unpack(to_buf[g_idx])), p).y;
        let b_val = lerp_pixel(to_bytes(unpack(from_buf[b_idx])), to_bytes(unpack(to_buf[b_idx])), p).z;
        var blackout: f32 = 1.0;
        if hash_r < envelope * 0.15 {
            blackout = 0.0;
        }
        result = vec4<f32>(r_val * blackout, g_val * blackout, b_val * blackout, 255.0);
    } else if params.kind_id == 55u {
        // Static TV: noise + scan lines + horizontal band shift.
        let intensity = params.param0;
        let line_h = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let hash = rand3(x + y * 65536u, bitcast<u32>(p), 9u);
        let hash2 = rand3(x + y * 65536u, bitcast<u32>(p), 10u);
        var scan_line: f32 = 1.0;
        if (u32(f32(y) / line_h) % 2u) == 0u {
            scan_line = 0.75;
        }
        let static_val = hash * 255.0 * envelope * scan_line;
        let band = u32(f32(y) / line_h);
        let band_hash = rand3(band, bitcast<u32>(p), 11u);
        let shift = (band_hash - 0.5) * envelope * 20.0;
        let sx = clamp(f32(x) + shift, 0.0, f32(params.width) - 1.0);
        let sample_idx = u32(y) * params.width + u32(sx);
        let from_s = to_bytes(unpack(from_buf[sample_idx]));
        let to_s = to_bytes(unpack(to_buf[sample_idx]));
        let base = lerp_pixel(from_s, to_s, p);
        let static_mix = envelope * hash2 * 0.8;
        result = vec4<f32>(
            clamp(base.x + (static_val - base.x) * static_mix, 0.0, 255.0),
            clamp(base.y + (static_val - base.y) * static_mix, 0.0, 255.0),
            clamp(base.z + (static_val - base.z) * static_mix, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 56u {
        // VHS Tracking: horizontal band shifts + chroma bleed.
        let intensity = params.param0;
        let band_count = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let band = u32(f32(y) / f32(params.height) * band_count);
        let band_hash = rand3(band, bitcast<u32>(p), 11u);
        let band_hash2 = rand3(band, bitcast<u32>(p), 12u);
        let shift = (band_hash - 0.5) * envelope * 40.0;
        let sx = clamp(f32(x) + shift, 0.0, f32(params.width) - 1.0);
        let sample_idx = u32(y) * params.width + u32(sx);
        let from_s = to_bytes(unpack(from_buf[sample_idx]));
        let to_s = to_bytes(unpack(to_buf[sample_idx]));
        let base = lerp_pixel(from_s, to_s, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722));
        let chroma_shift = (band_hash2 - 0.5) * envelope * 15.0;
        let csx = clamp(f32(x) + chroma_shift, 0.0, f32(params.width) - 1.0);
        let c_idx = u32(y) * params.width + u32(csx);
        let from_c = to_bytes(unpack(from_buf[c_idx]));
        let to_c = to_bytes(unpack(to_buf[c_idx]));
        let chroma_base = lerp_pixel(from_c, to_c, p);
        let vhs_r = mix(base.x, chroma_base.x, envelope * 0.5);
        let vhs_g = mix(base.y, luma, envelope * 0.5);
        let vhs_b = mix(base.z, chroma_base.z, envelope * 0.5);
        let brightness = 1.0 - band_hash * envelope * 0.4;
        result = vec4<f32>(
            clamp(vhs_r * brightness, 0.0, 255.0),
            clamp(vhs_g * brightness, 0.0, 255.0),
            clamp(vhs_b * brightness, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 53u {
        // Swirl: rotational distortion spiraling from center.
        let cx = f32(params.width) * 0.5;
        let cy = f32(params.height) * 0.5;
        let dx = f32(x) - cx;
        let dy = f32(y) - cy;
        let dist = length(vec2<f32>(dx, dy));
        let max_r = max(length(vec2<f32>(cx, cy)), 1.0);
        let norm_dist = dist / max_r;
        let max_angle = params.param0 * 3.14159265 / 180.0;
        let falloff = params.param1;
        let envelope = sin(p * 3.14159265);
        let swirl_angle = max_angle * envelope * exp(-norm_dist * falloff);
        let cos_a = cos(swirl_angle);
        let sin_a = sin(swirl_angle);
        let sx = clamp(cos_a * dx - sin_a * dy + cx, 0.0, f32(params.width) - 1.0);
        let sy = clamp(sin_a * dx + cos_a * dy + cy, 0.0, f32(params.height) - 1.0);
        let sample_idx = u32(sy) * params.width + u32(sx);
        let from_s = to_bytes(unpack(from_buf[sample_idx]));
        let to_s = to_bytes(unpack(to_buf[sample_idx]));
        result = lerp_pixel(from_s, to_s, p);
    } else if params.kind_id == 54u {
        // Wave Distortion: sinusoidal displacement.
        let amplitude = params.param0;
        let frequency = params.param1;
        let dir = u32(params.param2 + 0.5);
        let envelope = sin(p * 3.14159265) * amplitude;
        var off_x: f32 = 0.0;
        var off_y: f32 = 0.0;
        if dir == 0u {
            off_x = sin(f32(y) / f32(params.height) * frequency * 6.28318530) * envelope;
        } else if dir == 1u {
            off_y = sin(f32(x) / f32(params.width) * frequency * 6.28318530) * envelope;
        } else {
            off_x = sin(f32(y) / f32(params.height) * frequency * 6.28318530) * envelope * 0.7;
            off_y = sin(f32(x) / f32(params.width) * frequency * 6.28318530) * envelope * 0.7;
        }
        let sx = clamp(f32(x) + off_x, 0.0, f32(params.width) - 1.0);
        let sy = clamp(f32(y) + off_y, 0.0, f32(params.height) - 1.0);
        let from_s = bilinear(false, sx, sy);
        let to_s = bilinear(true, sx, sy);
        result = lerp_pixel(from_s, to_s, p);
    } else if params.kind_id == 51u {
        // Colorize Flash: user-defined color washes over at the cut.
        let flash_color = vec3<f32>(
            srgb_to_linear_component(params.color_r),
            srgb_to_linear_component(params.color_g),
            srgb_to_linear_component(params.color_b),
        ) * 255.0;
        let intensity = clamp(params.param0, 0.0, 1.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        var flash_t: f32;
        if p < ramp_up {
            flash_t = p / max(ramp_up, 0.001);
        } else if p < peak {
            flash_t = 1.0;
        } else if p < ramp_down {
            flash_t = 1.0 - (p - peak) / max(ramp_down - peak, 0.001);
        } else {
            flash_t = 0.0;
        }
        flash_t = flash_t * intensity;
        let dissolve_start = peak * 0.8;
        let dissolve_t = clamp(
            (p - dissolve_start) / max(1.0 - dissolve_start, 0.001),
            0.0,
            1.0,
        );
        let base = lerp_pixel(from_px, to_px, dissolve_t);
        result = vec4<f32>(
            mix(base.x, flash_color.x, flash_t),
            mix(base.y, flash_color.y, flash_t),
            mix(base.z, flash_color.z, flash_t),
            255.0,
        );
    } else if params.kind_id == 52u {
        // Hue Rotate: hue spins through the color wheel at midpoint.
        let rotations = params.param0;
        let envelope = sin(p * 3.14159265) * rotations;
        let hue_shift = envelope * 360.0;
        let base = lerp_pixel(from_px, to_px, p);
        let r = base.x / 255.0;
        let g = base.y / 255.0;
        let b = base.z / 255.0;
        let cmax = max(r, max(g, b));
        let cmin = min(r, min(g, b));
        let delta = cmax - cmin;
        // RGB → HSV (hue in degrees)
        var h: f32 = 0.0;
        if delta > 0.001 {
            if abs(cmax - r) < 0.001 {
                let tmp = (g - b) / delta;
                h = 60.0 * (tmp - floor(tmp / 6.0) * 6.0);
            } else if abs(cmax - g) < 0.001 {
                h = 60.0 * ((b - r) / delta + 2.0);
            } else {
                h = 60.0 * ((r - g) / delta + 4.0);
            }
        }
        let s = select(0.0, delta / cmax, cmax > 0.001);
        let v = cmax;
        // Shift hue, normalize to [0, 360)
        var h2 = h + hue_shift;
        h2 = h2 - floor(h2 / 360.0) * 360.0;
        h2 = h2 - floor(h2 / 360.0) * 360.0;
        // HSV → RGB
        let c = v * s;
        let h_sector = h2 / 60.0;
        let h_mod2 = h_sector - floor(h_sector / 2.0) * 2.0;
        let x2 = c * (1.0 - abs(h_mod2 - 1.0));
        let m = v - c;
        let hi = u32(h_sector) % 6u;
        var rgb: vec3<f32>;
        if hi == 0u { rgb = vec3<f32>(c, x2, 0.0); }
        else if hi == 1u { rgb = vec3<f32>(x2, c, 0.0); }
        else if hi == 2u { rgb = vec3<f32>(0.0, c, x2); }
        else if hi == 3u { rgb = vec3<f32>(0.0, x2, c); }
        else if hi == 4u { rgb = vec3<f32>(x2, 0.0, c); }
        else { rgb = vec3<f32>(c, 0.0, x2); }
        result = vec4<f32>((rgb + vec3<f32>(m)) * 255.0, 255.0);
    } else if params.kind_id == 49u {
        // Negative Flash: brief inversion at the cut point.
        let intensity = clamp(params.param0, 0.0, 1.0);
        let peak = clamp(params.param1, 0.05, 0.95);
        let ramp_up = peak * 0.6;
        let ramp_down = peak + (1.0 - peak) * 0.4;
        var flash_t: f32;
        if p < ramp_up {
            flash_t = p / max(ramp_up, 0.001);
        } else if p < peak {
            flash_t = 1.0;
        } else if p < ramp_down {
            flash_t = 1.0 - (p - peak) / max(ramp_down - peak, 0.001);
        } else {
            flash_t = 0.0;
        }
        flash_t = flash_t * intensity;
        let dissolve_start = peak * 0.8;
        let dissolve_t = clamp(
            (p - dissolve_start) / max(1.0 - dissolve_start, 0.001),
            0.0,
            1.0,
        );
        let base = lerp_pixel(from_px, to_px, dissolve_t);
        let negative = vec4<f32>(255.0 - base.x, 255.0 - base.y, 255.0 - base.z, 255.0);
        result = lerp_pixel(base, negative, flash_t);
    } else if params.kind_id == 50u {
        // Sepia Transition: warm sepia tone through the cut.
        let strength = clamp(params.param0, 0.0, 1.0);
        let warmth = clamp(params.param1, 0.5, 2.0);
        let envelope = sin(p * 3.14159265) * strength;
        let base = lerp_pixel(from_px, to_px, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722));
        let sepia_r = clamp(luma * warmth * 1.2, 0.0, 255.0);
        let sepia_g = clamp(luma * warmth * 1.0, 0.0, 255.0);
        let sepia_b = clamp(luma * warmth * 0.75, 0.0, 255.0);
        let sepia = vec4<f32>(sepia_r, sepia_g, sepia_b, 255.0);
        result = lerp_pixel(base, sepia, envelope);
    } else if params.kind_id == 47u {
        // Glitch Dissolve: block shifts + RGB split + noise.
        let intensity = params.param0;
        let block_size = max(params.param1, 1.0);
        let envelope = sin(p * 3.14159265) * intensity;
        let block_row = u32(f32(y) / block_size);
        let hash = rand3(block_row, bitcast<u32>(p), 13u);
        let hash2 = rand3(block_row, bitcast<u32>(p), 14u);
        let shift = (hash - 0.5) * envelope * 40.0;
        let sx_g = clamp(f32(x) + shift, 0.0, f32(params.width) - 1.0);
        let r_shift = shift * 0.5;
        let b_shift = -shift * 0.5;
        let rx = clamp(f32(x) + r_shift, 0.0, f32(params.width) - 1.0);
        let bx = clamp(f32(x) + b_shift, 0.0, f32(params.width) - 1.0);
        let g_idx = u32(y) * params.width + u32(sx_g);
        let r_idx = u32(y) * params.width + u32(rx);
        let b_idx = u32(y) * params.width + u32(bx);
        let base_g = lerp_pixel(to_bytes(unpack(from_buf[g_idx])), to_bytes(unpack(to_buf[g_idx])), p);
        let r_base = lerp_pixel(to_bytes(unpack(from_buf[r_idx])), to_bytes(unpack(to_buf[r_idx])), p);
        let b_base = lerp_pixel(to_bytes(unpack(from_buf[b_idx])), to_bytes(unpack(to_buf[b_idx])), p);
        let noise = (hash2 - 0.5) * envelope * 60.0;
        result = vec4<f32>(
            clamp(r_base.x + noise, 0.0, 255.0),
            clamp(base_g.y + noise * 0.5, 0.0, 255.0),
            clamp(b_base.z + noise, 0.0, 255.0),
            255.0,
        );
    } else if params.kind_id == 48u {
        // Desaturate Transition: drain to grey at midpoint.
        let depth = clamp(params.param0, 0.0, 1.0);
        let envelope = sin(p * 3.14159265) * depth;
        let base = lerp_pixel(from_px, to_px, p);
        // Luma in 0-255 space.
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722));
        let grey = vec4<f32>(luma, luma, luma, 255.0);
        result = mix(base, grey, envelope);
    } else if params.kind_id == 45u {
        // Additive Dissolve: cross-dissolve with additive brightness boost.
        let intensity = params.param0;
        let envelope = sin(p * 3.14159265) * intensity;
        let base = lerp_pixel(from_px, to_px, p);
        let add_val = envelope * 0.5 * 255.0;
        result = clamp(
            base + vec4<f32>(add_val, add_val, add_val, 0.0),
            vec4<f32>(0.0),
            vec4<f32>(255.0, 255.0, 255.0, 255.0),
        );
    } else if params.kind_id == 46u {
        // Fade to Color: A → solid color → B. User-supplied sRGB → linear.
        let fade_color = vec4<f32>(
            srgb_to_linear_component(params.color_r) * 255.0,
            srgb_to_linear_component(params.color_g) * 255.0,
            srgb_to_linear_component(params.color_b) * 255.0,
            255.0,
        );
        let hold = clamp(params.param0, 0.0, 0.45);
        let mid_start = 0.5 - hold * 0.5;
        let mid_end = 0.5 + hold * 0.5;
        if p <= mid_start {
            let t = p / max(mid_start, 0.001);
            result = lerp_pixel(from_px, fade_color, t);
        } else if p >= mid_end {
            let t = (p - mid_end) / max(1.0 - mid_end, 0.001);
            result = lerp_pixel(fade_color, to_px, t);
        } else {
            result = fade_color;
        }
    } else {
        result = lerp_pixel(from_px, to_px, p);
    }

    output[idx] = pack(from_bytes(result));
}
"
);

/// Posed piece transitions (#985, phase 2): the plan `pieces::plan` works
/// out (each piece's screen-to-picture homography, box, edges, light,
/// opacity and shadow, nearest first, an instant at a time) drawn front
/// to back over what shows behind, shadows on that, instants averaged.
/// `pieces::render` is its CPU twin, line for line.
const SHADER_PIECES: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    count: u32,
    samples: u32,
    background_to: u32,
    subject: u32,
    _pad1: u32,
    _pad2: u32,
}

@group(0) @binding(0) var<storage, read> from_buf: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> to_buf: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
@group(0) @binding(4) var<storage, read> pieces: array<f32>;
@group(0) @binding(5) var<storage, read> matte: array<f32>;

const STRIDE: u32 = 60u;
const O_BOX: u32 = 9u;
const O_JAC: u32 = 13u;
const O_SIDE: u32 = 14u;
const O_SHADE: u32 = 15u;
const O_OPACITY: u32 = 16u;
const O_SHADOW: u32 = 17u;
const O_AA: u32 = 21u;
const O_MATTE: u32 = 22u;
const O_PLANES: u32 = 24u;

// The matte at a picture point, as `piece_sample` reads the pictures;
// nothing without one (`pieces::matte_sample`).
fn matte_sample(sx: f32, sy: f32) -> f32 {
    if params.subject != 2u {
        return 0.0;
    }
    let fx = min(max(sx, 0.0), f32(params.width) - 1.0);
    let fy = min(max(sy, 0.0), f32(params.height) - 1.0);
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, params.width - 1u);
    let y1 = min(y0 + 1u, params.height - 1u);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    let m00 = matte[y0 * params.width + x0];
    let m10 = matte[y0 * params.width + x1];
    let m01 = matte[y1 * params.width + x0];
    let m11 = matte[y1 * params.width + x1];
    let top = m00 + (m10 - m00) * tx;
    let bot = m01 + (m11 - m01) * tx;
    return clamp(top + (bot - top) * ty, 0.0, 1.0);
}

// The share of the piece at `base` the matte leaves at a picture point
// (`pieces::matte_share`).
fn matte_share(base: u32, sx: f32, sy: f32) -> f32 {
    let part = pieces[base + O_MATTE];
    if params.subject == 0u || part == 0.0 {
        return 1.0;
    }
    let v = matte_sample(sx - 0.5, sy - 0.5);
    if part > 0.0 {
        return v;
    }
    return 1.0 - v;
}

fn piece_sample(to_side: bool, sx: f32, sy: f32) -> vec4<f32> {
    let fx = min(max(sx, 0.0), f32(params.width) - 1.0);
    let fy = min(max(sy, 0.0), f32(params.height) - 1.0);
    let x0 = u32(floor(fx));
    let y0 = u32(floor(fy));
    let x1 = min(x0 + 1u, params.width - 1u);
    let y1 = min(y0 + 1u, params.height - 1u);
    let tx = fx - f32(x0);
    let ty = fy - f32(y0);
    var c00: vec4<f32>;
    var c10: vec4<f32>;
    var c01: vec4<f32>;
    var c11: vec4<f32>;
    if to_side {
        c00 = unpack(to_buf[y0 * params.width + x0]);
        c10 = unpack(to_buf[y0 * params.width + x1]);
        c01 = unpack(to_buf[y1 * params.width + x0]);
        c11 = unpack(to_buf[y1 * params.width + x1]);
    } else {
        c00 = unpack(from_buf[y0 * params.width + x0]);
        c10 = unpack(from_buf[y0 * params.width + x1]);
        c01 = unpack(from_buf[y1 * params.width + x0]);
        c11 = unpack(from_buf[y1 * params.width + x1]);
    }
    let top = c00 + (c10 - c00) * tx;
    let bot = c01 + (c11 - c01) * tx;
    return top + (bot - top) * ty;
}

// The distance inside the piece at `base` of the picture point the
// screen point (x, y) maps to (x: in picture pixels), and that point
// (y, z); w < 0 outside its box or behind the camera.
fn piece_at(base: u32, x: f32, y: f32) -> vec4<f32> {
    if x < pieces[base + O_BOX] || y < pieces[base + O_BOX + 1u] || x > pieces[base + O_BOX + 2u] || y > pieces[base + O_BOX + 3u] {
        return vec4<f32>(0.0, 0.0, 0.0, -1.0);
    }
    let wz = pieces[base + 6u] * x + pieces[base + 7u] * y + pieces[base + 8u];
    if wz <= 1e-9 {
        return vec4<f32>(0.0, 0.0, 0.0, -1.0);
    }
    let sx = (pieces[base] * x + pieces[base + 1u] * y + pieces[base + 2u]) / wz;
    let sy = (pieces[base + 3u] * x + pieces[base + 4u] * y + pieces[base + 5u]) / wz;
    var sd: f32 = 1.0e6;
    for (var k: u32 = 0u; k < 12u; k = k + 1u) {
        let o = base + O_PLANES + k * 3u;
        sd = min(sd, pieces[o] * sx + pieces[o + 1u] * sy + pieces[o + 2u]);
    }
    return vec4<f32>(sd, sx, sy, 1.0);
}

fn coverage(base: u32, sd: f32) -> f32 {
    let aa = pieces[base + O_AA] * pieces[base + O_JAC];
    if aa <= 1e-4 {
        if sd >= -0.01 {
            return 1.0;
        }
        return 0.0;
    }
    return clamp(sd / aa + 0.5, 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width) + 0.5;
    let y = f32(idx / params.width) + 0.5;
    let hx = x + 0.001953125;
    let hy = y + 0.0029296875;
    var bg: vec4<f32>;
    if params.background_to == 1u {
        bg = unpack(to_buf[idx]);
    } else {
        bg = unpack(from_buf[idx]);
    }
    var total = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    for (var s: u32 = 0u; s < 8u; s = s + 1u) {
        if s >= params.samples {
            break;
        }
        var acc = vec4<f32>(0.0, 0.0, 0.0, 0.0);
        var alpha: f32 = 0.0;
        var shadow: f32 = 0.0;
        var subject_alpha: f32 = 0.0;
        for (var i: u32 = 0u; i < 129u; i = i + 1u) {
            if i >= params.count {
                break;
            }
            let base = (s * params.count + i) * STRIDE;
            if alpha < 0.999 {
                let hit = piece_at(base, hx, hy);
                if hit.w > 0.0 {
                    let cov0 = coverage(base, hit.x) * pieces[base + O_OPACITY];
                    if cov0 > 0.0 {
                        var at = piece_at(base, x, y);
                        if at.w <= 0.0 {
                            at = vec4<f32>(hit.x, 0.0, 0.0, 1.0);
                        }
                        let cov = cov0 * matte_share(base, at.y, at.z);
                        let c = piece_sample(pieces[base + O_SIDE] > 0.5, at.y - 0.5, at.z - 0.5);
                        var k = (1.0 - alpha) * cov;
                        if pieces[base + O_MATTE] < 0.0 && params.subject != 0u {
                            k = min((1.0 - alpha) * cov / (1.0 - subject_alpha), 1.0 - alpha);
                        }
                        if pieces[base + O_MATTE] > 0.0 {
                            subject_alpha = subject_alpha + k;
                        }
                        let shade = pieces[base + O_SHADE];
                        acc = acc + vec4<f32>(k * c.x * shade, k * c.y * shade, k * c.z * shade, k * c.w);
                        alpha = alpha + k;
                    }
                }
            }
            if pieces[base + O_SHADOW + 2u] > 0.0 {
                let sh = piece_at(base, hx - pieces[base + O_SHADOW], hy - pieces[base + O_SHADOW + 1u]);
                if sh.w > 0.0 {
                    let a = pieces[base + O_SHADOW + 2u] * clamp(sh.x / pieces[base + O_JAC] / pieces[base + O_SHADOW + 3u] + 0.5, 0.0, 1.0) * matte_share(base, sh.y, sh.z);
                    shadow = 1.0 - (1.0 - shadow) * (1.0 - a);
                }
            }
        }
        let dark = 1.0 - shadow;
        let rest = 1.0 - alpha;
        total = total + vec4<f32>(acc.x + rest * bg.x * dark, acc.y + rest * bg.y * dark, acc.z + rest * bg.z * dark, acc.w + rest * bg.w);
    }
    output[idx] = pack(total / f32(params.samples));
}
"
);

// Colour grade — the big one. Exposure, tone controls, LGG+offset
// wheels, contrast, saturation, temperature/tint, global mix. All
// run in linear so BT.709 luma matches its coefficient domain,
// exposure `2^stops` produces real photographic stops, contrast
// pivots at 0.18 (perceptual mid-grey), tone-control band centres
// map to the linear equivalents of the old 0.5/0.8/0.2 sRGB
// thresholds (0.214/0.604/0.033 linear — close perceptually, but
// now physically correct).
const SHADER_COLOR_GRADE: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    exposure: f32,
    contrast: f32,
    saturation: f32,
    wb0: f32, wb1: f32, wb2: f32,
    wb3: f32, wb4: f32, wb5: f32,
    wb6: f32, wb7: f32, wb8: f32,
    highlights: f32,
    shadows: f32,
    whites: f32,
    blacks: f32,
    lift_r: f32,
    lift_g: f32,
    lift_b: f32,
    gamma_r: f32,
    gamma_g: f32,
    gamma_b: f32,
    gain_r: f32,
    gain_g: f32,
    gain_b: f32,
    offset_r: f32,
    offset_g: f32,
    offset_b: f32,
    hue: f32,
    global_mix: f32,
    contrast_pivot_encoded: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    encoding: u32,
    log_mode: u32,
    log_low_r: f32, log_low_g: f32, log_low_b: f32,
    log_mid_r: f32, log_mid_g: f32, log_mid_b: f32,
    log_high_r: f32, log_high_g: f32, log_high_b: f32,
    log_low_range: f32,
    log_high_range: f32,
    luma_mix: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
// The grade model, transcribed from klipp_color::grade_model. Change
// one, change the other; the parity tests hold them equal.
const GRADE_GAMMA: f32 = 2.4;
const MID_GREY_ENCODED: f32 = 0.4894371;
fn grade_encode(x: f32) -> f32 {
    let e = pow(abs(x), 1.0 / GRADE_GAMMA);
    return select(e, -e, x < 0.0);
}
fn grade_decode(e: f32) -> f32 {
    let x = pow(abs(e), GRADE_GAMMA);
    return select(x, -x, e < 0.0);
}
// The grade encoding chosen (#467): 0 BT.1886, 1 ACEScct, 2 ACEScc.
// klipp_grade_model::{encode, decode, band_luma}, transcribed.
fn grade_encode_in(x: f32) -> f32 {
    if params.encoding == 1u {
        if x <= 0.0078125 { return 10.5402377 * x + 0.072905534; }
        return (log2(min(x, 65504.0)) + 9.72) / 17.52;
    }
    if params.encoding == 2u {
        if x <= 0.0 { return (-16.0 + 9.72) / 17.52; }
        if x < exp2(-15.0) { return (log2(exp2(-16.0) + x * 0.5) + 9.72) / 17.52; }
        return (log2(min(x, 65504.0)) + 9.72) / 17.52;
    }
    return grade_encode(x);
}
fn grade_decode_in(e: f32) -> f32 {
    let top = (log2(65504.0) + 9.72) / 17.52;
    if params.encoding == 1u {
        if e <= 0.15525114 { return (e - 0.072905534) / 10.5402377; }
        if e < top { return exp2(e * 17.52 - 9.72); }
        return 65504.0;
    }
    if params.encoding == 2u {
        if e < (9.72 - 15.0) / 17.52 { return (exp2(e * 17.52 - 9.72) - exp2(-16.0)) * 2.0; }
        if e < top { return exp2(e * 17.52 - 9.72); }
        return 65504.0;
    }
    return grade_decode(e);
}
fn band_luma(l: f32) -> f32 {
    if params.encoding == 0u { return l; }
    return grade_encode(grade_decode_in(l));
}
fn cdl_channel(e: f32, lift: f32, gamma: f32, gain: f32, offset: f32) -> f32 {
    let slope = (1.0 + gain) * (1.0 - lift);
    let t = e * slope + lift + offset;
    let power = 1.0 / max(1.0 + gamma, 0.05);
    return select(t, pow(t, power), t > 0.0);
}
fn contrast_channel_at(e: f32, amount: f32, pivot: f32) -> f32 {
    return (e - pivot) * (1.0 + amount) + pivot;
}
fn grade_smoothstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = clamp((x - lo) / (hi - lo), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}
// (highlights, shadows, whites, blacks)
fn tone_weights(l: f32) -> vec4<f32> {
    return vec4<f32>(
        grade_smoothstep(0.45, 0.85, l),
        1.0 - grade_smoothstep(0.30, 0.65, l),
        grade_smoothstep(0.65, 0.95, l),
        1.0 - grade_smoothstep(0.10, 0.40, l),
    );
}
fn grade_gauss(l: f32, centre: f32, sigma: f32) -> f32 {
    return exp(-((l - centre) * (l - centre)) / (2.0 * sigma * sigma));
}
// (low, mid, high), the low and high bands where the ranges put them
// (#900): klipp_grade_model::log_wheel_weights_in.
fn log_wheel_weights(l: f32) -> vec3<f32> {
    let low = clamp(params.log_low_range, 0.0, MID_GREY_ENCODED - 0.08);
    let high = clamp(params.log_high_range, MID_GREY_ENCODED + 0.08, 1.2);
    return vec3<f32>(grade_gauss(l, low, 0.14), grade_gauss(l, MID_GREY_ENCODED, 0.16), grade_gauss(l, high, 0.14));
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    var r = orig.x;
    var g = orig.y;
    var b = orig.z;

    // 1. Exposure (2^stops) — physically correct in linear.
    // 1. Exposure and white balance in linear light.
    let exp_factor = pow(2.0, params.exposure);
    r = r * exp_factor;
    g = g * exp_factor;
    b = b * exp_factor;
    let wb_r = params.wb0 * r + params.wb1 * g + params.wb2 * b;
    let wb_g = params.wb3 * r + params.wb4 * g + params.wb5 * b;
    let wb_b = params.wb6 * r + params.wb7 * g + params.wb8 * b;
    r = wb_r;
    g = wb_g;
    b = wb_b;
    // 2. In the grade encoding (klipp_color::grade_model, the CPU twin):
    //    the tone bands, the wheels as the ASC CDL, contrast about mid grey.
    let luma1 = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
    let l_band = band_luma(grade_encode_in(luma1));
    let tw = tone_weights(l_band);
    let tone_adj = params.highlights * tw.x + params.shadows * tw.y + params.whites * tw.z + params.blacks * tw.w;
    var er = grade_encode_in(r) + tone_adj;
    var eg = grade_encode_in(g) + tone_adj;
    var eb = grade_encode_in(b) + tone_adj;
    let before_wheels = vec3<f32>(er, eg, eb);
    if params.log_mode != 0u {
        let lw = log_wheel_weights(l_band);
        er = er + params.log_low_r * lw.x + params.log_mid_r * lw.y + params.log_high_r * lw.z;
        eg = eg + params.log_low_g * lw.x + params.log_mid_g * lw.y + params.log_high_g * lw.z;
        eb = eb + params.log_low_b * lw.x + params.log_mid_b * lw.y + params.log_high_b * lw.z;
    } else {
        er = cdl_channel(er, params.lift_r, params.gamma_r, params.gain_r, params.offset_r);
        eg = cdl_channel(eg, params.lift_g, params.gamma_g, params.gain_g, params.offset_g);
        eb = cdl_channel(eb, params.lift_b, params.gamma_b, params.gain_b, params.offset_b);
    }
    // The luma mix (#900), klipp_grade_model::luma_mix: the wheels'
    // change to luma taken back by its complement, in linear light.
    if params.luma_mix < 1.0 {
        let luma_c = vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb);
        let pre = vec3<f32>(grade_decode_in(before_wheels.x), grade_decode_in(before_wheels.y), grade_decode_in(before_wheels.z));
        let post = vec3<f32>(grade_decode_in(er), grade_decode_in(eg), grade_decode_in(eb));
        let back = (1.0 - clamp(params.luma_mix, 0.0, 1.0)) * (dot(luma_c, pre) - dot(luma_c, post));
        er = grade_encode_in(post.x + back);
        eg = grade_encode_in(post.y + back);
        eb = grade_encode_in(post.z + back);
    }
    er = contrast_channel_at(er, params.contrast, params.contrast_pivot_encoded);
    eg = contrast_channel_at(eg, params.contrast, params.contrast_pivot_encoded);
    eb = contrast_channel_at(eb, params.contrast, params.contrast_pivot_encoded);
    r = grade_decode_in(er);
    g = grade_decode_in(eg);
    b = grade_decode_in(eb);
    // 3. Back in linear light: saturation about the working space's luma.
    let luma3 = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
    r = luma3 + (r - luma3) * params.saturation;
    g = luma3 + (g - luma3) * params.saturation;
    b = luma3 + (b - luma3) * params.saturation;
    if abs(params.hue) >= 0.01 {
        let rot = hue_rotate_hsl(vec3<f32>(r, g, b), params.hue);
        r = rot.x;
        g = rot.y;
        b = rot.z;
    }

    // 8. Global mix (blend with original).
    r = orig.x + (r - orig.x) * params.global_mix;
    g = orig.y + (g - orig.y) * params.global_mix;
    b = orig.z + (b - orig.z) * params.global_mix;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
"
);

// Hue/Sat curves — six HSL-based curves (hue-vs-hue, hue-vs-sat,
// hue-vs-lum, lum-vs-sat, sat-vs-sat, orange-vs-sat). HSL math is
// colour-space-agnostic (works on any normalised RGB), but feeding
// it linear-light values means hue/sat/lum values shift vs sRGB
// (grey lum = 0.18 linear ≈ 0.46 sRGB). Curve authors re-tune in
// linear; the physical luminance axis of lum-vs-sat is now correct.
const SHADER_HUESAT: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    mix: f32,
    // Bit c set: curve c was edited. The others are left out: an
    // unedited curve's LUT is filler, and Sat vs Sat's would set every
    // saturation to 0.5.
    curves_on: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;
",
    shader_linear_helpers_wgsl!(),
    "
// LUT layout: 7 curves × 256 entries = indices 0..1791
// Curve 0: hue_vs_hue   (0..255)
// Curve 1: hue_vs_sat   (256..511)
// Curve 2: hue_vs_lum   (512..767)
// Curve 3: lum_vs_sat   (768..1023)
// Curve 4: sat_vs_sat   (1024..1279)
// Curve 5: orange_vs_sat (1280..1535)
// Curve 6: sat_vs_lum   (1536..1791)

fn lut_lookup(curve: u32, t: f32) -> f32 {
    let base = curve * 256u;
    let idx_f = t * 255.0;
    let idx0 = u32(clamp(idx_f, 0.0, 254.0));
    let idx1 = idx0 + 1u;
    let frac = idx_f - f32(idx0);
    return lut[base + idx0] * (1.0 - frac) + lut[base + idx1] * frac;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    let hsl = picture_hsl(orig.xyz);
    let lum_axis = picture_lum_axis(hsl);
    var h = hsl.h;
    var s = hsl.s;
    // The brightness the curves move is the value, not the lightness:
    // saturation is now a fraction of the brightest channel, so the two
    // have to be adjusted in the same currency or a lightness curve is
    // computed and then discarded. For a grey the two are identical; for
    // a saturated colour a shift here moves the peak channel.
    var v = hsl.v;

    // Curve 0: Hue vs Hue (0.5 = no change)
    if (params.curves_on & 1u) != 0u {
        let hvh = lut_lookup(0u, h);
        h = (h + (hvh - 0.5) * 2.0) % 1.0;
        if h < 0.0 { h = h + 1.0; }
    }

    // Curve 1: Hue vs Sat (0.5 = identity)
    if (params.curves_on & 2u) != 0u {
        let hvs = lut_lookup(1u, hsl.h);
        s = clamp(s * hvs * 2.0, 0.0, 1.0);
    }

    // Curve 2: Hue vs Lum (0.5 = no change)
    if (params.curves_on & 4u) != 0u {
        let hvl = lut_lookup(2u, hsl.h);
        v = clamp(v + (hvl - 0.5), 0.0, 1.0);
    }

    // Curve 3: Lum vs Sat (0.5 = identity)
    if (params.curves_on & 8u) != 0u {
        let lvs = lut_lookup(3u, lum_axis);
        s = clamp(s * lvs * 2.0, 0.0, 1.0);
    }

    // Curve 4: Sat vs Sat, a mapping of the input saturation
    if (params.curves_on & 16u) != 0u {
        let svs = lut_lookup(4u, hsl.s);
        s = clamp(svs, 0.0, 1.0);
    }

    // Curve 5: Orange vs Sat (targeted at ~30°/360° ≈ 0.083)
    if (params.curves_on & 32u) != 0u {
        let ovs = lut_lookup(5u, hsl.s);
        let orange_dist = min(abs(hsl.h - 0.083), min(abs(hsl.h - 0.083 + 1.0), abs(hsl.h - 0.083 - 1.0)));
        let weight = clamp(1.0 - orange_dist * 8.0, 0.0, 1.0);
        s = s + (ovs - s) * weight;
    }

    // Curve 6: Sat vs Lum (0.5 = no change). Indexed by the incoming
    // saturation, like every other s-indexed curve, so an earlier
    // saturation curve does not move this one's input.
    if (params.curves_on & 64u) != 0u {
        let svl = lut_lookup(6u, hsl.s);
        v = clamp(v + (svl - 0.5), 0.0, 1.0);
    }

    let rgb = picture_rgb(h, s, v, hsl.magnitude);

    let mixed = orig.xyz + (rgb - orig.xyz) * params.mix;
    output[idx] = pack(vec4<f32>(mixed, orig.w));
}
"
);

// ─── Qualifier (Secondary Color Correction) GPU Shader ──────────────────────

// Qualifier — HSL-based isolation + secondary colour correction on
// matched pixels. HSL / hue-wheel values are now in linear light
// (grey lum = 0.18 rather than 0.46). The sec_exposure adjustment
// now acts as a physically correct 2^stops multiplier on the
// isolated region.
const SHADER_QUALIFIER: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    hue_center: f32,
    hue_width: f32,
    hue_soft: f32,
    sat_low: f32,
    sat_high: f32,
    sat_soft: f32,
    lum_low: f32,
    lum_high: f32,
    lum_soft: f32,
    invert: u32,
    sec_exposure: f32,
    sec_saturation: f32,
    sec_hue_shift: f32,
    sec_temperature: f32,
    matte_preview: u32,
    mode: u32,
    _pad1: u32,
    _pad2: u32,
    red_low: f32,
    red_high: f32,
    green_low: f32,
    green_high: f32,
    blue_low: f32,
    blue_high: f32,
    rgb_soft: f32,
    _pad3: f32,
    keep_mean: vec4<f32>,
    keep_inv0: vec4<f32>,
    keep_inv1: vec4<f32>,
    keep_inv2: vec4<f32>,
    remove_mean: vec4<f32>,
    remove_inv0: vec4<f32>,
    remove_inv1: vec4<f32>,
    remove_inv2: vec4<f32>,
    reach: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "

fn circular_distance(a: f32, b: f32) -> f32 {
    let d = abs(a - b);
    return min(d, 1.0 - d);
}

fn soft_range_centered(dist: f32, width: f32, soft: f32) -> f32 {
    let half_w = width * 0.5;
    if dist <= half_w { return 1.0; }
    if soft <= 0.0001 { return 0.0; }
    let t = (dist - half_w) / soft;
    return clamp(1.0 - t, 0.0, 1.0);
}

// The 3D key's space and distance (#895), as `cpu_grade::volume_matte`.
fn qualifier_ycc(e: vec3<f32>) -> vec3<f32> {
    let y = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
    return vec3<f32>(y, (e.z - y) / 1.8556, (e.x - y) / 1.5748);
}

fn volume_distance(c: vec3<f32>, mean: vec4<f32>, i0: vec4<f32>, i1: vec4<f32>, i2: vec4<f32>) -> f32 {
    let d = c - mean.xyz;
    let q = d.x * dot(i0.xyz, d) + d.y * dot(i1.xyz, d) + d.z * dot(i2.xyz, d);
    return sqrt(max(q, 0.0));
}

fn volume_reach(d: f32, radius: f32, soft: f32) -> f32 {
    if d <= radius { return 1.0; }
    if soft <= 0.0001 { return 0.0; }
    return clamp(1.0 - (d - radius) / soft, 0.0, 1.0);
}

fn soft_range(val: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    if val >= lo && val <= hi { return 1.0; }
    if soft <= 0.0001 { return 0.0; }
    if val < lo {
        return clamp(1.0 - (lo - val) / soft, 0.0, 1.0);
    }
    return clamp(1.0 - (val - hi) / soft, 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    let r = orig.x;
    let g = orig.y;
    let b = orig.z;

    // The picture's hue, saturation and lightness — display-encoded, and
    // defined for a pixel brighter than white, which used to read as
    // fully desaturated and so never qualified.
    let hsl = picture_hsl(orig.xyz);
    let h = hsl.h;
    let s = hsl.s;
    let l = picture_lum_axis(hsl);

    // Qualifier matte: by hue, saturation and luminance, or (RGB mode,
    // #895) by each display-encoded channel on its own.
    var matte = 0.0;
    if params.mode == 2u {
        if params.keep_mean.w > 0.5 {
            let e = clamp(vec3<f32>(enc_signed(r), enc_signed(g), enc_signed(b)), vec3<f32>(0.0), vec3<f32>(1.0));
            let c = qualifier_ycc(e);
            let dk = volume_distance(c, params.keep_mean, params.keep_inv0, params.keep_inv1, params.keep_inv2);
            matte = volume_reach(dk, params.reach.x, params.reach.y);
            if params.remove_mean.w > 0.5 {
                let dr = volume_distance(c, params.remove_mean, params.remove_inv0, params.remove_inv1, params.remove_inv2);
                if dr < dk {
                    matte = matte * (1.0 - volume_reach(dr, params.reach.x, params.reach.y));
                }
            }
        }
    } else if params.mode == 1u {
        let e = clamp(vec3<f32>(enc_signed(r), enc_signed(g), enc_signed(b)), vec3<f32>(0.0), vec3<f32>(1.0));
        matte = soft_range(e.x, params.red_low, params.red_high, params.rgb_soft)
            * soft_range(e.y, params.green_low, params.green_high, params.rgb_soft)
            * soft_range(e.z, params.blue_low, params.blue_high, params.rgb_soft);
    } else {
        let hue_dist = circular_distance(h, params.hue_center);
        let hue_m = soft_range_centered(hue_dist, params.hue_width, params.hue_soft);
        let sat_m = soft_range(s, params.sat_low, params.sat_high, params.sat_soft);
        let lum_m = soft_range(l, params.lum_low, params.lum_high, params.lum_soft);
        matte = hue_m * sat_m * lum_m;
    }
    if params.invert != 0u { matte = 1.0 - matte; }

    // Matte preview mode: output greyscale matte
    if params.matte_preview != 0u {
        output[idx] = pack(vec4<f32>(matte, matte, matte, orig.w));
        return;
    }

    // Skip untouched pixels
    if matte < 0.001 {
        output[idx] = input[idx];
        return;
    }

    // Secondary adjustments
    var gr = r;
    var gg = g;
    var gb = b;

    // Exposure
    if abs(params.sec_exposure) > 0.001 {
        let factor = pow(2.0, params.sec_exposure);
        gr = gr * factor;
        gg = gg * factor;
        gb = gb * factor;
    }

    // Temperature (R += t, B -= t)
    if abs(params.sec_temperature) > 0.0001 {
        gr = gr + params.sec_temperature;
        gb = gb - params.sec_temperature;
    }

    // Saturation
    if abs(params.sec_saturation - 1.0) > 0.001 {
        let luma = 0.2126 * gr + 0.7152 * gg + 0.0722 * gb;
        gr = luma + (gr - luma) * params.sec_saturation;
        gg = luma + (gg - luma) * params.sec_saturation;
        gb = luma + (gb - luma) * params.sec_saturation;
    }

    // Hue shift (rotation matrix — coefficients are from the classic
    // AE luma-rotation matrix, tuned for linear-ish RGB)
    if abs(params.sec_hue_shift) > 0.001 {
        let cos_h = cos(params.sec_hue_shift);
        let sin_h = sin(params.sec_hue_shift);
        let ir = gr; let ig = gg; let ib = gb;
        gr = ir * (0.213 + cos_h * 0.787 - sin_h * 0.213)
           + ig * (0.715 - cos_h * 0.715 - sin_h * 0.715)
           + ib * (0.072 - cos_h * 0.072 + sin_h * 0.928);
        gg = ir * (0.213 - cos_h * 0.213 + sin_h * 0.143)
           + ig * (0.715 + cos_h * 0.285 + sin_h * 0.140)
           + ib * (0.072 - cos_h * 0.072 - sin_h * 0.283);
        gb = ir * (0.213 - cos_h * 0.213 - sin_h * 0.787)
           + ig * (0.715 - cos_h * 0.715 + sin_h * 0.715)
           + ib * (0.072 + cos_h * 0.928 + sin_h * 0.072);
    }

    // Blend through matte
    let out = vec3<f32>(
        r * (1.0 - matte) + gr * matte,
        g * (1.0 - matte) + gg * matte,
        b * (1.0 - matte) + gb * matte,
    );
    output[idx] = pack(vec4<f32>(out, orig.w));
}
"
);

// ─── Luma Key GPU Shader ────────────────────────────────────────────────────
//
// Standalone luminance-threshold key. Mirrors `SHADER_QUALIFIER`'s
// matte-producer contract (greyscale RGBA output) but evaluates
// Rec.709 luma alone — no HSL machinery, no secondary grade. Used
// by the `NodeKind::LumaKey` graph node when GPU is available.
//
// **Colorspace note:** Rec.709 luma weights are technically wrong
// for AP1 (ACEScg working colorspace) — should be ~0.272/0.674/0.054
// there. Matches Qualifier's behaviour; both will be re-tuned
// post-Phase-E.
// ─── Difference Key GPU Shader ──────────────────────────────────────────────
//
// The CPU twin is `klipp_color::difference_key::generate_difference_key_matte`.
// Change one, change the other; `difference_key_gpu_parity` holds them
// together.
//
// Unlike the luma key, this keys *linear* light on both sides, because
// the CPU function is handed the linear frame and the linear plate and
// compares them directly. Encoding either here would make the same
// tolerance mean a different distance depending on which path ran.
const SHADER_DIFFERENCE_KEY: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    tolerance: f32,
    soft: f32,
    invert: u32,
    match_space: u32,
    _pad0: u32,
    _pad1: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> plate: array<vec4<f32>>;
",
    shader_pixel_storage_wgsl!(),
    "

// `soft_step` from the CPU twin: 0 below `lo`, a linear ramp to 1 across
// [lo, hi], 1 above, collapsing to a hard step when the band is empty.
// The 1e-4 floor is the CPU's, not a rounding guard — without it a zero
// `soft` divides by zero instead of keying hard.
fn diff_soft_step(val: f32, lo: f32, hi: f32) -> f32 {
    if val <= lo { return 0.0; }
    if hi - lo <= 0.0001 { return 1.0; }
    if val >= hi { return 1.0; }
    return clamp((val - lo) / (hi - lo), 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let live = unpack(input[idx]);
    let ref_px = unpack(plate[idx]);

    var d: f32;
    if params.match_space == 1u {
        // Weighted luminance difference — forgiving of plate chroma
        // drift. The working space's own row, since both sides are
        // linear working-space values.
        let yl = dot(live.xyz, params.luma.xyz);
        let yp = dot(ref_px.xyz, params.luma.xyz);
        d = abs(yl - yp);
    } else {
        // Chebyshev / L-infinity, per channel, so a colour-only
        // difference registers even when luminance matches.
        let e = abs(live.xyz - ref_px.xyz);
        d = max(e.x, max(e.y, e.z));
    }

    var matte = diff_soft_step(d, params.tolerance, params.tolerance + params.soft);
    if params.invert != 0u { matte = 1.0 - matte; }

    output[idx] = pack(vec4<f32>(matte, matte, matte, live.w));
}
"
);

/// Temporal noise reduction (#1988): `temporal_denoise::denoise_frame`
/// for one pixel, line for line. The current pixel weighs 1; each
/// neighbour weighs a gaussian on its luma's difference from the
/// current one's; the weighted average is mixed back by `strength`.
/// Alpha is the current pixel's.
const SHADER_TEMPORAL_DENOISE: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    count: u32,
    _pad0: u32,
    denom: f32,
    strength: f32,
    _pad1: u32,
    _pad2: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> before: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> after: array<vec4<f32>>;
",
    shader_pixel_storage_wgsl!(),
    "

fn td_luma(p: vec3<f32>) -> f32 {
    return params.luma.x * p.x + params.luma.y * p.y + params.luma.z * p.z;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let cur = unpack(input[idx]);
    let cur_luma = td_luma(cur.xyz);
    var sum = cur.xyz;
    var weight = 1.0;
    if params.count > 0u {
        let n = unpack(before[idx]).xyz;
        let d = cur_luma - td_luma(n);
        let w = exp(-(d * d) / params.denom);
        sum = sum + n * w;
        weight = weight + w;
    }
    if params.count > 1u {
        let n = unpack(after[idx]).xyz;
        let d = cur_luma - td_luma(n);
        let w = exp(-(d * d) / params.denom);
        sum = sum + n * w;
        weight = weight + w;
    }
    let averaged = sum * (1.0 / weight);
    let out = cur.xyz + (averaged - cur.xyz) * params.strength;
    output[idx] = pack(vec4<f32>(out, cur.w));
}
"
);

const SHADER_LUMA_KEY: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    low: f32,
    high: f32,
    soft: f32,
    invert: u32,
    into_alpha: u32,
    _pad1: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "

fn soft_range(val: f32, lo: f32, hi: f32, soft: f32) -> f32 {
    if val >= lo && val <= hi { return 1.0; }
    if soft <= 0.0001 { return 0.0; }
    if val < lo {
        return clamp(1.0 - (lo - val) / soft, 0.0, 1.0);
    }
    return clamp(1.0 - (val - hi) / soft, 0.0, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    // Luma of the *encoded* picture, which is the axis the Qualifier's
    // lum window uses (`picture_lum_axis`) and the one the CPU twin
    // measures, since it quantises to u8 first. Keying linear luma here
    // instead meant the same threshold picked a different brightness
    // depending on which path ran: 0.5 is mid-grey as seen, not a bright
    // highlight. Weighting the encoded values rather than the linear
    // ones is the BT.709 luma (Y') definition, and the same order the
    // scopes use, so a threshold here means what the waveform shows.
    //
    // The coefficients come from the working space, not from Rec.709:
    // this picture is in the project's own primaries, so an ACEScg
    // project needs AP1's row (0.272/0.674/0.054) and Rec.709's
    // under-weights green. They arrive via `set_luma_coefs`, which the
    // executor installs from the loaded project.
    // Each channel clamped to [0, 1], as the CPU keys it
    // (`luma_key_value`): unclamped, a highlight above white fell out of
    // a window up to 1.0, the whole range (#1439).
    let e = clamp(enc3(orig.xyz), vec3<f32>(0.0), vec3<f32>(1.0));
    let y = dot(e, params.luma.xyz);

    var matte = soft_range(y, params.low, params.high, params.soft);
    if params.invert != 0u { matte = 1.0 - matte; }

    // The Luma Key clip effect (#1638): the matte multiplies the
    // picture's alpha, its colour untouched, as the CPU twin does.
    if params.into_alpha != 0u {
        output[idx] = pack(vec4<f32>(orig.xyz, orig.w * matte));
        return;
    }

    // A matte is opaque, as the CPU writes it (#1037): the input's alpha
    // made the two disagree over a part-transparent picture.
    output[idx] = pack(vec4<f32>(matte, matte, matte, 1.0));
}
"
);

// ─── Soft Clip GPU Shader ───────────────────────────────────────────────────
//
// Per-channel Reinhard-style highlight rolloff. Mirrors the CPU
// curve in klipp-color/soft_clip.rs::soft_clip_value: passthrough
// below `threshold`, hard clip when `knee == 0`, asymptotic-to-1.0
// rolloff when `knee > 0`. Value transform — alpha is preserved.
const SHADER_SOFT_CLIP: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    threshold: f32,
    knee: f32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
    _pad3: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "

fn soft_clip_value(x: f32, threshold: f32, knee: f32) -> f32 {
    if x <= threshold { return x; }
    if knee <= 0.000001 { return threshold; }
    let max_remaining = 1.0 - threshold;
    if max_remaining <= 0.0 { return min(x, threshold); }
    let excess = x - threshold;
    let rolloff = max_remaining * (1.0 - exp(-excess / knee));
    return min(threshold + rolloff, 1.0);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    let r = soft_clip_value(orig.x, params.threshold, params.knee);
    let g = soft_clip_value(orig.y, params.threshold, params.knee);
    let b = soft_clip_value(orig.z, params.threshold, params.knee);

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
"
);

// ─── Film Grain GPU Shader ──────────────────────────────────────────────────

// Film grain — additive pseudo-random noise, weighted by tonal band.
// Real film grain is photon shot noise, which IS additive in linear
// light; the sRGB version under-represented noise in highlights
// (gamma compresses noise there) and over-represented it in shadows.
// Linear additive matches the physical model.
const SHADER_GRAIN: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    size: f32,
    shadows: f32,
    highlights: f32,
    frame_seed: u32,
    monochrome: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
fn wang_hash(seed: u32) -> f32 {
    var x = seed;
    x = (x ^ 61u) ^ (x >> 16u);
    x = x * 9u;
    x = x ^ (x >> 4u);
    x = x * 0x27d4eb2du;
    x = x ^ (x >> 15u);
    return f32(x) / 4294967295.0;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let p = unpack(input[idx]);
    let luma = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), p.xyz);

    // Pixel coordinates (for grain size variation)
    let px = idx % params.width;
    let py = idx / params.width;
    // Quantize by grain size to create larger particles
    let qx = u32(f32(px) / params.size);
    let qy = u32(f32(py) / params.size);
    let pixel_seed = qx * 1973u + qy * 9277u + params.frame_seed * 26699u;

    // Generate grain noise
    let grain_r = (wang_hash(pixel_seed) - 0.5) * params.amount;
    var grain_g = grain_r;
    var grain_b = grain_r;
    if params.monochrome == 0u {
        grain_g = (wang_hash(pixel_seed + 1u) - 0.5) * params.amount;
        grain_b = (wang_hash(pixel_seed + 2u) - 0.5) * params.amount;
    }

    // Weight grain by tonal region
    let shadow_w = (1.0 - luma) * params.shadows;
    let highlight_w = luma * params.highlights;
    let weight = clamp(shadow_w + highlight_w, 0.0, 1.5);

    output[idx] = pack(vec4<f32>(
        p.x + grain_r * weight,
        p.y + grain_g * weight,
        p.z + grain_b * weight,
        p.w,
    ));
}
"
);

// ─── Alpha Mask GPU Shaders ─────────────────────────────────────────────────

// Alpha-only shaders use the shared buf_a/buf_b storage, but view them
// through a dedicated u32-per-pixel layout (one pixel per u32, no RGBA
// packing). That's fine even though buf_a/b are sized for vec4<f32> —
// the shader just touches the first `w*h` u32 elements.
const SHADER_ALPHA_BLUR_H: &str = "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let x = idx % params.width;
    var sum: u32 = 0u;
    var count: u32 = 0u;
    let r = i32(params.radius);
    for (var dx = -r; dx <= r; dx++) {
        let nx = i32(x) + dx;
        if nx >= 0 && nx < i32(params.width) {
            sum += input[y * params.width + u32(nx)];
            count += 1u;
        }
    }
    output[idx] = sum / count;
}
";

const SHADER_ALPHA_BLUR_V: &str = "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let x = idx % params.width;
    var sum: u32 = 0u;
    var count: u32 = 0u;
    let r = i32(params.radius);
    for (var dy = -r; dy <= r; dy++) {
        let ny = i32(y) + dy;
        if ny >= 0 && ny < i32(params.height) {
            sum += input[u32(ny) * params.width + x];
            count += 1u;
        }
    }
    output[idx] = sum / count;
}
";

const SHADER_ALPHA_DILATE: &str = "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    radius_sq: u32,
}

@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let x = idx % params.width;
    var max_val: u32 = 0u;
    let r = i32(params.radius);
    let r_sq = f32(params.radius_sq);
    for (var dy = -r; dy <= r; dy++) {
        let ny = i32(y) + dy;
        if ny < 0 || ny >= i32(params.height) { continue; }
        for (var dx = -r; dx <= r; dx++) {
            let nx = i32(x) + dx;
            if nx < 0 || nx >= i32(params.width) { continue; }
            let dist_sq = f32(dx * dx + dy * dy);
            if dist_sq <= r_sq {
                let val = input[u32(ny) * params.width + u32(nx)];
                if val > max_val { max_val = val; }
            }
        }
    }
    output[idx] = max_val;
}
";

// ─── Light effect shaders ──────────────────────────────────────────────────────

// Bloom threshold — extract pixels above a luma threshold and
// scale them by `(luma − threshold) / (1 − threshold)`. Linear
// luma (BT.709) matches the coefficients' definition; thresholding
// in linear gives a physically-meaningful "light above N nits".
const SHADER_BLOOM_THRESHOLD: &str = concat!(
    "
struct Params {
    width: u32, height: u32, threshold: f32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = unpack(input[idx]);
    let luma = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), px.xyz);
    if luma > params.threshold {
        // A weight, not a ramp: normalising by 1 - threshold handed a 12x
        // highlight a factor of 34, so the bloom grew the light it spread.
        let factor = clamp((luma - params.threshold) / (1.0 - params.threshold + 1e-6), 0.0, 1.0);
        output[idx] = pack(vec4<f32>(px.xyz * factor, px.w));
    } else {
        output[idx] = pack(vec4<f32>(0.0, 0.0, 0.0, 0.0));
    }
}
"
);

// Bloom blend — additive blend of the bright-extracted buffer onto
// the original. Linear add is physically correct for "combining
// light sources"; the sRGB u8 version clipped at 255 and produced
// the wrong midtone mix for HDR highlights.
const SHADER_BLOOM_BLEND: &str = concat!(
    "
struct Params { width: u32, height: u32, intensity: f32, _pad: u32 }
@group(0) @binding(0) var<storage, read> original: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> bright: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let o = unpack(original[idx]);
    let b = unpack(bright[idx]);
    let mixed = vec4<f32>(o.xyz + b.xyz * params.intensity, o.w);
    output[idx] = pack(mixed);
}
"
);

// Glow blend — Screen blend between original and blurred layer.
// Screen formula 1 − (1−a)(1−b) is algebraically identical in any
// linear [0, 1] space; running it in linear makes the "lighten"
// curve match how real light stacks rather than how sRGB bytes add.
const SHADER_GLOW_BLEND: &str = concat!(
    "
struct Params {
    width: u32, height: u32, intensity: f32, desat: f32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}
@group(0) @binding(0) var<storage, read> original: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> blurred: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let o = unpack(original[idx]);
    let bl = unpack(blurred[idx]).xyz;
    let l = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), bl.xyz);
    let b = (bl + (vec3<f32>(l) - bl) * params.desat) * params.intensity;
    output[idx] = pack(vec4<f32>(screen3_at_ceiling(o.xyz, b), o.w));
}
"
);

// Light rays (god rays) — radial sampling from a virtual light source,
// accumulates bright pixels (above threshold) along each ray, decays
// with distance. Linear luma matches the BT.709 coefficient domain,
// linear summation avoids gamma-darkening along the trail.
const SHADER_LIGHT_RAYS: &str = concat!(
    "
struct Params {
    width: u32, height: u32,
    center_x: f32, center_y: f32,
    threshold: f32, samples: u32,
    decay: f32, intensity: f32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    let cx = params.center_x * f32(params.width);
    let cy = params.center_y * f32(params.height);
    let dx = x - cx;
    let dy = y - cy;
    let step = params.decay / f32(params.samples);

    var ray = vec3<f32>(0.0);
    var weight: f32 = 1.0;
    let decay_per_sample = 1.0 - step;

    for (var s: u32 = 0u; s < params.samples; s = s + 1u) {
        let t = f32(s) * step;
        let sx = clamp(u32(x - dx * t), 0u, params.width - 1u);
        let sy = clamp(u32(y - dy * t), 0u, params.height - 1u);
        let sample = unpack(input[sy * params.width + sx]);
        let luma = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), sample.xyz);
        if luma > params.threshold {
            ray = ray + sample.xyz * weight;
        }
        weight *= decay_per_sample;
    }

    let inv_samples = 1.0 / f32(params.samples);
    let orig = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(orig.xyz + ray * inv_samples * params.intensity, orig.w));
}
"
);

// Vignette — multiplicative darkening of the frame's edges. The
// `factor` scalar applies in linear-light so `amount = -1.0` really
// means "half the photons reach the edge" rather than "half the
// sRGB byte value" (which the u8 version did — visibly darker).
// The generator effect (#1746), as `generator::Gen::at` computes it.
// The native living gradient's CPU half-float path, including its
// half-float roundings and its grain (#2073), followed by sRGB decode and
// working-space conversion.
const SHADER_LIVING_GRADIENT: &str = concat!(shader_pixel_storage_wgsl!(), shader_generator_grain_wgsl!(), "
struct Params {
    width: u32, height: u32, glow: f32, grain: f32,
    a: vec4<f32>, b: vec4<f32>, dir: vec2<f32>, centre: vec2<f32>,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
fn rounded(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(unpack2x16float(pack2x16float(c.xy)), unpack2x16float(pack2x16float(c.zw)));
}
fn linear(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.width * params.height { return; }
    let wh = vec2<f32>(f32(params.width), f32(params.height));
    let xy = vec2<f32>(f32(i % params.width), f32(i / params.width));
    let span = max(wh - vec2<f32>(1.0), vec2<f32>(0.0));
    let half_extent = dot(abs(params.dir), span * 0.5);
    let t = clamp((dot(xy - span * 0.5, params.dir) + half_extent) / max(2.0 * half_extent, 1.0), 0.0, 1.0);
    var c = rounded((params.a + (params.b - params.a) * t) / 255.0);
    if params.glow > 0.0 {
        let d = xy - params.centre;
        let radius = max(wh.x, wh.y) * 0.45;
        let f = exp(-dot(d, d) / (2.0 * radius * radius)) * params.glow;
        if f >= 0.002 {
            c = rounded(vec4<f32>(min(c.rgb + (vec3<f32>(1.0) - c.rgb) * f * 0.6, vec3<f32>(1.0)), c.a));
        }
    }
    if params.grain > 0.0 {
        let n = grain_noise(i);
        c = rounded(vec4<f32>(grain_channel(c.r, n, params.grain), grain_channel(c.g, n, params.grain), grain_channel(c.b, n, params.grain), c.a));
    }
    let rgb = vec3<f32>(linear(c.r), linear(c.g), linear(c.b));
    output[i] = pack(vec4<f32>(dot(params.m0.xyz, rgb), dot(params.m1.xyz, rgb), dot(params.m2.xyz, rgb), c.a));
}
");

// The native Grain generator (#2067), as `klipp_core::render_grain`
// draws it: a colour, and each cell's hash, `depth` deep, added and
// truncated to a byte. Then the eight-bit path's decode: the byte as a
// half float, sRGB to linear, into the working space.
const SHADER_GRAIN_GENERATOR: &str = concat!(shader_pixel_storage_wgsl!(), "
struct Params {
    width: u32, height: u32, frame: u32, _pad: u32,
    color: vec4<f32>,
    cell: f32, depth: f32, _p0: f32, _p1: f32,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
fn rounded(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(unpack2x16float(pack2x16float(c.xy)), unpack2x16float(pack2x16float(c.zw)));
}
fn linear(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
fn grain_at(x: u32, y: u32, frame: u32) -> f32 {
    var h = (x * 0x8da6b343u) ^ (y * 0xd8163841u) ^ (frame * 0xcb1ab31fu);
    h ^= h >> 15u;
    h = h * 0x2c1b3c6du;
    h ^= h >> 12u;
    h = h * 0x297a2d39u;
    h ^= h >> 15u;
    // `u32::MAX as f32`, as the CPU divides by.
    return (f32(h) / 4294967296.0) * 2.0 - 1.0;
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.width * params.height { return; }
    let gx = u32(f32(i % params.width) / params.cell);
    let gy = u32(f32(i / params.width) / params.cell);
    let n = grain_at(gx, gy, params.frame) * params.depth;
    let byte = floor(clamp(params.color.rgb + vec3<f32>(n), vec3<f32>(0.0), vec3<f32>(255.0)));
    let c = rounded(vec4<f32>(byte, params.color.a) / 255.0);
    let rgb = vec3<f32>(linear(c.r), linear(c.g), linear(c.b));
    output[i] = pack(vec4<f32>(dot(params.m0.xyz, rgb), dot(params.m1.xyz, rgb), dot(params.m2.xyz, rgb), c.a));
}
");

// Flat rects over a background (#2067): a countdown's or a timer's
// seven-segment digits, laid out on the CPU by `klipp_core::Segments`.
const SHADER_SEGMENTS: &str = concat!(shader_pixel_storage_wgsl!(), "
struct Params {
    width: u32, height: u32, count: u32, _pad: u32,
    background: vec4<f32>, color: vec4<f32>,
    m0: vec4<f32>, m1: vec4<f32>, m2: vec4<f32>,
}
@group(0) @binding(0) var<storage, read> rects: array<vec4<u32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
fn rounded(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(unpack2x16float(pack2x16float(c.xy)), unpack2x16float(pack2x16float(c.zw)));
}
fn linear(c: f32) -> f32 {
    if c <= 0.04045 { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
    if i >= params.width * params.height { return; }
    let x = i % params.width;
    let y = i / params.width;
    var c = params.background;
    for (var k = 0u; k < params.count; k++) {
        let r = rects[k];
        if x >= r.x && x < r.z && y >= r.y && y < r.w { c = params.color; break; }
    }
    let s = rounded(c / 255.0);
    let rgb = vec3<f32>(linear(s.r), linear(s.g), linear(s.b));
    output[i] = pack(vec4<f32>(dot(params.m0.xyz, rgb), dot(params.m1.xyz, rgb), dot(params.m2.xyz, rgb), s.a));
}
");

const SHADER_GENERATOR: &str = concat!(
    "
struct Params {
    width: u32, height: u32, shape: u32, _pad: u32,
    a: vec4<f32>, b: vec4<f32>,
    dir: vec2<f32>, centre: vec2<f32>,
    radius: f32, _p0: u32, _p1: u32, _p2: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let d = vec2<f32>(f32(idx % params.width) + 0.5, f32(idx / params.width) + 0.5) - params.centre;
    var t = 0.0;
    if params.shape == 1u {
        t = 0.5 + dot(d, params.dir) / (2.0 * params.radius);
    } else if params.shape == 2u {
        t = length(d) / params.radius;
    }
    t = clamp(t, 0.0, 1.0);
    output[idx] = pack(params.a + (params.b - params.a) * t);
}
"
);

const SHADER_VIGNETTE: &str = concat!(
    "
struct Params { width: u32, height: u32, amount: f32, size: f32, softness: f32, roundness: f32, _pad: u32, _pad2: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    let cx = f32(params.width) * 0.5;
    let cy = f32(params.height) * 0.5;
    let nx = (x - cx) / cx;
    let ny = (y - cy) / cy;
    let rect_dist = max(abs(nx), abs(ny));
    let circ_dist = sqrt(nx * nx + ny * ny);
    let dist = rect_dist * (1.0 - params.roundness) + circ_dist * params.roundness;
    let falloff = clamp((dist - params.size) / max(params.softness, 0.01), 0.0, 1.0);
    let factor = 1.0 + params.amount * falloff;
    let px = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(px.xyz * factor, px.w));
}
"
);

// Lens flare — synthetic additive disc glow + chromatic-abb'd
// ghost discs along the flare-to-centre axis. Adding synthetic
// light to linear RGB is physically correct; the u8 version added
// gamma-encoded bytes, which visibly under-weighted the glow
// (flares looked thin and under-saturated).
const SHADER_LENS_FLARE: &str = concat!(
    "
struct Params {
    width: u32, height: u32, pos_x: f32, pos_y: f32,
    brightness: f32, flare_size: f32, ghost_count: u32, ghost_intensity: f32,
    chromatic_aberration: f32, _pad: u32, _pad2: u32, _pad3: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = f32(idx % params.width);
    let py = f32(idx / params.width);
    let w = f32(params.width);
    let h = f32(params.height);
    let fx = params.pos_x * w;
    let fy = params.pos_y * h;
    let cx = w * 0.5;
    let cy = h * 0.5;
    let max_dim = max(w, h);
    let disc_r = params.flare_size * max_dim;
    var add = vec3<f32>(0.0);
    let dist = sqrt((px - fx) * (px - fx) + (py - fy) * (py - fy));
    if dist < disc_r {
        let falloff = 1.0 - dist / disc_r;
        let glow = falloff * falloff * params.brightness;
        add = add + vec3<f32>(glow, glow * 0.9, glow * 0.7);
    }
    for (var gi: u32 = 0u; gi < params.ghost_count; gi = gi + 1u) {
        let t = (f32(gi) + 1.0) / (f32(params.ghost_count) + 1.0);
        let gx = fx + (cx - fx) * t * 2.0;
        let gy = fy + (cy - fy) * t * 2.0;
        let gr = disc_r * (0.3 + t * 0.4);
        let gdist = sqrt((px - gx) * (px - gx) + (py - gy) * (py - gy));
        if gdist < gr {
            let gf = 1.0 - gdist / gr;
            let glow = gf * gf * params.ghost_intensity * params.brightness;
            let ca = params.chromatic_aberration * t;
            add = add + vec3<f32>(glow * (1.0 + ca), glow, glow * (1.0 - ca * 0.5));
        }
    }
    let orig = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(orig.xyz + add, orig.w));
}
"
);

// Anamorphic streak — horizontal threshold-accumulate-and-tint on
// bright pixels. Linear summation of light sources, then additive
// blend onto the original. Both benefit from linear: bright streaks
// don't dim-out through gamma averaging, and additive compositing
// is physically what anamorphic lenses produce.
const SHADER_ANAMORPHIC_STREAK: &str = concat!(
    "
struct Params {
    width: u32, height: u32, threshold: f32, half_len: u32,
    intensity: f32, tint: f32, _pad: u32, _pad2: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let tint_r = 1.0 + (1.0 - params.tint) * 0.3;
    let tint_b = 1.0 + params.tint * 0.5;
    var streak = vec3<f32>(0.0);
    let x_start = max(i32(x) - i32(params.half_len), 0);
    let x_end = min(x + params.half_len + 1u, params.width);
    let count = f32(x_end - u32(x_start));
    for (var sx: u32 = u32(x_start); sx < x_end; sx = sx + 1u) {
        let px = unpack(input[y * params.width + sx]);
        let luma = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), px.xyz);
        if luma > params.threshold {
            let factor = clamp((luma - params.threshold) / (1.0 - params.threshold + 1e-6), 0.0, 1.0);
            streak = streak + px.xyz * factor;
        }
    }
    streak = streak / count;
    let orig = unpack(input[idx]);
    let out = vec3<f32>(
        orig.x + streak.x * tint_r * params.intensity,
        orig.y + streak.y * params.intensity,
        orig.z + streak.z * tint_b * params.intensity,
    );
    output[idx] = pack(vec4<f32>(out, orig.w));
}
"
);

// Film halation — Screen-blend the pre-blurred highlights onto the
// original, in the halation's colour (deep red to warm yellow, from
// `hue`) whatever colour the highlight was.
// Screen blend in linear matches how light spreads on actual film
// emulsion — additive glow that asymptotes at diffuse white.
const SHADER_FILM_HALATION: &str = concat!(
    "
struct Params {
    width: u32, height: u32, threshold: f32, intensity: f32,
    hal_r: f32, hal_g: f32, hal_b: f32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}
@group(0) @binding(0) var<storage, read> original: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> halation: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let o = unpack(original[idx]);
    let bl = unpack(halation[idx]).xyz;
    let l = dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), bl.xyz);
    let h = vec3<f32>(params.hal_r, params.hal_g, params.hal_b) * l * params.intensity;
    output[idx] = pack(vec4<f32>(screen3_at_ceiling(o.xyz, h), o.w));
}
"
);

// LUT3D is defined in sRGB 0-1 space (each LUT entry maps an sRGB
// triplet to an sRGB triplet). Storage is native linear-f32, so we
// encode pixel → sRGB, sample the LUT, then decode the result back to
// linear. The opacity blend is done in linear space.
//
// A LUT tagged with other primaries than the working space's is
// bracketed: `pre` takes the pixel into the LUT's input primaries
// before the encode, `post` takes the decoded result from its output
// primaries back, as `klipp_effects::apply_3d_lut_bracketed` does on
// the CPU (#422). `bracket` bit 0 turns `pre` on, bit 1 `post`.
const SHADER_LUT3D: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    lut_size: u32,
    opacity: f32,
    tetrahedral: u32,
    bracket: u32,
    _pad0: u32,
    _pad1: u32,
    pre0: vec4<f32>,
    pre1: vec4<f32>,
    pre2: vec4<f32>,
    post0: vec4<f32>,
    post1: vec4<f32>,
    post2: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;

fn lut_sample(ri: u32, gi: u32, bi: u32) -> vec3<f32> {
    let n = params.lut_size;
    let idx = (bi * n * n + gi * n + ri) * 3u;
    return vec3<f32>(lut[idx], lut[idx + 1u], lut[idx + 2u]);
}

fn linear_to_srgb_component(c: f32) -> f32 {
    if (c <= 0.0031308) { return c * 12.92; }
    else { return 1.055 * pow(c, 1.0/2.4) - 0.055; }
}

fn srgb_to_linear_component(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    else { return pow((c + 0.055) / 1.055, 2.4); }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let pixel_idx = gid.x;
    if pixel_idx >= params.width * params.height { return; }

    let pixel = unpack(input[pixel_idx]);
    let orig = vec3<f32>(pixel.x, pixel.y, pixel.z);
    var lut_in = orig;
    if (params.bracket & 1u) != 0u {
        lut_in = vec3<f32>(dot(params.pre0.xyz, orig), dot(params.pre1.xyz, orig), dot(params.pre2.xyz, orig));
    }
    let r_in = clamp(linear_to_srgb_component(lut_in.x), 0.0, 1.0);
    let g_in = clamp(linear_to_srgb_component(lut_in.y), 0.0, 1.0);
    let b_in = clamp(linear_to_srgb_component(lut_in.z), 0.0, 1.0);

    let n1 = f32(params.lut_size - 1u);

    let r_idx = clamp(r_in * n1, 0.0, n1);
    let g_idx = clamp(g_in * n1, 0.0, n1);
    let b_idx = clamp(b_in * n1, 0.0, n1);

    let r0 = min(u32(r_idx), params.lut_size - 2u);
    let g0 = min(u32(g_idx), params.lut_size - 2u);
    let b0 = min(u32(b_idx), params.lut_size - 2u);
    let r1 = r0 + 1u;
    let g1 = g0 + 1u;
    let b1 = b0 + 1u;

    let rf = r_idx - f32(r0);
    let gf = g_idx - f32(g0);
    let bf = b_idx - f32(b0);

    let c000 = lut_sample(r0, g0, b0);
    let c100 = lut_sample(r1, g0, b0);
    let c010 = lut_sample(r0, g1, b0);
    let c110 = lut_sample(r1, g1, b0);
    let c001 = lut_sample(r0, g0, b1);
    let c101 = lut_sample(r1, g0, b1);
    let c011 = lut_sample(r0, g1, b1);
    let c111 = lut_sample(r1, g1, b1);

    var lut_out: vec3<f32>;
    if params.tetrahedral != 0u {
        // Tetrahedral: the unit cube split into six tetrahedra by the
        // order of the fractional coordinates, so the sample is
        // interpolated over the four corners that surround it rather
        // than all eight. It holds a LUT's straight lines straight,
        // where trilinear bows them — the wobble that shows on a skin
        // tone or in a sky gradient.
        if rf > gf {
            if gf > bf {
                lut_out = c000 + (c100 - c000) * rf + (c110 - c100) * gf + (c111 - c110) * bf;
            } else if rf > bf {
                lut_out = c000 + (c100 - c000) * rf + (c101 - c100) * bf + (c111 - c101) * gf;
            } else {
                lut_out = c000 + (c001 - c000) * bf + (c101 - c001) * rf + (c111 - c101) * gf;
            }
        } else if rf > bf {
            lut_out = c000 + (c010 - c000) * gf + (c110 - c010) * rf + (c111 - c110) * bf;
        } else if gf > bf {
            lut_out = c000 + (c010 - c000) * gf + (c011 - c010) * bf + (c111 - c011) * rf;
        } else {
            lut_out = c000 + (c001 - c000) * bf + (c011 - c001) * gf + (c111 - c011) * rf;
        }
    } else {
        let c00 = mix(c000, c100, rf);
        let c01 = mix(c001, c101, rf);
        let c10 = mix(c010, c110, rf);
        let c11 = mix(c011, c111, rf);
        let c0 = mix(c00, c10, gf);
        let c1 = mix(c01, c11, gf);
        lut_out = mix(c0, c1, bf);
    }

    // Decode LUT output (sRGB) back to linear, and into the working
    // space's primaries.
    var graded = vec3<f32>(
        srgb_to_linear_component(clamp(lut_out.x, 0.0, 1.0)),
        srgb_to_linear_component(clamp(lut_out.y, 0.0, 1.0)),
        srgb_to_linear_component(clamp(lut_out.z, 0.0, 1.0)),
    );
    if (params.bracket & 2u) != 0u {
        graded = vec3<f32>(dot(params.post0.xyz, graded), dot(params.post1.xyz, graded), dot(params.post2.xyz, graded));
    }

    // Opacity blend in linear.
    let result = mix(orig, graded, params.opacity);

    output[pixel_idx] = pack(vec4<f32>(result.x, result.y, result.z, pixel.w));
}
"
);

// ─── New effect shaders ─────────────────────────────────────────────────────

// Invert — 1 − x on the display-encoded picture: the photo negative
// editors mean by invert. (Inverting linear light turned 18% grey into
// a near-white, not a negative.)
const SHADER_INVERT: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    mix_val: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let m = params.mix_val;
    let e = enc3(p.xyz);
    // The inverse of a highlight is black, not a negative: invert what
    // the display can show. `e` stays the mix's base, so mix 0 is identity.
    let inv = vec3<f32>(1.0) - clamp(e, vec3<f32>(0.0), vec3<f32>(1.0));
    let out = e + (inv - e) * m;
    output[idx] = pack(vec4<f32>(dec3(out), p.w));
}
"
);

// Posterize — quantise to N levels, even on the display scale (in linear
// light the shadows collapsed into one band). White stays white.
const SHADER_POSTERIZE: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    levels: f32,
    above_count: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
// The ladder's band edges as linear values, ascending, padded with the
// largest float (`PosterizeLadder`): 256 below white, then 256 above it.
@group(0) @binding(3) var<storage, read> edges: array<f32>;
",
    shader_linear_helpers_wgsl!(),
    "
// How many of the 256 edges from `base` are at or below `v`, as the CPU's
// `PosterizeLadder::count` counts them.
fn count_at_or_below(base: u32, v: f32) -> u32 {
    var lo = 0u;
    var hi = 256u;
    loop {
        if lo >= hi { break; }
        let mid = (lo + hi) / 2u;
        if edges[base + mid] <= v { lo = mid + 1u; } else { hi = mid; }
    }
    return lo;
}

// The band `v` falls in, found against the edges the CPU finds it against
// (bit for bit the same band on both routes, #1443), then its value.
// Bands are even on the display scale; above white the ladder carries on
// at the same spacing, instead of every highlight landing on white.
fn band(v: f32) -> f32 {
    let div = params.levels - 1.0;
    let m = f32(count_at_or_below(0u, v));
    var j = count_at_or_below(256u, v);
    if j == params.above_count && params.above_count == 256u {
        // Past the table: the step from the encoding, never below it.
        j = max(j, u32(max(floor((linear_to_srgb_component(v) - 1.0) * params.levels), 0.0)));
    }
    return srgb_to_linear_component(min(m, div) / div + f32(j) / div);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(band(p.x), band(p.y), band(p.z), p.w));
}
"
);

// The colour-space transform effect (#1746), as
// `colour_transform::cst_pixel` computes it: the input curve's table is
// the slot table's first 256 floats, the output curve's the next 256.
const SHADER_CST: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    flags: u32,
    _pad: u32,
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> tables: array<f32>;
",
    shader_linear_helpers_wgsl!(),
    "
// The table from `base`'s linear value at `code`, carried on past the
// ends (`curve_decode`).
fn curve_decode(base: u32, code: f32) -> f32 {
    let x = code * 255.0;
    let i = min(u32(max(floor(x), 0.0)), 254u);
    let f = x - f32(i);
    let a = tables[base + i];
    return a + (tables[base + i + 1u] - a) * f;
}

// Its inverse, the segment found by halving (`curve_encode`).
fn curve_encode(base: u32, v: f32) -> f32 {
    var lo = 0u;
    var hi = 255u;
    loop {
        if lo + 1u >= hi { break; }
        let mid = (lo + hi) / 2u;
        if tables[base + mid] <= v { lo = mid; } else { hi = mid; }
    }
    let a = tables[base + lo];
    let b = tables[base + lo + 1u];
    var f = 0.0;
    if b != a { f = (v - a) / (b - a); }
    return (f32(lo) + f) / 255.0;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    var c = p.xyz;
    if (params.flags & 1u) != 0u {
        c = vec3<f32>(
            curve_decode(0u, linear_to_srgb_component(c.x)),
            curve_decode(0u, linear_to_srgb_component(c.y)),
            curve_decode(0u, linear_to_srgb_component(c.z)),
        );
    }
    if (params.flags & 4u) != 0u {
        c = vec3<f32>(dot(params.m0.xyz, c), dot(params.m1.xyz, c), dot(params.m2.xyz, c));
    }
    if (params.flags & 2u) != 0u {
        c = vec3<f32>(
            srgb_to_linear_component(curve_encode(256u, c.x)),
            srgb_to_linear_component(curve_encode(256u, c.y)),
            srgb_to_linear_component(curve_encode(256u, c.z)),
        );
    }
    output[idx] = pack(vec4<f32>(c, p.w));
}
"
);

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
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
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

// Edge detect — Sobel filter on luma. BT.709 coefficients are the
// standard for linear-light luma; using them on sRGB-encoded bytes
// (as the old version did) misweighed channels. Linear Sobel
// produces slightly different edge magnitudes that are more
// consistent with the physical gradient.
const SHADER_EDGE_DETECT: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    mix_val: f32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad_luma: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
fn get_lum(x: i32, y: i32) -> f32 {
    let cx = clamp(x, 0, i32(params.width) - 1);
    let cy = clamp(y, 0, i32(params.height) - 1);
    let p = unpack(input[u32(cy) * params.width + u32(cx)]);
    return dot(vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb), p.xyz);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);

    // Sobel Gx
    let gx = -1.0 * get_lum(x - 1, y - 1) + 1.0 * get_lum(x + 1, y - 1)
           + -2.0 * get_lum(x - 1, y)     + 2.0 * get_lum(x + 1, y)
           + -1.0 * get_lum(x - 1, y + 1) + 1.0 * get_lum(x + 1, y + 1);
    // Sobel Gy
    let gy = -1.0 * get_lum(x - 1, y - 1) + -2.0 * get_lum(x, y - 1) + -1.0 * get_lum(x + 1, y - 1)
           +  1.0 * get_lum(x - 1, y + 1) +  2.0 * get_lum(x, y + 1) +  1.0 * get_lum(x + 1, y + 1);

    let edge = clamp(sqrt(gx * gx + gy * gy) * params.strength, 0.0, 1.0);

    let orig = unpack(input[idx]);
    let m = params.mix_val;
    let mixed = orig.xyz * (1.0 - m) + vec3<f32>(edge) * m;
    output[idx] = pack(vec4<f32>(mixed, orig.w));
}
"
);

// Channel mixer — 3x3 linear combination of RGB channels. Linear
// mixing corresponds to real light-combining operations (e.g.
// "synthesise luma from 0.7 R + 0.2 G + 0.1 B"). The sRGB version
// mixed gamma-encoded bytes, which silently under- or over-weighted
// depending on channel intensity.
const SHADER_CHANNEL_MIXER: &str = concat!(
    "
struct Params {
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

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let out = vec3<f32>(
        params.rr * p.x + params.rg * p.y + params.rb * p.z,
        params.gr * p.x + params.gg * p.y + params.gb * p.z,
        params.br * p.x + params.bg * p.y + params.bb * p.z,
    );
    output[idx] = pack(vec4<f32>(out, p.w));
}
"
);

// Directional blur — averages along a `(dx, dy)` line. Linear-light
// averaging so midtones don't darken through the trail, and premultiplied
// so a picture over transparency smears at its coverage (#2021): the
// colour is the alpha-weighted mean, back to straight by the summed alpha
// (`UNPREMULTIPLY_MIN_ALPHA`, as the shutter's mean, #2017). Every tap
// opaque, the plain mean as before, to the bit.
const SHADER_DIRECTIONAL_BLUR: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    samples: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = f32(idx % params.width);
    let y = f32(idx / params.width);
    var sum = vec4<f32>(0.0);
    var pre = vec3<f32>(0.0);
    let w = i32(params.width);
    let h = i32(params.height);
    for (var i = 0u; i < params.samples; i = i + 1u) {
        let sx = clamp(i32(round(x + params.dx * f32(i))), 0, w - 1);
        let sy = clamp(i32(round(y + params.dy * f32(i))), 0, h - 1);
        let si = u32(sy) * params.width + u32(sx);
        let q = unpack(input[si]);
        sum += q;
        pre += q.xyz * q.w;
    }
    let inv = 1.0 / f32(params.samples);
    if sum.w == f32(params.samples) {
        output[idx] = pack(sum * inv);
        return;
    }
    var colour = vec3<f32>(0.0);
    if sum.w > ", unpremultiply_min_alpha_wgsl!(), " { colour = pre / sum.w; }
    output[idx] = pack(vec4<f32>(colour, sum.w * inv));
}
"
);

// Emboss — relief effect via directional difference. Same math as
// the sRGB version but pivoted at linear 0.18 (perceptual mid-grey)
// and with all differencing in linear. Result is visually close to
// the old version (the 0.5 sRGB vs 0.18 linear pivot differ by ≈
// one code value on neutral output).
const SHADER_EMBOSS: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    dx: f32,
    dy: f32,
    strength: f32,
    mix: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }

    let x = idx % w;
    let y = idx / w;
    let orig = unpack(input[idx]);

    // Skip border pixels
    if x == 0u || y == 0u || x >= w - 1u || y >= h - 1u {
        output[idx] = input[idx];
        return;
    }

    // Sample in light direction and opposite direction
    let sx1 = clamp(i32(round(f32(x) + params.dx)), 0, i32(w) - 1);
    let sy1 = clamp(i32(round(f32(y) + params.dy)), 0, i32(h) - 1);
    let sx2 = clamp(i32(round(f32(x) - params.dx)), 0, i32(w) - 1);
    let sy2 = clamp(i32(round(f32(y) - params.dy)), 0, i32(h) - 1);

    let p1 = unpack(input[u32(sy1) * w + u32(sx1)]);
    let p2 = unpack(input[u32(sy2) * w + u32(sx2)]);

    // Emboss: difference scaled around 0.18 (perceptual mid-grey
    // anchor in linear). sRGB version pivoted at 0.5 sRGB (~0.21
    // linear) — close enough that the slider feel stays familiar.
    let emboss = vec3<f32>(0.18) + (p1.xyz - p2.xyz) * params.strength;

    // Mix with original in linear light.
    let mixed = orig.xyz + (emboss - orig.xyz) * params.mix;
    output[idx] = pack(vec4<f32>(mixed, orig.w));
}
"
);

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
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
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
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
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

const SHADER_DILATE: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);
    let r = i32(params.radius);
    // From the centre, not from black: a channel darker than 0 (a colour
    // outside the working gamut) is a legal value to spread.
    var max_px = unpack(input[u32(y) * w + u32(x)]);
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            max_px = max(max_px, unpack(input[sy * w + sx]));
        }
    }
    output[idx] = pack(max_px);
}
"
);

const SHADER_ERODE: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);
    let r = i32(params.radius);
    // From the centre, not from white: starting at 1.0 made every pixel
    // of a picture brighter than white come back as white.
    var min_px = unpack(input[u32(y) * w + u32(x)]);
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            min_px = min(min_px, unpack(input[sy * w + sx]));
        }
    }
    output[idx] = pack(min_px);
}
"
);

// Denoise — bilateral filter in linear light that smooths luma `Y`
// and chroma `rgb − Y` separately, each with its own range sigma on
// the luma difference: Luma slider and Chroma slider (#1039).
// `cpu_grade::bilateral_denoise` is the CPU twin and documents it.
const SHADER_DENOISE: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    spatial_sigma: f32,
    luma_sigma: f32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, max_kernel: u32,
    chroma_sigma: f32,
    _pad0: u32, _pad1: u32, _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);

    let center = unpack(input[idx]);
    let coefs = vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb);
    let center_luma = dot(coefs, center.xyz);

    let kernel_size = min(i32(ceil(params.spatial_sigma * 2.0)), i32(params.max_kernel));
    let spatial_denom = 2.0 * params.spatial_sigma * params.spatial_sigma;
    // A sigma of 0 leaves that part as it is (#1039).
    let luma_on = params.luma_sigma > 0.0;
    let chroma_on = params.chroma_sigma > 0.0;
    let luma_denom = 2.0 * params.luma_sigma * params.luma_sigma;
    let chroma_denom = 2.0 * params.chroma_sigma * params.chroma_sigma;

    var sum_y = 0.0;
    var total_y = 0.0;
    var sum_c = vec3<f32>(0.0);
    var total_c = 0.0;

    if luma_on || chroma_on {
        for (var dy = -kernel_size; dy <= kernel_size; dy = dy + 1) {
            for (var dx = -kernel_size; dx <= kernel_size; dx = dx + 1) {
                let sx = u32(clamp(x + dx, 0, i32(w) - 1));
                let sy = u32(clamp(y + dy, 0, i32(h) - 1));
                let n = unpack(input[sy * w + sx]);
                let n_luma = dot(coefs, n.xyz);

                let spatial_dist = f32(dx * dx + dy * dy);
                let spatial_w = exp(-spatial_dist / spatial_denom);
                let luma_diff = center_luma - n_luma;
                let d2 = luma_diff * luma_diff;
                if luma_on {
                    let wl = spatial_w * exp(-d2 / luma_denom);
                    sum_y += n_luma * wl;
                    total_y += wl;
                }
                if chroma_on {
                    let wc = spatial_w * exp(-d2 / chroma_denom);
                    sum_c = sum_c + (n.xyz - vec3<f32>(n_luma)) * wc;
                    total_c += wc;
                }
            }
        }
    }

    var out_y = center_luma;
    if luma_on { out_y = sum_y * (1.0 / total_y); }
    var out_c = center.xyz - vec3<f32>(center_luma);
    if chroma_on { out_c = sum_c * (1.0 / total_c); }
    output[idx] = pack(vec4<f32>(vec3<f32>(out_y) + out_c, center.w));
}
"
);

// Gamma — power curve on the display-encoded picture, as in other
// editors.
const SHADER_GAMMA: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    inv_gamma: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let e = enc3(p.xyz);
    let out = vec3<f32>(
        pow(max(e.x, 0.0), params.inv_gamma),
        pow(max(e.y, 0.0), params.inv_gamma),
        pow(max(e.z, 0.0), params.inv_gamma),
    );
    output[idx] = pack(vec4<f32>(dec3(out), p.w));
}
"
);

// HSV adjust — hue shift + saturation + value multiplier, on the
// display-encoded picture: HSV is a perceptual model, and in linear
// light its saturation and value misread every midtone.
const SHADER_HSV_ADJUST: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    hue_shift: f32,
    saturation: f32,
    value_mult: f32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let e = enc3(p.xyz);
    let r = e.x;
    let g = e.y;
    let b = e.z;

    // RGB to HSV
    let cmax = max(r, max(g, b));
    let cmin = min(r, min(g, b));
    let delta = cmax - cmin;

    var h = 0.0;
    if delta > 0.001 {
        if abs(cmax - r) < 0.001 {
            h = ((g - b) / delta) % 6.0;
        } else if abs(cmax - g) < 0.001 {
            h = (b - r) / delta + 2.0;
        } else {
            h = (r - g) / delta + 4.0;
        }
        h = h * 60.0;
        if h < 0.0 { h += 360.0; }
    }
    let s = select(0.0, delta / cmax, cmax > 0.001);
    let val = cmax;

    // Apply adjustments
    h = (h + params.hue_shift + 360.0) % 360.0;
    let new_s = clamp(s * params.saturation, 0.0, 1.0);
    let new_v = val * params.value_mult;

    // HSV to RGB
    let c = new_v * new_s;
    let x = c * (1.0 - abs((h / 60.0) % 2.0 - 1.0));
    let m = new_v - c;

    var rr = 0.0;
    var gg = 0.0;
    var bb = 0.0;
    if h < 60.0 { rr = c; gg = x; bb = 0.0; }
    else if h < 120.0 { rr = x; gg = c; bb = 0.0; }
    else if h < 180.0 { rr = 0.0; gg = c; bb = x; }
    else if h < 240.0 { rr = 0.0; gg = x; bb = c; }
    else if h < 300.0 { rr = x; gg = 0.0; bb = c; }
    else { rr = c; gg = 0.0; bb = x; }

    output[idx] = pack(vec4<f32>(dec3(vec3<f32>(rr + m, gg + m, bb + m)), p.w));
}
"
);

const SHADER_NOISE_GENERATOR: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    amount: f32,
    monochrome: u32,
    seed: u32,
    _pad: u32,
    _pad2: u32,
    _pad3: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn hash(p: u32) -> f32 {
    var x = p;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = (x >> 16u) ^ x;
    return f32(x) / 4294967295.0;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let v = unpack(input[idx]);
    var r = v.x;
    var g = v.y;
    var b = v.z;

    let noise_seed = idx * 7919u + params.seed;
    if params.monochrome != 0u {
        let n = (hash(noise_seed) - 0.5) * 2.0 * params.amount;
        r = r + n;
        g = g + n;
        b = b + n;
    } else {
        r = r + (hash(noise_seed) - 0.5) * 2.0 * params.amount;
        g = g + (hash(noise_seed + 1u) - 0.5) * 2.0 * params.amount;
        b = b + (hash(noise_seed + 2u) - 0.5) * 2.0 * params.amount;
    }

    output[idx] = pack(vec4<f32>(r, g, b, v.w));
}
"
);

// Film grain — emulsion-like grain in cells of `size` px at 1080p,
// weighted away from the highlights, a new pattern per `seed`. The CPU
// twin uses the same hash and cell arithmetic.
const SHADER_FILM_GRAIN: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, amount: f32, per_px: f32,
    color: u32, shadows: f32, seed: u32, luma_yr: f32,
    luma_yg: f32, luma_yb: f32, _pad: u32, _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn hash(p: u32) -> f32 {
    var x = p;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = (x >> 16u) ^ x;
    return f32(x) / 4294967295.0;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let v = unpack(input[idx]);
    // A product, as the CPU works it (`grain_cells_per_px`): a division
    // here is good to 2.5 ULP, and an edge then fell in the next cell on
    // this route alone (#1441).
    let gx = u32(f32(x) * params.per_px);
    let gy = u32(f32(y) * params.per_px);
    let base = gx * 7919u + gy * 104729u + params.seed * 15485863u;
    let l = clamp(dot(v.xyz, vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb)), 0.0, 1.0);
    let depth = params.amount * 0.35 * (1.0 - params.shadows * l);
    let n0 = (hash(base) - 0.5) * 2.0 * depth;
    var r = v.x + n0;
    var g = v.y + n0;
    var b = v.z + n0;
    if params.color != 0u {
        g = v.y + (hash(base + 1u) - 0.5) * 2.0 * depth;
        b = v.z + (hash(base + 2u) - 0.5) * 2.0 * depth;
    }
    output[idx] = pack(vec4<f32>(r, g, b, v.w));
}
"
);

// Tilt-shift blend — the original inside the sharp band, the blurred
// frame beyond it, over a smoothstep ramp of `softness`.
// Focus blur blend (#592): the blurred layer mixed over the original
// by how far the pixel is from the band around the frame's middle,
// every channel, alpha too. `cpu::focus_blur_mix` is the CPU twin.
const SHADER_FOCUS_BLUR_BLEND: &str = concat!(
    "
struct Params { width: u32, height: u32, vertical: u32, band: f32 }
@group(0) @binding(0) var<storage, read> original: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> blurred: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    var u = (f32(idx % params.width) + 0.5) / f32(params.width);
    if params.vertical != 0u { u = (f32(idx / params.width) + 0.5) / f32(params.height); }
    let d = abs(u - 0.5) * 2.0;
    var m = clamp((d - params.band) / max(1.0 - params.band, 0.001), 0.0, 1.0);
    m = m * m * (3.0 - 2.0 * m);
    // Mixed premultiplied, as the blurred copy is, then back to straight.
    let o = unpack(original[idx]);
    let op = vec4<f32>(o.xyz * o.w, o.w);
    let mixed = op + (unpack(blurred[idx]) - op) * m;
    var rgb = vec3<f32>(0.0);
    if mixed.w > 1e-6 { rgb = mixed.xyz / mixed.w; }
    output[idx] = pack(vec4<f32>(rgb, mixed.w));
}
"
);

// A focus blur's copy of the layer, premultiplied and keeping its alpha,
// for the blur passes (the frame's own premultiply ends at alpha 1).
const SHADER_FOCUS_BLUR_PREMULTIPLY: &str = concat!(
    "
struct Params { width: u32, height: u32, radius: i32, _pad: u32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(p.xyz * p.w, p.w));
}
"
);

const SHADER_TILT_SHIFT_BLEND: &str = concat!(
    "
struct Params {
    width: u32, height: u32, center: f32, band: f32,
    softness: f32, _pad: u32, _pad2: u32, _pad3: u32,
}
@group(0) @binding(0) var<storage, read> original: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read> blurred: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(3) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let y = idx / params.width;
    let o = unpack(original[idx]);
    let bl = unpack(blurred[idx]);
    let ny = (f32(y) + 0.5) / f32(params.height);
    let d = abs(ny - params.center) - params.band * 0.5;
    var m = clamp(d / max(params.softness, 0.001), 0.0, 1.0);
    m = m * m * (3.0 - 2.0 * m);
    output[idx] = pack(vec4<f32>(o.xyz + (bl.xyz - o.xyz) * m, o.w));
}
"
);

// Old film — gate weave, flicker, sepia, scratches, dust and a soft
// vignette, one pass, all driven by `seed` (the frame number).
const SHADER_OLD_FILM: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, flicker: f32, scratches: f32,
    dust: f32, weave: f32, sepia: f32, seed: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn hash(p: u32) -> f32 {
    var x = p;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = (x >> 16u) ^ x;
    return f32(x) / 4294967295.0;
}

fn sample_clamped(x: f32, y: f32) -> vec4<f32> {
    let xi = clamp(i32(round(x)), 0, i32(params.width) - 1);
    let yi = clamp(i32(round(y)), 0, i32(params.height) - 1);
    return unpack(input[u32(yi) * params.width + u32(xi)]);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let wf = f32(params.width);
    let hf = f32(params.height);
    let fseed = params.seed * 2654435761u;
    let flick = 1.0 + (hash(fseed) - 0.5) * params.flicker * 0.4;
    let wx = (hash(fseed + 11u) - 0.5) * params.weave * 6.0 * wf / 1920.0;
    let wy = (hash(fseed + 23u) - 0.5) * params.weave * 6.0 * hf / 1080.0;
    let o = sample_clamped(f32(x) + wx, f32(y) + wy);
    let l = dot(o.xyz, vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb));
    var r = (o.x + (l * 1.07 - o.x) * params.sepia) * flick;
    var g = (o.y + (l * 0.93 - o.y) * params.sepia) * flick;
    var b = (o.z + (l * 0.72 - o.z) * params.sepia) * flick;
    let n_scr = u32(round(params.scratches * 4.0));
    let life = params.seed / 6u;
    var k: u32 = 0u;
    loop {
        if k >= n_scr { break; }
        let sx = hash((life + k * 97u) * 7919u) * wf;
        let jitter = (hash(params.seed + k * 31u) - 0.5) * 2.0;
        if abs(f32(x) - (sx + jitter)) < 0.8 {
            r = r + (1.0 - r) * 0.35;
            g = g + (1.0 - g) * 0.35;
            b = b + (1.0 - b) * 0.35;
        }
        k = k + 1u;
    }
    let cx = x / 5u;
    let cy = y / 5u;
    let dh = hash(cx * 7919u + cy * 104729u + params.seed * 15485863u);
    if dh > 1.0 - params.dust * 0.004 {
        r = r * 0.15;
        g = g * 0.15;
        b = b * 0.15;
    }
    let nx = (f32(x) - wf * 0.5) / (wf * 0.5);
    let ny = (f32(y) - hf * 0.5) / (hf * 0.5);
    let vig = 1.0 - 0.25 * (nx * nx + ny * ny);
    output[idx] = pack(vec4<f32>(r * vig, g * vig, b * vig, o.w));
}
"
);

// VHS — per-row tracking jitter with a wandering tear band, chroma
// bleed (luma from here, chroma from a few pixels left), tape noise
// and scanlines.
const SHADER_VHS: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, tracking: f32, bleed: f32,
    noise: f32, scanlines: f32, seed: u32, luma_yr: f32,
    luma_yg: f32, luma_yb: f32, _pad: u32, _pad2: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn hash(p: u32) -> f32 {
    var x = p;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = (x >> 16u) ^ x;
    return f32(x) / 4294967295.0;
}

fn sample_clamped(x: f32, yi: i32) -> vec4<f32> {
    let xi = clamp(i32(round(x)), 0, i32(params.width) - 1);
    let yc = clamp(yi, 0, i32(params.height) - 1);
    return unpack(input[u32(yc) * params.width + u32(xi)]);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let hf = f32(params.height);
    let band_y = hash(params.seed * 2654435761u) * hf;
    let band_h = hf * 0.06;
    let rh = hash(y * 7919u + params.seed * 15485863u);
    let in_band = abs(f32(y) - band_y) < band_h;
    var dx = (rh - 0.5) * params.tracking * 3.0;
    if in_band {
        dx = dx + (rh - 0.5) * params.tracking * 40.0;
    }
    let o = sample_clamped(f32(x) + dx, i32(y));
    let sh = sample_clamped(f32(x) + dx - params.bleed * 8.0, i32(y));
    let yw = vec3<f32>(params.luma_yr, params.luma_yg, params.luma_yb);
    let lo = dot(o.xyz, yw);
    let ls = dot(sh.xyz, yw);
    var r = lo + (sh.x - ls);
    var g = lo + (sh.y - ls);
    var b = lo + (sh.z - ls);
    var n = (hash((y * params.width + x) * 7919u + params.seed) - 0.5) * params.noise * 0.3;
    if in_band {
        n = n * 3.0;
    }
    r = r + n;
    g = g + n;
    b = b + n;
    if (y % 2u) == 1u {
        let k = 1.0 - params.scanlines * 0.25;
        r = r * k;
        g = g * k;
        b = b * k;
    }
    output[idx] = pack(vec4<f32>(r, g, b, o.w));
}
"
);

const SHADER_MEDIAN_FILTER: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    radius: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = i32(idx % w);
    let y = i32(idx / w);
    let r = i32(min(params.radius, 2u));

    // Collect neighborhood values (max 5x5 = 25)
    var vals_r: array<f32, 25>;
    var vals_g: array<f32, 25>;
    var vals_b: array<f32, 25>;
    var n = 0u;
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            let p = unpack(input[sy * w + sx]);
            vals_r[n] = p.x;
            vals_g[n] = p.y;
            vals_b[n] = p.z;
            n = n + 1u;
        }
    }

    // Partial selection sort to find median
    let mid = n / 2u;
    for (var i = 0u; i <= mid; i = i + 1u) {
        var min_idx = i;
        for (var j = i + 1u; j < n; j = j + 1u) {
            if vals_r[j] < vals_r[min_idx] { min_idx = j; }
        }
        if min_idx != i {
            let tmp = vals_r[i]; vals_r[i] = vals_r[min_idx]; vals_r[min_idx] = tmp;
        }
    }
    for (var i = 0u; i <= mid; i = i + 1u) {
        var min_idx = i;
        for (var j = i + 1u; j < n; j = j + 1u) {
            if vals_g[j] < vals_g[min_idx] { min_idx = j; }
        }
        if min_idx != i {
            let tmp = vals_g[i]; vals_g[i] = vals_g[min_idx]; vals_g[min_idx] = tmp;
        }
    }
    for (var i = 0u; i <= mid; i = i + 1u) {
        var min_idx = i;
        for (var j = i + 1u; j < n; j = j + 1u) {
            if vals_b[j] < vals_b[min_idx] { min_idx = j; }
        }
        if min_idx != i {
            let tmp = vals_b[i]; vals_b[i] = vals_b[min_idx]; vals_b[min_idx] = tmp;
        }
    }

    let orig = unpack(input[idx]);
    output[idx] = pack(vec4<f32>(vals_r[mid], vals_g[mid], vals_b[mid], orig.w));
}
"
);

// Radial blur (zoom or spin). Averages samples along a radial or
// rotational path, in linear light. Zoom spreads the samples over
// ±amount % of each pixel's distance from the centre; spin over an arc
// of `amount` degrees. (Both amounts used to be divided by the sample
// count and the distance again, so the presets moved pixels by less
// than one — no visible blur.)
const SHADER_RADIAL_BLUR: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    center_x: f32,
    center_y: f32,
    amount: f32,
    samples: u32,
    mode: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
// Bilinear, clamped at the edges, as the CPU's `bilinear` mixes: a
// nearest pixel flips on the last bit of a position landing on a half
// (a fused multiply-add here, the approximate sin and cos), and the two
// routes then read different pixels at every edge (#1440).
fn sample_linear_rgb(fx: f32, fy: f32, w: u32, h: u32) -> vec3<f32> {
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]).xyz;
    let tr = unpack(input[y0 * w + x1]).xyz;
    let bl = unpack(input[y1 * w + x0]).xyz;
    let br = unpack(input[y1 * w + x1]).xyz;
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = f32(idx % w);
    let y = f32(idx / w);

    let cx = params.center_x * f32(w);
    let cy = params.center_y * f32(h);
    let dx = x - cx;
    let dy = y - cy;
    let s = params.samples;

    var sum = vec3<f32>(0.0, 0.0, 0.0);

    if params.mode == 0u {
        // Zoom blur
        let scale = params.amount / 100.0;
        for (var i = 0u; i < s; i = i + 1u) {
            let t = (f32(i) / f32(s) - 0.5) * 2.0;
            sum = sum + sample_linear_rgb(x + dx * t * scale, y + dy * t * scale, w, h);
        }
    } else {
        // Spin blur
        let half_arc = params.amount * 3.14159265 / 180.0 * 0.5;
        for (var i = 0u; i < s; i = i + 1u) {
            let t = (f32(i) / f32(s) - 0.5) * 2.0;
            let ang = t * half_arc;
            let cos_a = cos(ang);
            let sin_a = sin(ang);
            sum = sum + sample_linear_rgb(cx + dx * cos_a - dy * sin_a, cy + dx * sin_a + dy * cos_a, w, h);
        }
    }

    let inv = 1.0 / f32(s);
    // Preserve original alpha (alpha stays linear, no gamma).
    let orig_a = unpack(input[idx]).w;
    output[idx] = pack(vec4<f32>(sum * inv, orig_a));
}
"
);

// Levels — black/white point remap + gamma on the display-encoded
// picture: the points are on the scale the user sees, as in other
// editors (in linear light, "0.1" meant sRGB 89).
const SHADER_LEVELS: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    in_black: f32,
    in_white: f32,
    inv_gamma: f32,
    out_black: f32,
    out_white: f32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let e = enc3(p.xyz);

    let in_range = max(params.in_white - params.in_black, 0.001);
    // No ceiling: a highlight above the input white keeps its distance
    // above the output white rather than clipping to it.
    let m = max((e - vec3<f32>(params.in_black)) / in_range, vec3<f32>(0.0));

    let g = vec3<f32>(
        pow(m.x, params.inv_gamma),
        pow(m.y, params.inv_gamma),
        pow(m.z, params.inv_gamma),
    );

    let out_range = params.out_white - params.out_black;
    let out = vec3<f32>(params.out_black) + g * out_range;
    output[idx] = pack(vec4<f32>(dec3(out), p.w));
}
"
);

// Equalize's histogram (#1215), as `crate::equalize_cdf` counts it: each
// pixel's display luma, `yw · encode(rgb)` summed in that order and
// clamped to 0–1, in bin `floor(l × 255)`. Each workgroup counts into its
// own bins, then adds them into the frame's; integer atomics, so the
// counts don't depend on order.
const SHADER_EQUALIZE_HISTOGRAM: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> hist: array<atomic<u32>>;
@group(0) @binding(2) var<uniform> params: Params;

var<workgroup> local: array<atomic<u32>, 256>;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(local_invocation_index) li: u32) {
    atomicStore(&local[li], 0u);
    workgroupBarrier();
    let idx = gid.x;
    if idx < params.width * params.height {
        let c = unpack(input[idx]);
        let l = params.luma.x * linear_to_srgb_component(c.x)
            + params.luma.y * linear_to_srgb_component(c.y)
            + params.luma.z * linear_to_srgb_component(c.z);
        // A NaN counts in bin 0, as the CPU's cast puts it.
        var bin = 0u;
        if l == l {
            bin = min(u32(clamp(l, 0.0, 1.0) * 255.0), 255u);
        }
        atomicAdd(&local[bin], 1u);
    }
    workgroupBarrier();
    let n = atomicLoad(&local[li]);
    if n > 0u {
        atomicAdd(&hist[li], n);
    }
}
"
);

// Equalize's CDF from its histogram (#1215), `crate::equalize_cdf`'s
// arithmetic in its order: from the first occupied bin, the share of the
// pixels at or below each bin past it, or the identity ramp for a frame of
// one bin. 256 bins: one thread walks them.
const SHADER_EQUALIZE_CDF: &str = "
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> hist: array<u32>;
@group(0) @binding(1) var<storage, read_write> cdf: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(1)
fn main() {
    let total = f32(params.width * params.height);
    var first = 0u;
    for (var i = 0u; i < 256u; i = i + 1u) {
        if hist[i] > 0u {
            first = i;
            break;
        }
    }
    var min_cum = 0u;
    for (var i = 0u; i <= first; i = i + 1u) {
        min_cum = min_cum + hist[i];
    }
    var cum = 0u;
    for (var i = 0u; i < 256u; i = i + 1u) {
        cum = cum + hist[i];
        if total > f32(min_cum) {
            let above = select(cum - min_cum, 0u, cum < min_cum);
            cdf[i] = clamp(f32(above) / (total - f32(min_cum)), 0.0, 1.0);
        } else {
            cdf[i] = f32(i) / 255.0;
        }
    }
}
";

// Histogram equalisation — flatten the distribution of display-encoded
// luma, the brightness the user sees. The CDF is the histogram passes'
// (or `crate::equalize_cdf`'s, on the break path).
const SHADER_EQUALIZE: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
    luma: vec4<f32>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> cdf: array<f32>;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let p = unpack(input[idx]);
    let e = enc3(p.xyz);

    // The histogram covers what the display shows; light above white
    // passes through, so equalising does not crush the highlights.
    let over = max(e - vec3<f32>(1.0), vec3<f32>(0.0));
    let d = min(e, vec3<f32>(1.0));

    // Compute luminance and look up equalised value from CDF
    let luma = dot(params.luma.xyz, d);
    let luma_idx = u32(clamp(luma * 255.0, 0.0, 255.0));
    let eq_luma = cdf[luma_idx];

    // Scale RGB proportionally to match new luminance
    let scale = select(eq_luma / luma, 1.0, luma < 0.001);

    // Blend with original based on strength
    let out = d + (d * scale - d) * params.strength + over;
    output[idx] = pack(vec4<f32>(dec3(out), p.w));
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
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
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

// Binocular vignette (#989): the frame seen through two eyepieces, the
// CPU twin's arithmetic step for step: the nearest eye's normalised
// distance, a soft edge inside its rim, red and blue split along the
// line from its centre towards the rim, then the outside darkened or
// lightened by `amount`.
// Particles (#1141): `particles::pixel`, step for step. Every number
// that needs trigonometry or a colour conversion arrives worked out.
const SHADER_PARTICLES: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, seed: u32, additive: u32,
    cell: f32, dir_x: f32, dir_y: f32, speed: f32,
    size: f32, spread: f32, lifetime: f32, opacity: f32,
    r: f32, g: f32, b: f32, time: f32,
    sway: f32, sway_rate: f32, gravity: f32, streak: f32,
    twinkle: f32, cooling: f32, inv_cell: f32, _pad1: f32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

const PERIOD: f32 = 64.0;

fn hash_u(v: u32) -> u32 {
    var x = v;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    x = ((x >> 16u) ^ x) * 0x45d9f3bu;
    return (x >> 16u) ^ x;
}

fn rand3(a: u32, b: u32, c: u32) -> f32 {
    return f32(hash_u(a ^ hash_u(b ^ hash_u(c)))) / 4294967295.0;
}

fn wave(t: f32) -> f32 {
    let x = t - floor(t);
    let tri = 1.0 - 4.0 * abs(x - 0.5);
    return tri * (1.5 - 0.5 * tri * tri);
}

fn sstep(lo: f32, hi: f32, x: f32) -> f32 {
    let t = clamp((x - lo) / (hi - lo), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = params;
    var c = unpack(input[idx]);
    let fx = f32(idx % p.width) + 0.5;
    let fy = f32(idx / p.width) + 0.5;
    for (var layer: u32 = 0u; layer < 3u; layer = layer + 1u) {
        let z = 0.55 + 0.225 * f32(layer);
        let s = p.cell;
        let span = s * PERIOD;
        var shift_x = p.dir_x * p.speed * z * p.time;
        var shift_y = p.dir_y * p.speed * z * p.time;
        shift_x = shift_x - floor(shift_x / span + 0.5) * span;
        shift_y = shift_y - floor(shift_y / span + 0.5) * span;
        let ux = fx - shift_x + s * 0.37 * f32(layer);
        let uy = fy - shift_y + s * 0.61 * f32(layer);
        let cx0 = floor(ux * p.inv_cell);
        let cy0 = floor(uy * p.inv_cell);
        let key = p.seed * 131u + layer * 7919u;
        for (var j: u32 = 0u; j < 9u; j = j + 1u) {
            let cxf = cx0 + f32(j % 3u) - 1.0;
            let cyf = cy0 + f32(j / 3u) - 1.0;
            let ci = u32(cxf - floor(cxf / PERIOD) * PERIOD);
            let cj = u32(cyf - floor(cyf / PERIOD) * PERIOD);
            let h1 = rand3(ci, cj, key);
            let h2 = rand3(ci, cj, key + 1u);
            let h3 = rand3(ci, cj, key + 2u);
            let h4 = rand3(ci, cj, key + 3u);
            let h5 = rand3(ci, cj, key + 4u);
            let life = p.lifetime * (0.6 + 0.8 * h1);
            let q = p.time / life + h2;
            let cycle = floor(q);
            let age = q - cycle;
            let born = hash_u(u32(i32(cycle)));
            let ax = (cxf + rand3(ci ^ born, cj, key + 5u)) * s;
            let ay = (cyf + rand3(ci, cj ^ born, key + 6u)) * s;
            let sw = wave(p.time * p.sway_rate + h3) * p.sway * s;
            let sx = wave(h4);
            let sy = wave(h4 + 0.25);
            var dx = -p.dir_y * sw + sx * p.spread * s * age;
            var dy = p.dir_x * sw + sy * p.spread * s * age + p.gravity * s * age * age;
            let len = sqrt(dx * dx + dy * dy);
            let reach = 0.45 * s;
            if len > reach {
                dx = dx * (reach / len);
                dy = dy * (reach / len);
            }
            let px_ = ax + dx;
            let py_ = ay + dy;
            var ex = ux - px_;
            var ey = uy - py_;
            let tail = p.speed * z * p.streak;
            if tail > 0.0 {
                let along = clamp(ex * -p.dir_x + ey * -p.dir_y, 0.0, tail);
                ex = ex + p.dir_x * along;
                ey = ey + p.dir_y * along;
            }
            let dist = sqrt(ex * ex + ey * ey);
            let true_radius = min(p.size * z * (0.6 + 0.8 * h5), 0.25 * s);
            let radius = min(max(true_radius, 1.0), 0.25 * s);
            if dist >= radius {
                continue;
            }
            let f = 1.0 - dist / radius;
            var a = (true_radius / radius) * (true_radius / radius) * p.opacity * (0.45 + 0.55 * z) * sstep(0.0, 0.12, age) * (1.0 - sstep(0.65, 1.0, age)) * f * f * (3.0 - 2.0 * f);
            if p.twinkle > 0.0 {
                let tw = wave(p.time * 2.3 + h3 * 7.0);
                a = a * (1.0 - p.twinkle * 0.5 * (1.0 + tw));
            }
            let warm = 1.0 - p.cooling * age;
            if p.additive == 1u {
                c.x = c.x + p.r * warm * a * 1.5;
                c.y = c.y + p.g * warm * warm * a * 1.5;
                c.z = c.z + p.b * warm * warm * warm * a * 1.5;
                c.w = c.w + a * (1.0 - c.w);
            } else {
                c.x = c.x + (p.r - c.x) * a;
                c.y = c.y + (p.g - c.y) * a;
                c.z = c.z + (p.b - c.z) * a;
                c.w = c.w + a * (1.0 - c.w);
            }
        }
    }
    output[idx] = pack(c);
}
"
);

const SHADER_BINOCULAR_VIGNETTE: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params { width: u32, height: u32, amount: f32, spacing: f32, eye_w: f32, eye_h: f32, softness: f32, fringe: f32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn sample_bilinear(fx: f32, fy: f32, w: u32, h: u32) -> vec4<f32> {
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]);
    let tr = unpack(input[y0 * w + x1]);
    let bl = unpack(input[y1 * w + x0]);
    let br = unpack(input[y1 * w + x1]);
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = f32(idx % w);
    let y = f32(idx / w);
    let wf = f32(w);
    let hf = f32(h);
    let rx = max(params.eye_w * hf * 0.5, 1.0);
    let ry = max(params.eye_h * hf * 0.5, 1.0);
    let cy = hf * 0.5;
    let c1 = wf * (0.5 - params.spacing * 0.5);
    let c2 = wf * (0.5 + params.spacing * 0.5);
    let d1 = sqrt(((x - c1) / rx) * ((x - c1) / rx) + ((y - cy) / ry) * ((y - cy) / ry));
    let d2 = sqrt(((x - c2) / rx) * ((x - c2) / rx) + ((y - cy) / ry) * ((y - cy) / ry));
    var d = d1;
    var cx = c1;
    if d2 < d1 { d = d2; cx = c2; }
    let soft = max(params.softness, 0.01);
    let falloff = clamp((d - (1.0 - soft)) / soft, 0.0, 1.0);
    let k = 1.0 + params.amount * falloff;
    let px = unpack(input[idx]);
    var r = px.x;
    var b = px.z;
    let vx = x - cx;
    let vy = y - cy;
    let len = sqrt(vx * vx + vy * vy);
    let reach = clamp(d, 0.0, 1.0);
    let s = params.fringe * reach * reach;
    if len > 0.0 && s != 0.0 {
        let ux = vx / len * s;
        let uy = vy / len * s;
        r = sample_bilinear(x + ux, y + uy, w, h).x;
        b = sample_bilinear(x - ux, y - uy, w, h).z;
    }
    output[idx] = pack(vec4<f32>(r * k, px.y * k, b * k, px.w));
}
"
);

const SHADER_CHROMATIC_ABERRATION: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    red_shift: f32,
    blue_shift: f32,
    center_x: f32,
    center_y: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

// Bilinear, clamped at the edges, as the CPU's `bilinear` mixes: a
// nearest pixel flips on the last bit of a position landing on a half
// (the division, the square root), and the two routes then read
// different pixels at edges (#1442).
fn sample_bilinear(fx: f32, fy: f32, w: u32, h: u32) -> vec4<f32> {
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]);
    let tr = unpack(input[y0 * w + x1]);
    let bl = unpack(input[y1 * w + x0]);
    let br = unpack(input[y1 * w + x1]);
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    let w = params.width;
    let h = params.height;
    if idx >= w * h { return; }
    let x = idx % w;
    let y = idx / w;

    let cx = params.center_x * f32(w);
    let cy = params.center_y * f32(h);
    let dx = f32(x) - cx;
    let dy = f32(y) - cy;
    let dist = sqrt(dx * dx + dy * dy);
    let max_dist = max(sqrt(cx * cx + cy * cy), 1.0);
    let norm_dist = dist / max_dist;

    // Red channel: sample shifted
    let r_scale = 1.0 + params.red_shift * norm_dist / max_dist;
    let r_val = sample_bilinear(cx + dx * r_scale, cy + dy * r_scale, w, h).x;

    // Green channel: original position
    let g_pixel = unpack(input[idx]);
    let g_val = g_pixel.y;
    let a_val = g_pixel.w;

    // Blue channel: sample shifted
    let b_scale = 1.0 + params.blue_shift * norm_dist / max_dist;
    let b_val = sample_bilinear(cx + dx * b_scale, cy + dy * b_scale, w, h).z;

    output[idx] = pack(vec4<f32>(r_val, g_val, b_val, a_val));
}
"
);

// Rolling shutter correction — per-row affine warp with bilinear
// sampling. Bilinear averaging benefits from linear light; without
// it, the four-tap interpolation darkens soft sub-pixel edges
// (the classic gamma-resize issue).
const SHADER_ROLLING_SHUTTER: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad: u32,
}

struct RowXform {
    a: f32, b: f32, tx: f32,
    c: f32, d: f32, ty: f32,
    _pad0: f32, _pad1: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> row_xforms: array<RowXform>;
",
    shader_linear_helpers_wgsl!(),
    "
fn sample_bilinear(fx: f32, fy: f32) -> vec4<f32> {
    let w = params.width;
    let h = params.height;
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]);
    let tr = unpack(input[y0 * w + x1]);
    let bl = unpack(input[y1 * w + x0]);
    let br = unpack(input[y1 * w + x1]);
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;

    let xf = row_xforms[y];
    let fx = f32(x);
    let fy = f32(y);

    let src_x = xf.a * fx + xf.b * fy + xf.tx;
    let src_y = xf.c * fx + xf.d * fy + xf.ty;

    output[idx] = pack(sample_bilinear(src_x, src_y));
}
"
);

// Stabilisation — per-frame homography warp with bilinear sampling.
// Bilinear averaging in linear preserves soft edge luminance through
// the warp.
const SHADER_STABILIZATION: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    crop_zoom: f32,
    _pad: u32,
}

struct Homography3x3 {
    h0: f32, h1: f32, h2: f32,
    h3: f32, h4: f32, h5: f32,
    h6: f32, h7: f32, h8: f32,
    _p0: f32, _p1: f32, _p2: f32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> homography: Homography3x3;
",
    shader_linear_helpers_wgsl!(),
    "
fn sample_bilinear(fx: f32, fy: f32) -> vec4<f32> {
    let w = params.width;
    let h = params.height;
    let x0 = u32(clamp(i32(floor(fx)), 0, i32(w) - 1));
    let x1 = u32(clamp(i32(floor(fx)) + 1, 0, i32(w) - 1));
    let y0 = u32(clamp(i32(floor(fy)), 0, i32(h) - 1));
    let y1 = u32(clamp(i32(floor(fy)) + 1, 0, i32(h) - 1));
    let dx = fract(fx);
    let dy = fract(fy);
    let tl = unpack(input[y0 * w + x0]);
    let tr = unpack(input[y0 * w + x1]);
    let bl = unpack(input[y1 * w + x0]);
    let br = unpack(input[y1 * w + x1]);
    return mix(mix(tl, tr, dx), mix(bl, br, dx), dy);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let px = idx % params.width;
    let py = idx / params.width;

    // Apply crop/zoom: center and scale
    let cx = f32(params.width) * 0.5;
    let cy = f32(params.height) * 0.5;
    let x = (f32(px) - cx) / params.crop_zoom + cx;
    let y = (f32(py) - cy) / params.crop_zoom + cy;

    // Apply homography (perspective divide for completeness, but h6/h7 ~ 0 for affine)
    let w_div = homography.h6 * x + homography.h7 * y + homography.h8;
    let src_x = (homography.h0 * x + homography.h1 * y + homography.h2) / w_div;
    let src_y = (homography.h3 * x + homography.h4 * y + homography.h5) / w_div;

    output[idx] = pack(sample_bilinear(src_x, src_y));
}
"
);

// Colour balance — temperature / tint as a white-balance matrix, an
// optional HSL hue rotation (the Corrector's), and a mix with the input.
const SHADER_COLOR_BALANCE: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    wb0: f32, wb1: f32, wb2: f32,
    wb3: f32, wb4: f32, wb5: f32,
    wb6: f32, wb7: f32, wb8: f32,
    hue: f32,
    global_mix: f32,
    _pad0: u32,
    _pad1: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    // Temperature / tint: the white-balance matrix.
    var r = params.wb0 * orig.x + params.wb1 * orig.y + params.wb2 * orig.z;
    var g = params.wb3 * orig.x + params.wb4 * orig.y + params.wb5 * orig.z;
    var b = params.wb6 * orig.x + params.wb7 * orig.y + params.wb8 * orig.z;

    // Hue rotation, in HSL (the Corrector's hue, the same maths).
    if abs(params.hue) >= 0.01 {
        let rot = hue_rotate_hsl(vec3<f32>(r, g, b), params.hue);
        r = rot.x;
        g = rot.y;
        b = rot.z;
    }

    // Global mix
    r = orig.x + (r - orig.x) * params.global_mix;
    g = orig.y + (g - orig.y) * params.global_mix;
    b = orig.z + (b - orig.z) * params.global_mix;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
"
);

// HDR zones (#240): `klipp_grade_model::hdr_zones::apply` per pixel.
// Zone 0 is Global, 1 to 3 reach down from their boundary, 4 to 6 up.
const SHADER_HDR_ZONES: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
    luma: vec4<f32>,
    gain_sat: array<vec4<f32>, 7>,
    edge: array<vec4<f32>, 7>,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
fn zone_weight(i: u32, s: f32, b: f32, f: f32) -> f32 {
    if i == 0u { return 1.0; }
    let half = max(f, 0.0) * 0.5;
    let lo = b - half;
    let hi = b + half;
    var up: f32;
    if hi <= lo {
        up = select(0.0, 1.0, s >= lo);
    } else {
        let t = clamp((s - lo) / (hi - lo), 0.0, 1.0);
        up = t * t * (3.0 - 2.0 * t);
    }
    if i <= 3u { return 1.0 - up; }
    return up;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let orig = unpack(input[idx]);
    let luma = params.luma.xyz;
    let s = log2(max(dot(luma, orig.xyz), 0.18 / 1048576.0) / 0.18);
    var sum = vec3<f32>(0.0);
    var sat = 1.0;
    for (var i = 0u; i < 7u; i = i + 1u) {
        let w = zone_weight(i, s, params.edge[i].x, params.edge[i].y);
        sum = sum + w * params.gain_sat[i].xyz;
        sat = sat * (1.0 + w * (params.gain_sat[i].w - 1.0));
    }
    var c = select(orig.xyz * exp2(sum), orig.xyz, sum == vec3<f32>(0.0));
    if sat != 1.0 {
        let y = dot(luma, c);
        c = vec3<f32>(y) + (c - vec3<f32>(y)) * sat;
    }
    output[idx] = pack(vec4<f32>(c, orig.w));
}
"
);

// Wheel sat/bri — per-wheel saturation and brightness adjustments,
// band-weighted. Same formula as the sRGB version but in linear so
// BT.709 luma sits in its defined domain and the wheel-band curves
// (lift = 1 − luma, gain = luma, gamma = sin curve) target physical
// tonal regions.
const SHADER_WHEEL_SAT_BRI: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    lift_sat: f32,
    lift_bri: f32,
    gamma_sat: f32,
    gamma_bri: f32,
    gain_sat: f32,
    gain_bri: f32,
    offset_sat: f32,
    offset_bri: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    var r = orig.x;
    var g = orig.y;
    var b = orig.z;

    let luma = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;

    let lift_w = max(1.0 - luma, 0.0);
    let gamma_w = max(sin(clamp(luma, 0.0, 1.0) * 3.14159265), 0.0);
    let gain_w = luma;

    let bri_adj = params.lift_bri * lift_w + params.gamma_bri * gamma_w + params.gain_bri * gain_w + params.offset_bri;

    r = r + bri_adj;
    g = g + bri_adj;
    b = b + bri_adj;

    let sat_adj = params.lift_sat * lift_w + params.gamma_sat * gamma_w + params.gain_sat * gain_w + params.offset_sat;
    let sat_factor = 1.0 + sat_adj;
    let luma2 = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
    r = luma2 + (r - luma2) * sat_factor;
    g = luma2 + (g - luma2) * sat_factor;
    b = luma2 + (b - luma2) * sat_factor;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
"
);

// RGB curves — four LUT-lookup curves (master + per-channel), on the
// display-encoded picture: the curve's axis is the sRGB encoding of the
// light, so mid-grey sits at 0.46 where a colourist expects it, not at
// 0.18. Past the curve's ends it continues straight at the slope it
// ends with, so an identity curve is identity on an HDR highlight
// instead of flattening it to white, and a curve the user flattened at
// the top still flattens. Negative channels (a wide-gamut colour in a
// narrower working space) take the curve mirrored, so they survive too.
// Preserve-luma and the mix stay in linear light, where luminance is.
const SHADER_RGB_CURVES: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    preserve_luma: u32,
    mix: f32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;
// LUT layout: 4 channels x 256 entries = 1024 floats
// 0..255   = master
// 256..511 = red
// 512..767 = green
// 768..1023 = blue
",
    shader_linear_helpers_wgsl!(),
    "
// The curve inside its [0, 1] domain.
fn lut_lookup(channel: u32, val: f32) -> f32 {
    let base = channel * 256u;
    let idx_f = clamp(val, 0.0, 1.0) * 255.0;
    let idx0 = u32(clamp(idx_f, 0.0, 254.0));
    let idx1 = idx0 + 1u;
    let frac = idx_f - f32(idx0);
    return lut[base + idx0] * (1.0 - frac) + lut[base + idx1] * frac;
}

// The curve over the whole real line: straight past either end, at the
// slope the curve ends with there.
fn curve(channel: u32, val: f32) -> f32 {
    let base = channel * 256u;
    if val > 1.0 {
        let slope = (lut[base + 255u] - lut[base + 254u]) * 255.0;
        return lut[base + 255u] + (val - 1.0) * slope;
    }
    if val < 0.0 {
        let slope = (lut[base + 1u] - lut[base]) * 255.0;
        return lut[base] + val * slope;
    }
    return lut_lookup(channel, val);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);

    // Per-channel curves, then master, on the display-encoded picture.
    var r = curve(1u, enc_signed(orig.x));
    var g = curve(2u, enc_signed(orig.y));
    var b = curve(3u, enc_signed(orig.z));
    r = dec_signed(curve(0u, r));
    g = dec_signed(curve(0u, g));
    b = dec_signed(curve(0u, b));

    // Preserve luma: scale output to match input luma
    if params.preserve_luma != 0u {
        let orig_luma = params.luma_yr * orig.x + params.luma_yg * orig.y + params.luma_yb * orig.z;
        let new_luma = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
        if new_luma > 0.0001 {
            let scale = orig_luma / new_luma;
            r = r * scale;
            g = g * scale;
            b = b * scale;
        }
    }

    // Mix with original
    r = orig.x + (r - orig.x) * params.mix;
    g = orig.y + (g - orig.y) * params.mix;
    b = orig.z + (b - orig.z) * params.mix;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
"
);

// ── Mask matte generation (SDF-based power windows, v2 storage layout) ─────
//
// All per-mask shape data lives in a storage buffer (`records`),
// pooled bezier control points live in a second storage buffer
// (`points`), and the uniform only carries frame constants. This
// removes the previous 4-mask cap and unblocks Bezier shapes —
// which can have arbitrary point counts and so don't fit in a
// fixed-size uniform.
//
// Coverage value (`matte`) is written to all RGB channels as a 0..1
// alpha. The CPU readback path uses `download_raw_rgba_as_u8` (no
// sRGB transform); writing to the linear storage buffer is a no-op
// conversion as long as readers don't gamma-encode it.
const SHADER_MASK_V2: &str = concat!(
    shader_pixel_storage_wgsl!(),
    mask_sdf_wgsl!(),
    "
struct Params {
    width: u32,
    height: u32,
    mask_count: u32,
    _pad: u32,
}

@group(0) @binding(0) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read> records: array<MaskRecord>;
@group(0) @binding(3) var<storage, read> points: array<vec2<f32>>;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let nx = f32(x) / f32(max(params.width - 1u, 1u));
    let ny = f32(y) / f32(max(params.height - 1u, 1u));
    let aspect = f32(params.height) / f32(params.width);

    let m = clamp(window_matte(nx, ny, aspect, params.mask_count), 0.0, 1.0);
    output[idx] = pack(vec4<f32>(m, m, m, 1.0));
}
"
);

// Vibrance — saturation-weighted boost. Muted colours get a bigger
// push; already-saturated colours (including skin) are protected.
// BT.709 luma now sits in its defined domain.
const SHADER_VIBRANCE: &str = concat!(
    "
struct Params {
    width: u32, height: u32, amount: f32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let mx = max(max(p.x, p.y), p.z);
    let mn = min(min(p.x, p.y), p.z);
    let sat = mx - mn;
    let luma = params.luma_yr * p.x + params.luma_yg * p.y + params.luma_yb * p.z;
    let weight = 1.0 - clamp(sat, 0.0, 1.0);
    let boost = 1.0 + params.amount * weight;
    let out = vec3<f32>(
        luma + (p.x - luma) * boost,
        luma + (p.y - luma) * boost,
        luma + (p.z - luma) * boost,
    );
    output[idx] = pack(vec4<f32>(out, p.w));
}
"
);

// Colour warper — 12-sector hue/sat shifting with cosine
// interpolation between sectors. HSL operates on linear RGB; hue
// values are colour-space-agnostic but lum in HSL is linear
// rather than sRGB-coded (grey = 0.18).
const SHADER_COLOR_WARPER: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    _pad0: u32,
    _pad1: u32,
    hue0: f32, hue1: f32, hue2: f32, hue3: f32,
    hue4: f32, hue5: f32, hue6: f32, hue7: f32,
    hue8: f32, hue9: f32, hue10: f32, hue11: f32,
    sat0: f32, sat1: f32, sat2: f32, sat3: f32,
    sat4: f32, sat5: f32, sat6: f32, sat7: f32,
    sat8: f32, sat9: f32, sat10: f32, sat11: f32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "

fn get_hue_shift(sector: u32) -> f32 {
    switch sector {
        case 0u: { return params.hue0; } case 1u: { return params.hue1; }
        case 2u: { return params.hue2; } case 3u: { return params.hue3; }
        case 4u: { return params.hue4; } case 5u: { return params.hue5; }
        case 6u: { return params.hue6; } case 7u: { return params.hue7; }
        case 8u: { return params.hue8; } case 9u: { return params.hue9; }
        case 10u: { return params.hue10; } default: { return params.hue11; }
    }
}
fn get_sat_shift(sector: u32) -> f32 {
    switch sector {
        case 0u: { return params.sat0; } case 1u: { return params.sat1; }
        case 2u: { return params.sat2; } case 3u: { return params.sat3; }
        case 4u: { return params.sat4; } case 5u: { return params.sat5; }
        case 6u: { return params.sat6; } case 7u: { return params.sat7; }
        case 8u: { return params.sat8; } case 9u: { return params.sat9; }
        case 10u: { return params.sat10; } default: { return params.sat11; }
    }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let hsl = picture_hsl(p.xyz);
    let h_deg = hsl.h * 360.0;
    let sector_f = h_deg / 30.0;
    let sector_low = u32(floor(sector_f)) % 12u;
    let sector_high = (sector_low + 1u) % 12u;
    let t = fract(sector_f);
    let blend = (1.0 - cos(t * 3.14159265)) * 0.5;
    let hue_shift = get_hue_shift(sector_low) * (1.0 - blend) + get_hue_shift(sector_high) * blend;
    let sat_shift = get_sat_shift(sector_low) * (1.0 - blend) + get_sat_shift(sector_high) * blend;
    let sat_weight = clamp(hsl.s, 0.0, 1.0);
    let new_h = ((h_deg + hue_shift * sat_weight) % 360.0 + 360.0) % 360.0 / 360.0;
    let new_s = clamp(hsl.s + sat_shift * sat_weight, 0.0, 1.0);
    let rgb = picture_rgb(new_h, new_s, hsl.v, hsl.magnitude);
    output[idx] = pack(vec4<f32>(rgb, p.w));
}
"
);


// Clarity — three-pass midtone unsharp mask. CLARITY_BLUR_H reads
// linear RGBA, computes linear-light BT.709 luma, writes the luma
// into the `.x` channel of a shared vec4<f32> intermediate buffer
// (other channels unused — we never downconvert or pack). CLARITY_BLUR_V
// averages that luma (still in `.x`). CLARITY_APPLY adds back detail
// weighted by linear midtone presence, centred on 0.18 linear.
const SHADER_CLARITY_BLUR_H: &str = concat!(
    "
struct Params {
    width: u32, height: u32, radius: u32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);
    let r = i32(params.radius);
    var sum = 0.0;
    var count = 0u;
    for (var dx = -r; dx <= r; dx = dx + 1) {
        let sx = clamp(x + dx, 0, i32(params.width) - 1);
        let p = unpack(input[u32(y) * params.width + u32(sx)]);
        sum += params.luma_yr * p.x + params.luma_yg * p.y + params.luma_yb * p.z;
        count += 1u;
    }
    let luma = sum / f32(count);
    // Store linear luma in the .x channel; downstream passes read it as
    // a linear [0,1] scalar.
    output[idx] = pack(vec4<f32>(luma, 0.0, 0.0, 0.0));
}
"
);

const SHADER_CLARITY_BLUR_V: &str = concat!(
    shader_pixel_storage_wgsl!(),
    "
struct Params {
    width: u32, height: u32, radius: u32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = i32(idx % params.width);
    let y = i32(idx / params.width);
    let r = i32(params.radius);
    var sum = 0.0;
    var count = 0u;
    for (var dy = -r; dy <= r; dy = dy + 1) {
        let sy = clamp(y + dy, 0, i32(params.height) - 1);
        let val = unpack(input[u32(sy) * params.width + u32(x)]).x;
        sum += val;
        count += 1u;
    }
    output[idx] = pack(vec4<f32>(sum / f32(count), 0.0, 0.0, 0.0));
}
"
);

/// Mask cutout — multiply chain RGB by matte's R channel and force
/// alpha=1.0 so the egui viewer (which expects opaque RGBA) stays
/// happy. Feathered mask edges fall out of the multiply for free.
/// 4-binding layout matches `clarity_apply` so we can reuse the same
/// `huesat_bgl` and `huesat_pl` for the pipeline.
const SHADER_MASK_CUTOUT: &str = concat!(
    "
struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> matte: array<vec4<f32>>;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let k = clamp(unpack(matte[idx]).x, 0.0, 1.0);
    output[idx] = pack(vec4<f32>(p.x * k, p.y * k, p.z * k, 1.0));
}
"
);

// Grade mix — a grade's result mixed with the picture it started from,
// kept in binding 3: the Corrector's mix, over all its stages.
/// ApplyMatte as a chain dispatch: the graded frame where the matte
/// selects, the frame kept from before the secondary where it does not.
///
/// `mix` scales the whole matte, so a secondary can be dialled back
/// without touching the key. The matte's coverage is its red channel
/// unless its correction picks luminance or alpha (M3 step 17a), the
/// product clamped to [0, 1], and alpha is left at 1.0 — the same
/// conventions the CPU twin in `klipp_nodes::executor` follows, because
/// the two are compared against each other.
const SHADER_APPLY_MATTE_CHAIN: &str = concat!(
    "
struct Params { width: u32, height: u32, mix: f32, channel: u32, luma: vec3<f32>, invert: u32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> kept: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read> matte: array<vec4<f32>>;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let graded = unpack(input[idx]);
    let plain = unpack(kept[idx]);
    let m = unpack(matte[idx]);
    var c = m.x;
    if params.channel == 1u { c = dot(m.xyz, params.luma); } else if params.channel == 2u { c = m.w; }
    if params.invert == 1u { c = 1.0 - clamp(c, 0.0, 1.0); }
    let k = clamp(c * params.mix, 0.0, 1.0);
    output[idx] = pack(vec4<f32>(
        plain.x + (graded.x - plain.x) * k,
        plain.y + (graded.y - plain.y) * k,
        plain.z + (graded.z - plain.z) * k,
        1.0,
    ));
}
"
);

const SHADER_GRADE_MIX: &str = concat!(
    "
struct Params { width: u32, height: u32, mix: f32, _pad: u32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> kept: array<vec4<f32>>;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let o = unpack(kept[idx]);
    output[idx] = pack(vec4<f32>(
        o.x + (p.x - o.x) * params.mix,
        o.y + (p.y - o.y) * params.mix,
        o.z + (p.z - o.z) * params.mix,
        p.w,
    ));
}
"
);

const SHADER_DEFLICKER: &str = concat!(
    "
struct Params { width: u32, height: u32, gain: f32, strength: f32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
",
    shader_linear_helpers_wgsl!(),
    "
// A scale on the light, which is what an exposure change is: a
// shutter catching less of a light's cycle multiplies the frame by a
// constant, and undoing it multiplies back. Doing this on the encoded
// values would make the correction depend on how dark the frame was.
//
// Alpha is left alone — the frame got darker, it did not become more
// transparent.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let g = 1.0 + (params.gain - 1.0) * params.strength;
    output[idx] = pack(vec4<f32>(p.x * g, p.y * g, p.z * g, p.w));
}
"
);

const SHADER_BLEND_MIX: &str = concat!(
    "
struct Params { width: u32, height: u32, mix: f32, _pad: u32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> other: array<vec4<f32>>;
",
    shader_linear_helpers_wgsl!(),
    "
// Blend: `a + (b - a) * mix`, where `a` is the chain's picture (port 0)
// and `b` is the composite buffer (port 1).
//
// All four channels, alpha included — unlike SHADER_GRADE_MIX, which
// keeps the current alpha because it is mixing a grade back against
// the same picture. Two different pictures have two different mattes,
// and the mix has to carry them both.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let a = unpack(input[idx]);
    let b = unpack(other[idx]);
    output[idx] = pack(a + (b - a) * params.mix);
}
"
);

const SHADER_LAYER_OVER: &str = concat!(
    "
struct Params { width: u32, height: u32, mix: f32, _pad: u32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> other: array<vec4<f32>>;
",
    shader_linear_helpers_wgsl!(),
    "
// Layer: the over operator. Foreground is the chain's picture
// (port 0), background is the composite buffer (port 1).
//
// Alpha composites as `fa + ba * (1 - fa)` rather than saturating at
// 1, so stacking two half-transparent layers gives 0.75 and stays
// available to whatever reads the matte downstream.
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let f = unpack(input[idx]);
    let b = unpack(other[idx]);
    let fa = clamp(f.w, 0.0, 1.0);
    let ba = clamp(b.w, 0.0, 1.0);
    let inv = 1.0 - fa;
    output[idx] = pack(vec4<f32>(
        f.x * fa + b.x * inv,
        f.y * fa + b.y * inv,
        f.z * fa + b.z * inv,
        fa + ba * inv,
    ));
}
"
);

const SHADER_CLARITY_APPLY: &str = concat!(
    "
struct Params {
    width: u32, height: u32, amount: f32, _pad: u32,
    luma_yr: f32, luma_yg: f32, luma_yb: f32, _pad1: u32,
}
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> blurred: array<vec4<f32>>;
",
    shader_linear_helpers_wgsl!(),
    "
@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let p = unpack(input[idx]);
    let luma = params.luma_yr * p.x + params.luma_yg * p.y + params.luma_yb * p.z;
    let blurred_luma = unpack(blurred[idx]).x;
    let detail = luma - blurred_luma;
    // Midtone weight peaked on linear 0.18 (perceptual mid-grey).
    let midtone_weight = max(1.0 - abs((luma / 0.18) - 1.0), 0.0);
    let adj = detail * params.amount * midtone_weight;
    output[idx] = pack(vec4<f32>(
        p.x + adj,
        p.y + adj,
        p.z + adj,
        p.w,
    ));
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
        let wgsl = shader_display_dither_wgsl!();
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
    /// literals the WGSL is written with are the Rust constants.
    #[test]
    fn generator_grain_wgsl_carries_the_rust_constants() {
        let wgsl = shader_generator_grain_wgsl!();
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
        ("TITLE_STYLES", include_str!("gpu/title_styles.wgsl")),
        ("SHADER_BLOOM_THRESHOLD", SHADER_BLOOM_THRESHOLD),
        ("SHADER_BLOOM_BLEND", SHADER_BLOOM_BLEND),
        ("SHADER_GLOW_BLEND", SHADER_GLOW_BLEND),
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
        ("SHADER_PREMULTIPLY", SHADER_PREMULTIPLY),
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
        let shaders = super::shader_sources().into_iter().chain(flow.iter().map(|(n, s)| (*n, s.as_str())));
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
