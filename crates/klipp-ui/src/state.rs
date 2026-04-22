use crate::history::CommandHistory;
use klipp_core::{AudioParam, ClipId, Fps, MediaId, Timecode, Tool, TrackId, TransitionId, VideoParam, ViewerMode};
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

/// Where to dock the color grading tools (wheels, curves, etc.).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorToolsPosition {
    /// Color tools in the right-side inspector panel (default).
    #[default]
    Right,
    /// Color tools in a panel below the viewer.
    Bottom,
}

/// A saved workspace layout (panel visibility + sizes + name).
///
/// Sizing fields are `Option<f32>` with `#[serde(default)]` so legacy
/// workspaces.json files saved before this field set loaded cleanly:
/// missing fields → `None` → the applier leaves the current dimension
/// alone rather than collapsing it.
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
    #[serde(default)]
    pub browser_width: Option<f32>,
    #[serde(default)]
    pub inspector_width: Option<f32>,
    #[serde(default)]
    pub timeline_height: Option<f32>,
    #[serde(default)]
    pub color_panel_height: Option<f32>,
}

/// The 5 built-in workspace layouts shipped out of the box. Mirrors
/// Kdenlive's workspace vocabulary (Logging / Editing / Effects /
/// Audio / Color) so users coming from Kdenlive find familiar names.
///
/// Never persisted to disk — computed fresh at runtime so layout
/// tweaks in later releases reach all users, not just new installs.
pub fn builtin_workspace_layouts() -> Vec<SavedWorkspaceLayout> {
    vec![
        // ── Logging ── media review focus: wide browser, narrow inspector,
        // tall-ish timeline for scrubbing through takes, markers panel open.
        SavedWorkspaceLayout {
            name: "Logging".into(),
            show_browser: true,
            show_inspector: false,
            show_history: false,
            show_markers: true,
            show_curve_editor: false,
            show_scopes: false,
            show_color_panel: false,
            browser_width: Some(420.0),
            inspector_width: Some(280.0),
            timeline_height: Some(180.0),
            color_panel_height: None,
        },
        // ── Editing ── the default balanced layout.
        SavedWorkspaceLayout {
            name: "Editing".into(),
            show_browser: true,
            show_inspector: true,
            show_history: false,
            show_markers: false,
            show_curve_editor: false,
            show_scopes: false,
            show_color_panel: false,
            browser_width: Some(280.0),
            inspector_width: Some(300.0),
            timeline_height: Some(240.0),
            color_panel_height: None,
        },
        // ── Effects ── wider inspector for effect params, curve editor on
        // for keyframe tweaking, narrower browser to save horizontal space.
        SavedWorkspaceLayout {
            name: "Effects".into(),
            show_browser: true,
            show_inspector: true,
            show_history: false,
            show_markers: false,
            show_curve_editor: true,
            show_scopes: false,
            show_color_panel: false,
            browser_width: Some(240.0),
            inspector_width: Some(360.0),
            timeline_height: Some(220.0),
            color_panel_height: None,
        },
        // ── Audio ── tall timeline so waveforms are readable, curve editor
        // for volume/pan automation, narrower inspector sized for the Audio
        // inspector tab rather than the wider Effects tab.
        SavedWorkspaceLayout {
            name: "Audio".into(),
            show_browser: true,
            show_inspector: true,
            show_history: false,
            show_markers: false,
            show_curve_editor: true,
            show_scopes: false,
            show_color_panel: false,
            browser_width: Some(240.0),
            inspector_width: Some(340.0),
            timeline_height: Some(300.0),
            color_panel_height: None,
        },
        // ── Color ── browser hidden to maximise scopes + viewer real estate,
        // color panel below viewer, scopes on, short timeline since editing
        // is usually done before grading.
        SavedWorkspaceLayout {
            name: "Color".into(),
            show_browser: false,
            show_inspector: true,
            show_history: false,
            show_markers: false,
            show_curve_editor: false,
            show_scopes: true,
            show_color_panel: true,
            browser_width: Some(200.0),
            inspector_width: Some(340.0),
            timeline_height: Some(180.0),
            color_panel_height: Some(320.0),
        },
    ]
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
    /// Show the workspace-manager modal (delete/rename/export/import).
    pub show_workspace_manager: bool,
    /// Currently highlighted workspace name in the manager dialog.
    /// String rather than index so built-ins and user presets share one
    /// selector without needing a variant enum.
    pub workspace_manager_selected: Option<String>,

    // Project
    pub sequences: Vec<Sequence>,
    pub active_sequence_idx: usize,
    /// Breadcrumb stack for navigating into compound clips. Each entry is
    /// (parent_sequence_idx, parent_playhead) so we can navigate back.
    pub compound_clip_stack: Vec<(usize, klipp_core::Timecode)>,
    pub media_pool: MediaPool,
    pub project_path: Option<PathBuf>,
    pub created_at: Option<String>,
    /// Free-form project notes — shot list, delivery specs, anything
    /// the editor wants to keep alongside the project. Persisted to
    /// project.json and edited via the Project Notes window.
    pub project_notes: String,
    /// Whether the Project Notes window is open.
    pub show_project_notes: bool,
    /// Mirrored from `prefs.audio.tp_oversample`. Drives the meter
    /// tooltip wording so users know whether they're seeing real
    /// 4× true-peak or sample-hold.
    pub tp_oversample: bool,
    pub dirty: bool,
    pub history: CommandHistory,

    // Layout
    pub viewer_inspector_split: f32,
    pub upper_timeline_split: f32,
    pub timeline_zoom: f32,
    pub timeline_scroll_x: f32,
    pub timeline_scroll_y: f32,
    /// (track_idx, pointer_y_at_drag_start, original_height) — set while a track resize is in progress.
    pub timeline_resizing_track: Option<(usize, f32, f32)>,
    /// Per-track pre-collapse height memory for double-click collapse/expand toggle.
    pub timeline_track_pre_collapse: std::collections::HashMap<usize, f32>,
    pub playhead_follow: PlayheadFollow,
    pub track_header_width: f32,
    /// Viewer zoom level (1.0 = fit, 0.25/0.5/2.0/4.0 for pixel-level zoom).
    pub viewer_zoom: f32,
    pub last_timeline_content_width: f32,
    pub show_overview_bar: bool,

    // Selection
    pub selected_clip: Option<ClipId>,
    /// UI-only state — not undoable. Multi-clip selection is managed directly
    /// for performance (marquee updates every frame). Do not convert to commands.
    pub selected_clips: Vec<ClipId>,
    pub hovered_clip: Option<ClipId>,
    /// Currently selected transition (track_idx, transition_id). UI-only state.
    pub selected_transition: Option<(usize, TransitionId)>,

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

    // Transport-bar jog wheel state (#38, scoped — software jog only).
    // Lives on AppState so in-progress drags survive panel relayouts.
    pub jog_wheel_state: crate::widgets::jog_wheel::JogWheelState,
    /// Whether the export-preset manager modal is visible. Opens from a
    /// button next to the preset dropdown in the export dialog.
    pub show_preset_manager: bool,
    /// Currently highlighted preset id inside the manager dialog. Session
    /// state only — lives on AppState rather than locally so the selection
    /// survives re-opens of the dialog within the same session.
    pub preset_manager_selected: Option<String>,

    // Cache manager dialog (#78)
    /// Whether the Cache Manager modal is visible.
    pub show_cache_manager: bool,
    /// Sized snapshot of on-disk cache categories for the Cache Manager
    /// dialog. Re-populated via `RefreshCacheManager` when opened or after
    /// a clear action.
    pub cache_categories: Vec<crate::cache_manager::CacheCategory>,

    // Clip properties dialog (#47)
    /// Whether the Clip Properties modal is visible.
    pub show_clip_properties: bool,
    /// Media item currently open in the Clip Properties modal. `None` when
    /// the modal is hidden.
    pub clip_properties_media_id: Option<MediaId>,
    /// Active tab inside the Clip Properties modal.
    pub clip_properties_tab: ClipPropertiesTab,

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
    /// Default resolution preset index for new sequences (mirrored from
    /// `prefs.editing.default_sequence_resolution_idx`). Index into
    /// `panels::new_project_dialog::RESOLUTION_PRESETS`. 0 = Automatic
    /// (inherit from active sequence). Used by the timeline "+" button.
    pub default_sequence_resolution_idx: usize,
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
    /// Whether magnetic timeline is enabled (synced from prefs).
    pub magnetic_timeline_enabled: bool,
    /// Whether multicam angle badges/cut list are drawn on timeline clips (synced from prefs).
    pub multicam_show_angles: bool,
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

    // Source viewer
    pub source_media_id: Option<MediaId>,
    pub source_playhead: Timecode,
    /// First-frame texture for the currently-loaded source clip. The
    /// `MediaId` is paired with the texture so a stale frame from a
    /// previous load is dropped the moment the user switches clips,
    /// rather than briefly flashing the old image.
    pub source_video_texture: Option<(MediaId, egui::TextureHandle)>,

    // Media browser multi-select
    pub selected_media_ids: std::collections::HashSet<MediaId>,
    pub last_clicked_media_id: Option<MediaId>,

    // Phase 4: Proxy
    pub use_proxies: bool,
    /// True while the user is actively scrubbing (dragging playhead on timeline).
    pub is_scrubbing: bool,
    /// Saved proxy state before scrub started (restored when scrub ends).
    pub pre_scrub_use_proxies: bool,
    /// Whether playback was running when scrub started — restored on scrub end.
    pub pre_scrub_was_playing: bool,
    /// True while waiting for a full-res frame after a seek — viewer shows filmstrip preview.
    pub showing_thumbnail_preview: bool,
    /// Set to true when a SeekTo is sent, cleared when FrameReady arrives.
    pub awaiting_seek_frame: bool,
    /// Render cache coverage per clip: (clip_id, cached_count, total_count).
    pub render_cache_ranges: Vec<(klipp_core::ClipId, u32, u32)>,

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
    /// Whether VIDEO files in the file browser are hover-skimmable.
    /// Mirrored from `prefs.file_browser.file_browser_video_skim`. Independent
    /// of the global `skimming_enabled` toolbar toggle — both must be on
    /// for the file browser to spawn skim decodes.
    pub file_browser_video_skim_enabled: bool,
    /// Whether AUDIO files in the file browser are hover-skimmable.
    /// Mirrored from `prefs.file_browser.file_browser_audio_skim`. Same gating
    /// as `file_browser_video_skim_enabled`.
    pub file_browser_audio_skim_enabled: bool,
    pub toast: crate::widgets::toast::ToastState,
    pub drag_drop: crate::drag_drop::DragDropState,

    // Deferred texture drops. egui-wgpu destroys the underlying
    // `wgpu::Texture` synchronously both when a `TextureHandle` is
    // dropped AND when `TextureHandle::set` is called with an image
    // whose dimensions differ from the current texture — in the
    // latter case the handle is reused but the wgpu::Texture backing
    // it is replaced. The crash surfaces as
    // "Texture with 'egui_texid_Managed(N)' has been destroyed"
    // during the next `Queue::submit` that still references the old
    // ID from a pending command buffer. glow tolerated this;
    // wgpu validation does not.
    //
    // We hold handles we intend to drop in a three-slot ring (two
    // full frames of deferral). At the start of every `update()` the
    // slot index advances and the newly-current slot is cleared —
    // dropping the handles stashed there ~2 frames earlier, by which
    // point every submit that could have referenced them has
    // completed on the GPU.
    //
    // Every `TextureHandle::set` with a potentially changing frame
    // size must be size-guarded: if the new image differs, take the
    // old handle, push it here, and allocate fresh. The helper
    // `KlippApp::upload_texture_deferred` implements this pattern.
    pub deferred_texture_drops: [Vec<egui::TextureHandle>; 3],
    pub deferred_texture_drops_slot: usize,

    // Browser skimming
    pub browser_skim_texture: Option<egui::TextureHandle>,
    pub browser_skim_media_id: Option<MediaId>,
    pub skim_transcoding: std::collections::HashSet<MediaId>,
    /// Active skim transcode progress: (filename, progress 0.0..1.0)
    pub skim_transcode_status: Option<(String, f32)>,
    /// When the last skim transcode finished (for "Done" display)
    pub skim_transcode_done_at: Option<std::time::Instant>,
    /// Pending skim frame RGBA to upload as texture (media_id, rgba, width, height)
    /// `(media_id, rgba, width, height, target)` — pending skim frame
    /// awaiting upload. `target` decides whether it lands in the
    /// engine's frame buffer (Program) or the source viewer's overlay
    /// texture (Source).
    pub browser_skim_pending: Option<(MediaId, Vec<u8>, u32, u32, klipp_core::ViewerMode)>,
    /// True while a skim decode thread is running (prevents spawning more threads)
    pub skim_decode_in_flight: bool,
    /// When the current skim decode was started (for watchdog timeout reset)
    pub skim_decode_started_at: Option<std::time::Instant>,
    /// Latest skim request queued while a decode was in flight (media_id, time_secs)
    /// `(media_id, time_secs, target)` — queued skim request waiting
    /// for the in-flight decode to finish. `target` follows the request
    /// through the queue so the eventual response routes to the correct
    /// viewer.
    pub skim_queued_request: Option<(MediaId, f64, klipp_core::ViewerMode)>,
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
    /// System audio mute detection (Linux/PipeWire only). Polled via
    /// `wpctl get-volume @DEFAULT_AUDIO_SINK@` every ~2 seconds. When
    /// the system sink is muted, this is `true` and the transport bar
    /// shows a warning so users don't chase phantom audio bugs.
    pub system_audio_muted: bool,
    pub system_audio_last_check: std::time::Instant,
    /// When the last "muted by system" toast was shown. Used to re-show
    /// the reminder at the configured interval.
    pub system_audio_last_reminder: std::time::Instant,
    /// Reminder interval in seconds, mirrored from prefs.
    pub mute_reminder_interval_secs: u32,
    /// True when the file browser panel has keyboard focus (user
    /// clicked a row). Mirrored from `BrowserPanel::files_panel_focused`
    /// so the transport handler knows to yield Spacebar to Quick Look
    /// instead of triggering play/pause.
    pub files_panel_focused: bool,
    /// Quick Look preview: the file currently being previewed via
    /// Spacebar in the file browser. `None` = Quick Look is closed.
    /// The path is used to decode a preview frame (video/image) or
    /// start an audio playback session.
    pub quick_look_path: Option<std::path::PathBuf>,
    /// Cached texture for the Quick Look preview (video poster frame
    /// or decoded image). `None` = not yet decoded or audio file.
    pub quick_look_texture: Option<egui::TextureHandle>,
    /// Cached media header info for the Quick Look overlay text.
    pub quick_look_info: Option<klipp_engine::MediaHeader>,
    /// Audio session for Quick Look (audio files auto-play).
    pub quick_look_audio: Option<klipp_engine::skim_audio::SkimAudioSession>,
    /// Whether the Recents sidebar button is visible. Mirrored from
    /// `prefs.file_browser.show_recent_files`.
    pub show_recent_files: bool,
    /// Whether filmstrip hover preview is enabled in the file browser.
    pub filmstrip_hover_enabled: bool,
    /// Decode quality for filmstrip hover frames.
    pub filmstrip_quality: crate::prefs::FilmstripQuality,
    /// Keep filmstrip tiles visible after cursor leaves.
    pub filmstrip_persist: bool,
    /// Whether the file browser auto-refreshes on external filesystem
    /// changes. Mirrored from `prefs.file_browser.auto_refresh`.
    pub file_browser_auto_refresh: bool,
    /// Snapshot of `prefs.file_browser.recent_files` synced into state so
    /// the browser panel can read it without borrowing `prefs`. Updated
    /// at startup and whenever the prefs change.
    pub recent_files_snapshot: Vec<std::path::PathBuf>,
    /// Time of last audio skim decode dispatch (throttle to prevent starvation).
    pub skim_last_audio_dispatch: std::time::Instant,

    // Video stabilization analysis state
    pub stabilization_in_progress: std::collections::HashSet<klipp_core::ClipId>,
    pub stabilization_progress: std::collections::HashMap<klipp_core::ClipId, f32>,

    // Scene (shot-boundary) detection state
    pub scene_detect_in_progress: std::collections::HashSet<klipp_core::ClipId>,
    pub scene_detect_progress: std::collections::HashMap<klipp_core::ClipId, f32>,

    // Audio spectrum analyser — shared PCM ring fed by the mix thread,
    // plus the latest FFT magnitude bins (dBFS, log-frequency) rendered by
    // the audio-spectrum scope. Sample rate is set from the engine once.
    pub spectrum_ring: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>,
    pub spectrum_bins: Vec<f32>,
    pub spectrum_sample_rate: u32,

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
    /// Whether a perf-telemetry recording is currently running. When
    /// true, the overlay shows a "REC" badge and the ring buffer
    /// captures samples from the `begin_recording` mark forward.
    pub perf_recording_active: bool,
    /// UI-side signal that something (menu / toolbar / keybinding
    /// other than the `P`/`Ctrl+Shift+P` shortcut) requested the
    /// recording be toggled. `KlippApp::update` consumes this on
    /// each paint and dispatches to `toggle_perf_recording`.
    pub pending_perf_recording_toggle: bool,
    /// Path of the most recent session dump written by a completed
    /// recording. Used by the overlay and toast to let the user find
    /// the file.
    pub perf_last_session_path: Option<std::path::PathBuf>,
    /// Latest per-frame perf sample snapshotted from the engine's
    /// shared ring buffer. `None` until the engine processes its
    /// first frame.
    pub perf_latest_sample: Option<klipp_engine::perf::PerfSample>,
    /// Rolling averages over the last ~2 s of samples (or the whole
    /// ring if it's shorter). Refreshed on the same 250 ms cadence
    /// as `perf_fps` / `perf_cpu_percent`.
    pub perf_averages: klipp_engine::perf::PerfAverages,
    /// Drops observed within the last second of wall clock.
    /// Painted in the overlay as "Drops: N in last 1s".
    pub perf_drops_last_sec: u32,
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
    /// Per-job frame progress: media_id → (frames_done, frames_expected).
    /// Updated on every `FilmstripResult::Frame`. Cleared on `Done`.
    /// Drives the "N/500" indicator in the status bar so the user can
    /// tell how much of a 4K filmstrip is still pending.
    pub filmstrip_progress: std::collections::HashMap<MediaId, (u32, u32)>,

    // Global LUT
    pub global_lut_path: Option<PathBuf>,
    pub global_lut_opacity: f32,
    pub global_lut_enabled: bool,

    // Color panel / node graph
    pub show_color_panel: bool,
    /// Where color tools are docked: Right (inspector) or Bottom (below viewer).
    pub color_tools_position: ColorToolsPosition,
    /// Selected node in the node graph (shared between canvas and inspector).
    pub selected_node_id: Option<klipp_nodes::NodeId>,
    /// Whether the compact node strip is visible in Color workspace.
    pub show_node_strip: bool,
    /// Active tab in the simple (non-node) color inspector.
    pub color_tab: ColorTab,        // right inspector panel
    pub color_tab_bottom: ColorTab, // bottom panel
    pub color_grade_drag_start: Option<klipp_nodes::NodeGraph>,
    pub color_grade_clipboard: Option<klipp_nodes::NodeGraph>,
    pub curves_single_mode: bool,
    pub curve_histograms: Option<klipp_color::scopes::HistogramData>,
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
    pub scope_scale: ScopeScale,
    pub scope_show_r: bool,
    pub scope_show_g: bool,
    pub scope_show_b: bool,
    pub scope_low_pass: bool,
    pub scope_bloom: bool,
    pub scope_settings_open: bool,
    /// Floating scope window state.
    pub scope_window: ScopeWindowState,

    /// Viewer image rect (set by viewer each frame for scope cursor sampling).
    pub viewer_image_rect: Option<egui::Rect>,
    /// The viewer panel's outer rect (NOT the post-letterbox image rect).
    /// Set every frame by `render_main_layout` (windowed) or `render_fullscreen_viewer`
    /// (fullscreen). Used by overlays that need to align with the viewer regardless of
    /// layout mode. `egui::Rect::NOTHING` until first frame.
    pub viewer_panel_rect: egui::Rect,
    /// Pointer position over the viewer image (set by viewer on hover, None when not hovering).
    pub viewer_pointer_pos: Option<egui::Pos2>,

    // ── Bottom panel scope cursor (lock-capable) ──
    /// When true, the scope cursor is locked to a fixed position.
    pub scope_cursor_locked: bool,
    /// The locked pointer position (screen coords, set on click).
    pub scope_cursor_locked_pos: Option<egui::Pos2>,
    /// The locked cursor data (computed at lock time).
    pub scope_cursor_locked_data: Option<ScopeCursorData>,
    /// Sample box half-size for the bottom panel scope cursor.
    pub scope_cursor_sample_radius: u32,
    /// Whether to show the scope cursor indicator on the bottom panel.
    pub scope_cursor_show: bool,

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

    // Stabilization compare split view (raw vs stabilized).
    // Mutually exclusive with before_after_active and grade_still_compare.
    pub stabilization_compare_active: bool,

    /// Per-effect split compare. `Some((clip_id, effect_index, is_ofx))`
    /// when a single effect is being A/B'd in the viewer — left half of
    /// the split renders that effect bypassed, right half renders it on.
    /// `is_ofx` distinguishes `clip.effects[idx]` (true) from
    /// `clip.builtin_effects[idx]` (false). Mutually exclusive with
    /// before_after_active / stabilization_compare_active (handler sets
    /// only one at a time). None = inactive.
    pub split_compare_effect: Option<(ClipId, usize, bool)>,

    // Grade stills (reference frame comparison)
    /// Saved grade reference stills for comparison.
    pub grade_stills: Vec<GradeStill>,
    /// Index of the active grade still being compared (None = no comparison).
    pub active_grade_still: Option<usize>,
    /// Whether split-view comparison with a grade still is active.
    pub grade_still_compare: bool,

    /// Reference frame captured for the next ColorMatch run. Stored as
    /// raw RGBA bytes + dimensions; not persisted with the project,
    /// purely a working buffer between "Capture reference" and
    /// "Apply match" clicks in the inspector. The match algorithm
    /// produces a small `ColorMatchSettings` that IS persisted on
    /// the node, so the reference frame's bytes don't need to live
    /// past the compute step.
    pub color_match_reference: Option<(Vec<u8>, u32, u32)>,

    // Viewer overlay (zebra / false color)
    pub viewer_overlays: ViewerOverlays,
    pub frame_count: u64, // for zebra animation
    /// Pre-built 256-entry LUT for false color: luma → [R, G, B]. Rebuilt when palette changes.
    pub false_color_lut: [u8; 768],
    /// Pending false color palette switch from toolbar quick-switcher (applied next frame).
    pub pending_false_color_palette: Option<crate::prefs::FalseColorPalette>,

    // OFX plugins
    pub available_plugins: Vec<klipp_plugin::OfxPluginInfo>,

    // LADSPA audio plugins — discovered once at startup.
    pub ladspa_plugins: Vec<klipp_audio::ladspa::LadspaPluginInfo>,

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

    // ── Audio recording (#60 tier 2a/2b) ──────────────────────────────
    // None of these serialize — recording state is transient per app run.
    // AppState itself is not Serialize, so no serde annotations needed.
    /// True while a recording is in progress.
    pub is_recording: bool,
    /// Track to place the recorded clip on when recording stops.
    pub recording_track_id: Option<klipp_core::TrackId>,
    /// Destination WAV path of the active recording.
    pub recording_path: Option<std::path::PathBuf>,
    /// Stop-signal sender for the WAV writer thread. Sending `()` tells it
    /// to drain the final ring-buffer tail, patch the WAV header sizes, and
    /// exit cleanly.
    pub recording_stop_tx: Option<std::sync::mpsc::Sender<()>>,
    /// Live cpal input stream. Dropping halts capture; kept here so the
    /// stream lives as long as the recording session.
    pub recording_audio_input: Option<klipp_engine::audio_input::AudioInput>,

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
    /// BS.1770-4 LUFS metering (raw from engine)
    pub lufs_momentary: f32,
    pub lufs_short_term: f32,
    pub lufs_integrated: f32,
    pub lufs_true_peak_dbfs: f32,
    /// Loudness range (EBU R128 LRA)
    pub lufs_lra: f32,
    /// Rolling short-term LUFS history for the loudness-history scope.
    /// Capacity: 600 samples @ ~10 Hz = ~60 seconds of timeline. Pushed
    /// once per `LufsReadings` event (see app.rs handler) and popped from
    /// the front when the cap is exceeded.
    pub lufs_short_term_history: std::collections::VecDeque<f32>,
    /// Rolling history of FFT bin slices for the spectrogram waterfall.
    /// Each entry is one normalized spectrum snapshot (256 bins, 0.0–1.0).
    /// Capacity: 300 slices ≈ ~10s at 30fps refresh. Pushed from the
    /// spectrum refresh loop in `app.rs` and popped from the front when
    /// the cap is exceeded.
    pub spectrum_history: std::collections::VecDeque<Vec<f32>>,
    /// Smoothed display values (EMA-filtered for stable visual)
    pub lufs_momentary_smooth: f32,
    pub lufs_short_term_smooth: f32,
    /// Latched clipping indicator (true if any sample exceeded 0 dBFS since last reset)
    pub clip_indicator: bool,
    /// LUFS target from preferences (-23 EBU R128, -24 ATSC A/85)
    pub lufs_target: f32,
    /// Peak hold time in seconds from preferences
    pub peak_hold_time_secs: f32,
    /// Mixer: track name being edited (track_index, buffer)
    pub mixer_editing_name: Option<(usize, String)>,

    // Phase 7: Dynamic tracks
    pub confirm_remove_track: Option<TrackId>,
    pub show_markers_panel: bool,
    /// Visibility of the dedicated subtitle editor panel.
    pub show_subtitle_editor: bool,
    /// Per-session editing state for the subtitle editor panel.
    pub subtitle_editor: SubtitleEditorState,
    /// Live state for a running project-consolidation operation (None = no
    /// consolidation in progress). Progress + completion arrive via the
    /// embedded mpsc channel; see consolidate::ConsolidateMessage.
    pub consolidate_progress: Option<ConsolidateProgress>,
    /// Editable destination path in the consolidate dialog.
    pub consolidate_dest_path: String,
    /// Dialog toggle: include proxy files.
    pub consolidate_include_proxies: bool,
    /// Dialog toggle: include skim transcodes.
    pub consolidate_include_skims: bool,
    /// Dialog toggle: after copying, zip the consolidated folder into
    /// `<dest_dir>.zip` alongside it. Default off.
    pub consolidate_create_zip: bool,
    /// Dialog toggle: after a successful zip, recursively delete the
    /// consolidated folder so only the archive remains. Default off.
    /// Only visible when `consolidate_create_zip` is true.
    pub consolidate_delete_folder_after_zip: bool,
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
    // Batch rename dialog (for bin media items).
    pub show_batch_rename_dialog: bool,
    pub batch_rename_ids: Vec<MediaId>,
    pub batch_rename_mode: crate::app::BatchRenameMode,
    pub batch_rename_find: String,
    pub batch_rename_replace: String,
    pub batch_rename_match_case: bool,
    pub batch_rename_prefix: String,
    pub batch_rename_suffix: String,
    pub batch_rename_start: u32,
    pub batch_rename_padding: u8,
    // Project Info window
    pub show_project_info: bool,
    /// File Info window — Some(path) when open, None when closed.
    /// Set from the file browser right-click menu.
    pub file_info: Option<std::path::PathBuf>,
    /// Lazily-loaded thumbnail texture for the currently-shown File Info window.
    /// Cleared when `file_info` is reset to None. Loaded from the shared
    /// media-pool disk cache (`klipp_media::disk_cache`).
    pub file_info_thumb: Option<egui::TextureHandle>,
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
    /// Per-track waveform amplitude zoom. Default is 1.0 when a key is absent.
    /// Alt+scroll over an audio track adjusts this; right-click track header resets it.
    /// Clamped to [0.1, 8.0] by the UI handlers.
    pub waveform_zoom: std::collections::HashMap<TrackId, f32>,

    /// The clip currently designated as the audio-sync reference (set via the
    /// clip context menu). `AlignToAudioReference` shifts a target clip's
    /// timeline position to align its audio to this reference.
    pub audio_reference_clip_id: Option<ClipId>,

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
    /// Set during scrub when no title clip is under the playhead — signals
    /// the next `upload_title_overlay` to clear the overlay so stale titles
    /// don't linger as the user drags past them.
    pub pending_title_overlay_clear: bool,
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

/// A saved reference frame for grading comparison. Persistence is via
/// PNG sidecars in `<project>/media/stills/` indexed by `filename`;
/// only `name + filename` survive in JSON, the rgba pixels are loaded
/// from disk on project open.
#[derive(Clone)]
pub struct GradeStill {
    pub name: String,
    /// Sidecar PNG filename (e.g. `still_1700000000.png`). Empty until
    /// the still is saved or attached to a project on disk.
    pub filename: String,
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub texture: Option<egui::TextureHandle>,
}

impl std::fmt::Debug for GradeStill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GradeStill")
            .field("name", &self.name)
            .field("filename", &self.filename)
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

/// Messages sent from the consolidation background thread to the UI thread.
///
/// Ordering guarantees: `Progress`* then `Done`. If the optional archive
/// phase runs, `ArchiveProgress`* then `ArchiveDone` follow, strictly after
/// `Done`.
#[derive(Debug)]
pub enum ConsolidateMessage {
    Progress {
        done: usize,
        total: usize,
        filename: String,
    },
    Done(Result<klipp_project::consolidate::ConsolidateResult, String>),
    /// Phase 2 archive progress in bytes (granular per-file).
    ArchiveProgress {
        bytes_done: u64,
        bytes_total: u64,
        filename: String,
    },
    /// Phase 2 terminal message. On success carries the zip path; on failure
    /// the error string (partial zip is already deleted by the writer).
    ArchiveDone(Result<std::path::PathBuf, String>),
}

/// Live state for an active consolidation operation. Dropped when the
/// operation completes (success or failure).
pub struct ConsolidateProgress {
    pub files_done: usize,
    pub files_total: usize,
    pub current_file: String,
    /// Set at construction time from the dialog's "Also create .zip archive"
    /// checkbox. The drain uses it to know whether to keep the progress
    /// struct alive after Phase 1 `Done` (archive messages will follow) or
    /// drop it immediately (Phase 1 was the only phase).
    pub expects_archive: bool,
    /// Set to true once the first `ArchiveProgress` or `ArchiveDone` message
    /// arrives. Keeps the dialog in archive mode for status rendering.
    pub archiving: bool,
    pub archive_bytes_done: u64,
    pub archive_bytes_total: u64,
    pub receiver: std::sync::mpsc::Receiver<ConsolidateMessage>,
}

impl std::fmt::Debug for ConsolidateProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsolidateProgress")
            .field("files_done", &self.files_done)
            .field("files_total", &self.files_total)
            .field("current_file", &self.current_file)
            .field("expects_archive", &self.expects_archive)
            .field("archiving", &self.archiving)
            .field("archive_bytes_done", &self.archive_bytes_done)
            .field("archive_bytes_total", &self.archive_bytes_total)
            .finish_non_exhaustive()
    }
}

