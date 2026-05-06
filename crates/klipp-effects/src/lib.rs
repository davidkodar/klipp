//! Built-in GPU effects and shared scope / analysis pipelines.
//!
//! The crate exposes [`BuiltinEffect`] — a serialisable enum covering
//! every first-party effect (opacity, blur, sharpen, crop, transform,
//! chroma key, bloom, glow, light rays, vignette, …) plus a few
//! workflow utilities (rolling shutter repair, scene detection, video
//! stabilisation). Each effect has a CPU implementation for fallback
//! and a WGSL compute-shader GPU path in [`gpu::GpuEffectsPipeline`].
//!
//! # GPU pipeline
//!
//! All GPU work goes through [`gpu::GpuEffectsPipeline`]: a single wgpu
//! device holding one storage buffer per intermediate + one
//! `apply_effects` dispatcher that chains effect passes via ping-pong
//! buffers. See [`nv12_gpu`] for the HW-decode output fast-path (NV12 →
//! RGBA on GPU) and [`gpu_scopes`] for histogram / vectorscope / RGB
//! parade compute kernels.
//!
//! # WGSL uniform rule
//!
//! WGSL uniform structs **must** use individual scalar padding fields
//! (`_pad0: u32, _pad1: u32, …`) rather than `array<u32, N>` — naga
//! silently rejects the array form in validation. Rust-side structs
//! mirror with `_pad: [u32; N]` which is fine, but the matching WGSL
//! declaration must unpack.
//!
//! # Stabilisation
//!
//! [`stabilization`] uses a sequential-frame analyse pass
//! ([`klipp_engine::SequentialFrameDecoder`]) to compute per-frame
//! motion vectors, then applies the inverse as a warp during playback
//! and export. Analyse went from 10–15 min to ~18 s on 4K through the
//! decoder optimisation.

pub mod aces_rrt_wgsl;
pub mod blur;
mod chroma_key;
pub mod color_wgsl;
pub mod composite;
mod crop;
pub mod gpu;
pub mod gpu_context;
pub mod gpu_scopes;
mod light;
pub mod nv12_gpu;
pub mod p010_gpu;
mod opacity;
pub mod rolling_shutter;
pub mod scene_detect;
pub mod stabilization;
mod sharpen;
pub mod track_matte;
pub mod transform;

use serde::{Deserialize, Serialize};

/// Per-clip composite blend mode. Lives here (not in klipp-core)
/// because the algebra in `composite::blend_*` is the primary
/// consumer; klipp-core re-exports the type as `klipp_core::BlendMode`
/// for back-compat with existing call-site paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    ColorDodge,
    ColorBurn,
    Darken,
    Lighten,
}

impl BlendMode {
    pub fn display_name(&self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Add => "Add",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft Light",
            BlendMode::HardLight => "Hard Light",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::ColorDodge => "Color Dodge",
            BlendMode::ColorBurn => "Color Burn",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
        }
    }

    pub const ALL: [BlendMode; 13] = [
        BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen,
        BlendMode::Overlay, BlendMode::SoftLight, BlendMode::HardLight,
        BlendMode::Difference, BlendMode::Exclusion, BlendMode::ColorDodge,
        BlendMode::ColorBurn, BlendMode::Darken, BlendMode::Lighten,
    ];
}

