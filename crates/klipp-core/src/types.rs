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
    /// Leica L-Log as the SL (Typ 601) records it: the same curve, in
    /// BT.709 primaries rather than BT.2020 (#1642).
    LeicaLLogBt709,
    /// RED Log3G10
    RedLog3G10,
    /// Blackmagic Design Film Generation 5
    BmdFilmGen5,
    /// Hybrid Log-Gamma (BBC/NHK, ITU-R BT.2100)
    HLG,
    /// Perceptual Quantizer (SMPTE ST 2084)
    PQ,
}

impl LogFormat {
    /// The camera-native (transfer, primaries) pair a file in this format
    /// is decoded with, or `None` for formats that are not camera logs.
    /// Range and matrix still come from the file: Sony, ARRI and Canon log
    /// all use the BT.709 matrix. Shared by the viewer's and the export's
    /// decoders so the two cannot decode the same file differently.
    pub fn camera_colourspace(self) -> Option<(crate::ColorTransfer, crate::ColorPrimaries)> {
        use crate::{ColorPrimaries as P, ColorTransfer as T};
        Some(match self {
            Self::SLog3 => (T::SLog3, P::SGamut3Cine),
            Self::SLog2 => (T::SLog2, P::SGamut3),
            Self::VLog => (T::VLog, P::VGamut),
            Self::CanonLog => (T::CanonLog, P::CanonCinemaGamut),
            Self::CanonLog3 => (T::CanonLog3, P::CanonCinemaGamut),
            Self::LogC3 => (T::LogC3, P::AlexaWideGamut3),
            Self::LogC4 => (T::LogC4, P::AlexaWideGamut4),
            Self::RedLog3G10 => (T::RedLog3G10, P::RedWideGamut),
            Self::BmdFilmGen5 => (T::BmdFilmGen5, P::BmdWideGamut),
            Self::DJIDLog => (T::DJIDLog, P::Bt709),
            Self::FLog => (T::FLog, P::Bt2020),
            Self::AppleLog => (T::AppleLog, P::Bt2020),
            Self::NLog => (T::NLog, P::Bt2020),
            Self::GoProFlat => (T::GoProFlat, P::Bt709),
            // Leica records L-Log in BT.2020 on every current body — SL2,
            // SL2-S, SL3, SL3-S and Q3 — and in BT.709 on the SL (Typ 601)
            // alone, per Table 3 of the L-Log Reference Manual (v1.6, v1.9).
            // The curve is one; the camera picks the primaries (#1642).
            Self::LeicaLLog => (T::LeicaLLog, P::Bt2020),
            Self::LeicaLLogBt709 => (T::LeicaLLog, P::Bt709),
            // sRGB, linear, PQ and HLG are not camera logs.
            Self::Srgb | Self::Linear | Self::PQ | Self::HLG => return None,
        })
    }

    /// `cs` with this format's camera transfer and primaries, when it is a
    /// camera log; `cs` unchanged otherwise.
    pub fn apply_to(self, cs: crate::SourceColorspace) -> crate::SourceColorspace {
        match self.camera_colourspace() {
            Some((transfer, primaries)) => crate::SourceColorspace { transfer, primaries, ..cs },
            None => cs,
        }
    }

    /// Whether an InputTransform node carrying this format has any work
    /// to do on the pixels it receives. It never does now.
    ///
    /// Every format is decoded before the graph runs: PQ/HLG by the
    /// decoder's HDR input stage, camera logs the same way once the media
    /// pool tells the decoder the file's format (`set_media_log_formats`).
    /// A node that transformed again would transform twice — exactly how
    /// every PQ clip used to crush the moment it was graded. The node still
    /// records the source format and is where the user picks it; it just
    /// must not act on the pixels.
    ///
    /// Its old in-graph transform was also broken for camera logs in its
    /// own right: the GPU version clamped scene-linear to [0, 1], throwing
    /// away every highlight above white, and neither version converted the
    /// camera's gamut.
    pub fn needs_input_transform(self) -> bool {
        false
    }

    /// Whether this is a camera log curve the decoder must decode (as
    /// opposed to display-referred sRGB/linear, or PQ/HLG, which the
    /// decoder recognises from the file's own tags).
    pub fn needs_camera_decode(self) -> bool {
        !matches!(self, Self::Srgb | Self::Linear | Self::PQ | Self::HLG)
    }
}

