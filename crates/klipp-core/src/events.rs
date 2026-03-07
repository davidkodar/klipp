use crate::id::{ClipId, MediaId};
use crate::time::{Fps, Timecode};
use crate::types::Resolution;
use std::path::PathBuf;

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

    // Thumbnails
    ThumbnailReady { media_id: MediaId, rgba: Vec<u8>, width: u32, height: u32 },
}
