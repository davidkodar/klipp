use crate::id::{BinId, ClipId, GroupId, MarkerId, MediaId, TakeId, TrackId};
use crate::modifier::{Modifier, ParamId};
use crate::time::{Fps, RationalTime, Timecode};
use crate::types::Resolution;
use crate::TemplateValue;
use crate::keyframe::TitleTransformProperty;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Which audio parameter a keyframe operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioParam {
    Volume,
    Pan,
    Gain,
}

/// Which video parameter a keyframe operates on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VideoParam {
    Opacity,
    PositionX,
    PositionY,
    Scale,
    Rotation,
    /// Phase 4D — track-matte feather radius (px). Routed via the
    /// existing video-keyframe AppCommand machinery so the inspector
    /// can use the same `ScrubbyParam`/`AddVideoKeyframe`/etc. surface
    /// it uses for opacity. Stored on the consumer clip (one set of
    /// keyframes per clip; the clip's `matte_source.feather_px` field
    /// supplies the default when no keyframes are set).
    MatteFeather,
    /// Phase 4D — track-matte choke radius (px). Same routing as above.
    MatteChoke,
    /// Tilt in perspective about the picture's horizontal axis, degrees
    /// (`Clip::transform_tilt_x_keyframes`).
    TiltX,
    /// Tilt in perspective about its vertical axis, degrees
    /// (`Clip::transform_tilt_y_keyframes`).
    TiltY,
    /// The height's scale keyed apart from the width's
    /// (`Clip::transform_scale_y_keyframes`, #1471); with no keys the
    /// height follows `Scale` at the clip's aspect.
    ScaleY,
}

/// HDR transfer characteristic for export. Writes matching color-space tags
/// into the output container (BT.2020 primaries + the selected transfer).
/// #65 baseline — full mastering-display metadata deferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HdrMode {
    #[default]
    Sdr,
    /// Hybrid Log-Gamma (ARIB STD-B67).
    Hlg,
    /// Perceptual Quantizer / HDR10 (SMPTE ST 2084).
    Pq,
}

/// What the viewer shows while grading for an HDR delivery.
///
/// `Off` is the SDR picture: every scene-referred clip tone-mapped to the
/// display, which is all the viewer could ever show. The other two render
/// the HDR picture the export writes — highlights kept, in display light
/// where 1.0 is 203 cd/m² — and differ in how that reaches an SDR screen,
/// which cannot show it directly:
///
/// * `Preview` — as an SDR picture (`klipp_color::hdr_view`): the grade's
///   mid-tones where they are, the light above reference white rolled into
///   the top so its structure stays visible.
/// * `FalseColour` — the standard levels as flat colours, for reading
///   where the picture sits against 26, 203 and 1000 cd/m².
///
/// Either way the viewer also publishes the picture's light in cd/m², for
/// the readout under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum HdrMonitor {
    #[default]
    Off,
    Preview,
    FalseColour,
}

impl HdrMonitor {
    /// Whether the viewer renders the HDR picture.
    pub fn renders_hdr(self) -> bool {
        self != HdrMonitor::Off
    }
}

/// Display-boundary tone-map operator.
///
/// Picked by the user as a viewer / export "colour science" option.
/// Lives in `klipp-core` so `ExportSettings` can carry it without
/// pulling in `klipp-color`; the actual curves live in
/// `klipp_color::display` and match one-to-one on these variants.
/// The transfer the SDR viewer encodes for: the monitor it assumes.
///
/// A Rec.709 delivery is graded on a BT.1886 display (gamma 2.4); the
/// viewer encoded sRGB, whose piecewise curve differs from 2.4 near
/// black, so shadows in the viewer did not match a calibrated grading
/// monitor. Export is unaffected: it encodes for its delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum SdrTransfer {
    /// The sRGB piecewise curve: a desktop monitor.
    #[default]
    Srgb,
    /// BT.1886, gamma 2.4: a grading monitor.
    Bt1886,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ToneMapOp {
    /// Identity — no tone mapping. Values above 1.0 clip at display
    /// encode. Useful for SDR content that is already display-ready.
    None,
    /// Simple Reinhard soft-roll-off: `x / (1 + x)`. Neutral look.
    Reinhard,
    /// Uncharted 2 filmic curve. Cinematic default.
    #[default]
    Hable,
    /// Stephen Hill's fitted ACES RRT+ODT. Industry-standard filmic
    /// look — preserves hue through highlight roll-off via matrix
    /// pre/post-transform, at the cost of a per-pixel (rather than
    /// per-channel) pass.
    Aces,
}

/// Delivery-target Output Display Transform for the export pipeline.
///
/// Picks which display-encoded colour space the rendered file lands
/// in. Lives in `klipp-core` next to [`ExportSettings`] so the
/// runtime export type can carry it without pulling in `klipp-color`;
/// the actual ODT curves live in `klipp_color::aces` and the
/// dispatch in `klipp_color::display::display_transform_for_export`.
///
/// # When each variant is right
///
/// * [`Self::MatchViewer`] — default. Mirrors the viewer's display
///   path so the exported file matches what the user has been seeing
///   while editing. With a Rec.709 working space this is byte-
///   identical to every export produced before this setting existed
///   (sRGB encode of the linear master). With ACEScg it routes
///   through the Rec.709 SDR ODT (RRT + ODT + BT.1886 inverse) — same
///   pipeline the viewer runs.
/// * [`Self::Rec709Sdr`] — explicit Rec.709 SDR cinema (RRT + Rec.709
///   ODT). Picks the same curve as `MatchViewer` under ACEScg, but
///   forces it on regardless of the project's working space (a
///   Rec.709-linear project still gets the cinema look).
/// * [`Self::Rec2100Pq1000`] / [`Self::Rec2100Pq4000`] — HDR10 PQ
///   (SMPTE ST 2084) at 1000 / 4000 nit peak luminance, BT.2020
///   primaries. Pick the one that matches the calibrated peak of the
///   target display.
/// * [`Self::DciP3D65_48`] — DCI-P3 D65 cinema, 48 cd/m² peak. The
///   theatrical-projection target.
/// * [`Self::Rec2020Sdr`] — SDR video in BT.2020 primaries: the ACES
///   Rec.709 video transform onto the wider gamut, BT.1886, tagged and
///   matrixed BT.2020. For a broadcaster or platform that takes SDR in
///   BT.2020.
/// * [`Self::Srgb`] — legacy sRGB encode with no ACES RRT/ODT. The
///   pre-Phase-E export path, kept addressable for users who don't
///   want any opinionated tone mapping. Byte-identical to the export
///   pipeline as it shipped before delivery-target ODTs landed.
///
/// # Wire stability
///
/// `as_u8` / `from_u8` pin the wire tags so a future enum reorder
/// can't silently change the meaning of an already-stored value.
/// Unknown tags fall back to [`Self::MatchViewer`] so a corrupted or
/// future-version setting never produces a wider-gamut surprise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OdtChoice {
    /// Mirror the viewer's display path. Default — matches what the
    /// user has been seeing while editing.
    #[default]
    MatchViewer,
    /// Rec.709 SDR cinema — RRT + Rec.709 ODT + BT.1886 inverse.
    /// Explicit form of the viewer's path under an ACEScg project.
    Rec709Sdr,
    /// Rec.2100 PQ at 1000 cd/m² peak. HDR10 deliverable.
    Rec2100Pq1000,
    /// Rec.2100 PQ at 4000 cd/m² peak. HDR10 deliverable for high-
    /// nit reference monitors.
    Rec2100Pq4000,
    /// DCI-P3 D65 at 48 cd/m² peak. Theatrical cinema deliverable.
    DciP3D65_48,
    /// Rec.2020 SDR video — RRT + Rec.2020 100-nit dim ODT + BT.1886
    /// inverse (#901). Placed after `Srgb` in the wire tags.
    Rec2020Sdr,
    /// Legacy sRGB encode — no ACES RRT/ODT, just sRGB inverse EOTF
    /// on the linear master. The pre-delivery-target export path.
    Srgb,
    /// Forwards-compat sentinel: any future tag we don't recognise
    /// deserialises here. Treated by the dispatch as MatchViewer so
    /// older builds opening a project with a newer ODT stay safe.
    #[serde(other)]
    Unknown,
}

impl OdtChoice {
    /// Wire-stable u8 tag — used by FFI / atomic packers that need a
    /// fixed-width representation. Discriminants are pinned so a
    /// future enum reorder can't silently change the meaning of a
    /// stored value.
    #[inline]
    pub const fn as_u8(self) -> u8 {
        match self {
            OdtChoice::MatchViewer => 0,
            OdtChoice::Rec709Sdr => 1,
            OdtChoice::Rec2100Pq1000 => 2,
            OdtChoice::Rec2100Pq4000 => 3,
            OdtChoice::DciP3D65_48 => 4,
            OdtChoice::Srgb => 5,
            OdtChoice::Rec2020Sdr => 6,
            OdtChoice::Unknown => 255,
        }
    }

    /// Inverse of [`Self::as_u8`]. Unknown tags fall back to
    /// [`Self::MatchViewer`] — the safe default that mirrors viewer
    /// behaviour, so a corrupted atomic / FFI integer never produces
    /// a delivery-target surprise.
    #[inline]
    pub const fn from_u8(tag: u8) -> Self {
        match tag {
            1 => OdtChoice::Rec709Sdr,
            2 => OdtChoice::Rec2100Pq1000,
            3 => OdtChoice::Rec2100Pq4000,
            4 => OdtChoice::DciP3D65_48,
            5 => OdtChoice::Srgb,
            6 => OdtChoice::Rec2020Sdr,
            _ => OdtChoice::MatchViewer,
        }
    }

    /// Iteration order for menus: the SDR video outputs together, then
    /// HDR, cinema and the legacy curve. `Unknown` is excluded; it's a
    /// serde sentinel, not a user choice.
    pub const ALL: [OdtChoice; 7] = [
        OdtChoice::MatchViewer,
        OdtChoice::Rec709Sdr,
        OdtChoice::Rec2020Sdr,
        OdtChoice::Rec2100Pq1000,
        OdtChoice::Rec2100Pq4000,
        OdtChoice::DciP3D65_48,
        OdtChoice::Srgb,
    ];

    /// The ACES Rec.2100 PQ transforms: their output is PQ-encoded.
    pub const fn is_pq(self) -> bool {
        matches!(self, OdtChoice::Rec2100Pq1000 | OdtChoice::Rec2100Pq4000)
    }

    /// Whether the transform can serve a delivery with this transfer: the
    /// PQ transforms only a PQ one, the SDR transforms only SDR. Match
    /// Viewer serves every delivery.
    pub const fn fits(self, delivery: HdrMode) -> bool {
        match delivery {
            HdrMode::Sdr => !self.is_pq(),
            HdrMode::Pq => matches!(self, OdtChoice::MatchViewer | OdtChoice::Unknown) || self.is_pq(),
            HdrMode::Hlg => matches!(self, OdtChoice::MatchViewer | OdtChoice::Unknown),
        }
    }

    /// Whether the transform is worth offering for a project working in
    /// `ws`.
    ///
    /// Only [`OdtChoice::Srgb`] is ever ruled out, and for two different
    /// reasons — see `ExportDisplay::new`, which decides what it actually
    /// does:
    ///
    /// * In an **ACEScg** project it leaves AP1 values under an sRGB
    ///   label. That is what it did before delivery-target ODTs existed
    ///   and an old export may depend on it, so the pixels stay — but it
    ///   is not something to offer a colourist in a menu.
    /// * In a **BT.2020** project it converts, and so gives the same
    ///   picture Match Viewer does. Nothing wrong with it; there is just
    ///   no reason to list the same transform twice.
    ///
    /// Note this is about the gamut, not the tone curve: skipping the ACES
    /// RRT and skipping the primaries conversion are separate choices, and
    /// "sRGB" names a gamut as much as a curve.
    pub const fn suits_working_space(self, ws: crate::WorkingColorspace) -> bool {
        match self {
            OdtChoice::Srgb => matches!(ws, crate::WorkingColorspace::Rec709Linear),
            _ => true,
        }
    }

    /// Human-readable label for menus.
    pub const fn label(self) -> &'static str {
        match self {
            OdtChoice::MatchViewer => "Match Viewer",
            OdtChoice::Rec709Sdr => "Rec.709 SDR",
            OdtChoice::Rec2100Pq1000 => "Rec.2100 PQ (1000 nit)",
            OdtChoice::Rec2100Pq4000 => "Rec.2100 PQ (4000 nit)",
            OdtChoice::DciP3D65_48 => "DCI-P3 D65 (48 nit)",
            OdtChoice::Rec2020Sdr => "Rec.2020 SDR",
            OdtChoice::Srgb => "sRGB curve only (legacy)",
            OdtChoice::Unknown => "Unknown",
        }
    }
}

/// The true-peak ceiling a loudness-normalised delivery is held under
/// when none is given, in dBTP.
pub const DEFAULT_TRUE_PEAK_DBTP: f32 = -1.0;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSettings {
    pub output_path: PathBuf,
    pub format: ExportFormat,
    pub video_codec: ExportVideoCodec,
    pub resolution: Option<Resolution>,
    pub fps: Option<Fps>,
    pub crf: u32,
    pub preset: EncoderPreset,
    pub audio_codec: ExportAudioCodec,
    pub audio_bitrate_kbps: Option<u32>,
    pub video_bitrate_kbps: Option<u32>,
    pub rate_control: RateControl,
    /// Burn subtitle/text track content into the video. Default true to preserve
    /// existing behavior (subtitle clips have always been composited as overlays).
    pub burn_in_subtitles: bool,
    /// Embed subtitles as a soft track in the output container.
    /// Uses format-appropriate codec: mov_text (MP4/MOV), subrip (MKV), webvtt (WebM).
    pub embed_subtitles: bool,
    /// Target integrated loudness in LUFS for EBU R128 normalization.
    /// None = no normalization (pass audio through unchanged).
    pub target_lufs: Option<f32>,
    /// The true-peak ceiling a normalised delivery is held under, in
    /// dBTP. None is -1, what EBU R128 and the -14 streaming convention
    /// ask; ATSC A/85 and Netflix ask -2. Only read with `target_lufs`.
    #[serde(default)]
    pub true_peak_dbtp: Option<f32>,
    /// How a track's pan position becomes left and right gains.
    ///
    /// Here alongside `target_lufs` because it is the same kind of
    /// thing: a mix parameter that decides how loud the delivered file
    /// is. The export mixdown used to have one law written into it,
    /// ignoring this preference and coming out 3 dB below playback.
    pub pan_law: crate::PanLaw,
    /// Embed sequence markers as chapter metadata in the output container.
    /// Only honoured for MP4/MKV/MOV — WebM has no chapter concept.
    pub embed_chapters: bool,
    /// Carry the subtitle track as CEA-608 captions inside the video
    /// stream (#1663): ATSC A/53 caption data before each picture, which
    /// broadcast and some streaming deliveries read without a sidecar.
    /// H.264 and H.265 only.
    #[serde(default)]
    pub captions_608: bool,
    /// HDR10+ dynamic metadata (SMPTE ST 2094-40) per shot in an H.265 PQ
    /// delivery (#468), so a dimmer display tone-maps each shot by its
    /// own light.
    #[serde(default)]
    pub hdr10plus: bool,
    /// Requested encoder bit depth. `0` = codec default (8-bit); `10` enables
    /// 10-bit encode for H.264/H.265. Other codecs ignore this (ProRes is
    /// always 10-bit, DNxHD is 8-bit). #65 baseline.
    pub bit_depth: u8,
    /// Keep the frames' alpha in the file (#3264), where the codec holds
    /// one ([`ExportVideoCodec::holds_alpha`]): straight, not multiplied
    /// in. Off, a frame's transparent parts are written as they composite
    /// over black, as every export did.
    #[serde(default)]
    pub alpha: bool,
    /// HDR transfer characteristic. Defaults to SDR (BT.709). When 10-bit
    /// encode is active, HLG/PQ write BT.2020 primaries + matching transfer.
    pub hdr_mode: HdrMode,
    /// Display-boundary tone-map operator. Defaults to Hable (filmic)
    /// for backwards compatibility with exports produced before this
    /// setting existed. ACES is the alternative for users who want
    /// hue-preserving highlight roll-off.
    pub tone_map_op: ToneMapOp,
    /// Output Display Transform — picks the delivery-target colour
    /// space the rendered file lands in. Defaults to
    /// [`OdtChoice::MatchViewer`] so existing projects without the
    /// field deserialise to the same pixels they'd render in the
    /// viewer. `#[serde(default)]` keeps pre-S8 settings files
    /// loadable.
    #[serde(default)]
    pub odt: OdtChoice,
    /// Peak luminance the HDR master is graded for, cd/m² — the
    /// mastering display the highlights roll into and the figure written
    /// as the file's mastering-display max luminance. The user picks it
    /// for a PQ delivery; 1000, 2000 and 4000 are the mastering displays
    /// in use. Read it through [`ExportSettings::hdr_peak_nits`] rather
    /// than directly: an ACES PQ output transform and an HLG delivery
    /// each carry a peak of their own that overrides this. `serde`
    /// default keeps settings files written before the peak was
    /// choosable loading as the 1000 nits they were exported at.
    #[serde(default = "default_mastering_peak_nits")]
    pub mastering_peak_nits: f32,
}

/// The mastering peak an export has when nobody has chosen one: the
/// 1000-nit display HDR10 masters assume, and what every Klipp HDR
/// export was fixed at before the peak became a setting.
pub const DEFAULT_MASTERING_PEAK_NITS: f32 = 1000.0;

/// Mastering peaks offered in the export dialog, cd/m².
pub const MASTERING_PEAK_CHOICES: [f32; 3] = [1000.0, 2000.0, 4000.0];

/// Bounds a hand-edited or malformed peak is held inside. Below 100 nits
/// is not an HDR display at all; above 10000 is past what PQ can encode.
pub const MASTERING_PEAK_RANGE: std::ops::RangeInclusive<f32> = 100.0..=10000.0;

fn default_mastering_peak_nits() -> f32 {
    DEFAULT_MASTERING_PEAK_NITS
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            output_path: PathBuf::new(),
            format: ExportFormat::Mp4,
            video_codec: ExportVideoCodec::H264,
            resolution: None,
            fps: None,
            crf: 23,
            preset: EncoderPreset::default(),
            audio_codec: ExportAudioCodec::Aac,
            pan_law: crate::PanLaw::default(),
            audio_bitrate_kbps: Some(192),
            video_bitrate_kbps: None,
            rate_control: RateControl::default(),
            burn_in_subtitles: true,
            embed_subtitles: false,
            target_lufs: None,
            true_peak_dbtp: None,
            embed_chapters: false,
            captions_608: false,
            hdr10plus: false,
            bit_depth: 0,
            alpha: false,
            hdr_mode: HdrMode::default(),
            tone_map_op: ToneMapOp::default(),
            odt: OdtChoice::default(),
            mastering_peak_nits: DEFAULT_MASTERING_PEAK_NITS,
        }
    }
}

impl ExportSettings {
    /// Replace a video or audio codec the container cannot hold with the
    /// container's default, keeping the codecs that fit.
    pub fn fit_codecs_to_format(&mut self) {
        (self.video_codec, self.audio_codec) = self.format.fit(self.video_codec, self.audio_codec);
    }

    /// Why this container cannot be written with these codecs, or `None`
    /// when it can. Checked before an encode starts, so a bad pair fails
    /// with this sentence instead of FFmpeg's "Invalid argument".
    /// Whether the file carries the frames' alpha: asked for, and the
    /// codec holds one.
    pub fn writes_alpha(&self) -> bool {
        self.alpha && self.video_codec.holds_alpha()
    }

    pub fn container_problem(&self) -> Option<String> {
        if !self.format.holds_video(self.video_codec) {
            return Some(format!(
                "{} cannot hold {} video; choose {} or another container",
                self.format.label(),
                self.video_codec.label(),
                self.format.default_video().label(),
            ));
        }
        if !self.format.holds_audio(self.audio_codec) {
            return Some(format!(
                "{} cannot hold {} audio; choose {} or another container",
                self.format.label(),
                self.audio_codec.label(),
                self.format.default_audio().label(),
            ));
        }
        None
    }

    /// The transfer the file carries. An ACES Rec.2100 PQ output
    /// transform writes PQ pixels whatever `hdr_mode` says, so it is a PQ
    /// delivery and is tagged as one.
    pub fn delivery(&self) -> HdrMode {
        if self.odt.is_pq() { HdrMode::Pq } else { self.hdr_mode }
    }

    /// The size the picture is encoded at, from a `seq_w`×`seq_h`
    /// sequence: the resolution asked for, else the sequence's, each side
    /// rounded down to even. 4:2:0 video, which every codec here writes,
    /// halves the colour planes, and ffmpeg's picture code aborts the
    /// process on an odd side; the export job rounded and the encoder did
    /// not, so an odd size asked for took the app down (hunt #2).
    pub fn output_size(&self, seq_w: u32, seq_h: u32) -> (u32, u32) {
        let (w, h) = self.resolution.map_or((seq_w, seq_h), |r| (r.width, r.height));
        let side = |v: u32| (v.min(Self::MAX_SIDE) & !1).max(2);
        (side(w), side(h))
    }

    /// What to say when the export's frame is another shape than the
    /// timeline's (#2784): the picture is fitted whole, with black bars
    /// above and below or at the sides, and a version made in that shape
    /// fills it. `None` when the shapes match (within 1.5%).
    pub fn shape_warning(&self, seq_w: u32, seq_h: u32) -> Option<String> {
        let (w, h) = self.output_size(seq_w, seq_h);
        let (out, timeline) = (w as f64 / h.max(1) as f64, seq_w as f64 / seq_h.max(1) as f64);
        if seq_w == 0 || seq_h == 0 || (out / timeline - 1.0).abs() <= 0.015 {
            return None;
        }
        let bars = if out < timeline { "above and below" } else { "at the sides" };
        Some(format!(
            "The timeline is {seq_w}x{seq_h} and this export {w}x{h}: the picture is fitted whole, with black bars {bars}. To fill the frame, make a version in that shape first (the format op with width {w} and height {h}, refit following the subjects) and export it."
        ))
    }

    /// The largest side an export is encoded at (#658): 16K, past every
    /// delivery format, where 99999×99999 asked for some 40 GB a frame.
    pub const MAX_SIDE: u32 = 16384;

    /// Whether the picture is rendered for HDR — scene-referred clips keep
    /// their highlights and the encoder writes display light as PQ or HLG
    /// — rather than being an SDR master through an output transform. True
    /// for an HLG or PQ delivery, except through an ACES PQ output
    /// transform, which renders its own HDR from the SDR master.
    pub fn renders_hdr(&self) -> bool {
        self.delivery() != HdrMode::Sdr && !self.odt.is_pq()
    }

    /// Peak luminance of the HDR master, cd/m². An ACES Rec.2100 output
    /// transform states its own peak and an HLG delivery is defined on a
    /// 1000-nit nominal display (BT.2100); a PQ delivery is mastered to
    /// the peak the user picked, held inside
    /// [`MASTERING_PEAK_RANGE`] in case a settings file carries
    /// something else.
    pub fn hdr_peak_nits(&self) -> f32 {
        match self.odt {
            OdtChoice::Rec2100Pq1000 => 1000.0,
            OdtChoice::Rec2100Pq4000 => 4000.0,
            _ if self.delivery() == HdrMode::Hlg => DEFAULT_MASTERING_PEAK_NITS,
            // `clamp` would pass a NaN straight through, and a NaN peak
            // divides the whole picture into NaN downstream.
            _ if !self.mastering_peak_nits.is_finite() => DEFAULT_MASTERING_PEAK_NITS,
            _ => self
                .mastering_peak_nits
                .clamp(*MASTERING_PEAK_RANGE.start(), *MASTERING_PEAK_RANGE.end()),
        }
    }
}

/// A single chapter entry frozen at export time. Timebase is milliseconds
/// (start_ms/end_ms). `title` is the marker label; empty is allowed and the
/// player displays a default label.
#[derive(Debug, Clone)]
pub struct ChapterSnapshot {
    pub start_ms: u64,
    pub end_ms: u64,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum EncoderPreset {
    Fast,
    #[default]
    Medium,
    Slow,
    VerySlow,
}

impl EncoderPreset {
    pub fn ffmpeg_value(&self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Medium => "medium",
            Self::Slow => "slow",
            Self::VerySlow => "veryslow",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Medium => "Medium",
            Self::Slow => "Slow",
            Self::VerySlow => "Very Slow",
        }
    }

    pub fn all() -> &'static [EncoderPreset] {
        &[Self::Fast, Self::Medium, Self::Slow, Self::VerySlow]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportFormat {
    Mp4,
    Mkv,
    Mov,
    WebM,
    /// Animated GIF (CPU-only, two-pass palette via palettegen +
    /// paletteuse). No audio, no subtitles, no chapters, no HDR.
    Gif,
    /// Sound alone, no picture (#1813): MP3, WAV or AAC in M4A.
    Mp3,
    Wav,
    M4a,
}

impl ExportFormat {
    /// Whether the file is sound alone, with no video stream (#1813).
    pub fn is_audio_only(&self) -> bool {
        matches!(self, Self::Mp3 | Self::Wav | Self::M4a)
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Mov => "mov",
            Self::WebM => "webm",
            Self::Gif => "gif",
            Self::Mp3 => "mp3",
            Self::Wav => "wav",
            Self::M4a => "m4a",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Mkv => "MKV",
            Self::Mov => "MOV",
            Self::WebM => "WebM",
            Self::Gif => "GIF",
            Self::Mp3 => "MP3 (audio only)",
            Self::Wav => "WAV (audio only)",
            Self::M4a => "M4A (audio only)",
        }
    }

    /// Whether this format can carry audio. GIF cannot.
    pub fn supports_audio(&self) -> bool {
        !matches!(self, Self::Gif)
    }

    /// Whether this format supports embedded chapter atoms.
    /// MP4/MKV/MOV do; WebM and GIF do not.
    pub fn supports_chapters(&self) -> bool {
        matches!(self, Self::Mp4 | Self::Mkv | Self::Mov)
    }

    /// Whether this format supports embedded subtitle streams.
    pub fn supports_subtitles(&self) -> bool {
        !matches!(self, Self::Gif) && !self.is_audio_only()
    }

    /// Whether FFmpeg's muxer for this container takes `codec`: MP4
    /// refuses ProRes and DNxHD, MOV refuses AV1 and VP9, WebM takes only
    /// VP9 and AV1, and the GIF codec goes only in a GIF file.
    pub fn holds_video(&self, codec: ExportVideoCodec) -> bool {
        use ExportVideoCodec as V;
        match self {
            Self::Mp4 => matches!(codec, V::H264 | V::H265 | V::Av1 | V::Vp9),
            // FFV1 is Matroska's: `codec != V::Gif` below takes it.
            Self::Mkv => codec != V::Gif,
            Self::Mov => !matches!(codec, V::Av1 | V::Vp9 | V::Gif | V::Ffv1),
            Self::WebM => matches!(codec, V::Vp9 | V::Av1),
            Self::Gif => codec == V::Gif,
            // No picture is written, so the video codec is not used.
            Self::Mp3 | Self::Wav | Self::M4a => true,
        }
    }

    /// Whether FFmpeg's muxer for this container takes `codec`. MOV
    /// refuses Opus and FLAC ("only supported in MP4") and WebM takes
    /// only Opus. No audio fits anywhere, and GIF writes no audio at all.
    pub fn holds_audio(&self, codec: ExportAudioCodec) -> bool {
        use ExportAudioCodec as A;
        match self {
            Self::Mp4 | Self::Mkv | Self::Gif => true,
            Self::Mov => !matches!(codec, A::Opus | A::Flac),
            Self::WebM => matches!(codec, A::Opus | A::Copy),
            Self::Mp3 => codec == A::Mp3,
            Self::Wav => matches!(codec, A::Pcm16 | A::Pcm24),
            Self::M4a => codec == A::Aac,
        }
    }

    /// The video codec to switch to when the current one does not fit.
    pub fn default_video(&self) -> ExportVideoCodec {
        match self {
            Self::Mp4 | Self::Mkv | Self::Mov => ExportVideoCodec::H264,
            Self::WebM => ExportVideoCodec::Vp9,
            Self::Gif => ExportVideoCodec::Gif,
            Self::Mp3 | Self::Wav | Self::M4a => ExportVideoCodec::H264,
        }
    }

    /// The audio codec to switch to when the current one does not fit.
    pub fn default_audio(&self) -> ExportAudioCodec {
        match self {
            Self::WebM => ExportAudioCodec::Opus,
            Self::Mp3 => ExportAudioCodec::Mp3,
            Self::Wav => ExportAudioCodec::Pcm16,
            _ => ExportAudioCodec::Aac,
        }
    }

    /// `video` and `audio` with any codec this container cannot hold
    /// replaced by its default, the ones that fit kept.
    pub fn fit(&self, video: ExportVideoCodec, audio: ExportAudioCodec) -> (ExportVideoCodec, ExportAudioCodec) {
        (
            if self.holds_video(video) { video } else { self.default_video() },
            if self.holds_audio(audio) { audio } else { self.default_audio() },
        )
    }
}

/// A codec/delivery pairing that will encode but should not be chosen,
/// with the codec to offer instead.
///
/// Separate from [`ExportVideoCodec::hint`] because that describes a
/// codec on its own, and the thing worth saying here only exists for a
/// *pair*: H.264 is the right answer for an SDR web file and the wrong
/// one for the same delivery in HDR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HdrAdvice {
    /// One line, short enough to sit in the export dialog.
    pub headline: &'static str,
    /// The reasoning, for a tooltip.
    pub detail: &'static str,
    /// The codec to switch to, when there is one worth offering.
    pub suggest: Option<ExportVideoCodec>,
}

/// The file a track is bounced to (#3264).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BounceFormat {
    /// ProRes 4444 with alpha in a .mov: plays in other editors.
    #[default]
    ProRes4444,
    /// FFV1, lossless 16-bit RGB with alpha, in a .mkv: larger, nothing lost.
    Ffv1,
}

impl BounceFormat {
    pub fn codec(&self) -> ExportVideoCodec {
        match self {
            Self::ProRes4444 => ExportVideoCodec::ProRes4444,
            Self::Ffv1 => ExportVideoCodec::Ffv1,
        }
    }

    pub fn container(&self) -> ExportFormat {
        match self {
            Self::ProRes4444 => ExportFormat::Mov,
            Self::Ffv1 => ExportFormat::Mkv,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::ProRes4444 => "ProRes 4444",
            Self::Ffv1 => "FFV1 (lossless)",
        }
    }

    /// The word the socket takes: `prores4444` or `ffv1`.
    pub fn from_word(word: &str) -> Option<Self> {
        match word.trim().to_ascii_lowercase().as_str() {
            "prores4444" | "prores" => Some(Self::ProRes4444),
            "ffv1" => Some(Self::Ffv1),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportVideoCodec {
    H264,
    H265,
    ProRes422,
    ProRes422HQ,
    ProRes4444,
    DNxHD115,
    DNxHD145,
    Av1,
    Vp9,
    /// Animated GIF — two-pass palette workflow inside the encoder.
    /// CPU-only, no audio/subtitles, no HDR.
    Gif,
    /// FFV1, lossless, 16-bit RGB (#3264): for a round trip back into an
    /// edit with nothing lost. Matroska only.
    Ffv1,
}

impl ExportVideoCodec {
    pub fn ffmpeg_codec(&self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
            Self::ProRes422 | Self::ProRes422HQ | Self::ProRes4444 => "prores_ks",
            Self::DNxHD115 | Self::DNxHD145 => "dnxhd",
            Self::Av1 => "libsvtav1",
            Self::Vp9 => "libvpx-vp9",
            Self::Gif => "gif",
            Self::Ffv1 => "ffv1",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265",
            Self::ProRes422 => "ProRes 422",
            Self::ProRes422HQ => "ProRes 422 HQ",
            Self::ProRes4444 => "ProRes 4444",
            Self::DNxHD115 => "DNxHD 115 Mbps",
            Self::DNxHD145 => "DNxHD 145 Mbps",
            Self::Av1 => "AV1",
            Self::Vp9 => "VP9",
            Self::Gif => "GIF (animated)",
            Self::Ffv1 => "FFV1 (lossless)",
        }
    }

    pub fn hint(&self) -> &'static str {
        match self {
            Self::H264 => "GPU accelerated \u{26A1} — fast, universal",
            Self::H265 => "GPU accelerated \u{26A1} — best quality/size",
            Self::ProRes422 => "CPU only — NLE interchange, large files",
            Self::ProRes422HQ => "CPU only — high quality NLE interchange",
            Self::ProRes4444 => "CPU only — alpha channel, mastering",
            Self::DNxHD115 => "CPU only — Avid interchange",
            Self::DNxHD145 => "CPU only — Avid interchange, high bitrate",
            Self::Av1 => "CPU — next-gen, best compression, slow encode",
            Self::Vp9 => "CPU — web video (YouTube, WebM)",
            Self::Gif => "CPU only — animated GIF, 256-colour palette",
            Self::Ffv1 => "CPU only — lossless, very large files, Matroska",
        }
    }

    /// Whether this codec at `bit_depth` writes 8-bit video that a smooth
    /// gradient can band in (#2074): H.264, H.265 and AV1 unless 10-bit
    /// is chosen, VP9 and DNxHD always. ProRes is 10-bit; a GIF dithers
    /// to its own palette, so the question does not arise there.
    pub fn writes_8bit(&self, bit_depth: u8) -> bool {
        match self {
            Self::H264 | Self::H265 | Self::Av1 => bit_depth != 10,
            Self::Vp9 | Self::DNxHD115 | Self::DNxHD145 => true,
            Self::ProRes422 | Self::ProRes422HQ | Self::ProRes4444 | Self::Gif | Self::Ffv1 => false,
        }
    }

    /// [`hint`](Self::hint) for the depth actually chosen.
    ///
    /// H.264's hint is a promise it cannot keep at ten bits: "GPU
    /// accelerated — fast, universal" is wrong on all three counts for
    /// High 10, which no NVENC encodes and little hardware decodes. It
    /// stays wrong for 10-bit SDR, so this turns on the depth rather
    /// than on the delivery.
    pub fn hint_at_depth(&self, bit_depth: u8) -> &'static str {
        match (self, bit_depth) {
            (Self::H264, 10) => {
                "CPU only at 10-bit — High 10 profile, limited playback support"
            }
            _ => self.hint(),
        }
    }

    /// What to say about delivering `mode` with this codec, and what to
    /// offer instead. `None` when the pairing is a good fit.
    ///
    /// The one case that matters is H.264, and it is worse than it looks.
    /// A 10-bit H.264 HDR file out of Klipp is *correct* — full BT.2020
    /// tags, ST 2084 transfer, mastering display luminance, MaxCLL and
    /// MaxFALL all present — but it is High 10, a profile consumer
    /// hardware skipped. On this machine NVENC will not encode it and
    /// NVDEC will not decode it ("Failed setup for format cuda"), so it
    /// is software-only to make *and* software-only to play, while
    /// HDR10 as a delivery format is specified on HEVC Main 10. The
    /// codec's own [`hint`](Self::hint) — "fast, universal" — is wrong
    /// on both counts once the depth goes to ten.
    pub fn hdr_advice(&self, mode: HdrMode) -> Option<HdrAdvice> {
        if mode == HdrMode::Sdr {
            return None;
        }
        match self {
            Self::H264 => Some(HdrAdvice {
                headline: "H.264 is a poor fit for HDR",
                detail:
                    "10-bit H.264 is High 10 profile: no GPU encodes it and most \
                     players and TVs cannot decode it, so the file is slow to write \
                     and may not play as HDR. H.265 delivers the same HLG or PQ \
                     picture, encodes on the GPU, and is what HDR10 is specified on.",
                suggest: Some(Self::H265),
            }),
            // The standard HDR deliveries, and ProRes the standard HDR
            // mastering format. Nothing to warn about.
            Self::H265 | Self::Av1 | Self::ProRes422 | Self::ProRes422HQ | Self::ProRes4444 | Self::Ffv1 => None,
            // Not offered an HDR delivery by the export dialog today —
            // DNxHD 115/145 are 8-bit, GIF is palettised. Answered
            // anyway so the advice does not depend on the UI's gating.
            Self::DNxHD115 | Self::DNxHD145 => Some(HdrAdvice {
                headline: "DNxHD cannot carry HDR",
                detail: "These DNxHD bitrates are 8-bit 4:2:2, which has nowhere to put \
                         an HDR signal. ProRes 422 HQ is the 10-bit equivalent.",
                suggest: Some(Self::ProRes422HQ),
            }),
            Self::Vp9 => Some(HdrAdvice {
                headline: "VP9 HDR has thin support",
                detail: "VP9 Profile 2 can carry 10-bit HDR, but player support is \
                         patchy. AV1 is the web HDR codec that succeeded it.",
                suggest: Some(Self::Av1),
            }),
            Self::Gif => Some(HdrAdvice {
                headline: "GIF cannot carry HDR",
                detail: "A 256-colour palette has no HDR representation.",
                suggest: None,
            }),
        }
    }

