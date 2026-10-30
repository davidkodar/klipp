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
//! RGBA on GPU). The scopes are drawn on the same device by
//! [`gpu_scopes::ScopeRenderer`].
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
//! (`klipp_engine::SequentialFrameDecoder`) to compute per-frame
//! motion vectors, then applies the inverse as a warp during playback
//! and export. Analyse went from 10–15 min to ~18 s on 4K through the
//! decoder optimisation.

pub mod residency;
pub mod gpu_memory;
pub mod animate;
pub mod blur;
mod cpu;
pub mod layer;
pub mod cpu_grade;
pub mod deflicker;
pub mod dither;
pub mod displace;
pub mod warp;
pub mod gradient_map;
pub mod planar;
pub mod old_film;
pub mod presets;
pub mod reflection;
pub mod surface_light;
pub mod generator_grain;
pub mod flicker;
pub mod node_analysis;
mod cpu_transition;
mod wgsl;
pub use cpu_transition::render_transition_linear;
pub use cpu::{apply_3d_lut, apply_3d_lut_bracketed, equalize_cdf, equalize_cdf_of, equalize_histogram, EQUALIZE_CDF_LEN};
pub mod color_fade;
pub mod color_wgsl;
pub mod colour_transform;
pub mod composite;
pub mod corner_pin;
pub mod flow;
pub mod frei0r_effect;
pub mod generator;
pub mod gpu;

/// The alpha below which a premultiplied pixel has no colour to give
/// back: unpremultiplied, it is left black rather than divided by next to
/// nothing. One rule for every mean and mix that unpremultiplies, on the
/// CPU and in the shaders (`unpremultiply_min_alpha_wgsl!` in `gpu.rs`):
/// a transition's mix, and the shutter's mean of a moving picture
/// (#2017, #1923).
pub const UNPREMULTIPLY_MIN_ALPHA: f32 = 1e-6;
pub mod gpu_context;
pub mod gpu_scopes;
pub mod homography;
pub mod light;
pub mod mask_mix;
pub mod nv12_gpu;
pub mod p010_gpu;
pub mod shader_check;
pub mod particles;
pub mod protect;
pub mod pieces;
pub mod registry;
pub mod rolling_shutter;
pub mod scene_detect;
pub mod stabilization;
pub mod temporal_denoise;
pub mod track_matte;
pub mod transform;
pub mod surface_snap;
pub mod window_track;

use serde::{Deserialize, Serialize};

/// Per-clip composite blend mode. Lives here (not in klipp-core)
/// because the algebra in `composite::blend_*` is the primary
/// consumer; klipp-core re-exports the type as `BlendMode`
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

    /// One channel of the mode: the backdrop `b` and the source `s`,
    /// 0..1 each, mixed as the clip compositor mixes them, so a title
    /// layer that blends (#1513) blends by the same formulas. Add is
    /// not clamped; the caller clamps where its values must stay in
    /// range.
    pub fn blend_channel(self, b: f32, s: f32) -> f32 {
        use composite::{burn_ch, dodge_ch, overlay_ch, soft_light_ch};
        match self {
            BlendMode::Normal => s,
            BlendMode::Add => b + s,
            BlendMode::Multiply => b * s,
            BlendMode::Screen => 1.0 - (1.0 - b) * (1.0 - s),
            BlendMode::Overlay => overlay_ch(b, s),
            BlendMode::SoftLight => soft_light_ch(b, s),
            BlendMode::HardLight => overlay_ch(s, b),
            BlendMode::Difference => (b - s).abs(),
            BlendMode::Exclusion => b + s - 2.0 * b * s,
            BlendMode::ColorDodge => dodge_ch(b, s),
            BlendMode::ColorBurn => burn_ch(b, s),
            BlendMode::Darken => b.min(s),
            BlendMode::Lighten => b.max(s),
        }
    }
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

/// Direction of a `BuiltinEffect::ColorFade` region.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ColorFadeMode {
    /// Fade FROM colour at the start of the clip — first
    /// `duration_secs` of the clip's playable range.
    #[default]
    In,
    /// Fade TO colour at the end of the clip — last
    /// `duration_secs` of the clip's playable range.
    Out,
}


/// Spill-suppression algorithm used by `BuiltinEffect::ChromaKey` to
/// reduce the key colour cast on the foreground RGB after the alpha
/// matte is computed. Runs only on visible (non-keyed-out) pixels.
///
/// All non-`None` methods identify the "key channel" as the dominant
/// channel of the user's `key_color` (`argmax`) — for a green key
/// that's R=0, G=255, B=0 → key channel = G. Each method then drops
/// the key channel toward a different target by `spill_strength`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpillMethod {
    /// No spill suppression — foreground RGB is left untouched.
    /// Default for back-compat with pre-spill saves.
    #[default]
    None,
    /// Channel clamp (Primatte-style, conservative): the key channel
    /// is clamped toward `max(other_a, other_b)`. Preserves
    /// natural-looking foreground; recommended default for new keys.
    ChannelClamp,
    /// Hawk method (aggressive): the key channel is clamped toward
    /// `(other_a + other_b) / 2`. Kills more spill but can dull
    /// foreground subjects whose true colour shares the key hue.
    Hawk,
}

fn half() -> f32 {
    0.5
}

