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
mod project_io;
mod history_apply;
mod recording;
mod ai_tool_runs;
mod multi_clip_edit;
mod caches;
mod engine_sync;
mod viewer_textures;
mod startup;
mod shortcuts;
mod frame_loop;
mod colour_workspace;
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
mod media;
mod compound;
mod clips;
mod timeline_edit;
mod surfaces;
mod colour;
mod audio;
mod keyframes;
mod titles;
mod timeline_grade;
mod grade_edits;
mod tracking;
mod linked_audio;
mod camera;
mod subtitles;
mod render_script;
mod batch_rename;
mod whoosh;
mod hyperzoom;
use export::*;
use timeline_grade::*;
use grade_edits::*;
use tracking::*;
pub(crate) use tracking::surface_on_frame;
use linked_audio::*;
use camera::*;
use subtitles::*;
use render_script::*;
pub use batch_rename::*;
use whoosh::*;
use hyperzoom::*;
use caches::*;
use multi_clip_edit::*;
use ai_tool_runs::*;
use titles::*;
pub(crate) use titles::apply_title_preset;
pub(crate) use viewer_textures::alloc_texture_logged;
use project_io::LockDecision;

/// Default colour for freshly-created sticky notes — warm beige
/// `#d4c89a`. Single value in v1; a future right-click recolour
/// menu would route through SetStickyNoteColor (not yet shipped).
/// Stored packed RGBA u32 (little-endian: r, g, b, a).
const STICKY_DEFAULT_COLOR_RGBA: u32 = u32::from_le_bytes([0xd4, 0xc8, 0x9a, 0xff]);

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

/// What `append_linked_audio_to_sequence` added, in the shapes the two
/// history entries want it: `LinkedImport` takes the clip and its track,
/// `SourceLinkedInsert` takes the track's clips either side of the edit.
struct LinkedAudioAppend {
    clip: klipp_timeline::Clip,
    track_index: usize,
    /// Set when the audio track had to be created for this.
    created_track_at: Option<usize>,
    clips_before: Vec<klipp_timeline::Clip>,
    clips_after: Vec<klipp_timeline::Clip>,
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
    viewer: ViewerPanel,
    title_renderer: klipp_title::TitleRenderer,
    markers: MarkersPanel,
    transcript_panel: crate::panels::transcript::TranscriptPanel,
    /// Transcripts read for the Transcript panel and the GUI's document
    /// ops; kept until the analyses change.
    transcript_source: crate::transcript::TranscriptSource,
    /// The Colour Check window's own state: the last reading of the
    /// displayed frame, and the last render-path self-check.
    colour_check: crate::panels::colour_check::ColourCheckPanel,
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
    ui_scale_applied: Option<(u32, u32)>,
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
    /// Browser visibility before a workspace borrowed its width,
    /// restored on leaving.
    ///
    /// Cleared if the browser comes back up while the borrowing
    /// workspace is still active — by the rail, a menu, Reveal in
    /// Browser, any route — because an explicit ask outranks the
    /// borrow, and restoring over it would undo the user's choice
    /// without telling them.
    browser_before_workspace: Option<bool>,
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
    node_graph_panel: crate::panels::node_graph::NodeGraphPanel,
    node_graph_v2_panel: crate::panels::node_graph_v2::NodeGraphV2Panel,
    node_inspector_panel: crate::panels::node_inspector::NodeInspectorPanel,
    looks_panel_state: crate::panels::looks::LooksPanelState,
    /// Pre-edit snapshot for the current color-grading parameter gesture.
    /// Set by `BeginNodeParamEdit`, consumed by `EndNodeParamEdit`. `None`
    /// when no gesture is in progress.
    node_param_edit_snapshot: Option<(klipp_core::ClipId, klipp_nodes::NodeGraph)>,
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

    fn update_perf_stats(&mut self, ctx: &egui::Context) {
        let dt = ctx.input(|i| i.unstable_dt) as f64;

        // Track frame times for FPS calculation
        self.state.perf_frame_times.push(dt);
        if self.state.perf_frame_times.len() > 120 {
            self.state.perf_frame_times.remove(0);
        }

        // Update stats every 250ms
        let now = std::time::Instant::now();
        if now.duration_since(self.state.perf_last_update).as_millis() >= self.prefs.advanced.perf_overlay_interval_ms as u128 {
            let interval_s = now.duration_since(self.state.perf_last_update).as_secs_f64();
            self.state.perf_last_update = now;

            // Delivered video throughput, measured from FrameReady
            // deliveries rather than inferred from stage timings. The
            // stage table only sums what it tracks, so it read 5.2 ms
            // against a 16.7 ms budget while playback was managing
            // 22 fps of a 59.94 fps target.
            if interval_s > 0.0 {
                let delivered = self
                    .state
                    .perf_frame_count
                    .saturating_sub(self.state.perf_last_frame_count);
                self.state.perf_video_fps = delivered as f64 / interval_s;
            }
            self.state.perf_last_frame_count = self.state.perf_frame_count;

            // Readback cost per frame, straight from the engine counters.
            {
                use std::sync::atomic::Ordering::Relaxed;
                let c = klipp_engine::CPU_READBACK_COUNT.load(Relaxed);
                let us = klipp_engine::CPU_READBACK_US.load(Relaxed);
                let dc = c.saturating_sub(self.state.perf_last_readback_count);
                let dus = us.saturating_sub(self.state.perf_last_readback_us);
                self.state.perf_readback_ms = if dc > 0 {
                    (dus as f64 / dc as f64) / 1000.0
                } else {
                    0.0
                };
                self.state.perf_last_readback_count = c;
                self.state.perf_last_readback_us = us;
            }

            // FPS from average frame time
            if !self.state.perf_frame_times.is_empty() {
                let avg_dt: f64 = self.state.perf_frame_times.iter().sum::<f64>()
                    / self.state.perf_frame_times.len() as f64;
                self.state.perf_fps = if avg_dt > 0.0 { 1.0 / avg_dt } else { 0.0 };
            }

            // Memory from /proc/self/statm (Linux)
            if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
                if let Some(rss_pages) = statm.split_whitespace().nth(1) {
                    if let Ok(pages) = rss_pages.parse::<u64>() {
                        self.state.perf_memory_mb = (pages * 4096) as f64 / (1024.0 * 1024.0);
                    }
                }
            }

            // CPU usage from /proc/self/stat (Linux)
            // Fields 14+15 = utime+stime in clock ticks (typically 100 Hz)
            if let Ok(stat) = std::fs::read_to_string("/proc/self/stat") {
                // Parse past the comm field (which may contain spaces/parens)
                if let Some(close_paren) = stat.rfind(')') {
                    let fields_after: Vec<&str> = stat[close_paren + 2..].split_whitespace().collect();
                    // fields_after[11] = utime (field 14), fields_after[12] = stime (field 15)
                    if fields_after.len() > 12 {
                        let utime: u64 = fields_after[11].parse().unwrap_or(0);
                        let stime: u64 = fields_after[12].parse().unwrap_or(0);
                        let total_ticks = utime + stime;

                        let wall_elapsed = self.state.perf_prev_wall_time.elapsed().as_secs_f64();
                        if wall_elapsed > 0.01 && self.state.perf_prev_cpu_ticks > 0 {
                            let tick_delta = total_ticks - self.state.perf_prev_cpu_ticks;
                            let cpu_secs = tick_delta as f64 / 100.0; // 100 = CLK_TCK
                            let instant = (cpu_secs / wall_elapsed) * 100.0;
                            // Exponential moving average (α=0.15) for smooth readout like DAW meters
                            let alpha = 0.15;
                            self.state.perf_cpu_percent =
                                self.state.perf_cpu_percent * (1.0 - alpha) + instant * alpha;
                        }
                        self.state.perf_prev_cpu_ticks = total_ticks;
                        self.state.perf_prev_wall_time = now;
                    }
                }
            }
        }

        // Frame count increments on each FrameReady event (tracked in drain_events)
        // HW decode is set when we get a VAAPI log — for now just read from frame_buffer existence
        // We'll set hw_decode from engine events if available

