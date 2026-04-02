use crate::history::CommandHistory;
use klipp_core::{AudioParam, ClipId, Fps, MediaId, Timecode, Tool, TrackId, VideoParam, ViewerMode};
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

/// Which transform handle is being dragged in the viewer
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransformHandle {
    Position,
    ScaleTopLeft,
    ScaleTopRight,
    ScaleBottomLeft,
    ScaleBottomRight,
    Rotation,
}

/// State for viewer transform control interaction
#[derive(Debug, Clone)]
pub struct ViewerTransformDrag {
    pub clip_id: klipp_core::ClipId,
    pub effect_index: usize,
    pub handle: TransformHandle,
    pub initial_transform: crate::inspector::video_tab::TransformParams,
    pub drag_start_pos: egui::Pos2,
}

/// Which part of a mask shape is being dragged in the viewer
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MaskHandle {
    Center,
    EdgeRight,
    EdgeBottom,
    /// Bezier control point by index.
    BezierPoint(usize),
}

/// State for viewer mask handle interaction
#[derive(Debug, Clone)]
pub struct ViewerMaskDrag {
    pub clip_id: klipp_core::ClipId,
    pub node_id: klipp_nodes::NodeId,
    pub mask_index: usize,
    pub handle: MaskHandle,
    pub initial_shape: klipp_core::MaskShape,
    pub drag_start_pos: egui::Pos2,
}

/// State during an active audio keyframe drag on the timeline rubber band.
#[derive(Debug, Clone)]
pub struct DraggingKeyframe {
    pub clip_id: ClipId,
    pub param: AudioParam,
    pub keyframe_index: usize,
}

/// State during an active Bezier tangent handle drag on the timeline rubber band.
#[derive(Debug, Clone)]
pub struct BezierHandleDrag {
    pub clip_id: ClipId,
    pub param: AudioParam,
    pub keyframe_index: usize,
    /// true = tangent_out (right handle), false = tangent_in (left handle)
    pub is_out: bool,
}

/// State during an active Bezier tangent handle drag on the video timeline rubber band.
#[derive(Debug, Clone)]
pub struct BezierVideoHandleDrag {
    pub clip_id: ClipId,
    pub param: VideoParam,
    pub keyframe_index: usize,
    /// true = tangent_out (right handle), false = tangent_in (left handle)
    pub is_out: bool,
}

/// State during an active video keyframe drag on the timeline rubber band.
#[derive(Debug, Clone)]
pub struct DraggingVideoKeyframe {
    pub clip_id: ClipId,
    pub param: VideoParam,
    pub keyframe_index: usize,
}

/// Clipboard for keyframe curves (copy/paste between clips/params).
#[derive(Debug, Clone)]
pub struct KeyframeClipboard {
    /// The copied keyframes.
    pub keyframes: Vec<klipp_core::Keyframe>,
    /// Duration of the source clip (for time scaling when pasting to different-length clips).
    pub source_duration_secs: f64,
}

/// Which kind of parameter a keyframe context menu or selection refers to.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyframeParamKind {
    Audio(AudioParam),
    Video(VideoParam),
}

/// State for the keyframe interpolation context menu (right-click on diamond).
#[derive(Debug, Clone)]
pub struct KeyframeContextMenu {
    pub clip_id: ClipId,
    pub param_kind: KeyframeParamKind,
    pub keyframe_index: usize,
    /// Screen position where the menu was opened.
    pub screen_pos: egui::Pos2,
}

/// A selected keyframe on the timeline rubber band.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedKeyframe {
    pub clip_id: ClipId,
    pub param_kind: KeyframeParamKind,
    pub keyframe_index: usize,
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

/// Workspace mode — determines panel arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Workspace {
    #[default]
    Edit,
    Color,
    Export,
}

/// A saved workspace layout (panel visibility + name).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SavedWorkspaceLayout {
    pub name: String,
    pub show_browser: bool,
    pub show_inspector: bool,
    pub show_history: bool,
    pub show_markers: bool,
    pub show_curve_editor: bool,
    pub show_scopes: bool,
    pub show_color_panel: bool,
}

/// Active tab in the simple color inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorTab {
    #[default]
    Adjustments,
    Wheels,
    Curves,
    HueSat,
    Warper,
}

pub struct AppState {
    pub playback: PlaybackState,
    pub playhead: Timecode,
    pub fps: Fps,
    pub playback_speed: f64,
    pub loop_playback_enabled: bool,

