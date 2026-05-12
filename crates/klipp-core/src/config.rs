//! Shared configuration types used across crates for user preferences.
//!
//! These enums are defined in klipp-core so they're reachable by klipp-audio,
//! klipp-color, klipp-engine, and klipp-ui. klipp-effects (which can't depend
//! on klipp-core) receives raw wgpu types instead.

use serde::{Deserialize, Serialize};

/// Stereo pan law — how the pan knob distributes signal between L/R channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PanLaw {
    /// Balance law: pan_l = 1 - pan, pan_r = 1 + pan. Current default.
    #[default]
    Linear,
    /// Constant-power (equal-power): preserves perceived loudness at center.
    ConstantPower,
    /// -3 dB at center (compromise between linear and constant-power).
    Minus3dB,
    /// -4.5 dB at center (Premiere Pro default).
    Minus4_5dB,
    /// -6 dB at center (hard pan, full cut to opposite channel at extremes).
    Minus6dB,
}

/// Audio metering ballistics mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MeteringBallistics {
    /// Standard peak with instant attack.
    #[default]
    Peak,
    /// VU meter with ~300ms integration time.
    Vu,
    /// PPM (Peak Programme Meter) with 5ms attack, 1.7s release.
    Ppm,
    /// True peak (oversampled peak detection).
    TruePeak,
}

/// Loudness metering standard for normalization and monitoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LoudnessStandard {
    /// EBU R128 (-23 LUFS target, European broadcast).
    #[default]
    EbuR128,
    /// ATSC A/85 (-24 LKFS target, US broadcast).
    AtscA85,
    /// No loudness standard applied.
    Off,
}

/// Scope rendering quality (controls pixel sampling step).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ScopeQuality {
    /// Sample every 4th pixel (fast, low detail).
    Low,
    /// Sample every 2nd pixel (default balance).
    #[default]
    Medium,
    /// Sample every pixel (slow, full detail).
    High,
}

impl ScopeQuality {
    /// Convert to pixel sample step for scope compute functions.
    pub fn sample_step(&self) -> usize {
        match self {
            Self::Low => 4,
            Self::Medium => 2,
            Self::High => 1,
        }
    }

    /// Splat-kernel dimensions (width × height) for trace deposition.
    /// **Width is always 1** — single-column deposition. Real CRT
    /// phosphor energy follows the electron beam vertically; horizontal
    /// spread comes from many independent samples landing in the same
    /// column over many frames, not from one splat bleeding sideways
    /// into its neighbours. The previous 3-px-wide footprint filled
    /// the gaps BETWEEN columns and made dense regions read as a
    /// continuous luminous mass instead of as accumulated filaments.
    /// Height controls the vertical streak length (the beam's drift
    /// during the sample window).
    pub fn splat_kernel_size(&self) -> (usize, usize) {
        match self {
            Self::Low => (1, 1),
            Self::Medium => (1, 5),
            Self::High => (1, 7),
        }
    }

    /// Per-axis Gaussian standard deviations (σx, σy). σx is always 0
    /// — width=1 has no horizontal axis to blur. σy controls how
    /// gradually the vertical streak falls off from the centre row.
    pub fn splat_kernel_sigma(&self) -> (f32, f32) {
        match self {
            Self::Low => (0.0, 0.0),
            Self::Medium => (0.0, 1.4),
            Self::High => (0.0, 1.8),
        }
    }

    /// FFmpeg scope input downsample dimensions for this quality tier.
    /// Lower quality feeds a smaller frame to the filter graph so the
    /// internal histogram cost scales with the user's perf budget.
    pub fn ffmpeg_downsample(&self) -> (u32, u32) {
        match self {
            Self::Low => (320, 180),
            Self::Medium => (640, 360),
            Self::High => (960, 540),
        }
    }

    /// FFmpeg waveform `intensity` parameter for this quality tier.
    /// Higher quality compensates the denser sampling so bright peaks
    /// don't oversaturate at higher resolutions.
    pub fn ffmpeg_intensity(&self) -> f32 {
        match self {
            Self::Low => 0.04,
            Self::Medium => 0.08,
            Self::High => 0.16,
        }
    }
}

/// Filter algorithm for the libavfilter `waveform` filter (waveform +
/// parade modes share this knob). Each value names exactly the
/// FFmpeg `filter=` keyword so the spec builder is a 1:1 translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WaveformFilter {
    /// `filter=lowpass` — soft column blur, gradient look. Default.
    #[default]
    Lowpass,
    /// `filter=flat` — raw column-density dot histogram, sparse look.
    Flat,
    /// `filter=aflat` — anti-aliased flat, smoother trace edges.
    Aflat,
    /// `filter=chroma` — only the chroma planes, luma muted.
    Chroma,
    /// `filter=color` — per-pixel colour painting modulated by source.
    Color,
    /// `filter=acolor` — anti-aliased colour painting.
    Acolor,
}

