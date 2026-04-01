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
    /// Apple Log (iPhone 15 Pro and later)
    AppleLog,
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