/// Override pointer for the focal anchor on `BuiltinEffect::Transform`.
///
/// When set on a Transform effect, the engine's render path resolves
/// this reference to clip-space `(focal_x, focal_y)` each frame and
/// uses the result as the anchor — *overriding* the static
/// `anchor_x` / `anchor_y` fields. When `None`, the static fields are
/// used directly (today's behaviour).
///
/// **Why a uuid and not a `ClipId`**: klipp-effects can't depend on
/// klipp-core (the cycle hint at the top of `Cargo.toml` explains).
/// Callers in klipp-core / klipp-ui / klipp-engine wrap this raw
/// uuid in `ClipId` at the boundary. Same pattern as `BlendMode`
/// living here and being re-exported from klipp-core.
///
/// Pinned by HZ-NEXT-GLYPH-LOCK as the schema growth point for
/// glyph-anchored hyperzoom focal points; future references
/// (tracker-driven, marker-driven) extend this enum.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FocalRef {
    /// Focal point follows a glyph in a title clip's shaped layout.
    /// `char_index` indexes into `klipp_title::compute_char_positions`'s
    /// returned vec for the referenced title's current `TitleData`.
    /// `fallback_xy` is the picker-time snapshot in clip-space —
    /// used by the resolver when the live lookup fails (clip
    /// removed, kind changed, char_index past end of glyphs).
    Glyph {
        /// Raw uuid of the title clip the focal point follows.
        ///
        /// Stored as `uuid::Uuid` — not `klipp_core::ClipId` — for
        /// the same reason `BlendMode` lives in this crate: klipp-core
        /// already depends on klipp-effects via `BuiltinEffect`
        /// payloads on AppCommand, so a back-edge from klipp-effects
        /// to klipp-core would close a dep cycle. Callers wrap this
        /// raw uuid into `ClipId(uuid)` at the boundary (klipp-core /
        /// klipp-engine / klipp-ui all do).
        title_clip_id: uuid::Uuid,
        char_index: u32,
        fallback_xy: (f32, f32),
    },
    /// Focal point follows a tracker output. The tracker lives on
    /// `clip_id`'s `Clip.track_points[point_index]`; its
    /// per-frame `(x, y)` positions feed the focal point each
    /// render frame. Out-of-range frames (before `start_frame` or
    /// past the end of `positions`) fall back to `fallback_xy` —
    /// same silent-fallback contract as `Glyph`.
    ///
    /// Same dep-cycle rationale for `uuid::Uuid` over `ClipId`
    /// (see `Glyph::title_clip_id`).
    Tracker {
        clip_id: uuid::Uuid,
        point_index: u32,
        fallback_xy: (f32, f32),
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BuiltinEffect {
    Crop {
        /// Fraction of width to crop from the left (0.0–1.0)
        left: f32,
        /// Fraction of width to crop from the right (0.0–1.0)
        right: f32,
        /// Fraction of height to crop from the top (0.0–1.0)
        top: f32,
        /// Fraction of height to crop from the bottom (0.0–1.0)
        bottom: f32,
        /// Feather in pixels (0.0 = hard edge)
        feather: f32,
    },
    Transform {
        /// Horizontal offset in pixels from center
        position_x: f32,
        /// Vertical offset in pixels from center
        position_y: f32,
        /// Horizontal scale (1.0 = 100%)
        scale_x: f32,
        /// Vertical scale (1.0 = 100%)
        scale_y: f32,
        /// Rotation in degrees (Z-axis)
        rotation: f32,
        /// Anchor X (0.0–1.0, default 0.5 = center)
        anchor_x: f32,
        /// Anchor Y (0.0–1.0, default 0.5 = center)
        anchor_y: f32,
        /// 3D rotation around X axis (tilt forward/backward) in degrees
        #[serde(default)]
        rotation_x: f32,
        /// 3D rotation around Y axis (tilt left/right) in degrees
        #[serde(default)]
        rotation_y: f32,
        /// Perspective strength (0 = none/orthographic, 1.0 = moderate, 2.0 = strong)
        #[serde(default)]
        perspective: f32,
        /// Optional focal-anchor override. When `Some`, the engine
        /// resolves the reference each frame (e.g. a glyph centroid
        /// from a referenced title clip) and uses the result as the
        /// effective anchor — `anchor_x` / `anchor_y` become the
        /// fallback for the resolution failure paths. When `None`
        /// (default), the static `anchor_x` / `anchor_y` are used
        /// directly. Old project files load with `None` via
        /// `serde(default)` — back-compat is byte-identical.
        #[serde(default)]
        focal_ref: Option<FocalRef>,
    },
    Opacity {
        /// Opacity value (0.0–1.0)
        value: f32,
    },
    GaussianBlur {
        /// Blur radius in pixels
        radius: f32,
    },
    Sharpen {
        /// Sharpen amount (0.0–2.0)
        amount: f32,
    },
    ChromaKey {
        /// RGB of the key colour
        key_color: [u8; 3],
        /// Tolerance (0.0–1.0)
        tolerance: f32,
        /// Spill suppression (0.0–1.0)
        spill_suppression: f32,
    },
    /// Bloom: extract bright areas, blur, and add back for a cinematic glow.
    Bloom {
        /// Luminance threshold for bright areas (0.0–1.0, default 0.7)
        threshold: f32,
        /// Blur radius for the bloom (pixels, default 20.0)
        radius: f32,
        /// Intensity of the bloom overlay (0.0–2.0, default 0.5)
        intensity: f32,
    },
    /// Soft glow: diffuse light wrap around bright areas.
    Glow {
        /// Blur radius for the glow (pixels, default 30.0)
        radius: f32,
        /// Glow intensity (0.0–2.0, default 0.4)
        intensity: f32,
    },
    /// God rays: volumetric light rays emanating from a point.
    LightRays {
        /// X position of the light source (0.0–1.0, default 0.5 = center)
        center_x: f32,
        /// Y position of the light source (0.0–1.0, default 0.3 = upper third)
        center_y: f32,
        /// Luminance threshold for ray sources (0.0–1.0, default 0.6)
        threshold: f32,
        /// Number of radial blur samples (4–128, default 64)
        samples: u32,
        /// How far rays extend (0.0–1.0, default 0.3)
        decay: f32,
        /// Ray intensity (0.0–2.0, default 0.5)
        intensity: f32,
    },
    /// Vignette: darken or lighten edges/corners of the frame.
    Vignette {
        /// How much to darken/lighten (-1.0=full black, 0.0=none, 1.0=full white, default -0.4)
        amount: f32,
        /// Size of the vignette center (0.0–2.0, default 0.8). Larger = more visible center.
        size: f32,
        /// Softness of the vignette falloff (0.0–2.0, default 0.5)
        softness: f32,
        /// Roundness: 0.0=rectangular, 1.0=circular (default 0.5)
        roundness: f32,
    },
    /// Diffusion / Pro-Mist: softens highlights for a dreamy cinematic look.
    Diffusion {
        /// How much bright areas bleed into surroundings (0.0–1.0, default 0.3)
        amount: f32,
        /// Blur radius for the diffusion (pixels, default 25.0)
        radius: f32,
        /// Luminance threshold — only soften above this (0.0–1.0, default 0.5)
        threshold: f32,
        /// Saturation reduction in the diffused areas (0.0–1.0, default 0.2)
        desaturation: f32,
    },
    /// Lens Flare: simulated lens flare with configurable position and style.
    LensFlare {
        /// X position of the flare source (0.0–1.0, default 0.7)
        position_x: f32,
        /// Y position of the flare source (0.0–1.0, default 0.3)
        position_y: f32,
        /// Overall brightness (0.0–2.0, default 0.6)
        brightness: f32,
        /// Size of the main flare disc (0.0–1.0, default 0.15)
        flare_size: f32,
        /// Number of ghost elements (0–8, default 4)
        ghost_count: u32,
        /// Intensity of ghost elements (0.0–1.0, default 0.3)
        ghost_intensity: f32,
        /// Chromatic fringing on ghosts (0.0–1.0, default 0.4)
        chromatic_aberration: f32,
    },
    /// Light Wrap: bleed background light onto foreground edges (compositing essential).
    LightWrap {
        /// Width of the light wrap in pixels (0.0–100.0, default 15.0)
        width: f32,
        /// Intensity of the wrap (0.0–1.0, default 0.5)
        intensity: f32,
        /// Softness of the wrap falloff (0.0–2.0, default 1.0)
        softness: f32,
    },
    /// Anamorphic Streak: horizontal lens flare streak on bright areas.
    AnamorphicStreak {
        /// Luminance threshold for streak sources (0.0–1.0, default 0.7)
        threshold: f32,
        /// Horizontal length of the streak in pixels (0.0–200.0, default 80.0)
        length: f32,
        /// Intensity of the streak (0.0–2.0, default 0.5)
        intensity: f32,
        /// Tint color: 0.0=warm amber, 0.5=neutral, 1.0=cool blue (default 0.3)
        tint: f32,
    },
    /// Film Halation: vintage film look with red/orange bleed around bright highlights.
    FilmHalation {
        /// Luminance threshold for halation sources (0.0–1.0, default 0.75)
        threshold: f32,
        /// Blur radius for the halation (pixels, default 15.0)
        radius: f32,
        /// Intensity of the halation (0.0–1.0, default 0.3)
        intensity: f32,
        /// Hue of the halation: 0.0=deep red, 0.5=orange, 1.0=warm yellow (default 0.2)
        hue: f32,
    },
    /// Invert: invert RGB channels with adjustable mix.
    Invert {
        /// Mix amount (0.0 = original, 1.0 = fully inverted)
        mix: f32,
    },
    /// Posterize: reduce color depth to discrete levels.
    Posterize {
        /// Number of levels per channel (2.0–256.0, default 8.0)
        levels: f32,
    },
    /// Mosaic (Pixelate): block-based pixelation effect.
    Mosaic {
        /// Size of each block in pixels (1.0–100.0, default 10.0)
        block_size: f32,
    },
    /// EdgeDetect: Sobel edge detection blended with original.
    EdgeDetect {
        /// Edge strength multiplier (0.0–5.0, default 1.0)
        strength: f32,
        /// Blend with original (0.0 = original, 1.0 = edges only)
        mix: f32,
    },
    /// ChannelMixer: 3x3 matrix multiplication on RGB channels.
    ChannelMixer {
        /// Output red = red_red * R + red_green * G + red_blue * B
        red_red: f32,
        red_green: f32,
        red_blue: f32,
        /// Output green = green_red * R + green_green * G + green_blue * B
        green_red: f32,
        green_green: f32,
        green_blue: f32,
        /// Output blue = blue_red * R + blue_green * G + blue_blue * B
        blue_red: f32,
        blue_green: f32,
        blue_blue: f32,
    },
    /// DirectionalBlur: motion blur along a specified angle.
    DirectionalBlur {
        /// Direction angle in degrees (0–360)
        angle: f32,
        /// Blur distance in pixels (0–100)
        distance: f32,
        /// Number of samples (3–32, default 8)
        samples: u32,
    },
    /// Emboss/Stamp: 3D relief effect using directional convolution.
    Emboss {
        /// Light direction angle in degrees (0–360, default 135 = top-left)
        angle: f32,
        /// Effect strength (0.0–5.0, default 1.0)
        strength: f32,
        /// Blend with original (0.0 = original, 1.0 = emboss only)
        mix: f32,
    },
    /// Mirror: flip image horizontally and/or vertically.
    Mirror {
        /// Flip horizontally (0 or 1, default 1)
        horizontal: u32,
        /// Flip vertically (0 or 1, default 0)
        vertical: u32,
    },
    /// LensDistortion: barrel/pincushion lens distortion.
    LensDistortion {
        /// Distortion amount (-1.0 barrel to 1.0 pincushion, default 0.0)
        distortion: f32,
        /// Scale factor (0.5-2.0, default 1.0)
        scale: f32,
    },
    /// Dilate: morphological dilation — expand bright areas.
    Dilate {
        /// Neighborhood radius (1-10, default 1)
        radius: u32,
    },
    /// Erode: morphological erosion — expand dark areas.
    Erode {
        /// Neighborhood radius (1-10, default 1)
        radius: u32,
    },
    /// Denoise: edge-preserving bilateral filter.
    Denoise {
        /// Spatial sigma for distance weighting (1.0-20.0, default 5.0)
        spatial_sigma: f32,
        /// Color sigma for intensity weighting (0.01-1.0, default 0.1)
        color_sigma: f32,
        /// Number of filter iterations (1-3, default 1)
        iterations: u32,
    },
    /// Gamma: per-channel gamma correction.
    Gamma {
        /// Gamma value (0.1-5.0, default 1.0)
        gamma: f32,
    },
    /// HSV Adjust: hue shift, saturation and value multiplier.
    HSVAdjust {
        /// Hue shift in degrees (-180..180, default 0)
        hue_shift: f32,
        /// Saturation multiplier (0..3, default 1.0)
        saturation: f32,
        /// Value/brightness multiplier (0..3, default 1.0)
        value: f32,
    },
    /// Vibrance: selective saturation that protects already-saturated colors.
    Vibrance {
        /// Vibrance amount (-1.0 to 2.0, default 0.0). Positive = more vibrant, negative = desaturate.
        amount: f32,
    },
    /// Noise Generator: add random noise to the image.
    NoiseGenerator {
        /// Noise amount (0..1, default 0.1)
        amount: f32,
        /// Monochrome noise (0 = color, 1 = grayscale)
        monochrome: u32,
        /// Random seed for noise pattern
        seed: u32,
    },
    /// Median Filter: edge-preserving noise removal via median of neighborhood.
    MedianFilter {
        /// Neighborhood radius (1-5, clamped to 2 on GPU, default 1)
        radius: u32,
    },
    /// Radial Blur: zoom or spin blur from a center point.
    RadialBlur {
        /// Center X position (0-1, default 0.5)
        center_x: f32,
        /// Center Y position (0-1, default 0.5)
        center_y: f32,
        /// Blur amount (0-50, default 10)
        amount: f32,
        /// Number of samples (4-32, default 12)
        samples: u32,
        /// Mode: 0=zoom, 1=spin
        mode: u32,
    },
    /// Levels: input/output remapping with gamma correction.
    Levels {
        /// Input black point (0-1, default 0)
        in_black: f32,
        /// Input white point (0-1, default 1)
        in_white: f32,
        /// Gamma (0.1-5, default 1)
        gamma: f32,
        /// Output black point (0-1, default 0)
        out_black: f32,
        /// Output white point (0-1, default 1)
        out_white: f32,
    },
    /// Equalize: histogram equalization for automatic contrast enhancement.
    Equalize {
        /// Strength of equalization (0-1, default 1.0)
        strength: f32,
    },
    /// Letterbox: aspect ratio matte overlay (bars on top/bottom or sides).
    Letterbox {
        /// Target aspect ratio (e.g., 2.39 for CinemaScope, 1.85 for theatrical)
        aspect_ratio: f32,
        /// Bar color: 0=black (default), 1=white
        bar_color: u32,
        /// Bar opacity (0.0 transparent - 1.0 fully opaque, default 1.0)
        opacity: f32,
    },
    /// Chromatic Aberration: radial RGB channel separation simulating lens fringing.
    ChromaticAberration {
        /// Red channel shift amount (pixels, negative = inward, positive = outward)
        red_shift: f32,
        /// Blue channel shift amount (pixels)
        blue_shift: f32,
        /// Center X (0.0-1.0, default 0.5)
        center_x: f32,
        /// Center Y (0.0-1.0, default 0.5)
        center_y: f32,
    },
    /// Rolling Shutter Correction: removes wobble/jello from CMOS sensor footage.
    RollingShutterCorrection {
        /// Correction strength (0.0=off, 1.0=full correction)
        strength: f32,
        /// Sensor readout ratio: fraction of frame duration for full sensor readout
        readout_ratio: f32,
    },
    /// Video Stabilization: smooths camera motion across a clip.
    /// Requires pre-analysis (AnalyzeStabilization command).
    VideoStabilization {
        /// Smoothing strength: fraction of clip duration used as smoothing window (0.0-1.0)
        smoothing: f32,
        /// Crop ratio: fraction of frame cropped from edges (0.0-0.2)
        crop_ratio: f32,
        /// Auto-zoom to fill cropped area
        zoom: bool,
    },
    /// Speed Ramp: GPU-accelerated retiming with frame blending.
    /// Changes playback speed with smooth interpolation between frames.
    SpeedRamp {
        /// Playback speed multiplier (0.1=10% slow-mo, 1.0=normal, 4.0=4x fast)
        speed: f32,
        /// Blend mode: 0=nearest (no blend), 1=linear (smooth), 2=optical flow (best quality)
        blend_mode: u32,
        /// Ramp curve: 0=constant, 1=ease-in, 2=ease-out, 3=ease-in-out, 4=s-curve
        ramp_curve: u32,
        /// Ramp start position (0.0–1.0 of clip duration, only used with ramp curves)
        ramp_start: f32,
        /// Ramp end position (0.0–1.0 of clip duration)
        ramp_end: f32,
        /// Reverse playback
        reverse: bool,
        /// Freeze frame at this position (0.0–1.0, only active when freeze=true)
        freeze_at: f32,
        /// Enable freeze frame mode
        freeze: bool,
    },
}

/// sRGB → linear-light (IEC 61966-2-1). Scalar helper mirroring
/// `klipp_color::transfer::srgb_to_linear` so this crate stays
/// klipp_color-free. Kept in lock-step with the curve used throughout
/// the engine.
#[inline]
fn srgb_to_linear_scalar(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Linear-light → sRGB (IEC 61966-2-1). See [`srgb_to_linear_scalar`].
#[inline]
fn linear_to_srgb_scalar(c: f32) -> f32 {
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// Bulk sRGB u8 RGBA → linear f32 RGBA decode. Alpha stays linear per
/// sRGB spec. Used at the u8⇄linear boundary when an effect (or caller
/// that still speaks u8) needs to meet the linear-f32 `apply_effects`
/// signature.
pub fn srgb_u8_to_linear_f32_rgba(rgba: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(rgba.len());
    for chunk in rgba.chunks_exact(4) {
        out.push(srgb_to_linear_scalar(chunk[0] as f32 / 255.0));
        out.push(srgb_to_linear_scalar(chunk[1] as f32 / 255.0));
        out.push(srgb_to_linear_scalar(chunk[2] as f32 / 255.0));
        out.push(chunk[3] as f32 / 255.0);
    }
    out
}

/// Inverse of [`srgb_u8_to_linear_f32_rgba`]. Values outside `[0, 1]`
/// clamp at the u8 boundary (not earlier).
pub fn linear_f32_rgba_to_srgb_u8(linear: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(linear.len());
    for chunk in linear.chunks_exact(4) {
        out.push((linear_to_srgb_scalar(chunk[0]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.push((linear_to_srgb_scalar(chunk[1]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.push((linear_to_srgb_scalar(chunk[2]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
        out.push((chunk[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
    }
    out
}

/// Apply a chain of built-in effects to a linear-f32 RGBA frame.
///
/// **CPU fallback path.** The individual effect implementations in
/// [`opacity`], [`blur`], [`crop`], [`sharpen`], [`transform`],
/// [`chroma_key`], [`light`], and the inline `apply_*` helpers below
/// still operate on sRGB u8 buffers. Rather than rewriting every one
/// of them at once, we bridge at the entry/exit of the chain: decode
/// linear → u8 once, run the chain, re-encode u8 → linear once. This
/// keeps the chain bit-identical to the pre-refactor behaviour while
/// matching the engine-level linear plumbing.
///
/// Empty chains skip the bridge entirely so a linear-f32 passthrough
/// costs nothing.
pub fn apply_effects(effects: &[BuiltinEffect], linear_rgba: Vec<f32>, width: u32, height: u32) -> Vec<f32> {
    if effects.is_empty() {
        return linear_rgba;
    }
    let mut rgba = linear_f32_rgba_to_srgb_u8(&linear_rgba);
    for effect in effects {
        rgba = apply_one(effect, rgba, width, height);
    }
    srgb_u8_to_linear_f32_rgba(&rgba)
}

/// Returns true when the effect is at its default / identity values
/// and therefore a no-op. Lets callers strip such entries before
/// dispatching to GPU/CPU pipelines, which saves the
/// linear → upload → download round-trip on a list whose effective
/// content is nothing. Used by the per-clip effects chain to keep
/// "default Transform / Opacity 1.0 / zero-radius blur" lists from
/// triggering GPU traffic each frame.
pub fn is_identity_effect(effect: &BuiltinEffect) -> bool {
    match effect {
        BuiltinEffect::Opacity { value } => (value - 1.0).abs() < f32::EPSILON,
        BuiltinEffect::GaussianBlur { radius } => *radius < 0.5,
        BuiltinEffect::Sharpen { amount } => amount.abs() < f32::EPSILON,
        BuiltinEffect::Crop { left, right, top, bottom, feather } => {
            *left == 0.0 && *right == 0.0 && *top == 0.0 && *bottom == 0.0 && *feather == 0.0
        }
        BuiltinEffect::Transform {
            position_x, position_y, scale_x, scale_y,
            rotation, rotation_x, rotation_y, ..
        } => {
            *position_x == 0.0 && *position_y == 0.0
                && (scale_x - 1.0).abs() < f32::EPSILON
                && (scale_y - 1.0).abs() < f32::EPSILON
                && rotation.abs() < f32::EPSILON
                && rotation_x.abs() < f32::EPSILON
                && rotation_y.abs() < f32::EPSILON
        }
        // Effects whose visual contribution depends on data (chroma
        // key, bloom, glow, light rays, vignette etc.) — be
        // conservative and never claim identity. False positives
        // here would silently drop user intent.
        _ => false,
    }
}

fn apply_one(effect: &BuiltinEffect, rgba: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
    match effect {
        BuiltinEffect::Opacity { value } => opacity::apply(rgba, *value),
        BuiltinEffect::Crop { left, right, top, bottom, feather } => {
            crop::apply(rgba, width, height, *left, *right, *top, *bottom, *feather)
        }
        BuiltinEffect::GaussianBlur { radius } => blur::apply(rgba, width, height, *radius),
        BuiltinEffect::Sharpen { amount } => sharpen::apply(rgba, width, height, *amount),
        BuiltinEffect::Transform {
            position_x, position_y, scale_x, scale_y,
            rotation, anchor_x, anchor_y,
            rotation_x, rotation_y, perspective,
            focal_ref: _,
        } => {
            transform::apply(
                rgba, width, height,
                *position_x, *position_y, *scale_x, *scale_y,
                *rotation, *anchor_x, *anchor_y,
                *rotation_x, *rotation_y, *perspective,
            )
        }
        BuiltinEffect::ChromaKey { key_color, tolerance, spill_suppression } => {
            chroma_key::apply(rgba, *key_color, *tolerance, *spill_suppression)
        }
        BuiltinEffect::Bloom { threshold, radius, intensity } => {
            light::apply_bloom(rgba, width, height, *threshold, *radius, *intensity)
        }
        BuiltinEffect::Glow { radius, intensity } => {
            light::apply_glow(rgba, width, height, *radius, *intensity)
        }
        BuiltinEffect::LightRays { center_x, center_y, threshold, samples, decay, intensity } => {
            light::apply_light_rays(rgba, width, height, *center_x, *center_y, *threshold, *samples, *decay, *intensity)
        }
        BuiltinEffect::Vignette { amount, size, softness, roundness } => {
            light::apply_vignette(rgba, width, height, *amount, *size, *softness, *roundness)
        }
        BuiltinEffect::Diffusion { amount, radius, threshold, desaturation } => {
            light::apply_diffusion(rgba, width, height, *amount, *radius, *threshold, *desaturation)
        }
        BuiltinEffect::LensFlare { position_x, position_y, brightness, flare_size, ghost_count, ghost_intensity, chromatic_aberration } => {
            light::apply_lens_flare(rgba, width, height, *position_x, *position_y, *brightness, *flare_size, *ghost_count, *ghost_intensity, *chromatic_aberration)
        }
        BuiltinEffect::LightWrap { width: wrap_w, intensity, softness } => {
            light::apply_light_wrap(rgba, width, height, *wrap_w, *intensity, *softness)
        }
        BuiltinEffect::AnamorphicStreak { threshold, length, intensity, tint } => {
            light::apply_anamorphic_streak(rgba, width, height, *threshold, *length, *intensity, *tint)
        }
        BuiltinEffect::FilmHalation { threshold, radius, intensity, hue } => {
            light::apply_film_halation(rgba, width, height, *threshold, *radius, *intensity, *hue)
        }
        BuiltinEffect::Invert { mix } => {
            apply_invert(rgba, *mix)
        }
        BuiltinEffect::Posterize { levels } => {
            apply_posterize(rgba, *levels)
        }
        BuiltinEffect::Mosaic { block_size } => {
            apply_mosaic(rgba, width, height, *block_size)
        }
        BuiltinEffect::EdgeDetect { strength, mix } => {
            apply_edge_detect(rgba, width, height, *strength, *mix)
        }
        BuiltinEffect::ChannelMixer {
            red_red, red_green, red_blue,
            green_red, green_green, green_blue,
            blue_red, blue_green, blue_blue,
        } => {
            apply_channel_mixer(rgba, *red_red, *red_green, *red_blue,
                *green_red, *green_green, *green_blue,
                *blue_red, *blue_green, *blue_blue)
        }
        BuiltinEffect::DirectionalBlur { angle, distance, samples } => {
            apply_directional_blur(rgba, width, height, *angle, *distance, *samples)
        }
        BuiltinEffect::Emboss { angle, strength, mix } => {
            apply_emboss(rgba, width, height, *angle, *strength, *mix)
        }
        BuiltinEffect::Mirror { horizontal, vertical } => {
            apply_mirror(rgba, width, height, *horizontal, *vertical)
        }
        BuiltinEffect::LensDistortion { distortion, scale } => {
            apply_lens_distortion(rgba, width, height, *distortion, *scale)
        }
        BuiltinEffect::Dilate { radius } => {
            apply_dilate(rgba, width, height, *radius)
        }
        BuiltinEffect::Erode { radius } => {
            apply_erode(rgba, width, height, *radius)
        }
        BuiltinEffect::Denoise { spatial_sigma, color_sigma, iterations } => {
            apply_denoise(rgba, width, height, *spatial_sigma, *color_sigma, *iterations)
        }
        BuiltinEffect::Gamma { gamma } => {
            apply_gamma(rgba, *gamma)
        }
        BuiltinEffect::HSVAdjust { hue_shift, saturation, value } => {
            apply_hsv_adjust(rgba, *hue_shift, *saturation, *value)
        }
        BuiltinEffect::Vibrance { amount } => {
            apply_vibrance(rgba, *amount)
        }
        BuiltinEffect::NoiseGenerator { amount, monochrome, seed } => {
            apply_noise_generator(rgba, *amount, *monochrome, *seed)
        }
        BuiltinEffect::MedianFilter { radius } => {
            apply_median_filter(rgba, width, height, *radius)
        }
        BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } => {
            apply_radial_blur(rgba, width, height, *center_x, *center_y, *amount, *samples, *mode)
        }
        BuiltinEffect::Levels { in_black, in_white, gamma, out_black, out_white } => {
            apply_levels(rgba, *in_black, *in_white, *gamma, *out_black, *out_white)
        }
        BuiltinEffect::Equalize { strength } => {
            apply_equalize(rgba, *strength)
        }
        BuiltinEffect::Letterbox { aspect_ratio, bar_color, opacity } => {
            apply_letterbox(rgba, width, height, *aspect_ratio, *bar_color, *opacity)
        }
        BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
            apply_chromatic_aberration(rgba, width, height, *red_shift, *blue_shift, *center_x, *center_y)
        }
        BuiltinEffect::RollingShutterCorrection { .. } => {
            // RSC requires temporal frame access — handled at engine level.
            rgba
        }
        BuiltinEffect::VideoStabilization { .. } => {
            // Stabilization is handled at engine level (requires pre-analysis + GPU homography).
            rgba
        }
        BuiltinEffect::SpeedRamp { .. } => {
            // SpeedRamp is handled at the engine level (frame queue + GPU blend),
            // not in the per-frame effects chain. If it reaches here, pass through.
            rgba
        }
    }
}

// ─── CPU fallback implementations for new effects ───────────────────────────

fn apply_invert(mut rgba: Vec<u8>, mix: f32) -> Vec<u8> {
    let mix = mix.clamp(0.0, 1.0);
    for chunk in rgba.chunks_exact_mut(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;
        chunk[0] = ((r + (1.0 - r - r) * mix) * 255.0).clamp(0.0, 255.0) as u8;
        chunk[1] = ((g + (1.0 - g - g) * mix) * 255.0).clamp(0.0, 255.0) as u8;
        chunk[2] = ((b + (1.0 - b - b) * mix) * 255.0).clamp(0.0, 255.0) as u8;
        // alpha unchanged
    }
    rgba
}

fn apply_posterize(mut rgba: Vec<u8>, levels: f32) -> Vec<u8> {
    let levels = levels.clamp(2.0, 256.0);
    let divisor = levels - 1.0;
    for chunk in rgba.chunks_exact_mut(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;
        chunk[0] = ((r * levels).floor() / divisor * 255.0).clamp(0.0, 255.0) as u8;
        chunk[1] = ((g * levels).floor() / divisor * 255.0).clamp(0.0, 255.0) as u8;
        chunk[2] = ((b * levels).floor() / divisor * 255.0).clamp(0.0, 255.0) as u8;
    }
    rgba
}

fn apply_mosaic(rgba: Vec<u8>, width: u32, height: u32, block_size: f32) -> Vec<u8> {
    let block_size = block_size.max(1.0);
    let mut out = rgba.clone();
    for y in 0..height {
        for x in 0..width {
            let bx = ((x as f32 / block_size).floor() * block_size) as u32;
            let by = ((y as f32 / block_size).floor() * block_size) as u32;
            let bx = bx.min(width - 1);
            let by = by.min(height - 1);
            let src_idx = (by * width + bx) as usize * 4;
            let dst_idx = (y * width + x) as usize * 4;
            out[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
        }
    }
    out
}

fn apply_edge_detect(rgba: Vec<u8>, width: u32, height: u32, strength: f32, mix: f32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mix = mix.clamp(0.0, 1.0);
    let mut out = rgba.clone();

    for y in 0..h {
        for x in 0..w {
            let mut gx = 0.0f32;
            let mut gy = 0.0f32;
            let sobel_x: [[f32; 3]; 3] = [[-1.0, 0.0, 1.0], [-2.0, 0.0, 2.0], [-1.0, 0.0, 1.0]];
            let sobel_y: [[f32; 3]; 3] = [[-1.0, -2.0, -1.0], [0.0, 0.0, 0.0], [1.0, 2.0, 1.0]];

            for ky in 0..3i32 {
                for kx in 0..3i32 {
                    let sx = (x as i32 + kx - 1).clamp(0, w as i32 - 1) as usize;
                    let sy = (y as i32 + ky - 1).clamp(0, h as i32 - 1) as usize;
                    let idx = (sy * w + sx) * 4;
                    let r = rgba[idx] as f32 / 255.0;
                    let g = rgba[idx + 1] as f32 / 255.0;
                    let b = rgba[idx + 2] as f32 / 255.0;
                    let lum = 0.299 * r + 0.587 * g + 0.114 * b;
                    gx += lum * sobel_x[ky as usize][kx as usize];
                    gy += lum * sobel_y[ky as usize][kx as usize];
                }
            }

            let edge = (gx * gx + gy * gy).sqrt() * strength;
            let edge = edge.clamp(0.0, 1.0);

            let dst_idx = (y * w + x) * 4;
            let orig_r = rgba[dst_idx] as f32 / 255.0;
            let orig_g = rgba[dst_idx + 1] as f32 / 255.0;
            let orig_b = rgba[dst_idx + 2] as f32 / 255.0;
            out[dst_idx] = ((orig_r * (1.0 - mix) + edge * mix) * 255.0).clamp(0.0, 255.0) as u8;
            out[dst_idx + 1] = ((orig_g * (1.0 - mix) + edge * mix) * 255.0).clamp(0.0, 255.0) as u8;
            out[dst_idx + 2] = ((orig_b * (1.0 - mix) + edge * mix) * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn apply_channel_mixer(
    mut rgba: Vec<u8>,
    rr: f32, rg: f32, rb: f32,
    gr: f32, gg: f32, gb: f32,
    br: f32, bg: f32, bb: f32,
) -> Vec<u8> {
    for chunk in rgba.chunks_exact_mut(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;
        chunk[0] = ((rr * r + rg * g + rb * b) * 255.0).clamp(0.0, 255.0) as u8;
        chunk[1] = ((gr * r + gg * g + gb * b) * 255.0).clamp(0.0, 255.0) as u8;
        chunk[2] = ((br * r + bg * g + bb * b) * 255.0).clamp(0.0, 255.0) as u8;
    }
    rgba
}

fn apply_directional_blur(rgba: Vec<u8>, width: u32, height: u32, angle: f32, distance: f32, samples: u32) -> Vec<u8> {
    let samples = samples.clamp(3, 32);
    if distance < 0.5 || samples < 2 {
        return rgba;
    }
    let w = width as i32;
    let h = height as i32;
    let angle_rad = angle.to_radians();
    let dx = angle_rad.cos() * distance / samples as f32;
    let dy = angle_rad.sin() * distance / samples as f32;

    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let mut sum_r = 0.0f32;
            let mut sum_g = 0.0f32;
            let mut sum_b = 0.0f32;
            let mut sum_a = 0.0f32;
            for i in 0..samples as i32 {
                let sx = (x as f32 + dx * i as f32).round() as i32;
                let sy = (y as f32 + dy * i as f32).round() as i32;
                let sx = sx.clamp(0, w - 1);
                let sy = sy.clamp(0, h - 1);
                let idx = (sy * w + sx) as usize * 4;
                sum_r += rgba[idx] as f32;
                sum_g += rgba[idx + 1] as f32;
                sum_b += rgba[idx + 2] as f32;
                sum_a += rgba[idx + 3] as f32;
            }
            let inv = 1.0 / samples as f32;
            let dst_idx = (y * w + x) as usize * 4;
            out[dst_idx] = (sum_r * inv).clamp(0.0, 255.0) as u8;
            out[dst_idx + 1] = (sum_g * inv).clamp(0.0, 255.0) as u8;
            out[dst_idx + 2] = (sum_b * inv).clamp(0.0, 255.0) as u8;
            out[dst_idx + 3] = (sum_a * inv).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn apply_emboss(rgba: Vec<u8>, width: u32, height: u32, angle: f32, strength: f32, mix: f32) -> Vec<u8> {
    let w = width as i32;
    let h = height as i32;
    let mix = mix.clamp(0.0, 1.0);
    let strength = strength.clamp(0.0, 5.0);
    let angle_rad = angle.to_radians();
    let dx = angle_rad.cos();
    let dy = angle_rad.sin();

    let mut out = rgba.clone();
    for y in 1..(h - 1) {
        for x in 1..(w - 1) {
            let idx = (y * w + x) as usize * 4;
            // Sample in light direction and opposite
            let sx1 = (x as f32 + dx).round().clamp(0.0, (w - 1) as f32) as usize;
            let sy1 = (y as f32 + dy).round().clamp(0.0, (h - 1) as f32) as usize;
            let sx2 = (x as f32 - dx).round().clamp(0.0, (w - 1) as f32) as usize;
            let sy2 = (y as f32 - dy).round().clamp(0.0, (h - 1) as f32) as usize;
            let i1 = (sy1 * w as usize + sx1) * 4;
            let i2 = (sy2 * w as usize + sx2) * 4;

            for ch in 0..3 {
                let diff = (rgba[i1 + ch] as f32 - rgba[i2 + ch] as f32) * strength;
                let emboss = (128.0 + diff).clamp(0.0, 255.0);
                let orig = rgba[idx + ch] as f32;
                out[idx + ch] = (orig + (emboss - orig) * mix).clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

fn apply_mirror(rgba: Vec<u8>, width: u32, height: u32, horizontal: u32, vertical: u32) -> Vec<u8> {
    if horizontal == 0 && vertical == 0 {
        return rgba;
    }
    let w = width as usize;
    let h = height as usize;
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let sx = if horizontal != 0 { w - 1 - x } else { x };
            let sy = if vertical != 0 { h - 1 - y } else { y };
            let src_idx = (sy * w + sx) * 4;
            let dst_idx = (y * w + x) * 4;
            out[dst_idx..dst_idx + 4].copy_from_slice(&rgba[src_idx..src_idx + 4]);
        }
    }
    out
}

fn apply_lens_distortion(rgba: Vec<u8>, width: u32, height: u32, distortion: f32, scale: f32) -> Vec<u8> {
    if distortion.abs() < f32::EPSILON && (scale - 1.0).abs() < f32::EPSILON {
        return rgba;
    }
    let w = width as usize;
    let h = height as usize;
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let mut out = vec![0u8; rgba.len()];
    for y in 0..h {
        for x in 0..w {
            let nx = (x as f32 - cx) / cx;
            let ny = (y as f32 - cy) / cy;
            let r2 = nx * nx + ny * ny;
            let factor = 1.0 + distortion * r2;
            let sx = (nx * factor / scale) * cx + cx;
            let sy = (ny * factor / scale) * cy + cy;
            // Bilinear interpolation
            let x0 = sx.floor() as i32;
            let y0 = sy.floor() as i32;
            let x1 = x0 + 1;
            let y1 = y0 + 1;
            let fx = sx - x0 as f32;
            let fy = sy - y0 as f32;
            let x0c = x0.clamp(0, w as i32 - 1) as usize;
            let y0c = y0.clamp(0, h as i32 - 1) as usize;
            let x1c = x1.clamp(0, w as i32 - 1) as usize;
            let y1c = y1.clamp(0, h as i32 - 1) as usize;
            let dst_idx = (y * w + x) * 4;
            for ch in 0..4 {
                let p00 = rgba[(y0c * w + x0c) * 4 + ch] as f32;
                let p10 = rgba[(y0c * w + x1c) * 4 + ch] as f32;
                let p01 = rgba[(y1c * w + x0c) * 4 + ch] as f32;
                let p11 = rgba[(y1c * w + x1c) * 4 + ch] as f32;
                let top = p00 + (p10 - p00) * fx;
                let bot = p01 + (p11 - p01) * fx;
                out[dst_idx + ch] = (top + (bot - top) * fy).clamp(0.0, 255.0) as u8;
            }
        }
    }
    out
}

fn apply_dilate(rgba: Vec<u8>, width: u32, height: u32, radius: u32) -> Vec<u8> {
    let w = width as i32;
    let h = height as i32;
    let r = radius.clamp(1, 10) as i32;
    let mut out = rgba.clone();
    for y in 0..h {
        for x in 0..w {
            let mut max_r = 0u8;
            let mut max_g = 0u8;
            let mut max_b = 0u8;
            let mut max_a = 0u8;
            for dy in -r..=r {
                for dx in -r..=r {
                    let sx = (x + dx).clamp(0, w - 1) as usize;
                    let sy = (y + dy).clamp(0, h - 1) as usize;
                    let idx = (sy * width as usize + sx) * 4;
                    max_r = max_r.max(rgba[idx]);
                    max_g = max_g.max(rgba[idx + 1]);
                    max_b = max_b.max(rgba[idx + 2]);
                    max_a = max_a.max(rgba[idx + 3]);
                }
            }
            let dst_idx = (y as usize * width as usize + x as usize) * 4;
            out[dst_idx] = max_r;
            out[dst_idx + 1] = max_g;
            out[dst_idx + 2] = max_b;
            out[dst_idx + 3] = max_a;
        }
    }
    out
}

fn apply_erode(rgba: Vec<u8>, width: u32, height: u32, radius: u32) -> Vec<u8> {
    let w = width as i32;
    let h = height as i32;
    let r = radius.clamp(1, 10) as i32;
    let mut out = rgba.clone();
    for y in 0..h {
        for x in 0..w {
            let mut min_r = 255u8;
            let mut min_g = 255u8;
            let mut min_b = 255u8;
            let mut min_a = 255u8;
            for dy in -r..=r {
                for dx in -r..=r {
                    let sx = (x + dx).clamp(0, w - 1) as usize;
                    let sy = (y + dy).clamp(0, h - 1) as usize;
                    let idx = (sy * width as usize + sx) * 4;
                    min_r = min_r.min(rgba[idx]);
                    min_g = min_g.min(rgba[idx + 1]);
                    min_b = min_b.min(rgba[idx + 2]);
                    min_a = min_a.min(rgba[idx + 3]);
                }
            }
            let dst_idx = (y as usize * width as usize + x as usize) * 4;
            out[dst_idx] = min_r;
            out[dst_idx + 1] = min_g;
            out[dst_idx + 2] = min_b;
            out[dst_idx + 3] = min_a;
        }
    }
    out
}

fn apply_denoise(rgba: Vec<u8>, width: u32, height: u32, spatial_sigma: f32, color_sigma: f32, iterations: u32) -> Vec<u8> {
    let w = width as i32;
    let h = height as i32;
    let iterations = iterations.clamp(1, 3);
    let spatial_sigma = spatial_sigma.clamp(1.0, 20.0);
    let color_sigma = color_sigma.clamp(0.01, 1.0);
    let kernel_size = (spatial_sigma * 2.0).ceil() as i32;
    let kernel_size = kernel_size.min(10);
    let spatial_denom = 2.0 * spatial_sigma * spatial_sigma;
    let color_denom = 2.0 * color_sigma * color_sigma;

    let mut current = rgba;
    for _ in 0..iterations {
        let mut out = vec![0u8; current.len()];
        for y in 0..h {
            for x in 0..w {
                let cidx = (y as usize * width as usize + x as usize) * 4;
                let cr = current[cidx] as f32 / 255.0;
                let cg = current[cidx + 1] as f32 / 255.0;
                let cb = current[cidx + 2] as f32 / 255.0;
                let center_luma = 0.299 * cr + 0.587 * cg + 0.114 * cb;

                let mut sum_r = 0.0f32;
                let mut sum_g = 0.0f32;
                let mut sum_b = 0.0f32;
                let mut total_weight = 0.0f32;

                for dy in -kernel_size..=kernel_size {
                    for dx in -kernel_size..=kernel_size {
                        let sx = (x + dx).clamp(0, w - 1) as usize;
                        let sy = (y + dy).clamp(0, h - 1) as usize;
                        let idx = (sy * width as usize + sx) * 4;
                        let nr = current[idx] as f32 / 255.0;
                        let ng = current[idx + 1] as f32 / 255.0;
                        let nb = current[idx + 2] as f32 / 255.0;
                        let n_luma = 0.299 * nr + 0.587 * ng + 0.114 * nb;

                        let spatial_dist = (dx * dx + dy * dy) as f32;
                        let spatial_w = (-spatial_dist / spatial_denom).exp();
                        let color_diff = center_luma - n_luma;
                        let color_w = (-color_diff * color_diff / color_denom).exp();
                        let weight = spatial_w * color_w;

                        sum_r += nr * weight;
                        sum_g += ng * weight;
                        sum_b += nb * weight;
                        total_weight += weight;
                    }
                }

                let inv = 1.0 / total_weight;
                out[cidx] = (sum_r * inv * 255.0).clamp(0.0, 255.0) as u8;
                out[cidx + 1] = (sum_g * inv * 255.0).clamp(0.0, 255.0) as u8;
                out[cidx + 2] = (sum_b * inv * 255.0).clamp(0.0, 255.0) as u8;
                out[cidx + 3] = current[cidx + 3]; // preserve alpha
            }
        }
        current = out;
    }
    current
}

fn apply_hsv_adjust(mut rgba: Vec<u8>, hue_shift: f32, saturation: f32, value: f32) -> Vec<u8> {
    if hue_shift.abs() < f32::EPSILON && (saturation - 1.0).abs() < f32::EPSILON && (value - 1.0).abs() < f32::EPSILON {
        return rgba;
    }
    for chunk in rgba.chunks_exact_mut(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;

        let cmax = r.max(g).max(b);
        let cmin = r.min(g).min(b);
        let delta = cmax - cmin;

        let mut h = 0.0f32;
        if delta > 0.001 {
            if (cmax - r).abs() < f32::EPSILON {
                h = ((g - b) / delta) % 6.0;
            } else if (cmax - g).abs() < f32::EPSILON {
                h = (b - r) / delta + 2.0;
            } else {
                h = (r - g) / delta + 4.0;
            }
            h *= 60.0;
            if h < 0.0 { h += 360.0; }
        }
        let s = if cmax > 0.001 { delta / cmax } else { 0.0 };
        let v = cmax;

        // Apply adjustments
        h = (h + hue_shift + 360.0) % 360.0;
        let new_s = (s * saturation).clamp(0.0, 1.0);
        let new_v = (v * value).clamp(0.0, 1.0);

        // HSV to RGB
        let c = new_v * new_s;
        let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
        let m = new_v - c;
        let (rr, gg, bb) = if h < 60.0 { (c, x, 0.0) }
            else if h < 120.0 { (x, c, 0.0) }
            else if h < 180.0 { (0.0, c, x) }
            else if h < 240.0 { (0.0, x, c) }
            else if h < 300.0 { (x, 0.0, c) }
            else { (c, 0.0, x) };

        chunk[0] = ((rr + m) * 255.0).clamp(0.0, 255.0) as u8;
        chunk[1] = ((gg + m) * 255.0).clamp(0.0, 255.0) as u8;
        chunk[2] = ((bb + m) * 255.0).clamp(0.0, 255.0) as u8;
    }
    rgba
}

/// Vibrance: selective saturation that increases saturation of muted colors more than
/// already-saturated colors. Negative values desaturate.
fn apply_vibrance(mut rgba: Vec<u8>, amount: f32) -> Vec<u8> {
    if amount.abs() < 0.001 { return rgba; }
    for chunk in rgba.chunks_exact_mut(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;
        let max_c = r.max(g).max(b);
        let min_c = r.min(g).min(b);
        let sat = if max_c > 0.001 { (max_c - min_c) / max_c } else { 0.0 };
        // Scale effect inversely with current saturation: low-sat colors get more boost
        let scale = amount * (1.0 - sat);
        let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let nr = (luma + (r - luma) * (1.0 + scale)).clamp(0.0, 1.0);
        let ng = (luma + (g - luma) * (1.0 + scale)).clamp(0.0, 1.0);
        let nb = (luma + (b - luma) * (1.0 + scale)).clamp(0.0, 1.0);
        chunk[0] = (nr * 255.0 + 0.5) as u8;
        chunk[1] = (ng * 255.0 + 0.5) as u8;
        chunk[2] = (nb * 255.0 + 0.5) as u8;
    }
    rgba
}

fn apply_noise_generator(mut rgba: Vec<u8>, amount: f32, monochrome: u32, seed: u32) -> Vec<u8> {
    if amount < f32::EPSILON {
        return rgba;
    }
    fn hash(p: u32) -> f32 {
        let mut x = p;
        x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
        x = ((x >> 16) ^ x).wrapping_mul(0x45d9f3b);
        x = (x >> 16) ^ x;
        x as f32 / 4294967295.0
    }
    for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
        let noise_seed = (i as u32).wrapping_mul(7919).wrapping_add(seed);
        if monochrome != 0 {
            let n = (hash(noise_seed) - 0.5) * 2.0 * amount;
            let nv = (n * 255.0) as i32;
            chunk[0] = (chunk[0] as i32 + nv).clamp(0, 255) as u8;
            chunk[1] = (chunk[1] as i32 + nv).clamp(0, 255) as u8;
            chunk[2] = (chunk[2] as i32 + nv).clamp(0, 255) as u8;
        } else {
            let nr = (hash(noise_seed) - 0.5) * 2.0 * amount;
            let ng = (hash(noise_seed.wrapping_add(1)) - 0.5) * 2.0 * amount;
            let nb = (hash(noise_seed.wrapping_add(2)) - 0.5) * 2.0 * amount;
            chunk[0] = (chunk[0] as i32 + (nr * 255.0) as i32).clamp(0, 255) as u8;
            chunk[1] = (chunk[1] as i32 + (ng * 255.0) as i32).clamp(0, 255) as u8;
            chunk[2] = (chunk[2] as i32 + (nb * 255.0) as i32).clamp(0, 255) as u8;
        }
    }
    rgba
}

fn apply_median_filter(rgba: Vec<u8>, width: u32, height: u32, radius: u32) -> Vec<u8> {
    let r = radius.clamp(1, 5) as i32;
    let w = width as i32;
    let h = height as i32;
    let mut out = rgba.clone();
    for y in 0..h {
        for x in 0..w {
            let mut vals_r: Vec<u8> = Vec::new();
            let mut vals_g: Vec<u8> = Vec::new();
            let mut vals_b: Vec<u8> = Vec::new();
            for dy in -r..=r {
                for dx in -r..=r {
                    let sx = (x + dx).clamp(0, w - 1) as usize;
                    let sy = (y + dy).clamp(0, h - 1) as usize;
                    let idx = (sy * width as usize + sx) * 4;
                    vals_r.push(rgba[idx]);
                    vals_g.push(rgba[idx + 1]);
                    vals_b.push(rgba[idx + 2]);
                }
            }
            vals_r.sort_unstable();
            vals_g.sort_unstable();
            vals_b.sort_unstable();
            let mid = vals_r.len() / 2;
            let dst_idx = (y as usize * width as usize + x as usize) * 4;
            out[dst_idx] = vals_r[mid];
            out[dst_idx + 1] = vals_g[mid];
            out[dst_idx + 2] = vals_b[mid];
            // alpha unchanged
        }
    }
    out
}

fn apply_radial_blur(rgba: Vec<u8>, width: u32, height: u32, center_x: f32, center_y: f32, amount: f32, samples: u32, mode: u32) -> Vec<u8> {
    let samples = samples.clamp(4, 32);
    if amount < 0.01 || samples < 2 {
        return rgba;
    }
    let w = width as i32;
    let h = height as i32;
    let cx = center_x * width as f32;
    let cy = center_y * height as f32;
    let mut out = vec![0u8; rgba.len()];

    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let mut sum_r = 0.0f32;
            let mut sum_g = 0.0f32;
            let mut sum_b = 0.0f32;
            let mut sum_a = 0.0f32;

            if mode == 0 {
                // Zoom blur
                let dist = (dx * dx + dy * dy).sqrt().max(1.0);
                let scale = amount / samples as f32 / dist;
                for i in 0..samples {
                    let t = (i as f32 / samples as f32 - 0.5) * 2.0;
                    let sx = (x as f32 + dx * t * scale).round().clamp(0.0, (w - 1) as f32) as usize;
                    let sy = (y as f32 + dy * t * scale).round().clamp(0.0, (h - 1) as f32) as usize;
                    let idx = (sy * width as usize + sx) * 4;
                    sum_r += rgba[idx] as f32;
                    sum_g += rgba[idx + 1] as f32;
                    sum_b += rgba[idx + 2] as f32;
                    sum_a += rgba[idx + 3] as f32;
                }
            } else {
                // Spin blur
                let angle_step = amount * std::f32::consts::PI / 180.0 / samples as f32;
                for i in 0..samples {
                    let t = (i as f32 / samples as f32 - 0.5) * 2.0;
                    let a = t * angle_step;
                    let cos_a = a.cos();
                    let sin_a = a.sin();
                    let sx = (cx + dx * cos_a - dy * sin_a).round().clamp(0.0, (w - 1) as f32) as usize;
                    let sy = (cy + dx * sin_a + dy * cos_a).round().clamp(0.0, (h - 1) as f32) as usize;
                    let idx = (sy * width as usize + sx) * 4;
                    sum_r += rgba[idx] as f32;
                    sum_g += rgba[idx + 1] as f32;
                    sum_b += rgba[idx + 2] as f32;
                    sum_a += rgba[idx + 3] as f32;
                }
            }

            let inv = 1.0 / samples as f32;
            let dst_idx = (y as usize * width as usize + x as usize) * 4;
            out[dst_idx] = (sum_r * inv).clamp(0.0, 255.0) as u8;
            out[dst_idx + 1] = (sum_g * inv).clamp(0.0, 255.0) as u8;
            out[dst_idx + 2] = (sum_b * inv).clamp(0.0, 255.0) as u8;
            out[dst_idx + 3] = (sum_a * inv).clamp(0.0, 255.0) as u8;
        }
    }
    out
}

fn apply_levels(mut rgba: Vec<u8>, in_black: f32, in_white: f32, gamma: f32, out_black: f32, out_white: f32) -> Vec<u8> {
    let in_range = (in_white - in_black).max(0.001);
    let inv_gamma = if gamma.abs() > f32::EPSILON { 1.0 / gamma } else { 1.0 };
    for chunk in rgba.chunks_exact_mut(4) {
        for ch in 0..3 {
            let v = chunk[ch] as f32 / 255.0;
            let mapped = ((v - in_black) / in_range).clamp(0.0, 1.0);
            let gamma_corrected = mapped.powf(inv_gamma);
            let result = out_black + gamma_corrected * (out_white - out_black);
            chunk[ch] = (result * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
    rgba
}

fn apply_equalize(rgba: Vec<u8>, strength: f32) -> Vec<u8> {
    if strength < f32::EPSILON {
        return rgba;
    }
    let pixel_count = rgba.len() / 4;
    if pixel_count == 0 {
        return rgba;
    }

    // Build luminance histogram (256 bins)
    let mut hist = [0u32; 256];
    for chunk in rgba.chunks_exact(4) {
        let r = chunk[0] as f32;
        let g = chunk[1] as f32;
        let b = chunk[2] as f32;
        let lum = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as u8;
        hist[lum as usize] += 1;
    }

    // Build CDF
    let mut cdf = [0.0f32; 256];
    let mut cumulative = 0u32;
    let first_nonzero = hist.iter().position(|&v| v > 0).unwrap_or(0);
    let cdf_min = hist[first_nonzero] as f32;
    let denom = pixel_count as f32 - cdf_min;
    if denom < 1.0 {
        // All pixels have the same luminance — nothing to equalize
        return rgba;
    }
    for i in 0..256 {
        cumulative += hist[i];
        cdf[i] = ((cumulative as f32 - cdf_min) / denom * 255.0).clamp(0.0, 255.0);
    }

    // Remap pixels: scale RGB proportionally based on luminance remapping
    let mut out = rgba;
    for chunk in out.chunks_exact_mut(4) {
        let r = chunk[0] as f32;
        let g = chunk[1] as f32;
        let b = chunk[2] as f32;
        let lum = (0.299 * r + 0.587 * g + 0.114 * b).clamp(0.0, 255.0) as u8;
        let eq_lum = cdf[lum as usize];
        let scale = if lum > 0 { eq_lum / lum as f32 } else { 1.0 };
        // Blend with strength
        let s = strength;
        chunk[0] = (r + (r * scale - r) * s).clamp(0.0, 255.0) as u8;
        chunk[1] = (g + (g * scale - g) * s).clamp(0.0, 255.0) as u8;
        chunk[2] = (b + (b * scale - b) * s).clamp(0.0, 255.0) as u8;
    }
    out
}

fn apply_gamma(mut rgba: Vec<u8>, gamma: f32) -> Vec<u8> {
    if (gamma - 1.0).abs() < f32::EPSILON {
        return rgba;
    }
    let inv_gamma = 1.0 / gamma;
    for chunk in rgba.chunks_exact_mut(4) {
        let r = chunk[0] as f32 / 255.0;
        let g = chunk[1] as f32 / 255.0;
        let b = chunk[2] as f32 / 255.0;
        chunk[0] = (r.powf(inv_gamma) * 255.0).clamp(0.0, 255.0) as u8;
        chunk[1] = (g.powf(inv_gamma) * 255.0).clamp(0.0, 255.0) as u8;
        chunk[2] = (b.powf(inv_gamma) * 255.0).clamp(0.0, 255.0) as u8;
        // alpha unchanged
    }
    rgba
}

fn apply_letterbox(mut rgba: Vec<u8>, width: u32, height: u32, aspect_ratio: f32, bar_color: u32, opacity: f32) -> Vec<u8> {
    if aspect_ratio <= 0.0 || opacity <= 0.0 {
        return rgba;
    }
    let frame_aspect = width as f32 / height as f32;
    let opacity = opacity.clamp(0.0, 1.0);
    let bar_val = if bar_color != 0 { 255u8 } else { 0u8 };

    let mut bar_height = 0.0f32;
    let mut bar_width = 0.0f32;
    if frame_aspect > aspect_ratio {
        // Frame wider than target: pillarbox (bars on sides)
        let visible_w = height as f32 * aspect_ratio;
        bar_width = (width as f32 - visible_w) / 2.0;
    } else {
        // Frame taller than target: letterbox (bars top/bottom)
        let visible_h = width as f32 / aspect_ratio;
        bar_height = (height as f32 - visible_h) / 2.0;
    }

    for y in 0..height {
        for x in 0..width {
            let in_bar = (y as f32) < bar_height || (y as f32) >= height as f32 - bar_height
                || (x as f32) < bar_width || (x as f32) >= width as f32 - bar_width;
            if in_bar {
                let idx = (y * width + x) as usize * 4;
                let orig_r = rgba[idx] as f32;
                let orig_g = rgba[idx + 1] as f32;
                let orig_b = rgba[idx + 2] as f32;
                rgba[idx] = (orig_r * (1.0 - opacity) + bar_val as f32 * opacity).clamp(0.0, 255.0) as u8;
                rgba[idx + 1] = (orig_g * (1.0 - opacity) + bar_val as f32 * opacity).clamp(0.0, 255.0) as u8;
                rgba[idx + 2] = (orig_b * (1.0 - opacity) + bar_val as f32 * opacity).clamp(0.0, 255.0) as u8;
                // Alpha: blend toward opaque
                let orig_a = rgba[idx + 3] as f32;
                rgba[idx + 3] = (orig_a * (1.0 - opacity) + 255.0 * opacity).clamp(0.0, 255.0) as u8;
            }
        }
    }
    rgba
}

fn apply_chromatic_aberration(rgba: Vec<u8>, width: u32, height: u32, red_shift: f32, blue_shift: f32, center_x: f32, center_y: f32) -> Vec<u8> {
    if red_shift.abs() < f32::EPSILON && blue_shift.abs() < f32::EPSILON {
        return rgba;
    }
    let w = width as i32;
    let h = height as i32;
    let cx = center_x * width as f32;
    let cy = center_y * height as f32;
    let max_dist = (cx * cx + cy * cy).sqrt().max(1.0);

    let mut out = rgba.clone();
    for y in 0..h {
        for x in 0..w {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let norm_dist = dist / max_dist;

            // Red channel: shifted
            let r_scale = 1.0 + red_shift * norm_dist / max_dist;
            let r_sx = (cx + dx * r_scale).round() as i32;
            let r_sy = (cy + dy * r_scale).round() as i32;
            let r_sx = r_sx.clamp(0, w - 1) as usize;
            let r_sy = r_sy.clamp(0, h - 1) as usize;
            let r_idx = (r_sy * width as usize + r_sx) * 4;

            // Blue channel: shifted
            let b_scale = 1.0 + blue_shift * norm_dist / max_dist;
            let b_sx = (cx + dx * b_scale).round() as i32;
            let b_sy = (cy + dy * b_scale).round() as i32;
            let b_sx = b_sx.clamp(0, w - 1) as usize;
            let b_sy = b_sy.clamp(0, h - 1) as usize;
            let b_idx = (b_sy * width as usize + b_sx) * 4;

            let dst_idx = (y as usize * width as usize + x as usize) * 4;
            out[dst_idx] = rgba[r_idx];         // R from shifted
            // G stays from original position (already correct in clone)
            out[dst_idx + 2] = rgba[b_idx];     // B from shifted
            // Alpha from original
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: u8-in, u8-out bridge around the new linear-f32
    /// [`apply_effects`]. Keeps the existing u8 assertions meaningful
    /// without rewriting each test after the linear-edge-to-edge refactor.
    fn apply_effects(effects: &[BuiltinEffect], rgba: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
        let linear = srgb_u8_to_linear_f32_rgba(&rgba);
        let out = super::apply_effects(effects, linear, width, height);
        linear_f32_rgba_to_srgb_u8(&out)
    }

    #[test]
    fn focal_ref_tracker_round_trips_through_serde() {
        // HZ-MATTE-NEXT-TRACKER-DRIVER commit 1 — additive growth
        // pin. Adding the Tracker variant is purely additive: old
        // files with Glyph still load (existing tests cover that),
        // and new files with Tracker round-trip cleanly here.
        let cid = uuid::Uuid::new_v4();
        let fr = FocalRef::Tracker { clip_id: cid, point_index: 4, fallback_xy: (0.3, 0.7) };
        let json = serde_json::to_string(&fr).expect("serialise");
        let back: FocalRef = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, fr);
    }

    #[test]
    fn is_identity_effect_classifies_default_values() {
        // Default-valued effects with no visual contribution.
        assert!(is_identity_effect(&BuiltinEffect::Opacity { value: 1.0 }));
        assert!(is_identity_effect(&BuiltinEffect::GaussianBlur { radius: 0.0 }));
        assert!(is_identity_effect(&BuiltinEffect::GaussianBlur { radius: 0.4 }));
        assert!(is_identity_effect(&BuiltinEffect::Sharpen { amount: 0.0 }));
        assert!(is_identity_effect(&BuiltinEffect::Crop {
            left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0,
        }));
        assert!(is_identity_effect(&BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        }));

        // Effects whose visual contribution is non-trivial when set.
        assert!(!is_identity_effect(&BuiltinEffect::Opacity { value: 0.5 }));
        assert!(!is_identity_effect(&BuiltinEffect::GaussianBlur { radius: 5.0 }));
        assert!(!is_identity_effect(&BuiltinEffect::Sharpen { amount: 0.3 }));
        assert!(!is_identity_effect(&BuiltinEffect::Crop {
            left: 0.1, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0,
        }));
        assert!(!is_identity_effect(&BuiltinEffect::Transform {
            position_x: 10.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        }));
    }

    #[test]
    fn test_invert_full() {
        let rgba = vec![100, 150, 200, 255];
        let result = apply_invert(rgba, 1.0);
        // Allow +/-1 for floating point rounding
        assert!((result[0] as i32 - 155).abs() <= 1, "r={}", result[0]);
        assert!((result[1] as i32 - 105).abs() <= 1, "g={}", result[1]);
        assert!((result[2] as i32 - 55).abs() <= 1, "b={}", result[2]);
        assert_eq!(result[3], 255);
    }

    #[test]
    fn test_invert_zero_mix() {
        let rgba = vec![100, 150, 200, 255];
        let result = apply_invert(rgba, 0.0);
        assert_eq!(result, vec![100, 150, 200, 255]);
    }

    #[test]
    fn test_posterize() {
        let rgba = vec![100, 150, 200, 255];
        let result = apply_posterize(rgba, 4.0);
        // r = floor(100/255 * 4) / 3 * 255 = floor(1.568) / 3 * 255 = 1/3*255 = 85
        // g = floor(150/255 * 4) / 3 * 255 = floor(2.352) / 3 * 255 = 2/3*255 = 170
        // b = floor(200/255 * 4) / 3 * 255 = floor(3.137) / 3 * 255 = 3/3*255 = 255
        assert_eq!(result[0], 85);
        assert_eq!(result[1], 170);
        assert_eq!(result[2], 255);
        assert_eq!(result[3], 255);
    }

    #[test]
    fn test_mosaic_block() {
        // 4x4 image, each pixel has unique value
        let mut rgba = vec![0u8; 4 * 4 * 4];
        for y in 0..4u32 {
            for x in 0..4u32 {
                let idx = (y * 4 + x) as usize * 4;
                rgba[idx] = (y * 4 + x) as u8 * 10;
                rgba[idx + 1] = 0;
                rgba[idx + 2] = 0;
                rgba[idx + 3] = 255;
            }
        }
        let result = apply_mosaic(rgba, 4, 4, 2.0);
        // Pixels (0,0), (1,0), (0,1), (1,1) should all equal pixel (0,0)
        let tl = result[0]; // pixel (0,0).r = 0
        assert_eq!(result[1 * 4], tl); // pixel (1,0).r
        assert_eq!(result[4 * 4], tl); // pixel (0,1).r
        assert_eq!(result[5 * 4], tl); // pixel (1,1).r
        // Pixels (2,0), (3,0), (2,1), (3,1) should all equal pixel (2,0)
        let tr = result[2 * 4]; // pixel (2,0).r = 20
        assert_eq!(tr, 20);
        assert_eq!(result[3 * 4], tr);
        assert_eq!(result[6 * 4], tr);
        assert_eq!(result[7 * 4], tr);
    }

    #[test]
    fn test_channel_mixer_identity() {
        let rgba = vec![100, 150, 200, 255];
        let result = apply_channel_mixer(rgba.clone(),
            1.0, 0.0, 0.0,
            0.0, 1.0, 0.0,
            0.0, 0.0, 1.0);
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_edge_detect_uniform() {
        // A uniform 4x4 image should produce no edges
        let rgba = vec![128, 128, 128, 255].repeat(16);
        let result = apply_edge_detect(rgba, 4, 4, 1.0, 1.0);
        // All edge values should be 0 (black)
        for chunk in result.chunks(4) {
            assert_eq!(chunk[0], 0);
            assert_eq!(chunk[1], 0);
            assert_eq!(chunk[2], 0);
        }
    }

    #[test]
    fn test_emboss_uniform() {
        // Uniform image: all differences are 0, emboss → neutral gray (128)
        let rgba = vec![100, 100, 100, 255].repeat(16);
        let result = apply_emboss(rgba, 4, 4, 135.0, 1.0, 1.0);
        // Interior pixels should be ~128 (neutral gray) since no edges
        let mid = &result[5 * 4..6 * 4]; // pixel (1,1)
        assert!((mid[0] as i32 - 128).unsigned_abs() <= 1);
        assert!((mid[1] as i32 - 128).unsigned_abs() <= 1);
        assert!((mid[2] as i32 - 128).unsigned_abs() <= 1);
    }

    #[test]
    fn test_emboss_mix_zero() {
        // mix=0 should return original
        let rgba = vec![100, 150, 200, 255].repeat(16);
        let result = apply_emboss(rgba.clone(), 4, 4, 135.0, 1.0, 0.0);
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_mirror_horizontal() {
        // 2x2 image: [A, B, C, D] → horizontal mirror → [B, A, D, C]
        let rgba = vec![
            10, 20, 30, 255,  50, 60, 70, 255,   // row 0: A, B
            80, 90, 100, 255, 110, 120, 130, 255, // row 1: C, D
        ];
        let result = apply_mirror(rgba, 2, 2, 1, 0);
        // (0,0) should now be B, (1,0) should be A
        assert_eq!(&result[0..4], &[50, 60, 70, 255]);
        assert_eq!(&result[4..8], &[10, 20, 30, 255]);
        // (0,1) should be D, (1,1) should be C
        assert_eq!(&result[8..12], &[110, 120, 130, 255]);
        assert_eq!(&result[12..16], &[80, 90, 100, 255]);
    }

    #[test]
    fn test_lens_distortion_passthrough() {
        // distortion=0, scale=1 should be identity
        let rgba = vec![100, 150, 200, 255].repeat(16);
        let result = apply_lens_distortion(rgba.clone(), 4, 4, 0.0, 1.0);
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_dilate_bright_pixel() {
        // 4x4 dark image with one bright pixel at (1,1)
        let mut rgba = vec![0, 0, 0, 255].repeat(16);
        let idx = (1 * 4 + 1) * 4;
        rgba[idx] = 200; rgba[idx + 1] = 200; rgba[idx + 2] = 200;
        let result = apply_dilate(rgba, 4, 4, 1);
        // Neighboring pixels should have picked up the bright value.
        // Index of pixel (0,0).R at (y*4+x)*4 stride = 0.
        let neighbor_idx = 0usize;
        assert_eq!(result[neighbor_idx], 200);
    }

    #[test]
    fn test_erode_dark_pixel() {
        // 4x4 bright image with one dark pixel at (1,1)
        let mut rgba = vec![200, 200, 200, 255].repeat(16);
        let idx = (1 * 4 + 1) * 4;
        rgba[idx] = 10; rgba[idx + 1] = 10; rgba[idx + 2] = 10;
        let result = apply_erode(rgba, 4, 4, 1);
        // Neighboring pixels should have picked up the dark value.
        // Index of pixel (0,0).R at (y*4+x)*4 stride = 0.
        let neighbor_idx = 0usize;
        assert_eq!(result[neighbor_idx], 10);
    }

    #[test]
    fn test_denoise_uniform() {
        // Uniform image should be unchanged by bilateral filter
        let rgba = vec![128, 128, 128, 255].repeat(16);
        let result = apply_denoise(rgba.clone(), 4, 4, 5.0, 0.1, 1);
        for (a, b) in rgba.iter().zip(result.iter()) {
            assert!((*a as i32 - *b as i32).abs() <= 1, "expected ~{a}, got {b}");
        }
    }

    #[test]
    fn test_gamma_passthrough() {
        let rgba = vec![100, 150, 200, 255];
        let result = apply_gamma(rgba.clone(), 1.0);
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_hsv_adjust_passthrough() {
        let rgba = vec![100, 150, 200, 255];
        let result = apply_hsv_adjust(rgba.clone(), 0.0, 1.0, 1.0);
        for (a, b) in rgba.iter().zip(result.iter()) {
            assert!((*a as i32 - *b as i32).abs() <= 1, "expected ~{a}, got {b}");
        }
    }

    #[test]
    fn test_noise_generator_passthrough() {
        let rgba = vec![128, 128, 128, 255];
        let result = apply_noise_generator(rgba.clone(), 0.0, 0, 42);
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_median_filter_uniform() {
        // Uniform image should be unchanged
        let rgba = vec![128, 128, 128, 255].repeat(16);
        let result = apply_median_filter(rgba.clone(), 4, 4, 1);
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_radial_blur_passthrough() {
        let rgba = vec![100, 150, 200, 255].repeat(16);
        let result = apply_radial_blur(rgba.clone(), 4, 4, 0.5, 0.5, 0.0, 12, 0);
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_levels_passthrough() {
        let rgba = vec![100, 150, 200, 255];
        let result = apply_levels(rgba.clone(), 0.0, 1.0, 1.0, 0.0, 1.0);
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_equalize_uniform() {
        // All same pixels → equalize should not change anything meaningfully
        let rgba = vec![128, 128, 128, 255].repeat(16);
        let result = apply_equalize(rgba.clone(), 1.0);
        for (a, b) in rgba.iter().zip(result.iter()) {
            assert!((*a as i32 - *b as i32).abs() <= 1, "expected ~{a}, got {b}");
        }
    }

    #[test]
    fn test_gamma_brighten() {
        let rgba = vec![128, 128, 128, 255];
        let result = apply_gamma(rgba, 2.0);
        // gamma=2.0 → pow(0.502, 0.5) ≈ 0.708 → ~181
        assert!(result[0] > 170 && result[0] < 190, "r={}", result[0]);
        assert_eq!(result[3], 255);
    }

    #[test]
    fn test_letterbox_passthrough() {
        // 16:9 frame with 16:9 target → no bars
        let rgba = vec![128, 64, 32, 255].repeat(16);
        let result = apply_letterbox(rgba.clone(), 4, 4, 1.0, 0, 1.0);
        // aspect ratio 1.0 on a 4x4 frame → no bars
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_letterbox_adds_bars() {
        // 4x4 frame (aspect 1.0) with target 2.0 → should add bars top/bottom
        let rgba = vec![128, 128, 128, 255].repeat(16);
        let result = apply_letterbox(rgba, 4, 4, 2.0, 0, 1.0);
        // Top row (y=0) should be black with letterbox
        assert_eq!(result[0], 0); // first pixel R
        assert_eq!(result[1], 0); // first pixel G
        assert_eq!(result[2], 0); // first pixel B
    }

    #[test]
    fn test_chromatic_aberration_passthrough() {
        let rgba = vec![128, 64, 32, 255].repeat(16);
        let result = apply_chromatic_aberration(rgba.clone(), 4, 4, 0.0, 0.0, 0.5, 0.5);
        assert_eq!(result, rgba);
    }

    #[test]
    fn test_chromatic_aberration_shifts() {
        // With non-zero shift, at least some pixels should differ from original
        let rgba = vec![200, 100, 50, 255, 50, 100, 200, 255,
                        100, 200, 50, 255, 50, 50, 50, 255];
        let result = apply_chromatic_aberration(rgba.clone(), 2, 2, 5.0, -5.0, 0.5, 0.5);
        // At least one pixel should differ
        let differs = rgba.chunks_exact(4).zip(result.chunks_exact(4)).any(|(a, b)| a != b);
        assert!(differs, "expected some pixels to be shifted");
    }

    // ── Opacity tests ──────────────────────────────────────────────────────

    #[test]
    fn opacity_half_on_white() {
        // White pixel (255,255,255,255) at 0.5 opacity → all channels halved (premultiplied alpha)
        let rgba = vec![255, 255, 255, 255];
        let result = apply_effects(&[BuiltinEffect::Opacity { value: 0.5 }], rgba, 1, 1);
        assert_eq!(result[0], 127); // 255 * 0.5
        assert_eq!(result[1], 127);
        assert_eq!(result[2], 127);
        assert_eq!(result[3], 127); // alpha also scaled (premultiplied)
    }

    #[test]
    fn opacity_zero_fully_transparent() {
        let rgba = vec![200, 150, 100, 255];
        let result = apply_effects(&[BuiltinEffect::Opacity { value: 0.0 }], rgba, 1, 1);
        assert_eq!(result, vec![0, 0, 0, 0]);
    }

    #[test]
    fn opacity_one_unchanged() {
        let rgba = vec![200, 150, 100, 255];
        let result = apply_effects(&[BuiltinEffect::Opacity { value: 1.0 }], rgba.clone(), 1, 1);
        assert_eq!(result, rgba);
    }

    // ── GaussianBlur tests ─────────────────────────────────────────────────

    #[test]
    fn gaussian_blur_solid_color_unchanged() {
        // Blurring a uniform image should produce the same image (constant signal is its own blur)
        let w = 8u32;
        let h = 8u32;
        let rgba = vec![120, 80, 200, 255].repeat((w * h) as usize);
        let result = apply_effects(
            &[BuiltinEffect::GaussianBlur { radius: 3.0 }],
            rgba.clone(), w, h,
        );
        assert_eq!(result.len(), rgba.len());
        for (i, chunk) in result.chunks_exact(4).enumerate() {
            assert!((chunk[0] as i32 - 120).abs() <= 1, "pixel {i} r={}", chunk[0]);
            assert!((chunk[1] as i32 - 80).abs() <= 1, "pixel {i} g={}", chunk[1]);
            assert!((chunk[2] as i32 - 200).abs() <= 1, "pixel {i} b={}", chunk[2]);
            assert_eq!(chunk[3], 255, "pixel {i} alpha changed");
        }
    }

    // ── Sharpen tests ──────────────────────────────────────────────────────

    #[test]
    fn sharpen_solid_color_unchanged() {
        // Sharpening a uniform image: no edges to enhance, result should match input
        let w = 8u32;
        let h = 8u32;
        let rgba = vec![100, 150, 200, 255].repeat((w * h) as usize);
        let result = apply_effects(
            &[BuiltinEffect::Sharpen { amount: 1.0 }],
            rgba.clone(), w, h,
        );
        assert_eq!(result.len(), rgba.len());
        for (i, chunk) in result.chunks_exact(4).enumerate() {
            assert!((chunk[0] as i32 - 100).abs() <= 1, "pixel {i} r={}", chunk[0]);
            assert!((chunk[1] as i32 - 150).abs() <= 1, "pixel {i} g={}", chunk[1]);
            assert!((chunk[2] as i32 - 200).abs() <= 1, "pixel {i} b={}", chunk[2]);
        }
    }

    // ── Crop tests ─────────────────────────────────────────────────────────

    #[test]
    fn crop_outside_region_transparent() {
        // 4x4 image, crop left 50%: left 2 columns become transparent
        let w = 4u32;
        let h = 4u32;
        let rgba = vec![180, 90, 45, 255].repeat((w * h) as usize);
        let result = apply_effects(
            &[BuiltinEffect::Crop { left: 0.5, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0 }],
            rgba, w, h,
        );
        assert_eq!(result.len(), (w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 4) as usize;
                if x < 2 {
                    // Outside crop region: transparent
                    assert_eq!(result[idx], 0, "({x},{y}) R should be 0");
                    assert_eq!(result[idx + 1], 0, "({x},{y}) G should be 0");
                    assert_eq!(result[idx + 2], 0, "({x},{y}) B should be 0");
                    assert_eq!(result[idx + 3], 0, "({x},{y}) A should be 0");
                } else {
                    // Inside crop region: preserved
                    assert_eq!(result[idx], 180, "({x},{y}) R should be 180");
                    assert_eq!(result[idx + 3], 255, "({x},{y}) A should be 255");
                }
            }
        }
    }

    #[test]
    fn crop_output_dimensions_match() {
        let w = 4u32;
        let h = 4u32;
        let rgba = vec![128; (w * h * 4) as usize];
        let result = apply_effects(
            &[BuiltinEffect::Crop { left: 0.25, right: 0.25, top: 0.25, bottom: 0.25, feather: 0.0 }],
            rgba, w, h,
        );
        // Crop does not resize; output buffer has same dimensions
        assert_eq!(result.len(), (w * h * 4) as usize);
    }

    // ── Transform identity tests ───────────────────────────────────────────

    #[test]
    fn transform_identity_unchanged() {
        // Identity transform: position 0, scale 1, rotation 0 -> output matches input
        let w = 4u32;
        let h = 4u32;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
            chunk[0] = (i * 17 % 256) as u8;
            chunk[1] = (i * 31 % 256) as u8;
            chunk[2] = (i * 53 % 256) as u8;
            chunk[3] = 255;
        }
        let result = apply_effects(
            &[BuiltinEffect::Transform {
                position_x: 0.0, position_y: 0.0,
                scale_x: 1.0, scale_y: 1.0,
                rotation: 0.0,
                anchor_x: 0.5, anchor_y: 0.5,
                rotation_x: 0.0, rotation_y: 0.0,
                perspective: 0.0,
                focal_ref: None,
            }],
            rgba.clone(), w, h,
        );
        assert_eq!(result, rgba);
    }

    #[test]
    fn transform_90_rotation_moves_pixels() {
        // Verify rotation actually moves pixels around. Use a small 90-degree rotation
        // to confirm the transform is not identity. We check that some interior pixels differ.
        let w = 8u32;
        let h = 8u32;
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for (i, chunk) in rgba.chunks_exact_mut(4).enumerate() {
            let x = i % w as usize;
            let y = i / w as usize;
            chunk[0] = (x * 30) as u8;
            chunk[1] = (y * 30) as u8;
            chunk[2] = 0;
            chunk[3] = 255;
        }
        let result = apply_effects(
            &[BuiltinEffect::Transform {
                position_x: 0.0, position_y: 0.0,
                scale_x: 1.0, scale_y: 1.0,
                rotation: 90.0,
                anchor_x: 0.5, anchor_y: 0.5,
                rotation_x: 0.0, rotation_y: 0.0,
                perspective: 0.0,
                focal_ref: None,
            }],
            rgba.clone(), w, h,
        );
        assert_eq!(result.len(), rgba.len());
        // The rotated image should differ from the original
        let differ_count = rgba.chunks_exact(4).zip(result.chunks_exact(4))
            .filter(|(a, b)| a != b)
            .count();
        assert!(differ_count > (w * h / 2) as usize,
            "90 rotation should change most pixels, only {differ_count} differed");
    }

    // ── ChromaKey tests ────────────────────────────────────────────────────

    #[test]
    fn chroma_key_green_pixel_keyed_out() {
        // Pure green pixel with green key -> fully transparent
        let rgba = vec![0, 255, 0, 255];
        let result = apply_effects(
            &[BuiltinEffect::ChromaKey {
                key_color: [0, 255, 0],
                tolerance: 0.3,
                spill_suppression: 0.2,
            }],
            rgba, 1, 1,
        );
        assert_eq!(result[3], 0, "green pixel should be fully keyed out");
        assert_eq!(result[0], 0);
        assert_eq!(result[1], 0);
        assert_eq!(result[2], 0);
    }

    #[test]
    fn chroma_key_non_key_preserved() {
        // Red pixel with green key -> should stay opaque
        let rgba = vec![255, 0, 0, 255];
        let result = apply_effects(
            &[BuiltinEffect::ChromaKey {
                key_color: [0, 255, 0],
                tolerance: 0.3,
                spill_suppression: 0.2,
            }],
            rgba.clone(), 1, 1,
        );
        assert_eq!(result, rgba);
    }

    // ── Invert tests ───────────────────────────────────────────────────────

    #[test]
    fn invert_white_to_black() {
        let rgba = vec![255, 255, 255, 255];
        let result = apply_effects(&[BuiltinEffect::Invert { mix: 1.0 }], rgba, 1, 1);
        assert_eq!(result[0], 0);
        assert_eq!(result[1], 0);
        assert_eq!(result[2], 0);
        assert_eq!(result[3], 255); // alpha unchanged
    }

    #[test]
    fn invert_black_to_white() {
        let rgba = vec![0, 0, 0, 255];
        let result = apply_effects(&[BuiltinEffect::Invert { mix: 1.0 }], rgba, 1, 1);
        assert!((result[0] as i32 - 255).abs() <= 1);
        assert!((result[1] as i32 - 255).abs() <= 1);
        assert!((result[2] as i32 - 255).abs() <= 1);
        assert_eq!(result[3], 255);
    }

    #[test]
    fn invert_gray_is_inverse() {
        // Gray (128,128,128) inverted should be ~(127,127,127)
        let rgba = vec![128, 128, 128, 255];
        let result = apply_effects(&[BuiltinEffect::Invert { mix: 1.0 }], rgba, 1, 1);
        assert!((result[0] as i32 - 127).abs() <= 1, "r={}", result[0]);
        assert!((result[1] as i32 - 127).abs() <= 1, "g={}", result[1]);
        assert!((result[2] as i32 - 127).abs() <= 1, "b={}", result[2]);
    }

    // ── Vibrance tests ─────────────────────────────────────────────────────

    #[test]
    fn vibrance_zero_unchanged() {
        let rgba = vec![100, 150, 200, 255];
        let result = apply_effects(&[BuiltinEffect::Vibrance { amount: 0.0 }], rgba.clone(), 1, 1);
        assert_eq!(result, rgba);
    }

    #[test]
    fn vibrance_positive_increases_saturation_of_muted() {
        // A muted color (low saturation) should become more saturated with positive vibrance
        // Use a desaturated blueish-gray: (100, 110, 130) — low saturation
        let rgba = vec![100, 110, 130, 255];
        let result = apply_effects(
            &[BuiltinEffect::Vibrance { amount: 1.0 }],
            rgba.clone(), 1, 1,
        );
        // The range (max - min) should increase: more spread between channels
        let orig_range = 130i32 - 100;
        let result_range = result[0..3].iter().map(|&v| v as i32).max().unwrap()
            - result[0..3].iter().map(|&v| v as i32).min().unwrap();
        assert!(result_range > orig_range,
            "vibrance should increase channel spread: orig={orig_range}, result={result_range}");
        assert_eq!(result[3], 255, "alpha unchanged");
    }

    // ── HSVAdjust tests ────────────────────────────────────────────────────

    #[test]
    fn hsv_adjust_identity() {
        // Hue shift 0, saturation 1, value 1 -> unchanged
        let rgba = vec![100, 150, 200, 255];
        let result = apply_effects(
            &[BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 1.0, value: 1.0 }],
            rgba.clone(), 1, 1,
        );
        for i in 0..4 {
            assert!((result[i] as i32 - rgba[i] as i32).abs() <= 1,
                "channel {i}: expected ~{}, got {}", rgba[i], result[i]);
        }
    }

    #[test]
    fn hsv_adjust_desaturate() {
        // Saturation 0 should produce grayscale
        let rgba = vec![200, 50, 100, 255];
        let result = apply_effects(
            &[BuiltinEffect::HSVAdjust { hue_shift: 0.0, saturation: 0.0, value: 1.0 }],
            rgba, 1, 1,
        );
        // All RGB channels should be equal (grayscale)
        assert!((result[0] as i32 - result[1] as i32).abs() <= 1, "not grayscale: r={} g={}", result[0], result[1]);
        assert!((result[1] as i32 - result[2] as i32).abs() <= 1, "not grayscale: g={} b={}", result[1], result[2]);
    }

    // ── Levels tests ───────────────────────────────────────────────────────

    #[test]
    fn levels_default_params_unchanged() {
        // Default levels: in_black=0, in_white=1, gamma=1, out_black=0, out_white=1 -> passthrough
        let rgba = vec![64, 128, 192, 255];
        let result = apply_effects(
            &[BuiltinEffect::Levels {
                in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0,
            }],
            rgba.clone(), 1, 1,
        );
        assert_eq!(result, rgba);
    }

    #[test]
    fn levels_crush_blacks() {
        // Set in_black to 0.5: everything below 128 maps to 0
        let rgba = vec![64, 128, 192, 255];
        let result = apply_effects(
            &[BuiltinEffect::Levels {
                in_black: 0.5, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0,
            }],
            rgba, 1, 1,
        );
        assert_eq!(result[0], 0, "64/255 < 0.5 should map to 0");
        assert!(result[2] > 128, "192/255 > 0.5 should map high");
        assert_eq!(result[3], 255, "alpha unchanged");
    }

    // ── apply_effects chain and dimensions tests ───────────────────────────

    #[test]
    fn effect_chain_preserves_dimensions() {
        // Apply multiple effects; output should always have same dimensions as input
        let w = 8u32;
        let h = 8u32;
        let rgba = vec![128, 64, 32, 255].repeat((w * h) as usize);
        let effects = vec![
            BuiltinEffect::Opacity { value: 0.8 },
            BuiltinEffect::GaussianBlur { radius: 2.0 },
            BuiltinEffect::Sharpen { amount: 0.5 },
            BuiltinEffect::Invert { mix: 0.5 },
        ];
        let result = apply_effects(&effects, rgba, w, h);
        assert_eq!(result.len(), (w * h * 4) as usize,
            "chained effects should preserve buffer size");
    }

    #[test]
    fn single_pixel_effects_do_not_panic() {
        // Edge case: 1x1 image through every effect that matters
        let rgba = vec![128, 128, 128, 255];
        let effects: Vec<BuiltinEffect> = vec![
            BuiltinEffect::Opacity { value: 0.5 },
            BuiltinEffect::GaussianBlur { radius: 2.0 },
            BuiltinEffect::Sharpen { amount: 1.0 },
            BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0 },
            BuiltinEffect::Invert { mix: 1.0 },
            BuiltinEffect::Vibrance { amount: 0.5 },
            BuiltinEffect::HSVAdjust { hue_shift: 45.0, saturation: 1.5, value: 0.8 },
            BuiltinEffect::Levels { in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0 },
        ];
        for effect in &effects {
            let r = apply_effects(&[effect.clone()], rgba.clone(), 1, 1);
            assert_eq!(r.len(), 4, "1x1 output should be 4 bytes for {:?}", effect);
        }
    }
}