/// Per-session editing state for the subtitle editor panel.
/// Not persisted — resets each time the panel opens.
#[derive(Debug, Clone, Default)]
pub struct SubtitleEditorState {
    /// Which clip's text field is being edited (None = display mode).
    pub editing_text: Option<ClipId>,
    /// Text buffer for the active text edit.
    pub text_buf: String,
    /// Which timecode field is being edited (None = display mode).
    /// Scoped by egui::Id so start- and end-fields of the same row don't collide.
    pub editing_timecode: Option<egui::Id>,
    /// Timecode edit buffer.
    pub timecode_buf: String,
    /// Ripple mode — when true, timing changes shift downstream clips.
    pub ripple: bool,
    /// Selected subtitle track (None = none selected / no filter). Used by the
    /// track-grouped editor to highlight the active track.
    pub selected_subtitle_track: Option<klipp_core::TrackId>,
    /// Set of subtitle track IDs currently collapsed in the editor.
    pub subtitle_track_collapsed: std::collections::HashSet<klipp_core::TrackId>,
    /// Track being renamed inline (header double-click). Name edit buffer.
    pub editing_track_name: Option<klipp_core::TrackId>,
    pub track_name_buf: String,
}

/// Independent viewer overlay toggles — multiple can be active simultaneously.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewerOverlays {
    pub zebras: bool,
    pub false_color: bool,
    pub focus_peaking: bool,
    pub safe_areas: bool,
    pub grid: bool,
    pub checkerboard: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScopeMode {
    #[default]
    Waveform,
    Parade,
    Vectorscope,
    Histogram,
}

