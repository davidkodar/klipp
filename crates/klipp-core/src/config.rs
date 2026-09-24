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

impl PanLaw {
    /// Left and right gains for a pan position in `[-1, 1]`.
    ///
    /// Lives here rather than in the mixer because two places mix: live
    /// playback and the export. They used different laws — playback
    /// asked this enum, the export had the constant-power trig written
    /// into it — so a centred stereo track came out of a render 3 dB
    /// below what it sounded like, at default settings, in every
    /// project. One definition, so that cannot drift again.
    ///
    /// Note what the default means for stereo material: `Linear` is a
    /// balance control, unity on both channels at centre, which is what
    /// passing a stereo track through untouched requires. The
    /// equal-power laws are for panning a *mono* source, where holding
    /// perceived loudness across the sweep costs 3 dB in the middle.
    /// Both gains are in `[0, 1]`. The clamp is not decoration:
    /// `cos(PI/2)` comes back as -4.4e-8, so the constant-power law at
    /// hard right produces a *negative* gain, and a negative gain
    /// inverts the signal's polarity rather than quietening it.
    pub fn gains(self, pan: f32) -> (f32, f32) {
        let (l, r) = self.raw_gains(pan.clamp(-1.0, 1.0));
        (l.clamp(0.0, 1.0), r.clamp(0.0, 1.0))
    }

    fn raw_gains(self, pan: f32) -> (f32, f32) {
        match self {
            PanLaw::Linear => {
                let l = if pan <= 0.0 { 1.0 } else { 1.0 - pan };
                let r = if pan >= 0.0 { 1.0 } else { 1.0 + pan };
                (l, r)
            }
            PanLaw::ConstantPower => {
                let angle = (pan + 1.0) * 0.5 * std::f32::consts::FRAC_PI_2;
                (angle.cos(), angle.sin())
            }
            PanLaw::Minus3dB => {
                let t = (pan + 1.0) * 0.5;
                (((1.0 - t) * 2.0).min(1.0).sqrt(), (t * 2.0).min(1.0).sqrt())
            }
            PanLaw::Minus4_5dB => {
                let t = (pan + 1.0) * 0.5;
                (
                    ((1.0 - t) * 2.0).min(1.0).powf(0.75),
                    (t * 2.0).min(1.0).powf(0.75),
                )
            }
            PanLaw::Minus6dB => {
                let t = (pan + 1.0) * 0.5;
                (((1.0 - t) * 2.0).min(1.0), (t * 2.0).min(1.0))
            }
        }
    }
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

/// Per-channel visibility for waveform & parade. At least one channel is
/// always drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelMask {
    pub r: bool,
    pub g: bool,
    pub b: bool,
}

impl Default for ChannelMask {
    fn default() -> Self {
        Self {
            r: true,
            g: true,
            b: true,
        }
    }
}

impl ChannelMask {
    /// The channels as a bitmask, R=1, G=2, B=4. At least one bit is set
    /// (R alone when every channel is off), so a trace is always drawn.
    pub fn to_components(&self) -> u8 {
        let mut m = 0u8;
        if self.r {
            m |= 1;
        }
        if self.g {
            m |= 2;
        }
        if self.b {
            m |= 4;
        }
        if m == 0 {
            1
        } else {
            m
        }
    }
}

/// Settings for the waveform and the parade, which share a shape.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WaveformParadeSettings {
    /// Trace brightness, 0..1: 0.5 is the drawn density as it is, 1 is
    /// six times brighter and 0 six times dimmer. Negative ⇒ 0.5.
    pub intensity: f32,
    pub channels: ChannelMask,
    /// Draw each column's lowest and highest level as a line.
    pub envelope: bool,
}

impl Default for WaveformParadeSettings {
    fn default() -> Self {
        Self {
            intensity: -1.0,
            channels: ChannelMask::default(),
            envelope: false,
        }
    }
}

// f32 fields prevent the derive — implement Eq manually so the type can
// live in HashMap/BTreeSet keys if ever needed.
impl Eq for WaveformParadeSettings {}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VectorscopeSettings {
    /// Tint the trace by the hue at each point; otherwise it is mono.
    pub colour: bool,
    /// Only pixels whose Y′ is at least this are plotted. Default 0.
    pub luma_lo: f32,
    /// Only pixels whose Y′ is at most this are plotted. Default 1.
    pub luma_hi: f32,
    /// Magnification of the chroma plane: 1 shows ±0.5, 2 shows ±0.25.
    #[serde(default = "default_vectorscope_zoom")]
    pub zoom: f32,
}

fn default_vectorscope_zoom() -> f32 {
    1.0
}

impl Default for VectorscopeSettings {
    fn default() -> Self {
        Self {
            colour: true,
            luma_lo: 0.0,
            luma_hi: 1.0,
            zoom: 1.0,
        }
    }
}