// `BlendMode` lives in klipp-effects (alongside the composite math
// that consumes it). klipp-core can't own the type because klipp-core
// already depends on klipp-effects via `BuiltinEffect` payloads — a
// reverse dep would close a cycle. Re-exporting here keeps the
// existing `klipp_core::BlendMode` import paths working unchanged.
pub use klipp_effects::BlendMode;
/// The grain a gradient generator carries unless told otherwise (#2073).
pub use klipp_effects::generator_grain::DEFAULT_GENERATOR_GRAIN;

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
    /// The left channel alone, on both sides: one half of a stereo clip
    /// split to mono (#922).
    LeftOnly,
    /// The right channel alone, on both sides (#922).
    RightOnly,
}

/// Audio fade curve shape.
///
/// The first four shape a fade against silence, where there is nothing
/// to sum with and the only question is how the level should travel.
/// [`FadeCurve::ConstantPower`] answers a different question, and the
/// two are not interchangeable — see its own note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FadeCurve {
    #[default]
    Linear,
    Exponential,
    Logarithmic,
    SCurve,
    /// Equal-power: the curve for a *crossfade*, where two sounds
    /// overlap.
    ///
    /// Uncorrelated audio sums by power, not by amplitude. Two linear
    /// ramps crossing both sit at 0.5 gain halfway, so the combined
    /// power is `0.5² + 0.5² = 0.5` — a 3 dB dip, and it lands exactly
    /// where the transition is most audible. A listener hears the
    /// sound duck and come back.
    ///
    /// `sin`/`cos` ramps sum to unity power throughout, so the
    /// crossfade is inaudible. This is why it is the default audio
    /// transition in Premiere and Avid.
    ///
    /// It is the wrong choice for a fade from or to silence: it starts
    /// and ends steeply, which is audible when there is no second
    /// sound to hide it. Hence a separate option rather than a new
    /// default — and see the note in [`Self::gain`].
    ConstantPower,
}

impl FadeCurve {
    /// The gain at `t`, where `t` runs 0 at silence to 1 at full.
    ///
    /// One definition, so playback and export cannot drift. Both used
    /// to carry their own copy of this `match` — `pcm_reader`'s
    /// `apply_curve` and `audio_mixdown`'s `fade_curve` — and adding a
    /// shape meant remembering both.
    pub fn gain(self, t: f64) -> f64 {
        match self {
            FadeCurve::Linear => t,
            // Slow start, fast end.
            FadeCurve::Exponential => t * t,
            // Fast start, slow end.
            FadeCurve::Logarithmic => 1.0 - (1.0 - t) * (1.0 - t),
            // Smoothstep: slow, fast, slow.
            FadeCurve::SCurve => t * t * (3.0 - 2.0 * t),
            // sin(t·π/2). Its partner cos(t·π/2) is the same curve
            // read backwards, which is what the fade-out side passes,
            // so `in² + out² == 1` at every point of the overlap.
            FadeCurve::ConstantPower => (t * std::f64::consts::FRAC_PI_2).sin(),
        }
    }

    /// The wire form playback and export share. Appending only, since
    /// saved projects carry the number.
    pub fn to_index(self) -> u32 {
        match self {
            FadeCurve::Linear => 0,
            FadeCurve::Exponential => 1,
            FadeCurve::Logarithmic => 2,
            FadeCurve::SCurve => 3,
            FadeCurve::ConstantPower => 4,
        }
    }

    /// The inverse, with anything unrecognised falling back to linear
    /// rather than panicking on a project from a later version.
    pub fn from_index(i: u32) -> Self {
        match i {
            1 => FadeCurve::Exponential,
            2 => FadeCurve::Logarithmic,
            3 => FadeCurve::SCurve,
            4 => FadeCurve::ConstantPower,
            _ => FadeCurve::Linear,
        }
    }
}

/// Where a clip's role came from (#2216): never set, chosen by a person
/// (or an agent asked to), or proposed once from what the sound is. A
/// role set by hand is never replaced by a proposal, and a clip is
/// proposed a role only once, so undoing a proposal keeps it undone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoleSource {
    #[default]
    Unset,
    ByHand,
    Proposed,
}

impl RoleSource {
    pub fn is_unset(&self) -> bool {
        *self == RoleSource::Unset
    }
}