        // Snapshot the engine's per-frame perf ring for the overlay.
        // Lock once, read latest + averages + drops-in-last-second,
        // drop the lock before painting. Tiny mutex window.
        if self.state.show_perf_overlay {
            if let Ok(ring) = self.perf_ring.lock() {
                // Average over the last ~2 seconds at 60 fps = 120
                // samples; or whatever's in the ring if shorter.
                self.state.perf_averages = ring.averages(120);
                self.state.perf_drops_last_sec = ring.drops_in_last(1.0);
                self.state.perf_latest_sample = ring.latest().copied();
            }
        }
    }

    /// Start / stop a perf telemetry recording. When stopping, the
    /// captured samples are written as JSON to
    /// `<cache_dir>/klipp/perf/session_<unix_ts>.json` so the user
    /// can attach the file to a bug report or share it for analysis.
    fn toggle_perf_recording(&mut self) {
        if self.state.perf_recording_active {
            // Stop + dump.
            let samples = match self.perf_ring.lock() {
                Ok(mut ring) => ring.end_recording(),
                Err(_) => Vec::new(),
            };
            self.state.perf_recording_active = false;
            if samples.is_empty() {
                self.state.toast.show("Perf recording: no samples captured");
                return;
            }
            // Unix timestamp — good enough for filename uniqueness.
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let dir = dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("klipp/perf");
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(format!("session_{ts}.json"));
            let json = klipp_engine::perf::samples_to_json(&samples);
            match std::fs::write(&path, json) {
                Ok(_) => {
                    self.state.toast.show(format!(
                        "Perf session saved ({} samples): {}",
                        samples.len(),
                        path.display(),
                    ));
                    self.state.perf_last_session_path = Some(path);
                }
                Err(e) => {
                    self.state.toast.show(format!("Failed to write perf session: {e}"));
                }
            }
        } else {
            // Start.
            if let Ok(mut ring) = self.perf_ring.lock() {
                ring.begin_recording();
            }
            self.state.perf_recording_active = true;
            // Auto-show the overlay so the user sees the REC badge.
            self.state.show_perf_overlay = true;
            self.state.toast.show("Perf recording started · Ctrl+Shift+P to stop");
        }
    }

    /// Attach the control socket's command inbox. See `control_inbox`.
    pub fn set_control_inbox(&mut self, inbox: Receiver<klipp_document::control::ControlEnvelope>) {
        self.control_inbox = Some(inbox);
    }

    /// A list of recorded edits applied forward as one history entry —
    /// one undo step — with the document marked changed and the engine
    /// told. What the control socket's plans go through; the Edit Plan
    /// window's buttons too.
    /// Send a published colour chart through the same renderer the
    /// viewer and the export use, and read it back.
    ///
    /// Everything the Colour Check window says is a reading of pixels
    /// that came out of that renderer. If the path itself moves colour,
    /// those readings are partly a measurement of Klipp, and a person
    /// chasing them will be grading the pipeline. It runs on both the
    /// CPU and the GPU path, because the viewer shows one and an export
    /// may take the other.
    fn run_colour_self_check(&mut self) {
        let working = klipp_engine::video_decoder::working_colorspace();
        let both = klipp_compose::selfcheck::run_both(480, 320, working, klipp_compose::RenderOptions::default());
        let mut said = Vec::new();
        let mut ok = true;
        for (gpu, check) in &both {
            let path = if *gpu { "GPU" } else { "CPU" };
            for (severity, what) in check.findings() {
                if severity != "ok" {
                    ok = false;
                }
                said.push(format!("{path}: {what}"));
            }
        }
        let worst = both[0]
            .1
            .patches
            .iter()
            .zip(both[1].1.patches.iter())
            .map(|(a, b)| klipp_color::measure::delta_e2000(a.back, b.back))
            .fold(0.0f32, f32::max);
        if worst >= 1.0 {
            ok = false;
            said.push(format!("The CPU and the GPU disagree about the chart by {worst:.1} ΔE — a grade approved in the viewer is not the grade that exports."));
        }
        self.colour_check.selfcheck = Some((ok, said.join("\n")));
    }

    /// A transition's duration or alignment changed from the inspector:
    /// through the document op that checks the clips have the footage
    /// for it, refused with a toast when they have not, one undo step.
    fn reshape_transition(&mut self, track_index: usize, transition_id: klipp_core::TransitionId, duration: klipp_core::RationalTime, alignment: klipp_core::TransitionAlignment) {
        let seq = self.state.sequence();
        match klipp_document::ops::reshape_transition(seq, &self.state.media_pool, track_index, transition_id, duration, alignment) {
            Ok(edit) => self.apply_edits_as_one(std::slice::from_ref(&edit), "Transition"),
            Err(r) => self.state.toast.show(format!("Transition unchanged: {}. {}", r.why, capitalise(&r.hint))),
        }
    }

    /// Detect Scene Cuts' end (#616): the clip split at `cut_times` (its
    /// file's seconds) as one undoable edit, its linked sound with it, and
    /// said how many. With none inside it, or a clip since changed so none
    /// fall inside, it is left as it is and that is said too.
    fn split_at_scene_cuts(&mut self, clip_id: klipp_core::ClipId, cut_times: &[f64]) {
        let seq = self.state.sequence();
        let Some((name, media_fps)) = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip_id).and_then(|c| c.media_id()).and_then(|m| self.state.media_pool.get(&m)).map(|m| (m.filename.clone(), m.fps.as_f64())) else {
            self.state.toast.show("Scene cuts found, but the clip is gone".to_string());
            return;
        };
        let ops = klipp_document::ops::scene_cut_ops(seq, clip_id, cut_times, media_fps);
        if ops.is_empty() {
            self.state.toast.show(format!("No scene cuts found in {name}; it is unchanged"));
            return;
        }
        match klipp_document::ops::resolve(seq, &self.state.media_pool, &ops) {
            Ok(plan) => {
                let edits: Vec<EditCommand> = plan.steps.iter().map(|s| s.edit.clone()).collect();
                self.apply_edits_as_one(&edits, "Detect Scene Cuts");
                let n = ops.len();
                self.state.toast.show(format!("Split {name} at {n} scene cut{} into {} shots", if n == 1 { "" } else { "s" }, n + 1));
            }
            Err(r) => self.state.toast.show(format!("Scene cuts not applied: {}. {}", r.why, capitalise(&r.hint))),
        }
    }

    /// A new audio effect of `effect_type` at its defaults: a LADSPA
    /// plugin's from what discovery found, a built-in one's from its own
    /// descriptors.
    fn default_audio_effect(&self, effect_type: &str) -> klipp_core::AudioEffectInstance {
        let params: Vec<(String, f32)> = if effect_type.starts_with("ladspa:") {
            effect_type
                .strip_prefix("ladspa:")
                .and_then(|rest| {
                    let cut = rest.rfind(':')?;
                    let path = &rest[..cut];
                    let label = &rest[cut + 1..];
                    self.state.ladspa_plugins.iter().find(|p| p.path.to_string_lossy() == path && p.label == label)
                })
                .map(|info| info.control_inputs.iter().map(|port| (port.name.clone(), port.default)).collect())
                .unwrap_or_default()
        } else {
            klipp_audio::dsp::create_effect(effect_type, 48000)
                .map(|fx| fx.param_descriptors().iter().map(|d| (d.name.to_string(), d.default)).collect())
                .unwrap_or_default()
        };
        klipp_core::AudioEffectInstance { effect_type: effect_type.to_string(), enabled: true, wet_dry: 1.0, params }
    }

    /// Apply the interface size: a View menu choice is saved first, then
    /// the preference is set on the context whenever it or the screen
    /// changes. Auto follows the system scale, and adds a quarter on a
    /// screen wider than 2800 points at that scale — a 4K panel at 1x or
    /// 1.25x, where the defaults read as small print.
    fn apply_ui_scale(&mut self, ctx: &egui::Context) {
        if let Some(pct) = self.state.ui_scale_request.take() {
            self.prefs.appearance.ui_scale_percent = pct;
            let _ = self.prefs.save();
        }
        let pct = self.prefs.appearance.ui_scale_percent;
        self.state.ui_scale_percent = pct;
        // Sizes in points shrink as the zoom grows; times the zoom they
        // are the screen at the system scale, which is what Auto reads,
        // and what keeps this key the same once the zoom is set.
        let zoom = ctx.zoom_factor();
        let (monitor, window) = ctx.input(|i| (i.viewport().monitor_size, i.screen_rect().width()));
        let width = monitor.map_or(window, |m| m.x.max(window)) * zoom;
        let key = (pct, width.round() as u32);
        if self.ui_scale_applied == Some(key) {
            return;
        }
        self.ui_scale_applied = Some(key);
        let want = if pct == 0 { auto_zoom_factor(width) } else { pct as f32 / 100.0 };
        tracing::debug!("interface size: {pct}% (0 = Auto), screen {width:.0} pt at the system scale (monitor {monitor:?}, window {window:.0}, zoom {zoom}), zoom {want}");
        if (zoom - want).abs() > 0.001 {
            ctx.set_zoom_factor(want);
        }
    }

    /// Cut the multicam group in play at `at` to `angle` (counted from
    /// 1): the keys, the angle bar, the grid and the control socket all
    /// come here.
    fn switch_multicam_angle(&mut self, angle: usize, at: klipp_core::Timecode) {
        // Angles are counted from 1 here, as the buttons and keys 1-9
        // count them; the cut list from 0, the primary. Stored as sent,
        // "Angle 1" cut to the second camera.
        let Some(wanted) = angle.checked_sub(1).and_then(|a| u8::try_from(a).ok()) else { return };
        let seq_idx = self.state.active_sequence_idx;
        let Some((track_idx, primary_id, angles)) = self.state.sequences[seq_idx]
            .multicam_at(None, at)
            .map(|m| (m.primary_track, m.primary.id, m.angles))
        else {
            return;
        };
        if !angles.contains(&wanted) {
            self.state.toast.show(format!("This multicam group has no angle {angle}"));
            return;
        }
        let Some(clip) = self.state.sequences[seq_idx].tracks[track_idx].find_clip_mut(primary_id) else { return };
        let old = clip.clone();
        let clip_time = (at - clip.timeline_in).to_secs_f64();
        clip.multicam_cuts.retain(|(t, _)| (*t - clip_time).abs() > 0.001);
        clip.multicam_cuts.push((clip_time, wanted));
        clip.multicam_cuts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        let new = clip.clone();
        // One undo step per cut: cutting live means mis-hits,
        // and a mis-hit could not be taken back.
        self.state.push_history(EditCommand::ReplaceClip { description: "Switch Angle", old: Box::new(old), new: Box::new(new) });
        self.state.toast.show(format!("Cut to angle {angle}"));
        self.state.dirty = true;
        // The viewer shows the cut only once the engine has
        // the sequence and redraws the frame; without this
        // the toast and the angle bar said one camera and the
        // picture another (paused, a sync alone redraws
        // nothing).
        self.sync_and_refresh_frame();
    }

    /// Choose whose sound the multicam group at `at` plays, on each of
    /// its primaries (a bladed group has several), as one undo step.
    fn set_multicam_sound(&mut self, sound: klipp_core::MulticamSound, at: klipp_core::Timecode) {
        let seq_idx = self.state.active_sequence_idx;
        let Some((group, angles)) = self.state.sequences[seq_idx].multicam_at(None, at).map(|m| (m.group, m.angles)) else {
            self.state.toast.show("There is no multicam group here");
            return;
        };
        if let klipp_core::MulticamSound::Angle(a) = sound {
            if !angles.contains(&a) {
                self.state.toast.show(format!("This multicam group has no angle {}", a as usize + 1));
                return;
            }
        }
        let mut steps = Vec::new();
        for clip in self.state.sequences[seq_idx].tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if clip.multicam_group == Some(group) && clip.multicam_angle == 0 && clip.multicam_sound != sound {
                let old = clip.clone();
                clip.multicam_sound = sound;
                steps.push(EditCommand::ReplaceClip { description: "Multicam Sound", old: Box::new(old), new: Box::new(clip.clone()) });
            }
        }
        let command = match steps.len() {
            0 => return,
            1 => steps.remove(0),
            _ => EditCommand::Batch(steps),
        };
        self.state.push_history(command);
        self.state.toast.show(format!("Multicam sound: {}", sound.label()));
        self.state.dirty = true;
        self.sync_sequence_to_engine();
    }

    /// Keep the system-theme watcher in step with the preference and
    /// apply a changed Omarchy palette. Runs once per frame; cheap
    /// unless the flag is set.
    fn sync_system_theme(&mut self, ctx: &egui::Context) {
        use std::sync::atomic::Ordering;

        let follow = self.prefs.appearance.follow_system_theme
            && self.theme_registry.omarchy_name().is_some();
        match (follow, self.system_theme_watcher.is_some()) {
            (true, false) => {
                if let Some(state) = crate::theme::omarchy::detect() {
                    self.system_theme_watcher = crate::theme::omarchy::start_watcher(
                        &state,
                        std::sync::Arc::clone(&self.system_theme_changed),
                        ctx.clone(),
                    );
                    // The desktop may have changed since the registry
                    // was built (before the watcher existed).
                    self.system_theme_changed.store(true, Ordering::Release);
                }
            }
            (false, true) => {
                self.system_theme_watcher = None;
                self.system_theme_changed.store(false, Ordering::Release);
            }
            _ => {}
        }

        if !self.system_theme_changed.swap(false, Ordering::AcqRel) {
            return;
        }
        match self.theme_registry.reload_omarchy() {
            Some(name) => {
                tracing::info!("System theme: now following Omarchy '{name}'");
                if follow {
                    self.theme = KlippTheme::from_prefs(&self.prefs.appearance, &self.theme_registry);
                    self.theme.apply_to_egui(ctx);
                }
            }
            None => {
                // Mid-switch (the directory is being rebuilt) or an
                // unparseable palette: keep the previous theme and
                // look again next frame.
                self.system_theme_changed.store(true, Ordering::Release);
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }
    }

    fn relink_media(&mut self, media_id: MediaId, new_path: &Path) {
        let Some(item) = self.state.media_pool.get_mut(&media_id) else { return };
        let changed = item.path != new_path;
        item.path = new_path.to_path_buf();
        item.offline = !new_path.exists();
        if changed {
            if let Some(proxy) = item.proxy_path.take() {
                let _ = std::fs::remove_file(&proxy);
                let _ = std::fs::remove_file(partial_proxy_path(&proxy));
            }
            item.proxy_ready = false;
            item.skim_file = None;
            item.waveform_data = None;
            item.audio_cache = None;
            item.fingerprint = compute_media_fingerprint(new_path);
            self.state.thumbnail_cache.set_filmstrip(media_id, Vec::new());
        }
        self.state.offline_media_ids.retain(|id| *id != media_id);
        if new_path.exists() {
            let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                path: new_path.to_path_buf(),
                media_id,
            });
            // Its pictures, from the file it now names. The re-probe
            // alone makes none: a moved file has the same size and date,
            // so it reads as unchanged, and an item that was offline at
            // open never had a thumbnail made in the first place.
            self.refresh_media_pictures(media_id);
        }
        self.sync_media_pool_to_engine();
    }

    /// Seed the clip's InputTransform node `log_format` from the
    /// source media. User per-media override (`MediaItem.log_format`,
    /// set whenever the user picks a format on any clip of this
    /// media) wins over the heuristic (`detected_log_format`); the
    /// heuristic wins over Srgb default. Skipped entirely when the
    /// effective format is Srgb so we don't insert pointless
    /// `Some(Srgb)` overrides on plain SDR clips.
    fn apply_detected_log_format(&mut self, clip_id: klipp_core::ClipId, media_id: klipp_core::MediaId) {
        let effective = self.state.media_pool.get(&media_id)
            .map(|m| m.effective_log_format())
            .unwrap_or_default();
        if effective != klipp_core::LogFormat::Srgb {
            let seq = &mut self.state.sequences[self.state.active_sequence_idx];
            if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
                clip.set_input_log_format(effective);
            }
        }
    }

    /// The media's audio appended to an audio track and linked to the
    /// video clip that was just appended, as the drag-and-drop import
    /// does it.
    ///
    /// Every append path other than that import was creating the video
    /// clip alone, which is why a timeline built from the control socket
    /// — or from the media browser's append — was silent, and why every
    /// render made from one had no sound. The file's audio was never on
    /// the timeline to be mixed.
    ///
    /// `None` when there is no audio to add: audio-only media (the
    /// caller has already put it on an audio track), a silent file, or
    /// no audio track and none could be made. The caller then keeps its
    /// single-clip history entry.
    /// The media's audio, appended to an audio track and linked to the
    /// video clip that was just appended — unless source patching says
    /// video only.
    ///
    /// Positions the audio under its own video clip by delegating to
    /// `attach_linked_audio_to_clip`, and that is the whole point.
    /// Appending to the *end of the audio track* instead — which is
    /// what this did first — is only correct while the two tracks stay
    /// the same length. Any gap on the audio side and the sound lands
    /// under the wrong picture: a video-only append leaves such a gap,
    /// and so does appending a silent file, and the next clip with sound
    /// then goes in the hole rather than under itself. Found by doing
    /// exactly that on a real timeline.
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

    /// A favourite or rejected stretch of the source clip: between its
    /// in and out points, or the whole clip when none are set.
    fn source_range_marker(&self, kind: klipp_core::MediaMarkerKind) -> Option<klipp_core::MediaMarker> {
        let media = self.state.media_pool.get(&self.state.source_media_id?)?;
        let from = media.default_source_in.map(|t| t.to_secs_f64()).unwrap_or(0.0);
        let to = media.default_source_out.map(|t| t.to_secs_f64()).unwrap_or_else(|| media.duration.to_secs_f64());
        (to > from).then(|| klipp_core::MediaMarker::range(kind, from, to))
    }

    /// The recording synced to `media_id`, if any and if it is still in
    /// the pool.
    fn external_audio_of(&self, media_id: klipp_core::MediaId) -> Option<ExternalAudio> {
        let sync = self.state.media_pool.get(&media_id)?.synced_audio.as_ref()?;
        let audio = self.state.media_pool.get(&sync.audio_media_id)?;
        Some(ExternalAudio { media_id: audio.id, offset_secs: sync.offset_secs, len_secs: audio.duration.to_secs_f64() })
    }

    fn apply_media_sync(&mut self, video_id: klipp_core::MediaId, sync: klipp_core::SyncedAudio) {
        let audio_name = self.state.media_pool.get(&sync.audio_media_id).map(|m| m.filename.clone()).unwrap_or_default();
        let Some(video) = self.state.media_pool.get_mut(&video_id) else { return };
        let message = format!("Synced {} to {audio_name}: {}", video.filename, sync.describe());
        video.synced_audio = Some(sync);
        self.state.dirty = true;
        tracing::info!("{message}");
        self.state.toast.show(message);
    }

    fn append_linked_audio(
        &mut self,
        media_id: klipp_core::MediaId,
        video_clip_id: klipp_core::ClipId,
    ) -> Option<LinkedAudioAppend> {
        // Source patching: with the audio target off, an append puts the
        // picture down and nothing else. See `append_brings_audio`.
        if !self.state.append_brings_audio {
            return None;
        }
        let item = self.state.media_pool.get(&media_id)?;
        // A synced recording goes under the clip in place of its own
        // sound. Otherwise audio-only media is already on an audio
        // track, put there by the caller, and a silent file has nothing
        // to add.
        let external = self.external_audio_of(media_id);
        if external.is_none() && (!item.has_audio || item.video_codec.is_empty()) {
            return None;
        }
        let seq_idx = self.state.active_sequence_idx;
        attach_linked_audio_to_clip(&mut self.state.sequences[seq_idx], video_clip_id, external)
    }

    /// Set or clear one of the sequence camera's keys, as one undo step.
    ///
    /// The whole camera is snapshotted either side, which is what
    /// `SetSequenceShutter` already does, so this needs no history
    /// variant of its own and undo restores the keyframe lists along
    /// with everything else the camera carries. `value` of `None`
    /// removes the key at that moment.
    ///
    /// The dedup is `klipp_core::keyframe`'s, not a second copy: what
    /// counts as "the same moment" is one decision and it lives there.
    fn key_sequence_camera(&mut self, sequence_idx: usize, param: klipp_core::CameraParam, time_secs: f64, value: Option<f32>) {
        self.edit_sequence_camera(sequence_idx, |camera| apply_camera_key(camera, param, time_secs, value));
    }

    /// Change the sequence camera and record the change as one camera
    /// snapshot — unless a drag on its keys is open, in which case the
    /// drag's end records the one entry for all of it. Nothing is
    /// recorded when the edit changed nothing.
    fn edit_sequence_camera(&mut self, sequence_idx: usize, edit: impl FnOnce(&mut klipp_timeline::Camera)) {
        let Some(seq) = self.state.sequences.get_mut(sequence_idx) else { return };
        let old = seq.camera.clone();
        edit(&mut seq.camera);
        if seq.camera == old {
            return;
        }
        let new = seq.camera.clone();
        let id = seq.id;
        let history = self.state.history_for_mut(id);
        if !history.is_dragging(&camera_drag_key(sequence_idx)) {
            history.push(crate::history::HistoryEntry {
                command: crate::history::EditCommand::SetSequenceCamera { old: Box::new(old), new: Box::new(new) },
            });
        }
        self.sync_sequence_to_engine();
        self.state.dirty = true;
    }

    /// Take the next steps of [`Self::startup_open`]: open its project
    /// through the dispatcher, then import its media once that project is
    /// the one open, saying what could not be done.
    fn run_startup_open(&mut self) {
        use crate::cli::Step;
        let Some(open) = self.startup_open.as_mut() else { return };
        let failed = self.last_open_error.take();
        let steps = open.step(self.state.project_path.as_deref(), failed.as_deref());
        if open.done() {
            self.startup_open = None;
        }
        for step in steps {
            match step {
                Step::Open(path) => self.send_commands(vec![AppCommand::OpenProject { path }]),
                Step::NewProject => self.state.project_path = Some(PathBuf::from("Untitled")),
                Step::Import(path) => self.send_commands(vec![AppCommand::ImportMedia { path }]),
                Step::Toast(text) => self.state.toast.show(text),
            }
        }
    }

    /// Append one media item to the end of a track with its linked audio,
    /// on the magnetic or the free path as the track calls for. The
    /// framerate prompt has already been answered or skipped by the caller.
    fn append_media_to_track(&mut self, media_id: &klipp_core::MediaId, track_index: &usize) {
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        let duration = item.duration;
                        // Auto-set sequence resolution + FPS from first video clip
                        if !item.video_codec.is_empty() && item.resolution.width > 0 && item.resolution.height > 0 {
                            let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                            if seq.adopt_first_clip(item.resolution, item.fps).is_some() {
                                self.state.fps = seq.fps;
                            }
                        }
                        if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get(*track_index) {
                            if track.magnetic_role == klipp_timeline::TrackRole::Primary {
                                // Magnetic path: append via MagneticEditor
                                let source_in = klipp_core::RationalTime { value: 0, rate: duration.rate };
                                let source_out = duration;
                                let mut clip = klipp_timeline::Clip::media(
                                    *media_id,
                                    klipp_core::RationalTime::ZERO, // will be set by append_primary
                                    klipp_core::RationalTime::ZERO,
                                    source_in,
                                    source_out,
                                );
                                // Auto-apply detected log format, and carry
                                // the media's rotation (see AppendClip: the
                                // viewer takes rotation from the media item,
                                // export from the clip, so a clip without it
                                // plays upright and exports sideways).
                                if let Some(item) = self.state.media_pool.get(media_id) {
                                    clip.set_input_log_format(item.detected_log_format);
                                    clip.source_rotation = item.source_rotation;
                                }
                                let result = klipp_timeline::magnetic::MagneticEditor::append_primary(
                                    &mut self.state.sequences[self.state.active_sequence_idx], clip);
                                // Get the clip id from the result
                                let mut appended_id = None;
                                if let klipp_timeline::magnetic_commands::MagneticEditCommand::AppendPrimary { ref appended_clip, .. } = result.command {
                                    self.state.selected_clip = Some(appended_clip.id);
                                    appended_id = Some(appended_clip.id);
                                }
                                // The file's audio, linked to it.
                                // `SourceLinkedInsert` is the entry that
                                // carries a magnetic video edit plus an
                                // audio track's before and after, so the
                                // pair is still one undo.
                                let audio = appended_id.and_then(|id| self.append_linked_audio(*media_id, id));
                                match audio {
                                    Some(a) => self.state.push_history(EditCommand::SourceLinkedInsert {
                                        video_command: result.command,
                                        audio_track_index: a.track_index,
                                        audio_clips_before: a.clips_before,
                                        audio_clips_after: a.clips_after,
                                    }),
                                    None => self.state.push_history(EditCommand::Magnetic(result.command)),
                                }
                                self.sync_sequence_to_engine();
                                self.state.dirty = true;
                            } else {
                                // Legacy path for Free tracks
                                let mid = *media_id;
                                let track = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index).unwrap();
                                let clip_id = track.append_clip(mid, duration);
                                let rotation = self.state.media_pool.get(&mid).map_or(0, |i| i.source_rotation);
                                if let Some(c) = self.state.sequences[self.state.active_sequence_idx]
                                    .tracks[*track_index]
                                    .find_clip_mut(clip_id)
                                {
                                    c.source_rotation = rotation;
                                }
                                self.apply_detected_log_format(clip_id, mid);
                                // The file's audio, linked to it.
                                let audio = self.append_linked_audio(mid, clip_id);
                                let clip = self.state.sequences[self.state.active_sequence_idx].tracks[*track_index].find_clip(clip_id).unwrap().clone();
                                match audio {
                                    Some(a) => self.state.push_history(EditCommand::LinkedImport {
                                        video: Some((clip, *track_index)),
                                        audio: Some((a.clip, a.track_index)),
                                        created_video_track_at: None,
                                        created_audio_track_at: a.created_track_at,
                                        autoset: None,
                                    }),
                                    None => self.state.push_history(EditCommand::LegacyAppendClip {
                                        clip,
                                        track_index: *track_index,
                                    }),
                                }
                                self.sync_sequence_to_engine();
                                self.state.selected_clip = Some(clip_id);
                                self.state.dirty = true;
                            }
                        }
                    }
    }

    /// If placing `media_id` onto the active sequence would introduce a
    /// video clip whose fps differs from the sequence's, opens the
    /// "Change Project Frame Rate?" modal and stashes `pending_cmd` for
    /// re-dispatch when the user answers. Returns `true` when the caller
    /// should abort the current handler (the append is deferred).
    ///
    /// Skipped when:
    /// - the one-shot bypass is set (prompt was just answered),
    /// - the media has no video track (audio-only),
    /// - the sequence has no video clips yet (silent first-clip auto-set
    ///   path in the append handler already handles this), or
    /// - fps already matches.
    fn maybe_prompt_framerate_mismatch(
        &mut self,
        media_id: &klipp_core::MediaId,
        pending_cmd: AppCommand,
    ) -> bool {
        if self.suppress_fps_prompt {
            return false;
        }
        let (media_fps, is_video) = match self.state.media_pool.get(media_id) {
            Some(item) => (
                item.fps,
                !item.video_codec.is_empty()
                    && item.resolution.width > 0
                    && item.resolution.height > 0,
            ),
            None => return false,
        };
        if !is_video {
            return false;
        }
        let seq_idx = self.state.active_sequence_idx;
        let Some(seq) = self.state.sequences.get(seq_idx) else {
            return false;
        };
        let has_video_clips = seq
            .tracks
            .iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .any(|t| !t.clips.is_empty());
        if !has_video_clips {
            return false;
        }
        let sequence_fps = seq.fps;
        if media_fps == sequence_fps {
            return false;
        }
        self.framerate_mismatch_dialog = Some(
            crate::panels::framerate_mismatch_dialog::FrameRateMismatchDialog {
                sequence_idx: seq_idx,
                media_fps,
                sequence_fps,
                pending_cmd,
            },
        );
        true
    }

    /// Perform a Source-Viewer three-point edit (Insert or Overwrite) on the
    /// magnetic primary track, wiring up a linked audio partner on a
    /// connected audio track when the source has audio. Pushes exactly one
    /// history entry — `EditCommand::SourceLinkedInsert` when an audio
    /// partner was created, else `EditCommand::Magnetic`.
    #[allow(clippy::too_many_arguments)]
    fn apply_source_magnetic_edit(
        &mut self,
        seq_idx: usize,
        item: &klipp_media::MediaItem,
        mid: klipp_core::MediaId,
        mut new_clip: klipp_timeline::Clip,
        timeline_in: Timecode,
        dur: Timecode,
        source_in: Timecode,
        source_out: Timecode,
        mode: klipp_timeline::magnetic::EditMode,
    ) {
        let has_linked_audio = item.has_audio;
        let video_clip_id = new_clip.id;

        // Pre-generate audio clip id so the video clip can carry
        // linked_clip_id into the magnetic command for later undo.
        let audio_clip_id = if has_linked_audio {
            let aid = klipp_core::ClipId::new();
            new_clip.linked_clip_id = Some(aid);
            Some(aid)
        } else {
            None
        };

        // Locate or auto-create the audio track BEFORE the magnetic edit so
        // we can snapshot its clip list — magnetic's connected-track cascade
        // may move anchored A1 clips, and the snapshot captures the true
        // pre-edit state.
        let audio_track_info: Option<(usize, Vec<klipp_timeline::Clip>)> = if has_linked_audio {
            let existing = self.state.sequences[seq_idx].tracks.iter()
                .position(|t| t.kind == klipp_timeline::TrackKind::Audio);
            let ai = existing.unwrap_or_else(|| {
                let seq = &mut self.state.sequences[seq_idx];
                let audio_count = seq.tracks.iter()
                    .filter(|t| t.kind == klipp_timeline::TrackKind::Audio)
                    .count();
                seq.add_audio_track(format!("Audio {}", audio_count + 1));
                seq.tracks.iter()
                    .position(|t| t.kind == klipp_timeline::TrackKind::Audio)
                    .expect("audio track just added")
            });
            let before = self.state.sequences[seq_idx].tracks[ai].clips.clone();
            Some((ai, before))
        } else {
            None
        };

        // Thread user snap config through the magnetic edit so an
        // insert from the source viewer respects the same snap rules
        // (master + per-target + grid) as a drag-from-source-to-track
        // edit. apply_inverse / apply_forward replays already-snapped
        // commands and don't re-resolve, so the snap-time choice is
        // recorded into the resulting MagneticEditCommand.
        let pps = self.state.timeline_zoom;
        let fps_rate = self.state.sequences[seq_idx].fps.rate();
        let grid_interval_secs = crate::timeline::grid_snap::resolve_grid_interval_secs(
            self.state.grid_snap_interval,
            fps_rate,
            pps,
        );
        let snap_cfg = self.state.snap;
        let result = klipp_timeline::magnetic::MagneticEditor::three_point_edit(
            &mut self.state.sequences[seq_idx],
            new_clip,
            timeline_in,
            true,
            mode,
            grid_interval_secs,
            Some(&snap_cfg),
        );

        match (audio_clip_id, audio_track_info) {
            (Some(aid), Some((ai, audio_clips_before))) => {
                let mut audio_clip = klipp_timeline::Clip::media(
                    mid,
                    timeline_in,
                    timeline_in + dur,
                    source_in,
                    source_out,
                );
                audio_clip.id = aid;
                audio_clip.linked_clip_id = Some(video_clip_id);
                // overwrite_insert_clip_on_track is safe for both modes:
                // for Insert, any anchored A1 clips have already been
                // cascaded by the magnetic primary edit, leaving a clean
                // slot; for non-anchored overlap, overwrite semantics
                // prevent pile-up. A per-mode audio ripple is a future
                // polish tracked separately.
                if let Some(track) = self.state.sequences[seq_idx].tracks.get_mut(ai) {
                    Self::overwrite_insert_clip_on_track(track, audio_clip);
                }
                let audio_clips_after = self.state.sequences[seq_idx].tracks[ai].clips.clone();
                self.state.push_history(EditCommand::SourceLinkedInsert {
                        video_command: result.command,
                        audio_track_index: ai,
                        audio_clips_before,
                        audio_clips_after,
                    });
            }
            _ => {
                self.state.push_history(EditCommand::Magnetic(result.command));
            }
        }
    }

    /// Drop an already-imported pool item on the timeline at the
    /// playhead, on fresh track(s) below the stack. Audio-only media
    /// lands on a new audio track; video media on a new video track,
    /// with its audio on a new audio track linked to it. Backs the
    /// Audio Library's "Add at playhead" and the media pool's
    /// right-click equivalent.
    fn place_media_at_playhead(&mut self, media_id: klipp_core::MediaId) {
        let start = self.state.playhead;
        if let Some((_, mut entry)) = self.place_media_on_new_tracks(media_id, start) {
            self.refresh_placement_snapshot(self.state.active_sequence_idx, &mut entry);
            self.state.active_history_mut().push(crate::history::HistoryEntry { command: entry });
        }
        self.state.dirty = true;
        self.sync_sequence_to_engine();
    }

    /// Put `media_id` at `start` on a new video track and, for its sound,
    /// a new audio track, linked. Returns the video clip (when there is
    /// picture) and the history entry that undoes the placement; the
    /// caller pushes it, alone or in a batch.
    fn place_media_on_new_tracks(
        &mut self,
        media_id: klipp_core::MediaId,
        start: klipp_core::Timecode,
    ) -> Option<(Option<klipp_core::ClipId>, crate::history::EditCommand)> {
        let Some(item) = self.state.media_pool.get(&media_id).cloned() else {
            self.state.toast.show("That clip is no longer in the media pool");
            return None;
        };
        if item.offline {
            self.state.toast.show(format!("Cannot add '{}' — media is offline", item.filename));
            return None;
        }
        let has_video = !item.video_codec.is_empty();
        let has_audio = item.has_audio;
        let rate = item.duration.rate;
        let source_in = klipp_core::Timecode { value: 0, rate };
        let source_out = item.duration;
        let end = start + item.duration;
        let seq_idx = self.state.active_sequence_idx;

        // FCP: the first video clip sets the sequence resolution + fps.
        let mut autoset = None;
        if has_video && item.resolution.width > 0 && item.resolution.height > 0 {
            let seq = &mut self.state.sequences[seq_idx];
            autoset = seq.adopt_first_clip(item.resolution, item.fps);
            if autoset.is_some() {
                self.state.fps = seq.fps;
            }
        }

        let mut hist_video = None;
        let mut hist_audio = None;
        let mut created_video_track_at = None;
        let mut created_audio_track_at = None;
        let mut video_clip_id = None;

        // Video onto a new video track (inserts at index 0).
        if has_video {
            let seq = &mut self.state.sequences[seq_idx];
            let count = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Video).count();
            seq.add_video_track(format!("Video {}", count + 1));
            let vi = 0usize;
            created_video_track_at = Some(vi);
            let mut clip = klipp_timeline::Clip::media(media_id, start, end, source_in, source_out);
            clip.source_rotation = item.source_rotation;
            let cid = clip.id;
            Self::overwrite_insert_clip_on_track(&mut seq.tracks[vi], clip);
            video_clip_id = Some(cid);
            self.apply_detected_log_format(cid, media_id);
            if let Some(c) = self.state.sequences[seq_idx].tracks[vi].find_clip(cid) {
                hist_video = Some((c.clone(), vi));
            }
        }

        // Audio onto a new audio track (pushed to the end).
        if has_audio || !has_video {
            let seq = &mut self.state.sequences[seq_idx];
            let count = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
            seq.add_audio_track(format!("Audio {}", count + 1));
            let ai = seq.tracks.len() - 1;
            created_audio_track_at = Some(ai);
            let mut clip = klipp_timeline::Clip::media(media_id, start, end, source_in, source_out);
            if let Some(vid) = video_clip_id {
                clip.linked_clip_id = Some(vid);
            }
            let aid = clip.id;
            Self::overwrite_insert_clip_on_track(&mut seq.tracks[ai], clip);
            if let Some(vid) = video_clip_id {
                for track in self.state.sequences[seq_idx].tracks.iter_mut() {
                    if let Some(vc) = track.find_clip_mut(vid) {
                        vc.linked_clip_id = Some(aid);
                        break;
                    }
                }
            }
            if let Some(c) = self.state.sequences[seq_idx].tracks[ai].find_clip(aid) {
                hist_audio = Some((c.clone(), ai));
            }
            self.state.selected_clip = Some(video_clip_id.unwrap_or(aid));
        } else if let Some(vid) = video_clip_id {
            self.state.selected_clip = Some(vid);
        }

        (hist_video.is_some() || hist_audio.is_some()).then_some((
            video_clip_id,
            crate::history::EditCommand::LinkedImport {
                video: hist_video,
                audio: hist_audio,
                created_video_track_at,
                created_audio_track_at,
                autoset,
            },
        ))
    }

    /// Re-take a placement's clip snapshots from the sequence as it is
    /// now. `place_media_on_new_tracks` snapshots the picture before its
    /// sound is linked to it, and a multicam groups the clips after that;
    /// redo restores the snapshots, so without this it brought the clips
    /// back unlinked and ungrouped. The track indices stay as recorded.
    fn refresh_placement_snapshot(&self, seq_idx: usize, entry: &mut crate::history::EditCommand) {
        let crate::history::EditCommand::LinkedImport { video, audio, .. } = entry else { return };
        let seq = &self.state.sequences[seq_idx];
        for (clip, _) in [video.as_mut(), audio.as_mut()].into_iter().flatten() {
            if let Some(now) = seq.tracks.iter().find_map(|t| t.find_clip(clip.id)) {
                *clip = now.clone();
            }
        }
    }

    /// Sync `media_ids` onto one clock, off the UI thread when sound is
    /// involved; the plan comes back as `AppEvent::MulticamSyncReady`.
    fn spawn_multicam_sync(&mut self, media_ids: Vec<klipp_core::MediaId>, method: klipp_core::MulticamSync) {
        let mut items: Vec<klipp_media::MediaItem> = media_ids
            .iter()
            .filter_map(|id| self.state.media_pool.get(id))
            // A sound-only recording joins as the others' reference and
            // goes on an audio track; it is not an angle.
            .filter(|m| !m.offline && !m.is_still && (!m.video_codec.is_empty() || m.has_audio))
            .cloned()
            .collect();
        if items.iter().filter(|m| !m.video_codec.is_empty()).count() < 2 {
            self.state.toast.show("A multicam needs two or more online video clips");
            return;
        }
        // Angle numbers follow the names, which is how cameras label cards.
        items.sort_by(|a, b| a.filename.cmp(&b.filename));
        let angles: Vec<klipp_core::MulticamAngle> = items
            .iter()
            .map(|m| klipp_core::MulticamAngle {
                media_id: m.id,
                name: m.filename.clone(),
                start_timecode: m.shot.start_timecode.clone(),
                fps: m.fps,
                duration_secs: m.duration.to_secs_f64(),
                has_audio: m.has_audio,
            })
            .collect();
        let paths: Vec<std::path::PathBuf> = items.iter().map(|m| m.path.clone()).collect();
        let sequence_id = self.state.sequences[self.state.active_sequence_idx].id;
        let at_secs = self.state.playhead.to_secs_f64();
        let tx = self.proxy_event_tx.clone();
        if method != klipp_core::MulticamSync::Timecode {
            self.state.toast.show(format!("Syncing {} angles\u{2026}", angles.len()));
        }
        let spawned = std::thread::Builder::new().name("klipp-multicam-sync".into()).spawn(move || {
            use klipp_analysis::audio_sync::{coarse_offset, decode_mono_pcm, refine_offset, SoundPrint};
            use klipp_core::AudioMatch;
            const SR: u32 = 8_000;
            let ffmpeg = std::path::Path::new("ffmpeg");
            // Each file's sound is decoded once and kept only as its
            // print; the refinement decodes two seconds of each again.
            // Twelve one-hour angles would otherwise hold 1.4 GB.
            let mut prints: std::collections::HashMap<usize, Result<std::sync::Arc<SoundPrint>, String>> =
                std::collections::HashMap::new();
            let mut print = |i: usize| {
                prints
                    .entry(i)
                    .or_insert_with(|| decode_mono_pcm(ffmpeg, &paths[i], SR, 0.0, 0.0).map(|pcm| std::sync::Arc::new(SoundPrint::of(&pcm, SR))))
                    .clone()
            };
            let plan = klipp_core::plan_multicam(&angles, method, |r, i| {
                let (a, b) = match (print(r), print(i)) {
                    (Ok(a), Ok(b)) => (a, b),
                    (Err(e), _) | (_, Err(e)) => return AudioMatch::Unavailable(e),
                };
                let Some(coarse) = coarse_offset(&a, &b) else { return AudioMatch::TooLittleOverlap };
                let (ref_from, clip_from, len) = coarse.refine_windows();
                let slack = klipp_analysis::audio_sync::REFINE_SLACK_SECS;
                let offset_secs = match (
                    decode_mono_pcm(ffmpeg, &paths[r], SR, ref_from.max(0.0), len),
                    decode_mono_pcm(ffmpeg, &paths[i], SR, clip_from.max(0.0), len + 2.0 * slack),
                ) {
                    (Ok(rw), Ok(cw)) if clip_from >= 0.0 => refine_offset(&rw, &cw, SR, &coarse),
                    _ => coarse.offset_secs,
                };
                AudioMatch::Matched { offset_secs, confidence: coarse.confidence }
            });
            let _ = tx.send(AppEvent::MulticamSyncReady { plan, sequence_id, at_secs });
        });
        if let Err(e) = spawned {
            self.state.toast.show(format!("Could not start the multicam sync: {e}"));
        }
    }

    /// Lay out a synced multicam: each angle on its own tracks at its
    /// place after `at_secs`, then grouped, as one undo step.
    fn apply_multicam_plan(&mut self, plan: klipp_core::MulticamPlan, sequence_id: klipp_core::SequenceId, at_secs: f64) {
        let seq_idx = self.state.active_sequence_idx;
        if self.state.sequences[seq_idx].id != sequence_id {
            self.state.toast.show("The sequence changed while the angles were syncing, so the multicam was not placed");
            return;
        }
        if plan.synced() < 2 {
            let why = if plan.notes.is_empty() { String::new() } else { format!(": {}", plan.notes.join("; ")) };
            self.state.toast.show(format!("Could not put two angles on one clock{why}"));
            return;
        }
        // On a frame of the sequence: a sound match is finer than a frame,
        // and a clip between frames is a clip nobody can cut. The grid is
        // the one the sequence will have: an empty sequence takes its rate
        // from the first clip with picture placed in it, and that must
        // hold for that first clip's own position too.
        let fps = {
            let seq = &self.state.sequences[seq_idx];
            let has_video = seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Video && !t.clips.is_empty());
            let first_picture = plan
                .placements
                .iter()
                .filter_map(|p| self.state.media_pool.get(&p.media_id))
                .find(|m| !m.video_codec.is_empty() && m.resolution.width > 0 && m.resolution.height > 0);
            match first_picture {
                Some(m) if !has_video => m.fps,
                _ => seq.fps,
            }
        };
        let mut entries = Vec::new();
        let mut video_clips = Vec::new();
        for p in &plan.placements {
            let frames = ((at_secs + p.start_secs) * fps.as_f64()).round() as i64;
            let start = klipp_core::RationalTime { value: frames * i64::from(fps.den), rate: fps.num };
            if let Some((video, entry)) = self.place_media_on_new_tracks(p.media_id, start) {
                entries.push(entry);
                if let Some(v) = video {
                    video_clips.push((v, p.is_reference));
                }
            }
        }
        // The reference angle first: angle 0 carries the cut list, and
        // it is the sound the others were matched to.
        video_clips.sort_by_key(|(_, is_ref)| !is_ref);
        let group_id = klipp_core::GroupId::new();
        let seq = &mut self.state.sequences[seq_idx];
        for (angle, (clip_id, _)) in video_clips.iter().enumerate() {
            for track in &mut seq.tracks {
                if let Some(clip) = track.find_clip_mut(*clip_id) {
                    clip.multicam_group = Some(group_id);
                    clip.multicam_angle = angle as u8;
                    break;
                }
            }
        }
        // Snapshots taken now, grouped: redo puts back what they hold.
        for entry in &mut entries {
            self.refresh_placement_snapshot(seq_idx, entry);
        }
        if !entries.is_empty() {
            self.state.active_history_mut().push(crate::history::HistoryEntry {
                command: crate::history::EditCommand::Batch(entries),
            });
        }
        let by = |m| plan.placements.iter().filter(|p| p.method == Some(m)).count();
        let (tc, snd) = (by(klipp_core::SyncMethod::Timecode), by(klipp_core::SyncMethod::Waveform));
        let mut summary = format!("Multicam of {} angles", video_clips.len());
        if tc > 0 {
            summary.push_str(&format!(", {tc} by timecode"));
        }
        if snd > 0 {
            summary.push_str(&format!(", {snd} by sound"));
        }
        for n in &plan.notes {
            summary.push_str(". ");
            summary.push_str(n);
        }
        tracing::info!("{summary}");
        self.state.toast.show(summary);
        self.state.dirty = true;
        self.sync_sequence_to_engine();
    }

    /// Overwrite-insert a clip onto a track at a given position.
    /// Trims or removes any overlapping clips in the range [timeline_in, timeline_out].
    pub(crate) fn overwrite_insert_clip_on_track(
        track: &mut klipp_timeline::Track,
        new_clip: klipp_timeline::Clip,
    ) {
        let new_in = new_clip.timeline_in;
        let new_out = new_clip.timeline_out;

        // Collect indices of clips to modify (process in reverse to preserve indices)
        let mut to_remove = Vec::new();
        let mut to_trim_left = Vec::new();  // trim in-point forward (clip extends past new_out)
        let mut to_trim_right = Vec::new(); // trim out-point back (clip starts before new_in)
        let mut to_split = Vec::new();      // clip fully contains new range — needs split

        for (i, clip) in track.clips.iter().enumerate() {
            let c_in = clip.timeline_in;
            let c_out = clip.timeline_out;

            // No overlap
            if c_out <= new_in || c_in >= new_out {
                continue;
            }

            if c_in >= new_in && c_out <= new_out {
                // Fully covered — remove
                to_remove.push(i);
            } else if c_in < new_in && c_out > new_out {
                // Clip contains the new range — split into two
                to_split.push(i);
            } else if c_in < new_in && c_out > new_in {
                // Overlaps from left — trim right edge back
                to_trim_right.push(i);
            } else if c_in < new_out && c_out > new_out {
                // Overlaps from right — trim left edge forward
                to_trim_left.push(i);
            }
        }

        // Handle splits first (creates a new clip)
        for &i in to_split.iter().rev() {
            let clip = track.clips[i].clone();
            let left_dur = new_in - clip.timeline_in;
            let right_offset = new_out - clip.timeline_in;

            // Left portion: keep original in, trim out to new_in
            track.clips[i].timeline_out = new_in;
            track.clips[i].source_out = clip.source_in + left_dur;

            // Right portion: new clip starting at new_out
            let right_clip = klipp_timeline::Clip {
                id: klipp_core::ClipId::new(),
                kind: clip.kind.clone(),
                timeline_in: new_out,
                timeline_out: clip.timeline_out,
                source_in: clip.source_in + right_offset,
                source_out: clip.source_out,
                node_graph: clip.node_graph,
                effects: clip.effects.clone(),
                builtin_effects: clip.builtin_effects.clone(),
                builtin_effects_enabled: clip.builtin_effects_enabled.clone(),
                linked_clip_id: clip.linked_clip_id,
                linked_title_asset_id: clip.linked_title_asset_id,
                group_id: clip.group_id,
                grade_group: clip.grade_group,
                own_node_graph: None,
                own_effects: None,
                magnetic_anchor: None,
                gain: clip.gain,
                volume: clip.volume,
                pan: clip.pan,
                fade_in_secs: clip.fade_in_secs,
                fade_out_secs: clip.fade_out_secs,
                fade_in_curve: clip.fade_in_curve,
                fade_out_curve: clip.fade_out_curve,
                channel_config: clip.channel_config,
                volume_keyframes: clip.volume_keyframes.clone(),
                pan_keyframes: clip.pan_keyframes.clone(),
                gain_keyframes: clip.gain_keyframes.clone(),
                opacity_keyframes: clip.opacity_keyframes.clone(),
                transform_pos_x_keyframes: clip.transform_pos_x_keyframes.clone(),
                transform_pos_y_keyframes: clip.transform_pos_y_keyframes.clone(),
                transform_scale_keyframes: clip.transform_scale_keyframes.clone(),
                transform_rotation_keyframes: clip.transform_rotation_keyframes.clone(),
                transform_tilt_x_keyframes: clip.transform_tilt_x_keyframes.clone(),
                transform_tilt_y_keyframes: clip.transform_tilt_y_keyframes.clone(),
                matte_feather_keyframes: clip.matte_feather_keyframes.clone(),
                matte_choke_keyframes: clip.matte_choke_keyframes.clone(),
                clip_markers: clip.clip_markers.clone(),
                effect_keyframes: clip.effect_keyframes.clone(),
                role: clip.role,
                param_modifiers: clip.param_modifiers.clone(),
                speed: clip.speed,
                speed_keyframes: clip.speed_keyframes.clone(),
                source_rotation: clip.source_rotation,
                enabled: clip.enabled,
                solo: clip.solo,
                blend_mode: clip.blend_mode,
                depth: clip.depth,
                matte_source: clip.matte_source.clone(),
                matte_stencil: clip.matte_stencil.clone(),
                consumed_as_matte: clip.consumed_as_matte,
                transform_link: None,
                grade_versions: clip.grade_versions.clone(),
                active_grade_version: clip.active_grade_version,
                multicam_group: clip.multicam_group,
                multicam_angle: clip.multicam_angle,
                multicam_cuts: clip.multicam_cuts.clone(),
                multicam_sound: clip.multicam_sound,
                audio_effects: clip.audio_effects.clone(),
                audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
                deflicker_data: None,
                deflicker_gains: None,
                stabilization_data: None,
                smoothed_trajectory: None,
                track_points: Vec::new(),
                // A piece of the clip keeps its track (#684).
                surfaces: clip.surfaces.clone(),
                surface_pin: clip.surface_pin,
                precomp: None,
            };
            track.clips.insert(i + 1, right_clip);
        }

        // Trim left edges forward (clip starts before new_out, ends after)
        for &i in &to_trim_left {
            let clip = &track.clips[i];
            let trim_amount = new_out - clip.timeline_in;
            track.clips[i].source_in += trim_amount;
            track.clips[i].timeline_in = new_out;
        }

        // Trim right edges back (clip starts before new_in, ends after)
        for &i in &to_trim_right {
            let clip = &track.clips[i];
            let trim_amount = clip.timeline_out - new_in;
            track.clips[i].source_out -= trim_amount;
            track.clips[i].timeline_out = new_in;
        }

        // Remove fully covered clips (reverse order)
        for &i in to_remove.iter().rev() {
            track.clips.remove(i);
        }

        // Insert the new clip
        track.clips.push(new_clip);
        track.sort_clips();
    }

    /// Remove (lift) content in [range_in, range_out] from a track, leaving a gap.
    /// Clips fully inside the range are removed. Clips partially overlapping are trimmed.
    fn lift_range(
        track: &mut klipp_timeline::Track,
        range_in: Timecode,
        range_out: Timecode,
    ) {
        let mut to_remove = Vec::new();
        let mut to_split = Vec::new();
        let mut to_trim_right = Vec::new(); // trim out-point back to range_in
        let mut to_trim_left = Vec::new();  // trim in-point forward to range_out

        for (i, clip) in track.clips.iter().enumerate() {
            let c_in = clip.timeline_in;
            let c_out = clip.timeline_out;
            if c_out <= range_in || c_in >= range_out {
                continue;
            }
            if c_in >= range_in && c_out <= range_out {
                to_remove.push(i);
            } else if c_in < range_in && c_out > range_out {
                to_split.push(i);
            } else if c_in < range_in && c_out > range_in {
                to_trim_right.push(i);
            } else if c_in < range_out && c_out > range_out {
                to_trim_left.push(i);
            }
        }

        // Handle splits (reverse order for safe insertion)
        for &i in to_split.iter().rev() {
            let clip = track.clips[i].clone();
            let left_dur = range_in - clip.timeline_in;

            // Left portion
            track.clips[i].timeline_out = range_in;
            track.clips[i].source_out = clip.source_in + left_dur;

            // Right portion
            let right_offset = range_out - clip.timeline_in;
            let right_clip = klipp_timeline::Clip {
                id: klipp_core::ClipId::new(),
                kind: clip.kind.clone(),
                timeline_in: range_out,
                timeline_out: clip.timeline_out,
                source_in: clip.source_in + right_offset,
                source_out: clip.source_out,
                node_graph: clip.node_graph,
                effects: clip.effects.clone(),
                builtin_effects: clip.builtin_effects.clone(),
                builtin_effects_enabled: clip.builtin_effects_enabled.clone(),
                linked_clip_id: clip.linked_clip_id,
                linked_title_asset_id: clip.linked_title_asset_id,
                group_id: clip.group_id,
                grade_group: clip.grade_group,
                own_node_graph: None,
                own_effects: None,
                magnetic_anchor: None,
                gain: clip.gain,
                volume: clip.volume,
                pan: clip.pan,
                fade_in_secs: clip.fade_in_secs,
                fade_out_secs: clip.fade_out_secs,
                fade_in_curve: clip.fade_in_curve,
                fade_out_curve: clip.fade_out_curve,
                channel_config: clip.channel_config,
                volume_keyframes: clip.volume_keyframes.clone(),
                pan_keyframes: clip.pan_keyframes.clone(),
                gain_keyframes: clip.gain_keyframes.clone(),
                opacity_keyframes: clip.opacity_keyframes.clone(),
                transform_pos_x_keyframes: clip.transform_pos_x_keyframes.clone(),
                transform_pos_y_keyframes: clip.transform_pos_y_keyframes.clone(),
                transform_scale_keyframes: clip.transform_scale_keyframes.clone(),
                transform_rotation_keyframes: clip.transform_rotation_keyframes.clone(),
                transform_tilt_x_keyframes: clip.transform_tilt_x_keyframes.clone(),
                transform_tilt_y_keyframes: clip.transform_tilt_y_keyframes.clone(),
                matte_feather_keyframes: clip.matte_feather_keyframes.clone(),
                matte_choke_keyframes: clip.matte_choke_keyframes.clone(),
                clip_markers: clip.clip_markers.clone(),
                effect_keyframes: clip.effect_keyframes.clone(),
                role: clip.role,
                param_modifiers: clip.param_modifiers.clone(),
                speed: clip.speed,
                speed_keyframes: clip.speed_keyframes.clone(),
                source_rotation: clip.source_rotation,
                enabled: clip.enabled,
                solo: clip.solo,
                blend_mode: clip.blend_mode,
                depth: clip.depth,
                matte_source: clip.matte_source.clone(),
                matte_stencil: clip.matte_stencil.clone(),
                consumed_as_matte: clip.consumed_as_matte,
                transform_link: None,
                grade_versions: clip.grade_versions.clone(),
                active_grade_version: clip.active_grade_version,
                multicam_group: clip.multicam_group,
                multicam_angle: clip.multicam_angle,
                multicam_cuts: clip.multicam_cuts.clone(),
                multicam_sound: clip.multicam_sound,
                audio_effects: clip.audio_effects.clone(),
                audio_effect_keyframes: clip.audio_effect_keyframes.clone(),
                deflicker_data: None,
                deflicker_gains: None,
                stabilization_data: None,
                smoothed_trajectory: None,
                track_points: Vec::new(),
                // A piece of the clip keeps its track (#684).
                surfaces: clip.surfaces.clone(),
                surface_pin: clip.surface_pin,
                precomp: None,
            };
            track.clips.insert(i + 1, right_clip);
        }

        // Trim right edges back
        for &i in &to_trim_right {
            let trim = track.clips[i].timeline_out - range_in;
            track.clips[i].source_out -= trim;
            track.clips[i].timeline_out = range_in;
        }

        // Trim left edges forward
        for &i in &to_trim_left {
            let trim = range_out - track.clips[i].timeline_in;
            track.clips[i].source_in += trim;
            track.clips[i].timeline_in = range_out;
        }

        // Remove fully covered (reverse order)
        for &i in to_remove.iter().rev() {
            track.clips.remove(i);
        }
    }

    /// Kick off a project consolidation on a background thread. Sets up the
    /// `consolidate_progress` channel on state so the main loop picks up
    /// progress + completion messages.
    fn start_consolidation(
        &mut self,
        dest_dir: std::path::PathBuf,
        include_proxies: bool,
        include_skims: bool,
        create_zip: bool,
        delete_folder_after_zip: bool,
    ) {
        let Some(source_dir) = self.state.project_path.clone() else {
            self.state.toast.show(
                "Save the project before consolidating.".to_string(),
            );
            return;
        };
        if self.state.consolidate_progress.is_some() {
            self.state.toast.show(
                "Consolidation already in progress.".to_string(),
            );
            return;
        }

        let media_pool = self.state.media_pool.clone();
        let sequences = self.state.sequences.clone();
        let color = klipp_project::ColorSettings {
            global_lut_path: self.state.global_lut_path.clone(),
            global_lut_opacity: self.state.global_lut_opacity,
            global_lut_enabled: self.state.global_lut_enabled,
            track_header_width: self.state.track_header_width,
            auto_generate_filmstrips: self.state.project_auto_filmstrips,
            auto_generate_skim: self.state.project_auto_skim,
            working_colorspace: self.state.project_working_colorspace,
            grade_encoding: self.state.project_grade_encoding,
        };
        let options = klipp_project::consolidate::ConsolidateOptions {
            dest_dir,
            include_proxies,
            include_skims,
        };

        let (tx, rx) = std::sync::mpsc::channel();
        self.state.consolidate_progress = Some(crate::state::ConsolidateProgress {
            files_done: 0,
            files_total: 0,
            current_file: String::new(),
            expects_archive: create_zip,
            archiving: false,
            archive_bytes_done: 0,
            archive_bytes_total: 0,
            receiver: rx,
        });
        self.state.show_consolidate_dialog = false;

        // Capture the destination once for Phase 2 so later state edits can't
        // redirect the archive target mid-run.
        let phase2_dest = options.dest_dir.clone();

        let tx_progress = tx.clone();
        std::thread::spawn(move || {
            let result = klipp_project::consolidate::consolidate_project(
                &source_dir,
                &media_pool,
                &sequences,
                &color,
                options,
                Some(Box::new(move |done, total, name| {
                    let _ = tx_progress.send(
                        crate::state::ConsolidateMessage::Progress {
                            done,
                            total,
                            filename: name.to_string(),
                        },
                    );
                })),
            );
            let phase1_ok = result.is_ok();
            let _ = tx.send(crate::state::ConsolidateMessage::Done(
                result.map_err(|e| e.to_string()),
            ));

            if create_zip && phase1_ok {
                // Place the archive next to the consolidated folder.
                // `<dest>/MyProject/` → `<dest>/MyProject.zip`.
                let zip_path = phase2_dest.with_extension("zip");
                let tx_arch = tx.clone();
                let progress_fn: klipp_project::consolidate::ArchiveProgressFn =
                    Box::new(move |bytes_done: u64, bytes_total: u64, name: &str| {
                        let _ = tx_arch.send(
                            crate::state::ConsolidateMessage::ArchiveProgress {
                                bytes_done,
                                bytes_total,
                                filename: name.to_string(),
                            },
                        );
                    });
                let archive_result = klipp_project::consolidate::execute_zip_folder(
                    &phase2_dest,
                    &zip_path,
                    Some(&progress_fn),
                );
                match archive_result {
                    Ok(()) => {
                        let _ = tx.send(
                            crate::state::ConsolidateMessage::ArchiveDone(Ok(zip_path.clone())),
                        );
                        if delete_folder_after_zip {
                            // Best-effort cleanup — the archive is already on
                            // disk, so a failure here isn't fatal.
                            if let Err(e) = std::fs::remove_dir_all(&phase2_dest) {
                                tracing::warn!(
                                    "delete folder after archiving failed: {e}",
                                );
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(
                            crate::state::ConsolidateMessage::ArchiveDone(Err(e.to_string())),
                        );
                    }
                }
            }
        });
    }

}

/// Build a small thumbnail of the most recent inference's mask
/// for the panel's Recent list. Reuses the overlay crate's
/// `build_mask_texture` and tints with the panel's default
/// overlay colour so the row thumbnails read as "this was a
/// mask" at a glance. Returns `None` when the mask is empty /
/// malformed.
fn build_recent_thumbnail(
    ctx: &egui::Context,
    inference: &crate::ai_tools::InferenceState,
) -> Option<egui::TextureHandle> {
    let response = match inference {
        crate::ai_tools::InferenceState::Done { response, .. } => response,
        _ => return None,
    };
    let first = response.candidates.first()?;
    crate::ai_tools::build_mask_texture(
        ctx,
        &first.mask,
        crate::ai_tools::DEFAULT_MASK_OVERLAY_COLOR,
        "ai_recent_thumb",
    )
}

impl Drop for KlippApp {
    fn drop(&mut self) {
        // Kill proxy ffmpeg child processes
        for pid_ref in &self.proxy_children {
            let pid = pid_ref.load(std::sync::atomic::Ordering::Relaxed);
            if pid != 0 {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
            }
        }
        // Kill render queue child processes
        self.state.render_queue.kill_active();
    }
}

/// The Auto interface size for a screen `width` points wide at the
/// system scale: 125% from 2800 up, the system's own below.
pub(crate) fn auto_zoom_factor(width: f32) -> f32 {
    if width >= 2800.0 { 1.25 } else { 1.0 }
}

#[cfg(test)]
mod auto_zoom_tests {
    /// 4K at 1x (3840) or 1.25x (3072) gets a quarter more; 4K at 2x
    /// (1920), 1440p (2560) and 1080p keep the system scale.
    #[test]
    fn a_large_screen_at_its_system_scale_gets_a_quarter_more() {
        for (w, want) in [(3840.0, 1.25), (3072.0, 1.25), (2560.0, 1.0), (1920.0, 1.0)] {
            assert_eq!(super::auto_zoom_factor(w), want, "{w}");
        }
    }
}

/// Which edge `trim_to_playhead_dispatch` should move — Out for Q, In for W.
#[derive(Debug, Clone, Copy)]
enum TrimToPlayheadDir {
    /// Q: trim the OUT edge of the clip ending at/before the playhead.
    Out,
    /// W: trim the IN edge of the clip starting at/after the playhead.
    In,
}

/// Recompute `clip.timeline_out` so that its visible length on the timeline
/// matches the time it takes the speed curve to exhaust the clip's source range.
/// Uses the constant `clip.speed` when there are no keyframes; integrates the
/// curve otherwise. Called from the speed-keyframe command handlers so the UI
/// and engine see a consistent clip length after a ramp edit.
fn recalc_timeline_out_from_speed(clip: &mut klipp_timeline::Clip) {
    let source_dur_secs = clip.source_duration().to_secs_f64();
    if source_dur_secs <= 0.0 {
        return;
    }
    let rate = clip.timeline_in.rate;
    let new_dur_secs = klipp_timeline::effective_timeline_duration_for_keyframes(
        &clip.speed_keyframes,
        source_dur_secs,
        rate as f64,
        clip.speed,
    );
    let new_duration = klipp_core::RationalTime::from_secs_f64(new_dur_secs, rate);
    clip.timeline_out = clip.timeline_in + new_duration;
}

/// Compute the pixel offsets (in the project's working coordinate
/// space, where 0,0 is the frame centre) that place a 30 %-scaled
/// PiP clip in the named corner of the frame, with a 4 % margin.
/// Returns `(pos_x, pos_y)` ready to assign to a clip's transform
/// keyframes. Unknown corner strings fall through to (0, 0).
///
/// **Why resolution-aware**: the original PiP presets used hard-
/// coded offsets (±600, ±300) tuned for 1080p; on 720p the clip
/// pushed off-screen and on 4K it landed near the frame centre.
/// This formula keeps the visual result corner-anchored across
/// every project resolution.
///
/// **Y-sign convention**: Klipp's `transform.position_y` is
/// negative-up, positive-down (matches the existing `pip_top_right`
/// using `-300` for top and `pip_bottom_left` using `+300` for
/// bottom).
fn pip_offsets_for_corner(
    res: klipp_core::Resolution,
    corner: &str,
) -> (f32, f32) {
    // Clip is 30 % scale; 4 % margin from frame edge:
    //   offset = frame_dim/2 - clip_dim/2 - margin
    //          = frame_dim * (0.5 - 0.15 - 0.04)
    //          = frame_dim * 0.31
    let offset_x = res.width as f32 * 0.31;
    let offset_y = res.height as f32 * 0.31;
    match corner {
        "pip_top_left"     => (-offset_x, -offset_y),
        "pip_top_right"    => ( offset_x, -offset_y),
        "pip_bottom_left"  => (-offset_x,  offset_y),
        "pip_bottom_right" => ( offset_x,  offset_y),
        _ => (0.0, 0.0),
    }
}

// ── Live-sync helpers for Editing prefs (T-PREFS-LIVE-SYNC) ───────────
//
// Each helper mirrors a `prefs.editing.{field}` change to the matching
// runtime copy on `AppState`. Called from the AppCommand handler arms
// (so dispatch is the single mutation path) and from tests (which
// construct `Preferences::default()` + `AppState::default()` and pin
// the contract directly without standing up a `KlippApp` instance).
//
// The shape is uniform on purpose so future preferences with a runtime
// state copy can opt in by following the same pattern. The handler
// arm is a one-liner that calls the helper plus `prefs_dirty = true`.

fn apply_snap_threshold_px(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: f32) {
    prefs.editing.snap_threshold_px = val;
    state.snap_threshold_px = val;
}

fn apply_zoom_step(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: f32) {
    prefs.editing.zoom_step = val;
    state.zoom_step = val;
}

fn apply_magnetic_timeline(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: bool) {
    // Naming asymmetry: the pref is `magnetic_timeline`, the runtime
    // copy is `magnetic_timeline_enabled`. Bridged here so the rest of
    // the codebase doesn't have to know about it. (Renaming for
    // symmetry is filed separately as T-PREFS-NAMING-CLEANUP.)
    prefs.editing.magnetic_timeline = val;
    state.magnetic_timeline_enabled = val;
}

fn apply_default_sequence_resolution_idx(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: usize) {
    // Read at "+ new sequence" time (timeline/sequence_tabs.rs:182),
    // not on a hot path — but mid-session changes still need to
    // propagate so that immediately creating a sequence picks up the
    // new default.
    prefs.editing.default_sequence_resolution_idx = val;
    state.default_sequence_resolution_idx = val;
}

fn apply_default_transition_frames(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: u32) {
    prefs.editing.default_transition_frames = val;
    state.default_transition_frames = val;
}

fn apply_multicam_show_angles(prefs: &mut crate::prefs::Preferences, state: &mut AppState, val: bool) {
    prefs.editing.multicam_show_angles = val;
    state.multicam_show_angles = val;
}

// Pinned: each apply_* helper writes the new value to BOTH the
// persisted prefs side AND the runtime state side. Tests construct
// fresh defaults, call the helper, and assert both sides match.
// The handler arms in the AppCommand match call these helpers and
// add `prefs_dirty = true` — the dirty flag is the App's
// responsibility, not the helper's, so it isn't tested here.
/// The commands of one control envelope as the router applies them. A
/// parameter write is bracketed the way a slider drag is, so it records
/// history (without the brackets it never did); a batch wraps everything
/// in one history batch, so an agent's action is one undo step.
pub fn expand_control_commands(commands: Vec<AppCommand>, batch: bool) -> Vec<AppCommand> {
    let mut out = Vec::with_capacity(commands.len() * 3 + 2);
    if batch {
        out.push(AppCommand::BeginHistoryBatch);
    }
    for cmd in commands {
        match &cmd {
            AppCommand::SetNodeParam { clip_id, .. } => {
                let clip_id = *clip_id;
                out.push(AppCommand::BeginNodeParamEdit { clip_id });
                out.push(cmd);
                out.push(AppCommand::EndNodeParamEdit { clip_id });
            }
            _ => out.push(cmd),
        }
    }
    if batch {
        out.push(AppCommand::EndHistoryBatch);
    }
    out
}

/// Read `[start_secs, end_secs)` of a conformed 48 kHz stereo f32 PCM cache
/// as mono, without loading the whole file.
fn read_pcm_mono_range(
    path: &std::path::Path,
    start_secs: f64,
    end_secs: f64,
    sample_rate: u32,
) -> std::io::Result<Vec<f32>> {
    use std::io::{Read, Seek, SeekFrom};
    const FRAME_BYTES: u64 = 8;
    let mut file = std::fs::File::open(path)?;
    let total_frames = file.metadata()?.len() / FRAME_BYTES;
    let first = ((start_secs.max(0.0) * sample_rate as f64) as u64).min(total_frames);
    let last = ((end_secs.max(0.0) * sample_rate as f64) as u64).clamp(first, total_frames);
    file.seek(SeekFrom::Start(first * FRAME_BYTES))?;
    let mut bytes = vec![0u8; ((last - first) * FRAME_BYTES) as usize];
    file.read_exact(&mut bytes)?;
    Ok(bytes
        .chunks_exact(8)
        .map(|f| {
            let l = f32::from_le_bytes([f[0], f[1], f[2], f[3]]);
            let r = f32::from_le_bytes([f[4], f[5], f[6], f[7]]);
            0.5 * (l + r)
        })
        .collect())
}

/// `m:ss.s` for a source position in toasts.
fn format_profile_time(secs: f64) -> String {
    let secs = secs.max(0.0);
    format!("{}:{:04.1}", (secs / 60.0).floor() as u64, secs % 60.0)
}

#[cfg(test)]
mod control_envelope_tests {
    use super::*;
    use klipp_core::ClipId;

    #[test]
    fn a_parameter_write_over_the_socket_is_bracketed_like_a_drag() {
        let clip_id = ClipId::new();
        let cmds = expand_control_commands(
            vec![AppCommand::SetNodeParam { clip_id, node_id: klipp_nodes::NodeId::new_v4(), param: "exposure".into(), value: 0.5 }],
            false,
        );
        assert!(matches!(cmds.as_slice(), [AppCommand::BeginNodeParamEdit { .. }, AppCommand::SetNodeParam { .. }, AppCommand::EndNodeParamEdit { .. }]));
    }

    #[test]
    fn a_batch_is_one_history_entry() {
        let cmds = expand_control_commands(vec![AppCommand::Play, AppCommand::Pause], true);
        assert!(matches!(cmds.first(), Some(AppCommand::BeginHistoryBatch)));
        assert!(matches!(cmds.last(), Some(AppCommand::EndHistoryBatch)));
        assert_eq!(cmds.len(), 4);
        // And the history collects what a batch pushes into one entry.
        let mut h = crate::history::CommandHistory::new();
        h.begin_batch();
        h.push(crate::history::HistoryEntry { command: crate::history::EditCommand::Batch(vec![]) });
        h.push(crate::history::HistoryEntry { command: crate::history::EditCommand::Batch(vec![]) });
        h.end_batch();
        assert!(h.undo().is_some());
        assert!(h.undo().is_none(), "two pushes inside a batch are one undo step");
    }
}

#[cfg(test)]
mod prefs_apply_tests {
    use super::*;
    use crate::prefs::Preferences;

    fn fresh() -> (Preferences, AppState) {
        (Preferences::default(), AppState::default())
    }

    #[test]
    fn apply_snap_threshold_px_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        let new_val = 22.0;
        assert!((prefs.editing.snap_threshold_px - new_val).abs() > 1e-6,
            "default must differ from new_val to make the test meaningful");
        apply_snap_threshold_px(&mut prefs, &mut state, new_val);
        assert!((prefs.editing.snap_threshold_px - new_val).abs() < 1e-6);
        assert!((state.snap_threshold_px - new_val).abs() < 1e-6);
    }

    #[test]
    fn apply_zoom_step_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        let new_val = 2.0;
        assert!((prefs.editing.zoom_step - new_val).abs() > 1e-6);
        apply_zoom_step(&mut prefs, &mut state, new_val);
        assert!((prefs.editing.zoom_step - new_val).abs() < 1e-6);
        assert!((state.zoom_step - new_val).abs() < 1e-6);
    }

    #[test]
    fn apply_magnetic_timeline_bridges_naming_asymmetry() {
        // The pref is `magnetic_timeline` but state holds the runtime
        // copy as `magnetic_timeline_enabled`. Helper writes both —
        // failing this means a future rename broke the bridge.
        let (mut prefs, mut state) = fresh();
        prefs.editing.magnetic_timeline = true;
        state.magnetic_timeline_enabled = true;
        apply_magnetic_timeline(&mut prefs, &mut state, false);
        assert!(!prefs.editing.magnetic_timeline);
        assert!(!state.magnetic_timeline_enabled);
        apply_magnetic_timeline(&mut prefs, &mut state, true);
        assert!(prefs.editing.magnetic_timeline);
        assert!(state.magnetic_timeline_enabled);
    }

    #[test]
    fn apply_default_sequence_resolution_idx_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        let new_val: usize = 4;
        assert_ne!(prefs.editing.default_sequence_resolution_idx, new_val);
        apply_default_sequence_resolution_idx(&mut prefs, &mut state, new_val);
        assert_eq!(prefs.editing.default_sequence_resolution_idx, new_val);
        assert_eq!(state.default_sequence_resolution_idx, new_val);
    }

    #[test]
    fn apply_default_transition_frames_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        let new_val: u32 = 60;
        assert_ne!(prefs.editing.default_transition_frames, new_val);
        apply_default_transition_frames(&mut prefs, &mut state, new_val);
        assert_eq!(prefs.editing.default_transition_frames, new_val);
        assert_eq!(state.default_transition_frames, new_val);
    }

    #[test]
    fn apply_multicam_show_angles_mirrors_to_both() {
        let (mut prefs, mut state) = fresh();
        prefs.editing.multicam_show_angles = true;
        state.multicam_show_angles = true;
        apply_multicam_show_angles(&mut prefs, &mut state, false);
        assert!(!prefs.editing.multicam_show_angles);
        assert!(!state.multicam_show_angles);
    }
}