    pub fn uses_crf(&self) -> bool {
        matches!(self, Self::H264 | Self::H265 | Self::Av1 | Self::Vp9)
    }

    pub fn supports_preset(&self) -> bool {
        matches!(self, Self::H264 | Self::H265 | Self::Av1)
    }


    /// The container a codec is moved to when the chosen one cannot hold
    /// it.
    pub fn home_format(&self) -> ExportFormat {
        match self {
            Self::H264 | Self::H265 | Self::Av1 => ExportFormat::Mp4,
            Self::ProRes422 | Self::ProRes422HQ | Self::ProRes4444 | Self::DNxHD115 | Self::DNxHD145 => ExportFormat::Mov,
            Self::Vp9 => ExportFormat::WebM,
            Self::Gif => ExportFormat::Gif,
            Self::Ffv1 => ExportFormat::Mkv,
        }
    }

    /// Whether the codec can carry an alpha channel: ProRes 4444 and FFV1
    /// (#3264).
    pub fn holds_alpha(&self) -> bool {
        matches!(self, Self::ProRes4444 | Self::Ffv1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportAudioCodec {
    Aac,
    Mp3,
    Flac,
    Opus,
    Pcm16,
    Pcm24,
    /// No audio stream at all. The name is kept for saved presets; there is
    /// nothing to copy from, since the audio is a mixdown made in-process.
    Copy,
}

impl ExportAudioCodec {
    pub fn ffmpeg_args(&self, bitrate_kbps: Option<u32>) -> Vec<String> {
        match self {
            Self::Aac => {
                let mut args = vec!["-c:a".into(), "aac".into()];
                if let Some(br) = bitrate_kbps {
                    args.push("-b:a".into());
                    args.push(format!("{br}k"));
                }
                args
            }
            Self::Mp3 => {
                let mut args = vec!["-c:a".into(), "libmp3lame".into()];
                if let Some(br) = bitrate_kbps {
                    args.push("-b:a".into());
                    args.push(format!("{br}k"));
                }
                args
            }
            Self::Opus => {
                let mut args = vec!["-c:a".into(), "libopus".into()];
                if let Some(br) = bitrate_kbps {
                    args.push("-b:a".into());
                    args.push(format!("{br}k"));
                }
                args
            }
            Self::Flac => vec!["-c:a".into(), "flac".into()],
            Self::Pcm16 => vec!["-c:a".into(), "pcm_s16le".into()],
            Self::Pcm24 => vec!["-c:a".into(), "pcm_s24le".into()],
            Self::Copy => vec!["-c:a".into(), "copy".into()],
        }
    }

    pub fn ffmpeg_codec(&self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Mp3 => "libmp3lame",
            Self::Flac => "flac",
            Self::Opus => "libopus",
            Self::Pcm16 => "pcm_s16le",
            Self::Pcm24 => "pcm_s24le",
            Self::Copy => "pcm_s16le",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Aac => "AAC",
            Self::Mp3 => "MP3",
            Self::Flac => "FLAC (lossless)",
            Self::Opus => "Opus",
            Self::Pcm16 => "PCM 16-bit (WAV)",
            Self::Pcm24 => "PCM 24-bit (WAV)",
            Self::Copy => "No audio",
        }
    }

    /// Whether this codec supports variable bitrate.
    pub fn uses_bitrate(&self) -> bool {
        matches!(self, Self::Aac | Self::Mp3 | Self::Opus)
    }
}

/// Video rate control mode for export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum RateControl {
    /// Constant Rate Factor — quality-based (default).
    #[default]
    Crf,
    /// Constant Bitrate.
    Cbr,
    /// Variable Bitrate (target average).
    Vbr,
}

impl RateControl {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Crf => "CRF (Quality)",
            Self::Cbr => "CBR (Constant Bitrate)",
            Self::Vbr => "VBR (Variable Bitrate)",
        }
    }
}

/// Identifies which AI Tools task a palette/keybinding action targets.
/// Lives in `klipp-core` because [`AppCommand`] does — `klipp-core` is
/// not allowed to depend on `klipp-ml`, so we mirror the task discriminator
/// here as a plain enum and let the consumer (`klipp-ui` / `klipp-ml`)
/// resolve it back to the manifest task type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiToolsTaskTag {
    /// Background removal (alpha matte). Maps to SAM2 / RMBG-class
    /// models in the manifest.
    RemoveBackground,
    /// Mask the user-selected object using a click/box prompt.
    IsolateObject,
    /// Run a one-shot detector and surface bounding boxes / labels.
    DetectObjects,
    /// Track a chosen object across the clip's frame range.
    TrackObject,
}

/// What the app asks of an audition (#3209), as the `alternatives` op
/// does it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AuditionEdit {
    /// The clip made an audition: itself and a copy of it to vary.
    Create,
    /// A copy of the take in the slot added, to vary.
    AddCopy,
    /// Each file added as a take, from its start, as long as the slot;
    /// the clip made an audition first when it is not one.
    AddMedia { media_ids: Vec<MediaId> },
    /// The take put in the slot; `ripple` moves what follows off the
    /// primary storyline too.
    Switch { take: TakeId, ripple: bool },
    Rename { take: TakeId, name: String },
    Remove { take: TakeId },
    /// The take in the slot kept as a plain clip, the others dropped.
    Finalize,
}

/// Where a freshly added clip should land relative to the timeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipDrop {
    /// Media pool only — leave the timeline untouched.
    PoolOnly,
    /// Drop at the playhead, on a fresh track below the stack.
    AtPlayhead,
    /// Append at the end of the existing V1/A1 stack (the double-click default).
    EndOfTimeline,
    /// Append at the end of one track: a file dropped from the Files
    /// browser onto that track's row.
    AppendToTrack { track_index: usize },
    /// Into one track at `at`: a pool item dropped on that track's row
    /// (#2549). Inserted there, moving what follows along, or laid over
    /// what is there with `overwrite`.
    OnTrackAt { track_index: usize, at: Timecode, overwrite: bool },
}


/// Which of the sequence camera's values a key belongs to.
///
/// The camera keeps one keyframe list per value rather than one list of
/// poses, so a key is named by the list it goes in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CameraParam {
    /// Where it looks, across: 0.5 is the middle of the scene.
    X,
    /// Where it looks, down.
    Y,
    /// How far in, 1 being the scene as composed.
    Zoom,
    /// Its roll, clockwise in degrees.
    Roll,
}

impl CameraParam {
    pub fn label(&self) -> &'static str {
        match self {
            Self::X => "X",
            Self::Y => "Y",
            Self::Zoom => "Zoom",
            Self::Roll => "Roll",
        }
    }

    /// What the value is when nothing has been keyed.
    pub fn resting(&self) -> f32 {
        match self {
            Self::X | Self::Y => 0.5,
            Self::Zoom => 1.0,
            Self::Roll => 0.0,
        }
    }
}

/// Which clips of a track a selection command takes, by the playhead
/// (#844).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectRange {
    /// Every clip.
    All,
    /// The clips starting at or after the playhead.
    Forward,
    /// The clips ending at or before the playhead.
    Backward,
}

/// A value of a title's published field (#1139): what a template keeps
/// editable once applied.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TitleFieldValue {
    /// A text layer's text.
    Text(String),
    /// A colour, sRGB with alpha.
    Colour([u8; 4]),
    /// How fast the title's motion runs against the template's: 1.0 as
    /// made, 2.0 twice as fast.
    Speed(f32),
    /// How big the title is against the template's (#1724): 1.0 as made,
    /// 2.0 twice the size, grown from the template's anchor.
    Size(f32),
    /// A text's font family (#2757).
    Font(String),
    /// How long the title's motion is laid out over, in seconds (#2757).
    Duration(f32),
    /// An image layer's file (#2757).
    Picture(String),
}

/// A viewing LUT for the scopes to measure the picture through (#466):
/// `key` changes with the table; `table` is r,g,b per grid point, red
/// fastest, `size` points a side.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopeLut {
    pub key: u64,
    pub size: u32,
    pub table: std::sync::Arc<Vec<f32>>,
}

/// The project's colour set-up, whole (#1875): what Colour Management
/// sets, as one value, so a change to it is one undo step. The output
/// LUT is the project's global LUT: its file and whether it is on. Its
/// opacity is a slider of its own and not part of this, or every drag of
/// it would be taken back with the next colour change undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectColour {
    pub working_colorspace: crate::WorkingColorspace,
    pub grade_encoding: crate::GradeEncoding,
    pub default_odt: OdtChoice,
    pub default_input_format: Option<crate::types::LogFormat>,
    pub input_lut: Option<PathBuf>,
    pub viewing_lut: Option<PathBuf>,
    pub scopes_after_viewing_lut: bool,
    pub output_lut: Option<PathBuf>,
    pub output_lut_enabled: bool,
}

/// A browser's entry by its name, as `frame_at` previews one (#3210): an
/// effect or look, a transition, a title template or a generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreviewEntry {
    Effect(String),
    Transition(String),
    Title(String),
    Generator(String),
}