    // Active workspace
    pub active_workspace: Workspace,
    /// User-saved workspace layouts.
    pub custom_workspaces: Vec<SavedWorkspaceLayout>,
    /// Show the save workspace name dialog.
    pub show_save_workspace_dialog: bool,
    /// Name for the workspace being saved.
    pub save_workspace_name: String,

    // Project
    pub sequences: Vec<Sequence>,
    pub active_sequence_idx: usize,
    /// Breadcrumb stack for navigating into compound clips. Each entry is
    /// (parent_sequence_idx, parent_playhead) so we can navigate back.
    pub compound_clip_stack: Vec<(usize, klipp_core::Timecode)>,
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
    pub timeline_scroll_y: f32,
    pub playhead_follow: PlayheadFollow,
    pub track_header_width: f32,
    /// Viewer zoom level (1.0 = fit, 0.25/0.5/2.0/4.0 for pixel-level zoom).
    pub viewer_zoom: f32,
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
    /// Default transition duration in frames (synced from prefs).
    pub default_transition_frames: u32,
    /// Timeline visual prefs (synced from prefs as whole struct).
    pub timeline_prefs: crate::prefs::TimelinePrefs,
    /// Viewer prefs (synced from prefs as whole struct).
    pub viewer_prefs: crate::prefs::ViewerPrefs,
    /// Snap threshold in pixels (synced from prefs).
    pub snap_threshold_px: f32,
    /// Zoom step multiplier (synced from prefs).
    pub zoom_step: f32,
    /// Toast duration in seconds (synced from prefs).
    pub toast_duration_secs: f64,
    /// Modal backdrop opacity (synced from prefs).
    pub modal_backdrop_alpha: u8,
    /// Scope quality (synced from prefs).
    pub scope_quality: klipp_core::ScopeQuality,
    /// Scope update interval ms (synced from prefs).
    pub scope_update_interval_ms: u32,
    /// Magnetic timeline edit mode: Insert (ripple) or Overwrite (replace)
    pub edit_mode: klipp_timeline::magnetic::EditMode,
    pub fullscreen_viewer: bool,
    pub fullscreen_changed: bool,

    // Phase 4: Clipboard
    pub clipboard: Option<ClipboardContent>,
    /// Attributes clipboard for Paste Attributes (Ctrl+Shift+V).
    pub attributes_clipboard: Option<AttributesClipboard>,
    /// Whether to show the Paste Attributes dialog.
    pub show_paste_attributes_dialog: bool,
    /// Whether to show the Set Clip Speed dialog.
    pub show_speed_dialog: bool,
    /// Current value in the speed dialog (percentage, e.g. 100.0 = normal).
    pub speed_dialog_value: f64,
    /// Whether speed dialog reverse is checked.
    pub speed_dialog_reverse: bool,

    // Phase 4: Source monitor
    pub source_media_id: Option<MediaId>,
    pub source_playhead: Timecode,

    // Media browser multi-select
    pub selected_media_ids: std::collections::HashSet<MediaId>,
    pub last_clicked_media_id: Option<MediaId>,

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
    /// True while a skim decode thread is running (prevents spawning more threads)
    pub skim_decode_in_flight: bool,
    /// Latest skim request queued while a decode was in flight (media_id, time_secs)
    pub skim_queued_request: Option<(MediaId, f64)>,
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

    // Video stabilization analysis state
    pub stabilization_in_progress: std::collections::HashSet<klipp_core::ClipId>,
    pub stabilization_progress: std::collections::HashMap<klipp_core::ClipId, f32>,

    // Point tracking state
    /// Whether the viewer is in tracking placement mode (click to place tracker).
    pub tracking_mode: bool,
    /// Index of the track point currently being edited/dragged in the viewer.
    pub active_track_point: Option<usize>,
    /// Track analysis in progress for these clip IDs.
    pub tracking_in_progress: std::collections::HashSet<klipp_core::ClipId>,
    /// Track analysis progress (0.0-1.0) per clip.
    pub tracking_progress: std::collections::HashMap<klipp_core::ClipId, f32>,

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
    /// Media IDs currently generating filmstrips — prevents duplicate spawns.
    pub filmstrip_in_progress: std::collections::HashSet<MediaId>,
    /// Queue of media IDs waiting for filmstrip generation (serialized, one at a time).
    pub filmstrip_queue: Vec<(MediaId, PathBuf, klipp_core::RationalTime)>,
    /// Total number of filmstrip jobs queued in this batch (for progress display).
    pub filmstrip_total_queued: usize,
    /// Number of filmstrip jobs completed in this batch.
    pub filmstrip_completed: usize,

