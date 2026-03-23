use crate::id::{ClipId, GroupId, MarkerId, MediaId, TrackId};
use crate::time::{Fps, RationalTime, Timecode};
use crate::types::Resolution;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct ExportSettings {
    pub output_path: PathBuf,
    pub format: ExportFormat,
    pub video_codec: ExportVideoCodec,
    pub resolution: Option<Resolution>,
    pub fps: Option<Fps>,
    pub crf: u32,
    pub audio_codec: ExportAudioCodec,
    pub audio_bitrate_kbps: u32,
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
    ProRes,
}

impl ExportVideoCodec {
    pub fn ffmpeg_codec(&self) -> &'static str {
        match self {
            Self::H264 => "libx264",
            Self::H265 => "libx265",
            Self::ProRes => "prores_ks",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265",
            Self::ProRes => "ProRes",
        }
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
    pub fn ffmpeg_args(&self, bitrate_kbps: u32) -> Vec<String> {
        match self {
            Self::Aac => vec!["-c:a".into(), "aac".into(), "-b:a".into(), format!("{bitrate_kbps}k")],
            Self::Mp3 => vec!["-c:a".into(), "libmp3lame".into(), "-b:a".into(), format!("{bitrate_kbps}k")],
            Self::Flac => vec!["-c:a".into(), "flac".into()],
            Self::Copy => vec!["-c:a".into(), "copy".into()],
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

    // Color
    ResetClipColorGrade { clip_id: ClipId },
    SetClipColorGradeParam { clip_id: ClipId, param: ColorGradeParam, value: f32 },
    SetClipCurves { clip_id: ClipId, curves: crate::color_grade::CurveSet },
    ToggleColorPanel,
    ToggleBeforeAfter,

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
    UpdateTitleData { clip_id: ClipId, text: String, font_size: f32, color: [u8; 4] },
    SetTitleAlignment { clip_id: ClipId, alignment: u8 },  // 0=Left, 1=Center, 2=Right
    SetTitlePosition { clip_id: ClipId, x: f32, y: f32 },
    SetTitleBackground { clip_id: ClipId, bg_kind: u8, color: [u8; 4] },  // 0=None, 1=Color, 2=FullFrame
    SetTitleShadow { clip_id: ClipId, enabled: bool, color: [u8; 4], offset_x: f32, offset_y: f32 },

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

    // Dynamic tracks
    AddVideoTrack,
    AddAudioTrack,
    RemoveTrack { track_id: TrackId },
    RenameTrack { track_id: TrackId, name: String },

    // Media pool management
    RemoveFromPool { media_id: MediaId },
    ConfirmRemoveFromPool { media_id: MediaId },
    SetMediaRating { media_id: MediaId, rating: u8 },

    SetMediaTags { media_id: MediaId, tags: Vec<String> },
    RevealInFileManager { path: PathBuf },

    // Bins
    CreateBin { name: String },
    DeleteBin { name: String },
    MoveItemToBin { media_id: MediaId, bin: Option<String> },
    RenameBin { old_name: String, new_name: String },

    // Browser skimming
    StartSkimTranscode { media_id: MediaId },
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

    // Application
    Quit,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorGradeParam {
    Exposure,
    Contrast,
    Saturation,
    Temperature,
    Tint,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    LiftX,
    LiftY,
    LiftMaster,
    GammaX,
    GammaY,
    GammaMaster,
    GainX,
    GainY,
    GainMaster,
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
    },
    MediaLoadFailed {
        media_id: MediaId,
        error: String,
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