/// What a media file's sound was classified as (#2216): the role it
/// proposes for its clips, how sure the classifier is (0 to 1), and
/// whether that is sure enough to apply without asking.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RoleProposal {
    pub role: ClipRole,
    pub confidence: f32,
    pub confident: bool,
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
    /// A window track's size and turn on each frame of `positions`, as
    /// (scale, degrees clockwise) from the reference frame (#238). Empty
    /// for a point track, which has a place only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub motion: Vec<(f64, f64)>,
    /// The frame the window was drawn on, which the track is measured
    /// from (#238).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_frame: Option<usize>,
    /// A window tracked in perspective (#238): where the corners of the
    /// region followed are on each frame of `positions`, in frame
    /// fractions, top left first, clockwise. Empty otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corners: Vec<[(f64, f64); 4]>,
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
            motion: Vec::new(),
            reference_frame: None,
            corners: Vec::new(),
        }
    }
}

/// Serializable audio effect instance (stored on Clip).
/// Each instance records the effect type, enabled state, wet/dry mix, and param values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// The piece transitions' subject cut out as its own piece (#985
    /// phase 3); none for every other kind, and unless set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<crate::piece_transition::SubjectCut>,
}

impl Default for TransitionUserParams {
    fn default() -> Self {
        Self {
            param0: 0.0,
            param1: 0.0,
            param2: 0.0,
            param3: 0.0,
            color: [1.0, 1.0, 1.0, 1.0],
            subject: None,
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

/// Direction of `GeneratorData::Timer`. CountUp is the default
/// "screen-recording timer" workflow; CountDown is for deadline-style
/// segments that complement the existing `Countdown` 7-segment-only
/// generator.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum TimerMode {
    #[default]
    CountUp,
    CountDown,
}

/// Display precision for `GeneratorData::Timer`. Picks how many digit
/// groups the 7-segment renderer draws; the rest of the time value is
/// truncated.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum TimerFormat {
    /// `SS` — bare seconds.
    Seconds,
    /// `MM:SS` — default for short demos / screen recordings.
    #[default]
    MmSs,
    /// `HH:MM:SS` — for long-form content.
    HhMmSs,
    /// `HH:MM:SS.mmm` — milliseconds for precision timing.
    HhMmSsMs,
}

/// Data for a generator clip (solid color, gradient, bars, countdown, still).
/// Lives in klipp-core so AppCommand can carry it directly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum GeneratorData {
    /// Solid color fill.
    SolidColor { color: [u8; 4] },
    /// A background that lives: the gradient's angle turns slowly and
    /// a soft light drifts across it. `grain` is the fine grain it
    /// carries so it stays smooth through an 8-bit encode, light
    /// included (#2073): code values of standard deviation, 0 for none.
    LivingGradient {
        a: [u8; 4],
        b: [u8; 4],
        angle_deg: f32,
        drift_deg_per_sec: f32,
        glow: f32,
        #[serde(default = "default_generator_grain")]
        grain: f32,
    },
    /// Film grain on a colour: a texture plate for behind titles.
    /// `amount` 0..1, `size_px` the grain size at 1080p, `speed` in
    /// new grain frames per second.
    Grain { color: [u8; 4], amount: f32, size_px: f32, speed: f32 },
    /// A labelled card standing in for a shot that is not cut yet.
    Placeholder { label: String, color: [u8; 4] },
    /// Two-point linear gradient (`top` → `bottom` along
    /// `angle_deg`) or, when `four_point` is `Some`, a 4-corner
    /// bilinear gradient that ignores `top` / `bottom` / `angle_deg`.
    /// `angle_deg` is the gradient direction in degrees: 0 = top→bottom
    /// (the historical default), 90 = left→right, 180 = bottom→top,
    /// 270 = right→left. The two `#[serde(default)]` fields keep older
    /// projects loadable — they deserialize to angle 0 / `None` and
    /// render exactly as before. `grain` is the fine grain the gradient
    /// carries so it stays smooth through an 8-bit encode (#2073), as
    /// [`GeneratorData::LivingGradient`]'s.
    Gradient {
        top: [u8; 4],
        bottom: [u8; 4],
        #[serde(default)]
        angle_deg: f32,
        #[serde(default)]
        four_point: Option<FourPointGradient>,
        #[serde(default = "default_generator_grain")]
        grain: f32,
    },
    /// SMPTE 75 % color bars (broadcast test pattern). The historical
    /// default — kept as a unit variant for byte-identical back-compat
    /// with every saved project. SMPTE 100 % is `ColorBars100` instead
    /// of an `intensity` field on this variant so older saves don't
    /// need a custom serde alias.
    ColorBars,
    /// SMPTE 100 % color bars — same seven vertical bars as
    /// `ColorBars` but at full intensity (255 vs 191). Used for
    /// hardware calibration and broadcast peak-luminance verification.
    ColorBars100,
    /// Countdown timer (seconds remaining overlaid on black).
    Countdown,
    /// On-screen timer counting up from clip start (`CountUp`) or down
    /// from `initial_seconds` (`CountDown`). Reuses `Countdown`'s
    /// 7-segment digit renderer; adds a colon segment between hours /
    /// minutes / seconds. `format` picks the displayed precision.
    Timer {
        mode: TimerMode,
        /// CountUp: display starts at `initial_seconds` and grows.
        /// CountDown: display starts at `initial_seconds` and shrinks
        /// to zero (then clamps).
        initial_seconds: f32,
        format: TimerFormat,
        text_color: [u8; 4],
        background_color: [u8; 4],
    },
    /// Production slate — text fields commonly burned into camera
    /// reports / dailies. Rendered via `klipp-title` so the typography
    /// matches Klipp's title system; the engine intercepts this variant
    /// before calling `render_generator` (which would only produce a
    /// magenta error frame from inside klipp-core).
    Slate {
        project: String,
        scene: String,
        take: String,
        director: String,
        camera: String,
        date: String,
        framerate: String,
        notes: String,
        text_color: [u8; 4],
        background_color: [u8; 4],
    },
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

impl GeneratorData {
    /// Whether the picture shows the clip's own time (a count), so it
    /// must be drawn only at times inside the clip: a count before the
    /// clip starts or after it ends would show a number the clip never
    /// reaches. Every other generator can be drawn at any time, before its
    /// start too, as a transition's overlap asks (#1918). Each kind says,
    /// so a new one decides.
    pub fn shows_own_time(&self) -> bool {
        match self {
            GeneratorData::Countdown | GeneratorData::Timer { .. } => true,
            GeneratorData::SolidColor { .. }
            | GeneratorData::LivingGradient { .. }
            | GeneratorData::Grain { .. }
            | GeneratorData::Placeholder { .. }
            | GeneratorData::Gradient { .. }
            | GeneratorData::ColorBars
            | GeneratorData::ColorBars100
            | GeneratorData::Slate { .. }
            | GeneratorData::StillFrame { .. }
            | GeneratorData::Polygon { .. }
            | GeneratorData::Star { .. } => false,
        }
    }
}

fn default_fill() -> bool { true }

/// A gradient without a `grain` of its own takes the default, so every
/// one carries it unless it is turned off (#2073).
fn default_generator_grain() -> f32 { klipp_effects::generator_grain::DEFAULT_GENERATOR_GRAIN }

impl Default for GeneratorData {
    fn default() -> Self {
        GeneratorData::SolidColor { color: [0, 0, 0, 255] }
    }
}

#[cfg(test)]
mod log_format_tests {
    use super::LogFormat;

