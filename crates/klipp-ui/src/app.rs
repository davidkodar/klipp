use crate::history::{EditCommand, HistoryEntry, KeyframeSnapshot};
use crate::icons::Icon;
use crate::layout_state::LayoutState;
use crate::panels::markers::MarkersPanel;
use crate::browser::BrowserPanel;
use crate::inspector::NewInspectorPanel;
use crate::panels::preferences::PreferencesPanel;
use crate::panels::timeline::TimelinePanel;
use crate::panels::toolbar::ToolbarPanel;
use crate::panels::transport::TransportBar;
use crate::panels::viewer::ViewerPanel;
use crate::state::{AppState, UnsavedAction};
use crate::theme::{self, KlippTheme};
use crate::widgets::resizer;
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{
    AppCommand, AppEvent, ClipId, GroupId, ExportAudioCodec, ExportFormat, ExportPreset,
    EncoderPreset, ExportPresetSettings, ExportSettings, ExportVideoCodec, MarkerId, MediaId,
    ParamChange, Timecode, Tool, ViewerMode, builtin_presets,
};
use klipp_engine::{PlaybackState, SharedFrameBuffer};

mod export;
mod export_banding;
mod frame_glide;
mod track_subject;
mod project_io;
mod unsaved;
#[cfg(test)]
mod made_tracks_tests;
#[cfg(test)]
mod sound_source_tests;
mod appearance;
mod tips;
mod perf;
mod shutdown;
mod multicam;
pub(crate) mod placement;
mod history_apply;
pub(crate) mod aspect_version;
mod brand_kit;
mod audio_roles;
mod visual_search;
mod recording;
mod automation;
mod ai_tool_runs;
mod multi_clip_edit;
mod caches;
mod engine_sync;
mod viewer_textures;
mod startup;
mod shortcuts;
mod frame_loop;
mod colour_workspace;
mod caption_translation;
mod delivery_check;
mod main_layout;
mod dialogs;
mod control_bridge;
mod proxies;
mod audio_jobs;
mod media_jobs;
mod engine_events;
mod project_commands;
mod markers;
mod transitions;
mod playback;
mod stabilization;
mod effects;
mod inspector_edit_host;
mod command_writes;
mod event_writes;
mod graph_dock;
pub(crate) mod colour_target;
mod music_trim;
#[cfg(test)]
mod music_trim_tests;
#[cfg(test)]
mod colour_target_tests;
#[cfg(test)]
mod colour_tab_tests;
pub(crate) mod node_labels;
pub(crate) mod node_thumbnails;
#[cfg(test)]
mod inspector_parity;
#[cfg(test)]
mod parity_manifest;
#[cfg(test)]
mod parity_controls;
#[cfg(test)]
mod parity_stack_switches;
#[cfg(test)]
mod parity_colour_pickers;
#[cfg(test)]
mod parity_grade_specialists;
#[cfg(test)]
mod parity_grade_workflows;
#[cfg(test)]
mod parity_curve_editor;
#[cfg(test)]
mod parity_harness;
#[cfg(test)]
mod parity_numeric;
#[cfg(test)]
mod parity_effect_keys;
#[cfg(test)]
mod parity_video_transform;
#[cfg(test)]
mod parity_pixels;
#[cfg(test)]
mod parity_keyed_pixels;
#[cfg(test)]
mod parity_specialist_pixels;
#[cfg(test)]
mod parity_masked_pixels;
#[cfg(test)]
mod parity_flip;
#[cfg(test)]
mod parity_grade_bypass;
#[cfg(test)]
mod parity_compound_body;
#[cfg(test)]
mod parity_grade_editors;
#[cfg(test)]
mod parity_grade_keys;
#[cfg(test)]
mod parity_grade_numeric;
#[cfg(test)]
mod parity_grade_wheels;
#[cfg(test)]
mod parity_specialists;
#[cfg(test)]
mod side_matte_cards;
#[cfg(test)]
mod placement_row_tests;
#[cfg(test)]
mod grade_inspector_tests;
#[cfg(test)]
mod grade_wheel_keys;
#[cfg(test)]
mod curve_editor_grade_tracks;
mod media;
mod compound;
mod clips;
#[cfg(test)]
mod picture_truth_rebuild_tests;
#[cfg(test)]
mod multicam_piece_tests;
mod timeline_edit;
mod surfaces;
mod water;
mod colour;
mod audio;
mod keyframes;
mod graph_ops;
mod path_commands;
mod titles;
mod text_tool;
mod first_time_help;
mod title_handles;
mod node_edits;
#[cfg(test)]
mod title_library_tests;
#[cfg(test)]
mod viewer_harness;
#[cfg(test)]
mod mask_tool_tests;
#[cfg(test)]
mod keymap_tests;
#[cfg(test)]
mod small_window_tests;
#[cfg(test)]
mod mask_key_lanes_tests;
#[cfg(test)]
mod key_selection_tests;
#[cfg(test)]
mod timeline_snap_tests;
#[cfg(test)]
mod timeline_stretch_tests;
#[cfg(test)]
mod qualifier_3d_tests;
#[cfg(test)]
mod wipe_tests;
mod poster_frames;
mod links;
mod prefs_apply;
mod timeline_grade;
mod grade_edits;
mod tracking;
mod linked_audio;
mod camera;
mod transition_keys;
mod transcription;
mod locks;
mod storyline;
mod clip_moves;
mod edit_points;
mod source_edits;
mod edit_helper_commands;
mod subtitles;
mod render_script;
mod batch_rename;
mod whoosh;
mod hyperzoom;
use export::*;
use poster_frames::*;
use links::*;
use prefs_apply::*;
use timeline_grade::*;
use grade_edits::*;
use tracking::*;
pub(crate) use tracking::{surface_area_on_frame, surface_on_frame};
use linked_audio::*;