fn is_half(v: &f32) -> bool {
    *v == 0.5
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
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
        /// A shape to cover (#2757): width over height, 0 for none. Set,
        /// the picture is cropped to that shape around the focus and
        /// scaled to fill the largest rectangle of that shape the frame
        /// holds, whatever the source's own shape; a Transform after it
        /// then places that rectangle. Worked out from the source's size
        /// as each frame is drawn ([`cover_shape`]).
        // Left out at their defaults, so a crop without a shape saves as
        // it always did.
        #[serde(default, skip_serializing_if = "is_zero")]
        aspect: f32,
        /// Where the crop to `aspect` is centred, as a fraction of the
        /// picture (0.5 the middle): what a subject's framing can move.
        #[serde(default = "half", skip_serializing_if = "is_half")]
        focus_x: f32,
        #[serde(default = "half", skip_serializing_if = "is_half")]
        focus_y: f32,
    },
    Transform {
        /// Horizontal offset in pixels from center
        position_x: f32,
        /// Vertical offset in pixels from center
        position_y: f32,
        /// Horizontal scale (1.0 = 100%)
        scale_x: f32,
        /// Vertical scale (1.0 = 100%): the clip's aspect times the width.
        /// A width set to exactly 0 resets the aspect to 1 (a zero width
        /// draws nothing); scale keys through 0 keep it.
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
    /// Per-clip colour fade region — mixes the source pixels toward
    /// `color` over a `duration_secs` window at the start (mode = In)
    /// or end (mode = Out) of the clip. The engine patches `progress`
    /// every frame from clip-time + duration + mode, the same way it
    /// patches `Opacity { value }`. Persisted fields are `color`,
    /// `duration_secs`, `mode`; `progress` is engine-transient and
    /// skipped during serialization so project files don't carry a
    /// per-frame value.
    ColorFade {
        /// RGBA target colour. Alpha = 255 means the fade region
        /// fully covers below-track content (typical fade-from-black);
        /// alpha < 255 lets the source bleed through at progress = 0.
        color: [u8; 4],
        /// Length of the fade region in seconds.
        duration_secs: f32,
        /// `In` = fade FROM colour at the start of the clip;
        /// `Out` = fade TO colour at the end of the clip.
        mode: ColorFadeMode,
        /// 0.0 = full colour, 1.0 = full source. Engine-overwritten
        /// every frame from clip-time; not user-editable.
        #[serde(skip)]
        progress: f32,
    },
    GaussianBlur {
        /// Blur radius in pixels
        radius: f32,
    },
    /// A credits roll's focus band going soft (#592): the layer mixed
    /// with itself blurred by `radius`, by how far each pixel is from the
    /// band around the frame's middle — sharp inside `band` (the part of
    /// the frame, 0 to 1), fully blurred at the frame's edges, down the
    /// frame when `vertical`, across it otherwise. Put on a title layer
    /// by the compositor; not one of the effects a clip is given.
    FocusBlur {
        radius: f32,
        vertical: bool,
        band: f32,
    },
    Sharpen {
        /// Sharpen amount (0.0–2.0)
        amount: f32,
    },
    ChromaKey {
        /// RGB of the key colour
        key_color: [u8; 3],
        /// Tolerance (0.0–1.0) — CbCr distance from `key_color` below
        /// which a pixel is fully keyed out.
        tolerance: f32,
        /// Soft-edge falloff width (0.0–1.0). Inside the band
        /// `[tolerance, tolerance + soft_edge]` the pixel's alpha
        /// scales linearly from 0 → 1, smoothing the matte edge.
        ///
        /// Renamed from `spill_suppression` (the old name was
        /// misleading — this knob has only ever modulated alpha,
        /// it never suppressed spill on the foreground RGB). Serde
        /// alias preserves back-compat with all existing saves.
        #[serde(alias = "spill_suppression")]
        soft_edge: f32,
        /// Algorithm used to suppress key-colour spill on the
        /// foreground RGB (the *real* spill suppression). Runs after
        /// the alpha matte. Defaults to `None` for back-compat with
        /// pre-spill saves; new effects added from the picker get
        /// `ChannelClamp` so users see spill removed out of the box.
        #[serde(default)]
        spill_method: SpillMethod,
        /// Strength of the spill-suppression algorithm (0.0–1.0).
        /// 0 is passthrough; 1 fully suppresses. Defaults to 0 for
        /// back-compat with pre-spill saves; new effects start at 0.6.
        #[serde(default)]
        spill_strength: f32,
    },
    /// Luma Key (#1638): the clip keyed by brightness, as Chroma Key keys
    /// it by colour. Pixels whose luma lies in `[low, high]` are keyed out
    /// (alpha 0); `soft` is how far past either end the key fades back in;
    /// `invert` keeps that range and keys out the rest. Luma is the
    /// working space's own weights on the encoded picture, as the grade's
    /// LumaKey node measures it (`klipp_color::luma_key`), so a threshold
    /// means what the waveform shows. Only alpha changes.
    LumaKey {
        low: f32,
        high: f32,
        soft: f32,
        invert: bool,
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
        /// The brightness (linear luma) the glow starts from, eased in
        /// above it: 0 glows the whole picture, as a title's glow does,
        /// more only its highlights (#2763).
        #[serde(default)]
        threshold: f32,
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
    /// Rounded corners on the layer itself: the corners are taken out of
    /// the picture's alpha, so a placed clip reads as a card rather than
    /// a rectangle of video. `radius` and `feather` are fractions of the
    /// visible picture's shorter side. Unlike a mask in the grade, this changes
    /// what the compositor sees.
    RoundedCorners {
        /// Corner radius as a fraction of the shorter side (0.0-0.5).
        radius: f32,
        /// Softness of the corner edge, in the same units.
        feather: f32,
    },
    /// A shadow cast by whatever is opaque in the layer, drawn under it
    /// and past its edge, so it falls on what is behind rather than on
    /// the layer. Offsets and blur are fractions of the frame width.
    DropShadow {
        /// Right, as a fraction of the frame width (negative is left).
        offset_x: f32,
        /// Down, as a fraction of the frame width (negative is up).
        offset_y: f32,
        /// Blur radius, as a fraction of the frame width.
        blur: f32,
        /// The shadow's colour, sRGB.
        color: [u8; 3],
        /// How dark it is, 0.0-1.0.
        opacity: f32,
    },
    /// The bars round a picture that does not fill the frame — an upright
    /// phone shot in a wide frame — filled with the picture itself,
    /// enlarged to cover the frame, blurred and darkened, under it
    /// (#680). Resolve's Blanking Fill; also found as blur fill and
    /// blurred background.
    FillBars {
        /// Blur, as a fraction of the frame width.
        blur: f32,
        /// How much further than covering the frame the fill is
        /// enlarged (1.0 just covers it).
        zoom: f32,
        /// How much darker the fill is than the picture, 0.0-1.0.
        darken: f32,
    },
    /// Light spilling out of the layer's edge onto what is behind it:
    /// a drop shadow with no offset and a bright colour.
    OuterGlow {
        /// How far the light reaches, as a fraction of the frame width.
        radius: f32,
        /// How strong it is, 0.0-1.0.
        intensity: f32,
        /// The light's colour, sRGB.
        color: [u8; 3],
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
    /// Displace (#2702): each pixel taken from a point moved by an
    /// animated noise field, as water or heat haze moves a picture. See
    /// [`displace`]. **The compositor sets `time`**, the clip's time in
    /// seconds, as it sets Flicker's.
    Displace {
        /// How far a pixel moves at most, canvas pixels (default 12).
        amount: f32,
        /// How large the field's features are, canvas pixels (default 80).
        size: f32,
        /// How fast it moves, features per second (default 1).
        speed: f32,
        /// 0 both ways, 1 across only, 2 up and down only.
        direction: u32,
        seed: u32,
        #[serde(default)]
        time: f32,
    },
    /// Gradient Map (#2758): the picture's brightness laid along a ramp
    /// of three colours (sRGB), shadows to `shadow`, the middle to `mid`,
    /// highlights to `highlight`, mixed over it by `mix`. See
    /// [`gradient_map`].
    GradientMap {
        shadow: [u8; 3],
        mid: [u8; 3],
        highlight: [u8; 3],
        mix: f32,
    },
    /// Warp (#2758): the picture bent by one of [`warp`]'s shapes, each
    /// pixel taken from where the shape moves it: a wave, a twirl, a
    /// bulge or pinch, ripples, or rows torn sideways. The same shapes the
    /// transitions that bend two shots use. **The compositor sets
    /// `time`**, the clip's time in seconds, as it sets Displace's.
    Warp {
        /// 0 wave, 1 twirl, 2 bulge or pinch, 3 ripples, 4 torn rows.
        mode: u32,
        /// Wave, ripples: how far at most, pixels of a 1080-line frame.
        amplitude: f32,
        /// Wave, ripples: a wave's length, a fraction of the frame.
        wavelength: f32,
        /// Waves, rings or tears a second.
        speed: f32,
        /// Wave: 0 across, 1 up and down, 2 both.
        direction: u32,
        /// Twirl: the turn at its centre, degrees.
        angle: f32,
        /// Bulge (above 0) or pinch (below), -0.9 to 0.9; torn rows: how
        /// far they tear.
        strength: f32,
        /// Twirl, bulge: its reach, a fraction of the frame's height.
        radius: f32,
        center_x: f32,
        center_y: f32,
        /// Torn rows: a band's height, a fraction of the frame's height.
        band: f32,
        #[serde(default)]
        time: f32,
    },
    /// Reflection (#2707): the layer mirrored about a waterline, as on
    /// water, a glossy floor, a wet street or glass, rippling, breaking up,
    /// smeared and fading with distance. See [`reflection`]. It draws
    /// below the layer, so it runs on the placed layer and `waterline` is
    /// a row of the frame. **The compositor sets `time`**, the clip's time
    /// in seconds, and blends the rows below the line by `blend`
    /// ([`BuiltinEffect::split_blend`]).
    Reflection {
        /// The line it is mirrored about, a fraction of the frame's
        /// height from the top (default 0.6): where the layer's base is.
        waterline: f32,
        /// How far below that line the water is, canvas pixels: 0 rests
        /// the layer on the water; more floats it above, its reflection
        /// starting as much lower and the contact band gone.
        #[serde(default)]
        gap: f32,
        /// How far the ripple moves it sideways at most, canvas pixels.
        ripple_amount: f32,
        /// How wide the ripple's bands are, canvas pixels.
        ripple_size: f32,
        /// How fast the ripple moves, bands per second.
        ripple_speed: f32,
        /// How much it breaks into strips and gaps further from the line,
        /// 0 to 1.
        breakup: f32,
        /// How much it smears down further from the line, 0 to 1.
        smear: f32,
        /// How quickly it fades with distance, 0 (not at all) to 1.
        fade: f32,
        /// How strong it is at the line, 0 to 1.
        opacity: f32,
        /// How much darker than the layer it is, 0 to 1: water reflects
        /// little light.
        #[serde(default)]
        darken: f32,
        /// How dark the thin band where the layer meets the line is, 0
        /// to 1: its shadow on the water.
        #[serde(default)]
        contact: f32,
        /// How the reflected rows mix with the picture below: 0 normal,
        /// 1 screen, 2 soft light, 3 overlay.
        blend: u32,
        /// The surface's colour it is tinted toward, sRGB.
        tint: [u8; 3],
        /// How far toward the tint, 0 to 1.
        tint_amount: f32,
        /// Whether the tint is the water's own colour, measured under the
        /// reflection (#2795): a measurement landing sets `tint` to it.
        /// Off, `tint` is the user's.
        #[serde(default)]
        tint_auto: bool,
        /// Whether how much darker it is follows the water's brightness
        /// (#2795): a measurement landing sets `darken` from it, within
        /// bounds ([`reflection::auto_darken`]). Off, `darken` is the
        /// user's, starting from what it last was.
        #[serde(default)]
        darken_auto: bool,
        /// The water's colour as last measured under the reflection, sRGB
        /// (#2795); what it was measured for is `water_key`.
        #[serde(default)]
        water: [u8; 3],
        /// Which footage, range and waterline `water` was measured for
        /// (#2795), a 24-bit fingerprint; 0 never measured. One that no
        /// longer matches is outdated until measured again on request.
        #[serde(default)]
        water_key: u32,
        /// How far the layer's own pixels below the line are under the
        /// water, 0 over it to 1 hidden, as letters dipping into water
        /// (#2795). Saved before as a yes or no, read as 1 or 0.
        #[serde(deserialize_with = "amount_or_yes_no")]
        submerge: f32,
        /// Where the ripple comes from: 0 its noise, the only source so
        /// far.
        ripple_from: u32,
        seed: u32,
        #[serde(default)]
        time: f32,
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
    /// Film Grain: animated emulsion grain, sized in pixels at 1080p and
    /// weighted towards the shadows and midtones. `color` 0 = mono,
    /// 1 = per channel. The compositor sets `seed` to the frame number.
    FilmGrain { amount: f32, size: f32, color: u32, shadows: f32, seed: u32 },
    /// Tilt-Shift: a sharp band across the frame at `center` (0..1 of
    /// the height), `width` tall, blurred by `radius` beyond it with a
    /// `softness` ramp.
    TiltShift { center: f32, width: f32, softness: f32, radius: f32 },
    /// Old Film (#2744, [`old_film`]): flicker, scratches, debris
    /// (`dust`), gate weave and a sepia wash; the colour aged, black and
    /// white, stains, the frame line, light leaks and burned edges, each
    /// its own amount, the damage scaled together by `amount`. The
    /// compositor sets `seed` to the frame number.
    OldFilm {
        flicker: f32,
        scratches: f32,
        dust: f32,
        weave: f32,
        sepia: f32,
        seed: u32,
        #[serde(default = "full")]
        amount: f32,
        #[serde(default)]
        age: f32,
        #[serde(default)]
        mono: f32,
        #[serde(default)]
        stains: f32,
        #[serde(default)]
        frame_lines: f32,
        #[serde(default)]
        leaks: f32,
        #[serde(default)]
        burn: f32,
    },
    /// VHS: tracking error, chroma bleed, tape noise and scanlines. The
    /// compositor sets `seed` to the frame number.
    Vhs { tracking: f32, bleed: f32, noise: f32, scanlines: f32, seed: u32 },
    /// Rolling Shutter Correction: removes wobble/jello from CMOS sensor footage.
    RollingShutterCorrection {
        /// Correction strength (0.0=off, 1.0=full correction)
        strength: f32,
        /// Sensor readout ratio: fraction of frame duration for full sensor readout
        readout_ratio: f32,
    },
    /// Temporal Noise Reduction: averages each pixel with itself in
    /// the neighbouring frames, guarded so moved pixels keep their own
    /// value.
    ///
    /// **Runs at the head of the clip's pipeline, before the grade**,
    /// not where it sits in the effect list — the frames it reads are
    /// source frames, so grading first would average a graded picture
    /// against ungraded ones. The compositor lifts it out and the
    /// chain never sees it. Resolve and Premiere place theirs the same
    /// way, and for the same reason.
    ///
    /// Display-referred sources only for now. A log, PQ or HLG clip is
    /// rendered to scene linear by the main path and to display sRGB
    /// by the frame provider, and averaging those two would be
    /// comparing different renderings of the same picture.
    TemporalDenoise {
        /// How much of the averaged result to mix back (0.0-1.0).
        strength: f32,
        /// How far a pixel's brightness may move between frames and
        /// still count as the same pixel, in linear light. Sized for
        /// the grain, not for the motion: too high and moving subjects
        /// ghost, too low and nothing is cleaned.
        threshold: f32,
    },
    /// Deflicker: undoes whole-frame brightness pumping — a shutter
    /// beating against mains-frequency lighting.
    ///
    /// Requires pre-analysis (`AnalyzeDeflicker`), which measures each
    /// frame's light once and keeps it. The per-frame gain is set on
    /// the pipeline before the effect runs, as stabilization's
    /// homography is; the strength here dials it in.
    Deflicker {
        /// How much of the measured correction to apply (0.0-1.0).
        strength: f32,
        /// Seconds of context that count as "the shot's own
        /// brightness" rather than flicker. Variation faster than this
        /// is removed, slower is kept.
        window_secs: f32,
        /// The gain for the frame being rendered. **The compositor
        /// sets this**, as it sets `seed` on FilmGrain and OldFilm:
        /// the correction is per frame and the rest of these are per
        /// clip, so it cannot travel with them.
        ///
        /// 1.0 means no correction, which is what a clip that has not
        /// been analysed gets, and what a saved project reloads with
        /// until it is analysed again.
        gain: f32,
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
        /// Blend mode: 0=nearest (no blend), 1=linear (cross-fade between
        /// neighbouring frames), 2=optical flow (the in-between frame
        /// drawn by following the motion, [`flow`]; #2215).
        blend_mode: u32,
    },
    /// Third-party frei0r 1.x video plugin. The plugin universe is
    /// open-ended (Kdenlive / Shotcut / OBS / MLT all share frei0r),
    /// so the variant carries the plugin's stable type_id rather than
    /// modelling each plugin as its own enum case.
    ///
    /// Runs as a node of its own on the CPU, between the GPU nodes around
    /// it, with its instances kept across frames (M4 step 5; see
    /// `klipp_frei0r::Frei0rInstanceSet` for sizes, padding and tiling).
    Frei0r {
        /// `frei0r:{path}:{name}` — see `klipp_frei0r::Frei0rPluginInfo::type_id`.
        type_id: String,
        /// One value per declared plugin parameter, in plugin order.
        /// `Frei0rValue` exhaustively covers all 5 ABI types (Bool /
        /// Double / Color / Position / String) so saved projects
        /// don't drop fields when v2 source/mixer support lands.
        params: Vec<klipp_frei0r::Frei0rValue>,
    },

    /// The picture pinned to four corners: where its top-left,
    /// top-right, bottom-right and bottom-left land, as fractions of the
    /// frame. The map is a homography, so a picture pinned onto a screen
    /// or a sign in the shot keeps its straight lines straight and takes
    /// the surface's perspective. Outside the four corners is
    /// transparent; a quadrilateral the map cannot reach (three corners
    /// in a line) draws nothing. `[0,0] [1,0] [1,1] [0,1]` is the
    /// picture as it is. Runs before the clip's transform, with the
    /// picture, so it scales and moves with the layer.
    CornerPin { tl: [f32; 2], tr: [f32; 2], br: [f32; 2], bl: [f32; 2] },

    /// The picture pinned onto a surface tracked in the clip (a sign, a
    /// screen): a corner pin whose corners follow the surface `surface`
    /// (its name or id) as the frame shows it, `inset` of its width and
    /// height in from each edge (a screen's bezel). **The compositor sets
    /// `on_source`**, the surface's corners at the clip's time in
    /// fractions of the clip's picture with the inset taken, and
    /// `source`, that picture's size; the kernels fit them into the frame
    /// as the renderer fits the clip ([`corner_pin::surface_as_pin`]).
    /// Unset, or on a surface the clip lacks, it draws nothing.
    SurfacePin {
        surface: String,
        #[serde(default)]
        inset: f32,
        #[serde(skip)]
        on_source: [[f32; 2]; 4],
        #[serde(skip)]
        source: [u32; 2],
    },

    /// A 3D LUT file over the picture, as the grade's Lut3D node applies
    /// it: looked up on the picture's sRGB encoding, between the primaries
    /// the file is tagged with, mixed by `opacity` in linear light.
    /// `prepared` is the file read for the working space, written by the
    /// renderer before each run and never saved; with none the picture
    /// passes, and the renderer declines the node with its reason
    /// ([`colour_transform`]).
    #[serde(rename = "lut_3d")]
    Lut3D {
        /// The `.cube`, `.3dl` or `.csp` file.
        path: String,
        opacity: f32,
        /// Tetrahedral interpolation rather than trilinear.
        tetrahedral: bool,
        #[serde(skip)]
        prepared: Option<colour_transform::Prepared<colour_transform::LutTable>>,
    },

    /// The picture from one gamut and curve to another
    /// ([`colour_transform::GAMUTS`], [`colour_transform::CURVES`]): read
    /// through the input curve, converted between the gamuts, written
    /// through the output curve. `prepared` is the matrix and curve
    /// tables for the working space, written by the renderer before each
    /// run and never saved.
    ColourSpaceTransform {
        input_gamut: u32,
        input_curve: u32,
        output_gamut: u32,
        output_curve: u32,
        #[serde(skip)]
        prepared: Option<colour_transform::Prepared<colour_transform::CstTables>>,
    },

    /// The picture moved, scaled and turned inside the clip's frame where
    /// it sits among the clip's effects (#1746): a node of the picture,
    /// never the clip's own move, which its placement makes. Drawn by the
    /// Transform effect's kernels, flat (no tilt, no focal anchor). The
    /// position is in pixels of the sequence's frame from where the picture
    /// is; the anchor in fractions of it.
    PictureTransform {
        position_x: f32,
        position_y: f32,
        scale_x: f32,
        scale_y: f32,
        /// Degrees.
        rotation: f32,
        anchor_x: f32,
        anchor_y: f32,
    },
    /// Flicker (#989): the picture's opacity, exposure or gamma jumping
    /// at random, `speed` jumps a second. `amount` is how far a jump
    /// goes (opacity down by up to it; exposure or gamma up and down by
    /// up to it in stops), `smoothness` eases from one to the next (0
    /// cuts), `hold` is the share of jumps that hold still, and `seed`
    /// picks the pattern. `target` 0 = opacity, 1 = exposure, 2 =
    /// gamma. **The compositor sets `time`**, the clip's time in
    /// seconds, as it sets `seed` on FilmGrain.
    Flicker {
        amount: f32,
        speed: f32,
        smoothness: f32,
        hold: f32,
        seed: u32,
        target: u32,
        #[serde(default)]
        time: f32,
    },
    /// Binocular Vignette (#989): the frame seen through two eyepieces,
    /// two overlapping ellipses `spacing` apart (a share of the width),
    /// each `width` wide and `height` tall (shares of the frame's
    /// height), soft by `softness`; `amount` darkens (negative) or
    /// lightens what is outside them, and `fringe` splits red and blue
    /// by up to that many pixels towards their edge.
    BinocularVignette {
        amount: f32,
        spacing: f32,
        width: f32,
        height: f32,
        softness: f32,
        fringe: f32,
    },
    /// Particles (#1141): dust, snow, rain, sparks or embers (`style` 0
    /// to 4) drawn over the picture. About `amount` of them in the frame,
    /// `size` and `speed` (a second) in pixels of a 1080-line frame, the
    /// wind blowing them at `direction` degrees (0 right, 90 down),
    /// `spread` how far each strays from it (0..1), each living
    /// `lifetime` seconds give or take; `color` sRGB, `opacity` 0..1,
    /// `seed` the pattern. **The compositor sets `time`**, the clip's time
    /// in seconds, as it sets a flicker's.
    Particles {
        style: u32,
        amount: f32,
        size: f32,
        speed: f32,
        direction: f32,
        spread: f32,
        lifetime: f32,
        opacity: f32,
        color: [u8; 3],
        seed: u32,
        #[serde(default)]
        time: f32,
    },

    /// A second picture laid over the clip's, by `blend_mode` and
    /// `opacity` (#1746): the one wired into its `fg` port from beside the
    /// picture's chain (a generator, another picture). The renderer draws
    /// it with the grade's layer mixer; with nothing wired the picture
    /// passes as it is.
    Merge {
        blend_mode: BlendMode,
        opacity: f32,
    },

    /// A solid colour or a gradient that replaces the picture it is given
    /// ([`generator::SHAPES`]): beside a picture's chain, the foreground a
    /// Merge lays over it (#1746). The colours are authored sRGB bytes,
    /// alpha straight. A gradient is centred at `center_*` (fractions of
    /// the frame): a linear one runs at `angle` degrees, `radius` (a
    /// fraction of the frame's height) either side of its centre; a radial
    /// one runs out from its centre to `radius`.
    Generator {
        shape: u32,
        color_a: [u8; 4],
        color_b: [u8; 4],
        angle: f32,
        center_x: f32,
        center_y: f32,
        radius: f32,
    },

    /// Another media item's picture, at the clip's time plus `offset`
    /// seconds (#1746): beside a picture's chain, the foreground a Merge
    /// lays over it. `media` is the pool item's id. The executor reads the
    /// frame through its frame context; these kernels
    /// pass the picture they are given.
    MediaSource {
        media: String,
        offset: f32,
    },

    /// A title drawn as a picture (#2758): beside a picture's chain, the
    /// foreground a Merge lays over it, as a title clip on the track above
    /// would be drawn, styles, motion and all. `title` is the title as a
    /// title file stores it (JSON), made by the title op; the renderer
    /// draws it and the executor reads the picture through its frame
    /// context. These kernels pass the picture they are given.
    TitleSource {
        title: String,
    },

    /// What is below the clip (#1746): everything the tracks under its
    /// track composite to at its time, before the timeline's grade, seen
    /// through the clip's placement so it lines up with what is under the
    /// clip on screen. Beside a picture's chain, the foreground a Merge
    /// lays over it. The executor draws it ([`draws_a_side_picture`]);
    /// these kernels pass the picture they are given.
    ClipBelow {},

    /// A grade in the picture's chain after its display step (#1746): a
    /// marker in a clip's effect list, the grade itself the executor's
    /// compound node, which the renderer builds from the picture's
    /// `klipp.grade_compound` node. These kernels pass the picture.
    GradeCompound {},
}

/// A serde default of 1.
fn full() -> f32 {
    1.0
}

/// An amount from 0 to 1 saved before as a yes or no: yes is 1, no 0.
fn amount_or_yes_no<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Held {
        Amount(f32),
        YesNo(bool),
    }
    Ok(match Held::deserialize(d)? {
        Held::Amount(v) => v,
        Held::YesNo(b) => f32::from(u8::from(b)),
    })
}

/// Whether `effect` draws a picture the render hands its run (another
/// media item's, or what is below the clip): the executor's side
/// picture node draws it, never this crate's kernels.
pub fn draws_a_side_picture(effect: &BuiltinEffect) -> bool {
    matches!(effect, BuiltinEffect::MediaSource { .. } | BuiltinEffect::TitleSource { .. } | BuiltinEffect::ClipBelow {})
}

impl BuiltinEffect {
    /// A picture transform as the Transform effect its kernels draw.
    pub fn picture_transform_as_transform(&self) -> Option<BuiltinEffect> {
        let BuiltinEffect::PictureTransform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y } = *self else { return None };
        Some(BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, anchor_x, anchor_y, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None })
    }
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
/// signature, and for every title and generator frame the renderer draws.
///
/// A u8 has 256 values, so the curve is a table — the same numbers as
/// evaluating it per channel, without a `powf` each — filled in parallel.
pub fn srgb_u8_to_linear_f32_rgba(rgba: &[u8]) -> Vec<f32> {
    use rayon::prelude::*;
    static LUT: std::sync::OnceLock<[f32; 256]> = std::sync::OnceLock::new();
    let lut = LUT.get_or_init(|| std::array::from_fn(|i| srgb_to_linear_scalar(i as f32 / 255.0)));
    let mut out = vec![0.0f32; rgba.len() / 4 * 4];
    out.par_chunks_mut(4 * 4096).zip(rgba.par_chunks(4 * 4096)).for_each(|(o, i)| {
        for (o, i) in o.chunks_exact_mut(4).zip(i.chunks_exact(4)) {
            o[0] = lut[i[0] as usize];
            o[1] = lut[i[1] as usize];
            o[2] = lut[i[2] as usize];
            o[3] = i[3] as f32 / 255.0;
        }
    });
    out
}

/// Inverse of [`srgb_u8_to_linear_f32_rgba`]. Values outside `[0, 1]`
/// clamp at the u8 boundary (not earlier).
pub fn linear_f32_rgba_to_srgb_u8(linear: &[f32]) -> Vec<u8> {
    use rayon::prelude::*;
    let mut out = vec![0u8; linear.len() / 4 * 4];
    out.par_chunks_mut(4 * 4096).zip(linear.par_chunks(4 * 4096)).for_each(|(o, i)| {
        for (o, i) in o.chunks_exact_mut(4).zip(i.chunks_exact(4)) {
            o[0] = (linear_to_srgb_scalar(i[0]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            o[1] = (linear_to_srgb_scalar(i[1]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            o[2] = (linear_to_srgb_scalar(i[2]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            o[3] = (i[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
    });
    out
}

/// BT.709's luminance weights, for a caller with no working space to
/// speak of.
///
/// A project has one and should pass it (see
/// [`apply_effects_with_luma`]); an effect *thumbnail* or a unit test
/// genuinely does not, and BT.709 is the right answer there rather than a
/// placeholder.
pub const BT709_LUMA_WEIGHTS: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// Apply a chain of built-in effects to a linear-f32 RGBA frame, on the
/// CPU: each effect computed as its GPU shader computes it (see the `cpu` module),
/// so the picture does not depend on which one ran. Frei0r plugins take
/// 8-bit frames; each is bridged on its own, the rest of the chain stays
/// in float.
///
/// Uses BT.709's luminance weights. A caller inside a project should use
/// [`apply_effects_with_luma`] instead: the effects that gate on
/// brightness mis-estimate it in any other basis.
pub fn apply_effects(effects: &[BuiltinEffect], linear_rgba: Vec<f32>, width: u32, height: u32) -> Vec<f32> {
    apply_chain(effects, linear_rgba, width, height, BT709_LUMA_WEIGHTS, &MotionData::default())
}

/// [`apply_effects`] with the working space's luminance weights, which
/// is what a render should call. No effect in the chain reads the clip's
/// time: frei0r, the one that did, runs as its own node with its
/// instances (M4 step 5).
///
/// `luma` is `klipp_color::primaries::luminance_weights(working_space)` —
/// passed in rather than derived, because this crate does not depend on
/// `klipp-color` outside its tests. The GPU pipeline takes the same
/// numbers through `set_luma_coefs`, and `cpu_gpu_effect_parity` holds the
/// two together.
pub fn apply_effects_with_luma(
    effects: &[BuiltinEffect],
    linear_rgba: Vec<f32>,
    width: u32,
    height: u32,
    luma: [f32; 3],
) -> Vec<f32> {
    apply_chain(effects, linear_rgba, width, height, luma, &MotionData::default())
}

/// The per-frame motion data stabilisation and rolling-shutter correction
/// warp by. The compositor works it out for the frame and gives it to
/// whichever arm runs the chain: uploaded to the GPU pipeline, or passed
/// here to the CPU one. Without it both effects pass the frame through,
/// on either arm, as before analysis.
#[derive(Clone, Default)]
pub struct MotionData {
    /// This frame's stabilisation correction: a row-major 3x3
    /// homography, padded to 12 as the GPU buffer holds it.
    pub stabilization: Option<[f32; 12]>,
    /// One transform per row of the frame, for rolling-shutter
    /// correction, its strength already applied.
    pub rsc_rows: Option<Vec<rolling_shutter::RowTransform>>,
}

/// [`apply_effects_with_luma`] with the frame's motion data, so the CPU
/// arm stabilises and corrects rolling shutter as the GPU does (#698).
pub fn apply_effects_with_motion(
    effects: &[BuiltinEffect],
    linear_rgba: Vec<f32>,
    width: u32,
    height: u32,
    luma: [f32; 3],
    motion: &MotionData,
) -> Vec<f32> {
    apply_chain(effects, linear_rgba, width, height, luma, motion)
}

/// How far stabilisation zooms in to hide the edges its warp uncovers.
pub(crate) fn stabilization_crop_zoom(crop_ratio: f32, zoom: bool) -> f32 {
    if zoom { 1.0 / (1.0 - 2.0 * crop_ratio) } else { 1.0 }
}

fn apply_chain(
    effects: &[BuiltinEffect],
    linear_rgba: Vec<f32>,
    width: u32,
    height: u32,
    luma: [f32; 3],
    motion: &MotionData,
) -> Vec<f32> {
    for fx in effects.iter().filter(|fx| !is_identity_effect(fx)) {
        residency::record(residency::effect_id(fx), "CPU effect kernel", residency::Work::Fallback);
    }
    let mut frame = linear_rgba;
    let mut i = 0;
    while i < effects.len() {
        // Shadows and glows next to each other are cast together, each
        // from the layer's own silhouette; see `layer::spills`.
        let run: Vec<layer::Spill> = effects[i..].iter().map_while(layer::Spill::of).collect();
        if !run.is_empty() {
            frame = layer::spills(frame, width, height, &run);
            i += run.len();
            continue;
        }
        frame = match &effects[i] {
            // A frei0r plug-in is a node (M4 step 5); the renderer never
            // sends one here. Reached anyway, it's left out, said so.
            BuiltinEffect::Frei0r { type_id, .. } => {
                tracing::error!("frei0r {type_id} reached the effect chain, which doesn't run plug-ins: left out");
                frame
            }
            BuiltinEffect::VideoStabilization { crop_ratio, zoom, .. } => match &motion.stabilization {
                Some(m) => cpu::stabilize(&frame, width, height, m, stabilization_crop_zoom(*crop_ratio, *zoom)),
                None => frame,
            },
            BuiltinEffect::RollingShutterCorrection { strength, .. } => match &motion.rsc_rows {
                Some(rows) if *strength >= f32::EPSILON && !rows.is_empty() => cpu::rolling_shutter(&frame, width, height, rows),
                _ => frame,
            },
            BuiltinEffect::Lut3D { prepared, opacity, tetrahedral, .. } => {
                let mut frame = frame;
                if let Some(lut) = prepared {
                    colour_transform::apply_lut(&mut frame, lut, *opacity, *tetrahedral);
                }
                frame
            }
            BuiltinEffect::ColourSpaceTransform { prepared, .. } => {
                let mut frame = frame;
                if let Some(t) = prepared {
                    colour_transform::apply_cst(&mut frame, t);
                }
                frame
            }
            &BuiltinEffect::Generator { shape, color_a, color_b, angle, center_x, center_y, radius } => {
                let mut frame = frame;
                generator::fill(&mut frame, &generator::Gen::new(shape, color_a, color_b, angle, center_x, center_y, radius, width, height), width);
                frame
            }
            e => cpu::apply(e, frame, width, height, luma),
        };
        i += 1;
    }
    frame
}

/// Whether the list holds a frei0r plug-in: a node of its own on the
/// CPU, with instances kept across frames (M4 step 5), which the viewer's
/// GPU display stack doesn't draw.
pub fn has_frei0r(effects: &[BuiltinEffect]) -> bool {
    effects.iter().any(|e| matches!(e, BuiltinEffect::Frei0r { .. }))
}

/// Whether the list holds an effect that only a node draws: a frei0r
/// plug-in, a side picture (#1746), which the render reads for the
/// executor, or a grade compound. These kernels would pass the picture
/// instead.
pub fn runs_only_as_a_node(effects: &[BuiltinEffect]) -> bool {
    has_frei0r(effects) || effects.iter().any(|e| draws_a_side_picture(e) || matches!(e, BuiltinEffect::GradeCompound {}))
}

impl BuiltinEffect {
    /// The effect as it should run on a buffer `k` times the sequence's
    /// size: every parameter measured in pixels scaled by `k`, the rest
    /// untouched.
    ///
    /// A blur radius or a streak length is authored against the picture
    /// the editor sees at the sequence's size. Rendered into a half-size
    /// preview — or into the 1080p frame the decoder makes of 4K media —
    /// the same number of pixels covers twice as much of the picture, so
    /// a 20 px blur looked like a 40 px one. Parameters already written
    /// as fractions of the frame (crop edges, centres, shadow distances)
    /// and those scaled by `/1080` internally (film grain, old film) need
    /// nothing. An integer radius stays at least 1 where it was, so a
    /// small erode does not vanish at a quarter size.
    pub fn at_render_scale(mut self, k: f32) -> Self {
        if (k - 1.0).abs() < 1e-6 || !k.is_finite() || k <= 0.0 {
            return self;
        }
        let px = |v: &mut f32| *v *= k;
        let whole = |v: &mut u32| {
            if *v > 0 {
                *v = ((*v as f32 * k).round() as u32).max(1);
            }
        };
        match &mut self {
            BuiltinEffect::Crop { feather, .. } => px(feather),
            BuiltinEffect::GaussianBlur { radius }
            | BuiltinEffect::Bloom { radius, .. }
            | BuiltinEffect::Glow { radius, .. }
            | BuiltinEffect::Diffusion { radius, .. }
            | BuiltinEffect::FilmHalation { radius, .. }
            | BuiltinEffect::TiltShift { radius, .. }
            | BuiltinEffect::FocusBlur { radius, .. } => px(radius),
            BuiltinEffect::LightWrap { width, .. } => px(width),
            BuiltinEffect::AnamorphicStreak { length, .. } => px(length),
            // A block never smaller than a pixel.
            BuiltinEffect::Mosaic { block_size } => *block_size = (*block_size * k).max(1.0),
            BuiltinEffect::DirectionalBlur { distance, .. } => px(distance),
            // The field's grain and its reach both in canvas pixels, so a
            // proxy moves the same part of the picture.
            BuiltinEffect::Displace { amount, size, .. } => {
                px(amount);
                px(size);
            }
            BuiltinEffect::Reflection { ripple_amount, ripple_size, gap, .. } => {
                px(gap);
                px(ripple_amount);
                px(ripple_size);
            }
            BuiltinEffect::ChromaticAberration { red_shift, blue_shift, .. } => {
                px(red_shift);
                px(blue_shift);
            }
            BuiltinEffect::BinocularVignette { fringe, .. } => px(fringe),
            BuiltinEffect::Dilate { radius } | BuiltinEffect::Erode { radius } | BuiltinEffect::MedianFilter { radius } => whole(radius),
            BuiltinEffect::Denoise { spatial_sigma, .. } => px(spatial_sigma),
            BuiltinEffect::PictureTransform { position_x, position_y, .. } => {
                px(position_x);
                px(position_y);
            }
            _ => {}
        }
        self
    }
}

/// Whether the effect draws outside the picture, in the margin a
/// transform leaves around a placed layer. Those run after the
/// transform that made the margin; see [`crate::layer`].
pub fn draws_outside_the_picture(effect: &BuiltinEffect) -> bool {
    matches!(effect, BuiltinEffect::DropShadow { .. } | BuiltinEffect::OuterGlow { .. } | BuiltinEffect::FillBars { .. } | BuiltinEffect::Reflection { .. })
}

impl BuiltinEffect {
    /// A Warp's settings as its kernels take them; `None` for another
    /// effect.
    pub fn warp_settings(&self) -> Option<warp::Settings> {
        let BuiltinEffect::Warp { mode, amplitude, wavelength, speed, direction, angle, strength, radius, center_x, center_y, band, time } = *self else { return None };
        Some(warp::Settings { mode, amplitude, wavelength, speed, direction, angle, strength, radius, center_x, center_y, band, time })
    }

    /// A reflection's (#2707) row of the frame `height` tall from which
    /// the layer is blended by another mode, and that mode: the rows
    /// below its waterline mix with the picture below as water does.
    /// `None` for every other effect, and a reflection of nothing.
    pub fn split_blend(&self, height: u32) -> Option<(u32, BlendMode)> {
        let BuiltinEffect::Reflection { waterline, gap, blend, .. } = self else { return None };
        if is_identity_effect(self) {
            return None;
        }
        let mode = match *blend {
            reflection::SCREEN => BlendMode::Screen,
            reflection::SOFT_LIGHT => BlendMode::SoftLight,
            reflection::OVERLAY => BlendMode::Overlay,
            _ => BlendMode::Normal,
        };
        let row = (waterline.clamp(0.0, 1.0) * height as f32 + gap.max(0.0)).round() as u32;
        Some((row.min(height), mode))
    }

    /// A reflection's numbers for its computation on a layer `width` ×
    /// `height` (#2707).
    /// An Old Film's numbers as both computations take them, at
    /// `width` × `height` with the working space's `luma` weights.
    pub fn old_film_params(&self, width: u32, height: u32, luma: [f32; 3]) -> Option<old_film::Params> {
        let BuiltinEffect::OldFilm { flicker, scratches, dust, weave, sepia, seed, amount, age, mono, stains, frame_lines, leaks, burn } = *self else { return None };
        Some(old_film::Params { width, height, flicker, scratches, dust, weave, sepia, seed, luma, amount, age, mono, stains, frame_lines, leaks, burn, _pad: [0; 2] })
    }

    pub fn reflection_params(&self, width: u32, height: u32) -> Option<reflection::Params> {
        let BuiltinEffect::Reflection { waterline, gap, ripple_amount, ripple_size, ripple_speed, breakup, smear, fade, opacity, darken, contact, tint, tint_amount, submerge, seed, time, .. } = self else { return None };
        Some(reflection::Params {
            width,
            height,
            waterline: waterline.clamp(0.0, 1.0) * height as f32,
            ripple_amount: *ripple_amount,
            ripple_size: *ripple_size,
            ripple_speed: *ripple_speed,
            breakup: *breakup,
            smear: *smear,
            fade: *fade,
            opacity: *opacity,
            tint: tint.map(reflection::linear),
            tint_amount: *tint_amount,
            submerge: *submerge,
            seed: *seed,
            time: *time,
            darken: *darken,
            contact: *contact,
            gap: *gap,
        })
    }
}

/// The crop and placement that make a `src`-sized picture cover the shape
/// `aspect` (width over height) in a `frame`-sized render (#2757).
///
/// The picture is first fitted to the frame as every source is
/// ([`gpu::fit_rect`]). The most of it that has the shape is kept, around
/// `focus` (fractions of the picture, the middle at 0.5), and is scaled
/// and centred onto the largest rectangle of that shape the frame holds.
/// So whatever the source's own shape, the same rectangle comes out, and
/// a Transform after it places that rectangle where a layout wants it.
/// `feather` is the crop's, as given.
pub fn cover_shape(src: (u32, u32), frame: (u32, u32), aspect: f32, focus: (f32, f32), feather: f32) -> [BuiltinEffect; 2] {
    let (fw, fh) = (frame.0.max(1) as f32, frame.1.max(1) as f32);
    let (dw, dh, xo, yo) = gpu::fit_rect(src.0.max(1), src.1.max(1), frame.0.max(1), frame.1.max(1));
    let (dw, dh, xo, yo) = (dw as f32, dh as f32, xo as f32, yo as f32);
    let a = if aspect.is_finite() && aspect > 0.0 { aspect } else { dw / dh };
    // The most of the fitted picture that has the shape.
    let (kw, kh) = if dw / dh > a { (dh * a, dh) } else { (dw, dw / a) };
    let off = |focus: f32, whole: f32, kept: f32| (focus.clamp(0.0, 1.0) * whole - kept / 2.0).clamp(0.0, (whole - kept).max(0.0));
    let (x0, y0) = (xo + off(focus.0, dw, kw), yo + off(focus.1, dh, kh));
    let crop = BuiltinEffect::Crop {
        left: x0 / fw,
        right: ((fw - x0 - kw) / fw).max(0.0),
        top: y0 / fh,
        bottom: ((fh - y0 - kh) / fh).max(0.0),
        feather,
        aspect: 0.0,
        focus_x: 0.5,
        focus_y: 0.5,
    };
    // The largest rectangle of the shape in the frame, and the kept part
    // scaled onto it, its centre brought to the frame's.
    let rw = if a > fw / fh { fw } else { fh * a };
    let scale = rw / kw;
    let (dx, dy) = (x0 + kw / 2.0 - fw / 2.0, y0 + kh / 2.0 - fh / 2.0);
    let place = BuiltinEffect::Transform {
        position_x: -dx * scale,
        position_y: -dy * scale,
        scale_x: scale,
        scale_y: scale,
        rotation: 0.0,
        anchor_x: 0.5,
        anchor_y: 0.5,
        rotation_x: 0.0,
        rotation_y: 0.0,
        perspective: 0.0,
        focal_ref: None,
    };
    [crop, place]
}

/// Each Crop in `fx` given a shape to cover made the crop and placement
/// [`cover_shape`] gives, for a `src`-sized source in a `frame`-sized
/// render: what the renderer does once it knows the source's size, on
/// the GPU and the CPU alike, so both draw it with the effects they
/// already have. A clip is placed by one Transform, so the cover's
/// placement is folded into the Transform after the crop (applied first,
/// then that Transform's own move, scale and turn), or added where there
/// is none. Returns where effects were added, in order, for lists kept
/// beside `fx`.
pub fn resolve_cover_shapes(fx: &mut Vec<BuiltinEffect>, src: (u32, u32), frame: (u32, u32)) -> Vec<usize> {
    let mut added = Vec::new();
    let mut i = 0;
    while i < fx.len() {
        if let BuiltinEffect::Crop { aspect, focus_x, focus_y, feather, .. } = fx[i] {
            if aspect > 0.0 {
                let [crop, place] = cover_shape(src, frame, aspect, (focus_x, focus_y), feather);
                fx[i] = crop;
                let BuiltinEffect::Transform { position_x: cx, position_y: cy, scale_x: cs, .. } = place else { unreachable!() };
                match fx[i + 1..].iter().position(|e| matches!(e, BuiltinEffect::Transform { .. })) {
                    Some(k) => {
                        if let BuiltinEffect::Transform { position_x, position_y, scale_x, scale_y, rotation, .. } = &mut fx[i + 1 + k] {
                            // The cover's offset, scaled and turned as that
                            // Transform scales and turns what it is given.
                            let (sin, cos) = rotation.to_radians().sin_cos();
                            let (ox, oy) = (cx * *scale_x, cy * *scale_y);
                            *position_x += ox * cos - oy * sin;
                            *position_y += ox * sin + oy * cos;
                            *scale_x *= cs;
                            *scale_y *= cs;
                        }
                    }
                    None => {
                        fx.insert(i + 1, place);
                        added.push(i + 1);
                        i += 1;
                    }
                }
            }
        }
        i += 1;
    }
    added
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
        // Outside the fade region the engine drives progress to 1.0 so
        // we can short-circuit to "no work" — the per-pixel apply does
        // the same fast path at progress >= 0.9999, but skipping the
        // upload/download bridge is cheaper.
        BuiltinEffect::ColorFade { progress, duration_secs, .. } => {
            *progress >= 0.9999 || *duration_secs <= 0.0
        }
        BuiltinEffect::GaussianBlur { radius } => *radius < 0.5,
        BuiltinEffect::FocusBlur { radius, band, .. } => *radius < 0.5 || *band >= 0.999,
        BuiltinEffect::RoundedCorners { radius, .. } => *radius <= 0.0,
        BuiltinEffect::DropShadow { offset_x, offset_y, blur, opacity, .. } => {
            *opacity <= 0.0 || (*offset_x == 0.0 && *offset_y == 0.0 && *blur <= 0.0)
        }
        BuiltinEffect::OuterGlow { radius, intensity, .. } => *intensity <= 0.0 || *radius <= 0.0,
        BuiltinEffect::Sharpen { amount } => amount.abs() < f32::EPSILON,
        // As the effect itself skips it (#1314).
        BuiltinEffect::Gamma { gamma } => (gamma - 1.0).abs() < f32::EPSILON,
        BuiltinEffect::Crop { left, right, top, bottom, feather, aspect, .. } => {
            *left == 0.0 && *right == 0.0 && *top == 0.0 && *bottom == 0.0 && *feather == 0.0 && *aspect <= 0.0
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
        BuiltinEffect::CornerPin { tl, tr, br, bl } => corner_pin::is_still(&[*tl, *tr, *br, *bl]),
        BuiltinEffect::Warp { .. } => effect.warp_settings().is_none_or(|s| warp::is_identity(&s)),
        BuiltinEffect::GradientMap { mix, .. } => mix.abs() < f32::EPSILON,
        BuiltinEffect::Lut3D { opacity, .. } => *opacity <= 0.0,
        BuiltinEffect::PictureTransform { .. } => effect.picture_transform_as_transform().is_some_and(|t| is_identity_effect(&t)),
        // A flicker of nothing, or one whose every jump holds (#989).
        BuiltinEffect::Flicker { amount, hold, .. } => amount.abs() < f32::EPSILON || *hold >= 1.0,
        BuiltinEffect::Displace { amount, .. } => amount.abs() < f32::EPSILON,
        // A reflection nobody sees, which hides nothing either.
        BuiltinEffect::Reflection { opacity, submerge, .. } => *opacity <= 0.0 && *submerge <= 0.0,
        // No particles, or none to see (#1141).
        BuiltinEffect::Particles { amount, opacity, size, .. } => *amount < 1.0 || *opacity <= 0.0 || *size <= 0.0,
        BuiltinEffect::Merge { opacity, .. } => *opacity <= 0.0,
        BuiltinEffect::ColourSpaceTransform { input_gamut, input_curve, output_gamut, output_curve, .. } => {
            input_gamut == output_gamut && *input_curve == 0 && *output_curve == 0
        }
        // Effects whose visual contribution depends on data (chroma
        // key, bloom, glow, light rays, vignette etc.) — be
        // conservative and never claim identity. False positives
        // here would silently drop user intent.
        _ => false,
    }
}

#[cfg(test)]
mod tests {

    /// A crop without a shape to cover saves as it always did: no shape
    /// or focus keys, so saved projects don't change; one with them
    /// keeps them.
    #[test]
    fn a_crop_without_a_shape_saves_as_before() {
        let plain = BuiltinEffect::Crop { left: 0.1, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5 };
        let json = serde_json::to_value(&plain).unwrap();
        assert!(json.get("aspect").is_none() && json.get("focus_x").is_none() && json.get("focus_y").is_none(), "{json}");
        let shaped = BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 1.0, focus_x: 0.3, focus_y: 0.5 };
        let json = serde_json::to_value(&shaped).unwrap();
        assert!(json.get("aspect").is_some() && json.get("focus_x").is_some() && json.get("focus_y").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<BuiltinEffect>(json).unwrap(), shaped);
    }

    /// A clip is placed by one Transform: the cover's own placement is
    /// folded into the one after the crop, not added beside it.
    #[test]
    fn a_cover_is_folded_into_the_clips_placement() {
        let user = BuiltinEffect::Transform { position_x: -96.0, position_y: 0.0, scale_x: 0.5, scale_y: 0.5, rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5, rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0, focal_ref: None };
        let crop = BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 1.0, focus_x: 1.0, focus_y: 0.5 };
        let mut fx = vec![crop, user];
        resolve_cover_shapes(&mut fx, (320, 180), (384, 216));
        assert_eq!(fx.iter().filter(|e| matches!(e, BuiltinEffect::Transform { .. })).count(), 1, "one placement");
        let [_, place] = cover_shape((320, 180), (384, 216), 1.0, (1.0, 0.5), 0.0);
        let (BuiltinEffect::Transform { position_x: cx, scale_x: cs, .. }, BuiltinEffect::Transform { position_x, scale_x, .. }) = (place, &fx[1]) else { panic!() };
        assert!((scale_x - 0.5 * cs).abs() < 1e-6 && (position_x - (-96.0 + 0.5 * cx)).abs() < 1e-4);
    }

    /// #2757: whatever the source's shape, a cover to a shape comes out as
    /// the largest rectangle of that shape in the frame, centred: the kept
    /// part has the shape, lies inside the fitted picture, and is scaled
    /// onto that rectangle. A focus off the middle keeps the crop inside
    /// the picture.
    #[test]
    fn a_cover_shape_is_the_same_rectangle_from_any_source() {
        for frame in [(1920u32, 1080u32), (1080, 1920), (1080, 1080)] {
            let (fw, fh) = (frame.0 as f32, frame.1 as f32);
            for src in [(1920u32, 1080u32), (1080, 1920), (640, 480)] {
                for aspect in [960.0 / 1080.0, 1080.0 / 960.0, 1.0, 16.0 / 9.0] {
                    for focus in [(0.5, 0.5), (0.0, 0.0), (1.0, 0.3)] {
                        let [crop, place] = cover_shape(src, frame, aspect, focus, 0.0);
                        let BuiltinEffect::Crop { left, right, top, bottom, .. } = crop else { panic!() };
                        let BuiltinEffect::Transform { position_x, position_y, scale_x, .. } = place else { panic!() };
                        let (kw, kh) = ((1.0 - left - right) * fw, (1.0 - top - bottom) * fh);
                        assert!((kw / kh - aspect).abs() < 0.01, "{src:?} in {frame:?} at {aspect}: kept {kw}x{kh}");
                        let (dw, dh, xo, yo) = gpu::fit_rect(src.0, src.1, frame.0, frame.1);
                        assert!(left * fw >= xo as f32 - 0.5 && left * fw + kw <= (xo + dw) as f32 + 0.5, "inside the picture across");
                        assert!(top * fh >= yo as f32 - 0.5 && top * fh + kh <= (yo + dh) as f32 + 0.5, "inside the picture down");
                        let rw = if aspect > fw / fh { fw } else { fh * aspect };
                        assert!((kw * scale_x - rw).abs() < 0.5, "scaled onto the frame's own rectangle");
                        // Its centre brought to the frame's.
                        let (cx, cy) = (left * fw + kw / 2.0 - fw / 2.0, top * fh + kh / 2.0 - fh / 2.0);
                        assert!((cx * scale_x + position_x).abs() < 0.5 && (cy * scale_x + position_y).abs() < 0.5);
                    }
                }
            }
        }
    }

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
            left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5
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
            left: 0.1, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5
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
    fn opacity_half_on_white() {
        // White at 0.5 opacity: every channel halved in linear light
        // (premultiplied), so RGB is linear 0.5 — sRGB 188 — and alpha 127.
        let rgba = vec![255, 255, 255, 255];
        let result = apply_effects(&[BuiltinEffect::Opacity { value: 0.5 }], rgba, 1, 1);
        assert_eq!(result[0], 188);
        assert_eq!(result[1], 188);
        assert_eq!(result[2], 188);
        assert_eq!(result[3], 128);
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
        let rgba = [120, 80, 200, 255].repeat((w * h) as usize);
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
        let rgba = [100, 150, 200, 255].repeat((w * h) as usize);
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
        let rgba = [180, 90, 45, 255].repeat((w * h) as usize);
        let result = apply_effects(
            &[BuiltinEffect::Crop { left: 0.5, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5}],
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
            &[BuiltinEffect::Crop { left: 0.25, right: 0.25, top: 0.25, bottom: 0.25, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5}],
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
                soft_edge: 0.2,
                spill_method: SpillMethod::None,
                spill_strength: 0.0,
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
                soft_edge: 0.2,
                spill_method: SpillMethod::None,
                spill_strength: 0.0,
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
        let rgba = [128, 64, 32, 255].repeat((w * h) as usize);
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
            BuiltinEffect::Crop { left: 0.0, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5},
            BuiltinEffect::Invert { mix: 1.0 },
            BuiltinEffect::Vibrance { amount: 0.5 },
            BuiltinEffect::HSVAdjust { hue_shift: 45.0, saturation: 1.5, value: 0.8 },
            BuiltinEffect::Levels { in_black: 0.0, in_white: 1.0, gamma: 1.0, out_black: 0.0, out_white: 1.0 },
        ];
        for effect in &effects {
            let r = apply_effects(std::slice::from_ref(effect), rgba.clone(), 1, 1);
            assert_eq!(r.len(), 4, "1x1 output should be 4 bytes for {:?}", effect);
        }
    }

    #[test]
    fn pixel_sized_parameters_follow_the_render_size_and_fractions_do_not() {
        let half = |e: BuiltinEffect| e.at_render_scale(0.5);
        assert_eq!(half(BuiltinEffect::GaussianBlur { radius: 20.0 }), BuiltinEffect::GaussianBlur { radius: 10.0 });
        assert_eq!(
            half(BuiltinEffect::Crop { left: 0.25, right: 0.0, top: 0.0, bottom: 0.0, feather: 8.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5}),
            BuiltinEffect::Crop { left: 0.25, right: 0.0, top: 0.0, bottom: 0.0, feather: 4.0, aspect: 0.0, focus_x: 0.5, focus_y: 0.5}
        );
        assert_eq!(half(BuiltinEffect::Erode { radius: 1 }), BuiltinEffect::Erode { radius: 1 }, "never rounded away");
        assert_eq!(half(BuiltinEffect::Dilate { radius: 6 }), BuiltinEffect::Dilate { radius: 3 });
        assert_eq!(half(BuiltinEffect::Mosaic { block_size: 1.5 }), BuiltinEffect::Mosaic { block_size: 1.0 });
        let sharpen = BuiltinEffect::Sharpen { amount: 1.2 };
        assert_eq!(half(sharpen.clone()), sharpen);
        assert_eq!(BuiltinEffect::GaussianBlur { radius: 20.0 }.at_render_scale(1.0), BuiltinEffect::GaussianBlur { radius: 20.0 });
    }
}
