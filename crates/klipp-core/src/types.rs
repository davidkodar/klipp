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
    /// ACES — ACEScc (logarithmic, for color grading)
    AcesCC,
    /// ACES — ACEScg (linear, AP1 primaries, common working space)
    AcesCG,
    /// ACES — ACES2065-1 (linear, AP0 primaries, archival interchange)
    Aces2065,
}

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
    /// Kind-specific default values. Call at creation time so a fresh transition
    /// gets parameters tuned for its kind instead of the kind-neutral `Default`.
    pub fn default_for_kind(kind_id: u8) -> Self {
        match kind_id {
            // CrossDissolve: param0 = easing curve (0=Linear)
            0 => Self { param0: 0.0, ..Self::default() },
            // Wipes: param0 = feather width in pixels (0=hard edge)
            1 | 2 | 3 | 4 => Self { param0: 0.0, ..Self::default() },
            // DipToBlack: param0 = hold duration, color = black
            5 => Self { param0: 0.0, color: [0.0, 0.0, 0.0, 1.0], ..Self::default() },
            // DipToWhite: param0 = hold duration, color = white (already default)
            6 => Self { param0: 0.0, color: [1.0, 1.0, 1.0, 1.0], ..Self::default() },
            // Push: param0 = direction (0=Left, 1=Right, 2=Up, 3=Down)
            7 => Self { param0: 0.0, ..Self::default() },
            // BlurDissolve: param0 = max blur radius in pixels (default 32)
            9 => Self { param0: 32.0, ..Self::default() },
            // Flash: param0 = intensity, param1 = peak position, color = flash colour
            10 => Self {
                param0: 1.0,
                param1: 0.5,
                color: [1.0, 1.0, 1.0, 1.0],
                ..Self::default()
            },
            // Spin: param0 = number of rotations (1.0 = one full turn),
            //       param1 = zoom bloom amount (0.3 = current default look)
            11 => Self { param0: 1.0, param1: 0.3, ..Self::default() },
            // LightLeak: param0 = intensity, color = warm amber by default
            12 => Self {
                param0: 1.0,
                color: [1.0, 0.706, 0.235, 1.0],
                ..Self::default()
            },
            _ => Self::default(),
        }
    }
}