/// Scope scale display mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ScopeScale {
    #[default]
    TenBit,
    Ire,
    Percent,
}

impl ScopeScale {
    pub fn label(&self) -> &'static str {
        match self {
            Self::TenBit => "10-bit",
            Self::Ire => "0\u{2013}100 IRE",
            Self::Percent => "0\u{2013}100%",
        }
    }
    pub fn max_val(&self) -> f32 {
        match self { Self::TenBit => 1023.0, _ => 100.0 }
    }
    pub fn button_label(&self) -> &'static str {
        match self { Self::TenBit => "10-bit", Self::Ire => "IRE", Self::Percent => "%" }
    }
}

/// Pixel data sampled from the viewer for scope cursor overlay.
#[derive(Debug, Clone, Copy)]
pub struct ScopeCursorData {
    /// Normalized x position in the video frame (0.0–1.0).
    pub x_frac: f32,
    /// Linear RGB values (0.0–1.0).
    pub r: f32,
    pub g: f32,
    pub b: f32,
    /// Luma (BT.709).
    pub luma: f32,
    /// CbCr for vectorscope.
    pub cb: f32,
    pub cr: f32,
}

impl ScopeCursorData {
    pub fn from_srgb(x_frac: f32, sr: f32, sg: f32, sb: f32) -> Self {
        let luma = 0.2126 * sr + 0.7152 * sg + 0.0722 * sb;
        let cb = -0.1146 * sr - 0.3854 * sg + 0.5000 * sb;
        let cr = 0.5000 * sr - 0.4542 * sg - 0.0458 * sb;
        Self { x_frac, r: sr, g: sg, b: sb, luma, cb, cr }
    }