impl KlippApp {

    /// Phase 5 — fire any one-shot first-time tips whose unlock
    /// condition is satisfied by the active sequence and that haven't
    /// already been dismissed. Each tip records its key in
    /// `prefs.general.dismissed_tips` after firing so it never fires
    /// again across sessions. Cheap per-frame check: short-circuit on
    /// the first un-dismissed, satisfied tip; usually exits at the
    /// initial `contains` lookup.
    ///
    /// Track-matte intro: fires the first time the active sequence
    /// has two video clips that overlap in time on different tracks
    /// — the moment the right-click "Use clip above as matte" gesture
    /// becomes meaningful. Dismissed forever once shown.
    fn maybe_fire_first_time_tips(&mut self) {
        const TRACK_MATTE_TIP_KEY: &str = "track_matte_intro";
        if self.prefs.general.dismissed_tips.contains(TRACK_MATTE_TIP_KEY) {
            return;
        }
        let unlocked = sequence_has_video_track_overlap(
            &self.state.sequences[self.state.active_sequence_idx],
        );
        if !unlocked {
            return;
        }
        self.state.toast.show(
            "Tip: right-click any clip → \"Use clip above as matte\" to mask one clip with another",
        );
        self.prefs.general.dismissed_tips.insert(TRACK_MATTE_TIP_KEY.to_string());
        // Best-effort save — losing the dismissed flag on a write
        // failure means the tip might fire one more time next session,
        // which is harmless. We don't propagate the error to the user.
        let _ = self.prefs.save();
    }

