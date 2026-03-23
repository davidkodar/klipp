use crate::history::CommandHistory;
use klipp_core::{ClipId, Fps, MediaId, Timecode, Tool, TrackId, ViewerMode};
use klipp_engine::PlaybackState;
use klipp_media::{MediaItem, MediaPool, ThumbnailCache};
use klipp_timeline::{Clip, Sequence};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct BrowserClipSelection {
    pub media_id: MediaId,
    pub range_in_secs: f64,
    pub range_out_secs: f64,
}

/// State during an active range handle drag or ruler range drag.
#[derive(Debug, Clone)]
pub struct RangeDragState {
    pub kind: RangeDragKind,
    pub original_in: Option<Timecode>,
    pub original_out: Option<Timecode>,
}

/// What kind of range drag is in progress.
#[derive(Debug, Clone, Copy)]
pub enum RangeDragKind {
    /// Dragging the in-point handle.
    InHandle,
    /// Dragging the out-point handle.
    OutHandle,
    /// Dragging on empty ruler to create a new range.
    RulerCreate { anchor_secs: f64 },
}

pub struct AppState {
    pub playback: PlaybackState,
    pub playhead: Timecode,
    pub fps: Fps,
    pub playback_speed: f64,
    pub loop_playback_enabled: bool,

    // Project
    pub sequences: Vec<Sequence>,
    pub active_sequence_idx: usize,
    pub media_pool: MediaPool,
    pub project_path: Option<PathBuf>,
    pub created_at: Option<String>,
    pub dirty: bool,
    pub history: CommandHistory,

    // Layout
    pub viewer_inspector_split: f32,
    pub upper_timeline_split: f32,
    pub timeline_zoom: f32,
    pub timeline_scroll_x: f32,
    pub playhead_follow: PlayheadFollow,
    pub track_header_width: f32,
    pub last_timeline_content_width: f32,
    pub show_overview_bar: bool,

    // Selection
    pub selected_clip: Option<ClipId>,
    pub selected_clips: Vec<ClipId>,
    pub hovered_clip: Option<ClipId>,

    // UI state
    pub show_unsaved_dialog: bool,
    pub unsaved_dialog_action: UnsavedAction,
    pub pending_media_path: Option<PathBuf>,

    // Thumbnails
    pub thumbnail_cache: ThumbnailCache,
    pub thumbnail_textures: std::collections::HashMap<MediaId, egui::TextureHandle>,
    /// Filmstrip textures: media_id → Vec of (time_secs, texture_handle)
    pub filmstrip_textures: std::collections::HashMap<MediaId, Vec<(f64, egui::TextureHandle)>>,

    // Export
    pub show_export_dialog: bool,

    // Phase 4: Tools & modes
    pub active_tool: Tool,
    pub viewer_mode: ViewerMode,
    pub snap_enabled: bool,
    pub ripple_enabled: bool,
    /// Magnetic timeline edit mode: Insert (ripple) or Overwrite (replace)
    pub edit_mode: klipp_timeline::magnetic::EditMode,
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

    // GUI redesign
    pub skimming_enabled: bool,
    pub toast: crate::widgets::toast::ToastState,
    pub drag_drop: crate::drag_drop::DragDropState,

    // Browser skimming
    pub browser_skim_texture: Option<egui::TextureHandle>,
    pub browser_skim_media_id: Option<MediaId>,
    pub skim_transcoding: std::collections::HashSet<MediaId>,
    /// Active skim transcode progress: (filename, progress 0.0..1.0)
    pub skim_transcode_status: Option<(String, f32)>,
    /// When the last skim transcode finished (for "Done" display)
    pub skim_transcode_done_at: Option<std::time::Instant>,
    /// Pending skim frame RGBA to upload as texture (media_id, rgba, width, height)
    pub browser_skim_pending: Option<(MediaId, Vec<u8>, u32, u32)>,
    /// Set to true each frame if hover skim is active (used to clear texture when mouse leaves)
    pub browser_skim_active_this_frame: bool,
    /// Media IDs that need skim transcode started (queued during drain_events)
    pub pending_skim_transcodes: Vec<MediaId>,

    // Audio skimming (Shift+S toggle, independent of video skimming)
    /// Whether audio plays during skimming. Independent of skimming_enabled.
    pub audio_skimming_enabled: bool,
    /// Current skim speed in pixels per second (for mode selection).
    pub skim_speed_px_per_sec: f32,
    /// Time of last skim position update (for speed calculation).
    pub skim_last_update: std::time::Instant,
    /// Last skim x position in screen pixels (for speed calculation).
    pub skim_last_x: f32,
    /// The active skim audio session. Some() while hovering a skimmable clip.
    pub skim_audio_session: Option<klipp_engine::skim_audio::SkimAudioSession>,
    /// Time of last audio skim decode dispatch (throttle to prevent starvation).
    pub skim_last_audio_dispatch: std::time::Instant,