    /// Convert sRGB to HSL.
    pub fn to_hsl(&self) -> (f32, f32, f32) {
        let max = self.r.max(self.g).max(self.b);
        let min = self.r.min(self.g).min(self.b);
        let l = (max + min) / 2.0;
        if (max - min).abs() < 1e-6 {
            return (0.0, 0.0, l);
        }
        let d = max - min;
        let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
        let h = if (max - self.r).abs() < 1e-6 {
            (self.g - self.b) / d + if self.g < self.b { 6.0 } else { 0.0 }
        } else if (max - self.g).abs() < 1e-6 {
            (self.b - self.r) / d + 2.0
        } else {
            (self.r - self.g) / d + 4.0
        };
        (h * 60.0, s, l)
    }
}

// ── Floating scope window types ─────────────────────────────────────────

/// Active tab in the Clip Properties modal dialog (#47). Session state
/// only — not persisted across runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipPropertiesTab {
    #[default]
    Info,
    File,
    Tags,
    Markers,
    Usage,
    Streams,
    Metadata,
}

impl ClipPropertiesTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "Info",
            Self::File => "File",
            Self::Tags => "Tags",
            Self::Markers => "Markers",
            Self::Usage => "Usage",
            Self::Streams => "Streams",
            Self::Metadata => "Metadata",
        }
    }

    pub const ALL: [ClipPropertiesTab; 7] = [
        Self::Info, Self::File, Self::Tags, Self::Markers,
        Self::Usage, Self::Streams, Self::Metadata,
    ];
}

