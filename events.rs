use serde::{Deserialize, Serialize};
use crate::id::*;
use crate::time::Timecode;

/// Events flow between subsystems via channels.
/// The UI sends commands, subsystems emit events back.
/// This keeps crates decoupled — nobody imports each other directly.

/// Commands sent FROM the UI / user TO the engine/subsystems
#[derive(Debug, Clone)]
pub enum AppCommand {
    // Playback
    Play,
    Pause,
    Stop,
    SeekTo(Timecode),
    SetPlaybackRate(f64),

    // Timeline editing
    AddClip { track_id: TrackId, media_id: MediaId, position: Timecode },
    RemoveClip(ClipId),
    MoveClip { clip_id: ClipId, track_id: TrackId, position: Timecode },
    TrimClipStart { clip_id: ClipId, new_start: Timecode },
    TrimClipEnd { clip_id: ClipId, new_end: Timecode },
    SplitClip { clip_id: ClipId, at: Timecode },

    // Project
    NewProject,
    OpenProject(std::path::PathBuf),
    SaveProject,
    SaveProjectAs(std::path::PathBuf),

    // Media
    ImportMedia(Vec<std::path::PathBuf>),
    GenerateProxy(MediaId),

    // Export
    StartExport(ExportRequest),
    CancelExport,
}

/// Events emitted FROM subsystems TO the UI
#[derive(Debug, Clone)]
pub enum AppEvent {
    // Playback state
    PlaybackStarted,
    PlaybackStopped,
    PlayheadMoved(Timecode),
    PlaybackRateChanged(f64),

    // Timeline changes
    ClipAdded(ClipId),
    ClipRemoved(ClipId),
    ClipMoved(ClipId),
    TimelineChanged,

    // Media
    MediaImported(MediaId),
    MediaImportFailed { path: std::path::PathBuf, reason: String },
    ProxyReady(MediaId),
    ThumbnailReady { media_id: MediaId, frame: i64 },

    // Project
    ProjectLoaded,
    ProjectSaved,
    ProjectModified,

    // Export
    ExportProgress { percent: f32 },
    ExportComplete(std::path::PathBuf),
    ExportFailed(String),

    // Engine
    FrameReady,    // A new composited frame is ready in the GPU texture
    EngineError(String),
}

/// Export configuration passed with StartExport command
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRequest {
    pub output_path: std::path::PathBuf,
    pub format: ExportFormat,
    pub sequence_id: SequenceId,
    pub range: Option<crate::time::TimeRange>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExportFormat {
    H264 { crf: u8, preset: String },
    H265 { crf: u8, preset: String },
    ProRes { profile: ProResProfile },
    Dnxhd { profile: String },
    Av1 { crf: u8 },
    Custom { codec: String, options: Vec<(String, String)> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProResProfile {
    Proxy,
    Lt,
    Standard,
    Hq,
    FourFourFourFour,
    FourFourFourFourXq,
}
