use crate::id::{ClipId, MediaId};
use crate::time::{Fps, Timecode};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    // Timeline
    SetPlayhead(Timecode),
    SetSpeed(f64),
    AppendClip { media_id: MediaId },
    RemoveClip { clip_id: ClipId },
    MoveClip { clip_id: ClipId, new_timeline_in: Timecode },
    TrimClip {
        clip_id: ClipId,
        new_source_in: Timecode,
        new_source_out: Timecode,
        new_timeline_in: Timecode,
        new_timeline_out: Timecode,
    },
    SelectClip(Option<ClipId>),

    // Project
    NewProject,
    SaveProject,
    SaveProjectAs { path: PathBuf },
    OpenProject { path: PathBuf },

    // Edit
    Undo,
    Redo,
    SequenceUpdated,

    // Export
    ShowExportDialog,
    ExportSequence { settings: ExportSettings },
    CancelExport,

    // Application
    Quit,
}

#[derive(Debug, Clone)]
pub enum AppEvent {
    // Playback state
    PlaybackStarted,
    PlaybackPaused,
    PlaybackStopped,
    PlayheadMoved(Timecode),

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

    // Thumbnails
    ThumbnailReady { media_id: MediaId, rgba: Vec<u8>, width: u32, height: u32 },
}