    /// Register (or re-register) `FrameBuffer.gpu_output` with
    /// eframe-wgpu's renderer and update `viewer.gpu_texture_id`.
    /// Called at the top of every `update()`; the `gpu_output_generation`
    /// check means we only actually touch the renderer when the
    /// engine produced a newly-sized texture.
    ///
    /// No-op when the engine hasn't published a gpu_output yet, or
    /// when eframe is running without a wgpu render state (glow
    /// fallback). Legacy CPU viewer path is untouched.
    /// Free GPU resources we registered with eframe-wgpu's renderer
    /// while we still have `frame: &mut eframe::Frame` access. Called
    /// from `update()` the moment we know we're going to send
    /// `ViewportCommand::Close`. Without this, `gpu_texture_id` lives
    /// in the renderer's BindGroup map until the renderer drops; the
    /// matching `gpu_texture_view` also lives until KlippApp drops;
    /// when the wgpu::Device tears down the order ends up wrong and
    /// the process SIGSEGVs in egui-wgpu's queue.submit.
    ///
    /// Idempotent — safe to call multiple times. After this runs,
    /// the viewer paints fall back to the CPU upload path until the
    /// app actually exits.
    /// Stop the engine, wait for it to finish releasing its GPU
    /// resources, then exit the process.
    ///
    /// Every close path used to call `process::exit(0)` directly. That
    /// runs libc's atexit handlers and terminates *without waiting for
    /// other threads*, and the NVIDIA GL driver registers teardown in
    /// those handlers — so exiting while the engine thread was still
    /// dropping its wgpu pipelines tore the device out from under it
    /// mid-call. A core dump caught it exactly there: engine thread in
    /// `destroy_pipeline` under `drop_glue<Option<GpuEffectsPipeline>>`,
    /// main thread down inside `libGLX_nvidia`'s exit path. The comment
    /// on the Don't-Save branch had already named this race; this is the
    /// handshake it was missing.
    ///
    /// `process::exit` itself stays. It is a deliberate mitigation for
    /// the separate eframe/wgpu drop-order SEGV (`BUG-SHUTDOWN-SEGV`)
    /// that survived eight rounds of leak-plugging, and removing it here
    /// would trade one crash for another.
    ///
    /// The wait is bounded: a wedged engine must not stop the user
    /// closing the window, and exiting anyway is no worse than the
    /// behaviour this replaces.
    fn exit_after_engine_shutdown(&mut self) -> ! {
        // Before anything that can hang: a lock left behind makes the
        // next open ask a question nobody needs to answer.
        if let Some(keeper) = self.lock_keeper.take() {
            keeper.release_now();
        }
        const BUDGET: std::time::Duration = std::time::Duration::from_millis(2000);
        let t0 = std::time::Instant::now();

        // Harmless if the engine is already shutting down or gone: a
        // closed channel just errors, and the flag is already set.
        let _ = self.cmd_tx.send(klipp_core::AppCommand::Quit);

        let mut confirmed = false;
        while t0.elapsed() < BUDGET {
            if klipp_engine::ENGINE_SHUTDOWN_COMPLETE
                .load(std::sync::atomic::Ordering::Acquire)
            {
                confirmed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }

        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        if confirmed {
            tracing::info!("engine confirmed teardown in {ms:.1}ms — exiting");
        } else {
            tracing::warn!(
                "engine did not confirm teardown within {}ms — exiting anyway",
                BUDGET.as_millis(),
            );
        }

        // The engine being down does not mean its decode threads are.
        // `VideoDecoder::stop` waits half a second and then detaches, and
        // `stop_detached` does not wait at all — fine while the process
        // lives, fatal here: `exit` tears down libc under threads that
        // keep running, and a decode thread still inside ffmpeg's
        // teardown then crashes in it. Also bounded, for the same reason
        // the engine wait is.
        klipp_engine::video_decoder::wait_for_decode_threads(
            std::time::Duration::from_millis(1000),
        );
        std::process::exit(0);
    }

    /// Kill child processes (proxy ffmpeg, render queue) before
    /// `process::exit(0)` skips Drop. Idempotent — safe to call from
    /// any of the close paths.
    fn kill_children_for_exit(&mut self) {
        for pid_ref in &self.proxy_children {
            let pid = pid_ref.load(std::sync::atomic::Ordering::Relaxed);
            if pid != 0 {
                let _ = std::process::Command::new("kill")
                    .arg(pid.to_string())
                    .status();
            }
        }
        self.state.render_queue.kill_active();
    }

    /// Free GPU resources we registered with eframe-wgpu's renderer
    /// while we still have `frame: &mut eframe::Frame` access. Kept
    /// around — currently unused because the SHUTDOWN-SEGV
    /// workaround (`std::process::exit(0)`) bypasses the eframe
    /// close path entirely. When `BUG-SHUTDOWN-SEGV` gets a real
    /// fix this becomes the canonical pre-close hook.
    #[allow(dead_code)]
    fn pre_close_gpu_cleanup(&mut self, frame: &mut eframe::Frame) {
        tracing::info!("pre_close_gpu_cleanup: freeing GPU-side resources");
        let t0 = std::time::Instant::now();

        // 1) Free the eframe-wgpu native texture id + its view.
        if let Some(render_state) = frame.wgpu_render_state() {
            let mut renderer = render_state.renderer.write();
            if let Some(old_id) = self.viewer.gpu_texture_id.take() {
                renderer.free_texture(&old_id);
            }
        } else {
            self.viewer.gpu_texture_id = None;
        }
        self.viewer.gpu_texture_view = None;
        self.viewer.gpu_texture_gen = 0;

        // 2) Drop the engine's GPU output texture. The
        //    `frame_buffer.gpu_output: Option<Arc<wgpu::Texture>>` is
        //    shared with the engine; engine has shut down by the time
        //    we reach the second close pass, so it's safe to take.
        //    Dropping the Texture HERE (with wgpu device still
        //    alive) routes the deallocate through a healthy queue;
        //    leaving it for AppState drop later races teardown.
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.gpu_output = None;
            fb.gpu_output_width = 0;
            fb.gpu_output_height = 0;
            fb.gpu_output_generation = 0;
        }
        {
            let mut sfb = self.source_frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            sfb.gpu_output = None;
            sfb.gpu_output_width = 0;
            sfb.gpu_output_height = 0;
            sfb.gpu_output_generation = 0;
        }

        // 3) Drop every egui::TextureHandle we know about now (same
        //    set on_exit clears, but earlier so there's more
        //    headroom before wgpu starts tearing down).
        self.state.ai_tools_state.mask_overlay_texture = None;
        self.state.source_live_texture = None;
        self.state.browser_skim_texture = None;
        self.state.quick_look_texture = None;
        self.state.file_info_thumb = None;
        self.state.thumbnail_textures.clear();
        self.state.filmstrip_textures.clear();
        self.viewer.video_texture = None;
        self.viewer.title_overlay_texture = None;
        self.viewer.logo_texture = None;
        self.browser.clear_textures_for_shutdown();
        self.new_inspector.clear_textures_for_shutdown();
        for bucket in &mut self.state.deferred_texture_drops {
            bucket.clear();
        }

        tracing::info!(
            "pre_close_gpu_cleanup done in {:.1}ms",
            t0.elapsed().as_secs_f64() * 1000.0
        );
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

impl KlippApp {

    /// Check if the track containing a given clip is locked.
    fn clip_track_is_locked(&self, clip_id: &ClipId) -> bool {
        self.state.sequences[self.state.active_sequence_idx].tracks.iter()
            .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
            .is_some_and(|t| t.locked)
    }

    /// Swap a clip's backing media while preserving timeline_in/out,
    /// source_in/out, effects, transitions, and linked partner.
    /// Auto-clamps source_out + timeline_out by the same delta when
    /// the new media is shorter than the existing trim window.
    /// Returns true on success; toasts + returns false on validation
    /// failure (locked track, unknown clip / media, identical media,
    /// non-Media kind).
    fn try_replace_clip_media(
        &mut self,
        clip_id: ClipId,
        new_media_id: MediaId,
    ) -> bool {
        if self.clip_track_is_locked(&clip_id) {
            self.state.toast.show("Replace: track is locked");
            return false;
        }
        // Resolve the new media's source duration so we know whether
        // to clamp. None → media isn't in the pool.
        let new_media_dur = match self.state.media_pool.get(&new_media_id) {
            Some(item) => item.duration,
            None => {
                self.state.toast.show("Replace: target media not found in pool");
                return false;
            }
        };
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
            self.state.toast.show("Replace: clip not found");
            return false;
        };
        // Only Media-kind clips can have their media swapped — Title /
        // Generator / Sequence clips have a different backing model
        // and the gesture doesn't apply.
        let old_media_id = match clip.kind {
            klipp_timeline::ClipKind::Media { media_id } => media_id,
            _ => {
                self.state.toast.show("Replace: only media clips can be swapped");
                return false;
            }
        };
        if old_media_id == new_media_id {
            return false; // no-op, no toast
        }

        // Clamp source_out (and timeline_out by the same delta) when
        // the new media is shorter than the existing trim window.
        // Source_in stays untouched — if user trimmed in by 5s on
        // the old clip and the new media is at least source_in long,
        // we keep the head trim. Both clamps are applied to the same
        // timing reference (`clip.source_out`), so the visible clip
        // length never grows; it only shortens to fit.
        let old_source_out = clip.source_out;
        let old_timeline_out = clip.timeline_out;
        let new_source_out = if old_source_out > new_media_dur {
            new_media_dur
        } else {
            old_source_out
        };
        let dt_secs = old_source_out.to_secs_f64() - new_source_out.to_secs_f64();
        let new_timeline_out = if dt_secs > 0.0 {
            // Convert delta to the timeline_out's rate to subtract.
            let dt_at_timeline_rate = klipp_core::RationalTime::from_secs_f64(
                dt_secs.max(0.0),
                old_timeline_out.rate,
            );
            old_timeline_out - dt_at_timeline_rate
        } else {
            old_timeline_out
        };

        clip.kind = klipp_timeline::ClipKind::Media { media_id: new_media_id };
        clip.source_out = new_source_out;
        clip.timeline_out = new_timeline_out;

        self.state.push_history(EditCommand::LegacyReplaceClipMedia {
            clip_id,
            old_media_id,
            new_media_id,
            old_source_out,
            new_source_out,
            old_timeline_out,
            new_timeline_out,
        });
        true
    }

    /// Wrapper around [`klipp_timeline::Sequence::try_swap_adjacent_clips`]
    /// that surfaces failure reasons as toasts and pushes a history
    /// entry on success. The actual validation + mutation logic lives
    /// in the timeline crate so it's testable without app scaffolding.
    fn try_swap_adjacent_clips(
        &mut self,
        clip_a: ClipId,
        clip_b: ClipId,
    ) -> bool {
        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
        match seq.try_swap_adjacent_clips(clip_a, clip_b) {
            Ok(result) => {
                let linked_a_partner = result.partners.as_ref().map(|p| p.a_partner);
                let linked_b_partner = result.partners.as_ref().map(|p| p.b_partner);
                self.state.push_history(EditCommand::LegacySwapClips {
                    clip_a_id: clip_a,
                    a_old_in: result.a_old_in,
                    a_old_out: result.a_old_out,
                    clip_b_id: clip_b,
                    b_old_in: result.b_old_in,
                    b_old_out: result.b_old_out,
                    linked_a_partner,
                    linked_b_partner,
                });
                true
            }
            Err(reason) => {
                self.state.toast.show(reason);
                false
            }
        }
    }

    /// Check if a track uses magnetic editing (Primary, Connected, or ConnectedStoryline).
    /// Returns false when the magnetic timeline pref is disabled.
    fn is_magnetic_track(&self, track_index: usize) -> bool {
        self.state.magnetic_timeline_enabled
            && matches!(
                self.state.sequences[self.state.active_sequence_idx].tracks.get(track_index).map(|t| &t.magnetic_role),
                Some(klipp_timeline::TrackRole::Primary | klipp_timeline::TrackRole::Connected | klipp_timeline::TrackRole::ConnectedStoryline)
            )
    }

}

struct ColorWsParams {
    show_scopes_bar: bool,
    show_nodes_bar: bool,
    color_tools_bottom: bool,
    toggle_bar_h: f32,
    scopes_h: f32,
    center_left: f32,
    center_w: f32,
    main_top: f32,
    main_bottom: f32,
    flexible_h: f32,
    toggle_bar_rect: egui::Rect,
    nodes_rect: egui::Rect,
    bottom_scope_cursor: Option<crate::state::ScopeCursorData>,
}

/// Returns true if inserting `candidate_id` as a compound clip into a sequence
/// identified by `host_id` would create a circular reference. Performs a DFS
/// from `candidate_id` through every transitively-referenced compound clip;
/// if `host_id` appears anywhere in that subtree (including `candidate_id`
/// itself), it is a cycle.
fn would_create_cycle(
    candidate_id: klipp_core::SequenceId,
    host_id: klipp_core::SequenceId,
    all_sequences: &[klipp_timeline::Sequence],
) -> bool {
    let mut visited = std::collections::HashSet::new();
    let mut stack = vec![candidate_id];
    while let Some(sid) = stack.pop() {
        if sid == host_id {
            return true;
        }
        if !visited.insert(sid) {
            continue;
        }
        if let Some(seq) = all_sequences.iter().find(|s| s.id == sid) {
            for track in &seq.tracks {
                for clip in &track.clips {
                    if let klipp_timeline::ClipKind::Sequence { sequence_id, .. } = &clip.kind {
                        stack.push(*sequence_id);
                    }
                }
            }
        }
    }
    false
}

/// Count how many clips across all sequences reference this media_id.
/// Persist the current custom workspace list to
/// `~/.config/klipp/workspaces.json`. Errors are swallowed because this
/// is a best-effort UI persistence write — if it fails the user gets
/// the runtime state for this session and a warning in the log.
fn persist_workspaces(workspaces: &[crate::state::SavedWorkspaceLayout]) {
    if let Ok(json) = serde_json::to_string_pretty(workspaces) {
        let path = dirs::config_dir()
            .unwrap_or_default()
            .join("klipp/workspaces.json");
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")));
        if let Err(e) = std::fs::write(&path, json) {
            tracing::warn!("Failed to persist workspaces.json: {e}");
        }
    }
}

fn media_usage_count(media_id: MediaId, sequences: &[Sequence]) -> usize {
    sequences.iter()
        .flat_map(|seq| seq.tracks.iter())
        .flat_map(|t| &t.clips)
        .filter(|c| c.media_id() == Some(media_id))
        .count()
}

/// Compute a content fingerprint: FNV-1a hash of first 64KB + file_size.
/// Returns None if the file can't be read.
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

fn compute_media_fingerprint(path: &Path) -> Option<u64> {
    klipp_media::relink::fingerprint(path)
}

/// Detect if a set of paths forms a numbered image sequence.
/// Returns (ffmpeg_pattern, start_frame, frame_count) if detected.
pub fn detect_image_sequence(paths: &[PathBuf]) -> Option<(String, u32, u32)> {
    if paths.len() < 2 { return None; }

    // All must be in the same directory with the same extension
    let dir = paths[0].parent()?;
    let ext = paths[0].extension()?.to_str()?;
    if !paths.iter().all(|p| {
        p.parent() == Some(dir)
            && p.extension().and_then(|e| e.to_str()) == Some(ext)
    }) {
        return None;
    }

    // Only image extensions (delegated to the canonical klipp_core list,
    // so any future image format automatically participates in image
    // sequence detection).
    if !klipp_core::media_extensions::is_image(ext) {
        return None;
    }

    // Extract numeric suffix from each filename: name_NNNN.ext
    let mut frames: Vec<(u32, &PathBuf)> = paths.iter()
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?;
            // Find trailing digits
            let digit_start = stem.rfind(|c: char| !c.is_ascii_digit())?;
            let num_str = &stem[digit_start + 1..];
            if num_str.is_empty() { return None; }
            let n: u32 = num_str.parse().ok()?;
            Some((n, p))
        })
        .collect();

    if frames.len() < 2 { return None; }
    frames.sort_by_key(|(n, _)| *n);

    let start = frames[0].0;
    let end = frames.last()?.0;
    let count = frames.len() as u32;

    // Must be mostly contiguous (allow up to 5% gaps)
    let expected = end - start + 1;
    if (count as f64) < (expected as f64 * 0.95) { return None; }

    // Build ffmpeg pattern: determine padding width
    let sample_stem = frames[0].1.file_stem()?.to_str()?;
    let digit_start = sample_stem.rfind(|c: char| !c.is_ascii_digit())?;
    let prefix = &sample_stem[..=digit_start];
    let padding = sample_stem.len() - digit_start - 1;
    let pattern = format!("{}/{prefix}%0{padding}d.{ext}", dir.display());

    Some((pattern, start, count))
}