/// Scope type for floating window panes (extends ScopeMode with CIE).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ScopeType {
    #[default]
    Waveform,
    Parade,
    Vectorscope,
    Histogram,
    CIEChromaticity,
    AudioSpectrum,
    /// Audio phase/correlation goniometer — Lissajous plot of L vs R plus
    /// a Pearson-correlation strip meter. Audio-only (skips the video
    /// frame-buffer lock, same as `AudioSpectrum`).
    AudioCorrelation,
    /// Rolling LUFS history graph — short-term LUFS over the last ~60s
    /// with the user's loudness target drawn as a horizontal line.
    LoudnessHistory,
    /// Scrolling FFT waterfall — x=time, y=frequency, color=magnitude.
    /// Rolling 10-second history of normalized spectrum slices; audio-only
    /// (skips the video frame-buffer lock, same as other audio scopes).
    Spectrogram,
}

impl From<ScopeMode> for ScopeType {
    fn from(m: ScopeMode) -> Self {
        match m {
            ScopeMode::Waveform => Self::Waveform,
            ScopeMode::Parade => Self::Parade,
            ScopeMode::Vectorscope => Self::Vectorscope,
            ScopeMode::Histogram => Self::Histogram,
        }
    }
}

impl ScopeType {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Waveform => "Waveform",
            Self::Parade => "Parade",
            Self::Vectorscope => "Vectorscope",
            Self::Histogram => "Histogram",
            Self::CIEChromaticity => "CIE Chromaticity",
            Self::AudioSpectrum => "Audio Spectrum",
            Self::AudioCorrelation => "Audio Correlation",
            Self::LoudnessHistory => "Loudness History",
            Self::Spectrogram => "Spectrogram",
        }
    }

    pub const ALL: [ScopeType; 9] = [
        Self::Waveform, Self::Parade, Self::Vectorscope, Self::Histogram, Self::CIEChromaticity, Self::AudioSpectrum,
        Self::AudioCorrelation, Self::LoudnessHistory, Self::Spectrogram,
    ];

    /// Convert to ScopeMode for the existing render pipeline (CIE/Audio scopes have no video render mode).
    pub fn to_scope_mode(&self) -> Option<ScopeMode> {
        match self {
            Self::Waveform => Some(ScopeMode::Waveform),
            Self::Parade => Some(ScopeMode::Parade),
            Self::Vectorscope => Some(ScopeMode::Vectorscope),
            Self::Histogram => Some(ScopeMode::Histogram),
            Self::CIEChromaticity => None,
            Self::AudioSpectrum => None,
            Self::AudioCorrelation => None,
            Self::LoudnessHistory => None,
            Self::Spectrogram => None,
        }
    }
}

/// Layout configuration for the floating scope window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ScopeLayout {
    OnePane,
    TwoPane,
    #[default]
    FourPane,
    NinePane,
}

impl ScopeLayout {
    pub fn pane_count(&self) -> usize {
        match self {
            Self::OnePane => 1,
            Self::TwoPane => 2,
            Self::FourPane => 4,
            Self::NinePane => 9,
        }
    }

    /// Grid dimensions (cols, rows) for this layout.
    pub fn grid(&self) -> (usize, usize) {
        match self {
            Self::OnePane => (1, 1),
            Self::TwoPane => (2, 1),
            Self::FourPane => (2, 2),
            Self::NinePane => (3, 3),
        }
    }
}