#[derive(Debug, Clone)]
pub enum AppCommand {
    /// A batch of graph ops on one picture, applied as one undo step named
    /// `label` or not at all (node-graph M2 step 15d): what the control
    /// socket's `graph_ops` and the assistant send. A refusal names the op
    /// that couldn't be applied.
    GraphOps { ops: Vec<klipp_graph_format::op::GraphOp>, label: String },
    // Path-addressed edits (M3 step 2): a node named by its root and the
    // ids down to it (`NodePath`) and a parameter by its key, never by a
    // place in a list. Each is one undo step. They sit beside the index and
    // bare-id commands, which stay until the inspector switches over.
    /// Put `node` after the node at `after`, in that node's level.
    AddNodeAt { after: klipp_graph_format::path::NodePath, node: Box<klipp_graph_format::Node> },
    RemoveNodeAt { addr: klipp_graph_format::path::NodePath },
    /// Move the node at `addr` to just after `after`, a node of its level.
    MoveNodeAt { addr: klipp_graph_format::path::NodePath, after: klipp_graph_format::NodeId },
    SetNodeEnabledAt { addr: klipp_graph_format::path::NodePath, enabled: bool },
    SetParamAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey, value: klipp_graph_format::ParamValue },
    /// A node's stored entry written whole: an effect's settings as
    /// `UpdateBuiltinEffect` writes them, or a grade node's entry with one
    /// struct changed (a qualifier, curves, masks), as `SetNodeQualifier`
    /// and its kin write it. Scalar parameters go by `SetParamAt`.
    SetPayloadAt { addr: klipp_graph_format::path::NodePath, payload: serde_json::Value },
    /// How much of an effect shows over its input; `None` is all of it.
    SetMixAt { addr: klipp_graph_format::path::NodePath, mix: Option<f32> },
    /// Feed the mask port of the effect at `addr` from `from`'s `from_port`.
    ConnectMaskAt { addr: klipp_graph_format::path::NodePath, from: klipp_graph_format::path::NodePath, from_port: klipp_graph_format::PortKey },
    SetMaskChannelAt { addr: klipp_graph_format::path::NodePath, channel: Option<String> },
    /// Open a drag of `(addr, key)`: the `SetParamAt`s on it (and its key
    /// edits) until its `EndParamEditAt` are one undo step. An empty `key`
    /// opens a drag over the node's parameters together (a corner pin's
    /// handle moves two at once).
    BeginParamEditAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey },
    EndParamEditAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey },
    /// Take the open drag of `(addr, key)` back without recording it: the
    /// clip's picture returns to what it was when the drag opened, values
    /// and keys alike. For a gesture that cannot close as an edit (Escape,
    /// the pointer leaving the window, its release never seen).
    AbandonParamEditAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey },
    /// Add a key to `(addr, key)`: `keyframe` as the node's animation
    /// holds one.
    AddKeyAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey, keyframe: serde_json::Value },
    RemoveKeyAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey, time_secs: f64 },
    /// Move the key at `time_secs` to `new_time_secs`, with `value`.
    MoveKeyAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey, time_secs: f64, new_time_secs: f64, value: f32 },
    SetInterpKeyAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey, time_secs: f64, interpolation: crate::keyframe::Interpolation },
    /// Ease the key of `(addr, key)` at `time_secs` on a side
    /// ([`crate::keyframe::ease_key`]): one undo step.
    EaseKeyAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey, time_secs: f64, ease: crate::keyframe::KeyEase },
    /// Copy the keys of `(addr, key)` to the keyframe clipboard, with the
    /// length of the clip they are on, as a video track's copy does.
    CopyKeysAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey },
    /// Put the clipboard's keys on `(addr, key)` in place of its own,
    /// scaled from the clip they came from to this one: one undo step.
    PasteKeysAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey },
    /// Mirror the keys of `(addr, key)` in time over their own span: one
    /// undo step.
    ReverseKeysAt { addr: klipp_graph_format::path::NodePath, key: klipp_graph_format::ParamKey },
    /// Split-compare the effect node at `path`: the viewer's left half
    /// shows the clip with it bypassed, the right half with it applied.
    /// Mutually exclusive with the other compare modes; `None` clears it on
    /// both sides (M3 step 4: by node, so a reorder doesn't move it).
    SetSplitCompareNode { clip_id: Option<ClipId>, path: Option<klipp_graph_format::path::NodePath> },
    /// Show the graph dock at the level holding the node at `path`, and
    /// select the node when `select`. Idempotent: it never closes the dock.
    RevealGraph { path: klipp_graph_format::path::NodePath, select: bool },
    /// Open or close the graph without changing workspaces.
    ToggleGraphDock,
    /// Show `root`'s top level in the graph dock.
    SetGraphDockRoot { root: klipp_graph_format::path::Root },
    /// Move one or several canvas nodes as a single undoable gesture.
    BeginNodeLayoutEdit { root: klipp_graph_format::path::Root },
    EndNodeLayoutEdit { root: klipp_graph_format::path::Root },
    AbandonNodeLayoutEdit { root: klipp_graph_format::path::Root },
    SetNodePositionAt { addr: klipp_graph_format::path::NodePath, position: (f32, f32) },
    /// The frames and sticky notes on the node editor's canvas at the
    /// level of the node `at` names, set whole, as one step named `label`
    /// (#1743); a note's move inside a layout gesture is the gesture's.
    SetAnnotationsAt { at: klipp_graph_format::path::NodePath, frames: Vec<serde_json::Value>, sticky_notes: Vec<serde_json::Value>, label: &'static str },
    // Transport
    Play,
    Pause,
    Stop,
    SeekTo(Timecode),
    /// Move the playhead by this many *frames* (±1 steps one frame). Not
    /// timecode ticks: at 29.97 fps a frame is 1001 of those.
    SeekRelative(i64),
    SetPlayheadFromTimecode { input: String },
    RequestWhiteBalanceSample { nx: f32, ny: f32 },
    RequestQualifierSample { nx: f32, ny: f32 },
    /// The viewer click for an armed hue-curve eyedropper
    /// (`PickHueCurve`), as a fraction of the frame.
    RequestHueCurveSample { nx: f32, ny: f32 },
    BeginTitleEdit { clip_id: ClipId },
    EndTitleEdit,

    // Media
    LoadMediaFile { path: PathBuf, media_id: MediaId },
    ImportMedia { path: PathBuf },
    /// Import a file and, once it has loaded, drop it on the timeline
    /// at the chosen place. The Audio Library uses this so a downloaded
    /// track can go straight to the playhead or the end of the timeline.
    ImportMediaTo { path: PathBuf, drop: ClipDrop },
    /// Place an already-imported pool item on the timeline.
    PlaceMedia { media_id: MediaId, drop: ClipDrop },
    /// Import the media files of a directory and its subfolders, within
    /// the walk's limits (#3096); `bins` puts each subfolder's files in a
    /// bin named after it.
    ImportMediaFolder { directory: PathBuf, bins: bool },

    // Loop
    /// Render `from..to` into the render cache in the background (#924):
    /// Render In to Out, and Render Selection over the selected clips.
    RenderRange { from: Timecode, to: Timecode },
    /// Stop the render under way, keeping what it rendered.
    CancelRenderRange,
    /// Clear the render cache under `from..to`, every clip there (#924).
    ClearRenderCache { from: Timecode, to: Timecode },
    /// A track's colour label, 0 for none (#922).
    SetTrackColourLabel { track_index: usize, label: u8 },
    /// Move a track from `from` to `to` among the sequence's tracks: a
    /// header dragged up or down, kept among tracks of its kind (#922).
    MoveTrack { from: usize, to: usize },
    /// Split a stereo audio clip into two mono clips, its left and right
    /// channels, the right on the audio track below (#922).
    SplitToMono { clip_id: ClipId },
    /// Whether an audio track draws each clip's channels apart (#922).
    SetTrackChannelWaveforms { track_index: usize, on: bool },
    ToggleLoopPlayback,
    SetLoopPlayback(bool),
    /// Play the sequence from its In to its Out once, and stop there
    /// (#921). Without an In it starts at the beginning; without an Out
    /// it plays to the end.
    PlayInToOut,

    /// Toggle audio playback during hover skim. Mirrors FCP X's
    /// Shift+S audio-skim toggle. Flips both `state.audio_skimming_enabled`
    /// (runtime) and `prefs.playback.skim_audio` (persisted), tears
    /// down any active skim audio session if disabling, and emits a
    /// status toast. Dispatched from the Shift+S keybinding and the
    /// command palette ("Toggle Audio Skim").
    ToggleAudioSkim,

    /// Sync the user's "play audio during playhead scrub" preference into the
    /// engine so mute/chirp decisions can honour it.
    SetScrubAudioEnabled(bool),

    // Timeline
    SetPlayhead(Timecode),
    SetSpeed(f64),
    AppendClip { media_id: MediaId },
    AppendClipToTrack { media_id: MediaId, track_index: usize },
    /// Several media items dropped on a track together: appended end to
    /// end in the order given (the browser's order), each with its linked
    /// audio, as one undo step. Items that cannot go on the track's kind
    /// are skipped and counted in a toast.
    AppendMediaBatchToTrack { media_ids: Vec<MediaId>, track_index: usize },
    /// Insert an existing audio media item as a clip at a specific
    /// timeline position. Building block for opt-in audio
    /// sweetener layers (hyperzoom whoosh today; transition
    /// stingers, marker pings later). The caller is responsible
    /// for ensuring the [`MediaId`] already resolves in the media
    /// pool — this command does not import or probe.
    ///
    /// `track_index` semantics:
    /// - `Some(idx)` — must reference an existing audio track.
    /// - `None` — handler picks the lowest-indexed audio track,
    ///   or auto-creates one at the end of the track list when
    ///   the sequence has no audio tracks yet.
    InsertAudioMediaClip {
        media_id: MediaId,
        track_index: Option<usize>,
        timeline_in: Timecode,
    },
    RemoveClip { clip_id: ClipId },
    /// Several clips taken out as one undo step (#3060): deleted (a
    /// magnetic track closing up) or lifted (`lift`, the gap left).
    RemoveClips { clip_ids: Vec<ClipId>, lift: bool },
    /// Give an existing video clip the audio of its own media file,
    /// linked, spanning exactly what the clip spans.
    AttachLinkedAudio { clip_id: ClipId },
    MoveClip { clip_id: ClipId, new_timeline_in: Timecode },
    MoveClipToTrack { clip_id: ClipId, new_timeline_in: Timecode, from_track: usize, to_track: usize },
    /// Several clips moved by one drag, as one undo step (#3060): each to
    /// its new start, and to another track when `to_track` says so; their
    /// linked partners with them while the Linked switch is on.
    MoveClips { moves: Vec<(ClipId, Timecode, Option<usize>)> },
    /// A selection on the gapless primary dragged by `delta` (#3060): its
    /// primary clips (`block`, each with its start) as one block, in
    /// Insert or `overwrite`, and the `others` to their new starts, one
    /// undo step.
    MovePrimaryBlock { block: Vec<(ClipId, Timecode)>, others: Vec<(ClipId, Timecode, Option<usize>)>, delta: Timecode, overwrite: bool },
    /// Exchange two adjacent clips on the same track, preserving the
    /// total span. Validation rejects: cross-track pairs, non-adjacent
    /// clips (>1 frame gap), pairs sharing the same group, and
    /// asymmetric linked-partner pairs (one has an A/V partner, the
    /// other doesn't). When both clips have linked partners that are
    /// themselves adjacent on the partner track, those partners swap
    /// in lock-step. Single ctrl+z reverses everything.
    SwapAdjacentClips { clip_a: ClipId, clip_b: ClipId },
    TrimClip {
        clip_id: ClipId,
        old_source_in: Timecode,
        old_source_out: Timecode,
        old_timeline_in: Timecode,
        old_timeline_out: Timecode,
        new_source_in: Timecode,
        new_source_out: Timecode,
        new_timeline_in: Timecode,
        new_timeline_out: Timecode,
        ripple: bool,
        /// Whether a linked partner follows (#916): false trims this
        /// clip alone, a J or L cut (Alt while trimming, or the timeline's
        /// Linked switch off).
        linked: bool,
    },
    /// Rate stretch (#3055): `clip_ids` (the first the one dragged) made
    /// to fill their length to `to` by their speed, from their end
    /// (`at_end`) or their start, as one undo step; `ripple` moves what
    /// follows, `keep_pitch` is how the sound plays.
    StretchClip {
        clip_ids: Vec<ClipId>,
        at_end: bool,
        to: Timecode,
        ripple: bool,
        keep_pitch: bool,
    },
    /// Roll trim: adjust the boundary between two adjacent clips.
    /// Clip A's out moves and Clip B's in moves by the same amount, keeping total duration constant.
    RollTrimClip {
        clip_a_id: ClipId,
        clip_b_id: ClipId,
        new_a_source_out: Timecode,
        new_a_timeline_out: Timecode,
        new_b_source_in: Timecode,
        new_b_timeline_in: Timecode,
    },
    SelectClip(Option<ClipId>),

    /// Center the timeline view on the given clip's start. Used by the
    /// matte-badge "jump to source" gesture (Phase 4A) and any future
    /// reveal-in-timeline affordance. The handler resolves the clip's
    /// timeline position to pixels via the current zoom and writes
    /// `state.timeline_scroll_x` so the next paint draws with the
    /// clip centred. Silent no-op if the clip-id isn't in the active
    /// sequence.
    CenterTimelineOnClip(ClipId),

    // Project
    NewProject,
    CloseProject,
    /// Wipe all engine-side state (decoders, caches, frame buffer) so the next
    /// project starts from a clean slate. Sent by the UI on new/close project.
    ResetEngine,
    SaveProject,
    SaveProjectAs { path: PathBuf },
    OpenProject { path: PathBuf },
    /// The folder new projects go in and the welcome screen lists
    /// projects from (#1143).
    SetProjectsFolder { path: PathBuf },
    /// Open `path` without a dialog. The UI checks identity and
    /// save/discard policy immediately before acting, and the project's
    /// lock and auto-save as every open does (#2789): a lock left by a
    /// Klipp that died is taken over, one another Klipp holds refuses the
    /// open, and an auto-save newer than the last save refuses it unless
    /// `recovery` says what to do, `Some(true)` restoring the auto-save and
    /// `Some(false)` keeping the saved version.
    OpenProjectNow { path: PathBuf, save: bool, discard_changes: bool, expected_identity: u64, recovery: Option<bool> },
    /// Close without a dialog, rechecking the dirty state on the UI thread.
    CloseProjectNow { save: bool, discard_changes: bool, expected_identity: u64 },
    CreateProject {
        name: String,
        location: PathBuf,
        fps_num: u32,
        fps_den: u32,
        width: u32,
        height: u32,
        sequence_name: String,
        /// A project template to start from (#618): its sequences, tracks,
        /// bins and colour setup in place of the format above.
        template: Option<PathBuf>,
        /// Graded in a wide-gamut working space for HLG or PQ delivery.
        hdr: bool,
        /// A brand kit whose look the new sequence takes (#2722).
        brand_kit: Option<String>,
        /// Where it goes (#3307): put in the new sequence's brief.
        destination: Option<crate::project_formats::Destination>,
    },
    /// The look the viewer shows through, on screen only, or none (#466).
    SetProjectViewingLut { path: Option<PathBuf> },
    /// The LUT new clips start their grade with, or none (#466).
    SetProjectInputLut { path: Option<PathBuf> },
    /// The project's colour set-up changed to `colour`, as one undo step
    /// (#1875). With `convert_grades`, a change of working space wraps each
    /// clip's grade so it keeps its look.
    SetProjectColour { colour: Box<ProjectColour>, convert_grades: bool },
    /// Remember the frame rate chosen for a welcome-screen format (#1864).
    SetFormatRate { rate: (u32, u32) },
    /// Open a new, unsaved Untitled project in this format, or from the
    /// template file. Nothing is written until its first edit or a save
    /// (#1864); 0×0 and 0/0 take picture and rate from the first clip.
    NewUntitledProject {
        fps_num: u32,
        fps_den: u32,
        width: u32,
        height: u32,
        template: Option<PathBuf>,
        hdr: bool,
        /// Where it goes (#3307): put in the new sequence's brief.
        destination: Option<crate::project_formats::Destination>,
    },
    /// Keep the open project's structure as a project template called
    /// `name` (#618): its sequences' formats and tracks, its bins and
    /// smart bins and its colour setup, without media or edits; and, as
    /// starter content, the sequences `starter` names kept whole, their
    /// media copied into the template with `package_media`, else linked.
    SaveProjectTemplate { name: String, description: String, starter: Vec<crate::SequenceId>, package_media: bool },
    /// Delete the project template kept at `path` (#618).
    DeleteProjectTemplate { path: PathBuf },
    /// Rename the project template kept at `path` (#618).
    RenameProjectTemplate { path: PathBuf, name: String },
    /// Write the project template kept at `path` as one zip at `to`, for
    /// another machine (#618).
    ExportProjectTemplate { path: PathBuf, to: PathBuf },
    /// Add the project template in the zip at `from` to the user's (#618).
    ImportProjectTemplate { from: PathBuf },
    /// Put the files at `paths` in the Shared Library's collection
    /// `collection` (#614), where they are: references, not copies. An
    /// empty name means the collection the Library shows.
    AddToSharedLibrary { paths: Vec<PathBuf>, collection: String },
    /// Put the pool items' files in the Shared Library (#614), as
    /// `AddToSharedLibrary` does.
    AddMediaToSharedLibrary { media_ids: Vec<MediaId>, collection: String },
    /// Take `path` out of the Shared Library's `collection` (#614); the
    /// file and every project that imported it are left as they are.
    RemoveFromSharedLibrary { collection: String, path: PathBuf },
    /// Point the Shared Library's entry for `old` in `collection` at
    /// `new`: a moved file found again (#614).
    RelinkSharedLibraryItem { collection: String, old: PathBuf, new: PathBuf },
    /// A new, empty Shared Library collection (#614).
    CreateSharedCollection { name: String },
    /// Rename a Shared Library collection (#614).
    RenameSharedCollection { from: String, to: String },
    /// Delete a Shared Library collection: its entries, not their files (#614).
    DeleteSharedCollection { name: String },
    RemoveRecentProject { path: PathBuf },
    /// Open Preferences, on the page named (`cache`, `keyboard`) when one is.
    OpenPreferences { page: Option<String> },
    /// Open Timeline → Apply Brand Kit… (#2722).
    OpenApplyBrandKit,
    /// Keep a recent project at the top of the list, or stop keeping it.
    PinRecentProject { path: PathBuf, pinned: bool },
    /// Copy a project folder beside it under a new name, and list the copy.
    DuplicateProject { path: PathBuf },
    /// Move a project folder to the system trash, from where it can be
    /// put back. Nothing deletes a project outright.
    MoveProjectToTrash { path: PathBuf },
    RenameProjectOnDisk { old_path: PathBuf, new_name: String },
    /// Set project thumbnail from the current viewer frame.
    SetProjectThumbnailFromViewer,
    /// Set project thumbnail from an external image file.
    SetProjectThumbnailFromFile { path: PathBuf },

    // Sequences
    AddSequence { name: String, fps_num: u32, fps_den: u32, width: u32, height: u32 },
    /// Add a sequence and make it the one open: the control socket's
    /// `new_sequence`, which does not know the new sequence's index.
    AddSequenceAndOpen { name: String, fps_num: u32, fps_den: u32, width: u32, height: u32 },
    DuplicateSequence { sequence_idx: usize, new_name: String },
    /// A copy of a sequence at another frame size, opened, each picture
    /// reframed to follow its subject once the files' framing analysis is
    /// done, and its titles kept in title-safe (#2211).
    NewAspectVersion { sequence_idx: usize, width: u32, height: u32, name: String },
    /// A version made again from its master's cut (#2724): the master's
    /// tracks copied in, each clip still there keeping the framing given
    /// it in the version, the rest reframed; one undo step.
    RebuildVersion { sequence_idx: usize },
    SetActiveSequence { sequence_idx: usize },
    /// Make a sequence the one shown as `SetActiveSequence` does, keeping
    /// the selection the timeline taking over brings (a stacked timeline,
    /// #82).
    FocusSequence { sequence_idx: usize },
    RenameSequence { sequence_idx: usize, new_name: String },
    /// Remove the sequence `sequence_id`: by its id, not its place, so a
    /// second removal sent before the first is applied names the one meant
    /// (#2366).
    DeleteSequence { sequence_id: crate::SequenceId },
    ReorderSequences { from_idx: usize, to_idx: usize },
    /// Change a sequence's frame size. Goes through the undo history because
    /// changing the canvas reshapes how every clip composites.
    SetSequenceResolution { sequence_idx: usize, width: u32, height: u32 },
    /// Change a sequence's frame rate. Affects timecode display, render, and
    /// new-clip rate inheritance. Undoable for the same reason as resolution.
    SetSequenceFps { sequence_idx: usize, fps_num: u32, fps_den: u32 },
    /// UI: open the "Sequence Settings…" modal for the given sequence.
    OpenSequenceSettings { sequence_idx: usize },
    /// Re-trigger audio conform (PCM + peaks) for every media item in the
    /// pool that has audio. Used by the "Clear" buttons in the cache prefs
    /// tab so deleting on-disk caches also kicks off regeneration instead of
    /// leaving items pointing at deleted files.
    RegenerateAudioCacheForAll,
    /// Re-trigger filmstrip thumbnail generation for every media item that
    /// has a filmstrip, after the on-disk cache has been wiped.
    RegenerateFilmstripsForAll,
    /// Re-queue skim transcode generation for every media item, after the
    /// on-disk skim cache has been wiped.
    RegenerateSkimTranscodesForAll,

    // Workspace
    CreateWorkspace { path: PathBuf, name: String },

    // Edit
    Undo,
    /// Fold the top `count` undo steps into one under `label`, when each
    /// is labelled `only` and the document is still at
    /// `expected_revision`; otherwise nothing (#514).
    SquashUndo { count: usize, label: String, only: String, expected_revision: u64 },
    Redo,
    /// Collect every history push until `EndHistoryBatch` into one undo
    /// entry: what an agent's action is, however many edits it makes.
    BeginHistoryBatch,
    EndHistoryBatch,
    /// Close the batch as `EndHistoryBatch` does, under `label`: the
    /// step named for what the person did ("Ease In-Out"), not for the
    /// first edit inside it.
    EndHistoryBatchAs { label: String },
    SequenceUpdated,
    /// The shared sequence's preview was set or cleared (#3210): an entry
    /// hovered in a browser, shown on the frame without being applied.
    /// The engine draws the frame again with it, or without it. With `at`,
    /// it draws that time instead, as a timeline skim does, the playhead
    /// left where it is (a transition, at its middle); without, any such
    /// skim ends and the parked frame is drawn again.
    RefreshPreview { at: Option<Timecode> },
    /// Show `entry` on `clip_id` (else the selected clip, else the top
    /// picture under the playhead) as its card's hover shows it, at the
    /// playhead, at once (#3210): `frame_at`'s preview. The app's alone;
    /// refused, with why, when it can't be shown.
    ShowPreview { entry: PreviewEntry, clip_id: Option<ClipId> },
    /// Take the preview `ShowPreview` put up down.
    ClearPreview,
    /// The sequence's aux buses or sends were changed in place, by the
    /// mixer (#2861): the app gives the engine the sequence and marks the
    /// project changed. The undo step, if any, is already pushed.
    RoutingChanged,
    SetAntiClickFade(bool),

    // Export
    ShowExportDialog,
    ExportSequence { settings: ExportSettings },
    /// One delivery of several formats (#2724): for each output, the open
    /// sequence's version at its aspect (the sequence itself when it is
    /// at it, made and reframed when there is none), exported with its
    /// settings, each a job of the queue.
    ExportDelivery { outputs: Vec<DeliveryOutput> },
    CancelExport,
    /// Cancel exactly this render, even if another render became active.
    CancelExportJob { job_id: String },
    /// Delete a user-defined export preset by id.
    DeleteExportPreset { preset_id: String },
    /// Rename a user-defined export preset.
    RenameExportPreset { preset_id: String, new_name: String },
    /// Clone an existing preset (user or built-in) into a new
    /// user-preset entry with "(copy)" appended to the name.
    DuplicateExportPreset { preset_id: String },

    // Clip properties dialog (#47)
    /// Open the Clip Properties modal focused on the given media item.
    OpenClipProperties { media_id: MediaId },

    // Software jog wheel (#38, scoped — HID support is deferred)
    /// Step the playhead by N frames (positive = forward, negative =
    /// backward). Emitted by the transport-bar jog wheel widget.
    JogFrames { frames: i32 },

    /// Show a workspace (`"edit"`, `"colour"`, `"export"`) and an inspector
    /// tab (`"video"`, `"audio"`, `"colour"`, `"effects"`, `"title"`,
    /// `"generator"`, `"info"`), the inspector made visible: the layout
    /// only, never the document. For harnesses driving the app (the
    /// control socket's `show_panel`, test hooks only).
    ShowPanel { workspace: Option<String>, inspector_tab: Option<String> },
    /// Play these pointer steps into the app's input, one a frame, as a
    /// mouse's events (the control socket's `pointer`, test hooks only):
    /// the input only, never the document by itself.
    InjectPointer(Vec<crate::pointer::PointerStep>),
    /// Panic once at `at` in the running app (the control socket's
    /// `inject_panic`, test hooks only, #3181): `job`, `decoder`,
    /// `effect`, `mix`, `mix_thread` or `plugin`.
    InjectPanic { at: String },
    /// Where frozen tracks' frames are stored (#3207): the project's cache
    /// folder, `None` without a saved project. The engine reads frames
    /// there; the app writes them.
    SetFreezeFolder(Option<std::path::PathBuf>),
    /// The tracks playback lays from their frozen frames where those are
    /// current (#3207), by id; every other track is drawn live.
    SetFrozenTracks(Vec<crate::TrackId>),
    /// Frozen sound tracks' stems (#3207), each current: the track plays
    /// its stem in place of its clips and its inserts.
    SetFrozenStems(Vec<(crate::TrackId, std::path::PathBuf)>),
    /// Open the Colour workspace on video track `track_id`'s own grade
    /// (#1408): the layout only, never the document.
    GradeTrackInColour { track_id: TrackId },
    // Workspace layout presets (#39)
    /// Apply one of the 5 built-in workspace layouts by index into
    /// `builtin_workspace_layouts()`. Writes visibility flags + sizes.
    ApplyBuiltinWorkspaceLayout { index: usize },
    /// Apply a user-saved workspace layout by its `name`.
    ApplyCustomWorkspaceLayout { name: String },
    /// Remove a user-saved workspace layout by name (built-ins are never
    /// removable — checked in the handler).
    DeleteWorkspaceLayout { name: String },
    /// Rename a user-saved workspace layout. No-op if `new_name` is empty
    /// or if a layout with that name already exists.
    RenameWorkspaceLayout { old_name: String, new_name: String },

    // Compositing
    SetTrackOpacity { track_index: usize, opacity: f32 },
    SetTrackVisibility { track_index: usize, visible: bool },

    // Audio mixing
    SetTrackVolume { track_index: usize, volume: f32 },
    SetTrackMute { track_index: usize, muted: bool },
    SetTrackSolo { track_index: usize, solo: bool },
    SetTrackPan { track_index: usize, pan: f32 },
    SetMasterVolume(f32),
    SetMasterMute(bool),
    /// The loudness an export would deliver the sequence at, and its
    /// true-peak ceiling, for playback to be heard at (#1565); `None`
    /// plays at unity.
    SetDeliveryTarget(Option<(f32, f32)>),
    /// Hear playback at the export's delivered loudness, or not: the
    /// preference (#1565).
    SetHearDeliveryLoudness(bool),
    /// Reset the BS.1770-4 LUFS meter in the mix thread.
    ResetLufsMeter,
    SetClipVolume(f32),
    SetClipPan(f32),
    SetClipGain(f32),
    SetClipChannelConfig(u32),
    /// Switch the timeline rubber band to show a specific audio param.
    SetRubberBandParam(AudioParam),
    /// Switch the timeline rubber band to show a specific video param.
    SetVideoRubberBandParam(VideoParam),

    // Editing
    BladeClip { track_index: usize, clip_id: ClipId, cut_at: RationalTime },
    CutClip { clip_id: ClipId },
    CopyClip { clip_id: ClipId },
    PasteClip,
    /// What lies from `range_in` to `range_out` in the selects sequence
    /// `sequence_id` (another than the one being cut) put into the active
    /// sequence at `at` (#82): with every track cut and moved along, or
    /// over what is there with `overwrite`. `at` left out is the cut's In
    /// point, else its playhead.
    EditFromSelects { sequence_id: crate::SequenceId, range_in: Timecode, range_out: Timecode, at: Option<Timecode>, overwrite: bool },
    /// Clips `ids` of sequence `from`, with their linked partners when
    /// `linked`, their effects and keys, copied into the sequence shown,
    /// the first at `at`, the topmost on `track` (the rest of its kind
    /// as far below it as they were), else each on its own track: an
    /// insert, or an overwrite. One step. Clips carried from one stacked
    /// timeline to the other (#82).
    PasteClipsFrom { from: crate::SequenceId, ids: Vec<ClipId>, at: Timecode, overwrite: bool, track: Option<usize>, linked: bool },
    /// Paste at the playhead as an insert: every unlocked track is cut
    /// there and what follows moves along by the paste's length (#845).
    PasteClipInsert,
    DuplicateClip { clip_id: ClipId },
    /// Copy attributes from a clip for later paste-attributes.
    CopyAttributes { clip_id: ClipId },
    /// Paste previously copied attributes onto the selected clip(s).
    PasteAttributes {
        effects: bool,
        color_grade: bool,
        audio: bool,
        transform: bool,
        speed: bool,
    },
    /// Toggle a clip's enabled state (disabled clips are dimmed and skipped in render).
    SetClipEnabled { clip_id: ClipId, enabled: bool },
    /// Solo a clip — mute all other clips on the same track.
    SetClipSolo { clip_id: ClipId, solo: bool },
    SelectAllClips,
    /// Select all clips that start after the playhead.
    SelectClipsForward,
    /// Select all clips that end before or at the playhead.
    SelectClipsBackward,
    /// Select the clips of one track, or of every track when `track` is
    /// `None`, in `range` of the playhead. Clips on locked tracks are
    /// left out (#844).
    SelectClipsIn { track: Option<usize>, range: SelectRange },
    /// [`AppCommand::SelectClipsIn`] on the track of the selected clip:
    /// for a key binding, which has no track of its own to name.
    SelectClipsOnSelectedTrack { range: SelectRange },
    /// Select every clip not selected now (#925).
    SelectInverse,
    /// Select every clip from the same media files as the selection (#925).
    SelectSameSource,
    /// Select every clip with this colour label (#925).
    SelectByColour { label: u8 },
    /// Close every gap on a track: each clip moved up to the one before (#925).
    CloseAllGaps { track_index: usize },
    /// Keep a retimed clip's pitch, or let it move with the speed as
    /// tape's does (#925); its linked sound with it.
    SetClipKeepPitch { clip_id: ClipId, keep_pitch: bool },
    /// Put the playhead on the next gap on any unlocked track (#925).
    GoToNextGap,
    /// A subclip of a timeline clip's media, over the source range the
    /// clip uses (#925).
    SubclipFromClip { clip_id: ClipId },
    /// Open the Speed dialog on a clip, filled with its speed (#925).
    OpenSpeedDialog { clip_id: ClipId },
    /// Open an editing helper's dialog on a clip (#988): 0 Strobe, 1 Set
    /// Length (of the selection), 2 Copy Over Every Edit.
    OpenEditHelper { kind: u8, clip_id: ClipId },
    /// Run the editing helper its dialog is set up for, as one undo step.
    RunEditHelper,
    /// Line the selected clips up with the selected one (the anchor), by
    /// their files' start timecodes, or by their first clip markers (#925).
    SyncSelectedClips { by_markers: bool },
    /// Save current viewer frame as PNG via a file dialog.
    FrameGrab,
    /// Save the current viewer frame as a PNG under `<project>/media/stills/`
    /// (or a temp directory when the project is unsaved) and import it into
    /// the media bin as a still MediaItem. No file dialog.
    ExtractFrameToBin,
    /// Import an SRT, VTT, ASS/SSA, TTML, SCC or EBU-STL subtitle file onto a subtitle track.
    ImportSubtitles { path: PathBuf },
    /// Import a Kdenlive MLT XML project as a new sequence.
    /// Unsupported features (filters, non-cross-dissolve transitions, keyframes)
    /// are shown in the last import report.
    ImportKdenlive { path: PathBuf },
    /// Parse an OpenTimelineIO `.otio` JSON file and import its timeline as
    /// a new Sequence. Unsupported features (transitions, effects, nested
    /// stacks) are shown in the last import report.
    ImportOtio { path: PathBuf },
    /// Import an existing supported interchange format and publish its report.
    ImportTimeline { path: PathBuf, request_id: uuid::Uuid },
    /// A timeline from another editor opened as a new project (#2083),
    /// its format told from its content, and its report published.
    /// `request_id` nil is the person's own: unsaved changes are asked
    /// about first; a socket's is refused while there are any.
    OpenFromAnotherEditor { path: PathBuf, request_id: uuid::Uuid },
    /// Import a CMX 3600 EDL as a new sequence, its events matched to the
    /// media in the pool (#925).
    ImportEdl { path: PathBuf },
    /// Parse a Final Cut Pro XML (`.fcpxml`) file and import its timeline as
    /// a new Sequence. Unsupported features (effects, transitions, compound
    /// clips, multicam, roles, connected clips) are shown in the
    /// last import report.
    ImportFcpxml { path: PathBuf },
    /// Copy the current project and all referenced media into a new
    /// self-contained directory at `dest_dir`. Source project is untouched.
    /// Runs on a background thread — see `ConsolidateDone` for completion.
    ConsolidateProject {
        dest_dir: PathBuf,
        include_proxies: bool,
        include_skims: bool,
        /// After copying, zip the consolidated folder into `<dest_dir>.zip`.
        create_zip: bool,
        /// After a successful zip, remove the consolidated folder. Ignored
        /// when `create_zip` is false.
        delete_folder_after_zip: bool,
    },
    /// Create a new empty subtitle clip at the given timeline position on
    /// the first Subtitle/Text track (creates a Subtitle track if none exists).
    /// Used by the subtitle editor panel's "Add at playhead" button.
    AddSubtitleAtPlayhead { timeline_in: Timecode, duration_frames: u32 },
    /// The subtitle editor's style draft saved as the track's caption
    /// style, and with `apply_to_all` put on every caption there (#70).
    SaveCaptionStyle { track_id: TrackId, apply_to_all: bool },
    /// Back to the default caption look for new captions on the track.
    ClearCaptionStyle { track_id: TrackId },
    /// Export subtitles from subtitle tracks as SRT.
    ExportSubtitles { path: PathBuf },
    /// Export sequence as FCPXML 1.9 for Final Cut Pro interchange.
    ExportFcpxml { path: PathBuf },
    /// Export sequence as MLT XML (.kdenlive) for Kdenlive interchange (cuts-only v1).
    ExportKdenlive { path: PathBuf },
    /// Export sequence as CMX 3600 EDL for broad NLE interchange.
    ExportEdl { path: PathBuf },
    /// Export sequence markers as YouTube-compatible chapter text
    /// ("H:MM:SS Label" per line, with a synthetic "Intro" at 0:00 when
    /// the first marker is after the start).
    ExportYouTubeChapters { path: PathBuf },
    /// Render a review copy of the active sequence to `path`, its round
    /// and timecode burned in, with a comment template beside it (#2214).
    ExportReviewCopy { path: PathBuf },
    /// Read reviewers' comments from `path` and add them as open review
    /// markers, as one undo step (#2214).
    ImportReviewComments { path: PathBuf },
    /// Write the sequence's review markers to `path` as a table (#2214).
    ExportReviewList { path: PathBuf },
    /// Set where a review comment stands (#2214).
    SetReviewStatus { marker_id: crate::MarkerId, status: crate::ReviewStatus },
    /// Open the export dialog with the range pre-set to "Selected
    /// clips" mode. Dispatched by the timeline clip-context-menu
    /// "Export This Clip…" entry and the matching palette command —
    /// both surfaces select the clip(s) first via [`Self::SelectClip`]
    /// or rely on an existing multi-select. The export-range itself
    /// is computed from `state.selected_clips` at dispatch time by
    /// the same code path the dialog uses for its "Selected clips"
    /// radio (`selected_clips_frame_range`), so behaviour stays in
    /// sync with the existing in-dialog flow.
    OpenExportDialogForSelectedClips,
    /// Swap the media backing a timeline clip while keeping every
    /// other property — `timeline_in/out`, `source_in/out`, effects,
    /// transitions, linked partner, keyframes. When the new media
    /// is shorter than the existing trim window, both `source_out`
    /// and `timeline_out` clamp by the same delta so no orphan
    /// timeline space appears. No-op when `new_media_id` already
    /// backs the clip.
    ReplaceClipMedia { clip_id: ClipId, new_media_id: MediaId },
    /// Generate a headless bash render script that calls ffmpeg to render
    /// the current sequence. Cuts only — effects/grades/transitions are NOT
    /// serialized. The script includes a prominent WARNING comment advising
    /// users to Ctrl+E for full-quality export.
    GenerateRenderScript { path: PathBuf },
    /// Create a compound clip (nested sequence) from selected clips.
    CreateCompoundClip { name: String },
    /// Create a multicam clip from clips on different tracks at the playhead.
    CreateMulticamClip,
    /// Sync these pool items onto one clock and lay them out as a
    /// multicam at the playhead: each angle on its own tracks, grouped.
    CreateSyncedMulticam { media_ids: Vec<MediaId>, method: crate::MulticamSync },
    /// Switch multicam angle (1-indexed).
    SwitchMulticamAngle { angle: usize },
    /// The same at a given time rather than the playhead: the control
    /// socket's `switch_angle`, whose cut must land where it was asked
    /// for, not wherever a seek had got to.
    SwitchMulticamAngleAt { angle: usize, at: Timecode },
    /// Choose whose sound the multicam group at `at` (the playhead when
    /// `None`) plays, on every primary of the group: one undo step.
    SetMulticamSound { sound: crate::MulticamSound, at: Option<Timecode> },
    /// Set the audio mixer's pan law.
    SetPanLaw(crate::config::PanLaw),
    /// Zoom timeline to fit all clips.
    FitTimelineToWindow,
    /// Set timeline zoom (pixels-per-second), optionally with a new scroll_x in
    /// the same step for cursor-anchored zoom. Clamped to (0.1, 2000.0).
    SetTimelineZoom {
        zoom: f32,
        scroll_x: Option<f32>,
    },
    /// Fit the timeline to the visible content width. Preferred over
    /// `FitTimelineToWindow` for zoom-button / fit-button call sites.
    ZoomTimelineToFit,
    /// Apply a motion preset (Ken Burns / PiP) to selected clip.
    ApplyMotionPreset { clip_id: ClipId, preset: String },
    /// Create an adjustment layer at the playhead on the top video track.
    CreateAdjustmentLayer,
    /// Create a generator clip on a video track at the playhead.
    CreateGeneratorClip { track_index: Option<usize>, data: crate::GeneratorData },
    /// Update a generator clip's data (color, gradient, etc.).
    UpdateGeneratorData { clip_id: ClipId, data: crate::GeneratorData },
    /// Capture the current viewer frame and insert a freeze-frame generator
    /// clip at the playhead. Blades any clip under the playhead so the freeze
    /// lands cleanly between the two halves.
    InsertFreezeFrame { duration_secs: f64 },
    /// Open (drill into) a compound clip for editing.
    OpenCompoundClip { clip_id: ClipId },
    /// Navigate back from a nested sequence to the parent.
    CloseCompoundClip,
    /// Decompose a compound clip — replace it on the parent timeline with the
    /// inner sequence's clips at their original positions, then delete the
    /// now-orphaned inner sequence. Inverse of `CreateCompoundClip`.
    DecomposeCompoundClip { clip_id: ClipId },
    /// Navigate to next edit point (clip boundary) on the timeline.
    NextEditPoint,
    /// Navigate to previous edit point (clip boundary) on the timeline.
    PrevEditPoint,
    /// Close the gap (empty space) at the playhead on a given track, or the nearest gap.
    CloseGap { track_index: usize, gap_start: RationalTime, gap_end: RationalTime },
    /// A gap filled with a generator spanning it exactly, as one step
    /// (#3116): a gap clip on the primary track gives up its room.
    FillGap { track_index: usize, start: RationalTime, end: RationalTime, data: crate::GeneratorData },

    // Source monitor
    LoadSourceMedia { media_id: MediaId },
    SetSourceInPoint(RationalTime),
    SetSourceOutPoint(RationalTime),
    InsertSourceAtPlayhead,
    /// Seek the Source Viewer to an absolute media-relative time.
    /// Updates `source_playhead` and triggers a frame re-decode at that
    /// position so the source viewer's display follows the scrub.
    SeekSource(RationalTime),

    /// Match Frame — program → source. Find the topmost video clip
    /// under the program playhead, load its media into the source
    /// viewer, and seek the source playhead to the source-time
    /// corresponding to the current program frame. NLE staple bound
    /// to the `F` shortcut. Honours speed-ramped clips and
    /// `clip.source_in`. No-op (toast) when no media clip sits under
    /// the program playhead.
    MatchFrameProgramToSource,

    /// Open the consent dialog for the model identified by its
    /// stable id in `assets/models.toml`. The handler checks the
    /// local cache first — already-cached models surface a toast
    /// instead of a dialog. The dialog lets the user confirm before
    /// any network call; on confirm the download runs on a worker
    /// thread with progress + cancel UI.
    OpenModelDownloadDialog { model_id: String },

    /// Open the AI Tools panel pre-selected on the given task. If the
    /// panel is already open, this just switches the active task.
    /// Surfaces the model picker + run/cancel controls.
    ShowAiToolsPanel { task: AiToolsTaskTag },
    /// Run the active AI Tools task on the current frame using the
    /// currently-selected model. No-op (toast) when no task is active
    /// or no model is selected.
    RunAiToolsInference,
    /// Cancel an in-progress AI Tools inference. Idempotent — no-op
    /// when nothing is running.
    CancelAiToolsInference,
    /// Set the active AI Tools task without forcing the panel open.
    /// The panel still updates its UI to reflect the new task.
    SetAiToolsTask(AiToolsTaskTag),
    /// Pick a specific model (by its `assets/models.toml` id) for the
    /// currently active task. The handler validates the model is
    /// compatible with the active task type before applying.
    SelectAiToolsModel { model_id: String },

    /// Reverse Match Frame — source → program. Given the source
    /// viewer's currently-loaded media + playhead, find the next
    /// occurrence on the timeline (first occurrence whose timeline_in
    /// is strictly past the program playhead, wrapping to the
    /// earliest occurrence if none). Bound to `Shift+F`. Lets the
    /// user repeat-press to cycle through every place a source frame
    /// appears in the edit. No-op (toast) when the source media
    /// isn't on the timeline at the loaded source-time.
    MatchFrameSourceToProgram,
    /// Create a subclip from the source media's current IN/OUT zone. The new
    /// MediaItem inherits codec/fps/resolution from the parent, reuses the
    /// parent's path, and carries its own default_source_in/out matching the
    /// zone. `name: None` → auto-generated as "{parent_stem}_sub{N}".
    CreateSubclipFromZone { source_media_id: MediaId, name: Option<String> },

    // Sequence markers
    SetSequenceInPoint(RationalTime),
    /// The sequence's range set to a clip's span, one step (#3127): the
    /// selected clip, else the one under the playhead.
    SetRangeToClip,
    /// The program viewer's timecode opened to type a time to go to
    /// (#3130).
    GoToTimecode,
    SetSequenceOutPoint(RationalTime),
    ClearSequenceInPoint,
    ClearSequenceOutPoint,

    // Proxy
    GenerateProxy { media_id: MediaId },
    CancelProxy { media_id: MediaId },
    SetUseProxies(bool),
    /// Show or hide one of the viewer's overlays (the inspector's
    /// switches; the keys and the palette toggle them in place).
    SetViewerOverlay { overlay: ViewerOverlay, on: bool },
    /// Multicam mode in the Source viewer (#1844): on, the Source viewer
    /// shows the angle grid of the multicam clip under the playhead, or of
    /// `open` (a multicam clip opened by a double-click); off, the source.
    /// Only this turns it on or off; the playhead crossing a multicam clip
    /// never does.
    SetMulticamMode { on: bool, open: Option<ClipId> },
    /// Begin timeline scrubbing — engine may switch to proxy decode for speed.
    /// Enable or disable the smart render cache.
    SetRenderCacheEnabled(bool),
    /// Draw heavy frames ahead while idle, on mains power, after
    /// `delay_secs` with nothing done (#3212).
    SetBackgroundRender { on: bool, delay_secs: f32 },
    /// The render cache's size cap in GB (the cache preferences' Max
    /// cache size): past it the least recently used frames go, at once
    /// when it shrinks.
    SetRenderCacheLimitGb(u32),
    /// Every frame of the render cache let go, on disk and in its index.
    ClearRenderCacheAll,
    /// The open project's frozen frames and/or frozen sound cleared
    /// (#3207): their tracks played live again, one undo step.
    ClearFrozenCache { frames: bool, sound: bool },
    /// The size frames are drawn at while playing (#3234), saved as the
    /// preference, as its control in Preferences sets it.
    SetPlaybackQuality(crate::PlaybackQuality),
    /// The playback note put away (#3234): for now, or with `forever`
    /// never suggested again (the preference turned off).
    DismissPlaybackNote { forever: bool },
    /// Scan cache directories and remove files not referenced by any media in the pool.
    CleanOrphanedCaches,
    /// Cache Manager (feature #78): clear a single cache category by wiping
    /// its directory then recreating the empty folder so later writes don't
    /// fail. Followed by an implicit refresh of the cache-manager snapshot.
    ClearCacheCategory { path: PathBuf },
    /// Re-scan all cache categories and update `AppState::cache_categories`.
    RefreshCacheManager,
    /// Open the Cache Manager modal. Dispatched from Preferences → Proxy.
    /// Handled by flipping `AppState::show_cache_manager` to true after
    /// calling `refresh_cache_categories`.
    OpenCacheManager,
    /// Open the AI Models management modal. Lists every model in the
    /// manifest, install status, and per-model install / reinstall
    /// actions. Handled by flipping `AppState::show_ai_models_dialog`.
    OpenAiModelsDialog,
    /// Open the Audio Plugins management modal. Lists every discovered
    /// LADSPA plugin grouped by maker, the search paths that were
    /// scanned, and exposes a Rescan action that clears the disk cache
    /// and re-runs discovery. Handled by flipping
    /// `AppState::show_audio_plugins_dialog`.
    OpenAudioPluginsDialog,
    /// Drop the on-disk LADSPA discovery cache and re-run
    /// `discover_ladspa_plugins`. Used by the Audio Plugins dialog's
    /// Rescan button. Engine doesn't need to know — this is a UI-side
    /// operation that updates `state.ladspa_plugins` and the
    /// `effect_registry`.
    RescanAudioPlugins,
    /// Toggle the enabled/disabled state of a LADSPA plugin keyed on
    /// its `unique_id`. The Audio Plugins dialog dispatches this from
    /// the per-row toggle. Updates `state.disabled_audio_plugins`,
    /// rebuilds the LADSPA section of `effect_registry`, and persists
    /// the new disabled set to disk. Existing AudioEffectInstance
    /// entries on saved clips continue to work; the disable only
    /// blocks the plugin from the browser / "+ Add Effect" picker.
    ToggleAudioPluginDisabled { unique_id: u64 },
    /// Re-enable every disabled audio plugin in one shot. Issued from
    /// the Audio Plugins dialog's "Clear all disabled" footer button
    /// (with a confirmation prompt rendered there). Persists the
    /// now-empty disabled set and rebuilds the registry.
    ClearAllAudioPluginsDisabled,
    /// Open the System Probe modal — read-only summary of GPU
    /// vendor, ONNX Runtime status, audio backend, build features,
    /// Klipp version. Single-shot snapshot intended to paste into
    /// bug reports.
    OpenSystemProbe,
    /// Open the Project Doctor modal — scans the currently-loaded
    /// `.klipp` directory for missing media, missing LADSPA
    /// plugins, missing AI mask files, and other inconsistencies.
    /// Read-only report in v1; per-issue Fix actions are queued.
    OpenProjectDoctor,
    BeginScrub,
    /// The timeline's picture at `time` for a skim (#2945): drawn by the
    /// engine from the media's skim files with every title, effect and
    /// transition, the playhead left where it is. Answered by
    /// `AppEvent::TimelineSkimShown`; one not drawn is left to the raw
    /// skim frame.
    SkimTimeline { time: Timecode },
    /// End timeline scrubbing — engine restores full-res decode.
    EndScrub,
    /// Relink a specific offline media item to a new path.
    RelinkMedia { media_id: MediaId, new_path: PathBuf },
    /// Relink one item to a file the user picked by hand: relinked at
    /// once, then checked against what the project knew of it (reel,
    /// timecode, length, rate, size), and a warning shown when the file
    /// is not that clip.
    RelinkMediaByHand { media_id: MediaId, new_path: PathBuf },
    /// Transcribe one clip now — the media browser's "Transcribe Clip"
    /// right-click action. Runs speech-to-text on this item even when
    /// automatic transcription is switched off in preferences.
    TranscribeClip { media_id: MediaId },
    /// Read these items' files again for what the camera recorded about
    /// the shot (timecode, reel, exposure, sidecar, every tag), keeping
    /// what the user said about them.
    RefreshMediaMetadata { media_ids: Vec<MediaId> },
    /// Write the pool's metadata as a sheet, CSV or ALE.
    ExportMetadataSheet { path: PathBuf, ale: bool },
    /// Read a sheet, CSV or ALE, into the clips it names.
    ImportMetadataSheet { path: PathBuf },
    /// Relink all offline media by searching a directory recursively.
    RelinkAllFromDirectory { directory: PathBuf },
    /// Show the media relink dialog.
    ShowRelinkDialog,
    AddMediaTag { media_id: MediaId, tag: String },
    RemoveMediaTag { media_id: MediaId, tag_index: usize },

    // Node graph (color grading)
    /// A grade node of `kind` (a name in `klipp_nodes::kinds::addable_node_kinds`).
    /// With `node_id` (the sender has to know it: the control socket) it
    /// has that id and goes into the clip's top-level graph; without, it
    /// gets a new id and goes into the graph the node editor shows.
    AddNode { clip_id: ClipId, kind: String, node_id: Option<uuid::Uuid>, position: (f32, f32) },
    RemoveNode { clip_id: ClipId, node_id: uuid::Uuid },
    /// Collapse a multi-node selection into a single Compound node.
    /// The Compound's `params.compound_subgraph` captures the selected
    /// nodes + their internal edges (wrapped with synthetic Source +
    /// Output so the executor recognises it as a runnable graph). The
    /// parent graph rewires its inbound external edge to the Compound's
    /// input and its outbound external edge to whatever consumed the
    /// old subgraph's output. Single-input / single-output topology
    /// only for v1; the handler refuses (toast + no-op) when the
    /// selection has more than one external in-edge target or more
    /// than one external out-edge source.
    GroupIntoCompound {
        clip_id: ClipId,
        node_ids: Vec<uuid::Uuid>,
    },
    /// Navigate the V2 canvas into a Compound node's sub-graph.
    /// Appends `node_id` to `AppState::graph_descent_path`; the
    /// canvas then renders `compound.params.compound_subgraph`
    /// instead of the parent. Selection is cleared on descent so the
    /// inspector doesn't show a stale node from the outer level.
    /// No-op when `node_id` isn't a Compound with a sub-graph.
    DescendIntoCompound {
        clip_id: ClipId,
        node_id: uuid::Uuid,
    },
    /// Explode a Compound node back into its constituent nodes in
    /// the parent graph. The sub-graph's synthetic Source + Output
    /// are discarded; everything else is cloned out and the
    /// external edges that ran through the Compound are rewired
    /// directly to the nodes that previously fed / received the
    /// sub-graph's Source / Output internally. Single-port compounds
    /// only in v1 (same shape Group accepts). Selection is replaced
    /// with the freshly-unpacked set so the user can immediately
    /// continue editing.
    UngroupCompound {
        clip_id: ClipId,
        node_id: uuid::Uuid,
    },

    /// Overwrite the currently-active grade version with the
    /// clip's live `node_graph`. For "I tweaked V1, save my
    /// changes into V1" workflows. No-op when no version is active.
    UpdateActiveGradeVersion { clip_id: ClipId },
    ConnectNodes { clip_id: ClipId, from: uuid::Uuid, from_port: u8, to: uuid::Uuid, to_port: u8 },
    DisconnectEdge { clip_id: ClipId, to: uuid::Uuid, to_port: u8 },
    /// Wireless-link sender label on a node's primary output. `None`
    /// clears it.
    SetNodeOutputLink { clip_id: ClipId, node_id: uuid::Uuid, label: Option<String> },
    /// Wireless-link receiver label on a node's input port. `None`
    /// clears it — the port falls back to normal wired-edge
    /// resolution.
    SetNodeInputLink { clip_id: ClipId, node_id: uuid::Uuid, port: u8, label: Option<String> },
    MoveNode { clip_id: ClipId, node_id: uuid::Uuid, delta_x: f32, delta_y: f32 },
    ToggleNodeEnabled { clip_id: ClipId, node_id: uuid::Uuid },
    /// A grade node's settings back to their defaults, wherever in the
    /// grade it is (#1742). What it is stays: its effect, plug-in, LUT or
    /// matte file, and a group's nodes.
    ResetNode { clip_id: ClipId, node_id: uuid::Uuid },
    /// The outside node of a correction gated by a key (#898): a Corrector
    /// straight after it, reading the same key inverted, so it grades
    /// where the inside one doesn't. One undo step.
    AddOutsideNode { clip_id: ClipId, node_id: uuid::Uuid },
    /// Auto-layout the clip's node graph — recompute every node's
    /// position using a hierarchical layered layout. Pushed as a
    /// single undoable batch (one entry on the history stack
    /// reverts every position with one Ctrl+Z).
    AutoLayoutNodeGraph { clip_id: ClipId },
    /// Create a new lightweight frame around the given node ids.
    /// Frame id is allocated by the handler. Members must already
    /// exist in the clip's graph; missing ids are silently dropped.
    /// A frame with zero members after filtering is not created.
    CreateNodeFrame { clip_id: ClipId, node_ids: Vec<uuid::Uuid>, label: String, color_rgba: u32 },
    /// Rename a frame. No-op if the id doesn't exist.
    RenameNodeFrame { clip_id: ClipId, frame_id: uuid::Uuid, label: String },
    /// Set a frame's multi-line description. Empty string clears.
    SetNodeFrameDescription { clip_id: ClipId, frame_id: uuid::Uuid, description: String },
    /// Create a free-floating sticky note at the given graph
    /// position. The handler allocates the id; width defaults to
    /// 200 graph-px; text starts empty (the user double-clicks to
    /// edit). Single source of truth for sticky-note creation
    /// across the right-click "Add note here" entry and any
    /// future palette / shortcut surfaces.
    CreateStickyNote { clip_id: ClipId, x: f32, y: f32 },
    /// Replace the multi-line text of the named sticky note.
    /// No-op if the id doesn't exist.
    SetStickyNoteText { clip_id: ClipId, sticky_id: uuid::Uuid, text: String },
    /// Move a sticky note by (delta_x, delta_y) in graph pixels.
    /// Mirrors MoveNode's incremental-delta shape so live drags
    /// can stream tick deltas without total-position drift.
    MoveStickyNote { clip_id: ClipId, sticky_id: uuid::Uuid, delta_x: f32, delta_y: f32 },
    /// Delete a sticky note. No-op if the id doesn't exist.
    DeleteStickyNote { clip_id: ClipId, sticky_id: uuid::Uuid },
    /// Recolour a frame.
    RecolorNodeFrame { clip_id: ClipId, frame_id: uuid::Uuid, color_rgba: u32 },
    /// Drop the frame. Member nodes stay where they are; only the
    /// visual rectangle goes away.
    DeleteNodeFrame { clip_id: ClipId, frame_id: uuid::Uuid },
    /// Add a single node to an existing frame's membership set.
    AddToNodeFrame { clip_id: ClipId, frame_id: uuid::Uuid, node_id: uuid::Uuid },
    /// Remove a single node from an existing frame's membership.
    /// Frame is pruned if this leaves it with zero members.
    RemoveFromNodeFrame { clip_id: ClipId, frame_id: uuid::Uuid, node_id: uuid::Uuid },
    /// Set the node-graph "solo" target — the engine renders the
    /// graph normally but returns the soloed node's output as the
    /// final image, so the user can preview any mid-chain node as
    /// if it were the Output. `None` clears solo and restores the
    /// normal Output-node-input result. Single global slot (one
    /// active clip at a time). The node by its path in the inspected
    /// clip's picture and its output (M3 step 16a); one the clip's run
    /// doesn't hold is said to be unavailable, never shown as the output.
    SetNodeSolo { at: Option<klipp_graph_format::path::NodeOutput> },
    /// Show one channel of the viewed picture in grey, or the picture
    /// again (#2758). The viewer's alone: exports never read it.
    SetViewChannel { channel: crate::ViewChannel },
    /// Set node-graph view bookmark slot `slot` (0..8 corresponding
    /// to hotkeys 1..9) to the panel's current pan + zoom. Routed to
    /// the panel directly in the dispatcher because the bookmark
    /// state lives on the panel, not on the project / engine /
    /// clip — it's a navigation aid, not a property of the grade.
    SetNodeViewBookmark { slot: u8 },
    /// Jump the node-graph canvas to view bookmark slot `slot`. No-op
    /// when the slot is empty (the panel emits its in-canvas toast in
    /// that case so the user knows the binding registered).
    JumpNodeViewBookmark { slot: u8 },
    /// Copy the currently-selected color nodes (from `selected_node_ids`)
    /// into AppState's grade-node clipboard. Emits a toast with the count;
    /// no-op + toast when the selection is empty. Edges between copied
    /// nodes are not preserved.
    CopyColorNodes { clip_id: ClipId },
    /// Paste the grade-node clipboard into `clip_id`'s node graph. Each
    /// pasted node receives a freshly-allocated `NodeId` so collisions
    /// with existing nodes — or with a previous paste — are impossible.
    /// No-op + toast when the clipboard is empty.
    PasteColorNodes { clip_id: ClipId },
    /// The node editor's own edits of its selection (#1745), on the grade
    /// level or picture level the canvas shows: copies of `node_ids` put
    /// beside them, as one undo step.
    DuplicateNodes { clip_id: ClipId, node_ids: Vec<uuid::Uuid> },
    /// `node_ids` to the node editor's clipboard, from the level holding
    /// them.
    CopyNodes { clip_id: ClipId, node_ids: Vec<uuid::Uuid> },
    /// The node editor's clipboard into the level the canvas shows, its
    /// first node at `position` (canvas units), as one undo step.
    PasteNodes { clip_id: ClipId, position: (f32, f32) },
    /// A node swapped for a new node of `kind` in its place: its wires
    /// and its place kept, as one undo step.
    ReplaceNode { clip_id: ClipId, node_id: uuid::Uuid, kind: String },
    /// A new node of `kind` put into the wire from `from`'s `from_port`
    /// to `to`'s `to_port` (#1744): fed by the one, feeding the other,
    /// at `position`, as one undo step.
    InsertNodeOnWire { clip_id: ClipId, kind: String, position: (f32, f32), from: uuid::Uuid, from_port: u8, to: uuid::Uuid, to_port: u8 },
    /// The node `node_id`, dropped on that wire (#1744): taken out of
    /// where it was, what fed it now feeding what it fed, and put into
    /// the wire. Inside the canvas's layout gesture, its step.
    SpliceNodeIntoWire { clip_id: ClipId, node_id: uuid::Uuid, from: uuid::Uuid, from_port: u8, to: uuid::Uuid, to_port: u8 },
    /// The wire into `to`'s `to_port`, its end dragged to `new_to`'s
    /// `new_to_port` (#1744), as one undo step.
    ReconnectEdge { clip_id: ClipId, to: uuid::Uuid, to_port: u8, new_to: uuid::Uuid, new_to_port: u8 },
    /// A new node of `kind`, fed by `from`'s `from_port` (#1744): a wire
    /// dragged into empty canvas and a node picked for it.
    AddNodeWired { clip_id: ClipId, kind: String, position: (f32, f32), from: uuid::Uuid, from_port: u8 },
    /// The picture canvas's copy (#1745): the nodes as the picture holds
    /// them, kept for a paste.
    CopyPictureNodes { nodes: Vec<klipp_graph_format::Node> },
    /// The picture canvas's paste: the kept nodes, fresh ids, after the
    /// node `after` names, the first at `position`.
    PastePictureNodes { after: klipp_graph_format::path::NodePath, position: (f32, f32) },
    RenameNode { clip_id: ClipId, node_id: uuid::Uuid, label: String },
    /// The LUT file a LUT node applies (#892); `None` takes it off. One
    /// undo step.
    SetNodeLut { clip_id: ClipId, node_id: uuid::Uuid, path: Option<std::path::PathBuf> },
    /// The word spoken at `source_secs` of `media_id`'s sound corrected to
    /// `text` in its transcript (#531): captions, search and word cuts read
    /// the correction, and it is kept when the file is transcribed again.
    CorrectTranscriptWord { media_id: MediaId, source_secs: f64, text: String },
    /// Transcribe `media_id` again however fresh its transcript (#531):
    /// with `model` (an id; `None` the best installed) and in `language`
    /// (an ISO 639-1 code; `None` the brief's, or detected).
    RetranscribeClip { media_id: MediaId, model: Option<String>, language: Option<String> },
    /// The language the project's speech is in, kept on the brief (#531):
    /// an ISO 639-1 code, or `None` to detect it per file. One undo step;
    /// transcripts detected in another language are made again.
    SetTranscriptLanguage { language: Option<String> },
    /// An audition's edit on clip `clip_id` (#3209): the `alternatives`
    /// op, one undo step; refused with why in a toast.
    EditAudition { clip_id: ClipId, edit: AuditionEdit },
    /// The audition picker for `clip_id`, or closed (`None`) (#3209).
    OpenAuditionPicker { clip_id: Option<ClipId> },
    /// Play an audition's takes in its slot (#3209), each over its slot
    /// and a little either side, nothing in the project changed: from
    /// `take` (the one in the slot when `None`), each in turn with `all`,
    /// else that one round and round.
    PreviewTakes { clip_id: ClipId, take: Option<TakeId>, all: bool },
    /// The take preview moved `by` takes, round from the last to the first.
    StepTakePreview { by: i32 },
    /// The take previewed put in the slot, one undo step, and the preview
    /// ended.
    PickPreviewedTake,
    /// The take preview ended, the document back as it was.
    EndTakePreview,
    /// Take `word` as right in this project (#3236): it goes on the
    /// brief's vocabulary, one undo step.
    SpellingIgnore { word: String },
    /// Take `word` as right in every project (#3236): it goes on the
    /// person's own word list, which no undo takes back.
    SpellingLearn { word: String },
    /// The language a title's texts are written in, for their spelling
    /// (#3236): a tag such as `de`, or `None` for the track's.
    SetTitleLanguage { clip_id: ClipId, language: Option<String> },
    /// Read the picture the node at `at` receives, at `(nx, ny)` (0..=1
    /// over the frame) of the parked frame: the qualifier eyedropper's and
    /// the Hue-Sat picker's sample. `at` names the node by path, in a
    /// clip's grade or in one of the sequence's own grades (the timeline
    /// grade, a grade group's stage); `clip_id` and `node_id` are the
    /// asker's names for it, given back in the answer,
    /// `AppEvent::NodeInputSampled` (#1428).
    SampleNodeInput { clip_id: ClipId, node_id: uuid::Uuid, at: klipp_graph_format::path::NodePath, nx: f32, ny: f32 },
    /// Set which scope mode the engine should render (0=waveform, 1=parade, 3=histogram).
    SetScopeRenderMode { mode: u8 },
    /// Set the union of scope modes the UI currently needs rendered per tick.
    /// Sum of (main dock active mode) + (every floating-window pane mode).
    /// The engine renders every mode in this set on each scope tick and writes
    /// the results into `FrameBuffer::scope_textures` keyed by mode.
    /// Sent on UI-side change only (e.g. opening the floating window, adding a
    /// pane, switching a pane's mode) — not every frame.
    SetActiveScopeModes { modes: std::collections::BTreeSet<u8> },
    /// The physical pixel size each video scope mode is shown at: the
    /// largest pane showing it. The engine draws its GPU scopes at this
    /// size, so a scope is never a small image stretched to fill its pane.
    /// A mode missing from the map is drawn at the default size. Sent on
    /// change only.
    SetScopeSizes { sizes: std::collections::BTreeMap<u8, (u32, u32)> },
    /// Push the scope settings (quality, channels, envelope, vectorscope
    /// window, histogram layout) to the engine. Sent on a UI-side change
    /// only, not every frame.
    SetScopeSettings { settings: crate::config::ScopeSettings },
    /// Push the scope bloom toggle to the engine. When enabled the engine
    /// runs a threshold + Gaussian + additive composite on the rendered
    /// waveform/parade scope textures so dense traces glow. Sent on
    /// UI-side change only.
    SetScopeBloom { enabled: bool },
    /// Whether the GPU waveform and parade measure a low-passed copy of
    /// the picture. Sent on UI-side change only.
    SetScopeLowPass { enabled: bool },
    /// Measure the picture after the project's viewing LUT, or before it
    /// (`None`), in SDR (#466). Sent on change only.
    SetScopeLut { lut: Option<ScopeLut> },
    /// Tell the engine whether the scope panel is actually visible
    /// in the UI. The engine uses this to skip the per-frame
    /// `linear_rgba_to_u8` re-encode and the GPU scope shader pass —
    /// both fire from `post_process_rgba` regardless of mode and
    /// cost ~12 ms / 1080p frame on CPU. Without the flag the
    /// engine has no way to know the scope panel is hidden and
    /// burns the budget on data nobody is looking at.
    SetScopesVisible(bool),
    /// Draw the scopes of an arbitrary sRGB RGBA frame (a media-browser
    /// hover skim, a timeline scrub) into `frame_buffer.scope_textures`,
    /// the way the playback pipeline draws them.
    RenderScopesForFrame { rgba: Vec<u8>, width: u32, height: u32 },
    /// Draw the scopes of an sRGB RGBA frame into `frame_buffer.
    /// snapshot_scope_textures`, a store separate from the live scopes, so
    /// an off-screen reader (the agent's `scope` tool) can measure any
    /// frame without disturbing what the viewer's scopes show. `modes` are
    /// 0 waveform, 1 parade, 2 vectorscope, 3 histogram.
    RenderScopeSnapshot { modes: Vec<u8>, rgba: Vec<u8>, width: u32, height: u32 },
    SetNodeParam { clip_id: ClipId, node_id: uuid::Uuid, param: String, value: f32 },
    /// Printer lights (#241): the selected clip's offset moved `points`
    /// printer points on red (0), green (1), blue (2) or all three (3).
    PrinterLight { channel: u8, points: i32 },
    /// Called when the user starts dragging a color-grading parameter
    /// (color wheel, sat/bri slider, scrubby slider, etc.). Snapshots the
    /// current node graph so the gesture can be committed as one undo entry.
    BeginNodeParamEdit { clip_id: ClipId },
    /// Called when the user releases a color-grading parameter drag. Commits
    /// the snapshot taken at the matching BeginNodeParamEdit as a single
    /// `SetNodeGraph` history entry covering the whole gesture.
    EndNodeParamEdit { clip_id: ClipId },
    /// Called when the user cancels a color-grading parameter drag
    /// (Escape): the graph goes back to the snapshot taken at the matching
    /// BeginNodeParamEdit, and no history entry is recorded.
    AbandonNodeParamEdit { clip_id: ClipId },
    SetNodeCurves { clip_id: ClipId, node_id: uuid::Uuid, curves: crate::color_grade::CurveSet },
    SetNodeHueSatCurves { clip_id: ClipId, node_id: uuid::Uuid, huesat: crate::color_grade::HueSatCurves },
    SetNodeQualifier { clip_id: ClipId, node_id: uuid::Uuid, qualifier: crate::color_grade::QualifierSettings },
    SetNodeLumaKey { clip_id: ClipId, node_id: uuid::Uuid, luma_key: crate::color_grade::LumaKeySettings },
    /// Replace the full `DifferenceKeySettings` on a DifferenceKey
    /// node — used by inspector slider drags + checkbox toggles.
    /// The plate-only file-picker path goes through
    /// `SetNodeDifferenceKeyPlate` instead so its delta is small
    /// enough to ride a single undo step without bundling the
    /// whole grade.
    SetNodeDifferenceKey { clip_id: ClipId, node_id: uuid::Uuid, difference_key: crate::color_grade::DifferenceKeySettings },
    /// Point a DifferenceKey node at a new clean-plate image (or
    /// clear it). Mirrors `SetExternalMattePath`'s shape — separate
    /// command so the file picker dispatches a small delta and the
    /// "Clear" button doesn't have to round-trip the entire
    /// settings struct.
    SetNodeDifferenceKeyPlate { clip_id: ClipId, node_id: uuid::Uuid, plate_path: Option<std::path::PathBuf> },
    SetNodeSoftClip { clip_id: ClipId, node_id: uuid::Uuid, soft_clip: crate::color_grade::SoftClipSettings },
    /// A Film Grain node's settings, whole (#894).
    SetNodeFilmGrain { clip_id: ClipId, node_id: uuid::Uuid, grain: crate::color_grade::FilmGrainSettings },
    SetNodeMasks { clip_id: ClipId, node_id: uuid::Uuid, masks: crate::color_grade::MaskStack },
    /// Store a computed colour-match result on a ColorMatch node. The
    /// UI runs `klipp_color::match_colors(source, reference)` against
    /// the reference frame the user captured, then dispatches this
    /// command with the resulting four-scalar `ColorMatchSettings`.
    /// The node's executor then applies the adjustments via the
    /// unified grade shader.
    SetNodeColorMatch { clip_id: ClipId, node_id: uuid::Uuid, settings: crate::color_grade::ColorMatchSettings },
    /// Point an ExternalMatte node at a new image file.
    SetExternalMattePath { clip_id: ClipId, node_id: uuid::Uuid, path: Option<std::path::PathBuf> },
    /// Track the subject through `clip_id` (#239): its grade gated by a
    /// matte that follows the subject, made in the background over every
    /// frame the render reads. `model` is "subject" (one detected person
    /// or thing) or "foreground" (everything in front). The model is
    /// downloaded only after the person agrees to it. `into` binds the matte
    /// to Grade or CutOut; None only continues the existing pass.
    TrackSubject { clip_id: ClipId, model: String, into: Option<SubjectMatteUse> },
    /// Restore the whole picture after a subject/image matte cut-out.
    RestoreBackground { clip_id: ClipId },
    /// Fade the clip's opacity across the frame by `mask`, a gradient
    /// (#2704); `None` takes it off.
    SetOpacityGradient { clip_id: ClipId, mask: Option<Box<crate::Mask>> },
    /// Stop tracking the subject through `clip_id`: the frames made so far
    /// are kept (#239).
    CancelTrackSubject { clip_id: ClipId },
    /// Track the subject a piece transition cuts round (#985 phase 3):
    /// the matte pass over just the frames of its subject side it reads,
    /// bound to nothing else. The model is downloaded only after the
    /// person agrees to it.
    TrackTransitionSubject { transition_id: crate::TransitionId },
    /// Tell the engine which nodes the V2 canvas currently has
    /// visible — the executor captures a downsampled thumbnail of
    /// each of them on the paused / post-seek re-grade so the canvas
    /// can paint per-node previews. Empty list disables thumb capture.
    /// Sent when the viewport's node set changes (pan / zoom / add /
    /// remove). Playback path skips capture entirely to avoid the N
    /// extra GPU readbacks per frame.
    /// Each node by its path in `clip_id`'s picture and its output (M3
    /// step 16a). `request` identifies the UI context; replies retain it
    /// so delayed results cannot populate a newer graph or frame.
    SetVisibleNodeThumbs { request: u64, clip_id: ClipId, at: Vec<klipp_graph_format::path::NodeOutput> },
    /// Render per-node thumbnails by running the clip's node graph on a
    /// caller-supplied pre-grade source bitmap (typically the filmstrip
    /// tile nearest the playhead). Used by the V2 canvas to populate
    /// node previews without waiting on a full-res decoder seek. Source
    /// is sRGB u8 RGBA at the given width × height; engine evaluates
    /// time-keyed grade params at `playhead` so keyframes remain
    /// frame-accurate even though the source bitmap came from a
    /// slightly different timestamp. `request` must match the active
    /// visible-node request. These results are marked as low-resolution.
    RenderNodeThumbsFromSource {
        request: u64,
        clip_id: ClipId,
        playhead: crate::RationalTime,
        src_rgba: Vec<u8>,
        src_w: u32,
        src_h: u32,
    },
    /// Change which channel of the ExternalMatte image becomes the
    /// matte signal (Alpha / Luminance / Red / Green / Blue) and
    /// whether the output is inverted.
    SetExternalMatteChannel { clip_id: ClipId, node_id: uuid::Uuid, channel: u8, invert: bool },
    /// Set how a `CombineMattes` node combines its two inputs. `op` is
    /// the discriminant index into `MatteCombineOp::ALL`, same shape as
    /// `SetExternalMatteChannel` above.
    SetMatteCombineOp { clip_id: ClipId, node_id: uuid::Uuid, op: u8 },
    /// Update one input slot on a `LayerMixer` node (port = index into
    /// `LayerMixerParams.inputs`). The handler grows the params Vec as
    /// needed so a fresh slot can be set without an explicit AddPort
    /// command.
    SetNodeLayerMixerInput {
        clip_id: ClipId,
        node_id: uuid::Uuid,
        port: u8,
        input: crate::color_grade::LayerMixerInput,
    },
    /// Capture the currently displayed program-viewer frame as the
    /// reference for the next ColorMatch compute. Stores raw RGBA in
    /// `AppState.color_match_reference`. Held in RAM only; the resulting
    /// `ColorMatchSettings` (4 floats) is what gets persisted on the
    /// node when `ApplyColorMatch` runs.
    CaptureColorMatchReference,
    /// Use a specific clip's representative frame as the colour-match
    /// reference. Replaces any previously captured reference. The
    /// handler decodes a mid-clip thumbnail so the user doesn't have
    /// to scrub the playhead onto the clip first.
    UseClipAsColorMatchReference { clip_id: ClipId },
    /// Run `match_colors` against the clip's mid-frame and the current
    /// colour reference, ensuring a ColorMatch node exists on the
    /// clip's graph and writing the resulting settings to it. No-op
    /// with a toast if no reference has been captured.
    MatchClipToColorReference { clip_id: ClipId },
    /// Lazy-create a Mask node (if absent) on the clip's graph and
    /// append a new shape to its mask stack. Used by the Color
    /// workspace "+ Add Mask" control so users who don't open the
    /// node canvas can still land a mask directly.
    AddMaskShape { clip_id: ClipId, shape: crate::color_grade::MaskShape },
    /// Remove mask at `mask_index` from the first Mask node on the
    /// clip's graph. No-op if index out of range.
    RemoveMaskShape { clip_id: ClipId, mask_index: usize },
    /// Move the mask at `from` in the Mask node `node_id`'s stack to
    /// `to` (#74): the order masks join in. Trackers bound to the masks
    /// keep following theirs. One undo step.
    MoveMaskShape { clip_id: ClipId, node_id: uuid::Uuid, from: usize, to: usize },
    /// Route inspector focus to the mask-editor view. Fired when the
    /// user interacts with a mask on the Program viewer (click a
    /// handle / grab to move) — without this the Edit-workspace user
    /// sees the circle overlay but clicking it doesn't bring up the
    /// mask params, because the Edit inspector defaults to Video /
    /// Info tabs. The handler sets the active inspector tab to Color
    /// so the Masks section is visible.
    FocusMaskInspector,
    /// Run `klipp_color::match_colors(source, reference)` against the
    /// last-captured reference frame and the clip's pre-grade frame
    /// at the playhead, then store the result on the named ColorMatch
    /// node via `SetNodeColorMatch`. No-op (with toast) when no
    /// reference has been captured yet.
    ApplyColorMatch { clip_id: ClipId, node_id: uuid::Uuid },
    /// Move the key of mask parameter `param` at `time_secs` to
    /// `new_time_secs`, its value to `value` when given (#1947). `param`
    /// "shape" is a freeform mask's shape keys, which move in time only.
    /// Refused where another key is. One step, or the drag's when inside
    /// `BeginNodeParamEdit`.
    MoveMaskKeyframe { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize, param: String, time_secs: f64, new_time_secs: f64, value: Option<f32> },
    /// Show the mask's keys on the clip's timeline key lane, its first
    /// keyed track (#1947). Layout only.
    ShowMaskKeysOnTimeline { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize },
    /// How mask parameter `param` leaves its key at `time_secs`: for the
    /// shape's keys Linear, Step (holds) or Bezier (eases) (#1947).
    SetMaskKeyframeInterpolation { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize, param: String, time_secs: f64, interpolation: crate::Interpolation },
    /// Add or update a keyframe on a mask parameter ("shape": the
    /// freeform shape as it is at that time).
    AddMaskKeyframe { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize, param: String, time_secs: f64, value: f32 },
    /// Remove a mask keyframe at a specific time.
    RemoveMaskKeyframe { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize, param: String, time_secs: f64 },
    /// Clear all keyframes for a mask parameter.
    ClearMaskKeyframes { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize, param: String },
    AddKeyframe { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, value: f32 },
    RemoveKeyframe { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64 },
    ClearKeyframes { clip_id: ClipId, node_id: uuid::Uuid, param: String },
    SetKeyframeInterpolation { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, interpolation: crate::keyframe::Interpolation },
    SetKeyframeTangents { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, tangent_in: (f64, f32), tangent_out: (f64, f32) },
    /// Ease a grade node's key at `time_secs`, as `EaseKeyAt` does by path.
    SetKeyframeEase { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, ease: crate::keyframe::KeyEase },
    /// Trigger a grade refresh without changing any params (used after bulk preset apply).
    RefreshGrade,
    ToggleColorPanel,
    ShowEffectsBrowser,
    ToggleBeforeAfter,
    /// Collapse the left browser to its rail, or bring it back.
    ToggleBrowserPanel,
    /// Collapse the right inspector to its rail, or bring it back.
    ToggleInspectorPanel,
    /// Toggle stabilization compare split view (raw vs stabilized).
    /// Mutually exclusive with ToggleBeforeAfter and grade still compare.
    ToggleStabilizationCompare,
    SetMattePreview(bool),

    /// What the viewer shows while grading for an HDR delivery — see
    /// [`HdrMonitor`]. Off is the SDR picture the viewer has always shown.
    SetHdrMonitor(HdrMonitor),
    /// Publish the HDR picture for `klipp-hdr-out` to show on a real HDR
    /// surface, or stop.
    ///
    /// Separate from [`Self::SetHdrMonitor`]: that decides what the
    /// *viewer's own* screen shows, and this whether a reference output is
    /// being fed. Both can be on — a colourist watches the preview while
    /// the reference monitor carries the real thing — and the picture
    /// published is the same HDR display light either way.
    SetHdrReferenceOutput(bool),

    /// Phase 4C — Inspector requests track-matte preview thumbnails for
    /// the given consumer clip. `None` clears the request. Engine emits
    /// `AppEvent::MatteMaskReady` per composited frame while a target
    /// is set; the inspector renders the latest as a small grayscale
    /// preview in the COMPOSITING section.
    SetInspectorMattePreviewTarget(Option<ClipId>),
    /// Render this clip alone, graded and placed, with nothing over or
    /// under it, until set back to `None`: what a colour measurement of
    /// the clip reads (#719). Engine-only.
    SetMeasureOnlyClip(Option<ClipId>),
    SetClipLogFormat { clip_id: ClipId, log_format: crate::types::LogFormat },
    /// How some media decodes has changed (its camera log format). Frames
    /// decoded before the change are stale — the engine's frame cache, its
    /// pre-grade snapshot and whatever the decoders read ahead — so the
    /// engine drops them and re-decodes at the playhead. Grade caches are
    /// unaffected: their key covers the clip's InputTransform node, which
    /// carries the format.
    MediaDecodeChanged,
    /// Auto white balance: analyze current frame and set temperature/tint on the clip's ColorBalance node.
    AutoWhiteBalance { clip_id: ClipId },
    /// Arm the viewer's white balance eyedropper for `clip_id`: the next
    /// click picks the grey the clip is balanced on
    /// (`RequestWhiteBalanceSample`).
    PickWhiteBalance { clip_id: ClipId },
    /// Arm the viewer's qualifier eyedropper for a Qualifier node: the
    /// next click keys the node on the colour picked
    /// (`RequestQualifierSample`).
    PickQualifierColour { clip_id: ClipId, node_id: uuid::Uuid },
    /// Arm the viewer for a 3D qualifier stroke (#895): a drag across the
    /// colours to keep, or with `remove` to take out.
    PickQualifierStroke { clip_id: ClipId, node_id: uuid::Uuid, remove: bool },
    /// A stroke drawn on the viewer, its points in frame fractions: sampled
    /// on the picture the armed node receives and added to its volume.
    QualifierStroke { points: Vec<[f32; 2]> },
    /// Arm the viewer's eyedropper for a Hue/Sat Curves node's `curve`
    /// (0 Hue vs Hue, 1 Hue vs Sat, 2 Hue vs Lum): the next click puts
    /// a range on that curve round the hue picked (#612).
    PickHueCurve { clip_id: ClipId, node_id: uuid::Uuid, curve: u8 },

    // Global LUT
    LoadGlobalLut { path: PathBuf },
    ClearGlobalLut,
    SetGlobalLutOpacity(f32),
    SetGlobalLutEnabled(bool),
    /// Switch LUT interpolation: true = tetrahedral, false = trilinear.
    SetLutTetrahedral(bool),
    /// The transfer the SDR viewer encodes for (see [`SdrTransfer`]).
    SetSdrTransfer(SdrTransfer),

    // Effects (OFX)
    AddEffect { clip_id: ClipId, plugin_name: String },
    RemoveEffect { clip_id: ClipId, effect_index: usize },
    SetEffectEnabled { clip_id: ClipId, effect_index: usize, enabled: bool },
    ReorderEffect { clip_id: ClipId, from_index: usize, to_index: usize },
    SetEffectParam { clip_id: ClipId, effect_index: usize, param_name: String, value: crate::OfxValue },

    // Built-in effects
    AddBuiltinEffect { clip_id: ClipId, effect: klipp_effects::BuiltinEffect },
    /// A LUT preset on a clip as its look (#3255): the `look` op, the
    /// LUT in the clip's grade replacing any look there, one undo step.
    ApplyClipLut { clip_id: ClipId, lut_path: PathBuf, name: String },
    RemoveBuiltinEffect { clip_id: ClipId, index: usize },
    ToggleBuiltinEffectEnabled { clip_id: ClipId, index: usize },
    UpdateBuiltinEffect { clip_id: ClipId, index: usize, effect: klipp_effects::BuiltinEffect },
    ReorderBuiltinEffect { clip_id: ClipId, from_index: usize, to_index: usize },

    // Effect presets
    SaveBuiltinEffectPreset { name: String, description: String, effect_key: String, effect: klipp_effects::BuiltinEffect },
    DeleteBuiltinEffectPreset { effect_key: String, name: String },

    // Render Queue
    ShowRenderQueue,
    ClearCompletedJobs,

    // Slip & Slide trim
    SlipClip {
        clip_id: ClipId,
        new_source_in: Timecode,
        new_source_out: Timecode,
    },
    SlideClip {
        clip_id: ClipId,
        new_timeline_in: Timecode,
        new_timeline_out: Timecode,
        left_clip_id: Option<ClipId>,
        new_left_out: Option<Timecode>,
        right_clip_id: Option<ClipId>,
        new_right_in: Option<Timecode>,
    },

    // Snap & Ripple
    /// Toggle the master snap gate. When `false`, every per-target
    /// snap (clip-edge, playhead, marker, in/out, grid) is disabled
    /// regardless of the per-target booleans.
    SetSnapMaster(bool),
    /// Toggle a single per-target snap kind. Has no effect when
    /// `SetSnapMaster(false)` was the most recent master toggle.
    SetSnapTarget {
        target: crate::SnapTargetKind,
        enabled: bool,
    },
    /// Set the grid-snap interval. Mirrored to `prefs.editing.
    /// grid_snap_interval` and to `state.grid_snap_interval` so the
    /// preferences pane and the ruler context menu stay in sync.
    SetGridInterval(crate::GridInterval),
    /// Pixel distance before snapping fires during a drag. Mirrors
    /// `prefs.editing.snap_threshold_px` ↔ `state.snap_threshold_px`.
    SetSnapThresholdPx(f32),
    /// Mouse-wheel / Ctrl+= zoom step multiplier. Mirrors
    /// `prefs.editing.zoom_step` ↔ `state.zoom_step`.
    SetZoomStep(f32),
    /// Toggle the magnetic timeline behaviour. Bridges the naming
    /// asymmetry: `prefs.editing.magnetic_timeline` (bool) ↔
    /// `state.magnetic_timeline_enabled` (bool).
    SetMagneticTimeline(bool),
    /// Default resolution preset for new sequences (index into
    /// `RESOLUTION_PRESETS`). Mirrors `prefs.editing.
    /// default_sequence_resolution_idx` ↔
    /// `state.default_sequence_resolution_idx`. Read at "+ new
    /// sequence" time, so mid-session changes need to propagate
    /// before the next click.
    SetDefaultSequenceResolutionIdx(usize),
    /// Default transition duration in frames. Mirrors
    /// `prefs.editing.default_transition_frames` ↔
    /// `state.default_transition_frames`.
    SetDefaultTransitionFrames(u32),
    /// How many frames Shift+Left and Shift+Right move, 0 for one
    /// second (#921). Mirrors `prefs.editing.shift_step_frames` ↔
    /// `state.shift_step_frames`.
    SetShiftStepFrames(u32),
    /// Whether multicam clips show angle-number overlays. Mirrors
    /// `prefs.editing.multicam_show_angles` ↔
    /// `state.multicam_show_angles`.
    SetMulticamShowAngles(bool),
    /// Whether clips show a strip where they use the same frames as
    /// another clip (#3272). Mirrors `prefs.editing.show_reused_footage`.
    SetShowReusedFootage(bool),
    SetRippleEnabled(bool),
    /// Whether appending a clip also brings its audio (source patching).
    SetAppendBringsAudio(bool),

    // Filmstrip
    /// Request more filmstrip detail for a time range (progressive refinement on zoom)
    RefineFilmstrip {
        media_id: MediaId,
        start_secs: f64,
        end_secs: f64,
        interval_secs: f64,
    },

    // Three-point edits
    InsertEdit,
    OverwriteEdit,
    /// The source In to Out into the sequence In to Out, its speed
    /// changed to fill it (#919).
    FitToFillEdit,
    /// The source on the first video track above the patched one that is
    /// free over its span, a new track when none is (#919).
    PlaceOnTopEdit,
    /// The clip under the playhead on the patched track takes the source,
    /// the source's playhead frame at the timeline's playhead; the clip
    /// keeps its place and length (#919).
    ReplaceEdit,
    /// The clip under the playhead on the patched track replaced by the
    /// source's marked range, which may be longer or shorter: what
    /// follows moves by the difference (#919).
    RippleOverwriteEdit,
    LiftEdit,
    ExtractEdit,

    // Transitions
    AddTransition { track_index: usize, clip_a_id: ClipId, clip_b_id: ClipId, kind_id: u8, duration_frames: u32 },
    RemoveTransition { track_index: usize, transition_id: crate::id::TransitionId },
    SetTransitionDuration { track_index: usize, transition_id: crate::id::TransitionId, duration_frames: u32 },
    /// The default transition (a cross dissolve, or on sound a crossfade)
    /// at the cut nearest the playhead (#918): on the selected clip's
    /// track when it is the right kind, else the nearest on any.
    AddDefaultTransition { audio: bool },
    SetTransitionKind { track_index: usize, transition_id: crate::id::TransitionId, kind_id: u8 },
    SetTransitionParam { track_index: usize, transition_id: crate::id::TransitionId, user_params: crate::types::TransitionUserParams },
    SetTransitionAlignment { track_index: usize, transition_id: crate::id::TransitionId, alignment: crate::types::TransitionAlignment },

    // Title clips
    CreateTitleClip { track_index: usize, timeline_in: Timecode, duration_frames: u32 },
    /// The one-gesture add: a fresh title on a free text track at the
    /// playhead (five seconds), or over the whole of `over_clip`. The
    /// clip is selected, the inspector opens on its text, the playhead
    /// parks where the title is at rest.
    AddTitleAtPlayhead { over_clip: Option<ClipId> },
    /// The Text tool's click or drag in the viewer (#1713): a title on a
    /// free text track at the playhead, its text layer centred at `x`, `y`
    /// (fractions of the frame), sized `box_size` when a box was drawn, and
    /// its text edited in place at once.
    CreateTitleInViewer { x: f32, y: f32, box_size: Option<(f32, f32)> },
    /// Insert a saved composition template as a precomposition clip at the
    /// current playhead. The app resolves the template metadata and records
    /// the resulting clip through the normal edit history.
    InsertCompositionTemplate { template_id: uuid::Uuid },
    /// Change one exposed value on an inserted composition template.
    SetTemplateOverride { clip_id: ClipId, parameter: String, value: TemplateValue },
    /// One link of a placed template's control (#2757), by the property
    /// it drives: unlinked, relinked, its offset reset, or set by hand.
    SetTemplateLink { clip_id: ClipId, property: String, action: crate::LinkAction },
    /// Save a composition as a reusable, locally installed template.
    SaveCompositionTemplate { sequence_id: uuid::Uuid, name: String },
    /// Open the dialog that saves a title clip as a user template in
    /// the Titles browser.
    SaveTitleTemplate { clip_id: ClipId },
    /// Save the selected clips together as a composition template: opens
    /// its dialog (#2757).
    SaveSelectionAsTemplate,
    /// Save nodes of a clip's picture together as a recipe: opens its
    /// dialog (#2757 step 6).
    SaveNodesAsRecipe { clip_id: ClipId, node_ids: Vec<uuid::Uuid> },
    /// Read the saved title templates again: one saved over the control
    /// socket joins the Titles browser (#2757).
    ReloadTitleTemplates,
    /// Add an image layer (a logo, a still) to a title. The layer's
    /// box takes the picture's aspect at a third of the frame width.
    AddTitleImageObject { clip_id: ClipId, path: String },
    /// Point an image layer at another file.
    SetTitleImagePath { clip_id: ClipId, object_id: u32, path: String },
    /// 0 = fit, 1 = fill, 2 = stretch.
    SetTitleImageFit { clip_id: ClipId, object_id: u32, fit: u8 },
    /// Promote an existing title clip to a reusable Project Asset.
    /// Reads the clip's `TitleData`, builds a `TitleAsset` keyed by a
    /// fresh `MediaId`, inserts a synthetic `MediaItem` into the pool
    /// (so it shows up in the Media tab), stores the asset in the
    /// project's `title_assets` map, and links the source clip to it.
    /// All future edits to any linked clip propagate via the asset.
    CreateTitleAsset { source_clip_id: ClipId },
    /// Detach a clip from its linked Project Asset. The clip's inline
    /// `TitleData` is kept (already in sync), `linked_title_asset_id`
    /// is cleared, and edits to this clip stop affecting the asset
    /// (and vice versa).
    DetachTitleAsset { clip_id: ClipId },
    /// Rename a Project Asset (the user-facing label in the Media tab
    /// and the Inspector banner). The `MediaId` stays stable so all
    /// linked instances remain linked.
    RenameTitleAsset { asset_id: MediaId, new_name: String },
    UpdateTitleData { clip_id: ClipId, text: String, font_size: f32, font_name: String, font_style: u8, color: [u8; 4] },
    /// Give a title every field of the brief style it follows again (#66).
    RevertTitleToStyle { clip_id: ClipId },
    /// Keep this title's look to paste on others (#1718).
    CopyTitleStyle { clip_id: ClipId },
    /// Give the titles `to` a look, each keeping its words, as one undo
    /// step (#1718): the title `from`'s, or the copied one's.
    PasteTitleStyle { from: Option<ClipId>, to: Vec<ClipId> },
    /// The Title tab's style eyedropper (#1718): the next title clicked in
    /// the timeline or viewer gives `target` its look. `None` puts it
    /// down.
    PickTitleStyle { target: Option<ClipId> },
    /// Give every other title made from the template this title was made
    /// from (or following its style) this title's look, each keeping its
    /// own words, as one undo step (#1716).
    UpdateTitlesLike { clip_id: ClipId },
    /// Put `replace` in place of `find` in the titles' text (#1717): in
    /// the active sequence, or every sequence with `project`; the
    /// subtitles too with `subtitles`; every match, or only `only`
    /// (a title, its text layer, and the match's bytes). One undo step
    /// in each sequence changed.
    ReplaceInTitles {
        find: String,
        replace: String,
        match_case: bool,
        whole_word: bool,
        subtitles: bool,
        project: bool,
        only: Option<(ClipId, u32, usize, usize)>,
    },
    SetTitleAlignment { clip_id: ClipId, alignment: u8 },  // 0=Left, 1=Center, 2=Right, 3=Justify
    /// Where a justified text box's paragraph ends (#1506):
    /// 0=Left, 1=Center, 2=Justify.
    SetTitleLastLine { clip_id: ClipId, last_line: u8 },
    /// Where a text box's text sits up and down the box (#1506):
    /// 0=Top, 1=Middle, 2=Bottom.
    SetTitleVerticalAlign { clip_id: ClipId, vertical_align: u8 },
    /// How a text box's text meets the box when it is too long (#1722):
    /// 0=Wrap, 1=Shrink (no smaller than `min_size`), 2=Grow.
    SetTitleFit { clip_id: ClipId, fit: u8, min_size: f32 },
    SetTitlePosition { clip_id: ClipId, x: f32, y: f32 },
    /// Replace one Title-object's transform wholesale. Used by the
    /// on-canvas editor (drag-to-move, drag handle-resize, drag
    /// rotation handle). The transform fields are normalised
    /// 0..1 frame-relative coords; rotation in degrees.
    SetTitleObjectTransform {
        clip_id: ClipId,
        object_id: u32,
        center_x: f32,
        center_y: f32,
        size_x: f32,
        size_y: f32,
        rotation: f32,
        anchor_x: f32,
        anchor_y: f32,
    },
    /// Select a Title-object on the on-canvas editor. `object_id`
    /// = `None` deselects (reverts to clip-level transform handles).
    SelectTitleObject { clip_id: ClipId, object_id: Option<u32> },
    /// Add a new object to the title's object stack. `kind`:
    /// 0 = TextBox (default styling, "New Text" placeholder),
    /// 1 = Shape (rectangle filling the centre of the frame),
    /// 3 to 8 = a ready-made shape (ellipse, triangle, hexagon, star,
    /// arrow, line; the UI's `inspector::title_shape_preset` maps them,
    /// #67). 2 is not used here: an image comes by `AddTitleImageObject`.
    /// The new object is appended at the top of the stack and
    /// becomes the selected object so the user can immediately
    /// drag / edit it.
    AddTitleObject { clip_id: ClipId, kind: u8 },
    /// Remove an object from a title. Selection clears if the
    /// removed object was the active one.
    DeleteTitleObject { clip_id: ClipId, object_id: u32 },
    /// A copy of a title layer just above it, selected (#1715).
    DuplicateTitleObject { clip_id: ClipId, object_id: u32 },
    /// Toggle the eye-icon visibility flag on an object.
    SetTitleObjectVisible { clip_id: ClipId, object_id: u32, visible: bool },
    /// Toggle the lock flag on an object — locked objects are
    /// skipped on canvas hit-test and resist on-canvas drag, but
    /// remain editable from the Inspector.
    SetTitleObjectLocked { clip_id: ClipId, object_id: u32, locked: bool },
    /// Set the per-object opacity (0..1). Multiplies into the
    /// alpha of every pixel this object contributes during the
    /// title's compose pass; independent from clip-level fades.
    SetTitleObjectOpacity { clip_id: ClipId, object_id: u32, opacity: f32 },
    AddTitleObjectOpacityKeyframe { clip_id: ClipId, object_id: u32, time_secs: f64, value: f32 },
    RemoveTitleObjectOpacityKeyframe { clip_id: ClipId, object_id: u32, time_secs: f64 },
    /// How a layer blends with the layers below it in its title (#1513).
    SetTitleObjectBlend { clip_id: ClipId, object_id: u32, blend: crate::BlendMode },
    /// Mask a layer with another layer of its title, by the source's
    /// id and what of it masks; `None` takes the mask off (#1513).
    SetTitleObjectMask { clip_id: ClipId, object_id: u32, mask: Option<(u32, crate::layer_mask::LayerMaskKind)> },
    /// Rename the layer-list label for an object.
    RenameTitleObject { clip_id: ClipId, object_id: u32, name: String },
    /// Reorder objects in the stack. `new_order` is the full list
    /// of object ids in the new back→front order; ids missing from
    /// the list are left at the end in their existing relative order
    /// (defensive against partial-update races).
    ReorderTitleObjects { clip_id: ClipId, new_order: Vec<u32> },
    /// Per-shape style mutations. Each command targets one Shape
    /// object inside a Title clip, identified by its object id.
    /// Used by the Inspector when a Shape layer is selected.
    TitlePathError { message: String },
    BeginTitlePathDrag { clip_id: ClipId },
    EndTitlePathDrag { clip_id: ClipId },
    SetTitleVectorPath { clip_id: ClipId, object_id: u32, path: crate::vector_path::VectorPath },
    SetTitleShapeFill { clip_id: ClipId, object_id: u32, color: Option<[u8; 4]> },
    SetTitleShapeStroke { clip_id: ClipId, object_id: u32, color: Option<[u8; 4]>, width: f32 },
    SetTitleShapeCornerRadius { clip_id: ClipId, object_id: u32, radius: f32 },
    /// Fit a backing shape to a text layer, with padding in 1080p pixels.
    SetTitleShapeFit { clip_id: ClipId, object_id: u32, target_id: Option<u32>, padding_x: f32, padding_y: f32 },

    /// Inline text edit: replace the text of a specific TextBox
    /// object (identified by id) with a new value. Used by the
    /// in-bbox text editor; doesn't touch font / size / colour /
    /// style — those flow through the existing `UpdateTitleData`
    /// or per-property commands so they stay separable from the
    /// keystroke-by-keystroke text mutation.
    SetTitleObjectText { clip_id: ClipId, object_id: u32, text: String },
    /// Same as SetTitleObjectTransform but also updates the primary
    /// TextBox's font_size — used by the corner-handle drag where
    /// the pro-NLE convention is "scale the font, not just the box".
    SetTitleObjectTransformAndFont {
        clip_id: ClipId,
        object_id: u32,
        center_x: f32,
        center_y: f32,
        size_x: f32,
        size_y: f32,
        rotation: f32,
        anchor_x: f32,
        anchor_y: f32,
        font_size: f32,
    },
    /// A drag on a title layer's handles in the viewer begins and ends
    /// (#1714): the title edits between are one undo step.
    BeginTitleObjectDrag { clip_id: ClipId },
    EndTitleObjectDrag { clip_id: ClipId },
    /// Where a text set on an arc starts, in degrees (0 = right, 90 =
    /// below), set by the viewer's path-start handle (#1714).
    SetTitleArcStart { clip_id: ClipId, object_id: u32, start_angle: f32 },
    SetTitleBackground { clip_id: ClipId, bg_kind: u8, color: [u8; 4] },  // 0=None, 1=Color, 2=FullFrame
    /// A title's box backing drawn one box per line or not (#3237): the
    /// boxes' corner radius and the gap between them (0 joins them), in
    /// pixels at 1080p. A title with no box gets one in `color`.
    SetTitleBackgroundLines { clip_id: ClipId, on: bool, radius: f32, gap: f32, color: [u8; 4] },
    /// A title's contrast protection (#2734): 0 off, 1 auto, 2 always.
    SetTitleContrastProtect { clip_id: ClipId, mode: u8 },
    SetTitleShadow { clip_id: ClipId, enabled: bool, color: [u8; 4], offset_x: f32, offset_y: f32, blur: f32 },
    /// Replace the entire drop-shadow stack. Each tuple is
    /// (color_rgba, offset_x, offset_y, blur). Existing per-index
    /// keyframe vecs are preserved in place when the new vec is at
    /// least as long; trimmed slots lose their keyframes.
    SetTitleShadows { clip_id: ClipId, shadows: Vec<([u8; 4], f32, f32, f32)> },
    /// Set the static fields on a single shadow layer at `index`,
    /// preserving its keyframe vecs. Used by the per-row inspector
    /// path so other layers' keyframes survive an inline edit.
    SetTitleShadowLayer {
        clip_id: ClipId,
        index: u32,
        enabled: bool,
        color: [u8; 4],
        offset_x: f32,
        offset_y: f32,
        blur: f32,
    },
    /// Soft-disable the secondary drop-shadow stack (`shadows[1..]`).
    /// Primary shadow (index 0) is unaffected.
    SetTitleShadowStackEnabled { clip_id: ClipId, enabled: bool },
    /// Set text outline. `None` = disable.
    SetTitleOutline { clip_id: ClipId, enabled: bool, color: [u8; 4], width: f32 },
    /// Set text glow. `enabled=false` = disable.
    SetTitleGlow { clip_id: ClipId, enabled: bool, color: [u8; 4], radius: f32, intensity: f32 },
    /// Replace the entire glow stack. Each tuple is
    /// (color_rgba, radius, intensity).
    SetTitleGlows { clip_id: ClipId, glows: Vec<([u8; 4], f32, f32)> },
    /// Set the static fields on a single glow layer at `index`,
    /// preserving its keyframe vecs.
    SetTitleGlowLayer {
        clip_id: ClipId,
        index: u32,
        enabled: bool,
        color: [u8; 4],
        radius: f32,
        intensity: f32,
    },
    /// Soft-disable for the secondary glow stack.
    SetTitleGlowStackEnabled { clip_id: ClipId, enabled: bool },
    /// Set gradient fill. `enabled=false` = disable, uses solid color instead.
    SetTitleGradient { clip_id: ClipId, enabled: bool, top_color: [u8; 4], bottom_color: [u8; 4] },
    /// Set a gradient fill of any number of stops, linear at an angle or
    /// radial (#1509): on the title's text when `object_id` is `None`,
    /// else on that shape. `stops` has at least two.
    SetTitleGradientStops {
        clip_id: ClipId,
        object_id: Option<u32>,
        enabled: bool,
        stops: Vec<crate::fill_gradient::GradientStop>,
        kind: crate::fill_gradient::GradientKind,
    },
    /// A gradient stop or the angle dragged: the steps written between
    /// the two are one undo step (#1509).
    BeginTitleGradientDrag { clip_id: ClipId },
    EndTitleGradientDrag { clip_id: ClipId },
    /// Set multiple outline layers (replaces single outline).
    /// Each tuple is (color_rgba, width_px).
    SetTitleOutlines { clip_id: ClipId, outlines: Vec<([u8; 4], f32)> },
    /// Set the static fields on a single outline layer at `index`,
    /// preserving its `width_keyframes`. Used by the per-row keyframe-
    /// aware scrubby in the Outline Layers stack — when no keyframes
    /// exist on that index, drag edits the static `width` here; when
    /// keyframes exist, the inspector dispatches the per-effect
    /// AddTitleObjectEffectKeyframe path instead.
    SetTitleOutlineLayer {
        clip_id: ClipId,
        index: u32,
        enabled: bool,
        color: [u8; 4],
        width: f32,
    },
    /// Soft-disable the secondary outline stack (`outlines[1..]`).
    /// Primary outline (index 0) is unaffected. The renderer skips
    /// stacked layers when `enabled=false` but the data is preserved.
    SetTitleOutlineStackEnabled { clip_id: ClipId, enabled: bool },
    /// Set inner shadow. `enabled=false` = disable.
    SetTitleInnerShadow { clip_id: ClipId, enabled: bool, color: [u8; 4], offset_x: f32, offset_y: f32, blur: f32 },
    /// Replace the entire inner-shadow stack. Each tuple is
    /// (color_rgba, offset_x, offset_y, blur). Mirrors SetTitleShadows.
    SetTitleInnerShadows { clip_id: ClipId, shadows: Vec<([u8; 4], f32, f32, f32)> },
    /// Set the static fields on a single inner-shadow layer at `index`,
    /// preserving its keyframe vecs.
    SetTitleInnerShadowLayer {
        clip_id: ClipId,
        index: u32,
        enabled: bool,
        color: [u8; 4],
        offset_x: f32,
        offset_y: f32,
        blur: f32,
    },
    /// Soft-disable for the secondary inner-shadow stack.
    SetTitleInnerShadowStackEnabled { clip_id: ClipId, enabled: bool },
    /// Set pattern fill. pattern_kind: 0=None, 1=HStripes, 2=VStripes, 3=Checker, 4=Diagonal
    SetTitlePattern { clip_id: ClipId, pattern_kind: u8, color_a: [u8; 4], color_b: [u8; 4], size: f32 },
    /// Set per-character tracking (extra spacing in px).
    SetTitleTracking { clip_id: ClipId, tracking: f32 },
    /// The entrance and exit motion of the targeted text layer.
    SetTitleMotion { clip_id: ClipId, motion: crate::title_motion::TitleMotion },
    /// Set published field `index` of title `clip_id` to `value` (#1139):
    /// a template's text, colour or speed. `done` false while a drag is
    /// under way, shown at once and recorded as one step when it ends.
    SetTitleField { clip_id: ClipId, index: usize, value: TitleFieldValue, done: bool },
    /// Read a credits list (#1514) from `path` into the open credits
    /// dialog: what its Load button picked.
    LoadCreditsList { path: std::path::PathBuf },
    /// Star a font family in the font picker, or unstar it (#1719).
    ToggleFavouriteFont { family: String },
    /// A font family was just picked: first of the recent ones.
    NoteRecentFont { family: String },
    /// Every title naming the font `from` set to `to`, one step a
    /// sequence (#1720).
    ReplaceFont { from: String, to: String },
    /// Open the missing-fonts dialog: every missing font, or `family`'s.
    ShowMissingFonts { family: Option<String> },
    /// Set write-on reveal progress (0.0-1.0).
    SetTitleWriteOn { clip_id: ClipId, write_on: f32 },
    /// Set line spacing multiplier.
    SetTitleLineSpacing { clip_id: ClipId, line_spacing: f32 },
    /// One of a text box's paragraph settings (#1721), by its key
    /// (`title_effect_keys::TEXT_SPACE_BEFORE` and on), in pixels at 1080p.
    SetTitleParagraph { clip_id: ClipId, key: String, value: f32 },
    /// A text box's tab stops in place of its own (#1721): each at pixels
    /// at 1080p from the start indent, and 0=Left, 1=Center, 2=Right,
    /// 3=Decimal.
    SetTitleTabStops { clip_id: ClipId, stops: Vec<(f32, u8)> },
    /// A text box's paragraphs as a list (#1721): 0=None, 1=Bullets,
    /// 2=1 2 3, 3=a b c, 4=A B C, 5=i ii iii, 6=I II III; the items' text
    /// `indent` pixels at 1080p past the start indent. A count keeps the
    /// start and suffix it had.
    SetTitleList { clip_id: ClipId, style: u8, indent: f32 },
    /// The fewest words a text box's wrapped paragraphs end on (#1721);
    /// 0 for no rule.
    SetTitleMinLastWords { clip_id: ClipId, words: u8 },
    /// Toggle text underline rendering.
    SetTitleUnderline { clip_id: ClipId, underline: bool },
    /// The weight the text box is set in, `None` for its style's, and
    /// a variable font's axes, each a tag and a value (#1504).
    SetTitleFontVariation { clip_id: ClipId, weight: Option<u16>, axes: Vec<(String, f32)> },
    /// The OpenType features the text box is shaped with, all of them,
    /// in HarfBuzz syntax ("smcp", "-liga", "ss01") (#1710).
    SetTitleFeatures { clip_id: ClipId, features: Vec<String> },
    /// Toggle text strikethrough rendering.
    SetTitleStrikethrough { clip_id: ClipId, strikethrough: bool },
    /// Soft-disable for the Pattern Fill subcard. `pattern_fill` is an
    /// `Option<TextPattern>` enum without a place to add a struct-level
    /// `enabled` flag, so the toggle bit lives in `tb.pattern_fill_enabled`
    /// alongside the data. Disabling preserves the chosen pattern.
    SetTitlePatternEnabled { clip_id: ClipId, enabled: bool },
    /// Apply a named style preset to the selected TextBox layer. The
    /// preset is *flattened* into the underlying fields (colour, glow,
    /// outline, shadow, chromatic aberration, glitch, …) — there is no
    /// "currently-applied preset" recorded on the data; the inspector
    /// dropdown is a one-shot quick-style picker rather than a slot.
    /// Preset ids are kept in a single registry inside the inspector
    /// so the handler can match on them; see PRESET_REGISTRY.
    ApplyTitlePreset { clip_id: ClipId, preset_id: u8 },
    /// Soft-disable for the Texture Fill subcard. `texture_fill_path` is
    /// `Option<String>`; the toggle bit is `tb.texture_fill_enabled`.
    /// Disabling preserves the path so the user's selection survives.
    SetTitleTextureEnabled { clip_id: ClipId, enabled: bool },
    /// Set the pre-layout case transform applied to the text body.
    /// `case_kind`: 0 = None, 1 = Uppercase, 2 = Lowercase, 3 = SmallCaps.
    SetTitleCaseTransform { clip_id: ClipId, case_kind: u8 },
    /// Vertical baseline shift in 1080p reference pixels (positive
    /// shifts text DOWN). Resolution-stable: scales with frame height
    /// at render time.
    SetTitleBaselineShift { clip_id: ClipId, shift_px: f32 },
    /// Drop-cap scale factor for the first character. `0.0` = off,
    /// `>= 1.0` = enlarge first char by this factor (3.0 = three-line
    /// drop initial). Body text continues at the same baseline.
    SetTitleDropCap { clip_id: ClipId, scale: f32 },
    /// Chromatic aberration in 1080p reference pixels. `0.0` = off.
    SetTitleChromaticAberration { clip_id: ClipId, offset_px: f32 },
    /// Glitch intensity 0..1 (random horizontal slice displacement).
    /// `0.0` = off; `1.0` = max chaos.
    SetTitleGlitch { clip_id: ClipId, intensity: f32 },
    /// One-click "neon" preset: rewrites the textbox with a bright
    /// fill, a strong outer glow tinted to match, and a thin dark
    /// outline so the text reads against any background. The
    /// `accent_color` is the neon hue; saturation and value are
    /// pinned for legibility regardless of what the user picks.
    SetTitleNeonPreset { clip_id: ClipId, accent_color: [u8; 4] },
    /// Number of columns to flow body text into (1 = single column).
    SetTitleColumns { clip_id: ClipId, columns: u8 },
    /// Toggle vertical (CJK-style) character stacking.
    SetTitleVerticalLayout { clip_id: ClipId, vertical: bool },
    /// Set vertical text's columns from the left (`true`) or, as
    /// Japanese and Chinese are set, from the right (#1507).
    SetTitleVerticalColumnsLtr { clip_id: ClipId, left_to_right: bool },
    /// Set text direction (0 = Ltr, 1 = Rtl).
    SetTitleDirection { clip_id: ClipId, direction: u8 },
    /// Set 3D text extrusion. `enabled=false` = disable.
    SetTitleExtrusion { clip_id: ClipId, enabled: bool, depth: f32, angle: f32, face_color: [u8; 4], side_color: [u8; 4], lighting: f32 },
    /// Set text path. path_kind: 0=None, 1=Arc, 2=Circle, 3=Wave.
    /// For Arc: p1=radius, p2=start_angle, p3=sweep
    /// For Circle: p1=radius
    /// For Wave: p1=amplitude, p2=frequency, p3=phase
    SetTitleTextPath { clip_id: ClipId, path_kind: u8, p1: f32, p2: f32, p3: f32 },
    /// Set the title's text on a drawn path (#1508): contour `contour`
    /// of the path layer `layer` (an object id in the title), at
    /// `offset` (0..1) along it, `align` 0 start, 1 centre, 2 end, and
    /// `flip` to the path's other side. The offset's keys stay while the
    /// text stays on the same layer.
    SetTitleTextPathLayer { clip_id: ClipId, layer: u32, contour: usize, offset: f32, align: u8, flip: bool },
    /// Add a path for the title's text to follow, an unpainted S across
    /// the text's place, set the text on it and select it (#1508).
    DrawTitleTextPath { clip_id: ClipId },
    /// Set per-character animation. entrance: 0=None, 1=FadeIn, 2=SlideUp, 3=SlideDown, 4=ScaleUp.
    /// Set enabled=false to disable entirely.
    /// Set image texture fill path. `path: None` = disable texture fill.
    SetTitleTextureFill { clip_id: ClipId, path: Option<String> },
    /// Switch the texture source to one of the bundled procedural
    /// presets. preset_id maps to TexturePresetId (0=PaperGrain,
    /// 1=Scanlines, 2=DiagonalHatch, 3=GradientSweep). The other
    /// TextureFill params (scale / offset / etc.) are preserved.
    SetTitleTexturePreset { clip_id: ClipId, preset_id: u8 },
    /// Set every transform / blend param on the texture fill in one
    /// command. tile_mode is u8 (0=Stretch, 1=Tile, 2=Fit, 3=Cover).
    /// Source field is preserved; this command never changes the
    /// File path or Preset id.
    SetTitleTextureParams {
        clip_id: ClipId,
        tile_mode: u8,
        scale: f32,
        offset_x: f32,
        offset_y: f32,
        rotation: f32,
        opacity: f32,
        tint: [u8; 4],
    },
    /// Set motion blur. `enabled=false` = disable.
    SetTitleMotionBlur { clip_id: ClipId, enabled: bool, shutter_angle: f32, samples: u8 },
    /// Apply a style property to a character range (rich text style runs).
    /// `property`: "font_size", "color", "font_name", "font_style", "tracking", "baseline_offset",
    /// "font_weight", "font_axes" (#1504), "features", "underline" (#1710)
    /// `value`: JSON-encoded value for the property ("font_name" takes
    /// the family as it is, #1505).
    ApplyTitleStyle {
        clip_id: ClipId,
        range_start: usize,
        range_end: usize,
        property: String,
        value: String,
    },
    /// Clear all style runs — reset to uniform base style.
    ClearTitleStyleRuns { clip_id: ClipId },
    /// Update the title animation fade duration stored in AppState.
    SetTitleFadeDuration { duration: f32 },
    ClearTitleOverlay,
    /// Replace the loops of the primary text layer: what it keeps
    /// doing while on screen (blink, pulse, shake, wave, colour
    /// cycle, scroll, crawl, counter).
    SetTitleLoops { clip_id: ClipId, loops: Vec<crate::title_motion::TitleLoop> },
    /// The targeted text layer's animators (#1510), all of them: a preset
    /// added, one removed or one edited is one step.
    SetTitleAnimators { clip_id: ClipId, animators: Vec<crate::title_motion::Animator> },
    /// The targeted text layer's 3D settings (#1512), whole; `None` takes
    /// 3D off it altogether.
    SetTitleText3d { clip_id: ClipId, three_d: Option<crate::text3d::Text3d> },
    /// The targeted text layer's metal face (#2706), whole; `None` takes
    /// it off. Its colour becomes what a frame without a GPU shows.
    SetTitleMaterial { clip_id: ClipId, material: Option<crate::title_material::TitleMaterial> },
    /// Set the targeted title layer's edge fade: how it fades out
    /// towards the frame's edges, by where it is on screen (#592).
    SetTitleEdgeFade { clip_id: ClipId, edge_fade: crate::title_motion::EdgeFade },

    // Linked clips
    ImportMediaLinked {
        media_id: MediaId,
        /// When `true`, append on freshly created tracks above the
        /// existing stack (Shift+double-click escape hatch). When
        /// `false`, append onto V1/A1 (the FCP-style default).
        on_new_tracks: bool,
    },
    UnlinkClip { clip_id: ClipId },
    LinkClips { clip_a: ClipId, clip_b: ClipId },

    // Markers
    AddMarker { position: Timecode, label: String, color: MarkerColor },
    /// A marker with everything said at once and its id chosen by the
    /// sender: the control socket's `add_marker`, which answers with the
    /// id so the marker can be named later.
    AddMarkerWith {
        marker_id: MarkerId,
        position: Timecode,
        duration: Option<Timecode>,
        label: String,
        comment: String,
        color: MarkerColor,
        kind: MarkerKind,
    },
    RemoveMarker { marker_id: MarkerId },
    UpdateMarkerLabel { marker_id: MarkerId, label: String },
    UpdateMarkerColor { marker_id: MarkerId, color: MarkerColor },
    UpdateMarkerComment { marker_id: MarkerId, comment: String },
    UpdateMarkerKind { marker_id: MarkerId, kind: MarkerKind },
    /// Set a sequence marker's range duration. `None` converts the marker
    /// back to a point marker; `Some(dur)` promotes it to a range spanning
    /// `position..position + dur`.
    SetMarkerDuration { marker_id: MarkerId, duration: Option<Timecode> },
    JumpToMarker { marker_id: MarkerId },
    /// Move a sequence marker to `position`: a drag of its flag on the
    /// ruler, let go (#909).
    MoveMarker { marker_id: MarkerId, position: Timecode },
    /// Connect a clip to the primary clip nearest its start (G).
    ConnectToPrimary { clip_id: ClipId },
    /// Disconnect a connected clip from its primary clip (Shift+G).
    DisconnectFromPrimary { clip_id: ClipId },
    /// Promote the clip's connected track to a storyline, or break a
    /// storyline apart (Shift+S on a connected clip).
    ToggleStoryline { clip_id: ClipId },
    /// Zoom the timeline to the selected clips (Shift+Z).
    ZoomToSelection,
    /// Select the edit point nearest the playhead, both sides (#913).
    SelectNearestEditPoint,
    /// The selected edit points' next side: both, outgoing, incoming.
    CycleEditSide,
    /// Trim the selected edit points by `frames` (later when positive),
    /// as one undo step.
    TrimEditPoints { frames: i64 },
    /// Move the selected edit points to the playhead (extend edit).
    ExtendEditPoints,
    /// Dynamic trimming (#914): J or L with an edit point selected plays
    /// from its cut; this arms the trim there.
    BeginDynamicTrim,
    /// K during a dynamic trim: the selected edit points go to where the
    /// playhead stopped, one undo step.
    CommitDynamicTrim,
    /// Esc during a dynamic trim: playback stops and the playhead goes
    /// back to the cut, nothing trimmed.
    CancelDynamicTrim,
    /// Play around the selected edit point (or the playhead), two seconds
    /// each side, looping until stopped (#914).
    PlayAroundEdit,
    /// Move the selected clips by `frames` (later when positive), as one
    /// undo step (#917).
    NudgeClips { frames: i64 },
    /// Move the selected clips to the next track of their kind above (or
    /// below), at the same time (#917).
    MoveClipsToTrack { up: bool },

    // Per-clip markers
    AddClipMarker { clip_id: ClipId, time_secs: f64, name: String, color: MarkerColor },
    RemoveClipMarker { clip_id: ClipId, index: usize },

    // Dynamic tracks
    AddVideoTrack,
    AddAudioTrack,
    /// Add a new subtitle track with an explicit name.
    /// Used by the subtitle-editor "+ Add Subtitle Track" and multi-file import.
    AddSubtitleTrack { name: String },
    /// Sync auto_create_tracks preference to AppState.
    SyncAutoCreateTracks(bool),
    /// Insert a new track of the same kind above the given track index.
    AddTrackAbove { track_index: usize },
    /// Insert a new track of the same kind below the given track index.
    AddTrackBelow { track_index: usize },
    RemoveTrack { track_id: TrackId },
    RenameTrack { track_id: TrackId, name: String },
    /// A track's sync lock on or off (#915): on, a ripple on another track
    /// moves its clips too.
    SetTrackSyncLock { track_id: TrackId, on: bool },
    /// How a track's automation is played and recorded (#2306).
    SetTrackAutomationMode { track_index: usize, mode: crate::automation::AutomationMode },
    /// A track's own keys of one control, edited in its automation lane
    /// (#2306).
    SetTrackAutomationKeys { track_index: usize, param: crate::automation::TrackParam, keys: Vec<crate::Keyframe> },
    /// A track's control played at `value` in place of its keys and its
    /// value as set, while the user moves it or a pass records it, or back
    /// to them (`None`) (#2306): the engine's only.
    OverrideTrackAutomation { track_index: usize, param: crate::automation::TrackParam, value: Option<f32> },
    /// A subtitle track's language, a BCP 47 tag, or none (#2223).
    SetTrackLanguage { track_id: TrackId, language: Option<String> },
    /// A track's targeting on or off (#915): edit-point navigation,
    /// paste, split at playhead and match frame act on targeted tracks.
    SetTrackTargeted { track_id: TrackId, on: bool },
    /// A click on a track's badge (#2940): it becomes where source edits
    /// of its kind go, targeted (alone of its kind while every track of it
    /// was) with the source patched to it; targeted already, it is
    /// untargeted and loses the patch. See `Sequence::target_from_badge`.
    TargetTrackFromBadge { track_id: TrackId },
    /// Patch the source's picture or sound to a track (#915), or, off,
    /// take the patch away so source edits use the first track of the
    /// kind again.
    SetTrackSourcePatch { track_id: TrackId, on: bool },
    /// Keep the track's levels and own effects to paste onto others
    /// (Copy Track Attributes, #846).
    CopyTrackAttributes { track_id: TrackId },
    /// Paste the copied track attributes onto `track_ids`, the parts
    /// asked for (levels: volume, pan, opacity; picture: effects and
    /// grade; sound: the audio chain), as one undo step.
    PasteTrackAttributes { track_ids: Vec<TrackId>, levels: bool, picture: bool, sound: bool },
    /// A picture effect by name (as the `effect` op names them) onto a
    /// track itself, after its clips' own; one of a kind replaces it.
    AddTrackVideoEffect { track_id: TrackId, kind: String },
    /// An audio effect (an `effect_type`) at the end of a track's chain.
    AddTrackAudioEffect { track_id: TrackId, effect_type: String },
    /// Switch one of a track's own effects on or off: its picture effects,
    /// or (`audio`) its audio chain, by index.
    ToggleTrackEffect { track_id: TrackId, audio: bool, index: usize },
    /// Take one of a track's own effects off, by index.
    RemoveTrackEffect { track_id: TrackId, audio: bool, index: usize },
    /// Take all of a track's own effects and its grade off.
    ClearTrackEffects { track_id: TrackId },
    /// Reset the per-track waveform amplitude zoom back to 1.0 (#50).
    ResetWaveformZoom { track_id: TrackId },

    // Audio sync (#61)
    /// Mark a clip as the reference for future `AlignToAudioReference` calls.
    SetAudioReference { clip_id: ClipId },
    /// Cross-correlate the given clip's audio with the current reference clip
    /// and shift its timeline position to align them.
    AlignToAudioReference { clip_id: ClipId },
    /// F-160 — bin-scoped variants of the two above. The handler resolves
    /// `media_id` to the first timeline clip using that media and
    /// forwards to `SetAudioReference` / `AlignToAudioReference`. If no
    /// clip uses this media, emits a toast and no-ops.
    SetAudioReferenceByMedia { media_id: MediaId },
    AlignToAudioReferenceByMedia { media_id: MediaId },
    /// Open the Auto Align Audio dialog for a camera clip's sound and a
    /// separate recording (#615).
    OpenAutoAlignAudio { reference: ClipId, recording: ClipId },
    /// Move `recording` so its sound lines up with `reference`'s, by
    /// waveform (`by_timecode` false) or by the files' start timecodes.
    /// Nothing moves on a failed or weak match (#615).
    AutoAlignAudio { reference: ClipId, recording: ClipId, by_timecode: bool },

    // Audio recording (#60)
    /// Arm / un-arm a track for recording. Only one track is armed at a time
    /// for the MVP — arming track X un-arms all others.
    ArmTrack { track_id: TrackId, armed: bool },
    /// Toggle the per-track expandable automation lane on the audio track at
    /// `track_idx`. The lane shows the active rubber-band param's curve at
    /// full lane height — DaVinci Fairlight / Premiere "Show Track Keyframes"
    /// style. Currently-collapsed → opens with the global rubber-band param;
    /// already-open → collapses. State persists with the project file.
    ToggleAudioAutomationLane { track_idx: usize },
    /// Switch the param shown in a track's automation lane without
    /// closing it. Used by the lane's right-click param picker so
    /// users can flip between Volume / Pan / Gain without first
    /// collapsing the lane and changing the global rubber-band pill.
    /// `track`: the track's own keys of `param` (#2306), not the clips'.
    SetAudioAutomationLaneParam { track_idx: usize, param: AudioParam, track: bool },
    /// Begin capturing audio from the configured input device into a WAV file
    /// in the proxy cache. Does nothing if no track is armed.
    StartRecording,
    /// Stop the active recording, close the WAV file, and auto-import it.
    StopRecording,

    // Media pool management
    RemoveFromPool { media_id: MediaId },
    ConfirmRemoveFromPool { media_id: MediaId },
    SetMediaRating { media_id: MediaId, rating: u8 },
    /// What the editor logged about a clip: scene, shot, take,
    /// description, comments, keywords, good take, reel.
    SetMediaLog { media_id: MediaId, log: crate::ClipLog },
    /// A note, favourite stretch or rejected one on a pool clip.
    AddMediaMarker { media_id: MediaId, marker: crate::MediaMarker },
    RemoveMediaMarker { media_id: MediaId, index: usize },
    RenameMediaMarker { media_id: MediaId, index: usize, name: String },
    /// Pair a camera clip with a separate recording: by timecode when
    /// both carry one, by the sound otherwise.
    SyncMediaPair { video_id: MediaId, audio_id: MediaId },
    /// Undo the pairing; the clip goes back to its own sound.
    ClearSyncedAudio { video_id: MediaId },

    SetMediaTags { media_id: MediaId, tags: Vec<String> },
    RevealInFileManager { path: PathBuf },
    /// Open a media clip in an external application (GIMP / Krita / Audacity / etc).
    /// The app is chosen from `ExternalToolsPrefs` based on media type and falls
    /// back to `xdg-open` when the configured field is empty.
    OpenInExternalApp { media_id: MediaId },
    RevealMediaInBrowser { media_id: MediaId },
    SetMediaColorLabel { media_id: MediaId, label: u8 },
    DeleteUnusedMedia,
    ConfirmDeleteUnusedMedia,
    /// Export despite offline media (user confirmed from warning dialog).
    ForceExportSequence { settings: Box<ExportSettings> },
    /// Bypasses the source-overwrite guard. Sent by the warning
    /// dialog's "Overwrite Anyway" button when the user explicitly
    /// confirms they want to write the export to the same path as
    /// a clip in the project's media pool.
    ForceExportSequenceOverwrite { settings: Box<ExportSettings> },

    // Bins
    CreateBin { name: String },
    /// Save a smart bin: a new one, or the one with this id over again.
    SaveSmartBin { bin: crate::SmartBin },
    DeleteSmartBin { id: String },
    /// Create a child bin inside an existing bin.
    CreateSubBin { parent_id: BinId, name: String },
    /// Import a detected image sequence as a single video clip.
    ImportImageSequence {
        pattern: String,   // ffmpeg pattern e.g. "/path/frame_%04d.png"
        start_frame: u32,
        frame_count: u32,
        fps: f64,
    },
    DeleteBin { name: String },
    MoveItemToBin { media_id: MediaId, bin: Option<String> },
    /// Move these items into `bin`, or out of their bins with `None`, as
    /// one undo step.
    MoveItemsToBin { media_ids: Vec<MediaId>, bin: Option<String> },
    /// Make a bin with a name no other bin has, move these items into it
    /// (none for an empty bin) and show it in the media pool, its name
    /// ready to type over.
    NewBin { media_ids: Vec<MediaId> },
    RenameBin { old_name: String, new_name: String },
    /// Rename a single MediaItem — updates `MediaItem.filename` only. The
    /// source file on disk (`MediaItem.path`) is never touched.
    RenameMediaItem { media_id: MediaId, new_filename: String },
    /// Apply a set of filename changes atomically as one undo step. The UI
    /// computes `(id, new_filename)` pairs from the preview; the handler
    /// snapshots the old names from the current pool to build the undo
    /// record.
    BatchRenameMediaItems { changes: Vec<(MediaId, String)> },
    /// Open the batch-rename modal seeded with the given media ids.
    /// Dispatched from the bin context menus where `AppState` is not
    /// directly accessible.
    OpenBatchRenameDialog { media_ids: Vec<MediaId> },

    // Browser skimming
    StartSkimTranscode { media_id: MediaId },
    /// Force-regenerate skim transcode (deletes existing cache first).
    RegenerateSkimTranscode { media_id: MediaId },
    /// Force-regenerate filmstrip thumbnails (deletes existing cache first).
    RegenerateFilmstrip { media_id: MediaId },
    /// Decode a single skim frame at `time_secs` for `media_id`. `target`
    /// selects which viewer the resulting frame is destined for so the
    /// upload path can route it correctly: timeline scrub / filmstrip
    /// hover = `Program`; media-browser bin hover = `Source`. The
    /// returned `BrowserSkimFrameReady` event carries the same target.
    BrowserSkimFrame { media_id: MediaId, time_secs: f64, target: ViewerMode },

    // Browser audio skimming
    /// Begin an audio skim session for a media item (mouse enters skimmable clip).
    BrowserSkimAudioStart {
        media_id: MediaId,
        source_path: PathBuf,
        time_secs: f64,
    },
    /// Begin an audio skim session for a file that has NOT been imported into
    /// the media pool (no `MediaId`, no PCM cache). Used by the file browser
    /// hover preview where the user is just exploring a directory. The audio
    /// is decoded on the fly via ffmpeg into the same 48 kHz stereo f32 PCM
    /// the existing skim feeder thread expects.
    BrowserSkimAudioStartFromPath {
        source_path: PathBuf,
        time_secs: f64,
    },
    /// Update audio skim position (mouse moves while skimming).
    BrowserSkimAudioUpdate {
        media_id: MediaId,
        time_secs: f64,
    },
    /// End the audio skim session (mouse leaves clip or skimming disabled).
    BrowserSkimAudioEnd,
    /// Seek the active skim/audition session to an absolute position
    /// in seconds and keep playing. Distinct from
    /// `BrowserSkimAudioUpdate` because that one runs the cursor-speed
    /// heuristic (no movement → silence), which is wrong for the FX
    /// audition scrubber where the user clicks once and expects
    /// playback to continue from the new position.
    AuditionSeek {
        time_secs: f64,
    },
    /// Start the FX audition: same machinery as `BrowserSkimAudioStart`
    /// (cached PCM via media_id), but takes exclusive ownership of the
    /// skim session so the cursor-skim paths (file/media browser
    /// hover, timeline scrubbing) can't clobber it with `silence()`
    /// calls when the cursor stops moving.
    AuditionStart {
        media_id: MediaId,
        source_path: PathBuf,
        time_secs: f64,
    },
    /// End the FX audition. Releases the audition lock and stops the
    /// session, mirroring `BrowserSkimAudioEnd`.
    AuditionEnd,
    /// Update the audition session's playback volume. Range 0.0–1.5
    /// in practice (1.0 = unity). The skim feeder picks this up on
    /// the next chunk; no-op if no audition session is active.
    AuditionSetVolume(f32),

    // UI
    SetViewerMode(ViewerMode),
    SetActiveTool(Tool),
    ToggleFullscreen,
    ShowPreferences,
    /// Capture the whole window, panels and all, to a PNG at `path`, no
    /// wider than `max_width`: what the person sees, for the control
    /// socket's `window_screenshot`.
    CaptureWindow { path: std::path::PathBuf, max_width: Option<u32> },

    // Track locking
    SetTrackLocked { track_id: TrackId, locked: bool },
    /// Freeze a video track, or play it live again (#3207): frozen, it
    /// plays from frames drawn ahead where they are current.
    SetTrackFrozen { track_id: TrackId, frozen: bool },
    /// Lock a track while it's frozen, or not (#3207).
    SetTrackLockWhileFrozen { track_id: TrackId, on: bool },
    /// Open the Bounce to Media File dialog for a track (#3264).
    OpenBounceDialog { track_id: TrackId },
    /// Bounce a track to a media file (#3264): drawn alone at full size
    /// with its alpha, and its linked sound when `sound`, in `format`; the
    /// clip of it placed on a new track above, the original hidden, as one
    /// undo step once the file is written.
    BounceTrack { track_id: TrackId, format: BounceFormat, sound: bool },

    /// Set (or clear) a track's custom pixel height. `None` restores the
    /// kind's default height. Height is clamped to (32.0, 300.0).
    SetTrackCustomHeight { track_id: TrackId, height: Option<f32> },
    /// Set the per-track automation-lane height. `None` resets to the
    /// default lane height. Mirrors `SetTrackCustomHeight` for the
    /// clip area.
    SetTrackAutomationLaneHeight { track_id: TrackId, height: Option<f32> },

    // Clip grouping
    GroupClips { clip_ids: Vec<ClipId> },
    UngroupClips { group_id: GroupId },
    UngroupClip { clip_id: ClipId },

    // Parameter modifiers (behaviors)
    AddModifier { clip_id: ClipId, param: ParamId, modifier: Modifier },
    RemoveModifier { clip_id: ClipId, param: ParamId, modifier_index: usize },
    UpdateModifier { clip_id: ClipId, param: ParamId, modifier_index: usize, modifier: Modifier },
    ToggleModifier { clip_id: ClipId, param: ParamId, modifier_index: usize },

    // Application
    Quit,

    // Audio keyframes
    /// Inspector edit selecting each linked clip's nearest key in the displayed frame.
    /// Exact curve/agent mutations continue to use Add/RemoveAudioKeyframe.
    EditAudioKeyframeAtFrame { clip_id: ClipId, param: AudioParam, time_secs: f64, value: Option<f32> },
    AddAudioKeyframe { clip_id: ClipId, param: AudioParam, time_secs: f64, value: f32 },
    RemoveAudioKeyframe { clip_id: ClipId, param: AudioParam, time_secs: f64 },
    MoveAudioKeyframe { clip_id: ClipId, param: AudioParam, index: usize, new_time_secs: f64, new_value: f32 },
    SetAudioKeyframeInterpolation { clip_id: ClipId, param: AudioParam, keyframe_index: usize, interpolation: crate::keyframe::Interpolation },
    SetAudioKeyframeTangent { clip_id: ClipId, param: AudioParam, keyframe_index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)>, handles: Option<crate::keyframe::Handles> },

    // Video keyframes
    AddVideoKeyframe { clip_id: ClipId, param: VideoParam, time_secs: f64, value: f32 },
    RemoveVideoKeyframe { clip_id: ClipId, param: VideoParam, time_secs: f64 },
    MoveVideoKeyframe { clip_id: ClipId, param: VideoParam, index: usize, new_time_secs: f64, new_value: f32 },
    SetVideoKeyframeInterpolation { clip_id: ClipId, param: VideoParam, keyframe_index: usize, interpolation: crate::keyframe::Interpolation },
    SetVideoKeyframeTangent { clip_id: ClipId, param: VideoParam, keyframe_index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)>, handles: Option<crate::keyframe::Handles> },

    // Per-effect title keyframes — animate any keyframable param on
    // any TitleObject's effect (TextGlow radius/intensity today, more
    // effects to follow). `param_key` matches a constant in
    // `crate::title_effect_keys`; the handler debug_asserts on
    // unknown keys.
    AddTitleObjectEffectKeyframe {
        clip_id: ClipId,
        object_id: u32,
        param_key: String,
        time_secs: f64,
        value: f32,
    },
    RemoveTitleObjectEffectKeyframe {
        clip_id: ClipId,
        object_id: u32,
        param_key: String,
        time_secs: f64,
    },
    MoveTitleObjectEffectKeyframe {
        clip_id: ClipId,
        object_id: u32,
        param_key: String,
        index: usize,
        new_time_secs: f64,
        new_value: f32,
    },
    /// Adjust one or both Bezier handles of a title layer keyframe.
    SetTitleObjectEffectKeyframeTangent {
        clip_id: ClipId,
        object_id: u32,
        param_key: String,
        keyframe_index: usize,
        tangent_in: Option<(f64, f32)>,
        tangent_out: Option<(f64, f32)>,
        /// The key's handle kind, set before the handles given
        /// ([`crate::keyframe::edit_handles`]).
        handles: Option<crate::keyframe::Handles>,
    },
    /// Key a title layer's colour at `time_secs` on all four of its
    /// channels at once, or (`color` None) take that key from all four:
    /// one step (#1511). `param_key` names the colour (`text.color`).
    SetTitleObjectColourKeyframe {
        clip_id: ClipId,
        object_id: u32,
        param_key: String,
        time_secs: f64,
        color: Option<[u8; 4]>,
    },
    SetTitleObjectEffectKeyframeInterpolation {
        clip_id: ClipId,
        object_id: u32,
        param_key: String,
        keyframe_index: usize,
        interpolation: crate::keyframe::Interpolation,
    },

    // Title-object position keyframes — per-layer animation independent
    // from the clip-level transform_pos_*_keyframes that move the whole
    // rendered title image.
    AddTitleObjectPositionKeyframe {
        clip_id: ClipId,
        object_id: u32,
        axis: crate::keyframe::TitlePositionAxis,
        time_secs: f64,
        value: f32,
    },
    AddTitleObjectTransformKeyframe {
        clip_id: ClipId,
        object_id: u32,
        property: TitleTransformProperty,
        time_secs: f64,
        value: f32,
    },
    RemoveTitleObjectTransformKeyframe {
        clip_id: ClipId,
        object_id: u32,
        property: TitleTransformProperty,
        time_secs: f64,
    },
    RemoveTitleObjectPositionKeyframe {
        clip_id: ClipId,
        object_id: u32,
        axis: crate::keyframe::TitlePositionAxis,
        time_secs: f64,
    },
    MoveTitleObjectPositionKeyframe {
        clip_id: ClipId,
        object_id: u32,
        axis: crate::keyframe::TitlePositionAxis,
        index: usize,
        new_time_secs: f64,
        new_value: f32,
    },
    /// Adjust one or both Bezier handles of a title layer keyframe.
    SetTitleObjectPositionKeyframeTangent {
        clip_id: ClipId,
        object_id: u32,
        axis: crate::keyframe::TitlePositionAxis,
        keyframe_index: usize,
        tangent_in: Option<(f64, f32)>,
        tangent_out: Option<(f64, f32)>,
        /// The key's handle kind, set before the handles given
        /// ([`crate::keyframe::edit_handles`]).
        handles: Option<crate::keyframe::Handles>,
    },
    SetTitleObjectPositionKeyframeInterpolation {
        clip_id: ClipId,
        object_id: u32,
        axis: crate::keyframe::TitlePositionAxis,
        keyframe_index: usize,
        interpolation: crate::keyframe::Interpolation,
    },

    // Keyframe clipboard operations
    CopyAudioKeyframes { clip_id: ClipId, param: AudioParam },
    CopyVideoKeyframes { clip_id: ClipId, param: VideoParam },
    PasteAudioKeyframes { clip_id: ClipId, param: AudioParam },
    PasteVideoKeyframes { clip_id: ClipId, param: VideoParam },

    // Reverse keyframes
    ReverseAudioKeyframes { clip_id: ClipId, param: AudioParam },
    ReverseVideoKeyframes { clip_id: ClipId, param: VideoParam },

    // Effect keyframes
    AddEffectKeyframe { clip_id: ClipId, effect_index: usize, param_name: String, time_secs: f64, value: f32 },
    RemoveEffectKeyframe { clip_id: ClipId, effect_index: usize, param_name: String, time_secs: f64 },
    MoveEffectKeyframe { clip_id: ClipId, effect_index: usize, param_name: String, keyframe_index: usize, new_time_secs: f64, new_value: f32 },
    SetEffectKeyframeInterpolation { clip_id: ClipId, effect_index: usize, param_name: String, keyframe_index: usize, interpolation: crate::keyframe::Interpolation },

    // Clip role
    SetClipRole { clip_id: ClipId, role: crate::ClipRole },
    /// How far back a clip sits for the sequence camera (`Clip::depth`).
    /// Undoable; a drag of the value is one undo step.
    SetClipDepth { clip_id: ClipId, depth: f32 },
    /// Clips given their own name on the timeline (#920); `None` or a
    /// blank name gives back their media's. One undo step.
    RenameClips { clip_ids: Vec<ClipId>, name: Option<String> },
    /// Clips given their own colour label (#920), on the media pool's
    /// scale (0 none, then red, orange, yellow, green, blue, purple).
    /// One undo step.
    SetClipColourLabels { clip_ids: Vec<ClipId>, label: u8 },
    /// Flag where each clip's grade stands (#899), one undo step.
    SetClipGradeFlags { clip_ids: Vec<ClipId>, flag: crate::GradeFlag },
    /// A new grade group named `name`, with `clip_ids` in it (#237). One
    /// undo step.
    CreateGradeGroup { name: String, clip_ids: Vec<ClipId> },
    /// `clip_ids` into grade group `group`, or out of any with `None`.
    SetClipsGradeGroup { clip_ids: Vec<ClipId>, group: Option<crate::GradeGroupId> },
    RenameGradeGroup { group: crate::GradeGroupId, name: String },
    /// Remove a grade group and its grades; its clips keep their own.
    DeleteGradeGroup { group: crate::GradeGroupId },
    /// The sequence camera's shutter: motion blur on everything that moves.
    /// `shutter_deg` 0 is off. Undoable.
    SetSequenceShutter { sequence_idx: usize, shutter_deg: f32, samples: u8 },
    /// How bright the sequence's titles are in an HDR delivery, cd/m²
    /// (`Sequence::title_nits`), held inside `TITLE_NITS_RANGE`. Undoable.
    SetSequenceTitleNits { sequence_idx: usize, nits: f32 },
    /// Give the node at `node` a colour label, 1 to 6 as a clip's are
    /// numbered, or take it off with 0 (#1753). A node the colour page
    /// edits through a stand-in clip names it by that clip; it is kept
    /// under the grade it stands in for. Undoable.
    SetNodeColourLabel { node: klipp_graph_format::path::NodePath, colour: u8 },

    /// One of the sequence camera's moving values, keyed at a moment.
    /// Where it looks, how far in, and its roll — the move itself,
    /// which until now could only be set through the `camera` op over
    /// the control socket. Undoable, as one camera snapshot.
    SetSequenceCameraKey { sequence_idx: usize, param: CameraParam, time_secs: f64, value: f32 },
    /// The key at that moment, taken away again. Undoable.
    RemoveSequenceCameraKey { sequence_idx: usize, param: CameraParam, time_secs: f64 },
    /// A key in one of the camera's lists carried to another moment and
    /// value, named by its index in that list: what a drag on the
    /// sequence lane or in the curve editor emits every frame. Undoable
    /// as one camera snapshot, unless a `BeginSequenceCameraDrag` is
    /// open, in which case the drag's end records the one entry.
    MoveSequenceCameraKey { sequence_idx: usize, param: CameraParam, index: usize, new_time_secs: f64, new_value: f32 },
    /// How the curve leaves a camera key: the interpolation of the key
    /// at `index` and, as the clip keyframe commands do, of the key
    /// before it, so the segment either side of the diamond takes the
    /// shape. Undoable.
    SetSequenceCameraKeyInterpolation { sequence_idx: usize, param: CameraParam, index: usize, interpolation: crate::keyframe::Interpolation },
    /// A camera key's Bezier handles, either or both. Undoable as a
    /// camera snapshot, or folded into an open drag.
    SetSequenceCameraKeyTangent { sequence_idx: usize, param: CameraParam, index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)> },
    /// The move into the camera key at `index` given a named ease, the
    /// same curves the `camera` op writes. Undoable.
    EaseSequenceCameraKey { sequence_idx: usize, param: CameraParam, index: usize, ease: crate::keyframe::Ease },
    /// A drag on the camera's keys has begun: the camera is snapshotted
    /// once, and the moves and handle edits until the matching end
    /// collapse into one undo entry.
    BeginSequenceCameraDrag { sequence_idx: usize },
    /// The drag has ended; one entry from the snapshot to the camera as
    /// it now is, or none if nothing changed.
    EndSequenceCameraDrag { sequence_idx: usize },

    // Clip speed
    /// A clip's speed, negative for reverse, its linked sound with it.
    /// With `ripple` the clips after it on their tracks move by the change
    /// in length; without it, one it would grow into refuses it (#639).
    SetClipSpeed { clip_id: ClipId, speed: f64, ripple: bool },
    /// How a slowed or ramped clip's in-between frames are drawn: "sample"
    /// repeats the nearest, "blend" cross-fades, "flow" follows the motion
    /// (#2773), as the retime and ramp ops' `interpolation`.
    SetClipInterpolation { clip_id: ClipId, interpolation: String },
    /// Hold `clip_id`'s frame at `at_secs` on the timeline for `secs`,
    /// inserted there, the rest moved on (#2773).
    FreezeFrame { clip_id: ClipId, at_secs: f64, secs: f64 },

    // Speed keyframes (variable speed ramp)
    /// Add a speed keyframe to a clip at the given time.
    AddSpeedKeyframe { clip_id: ClipId, time_secs: f64, speed: f64 },
    /// Remove a speed keyframe by index.
    RemoveSpeedKeyframe { clip_id: ClipId, index: usize },
    /// Update an existing speed keyframe's time and value.
    UpdateSpeedKeyframe { clip_id: ClipId, index: usize, time_secs: f64, speed: f64 },
    /// Replace all speed keyframes (used by presets).
    SetSpeedKeyframes { clip_id: ClipId, keyframes: Vec<crate::Keyframe> },
    /// Begin drag for speed keyframe undo coalescing.
    BeginSpeedKeyframeDrag { key: String, clip_id: ClipId },
    /// End drag for speed keyframe undo coalescing.
    EndSpeedKeyframeDrag { key: String, clip_id: ClipId },

    // Source rotation override (0, 90, 180, 270 degrees)
    SetSourceRotation { clip_id: ClipId, degrees: i32 },

    // Clip audio
    UpdateClipAudio {
        clip_id: ClipId, gain: f32, volume: f32, pan: f32, fade_in_secs: f32, fade_out_secs: f32,
        fade_in_curve: crate::types::FadeCurve, fade_out_curve: crate::types::FadeCurve,
        channel_config: crate::types::ChannelConfig,
    },
    BeginAudioDrag { key: String, clip_id: ClipId, old_gain: f32, old_volume: f32, old_pan: f32, old_fade_in: f32, old_fade_out: f32 },
    EndAudioDrag { key: String, clip_id: ClipId, new_gain: f32, new_volume: f32, new_pan: f32, new_fade_in: f32, new_fade_out: f32 },

    // Scale keyframe timing
    ScaleAudioKeyframeTiming { clip_id: ClipId, param: AudioParam, factor: f64 },
    ScaleVideoKeyframeTiming { clip_id: ClipId, param: VideoParam, factor: f64 },

    // Insert keyframe bump pattern
    InsertAudioKeyframeBump { clip_id: ClipId, param: AudioParam, center_time: f64, bump_value: f32, base_value: f32, duration_secs: f64, ramp_secs: f64 },
    InsertVideoKeyframeBump { clip_id: ClipId, param: VideoParam, center_time: f64, bump_value: f32, base_value: f32, duration_secs: f64, ramp_secs: f64 },

    // Undo coalescing — drag gesture signals for keyframe moves/tangents
    BeginKeyframeDrag { key: String, clip_id: ClipId },
    EndKeyframeDrag { key: String, clip_id: ClipId },

    // Ken Burns presets
    ApplyKenBurns {
        clip_id: ClipId,
        start_scale: f32,
        end_scale: f32,
        start_pos_x: f32,
        start_pos_y: f32,
        end_pos_x: f32,
        end_pos_y: f32,
    },
    /// A clip's Frame Glide set, changed or (with `None`) taken off
    /// (#611): one undo step. Position or scale animation the clip has of
    /// its own is replaced only with `replace_animation`; without it the
    /// edit is refused and says so.
    SetFrameGlide { clip_id: ClipId, glide: Option<crate::frame_glide::FrameGlide>, replace_animation: bool },
    /// The clip whose Frame Glide is being framed in the viewer, or none
    /// (#611).
    SetFrameGlideMode(Option<ClipId>),
    ClearTransformKeyframes { clip_id: ClipId },
    ClearVideoParamKeyframes { clip_id: ClipId, param: VideoParam },

    /// Bake the current grade chain of `clip_id` to a 3D LUT and write it
    /// to `output_path` as Adobe .cube format. `lut_size` is the cube edge
    /// length (17 = preview, 33 = broadcast standard, 65 = high-quality).
    /// Spatial nodes (Qualifier / Mask / matte ops) are skipped during
    /// bake — only the per-pixel grade chain is captured. Runs CPU-only on
    /// a fresh executor; live state untouched.
    BakeGradeToLut {
        clip_id: ClipId,
        output_path: PathBuf,
        lut_size: usize,
    },

    /// Persist the user's chosen LUT cube edge length for the next bake.
    /// Decoupled from BakeGradeToLut so the inspector dropdown can update
    /// the AppState without immediately running a bake.
    SetBakeLutSize(usize),

    // Audio ducking
    AutoDuckAudio {
        target_clip_id: ClipId,
        /// Track index of the dialogue/sidechain source
        sidechain_track_idx: usize,
        /// How much to reduce volume (0.0 = silence, 0.3 = reduce to 30%)
        duck_level: f32,
        /// How quickly to ramp down in seconds
        attack_secs: f32,
        /// How quickly to ramp back up in seconds
        release_secs: f32,
        /// Threshold: audio level above which ducking activates (0.0-1.0)
        threshold: f32,
    },

    // Magnetic timeline ops
    RippleTrimHead { clip_id: ClipId },
    RippleTrimTail { clip_id: ClipId },
    InsertGapAtPlayhead,
    /// Insert a silence / gap clip on a specific track at the playhead.
    /// Distinct from `InsertGapAtPlayhead` (which routes through the
    /// magnetic editor and lands on the primary track only). Used for
    /// "Insert N seconds of silence on this audio track" workflows
    /// where the user has a specific track in mind and a non-default
    /// duration. Subsequent clips on the target track ripple right.
    InsertGapOnTrack { track_idx: usize, duration_secs: f64 },
    /// Q — trim the OUT edge of the clip whose OUT is at or before the playhead
    /// to the playhead position. Picks target by proximity; no selection required.
    TrimOutToPlayhead,
    /// W — trim the IN edge of the clip whose IN is at or after the playhead
    /// to the playhead position. Picks target by proximity; no selection required.
    TrimInToPlayhead,

    // Audio DSP effects
    AddAudioEffect { clip_id: ClipId, effect_type: String },
    RemoveAudioEffect { clip_id: ClipId, effect_index: usize },
    UpdateAudioEffectParam { clip_id: ClipId, effect_index: usize, param_name: String, value: f32 },
    ToggleAudioEffect { clip_id: ClipId, effect_index: usize },
    SetAudioEffectWetDry { clip_id: ClipId, effect_index: usize, wet_dry: f32 },
    ReorderAudioEffect { clip_id: ClipId, from_index: usize, to_index: usize },
    /// Learn a Broadband Denoiser noise profile from the clip's source
    /// audio. `at_clip_secs` is a clip-relative start (the playhead);
    /// `None` searches the clip for its quietest stretch.
    LearnDenoiserProfile { clip_id: ClipId, effect_index: usize, at_clip_secs: Option<f64> },
    /// Set several params of one audio effect as a single undo step
    /// (presets, learned profiles).
    SetAudioEffectParams { clip_id: ClipId, effect_index: usize, params: Vec<(String, f32)> },

    // Undo coalescing — drag gesture signals for builtin effects
    BeginEffectDrag {
        key: String,
        clip_id: ClipId,
        effect_index: usize,
        old_effect: klipp_effects::BuiltinEffect,
    },
    EndEffectDrag {
        key: String,
        clip_id: ClipId,
        effect_index: usize,
        new_effect: klipp_effects::BuiltinEffect,
    },
    /// Take a drag gesture back without recording it: the effect
    /// returns to what it was when the gesture opened, or, when the
    /// gesture added it (`added`), is removed again along with the
    /// history entry for the add. For a gesture that ended where it
    /// began, or lost its release (Escape, the pointer leaving the
    /// window, the gizmo hidden mid-drag).
    AbandonEffectDrag {
        clip_id: ClipId,
        effect_index: usize,
        added: bool,
    },

    // Video stabilization
    AnalyzeStabilization { clip_id: ClipId },
    CancelStabilization { clip_id: ClipId },
    /// Measure a clip's per-frame light, for the Deflicker effect.
    AnalyzeDeflicker { clip_id: ClipId },
    CancelDeflicker { clip_id: ClipId },

    // Scene (shot-boundary) detection
    DetectScenes { clip_id: ClipId },
    /// Timeline → Detect Scene Cuts (#616): find the picture changes in
    /// the clip's part of its file and split it there, its linked sound
    /// with it, as one undoable edit. No markers are left behind.
    DetectSceneCuts { clip_id: ClipId },
    CancelSceneDetect { clip_id: ClipId },
    /// Join Through Edit (#683): the clip and the piece of the same shot
    /// after it (`next`) or before it made one again, its linked sound
    /// with it, one undo step.
    JoinThroughEdit { clip_id: ClipId, next: bool },
    /// Razor-split a clip at every scene marker it carries, atomically.
    /// Undo restores the original single clip in one step.
    SplitAtSceneMarkers { clip_id: ClipId },

    // Point tracking
    /// Add a new track point to a clip at the given normalized position.
    AddTrackPoint { clip_id: ClipId, x: f64, y: f64 },
    /// One-shot "track this mask region" workflow: takes the mask's
    /// current centre (evaluated at the playhead), spawns a track
    /// point there, kicks off analysis, and on completion auto-links
    /// the resulting positions back into the mask's centre keyframes.
    /// Closes the 3-step Place / Analyze / Link dance into a single
    /// gesture for the common case "I want this mask to follow the
    /// thing under it".
    /// `perspective` follows the window's four corners on a flat surface
    /// instead of a move, size and turn (#238).
    TrackMaskRegion { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize, perspective: bool },
    /// Follow a flat surface in a clip, a phone screen, a sign, a monitor,
    /// through the clip in perspective (#684), from `corners` (top left,
    /// top right, bottom right, bottom left, fractions of the frame as it
    /// is shown, the clip's fit and placement included) as they are at
    /// timeline second `at_secs` (the playhead when `None`). With
    /// `surface`, that surface is tracked again from these corners, and
    /// whatever is pinned to it follows the new track.
    ///
    /// `direction` 1 or -1 follows only later or only earlier frames and
    /// lays the result over the surface's track, keeping the rest: how a
    /// track lost partway is continued. 0 follows both ways and replaces
    /// it. `interactive` is a person at the viewer: where the surface is
    /// lost, the playhead goes there with the corners up to correct.
    ///
    /// `track` is the area followed when it isn't the corners, a larger
    /// part of the same plane; `exclude` are parts of it left out
    /// (#2754), both as the corners are given. Left out on a surface the
    /// clip has, the surface's own are carried to this frame.
    TrackSurface { clip_id: ClipId, corners: [[f32; 2]; 4], at_secs: Option<f64>, name: Option<String>, surface: Option<uuid::Uuid>, direction: i8, interactive: bool, track: Option<[[f32; 2]; 4]>, exclude: Option<Vec<Vec<[f32; 2]>>> },
    /// Correct a tracked surface on one frame (#2754): `corners` (as
    /// [`AppCommand::TrackSurface`] takes them) are where `surface` is at
    /// timeline second `at_secs` (the playhead when `None`). The key
    /// there becomes a correction set by hand, and the track either side
    /// is solved again from it, as far as the anchors next to it (the
    /// reference frame or another correction): forward and backward and
    /// blended, so the rest of the track is kept.
    CorrectSurface { clip_id: ClipId, surface: uuid::Uuid, corners: [[f32; 2]; 4], at_secs: Option<f64> },
    /// The viewer's surface tool on a clip (#684 phase 2): four corners to
    /// set on the surface, then Track. With `surface`, they start where
    /// that surface is at the playhead, to correct and track it again.
    /// Measure the water under the clip's reflection again (#2795): its
    /// Re-analyze, for a measurement that is outdated or looks wrong.
    MeasureWater { clip_id: ClipId },
    OpenSurfaceTool { clip_id: ClipId, surface: Option<uuid::Uuid> },
    /// Find the four corners of the flat surface round `click` (fractions
    /// of the frame as shown) in the clip's picture at timeline second
    /// `at_secs` (the playhead when `None`), in the background (#684). The
    /// viewer's surface tool takes them; with `then_track` they are
    /// tracked from at once, which is how the assistant's click works.
    /// A quad found is checked against the frames after it: it must move
    /// as one plane (#2754). `request_id` names the request whose outcome
    /// the control socket waits for in [`ControlShared::surface_snap`].
    SnapSurface { clip_id: ClipId, click: [f32; 2], at_secs: Option<f64>, then_track: bool, name: Option<String>, request_id: Option<uuid::Uuid> },
    /// Take a tracked surface off a clip, one undo step; what is pinned to
    /// it stays where it was on the reference frame.
    RemoveSurface { clip_id: ClipId, surface: uuid::Uuid },
    /// Remove the tracker bound to this mask plus the mask's centre
    /// keyframes — gives the user a clean "untrack" gesture without
    /// leaving an orphan tracker behind. No-op when no tracker is
    /// linked to the mask.
    RemoveMaskTracking { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize },
    /// Remove a track point by index.
    RemoveTrackPoint { clip_id: ClipId, index: usize },
    /// Analyze a track point (forward and backward from current frame).
    AnalyzeTrackPoint { clip_id: ClipId, point_index: usize },
    /// Cancel track analysis.
    CancelTrackAnalysis { clip_id: ClipId },
    /// Toggle tracking placement mode.
    ToggleTrackingMode,
    /// Generate keyframes from track point data to a target parameter.
    GenerateKeyframesFromTrack { clip_id: ClipId, point_index: usize, target: TrackTarget },

    // Grade versioning
    /// Save the current node_graph as a new named grade version.
    SaveGradeVersion { clip_id: ClipId, name: String },
    /// Switch to a different grade version by index.
    SwitchGradeVersion { clip_id: ClipId, version_index: usize },
    /// Delete a grade version by index.
    DeleteGradeVersion { clip_id: ClipId, version_index: usize },
    /// Rename a grade version.
    RenameGradeVersion { clip_id: ClipId, version_index: usize, name: String },

    // Looks (project-wide reusable colour grades)
    /// Snapshot the selected clip's node graph as a new project-level
    /// Look. The handler reads the clip's current graph; this command
    /// just supplies the user-facing name.
    SaveLook { name: String },
    /// Apply a saved Look to one or more target clips. The Look's
    /// node graph replaces each target's `node_graph` (with fresh
    /// node IDs via `remap_ids`).
    /// With `append`, the look runs after each clip's own grade rather
    /// than replacing it, so a clip's balance survives the look.
    ApplyLook { look_id: uuid::Uuid, clip_ids: Vec<ClipId>, append: bool },
    DeleteLook { look_id: uuid::Uuid },
    RenameLook { look_id: uuid::Uuid, new_name: String },

    // Grade stills (reference frame comparison)
    /// Capture the current viewer frame as a grade reference still.
    /// Keep `clip_id`'s whole grade in the grade library as `name`, with
    /// its graded thumbnail, for every project (#899).
    SaveGradeToLibrary { clip_id: ClipId, name: String },
    /// Put the library's grade `name` on `clip_ids`: in place of each one's
    /// grade, or after it with `append`, as one undo step.
    ApplyGradeFromLibrary { name: String, clip_ids: Vec<ClipId>, append: bool },
    /// Put `clip_id`'s grade on the other pieces of its shot that have no
    /// grade of their own (the neighbours a split left, carrying on in the
    /// same file), as one undo step.
    ApplyGradeToShotPieces { clip_id: ClipId },
    /// Take `name` out of the grade library.
    DeleteGradeFromLibrary { name: String },
    /// Label the library's grade `name` with the camera and profile it
    /// was made for, and notes, to find it by (#614); empty ones take its
    /// labels off. A label grades nothing by itself.
    SetGradeLibraryLabels { name: String, camera: String, profile: String, notes: String },
    CaptureGradeStill { name: String },
    /// Delete a saved grade still by index.
    DeleteGradeStill { index: usize },
    /// Put the grade a still was grabbed with on these clips, as one undo
    /// step. Each gets its own node ids. With `append` it runs after each
    /// clip's own grade instead of replacing it.
    ApplyGradeFromStill { index: usize, clip_ids: Vec<ClipId>, append: bool },
    /// Set the active grade still for comparison.
    SetActiveGradeStill { index: Option<usize> },
    /// Toggle split-view comparison with the active grade still.
    ToggleGradeStillCompare,
    /// Wipe the viewer against another clip's graded frame (#899): the
    /// clip before or after the one graded on the timeline, or a clip
    /// named; `None` ends the wipe.
    WipeAgainstClip { target: Option<WipeClip> },

    // ── Source Viewer playback ─────────────────────────────────────
    /// Bind a media file to the source viewer. Opens a persistent
    /// `VideoDecoder` (and audio source if the file has audio) so the
    /// transport can drive real playback without re-decoding per seek.
    /// `path` is the raw media file; `media_id` is the MediaPool id —
    /// engine uses the path directly to avoid a MediaPool lookup on
    /// the engine thread. `source_rotation` is media-metadata rotation
    /// (0 / 90 / 180 / 270) so portrait sources display upright.
    SetSourceMedia {
        media_id: MediaId,
        path: std::path::PathBuf,
        source_rotation: i32,
    },
    /// Begin source-viewer playback. Auto-pauses the timeline so the
    /// audio mixer doesn't fight: at most one viewer is the active
    /// audio owner at any moment.
    PlaySource,
    /// Pause source-viewer playback. Decoder stays open; Play resumes
    /// from the current source playhead.
    PauseSource,
    /// Step the source viewer forward (positive) or backward (negative)
    /// by `frames` source frames. Mirrors timeline `SeekRelative` for
    /// J/K/L → arrow-key parity in the source viewer.
    StepSource(i64),
    /// Tear down the source viewer's decoder and audio. Frees the
    /// NVDEC slot and releases the audio source from the mixer. Used
    /// when the user closes the source viewer or loads a new clip.
    StopSource,

    // Audio effect keyframes — automate per-effect params, the bypass
    // toggle, and the wet/dry mix. `param_name` follows the same
    // convention as the chain: any descriptor name routes to that
    // param's track; the reserved `__bypass` name routes to the
    // slot's bypass-keyframe track and `__wetdry` to wet/dry.
    AddAudioEffectKeyframe { clip_id: ClipId, effect_index: usize, param_name: String, time_secs: f64, value: f32 },
    RemoveAudioEffectKeyframe { clip_id: ClipId, effect_index: usize, param_name: String, time_secs: f64 },
    MoveAudioEffectKeyframe { clip_id: ClipId, effect_index: usize, param_name: String, keyframe_index: usize, new_time_secs: f64, new_value: f32 },
    SetAudioEffectKeyframeInterpolation { clip_id: ClipId, effect_index: usize, param_name: String, keyframe_index: usize, interpolation: crate::keyframe::Interpolation },
    /// Bezier tangent edit on an audio EFFECT keyframe. Either side
    /// can be left None to keep its current value (matches the
    /// SetAudioKeyframeTangent contract).
    SetAudioEffectKeyframeTangent {
        clip_id: ClipId,
        effect_index: usize,
        param_name: String,
        keyframe_index: usize,
        tangent_in: Option<(f64, f32)>,
        tangent_out: Option<(f64, f32)>,
        /// The key's handle kind, set before the handles given
        /// ([`crate::keyframe::edit_handles`]).
        handles: Option<crate::keyframe::Handles>,
    },
    /// Copy a single audio-effect param's keyframe vec into the
    /// clipboard so the user can paste it onto another (clip, effect,
    /// param). Mirrors `CopyAudioKeyframes` for clip-level params.
    CopyAudioEffectKeyframes { clip_id: ClipId, effect_index: usize, param_name: String },
    /// Paste the keyframe clipboard into a target audio-effect param.
    /// Time-scales the kfs to the target clip's duration if it differs
    /// from the source clip's recorded duration (matches
    /// `PasteAudioKeyframes`).
    PasteAudioEffectKeyframes { clip_id: ClipId, effect_index: usize, param_name: String },
    /// Reverse the keyframe order on an audio-effect param track —
    /// mirrors the kf times around the midpoint of the track's range.
    ReverseAudioEffectKeyframes { clip_id: ClipId, effect_index: usize, param_name: String },
    /// Scale every kf time on an audio-effect param track by a
    /// constant factor. `factor < 1.0` compresses the envelope
    /// (faster), `factor > 1.0` stretches it (slower).
    ScaleAudioEffectKeyframeTiming {
        clip_id: ClipId,
        effect_index: usize,
        param_name: String,
        factor: f64,
    },
    /// Insert a "bump" envelope (rise → hold → fall) on an audio-
    /// effect param at `center_time`. `duration_secs` is the total
    /// width; `ramp_secs` is the rise/fall ramp duration. Matches
    /// `InsertAudioKeyframeBump` for clip-level params.
    InsertAudioEffectKeyframeBump {
        clip_id: ClipId,
        effect_index: usize,
        param_name: String,
        center_time: f64,
        bump_value: f32,
        base_value: f32,
        duration_secs: f64,
        ramp_secs: f64,
    },
    /// Drag-coalesce envelope begin: snapshot the clip's audio chain
    /// state before a slider drag begins. Per-tick UpdateAudioEffectParam
    /// / SetAudioEffectWetDry calls during the drag skip their history
    /// push so a single drag becomes one undo step (and one ops_since_save
    /// bump, not 60+/second of autosave triggers).
    BeginAudioChainDrag { drag_key: String, clip_id: ClipId },
    /// Drag-coalesce envelope end: snapshot the clip's audio chain
    /// state after the drag and commit a single EditAudioChain entry
    /// covering the entire gesture.
    EndAudioChainDrag { drag_key: String, clip_id: ClipId },

    // ── Track matte / blend mode ───────────────────────────────────
    //
    // Phase 1 of the track-matte feature: the data model + AppCommands
    // ship now; the render path (Phase 2) and the inspector UX
    // (Phase 3) read from these on top of an already-stable wire format.
    //
    // Channel is passed as a `u8` to keep `klipp-core` free of a
    // dependency on `klipp-nodes` (where `MatteChannel` lives). The
    // handler maps via the `matte_channel_from_u8` helper. Same
    // pattern as `SetExternalMatteChannel`.

    /// Set a clip's compositor blend mode (Normal, Multiply, Screen, …).
    /// `mode` is the discriminant index into `BlendMode::ALL`.
    SetClipBlendMode { clip_id: ClipId, mode: crate::types::BlendMode },

    /// Bind another clip as this clip's track matte. The render path
    /// resolves `source_clip_id`'s rendered RGBA at the same timeline
    /// time, extracts `channel`, applies `invert`/`feather_px`/`choke_px`,
    /// and multiplies the consumer clip's alpha by the resulting mask.
    ///
    /// The handler rejects:
    /// * `source_clip_id == clip_id` (self-reference),
    /// * cycles (A→B→A or longer chains),
    /// * pointing at an `AdjustmentLayer` (no own pixels),
    /// * pointing at a non-existent clip in the active sequence.
    ///
    /// `consumed_as_matte` on the source clip defaults to `true` (hide
    /// from visible composite) — matches the dominant NLE expectation
    /// and what the right-click "Use clip above as matte" gesture
    /// produces. Clear it via `SetClipConsumedAsMatte` if a creative
    /// effect requires the matte source to stay visible.
    SetClipMatte {
        clip_id: ClipId,
        source_clip_id: ClipId,
        /// 0=Alpha, 1=Luminance, 2=Red, 3=Green, 4=Blue. Mirrors the
        /// `klipp_nodes::params::MatteChannel` discriminant order.
        channel: u8,
        invert: bool,
        feather_px: f32,
        choke_px: f32,
    },

    /// Remove a clip's track-matte binding. If the source clip's
    /// `consumed_as_matte` was set only because of this binding, clear
    /// that too so the source returns to the visible composite. The
    /// handler walks the sequence to check for other consumers.
    ClearClipMatte { clip_id: ClipId },

    /// Inherit `clip_id`'s transform from `target` per the supplied
    /// composition rule and per-axis flags. `mode` is 0 = Absolute
    /// (override) or 1 = Additive (compose: positions add, scales
    /// multiply, rotations add). The handler validates: source != self,
    /// target exists, target is not an `AdjustmentLayer`, and the bind
    /// would not introduce a cycle (A→B→A). Failed validation logs a
    /// warning and leaves the clip untouched.
    SetClipTransformLink {
        clip_id: ClipId,
        target: ClipId,
        /// 0 = Absolute, 1 = Additive. Mirrors the
        /// `klipp_timeline::LinkMode` discriminant order.
        mode: u8,
        inherit_scale: bool,
        inherit_position: bool,
        inherit_rotation: bool,
        inherit_anchor: bool,
    },

    /// Bind `clip_id`'s transform to a tracker target instead of
    /// another clip's transform — `TransformLinkTarget::Tracker`
    /// (HZ-MATTE-NEXT-TRACKER-DRIVER). The link's `target` becomes
    /// `Tracker { clip_id: host_clip_id, point_index }`; per-axis
    /// inherit flags + mode work the same as the clip-target path.
    /// Sibling AppCommand to `SetClipTransformLink` rather than a
    /// discriminator on it — keeps each variant's signature focused
    /// on its target shape, and the inspector dispatch picks the
    /// right command based on which dropdown section the user
    /// selected.
    ///
    /// The handler validates: `host_clip_id` exists in the active
    /// sequence, the named tracker (`host_clip.track_points
    /// .get(point_index as usize)`) exists, and binding doesn't
    /// close a cycle (a tracker target can't form a transform_link
    /// chain by itself, but defence-in-depth covers a clip whose
    /// own transform_link points back at the consumer).
    SetClipTransformLinkTracker {
        clip_id: ClipId,
        host_clip_id: ClipId,
        point_index: u32,
        /// 0 = Absolute, 1 = Additive. Same wire-format tag as
        /// `SetClipTransformLink::mode`.
        mode: u8,
        inherit_scale: bool,
        inherit_position: bool,
        inherit_rotation: bool,
        inherit_anchor: bool,
    },

    /// Remove a clip's `transform_link`. Idempotent — a clip with no
    /// link is left as-is.
    ClearClipTransformLink { clip_id: ClipId },

    /// Apply a hyperzoom transition between `source_clip_id` (typically
    /// a title clip the camera flies *into*) and a target clip (the
    /// shot that emerges from inside the focal point). The handler
    /// writes scale keyframes on both clips:
    ///
    /// * Source: scale 1.0 → `max_scale` over the last `duration_secs`,
    ///   pivoting around `(focal_x_norm, focal_y_norm)` so the focal
    ///   point stays fixed on screen as the rest zooms past.
    /// * Target: scale `max_scale` → 1.0 over the first `duration_secs`,
    ///   anchored at the same focal point so the new shot emerges from
    ///   the matching position.
    ///
    /// `target_clip_id = None` resolves to "the next clip on
    /// `source_clip_id`'s track" (handler walks the sequence; gives up
    /// if the source is the track's last clip). `focal_*_norm` are in
    /// `0.0 ..= 1.0` (normalised screen coords; 0.5/0.5 = centre).
    ///
    /// Layer 2 of the hyperzoom delivery: this is a preset, not a new
    /// effect type — it writes through the existing transform-keyframe
    /// machinery so the result is fully editable in the keyframe
    /// editor and round-trips through save/load like any other
    /// keyframed clip.
    ApplyHyperzoom {
        source_clip_id: ClipId,
        target_clip_id: Option<ClipId>,
        focal_x_norm: f32,
        focal_y_norm: f32,
        duration_secs: f64,
        max_scale: f32,
        /// Easing curve for the ramp keyframes. Wire-format tag:
        /// 0 = Linear (back-compat default), 1 = Bezier S-curve.
        /// Unknown values fall back to Linear in the planner so a
        /// stale dispatch can never produce a curve shape the UI
        /// didn't ask for.
        easing: u8,
        /// Optional flat-at-peak segment straddling the cut. Source
        /// holds peak in its last `hold_secs`; target holds peak in
        /// its first `hold_secs`. `0.0` (default) reproduces the
        /// original two-keyframe behaviour exactly.
        hold_secs: f64,
        /// When `true`, the handler additionally inserts a
        /// `BuiltinEffect::RadialBlur` on source + target with center
        /// bound to the focal point and amount keyframed
        /// `0 → peak → 0` across the ramp window (sharing time-shape
        /// with the scale ramp so the two motions stay locked).
        /// `false` (default) skips the radial-blur insertion entirely.
        add_radial_blur: bool,
        /// Peak `RadialBlur::amount` value the keyframes climb to
        /// (matches the standalone effect's `amount` field). Ignored
        /// when `add_radial_blur` is `false`. `0.0` is treated as
        /// "off" even when `add_radial_blur` is `true` — guards
        /// against an inspector race producing a zero-blur insertion.
        radial_blur_amount: f32,
        /// Glyph-anchored focal-point override (HZ-NEXT-GLYPH-LOCK).
        /// When `Some`, the handler writes the reference into the
        /// source clip's `BuiltinEffect::Transform.focal_ref`; the
        /// engine resolves it each frame to clip-space via the
        /// referenced title's glyph layout. `focal_x_norm` /
        /// `focal_y_norm` continue to be written into `anchor_x` /
        /// `anchor_y` as the static fallback (used when the live
        /// resolution can't reach the glyph — title clip removed,
        /// kind changed, char_index past end of text).
        ///
        /// `None` (default) reproduces today's static-anchor
        /// behaviour byte-identically — pinned by
        /// `focal_ref_none_yields_static_anchor` in the engine
        /// resolver tests.
        focal_ref: Option<crate::FocalRef>,
        /// Opt-in audio stinger inserted on apply (HZ-NEXT-WHOOSH).
        /// `false` (default) skips every whoosh code path —
        /// no synthesis, no media-pool registration, no audio
        /// clip insertion. Pinned to byte-identical behaviour
        /// against pre-feature applies.
        ///
        /// This and the two follow-up whoosh fields are
        /// **additive** — callers that don't care default to
        /// `false` / `0` / `None` and the handler short-circuits
        /// before any audio work runs. The forward-looking
        /// expectation: if `AppCommand` gains `Serialize` /
        /// `Deserialize` derives in a future refactor, these
        /// three fields should receive `#[serde(default)]` at
        /// the same time so old serialised commands round-trip
        /// to safe defaults. We don't add the derives here
        /// because that's a separate refactor (every nested
        /// type would need serde guarantees too).
        add_whoosh: bool,
        /// Variant id for the synthesised whoosh: `0 = Short`,
        /// `1 = Medium`, `2 = Long`, `255 = Custom`. Custom reads
        /// from `whoosh_custom_path` and ignores the built-in
        /// synth. Unknown values fall back to Medium in the
        /// handler so a stale dispatch can never produce silence
        /// when the user ticked the box.
        whoosh_variant: u8,
        /// Path to a user-specified WAV / audio file when
        /// `whoosh_variant == 255` (Custom). Ignored otherwise.
        /// `None` (default) is the built-in-synth path.
        whoosh_custom_path: Option<std::path::PathBuf>,
        /// Opt-in chromatic-aberration ramp inserted on apply
        /// (HZ-NEXT-LIGHT-LEAK-CHROMATIC). `false` (default) skips
        /// every chromatic-aberration code path — pinned to
        /// byte-identical behaviour against pre-feature applies.
        ///
        /// When `true`, the handler inserts/updates a
        /// `BuiltinEffect::ChromaticAberration` on source + target
        /// with `center` bound to the focal point and `red_shift` /
        /// `blue_shift` keyframed `0 → ∓amount → 0` across the
        /// same ramp window as the scale animation (red converges,
        /// blue diverges — the canonical trailer-RGB-split look).
        ///
        /// Same additive-defaulting-to-safe-values intent as the
        /// whoosh fields: when `AppCommand` gains
        /// `Serialize`/`Deserialize` derives in a future refactor,
        /// this field should receive `#[serde(default)]` so old
        /// commands round-trip to safe defaults.
        add_chromatic_aberration: bool,
        /// Peak chromatic-aberration amount (pixels). Drives both
        /// `red_shift` (animated `0 → -amount → 0`) and `blue_shift`
        /// (animated `0 → +amount → 0`) symmetrically. Ignored
        /// when `add_chromatic_aberration` is `false`. `0.0` is
        /// treated as "off" even when the bool is `true` —
        /// guards against a stale dispatch producing a zero-effect
        /// insertion.
        chromatic_aberration_amount: f32,
        /// Opt-in light-leak (film-halation) ramp inserted on
        /// apply. Same shape as `add_chromatic_aberration`.
        add_light_leak: bool,
        /// Peak `BuiltinEffect::FilmHalation::intensity` value the
        /// keyframes climb to. Ignored when `add_light_leak` is
        /// `false`; `0.0` short-circuits to "off".
        light_leak_intensity: f32,
    },
    /// Open the "Save hyperzoom as preset" modal dialog
    /// (HZ-NEXT-SAVED-PRESETS). Pins `source_clip` so the dialog
    /// reads the right clip's hyperzoom scratch even if the user
    /// navigates the timeline before clicking Save.
    OpenSaveHyperzoomPresetDialog {
        source_clip: ClipId,
    },

    /// Auto-apply `transform_link` to every clip *beneath* the
    /// hyperzoom source within the ramp window so they fly into the
    /// focal point alongside the source. Closes the loop on the
    /// canonical "title overlay above an underlying shot" workflow:
    /// after `ApplyHyperzoom` writes keyframes on the title and target,
    /// the user invokes this to link the underlying V1/V2 shots so
    /// they swell with the title rather than sitting still.
    ///
    /// The handler walks every track strictly *below* the source's
    /// track and links any clip overlapping
    /// `[source.timeline_out - ramp_secs, source.timeline_out]` whose
    /// kind is meaningfully linkable (not AdjustmentLayer / Gap /
    /// nested Sequence) and that doesn't already have a link of its
    /// own (preserves explicit user parenting choices). Cycle-rejected
    /// at bind time so a pathological project file can't produce a
    /// runtime loop.
    ///
    /// `mode = 0` (Absolute) is the canonical "fly with parent"
    /// mode — the underlying shot's own keyframes get replaced by the
    /// source's resolved transform during the ramp. `mode = 1`
    /// (Additive) composes if the underlying shot already had its own
    /// motion the user wants preserved.
    AutoLinkClipsBeneathSource {
        source_clip_id: ClipId,
        ramp_secs: f64,
        /// 0 = Absolute, 1 = Additive (matches `SetClipTransformLink`).
        mode: u8,
        inherit_scale: bool,
        inherit_position: bool,
        inherit_rotation: bool,
        inherit_anchor: bool,
    },

    /// Toggle the "hide matte source from visible composite" flag on a
    /// clip. Default behaviour when bound via `SetClipMatte` is `true`;
    /// pros can flip this to keep the matte source visible for
    /// debugging or unusual creative effects.
    SetClipConsumedAsMatte { clip_id: ClipId, consumed: bool },

    /// Phase 4B — Configure or update the clip's stencil mode. The
    /// clip's pixels become a mask that multiplies the alpha of every
    /// clip on a lower track at the same time-range. Channel/invert/
    /// feather/choke mirror `SetClipMatte` semantics. Stencil clips
    /// are implicitly hidden from the visible composite (AE convention).
    SetClipStencil {
        clip_id: ClipId,
        /// 0=Alpha, 1=Luminance, 2=Red, 3=Green, 4=Blue.
        channel: u8,
        invert: bool,
        feather_px: f32,
        choke_px: f32,
    },

    /// Phase 4B — clear stencil mode on a clip; the clip returns to
    /// visible composite participation.
    ClearClipStencil { clip_id: ClipId },

    /// Phase 5 — single-shot dispatch for the "Use clip above as matte"
    /// gesture so the command palette can offer it without
    /// re-implementing the candidate-finder. Handler walks the active
    /// sequence for the nearest higher-track clip overlapping `clip_id`,
    /// then runs the same `SetClipMatte` flow as the right-click menu.
    /// Toasts when no candidate exists rather than silently no-oping.
    /// `channel` is the same wire format as `SetClipMatte`.
    UseClipAboveAsMatte { clip_id: ClipId, channel: u8 },

    /// Phase 5 — toggle stencil mode on a clip. Flips between
    /// `SetClipStencil` (luma channel default) and `ClearClipStencil`
    /// based on current state. Lets the command palette expose a single
    /// idempotent entry instead of two.
    ToggleClipStencil { clip_id: ClipId },

    /// Import a file with explicit control over automatic analysis.
    ImportMediaWithAnalysis { path: PathBuf, analyze: bool },

    /// Write the active sequence as an OpenTimelineIO `.otio` file.
    ///
    /// Appended here rather than beside `ExportFcpxml` and `ExportEdl`
    /// because parallel lanes share this enum and appends merge where
    /// insertions in the middle conflict.
    ExportOtio { path: PathBuf },

    /// Write the active sequence as an AAF file (#89): exported as OTIO
    /// and converted by OpenTimelineIO's `otioconvert`, with its AAF
    /// adapter, where they are installed.
    ExportAaf { path: PathBuf },

    /// Tell the editor `text`: what work done off the UI thread says when
    /// it finishes.
    ShowToast { text: String },

    /// Open the Normalize dialog for the selected clips and `clip_id`,
    /// when given (the one a context menu is on) (#1843).
    OpenNormalizeDialog { clip_id: Option<crate::ClipId> },

    /// Open the Find by Picture window (#2212).
    OpenVisualSearch,
    /// Search the analysed footage for stretches that look like `query`,
    /// off the UI thread; [`AppCommand::VisualSearchFound`] brings back
    /// what it found.
    RunVisualSearch { query: String },
    /// What a picture search found, best first, as (media, start, end,
    /// best frame, score) in seconds of the file; or why it could not
    /// search.
    VisualSearchFound { query: String, hits: Result<Vec<(MediaId, f64, f64, f64, f32)>, String> },

    /// Measure `clip_ids` and bring them to `settings`' target, off the
    /// UI thread; [`AppCommand::ApplyClipGains`] applies what it finds.
    NormalizeClips { clip_ids: Vec<crate::ClipId>, settings: crate::normalize::NormalizeSettings },

    /// Open the Fit Music to Length dialog for a music clip (#2210).
    OpenMusicFitDialog { clip_id: crate::ClipId },

    /// Open the Make Shorts dialog for a media file (#2720).
    OpenShortsDialog { media_id: crate::MediaId },
    /// Fill the open sequence with a short: `pieces` of `media_id` (file
    /// seconds) end to end, upright, framed on the speaker, every other
    /// piece pushed in, captioned, `hook` as a title when given, its
    /// sound brought to -14 LUFS (#2720).
    FillShort { media_id: crate::MediaId, pieces: Vec<(f64, f64)>, hook: Option<String> },

    /// Read `clip_id`'s track and fit it to about `target_secs` by whole
    /// bars, off the UI thread; [`AppCommand::MusicFitPlanned`] brings the
    /// plan, or why there is none, back to the dialog.
    PlanMusicFit { clip_id: crate::ClipId, target_secs: f64, skip_intro: bool },

    /// A fit planned for `clip_id`: the source stretches to play back to
    /// back, or why no whole-bar fit lands near `target_secs`.
    MusicFitPlanned { clip_id: crate::ClipId, target_secs: f64, plan: Result<(Vec<(f64, f64)>, f64, Option<f64>, i64), String> },

    /// Replace `clip_id` with `segments` of its media, placed back to back
    /// from where it starts, joins crossfaded, as one undo step.
    ApplyMusicFit { clip_id: crate::ClipId, segments: Vec<(f64, f64)> },

    /// Set each clip's gain (linear), mirrored to a linked partner, as one
    /// undo step.
    ApplyClipGains { gains: Vec<(crate::ClipId, f32)> },

    /// Translate the captions on `track_id` from `source` to `target`
    /// (ISO 639-1 codes) as translation job `job_id`, off the UI thread,
    /// its progress and cancel in the translation jobs; when it is done a
    /// new subtitle track under it carries them, as one undo step (#2213).
    /// The brief's names and vocabulary are kept as written, and each
    /// `glossary` term (source, translation) comes out as its translation
    /// (#2429).
    TranslateCaptions { track_id: TrackId, source: String, target: String, job_id: String, glossary: Vec<(String, String)> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerMode {
    Program,
    Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Blade,
    Range,
    /// Trim tool — modifier-free access to Slip (clip body) and Roll
    /// (junction). Shift+drag on clip body triggers Slide. Alt/Alt+Shift
    /// modifiers continue to work in Tool::Select for power-user access.
    Trim,
    /// Text tool (#1713): a click in the viewer starts a title at that
    /// point, a drag draws its text box, and typing goes straight into
    /// it. The timeline treats it as Select.
    Text,
    /// Mask tool (#74): draws a freeform mask on the clip in the viewer
    /// and edits its points and curve handles, the viewer taking Delete,
    /// Enter, Escape and Alt-click for them. The timeline treats it as
    /// Select.
    Mask,
    /// Rate Stretch tool (#3055): dragging a clip's edge changes its
    /// speed so the same content fills the new length, where the Select
    /// tool's edge trims; its top corners stretch with either.
    Stretch,
}

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub enum MarkerColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

impl MarkerColor {
    pub fn to_rgb(self) -> (u8, u8, u8) {
        match self {
            MarkerColor::Red => (220, 60, 60),
            MarkerColor::Orange => (220, 140, 40),
            MarkerColor::Yellow => (220, 200, 40),
            MarkerColor::Green => (60, 180, 80),
            MarkerColor::Blue => (60, 120, 220),
            MarkerColor::Purple => (160, 80, 200),
        }
    }

    pub fn all() -> &'static [MarkerColor] {
        &[
            MarkerColor::Red,
            MarkerColor::Orange,
            MarkerColor::Yellow,
            MarkerColor::Green,
            MarkerColor::Blue,
            MarkerColor::Purple,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            MarkerColor::Red => "Red",
            MarkerColor::Orange => "Orange",
            MarkerColor::Yellow => "Yellow",
            MarkerColor::Green => "Green",
            MarkerColor::Blue => "Blue",
            MarkerColor::Purple => "Purple",
        }
    }
}

