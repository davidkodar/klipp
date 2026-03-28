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
}

#[derive(Debug, Clone)]
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
}

impl ExportFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Mov => "mov",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Mkv => "MKV",
            Self::Mov => "MOV",
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
}

impl ExportVideoCodec {
    pub fn ffmpeg_codec(&self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
            Self::ProRes422 | Self::ProRes422HQ | Self::ProRes4444 => "prores_ks",
            Self::DNxHD115 | Self::DNxHD145 => "dnxhd",
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
        }
    }

    /// Short description shown next to the codec in the export dialog.
    pub fn hint(&self) -> &'static str {
        match self {
            Self::H264 => "GPU accelerated \u{26A1} — fast, universal",
            Self::H265 => "GPU accelerated \u{26A1} — best quality/size",
            Self::ProRes422 => "CPU only — NLE interchange, large files",
            Self::ProRes422HQ => "CPU only — high quality NLE interchange",
            Self::ProRes4444 => "CPU only — alpha channel, mastering",
            Self::DNxHD115 => "CPU only — Avid interchange",
            Self::DNxHD145 => "CPU only — Avid interchange, high bitrate",
        }
    }

    /// Whether this codec uses CRF-based quality (H.264/H.265) or fixed profile/bitrate.
    pub fn uses_crf(&self) -> bool {
        matches!(self, Self::H264 | Self::H265)
    }

    /// Whether this codec supports encoder preset (fast/medium/slow/veryslow).
    pub fn supports_preset(&self) -> bool {
        matches!(self, Self::H264 | Self::H265)
    }

    /// Whether this codec requires a MOV container (not compatible with MP4/MKV).
    pub fn requires_mov(&self) -> bool {
        matches!(
            self,
            Self::ProRes422 | Self::ProRes422HQ | Self::ProRes4444 | Self::DNxHD115 | Self::DNxHD145
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ExportAudioCodec {
    Aac,
    Mp3,
    Flac,
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
            Self::Flac => vec!["-c:a".into(), "flac".into()],
            Self::Copy => vec!["-c:a".into(), "copy".into()],
        }
    }

    /// ffmpeg-next codec name for in-process encoding.
    pub fn ffmpeg_codec(&self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::Mp3 => "libmp3lame",
            Self::Flac => "flac",
            Self::Copy => "pcm_s16le", // Copy doesn't apply to in-process; use PCM fallback
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Aac => "AAC",
            Self::Mp3 => "MP3",
            Self::Flac => "FLAC",
            Self::Copy => "Copy Source",
        }
    }
}

#[derive(Debug, Clone)]
pub enum AppCommand {
    // Transport
    Play,
    Pause,
    Stop,
    SeekTo(Timecode),
    SeekRelative(i64),

    // Media
    LoadMediaFile { path: PathBuf, media_id: MediaId },
    ImportMedia { path: PathBuf },

    // Loop
    ToggleLoopPlayback,
    SetLoopPlayback(bool),

