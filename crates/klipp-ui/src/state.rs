use crate::history::CommandHistory;
use klipp_core::{ClipId, Fps, MediaId, Timecode, Tool, ViewerMode};
use klipp_engine::PlaybackState;
use klipp_media::{MediaItem, MediaPool, ThumbnailCache};
use klipp_timeline::{Clip, Sequence};
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

    // Phase 4: Tools & modes
    pub active_tool: Tool,
    pub viewer_mode: ViewerMode,
    pub snap_enabled: bool,
    pub ripple_enabled: bool,
    pub fullscreen_viewer: bool,
    pub fullscreen_changed: bool,

    // Phase 4: Clipboard
    pub clipboard: Option<ClipboardContent>,

    // Phase 4: Source monitor
    pub source_media_id: Option<MediaId>,
    pub source_playhead: Timecode,

    // Phase 4: Proxy
    pub use_proxies: bool,

    // Phase 4: Relink
    pub show_relink_dialog: bool,
    pub offline_media_ids: Vec<MediaId>,

    // Phase 4: About
    pub show_about_dialog: bool,

    // Phase 4: Timecode input
    pub editing_timecode: bool,
    pub timecode_input: String,

    // Phase 4: Fullscreen overlay
    pub fullscreen_overlay_visible: bool,
    pub fullscreen_overlay_timer: f64,

    // Phase 4: Media pool drag-and-drop
    pub dragging_media: Option<MediaId>,
}

#[derive(Debug, Clone)]
pub struct ClipboardContent {
    pub clips: Vec<(Clip, MediaItem)>,
    pub source_track_index: usize,
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
            active_tool: Tool::Select,
            viewer_mode: ViewerMode::Program,
            snap_enabled: true,
            ripple_enabled: true,
            fullscreen_viewer: false,
            fullscreen_changed: false,
            clipboard: None,
            source_media_id: None,
            source_playhead: Timecode::zero(),
            use_proxies: true,
            show_relink_dialog: false,
            offline_media_ids: Vec::new(),
            show_about_dialog: false,
            editing_timecode: false,
            timecode_input: String::new(),
            fullscreen_overlay_visible: false,
            fullscreen_overlay_timer: 0.0,
            dragging_media: None,
        }
    }
}

impl AppState {
    pub fn sequence_duration(&self) -> Timecode {
        self.sequence.total_duration()
    }
}
