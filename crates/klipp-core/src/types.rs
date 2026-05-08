use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl Resolution {
    pub fn hd_1080() -> Self {
        Self { width: 1920, height: 1080 }
    }

    pub fn uhd_4k() -> Self {
        Self { width: 3840, height: 2160 }
    }

    pub fn aspect_ratio(&self) -> f32 {
        self.width as f32 / self.height as f32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    Rgba8Unorm,
    Bgra8Unorm,
    Rgba16Float,
    Nv12,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorSpace {
    Srgb,
    #[default]
    Rec709,
    Rec2020,
    P3D65,
    AcesCG,
    Aces2065,
    Unknown,
}

/// Camera log encoding format — determines the input device transform (IDT).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LogFormat {
    /// Standard SDR (no conversion needed)
    #[default]
    Srgb,
    /// Linear light (no transfer curve)
    Linear,
    /// ARRI LogC3 (Alexa Classic)
    LogC3,
    /// ARRI LogC4 (Alexa 35)
    LogC4,
    /// Sony S-Log2
    SLog2,
    /// Sony S-Log3
    SLog3,
    /// Canon Log
    CanonLog,
    /// Canon Log 3
    CanonLog3,
    /// Panasonic V-Log
    VLog,
    /// Nikon N-Log
    NLog,
    /// Fuji F-Log
    FLog,
    /// DJI D-Log
    DJIDLog,
    /// GoPro ProTune Flat
    GoProFlat,
    /// Apple Log (iPhone 15 Pro and later)
    AppleLog,
    /// Leica L-Log
    LeicaLLog,
    /// RED Log3G10
    RedLog3G10,
    /// Blackmagic Design Film Generation 5
    BmdFilmGen5,
    /// Hybrid Log-Gamma (BBC/NHK, ITU-R BT.2100)
    HLG,
    /// Perceptual Quantizer (SMPTE ST 2084)
    PQ,
}

// `BlendMode` lives in klipp-effects (alongside the composite math
// that consumes it). klipp-core can't own the type because klipp-core
// already depends on klipp-effects via `BuiltinEffect` payloads — a
// reverse dep would close a cycle. Re-exporting here keeps the
// existing `klipp_core::BlendMode` import paths working unchanged.
pub use klipp_effects::BlendMode;

// `FocalRef` follows the same pattern: lives in klipp-effects so the
// `BuiltinEffect::Transform.focal_ref` payload owns its definition;
// re-exported here so AppCommand variants (`ApplyHyperzoom`) and
// other klipp-core consumers can reference the type directly.
pub use klipp_effects::FocalRef;

/// Per-clip audio channel routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ChannelConfig {
    #[default]
    Default,       // Use source as-is (stereo stays stereo, mono gets centered)
    MonoDownmix,   // Sum L+R, halve amplitude, output to both channels
    SwapLR,        // Swap left and right channels
    Surround51,    // 5.1 output: L, R, C, LFE, Ls, Rs
    Surround71,    // 7.1 output: L, R, C, LFE, Ls, Rs, Lb, Rb
}

/// Audio fade curve shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FadeCurve {
    #[default]
    Linear,
    Exponential,
    Logarithmic,
    SCurve,
}

/// Content role of a clip — determines timeline color coding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ClipRole {
    #[default]
    Dialogue,
    Music,
    SFX,
    Ambience,
    Foley,
    Voiceover,
    BRoll,
    Graphics,
}

impl ClipRole {
    /// All variants for iteration in UI dropdowns.
    pub fn all() -> &'static [ClipRole] {
        &[
            ClipRole::Dialogue,
            ClipRole::Music,
            ClipRole::SFX,
            ClipRole::Ambience,
            ClipRole::Foley,
            ClipRole::Voiceover,
            ClipRole::BRoll,
            ClipRole::Graphics,
        ]
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            ClipRole::Dialogue => "Dialogue",
            ClipRole::Music => "Music",
            ClipRole::SFX => "SFX",
            ClipRole::Ambience => "Ambience",
            ClipRole::Foley => "Foley",
            ClipRole::Voiceover => "Voiceover",
            ClipRole::BRoll => "B-Roll",
            ClipRole::Graphics => "Graphics",
        }
    }
}

/// A tracked point with per-frame position data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackPoint {
    pub id: uuid::Uuid,
    pub name: String,
    pub color: [u8; 4],
    /// Per-frame positions (frame_index -> (x, y) in normalized 0.0-1.0 coordinates).
    pub positions: Vec<(f64, f64)>,
    /// Frame index where tracking starts (relative to clip source_in).
    pub start_frame: usize,
    pub enabled: bool,
    /// Search region half-size in pixels for tracking analysis.
    pub search_radius: u32,
    /// Optional back-reference to a mask this tracker is bound to.
    /// Set by `TrackMaskRegion` (the one-shot Color-tab flow) and by
    /// `GenerateKeyframesFromTrack { MaskCenter }` (the legacy Video-tab
    /// flow). Lets `RemoveMaskShape` clean up dependent trackers, and
    /// lets the Color-tab UI show an "Untrack" button on masks that
    /// already have a tracker driving them. `None` for free-standing
    /// trackers.
    #[serde(default)]
    pub linked_mask: Option<(uuid::Uuid, usize)>,
}