    // Global LUT
    pub global_lut_path: Option<PathBuf>,
    pub global_lut_opacity: f32,
    pub global_lut_enabled: bool,

    // Color panel / node graph
    pub show_color_panel: bool,
    /// Selected node in the node graph (shared between canvas and inspector).
    pub selected_node_id: Option<klipp_nodes::NodeId>,
    /// Whether the compact node strip is visible in Color workspace.
    pub show_node_strip: bool,
    /// Active tab in the simple (non-node) color inspector.
    pub color_tab: ColorTab,
    pub color_grade_drag_start: Option<klipp_nodes::NodeGraph>,
    pub color_grade_clipboard: Option<klipp_nodes::NodeGraph>,
    pub curves_single_mode: bool,
    pub curve_histograms: Option<klipp_color::scopes::HistogramData>,
    pub slider_edit: crate::widgets::color_slider::SliderEditState,
    pub show_preset_menu: bool,
    pub preset_save_name: String,
    pub show_preset_save: bool,
    pub grade_presets: Vec<(String, klipp_nodes::NodeGraph)>,
    pub huesat_single_mode: bool,
    pub huesat_active_curve: u8, // 0-5 index for Single mode

    // Scopes
    pub show_scopes: bool,
    /// Render scopes as a semi-transparent overlay on the viewer instead of a separate panel.
    pub scope_overlay_mode: bool,
    pub show_keyframe_editor: bool,
    pub scope_mode: ScopeMode,
    pub scope_state: crate::widgets::scopes::ScopeState,

    // Eyedropper mode (shared between WB and qualifier)
    pub eyedropper_mode: EyedropperMode,
    /// Pixel coordinates to sample for white balance (set by viewer on click, consumed by app)
    pub wb_sample_request: Option<(f32, f32)>, // normalized 0-1
    /// Pixel coordinates to sample for qualifier (set by viewer on click, consumed by app)
    pub qualifier_sample_request: Option<(f32, f32)>, // normalized 0-1
    /// Matte preview — show B&W qualifier/mask matte in viewer
    pub matte_preview_active: bool,

    // Before/After split view
    pub before_after_active: bool,
    pub split_position: f32, // 0.0-1.0, default 0.5

    // Grade stills (reference frame comparison)
    /// Saved grade reference stills for comparison.
    pub grade_stills: Vec<GradeStill>,
    /// Index of the active grade still being compared (None = no comparison).
    pub active_grade_still: Option<usize>,
    /// Whether split-view comparison with a grade still is active.
    pub grade_still_compare: bool,

    // Viewer overlay (zebra / false color)
    pub viewer_overlay: ViewerOverlay,
    pub frame_count: u64, // for zebra animation

    // OFX plugins
    pub available_plugins: Vec<klipp_plugin::OfxPluginInfo>,

    // Unified effects registry (built-in + OFX merged)
    pub effect_registry: crate::browser::effect_registry::EffectRegistry,
    /// Effect browser filters
    pub effect_filter_builtin: bool,
    pub effect_filter_ofx: bool,
    pub effect_filter_gpu: bool,
    pub effect_filter_hide_unsupported: bool,
    /// Effect presets (built-in + user-created)
    pub preset_manager: crate::effect_presets::PresetManager,
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

    // Inspector section collapse state
    /// Names of inspector sections that are currently collapsed.
    pub collapsed_sections: std::collections::HashSet<String>,
    /// Whether to persist collapsed sections across restarts.
    pub remember_inspector_layout: bool,

    // Audio backend status
    /// True if the audio output device was successfully opened (cpal/PipeWire).
    pub audio_backend_ok: bool,
    /// Audio backend sample rate (probed at startup).
    pub audio_sample_rate: u32,

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
    pub show_browser_panel: bool,
    /// When set, the browser panel should scroll to this media item.
    pub reveal_in_browser: Option<klipp_core::MediaId>,
    pub show_inspector_panel: bool,

    /// True when any egui popup/menu/combobox is open. Set at start of each frame.
    pub modal_or_popup_open: bool,