    // Browser range selection (filmstrip drag-to-select in grid view)
    pub browser_range_selection: Option<BrowserClipSelection>,
    pub browser_drag_start: Option<(egui::Pos2, std::time::Instant)>,

    // Timeline range handle drag state
    pub range_drag: Option<RangeDragState>,
    /// Snap line x position during range handle drag (drawn in mod.rs).
    pub range_snap_line_x: Option<f32>,
    /// Press origin for 4px drag threshold on ruler.
    pub ruler_press_origin: Option<egui::Pos2>,

    // Phase 5: Performance overlay
    pub show_perf_overlay: bool,
    pub perf_fps: f64,
    pub perf_frame_count: u64,
    pub perf_dropped_frames: u64,
    pub perf_memory_mb: f64,
    pub perf_cpu_percent: f64,
    pub perf_hw_decode: bool,
    pub perf_last_update: std::time::Instant,
    pub perf_frame_times: Vec<f64>,
    pub perf_prev_cpu_ticks: u64,
    pub perf_prev_wall_time: std::time::Instant,

    // Filmstrip generation progress
    pub filmstrip_generating: bool,
    pub filmstrip_generate_start: std::time::Instant,
    pub filmstrip_toast_dismissed: bool,

    // Global LUT
    pub global_lut_path: Option<PathBuf>,
    pub global_lut_opacity: f32,

    // Color panel
    pub show_color_panel: bool,
    pub color_grade_drag_start: Option<klipp_timeline::ClipColorGrade>,
    pub color_grade_clipboard: Option<klipp_timeline::ClipColorGrade>,

    // Scopes
    pub show_scopes: bool,
    pub scope_mode: ScopeMode,

    // Before/After
    pub before_after_active: bool,

    // OFX plugins
    pub available_plugins: Vec<klipp_plugin::OfxPluginInfo>,

    // Unified effects registry (built-in + OFX merged)
    pub effect_registry: crate::browser::effect_registry::EffectRegistry,
    pub effect_thumbnails: crate::browser::effect_thumbnails::ThumbnailRenderer,

    // Render queue
    pub render_queue: klipp_export::RenderQueue,
    pub show_render_queue: bool,

    // Smooth playhead interpolation
    /// Last engine-reported playhead and the wall time it was received
    pub playhead_anchor: Timecode,
    pub playhead_anchor_wall: std::time::Instant,
    pub playhead_speed: f64,
    /// High-precision playhead for smooth display (not quantized to frame rate)
    pub playhead_smooth_secs: f64,

    // Phase 6B: VU metering
    /// Per-track peak levels (display values with decay)
    pub track_peak_levels: Vec<(f32, f32)>,
    /// Master peak levels (display values with decay)
    pub master_peak_left: f32,
    pub master_peak_right: f32,
    /// Master volume control
    pub master_volume: f32,
    /// Master mute
    pub master_muted: bool,

    // Phase 7: Dynamic tracks
    pub confirm_remove_track: Option<TrackId>,
    pub show_markers_panel: bool,

    // Phase 9: Preferences
    pub show_preferences: bool,

    // Media panel
    pub show_media_panel: bool,

    // Remove media confirmation
    pub show_remove_media_confirm: Option<MediaId>,

    // New Project dialog
    pub show_new_project_dialog: bool,
    // Project Info window
    pub show_project_info: bool,
    // Consolidate Project dialog
    pub show_consolidate_dialog: bool,

    // Sequence tab rename
    pub renaming_sequence_idx: Option<usize>,
    pub rename_sequence_buf: String,

    // Auto-save recovery dialog
    pub show_autosave_recovery: bool,
    pub autosave_recovery_path: Option<PathBuf>,
    pub autosave_original_path: Option<PathBuf>,