/// Waveform channel selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum WaveformChannel {
    #[default]
    Y,
    R,
    G,
    B,
}

impl WaveformChannel {
    pub fn label(&self) -> &'static str {
        match self { Self::Y => "Y", Self::R => "R", Self::G => "G", Self::B => "B" }
    }
}

/// Skin tone indicator angle (degrees) — standard vectorscope convention.
pub const SKIN_TONE_ANGLE_DEGREES: f32 = 123.0;

/// Histogram Y-axis scaling mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum HistogramScale {
    /// Square-root compression — emphasises low counts, default NLE style.
    #[default]
    Sqrt,
    /// Linear count — raw proportion, tall peaks dominate.
    Linear,
    /// Natural log — heaviest low-count emphasis, for spotting sparse outliers.
    Log,
}

/// Waveform trace base colour (monochrome mode only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum WaveformPaintMode {
    /// Classic green phosphor (default).
    #[default]
    Green,
    /// Warm yellow trace.
    Yellow,
    /// Neutral white trace.
    White,
    /// Per-channel RGB — R channel red, G channel green, B channel blue.
    Classic,
}

/// Per-pane scope settings — unified struct with all per-type fields.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaneScopeSettings {
    // ── Common ──
    pub waveform_channel: WaveformChannel,
    pub low_pass: bool,
    pub bloom: bool,
    pub quality: klipp_core::ScopeQuality,
    pub scale: ScopeScale,

    // ── Waveform-specific ──
    pub colorize: bool,
    pub extents: bool,
    pub waveform_brightness: f32,
    pub graticule_brightness: f32,
    pub show_reference_levels: bool,
    pub reference_low: f32,
    pub reference_high: f32,

    // ── Vectorscope-specific ──
    pub vectorscope_brightness: f32,
    pub vectorscope_graticule_brightness: f32,
    pub low_range: f32,
    pub high_range: f32,
    pub show_2x_zoom: bool,
    pub show_skin_tone_indicator: bool,
    pub show_graticule: bool,

    // ── Parade-specific ──
    pub overlay_on_viewer: bool,

    // ── Scope cursor (picker overlay) ──
    pub show_scope_cursor: bool,
    /// Half-size of the sample box (0 = 1×1 single pixel, 2 = 5×5, up to 7 = 15×15).
    pub scope_cursor_sample_radius: u32,

    // ── Additional reference / display options (serde-defaulted for backward compat) ──
    /// Vectorscope: draw the NTSC I/Q axes (I = skin tone, Q = perpendicular).
    #[serde(default)]
    pub show_iq_lines: bool,
    /// Histogram: Y-axis scaling mode (Sqrt/Linear/Log).
    #[serde(default)]
    pub histogram_scale: HistogramScale,
    /// Waveform: base trace colour in monochrome mode (Green/Yellow/White/Classic).
    #[serde(default)]
    pub waveform_paint_mode: WaveformPaintMode,
    /// Spectrogram: lower bound of the dynamic range in dBFS. Anything
    /// softer than this maps to the darkest colour. Default -72 matches
    /// the old hardcoded value; commonly-useful range is -48 (loud-only
    /// view) to -96 (full noise floor). Upper bound is always 0 dBFS.
    #[serde(default = "default_spectrogram_db_floor")]
    pub spectrogram_db_floor: f32,
}

fn default_spectrogram_db_floor() -> f32 { -72.0 }

impl Default for PaneScopeSettings {
    fn default() -> Self {
        Self {
            waveform_channel: WaveformChannel::Y,
            low_pass: false,
            bloom: true,
            quality: klipp_core::ScopeQuality::Medium,
            scale: ScopeScale::TenBit,
            // Waveform
            colorize: true,
            extents: false,
            waveform_brightness: 0.5,
            graticule_brightness: 0.5,
            show_reference_levels: false,
            reference_low: 0.0,
            reference_high: 1023.0,
            // Vectorscope
            vectorscope_brightness: 0.5,
            vectorscope_graticule_brightness: 0.5,
            low_range: 0.30,
            high_range: 0.70,
            show_2x_zoom: false,
            show_skin_tone_indicator: false,
            show_graticule: true,
            // Parade
            overlay_on_viewer: false,
            // Scope cursor
            show_scope_cursor: true,
            scope_cursor_sample_radius: 2,
            // Additional reference / display options
            show_iq_lines: false,
            histogram_scale: HistogramScale::Sqrt,
            waveform_paint_mode: WaveformPaintMode::Green,
            spectrogram_db_floor: default_spectrogram_db_floor(),
        }
    }
}

/// Compute tone map gain from waveform brightness setting (0.0–1.0).
pub fn compute_tone_map_gain(settings: &PaneScopeSettings) -> f32 {
    // Map 0.0–1.0 brightness to 0.3–3.0 gain range (log-ish)
    0.3 + settings.waveform_brightness * 2.7
}

/// Compute graticule alpha from brightness setting (0.0–1.0).
pub fn graticule_alpha(brightness: f32) -> f32 {
    // Always at least slightly visible, up to full brightness
    (0.05 + brightness * 0.95).clamp(0.05, 1.0)
}

/// Reference line position for a given value and panel height.
pub struct ReferenceLine {
    pub y: f32,
    pub value: f32,
}

/// Compute reference level line positions.
pub fn compute_reference_lines(settings: &PaneScopeSettings, height: f32) -> Vec<ReferenceLine> {
    if !settings.show_reference_levels {
        return Vec::new();
    }
    let max = settings.scale.max_val();
    let draw_h = height - 14.0 - 16.0; // PAD_TOP, PAD_BOTTOM
    let val_to_y = |val: f32| -> f32 {
        14.0 + (1.0 - val / max) * draw_h
    };
    vec![
        ReferenceLine { y: val_to_y(settings.reference_high), value: settings.reference_high },
        ReferenceLine { y: val_to_y(settings.reference_low), value: settings.reference_low },
    ]
}

/// Saturation ring for vectorscope.
pub struct SaturationRing {
    pub radius: f32,
    pub range_value: f32,
}

/// Compute vectorscope saturation ring radii.
pub fn compute_saturation_rings(settings: &PaneScopeSettings, diameter: f32) -> Vec<SaturationRing> {
    let max_r = diameter / 2.0 - 8.0;
    vec![
        SaturationRing { radius: settings.low_range * max_r, range_value: settings.low_range },
        SaturationRing { radius: settings.high_range * max_r, range_value: settings.high_range },
    ]
}

/// Configuration for a single pane in the floating scope window.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PaneConfig {
    pub scope_type: ScopeType,
    pub settings: PaneScopeSettings,
}

impl PaneConfig {
    pub fn new(scope_type: ScopeType) -> Self {
        Self { scope_type, settings: PaneScopeSettings::default() }
    }

    /// Default pane configurations for a given layout.
    pub fn default_for_layout(layout: ScopeLayout) -> Vec<PaneConfig> {
        let defaults = [
            ScopeType::Parade,
            ScopeType::Waveform,
            ScopeType::Vectorscope,
            ScopeType::Histogram,
            ScopeType::Waveform,
            ScopeType::Parade,
            ScopeType::Histogram,
            ScopeType::Vectorscope,
            ScopeType::Waveform,
        ];
        defaults[..layout.pane_count()].iter().map(|&t| PaneConfig::new(t)).collect()
    }
}

/// Waveform scale style (for ··· menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum WaveformScaleStyle {
    #[default]
    TenBit,
    Ire,
    Percent,
}

/// Vectorscope scale style (for ··· menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum VectorscopeScaleStyle {
    #[default]
    SeventyFive,
    Hundred,
}

impl VectorscopeScaleStyle {
    pub fn label(&self) -> &'static str {
        match self { Self::SeventyFive => "75% color bars", Self::Hundred => "100% color bars" }
    }
}

/// Persistent state for the floating scope window.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScopeWindowState {
    pub open: bool,
    pub layout: ScopeLayout,
    pub panes: Vec<PaneConfig>,
    pub global_low_pass: bool,
    pub global_quality: klipp_core::ScopeQuality,
    pub waveform_scale: WaveformScaleStyle,
    pub vectorscope_scale: VectorscopeScaleStyle,
    pub qualifier_focus: bool,
}

