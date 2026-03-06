use crate::id::MediaId;
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

    // Timeline
    SetPlayhead(Timecode),

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
        duration: Timecode,
        fps: Fps,
        resolution: Resolution,
    },
    MediaLoadFailed {
        media_id: MediaId,
        error: String,
    },

    // Application
    FrameReady,
}
