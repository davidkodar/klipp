use klipp_core::{ClipId, Fps, MediaId, Resolution, Timecode};
use klipp_engine::PlaybackState;
use std::path::PathBuf;

pub struct LoadedMedia {
    pub media_id: MediaId,
    pub path: PathBuf,
    pub filename: String,
    pub duration: Timecode,
    pub fps: Fps,
    pub resolution: Resolution,
}

pub struct AppState {
    pub playback: PlaybackState,
    pub playhead: Timecode,
    pub duration: Timecode,
    pub fps: Fps,

    pub loaded_media: Option<LoadedMedia>,

    pub viewer_inspector_split: f32,
    pub upper_timeline_split: f32,

    pub timeline_zoom: f32,
    pub timeline_scroll_x: f32,

    pub selected_clip: Option<ClipId>,

    pub pending_media_path: Option<PathBuf>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            playback: PlaybackState::Idle,
            playhead: Timecode::zero(),
            duration: Timecode::zero(),
            fps: Fps::film_24(),
            loaded_media: None,
            viewer_inspector_split: 0.6,
            upper_timeline_split: 0.65,
            timeline_zoom: 50.0,
            timeline_scroll_x: 0.0,
            selected_clip: None,
            pending_media_path: None,
        }
    }
}
