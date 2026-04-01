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
}

/// GPU device preference for wgpu adapter selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum GpuPreference {
    /// Prefer discrete GPU (NVIDIA/AMD).
    #[default]
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