impl Default for TrackPoint {
    fn default() -> Self {
        Self {
            id: uuid::Uuid::new_v4(),
            name: String::new(),
            color: [0xe8, 0xa0, 0x20, 0xff], // amber
            positions: Vec::new(),
            start_frame: 0,
            enabled: true,
            search_radius: 32,
            linked_mask: None,
        }
    }
}

/// Serializable audio effect instance (stored on Clip).
/// Each instance records the effect type, enabled state, wet/dry mix, and param values.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioEffectInstance {
    pub effect_type: String,
    #[serde(default = "default_audio_fx_enabled")]
    pub enabled: bool,
    #[serde(default = "default_audio_fx_wet_dry")]
    pub wet_dry: f32,
    #[serde(default)]
    pub params: Vec<(String, f32)>,
}

fn default_audio_fx_enabled() -> bool { true }
fn default_audio_fx_wet_dry() -> f32 { 1.0 }

/// Where a transition sits relative to the cut between its two clips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TransitionAlignment {
    /// Centred on the cut: `[cut - D/2, cut + D/2)`. Standard NLE default.
    #[default]
    Center,
    /// Entire transition sits on clip_b: `[cut, cut + D)`.
    StartAtCut,
    /// Entire transition sits on clip_a: `[cut - D, cut)`.
    EndAtCut,
}

/// Generic per-transition user-tunable parameters.
///
/// The four scalar slots and RGBA colour are interpreted per `TransitionKind`
/// by the shader and CPU fallback. This struct is intentionally untyped so
/// the shader uniform layout stays a fixed size — the meaning of each slot
/// is a per-transition contract.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TransitionUserParams {
    pub param0: f32,
    pub param1: f32,
    pub param2: f32,
    pub param3: f32,
    /// RGBA in 0.0–1.0 range.
    pub color: [f32; 4],
}

impl Default for TransitionUserParams {
    fn default() -> Self {
        Self {
            param0: 0.0,
            param1: 0.0,
            param2: 0.0,
            param3: 0.0,
            color: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

impl TransitionUserParams {
    /// Kind-specific default values from the transition registry.
    pub fn default_for_kind(kind_id: u8) -> Self {
        crate::spec_for_kind_id(kind_id)
            .map(|s| s.default_params)
            .unwrap_or_default()
    }
}

/// 4-point gradient: one colour per frame corner, bilinearly
/// interpolated across the body. Used when `Gradient.four_point` is
/// `Some`. Field names match the visual corners (no Y-flip surprise).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct FourPointGradient {
    pub top_left: [u8; 4],
    pub top_right: [u8; 4],
    pub bottom_left: [u8; 4],
    pub bottom_right: [u8; 4],
}

/// Data for a generator clip (solid color, gradient, bars, countdown, still).
/// Lives in klipp-core so AppCommand can carry it directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GeneratorData {
    /// Solid color fill.
    SolidColor { color: [u8; 4] },
    /// Two-point linear gradient (`top` → `bottom` along
    /// `angle_deg`) or, when `four_point` is `Some`, a 4-corner
    /// bilinear gradient that ignores `top` / `bottom` / `angle_deg`.
    /// `angle_deg` is the gradient direction in degrees: 0 = top→bottom
    /// (the historical default), 90 = left→right, 180 = bottom→top,
    /// 270 = right→left. The two `#[serde(default)]` fields keep older
    /// projects loadable — they deserialize to angle 0 / `None` and
    /// render exactly as before.
    Gradient {
        top: [u8; 4],
        bottom: [u8; 4],
        #[serde(default)]
        angle_deg: f32,
        #[serde(default)]
        four_point: Option<FourPointGradient>,
    },
    /// SMPTE color bars (broadcast test pattern).
    ColorBars,
    /// Countdown timer (seconds remaining overlaid on black).
    Countdown,
    /// Freeze frame: a still RGBA image persisted to a sidecar PNG on disk.
    /// The renderer re-decodes the PNG on demand. `png_path` is absolute at
    /// runtime; project save/load should translate to/from a relative path
    /// under `<project>/media/stills/` the same way MediaPool handles media
    /// paths. TODO: relative-path serialization.
    StillFrame {
        png_path: std::path::PathBuf,
        width: u32,
        height: u32,
    },
    /// Regular polygon centred in the frame. `sides ≥ 3` (clamped on
    /// render). Inscribed in a circle with radius = 40 % of the
    /// frame's shorter side. `outline_width_px > 0` adds a stroke;
    /// `fill = false` renders outline only (transparent interior).
    Polygon {
        sides: u32,
        color: [u8; 4],
        #[serde(default)]
        outline_width_px: f32,
        #[serde(default = "default_fill")]
        fill: bool,
    },
    /// N-pointed star centred in the frame. Even-vertex polygon
    /// alternating between an outer radius (40 % of the frame's
    /// shorter side) and an inner radius
    /// (`outer * inner_radius_pct`). `points ≥ 3`,
    /// `inner_radius_pct ∈ (0, 1]`. Outline + fill semantics match
    /// `Polygon`.
    Star {
        points: u32,
        inner_radius_pct: f32,
        color: [u8; 4],
        #[serde(default)]
        outline_width_px: f32,
        #[serde(default = "default_fill")]
        fill: bool,
    },
}

fn default_fill() -> bool { true }

impl Default for GeneratorData {
    fn default() -> Self {
        GeneratorData::SolidColor { color: [0, 0, 0, 255] }
    }
}