    // Timeline
    SetPlayhead(Timecode),
    SetSpeed(f64),
    AppendClip { media_id: MediaId },
    AppendClipToTrack { media_id: MediaId, track_index: usize },
    RemoveClip { clip_id: ClipId },
    MoveClip { clip_id: ClipId, new_timeline_in: Timecode },
    MoveClipToTrack { clip_id: ClipId, new_timeline_in: Timecode, from_track: usize, to_track: usize },
    TrimClip {
        clip_id: ClipId,
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

    // Project
    NewProject,
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

    // Sequences
    AddSequence { name: String, fps_num: u32, fps_den: u32, width: u32, height: u32 },
    DuplicateSequence { sequence_idx: usize, new_name: String },
    SetActiveSequence { sequence_idx: usize },
    RenameSequence { sequence_idx: usize, new_name: String },
    DeleteSequence { sequence_idx: usize },
    ReorderSequences { from_idx: usize, to_idx: usize },

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

    // Compositing
    SetTrackOpacity { track_index: usize, opacity: f32 },
    SetTrackVisibility { track_index: usize, visible: bool },

    // Audio mixing
    SetTrackVolume { track_index: usize, volume: f32 },
    SetTrackMute { track_index: usize, muted: bool },
    SetTrackSolo { track_index: usize, solo: bool },
    SetTrackPan { track_index: usize, pan: f32 },
    SetMasterVolume(f32),
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

    // Source monitor
    LoadSourceMedia { media_id: MediaId },
    SetSourceInPoint(RationalTime),
    SetSourceOutPoint(RationalTime),
    InsertSourceAtPlayhead,

    // Sequence markers
    SetSequenceInPoint(RationalTime),
    SetSequenceOutPoint(RationalTime),
    ClearSequenceInPoint,
    ClearSequenceOutPoint,

    // Proxy
    GenerateProxy { media_id: MediaId },
    CancelProxy { media_id: MediaId },
    SetUseProxies(bool),
    RelinkMedia { media_id: MediaId, new_path: PathBuf },

    // Node graph (color grading)
    AddNode { clip_id: ClipId, kind: String, position: (f32, f32) },
    RemoveNode { clip_id: ClipId, node_id: uuid::Uuid },
    ConnectNodes { clip_id: ClipId, from: uuid::Uuid, from_port: u8, to: uuid::Uuid, to_port: u8 },
    DisconnectEdge { clip_id: ClipId, to: uuid::Uuid, to_port: u8 },
    MoveNode { clip_id: ClipId, node_id: uuid::Uuid, delta_x: f32, delta_y: f32 },
    ToggleNodeEnabled { clip_id: ClipId, node_id: uuid::Uuid },
    RenameNode { clip_id: ClipId, node_id: uuid::Uuid, label: String },
    SetScopeTapNode { clip_id: ClipId, node_id: Option<uuid::Uuid> },
    SetNodeParam { clip_id: ClipId, node_id: uuid::Uuid, param: String, value: f32 },
    SetNodeCurves { clip_id: ClipId, node_id: uuid::Uuid, curves: crate::color_grade::CurveSet },
    SetNodeHueSatCurves { clip_id: ClipId, node_id: uuid::Uuid, huesat: crate::color_grade::HueSatCurves },
    SetNodeQualifier { clip_id: ClipId, node_id: uuid::Uuid, qualifier: crate::color_grade::QualifierSettings },
    SetNodeMasks { clip_id: ClipId, node_id: uuid::Uuid, masks: crate::color_grade::MaskStack },
    AddKeyframe { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, value: f32 },
    RemoveKeyframe { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64 },
    ClearKeyframes { clip_id: ClipId, node_id: uuid::Uuid, param: String },
    SetKeyframeInterpolation { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, interpolation: crate::keyframe::Interpolation },
    SetKeyframeTangents { clip_id: ClipId, node_id: uuid::Uuid, param: String, time_secs: f64, tangent_in: (f64, f32), tangent_out: (f64, f32) },
    ToggleColorPanel,
    ToggleBeforeAfter,
    SetMattePreview(bool),
    SetClipLogFormat { clip_id: ClipId, log_format: crate::types::LogFormat },

    // Global LUT
    LoadGlobalLut { path: PathBuf },
    ClearGlobalLut,
    SetGlobalLutOpacity(f32),

    // Effects (OFX)
    AddEffect { clip_id: ClipId, plugin_name: String },
    RemoveEffect { clip_id: ClipId, effect_index: usize },
    SetEffectEnabled { clip_id: ClipId, effect_index: usize, enabled: bool },
    ReorderEffect { clip_id: ClipId, from_index: usize, to_index: usize },
    SetEffectParam { clip_id: ClipId, effect_index: usize, param_name: String, value: crate::OfxValue },

    // Built-in effects
    AddBuiltinEffect { clip_id: ClipId, effect: klipp_effects::BuiltinEffect },
    RemoveBuiltinEffect { clip_id: ClipId, index: usize },
    UpdateBuiltinEffect { clip_id: ClipId, index: usize, effect: klipp_effects::BuiltinEffect },
    ReorderBuiltinEffect { clip_id: ClipId, from_index: usize, to_index: usize },

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
    SetSnapEnabled(bool),
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

    // Title clips
    CreateTitleClip { track_index: usize, timeline_in: Timecode, duration_frames: u32 },
    UpdateTitleData { clip_id: ClipId, text: String, font_size: f32, font_name: String, font_style: u8, color: [u8; 4] },
    SetTitleAlignment { clip_id: ClipId, alignment: u8 },  // 0=Left, 1=Center, 2=Right
    SetTitlePosition { clip_id: ClipId, x: f32, y: f32 },
    SetTitleBackground { clip_id: ClipId, bg_kind: u8, color: [u8; 4] },  // 0=None, 1=Color, 2=FullFrame
    SetTitleShadow { clip_id: ClipId, enabled: bool, color: [u8; 4], offset_x: f32, offset_y: f32, blur: f32 },
    /// Set text outline. `None` = disable.
    SetTitleOutline { clip_id: ClipId, enabled: bool, color: [u8; 4], width: f32 },
    /// Set text glow. `enabled=false` = disable.
    SetTitleGlow { clip_id: ClipId, enabled: bool, color: [u8; 4], radius: f32, intensity: f32 },
    /// Set gradient fill. `enabled=false` = disable, uses solid color instead.
    SetTitleGradient { clip_id: ClipId, enabled: bool, top_color: [u8; 4], bottom_color: [u8; 4] },
    /// Set multiple outline layers (replaces single outline).
    /// Each tuple is (color_rgba, width_px).
    SetTitleOutlines { clip_id: ClipId, outlines: Vec<([u8; 4], f32)> },
    /// Set inner shadow. `enabled=false` = disable.
    SetTitleInnerShadow { clip_id: ClipId, enabled: bool, color: [u8; 4], offset_x: f32, offset_y: f32, blur: f32 },
    /// Set pattern fill. pattern_kind: 0=None, 1=HStripes, 2=VStripes, 3=Checker, 4=Diagonal
    SetTitlePattern { clip_id: ClipId, pattern_kind: u8, color_a: [u8; 4], color_b: [u8; 4], size: f32 },
    /// Set per-character tracking (extra spacing in px).
    SetTitleTracking { clip_id: ClipId, tracking: f32 },
    /// Set write-on reveal progress (0.0-1.0).
    SetTitleWriteOn { clip_id: ClipId, write_on: f32 },
    /// Set line spacing multiplier.
    SetTitleLineSpacing { clip_id: ClipId, line_spacing: f32 },
    /// Update the title animation fade duration stored in AppState.
    SetTitleFadeDuration { duration: f32 },
    ClearTitleOverlay,

    // Linked clips
    ImportMediaLinked { media_id: MediaId },
    UnlinkClip { clip_id: ClipId },
    LinkClips { clip_a: ClipId, clip_b: ClipId },

    // Markers
    AddMarker { position: Timecode, label: String, color: MarkerColor },
    RemoveMarker { marker_id: MarkerId },
    UpdateMarkerLabel { marker_id: MarkerId, label: String },
    UpdateMarkerColor { marker_id: MarkerId, color: MarkerColor },
    JumpToMarker { marker_id: MarkerId },

    // Per-clip markers
    AddClipMarker { clip_id: ClipId, time_secs: f64, name: String, color: MarkerColor },
    RemoveClipMarker { clip_id: ClipId, index: usize },

    // Dynamic tracks
    AddVideoTrack,
    AddAudioTrack,
    /// Sync auto_create_tracks preference to AppState.
    SyncAutoCreateTracks(bool),
    /// Insert a new track of the same kind above the given track index.
    AddTrackAbove { track_index: usize },
    /// Insert a new track of the same kind below the given track index.
    AddTrackBelow { track_index: usize },
    RemoveTrack { track_id: TrackId },
    RenameTrack { track_id: TrackId, name: String },

    // Media pool management
    RemoveFromPool { media_id: MediaId },
    ConfirmRemoveFromPool { media_id: MediaId },
    SetMediaRating { media_id: MediaId, rating: u8 },

    SetMediaTags { media_id: MediaId, tags: Vec<String> },
    RevealInFileManager { path: PathBuf },
    RevealMediaInBrowser { media_id: MediaId },
    SetMediaColorLabel { media_id: MediaId, label: u8 },
    DeleteUnusedMedia,
    ConfirmDeleteUnusedMedia,
    /// Export despite offline media (user confirmed from warning dialog).
    ForceExportSequence { settings: Box<ExportSettings> },

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

    // Browser skimming
    StartSkimTranscode { media_id: MediaId },
    /// Force-regenerate skim transcode (deletes existing cache first).
    RegenerateSkimTranscode { media_id: MediaId },
    /// Force-regenerate filmstrip thumbnails (deletes existing cache first).
    RegenerateFilmstrip { media_id: MediaId },
    BrowserSkimFrame { media_id: MediaId, time_secs: f64 },

    // Browser audio skimming
    /// Begin an audio skim session for a media item (mouse enters skimmable clip).
    BrowserSkimAudioStart {
        media_id: MediaId,
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

    // UI
    SetViewerMode(ViewerMode),
    SetActiveTool(Tool),
    ToggleFullscreen,
    ShowPreferences,

    // Track locking
    SetTrackLocked { track_id: TrackId, locked: bool },

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

#[derive(Debug, Clone)]
pub enum AppEvent {
    // Playback state
    PlaybackStarted,
    PlaybackPaused,
    PlaybackStopped,
    PlayheadMoved(Timecode),
    LoopedBack,

    // Media
    MediaLoaded {
        media_id: MediaId,
        path: PathBuf,
        duration: Timecode,
        fps: Fps,
        resolution: Resolution,
        video_codec: String,
        audio_codec: String,
        has_audio: bool,
        file_size_bytes: u64,
        sample_rate: Option<u32>,
        audio_channels: Option<u16>,
        is_still: bool,
        file_modified_at: Option<u64>,
        media_created_at: Option<u64>,
        colour_space: u8,
    },
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

    // Audio conforming
    AudioConformStarted { media_id: MediaId },
    AudioConformComplete { media_id: MediaId },
    AudioConformFailed { media_id: MediaId, error: String },

    // Audio metering
    TrackPeakLevels { track_index: usize, left: f32, right: f32 },
    MasterPeakLevels { left: f32, right: f32 },

    // Performance stats
    EngineStats { dropped_frames: u64, hw_decode: bool },

    // Skim transcode
    SkimTranscodeProgress { media_id: MediaId, progress: f32 },
    SkimTranscodeComplete { media_id: MediaId, path: PathBuf },
    SkimTranscodeFailed { media_id: MediaId, error: String },

    // Browser skim frame
    BrowserSkimFrameReady { media_id: MediaId, rgba: Vec<u8>, width: u32, height: u32 },
}