/// Marker kind / type-flag. Decouples a marker's *role* from its
/// colour: a Chapter and a ToDo can share the same colour without
/// losing their distinct workflow meaning. The set is intentionally
/// small — six built-ins covering the common cross-NLE roles. User-
/// defined categories with custom names are tracked separately under
/// GAP-KDE-015 and would replace this enum with a project-defined id
/// when they ship.
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq, Default)]
pub enum MarkerKind {
    /// Untyped marker — the existing default behaviour.
    #[default]
    None,
    /// Chapter / DVD / YouTube chapter point.
    Chapter,
    /// To-do / fix-this-later note.
    ToDo,
    /// General comment / annotation.
    Comment,
    /// Best take / pick from a multi-take session.
    BestTake,
    /// Section / act / scene boundary.
    Section,
    /// A reviewer's comment brought back from a review round (#2214).
    Review,
}

/// Where a review comment stands (#2214).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum ReviewStatus {
    #[default]
    Open,
    Done,
    WontDo,
}

impl ReviewStatus {
    pub fn label(self) -> &'static str {
        match self {
            ReviewStatus::Open => "Open",
            ReviewStatus::Done => "Done",
            ReviewStatus::WontDo => "Won't do",
        }
    }

    pub fn all() -> &'static [ReviewStatus] {
        &[ReviewStatus::Open, ReviewStatus::Done, ReviewStatus::WontDo]
    }
}

