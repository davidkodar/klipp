use crate::history::CommandHistory;
use klipp_core::{ClipId, Fps, MediaId, Timecode};
use klipp_engine::PlaybackState;
use klipp_media::{MediaPool, ThumbnailCache};
use klipp_timeline::Sequence;
use std::path::PathBuf;

pub struct AppState {
    pub playback: PlaybackState,
    pub playhead: Timecode,
    pub fps: Fps,
    pub playback_speed: f64,

    // Project
    pub sequence: Sequence,
    pub media_pool: MediaPool,
    pub project_path: Option<PathBuf>,
    pub dirty: bool,
    pub history: CommandHistory,

    // Layout
    pub viewer_inspector_split: f32,
    pub upper_timeline_split: f32,
    pub timeline_zoom: f32,
    pub timeline_scroll_x: f32,

    // Selection
    pub selected_clip: Option<ClipId>,

    // UI state
    pub show_unsaved_dialog: bool,
    pub unsaved_dialog_action: UnsavedAction,
    pub pending_media_path: Option<PathBuf>,

    // Thumbnails
    pub thumbnail_cache: ThumbnailCache,
    pub thumbnail_textures: std::collections::HashMap<MediaId, egui::TextureHandle>,

    // Export
    pub show_export_dialog: bool,
    pub export_in_progress: bool,
    pub export_progress: f32,
    pub export_error: Option<String>,
}

#[derive(Debug, Clone)]
pub enum UnsavedAction {
    None,
    NewProject,
    OpenProject(PathBuf),
    Quit,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            playback: PlaybackState::Idle,
            playhead: Timecode::zero(),
            fps: Fps::film_24(),
            playback_speed: 1.0,
            sequence: Sequence::new_default(),
            media_pool: MediaPool::new(),
            project_path: None,
            dirty: false,
            history: CommandHistory::new(),
            viewer_inspector_split: 0.6,
            upper_timeline_split: 0.65,
            timeline_zoom: 50.0,
            timeline_scroll_x: 0.0,
            selected_clip: None,
            show_unsaved_dialog: false,
            unsaved_dialog_action: UnsavedAction::None,
            pending_media_path: None,
            thumbnail_cache: ThumbnailCache::new(),
            thumbnail_textures: std::collections::HashMap::new(),
            show_export_dialog: false,
            export_in_progress: false,
            export_progress: 0.0,
            export_error: None,
        }
    }
}

impl AppState {
    pub fn sequence_duration(&self) -> Timecode {
        self.sequence.total_duration()
    }
}