    /// Every format is decoded before the graph runs; the node must never
    /// transform again. Camera logs are the ones the decoder needs told.
    #[test]
    fn no_format_needs_the_node_and_camera_logs_need_the_decoder() {
        for f in [LogFormat::Srgb, LogFormat::Linear, LogFormat::PQ, LogFormat::HLG,
                  LogFormat::SLog3, LogFormat::LogC3, LogFormat::VLog, LogFormat::AppleLog] {
            assert!(!f.needs_input_transform(), "{f:?}");
        }
        for f in [LogFormat::Srgb, LogFormat::Linear, LogFormat::PQ, LogFormat::HLG] {
            assert!(!f.needs_camera_decode(), "{f:?}");
        }
        for f in [LogFormat::SLog3, LogFormat::LogC4, LogFormat::VLog, LogFormat::AppleLog] {
            assert!(f.needs_camera_decode(), "{f:?}");
        }
    }
}

#[cfg(test)]
mod fade_curve_tests {
    use super::FadeCurve;

    const ALL: [FadeCurve; 5] = [
        FadeCurve::Linear,
        FadeCurve::Exponential,
        FadeCurve::Logarithmic,
        FadeCurve::SCurve,
        FadeCurve::ConstantPower,
    ];

    /// Whatever the shape, a fade starts at silence and ends at full.
    #[test]
    fn every_curve_runs_from_nothing_to_everything() {
        for c in ALL {
            assert!(c.gain(0.0).abs() < 1e-12, "{c:?} does not start at 0");
            assert!((c.gain(1.0) - 1.0).abs() < 1e-12, "{c:?} does not reach 1");
        }
    }

