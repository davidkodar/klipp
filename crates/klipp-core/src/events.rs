use crate::id::{BinId, ClipId, GroupId, MarkerId, MediaId, TrackId};
use crate::modifier::{Modifier, ParamId};
use crate::time::{Fps, RationalTime, Timecode};
use crate::types::Resolution;
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

/// Display-boundary tone-map operator.
///
/// Picked by the user as a viewer / export "colour science" option.
/// Lives in `klipp-core` so `ExportSettings` can carry it without
/// pulling in `klipp-color`; the actual curves live in
/// [`klipp_color::display`] and match one-to-one on these variants.
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
/// the actual ODT curves live in [`klipp_color::aces`] and the
/// dispatch in [`klipp_color::display::display_transform_for_export`].
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
            _ => OdtChoice::MatchViewer,
        }
    }

    /// Iteration order for menus (matches the discriminant order of
    /// the supported variants — `Unknown` is excluded; it's a serde
    /// sentinel, not a user choice).
    pub const ALL: [OdtChoice; 6] = [
        OdtChoice::MatchViewer,
        OdtChoice::Rec709Sdr,
        OdtChoice::Rec2100Pq1000,
        OdtChoice::Rec2100Pq4000,
        OdtChoice::DciP3D65_48,
        OdtChoice::Srgb,
    ];

    /// Human-readable label for menus.
    pub const fn label(self) -> &'static str {
        match self {
            OdtChoice::MatchViewer => "Match Viewer",
            OdtChoice::Rec709Sdr => "Rec.709 SDR",
            OdtChoice::Rec2100Pq1000 => "Rec.2100 PQ (1000 nit)",
            OdtChoice::Rec2100Pq4000 => "Rec.2100 PQ (4000 nit)",
            OdtChoice::DciP3D65_48 => "DCI-P3 D65 (48 nit)",
            OdtChoice::Srgb => "sRGB (legacy)",
            OdtChoice::Unknown => "Unknown",
        }
    }
}

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
    /// Embed sequence markers as chapter metadata in the output container.
    /// Only honoured for MP4/MKV/MOV — WebM has no chapter concept.
    pub embed_chapters: bool,
    /// Requested encoder bit depth. `0` = codec default (8-bit); `10` enables
    /// 10-bit encode for H.264/H.265. Other codecs ignore this (ProRes is
    /// always 10-bit, DNxHD is 8-bit). #65 baseline.
    pub bit_depth: u8,
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
            audio_bitrate_kbps: Some(192),
            video_bitrate_kbps: None,
            rate_control: RateControl::default(),
            burn_in_subtitles: true,
            embed_subtitles: false,
            target_lufs: None,
            embed_chapters: false,
            bit_depth: 0,
            hdr_mode: HdrMode::default(),
            tone_map_op: ToneMapOp::default(),
            odt: OdtChoice::default(),
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
}