    // Phase 9: Preferences
    pub show_preferences: bool,
    /// Auto-create tracks when dropping clips outside existing tracks (synced from prefs).
    pub auto_create_tracks: bool,

    // Per-project media generation overrides (None = use global pref)
    pub project_auto_filmstrips: Option<bool>,
    pub project_auto_skim: Option<bool>,

    // Media panel
    pub show_media_panel: bool,

    // Remove media confirmation
    pub show_remove_media_confirm: Option<MediaId>,
    /// Delete unused media confirmation: (count, total_bytes)
    pub show_delete_unused_confirm: Option<(usize, u64)>,
    /// Export offline warning: list of (filename, timecode_string) for offline clips
    pub export_offline_warning: Option<(Vec<(String, String)>, Box<klipp_core::ExportSettings>)>,

    // Usage count cache: MediaId → number of clips referencing it.
    // Rebuilt lazily when media_usage_cache_dirty is set (after any timeline edit).
    pub media_usage_cache: std::collections::HashMap<MediaId, usize>,
    pub media_usage_cache_dirty: bool,

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

    // Audio keyframe rubber band drag state
    pub dragging_audio_keyframe: Option<DraggingKeyframe>,
    // Video keyframe rubber band drag state
    pub dragging_video_keyframe: Option<DraggingVideoKeyframe>,
    /// Clip whose flat rubber band line is being dragged (no-keyframes volume adjust)
    pub dragging_rubber_band: Option<ClipId>,
    /// Bezier tangent handle drag state (audio)
    pub dragging_bezier_handle: Option<BezierHandleDrag>,
    /// Bezier tangent handle drag state (video)
    pub dragging_video_bezier_handle: Option<BezierVideoHandleDrag>,
    /// Keyframe interpolation right-click context menu
    pub keyframe_context_menu: Option<KeyframeContextMenu>,
    /// Keyframe clipboard for copy/paste between clips/params
    pub keyframe_clipboard: Option<KeyframeClipboard>,
    /// Currently selected keyframe on timeline (click to select, right-click for menu)
    pub selected_keyframe: Option<SelectedKeyframe>,
    /// Which audio parameter the timeline rubber band displays.
    pub rubber_band_param: AudioParam,
    /// Which video parameter the timeline rubber band displays.
    pub video_rubber_band_param: VideoParam,
    /// Audio prefs synced from preferences (fade overlay mode, show rubber band, show waveform)
    pub audio_fade_overlay_mode: crate::prefs::FadeOverlayMode,
    pub audio_show_rubber_band: bool,
    pub audio_show_waveform: bool,
    pub audio_anti_click_fade: bool,
    pub audio_show_crossfade_indicators: bool,

    /// Fade handle drag state: (clip_id, true=fade_in, false=fade_out, original_fade_secs)
    pub dragging_fade_handle: Option<(ClipId, bool, f32)>,

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

    // Viewer transform controls
    /// Active drag on a viewer transform handle
    pub viewer_transform_drag: Option<ViewerTransformDrag>,
    /// Whether on-screen transform handles are shown when a clip is selected
    pub show_transform_controls: bool,
    /// Active drag on a viewer mask handle
    pub viewer_mask_drag: Option<ViewerMaskDrag>,

    /// Whether the curve editor panel is visible
    pub show_curve_editor: bool,
    /// Whether the audio mixer panel is visible
    pub show_audio_mixer: bool,
    /// Whether the history panel is visible
    pub show_history_panel: bool,
    /// Command palette state
    pub show_command_palette: bool,
    pub command_palette_query: String,
    pub command_palette_selected: usize,

    /// Available system font families (populated at startup from font discovery)
    pub available_fonts: Vec<String>,

    /// Pending title data for styled title creation (consumed by CreateTitleClip handler)
    pub pending_title_data: Option<klipp_timeline::TitleData>,
    /// Title clip that needs its overlay texture refreshed on the next frame.
    pub pending_title_preview_clip: Option<klipp_core::ClipId>,
    /// Clear title overlay texture (animation preset applied — text starts invisible)
    pub clear_title_overlay: bool,

    /// Duration (in seconds) for title animation fade presets.
    pub title_fade_duration: f32,

    /// Whether the multicam viewer grid is active.
    pub multicam_viewer_active: bool,
    /// Number of angles in the current multicam group.
    pub multicam_angle_count: usize,
    /// Currently active multicam angle index.
    pub multicam_active_angle: u8,