impl WaveformFilter {
    pub fn keyword(&self) -> &'static str {
        match self {
            Self::Lowpass => "lowpass",
            Self::Flat => "flat",
            Self::Aflat => "aflat",
            Self::Chroma => "chroma",
            Self::Color => "color",
            Self::Acolor => "acolor",
        }
    }
}

/// Per-channel visibility for waveform & parade. Translates to FFmpeg's
/// `components=` bitmask (R=4, G=2, B=1). At least one channel is
/// forced on so the filter graph parses cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMask {
    pub r: bool,
    pub g: bool,
    pub b: bool,
}

impl Default for ChannelMask {
    fn default() -> Self { Self { r: true, g: true, b: true } }
}

impl ChannelMask {
    pub fn to_components(&self) -> u8 {
        let mut m = 0u8;
        if self.r { m |= 4; }
        if self.g { m |= 2; }
        if self.b { m |= 1; }
        if m == 0 { 1 } else { m }
    }
}

/// Settings for waveform & parade modes (same shape — both are
/// libavfilter `waveform` filter invocations with different `display=`
/// values which the spec builder injects automatically). `intensity`
/// floors at the quality-tier value so a Draft preset with intensity 0.4
/// can't out-blow a Full preset's 0.16 by accident.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WaveformParadeSettings {
    pub filter: WaveformFilter,
    /// Per-mode override of the quality-tier intensity. Negative ⇒ use
    /// the tier default. Positive ⇒ direct override.
    pub intensity_override: f32,
    pub channels: ChannelMask,
}

impl Default for WaveformParadeSettings {
    fn default() -> Self {
        Self {
            filter: WaveformFilter::Lowpass,
            intensity_override: -1.0,
            channels: ChannelMask::default(),
        }
    }
}

// f32 fields prevent the derive — implement Eq manually so the type can
// live in HashMap/BTreeSet keys if ever needed.
impl Eq for WaveformParadeSettings {}

/// libavfilter `vectorscope` `mode=` choice. The "color" variants paint
/// the trace in per-pixel input colour; higher numbers are saturation-
/// boosted blends. `gray` is luma-only (rarely useful for grading).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VectorscopeMode {
    Gray,
    Color,
    Color2,
    #[default]
    Color3,
    Color4,
    Color5,
}

impl VectorscopeMode {
    pub fn keyword(&self) -> &'static str {
        match self {
            Self::Gray => "gray",
            Self::Color => "color",
            Self::Color2 => "color2",
            Self::Color3 => "color3",
            Self::Color4 => "color4",
            Self::Color5 => "color5",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct VectorscopeSettings {
    pub mode: VectorscopeMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HistogramDisplayMode {
    #[default]
    Parade,
    Stack,
    Overlay,
}

impl HistogramDisplayMode {
    pub fn keyword(&self) -> &'static str {
        match self {
            Self::Parade => "parade",
            Self::Stack => "stack",
            Self::Overlay => "overlay",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HistogramLevels {
    #[default]
    Linear,
    Logarithmic,
}

impl HistogramLevels {
    pub fn keyword(&self) -> &'static str {
        match self {
            Self::Linear => "linear",
            Self::Logarithmic => "logarithmic",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct HistogramSettings {
    pub display_mode: HistogramDisplayMode,
    pub levels: HistogramLevels,
}

/// Per-mode runtime settings for the FFmpeg scope pipeline. Replaces
/// the global Phase-3 toggles: each scope mode can now run a different
/// filter, intensity, and (for waveform/parade) channel mask. Quality
/// stays global because it's a perf-budget knob, not a per-mode visual
/// choice. Plumbed engine → bank → spec builder; a change triggers a
/// per-mode filter-graph rebuild on the next render.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct ScopeFfmpegSettings {
    pub waveform: WaveformParadeSettings,
    pub parade: WaveformParadeSettings,
    pub vectorscope: VectorscopeSettings,
    pub histogram: HistogramSettings,
    /// Global perf preset — drives input downsample + the
    /// fallback intensity used when a per-mode `intensity_override`
    /// is negative.
    pub quality: ScopeQuality,
}

impl Eq for ScopeFfmpegSettings {}

/// GPU device preference for wgpu adapter selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GpuPreference {
    /// Automatic GPU selection (recommended).
    #[default]
    Auto,
    /// Prefer discrete GPU (NVIDIA/AMD).
    HighPerformance,
    /// Prefer integrated GPU (lower power).
    LowPower,
    /// Force CPU fallback (disable GPU effects).
    CpuOnly,
}

/// Playback preview quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PlaybackQuality {
    /// Full resolution.
    #[default]
    Full,
    /// Half resolution.
    Half,
    /// Quarter resolution.
    Quarter,
}