/// A pool clip dropped on `track` at `at` as the app places it, without
/// its sound, for tests outside this module.
#[cfg(test)]
pub(crate) fn media_drop_for_tests(seq: &klipp_timeline::Sequence, clip: klipp_timeline::Clip, track: usize, at: klipp_core::Timecode, overwrite: bool) -> Result<(Vec<klipp_document::history::EditCommand>, klipp_core::ClipId), &'static str> {
    let mode = if overwrite { crate::clip_clipboard::PasteMode::Overwrite } else { crate::clip_clipboard::PasteMode::Insert };
    placement::media_drop_edits(seq, clip, track, None, at, mode, "Drop")
}

/// A clip's own sound linked under it, for tests outside this module.
#[cfg(test)]
pub(crate) fn linked_audio_for_tests(seq: &mut klipp_timeline::Sequence, clip: klipp_core::ClipId) {
    attach_linked_audio_to_clip(seq, clip, None).expect("its sound linked under it");
}
use camera::*;
use subtitles::*;
use render_script::*;
pub use batch_rename::*;
use whoosh::*;
use hyperzoom::*;
use caches::*;
use multi_clip_edit::*;
use ai_tool_runs::*;
use media::*;
use project_io::*;
use compound::*;
use colour::*;
use proxies::*;
use audio::*;
use colour_workspace::*;
pub use media::detect_image_sequence;
pub(crate) use project_io::{autosave_path_for, is_autosave_path, project_for_autosave, has_newer_autosave, crash_notice, iso8601_now};
#[cfg(test)]
pub(crate) use project_io::retire_autosave;
use titles::*;
pub(crate) use titles::apply_title_preset;
pub(crate) use viewer_textures::alloc_texture_logged;
use project_io::LockDecision;

/// Why a grade holding a correction's mix or mask channel (M3 step 17a)
/// isn't copied: a copy takes the grade's fields, which hold neither, so
/// it would arrive with the correction at full strength.
const GRADE_MIX_NOT_COPIED: &str = "A grade with a correction's mix or mask channel can't be copied yet: set them back to whole and red first";

/// Default colour for freshly-created sticky notes — warm beige
/// `#d4c89a`. Single value in v1; a future right-click recolour
/// menu would route through SetStickyNoteColor (not yet shipped).
/// Stored packed RGBA u32 (little-endian: r, g, b, a).
pub(crate) const STICKY_DEFAULT_COLOR_RGBA: u32 = u32::from_le_bytes([0xd4, 0xc8, 0x9a, 0xff]);

use crossbeam_queue::ArrayQueue;
use klipp_media::{MediaItem, ThumbnailEntry};
use klipp_timeline::{Marker, Sequence};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