impl Eq for VectorscopeSettings {}

/// How the histogram lays out its channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HistogramDisplayMode {
    /// Red, green and blue side by side.
    Parade,
    /// Red, green and blue in rows.
    Stack,
    /// Red, green and blue over each other, additive.
    #[default]
    Overlay,
    /// One distribution of Y′.
    Luma,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HistogramLevels {
    #[default]
    Linear,
    Logarithmic,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HistogramSettings {
    pub display_mode: HistogramDisplayMode,
    /// Height by the count, or by its logarithm.
    pub levels: HistogramLevels,
    /// Brightness of the area under each curve, 0..1. Default 0.7.
    pub fill: f32,
}

impl Default for HistogramSettings {
    fn default() -> Self {
        Self {
            display_mode: HistogramDisplayMode::Overlay,
            levels: HistogramLevels::Linear,
            fill: 0.7,
        }
    }
}

impl Eq for HistogramSettings {}

/// The scopes' settings, as the UI sends them to the engine. Quality is
/// global because it is a perf-budget knob, not a per-mode visual choice.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct ScopeSettings {
    pub waveform: WaveformParadeSettings,
    pub parade: WaveformParadeSettings,
    pub vectorscope: VectorscopeSettings,
    pub histogram: HistogramSettings,
    /// How many pixels are measured during playback.
    pub quality: ScopeQuality,
    /// Draw the waveform and parade as one trace of Y′ rather than the
    /// red, green and blue traces. Does not affect the vectorscope or the
    /// histogram.
    pub luma_only: bool,
}

impl Eq for ScopeSettings {}

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

#[cfg(test)]
mod pan_law_tests {
    use super::PanLaw;

    fn db(g: f32) -> f32 {
        20.0 * g.log10()
    }

    /// The default law passes a centred stereo track through untouched.
    ///
    /// This is the one that mattered: the export had the constant-power
    /// law written into it, which is 0.707 at centre, so every render of
    /// a centred stereo track came out 3 dB below what it sounded like
    /// in the viewer. Measured on real footage before the fix: source
    /// peak -29.4 dBFS, rendered peak -32.5.
    #[test]
    fn the_default_law_is_unity_at_centre() {
        let (l, r) = PanLaw::default().gains(0.0);
        assert_eq!(
            (l, r),
            (1.0, 1.0),
            "a centred stereo track is not attenuated"
        );
    }

    /// And the equal-power laws are not, by design — they hold perceived
    /// loudness while a *mono* source sweeps, which costs level in the
    /// middle. The numbers are the point: each law's name is its centre
    /// gain, so a wrong one is visible here rather than only audible.
    #[test]
    fn each_law_has_the_centre_gain_its_name_claims() {
        for (law, want_db) in [
            (PanLaw::Linear, 0.0),
            (PanLaw::ConstantPower, -3.01),
            (PanLaw::Minus3dB, 0.0),
            (PanLaw::Minus4_5dB, 0.0),
            (PanLaw::Minus6dB, 0.0),
        ] {
            let (l, r) = law.gains(0.0);
            assert!(
                (l - r).abs() < 1e-6,
                "{law:?}: centre is not balanced ({l} vs {r})"
            );
            assert!(
                (db(l) - want_db).abs() < 0.05,
                "{law:?}: centre is {:.2} dB, expected {want_db:.2}",
                db(l),
            );
        }
    }

    /// Hard left is silence on the right, and the reverse, under every
    /// law — a pan that cannot reach one side is not a pan.
    #[test]
    fn every_law_reaches_both_extremes() {
        for law in [
            PanLaw::Linear,
            PanLaw::ConstantPower,
            PanLaw::Minus3dB,
            PanLaw::Minus4_5dB,
            PanLaw::Minus6dB,
        ] {
            let (l, r) = law.gains(-1.0);
            assert!(l > 0.99 && r < 1e-6, "{law:?}: hard left gave {l}/{r}");
            let (l, r) = law.gains(1.0);
            assert!(l < 1e-6 && r > 0.99, "{law:?}: hard right gave {l}/{r}");
        }
    }

    /// Out-of-range pan is clamped rather than producing a negative gain,
    /// which would invert the signal's polarity.
    #[test]
    fn pan_past_the_ends_is_clamped() {
        for law in [PanLaw::Linear, PanLaw::ConstantPower, PanLaw::Minus6dB] {
            for pan in [-4.0, 4.0, f32::INFINITY, f32::NEG_INFINITY] {
                let (l, r) = law.gains(pan);
                assert!(
                    l >= 0.0 && r >= 0.0,
                    "{law:?} at {pan}: negative gain {l}/{r}"
                );
                assert!(
                    l <= 1.0 && r <= 1.0,
                    "{law:?} at {pan}: gain above unity {l}/{r}"
                );
            }
        }
    }
}
