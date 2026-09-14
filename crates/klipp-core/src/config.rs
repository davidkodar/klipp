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
    /// FFmpeg's `components=` bitmask for the waveform filter on RGB
    /// input. Bit positions are R=1, G=2, B=4 (NOT plane-order — they
    /// follow component index regardless of the underlying gbrp plane
    /// storage). Returns at least 1 (R-only) so an all-channels-off
    /// state still produces a parseable filter spec — the user's "no
    /// channels" mental state is the right thing visually but the
    /// graph must still build.
    pub fn to_components(&self) -> u8 {
        let mut m = 0u8;
        if self.r { m |= 1; }
        if self.g { m |= 2; }
        if self.b { m |= 4; }
        if m == 0 { 1 } else { m }
    }
}

/// FFmpeg waveform `envelope=` choice. The envelope is an overlay drawn
/// on top of the waveform trace marking min/max excursions across the
/// frame — useful for spotting clipping (peak) and dropouts (instant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WaveformEnvelope {
    /// No envelope overlay. Default.
    #[default]
    None,
    /// Instant min/max for the current frame.
    Instant,
    /// Rolling peak — accumulates highest excursion across time.
    Peak,
    /// Both instant and rolling peak overlaid.
    PeakInstant,
}

impl WaveformEnvelope {
    /// FFmpeg `envelope=` numeric value.
    pub fn ffmpeg_value(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::Instant => 1,
            Self::Peak => 2,
            Self::PeakInstant => 3,
        }
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
    pub envelope: WaveformEnvelope,
}

impl Default for WaveformParadeSettings {
    fn default() -> Self {
        Self {
            filter: WaveformFilter::Lowpass,
            intensity_override: -1.0,
            channels: ChannelMask::default(),
            envelope: WaveformEnvelope::None,
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

/// FFmpeg vectorscope `graticule=` choice. Off by default because
/// Klipp's main-dock overlay draws its own ring + I-line + targets +
/// labels on top of the scope texture — having FFmpeg also paint
/// targets and letters duplicates the labels at slightly offset
/// positions. Available for diagnostic comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VectorscopeGraticule {
    /// No FFmpeg overlay — Klipp draws its own ring + I-line + targets.
    /// Default.
    #[default]
    None,
    /// FFmpeg paints target boxes + letters in green.
    Green,
    /// FFmpeg paints target boxes + letters in each target's hue.
    Color,
    /// FFmpeg paints targets inverted vs background.
    Invert,
}

impl VectorscopeGraticule {
    pub fn keyword(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Green => "green",
            Self::Color => "color",
            Self::Invert => "invert",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VectorscopeSettings {
    pub mode: VectorscopeMode,
    pub graticule: VectorscopeGraticule,
    /// Background opacity (FFmpeg `bgopacity=`). 0 = transparent
    /// background, 1 = fully opaque. Default 0.3.
    pub bgopacity: f32,
    /// Low luma threshold (FFmpeg `lthreshold=`). Pixels below this
    /// luma are dropped from the scatter — lets the user focus on
    /// chroma in the highlights. Default 0.0.
    pub lthreshold: f32,
    /// High luma threshold (FFmpeg `hthreshold=`). Pixels above this
    /// luma are dropped — focuses on shadow chroma. Default 1.0.
    pub hthreshold: f32,
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
            mode: VectorscopeMode::Color3,
            graticule: VectorscopeGraticule::None,
            bgopacity: 0.3,
            lthreshold: 0.0,
            hthreshold: 1.0,
            zoom: 1.0,
        }
    }
}

impl Eq for VectorscopeSettings {}

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

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HistogramSettings {
    pub display_mode: HistogramDisplayMode,
    pub levels: HistogramLevels,
    /// Foreground (bar) opacity (FFmpeg `fgopacity=`). Default 0.7.
    pub fgopacity: f32,
    /// Background opacity (FFmpeg `bgopacity=`). Default 0.5.
    pub bgopacity: f32,
}

impl Default for HistogramSettings {
    fn default() -> Self {
        Self {
            display_mode: HistogramDisplayMode::Parade,
            levels: HistogramLevels::Linear,
            fgopacity: 0.7,
            bgopacity: 0.5,
        }
    }
}

impl Eq for HistogramSettings {}

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
    /// When true, route waveform & parade through `format=yuv444p,...
    /// components=1` so they render a single Y (luma) trace in black &
    /// white instead of R/G/B in three colours. Pro broadcast luma
    /// scope view. Doesn't affect vectorscope or histogram.
    pub luma_only: bool,
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
        assert_eq!((l, r), (1.0, 1.0), "a centred stereo track is not attenuated");
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
            assert!((l - r).abs() < 1e-6, "{law:?}: centre is not balanced ({l} vs {r})");
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
                assert!(l >= 0.0 && r >= 0.0, "{law:?} at {pan}: negative gain {l}/{r}");
                assert!(l <= 1.0 && r <= 1.0, "{law:?} at {pan}: gain above unity {l}/{r}");
            }
        }
    }
}