struct ThumbnailResult {
    media_id: MediaId,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

/// First-frame decode result for the Source Viewer. The id lets the UI
/// drop the result if the user has since loaded a different clip.
struct SourceFrameResult {
    media_id: MediaId,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

enum FilmstripResult {
    /// A single frame generated incrementally
    Frame {
        media_id: MediaId,
        frame: klipp_media::FilmstripFrame,
    },
    /// Generation complete for this media
    Done {
        media_id: MediaId,
    },
    /// The disk cache for this media could not be read (and has been
    /// removed); the filmstrip is to be generated afresh.
    CacheUnreadable {
        media_id: MediaId,
    },
}

enum AudioConformResult {
    Complete {
        media_id: MediaId,
        entry: klipp_media::AudioCacheEntry,
    },
    Failed {
        media_id: MediaId,
        error: String,
    },
}

/// Runs one control request, as JSON, in this process and answers it:
/// `klipp_control::respond_local` over the app's own control context.
pub type LocalControl = std::sync::Arc<dyn Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>;

/// A project being copied to another filesystem by Save As (#1864).
struct ProjectCopy {
    from: PathBuf,
    to: PathBuf,
    done: std::sync::mpsc::Receiver<std::io::Result<()>>,
    /// Bytes copied so far, of `total`.
    copied: std::sync::Arc<std::sync::atomic::AtomicU64>,
    total: u64,
    /// When progress was last shown.
    shown_at: std::time::Instant,
}

pub struct KlippApp {
    /// The playback engine's channel, taking only what it acts on: a UI
    /// command sent there did nothing, silently (#437).
    cmd_tx: klipp_engine::commands::EngineSender,
    event_rx: Receiver<AppEvent>,
    /// Commands from the control socket (klipp-control / klipp-mcp).
    ///
    /// Drained into `send_commands` every frame, so a remote command
    /// takes exactly the route a button click does. That matters: many
    /// `AppCommand`s — colour, clip, media — are handled *here* by the
    /// UI's routers and never reach the engine's channel, so a control
    /// server that wrote to the engine directly saw them silently vanish.
    control_inbox: Option<Receiver<klipp_document::control::ControlEnvelope>>,
    frame_buffer: SharedFrameBuffer,
    /// Source-viewer frame buffer — engine writes decoded source
    /// media frames here during source playback / seek; the source
    /// viewer panel uploads from it via the same defer-drop pattern
    /// the program viewer uses.
    pub(crate) source_frame_buffer: SharedFrameBuffer,
    sequence: Arc<klipp_timeline::SharedSequence>,
    media_pool_shared: Arc<RwLock<klipp_media::MediaPool>>,
    state: AppState,
    toolbar: ToolbarPanel,
    updates: crate::updates::Updates,
    /// When `dirty` was last settled into `unsaved` (#2626).
    unsaved_settled_at: Option<std::time::Instant>,
    viewer: ViewerPanel,
    title_renderer: klipp_title::TitleRenderer,
    markers: MarkersPanel,
    edit_list: crate::panels::edit_list::EditListPanel,
    transcript_panel: crate::panels::transcript::TranscriptPanel,
    /// Transcripts read for the Transcript panel and the GUI's document
    /// ops; kept until the analyses change.
    transcript_source: crate::transcript::TranscriptSource,
    /// The Colour Check window's own state: the last reading of the
    /// displayed frame, and the last render-path self-check.
    colour_check: crate::panels::colour_check::ColourCheckPanel,
    /// Answers a control request in this process (#897): the app's own
    /// buttons for the measured colour tools run the code an assistant's
    /// request runs. Set by the binary; `None` in tests.
    local_control: Option<LocalControl>,
    /// The running colour tool's answer, when it comes.
    colour_tool_reply: Option<std::sync::mpsc::Receiver<Result<serde_json::Value, String>>>,
    /// The Delivery Check window's own state (#1978).
    delivery_check: crate::panels::delivery_check::DeliveryCheckPanel,
    /// The answer of the request the Delivery Check window sent, and
    /// whether it was the check itself.
    delivery_reply: Option<(bool, std::sync::mpsc::Receiver<Result<serde_json::Value, String>>)>,
    /// The caption translation running (#2213), its answer to come.
    translation: Option<caption_translation::Pending>,
    /// The Sound Over Time window's own state: the reading in hand or
    /// the thread still making it.
    sound_over_time: crate::panels::sound_over_time::SoundOverTimePanel,
    narrate: crate::panels::narrate::NarratePanel,
    preferences_panel: PreferencesPanel,
    transport: TransportBar,
    timeline: TimelinePanel,
    thumb_tx: Sender<ThumbnailResult>,
    thumb_rx: Receiver<ThumbnailResult>,
    source_frame_tx: Sender<SourceFrameResult>,
    source_frame_rx: Receiver<SourceFrameResult>,
    filmstrip_tx: Sender<FilmstripResult>,
    /// Filmstrips being read back from the disk cache. They skip the
    /// queue and its "generating" flag, which is what keeps the UI
    /// repainting while frames arrive, so after a project opened their
    /// frames sat in the channel and the timeline showed flat bars until
    /// something else redrew it.
    filmstrip_cache_loads: std::collections::HashSet<MediaId>,
    /// The multicam angle grid's decoder and tiles, made the first time
    /// the grid is shown.
    multicam_grid: Option<crate::multicam_grid::GridRuntime>,
    /// An Inspector change on one of several selected clips, being made
    /// on the others too (#843): how they were when it began.
    multi_edit: Option<MultiEditSession>,
    /// The interface size last applied, and the screen width it was
    /// worked out for, so it is set once per change rather than every
    /// frame (and never fights a zoom made another way).
    ui_scale_applied: Option<(u32, u32, u32)>,
    /// The files a filmstrip refinement pass is running for; see
    /// `AppCommand::RefineFilmstrip`.
    filmstrip_refining: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<MediaId>>>,
    filmstrip_rx: Receiver<FilmstripResult>,
    /// Hand-picked relinks checked off the UI thread: the file picked,
    /// the name the clip had, and what the check found.
    relink_check_tx: Sender<(String, PathBuf, klipp_media::relink::PickVerdict)>,
    relink_check_rx: Receiver<(String, PathBuf, klipp_media::relink::PickVerdict)>,
    audio_conform_tx: Sender<AudioConformResult>,
    audio_conform_rx: Receiver<AudioConformResult>,
    /// Track proxy ffmpeg child PIDs so we can kill them on exit
    proxy_children: Vec<Arc<std::sync::atomic::AtomicU32>>,
    /// Channel for proxy generation events (started/complete/failed)
    proxy_event_tx: Sender<AppEvent>,
    /// Normalize measurements under way (#2778): the clip, the effect's
    /// place and what it is measured for, so each runs once.
    normalize_measuring: std::collections::HashSet<(klipp_core::ClipId, usize, u64)>,
    /// Water measurements running (#2795), by clip and footprint.
    water_measuring: std::collections::HashSet<(klipp_core::ClipId, u32)>,
    /// The thread that makes skim frames, from decoders it keeps open
    /// per skimmed file. Both the media browser's hover and the timeline's
    /// skimmer ask it.
    skim_worker: klipp_engine::skim::SkimWorker<(MediaId, klipp_core::ViewerMode)>,
    proxy_event_rx: Receiver<AppEvent>,
    // Export dialog state
    export_dialog: ExportDialogState,
    // Preferences
    prefs: crate::prefs::Preferences,
    prefs_dirty: bool,
    // Keyboard shortcut bindings
    key_bindings: crate::keybindings::KeyBindings,
    prefs_last_save: std::time::Instant,
    // Proxy progress channel: (media_id, progress 0.0..1.0)
    proxy_progress_tx: Sender<(MediaId, f32)>,
    proxy_progress_rx: Receiver<(MediaId, f32)>,
    // Auto-save
    last_autosave: std::time::Instant,
    /// An Untitled project's template media, held in the cache until the
    /// project is first written (#1864).
    untitled_staging: Option<PathBuf>,
    /// A project written under "Untitled" on its first edit: an explicit
    /// save asks for its name and place, and moves it there.
    unnamed_project: Option<PathBuf>,
    /// The projects root, worked out for the preference it came from
    /// (#1864): it looks at the chosen folder, so not every frame.
    projects_root: Option<(Option<PathBuf>, PathBuf)>,
    /// A Save As copying the project to another filesystem, on its own
    /// thread.
    project_copy: Option<ProjectCopy>,
    /// The catalogue fetches finished when the titles were last drawn:
    /// a face that arrives since redraws them (#2734).
    fonts_seen: u64,
    /// Result channel for the in-flight background auto-save.
    ///
    /// `Some` from the moment a writer thread is spawned until the UI
    /// reaps its result, so it doubles as the "only one at a time"
    /// guard — a slow disk must not queue up a save per interval.
    autosave_rx: Option<Receiver<Result<std::path::PathBuf, String>>>,
    // Main-thread liveness — written to klipp-debug.log every 30 s so an
    // idle freeze is visible as a gap in the log timeline. Paired with the
    // panic hook in klipp-app/src/main.rs, every freeze or crash now leaves
    // a trace.
    last_heartbeat: std::time::Instant,
    frame_count: u64,
    last_heartbeat_frame: u64,
    /// Input events winit delivered since the last heartbeat, and the
    /// most that arrived in one frame. A frame that comes after seconds
    /// of silence carrying dozens of events means input was queued while
    /// nothing painted; one carrying none means the compositor delivered
    /// nothing. The two need different fixes, and a log that shows only
    /// fps cannot tell them apart — 2026-09-14, "the app is not
    /// responding" with the UI thread idle in the event loop.
    hb_input_events: u64,
    hb_max_batch: usize,
    /// Longest gap between two `update()` calls in the current
    /// heartbeat interval, and the longest time spent *inside* one.
    ///
    /// Together they say where a freeze lives. A 94-second gap with a
    /// 3 ms update means the thread was not in our code at all — it was
    /// in eframe's paint-and-present, which on Wayland blocks inside
    /// `Surface::get_current_texture` when the compositor is not
    /// returning buffers for an occluded window. A long update means
    /// the opposite, and `ui_stage` then says where.
    /// Playhead position the Colour workspace last followed, so the
    /// follow fires on movement rather than every frame.
    last_graded_playhead: Option<klipp_core::Timecode>,
    /// Where the playhead last sat inside each clip on the Colour
    /// page, so selecting a shot again returns to the frame it was
    /// graded on rather than its first frame.
    graded_positions: std::collections::HashMap<klipp_core::ClipId, klipp_core::Timecode>,
    /// "Go to it" clicked in the Colour page's header (#2952), taken
    /// at the next frame's follow.
    colour_go_to: Option<klipp_core::ClipId>,
    /// Browser visibility before a workspace borrowed its width,
    /// restored on leaving.
    ///
    /// Cleared if the browser comes back up while the borrowing
    /// workspace is still active — by the rail, a menu, Reveal in
    /// Browser, any route — because an explicit ask outranks the
    /// borrow, and restoring over it would undo the user's choice
    /// without telling them.
    browser_before_workspace: Option<bool>,
    /// Whether the graph dock was open before the Composite page opened it
    /// (#2758), put back on leaving.
    dock_before_composite: Option<bool>,
    node_browser_borrow: crate::layout_state::NodeBrowserBorrow,
    nodes_were_on_right: bool,
    /// The workspace `reconcile_workspace` last saw.
    ///
    /// Switching is not funnelled through one command: the toolbar
    /// tabs and the View menu assign `active_workspace` directly, and
    /// only the `C` shortcut went through `ToggleColorPanel`. So the
    /// enter/leave bookkeeping watches the field instead of trusting
    /// the callers, and every route gets the same treatment.
    last_workspace: crate::state::Workspace,
    hb_max_gap: std::time::Duration,
    /// The unsaved-changes dialog's Save is waiting on its Save As picker.
    unsaved_save_waiting: bool,
    hb_max_update: std::time::Duration,
    /// When the last `update()` returned, for the gap above.
    last_update_end: std::time::Instant,
    last_frame_at: std::time::Instant,
    /// Throttle for `refresh_scope_preview_fast`. The UI-thread scope
    /// preview re-grade is ~10 ms at 1080p/4 on a simple grade, which
    /// saturates the UI thread when a slider emits 60–120 SetNodeParam
    /// per second. Capping at ~33 Hz keeps scopes feeling live while
    /// leaving 2/3 of the UI-thread budget for everything else.
    last_scope_preview_refresh: std::time::Instant,
    /// Remember the last scope mode we synced to the engine so we don't
    /// re-send `SetScopeRenderMode` every frame (which was flooding the
    /// engine command channel at UI fps rate — ~120 commands/sec of
    /// no-op work).
    last_sent_scope_mode: Option<u8>,
    /// Last `SetActiveScopeModes` payload sent to the engine. Tracks the
    /// union of (main dock active mode) ∪ (each floating-window pane's
    /// video scope mode). Re-sent only when the union changes —
    /// floating-window open/close, layout swap, or per-pane mode picks.
    last_sent_active_scope_modes: Option<std::collections::BTreeSet<u8>>,
    /// Last `SetScopeSizes` payload sent to the engine: the physical size
    /// each scope mode was last shown at.
    last_sent_scope_sizes: Option<std::collections::BTreeMap<u8, (u32, u32)>>,
    /// Last `SetScopeSettings` payload sent to the engine. Tracks
    /// the union of low-pass / quality / R-G-B toggles. Re-sent only on
    /// change so settings-popover idle traffic doesn't churn the engine.
    last_sent_scope_settings: Option<klipp_core::ScopeSettings>,
    /// Last `SetScopeBloom` value sent to the engine. Tracks the scope
    /// bloom toggle so the publish fires only on change.
    last_sent_scope_bloom: Option<bool>,
    /// Last `SetScopeLowPass` value sent to the engine.
    last_sent_scope_low_pass: Option<bool>,
    /// Where the scopes read, last told the engine: after the viewing LUT
    /// at this path, or before it (#466); with the key it was sent with.
    last_sent_scope_lut: Option<(Option<PathBuf>, u64)>,
    /// Same change-tracking pattern as `last_sent_scope_mode` for the
    /// scope-panel visibility flag. The engine uses this to skip the
    /// per-frame `linear_rgba_to_u8` re-encode and the GPU scope
    /// shader pass when the panel is hidden — together those cost
    /// ~12 ms / 1080p frame and were eating most of the playback
    /// budget for users who don't have scopes open.
    last_sent_scopes_visible: Option<bool>,
    // GUI redesign
    theme: KlippTheme,
    /// Loaded once at startup; holds built-in (Dark, Light) themes
    /// plus any TOML the user dropped in `~/.config/klipp/themes/`.
    /// `theme` itself is rebuilt from this on every prefs reload.
    theme_registry: crate::theme::ThemeRegistry,
    layout: LayoutState,
    layout_dirty: bool,
    browser: BrowserPanel,
    new_inspector: NewInspectorPanel,
    /// The Inspector's Keyframes column, drawn beside its rows (#2305).
    key_column: crate::inspector::key_column::KeyColumn,
    new_timeline: crate::timeline::NewTimelinePanel,
    // Skim audio — shared with engine's cpal callback
    skim_consumer: klipp_engine::audio_output::SkimConsumer,
    /// cpal device sample rate (for skim audio resampling)
    cpal_sample_rate: u32,
    last_window_title: String,
    // Launch screen
    launch_screen: crate::panels::launch_screen::LaunchScreen,
    // New Project dialog
    new_project_dialog: crate::panels::new_project_dialog::NewProjectDialog,
    /// A project (and media) to open once the UI is running: from the
    /// command line, the open-last-project preference, or a dropped
    /// project folder. Stepped each frame through the UI's dispatcher.
    startup_open: Option<crate::cli::StartupOpen>,
    /// The error of the last project open that failed, for
    /// [`Self::startup_open`] to report and stop waiting on.
    last_open_error: Option<String>,
    relink_panel: crate::panels::relink::RelinkPanel,
    /// Who this session is, in the lock files it writes.
    lock_holder: klipp_project::lock::Holder,
    /// The lock this session holds on the open project, kept alive by
    /// its own heartbeat thread.
    lock_keeper: Option<klipp_project::lock::Keeper>,
    /// The project path the lock was last reconciled against.
    lock_seen_path: Option<PathBuf>,
    /// The open project, when it is open read-only because someone else
    /// holds its lock. Saving to it is refused.
    read_only_project: Option<PathBuf>,
    /// A lock found on a project being opened, waiting for the user.
    lock_prompt: Option<(PathBuf, klipp_project::lock::LockInfo, klipp_project::lock::Liveness)>,
    /// What the user chose for a locked project; the open that follows
    /// acts on it instead of asking again.
    lock_decision: Option<(PathBuf, LockDecision)>,
    /// "Sequence Settings…" modal — `Some` when open. Carries the per-
    /// invocation editor state (the targeted sequence index + its current
    /// width/height/fps being edited). `None` when closed.
    sequence_settings_dialog: Option<crate::panels::sequence_settings_dialog::SequenceSettingsDialog>,
    /// "Change Project Frame Rate?" modal. `Some` when the user is being
    /// asked whether to adopt an imported clip's fps; holds the deferred
    /// append/import command that will be re-dispatched on confirm.
    framerate_mismatch_dialog: Option<crate::panels::framerate_mismatch_dialog::FrameRateMismatchDialog>,
    /// One-shot bypass for the fps-mismatch prompt. Set after the user has
    /// already answered the dialog so the re-dispatched append does not
    /// re-open it. Cleared the moment the next append handler runs.
    suppress_fps_prompt: bool,
    // Node graph and inspector panels (Color workspace)
    node_graph_v2_panel: crate::panels::node_graph_v2::NodeGraphV2Panel,
    node_inspector_panel: crate::panels::node_inspector::NodeInspectorPanel,
    looks_panel_state: crate::panels::looks::LooksPanelState,
    grade_library_panel_state: crate::panels::grade_library::GradeLibraryPanelState,
    /// Pre-edit snapshot for the current color-grading parameter gesture.
    /// Set by `BeginNodeParamEdit`, consumed by `EndNodeParamEdit`. `None`
    /// when no gesture is in progress.
    node_param_edit_snapshot: Option<(klipp_core::ClipId, klipp_nodes::NodeGraph)>,
    /// The selected clip's node graph as it was before the press, key or
    /// text that may start a gesture, kept while the pointer is held. The
    /// grade panels write a value into the live graph as they draw it, a
    /// moment before `BeginNodeParamEdit` is handled; the gesture's
    /// snapshot comes from here so it holds the value from before.
    graph_before_input: Option<(klipp_core::ClipId, klipp_nodes::NodeGraph)>,
    /// The default graph the timeline grade's stand-in shows while the
    /// sequence has no grade of its own; see
    /// [`stand_in_grade_on`] and [`stand_down_timeline_grade_on`].
    timeline_grade_fresh: Option<(klipp_core::ClipId, klipp_nodes::NodeGraph)>,
    /// Last wall time `sync_and_refresh_frame` ran while a
    /// node-param-edit gesture was in flight. Used to throttle the
    /// heavy sequence resync to ~30 Hz during drags so the engine
    /// command channel doesn't get flooded.
    last_gesture_sync: std::time::Instant,
    /// CPU-only graph executor for UI-thread re-grading (instant slider feedback).
    /// CPU-only executor for fast reduced-resolution scope previews.
    scope_executor: klipp_nodes::GraphExecutor,
    /// File watcher for external media modification detection.
    /// Must be kept alive — dropping stops watching.
    #[allow(dead_code)]
    media_watcher: Option<notify::RecommendedWatcher>,
    /// Watches Omarchy's theme state while `follow_system_theme` is
    /// on; `None` otherwise, so a switch in Omarchy cannot reach the
    /// UI when the user has opted out. See `theme/omarchy.rs`.
    system_theme_watcher: Option<notify::RecommendedWatcher>,
    /// Set by the watcher thread, cleared by `sync_system_theme`.
    system_theme_changed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The desktop portal's appearance settings, read and followed on a
    /// thread of their own (`theme/portal.rs`).
    desktop_appearance: crate::theme::portal::PortalWatch,
    // Curve editor panel (spline editor for keyframe animation)
    curve_editor: crate::panels::curve_editor::CurveEditorPanel,
    // Floating scope window runtime (textures, UI state)
    scope_window_runtime: crate::widgets::scope_window::ScopeWindowRuntime,
    /// Textures and per-pane UI state for the docked scopes, separate
    /// from the floating window's so the two never share a slot.
    scope_dock_runtime: crate::widgets::scope_window::ScopeWindowRuntime,
    /// Shared per-frame perf telemetry from the engine. Painted by
    /// the performance overlay and snapshotted to JSON when the user
    /// clicks "Record Performance".
    perf_ring: klipp_engine::perf::SharedPerfRing,
    /// Lock-free queue of colour-grade parameter changes sent to the engine.
    /// Populated by the `SetNodeParam` handler at slider-scrub rate (60-120 Hz);
    /// drained by the engine at the start of each frame. Bypasses the
    /// `Arc<klipp_timeline::SharedSequence>` write lock / `flatten_sequence` rebuild path
    /// that `sync_sequence_to_engine` takes — which would otherwise contend
    /// with the engine's long read lock during active grading.
    param_change_queue: Arc<ArrayQueue<ParamChange>>,
    /// Shared atomic foreground-activity signal — engine writes
    /// `is_playing`, UI bumps `touch_input` on every input event,
    /// background `JobScheduler` reads to decide pause/resume.
    foreground_activity: Arc<klipp_jobs::ForegroundActivity>,
    /// Background job scheduler — pauses filmstrip / skim transcode /
    /// audio conform jobs whenever the user is busy. Lifetime tied
    /// to the app — dropped on exit; surviving subprocess jobs are
    /// killed by their `cancel()` impl on Drop.
    job_scheduler: Arc<klipp_jobs::JobScheduler>,
}

impl KlippApp {
    /// Tell the parts of split takes in the folder `path` is in: every
    /// clip there is placed in its run, or taken out of one it no longer
    /// belongs to.
    fn refresh_spans_beside(&mut self, path: &std::path::Path) {
        let dir = path.parent().map(std::path::Path::to_path_buf);
        let here: Vec<klipp_media::MediaItem> = self
            .state
            .media_pool
            .all()
            .filter(|m| matches!(m.kind, klipp_media::MediaItemKind::File) && m.path.parent().map(std::path::Path::to_path_buf) == dir)
            .cloned()
            .collect();
        let refs: Vec<&klipp_media::MediaItem> = here.iter().collect();
        let groups = klipp_media::card::spanned_groups(&refs);
        let mut spans: std::collections::HashMap<klipp_core::MediaId, klipp_core::ClipSpan> = std::collections::HashMap::new();
        for group in &groups {
            for (i, id) in group.iter().enumerate() {
                spans.insert(*id, klipp_core::ClipSpan { first: group[0], index: i as u32 + 1, count: group.len() as u32 });
            }
        }
        for item in here {
            let span = spans.get(&item.id).copied();
            if let Some(m) = self.state.media_pool.get_mut(&item.id) {
                if m.span != span {
                    m.span = span;
                    self.state.dirty = true;
                }
            }
        }
        if !groups.is_empty() {
            tracing::info!("{} split take(s) beside {:?}", groups.len(), path.file_name().unwrap_or_default());
        }
    }

}

/// A window under this size, in points at the system scale, is small
/// (#2944): the interface steps down and the defaults keep the timeline's
/// room.
pub(crate) const SMALL_WINDOW: egui::Vec2 = egui::vec2(1600.0, 900.0);

/// The Auto interface size for a window `size` points at the system scale
/// (#2944): 125% from 2800 wide; the system's own from 1600×900; 90% below
/// that, and 80% below 1440×810, never less, so the editor fits a small
/// window the way it fits a full HD one.
pub(crate) fn auto_zoom_factor(size: egui::Vec2) -> f32 {
    if size.x >= 2800.0 {
        1.25
    } else if size.x < 1440.0 || size.y < 810.0 {
        0.8
    } else if size.x < SMALL_WINDOW.x || size.y < SMALL_WINDOW.y {
        0.9
    } else {
        1.0
    }
}

impl eframe::App for KlippApp {
    /// Called by eframe right before the window closes — wgpu is still
    /// alive at this point. AppState drops AFTER this returns, by which
    /// time wgpu has already started tearing down; any GPU-resource
    /// drop ordered after that races with the device shutdown and
    /// SIGSEGVs. Drop everything that owns GPU resources HERE while
    /// the device is still healthy.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::ON_EXIT);
        // Use eprintln! alongside tracing — when SIGSEGV strikes
        // before tracing-subscriber flushes the file buffer, the
        // last few `info!()` lines disappear from klipp-debug.log.
        // eprintln writes to stderr unbuffered.
        eprintln!("on_exit ENTER — beginning final GPU-resource cleanup");
        tracing::info!("on_exit ENTER — beginning final GPU-resource cleanup");
        let t0 = std::time::Instant::now();
        // The wgpu-side cleanup (free_texture from the renderer, drop
        // gpu_texture_view) already ran during `pre_close_gpu_cleanup`
        // in the last update() — eframe doesn't pass us the wgpu
        // render state in on_exit. Here we just mop up any
        // egui::TextureHandle fields and texture caches that don't
        // need renderer access.
        // Cancel any in-flight AI inference so the worker thread
        // sees the cancel signal before its sender disconnects.
        if let crate::ai_tools::InferenceState::Running { handle, .. } =
            &self.state.ai_tools_state.inference
        {
            handle
                .cancel
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.state.ai_tools_state.mask_overlay_texture = None;
        self.state.ai_tools_state.inference = crate::ai_tools::InferenceState::Idle;

        // Drop every Option<TextureHandle> on AppState. Each one
        // owns wgpu resources that need to free while the device
        // is still healthy.
        self.state.source_live_texture = None;
        self.state.browser_skim_texture = None;
        self.state.quick_look_texture = None;
        self.state.file_info_thumb = None;

        // Drain texture caches.
        self.state.thumbnail_textures.clear();
        self.state.filmstrip_textures.clear();

        // Viewer-panel textures (separate from AppState).
        self.viewer.video_texture = None;
        self.viewer.title_overlay_texture = None;
        self.viewer.logo_texture = None;

        // Browser caches.
        self.browser.clear_textures_for_shutdown();
        self.new_inspector.clear_textures_for_shutdown();

        // Empty every bucket of the AppState-side deferred-drop ring;
        // any handle queued in the last two frames hasn't been
        // released yet and would otherwise drop after wgpu teardown.
        for bucket in &mut self.state.deferred_texture_drops {
            bucket.clear();
        }
        let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
        eprintln!("on_exit DONE — cleanup took {:.1}ms; exiting via process::exit(0)", elapsed_ms);
        tracing::info!("on_exit DONE — cleanup took {:.1}ms; exiting via process::exit(0)", elapsed_ms);

        // Kill child processes (proxy ffmpeg, render queue) — this is
        // normally done by KlippApp::Drop, but we're about to skip
        // every drop with process::exit, so do it here explicitly.
        for pid_ref in &self.proxy_children {
            let pid = pid_ref.load(std::sync::atomic::Ordering::Relaxed);
            if pid != 0 {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
            }
        }
        self.state.render_queue.kill_active();

        // PRAGMATIC FIX (after 8+ rounds of plugging specific
        // wgpu-resource leaks): once eframe is exiting, the
        // drop-order race between our app state and eframe's wgpu
        // teardown causes SIGSEGV regardless of how thoroughly we
        // clear our textures. The OS reaps the process cleanly via
        // exit(0); the user sees the window disappear immediately
        // and there are no further side effects we'd be skipping
        // (autosave delete already runs on a detached thread, child
        // processes were killed above). This is a known mitigation
        // pattern for wgpu/eframe + Wayland teardown issues.
        self.exit_after_engine_shutdown();
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.run_startup_open();
        // Heartbeat for the freeze watchdog in klipp-app::main. Bumped
        // at the very top so any stall below (layout, viewer paint,
        // node graph layout, mutex wait) freezes the counter.
        klipp_engine::UI_TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::FRAME_TOP);
        // How long we were *outside* this function. Everything eframe
        // does between two updates — paint, submit, present, and the
        // wait for a surface texture — lands here.
        let update_entered = std::time::Instant::now();
        self.hb_max_gap = self.hb_max_gap.max(self.last_update_end.elapsed());