impl MarkerKind {
    pub fn label(self) -> &'static str {
        match self {
            MarkerKind::None => "None",
            MarkerKind::Chapter => "Chapter",
            MarkerKind::ToDo => "To Do",
            MarkerKind::Comment => "Comment",
            MarkerKind::BestTake => "Best Take",
            MarkerKind::Section => "Section",
            MarkerKind::Review => "Review",
        }
    }

    /// Single-letter badge rendered next to a marker row in the
    /// panel — keeps the row compact without forcing the full label.
    pub fn badge(self) -> &'static str {
        match self {
            MarkerKind::None => "",
            MarkerKind::Chapter => "C",
            MarkerKind::ToDo => "T",
            MarkerKind::Comment => "✱",
            MarkerKind::BestTake => "★",
            MarkerKind::Section => "§",
            MarkerKind::Review => "R",
        }
    }

    pub fn all() -> &'static [MarkerKind] {
        &[
            MarkerKind::None,
            MarkerKind::Chapter,
            MarkerKind::ToDo,
            MarkerKind::Comment,
            MarkerKind::BestTake,
            MarkerKind::Section,
            MarkerKind::Review,
        ]
    }
}

/// Payload for [`AppEvent::MediaLoaded`]. See the T-4 note on the
/// variant itself for the rationale on boxing this out.
#[derive(Debug, Clone)]
pub struct MediaLoadedPayload {
    pub media_id: MediaId,
    pub path: PathBuf,
    pub duration: Timecode,
    pub fps: Fps,
    pub resolution: Resolution,
    pub video_codec: String,
    pub audio_codec: String,
    pub has_audio: bool,
    pub file_size_bytes: u64,
    pub sample_rate: Option<u32>,
    pub audio_channels: Option<u16>,
    pub is_still: bool,
    pub file_modified_at: Option<u64>,
    pub media_created_at: Option<u64>,
    pub colour_space: u8,
    pub source_rotation: i32,
    // Extended metadata
    pub camera_make: Option<String>,
    pub camera_model: Option<String>,
    pub encoder: Option<String>,
    pub bit_depth: Option<u8>,
    pub video_bitrate_kbps: Option<u32>,
    pub audio_bitrate_kbps: Option<u32>,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub pixel_format: Option<String>,
    /// Timecode, reel, lens, exposure and every tag the file carried.
    pub shot: crate::ShotMetadata,
    /// The duration the file's header claims, when the file holds less:
    /// a copy cut short. `duration` is then what it holds.
    pub truncated_from_secs: Option<f64>,
    /// Where its picture and its sound end (#3230).
    pub stream_ends: crate::StreamEnds,
    /// The Dolby Vision profile of its picture, when it carries one.
    pub dolby_vision_profile: Option<u8>,
}