impl ExportFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Mov => "mov",
            Self::WebM => "webm",
            Self::Gif => "gif",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Mkv => "MKV",
            Self::Mov => "MOV",
            Self::WebM => "WebM",
            Self::Gif => "GIF",
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
        !matches!(self, Self::Gif)
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
        }
    }

    pub fn uses_crf(&self) -> bool {
        matches!(self, Self::H264 | Self::H265 | Self::Av1 | Self::Vp9)
    }

    pub fn supports_preset(&self) -> bool {
        matches!(self, Self::H264 | Self::H265 | Self::Av1)
    }

    pub fn requires_mov(&self) -> bool {
        matches!(
            self,
            Self::ProRes422 | Self::ProRes422HQ | Self::ProRes4444 | Self::DNxHD115 | Self::DNxHD145
        )
    }

    /// Whether this codec requires a WebM container.
    pub fn requires_webm(&self) -> bool {
        matches!(self, Self::Vp9)
    }

    /// Whether this codec requires the GIF container. Mutually
    /// exclusive with every other format — no other codec writes to
    /// `.gif`, and the GIF codec doesn't go anywhere else.
    pub fn requires_gif(&self) -> bool {
        matches!(self, Self::Gif)
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
            Self::Copy => "Copy Source",
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

#[derive(Debug, Clone)]
pub enum AppCommand {
    // Transport
    Play,
    Pause,
    Stop,
    SeekTo(Timecode),
    SeekRelative(i64),
    SetPlayheadFromTimecode { input: String },
    RequestWhiteBalanceSample { nx: f32, ny: f32 },
    RequestQualifierSample { nx: f32, ny: f32 },
    BeginTitleEdit { clip_id: ClipId },
    EndTitleEdit,

    // Media
    LoadMediaFile { path: PathBuf, media_id: MediaId },
    ImportMedia { path: PathBuf },
    /// Recursively import all media files from a directory.
    ImportMediaFolder { directory: PathBuf },

    // Loop
    ToggleLoopPlayback,
    SetLoopPlayback(bool),

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
    MoveClip { clip_id: ClipId, new_timeline_in: Timecode },
    MoveClipToTrack { clip_id: ClipId, new_timeline_in: Timecode, from_track: usize, to_track: usize },
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
    CreateProject {
        name: String,
        location: PathBuf,
        fps_num: u32,
        fps_den: u32,
        width: u32,
        height: u32,
        sequence_name: String,
    },
    RemoveRecentProject { path: PathBuf },
    DeleteProject { path: PathBuf },
    RenameProjectOnDisk { old_path: PathBuf, new_name: String },
    /// Set project thumbnail from the current viewer frame.
    SetProjectThumbnailFromViewer,
    /// Set project thumbnail from an external image file.
    SetProjectThumbnailFromFile { path: PathBuf },

    // Sequences
    AddSequence { name: String, fps_num: u32, fps_den: u32, width: u32, height: u32 },
    DuplicateSequence { sequence_idx: usize, new_name: String },
    SetActiveSequence { sequence_idx: usize },
    RenameSequence { sequence_idx: usize, new_name: String },
    DeleteSequence { sequence_idx: usize },
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
    Redo,
    SequenceUpdated,
    SetAntiClickFade(bool),

    // Export
    ShowExportDialog,
    ExportSequence { settings: ExportSettings },
    CancelExport,
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
    /// Save current viewer frame as PNG via a file dialog.
    FrameGrab,
    /// Save the current viewer frame as a PNG under `<project>/media/stills/`
    /// (or a temp directory when the project is unsaved) and import it into
    /// the media bin as a still MediaItem. No file dialog.
    ExtractFrameToBin,
    /// Import SRT/VTT subtitle file onto a subtitle track.
    ImportSubtitles { path: PathBuf },
    /// Import a Kdenlive MLT XML project as a new sequence.
    /// Unsupported features (filters, non-cross-dissolve transitions, keyframes)
    /// are logged to a `<name>.import_warnings.txt` file alongside the source.
    ImportKdenlive { path: PathBuf },
    /// Parse an OpenTimelineIO `.otio` JSON file and import its timeline as
    /// a new Sequence. Unsupported features (transitions, effects, nested
    /// stacks) are logged to a `<name>.import_warnings.txt` file alongside
    /// the source.
    ImportOtio { path: PathBuf },
    /// Parse a Final Cut Pro XML (`.fcpxml`) file and import its timeline as
    /// a new Sequence. Unsupported features (effects, transitions, compound
    /// clips, multicam, roles, connected clips) are logged to a
    /// `<name>.import_warnings.txt` file alongside the source.
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
    /// Switch multicam angle (1-indexed).
    SwitchMulticamAngle { angle: usize },
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
    SetSequenceOutPoint(RationalTime),
    ClearSequenceInPoint,
    ClearSequenceOutPoint,

    // Proxy
    GenerateProxy { media_id: MediaId },
    CancelProxy { media_id: MediaId },
    SetUseProxies(bool),
    /// Begin timeline scrubbing — engine may switch to proxy decode for speed.
    /// Enable or disable the smart render cache.
    SetRenderCacheEnabled(bool),
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
    /// End timeline scrubbing — engine restores full-res decode.
    EndScrub,
    /// Relink a specific offline media item to a new path.
    RelinkMedia { media_id: MediaId, new_path: PathBuf },
    /// Relink all offline media by searching a directory recursively.
    RelinkAllFromDirectory { directory: PathBuf },
    /// Show the media relink dialog.
    ShowRelinkDialog,
    AddMediaTag { media_id: MediaId, tag: String },
    RemoveMediaTag { media_id: MediaId, tag_index: usize },

    // Node graph (color grading)
    AddNode { clip_id: ClipId, kind: String, position: (f32, f32) },
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
    /// Set the node-graph "solo" target — the engine renders the
    /// graph normally but returns the soloed node's output as the
    /// final image, so the user can preview any mid-chain node as
    /// if it were the Output. `None` clears solo and restores the
    /// normal Output-node-input result. Single global slot (one
    /// active clip at a time); auto-inactive when the soloed id
    /// doesn't exist in the current clip's graph.
    SetNodeSolo { node_id: Option<uuid::Uuid> },
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
    RenameNode { clip_id: ClipId, node_id: uuid::Uuid, label: String },
    SetScopeTapNode { clip_id: ClipId, node_id: Option<uuid::Uuid> },
    /// Set which scope mode the engine should render (0=waveform, 1=parade, 3=histogram).
    SetScopeRenderMode { mode: u8 },
    /// Tell the engine whether the scope panel is actually visible
    /// in the UI. The engine uses this to skip the per-frame
    /// `linear_rgba_to_u8` re-encode and the GPU scope shader pass —
    /// both fire from `post_process_rgba` regardless of mode and
    /// cost ~12 ms / 1080p frame on CPU. Without the flag the
    /// engine has no way to know the scope panel is hidden and
    /// burns the budget on data nobody is looking at.
    SetScopesVisible(bool),
    /// Render the engine's GPU scope shaders against an arbitrary RGBA frame
    /// (used for media-browser hover skim and timeline scrub) and write the
    /// resulting 720×256 texture into `frame_buffer.scope_texture`. Same code
    /// path the playback pipeline uses, so skim scopes match playback quality
    /// pixel-for-pixel.
    RenderScopesForFrame { rgba: Vec<u8>, width: u32, height: u32 },
    SetNodeParam { clip_id: ClipId, node_id: uuid::Uuid, param: String, value: f32 },
    /// Called when the user starts dragging a color-grading parameter
    /// (color wheel, sat/bri slider, scrubby slider, etc.). Snapshots the
    /// current node graph so the gesture can be committed as one undo entry.
    BeginNodeParamEdit { clip_id: ClipId },
    /// Called when the user releases a color-grading parameter drag. Commits
    /// the snapshot taken at the matching BeginNodeParamEdit as a single
    /// `SetNodeGraph` history entry covering the whole gesture.
    EndNodeParamEdit { clip_id: ClipId },
    SetNodeCurves { clip_id: ClipId, node_id: uuid::Uuid, curves: crate::color_grade::CurveSet },
    SetNodeHueSatCurves { clip_id: ClipId, node_id: uuid::Uuid, huesat: crate::color_grade::HueSatCurves },
    SetNodeQualifier { clip_id: ClipId, node_id: uuid::Uuid, qualifier: crate::color_grade::QualifierSettings },
    SetNodeLumaKey { clip_id: ClipId, node_id: uuid::Uuid, luma_key: crate::color_grade::LumaKeySettings },
    SetNodeSoftClip { clip_id: ClipId, node_id: uuid::Uuid, soft_clip: crate::color_grade::SoftClipSettings },
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
    /// Tell the engine which nodes the V2 canvas currently has
    /// visible — the executor captures a downsampled thumbnail of
    /// each of them on the paused / post-seek re-grade so the canvas
    /// can paint per-node previews. Empty list disables thumb capture.
    /// Sent when the viewport's node set changes (pan / zoom / add /
    /// remove). Playback path skips capture entirely to avoid the N
    /// extra GPU readbacks per frame.
    SetVisibleNodeThumbs { clip_id: ClipId, node_ids: Vec<uuid::Uuid> },
    /// Render per-node thumbnails by running the clip's node graph on a
    /// caller-supplied pre-grade source bitmap (typically the filmstrip
    /// tile nearest the playhead). Used by the V2 canvas to populate
    /// node previews without waiting on a full-res decoder seek. Source
    /// is sRGB u8 RGBA at the given width × height; engine evaluates
    /// time-keyed grade params at `playhead` so keyframes remain
    /// frame-accurate even though the source bitmap came from a
    /// slightly different timestamp.
    RenderNodeThumbsFromSource {
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
    /// Add or update a keyframe on a mask parameter.
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
    /// Trigger a grade refresh without changing any params (used after bulk preset apply).
    RefreshGrade,
    ToggleColorPanel,
    ShowEffectsBrowser,
    ToggleBeforeAfter,
    /// Toggle stabilization compare split view (raw vs stabilized).
    /// Mutually exclusive with ToggleBeforeAfter and grade still compare.
    ToggleStabilizationCompare,
    /// Activate / deactivate per-effect split compare. `clip_id = None`
    /// deactivates and clears the compare state on both sides. `is_ofx`
    /// tells the engine which effect Vec on the clip to target.
    SetSplitCompareEffect {
        clip_id: Option<ClipId>,
        effect_index: usize,
        is_ofx: bool,
    },
    SetMattePreview(bool),

    /// Phase 4C — Inspector requests track-matte preview thumbnails for
    /// the given consumer clip. `None` clears the request. Engine emits
    /// `AppEvent::MatteMaskReady` per composited frame while a target
    /// is set; the inspector renders the latest as a small grayscale
    /// preview in the COMPOSITING section.
    SetInspectorMattePreviewTarget(Option<ClipId>),
    SetClipLogFormat { clip_id: ClipId, log_format: crate::types::LogFormat },
    /// Auto white balance: analyze current frame and set temperature/tint on the clip's ColorBalance node.
    AutoWhiteBalance { clip_id: ClipId },

    // Global LUT
    LoadGlobalLut { path: PathBuf },
    ClearGlobalLut,
    SetGlobalLutOpacity(f32),
    SetGlobalLutEnabled(bool),
    /// Switch LUT interpolation: true = tetrahedral, false = trilinear.
    SetLutTetrahedral(bool),

    // Effects (OFX)
    AddEffect { clip_id: ClipId, plugin_name: String },
    RemoveEffect { clip_id: ClipId, effect_index: usize },
    SetEffectEnabled { clip_id: ClipId, effect_index: usize, enabled: bool },
    ReorderEffect { clip_id: ClipId, from_index: usize, to_index: usize },
    SetEffectParam { clip_id: ClipId, effect_index: usize, param_name: String, value: crate::OfxValue },

    // Built-in effects
    AddBuiltinEffect { clip_id: ClipId, effect: klipp_effects::BuiltinEffect },
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
    /// Whether multicam clips show angle-number overlays. Mirrors
    /// `prefs.editing.multicam_show_angles` ↔
    /// `state.multicam_show_angles`.
    SetMulticamShowAngles(bool),
    SetRippleEnabled(bool),

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
    LiftEdit,
    ExtractEdit,

    // Transitions
    AddTransition { track_index: usize, clip_a_id: ClipId, clip_b_id: ClipId, kind_id: u8, duration_frames: u32 },
    RemoveTransition { track_index: usize, transition_id: crate::id::TransitionId },
    SetTransitionDuration { track_index: usize, transition_id: crate::id::TransitionId, duration_frames: u32 },
    SetTransitionKind { track_index: usize, transition_id: crate::id::TransitionId, kind_id: u8 },
    SetTransitionParam { track_index: usize, transition_id: crate::id::TransitionId, user_params: crate::types::TransitionUserParams },
    SetTransitionAlignment { track_index: usize, transition_id: crate::id::TransitionId, alignment: crate::types::TransitionAlignment },

    // Title clips
    CreateTitleClip { track_index: usize, timeline_in: Timecode, duration_frames: u32 },
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
    SetTitleAlignment { clip_id: ClipId, alignment: u8 },  // 0=Left, 1=Center, 2=Right
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
    /// 1 = Shape (rectangle filling the centre of the frame).
    /// The new object is appended at the top of the stack and
    /// becomes the selected object so the user can immediately
    /// drag / edit it.
    AddTitleObject { clip_id: ClipId, kind: u8 },
    /// Remove an object from a title. Selection clears if the
    /// removed object was the active one.
    DeleteTitleObject { clip_id: ClipId, object_id: u32 },
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
    SetTitleShapeFill { clip_id: ClipId, object_id: u32, color: Option<[u8; 4]> },
    SetTitleShapeStroke { clip_id: ClipId, object_id: u32, color: Option<[u8; 4]>, width: f32 },
    SetTitleShapeCornerRadius { clip_id: ClipId, object_id: u32, radius: f32 },

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
    SetTitleBackground { clip_id: ClipId, bg_kind: u8, color: [u8; 4] },  // 0=None, 1=Color, 2=FullFrame
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
    /// Set write-on reveal progress (0.0-1.0).
    SetTitleWriteOn { clip_id: ClipId, write_on: f32 },
    /// Set line spacing multiplier.
    SetTitleLineSpacing { clip_id: ClipId, line_spacing: f32 },
    /// Toggle text underline rendering.
    SetTitleUnderline { clip_id: ClipId, underline: bool },
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
    /// Set text direction (0 = Ltr, 1 = Rtl).
    SetTitleDirection { clip_id: ClipId, direction: u8 },
    /// Set 3D text extrusion. `enabled=false` = disable.
    SetTitleExtrusion { clip_id: ClipId, enabled: bool, depth: f32, angle: f32, face_color: [u8; 4], side_color: [u8; 4], lighting: f32 },
    /// Set text path. path_kind: 0=None, 1=Arc, 2=Circle, 3=Wave.
    /// For Arc: p1=radius, p2=start_angle, p3=sweep
    /// For Circle: p1=radius
    /// For Wave: p1=amplitude, p2=frequency, p3=phase
    SetTitleTextPath { clip_id: ClipId, path_kind: u8, p1: f32, p2: f32, p3: f32 },
    /// Set per-character animation. entrance: 0=None, 1=FadeIn, 2=SlideUp, 3=SlideDown, 4=ScaleUp.
    /// Set enabled=false to disable entirely.
    SetTitleCharAnimation { clip_id: ClipId, enabled: bool, stagger: f32, entrance: u8, wave_amplitude: f32, wave_speed: f32 },
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
    /// `property`: "font_size", "color", "font_style", "tracking", "baseline_offset"
    /// `value`: JSON-encoded value for the property.
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
    /// Set the title motion animation. The handler picks which
    /// clip-level keyframe channel(s) to write (`opacity_keyframes`
    /// for fade/flash/blink/pulse, `transform_scale_keyframes` for
    /// zoom, `transform_pos_x/y_keyframes` for scroll/crawl/slide-in)
    /// based on the variant.
    ///
    /// `animation_kind` codes (see `decode_title_animation` in
    /// klipp-ui/src/app.rs for the canonical mapping):
    ///   0  Static
    ///   1  ScrollUp        2  ScrollDown        3  CrawlLeft       4  CrawlRight
    ///   5  Typewriter
    ///   6  FadeIn          7  FadeOut           8  FadeInOut
    ///   9  Flash          10  Blink            11  Pulse
    ///  12  ZoomIn         13  ZoomOut
    ///  14  SlideInLeft    15  SlideInRight    16  SlideInTop    17  SlideInBottom
    ///
    /// `speed` is the primary numeric parameter and meaning depends
    /// on the variant: speed_px_per_sec for scroll/crawl, chars_per_sec
    /// for Typewriter, duration_secs for fade/zoom/slide-in,
    /// rate_hz for flash/blink/pulse.
    /// `param2` is the secondary parameter used by variants that need
    /// one (Blink: duty_cycle; Pulse: min_opacity; ZoomIn: from_scale;
    /// ZoomOut: to_scale). Ignored otherwise.
    SetTitleAnimation {
        clip_id: ClipId,
        animation_kind: u8,
        speed: f32,
        param2: f32,
    },
    /// Counter has more parameters than `SetTitleAnimation` carries,
    /// so it gets its own event. `from_value` / `to_value` are the
    /// interpolation endpoints (linearly tweened over `duration_secs`
    /// from clip start; held at `to_value` after). `decimals` is the
    /// formatted-output decimal-place count, clamped to 0..=3.
    SetTitleCounterAnimation {
        clip_id: ClipId,
        from_value: f64,
        to_value: f64,
        duration_secs: f32,
        decimals: u8,
    },
    /// ColorBlend has two RGBA colour endpoints + a rate, doesn't
    /// fit `SetTitleAnimation`'s scalar pair. Direct event so the
    /// Inspector's two colour pickers can drive both endpoints.
    SetTitleColorBlend {
        clip_id: ClipId,
        color_a: [u8; 4],
        color_b: [u8; 4],
        rate_hz: f32,
    },

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
    SetAudioAutomationLaneParam { track_idx: usize, param: AudioParam },
    /// Begin capturing audio from the configured input device into a WAV file
    /// in the proxy cache. Does nothing if no track is armed.
    StartRecording,
    /// Stop the active recording, close the WAV file, and auto-import it.
    StopRecording,

    // Media pool management
    RemoveFromPool { media_id: MediaId },
    ConfirmRemoveFromPool { media_id: MediaId },
    SetMediaRating { media_id: MediaId, rating: u8 },

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

    // Track locking
    SetTrackLocked { track_id: TrackId, locked: bool },

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
    AddAudioKeyframe { clip_id: ClipId, param: AudioParam, time_secs: f64, value: f32 },
    RemoveAudioKeyframe { clip_id: ClipId, param: AudioParam, time_secs: f64 },
    MoveAudioKeyframe { clip_id: ClipId, param: AudioParam, index: usize, new_time_secs: f64, new_value: f32 },
    SetAudioKeyframeInterpolation { clip_id: ClipId, param: AudioParam, keyframe_index: usize, interpolation: crate::keyframe::Interpolation },
    SetAudioKeyframeTangent { clip_id: ClipId, param: AudioParam, keyframe_index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)> },

    // Video keyframes
    AddVideoKeyframe { clip_id: ClipId, param: VideoParam, time_secs: f64, value: f32 },
    RemoveVideoKeyframe { clip_id: ClipId, param: VideoParam, time_secs: f64 },
    MoveVideoKeyframe { clip_id: ClipId, param: VideoParam, index: usize, new_time_secs: f64, new_value: f32 },
    SetVideoKeyframeInterpolation { clip_id: ClipId, param: VideoParam, keyframe_index: usize, interpolation: crate::keyframe::Interpolation },
    SetVideoKeyframeTangent { clip_id: ClipId, param: VideoParam, keyframe_index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)> },

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

    // Clip speed
    SetClipSpeed { clip_id: ClipId, speed: f64 },

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

    // Video stabilization
    AnalyzeStabilization { clip_id: ClipId },
    CancelStabilization { clip_id: ClipId },

    // Scene (shot-boundary) detection
    DetectScenes { clip_id: ClipId },
    CancelSceneDetect { clip_id: ClipId },
    /// Razor-split a clip at every scene marker it carries, atomically.
    /// Undo restores the original single clip in one step.
    SplitAtSceneMarkers { clip_id: ClipId },
    /// Re-run smoothing on existing stabilization_data with the current smoothing
    /// slider value. Cheap (microseconds) — no decode, just gaussian over the
    /// already-baked trajectory. Used for live slider updates.
    ResmoothStabilization { clip_id: ClipId },

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
    TrackMaskRegion { clip_id: ClipId, node_id: uuid::Uuid, mask_index: usize },
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
    ApplyLook { look_id: uuid::Uuid, clip_ids: Vec<ClipId> },
    DeleteLook { look_id: uuid::Uuid },
    RenameLook { look_id: uuid::Uuid, new_name: String },

    // Grade stills (reference frame comparison)
    /// Capture the current viewer frame as a grade reference still.
    CaptureGradeStill { name: String },
    /// Delete a saved grade still by index.
    DeleteGradeStill { index: usize },
    /// Set the active grade still for comparison.
    SetActiveGradeStill { index: Option<usize> },
    /// Toggle split-view comparison with the active grade still.
    ToggleGradeStillCompare,

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
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    // Playback state
    PlaybackStarted,
    PlaybackPaused,
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
    MediaLoadFailed {
        media_id: MediaId,
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
    MasterPeakLevels { left: f32, right: f32 },
    /// BS.1770-4 loudness meter readings from the mix thread.
    LufsReadings { momentary: f32, short_term: f32, integrated: f32, loudness_range: f32, true_peak_dbfs: f32 },

    // Performance stats
    EngineStats { dropped_frames: u64, hw_decode: bool },

    // Skim transcode
    SkimTranscodeProgress { media_id: MediaId, progress: f32 },
    SkimTranscodeComplete { media_id: MediaId, path: PathBuf },
    SkimTranscodeFailed { media_id: MediaId, path: PathBuf, error: String },

    // Browser skim frame
    BrowserSkimFrameReady { media_id: MediaId, rgba: Vec<u8>, width: u32, height: u32, target: ViewerMode },

    // Video stabilization
    StabilizationProgress { clip_id: ClipId, progress: f32 },
    StabilizationComplete { clip_id: ClipId },
    StabilizationFailed { clip_id: ClipId, error: String },

    // Scene (shot-boundary) detection
    SceneDetectProgress { clip_id: ClipId, progress: f32 },
    SceneDetectComplete { clip_id: ClipId, cut_times: Vec<f64> },
    SceneDetectFailed { clip_id: ClipId, error: String },

    // Point tracking
    TrackingProgress { clip_id: ClipId, progress: f32 },
    TrackingComplete { clip_id: ClipId, point_index: usize, positions: Vec<(f64, f64)>, start_frame: usize },
    TrackingFailed { clip_id: ClipId, error: String },

    // Audio sync (#61)
    /// Background cross-correlation finished. `offset_samples` at 48 kHz —
    /// positive means the target clip was found to start _later_ than the
    /// reference; negative means _earlier_. `ref_timeline_in_secs` is the
    /// reference clip's timeline position at the moment the job was spawned
    /// (read on the UI thread, shipped to the worker, and echoed back so the
    /// completion handler has it without another lookup).
    AudioSyncComplete {
        clip_id: ClipId,
        offset_samples: i64,
        ref_timeline_in_secs: f64,
    },
    AudioSyncFailed { clip_id: ClipId, error: String },
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
/// frame, which applies the change to the shared `Arc<RwLock<Sequence>>`
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
        assert_eq!(OdtChoice::Unknown.as_u8(), 255);
    }

    #[test]
    fn unknown_u8_falls_back_to_match_viewer() {
        // A corrupted atomic / FFI integer must default to the safe
        // viewer path — never produce a delivery-target surprise.
        for tag in [6u8, 7, 42, 100, 200, 254] {
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

    #[test]
    fn gif_codec_is_mutually_exclusive() {
        // The GIF codec only fits the GIF container; no other codec
        // claims it. Both directions tested.
        assert!(ExportVideoCodec::Gif.requires_gif());
        for c in [
            ExportVideoCodec::H264,
            ExportVideoCodec::H265,
            ExportVideoCodec::ProRes422,
            ExportVideoCodec::ProRes422HQ,
            ExportVideoCodec::ProRes4444,
            ExportVideoCodec::DNxHD115,
            ExportVideoCodec::DNxHD145,
            ExportVideoCodec::Av1,
            ExportVideoCodec::Vp9,
        ] {
            assert!(!c.requires_gif(), "codec {:?} must not claim GIF", c);
        }
        // The GIF codec is also explicitly NOT in any other
        // container's requirement set.
        assert!(!ExportVideoCodec::Gif.requires_mov());
        assert!(!ExportVideoCodec::Gif.requires_webm());
    }

    #[test]
    fn gif_codec_uses_ffmpeg_gif_encoder() {
        assert_eq!(ExportVideoCodec::Gif.ffmpeg_codec(), "gif");
    }
}