        // End-to-end UI tick timing. A single update() that exceeds the
        // 16.6 ms frame budget starves the window of paint events —
        // that's the "app feels frozen" symptom the user sees during
        // slider drags. The total is logged at the end when > 25 ms;
        // the per-stage breakdowns (scope preview refresh, viewer paint)
        // are logged from their own sites and correlated by timestamp.
        let t_update_start = std::time::Instant::now();

        self.begin_frame(ctx, frame);
        self.take_in_results(ctx, frame);
        self.sync_derived_state(ctx);
        self.tick_meters_and_skims(ctx);
        let all_cmds = self.draw_window(ctx);
        let has_quit = self.dispatch_commands(ctx, all_cmds);
        self.finish_frame(ctx, has_quit);

        // Log slow UI ticks. 25 ms is a conservative threshold — at 60 fps
        // we have 16.6 ms, so anything > 25 ms dropped at least one frame.
        // Correlate the timestamp with ENGINE_TICK_SLOW / PERF post_process_wrap /
        // SCOPE_PREVIEW_FAST lines to see which subsystem owned the time.
        //
        // Also logs a separate UI_TICK_FROZEN line at ERROR level when the
        // tick runs longer than 2 s — that's the threshold where the user
        // starts reaching for Ctrl-C. Having a distinct log name makes the
        // "what actually froze the app" grep trivial on a soak run.
        let ui_tick_ms = t_update_start.elapsed().as_secs_f64() * 1000.0;
        if ui_tick_ms > 2000.0 {
            tracing::error!(
                "UI_TICK_FROZEN {:.0}ms state={:?} show_scopes={} show_color={} viewer_mode={:?}",
                ui_tick_ms,
                self.state.playback,
                self.state.show_scopes,
                self.state.show_color_panel,
                self.state.viewer_mode,
            );
        } else if ui_tick_ms > 25.0 {
            tracing::debug!(
                "UI_TICK_SLOW {:.1}ms state={:?} show_scopes={} show_color={} viewer_mode={:?}",
                ui_tick_ms,
                self.state.playback,
                self.state.show_scopes,
                self.state.show_color_panel,
                self.state.viewer_mode,
            );
        }