/// Why the engine could not read a node's input for the eyedropper.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeSampleRefusal {
    /// The clip is moved, scaled or turned here, so the point on screen
    /// is not the same point of its picture: read the screen instead.
    Placed,
    /// Nothing to read, and why, for the user.
    Why(String),
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    /// The picture a node receives at the eyedropper's point, in the
    /// working space's linear light: the mean of a 5×5 patch. Answers
    /// `AppCommand::SampleNodeInput`.
    NodeInputSampled { clip_id: ClipId, node_id: uuid::Uuid, nx: f32, ny: f32, sample: Result<[f32; 3], NodeSampleRefusal> },
    // Playback state
    PlaybackStarted,
    PlaybackPaused,
    /// Render In to Out under way: frames done of the total (#924).
    RenderRangeProgress { done: usize, total: usize },
    /// Render In to Out over: frames rendered, frames passed over (edited
    /// away or undecodable), whether it was stopped, and why it did not
    /// start when it rendered nothing.
    RenderRangeFinished { rendered: usize, passed_over: usize, cancelled: bool, note: Option<String> },
    PlaybackStopped,
    PlayheadMoved(Timecode),
    LoopedBack,

    // Source viewer playback state — separate so the UI can keep its
    // `state.source_playback` field in sync without conflating with
    // the timeline transport.
    SourcePlaybackStarted,
    SourcePlaybackPaused,
    SourcePlayheadMoved(f64),

    // Media. T-4: the payload is carried by a separate struct behind
    // a Box so the AppEvent enum stays small — 25 fields (8× Option<String>)
    // inlined made every variant ~344 B, including the 60 Hz
    // FrameReady variant that doesn't carry any payload. Boxing pushes
    // AppEvent down to 32 B and the MediaLoaded payload is
    // heap-allocated on the rare occasions a media file is loaded
    // (once per import, not per frame).
    MediaLoaded(Box<MediaLoadedPayload>),
    /// The picture or the sound kept failing (a panic each time it was
    /// made again) and is left off until the playhead is moved or played
    /// (#3181): `what` names it, "The picture", "The sound".
    PlaybackGaveUp { what: String },
    MediaLoadFailed {
        media_id: MediaId,
        /// The file that would not open, for saying so (#654).
        path: PathBuf,
        error: String,
    },
    /// File watcher detected a source file was modified or deleted.
    MediaFileChanged {
        path: PathBuf,
        deleted: bool,
    },

    // Frame
    FrameReady,

    // Multi-clip
    ClipAppended { clip_id: ClipId },
    ClipRemoved { clip_id: ClipId },
    ClipMoved { clip_id: ClipId, new_timeline_in: Timecode },
    ClipTrimmed { clip_id: ClipId },
    ClipSelected(Option<ClipId>),

    // Project
    ProjectSaved { path: PathBuf },
    ProjectLoaded { path: PathBuf },
    ProjectError { message: String },

    // Export
    ExportProgress { percent: f32, time_secs: f64 },
    ExportComplete { path: PathBuf },
    ExportError { message: String },

    // Source monitor
    SourceFrameReady,
    SourceMediaLoaded { media_id: MediaId, duration: Timecode },

    // Proxy
    ProxyGenerationStarted { media_id: MediaId },
    ProxyGenerationComplete { media_id: MediaId, path: PathBuf },
    ProxyGenerationFailed { media_id: MediaId, error: String },

    // Thumbnails
    ThumbnailReady { media_id: MediaId, rgba: Vec<u8>, width: u32, height: u32 },

    /// Phase 4C — track-matte preview thumbnail. Engine emits a tiny
    /// grayscale RGBA snapshot of the resolved mask for whichever clip
    /// the inspector currently has open. Emitted at most once per
    /// composited frame; ~80×45 px, < 15 KB. Cleared when the consumer
    /// no longer has an active matte source (event omitted entirely).
    MatteMaskReady {
        consumer_clip_id: ClipId,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    },

    // Audio conforming
    AudioConformStarted { media_id: MediaId },
    AudioConformComplete { media_id: MediaId },
    AudioConformFailed { media_id: MediaId, error: String },

    // Audio metering
    TrackPeakLevels { track_index: usize, left: f32, right: f32 },
    /// An aux bus's levels after its fader (#2861), by its index.
    BusPeakLevels { bus_index: usize, left: f32, right: f32 },
    /// Which aux buses' way to master is longer than playback and the
    /// export make up for, by index (#2861): sent when it changes.
    BusesHeldAtCap(Vec<bool>),
    MasterPeakLevels { left: f32, right: f32 },
    /// BS.1770-4 loudness meter readings from the mix thread.
    LufsReadings { momentary: f32, short_term: f32, integrated: f32, loudness_range: f32, true_peak_dbfs: f32 },
    /// The gain playback adds to be heard as the export will deliver
    /// (#1565): `gain_db` once measured, the export's own gain beside it
    /// (larger when the export limits the peaks), `measuring` while the
    /// mix is measured.
    DeliveryLoudness { target_lufs: Option<f32>, gain_db: Option<f32>, export_gain_db: Option<f32>, measuring: bool },

    // Performance stats
    /// `hw_decode` is `None` when no video decoder is currently open —
    /// the overlay shows "—" instead of guessing SW/NVDEC.
    EngineStats { dropped_frames: u64, hw_decode: Option<bool> },

    // Skim transcode
    SkimTranscodeProgress { media_id: MediaId, progress: f32 },
    SkimTranscodeComplete { media_id: MediaId, path: PathBuf },
    SkimTranscodeFailed { media_id: MediaId, path: PathBuf, error: String },

    // Browser skim frame
    BrowserSkimFrameReady { media_id: MediaId, rgba: Vec<u8>, width: u32, height: u32, target: ViewerMode },
    /// A `SkimTimeline` answered: `composited` when the engine drew it,
    /// else the UI shows the raw skim frame for it (#2945).
    TimelineSkimShown { time: Timecode, composited: bool },

    // Video stabilization
    StabilizationProgress { clip_id: ClipId, progress: f32 },
    DeflickerProgress { clip_id: ClipId, progress: f32 },
    DeflickerComplete { clip_id: ClipId },
    DeflickerFailed { clip_id: ClipId, error: String },
    StabilizationComplete { clip_id: ClipId },
    StabilizationFailed { clip_id: ClipId, error: String },

    // Scene (shot-boundary) detection
    SceneDetectProgress { clip_id: ClipId, progress: f32 },
    SceneDetectComplete { clip_id: ClipId, cut_times: Vec<f64> },
    SceneDetectFailed { clip_id: ClipId, error: String },

    // Point tracking
    TrackingProgress { clip_id: ClipId, progress: f32 },
    /// `motion`: a window track's (scale, degrees) on each frame of
    /// `positions` (#238); empty for a point track.
    TrackingComplete { clip_id: ClipId, point_index: usize, positions: Vec<(f64, f64)>, start_frame: usize, motion: Vec<(f64, f64)>, corners: Vec<[(f64, f64); 4]> },
    TrackingFailed { clip_id: ClipId, error: String },
    /// A surface tracked (#684): where its corners are in each file frame
    /// followed, `(file seconds, corners)`, earliest first, the reference
    /// frame's among them.
    /// `source_size` is the picture the corners are fractions of, as
    /// shown; `held_secs` the file seconds read for the track, which can
    /// be fewer than the clip's when they would not all fit in memory.
    /// `direction` and `interactive` are the request's.
    /// A surface tracked (#2754): each key its file time, the map from
    /// `corners` to that frame (fractions of the picture, row by row), the
    /// tracker's confidence, 0 to 1, and how it was followed: 0 well, 1
    /// uncertainly, 2 lost (the last good map held), and how light the
    /// surface is there, 0 to 1 (#2751).
    /// `track_area` and `exclude` are the area followed and the parts left
    /// out, on the reference frame as `corners` are.
    SurfaceTracked { clip_id: ClipId, surface: uuid::Uuid, name: String, corners: [[f32; 2]; 4], reference_secs: f64, keys: Vec<(f64, [f64; 9], f32, u8, Option<f32>)>, source_size: [u32; 2], media_fps: f64, held_secs: (f64, f64), direction: i8, interactive: bool, track_area: Option<[[f32; 2]; 4]>, exclude: Vec<Vec<[f32; 2]>> },
    SurfaceTrackFailed { clip_id: ClipId, surface: uuid::Uuid, error: String },
    /// A [`AppCommand::CorrectSurface`] solved (#2754): the correction at
    /// file second `source_secs`, `corners` there as fractions of the
    /// picture, and each span re-solved, its keys as
    /// [`AppEvent::SurfaceTracked`]'s, replacing those strictly between
    /// its bounds (file seconds; `None` open to that end of the track).
    SurfaceCorrected { clip_id: ClipId, surface: uuid::Uuid, source_secs: f64, corners: [[f32; 2]; 4], spans: Vec<(Option<f64>, Option<f64>, Vec<(f64, [f64; 9], f32, u8, Option<f32>)>)> },
    /// The corners a [`AppCommand::SnapSurface`] found, as fractions of the
    /// clip's own picture, or `None` where none was found; `why` then
    /// says what was wrong (#2754).
    SurfaceSnapped { clip_id: ClipId, at_secs: f64, corners: Option<[[f32; 2]; 4]>, then_track: bool, name: Option<String>, why: Option<String>, request_id: Option<uuid::Uuid> },

    // Audio sync (#61)
    /// Background cross-correlation finished. `offset_samples` at 48 kHz —
    /// positive means the target clip was found to start _later_ than the
    /// reference; negative means _earlier_. `ref_timeline_in_secs` is the
    /// reference clip's timeline position at the moment the job was spawned
    /// (read on the UI thread, shipped to the worker, and echoed back so the
    /// completion handler has it without another lookup).
    /// A pool pair was synced (see `AppCommand::SyncMediaPair`).
    MediaSyncComplete { video_id: MediaId, sync: crate::SyncedAudio },
    MediaSyncFailed { video_id: MediaId, error: String },
    /// A multicam's angles were put on one clock (see
    /// `AppCommand::CreateSyncedMulticam`), for the sequence and playhead
    /// the request was made at.
    MulticamSyncReady { plan: crate::MulticamPlan, sequence_id: crate::SequenceId, at_secs: f64 },
    AudioSyncComplete {
        clip_id: ClipId,
        offset_samples: i64,
        ref_timeline_in_secs: f64,
    },
    AudioSyncFailed { clip_id: ClipId, error: String },
    /// A denoiser profile was learned in the background (see
    /// `AppCommand::LearnDenoiserProfile`); `params` are ready to store.
    DenoiserProfileLearned { clip_id: ClipId, effect_index: usize, params: Vec<(String, f32)>, source_secs: f64 },
    DenoiserProfileFailed { error: String },
    /// A Normalize's input measured in the background (#2778): the
    /// clip's integrated loudness into the effect at `effect_index`
    /// (`None`: silent), for the `key` it was measured for.
    NormalizeMeasured { clip_id: ClipId, effect_index: usize, key: u64, lufs: Option<f32> },
    /// The water under a clip's reflection, measured for footprint `key`
    /// (#2795), sRGB; `None` when none could be.
    WaterMeasured { clip_id: ClipId, key: u32, water: Option<[u8; 3]> },
}