    /// Pending animation identifier for title creation (consumed by CreateTitleClip handler).
    pub pending_title_animation: Option<String>,

    // Viewer text edit mode (double-click title in viewer)
    /// When true, the viewer shows an editable text overlay for a title clip.
    pub viewer_text_edit: bool,
    /// The text being edited (clone of TitleData.text, synced on every change).
    pub viewer_text_edit_buffer: String,
    /// Clip being text-edited.
    pub viewer_text_edit_clip: Option<ClipId>,
    /// Whether to request focus on the text edit widget (first frame only).
    pub viewer_text_edit_request_focus: bool,
    /// Character selection range in viewer text edit mode (character indices, start..end).
    /// This is updated ONLY when the TextEdit has focus and reports a selection.
    /// It persists when focus moves to the inspector so style changes apply to it.
    pub viewer_text_selection: Option<(usize, usize)>,
    /// Locked copy of the selection that survives focus changes.
    /// Set whenever a real selection is made, cleared only on explicit deselect or exit.
    pub viewer_text_selection_locked: Option<(usize, usize)>,
    /// Cursor position (character index) in viewer WYSIWYG text editor.
    pub viewer_text_cursor: usize,
    /// Cached character positions from title renderer (frame pixel coords).
    /// Recomputed when title text/style changes.
    pub viewer_char_positions: Vec<klipp_title::CharPosition>,
    /// Anchor position for shift-click / shift-arrow selection extension.
    pub viewer_text_select_anchor: Option<usize>,
    /// Wall-clock time of last cursor movement (for blink reset).
    pub viewer_text_cursor_blink_time: f64,
    /// Font size at current cursor position (resolved from style runs).
    /// Updated when cursor moves. Inspector shows this value.
    pub viewer_text_cursor_font_size: Option<f32>,
    /// Color at current cursor position (resolved from style runs).
    pub viewer_text_cursor_color: Option<[u8; 4]>,
}

/// A saved reference frame for grading comparison.
#[derive(Clone)]
pub struct GradeStill {
    pub name: String,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub texture: Option<egui::TextureHandle>,
}

impl std::fmt::Debug for GradeStill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GradeStill")
            .field("name", &self.name)
            .field("rgba_len", &self.rgba.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("has_texture", &self.texture.is_some())
            .finish()
    }
}