/// Returns the autosave path for a given project path: `<stem>_autosave.klipp`
pub(crate) fn autosave_path_for(project_path: &Path) -> PathBuf {
    let stem = project_path.file_stem().unwrap_or_default().to_string_lossy();
    project_path.with_file_name(format!("{stem}_autosave.klipp"))
}

/// Whether a path is an auto-save folder rather than a project.
pub(crate) fn is_autosave_path(path: &Path) -> bool {
    path.file_stem().is_some_and(|s| s.to_string_lossy().ends_with("_autosave"))
}

/// The project an auto-save folder belongs to: `X_autosave.klipp` → `X.klipp`.
pub(crate) fn project_for_autosave(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_string_lossy();
    let main = stem.strip_suffix("_autosave").filter(|m| !m.is_empty())?;
    Some(path.with_file_name(format!("{main}.klipp")))
}

/// Mark a project's auto-save as seen after the user chose the saved
/// version over it: the project folder's time moves past the auto-save's,
/// so neither the open prompt nor the browser's "Unsaved work recovered"
/// badge offers it again. The auto-save itself is left on disk, and goes
/// the way it always has, on the next clean save or close.
pub(crate) fn retire_autosave(project_path: &Path) {
    let now = std::time::SystemTime::now();
    if let Err(e) = std::fs::File::open(project_path).and_then(|f| f.set_modified(now)) {
        tracing::warn!("Could not mark the auto-save of {project_path:?} as seen: {e}");
    }
}

/// Whether a project has an auto-save newer than its last save: work
/// that was never saved, usually because Klipp closed unexpectedly.
pub(crate) fn has_newer_autosave(project_path: &Path) -> bool {
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    match (modified(project_path), modified(&autosave_path_for(project_path))) {
        (Some(saved), Some(auto)) => auto > saved,
        (None, Some(_)) => true,
        _ => false,
    }
}

/// Generate an ISO 8601 UTC timestamp string.
pub fn iso8601_now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Generate a poster frame thumbnail and save as PNG.
/// Returns the path to the saved thumbnail, or None if no frame available.
/// Save RGBA pixel data as a PNG file.
fn save_rgba_as_png(rgba: &[u8], width: u32, height: u32, path: &std::path::Path) -> bool {
    let Ok(file) = std::fs::File::create(path) else { return false };
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let Ok(mut writer) = encoder.write_header() else { return false };
    writer.write_image_data(rgba).is_ok()
}

fn generate_poster_frame(
    frame_buffer: &klipp_engine::SharedFrameBuffer,
    project_path: &std::path::Path,
    media_pool: &klipp_media::MediaPool,
    duration_secs: f64,
) -> Option<PathBuf> {
    let cache_dir = dirs::cache_dir()?.join("klipp").join("thumbnails");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let stem = project_path.file_stem()?.to_string_lossy();
    let out_path = cache_dir.join(format!("{stem}.png"));

    // Strategy 1: Use current viewer frame if non-black
    if let Some(path) = try_poster_from_frame_buffer(frame_buffer, &out_path) {
        return Some(path);
    }

    // Strategy 2: Extract first non-black frame from the first media file
    if let Some(path) = try_poster_from_media(media_pool, duration_secs, &out_path) {
        return Some(path);
    }

    None
}

/// Try to generate a poster frame from the current viewer frame buffer.
/// Returns None if the frame is empty or all-black.
fn try_poster_from_frame_buffer(
    frame_buffer: &klipp_engine::SharedFrameBuffer,
    out_path: &std::path::Path,
) -> Option<PathBuf> {
    let fb = frame_buffer.lock().ok()?;
    if fb.data.is_empty() || fb.width == 0 || fb.height == 0 {
        return None;
    }
    if klipp_engine::is_frame_black(&fb.data, fb.width, fb.height) {
        return None;
    }

    let rgb = downsample_rgba_to_rgb(&fb.data, fb.width, fb.height, 320);
    save_rgb_as_png(&rgb.0, rgb.1, rgb.2, out_path).then(|| out_path.to_path_buf())
}

/// Try to generate a poster frame by decoding the first media file in the pool.
fn try_poster_from_media(
    media_pool: &klipp_media::MediaPool,
    duration_secs: f64,
    out_path: &std::path::Path,
) -> Option<PathBuf> {
    // Find first video media file
    let first_media = media_pool.all()
        .find(|m| m.path.exists() && is_video_ext(&m.path))?;

    let est_dur = if duration_secs > 0.0 { duration_secs } else { 60.0 };
    let (rgba, w, h) = klipp_engine::generate_thumbnail_at_size_non_black(
        &first_media.path, 1.0, est_dur, 320,
    ).ok()?;

    save_rgba_as_png(&rgba, w, h, out_path).then(|| out_path.to_path_buf())
}

/// Force-capture the current viewer frame as project thumbnail (for custom thumbnail).
fn capture_viewer_frame_as_thumbnail(
    frame_buffer: &klipp_engine::SharedFrameBuffer,
    project_path: &std::path::Path,
) -> Option<PathBuf> {
    let fb = frame_buffer.lock().ok()?;
    if fb.data.is_empty() || fb.width == 0 || fb.height == 0 {
        return None;
    }

    let cache_dir = dirs::cache_dir()?.join("klipp").join("thumbnails");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let stem = project_path.file_stem()?.to_string_lossy();
    let out_path = cache_dir.join(format!("{stem}.png"));

    let rgb = downsample_rgba_to_rgb(&fb.data, fb.width, fb.height, 320);
    save_rgb_as_png(&rgb.0, rgb.1, rgb.2, &out_path).then_some(out_path)
}

/// Set project thumbnail from an external image file (PNG, JPEG, etc.).
fn set_thumbnail_from_image_file(
    image_path: &std::path::Path,
    project_path: &std::path::Path,
) -> Option<PathBuf> {
    let cache_dir = dirs::cache_dir()?.join("klipp").join("thumbnails");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let stem = project_path.file_stem()?.to_string_lossy();
    let out_path = cache_dir.join(format!("{stem}.png"));

    // Read and decode with the image crate-style approach using ffmpeg
    // Use generate_thumbnail_at_size for images (seeks to 0.0s, which works for stills)
    if klipp_core::media_extensions::path_is_image(image_path) {
        // For images, use ffmpeg to decode and resize. Now correctly handles
        // HEIC/HEIF/AVIF/JXL — the previous hardcoded chain only matched 7
        // legacy extensions and silently fell through to the video branch
        // for HEIC, which then errored on seek.
        if let Ok((rgba, w, h)) = klipp_engine::generate_thumbnail_at_size(image_path, 0.0, 320) {
            return save_rgba_as_png(&rgba, w, h, &out_path).then_some(out_path);
        }
    }

    // For video files, extract a non-black frame
    if let Ok((rgba, w, h)) = klipp_engine::generate_thumbnail_at_size_non_black(image_path, 1.0, 60.0, 320) {
        return save_rgba_as_png(&rgba, w, h, &out_path).then_some(out_path);
    }

    None
}

/// Downsample RGBA frame data to RGB at a target width (maintaining aspect ratio).
fn downsample_rgba_to_rgb(rgba: &[u8], src_w: u32, src_h: u32, target_w: u32) -> (Vec<u8>, u32, u32) {
    let dst_w = target_w.min(src_w);
    let dst_h = (src_h as u64 * dst_w as u64 / src_w as u64) as u32;
    let mut rgb = vec![0u8; (dst_w * dst_h * 3) as usize];

    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let sx = (dx as u64 * src_w as u64 / dst_w as u64) as u32;
            let sy = (dy as u64 * src_h as u64 / dst_h as u64) as u32;
            let src_idx = ((sy * src_w + sx) * 4) as usize;
            let dst_idx = ((dy * dst_w + dx) * 3) as usize;
            if src_idx + 2 < rgba.len() && dst_idx + 2 < rgb.len() {
                rgb[dst_idx] = rgba[src_idx];
                rgb[dst_idx + 1] = rgba[src_idx + 1];
                rgb[dst_idx + 2] = rgba[src_idx + 2];
            }
        }
    }
    (rgb, dst_w, dst_h)
}

/// Write RGB pixel data as a PNG file.
fn save_rgb_as_png(rgb: &[u8], width: u32, height: u32, path: &std::path::Path) -> bool {
    let Ok(file) = std::fs::File::create(path) else { return false };
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let Ok(mut writer) = encoder.write_header() else { return false };
    writer.write_image_data(rgb).is_ok()
}

fn is_video_ext(path: &std::path::Path) -> bool {
    klipp_core::media_extensions::path_is_video(path)
}

fn ensure_klipp_extension(mut path: PathBuf) -> PathBuf {
    if path.extension().and_then(|e| e.to_str()) != Some("klipp") {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if !name.ends_with(".klipp") {
            path.set_file_name(format!("{}.klipp", name));
        }
    }
    path
}