/// Target for generating keyframes from tracked point data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TrackTarget {
    /// Generate position keyframes for a mask's center.
    MaskCenter { node_id: uuid::Uuid, mask_index: usize },
    /// Generate transform position keyframes.
    TransformPosition,
}

/// A single colour-grade parameter change queued by the UI for the engine.
///
/// Written to a lock-free `ArrayQueue` by the UI's `SetNodeParam` handler at
/// slider-scrub rate (60-120 Hz). Drained by the engine at the start of each
/// frame, which applies the change to the shared `Arc<klipp_timeline::SharedSequence>`
/// with ONE write-lock acquisition per frame (not one per slider tick).
///
/// This path bypasses `sync_sequence_to_engine` (which rebuilds the whole
/// flattened sequence — fine for structural edits, expensive per-tick for
/// slider scrubbing that contends with the engine's long-held read lock).
#[derive(Debug, Clone)]
pub struct ParamChange {
    pub clip_id: ClipId,
    pub node_id: uuid::Uuid,
    pub param: String,
    pub value: f32,
}

#[cfg(test)]
mod shape_warning_tests {
    use super::*;

    /// #2784: another shape than the timeline's is said, with where the
    /// bars go; the same shape, or within 1.5%, isn't.
    #[test]
    fn another_shape_is_said() {
        let at = |w, h| ExportSettings { resolution: Some(Resolution { width: w, height: h }), ..Default::default() };
        assert!(at(1080, 1920).shape_warning(1920, 1080).unwrap().contains("above and below"));
        assert!(at(1080, 1080).shape_warning(1920, 1080).unwrap().contains("above and below"), "a wide picture in a square: bars above and below");
        assert!(at(1920, 1080).shape_warning(1080, 1920).unwrap().contains("at the sides"));
        assert_eq!(at(3840, 2160).shape_warning(1920, 1080), None);
        assert_eq!(at(1920, 1088).shape_warning(1920, 1080), None, "within 1.5%");
        assert_eq!(ExportSettings::default().shape_warning(1920, 1080), None, "the timeline's own size");
    }
}

#[cfg(test)]
mod odt_choice_tests {
    use super::*;

    #[test]
    fn default_is_match_viewer() {
        // Default must match the viewer's display path so projects
        // produced before delivery-target ODTs landed render exactly
        // the same pixels they did before. Pinned here and in the
        // ExportSettings default below.
        assert_eq!(OdtChoice::default(), OdtChoice::MatchViewer);
    }

    #[test]
    fn variants_distinct() {
        // Cheap guard against an accidental enum dedupe via macro.
        for (i, a) in OdtChoice::ALL.iter().enumerate() {
            for b in &OdtChoice::ALL[i + 1..] {
                assert_ne!(a, b, "duplicate variants in OdtChoice::ALL");
            }
        }
    }