    #[test]
    fn every_curve_only_ever_rises() {
        for c in ALL {
            let mut last = -1.0;
            for i in 0..=1000 {
                let g = c.gain(i as f64 / 1000.0);
                assert!(g >= last - 1e-12, "{c:?} dips at t={}", i as f64 / 1000.0);
                assert!((0.0..=1.0).contains(&g), "{c:?} leaves 0..1 at t={}", i as f64 / 1000.0);
                last = g;
            }
        }
    }

    /// The reason this curve exists.
    ///
    /// A crossfade runs one clip's fade-out against another's fade-in.
    /// Uncorrelated audio sums by power, so what has to stay constant
    /// is `in² + out²`, not `in + out`.
    #[test]
    fn constant_power_holds_the_power_constant_across_a_crossfade() {
        for i in 0..=1000 {
            let t = i as f64 / 1000.0;
            let fading_in = FadeCurve::ConstantPower.gain(t);
            // The other clip is at the same point of its fade-out, which
            // is this curve read backwards.
            let fading_out = FadeCurve::ConstantPower.gain(1.0 - t);
            let power = fading_in * fading_in + fading_out * fading_out;
            assert!(
                (power - 1.0).abs() < 1e-9,
                "power {power} at t={t}, should be 1",
            );
        }
    }

    /// And the reason the other curves are not it: the dip this
    /// replaces, on the record as a number.
    ///
    /// Linear ramps cross at 0.5 each, summing to 0.5 power — a little
    /// over 3 dB down, arriving exactly halfway through the transition
    /// where it is most audible.
    #[test]
    fn a_linear_crossfade_dips_by_three_decibels_in_the_middle() {
        let a = FadeCurve::Linear.gain(0.5);
        let b = FadeCurve::Linear.gain(0.5);
        let power = a * a + b * b;
        assert!((power - 0.5).abs() < 1e-12, "expected 0.5, got {power}");
        let db = 10.0 * power.log10();
        assert!((db + 3.0103).abs() < 0.001, "expected -3.01 dB, got {db}");

        // Constant power, at the same point, does not.
        let c = FadeCurve::ConstantPower.gain(0.5);
        let d = FadeCurve::ConstantPower.gain(0.5);
        assert!(((c * c + d * d) - 1.0).abs() < 1e-9);
    }

    /// The other four are unchanged: they were transcribed in five
    /// places and are now defined once, so this pins the shapes that
    /// were there before against what the single definition returns.
    #[test]
    fn the_existing_shapes_are_what_they_were() {
        for i in 0..=100 {
            let t = i as f64 / 100.0;
            assert!((FadeCurve::Linear.gain(t) - t).abs() < 1e-12);
            assert!((FadeCurve::Exponential.gain(t) - t * t).abs() < 1e-12);
            assert!(
                (FadeCurve::Logarithmic.gain(t) - (1.0 - (1.0 - t) * (1.0 - t))).abs() < 1e-12
            );
            assert!((FadeCurve::SCurve.gain(t) - t * t * (3.0 - 2.0 * t)).abs() < 1e-12);
            // The timeline drew the S-curve as `3t² - 2t³`, which is the
            // same polynomial factored differently. Pinned so the two
            // spellings cannot drift back apart.
            assert!(
                (FadeCurve::SCurve.gain(t) - (3.0 * t * t - 2.0 * t * t * t)).abs() < 1e-12
            );
        }
    }

    /// Saved projects carry the index, so the numbering is a format.
    #[test]
    fn the_wire_indices_round_trip_and_only_ever_append() {
        assert_eq!(FadeCurve::Linear.to_index(), 0);
        assert_eq!(FadeCurve::Exponential.to_index(), 1);
        assert_eq!(FadeCurve::Logarithmic.to_index(), 2);
        assert_eq!(FadeCurve::SCurve.to_index(), 3);
        assert_eq!(FadeCurve::ConstantPower.to_index(), 4);
        for c in ALL {
            assert_eq!(FadeCurve::from_index(c.to_index()), c, "{c:?}");
        }
        // A project written by a later version must open, not panic.
        assert_eq!(FadeCurve::from_index(99), FadeCurve::Linear);
    }

    /// Adding a shape must not move the default, which existing
    /// projects rely on for every fade that omits the field.
    #[test]
    fn the_default_is_still_linear() {
        assert_eq!(FadeCurve::default(), FadeCurve::Linear);
    }
}