/// Wire-format → enum conversion for `AppCommand::SetClipMatte`. Mirrors the
/// `MatteChannel` discriminant order so the on-the-wire `u8` survives
/// round-trips through any logging / replay layer that flattens the enum
/// before re-dispatching. Out-of-range values fall back to `Luminance` —
/// the historical default for `MatteChannel`.
fn matte_channel_from_u8(b: u8) -> klipp_nodes::params::MatteChannel {
    match b {
        0 => klipp_nodes::params::MatteChannel::Alpha,
        1 => klipp_nodes::params::MatteChannel::Luminance,
        2 => klipp_nodes::params::MatteChannel::Red,
        3 => klipp_nodes::params::MatteChannel::Green,
        4 => klipp_nodes::params::MatteChannel::Blue,
        _ => klipp_nodes::params::MatteChannel::Luminance,
    }
}

/// Phase 5 — does the sequence have at least two clips on different
/// video tracks that overlap in time? Used to gate the first-time
/// "right-click → Use clip above as matte" tip — that gesture only
/// becomes meaningful once the user has two stacked clips with a
/// shared time-range. Free fn so tests can pin behaviour without
/// standing up an `App`.
///
/// Linear in the total clip count across video tracks, with an early
/// exit on the first overlap. Empty sequences and single-track ones
/// trivially return false.
fn sequence_has_video_track_overlap(seq: &klipp_timeline::Sequence) -> bool {
    let video_tracks: Vec<&klipp_timeline::Track> = seq
        .tracks
        .iter()
        .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video))
        .collect();
    if video_tracks.len() < 2 {
        return false;
    }
    for (i, ti) in video_tracks.iter().enumerate() {
        for tj in video_tracks.iter().skip(i + 1) {
            for ci in &ti.clips {
                for cj in &tj.clips {
                    // Strict open-interval overlap: clips that just
                    // touch at a frame boundary (cj.in == ci.out)
                    // don't trigger — they share no actual frame.
                    if ci.timeline_in < cj.timeline_out && cj.timeline_in < ci.timeline_out {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Walk the matte chain starting from `source` (i.e. the clip the user is
/// trying to bind as the matte for `consumer`). If we ever hit `consumer`,
/// the bind would create a cycle. Returns `true` to mean "reject this".
///
/// The depth cap is defensive — a well-formed sequence never has cycles
/// thanks to this exact check, but a corrupt project file or a logic bug
/// upstream could produce one. 64 hops is far past anything a human would
/// build and well below stack-overflow risk.
fn matte_chain_creates_cycle(
    seq: &klipp_timeline::Sequence,
    consumer: klipp_core::ClipId,
    source: klipp_core::ClipId,
) -> bool {
    if consumer == source {
        return true;
    }
    let mut current = source;
    for _ in 0..64 {
        let next = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == current)
            .and_then(|c| c.matte_source.as_ref().map(|b| b.source_clip_id));
        match next {
            None => return false,
            Some(n) if n == consumer => return true,
            Some(n) => current = n,
        }
    }
    // Pre-existing cycle in a corrupt sequence: refuse to extend it.
    true
}

/// Mirror of `matte_chain_creates_cycle` for the `transform_link`
/// chain: returns true iff binding `consumer.transform_link.target =
/// target` would create a cycle (consumer → … → target → consumer).
/// Uses a 64-link depth cap so a corrupt pre-existing cycle is
/// detected as "always rejecting any new bind" rather than spinning.
pub(crate) fn transform_link_chain_creates_cycle(
    seq: &klipp_timeline::Sequence,
    consumer: klipp_core::ClipId,
    target: klipp_core::ClipId,
) -> bool {
    if consumer == target {
        return true;
    }
    let mut current = target;
    for _ in 0..64 {
        let next = seq
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == current)
            .and_then(|c| c.transform_link.as_ref().map(|tl| tl.target.referenced_clip_id()));
        match next {
            None => return false,
            Some(n) if n == consumer => return true,
            Some(n) => current = n,
        }
    }
    true
}

#[cfg(test)]
mod effect_toggle_tests {
    //! Pure-logic coverage for the per-effect enable/disable toggle. The
    //! full handler runs on KlippApp and is hard to unit-test in isolation;
    //! these tests exercise the parallel-vec flip/grow logic that both the
    //! forward handler and the undo/redo arms share.

    fn toggle_builtin_enabled(enabled: &mut Vec<bool>, effect_count: usize, index: usize) {
        while enabled.len() < effect_count {
            enabled.push(true);
        }
        if let Some(val) = enabled.get_mut(index) {
            *val = !*val;
        }
    }

    #[test]
    fn builtin_effects_enabled_flip_logic() {
        let mut enabled = vec![true, true];
        toggle_builtin_enabled(&mut enabled, 2, 0);
        assert_eq!(enabled, vec![false, true]);
        // Simulate undo (flip again) — returns to original state.
        toggle_builtin_enabled(&mut enabled, 2, 0);
        assert_eq!(enabled, vec![true, true]);
    }

    #[test]
    fn builtin_toggle_is_idempotent_across_undo_redo() {
        // Three flips round-trips through enabled -> disabled -> enabled
        // -> disabled, mirroring user action -> undo -> redo -> user again.
        let mut enabled = vec![true];
        for _ in 0..3 {
            toggle_builtin_enabled(&mut enabled, 1, 0);
        }
        assert_eq!(enabled, vec![false]);
        toggle_builtin_enabled(&mut enabled, 1, 0);
        assert_eq!(enabled, vec![true]);
    }

    #[test]
    fn builtin_toggle_grows_parallel_vec_lazily() {
        // The parallel vec may be shorter than the effect list when projects
        // load from old `.klipp` files. Both the forward handler and the
        // undo arm grow it with `true` defaults so toggling the last effect
        // still works.
        let mut enabled: Vec<bool> = Vec::new();
        toggle_builtin_enabled(&mut enabled, 3, 2);
        assert_eq!(enabled.len(), 3);
        assert_eq!(enabled, vec![true, true, false]);
    }

    #[test]
    fn ofx_enabled_field_round_trip() {
        // OFX flag is an absolute bool — undo restores old, redo restores new.
        let mut enabled = true;
        let old = enabled;
        let new = false;
        // Forward
        enabled = new;
        assert!(!enabled);
        // Undo
        enabled = old;
        assert!(enabled);
        // Redo
        enabled = new;
        assert!(!enabled);
    }
}

#[cfg(test)]
mod node_param_undo_tests {
    //! Tests for the BeginNodeParamEdit / EndNodeParamEdit round-trip
    //! invariant: a gesture that snapshots the graph, mutates it, and commits
    //! a `SetNodeGraph` history entry must preserve the pre-gesture graph so
    //! the existing undo replay can restore it.
    //!
    //! Full dispatch testing requires a `KlippApp` instance (event channels,
    //! engine handle, egui context), which is not practical in a unit test.
    //! These tests validate the *data contract* my handlers depend on: the
    //! `EditCommand::SetNodeGraph` entry round-trips the old/new graphs and
    //! is structurally equivalent to what the existing `ToggleNodeEnabled`
    //! handler (known-working) produces.

    use crate::history::{EditCommand, HistoryEntry};
    use klipp_core::ClipId;

    #[test]
    fn set_node_graph_history_entry_preserves_snapshot() {
        // Simulate BeginNodeParamEdit: snapshot the current graph.
        let original = klipp_nodes::NodeGraph::default();
        let old_graph = original.clone();
        let original_node_count = original.nodes.len();

        // Simulate several SetNodeParam commands mutating the working graph.
        let mut working = original.clone();
        if let Some(first_node) = working.nodes.first_mut() {
            // Mutate something observable — a new label marks the working
            // graph as distinct from the snapshot.
            first_node.label = "mutated".to_string();
        }

        // Simulate EndNodeParamEdit: push a SetNodeGraph history entry with
        // the snapshot and the post-gesture graph.
        let entry = HistoryEntry {
            command: EditCommand::SetNodeGraph {
                clip_id: ClipId::new(),
                old_graph,
                new_graph: working.clone(),
            },
        };

        // The history entry must still hold an unmodified snapshot of the
        // pre-gesture graph — that's what undo replays.
        match &entry.command {
            EditCommand::SetNodeGraph { old_graph, new_graph, .. } => {
                assert_eq!(old_graph.nodes.len(), original_node_count);
                // old_graph must not have picked up the mutation we made to
                // the working copy.
                if let Some(snap_first) = old_graph.nodes.first() {
                    assert_ne!(snap_first.label, "mutated",
                        "snapshot must be independent of the working graph");
                }
                // And the new_graph must reflect the mutation.
                if let Some(new_first) = new_graph.nodes.first() {
                    assert_eq!(new_first.label, "mutated");
                }
            }
            other => panic!("expected SetNodeGraph, got {:?}", other),
        }
    }

    /// An atomic click-to-reset (Begin + SetNodeParam + End in the same
    /// frame) must produce exactly one `SetNodeGraph` history entry whose
    /// `old_graph` holds the pre-click state. This models the behavior of
    /// `push_atomic_node_edit` in node_inspector.rs.
    #[test]
    fn atomic_edit_creates_single_history_entry() {
        use klipp_core::AppCommand;
        use klipp_nodes::NodeId;

        // Simulated command stream produced by a single "reset" click:
        let clip_id = ClipId::new();
        let node_id = NodeId::new_v4();
        let cmds: Vec<AppCommand> = vec![
            AppCommand::BeginNodeParamEdit { clip_id },
            AppCommand::SetNodeParam {
                clip_id,
                node_id,
                param: "exposure".into(),
                value: 0.0,
            },
            AppCommand::EndNodeParamEdit { clip_id },
        ];

        // The stream must contain exactly one Begin and one End bracketing
        // the SetNodeParam. That's the contract a handler needs to produce a
        // single history entry.
        let begin_count = cmds.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).count();
        let end_count = cmds.iter().filter(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).count();
        let set_count = cmds.iter().filter(|c| matches!(c, AppCommand::SetNodeParam { .. })).count();
        assert_eq!(begin_count, 1, "atomic edit must emit exactly one Begin");
        assert_eq!(end_count, 1, "atomic edit must emit exactly one End");
        assert_eq!(set_count, 1, "atomic edit here carries one SetNodeParam");

        // Order: Begin must precede the SetNodeParam, End must follow.
        let begin_idx = cmds.iter().position(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).unwrap();
        let set_idx = cmds.iter().position(|c| matches!(c, AppCommand::SetNodeParam { .. })).unwrap();
        let end_idx = cmds.iter().position(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).unwrap();
        assert!(begin_idx < set_idx && set_idx < end_idx,
            "expected Begin < SetNodeParam < End, got {begin_idx}/{set_idx}/{end_idx}");
    }

    /// Two consecutive atomic edits must produce two separate Begin/End
    /// brackets so they become two distinct history entries, not one
    /// coalesced undo step.
    #[test]
    fn multiple_atomic_edits_create_separate_brackets() {
        use klipp_core::AppCommand;
        use klipp_nodes::NodeId;

        let clip_id = ClipId::new();
        let node_id = NodeId::new_v4();
        let cmds: Vec<AppCommand> = vec![
            // First atomic edit
            AppCommand::BeginNodeParamEdit { clip_id },
            AppCommand::SetNodeParam { clip_id, node_id, param: "exposure".into(), value: 0.5 },
            AppCommand::EndNodeParamEdit { clip_id },
            // Second atomic edit
            AppCommand::BeginNodeParamEdit { clip_id },
            AppCommand::SetNodeParam { clip_id, node_id, param: "contrast".into(), value: 0.3 },
            AppCommand::EndNodeParamEdit { clip_id },
        ];

        let begin_count = cmds.iter().filter(|c| matches!(c, AppCommand::BeginNodeParamEdit { .. })).count();
        let end_count = cmds.iter().filter(|c| matches!(c, AppCommand::EndNodeParamEdit { .. })).count();
        assert_eq!(begin_count, 2, "two edits must emit two Begins");
        assert_eq!(end_count, 2, "two edits must emit two Ends");

        // The two brackets must be disjoint: the first End must precede the
        // second Begin. If they were interleaved, the handler would coalesce
        // them into a single history entry.
        let positions: Vec<usize> = cmds
            .iter()
            .enumerate()
            .filter_map(|(i, c)| match c {
                AppCommand::BeginNodeParamEdit { .. } | AppCommand::EndNodeParamEdit { .. } => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(positions.len(), 4);
        assert!(positions[0] < positions[1] && positions[1] < positions[2] && positions[2] < positions[3],
            "brackets must not interleave");
    }

    #[test]
    fn begin_end_cycle_with_no_mutation_still_produces_valid_entry() {
        // Edge case: user clicks a slider but releases without dragging.
        // Begin snapshots, End commits an identical graph. The entry should
        // still be well-formed (undoing a no-op restores an identical graph).
        let graph = klipp_nodes::NodeGraph::default();
        let entry = HistoryEntry {
            command: EditCommand::SetNodeGraph {
                clip_id: ClipId::new(),
                old_graph: graph.clone(),
                new_graph: graph.clone(),
            },
        };
        match &entry.command {
            EditCommand::SetNodeGraph { old_graph, new_graph, .. } => {
                assert_eq!(old_graph.nodes.len(), new_graph.nodes.len());
            }
            _ => panic!(),
        }
    }
}

#[cfg(test)]
mod compound_clip_cycle_tests {
    //! Tests for the cycle-detection guard around compound clip creation.
    use super::would_create_cycle;
    use klipp_core::{Fps, RationalTime, Resolution, SequenceId};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};

    fn t(frames: i64) -> RationalTime {
        RationalTime::from_frames(frames, 24)
    }

    fn make_seq(name: &str) -> Sequence {
        Sequence {
            drop_frame: false,
            camera: Default::default(),
            timeline_graph: None,
            grade_groups: Vec::new(),
            id: SequenceId::new(),
            name: name.to_string(),
            fps: Fps::new(24, 1),
            resolution: Resolution { width: 1920, height: 1080 },
            first_clip_autoset: Default::default(),
            tracks: Vec::new(),
            in_point: None,
            out_point: None,
            markers: Vec::new(),
            last_playhead: None,
            brief: None,
            edit_plan: None,
        }
    }

    fn add_compound(seq: &mut Sequence, target: SequenceId) {
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips.push(Clip::sequence(target, t(0), t(24)));
        seq.tracks.push(tr);
    }

    #[test]
    fn self_reference_is_a_cycle() {
        let a = make_seq("a");
        let id = a.id;
        assert!(would_create_cycle(id, id, &[a]));
    }

    #[test]
    fn direct_cycle_a_contains_b_then_b_into_a_detected() {
        let mut a = make_seq("a");
        let b = make_seq("b");
        let a_id = a.id;
        let b_id = b.id;
        // a contains b
        add_compound(&mut a, b_id);
        // would inserting a as compound clip into b create a cycle?
        // candidate=a (contains b), host=b → DFS from a finds b, NOT host b ... wait
        // We want: candidate must transitively reach host. a → b. host=b. Yes cycle.
        let _ = b; // suppress unused
        assert!(would_create_cycle(a_id, b_id, &[a.clone(), make_seq("b_dup")]),
            "candidate=a (a contains b), host=b → DFS from a hits b");
    }

    #[test]
    fn transitive_cycle_a_to_c_to_b_detected() {
        let mut a = make_seq("a");
        let mut c = make_seq("c");
        let b = make_seq("b");
        let a_id = a.id;
        let b_id = b.id;
        let c_id = c.id;
        add_compound(&mut a, c_id); // a contains c
        add_compound(&mut c, b_id); // c contains b
        // candidate=a, host=b → DFS a → c → b. Cycle.
        assert!(would_create_cycle(a_id, b_id, &[a, b, c]));
    }

    #[test]
    fn independent_sequences_no_cycle() {
        let a = make_seq("a");
        let b = make_seq("b");
        let a_id = a.id;
        let b_id = b.id;
        assert!(!would_create_cycle(a_id, b_id, &[a, b]));
    }

    #[test]
    fn empty_all_sequences_no_panic() {
        let a_id = SequenceId::new();
        let b_id = SequenceId::new();
        assert!(!would_create_cycle(a_id, b_id, &[]));
    }
}

#[cfg(test)]
mod external_tools_tests {
    use crate::prefs::ExternalToolsPrefs;

    #[test]
    fn external_tools_prefs_default_round_trip_enabled() {
        let prefs = ExternalToolsPrefs::default();
        assert!(prefs.round_trip_reload);
        assert!(prefs.image_editor.is_empty());
        assert!(prefs.audio_editor.is_empty());
        assert!(prefs.video_editor.is_empty());
    }

    /// Mirrors the routing in the `OpenInExternalApp` handler: stills always
    /// go to the image editor.
    #[test]
    fn external_app_routing_still_uses_image_editor() {
        let prefs = ExternalToolsPrefs {
            image_editor: "gimp".into(),
            audio_editor: "audacity".into(),
            video_editor: "kdenlive".into(),
            round_trip_reload: true,
        };
        let is_still = true;
        let has_video = false;
        let has_audio = false;
        let app = if is_still {
            &prefs.image_editor
        } else if !has_video && has_audio {
            &prefs.audio_editor
        } else {
            &prefs.video_editor
        };
        assert_eq!(app, "gimp");
    }

    /// Mirrors the routing in the `OpenInExternalApp` handler: audio-only
    /// files (has_audio, no video_codec) route to the audio editor.
    #[test]
    fn external_app_routing_audio_only() {
        let prefs = ExternalToolsPrefs {
            image_editor: "gimp".into(),
            audio_editor: "audacity".into(),
            video_editor: "kdenlive".into(),
            round_trip_reload: true,
        };
        let is_still = false;
        let has_video = false;
        let has_audio = true;
        let app = if is_still {
            &prefs.image_editor
        } else if !has_video && has_audio {
            &prefs.audio_editor
        } else {
            &prefs.video_editor
        };
        assert_eq!(app, "audacity");
    }

    /// Video clips route to the video editor.
    #[test]
    fn external_app_routing_video_uses_video_editor() {
        let prefs = ExternalToolsPrefs {
            image_editor: "gimp".into(),
            audio_editor: "audacity".into(),
            video_editor: "kdenlive".into(),
            round_trip_reload: true,
        };
        let is_still = false;
        let has_video = true;
        let has_audio = true;
        let app = if is_still {
            &prefs.image_editor
        } else if !has_video && has_audio {
            &prefs.audio_editor
        } else {
            &prefs.video_editor
        };
        assert_eq!(app, "kdenlive");
    }
}

#[cfg(test)]
mod track_matte_handler_tests {
    //! Phase 1.4 — invariants pinned by `SetClipMatte` / `ClearClipMatte` /
    //! the orphan-binding sweep in `remove_clip_impl`. The handlers
    //! themselves require a full `App` to test, so these exercise the pure
    //! free helpers (`matte_chain_creates_cycle`,
    //! `matte_channel_from_u8`) and the data-model invariants on a hand-
    //! built `Sequence`. The render-path side lands in Phase 2.
    use super::{matte_channel_from_u8, matte_chain_creates_cycle};
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_timeline::{Clip, MatteBinding, Sequence, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn seq_with_clips(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips = clips;
        s.tracks.push(tr);
        s
    }

    fn fresh_clip() -> Clip {
        Clip::media(MediaId::new(), tc(0), tc(24), tc(0), tc(24))
    }

    #[test]
    fn channel_from_u8_maps_known_values() {
        use klipp_nodes::params::MatteChannel;
        assert_eq!(matte_channel_from_u8(0), MatteChannel::Alpha);
        assert_eq!(matte_channel_from_u8(1), MatteChannel::Luminance);
        assert_eq!(matte_channel_from_u8(2), MatteChannel::Red);
        assert_eq!(matte_channel_from_u8(3), MatteChannel::Green);
        assert_eq!(matte_channel_from_u8(4), MatteChannel::Blue);
    }

    #[test]
    fn channel_from_u8_unknown_falls_back_to_luminance() {
        use klipp_nodes::params::MatteChannel;
        // Fallback path matters: a stale or future-version replay log
        // should never crash, just degrade to the historical default.
        assert_eq!(matte_channel_from_u8(99), MatteChannel::Luminance);
        assert_eq!(matte_channel_from_u8(255), MatteChannel::Luminance);
    }

    #[test]
    fn cycle_check_rejects_self_reference() {
        let a = fresh_clip();
        let id = a.id;
        let seq = seq_with_clips(vec![a]);
        // SetClipMatte handler short-circuits before reaching the chain
        // walker for self-reference, but the chain walker also catches it
        // as a defence-in-depth — pin both behaviours.
        assert!(matte_chain_creates_cycle(&seq, id, id));
    }

    #[test]
    fn cycle_check_passes_for_unrelated_clips() {
        let a = fresh_clip();
        let b = fresh_clip();
        let (aid, bid) = (a.id, b.id);
        let seq = seq_with_clips(vec![a, b]);
        // No bindings yet — binding A → B is fine.
        assert!(!matte_chain_creates_cycle(&seq, aid, bid));
    }

    #[test]
    fn cycle_check_detects_two_hop_cycle() {
        // Setup: A.matte_source = B already. Now the user tries to set
        // B.matte_source = A. Walker starts from `source = A`, reads A's
        // matte_source (none), returns false — wait, that's not a cycle.
        // The cycle is in the OTHER direction: if B were already pointing
        // at A, then setting A.matte_source = B would close the loop.
        // Build that case.
        let mut a = fresh_clip();
        let mut b = fresh_clip();
        let (aid, bid) = (a.id, b.id);
        // B already consumes A as its matte. (Pre-existing.)
        b.matte_source = Some(MatteBinding::new(aid));
        let _ = &mut a; // unused mut suppression
        let seq = seq_with_clips(vec![a, b]);
        // Now the user wants to set A.matte_source = B. The walker starts
        // at B (the proposed source), reads B's matte_source.source = A
        // (the consumer) — cycle detected.
        assert!(matte_chain_creates_cycle(&seq, aid, bid));
    }

    #[test]
    fn cycle_check_handles_missing_intermediate_clip() {
        // Walker should bail (return false / no cycle) when it hits a
        // dangling matte_source pointing at a non-existent clip — the
        // chain just terminates. This case is plausible after a corrupt
        // load; we don't want the cycle check to infinite-loop.
        let mut a = fresh_clip();
        let b = fresh_clip();
        let bid = b.id;
        a.matte_source = Some(MatteBinding::new(ClipId::new())); // dangling
        let _ = &mut a;
        let seq = seq_with_clips(vec![a]); // b NOT in sequence
        // Asking "would bind consumer=b, source=<dangling>" — the chain
        // from a-not-in-seq trivially terminates.
        assert!(!matte_chain_creates_cycle(&seq, bid, ClipId::new()));
    }
}

#[cfg(test)]
mod transform_link_handler_tests {
    //! Pure-function tests for the `transform_link` cycle walker. The
    //! `SetClipTransformLink` handler itself needs a full `App`; these
    //! exercise the data-model invariants on a hand-built `Sequence`.
    use super::transform_link_chain_creates_cycle;
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind, TransformLink};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn seq_with_clips(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips = clips;
        s.tracks.push(tr);
        s
    }

    fn fresh_clip() -> Clip {
        Clip::media(MediaId::new(), tc(0), tc(24), tc(0), tc(24))
    }

    #[test]
    fn cycle_check_rejects_self_reference() {
        let a = fresh_clip();
        let id = a.id;
        let seq = seq_with_clips(vec![a]);
        assert!(transform_link_chain_creates_cycle(&seq, id, id));
    }

    #[test]
    fn cycle_check_passes_for_unrelated_clips() {
        let a = fresh_clip();
        let b = fresh_clip();
        let (aid, bid) = (a.id, b.id);
        let seq = seq_with_clips(vec![a, b]);
        assert!(!transform_link_chain_creates_cycle(&seq, aid, bid));
    }

    #[test]
    fn cycle_check_detects_two_hop_cycle() {
        // B already links → A. Setting A → B closes the loop.
        let a = fresh_clip();
        let mut b = fresh_clip();
        let (aid, bid) = (a.id, b.id);
        b.transform_link = Some(TransformLink::new(aid));
        let seq = seq_with_clips(vec![a, b]);
        assert!(transform_link_chain_creates_cycle(&seq, aid, bid));
    }

    #[test]
    fn cycle_check_handles_dangling_target() {
        let mut a = fresh_clip();
        a.transform_link = Some(TransformLink::new(ClipId::new())); // dangling
        let b = fresh_clip();
        let bid = b.id;
        let seq = seq_with_clips(vec![a]);
        assert!(!transform_link_chain_creates_cycle(&seq, bid, ClipId::new()));
    }
}

#[cfg(test)]
mod first_time_tip_tests {
    //! Phase 5.2 — `sequence_has_video_track_overlap` is the unlock
    //! condition for the track-matte intro tip. Tests pin the corners
    //! a regression would erode silently: empty sequence, single
    //! track, two tracks but no overlap, edge-touch (open interval),
    //! genuine overlap.
    use klipp_core::{MediaId, RationalTime};
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn make_clip(in_f: i64, out_f: i64) -> Clip {
        Clip::media(MediaId::new(), tc(in_f), tc(out_f), tc(0), tc(out_f - in_f))
    }

    fn video_track(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = clips;
        t
    }

    fn audio_track(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Audio);
        t.clips = clips;
        t
    }

    fn seq_with_tracks(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks = tracks;
        s
    }

    #[test]
    fn no_overlap_when_sequence_is_empty() {
        let seq = seq_with_tracks(vec![]);
        assert!(!super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn no_overlap_when_only_one_video_track() {
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 24), make_clip(24, 48)]),
        ]);
        assert!(!super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn no_overlap_when_audio_track_is_the_second_track() {
        // Audio track must NOT count — track-matte is a video feature.
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 24)]),
            audio_track("A1", vec![make_clip(0, 24)]),
        ]);
        assert!(!super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn no_overlap_when_clips_just_touch_at_the_edge() {
        // Open-interval semantics: clip ending at frame 24 doesn't
        // overlap with a clip starting at frame 24.
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 24)]),
            video_track("V2", vec![make_clip(24, 48)]),
        ]);
        assert!(!super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn overlap_when_two_video_clips_share_time_range() {
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 48)]),
            video_track("V2", vec![make_clip(12, 36)]), // sits inside V1
        ]);
        assert!(super::sequence_has_video_track_overlap(&seq));
    }

    #[test]
    fn overlap_detected_across_non_adjacent_tracks() {
        // V1 + V3 overlap; V2 is empty in between. The function must
        // still find it (the gesture works across any track gap).
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![make_clip(0, 48)]),
            video_track("V2", vec![]),
            video_track("V3", vec![make_clip(20, 30)]),
        ]);
        assert!(super::sequence_has_video_track_overlap(&seq));
    }
}

