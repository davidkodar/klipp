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
use tracing::{debug, error, info, warn};

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
    /// bit 0 = invert, bits 8-15 = opacity (0-255)
    pub flags: u32,
    /// Closed-polygon control points for bezier shapes. Empty for
    /// every other shape. Coords are normalised (0..1) on width.
    pub bezier_points: Vec<(f32, f32)>,
}

impl GpuMaskDef {
    pub fn circle(cx: f32, cy: f32, radius: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self {
            shape_type: 0, cx, cy, p1: radius, p2: 0.0, p3: 0.0, feather,
            flags: Self::make_flags(invert, opacity),
            bezier_points: Vec::new(),
        }
    }
    pub fn rectangle(cx: f32, cy: f32, w: f32, h: f32, rotation: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self {
            shape_type: 1, cx, cy, p1: w, p2: h, p3: rotation, feather,
            flags: Self::make_flags(invert, opacity),
            bezier_points: Vec::new(),
        }
    }
    pub fn linear_gradient(sx: f32, sy: f32, ex: f32, ey: f32, feather: f32, invert: bool, opacity: f32) -> Self {
        Self {
            shape_type: 2, cx: sx, cy: sy, p1: ex, p2: ey, p3: 0.0, feather,
            flags: Self::make_flags(invert, opacity),
            bezier_points: Vec::new(),
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
        }
    }
    fn make_flags(invert: bool, opacity: f32) -> u32 {
        let opacity_byte = (opacity.clamp(0.0, 1.0) * 255.0) as u32;
        (if invert { 1 } else { 0 }) | (opacity_byte << 8)
    }
}


/// GPU effects pipeline — holds device, queue, compute pipelines, and reusable buffers.
pub struct GpuEffectsPipeline {
    // Arc-wrapped so the effects pipeline can share its device with
    // klipp-effects' NV12/P010 converters (and any future GPU-using
    // type) through `GpuContext`. Internal call sites deref
    // transparently via Arc's Deref<Target = wgpu::Device>.
    device: std::sync::Arc<wgpu::Device>,
    queue: std::sync::Arc<wgpu::Queue>,
    // Compute pipelines — effects (2-buffer: input, output, params)
    opacity_pipeline: wgpu::ComputePipeline,
    color_fade_pipeline: wgpu::ComputePipeline,
    blur_h_pipeline: wgpu::ComputePipeline,
    blur_v_pipeline: wgpu::ComputePipeline,
    sharpen_pipeline: wgpu::ComputePipeline,
    crop_pipeline: wgpu::ComputePipeline,
    transform_pipeline: wgpu::ComputePipeline,
    chroma_key_pipeline: wgpu::ComputePipeline,
    // Compute pipelines — light effects
    bloom_threshold_pipeline: wgpu::ComputePipeline,
    bloom_blend_pipeline: wgpu::ComputePipeline,
    glow_blend_pipeline: wgpu::ComputePipeline,
    light_rays_pipeline: wgpu::ComputePipeline,
    vignette_pipeline: wgpu::ComputePipeline,
    lens_flare_pipeline: wgpu::ComputePipeline,
    anamorphic_streak_pipeline: wgpu::ComputePipeline,
    film_halation_blend_pipeline: wgpu::ComputePipeline,
    // Compute pipelines — new effects
    invert_pipeline: wgpu::ComputePipeline,
    posterize_pipeline: wgpu::ComputePipeline,
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
    letterbox_pipeline: wgpu::ComputePipeline,
    chromatic_aberration_pipeline: wgpu::ComputePipeline,
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
    huesat_lut_buf: wgpu::Buffer,
    // Compute pipeline — 3D LUT (reuses huesat_bgl: input, output, params, lut storage)
    lut3d_pipeline: wgpu::ComputePipeline,
    lut3d_buf: Option<wgpu::Buffer>,
    lut3d_buf_size: u64,
    // Compute pipeline — color balance (temperature/tint/hue)
    color_balance_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — wheel sat/bri (per-wheel saturation & brightness)
    wheel_sat_bri_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — RGB curves (LUT-based, reuses huesat_bgl 4-binding layout)
    rgb_curves_pipeline: wgpu::ComputePipeline,
    rgb_curves_lut_buf: wgpu::Buffer,
    // Compute pipeline — vibrance
    vibrance_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — color warper (12-sector hue/sat)
    color_warper_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — input transform (log-to-sRGB via 1D LUT, reuses huesat_bgl)
    input_transform_pipeline: wgpu::ComputePipeline,
    input_transform_lut_buf: wgpu::Buffer,
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
    // Compute pipeline — qualifier (secondary color correction)
    qualifier_pipeline: wgpu::ComputePipeline,
    // Compute pipeline — standalone luminance-threshold key (matte
    // producer, simpler than qualifier — luma in, matte out).
    luma_key_pipeline: wgpu::ComputePipeline,
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
    // Compute pipelines — transitions (3-buffer: from, to, output, params)
    transition_pipeline: wgpu::ComputePipeline,
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
    /// Packed RGB10 HDR code values → scene-linear working-space f32,
    /// written into the chain's `buf_a`. The GPU half of
    /// `klipp_color::hdr::hdr10_codes_to_scene_linear_rgba`.
    bridge_hdr10_pipeline: wgpu::ComputePipeline,
    /// Chain output (linear f32) → display sRGB u8, with the HDR display
    /// tone map applied first when asked. Lets a graded frame leave the
    /// GPU as 4 bytes a pixel instead of 16.
    display_u8_pipeline: wgpu::ComputePipeline,
    /// Map-readable copy of `bridge_u8_out_buf` for the display readback.
    display_u8_staging: Option<wgpu::Buffer>,
    // Bind group layouts
    bind_group_layout: wgpu::BindGroupLayout,         // effects: input, output, params
    transition_bgl: wgpu::BindGroupLayout,             // transitions: from, to, output, params
    // Reusable buffers — resized when frame dimensions change
    buf_a: Option<wgpu::Buffer>,
    buf_b: Option<wgpu::Buffer>,
    buf_c: Option<wgpu::Buffer>,  // third buffer for transition "to" frame
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
    params_ring: Vec<wgpu::Buffer>,
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
    /// `input_transform_lut_buf` as binding 3. Used by
    /// `apply_input_transform_chain`.
    chain_bg_input_transform: Vec<Option<wgpu::BindGroup>>,
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
    /// Next ring slot to claim. Reset to 0 in `begin_chain`.
    chain_slot_idx: u32,
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

    /// Working-space luma coefficients pushed into every grade-shader
    /// uniform (saturation, tone-controls keying, lift/gamma/gain
    /// banding). Default `(0.2126, 0.7152, 0.0722)` keeps the pre-
    /// Phase-E behaviour byte-identical for any caller that hasn't
    /// installed a working-space-aware setter (notably the existing
    /// integration tests). The engine flips this to AcesCG / etc. via
    /// [`Self::set_luma_coefs`] when the project's working colorspace
    /// changes.
    luma_coefs: [f32; 3],
}