        // Closes the pair opened at the top: time inside this function,
        // and the mark for the gap until the next call.
        self.hb_max_update = self.hb_max_update.max(update_entered.elapsed());
        self.last_update_end = std::time::Instant::now();
    }
}

/// Check if Klipp's audio stream is muted at the OS level (PipeWire /
/// WirePlumber). WirePlumber persists per-app volume and mute state in
/// `~/.local/state/wireplumber/stream-properties` as JSON-ish entries.
/// Klipp's entry looks like:
///   `Output/Audio:application.name:PipeWire\sALSA\s\oklipp-app\c={"mute":true,...}`
/// We just read this file and look for `klipp-app` + `"mute":true`.
///
/// Returns `false` on any error (file missing, non-Linux, non-PipeWire)
/// so the warning only shows when we're confident our stream is muted.
/// No subprocess spawning — pure file read, <0.1ms.
fn check_system_audio_muted() -> bool {
    let path = dirs::state_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local").join("state")))
        .map(|d| d.join("wireplumber").join("stream-properties"));
    let Some(path) = path else { return false };
    let contents = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return false,
    };
    // Find the line containing klipp-app and check for "mute":true.
    // The entry key uses WirePlumber's escape format:
    //   \s = space, \o = open-paren, \c = close-paren
    // We just search for "klipp-app" which is unambiguous.
    for line in contents.lines() {
        if line.contains("klipp-app") {
            // The value after '=' is a JSON object. Check for "mute":true.
            if let Some(eq_pos) = line.find('=') {
                let json_part = &line[eq_pos + 1..];
                // Simple string check — no need for a full JSON parser.
                return json_part.contains("\"mute\":true");
            }
        }
    }
    false
}