impl Default for ScopeWindowState {
    fn default() -> Self {
        let layout = ScopeLayout::FourPane;
        Self {
            open: false,
            layout,
            panes: PaneConfig::default_for_layout(layout),
            global_low_pass: false,
            global_quality: klipp_core::ScopeQuality::Medium,
            waveform_scale: WaveformScaleStyle::default(),
            vectorscope_scale: VectorscopeScaleStyle::default(),
            qualifier_focus: false,
        }
    }
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
            || self.quick_look_path.is_some()
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
            playhead: Timecode { value: 0, rate: 24 },
            fps: Fps::film_24(),
            playback_speed: 1.0,
            loop_playback_enabled: false,
            active_workspace: Workspace::default(),
            custom_workspaces: Vec::new(),
            show_save_workspace_dialog: false,
            save_workspace_name: String::new(),
            show_workspace_manager: false,
            workspace_manager_selected: None,
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
            timeline_resizing_track: None,
            timeline_track_pre_collapse: std::collections::HashMap::new(),
            playhead_follow: PlayheadFollow::default(),
            track_header_width: 180.0,
            viewer_zoom: 0.0, // 0.0 = fit-to-window mode
            last_timeline_content_width: 800.0,
            show_overview_bar: true,
            selected_clip: None,
            selected_transition: None,
            selected_clips: Vec::new(),
            hovered_clip: None,
            show_unsaved_dialog: false,
            unsaved_dialog_action: UnsavedAction::None,
            pending_media_path: None,
            thumbnail_cache: ThumbnailCache::new(),
            thumbnail_textures: std::collections::HashMap::new(),
            filmstrip_textures: std::collections::HashMap::new(),
            show_export_dialog: false,
            jog_wheel_state: crate::widgets::jog_wheel::JogWheelState::new(),
            show_preset_manager: false,
            preset_manager_selected: None,
            show_cache_manager: false,
            cache_categories: Vec::new(),
            show_clip_properties: false,
            clip_properties_media_id: None,
            clip_properties_tab: ClipPropertiesTab::default(),
            active_tool: Tool::Select,
            viewer_mode: ViewerMode::Program,
            snap_enabled: true,
            ripple_enabled: true,
            default_transition_frames: 24,
            timeline_prefs: Default::default(),
            viewer_prefs: Default::default(),
            snap_threshold_px: 10.0,
            zoom_step: 1.5,
            default_sequence_resolution_idx: 0,
            toast_duration_secs: 2.2,
            modal_backdrop_alpha: 120,
            scope_quality: Default::default(),
            scope_update_interval_ms: 32,
            edit_mode: klipp_timeline::magnetic::EditMode::default(),
            magnetic_timeline_enabled: true,
            multicam_show_angles: true,
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
            source_video_texture: None,
            selected_media_ids: std::collections::HashSet::new(),
            last_clicked_media_id: None,
            use_proxies: true,
            is_scrubbing: false,
            pre_scrub_use_proxies: true,
            pre_scrub_was_playing: false,
            showing_thumbnail_preview: false,
            awaiting_seek_frame: false,
            render_cache_ranges: Vec::new(),
            show_relink_dialog: false,
            offline_media_ids: Vec::new(),
            show_about_dialog: false,
            editing_timecode: false,
            timecode_input: String::new(),
            fullscreen_overlay_visible: false,
            fullscreen_overlay_timer: 0.0,
            dragging_media: None,
            skimming_enabled: true,
            file_browser_video_skim_enabled: true,
            file_browser_audio_skim_enabled: true,
            toast: crate::widgets::toast::ToastState::default(),
            drag_drop: crate::drag_drop::DragDropState::default(),
            deferred_texture_drops: [Vec::new(), Vec::new(), Vec::new()],
            deferred_texture_drops_slot: 0,
            browser_skim_texture: None,
            browser_skim_media_id: None,
            skim_transcoding: std::collections::HashSet::new(),
            skim_transcode_status: None,
            skim_transcode_done_at: None,
            browser_skim_pending: None,
            skim_decode_in_flight: false,
            skim_decode_started_at: None,
            skim_queued_request: None,
            browser_skim_active_this_frame: false,
            pending_skim_transcodes: Vec::new(),
            audio_skimming_enabled: true,
            skim_speed_px_per_sec: 0.0,
            skim_last_update: std::time::Instant::now(),
            skim_last_x: 0.0,
            skim_audio_session: None,
            system_audio_muted: false,
            system_audio_last_check: std::time::Instant::now(),
            system_audio_last_reminder: std::time::Instant::now(),
            mute_reminder_interval_secs: 15,
            files_panel_focused: false,
            quick_look_path: None,
            quick_look_texture: None,
            quick_look_info: None,
            quick_look_audio: None,
            show_recent_files: true,
            filmstrip_hover_enabled: true,
            filmstrip_quality: Default::default(),
            filmstrip_persist: false,
            file_browser_auto_refresh: true,
            recent_files_snapshot: Vec::new(),
            skim_last_audio_dispatch: std::time::Instant::now(),
            stabilization_in_progress: std::collections::HashSet::new(),
            stabilization_progress: std::collections::HashMap::new(),
            scene_detect_in_progress: std::collections::HashSet::new(),
            scene_detect_progress: std::collections::HashMap::new(),
            spectrum_ring: std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            spectrum_bins: vec![-96.0f32; 256],
            spectrum_sample_rate: 48000,
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
            perf_recording_active: false,
            pending_perf_recording_toggle: false,
            perf_last_session_path: None,
            perf_latest_sample: None,
            perf_averages: klipp_engine::perf::PerfAverages::default(),
            perf_drops_last_sec: 0,
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
            color_tools_position: ColorToolsPosition::Right,
            selected_node_id: None,
            show_node_strip: false,
            color_tab: ColorTab::default(),
            color_tab_bottom: ColorTab::default(),
            color_grade_drag_start: None,
            color_grade_clipboard: None,
            curves_single_mode: false,
            curve_histograms: None,
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
            scope_scale: ScopeScale::TenBit,
            scope_show_r: true,
            scope_show_g: true,
            scope_show_b: true,
            scope_low_pass: false,
            scope_bloom: true,
            scope_settings_open: false,
            scope_window: ScopeWindowState::default(),
            viewer_image_rect: None,
            viewer_panel_rect: egui::Rect::NOTHING,
            viewer_pointer_pos: None,
            scope_cursor_locked: false,
            scope_cursor_locked_pos: None,
            scope_cursor_locked_data: None,
            scope_cursor_sample_radius: 2,
            scope_cursor_show: true,
            eyedropper_mode: EyedropperMode::None,
            wb_sample_request: None,
            qualifier_sample_request: None,
            matte_preview_active: false,
            before_after_active: false,
            stabilization_compare_active: false,
            split_compare_effect: None,
            split_position: 0.5,
            grade_stills: Vec::new(),
            active_grade_still: None,
            grade_still_compare: false,
            color_match_reference: None,
            viewer_overlays: ViewerOverlays::default(),
            frame_count: 0,
            false_color_lut: crate::prefs::FalseColorPalette::broadcast().build_lut(),
            pending_false_color_palette: None,
            available_plugins: Vec::new(),
            ladspa_plugins: Vec::new(),
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
            filmstrip_progress: std::collections::HashMap::new(),
            playhead_anchor: Timecode::zero(),
            playhead_anchor_wall: std::time::Instant::now(),
            playhead_speed: 1.0,
            playhead_smooth_secs: 0.0,
            collapsed_sections: std::collections::HashSet::new(),
            remember_inspector_layout: true,
            audio_backend_ok: false,
            audio_sample_rate: 48000,
            is_recording: false,
            recording_track_id: None,
            recording_path: None,
            recording_stop_tx: None,
            recording_audio_input: None,
            track_peak_levels: Vec::new(),
            master_peak_left: 0.0,
            master_peak_right: 0.0,
            master_volume: 1.0,
            master_muted: false,
            lufs_momentary: f32::NEG_INFINITY,
            lufs_short_term: f32::NEG_INFINITY,
            lufs_integrated: f32::NEG_INFINITY,
            lufs_true_peak_dbfs: f32::NEG_INFINITY,
            lufs_lra: 0.0,
            lufs_short_term_history: {
                let mut v = std::collections::VecDeque::new();
                v.reserve(600);
                v
            },
            spectrum_history: {
                let mut v = std::collections::VecDeque::new();
                v.reserve(300);
                v
            },
            lufs_momentary_smooth: f32::NEG_INFINITY,
            lufs_short_term_smooth: f32::NEG_INFINITY,
            clip_indicator: false,
            mixer_editing_name: None,
            lufs_target: -23.0,
            peak_hold_time_secs: 3.0,
            confirm_remove_track: None,
            show_markers_panel: false,
            show_subtitle_editor: false,
            show_project_notes: false,
            tp_oversample: true,
            project_notes: String::new(),
            subtitle_editor: SubtitleEditorState::default(),
            consolidate_progress: None,
            consolidate_dest_path: String::new(),
            consolidate_include_proxies: false,
            consolidate_include_skims: false,
            consolidate_create_zip: false,
            consolidate_delete_folder_after_zip: false,
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
            show_batch_rename_dialog: false,
            batch_rename_ids: Vec::new(),
            batch_rename_mode: crate::app::BatchRenameMode::FindReplace,
            batch_rename_find: String::new(),
            batch_rename_replace: String::new(),
            batch_rename_match_case: true,
            batch_rename_prefix: String::new(),
            batch_rename_suffix: String::new(),
            batch_rename_start: 1,
            batch_rename_padding: 2,
            show_project_info: false,
            file_info: None,
            file_info_thumb: None,
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
            waveform_zoom: std::collections::HashMap::new(),
            audio_reference_clip_id: None,
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
            pending_title_overlay_clear: false,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_editor_state_default_is_inactive() {
        let s = SubtitleEditorState::default();
        assert!(s.editing_text.is_none());
        assert!(s.editing_timecode.is_none());
        assert!(s.text_buf.is_empty());
        assert!(s.timecode_buf.is_empty());
        assert!(!s.ripple);
    }