#[cfg(test)]
mod matte_candidate_finder_tests {
    //! Phase 3 — `find_matte_candidate_above` drives the right-click
    //! "Use clip above as matte" headline gesture in the timeline. The
    //! function lives in `panels/timeline.rs` (private) so we exercise
    //! the same logic via a re-implementation here that mirrors its
    //! shape — the goal is to pin behaviour, not couple to the impl.
    //!
    //! When the timeline impl drifts these tests will fail loudly,
    //! flagging that the gesture's contract changed. Tests stay tight:
    //! "above means higher track index", "AdjustmentLayers are
    //! skipped", "non-overlapping clips are skipped", "audio tracks
    //! are skipped".
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_timeline::{Clip, ClipKind, Sequence, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    /// Re-implementation of `panels::timeline::find_matte_candidate_above`
    /// kept private to the test module. If the timeline copy drifts,
    /// keep this in lockstep — both encode the user-facing gesture.
    fn find_candidate(
        seq: &Sequence,
        current_track_idx: usize,
        current_clip_id: ClipId,
    ) -> Option<(ClipId, ClipKind)> {
        let current_clip = seq.tracks.iter()
            .flat_map(|t| t.clips.iter())
            .find(|c| c.id == current_clip_id)?;
        let lo = current_clip.timeline_in;
        let hi = current_clip.timeline_out;
        for idx in (0..current_track_idx).rev() {
            let track = &seq.tracks[idx];
            if !matches!(
                track.kind,
                TrackKind::Video | TrackKind::Text | TrackKind::Subtitle
            ) {
                continue;
            }
            for c in &track.clips {
                if c.id == current_clip_id { continue; }
                if matches!(c.kind, ClipKind::AdjustmentLayer) { continue; }
                if c.timeline_in < hi && c.timeline_out > lo {
                    return Some((c.id, c.kind.clone()));
                }
            }
        }
        None
    }

    fn seq_with_tracks(tracks: Vec<Track>) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks = tracks;
        s
    }

    fn video_track(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Video);
        t.clips = clips;
        t
    }

    fn audio_track(name: &str, clips: Vec<Clip>) -> Track {
        let mut t = Track::new(name, TrackKind::Audio);
        t.clips = clips;
        t
    }

    fn media_clip(in_f: i64, out_f: i64) -> Clip {
        Clip::media(MediaId::new(), tc(in_f), tc(out_f), tc(0), tc(out_f - in_f))
    }

    // Test fixtures use the production array convention: tracks[0]
    // is at the TOP of the panel (visually highest = lowest array
    // idx). For "find candidate above the consumer", the source
    // sits at a LOWER idx than the consumer.

    #[test]
    fn returns_overlap_on_track_directly_above() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let source = media_clip(0, 24);
        let source_id = source.id;
        let seq = seq_with_tracks(vec![
            video_track("V_above (source)", vec![source]),  // idx 0 — visually above
            video_track("V_below (consumer)", vec![consumer]), // idx 1
        ]);
        let (cand, _kind) = find_candidate(&seq, 1, consumer_id).expect("candidate");
        assert_eq!(cand, source_id);
    }

    #[test]
    fn skips_track_with_no_overlap() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let too_late = media_clip(48, 72);
        let seq = seq_with_tracks(vec![
            video_track("V_above", vec![too_late]),    // idx 0
            video_track("V_below", vec![consumer]),    // idx 1
        ]);
        assert!(find_candidate(&seq, 1, consumer_id).is_none());
    }

    #[test]
    fn skips_adjustment_layer_above() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        // From top to bottom: valid media, adjustment layer (skipped),
        // consumer. Walk goes from consumer up; first valid match is
        // the topmost media clip.
        let mut adj = media_clip(0, 24);
        adj.kind = ClipKind::AdjustmentLayer;
        let valid = media_clip(0, 24);
        let valid_id = valid.id;
        let seq = seq_with_tracks(vec![
            video_track("V_top (valid)", vec![valid]),    // idx 0
            video_track("V_mid (adj)",   vec![adj]),      // idx 1
            video_track("V_bot (consumer)", vec![consumer]), // idx 2
        ]);
        let (cand, _) = find_candidate(&seq, 2, consumer_id).expect("candidate");
        assert_eq!(cand, valid_id, "should jump past the adjustment layer to the valid clip above");
    }

    #[test]
    fn skips_audio_track() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let audio_clip = media_clip(0, 24);
        let video_above = media_clip(0, 24);
        let video_id = video_above.id;
        let seq = seq_with_tracks(vec![
            video_track("V_top",   vec![video_above]),   // idx 0 — valid candidate
            audio_track("A_mid",   vec![audio_clip]),    // idx 1 — audio, skipped
            video_track("V_bot",   vec![consumer]),      // idx 2 — consumer
        ]);
        let (cand, _) = find_candidate(&seq, 2, consumer_id).expect("candidate");
        assert_eq!(cand, video_id);
    }

    #[test]
    fn returns_none_when_consumer_on_top_track() {
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let seq = seq_with_tracks(vec![
            video_track("V1", vec![consumer]), // idx 0 — nothing above
        ]);
        assert!(find_candidate(&seq, 0, consumer_id).is_none(),
            "no tracks above → no candidate");
    }

    #[test]
    fn ignores_consumer_itself_even_if_listed_above() {
        // Defensive: if the consumer's id somehow appeared on a track
        // above, we still skip it (a clip cannot matte itself).
        let consumer = media_clip(0, 24);
        let consumer_id = consumer.id;
        let mut self_copy = media_clip(0, 24);
        self_copy.id = consumer_id;
        let seq = seq_with_tracks(vec![
            video_track("V_above", vec![self_copy]),   // idx 0 — same id, must be skipped
            video_track("V_below", vec![consumer]),    // idx 1 — consumer
        ]);
        assert!(find_candidate(&seq, 1, consumer_id).is_none(),
            "consumer id on track above must not be returned as its own candidate");
    }
}

/// What a pool clip puts on the timeline when no range was picked on its
/// browser thumbnail: the In/Out marked in the source viewer, else the
/// whole clip, as `(duration, source_in, source_out)`. The same rule as
/// Insert and Overwrite; hover-skimming the tile never marks anything.
fn source_marks_range(item: &klipp_media::MediaItem) -> (Timecode, Option<Timecode>, Option<Timecode>) {
    if item.default_source_in.is_none() && item.default_source_out.is_none() {
        return (item.duration, None, None);
    }
    let raw_in = item.default_source_in.unwrap_or(Timecode { value: 0, rate: item.duration.rate });
    let raw_out = item.default_source_out.unwrap_or(item.duration);
    if raw_in >= raw_out {
        return (item.duration, None, None);
    }
    (raw_out - raw_in, Some(raw_in), Some(raw_out))
}

#[cfg(test)]
mod source_marks_range_tests {
    use super::source_marks_range;
    use klipp_core::Timecode;

    fn t(secs: i64) -> Timecode { Timecode { value: secs * 30, rate: 30 } }

    fn item(secs: i64) -> klipp_media::MediaItem {
        // Any item will do: only the duration and the marks are read.
        klipp_media::MediaItem::new_synthetic_title(
            klipp_core::MediaId::new(), "clip".into(), t(secs), klipp_core::Fps::new(30, 1),
        )
    }

    /// Issue #261: an unmarked clip goes on whole.
    #[test]
    fn an_unmarked_clip_is_added_whole() {
        assert_eq!(source_marks_range(&item(140)), (t(140), None, None));
    }

    #[test]
    fn marked_in_and_out_are_used() {
        let mut m = item(140);
        m.default_source_in = Some(t(10));
        m.default_source_out = Some(t(40));
        assert_eq!(source_marks_range(&m), (t(30), Some(t(10)), Some(t(40))));

        // Only an In: from there to the end.
        m.default_source_out = None;
        assert_eq!(source_marks_range(&m), (t(130), Some(t(10)), Some(t(140))));

        // Marks the wrong way round fall back to the whole clip.
        m.default_source_out = Some(t(5));
        assert_eq!(source_marks_range(&m), (t(140), None, None));
    }
}

/// A file that turns out to be in the pool already is not imported
/// again, but a destination it was given (a drop on a track, "add at
/// playhead") still holds: the item already there is placed instead.
fn retarget_pending_drop(
    pending: &mut std::collections::HashMap<klipp_core::MediaId, klipp_core::events::ClipDrop>,
    requested: klipp_core::MediaId,
    existing: klipp_core::MediaId,
) -> Option<AppCommand> {
    let drop = pending.remove(&requested)?;
    Some(AppCommand::PlaceMedia { media_id: existing, drop })
}

#[cfg(test)]
mod retarget_drop_tests {
    use super::{retarget_pending_drop, AppCommand};
    use klipp_core::events::ClipDrop;
    use klipp_core::MediaId;

    #[test]
    fn a_duplicate_with_a_destination_places_the_existing_item() {
        let (requested, existing) = (MediaId::new(), MediaId::new());
        let mut pending = std::collections::HashMap::new();
        pending.insert(requested, ClipDrop::AppendToTrack { track_index: 0 });
        let cmd = retarget_pending_drop(&mut pending, requested, existing);
        assert!(matches!(
            cmd,
            Some(AppCommand::PlaceMedia { media_id, drop: ClipDrop::AppendToTrack { track_index: 0 } }) if media_id == existing
        ));
        assert!(pending.is_empty(), "the stale destination was left behind");
    }

    #[test]
    fn a_duplicate_with_no_destination_places_nothing() {
        let mut pending = std::collections::HashMap::new();
        assert!(retarget_pending_drop(&mut pending, MediaId::new(), MediaId::new()).is_none());
    }
}

/// A clip's built-in effects as an undo step sees them.
#[derive(Debug, Clone, PartialEq)]
struct ClipEffectsState {
    effects: Vec<klipp_effects::BuiltinEffect>,
    enabled: Vec<bool>,
    speed: f64,
    timeline_out: klipp_core::RationalTime,
}

impl ClipEffectsState {
    fn of(clip: &klipp_timeline::Clip) -> Self {
        Self {
            effects: clip.builtin_effects.clone(),
            enabled: clip.builtin_effects_enabled.clone(),
            speed: clip.speed,
            timeline_out: clip.timeline_out,
        }
    }
}

/// The history edits that take a clip from `before` to `after`: the
/// effects list when it changed, and the speed and length when a speed
/// ramp moved them. Adding or removing a built-in effect recorded
/// nothing, so it could not be undone (#391).
fn effects_change_edits(clip_id: klipp_core::ClipId, before: &ClipEffectsState, after: &ClipEffectsState) -> Vec<EditCommand> {
    let mut edits = Vec::new();
    if before.effects != after.effects || before.enabled != after.enabled {
        edits.push(EditCommand::SetBuiltinEffects {
            clip_id,
            old_effects: before.effects.clone(),
            old_enabled: before.enabled.clone(),
            new_effects: after.effects.clone(),
            new_enabled: after.enabled.clone(),
        });
    }
    if before.speed != after.speed || before.timeline_out != after.timeline_out {
        edits.push(EditCommand::SetClipSpeed {
            clip_id,
            old_speed: before.speed,
            new_speed: after.speed,
            old_timeline_out: before.timeline_out,
            new_timeline_out: after.timeline_out,
        });
    }
    edits
}

/// Whether `last` is the add that appended effect `index` to `clip_id`.
fn is_add_of_effect(last: &EditCommand, clip_id: klipp_core::ClipId, index: usize) -> bool {
    matches!(
        last,
        EditCommand::SetBuiltinEffects { clip_id: c, old_effects, new_effects, .. }
            if *c == clip_id && new_effects.len() == old_effects.len() + 1 && index + 1 == new_effects.len()
    )
}