/// `s` with its first letter upper-case, for a refusal's hint shown as a
/// sentence of its own.
fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// [`shortcuts::remove_selected_keyframe`], for the key selection (#1958).
pub(crate) fn remove_selected_keyframe_for(state: &crate::state::AppState, sk: &crate::state::SelectedKeyframe) -> Option<Vec<AppCommand>> {
    shortcuts::remove_selected_keyframe(state, sk)
}

#[cfg(test)]
mod auto_zoom_tests {
    /// 4K at 1x (3840) or 1.25x (3072) gets a quarter more; 4K at 2x
    /// (1920), 1440p (2560) and 1080p keep the system scale.
    #[test]
    fn a_large_screen_at_its_system_scale_gets_a_quarter_more() {
        for (w, h, want) in [(3840.0, 2160.0, 1.25), (3072.0, 1728.0, 1.25), (2560.0, 1440.0, 1.0), (1920.0, 1080.0, 1.0)] {
            assert_eq!(super::auto_zoom_factor(egui::vec2(w, h)), want, "{w}x{h}");
        }
    }

    /// #2944: a small window steps down, 90% then 80%, never below; at
    /// 1366×768 and 1280×720 the editor then has 1600×900 points or more.
    #[test]
    fn a_small_window_gets_a_smaller_interface() {
        for (w, h, want) in [(1366.0, 768.0, 0.8), (1280.0, 720.0, 0.8), (1536.0, 864.0, 0.9), (1440.0, 900.0, 0.9), (1024.0, 600.0, 0.8), (1600.0, 900.0, 1.0)] {
            let z = super::auto_zoom_factor(egui::vec2(w, h));
            assert_eq!(z, want, "{w}x{h}");
            assert!(z >= 0.8, "readable");
        }
        for (w, h) in [(1366.0_f32, 768.0_f32), (1280.0, 720.0)] {
            let z = super::auto_zoom_factor(egui::vec2(w, h));
            assert!(w / z >= 1600.0 - 0.5 && h / z >= 900.0 - 0.5, "{w}x{h} at {z}: {}x{}", w / z, h / z);
        }
    }
}