/// Maximum params size (padded to 256 bytes for alignment)
const PARAMS_SIZE: u64 = 256;
/// Number of uniform-buffer slots in the chain params ring. A maximal
/// grade (~13 active nodes in the default graph) plus multi-pass nodes
/// like clarity (3 slots) fits comfortably. Bump if a future graph ever
/// asserts in `next_params_slot`.
const CHAIN_PARAMS_RING_SIZE: usize = 32;
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
    // Discriminants 4–8 were the legacy in-tree scope-render stage
    // labels (ScopeWfMaxRefresh / ScopeImgDrain / ScopeHistogram /
    // ScopeWaveform / ScopeParade). Removed in Phase 8 — the FFmpeg
    // path does its own timing. The numeric gap is intentional so
    // any persisted instrumentation logs still resolve.
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
        let chroma_key_pipeline = create_pipeline(&device, &pipeline_layout, "chroma_key", SHADER_CHROMA_KEY);
        let bloom_threshold_pipeline = create_pipeline(&device, &pipeline_layout, "bloom_threshold", SHADER_BLOOM_THRESHOLD);
        // Bloom/glow blend uses 4-binding layout (input, bright/blurred, output, params) — same as transitions
        let bloom_blend_pipeline = create_pipeline(&device, &transition_pl, "bloom_blend", SHADER_BLOOM_BLEND);
        let glow_blend_pipeline = create_pipeline(&device, &transition_pl, "glow_blend", SHADER_GLOW_BLEND);
        let light_rays_pipeline = create_pipeline(&device, &pipeline_layout, "light_rays", SHADER_LIGHT_RAYS);
        let vignette_pipeline = create_pipeline(&device, &pipeline_layout, "vignette", SHADER_VIGNETTE);
        let lens_flare_pipeline = create_pipeline(&device, &pipeline_layout, "lens_flare", SHADER_LENS_FLARE);
        let anamorphic_streak_pipeline = create_pipeline(&device, &pipeline_layout, "anamorphic_streak", SHADER_ANAMORPHIC_STREAK);
        let film_halation_blend_pipeline = create_pipeline(&device, &transition_pl, "film_halation_blend", SHADER_FILM_HALATION);
        let invert_pipeline = create_pipeline(&device, &pipeline_layout, "invert", SHADER_INVERT);
        let posterize_pipeline = create_pipeline(&device, &pipeline_layout, "posterize", SHADER_POSTERIZE);
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
        let equalize_pipeline = create_pipeline(&device, &huesat_pl, "equalize", SHADER_EQUALIZE);
        let rsc_pipeline = create_pipeline(&device, &huesat_pl, "rolling_shutter", SHADER_ROLLING_SHUTTER);
        let stabilization_pipeline = create_pipeline(&device, &huesat_pl, "stabilization", SHADER_STABILIZATION);
        let vibrance_pipeline = create_pipeline(&device, &pipeline_layout, "vibrance", SHADER_VIBRANCE);
        let color_warper_pipeline = create_pipeline(&device, &pipeline_layout, "color_warper", SHADER_COLOR_WARPER);
        let input_transform_pipeline = create_pipeline(&device, &huesat_pl, "input_transform", SHADER_INPUT_TRANSFORM);
        let clarity_blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "clarity_blur_h", SHADER_CLARITY_BLUR_H);
        let clarity_blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "clarity_blur_v", SHADER_CLARITY_BLUR_V);
        let clarity_apply_pipeline = create_pipeline(&device, &huesat_pl, "clarity_apply", SHADER_CLARITY_APPLY);
        let mask_cutout_pipeline = create_pipeline(&device, &huesat_pl, "mask_cutout", SHADER_MASK_CUTOUT);
        let mask_pipeline = create_pipeline(&device, &mask_v2_pl, "mask_v2", SHADER_MASK_V2);
        let color_balance_pipeline = create_pipeline(&device, &pipeline_layout, "color_balance", SHADER_COLOR_BALANCE);
        let wheel_sat_bri_pipeline = create_pipeline(&device, &pipeline_layout, "wheel_sat_bri", SHADER_WHEEL_SAT_BRI);
        let rgb_curves_pipeline = create_pipeline(&device, &huesat_pl, "rgb_curves", SHADER_RGB_CURVES);
        let qualifier_pipeline = create_pipeline(&device, &pipeline_layout, "qualifier", SHADER_QUALIFIER);
        let luma_key_pipeline = create_pipeline(&device, &pipeline_layout, "luma_key", SHADER_LUMA_KEY);
        let soft_clip_pipeline = create_pipeline(&device, &pipeline_layout, "soft_clip", SHADER_SOFT_CLIP);
        let grain_pipeline = create_pipeline(&device, &pipeline_layout, "grain", SHADER_GRAIN);
        let alpha_blur_h_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_blur_h", SHADER_ALPHA_BLUR_H);
        let alpha_blur_v_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_blur_v", SHADER_ALPHA_BLUR_V);
        let alpha_dilate_pipeline = create_pipeline(&device, &pipeline_layout, "alpha_dilate", SHADER_ALPHA_DILATE);

        // LUT buffer: 6 curves × 256 entries × 4 bytes = 6144 bytes
        let huesat_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("huesat_lut"),
            size: 6 * 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // RGB curves LUT buffer: 4 channels × 256 entries × 4 bytes = 4096 bytes
        let rgb_curves_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rgb_curves_lut"),
            size: 4 * 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // Input transform LUT buffer: 256 entries × 4 bytes = 1024 bytes
        let input_transform_lut_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("input_transform_lut"),
            size: 256 * 4,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let transition_pipeline = create_pipeline(&device, &transition_pl, "transition", SHADER_TRANSITION);
        // Linear-to-u8 bridge — uses the same 3-binding effects BGL.
        // Input: array<vec4<f32>>. Output: array<u32> (packed u8 RGBA).
        // Output buffer is 4× smaller than input; readback dominates
        // wall time on this op so the smaller output is the win.
        let bridge_u8_pipeline = create_pipeline(&device, &pipeline_layout, "bridge_u8", SHADER_LINEAR_TO_U8);
        // Same bind-group layout (3-binding: input, output, params),
        // shader flipped to read u8 and write linear.
        let bridge_u8_to_linear_pipeline = create_pipeline(
            &device, &pipeline_layout, "bridge_u8_to_linear", SHADER_U8_TO_LINEAR,
        );
        // Both HDR kernels share the colourspace prelude with the P010
        // decoder, so the PQ/HLG curves on either side are one definition.
        let hdr10_src = format!(
            "{}{}{}",
            crate::color_wgsl::WGSL_PRELUDE,
            crate::color_wgsl::WGSL_HELPERS,
            SHADER_HDR10_TO_LINEAR_BODY,
        );
        let bridge_hdr10_pipeline = create_pipeline(
            &device, &pipeline_layout, "bridge_hdr10_to_linear", &hdr10_src,
        );
        let display_u8_pipeline = create_pipeline(
            &device, &pipeline_layout, "linear_to_display_u8", SHADER_LINEAR_TO_DISPLAY_U8,
        );

        let pipeline_count = 63;
        info!(
            "GPU effects: compiled {} pipelines in {:.0}ms",
            pipeline_count,
            pipeline_compile_start.elapsed().as_secs_f64() * 1000.0,
        );

        // Check if any shader compilation failed
        if let Some(err) = pollster::block_on(device.pop_error_scope()) {
            warn!("GPU effects: shader compilation failed: {err} — falling back to CPU");
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
        let params_ring: Vec<wgpu::Buffer> = (0..CHAIN_PARAMS_RING_SIZE)
            .map(|i| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("chain_params_{i}")),
                    size: PARAMS_SIZE,
                    usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                })
            })
            .collect();

        info!("GPU effects pipeline initialized");

        Some(Self {
            device,
            queue,
            opacity_pipeline,
            color_fade_pipeline,
            blur_h_pipeline,
            blur_v_pipeline,
            sharpen_pipeline,
            crop_pipeline,
            transform_pipeline,
            chroma_key_pipeline,
            bloom_threshold_pipeline,
            bloom_blend_pipeline,
            glow_blend_pipeline,
            light_rays_pipeline,
            vignette_pipeline,
            lens_flare_pipeline,
            anamorphic_streak_pipeline,
            film_halation_blend_pipeline,
            invert_pipeline,
            posterize_pipeline,
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
            letterbox_pipeline,
            chromatic_aberration_pipeline,
            rsc_pipeline,
            rsc_row_buf: None,
            rsc_row_buf_size: 0,
            stabilization_pipeline,
            stabilization_buf: None,
            stabilization_buf_size: 0,
            color_grade_pipeline,
            huesat_pipeline,
            huesat_bgl,
            huesat_lut_buf,
            lut3d_pipeline,
            lut3d_buf: None,
            lut3d_buf_size: 0,
            vibrance_pipeline,
            color_warper_pipeline,
            input_transform_pipeline,
            input_transform_lut_buf,
            clarity_blur_h_pipeline,
            clarity_blur_v_pipeline,
            clarity_apply_pipeline,
            mask_cutout_pipeline,
            mask_pipeline,
            color_balance_pipeline,
            wheel_sat_bri_pipeline,
            rgb_curves_pipeline,
            rgb_curves_lut_buf,
            qualifier_pipeline,
            luma_key_pipeline,
            soft_clip_pipeline,
            grain_pipeline,
            alpha_blur_h_pipeline,
            alpha_blur_v_pipeline,
            alpha_dilate_pipeline,
            transition_pipeline,
            bridge_u8_pipeline,
            bridge_u8_to_linear_pipeline,
            bridge_hdr10_pipeline,
            display_u8_pipeline,
            display_u8_staging: None,
            bridge_u8_out_buf: None,
            bridge_u8_out_size: 0,
            bridge_u8_staging_buf: None,
            bridge_u8_staging_size: 0,
            bind_group_layout,
            transition_bgl,
            buf_a: None,
            buf_b: None,
            buf_c: None,
            matte_buf: None,
            mask_records_buf: None,
            mask_points_buf: None,
            staging_read: None,
            params_buf,
            params_ring,
            // Start empty; populated by `rebuild_chain_bind_group_pools`
            // after the first `ensure_buffers` call (buf_a/b/c don't
            // exist until then).
            chain_bg_3binding: (0..CHAIN_PARAMS_RING_SIZE * 4).map(|_| None).collect(),
            chain_bg_huesat: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_rgb_curves: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_input_transform: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_clarity_apply: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_mask_cutout: (0..CHAIN_PARAMS_RING_SIZE * 2).map(|_| None).collect(),
            chain_bg_build_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            chain_encoder: None,
            chain_slot_idx: 0,
            current_buf_size: 0,
            device_lost,
            submit_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            luma_coefs: [0.2126, 0.7152, 0.0722],
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
                            resource: self.params_ring[slot].as_entire_binding(),
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
                        wgpu::BindGroupEntry { binding: 2, resource: self.params_ring[slot].as_entire_binding() },
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
                        wgpu::BindGroupEntry { binding: 2, resource: self.params_ring[slot].as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: self.rgb_curves_lut_buf.as_entire_binding() },
                    ],
                });
                self.chain_bg_build_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.chain_bg_rgb_curves[idx] = Some(bg_rgb_curves);

                let bg_input_transform = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("chain_bg_input_transform"),
                    layout: &self.huesat_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: self.params_ring[slot].as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: self.input_transform_lut_buf.as_entire_binding() },
                    ],
                });
                self.chain_bg_build_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                self.chain_bg_input_transform[idx] = Some(bg_input_transform);

                let bg_clarity_apply = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("chain_bg_clarity_apply"),
                    layout: &self.huesat_bgl,
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: self.params_ring[slot].as_entire_binding() },
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
                        wgpu::BindGroupEntry { binding: 2, resource: self.params_ring[slot].as_entire_binding() },
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
                    resource: self.params_buf.as_entire_binding(),
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
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        exposure: f32,
        contrast: f32,
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
        hue: f32,
        global_mix: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue
            .write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

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
            _pad0: 0,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad1: 0,
        });
        self.dispatch(&self.color_grade_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply color balance (white balance, hue rotation, global mix) on GPU.
    /// `white_balance` is temperature / tint as a matrix on linear light
    /// (`klipp_color::white_balance::white_balance_matrix`).
    pub fn apply_color_balance_gpu(
        &mut self,
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        white_balance: [[f32; 3]; 3],
        hue: f32,
        global_mix: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue
            .write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuColorBalanceParams {
            width, height, wb: flat3(white_balance), hue, global_mix,
            _pad0: 0, _pad1: 0,
        });
        self.dispatch(&self.color_balance_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply per-wheel saturation & brightness adjustments on GPU.
    pub fn apply_wheel_sat_bri_gpu(
        &mut self,
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        lift_sat: f32, lift_bri: f32,
        gamma_sat: f32, gamma_bri: f32,
        gain_sat: f32, gain_bri: f32,
        offset_sat: f32, offset_bri: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue
            .write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&GpuWheelSatBriParams {
            width, height,
            lift_sat, lift_bri, gamma_sat, gamma_bri,
            gain_sat, gain_bri, offset_sat, offset_bri,
            _pad0: 0, _pad1: 0,
        });
        self.dispatch(&self.wheel_sat_bri_pipeline, buf_a, buf_b, workgroups);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply RGB curves on GPU using pre-computed LUTs.
    /// `lut_data` must be 4×256 f32 values (master, red, green, blue).
    pub fn apply_rgb_curves_gpu(
        &mut self,
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        lut_data: &[f32; 1024], // 4 × 256
        preserve_luma: bool,
        mix: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue
            .write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

        // Upload curve LUT data
        self.queue.write_buffer(&self.rgb_curves_lut_buf, 0, bytemuck::cast_slice(lut_data));

        self.write_params(&GpuRgbCurvesParams {
            width, height,
            preserve_luma: if preserve_luma { 1 } else { 0 },
            mix,
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
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.rgb_curves_lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.rgb_curves_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
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
        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(linear_rgba));
        self.write_params(&BridgeU8Params { width, height, _pad0: 0, _pad1: 0 });

        // Bind group: input(buf_a, read), output(out_buf, write), params(uniform).
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bridge_u8_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
            ],
        });

        // Single submit: dispatch compute + copy_buffer_to_buffer for
        // readback. Same one-submit pattern as flush_chain — halves
        // NVIDIA Vulkan driver overhead vs two separate submits.
        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("bridge_u8_enc"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("bridge_u8_pass"), timestamp_writes: None,
            });
            pass.set_pipeline(&self.bridge_u8_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        encoder.copy_buffer_to_buffer(out_buf, 0, staging, 0, out_bytes);
        self.queue.submit(std::iter::once(encoder.finish()));
        self.submit_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        mark_gpu_stage(GpuStage::LinearRgbaToU8);
        let slice = staging.slice(..out_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
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
    pub fn device(&self) -> std::sync::Arc<wgpu::Device> {
        std::sync::Arc::clone(&self.device)
    }

    /// Clone the Arc-wrapped queue, for writing into buffers made on
    /// [`Self::device`].
    pub fn queue(&self) -> std::sync::Arc<wgpu::Queue> {
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
        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(linear_rgba));
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
        if bytes_per_row % wgpu::COPY_BYTES_PER_ROW_ALIGNMENT != 0 {
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
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
            ],
        });
        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("encode_grade_to_texture") },
        );
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
        self.queue.submit(std::iter::once(encoder.finish()));
        self.submit_count.fetch_add(1, Ordering::Relaxed);
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
            layout: &self.bind_group_layout,
            entries: &[
                // binding 0: input u8 buffer (external, read-only)
                wgpu::BindGroupEntry { binding: 0, resource: input_u8.as_entire_binding() },
                // binding 1: output linear buffer (chain's buf_a, read-write)
                wgpu::BindGroupEntry { binding: 1, resource: buf_a.as_entire_binding() },
                // binding 2: params uniform
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
            ],
        });

        let workgroups = (pixel_count as u32).div_ceil(WORKGROUP_SIZE);
        let mut encoder = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("bridge_u8_to_linear") },
        );
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.bridge_u8_to_linear_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.submit_count.fetch_add(1, Ordering::Relaxed);
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
        if self.is_device_lost() { return false; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return true; }
        self.ensure_buffers(pixel_count * 16);
        if codes.size() < pixel_count * 4 { return false; }

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
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("bridge_hdr10_to_linear") },
        );
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.bridge_hdr10_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((pixel_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.submit_count.fetch_add(1, Ordering::Relaxed);
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
        if self.is_device_lost() { return None; }
        let pixel_count = (width as u64) * (height as u64);
        if pixel_count == 0 { return Some(Vec::new()); }
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
        if self.display_u8_staging.as_ref().map_or(true, |b| b.size() < out_bytes) {
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
        });
        let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("linear_to_display_u8_bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: src.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: out.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("linear_to_display_u8") },
        );
        {
            let mut pass = encoder.begin_compute_pass(
                &wgpu::ComputePassDescriptor { label: None, timestamp_writes: None },
            );
            pass.set_pipeline(&self.display_u8_pipeline);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((pixel_count as u32).div_ceil(WORKGROUP_SIZE), 1, 1);
        }
        encoder.copy_buffer_to_buffer(out, 0, staging, 0, out_bytes);
        self.queue.submit(std::iter::once(encoder.finish()));
        self.submit_count.fetch_add(1, Ordering::Relaxed);

        let slice = staging.slice(..out_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let bytes = slice.get_mapped_range().to_vec();
                staging.unmap();
                Some(bytes)
            }
            other => {
                self.mark_device_lost(&format!("display_u8 map_async failed: {other:?}"));
                None
            }
        }
    }

    pub fn apply_vibrance_gpu(&mut self, linear_rgba: Vec<f32>, width: u32, height: u32, amount: f32) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuVibranceParams { width, height, amount, _pad: 0 });
        self.dispatch(&self.vibrance_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply 12-sector color warper on GPU.
    pub fn apply_color_warper_gpu(&mut self, linear_rgba: Vec<f32>, width: u32, height: u32, hue_shifts: &[f32; 12], sat_shifts: &[f32; 12]) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        self.write_params(&GpuColorWarperParams {
            width, height, _pad0: 0, _pad1: 0,
            hue_shifts: *hue_shifts, sat_shifts: *sat_shifts,
        });
        self.dispatch(&self.color_warper_pipeline, buf_a, buf_b, workgroups);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply input transform (log-to-sRGB) on GPU using a pre-computed 1D LUT.
    pub fn apply_input_transform_gpu(&mut self, linear_rgba: Vec<f32>, width: u32, height: u32, lut_data: &[f32; 256]) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);
        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();
        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));
        self.queue.write_buffer(&self.input_transform_lut_buf, 0, bytemuck::cast_slice(lut_data));
        self.write_params(&GpuInputTransformParams { width, height, _pad0: 0, _pad1: 0 });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("input_transform_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.input_transform_lut_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.input_transform_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply clarity (midtone unsharp mask) on GPU using 3-pass approach.
    pub fn apply_clarity_gpu(&mut self, linear_rgba: Vec<f32>, width: u32, height: u32, amount: f32) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
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

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let radius = 20u32;

        // Pass 1: horizontal blur (input RGBA → buf_b luma)
        self.write_params(&GpuClarityBlurParams { width, height, radius, _pad: 0 });
        self.dispatch(&self.clarity_blur_h_pipeline, buf_a, buf_b, workgroups);

        // Pass 2: vertical blur (buf_b luma → buf_c blurred luma)
        self.dispatch(&self.clarity_blur_v_pipeline, buf_b, buf_c, workgroups);

        // Pass 3: apply unsharp mask (input RGBA + blurred luma → output RGBA)
        self.write_params(&GpuClarityApplyParams { width, height, amount, _pad: 0 });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clarity_apply_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: buf_c.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_pipeline(&self.clarity_apply_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);
        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply bilateral denoise on GPU. Used by the color grading Denoise node.
    pub fn apply_denoise_gpu(
        &mut self,
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        spatial_sigma: f32,
        color_sigma: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        self.write_params(&DenoiseParams {
            width, height, spatial_sigma, color_sigma,
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
                p3: m.p3, feather: m.feather, _pad0: 0, _pad1: 0,
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
        let records_bytes = records_bytes.max(48);  // one MaskRecord
        let points_bytes = points_bytes.max(8);     // one vec2<f32>
        let need_records = self.mask_records_buf.as_ref()
            .map_or(true, |b| b.size() < records_bytes);
        if need_records {
            self.mask_records_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("mask_records_buf"),
                size: records_bytes.next_power_of_two().max(48),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
        }
        let need_points = self.mask_points_buf.as_ref()
            .map_or(true, |b| b.size() < points_bytes);
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
            p3: 0.0, feather: 0.0, _pad0: 0, _pad1: 0,
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
        self.queue.write_buffer(records_buf, 0, records_bytes_to_write);
        self.queue.write_buffer(points_buf, 0, points_bytes_to_write);

        // write_params is &mut self elsewhere; inline the small
        // uniform write here so this helper stays &self.
        let params = GpuMaskParams {
            width, height,
            mask_count,
            _pad: 0,
        };
        self.queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        // Pull the bind group layout straight off the pipeline so
        // we don't have to keep a second handle around in the
        // pipeline struct for every dispatch.
        let bgl = self.mask_pipeline.get_bind_group_layout(0);
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("mask_v2_bg"),
            layout: &bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: dest.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: records_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: points_buf.as_entire_binding() },
            ],
        });
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("mask_v2_enc"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("mask_v2_pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.mask_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.device.poll(wgpu::Maintain::Poll);
    }

    /// Apply hue/sat curves on the GPU using pre-computed LUTs.
    /// `lut_data` must be 6×256 f32 values (6 curves, 256 entries each).
    pub fn apply_huesat_gpu(
        &mut self,
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        mix: f32,
        lut_data: &[f32; 1536], // 6 × 256
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

        // Upload LUT data
        self.queue.write_buffer(&self.huesat_lut_buf, 0, bytemuck::cast_slice(lut_data));

        // Write params
        self.write_params(&GpuHueSatParams {
            width,
            height,
            mix,
            _pad0: 0,
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
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.huesat_lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.huesat_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Apply a 3D LUT with trilinear interpolation on GPU.
    /// `lut_data` is RGB triples in B-major order, `lut_size` is the grid dimension (e.g. 33).
    pub fn apply_3d_lut_gpu(
        &mut self,
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        lut_data: &[f32],
        lut_size: u32,
        opacity: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        if opacity <= 0.0 {
            return linear_rgba;
        }

        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

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
        self.queue.write_buffer(lut_buf, 0, bytemuck::cast_slice(lut_data));

        self.write_params(&GpuLut3dParams {
            width,
            height,
            lut_size,
            opacity,
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lut3d_bg"),
            layout: &self.huesat_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: buf_a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: buf_b.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: lut_buf.as_entire_binding() },
            ],
        });

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None, timestamp_writes: None,
            });
            pass.set_pipeline(&self.lut3d_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
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
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        hue_center: f32, hue_width: f32, hue_soft: f32,
        sat_low: f32, sat_high: f32, sat_soft: f32,
        lum_low: f32, lum_high: f32, lum_soft: f32,
        invert: bool,
        sec_exposure: f32, sec_saturation: f32, sec_hue_shift: f32, sec_temperature: f32,
        matte_preview: bool,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

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
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
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
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        low: f32,
        high: f32,
        soft: f32,
        invert: bool,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

        self.write_params(&GpuLumaKeyParams {
            width,
            height,
            low,
            high,
            soft,
            invert: if invert { 1 } else { 0 },
            _pad0: 0,
            _pad1: 0,
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
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
        threshold: f32,
        knee: f32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

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
        linear_rgba: Vec<f32>,
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
            return linear_rgba;
        }
        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

        self.write_params(&GpuGrainParams {
            width,
            height,
            amount,
            size,
            shadows,
            highlights,
            frame_seed,
            monochrome: if monochrome { 1 } else { 0 },
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
    // `params_ring` via `next_params_slot`, and the pass binds THAT slot
    // rather than the shared `params_buf`.

    /// Start a new chain segment. Resets the slot ring and opens a fresh
    /// command encoder. If an encoder is already open (should not happen
    /// — executor pairs begin/submit in lock-step) we submit+drop it
    /// first so no recorded work is lost.
    pub fn begin_chain(&mut self) {
        if let Some(enc) = self.chain_encoder.take() {
            // Recover from a missed submit_chain. Should be unreachable
            // in well-formed callers.
            self.queue.submit(std::iter::once(enc.finish()));
            self.submit_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        self.chain_slot_idx = 0;
        let enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("chain_encoder"),
        });
        self.chain_encoder = Some(enc);
    }

    /// Finalise the current chain segment: submit the batched encoder if
    /// one is live. No-op when called without a prior `begin_chain`
    /// (covers short-circuit / device-lost paths).
    pub fn submit_chain(&mut self) {
        if let Some(enc) = self.chain_encoder.take() {
            self.queue.submit(std::iter::once(enc.finish()));
            self.submit_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

    /// Count of `queue.submit` calls since pipeline creation. Used by
    /// `chain_single_submit_per_segment` (klipp-nodes tests) to verify
    /// the chain path collapses per-node submits into one.
    pub fn submit_count(&self) -> u64 {
        self.submit_count.load(std::sync::atomic::Ordering::Relaxed)
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
        let idx = self.chain_slot_idx as usize;
        assert!(
            idx < CHAIN_PARAMS_RING_SIZE,
            "chain params ring exhausted ({idx} >= {CHAIN_PARAMS_RING_SIZE}); bump CHAIN_PARAMS_RING_SIZE",
        );
        self.chain_slot_idx += 1;
        idx
    }

    /// Write `data` to the params-ring slot at `slot_idx`. Paired with
    /// `next_params_slot` by every `*_chain` entry.
    fn write_chain_params<T: bytemuck::Pod>(&self, slot_idx: usize, data: &T) {
        let bytes = bytemuck::bytes_of(data);
        self.queue.write_buffer(&self.params_ring[slot_idx], 0, bytes);
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
            hue, global_mix, _pad0: 0,
            luma_yr: self.luma_coefs[0],
            luma_yg: self.luma_coefs[1],
            luma_yb: self.luma_coefs[2],
            _pad1: 0,
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
            _pad0: 0, _pad1: 0,
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
        self.queue.write_buffer(&self.rgb_curves_lut_buf, 0, bytemuck::cast_slice(lut_data));
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuRgbCurvesParams {
            width, height,
            preserve_luma: if preserve_luma { 1 } else { 0 },
            mix,
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
        self.write_chain_params(slot, &GpuVibranceParams { width, height, amount, _pad: 0 });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_3binding[slot * 4 + Self::chain_cfg_3b_ab(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.vibrance_pipeline, bind_group, workgroups);
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

    /// Dispatch-only input-transform (log→linear via 1D LUT) pass.
    pub fn apply_input_transform_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        lut_data: &[f32; 256],
    ) {
        if self.is_device_lost() {
            return;
        }
        self.queue.write_buffer(&self.input_transform_lut_buf, 0, bytemuck::cast_slice(lut_data));
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuInputTransformParams { width, height, _pad0: 0, _pad1: 0 });
        let workgroups = (width * height).div_ceil(WORKGROUP_SIZE);
        let bind_group = self.chain_bg_input_transform[slot * 2 + Self::chain_cfg_4b(in_is_a)]
            .as_ref()
            .expect(Self::CHAIN_BG_EXPECT);
        let enc = self.chain_encoder.as_mut().expect(Self::CHAIN_ENCODER_EXPECT);
        Self::record_compute_dispatch(enc, &self.input_transform_pipeline, bind_group, workgroups);
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
        let radius = 20u32;

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
        self.write_chain_params(blur_slot, &GpuClarityBlurParams { width, height, radius, _pad: 0 });
        let apply_slot = self.next_params_slot();
        self.write_chain_params(apply_slot, &GpuClarityApplyParams { width, height, amount, _pad: 0 });

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

    /// Dispatch-only bilateral denoise pass.
    pub fn apply_denoise_chain(
        &mut self,
        in_is_a: bool,
        width: u32,
        height: u32,
        spatial_sigma: f32,
        color_sigma: f32,
    ) {
        if self.is_device_lost() {
            return;
        }
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &DenoiseParams { width, height, spatial_sigma, color_sigma });
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
        lut_data: &[f32; 1536],
    ) {
        if self.is_device_lost() {
            return;
        }
        self.queue.write_buffer(&self.huesat_lut_buf, 0, bytemuck::cast_slice(lut_data));
        let slot = self.next_params_slot();
        self.write_chain_params(slot, &GpuHueSatParams { width, height, mix, _pad0: 0 });
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
        self.write_chain_params(slot, &GpuLut3dParams { width, height, lut_size, opacity });
        let lut_buf = self.lut3d_buf.as_ref().unwrap();
        self.queue.write_buffer(lut_buf, 0, bytemuck::cast_slice(lut_data));
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
                wgpu::BindGroupEntry { binding: 2, resource: self.params_ring[slot].as_entire_binding() },
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
        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(linear_rgba));
    }

    /// Download the final linear-f32 RGBA frame. `slot_a` selects `buf_a`
    /// when true, `buf_b` when false — the caller's ping-pong parity.
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
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("flush_chain_dl") })
        });
        encoder.copy_buffer_to_buffer(source_buf, 0, staging, 0, byte_size);
        self.queue.submit(std::iter::once(encoder.finish()));
        self.submit_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let slice = staging.slice(..byte_size);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });

        mark_gpu_stage(GpuStage::FlushChain);
        let result = match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let data = slice.get_mapped_range();
                let linear: &[f32] = bytemuck::cast_slice(&data);
                let result = linear.to_vec();
                drop(data);
                staging.unmap();
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

    /// Apply a chain of effects on the GPU. Returns the processed RGBA data.
    pub fn apply_effects(
        &mut self,
        effects: &[BuiltinEffect],
        linear_rgba: Vec<f32>,
        width: u32,
        height: u32,
    ) -> Vec<f32> {
        if self.is_device_lost() {
            return linear_rgba;
        }
        if effects.is_empty() {
            return linear_rgba;
        }

        let byte_size = (width * height * 16) as u64;
        self.ensure_buffers(byte_size);

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        // Upload frame data to buf_a (already linear-f32 — direct upload).
        self.queue
            .write_buffer(buf_a, 0, bytemuck::cast_slice(&linear_rgba));

        // Track which buffer has the current data (true = buf_a, false = buf_b)
        let mut current_in_a = true;
        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        for effect in effects {
            let (input, output) = if current_in_a {
                (buf_a, buf_b)
            } else {
                (buf_b, buf_a)
            };

            match effect {
                BuiltinEffect::Opacity { value } => {
                    if (*value - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&OpacityParams {
                        width,
                        height,
                        value: *value,
                        _pad: 0,
                    });
                    self.dispatch(&self.opacity_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::GaussianBlur { radius } => {
                    if *radius < 0.5 {
                        continue;
                    }
                    // 3-pass box blur to approximate Gaussian
                    let boxes = boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        if box_r == 0 {
                            continue;
                        }
                        let (inp, out) = if current_in_a {
                            (buf_a, buf_b)
                        } else {
                            (buf_b, buf_a)
                        };
                        // Horizontal pass
                        self.write_params(&BlurParams {
                            width,
                            height,
                            radius: box_r as i32,
                            _pad: 0,
                        });
                        self.dispatch(&self.blur_h_pipeline, inp, out, workgroups);
                        current_in_a = !current_in_a;

                        // Vertical pass
                        let (inp, out) = if current_in_a {
                            (buf_a, buf_b)
                        } else {
                            (buf_b, buf_a)
                        };
                        self.dispatch(&self.blur_v_pipeline, inp, out, workgroups);
                        current_in_a = !current_in_a;
                    }
                }
                BuiltinEffect::Sharpen { amount } => {
                    if amount.abs() < f32::EPSILON {
                        continue;
                    }
                    // Single-pass 3x3 unsharp mask — the shader samples neighbors directly
                    self.write_params(&SharpenParams {
                        width,
                        height,
                        amount: *amount,
                        _pad: 0,
                    });
                    self.dispatch(&self.sharpen_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Crop {
                    left,
                    right,
                    top,
                    bottom,
                    feather,
                } => {
                    if *left <= 0.0 && *right <= 0.0 && *top <= 0.0 && *bottom <= 0.0 {
                        continue;
                    }
                    self.write_params(&CropParams {
                        width,
                        height,
                        crop_left: *left * width as f32,
                        crop_right: (1.0 - *right) * width as f32,
                        crop_top: *top * height as f32,
                        crop_bottom: (1.0 - *bottom) * height as f32,
                        feather: *feather,
                        _pad: 0,
                    });
                    self.dispatch(&self.crop_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Transform {
                    position_x,
                    position_y,
                    scale_x,
                    scale_y,
                    rotation,
                    anchor_x,
                    anchor_y,
                    rotation_x,
                    rotation_y,
                    perspective,
                    focal_ref: _,
                } => {
                    if *position_x == 0.0
                        && *position_y == 0.0
                        && (*scale_x - 1.0).abs() < f32::EPSILON
                        && (*scale_y - 1.0).abs() < f32::EPSILON
                        && rotation.abs() < f32::EPSILON
                        && rotation_x.abs() < f32::EPSILON
                        && rotation_y.abs() < f32::EPSILON
                    {
                        continue;
                    }
                    let rot_rad = rotation.to_radians();
                    let cos_r = rot_rad.cos();
                    let sin_r = rot_rad.sin();
                    let inv_sx = if scale_x.abs() > f32::EPSILON {
                        1.0 / scale_x
                    } else {
                        1.0
                    };
                    let inv_sy = if scale_y.abs() > f32::EPSILON {
                        1.0 / scale_y
                    } else {
                        1.0
                    };
                    let rx_rad = rotation_x.to_radians();
                    let ry_rad = rotation_y.to_radians();
                    let focal = if *perspective > 0.001 { width as f32 / *perspective } else { 0.0 };
                    self.write_params(&TransformParams {
                        width,
                        height,
                        anchor_x: *anchor_x * width as f32,
                        anchor_y: *anchor_y * height as f32,
                        position_x: *position_x,
                        position_y: *position_y,
                        cos_r,
                        sin_r,
                        inv_sx,
                        inv_sy,
                        cos_rx: rx_rad.cos(),
                        sin_rx: rx_rad.sin(),
                        cos_ry: ry_rad.cos(),
                        sin_ry: ry_rad.sin(),
                        focal,
                        _pad: [0; 3],
                    });
                    self.dispatch(&self.transform_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::ChromaKey {
                    key_color,
                    tolerance,
                    soft_edge,
                    spill_method,
                    spill_strength,
                } => {
                    // Convert key color to CbCr
                    let rf = key_color[0] as f32;
                    let gf = key_color[1] as f32;
                    let bf = key_color[2] as f32;
                    let key_cb = 128.0 + (-0.168736 * rf - 0.331264 * gf + 0.5 * bf);
                    let key_cr = 128.0 + (0.5 * rf - 0.418688 * gf - 0.081312 * bf);

                    // Dominant key channel — same logic as the CPU
                    // path's `dominant_channel`. Inlined here to keep
                    // the GPU encoder self-contained.
                    let key_idx: u32 = if key_color[1] >= key_color[0] && key_color[1] >= key_color[2] {
                        1
                    } else if key_color[2] > key_color[0] {
                        2
                    } else {
                        0
                    };
                    let method_code: u32 = match spill_method {
                        crate::SpillMethod::None => 0,
                        crate::SpillMethod::ChannelClamp => 1,
                        crate::SpillMethod::Hawk => 2,
                    };

                    self.write_params(&ChromaKeyParams {
                        width,
                        height,
                        key_cb,
                        key_cr,
                        tolerance: *tolerance,
                        soft_edge: *soft_edge,
                        spill_method: method_code,
                        spill_strength: *spill_strength,
                        spill_key_idx: key_idx,
                        _pad: [0; 3],
                    });
                    self.dispatch(&self.chroma_key_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Bloom { threshold, radius, intensity } => {
                    // Step 1: Extract bright pixels to buf_c
                    let buf_c = self.buf_c.as_ref().unwrap();
                    self.write_params(&BloomThresholdParams {
                        width, height,
                        threshold: *threshold,
                        _pad: 0,
                    });
                    self.dispatch(&self.bloom_threshold_pipeline, input, buf_c, workgroups);

                    // Step 2: Blur the bright pixels (3-pass box blur in buf_c)
                    let boxes = crate::blur::boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }

                    // Step 3: Additive blend blurred bright (buf_c) onto input
                    self.write_params(&BloomBlendParams {
                        width, height,
                        intensity: *intensity,
                        desat: 0.0,
                    });
                    // bloom_blend reads input + buf_c, writes output
                    self.dispatch_bloom_blend(input, buf_c, output, workgroups, *intensity, width, height);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Glow { radius, intensity } => {
                    // Step 1: Blur entire image to buf_c
                    let buf_c = self.buf_c.as_ref().unwrap();
                    // Copy input to buf_c first
                    let byte_size = (width * height * 16) as u64;
                    {
                        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                        enc.copy_buffer_to_buffer(input, 0, buf_c, 0, byte_size);
                        self.queue.submit(Some(enc.finish()));
                        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
                        // queue drains between submits. See submit_chain for context.
                        self.device.poll(wgpu::Maintain::Poll);
                    }

                    let boxes = crate::blur::boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }

                    // Step 2: Screen blend blurred (buf_c) with original (input) → output
                    self.dispatch_bloom_blend_screen(input, buf_c, output, workgroups, *intensity, 0.0, width, height);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::LightRays { center_x, center_y, threshold, samples, decay, intensity } => {
                    self.write_params(&LightRaysParams {
                        width, height,
                        center_x: *center_x,
                        center_y: *center_y,
                        threshold: *threshold,
                        samples: (*samples).clamp(4, 128),
                        decay: *decay,
                        intensity: *intensity,
                    });
                    self.dispatch(&self.light_rays_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Vignette { amount, size, softness, roundness } => {
                    self.write_params(&VignetteParams {
                        width, height,
                        amount: *amount, size: *size, softness: *softness, roundness: *roundness,
                        _pad: 0, _pad2: 0,
                    });
                    self.dispatch(&self.vignette_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Diffusion { amount, radius, threshold, desaturation } => {
                    // Same as bloom: threshold → blur → screen blend
                    let buf_c = self.buf_c.as_ref().unwrap();
                    self.write_params(&BloomThresholdParams { width, height, threshold: *threshold, _pad: 0 });
                    self.dispatch(&self.bloom_threshold_pipeline, input, buf_c, workgroups);
                    let boxes = crate::blur::boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }
                    self.dispatch_bloom_blend_screen(input, buf_c, output, workgroups, *amount, *desaturation, width, height);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::LensFlare { position_x, position_y, brightness, flare_size, ghost_count, ghost_intensity, chromatic_aberration } => {
                    self.write_params(&LensFlareParams {
                        width, height,
                        pos_x: *position_x, pos_y: *position_y,
                        brightness: *brightness, flare_size: *flare_size,
                        ghost_count: *ghost_count, ghost_intensity: *ghost_intensity,
                        chromatic_aberration: *chromatic_aberration,
                        _pad: 0, _pad2: 0, _pad3: 0,
                    });
                    self.dispatch(&self.lens_flare_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::LightWrap { width: wrap_w, intensity, softness } => {
                    // Blur → blend where dark
                    let buf_c = self.buf_c.as_ref().unwrap();
                    let byte_size = (width * height * 16) as u64;
                    {
                        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                        enc.copy_buffer_to_buffer(input, 0, buf_c, 0, byte_size);
                        self.queue.submit(Some(enc.finish()));
                        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
                        // queue drains between submits. See submit_chain for context.
                        self.device.poll(wgpu::Maintain::Poll);
                    }
                    let boxes = crate::blur::boxes_for_gaussian(*wrap_w * *softness);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }
                    self.dispatch_bloom_blend_screen(input, buf_c, output, workgroups, *intensity, 0.0, width, height);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::AnamorphicStreak { threshold, length, intensity, tint } => {
                    self.write_params(&AnamorphicStreakParams {
                        width, height,
                        threshold: *threshold,
                        half_len: (*length * 0.5) as u32,
                        intensity: *intensity,
                        tint: *tint,
                        _pad: 0, _pad2: 0,
                    });
                    self.dispatch(&self.anamorphic_streak_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::FilmHalation { threshold, radius, intensity, hue } => {
                    // Extract bright with halation color → blur → screen blend
                    let buf_c = self.buf_c.as_ref().unwrap();
                    let hal_r = 1.0f32;
                    let hal_g = *hue * 0.6;
                    let hal_b = *hue * *hue * 0.2;
                    // Use bloom threshold to extract brights (with color tint applied after)
                    self.write_params(&BloomThresholdParams { width, height, threshold: *threshold, _pad: 0 });
                    self.dispatch(&self.bloom_threshold_pipeline, input, buf_c, workgroups);
                    let boxes = crate::blur::boxes_for_gaussian(*radius);
                    for &box_r in &boxes {
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_h_pipeline, buf_c, output, workgroups);
                        self.write_params(&BlurParams { width, height, radius: box_r as i32, _pad: 0 });
                        self.dispatch(&self.blur_v_pipeline, output, buf_c, workgroups);
                    }
                    // Screen blend with halation tint
                    self.write_params(&FilmHalationBlendParams {
                        width, height, threshold: *threshold, intensity: *intensity,
                        hal_r, hal_g, hal_b, _pad: 0,
                    });
                    let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("halation_bg"),
                        layout: &self.transition_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: buf_c.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
                        ],
                    });
                    let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    { let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                      pass.set_pipeline(&self.film_halation_blend_pipeline);
                      pass.set_bind_group(0, &bind_group, &[]);
                      pass.dispatch_workgroups(workgroups, 1, 1);
                    }
                    self.queue.submit(Some(enc.finish()));
                    // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
                    // queue drains between submits. See submit_chain for context.
                    self.device.poll(wgpu::Maintain::Poll);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Invert { mix } => {
                    if (*mix).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&InvertParams {
                        width, height,
                        mix: *mix,
                        _pad: 0,
                    });
                    self.dispatch(&self.invert_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Posterize { levels } => {
                    self.write_params(&PosterizeParams {
                        width, height,
                        levels: *levels,
                        _pad: 0,
                    });
                    self.dispatch(&self.posterize_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Mosaic { block_size } => {
                    if *block_size <= 1.0 {
                        continue;
                    }
                    self.write_params(&MosaicParams {
                        width, height,
                        block_size: *block_size,
                        _pad: 0,
                    });
                    self.dispatch(&self.mosaic_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::EdgeDetect { strength, mix } => {
                    self.write_params(&EdgeDetectParams {
                        width, height,
                        strength: *strength,
                        mix: *mix,
                    });
                    self.dispatch(&self.edge_detect_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::ChannelMixer {
                    red_red, red_green, red_blue,
                    green_red, green_green, green_blue,
                    blue_red, blue_green, blue_blue,
                } => {
                    self.write_params(&ChannelMixerParams {
                        width, height,
                        rr: *red_red, rg: *red_green, rb: *red_blue,
                        gr: *green_red, gg: *green_green, gb: *green_blue,
                        br: *blue_red, bg: *blue_green, bb: *blue_blue,
                        _pad: 0,
                    });
                    self.dispatch(&self.channel_mixer_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::DirectionalBlur { angle, distance, samples } => {
                    if *distance < 0.5 {
                        continue;
                    }
                    let samples_clamped = (*samples).clamp(3, 32);
                    let angle_rad = angle.to_radians();
                    self.write_params(&DirectionalBlurParams {
                        width, height,
                        dx: angle_rad.cos() * *distance / samples_clamped as f32,
                        dy: angle_rad.sin() * *distance / samples_clamped as f32,
                        samples: samples_clamped,
                        _pad: 0, _pad2: 0, _pad3: 0,
                    });
                    self.dispatch(&self.directional_blur_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Emboss { angle, strength, mix } => {
                    let angle_rad = angle.to_radians();
                    self.write_params(&EmbossParams {
                        width, height,
                        dx: angle_rad.cos(),
                        dy: angle_rad.sin(),
                        strength: *strength,
                        mix: *mix,
                        _pad0: 0, _pad1: 0,
                    });
                    self.dispatch(&self.emboss_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Mirror { horizontal, vertical } => {
                    if *horizontal == 0 && *vertical == 0 {
                        continue;
                    }
                    self.write_params(&MirrorParams {
                        width, height,
                        horizontal: *horizontal,
                        vertical: *vertical,
                    });
                    self.dispatch(&self.mirror_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::LensDistortion { distortion, scale } => {
                    if distortion.abs() < f32::EPSILON && (*scale - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&LensDistortionParams {
                        width, height,
                        distortion: *distortion,
                        scale: *scale,
                    });
                    self.dispatch(&self.lens_distortion_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Dilate { radius } => {
                    self.write_params(&DilateErodeParams {
                        width, height,
                        radius: (*radius).clamp(1, 10),
                        _pad: 0,
                    });
                    self.dispatch(&self.dilate_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Erode { radius } => {
                    self.write_params(&DilateErodeParams {
                        width, height,
                        radius: (*radius).clamp(1, 10),
                        _pad: 0,
                    });
                    self.dispatch(&self.erode_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Denoise { spatial_sigma, color_sigma, iterations } => {
                    let iters = (*iterations).clamp(1, 3);
                    self.write_params(&DenoiseParams {
                        width, height,
                        spatial_sigma: *spatial_sigma,
                        color_sigma: *color_sigma,
                    });
                    // Multi-pass: ping-pong between buffers
                    for _iter in 0..iters {
                        let (inp, out) = if current_in_a {
                            (buf_a, buf_b)
                        } else {
                            (buf_b, buf_a)
                        };
                        self.dispatch(&self.denoise_pipeline, inp, out, workgroups);
                        current_in_a = !current_in_a;
                    }
                }
                BuiltinEffect::Gamma { gamma } => {
                    if (*gamma - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&GammaParams {
                        width, height,
                        inv_gamma: 1.0 / *gamma,
                        _pad: 0,
                    });
                    self.dispatch(&self.gamma_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::HSVAdjust { hue_shift, saturation, value } => {
                    if hue_shift.abs() < f32::EPSILON && (*saturation - 1.0).abs() < f32::EPSILON && (*value - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&HSVAdjustParams {
                        width, height,
                        hue_shift: *hue_shift,
                        saturation: *saturation,
                        value: *value,
                        _pad: 0, _pad2: 0, _pad3: 0,
                    });
                    self.dispatch(&self.hsv_adjust_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::NoiseGenerator { amount, monochrome, seed } => {
                    if *amount < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&NoiseGeneratorParams {
                        width, height,
                        amount: *amount,
                        monochrome: *monochrome,
                        seed: *seed,
                        _pad: 0, _pad2: 0, _pad3: 0,
                    });
                    self.dispatch(&self.noise_generator_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::MedianFilter { radius } => {
                    let r = (*radius).clamp(1, 2); // clamp to 2 on GPU for performance
                    self.write_params(&MedianFilterParams {
                        width, height,
                        radius: r,
                        _pad: 0,
                    });
                    self.dispatch(&self.median_filter_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } => {
                    if *amount < 0.01 {
                        continue;
                    }
                    let s = (*samples).clamp(4, 32);
                    self.write_params(&RadialBlurParams {
                        width, height,
                        center_x: *center_x,
                        center_y: *center_y,
                        amount: *amount,
                        samples: s,
                        mode: *mode,
                        _pad: 0,
                    });
                    self.dispatch(&self.radial_blur_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Levels { in_black, in_white, gamma, out_black, out_white } => {
                    if *in_black <= 0.0 && (*in_white - 1.0).abs() < f32::EPSILON
                        && (*gamma - 1.0).abs() < f32::EPSILON
                        && *out_black <= 0.0 && (*out_white - 1.0).abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&LevelsParams {
                        width, height,
                        in_black: *in_black,
                        in_white: *in_white,
                        inv_gamma: if gamma.abs() > f32::EPSILON { 1.0 / *gamma } else { 1.0 },
                        out_black: *out_black,
                        out_white: *out_white,
                        _pad: 0,
                    });
                    self.dispatch(&self.levels_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Equalize { strength } => {
                    // Hybrid GPU: build histogram/CDF on CPU (fast), remap on GPU (parallel).
                    // Step 1: Download current frame to compute histogram.
                    // The CDF of display-encoded luma, built as the CPU
                    // path builds it.
                    let current_buf = if current_in_a { buf_a } else { buf_b };
                    let frame_bytes = self.download(current_buf, byte_size);
                    let frame_data: &[f32] = bytemuck::cast_slice(&frame_bytes);

                    let cdf = crate::equalize_cdf(frame_data);

                    // Step 4: Upload CDF as storage buffer and dispatch GPU remap shader
                    let cdf_bytes = bytemuck::cast_slice(&cdf);
                    // Reuse huesat_lut_buf (1024 bytes = 256 × 4 fits in 6144 byte huesat buf)
                    self.queue.write_buffer(&self.huesat_lut_buf, 0, cdf_bytes);

                    self.write_params(&EqualizeParams {
                        width,
                        height,
                        strength: *strength,
                        _pad0: 0,
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
                            wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: self.huesat_lut_buf.as_entire_binding() },
                        ],
                    });

                    let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                        pass.set_pipeline(&self.equalize_pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.dispatch_workgroups(workgroups, 1, 1);
                    }
                    self.queue.submit(std::iter::once(encoder.finish()));
                    // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
                    // queue drains between submits. See submit_chain for context.
                    self.device.poll(wgpu::Maintain::Poll);

                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Letterbox { aspect_ratio, bar_color, opacity } => {
                    self.write_params(&LetterboxParams {
                        width, height,
                        aspect_ratio: *aspect_ratio,
                        bar_color: *bar_color,
                        opacity: *opacity,
                        _pad0: 0,
                        _pad1: 0,
                        _pad2: 0,
                    });
                    self.dispatch(&self.letterbox_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
                    if red_shift.abs() < f32::EPSILON && blue_shift.abs() < f32::EPSILON {
                        continue;
                    }
                    self.write_params(&ChromaticAberrationParams {
                        width, height,
                        red_shift: *red_shift,
                        blue_shift: *blue_shift,
                        center_x: *center_x,
                        center_y: *center_y,
                        _pad0: 0,
                        _pad1: 0,
                    });
                    self.dispatch(&self.chromatic_aberration_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::RollingShutterCorrection { strength, .. } => {
                    if *strength < f32::EPSILON || !self.has_rsc_data() {
                        continue; // No correction or no motion data ready
                    }
                    self.write_params(&RscParams { width, height, strength: *strength, _pad: 0 });
                    let input = if current_in_a { buf_a } else { buf_b };
                    let output = if current_in_a { buf_b } else { buf_a };
                    let row_buf = self.rsc_row_buf.as_ref().unwrap();
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("rsc_bg"),
                        layout: &self.huesat_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: row_buf.as_entire_binding() },
                        ],
                    });
                    let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    {
                        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                        pass.set_pipeline(&self.rsc_pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.dispatch_workgroups(workgroups, 1, 1);
                    }
                    self.queue.submit(std::iter::once(enc.finish()));
                    // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
                    // queue drains between submits. See submit_chain for context.
                    self.device.poll(wgpu::Maintain::Poll);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::VideoStabilization { crop_ratio, zoom, .. } => {
                    if !self.has_stabilization_data() { continue; }
                    let crop_zoom = if *zoom { 1.0 / (1.0 - 2.0 * crop_ratio) } else { 1.0 };
                    self.write_params(&StabilizationParams { width, height, crop_zoom, _pad: 0 });
                    let input = if current_in_a { buf_a } else { buf_b };
                    let output = if current_in_a { buf_b } else { buf_a };
                    let hom_buf = self.stabilization_buf.as_ref().unwrap();
                    let bg = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("stabilization_bg"),
                        layout: &self.huesat_bgl,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 2, resource: self.params_buf.as_entire_binding() },
                            wgpu::BindGroupEntry { binding: 3, resource: hom_buf.as_entire_binding() },
                        ],
                    });
                    let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    {
                        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                        pass.set_pipeline(&self.stabilization_pipeline);
                        pass.set_bind_group(0, &bg, &[]);
                        pass.dispatch_workgroups(workgroups, 1, 1);
                    }
                    self.queue.submit(std::iter::once(enc.finish()));
                    // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
                    // queue drains between submits. See submit_chain for context.
                    self.device.poll(wgpu::Maintain::Poll);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Vibrance { amount } => {
                    self.write_params(&GpuVibranceParams { width, height, amount: *amount, _pad: 0 });
                    self.dispatch(&self.vibrance_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::SpeedRamp { .. } => {
                    // SpeedRamp is handled at the engine level (frame queue + GPU blend),
                    // not in the per-frame GPU effects chain. Pass through.
                }
                BuiltinEffect::ColorFade { color, progress, .. } => {
                    let t = progress.clamp(0.0, 1.0);
                    if t >= 0.9999 {
                        continue;
                    }
                    let lin = |c: u8| crate::cpu::decode(c as f32 / 255.0);
                    self.write_params(&ColorFadeParams {
                        width, height, progress: t, _pad: 0,
                        color: [lin(color[0]), lin(color[1]), lin(color[2]), color[3] as f32 / 255.0],
                    });
                    self.dispatch(&self.color_fade_pipeline, input, output, workgroups);
                    current_in_a = !current_in_a;
                }
                BuiltinEffect::Frei0r { .. } => {
                    // Frei0r 1.x has no GPU surface; chain_supports_gpu returns
                    // false on any chain containing a Frei0r entry, so the engine
                    // falls back to apply_effects_with_time on CPU. Reaching this
                    // arm means dispatcher policy was bypassed — passthrough
                    // rather than producing visually-broken output.
                }
            }
        }

        // Download result from GPU as linear-f32 directly. The caller
        // owns the boundary — engine paths stay linear, export / scope
        // paths bridge at their own seam.
        let final_buf = if current_in_a { buf_a } else { buf_b };
        self.download_linear_rgba_f32(final_buf, (width * height) as usize)
    }

    /// Blur an alpha mask on the GPU. Input/output are single-channel u8 buffers.
    /// Uses 3-pass box blur to approximate Gaussian, same as the RGBA blur.
    pub fn blur_alpha_gpu(&mut self, mask: &[u8], w: u32, h: u32, radius: f32) -> Vec<u8> {
        let pixel_count = (w * h) as u64;
        // Each alpha value stored as one u32
        let byte_size = pixel_count * 4;
        self.ensure_buffers(byte_size);

        // Convert u8 alpha -> u32 for GPU
        let input_u32: Vec<u32> = mask.iter().map(|&a| a as u32).collect();

        let buf_a = self.buf_a.as_ref().unwrap();
        let buf_b = self.buf_b.as_ref().unwrap();

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&input_u32));

        let workgroups = (w * h).div_ceil(WORKGROUP_SIZE);

        // 3-pass box blur for gaussian approximation
        let boxes = boxes_for_gaussian(radius);
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

        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(&input_u32));

        let workgroups = (w * h).div_ceil(WORKGROUP_SIZE);

        self.write_params(&AlphaDilateParams { width: w, height: h, radius: r, _pad: 0 });
        self.dispatch(&self.alpha_dilate_pipeline, buf_a, buf_b, workgroups);

        let result_bytes = self.download(buf_b, byte_size);
        let result_u32: &[u32] = bytemuck::cast_slice(&result_bytes);
        result_u32.iter().map(|&v| v.min(255) as u8).collect()
    }

    fn write_params<T: bytemuck::Pod>(&self, data: &T) {
        let bytes = bytemuck::bytes_of(data);
        self.queue.write_buffer(&self.params_buf, 0, bytes);
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
        self.queue.submit(std::iter::once(encoder.finish()));
    }

    pub fn upload_rsc_row_transforms(&mut self, row_transforms: &[crate::rolling_shutter::RowTransform]) {
        let byte_size = (row_transforms.len() * std::mem::size_of::<crate::rolling_shutter::RowTransform>()) as u64;
        if self.rsc_row_buf.as_ref().map_or(true, |_| self.rsc_row_buf_size < byte_size) {
            self.rsc_row_buf = Some(self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rsc_row_transforms"),
                size: byte_size,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.rsc_row_buf_size = byte_size;
        }
        self.queue.write_buffer(self.rsc_row_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(row_transforms));
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
        self.queue.write_buffer(self.stabilization_buf.as_ref().unwrap(), 0, bytemuck::cast_slice(h));
    }

    pub fn has_stabilization_data(&self) -> bool {
        self.stabilization_buf.is_some()
    }

    /// Download `byte_size` raw bytes from a storage buffer. Kept for
    /// buffers that legitimately store bytes (for example the u32
    /// scope image buffer) — pixel buffers now hold linear-f32 vec4s
    /// and callers should use [`download_linear_rgba_as_u8`] or
    /// [`download_linear_rgba`] instead.
    fn download(&self, source: &wgpu::Buffer, byte_size: u64) -> Vec<u8> {
        if self.is_device_lost() {
            return vec![0u8; byte_size as usize];
        }
        let staging = self.staging_read.as_ref().unwrap();
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(source, 0, staging, 0, byte_size);
        self.queue.submit(std::iter::once(encoder.finish()));
        self.submit_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let buffer_slice = staging.slice(..byte_size);
        let (tx, rx) = std::sync::mpsc::channel();
        buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });

        mark_gpu_stage(GpuStage::FlushChain);
        let result = match poll_until_mapped(&self.device, &rx) {
            Ok(Ok(())) => {
                let data = buffer_slice.get_mapped_range();
                let result = data.to_vec();
                drop(data);
                staging.unmap();
                result
            }
            other => {
                // `map_async` signalled an error (or the channel dropped
                // without delivering one). That means the GPU state is
                // suspect: either the buffer wasn't mapped or the device
                // is on its way out. Flip the lost flag so subsequent
                // calls short-circuit before they touch the device.
                self.mark_device_lost(&format!("buffer map_async failed: {other:?}"));
                vec![0u8; byte_size as usize]
            }
        };
        mark_gpu_stage(GpuStage::Idle);
        result
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
        let bytes = self.download(source, byte_size);
        if bytes.len() < pixel_count * 16 {
            // Device-lost fallback: `download` returned zero bytes.
            return vec![0.0f32; pixel_count * 4];
        }
        let linear: &[f32] = bytemuck::cast_slice(&bytes);
        linear.to_vec()
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
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
            ],
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.transition_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_as_u8(buf_b, (width * height) as usize)
    }

    /// Linear-f32 input/output transition. Identical shader work to
    /// [`render_transition`] but skips the sRGB encode on the output
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
        self.queue.write_buffer(buf_a, 0, bytemuck::cast_slice(from_linear));
        self.queue.write_buffer(buf_c, 0, bytemuck::cast_slice(to_linear));

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
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
            ],
        });

        let pixel_count = width * height;
        let workgroups = pixel_count.div_ceil(WORKGROUP_SIZE);

        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("render_transition_linear"),
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.transition_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.device.poll(wgpu::Maintain::Poll);

        self.download_linear_rgba_f32(buf_b, (width * height) as usize)
    }

    /// Dispatch bloom blend: additive blend of bright (buf_c) onto original (input) → output
    fn dispatch_bloom_blend(
        &self, input: &wgpu::Buffer, bright: &wgpu::Buffer, output: &wgpu::Buffer,
        workgroups: u32, intensity: f32, width: u32, height: u32,
    ) {
        self.write_params(&BloomBlendParams { width, height, intensity, desat: 0.0 });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("bloom_blend_bg"),
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: bright.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
            ],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.bloom_blend_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(Some(enc.finish()));
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);
    }

    /// Dispatch glow blend: screen blend of blurred (buf_c) onto original (input) → output
    #[allow(clippy::too_many_arguments)]
    fn dispatch_bloom_blend_screen(
        &self, input: &wgpu::Buffer, blurred: &wgpu::Buffer, output: &wgpu::Buffer,
        workgroups: u32, intensity: f32, desat: f32, width: u32, height: u32,
    ) {
        self.write_params(&BloomBlendParams { width, height, intensity, desat });
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glow_blend_bg"),
            layout: &self.transition_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: blurred.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: output.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: self.params_buf.as_entire_binding() },
            ],
        });
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.glow_blend_pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(workgroups, 1, 1);
        }
        self.queue.submit(Some(enc.finish()));
        // Non-blocking driver nudge so NVIDIA Vulkan's deferred-destruction
        // queue drains between submits. See submit_chain for context.
        self.device.poll(wgpu::Maintain::Poll);
    }
}

// ─── Param structs (must be Pod for bytemuck) ────────────────────────────────

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
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct BloomBlendParams {
    width: u32,
    height: u32,
    intensity: f32,
    /// Desaturation of the blended layer (screen blend only; Diffusion).
    desat: f32,
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
    _pad0: u32,
    /// Working-space luma coefficients used by the saturation /
    /// tone-controls / lift-gamma-gain stages of `SHADER_COLOR_GRADE`.
    /// `(0.2126, 0.7152, 0.0722)` for Rec709Linear,
    /// `(0.2722, 0.6741, 0.0537)` for AcesCG (AP1), etc. Populated by
    /// the engine via `GpuEffectsPipeline::set_luma_coefs`.
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
    _pad1: u32,
}
// 37 words = 148 bytes (still under 256-byte PARAMS_SIZE)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuHueSatParams {
    width: u32,
    height: u32,
    mix: f32,
    _pad0: u32,
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
    _pad0: u32,
    _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuRgbCurvesParams {
    width: u32,
    height: u32,
    preserve_luma: u32,
    mix: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuVibranceParams {
    width: u32, height: u32, amount: f32, _pad: u32,
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
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuClarityApplyParams {
    width: u32, height: u32, amount: f32, _pad: u32,
}

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
/// `u32`/`f32` slots = 48 bytes, naturally 16-byte aligned for
/// `array<MaskRecord>` in std430).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskRecord {
    shape_type: u32,     // 0=circle, 1=rect, 2=gradient, 3=bezier
    flags: u32,          // bit 0 invert, bits 8-15 opacity (0-255)
    bezier_offset: u32,  // index into points buffer (bezier only)
    bezier_count: u32,   // number of points (bezier only)
    cx: f32, cy: f32, p1: f32, p2: f32,
    p3: f32, feather: f32, _pad0: u32, _pad1: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuLut3dParams {
    width: u32,
    height: u32,
    lut_size: u32,
    opacity: f32,
}
// 4 fields × 4 bytes = 16 bytes

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
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}
// 20 fields × 4 bytes = 80 bytes (well under 256-byte PARAMS_SIZE)

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuLumaKeyParams {
    width: u32,
    height: u32,
    low: f32,
    high: f32,
    soft: f32,
    invert: u32,
    _pad0: u32,
    _pad1: u32,
}
// 8 fields × 4 bytes = 32 bytes (well under 256-byte PARAMS_SIZE)

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
    _pad: u32,
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
struct PosterizeParams {
    width: u32,
    height: u32,
    levels: f32,
    _pad: u32,
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
    color_sigma: f32,
}
// 4 fields x 4 bytes = 16 bytes

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
}
// 4 fields x 4 bytes = 16 bytes

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

// ─── Box blur Gaussian approximation (same as CPU version) ───────────────────

fn boxes_for_gaussian(sigma: f32) -> [usize; 3] {
    let w_ideal = ((12.0 * sigma * sigma / 3.0) + 1.0).sqrt();
    let mut wl = w_ideal.floor() as usize;
    if wl.is_multiple_of(2) {
        wl = wl.saturating_sub(1).max(1);
    }
    let wu = wl + 2;
    let m_ideal = (12.0 * sigma * sigma
        - 3.0 * (wl * wl) as f32
        - 12.0 * wl as f32
        - 9.0)
        / (-4.0 * wl as f32 - 4.0);
    let m = m_ideal.round() as usize;
    let r = |size: usize| (size - 1) / 2;
    [
        r(if 0 < m { wl } else { wu }),
        r(if 1 < m { wl } else { wu }),
        r(if 2 < m { wl } else { wu }),
    ]
}

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
macro_rules! shader_linear_helpers_wgsl {
    () => {
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

fn unpack(v: vec4<f32>) -> vec4<f32> { return v; }
fn pack(v: vec4<f32>) -> vec4<f32> { return v; }

fn enc3(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(linear_to_srgb_component(v.x), linear_to_srgb_component(v.y), linear_to_srgb_component(v.z));
}
fn dec3(v: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(srgb_to_linear_component(v.x), srgb_to_linear_component(v.y), srgb_to_linear_component(v.z));
}
"
    };
}

// Opacity — multiply linear RGBA by a scalar. In linear light,
// opacity 0.5 means "half the photons" (matching DaVinci / Nuke);
// the sRGB u8 version multiplied gamma-encoded bytes, which
// produced visibly darker-than-half output.
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
const SHADER_COLOR_FADE: &str = "
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
    output[idx] = mix(params.color, input[idx], params.progress);
}
";

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

const SHADER_CROP: &str = "
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
        output[idx] = vec4<f32>(0.0);
        return;
    }

    var pixel = input[idx];

    if params.feather > 0.0 {
        let dist = min(
            min(xf - params.crop_left, params.crop_right - 1.0 - xf),
            min(yf - params.crop_top, params.crop_bottom - 1.0 - yf),
        );
        if dist < params.feather {
            let factor = dist / params.feather;
            pixel.w = pixel.w * factor;
        }
    }

    output[idx] = pixel;
}
";

const SHADER_TRANSFORM: &str = concat!(
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

    // 3D rotation + perspective
    if params.cos_rx != 1.0 || params.cos_ry != 1.0 {
        let y1 = sy * params.cos_rx;
        let z1 = sy * params.sin_rx;
        let x2 = sx * params.cos_ry + z1 * params.sin_ry;
        let z2 = -sx * params.sin_ry + z1 * params.cos_ry;
        if params.focal > 0.0 {
            let w_f = 1.0 + z2 / params.focal;
            if w_f > 0.01 {
                sx = x2 / w_f;
                sy = y1 / w_f;
            }
        } else {
            sx = x2;
            sy = y1;
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
    output[idx] = mix(top, bot, fy);
}

fn sample_pixel(x: i32, y: i32) -> vec4<f32> {
    if x < 0 || y < 0 || x >= i32(params.width) || y >= i32(params.height) {
        return vec4<f32>(0.0);
    }
    let src_idx = u32(y) * params.width + u32(x);
    return input[src_idx];
}
"
);

// Chroma-key math lives in sRGB YCbCr space (that's where `key_cb` /
// `key_cr` are defined — they're derived from the user-picked sRGB
// colour). Storage buffers are now linear-f32, so we re-encode to
// sRGB inside the shader for the YCbCr distance test, then write back
// the (possibly alpha-faded) linear pixel.
const SHADER_CHROMA_KEY: &str = "
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

    var pixel = input[idx];
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
        output[idx] = vec4<f32>(0.0);
        return;
    } else if dist < params.tolerance + params.soft_edge {
        let factor = (dist - params.tolerance) / params.soft_edge;
        pixel.w = pixel.w * factor;
    }

    // Spill suppression on whatever's still visible. Mirrors the CPU
    // `apply_spill` shape: pick a target value the key channel drops
    // toward when it exceeds that target, blend by spill_strength.
    if params.spill_method != 0u && params.spill_strength > 0.0 && pixel.w > 0.0 {
        // Work in sRGB 0-255 to match the CPU oracle byte-for-byte.
        var srgb = vec3<f32>(srgb_r, srgb_g, srgb_b);
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
            srgb[params.spill_key_idx] = clamp(new_key, 0.0, 255.0);
            // Re-encode the mutated sRGB triplet back to linear for output.
            pixel.x = srgb_to_linear_component(srgb.x / 255.0);
            pixel.y = srgb_to_linear_component(srgb.y / 255.0);
            pixel.z = srgb_to_linear_component(srgb.z / 255.0);
        }
    }

    output[idx] = pixel;
}
";

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
const SHADER_HDR10_TO_LINEAR_BODY: &str = "
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
    output[idx] = vec4<f32>(
        dot(params.m0.xyz, scene),
        dot(params.m1.xyz, scene),
        dot(params.m2.xyz, scene),
        1.0,
    );
}
";

// Chain output (linear f32 RGBA) → display sRGB u8, for a graded frame
// that nothing else needs on the CPU. With `hdr_tonemap` set it applies
// the HDR display tone map first — the GPU half of
// `klipp_color::hdr::apply_hdr_display_tonemap` — so an HDR clip's last
// step runs here rather than on a 132 MB CPU copy. Without it this is the
// SDR display encode (`ToneMapOp::None`: clamp, sRGB). Same layout as
// SHADER_LINEAR_TO_U8.
const SHADER_LINEAR_TO_DISPLAY_U8: &str = "
struct Params { width: u32, height: u32, hdr_tonemap: u32, exposure: f32 }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

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
    let px = input[idx];
    var rgb = max(px.xyz, vec3<f32>(0.0));
    if (params.hdr_tonemap != 0u) {
        rgb = vec3<f32>(filmic(rgb.x * params.exposure), filmic(rgb.y * params.exposure), filmic(rgb.z * params.exposure));
    }
    let r = u32(clamp(linear_to_srgb_c(rgb.x) * 255.0 + 0.5, 0.0, 255.0));
    let g = u32(clamp(linear_to_srgb_c(rgb.y) * 255.0 + 0.5, 0.0, 255.0));
    let b = u32(clamp(linear_to_srgb_c(rgb.z) * 255.0 + 0.5, 0.0, 255.0));
    let a = u32(clamp(px.w * 255.0 + 0.5, 0.0, 255.0));
    output[idx] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
";

const SHADER_LINEAR_TO_U8: &str = "
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
    let pixel = input[idx];
    // Encode RGB through sRGB transfer, alpha is linear (no gamma).
    let r = u32(clamp(linear_to_srgb_c(pixel.x) * 255.0 + 0.5, 0.0, 255.0));
    let g = u32(clamp(linear_to_srgb_c(pixel.y) * 255.0 + 0.5, 0.0, 255.0));
    let b = u32(clamp(linear_to_srgb_c(pixel.z) * 255.0 + 0.5, 0.0, 255.0));
    let a = u32(clamp(pixel.w * 255.0 + 0.5, 0.0, 255.0));
    // Pack RGBA into a single u32, little-endian (R in byte 0).
    output[idx] = r | (g << 8u) | (b << 16u) | (a << 24u);
}
";

// Opposite direction: packed sRGB-u8 RGBA (4 bytes per pixel, one u32)
// → linear-f32 vec4 RGBA. Reads from `input: array<u32>`, writes to
// `output: array<vec4<f32>>`. Same binding layout as SHADER_LINEAR_TO_U8
// so it reuses the pipeline's 3-binding bind_group_layout. Powers the
// phase-4d GPU-resident decode → grade path: the decoder's output
// buffer is plugged in as `input`, the grade chain's `buf_a` is the
// `output`.
const SHADER_U8_TO_LINEAR: &str = "
struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32 }
@group(0) @binding(0) var<storage, read> input: array<u32>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn srgb_to_linear_c(c: f32) -> f32 {
    if (c <= 0.04045) { return c / 12.92; }
    return pow((c + 0.055) / 1.055, 2.4);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let packed = input[idx];
    let r_u = (packed >> 0u) & 0xFFu;
    let g_u = (packed >> 8u) & 0xFFu;
    let b_u = (packed >> 16u) & 0xFFu;
    let a_u = (packed >> 24u) & 0xFFu;
    let r = f32(r_u) / 255.0;
    let g = f32(g_u) / 255.0;
    let b = f32(b_u) / 255.0;
    let a = f32(a_u) / 255.0;
    // sRGB transfer on RGB; alpha stays linear per sRGB spec.
    output[idx] = vec4<f32>(srgb_to_linear_c(r), srgb_to_linear_c(g), srgb_to_linear_c(b), a);
}
";

const SHADER_TRANSITION: &str = "
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
fn unpack(v: vec4<f32>) -> vec4<f32> { return v * 255.0; }
fn pack(v: vec4<f32>) -> vec4<f32> { return v / 255.0; }

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
        c00 = to_buf[y0 * params.width + x0];
        c10 = to_buf[y0 * params.width + x1];
        c01 = to_buf[y1 * params.width + x0];
        c11 = to_buf[y1 * params.width + x1];
    } else {
        c00 = from_buf[y0 * params.width + x0];
        c10 = from_buf[y0 * params.width + x1];
        c01 = from_buf[y1 * params.width + x0];
        c11 = from_buf[y1 * params.width + x1];
    }
    return unpack(mix(mix(c00, c10, tx), mix(c01, c11, tx), ty));
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let y = idx / params.width;
    let x = idx % params.width;
    let p = params.progress;

    let from_px = unpack(from_buf[idx]);
    let to_px = unpack(to_buf[idx]);

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
                result = unpack(from_buf[y * params.width + u32(src_x)]);
            } else {
                let in_x = u32(src_x + i32(params.width));
                result = unpack(to_buf[y * params.width + in_x]);
            }
        } else if dir == 2u {
            // Push Up: outgoing slides up, incoming enters from bottom
            let offset = u32(p * f32(params.height));
            let src_y = y + offset;
            if src_y < params.height {
                result = unpack(from_buf[src_y * params.width + x]);
            } else {
                let in_y = src_y - params.height;
                result = unpack(to_buf[in_y * params.width + x]);
            }
        } else if dir == 3u {
            // Push Down: outgoing slides down, incoming enters from top
            let offset = i32(p * f32(params.height));
            let src_y = i32(y) - offset;
            if src_y >= 0 {
                result = unpack(from_buf[u32(src_y) * params.width + x]);
            } else {
                let in_y = u32(src_y + i32(params.height));
                result = unpack(to_buf[in_y * params.width + x]);
            }
        } else {
            // Push Left (default): outgoing slides left, incoming enters from right
            let offset = u32(p * f32(params.width));
            let src_x = x + offset;
            if src_x < params.width {
                result = unpack(from_buf[y * params.width + src_x]);
            } else {
                let in_x = src_x - params.width;
                result = unpack(to_buf[y * params.width + in_x]);
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
            let fpx = unpack(from_buf[si]);
            let tpx = unpack(to_buf[si]);
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
        let from_block = unpack(from_buf[block_idx]);
        if params.param1 > 0.5 {
            let to_block = unpack(to_buf[block_idx]);
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
        let from_distorted = unpack(from_buf[sample_idx]);
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
        let r_val = lerp_pixel(unpack(from_buf[r_idx]), unpack(to_buf[r_idx]), p).x;
        let bx = clamp(f32(x) + b_ox, 0.0, f32(params.width) - 1.0);
        let by = clamp(f32(y) + b_oy, 0.0, f32(params.height) - 1.0);
        let b_idx = u32(by) * params.width + u32(bx);
        let b_val = lerp_pixel(unpack(from_buf[b_idx]), unpack(to_buf[b_idx]), p).z;
        result = vec4<f32>(r_val, base_g, b_val, 255.0);
    } else if params.kind_id == 24u {
        // Film Burn: hot-spot bleach toward burn color, with grain.
        // param0 = temperature, param1 = speed, param2 = grain
        let temp = params.param0;
        let speed = params.param1;
        let grain = params.param2;
        let burn_r = 1.0;
        let burn_g = 1.0 - temp * 0.35;
        let burn_b = 1.0 - temp * 0.75;
        let shaped_p = pow(p * speed, 0.6);
        let envelope = sin(min(shaped_p, 1.0) * 3.14159265);
        let hash = rand3(x, y, 1u);
        let hash2 = rand3(x, y, 2u);
        let pixel_thresh = hash * 0.6 + 0.2;
        let burn_t = clamp((envelope - pixel_thresh) / 0.3, 0.0, 1.0);
        let grain_val = (hash2 - 0.5) * grain * envelope;
        // unpack() returns 0..255 floats in this shader; normalize for the burn math.
        let base_rn = (from_px.x * (1.0 - p) + to_px.x * p) / 255.0;
        let base_gn = (from_px.y * (1.0 - p) + to_px.y * p) / 255.0;
        let base_bn = (from_px.z * (1.0 - p) + to_px.z * p) / 255.0;
        let rn = clamp(base_rn + (burn_r - base_rn) * burn_t + grain_val, 0.0, 1.0);
        let gn = clamp(base_gn + (burn_g - base_gn) * burn_t + grain_val, 0.0, 1.0);
        let bn = clamp(base_bn + (burn_b - base_bn) * burn_t + grain_val, 0.0, 1.0);
        result = vec4<f32>(rn * 255.0, gn * 255.0, bn * 255.0, 255.0);
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
                        out_color = unpack(from_buf[u32(y) * params.width + u32(src_x)]);
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
                        out_color = unpack(to_buf[u32(y) * params.width + u32(src_x)]);
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
                        out_color = unpack(from_buf[u32(src_y) * params.width + u32(x)]);
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
                        out_color = unpack(to_buf[u32(src_y) * params.width + u32(x)]);
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
                result = unpack(to_buf[sidx]);
            } else {
                result = unpack(from_buf[sidx]);
            }
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
        let from_s = unpack(from_buf[sample_idx]);
        let to_s = unpack(to_buf[sample_idx]);
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
        let r_val = lerp_pixel(unpack(from_buf[r_idx]), unpack(to_buf[r_idx]), p).x;
        let g_val = lerp_pixel(unpack(from_buf[g_idx]), unpack(to_buf[g_idx]), p).y;
        let b_val = lerp_pixel(unpack(from_buf[b_idx]), unpack(to_buf[b_idx]), p).z;
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
        let from_s = unpack(from_buf[sample_idx]);
        let to_s = unpack(to_buf[sample_idx]);
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
        let from_s = unpack(from_buf[sample_idx]);
        let to_s = unpack(to_buf[sample_idx]);
        let base = lerp_pixel(from_s, to_s, p);
        let luma = dot(base.xyz, vec3<f32>(0.2126, 0.7152, 0.0722));
        let chroma_shift = (band_hash2 - 0.5) * envelope * 15.0;
        let csx = clamp(f32(x) + chroma_shift, 0.0, f32(params.width) - 1.0);
        let c_idx = u32(y) * params.width + u32(csx);
        let from_c = unpack(from_buf[c_idx]);
        let to_c = unpack(to_buf[c_idx]);
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
        let from_s = unpack(from_buf[sample_idx]);
        let to_s = unpack(to_buf[sample_idx]);
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
        let base_g = lerp_pixel(unpack(from_buf[g_idx]), unpack(to_buf[g_idx]), p);
        let r_base = lerp_pixel(unpack(from_buf[r_idx]), unpack(to_buf[r_idx]), p);
        let b_base = lerp_pixel(unpack(from_buf[b_idx]), unpack(to_buf[b_idx]), p);
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

    output[idx] = pack(result);
}
";

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
    _pad0: u32,
    luma_yr: f32,
    luma_yg: f32,
    luma_yb: f32,
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
    var r = orig.x;
    var g = orig.y;
    var b = orig.z;

    // 1. Exposure (2^stops) — physically correct in linear.
    let exp_factor = pow(2.0, params.exposure);
    r = r * exp_factor;
    g = g * exp_factor;
    b = b * exp_factor;

    // 2. Tone controls. Band centres in linear: highlights 0.214,
    //    whites 0.604, shadows 0.214, blacks 0.033. Luma uses the
    //    project's working-space coefficients (BT.709 for Rec709Linear,
    //    AP1 for AcesCG).
    let luma1 = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
    let hi_w = max(luma1 - 0.214, 0.0) * (1.0 / (1.0 - 0.214));
    let lo_w = max(0.214 - luma1, 0.0) * (1.0 / 0.214);
    let white_w = max(luma1 - 0.604, 0.0) * (1.0 / (1.0 - 0.604));
    let black_w = max(0.033 - luma1, 0.0) * (1.0 / 0.033);
    let tone_adj = params.highlights * hi_w + params.shadows * lo_w + params.whites * white_w + params.blacks * black_w;
    r = r + tone_adj;
    g = g + tone_adj;
    b = b + tone_adj;

    // 3. Lift/Gamma/Gain/Offset colour wheels — band weighting via
    //    working-space luma.
    let luma2 = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
    let lift_w = max(1.0 - luma2, 0.0);
    let gain_w = max(luma2, 0.0);
    let gamma_w = max(sin(clamp(luma2, 0.0, 1.0) * 3.14159265), 0.0);
    r = r + params.lift_r * lift_w + params.gamma_r * gamma_w + params.gain_r * gain_w + params.offset_r;
    g = g + params.lift_g * lift_w + params.gamma_g * gamma_w + params.gain_g * gain_w + params.offset_g;
    b = b + params.lift_b * lift_w + params.gamma_b * gamma_w + params.gain_b * gain_w + params.offset_b;

    // 4. Contrast — pivots at 0.18 linear mid-grey.
    let cs = 1.0 + params.contrast;
    r = (r - 0.18) * cs + 0.18;
    g = (g - 0.18) * cs + 0.18;
    b = (b - 0.18) * cs + 0.18;

    // 5. Saturation — working-space luma lerp.
    let luma3 = params.luma_yr * r + params.luma_yg * g + params.luma_yb * b;
    r = luma3 + (r - luma3) * params.saturation;
    g = luma3 + (g - luma3) * params.saturation;
    b = luma3 + (b - luma3) * params.saturation;

    // 6. Temperature / Tint: a white balance, the matrix
    //    `klipp_color::white_balance` builds. It scales, so it acts the
    //    same at every exposure.
    let wb_r = params.wb0 * r + params.wb1 * g + params.wb2 * b;
    let wb_g = params.wb3 * r + params.wb4 * g + params.wb5 * b;
    let wb_b = params.wb6 * r + params.wb7 * g + params.wb8 * b;
    r = wb_r;
    g = wb_g;
    b = wb_b;

    // 7. Hue rotation — CPU path handles this (HSL conversion is
    //    expensive on GPU and UX decision to keep).

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
    _pad0: u32,
}

@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;
",
    shader_linear_helpers_wgsl!(),
    "
// LUT layout: 6 curves × 256 entries = indices 0..1535
// Curve 0: hue_vs_hue   (0..255)
// Curve 1: hue_vs_sat   (256..511)
// Curve 2: hue_vs_lum   (512..767)
// Curve 3: lum_vs_sat   (768..1023)
// Curve 4: sat_vs_sat   (1024..1279)
// Curve 5: orange_vs_sat (1280..1535)

fn lut_lookup(curve: u32, t: f32) -> f32 {
    let base = curve * 256u;
    let idx_f = t * 255.0;
    let idx0 = u32(clamp(idx_f, 0.0, 254.0));
    let idx1 = idx0 + 1u;
    let frac = idx_f - f32(idx0);
    return lut[base + idx0] * (1.0 - frac) + lut[base + idx1] * frac;
}

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> vec3<f32> {
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    if delta < 0.0001 {
        return vec3<f32>(0.0, 0.0, l);
    }
    let s = delta / (1.0 - abs(2.0 * l - 1.0));
    var h: f32;
    if mx == r {
        h = ((g - b) / delta) % 6.0;
        if h < 0.0 { h = h + 6.0; }
    } else if mx == g {
        h = (b - r) / delta + 2.0;
    } else {
        h = (r - g) / delta + 4.0;
    }
    h = h / 6.0;
    return vec3<f32>(h, clamp(s, 0.0, 1.0), l);
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> vec3<f32> {
    if s < 0.0001 {
        return vec3<f32>(l, l, l);
    }
    let c = (1.0 - abs(2.0 * l - 1.0)) * s;
    let x = c * (1.0 - abs((h * 6.0) % 2.0 - 1.0));
    let m = l - c * 0.5;
    let h6 = u32(h * 6.0);
    var rgb: vec3<f32>;
    switch h6 {
        case 0u: { rgb = vec3<f32>(c, x, 0.0); }
        case 1u: { rgb = vec3<f32>(x, c, 0.0); }
        case 2u: { rgb = vec3<f32>(0.0, c, x); }
        case 3u: { rgb = vec3<f32>(0.0, x, c); }
        case 4u: { rgb = vec3<f32>(x, 0.0, c); }
        default: { rgb = vec3<f32>(c, 0.0, x); }
    }
    return rgb + vec3<f32>(m, m, m);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }

    let orig = unpack(input[idx]);
    let hsl = rgb_to_hsl(orig.x, orig.y, orig.z);
    var h = hsl.x;
    var s = hsl.y;
    var l = hsl.z;

    // Curve 0: Hue vs Hue (0.5 = no change)
    let hvh = lut_lookup(0u, h);
    h = (h + (hvh - 0.5) * 2.0) % 1.0;
    if h < 0.0 { h = h + 1.0; }

    // Curve 1: Hue vs Sat (0.5 = identity)
    let hvs = lut_lookup(1u, hsl.x);
    s = clamp(s * hvs * 2.0, 0.0, 1.0);

    // Curve 2: Hue vs Lum (0.5 = no change)
    let hvl = lut_lookup(2u, hsl.x);
    l = clamp(l + (hvl - 0.5), 0.0, 1.0);

    // Curve 3: Lum vs Sat (0.5 = identity)
    let lvs = lut_lookup(3u, hsl.z);
    s = clamp(s * lvs * 2.0, 0.0, 1.0);

    // Curve 4: Sat vs Sat
    let svs = lut_lookup(4u, hsl.y);
    s = clamp(svs, 0.0, 1.0);

    // Curve 5: Orange vs Sat (targeted at ~30°/360° ≈ 0.083)
    let ovs = lut_lookup(5u, hsl.y);
    let orange_dist = min(abs(hsl.x - 0.083), min(abs(hsl.x - 0.083 + 1.0), abs(hsl.x - 0.083 - 1.0)));
    let weight = clamp(1.0 - orange_dist * 8.0, 0.0, 1.0);
    s = s + (ovs - s) * weight;

    let rgb = hsl_to_rgb(h, s, l);

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
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
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

    // RGB -> HSL
    let mx = max(r, max(g, b));
    let mn = min(r, min(g, b));
    let delta = mx - mn;
    let l = (mx + mn) * 0.5;
    var s = 0.0;
    if delta > 0.0001 {
        s = clamp(delta / (1.0 - abs(2.0 * l - 1.0)), 0.0, 1.0);
    }
    var h = 0.0;
    if delta > 0.0001 {
        if mx == r {
            h = ((g - b) / delta) % 6.0;
            if h < 0.0 { h += 6.0; }
            h = h / 6.0;
        } else if mx == g {
            h = ((b - r) / delta + 2.0) / 6.0;
        } else {
            h = ((r - g) / delta + 4.0) / 6.0;
        }
    }

    // Qualifier matte
    let hue_dist = circular_distance(h, params.hue_center);
    let hue_m = soft_range_centered(hue_dist, params.hue_width, params.hue_soft);
    let sat_m = soft_range(s, params.sat_low, params.sat_high, params.sat_soft);
    let lum_m = soft_range(l, params.lum_low, params.lum_high, params.lum_soft);
    var matte = hue_m * sat_m * lum_m;
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
const SHADER_LUMA_KEY: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    low: f32,
    high: f32,
    soft: f32,
    invert: u32,
    _pad0: u32,
    _pad1: u32,
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
    let y = 0.2126 * orig.x + 0.7152 * orig.y + 0.0722 * orig.z;

    var matte = soft_range(y, params.low, params.high, params.soft);
    if params.invert != 0u { matte = 1.0 - matte; }

    output[idx] = pack(vec4<f32>(matte, matte, matte, orig.w));
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
    let luma = 0.2126 * p.x + 0.7152 * p.y + 0.0722 * p.z;

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
    var max_val: u32 = 0u;
    let r = i32(params.radius);
    let r_sq = f32(r * r);
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
struct Params { width: u32, height: u32, threshold: f32, _pad: u32 }
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
    let luma = 0.2126 * px.x + 0.7152 * px.y + 0.0722 * px.z;
    if luma > params.threshold {
        let factor = (luma - params.threshold) / (1.0 - params.threshold + 1e-6);
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
struct Params { width: u32, height: u32, intensity: f32, desat: f32 }
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
    let l = 0.2126 * bl.x + 0.7152 * bl.y + 0.0722 * bl.z;
    let b = (bl + (vec3<f32>(l) - bl) * params.desat) * params.intensity;
    // Screen blend in linear: 1 − (1 − o)(1 − b).
    let s = vec3<f32>(
        1.0 - (1.0 - o.x) * (1.0 - b.x),
        1.0 - (1.0 - o.y) * (1.0 - b.y),
        1.0 - (1.0 - o.z) * (1.0 - b.z),
    );
    output[idx] = pack(vec4<f32>(s, o.w));
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
        let luma = 0.2126 * sample.x + 0.7152 * sample.y + 0.0722 * sample.z;
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
struct Params { width: u32, height: u32, threshold: f32, half_len: u32, intensity: f32, tint: f32, _pad: u32, _pad2: u32 }
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
        let luma = 0.2126 * px.x + 0.7152 * px.y + 0.0722 * px.z;
        if luma > params.threshold {
            let factor = (luma - params.threshold) / (1.0 - params.threshold + 1e-6);
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
struct Params { width: u32, height: u32, threshold: f32, intensity: f32, hal_r: f32, hal_g: f32, hal_b: f32, _pad: u32 }
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
    let l = 0.2126 * bl.x + 0.7152 * bl.y + 0.0722 * bl.z;
    let h = vec3<f32>(params.hal_r, params.hal_g, params.hal_b) * l * params.intensity;
    let s = vec3<f32>(
        1.0 - (1.0 - o.x) * (1.0 - h.x),
        1.0 - (1.0 - o.y) * (1.0 - h.y),
        1.0 - (1.0 - o.z) * (1.0 - h.z),
    );
    output[idx] = pack(vec4<f32>(s, o.w));
}
"
);

// LUT3D is defined in sRGB 0-1 space (each LUT entry maps an sRGB
// triplet to an sRGB triplet). Storage is now native linear-f32, so
// we encode pixel → sRGB, sample the LUT, then decode the result back
// to linear. The opacity blend is done in linear space.
const SHADER_LUT3D: &str = "
struct Params {
    width: u32,
    height: u32,
    lut_size: u32,
    opacity: f32,
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

    let pixel = input[pixel_idx];
    let r_in = clamp(linear_to_srgb_component(pixel.x), 0.0, 1.0);
    let g_in = clamp(linear_to_srgb_component(pixel.y), 0.0, 1.0);
    let b_in = clamp(linear_to_srgb_component(pixel.z), 0.0, 1.0);

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

    // Trilinear interpolation
    let c000 = lut_sample(r0, g0, b0);
    let c100 = lut_sample(r1, g0, b0);
    let c010 = lut_sample(r0, g1, b0);
    let c110 = lut_sample(r1, g1, b0);
    let c001 = lut_sample(r0, g0, b1);
    let c101 = lut_sample(r1, g0, b1);
    let c011 = lut_sample(r0, g1, b1);
    let c111 = lut_sample(r1, g1, b1);

    let c00 = mix(c000, c100, rf);
    let c01 = mix(c001, c101, rf);
    let c10 = mix(c010, c110, rf);
    let c11 = mix(c011, c111, rf);

    let c0 = mix(c00, c10, gf);
    let c1 = mix(c01, c11, gf);

    let graded_srgb = mix(c0, c1, bf);

    // Decode LUT output (sRGB) back to linear.
    let graded = vec3<f32>(
        srgb_to_linear_component(clamp(graded_srgb.x, 0.0, 1.0)),
        srgb_to_linear_component(clamp(graded_srgb.y, 0.0, 1.0)),
        srgb_to_linear_component(clamp(graded_srgb.z, 0.0, 1.0)),
    );

    // Opacity blend in linear.
    let orig = vec3<f32>(pixel.x, pixel.y, pixel.z);
    let result = mix(orig, graded, params.opacity);

    output[pixel_idx] = vec4<f32>(result.x, result.y, result.z, pixel.w);
}
";

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
    let inv = vec3<f32>(1.0) - e;
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
    let lvl = params.levels;
    let div = lvl - 1.0;
    let e = clamp(enc3(p.xyz), vec3<f32>(0.0), vec3<f32>(1.0));
    let out = min(floor(e * lvl), vec3<f32>(div)) / div;
    output[idx] = pack(vec4<f32>(dec3(out), p.w));
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
    return 0.2126 * p.x + 0.7152 * p.y + 0.0722 * p.z;
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
// averaging so midtones don't darken through the trail.
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
    let w = i32(params.width);
    let h = i32(params.height);
    for (var i = 0u; i < params.samples; i = i + 1u) {
        let sx = clamp(i32(round(x + params.dx * f32(i))), 0, w - 1);
        let sy = clamp(i32(round(y + params.dy * f32(i))), 0, h - 1);
        let si = u32(sy) * params.width + u32(sx);
        sum += unpack(input[si]);
    }
    let inv = 1.0 / f32(params.samples);
    output[idx] = pack(sum * inv);
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

const SHADER_LENS_DISTORTION: &str = "
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
    let c00 = input[cy0 * w + cx0];
    let c10 = input[cy0 * w + cx1];
    let c01 = input[cy1 * w + cx0];
    let c11 = input[cy1 * w + cx1];
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
    output[idx] = sample_pixel(sx, sy, w, h);
}
";

const SHADER_DILATE: &str = "
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
    var max_px = vec4<f32>(0.0);
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            max_px = max(max_px, input[sy * w + sx]);
        }
    }
    output[idx] = max_px;
}
";

const SHADER_ERODE: &str = "
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
    // 1.0 = 'white', matches the 255u sentinel the byte-packed version used.
    var min_px = vec4<f32>(1.0);
    for (var dy = -r; dy <= r; dy = dy + 1) {
        for (var dx = -r; dx <= r; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            min_px = min(min_px, input[sy * w + sx]);
        }
    }
    output[idx] = min_px;
}
";

// Denoise — bilateral filter. Spatial + colour-similarity weights.
// Linear-light luma (BT.709) matches what the coefficients were
// defined for; linear colour-diff means the colour_sigma slider
// now measures physical light-level distance rather than sRGB
// byte distance. Slider values may need re-tuning on complex
// images (the scale of "similar" pixels shifts).
const SHADER_DENOISE: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    spatial_sigma: f32,
    color_sigma: f32,
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
    let center_luma = 0.2126 * center.x + 0.7152 * center.y + 0.0722 * center.z;

    let kernel_size = min(i32(ceil(params.spatial_sigma * 2.0)), 10);
    let spatial_denom = 2.0 * params.spatial_sigma * params.spatial_sigma;
    let color_denom = 2.0 * params.color_sigma * params.color_sigma;

    var sum = vec3<f32>(0.0);
    var total_weight = 0.0;

    for (var dy = -kernel_size; dy <= kernel_size; dy = dy + 1) {
        for (var dx = -kernel_size; dx <= kernel_size; dx = dx + 1) {
            let sx = u32(clamp(x + dx, 0, i32(w) - 1));
            let sy = u32(clamp(y + dy, 0, i32(h) - 1));
            let n = unpack(input[sy * w + sx]);
            let n_luma = 0.2126 * n.x + 0.7152 * n.y + 0.0722 * n.z;

            let spatial_dist = f32(dx * dx + dy * dy);
            let spatial_w = exp(-spatial_dist / spatial_denom);
            let color_diff = center_luma - n_luma;
            let color_w = exp(-color_diff * color_diff / color_denom);
            let weight = spatial_w * color_w;

            sum = sum + n.xyz * weight;
            total_weight += weight;
        }
    }

    let inv = 1.0 / total_weight;
    output[idx] = pack(vec4<f32>(sum * inv, center.w));
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

const SHADER_NOISE_GENERATOR: &str = "
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
    let v = input[idx];
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

    output[idx] = vec4<f32>(r, g, b, v.w);
}
";

const SHADER_MEDIAN_FILTER: &str = "
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
            let p = input[sy * w + sx];
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

    let orig = input[idx];
    output[idx] = vec4<f32>(vals_r[mid], vals_g[mid], vals_b[mid], orig.w);
}
";

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
fn sample_linear_rgb(sx: i32, sy: i32, w: u32, h: u32) -> vec3<f32> {
    let cx = u32(clamp(sx, 0, i32(w) - 1));
    let cy = u32(clamp(sy, 0, i32(h) - 1));
    return unpack(input[cy * w + cx]).xyz;
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
            let sx = i32(round(x + dx * t * scale));
            let sy = i32(round(y + dy * t * scale));
            sum = sum + sample_linear_rgb(sx, sy, w, h);
        }
    } else {
        // Spin blur
        let half_arc = params.amount * 3.14159265 / 180.0 * 0.5;
        for (var i = 0u; i < s; i = i + 1u) {
            let t = (f32(i) / f32(s) - 0.5) * 2.0;
            let ang = t * half_arc;
            let cos_a = cos(ang);
            let sin_a = sin(ang);
            let sx = i32(round(cx + dx * cos_a - dy * sin_a));
            let sy = i32(round(cy + dx * sin_a + dy * cos_a));
            sum = sum + sample_linear_rgb(sx, sy, w, h);
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
    let m = clamp((e - vec3<f32>(params.in_black)) / in_range, vec3<f32>(0.0), vec3<f32>(1.0));

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

// Histogram equalisation — flatten the distribution of display-encoded
// luma, the brightness the user sees. The CDF is built on the CPU by
// `crate::equalize_cdf`.
const SHADER_EQUALIZE: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    strength: f32,
    _pad0: u32,
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

    // Compute luminance and look up equalised value from CDF
    let luma = 0.2126 * e.x + 0.7152 * e.y + 0.0722 * e.z;
    let luma_idx = u32(clamp(luma * 255.0, 0.0, 255.0));
    let eq_luma = cdf[luma_idx];

    // Scale RGB proportionally to match new luminance
    let scale = select(eq_luma / luma, 1.0, luma < 0.001);

    // Blend with original based on strength
    let out = e + (e * scale - e) * params.strength;
    output[idx] = pack(vec4<f32>(dec3(out), p.w));
}
"
);

const SHADER_LETTERBOX: &str = "
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

    let v = input[idx];
    if in_bar {
        // bar_color 0 = black, non-zero = white. Linear black = 0.0,
        // linear white = 1.0 (same as sRGB 0/1 endpoints).
        let bv = select(0.0, 1.0, params.bar_color != 0u);
        let op = params.opacity;
        let orig_rgb = vec3<f32>(v.x, v.y, v.z);
        let bar_rgb = vec3<f32>(bv);
        let mixed = orig_rgb * (1.0 - op) + bar_rgb * op;
        let na = v.w * (1.0 - op) + op;
        output[idx] = vec4<f32>(mixed.x, mixed.y, mixed.z, na);
    } else {
        output[idx] = v;
    }
}
";

const SHADER_CHROMATIC_ABERRATION: &str = "
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
    let r_sx = clamp(i32(round(cx + dx * r_scale)), 0, i32(w) - 1);
    let r_sy = clamp(i32(round(cy + dy * r_scale)), 0, i32(h) - 1);
    let r_val = input[u32(r_sy) * w + u32(r_sx)].x;

    // Green channel: original position
    let g_pixel = input[idx];
    let g_val = g_pixel.y;
    let a_val = g_pixel.w;

    // Blue channel: sample shifted
    let b_scale = 1.0 + params.blue_shift * norm_dist / max_dist;
    let b_sx = clamp(i32(round(cx + dx * b_scale)), 0, i32(w) - 1);
    let b_sy = clamp(i32(round(cy + dy * b_scale)), 0, i32(h) - 1);
    let b_val = input[u32(b_sy) * w + u32(b_sx)].z;

    output[idx] = vec4<f32>(r_val, g_val, b_val, a_val);
}
";

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

// Colour balance — temperature / tint additive shifts + optional
// Rodrigues hue rotation around the luma diagonal. Additive shifts
// in linear mean "add N units of light to R, subtract from B"
// rather than "add N units of gamma-encoded byte."
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

    // Hue rotation in RGB (Rodrigues rotation around (1,1,1)/sqrt(3) axis)
    if abs(params.hue) > 0.01 {
        let angle = params.hue * 3.14159265 / 180.0;
        let cos_a = cos(angle);
        let sin_a = sin(angle);
        let k = 1.0 / 3.0;
        let new_r = r * (cos_a + k * (1.0 - cos_a)) + g * (k * (1.0 - cos_a) - 0.57735 * sin_a) + b * (k * (1.0 - cos_a) + 0.57735 * sin_a);
        let new_g = r * (k * (1.0 - cos_a) + 0.57735 * sin_a) + g * (cos_a + k * (1.0 - cos_a)) + b * (k * (1.0 - cos_a) - 0.57735 * sin_a);
        let new_b = r * (k * (1.0 - cos_a) - 0.57735 * sin_a) + g * (k * (1.0 - cos_a) + 0.57735 * sin_a) + b * (cos_a + k * (1.0 - cos_a));
        r = new_r;
        g = new_g;
        b = new_b;
    }

    // Global mix
    r = orig.x + (r - orig.x) * params.global_mix;
    g = orig.y + (g - orig.y) * params.global_mix;
    b = orig.z + (b - orig.z) * params.global_mix;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
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
    var r = orig.x;
    var g = orig.y;
    var b = orig.z;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

    let lift_w = max(1.0 - luma, 0.0);
    let gamma_w = max(sin(clamp(luma, 0.0, 1.0) * 3.14159265), 0.0);
    let gain_w = luma;

    let bri_adj = params.lift_bri * lift_w + params.gamma_bri * gamma_w + params.gain_bri * gain_w + params.offset_bri;

    r = r + bri_adj;
    g = g + bri_adj;
    b = b + bri_adj;

    let sat_adj = params.lift_sat * lift_w + params.gamma_sat * gamma_w + params.gain_sat * gain_w + params.offset_sat;
    let sat_factor = 1.0 + sat_adj;
    let luma2 = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    r = luma2 + (r - luma2) * sat_factor;
    g = luma2 + (g - luma2) * sat_factor;
    b = luma2 + (b - luma2) * sat_factor;

    output[idx] = pack(vec4<f32>(r, g, b, orig.w));
}
"
);

// RGB curves — four LUT-lookup curves (master + per-channel).
// Curves are authored on linear input now; 0.18 is mid-grey on
// the curve axis, not 0.46. BT.709 luma coefficients apply to
// linear directly for the preserve-luma option.
const SHADER_RGB_CURVES: &str = concat!(
    "
struct Params {
    width: u32,
    height: u32,
    preserve_luma: u32,
    mix: f32,
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
fn lut_lookup(channel: u32, val: f32) -> f32 {
    let base = channel * 256u;
    let idx_f = clamp(val, 0.0, 1.0) * 255.0;
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

    // Apply per-channel curves, then master
    var r = lut_lookup(1u, orig.x);
    var g = lut_lookup(2u, orig.y);
    var b = lut_lookup(3u, orig.z);
    r = lut_lookup(0u, r);
    g = lut_lookup(0u, g);
    b = lut_lookup(0u, b);

    // Preserve luma: scale output to match input luma
    if params.preserve_luma != 0u {
        let orig_luma = 0.2126 * orig.x + 0.7152 * orig.y + 0.0722 * orig.z;
        let new_luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
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
const SHADER_MASK_V2: &str = "
struct Params {
    width: u32,
    height: u32,
    mask_count: u32,
    _pad: u32,
}

struct MaskRecord {
    shape_type: u32,
    flags: u32,
    bezier_offset: u32,
    bezier_count: u32,
    cx: f32, cy: f32, p1: f32, p2: f32,
    p3: f32, feather: f32, _pad0: u32, _pad1: u32,
}

@group(0) @binding(0) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(1) var<uniform> params: Params;
@group(0) @binding(2) var<storage, read> records: array<MaskRecord>;
@group(0) @binding(3) var<storage, read> points: array<vec2<f32>>;

fn smooth_step_sdf(dist: f32, feather: f32) -> f32 {
    if dist <= 0.0 { return 1.0; }
    if feather > 0.0001 && dist < feather {
        let t = dist / feather;
        return 1.0 - t * t * (3.0 - 2.0 * t);
    }
    return 0.0;
}

// Signed-distance to a closed polygon using the same winding-
// number + min edge distance algorithm as the CPU rasteriser at
// klipp-color/src/masks.rs:80-115. Returns negative inside,
// positive outside, in normalised width-relative units.
fn bezier_sdf(nx: f32, ny: f32, aspect: f32, offset: u32, count: u32) -> f32 {
    if count < 3u { return 1.0; }
    let px = nx;
    let py = ny / aspect;
    var min_dist_sq: f32 = 1.0e30;
    var winding: i32 = 0;
    for (var i: u32 = 0u; i < count; i = i + 1u) {
        let a_raw = points[offset + i];
        let b_raw = points[offset + ((i + 1u) % count)];
        let ax = a_raw.x;
        let ay = a_raw.y / aspect;
        let bx = b_raw.x;
        let by = b_raw.y / aspect;
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

fn eval_record(rec: MaskRecord, nx: f32, ny: f32, aspect: f32) -> f32 {
    var dist: f32;
    switch rec.shape_type {
        case 0u: {
            // circle
            let dx = nx - rec.cx;
            let dy = (ny - rec.cy) / aspect;
            dist = sqrt(dx * dx + dy * dy) - rec.p1;
        }
        case 1u: {
            // rectangle
            let rad = rec.p3 * 3.14159265 / 180.0;
            let cos_r = cos(rad);
            let sin_r = sin(rad);
            let dx = nx - rec.cx;
            let dy = (ny - rec.cy) / aspect;
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
            let gy = (rec.p2 - rec.cy) / aspect;
            let glen2 = gx * gx + gy * gy;
            if glen2 < 1.0e-8 {
                dist = 0.0;
            } else {
                let dx = nx - rec.cx;
                let dy = (ny - rec.cy) / aspect;
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
    var m = smooth_step_sdf(dist, rec.feather);
    if (rec.flags & 1u) != 0u { m = 1.0 - m; }
    let opacity = f32((rec.flags >> 8u) & 0xFFu) / 255.0;
    return m * opacity;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let x = idx % params.width;
    let y = idx / params.width;
    let nx = f32(x) / f32(max(params.width - 1u, 1u));
    let ny = f32(y) / f32(max(params.height - 1u, 1u));
    let aspect = f32(params.height) / f32(params.width);

    var matte: f32 = 0.0;
    for (var i: u32 = 0u; i < params.mask_count; i = i + 1u) {
        let m = eval_record(records[i], nx, ny, aspect);
        matte = max(matte, m);
    }
    let m = clamp(matte, 0.0, 1.0);
    output[idx] = vec4<f32>(m, m, m, 1.0);
}
";

// Vibrance — saturation-weighted boost. Muted colours get a bigger
// push; already-saturated colours (including skin) are protected.
// BT.709 luma now sits in its defined domain.
const SHADER_VIBRANCE: &str = concat!(
    "
struct Params { width: u32, height: u32, amount: f32, _pad: u32, }
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
    let luma = 0.2126 * p.x + 0.7152 * p.y + 0.0722 * p.z;
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

fn rgb2hsl(r: f32, g: f32, b: f32) -> vec3<f32> {
    let mx = max(max(r, g), b);
    let mn = min(min(r, g), b);
    let d = mx - mn;
    let l = (mx + mn) * 0.5;
    if d < 0.0001 { return vec3<f32>(0.0, 0.0, l); }
    let s = d / (1.0 - abs(2.0 * l - 1.0));
    var h: f32;
    if mx == r { h = ((g - b) / d) % 6.0; if h < 0.0 { h += 6.0; } }
    else if mx == g { h = (b - r) / d + 2.0; }
    else { h = (r - g) / d + 4.0; }
    return vec3<f32>(h / 6.0, clamp(s, 0.0, 1.0), l);
}

fn hsl2rgb(h: f32, s: f32, l: f32) -> vec3<f32> {
    if s < 0.0001 { return vec3<f32>(l, l, l); }
    let c = (1.0 - abs(2.0 * l - 1.0)) * s;
    let x = c * (1.0 - abs((h * 6.0) % 2.0 - 1.0));
    let m = l - c * 0.5;
    let h6 = u32(h * 6.0);
    var rgb: vec3<f32>;
    switch h6 {
        case 0u: { rgb = vec3<f32>(c, x, 0.0); }
        case 1u: { rgb = vec3<f32>(x, c, 0.0); }
        case 2u: { rgb = vec3<f32>(0.0, c, x); }
        case 3u: { rgb = vec3<f32>(0.0, x, c); }
        case 4u: { rgb = vec3<f32>(x, 0.0, c); }
        default: { rgb = vec3<f32>(c, 0.0, x); }
    }
    return rgb + vec3<f32>(m, m, m);
}

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
    let hsl = rgb2hsl(p.x, p.y, p.z);
    let h_deg = hsl.x * 360.0;
    let sector_f = h_deg / 30.0;
    let sector_low = u32(floor(sector_f)) % 12u;
    let sector_high = (sector_low + 1u) % 12u;
    let t = fract(sector_f);
    let blend = (1.0 - cos(t * 3.14159265)) * 0.5;
    let hue_shift = get_hue_shift(sector_low) * (1.0 - blend) + get_hue_shift(sector_high) * blend;
    let sat_shift = get_sat_shift(sector_low) * (1.0 - blend) + get_sat_shift(sector_high) * blend;
    let sat_weight = clamp(hsl.y, 0.0, 1.0);
    let new_h = ((h_deg + hue_shift * sat_weight) % 360.0 + 360.0) % 360.0 / 360.0;
    let new_s = clamp(hsl.y + sat_shift * sat_weight, 0.0, 1.0);
    let rgb = hsl2rgb(new_h, new_s, hsl.z);
    output[idx] = pack(vec4<f32>(rgb, p.w));
}
"
);

// ── Input Transform (log-to-linear + sRGB gamma) ─────────────────────────────
// Uses a 256-entry 1D LUT per channel (pre-computed on CPU from the log format).
const SHADER_INPUT_TRANSFORM: &str = "
struct Params { width: u32, height: u32, _pad0: u32, _pad1: u32, }
@group(0) @binding(0) var<storage, read> input: array<vec4<f32>>;
@group(0) @binding(1) var<storage, read_write> output: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<f32>;
// LUT layout: 256 entries (input 0..255 → output 0..1 sRGB)

// Input transform is a 1D LUT keyed by sRGB 0-255 (the historical
// design). Storage is now linear-f32, so we re-encode to sRGB,
// index, and decode the LUT output (also sRGB) back to linear.
fn it_linear_to_srgb(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    if (x <= 0.0031308) { return x * 12.92; }
    else { return 1.055 * pow(x, 1.0/2.4) - 0.055; }
}

fn it_srgb_to_linear(c: f32) -> f32 {
    let x = clamp(c, 0.0, 1.0);
    if (x <= 0.04045) { return x / 12.92; }
    else { return pow((x + 0.055) / 1.055, 2.4); }
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let idx = gid.x;
    if idx >= params.width * params.height { return; }
    let pixel = input[idx];
    let ri = u32(clamp(it_linear_to_srgb(pixel.x) * 255.0, 0.0, 255.0));
    let gi = u32(clamp(it_linear_to_srgb(pixel.y) * 255.0, 0.0, 255.0));
    let bi = u32(clamp(it_linear_to_srgb(pixel.z) * 255.0, 0.0, 255.0));
    let ro = it_srgb_to_linear(lut[ri]);
    let go = it_srgb_to_linear(lut[gi]);
    let bo = it_srgb_to_linear(lut[bi]);
    output[idx] = vec4<f32>(ro, go, bo, pixel.w);
}
";

// Clarity — three-pass midtone unsharp mask. CLARITY_BLUR_H reads
// linear RGBA, computes linear-light BT.709 luma, writes the luma
// into the `.x` channel of a shared vec4<f32> intermediate buffer
// (other channels unused — we never downconvert or pack). CLARITY_BLUR_V
// averages that luma (still in `.x`). CLARITY_APPLY adds back detail
// weighted by linear midtone presence, centred on 0.18 linear.
const SHADER_CLARITY_BLUR_H: &str = concat!(
    "
struct Params { width: u32, height: u32, radius: u32, _pad: u32, }
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
        sum += 0.2126 * p.x + 0.7152 * p.y + 0.0722 * p.z;
        count += 1u;
    }
    let luma = sum / f32(count);
    // Store linear luma in the .x channel; downstream passes read it as
    // a linear [0,1] scalar.
    output[idx] = vec4<f32>(luma, 0.0, 0.0, 0.0);
}
"
);

const SHADER_CLARITY_BLUR_V: &str = "
struct Params { width: u32, height: u32, radius: u32, _pad: u32, }
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
        let val = input[u32(sy) * params.width + u32(x)].x;
        sum += val;
        count += 1u;
    }
    output[idx] = vec4<f32>(sum / f32(count), 0.0, 0.0, 0.0);
}
";

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
    let k = clamp(matte[idx].x, 0.0, 1.0);
    output[idx] = pack(vec4<f32>(p.x * k, p.y * k, p.z * k, 1.0));
}
"
);

const SHADER_CLARITY_APPLY: &str = concat!(
    "
struct Params { width: u32, height: u32, amount: f32, _pad: u32, }
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
    let luma = 0.2126 * p.x + 0.7152 * p.y + 0.0722 * p.z;
    let blurred_luma = blurred[idx].x;
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