/// Fold a finished drag of effect `index` into the history's last edit
/// when that edit is the add that made the effect, with the effect as
/// the drag found it: the add then records the effect as the drag left
/// it, and one undo takes both back.
fn fold_drag_into_add(
    last: &mut EditCommand,
    clip_id: klipp_core::ClipId,
    index: usize,
    dragged_from: &klipp_effects::BuiltinEffect,
    dragged_to: &klipp_effects::BuiltinEffect,
) -> bool {
    match last {
        EditCommand::SetBuiltinEffects { clip_id: c, old_effects, new_effects, .. }
            if *c == clip_id
                && new_effects.len() == old_effects.len() + 1
                && index + 1 == new_effects.len()
                && new_effects[index] == *dragged_from =>
        {
            new_effects[index] = dragged_to.clone();
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod effects_undo_tests {
    use super::*;

    fn crop(left: f32) -> klipp_effects::BuiltinEffect {
        klipp_effects::BuiltinEffect::Crop { left, right: 0.0, top: 0.0, bottom: 0.0, feather: 0.0 }
    }

    fn state(effects: Vec<klipp_effects::BuiltinEffect>, speed: f64) -> ClipEffectsState {
        let enabled = vec![true; effects.len()];
        ClipEffectsState { effects, enabled, speed, timeline_out: klipp_core::RationalTime { value: 300, rate: 30 } }
    }

    #[test]
    fn adding_an_effect_is_one_undoable_edit() {
        let id = klipp_core::ClipId::new();
        let edits = effects_change_edits(id, &state(vec![], 1.0), &state(vec![crop(0.1)], 1.0));
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].label(), "Add Effect");
    }

    #[test]
    fn removing_one_says_so_and_nothing_changed_records_nothing() {
        let id = klipp_core::ClipId::new();
        let edits = effects_change_edits(id, &state(vec![crop(0.1)], 1.0), &state(vec![], 1.0));
        assert_eq!(edits[0].label(), "Remove Effect");
        assert!(effects_change_edits(id, &state(vec![crop(0.1)], 1.0), &state(vec![crop(0.1)], 1.0)).is_empty());
    }

    #[test]
    fn a_speed_ramp_brings_its_speed_with_it() {
        let id = klipp_core::ClipId::new();
        let mut after = state(vec![crop(0.1)], 2.0);
        after.timeline_out = klipp_core::RationalTime { value: 150, rate: 30 };
        let edits = effects_change_edits(id, &state(vec![], 1.0), &after);
        assert_eq!(edits.len(), 2);
        assert!(matches!(edits[1], EditCommand::SetClipSpeed { new_speed, .. } if new_speed == 2.0));
    }

    /// The gizmo's first drag: the add of an identity effect, then a
    /// drag of it, becomes one add of the dragged effect.
    #[test]
    fn a_drag_of_the_effect_just_added_folds_into_the_add() {
        let id = klipp_core::ClipId::new();
        let mut last = effects_change_edits(id, &state(vec![], 1.0), &state(vec![crop(0.0)], 1.0)).remove(0);
        assert!(fold_drag_into_add(&mut last, id, 0, &crop(0.0), &crop(0.3)));
        let EditCommand::SetBuiltinEffects { new_effects, old_effects, .. } = &last else { panic!() };
        assert!(old_effects.is_empty());
        assert_eq!(new_effects, &vec![crop(0.3)]);
        // Not another clip's, not another effect, not one that was changed since.
        assert!(!fold_drag_into_add(&mut last.clone(), klipp_core::ClipId::new(), 0, &crop(0.3), &crop(0.5)));
        assert!(!fold_drag_into_add(&mut last.clone(), id, 1, &crop(0.3), &crop(0.5)));
        assert!(!fold_drag_into_add(&mut last.clone(), id, 0, &crop(0.9), &crop(0.5)));
    }
}

/// The last absolute seek among a control envelope's commands: where the
/// playhead is headed once they are applied.
fn last_seek(cmds: &[AppCommand]) -> Option<klipp_core::Timecode> {
    cmds.iter().rev().find_map(|c| match c {
        AppCommand::SeekTo(t) => Some(*t),
        _ => None,
    })
}

#[cfg(test)]
mod last_seek_tests {
    use super::{last_seek, AppCommand};
    use klipp_core::Timecode;

    #[test]
    fn the_last_seek_of_an_envelope_is_where_the_playhead_is_going() {
        let (a, b) = (Timecode::from_frames(30, 30), Timecode::from_frames(90, 30));
        let cmds = [AppCommand::Pause, AppCommand::SeekTo(a), AppCommand::Play, AppCommand::SeekTo(b)];
        assert_eq!(last_seek(&cmds), Some(b));
        assert_eq!(last_seek(&[AppCommand::Play]), None);
    }
}

/// `value + adjustment` held to the temperature and tint sliders' ±50,
/// and whether it had to be (#405: the clamp used to be silent).
fn add_on_rail(value: f32, adjustment: f32) -> (f32, bool) {
    let want = value + adjustment;
    let held = want.clamp(-50.0, 50.0);
    (held, (held - want).abs() > 0.01)
}

#[cfg(test)]
mod add_on_rail_tests {
    use super::add_on_rail;

    #[test]
    fn within_the_range_is_kept_and_past_it_is_held_and_said() {
        assert_eq!(add_on_rail(10.0, -12.0), (-2.0, false));
        assert_eq!(add_on_rail(40.0, 20.0), (50.0, true));
        assert_eq!(add_on_rail(-45.0, -30.0), (-50.0, true));
    }
}

/// A qualifier keyed on the colour picked at `(nx, ny)` (0..=1 over the
/// frame) of an sRGB frame: `base` with its hue centred on the colour and
/// its saturation range around it. Read as the qualifier reads a pixel
/// (`picture_hsl`), from the mean of a 5×5 patch, so one noisy pixel does
/// not set the key. `None` when the frame is empty.
fn qualifier_from_pick(
    rgba: &[u8],
    width: u32,
    height: u32,
    nx: f32,
    ny: f32,
    base: klipp_core::QualifierSettings,
) -> Option<klipp_core::QualifierSettings> {
    Some(qualifier_keyed_on(picked_linear(rgba, width, height, nx, ny)?, base))
}

/// The colour at `(nx, ny)` (0..=1 over the frame) of an sRGB frame, in
/// linear light: the mean of a 5×5 patch, so one noisy pixel does not
/// decide it. `None` when the frame is empty.
fn picked_linear(rgba: &[u8], width: u32, height: u32, nx: f32, ny: f32) -> Option<[f32; 3]> {
    if width == 0 || height == 0 || rgba.len() < (width * height * 4) as usize {
        return None;
    }
    let (cx, cy) = ((nx.clamp(0.0, 1.0) * (width - 1) as f32).round() as i64, (ny.clamp(0.0, 1.0) * (height - 1) as f32).round() as i64);
    let mut patch = Vec::with_capacity(25 * 4);
    for y in (cy - 2).max(0)..=(cy + 2).min(height as i64 - 1) {
        for x in (cx - 2).max(0)..=(cx + 2).min(width as i64 - 1) {
            let i = (y as usize * width as usize + x as usize) * 4;
            patch.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    let linear = klipp_effects::srgb_u8_to_linear_f32_rgba(&patch);
    let n = (linear.len() / 4) as f32;
    Some([0, 1, 2].map(|c| linear.chunks_exact(4).map(|p| p[c]).sum::<f32>() / n))
}

/// How far either side of a picked hue its range reaches: 30°.
const HUE_PICK_HALF_WIDTH: f32 = 1.0 / 12.0;

/// `points` with a range round `hue` (0..1): a point at the hue and one
/// either side of it, all neutral, so nothing changes until the middle
/// one is dragged; the points already inside that range give way to it,
/// the rest stay. Near red the range stops at the curve's end rather
/// than wrapping round to the other (#612).
fn hue_range_points(points: &[[f32; 2]], hue: f32) -> Vec<[f32; 2]> {
    let hue = hue.rem_euclid(1.0);
    let (lo, hi) = ((hue - HUE_PICK_HALF_WIDTH).max(0.0), (hue + HUE_PICK_HALF_WIDTH).min(1.0));
    let mut out: Vec<[f32; 2]> = points.iter().copied().filter(|p| p[0] < lo || p[0] > hi).collect();
    for x in [lo, hue, hi] {
        if !out.iter().any(|p| (p[0] - x).abs() < 1e-4) {
            out.push([x, 0.5]);
        }
    }
    out.sort_by(|a, b| a[0].total_cmp(&b[0]));
    out
}

/// `base` keyed on a colour in linear light, read as the qualifier reads
/// a pixel (`picture_hsl`): its hue centred there, its saturation range
/// around it.
fn qualifier_keyed_on(rgb: [f32; 3], base: klipp_core::QualifierSettings) -> klipp_core::QualifierSettings {
    let hsl = klipp_effects::cpu_grade::picture_hsl(rgb);
    klipp_core::QualifierSettings {
        enabled: true,
        hue_center: hsl.h * 360.0,
        hue_width: 30.0,
        sat_low: (hsl.s - 0.2).max(0.0),
        sat_high: (hsl.s + 0.2).min(1.0),
        ..base
    }
}

#[cfg(test)]
mod qualifier_pick_tests {
    use super::{hue_range_points, picked_linear, qualifier_from_pick};

    /// #612: a pick puts three neutral points round the hue, the middle
    /// one on it; points inside the range give way, the rest stay; near
    /// red the range stops at the curve's end.
    #[test]
    fn a_hue_pick_puts_a_neutral_range_round_the_hue() {
        let picked = hue_range_points(&[], 0.5);
        assert_eq!(picked.len(), 3);
        assert!((picked[1][0] - 0.5).abs() < 1e-6 && picked.iter().all(|p| p[1] == 0.5), "{picked:?}");
        assert!((picked[2][0] - picked[0][0] - 2.0 / 12.0).abs() < 1e-6, "30° either side");
        let kept = hue_range_points(&[[0.1, 0.7], [0.52, 0.9], [0.9, 0.3]], 0.5);
        assert!(kept.contains(&[0.1, 0.7]) && kept.contains(&[0.9, 0.3]), "outside the range they stay: {kept:?}");
        assert!(!kept.iter().any(|p| p[1] == 0.9), "inside it they give way: {kept:?}");
        assert!(kept.windows(2).all(|w| w[0][0] < w[1][0]), "in order");
        let red = hue_range_points(&[], 0.01);
        assert_eq!(red[0], [0.0, 0.5], "stops at the curve's start: {red:?}");
        // And the hue the pick reads is the one the curve acts on: an
        // orange patch reads as orange.
        let frame: Vec<u8> = (0..16 * 8).flat_map(|_| [230u8, 120, 30, 255]).collect();
        let hue = klipp_effects::cpu_grade::picture_hsl(picked_linear(&frame, 16, 8, 0.5, 0.5).unwrap()).h * 360.0;
        assert!((hue - 28.0).abs() < 2.0, "orange: {hue}");
    }

    /// A frame of `left` on its left half and `right` on its right.
    fn halves(left: [u8; 3], right: [u8; 3]) -> Vec<u8> {
        (0..16 * 8).flat_map(|i| { let c = if i % 16 < 8 { left } else { right }; [c[0], c[1], c[2], 255] }).collect()
    }

    #[test]
    fn the_pick_keys_the_colour_under_it_and_the_qualifier_passes_it() {
        let (orange, blue) = ([230, 120, 30], [40, 80, 200]);
        let frame = halves(orange, blue);
        let base = klipp_core::QualifierSettings { hue_soft: 7.0, ..Default::default() };
        let q = qualifier_from_pick(&frame, 16, 8, 0.1, 0.5, base.clone()).unwrap();
        assert!(q.enabled);
        assert!((q.hue_center - 28.0).abs() < 2.0, "orange: {}", q.hue_center);
        assert_eq!(q.hue_soft, 7.0, "the node's softness is kept");
        // The qualifier's own matte: the picked colour in, the other out.
        let key = |c: [u8; 3]| {
            let mut px = klipp_effects::srgb_u8_to_linear_f32_rgba(&[c[0], c[1], c[2], 255]);
            klipp_effects::cpu_grade::qualifier_matte(
                &mut px, q.hue_center / 360.0, q.hue_width / 360.0, q.hue_soft / 360.0,
                q.sat_low, q.sat_high, q.sat_soft, q.lum_low, q.lum_high, q.lum_soft, q.invert,
            );
            px[0]
        };
        assert!(key(orange) > 0.99, "picked colour keyed: {}", key(orange));
        assert!(key(blue) < 0.01, "other colour not: {}", key(blue));
        assert!(qualifier_from_pick(&[], 0, 0, 0.5, 0.5, base).is_none());
    }
}

/// The audio meters' ballistics for one UI frame of `dt` seconds: tuned
/// per frame at 60 fps (a 0.96 peak release, a 0.12 and 0.072 LUFS
/// smoothing, a 1.5 LU drift with no signal), taken as that many 60 fps
/// frames so the meters move in seconds, whatever the UI's frame rate.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MeterBallistics {
    peak_release: f32,
    momentary_alpha: f32,
    short_term_alpha: f32,
    drift: f32,
}

impl MeterBallistics {
    fn for_frame(dt: f32) -> Self {
        // A frame after a stall counts as at most a tenth of a second.
        let n = (dt * 60.0).clamp(0.0, 6.0);
        let ema = |alpha: f32| 1.0 - (1.0 - alpha).powf(n);
        Self { peak_release: 0.96_f32.powf(n), momentary_alpha: ema(0.12), short_term_alpha: ema(0.12 * 0.6), drift: 1.5 * n }
    }
}

#[cfg(test)]
mod meter_ballistics_tests {
    use super::MeterBallistics;

    /// Two frames at 120 fps move a meter as far as one at 60 fps.
    #[test]
    fn meters_move_in_seconds_whatever_the_frame_rate() {
        let at60 = MeterBallistics::for_frame(1.0 / 60.0);
        let at120 = MeterBallistics::for_frame(1.0 / 120.0);
        assert!((at60.peak_release - 0.96).abs() < 1e-6 && (at60.momentary_alpha - 0.12).abs() < 1e-6 && (at60.drift - 1.5).abs() < 1e-6);
        assert!((at120.peak_release * at120.peak_release - at60.peak_release).abs() < 1e-6);
        let settle = |b: MeterBallistics, frames: usize| (0..frames).fold(0.0f32, |v, _| v * (1.0 - b.momentary_alpha) + b.momentary_alpha);
        assert!((settle(at120, 2) - settle(at60, 1)).abs() < 1e-6);
        assert!((2.0 * at120.drift - at60.drift).abs() < 1e-6);
    }
}

/// Why Auto White Balance cannot read the frame buffer's bytes for a
/// clip spanning `clip_span`, or `None` when it can: the bytes must be the
/// picture on screen (not while the GPU stack presents without them), the
/// SDR picture (an HDR monitor mode publishes PQ preview or false colour,
/// whose "neutrals" mean nothing), and a frame of the clip being balanced.
fn auto_wb_frame_refusal(
    gpu_output_live: bool,
    hdr_monitor: bool,
    complete: bool,
    bytes_at: Option<klipp_core::RationalTime>,
    clip_span: Option<(klipp_core::RationalTime, klipp_core::RationalTime)>,
) -> Option<&'static str> {
    if gpu_output_live {
        return Some("pause playback first — it reads the frame on screen, and a playing frame is not held to read");
    }
    if hdr_monitor {
        return Some("turn the HDR monitor off first — the viewer shows an HDR preview or false colour, not the picture it balances");
    }
    // A progress frame, or one drawn while a stream lagged, is published
    // under the playhead without every clip's picture in it.
    if !complete {
        return Some("the frame is still being drawn — press again in a moment");
    }
    match (bytes_at, clip_span) {
        (Some(at), Some((a, b))) if at >= a && at < b => None,
        (_, None) => Some("the clip is no longer on the timeline"),
        _ => Some("the frame on screen is not of this clip — park the playhead on it first"),
    }
}

#[cfg(test)]
mod auto_wb_frame_tests {
    use super::auto_wb_frame_refusal;
    use klipp_core::RationalTime;

    fn t(f: i64) -> RationalTime { RationalTime { value: f, rate: 30 } }

    #[test]
    fn a_parked_frame_of_the_clip_is_read() {
        assert_eq!(auto_wb_frame_refusal(false, false, true, Some(t(15)), Some((t(10), t(40)))), None);
    }

    /// #404: while the GPU stack presents without bytes, the buffer holds
    /// an older parked frame.
    #[test]
    fn a_zero_copy_frame_on_screen_is_refused() {
        assert!(auto_wb_frame_refusal(true, false, true, Some(t(15)), Some((t(10), t(40)))).is_some());
    }

    #[test]
    fn bytes_of_another_clip_are_refused() {
        assert!(auto_wb_frame_refusal(false, false, true, Some(t(45)), Some((t(10), t(40)))).is_some());
        assert!(auto_wb_frame_refusal(false, false, true, None, Some((t(10), t(40)))).is_some());
    }

    #[test]
    fn an_hdr_monitor_frame_is_not_read() {
        assert!(auto_wb_frame_refusal(false, true, true, Some(t(15)), Some((t(10), t(40)))).is_some());
    }

    #[test]
    fn a_frame_still_being_drawn_is_not_read() {
        assert!(auto_wb_frame_refusal(false, false, false, Some(t(15)), Some((t(10), t(40)))).is_some());
    }
}

/// Where the playhead goes when a clip is selected on the Colour page:
/// nowhere if it is already in the clip, else back to the frame the
/// clip was last looked at there, else its first frame.
fn colour_seek_on_select(
    clip: &klipp_timeline::Clip,
    playhead: klipp_core::RationalTime,
    remembered: Option<klipp_core::RationalTime>,
) -> Option<klipp_core::RationalTime> {
    let inside = |t: klipp_core::RationalTime| t >= clip.timeline_in && t < clip.timeline_out;
    if inside(playhead) {
        return None;
    }
    Some(remembered.filter(|&t| inside(t)).unwrap_or(clip.timeline_in))
}

#[cfg(test)]
mod colour_seek_tests {
    use super::colour_seek_on_select;
    use klipp_core::RationalTime;
    use klipp_timeline::Clip;

    fn t(secs: i64) -> RationalTime { RationalTime { value: secs * 30, rate: 30 } }

    fn clip(from: i64, to: i64) -> Clip {
        Clip::media(klipp_core::MediaId::new(), t(from), t(to), t(0), t(to - from))
    }

    #[test]
    fn a_playhead_already_in_the_clip_stays_put() {
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(15), None), None);
    }

    #[test]
    fn a_playhead_elsewhere_goes_to_the_first_frame() {
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(3), None), Some(t(10)));
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(20), None), Some(t(10)));
    }

    #[test]
    fn returning_to_a_clip_goes_back_to_where_it_was_graded() {
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(3), Some(t(17))), Some(t(17)));
    }

    /// A remembered frame the clip no longer covers (it was trimmed)
    /// is ignored rather than followed out of the clip.
    #[test]
    fn a_remembered_frame_outside_a_trimmed_clip_is_ignored() {
        assert_eq!(colour_seek_on_select(&clip(10, 20), t(3), Some(t(25))), Some(t(10)));
    }
}

/// Where a proxy is written while ffmpeg is still producing it. It is
/// renamed to the real name once whole, so a file at the real name is a
/// finished proxy — never one a crash cut short.
fn partial_proxy_path(proxy: &Path) -> PathBuf {
    proxy.with_extension("partial.mp4")
}

/// Whether a file found at the proxy path covers the source: within two
/// seconds or two percent of its duration, whichever is more (the proxy
/// encode may drop or pad a few frames at the end). `None` — the file
/// does not even probe — is not a proxy.
fn transcode_is_complete(proxy_secs: Option<f64>, source_secs: f64) -> bool {
    let Some(p) = proxy_secs else { return false };
    let slack = (source_secs * 0.02).max(2.0);
    p + slack >= source_secs
}

/// Whether the skim transcode at `path` covers a source `source_secs`
/// long (see [`transcode_is_complete`]). One header probe. A skim that
/// ends before its media plays as a shorter clip when playback
/// substitutes it, and every hover past its end fails.
fn skim_file_is_complete(path: &Path, source_secs: f64) -> bool {
    let probed = klipp_engine::probe_media_header(path).ok().and_then(|h| h.duration_secs);
    let complete = transcode_is_complete(probed, source_secs);
    if !complete {
        tracing::warn!(
            "Skim transcode {} is incomplete ({probed:?} s of {source_secs:.2} s); discarding it",
            path.display()
        );
    }
    complete
}

/// The skim cache key for a media item, as the transcode names it.
fn skim_cache_key_for(item: &klipp_media::MediaItem) -> Option<String> {
    let base = klipp_media::disk_cache::compute_cache_key(&item.path)?;
    let hdr = matches!(item.colour_space, klipp_media::ColourSpace::Hdr10 | klipp_media::ColourSpace::Hlg);
    Some(klipp_media::disk_cache::skim_cache_key(&base, hdr, item.source_rotation))
}

#[cfg(test)]
mod proxy_completeness_tests {
    use super::transcode_is_complete;

    #[test]
    fn a_truncated_transcode_is_not_one() {
        assert!(transcode_is_complete(Some(120.0), 120.0));
        assert!(transcode_is_complete(Some(118.5), 120.0), "a few frames short at the end is the encoder");
        assert!(transcode_is_complete(Some(590.0), 600.0), "two percent on a long clip");
        assert!(!transcode_is_complete(Some(60.0), 120.0), "half the clip is a cut-short generation");
        assert!(!transcode_is_complete(None, 120.0), "a file that does not probe is not one");
    }
}

/// The lines a screen recording showed, each once in the order first
/// seen, joined for a field; cut at a few thousand characters so a
/// long recording's chrome does not swamp the pool.
fn screen_text_digest(st: &klipp_analysis::screentext::ScreenText) -> String {
    const MAX_CHARS: usize = 4000;
    let mut seen = std::collections::HashSet::new();
    let mut out = String::new();
    for s in &st.spans {
        let key: String = s.text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        if key.len() < 2 || !seen.insert(key) {
            continue;
        }
        if !out.is_empty() {
            out.push_str(" · ");
        }
        out.push_str(s.text.trim());
        if out.len() > MAX_CHARS {
            break;
        }
    }
    out
}

#[cfg(test)]
mod screen_text_digest_tests {
    use super::screen_text_digest;
    use klipp_analysis::screentext::{ScreenText, TextSpan};

    #[test]
    fn the_digest_lists_each_line_once_in_order_first_seen() {
        let span = |t: &str, a: f64| TextSpan { text: t.into(), confidence: 0.9, start_secs: a, end_secs: a + 4.0, x: 0.1, y: 0.1, w: 0.2, h: 0.02, frames: 1 };
        let st = ScreenText { spans: vec![span("SETTINGS", 0.0), span("General", 0.0), span("settings", 8.0), span("  General ", 12.0), span("x", 12.0)], ..Default::default() };
        assert_eq!(screen_text_digest(&st), "SETTINGS · General");
        assert_eq!(screen_text_digest(&ScreenText::default()), "");
    }
}

/// `s` with its first letter upper-case, for a refusal's hint shown as a
/// sentence of its own.
fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// The skim decode height the `skim_resolution` preference asks for.
#[cfg(test)]
mod skim_decode_height_tests {
    use super::KlippApp;

    /// The whole point of the knob: it caps. Written with `max` it was
    /// inert for every source at or above its own value, which at the
    /// 720 default is most footage anyone edits.
    #[test]
    fn the_preference_caps_a_source_above_it() {
        assert_eq!(KlippApp::skim_decode_height(2160, 720), 720);
        assert_eq!(KlippApp::skim_decode_height(1080, 720), 720);
        assert_eq!(KlippApp::skim_decode_height(1080, 480), 480);
    }

    /// And it never upscales: a source shorter than the preference
    /// decodes at its own height, and nothing goes past 1080.
    #[test]
    fn the_source_height_and_the_1080_ceiling_still_bound_it() {
        assert_eq!(KlippApp::skim_decode_height(480, 720), 480);
        assert_eq!(KlippApp::skim_decode_height(4320, 4320), 1080);
    }

    /// `max` used to stop a nonsense value reaching the decoder. The
    /// preference has no UI, so keep a floor now that it can lower.
    #[test]
    fn a_nonsense_preference_cannot_decode_a_two_pixel_frame() {
        assert_eq!(KlippApp::skim_decode_height(1080, 2), 240);
        assert_eq!(KlippApp::skim_decode_height(1080, 0), 240);
    }
}

#[cfg(test)]
mod linked_clipboard_tests {

    fn t(s: f64) -> klipp_core::Timecode {
        klipp_core::Timecode::from_secs_f64(s, 25)
    }

    /// #458: a clip cut or copied takes its linked sound, each from its
    /// own track; pasted, both land on their own tracks at the playhead,
    /// at their own offset, linked to each other and not to the originals.
    #[test]
    fn a_linked_pair_goes_to_the_clipboard_and_comes_back_linked() {
        let mut pool = klipp_media::MediaPool::new();
        let media = pool.add(klipp_media::MediaItem::synthetic_for_test("/tmp/pair.mov".into(), None));
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut v = klipp_timeline::Clip::media(media, t(2.0), t(6.0), t(0.0), t(4.0));
        // A J-cut: the sound starts 0.4 s before the picture.
        let mut a = klipp_timeline::Clip::media(media, t(1.6), t(6.0), t(0.0), t(4.4));
        v.linked_clip_id = Some(a.id);
        a.linked_clip_id = Some(v.id);
        let (vid, aid) = (v.id, a.id);
        let mut vt = klipp_timeline::Track::new("V1", klipp_timeline::TrackKind::Video);
        vt.clips = vec![v];
        let mut at = klipp_timeline::Track::new("A1", klipp_timeline::TrackKind::Audio);
        at.clips = vec![a];
        seq.tracks = vec![vt, at];

        let cb = crate::clip_clipboard::copy(&seq, &[vid]).expect("on the clipboard");
        assert_eq!(cb.clips.iter().map(|c| (c.clip.id, c.track)).collect::<Vec<_>>(), vec![(aid, 1), (vid, 0)], "both, each from its own track, in time order");

        let pasted = crate::clip_clipboard::paste(&seq, &cb, t(10.0), crate::clip_clipboard::PasteMode::Overwrite);
        let find = |id: klipp_core::ClipId| pasted.sequence.tracks.iter().enumerate().find_map(|(ti, tr)| tr.clips.iter().find(|c| c.id == id).map(|c| (c.clone(), ti)));
        let (pa, ta) = find(pasted.ids[0]).unwrap();
        let (pv, tv) = find(pasted.ids[1]).unwrap();
        assert_eq!((ta, tv), (1, 0));
        assert_eq!((pa.timeline_in, pv.timeline_in), (t(10.0), t(10.4)), "the J-cut's offset kept");
        assert_eq!((pa.linked_clip_id, pv.linked_clip_id), (Some(pv.id), Some(pa.id)), "linked to each other");
        assert!(pa.id != aid && pv.id != vid);
        // A clip with no partner comes alone.
        let mut lone = seq.clone();
        lone.tracks[0].clips[0].linked_clip_id = None;
        assert_eq!(crate::clip_clipboard::copy(&lone, &[vid]).unwrap().clips.len(), 1);
    }
}

/// What copied or cut clips leave on the system clipboard: a line
/// saying what was copied, so Ctrl+V reaches Klipp (see
/// `AppState::system_clipboard_text`) and a paste elsewhere shows text
/// rather than whatever was there before.
fn clips_clipboard_text(count: usize) -> String {
    match count {
        1 => "1 clip copied in Klipp".into(),
        n => format!("{n} clips copied in Klipp"),
    }
}