    #[test]
    fn u8_round_trip_for_all_supported_variants() {
        // Every variant the menu exposes must survive the u8 wire
        // round-trip. Unknown is the serde sentinel, exercised
        // separately below.
        for choice in OdtChoice::ALL {
            assert_eq!(OdtChoice::from_u8(choice.as_u8()), choice);
        }
    }

    /// The legacy sRGB path is offered only in a Rec.709 project: in an
    /// ACEScg one it would put AP1 values under an sRGB label, and in a
    /// BT.2020 one it does exactly what Match Viewer does. Either way
    /// there is nothing to gain from listing it.
    #[test]
    fn the_legacy_srgb_path_is_only_offered_where_it_is_srgb() {
        use crate::WorkingColorspace as W;
        assert!(OdtChoice::Srgb.suits_working_space(W::Rec709Linear));
        assert!(!OdtChoice::Srgb.suits_working_space(W::Bt2020Linear));
        assert!(!OdtChoice::Srgb.suits_working_space(W::AcesCG));
    }

    /// Every other transform converts into the space it names, so every
    /// working space can reach all of them.
    #[test]
    fn every_converting_transform_suits_every_working_space() {
        use crate::WorkingColorspace as W;
        for odt in OdtChoice::ALL.into_iter().filter(|o| *o != OdtChoice::Srgb) {
            for ws in [W::Rec709Linear, W::Bt2020Linear, W::AcesCG] {
                assert!(odt.suits_working_space(ws), "{odt:?} in {ws:?}");
            }
        }
    }

    /// Whatever the project, there is always something to pick — the
    /// picker filters on both axes at once, so the two must not exclude
    /// everything between them.
    #[test]
    fn some_transform_always_fits() {
        use crate::WorkingColorspace as W;
        for delivery in [HdrMode::Sdr, HdrMode::Pq, HdrMode::Hlg] {
            for ws in [W::Rec709Linear, W::Bt2020Linear, W::AcesCG] {
                assert!(
                    OdtChoice::ALL
                        .into_iter()
                        .any(|o| o.fits(delivery) && o.suits_working_space(ws)),
                    "nothing to export a {ws:?} project as {delivery:?}",
                );
            }
        }
    }

    #[test]
    fn u8_tags_are_stable() {
        // Pin the wire tags. Reordering the enum or inserting a new
        // variant must not silently change these — atomic stores and
        // FFI consumers depend on them.
        assert_eq!(OdtChoice::MatchViewer.as_u8(), 0);
        assert_eq!(OdtChoice::Rec709Sdr.as_u8(), 1);
        assert_eq!(OdtChoice::Rec2100Pq1000.as_u8(), 2);
        assert_eq!(OdtChoice::Rec2100Pq4000.as_u8(), 3);
        assert_eq!(OdtChoice::DciP3D65_48.as_u8(), 4);
        assert_eq!(OdtChoice::Srgb.as_u8(), 5);
        assert_eq!(OdtChoice::Rec2020Sdr.as_u8(), 6);
        assert_eq!(OdtChoice::Unknown.as_u8(), 255);
    }

    #[test]
    fn unknown_u8_falls_back_to_match_viewer() {
        // A corrupted atomic / FFI integer must default to the safe
        // viewer path — never produce a delivery-target surprise.
        for tag in [7u8, 8, 42, 100, 200, 254] {
            assert_eq!(OdtChoice::from_u8(tag), OdtChoice::MatchViewer);
        }
    }

    #[test]
    fn export_settings_default_carries_match_viewer() {
        // Pin the back-compat contract: every project that goes
        // through `ExportSettings::default()` (the entry point for
        // every export-dialog open + every legacy site that builds
        // a settings struct) starts with `odt = MatchViewer`. If
        // this default ever shifts, an existing user's export
        // pixels silently change at the next build.
        let s = ExportSettings::default();
        assert_eq!(s.odt, OdtChoice::MatchViewer);
    }

    #[test]
    fn label_covers_every_variant_including_sentinel() {
        // The menu only iterates `ALL`, but the Inspector / debug
        // logs may print `Unknown` if a future-version project
        // file is opened. Catch the missing-arm case here so we
        // don't hit a `match` panic at runtime.
        for choice in OdtChoice::ALL {
            assert!(!choice.label().is_empty());
        }
        assert_eq!(OdtChoice::Unknown.label(), "Unknown");
    }
}

#[cfg(test)]
mod gif_format_tests {
    use super::*;

    #[test]
    fn gif_format_extension_and_label() {
        assert_eq!(ExportFormat::Gif.extension(), "gif");
        assert_eq!(ExportFormat::Gif.label(), "GIF");
    }

    #[test]
    fn gif_format_capability_gates() {
        // GIF must report no audio / no chapters / no subtitles so
        // the export dialog hides or disables those sections.
        assert!(!ExportFormat::Gif.supports_audio());
        assert!(!ExportFormat::Gif.supports_chapters());
        assert!(!ExportFormat::Gif.supports_subtitles());
    }

    #[test]
    fn standard_formats_keep_full_capabilities() {
        // Defensive — make sure the new helpers didn't accidentally
        // weaken any of the existing formats.
        for f in [ExportFormat::Mp4, ExportFormat::Mkv, ExportFormat::Mov] {
            assert!(f.supports_audio());
            assert!(f.supports_chapters());
            assert!(f.supports_subtitles());
        }
        // WebM has no chapters but does carry audio + subtitles.
        assert!(ExportFormat::WebM.supports_audio());
        assert!(!ExportFormat::WebM.supports_chapters());
        assert!(ExportFormat::WebM.supports_subtitles());
    }

    /// #1813: the sound-alone formats take no picture, chapters or
    /// subtitles, hold only their own audio, and a codec that doesn't fit
    /// gives way to theirs; the video codec is not looked at.
    #[test]
    fn audio_only_formats_hold_only_their_own_sound() {
        use ExportAudioCodec as A;
        for (f, ext, own) in [(ExportFormat::Mp3, "mp3", A::Mp3), (ExportFormat::Wav, "wav", A::Pcm16), (ExportFormat::M4a, "m4a", A::Aac)] {
            assert!(f.is_audio_only() && f.supports_audio(), "{f:?}");
            assert!(!f.supports_chapters() && !f.supports_subtitles(), "{f:?}");
            assert_eq!(f.extension(), ext);
            assert!(f.holds_audio(own) && f.holds_video(ExportVideoCodec::ProRes4444), "{f:?}");
            assert_eq!(f.fit(ExportVideoCodec::H264, A::Opus).1, own, "{f:?}: Opus gives way");
        }
        assert!(ExportFormat::Wav.holds_audio(A::Pcm24) && !ExportFormat::Wav.holds_audio(A::Aac));
        assert!(!ExportFormat::Mp4.is_audio_only() && !ExportFormat::Gif.is_audio_only());
    }

    #[test]
    fn gif_codec_is_mutually_exclusive() {
        // The GIF codec only fits the GIF container, and the GIF
        // container holds nothing else.
        use ExportVideoCodec as V;
        for f in [ExportFormat::Mp4, ExportFormat::Mkv, ExportFormat::Mov, ExportFormat::WebM] {
            assert!(!f.holds_video(V::Gif), "{f:?} holds GIF");
        }
        for c in [V::H264, V::H265, V::ProRes422, V::ProRes422HQ, V::ProRes4444, V::DNxHD115, V::DNxHD145, V::Av1, V::Vp9] {
            assert!(!ExportFormat::Gif.holds_video(c), "GIF holds {c:?}");
        }
    }

    #[test]
    fn containers_refuse_the_codecs_ffmpeg_will_not_mux() {
        use ExportAudioCodec as A;
        use ExportVideoCodec as V;
        // Each of these made FFmpeg fail to write the header.
        assert!(!ExportFormat::WebM.holds_video(V::H264));
        assert!(!ExportFormat::WebM.holds_video(V::H265));
        assert!(!ExportFormat::Mov.holds_video(V::Av1));
        assert!(!ExportFormat::Mov.holds_video(V::Vp9));
        assert!(!ExportFormat::Mov.holds_audio(A::Opus));
        assert!(!ExportFormat::Mov.holds_audio(A::Flac));
        for a in [A::Aac, A::Mp3, A::Flac, A::Pcm16, A::Pcm24] {
            assert!(!ExportFormat::WebM.holds_audio(a), "{a:?} in WebM");
        }
        assert!(!ExportFormat::Mp4.holds_video(V::ProRes422));
        assert!(!ExportFormat::Mp4.holds_video(V::DNxHD115));
        // And the pairs it writes, checked the same way: tightening a
        // container past these would refuse a file that encodes.
        for (f, v) in [
            (ExportFormat::Mkv, V::ProRes422HQ),
            (ExportFormat::Mkv, V::DNxHD145),
            (ExportFormat::Mkv, V::Vp9),
            (ExportFormat::Mp4, V::Vp9),
            (ExportFormat::Mp4, V::Av1),
            (ExportFormat::Mov, V::H265),
            (ExportFormat::WebM, V::Av1),
        ] {
            assert!(f.holds_video(v), "{f:?} refuses {v:?}");
        }
        for (f, a) in [(ExportFormat::Mp4, A::Opus), (ExportFormat::Mp4, A::Flac), (ExportFormat::Mp4, A::Pcm24), (ExportFormat::Mkv, A::Flac), (ExportFormat::Mov, A::Pcm24), (ExportFormat::Mov, A::Mp3)] {
            assert!(f.holds_audio(a), "{f:?} refuses {a:?}");
        }
        // Every container holds its own defaults and every codec fits
        // the container it is moved to.
        for f in [ExportFormat::Mp4, ExportFormat::Mkv, ExportFormat::Mov, ExportFormat::WebM, ExportFormat::Gif] {
            assert!(f.holds_video(f.default_video()), "{f:?}");
            assert!(f.holds_audio(f.default_audio()), "{f:?}");
        }
        for v in [V::H264, V::H265, V::ProRes422, V::ProRes422HQ, V::ProRes4444, V::DNxHD115, V::DNxHD145, V::Av1, V::Vp9, V::Gif] {
            assert!(v.home_format().holds_video(v), "{v:?}");
        }
    }

    #[test]
    fn a_webm_export_with_default_codecs_is_fitted_to_vp9_and_opus() {
        let mut s = ExportSettings { format: ExportFormat::WebM, ..Default::default() };
        assert!(s.container_problem().is_some());
        s.fit_codecs_to_format();
        assert_eq!((s.video_codec, s.audio_codec), (ExportVideoCodec::Vp9, ExportAudioCodec::Opus));
        assert_eq!(s.container_problem(), None);
        // Codecs that fit are kept.
        let mut s = ExportSettings { format: ExportFormat::Mov, video_codec: ExportVideoCodec::ProRes4444, audio_codec: ExportAudioCodec::Pcm24, ..Default::default() };
        s.fit_codecs_to_format();
        assert_eq!((s.video_codec, s.audio_codec), (ExportVideoCodec::ProRes4444, ExportAudioCodec::Pcm24));
    }

    #[test]
    fn gif_codec_uses_ffmpeg_gif_encoder() {
        assert_eq!(ExportVideoCodec::Gif.ffmpeg_codec(), "gif");
    }
}

#[cfg(test)]
mod delivery_tests {
    use super::*;

    /// The HDR setting picks the delivery; an ACES PQ transform makes it
    /// PQ regardless, and renders its own HDR from the SDR master.
    #[test]
    fn delivery_follows_the_hdr_setting_and_the_pq_transforms() {
        let with = |hdr_mode, odt| ExportSettings { hdr_mode, odt, ..Default::default() };
        let s = with(HdrMode::Sdr, OdtChoice::MatchViewer);
        assert_eq!((s.delivery(), s.renders_hdr()), (HdrMode::Sdr, false));
        let s = with(HdrMode::Pq, OdtChoice::MatchViewer);
        assert_eq!((s.delivery(), s.renders_hdr(), s.hdr_peak_nits()), (HdrMode::Pq, true, 1000.0));
        let s = with(HdrMode::Hlg, OdtChoice::MatchViewer);
        assert_eq!((s.delivery(), s.renders_hdr()), (HdrMode::Hlg, true));
        let s = with(HdrMode::Sdr, OdtChoice::Rec2100Pq4000);
        assert_eq!((s.delivery(), s.renders_hdr(), s.hdr_peak_nits()), (HdrMode::Pq, false, 4000.0));
    }

    #[test]
    fn output_transforms_fit_their_deliveries() {
        for odt in OdtChoice::ALL {
            assert_eq!(odt.fits(HdrMode::Sdr), !odt.is_pq(), "{odt:?}");
            assert_eq!(odt.fits(HdrMode::Hlg), odt == OdtChoice::MatchViewer, "{odt:?}");
            assert_eq!(odt.fits(HdrMode::Pq), odt == OdtChoice::MatchViewer || odt.is_pq(), "{odt:?}");
        }
    }
}

#[cfg(test)]
mod hdr_advice_tests {
    use super::*;

    const CODECS: [ExportVideoCodec; 10] = [
        ExportVideoCodec::H264,
        ExportVideoCodec::H265,
        ExportVideoCodec::ProRes422,
        ExportVideoCodec::ProRes422HQ,
        ExportVideoCodec::ProRes4444,
        ExportVideoCodec::DNxHD115,
        ExportVideoCodec::DNxHD145,
        ExportVideoCodec::Av1,
        ExportVideoCodec::Vp9,
        ExportVideoCodec::Gif,
    ];

    /// An SDR export is never second-guessed. H.264 is the right answer
    /// for an SDR web file; the advice exists for the pairing, not the
    /// codec.
    #[test]
    fn sdr_is_never_advised_against() {
        for c in CODECS {
            assert_eq!(c.hdr_advice(HdrMode::Sdr), None, "{c:?} advised against for SDR");
        }
    }

    /// H.264 is steered to H.265 for both HDR transfers — the case the
    /// user hit, where the render failed outright and the dialog had
    /// offered the combination anyway.
    #[test]
    fn h264_hdr_suggests_h265() {
        for mode in [HdrMode::Hlg, HdrMode::Pq] {
            let a = ExportVideoCodec::H264.hdr_advice(mode).expect("H.264 HDR needs advice");
            assert_eq!(a.suggest, Some(ExportVideoCodec::H265), "{mode:?}");
        }
    }

    /// The standard HDR deliveries and the standard HDR mastering codec
    /// pass without comment. A dialog that warned about everything would
    /// train the user to ignore it.
    #[test]
    fn the_good_hdr_codecs_say_nothing() {
        for c in [
            ExportVideoCodec::H265,
            ExportVideoCodec::Av1,
            ExportVideoCodec::ProRes422,
            ExportVideoCodec::ProRes422HQ,
            ExportVideoCodec::ProRes4444,
        ] {
            for mode in [HdrMode::Hlg, HdrMode::Pq] {
                assert_eq!(c.hdr_advice(mode), None, "{c:?} warns for {mode:?}");
            }
        }
    }

    /// Taking the suggestion has to end the conversation: whatever a
    /// codec points at must itself be clean for that delivery. Otherwise
    /// the dialog can send the user in a circle, or to a second warning,
    /// which is worse than the first one.
    #[test]
    fn a_suggestion_lands_somewhere_clean() {
        for c in CODECS {
            for mode in [HdrMode::Hlg, HdrMode::Pq] {
                let Some(a) = c.hdr_advice(mode) else { continue };
                let Some(to) = a.suggest else { continue };
                assert_ne!(to, c, "{c:?} suggests itself for {mode:?}");
                assert_eq!(
                    to.hdr_advice(mode),
                    None,
                    "{c:?} sends {mode:?} to {to:?}, which is itself advised against",
                );
            }
        }
    }

    /// Every codec that cannot deliver HDR says so. The gap this closes
    /// is silence: a codec the dialog might one day offer for HDR, with
    /// no opinion recorded here, reads as approval.
    #[test]
    fn every_codec_has_an_answer_for_hdr() {
        for c in CODECS {
            let ten_bit_capable = matches!(
                c,
                ExportVideoCodec::H264
                    | ExportVideoCodec::H265
                    | ExportVideoCodec::Av1
                    | ExportVideoCodec::Vp9
                    | ExportVideoCodec::ProRes422
                    | ExportVideoCodec::ProRes422HQ
                    | ExportVideoCodec::ProRes4444
            );
            let advice = c.hdr_advice(HdrMode::Pq);
            if !ten_bit_capable {
                assert!(advice.is_some(), "{c:?} cannot carry HDR and the dialog is not told");
            }
            if let Some(a) = advice {
                assert!(!a.headline.is_empty() && !a.detail.is_empty(), "{c:?} has empty advice");
            }
        }
    }
}

#[cfg(test)]
mod codec_hint_tests {
    use super::*;

    /// The dropdown hint stops promising what 10-bit H.264 cannot
    /// deliver. It sat directly above the HDR warning and contradicted
    /// it, which is how the codec got picked in the first place.
    #[test]
    fn the_h264_hint_stops_claiming_gpu_at_ten_bits() {
        let eight = ExportVideoCodec::H264.hint_at_depth(0);
        let ten = ExportVideoCodec::H264.hint_at_depth(10);
        assert_eq!(eight, ExportVideoCodec::H264.hint(), "8-bit H.264 should keep its hint");
        assert_ne!(ten, eight, "10-bit H.264 shows the same hint as 8-bit");
        assert!(!ten.contains("GPU") && !ten.contains('\u{26A1}'), "still claims GPU: {ten}");
        assert!(!ten.contains("universal"), "still claims universal: {ten}");
    }

    /// Every other codec keeps one hint. Only H.264 changes character
    /// with the depth; inventing differences elsewhere would be noise.
    #[test]
    fn no_other_codec_changes_with_depth() {
        for c in [
            ExportVideoCodec::H265,
            ExportVideoCodec::Av1,
            ExportVideoCodec::Vp9,
            ExportVideoCodec::ProRes422,
            ExportVideoCodec::ProRes422HQ,
            ExportVideoCodec::ProRes4444,
            ExportVideoCodec::DNxHD115,
            ExportVideoCodec::DNxHD145,
            ExportVideoCodec::Gif,
        ] {
            assert_eq!(c.hint_at_depth(0), c.hint(), "{c:?} at 8-bit");
            assert_eq!(c.hint_at_depth(10), c.hint(), "{c:?} at 10-bit");
        }
    }
}

/// State the control socket reads without a round trip through the UI:
/// the document revision, and the render queue's jobs as the UI last saw
/// them.
#[derive(Default)]
pub struct ControlShared {
    /// Last interchange import outcome, with its request id and report/error.
    pub import_outcome: std::sync::Mutex<Option<serde_json::Value>>,
    /// The last surface click's outcome (#2754): its request id, and the
    /// corners found or why none were.
    pub surface_snap: std::sync::Mutex<Option<serde_json::Value>>,
    pub revision: std::sync::atomic::AtomicU64,
    pub exports: std::sync::Mutex<Vec<ExportStatus>>,
    /// The open project as the UI last saw it.
    pub project: std::sync::Mutex<ProjectStatus>,
    /// The playhead and whether it is playing, as the UI last saw them.
    /// A frame read moves the viewer to the time it measures and puts
    /// it back here afterwards.
    pub transport: std::sync::Mutex<TransportStatus>,
    /// The compare the viewer is showing, when one is on — a split
    /// against the ungraded frame or a grade still — so a client reading
    /// the viewer's frame knows the person is looking at something else.
    pub viewer_compare: std::sync::Mutex<Option<String>>,
    /// Proxies as the UI last saw them (#2478).
    pub proxies: std::sync::Mutex<ProxyStatus>,
    /// What the viewer shows of the picture, as the UI last saw it
    /// (#2758): a frame read that views a channel or a node puts this
    /// back afterwards.
    pub viewing: std::sync::Mutex<ViewerShows>,
}

/// The viewer's channel and the node it shows in place of the finished
/// picture, if any (#2758).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewerShows {
    pub channel: crate::ViewChannel,
    pub solo: Option<klipp_graph_format::path::NodeOutput>,
}

/// Proxies as a client sees them (#2478): whether playback reads them,
/// and the ones being made or that failed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProxyStatus {
    /// Playback reads a file's proxy where it has one, else the original.
    pub use_proxies: bool,
    /// Files whose proxy is being made, with how far along it is (0..1).
    pub making: Vec<(MediaId, f32)>,
    /// Files whose last proxy failed, with why.
    pub failed: Vec<(MediaId, String)>,
}

/// Where the user's playhead is, as the UI last saw it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TransportStatus {
    pub playhead: Option<RationalTime>,
    pub playing: bool,
}

/// The open project as a client sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectStatus {
    /// Where it is saved; `None` for a project never saved.
    pub path: Option<PathBuf>,
    /// Changes since the last save.
    pub dirty: bool,
    /// How many times a project has been opened or saved this session:
    /// a client that asks for one waits for this to move.
    pub generation: u64,
    /// Why the last save or open failed, until the next one succeeds.
    pub last_error: Option<String>,
    /// Which document this is: moves when the project itself changes —
    /// made new, opened, saved to another path — and not when it is
    /// saved where it already was. A client's timeline and media ids
    /// stay good for as long as this does not move; `generation` moves
    /// on every save, for a client waiting on one.
    ///
    /// It starts from [`launch_identity_base`], a random base for each
    /// run of Klipp, so an identity a client kept from before a restart
    /// never names a project after it, though the document revision
    /// starts again from 0 (#693).
    #[serde(default)]
    pub identity: u64,
    /// What made the project unsaved, when it is: "edits", or something
    /// that changed it on its own, such as analysis markers arriving.
    #[serde(default)]
    pub changed_by: Option<String>,
    /// What Undo would take back and Redo put back now, as the edit
    /// history names them ("Compose", "Add Title"): an undo can say what
    /// it undid (#741).
    #[serde(default)]
    pub undo: Option<String>,
    #[serde(default)]
    pub redo: Option<String>,
    /// How many steps Undo and Redo can take now, for a client checking
    /// that something wrote no history.
    #[serde(default)]
    pub undo_depth: usize,
    #[serde(default)]
    pub redo_depth: usize,
    /// Which history the next undo and redo come from: "project" for a
    /// step of the whole project's, "sequence" for the open sequence's
    /// (#1986).
    #[serde(default)]
    pub undo_scope: Option<String>,
    #[serde(default)]
    pub redo_scope: Option<String>,
    /// The project's global LUT, kept when its file is missing (#1019),
    /// so `check` can say so.
    #[serde(default)]
    pub global_lut: Option<PathBuf>,
    /// What the viewer's frame goes through after the grade, as the UI
    /// has it set: the global LUT's path, switch and opacity, the SDR
    /// viewer transfer and the HDR monitor mode, as one text. A reviewed
    /// colour finding's id holds it, so a change to any lapses the
    /// review (#1976).
    #[serde(default)]
    pub picture_transform: String,
    /// The LUT the viewer applies over the picture and at what opacity,
    /// when it is switched on: what a frame rendered off screen applies
    /// too (#2491).
    #[serde(default)]
    pub viewer_lut: Option<(PathBuf, f32)>,
    /// Every sequence in the project, in the order its tabs list them,
    /// and which one is open: a client can reach each (M3 step 18).
    #[serde(default)]
    pub sequences: Vec<SequenceSummary>,
    #[serde(default)]
    pub active_sequence: usize,
    /// Metadata from the welcome screen, for the same-user control client.
    #[serde(default)]
    pub recent_projects: Vec<RecentProjectSummary>,
}

/// Deliberately limited recent-project metadata: no thumbnail, media paths
/// or project contents. Paths are data for the local control client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentProjectSummary {
    pub path: PathBuf,
    pub name: String,
    pub last_saved: Option<String>,
    pub duration_secs: Option<f64>,
    pub fps: Option<String>,
    pub resolution: Option<String>,
    pub missing_media: Option<bool>,
}

impl ProjectStatus {
    /// Run against the UI's current state before replacing a project.
    /// A returned path must be saved successfully before replacement.
    pub fn replacement_save_path(&self, expected_identity: u64, save: bool, discard_changes: bool) -> Result<Option<PathBuf>, String> {
        if save && discard_changes { return Err("choose save or discard_changes, not both".into()); }
        if self.identity != expected_identity { return Err("the project changed before the request ran; read status and retry".into()); }
        if !self.dirty { return Ok(None); }
        if save { return self.path.clone().map(Some).ok_or_else(|| "the project has never been saved: save_project with a path first".into()); }
        if discard_changes { return Ok(None); }
        Err("the open project has unsaved changes: save_project first, or pass save true or discard_changes true".into())
    }
}

/// One sequence of the project, as a client lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SequenceSummary {
    pub id: String,
    pub name: String,
    /// Clips on all its tracks.
    pub clips: usize,
    /// How long it runs, in seconds.
    #[serde(default)]
    pub duration_secs: f64,
    /// The sequence that holds it as a compound clip, if one does: it
    /// can't be removed while it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_in: Option<String>,
    /// What it is a version of at another aspect (#2724): the master's
    /// id, its size, and whether the master's cut has changed since.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<VersionSummary>,
    /// Its picture size.
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
}

/// One output of a delivery (#2724): the aspect, as width and height
/// parts, and how it is written.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeliveryOutput {
    pub aspect: (u32, u32),
    pub settings: ExportSettings,
}

/// A version's master and whether it is older than the master's cut.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionSummary {
    pub of: String,
    pub stale: bool,
}

impl Default for ProjectStatus {
    fn default() -> Self {
        Self { path: None, dirty: false, generation: 0, last_error: None, identity: launch_identity_base(), changed_by: None, undo: None, redo: None, undo_depth: 0, redo_depth: 0, undo_scope: None, redo_scope: None, sequences: Vec::new(), active_sequence: 0, global_lut: None, picture_transform: String::new(), viewer_lut: None, recent_projects: Vec::new() }
    }
}

/// Where this run of Klipp's project identities start (#693): random for
/// each launch, so a plan's identity from an earlier run cannot match a
/// project in this one. It is a multiple of 2^20, room for that many
/// projects a launch, and under 2^52, so a client that reads JSON numbers
/// as doubles reads it exactly.
pub fn launch_identity_base() -> u64 {
    static BASE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BASE.get_or_init(|| {
        let bytes = *uuid::Uuid::new_v4().as_bytes();
        identity_base_from(u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]))
    })
}

/// [`launch_identity_base`] from a random number: a multiple of 2^20
/// under 2^52, never 0, which is what a reply without an identity
/// decodes to.
fn identity_base_from(random: u64) -> u64 {
    ((random % (1 << 32)).max(1)) << 20
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    /// #693: each run of Klipp numbers its projects from its own random
    /// base, so an identity a client kept from an earlier run, with a
    /// revision that started again from 0, cannot match a project now.
    #[test]
    fn identities_from_two_launches_never_meet() {
        let base = launch_identity_base();
        assert_eq!(base, launch_identity_base(), "one base for the whole run");
        assert_eq!(ProjectStatus::default().identity, base, "the first project of a run starts there");
        for random in [0u64, 1, 7, u64::MAX, 0xdead_beef_cafe_f00d] {
            let b = identity_base_from(random);
            assert!(b != 0 && b.is_multiple_of(1 << 20) && b < (1 << 52), "{random}: {b}");
        }
        // Two runs: their identities, a million projects each, apart.
        let (one, two) = (identity_base_from(41), identity_base_from(42));
        assert!(one + (1 << 20) <= two || two + (1 << 20) <= one);
    }
}

impl ControlShared {
    pub fn bump_revision(&self) -> u64 {
        self.revision.fetch_add(1, std::sync::atomic::Ordering::AcqRel) + 1
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Acquire)
    }
}

/// One render job as a client sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportStatus {
    pub job_id: String,
    pub path: PathBuf,
    /// queued, rendering, complete, failed, cancelled.
    pub status: String,
    /// 0 to 1.
    pub progress: f32,
    pub frames_done: u64,
    pub total_frames: u64,
    pub stage: String,
    pub error: Option<String>,
    pub loudness: Option<String>,
    /// How the job decodes and encodes: "Decode: CUDA (NVDEC) | Encode: …".
    #[serde(default)]
    pub hardware: String,
    /// Frames a second since the job started, and how long the rest
    /// should take at that rate. A ninety-second piece once took twenty
    /// minutes with no way to tell in advance.
    #[serde(default)]
    pub fps: Option<f32>,
    #[serde(default)]
    pub eta_secs: Option<u64>,
    /// The size the file will be, from what is written so far (#19).
    #[serde(default)]
    pub projected_bytes: Option<u64>,
    /// The video encoder the job opened (`h264_nvenc`, `libx264`, …):
    /// what encodes, apart from where frames are made (#1924).
    #[serde(default)]
    pub encoder: Option<String>,
    /// The audio encoder (`aac`, `libmp3lame`, `pcm_s24le`, …), also for
    /// sound alone.
    #[serde(default)]
    pub audio_encoder: Option<String>,
    /// Where frames were composited: `GPU`, or `CPU: <reason> (N of M
    /// frames)`, apart from the codec's acceleration (`hardware`).
    #[serde(default)]
    pub compositing: Option<String>,
    /// Where the time went, updated while the job runs and kept once it
    /// ends (#1924).
    #[serde(default)]
    pub timings: Option<ExportTimings>,
    /// The 8-bit banding check of an 8-bit job (#2074): "pending" until
    /// it has run (only while no render runs, so often after the job),
    /// then "bands" or "smooth". None for a 10-bit target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banding: Option<String>,
    /// Advice on the file, never a failure: an 8-bit target of a sequence
    /// whose smooth gradients will show steps (#2074).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Where an export's time goes (#1924): the loudness fit before the
/// render, then per frame the render (decode and source convert inside
/// it), padding, the video encode and the audio, and how frames were
/// composited.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ExportTimings {
    /// Frames rendered so far.
    pub frames: u64,
    /// Seconds since the render loop started.
    pub wall_secs: f64,
    /// Per frame, milliseconds.
    pub render_ms: f64,
    pub decode_ms: f64,
    pub convert_ms: f64,
    pub pad_ms: f64,
    pub encode_ms: f64,
    pub audio_ms: f64,
    /// Frames composited on the GPU stack, and on the CPU.
    pub gpu_composited: u64,
    pub cpu_composited: u64,
    /// Why frames went to the CPU, most frames first.
    pub fallbacks: Vec<CompositeFallback>,
    /// Seconds the loudness measurement and its fit's trial encodes took
    /// before the render (#1930), and how many trials it ran.
    pub loudness_verify_secs: f64,
    pub loudness_trials: u32,
    /// The encoders opened.
    pub video_encoder: Option<String>,
    pub audio_encoder: Option<String>,
}

/// A reason frames were composited on the CPU, and how many.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CompositeFallback {
    pub reason: String,
    pub frames: u64,
}

impl ExportTimings {
    /// Count one frame composited on the CPU for `reason`.
    pub fn cpu_frame(&mut self, reason: &str) {
        self.cpu_composited += 1;
        match self.fallbacks.iter_mut().find(|f| f.reason == reason) {
            Some(f) => f.frames += 1,
            None => self.fallbacks.push(CompositeFallback { reason: reason.to_string(), frames: 1 }),
        }
        self.fallbacks.sort_by_key(|f| std::cmp::Reverse(f.frames));
    }

    /// Where frames were composited, in words: `GPU`, `CPU: <reason>
    /// (N of M frames)`, or both. `None` before the first frame.
    pub fn compositing(&self) -> Option<String> {
        let all = self.gpu_composited + self.cpu_composited;
        if all == 0 {
            return None;
        }
        let Some(first) = self.fallbacks.first() else { return Some("GPU".to_string()) };
        Some(match self.gpu_composited {
            0 => format!("CPU: {} ({} of {all} frames)", first.reason, first.frames),
            gpu => format!("GPU for {gpu} of {all} frames; CPU: {} ({} frames)", first.reason, first.frames),
        })
    }
}

#[cfg(test)]
mod export_timings_tests {
    use super::*;

    /// #1924: a status written before the timings existed still reads,
    /// and one with them round-trips.
    #[test]
    fn a_status_without_timings_still_reads() {
        let old = r#"{"job_id":"j","path":"/o.mp4","status":"complete","progress":1.0,"frames_done":10,"total_frames":10,"stage":"Complete","error":null,"loudness":null}"#;
        let status: ExportStatus = serde_json::from_str(old).unwrap();
        assert!(status.timings.is_none() && status.encoder.is_none() && status.compositing.is_none());
        let mut timings = ExportTimings { frames: 10, gpu_composited: 7, ..Default::default() };
        for _ in 0..3 {
            timings.cpu_frame("motion blur");
        }
        let status = ExportStatus { timings: Some(timings.clone()), compositing: timings.compositing(), ..status };
        let back: ExportStatus = serde_json::from_str(&serde_json::to_string(&status).unwrap()).unwrap();
        assert_eq!(back.timings, Some(timings));
    }

    /// Where frames were composited, in words, the commonest reason first.
    #[test]
    fn compositing_says_where_and_why() {
        let mut t = ExportTimings::default();
        assert_eq!(t.compositing(), None, "before a frame");
        t.gpu_composited = 4;
        assert_eq!(t.compositing().as_deref(), Some("GPU"));
        t.cpu_frame("a transition");
        t.cpu_frame("motion blur");
        t.cpu_frame("motion blur");
        assert_eq!(t.fallbacks[0], CompositeFallback { reason: "motion blur".into(), frames: 2 });
        assert_eq!(t.compositing().as_deref(), Some("GPU for 4 of 7 frames; CPU: motion blur (2 frames)"));
        t.gpu_composited = 0;
        assert_eq!(t.compositing().as_deref(), Some("CPU: motion blur (2 of 3 frames)"));
    }
}

#[cfg(test)]
mod output_size_tests {
    use super::*;

    /// Hunt #2: whatever size is asked for, or the sequence has, the
    /// encoder gets even sides, as 4:2:0 video needs; an even size is
    /// left as it is.
    #[test]
    fn an_export_is_encoded_at_even_sides() {
        let at = |res: Option<(u32, u32)>, seq: (u32, u32)| {
            let s = ExportSettings { resolution: res.map(|(width, height)| Resolution { width, height }), ..Default::default() };
            s.output_size(seq.0, seq.1)
        };
        assert_eq!(at(Some((1081, 721)), (1920, 1080)), (1080, 720));
        assert_eq!(at(None, (1919, 1079)), (1918, 1078));
        assert_eq!(at(None, (1920, 1080)), (1920, 1080));
        assert_eq!(at(Some((1, 1)), (1920, 1080)), (2, 2), "never nothing");
        assert_eq!(at(Some((99999, 99999)), (1920, 1080)), (16384, 16384), "never past 16K (#658)");
    }
}

/// A viewer overlay the inspector can switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewerOverlay {
    SafeAreas,
    Grid,
    Checkerboard,
}

/// The clip a wipe compares against (#899): the picture clip before or
/// after the one graded, in timeline order, or one named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WipeClip {
    Previous,
    Next,
    Clip(ClipId),
}

#[cfg(test)]
mod project_switch_tests {
    use super::ProjectStatus;
    #[test]
    fn project_switch_preserves_unsaved_work_and_checks_identity() {
        let mut p = ProjectStatus { path:Some("/tmp/film.klipp".into()), identity:7, ..Default::default() };
        assert_eq!(p.replacement_save_path(7,false,false).unwrap(),None);
        p.dirty=true;
        assert!(p.replacement_save_path(7,false,false).unwrap_err().contains("unsaved"));
        assert_eq!(p.replacement_save_path(7,true,false).unwrap(),p.path);
        assert_eq!(p.replacement_save_path(7,false,true).unwrap(),None);
        assert!(p.replacement_save_path(7,true,true).is_err());
        assert!(p.replacement_save_path(6,false,true).is_err());
        p.path=None;
        assert!(p.replacement_save_path(7,true,false).unwrap_err().contains("never been saved"));
        p.dirty=false;
        assert_eq!(p.replacement_save_path(7,false,false).unwrap(),None);
    }
}

/// Where an explicitly requested subject matte is bound. None on a
/// TrackSubject command continues the pass without changing the graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectMatteUse { Grade, CutOut }