    #[test]
    fn consolidate_progress_channel_roundtrip() {
        // Sanity: the mpsc channel used by ConsolidateProgress can deliver
        // both Progress and Done messages, and try_recv returns them in order.
        let (tx, rx) = std::sync::mpsc::channel::<ConsolidateMessage>();
        tx.send(ConsolidateMessage::Progress {
            done: 1,
            total: 3,
            filename: "clip.mp4".into(),
        })
        .unwrap();
        tx.send(ConsolidateMessage::Done(Err("simulated".into())))
            .unwrap();
        drop(tx);

        let first = rx.recv().unwrap();
        match first {
            ConsolidateMessage::Progress { done, total, filename } => {
                assert_eq!(done, 1);
                assert_eq!(total, 3);
                assert_eq!(filename, "clip.mp4");
            }
            _ => panic!("expected Progress first"),
        }
        let second = rx.recv().unwrap();
        assert!(matches!(second, ConsolidateMessage::Done(Err(_))));
    }

    #[test]
    fn split_compare_state_round_trip() {
        // The compare-effect state is an Option<(ClipId, usize, bool)>. The
        // bool discriminates builtin (false) from OFX (true). Round-trip the
        // two cases plus the cleared state to guard against a field shape
        // change.
        let cid = klipp_core::ClipId::new();

        let mut slot: Option<(klipp_core::ClipId, usize, bool)> = None;
        assert!(slot.is_none());

        slot = Some((cid, 3, false));
        assert_eq!(slot, Some((cid, 3, false)));
        assert!(matches!(slot, Some((_, 3, false))));

        slot = Some((cid, 7, true));
        assert_eq!(slot, Some((cid, 7, true)));
        assert!(matches!(slot, Some((_, _, true))));

        slot = None;
        assert!(slot.is_none());
    }

    #[test]
    fn split_compare_clears_on_effect_remove() {
        // Behaviour mirrored from the RemoveBuiltinEffect / RemoveEffect
        // handlers in app.rs — on remove, the compare pointer is cleared
        // when it targeted the removed effect, or decremented when it
        // targeted a later one in the same clip, or left alone otherwise.
        fn adjust(
            slot: &mut Option<(klipp_core::ClipId, usize, bool)>,
            removed_clip: klipp_core::ClipId,
            removed_index: usize,
            removed_is_ofx: bool,
        ) {
            if let Some((cid, idx, is_ofx)) = *slot {
                if cid == removed_clip && is_ofx == removed_is_ofx {
                    if idx == removed_index {
                        *slot = None;
                    } else if idx > removed_index {
                        *slot = Some((cid, idx - 1, is_ofx));
                    }
                }
            }
        }

        let a = klipp_core::ClipId::new();
        let b = klipp_core::ClipId::new();

        // Case 1: compared effect is removed → cleared.
        let mut slot = Some((a, 2, false));
        adjust(&mut slot, a, 2, false);
        assert!(slot.is_none());

        // Case 2: earlier sibling removed → index shifts down.
        let mut slot = Some((a, 2, false));
        adjust(&mut slot, a, 0, false);
        assert_eq!(slot, Some((a, 1, false)));

        // Case 3: later sibling removed → index unchanged.
        let mut slot = Some((a, 2, false));
        adjust(&mut slot, a, 5, false);
        assert_eq!(slot, Some((a, 2, false)));

        // Case 4: different clip → untouched.
        let mut slot = Some((a, 2, false));
        adjust(&mut slot, b, 2, false);
        assert_eq!(slot, Some((a, 2, false)));

        // Case 5: different list (builtin vs OFX) → untouched.
        let mut slot = Some((a, 2, true));
        adjust(&mut slot, a, 2, false);
        assert_eq!(slot, Some((a, 2, true)));
    }

    // ── Workspace layout tests (#39) ──

    #[test]
    fn builtin_layouts_has_five_entries() {
        let layouts = builtin_workspace_layouts();
        assert_eq!(layouts.len(), 5);
    }

    #[test]
    fn builtin_layout_names_are_unique() {
        let layouts = builtin_workspace_layouts();
        let names: std::collections::HashSet<_> =
            layouts.iter().map(|l| l.name.clone()).collect();
        assert_eq!(names.len(), layouts.len());
    }

    #[test]
    fn builtin_layouts_include_expected_names() {
        let layouts = builtin_workspace_layouts();
        let names: std::collections::HashSet<_> =
            layouts.iter().map(|l| l.name.clone()).collect();
        for expected in ["Logging", "Editing", "Effects", "Audio", "Color"] {
            assert!(names.contains(expected), "missing built-in: {expected}");
        }
    }

    #[test]
    fn apply_layout_with_none_sizes_preserves_existing_size() {
        // Mirrors the apply path in app.rs: a None size must leave the
        // current pixel width untouched. Pure logic — no egui needed.
        let mut browser_width = 350.0f32;
        let layout_browser_width: Option<f32> = None;
        if let Some(w) = layout_browser_width {
            browser_width = w;
        }
        assert_eq!(browser_width, 350.0);

        // Sanity: Some _does_ overwrite.
        let layout_browser_width: Option<f32> = Some(420.0);
        if let Some(w) = layout_browser_width {
            browser_width = w;
        }
        assert_eq!(browser_width, 420.0);
    }

    #[test]
    fn color_layout_has_color_panel_height() {
        // The Color preset is the only built-in that uses the color
        // panel — guard that the dedicated size field is populated so
        // applying Color doesn't leave an unsized panel.
        let layouts = builtin_workspace_layouts();
        let color = layouts.iter().find(|l| l.name == "Color").unwrap();
        assert!(color.show_color_panel);
        assert!(color.color_panel_height.is_some());
    }
}