    // Proxy generation tracking
    /// Media IDs currently generating proxies — shown as status in UI
    pub proxy_generating: std::collections::HashSet<MediaId>,
    /// Per-media proxy generation progress (0.0..1.0)
    pub proxy_progress: std::collections::HashMap<MediaId, f32>,
    /// Per-media proxy generation start time (for ETA calculation)
    pub proxy_start_times: std::collections::HashMap<MediaId, std::time::Instant>,
    /// Last proxy generation message (toast-like feedback)
    pub proxy_status_message: Option<(String, std::time::Instant)>,
    /// Whether the proxy toast was dismissed by the user
    pub proxy_toast_dismissed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeMode {
    #[default]
    Waveform,
    Vectorscope,
    Histogram,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayheadFollow {
    /// No auto-scroll
    Off,
    /// Jump one page when playhead reaches edge
    #[default]
    Page,
    /// Continuously scroll to keep playhead visible
    Smooth,
}

impl PlayheadFollow {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Page => "Page",
            Self::Smooth => "Smooth",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Off => Self::Page,
            Self::Page => Self::Smooth,
            Self::Smooth => Self::Off,
        }
    }
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
    OpenNewProjectDialog,
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
            loop_playback_enabled: false,
            sequences: vec![Sequence::new_default()],
            active_sequence_idx: 0,
            media_pool: MediaPool::new(),
            project_path: None,
            created_at: None,
            dirty: false,
            history: CommandHistory::new(),
            viewer_inspector_split: 0.6,
            upper_timeline_split: 0.65,
            timeline_zoom: 50.0,
            timeline_scroll_x: 0.0,
            playhead_follow: PlayheadFollow::default(),
            track_header_width: 160.0,
            last_timeline_content_width: 800.0,
            show_overview_bar: true,
            selected_clip: None,
            selected_clips: Vec::new(),
            hovered_clip: None,
            show_unsaved_dialog: false,
            unsaved_dialog_action: UnsavedAction::None,
            pending_media_path: None,
            thumbnail_cache: ThumbnailCache::new(),
            thumbnail_textures: std::collections::HashMap::new(),
            filmstrip_textures: std::collections::HashMap::new(),
            show_export_dialog: false,
            active_tool: Tool::Select,
            viewer_mode: ViewerMode::Program,
            snap_enabled: true,
            ripple_enabled: true,
            edit_mode: klipp_timeline::magnetic::EditMode::default(),
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
            skimming_enabled: true,
            toast: crate::widgets::toast::ToastState::default(),
            drag_drop: crate::drag_drop::DragDropState::default(),
            browser_skim_texture: None,
            browser_skim_media_id: None,
            skim_transcoding: std::collections::HashSet::new(),
            skim_transcode_status: None,
            skim_transcode_done_at: None,
            browser_skim_pending: None,
            browser_skim_active_this_frame: false,
            pending_skim_transcodes: Vec::new(),
            audio_skimming_enabled: true,
            skim_speed_px_per_sec: 0.0,
            skim_last_update: std::time::Instant::now(),
            skim_last_x: 0.0,
            skim_audio_session: None,
            skim_last_audio_dispatch: std::time::Instant::now(),
            browser_range_selection: None,
            browser_drag_start: None,
            range_drag: None,
            range_snap_line_x: None,
            ruler_press_origin: None,
            show_perf_overlay: false,
            perf_fps: 0.0,
            perf_frame_count: 0,
            perf_dropped_frames: 0,
            perf_memory_mb: 0.0,
            perf_cpu_percent: 0.0,
            perf_hw_decode: false,
            perf_last_update: std::time::Instant::now(),
            perf_frame_times: Vec::with_capacity(120),
            perf_prev_cpu_ticks: 0,
            perf_prev_wall_time: std::time::Instant::now(),
            global_lut_path: None,
            global_lut_opacity: 1.0,
            show_color_panel: false,
            color_grade_drag_start: None,
            color_grade_clipboard: None,
            show_scopes: false,
            scope_mode: ScopeMode::Waveform,
            before_after_active: false,
            available_plugins: Vec::new(),
            effect_registry: crate::browser::effect_registry::EffectRegistry::new(),
            effect_thumbnails: crate::browser::effect_thumbnails::ThumbnailRenderer::default(),
            render_queue: klipp_export::RenderQueue::new(),
            show_render_queue: false,
            filmstrip_generating: false,
            filmstrip_generate_start: std::time::Instant::now(),
            filmstrip_toast_dismissed: false,
            playhead_anchor: Timecode::zero(),
            playhead_anchor_wall: std::time::Instant::now(),
            playhead_speed: 1.0,
            playhead_smooth_secs: 0.0,
            track_peak_levels: Vec::new(),
            master_peak_left: 0.0,
            master_peak_right: 0.0,
            master_volume: 1.0,
            master_muted: false,
            confirm_remove_track: None,
            show_markers_panel: false,
            show_preferences: false,
            show_media_panel: false,
            show_remove_media_confirm: None,
            show_new_project_dialog: false,
            show_project_info: false,
            show_consolidate_dialog: false,
            renaming_sequence_idx: None,
            rename_sequence_buf: String::new(),
            show_autosave_recovery: false,
            autosave_recovery_path: None,
            autosave_original_path: None,
            proxy_generating: std::collections::HashSet::new(),
            proxy_progress: std::collections::HashMap::new(),
            proxy_start_times: std::collections::HashMap::new(),
            proxy_status_message: None,
            proxy_toast_dismissed: false,
        }
    }
}

impl AppState {
    pub fn sequence(&self) -> &Sequence {
        &self.sequences[self.active_sequence_idx]
    }
    pub fn sequence_mut(&mut self) -> &mut Sequence {
        &mut self.sequences[self.active_sequence_idx]
    }
    pub fn sequence_duration(&self) -> Timecode {
        self.sequence().total_duration()
    }
}