/// Eyedropper mode — determines what happens when the user clicks the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EyedropperMode {
    #[default]
    None,
    WhiteBalance,
    Qualifier,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewerOverlay {
    #[default]
    None,
    Zebra,
    FalseColor,
    SafeAreas,
    Grid,
    Checkerboard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeMode {
    #[default]
    Waveform,
    Parade,
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

/// Snapshot of a clip's attributes for Paste Attributes.
#[derive(Debug, Clone)]
pub struct AttributesClipboard {
    /// Built-in effects chain (GPU/CPU).
    pub builtin_effects: Vec<klipp_effects::BuiltinEffect>,
    pub builtin_effects_enabled: Vec<bool>,
    /// OFX effects chain.
    pub effects: Vec<klipp_plugin::AppliedEffect>,
    /// Node-based color grade.
    pub node_graph: klipp_nodes::NodeGraph,
    /// Audio params.
    pub volume: f32,
    pub pan: f32,
    pub gain: f32,
    pub fade_in_secs: f32,
    pub fade_out_secs: f32,
    pub fade_in_curve: klipp_timeline::FadeCurve,
    pub fade_out_curve: klipp_timeline::FadeCurve,
    pub volume_keyframes: Vec<klipp_core::Keyframe>,
    pub pan_keyframes: Vec<klipp_core::Keyframe>,
    pub gain_keyframes: Vec<klipp_core::Keyframe>,
    /// Audio effects.
    pub audio_effects: Vec<klipp_core::AudioEffectInstance>,
    /// Transform keyframes.
    pub opacity_keyframes: Vec<klipp_core::Keyframe>,
    pub transform_pos_x_keyframes: Vec<klipp_core::Keyframe>,
    pub transform_pos_y_keyframes: Vec<klipp_core::Keyframe>,
    pub transform_scale_keyframes: Vec<klipp_core::Keyframe>,
    pub transform_rotation_keyframes: Vec<klipp_core::Keyframe>,
    /// Effect keyframes.
    pub effect_keyframes: std::collections::HashMap<String, Vec<klipp_core::Keyframe>>,
    /// Param modifiers (behaviors).
    pub param_modifiers: std::collections::HashMap<klipp_core::ParamId, klipp_core::ModifierStack>,
    /// Speed.
    pub speed: f64,
    /// Speed keyframes.
    pub speed_keyframes: Vec<klipp_core::Keyframe>,
}

impl AppState {
    /// Returns true when any modal dialog, window, or popup menu is open.
    /// Used to block background interactions like skimming, timeline hover, etc.
    pub fn has_modal_open(&self) -> bool {
        self.modal_or_popup_open
            || self.show_preferences
            || self.show_unsaved_dialog
            || self.show_export_dialog
            || self.show_new_project_dialog
            || self.show_relink_dialog
            || self.show_about_dialog
            || self.show_autosave_recovery
            || self.show_render_queue
    }
}

#[derive(Debug, Clone)]
pub enum UnsavedAction {
    None,
    NewProject,
    OpenNewProjectDialog,
    OpenProject(PathBuf),
    CloseProject,
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
            active_workspace: Workspace::default(),
            custom_workspaces: Vec::new(),
            show_save_workspace_dialog: false,
            save_workspace_name: String::new(),
            sequences: vec![Sequence::new_default()],
            active_sequence_idx: 0,
            compound_clip_stack: Vec::new(),
            media_pool: MediaPool::new(),
            project_path: None,
            created_at: None,
            dirty: false,
            history: CommandHistory::new(),
            viewer_inspector_split: 0.6,
            upper_timeline_split: 0.65,
            timeline_zoom: 50.0,
            timeline_scroll_x: 0.0,
            timeline_scroll_y: 0.0,
            playhead_follow: PlayheadFollow::default(),
            track_header_width: 160.0,
            viewer_zoom: 0.0, // 0.0 = fit-to-window mode
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
            default_transition_frames: 24,
            timeline_prefs: Default::default(),
            viewer_prefs: Default::default(),
            snap_threshold_px: 10.0,
            zoom_step: 1.5,
            toast_duration_secs: 2.2,
            modal_backdrop_alpha: 120,
            scope_quality: Default::default(),
            scope_update_interval_ms: 32,
            edit_mode: klipp_timeline::magnetic::EditMode::default(),
            fullscreen_viewer: false,
            fullscreen_changed: false,
            clipboard: None,
            attributes_clipboard: None,
            show_paste_attributes_dialog: false,
            show_speed_dialog: false,
            speed_dialog_value: 100.0,
            speed_dialog_reverse: false,
            source_media_id: None,
            source_playhead: Timecode::zero(),
            selected_media_ids: std::collections::HashSet::new(),
            last_clicked_media_id: None,
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
            skim_decode_in_flight: false,
            skim_queued_request: None,
            browser_skim_active_this_frame: false,
            pending_skim_transcodes: Vec::new(),
            audio_skimming_enabled: true,
            skim_speed_px_per_sec: 0.0,
            skim_last_update: std::time::Instant::now(),
            skim_last_x: 0.0,
            skim_audio_session: None,
            skim_last_audio_dispatch: std::time::Instant::now(),
            stabilization_in_progress: std::collections::HashSet::new(),
            stabilization_progress: std::collections::HashMap::new(),
            tracking_mode: false,
            active_track_point: None,
            tracking_in_progress: std::collections::HashSet::new(),
            tracking_progress: std::collections::HashMap::new(),
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
            global_lut_enabled: true,
            show_color_panel: false,
            selected_node_id: None,
            show_node_strip: false,
            color_tab: ColorTab::default(),
            color_grade_drag_start: None,
            color_grade_clipboard: None,
            curves_single_mode: false,
            curve_histograms: None,
            slider_edit: Default::default(),
            show_preset_menu: false,
            preset_save_name: String::new(),
            show_preset_save: false,
            grade_presets: crate::presets::load_all_presets(),
            huesat_single_mode: false,
            huesat_active_curve: 0,
            show_scopes: false,
            scope_overlay_mode: false,
            scope_state: Default::default(),
            show_keyframe_editor: false,
            scope_mode: ScopeMode::Waveform,
            eyedropper_mode: EyedropperMode::None,
            wb_sample_request: None,
            qualifier_sample_request: None,
            matte_preview_active: false,
            before_after_active: false,
            split_position: 0.5,
            grade_stills: Vec::new(),
            active_grade_still: None,
            grade_still_compare: false,
            viewer_overlay: ViewerOverlay::None,
            frame_count: 0,
            available_plugins: Vec::new(),
            effect_registry: crate::browser::effect_registry::EffectRegistry::new(),
            effect_filter_builtin: true,
            effect_filter_ofx: true,
            effect_filter_gpu: true,
            effect_filter_hide_unsupported: false,
            preset_manager: crate::effect_presets::PresetManager::new(),
            effect_thumbnails: crate::browser::effect_thumbnails::ThumbnailRenderer::default(),
            render_queue: klipp_export::RenderQueue::new(),
            show_render_queue: false,
            filmstrip_generating: false,
            filmstrip_generate_start: std::time::Instant::now(),
            filmstrip_toast_dismissed: false,
            filmstrip_in_progress: std::collections::HashSet::new(),
            filmstrip_queue: Vec::new(),
            filmstrip_total_queued: 0,
            filmstrip_completed: 0,
            playhead_anchor: Timecode::zero(),
            playhead_anchor_wall: std::time::Instant::now(),
            playhead_speed: 1.0,
            playhead_smooth_secs: 0.0,
            collapsed_sections: std::collections::HashSet::new(),
            remember_inspector_layout: true,
            audio_backend_ok: false,
            audio_sample_rate: 48000,
            track_peak_levels: Vec::new(),
            master_peak_left: 0.0,
            master_peak_right: 0.0,
            master_volume: 1.0,
            master_muted: false,
            confirm_remove_track: None,
            show_markers_panel: false,
            show_browser_panel: true,
            reveal_in_browser: None,
            show_inspector_panel: true,
            modal_or_popup_open: false,
            show_preferences: false,
            auto_create_tracks: true,
            project_auto_filmstrips: None,
            project_auto_skim: None,
            show_media_panel: false,
            show_remove_media_confirm: None,
            show_delete_unused_confirm: None,
            export_offline_warning: None,
            media_usage_cache: std::collections::HashMap::new(),
            media_usage_cache_dirty: true,
            show_new_project_dialog: false,
            show_project_info: false,
            show_consolidate_dialog: false,
            renaming_sequence_idx: None,
            rename_sequence_buf: String::new(),
            show_autosave_recovery: false,
            autosave_recovery_path: None,
            autosave_original_path: None,
            dragging_audio_keyframe: None,
            dragging_video_keyframe: None,
            dragging_rubber_band: None,
            dragging_bezier_handle: None,
            dragging_video_bezier_handle: None,
            keyframe_context_menu: None,
            keyframe_clipboard: None,
            selected_keyframe: None,
            rubber_band_param: AudioParam::Volume,
            video_rubber_band_param: VideoParam::Opacity,
            audio_fade_overlay_mode: crate::prefs::FadeOverlayMode::default(),
            audio_show_rubber_band: true,
            audio_show_waveform: true,
            audio_anti_click_fade: true,
            audio_show_crossfade_indicators: true,
            dragging_fade_handle: None,
            proxy_generating: std::collections::HashSet::new(),
            proxy_progress: std::collections::HashMap::new(),
            proxy_start_times: std::collections::HashMap::new(),
            proxy_status_message: None,
            proxy_toast_dismissed: false,
            viewer_transform_drag: None,
            show_transform_controls: true,
            viewer_mask_drag: None,
            show_curve_editor: false,
            show_audio_mixer: false,
            show_history_panel: false,
            show_command_palette: false,
            command_palette_query: String::new(),
            command_palette_selected: 0,
            available_fonts: Vec::new(),
            pending_title_data: None,
            pending_title_preview_clip: None,
            clear_title_overlay: false,
            title_fade_duration: 0.5,
            multicam_viewer_active: false,
            multicam_angle_count: 0,
            multicam_active_angle: 0,
            pending_title_animation: None,
            viewer_text_edit: false,
            viewer_text_edit_buffer: String::new(),
            viewer_text_edit_clip: None,
            viewer_text_edit_request_focus: false,
            viewer_text_selection: None,
            viewer_text_selection_locked: None,
            viewer_text_cursor: 0,
            viewer_char_positions: Vec::new(),
            viewer_text_select_anchor: None,
            viewer_text_cursor_blink_time: 0.0,
            viewer_text_cursor_font_size: None,
            viewer_text_cursor_color: None,
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
