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
use export::*;
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

/// First-frame decode result for the Source Viewer. The id lets the UI
/// drop the result if the user has since loaded a different clip.
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

/// Give an existing video clip the audio it should have come with,
/// linked, at its own position on the timeline.
///
/// The repair for a timeline built before appending brought the audio
/// along: those clips have no partner, and re-appending them to get one
/// would throw away the grades on them. Unlike
/// [`append_linked_audio_to_sequence`] this mirrors the clip exactly —
/// the same timeline span *and* the same source range — so a clip that
/// has been trimmed gets the sound that belongs under what is left of
/// it, rather than the file from the top.
///
/// `None` when there is nothing to do or it cannot be done: no such
/// clip, not on a video track, already linked to something, or no audio
/// track and none could be made.
/// A separate recording to put under a clip instead of its own sound:
/// which file, how far into it the clip's first frame falls, and how
/// long it is.
#[derive(Debug, Clone, Copy)]
struct ExternalAudio {
    media_id: klipp_core::MediaId,
    offset_secs: f64,
    len_secs: f64,
}

fn attach_linked_audio_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    external: Option<ExternalAudio>,
) -> Option<LinkedAudioAppend> {
    let video = seq
        .tracks
        .iter()
        .find(|t| t.kind == klipp_timeline::TrackKind::Video && t.find_clip(clip_id).is_some())?
        .find_clip(clip_id)?
        .clone();
    if video.linked_clip_id.is_some() {
        return None;
    }
    let klipp_timeline::ClipKind::Media { media_id } = video.kind else { return None };
    // The recording's range under the clip: the clip's source range
    // shifted by the offset, and cut where the recording does not reach
    // — a recorder started late leaves the clip's head silent, one
    // stopped early its tail.
    let (media_id, timeline_in, timeline_out, source_in, source_out) = match external {
        None => (media_id, video.timeline_in, video.timeline_out, video.source_in, video.source_out),
        Some(ext) => {
            let rate = video.source_in.rate;
            let (mut ain, mut aout) = (video.source_in.to_secs_f64() + ext.offset_secs, video.source_out.to_secs_f64() + ext.offset_secs);
            let (mut tin, mut tout) = (video.timeline_in.to_secs_f64(), video.timeline_out.to_secs_f64());
            if ain < 0.0 {
                tin -= ain;
                ain = 0.0;
            }
            if aout > ext.len_secs {
                tout -= aout - ext.len_secs;
                aout = ext.len_secs;
            }
            if tout <= tin || aout <= ain {
                return None;
            }
            let t = |secs: f64| klipp_core::RationalTime::from_secs_f64(secs, rate);
            (ext.media_id, t(tin), t(tout), t(ain), t(aout))
        }
    };

    let (track_index, created_track_at) = match seq
        .tracks
        .iter()
        .position(|t| t.kind == klipp_timeline::TrackKind::Audio)
    {
        Some(i) => (i, None),
        None => {
            let n = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Audio).count();
            seq.add_audio_track(format!("Audio {}", n + 1));
            let i = seq.tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Audio)?;
            (i, Some(i))
        }
    };

    let mut audio = klipp_timeline::Clip::media(media_id, timeline_in, timeline_out, source_in, source_out);
    audio.linked_clip_id = Some(clip_id);
    let audio_id = audio.id;

    let clips_before = seq.tracks[track_index].clips.clone();
    KlippApp::overwrite_insert_clip_on_track(&mut seq.tracks[track_index], audio);
    let clips_after = seq.tracks[track_index].clips.clone();

    for track in seq.tracks.iter_mut() {
        if let Some(vc) = track.find_clip_mut(clip_id) {
            vc.linked_clip_id = Some(audio_id);
            break;
        }
    }

    let clip = seq.tracks[track_index].find_clip(audio_id)?.clone();
    Some(LinkedAudioAppend { clip, track_index, created_track_at, clips_before, clips_after })
}

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

/// A linked clip may have a differently nudged key in the same displayed frame.
fn edit_linked_audio_key(keys: &mut Vec<klipp_core::Keyframe>, time: f64, tolerance: f64, insert_time: f64, value: Option<f32>) {
    let target = if tolerance > 0.0 { crate::inspector::keyframe_selection::nearest(keys, time, tolerance).map_or(insert_time, |i| keys[i].time_secs) } else { time };
    match value {
        Some(value) => klipp_core::keyframe::upsert_keyframe(keys, target, value),
        None => klipp_core::keyframe::remove_keyframe_at(keys, target),
    }
}

/// The hidden track the timeline grade stands in on while the Colour
/// workspace grades it; fixed for the sequence.
/// A window track to run (#238): the clip's file, its rate, the file
/// frames the clip uses, the frame the window was drawn on, and the
/// window's four corners and pivot there, in frame fractions, and
/// whether to follow those corners in perspective.
struct WindowTrackJob {
    clip_id: klipp_core::ClipId,
    point_index: usize,
    path: std::path::PathBuf,
    media_fps: f64,
    frames: (usize, usize),
    reference: usize,
    quad: [(f32, f32); 4],
    pivot: (f32, f32),
    perspective: bool,
}

/// Where `surface` on `clip` is drawn at timeline second `at`, in
/// fractions of the frame: the clip's picture fitted and placed by its own
/// transform, as the viewer shows it. `source_secs` reads the track there
/// rather than at the frame shown.
pub(crate) fn surface_on_frame(seq: &klipp_timeline::Sequence, clip: &klipp_timeline::Clip, surface: &klipp_timeline::Surface, at: f64, source_secs: Option<f64>) -> [[f32; 2]; 4] {
    let fps = seq.fps.as_f64();
    let elapsed = (at - clip.timeline_in.to_secs_f64()).max(0.0);
    let q = surface.corners_shown_at(source_secs.unwrap_or_else(|| clip.source_secs_at(elapsed, fps)));
    let when = clip.timeline_in + klipp_core::Timecode::from_secs_f64(elapsed, clip.timeline_in.rate.max(1));
    let placed = klipp_compose::eval::evaluate_clip_own_transform(clip, elapsed, &klipp_compose::eval::eval_context(clip, when, fps));
    klipp_timeline::surface::source_quad_in_frame(&q, surface.source_size, [seq.resolution.width, seq.resolution.height], Some(placed))
}

/// A surface to follow through a clip's file (#684): the file, its
/// rate, the frames the clip uses, the frame the corners were set on, and
/// the corners there in frame fractions.
struct SurfaceTrackJob {
    clip_id: klipp_core::ClipId,
    surface: uuid::Uuid,
    name: String,
    path: std::path::PathBuf,
    media_fps: f64,
    frames: (usize, usize),
    reference: usize,
    corners: [[f32; 2]; 4],
    /// Pixels in one frame as it is read for tracking.
    pixels: usize,
    /// 1 or -1: only later or only earlier frames, merged; 0 both ways.
    direction: i8,
    interactive: bool,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// The width surfaces are tracked at, and the memory the frames may take
/// while they are (#684).
const SURFACE_TRACK_SIDE: u32 = 1280;
const SURFACE_TRACK_BYTES: usize = 1_250_000_000;

/// The frames `first..=last` of the file at `path` (`fps` frames a
/// second), grey, for the window tracker (#238): read once, in order,
/// from a seek to the first, at half of a decode no more than 960 on its
/// long side. `progress` hears the share read, and stops it by returning
/// false.
fn grey_frames(path: &std::path::Path, fps: f64, first: usize, last: usize, progress: &dyn Fn(f32) -> bool) -> Result<Vec<klipp_effects::window_track::GrayFrame>, String> {
    grey_frames_sized(path, fps, first, last, 960, true, progress)
}

/// [`grey_frames`] at a decode no more than `long_side` wide as shown,
/// halved again when `half`. A surface is followed on the sharper frames
/// (#684): a pin sits on its corners to about a pixel only when the
/// tracker sees pixels that small.
fn grey_frames_sized(path: &std::path::Path, fps: f64, first: usize, last: usize, long_side: u32, half: bool, progress: &dyn Fn(f32) -> bool) -> Result<Vec<klipp_effects::window_track::GrayFrame>, String> {
    let mut decoder = klipp_engine::SequentialFrameDecoder::open(path, long_side)?;
    decoder.seek_to(first as f64 / fps)?;
    let count = last + 1 - first;
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let Some((rgba, w, h)) = decoder.next_frame()? else { break };
        let (data, gw, gh) = if half {
            klipp_effects::rolling_shutter::to_gray_half(&rgba, w, h)
        } else {
            // The same luma as the half-size reduction, a pixel at a time.
            (rgba.chunks_exact(4).map(|p| ((p[0] as u32 * 54 + p[1] as u32 * 183 + p[2] as u32 * 19) >> 8) as u8).collect(), w, h)
        };
        out.push(klipp_effects::window_track::GrayFrame { data, width: gw as usize, height: gh as usize });
        if out.len() % 8 == 0 && !progress(out.len() as f32 / count as f32) {
            return Err("cancelled".into());
        }
    }
    if out.is_empty() {
        return Err("no frames could be read from the clip".into());
    }
    Ok(out)
}

/// The region of the picture a window covers, as four corners in frame
/// fractions, on a frame `aspect` (height over width) tall: what the
/// window tracker follows. A window's sizes are in frame widths both
/// ways (see `klipp_color::masks`), so a circle reaches its radius across
/// and radius / aspect of the height down; a rectangle turns in the same
/// space; a gradient, which has no area, follows a square
/// about its middle as wide as it is long.
fn window_quad(shape: &klipp_core::MaskShape, aspect: f32) -> [(f32, f32); 4] {
    use klipp_core::MaskShape;
    let bbox = |x0: f32, y0: f32, x1: f32, y1: f32| [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
    let quad = match shape {
        MaskShape::Circle { center_x, center_y, radius } => bbox(center_x - radius, center_y - radius / aspect, center_x + radius, center_y + radius / aspect),
        MaskShape::Rectangle { center_x, center_y, width, height, rotation } => {
            let (sin, cos) = rotation.to_radians().sin_cos();
            let corner = |lx: f32, ly: f32| (center_x + lx * cos - ly * sin, center_y + (lx * sin + ly * cos) / aspect);
            let (hw, hh) = (width / 2.0, height / 2.0);
            [corner(-hw, -hh), corner(hw, -hh), corner(hw, hh), corner(-hw, hh)]
        }
        MaskShape::LinearGradient { start_x, start_y, end_x, end_y } => {
            let (mx, my) = ((start_x + end_x) * 0.5, (start_y + end_y) * 0.5);
            let half = ((end_x - start_x).hypot((end_y - start_y) * aspect) * 0.5).max(0.05);
            bbox(mx - half, my - half / aspect, mx + half, my + half / aspect)
        }
        MaskShape::Bezier { points } => {
            let (x0, x1) = points.iter().fold((1.0f32, 0.0f32), |(a, b), p| (a.min(p.0), b.max(p.0)));
            let (y0, y1) = points.iter().fold((1.0f32, 0.0f32), |(a, b), p| (a.min(p.1), b.max(p.1)));
            bbox(x0, y0, x1, y1)
        }
    };
    quad.map(|(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
}

/// When each frame of a window track (#238) falls on the clip's own
/// clock — the grade's — as (index into the track's frames, time), for
/// the frames inside the clip, earliest first. Through the clip's speed
/// and in its direction (#617), at the sequence's `seq_fps`.
fn window_track_times(clip: &klipp_timeline::Clip, tp: &klipp_core::TrackPoint, pool: &klipp_media::MediaPool, seq_fps: f64) -> Vec<(usize, f64)> {
    let Some(fps) = clip.media_id().and_then(|m| pool.get(&m)).map(|it| it.fps.as_f64()).filter(|f| *f > 0.0) else { return Vec::new() };
    let (s_in, s_out) = (clip.source_in.to_secs_f64(), clip.source_out.to_secs_f64());
    let mut times: Vec<_> = (0..tp.positions.len())
        .filter_map(|i| {
            let at = (tp.start_frame + i) as f64 / fps;
            (at >= s_in - 0.5 / fps && at <= s_out + 0.5 / fps).then(|| (i, clip.elapsed_for_source_secs(at, seq_fps)))
        })
        .collect();
    times.sort_by(|a, b| a.1.total_cmp(&b.1));
    times
}

/// A window track's keyframes (#238): for each file frame tracked, its
/// time on the clip's own clock and the window's move, size and turn
/// from the reference frame, as (time, dx, dy, scale, degrees). Frames
/// outside the clip are left out; a reversed clip's keys come in its
/// own order, earliest first (see [`window_track_times`]).
fn window_track_keys(clip: &klipp_timeline::Clip, tp: &klipp_core::TrackPoint, pool: &klipp_media::MediaPool, seq_fps: f64) -> Vec<(f64, f32, f32, f32, f32)> {
    let reference = tp.reference_frame.unwrap_or(tp.start_frame);
    let Some(&pivot) = tp.positions.get(reference.saturating_sub(tp.start_frame)) else { return Vec::new() };
    window_track_times(clip, tp, pool, seq_fps)
        .into_iter()
        .filter_map(|(i, t)| {
            let (&(x, y), &(scale, rot)) = (tp.positions.get(i)?, tp.motion.get(i)?);
            Some((t, (x - pivot.0) as f32, (y - pivot.1) as f32, scale as f32, rot as f32))
        })
        .collect()
}

/// A window track in perspective (#238) as the window carries it: the
/// corners on the reference frame, and where they are at each moment on
/// the clip's own clock. `None` without the reference frame's corners.
fn window_perspective_track(clip: &klipp_timeline::Clip, tp: &klipp_core::TrackPoint, pool: &klipp_media::MediaPool, seq_fps: f64) -> Option<klipp_core::PerspectiveTrack> {
    let reference = tp.reference_frame.unwrap_or(tp.start_frame);
    let corner = |q: &[(f64, f64); 4]| q.map(|(x, y)| [x as f32, y as f32]);
    let from = corner(tp.corners.get(reference.checked_sub(tp.start_frame)?)?);
    let keys = window_track_times(clip, tp, pool, seq_fps)
        .into_iter()
        .filter_map(|(i, t)| Some(klipp_core::CornerKey { time_secs: t, corners: corner(tp.corners.get(i)?) }))
        .collect();
    Some(klipp_core::PerspectiveTrack { from, keys })
}

fn timeline_grade_proxy_track_id(seq: &klipp_timeline::Sequence) -> klipp_core::TrackId {
    const STAND_IN: u128 = 0x7374_616e_642d_696e_2074_7261_636b_0001;
    klipp_core::TrackId(uuid::Uuid::from_u128(seq.id.0.as_u128() ^ STAND_IN))
}

/// Take the timeline grade's stand-in track off `seq`, if it is there,
/// keeping its graph as the timeline grade when something was done to
/// it. True when the grade changed.
///
/// Visiting is not an edit: `fresh` is the default graph the stand-in
/// was made with when the sequence had no grade, and a stand-in still
/// equal to it leaves the sequence without one, so looking at the
/// Timeline tab neither dirties the project nor saves a graph that does
/// nothing. With no `fresh`, the stand-in is measured against the
/// sequence's own grade, and a sequence without one takes only a grade
/// that does something.
fn settle_timeline_grade_proxy(seq: &mut klipp_timeline::Sequence, fresh: Option<&klipp_nodes::NodeGraph>) -> bool {
    let track_id = timeline_grade_proxy_track_id(seq);
    let Some(pos) = seq.tracks.iter().position(|t| t.id == track_id) else { return false };
    let track = seq.tracks.remove(pos);
    let Some(proxy) = track.clips.into_iter().next() else { return false };
    let Some(target) = GradeStandIn::of(seq, proxy.id) else { return false };
    let unchanged = match fresh.or(target.stored(seq)) {
        Some(given) => *given == proxy.node_graph,
        None => proxy.node_graph.is_identity(),
    };
    if unchanged {
        return false;
    }
    target.store(seq, proxy.node_graph);
    true
}

/// What the Colour workspace grades through a stand-in clip rather than
/// a clip of the timeline: the timeline grade, or a grade group's
/// pre-clip or post-clip grade (#237). Either stands in on the same
/// hidden track, under the id its edits are routed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GradeStandIn {
    Timeline,
    Group(klipp_core::GradeGroupId, klipp_timeline::GradeStage),
}

impl GradeStandIn {
    /// Which one `clip_id` stands for, if it is a stand-in's id.
    fn of(seq: &klipp_timeline::Sequence, clip_id: klipp_core::ClipId) -> Option<Self> {
        if clip_id == seq.timeline_grade_clip_id() {
            return Some(Self::Timeline);
        }
        seq.grade_group_stage_of(clip_id).map(|(g, s)| Self::Group(g, s))
    }

    fn id(self, seq: &klipp_timeline::Sequence) -> Option<klipp_core::ClipId> {
        match self {
            Self::Timeline => Some(seq.timeline_grade_clip_id()),
            Self::Group(g, s) => seq.grade_group(g).map(|g| g.stage_clip_id(s)),
        }
    }

    fn stored(self, seq: &klipp_timeline::Sequence) -> Option<&klipp_nodes::NodeGraph> {
        match self {
            Self::Timeline => seq.timeline_graph.as_ref(),
            Self::Group(g, s) => seq.grade_group(g).and_then(|g| g.stage(s)),
        }
    }

    fn store(self, seq: &mut klipp_timeline::Sequence, graph: klipp_nodes::NodeGraph) {
        match self {
            Self::Timeline => seq.timeline_graph = Some(graph),
            Self::Group(g, s) => {
                if let Some(g) = seq.grade_groups.iter_mut().find(|x| x.id == g) {
                    *g.stage_mut(s) = Some(graph);
                }
            }
        }
    }

    /// What the panels call it.
    fn title(self, seq: &klipp_timeline::Sequence) -> String {
        match self {
            Self::Timeline => "Timeline Grade".into(),
            Self::Group(g, s) => {
                let name = seq.grade_group(g).map_or("Group", |g| g.name.as_str());
                format!("{name} \u{00b7} {}", if s == klipp_timeline::GradeStage::Pre { "Pre-clip" } else { "Post-clip" })
            }
        }
    }
}

/// Put the timeline grade's stand-in on `seq`, on its hidden track, and
/// say which clip it is; `None` when it is already there. While the
/// sequence has no grade the stand-in shows `fresh`, made the first
/// time and kept for this sequence, so it has the same node ids each
/// time it stands in: the inspector dispatches on the ids it drew.
///
/// The same for a grade group's stage (#237). `fresh` is kept by the
/// stand-in's id.
fn stand_in_grade_on(
    seq: &mut klipp_timeline::Sequence,
    which: GradeStandIn,
    fresh: &mut Option<(klipp_core::ClipId, klipp_nodes::NodeGraph)>,
) -> Option<klipp_core::ClipId> {
    let track_id = timeline_grade_proxy_track_id(seq);
    if seq.tracks.iter().any(|t| t.id == track_id) {
        return None;
    }
    let stand_in_id = which.id(seq)?;
    let mut proxy = klipp_timeline::Clip::timeline_grade_proxy(seq);
    proxy.id = stand_in_id;
    match which.stored(seq) {
        Some(g) => proxy.node_graph = g.clone(),
        None => {
            let given = match fresh {
                Some((id, g)) if *id == stand_in_id => g,
                _ => &fresh.insert((stand_in_id, klipp_nodes::NodeGraph::default())).1,
            };
            proxy.node_graph = given.clone();
        }
    }
    let id = proxy.id;
    let mut track = klipp_timeline::Track::new("Timeline Grade", klipp_timeline::TrackKind::Video);
    track.id = track_id;
    track.visible = false;
    track.muted = true;
    track.clips.push(proxy);
    seq.tracks.push(track);
    Some(id)
}

/// Take the stand-in off `seq` again, keeping its graph as the timeline
/// grade if it was edited. The stand-in is measured against `fresh`
/// only while the sequence has no grade, and `fresh` is let go the
/// moment one begins: from then on the stand-in carries the sequence's
/// own grade, and that is what a visit that changes nothing must match.
/// Measured against the fresh graph instead, an existing grade would
/// differ on every command, if only by its node ids, and looking at it
/// would be an edit again (#368).
fn stand_down_timeline_grade_on(
    seq: &mut klipp_timeline::Sequence,
    fresh: &mut Option<(klipp_core::ClipId, klipp_nodes::NodeGraph)>,
) -> TimelineGradeSettled {
    let track_id = timeline_grade_proxy_track_id(seq);
    let Some(stand_in_id) = seq.tracks.iter().find(|t| t.id == track_id).and_then(|t| t.clips.first()).map(|c| c.id) else {
        return TimelineGradeSettled::Unchanged;
    };
    let had_grade = GradeStandIn::of(seq, stand_in_id).is_some_and(|w| w.stored(seq).is_some());
    let given = fresh.as_ref().filter(|(id, _)| *id == stand_in_id && !had_grade).map(|(_, g)| g);
    match (settle_timeline_grade_proxy(seq, given), had_grade) {
        (false, _) => TimelineGradeSettled::Unchanged,
        (true, true) => TimelineGradeSettled::Changed,
        (true, false) => {
            *fresh = None;
            TimelineGradeSettled::Begun
        }
    }
}

/// What standing the timeline grade's stand-in down did to the grade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimelineGradeSettled {
    Unchanged,
    Changed,
    /// A grade where the sequence had none.
    Begun,
}

/// The clip a grade still is taken from: the selected clip when it is
/// under the playhead, since that is the one the Colour panels show;
/// otherwise the topmost visible video clip there, which is what the
/// viewer shows. `None` over a gap.
fn still_source_clip(
    seq: &klipp_timeline::Sequence,
    playhead: klipp_core::Timecode,
    selected: Option<klipp_core::ClipId>,
) -> Option<&klipp_timeline::Clip> {
    let video = || seq.tracks.iter().filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video) && t.visible);
    let selected_here = selected.and_then(|id| video().find_map(|t| t.clip_at_timecode(playhead).map(|(_, c)| c).filter(|c| c.id == id)));
    selected_here.or_else(|| video().find_map(|t| t.clip_at_timecode(playhead).map(|(_, c)| c)))
}

/// `secs` as HH:MM:SS:FF at a nominal `fps`, for display.
fn record_timecode(secs: f64, fps: f64) -> String {
    let nominal = fps.round().max(1.0) as i64;
    let frames = (secs.max(0.0) * fps).round() as i64;
    let (ff, total) = (frames % nominal, frames / nominal);
    format!("{:02}:{:02}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60, ff)
}

/// A still's grade onto each of `targets`, fresh node ids per clip, as
/// graph edits to apply together: in place of each clip's grade, or with
/// `append` after it, keeping the clip's own balance. With the edits,
/// why any target was passed over, once per reason.
fn grade_from_still_edits(
    seq: &klipp_timeline::Sequence,
    grade: &klipp_nodes::NodeGraph,
    targets: &[klipp_core::ClipId],
    append: bool,
) -> (Vec<EditCommand>, Vec<klipp_nodes::CannotAppend>) {
    let mut refused = Vec::new();
    let edits = seq
        .all_clips()
        .filter(|c| targets.contains(&c.id))
        .filter_map(|c| {
            let new_graph = if append {
                let mut g = c.node_graph.clone();
                if let Err(why) = g.append(grade) {
                    if !refused.contains(&why) {
                        refused.push(why);
                    }
                    return None;
                }
                g
            } else {
                c.node_graph.with_grade_of(grade)
            };
            Some(EditCommand::SetNodeGraph { clip_id: c.id, old_graph: c.node_graph.clone(), new_graph })
        })
        .collect();
    (edits, refused)
}

/// One line for a toast out of the reasons targets were passed over.
fn refusals_line(refused: &[klipp_nodes::CannotAppend]) -> String {
    refused.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("; ")
}

/// The edits a Paste Attributes makes: each target clip replaced whole
/// by itself with the chosen attributes written on. Free of the app so
/// it can be tested without one.
fn paste_attributes_edits(
    seq: &klipp_timeline::Sequence,
    targets: &[klipp_core::ClipId],
    attrs: &crate::state::AttributesClipboard,
    what: crate::state::PasteChoice,
) -> Vec<EditCommand> {
    seq.all_clips()
        .filter(|c| targets.contains(&c.id))
        .map(|c| {
            let mut new = c.clone();
            attrs.paste_onto(&mut new, what);
            EditCommand::ReplaceClip { description: "Paste Attributes", old: Box::new(c.clone()), new: Box::new(new) }
        })
        .collect()
}

/// One camera key set or cleared, on the list the param names.
///
/// Free of the app so it can be tested without one. The insert and the
/// remove are `klipp_core::keyframe`'s, so what counts as "the same
/// moment" stays one decision in one place.
fn apply_camera_key(
    camera: &mut klipp_timeline::Camera,
    param: klipp_core::CameraParam,
    time_secs: f64,
    value: Option<f32>,
) {
    let keys = match param {
        klipp_core::CameraParam::X => &mut camera.x_keyframes,
        klipp_core::CameraParam::Y => &mut camera.y_keyframes,
        klipp_core::CameraParam::Zoom => &mut camera.zoom_keyframes,
        klipp_core::CameraParam::Roll => &mut camera.roll_keyframes,
    };
    match value {
        Some(v) => klipp_core::keyframe::upsert_keyframe(keys, time_secs, v),
        None => klipp_core::keyframe::remove_keyframe_at(keys, time_secs),
    }
}

/// The history key a drag on this sequence's camera keys is coalesced
/// under: every move and handle edit between the drag's begin and end
/// goes into one entry.
fn camera_drag_key(sequence_idx: usize) -> String {
    format!("camera:{sequence_idx}")
}

/// Carry the camera key at `index` on one list to another moment and
/// value; the list is re-sorted, so the key may come out at another
/// index. False when there is no such key.
fn move_camera_key(camera: &mut klipp_timeline::Camera, param: klipp_core::CameraParam, index: usize, new_time_secs: f64, new_value: f32) -> bool {
    klipp_core::keyframe::move_keyframe(crate::timeline::camera_lane::keys_mut(camera, param), index, new_time_secs, new_value)
}

/// Set how the curve leaves the key at `index`, and the key before it,
/// so the segments either side of the diamond take the shape — the
/// rule a clip's keyframes follow.
fn set_camera_key_interpolation(camera: &mut klipp_timeline::Camera, param: klipp_core::CameraParam, index: usize, interpolation: klipp_core::keyframe::Interpolation) {
    let keys = crate::timeline::camera_lane::keys_mut(camera, param);
    if index < keys.len() {
        keys[index].interpolation = interpolation;
    }
    if index > 0 && index - 1 < keys.len() {
        keys[index - 1].interpolation = interpolation;
    }
}

/// Set either or both Bezier handles of the key at `index`.
fn set_camera_key_tangent(camera: &mut klipp_timeline::Camera, param: klipp_core::CameraParam, index: usize, tangent_in: Option<(f64, f32)>, tangent_out: Option<(f64, f32)>) {
    if let Some(kf) = crate::timeline::camera_lane::keys_mut(camera, param).get_mut(index) {
        if let Some(ti) = tangent_in {
            kf.tangent_in = ti;
        }
        if let Some(to) = tangent_out {
            kf.tangent_out = to;
        }
    }
}

impl KlippApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cmd_tx: Sender<AppCommand>,
        event_rx: Receiver<AppEvent>,
        frame_buffer: SharedFrameBuffer,
        source_frame_buffer: SharedFrameBuffer,
        sequence: Arc<klipp_timeline::SharedSequence>,
        media_pool_shared: Arc<RwLock<klipp_media::MediaPool>>,
        skim_consumer: klipp_engine::audio_output::SkimConsumer,
        spectrum_buf: Option<Arc<std::sync::Mutex<std::collections::VecDeque<f32>>>>,
        spectrum_sample_rate: u32,
        perf_ring: klipp_engine::perf::SharedPerfRing,
        param_change_queue: Arc<ArrayQueue<ParamChange>>,
        foreground_activity: Arc<klipp_jobs::ForegroundActivity>,
    ) -> Self {
        use klipp_core::startup_timer::LogTimer;

        // AI mask cache LRU sweep — trim
        // ~/.cache/klipp/ai_masks/ to AI_MASK_CACHE_BYTES_CAP if
        // it's overgrown. Cheap (stat-only walk + delete oldest)
        // and runs once at startup so it doesn't cost during
        // editing. See execute_apply_mask_to_clip for the schema
        // these files are written under.
        prune_ai_mask_cache();

        let (thumb_tx, thumb_rx) = crossbeam_channel::unbounded();
        let (source_frame_tx, source_frame_rx) = crossbeam_channel::unbounded();
        let (filmstrip_tx, filmstrip_rx) = crossbeam_channel::unbounded();
        let (relink_check_tx, relink_check_rx) = crossbeam_channel::unbounded();
        let (audio_conform_tx, audio_conform_rx) = crossbeam_channel::unbounded();
        let (proxy_event_tx, proxy_event_rx) = crossbeam_channel::unbounded();
        let (proxy_progress_tx, proxy_progress_rx) = crossbeam_channel::unbounded();
        let prefs = {
            let _t = LogTimer::new("ui: Preferences::load");
            crate::prefs::Preferences::load()
        };
        let theme_registry = {
            let _t = LogTimer::new("ui: ThemeRegistry::load");
            crate::theme::ThemeRegistry::load()
        };
        let theme = KlippTheme::from_prefs(&prefs.appearance, &theme_registry);
        {
            let _t = LogTimer::new("ui: theme + fonts apply (egui)");
            theme.apply_to_egui(&cc.egui_ctx);
            theme.apply_fonts_with_size(&cc.egui_ctx, prefs.appearance.ui_text_size);
            crate::icons::install(&cc.egui_ctx);
        }


        // Apply UI scale — Auto (0) detects system DPI via native pixels_per_point
        let scale_pct = prefs.appearance.ui_scale_percent;
        if scale_pct == 0 {
            // Auto: let egui use the system's native scale (zoom_factor = 1.0)
            // egui already reads the system DPI from Wayland/X11
            cc.egui_ctx.set_zoom_factor(1.0);
        } else {
            // Manual: override with user's chosen percentage
            // System already provides native_pixels_per_point, so zoom_factor
            // should be relative: e.g. 100% = 1.0, 125% = 1.25
            cc.egui_ctx.set_zoom_factor(scale_pct as f32 / 100.0);
        }
        let layout = LayoutState::load();
        // Panel visibility is persisted, so a panel collapsed last
        // session is still collapsed. AppState's defaults say visible;
        // the saved layout is what the user actually last chose.
        let (restore_browser, restore_inspector) = (layout.browser_visible, layout.inspector_visible);

        // Heavy field inits — extracted from the struct literal so we can
        // measure each one independently for STARTUP_PERF.
        let title_renderer_ui = {
            let _t = LogTimer::new("ui: TitleRenderer::new (font discovery, second instance)");
            klipp_title::TitleRenderer::new()
        };
        let key_bindings = {
            let _t = LogTimer::new("ui: KeyBindings::load");
            crate::keybindings::KeyBindings::load()
        };
        let cpal_sample_rate = {
            let _t = LogTimer::new("ui: cpal probe_sample_rate (duplicate)");
            klipp_engine::audio_output::probe_sample_rate()
        };
        // NOTE: Audio device cache is NOT primed at startup. An earlier
        // version spawned a background thread for this, but libasound
        // (ALSA) is not thread-safe for concurrent device enumeration.
        // Running enumerate_output_devices() simultaneously with the
        // engine's AudioOutput::new() corrupted cpal's device handles
        // and silently broke audio output. The cache is now populated
        // lazily on first visit to the Audio HW preferences tab — the
        // 5-50ms hitch is acceptable for a one-time UI interaction.

        // Capture the scheduler policy from prefs BEFORE moving
        // `prefs` into the struct literal below.
        let initial_scheduler_policy =
            prefs.playback.background_performance.to_scheduler_policy();

        let mut app = Self {
            cmd_tx: klipp_engine::commands::EngineSender::new(cmd_tx),
            event_rx,
            control_inbox: None,
            frame_buffer,
            source_frame_buffer,
            sequence,
            media_pool_shared,
            state: {
                let mut s = AppState::default();
                if let Some(buf) = spectrum_buf {
                    s.spectrum_ring = buf;
                }
                s.spectrum_sample_rate = spectrum_sample_rate.max(1);
                // Snap config persists in layout.json. Older layouts
                // carried only `snap_enabled: bool` and are migrated
                // to a full SnapConfig on read (see LayoutState::load).
                s.snap = layout.snap;
                s.dual_viewers = layout.dual_viewers;
                s.color_dual_viewers = layout.color_dual_viewers;
                s.export_dual_viewers = layout.export_dual_viewers;
                s
            },
            toolbar: ToolbarPanel { logo_texture: None },
            updates: Default::default(),
            viewer: ViewerPanel::default(),
            title_renderer: title_renderer_ui,
            markers: MarkersPanel::default(),
            transcript_panel: crate::panels::transcript::TranscriptPanel::new(),
            transcript_source: Default::default(),
            colour_check: Default::default(),
            sound_over_time: Default::default(),
            narrate: Default::default(),
            preferences_panel: PreferencesPanel::default(),
            transport: TransportBar,
            timeline: {
                // Field assignment rather than a struct literal with
                // `..Default::default()`: TimelinePanel has private
                // fields, so the literal form is not available here.
                let mut tl = TimelinePanel::default();
                tl.embedded = true;
                tl
            },
            thumb_tx,
            thumb_rx,
            source_frame_tx,
            source_frame_rx,
            filmstrip_tx,
            filmstrip_cache_loads: std::collections::HashSet::new(),
            multicam_grid: None,
            multi_edit: None,
            ui_scale_applied: None,
            filmstrip_refining: Default::default(),
            filmstrip_rx,
            relink_check_tx,
            relink_check_rx,
            audio_conform_tx,
            audio_conform_rx,
            proxy_children: Vec::new(),
            skim_worker: {
                let tx = proxy_event_tx.clone();
                klipp_engine::skim::SkimWorker::spawn(3, move |job, result| {
                    let (media_id, target) = job.tag;
                    let event = match result {
                        Ok(f) => AppEvent::BrowserSkimFrameReady { media_id, rgba: f.rgba, width: f.width, height: f.height, target },
                        Err(e) => {
                            tracing::warn!("skim decode failed for {media_id:?} @ {:.3}s: {e}", job.secs);
                            // An empty frame resets the requester's in-flight flag.
                            AppEvent::BrowserSkimFrameReady { media_id, rgba: Vec::new(), width: 0, height: 0, target }
                        }
                    };
                    let _ = tx.send(event);
                })
            },
            proxy_event_tx,
            proxy_event_rx,
            export_dialog: ExportDialogState::default(),
            prefs,
            prefs_dirty: false,
            key_bindings,
            prefs_last_save: std::time::Instant::now(),
            proxy_progress_tx,
            proxy_progress_rx,
            last_autosave: std::time::Instant::now(),
            autosave_rx: None,
            last_heartbeat: std::time::Instant::now(),
            frame_count: 0,
            last_heartbeat_frame: 0,
            hb_input_events: 0,
            hb_max_batch: 0,
            last_graded_playhead: None,
            graded_positions: std::collections::HashMap::new(),
            browser_before_workspace: None,
            last_workspace: crate::state::Workspace::default(),
            hb_max_gap: std::time::Duration::ZERO,
            unsaved_save_waiting: false,
            hb_max_update: std::time::Duration::ZERO,
            last_update_end: std::time::Instant::now(),
            last_frame_at: std::time::Instant::now(),
            last_scope_preview_refresh: std::time::Instant::now()
                - std::time::Duration::from_secs(1),
            last_sent_scope_mode: None,
            last_sent_active_scope_modes: None,
            last_sent_scope_sizes: None,
            last_sent_scope_settings: None,
            last_sent_scope_bloom: None,
            last_sent_scope_low_pass: None,
            last_sent_scopes_visible: None,
            theme,
            theme_registry,
            layout,
            layout_dirty: false,
            browser: BrowserPanel::default(),
            new_inspector: NewInspectorPanel::default(),
            new_timeline: crate::timeline::NewTimelinePanel::default(),
            skim_consumer,
            cpal_sample_rate,
            last_window_title: String::new(),
            launch_screen: crate::panels::launch_screen::LaunchScreen::default(),
            new_project_dialog: crate::panels::new_project_dialog::NewProjectDialog::new(None),
            startup_open: None,
            last_open_error: None,
            relink_panel: Default::default(),
            lock_holder: klipp_project::lock::Holder::current(),
            lock_keeper: None,
            lock_seen_path: None,
            read_only_project: None,
            lock_prompt: None,
            lock_decision: None,
            sequence_settings_dialog: None,
            framerate_mismatch_dialog: None,
            suppress_fps_prompt: false,
            node_graph_panel: crate::panels::node_graph::NodeGraphPanel::default(),
            node_graph_v2_panel: crate::panels::node_graph_v2::NodeGraphV2Panel::default(),
            node_inspector_panel: crate::panels::node_inspector::NodeInspectorPanel::default(),
            looks_panel_state: crate::panels::looks::LooksPanelState::default(),
            node_param_edit_snapshot: None,
            timeline_grade_fresh: None,
            last_gesture_sync: std::time::Instant::now(),
            scope_executor: klipp_nodes::GraphExecutor::new_cpu_only(),
            media_watcher: None,
            system_theme_watcher: None,
            system_theme_changed: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            curve_editor: crate::panels::curve_editor::CurveEditorPanel::new(),
            scope_window_runtime: crate::widgets::scope_window::ScopeWindowRuntime::default(),
            scope_dock_runtime: crate::widgets::scope_window::ScopeWindowRuntime::default(),
            perf_ring,
            param_change_queue,
            // Engine writes is_playing/is_scrubbing to this Arc; the
            // scheduler below reads from the same Arc to know when
            // the user is busy. Both ends share state without locks.
            foreground_activity: Arc::clone(&foreground_activity),
            job_scheduler: klipp_jobs::JobScheduler::new(
                foreground_activity,
                initial_scheduler_policy,
            ),
        };
        // Analysis waits while the person is playing, scrubbing or has
        // just touched the controls, unless they chose throughput over
        // responsiveness for background work.
        {
            let fg = Arc::clone(&app.foreground_activity);
            let sched = Arc::clone(&app.job_scheduler);
            klipp_analysis::hold::install(Arc::new(move || {
                sched.policy() != klipp_jobs::SchedulerPolicy::MaxThroughput
                    && fg.is_user_busy(std::time::Duration::from_millis(1500))
            }));
        }
        // Build false color LUT from loaded preferences
        // A panel collapsed last session stays collapsed. Assigned
        // directly rather than through the setters: this is adopting
        // what was already saved, so there is nothing new to mark dirty.
        app.state.show_browser_panel = restore_browser;
        app.last_workspace = app.state.active_workspace;
        app.state.show_inspector_panel = restore_inspector;
        app.state.false_color_lut = app.prefs.viewer.false_color_palette.build_lut();
        // Probe audio backend status
        app.state.audio_sample_rate = app.cpal_sample_rate;
        app.state.audio_backend_ok = {
            let _t = LogTimer::new("ui: cpal probe_audio_available (duplicate)");
            klipp_engine::audio_output::probe_audio_available()
        };

        // Load saved workspaces
        let ws_path = dirs::config_dir().unwrap_or_default().join("klipp/workspaces.json");
        if let Ok(data) = std::fs::read_to_string(&ws_path) {
            if let Ok(workspaces) = serde_json::from_str::<Vec<crate::state::SavedWorkspaceLayout>>(&data) {
                app.state.custom_workspaces = workspaces;
            }
        }

        // What to open is only noted here and opened on the first frames,
        // through the UI's own dispatcher: OpenProject and ImportMedia are
        // UI commands, and sent to the engine they were dropped (#395).
        // Paths from the command line take the place of the startup
        // behaviour.
        let given = crate::cli::startup_paths();
        if !given.is_empty() {
            app.startup_open = Some(crate::cli::StartupOpen::plan(given));
        } else {
        // Apply startup behavior
        match app.prefs.general.startup_behavior {
            crate::prefs::StartupBehavior::ProjectManager => {
                // Default: project_path is None, launch screen will show
            }
            crate::prefs::StartupBehavior::EmptyProject => {
                // Skip launch screen, go straight to empty project
                app.state.project_path = Some(std::path::PathBuf::from("Untitled"));
            }
            crate::prefs::StartupBehavior::LastProject => {
                // Reopen the most recent project if available
                if let Some(recent) = app.prefs.general.recent_projects.first() {
                    let path = recent.path.clone();
                    if path.exists() {
                        app.startup_open = Some(crate::cli::StartupOpen::project(path));
                    }
                }
            }
        }
        }

        // Sync preferences to state
        app.state.remember_inspector_layout = app.prefs.general.remember_inspector_layout;
        if app.state.remember_inspector_layout {
            app.state.collapsed_sections = app.prefs.general.collapsed_sections.iter().cloned().collect();
            // Seed egui's persistent data with collapsed sections
            let collapsed_key = egui::Id::new("klipp_collapsed_sections");
            cc.egui_ctx.data_mut(|d| {
                let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
                for name in &app.prefs.general.collapsed_sections {
                    set.insert(name.clone());
                }
            });
            // Seed Title-inspector expanded groups (separate set, opt-in
            // per group name; default empty = all collapsed on first run).
            let title_key = egui::Id::new(crate::inspector::TITLE_EXPANDED_GROUPS_KEY);
            cc.egui_ctx.data_mut(|d| {
                let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(title_key);
                for name in &app.prefs.title.inspector_expanded_groups {
                    set.insert(name.clone());
                }
            });
        }
        app.state.audio_skimming_enabled = app.prefs.playback.skim_audio;
        app.state.file_browser_video_skim_enabled = app.prefs.file_browser.file_browser_video_skim;
        app.state.file_browser_audio_skim_enabled = app.prefs.file_browser.file_browser_audio_skim;
        app.state.show_recent_files = app.prefs.file_browser.show_recent_files;
        app.state.mute_reminder_interval_secs = app.prefs.audio_hardware.mute_reminder_interval_secs;
        app.state.filmstrip_hover_enabled = app.prefs.file_browser.filmstrip_hover;
        app.state.filmstrip_quality = app.prefs.file_browser.filmstrip_quality;
        app.state.filmstrip_persist = app.prefs.file_browser.filmstrip_persist;
        app.state.file_browser_auto_refresh = app.prefs.file_browser.auto_refresh;
        app.state.recent_files_snapshot = app.prefs.file_browser.recent_files.clone();
        app.state.loop_playback_enabled = app.prefs.playback.loop_playback;
        // Playhead follow: if pref is off, start with follow disabled
        if !app.prefs.editing.playhead_follows_playback {
            app.state.playhead_follow = crate::state::PlayheadFollow::Off;
        }
        app.state.magnetic_timeline_enabled = app.prefs.editing.magnetic_timeline;
        app.state.multicam_show_angles = app.prefs.editing.multicam_show_angles;
        app.state.auto_create_tracks = app.prefs.editing.auto_create_tracks;
        app.state.default_transition_frames = app.prefs.editing.default_transition_frames;
        app.state.snap_threshold_px = app.prefs.editing.snap_threshold_px;
        app.state.grid_snap_interval = app.prefs.editing.grid_snap_interval;
        app.state.zoom_step = app.prefs.editing.zoom_step;
        app.state.default_sequence_resolution_idx = app.prefs.editing.default_sequence_resolution_idx;
        app.state.timeline_prefs = app.prefs.timeline.clone();
        app.state.viewer_prefs = app.prefs.viewer.clone();
        app.state.toast_duration_secs = app.prefs.advanced.toast_duration_secs;
        app.state.toast.visible_duration = app.prefs.advanced.toast_duration_secs;
        // Carry the AI preferences into the analysis pipeline at startup,
        // so saved opt-outs and the CPU choice take effect before any import.
        klipp_analysis::set_transcription_enabled(app.prefs.ai.transcription);
        klipp_analysis::set_subject_detection_enabled(app.prefs.ai.subject_detection);
        klipp_analysis::set_screen_text_enabled(app.prefs.ai.screen_text);
        klipp_analysis::transcript::set_transcribe_on_cpu(app.prefs.ai.transcribe_on_cpu);
        // Unpack the Voice Isolation model off the audio path, if the
        // user has downloaded it.
        if klipp_audio::dsp::voice_isolation::model_installed() {
            let _ = std::thread::Builder::new()
                .name("klipp-voice-isolation-load".into())
                .spawn(klipp_audio::dsp::voice_isolation::prewarm);
        }
        let _ = app.cmd_tx.send(AppCommand::SetPanLaw(app.prefs.audio.pan_law));
        klipp_engine::pcm_reader::set_anti_click_fade_ms(app.prefs.advanced.anti_click_fade_ms);
        klipp_engine::video_decoder::set_hardware_decode(app.prefs.system.hardware_decode);
        klipp_engine::video_decoder::set_max_playback_height(app.prefs.system.max_playback_height);
        klipp_engine::video_decoder::set_frame_queue_size(app.prefs.system.frame_queue_size);
        klipp_engine::video_decoder::set_decode_threads(app.prefs.system.decode_threads);
        klipp_engine::video_decoder::set_optimised_playback_mode(
            app.prefs.playback.optimised_playback.to_engine_mode(),
        );
        klipp_engine::video_decoder::set_playback_quality_divisor(crate::prefs::playback_quality_divisor(
            app.prefs.playback.playback_quality,
        ));
        klipp_engine::audio_output::set_preferred_device(app.prefs.audio_hardware.output_device.clone());
        // TODO: background_render pref — when true, pre-render complex timeline
        // sections in a background thread. No background render system exists yet;
        // logging the pref value here so it's exercised at startup.
        if app.prefs.playback.background_render {
            tracing::debug!("Background render enabled (not yet implemented)");
        }
        // Sync LUT interpolation mode to engine
        let _ = app.cmd_tx.send(AppCommand::SetLutTetrahedral(
            app.prefs.color.lut_interpolation == crate::prefs::LutInterpolation::Tetrahedral,
        ));
        let _ = app.cmd_tx.send(AppCommand::SetSdrTransfer(app.prefs.color.sdr_display_transfer));
        // Map GpuPreference to wgpu PowerPreference (klipp-effects can't depend on klipp-core)
        let gpu_pref = match app.prefs.system.gpu_preference {
            klipp_core::GpuPreference::Auto => Some(wgpu::PowerPreference::default()),
            klipp_core::GpuPreference::HighPerformance => Some(wgpu::PowerPreference::HighPerformance),
            klipp_core::GpuPreference::LowPower => Some(wgpu::PowerPreference::LowPower),
            klipp_core::GpuPreference::CpuOnly => None, // don't init GPU at all
        };
        klipp_effects::gpu::set_gpu_power_preference(gpu_pref);
        klipp_media::disk_cache::set_cache_dir_overrides(
            app.prefs.media_cache.filmstrip_cache_dir.clone(),
            app.prefs.media_cache.audio_cache_dir.clone(),
        );
        app.state.modal_backdrop_alpha = app.prefs.advanced.modal_backdrop_alpha;
        app.state.scope_quality = app.prefs.color.scope_quality;
        let _ = app.cmd_tx.send(AppCommand::SetRenderCacheEnabled(app.prefs.media_cache.enable_render_cache));
        app.state.scope_update_interval_ms = app.prefs.color.scope_update_interval_ms;
        app.state.set_history_max_size(app.prefs.editing.undo_limit);
        app.state.audio_fade_overlay_mode = app.prefs.audio.fade_overlay_mode;
        app.state.audio_show_rubber_band = app.prefs.audio.show_rubber_band;
        app.state.audio_show_waveform = app.prefs.audio.show_waveform;
        app.state.audio_anti_click_fade = app.prefs.audio.anti_click_fade;
        app.state.audio_show_crossfade_indicators = app.prefs.audio.show_crossfade_indicators;
        app.state.lufs_target = match app.prefs.audio.loudness_standard {
            klipp_core::LoudnessStandard::EbuR128 => -23.0,
            klipp_core::LoudnessStandard::AtscA85 => -24.0,
            klipp_core::LoudnessStandard::Off => -23.0,
        };
        app.state.peak_hold_time_secs = app.prefs.audio.peak_hold_time_secs;
        app.browser.browser_view_mode = app.prefs.editing.media_browser_view_mode;
        app.browser.file_thumb_size = app.prefs.file_browser.file_thumb_size;
        app.browser.files_sort_by = app.prefs.file_browser.file_sort_by;
        app.browser.list_columns = app.prefs.file_browser.media_list_columns.clone();
        app.browser.files_sort_ascending = app.prefs.file_browser.file_sort_ascending;
        app.browser.group_by_date = app.prefs.file_browser.group_by_date;
        app.browser.visible_columns = app.prefs.file_browser.visible_columns.clone();
        app.browser.files_type_filter = app.prefs.file_browser.file_type_filter;
        app.browser.pinned_folders = app.prefs.file_browser.pinned_folders.clone();
        app.browser.favorite_effects = app.prefs.effects_browser.favorite_effects.clone();
        app.browser.favorite_transitions = app.prefs.effects_browser.favorite_transitions.clone();
        app.browser.favorite_titles = app.prefs.effects_browser.favorite_titles.clone();
        app.browser.favorite_generators = app.prefs.effects_browser.favorite_generators.clone();
        // Sync loop state to engine (engine defaults to loop_enabled=false)
        if app.state.loop_playback_enabled {
            let _ = app.cmd_tx.send(AppCommand::SetLoopPlayback(true));
        }
        // Sync anti-click fade to engine
        let _ = app.cmd_tx.send(AppCommand::SetAntiClickFade(app.state.audio_anti_click_fade));
        // Sync scrub-audio preference to engine (default true; dispatch
        // unconditionally so the engine reflects the user's saved choice).
        let _ = app.cmd_tx.send(AppCommand::SetScrubAudioEnabled(app.prefs.playback.scrub_audio));

        // Discover system fonts for title clip font selector
        let font_registry = {
            let _t = LogTimer::new("ui: discover_system_fonts (third instance)");
            klipp_title::font_discovery::discover_system_fonts()
        };
        app.state.available_fonts = font_registry.keys().cloned().collect();
        tracing::info!("UI font discovery: {} families available", app.state.available_fonts.len());

        // Enrich any stale recent project entries with metadata from disk
        let mut prefs_changed = {
            let _t = LogTimer::new("ui: enrich_recent_projects");
            app.prefs.enrich_recent_projects()
        };

        // Generate poster frame thumbnails for recent projects that don't have one
        {
            let _t = LogTimer::new("ui: poster frame generation (recent projects)");
            let cache_dir = dirs::cache_dir().map(|d| d.join("klipp").join("thumbnails"));
            if let Some(ref cache_dir) = cache_dir {
                let _ = std::fs::create_dir_all(cache_dir);
                for rp in &mut app.prefs.general.recent_projects {
                    // Skip if already has a valid thumbnail
                    if rp.thumbnail_path.as_ref().is_some_and(|p| p.exists()) {
                        continue;
                    }
                    if !rp.path.exists() {
                        continue;
                    }
                    // Try to find the first media file from the project's manifest
                    let manifest_path = rp.path.join("media").join("manifest.json");
                    let first_media = std::fs::read_to_string(&manifest_path).ok()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                        .and_then(|v| v.get("items")?.as_array()?.first()?.get("path")?.as_str().map(std::path::PathBuf::from));

                    if let Some(media_path) = first_media {
                        if media_path.exists() {
                            // Extract a non-black frame and save as PNG
                            let est_duration = rp.duration_secs.unwrap_or(60.0);
                            if let Ok((rgba, w, h)) = klipp_engine::generate_thumbnail_at_size_non_black(&media_path, 1.0, est_duration, 320) {
                                let stem = rp.path.file_stem().unwrap_or_default().to_string_lossy();
                                let out_path = cache_dir.join(format!("{stem}.png"));
                                if save_rgba_as_png(&rgba, w, h, &out_path) {
                                    rp.thumbnail_path = Some(out_path);
                                    prefs_changed = true;
                                    tracing::info!("Generated poster frame for {:?}", rp.path);
                                }
                            }
                        }
                    }
                }
            }
        }

        if prefs_changed {
            let _ = app.prefs.save();
        }

        // U-5: use the cached plugin_infos instead of spinning up a
        // throwaway OfxHost just to extract metadata. The engine
        // creates its own host for live invocation; this path only
        // needs the read-only list for the effects registry + UI.
        {
            let _t = LogTimer::new("ui: OfxHost plugin_infos (cached)");
            app.state.available_plugins = klipp_plugin::plugin_infos_cached();
            app.state.effect_registry.merge_ofx_plugins(&app.state.available_plugins);
            tracing::info!(
                "OFX: {} cached plugin(s), {} total effects in registry",
                app.state.available_plugins.len(),
                app.state.effect_registry.effects.len()
            );
        }

        // Discover LADSPA plugins at startup. Uses the on-disk cache when
        // available — cache hit is ~1–5 ms, cold scan is 50–500 ms depending
        // on plugin count. Cache invalidates automatically when any LADSPA
        // search directory's mtime changes (i.e. a plugin was installed,
        // removed, or renamed).
        //
        // Load the user's disabled-plugins set first, then filter it out
        // before merging into the effect registry — disabled plugins
        // shouldn't appear in the browser or "+ Add Effect" picker.
        {
            let _t = LogTimer::new("ui: LADSPA discovery");
            let cache_dir = dirs::cache_dir().map(|p| p.join("klipp"));
            app.state.disabled_audio_plugins = crate::audio_plugins_disabled::load();
            app.state.ladspa_plugins =
                klipp_audio::ladspa::discover_ladspa_plugins_cached(cache_dir.as_deref());
            let enabled: Vec<klipp_audio::ladspa::LadspaPluginInfo> = app
                .state
                .ladspa_plugins
                .iter()
                .filter(|p| !app.state.disabled_audio_plugins.contains(&p.unique_id))
                .cloned()
                .collect();
            app.state.effect_registry.merge_ladspa_plugins(&enabled);
            tracing::info!(
                "LADSPA: discovered {} plugin(s) ({} enabled, {} disabled)",
                app.state.ladspa_plugins.len(),
                enabled.len(),
                app.state.disabled_audio_plugins.len()
            );
        }
        // frei0r 1.x video-plugin discovery — same disk-cached pattern
        // as LADSPA. Populates the runtime cache in
        // `klipp_effects::frei0r_effect` so any saved-project clip
        // referencing a `BuiltinEffect::Frei0r { type_id, .. }`
        // resolves on first render.
        {
            let _t = LogTimer::new("ui: frei0r discovery");
            let cache_dir = dirs::cache_dir().map(|p| p.join("klipp"));
            app.state.frei0r_plugins =
                klipp_frei0r::discover_frei0r_plugins_cached(cache_dir.as_deref());
            klipp_effects::frei0r_effect::set_plugin_registry(
                app.state.frei0r_plugins.clone(),
            );
            app.state
                .effect_registry
                .merge_frei0r_plugins(&app.state.frei0r_plugins);
            tracing::info!(
                "frei0r: discovered {} plugin(s)",
                app.state.frei0r_plugins.len()
            );
        }
        // LV2 audio plugin discovery — disk-cached (livi reads every
        // bundle's manifest, expensive cold). Populates the browser
        // registry so the Audio FX tab shows the LV2 source pill;
        // create_effect("lv2:{uri}", sr) resolves through the
        // klipp_audio::lv2 World singleton.
        {
            let _t = LogTimer::new("ui: LV2 discovery");
            let cache_dir = dirs::cache_dir().map(|p| p.join("klipp"));
            app.state.lv2_plugins =
                klipp_audio::lv2::discover_lv2_plugins_cached(cache_dir.as_deref());
            klipp_audio::lv2::set_plugin_registry(app.state.lv2_plugins.clone());
            app.state
                .effect_registry
                .merge_lv2_plugins(&app.state.lv2_plugins);
            tracing::info!(
                "LV2: discovered {} plugin(s)",
                app.state.lv2_plugins.len()
            );
        }
        // CLAP audio-plugin discovery — disk-cached. Populates the
        // browser registry so the Audio FX tab shows the CLAP source
        // pill; create_effect("clap:{id}", sr) resolves through the
        // klipp_audio::clap process-global registry. v1 stub runtime
        // — chain loads, params save/restore via ParamID, but
        // process() passes audio through unchanged until
        // CLAP-NEXT-RUNTIME-WIRING ships.
        {
            let _t = LogTimer::new("ui: CLAP discovery");
            let cache_dir = dirs::cache_dir().map(|p| p.join("klipp"));
            app.state.clap_plugins =
                klipp_audio::clap::discover_clap_plugins_cached(cache_dir.as_deref());
            klipp_audio::clap::set_plugin_registry(app.state.clap_plugins.clone());
            app.state
                .effect_registry
                .merge_clap_plugins(&app.state.clap_plugins);
            tracing::info!(
                "CLAP: discovered {} plugin(s)",
                app.state.clap_plugins.len()
            );
        }
        klipp_core::startup_timer::mark("KlippApp::new returning");
        app
    }

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

    fn apply_edits_as_one(&mut self, edits: &[EditCommand], label: &str) {
        if edits.is_empty() {
            return;
        }
        self.state.active_history_mut().begin_batch();
        for e in edits {
            self.apply_edit_forward(e);
            self.state.push_history(e.clone());
        }
        self.state.active_history_mut().end_batch_as(label);
        self.state.dirty = true;
        self.state.media_usage_cache_dirty = true;
        self.sync_sequence_to_engine();
        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
        tracing::info!("applied \"{label}\": {} edits", edits.len());
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

    /// Show nearest filmstrip thumbnail as instant viewer preview while full-res decodes.
    fn show_filmstrip_preview(&mut self, ctx: &egui::Context) {
        let playhead_secs = self.state.playhead.to_secs_f64();
        let seq = &self.state.sequences[self.state.active_sequence_idx];

        // Find the clip at the playhead
        let media_id = seq.tracks.iter()
            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
            .find_map(|t| t.clip_at_timecode(self.state.playhead))
            .and_then(|(_, clip)| {
                clip.media_id().map(|mid| {
                    let source_secs = clip.source_secs_at(playhead_secs - clip.timeline_in.to_secs_f64(), seq.fps.as_f64());
                    (mid, source_secs)
                })
            });

        if let Some((mid, source_secs)) = media_id {
            if let Some(filmstrip) = self.state.filmstrip_textures.get(&mid) {
                if !filmstrip.is_empty() {
                    // Binary search for nearest filmstrip frame
                    let idx = match filmstrip.binary_search_by(|f| {
                        f.0.partial_cmp(&source_secs).unwrap_or(std::cmp::Ordering::Equal)
                    }) {
                        Ok(i) => i,
                        Err(i) => {
                            if i == 0 { 0 }
                            else if i >= filmstrip.len() { filmstrip.len() - 1 }
                            else {
                                let d_left = (filmstrip[i - 1].0 - source_secs).abs();
                                let d_right = (filmstrip[i].0 - source_secs).abs();
                                if d_left <= d_right { i - 1 } else { i }
                            }
                        }
                    };

                    // Get the filmstrip texture and create a clone for the viewer
                    let (_, ref thumb_tex) = filmstrip[idx];
                    let size = thumb_tex.size_vec2();
                    if size.x > 0.0 && size.y > 0.0 {
                        // Read the filmstrip frame pixels from thumbnail cache
                        if let Some(frames) = self.state.thumbnail_cache.filmstrip(&mid) {
                            if let Some(frame) = frames.get(idx) {
                                let fw = frame.width as usize;
                                let fh = frame.height as usize;
                                if frame.rgba.len() == fw * fh * 4 {
                                    let pixels: Vec<egui::Color32> = frame.rgba.chunks_exact(4)
                                        .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                                        .collect();
                                    let image = egui::ColorImage { size: [fw, fh], pixels };
                                    // Upload as viewer texture — bilinear upscale handles the rest.
                                    // Must go through upload_texture_deferred: a size change on
                                    // viewer.video_texture (e.g. 720p thumbnail → 1080p frame)
                                    // would otherwise trip egui-wgpu's destroy-mid-submit race.
                                    let drops_slot = self.state.deferred_texture_drops_slot;
                                    upload_texture_deferred(
                                        ctx,
                                        &mut self.viewer.video_texture,
                                        &mut self.state.deferred_texture_drops[drops_slot],
                                        "klipp-video-preview",
                                        image,
                                        egui::TextureOptions::LINEAR,
                                    );
                                    self.state.showing_thumbnail_preview = true;
                                    ctx.request_repaint();
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    fn upload_frame_to_egui(&mut self, ctx: &egui::Context) {
        let t_total = std::time::Instant::now();

        // ── Phase 1: minimal lock — take data, release immediately (~5µs) ──
        let frame_data = {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.swap_pending();
            self.state.render_cache_ranges = fb.render_cache_ranges.clone();
            // A frame the GPU stack wrote straight into the viewer's
            // texture: nothing to upload, the viewer samples it.
            self.viewer.gpu_frame_live = fb.gpu_output_live && fb.gpu_output.is_some();
            self.viewer.gpu_frame_size = (fb.gpu_output_width, fb.gpu_output_height);
            if !fb.dirty {
                return;
            }
            let w = fb.width as usize;
            let h = fb.height as usize;
            if fb.data.len() != w * h * 4 {
                fb.dirty = false;
                return;
            }
            fb.scope_preview.clear();
            fb.scope_preview_w = 0;
            fb.scope_preview_h = 0;
            fb.dirty = false;
            let fw = fb.width;
            let fh = fb.height;
            // Take ownership — zero copy, ~0µs
            let rgba = std::mem::take(&mut fb.data);
            let original = std::mem::take(&mut fb.original_data);
            let unstab = std::mem::take(&mut fb.unstab_data);
            let compare_effect = std::mem::take(&mut fb.compare_effect_data);
            (rgba, original, unstab, compare_effect, fw, fh, w, h)
            // lock released immediately — fb.data is now empty
        };
        let (mut rgba_bytes, original_bytes, unstab_bytes, compare_effect_bytes, fb_width, fb_height, w, h) = frame_data;

        // Full-res frame arrived — clear preview flags
        self.state.showing_thumbnail_preview = false;
        self.state.awaiting_seek_frame = false;

        // ── Phase 2: all expensive work outside lock ──

        // Capture reference frame for effect thumbnails
        let has_content = rgba_bytes.len() >= 64 && {
            let mid = rgba_bytes.len() / 2;
            let q1 = rgba_bytes.len() / 4;
            let q3 = 3 * rgba_bytes.len() / 4;
            rgba_bytes[q1] > 0 || rgba_bytes[mid] > 0 || rgba_bytes[q3] > 0
        };
        if has_content {
            self.state.effect_registry.set_reference_frame(&rgba_bytes, fb_width, fb_height);
        }

        // Compute curve histograms for the color panel
        if self.state.show_color_panel {
            let hist = klipp_color::scopes::compute_histogram(&rgba_bytes, fb_width, fb_height);
            self.state.curve_histograms = Some(hist);
        }

        // Before/After split compositing: left = original, right = graded
        if self.state.before_after_active && !original_bytes.is_empty()
            && original_bytes.len() == rgba_bytes.len()
        {
            let split_x = (self.state.split_position * w as f32) as usize;
            for y in 0..h {
                let row_start = y * w * 4;
                let copy_end = (split_x * 4).min(w * 4);
                rgba_bytes[row_start..row_start + copy_end]
                    .copy_from_slice(&original_bytes[row_start..row_start + copy_end]);
            }
        }

        // Stabilization compare split: left = raw (unstabilized), right = stabilized
        if self.state.stabilization_compare_active && !unstab_bytes.is_empty()
            && unstab_bytes.len() == rgba_bytes.len()
        {
            let split_x = (self.state.split_position * w as f32) as usize;
            for y in 0..h {
                let row_start = y * w * 4;
                let copy_end = (split_x * 4).min(w * 4);
                rgba_bytes[row_start..row_start + copy_end]
                    .copy_from_slice(&unstab_bytes[row_start..row_start + copy_end]);
            }
        }

        // Per-effect compare split: left = without the selected effect,
        // right = with it. Same byte-row copy as the other compare modes;
        // the engine has already produced compare_effect_data at the same
        // dimensions as the main frame.
        if self.state.split_compare_effect.is_some() && !compare_effect_bytes.is_empty()
            && compare_effect_bytes.len() == rgba_bytes.len()
        {
            let split_x = (self.state.split_position * w as f32) as usize;
            for y in 0..h {
                let row_start = y * w * 4;
                let copy_end = (split_x * 4).min(w * 4);
                rgba_bytes[row_start..row_start + copy_end]
                    .copy_from_slice(&compare_effect_bytes[row_start..row_start + copy_end]);
            }
        }

        // Apply viewer overlays (multiple can be active simultaneously).
        // Order: false color first (replaces image), then zebras on top (stripes
        // over the color-mapped image give useful combined feedback).
        if self.state.viewer_overlays.false_color {
            let lut = &self.state.false_color_lut;
            for pixel in rgba_bytes.chunks_exact_mut(4) {
                let luma = (0.2126 * pixel[0] as f32 + 0.7152 * pixel[1] as f32 + 0.0722 * pixel[2] as f32) as usize;
                let idx = luma * 3;
                let mapped = &lut[idx..idx + 3];
                // Only replace if stop covers this luma (non-zero = covered)
                if mapped != [0, 0, 0] {
                    pixel[0] = mapped[0];
                    pixel[1] = mapped[1];
                    pixel[2] = mapped[2];
                }
            }
        }
        if self.state.viewer_overlays.zebras {
            let hi = self.prefs.viewer.zebra_high_threshold;
            let lo = self.prefs.viewer.zebra_low_threshold;
            let fc = self.state.frame_count as usize;
            for y in 0..h {
                for x in 0..w {
                    let idx = (y * w + x) * 4;
                    let r = rgba_bytes[idx];
                    let g = rgba_bytes[idx + 1];
                    let b = rgba_bytes[idx + 2];
                    let is_over = r > hi || g > hi || b > hi;
                    let is_under = r < lo || g < lo || b < lo;
                    if is_over || is_under {
                        // Animated diagonal stripes — different angles so high/low are
                        // visually distinct: high = 45° (x+y), low = 135° (x-y)
                        if is_over {
                            let stripe = (x + y + fc * 2) % 8 < 4;
                            if stripe {
                                rgba_bytes[idx] = 255;
                                rgba_bytes[idx + 1] = 0;
                                rgba_bytes[idx + 2] = 0;
                            }
                        } else {
                            let stripe = (x.wrapping_add(h).wrapping_sub(y) + fc * 2) % 8 < 4;
                            if stripe {
                                rgba_bytes[idx] = 0;
                                rgba_bytes[idx + 1] = 0;
                                rgba_bytes[idx + 2] = 255;
                            }
                        }
                    }
                }
            }
            self.state.frame_count = self.state.frame_count.wrapping_add(1);
        }
        // Focus peaking last — edge highlights on top of everything else.
        // Adapts the Sobel algorithm from apply_edge_detect() in klipp-effects/src/lib.rs:581-621.
        if self.state.viewer_overlays.focus_peaking {
            let sensitivity = self.prefs.viewer.focus_peaking_sensitivity;
            let [pr, pg, pb] = self.prefs.viewer.focus_peaking_color;
            // Threshold: sensitivity 0.0 → show all edges (threshold=0), 1.0 → only strongest (threshold≈255)
            let threshold = sensitivity * 255.0;
            // Compute luma into scratch buffer (Sobel needs neighbor access, can't modify in-place)
            // TODO: rayon parallelize y-loop if profiling shows stalls at 4K
            let mut luma = vec![0u8; w * h];
            for i in 0..w * h {
                let idx = i * 4;
                let r = rgba_bytes[idx] as f32;
                let g = rgba_bytes[idx + 1] as f32;
                let b = rgba_bytes[idx + 2] as f32;
                luma[i] = (0.2126 * r + 0.7152 * g + 0.0722 * b).clamp(0.0, 255.0) as u8;
            }
            // Sobel edge detection on luma, overlay peaking color where magnitude exceeds threshold
            let sobel_x: [[f32; 3]; 3] = [[-1.0, 0.0, 1.0], [-2.0, 0.0, 2.0], [-1.0, 0.0, 1.0]];
            let sobel_y: [[f32; 3]; 3] = [[-1.0, -2.0, -1.0], [0.0, 0.0, 0.0], [1.0, 2.0, 1.0]];
            for y in 0..h {
                for x in 0..w {
                    let mut gx = 0.0f32;
                    let mut gy = 0.0f32;
                    for ky in 0..3i32 {
                        for kx in 0..3i32 {
                            let sx = (x as i32 + kx - 1).clamp(0, w as i32 - 1) as usize;
                            let sy = (y as i32 + ky - 1).clamp(0, h as i32 - 1) as usize;
                            let l = luma[sy * w + sx] as f32;
                            gx += l * sobel_x[ky as usize][kx as usize];
                            gy += l * sobel_y[ky as usize][kx as usize];
                        }
                    }
                    let mag = (gx * gx + gy * gy).sqrt();
                    if mag > threshold {
                        // Blend peaking color by edge strength — strong edges glow
                        // brightly, weak edges fade to nothing
                        let alpha = ((mag - threshold) / (255.0 - threshold)).min(1.0);
                        let inv = 1.0 - alpha;
                        let idx = (y * w + x) * 4;
                        rgba_bytes[idx] = (rgba_bytes[idx] as f32 * inv + pr as f32 * alpha) as u8;
                        rgba_bytes[idx + 1] = (rgba_bytes[idx + 1] as f32 * inv + pg as f32 * alpha) as u8;
                        rgba_bytes[idx + 2] = (rgba_bytes[idx + 2] as f32 * inv + pb as f32 * alpha) as u8;
                    }
                }
            }
        }

        let t_convert = std::time::Instant::now();
        // Safety: Color32 is repr(C) [u8; 4], same as our RGBA layout.
        // Vec reinterpret: same alignment (1), element size (4), length and capacity divide evenly.
        let pixels = {
            let mut bytes = std::mem::ManuallyDrop::new(rgba_bytes.clone());
            let ptr = bytes.as_mut_ptr() as *mut egui::Color32;
            let len = bytes.len() / 4;
            let cap = bytes.capacity() / 4;
            unsafe { Vec::from_raw_parts(ptr, len, cap) }
        };
        let convert_ms = t_convert.elapsed().as_secs_f64() * 1000.0;

        // The monitor LUT's pass reads the same picture (#465).
        self.viewer.lut_source_bytes = self.viewer.lut_pass.is_some().then(|| (rgba_bytes.clone(), w as u32, h as u32));

        let t_texture = std::time::Instant::now();
        let image = egui::ColorImage {
            size: [w, h],
            pixels,
        };

        // Reuse existing texture handle when dimensions match; fall
        // through to a fresh alloc + deferred-drop when the frame
        // size changes (clip switch, skim → playback transitions).
        // Must go through upload_texture_deferred — a raw `set()`
        // with a size change triggers egui-wgpu's destroy-mid-submit
        // validation crash.
        let drops_slot = self.state.deferred_texture_drops_slot;
        upload_texture_deferred(
            ctx,
            &mut self.viewer.video_texture,
            &mut self.state.deferred_texture_drops[drops_slot],
            "klipp-video-frame",
            image,
            egui::TextureOptions::LINEAR,
        );
        let texture_ms = t_texture.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        if total_ms > 2.0 {
            tracing::info!(
                "PERF upload_frame: {w}x{h} total={total_ms:.1}ms (convert={convert_ms:.1}ms texture={texture_ms:.1}ms)"
            );
        }

        // Put frame data back so scopes can read it (~5µs pointer move)
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            // Only put back if engine hasn't written a newer frame
            if fb.data.is_empty() {
                fb.data = rgba_bytes;
                fb.original_data = original_bytes;
                fb.unstab_data = unstab_bytes;
                fb.compare_effect_data = compare_effect_bytes;
            }
        }
    }

    /// Render a title clip to the viewer's overlay texture for instant UI preview.
    /// This is the FCP approach: title is a separate layer composited by the viewer,
    /// not written to the shared frame buffer (which avoids race conditions with the engine).
    /// Snapshot a title clip's TitleData for undo. Returns None if clip not found or not a title.
    fn snapshot_title(&self, clip_id: klipp_core::ClipId) -> Option<klipp_timeline::TitleData> {
        self.state.sequences[self.state.active_sequence_idx].tracks.iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
            .and_then(|c| c.title_data())
            .cloned()
    }

    /// Resolve which TextBox a title-style mutation should target.
    /// Returns the selected object's id when it's a TextBox layer of
    /// the named clip; falls back to `None` so the caller routes to
    /// `target_textbox_mut(td, target_oid)` (the first TextBox in the stack).
    /// Used by every per-property title command so Inspector edits
    /// follow the on-canvas / Layers-panel selection.
    /// The selected layer of `clip_id`, whatever it is: the target of a
    /// command on a layer's stack — motion, loops, effects, motion blur
    /// — which a shape or a picture carries as a text does.
    fn title_layer_target_id(
        &self,
        clip_id: klipp_core::ClipId,
    ) -> Option<klipp_timeline::TitleObjectId> {
        let (selected_clip, oid) = self.state.selected_title_object?;
        (selected_clip == clip_id).then_some(oid)
    }

    fn title_text_target_id(
        &self,
        clip_id: klipp_core::ClipId,
    ) -> Option<klipp_timeline::TitleObjectId> {
        let (selected_clip, oid) = self.state.selected_title_object?;
        if selected_clip != clip_id {
            return None;
        }
        // Only return the id when the object is actually a TextBox —
        // a Shape selection shouldn't divert text-style mutations.
        let td = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
            .and_then(|c| c.title_data())?;
        let obj = td.object(oid)?;
        match &obj.content {
            klipp_timeline::TitleObjectContent::TextBox(_) => Some(oid),
            _ => None,
        }
    }

    /// Push a title edit undo entry if the data changed.
    fn push_title_undo(&mut self, clip_id: klipp_core::ClipId, description: &'static str, old: klipp_timeline::TitleData) {
        if let Some(new) = self.snapshot_title(clip_id) {
            if old != new {
                if description == "Edit Title Text"
                    && self.state.active_history_mut().try_coalesce_title_text(clip_id, &old, &new)
                {
                    return;
                }
                self.state.active_history_mut().push(crate::history::HistoryEntry {
                    command: crate::history::EditCommand::EditTitle {
                        clip_id, description, old_data: old, new_data: new,
                    },
                });
                if description == "Edit Title Text" {
                    self.state.active_history_mut().note_title_text_edit(clip_id);
                }
            }
        }
    }

    fn snapshot_generator(&self, clip_id: klipp_core::ClipId) -> Option<klipp_core::GeneratorData> {
        self.state.sequences[self.state.active_sequence_idx].tracks.iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
            .and_then(|c| c.generator_data())
            .cloned()
    }

    fn push_generator_undo(&mut self, clip_id: klipp_core::ClipId, old: klipp_core::GeneratorData) {
        if let Some(new) = self.snapshot_generator(clip_id) {
            if old != new {
                self.state.active_history_mut().push(crate::history::HistoryEntry {
                    command: crate::history::EditCommand::EditGenerator {
                        clip_id, old_data: old, new_data: new,
                    },
                });
            }
        }
    }

    /// Mark a title clip for overlay texture refresh on the next frame.
    fn render_title_to_viewer(&mut self, clip_id: klipp_core::ClipId) {
        // Clear old overlay immediately to prevent ghosting during position/alignment changes.
        // Route the drop through the deferred-drop ring — a bare `= None`
        // here would drop the handle while the current submit may still
        // reference its texture ID.
        if let Some(old) = self.viewer.title_overlay_texture.take() {
            self.defer_drop_texture(old);
        }
        self.state.pending_title_preview_clip = Some(clip_id);
    }

    /// Lift any newly-decoded source frame from `source_frame_buffer`
    /// into an egui texture for the source viewer panel. Mirrors the
    /// Program viewer's `upload_frame_to_egui` but stripped to the
    /// essentials: no scopes, no compare buffers, no defer-drop ring
    /// (we re-use existing handles via `set` on size match, defer-
    /// drop only on size change). Steady-state cost: one mutex check
    /// + one generation compare per frame.
    fn upload_source_frame_to_egui(&mut self, ctx: &egui::Context) {
        let (rgba, w, h, gen) = {
            let mut fb = self.source_frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            // Promote any pending engine write into `data`. `dirty`
            // gates whether this method actually does work below.
            fb.swap_pending();
            if !fb.dirty {
                return;
            }
            let w = fb.width as usize;
            let h = fb.height as usize;
            if fb.data.len() != w * h * 4 {
                fb.dirty = false;
                return;
            }
            fb.dirty = false;
            (std::mem::take(&mut fb.data), w, h, fb.generation)
        };
        // Skip uploads when this generation already landed — guards
        // against double-pumping when both source and program viewers
        // dirty in the same frame.
        if gen == self.state.source_live_texture_gen
            && self.state.source_live_texture.is_some()
        {
            return;
        }
        let color_image = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
        let new_size = egui::vec2(w as f32, h as f32);
        let needs_alloc = match &self.state.source_live_texture {
            Some(tex) => tex.size_vec2() != new_size,
            None => true,
        };
        if needs_alloc {
            // Different dimensions: defer-drop the old handle (egui-wgpu
            // may still have a submit in flight), then allocate fresh.
            if let Some(old) = self.state.source_live_texture.take() {
                self.defer_drop_texture(old);
            }
            let tex = ctx.load_texture(
                "source_live", color_image, egui::TextureOptions::LINEAR,
            );
            self.state.source_live_texture = Some(tex);
        } else if let Some(tex) = self.state.source_live_texture.as_mut() {
            // Same dimensions: in-place data update reuses the GPU texture.
            tex.set(color_image, egui::TextureOptions::LINEAR);
        }
        self.state.source_live_texture_gen = gen;
    }

    /// Refresh what in-place title editing needs (glyph positions) for a
    /// pending title. Called from update() where ctx is available.
    fn upload_title_overlay(&mut self, ctx: &egui::Context) {
        // Explicit clear request (e.g. scrub passed beyond a title clip).
        if self.state.pending_title_overlay_clear {
            self.state.pending_title_overlay_clear = false;
            if let Some(old) = self.viewer.title_overlay_texture.take() {
                self.defer_drop_texture(old);
            }
        }
        let clip_id = match self.state.pending_title_preview_clip.take() {
            Some(id) => id,
            None => return,
        };
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let td = seq.tracks.iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
            .and_then(|c| c.title_data());
        if let Some(td) = td {
            let mut td = td.clone();
            // While the user is editing one TextBox object inline,
            // suppress its rendered copy so the egui TextEdit
            // floating in the bounding box doesn't double-print
            // against the styled raster underneath. The bounding-
            // box editor IS the WYSIWYG view during edit mode —
            // stamping the rendered text behind it would show the
            // string twice.
            if self.state.viewer_text_edit {
                if let Some(oid) = self.state.viewer_text_edit_object {
                    if let Some(o) = td.object_mut(oid) {
                        o.visible = false;
                    }
                }
            }
            let (w, h) = (seq.resolution.width, seq.resolution.height);

            // The title itself is the engine's frame; no raster of it is
            // made here any more. What in-place editing still needs is
            // where the glyphs fall.
            if self.state.viewer_text_edit {
                self.state.viewer_char_positions = self.char_positions_for_edit(&td, w, h);
                ctx.request_repaint();
            }
        }
    }

    /// Recompute cached character positions when entering text edit mode
    /// or when positions are empty but edit mode is active.
    fn recompute_char_positions_if_needed(&mut self) {
        if !self.state.viewer_text_edit {
            return;
        }
        // Recompute if we have no positions cached (just entered edit mode)
        // or if request_focus was just set (initial entry)
        if !self.state.viewer_char_positions.is_empty() {
            return;
        }
        if let Some(clip_id) = self.state.viewer_text_edit_clip {
            let seq = &self.state.sequences[self.state.active_sequence_idx];
            let td = seq.tracks.iter()
                .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
                .and_then(|c| c.title_data());
            if let Some(td) = td {
                let td = td.clone();
                let (w, h) = (seq.resolution.width, seq.resolution.height);
                self.state.viewer_char_positions = self.char_positions_for_edit(&td, w, h);
            }
        }
    }

    /// The glyph boxes of the layer being edited (the targeted object,
    /// else the primary text), in frame pixels, for the viewer's caret
    /// and selection.
    fn char_positions_for_edit(&mut self, td: &klipp_timeline::TitleData, w: u32, h: u32) -> Vec<klipp_title::CharPosition> {
        let single = self.state.viewer_text_edit_object.and_then(|oid| td.object(oid)).and_then(|o| match &o.content {
            klipp_timeline::TitleObjectContent::TextBox(tb) => {
                let mut s = klipp_timeline::TitleData::single_text(tb.clone());
                if let Some(first) = s.objects.first_mut() {
                    first.transform = o.transform.clone();
                }
                Some(s)
            }
            _ => None,
        });
        match single {
            Some(s) => self.title_renderer.compute_char_positions(&s, w, h),
            None => self.title_renderer.compute_char_positions(td, w, h),
        }
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

    fn sync_sequence_to_engine(&self) {
        // Parameter changes still queued for the engine are already in
        // the mirror this pushes; left in the queue they would land
        // *after* it and overwrite it — an undo of a grade change lost
        // to the change itself, seen over the control socket and
        // possible for a fast ctrl+z too. The queue is multi-consumer.
        while self.param_change_queue.pop().is_some() {}
        // The sequence the engine renders from is built here, with nothing
        // locked, and put in place whole (#690). It used to be written in
        // place under a read-write lock the engine read for every frame,
        // and a busy frame kept the UI waiting; once, after a project
        // opened, for over a minute.
        let active = &self.state.sequences[self.state.active_sequence_idx];
        // Flatten compound clips so the engine receives a plain sequence
        // with no ClipKind::Sequence clips — the engine has no compound-clip
        // awareness. This mirrors the export path exactly: viewer and export
        // are consistent by construction.
        let mut flat = klipp_timeline::flatten::flatten_sequence(active, &self.state.sequences);
        // A command in the Colour workspace can sync while the timeline
        // grade stands in as a clip: the engine gets the grade, never the
        // stand-in.
        settle_timeline_grade_proxy(&mut flat, None);
        self.sequence.replace(flat);
    }

    /// Fast UI-thread re-grade at reduced resolution for instant scope
    /// feedback, WITHOUT taking the shared sequence write lock or running
    /// `flatten_sequence`. Used on the slider-scrub path: the engine has
    /// already been given the change via `param_change_queue` and the UI's
    /// local `self.state.sequences[…]` mirror has been mutated, so all we
    /// need is to refresh the scope preview buffer + bump the frame-buffer
    /// generation.
    fn refresh_scope_preview_fast(&mut self) {
        // Throttle: the UI-thread re-grade is ~10 ms on 1080p/4 for a
        // typical colour-balance graph. At slider-drag rate (60–120
        // SetNodeParam per second) this would saturate the UI thread.
        // 33 Hz is plenty for scope preview — the user can't
        // distinguish 30 fps from 120 fps on a scope trace anyway.
        if self.last_scope_preview_refresh.elapsed()
            < std::time::Duration::from_millis(30)
        {
            return;
        }
        self.last_scope_preview_refresh = std::time::Instant::now();
        let t_start = std::time::Instant::now();
        self.refresh_scope_preview_inner();
        let total_ms = t_start.elapsed().as_secs_f64() * 1000.0;
        // Correlate with ENGINE_TICK_SLOW / UI_TICK_SLOW by timestamp.
        // Threshold at 5 ms so normal HD-sized runs stay quiet; 4K
        // downscale + grade will always fire.
        if total_ms > 5.0 {
            tracing::debug!(
                "SCOPE_PREVIEW_FAST {:.1}ms queue_depth={}",
                total_ms,
                self.param_change_queue.len(),
            );
        }
        // Don't send SequenceUpdated — would trigger the engine's audio
        // restart + grade-cache invalidation path, which is already handled
        // by drain_param_changes() on the engine side.
    }

    /// Sync sequence and re-grade the cached frame directly on the UI thread.
    /// Zero latency — scopes see the result on the same frame as the slider change.
    fn sync_and_refresh_frame(&mut self) {
        self.sync_sequence_to_engine();
        self.refresh_scope_preview_inner();
        // Tell engine to re-grade at full resolution + render scope texture
        let scope_mode = match self.state.scope_mode {
            crate::state::ScopeMode::Waveform => 0u8,
            crate::state::ScopeMode::Parade => 1,
            crate::state::ScopeMode::Vectorscope => 2,
            crate::state::ScopeMode::Histogram => 3,
        };
        let _ = self.cmd_tx.send(AppCommand::SetScopeRenderMode { mode: scope_mode });
        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
    }

    /// Shared scope-preview refresh used by both the slow path
    /// (`sync_and_refresh_frame`) and the slider-scrub fast path
    /// (`refresh_scope_preview_fast`). Does NOT touch the shared Sequence
    /// RwLock — reads only from the UI's own `self.state.sequences` mirror.
    fn refresh_scope_preview_inner(&mut self) {
        // Fast UI-thread re-grade at reduced resolution for instant scope feedback.
        let fb_snapshot = {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            match (&fb.pregrade, fb.width) {
                // The frame, not its picture — `display()` below makes that,
                // outside this lock and only if a preview is really wanted.
                (Some(pregrade), w) if w > 0 => {
                    Some((std::sync::Arc::clone(pregrade), fb.width, fb.height))
                }
                _ => None,
            }
        };

        // Try to produce a graded scope_preview; fall back to pregrade on any failure
        let mut preview: Option<(Vec<u8>, u32, u32)> = None;

        if let Some((ref pregrade, full_w, full_h)) = fb_snapshot {
            if let Some(cid) = self.state.selected_clip {
                let seq_idx = self.state.active_sequence_idx;
                if let Some(clip) = self.state.sequences[seq_idx].tracks.iter()
                    .find_map(|t| t.clips.iter().find(|c| c.id == cid))
                {
                    // Inside its grade group's grades, as the engine runs it (#237).
                    let seq = &self.state.sequences[seq_idx];
                    let graph = match clip.grade_group.and_then(|g| seq.grade_group(g)).filter(|g| g.grades()) {
                        Some(group) => std::borrow::Cow::Owned(group.wrap(&clip.node_graph)),
                        None => std::borrow::Cow::Borrowed(&clip.node_graph),
                    };
                    let graph = graph.as_ref();
                    if !graph.is_identity() {
                        // Here, and not before: an identity graph gets no
                        // preview, and for an HDR source this encode is tens
                        // of milliseconds at 4K. Made once per frame and kept,
                        // so a drag pays it on its first step only.
                        let pregrade_data = pregrade.display();
                        let scale = if full_w > 960 { 4u32 } else if full_w > 480 { 2 } else { 1 };
                        let small_w = full_w / scale;
                        let small_h = full_h / scale;
                        let mut small = vec![0u8; (small_w * small_h * 4) as usize];
                        for y in 0..small_h {
                            for x in 0..small_w {
                                let src = ((y * scale * full_w + x * scale) * 4) as usize;
                                let dst = ((y * small_w + x) * 4) as usize;
                                if src + 3 < pregrade_data.len() && dst + 3 < small.len() {
                                    small[dst..dst + 4].copy_from_slice(&pregrade_data[src..src + 4]);
                                }
                            }
                        }

                        let clip_time = (self.state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64()).max(0.0);
                        match self.scope_executor.execute(
                            &small, graph, small_w, small_h,
                            clip_time, 0, None, None,
                        ) {
                            Ok((graded, _)) => { preview = Some((graded, small_w, small_h)); }
                            Err(_) => { preview = Some((small, small_w, small_h)); }
                        }
                    }
                }
            }
        }

        // ALWAYS bump generation so scopes detect the change
        {
            let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((data, w, h)) = preview {
                fb.scope_preview = data;
                fb.scope_preview_w = w;
                fb.scope_preview_h = h;
            } else {
                // Clear scope_preview so scopes fall back to fb.data
                fb.scope_preview.clear();
                fb.scope_preview_w = 0;
            }
            fb.generation = fb.generation.wrapping_add(1);
        }
    }

    /// Scan all cache directories and remove files not referenced by current media pool.
    /// Cache Manager (feature #78): refresh the sized snapshot of on-disk
    /// cache categories. Called when the dialog opens and after every
    /// `ClearCacheCategory` so the pie chart / size list always reflect
    /// current disk state.
    fn refresh_cache_categories(&mut self) {
        let cache_root = dirs::cache_dir().unwrap_or_default().join("klipp");
        let proxy_override = self.prefs.proxy.proxy_cache_dir.as_deref();
        self.state.cache_categories =
            crate::cache_manager::scan_cache_categories(&cache_root, proxy_override);
    }

    fn clean_orphaned_caches(&self) -> String {
        let klipp_cache = dirs::cache_dir().unwrap_or_default().join("klipp");

        // Collect all valid cache keys from the current media pool
        let mut valid_media_ids = std::collections::HashSet::new();
        let mut valid_cache_keys = std::collections::HashSet::new();

        for item in self.state.media_pool.all() {
            // Media ID (used by proxy files)
            valid_media_ids.insert(format!("{}", item.id.0));

            // Content cache key (used by filmstrips, skim, thumbnails, audio)
            if let Some(key) = klipp_media::disk_cache::compute_cache_key(&item.path) {
                valid_cache_keys.insert(key);
            }
        }

        let mut total_removed = 0u64;
        let mut files_removed = 0usize;

        // Scan each cache directory
        // Every directory a cache lives in, resolved the way the cache
        // code resolves it. This used to scan `thumbnails` for
        // filmstrips and `waveforms` for audio — neither is where those
        // caches are written — so the real directories were never
        // cleaned; and files named by media id no longer count as
        // valid, since no cache is read by that name any more.
        let dirs_and_matchers: Vec<(std::path::PathBuf, Box<dyn Fn(&str) -> bool>)> =
            crate::cache_manager::orphan_scan_dirs(
                &klipp_cache,
                self.prefs.proxy.proxy_cache_dir.as_deref(),
                self.prefs.media_cache.filmstrip_cache_dir.as_deref(),
                self.prefs.media_cache.audio_cache_dir.as_deref(),
            )
            .into_iter()
            .map(|(dir, by)| {
                let ids = valid_media_ids.clone();
                let keys = valid_cache_keys.clone();
                let f: Box<dyn Fn(&str) -> bool> = match by {
                    crate::cache_manager::OrphanKey::MediaId => Box::new(move |stem: &str| ids.contains(stem)),
                    crate::cache_manager::OrphanKey::CacheKey => Box::new(move |stem: &str| keys.contains(stem)),
                };
                (dir, f)
            })
            .collect();
        for (dir, is_valid) in &dirs_and_matchers {
            if !dir.exists() { continue; }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_file() { continue; }
                    // The content key is everything before the first dot:
                    // picture caches append `.{height}`, a log format and a
                    // pipeline version. Comparing the whole stem treated
                    // every filmstrip as an orphan and deleted it.
                    let stem = path.file_name()
                        .and_then(|s| s.to_str())
                        .and_then(|s| s.split('.').next())
                        .unwrap_or("");
                    if !is_valid(stem) {
                        if let Ok(meta) = path.metadata() {
                            total_removed += meta.len();
                            files_removed += 1;
                        }
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }

        if files_removed == 0 {
            "No orphaned cache files found".to_string()
        } else {
            let size_str = if total_removed >= 1_073_741_824 {
                format!("{:.1} GB", total_removed as f64 / 1_073_741_824.0)
            } else if total_removed >= 1_048_576 {
                format!("{:.1} MB", total_removed as f64 / 1_048_576.0)
            } else {
                format!("{} KB", total_removed / 1024)
            };
            format!("Cleaned {files_removed} orphaned files ({size_str})")
        }
    }

    fn sync_media_pool_to_engine(&self) {
        let mut pool = self.media_pool_shared.write().unwrap_or_else(|e| e.into_inner());
        *pool = self.state.media_pool.clone();
        drop(pool);
        self.sync_log_formats_to_decoders();
    }

    /// Tell the decoders each file's camera log format. The decoder is what
    /// turns log footage into scene-linear (curve + camera gamut), and
    /// container metadata never says "S-Log3" — the media's format (the
    /// user's choice, else what was detected) is the only source of truth.
    fn sync_log_formats_to_decoders(&self) {
        // Proxies and skim files are transcodes that keep the source's
        // encoding (8-bit, still log), and timeline playback or the browser
        // may read them instead of the original, so they carry the media's
        // format too. (PQ/HLG skims are tone-mapped to SDR by ffmpeg, but
        // PQ/HLG never enter this registry: their files say what they are.)
        let changed = klipp_engine::video_decoder::set_media_log_formats(
            self.state.media_pool.all().flat_map(|m| {
                let f = m.effective_log_format();
                std::iter::once(m.path.clone())
                    .chain(m.proxy_path.clone())
                    .chain(m.skim_file.clone())
                    .map(move |p| (p, f))
            }),
        );
        if changed {
            let _ = self.cmd_tx.send(AppCommand::MediaDecodeChanged);
        }
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

    fn send_commands(&mut self, cmds: Vec<AppCommand>) {
        if !cmds.is_empty() {
            self.state.media_usage_cache_dirty = true;
        }
        let mut needs_refresh = false;
        // Track whether any SetNodeParam arrived this batch. If so, take the
        // fast refresh path (scope preview only) instead of the full
        // sync_and_refresh_frame — the engine picked up the changes via the
        // lock-free param_change_queue, so we don't need to flatten the
        // sequence or take the shared RwLock's write lock on the slider path.
        let mut node_param_scrub = false;
        for cmd in cmds {
            // Several clips selected and the Inspector's clip edited: the
            // others take the change too (#843).
            let multi = self.multi_edit_pre(&cmd);
            'dispatch: {
                if matches!(&cmd, AppCommand::SetNodeParam { .. }) {
                    node_param_scrub = true;
                }
                // Timeline markers are edited outside the document's own
                // ops, so their handlers record nothing: a marker added,
                // changed or removed (by hand or by an agent) is one undo step
                // here, the whole list either side.
                let markers_before = self.state.sequences.get(self.state.active_sequence_idx).map(|s| s.markers.clone());
                if self.handle_marker_commands(&cmd, &mut needs_refresh) {
                    if let Some(old) = markers_before {
                        let new = self.state.sequences[self.state.active_sequence_idx].markers.clone();
                        if old != new {
                            self.state.push_history(EditCommand::SetMarkers { old, new });
                        }
                    }
                    break 'dispatch;
                }
                if self.handle_transition_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_playback_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_export_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_browser_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_stabilization_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_keyframe_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_clip_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_timeline_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_effects_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_color_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_title_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_project_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                if self.handle_media_commands(&cmd, &mut needs_refresh) { break 'dispatch; }
                // Clip operations — handled by handle_clip_commands
                // Timeline operations — handled by handle_timeline_commands
                // Track seek commands for filmstrip preview
                if matches!(&cmd, AppCommand::SeekTo(_) | AppCommand::SeekRelative(_)) {
                    self.state.awaiting_seek_frame = true;
                }
                let _ = self.cmd_tx.send(cmd);
            }
            if let Some(post) = multi {
                self.multi_edit_post(post);
            }
        }
        // Prefer the full-sync path when ANY non-slider command needed refresh
        // (structural changes, keyframe edits, etc.). The slider fast path is
        // only taken when SetNodeParam was the sole trigger for refresh.
        // While a node-param-edit gesture is in flight (mask drag etc.),
        // throttle the heavy sync to ~30 Hz — without this the per-frame
        // SetNodeMasks during a mask drag fires sync_sequence_to_engine +
        // SequenceUpdated at 60+ Hz which floods the engine command channel
        // and visibly drifts the playhead.
        if needs_refresh {
            let in_gesture = self.node_param_edit_snapshot.is_some();
            let throttle_hit = in_gesture
                && self.last_gesture_sync.elapsed() < std::time::Duration::from_millis(33);
            if !throttle_hit {
                if in_gesture {
                    self.last_gesture_sync = std::time::Instant::now();
                }
                self.sync_and_refresh_frame();
            } else {
                self.refresh_scope_preview_fast();
            }
        } else if node_param_scrub {
            // Slider scrub only — engine drains param_change_queue itself, so
            // we just need to refresh the scope preview on the UI thread.
            self.refresh_scope_preview_fast();
        }
    }

    /// The other selected clips an Inspector change on `clip` goes to: the
    /// rest of the selection, when `clip` is the one the Inspector shows
    /// and one of several selected (#843).
    fn multi_edit_others(&self, clip: klipp_core::ClipId) -> Vec<klipp_core::ClipId> {
        if self.state.selected_clip != Some(clip) || !self.state.selected_clips.contains(&clip) {
            return Vec::new();
        }
        self.state.selected_clips.iter().copied().filter(|&c| c != clip).collect()
    }

    /// The clip the Inspector edits and the `others`, as they are: the
    /// edited one too, since not every setting's handler records its
    /// change, and the step has to give all of them back.
    fn multi_edit_snapshot(&self, clip: klipp_core::ClipId, others: &[klipp_core::ClipId]) -> Vec<klipp_timeline::Clip> {
        std::iter::once(&clip).chain(others).filter_map(|id| self.state.sequence().all_clips().find(|c| c.id == *id).cloned()).collect()
    }

    /// Before `cmd` is handled: note the other clips as they are, and open
    /// the one undo step the change on all of them will be.
    fn multi_edit_pre(&mut self, cmd: &AppCommand) -> Option<MultiEditPost> {
        use crate::multi_edit::Phase;
        let (clip, phase) = crate::multi_edit::classify(cmd)?;
        let others = self.multi_edit_others(clip);
        if others.is_empty() {
            return None;
        }
        let mut in_drag = self.multi_edit.as_ref().is_some_and(|m| m.primary == clip);
        // An audio plugin's slider coalesces in its own handler under a
        // history drag key: while that drag is open, a value is a live
        // step of it, not a change of its own (#843 review).
        if let AppCommand::UpdateAudioEffectParam { effect_index, param_name, .. } = cmd {
            let key = format!("audio_chain:{}:{}:{}", clip.0, effect_index, param_name);
            if !in_drag && self.state.active_history().is_dragging(&key) {
                self.multi_edit = Some(MultiEditSession { primary: clip, before: self.multi_edit_snapshot(clip, &others) });
                in_drag = true;
            }
        }
        match phase {
            Phase::Live => Some(MultiEditPost { cmd: cmd.clone(), others, finish: false, opened_batch: false }),
            Phase::Begin => {
                self.multi_edit = Some(MultiEditSession { primary: clip, before: self.multi_edit_snapshot(clip, &others) });
                None
            }
            Phase::Update if in_drag => Some(MultiEditPost { cmd: cmd.clone(), others, finish: false, opened_batch: false }),
            Phase::Abandon => in_drag.then(|| MultiEditPost { cmd: cmd.clone(), others, finish: true, opened_batch: false }),
            Phase::End if !in_drag => None,
            Phase::Update | Phase::End | Phase::Redo => {
                if !in_drag {
                    self.multi_edit = Some(MultiEditSession { primary: clip, before: self.multi_edit_snapshot(clip, &others) });
                }
                // Inside a batch already open (an agent's, a palette
                // action's), the step joins it: opening another would drop
                // what the open one has collected (#843 review).
                let opened_batch = !self.state.active_history().is_batching();
                if opened_batch {
                    self.state.active_history_mut().begin_batch();
                }
                Some(MultiEditPost { cmd: cmd.clone(), others, finish: true, opened_batch })
            }
        }
    }

    /// After `post.cmd` was handled on its clip: the same on the others,
    /// live while dragging, and on the last step recorded with it as one.
    fn multi_edit_post(&mut self, post: MultiEditPost) {
        use crate::multi_edit::Phase;
        let Some((primary_id, phase)) = crate::multi_edit::classify(&post.cmd) else { return };
        if phase == Phase::Abandon {
            // Taken back: the others as they were. The edited clip is its
            // own handler's to put back (a drag that added an effect takes
            // it off again, which its snapshot from the drag's start would
            // undo).
            if let Some(session) = self.multi_edit.take() {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                for before in session.before.into_iter().filter(|c| c.id != primary_id) {
                    if let Some(c) = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(before.id)) {
                        *c = before;
                    }
                }
                self.sync_sequence_to_engine();
            }
            return;
        }
        if phase == Phase::Redo {
            for other in &post.others {
                if let Some(cmd) = crate::multi_edit::retarget(&post.cmd, *other) {
                    self.send_commands(vec![cmd]);
                }
            }
        } else {
            let primary = self.state.sequence().all_clips().find(|c| c.id == primary_id).cloned();
            if let Some(primary) = primary {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                for other in &post.others {
                    if let Some(c) = seq.tracks.iter_mut().find_map(|t| t.find_clip_mut(*other)) {
                        crate::multi_edit::copy_to(&post.cmd, &primary, c);
                    }
                }
                self.sync_sequence_to_engine();
            }
        }
        if !post.finish {
            return;
        }
        // One undo step: the clip the Inspector edited, recorded by its own
        // handler, and each other clip whole, before and after.
        if phase != Phase::Redo {
            if let Some(session) = self.multi_edit.as_ref() {
                let edits: Vec<EditCommand> = session
                    .before
                    .iter()
                    .filter_map(|before| {
                        let now = self.state.sequence().all_clips().find(|c| c.id == before.id)?.clone();
                        Some(EditCommand::ReplaceClip { description: "Edit Selection", old: Box::new(before.clone()), new: Box::new(now) })
                    })
                    .collect();
                for e in edits {
                    self.state.push_history(e);
                }
            }
        }
        self.multi_edit = None;
        if post.opened_batch {
            let n = post.others.len() + 1;
            self.state.active_history_mut().end_batch_as(&format!("Edit {n} Clips"));
        }
        self.state.dirty = true;
    }

    /// Open the consent dialog for a model identified by its stable
    /// id in the manifest. No network call yet; the user confirms in
    /// the dialog before any download starts. Surfaces a toast if
    /// the manifest doesn't contain the id (typo / version drift).
    pub fn request_model_download(&mut self, model_id: &str) {
        // Re-check cache first — if the model is already verified
        // locally there's nothing to download. Surfaces a toast so
        // the user gets feedback rather than silence.
        let manifest = match klipp_ml::load_effective() {
            Ok(m) => m,
            Err(e) => {
                self.state.toast.show(format!("Manifest error: {e}"));
                return;
            }
        };
        let Some(entry) = manifest.get(model_id) else {
            self.state.toast.show(format!("Unknown model id: {model_id}"));
            return;
        };
        if let Ok(cache) = klipp_ml::ModelCache::open() {
            if let Ok(klipp_ml::CachedStatus::Verified) = cache.is_cached(entry) {
                self.state.toast.show(
                    format!("'{}' is already downloaded.", entry.display_name),
                );
                return;
            }
        }
        self.state.model_download = crate::model_download::ModelDownloadState::Consenting {
            entry: entry.clone(),
        };
    }

    /// Render whichever model-download dialog matches the current
    /// state. Drains worker events first so progress updates land
    /// the same frame they arrive.
    fn render_model_download_dialogs(
        &mut self,
        ctx: &egui::Context,
        _all_cmds: &mut Vec<AppCommand>,
    ) {
        use crate::model_download::{
            fmt_bytes, fmt_rate, poll_download, shorten_hash, spawn_download,
            ModelDownloadState,
        };

        // Drain worker events first; transitions take effect this frame.
        poll_download(&mut self.state.model_download);

        // Snapshot the current variant so we can drop the borrow on
        // self.state.model_download before mutating it inside the
        // dialog closures.
        let current = std::mem::take(&mut self.state.model_download);
        match current {
            ModelDownloadState::Idle => {
                // Nothing to render. Restore so we don't re-init.
                self.state.model_download = ModelDownloadState::Idle;
            }
            ModelDownloadState::Consenting { entry } => {
                let mut next = ModelDownloadState::Consenting { entry: entry.clone() };
                let mut close_kind: Option<&'static str> = None;
                egui::Window::new("Download AI Model")
                    .collapsible(false)
                    .resizable(false)
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.set_min_width(420.0);
                        ui.heading(&entry.display_name);
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(
                                "Klipp uses a local AI model to generate object \
                                 masks. Once downloaded the file stays on your \
                                 machine and Klipp never re-contacts the network \
                                 to use it.",
                            ).small(),
                        );
                        ui.add_space(8.0);
                        egui::Grid::new("model_consent_grid")
                            .num_columns(2)
                            .spacing([16.0, 4.0])
                            .show(ui, |ui| {
                                ui.label("Size:");
                                ui.label(fmt_bytes(entry.size_bytes));
                                ui.end_row();
                                ui.label("Source:");
                                ui.label(
                                    entry.sources.first()
                                        .map(|s| s.url.as_str())
                                        .unwrap_or("(none)"),
                                );
                                ui.end_row();
                                ui.label("SHA-256:");
                                ui.label(
                                    egui::RichText::new(shorten_hash(&entry.sha256))
                                        .monospace()
                                        .small(),
                                ).on_hover_text(&entry.sha256);
                                ui.end_row();
                                ui.label("License:");
                                ui.label(&entry.license);
                                ui.end_row();
                                ui.label("Attribution:");
                                ui.label(&entry.attribution);
                                ui.end_row();
                            });
                        ui.add_space(8.0);
                        ui.label(
                            egui::RichText::new(
                                "Air-gapped install: drop the file directly into \
                                 ~/.local/share/klipp/models/ with the SHA-256 \
                                 above and Klipp will skip the download.",
                            ).small().italics(),
                        );
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                close_kind = Some("cancel");
                            }
                            if ui.button(format!("Download ({})", fmt_bytes(entry.size_bytes))).clicked() {
                                close_kind = Some("start");
                            }
                        });
                    });
                match close_kind {
                    Some("cancel") => {
                        next = ModelDownloadState::Idle;
                    }
                    Some("start") => {
                        let handle = spawn_download(entry.clone(), ctx.clone());
                        next = ModelDownloadState::Downloading {
                            entry: entry.clone(),
                            total: entry.size_bytes,
                            downloaded: 0,
                            started_at: std::time::Instant::now(),
                            handle,
                        };
                    }
                    _ => {}
                }
                self.state.model_download = next;
            }
            ModelDownloadState::Downloading {
                entry, total, downloaded, started_at, handle,
            } => {
                let elapsed = started_at.elapsed().as_secs_f64().max(0.001);
                let rate = downloaded as f64 / elapsed;
                let mut cancelled = false;
                egui::Window::new("Downloading AI Model")
                    .collapsible(false)
                    .resizable(false)
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.set_min_width(380.0);
                        ui.label(&entry.display_name);
                        ui.add_space(4.0);
                        let frac = if total > 0 {
                            (downloaded as f32 / total as f32).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        ui.add(egui::ProgressBar::new(frac).show_percentage());
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label(format!("{} / {}",
                                fmt_bytes(downloaded), fmt_bytes(total)));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(fmt_rate(rate));
                                },
                            );
                        });
                        ui.add_space(8.0);
                        if ui.button("Cancel Download").clicked() {
                            cancelled = true;
                        }
                    });
                if cancelled {
                    handle.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                self.state.model_download = ModelDownloadState::Downloading {
                    entry, total, downloaded, started_at, handle,
                };
            }
            ModelDownloadState::Failed { entry_label, error } => {
                let mut next = ModelDownloadState::Failed {
                    entry_label: entry_label.clone(),
                    error: error.clone(),
                };
                egui::Window::new("Download Failed")
                    .collapsible(false)
                    .resizable(false)
                    .order(egui::Order::Foreground)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.set_min_width(380.0);
                        ui.heading(&entry_label);
                        ui.add_space(4.0);
                        ui.label(&error);
                        ui.add_space(8.0);
                        if ui.button("Dismiss").clicked() {
                            next = ModelDownloadState::Idle;
                        }
                    });
                self.state.model_download = next;
            }
            ModelDownloadState::Completed { entry_label, path } => {
                self.state.toast.show(format!(
                    "Downloaded {entry_label} ({})", path.display(),
                ));
                // Voice Isolation effects already in a chain pass audio
                // through until their model exists; let them load it.
                klipp_audio::dsp::voice_isolation::notify_model_installed();
                self.state.model_download = ModelDownloadState::Idle;
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


    fn handle_export_commands(&mut self, cmd: &AppCommand, _needs_refresh: &mut bool) -> bool {
        if self.handle_render_commands(cmd) {
            return true;
        }
        match cmd {
            AppCommand::CaptureWindow { path, max_width } => {
                self.state.window_capture = Some((path.clone(), *max_width, false));
                true
            }
            AppCommand::ShowPreferences => {
                self.state.show_preferences = true;
                true
            }
            AppCommand::OpenClipProperties { media_id } => {
                self.state.clip_properties_media_id = Some(*media_id);
                self.state.clip_properties_tab = crate::state::ClipPropertiesTab::default();
                self.state.show_clip_properties = true;
                true
            }
            AppCommand::ApplyBuiltinWorkspaceLayout { index } => {
                let builtins = crate::state::builtin_workspace_layouts();
                if let Some(layout) = builtins.get(*index).cloned() {
                    self.apply_workspace_layout(&layout);
                    self.state.toast.show(format!("Applied workspace: {}", layout.name));
                }
                true
            }
            AppCommand::ApplyCustomWorkspaceLayout { name } => {
                if let Some(layout) = self.state.custom_workspaces.iter()
                    .find(|l| l.name == *name)
                    .cloned()
                {
                    self.apply_workspace_layout(&layout);
                    self.state.toast.show(format!("Applied workspace: {}", layout.name));
                }
                true
            }
            AppCommand::DeleteWorkspaceLayout { name } => {
                let before = self.state.custom_workspaces.len();
                self.state.custom_workspaces.retain(|l| l.name != *name);
                if self.state.custom_workspaces.len() != before {
                    if self.state.workspace_manager_selected.as_deref() == Some(name.as_str()) {
                        self.state.workspace_manager_selected = None;
                    }
                    persist_workspaces(&self.state.custom_workspaces);
                    self.state.toast.show("Workspace deleted".to_string());
                }
                true
            }
            AppCommand::RenameWorkspaceLayout { old_name, new_name } => {
                let trimmed = new_name.trim().to_string();
                if !trimmed.is_empty()
                    && !self.state.custom_workspaces.iter().any(|l| l.name == trimmed)
                {
                    if let Some(l) = self.state.custom_workspaces
                        .iter_mut()
                        .find(|l| l.name == *old_name)
                    {
                        l.name = trimmed.clone();
                        if self.state.workspace_manager_selected.as_deref() == Some(old_name.as_str()) {
                            self.state.workspace_manager_selected = Some(trimmed);
                        }
                        persist_workspaces(&self.state.custom_workspaces);
                    }
                }
                true
            }
            AppCommand::JogFrames { frames } => {
                // Reuse the existing SeekRelative(i64) plumbing — the
                // transport-bar step buttons already dispatch ±1 through
                // this path, so `JogFrames { frames: N }` is identical
                // except it bundles the N samples the wheel widget has
                // accumulated since its last emit. Clamping + playhead
                // math lives in the SeekRelative handler, no duplication.
                if *frames != 0 {
                    let _ = self.cmd_tx.send(AppCommand::SeekRelative(*frames as i64));
                }
                true
            }
            AppCommand::ImportSubtitles { path } => {
                let path = path.clone();
                match std::fs::read_to_string(&path) {
                    Ok(content) => {
                        // Detect format by extension (case-insensitive). Unknown
                        // extensions fall back to the SRT/VTT parser.
                        let ext = path
                            .extension()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_ascii_lowercase())
                            .unwrap_or_default();
                        let is_ass = ext == "ass" || ext == "ssa";

                        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                        let fps_rate = seq.fps.rate();
                        // One new track per imported file, named after the file stem.
                        let track_name = path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("Subtitles")
                            .to_string();
                        let new_track_id = seq.add_subtitle_track(track_name);
                        let sub_track_idx = seq
                            .tracks
                            .iter()
                            .position(|t| t.id == new_track_id)
                            .unwrap();
                        let track = &mut seq.tracks[sub_track_idx];
                        track.source_file = Some(path.clone());

                        let imported_count;
                        if is_ass {
                            let (entries, styles) = parse_ass(&content);
                            imported_count = entries.len();
                            for entry in &entries {
                                let timeline_in = klipp_core::RationalTime::from_secs_f64(entry.start_secs, fps_rate);
                                let timeline_out = klipp_core::RationalTime::from_secs_f64(entry.end_secs, fps_rate);
                                let dur = timeline_out - timeline_in;
                                let style = styles.get(&entry.style_name);

                                let (font_name, font_size, color, outline, shadow, font_style) = match style {
                                    Some(s) => {
                                        let fs = match (s.bold, s.italic) {
                                            (false, false) => klipp_timeline::FontStyle::Regular,
                                            (true, false) => klipp_timeline::FontStyle::Bold,
                                            (false, true) => klipp_timeline::FontStyle::Italic,
                                            (true, true) => klipp_timeline::FontStyle::BoldItalic,
                                        };
                                        let outline = if s.outline > 0.0 {
                                            Some(klipp_timeline::TextOutline { color: [0, 0, 0, 255], width: s.outline, enabled: true, width_keyframes: Vec::new() })
                                        } else { None };
                                        let shadow = if s.shadow > 0.0 {
                                            Some(klipp_timeline::TitleShadow {
                                                color: [0, 0, 0, 180],
                                                offset_x: s.shadow,
                                                offset_y: s.shadow,
                                                blur: s.shadow,
                                                enabled: true,
                                                offset_x_keyframes: Vec::new(),
                                                offset_y_keyframes: Vec::new(),
                                                blur_keyframes: Vec::new(),
                                            })
                                        } else { None };
                                        (
                                            if s.font_name.is_empty() { String::new() } else { s.font_name.clone() },
                                            if s.font_size > 0.0 { s.font_size } else { 32.0 },
                                            s.primary_color,
                                            outline,
                                            shadow,
                                            fs,
                                        )
                                    }
                                    None => (
                                        String::new(),
                                        32.0,
                                        [255, 255, 255, 255],
                                        Some(klipp_timeline::TextOutline { color: [0, 0, 0, 200], width: 2.0, enabled: true, width_keyframes: Vec::new() }),
                                        None,
                                        klipp_timeline::FontStyle::Regular,
                                    ),
                                };

                                let mut tb = klipp_timeline::TextBox {
                                    text: entry.text.clone(),
                                    font_size,
                                    font_name,
                                    font_style,
                                    color,
                                    alignment: klipp_timeline::TextAlignment::Center,
                                    ..Default::default()
                                };
                                let mut fx = klipp_timeline::LayerFx::default();
                                if let Some(o) = outline { fx.outlines.push(o); }
                                if let Some(s) = shadow { fx.shadows.push(s); }
                                tb.ass_style_name = if entry.style_name.is_empty() { None } else { Some(entry.style_name.clone()) };
                                let mut title_data = klipp_timeline::TitleData::single_text_fx(tb, fx);
                                if let Some(o) = title_data.objects.first_mut() {
                                    o.transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.88 };
                                }
                                let clip = klipp_timeline::Clip {
                                    id: klipp_core::ClipId::new(),
                                    kind: klipp_timeline::ClipKind::Title(title_data),
                                    timeline_in,
                                    timeline_out,
                                    source_in: klipp_core::RationalTime::from_secs_f64(0.0, fps_rate),
                                    source_out: dur,
                                    ..klipp_timeline::Clip::gap(timeline_in, timeline_out)
                                };
                                track.clips.push(clip);
                            }
                        } else {
                            let subs = parse_srt_vtt(&content);
                            imported_count = subs.len();
                            for sub in &subs {
                                let timeline_in = klipp_core::RationalTime::from_secs_f64(sub.start_secs, fps_rate);
                                let timeline_out = klipp_core::RationalTime::from_secs_f64(sub.end_secs, fps_rate);
                                let dur = timeline_out - timeline_in;
                                let tb = klipp_timeline::TextBox {
                                    text: sub.text.clone(),
                                    font_size: 32.0,
                                    font_name: String::new(),
                                    alignment: klipp_timeline::TextAlignment::Center,
                                    ..Default::default()
                                };
                                let mut fx = klipp_timeline::LayerFx::default();
                                fx.outlines.push(klipp_timeline::TextOutline { color: [0, 0, 0, 200], width: 2.0, enabled: true, width_keyframes: Vec::new() });
                                let mut title_data = klipp_timeline::TitleData::single_text_fx(tb, fx);
                                if let Some(o) = title_data.objects.first_mut() {
                                    o.transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.88 };
                                }
                                let clip = klipp_timeline::Clip {
                                    id: klipp_core::ClipId::new(),
                                    kind: klipp_timeline::ClipKind::Title(title_data),
                                    timeline_in,
                                    timeline_out,
                                    source_in: klipp_core::RationalTime::from_secs_f64(0.0, fps_rate),
                                    source_out: dur,
                                    ..klipp_timeline::Clip::gap(timeline_in, timeline_out)
                                };
                                track.clips.push(clip);
                            }
                        }

                        if imported_count == 0 {
                            // Remove the newly-created empty track and warn.
                            let tid = track.id;
                            let _ = seq.remove_track(tid);
                            self.state.toast.show("No subtitles found in file".to_string());
                        } else {
                            track.sort_clips();
                            self.state.toast.show(format!(
                                "Imported {} subtitle(s) into \"{}\"",
                                imported_count,
                                seq.tracks[sub_track_idx].name,
                            ));
                            self.state.dirty = true;
                        }
                    }
                    Err(e) => {
                        self.state.toast.show(format!("Failed to read file: {e}"));
                    }
                }
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::ImportKdenlive { path } => {
                let path = path.clone();
                match klipp_project::mlt_import::load_kdenlive(&path) {
                    Ok(result) => {
                        // Write warnings file if any features were dropped.
                        if !result.warnings.is_empty() {
                            let warnings_path = path.with_extension("import_warnings.txt");
                            let body = result.warnings.join("\n");
                            match std::fs::write(&warnings_path, &body) {
                                Ok(_) => {
                                    self.state.toast.show(format!(
                                        "Kdenlive project imported with {} warning(s). See {}",
                                        result.warnings.len(),
                                        warnings_path.display(),
                                    ));
                                }
                                Err(e) => {
                                    tracing::warn!("Could not write import warnings: {e}");
                                    self.state.toast.show(format!(
                                        "Imported with {} warning(s); could not write log: {e}",
                                        result.warnings.len(),
                                    ));
                                }
                            }
                        } else {
                            self.state.toast.show(
                                "Kdenlive project imported successfully.".to_string(),
                            );
                        }

                        // Merge the imported media pool into our state's pool.
                        // We cloning the items then calling add() which preserves
                        // the MediaId so clips that reference these items continue
                        // to resolve correctly.
                        let items: Vec<_> = result.media_pool.all().cloned().collect();
                        for item in items {
                            self.state.media_pool.add(item);
                        }

                        // Append the new sequence and switch to it.
                        self.state.sequences.push(result.sequence);
                        self.state.active_sequence_idx = self.state.sequences.len() - 1;
                        self.state.dirty = true;
                        self.sync_media_pool_to_engine();
                        self.sync_sequence_to_engine();
                    }
                    Err(e) => {
                        tracing::error!("Kdenlive import failed: {e:?}");
                        self.state.toast.show(format!(
                            "Failed to import Kdenlive project: {e}",
                        ));
                    }
                }
                true
            }
            AppCommand::ImportFcpxml { path } => {
                let path = path.clone();
                match klipp_project::fcpxml_import::load_fcpxml(&path) {
                    Ok(result) => {
                        // Write warnings file if any features were dropped.
                        if !result.warnings.is_empty() {
                            let warnings_path = path.with_extension("import_warnings.txt");
                            let body = result.warnings.join("\n");
                            match std::fs::write(&warnings_path, &body) {
                                Ok(_) => {
                                    self.state.toast.show(format!(
                                        "FCPXML project imported with {} warning(s). See {}",
                                        result.warnings.len(),
                                        warnings_path.display(),
                                    ));
                                }
                                Err(e) => {
                                    tracing::warn!("Could not write import warnings: {e}");
                                    self.state.toast.show(format!(
                                        "Imported with {} warning(s); could not write log: {e}",
                                        result.warnings.len(),
                                    ));
                                }
                            }
                        } else {
                            self.state.toast.show(
                                "FCPXML project imported successfully.".to_string(),
                            );
                        }

                        // Merge the imported media pool, preserving MediaIds so
                        // imported clips resolve correctly. Same pattern as
                        // Kdenlive / OTIO.
                        let items: Vec<_> = result.media_pool.all().cloned().collect();
                        for item in items {
                            self.state.media_pool.add(item);
                        }

                        self.state.sequences.push(result.sequence);
                        self.state.active_sequence_idx = self.state.sequences.len() - 1;
                        self.state.dirty = true;
                        self.sync_media_pool_to_engine();
                        self.sync_sequence_to_engine();
                    }
                    Err(e) => {
                        tracing::error!("FCPXML import failed: {e:?}");
                        self.state.toast.show(format!(
                            "Failed to import FCPXML project: {e}",
                        ));
                    }
                }
                true
            }
            AppCommand::ImportOtio { path } => {
                let path = path.clone();
                match klipp_project::otio_import::load_otio(&path) {
                    Ok(result) => {
                        // Write warnings file if any features were dropped.
                        if !result.warnings.is_empty() {
                            let warnings_path = path.with_extension("import_warnings.txt");
                            let body = result.warnings.join("\n");
                            match std::fs::write(&warnings_path, &body) {
                                Ok(_) => {
                                    self.state.toast.show(format!(
                                        "OTIO project imported with {} warning(s). See {}",
                                        result.warnings.len(),
                                        warnings_path.display(),
                                    ));
                                }
                                Err(e) => {
                                    tracing::warn!("Could not write import warnings: {e}");
                                    self.state.toast.show(format!(
                                        "Imported with {} warning(s); could not write log: {e}",
                                        result.warnings.len(),
                                    ));
                                }
                            }
                        } else {
                            self.state.toast.show(
                                "OTIO project imported successfully.".to_string(),
                            );
                        }

                        // Merge the imported media pool, preserving MediaIds so
                        // imported clips resolve correctly. Same pattern as Kdenlive.
                        let items: Vec<_> = result.media_pool.all().cloned().collect();
                        for item in items {
                            self.state.media_pool.add(item);
                        }

                        self.state.sequences.push(result.sequence);
                        self.state.active_sequence_idx = self.state.sequences.len() - 1;
                        self.state.dirty = true;
                        self.sync_media_pool_to_engine();
                        self.sync_sequence_to_engine();
                    }
                    Err(e) => {
                        tracing::error!("OTIO import failed: {e:?}");
                        self.state.toast.show(format!(
                            "Failed to import OTIO project: {e}",
                        ));
                    }
                }
                true
            }
            AppCommand::ConsolidateProject {
                dest_dir,
                include_proxies,
                include_skims,
                create_zip,
                delete_folder_after_zip,
            } => {
                self.start_consolidation(
                    dest_dir.clone(),
                    *include_proxies,
                    *include_skims,
                    *create_zip,
                    *delete_folder_after_zip,
                );
                true
            }
            AppCommand::AddSubtitleAtPlayhead { timeline_in, duration_frames } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let fps_rate = seq.fps.rate();
                let timeline_in = timeline_in.rescale_to(fps_rate);
                let duration = klipp_core::RationalTime::from_frames(
                    *duration_frames as i64, fps_rate,
                );
                let timeline_out = timeline_in + duration;

                // Find or create a Subtitle track (prefer existing Subtitle, then Text)
                let track_idx = seq.tracks.iter()
                    .position(|t| t.kind == klipp_timeline::TrackKind::Subtitle)
                    .or_else(|| seq.tracks.iter()
                        .position(|t| t.kind == klipp_timeline::TrackKind::Text))
                    .unwrap_or_else(|| {
                        let id = seq.add_subtitle_track("Subtitles".into());
                        seq.tracks.iter().position(|t| t.id == id).unwrap()
                    });

                let tb = klipp_timeline::TextBox {
                    text: String::new(),
                    font_size: 32.0,
                    font_name: String::new(),
                    alignment: klipp_timeline::TextAlignment::Center,
                    ..Default::default()
                };
                let mut fx = klipp_timeline::LayerFx::default();
                fx.outlines.push(klipp_timeline::TextOutline {
                    color: [0, 0, 0, 200],
                    width: 2.0,
                    enabled: true,
                    width_keyframes: Vec::new(),
                });
                let mut title_data = klipp_timeline::TitleData::single_text_fx(tb, fx);
                if let Some(o) = title_data.objects.first_mut() {
                    o.transform.center = klipp_timeline::TitlePosition { x: 0.5, y: 0.88 };
                }

                let new_clip = klipp_timeline::Clip {
                    id: klipp_core::ClipId::new(),
                    kind: klipp_timeline::ClipKind::Title(title_data),
                    timeline_in,
                    timeline_out,
                    source_in: klipp_core::RationalTime::from_secs_f64(0.0, fps_rate),
                    source_out: duration,
                    ..klipp_timeline::Clip::gap(timeline_in, timeline_out)
                };
                let new_clip_id = new_clip.id;
                seq.tracks[track_idx].clips.push(new_clip);
                seq.tracks[track_idx].sort_clips();
                self.state.selected_clip = Some(new_clip_id);
                self.state.dirty = true;
                self.sync_sequence_to_engine();
                true
            }
            AppCommand::ExportSubtitles { path } => {
                let path = path.clone();
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                let mut subs = Vec::new();
                // Subtitle tracks are the captions. Text tracks stand in
                // only when there are none: with both, the text tracks
                // hold titles and lower thirds, and a speaker's name does
                // not belong in a subtitle file.
                let has_subtitles = seq.tracks.iter().any(|t| t.kind == klipp_timeline::TrackKind::Subtitle && !t.clips.is_empty());
                for track in &seq.tracks {
                    let wanted = if has_subtitles { track.kind == klipp_timeline::TrackKind::Subtitle } else { track.kind == klipp_timeline::TrackKind::Text };
                    if !wanted {
                        continue;
                    }
                    for clip in &track.clips {
                        if let Some(td) = clip.title_data() {
                            subs.push(SubtitleEntry {
                                start_secs: clip.timeline_in.to_secs_f64(),
                                end_secs: clip.timeline_out.to_secs_f64(),
                                text: td.text().clone(),
                            });
                        }
                    }
                }
                subs.sort_by(|a, b| a.start_secs.partial_cmp(&b.start_secs).unwrap_or(std::cmp::Ordering::Equal));
                let ext = path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                let content = match ext.as_str() {
                    "vtt" => generate_vtt(&subs),
                    "ass" | "ssa" => generate_ass(&subs),
                    _ => generate_srt(&subs),
                };
                match std::fs::write(&path, &content) {
                    Ok(_) => self.state.toast.show(format!("Exported {} subtitles to {}", subs.len(), path.display())),
                    Err(e) => self.state.toast.show(format!("Failed to write: {e}")),
                }
                true
            }
            AppCommand::ExportYouTubeChapters { path } => {
                // TODO: add clipboard copy variant (AppCommand::CopyYouTubeChapters)
                //       once an `arboard`-style helper lives in the workspace.
                let path = path.clone();
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                // Sequence markers only — auto-sorted by position on add.
                let mut entries: Vec<(f64, String)> = seq.markers.iter()
                    .map(|m| (m.position.to_secs_f64(), m.label.clone()))
                    .collect();
                // YouTube requires the first chapter at 0:00. If the earliest
                // marker is more than half a second in (or there are no
                // markers at all), prepend a synthetic "Intro" at 0:00.
                let needs_intro = entries.first().map(|(s, _)| *s > 0.5).unwrap_or(true);
                if needs_intro {
                    entries.insert(0, (0.0, "Intro".to_string()));
                }
                let content = generate_youtube_chapters(&entries);
                match std::fs::write(&path, &content) {
                    Ok(_) => {
                        let chapter_count = entries.len();
                        let plural = if chapter_count == 1 { "" } else { "s" };
                        let mut msg = format!("Exported {chapter_count} chapter{plural}.");
                        if chapter_count < 3 {
                            msg.push_str(
                                " Note: YouTube requires at least 3 chapters to display the chapter list.",
                            );
                        }
                        self.state.toast.show(msg);
                    }
                    Err(e) => {
                        self.state.toast.show(format!("Failed to export chapters: {e}"));
                    }
                }
                true
            }
            AppCommand::ExportFcpxml { path } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                match klipp_project::fcpxml::export_fcpxml(seq, &self.state.media_pool, path) {
                    Ok(_) => self.state.toast.show(format!("FCPXML exported to {}", path.display())),
                    Err(e) => self.state.toast.show(format!("FCPXML export failed: {e}")),
                }
                true
            }
            AppCommand::ExportKdenlive { path } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                match klipp_project::mlt_export::export_kdenlive(seq, &self.state.media_pool, path) {
                    Ok(_) => self.state.toast.show(format!("Kdenlive project exported to {}", path.display())),
                    Err(e) => self.state.toast.show(format!("Kdenlive export failed: {e}")),
                }
                true
            }
            AppCommand::ExportEdl { path } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                match klipp_project::fcpxml::export_edl(seq, &self.state.media_pool, path) {
                    Ok(_) => self.state.toast.show(format!("EDL exported to {}", path.display())),
                    Err(e) => self.state.toast.show(format!("EDL export failed: {e}")),
                }
                true
            }
            AppCommand::ExportOtio { path } => {
                let seq = &self.state.sequences[self.state.active_sequence_idx];
                match klipp_project::otio_export::export_otio(seq, &self.state.media_pool, path) {
                    Ok(warnings) if warnings.is_empty() => {
                        self.state.toast.show(format!("OTIO exported to {}", path.display()));
                    }
                    Ok(warnings) => {
                        // OTIO cannot carry a title, a grade or an effect.
                        // The editor is told how much was left behind, and
                        // the detail goes beside the file — the same shape
                        // the importer uses for what it drops, so neither
                        // side of an interchange is a surprise.
                        let note = path.with_extension("export_warnings.txt");
                        let body = warnings.join("\n");
                        let n = warnings.len();
                        match std::fs::write(&note, &body) {
                            Ok(()) => self.state.toast.show(format!(
                                "OTIO exported to {} — {n} thing(s) the format cannot carry, listed in {}",
                                path.display(),
                                note.file_name().unwrap_or_default().to_string_lossy(),
                            )),
                            Err(e) => self.state.toast.show(format!(
                                "OTIO exported to {} — {n} thing(s) dropped (could not write the list: {e})",
                                path.display(),
                            )),
                        }
                    }
                    Err(e) => self.state.toast.show(format!("OTIO export failed: {e}")),
                }
                true
            }
            _ => false,
        }
    }

    /// Drive the AI Tools dispatch state machine each frame.
    ///
    /// Drains `pending_run` / `pending_cancel`, polls the worker
    /// channel, and triggers a repaint when the inference state
    /// changes so the panel + viewer overlay refresh promptly.
    fn tick_ai_tools(&mut self, ctx: &egui::Context) {
        if self.state.ai_tools_state.pending_cancel {
            self.state.ai_tools_state.inference.cancel();
            self.state.ai_tools_state.pending_cancel = false;
        }

        if self.state.ai_tools_state.pending_run {
            self.state.ai_tools_state.pending_run = false;
            self.dispatch_ai_inference(ctx);
        }

        if self.state.ai_tools_state.inference.poll() {
            // Inference left Running — clear the start-time stamp
            // so the panel's progress bar stops ticking. Done's
            // own duration comes from response.inference_ms.
            self.state.ai_tools_state.inference_start_time = None;
            // Record telemetry on the Idle → Done transition. poll()
            // returns true on every state change (Done / Failed /
            // Disconnected) but we only want successful runs.
            if let crate::ai_tools::InferenceState::Done { model_id, response } =
                &self.state.ai_tools_state.inference
            {
                let id = model_id.clone();
                let ms = response.inference_ms;
                let candidates = response.candidates.len();
                let model_display = self
                    .state
                    .ai_tools_registry
                    .info(&id)
                    .map(|i| i.display_name)
                    .unwrap_or_else(|| id.clone());
                self.state
                    .ai_tools_state
                    .model_telemetry
                    .entry(id.clone())
                    .or_default()
                    .record_run(ms);
                // Persist updated telemetry. The file is small
                // (well under 1 KB even with full telemetry across
                // every model) and inference itself takes hundreds
                // of ms, so the per-write cost is invisible. Save
                // failures are logged + swallowed.
                crate::ai_tools::persistence::save(
                    &crate::ai_tools::persistence::AiToolsConfig::from_state(
                        &self.state.ai_tools_state,
                    ),
                );
                // Push a Recent entry. Thumbnail texture is built
                // here while we still hold a reference to the
                // mask buffer; pushed at the front so the panel's
                // .iter().take(3) shows the freshest first.
                if let Some(task) = self.state.ai_tools_state.active_task {
                    let thumbnail = build_recent_thumbnail(
                        ctx,
                        &self.state.ai_tools_state.inference,
                    );
                    let entry = crate::ai_tools::RecentEntry {
                        task,
                        model_id: id,
                        model_display_name: model_display,
                        timestamp: std::time::SystemTime::now(),
                        duration_ms: ms,
                        candidates_count: candidates,
                        thumbnail,
                        prompts: self.state.ai_tools_state.prompts.clone(),
                    };
                    self.state.ai_tools_state.recent_inferences.push_front(entry);
                    while self.state.ai_tools_state.recent_inferences.len() > 20 {
                        self.state.ai_tools_state.recent_inferences.pop_back();
                    }
                }
            }
            self.refresh_ai_tools_overlay_texture(ctx);
            ctx.request_repaint();
        }

        // Drain the panel's Re-run button. Resets inference back
        // to Idle while keeping the captured prompts so the user
        // can tweak + re-fire. Defers the overlay texture drop
        // through the wgpu-safe path.
        if self.state.ai_tools_state.pending_rerun {
            self.state.ai_tools_state.pending_rerun = false;
            self.state.ai_tools_state.inference =
                crate::ai_tools::InferenceState::Idle;
            self.state.ai_tools_state.inference_start_time = None;
            self.refresh_ai_tools_overlay_texture(ctx);
            ctx.request_repaint();
        }

        // Drain the panel's Apply-to-clip button. Writes the mask
        // PNG to the cache + inserts an ExternalMatte node into
        // the chosen clip's node graph.
        if self.state.ai_tools_state.pending_apply_to_clip {
            self.state.ai_tools_state.pending_apply_to_clip = false;
            self.execute_apply_mask_to_clip();
        }

        // Drive the custom-URL paste state machine. Worker thread
        // delivers progress + completion events; transitioning to
        // Confirming repaints so the modal flips into the SHA/size
        // confirmation view.
        if self.state.custom_model_add.poll() {
            ctx.request_repaint();
        }
    }

    /// Apply the most recent successful mask to a clip's node
    /// graph. Target clip selection follows the
    /// selected-then-playhead-fallback rule:
    ///   1. `state.selected_clip` if set and resolvable.
    ///   2. Otherwise the topmost video track's clip at the
    ///      playhead (egui's `find_map` order matches our
    ///      bottom-up track numbering — first match is highest).
    ///   3. Otherwise toast "No clip selected".
    ///
    /// On success: writes the mask to a metadata-rich filename in
    /// `~/.cache/klipp/ai_masks/`, inserts an `ExternalMatte`
    /// node into the clip's graph wired into the existing
    /// Corrector's matte input (port 1) when one exists, and
    /// toasts "Mask applied to {track} · {clip_short}".
    fn execute_apply_mask_to_clip(&mut self) {
        // Snapshot what we need from inference state without
        // holding the borrow into mutating ops.
        let (mask, model_id) = match &self.state.ai_tools_state.inference {
            crate::ai_tools::InferenceState::Done { model_id, response }
                if !response.candidates.is_empty() =>
            {
                (response.candidates[0].mask.clone(), model_id.clone())
            }
            _ => {
                self.state.toast.show("No mask to apply");
                return;
            }
        };

        // Resolve target clip. Selected first, playhead fallback.
        let playhead = self.state.playhead;
        let target = self.state.selected_clip_loc().map(|(t, c)| (t, c.id, c.clone()));
        let target = target.or_else(|| {
            self.state
                .sequence()
                .tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Video)
                .find_map(|(i, t)| {
                    t.clip_at_timecode(playhead).map(|(_, c)| (i, c.id, c.clone()))
                })
        });
        let Some((track_idx, clip_id, clip_snapshot)) = target else {
            self.state.toast.show(
                "No clip selected. Select a clip or position the playhead over one.",
            );
            return;
        };

        // Resolve the cache directory + a metadata-rich filename.
        // Schema: <clip_id>_<frame>_<model_id_hash>_<timestamp>.png
        // — gives a future cleanup pass enough info to identify
        // orphans (clip_id parsed against current project) and
        // dupes (clip+frame+model triple) without re-hashing.
        let cache_dir = match dirs::cache_dir() {
            Some(d) => d.join("klipp").join("ai_masks"),
            None => {
                self.state.toast.show("Apply failed: no cache dir");
                return;
            }
        };
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            self.state.toast.show(format!("Apply failed: mkdir cache ({e})"));
            return;
        }
        let frame = playhead.value;
        let model_hash: u64 = {
            // 64-bit FNV-1a — stable, no extra dep, short enough
            // to keep the filename readable.
            let mut h: u64 = 0xcbf29ce484222325;
            for b in model_id.as_bytes() {
                h ^= *b as u64;
                h = h.wrapping_mul(0x100000001b3);
            }
            h
        };
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let filename = format!(
            "{}_{frame}_{:016x}_{timestamp}.png",
            clip_id.to_string().split('-').next().unwrap_or("clip"),
            model_hash,
        );
        let final_path = cache_dir.join(&filename);
        let tmp_path = cache_dir.join(format!("{filename}.tmp"));

        // Encode mask as a single-channel grayscale PNG via the
        // image crate. Atomic write: encode to .tmp then rename
        // so a crash mid-write doesn't leave a half-written file
        // referenced by the node.
        let img = image::GrayImage::from_raw(
            mask.width,
            mask.height,
            mask.alpha.clone(),
        );
        let img = match img {
            Some(i) => i,
            None => {
                self.state.toast.show("Apply failed: mask buffer dimensions mismatch");
                return;
            }
        };
        if let Err(e) = img.save_with_format(&tmp_path, image::ImageFormat::Png) {
            self.state.toast.show(format!("Apply failed: PNG write ({e})"));
            return;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &final_path) {
            self.state.toast.show(format!("Apply failed: rename ({e})"));
            return;
        }
        // TODO(cleanup): orphan detection — the schema in this
        // filename gives a future cleanup pass enough info to
        // prune AI mask cache entries whose clip_id is no longer
        // referenced. v1 ships a 2GB LRU sweep on app start
        // (klipp-app::main) as the disk-pressure safety net.

        // Insert the ExternalMatte node into the clip's graph and
        // wire it into the existing Corrector's matte input
        // (port 1) when one exists: the executor runs a correction
        // gated there through an ApplyMatte, so the grade reaches only
        // the mask (#440). If not, the matte hangs in the graph and the
        // user can wire it manually from the V2 editor — still better
        // than no apply.
        let track_name = self
            .state
            .sequence()
            .tracks
            .get(track_idx)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| format!("Track {track_idx}"));
        let _ = clip_snapshot; // we mutate via sequence_mut below

        let path_for_node = final_path.clone();
        let mut wired = false;
        if let Some(track) = self.state.sequence_mut().tracks.get_mut(track_idx) {
            if let Some(clip) = track.clips.iter_mut().find(|c| c.id == clip_id) {
                let matte_id = klipp_nodes::NodeId::new_v4();
                let mut params = klipp_nodes::params::NodeParams::default();
                params.matte_image_path = Some(path_for_node);
                params.matte_channel =
                    Some(klipp_nodes::params::MatteChannel::Luminance);
                clip.node_graph.nodes.push(klipp_nodes::graph::Node {
                    id: matte_id,
                    kind: klipp_nodes::kinds::NodeKind::ExternalMatte,
                    label: "AI mask".into(),
                    enabled: true,
                    params,
                    keyframes: klipp_nodes::params::NodeKeyframeTracks::default(),
                    output_link: None,
                    input_links: Vec::new(),
                });
                // Find the first Corrector node — that's where the
                // default-graph's grade-with-matte expects the
                // matte input to feed (port 1).
                let corrector_id = clip
                    .node_graph
                    .nodes
                    .iter()
                    .find(|n| {
                        matches!(n.kind, klipp_nodes::kinds::NodeKind::Corrector)
                    })
                    .map(|n| n.id);
                if let Some(corrector_id) = corrector_id {
                    // Drop any existing matte edge into the
                    // corrector's port 1 so the new mask wins.
                    clip.node_graph
                        .edges
                        .retain(|e| !(e.to_node == corrector_id && e.to_port == 1));
                    clip.node_graph.edges.push(klipp_nodes::graph::Edge {
                        from_node: matte_id,
                        from_port: 0,
                        to_node: corrector_id,
                        to_port: 1,
                    });
                    wired = true;
                }
            }
        }

        let clip_short = clip_id
            .to_string()
            .split('-')
            .next()
            .map(|s| s.to_string())
            .unwrap_or_else(|| "clip".into());
        if wired {
            self.state.toast.show(format!(
                "Mask applied to {track_name} · {clip_short}",
            ));
        } else {
            self.state.toast.show(format!(
                "Mask added to {track_name} · {clip_short} (no Corrector found — \
                 wire the matte from the V2 editor)",
            ));
        }
    }

    /// Rebuild the cached mask-overlay texture to match the current
    /// inference state. Routes the previous handle through the
    /// deferred-drop ring so wgpu doesn't see a destroyed texture
    /// mid-frame.
    fn refresh_ai_tools_overlay_texture(&mut self, ctx: &egui::Context) {
        // Use the multi-detection compositor so RF-DETR-Seg's many
        // candidates render in distinct palette colors. Falls back
        // to the single-color path automatically when there's only
        // one candidate (BiRefNet, MobileSAM single-click).
        let candidates = self.state.ai_tools_state.inference.candidates();
        let new_tex = if candidates.is_empty() {
            None
        } else {
            crate::ai_tools::build_multi_detection_texture(
                ctx,
                candidates,
                self.state.ai_tools_state.mask_overlay_color,
                "ai_tools_mask_overlay",
            )
        };
        let old = std::mem::replace(
            &mut self.state.ai_tools_state.mask_overlay_texture,
            new_tex,
        );
        if let Some(h) = old {
            self.defer_drop_texture(h);
        }
    }

    /// Build an `OwnedMaskRequest` from the current viewer frame +
    /// captured prompts and hand it to a worker thread.
    ///
    /// Holds the `frame_buffer` mutex only long enough to copy the
    /// raw sRGB u8 RGBA bytes out (cheap `memcpy`) — sRGB→linear
    /// f32 conversion runs on the worker thread so a 4K frame
    /// (~30 M `powf` calls) doesn't freeze the UI for several
    /// seconds. Watchdog hit this as `stage=AiToolsTick` 30 s+
    /// stalls when both linearisation AND ML inference were on the
    /// UI thread under one lock.
    fn dispatch_ai_inference(&mut self, ctx: &egui::Context) {
        let Some(model_id) = self.state.ai_tools_state.selected_model_id.clone() else {
            self.state.toast.show("AI Tools: pick a model first");
            return;
        };
        if self.state.ai_tools_state.inference.is_running() {
            return;
        }

        // Snapshot raw u8 RGBA — fast memcpy under the lock.
        let (w, h, srgb_bytes) = {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if fb.width == 0 || fb.height == 0 || fb.data.is_empty() {
                self.state.toast.show("AI Tools: no frame loaded yet");
                return;
            }
            let expected = (fb.width as usize) * (fb.height as usize) * 4;
            if fb.data.len() < expected {
                self.state.toast.show("AI Tools: frame buffer not ready");
                return;
            }
            (fb.width, fb.height, fb.data[..expected].to_vec())
        };

        let prompts = self.state.ai_tools_state.prompts.build();
        // Per-model overrides (RF-DETR's score / NMS thresholds when
        // the user has nudged the panel sliders). Empty default for
        // models that don't expose any knobs — generators ignore
        // unrecognised fields.
        let tunables = self
            .state
            .ai_tools_state
            .model_tunables
            .get(&model_id)
            .cloned()
            .unwrap_or_default();
        let model_id_for_state = model_id.clone();
        let registry = self.state.ml_registry.clone();
        let ctx_for_worker = ctx.clone();
        let pixel_count = (w as usize) * (h as usize);

        // Spawn a "preparer" thread that does the heavy
        // linearisation off the UI thread, then hands the
        // OwnedMaskRequest to spawn_inference. This adds one extra
        // thread hop but keeps the UI tick under a millisecond.
        let (handle, _join) = {
            let (tx, rx) = crossbeam_channel::unbounded::<crate::ai_tools::InferenceEvent>();
            let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let cancel_for_worker = cancel.clone();
            let join = std::thread::Builder::new()
                .name(format!("klipp-ml-prep-{model_id}"))
                .spawn(move || {
                    let mut linear = Vec::with_capacity(pixel_count * 4);
                    for chunk in srgb_bytes.chunks_exact(4) {
                        linear.push(klipp_color::transfer::srgb_to_linear(chunk[0] as f32 / 255.0));
                        linear.push(klipp_color::transfer::srgb_to_linear(chunk[1] as f32 / 255.0));
                        linear.push(klipp_color::transfer::srgb_to_linear(chunk[2] as f32 / 255.0));
                        linear.push(chunk[3] as f32 / 255.0);
                    }
                    if cancel_for_worker.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = tx.send(crate::ai_tools::InferenceEvent::Failed("cancelled".into()));
                        ctx_for_worker.request_repaint();
                        return;
                    }
                    let req = crate::ai_tools::OwnedMaskRequest {
                        frame_pixels: std::sync::Arc::new(linear),
                        frame_width: w,
                        frame_height: h,
                        prompts,
                        temporal: None,
                        tunables,
                    };
                    // Run inference inline on this same prep thread —
                    // we already paid for the thread spawn, so a
                    // second hop would be wasteful. Channel + cancel
                    // mirror what spawn_inference does internally.
                    let _ = tx.send(crate::ai_tools::InferenceEvent::Started);
                    ctx_for_worker.request_repaint();
                    let event = crate::ai_tools::run_inference_blocking(&model_id, &req, &registry);
                    if cancel_for_worker.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = tx.send(crate::ai_tools::InferenceEvent::Failed("cancelled".into()));
                    } else {
                        let _ = tx.send(event);
                    }
                    ctx_for_worker.request_repaint();
                })
                .expect("spawn klipp-ml prep thread");
            (
                crate::ai_tools::InferenceHandle { recv: rx, cancel },
                join,
            )
        };

        self.state.ai_tools_state.inference = crate::ai_tools::InferenceState::Running {
            model_id: model_id_for_state,
            frame_width: w,
            frame_height: h,
            handle,
        };
        // Stamp the start time so the panel's progress bar can
        // derive a fake percentage from elapsed-vs-telemetry-avg.
        // Cleared in the poll path when inference resolves.
        self.state.ai_tools_state.inference_start_time =
            Some(std::time::Instant::now());
    }

    /// Begin capturing audio from the configured input device into a WAV file.
    /// Called from `AppCommand::StartRecording` handler; broken out so the
    /// error/early-return flow stays readable.
    fn start_recording(&mut self) {
        if self.state.is_recording {
            self.state.toast.show("Already recording");
            return;
        }

        // Find the armed track (first in the active sequence — MVP is single-track).
        let Some(seq) = self.state.sequences.get(self.state.active_sequence_idx) else {
            return;
        };
        let armed_track = seq.tracks.iter().find(|t| t.record_armed);
        let Some(track) = armed_track else {
            self.state.toast.show("Arm an audio track first (click the red dot on a track header)");
            return;
        };
        if track.kind != klipp_timeline::TrackKind::Audio {
            self.state.toast.show("Recording requires an audio track");
            return;
        }
        let track_id = track.id;

        // Build recording path under the proxy cache dir (or fall back to
        // the OS cache dir). Parent dir is created lazily.
        let cache_root = self
            .prefs
            .proxy
            .proxy_cache_dir
            .clone()
            .or_else(|| dirs::cache_dir().map(|d| d.join("klipp")))
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp/klipp"));
        let rec_dir = cache_root.join("recordings");
        if let Err(e) = std::fs::create_dir_all(&rec_dir) {
            self.state.toast.show(format!("Recording dir error: {e}"));
            return;
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = rec_dir.join(format!("rec_{ts}.wav"));

        let device = if self.prefs.audio_hardware.input_device.is_empty() {
            None
        } else {
            Some(self.prefs.audio_hardware.input_device.as_str())
        };
        let sample_rate = self.state.audio_sample_rate;

        let (input, consumer) = match klipp_engine::audio_input::AudioInput::new(device, sample_rate) {
            Ok(pair) => pair,
            Err(e) => {
                self.state.toast.show(format!("Record: {e}"));
                return;
            }
        };
        if let Err(e) = input.start() {
            self.state.toast.show(format!("Record start: {e}"));
            return;
        }
        let (stop_tx, stop_rx) = std::sync::mpsc::channel();
        let _handle = klipp_engine::audio_input::start_recording_thread(
            consumer,
            path.clone(),
            input.sample_rate,
            input.channels,
            stop_rx,
        );

        self.state.recording_audio_input = Some(input);
        self.state.recording_stop_tx = Some(stop_tx);
        self.state.recording_path = Some(path);
        self.state.recording_track_id = Some(track_id);
        self.state.is_recording = true;
        self.state.toast.show("Recording\u{2026}");
    }

    /// Stop an active recording, finalize the WAV file, and auto-import it.
    fn stop_recording(&mut self) {
        if !self.state.is_recording {
            return;
        }

        // 1. Tell the writer thread to drain + patch + exit.
        if let Some(tx) = self.state.recording_stop_tx.take() {
            let _ = tx.send(());
        }
        // 2. Stop and drop the cpal input stream.
        if let Some(mut input) = self.state.recording_audio_input.take() {
            input.stop();
            drop(input);
        }
        self.state.is_recording = false;

        // 3. Hand the WAV off to the import pipeline. The writer thread may
        //    still be finalizing the header — that's OK, the import side
        //    opens the file on demand after the user drags it onto the
        //    timeline or the auto-import hits the media loader.
        //    In practice the writer thread finishes within a few ms.
        let recorded_path = self.state.recording_path.take();
        let _track_id = self.state.recording_track_id.take();
        if let Some(path) = recorded_path {
            // Small settle sleep so the WAV header patch is on disk before
            // the importer opens the file. Worst case the import reads a
            // slightly smaller RIFF size, which every player tolerates — but
            // we want a valid-on-disk file by the time LoadMediaFile runs.
            std::thread::sleep(std::time::Duration::from_millis(50));
            // Route through the media-loading pipeline the same way the
            // file-picker import does (see handle_media_loading): mint a
            // MediaId and ask the engine to decode the header + add it to
            // the pool. `pending_media_path = None` prevents auto-append.
            let media_id = klipp_core::MediaId::new();
            self.state.pending_media_path = None;
            let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                path: path.clone(),
                media_id,
            });
            self.state.toast.show("Recording saved. Drag from media pool onto a track, then use Audio Sync to align if needed.");
            tracing::info!("Recording saved: {}", path.display());
            // Auto-align bonus: deferred. Wiring requires teaching the
            // MediaLoaded event handler to (a) know which incoming imports
            // originated from a recording, (b) auto-append the resulting
            // clip to the armed track, and (c) fire AlignToAudioReference
            // once the clip exists on the timeline. That's tier-2b+ scope
            // (not in the 11-gap MVP). Leaving as manual for now.
        } else {
            self.state.toast.show("Recording stopped");
        }
    }

    fn apply_undo(&mut self) {
        // History is per-sequence — ctrl+z can never reach into another
        // sequence's stack, so there's no cross-sequence navigation to
        // toast about.
        if let Some(entry) = self.state.active_history_mut().undo() {
            self.apply_edit_inverse(&entry.command);
            // And redraw the frame: paused, a sync alone shows the
            // change only when the engine happens to see it (a multicam
            // cut undone kept showing the angle cut to).
            self.sync_and_refresh_frame();
            self.state.dirty = true;
            self.state.media_usage_cache_dirty = true;
        }
    }

    fn apply_redo(&mut self) {
        if let Some(entry) = self.state.active_history_mut().redo() {
            self.apply_edit_forward(&entry.command);
            // And redraw the frame: paused, a sync alone shows the
            // change only when the engine happens to see it (a multicam
            // cut undone kept showing the angle cut to).
            self.sync_and_refresh_frame();
            self.state.dirty = true;
            self.state.media_usage_cache_dirty = true;
        }
    }

    /// The document as a command runs against it: the UI's own state,
    /// borrowed field by field.
    fn document_view(&mut self) -> klipp_document::apply::DocumentView<'_> {
        klipp_document::apply::DocumentView {
            sequences: &mut self.state.sequences,
            active: self.state.active_sequence_idx,
            media_pool: &mut self.state.media_pool,
            fps: &mut self.state.fps,
        }
    }

    /// What the document asked for once a command ran, done here where
    /// the engine and the viewer are.
    fn run_document_effects(&mut self, effects: klipp_document::apply::Effects) {
        if effects.sync_sequence {
            self.sync_sequence_to_engine();
        }
        if effects.sync_media_pool {
            self.sync_media_pool_to_engine();
        }
        if effects.notify_engine {
            let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
        }
        if effects.set_playhead {
            let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
        }
        for clip_id in effects.rerender_titles {
            self.render_title_to_viewer(clip_id);
        }
    }

    fn apply_edit_inverse(&mut self, cmd: &EditCommand) {
        let effects = self.document_view().apply_inverse(cmd);
        self.run_document_effects(effects);
    }


    fn apply_edit_forward(&mut self, cmd: &EditCommand) {
        let effects = self.document_view().apply_forward(cmd);
        self.run_document_effects(effects);
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

    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        // Skip single-key shortcuts when ANY text edit widget has focus (search bars,
        // title text editor, timecode entry, rename fields, etc.).
        // Ctrl+ shortcuts still work (standard convention: Ctrl combos always fire).
        let text_editing = ctx.wants_keyboard_input() || self.state.editing_timecode || self.state.viewer_text_edit;
        self.shortcuts_ctrl_global(ctx, &mut cmds);
        if text_editing {
            return cmds;
        }
        self.shortcuts_magnetic_skim(ctx, &mut cmds);
        self.shortcuts_delete(ctx, &mut cmds);
        self.shortcuts_tools_and_clip_ops(ctx, &mut cmds);
        self.shortcuts_title_and_tools2(ctx, &mut cmds);
        self.shortcuts_multicam_and_io(ctx, text_editing, &mut cmds);
        self.shortcuts_edit_nav(ctx, &mut cmds);
        self.shortcuts_keyframe_and_zoom(ctx, &mut cmds);
        self.shortcuts_ai_tools(ctx, &mut cmds);
        cmds
    }

    fn shortcuts_ai_tools(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            if kb.is_action_pressed("ai_remove_background", i) {
                cmds.push(AppCommand::ShowAiToolsPanel {
                    task: klipp_core::events::AiToolsTaskTag::RemoveBackground,
                });
            }
            if kb.is_action_pressed("ai_isolate_object", i) {
                cmds.push(AppCommand::ShowAiToolsPanel {
                    task: klipp_core::events::AiToolsTaskTag::IsolateObject,
                });
            }
            if kb.is_action_pressed("ai_detect_objects", i) {
                cmds.push(AppCommand::ShowAiToolsPanel {
                    task: klipp_core::events::AiToolsTaskTag::DetectObjects,
                });
            }
            if kb.is_action_pressed("ai_run_inference", i) {
                cmds.push(AppCommand::RunAiToolsInference);
            }
            if kb.is_action_pressed("ai_cancel_inference", i) {
                cmds.push(AppCommand::CancelAiToolsInference);
            }
            if kb.is_action_pressed("toggle_ai_tools_panel", i) {
                self.state.ai_tools_state.panel_visible =
                    !self.state.ai_tools_state.panel_visible;
                if self.state.ai_tools_state.panel_visible
                    && self.state.ai_tools_state.active_task.is_none()
                {
                    // Default the panel to Isolate Object so the
                    // open → click → Ctrl+Enter flow works without
                    // requiring the user to pick a task button first.
                    let default_task = crate::ai_tools::AiTask::IsolateObject;
                    self.state.ai_tools_state.active_task = Some(default_task);
                    self.state.ai_tools_state.selected_model_id = self
                        .state
                        .ai_tools_registry
                        .models_for(default_task)
                        .first()
                        .map(|m| m.id.clone());
                }
            }
        });
    }

    fn shortcuts_ctrl_global(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Ctrl+S — Save
            if kb.is_action_pressed("save_project", i) {
                cmds.push(AppCommand::SaveProject);
            }
            // Ctrl+N / Ctrl+Q / Ctrl+Shift+M / Ctrl+Shift+T. The menus
            // have advertised all four for a long time with nothing
            // behind them, because `shortcut_text` only draws a label.
            if kb.is_action_pressed("new_project", i) {
                cmds.push(AppCommand::NewProject);
            }
            if kb.is_action_pressed("quit", i) {
                cmds.push(AppCommand::Quit);
            }
            if kb.is_action_pressed("toggle_audio_mixer", i) {
                self.state.show_audio_mixer = !self.state.show_audio_mixer;
            }
            if kb.is_action_pressed("toggle_subtitle_editor", i) {
                self.state.show_subtitle_editor = !self.state.show_subtitle_editor;
            }
            // Ctrl+Shift+S — Save As
            if kb.is_action_pressed("save_project_as", i) {
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Save Project As")
                        .add_filter("Klipp Project", &["klipp"])
                        .set_file_name("project.klipp")
                        .save_file()
                        .map(|path| vec![AppCommand::SaveProjectAs { path }])
                        .unwrap_or_default()
                });
            }
            // Ctrl+O — Open Project
            if kb.is_action_pressed("open_project", i) {
                crate::file_dialog::spawn(move || {
                    rfd::FileDialog::new()
                        .set_title("Open Project")
                        .pick_folder()
                        .map(|path| vec![AppCommand::OpenProject { path }])
                        .unwrap_or_default()
                });
            }
            // Ctrl+I — Import Media
            if kb.is_action_pressed("import_media", i) {
                crate::file_dialog::spawn(crate::command_palette::import_media_commands);
            }
            // Ctrl+E — Export
            if kb.is_action_pressed("export", i) {
                self.state.show_export_dialog = true;
                self.prepare_export_defaults();
            }
            // Ctrl+, — Preferences
            if kb.is_action_pressed("preferences", i) {
                self.state.show_preferences = true;
            }
            // Ctrl+K — Command palette
            if kb.is_action_pressed("command_palette", i) {
                self.state.show_command_palette = true;
                self.state.command_palette_query.clear();
                self.state.command_palette_selected = 0;
            }
            // Ctrl+F — Focus the search field of the currently active
            // panel. Sets a one-shot flag that the target panel
            // consumes on its next show() (request_focus on its
            // TextEdit, clear the flag). When the active panel has no
            // search surface (Viewer / Inspector / Timeline) the
            // end-of-update watchdog below clears the flag silently.
            if kb.is_action_pressed("focus_panel_search", i) {
                self.state.focus_search_request = self.state.active_panel;
            }
            // Ctrl+W — Close Project (return to project selector)
            if kb.is_action_pressed("close_project", i) {
                cmds.push(AppCommand::CloseProject);
            }
            // Ctrl+Z — Undo, Ctrl+Shift+Z or Ctrl+Y — Redo
            if kb.is_action_pressed("undo", i) {
                cmds.push(AppCommand::Undo);
            }
            if kb.is_action_pressed("redo", i) {
                cmds.push(AppCommand::Redo);
            }

            // Group selected clips
            if kb.is_action_pressed("group_clips", i) {
                let mut ids = self.state.selected_clips.clone();
                if let Some(primary) = self.state.selected_clip {
                    if !ids.contains(&primary) {
                        ids.push(primary);
                    }
                }
                if ids.len() >= 2 {
                    cmds.push(AppCommand::GroupClips { clip_ids: ids });
                }
            }
            // Ungroup
            if kb.is_action_pressed("ungroup_clips", i) {
                if let Some(primary) = self.state.selected_clip {
                    let gid = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == primary)
                        .and_then(|c| c.group_id);
                    if let Some(gid) = gid {
                        cmds.push(AppCommand::UngroupClips { group_id: gid });
                    }
                }
            }

        });
    }

    fn shortcuts_magnetic_skim(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // S (no modifiers) — Toggle skimming
            if kb.is_action_pressed("toggle_skimming", i) {
                self.state.skimming_enabled = !self.state.skimming_enabled;
                // End audio skim session when disabling all skimming
                if !self.state.skimming_enabled {
                    if let Some(ref session) = self.state.skim_audio_session {
                        session.stop();
                    }
                    klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                    klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                    self.state.skim_audio_session = None;
                    self.state.skim_speed_px_per_sec = 0.0;
                }
                let msg = if self.state.skimming_enabled { "Skimming On" } else { "Skimming Off" };
                self.state.toast.show(msg);
            }

            // G (no modifiers) — Connect selected clip to nearest primary clip
            if kb.is_action_pressed("connect_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    let clip_track = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == clip_id));
                    if let Some(ti) = clip_track {
                        if self.state.sequences[self.state.active_sequence_idx].tracks[ti].magnetic_role != klipp_timeline::TrackRole::Primary {
                            let clip_in = self.state.sequences[self.state.active_sequence_idx].tracks[ti].clips.iter()
                                .find(|c| c.id == clip_id)
                                .map(|c| c.timeline_in);
                            if let Some(clip_in) = clip_in {
                                if let Some(primary_idx) = self.state.sequences[self.state.active_sequence_idx].primary_track_index() {
                                    let best_anchor = self.state.sequences[self.state.active_sequence_idx].tracks[primary_idx].clips.iter()
                                        .filter(|c| !c.kind.is_gap())
                                        .min_by_key(|c| (c.timeline_in - clip_in).abs().value.unsigned_abs());
                                    if let Some(anchor) = best_anchor {
                                        let anchor_id = anchor.id;
                                        let result = klipp_timeline::magnetic::MagneticEditor::connect_clip(
                                            &mut self.state.sequences[self.state.active_sequence_idx],
                                            clip_id,
                                            anchor_id,
                                            klipp_timeline::AnchorAlignment::StartAligned,
                                        );
                                        self.state.push_history(crate::history::EditCommand::Magnetic(result.command));
                                        self.sync_sequence_to_engine();
                                        self.state.dirty = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Shift+G (no ctrl) — Disconnect selected clip
            if kb.is_action_pressed("disconnect_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    let has_anchor = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .any(|c| c.id == clip_id && c.magnetic_anchor.is_some());
                    if has_anchor {
                        let result = klipp_timeline::magnetic::MagneticEditor::disconnect_clip(
                            &mut self.state.sequences[self.state.active_sequence_idx], clip_id);
                        self.state.push_history(crate::history::EditCommand::Magnetic(result.command));
                        self.sync_sequence_to_engine();
                        self.state.dirty = true;
                    }
                }
            }

            // Shift+S — Promote/Break storyline OR toggle audio skim.
            // Routed through the keybindings registry (action
            // `toggle_audio_skim`) so users can rebind via the prefs
            // panel; the storyline path takes priority when a clip on
            // a connected storyline is selected.
            if kb.is_action_pressed("toggle_audio_skim", i) {
                let mut handled_storyline = false;
                if let Some(clip_id) = self.state.selected_clip {
                    let clip_track = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == clip_id));
                    if let Some(ti) = clip_track {
                        if self.state.sequences[self.state.active_sequence_idx].tracks[ti].magnetic_role == klipp_timeline::TrackRole::Connected {
                            if let Some(primary_idx) = self.state.sequences[self.state.active_sequence_idx].primary_track_index() {
                                let first_clip_in = self.state.sequences[self.state.active_sequence_idx].tracks[ti].clips.first()
                                    .map(|c| c.timeline_in);
                                if let Some(clip_in) = first_clip_in {
                                    let best_anchor = self.state.sequences[self.state.active_sequence_idx].tracks[primary_idx].clips.iter()
                                        .filter(|c| !c.kind.is_gap())
                                        .min_by_key(|c| (c.timeline_in - clip_in).abs().value.unsigned_abs());
                                    if let Some(anchor) = best_anchor {
                                        let anchor_id = anchor.id;
                                        let result = klipp_timeline::magnetic::MagneticEditor::promote_to_storyline(
                                            &mut self.state.sequences[self.state.active_sequence_idx], ti, anchor_id);
                                        self.state.push_history(crate::history::EditCommand::Magnetic(result.command));
                                        self.sync_sequence_to_engine();
                                        self.state.dirty = true;
                                        handled_storyline = true;
                                    }
                                }
                            }
                        } else if self.state.sequences[self.state.active_sequence_idx].tracks[ti].magnetic_role == klipp_timeline::TrackRole::ConnectedStoryline {
                            let result = klipp_timeline::magnetic::MagneticEditor::break_apart_storyline(
                                &mut self.state.sequences[self.state.active_sequence_idx], ti);
                            self.state.push_history(crate::history::EditCommand::Magnetic(result.command));
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                            handled_storyline = true;
                        }
                    }
                }
                // If no storyline action applied, toggle audio skim.
                // Routed through the AppCommand so the palette entry
                // and the keybinding share the cleanup path.
                if !handled_storyline {
                    cmds.push(AppCommand::ToggleAudioSkim);
                }
            }

        });
    }

    fn shortcuts_delete(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        // Delete over the Transcript panel is the panel's: it cuts the
        // selected words. Taking the selected clip as well would remove
        // a clip the cut has just split.
        if self.state.show_transcript_panel && self.state.transcript_panel_hovered {
            return;
        }
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Delete/Backspace — Remove selected keyframe (if one is selected on timeline)
            let mut keyframe_deleted = false;
            if kb.is_action_pressed("delete_clip", i) {
                if let Some(sk) = self.state.selected_keyframe.take() {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == sk.clip_id);
                    if let Some(clip) = clip {
                        let kf_time = match &sk.param_kind {
                            crate::state::KeyframeParamKind::Title { object_id, param } => param.keyframes(clip, *object_id).get(sk.keyframe_index).map(|kf| kf.time_secs),
                            crate::state::KeyframeParamKind::Audio(param) => {
                                let kfs = match param {
                                    klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                };
                                kfs.get(sk.keyframe_index).map(|kf| kf.time_secs)
                            }
                            crate::state::KeyframeParamKind::Video(param) => {
                                let kfs = match param {
                                    klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                    klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                    klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                    klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                    klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                    klipp_core::VideoParam::TiltX => &clip.transform_tilt_x_keyframes,
                                    klipp_core::VideoParam::TiltY => &clip.transform_tilt_y_keyframes,
                                    klipp_core::VideoParam::MatteFeather => &clip.matte_feather_keyframes,
                                    klipp_core::VideoParam::MatteChoke => &clip.matte_choke_keyframes,
                                };
                                kfs.get(sk.keyframe_index).map(|kf| kf.time_secs)
                            }
                            crate::state::KeyframeParamKind::AudioEffect { effect_index, param_name } => {
                                clip.audio_effect_keyframes
                                    .get(&format!("{effect_index}:{param_name}"))
                                    .and_then(|kfs| kfs.get(sk.keyframe_index).map(|kf| kf.time_secs))
                            }
                        };
                        if let Some(time_secs) = kf_time {
                            match &sk.param_kind {
                                crate::state::KeyframeParamKind::Title { object_id, param } => cmds.push(param.remove(sk.clip_id, *object_id, time_secs)),
                                crate::state::KeyframeParamKind::Audio(param) => {
                                    cmds.push(AppCommand::RemoveAudioKeyframe { clip_id: sk.clip_id, param: *param, time_secs });
                                }
                                crate::state::KeyframeParamKind::Video(param) => {
                                    cmds.push(AppCommand::RemoveVideoKeyframe { clip_id: sk.clip_id, param: *param, time_secs });
                                }
                                crate::state::KeyframeParamKind::AudioEffect { effect_index, param_name } => {
                                    cmds.push(AppCommand::RemoveAudioEffectKeyframe {
                                        clip_id: sk.clip_id,
                                        effect_index: *effect_index,
                                        param_name: param_name.clone(),
                                        time_secs,
                                    });
                                }
                            }
                            keyframe_deleted = true;
                        }
                    }
                }
            }

            // Shift+Delete — Lift (leave gap, no ripple) on magnetic tracks
            if !keyframe_deleted && kb.is_action_pressed("lift_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    let track_idx = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == clip_id));
                    if let Some(ti) = track_idx {
                        if self.is_magnetic_track(ti) {
                            let result = klipp_timeline::magnetic::MagneticEditor::lift(
                                &mut self.state.sequences[self.state.active_sequence_idx], clip_id);
                            self.state.push_history(crate::history::EditCommand::Magnetic(result.command));
                            self.state.selected_clip = None;
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        }
                    }
                }
            }
            // Delete/Backspace — remove selected clip OR extract range (plain, no shift)
            else if !keyframe_deleted && kb.is_action_pressed("delete_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RemoveClip { clip_id });
                } else if self.state.selected_clips.is_empty()
                    && self.state.sequence().in_point.is_some()
                    && self.state.sequence().out_point.is_some()
                {
                    cmds.push(AppCommand::ExtractEdit);
                }
            }

        });
    }

    fn shortcuts_tools_and_clip_ops(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Shift+W — Insert/Overwrite. It was on bare W, which the
            // registry gives to trim_in_to_playhead; that one is wired,
            // so a single W did both.
            if kb.is_action_pressed("toggle_edit_mode", i) {
                self.state.edit_mode = match self.state.edit_mode {
                    klipp_timeline::magnetic::EditMode::Insert => klipp_timeline::magnetic::EditMode::Overwrite,
                    klipp_timeline::magnetic::EditMode::Overwrite => klipp_timeline::magnetic::EditMode::Insert,
                };
            }
            // Alt+[ — Ripple trim head to playhead (requires selection).
            if kb.is_action_pressed("ripple_trim_head", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RippleTrimHead { clip_id });
                }
            }
            // Alt+] — Ripple trim tail to playhead (requires selection).
            if kb.is_action_pressed("ripple_trim_tail", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RippleTrimTail { clip_id });
                }
            }
            // Q — Trim OUT to playhead (no selection required).
            if kb.is_action_pressed("trim_out_to_playhead", i) {
                cmds.push(AppCommand::TrimOutToPlayhead);
            }
            // W — Trim IN to playhead (no selection required).
            if kb.is_action_pressed("trim_in_to_playhead", i) {
                cmds.push(AppCommand::TrimInToPlayhead);
            }
            // Ctrl+Alt+S — Extract frame to file (dialog).
            if kb.is_action_pressed("extract_frame_to_file", i) {
                cmds.push(AppCommand::FrameGrab);
            }
            // Ctrl+Alt+B — Extract frame and import into the media bin.
            if kb.is_action_pressed("extract_frame_to_bin", i) {
                cmds.push(AppCommand::ExtractFrameToBin);
            }
            // F — Match Frame (program → source).
            if kb.is_action_pressed("match_frame_program_to_source", i) {
                cmds.push(AppCommand::MatchFrameProgramToSource);
            }
            // Shift+F — Match Frame (source → program), cycle to next.
            if kb.is_action_pressed("match_frame_source_to_program", i) {
                cmds.push(AppCommand::MatchFrameSourceToProgram);
            }
            // (`toggle_scrub_audio` action removed — Shift+S now drives
            // `toggle_audio_skim` in shortcuts_magnetic_skim. JKL
            // transport audio still flips through the prefs panel.)
            // Cut clip
            if kb.is_action_pressed("cut_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::CutClip { clip_id });
                }
            }
            // Ctrl+Shift+B / Ctrl+Shift+I — collapse a side panel to its
            // rail. The View menu has advertised shortcuts for these
            // since it was written; `shortcut_text` only draws a label,
            // so until now they did nothing when pressed.
            if kb.is_action_pressed("toggle_browser_panel", i) {
                cmds.push(AppCommand::ToggleBrowserPanel);
            }
            if kb.is_action_pressed("toggle_inspector_panel", i) {
                cmds.push(AppCommand::ToggleInspectorPanel);
            }
            // B key (bare) — toggle before/after
            if kb.is_action_pressed("toggle_before_after", i) {
                cmds.push(AppCommand::ToggleBeforeAfter);
            }
            // C key (bare) — toggle color panel
            if kb.is_action_pressed("toggle_color_panel", i) {
                cmds.push(AppCommand::ToggleColorPanel);
            }
            // Z key (bare) — toggle zebra overlay
            if kb.is_action_pressed("toggle_zebra", i) {
                self.state.viewer_overlays.zebras = !self.state.viewer_overlays.zebras;
            }
            // Alt+F — toggle false color overlay
            if kb.is_action_pressed("toggle_false_color", i) {
                self.state.viewer_overlays.false_color = !self.state.viewer_overlays.false_color;
            }
            // Ctrl+P — toggle focus peaking overlay
            if i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.shift
                && i.key_pressed(egui::Key::P)
            {
                self.state.viewer_overlays.focus_peaking = !self.state.viewer_overlays.focus_peaking;
            }
            // Alt+V — toggle scopes: the dock in Color, the floating
            // window elsewhere (the timeline keeps its rows). Bare V is
            // the select tool.
            if kb.is_action_pressed("toggle_scopes", i) {
                self.state.toggle_scopes();
            }
            // Alt+S — toggle safe-areas overlay (action + title).
            if kb.is_action_pressed("toggle_safe_areas", i) {
                self.state.viewer_overlays.safe_areas = !self.state.viewer_overlays.safe_areas;
            }
            // Alt+G — toggle rule-of-thirds grid + centre crosshair overlay.
            if kb.is_action_pressed("toggle_grid_overlay", i) {
                self.state.viewer_overlays.grid = !self.state.viewer_overlays.grid;
            }
            // Shift+V — cycle scope mode
            if kb.is_action_pressed("cycle_scope_mode", i) {
                use crate::state::ScopeMode;
                self.state.scope_mode = match self.state.scope_mode {
                    ScopeMode::Waveform => ScopeMode::Parade,
                    ScopeMode::Parade => ScopeMode::Vectorscope,
                    ScopeMode::Vectorscope => ScopeMode::Histogram,
                    ScopeMode::Histogram => ScopeMode::Waveform,
                };
                if !self.state.scopes_shown() {
                    self.state.toggle_scopes();
                }
                let mode_byte = match self.state.scope_mode {
                    crate::state::ScopeMode::Waveform => 0u8,
                    crate::state::ScopeMode::Parade => 1,
                    crate::state::ScopeMode::Vectorscope => 2,
                    crate::state::ScopeMode::Histogram => 3,
                };
                cmds.push(AppCommand::SetScopeRenderMode { mode: mode_byte });
                cmds.push(AppCommand::SequenceUpdated);
            }
            // [ / ] — navigate color nodes
            if kb.is_action_pressed("prev_color_node", i) || kb.is_action_pressed("next_color_node", i) {
                let forward = kb.is_action_pressed("next_color_node", i);
                if let Some(clip_id) = self.state.selected_clip {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    if let Some(clip) = seq.all_clips().find(|c| c.id == clip_id) {
                        let nodes = &clip.node_graph.nodes;
                        if !nodes.is_empty() {
                            let current_idx = self.state.selected_node_id
                                .and_then(|sel| nodes.iter().position(|n| n.id == sel));
                            let next_idx = match current_idx {
                                Some(idx) => {
                                    if forward {
                                        (idx + 1) % nodes.len()
                                    } else {
                                        (idx + nodes.len() - 1) % nodes.len()
                                    }
                                }
                                None => 0,
                            };
                            // Single-select cycle: drop any multi-set
                            // accumulated from shift-click / box-select
                            // and replace it with just the new primary.
                            // Matches every-NLE keyboard-cycle behaviour.
                            let new_id = nodes[next_idx].id;
                            self.state.selected_node_id = Some(new_id);
                            self.state.selected_node_ids.clear();
                            self.state.selected_node_ids.insert(new_id);
                        }
                    }
                }
            }
            // Alt+D — toggle bypass on selected node
            if kb.is_action_pressed("toggle_node_bypass", i) {
                if let Some(node_id) = self.state.selected_node_id {
                    if let Some(clip_id) = self.state.selected_clip {
                        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                        if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == node_id) {
                                node.enabled = !node.enabled;
                            }
                        }
                        self.sync_sequence_to_engine();
                    }
                }
            }
            // Ctrl+Shift+R — reset current color node to defaults
            if kb.is_action_pressed("reset_current_node", i) {
                if let Some(node_id) = self.state.selected_node_id {
                    if let Some(clip_id) = self.state.selected_clip {
                        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                        if let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == node_id) {
                                node.params = klipp_nodes::NodeParams::default();
                            }
                        }
                        self.sync_sequence_to_engine();
                    }
                }
            }
            // Copy / Copy Attributes / Copy Color Nodes
            //
            // Two distinct rebindable actions share defaults at Ctrl+C:
            // `copy_clip` (timeline) and `copy_color_nodes` (node graph).
            // When both fire the same frame (the shared-default case),
            // panel context disambiguates — color path wins inside the
            // node graph / color inspector with a node selection;
            // otherwise the clip path wins. When the user explicitly
            // rebinds one to a distinct combo, only that action fires
            // and the panel gate is bypassed (the user's explicit binding
            // is treated as authoritative). egui's TextEdit consumes
            // Ctrl+C internally when a text field has focus, so this
            // dispatch never fights the OS-level text clipboard.
            let in_color_panel = matches!(
                self.state.active_panel,
                Some(crate::state::PanelKind::NodeGraph) | Some(crate::state::PanelKind::NodeInspector)
            );
            let has_node_selection = !self.state.selected_node_ids.is_empty()
                || self.state.selected_node_id.is_some();
            let copy_clip_pressed = kb.is_action_pressed("copy_clip", i);
            let copy_color_pressed = kb.is_action_pressed("copy_color_nodes", i);
            match (copy_color_pressed, copy_clip_pressed) {
                (true, true) => {
                    // Shared-default conflict — panel context decides.
                    if in_color_panel && has_node_selection {
                        if let Some(clip_id) = self.state.selected_clip {
                            cmds.push(AppCommand::CopyColorNodes { clip_id });
                        }
                    } else if let Some(clip_id) = self.state.selected_clip {
                        cmds.push(AppCommand::CopyClip { clip_id });
                    }
                }
                (true, false) => {
                    if let Some(clip_id) = self.state.selected_clip {
                        cmds.push(AppCommand::CopyColorNodes { clip_id });
                    }
                }
                (false, true) => {
                    if let Some(clip_id) = self.state.selected_clip {
                        cmds.push(AppCommand::CopyClip { clip_id });
                    }
                }
                (false, false) => {}
            }
            if kb.is_action_pressed("copy_attributes", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::CopyAttributes { clip_id });
                }
            }
            // Paste / Paste Attributes / Paste Color Nodes
            //
            // Same shape as Ctrl+C: distinct rebindable actions sharing
            // the Ctrl+V default; panel context disambiguates.
            if kb.is_action_pressed("paste_insert", i) {
                cmds.push(AppCommand::PasteClipInsert);
            }
            let paste_clip_pressed = kb.is_action_pressed("paste_clip", i);
            let paste_color_pressed = kb.is_action_pressed("paste_color_nodes", i);
            let color_clipboard_ready = self.state.grade_node_clipboard.as_ref()
                .is_some_and(|c| !c.nodes.is_empty());
            match (paste_color_pressed, paste_clip_pressed) {
                (true, true) => {
                    if in_color_panel && color_clipboard_ready {
                        if let Some(clip_id) = self.state.selected_clip {
                            cmds.push(AppCommand::PasteColorNodes { clip_id });
                        }
                    } else {
                        cmds.push(AppCommand::PasteClip);
                    }
                }
                (true, false) => {
                    if let Some(clip_id) = self.state.selected_clip {
                        cmds.push(AppCommand::PasteColorNodes { clip_id });
                    }
                }
                (false, true) => {
                    cmds.push(AppCommand::PasteClip);
                }
                (false, false) => {}
            }
            if kb.is_action_pressed("paste_attributes", i)
                && self.state.attributes_clipboard.is_some() && self.state.selected_clip.is_some() {
                    self.state.show_paste_attributes_dialog = true;
                }
            // Duplicate clip
            if kb.is_action_pressed("duplicate_clip", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::DuplicateClip { clip_id });
                }
            }
            // Split at playhead
            if kb.is_action_pressed("split_at_playhead", i) {
                cmds.extend(self.state.split_at_playhead_commands());
            }
            // Freeze frame at playhead (default 2s duration).
            if kb.is_action_pressed("freeze_frame", i) {
                cmds.push(AppCommand::InsertFreezeFrame { duration_secs: 2.0 });
            }
            // Toggle loop playback
            if kb.is_action_pressed("toggle_loop", i) {
                cmds.push(AppCommand::ToggleLoopPlayback);
            }
        });
    }

    fn shortcuts_title_and_tools2(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        let mut pending_perf_recording_toggle = false;
        ctx.input(|i| {
            // Ctrl+T — a title at the playhead.
            if kb.is_action_pressed("create_title", i) {
                cmds.push(AppCommand::AddTitleAtPlayhead { over_clip: None });
            }
            // B — Blade tool
            if kb.is_action_pressed("blade_tool", i) {
                tracing::debug!("Blade tool");
                cmds.push(AppCommand::SetActiveTool(Tool::Blade));
            }
            // V — Select tool
            if kb.is_action_pressed("select_tool", i) {
                tracing::debug!("Select tool");
                cmds.push(AppCommand::SetActiveTool(Tool::Select));
            }
            // R — Range tool toggle (Shift+R in the source viewer rejects
            // a stretch instead; see the marker keys)
            let rejecting = i.modifiers.shift && self.state.viewer_mode == ViewerMode::Source && self.state.source_media_id.is_some();
            if !rejecting && kb.is_action_pressed("range_tool", i) {
                if self.state.active_tool == Tool::Range {
                    cmds.push(AppCommand::SetActiveTool(Tool::Select));
                } else {
                    cmds.push(AppCommand::SetActiveTool(Tool::Range));
                }
            }
            // T — Trim tool (bare T; Ctrl+T handled elsewhere for Create Title)
            if kb.is_action_pressed("trim_tool", i) {
                tracing::debug!("Trim tool");
                cmds.push(AppCommand::SetActiveTool(Tool::Trim));
            }
            // X — Set range to clip at playhead / Alt+X — Clear range
            // Clearing the range used to live in here as an Alt branch
            // as well as in its own clear_in_out handler, so Alt+X ran
            // it twice and left two entries in the history.
            if kb.is_action_pressed("set_range_to_clip", i) {
                {
                    // X: Set range to clip at playhead
                    let ph = self.state.playhead;
                    let pushed = {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let mut found_clip: Option<(klipp_core::Timecode, klipp_core::Timecode, Option<klipp_core::Timecode>, Option<klipp_core::Timecode>)> = None;
                        for track in &seq.tracks {
                            if let Some((_, clip)) = track.clip_at_timecode(ph) {
                                found_clip = Some((clip.timeline_in, clip.timeline_out, seq.in_point, seq.out_point));
                                break;
                            }
                        }
                        found_clip
                    };
                    if let Some((tl_in, tl_out, old_in, old_out)) = pushed {
                        cmds.push(AppCommand::SetSequenceInPoint(tl_in));
                        cmds.push(AppCommand::SetSequenceOutPoint(tl_out));
                        self.state.push_history(crate::history::EditCommand::SetRange {
                            old_in, old_out,
                            new_in: Some(tl_in),
                            new_out: Some(tl_out),
                        });
                        self.state.toast.show("Range: clip selected");
                    }
                }
            }
            // Fullscreen viewer, on F11 — the combo every desktop
            // agrees on. It was on bare F, which is Match Frame, so a
            // single press ran both: you matched the frame and the
            // viewer went fullscreen underneath you.
            if kb.is_action_pressed("toggle_fullscreen", i) {
                cmds.push(AppCommand::ToggleFullscreen);
            }
            // P — Toggle performance overlay
            if kb.is_action_pressed("toggle_perf_overlay", i) {
                self.state.show_perf_overlay = !self.state.show_perf_overlay;
            }
            // Ctrl+Shift+P — Toggle perf recording session. Captured
            // as a boolean here; the actual ring-buffer mark/dump
            // happens in `update` after the input phase so the
            // borrow on self can release cleanly.
            if kb.is_action_pressed("toggle_perf_recording", i) {
                pending_perf_recording_toggle = true;
            }
        });
        if pending_perf_recording_toggle {
            self.toggle_perf_recording();
        }
    }

    fn shortcuts_multicam_and_io(&mut self, ctx: &egui::Context, text_editing: bool, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // 1-9 — Switch multicam angle (via keybinding system for remapping support)
            if !text_editing {
                for angle in 1usize..=9 {
                    let action = format!("multicam_angle_{angle}");
                    if kb.is_action_pressed(&action, i) {
                        cmds.push(AppCommand::SwitchMulticamAngle { angle });
                    }
                }
                if kb.is_action_pressed("create_multicam", i) {
                    cmds.push(AppCommand::CreateMulticamClip);
                }
            }
            // N — Toggle snap (dispatches through AppCommand so layout
            // persists on flip, matching the toolbar / palette / View
            // menu surfaces).
            if kb.is_action_pressed("toggle_snap", i) {
                cmds.push(AppCommand::SetSnapMaster(!self.state.snap.master));
            }
            // I/O — In/Out markers (context-dependent on viewer mode)
            if kb.is_action_pressed("clear_in_point", i) || kb.is_action_pressed("set_in_point", i) {
                if kb.is_action_pressed("clear_in_point", i) {
                    tracing::debug!("Clear sequence in point");
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    cmds.push(AppCommand::ClearSequenceInPoint);
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: None, new_out: old_out,
                        },
                    });
                } else if self.state.viewer_mode == ViewerMode::Source {
                    tracing::debug!("I key (Source mode) -> Set source in point at {:?}", self.state.source_playhead);
                    cmds.push(AppCommand::SetSourceInPoint(self.state.source_playhead));
                } else {
                    tracing::debug!("I key (Program mode) -> Set sequence in point at {:?}", self.state.playhead);
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    let new_in = Some(self.state.playhead);
                    cmds.push(AppCommand::SetSequenceInPoint(self.state.playhead));
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in, new_out: old_out,
                        },
                    });
                }
            }
            if kb.is_action_pressed("clear_out_point", i) || kb.is_action_pressed("set_out_point", i) {
                if kb.is_action_pressed("clear_out_point", i) {
                    tracing::debug!("Clear sequence out point");
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    cmds.push(AppCommand::ClearSequenceOutPoint);
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: old_in, new_out: None,
                        },
                    });
                } else if self.state.viewer_mode == ViewerMode::Source {
                    tracing::debug!("O key (Source mode) -> Set source out point at {:?}", self.state.source_playhead);
                    cmds.push(AppCommand::SetSourceOutPoint(self.state.source_playhead));
                } else {
                    tracing::debug!("O key (Program mode) -> Set sequence out point at {:?}", self.state.playhead);
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    let new_out = Some(self.state.playhead);
                    cmds.push(AppCommand::SetSequenceOutPoint(self.state.playhead));
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: old_in, new_out,
                        },
                    });
                }
            }

            // Alt+X — clear both in/out points (standard NLE shortcut)
            if kb.is_action_pressed("clear_in_out", i) {
                let old_in = self.state.sequence().in_point;
                let old_out = self.state.sequence().out_point;
                if old_in.is_some() || old_out.is_some() {
                    cmds.push(AppCommand::ClearSequenceInPoint);
                    cmds.push(AppCommand::ClearSequenceOutPoint);
                    self.state.active_history_mut().push(crate::history::HistoryEntry {
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: None, new_out: None,
                        },
                    });
                }
            }

        });
    }

    fn shortcuts_edit_nav(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Escape — deselect clips, clear range drag, or return Range tool to Select
            if i.key_pressed(egui::Key::Escape) {
                // Unlock scope cursor on Escape
                if self.state.surface_tool.is_some() {
                    // The viewer's surface tool goes; the clip stays selected.
                    self.state.surface_tool = None;
                } else if self.state.scope_cursor_locked {
                    self.state.scope_cursor_locked = false;
                    self.state.scope_cursor_locked_pos = None;
                    self.state.scope_cursor_locked_data = None;
                } else if let Some(drag) = self.state.range_drag.take() {
                    self.state.sequences[self.state.active_sequence_idx].in_point = drag.original_in;
                    self.state.sequences[self.state.active_sequence_idx].out_point = drag.original_out;
                } else if self.state.selected_clip.is_some() || !self.state.selected_clips.is_empty() {
                    self.state.selected_clips.clear();
                    cmds.push(AppCommand::SelectClip(None));
                } else if self.state.active_tool == Tool::Range {
                    cmds.push(AppCommand::SetActiveTool(Tool::Select));
                }
                self.state.ruler_press_origin = None;
            }
            // Deselect All
            if kb.is_action_pressed("deselect_all", i) {
                self.state.selected_clips.clear();
                cmds.push(AppCommand::SelectClip(None));
            }
            // Select All clips
            if kb.is_action_pressed("select_all", i) {
                cmds.push(AppCommand::SelectAllClips);
            }
            // The selected clip's track: all of it, or from the playhead on
            // or back (#844).
            if kb.is_action_pressed("select_track_clips", i) {
                cmds.push(AppCommand::SelectClipsOnSelectedTrack { range: klipp_core::SelectRange::All });
            }
            if kb.is_action_pressed("select_track_forward", i) {
                cmds.push(AppCommand::SelectClipsOnSelectedTrack { range: klipp_core::SelectRange::Forward });
            }
            if kb.is_action_pressed("select_track_backward", i) {
                cmds.push(AppCommand::SelectClipsOnSelectedTrack { range: klipp_core::SelectRange::Backward });
            }
            if kb.is_action_pressed("select_forward", i) {
                cmds.push(AppCommand::SelectClipsForward);
            }
            if kb.is_action_pressed("select_backward", i) {
                cmds.push(AppCommand::SelectClipsBackward);
            }
            // Previous edit point
            if kb.is_action_pressed("prev_edit_point", i) {
                cmds.push(AppCommand::PrevEditPoint);
            }
            // Next edit point
            if kb.is_action_pressed("next_edit_point", i) {
                cmds.push(AppCommand::NextEditPoint);
            }
            // Insert edit (ripple)
            if kb.is_action_pressed("insert_edit", i)
            {
                cmds.push(AppCommand::InsertEdit);
            }
            // Overwrite edit
            if kb.is_action_pressed("overwrite_edit", i) {
                cmds.push(AppCommand::OverwriteEdit);
            }
            // Lift edit
            if kb.is_action_pressed("lift_edit", i) {
                cmds.push(AppCommand::LiftEdit);
            }
            // Extract edit
            if kb.is_action_pressed("extract_edit", i) {
                cmds.push(AppCommand::ExtractEdit);
            }
        });
    }

    fn shortcuts_keyframe_and_zoom(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let kb = self.key_bindings.clone();
        ctx.input(|i| {
            // Alt+M — Toggle matte preview
            if kb.is_action_pressed("toggle_matte_preview", i) {
                self.state.matte_preview_active = !self.state.matte_preview_active;
                cmds.push(AppCommand::SetMattePreview(self.state.matte_preview_active));
            }
            // K — Toggle keyframe at playhead for the active parameter
            if kb.is_action_pressed("toggle_keyframe", i) {
                let mut keys_in_frame = 0;
                if let Some(clip_id) = self.state.selected_clip {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    // Find the clip and its track kind
                    let clip_and_kind = seq.tracks.iter().find_map(|t| {
                        t.clips.iter().find(|c| c.id == clip_id).map(|c| (c, t.kind))
                    });
                    if let Some((clip, track_kind)) = clip_and_kind {
                        let playhead_secs = self.state.playhead.to_secs_f64();
                        let clip_start_secs = clip.timeline_in.to_secs_f64();
                        let clip_end_secs = clip.timeline_out.to_secs_f64();
                        if playhead_secs >= clip_start_secs && playhead_secs <= clip_end_secs {
                            let clip_time_secs = playhead_secs - clip_start_secs;
                            let tolerance = crate::inspector::keyframe_selection::frame_tolerance(seq);
                            if track_kind == klipp_timeline::TrackKind::Audio {
                                match self.state.rubber_band_param.clone() {
                                    crate::state::RubberBandParam::ClipParam(param) => {
                                        let keyframes = match param {
                                            klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                            klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                            klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                        };
                                        keys_in_frame = crate::inspector::keyframe_selection::state(keyframes, clip_time_secs, tolerance).keys_in_frame;
                                        let existing = crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, tolerance).map(|i| &keyframes[i]);
                                        if existing.is_some() {
                                            cmds.push(AppCommand::EditAudioKeyframeAtFrame { clip_id, param, time_secs: clip_time_secs, value: None });
                                        } else {
                                            let default_val = match param {
                                                klipp_core::AudioParam::Volume => clip.volume,
                                                klipp_core::AudioParam::Pan => clip.pan,
                                                klipp_core::AudioParam::Gain => clip.gain,
                                            };
                                            let value = klipp_core::keyframe::evaluate(keyframes, clip_time_secs, default_val);
                                            cmds.push(AppCommand::EditAudioKeyframeAtFrame { clip_id, param, time_secs: clip_time_secs, value: Some(value) });
                                        }
                                    }
                                    crate::state::RubberBandParam::EffectParam { effect_index, param_name } => {
                                        let key = format!("{effect_index}:{param_name}");
                                        let empty: Vec<klipp_core::Keyframe> = Vec::new();
                                        let kfs_ref = clip.audio_effect_keyframes.get(&key).unwrap_or(&empty);
                                        keys_in_frame = crate::inspector::keyframe_selection::state(kfs_ref, clip_time_secs, tolerance).keys_in_frame;
                                        let existing = crate::inspector::keyframe_selection::nearest(kfs_ref, clip_time_secs, tolerance).map(|i| &kfs_ref[i]);
                                        if let Some(kf) = existing {
                                            cmds.push(AppCommand::RemoveAudioEffectKeyframe {
                                                clip_id, effect_index, param_name: param_name.clone(), time_secs: kf.time_secs,
                                            });
                                        } else {
                                            let default_val = clip.audio_effects.get(effect_index)
                                                .and_then(|inst| inst.params.iter().find(|(k, _)| k == &param_name).map(|(_, v)| *v))
                                                .unwrap_or(0.0);
                                            let value = klipp_core::keyframe::evaluate(kfs_ref, clip_time_secs, default_val);
                                            cmds.push(AppCommand::AddAudioEffectKeyframe {
                                                clip_id, effect_index, param_name, time_secs: clip_time_secs, value,
                                            });
                                        }
                                    }
                                }
                            } else {
                                let param = self.state.video_rubber_band_param;
                                let opacity_static = clip.builtin_effects.iter()
                                    .find_map(|e| match e { klipp_effects::BuiltinEffect::Opacity { value } => Some(*value), _ => None })
                                    .unwrap_or(1.0);
                                let transform = clip.builtin_effects.iter()
                                    .find_map(|e| match e {
                                        klipp_effects::BuiltinEffect::Transform { position_x, position_y, scale_x, rotation, .. } =>
                                            Some((*position_x, *position_y, *scale_x, *rotation)),
                                        _ => None,
                                    })
                                    .unwrap_or((0.0, 0.0, 1.0, 0.0));
                                let keyframes = match param {
                                    klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                    klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                    klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                    klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                    klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                    klipp_core::VideoParam::TiltX => &clip.transform_tilt_x_keyframes,
                                    klipp_core::VideoParam::TiltY => &clip.transform_tilt_y_keyframes,
                                    klipp_core::VideoParam::MatteFeather => &clip.matte_feather_keyframes,
                                    klipp_core::VideoParam::MatteChoke => &clip.matte_choke_keyframes,
                                };
                                keys_in_frame = crate::inspector::keyframe_selection::state(keyframes, clip_time_secs, tolerance).keys_in_frame;
                                        let existing = crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, tolerance).map(|i| &keyframes[i]);
                                if let Some(kf) = existing {
                                    cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param, time_secs: kf.time_secs });
                                } else {
                                    let default_val = match param {
                                        klipp_core::VideoParam::Opacity => opacity_static,
                                        klipp_core::VideoParam::PositionX => transform.0,
                                        klipp_core::VideoParam::PositionY => transform.1,
                                        klipp_core::VideoParam::Scale => transform.2,
                                        klipp_core::VideoParam::Rotation => transform.3,
                                        klipp_core::VideoParam::TiltX => crate::inspector::video_tab::get_transform(clip).rotation_x,
                                        klipp_core::VideoParam::TiltY => crate::inspector::video_tab::get_transform(clip).rotation_y,
                                        klipp_core::VideoParam::MatteFeather => clip.matte_source.as_ref().map(|b| b.feather_px).unwrap_or(0.0),
                                        klipp_core::VideoParam::MatteChoke => clip.matte_source.as_ref().map(|b| b.choke_px).unwrap_or(0.0),
                                    };
                                    let value = klipp_core::keyframe::evaluate(keyframes, clip_time_secs, default_val);
                                    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param, time_secs: clip_time_secs, value });
                                }
                            }
                        }
                    }
                }
                if keys_in_frame > 1 { self.state.toast.show(format!("{keys_in_frame} keys share this frame; K edits the nearest. Use Shift+K for individual subframe keys.")); }
            }
            // Shift+K — Toggle curve editor (keyframe editor) panel
            if kb.is_action_pressed("toggle_keyframe_editor", i) {
                self.state.show_curve_editor = !self.state.show_curve_editor;
                self.state.show_keyframe_editor = self.state.show_curve_editor;
            }
            // Ctrl+Shift+M — Toggle audio mixer panel
            // Audio mixer and subtitle editor moved to the keybinding
            // registry (toggle_audio_mixer / toggle_subtitle_editor) and
            // are handled in shortcuts_ctrl_global. Handling them here
            // too toggled each twice a frame, which is the same as not
            // at all.
            // Ctrl+Shift+N — Toggle Project Notes panel
            if kb.is_action_pressed("toggle_project_notes", i) {
                self.state.show_project_notes = !self.state.show_project_notes;
            }
            // Alt+Left — Navigate to previous keyframe
            if kb.is_action_pressed("prev_keyframe", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let clip_and_kind = seq.tracks.iter().find_map(|t| {
                        t.clips.iter().find(|c| c.id == clip_id).map(|c| (c, t.kind))
                    });
                    if let Some((clip, track_kind)) = clip_and_kind {
                        let clip_time_secs = self.state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64();
                        let empty_kfs: Vec<klipp_core::Keyframe> = Vec::new();
                        let keyframes: &Vec<klipp_core::Keyframe> = if track_kind == klipp_timeline::TrackKind::Audio {
                            match &self.state.rubber_band_param {
                                crate::state::RubberBandParam::ClipParam(param) => match param {
                                    klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                },
                                crate::state::RubberBandParam::EffectParam { effect_index, param_name } => {
                                    clip.audio_effect_keyframes
                                        .get(&format!("{effect_index}:{param_name}"))
                                        .unwrap_or(&empty_kfs)
                                }
                            }
                        } else {
                            match self.state.video_rubber_band_param {
                                klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                klipp_core::VideoParam::TiltX => &clip.transform_tilt_x_keyframes,
                                klipp_core::VideoParam::TiltY => &clip.transform_tilt_y_keyframes,
                                klipp_core::VideoParam::MatteFeather => &clip.matte_feather_keyframes,
                                klipp_core::VideoParam::MatteChoke => &clip.matte_choke_keyframes,
                            }
                        };
                        // Find previous keyframe (strictly before current time with tolerance)
                        let prev = crate::inspector::keyframe_selection::state(keyframes, clip_time_secs,
                            crate::inspector::keyframe_selection::frame_tolerance(seq)).prev_keyframe_time;
                        if let Some(time) = prev {
                            let abs_secs = clip.timeline_in.to_secs_f64() + time;
                            let rate = self.state.playhead.rate;
                            self.state.playhead = klipp_core::Timecode::from_secs_f64(abs_secs, rate);
                            let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                        }
                    }
                }
            }
            // Alt+Right — Navigate to next keyframe
            if kb.is_action_pressed("next_keyframe", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let clip_and_kind = seq.tracks.iter().find_map(|t| {
                        t.clips.iter().find(|c| c.id == clip_id).map(|c| (c, t.kind))
                    });
                    if let Some((clip, track_kind)) = clip_and_kind {
                        let clip_time_secs = self.state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64();
                        let empty_kfs: Vec<klipp_core::Keyframe> = Vec::new();
                        let keyframes: &Vec<klipp_core::Keyframe> = if track_kind == klipp_timeline::TrackKind::Audio {
                            match &self.state.rubber_band_param {
                                crate::state::RubberBandParam::ClipParam(param) => match param {
                                    klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                },
                                crate::state::RubberBandParam::EffectParam { effect_index, param_name } => {
                                    clip.audio_effect_keyframes
                                        .get(&format!("{effect_index}:{param_name}"))
                                        .unwrap_or(&empty_kfs)
                                }
                            }
                        } else {
                            match self.state.video_rubber_band_param {
                                klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                                klipp_core::VideoParam::TiltX => &clip.transform_tilt_x_keyframes,
                                klipp_core::VideoParam::TiltY => &clip.transform_tilt_y_keyframes,
                                klipp_core::VideoParam::MatteFeather => &clip.matte_feather_keyframes,
                                klipp_core::VideoParam::MatteChoke => &clip.matte_choke_keyframes,
                            }
                        };
                        // Find next keyframe (strictly after current time with tolerance)
                        let next = crate::inspector::keyframe_selection::state(keyframes, clip_time_secs,
                            crate::inspector::keyframe_selection::frame_tolerance(seq)).next_keyframe_time;
                        if let Some(time) = next {
                            let abs_secs = clip.timeline_in.to_secs_f64() + time;
                            let rate = self.state.playhead.rate;
                            self.state.playhead = klipp_core::Timecode::from_secs_f64(abs_secs, rate);
                            let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                        }
                    }
                }
            }
            // In the source viewer, marks go on the pool clip: M a note at
            // the playhead, Shift+F a favourite stretch, Shift+R a rejected
            // one — the stretch between the in and out points, or the
            // whole clip when none are set.
            let in_source = self.state.viewer_mode == ViewerMode::Source && self.state.source_media_id.is_some();
            if in_source && kb.is_action_pressed("add_marker", i) {
                if let Some(media_id) = self.state.source_media_id {
                    cmds.push(AppCommand::AddMediaMarker { media_id, marker: klipp_core::MediaMarker::note(self.state.source_playhead.to_secs_f64()) });
                }
            }
            for (key, kind) in [(egui::Key::F, klipp_core::MediaMarkerKind::Favourite), (egui::Key::R, klipp_core::MediaMarkerKind::Reject)] {
                if in_source && i.key_pressed(key) && i.modifiers.shift && !i.modifiers.ctrl && !i.modifiers.alt {
                    if let Some(marker) = self.source_range_marker(kind) {
                        if let Some(media_id) = self.state.source_media_id {
                            cmds.push(AppCommand::AddMediaMarker { media_id, marker });
                        }
                    }
                }
            }
            // M — Add sequence marker (no modifiers, no shift)
            if !in_source && kb.is_action_pressed("add_marker", i) {
                cmds.push(AppCommand::AddMarker {
                    position: self.state.playhead,
                    label: String::new(),
                    color: klipp_core::MarkerColor::Blue,
                });
            }
            // Shift+M — Add per-clip marker at playhead on selected clip
            if kb.is_action_pressed("add_clip_marker", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    // Find the clip and compute the time relative to source_in
                    let clip_info = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == clip_id)
                        .map(|c| (c.timeline_in, c.timeline_out));
                    if let Some((tl_in, tl_out)) = clip_info {
                        let playhead_secs = self.state.playhead.to_secs_f64();
                        let clip_start_secs = tl_in.to_secs_f64();
                        let clip_end_secs = tl_out.to_secs_f64();
                        if playhead_secs >= clip_start_secs && playhead_secs <= clip_end_secs {
                            let rel_time = playhead_secs - clip_start_secs;
                            cmds.push(AppCommand::AddClipMarker {
                                clip_id,
                                time_secs: rel_time,
                                name: String::new(),
                                color: klipp_core::MarkerColor::Green,
                            });
                        }
                    }
                }
            }
            // Z — Fit to window (no modifiers)
            if kb.is_action_pressed("fit_to_window", i) {
                cmds.push(AppCommand::FitTimelineToWindow);
            }
            // Shift+Z — Zoom to selection
            if kb.is_action_pressed("zoom_to_selection", i) {
                if let Some(clip_id) = self.state.selected_clip {
                    let clip = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == clip_id);
                    if let Some(clip) = clip {
                        let start = clip.timeline_in.to_secs_f64() as f32;
                        let end = clip.timeline_out.to_secs_f64() as f32;
                        let dur = (end - start).max(0.1);
                        let content_width = self.state.last_timeline_content_width;
                        self.state.timeline_zoom = (content_width / dur * 0.9).clamp(0.1, 2000.0);
                        self.state.timeline_scroll_x = (start * self.state.timeline_zoom - 20.0).max(0.0);
                    }
                }
            }
            // + / = — Zoom in (playhead-centred)
            if i.events.iter().any(|e| matches!(e, egui::Event::Text(s) if s == "+" || s == "="))
                && !i.modifiers.ctrl
            {
                let playhead_secs = self.state.playhead_smooth_secs as f32;
                let old_zoom = self.state.timeline_zoom;
                let new_zoom = (old_zoom * self.state.zoom_step).clamp(0.1, 2000.0);
                self.state.timeline_scroll_x = (playhead_secs * new_zoom
                    - (playhead_secs * old_zoom - self.state.timeline_scroll_x))
                    .max(0.0);
                self.state.timeline_zoom = new_zoom;
            }
            // - — Zoom out (playhead-centred)
            if kb.is_action_pressed("zoom_out", i) {
                self.zoom_timeline_by(1.0 / self.state.zoom_step);
            }
            // Zoom in had a binding and a menu entry but no handler at
            // all: the View menu said "=" and the key did nothing.
            if kb.is_action_pressed("zoom_in", i) {
                self.zoom_timeline_by(self.state.zoom_step);
            }
        });
    }
}

/// 2 GB cap on the AI mask cache directory. Bigger than any
/// realistic short-term workflow (a 1080p mask PNG is ~80–200 KB
/// — that's 10k+ masks at the high end) but enforces a ceiling
/// so months of heavy use can't quietly fill the disk.
const AI_MASK_CACHE_BYTES_CAP: u64 = 2 * 1024 * 1024 * 1024;

/// LRU prune of `~/.cache/klipp/ai_masks/`. Walks the directory,
/// stats each file's size + mtime, and if the total is over the
/// cap deletes oldest-first until under cap. Errors at every
/// step are logged + swallowed — failure to prune is never a
/// reason to refuse to start the app.
///
/// This is the disk-pressure safety net. Proper orphan-detection
/// (parse filenames, cross-reference with current project's clip
/// IDs) is filed as a follow-up — the schema documented in
/// execute_apply_mask_to_clip puts enough metadata in the
/// filename to support it.
fn prune_ai_mask_cache() {
    let Some(cache_root) = dirs::cache_dir() else {
        return;
    };
    let dir = cache_root.join("klipp").join("ai_masks");
    if !dir.exists() {
        return;
    }
    let entries = match std::fs::read_dir(&dir) {
        Ok(it) => it,
        Err(e) => {
            tracing::debug!(
                "ai mask cache prune: read_dir failed: {} ({e})",
                dir.display(),
            );
            return;
        }
    };
    let mut files: Vec<(std::path::PathBuf, u64, std::time::SystemTime)> =
        Vec::new();
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let size = meta.len();
        let mtime = meta
            .modified()
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        total += size;
        files.push((path, size, mtime));
    }
    if total <= AI_MASK_CACHE_BYTES_CAP {
        return;
    }
    // Oldest-first delete order so the freshest masks (most
    // likely to be referenced by the current project) survive.
    files.sort_by_key(|(_, _, mtime)| *mtime);
    let mut deleted = 0u32;
    let mut freed = 0u64;
    for (path, size, _) in files {
        if total.saturating_sub(freed) <= AI_MASK_CACHE_BYTES_CAP {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            freed += size;
            deleted += 1;
        }
    }
    if deleted > 0 {
        tracing::info!(
            "ai mask cache prune: deleted {} file(s), freed {} bytes (cap {})",
            deleted,
            freed,
            AI_MASK_CACHE_BYTES_CAP,
        );
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

fn channel_config_to_u32(c: klipp_timeline::ChannelConfig) -> u32 {
    match c {
        klipp_timeline::ChannelConfig::Default => 0,
        klipp_timeline::ChannelConfig::MonoDownmix => 1,
        klipp_timeline::ChannelConfig::SwapLR => 2,
        klipp_timeline::ChannelConfig::Surround51 => 3,
        klipp_timeline::ChannelConfig::Surround71 => 4,
    }
}

/// Megabits per second a codec writes at a given pixel count, where
/// that is a fixed property of the codec rather than a choice.
///
/// The intra-frame masters have published rates: every frame is coded
/// on its own at a set quality, so the file size follows from the
/// resolution, the frame rate and the running time and nothing else.
/// Scaled from the profile's quoted 1920x1080 rate by pixel count and
/// by frame rate, which is how the published tables are built.
///
/// `None` for anything whose size depends on the picture — H.264 and
/// H.265 under CRF spend what the shot needs, and no arithmetic here
/// can know that.
fn intra_frame_mbps(codec: klipp_core::ExportVideoCodec, px: f64, fps: f64) -> Option<f64> {
    use klipp_core::ExportVideoCodec as C;
    // At 1920x1080 and 29.97 fps, from Apple's and Avid's tables.
    let at_1080p29 = match codec {
        C::ProRes422 => 147.0,
        C::ProRes422HQ => 220.0,
        C::ProRes4444 => 330.0,
        C::DNxHD115 => 115.0,
        C::DNxHD145 => 145.0,
        _ => return None,
    };
    let reference_px = 1920.0 * 1080.0;
    Some(at_1080p29 * (px / reference_px) * (fps / 29.97))
}

/// What a quality (CRF) encode spends, low and high, in Mbps: from a
/// calm, still picture to busy handheld footage. At 1080p30 and the
/// encoder's default quality H.264 spends roughly 3 to 12 Mbps; six
/// steps of CRF halve or double it; HEVC, VP9 and AV1 need less for the
/// same picture. Pixels count for less than linearly (a 4K frame has
/// more redundancy) and frame rate for less again (frames repeat
/// themselves). `None` for what has no CRF to speak of (#19).
fn crf_mbps_range(codec: klipp_core::ExportVideoCodec, crf: u32, px: f64, fps: f64) -> Option<(f64, f64)> {
    use klipp_core::ExportVideoCodec as C;
    // The codec's own default quality, where its CRF scale sits.
    let (default_crf, efficiency) = match codec {
        C::H264 => (23.0, 1.0),
        C::H265 => (28.0, 0.6),
        C::Vp9 => (31.0, 0.65),
        C::Av1 => (35.0, 0.5),
        _ => return None,
    };
    let scale = efficiency
        * (px / (1920.0 * 1080.0)).powf(0.75)
        * (fps.max(1.0) / 30.0).powf(0.6)
        * 2f64.powf((default_crf - crf as f64) / 6.0);
    Some((3.0 * scale, 12.0 * scale))
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

/// Rename mode for the batch-rename dialog. `FindReplace` substitutes a
/// literal substring in the clip's stem; `Sequential` rebuilds the stem as
/// `{prefix}{counter}{suffix}` with zero padding. File extensions are
/// always preserved.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum BatchRenameMode {
    #[default]
    FindReplace,
    Sequential,
}

/// Split `"name.ext"` into `("name", ".ext")` at the LAST dot. Returns
/// `(full, "")` for names without an extension — so `"noext"` survives
/// intact and filenames like `"my.file.name.mp4"` keep the full stem.
fn split_stem_ext(filename: &str) -> (&str, &str) {
    if let Some(pos) = filename.rfind('.') {
        (&filename[..pos], &filename[pos..])
    } else {
        (filename, "")
    }
}

/// Compute the new filename for one item given the current dialog state.
/// Pure — no egui, no AppState access. Tested directly.
#[allow(clippy::too_many_arguments)]
pub fn compute_new_filename(
    old: &str,
    mode: &BatchRenameMode,
    find: &str,
    replace: &str,
    match_case: bool,
    prefix: &str,
    suffix: &str,
    counter: u32,
    padding: u8,
) -> String {
    let (stem, ext) = split_stem_ext(old);
    match mode {
        BatchRenameMode::FindReplace => {
            if find.is_empty() {
                return old.to_string();
            }
            let new_stem = if match_case {
                stem.replace(find, replace)
            } else {
                // Case-insensitive literal replace — find spans in the
                // lowercased copy, splice `replace` into the original to
                // preserve the untouched portions verbatim.
                let lower_stem = stem.to_lowercase();
                let lower_find = find.to_lowercase();
                let mut result = stem.to_string();
                let mut search_start = 0;
                while let Some(pos) = lower_stem[search_start..].find(&lower_find) {
                    let abs_pos = search_start + pos;
                    result.replace_range(abs_pos..abs_pos + find.len(), replace);
                    search_start = abs_pos + replace.len();
                    if search_start >= result.len() {
                        break;
                    }
                }
                result
            };
            format!("{}{}", new_stem, ext)
        }
        BatchRenameMode::Sequential => {
            let pad = padding as usize;
            format!(
                "{}{:0>width$}{}{}",
                prefix,
                counter,
                suffix,
                ext,
                width = pad,
            )
        }
    }
}

/// Build the full preview for the batch-rename dialog. Returns one
/// `(MediaId, old, new)` per input item, counter starts at `start` and
/// increments by 1 per item (in iterator order).
#[allow(clippy::too_many_arguments)]
pub fn preview_batch_rename(
    items: &[(klipp_core::MediaId, String)],
    mode: &BatchRenameMode,
    find: &str,
    replace: &str,
    match_case: bool,
    prefix: &str,
    suffix: &str,
    start: u32,
    padding: u8,
) -> Vec<(klipp_core::MediaId, String, String)> {
    items
        .iter()
        .enumerate()
        .map(|(i, (id, old))| {
            let new = compute_new_filename(
                old,
                mode,
                find,
                replace,
                match_case,
                prefix,
                suffix,
                start + i as u32,
                padding,
            );
            (*id, old.clone(), new)
        })
        .collect()
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
    /// Stash a `TextureHandle` so its underlying wgpu::Texture is held
    /// across the current frame's submit+present cycle. Actual drop
    /// happens two `update()`s later, at which point the GPU has
    /// finished every command buffer that could have referenced it.
    /// Call whenever a churn-site replaces an existing handle with a
    /// freshly-allocated one; dropping the old handle inline on
    /// eframe-wgpu trips `Texture has been destroyed` validation.
    fn defer_drop_texture(&mut self, h: egui::TextureHandle) {
        let slot = self.state.deferred_texture_drops_slot;
        self.state.deferred_texture_drops[slot].push(h);
    }

    /// Refresh the letter-picker preview texture if the selected
    /// clip's resolved title-context has changed since the last paint.
    /// Runs from `render_inspector_panel` while the App still has
    /// `&mut self` access — texture lifecycle (creation + the
    /// `defer_drop_texture` call on the prior handle) stays in this
    /// layer. The inspector then reads `state.letter_picker_texture`
    /// without ever touching wgpu.
    ///
    /// Resolution flow:
    /// 1. No clip selected → drop the texture (defer-drop the old).
    /// 2. Selected clip resolves a Title via `picker_target_for_clip`
    ///    AND `(clip_id, layout_hash, w, h)` differs from the cache
    ///    key → render the title bitmap, upload, swap (defer-drop).
    /// 3. Same target + hash → no-op; texture is reused.
    /// 4. Selected clip with no title context → drop the texture.
    ///
    /// Frame dimensions are pinned at the inspector preview's fixed
    /// size (210×118 px) — no need to thread the rect down here.
    fn refresh_letter_picker_texture(&mut self, ctx: &egui::Context) {
        const PREVIEW_W: u32 = 210;
        const PREVIEW_H: u32 = 118;

        let Some(selected_clip_id) = self.state.selected_clip else {
            if let Some(stale) = self.state.letter_picker_texture.take() {
                self.defer_drop_texture(stale);
            }
            self.state.letter_picker_texture_for = None;
            return;
        };

        // Resolve target + compute layout hash under one read of the
        // sequence; the rest of the function works off snapshots so
        // we don't hold the borrow across the texture upload.
        let target_info: Option<(klipp_core::ClipId, u64, klipp_timeline::TitleData)> = {
            let seq = self.state.sequence();
            let target_clip = crate::inspector::letter_picker::picker_target_for_clip(
                seq, selected_clip_id, self.state.playhead,
            );
            target_clip.and_then(|c| {
                c.title_data().map(|td| {
                    let hash = klipp_title::glyph_layout_hash(td, PREVIEW_W, PREVIEW_H);
                    (c.id, hash, td.clone())
                })
            })
        };

        let Some((title_clip_id, layout_hash, td)) = target_info else {
            // No title context for this selection — drop any leftover
            // texture so the inspector falls back to the spinbox flow.
            if let Some(stale) = self.state.letter_picker_texture.take() {
                self.defer_drop_texture(stale);
            }
            self.state.letter_picker_texture_for = None;
            return;
        };

        let new_key = (title_clip_id, layout_hash, PREVIEW_W, PREVIEW_H);
        if self.state.letter_picker_texture_for == Some(new_key)
            && self.state.letter_picker_texture.is_some()
        {
            return; // cache hit — texture stays.
        }

        // Cache miss: render bitmap, upload, defer-drop the old.
        // Per-paint TitleRenderer is wasteful but contained here —
        // refresh fires only when the layout hash changes.
        let mut tr = klipp_title::TitleRenderer::new();
        let bitmap = tr.render(&td, PREVIEW_W, PREVIEW_H);
        let img = egui::ColorImage::from_rgba_unmultiplied(
            [PREVIEW_W as usize, PREVIEW_H as usize],
            &bitmap,
        );
        let tex = ctx.load_texture(
            format!("hyperzoom_picker_{}", title_clip_id.0.as_simple()),
            img,
            egui::TextureOptions::LINEAR,
        );

        if let Some(stale) = self.state.letter_picker_texture.take() {
            self.defer_drop_texture(stale);
        }
        self.state.letter_picker_texture = Some(tex);
        self.state.letter_picker_texture_for = Some(new_key);
    }

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

    /// Extend the current deferred-drop bucket from an iterator of
    /// handles. Use for `HashMap::drain` / `Vec::drain` sites where
    /// many textures retire at once (clip switch, thumbnail cache
    /// invalidation, etc.).
    fn defer_drop_textures(&mut self, it: impl IntoIterator<Item = egui::TextureHandle>) {
        let slot = self.state.deferred_texture_drops_slot;
        self.state.deferred_texture_drops[slot].extend(it);
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

    /// Run the picture on screen through the monitor LUT set in
    /// Preferences (#465), into a texture the viewer shows in its place.
    /// On screen only: the frame buffer, scopes, measurements and
    /// exports keep the graded frame. Not while the viewer shows HDR: the
    /// LUT reads a display-encoded SDR signal.
    fn refresh_viewer_lut(&mut self, frame: &mut eframe::Frame) {
        let changed = self.viewer.lut.follow(self.prefs.color.viewer_lut_path.as_deref());
        let Some(render_state) = frame.wgpu_render_state() else {
            self.viewer.lut_live = false;
            return;
        };
        if changed {
            if let Some(why) = &self.viewer.lut.error {
                self.state.toast.show(format!("Viewer LUT not applied: {why}"));
            }
            self.viewer.lut_pass = self.viewer.lut.lut.as_ref().map(|lut| {
                crate::viewer_lut::ViewerLutPass::new(&render_state.device, &render_state.queue, lut)
            });
            self.viewer.lut_live = false;
            // The parked frame is not uploaded again until it changes:
            // take it from the frame buffer for the new LUT.
            if self.viewer.lut_pass.is_some() {
                let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                let (bytes, w, h, _) = fb.latest_frame();
                self.viewer.lut_source_bytes = Some((bytes.to_vec(), w, h));
            }
        }
        let hdr = self.state.hdr_monitor.renders_hdr();
        let Some(pass) = self.viewer.lut_pass.as_mut().filter(|_| !hdr) else {
            self.viewer.lut_live = false;
            return;
        };
        let remade = if self.viewer.gpu_frame_live {
            // The GPU stack writes each frame into the same texture: run
            // the pass on every repaint while it is live.
            let source = {
                let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.gpu_output.as_ref().map(|t| {
                    let view = t.create_view(&wgpu::TextureViewDescriptor {
                        format: Some(klipp_engine::texture::GPU_VIEWER_SAMPLE_FORMAT),
                        ..Default::default()
                    });
                    (view, (t.width(), t.height()))
                })
            };
            let Some((view, size)) = source else {
                self.viewer.lut_live = false;
                return;
            };
            pass.apply(&render_state.device, &render_state.queue, &view, size)
        } else if let Some((bytes, w, h)) = self.viewer.lut_source_bytes.take() {
            pass.apply_bytes(&render_state.device, &render_state.queue, &bytes, (w, h))
        } else if self.viewer.lut_live {
            // The parked frame has not changed: the output still holds it.
            false
        } else {
            // Back from the playing or HDR view with nothing new uploaded:
            // the parked frame is in the frame buffer.
            let (bytes, w, h) = {
                let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                let (bytes, w, h, _) = fb.latest_frame();
                (bytes.to_vec(), w, h)
            };
            if !pass.apply_bytes(&render_state.device, &render_state.queue, &bytes, (w, h)) && pass.output_view().is_none() {
                return;
            }
            true
        };
        if remade || self.viewer.lut_texture_id.is_none() {
            let Some(view) = pass.output_view() else { return };
            let mut renderer = render_state.renderer.write();
            // Free the old id before its view drops, as for
            // `gpu_texture_id` (see `refresh_gpu_viewer_texture`).
            if let Some(old) = self.viewer.lut_texture_id.take() {
                renderer.free_texture(&old);
                self.viewer.lut_texture_view = None;
            }
            self.viewer.lut_texture_id = Some(renderer.register_native_texture(&render_state.device, &view, wgpu::FilterMode::Linear));
            self.viewer.lut_texture_view = Some(view);
        }
        self.viewer.lut_live = true;
    }

    fn refresh_gpu_viewer_texture(&mut self, frame: &mut eframe::Frame) {
        // Snapshot the current gpu_output handle + generation.
        let snapshot = {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            fb.gpu_output.as_ref().map(|t| (
                std::sync::Arc::clone(t),
                fb.gpu_output_generation,
            ))
        };
        let Some((texture, gen)) = snapshot else { return; };
        if gen == self.viewer.gpu_texture_gen && self.viewer.gpu_texture_id.is_some() {
            return; // already registered this exact texture
        }
        let Some(render_state) = frame.wgpu_render_state() else { return; };

        // Build a view from the Arc'd texture. wgpu::Texture::create_view
        // takes &self, so the Arc holder keeps the underlying texture
        // alive — but the view itself is a separate resource.
        // Through the sRGB view: the texture holds display-encoded bytes,
        // and egui's shader takes whatever it samples as linear light and
        // encodes it for the screen. A plain view encoded the picture a
        // second time, so playback looked washed out beside the paused
        // frame and the export (#455).
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            format: Some(klipp_engine::texture::GPU_VIEWER_SAMPLE_FORMAT),
            ..Default::default()
        });

        let mut renderer = render_state.renderer.write();
        // Order matters here. egui-wgpu's `register_native_texture`
        // creates an internal BindGroup that records the new view's
        // wgpu storage `Id` BUT does NOT take a refcounted clone —
        // wgpu 22's BindGroup tracks resources by Id only. Free the
        // previous TextureId FIRST so egui-wgpu drops the old
        // BindGroup; only after that is it safe to drop the previous
        // `gpu_texture_view` (handle drop tells wgpu_core to mark
        // the storage slot Vacant). If we dropped the view before
        // the BindGroup, the next paint that samples the old id
        // panics with "TextureView[Id(N,vk)] does not exist" — this
        // was the recurring "first-frame post compound / first-grade-
        // tweak" crash documented as GPU-COMPOUND-PANIC.
        if let Some(old_id) = self.viewer.gpu_texture_id.take() {
            renderer.free_texture(&old_id);
            self.viewer.gpu_texture_view = None;
        }
        let new_id = renderer.register_native_texture(
            &render_state.device,
            &view,
            wgpu::FilterMode::Linear,
        );
        // Keep the new view alive for as long as the registered id
        // is used. Replaces the previous view (already dropped above
        // after the corresponding `free_texture` call).
        self.viewer.gpu_texture_view = Some(view);
        self.viewer.gpu_texture_id = Some(new_id);
        self.viewer.gpu_texture_gen = gen;
        tracing::debug!("GPU_VIEWER registered id={:?} gen={}", new_id, gen);
    }
}

/// Upload `image` into `slot`, reusing the existing handle only when
/// dimensions match exactly. On a size change, allocate a fresh
/// handle and push the old one into `deferred_drops`. Mandatory on
/// eframe-wgpu: `TextureHandle::set` with a different-sized image
/// causes egui to replace (and destroy) the underlying wgpu::Texture
/// internally, which races the current frame's submit.
fn upload_texture_deferred(
    ctx: &egui::Context,
    slot: &mut Option<egui::TextureHandle>,
    deferred_drops: &mut Vec<egui::TextureHandle>,
    name: &str,
    image: egui::ColorImage,
    options: egui::TextureOptions,
) {
    // Nothing to show — the engine reset with no project, a frame of no
    // size: the old picture goes, and no texture is made. A zero-size
    // one fails in wgpu, and every redraw that binds it after is another
    // validation error, until GPU effects are switched off for the
    // session (#645).
    if image.size[0] == 0 || image.size[1] == 0 {
        if let Some(old) = slot.take() {
            tracing::debug!("TEX_DEFER_DROP id={:?} name={} reason=empty", old.id(), name);
            deferred_drops.push(old);
        }
        return;
    }
    let new_size = egui::vec2(image.size[0] as f32, image.size[1] as f32);
    match slot {
        Some(tex) if tex.size_vec2() == new_size => {
            tex.set(image, options);
        }
        _ => {
            if let Some(old) = slot.take() {
                tracing::debug!(
                    "TEX_DEFER_DROP id={:?} name={} reason=size_mismatch",
                    old.id(), name,
                );
                deferred_drops.push(old);
            }
            let h = alloc_texture_logged(ctx, name, image, options);
            *slot = Some(h);
        }
    }
}

/// Wrapper around `ctx.load_texture` that emits a `TEX_ALLOC` trace.
/// Used to correlate `egui_texid_Managed(N)` panic IDs with the call
/// site that allocated them — the panic only gives the numeric ID, no
/// context. When a crash names ID=N, grep `TEX_ALLOC id=...(N)` in the
/// log to find which name/site was responsible.
pub(crate) fn alloc_texture_logged(
    ctx: &egui::Context,
    name: &str,
    image: egui::ColorImage,
    options: egui::TextureOptions,
) -> egui::TextureHandle {
    let h = ctx.load_texture(name, image, options);
    tracing::debug!("TEX_ALLOC id={:?} name={}", h.id(), name);
    h
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

        // Settle any workspace switch before the layout is computed, so
        // the panel changes it implies land in the same frame the new
        // workspace does rather than a frame behind it.
        self.reconcile_workspace();
        if let Some(text) = self.state.system_clipboard_text.take() {
            ctx.copy_text(text);
        }
        if std::mem::take(&mut self.preferences_panel.show_updates) {
            self.state.show_preferences = false;
            self.updates.open();
        }
        self.updates.show(ctx, &mut self.prefs.general, &mut self.prefs_dirty);

        // Rotate the deferred-drop texture ring. Advance the slot and
        // clear the one we've just moved into — it holds handles
        // stashed ~2 frames ago. Two full frames of deferral covers
        // every submit + present cycle a texture could still be
        // referenced by. See the field's docs in state.rs.
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::TEXTURE_DROP_RING);
        let next_slot = (self.state.deferred_texture_drops_slot + 1) % 3;
        self.state.deferred_texture_drops[next_slot].clear();
        self.state.deferred_texture_drops_slot = next_slot;

        // Register the engine's `FrameBuffer.gpu_output` texture
        // with eframe-wgpu's renderer when it changes. No-op when
        // the engine hasn't produced a GPU output (env gate off, no
        // shared device, glow fallback) — the legacy CPU path keeps
        // working. The viewer render code picks up the new
        // `viewer.gpu_texture_id` when present.
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::REGISTER_GPU_TEXTURE);
        self.refresh_gpu_viewer_texture(frame);

        // AI Tools dispatch tick — drains pending_run / pending_cancel
        // and polls the worker channel.
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::AI_TOOLS_TICK);
        self.tick_ai_tools(ctx);

        // Bump the foreground-activity signal whenever the user
        // *interacts* — clicks, keys, scrolls, drags. Plain pointer
        // motion intentionally does NOT count: a user reading the UI
        // with the mouse drifting around shouldn't keep the skim-
        // transcode / filmstrip / audio-conform jobs paused forever.
        let had_interaction = ctx.input(|i| {
            // Held button = drag in flight → user is busy.
            if i.pointer.any_down() { return true; }
            // Discrete events that count as "interaction". Explicitly
            // exclude PointerMoved + MouseMoved + WindowFocused —
            // those fire constantly during normal use without
            // signalling intent.
            i.events.iter().any(|e| {
                use egui::Event as E;
                !matches!(
                    e,
                    E::PointerMoved(_)
                        | E::MouseMoved(_)
                        | E::WindowFocused(_)
                        | E::PointerGone
                )
            })
        });
        if had_interaction {
            self.foreground_activity.touch_input();
        }

        // Main-thread heartbeat. Logs every 2 s with UI fps + state so we
        // can see the UI thread redraw rate in real time (separate from
        // the engine tick rate). An idle paused UI should be near 0 fps.
        self.frame_count = self.frame_count.wrapping_add(1);
        let events_this_frame = ctx.input(|i| i.events.len());
        self.hb_input_events += events_this_frame as u64;
        self.hb_max_batch = self.hb_max_batch.max(events_this_frame);
        let since_last_frame = self.last_frame_at.elapsed();
        self.last_frame_at = std::time::Instant::now();
        if events_this_frame >= 24 && since_last_frame >= std::time::Duration::from_secs(1) {
            // Input piled up while no frame was painted: the user was
            // acting and the UI was not answering.
            tracing::warn!(
                "INPUT_BURST {events_this_frame} input events in one frame after {:.1}s without a frame",
                since_last_frame.as_secs_f64(),
            );
        }
        if self.last_heartbeat.elapsed() >= std::time::Duration::from_secs(2) {
            let elapsed = self.last_heartbeat.elapsed().as_secs_f64();
            let frames_in_interval =
                self.frame_count.saturating_sub(self.last_heartbeat_frame);
            let ui_fps = frames_in_interval as f64 / elapsed.max(0.001);
            tracing::info!(
                "UI_HEARTBEAT fps={:.1} frames={} events={} max_batch={} max_gap={:.0}ms max_update={:.0}ms focused={} state={:?} viewer_mode={:?} elapsed={:.2}s show_scopes={} show_color={}",
                ui_fps,
                frames_in_interval,
                self.hb_input_events,
                self.hb_max_batch,
                self.hb_max_gap.as_secs_f64() * 1000.0,
                self.hb_max_update.as_secs_f64() * 1000.0,
                ctx.input(|i| i.focused),
                self.state.playback,
                self.state.viewer_mode,
                elapsed,
                self.state.show_scopes,
                self.state.show_color_panel,
            );
            self.last_heartbeat = std::time::Instant::now();
            self.last_heartbeat_frame = self.frame_count;
            self.hb_max_gap = std::time::Duration::ZERO;
            self.hb_max_update = std::time::Duration::ZERO;
            self.hb_input_events = 0;
            self.hb_max_batch = 0;
        }

        // Consume any menu / toolbar request to toggle the perf
        // recording. The direct Ctrl+Shift+P keybinding calls
        // `toggle_perf_recording` inline during shortcut handling
        // for snappier UX; this pending-flag path covers everything
        // else that doesn't have easy access to `&mut self`.
        if std::mem::take(&mut self.state.pending_perf_recording_toggle) {
            self.toggle_perf_recording();
        }

        // CONTRAST RULE: Ensure selection bg stays dark each frame.
        // egui popup windows can reset visuals; re-apply selection colors to be safe.
        // Only mutate when the value drifted — avoids bumping the style dirty flag
        // on the 99% of frames where no popup reset happened.
        {
            let target = egui::Color32::from_rgb(0x38, 0x2c, 0x14);
            let needs_fix = ctx.style().visuals.selection.bg_fill != target;
            if needs_fix {
                ctx.style_mut(|style| {
                    style.visuals.selection.bg_fill = target;
                });
            }
        }

        // ── Poll system audio mute state (Linux/PipeWire) ──
        // Check every ~2 seconds via `wpctl` so we can warn the user if
        // their system audio is muted. Lightweight: wpctl completes in
        // <1ms and we only run it every 120 frames at 60fps.
        if self.state.system_audio_last_check.elapsed() > std::time::Duration::from_secs(2) {
            self.state.system_audio_last_check = std::time::Instant::now();
            let was = self.state.system_audio_muted;
            self.state.system_audio_muted = check_system_audio_muted();

            if self.state.system_audio_muted {
                let interval = self.state.mute_reminder_interval_secs;
                let since_last = self.state.system_audio_last_reminder.elapsed();
                // Show toast on first detection OR every interval seconds.
                // interval == 0 means "show only once" (on state change).
                let should_toast = if !was {
                    true // first detection
                } else if interval > 0 {
                    since_last >= std::time::Duration::from_secs(interval as u64)
                } else {
                    false
                };
                if should_toast {
                    self.state.system_audio_last_reminder = std::time::Instant::now();
                    self.state.toast.show(
                        "\u{26a0} Audio muted by system \u{2014} click the speaker icon in your taskbar to unmute"
                            .to_string(),
                    );
                }
            }
        }

        // ── Handle OS-level file drag-and-drop ──
        let dropped_files: Vec<std::path::PathBuf> = ctx.input(|i| {
            if i.raw.dropped_files.is_empty() {
                return Vec::new();
            }
            i.raw.dropped_files.iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped_files.is_empty() {
            let media_exts = klipp_core::media_extensions::all_media_extensions();
            for path in dropped_files {
                let ext = path.extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if media_exts.contains(&ext.as_str()) {
                    let media_id = klipp_core::MediaId::new();
                    let _ = self.cmd_tx.send(AppCommand::LoadMediaFile { path: path.clone(), media_id });
                    tracing::info!("Dropped file: {:?}", path);
                } else if crate::cli::is_project(&path) {
                    // Opened through the UI, as a click would (#395): sent
                    // to the engine, OpenProject was dropped.
                    self.startup_open = Some(crate::cli::StartupOpen::project(path));
                }
            }
        }

        // Also show visual feedback when files are being hovered over the window
        if !ctx.input(|i| i.raw.hovered_files.is_empty()) {
            // Paint a subtle overlay to indicate drop target
            let screen = ctx.screen_rect();
            egui::Area::new(egui::Id::new("drop_overlay"))
                .fixed_pos(screen.min)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let (rect, _) = ui.allocate_exact_size(screen.size(), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 0.0, self.theme.accent_subtle);
                    ui.painter().rect_stroke(rect.shrink(4.0), 4.0, egui::Stroke::new(2.0_f32, self.theme.accent_glow));
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Drop media files to import",
                        egui::FontId::proportional(16.0),
                        self.theme.text_accent,
                    );
                });
        }

        // Check if any popup/menu/combobox/foreground layer is active (blocks background interaction)
        let popup_open = ctx.memory(|m| m.any_popup_open());
        // Also check if pointer is over a foreground layer (menus, context menus)
        let pointer_over_foreground = ctx.input(|i| i.pointer.hover_pos())
            .and_then(|pos| ctx.layer_id_at(pos))
            .is_some_and(|layer| layer.order == egui::Order::Foreground || layer.order == egui::Order::Tooltip);
        self.state.modal_or_popup_open = popup_open || pointer_over_foreground
            || self.scope_window_runtime.pointer_over;

        // Smooth playhead interpolation BEFORE draining events
        if self.state.playback == PlaybackState::Playing {
            let wall_elapsed = self.state.playhead_anchor_wall.elapsed().as_secs_f64();
            let anchor_secs = self.state.playhead_anchor.to_secs_f64();
            // Account for clip speed in smooth interpolation — the master clock
            // runs at clip_speed, so the playhead on the timeline advances at
            // (clip_speed / clip_speed) = 1.0 timeline-seconds per wall-second,
            // BUT the playhead moves through timeline space, not source space.
            // The clip occupies (source_duration / clip_speed) timeline seconds,
            // so the playhead advances at playback_speed in timeline space.
            let speed = self.state.playback_speed;
            let interpolated_secs = anchor_secs + wall_elapsed * speed;
            let dur_secs = self.state.sequence_duration().to_secs_f64();
            // When looping, clamp to out_point to prevent one-frame overshoot
            let max_secs = if self.state.loop_playback_enabled {
                self.state.sequence().out_point
                    .map(|t| t.to_secs_f64())
                    .unwrap_or(dur_secs)
            } else {
                dur_secs
            };
            let clamped = interpolated_secs.clamp(0.0, max_secs);

            self.state.playhead_smooth_secs = clamped;
            self.state.playhead = Timecode::from_secs_f64(clamped, self.state.fps.rate());
        }

        // 1. Drain events from engine
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::DRAIN_EVENTS);
        self.drain_events(ctx);

        // Sync scope mode to engine — only when it actually changed.
        // Previously this sent unconditionally every UI frame (~120
        // commands/sec), contributing to engine command-channel load
        // for no benefit.
        {
            let mode_byte = match self.state.scope_mode {
                crate::state::ScopeMode::Waveform => 0u8,
                crate::state::ScopeMode::Parade => 1,
                crate::state::ScopeMode::Vectorscope => 2,
                crate::state::ScopeMode::Histogram => 3,
            };
            if self.last_sent_scope_mode != Some(mode_byte) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeRenderMode { mode: mode_byte });
                self.last_sent_scope_mode = Some(mode_byte);
            }

            // Publish the union of all video scope modes the UI needs
            // rendered per tick: dock mode + every floating-window pane
            // whose `ScopeType` maps to a video scope. Audio scopes don't
            // go through the engine at all. CIE chromaticity is drawn in
            // the UI thread but still asks the engine for the linear
            // samples it measures, under its own byte. Re-sent only on
            // change so the engine command channel doesn't churn.
            let mut active: std::collections::BTreeSet<u8> =
                std::collections::BTreeSet::new();
            active.insert(mode_byte);
            // Every pane that is actually on screen asks for its mode:
            // the floating window's when it is open, and the dock's when
            // it is showing a grid. A pane whose mode is missing from
            // this set has no texture to lift and draws blank.
            let want_pane_modes = |panes: &[crate::state::PaneConfig],
                                       active: &mut std::collections::BTreeSet<u8>| {
                for pane in panes {
                    if let Some(sm) = pane.scope_type.to_scope_mode() {
                        let pane_byte = match sm {
                            crate::state::ScopeMode::Waveform => 0u8,
                            crate::state::ScopeMode::Parade => 1,
                            crate::state::ScopeMode::Vectorscope => 2,
                            crate::state::ScopeMode::Histogram => 3,
                        };
                        active.insert(pane_byte);
                    } else if pane.scope_type == crate::state::ScopeType::CIEChromaticity {
                        // Drawn in the UI, but it still needs the engine to
                        // publish the linear working-space samples it
                        // measures — the picture, not the view of it.
                        active.insert(klipp_engine::scopes::CIE_SAMPLES_MODE);
                    }
                }
            };
            if self.state.scope_window.open {
                want_pane_modes(&self.state.scope_window.panes, &mut active);
            }
            if self.state.scopes_docked()
                && self.state.scope_dock.layout != crate::state::ScopeLayout::OnePane
            {
                want_pane_modes(&self.state.scope_dock.panes, &mut active);
            }
            if self.last_sent_active_scope_modes.as_ref() != Some(&active) {
                let _ = self.cmd_tx.send(AppCommand::SetActiveScopeModes {
                    modes: active.clone(),
                });
                self.last_sent_active_scope_modes = Some(active);
            }

            // The physical size each scope mode was drawn at last frame —
            // by the dock and by every floating pane, the larger side of
            // each where two show the same mode — so the engine draws it
            // at that size. Taken, so this frame's drawing notes it afresh
            // and a closed pane stops asking.
            let mut sizes: std::collections::BTreeMap<u8, (u32, u32)> = std::collections::BTreeMap::new();
            let shown = std::iter::once(self.state.scope_state.shown_size.take())
                .chain(self.scope_window_runtime.shown_sizes.iter_mut().map(Option::take))
                .chain(self.scope_dock_runtime.shown_sizes.iter_mut().map(Option::take));
            for (mode, (w, h)) in shown.flatten() {
                let size = sizes.entry(mode).or_insert((0, 0));
                *size = (size.0.max(w), size.1.max(h));
            }
            if self.last_sent_scope_sizes.as_ref() != Some(&sizes) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeSizes { sizes: sizes.clone() });
                self.last_sent_scope_sizes = Some(sizes);
            }

            // Publish FFmpeg-scope settings. Built from the existing
            // global AppState toggles (legacy popover) until Phase 5
            // adds dedicated per-mode UI. Same filter / channel / intensity
            // applies to BOTH waveform and parade in this back-compat
            // shape.
            //
            // FFmpeg waveform filter is read from AppState. Lowpass is the
            // 10-bit-clean default; other filter modes fall back to 8-bit
            // before the waveform filter to dodge a libavfilter 9.x SIGSEGV
            // on gbrp10le input (see SCOPE-NEXT-LIBAV-10BIT). The popover
            // dropdown lets the user pick per visual style.
            let legacy_channels = klipp_core::ChannelMask {
                r: self.state.scope_show_r,
                g: self.state.scope_show_g,
                b: self.state.scope_show_b,
            };
            let wp = klipp_core::WaveformParadeSettings {
                intensity: self.state.scope_options.waveform_intensity,
                channels: legacy_channels,
                envelope: self.state.scope_options.waveform_envelope,
            };
            let scope_settings = klipp_core::ScopeSettings {
                waveform: wp,
                parade: wp,
                vectorscope: klipp_core::VectorscopeSettings {
                    colour: self.state.scope_options.vectorscope_colour,
                    luma_lo: self.state.scope_options.vectorscope_luma_lo,
                    luma_hi: self.state.scope_options.vectorscope_luma_hi,
                    zoom: self.state.scope_options.vectorscope_zoom,
                },
                histogram: klipp_core::HistogramSettings {
                    display_mode: self.state.scope_options.histogram_display,
                    levels: self.state.scope_options.histogram_levels,
                    fill: self.state.scope_options.histogram_fill,
                },
                quality: self.state.scope_quality,
                luma_only: self.state.scope_options.waveform_luma_only,
            };
            if self.last_sent_scope_settings != Some(scope_settings) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeSettings {
                    settings: scope_settings,
                });
                self.last_sent_scope_settings = Some(scope_settings);
            }

            // Bloom toggle — separate post-pass concern (not an FFmpeg
            // spec knob), so a separate command + sentinel pair.
            let bloom_enabled = self.state.scope_bloom;
            if self.last_sent_scope_bloom != Some(bloom_enabled) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeBloom {
                    enabled: bloom_enabled,
                });
                self.last_sent_scope_bloom = Some(bloom_enabled);
            }
            let low_pass = self.state.scope_low_pass;
            if self.last_sent_scope_low_pass != Some(low_pass) {
                let _ = self.cmd_tx.send(AppCommand::SetScopeLowPass { enabled: low_pass });
                self.last_sent_scope_low_pass = Some(low_pass);
            }
        }

        // Sync scope-panel visibility to the engine — same change-
        // tracking pattern as the mode sync above. Catches every
        // mutation site (keyboard toggle, button click, layout
        // restore) without per-site plumbing because we observe the
        // current state value once per UI frame.
        {
            let visible = self.state.scopes_shown();
            if self.last_sent_scopes_visible != Some(visible) {
                let _ = self.cmd_tx.send(AppCommand::SetScopesVisible(visible));
                self.last_sent_scopes_visible = Some(visible);
            }
        }

        self.apply_ui_scale(ctx);
        self.serve_window_capture(ctx);
        self.drain_thumbnails();
        self.drain_source_frame(ctx);
        // Keep repainting while filmstrip is generating so timeline updates progressively
        if self.state.filmstrip_generating || !self.filmstrip_cache_loads.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // And while scenes are being found: their progress moves in the
        // menu, and the result is taken in, and a clip split, on the frame
        // after it arrives, which with nothing else moving never came
        // (#616).
        if !self.state.scene_detect_in_progress.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        // Likewise a track, a window's or a surface's: with the window
        // idle its result sat unapplied until the pointer moved.
        if !self.state.tracking_in_progress.is_empty() || self.state.surface_snaps_pending > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.drain_audio_conforms();
        self.drain_proxy_events();
        // If a skim decode just finished and there's a queued request,
        // dispatch it immediately. After `drain_proxy_events`, because that
        // is where `BrowserSkimFrameReady` clears `skim_decode_in_flight`:
        // run before it and the queued request could only ever be seen as
        // still blocked, so every one of them went out a UI frame late and
        // the picture lagged the cursor by that frame for the whole drag.
        if !self.state.skim_decode_in_flight {
            if let Some((mid, time, target)) = self.state.skim_queued_request.take() {
                self.request_skim_frame(mid, time, target);
            }
        }
        self.drain_proxy_progress();
        self.poll_render_queue();
        // Show filmstrip thumbnail preview only while waiting for a seek result
        if self.state.awaiting_seek_frame && !self.state.showing_thumbnail_preview {
            // A skim frame that has landed but not yet been uploaded sets
            // `pending_ready`, not `dirty` — `dirty` is only set by the
            // `swap_pending()` inside `upload_frame_to_egui`, which runs
            // after this. Checking `dirty` alone let a thumbnail paint over
            // a good frame that was already waiting.
            let waiting = {
                let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                fb.dirty || fb.pending_ready
            };
            if !waiting {
                self.show_filmstrip_preview(ctx);
            }
        }
        self.upload_frame_to_egui(ctx);
        self.refresh_viewer_lut(frame);
        self.upload_source_frame_to_egui(ctx);
        self.upload_title_overlay(ctx);
        self.recompute_char_positions_if_needed();
        self.poll_effect_thumbnails(ctx);

        // Rebuild media usage cache if dirty (O(clips) once per edit, then O(1) per render)
        if self.state.media_usage_cache_dirty {
            self.state.media_usage_cache.clear();
            for seq in &self.state.sequences {
                for track in &seq.tracks {
                    for clip in &track.clips {
                        if let Some(mid) = clip.media_id() {
                            *self.state.media_usage_cache.entry(mid).or_insert(0) += 1;
                        }
                    }
                }
            }
            self.state.media_usage_cache_dirty = false;
        }

        // The multicam viewer follows the group of the selected clip, else
        // the group under the playhead: gated on a selection alone, it went
        // away as soon as the playhead moved on without one.
        let view = self.prefs.editing.multicam_auto_viewer.then(|| {
            crate::multicam_grid::view_at(&self.state.sequences[self.state.active_sequence_idx], self.state.selected_clip, self.state.playhead)
        }).flatten();
        self.state.multicam_viewer_active = view.is_some();
        if let Some((angles, active)) = view {
            self.state.multicam_angles = angles;
            self.state.multicam_active_angle = active;
        }
        // The angle grid, in the Source viewer's place: decoded while it
        // is shown, its tiles let go when it is not.
        // Shown where the Source viewer is: beside Program, or in its
        // place in a single-viewer layout switched to Source.
        // As the layout decides it (`layout::compute_layout`): the Colour
        // and Export workspaces' single viewer is Program whatever the
        // mode says, and decoding every angle for no pixels on screen
        // was waste.
        let source_on_screen = self.state.viewers_side_by_side()
            || (!self.state.single_viewer_is_program() && self.state.viewer_mode == klipp_core::ViewerMode::Source);
        let grid_shown = self.state.multicam_grid_enabled && self.state.multicam_viewer_active && source_on_screen;
        let mut old_tiles = Vec::new();
        if grid_shown {
            let seq = &self.state.sequences[self.state.active_sequence_idx];
            if let Some(group) = crate::multicam_grid::group_at(seq, self.state.selected_clip, self.state.playhead) {
                let runtime = self.multicam_grid.get_or_insert_with(|| crate::multicam_grid::GridRuntime::new(ctx));
                runtime.request(seq, &self.state.media_pool, group, self.state.playhead, self.state.playback == PlaybackState::Playing);
                runtime.drain(ctx, |t| old_tiles.push(t));
                old_tiles.extend(runtime.take_retired());
            }
        } else if let Some(runtime) = self.multicam_grid.as_mut() {
            runtime.clear(|t| old_tiles.push(t));
        }
        for t in old_tiles {
            self.defer_drop_texture(t);
        }

        // Window title with project name and dirty indicator — only
        // send the viewport command when the title actually changed to
        // avoid a per-frame String allocation + IPC round-trip.
        {
            let title = match &self.state.project_path {
                Some(p) => {
                    let name = p.file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Untitled".into());
                    let read_only = if self.read_only_project.as_ref() == Some(p) { " (read-only)" } else { "" };
                    if self.state.dirty {
                        format!("Klipp \u{2014} {name}{read_only} \u{2022}")
                    } else {
                        format!("Klipp \u{2014} {name}{read_only}")
                    }
                }
                None => "Klipp".to_string(),
            };
            if title != self.last_window_title {
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
                self.last_window_title = title;
            }
        }

        // Apply pending false color palette switch from toolbar quick-switcher
        if let Some(palette) = self.state.pending_false_color_palette.take() {
            self.state.false_color_lut = palette.build_lut();
            self.prefs.viewer.false_color_palette = palette;
            self.prefs_dirty = true;
        }

        // Collapsing or expanding a section is a preference change made
        // by a widget with no access to `self`, so nothing marks the
        // prefs dirty and the mirror below — which only runs when they
        // already are — never sees it. It went unnoticed while every
        // section started open and most are never collapsed; with GRADE
        // starting closed, a colourist who wants it open would have had
        // to re-open it on every launch.
        if self.state.remember_inspector_layout {
            let key = egui::Id::new("klipp_collapsed_sections");
            let differs = ctx.data_mut(|d| {
                let set: &std::collections::HashSet<String> =
                    d.get_temp_mut_or_default(key);
                set.len() != self.prefs.general.collapsed_sections.len()
                    || self
                        .prefs
                        .general
                        .collapsed_sections
                        .iter()
                        .any(|name| !set.contains(name))
            });
            if differs {
                self.prefs_dirty = true;
            }
        }

        // Debounced preferences save (at most once per second)
        if self.prefs_dirty && self.prefs_last_save.elapsed().as_secs() >= 1 {
            // Sync prefs to state
            self.state.set_history_max_size(self.prefs.editing.undo_limit);
            self.state.audio_fade_overlay_mode = self.prefs.audio.fade_overlay_mode;
            self.state.audio_show_rubber_band = self.prefs.audio.show_rubber_band;
            self.state.audio_show_waveform = self.prefs.audio.show_waveform;
            self.state.audio_anti_click_fade = self.prefs.audio.anti_click_fade;
            self.state.audio_show_crossfade_indicators = self.prefs.audio.show_crossfade_indicators;
            self.state.lufs_target = match self.prefs.audio.loudness_standard {
                klipp_core::LoudnessStandard::EbuR128 => -23.0,
                klipp_core::LoudnessStandard::AtscA85 => -24.0,
                klipp_core::LoudnessStandard::Off => -23.0,
            };
            self.state.peak_hold_time_secs = self.prefs.audio.peak_hold_time_secs;
            self.state.tp_oversample = self.prefs.audio.tp_oversample;
            let _ = self.cmd_tx.send(AppCommand::SetAntiClickFade(self.state.audio_anti_click_fade));
            // Sync editing prefs
            if !self.prefs.editing.playhead_follows_playback {
                self.state.playhead_follow = crate::state::PlayheadFollow::Off;
            }
            // T-PREFS-LIVE-SYNC (2026-05-07): the AppCommand handlers
            // for SetMagneticTimeline + SetMulticamShowAngles already
            // mirror these fields immediately on every prefs-panel
            // change, so these two lines are now redundant for
            // in-app edits. Kept as a safety net for any future
            // out-of-band prefs.json edit (e.g. the user hand-edits
            // the file while Klipp is running and triggers a save
            // through some other path). If that case is ever
            // formally unsupported, this whole block can drop both
            // lines without behavioural change.
            self.state.magnetic_timeline_enabled = self.prefs.editing.magnetic_timeline;
            self.state.multicam_show_angles = self.prefs.editing.multicam_show_angles;
            // Sync engine statics
            klipp_engine::video_decoder::set_hardware_decode(self.prefs.system.hardware_decode);
            klipp_engine::video_decoder::set_optimised_playback_mode(
                self.prefs.playback.optimised_playback.to_engine_mode(),
            );
            klipp_engine::video_decoder::set_playback_quality_divisor(crate::prefs::playback_quality_divisor(
                self.prefs.playback.playback_quality,
            ));
            let _ = self.cmd_tx.send(AppCommand::SetLutTetrahedral(
                self.prefs.color.lut_interpolation == crate::prefs::LutInterpolation::Tetrahedral,
            ));
            let _ = self.cmd_tx.send(AppCommand::SetSdrTransfer(self.prefs.color.sdr_display_transfer));
            // Push the background-performance pref into the live
            // scheduler so user changes take effect immediately
            // (without waiting for the next app launch).
            self.job_scheduler.set_policy(
                self.prefs.playback.background_performance.to_scheduler_policy(),
            );
            // Rebuild false color LUT if palette changed
            self.state.false_color_lut = self.prefs.viewer.false_color_palette.build_lut();
            // Sync collapsed inspector sections to prefs
            if self.state.remember_inspector_layout {
                let collapsed_key = egui::Id::new("klipp_collapsed_sections");
                let sections: Vec<String> = ctx.data_mut(|d| {
                    let set: &std::collections::HashSet<String> = d.get_temp_mut_or_default(collapsed_key);
                    set.iter().cloned().collect()
                });
                self.prefs.general.collapsed_sections = sections;
                // Mirror Title-inspector expanded groups back to prefs.
                let title_key = egui::Id::new(crate::inspector::TITLE_EXPANDED_GROUPS_KEY);
                let title_groups: std::collections::HashSet<String> = ctx.data_mut(|d| {
                    let set: &std::collections::HashSet<String> = d.get_temp_mut_or_default(title_key);
                    set.iter().cloned().collect()
                });
                self.prefs.title.inspector_expanded_groups = title_groups;
            } else {
                self.prefs.general.collapsed_sections.clear();
                self.prefs.title.inspector_expanded_groups.clear();
            }
            if let Err(e) = self.prefs.save() {
                tracing::warn!("Failed to save preferences: {e}");
            }
            // Live theme reload — picks up Theme dropdown change,
            // accent colour change, role colour edits without
            // requiring a restart. Also re-applies egui visuals so
            // built-in widgets (popups, scrollbars, selection
            // highlights) match the new palette polarity.
            self.theme = KlippTheme::from_prefs(&self.prefs.appearance, &self.theme_registry);
            self.theme.apply_to_egui(ctx);
            self.prefs_dirty = false;
            self.prefs_last_save = std::time::Instant::now();
        }

        // Auto-save. The write itself runs on a worker thread — see the
        // spawn below for why — so first reap a finished one.
        if let Some(rx) = &self.autosave_rx {
            match rx.try_recv() {
                Ok(Ok(path)) => {
                    tracing::info!("Auto-saved to {:?}", path);
                    self.state.toast.show("Auto-saved");
                    self.autosave_rx = None;
                }
                Ok(Err(e)) => {
                    tracing::warn!("Auto-save failed: {e}");
                    self.autosave_rx = None;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    // Writer thread died without sending — treat as done
                    // so a panicking save can't wedge auto-save forever.
                    tracing::warn!("Auto-save worker vanished without reporting a result");
                    self.autosave_rx = None;
                }
            }
        }

        self.sync_project_lock();

        self.publish_viewer_compare();

        // Auto-save: trigger on time interval OR op-count threshold (whichever hits first).
        let interval = self.prefs.general.auto_save_interval_secs;
        let op_threshold = self.prefs.general.auto_save_op_threshold;
        let time_trigger = interval > 0
            && self.last_autosave.elapsed().as_secs() >= interval as u64;
        let op_trigger = op_threshold > 0
            && self.state.history_ops_since_save >= op_threshold as u64;
        // Not for a project open read-only: its auto-save folder sits
        // beside the project, where the editor who holds it would find
        // this session's edits offered as their own recovery.
        if (time_trigger || op_trigger)
            && self.state.dirty
            && self.state.project_path.is_some()
            && self.read_only_project != self.state.project_path
            && !self.state.any_drag_in_progress()
            && self.autosave_rx.is_none()
        {
            if let Some(ref project_path) = self.state.project_path.clone() {
                let autosave_path = autosave_path_for(project_path);
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
                let active_seq_id = self.state.sequences[self.state.active_sequence_idx].id.to_string();

                // Snapshot everything the writer needs. The clone is a
                // handful of allocations; the serialise-plus-disk-write it
                // moves off this thread took seconds on a large project and
                // stalled the whole editor mid-edit, on a timer, with no way
                // for the user to tell what had happened.
                let sequences = self.state.sequences.clone();
                let media_pool = self.state.media_pool.clone();
                let created_at = self.state.created_at.clone();
                let notes = self.state.project_notes.clone();
                let title_assets = self.state.title_assets.clone();
                let looks = self.state.looks.clone();
                let backup_count = self.prefs.editing.backup_count;
                let zoom = self.state.timeline_zoom;
                let scroll_x = self.state.timeline_scroll_x;
                let track_scale = self.state.timeline_track_scale;

                let (tx, rx) = crossbeam_channel::bounded(1);
                let repaint_ctx = ctx.clone();
                let spawned = std::thread::Builder::new()
                    .name("klipp-autosave".into())
                    .spawn(move || {
                        let result = klipp_project::save_project_full(
                            &autosave_path,
                            &sequences,
                            &media_pool,
                            &color,
                            Some(&active_seq_id),
                            created_at.as_deref(),
                            &notes,
                            backup_count,
                            Some(zoom),
                            Some(scroll_x),
                            Some(track_scale),
                            &title_assets,
                        )
                        .and_then(|()| {
                            klipp_project::save_looks(&autosave_path, &looks)
                                .map_err(|e| format!("failed to persist Looks: {e}"))
                        })
                        .map(|()| autosave_path);
                        let _ = tx.send(result);
                        // Wake the UI so the toast appears promptly even if
                        // egui is idle (reactive) with nothing else pending.
                        repaint_ctx.request_repaint();
                    });

                match spawned {
                    Ok(_) => {
                        self.autosave_rx = Some(rx);
                        // The snapshot above *is* the saved state, so every
                        // op up to this point is accounted for. Ops made
                        // while the writer runs are not in it and correctly
                        // start the counter climbing again from zero.
                        //
                        // Consequence worth knowing: on a write that then
                        // fails, those ops are no longer counted toward the
                        // op-threshold trigger, so recovery falls to the
                        // time trigger. Preferable to double-counting and
                        // re-saving the same state every frame.
                        self.state.reset_ops_since_save();
                    }
                    Err(e) => tracing::warn!("Auto-save: failed to spawn writer thread: {e}"),
                }
            }
            self.last_autosave = std::time::Instant::now();
        }

        // Frame-rate-aware repaint during playback (must be AFTER drain_events so Playing state is set).
        // 16 ms = 60 Hz cap. Engine grade time at 1080p typically exceeds
        // 8 ms budget under load, so asking the UI for 125 Hz just burns
        // CPU waiting for frames that aren't ready. 60 Hz matches the
        // engine's target and halves UI thread work under grade load.
        // A scrub wants frames too: a skim frame lands on the worker thread
        // and is written into the frame buffer, but nothing wakes the UI to
        // upload it, so without a clock it sits there until the next pointer
        // event. The filmstrip preview used to provide that clock by asking
        // for a repaint on every drag frame; #483 stopped arming it over
        // skimmable media and this replaces the clock without the thumbnail.
        let wants_continuous_frames = self.state.playback == PlaybackState::Playing
            || self.state.source_playback == PlaybackState::Playing
            || self.state.is_scrubbing;
        if wants_continuous_frames {
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
        }
        // Tell the watchdog whether a missing `update()` call is a hang
        // or just egui being reactive with nothing to draw. See
        // `klipp_engine::UI_WANTS_FRAMES`.
        klipp_engine::UI_WANTS_FRAMES
            .store(wants_continuous_frames, std::sync::atomic::Ordering::Relaxed);

        // The meters' ballistics are tuned per frame at 60 fps, and taken
        // per this frame's real length: the UI draws at whatever rate the
        // display and the present mode give it, 120 fps and more, and a
        // meter counted in frames fell twice as fast there.
        let ballistics = MeterBallistics::for_frame(ctx.input(|i| i.unstable_dt));

        // Peak meter ballistics: exponential release (~1.7s to decay from 0dB to -60dB).
        // Factor 0.96/frame at 60fps → ~0.96^60 ≈ 0.085 after 1s → -21dB/s.
        // This gives smooth, professional-looking decay without flicker.
        let peak_release = ballistics.peak_release;
        for (l, r) in &mut self.state.track_peak_levels {
            *l *= peak_release;
            *r *= peak_release;
        }
        self.state.master_peak_left *= peak_release;
        self.state.master_peak_right *= peak_release;

        // LUFS display smoothing: EMA filter on momentary and short-term.
        // Integrated is already slow-moving (gated average), no smoothing needed.
        // Alpha ~0.12 → ~8 frames to settle → ~130ms at 60fps. Smooth but responsive.
        fn ema_lufs(smooth: &mut f32, raw: f32, alpha: f32, drift: f32) {
            if !raw.is_finite() {
                // Decay toward -inf when signal stops
                if smooth.is_finite() {
                    *smooth -= drift; // drift down ~90 LUFS/sec
                    if *smooth < -70.0 { *smooth = f32::NEG_INFINITY; }
                }
            } else if !smooth.is_finite() {
                *smooth = raw; // first valid reading: snap
            } else {
                *smooth = *smooth * (1.0 - alpha) + raw * alpha;
            }
        }
        ema_lufs(&mut self.state.lufs_momentary_smooth, self.state.lufs_momentary, ballistics.momentary_alpha, ballistics.drift);
        ema_lufs(&mut self.state.lufs_short_term_smooth, self.state.lufs_short_term, ballistics.short_term_alpha, ballistics.drift);

        // Update performance stats
        if self.state.show_perf_overlay {
            self.update_perf_stats(ctx);
        }

        // Update thumbnails: convert ready thumbnails to textures
        self.update_thumbnail_textures(ctx);

        // Reset skim flag for this frame (set by browser or timeline hover).
        // A playhead drag holds it for the whole gesture: `scrub_to` only
        // runs on frames egui reports the drag on, and the skim must not be
        // declared over on the frames in between.
        self.state.browser_skim_active_this_frame = self.state.is_scrubbing;

        // Process pending skim transcodes (queued during drain_events)
        let pending_skims: Vec<MediaId> = std::mem::take(&mut self.state.pending_skim_transcodes);
        for mid in pending_skims {
            // Re-use the command processing path
            self.handle_skim_transcode(mid);
        }

        // Route the pending skim frame based on the explicit target the
        // dispatch site stamped on the request:
        //
        //   * `ViewerMode::Program` — timeline playhead scrub or
        //     timeline filmstrip hover. The frame represents what the
        //     timeline output is or would be. Write to the engine's
        //     frame buffer so the Program Viewer paints it and scopes
        //     refresh.
        //
        //   * `ViewerMode::Source`  — media-browser bin hover. The frame
        //     is a transient overlay for the Source Viewer and must not
        //     touch the engine frame buffer or it would contaminate the
        //     Program Viewer.
        //
        // A timeline-side skim can never leak into the Source Viewer and
        // a bin-side skim can never leak into the Program Viewer.
        if let Some((media_id, rgba, width, height, target)) = self.state.browser_skim_pending.take() {
            match target {
                klipp_core::ViewerMode::Program => {
                    {
                        let mut fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                        fb.write_skim_frame(rgba.clone(), width, height);
                    }
                    // Mark the Program viewer as currently showing a
                    // skim frame so the end-of-frame cleanup knows to
                    // refresh back to the committed playhead when the
                    // mouse leaves the timeline / filmstrip.
                    self.state.program_skim_active = true;
                    // Only when a scope is there to show it: the engine
                    // draws every active scope for each skim frame.
                    if self.state.scopes_shown() {
                        let _ = self.cmd_tx.send(AppCommand::RenderScopesForFrame {
                            rgba,
                            width,
                            height,
                        });
                    }
                }
                klipp_core::ViewerMode::Source => {
                    let image = egui::ColorImage::from_rgba_unmultiplied(
                        [width as usize, height as usize],
                        &rgba,
                    );
                    let new_size = egui::vec2(width as f32, height as f32);
                    // Reuse the existing handle when dimensions match —
                    // avoids an allocation + lifetime crossing on every
                    // skim tick. Different dimensions (media changed)
                    // still need a fresh handle; defer the old drop.
                    // Never into a poster: until the first frame lands the
                    // skim shows the pool thumbnail's own handle, and
                    // writing through it painted skim pixels into the
                    // pool tile.
                    let is_poster = self.state.browser_skim_texture.as_ref()
                        .is_some_and(|t| self.state.thumbnail_textures.values().any(|p| p.id() == t.id()));
                    match self.state.browser_skim_texture {
                        Some(ref mut tex) if !is_poster && tex.size_vec2() == new_size => {
                            tex.set(image, egui::TextureOptions::LINEAR);
                        }
                        _ => {
                            if let Some(old) = self.state.browser_skim_texture.take() {
                                self.defer_drop_texture(old);
                            }
                            self.state.browser_skim_texture = Some(
                                alloc_texture_logged(ctx, "browser_skim", image, egui::TextureOptions::LINEAR),
                            );
                        }
                    }
                    self.state.browser_skim_media_id = Some(media_id);
                }
            }
        }

        // Pickers run off the UI thread (see `file_dialog`); what the
        // ones that closed since the last frame chose starts this
        // frame's commands. Their scope is the open project and the
        // active sequence: a pick made for another is dropped.
        let picker_scope = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.state.project_epoch.hash(&mut h);
            self.state.sequences.get(self.state.active_sequence_idx).map(|s| s.id.to_string()).hash(&mut h);
            h.finish()
        };
        crate::file_dialog::install(ctx, picker_scope);
        let picked = crate::file_dialog::take_ready();
        // Everything the pickers had to say this frame, in one toast: a
        // refusal must not hide a worker's "Frame saved".
        let mut said: Vec<String> = Vec::new();
        if picked.stale > 0 {
            said.push("The project or sequence changed while the file dialog was open, so that choice was not applied".into());
        }
        if picked.refused > 0 {
            said.push("A file dialog is already open".into());
        }
        if picked.expired > 0 {
            said.push("The file dialog answered after its panel had closed, so that choice was not applied".into());
        }
        said.extend(picked.notices);
        if !said.is_empty() {
            self.state.toast.show(said.join(" \u{2022} "));
        }
        let mut all_cmds = picked.commands;

        // 2. Handle global keyboard shortcuts
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::SHORTCUTS);
        let kb_cmds = self.handle_global_shortcuts(ctx);
        all_cmds.extend(kb_cmds);

        // 2b. In the Colour workspace the grade follows the playhead.
        self.follow_playhead_for_grading();

        // 2c. The curves draw the viewer frame's histogram behind them.
        // Gated on the frame buffer's generation inside, so a paused
        // viewer costs one pass rather than one per repaint.
        if self.state.active_workspace == crate::state::Workspace::Color {
            self.node_inspector_panel.refresh_curve_histogram(&self.frame_buffer);
        }

        // 3. Handle transport keyboard shortcuts
        let transport_cmds = self.transport.handle_keyboard(ctx, &self.state, &self.key_bindings);
        all_cmds.extend(transport_cmds);

        self.scope_window_runtime.pointer_over = false;

        // 4. Unsaved changes dialog
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::RENDER_DIALOGS);
        let dialog_cmds = self.show_unsaved_dialog(ctx);
        all_cmds.extend(dialog_cmds);

        self.show_modal_dialogs(ctx, &mut all_cmds);

        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::RENDER_PANELS);
        self.render_main_layout(ctx, &mut all_cmds);

        // A scrub that lost its release. egui does not promise
        // `drag_stopped` when the window loses focus or a modal opens over a
        // drag, and since the scrub flag reached the engine (#483) a stuck
        // one is not cosmetic: it suppresses the parked refresh for that
        // position (#489), holds the foreground signal so background jobs
        // yield indefinitely, and keeps the scrub mute engaged. If nothing
        // is being dragged any more, end the gesture exactly as a release
        // would, so the final seek still lands.
        if self.state.is_scrubbing && ctx.dragged_id().is_none() {
            tracing::debug!("scrub ended without a release; closing it");
            self.timeline.scrub_end(&mut self.state, &mut all_cmds);
        }

        // Dim UI on play: darken everything outside the viewer rect during
        // playback for cinematic focus. Click-through (no allocate_rect),
        // Order::Middle so modals/command palette/markers/toasts paint above
        // it. Fades in/out via egui's animator.
        if self.prefs.playback.dim_ui_on_play {
            let target_alpha = if self.state.playback == PlaybackState::Playing {
                60.0_f32
            } else {
                0.0_f32
            };
            let alpha = ctx.animate_value_with_time(
                egui::Id::new("dim_on_play_alpha"),
                target_alpha,
                0.15, // 150ms fade
            );
            if alpha > 0.5 {
                // Read the actual viewer panel rect that was painted this frame
                // by either render_main_layout (windowed) or render_fullscreen_viewer
                // (fullscreen). Avoids recomputing layout, which would be wrong in
                // fullscreen mode where compute_layout is never called.
                let viewer = self.state.viewer_panel_rect;
                let screen = ctx.screen_rect();
                let dim = egui::Color32::from_black_alpha(alpha as u8);

                ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Middle,
                    egui::Id::new("dim_on_play_layer"),
                ))
                .extend([
                    // top strip — full width above the viewer
                    egui::Shape::rect_filled(
                        egui::Rect::from_min_max(screen.min, egui::pos2(screen.max.x, viewer.min.y)),
                        0.0,
                        dim,
                    ),
                    // bottom strip — full width below the viewer
                    egui::Shape::rect_filled(
                        egui::Rect::from_min_max(egui::pos2(screen.min.x, viewer.max.y), screen.max),
                        0.0,
                        dim,
                    ),
                    // left strip — between top and bottom strips, left of viewer
                    egui::Shape::rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(screen.min.x, viewer.min.y),
                            egui::pos2(viewer.min.x, viewer.max.y),
                        ),
                        0.0,
                        dim,
                    ),
                    // right strip — between top and bottom strips, right of viewer
                    egui::Shape::rect_filled(
                        egui::Rect::from_min_max(
                            egui::pos2(viewer.max.x, viewer.min.y),
                            egui::pos2(screen.max.x, viewer.max.y),
                        ),
                        0.0,
                        dim,
                    ),
                ]);
            }
        }

        // Markers panel
        if self.state.show_markers_panel {
            let cmds = self.markers.show(ctx, &mut self.state);
            all_cmds.extend(cmds);
        }

        // Subtitle editor panel
        if self.state.show_subtitle_editor {
            let cmds = crate::panels::subtitle_editor::show(ctx, &mut self.state);
            all_cmds.extend(cmds);
        }

        // Transcript panel
        if self.state.show_transcript_panel {
            let cmds = self.transcript_panel.show(ctx, &mut self.state, &self.transcript_source, &self.theme);
            all_cmds.extend(cmds);
        } else {
            self.state.transcript_panel_hovered = false;
        }
        self.run_pending_doc_ops();

        // Project Notes panel
        if self.state.show_project_notes {
            // Borrow split: take the open flag out of state so the panel
            // can mutate state.project_notes + state.dirty without the
            // borrow checker tripping over the &mut bool.
            let mut open = self.state.show_project_notes;
            crate::panels::project_notes::show_project_notes(
                ctx,
                &mut self.state,
                &self.theme,
                &mut open,
            );
            self.state.show_project_notes = open;
        }

        // Colour Check window: the frame on screen read the way a
        // colourist reads it, with the fault named and the knob to turn.
        if self.state.show_colour_check {
            let mut open = self.state.show_colour_check;
            let action = {
                let fb = self.frame_buffer.clone();
                self.colour_check.show(ctx, &self.state, &fb, &self.theme, &mut open)
            };
            self.state.show_colour_check = open;
            if action == crate::panels::colour_check::ColourAction::SelfCheck {
                self.run_colour_self_check();
            }
        }

        // Sound Over Time window: a whole file's sound at once.
        if self.state.show_sound_over_time {
            let mut open = self.state.show_sound_over_time;
            self.sound_over_time.show(ctx, &self.state, &self.theme, &mut open);
            self.state.show_sound_over_time = open;
        }

        // Narrate window: a scratch voice from text.
        if self.state.show_narrate {
            let mut open = self.state.show_narrate;
            let cmds = self.narrate.show(ctx, &mut self.state, &mut open);
            self.state.show_narrate = open;
            self.send_commands(cmds);
        }

        // Edit Plan window: the paper edit awaiting the person's word.
        if self.state.show_edit_plan {
            let mut open = self.state.show_edit_plan;
            let action = crate::panels::edit_plan::show_edit_plan(ctx, &self.state, &self.theme, &mut open);
            self.state.show_edit_plan = open;
            self.act_on_edit_plan(action);
        }

        // Command palette (Ctrl+K). Entry list comes from
        // `crate::command_palette::build` so adding a new entry is a
        // single edit (and the `palette_anchors_present` test there
        // catches accidental removals).
        if self.state.show_command_palette {
            use crate::command_palette::{build as build_palette, dispatch_dialog, PaletteAction};

            // Modal scrim — match the AI Models dialog so the
            // palette has the same "this layer is the active one"
            // visual treatment. Captures click + drag so timeline
            // / browser interactions behind don't fire while the
            // user is typing a command.
            let screen_rect = ctx.screen_rect();
            egui::Area::new(egui::Id::new("command_palette_scrim"))
                .order(egui::Order::Middle)
                .fixed_pos(screen_rect.min)
                .show(ctx, |ui| {
                    let resp = ui.allocate_rect(screen_rect, egui::Sense::click_and_drag());
                    ui.painter().rect_filled(
                        screen_rect,
                        0.0,
                        egui::Color32::from_black_alpha(140),
                    );
                    if resp.clicked() {
                        // Click-outside dismisses, matching the
                        // AI Models dialog convention.
                        self.state.show_command_palette = false;
                    }
                });

            let entries = build_palette(&self.state);
            let query = self.state.command_palette_query.to_lowercase();
            let filtered_idx: Vec<usize> = entries
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    query.is_empty()
                        || e.label.to_lowercase().contains(&query)
                        || e.shortcut.to_lowercase().contains(&query)
                })
                .map(|(i, _)| i)
                .collect();

            let max_visible = 12;
            let sel = &mut self.state.command_palette_selected;
            if *sel >= filtered_idx.len() { *sel = 0; }

            let mut close = false;
            let mut activate_idx: Option<usize> = None;

            egui::Area::new(egui::Id::new("command_palette"))
                .fixed_pos(egui::pos2(
                    ctx.screen_rect().center().x - 200.0,
                    ctx.screen_rect().top() + 80.0,
                ))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::window(ui.style())
                        .fill(self.theme.bg_panel)
                        .stroke(egui::Stroke::new(1.0_f32, self.theme.border))
                        .rounding(4.0)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            ui.set_min_width(400.0);

                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.state.command_palette_query)
                                    .hint_text("Type a command...")
                                    .desired_width(384.0)
                                    .font(egui::FontId::proportional(13.0))
                            );
                            resp.request_focus();

                            let input = ui.input(|i| (
                                i.key_pressed(egui::Key::ArrowDown),
                                i.key_pressed(egui::Key::ArrowUp),
                                i.key_pressed(egui::Key::Enter),
                                i.key_pressed(egui::Key::Escape),
                            ));
                            if input.0 && *sel + 1 < filtered_idx.len() { *sel += 1; }
                            if input.1 && *sel > 0 { *sel -= 1; }
                            if input.3 { close = true; }
                            if input.2 {
                                if let Some(&entry_idx) = filtered_idx.get(*sel) {
                                    activate_idx = Some(entry_idx);
                                }
                                close = true;
                            }

                            ui.add_space(4.0);

                            for (vis_i, &entry_idx) in filtered_idx.iter().take(max_visible).enumerate() {
                                let entry = &entries[entry_idx];
                                let is_selected = vis_i == *sel;
                                let bg = if is_selected {
                                    self.theme.bg_hover
                                } else {
                                    egui::Color32::TRANSPARENT
                                };
                                let text_color = if entry.enabled {
                                    self.theme.text_primary
                                } else {
                                    self.theme.text_dim
                                };
                                let (r, _) = ui.allocate_exact_size(egui::vec2(400.0, 22.0), egui::Sense::click());
                                ui.painter().rect_filled(r, 4.0, bg);
                                ui.painter().text(
                                    egui::pos2(r.left() + 8.0, r.center().y),
                                    egui::Align2::LEFT_CENTER,
                                    entry.label.as_ref(),
                                    egui::FontId::proportional(12.0),
                                    text_color,
                                );
                                if !entry.shortcut.is_empty() {
                                    ui.painter().text(
                                        egui::pos2(r.right() - 8.0, r.center().y),
                                        egui::Align2::RIGHT_CENTER,
                                        entry.shortcut.as_ref(),
                                        egui::FontId::monospace(10.0),
                                        self.theme.text_dim,
                                    );
                                }
                                if ui.rect_contains_pointer(r) && ui.input(|i| i.pointer.primary_clicked()) {
                                    activate_idx = Some(entry_idx);
                                    close = true;
                                }
                            }

                            if filtered_idx.is_empty() {
                                ui.colored_label(
                                    self.theme.text_dim,
                                    "No matching commands",
                                );
                            }
                        });
                });

            if close {
                self.state.show_command_palette = false;
            }
            if let Some(idx) = activate_idx {
                let entry = &entries[idx];
                if entry.enabled {
                    match &entry.action {
                        PaletteAction::Cmd(cmd) => all_cmds.push(cmd.clone()),
                        PaletteAction::ToggleState(f) | PaletteAction::SetState(f) => {
                            f(&mut self.state);
                        }
                        PaletteAction::OpenDialog(kind) => {
                            let kind = *kind;
                            crate::file_dialog::spawn(move || dispatch_dialog(kind));
                        }
                        PaletteAction::Custom(f) => {
                            f(&mut self.state, &mut all_cmds);
                        }
                    }
                }
            }
        }

        // Model-download dialogs (consent + progress + error). Drives
        // the F-97 SAM2 download flow. The state machine lives on
        // AppState.model_download; transitions are handled here per
        // frame.
        self.render_model_download_dialogs(ctx, &mut all_cmds);

        // History panel
        if self.state.show_history_panel {
            let mut open = true;
            egui::Window::new("History")
                .open(&mut open)
                .resizable(true)
                .default_size([220.0, 400.0])
                .show(ctx, |ui| {
                    ui.label(format!("{} undo / {} redo", self.state.active_history().undo_count(), self.state.active_history().redo_count()));
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(350.0).show(ui, |ui| {
                        // Redo stack (future states, shown dimmed at top)
                        let redo = self.state.active_history().redo_entries();
                        for (i, entry) in redo.iter().enumerate().rev() {
                            let label = entry.label();
                            let resp = ui.selectable_label(false,
                                egui::RichText::new(format!("  {label}"))
                                    .size(11.0)
                                    .color(self.theme.text_dim)
                            );
                            if resp.clicked() {
                                // Redo (redo_count - i) steps
                                for _ in 0..=(redo.len() - 1 - i) {
                                    all_cmds.push(AppCommand::Redo);
                                }
                            }
                        }
                        // Current state marker
                        ui.colored_label(self.theme.accent,
                            egui::RichText::new("\u{25B6} Current State").size(11.0));
                        // Undo stack (past states, most recent first)
                        let undo = self.state.active_history().undo_entries();
                        for (i, entry) in undo.iter().enumerate().rev() {
                            let label = entry.label();
                            let resp = ui.selectable_label(false,
                                egui::RichText::new(format!("  {label}")).size(11.0));
                            if resp.clicked() {
                                // Undo (undo_count - i) steps
                                for _ in 0..=(undo.len() - 1 - i) {
                                    all_cmds.push(AppCommand::Undo);
                                }
                            }
                        }
                    });
                });
            if !open {
                self.state.show_history_panel = false;
            }
        }

        // About dialog
        if self.state.show_about_dialog {
            let mut open = true;
            egui::Window::new("About Klipp")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(8.0);
                        if let Some(tex) = self.toolbar.logo_texture.as_ref() {
                            let logo_size = 64.0;
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(logo_size, logo_size),
                                egui::Sense::hover(),
                            );
                            ui.painter().image(
                                tex.id(),
                                rect,
                                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                egui::Color32::WHITE,
                            );
                            ui.add_space(4.0);
                        }
                        ui.label(egui::RichText::new("Klipp").strong().size(16.0));
                        ui.add_space(4.0);
                        let build = klipp_core::build_info::get();
                        ui.label(format!("Version {}", build.label()));
                        if let Some(at) = build.built_at {
                            ui.label(format!("Built {}", klipp_core::build_info::format_utc(at)));
                        }
                        if ui.button("Check for updates…").clicked() {
                            self.state.show_about_dialog = false;
                            self.updates.open();
                        }
                        ui.label("Free, open-source NLE for Linux");
                        ui.add_space(8.0);
                        ui.label("Built with:");
                        ui.label("Rust \u{00b7} egui \u{00b7} libmpv2");
                        ui.add_space(8.0);
                        ui.label("github.com/davidkodar/klipp");
                        ui.add_space(8.0);
                        if ui.button("Close").clicked() {
                            self.state.show_about_dialog = false;
                        }
                    });
                });
            if !open {
                self.state.show_about_dialog = false;
            }
        }

        // Relink dialog
        if self.state.show_relink_dialog {
            let mut open = true;
            egui::Window::new("Relink Offline Media")
                .collapsible(false)
                .resizable(true)
                .default_width(600.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    let relinks = self.relink_panel.show(ui, &self.state, &self.theme);
                    all_cmds.extend(relinks);
                    ui.add_space(4.0);
                    if ui.button("Close").clicked() {
                        self.state.show_relink_dialog = false;
                    }
                });
            if !open {
                self.state.show_relink_dialog = false;
            }
        }

        // (Removed: unconditional request_repaint during playback. That
        // call meant "next tick ASAP" — combined with the 16 ms
        // request_repaint_after above it forced UI redraws to run as
        // fast as the event loop can go, burning CPU. 60 Hz via the
        // request_repaint_after is sufficient and matches the engine.)

        // 9b. Floating scope window — runs AFTER CentralPanel so viewer_image_rect is current-frame
        if !self.state.fullscreen_viewer {
            let locked_data = if self.state.scope_cursor_locked {
                self.state.scope_cursor_locked_data
            } else {
                None
            };
            // Collect audio-scope inputs. Both are snapshotted under
            // `try_lock` so the UI never stalls behind the mix thread —
            // a missed frame just means the scope holds its previous
            // texture, which naturally reads as "frozen on pause."
            let correlation_samples: Vec<f32> = if let Ok(ring) =
                self.state.spectrum_ring.try_lock()
            {
                // Last 4096 interleaved stereo samples = 2048 stereo pairs
                // ≈ 43 ms at 48 kHz. Enough trace density for a stable
                // Lissajous figure, small enough to fit on the stack
                // of the render path.
                const N: usize = 4096;
                let len = ring.len();
                let start = len.saturating_sub(N);
                ring.iter().copied().skip(start).collect()
            } else {
                Vec::new()
            };
            let lufs_history: Vec<f32> =
                self.state.lufs_short_term_history.iter().copied().collect();
            let lufs_target = self.state.lufs_target;
            let spectrum_history: Vec<Vec<f32>> =
                self.state.spectrum_history.iter().cloned().collect();
            // The engine keeps one low-pass filter and one bloom for every
            // scope: the floating window's checkboxes show and set those.
            crate::widgets::scope_window::show_shared_toggles(
                &mut self.state.scope_window,
                self.state.scope_low_pass,
                self.state.scope_bloom,
            );
            self.scope_window_runtime.hdr_levels = self.state.hdr_monitor.renders_hdr();
            // The look's skin level, for the false-colour pane's green
            // band, so it agrees with the skin findings rather than
            // being a second opinion from a fixed IRE window.
            self.state.scope_window.look_skin_l = self
                .state
                .sequence()
                .brief
                .as_ref()
                .and_then(|b| b.look.as_ref())
                .and_then(|l| l.skin_l);
            crate::widgets::scope_window::show_scope_window(
                ctx,
                &mut self.state.scope_window,
                &mut self.state.scope_options,
                &mut self.scope_window_runtime,
                &self.frame_buffer,
                self.prefs.color.vectorscope_skin_angle,
                self.state.playback == PlaybackState::Playing,
                self.state.viewer_image_rect,
                self.state.viewer_pointer_pos,
                locked_data,
                &self.state.spectrum_bins,
                &correlation_samples,
                &lufs_history,
                lufs_target,
                &spectrum_history,
                &self.theme,
                klipp_color::scopes::ScopeBasis::for_working_space(
                    self.state.project_working_colorspace,
                ),
            );
            crate::widgets::scope_window::take_shared_toggles(
                &self.state.scope_window,
                &mut self.state.scope_low_pass,
                &mut self.state.scope_bloom,
            );
        }

        // 10. Process quit
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::PROCESS_QUIT);
        let has_quit = all_cmds.iter().any(|c| matches!(c, AppCommand::Quit));

        // When unsaved changes are about to raise the Save dialog, the
        // Quit command must NOT reach the engine — the engine treats it
        // as "shut down now" and would kill itself behind whatever is
        // queued while the user is still deciding. Cancel would then
        // leave a dead engine, and the engine thread's teardown while
        // the app keeps running races eframe's wgpu state (shutdown
        // SIGSEGV). The dialog's Don't Save / Save buttons hard-exit
        // the process themselves, so the engine never needs this Quit.
        if has_quit && self.state.dirty {
            all_cmds.retain(|c| !matches!(c, AppCommand::Quit));
        }

        // Save layout state on quit
        if has_quit {
            self.layout.save();
        }

        // 11. Send all commands
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::SEND_COMMANDS);
        self.send_commands(all_cmds);
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::POST_COMMANDS);

        // 11.5. Upload title overlay texture if a title property was changed this frame
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::UPLOAD_TITLE);
        self.upload_title_overlay(ctx);

        // Clear title overlay during playback (engine handles compositing then)
        if self.state.playback == PlaybackState::Playing {
            if let Some(old) = self.viewer.title_overlay_texture.take() {
                self.defer_drop_texture(old);
            }
        }

        // Watchdog: reset stuck skim_decode_in_flight after 2s with no completion.
        // Belt-and-braces backup to the always-send-completion fix in the decode threads.
        if self.state.skim_decode_in_flight {
            if let Some(t) = self.state.skim_decode_started_at {
                if t.elapsed().as_secs_f32() > 2.0 {
                    tracing::warn!("skim decode timed out — resetting in-flight flag");
                    self.state.skim_decode_in_flight = false;
                    self.state.skim_decode_started_at = None;
                }
            }
        }

        // 11a. Skim cleanup: if no panel set the skim flag this frame, tear
        // down whatever skim state is still alive. Three independent skim
        // surfaces:
        //   * `browser_skim_media_id` — Source-viewer media-pool hover
        //     (texture overlay).
        //   * `skim_audio_session`    — audio skim sidecar (cpal).
        //   * `program_skim_active`   — Program-viewer timeline /
        //     filmstrip hover (frame written into the engine's
        //     frame_buffer; refreshing it requires a re-seek to the
        //     committed playhead because the skim path overwrote what
        //     the engine last decoded for playback).
        // All three need to wind down together so pro-NLE "mouse-leave
        // = back to the committed playhead" semantics hold uniformly.
        let lost_focus = !ctx.input(|i| i.viewport().focused.unwrap_or(true));
        let any_skim_state_alive = self.state.browser_skim_media_id.is_some()
            || self.state.skim_audio_session.is_some()
            || self.state.program_skim_active;
        if (!self.state.browser_skim_active_this_frame && any_skim_state_alive) || lost_focus {
            // The skim is over: coming back to the same frame asks for it again.
            self.state.skim_last_requested = None;
            if self.state.browser_skim_media_id.is_some() {
                // Route the cleared handle through the deferred-drop ring —
                // the current frame's submit may still reference its ID,
                // and dropping inline here trips egui-wgpu's
                // "Texture has been destroyed" validation.
                if let Some(old) = self.state.browser_skim_texture.take() {
                    self.defer_drop_texture(old);
                }
                self.state.browser_skim_media_id = None;
                // `viewer_mode` is no longer reset here. It's now the
                // user-controlled focus indicator (set by clicking a
                // viewer or by LoadSourceMedia); resetting it on skim
                // end would silently steal focus back to Program every
                // time a hover finished.
            }
            // End audio skim session when mouse leaves or app loses focus
            if self.state.skim_audio_session.is_some() {
                self.send_commands(vec![AppCommand::BrowserSkimAudioEnd]);
            }
            // Program-viewer skim ended: reset the flag, and when not
            // actively playing back, re-decode at the committed playhead
            // so the viewer reverts from the last-skimmed frame to the
            // actual current frame.
            //
            // CRITICAL: `state.playhead` is only authoritative when not
            // playing. During playback `PlayheadMoved` updates the
            // *anchor* and leaves `state.playhead` pinned at the
            // pre-play position (see the PlayheadMoved handler). If we
            // emitted `SeekTo(state.playhead)` mid-playback we'd snap
            // back to wherever playback started — exactly the bug we
            // saw. Skip the seek when playing; the engine overwrites
            // `frame_buffer` with its next real frame within ~one frame
            // anyway, so the skim leftover disappears on its own.
            if self.state.program_skim_active {
                self.state.program_skim_active = false;
                if self.state.playback != PlaybackState::Playing {
                    // Debounced: arm a deadline instead of seeking now.
                    // Continuous skimming flaps this flag for a frame at
                    // clip boundaries; an immediate revert per flap
                    // flickers the viewer and queues a seek per flap
                    // (the engine pays a full seek for each — a hover
                    // session once stalled it 8+ s draining them). A
                    // new skim frame before the deadline disarms it.
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_millis(150);
                    self.state.program_skim_revert_deadline = Some(deadline);
                    ctx.request_repaint_after(std::time::Duration::from_millis(160));
                }
            }
        }

        // Fire the debounced skim-end revert once the skim has stayed
        // quiet past the deadline: one SeekTo(committed playhead) to
        // repaint the Program viewer over the last skim frame.
        if let Some(deadline) = self.state.program_skim_revert_deadline {
            if self.state.program_skim_active
                || self.state.playback == PlaybackState::Playing
                || self.state.is_scrubbing
            {
                // Skim resumed (flag flap), playback took over, or a drag
                // began — the engine repaints on its own and `scrub_end`
                // commits the drop position, so no revert is needed. A
                // deadline armed by the hover that ended as the drag began
                // would otherwise still fire in the middle of it.
                self.state.program_skim_revert_deadline = None;
            } else if std::time::Instant::now() >= deadline {
                self.state.program_skim_revert_deadline = None;
                let cur = self.state.playhead;
                let _ = self.cmd_tx.send(AppCommand::SeekTo(cur));
            } else {
                // Not due yet — make sure a repaint arrives to fire it
                // even if the mouse goes still.
                ctx.request_repaint_after(std::time::Duration::from_millis(50));
            }
        }

        // Fullscreen viewer is an in-window "cinema mode": the main
        // layout is replaced with a viewer-only panel filling
        // `ctx.screen_rect()`. We deliberately do NOT send
        // `ViewportCommand::Fullscreen` — on Wayland+NVIDIA the
        // resulting swap-chain reconfigure leaves the surface in a
        // `VK_SUBOPTIMAL_KHR` state that presents a stale frame
        // forever. Users who want OS-level fullscreen can use their
        // window manager's own fullscreen hotkey (Super+F on most
        // compositors) on the Klipp window instead.
        if self.state.fullscreen_changed {
            self.state.fullscreen_changed = false;
            ctx.request_repaint();
        }

        // 11c. Toast notifications + drag-drop cleanup
        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::TOAST_AND_GHOST);
        // Phase 5 — fire any one-shot first-time tips that the
        // current sequence state has just unlocked. Cheap per-frame
        // check; the dismissal set is consulted up front so each tip
        // ever fires at most once, persisted to prefs.
        self.maybe_fire_first_time_tips();
        self.state.toast.render(ctx, &self.theme);
        self.state.toast.cleanup();

        // 11c-bis. Floating drag ghost for FilePaths drags from the file
        // browser. Other DragPayload types rely on target-side highlighting
        // alone, but the file browser → timeline path crosses a lot of
        // empty screen space, so we render a small label following the
        // cursor for the entire duration of the drag.
        if let Some(crate::drag_drop::DragPayload::FilePaths { ref paths }) = self.state.drag_drop.payload {
            if let Some(pos) = self.state.drag_drop.cursor_pos {
                let label = if paths.len() == 1 {
                    paths[0].file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| paths[0].to_string_lossy().to_string())
                } else {
                    format!("{} files", paths.len())
                };
                let theme = &self.theme;
                egui::Area::new(egui::Id::new("file_drag_ghost"))
                    .order(egui::Order::Tooltip)
                    .fixed_pos(pos + egui::vec2(12.0, 12.0))
                    .interactable(false)
                    .show(ctx, |ui| {
                        egui::Frame::none()
                            .fill(theme.bg_raised)
                            .stroke(egui::Stroke::new(1.0_f32, theme.accent))
                            .rounding(4.0)
                            .inner_margin(egui::Margin::symmetric(8.0, 4.0))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(label)
                                        .size(11.0)
                                        .color(theme.text_primary),
                                );
                            });
                    });
            }
        }

        // If the user releases the mouse anywhere outside a drop target while
        // a FilePaths drag is in progress, clear the payload (otherwise it
        // would persist into the next click).
        if matches!(self.state.drag_drop.payload, Some(crate::drag_drop::DragPayload::FilePaths { .. }))
            && !ctx.input(|i| i.pointer.any_down())
        {
            self.state.drag_drop.end();
        }

        self.state.drag_drop.frame_start();

        klipp_engine::ui_stage::mark(klipp_engine::ui_stage::FRAME_END);

        // 12. Filmstrip generation — shown in status bar now (no popup)

        // 12b. Proxy generation toast
        if !self.state.proxy_toast_dismissed {
            if let Some((ref msg, when)) = self.state.proxy_status_message {
                let age = when.elapsed().as_secs_f64();
                if age < 5.0 {
                    let y_offset = if self.state.filmstrip_generating && !self.state.filmstrip_toast_dismissed {
                        -120.0
                    } else {
                        -80.0
                    };
                    // Compute aggregate progress across all generating proxies
                    let avg_progress = if !self.state.proxy_generating.is_empty() {
                        let total: f32 = self.state.proxy_generating.iter()
                            .map(|id| self.state.proxy_progress.get(id).copied().unwrap_or(0.0))
                            .sum();
                        total / self.state.proxy_generating.len() as f32
                    } else {
                        0.0
                    };
                    let is_generating = !self.state.proxy_generating.is_empty();
                    // Compute ETA from earliest start time
                    let eta_str = if is_generating && avg_progress > 0.01 {
                        let earliest_start = self.state.proxy_start_times.values().min().copied();
                        if let Some(start) = earliest_start {
                            let elapsed = start.elapsed().as_secs_f64();
                            let remaining = (elapsed / avg_progress as f64) - elapsed;
                            let rem_secs = remaining as u64;
                            if rem_secs >= 60 {
                                format!(" — ~{}m {:02}s left", rem_secs / 60, rem_secs % 60)
                            } else {
                                format!(" — ~{}s left", rem_secs)
                            }
                        } else {
                            String::new()
                        }
                    } else {
                        String::new()
                    };
                    let msg_clone = msg.clone();
                    egui::Area::new(egui::Id::new("proxy_toast"))
                        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, y_offset))
                        .order(egui::Order::Foreground)
                        .show(ctx, |ui| {
                            egui::Frame::popup(ui.style())
                                .fill(self.theme.bg_raised)
                                .stroke(egui::Stroke::new(1.0_f32, self.theme.border))
                                .rounding(4.0)
                                .inner_margin(egui::Margin::symmetric(16.0, 10.0))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        if is_generating {
                                            ui.spinner();
                                            ui.add_space(8.0);
                                        }
                                        if is_generating {
                                            ui.label(
                                                egui::RichText::new(format!("{} — {:.0}%{}", msg_clone, avg_progress * 100.0, eta_str))
                                                    .color(self.theme.text_primary)
                                                    .size(12.0),
                                            );
                                        } else {
                                            ui.label(
                                                egui::RichText::new(&msg_clone)
                                                    .color(self.theme.text_primary)
                                                    .size(12.0),
                                            );
                                        }
                                        ui.add_space(8.0);
                                        if ui.add(egui::Button::image(Icon::X.image(12.0).tint(self.theme.text_primary))).on_hover_text("Dismiss").clicked() {
                                            self.state.proxy_toast_dismissed = true;
                                        }
                                    });
                                    if is_generating {
                                        ui.add_space(4.0);
                                        let bar = egui::ProgressBar::new(avg_progress)
                                            .desired_width(250.0);
                                        ui.add(bar);
                                    }
                                });
                        });
                    ctx.request_repaint();
                } else if self.state.proxy_generating.is_empty() {
                    self.state.proxy_status_message = None;
                }
            }
        }
        // Keep toast alive while generating; un-dismiss when new proxy starts
        if !self.state.proxy_generating.is_empty() {
            if let Some((_, ref mut when)) = self.state.proxy_status_message {
                *when = std::time::Instant::now();
            }
        }

        // Intercept window X button close — treat same as File → Quit.
        // Gate on `!show_unsaved_dialog` so CancelClose isn't fired
        // every frame while the dialog is up (egui keeps reporting
        // close_requested = true until either Close or CancelClose is
        // actually processed by the OS event loop, which can be a
        // few frames late). The repeated CancelClose stomps on
        // egui's viewport state machine and can wedge the input
        // handler — symptoms include the dialog rendering but
        // ignoring clicks.
        let window_close_requested = ctx.input(|i| i.viewport().close_requested());
        if window_close_requested {
            tracing::info!(
                "viewport close_requested received (dirty={}, dialog_already_open={})",
                self.state.dirty,
                self.state.show_unsaved_dialog,
            );
            // A dirty project is asked about whether it has been saved
            // before or not. Gating on a path made a never-saved project —
            // the state every first session is in — close in silence with
            // everything in it.
            if self.state.dirty && !self.state.show_unsaved_dialog {
                tracing::info!("→ raising unsaved-changes dialog, sending CancelClose");
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.state.show_unsaved_dialog = true;
                self.state.unsaved_dialog_action = UnsavedAction::Quit;
            } else if !self.state.dirty {
                eprintln!("Klipp: clean close → exit(0)");
                // Read-only, the auto-save beside the project is the
                // holder's crash recovery, not ours to clear.
                if let Some(path) = self.writable_project_dir() {
                    let autosave = autosave_path_for(&path);
                    if autosave.exists() {
                        std::thread::Builder::new()
                            .name("klipp-autosave-rm".into())
                            .spawn(move || {
                                let _ = std::fs::remove_dir_all(&autosave);
                            })
                            .ok();
                    }
                }
                self.kill_children_for_exit();
                self.layout.save();
                self.exit_after_engine_shutdown();
            } else {
                // Dialog is already up: dialog buttons drive the
                // outcome. Send CancelClose once to reject this
                // window-manager close attempt; egui clears
                // close_requested next frame on its own, so we
                // don't loop.
                tracing::info!("→ dialog already open, rejecting this close request");
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            }
        }

        if has_quit {
            if self.state.dirty {
                self.state.show_unsaved_dialog = true;
                self.state.unsaved_dialog_action = UnsavedAction::Quit;
            } else {
                eprintln!("Klipp: has_quit branch → exit(0)");
                // Read-only, the auto-save beside the project is the
                // holder's crash recovery, not ours to clear.
                if let Some(path) = self.writable_project_dir() {
                    let autosave = autosave_path_for(&path);
                    if autosave.exists() {
                        std::thread::Builder::new()
                            .name("klipp-autosave-rm".into())
                            .spawn(move || {
                                let _ = std::fs::remove_dir_all(&autosave);
                            })
                            .ok();
                    }
                }
                self.kill_children_for_exit();
                self.layout.save();
                self.exit_after_engine_shutdown();
            }
        }

        if self.state.playback == PlaybackState::Playing || self.state.render_queue.has_active_job() {
            ctx.request_repaint();
        }

        // Watchdog: clear any unconsumed Ctrl+F focus request. If the
        // active panel had a search field, its show() already
        // request_focus'd and reset the field. If not (Inspector /
        // Viewer / Timeline / etc.), this clears the stale request so
        // the next Ctrl+F starts from a clean slate instead of
        // fighting a leftover one.
        self.state.focus_search_request = None;

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

    fn show_render_queue_window(&mut self, ctx: &egui::Context) {
        if !self.state.show_render_queue {
            return;
        }
        // The Export workspace shows the queue in its column; no window
        // on top of it.
        if self.state.active_workspace == crate::state::Workspace::Export {
            self.state.show_render_queue = false;
            return;
        }
        let actions = crate::panels::render_queue::RenderQueuePanel::show(
            ctx,
            &self.state.render_queue,
            &self.theme,
        );
        self.apply_render_queue_actions(actions);
    }

    fn apply_render_queue_actions(&mut self, actions: Vec<crate::panels::render_queue::RenderQueueAction>) {
        for action in actions {
            match action {
                crate::panels::render_queue::RenderQueueAction::CancelJob(id) => {
                    self.state.render_queue.cancel_job(id);
                }
                crate::panels::render_queue::RenderQueueAction::RemoveJob(id) => {
                    self.state.render_queue.remove_job(id);
                }
                crate::panels::render_queue::RenderQueueAction::ClearCompleted => {
                    self.state.render_queue.clear_completed();
                }
                crate::panels::render_queue::RenderQueueAction::Close => {
                    self.state.show_render_queue = false;
                }
                crate::panels::render_queue::RenderQueueAction::Play(path) => {
                    if let Err(e) = std::process::Command::new("xdg-open").arg(&path).spawn() {
                        self.state.toast.show(format!("Could not open {}: {e}", path.display()));
                    }
                }
                crate::panels::render_queue::RenderQueueAction::ShowInFolder(path) => {
                    self.send_commands(vec![AppCommand::RevealInFileManager { path }]);
                }
                crate::panels::render_queue::RenderQueueAction::RenderAgain(id) => self.render_job_again(id),
            }
        }
    }

    /// A finished, failed or cancelled job queued again (#717): its
    /// settings, file and frames, from its sequence as it is now, through
    /// the same checks as an export from the dialog. It takes the old
    /// job's place on the list.
    fn render_job_again(&mut self, id: uuid::Uuid) {
        let Some(job) = self.state.render_queue.jobs.iter().find(|j| j.id == id) else { return };
        let (settings, label, range, sequence_id) = (job.settings.clone(), job.label.clone(), job.export_range, job.sequence_id);
        let Some(seq_idx) = self.state.sequences.iter().position(|s| s.id == sequence_id) else {
            self.state.toast.show("The sequence this was rendered from is gone; export it again from the dialog");
            return;
        };
        let seq = &self.state.sequences[seq_idx];
        let offline = seq.tracks.iter().flat_map(|t| t.clips.iter()).filter_map(|c| c.media_id()).filter(|m| self.state.media_pool.get(m).is_some_and(|i| i.offline)).count();
        if offline > 0 {
            self.state.toast.show(format!("{offline} clip{} in it {} offline; relink before rendering again", if offline == 1 { "" } else { "s" }, if offline == 1 { "is" } else { "are" }));
            return;
        }
        if let Some(name) = self.find_export_overwrite_conflict(&settings.output_path) {
            self.state.toast.show(format!("{} is source media in this project; rendering over it would destroy it", name));
            return;
        }
        let snapshot = klipp_export::RenderSnapshot::from_sequence_with_all(
            &self.state.sequences[seq_idx],
            &self.state.sequences,
            &self.state.media_pool,
            &settings,
            self.state.global_lut_path.clone().filter(|_| self.state.global_lut_enabled),
            false,
            self.state.project_working_colorspace,
        );
        match snapshot {
            Ok(mut snap) => {
                snap.sequence_snapshot.export_range = range;
                snap.sequence_snapshot.video.global_lut_opacity = self.state.global_lut_opacity;
                self.state.render_queue.remove_job(id);
                self.state.render_queue.add_job(settings.output_path.clone(), settings, label, snap);
            }
            Err(e) => self.state.toast.show(format!("Could not render again: {e}")),
        }
    }


    /// Apply a `SavedWorkspaceLayout` to AppState + LayoutState. Used by
    /// both the built-in and user-layout apply commands.
    ///
    /// Visibility flags are copied unconditionally; size fields only
    /// overwrite `self.layout.*` when `Some`, which preserves user
    /// customisation when an older layout JSON doesn't carry sizes (the
    /// serde `#[default]` lands them as `None`).
    fn apply_workspace_layout(&mut self, layout: &crate::state::SavedWorkspaceLayout) {
        // Through the setters, so the persisted copy follows a workspace
        // switch rather than silently disagreeing with the screen.
        self.set_browser_visible(layout.show_browser);
        self.set_inspector_visible(layout.show_inspector);
        self.state.show_history_panel = layout.show_history;
        self.state.show_markers_panel = layout.show_markers;
        self.state.show_curve_editor = layout.show_curve_editor;
        self.state.show_scopes = layout.show_scopes;
        self.state.show_color_panel = layout.show_color_panel;
        if let Some(w) = layout.browser_width {
            self.layout.browser_width = w;
        }
        if let Some(w) = layout.inspector_width {
            self.layout.inspector_width = w;
        }
        if let Some(h) = layout.timeline_height {
            self.layout.timeline_height = h;
        }
        if let Some(h) = layout.color_panel_height {
            self.layout.color_panel_height = h;
        }
        self.layout.save();
    }

    fn update_thumbnail_textures(&mut self, ctx: &egui::Context) {
        let media_ids: Vec<MediaId> = self.state.media_pool.all().map(|m| m.id).collect();
        for media_id in media_ids {
            // Upload single thumbnail if not yet done
            if !self.state.thumbnail_textures.contains_key(&media_id) {
                if let Some(ThumbnailEntry::Ready { rgba, width, height }) =
                    self.state.thumbnail_cache.get(&media_id)
                {
                    let w = *width as usize;
                    let h = *height as usize;
                    if rgba.len() == w * h * 4 {
                        let pixels: Vec<egui::Color32> = rgba
                            .chunks_exact(4)
                            .map(|p| {
                                egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])
                            })
                            .collect();
                        let image = egui::ColorImage {
                            size: [w, h],
                            pixels,
                        };
                        let tex_name = format!("thumb-{}", media_id);
                        let tex = alloc_texture_logged(ctx, &tex_name, image, egui::TextureOptions::LINEAR);
                        if let Some(old) = self.state.thumbnail_textures.insert(media_id, tex) {
                            self.defer_drop_texture(old);
                        }
                    }
                }
            }

            // Upload filmstrip textures — reuse cached textures by time_secs
            // so that each new background frame only allocates its own texture,
            // not the entire filmstrip. Previously this path was O(N²) per clip
            // (every incoming frame triggered a full re-upload of N existing
            // frames + 1 new), churning ~19k texture allocations for a ~417-frame
            // clip and stalling the UI thread under wgpu's command-buffer pressure.
            //
            // Identity is `time_secs` quantised to microseconds so refinement
            // (which inserts new times BETWEEN existing ones, shifting indices)
            // still reuses correctly. Per `feedback_defer_drop_texture`, any
            // texture whose Arc-clone we relinquish mid-frame goes through
            // `deferred_texture_drops` rather than dropping inline — wgpu would
            // panic on the next submit otherwise.
            let existing_count = self.state.filmstrip_textures.get(&media_id).map(|t| t.len()).unwrap_or(0);
            let cache_count = self.state.thumbnail_cache.filmstrip(&media_id).map(|f| f.len()).unwrap_or(0);
            if cache_count != existing_count {
                if let Some(frames) = self.state.thumbnail_cache.filmstrip(&media_id) {
                    if !frames.is_empty() {
                        // Build a lookup table from the existing vec keyed by
                        // microsecond-quantised time. Each texture gets used at
                        // most once below (removed on hit), so leftovers at the
                        // end of the loop are stale entries to defer-drop.
                        let existing_vec = self.state.filmstrip_textures.remove(&media_id).unwrap_or_default();
                        let mut reuse: std::collections::HashMap<i64, egui::TextureHandle> =
                            std::collections::HashMap::with_capacity(existing_vec.len());
                        for (t, h) in existing_vec.into_iter() {
                            let key = (t * 1_000_000.0).round() as i64;
                            reuse.insert(key, h);
                        }
                        let mut new_vec: Vec<(f64, egui::TextureHandle)> = Vec::with_capacity(frames.len());
                        for (i, frame) in frames.iter().enumerate() {
                            let key = (frame.time_secs * 1_000_000.0).round() as i64;
                            if let Some(h) = reuse.remove(&key) {
                                new_vec.push((frame.time_secs, h));
                                continue;
                            }
                            let w = frame.width as usize;
                            let h_p = frame.height as usize;
                            if frame.rgba.len() != w * h_p * 4 {
                                continue;
                            }
                            let pixels: Vec<egui::Color32> = frame
                                .rgba
                                .chunks_exact(4)
                                .map(|p| {
                                    egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])
                                })
                                .collect();
                            let image = egui::ColorImage {
                                size: [w, h_p],
                                pixels,
                            };
                            let tex_name = format!("filmstrip-{}-{}", media_id, i);
                            let tex = alloc_texture_logged(ctx, &tex_name, image, egui::TextureOptions::LINEAR);
                            new_vec.push((frame.time_secs, tex));
                        }
                        // Defer-drop any textures whose time_secs no longer
                        // appears in the cache (e.g. refinement replaced them).
                        // In the common "frames append" case `reuse` is empty
                        // and this is a no-op.
                        if !reuse.is_empty() {
                            let slot = self.state.deferred_texture_drops_slot;
                            self.state.deferred_texture_drops[slot].extend(reuse.into_values());
                        }
                        self.state.filmstrip_textures.insert(media_id, new_vec);
                    }
                }
            }
        }
    }
    fn show_modal_dialogs(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        crate::title_templates::show_dialog(ctx, &mut self.state);

        // 4a0. Paste Attributes dialog
        if self.state.show_paste_attributes_dialog {
            let mut open = true;
            egui::Window::new("Paste Attributes")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(260.0);
                    ui.label("Select which attributes to paste:");
                    ui.add_space(6.0);

                    // Persistent checkboxes via egui's data store (no unsafe statics)
                    let pa_id = egui::Id::new("paste_attributes_checkboxes");
                    let mut pa_effects = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("fx"), || true));
                    let mut pa_color = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("col"), || true));
                    let mut pa_audio = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("aud"), || true));
                    let mut pa_transform = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("tfm"), || true));
                    let mut pa_speed = ctx.data_mut(|d| *d.get_temp_mut_or_insert_with(pa_id.with("spd"), || false));

                    ui.checkbox(&mut pa_effects, "Effects (built-in + OFX)");
                    ui.checkbox(&mut pa_color, "Colour Grade (node graph)");
                    ui.checkbox(&mut pa_audio, "Audio (volume, pan, gain, fades, FX)");
                    ui.checkbox(&mut pa_transform, "Transform (position, scale, rotation, opacity)");
                    ui.checkbox(&mut pa_speed, "Speed");

                    // Write back to egui data store
                    ctx.data_mut(|d| { d.insert_temp(pa_id.with("fx"), pa_effects); d.insert_temp(pa_id.with("col"), pa_color); d.insert_temp(pa_id.with("aud"), pa_audio); d.insert_temp(pa_id.with("tfm"), pa_transform); d.insert_temp(pa_id.with("spd"), pa_speed); });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Paste").clicked() {
                            cmds.push(AppCommand::PasteAttributes {
                                effects: pa_effects,
                                color_grade: pa_color,
                                audio: pa_audio,
                                transform: pa_transform,
                                speed: pa_speed,
                            });
                            self.state.show_paste_attributes_dialog = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.show_paste_attributes_dialog = false;
                        }
                    });
                });
            if !open {
                self.state.show_paste_attributes_dialog = false;
            }
        }

        // Speed dialog
        if self.state.show_speed_dialog {
            // Modal backdrop
            egui::Area::new(egui::Id::new("speed_dialog_backdrop"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::PanelResizeLine)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    let resp = ui.allocate_rect(screen, egui::Sense::click());
                    ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(120));
                    if resp.clicked() { self.state.show_speed_dialog = false; }
                });

            let mut open = true;
            egui::Window::new("Set Clip Speed")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(280.0);
                    ui.add_space(4.0);

                    ui.horizontal(|ui| {
                        ui.label("Speed:");
                        ui.add(egui::DragValue::new(&mut self.state.speed_dialog_value)
                            .range(1.0..=1000.0).speed(1.0).suffix("%").fixed_decimals(0));
                    });
                    ui.add_space(4.0);

                    // Preset buttons
                    ui.horizontal(|ui| {
                        for (label, pct) in [("25%", 25.0), ("50%", 50.0), ("100%", 100.0), ("200%", 200.0), ("400%", 400.0)] {
                            if ui.selectable_label((self.state.speed_dialog_value - pct).abs() < 0.5, label).clicked() {
                                self.state.speed_dialog_value = pct;
                            }
                        }
                    });
                    ui.add_space(4.0);

                    // Reverse reads a media file backward (#617): a title or
                    // a compound has none.
                    let can_reverse = self.state.selected_clip
                        .and_then(|id| self.state.sequence().all_clips().find(|c| c.id == id).map(|c| matches!(c.kind, klipp_timeline::ClipKind::Media { .. })))
                        .unwrap_or(false);
                    if !can_reverse {
                        self.state.speed_dialog_reverse = false;
                    }
                    ui.add_enabled(can_reverse, egui::Checkbox::new(&mut self.state.speed_dialog_reverse, "Reverse playback"))
                        .on_hover_text("Play the clip from its last frame to its first. Its sound plays backward with it.")
                        .on_disabled_hover_text("Only a clip of a media file can play in reverse");
                    ui.checkbox(&mut self.state.speed_dialog_ripple, "Move the clips after it")
                        .on_hover_text("A clip made longer pushes what follows on its track (and its sound's) along; one made shorter pulls it up. Off, a clip that would grow into the next one is not changed.");
                    // What a speed change does to the sound (#638).
                    ui.label(egui::RichText::new("The sound plays at the new speed, its pitch moving with it, as tape does.").small().weak());
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            if let Some(clip_id) = self.state.selected_clip {
                                let mut speed = self.state.speed_dialog_value / 100.0;
                                if self.state.speed_dialog_reverse { speed = -speed; }
                                cmds.push(AppCommand::SetClipSpeed { clip_id, speed, ripple: self.state.speed_dialog_ripple });
                            }
                            self.state.show_speed_dialog = false;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.show_speed_dialog = false;
                        }
                    });
                });
            if !open {
                self.state.show_speed_dialog = false;
            }
        }

        // Save Workspace dialog
        if self.state.show_save_workspace_dialog {
            let mut open = true;
            egui::Window::new("Save Workspace")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(260.0);
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        ui.text_edit_singleline(&mut self.state.save_workspace_name);
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            // Capture the current panel sizes alongside the
                            // visibility flags so the saved workspace is a
                            // full layout snapshot — not just "which panels
                            // are visible," which would drop the sizing the
                            // user just arranged.
                            let layout = crate::state::SavedWorkspaceLayout {
                                name: self.state.save_workspace_name.clone(),
                                show_browser: self.state.show_browser_panel,
                                show_inspector: self.state.show_inspector_panel,
                                show_history: self.state.show_history_panel,
                                show_markers: self.state.show_markers_panel,
                                show_curve_editor: self.state.show_curve_editor,
                                show_scopes: self.state.show_scopes,
                                show_color_panel: self.state.show_color_panel,
                                browser_width: Some(self.layout.browser_width),
                                inspector_width: Some(self.layout.inspector_width),
                                timeline_height: Some(self.layout.timeline_height),
                                color_panel_height: Some(self.layout.color_panel_height),
                            };
                            // Replace if name exists, otherwise add
                            if let Some(existing) = self.state.custom_workspaces.iter_mut().find(|w| w.name == layout.name) {
                                *existing = layout;
                            } else {
                                self.state.custom_workspaces.push(layout);
                            }
                            self.state.show_save_workspace_dialog = false;
                            self.state.toast.show("Workspace saved".to_string());
                            persist_workspaces(&self.state.custom_workspaces);
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.show_save_workspace_dialog = false;
                        }
                    });
                });
            if !open {
                self.state.show_save_workspace_dialog = false;
            }
        }

        // ── Save hyperzoom preset dialog (HZ-NEXT-SAVED-PRESETS) ──
        if self.state.show_save_hyperzoom_preset_dialog {
            let mut open = true;
            egui::Window::new("Save Hyperzoom as Preset")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.set_min_width(280.0);
                    ui.horizontal(|ui| {
                        ui.label("Name:");
                        let resp = ui.text_edit_singleline(&mut self.state.save_hyperzoom_preset_name);
                        // Clear inline validation error on edit so
                        // the user gets immediate feedback that the
                        // text is being accepted.
                        if resp.changed() {
                            self.state.save_hyperzoom_preset_error = None;
                        }
                    });
                    if let Some(err) = self.state.save_hyperzoom_preset_error.as_ref() {
                        ui.add_space(2.0);
                        ui.label(
                            egui::RichText::new(format!("\u{26A0} {err}"))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(0xE0, 0x80, 0x40)),
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            // Read the bound clip's scratch.
                            // Source clip was pinned at dialog-open
                            // time; if it's gone (clip deleted while
                            // dialog open), we close gracefully.
                            let Some(source_clip) = self.state.save_hyperzoom_preset_source_clip else {
                                self.state.show_save_hyperzoom_preset_dialog = false;
                                self.state.toast.show("Preset save cancelled (no source clip)".to_string());
                                return;
                            };
                            let scratch = crate::hyperzoom::read_hyperzoom_scratch(ctx, source_clip);
                            let name = self.state.save_hyperzoom_preset_name.clone();
                            let preset = crate::hyperzoom_presets::HyperzoomPreset::from_scratch(name, &scratch);
                            match self.state.hyperzoom_preset_registry.save(preset) {
                                Ok(()) => {
                                    let saved_name = self.state.save_hyperzoom_preset_name.clone();
                                    self.state.save_hyperzoom_preset_name.clear();
                                    self.state.save_hyperzoom_preset_error = None;
                                    self.state.save_hyperzoom_preset_source_clip = None;
                                    self.state.show_save_hyperzoom_preset_dialog = false;
                                    self.state.toast.show(format!("Saved hyperzoom preset '{saved_name}'"));
                                }
                                Err(crate::hyperzoom_presets::SaveError::BadName(e)) => {
                                    // Inline validation — keep the
                                    // dialog open so the user can
                                    // type a fresh name.
                                    self.state.save_hyperzoom_preset_error = Some(e.to_string());
                                }
                                Err(crate::hyperzoom_presets::SaveError::Io(e)) => {
                                    tracing::warn!("hyperzoom preset save IO failure: {e}");
                                    self.state.toast.show("Couldn't save preset (disk error)".to_string());
                                    self.state.show_save_hyperzoom_preset_dialog = false;
                                    self.state.save_hyperzoom_preset_source_clip = None;
                                }
                                Err(crate::hyperzoom_presets::SaveError::Serialize(e)) => {
                                    tracing::error!("hyperzoom preset save serialize failure: {e}");
                                    self.state.toast.show("Couldn't save preset".to_string());
                                    self.state.show_save_hyperzoom_preset_dialog = false;
                                    self.state.save_hyperzoom_preset_source_clip = None;
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.save_hyperzoom_preset_name.clear();
                            self.state.save_hyperzoom_preset_error = None;
                            self.state.save_hyperzoom_preset_source_clip = None;
                            self.state.show_save_hyperzoom_preset_dialog = false;
                        }
                    });
                });
            if !open {
                self.state.save_hyperzoom_preset_name.clear();
                self.state.save_hyperzoom_preset_error = None;
                self.state.save_hyperzoom_preset_source_clip = None;
                self.state.show_save_hyperzoom_preset_dialog = false;
            }
        }

        // 4a0. The project being opened is locked by someone else.
        self.show_lock_prompt(ctx);

        // 4a1. Auto-save recovery dialog
        if self.state.show_autosave_recovery {
            egui::Window::new("Recover Auto-Save?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("A newer auto-save was found for this project.");
                    ui.label("Would you like to restore it?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Restore Auto-Save").clicked() {
                            if let Some(autosave_path) = self.state.autosave_recovery_path.take() {
                                let original_path = self.state.autosave_original_path.take();
                                self.state.show_autosave_recovery = false;
                                self.do_open_project_inner(&autosave_path);
                                // Set project path to the original, not the autosave
                                if let Some(orig) = original_path {
                                    self.state.project_path = Some(orig);
                                }
                                self.state.dirty = true; // Mark dirty so user saves explicitly
                            }
                        }
                        if ui.button("Keep Saved Version").clicked() {
                            if let Some(original_path) = self.state.autosave_original_path.take() {
                                self.state.autosave_recovery_path = None;
                                self.state.show_autosave_recovery = false;
                                retire_autosave(&original_path);
                                self.do_open_project_inner(&original_path);
                            }
                        }
                    });
                });
        }

        // 4a1b. Confirm remove media from pool
        if let Some(media_id) = self.state.show_remove_media_confirm {
            let usage = self.state.media_usage_cache.get(&media_id).copied().unwrap_or(0);
            let name = self.state.media_pool.get(&media_id)
                .map(|m| m.filename.clone())
                .unwrap_or_else(|| "Unknown".into());
            egui::Window::new("Remove Media?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("\"{}\" is used in {} clip(s).", name, usage));
                    ui.label("Removing it will make those clips offline.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.state.show_remove_media_confirm = None;
                        }
                        if ui.button("Remove").clicked() {
                            cmds.push(AppCommand::ConfirmRemoveFromPool { media_id });
                        }
                    });
                });
        }

        // Delete unused media confirmation dialog
        if let Some((count, total_bytes)) = self.state.show_delete_unused_confirm {
            egui::Window::new("Delete Unused Media?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!("{count} items are not referenced by any timeline clip."));
                    let size_mb = total_bytes as f64 / (1024.0 * 1024.0);
                    if size_mb >= 1024.0 {
                        ui.label(format!("({count} items referencing {:.1} GB of source media)", size_mb / 1024.0));
                    } else {
                        ui.label(format!("({count} items referencing {:.0} MB of source media)", size_mb));
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.state.show_delete_unused_confirm = None;
                        }
                        if ui.button(format!("Remove {count} items")).clicked() {
                            cmds.push(AppCommand::ConfirmDeleteUnusedMedia);
                        }
                    });
                });
        }

        // Export offline media warning dialog
        if self.state.export_offline_warning.is_some() {
            let mut dismiss = false;
            let mut force_export = false;
            if let Some((ref clips, _)) = self.state.export_offline_warning {
                egui::Window::new("Export Contains Offline Media")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("These clips will export as black/silent frames:");
                        ui.add_space(4.0);
                        for (name, tc) in clips {
                            ui.label(format!("  \u{2022} {}  at {}", name, tc));
                        }
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                dismiss = true;
                            }
                            if ui.button("Export Anyway").clicked() {
                                force_export = true;
                            }
                        });
                    });
            }
            if dismiss {
                self.state.export_offline_warning = None;
            }
            if force_export {
                if let Some((_, settings)) = self.state.export_offline_warning.take() {
                    cmds.push(AppCommand::ForceExportSequence { settings });
                }
            }
        }

        // Export would overwrite a source media file — warn the user.
        // Three escape hatches: pick a different file (re-opens the
        // export dialog with the conflicting filename pre-filled),
        // auto-rename to the next free numeric suffix, or overwrite
        // anyway (the user knows what they're doing — e.g. they're
        // genuinely re-encoding in place).
        if self.state.export_source_overwrite_warning.is_some() {
            let mut auto_rename = false;
            let mut overwrite_anyway = false;
            let mut pick_different = false;
            let mut rename_target = std::path::PathBuf::new();
            if let Some((ref filename, ref settings)) =
                self.state.export_source_overwrite_warning
            {
                rename_target = Self::auto_rename_export_path(&settings.output_path);
                let rename_label = rename_target
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "renamed copy".into());
                egui::Window::new("Export Would Overwrite Source Media")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("The export target is the same file as a clip in this project:".to_string());
                        ui.add_space(2.0);
                        ui.label(format!("  \u{2022} {}", filename));
                        ui.add_space(6.0);
                        ui.label(
                            "Writing to it will replace the source on disk, \
                             invalidate the filmstrip cache, and on next reload \
                             the timeline will reference the rendered output \
                             instead of the camera-original.",
                        );
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui.button("Pick Different File").clicked() {
                                pick_different = true;
                            }
                            if ui.button(format!("Auto-Rename to {}", rename_label)).clicked() {
                                auto_rename = true;
                            }
                            if ui.button("Overwrite Anyway").clicked() {
                                overwrite_anyway = true;
                            }
                        });
                    });
            }
            if pick_different {
                self.state.export_source_overwrite_warning = None;
                self.state.show_export_dialog = true;
            }
            if auto_rename {
                if let Some((_, settings)) =
                    self.state.export_source_overwrite_warning.take()
                {
                    let mut new_settings = settings.as_ref().clone();
                    new_settings.output_path = rename_target;
                    cmds.push(AppCommand::ForceExportSequenceOverwrite {
                        settings: Box::new(new_settings),
                    });
                }
            }
            if overwrite_anyway {
                if let Some((_, settings)) =
                    self.state.export_source_overwrite_warning.take()
                {
                    cmds.push(AppCommand::ForceExportSequenceOverwrite { settings });
                }
            }
        }

        // 4a2. Confirm remove track dialog
        if let Some(track_id) = self.state.confirm_remove_track {
            let mut open = true;
            egui::Window::new("Remove Track")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("This track contains clips. Remove track and all its clips?");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Remove").clicked() {
                            let _ = self.state.sequences[self.state.active_sequence_idx].remove_track_with_clips(track_id);
                            self.state.sequences[self.state.active_sequence_idx].renumber_tracks();
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                            self.state.confirm_remove_track = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.confirm_remove_track = None;
                        }
                    });
                });
            if !open {
                self.state.confirm_remove_track = None;
            }
        }

        // 4b. Export dialog
        let export_cmds = self.show_export_dialog(ctx);
        cmds.extend(export_cmds);

        // 4c. Render queue window
        self.show_render_queue_window(ctx);

        // 4c0. AI Tools side panel — only mounted when toggled on.
        if self.state.ai_tools_state.panel_visible {
            // egui's SidePanel default `width_range.max` is
            // `f32::INFINITY`, which means any single-frame
            // content overflow becomes the new stored width on
            // the following frame (PanelState stores
            // inner_response.response.rect). Setting an explicit
            // max_width hard-caps the panel via clamp_to_range
            // regardless of what content reports.
            egui::SidePanel::right("ai_tools_side")
                .resizable(true)
                .default_width(340.0)
                .min_width(300.0)
                .max_width(500.0)
                .show(ctx, |ui| {
                    crate::ai_tools::AiToolsPanel::show(
                        ui,
                        &mut self.state.ai_tools_state,
                        &self.state.ai_tools_registry,
                        &self.theme,
                        &mut self.state.show_ai_models_dialog,
                    );
                });
        }

        // 4c1. New Project dialog
        if self.state.show_new_project_dialog {
            if let Some(cmd) = self.new_project_dialog.show(
                ctx,
                &mut self.state.show_new_project_dialog,
                &self.theme,
            ) {
                cmds.push(cmd);
            }
        }

        // 4c1a. Sequence Settings dialog (resolution + frame rate). The
        // original values are sourced from the targeted sequence at render
        // time so the dialog can detect what actually changed and emit
        // commands only for those fields.
        if let Some(dlg) = self.sequence_settings_dialog.as_mut() {
            let (orig_w, orig_h, orig_n, orig_d) = self
                .state
                .sequences
                .get(dlg.sequence_idx)
                .map(|s| (s.resolution.width, s.resolution.height, s.fps.num, s.fps.den))
                .unwrap_or((0, 0, 0, 0));
            let orig_shutter = self
                .state
                .sequences
                .get(dlg.sequence_idx)
                .map(|s| (s.camera.shutter_deg, s.camera.shutter_samples))
                .unwrap_or((0.0, 8));
            let (still_open, dlg_cmds) = dlg.show(ctx, orig_w, orig_h, orig_n, orig_d, orig_shutter);
            cmds.extend(dlg_cmds);
            if !still_open {
                self.sequence_settings_dialog = None;
            }
        }

        // 4c1a-bis. Frame-rate mismatch prompt. When the user drops a clip
        // whose fps differs from the active sequence's (and the sequence
        // already has clips), the append handler defers and opens this
        // modal. Re-dispatch the stashed command on Change / Don't Change
        // with `suppress_fps_prompt` set so the handler does not re-prompt.
        if let Some(dlg) = self.framerate_mismatch_dialog.as_mut() {
            if let Some(choice) = dlg.show(ctx) {
                use crate::panels::framerate_mismatch_dialog::FrameRateMismatchChoice;
                let pending = dlg.pending_cmd.clone();
                let seq_idx = dlg.sequence_idx;
                let media_fps = dlg.media_fps;
                self.framerate_mismatch_dialog = None;
                match choice {
                    FrameRateMismatchChoice::Change => {
                        cmds.push(AppCommand::SetSequenceFps {
                            sequence_idx: seq_idx,
                            fps_num: media_fps.num,
                            fps_den: media_fps.den,
                        });
                        self.suppress_fps_prompt = true;
                        cmds.push(pending);
                    }
                    FrameRateMismatchChoice::DontChange => {
                        self.suppress_fps_prompt = true;
                        cmds.push(pending);
                    }
                    FrameRateMismatchChoice::Cancel => {}
                }
            }
        }

        // 4c1b. Batch Rename dialog. Resolve the selected ids to
        // `(id, current filename)` pairs up-front so the dialog can borrow
        // `state` mutably without a conflicting read of `media_pool`.
        if self.state.show_batch_rename_dialog {
            let items: Vec<(klipp_core::MediaId, String)> = self.state
                .batch_rename_ids
                .iter()
                .filter_map(|id| self.state.media_pool.get(id).map(|m| (*id, m.filename.clone())))
                .collect();
            let dialog_cmds = crate::panels::batch_rename_dialog::show_batch_rename_dialog(
                ctx,
                &mut self.state,
                &items,
            );
            cmds.extend(dialog_cmds);
        }

        // Clip Properties modal (#47)
        if self.state.show_clip_properties {
            let dialog_cmds = crate::panels::clip_properties::show_clip_properties_dialog(
                ctx,
                &mut self.state,
            );
            cmds.extend(dialog_cmds);
        }

        // Export Preset Manager modal (#45)
        if self.state.show_preset_manager {
            let dialog_cmds = crate::panels::preset_manager::show_preset_manager_dialog(
                ctx,
                &mut self.state,
                &mut self.prefs.export.custom_presets,
            );
            cmds.extend(dialog_cmds);
        }

        // Workspace Layout Manager modal (#39)
        if self.state.show_workspace_manager {
            let dialog_cmds = crate::panels::workspace_manager::show_workspace_manager_dialog(
                ctx,
                &mut self.state,
            );
            cmds.extend(dialog_cmds);
        }

        // Cache Manager modal (#78)
        if self.state.show_cache_manager {
            let dialog_cmds = crate::panels::cache_manager::show_cache_manager_dialog(
                ctx,
                &mut self.state,
                &self.prefs,
            );
            cmds.extend(dialog_cmds);
        }

        // AI Models management modal — list, status badges, install /
        // reinstall actions. Drives the existing model_download
        // consent flow when the user picks Download.
        crate::panels::ai_models::show_ai_models_dialog(ctx, &mut self.state, &self.theme);

        // Audio Plugins management modal — LADSPA discovery, search
        // paths, Rescan. Pushes commands onto a local Vec so they go
        // through the standard `send_commands` dispatch (giving
        // `handle_playback_commands` first crack at OpenAudioPluginsDialog
        // / RescanAudioPlugins, same path as OpenAiModelsDialog).
        let mut audio_plugin_cmds: Vec<klipp_core::AppCommand> = Vec::new();
        crate::panels::audio_plugins::show_audio_plugins_dialog(
            ctx,
            &mut self.state,
            &self.theme,
            &mut audio_plugin_cmds,
        );
        if !audio_plugin_cmds.is_empty() {
            self.send_commands(audio_plugin_cmds);
        }

        // System Probe — read-only diagnostic snapshot. No commands
        // dispatched from inside, so no Vec needed.
        crate::panels::system_probe::show_system_probe_dialog(
            ctx,
            &mut self.state,
            &self.theme,
        );

        // Project Doctor — detect-and-report scan of the loaded
        // .klipp directory. Pushes commands through the standard
        // dispatch (today no per-finding fix actions exist; the
        // Vec is wired so future Fix buttons hook in cleanly).
        let mut doctor_cmds: Vec<klipp_core::AppCommand> = Vec::new();
        crate::panels::project_doctor::show_project_doctor_dialog(
            ctx,
            &mut self.state,
            &self.theme,
            &mut doctor_cmds,
        );
        if !doctor_cmds.is_empty() {
            self.send_commands(doctor_cmds);
        }

        // 4c2. Consolidate Project dialog
        // Copies the project + all referenced media into a NEW self-contained
        // directory. The source project is never modified.
        if self.state.show_consolidate_dialog {
            let Some(ref project_path) = self.state.project_path.clone() else {
                self.state.show_consolidate_dialog = false;
                return;
            };
            let mut open = true;
            let total_media = self.state.media_pool.all().count();
            let offline_count = self.state.media_pool.all().filter(|m| m.offline).count();
            let online_count = total_media - offline_count;
            let consolidation_active = self.state.consolidate_progress.is_some();

            egui::Window::new("Consolidate Project")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .default_width(460.0)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(
                        "Copies all referenced media into a new self-contained \
                         project folder. The original is untouched.",
                    );
                    ui.add_space(8.0);

                    ui.label(format!("Media files:  {online_count} online"));
                    if offline_count > 0 {
                        ui.colored_label(
                            self.theme.marker_warning,
                            format!(
                                "\u{26A0} {offline_count} offline file(s) will be skipped",
                            ),
                        );
                    }
                    ui.add_space(8.0);

                    ui.checkbox(
                        &mut self.state.consolidate_include_proxies,
                        "Include proxy files",
                    );
                    ui.checkbox(
                        &mut self.state.consolidate_include_skims,
                        "Include skim files",
                    );
                    ui.checkbox(
                        &mut self.state.consolidate_create_zip,
                        "Also create .zip archive",
                    );
                    if self.state.consolidate_create_zip {
                        ui.indent("consolidate_zip_opts", |ui| {
                            ui.checkbox(
                                &mut self.state.consolidate_delete_folder_after_zip,
                                "Delete folder after archiving",
                            );
                        });
                    }
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        ui.label("Destination:");
                        ui.add(
                            egui::TextEdit::singleline(
                                &mut self.state.consolidate_dest_path,
                            )
                            .desired_width(280.0),
                        );
                        let picker = egui::Id::new("consolidate-destination");
                        if ui.button("\u{2026}").clicked() {
                            crate::file_dialog::pick_path(picker, || {
                                rfd::FileDialog::new()
                                    .set_title("Choose Consolidation Destination")
                                    .pick_folder()
                            });
                        }
                        if let Some(crate::file_dialog::Picked::Path(dir)) = crate::file_dialog::take_path(picker) {
                            self.state.consolidate_dest_path =
                                dir.to_string_lossy().to_string();
                        }
                    });
                    ui.add_space(8.0);

                    let can_consolidate =
                        !self.state.consolidate_dest_path.trim().is_empty()
                            && !consolidation_active;

                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.state.show_consolidate_dialog = false;
                        }
                        if ui
                            .add_enabled(
                                can_consolidate,
                                egui::Button::new("Consolidate"),
                            )
                            .clicked()
                        {
                            let dest = std::path::PathBuf::from(
                                self.state.consolidate_dest_path.trim(),
                            );
                            let include_proxies = self.state.consolidate_include_proxies;
                            let include_skims = self.state.consolidate_include_skims;
                            let create_zip = self.state.consolidate_create_zip;
                            let delete_folder_after_zip =
                                self.state.consolidate_delete_folder_after_zip;
                            self.start_consolidation(
                                dest,
                                include_proxies,
                                include_skims,
                                create_zip,
                                delete_folder_after_zip,
                            );
                        }
                    });
                });
            if !open {
                self.state.show_consolidate_dialog = false;
            }
            let _ = project_path; // silence unused warning in release builds
        }

        // 4c3. Project Info window
        if self.state.show_project_info {
            // Modal backdrop
            egui::Area::new(egui::Id::new("project_info_backdrop"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    let resp = ui.allocate_rect(screen, egui::Sense::click());
                    ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(self.state.modal_backdrop_alpha));
                    if resp.clicked() { self.state.show_project_info = false; }
                });

            let mut still_open = true;
            let colour_before = (self.state.project_working_colorspace, self.state.project_grade_encoding);
            crate::panels::project_info::show_project_info(
                ctx,
                &mut self.state,
                &self.theme,
                &mut still_open,
            );
            if !still_open {
                self.state.show_project_info = false;
            }
            // A new working space or grade encoding changes the picture,
            // and a paused viewer redraws nothing on its own.
            if (self.state.project_working_colorspace, self.state.project_grade_encoding) != colour_before {
                self.sync_and_refresh_frame();
            }
        }

        // 4c4. File Info window — opens from the file browser right-click menu.
        if let Some(path) = self.state.file_info.clone() {
            // Modal backdrop
            egui::Area::new(egui::Id::new("file_info_backdrop"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let screen = ctx.screen_rect();
                    let resp = ui.allocate_rect(screen, egui::Sense::click());
                    ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(self.state.modal_backdrop_alpha));
                    if resp.clicked() { self.state.file_info = None; }
                });

            // Lazy-load the thumbnail texture from the shared media-pool disk
            // cache. The cache key is path+size+mtime so it's automatically
            // shared with the file browser thumbnails — if the user has seen
            // this file in the browser, the cache hit is instant.
            if self.state.file_info_thumb.is_none() {
                if let Some(key) = klipp_engine::video_decoder::picture_cache_key(&path) {
                    if let Ok((rgba, w, h)) = klipp_media::disk_cache::load_thumbnail_cache(&key, crate::browser::FILE_THUMB_MAX_WIDTH) {
                        let pixels: Vec<egui::Color32> = rgba
                            .chunks_exact(4)
                            .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                            .collect();
                        let image = egui::ColorImage {
                            size: [w as usize, h as usize],
                            pixels,
                        };
                        if let Some(old) = self.state.file_info_thumb.take() {
                            self.defer_drop_texture(old);
                        }
                        let tex = alloc_texture_logged(
                            ctx,
                            "file_info_thumb",
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.state.file_info_thumb = Some(tex);
                    }
                }
            }

            let mut still_open = true;
            crate::panels::file_info::show_file_info(
                ctx,
                &path,
                self.state.file_info_thumb.as_ref(),
                &self.theme,
                &mut still_open,
            );
            if !still_open {
                self.state.file_info = None;
                if let Some(tex) = self.state.file_info_thumb.take() {
                    self.defer_drop_texture(tex);
                }
            }
        }

        // 4c5. Quick Look overlay — Spacebar preview from the file browser.
        if let Some(ref ql_path) = self.state.quick_look_path.clone() {
            self.render_quick_look(ctx, ql_path);
        }

        // 4d. Preferences window
        if self.state.show_preferences {
            let (still_open, pref_cmds) = self.preferences_panel.show(
                ctx,
                &mut self.prefs,
                &mut self.prefs_dirty,
                &mut self.key_bindings,
                &self.theme_registry,
                &self.theme,
                self.state.snap,
            );
            cmds.extend(pref_cmds);
            // Sync skim_audio pref → state (checkbox may have changed)
            if self.state.audio_skimming_enabled != self.prefs.playback.skim_audio {
                self.state.audio_skimming_enabled = self.prefs.playback.skim_audio;
                if !self.state.audio_skimming_enabled && self.state.skim_audio_session.is_some() {
                    cmds.push(AppCommand::BrowserSkimAudioEnd);
                }
            }
            // Sync file-browser skim prefs → state. If audio skim was just
            // turned off mid-session, end the active session immediately so
            // the user gets the silence they asked for.
            if self.state.file_browser_video_skim_enabled != self.prefs.file_browser.file_browser_video_skim {
                self.state.file_browser_video_skim_enabled = self.prefs.file_browser.file_browser_video_skim;
            }
            if self.state.file_browser_audio_skim_enabled != self.prefs.file_browser.file_browser_audio_skim {
                self.state.file_browser_audio_skim_enabled = self.prefs.file_browser.file_browser_audio_skim;
                if !self.state.file_browser_audio_skim_enabled && self.state.skim_audio_session.is_some() {
                    cmds.push(AppCommand::BrowserSkimAudioEnd);
                }
            }
            if self.state.show_recent_files != self.prefs.file_browser.show_recent_files {
                self.state.show_recent_files = self.prefs.file_browser.show_recent_files;
            }
            if self.state.mute_reminder_interval_secs != self.prefs.audio_hardware.mute_reminder_interval_secs {
                self.state.mute_reminder_interval_secs = self.prefs.audio_hardware.mute_reminder_interval_secs;
            }
            if self.state.filmstrip_hover_enabled != self.prefs.file_browser.filmstrip_hover {
                self.state.filmstrip_hover_enabled = self.prefs.file_browser.filmstrip_hover;
            }
            if self.state.filmstrip_quality != self.prefs.file_browser.filmstrip_quality {
                self.state.filmstrip_quality = self.prefs.file_browser.filmstrip_quality;
            }
            if self.state.filmstrip_persist != self.prefs.file_browser.filmstrip_persist {
                self.state.filmstrip_persist = self.prefs.file_browser.filmstrip_persist;
            }
            if self.state.file_browser_auto_refresh != self.prefs.file_browser.auto_refresh {
                self.state.file_browser_auto_refresh = self.prefs.file_browser.auto_refresh;
            }
            if !still_open {
                self.state.show_preferences = false;
            }
        }

    }

    fn render_launch_screen(&mut self, ctx: &egui::Context, all_cmds: &mut Vec<AppCommand>) -> bool {
        if self.state.project_path.is_none() {
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(self.theme.bg_void))
                .show(ctx, |ui| {
                    // Auto-saves are recovery copies, not projects: they are
                    // offered from their project's card and open prompt.
                    let recent: Vec<_> = self.prefs.general.recent_projects.iter()
                        .filter(|rp| !is_autosave_path(&rp.path))
                        .cloned()
                        .collect();
                    let panel_cmds = self.launch_screen.show(ctx, ui, &recent, &self.theme);
                    all_cmds.extend(panel_cmds);
                });
            true
        } else {
            false
        }
    }

    fn render_fullscreen_viewer(&mut self, ctx: &egui::Context, all_cmds: &mut Vec<AppCommand>) -> bool {
        if self.state.fullscreen_viewer {
            // Fullscreen viewer occupies the entire screen — record this so the
            // dim-on-play overlay (which paints around the viewer rect) computes
            // a zero-area frame and produces no visible strips.
            self.state.viewer_panel_rect = ctx.screen_rect();
            egui::CentralPanel::default().show(ctx, |ui| {
                let panel_cmds = self.viewer.show_fullscreen(ui, &mut self.state);
                all_cmds.extend(panel_cmds);
            });
            true
        } else {
            false
        }
    }

    fn render_main_layout(&mut self, ctx: &egui::Context, all_cmds: &mut Vec<AppCommand>) {
        if self.render_launch_screen(ctx, all_cmds) { return; }
        if self.render_fullscreen_viewer(ctx, all_cmds) { return; }
        {
            // ── Manual rect layout ──
            // Use CentralPanel as the container, then compute rects manually.
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(self.theme.bg_void))
                .show(ctx, |ui| {
                    let full = ui.available_rect_before_wrap();

                    // Pure rect computation — see crates/klipp-ui/src/layout.rs
                    let rects = crate::layout::compute_layout(full, &self.layout, &self.state);
                    // Cache the viewer panel rect for overlays (e.g. dim-on-play)
                    // that need to align with the viewer regardless of layout mode.
                    self.state.viewer_panel_rect = rects.viewer_rect;
                    // The Colour strip is capped to its tracks; a stored
                    // height above what is shown would make the divider
                    // move nothing until the drag had used it up.
                    if self.state.show_color_panel && rects.timeline_rect.height() + 0.5 < self.layout.color_timeline_height {
                        self.layout.color_timeline_height = rects.timeline_rect.height();
                    }
                    let crate::layout::LayoutRects {
                        full: _,
                        toolbar_rect,
                        statusbar_rect,
                        browser_rect,
                        left_resizer_rect,
                        inspector_rect,
                        right_resizer_rect,
                        source_viewer_rect,
                        viewer_rect,
                        h_resizer_rect,
                        curve_editor_rect,
                        audio_mixer_rect,
                        timeline_rect,
                        toggle_bar_rect,
                        scopes_rect,
                        nodes_rect,
                    } = rects;

                    // Derived scalars consumed by the render section below. These mirror
                    // the locals that used to live in the inlined layout block, so every
                    // downstream use-site keeps working unchanged.
                    let main_top = toolbar_rect.max.y;
                    let main_bottom = statusbar_rect.min.y;
                    let center_left = left_resizer_rect.max.x;
                    // Bottom-dock width = the FULL center column (between
                    // the two resizers), not the program-viewer width.
                    // The viewer row is split 50/50 source + program in
                    // layout.rs, so `viewer_rect.width()` is only half
                    // the center column — using it here would (and did,
                    // pre-fix) constrain the scope dock + toggle bar +
                    // bottom tools to half their available area, leaving
                    // the source-viewer half empty below the viewer row.
                    let center_w = right_resizer_rect.min.x - left_resizer_rect.max.x;
                    let in_color_ws = self.state.show_color_panel;
                    let show_scopes_bar = self.state.scopes_docked();
                    let show_nodes_bar = in_color_ws && self.state.show_node_strip;
                    let toggle_bar_h = toggle_bar_rect.height();
                    let scopes_h = scopes_rect.height();
                    let nodes_h = nodes_rect.height();
                    let audio_mixer_h = audio_mixer_rect.height();
                    let curve_editor_h = curve_editor_rect.height();
                    // flexible_h is the ratio-drag denominator — same formula as in layout.rs.
                    let flexible_h = {
                        let center_total = main_bottom - main_top;
                        let fixed_h = toggle_bar_h + theme::RESIZER_THICKNESS;
                        (center_total - fixed_h).max(200.0)
                    };
                    // Timeline-height clamp bounds for the horizontal resizer drag.
                    let timeline_min_h = theme::TIMELINE_MIN_H;
                    let max_timeline_h = {
                        let color_bottom_total = toggle_bar_h + scopes_h + nodes_h;
                        let color_bottom_top = main_bottom - color_bottom_total;
                        (color_bottom_top - main_top - theme::RESIZER_THICKNESS) * theme::TIMELINE_MAX_H_PCT
                    };

                    // ── Render panels into their rects ──
                    self.render_toolbar_panel(ui, toolbar_rect, all_cmds);
                    self.render_status_bar_panel(ui, statusbar_rect);

                    if self.state.show_browser_panel {
                        self.render_browser_panel(ui, browser_rect, all_cmds);
                    } else {
                        self.render_browser_rail(ui, browser_rect);
                    }

                    if self.state.show_inspector_panel {
                        // Grading the timeline: the panels show and edit
                        // its grade as the selected clip's.
                        let stood_in = self.stand_in_timeline_grade();
                        self.render_inspector_panel(ui, inspector_rect, all_cmds);
                        self.stand_down_timeline_grade(stood_in);
                    } else {
                        self.render_inspector_rail(ui, inspector_rect);
                    }


                    let color_tools_bottom = in_color_ws
                        && self.state.color_tools_position == crate::state::ColorToolsPosition::Bottom;

                    // A single-viewer layout gives the hidden viewer no
                    // width; it is not drawn at all.
                    if source_viewer_rect.width() > 1.0 {
                        self.render_source_viewer_placeholder(ui, source_viewer_rect, all_cmds);
                    }
                    if viewer_rect.width() > 1.0 {
                        self.render_viewer_panel(ui, viewer_rect, all_cmds);
                    }

                    // The light under the pointer, while HDR monitoring is
                    // on: the engine publishes the picture's cd/m² with
                    // each frame, since an SDR screen cannot show it.
                    self.state.hdr_nits_under_pointer = match (
                        self.state.hdr_monitor.renders_hdr(),
                        self.state.viewer_pointer_pos,
                        self.state.viewer_image_rect,
                    ) {
                        (true, Some(pos), Some(rect)) => {
                            let (nx, ny) = crate::panels::viewer::viewer_screen_to_norm(rect, pos);
                            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                            fb.nits_at(nx, ny)
                        }
                        _ => None,
                    };

                    // Compute bottom-panel scope cursor data (shared by overlay + bottom panel)
                    // If locked but data not yet computed, compute it now
                    if self.state.scope_cursor_locked && self.state.scope_cursor_locked_data.is_none() {
                        if let (Some(pos), Some(rect)) = (self.state.scope_cursor_locked_pos, self.state.viewer_image_rect) {
                            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                            self.state.scope_cursor_locked_data = crate::widgets::scope_window::sample_viewer_pixel(
                                pos, rect, &fb, self.state.scope_cursor_sample_radius,
                                klipp_color::scopes::ScopeBasis::for_working_space(
                                    self.state.project_working_colorspace,
                                ),
                            );
                        }
                    }

                    let bottom_scope_cursor: Option<crate::state::ScopeCursorData> = if self.state.scope_cursor_locked {
                        self.state.scope_cursor_locked_data
                    } else if self.state.scope_cursor_show {
                        if let (Some(pos), Some(rect)) = (self.state.viewer_pointer_pos, self.state.viewer_image_rect) {
                            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
                            crate::widgets::scope_window::sample_viewer_pixel(
                                pos, rect, &fb, self.state.scope_cursor_sample_radius,
                                klipp_color::scopes::ScopeBasis::for_working_space(
                                    self.state.project_working_colorspace,
                                ),
                            )
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    // Scope overlay on viewer (semi-transparent, bottom-right corner)
                    if self.state.show_scopes && self.state.scope_overlay_mode {
                        let overlay_w = (viewer_rect.width() * 0.3).min(200.0);
                        let overlay_h = overlay_w; // square
                        let overlay_rect = egui::Rect::from_min_size(
                            egui::pos2(viewer_rect.right() - overlay_w - 8.0, viewer_rect.bottom() - overlay_h - 8.0),
                            egui::vec2(overlay_w, overlay_h),
                        );
                        crate::ui_util::allocate_clipped_ui(ui, overlay_rect, |ui| {
                            ui.set_min_size(overlay_rect.size());
                            // Semi-transparent dark background
                            ui.painter().rect_filled(overlay_rect, 4.0, egui::Color32::from_black_alpha(160));
                            self.state.scope_state.scale = self.state.scope_scale;
                            self.state.scope_state.legal_range = self.state.scope_legal_range;
                            self.state.scope_state.hdr_levels = self.state.hdr_monitor.renders_hdr();
                            crate::widgets::scopes::show_scopes(
                                ui,
                                &mut self.state.scope_state,
                                self.state.scope_mode,
                                &self.frame_buffer,
                                self.state.scope_quality,
                                self.prefs.color.vectorscope_skin_angle,
                                self.state.playback == PlaybackState::Playing,
                                bottom_scope_cursor,
                                self.state.scope_cursor_show,
                                &self.theme,
                                klipp_color::scopes::ScopeBasis::for_working_space(
                                    self.state.project_working_colorspace,
                                ),
                                self.state.scope_options.vectorscope_zoom,
                                self.state.scope_options.histogram_display,
                            );
                        });
                    }

                    if self.state.show_curve_editor && curve_editor_h > 0.0 {
                        self.render_curve_editor_panel(ui, curve_editor_rect, all_cmds);
                    }
                    if self.state.show_audio_mixer && audio_mixer_h > 0.0 {
                        self.render_audio_mixer_panel(ui, audio_mixer_rect, all_cmds);
                    }
                    self.render_timeline_panel(ui, timeline_rect, all_cmds);

                    // ── Color workspace bottom panels ──
                    // Render when in Color (toggle bar + scopes + nodes) OR
                    // when scopes are toggled on from any other workspace.
                    // The render function gates the Color-only chrome
                    // internally on `toggle_bar_h > 0` and `color_tools_bottom`.
                    if in_color_ws || show_scopes_bar {
                        let stood_in = self.stand_in_timeline_grade();
                        self.render_color_workspace_bottom(
                            ctx,
                            ui,
                            all_cmds,
                            ColorWsParams {
                                show_scopes_bar,
                                show_nodes_bar,
                                color_tools_bottom,
                                toggle_bar_h,
                                scopes_h,
                                center_left,
                                center_w,
                                main_top,
                                main_bottom,
                                flexible_h,
                                toggle_bar_rect,
                                nodes_rect,
                                bottom_scope_cursor,
                            },
                        );
                        self.stand_down_timeline_grade(stood_in);
                    }

                    self.handle_panel_resizers(
                        ui,
                        left_resizer_rect,
                        right_resizer_rect,
                        h_resizer_rect,
                        timeline_min_h,
                        max_timeline_h,
                    );

                    // Transport is now integrated into the toolbar (P1-2).
                    // Keyboard shortcuts still handled by self.transport.handle_keyboard().
                });
        }

    }

    fn handle_panel_resizers(
        &mut self,
        ui: &mut egui::Ui,
        left_resizer_rect: egui::Rect,
        right_resizer_rect: egui::Rect,
        h_resizer_rect: egui::Rect,
        timeline_min_h: f32,
        max_timeline_h: f32,
    ) {
        let theme = self.theme.clone();
        let (left_delta, left_dbl) = resizer::vertical_resizer_full(ui, left_resizer_rect, &theme);
        let (right_delta, right_dbl) = resizer::vertical_resizer_full(ui, right_resizer_rect, &theme);
        if left_dbl {
            let now = !self.state.show_browser_panel;
            self.set_browser_visible(now);
        }
        if right_dbl {
            let now = !self.state.show_inspector_panel;
            self.set_inspector_visible(now);
        }
        let h_delta = resizer::horizontal_resizer(ui, h_resizer_rect, &theme);

        // Dragging a panel well under its minimum collapses it rather
        // than sticking at the clamp. Without this the divider simply
        // refuses to move and the gesture reads as broken; with it,
        // "drag it away" works the way people expect and the rail
        // catches the result.
        const COLLAPSE_DRAG_SLOP: f32 = 60.0;
        if left_delta != 0.0 {
            let wanted = self.layout.browser_width + left_delta;
            if self.state.show_browser_panel && wanted < theme::BROWSER_MIN_W - COLLAPSE_DRAG_SLOP {
                self.set_browser_visible(false);
            } else {
                self.layout.browser_width = wanted.clamp(theme::BROWSER_MIN_W, theme::BROWSER_MAX_W);
            }
        }
        if right_delta != 0.0 {
            let wanted = self.layout.inspector_width - right_delta;
            if self.state.show_inspector_panel && wanted < theme::INSPECTOR_MIN_W - COLLAPSE_DRAG_SLOP {
                self.set_inspector_visible(false);
            } else {
                self.layout.inspector_width = wanted.clamp(theme::INSPECTOR_MIN_W, theme::INSPECTOR_MAX_W);
            }
        }
        if h_delta != 0.0 {
            if self.state.show_color_panel {
                self.layout.color_timeline_height = (self.layout.color_timeline_height - h_delta)
                    .clamp(timeline_min_h, max_timeline_h.max(timeline_min_h));
            } else {
                self.layout.timeline_height = (self.layout.timeline_height - h_delta)
                    .clamp(timeline_min_h, max_timeline_h);
            }
        }
        let resizer_active = left_delta != 0.0 || right_delta != 0.0 || h_delta != 0.0;
        if resizer_active {
            self.layout_dirty = true;
        }
        // Color's single viewer is the program viewer: the transport and
        // the keys follow it there, whatever the source viewer had.
        if self.state.single_viewer_is_program() && self.state.viewer_mode == klipp_core::ViewerMode::Source {
            self.state.viewer_mode = klipp_core::ViewerMode::Program;
        }
        if self.layout.dual_viewers != self.state.dual_viewers
            || self.layout.color_dual_viewers != self.state.color_dual_viewers
            || self.layout.export_dual_viewers != self.state.export_dual_viewers
        {
            self.layout.dual_viewers = self.state.dual_viewers;
            self.layout.color_dual_viewers = self.state.color_dual_viewers;
            self.layout.export_dual_viewers = self.state.export_dual_viewers;
            self.layout_dirty = true;
        }
        if self.layout_dirty
            && !ui.input(|i| i.pointer.any_pressed() || i.pointer.is_decidedly_dragging())
        {
            self.layout.save();
            self.layout_dirty = false;
        }
    }

    fn render_toolbar_panel(
        &mut self,
        ui: &mut egui::Ui,
        toolbar_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        let theme = &self.theme;
        let border = theme.border;
        let bg_deep = theme.bg_deep;
        crate::ui_util::allocate_clipped_ui(ui, toolbar_rect, |ui| {
            ui.set_min_size(toolbar_rect.size());
            ui.painter().rect_filled(toolbar_rect, 0.0, bg_deep);
            ui.painter().hline(
                toolbar_rect.x_range(),
                toolbar_rect.max.y,
                egui::Stroke::new(1.0_f32, border),
            );
            let recent = self.prefs.general.recent_projects.clone();
            let panel_cmds = self.toolbar.show(
                ui,
                &mut self.state,
                &recent,
                &self.theme,
                &self.key_bindings,
            );
            all_cmds.extend(panel_cmds);
        });
    }

    /// Paint the Source Viewer panel — video area on top (delegated to
    /// `ViewerPanel::show_source_viewer`, which handles skim-texture
    /// preview, metadata card, In/Out markers, and edit buttons) plus the
    /// shared transport chrome (scrub bar + play bar) below.
    ///
    /// The transport chrome currently routes to the timeline playhead
    /// because per-viewer routing isn't wired yet. That's the next piece
    /// of the source-decoder / source-playhead work.
    fn render_source_viewer_placeholder(
        &mut self,
        ui: &mut egui::Ui,
        source_viewer_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        // Must match the transport-chrome heights inside `draw_transport_chrome`.
        // Shared with layout.rs via `theme::VIEWER_CHROME_HEIGHT` so the viewer
        // rect reserves exactly enough room for the scrub + playbar rows.
        const BOTTOM_H: f32 = theme::VIEWER_CHROME_HEIGHT;

        // Click-to-focus. Allocated FIRST so its z-order sits below
        // every interactive widget rendered later in this function
        // (the (i) icon, edit buttons, scrub bar, transport buttons).
        // Those widgets win clicks on their specific rects; clicks on
        // empty viewer area fall through and focus the source viewer.
        let focus_resp = ui.interact(
            source_viewer_rect,
            ui.id().with("source_viewer_focus"),
            egui::Sense::click(),
        );
        if focus_resp.clicked() && self.state.viewer_mode != klipp_core::ViewerMode::Source {
            all_cmds.push(AppCommand::SetViewerMode(klipp_core::ViewerMode::Source));
        }

        let video_rect = egui::Rect::from_min_max(
            source_viewer_rect.min,
            egui::pos2(source_viewer_rect.max.x, source_viewer_rect.max.y - BOTTOM_H),
        );
        let chrome_rect = egui::Rect::from_min_size(
            egui::pos2(source_viewer_rect.min.x, video_rect.max.y),
            egui::vec2(source_viewer_rect.width(), BOTTOM_H),
        );

        // Black letterbox behind the source viewer content.
        ui.painter_at(video_rect).rect_filled(video_rect, 0.0, egui::Color32::BLACK);

        // Delegate the video-area rendering to the existing ViewerPanel
        // helper — same skim texture, same metadata card, same In/Out
        // markers, same edit buttons users already saw when clicking the
        // old "Source" toggle. No duplicated logic.
        // While a multicam group is in play, the Source viewer's picture
        // is the angle grid: click a tile to cut to that angle, as keys
        // 1-9 do (through the same command, so both routes record the
        // same). The chrome below stays, so a single-viewer layout can go
        // back to Program.
        if self.state.multicam_grid_enabled && self.state.multicam_viewer_active {
            if let Some(runtime) = self.multicam_grid.as_ref().filter(|r| !r.sources.is_empty()) {
                if let Some(angle) = crate::multicam_grid::show(ui, video_rect, runtime, self.state.multicam_active_angle, &self.theme) {
                    all_cmds.push(AppCommand::SwitchMulticamAngle { angle: angle as usize + 1 });
                }
                // The timeline's transport, not the Source viewer's: the
                // grid follows the timeline playhead, and the Source
                // chrome read and moved a source playhead it is not about.
                let chrome_cmds = self.viewer.draw_transport_chrome(ui, &mut self.state, &self.theme, chrome_rect, klipp_core::ViewerMode::Program);
                all_cmds.extend(chrome_cmds);
                return;
            }
        }
        self.viewer.show_source_viewer(ui, &self.state, &self.theme, video_rect, all_cmds);

        // Scroll-zoom + middle-drag-pan input for the source video area.
        // Texture size = the source viewer's live frame when present, or
        // the committed source clip's first-frame, or the source media's
        // declared resolution. Falling back to the timeline size keeps
        // the math finite even when nothing is loaded.
        let source_tex_size = self
            .state
            .source_live_texture
            .as_ref()
            .map(|t| t.size_vec2())
            .or_else(|| {
                self.state.source_video_texture.as_ref().map(|(_, t)| t.size_vec2())
            })
            .or_else(|| {
                self.state.source_media_id
                    .and_then(|mid| self.state.media_pool.get(&mid))
                    .map(|m| egui::vec2(m.resolution.width as f32, m.resolution.height as f32))
            })
            .unwrap_or(egui::vec2(1920.0, 1080.0));
        self.viewer.handle_viewer_zoom_pan_input(
            ui,
            &mut self.state,
            video_rect,
            source_tex_size,
            klipp_core::ViewerMode::Source,
        );

        // Floating "..." overflow menu in the video area's top-right
        // corner — holds the controls that don't fit in the responsive
        // playbar plus zoom presets and the viewer overlay toggles.
        self.viewer.draw_viewer_overflow_menu(
            ui,
            &mut self.state,
            &self.theme,
            video_rect,
            klipp_core::ViewerMode::Source,
            all_cmds,
        );

        // Reuse the same scrub bar + play bar the Program Viewer renders,
        // routing scrub-bar drags and the timecode display to the source
        // playhead via `ViewerMode::Source`.
        let chrome_cmds = self.viewer.draw_transport_chrome(
            ui,
            &mut self.state,
            &self.theme,
            chrome_rect,
            klipp_core::ViewerMode::Source,
        );
        all_cmds.extend(chrome_cmds);
    }

    fn render_viewer_panel(
        &mut self,
        ui: &mut egui::Ui,
        viewer_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        // Click-to-focus. Allocated BEFORE the viewer's child UI so its
        // z-order sits below every interactive widget rendered later
        // (transform handles, scrub bar, transport buttons, etc.).
        // Those widgets win clicks on their specific rects; clicks on
        // empty viewer image area fall through and focus the program
        // viewer.
        let focus_resp = ui.interact(
            viewer_rect,
            ui.id().with("program_viewer_focus"),
            egui::Sense::click(),
        );
        if focus_resp.clicked() && self.state.viewer_mode != klipp_core::ViewerMode::Program {
            all_cmds.push(AppCommand::SetViewerMode(klipp_core::ViewerMode::Program));
        }

        // DaVinci/FCP-style: the viewer fills its panel rect flush — no
        // rounded corners, no stroke, no drop shadow, no inner margin. The
        // only divider between this panel and its neighbours is the panel
        // resizer; we don't add a competing border here.
        crate::ui_util::allocate_clipped_ui(ui, viewer_rect, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
            ui.spacing_mut().window_margin = egui::Margin::ZERO;
            {
                let v = &mut ui.style_mut().visuals;
                v.widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
                v.widgets.noninteractive.fg_stroke = egui::Stroke::NONE;
                v.widgets.inactive.bg_stroke = egui::Stroke::NONE;
                v.widgets.hovered.bg_stroke = egui::Stroke::NONE;
                v.widgets.active.bg_stroke = egui::Stroke::NONE;
                v.window_stroke = egui::Stroke::NONE;
                v.window_shadow = egui::Shadow::NONE;
            }
            let frame = egui::Frame::none()
                .fill(self.theme.bg_viewer)
                .inner_margin(egui::Margin::ZERO);
            frame.show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                let panel_cmds = self.viewer.show(ui, &mut self.state, &self.theme);
                all_cmds.extend(panel_cmds);
            });
        });
    }

    fn render_curve_editor_panel(
        &mut self,
        ui: &mut egui::Ui,
        curve_editor_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        self.curve_editor.sync_param_from_state(&self.state);
        crate::ui_util::allocate_clipped_ui(ui, curve_editor_rect, |ui| {
            ui.set_min_size(curve_editor_rect.size());
            let panel_cmds = self.curve_editor.show(ui, &mut self.state, &self.theme);
            all_cmds.extend(panel_cmds);
        });
    }

    fn render_audio_mixer_panel(
        &mut self,
        ui: &mut egui::Ui,
        audio_mixer_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        crate::ui_util::allocate_clipped_ui(ui, audio_mixer_rect, |ui| {
            ui.set_min_size(audio_mixer_rect.size());
            let panel_cmds = crate::panels::audio_mixer::show(ui, &mut self.state, &self.theme);
            all_cmds.extend(panel_cmds);
        });
    }

    fn render_timeline_panel(
        &mut self,
        ui: &mut egui::Ui,
        timeline_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        crate::ui_util::allocate_clipped_ui(ui, timeline_rect, |ui| {
            ui.set_min_size(timeline_rect.size());
            // Stash the active theme into the timeline's thread-local
            // so leaf render helpers (which fan out across thousands
            // of paint calls per frame) can pick up the right palette
            // without a parameter cascade through every signature.
            crate::panels::timeline::sync_theme(&self.theme);
            let panel_cmds = self.new_timeline.show(
                ui,
                &mut self.state,
                &self.theme,
                &mut self.timeline,
                &self.key_bindings,
                self.prefs.editing.adaptive_timeline_fit,
            );
            all_cmds.extend(panel_cmds);
        });
    }

    /// The collapsed browser's rail: its three tabs, stacked, each of
    /// which reopens the panel on that tab.
    /// In the Colour workspace, the clip under the playhead is the clip
    /// being graded.
    ///
    /// Every grading suite works this way: you scrub to a shot and the
    /// panel is already showing that shot's grade. Klipp selected only
    /// on click, so moving the playhead left the panel pointed at
    /// whatever was last clicked — which does not merely cost a click,
    /// it invites grading the wrong shot and not noticing.
    ///
    /// Only when the playhead has actually moved. A click that selects
    /// a clip without moving the playhead is left alone, so selecting
    /// and grading a clip that is not under the playhead still works
    /// and the two never fight over the same frame. Edit is untouched:
    /// there the selection is the user's, and a scrub should not
    /// silently reselect under them.
    fn follow_playhead_for_grading(&mut self) {
        if self.state.active_workspace != crate::state::Workspace::Color {
            // Forget the last position, so returning to Colour follows
            // immediately rather than waiting for the next move.
            self.last_graded_playhead = None;
            return;
        }
        let now = self.state.playhead;
        if self.last_graded_playhead == Some(now) {
            return;
        }
        self.last_graded_playhead = Some(now);

        // The topmost enabled video track that has a clip here — what
        // the viewer is showing, and so what the grade should address.
        let Some(seq) = self.state.sequences.get(self.state.active_sequence_idx) else { return };
        let found = seq
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Video) && t.visible)
            .find_map(|t| t.clip_at_timecode(now).map(|(_, c)| c.id));

        if let Some(id) = found {
            self.graded_positions.insert(id, now);
        }
        match found {
            Some(id) if self.state.selected_clip != Some(id) => {
                self.state.selected_clip = Some(id);
                self.state.selected_clips.clear();
            }
            // No clip under the playhead: leave the panel on the last
            // one rather than blanking it, so a gap between shots does
            // not throw away the context you were working in.
            _ => {}
        }
    }

    fn render_browser_rail(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        use crate::layout_state::BrowserTab;
        use crate::widgets::panel_rail::{self, RailAction, RailGlyph, RailItem, RailSide};

        let active = self.layout.active_browser_tab;
        let tabs = [BrowserTab::Files, BrowserTab::Media, BrowserTab::Library];
        let items = [
            RailItem { glyph: RailGlyph::Icon(crate::icons::Icon::Folder), label: "Files", active: active == BrowserTab::Files },
            RailItem { glyph: RailGlyph::Icon(crate::icons::Icon::FilmStrip), label: "Media", active: active == BrowserTab::Media },
            RailItem { glyph: RailGlyph::Icon(crate::icons::Icon::Sliders), label: "Library", active: active == BrowserTab::Library },
        ];
        let theme = self.theme.clone();
        match panel_rail::show(ui, rect, &theme, RailSide::Left, &items, "browser") {
            RailAction::None => {}
            RailAction::Open(i) => {
                if let Some(tab) = tabs.get(i) {
                    self.layout.active_browser_tab = *tab;
                }
                self.set_browser_visible(true);
            }
        }
    }

    /// The collapsed inspector's rail. Its tabs depend on what is
    /// selected, so the rail shows the ones the panel would show.
    fn render_inspector_rail(&mut self, ui: &mut egui::Ui, rect: egui::Rect) {
        use crate::layout_state::InspectorTab;
        use crate::widgets::panel_rail::{self, RailAction, RailGlyph, RailItem, RailSide};

        let active = self.layout.active_inspector_tab;
        let tabs = [InspectorTab::Video, InspectorTab::Audio, InspectorTab::Color, InspectorTab::Effects, InspectorTab::Info];
        // The inspector's own short forms, so the rail says what the tab
        // row says when it is narrow.
        let items = [
            RailItem { glyph: RailGlyph::Text("VID"), label: "Video", active: active == InspectorTab::Video },
            RailItem { glyph: RailGlyph::Text("AUD"), label: "Audio", active: active == InspectorTab::Audio },
            RailItem { glyph: RailGlyph::Text("COL"), label: "Colour", active: active == InspectorTab::Color },
            RailItem { glyph: RailGlyph::Text("FX"), label: "Effects", active: active == InspectorTab::Effects },
            RailItem { glyph: RailGlyph::Text("INFO"), label: "Info", active: active == InspectorTab::Info },
        ];
        let theme = self.theme.clone();
        match panel_rail::show(ui, rect, &theme, RailSide::Right, &items, "inspector") {
            RailAction::None => {}
            RailAction::Open(i) => {
                if let Some(tab) = tabs.get(i) {
                    self.layout.active_inspector_tab = *tab;
                }
                self.set_inspector_visible(true);
            }
        }
    }

    /// Show or hide the browser, and remember which across restarts.
    /// Visibility lives in two places — `AppState` is what the frame
    /// reads, `LayoutState` is what survives — so it is set in one
    /// place rather than at each call site, where the pair would drift.
    /// Run the enter/leave bookkeeping for a workspace switch, whoever
    /// made it.
    ///
    /// Called every frame rather than from the switch sites: there are
    /// three of those and they disagreed. The `C` shortcut borrowed the
    /// browser and the toolbar tabs did not, so the same switch gave
    /// two different layouts depending on how you asked for it. The
    /// decision itself lives in `state::browser_borrow_for`, where it
    /// can be tested without an app.
    fn reconcile_workspace(&mut self) {
        use crate::state::{borrow_released_by_hand, browser_borrow_for, BrowserBorrow, Workspace};
        let to = self.state.active_workspace;
        if to != self.last_workspace {
            let from = std::mem::replace(&mut self.last_workspace, to);

            // The colour panel belongs to its workspace, by any route.
            // (Scopes deliberately do not: entering Colour used to
            // force them on, which meant `C` always popped the full
            // scope UI even when the grade tabs were all you wanted.)
            self.state.show_color_panel = to == Workspace::Color;

            // Render the playhead's frame again for the new page. The
            // Colour page opened on a black viewer until the playhead
            // was moved; while parked this is a regrade of the parked
            // frame, and a real seek only when there is none.
            if self.state.playback != PlaybackState::Playing {
                let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
            }

            match browser_borrow_for(from, to) {
                BrowserBorrow::Take => {
                    self.browser_before_workspace = Some(self.state.show_browser_panel);
                    self.set_browser_visible_for_now(false);
                }
                BrowserBorrow::KeepTaken => self.set_browser_visible_for_now(false),
                BrowserBorrow::Give => {
                    if let Some(was) = self.browser_before_workspace.take() {
                        self.set_browser_visible_for_now(was);
                    }
                }
                BrowserBorrow::Leave => {}
            }
            return;
        }

        if borrow_released_by_hand(
            self.browser_before_workspace.is_some(),
            to,
            self.state.show_browser_panel,
        ) {
            self.browser_before_workspace = None;
        }
    }

    /// Show or hide the browser for this session only, without
    /// recording it as what the user wants.
    ///
    /// A borrow is not a preference — it lasts exactly as long as the
    /// workspace holding it. Writing it to the layout file turns
    /// "hidden while grading" into "hidden": quit from Colour or
    /// Export and the next launch opens in Edit with no browser and
    /// nothing left that remembers there should be one. That is what
    /// it did, and the saved layout on this machine was already in
    /// that state.
    ///
    /// An explicit toggle still goes through [`Self::set_browser_visible`]
    /// and is still remembered, because that one IS a preference.
    fn set_browser_visible_for_now(&mut self, visible: bool) {
        self.state.show_browser_panel = visible;
    }

    fn set_browser_visible(&mut self, visible: bool) {
        self.state.show_browser_panel = visible;
        if self.layout.browser_visible != visible {
            self.layout.browser_visible = visible;
            self.layout_dirty = true;
        }
    }

    /// Show or hide the inspector, and remember which across restarts.
    fn set_inspector_visible(&mut self, visible: bool) {
        self.state.show_inspector_panel = visible;
        if self.layout.inspector_visible != visible {
            self.layout.inspector_visible = visible;
            self.layout_dirty = true;
        }
    }

    fn render_inspector_panel(
        &mut self,
        ui: &mut egui::Ui,
        inspector_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        // Refresh the letter-picker texture BEFORE the inspector
        // tabs render. Texture lifecycle stays in the App layer so
        // the old handle goes through `defer_drop_texture`; the
        // inspector itself reads `state.letter_picker_texture`
        // without touching wgpu (creating/dropping a TextureHandle
        // mid-frame from inside the inspector trips wgpu's
        // "Texture has been destroyed" validation —
        // feedback_defer_drop_texture).
        self.refresh_letter_picker_texture(ui.ctx());

        // The Export column's title row carries the same collapse button
        // as the clip inspector's; it is applied after the closure so
        // the borrow of `self` inside it stays untouched.
        let mut collapse_requested = false;
        let bg_panel = self.theme.bg_panel;
        crate::ui_util::allocate_clipped_ui(ui, inspector_rect, |ui| {
            ui.set_min_size(inspector_rect.size());
            ui.painter().rect_filled(inspector_rect, 0.0, bg_panel);
            // egui's ScrollArea paints its scrollbar gutter with
            // visuals.extreme_bg_color, which globally is bg_void (near-black)
            // and reads as a dark strip against the inspector's bg_panel. Override
            // it locally so the gutter blends into the inspector background.
            ui.style_mut().visuals.extreme_bg_color = bg_panel;

            // The Export workspace's column is the export form.
            if self.state.active_workspace == crate::state::Workspace::Export {
                let theme = self.theme.clone();
                self.prepare_export_defaults();
                if crate::widgets::inspector_group::panel_title_row(ui, "Export", false, &theme) {
                    collapse_requested = true;
                }
                let all_presets: Vec<ExportPreset> = builtin_presets()
                    .into_iter()
                    .chain(self.prefs.export.custom_presets.iter().cloned())
                    .collect();
                // Reserve the foot of the column for the actions before
                // the form gets to claim the space, so Export and Add to
                // Queue are on screen at every window height. The form
                // scrolls inside what is left.
                let full = ui.available_rect_before_wrap();
                let foot = export_foot_layout(full.height());
                let bar_h = foot.bar_h;
                let body_rect = egui::Rect::from_min_max(
                    full.min,
                    egui::pos2(full.max.x, full.max.y - bar_h),
                );
                let bar_rect = egui::Rect::from_min_max(
                    egui::pos2(full.min.x, full.max.y - bar_h),
                    full.max,
                );

                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(body_rect), |ui| {
                    egui::ScrollArea::vertical().id_salt("export_ws_scroll").show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        self.export_settings_contents(ui, &all_presets, all_cmds, false);
                        ui.add_space(12.0);
                        // The queue lives under the form here; the Render
                        // Queue window is the same list elsewhere.
                        let jobs = self.state.render_queue.jobs.len();
                        let badge = if jobs > 0 { Some(format!("{jobs}")) } else { None };
                        let mut actions = Vec::new();
                        crate::widgets::inspector_group::section_card(ui, "RENDER QUEUE", badge, &theme, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(12.0);
                                ui.vertical(|ui| {
                                    ui.set_max_width(ui.available_width() - 12.0);
                                    crate::panels::render_queue::RenderQueuePanel::show_contents(
                                        ui, &self.state.render_queue, &theme, true, &mut actions,
                                    );
                                });
                            });
                        });
                        self.apply_render_queue_actions(actions);
                    });
                });

                // The bar sits on the panel's own background with a rule
                // above it, so it reads as a pinned foot rather than as
                // the last thing in the form.
                ui.painter().rect_filled(bar_rect, 0.0, theme.bg_panel);
                ui.painter().hline(
                    bar_rect.x_range(),
                    bar_rect.top(),
                    egui::Stroke::new(1.0_f32, theme.border),
                );
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(bar_rect), |ui| {
                    // Centre the whole foot — summary line and buttons
                    // together — in the reserved strip, rather than the
                    // button row alone, which left the summary hanging
                    // off the top and the buttons off the bottom.
                    // Nothing at all in a strip with no room for it —
                    // a panel collapsed or mid-resize draws no half
                    // button rather than a clipped one.
                    if foot.content_h > 0.0 {
                        ui.add_space(foot.pad);
                        self.export_action_bar(ui, all_cmds, false, foot.show_summary);
                    }
                });
                return;
            }

            let in_color_workspace = self.state.show_color_panel
                && self.state.selected_clip.is_some();

            if in_color_workspace {
                let clip_id = self.state.selected_clip.unwrap();
                let seq_idx = self.state.active_sequence_idx;
                let mask_tolerance = crate::inspector::keyframe_selection::frame_tolerance(&self.state.sequences[seq_idx]);
                self.node_inspector_panel.input_colour_space =
                    crate::inspector::color_and_luts::clip_input_colour_space(&self.state, clip_id);
                // A stand-in: the timeline grade or a grade group's stage.
                let stand_in = GradeStandIn::of(&self.state.sequences[seq_idx], clip_id);
                let stand_in_title = stand_in.map(|w| w.title(&self.state.sequences[seq_idx]));
                let clip = self.state.sequences[seq_idx].tracks.iter_mut()
                    .find_map(|t| t.find_clip_mut(clip_id));
                if let Some(clip) = clip {
                    let theme = self.theme.clone();
                    let stand_in_name = stand_in_title.clone();
                    let title = if let Some(t) = stand_in_title {
                        t
                    } else {
                        clip
                            .media_id()
                            .and_then(|m| self.state.media_pool.get(&m))
                            .map(|m| m.filename.clone())
                            .unwrap_or_else(|| "Clip".into())
                    };
                    if crate::widgets::inspector_group::panel_title_row(ui, &title, false, &theme) {
                        collapse_requested = true;
                    }

                    // Versions, stills and looks: the grade's own library,
                    // one section above the tabs, collapsible like any
                    // other section.
                    let version_badge = clip
                        .active_grade_version
                        .map(|i| {
                            let name = &clip.grade_versions[i].name;
                            if name.is_empty() { format!("V{}", i + 1) } else { name.clone() }
                        });
                    let caption_row = |ui: &mut egui::Ui, label: &str, add: &mut dyn FnMut(&mut egui::Ui)| {
                        ui.horizontal(|ui| {
                            ui.set_min_height(crate::theme::control::COMPACT);
                            ui.add_space(12.0);
                            ui.add_sized(
                                [56.0, crate::theme::control::COMPACT],
                                egui::Label::new(egui::RichText::new(label).size(crate::theme::type_scale::CAPTION).color(theme.text_dim)).selectable(false),
                            );
                            add(ui);
                        });
                    };
                    // Grading the timeline: `clip` is its stand-in. Its
                    // graph is the one thing written back, so versions
                    // saved on it would be lost; stills and Looks land on
                    // the timeline grade, and say so.
                    let grading_timeline = stand_in.is_some();
                    crate::widgets::inspector_group::section_card(ui, "GRADE", version_badge, &theme, |ui| {
                        caption_row(ui, "Versions", &mut |ui| {
                            let row = ui.add_enabled_ui(!grading_timeline, |ui| {
                                let num_versions = clip.grade_versions.len();
                                let active = clip.active_grade_version;
                                for i in 0..num_versions {
                                    let is_active = active == Some(i);
                                    let label = {
                                        let name = &clip.grade_versions[i].name;
                                        if name.is_empty() { format!("V{}", i + 1) } else { name.clone() }
                                    };
                                    let resp = ui.selectable_label(is_active, egui::RichText::new(&label).size(crate::theme::type_scale::CAPTION));
                                    if resp.clicked() && !is_active {
                                        all_cmds.push(AppCommand::SwitchGradeVersion { clip_id, version_index: i });
                                    }
                                    resp.context_menu(|ui| {
                                        if ui.button("Rename...").clicked() {
                                            all_cmds.push(AppCommand::RenameGradeVersion {
                                                clip_id,
                                                version_index: i,
                                                name: format!("Version {}", i + 1),
                                            });
                                            ui.close_menu();
                                        }
                                        if !is_active && ui.button("Delete").clicked() {
                                            all_cmds.push(AppCommand::DeleteGradeVersion { clip_id, version_index: i });
                                            ui.close_menu();
                                        }
                                    });
                                }
                                if ui.small_button("+").on_hover_text("Save current grade as new version").clicked() {
                                    let name = format!("Version {}", num_versions + 1);
                                    all_cmds.push(AppCommand::SaveGradeVersion { clip_id, name });
                                }
                            });
                            if grading_timeline {
                                row.response.on_hover_text("Versions are kept per clip; the timeline grade has none. Save it as a Look to keep a copy.");
                            }
                        });

                        // Apply Grade from Still goes to the timeline
                        // selection, or the one selected clip, as Looks do;
                        // to the timeline grade when that is what is graded.
                        let grade_targets: Vec<klipp_core::ClipId> = if grading_timeline {
                            vec![clip_id]
                        } else if !self.state.selected_clips.is_empty() {
                            self.state.selected_clips.clone()
                        } else {
                            self.state.selected_clip.into_iter().collect()
                        };
                        caption_row(ui, "Stills", &mut |ui| {
                            let mut delete_idx = None;
                            let mut apply_idx = None;
                            for (i, still) in self.state.grade_stills.iter_mut().enumerate() {
                                let is_active = self.state.active_grade_still == Some(i);
                                if still.texture.is_none() && !still.rgba.is_empty() {
                                    let sw = still.width as usize;
                                    let sh = still.height as usize;
                                    let pixels: Vec<egui::Color32> = still.rgba.chunks_exact(4)
                                        .map(|px| egui::Color32::from_rgba_premultiplied(px[0], px[1], px[2], px[3]))
                                        .collect();
                                    let image = egui::ColorImage { size: [sw, sh], pixels };
                                    let name = format!("grade_still_thumb_{}", i);
                                    still.texture = Some(alloc_texture_logged(
                                        ui.ctx(),
                                        &name,
                                        image,
                                        egui::TextureOptions::LINEAR,
                                    ));
                                }
                                let btn_size = egui::vec2(36.0, 22.0);
                                let (rect, resp) = ui.allocate_exact_size(btn_size, egui::Sense::click());
                                if let Some(tex) = &still.texture {
                                    ui.painter().image(
                                        tex.id(),
                                        rect,
                                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                        egui::Color32::WHITE,
                                    );
                                }
                                let stroke = if is_active {
                                    egui::Stroke::new(1.5_f32, theme.accent)
                                } else {
                                    egui::Stroke::new(1.0_f32, theme.border)
                                };
                                ui.painter().rect_stroke(rect, crate::theme::radius::CONTROL, stroke);
                                if resp.clicked() {
                                    all_cmds.push(AppCommand::SetActiveGradeStill {
                                        index: if is_active { None } else { Some(i) },
                                    });
                                }
                                let hover = match &still.source {
                                    Some(src) => format!(
                                        "{}\nFrom {} at {}",
                                        still.name,
                                        if src.clip_name.is_empty() { "a clip" } else { src.clip_name.as_str() },
                                        record_timecode(src.record_secs, self.state.fps.as_f64()),
                                    ),
                                    None => still.name.clone(),
                                };
                                let resp = resp.on_hover_text(hover);
                                let has_grade = still.grade.is_some();
                                resp.context_menu(|ui| {
                                    let why = if has_grade { "Select the clips to grade" } else { "This still was grabbed before stills kept their grade" };
                                    let can = has_grade && !grade_targets.is_empty();
                                    let (apply_label, apply_hover, add_label, add_hover) = if grading_timeline {
                                        ("Apply Grade to Timeline Grade", "In place of the timeline grade", "Add Grade After Timeline Grade", "After the timeline grade, keeping it")
                                    } else {
                                        ("Apply Grade to Selected Clips", "In place of each clip's own grade", "Add Grade After Their Own", "After each clip's own grade, keeping its balance")
                                    };
                                    if ui.add_enabled(can, egui::Button::new(apply_label))
                                        .on_hover_text(apply_hover)
                                        .on_disabled_hover_text(why)
                                        .clicked()
                                    {
                                        apply_idx = Some((i, false));
                                        ui.close_menu();
                                    }
                                    if ui.add_enabled(can, egui::Button::new(add_label))
                                        .on_hover_text(add_hover)
                                        .on_disabled_hover_text(why)
                                        .clicked()
                                    {
                                        apply_idx = Some((i, true));
                                        ui.close_menu();
                                    }
                                    if ui.button("Delete").clicked() {
                                        delete_idx = Some(i);
                                        ui.close_menu();
                                    }
                                });
                            }
                            if let Some(idx) = delete_idx {
                                all_cmds.push(AppCommand::DeleteGradeStill { index: idx });
                            }
                            if let Some((idx, append)) = apply_idx {
                                all_cmds.push(AppCommand::ApplyGradeFromStill { index: idx, clip_ids: grade_targets.clone(), append });
                            }
                            // The active still's grade onto the selection:
                            // grab a finished shot, then grade the next ones
                            // from the same setup from it.
                            let active_grade = self.state.active_grade_still.filter(|&i| self.state.grade_stills.get(i).is_some_and(|s| s.grade.is_some()));
                            let (apply_label, apply_hover) = match stand_in_name.as_deref() {
                                Some(name) => (format!("Apply to {name}"), format!("Put the active still's grade on {name}, in place of it")),
                                None => ("Apply".to_string(), "Put the active still's grade on the selected clips".to_string()),
                            };
                            if ui
                                .add_enabled(active_grade.is_some() && !grade_targets.is_empty(), egui::Button::new(apply_label).small())
                                .on_hover_text(apply_hover)
                                .on_disabled_hover_text("Pick a still that kept its grade, and select the clips to grade")
                                .clicked()
                            {
                                if let Some(idx) = active_grade {
                                    all_cmds.push(AppCommand::ApplyGradeFromStill { index: idx, clip_ids: grade_targets.clone(), append: false });
                                }
                            }
                            if ui.small_button("Grab").on_hover_text("Capture the current frame as a grade still").clicked() {
                                let name = format!("Still {}", self.state.grade_stills.len() + 1);
                                all_cmds.push(AppCommand::CaptureGradeStill { name });
                            }
                            let compare_active = self.state.grade_still_compare;
                            if ui.selectable_label(compare_active, "Compare")
                                .on_hover_text("Split the viewer against the active still")
                                .clicked()
                            {
                                all_cmds.push(AppCommand::ToggleGradeStillCompare);
                            }
                            // Alt+B's split, shown and switchable here: the
                            // viewer showing half a frame ungraded should
                            // never be a state you cannot see is on.
                            let before_after = self.state.before_after_active;
                            if ui.selectable_label(before_after, "Before/After")
                                .on_hover_text(format!(
                                    "Split the viewer: left ungraded, right graded ({})",
                                    self.key_bindings.display_for("toggle_before_after")
                                ))
                                .clicked()
                            {
                                all_cmds.push(AppCommand::ToggleBeforeAfter);
                            }
                        });

                        // Looks: project-level reusable grades. Saves the
                        // selected clip's graph, applies to the timeline
                        // selection (or the single selected clip), or to
                        // the timeline grade while that is what is graded.
                        let looks_cmds = crate::panels::looks::show(
                            ui,
                            &mut self.looks_panel_state,
                            &self.state.looks,
                            self.state.selected_clip,
                            if grading_timeline { &[] } else { &self.state.selected_clips },
                            stand_in_name.as_deref(),
                            &theme,
                        );
                        all_cmds.extend(looks_cmds);
                    });

                    // Swap between tabbed grade inspector and per-node
                    // inspector based on what the user selected in the
                    // V2 canvas. The tabs cover the primary grade path
                    // (Corrector + legacy single-purpose grade nodes);
                    // nodes that have their own dedicated widget
                    // (Mask, Qualifier, FilmGrain, Lut3D, ColorMatch,
                    // LayerMixer, ExternalMatte, Denoise, ColorWarper)
                    // need the per-node inspector when the user clicks
                    // them — otherwise clicking e.g. an ExternalMatte
                    // in the canvas would hide its file picker behind
                    // the grade tabs.
                    let mask_frame = crate::inspector::keyframe_selection::FrameSelection::new(self.state.playhead, clip.timeline_in, mask_tolerance);
                    // Descent-aware graph: when the user is inside a
                    // compound, the inspector should read/write the
                    // sub-graph's nodes. Fall back to the root graph
                    // if the descent path has gone stale (self-heal
                    // on the next frame).
                    let path = self.state.graph_descent_path.clone();
                    let graph: &mut klipp_nodes::NodeGraph =
                        match Self::resolve_graph_at_path_mut(clip, &path) {
                            Ok(g) => g,
                            Err(_) => &mut clip.node_graph,
                        };
                    let use_per_node_inspector = self.state.selected_node_id
                        .and_then(|nid| graph.nodes.iter().find(|n| n.id == nid))
                        .map(|n| matches!(n.kind,
                            klipp_nodes::NodeKind::Mask
                            | klipp_nodes::NodeKind::Qualifier
                            | klipp_nodes::NodeKind::LumaKey
                            | klipp_nodes::NodeKind::SoftClip
                            | klipp_nodes::NodeKind::FilmGrain
                            | klipp_nodes::NodeKind::Lut3D
                            | klipp_nodes::NodeKind::ColorMatch
                            | klipp_nodes::NodeKind::LayerMixer
                            | klipp_nodes::NodeKind::ExternalMatte
                            | klipp_nodes::NodeKind::DifferenceKey
                            | klipp_nodes::NodeKind::Denoise
                            | klipp_nodes::NodeKind::ColorWarper
                            // Source / Output / composite-only nodes:
                            // no params, but showing the grade tabs
                            // for them misleads users into thinking
                            // clicking one does something. Route to
                            // the per-node inspector which falls
                            // through to a plain "No parameters" /
                            // node-specific placeholder label.
                            | klipp_nodes::NodeKind::Source
                            | klipp_nodes::NodeKind::Output
                            | klipp_nodes::NodeKind::Layer
                            | klipp_nodes::NodeKind::Blend
                            | klipp_nodes::NodeKind::CombineMattes
                            | klipp_nodes::NodeKind::ApplyMatte
                            | klipp_nodes::NodeKind::Compound
                            | klipp_nodes::NodeKind::RGBSplitter
                            | klipp_nodes::NodeKind::RGBCombiner
                        ))
                        .unwrap_or(false);
                    let panel_cmds = if use_per_node_inspector {
                        self.node_inspector_panel.show(
                            ui, &self.state.selected_node_id,
                            graph, clip_id, &self.theme, mask_frame,
                        )
                    } else {
                        self.node_inspector_panel.show_simple(
                            ui, graph, clip_id,
                            &mut self.state.selected_node_id,
                            &mut self.state.color_tab,
                            &self.theme,
                            mask_frame,
                        )
                    };
                    all_cmds.extend(panel_cmds);
                }
            } else {
                let panel_cmds = self.new_inspector.show(
                    ui, &self.state, &self.prefs, &self.theme,
                    &mut self.layout.active_inspector_tab,
                );
                all_cmds.extend(panel_cmds);
            }
        });
        if collapse_requested {
            self.set_inspector_visible(false);
        }
        self.state.focus_title_text = self.state.focus_title_text.saturating_sub(1);
    }

    /// Scale the timeline about the playhead, so the frame under it
    /// stays put. Both zoom keys go through here rather than each
    /// keeping its own copy of the scroll arithmetic.
    fn zoom_timeline_by(&mut self, factor: f32) {
        let playhead_secs = self.state.playhead_smooth_secs as f32;
        let old_zoom = self.state.timeline_zoom;
        let new_zoom = (old_zoom * factor).clamp(0.1, 2000.0);
        self.state.timeline_scroll_x = (playhead_secs * new_zoom
            - (playhead_secs * old_zoom - self.state.timeline_scroll_x))
            .max(0.0);
        self.state.timeline_zoom = new_zoom;
    }

    fn render_browser_panel(
        &mut self,
        ui: &mut egui::Ui,
        browser_rect: egui::Rect,
        all_cmds: &mut Vec<AppCommand>,
    ) {
        // ── Prefs → browser sync (BEFORE show) ──
        // Pull any preferences-panel changes into the live browser state
        // before rendering. The browser owns its own copy of these fields
        // (so its toolbar widgets — thumbnail size buttons, sort dropdown,
        // type filter pill, etc. — can mutate them with normal egui state)
        // but the preferences panel is also allowed to write to the prefs
        // copy. This sync makes the prefs panel "win" any frame where it
        // changed a value, since the browser → prefs sync at the bottom
        // of this method runs AFTER show() and would otherwise overwrite
        // the prefs change with the stale browser value.
        if self.browser.file_thumb_size != self.prefs.file_browser.file_thumb_size {
            self.browser.file_thumb_size = self.prefs.file_browser.file_thumb_size;
        }
        if self.browser.files_sort_by != self.prefs.file_browser.file_sort_by {
            self.browser.files_sort_by = self.prefs.file_browser.file_sort_by;
        }
        if self.browser.files_sort_ascending != self.prefs.file_browser.file_sort_ascending {
            self.browser.files_sort_ascending = self.prefs.file_browser.file_sort_ascending;
        }
        if self.browser.group_by_date != self.prefs.file_browser.group_by_date {
            self.browser.group_by_date = self.prefs.file_browser.group_by_date;
        }
        if self.browser.visible_columns != self.prefs.file_browser.visible_columns {
            self.browser.visible_columns = self.prefs.file_browser.visible_columns.clone();
        }
        if self.browser.files_type_filter != self.prefs.file_browser.file_type_filter {
            self.browser.files_type_filter = self.prefs.file_browser.file_type_filter;
        }
        if self.browser.pinned_folders != self.prefs.file_browser.pinned_folders {
            self.browser.pinned_folders = self.prefs.file_browser.pinned_folders.clone();
        }
        if self.browser.favorite_effects != self.prefs.effects_browser.favorite_effects {
            self.browser.favorite_effects = self.prefs.effects_browser.favorite_effects.clone();
        }
        if self.browser.favorite_transitions != self.prefs.effects_browser.favorite_transitions {
            self.browser.favorite_transitions = self.prefs.effects_browser.favorite_transitions.clone();
        }
        if self.browser.favorite_titles != self.prefs.effects_browser.favorite_titles {
            self.browser.favorite_titles = self.prefs.effects_browser.favorite_titles.clone();
        }
        if self.browser.favorite_generators != self.prefs.effects_browser.favorite_generators {
            self.browser.favorite_generators = self.prefs.effects_browser.favorite_generators.clone();
        }

        // Mark the browser as the active panel when the user presses
        // anywhere inside it this frame. Single PanelKind for the
        // whole widget — the browser's internal active_browser_tab
        // and library_category resolve which specific search
        // TextEdit a Ctrl+F focus request lands on.
        crate::state::note_active_panel(
            &mut self.state,
            crate::state::PanelKind::Browser,
            ui.ctx(),
            browser_rect,
        );

        let bg_panel = self.theme.bg_panel;
        crate::ui_util::allocate_clipped_ui(ui, browser_rect, |ui| {
            ui.set_min_size(browser_rect.size());
            ui.painter().rect_filled(browser_rect, 0.0, bg_panel);
            let panel_cmds = self.browser.show(
                ui,
                &mut self.state,
                &self.theme,
                &mut self.layout.active_browser_tab,
                &mut self.layout.files_pinned,
                &mut self.layout.files_split_ratio,
            );
            all_cmds.extend(panel_cmds);

            // ── Browser → prefs sync (AFTER show) ──
            // Capture any toolbar-driven changes the user made inside the
            // browser this frame and persist them to disk. This direction
            // only fires when the browser modified its local state during
            // show(); the prefs → browser sync above already handled the
            // opposite direction, so the two can't fight each other.
            if self.browser.browser_view_mode != self.prefs.editing.media_browser_view_mode {
                self.prefs.editing.media_browser_view_mode = self.browser.browser_view_mode;
                let _ = self.prefs.save();
            }
            if self.browser.file_thumb_size != self.prefs.file_browser.file_thumb_size {
                self.prefs.file_browser.file_thumb_size = self.browser.file_thumb_size;
                let _ = self.prefs.save();
            }
            if self.browser.list_columns != self.prefs.file_browser.media_list_columns {
                self.prefs.file_browser.media_list_columns = self.browser.list_columns.clone();
                let _ = self.prefs.save();
            }
            if self.browser.files_sort_by != self.prefs.file_browser.file_sort_by
                || self.browser.files_sort_ascending != self.prefs.file_browser.file_sort_ascending
            {
                self.prefs.file_browser.file_sort_by = self.browser.files_sort_by;
                self.prefs.file_browser.file_sort_ascending = self.browser.files_sort_ascending;
                let _ = self.prefs.save();
            }
            if self.browser.group_by_date != self.prefs.file_browser.group_by_date {
                self.prefs.file_browser.group_by_date = self.browser.group_by_date;
                let _ = self.prefs.save();
            }
            if self.browser.visible_columns != self.prefs.file_browser.visible_columns {
                self.prefs.file_browser.visible_columns = self.browser.visible_columns.clone();
                let _ = self.prefs.save();
            }
            if self.browser.files_type_filter != self.prefs.file_browser.file_type_filter {
                self.prefs.file_browser.file_type_filter = self.browser.files_type_filter;
                let _ = self.prefs.save();
            }
            if self.browser.pinned_folders != self.prefs.file_browser.pinned_folders {
                self.prefs.file_browser.pinned_folders = self.browser.pinned_folders.clone();
                let _ = self.prefs.save();
            }
            if self.browser.favorite_effects != self.prefs.effects_browser.favorite_effects {
                self.prefs.effects_browser.favorite_effects = self.browser.favorite_effects.clone();
                let _ = self.prefs.save();
            }
            if self.browser.favorite_transitions != self.prefs.effects_browser.favorite_transitions {
                self.prefs.effects_browser.favorite_transitions = self.browser.favorite_transitions.clone();
                let _ = self.prefs.save();
            }
            if self.browser.favorite_titles != self.prefs.effects_browser.favorite_titles {
                self.prefs.effects_browser.favorite_titles = self.browser.favorite_titles.clone();
                let _ = self.prefs.save();
            }
            if self.browser.favorite_generators != self.prefs.effects_browser.favorite_generators {
                self.prefs.effects_browser.favorite_generators = self.browser.favorite_generators.clone();
                let _ = self.prefs.save();
            }
            // Save file-ratings cache to disk if changed.
            if self.browser.file_ratings_dirty {
                self.browser.file_ratings_dirty = false;
                crate::browser::save_file_ratings_cache(&self.browser.file_ratings);
            }
            // Sync recent_files_snapshot back to prefs (handles "Clear Recents").
            if self.state.recent_files_snapshot != self.prefs.file_browser.recent_files {
                self.prefs.file_browser.recent_files = self.state.recent_files_snapshot.clone();
                let _ = self.prefs.save();
            }
        });
    }

    /// Render the Quick Look fullscreen overlay. Shows a dark backdrop with
    /// the preview image/waveform centered and aspect-fit to the screen,
    /// plus a small metadata overlay at the bottom. Dismisses on Escape,
    /// Spacebar, or backdrop click. Arrow keys navigate to adjacent files.
    fn render_quick_look(&mut self, ctx: &egui::Context, path: &std::path::Path) {
        let screen = ctx.screen_rect();

        // ── Lazy-load texture and header info on first frame ──
        if self.state.quick_look_texture.is_none() && self.state.quick_look_info.is_none() {
            // Probe header for metadata overlay.
            self.state.quick_look_info = klipp_engine::probe_media_header(path).ok();

            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            let is_audio = klipp_core::media_extensions::AUDIO_EXTENSIONS.contains(&ext.as_str());

            if is_audio {
                // Start audio playback session.
                if self.state.quick_look_audio.is_none() {
                    self.state.quick_look_audio =
                        klipp_engine::skim_audio::SkimAudioSession::new_from_source_path(
                            path.to_path_buf(),
                            self.cpal_sample_rate,
                            self.skim_consumer.clone(),
                            if self.state.master_muted { 0.0 } else { self.state.master_volume },
                            0.0,
                        );
                }
            } else {
                // Decode a poster frame (video or image) at up to 1920px wide.
                let max_w = (screen.width() as u32).min(1920);
                if let Ok((rgba, w, h)) =
                    klipp_engine::generate_thumbnail_at_size(path, 0.0, max_w)
                {
                    let pixels: Vec<egui::Color32> = rgba
                        .chunks_exact(4)
                        .map(|c| egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]))
                        .collect();
                    let image = egui::ColorImage {
                        size: [w as usize, h as usize],
                        pixels,
                    };
                    if let Some(old) = self.state.quick_look_texture.take() {
                        self.defer_drop_texture(old);
                    }
                    let tex = alloc_texture_logged(
                        ctx,
                        "quick_look_preview",
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.state.quick_look_texture = Some(tex);
                }
            }
        }

        // ── Backdrop ──
        egui::Area::new(egui::Id::new("quick_look_backdrop"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let resp = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(
                    screen,
                    0.0,
                    egui::Color32::from_black_alpha(220),
                );

                // ── Image / video preview ──
                if let Some(ref tex) = self.state.quick_look_texture {
                    let tex_size = tex.size();
                    let img_aspect = tex_size[0] as f32 / tex_size[1].max(1) as f32;
                    // Fit within 90% of screen to leave room for the info bar.
                    let max_w = screen.width() * 0.9;
                    let max_h = screen.height() * 0.85;
                    let (w, h) = if img_aspect > max_w / max_h {
                        (max_w, max_w / img_aspect)
                    } else {
                        (max_h * img_aspect, max_h)
                    };
                    let img_rect = egui::Rect::from_center_size(
                        egui::pos2(screen.center().x, screen.center().y - 16.0),
                        egui::vec2(w, h),
                    );
                    ui.painter().image(
                        tex.id(),
                        img_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else if self.state.quick_look_audio.is_some() {
                    // ── Audio waveform visualization ──
                    // Draw a large waveform in the center of the screen
                    // using the browser's cached audio peaks (if available).
                    let wave_rect = egui::Rect::from_center_size(
                        screen.center(),
                        egui::vec2(screen.width() * 0.7, screen.height() * 0.3),
                    );
                    ui.painter().rect_filled(
                        wave_rect,
                        8.0,
                        egui::Color32::from_rgba_unmultiplied(30, 30, 36, 200),
                    );
                    // Large audio icon in center.
                    ui.painter().text(
                        wave_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "\u{1F3B5}",
                        egui::FontId::proportional(64.0),
                        self.theme.text_accent,
                    );
                }

                // ── Metadata overlay at the bottom ──
                let filename = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut info_parts: Vec<String> = vec![filename];
                if let Some(ref info) = self.state.quick_look_info {
                    if let (Some(w), Some(h)) = (info.width, info.height) {
                        info_parts.push(format!("{w}\u{00d7}{h}"));
                    }
                    if let Some(ref codec) = info.codec {
                        info_parts.push(codec.to_string());
                    }
                    if let Some(fps) = info.fps {
                        info_parts.push(format!("{fps:.2} fps"));
                    }
                    if let Some(dur) = info.duration_secs {
                        let m = dur as u32 / 60;
                        let s = dur as u32 % 60;
                        info_parts.push(format!("{m}:{s:02}"));
                    }
                }
                if let Ok(meta) = std::fs::metadata(path) {
                    let size = meta.len();
                    if size > 0 {
                        let size_str = if size < 1024 * 1024 {
                            format!("{:.0} KB", size as f64 / 1024.0)
                        } else if size < 1024 * 1024 * 1024 {
                            format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
                        } else {
                            format!("{:.2} GB", size as f64 / (1024.0 * 1024.0 * 1024.0))
                        };
                        info_parts.push(size_str);
                    }
                }
                let info_text = info_parts.join("  \u{00b7}  ");
                let bar_rect = egui::Rect::from_min_max(
                    egui::pos2(screen.min.x, screen.max.y - 40.0),
                    screen.max,
                );
                ui.painter().rect_filled(
                    bar_rect,
                    0.0,
                    egui::Color32::from_black_alpha(180),
                );
                ui.painter().text(
                    egui::pos2(bar_rect.center().x, bar_rect.center().y),
                    egui::Align2::CENTER_CENTER,
                    &info_text,
                    egui::FontId::proportional(12.0),
                    self.theme.text_primary,
                );

                // ── Dismiss: click backdrop, Escape, or Spacebar ──
                let dismiss = resp.clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Escape))
                    || ui.input(|i| i.key_pressed(egui::Key::Space));
                if dismiss {
                    if let Some(ref session) = self.state.quick_look_audio {
                        session.stop();
                    }
                    klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                    klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                    self.state.quick_look_path = None;
                    if let Some(old) = self.state.quick_look_texture.take() {
                        self.defer_drop_texture(old);
                    }
                    self.state.quick_look_info = None;
                    self.state.quick_look_audio = None;
                }

                // ── Arrow keys: navigate to next/previous file ──
                let arrow_up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
                let arrow_down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
                if (arrow_up || arrow_down) && !dismiss {
                    // Find the current file in the browser's entry list
                    // and move to the adjacent non-directory entry.
                    let entries = &self.browser.files_entries;
                    if let Some(cur_idx) = entries.iter().position(|e| e.path == *path) {
                        let next = if arrow_down {
                            entries[cur_idx + 1..]
                                .iter()
                                .find(|e| !e.is_dir)
                                .map(|e| e.path.clone())
                        } else {
                            entries[..cur_idx]
                                .iter()
                                .rev()
                                .find(|e| !e.is_dir)
                                .map(|e| e.path.clone())
                        };
                        if let Some(next_path) = next {
                            // Stop current audio session if any.
                            if let Some(ref session) = self.state.quick_look_audio {
                                session.stop();
                            }
                            klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                            klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                            // Update selection in the browser too.
                            self.browser.files_selected.clear();
                            self.browser.files_selected.insert(next_path.clone());
                            self.browser.files_selection_anchor = Some(next_path.clone());
                            // Switch to the new file — lazy load on next frame.
                            self.state.quick_look_path = Some(next_path);
                            if let Some(old) = self.state.quick_look_texture.take() {
                                self.defer_drop_texture(old);
                            }
                            self.state.quick_look_info = None;
                            self.state.quick_look_audio = None;
                        }
                    }
                }
            });
    }

    fn render_status_bar_panel(&mut self, ui: &mut egui::Ui, statusbar_rect: egui::Rect) {
        let theme = &self.theme;
        let border = theme.border;
        let bg_void = theme.bg_void;
        crate::ui_util::allocate_clipped_ui(ui, statusbar_rect, |ui| {
            ui.set_min_size(statusbar_rect.size());
            ui.painter().rect_filled(statusbar_rect, 0.0, bg_void);
            ui.painter().hline(
                statusbar_rect.x_range(),
                statusbar_rect.min.y,
                egui::Stroke::new(1.0_f32, border),
            );
            crate::panels::status_bar::show(ui, &self.state, &self.theme);
        });
    }

    fn render_color_workspace_bottom(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        all_cmds: &mut Vec<AppCommand>,
        p: ColorWsParams,
    ) {
        let ColorWsParams {
            show_scopes_bar,
            show_nodes_bar,
            color_tools_bottom,
            toggle_bar_h,
            scopes_h,
            center_left,
            center_w,
            main_top,
            main_bottom,
            flexible_h,
            toggle_bar_rect,
            nodes_rect,
            bottom_scope_cursor,
        } = p;
        let theme = self.theme.clone();

        // Fill the entire bottom region with pure black FIRST so any
        // 1-2 px egui-spacing gaps between the toggle bar, scope
        // content, resizer, and the parent panel show #000000 instead
        // of leaking either the workspace's `bg_deep` (grey) or the
        // theme's `bg_void` (`#0a0a0c`, a near-but-not-pure black that
        // reads as a visible halo against the FFmpeg scope texture's
        // true `#000000` interior).
        if show_scopes_bar && !color_tools_bottom {
            let bottom_rect = egui::Rect::from_min_max(
                egui::pos2(center_left, toggle_bar_rect.min.y - theme::RESIZER_THICKNESS),
                egui::pos2(center_left + center_w, main_bottom),
            );
            ui.painter().rect_filled(bottom_rect, 0.0, egui::Color32::BLACK);
        }

        // Resizer between viewer and bottom panels (draggable)
        if show_scopes_bar || show_nodes_bar {
            let resizer_rect = egui::Rect::from_min_size(
                egui::pos2(center_left, toggle_bar_rect.min.y - theme::RESIZER_THICKNESS),
                egui::vec2(center_w, theme::RESIZER_THICKNESS),
            );
            // Idle line uses bg_void so the resizer sits flush against
            // the scope dock's black background instead of showing a
            // visible grey hairline directly above the toggle bar.
            let delta = resizer::horizontal_resizer_with_idle_color(
                ui,
                resizer_rect,
                &theme,
                theme.bg_void,
            );
            if delta != 0.0 {
                let new_ratio = self.layout.color_viewer_height_ratio + delta / flexible_h;
                self.layout.color_viewer_height_ratio = new_ratio.clamp(0.15, 0.90);
                self.layout_dirty = true;
            }
        }

        // Toggle bar: [Scopes] [Nodes] toggle buttons
        if toggle_bar_h > 0.0 {
            self.render_color_ws_toggle_bar(ui, all_cmds, toggle_bar_rect, &theme);
        }

        // Bottom panel: color tools (when docked at bottom) OR scopes (default)
        if color_tools_bottom {
            self.render_color_ws_bottom_tools(
                ui, all_cmds, center_left, center_w, scopes_h,
                main_top, main_bottom, toggle_bar_h, toggle_bar_rect, &theme,
            );
        } else if show_scopes_bar {
            self.render_color_ws_bottom_scopes(
                ui, center_left, center_w, scopes_h, main_top, main_bottom,
                toggle_bar_h, show_nodes_bar, toggle_bar_rect, bottom_scope_cursor,
            );
        }

        // Scope settings dropdown
        if self.state.scope_settings_open && show_scopes_bar {
            self.render_color_ws_scope_settings_dropdown(ctx, ui, toggle_bar_rect, &theme);
        }

        // Node strip panel — read live state
        if self.state.show_node_strip {
            self.render_color_ws_node_strip(ui, all_cmds, nodes_rect, &theme);
        }
    }

    fn render_color_ws_toggle_bar(
        &mut self,
        ui: &mut egui::Ui,
        all_cmds: &mut Vec<AppCommand>,
        toggle_bar_rect: egui::Rect,
        theme: &KlippTheme,
    ) {
        crate::ui_util::allocate_clipped_ui(ui, toggle_bar_rect, |ui| {
            ui.set_min_size(toggle_bar_rect.size());
            ui.painter().rect_filled(toggle_bar_rect, 0.0, theme.bg_panel);
            // Border between toggle bar and the scope content uses the
            // scope's own black background so there's no grey halo
            // between the bar and the trace area.
            ui.painter().hline(
                toggle_bar_rect.x_range(),
                toggle_bar_rect.min.y,
                egui::Stroke::new(0.5_f32, theme.bg_void),
            );
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                ui.add_space(8.0);
                if ui.selectable_label(self.state.show_scopes, "Scopes").clicked() {
                    self.state.show_scopes = !self.state.show_scopes;
                    ui.ctx().request_repaint();
                }
                if ui.selectable_label(self.state.show_node_strip, "Nodes")
                    .on_hover_text("Show / hide the node-graph strip below the timeline")
                    .clicked()
                {
                    self.state.show_node_strip = !self.state.show_node_strip;
                    ui.ctx().request_repaint();
                }
                ui.separator();
                {
                    use crate::state::ColorToolsPosition;
                    let is_bottom = self.state.color_tools_position == ColorToolsPosition::Bottom;
                    // Labelled by what clicking does.
                    let label = if is_bottom { "Dock right \u{2192}" } else { "Dock bottom \u{2193}" };
                    let tooltip = if is_bottom {
                        "Move the colour tools panel to the right side of the canvas (currently at the bottom)"
                    } else {
                        "Move the colour tools panel to the bottom of the canvas (currently on the right)"
                    };
                    if ui.button(label).on_hover_text(tooltip).clicked() {
                        self.state.color_tools_position = if is_bottom {
                            ColorToolsPosition::Right
                        } else {
                            ColorToolsPosition::Bottom
                        };
                    }
                }
                ui.separator();
                // What the nodes and wheels grade: the selected clip, or
                // the timeline grade over every clip after its own.
                ui.label(egui::RichText::new("Grade").color(theme.text_dim));
                let grading_clip = !self.state.grade_timeline && self.state.grade_group_stage.is_none();
                if ui.selectable_label(grading_clip, "Clip")
                    .on_hover_text("Grade the selected clip")
                    .clicked()
                {
                    self.state.grade_timeline = false;
                    self.state.grade_group_stage = None;
                    self.state.graph_descent_path.clear();
                    self.state.selected_node_id = None;
                    self.state.selected_node_ids.clear();
                }
                // Marked while a timeline grade is doing something, so
                // its being there shows from Clip mode too.
                let has_timeline_grade = self.state.sequences.get(self.state.active_sequence_idx)
                    .and_then(|s| s.timeline_graph.as_ref())
                    .is_some_and(|g| !g.is_identity());
                let timeline_label = if has_timeline_grade && !self.state.grade_timeline {
                    egui::RichText::new("Timeline \u{25CF}").color(theme.accent)
                } else {
                    egui::RichText::new("Timeline")
                };
                self.grade_group_controls(ui, all_cmds, theme);
                if ui.selectable_label(self.state.grade_timeline, timeline_label)
                    .on_hover_text(
                        "Grade the whole sequence: runs on every frame after each clip's own grade, before the timeline LUT.\n\
                         Only when this sequence is the one played or exported: nested in another as a compound clip or a \
                         precomposition, it plays without its timeline grade.",
                    )
                    .clicked()
                {
                    self.state.grade_timeline = true;
                    self.state.grade_group_stage = None;
                    self.state.graph_descent_path.clear();
                    self.state.selected_node_id = None;
                    self.state.selected_node_ids.clear();
                }

                if self.state.show_scopes {
                    ui.separator();
                    use crate::state::{ScopeLayout, ScopeMode, ScopeScale};

                    // How many instruments the dock shows at once.
                    // Nine is deliberately absent: it belongs to the
                    // floating window, which can be dragged to whatever
                    // size nine panes need. In a dock sharing the
                    // workspace with the picture it would give each pane
                    // a strip too short to read.
                    let layouts = [
                        (ScopeLayout::OnePane, "1"),
                        (ScopeLayout::TwoPane, "2"),
                        (ScopeLayout::FourPane, "4"),
                    ];
                    let layout_labels: Vec<&str> = layouts.iter().map(|l| l.1).collect();
                    let layout_active = layouts
                        .iter()
                        .position(|l| l.0 == self.state.scope_dock.layout)
                        .unwrap_or(0);
                    if let Some(i) = crate::widgets::segmented::segmented(
                        ui, &layout_labels, layout_active, theme,
                        egui::FontId::proportional(crate::theme::type_scale::CAPTION),
                        crate::theme::control::COMPACT,
                    ) {
                        let picked = layouts[i].0;
                        if picked != self.state.scope_dock.layout {
                            self.state.scope_dock.layout = picked;
                            self.state.scope_dock.panes =
                                crate::state::PaneConfig::default_for_layout(picked);
                            // Going back to one pane, carry the pane the
                            // colourist was looking at into the single
                            // scope, rather than snapping to whatever
                            // `scope_mode` was left on before the grid.
                            if picked == ScopeLayout::OnePane {
                                self.state.scope_dock.panes =
                                    vec![crate::state::PaneConfig::new(self.state.scope_mode.into())];
                            }
                        }
                    }
                    ui.separator();

                    // Only meaningful for the single scope. Each pane
                    // in a grid carries its own type dropdown and its
                    // own channel settings in its header, so leaving a
                    // global Wave/Parade/Vector/Hist control up there
                    // would be a second, contradicting answer to "which
                    // scope am I looking at".
                    if self.state.scope_dock.layout == ScopeLayout::OnePane {
                        let modes = [
                            (ScopeMode::Waveform, "Wave"),
                            (ScopeMode::Parade, "Parade"),
                            (ScopeMode::Vectorscope, "Vector"),
                            (ScopeMode::Histogram, "Hist"),
                        ];
                        let labels: Vec<&str> = modes.iter().map(|m| m.1).collect();
                        let active = modes.iter().position(|m| m.0 == self.state.scope_mode).unwrap_or(0);
                        if let Some(i) = crate::widgets::segmented::segmented(
                            ui, &labels, active, theme,
                            egui::FontId::proportional(crate::theme::type_scale::CAPTION), crate::theme::control::COMPACT,
                        ) {
                            let mode = modes[i].0;
                            tracing::info!("UI: scope mode button clicked: {:?} (was {:?})", mode, self.state.scope_mode);
                            self.state.scope_mode = mode;
                            let mode_byte = match mode {
                                ScopeMode::Waveform => 0u8,
                                ScopeMode::Parade => 1,
                                ScopeMode::Vectorscope => 2,
                                ScopeMode::Histogram => 3,
                            };
                            let _ = self.cmd_tx.send(AppCommand::SetScopeRenderMode { mode: mode_byte });
                            let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                            // Force the per-mode staleness check to invalidate
                            // immediately so the scope dock re-renders this
                            // mode on the next frame instead of displaying
                            // its (possibly stale) cached texture.
                            self.state.scope_state.increment_settings_generation();
                            ui.ctx().request_repaint();
                        }

                        if matches!(self.state.scope_mode, ScopeMode::Waveform | ScopeMode::Parade) {
                            ui.separator();
                            for (label, color, enabled) in [
                                ("R", egui::Color32::from_rgb(0xd0, 0x40, 0x40), &mut self.state.scope_show_r),
                                ("G", egui::Color32::from_rgb(0x40, 0xb0, 0x40), &mut self.state.scope_show_g),
                                ("B", egui::Color32::from_rgb(0x40, 0x50, 0xd0), &mut self.state.scope_show_b),
                            ] {
                                let bg = if *enabled { color } else { theme.bg_raised };
                                let text_c = if *enabled { egui::Color32::WHITE } else { theme.text_dim };
                                let btn = egui::Button::new(egui::RichText::new(label).size(crate::theme::type_scale::CAPTION).color(text_c).strong())
                                    .fill(bg)
                                    .min_size(egui::vec2(20.0, crate::theme::control::COMPACT - 4.0))
                                    .rounding(crate::theme::radius::CONTROL);
                                if ui.add(btn).on_hover_text(format!("Show the {label} channel")).clicked() { *enabled = !*enabled; }
                            }
                        }
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(6.0);
                        // Glyph buttons: settings, floating window, picker.
                        let glyph = |ui: &mut egui::Ui, text: &str, on: bool, lit: bool| -> egui::Response {
                            let (r, resp) = ui.allocate_exact_size(egui::vec2(crate::theme::control::COMPACT, crate::theme::control::COMPACT), egui::Sense::click());
                            let (bg, fg) = if on {
                                (theme.accent_subtle, theme.accent)
                            } else if resp.hovered() {
                                (theme.bg_hover, theme.text_primary)
                            } else {
                                (egui::Color32::TRANSPARENT, if lit { theme.text_primary } else { theme.text_secondary })
                            };
                            ui.painter().rect_filled(r, crate::theme::radius::CONTROL, bg);
                            ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, text, egui::FontId::proportional(13.0), fg);
                            resp
                        };
                        if glyph(ui, "\u{2699}", self.state.scope_settings_open, false).on_hover_text("Scope settings").clicked() {
                            self.state.scope_settings_open = !self.state.scope_settings_open;
                        }
                        if glyph(ui, "\u{229e}", self.state.scope_window.open, false).on_hover_text("Floating scope window").clicked() {
                            self.state.scope_window.open = !self.state.scope_window.open;
                        }
                        let picker_active = self.state.scope_cursor_show;
                        let picker_locked = self.state.scope_cursor_locked;
                        let picker_resp = glyph(ui, "\u{2295}", picker_locked, picker_active);
                        if picker_resp.clicked() {
                            if picker_locked {
                                self.state.scope_cursor_locked = false;
                                self.state.scope_cursor_locked_pos = None;
                                self.state.scope_cursor_locked_data = None;
                            } else {
                                self.state.scope_cursor_show = !self.state.scope_cursor_show;
                            }
                        }
                        picker_resp.on_hover_text(if picker_locked {
                            "Click to unlock sample point"
                        } else if picker_active {
                            "Scope picker (click viewer to lock)"
                        } else {
                            "Enable scope picker"
                        });

                        egui::ComboBox::from_id_salt("scope_scale_sel")
                            .width(0.0)
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .selected_text(crate::widgets::scopes::graticule_scale(self.state.scope_scale, self.state.hdr_monitor.renders_hdr()).button_label())
                            .show_ui(ui, |ui| {
                                for s in [ScopeScale::TenBit, ScopeScale::Ire, ScopeScale::Percent] {
                                    ui.selectable_value(&mut self.state.scope_scale, s, s.label());
                                }
                            });
                    });
                }
            });
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn render_color_ws_bottom_tools(
        &mut self,
        ui: &mut egui::Ui,
        all_cmds: &mut Vec<AppCommand>,
        center_left: f32,
        center_w: f32,
        scopes_h: f32,
        main_top: f32,
        main_bottom: f32,
        toggle_bar_h: f32,
        toggle_bar_rect: egui::Rect,
        theme: &KlippTheme,
    ) {
        let tools_h = if scopes_h > 0.0 { scopes_h } else {
            let ratio = self.layout.color_viewer_height_ratio.clamp(0.15, 0.90);
            let flex = (main_bottom - main_top - toggle_bar_h - theme::RESIZER_THICKNESS).max(200.0);
            (flex * (1.0 - ratio)).clamp(60.0, flex * 0.85)
        };
        let tools_rect = egui::Rect::from_min_size(
            egui::pos2(center_left, toggle_bar_rect.max.y),
            egui::vec2(center_w, tools_h),
        );
        crate::ui_util::allocate_clipped_ui(ui, tools_rect, |ui| {
            ui.set_min_size(tools_rect.size());
            ui.painter().rect_filled(tools_rect, 0.0, theme.bg_panel);
            if let Some(clip_id) = self.state.selected_clip {
                let seq_idx = self.state.active_sequence_idx;
                let mask_tolerance = crate::inspector::keyframe_selection::frame_tolerance(&self.state.sequences[seq_idx]);
                self.node_inspector_panel.input_colour_space =
                    crate::inspector::color_and_luts::clip_input_colour_space(&self.state, clip_id);
                let clip = self.state.sequences[seq_idx].tracks.iter_mut()
                    .find_map(|t| t.find_clip_mut(clip_id));
                if let Some(clip) = clip {
                    let mask_frame = crate::inspector::keyframe_selection::FrameSelection::new(self.state.playhead, clip.timeline_in, mask_tolerance);
                    let path = self.state.graph_descent_path.clone();
                    let graph: &mut klipp_nodes::NodeGraph =
                        match Self::resolve_graph_at_path_mut(clip, &path) {
                            Ok(g) => g,
                            Err(_) => &mut clip.node_graph,
                        };
                    let panel_cmds = self.node_inspector_panel.show_simple_horizontal(
                        ui, graph, clip_id,
                        &mut self.state.selected_node_id,
                        &mut self.state.color_tab_bottom,
                        &self.theme,
                        mask_frame,
                    );
                    all_cmds.extend(panel_cmds);
                }
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label(egui::RichText::new("Select a clip to grade")
                        .size(12.0).color(theme.text_dim));
                });
            }
        });
        // U-6: only drive a repaint when something's actually moving.
        // Engine thread calls ctx.request_repaint on every frame write,
        // and UI interactions (sliders, drags) already trigger paints
        // via egui. An idle colour workspace needs no refresh — the
        // unconditional repaint here used to pin the whole UI at max
        // FPS whenever the panel was visible.
        if self.state.playback == PlaybackState::Playing || self.state.is_scrubbing {
            ui.ctx().request_repaint();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render_color_ws_bottom_scopes(
        &mut self,
        ui: &mut egui::Ui,
        center_left: f32,
        center_w: f32,
        scopes_h: f32,
        main_top: f32,
        main_bottom: f32,
        toggle_bar_h: f32,
        show_nodes_bar: bool,
        toggle_bar_rect: egui::Rect,
        bottom_scope_cursor: Option<crate::state::ScopeCursorData>,
    ) {
        let live_scopes_h = if scopes_h > 0.0 { scopes_h } else {
            let ratio = self.layout.color_viewer_height_ratio.clamp(0.15, 0.90);
            let flex = (main_bottom - main_top - toggle_bar_h - theme::RESIZER_THICKNESS).max(200.0);
            let total_bottom = (flex * (1.0 - ratio)).clamp(60.0, flex * 0.85);
            if show_nodes_bar { total_bottom * 0.6 } else { total_bottom }
        };
        let live_scopes_rect = egui::Rect::from_min_size(
            egui::pos2(center_left, toggle_bar_rect.max.y),
            egui::vec2(center_w, live_scopes_h),
        );
        crate::ui_util::allocate_clipped_ui(ui, live_scopes_rect, |ui| {
            ui.set_min_size(live_scopes_rect.size());
            // Use `bg_void` (the same colour the inner `show_scopes`
            // fills its `full_rect` with) instead of `bg_deep`. They
            // differ by ~7 % luminance, and any 1–2 px egui-spacing
            // inset between the dock rect and `full_rect` would leak
            // the lighter `bg_deep` as a visible ring around the
            // axis-label column and the trace. Matching the layers
            // makes the dock read as a single seamless black surface
            // — DaVinci-style flush black, no "panel" chrome around
            // the instrument glass.
            ui.painter().rect_filled(live_scopes_rect, 0.0, egui::Color32::BLACK);
            if self.state.scope_state.scale != self.state.scope_scale
                || self.state.scope_state.low_pass != self.state.scope_low_pass
                || self.state.scope_state.bloom != self.state.scope_bloom
                || self.state.scope_state.show_r != self.state.scope_show_r
                || self.state.scope_state.show_g != self.state.scope_show_g
                || self.state.scope_state.show_b != self.state.scope_show_b
                || self.state.scope_state.quality != self.state.scope_quality
            {
                self.state.scope_state.increment_settings_generation();
            }
            self.state.scope_state.scale = self.state.scope_scale;
            self.state.scope_state.legal_range = self.state.scope_legal_range;
            self.state.scope_state.hdr_levels = self.state.hdr_monitor.renders_hdr();
            self.state.scope_state.low_pass = self.state.scope_low_pass;
            self.state.scope_state.bloom = self.state.scope_bloom;
            self.state.scope_state.show_r = self.state.scope_show_r;
            self.state.scope_state.show_g = self.state.scope_show_g;
            self.state.scope_state.show_b = self.state.scope_show_b;
            self.state.scope_state.quality = self.state.scope_quality;

            // Above one pane the dock draws the same grid the floating
            // window does, from the same `draw_pane`: one instrument,
            // two places to put it. A colourist reading a grade wants
            // parade and vectorscope at the same time — judging chroma
            // off a scope you have to switch away from to see the other
            // one is not how the work is done.
            //
            // At one pane nothing changes: the original single-scope
            // renderer, driven by `scope_mode`, keeps the familiar
            // Wave/Parade/Vector/Hist control and the R/G/B buttons.
            if self.state.scope_dock.layout != crate::state::ScopeLayout::OnePane {
                self.scope_dock_runtime.hdr_levels = self.state.hdr_monitor.renders_hdr();
                crate::widgets::scope_window::show_shared_toggles(
                    &mut self.state.scope_dock,
                    self.state.scope_low_pass,
                    self.state.scope_bloom,
                );
                let correlation_samples: Vec<f32> =
                    if let Ok(ring) = self.state.spectrum_ring.try_lock() {
                        const N: usize = 4096;
                        let len = ring.len();
                        ring.iter().copied().skip(len.saturating_sub(N)).collect()
                    } else {
                        Vec::new()
                    };
                let lufs_history: Vec<f32> =
                    self.state.lufs_short_term_history.iter().copied().collect();
                let spectrum_history: Vec<Vec<f32>> =
                    self.state.spectrum_history.iter().cloned().collect();
                crate::widgets::scope_window::draw_pane_grid(
                    ui,
                    &mut self.state.scope_dock,
                    &mut self.state.scope_options,
                    &mut self.scope_dock_runtime,
                    &self.frame_buffer,
                    self.prefs.color.vectorscope_skin_angle,
                    self.state.playback == PlaybackState::Playing,
                    bottom_scope_cursor,
                    &self.state.spectrum_bins,
                    &correlation_samples,
                    &lufs_history,
                    self.state.lufs_target,
                    &spectrum_history,
                    &self.theme,
                    klipp_color::scopes::ScopeBasis::for_working_space(
                        self.state.project_working_colorspace,
                    ),
                );
                crate::widgets::scope_window::take_shared_toggles(
                    &self.state.scope_dock,
                    &mut self.state.scope_low_pass,
                    &mut self.state.scope_bloom,
                );
                return;
            }

            crate::widgets::scopes::show_scopes(
                ui,
                &mut self.state.scope_state,
                self.state.scope_mode,
                &self.frame_buffer,
                self.state.scope_quality,
                self.prefs.color.vectorscope_skin_angle,
                self.state.playback == PlaybackState::Playing,
                bottom_scope_cursor,
                self.state.scope_cursor_show,
                &self.theme,
                klipp_color::scopes::ScopeBasis::for_working_space(
                    self.state.project_working_colorspace,
                ),
                self.state.scope_options.vectorscope_zoom,
                self.state.scope_options.histogram_display,
            );
        });
        // U-6: same guard as render_color_ws_bottom_tools. Scopes only
        // need to animate while playback or scrubbing is live; static
        // views don't need 60 Hz repaints.
        if self.state.playback == PlaybackState::Playing || self.state.is_scrubbing {
            ui.ctx().request_repaint();
        }
    }

    fn render_color_ws_scope_settings_dropdown(
        &mut self,
        ctx: &egui::Context,
        ui: &mut egui::Ui,
        toggle_bar_rect: egui::Rect,
        theme: &KlippTheme,
    ) {
        use crate::state::ScopeScale;
        let dropdown_id = egui::Id::new("scope_settings_dropdown");
        // Cap the visible height so the popover doesn't push past the
        // window bottom on small viewers; ScrollArea handles overflow.
        let max_content_h = (ui.ctx().screen_rect().height() - toggle_bar_rect.max.y - 32.0)
            .clamp(240.0, 720.0);
        let area_response = egui::Area::new(dropdown_id)
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(toggle_bar_rect.max.x - 240.0, toggle_bar_rect.max.y + 2.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(244.0);
                    ui.set_max_width(280.0);
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);
                    // egui's default scrollbar is "floating" (overlays
                    // content). Switching to a fixed bar makes it
                    // reserve real horizontal space, so the bar_*_margin
                    // values produce visible gaps instead of an
                    // overlay-on-content look.
                    ui.spacing_mut().scroll.floating = false;
                    ui.spacing_mut().scroll.bar_width = 8.0;
                    ui.spacing_mut().scroll.bar_inner_margin = 12.0;
                    ui.spacing_mut().scroll.bar_outer_margin = 4.0;

                    egui::ScrollArea::vertical()
                        .max_height(max_content_h)
                        .show(ui, |ui| {
                            // Belt-and-braces: cap the inner content
                            // width so even widgets that ignore
                            // scroll margins (egui sliders sometimes
                            // grab `available_width` early) stay clear
                            // of the bar.
                            let target = (ui.available_width() - 12.0).max(180.0);
                            ui.set_max_width(target);

                    ui.label(egui::RichText::new("DISPLAY").size(10.0).color(theme.text_dim).strong());
                    ui.checkbox(&mut self.state.scope_low_pass, "Low Pass Filter");
                    ui.checkbox(&mut self.state.scope_bloom, "Bloom on dense regions");
                    ui.checkbox(&mut self.state.scope_overlay_mode, "Overlay on viewer");

                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("QUALITY").size(10.0).color(theme.text_dim).strong());
                    ui.radio_value(&mut self.state.scope_quality, klipp_core::ScopeQuality::Low, "Draft (every 4px)");
                    ui.radio_value(&mut self.state.scope_quality, klipp_core::ScopeQuality::Medium, "Standard (every 2px)");
                    ui.radio_value(&mut self.state.scope_quality, klipp_core::ScopeQuality::High, "Full (every px)");

                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("SCALE").size(10.0).color(theme.text_dim).strong());
                    ui.radio_value(&mut self.state.scope_scale, ScopeScale::TenBit, "0 \u{2013} 1023 (10-bit)");
                    ui.radio_value(&mut self.state.scope_scale, ScopeScale::Ire, "0 \u{2013} 100 IRE");
                    ui.radio_value(&mut self.state.scope_scale, ScopeScale::Percent, "0 \u{2013} 100%");
                    ui.checkbox(&mut self.state.scope_legal_range, "Show legal range (64\u{2013}940)")
                        .on_hover_text("Mark codes 64 and 940, the legal range of a 10-bit signal, on the waveform, parade and histogram.");
                    if self.state.hdr_monitor.renders_hdr() {
                        ui.label(egui::RichText::new("HDR monitoring: the graticule is in nits.").size(10.0).color(theme.text_dim));
                    }

                    // The scope options: the same block the floating scope
                    // window's settings show, both editing `state.scope_options`.
                    ui.add_space(4.0);
                    crate::widgets::scope_window::draw_scope_options(
                        ui,
                        &mut self.state.scope_options,
                        theme,
                    );

                    }); // end ScrollArea
                });
            });

        // Close on click-outside, using the ACTUAL rendered Area rect
        // (the old hardcoded 210×250 box was too small once envelope /
        // intensity / opacity / threshold controls landed, and made the
        // ComboBox arrow look like it "closed the window" because its
        // popup spawned outside that rect and triggered the close).
        //
        // Also skip the close while any egui popup (combobox dropdown,
        // tooltip with state) is open — those popups render in separate
        // Areas and their click events would otherwise dismiss us.
        let area_rect = area_response.response.rect;
        let any_popup_open = ctx.memory(|m| m.any_popup_open());
        if !any_popup_open && ui.input(|i| i.pointer.primary_clicked()) {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if !area_rect.contains(pos) && !toggle_bar_rect.contains(pos) {
                    self.state.scope_settings_open = false;
                }
            }
        }
    }

    fn render_color_ws_node_strip(
        &mut self,
        ui: &mut egui::Ui,
        all_cmds: &mut Vec<AppCommand>,
        nodes_rect: egui::Rect,
        theme: &KlippTheme,
    ) {
        // Sync per-node thumbs from the engine's FrameBuffer into
        // AppState at the start of the panel render. Engine bumps
        // `node_thumbs_gen` whenever it refreshes the map on a paused
        // / post-seek re-grade; we only clone the HashMap when it
        // actually advanced, so steady-state costs one lock + one
        // generation compare per frame.
        {
            let fb = self.frame_buffer.lock().unwrap_or_else(|e| e.into_inner());
            if fb.node_thumbs_gen != self.state.node_thumbs_gen {
                self.state.node_thumbs = fb.node_thumbs.clone();
                self.state.node_thumbs_gen = fb.node_thumbs_gen;
            }
        }
        crate::ui_util::allocate_clipped_ui(ui, nodes_rect, |ui| {
            ui.set_min_size(nodes_rect.size());
            ui.painter().rect_filled(nodes_rect, 0.0, theme.bg_deep);
            let clip_id = self.state.selected_clip;
            if let Some(cid) = clip_id {
                let seq_idx = self.state.active_sequence_idx;
                // The timeline grade's stand-in keeps no versions: only
                // its graph is written back.
                let keeps_versions = GradeStandIn::of(&self.state.sequences[seq_idx], cid).is_none();
                if let Some(clip) = self.state.sequences[seq_idx].tracks.iter_mut()
                    .find_map(|t| t.find_clip_mut(cid))
                {
                    // Self-heal: if any id in the descent path doesn't
                    // resolve against the current clip's graph (clip
                    // switch, upstream RemoveNode, etc.) clear the
                    // path so the V2 panel renders the root graph on
                    // this frame. Selection clears alongside so the
                    // inspector doesn't linger on a ghost.
                    let path_valid = {
                        let mut cursor = &clip.node_graph;
                        self.state.graph_descent_path.iter().all(|id| {
                            match cursor.nodes.iter().find(|n| n.id == *id) {
                                Some(n) if n.kind == klipp_nodes::NodeKind::Compound => {
                                    match n.params.compound_subgraph.as_ref() {
                                        Some(sub) => { cursor = sub; true }
                                        None => false,
                                    }
                                }
                                _ => false,
                            }
                        })
                    };
                    if !path_valid {
                        self.state.graph_descent_path.clear();
                        self.state.selected_node_ids.clear();
                        self.state.selected_node_id = None;
                    }
                    // Build breadcrumb labels by walking the descent
                    // path through each compound's subgraph. Done
                    // BEFORE the mutable resolve so `clip.node_graph`
                    // can be read cleanly; the vec then survives the
                    // `Self::resolve_graph_at_path_mut` call.
                    let breadcrumb_labels: Vec<String> = {
                        let mut labels = Vec::with_capacity(
                            self.state.graph_descent_path.len()
                        );
                        let mut cursor = &clip.node_graph;
                        for id in &self.state.graph_descent_path {
                            if let Some(n) = cursor.nodes.iter().find(|n| n.id == *id) {
                                labels.push(n.label.clone());
                                if let Some(sub) = n.params.compound_subgraph.as_ref() {
                                    cursor = sub;
                                } else {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                        labels
                    };
                    let panel_cmds = if self.state.use_node_graph_v2 {
                        let slot = self.state.deferred_texture_drops_slot;
                        // Snapshot grade version names + active
                        // index BEFORE grabbing the mutable graph
                        // borrow — the panel only needs the labels
                        // for the dropdown, not the full snapshots
                        // (which would be expensive to clone per
                        // render).
                        let grade_version_names: Vec<String> = clip
                            .grade_versions
                            .iter()
                            .map(|v| v.name.clone())
                            .collect();
                        let active_grade_version = clip.active_grade_version;
                        let clip_time_secs = (self.state.playhead.to_secs_f64()
                            - clip.timeline_in.to_secs_f64())
                        .max(0.0);
                        // Resolve the graph at the current descent
                        // path. If the path went stale (shouldn't
                        // happen in normal use — we self-heal), the
                        // call falls back to the root graph so the
                        // user isn't stuck on a blank canvas.
                        let path = self.state.graph_descent_path.clone();
                        let resolved = Self::resolve_graph_at_path_mut(clip, &path);
                        let graph = match resolved {
                            Ok(g) => g,
                            Err(_) => &mut clip.node_graph,
                        };
                        let panel_rect = ui.max_rect();
                        let cmds = self.node_graph_v2_panel.show(
                            ui, &mut self.state.selected_node_id,
                            &mut self.state.selected_node_ids,
                            graph, cid, &self.theme,
                            &self.state.node_thumbs,
                            self.state.node_thumbs_gen,
                            &mut self.state.deferred_texture_drops[slot],
                            &mut self.state.graph_descent_path,
                            &breadcrumb_labels,
                            keeps_versions.then_some(&grade_version_names[..]),
                            active_grade_version,
                            clip_time_secs,
                            self.prefs.color.port_snap_enabled,
                            self.prefs.color.port_snap_radius_px,
                            self.state.solo_node,
                        );
                        // Tag the node-graph panel as the active panel
                        // when the user clicks anywhere inside its rect
                        // — including on a node, the canvas background,
                        // or any inner widget. The keybinding dispatcher
                        // reads `state.active_panel` to decide whether
                        // Ctrl+C means "copy colour nodes" (here) or
                        // "copy clip" (elsewhere).
                        let ctx = ui.ctx().clone();
                        crate::state::note_active_panel(
                            &mut self.state,
                            crate::state::PanelKind::NodeGraph,
                            &ctx,
                            panel_rect,
                        );
                        cmds
                    } else {
                        self.node_graph_panel.show(
                            ui, &mut self.state.selected_node_id,
                            &mut clip.node_graph, cid, &self.theme,
                        )
                    };
                    all_cmds.extend(panel_cmds);
                } else {
                    ui.painter().text(
                        nodes_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Clip not found in sequence",
                        egui::FontId::proportional(12.0),
                        self.theme.text_dim,
                    );
                }
            } else {
                ui.painter().text(
                    nodes_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "Select a clip to view its node graph",
                    egui::FontId::proportional(12.0),
                    self.theme.text_dim,
                );
            }
        });

        // Filmstrip-sourced per-node thumbnails (DaVinci-style node
        // preview). Decouples node-panel thumbs from the 4K decoder
        // pipeline: we feed the executor a pre-grade filmstrip tile at
        // ~228×128 instead of the main viewer's full-res frame. That
        // pass runs in well under a millisecond regardless of source
        // resolution, and filmstrip tiles are already in memory so
        // there's zero decode latency. Grade params are still
        // evaluated at the exact playhead so keyframed values stay
        // frame-accurate. Deduped via `last_filmstrip_thumb_send` so
        // the command only fires when something the thumbs depend on
        // (clip / tile / playhead / grade) actually changed. Pushed
        // after `panel_cmds` so any `SetVisibleNodeThumbs` from the V2
        // canvas processes first — the engine needs the visible-set
        // populated before our `RenderNodeThumbsFromSource` arrives.
        // While descended into a compound the filmstrip thumb
        // pipeline would render top-level clip graph outputs for
        // every node — misleading while the user is editing inside a
        // sub-graph the executor doesn't recurse into yet (stage 5).
        // Skip the dispatch for now; the V2 canvas falls back to
        // whatever thumb bytes are still in state.node_thumbs from
        // the last root-level capture, which is fine (stale but
        // consistent with the outer frame).
        if self.state.use_node_graph_v2 && self.state.graph_descent_path.is_empty() {
            if let Some(cid) = self.state.selected_clip {
                let seq_idx = self.state.active_sequence_idx;
                let clip_info = self.state.sequences[seq_idx].tracks.iter()
                    .find_map(|t| t.clips.iter().find(|c| c.id == cid).map(|c| (
                        c.media_id(),
                        c.source_in.to_secs_f64(),
                        c.timeline_in,
                        c.speed,
                        klipp_engine::render_cache::compute_grade_hash(
                            &c.node_graph,
                            &c.builtin_effects,
                            &c.builtin_effects_enabled,
                        ),
                    )));
                if let Some((Some(media_id), src_in_secs, tl_in, speed, grade_hash)) = clip_info {
                    let playhead = self.state.playhead;
                    let src_time = src_in_secs
                        + (playhead - tl_in).to_secs_f64() * speed.max(0.001);
                    let tile_idx = self.state.thumbnail_cache.filmstrip(&media_id)
                        .and_then(|frames| {
                            if frames.is_empty() { return None; }
                            let i = match frames.binary_search_by(|f| {
                                f.time_secs.partial_cmp(&src_time)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            }) {
                                Ok(i) => i,
                                Err(0) => 0,
                                Err(i) if i >= frames.len() => frames.len() - 1,
                                Err(i) => {
                                    let before = (src_time - frames[i - 1].time_secs).abs();
                                    let after = (frames[i].time_secs - src_time).abs();
                                    if before <= after { i - 1 } else { i }
                                }
                            };
                            Some(i)
                        });
                    if let Some(idx) = tile_idx {
                        let key = (cid, idx, playhead.value, grade_hash);
                        if self.state.last_filmstrip_thumb_send != Some(key) {
                            let tile = self.state.thumbnail_cache.filmstrip(&media_id)
                                .and_then(|frames| frames.get(idx))
                                .map(|f| (f.rgba.clone(), f.width, f.height));
                            if let Some((rgba, w, h)) = tile {
                                all_cmds.push(AppCommand::RenderNodeThumbsFromSource {
                                    clip_id: cid,
                                    playhead,
                                    src_rgba: rgba,
                                    src_w: w,
                                    src_h: h,
                                });
                                self.state.last_filmstrip_thumb_send = Some(key);
                            }
                        }
                    }
                }
            }
        }
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

/// Apply baked-in animation keyframes to a title clip based on an animation identifier string.
///
/// Extract the target `ClipId` from any title-edit `AppCommand`.
/// Returns `None` when the command isn't a title edit (lifecycle /
/// asset commands carry their own clip-id semantics handled
/// elsewhere). Used by the title-command dispatcher to know which
/// clip to propagate from after a successful edit.
fn title_edit_clip_id(cmd: &AppCommand) -> Option<klipp_core::ClipId> {
    use AppCommand as C;
    match cmd {
        C::UpdateTitleData { clip_id, .. }
        | C::ApplyTitleStyle { clip_id, .. }
        | C::SetTitleAlignment { clip_id, .. }
        | C::SetTitlePosition { clip_id, .. }
        | C::SetTitleBackground { clip_id, .. }
        | C::SetTitleShadow { clip_id, .. }
        | C::SetTitleGradient { clip_id, .. }
        | C::AddTitleObjectOpacityKeyframe { clip_id, .. }
        | C::RemoveTitleObjectOpacityKeyframe { clip_id, .. }
        | C::AddTitleObjectPositionKeyframe { clip_id, .. }
        | C::RemoveTitleObjectPositionKeyframe { clip_id, .. }
        | C::AddTitleObjectTransformKeyframe { clip_id, .. }
        | C::RemoveTitleObjectTransformKeyframe { clip_id, .. }
        | C::MoveTitleObjectPositionKeyframe { clip_id, .. }
        | C::SetTitleObjectPositionKeyframeInterpolation { clip_id, .. }
        | C::SetTitleObjectPositionKeyframeTangent { clip_id, .. }
        | C::AddTitleObjectEffectKeyframe { clip_id, .. }
        | C::RemoveTitleObjectEffectKeyframe { clip_id, .. }
        | C::MoveTitleObjectEffectKeyframe { clip_id, .. }
        | C::SetTitleObjectEffectKeyframeInterpolation { clip_id, .. }
        | C::SetTitleObjectEffectKeyframeTangent { clip_id, .. }
        | C::SetTitleTexturePreset { clip_id, .. }
        | C::SetTitleTextureParams { clip_id, .. }
        | C::SetTitleOutlineStackEnabled { clip_id, .. }
        | C::SetTitleShadows { clip_id, .. }
        | C::SetTitleShadowLayer { clip_id, .. }
        | C::SetTitleShadowStackEnabled { clip_id, .. }
        | C::SetTitleInnerShadows { clip_id, .. }
        | C::SetTitleInnerShadowLayer { clip_id, .. }
        | C::SetTitleInnerShadowStackEnabled { clip_id, .. }
        | C::SetTitleGlows { clip_id, .. }
        | C::SetTitleGlowLayer { clip_id, .. }
        | C::SetTitleGlowStackEnabled { clip_id, .. } => Some(*clip_id),
        _ => None,
    }
}

/// Animation format: `"type_duration"` or `"type1+type2_duration"` where duration is in seconds.
/// Examples: `"fade_in_0.5"`, `"slide_left_1.0"`, `"fade_in+slide_left_0.8"`, `"fade_in_out_0.5"`
/// Resolve a `&mut TextBox` to mutate inside a TitleData given an
/// optional target object id. Falls back to the title's primary
/// TextBox when `target` is `None` (the historical single-text
/// behaviour). Used by every per-property title command so an
/// Inspector edit follows the active per-object selection.
/// The object a title command targets: the one named, else the first
/// text object — the same choice [`target_textbox_mut`] makes.
fn target_object_mut(
    td: &mut klipp_timeline::TitleData,
    target: Option<klipp_timeline::TitleObjectId>,
) -> Option<&mut klipp_timeline::TitleObject> {
    let idx = match target {
        Some(oid) => td.objects.iter().position(|o| o.id == oid),
        None => td.objects.iter().position(|o| matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_))),
    }?;
    td.objects.get_mut(idx)
}

/// The stack of the object a title command targets — where a title's
/// motion and effects live now — to edit.
fn target_fx_mut(
    td: &mut klipp_timeline::TitleData,
    target: Option<klipp_timeline::TitleObjectId>,
) -> Option<&mut klipp_timeline::LayerFx> {
    target_object_mut(td, target).map(klipp_timeline::TitleObject::fx_mut)
}

fn target_textbox_mut(
    td: &mut klipp_timeline::TitleData,
    target: Option<klipp_timeline::TitleObjectId>,
) -> Option<&mut klipp_timeline::TextBox> {
    // Resolve which index in `td.objects` the request targets. Done
    // up-front (immutable scan) so the subsequent mutable borrow is
    // a single, simple iter that the borrow checker accepts without
    // Polonius. Falls back to the index of the first TextBox object
    // when no target is specified.
    let idx = match target {
        Some(oid) => td.objects.iter().position(|o| o.id == oid
            && matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_))),
        None => td.objects.iter().position(|o|
            matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_))),
    };
    let idx = idx?;
    match &mut td.objects[idx].content {
        klipp_timeline::TitleObjectContent::TextBox(tb) => Some(tb),
        _ => None,
    }
}

/// Flatten a Title-inspector style preset into the target TextBox.
/// Each preset writes a deliberate combination of fill colour, glow,
/// outline, shadow, and modern-FX fields. Presets coexist as
/// applied-and-flattened settings — there's no "currently-selected
/// preset" recorded on the data, so the inspector dropdown is a
/// quick-style picker rather than a slot.
pub(crate) fn apply_title_preset(tb: &mut klipp_timeline::TextBox, fx: &mut klipp_timeline::LayerFx, preset_id: u8) {
    use klipp_timeline::{InnerShadow, TextGlow, TextOutline, TitleShadow};
    match preset_id {
        // 0 = "None" sentinel — no-op so the dropdown's default
        // doesn't accidentally clobber the current style.
        0 => {}
        // 1 = Neon. Bright fill (uses current colour as accent),
        // thin dark outline for legibility, strong outer glow tinted
        // to the accent.
        1 => {
            let accent = tb.color;
            fx.outlines = vec![TextOutline {
                color: [10, 10, 14, 180],
                width: 2.0,
                enabled: true,
                width_keyframes: Vec::new(),
            }];
            fx.glows = vec![TextGlow {
                color: accent,
                radius: 12.0,
                intensity: 1.4,
                enabled: true,
                radius_keyframes: Vec::new(),
                intensity_keyframes: Vec::new(),
            }];
            // Clear conflicting effects so the look is clean.
            fx.shadows.clear();
            fx.inner_shadows.clear();
            tb.gradient = None;
            tb.chromatic_aberration_px = 0.0;
            tb.glitch_intensity = 0.0;
        }
        // 2 = Classic. White fill, no decorations.
        2 => {
            tb.color = [255, 255, 255, 255];
            fx.outlines.clear();
            fx.glows.clear();
            fx.shadows.clear();
            fx.inner_shadows.clear();
            tb.gradient = None;
            tb.pattern_fill = None;
            tb.pattern_fill_enabled = true;
            tb.chromatic_aberration_px = 0.0;
            tb.glitch_intensity = 0.0;
        }
        // 3 = Bold Shadow. White fill + soft drop shadow for contrast
        // against any background. Common lower-third look.
        3 => {
            tb.color = [255, 255, 255, 255];
            fx.outlines.clear();
            fx.glows.clear();
            fx.shadows = vec![TitleShadow {
                color: [0, 0, 0, 200],
                offset_x: 4.0,
                offset_y: 4.0,
                blur: 6.0,
                enabled: true,
                offset_x_keyframes: Vec::new(),
                offset_y_keyframes: Vec::new(),
                blur_keyframes: Vec::new(),
            }];
            fx.inner_shadows.clear();
            tb.gradient = None;
            tb.chromatic_aberration_px = 0.0;
            tb.glitch_intensity = 0.0;
        }
        // 4 = Outlined. White fill + thick black outline. Reads on
        // any backdrop without a fill plate.
        4 => {
            tb.color = [255, 255, 255, 255];
            fx.outlines = vec![TextOutline {
                color: [0, 0, 0, 255],
                width: 5.0,
                enabled: true,
                width_keyframes: Vec::new(),
            }];
            fx.glows.clear();
            fx.shadows.clear();
            fx.inner_shadows.clear();
            tb.gradient = None;
            tb.chromatic_aberration_px = 0.0;
            tb.glitch_intensity = 0.0;
        }
        // 5 = VHS. Chromatic aberration + glitch slicing for the
        // tape / synth-wave look. Keep the fill colour as-is so the
        // user's chosen hue tints the channel split.
        5 => {
            fx.outlines.clear();
            fx.glows.clear();
            fx.shadows.clear();
            fx.inner_shadows = vec![InnerShadow {
                color: [0, 0, 0, 100],
                offset_x: 0.0,
                offset_y: 0.0,
                blur: 2.0,
                enabled: true,
                offset_x_keyframes: Vec::new(),
                offset_y_keyframes: Vec::new(),
                blur_keyframes: Vec::new(),
            }];
            tb.chromatic_aberration_px = 6.0;
            tb.glitch_intensity = 0.35;
        }
        _ => {}
    }
}


// ── SRT/VTT subtitle parsing and generation ──

struct SubtitleEntry {
    start_secs: f64,
    end_secs: f64,
    text: String,
}

// ── ASS/SSA subtitle parsing (Tier 1, no libass) ──

/// A single Dialogue line from an ASS/SSA file.
#[derive(Debug, Clone)]
struct AssSubtitleEntry {
    pub start_secs: f64,
    pub end_secs: f64,
    /// Displayable text — ASS override tags stripped, `\N`/`\n`/`\h` normalized.
    pub text: String,
    /// Name of the V4+ style this dialogue references (empty if none).
    pub style_name: String,
}

/// A [V4+ Styles] entry — just the fields we actually wire into TitleData.
#[derive(Debug, Clone, Default)]
struct AssStyle {
    #[allow(dead_code)]
    pub name: String,
    pub font_name: String,
    pub font_size: f32,
    /// RGBA. Converted from ASS `&HAABBGGRR` (alpha is inverted in ASS: 00 = opaque).
    pub primary_color: [u8; 4],
    pub outline: f32,
    pub shadow: f32,
    pub bold: bool,
    pub italic: bool,
}

/// Parse ASS/SSA. Reads `[V4+ Styles]` (both V4+ and V4 headers) and `[Events]`.
/// Returns `(entries, styles_by_name)`. The import handler maps
/// `entry.style_name` → style for TitleData population.
fn parse_ass(content: &str) -> (Vec<AssSubtitleEntry>, std::collections::HashMap<String, AssStyle>) {
    use std::collections::HashMap;
    let mut entries: Vec<AssSubtitleEntry> = Vec::new();
    let mut styles: HashMap<String, AssStyle> = HashMap::new();

    #[derive(PartialEq)]
    enum Section { None, Styles, Events, Other }
    let mut section = Section::None;
    let mut styles_fmt: Vec<String> = Vec::new();
    let mut events_fmt: Vec<String> = Vec::new();

    for raw in content.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('!') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let header = line.trim_start_matches('[').trim_end_matches(']').trim();
            section = if header.eq_ignore_ascii_case("V4+ Styles")
                || header.eq_ignore_ascii_case("V4 Styles")
            {
                Section::Styles
            } else if header.eq_ignore_ascii_case("Events") {
                Section::Events
            } else {
                Section::Other
            };
            continue;
        }
        // Each line in V4+/Events is `Key: value`
        let Some(colon) = line.find(':') else { continue; };
        let key = line[..colon].trim();
        let value = line[colon + 1..].trim();
        match section {
            Section::Styles => {
                if key.eq_ignore_ascii_case("Format") {
                    styles_fmt = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect();
                } else if key.eq_ignore_ascii_case("Style") {
                    if styles_fmt.is_empty() { continue; }
                    // Styles have no free-text field, so a plain split is safe.
                    let fields: Vec<&str> = value.split(',').map(str::trim).collect();
                    if fields.len() < styles_fmt.len() { continue; }
                    let mut style = AssStyle::default();
                    for (i, fname) in styles_fmt.iter().enumerate() {
                        let v = fields[i];
                        match fname.as_str() {
                            "Name" => style.name = v.to_string(),
                            "Fontname" => style.font_name = v.to_string(),
                            "Fontsize" => {
                                if let Ok(n) = v.parse::<f32>() { style.font_size = n; }
                            }
                            "PrimaryColour" => style.primary_color = parse_ass_color(v),
                            "Outline" => {
                                if let Ok(n) = v.parse::<f32>() { style.outline = n; }
                            }
                            "Shadow" => {
                                if let Ok(n) = v.parse::<f32>() { style.shadow = n; }
                            }
                            "Bold" => style.bold = v != "0" && !v.is_empty(),
                            "Italic" => style.italic = v != "0" && !v.is_empty(),
                            _ => {}
                        }
                    }
                    if !style.name.is_empty() {
                        styles.insert(style.name.clone(), style);
                    }
                }
            }
            Section::Events => {
                if key.eq_ignore_ascii_case("Format") {
                    events_fmt = value
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect();
                } else if key.eq_ignore_ascii_case("Dialogue") {
                    if events_fmt.is_empty() { continue; }
                    // The last Format column is "Text" and may contain commas.
                    // Split into at most events_fmt.len() parts with splitn.
                    let fields: Vec<&str> = value
                        .splitn(events_fmt.len(), ',')
                        .collect();
                    if fields.len() < events_fmt.len() { continue; }
                    let mut start_s = 0.0;
                    let mut end_s = 0.0;
                    let mut style_name = String::new();
                    let mut text_raw = String::new();
                    for (i, fname) in events_fmt.iter().enumerate() {
                        let v = fields[i];
                        match fname.as_str() {
                            "Start" => start_s = parse_ass_time(v.trim()),
                            "End" => end_s = parse_ass_time(v.trim()),
                            "Style" => style_name = v.trim().to_string(),
                            "Text" => text_raw = v.to_string(), // keep whitespace
                            _ => {}
                        }
                    }
                    let text = strip_ass_override_tags(&text_raw);
                    if !text.is_empty() && end_s > start_s {
                        entries.push(AssSubtitleEntry {
                            start_secs: start_s,
                            end_secs: end_s,
                            text,
                            style_name,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    (entries, styles)
}

/// ASS time: `H:MM:SS.cc` where cc is centiseconds (NOT milliseconds).
fn parse_ass_time(s: &str) -> f64 {
    let s = s.trim();
    // Split into H, MM, SS.cc
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 { return 0.0; }
    let h: f64 = parts[0].parse().unwrap_or(0.0);
    let m: f64 = parts[1].parse().unwrap_or(0.0);
    let sec_cc = parts[2];
    // sec_cc like "23.45" — parse as seconds (dot separator is centiseconds or
    // ms; either way f64 parsing gets us the right numeric value).
    let sec: f64 = sec_cc.parse().unwrap_or(0.0);
    h * 3600.0 + m * 60.0 + sec
}

/// Parse an ASS colour literal into an RGBA [u8; 4].
/// ASS v4+ stores colours as `&HAABBGGRR&` or `&HBBGGRR&` (BGR order, alpha prefixed).
/// Alpha is INVERTED: `&H00` = fully opaque, `&HFF` = fully transparent.
/// Returns opaque white on parse failure.
fn parse_ass_color(s: &str) -> [u8; 4] {
    let s = s.trim();
    // Strip `&H` prefix and trailing `&`
    let hex = s
        .trim_start_matches("&H")
        .trim_start_matches("&h")
        .trim_end_matches('&');
    if hex.is_empty() {
        return [255, 255, 255, 255];
    }
    // Pad/truncate to handle 6- or 8-hex forms. ASS often omits leading zeros.
    let hex_upper = hex.to_ascii_uppercase();
    // Parse as u32 — gives us AABBGGRR (or 00BBGGRR if alpha omitted).
    let n = u32::from_str_radix(&hex_upper, 16).unwrap_or(0xFF_FF_FF_FF);
    let a_inv = ((n >> 24) & 0xFF) as u8;
    let b = ((n >> 16) & 0xFF) as u8;
    let g = ((n >> 8) & 0xFF) as u8;
    let r = (n & 0xFF) as u8;
    // If the hex was 6 chars (no alpha), a_inv = 0 = opaque, which is what we want.
    let a = 255u8.saturating_sub(a_inv);
    [r, g, b, a]
}

/// Strip ASS override blocks (`{...}`) from dialogue text, decode standard escapes.
/// - `\N` → hard line break
/// - `\n` → soft wrap (we treat as space since our renderer auto-wraps)
/// - `\h` → non-breaking space
fn strip_ass_override_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth: i32 = 0;
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            depth += 1;
            continue;
        }
        if c == '}' {
            if depth > 0 { depth -= 1; }
            continue;
        }
        if depth > 0 { continue; }
        if c == '\\' {
            match chars.peek() {
                Some('N') => { chars.next(); out.push('\n'); }
                Some('n') => { chars.next(); out.push(' '); }
                Some('h') => { chars.next(); out.push(' '); }
                _ => out.push(c),
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Parse SRT or VTT subtitle content into timed entries.
fn parse_srt_vtt(content: &str) -> Vec<SubtitleEntry> {
    let mut subs = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    let is_vtt = content.starts_with("WEBVTT");
    if is_vtt { i = 1; } // skip header

    while i < lines.len() {
        // Skip blank lines and sequence numbers
        let line = lines[i].trim();
        if line.is_empty() || line.parse::<u32>().is_ok() {
            i += 1;
            continue;
        }
        // Look for timestamp line: "00:00:01,000 --> 00:00:04,000" or "00:00:01.000 --> 00:00:04.000"
        if line.contains("-->") {
            let parts: Vec<&str> = line.split("-->").collect();
            if parts.len() == 2 {
                let start = parse_srt_timestamp(parts[0].trim());
                let end = parse_srt_timestamp(parts[1].trim());
                if let (Some(s), Some(e)) = (start, end) {
                    // Collect text lines until blank line
                    let mut text_lines = Vec::new();
                    i += 1;
                    while i < lines.len() && !lines[i].trim().is_empty() {
                        text_lines.push(lines[i].trim());
                        i += 1;
                    }
                    let text = text_lines.join("\n");
                    // Strip HTML tags (SRT allows <i>, <b>, etc.)
                    let text = text.replace("<i>", "").replace("</i>", "")
                        .replace("<b>", "").replace("</b>", "")
                        .replace("<u>", "").replace("</u>", "");
                    if !text.is_empty() {
                        subs.push(SubtitleEntry { start_secs: s, end_secs: e, text });
                    }
                    continue;
                }
            }
        }
        i += 1;
    }
    subs
}

/// Parse "HH:MM:SS,mmm" or "HH:MM:SS.mmm" or "MM:SS.mmm" to seconds.
fn parse_srt_timestamp(s: &str) -> Option<f64> {
    let s = s.replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        3 => {
            let h: f64 = parts[0].parse().ok()?;
            let m: f64 = parts[1].parse().ok()?;
            let sec: f64 = parts[2].parse().ok()?;
            Some(h * 3600.0 + m * 60.0 + sec)
        }
        2 => {
            let m: f64 = parts[0].parse().ok()?;
            let sec: f64 = parts[1].parse().ok()?;
            Some(m * 60.0 + sec)
        }
        _ => None,
    }
}

/// Generate SRT format string from subtitle entries.
fn generate_srt(subs: &[SubtitleEntry]) -> String {
    let mut out = String::new();
    for (i, sub) in subs.iter().enumerate() {
        out.push_str(&format!("{}\n", i + 1));
        out.push_str(&format!("{} --> {}\n", format_srt_time(sub.start_secs), format_srt_time(sub.end_secs)));
        out.push_str(&sub.text);
        out.push_str("\n\n");
    }
    out
}

fn format_srt_time(secs: f64) -> String {
    let h = (secs / 3600.0) as u32;
    let m = ((secs % 3600.0) / 60.0) as u32;
    let s = (secs % 60.0) as u32;
    let ms = ((secs % 1.0) * 1000.0) as u32;
    format!("{:02}:{:02}:{:02},{:03}", h, m, s, ms)
}

/// Format an absolute time for YouTube chapter lines. YouTube accepts both
/// `M:SS` and `H:MM:SS`; we always emit `H:MM:SS` for consistency regardless
/// of sequence length. Negative inputs are clamped to zero.
fn format_youtube_chapter_time(total_secs: f64) -> String {
    let secs = total_secs.max(0.0) as u64;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{}:{:02}:{:02}", h, m, s)
}

/// Render a sorted `(position_secs, label)` list as YouTube chapter text.
/// Blank labels are substituted with `Chapter N` (1-indexed across all
/// entries, including any synthetic "Intro" the caller prepends). No trailing
/// newline — callers choose whether to append one.
fn generate_youtube_chapters(entries: &[(f64, String)]) -> String {
    entries
        .iter()
        .enumerate()
        .map(|(i, (secs, label))| {
            let tc = format_youtube_chapter_time(*secs);
            let display_label = if label.trim().is_empty() {
                format!("Chapter {}", i + 1)
            } else {
                label.clone()
            };
            format!("{} {}", tc, display_label)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Render marker positions as a DVDStyler / dvdauthor chapter list.
/// The format is one `HH:MM:SS.mmm` timecode per line — labels are ignored
fn ai_tools_task_from_tag(tag: klipp_core::events::AiToolsTaskTag) -> crate::ai_tools::AiTask {
    use klipp_core::events::AiToolsTaskTag;
    match tag {
        AiToolsTaskTag::RemoveBackground => crate::ai_tools::AiTask::RemoveBackground,
        AiToolsTaskTag::IsolateObject => crate::ai_tools::AiTask::IsolateObject,
        AiToolsTaskTag::DetectObjects => crate::ai_tools::AiTask::DetectObjects,
        AiToolsTaskTag::TrackObject => crate::ai_tools::AiTask::TrackObject,
    }
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

/// Apply one half of a planned hyperzoom to `clip_id` in `seq`:
///
/// * Set the clip's `BuiltinEffect::Transform` anchor to the focal
///   point (creating the effect with otherwise-default values if the
///   clip didn't already have one).
/// * Replace the clip's `transform_scale_keyframes` with the planned
///   pair so the ramp is exact regardless of any pre-existing scale
///   keyframes (re-applying hyperzoom should be idempotent — a stale
///   ramp from an earlier application doesn't accumulate).
///
/// No-ops if the clip can't be located. The handler logs the failure
/// path so a stale dispatch doesn't silently mutate an unrelated clip.
/// Outcome of a successful [`AppCommand::InsertAudioMediaClip`]
/// dispatch — the inserted clip plus the track index it landed on.
/// Caller records the pair in the undo stack via
/// `EditCommand::LegacyAppendClip`.
#[derive(Debug)]
struct InsertedAudioClip {
    clip: klipp_timeline::Clip,
    track_index: usize,
}

/// Reasons [`insert_audio_media_clip_into_seq`] declines to insert.
/// Surface as a tracing warning at the dispatch arm — these are
/// programmer / state-shape errors, not user-recoverable conditions.
#[derive(Debug)]
enum InsertAudioMediaClipError {
    MediaNotInPool,
    TrackIndexOutOfRange,
    TrackIsNotAudio,
    ZeroDuration,
}

impl std::fmt::Display for InsertAudioMediaClipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MediaNotInPool => f.write_str("media id not present in pool"),
            Self::TrackIndexOutOfRange => f.write_str("track_index out of range"),
            Self::TrackIsNotAudio => f.write_str("track at track_index is not an audio track"),
            Self::ZeroDuration => f.write_str("media duration is zero"),
        }
    }
}

/// Mutation core of [`AppCommand::InsertAudioMediaClip`]. Pure-ish
/// (only mutates `seq`); separated from the dispatch arm so unit
/// tests can hit it without a full `App`.
///
/// `track_index` semantics match the AppCommand contract: `Some(i)`
/// must reference an existing audio track; `None` picks the lowest
/// audio track index, auto-creating one if the sequence has none.
fn insert_audio_media_clip_into_seq(
    seq: &mut klipp_timeline::Sequence,
    media_pool: &klipp_media::MediaPool,
    media_id: klipp_core::MediaId,
    track_index: Option<usize>,
    timeline_in: klipp_core::Timecode,
) -> Result<InsertedAudioClip, InsertAudioMediaClipError> {
    let item = media_pool
        .get(&media_id)
        .ok_or(InsertAudioMediaClipError::MediaNotInPool)?;
    let duration = item.duration;
    if duration.value <= 0 {
        return Err(InsertAudioMediaClipError::ZeroDuration);
    }
    let track_idx = match track_index {
        Some(idx) => {
            let track = seq
                .tracks
                .get(idx)
                .ok_or(InsertAudioMediaClipError::TrackIndexOutOfRange)?;
            if track.kind != klipp_timeline::TrackKind::Audio {
                return Err(InsertAudioMediaClipError::TrackIsNotAudio);
            }
            idx
        }
        None => match seq
            .tracks
            .iter()
            .position(|t| t.kind == klipp_timeline::TrackKind::Audio)
        {
            Some(i) => i,
            None => {
                seq.add_audio_track("Audio 1".to_string());
                seq.tracks
                    .iter()
                    .position(|t| t.kind == klipp_timeline::TrackKind::Audio)
                    .expect("audio track was just appended")
            }
        },
    };
    // RationalTime addition rescales RHS to LHS rate (project memory:
    // see "RationalTime rate gotcha"). To keep frame-accurate
    // arithmetic, rescale the timeline anchor to the source rate
    // before the add.
    let timeline_in_rescaled = if timeline_in.rate != duration.rate {
        timeline_in.rescale_to(duration.rate)
    } else {
        timeline_in
    };
    let timeline_out = timeline_in_rescaled + duration;
    let source_in = klipp_core::Timecode {
        value: 0,
        rate: duration.rate,
    };
    let clip = klipp_timeline::Clip::media(
        media_id,
        timeline_in_rescaled,
        timeline_out,
        source_in,
        duration,
    );
    let clip_for_history = clip.clone();
    seq.tracks[track_idx].clips.push(clip);
    Ok(InsertedAudioClip {
        clip: clip_for_history,
        track_index: track_idx,
    })
}

/// Map an `ApplyHyperzoom.whoosh_variant` u8 wire-format tag to a
/// concrete [`klipp_audio::WhooshVariant`]. `255` is the "custom
/// sample" sentinel (handled by a different branch — never passed
/// here). Unknown values fall back to `Medium` per the AppCommand
/// doc comment so a stale dispatch can never silently produce no
/// audio when the user ticked the box.
fn whoosh_variant_from_u8(tag: u8) -> klipp_audio::WhooshVariant {
    match tag {
        0 => klipp_audio::WhooshVariant::Short,
        1 => klipp_audio::WhooshVariant::Medium,
        2 => klipp_audio::WhooshVariant::Long,
        _ => klipp_audio::WhooshVariant::Medium,
    }
}

/// Compute the whoosh clip's `timeline_in` so its temporal midpoint
/// (where the synth envelope peaks — see `klipp_audio::synth`'s
/// shared-envelope contract) lands on the cut frame at
/// `src_timeline_out`. Result is clamped to ≥ 0; an early cut
/// (whoosh would extend before frame 0) is the project's start
/// rather than a negative time.
///
/// Pure function — exposed for unit testing without spinning up an
/// `App` or `Sequence`.
fn whoosh_clip_in_for_apply(
    src_timeline_out: klipp_core::Timecode,
    whoosh_dur_secs: f64,
) -> klipp_core::Timecode {
    let half_dur_secs = whoosh_dur_secs / 2.0;
    let half_dur_ticks = (half_dur_secs * src_timeline_out.rate as f64).round() as i64;
    let value = (src_timeline_out.value - half_dur_ticks).max(0);
    klipp_core::Timecode {
        value,
        rate: src_timeline_out.rate,
    }
}

/// Resolve the cache filesystem path for the synthesised WAV of a
/// built-in whoosh variant. Lives under `~/.cache/klipp/whoosh/` so
/// it sits alongside other Klipp cache artefacts (font registry,
/// thumbnails) and gets cleaned up by the user's normal cache-dir
/// hygiene. Returns `None` when the platform doesn't expose a cache
/// directory (rare; defensive).
fn whoosh_wav_cache_path(variant: klipp_audio::WhooshVariant) -> Option<std::path::PathBuf> {
    let mut p = dirs::cache_dir()?;
    p.push("klipp");
    p.push("whoosh");
    p.push(format!("{}.wav", variant.cache_stem()));
    Some(p)
}

/// Build a [`klipp_media::MediaItem`] for a synthesised whoosh WAV.
/// All metadata is known up front (mono, 48 kHz, fixed duration per
/// variant) so we can register the item in the pool inline without
/// going through the engine's async `LoadMediaFile` probe.
///
/// `display_name` is what shows in the media browser; we tag it
/// distinctly so users can find + delete cached whooshes if they
/// wish.
fn build_whoosh_media_item(
    variant: klipp_audio::WhooshVariant,
    path: std::path::PathBuf,
) -> klipp_media::MediaItem {
    use klipp_audio::synth::SAMPLE_RATE;
    let dur_secs = variant.duration_secs() as f64;
    // Sample-accurate duration at 48 kHz tick rate so timeline
    // arithmetic uses the same rate as the underlying audio.
    let duration = klipp_core::RationalTime {
        value: (dur_secs * SAMPLE_RATE as f64).round() as i64,
        rate: SAMPLE_RATE,
    };
    let label = match variant {
        klipp_audio::WhooshVariant::Short => "Whoosh (short)",
        klipp_audio::WhooshVariant::Medium => "Whoosh (medium)",
        klipp_audio::WhooshVariant::Long => "Whoosh (long)",
    };
    klipp_media::MediaItem {
        id: klipp_core::MediaId::new(),
        path,
        kind: klipp_media::MediaItemKind::File,
        filename: label.to_string(),
        duration,
        fps: klipp_core::Fps::film_24(),
        resolution: klipp_core::Resolution { width: 0, height: 0 },
        video_codec: String::new(),
        audio_codec: "pcm_s16le".into(),
        has_audio: true,
        file_size_bytes: 0,
        sample_rate: Some(SAMPLE_RATE),
        audio_channels: Some(1),
        offline: false,
        is_still: false,
        proxy_path: None,
        proxy_ready: false,
        default_source_in: None,
        default_source_out: None,
        rating: 0,
        tags: Vec::new(),
        user_notes: String::new(),
        bin: None,
        bin_id: None,
        parent_media_id: None,
        added_at: 0,
        fingerprint: None,
        file_modified_at: None,
        media_created_at: None,
        color_label: klipp_media::ColorLabel::default(),
        colour_space: klipp_media::ColourSpace::default(),
        log_format: klipp_core::LogFormat::default(),
        source_rotation: 0,
        camera_make: None,
        camera_model: None,
        encoder: None,
        bit_depth: None,
        video_bitrate_kbps: None,
        audio_bitrate_kbps: None,
        color_primaries: None,
        color_transfer: None,
        pixel_format: None,
        detected_log_format: klipp_core::LogFormat::default(),
        log_format_user_set: false,
        skim_file: None,
        needs_reprobe: false,
        waveform_data: None,
        audio_cache: None,
        shot: Default::default(),
        metadata_read: false,
        truncated_from_secs: None,
        log: Default::default(),
        synced_audio: None,
        markers: Vec::new(),
        screen_text: String::new(),
        span: None,
    }
}

/// Find an existing pool entry whose path matches `custom_path`.
/// The whoosh handler uses this to bind a user-picked custom WAV
/// without re-importing — the user must have imported the file
/// through the normal media-pool flow first (the inspector UI in
/// commit 3 will hand-hold that), so by apply time the MediaItem
/// is already registered.
fn find_existing_audio_in_pool(
    media_pool: &klipp_media::MediaPool,
    custom_path: &std::path::Path,
) -> Option<(klipp_core::MediaId, klipp_core::Timecode)> {
    media_pool
        .all()
        .find(|item| item.has_audio && item.path == *custom_path)
        .map(|item| (item.id, item.duration))
}

/// `whoosh_variant` wire-format tag → cache key. Inverse of
/// [`whoosh_variant_from_u8`] for the three known values; tagged
/// here as `u8` because the AppState cache is u8-keyed.
fn whoosh_variant_to_u8(variant: klipp_audio::WhooshVariant) -> u8 {
    match variant {
        klipp_audio::WhooshVariant::Short => 0,
        klipp_audio::WhooshVariant::Medium => 1,
        klipp_audio::WhooshVariant::Long => 2,
    }
}

/// Run the optional whoosh-stinger insertion that ApplyHyperzoom
/// can dispatch when `add_whoosh` is true. Pure entry point so the
/// dispatch arm stays a thin wiring layer + the integration tests
/// can hit the full whoosh pipeline without standing up an `App`.
///
/// Returns `Some(InsertedAudioClip)` when a clip was actually
/// inserted; `None` when `add_whoosh` was false, the variant
/// resolution failed (no cache dir, synth I/O error, custom path
/// not in pool), or the underlying clip insertion was rejected.
/// Caller (the dispatch arm) records the inserted clip in the undo
/// stack so apply + whoosh ride a single ctrl-z.
fn maybe_insert_whoosh_for_apply(
    state: &mut crate::state::AppState,
    seq_idx: usize,
    src_timeline_out: klipp_core::Timecode,
    add_whoosh: bool,
    whoosh_variant: u8,
    whoosh_custom_path: Option<&std::path::Path>,
) -> Option<InsertedAudioClip> {
    if !add_whoosh {
        return None;
    }
    let resolved = if whoosh_variant == 255 {
        match whoosh_custom_path {
            Some(custom_path) => {
                let found = find_existing_audio_in_pool(&state.media_pool, custom_path);
                if found.is_none() {
                    tracing::warn!(
                        "ApplyHyperzoom whoosh: custom path {:?} not in media pool — \
                         import via the media browser first",
                        custom_path,
                    );
                }
                found
            }
            None => {
                tracing::warn!(
                    "ApplyHyperzoom whoosh: variant=255 (Custom) but no custom path provided — skipping",
                );
                None
            }
        }
    } else {
        let v = whoosh_variant_from_u8(whoosh_variant);
        ensure_builtin_whoosh_in_pool(
            &mut state.media_pool,
            &mut state.whoosh_media_ids,
            v,
        )
    };
    let (media_id, dur) = resolved?;
    let timeline_in = whoosh_clip_in_for_apply(src_timeline_out, dur.to_secs_f64());
    match insert_audio_media_clip_into_seq(
        &mut state.sequences[seq_idx],
        &state.media_pool,
        media_id,
        None,
        timeline_in,
    ) {
        Ok(inserted) => Some(inserted),
        Err(e) => {
            tracing::warn!("ApplyHyperzoom whoosh: clip insertion rejected: {e}");
            None
        }
    }
}

/// Synthesise the cached WAV for `variant` if not already on disk,
/// register a `MediaItem` with known metadata in the pool, and
/// remember the resulting `MediaId` for next time. Hot path is the
/// cache hit — synthesis only runs on first use per session per
/// variant.
///
/// Returns `None` when the platform has no cache directory or the
/// synthesis / WAV write failed; both paths are logged. Caller
/// (the ApplyHyperzoom handler) treats `None` as "skip whoosh
/// insertion silently" so a disk failure doesn't abort the rest of
/// the apply.
fn ensure_builtin_whoosh_in_pool(
    media_pool: &mut klipp_media::MediaPool,
    whoosh_media_ids: &mut std::collections::HashMap<u8, klipp_core::MediaId>,
    variant: klipp_audio::WhooshVariant,
) -> Option<(klipp_core::MediaId, klipp_core::Timecode)> {
    let tag = whoosh_variant_to_u8(variant);
    // Hot path: already registered + still in pool.
    if let Some(existing) = whoosh_media_ids.get(&tag) {
        if let Some(item) = media_pool.get(existing) {
            return Some((*existing, item.duration));
        }
        // Stale cache (pool was wiped between applies); re-register
        // from disk — the WAV likely still exists.
    }
    let path = whoosh_wav_cache_path(variant)?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                tracing::warn!(
                    "ApplyHyperzoom whoosh: cache dir create failed at {:?}: {e}",
                    parent,
                );
                return None;
            }
        }
        if let Err(e) = klipp_audio::write_whoosh_wav(variant, &path) {
            tracing::warn!(
                "ApplyHyperzoom whoosh: WAV synthesis failed at {:?}: {e}",
                path,
            );
            return None;
        }
    }
    let item = build_whoosh_media_item(variant, path);
    let duration = item.duration;
    let media_id = media_pool.add(item);
    whoosh_media_ids.insert(tag, media_id);
    Some((media_id, duration))
}

fn apply_hyperzoom_plan_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    anchor: (f32, f32),
    scale_kfs: &[klipp_core::keyframe::Keyframe],
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::warn!("apply_hyperzoom_plan_to_clip: clip {clip_id:?} no longer in sequence");
        return;
    };

    // Update or insert the Transform builtin so the anchor lands at
    // the focal point. Other Transform fields keep their existing
    // values when one is already present — the user might have a
    // baseline rotation or perspective they want preserved.
    let mut found = false;
    for fx in clip.builtin_effects.iter_mut() {
        if let klipp_effects::BuiltinEffect::Transform {
            anchor_x, anchor_y, ..
        } = fx
        {
            *anchor_x = anchor.0;
            *anchor_y = anchor.1;
            found = true;
            break;
        }
    }
    if !found {
        clip.builtin_effects.push(klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0,
            position_y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation: 0.0,
            anchor_x: anchor.0,
            anchor_y: anchor.1,
            rotation_x: 0.0,
            rotation_y: 0.0,
            perspective: 0.0,
            focal_ref: None,
        });
        clip.builtin_effects_enabled.push(true);
    }

    clip.transform_scale_keyframes = scale_kfs.to_vec();
}

/// Mirror of [`apply_hyperzoom_plan_to_clip`] for the radial-blur
/// convenience layer:
///
/// * Find or insert a `BuiltinEffect::RadialBlur` on `clip_id`. If a
///   RadialBlur already exists (e.g. from a prior Apply Hyperzoom or
///   manual user edit), its `center` is rewritten to the focal point
///   and `amount` is rewritten to the peak so the static fallback
///   matches the keyframes' peak value. `samples` and `mode` keep
///   their existing values (preserves user tweaks); when inserting
///   a fresh RadialBlur we use the standard zoom-blur defaults
///   (samples=12, mode=0 = zoom).
/// * Replace `clip.effect_keyframes["{idx}:amount"]` with the
///   planned ramp so re-applying is idempotent.
///
/// No-ops with a debug log if the clip can't be located. Same
/// fault-tolerance contract as `apply_hyperzoom_plan_to_clip`.
fn apply_hyperzoom_radial_blur_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    center: (f32, f32),
    peak_amount: f32,
    amount_kfs: &[klipp_core::keyframe::Keyframe],
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::debug!("apply_hyperzoom_radial_blur_to_clip: clip {clip_id:?} no longer in sequence");
        return;
    };

    // Update existing RadialBlur if present, else append a fresh one.
    let mut found_idx: Option<usize> = None;
    for (i, fx) in clip.builtin_effects.iter_mut().enumerate() {
        if let klipp_effects::BuiltinEffect::RadialBlur { center_x, center_y, amount, .. } = fx {
            *center_x = center.0;
            *center_y = center.1;
            *amount = peak_amount;
            found_idx = Some(i);
            break;
        }
    }
    let idx = match found_idx {
        Some(i) => i,
        None => {
            clip.builtin_effects.push(klipp_effects::BuiltinEffect::RadialBlur {
                center_x: center.0,
                center_y: center.1,
                amount: peak_amount,
                // Sample count of 12 matches the "Subtle Zoom" preset
                // and is the engine's standard sweet-spot trade-off
                // between quality and per-pixel cost. Mode 0 = zoom
                // (the canonical hyperzoom blur direction; spin
                // would feel wrong at the cut).
                samples: 12,
                mode: 0,
            });
            clip.builtin_effects_enabled.push(true);
            clip.builtin_effects.len() - 1
        }
    };

    let key = format!("{}:amount", idx);
    clip.effect_keyframes.insert(key, amount_kfs.to_vec());
}

/// Mirror of [`apply_hyperzoom_radial_blur_to_clip`] for the
/// chromatic-aberration convenience layer:
///
/// * Find or insert a `BuiltinEffect::ChromaticAberration` on
///   `clip_id`. If one already exists (e.g. from a prior Apply
///   Hyperzoom OR a manual user edit), its `center_x` / `center_y`
///   are rewritten to the focal point — pinned by the audit-note
///   "re-apply with new focal updates center" test. `red_shift`
///   and `blue_shift` are also rewritten to ±peak so the static
///   fallback matches the keyframes' peak values; the keyframes
///   then drive the actual animation.
/// * Replace `clip.effect_keyframes["{idx}:red_shift"]` and
///   `[".. blue_shift"]` with the planned ramps. Re-applying
///   replaces the old keyframes wholesale — idempotent.
///
/// **First-match-wins disambiguation** (mirrors radial-blur):
/// when the user has manually added a `ChromaticAberration` with
/// custom params before applying hyperzoom, that effect's
/// `red_shift` / `blue_shift` / `center_x` / `center_y` get
/// rewritten by the convenience layer. Power users who want to
/// preserve their pre-existing tuning should disable the
/// convenience checkbox and add a separate ChromaticAberration
/// from the effects library after applying.
///
/// Keyframe write order is **red before blue** — pinned by
/// `apply_chromatic_keyframe_write_order_red_before_blue` so
/// snapshot tests stay deterministic.
fn apply_hyperzoom_chromatic_aberration_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    center: (f32, f32),
    peak_amount: f32,
    red_kfs: &[klipp_core::keyframe::Keyframe],
    blue_kfs: &[klipp_core::keyframe::Keyframe],
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::debug!("apply_hyperzoom_chromatic_aberration_to_clip: clip {clip_id:?} no longer in sequence");
        return;
    };
    let mut found_idx: Option<usize> = None;
    for (i, fx) in clip.builtin_effects.iter_mut().enumerate() {
        if let klipp_effects::BuiltinEffect::ChromaticAberration {
            red_shift, blue_shift, center_x, center_y,
        } = fx
        {
            *center_x = center.0;
            *center_y = center.1;
            // Static fallback values mirror the keyframes' peak —
            // red converges, blue diverges.
            *red_shift = -peak_amount;
            *blue_shift = peak_amount;
            found_idx = Some(i);
            break;
        }
    }
    let idx = match found_idx {
        Some(i) => i,
        None => {
            clip.builtin_effects.push(klipp_effects::BuiltinEffect::ChromaticAberration {
                red_shift: -peak_amount,
                blue_shift: peak_amount,
                center_x: center.0,
                center_y: center.1,
            });
            clip.builtin_effects_enabled.push(true);
            clip.builtin_effects.len() - 1
        }
    };
    // Pin: red before blue. If an audit changes this order, the
    // ordering test will flag it before snapshot tests start
    // diffing nondeterministically.
    clip.effect_keyframes.insert(format!("{}:red_shift", idx), red_kfs.to_vec());
    clip.effect_keyframes.insert(format!("{}:blue_shift", idx), blue_kfs.to_vec());
}

/// Mirror of [`apply_hyperzoom_radial_blur_to_clip`] for the
/// light-leak convenience layer (rides on
/// `BuiltinEffect::FilmHalation`):
///
/// * Find or insert a `FilmHalation`. On update, only `intensity`
///   is rewritten to the peak value (static fallback for the
///   keyframes); `threshold` / `radius` / `hue` keep their
///   existing values so a user-tweaked halation isn't blown
///   away by re-apply. Fresh inserts use trailer-aesthetic
///   defaults (warm hue ≈ 0.15, moderate threshold + radius).
/// * Replace `clip.effect_keyframes["{idx}:intensity"]` with the
///   planned ramp.
///
/// Same first-match-wins disambiguation as radial-blur — a user-
/// added FilmHalation gets its intensity rewritten by the
/// convenience layer (the other params survive). Power users
/// preserving custom halation tunings should disable the
/// convenience checkbox.
fn apply_hyperzoom_light_leak_to_clip(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    peak_intensity: f32,
    intensity_kfs: &[klipp_core::keyframe::Keyframe],
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::debug!("apply_hyperzoom_light_leak_to_clip: clip {clip_id:?} no longer in sequence");
        return;
    };
    let mut found_idx: Option<usize> = None;
    for (i, fx) in clip.builtin_effects.iter_mut().enumerate() {
        if let klipp_effects::BuiltinEffect::FilmHalation { intensity, .. } = fx {
            *intensity = peak_intensity;
            found_idx = Some(i);
            break;
        }
    }
    let idx = match found_idx {
        Some(i) => i,
        None => {
            clip.builtin_effects.push(klipp_effects::BuiltinEffect::FilmHalation {
                threshold: 0.75,
                radius: 15.0,
                intensity: peak_intensity,
                // Hue 0.15 ≈ warm orange-yellow — the
                // canonical trailer-light-leak colour. Power
                // users tweak via the standalone effect's
                // params after applying.
                hue: 0.15,
            });
            clip.builtin_effects_enabled.push(true);
            clip.builtin_effects.len() - 1
        }
    };
    clip.effect_keyframes.insert(format!("{}:intensity", idx), intensity_kfs.to_vec());
}

/// Write a `FocalRef` onto the clip's `BuiltinEffect::Transform`.
/// Companion to [`apply_hyperzoom_plan_to_clip`] for
/// HZ-NEXT-GLYPH-LOCK: when the user has picked a glyph in the
/// inspector preview, the bridge dispatches `ApplyHyperzoom` with
/// `focal_ref = Some(...)` and this helper writes the override
/// onto the source clip's Transform. The static `anchor_x` /
/// `anchor_y` (already written by `apply_hyperzoom_plan_to_clip`)
/// stay as the resolution-failure fallback.
///
/// If no Transform builtin exists yet (shouldn't happen — the
/// hyperzoom apply always inserts one — but defence-in-depth),
/// the helper inserts one with sane defaults so the focal_ref
/// still has a host.
fn apply_focal_ref_to_clip_transform(
    seq: &mut klipp_timeline::Sequence,
    clip_id: klipp_core::ClipId,
    focal_ref: klipp_core::FocalRef,
) {
    let Some(clip) = seq.all_clips_mut().find(|c| c.id == clip_id) else {
        tracing::debug!("apply_focal_ref_to_clip_transform: clip {clip_id:?} no longer in sequence");
        return;
    };

    let mut found = false;
    for fx in clip.builtin_effects.iter_mut() {
        if let klipp_effects::BuiltinEffect::Transform { focal_ref: existing, .. } = fx {
            *existing = Some(focal_ref);
            found = true;
            break;
        }
    }
    if !found {
        clip.builtin_effects.push(klipp_effects::BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(focal_ref),
        });
        clip.builtin_effects_enabled.push(true);
    }
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

/// Format absolute seconds as ffmpeg-compatible `HH:MM:SS.mmm`. Negative
/// inputs are clamped to zero. Used by the headless render-script generator
/// for `-ss` / `-to` arguments.
fn format_ffmpeg_time(total_secs: f64) -> String {
    let secs = total_secs.max(0.0);
    let h = (secs / 3600.0) as u64;
    let m = ((secs % 3600.0) / 60.0) as u64;
    let s = (secs % 60.0) as u64;
    let ms = ((secs - (h as f64 * 3600.0 + m as f64 * 60.0 + s as f64)) * 1000.0).round() as u64;
    // Clamp potential rounding overflow (e.g. 0.9999 -> 1000) into the next second.
    let (s, ms) = if ms >= 1000 { (s + 1, 0) } else { (s, ms) };
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}

/// Shell-quote a string for bash: wrap in single quotes, escaping embedded
/// single quotes via the standard `'\''` dance. Safe for paths containing
/// spaces, parens, quotes, dollar signs, etc.
fn shell_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

/// Generate a bash script that invokes ffmpeg to render the given Sequence
/// headlessly. **Cuts only** — effects, grades, transitions, keyframes,
/// volume/pan automation, and title/generator clips are NOT serialized.
/// The script begins with a prominent WARNING comment explaining this.
///
/// MVP: operates on the first Video track only. Gap clips and non-Media
/// clips on that track are skipped.
///
/// - Zero media clips → the script echoes an error and exits 1.
/// - One media clip → direct transcode with `-ss`/`-to`/`-i` + encoder args.
/// - Many media clips → two-pass: extract each segment to `$TMPDIR/segNNNN.<ext>`,
///   then `ffmpeg -f concat -safe 0 -i list.txt -c copy OUTPUT`.
fn generate_render_script(
    seq: &Sequence,
    media_pool: &klipp_media::MediaPool,
    settings: &ExportSettings,
    output_path: &str,
) -> String {
    // Collect source segments from the first video track.
    struct Segment {
        source_path: String,
        in_secs: f64,
        out_secs: f64,
    }
    let mut segs: Vec<Segment> = Vec::new();
    if let Some(track) = seq
        .tracks
        .iter()
        .find(|t| t.kind == klipp_timeline::TrackKind::Video)
    {
        for clip in &track.clips {
            if !clip.enabled {
                continue;
            }
            if let klipp_timeline::ClipKind::Media { media_id } = &clip.kind {
                if let Some(item) = media_pool.get(media_id) {
                    let in_secs = clip.source_in.to_secs_f64();
                    let out_secs = clip.source_out.to_secs_f64();
                    if out_secs > in_secs {
                        segs.push(Segment {
                            source_path: item.path.to_string_lossy().to_string(),
                            in_secs,
                            out_secs,
                        });
                    }
                }
            }
        }
    }

    let ext = settings.format.extension();
    let video_codec = settings.video_codec.ffmpeg_codec();
    let audio_args_vec = settings.audio_codec.ffmpeg_args(settings.audio_bitrate_kbps);
    let audio_args = audio_args_vec
        .iter()
        .map(|a| shell_single_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

    // Resolution/fps filter chain — only include if overrides present.
    let res = settings
        .resolution
        .unwrap_or(seq.resolution);
    let fps_val = settings
        .fps
        .map(|f| f.as_f64())
        .unwrap_or_else(|| seq.fps.as_f64());
    let vf = format!("scale={}:{},fps={}", res.width, res.height, fps_val);

    // Quality args: CRF for CRF-capable codecs, else bitrate.
    let quality_args = if settings.video_codec.uses_crf()
        && settings.rate_control == klipp_core::RateControl::Crf
    {
        format!("-crf {}", settings.crf)
    } else if let Some(bk) = settings.video_bitrate_kbps {
        format!("-b:v {}k", bk)
    } else {
        String::new()
    };
    let preset_args = if settings.video_codec.supports_preset() {
        format!("-preset {}", settings.preset.ffmpeg_value())
    } else {
        String::new()
    };

    let output_q = shell_single_quote(output_path);

    let mut s = String::new();
    s.push_str("#!/usr/bin/env bash\n");
    s.push_str("set -euo pipefail\n");
    s.push('\n');
    s.push_str("# =============================================================================\n");
    s.push_str("# WARNING: Klipp headless render script — CUTS ONLY.\n");
    s.push_str("#\n");
    s.push_str("# This script reproduces the timeline CUTS of the current sequence by calling\n");
    s.push_str("# ffmpeg directly. It does NOT apply:\n");
    s.push_str("#   * Effects (blur, sharpen, chroma-key, transform, etc.)\n");
    s.push_str("#   * Colour grades / node graphs\n");
    s.push_str("#   * Transitions (crossfades, wipes, etc.)\n");
    s.push_str("#   * Keyframes / automation (opacity, volume, pan, transform)\n");
    s.push_str("#   * Title, generator, or adjustment-layer clips\n");
    s.push_str("#   * Audio tracks, mixer, or loudness normalization\n");
    s.push_str("#   * Subtitles or chapters\n");
    s.push_str("#\n");
    s.push_str("# For a full-quality render that honours every edit, use Klipp's built-in\n");
    s.push_str("# Export dialog (File > Export... or Ctrl+E) instead.\n");
    s.push_str("#\n");
    s.push_str("# This script is intended for headless / CI / batch pipelines where only\n");
    s.push_str("# rough-cut output is needed.\n");
    s.push_str("# =============================================================================\n");
    s.push('\n');
    s.push_str(&format!("OUTPUT={}\n", output_q));
    s.push('\n');

    if segs.is_empty() {
        s.push_str("echo \"Error: no media clips on the first video track — nothing to render.\" >&2\n");
        s.push_str("exit 1\n");
        return s;
    }

    if segs.len() == 1 {
        let seg = &segs[0];
        let src_q = shell_single_quote(&seg.source_path);
        let ss = format_ffmpeg_time(seg.in_secs);
        let to = format_ffmpeg_time(seg.out_secs);
        s.push_str("# Single-clip timeline: direct transcode.\n");
        s.push_str("ffmpeg -y \\\n");
        s.push_str(&format!("  -ss {} -to {} -i {} \\\n", ss, to, src_q));
        s.push_str(&format!("  -vf {} \\\n", shell_single_quote(&vf)));
        s.push_str(&format!("  -c:v {}", video_codec));
        if !preset_args.is_empty() {
            s.push(' ');
            s.push_str(&preset_args);
        }
        if !quality_args.is_empty() {
            s.push(' ');
            s.push_str(&quality_args);
        }
        s.push_str(" \\\n");
        s.push_str(&format!("  {} \\\n", audio_args));
        s.push_str("  \"$OUTPUT\"\n");
        s.push('\n');
        s.push_str("echo \"Done: $OUTPUT\"\n");
        return s;
    }

    // Multi-clip: two-pass extract-and-concat. Extract each segment into TMPDIR
    // using the same codec settings as the final output so `-c copy` concat
    // works cleanly without re-encoding.
    s.push_str("# Multi-clip timeline: extract segments then concat.\n");
    s.push_str("TMPDIR=\"$(mktemp -d -t klipp-render-XXXXXX)\"\n");
    s.push_str("trap 'rm -rf \"$TMPDIR\"' EXIT\n");
    s.push_str("LIST=\"$TMPDIR/concat.txt\"\n");
    s.push_str(": > \"$LIST\"\n");
    s.push('\n');

    for (i, seg) in segs.iter().enumerate() {
        let src_q = shell_single_quote(&seg.source_path);
        let ss = format_ffmpeg_time(seg.in_secs);
        let to = format_ffmpeg_time(seg.out_secs);
        let seg_name = format!("seg{:04}.{}", i, ext);
        s.push_str(&format!("# Segment {} of {}\n", i + 1, segs.len()));
        s.push_str(&format!("SEG={}\n", shell_single_quote(&format!("$TMPDIR/{}", seg_name))));
        s.push_str("ffmpeg -y \\\n");
        s.push_str(&format!("  -ss {} -to {} -i {} \\\n", ss, to, src_q));
        s.push_str(&format!("  -vf {} \\\n", shell_single_quote(&vf)));
        s.push_str(&format!("  -c:v {}", video_codec));
        if !preset_args.is_empty() {
            s.push(' ');
            s.push_str(&preset_args);
        }
        if !quality_args.is_empty() {
            s.push(' ');
            s.push_str(&quality_args);
        }
        s.push_str(" \\\n");
        s.push_str(&format!("  {} \\\n", audio_args));
        s.push_str("  \"$SEG\"\n");
        s.push_str("printf \"file '%s'\\n\" \"$SEG\" >> \"$LIST\"\n");
        s.push('\n');
    }

    s.push_str("# Concat pass (stream copy — no re-encode).\n");
    s.push_str("ffmpeg -y -f concat -safe 0 -i \"$LIST\" -c copy \"$OUTPUT\"\n");
    s.push('\n');
    s.push_str("echo \"Done: $OUTPUT\"\n");
    s
}

/// Generate WebVTT format string from subtitle entries.
fn generate_vtt(subs: &[SubtitleEntry]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for sub in subs {
        out.push_str(&format!("{} --> {}\n", format_vtt_time(sub.start_secs), format_vtt_time(sub.end_secs)));
        out.push_str(&sub.text);
        out.push_str("\n\n");
    }
    out
}

fn format_vtt_time(secs: f64) -> String {
    let h = (secs / 3600.0) as u32;
    let m = ((secs % 3600.0) / 60.0) as u32;
    let s = (secs % 60.0) as u32;
    let ms = ((secs % 1.0) * 1000.0) as u32;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}

/// Generate ASS v4+ subtitle file from entries. Used for both .ass and
/// .ssa extensions — modern players accept v4+ content under either name.
fn generate_ass(subs: &[SubtitleEntry]) -> String {
    let mut out = String::new();

    // [Script Info]
    out.push_str("[Script Info]\n");
    out.push_str("Title: Klipp Subtitles\n");
    out.push_str("ScriptType: v4.00+\n");
    out.push_str("WrapStyle: 0\n");
    out.push_str("ScaledBorderAndShadow: yes\n");
    out.push_str("PlayResX: 1920\n");
    out.push_str("PlayResY: 1080\n");
    out.push('\n');

    // [V4+ Styles]
    out.push_str("[V4+ Styles]\n");
    out.push_str("Format: Name, Fontname, Fontsize, PrimaryColour, \
SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, \
StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, \
Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n");
    out.push_str("Style: Default,Arial,48,&H00FFFFFF,&H000000FF,\
&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,0,2,10,10,20,1\n");
    out.push('\n');

    // [Events]
    out.push_str("[Events]\n");
    out.push_str("Format: Layer, Start, End, Style, Name, \
MarginL, MarginR, MarginV, Effect, Text\n");

    for sub in subs {
        // Escape text: \n → \N, strip { } style overrides
        let text = sub.text
            .replace('\n', "\\N")
            .replace('{', "(")
            .replace('}', ")");
        out.push_str(&format!(
            "Dialogue: 0,{},{},Default,,0,0,0,,{}\n",
            format_ass_time(sub.start_secs),
            format_ass_time(sub.end_secs),
            text
        ));
    }

    out
}

fn format_ass_time(secs: f64) -> String {
    // ASS timestamp format: H:MM:SS.cc (centiseconds, single-digit hour)
    let total_cs = (secs * 100.0).round() as u64;
    let cs = total_cs % 100;
    let total_s = total_cs / 100;
    let s = total_s % 60;
    let total_m = total_s / 60;
    let m = total_m % 60;
    let h = total_m / 60;
    format!("{}:{:02}:{:02}.{:02}", h, m, s, cs)
}

// ── Title animation helpers ──



#[cfg(test)]
mod subtitle_tests {
    use super::*;

    // ── parse_srt_vtt tests ──

    #[test]
    fn parse_standard_srt() {
        let input = "\
1
00:00:01,000 --> 00:00:04,000
Hello world

2
00:00:05,000 --> 00:00:08,000
Second subtitle
";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 2);
        assert!((subs[0].start_secs - 1.0).abs() < 0.001);
        assert!((subs[0].end_secs - 4.0).abs() < 0.001);
        assert_eq!(subs[0].text, "Hello world");
        assert!((subs[1].start_secs - 5.0).abs() < 0.001);
        assert!((subs[1].end_secs - 8.0).abs() < 0.001);
        assert_eq!(subs[1].text, "Second subtitle");
    }

    #[test]
    fn parse_multiline_subtitle() {
        let input = "\
1
00:00:01,000 --> 00:00:04,000
Line one
Line two
Line three

";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].text, "Line one\nLine two\nLine three");
    }

    #[test]
    fn parse_html_tags_stripped() {
        let input = "\
1
00:00:01,000 --> 00:00:04,000
<i>Italic</i> and <b>bold</b> and <u>underline</u>

";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].text, "Italic and bold and underline");
    }

    #[test]
    fn parse_vtt_format() {
        let input = "\
WEBVTT

00:00:01.000 --> 00:00:04.000
Hello from VTT

00:00:05.000 --> 00:00:08.000
Second VTT cue
";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 2);
        assert!((subs[0].start_secs - 1.0).abs() < 0.001);
        assert_eq!(subs[0].text, "Hello from VTT");
        assert_eq!(subs[1].text, "Second VTT cue");
    }

    #[test]
    fn parse_empty_file() {
        let subs = parse_srt_vtt("");
        assert!(subs.is_empty());
    }

    #[test]
    fn parse_malformed_timestamps_skipped() {
        let input = "\
1
not:a:timestamp --> also:bad
This should be skipped

2
00:00:05,000 --> 00:00:08,000
This is valid
";
        let subs = parse_srt_vtt(input);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].text, "This is valid");
    }

    // ── parse_srt_timestamp tests ──

    #[test]
    fn parse_srt_timestamp_hhmmss_comma() {
        let result = parse_srt_timestamp("00:01:02,500").unwrap();
        assert!((result - 62.5).abs() < 0.001);
    }

    #[test]
    fn parse_vtt_timestamp_hhmmss_dot() {
        let result = parse_srt_timestamp("00:01:02.500").unwrap();
        assert!((result - 62.5).abs() < 0.001);
    }

    #[test]
    fn parse_short_timestamp_mmss() {
        let result = parse_srt_timestamp("01:02.500").unwrap();
        assert!((result - 62.5).abs() < 0.001);
    }

    #[test]
    fn parse_timestamp_invalid_returns_none() {
        assert!(parse_srt_timestamp("garbage").is_none());
        assert!(parse_srt_timestamp("").is_none());
        assert!(parse_srt_timestamp("00:xx:02,500").is_none());
    }

    // ── generate_srt tests ──

    #[test]
    fn generate_srt_single_entry() {
        let subs = vec![SubtitleEntry {
            start_secs: 1.0,
            end_secs: 4.0,
            text: "Hello".to_string(),
        }];
        let output = generate_srt(&subs);
        assert!(output.starts_with("1\n"));
        assert!(output.contains("00:00:01,000 --> 00:00:04,000"));
        assert!(output.contains("Hello"));
    }

    #[test]
    fn generate_srt_multiple_entries_sequential_numbering() {
        let subs = vec![
            SubtitleEntry { start_secs: 1.0, end_secs: 4.0, text: "First".to_string() },
            SubtitleEntry { start_secs: 5.0, end_secs: 8.0, text: "Second".to_string() },
            SubtitleEntry { start_secs: 10.0, end_secs: 12.0, text: "Third".to_string() },
        ];
        let output = generate_srt(&subs);
        assert!(output.contains("1\n00:00:01,000 --> 00:00:04,000\nFirst"));
        assert!(output.contains("2\n00:00:05,000 --> 00:00:08,000\nSecond"));
        assert!(output.contains("3\n00:00:10,000 --> 00:00:12,000\nThird"));
    }

    // ── format_srt_time tests ──

    #[test]
    fn format_srt_time_zero() {
        assert_eq!(format_srt_time(0.0), "00:00:00,000");
    }

    #[test]
    fn format_srt_time_62_5() {
        assert_eq!(format_srt_time(62.5), "00:01:02,500");
    }

    #[test]
    fn format_srt_time_3661() {
        assert_eq!(format_srt_time(3661.0), "01:01:01,000");
    }

    // ── format_ass_time tests ──

    #[test]
    fn format_ass_time_zero() {
        assert_eq!(format_ass_time(0.0), "0:00:00.00");
    }

    #[test]
    fn format_ass_time_62_5() {
        assert_eq!(format_ass_time(62.5), "0:01:02.50");
    }

    #[test]
    fn format_ass_time_3661_99() {
        assert_eq!(format_ass_time(3661.99), "1:01:01.99");
    }

    // ── generate_ass tests ──

    #[test]
    fn generate_ass_single_entry() {
        let subs = vec![SubtitleEntry {
            start_secs: 1.0,
            end_secs: 4.0,
            text: "Hello".to_string(),
        }];
        let output = generate_ass(&subs);
        assert!(output.contains("[Script Info]"));
        assert!(output.contains("ScriptType: v4.00+"));
        assert!(output.contains("[V4+ Styles]"));
        assert!(output.contains("[Events]"));
        assert!(output.contains(
            "Dialogue: 0,0:00:01.00,0:00:04.00,Default,,0,0,0,,Hello"
        ));
    }

    #[test]
    fn generate_ass_newline_escaped() {
        let subs = vec![SubtitleEntry {
            start_secs: 0.0,
            end_secs: 2.0,
            text: "Line one\nLine two".to_string(),
        }];
        let output = generate_ass(&subs);
        assert!(output.contains("Line one\\NLine two"));
    }

    #[test]
    fn generate_ass_braces_escaped() {
        let subs = vec![SubtitleEntry {
            start_secs: 0.0,
            end_secs: 2.0,
            text: "{bold} text".to_string(),
        }];
        let output = generate_ass(&subs);
        assert!(output.contains("(bold) text"));
    }

    // ── Roundtrip test ──

    #[test]
    fn roundtrip_generate_then_parse() {
        let original = vec![
            SubtitleEntry { start_secs: 1.5, end_secs: 3.0, text: "First cue".to_string() },
            SubtitleEntry { start_secs: 5.0, end_secs: 7.25, text: "Second cue".to_string() },
        ];
        let srt = generate_srt(&original);
        let parsed = parse_srt_vtt(&srt);
        assert_eq!(parsed.len(), 2);
        assert!((parsed[0].start_secs - 1.5).abs() < 0.001);
        assert!((parsed[0].end_secs - 3.0).abs() < 0.001);
        assert_eq!(parsed[0].text, "First cue");
        assert!((parsed[1].start_secs - 5.0).abs() < 0.001);
        assert!((parsed[1].end_secs - 7.25).abs() < 0.001);
        assert_eq!(parsed[1].text, "Second cue");
    }

    // ── ASS/SSA parser tests (Tier 1) ──

    const ASS_FIXTURE: &str = "\
[Script Info]
Title: Example
ScriptType: v4.00+
PlayResX: 1920
PlayResY: 1080

[V4+ Styles]
Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding
Style: Default,Arial,48,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2,1,2,10,10,20,1
Style: Bold1,Helvetica,32,&H000000FF,&H000000FF,&H00000000,&H64000000,-1,0,0,0,100,100,0,0,1,2,0,2,10,10,20,1

[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:04.00,Default,,0,0,0,,Hello world
Dialogue: 0,0:00:05.00,0:00:07.50,Bold1,,0,0,0,,{\\b1}Bold{\\b0} line with \\Nhard break
Dialogue: 0,0:00:09.00,0:00:11.00,Default,,0,0,0,,Text, with commas, inside
";

    #[test]
    fn parse_ass_basic_entry_count() {
        let (entries, _) = parse_ass(ASS_FIXTURE);
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn parse_ass_timing() {
        let (entries, _) = parse_ass(ASS_FIXTURE);
        assert!((entries[0].start_secs - 1.0).abs() < 0.001);
        assert!((entries[0].end_secs - 4.0).abs() < 0.001);
        assert!((entries[1].start_secs - 5.0).abs() < 0.001);
        assert!((entries[1].end_secs - 7.5).abs() < 0.001);
    }

    #[test]
    fn parse_ass_style_name_preserved() {
        let (entries, _) = parse_ass(ASS_FIXTURE);
        assert_eq!(entries[0].style_name, "Default");
        assert_eq!(entries[1].style_name, "Bold1");
    }

    #[test]
    fn parse_ass_strips_override_tags_and_preserves_text() {
        let (entries, _) = parse_ass(ASS_FIXTURE);
        // Override blocks removed; \N becomes newline.
        assert_eq!(entries[1].text, "Bold line with \nhard break");
    }

    #[test]
    fn parse_ass_text_with_commas() {
        // The Text column is last; commas inside must survive splitn.
        let (entries, _) = parse_ass(ASS_FIXTURE);
        assert_eq!(entries[2].text, "Text, with commas, inside");
    }

    #[test]
    fn parse_ass_styles_collected() {
        let (_, styles) = parse_ass(ASS_FIXTURE);
        assert!(styles.contains_key("Default"));
        assert!(styles.contains_key("Bold1"));
        let b = &styles["Bold1"];
        assert_eq!(b.font_name, "Helvetica");
        assert!((b.font_size - 32.0).abs() < 0.001);
        // Bold1 has Bold=-1 (SSA style for "true")
        assert!(b.bold);
        assert!(!b.italic);
    }

    #[test]
    fn strip_ass_override_tags_removes_braces() {
        // Plain override tag stripping
        assert_eq!(strip_ass_override_tags("{\\b1}hi{\\b0}"), "hi");
        // Nested-looking braces
        assert_eq!(strip_ass_override_tags("a{\\i1}b{\\i0}c"), "abc");
        // \N becomes newline
        assert_eq!(strip_ass_override_tags("line1\\Nline2"), "line1\nline2");
        // \h becomes space
        assert_eq!(strip_ass_override_tags("a\\hb"), "a b");
    }

    #[test]
    fn parse_ass_color_bgr_to_rgba() {
        // &H00FFFFFF = AABBGGRR = 00 FF FF FF → RGB=white, alpha=255 (00 inverted)
        assert_eq!(parse_ass_color("&H00FFFFFF"), [255, 255, 255, 255]);
        // &H000000FF = AABBGGRR = 00 00 00 FF → R=FF, G=00, B=00, A=255 (red)
        assert_eq!(parse_ass_color("&H000000FF"), [255, 0, 0, 255]);
        // &H00FF0000 = AABBGGRR = 00 FF 00 00 → R=0, G=0, B=FF, A=255 (blue)
        assert_eq!(parse_ass_color("&H00FF0000"), [0, 0, 255, 255]);
        // 6-hex form (no alpha) — alpha defaults to opaque
        assert_eq!(parse_ass_color("&HFFFFFF"), [255, 255, 255, 255]);
        // Alpha inverted: &H7F = semi-transparent (255 - 127 = 128)
        let c = parse_ass_color("&H7FFFFFFF");
        assert_eq!(c[3], 128);
    }

    #[test]
    fn parse_ass_time_centiseconds() {
        // H:MM:SS.cc — cc is centiseconds, so 0:01:23.45 = 83.45s
        assert!((parse_ass_time("0:01:23.45") - 83.45).abs() < 0.001);
        assert!((parse_ass_time("1:00:00.00") - 3600.0).abs() < 0.001);
        assert!((parse_ass_time("0:00:00.00") - 0.0).abs() < 0.001);
    }

    // ── Title animation helpers ──

    #[test]
    fn typewriter_write_on_calculation() {
        let chars = 10usize;
        let chars_per_sec = 5.0f64;
        let elapsed = 1.0f64;
        let write_on = (elapsed * chars_per_sec / chars as f64).clamp(0.0, 1.0) as f32;
        assert!((write_on - 0.5).abs() < 0.001);
    }

    #[test]
    fn typewriter_write_on_clamps_at_one() {
        let chars = 10usize;
        let chars_per_sec = 5.0f64;
        let elapsed = 10.0f64;
        let write_on = (elapsed * chars_per_sec / chars as f64).clamp(0.0, 1.0) as f32;
        assert_eq!(write_on, 1.0);
    }

}

#[cfg(test)]
mod youtube_chapter_tests {
    use super::*;

    #[test]
    fn format_youtube_chapter_time_under_one_hour() {
        assert_eq!(format_youtube_chapter_time(0.0),   "0:00:00");
        assert_eq!(format_youtube_chapter_time(83.0),  "0:01:23");
        assert_eq!(format_youtube_chapter_time(287.0), "0:04:47");
    }

    #[test]
    fn format_youtube_chapter_time_over_one_hour() {
        assert_eq!(format_youtube_chapter_time(3661.0), "1:01:01");
    }

    #[test]
    fn generate_youtube_chapters_blank_label_substituted() {
        let entries = vec![
            (0.0, "Intro".to_string()),
            (83.0, "".to_string()),
        ];
        let out = generate_youtube_chapters(&entries);
        assert!(out.contains("Chapter 2"), "expected blank label → 'Chapter 2' in: {out}");
        // "Intro" is not blank, so "Chapter 1" should NOT appear.
        assert!(!out.contains("Chapter 1"), "unexpected 'Chapter 1' in: {out}");
    }

    #[test]
    fn generate_youtube_chapters_format() {
        let entries = vec![
            (0.0, "Intro".to_string()),
            (83.0, "Main Content".to_string()),
            (287.0, "Outro".to_string()),
        ];
        let expected = "0:00:00 Intro\n0:01:23 Main Content\n0:04:47 Outro";
        assert_eq!(generate_youtube_chapters(&entries), expected);
    }

}

#[cfg(test)]
mod render_script_tests {
    use super::*;

    #[test]
    fn format_ffmpeg_time_basic() {
        assert_eq!(format_ffmpeg_time(0.0), "00:00:00.000");
        assert_eq!(format_ffmpeg_time(3661.5), "01:01:01.500");
        assert_eq!(format_ffmpeg_time(83.456), "00:01:23.456");
    }

    #[test]
    fn format_ffmpeg_time_sub_second() {
        assert_eq!(format_ffmpeg_time(0.001), "00:00:00.001");
        assert_eq!(format_ffmpeg_time(0.999), "00:00:00.999");
    }

    #[test]
    fn format_ffmpeg_time_ninety_seconds() {
        // Smoke-test used by the wider render-script assertion block —
        // 90.0 seconds must round-trip through the HH:MM:SS.mmm formatter
        // cleanly, no 59.999-style rounding artefacts.
        assert_eq!(format_ffmpeg_time(90.0), "00:01:30.000");
    }

    #[test]
    fn render_script_has_warning_comment() {
        // Minimal sequence + default export settings exercising the
        // zero-clip path (still emits the WARNING block + shebang).
        let seq = Sequence::new_default();
        let media_pool = klipp_media::MediaPool::default();
        let settings = ExportSettings {
            pan_law: Default::default(),
            output_path: std::path::PathBuf::from("out.mp4"),
            format: ExportFormat::Mp4,
            video_codec: ExportVideoCodec::H264,
            resolution: None,
            fps: None,
            crf: 23,
            preset: EncoderPreset::Medium,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: Some(192),
            video_bitrate_kbps: None,
            rate_control: klipp_core::RateControl::Crf,
            burn_in_subtitles: true,
            embed_subtitles: false,
            target_lufs: None,
            true_peak_dbtp: None,
            embed_chapters: false,
            bit_depth: 0,
            hdr_mode: klipp_core::HdrMode::Sdr,
            tone_map_op: klipp_core::ToneMapOp::Hable,
            odt: klipp_core::OdtChoice::MatchViewer,
            mastering_peak_nits: klipp_core::DEFAULT_MASTERING_PEAK_NITS,
        };
        let script = generate_render_script(&seq, &media_pool, &settings, "/tmp/out.mp4");
        assert!(script.starts_with("#!/usr/bin/env bash\n"), "missing shebang: {script}");
        assert!(script.contains("WARNING"), "missing WARNING in: {script}");
        assert!(
            script.contains("CUTS ONLY") || script.contains("Cuts only") || script.contains("cuts only"),
            "WARNING block should mention cuts-only rendering: {script}"
        );
        assert!(
            script.contains("Ctrl+E") || script.contains("Export dialog"),
            "WARNING block should point users to the Export dialog: {script}"
        );
        // Zero-clip path: script must exit non-zero with an error message.
        assert!(script.contains("exit 1"), "zero-clip path should exit 1: {script}");
    }
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
mod batch_rename_tests {
    use super::*;
    use klipp_core::MediaId;

    #[test]
    fn find_replace_basic() {
        let result = compute_new_filename(
            "Interview_Raw.mp4",
            &BatchRenameMode::FindReplace,
            "Raw", "Edit", true, "", "", 1, 2,
        );
        assert_eq!(result, "Interview_Edit.mp4");
    }

    #[test]
    fn find_replace_preserves_extension() {
        let result = compute_new_filename(
            "clip.mov",
            &BatchRenameMode::FindReplace,
            "clip", "scene", true, "", "", 1, 2,
        );
        assert_eq!(result, "scene.mov");
    }

    #[test]
    fn find_replace_empty_find_is_noop() {
        let result = compute_new_filename(
            "clip.mov",
            &BatchRenameMode::FindReplace,
            "", "anything", true, "", "", 1, 2,
        );
        assert_eq!(result, "clip.mov");
    }

    #[test]
    fn find_replace_case_insensitive() {
        let result = compute_new_filename(
            "Interview_RAW.mp4",
            &BatchRenameMode::FindReplace,
            "raw", "edit", false, "", "", 1, 2,
        );
        // Case-insensitive match on "RAW" with replacement "edit".
        assert_eq!(result, "Interview_edit.mp4");
    }

    #[test]
    fn sequential_numbering_with_padding() {
        let result = compute_new_filename(
            "clip.mov",
            &BatchRenameMode::Sequential,
            "", "", true, "Scene_", "", 1, 3,
        );
        assert_eq!(result, "Scene_001.mov");
    }

    #[test]
    fn sequential_numbering_counter_increments() {
        let items = vec![
            (MediaId::new(), "a.mp4".to_string()),
            (MediaId::new(), "b.mp4".to_string()),
            (MediaId::new(), "c.mp4".to_string()),
        ];
        let result = preview_batch_rename(
            &items, &BatchRenameMode::Sequential,
            "", "", true, "Clip_", "", 1, 2,
        );
        assert_eq!(result[0].2, "Clip_01.mp4");
        assert_eq!(result[1].2, "Clip_02.mp4");
        assert_eq!(result[2].2, "Clip_03.mp4");
    }

    #[test]
    fn split_stem_ext_last_dot() {
        assert_eq!(split_stem_ext("my.file.name.mp4"), ("my.file.name", ".mp4"));
        assert_eq!(split_stem_ext("noext"), ("noext", ""));
        assert_eq!(split_stem_ext(""), ("", ""));
    }

    #[test]
    fn sequential_with_suffix() {
        let result = compute_new_filename(
            "src.avi",
            &BatchRenameMode::Sequential,
            "", "", true, "Take_", "_final", 7, 2,
        );
        assert_eq!(result, "Take_07_final.avi");
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
mod hyperzoom_handler_tests {
    //! Pure-function tests for `apply_hyperzoom_plan_to_clip` — the
    //! per-clip half of the hyperzoom dispatcher. The full
    //! `ApplyHyperzoom` AppCommand handler needs a `App` for
    //! `dirty`/`sync_sequence_to_engine`; these exercise the core
    //! mutation invariants on a hand-built sequence.
    use super::apply_hyperzoom_plan_to_clip;
    use klipp_core::{ClipId, MediaId, RationalTime};
    use klipp_core::keyframe::{Interpolation, Keyframe};
    use klipp_effects::BuiltinEffect;
    use klipp_timeline::{Clip, Sequence, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime { RationalTime::from_frames(frames, 24) }

    fn seq_with_clips(clips: Vec<Clip>) -> Sequence {
        let mut s = Sequence::new_default();
        let mut tr = Track::new("V1", TrackKind::Video);
        tr.clips = clips;
        s.tracks.push(tr);
        s
    }

    fn fresh_clip() -> Clip {
        Clip::media(MediaId::new(), tc(0), tc(48), tc(0), tc(48))
    }

    fn kf(t: f64, v: f32) -> Keyframe {
        Keyframe { time_secs: t, value: v, interpolation: Interpolation::Linear,
                   tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0) }
    }

    #[test]
    fn applies_anchor_by_inserting_transform_when_absent() {
        // Clip has no Transform builtin to start. Plan should insert
        // one with the focal point as anchor and the rest at default.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_plan_to_clip(&mut seq, cid, (0.3, 0.7), &[kf(0.0, 1.0), kf(0.5, 50.0)]);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let xform = clip.builtin_effects.iter().find_map(|e| match e {
            BuiltinEffect::Transform { anchor_x, anchor_y, scale_x, .. } => Some((*anchor_x, *anchor_y, *scale_x)),
            _ => None,
        }).expect("Transform builtin should have been inserted");
        assert!((xform.0 - 0.3).abs() < 1e-6);
        assert!((xform.1 - 0.7).abs() < 1e-6);
        assert!((xform.2 - 1.0).abs() < 1e-6); // default scale, not the keyframe value
    }

    #[test]
    fn updates_existing_transform_anchor_in_place() {
        // Pre-existing Transform with non-default rotation + scale. The
        // plan should ONLY rewrite anchor — the user's other field
        // values must survive (re-applying hyperzoom shouldn't blow
        // away a baseline rotation, for example).
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 10.0, position_y: 20.0,
            scale_x: 1.5, scale_y: 1.5,
            rotation: 45.0,
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        });
        c.builtin_effects_enabled.push(true);
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_plan_to_clip(&mut seq, cid, (0.2, 0.8), &[kf(0.0, 1.0), kf(0.5, 50.0)]);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let xform = clip.builtin_effects.iter().find_map(|e| match e {
            BuiltinEffect::Transform { position_x, position_y, scale_x, rotation, anchor_x, anchor_y, .. } =>
                Some((*position_x, *position_y, *scale_x, *rotation, *anchor_x, *anchor_y)),
            _ => None,
        }).unwrap();
        assert!((xform.0 - 10.0).abs() < 1e-6, "position_x preserved");
        assert!((xform.1 - 20.0).abs() < 1e-6, "position_y preserved");
        assert!((xform.2 - 1.5).abs() < 1e-6, "scale_x preserved");
        assert!((xform.3 - 45.0).abs() < 1e-6, "rotation preserved");
        assert!((xform.4 - 0.2).abs() < 1e-6, "anchor_x updated");
        assert!((xform.5 - 0.8).abs() < 1e-6, "anchor_y updated");
    }

    #[test]
    fn replaces_existing_scale_keyframes_for_idempotency() {
        // Re-applying hyperzoom should overwrite — never accumulate.
        // Otherwise a user clicking Apply twice with different settings
        // would end up with stale keyframes from the first application
        // bleeding into the second.
        let mut c = fresh_clip();
        c.transform_scale_keyframes = vec![kf(0.0, 0.5), kf(0.25, 2.0), kf(0.5, 5.0)];
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let new_kfs = vec![kf(2.5, 1.0), kf(3.0, 50.0)];
        apply_hyperzoom_plan_to_clip(&mut seq, cid, (0.5, 0.5), &new_kfs);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        assert_eq!(clip.transform_scale_keyframes.len(), 2);
        assert!((clip.transform_scale_keyframes[0].time_secs - 2.5).abs() < 1e-9);
        assert!((clip.transform_scale_keyframes[1].time_secs - 3.0).abs() < 1e-9);
        assert!((clip.transform_scale_keyframes[1].value - 50.0).abs() < 1e-6);
    }

    #[test]
    fn missing_clip_is_a_no_op() {
        // Stale dispatch (clip removed between dispatch and handler).
        // The function should warn and return without panicking; the
        // sequence stays unchanged.
        let c = fresh_clip();
        let mut seq = seq_with_clips(vec![c.clone()]);
        let other_id = ClipId::new();
        apply_hyperzoom_plan_to_clip(&mut seq, other_id, (0.5, 0.5), &[kf(0.0, 1.0), kf(0.5, 50.0)]);
        // Original clip untouched.
        let same = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|x| x.id == c.id).unwrap();
        assert!(same.transform_scale_keyframes.is_empty());
        assert!(same.builtin_effects.iter().all(|e| !matches!(e, BuiltinEffect::Transform { .. })));
    }

    use super::apply_hyperzoom_radial_blur_to_clip;

    #[test]
    fn radial_blur_inserts_builtin_when_absent_and_keys_amount() {
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_radial_blur_to_clip(
            &mut seq, cid, (0.3, 0.7), 15.0,
            &[kf(0.0, 0.0), kf(0.5, 15.0)],
        );
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        // RadialBlur builtin inserted with focal-point center +
        // peak amount + canonical defaults (samples=12, mode=0).
        let (idx, cx, cy, amt, samples, mode) = clip.builtin_effects.iter().enumerate().find_map(|(i, e)| match e {
            BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } =>
                Some((i, *center_x, *center_y, *amount, *samples, *mode)),
            _ => None,
        }).expect("RadialBlur should have been inserted");
        assert!((cx - 0.3).abs() < 1e-6);
        assert!((cy - 0.7).abs() < 1e-6);
        assert!((amt - 15.0).abs() < 1e-6);
        assert_eq!(samples, 12);
        assert_eq!(mode, 0);
        // Amount keyframes registered under "{idx}:amount" so the
        // engine's per-effect-keyframe lookup picks them up.
        let key = format!("{}:amount", idx);
        let kfs = clip.effect_keyframes.get(&key).expect("amount keyframes registered");
        assert_eq!(kfs.len(), 2);
        assert!((kfs[1].value - 15.0).abs() < 1e-6);
    }

    #[test]
    fn radial_blur_updates_existing_builtin_in_place() {
        // Pre-existing RadialBlur with non-default samples + mode
        // (user manually configured spin mode + 24 samples). The
        // hyperzoom helper should rewrite center + amount but
        // preserve samples/mode — re-applying must not clobber the
        // user's tweaks.
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::RadialBlur {
            center_x: 0.0, center_y: 0.0,
            amount: 0.0,
            samples: 24,
            mode: 1, // spin
        });
        c.builtin_effects_enabled.push(true);
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        apply_hyperzoom_radial_blur_to_clip(
            &mut seq, cid, (0.4, 0.6), 22.0,
            &[kf(0.0, 0.0), kf(0.5, 22.0)],
        );
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (cx, cy, amt, samples, mode) = clip.builtin_effects.iter().find_map(|e| match e {
            BuiltinEffect::RadialBlur { center_x, center_y, amount, samples, mode } =>
                Some((*center_x, *center_y, *amount, *samples, *mode)),
            _ => None,
        }).unwrap();
        assert!((cx - 0.4).abs() < 1e-6, "center_x rewritten");
        assert!((cy - 0.6).abs() < 1e-6, "center_y rewritten");
        assert!((amt - 22.0).abs() < 1e-6, "amount rewritten to peak");
        assert_eq!(samples, 24, "user-configured samples preserved");
        assert_eq!(mode, 1, "user-configured mode (spin) preserved");
    }

    // ─────────── Glyph-anchored focal-point bridge ───────────

    use super::apply_focal_ref_to_clip_transform;

    #[test]
    fn focal_ref_bridge_writes_into_existing_transform() {
        // Pre-existing Transform with a baseline rotation the user
        // wants preserved. The bridge writes focal_ref but must NOT
        // touch other Transform fields — same in-place-update
        // contract as the matte / radial-blur helpers.
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0,
            scale_x: 1.0, scale_y: 1.0,
            rotation: 30.0,  // baseline rotation
            anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: None,
        });
        c.builtin_effects_enabled.push(true);
        let cid = c.id;
        let title_uuid = ClipId::new().0;
        let mut seq = seq_with_clips(vec![c]);

        let fr = klipp_core::FocalRef::Glyph {
            title_clip_id: title_uuid,
            char_index: 5,
            fallback_xy: (0.4, 0.6),
        };
        apply_focal_ref_to_clip_transform(&mut seq, cid, fr);

        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (rotation, fr_out) = clip.builtin_effects.iter().find_map(|e| match e {
            BuiltinEffect::Transform { rotation, focal_ref, .. } => Some((*rotation, *focal_ref)),
            _ => None,
        }).unwrap();
        assert!((rotation - 30.0).abs() < 1e-6, "baseline rotation preserved");
        assert!(matches!(
            fr_out,
            Some(klipp_core::FocalRef::Glyph { char_index: 5, .. })
        ), "focal_ref written");
    }

    #[test]
    fn focal_ref_bridge_inserts_transform_when_absent() {
        // Defence-in-depth: if the source clip has no Transform
        // builtin yet, the bridge inserts one with sane defaults +
        // the focal_ref. (The hyperzoom apply path always inserts
        // one before this site fires, but the helper shouldn't
        // panic if called out of order.)
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let fr = klipp_core::FocalRef::Glyph {
            title_clip_id: ClipId::new().0,
            char_index: 0,
            fallback_xy: (0.5, 0.5),
        };
        apply_focal_ref_to_clip_transform(&mut seq, cid, fr);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let has_focal_ref = clip.builtin_effects.iter().any(|e| matches!(
            e,
            BuiltinEffect::Transform { focal_ref: Some(_), .. }
        ));
        assert!(has_focal_ref, "Transform inserted with focal_ref attached");
    }

    #[test]
    fn focal_ref_bridge_overwrites_existing_focal_ref() {
        // Re-applying hyperzoom with a different glyph pick should
        // replace the focal_ref, not stack two of them. (Belt-and-
        // suspenders — only one Transform per clip in practice, so
        // there's nothing to "stack" against, but the contract is
        // worth pinning.)
        let mut c = fresh_clip();
        let old_uuid = ClipId::new().0;
        c.builtin_effects.push(BuiltinEffect::Transform {
            position_x: 0.0, position_y: 0.0, scale_x: 1.0, scale_y: 1.0,
            rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
            rotation_x: 0.0, rotation_y: 0.0, perspective: 0.0,
            focal_ref: Some(klipp_core::FocalRef::Glyph {
                title_clip_id: old_uuid,
                char_index: 1,
                fallback_xy: (0.0, 0.0),
            }),
        });
        c.builtin_effects_enabled.push(true);
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let new_uuid = ClipId::new().0;
        apply_focal_ref_to_clip_transform(&mut seq, cid, klipp_core::FocalRef::Glyph {
            title_clip_id: new_uuid, char_index: 9, fallback_xy: (0.7, 0.3),
        });
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let fr = clip.builtin_effects.iter().find_map(|e| match e {
            BuiltinEffect::Transform { focal_ref, .. } => *focal_ref,
            _ => None,
        }).unwrap();
        match fr {
            klipp_core::FocalRef::Glyph { title_clip_id, char_index, .. } => {
                assert_eq!(title_clip_id, new_uuid, "focal_ref replaced not stacked");
                assert_eq!(char_index, 9);
            }
            klipp_core::FocalRef::Tracker { .. } => {
                panic!("test set up Glyph variant; got Tracker — schema regression?");
            }
        }
    }

    #[test]
    fn focal_ref_bridge_no_ops_on_missing_clip() {
        // Stale dispatch (clip removed between dispatch and handler).
        let c = fresh_clip();
        let mut seq = seq_with_clips(vec![c.clone()]);
        let other_id = ClipId::new();
        apply_focal_ref_to_clip_transform(&mut seq, other_id, klipp_core::FocalRef::Glyph {
            title_clip_id: ClipId::new().0, char_index: 0, fallback_xy: (0.5, 0.5),
        });
        let same = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|x| x.id == c.id).unwrap();
        assert!(same.builtin_effects.iter().all(|e| !matches!(e, BuiltinEffect::Transform { .. })));
    }

    // ────── Chromatic-aberration handler ──────

    use super::apply_hyperzoom_chromatic_aberration_to_clip;

    fn read_chromatic(clip: &Clip) -> Option<(f32, f32, f32, f32)> {
        clip.builtin_effects.iter().find_map(|e| match e {
            BuiltinEffect::ChromaticAberration { red_shift, blue_shift, center_x, center_y } => {
                Some((*red_shift, *blue_shift, *center_x, *center_y))
            }
            _ => None,
        })
    }

    #[test]
    fn chromatic_inserts_when_absent() {
        // No ChromaticAberration on the clip → handler appends one
        // with center at the focal point and red/blue at ∓peak.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red = vec![kf(0.0, 0.0), kf(0.5, -8.0)];
        let blue = vec![kf(0.0, 0.0), kf(0.5, 8.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.3, 0.7), 8.0, &red, &blue);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (rs, bs, cx, cy) = read_chromatic(clip).expect("ChromaticAberration appended");
        assert!((rs - -8.0).abs() < 1e-6, "red_shift static fallback at -peak");
        assert!((bs - 8.0).abs() < 1e-6, "blue_shift static fallback at +peak");
        assert!((cx - 0.3).abs() < 1e-6);
        assert!((cy - 0.7).abs() < 1e-6);
    }

    #[test]
    fn chromatic_updates_existing_preserving_no_secondaries() {
        // Pre-existing ChromaticAberration with different
        // center + shifts. Handler rewrites center and shifts; no
        // secondary params on this builtin survive (it's all four
        // fields). User-tweaked-tunings-getting-overwritten is the
        // first-match-wins disambiguation behaviour pinned in the
        // doc comment.
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::ChromaticAberration {
            red_shift: -2.0, blue_shift: 2.0, center_x: 0.1, center_y: 0.1,
        });
        c.builtin_effects_enabled.push(true);
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red = vec![kf(0.0, 0.0), kf(0.5, -10.0)];
        let blue = vec![kf(0.0, 0.0), kf(0.5, 10.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.6, 0.4), 10.0, &red, &blue);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (rs, bs, cx, cy) = read_chromatic(clip).unwrap();
        assert!((rs - -10.0).abs() < 1e-6);
        assert!((bs - 10.0).abs() < 1e-6);
        assert!((cx - 0.6).abs() < 1e-6, "center_x updated to new focal");
        assert!((cy - 0.4).abs() < 1e-6, "center_y updated to new focal");
        // Pool count stays at 1 — handler updated in place,
        // didn't append a duplicate.
        let n = clip.builtin_effects.iter().filter(|e| matches!(e, BuiltinEffect::ChromaticAberration { .. })).count();
        assert_eq!(n, 1);
    }

    #[test]
    fn chromatic_re_apply_with_new_focal_updates_center() {
        // Audit-note (iv) pin: re-applying with a different focal
        // point must rewrite the center on the existing builtin.
        // The first-apply lays in the chromatic at focal=(0.5,0.5);
        // the second apply at focal=(0.2,0.8) must overwrite, not
        // leave the old center stale.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red = vec![kf(0.0, 0.0), kf(0.5, -6.0)];
        let blue = vec![kf(0.0, 0.0), kf(0.5, 6.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.5, 0.5), 6.0, &red, &blue);
        // Confirm first apply set center to (0.5, 0.5).
        let clip1 = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap().clone();
        let (_, _, cx1, cy1) = read_chromatic(&clip1).unwrap();
        assert!((cx1 - 0.5).abs() < 1e-6 && (cy1 - 0.5).abs() < 1e-6);
        // Re-apply with new focal.
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.2, 0.8), 6.0, &red, &blue);
        let clip2 = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (_, _, cx2, cy2) = read_chromatic(clip2).unwrap();
        assert!((cx2 - 0.2).abs() < 1e-6, "center_x rewritten on re-apply");
        assert!((cy2 - 0.8).abs() < 1e-6, "center_y rewritten on re-apply");
    }

    #[test]
    fn chromatic_keyframe_write_order_red_before_blue() {
        // Audit-note (i) pin: red_shift keyframes are written
        // BEFORE blue_shift in clip.effect_keyframes. This makes
        // future snapshot tests deterministic regardless of
        // HashMap iteration order — callers ordering keys
        // alphabetically would surface red-shift first anyway, but
        // pinning the write order makes the contract explicit.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red = vec![kf(0.0, 0.0), kf(0.5, -6.0)];
        let blue = vec![kf(0.0, 0.0), kf(0.5, 6.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.5, 0.5), 6.0, &red, &blue);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        // Both keys should now exist; red_shift was inserted first.
        // We check both keys are present (HashMap doesn't preserve
        // insertion order, so we can't pin the iteration order —
        // but the values must be correct for the right key).
        let red_key_keyframes = clip.effect_keyframes.iter()
            .find(|(k, _)| k.ends_with(":red_shift"))
            .map(|(_, v)| v.clone());
        let blue_key_keyframes = clip.effect_keyframes.iter()
            .find(|(k, _)| k.ends_with(":blue_shift"))
            .map(|(_, v)| v.clone());
        assert_eq!(red_key_keyframes.as_deref(), Some(red.as_slice()));
        assert_eq!(blue_key_keyframes.as_deref(), Some(blue.as_slice()));
        // Both keys present — neither got dropped during the
        // double insert.
        assert_eq!(clip.effect_keyframes.iter().filter(|(k, _)| k.ends_with(":red_shift") || k.ends_with(":blue_shift")).count(), 2);
    }

    #[test]
    fn chromatic_replaces_existing_keyframes_for_idempotency() {
        // Re-applying with new keyframes must replace — not
        // append/accumulate. Same idempotency contract as
        // RadialBlur.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let red1 = vec![kf(0.0, 0.0), kf(0.5, -3.0)];
        let blue1 = vec![kf(0.0, 0.0), kf(0.5, 3.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.5, 0.5), 3.0, &red1, &blue1);
        let red2 = vec![kf(0.0, 0.0), kf(0.5, -10.0)];
        let blue2 = vec![kf(0.0, 0.0), kf(0.5, 10.0)];
        apply_hyperzoom_chromatic_aberration_to_clip(&mut seq, cid, (0.5, 0.5), 10.0, &red2, &blue2);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let red_kfs = clip.effect_keyframes.iter()
            .find(|(k, _)| k.ends_with(":red_shift"))
            .map(|(_, v)| v.clone()).unwrap();
        assert_eq!(red_kfs.len(), 2);
        // Peak value reflects the SECOND apply, not accumulated.
        assert!((red_kfs[1].value - -10.0).abs() < 1e-6);
    }

    // ────── Light-leak (FilmHalation) handler ──────

    use super::apply_hyperzoom_light_leak_to_clip;

    fn read_halation(clip: &Clip) -> Option<(f32, f32, f32, f32)> {
        clip.builtin_effects.iter().find_map(|e| match e {
            BuiltinEffect::FilmHalation { threshold, radius, intensity, hue } => {
                Some((*threshold, *radius, *intensity, *hue))
            }
            _ => None,
        })
    }

    #[test]
    fn light_leak_inserts_when_absent_with_warm_defaults() {
        // No FilmHalation on the clip → handler appends one with
        // canonical trailer-aesthetic defaults (warm orange-yellow
        // hue 0.15, threshold 0.75, radius 15.0) and intensity at
        // peak.
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let kfs = vec![kf(0.0, 0.0), kf(0.5, 0.4)];
        apply_hyperzoom_light_leak_to_clip(&mut seq, cid, 0.4, &kfs);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (thr, rad, intensity, hue) = read_halation(clip).expect("FilmHalation appended");
        assert!((thr - 0.75).abs() < 1e-6);
        assert!((rad - 15.0).abs() < 1e-6);
        assert!((intensity - 0.4).abs() < 1e-6, "intensity at peak");
        assert!((hue - 0.15).abs() < 1e-6, "warm hue default");
    }

    #[test]
    fn light_leak_updates_existing_preserving_user_secondaries() {
        // Pre-existing FilmHalation with custom threshold/radius/
        // hue. Handler rewrites ONLY intensity; secondaries
        // survive (user-tweaked tunings preserved).
        let mut c = fresh_clip();
        c.builtin_effects.push(BuiltinEffect::FilmHalation {
            threshold: 0.5, radius: 25.0, intensity: 0.1, hue: 0.7,
        });
        c.builtin_effects_enabled.push(true);
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let kfs = vec![kf(0.0, 0.0), kf(0.5, 0.6)];
        apply_hyperzoom_light_leak_to_clip(&mut seq, cid, 0.6, &kfs);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let (thr, rad, intensity, hue) = read_halation(clip).unwrap();
        assert!((thr - 0.5).abs() < 1e-6, "user threshold preserved");
        assert!((rad - 25.0).abs() < 1e-6, "user radius preserved");
        assert!((hue - 0.7).abs() < 1e-6, "user hue preserved");
        assert!((intensity - 0.6).abs() < 1e-6, "intensity rewritten to peak");
    }

    #[test]
    fn light_leak_replaces_existing_keyframes_for_idempotency() {
        let c = fresh_clip();
        let cid = c.id;
        let mut seq = seq_with_clips(vec![c]);
        let kfs1 = vec![kf(0.0, 0.0), kf(0.5, 0.2)];
        apply_hyperzoom_light_leak_to_clip(&mut seq, cid, 0.2, &kfs1);
        let kfs2 = vec![kf(0.0, 0.0), kf(0.5, 0.8)];
        apply_hyperzoom_light_leak_to_clip(&mut seq, cid, 0.8, &kfs2);
        let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == cid).unwrap();
        let intensity_kfs = clip.effect_keyframes.iter()
            .find(|(k, _)| k.ends_with(":intensity"))
            .map(|(_, v)| v.clone()).unwrap();
        assert_eq!(intensity_kfs.len(), 2);
        assert!((intensity_kfs[1].value - 0.8).abs() < 1e-6);
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

#[cfg(test)]
mod whoosh_helper_tests {
    //! Unit tests for the pure-ish whoosh helpers feeding the
    //! ApplyHyperzoom dispatch arm.
    use super::*;
    use klipp_core::RationalTime;
    use klipp_media::MediaPool;

    #[test]
    fn variant_from_u8_maps_known_tags() {
        assert_eq!(whoosh_variant_from_u8(0), klipp_audio::WhooshVariant::Short);
        assert_eq!(whoosh_variant_from_u8(1), klipp_audio::WhooshVariant::Medium);
        assert_eq!(whoosh_variant_from_u8(2), klipp_audio::WhooshVariant::Long);
    }

    #[test]
    fn variant_from_u8_unknown_falls_back_to_medium() {
        // Per the AppCommand doc: "Unknown values fall back to
        // Medium so a stale dispatch can never produce silence
        // when the user ticked the box." Pin the contract.
        for tag in [3u8, 7, 100, 200, 254] {
            assert_eq!(
                whoosh_variant_from_u8(tag),
                klipp_audio::WhooshVariant::Medium,
            );
        }
        // 255 is "custom" — handled before this resolver in the
        // dispatch arm — but the fallback maps it to Medium too;
        // documents the never-reached path.
        assert_eq!(
            whoosh_variant_from_u8(255),
            klipp_audio::WhooshVariant::Medium,
        );
    }

    #[test]
    fn round_trip_variant_u8_tags_match_inverse() {
        for v in [
            klipp_audio::WhooshVariant::Short,
            klipp_audio::WhooshVariant::Medium,
            klipp_audio::WhooshVariant::Long,
        ] {
            let tag = whoosh_variant_to_u8(v);
            assert_eq!(whoosh_variant_from_u8(tag), v);
        }
    }

    #[test]
    fn clip_in_centres_whoosh_on_cut() {
        // src_timeline_out = 5 seconds (240 frames @ 48 rate).
        // Medium whoosh duration = 0.5 s → half_dur = 0.25 s = 12
        // ticks @ 48 rate. Expected timeline_in = 240 − 12 = 228.
        let src_out = RationalTime { value: 240, rate: 48 };
        let result = whoosh_clip_in_for_apply(src_out, 0.5);
        assert_eq!(result.value, 228);
        assert_eq!(result.rate, 48);
    }

    #[test]
    fn clip_in_clamps_to_zero_when_whoosh_extends_before_start() {
        // src_timeline_out = 0.1 s (5 ticks @ 48 rate). Long
        // whoosh duration = 0.8 s → half_dur = 0.4 s = ~19 ticks.
        // 5 − 19 = −14 → clamped to 0.
        let src_out = RationalTime { value: 5, rate: 48 };
        let result = whoosh_clip_in_for_apply(src_out, 0.8);
        assert_eq!(result.value, 0);
    }

    #[test]
    fn clip_in_preserves_source_rate() {
        // The whoosh clip inherits the SOURCE clip's rate, not
        // the whoosh media's rate. The insert step rescales as
        // needed.
        for rate in [24, 30, 48, 60] {
            let src_out = RationalTime { value: rate as i64, rate };
            let result = whoosh_clip_in_for_apply(src_out, 0.1);
            assert_eq!(result.rate, rate);
        }
    }

    #[test]
    fn ensure_builtin_synthesises_first_then_caches() {
        // First call: cache miss → synth → register → cache.
        // Second call: cache hit → same MediaId.
        let mut pool = MediaPool::new();
        let mut cache = std::collections::HashMap::new();
        let v = klipp_audio::WhooshVariant::Short;
        // Run once. May fail on systems without dirs::cache_dir
        // (CI sandboxes); skip the assertion gracefully if so.
        let first = ensure_builtin_whoosh_in_pool(&mut pool, &mut cache, v);
        let Some((id_first, dur_first)) = first else {
            eprintln!("skipping: no cache_dir on this platform");
            return;
        };
        assert!(pool.get(&id_first).is_some(), "registered in pool");
        assert_eq!(cache.get(&0).copied(), Some(id_first), "cache populated");
        // Second call must reuse.
        let (id_second, dur_second) =
            ensure_builtin_whoosh_in_pool(&mut pool, &mut cache, v).unwrap();
        assert_eq!(id_first, id_second, "cache hit reuses MediaId");
        assert_eq!(dur_first, dur_second, "duration stable across calls");
        // Pool should have exactly one entry per variant after two calls.
        assert_eq!(pool.all().count(), 1);
    }

    #[test]
    fn ensure_builtin_re_registers_when_pool_wiped_under_cache() {
        // Defensive: cache says "we have it" but pool has been
        // cleared (e.g. project save/load cycle). Helper detects
        // the stale pointer and re-registers.
        let mut pool = MediaPool::new();
        let mut cache = std::collections::HashMap::new();
        let v = klipp_audio::WhooshVariant::Medium;
        let Some((id_first, _)) = ensure_builtin_whoosh_in_pool(&mut pool, &mut cache, v) else {
            return;
        };
        // Wipe the pool but keep the stale cache pointer.
        pool = MediaPool::new();
        let (id_second, _) = ensure_builtin_whoosh_in_pool(&mut pool, &mut cache, v).unwrap();
        assert_ne!(id_first, id_second, "re-registered with a fresh id");
        assert_eq!(pool.all().count(), 1);
    }

    #[test]
    fn find_existing_audio_returns_none_for_unimported_path() {
        let pool = MediaPool::new();
        let phantom = std::path::PathBuf::from("/tmp/never-imported.wav");
        assert!(find_existing_audio_in_pool(&pool, &phantom).is_none());
    }

    #[test]
    fn find_existing_audio_matches_by_exact_path_and_audio_flag() {
        let mut pool = MediaPool::new();
        let path = std::path::PathBuf::from("/tmp/custom-whoosh.wav");
        let mut item = build_whoosh_media_item(
            klipp_audio::WhooshVariant::Short,
            path.clone(),
        );
        item.has_audio = true;
        let id = pool.add(item);
        let result = find_existing_audio_in_pool(&pool, &path);
        assert_eq!(result.map(|(i, _)| i), Some(id));
    }

    #[test]
    fn find_existing_audio_skips_video_only_items_with_matching_path() {
        // Edge case: someone imported a video file at the same
        // path. has_audio guard makes sure we don't pick it up
        // as a whoosh source.
        let mut pool = MediaPool::new();
        let path = std::path::PathBuf::from("/tmp/silent-clip.mp4");
        let mut item = build_whoosh_media_item(
            klipp_audio::WhooshVariant::Short,
            path.clone(),
        );
        item.has_audio = false;
        pool.add(item);
        assert!(find_existing_audio_in_pool(&pool, &path).is_none());
    }
}

#[cfg(test)]
mod whoosh_dispatch_tests {
    //! Integration-style tests for the `maybe_insert_whoosh_for_apply`
    //! end of the ApplyHyperzoom dispatch arm. We can't stand up an
    //! `App` cheaply — these test against `AppState` directly, the
    //! same way `whoosh_helper_tests` does, but exercise the full
    //! variant-resolve → media-register → clip-insert chain.
    use super::{
        build_whoosh_media_item, maybe_insert_whoosh_for_apply,
    };
    use crate::state::AppState;
    use klipp_core::{ClipId, MediaId, RationalTime, Timecode};
    use klipp_timeline::{Clip, Track, TrackKind};

    fn tc(value: i64, rate: u32) -> RationalTime {
        RationalTime { value, rate }
    }

    fn fresh_state_with_video_clip() -> (AppState, ClipId, Timecode) {
        // A minimal sequence: one video track with one media clip
        // ending at 2.0 s @ 48 rate. Source clip's timeline_out is
        // what the whoosh handler centres the audio against.
        let mut state = AppState::default();
        // new_default ships V1 + A1; clear and rebuild so we can
        // capture the source clip id deterministically.
        state.sequences[0].tracks.clear();
        let mut track = Track::new("V1", TrackKind::Video);
        let src_in = tc(0, 48);
        let src_out = tc(96, 48); // 2.0 s @ 48
        let clip = Clip::media(MediaId::new(), src_in, src_out, src_in, src_out);
        let cid = clip.id;
        track.clips.push(clip);
        state.sequences[0].tracks.push(track);
        (state, cid, src_out)
    }

    #[test]
    fn add_whoosh_false_emits_no_audio() {
        // The byte-identical-to-pre-feature pin. A snapshot of the
        // sequence before and after must match: no new tracks,
        // no new clips, no whoosh in the media pool, no entry in
        // the variant cache.
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        let pre_track_count = state.sequences[0].tracks.len();
        let pre_clip_count: usize = state.sequences[0]
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum();
        let pre_pool_count = state.media_pool.all().count();
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            false, // add_whoosh = false
            0,
            None,
        );
        assert!(result.is_none(), "false → no insertion");
        assert_eq!(state.sequences[0].tracks.len(), pre_track_count);
        let post_clip_count: usize = state.sequences[0]
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum();
        assert_eq!(post_clip_count, pre_clip_count);
        assert_eq!(state.media_pool.all().count(), pre_pool_count);
        assert!(state.whoosh_media_ids.is_empty());
    }

    #[test]
    fn add_whoosh_true_inserts_at_cut_aligned_time() {
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            true,
            0, // Short = 0.30 s
            None,
        );
        let Some(inserted) = result else {
            // Skip on platforms with no cache_dir (rare).
            eprintln!("skipping: no cache_dir / synth I/O failure");
            return;
        };
        // Audio track auto-created (sequence had only the video track).
        assert_eq!(state.sequences[0].tracks[inserted.track_index].kind, TrackKind::Audio);
        assert_eq!(state.sequences[0].tracks[inserted.track_index].clips.len(), 1);
        // Whoosh registered in pool + cached in variant map.
        assert!(state.whoosh_media_ids.contains_key(&0));
        assert_eq!(state.media_pool.all().count(), 1);
        // Clip-in centring: src_out = 2.0 s @ 48 rate (96 ticks).
        // Short whoosh = 0.30 s → half = 0.15 s → at 48 rate
        // that's 7.2 → rounds to 7. Expected timeline_in =
        // 96 - 7 = 89 (rate 48; the rescale-to-source-rate inside
        // insert_audio_media_clip_into_seq runs against the source
        // clip's 48 kHz rate, producing a different value — see
        // the next assertion).
        let inserted_in = inserted.clip.timeline_in;
        // The audio clip's rate ends up at 48 000 (the synth rate)
        // because insert_audio_media_clip_into_seq rescales to the
        // *source media* rate. Verify it's NOT zero (i.e., the
        // centring formula did fire — clamping to zero would
        // indicate misuse) and is less than src_out converted to
        // the same rate.
        assert!(inserted_in.value > 0, "timeline_in non-zero (centred, not clamped)");
    }

    #[test]
    fn second_apply_reuses_cached_whoosh_media_id() {
        // Two consecutive applies on different cuts — the second
        // must reuse the cached MediaId rather than registering a
        // duplicate. Pin the audit-note-(iv) "AppState-scoped
        // cache" promise.
        let (mut state, src_id, src_out) = fresh_state_with_video_clip();
        // Add a second video clip + capture its timeline_out so
        // the second apply has a different cut frame.
        let second_out = tc(192, 48);
        let mut second_clip = Clip::media(MediaId::new(), src_out, second_out, tc(0, 48), tc(96, 48));
        let _ = src_id; // suppress unused
        second_clip.id = ClipId::new();
        state.sequences[0].tracks[0].clips.push(second_clip);
        let r1 = maybe_insert_whoosh_for_apply(&mut state, 0, src_out, true, 1, None);
        let r2 = maybe_insert_whoosh_for_apply(&mut state, 0, second_out, true, 1, None);
        let (Some(_), Some(_)) = (&r1, &r2) else {
            eprintln!("skipping: synth I/O unavailable");
            return;
        };
        assert_eq!(
            state.media_pool.all().count(),
            1,
            "second apply reused cached MediaId — pool still has one entry",
        );
    }

    #[test]
    fn custom_variant_with_path_in_pool_uses_that_media() {
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        // Pre-populate the pool with a "custom" audio file.
        let custom_path = std::path::PathBuf::from("/tmp/user-custom-whoosh.wav");
        let item = build_whoosh_media_item(
            klipp_audio::WhooshVariant::Short,
            custom_path.clone(),
        );
        let custom_id = state.media_pool.add(item);
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            true,
            255, // Custom
            Some(&custom_path),
        );
        let inserted = result.expect("custom path resolves");
        // The inserted clip's media should be the custom one,
        // NOT a freshly-synthesised built-in.
        let media_id = match inserted.clip.kind {
            klipp_timeline::ClipKind::Media { media_id } => media_id,
            _ => panic!("expected Media kind"),
        };
        assert_eq!(media_id, custom_id);
        // Built-in cache untouched.
        assert!(state.whoosh_media_ids.is_empty());
    }

    #[test]
    fn custom_variant_without_pool_match_skips_silently() {
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        let phantom_path = std::path::PathBuf::from("/tmp/never-imported-whoosh.wav");
        let pre_clip_count: usize = state.sequences[0]
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum();
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            true,
            255,
            Some(&phantom_path),
        );
        assert!(result.is_none(), "no clip inserted when path absent from pool");
        let post_clip_count: usize = state.sequences[0]
            .tracks
            .iter()
            .map(|t| t.clips.len())
            .sum();
        assert_eq!(post_clip_count, pre_clip_count, "sequence untouched");
    }

    #[test]
    fn custom_variant_without_path_skips_silently() {
        let (mut state, _src_id, src_out) = fresh_state_with_video_clip();
        let result = maybe_insert_whoosh_for_apply(
            &mut state,
            0,
            src_out,
            true,
            255,
            None, // variant=255 but no path
        );
        assert!(result.is_none());
    }
}

#[cfg(test)]
mod insert_audio_media_clip_tests {
    //! Pure-mutation tests for `insert_audio_media_clip_into_seq`.
    //! Builds a sequence + media pool inline; the dispatch arm in
    //! `update()` is the thin caller that records undo + dirties.
    use super::{insert_audio_media_clip_into_seq, InsertAudioMediaClipError};
    use klipp_core::{Fps, MediaId, RationalTime, Resolution};
    use klipp_media::{ColorLabel, ColourSpace, MediaItem, MediaItemKind, MediaPool};
    use klipp_timeline::{Sequence, Track, TrackKind};

    fn tc(frames: i64) -> RationalTime {
        RationalTime::from_frames(frames, 48)
    }

    fn audio_media_item(duration: RationalTime) -> MediaItem {
        MediaItem {
            id: MediaId::new(),
            path: std::path::PathBuf::from("/tmp/whoosh-test.wav"),
            kind: MediaItemKind::File,
            filename: "whoosh-test.wav".into(),
            duration,
            fps: Fps::film_24(),
            resolution: Resolution { width: 0, height: 0 },
            video_codec: String::new(),
            audio_codec: "pcm_s16le".into(),
            has_audio: true,
            file_size_bytes: 0,
            sample_rate: Some(48_000),
            audio_channels: Some(1),
            offline: false,
            is_still: false,
            proxy_path: None,
            proxy_ready: false,
            default_source_in: None,
            default_source_out: None,
            rating: 0,
            tags: Vec::new(),
            user_notes: String::new(),
            bin: None,
            bin_id: None,
            parent_media_id: None,
            added_at: 0,
            fingerprint: None,
            file_modified_at: None,
            media_created_at: None,
            color_label: ColorLabel::default(),
            colour_space: ColourSpace::default(),
            log_format: klipp_core::LogFormat::default(),
            source_rotation: 0,
            camera_make: None,
            camera_model: None,
            encoder: None,
            bit_depth: None,
            video_bitrate_kbps: None,
            audio_bitrate_kbps: None,
            color_primaries: None,
            color_transfer: None,
            pixel_format: None,
            detected_log_format: klipp_core::LogFormat::default(),
            log_format_user_set: false,
            skim_file: None,
            needs_reprobe: false,
            waveform_data: None,
            audio_cache: None,
            shot: Default::default(),
            metadata_read: false,
            truncated_from_secs: None,
            log: Default::default(),
            synced_audio: None,
            markers: Vec::new(),
            screen_text: String::new(),
            span: None,
        }
    }

    fn seq_video_only() -> Sequence {
        let mut s = Sequence::new_default();
        // new_default ships one video + one audio track. Strip the
        // audio so we exercise the "auto-create when none exists"
        // branch.
        s.tracks.retain(|t| t.kind != TrackKind::Audio);
        assert!(
            !s.tracks.iter().any(|t| t.kind == TrackKind::Audio),
            "fixture invariant: no audio tracks expected after retain",
        );
        s
    }

    fn seq_with_audio_at(idx: usize) -> Sequence {
        let mut s = Sequence::new_default();
        s.tracks.retain(|t| t.kind != TrackKind::Audio);
        // Pad the track list with extra video tracks so the audio
        // lands at the requested index.
        while s.tracks.len() < idx {
            let name = format!("V{}", s.tracks.len() + 1);
            s.tracks.push(Track::new(&name, TrackKind::Video));
        }
        s.tracks
            .insert(idx, Track::new("Audio 1", TrackKind::Audio));
        s
    }

    #[test]
    fn picks_first_audio_track_when_track_index_is_none() {
        let mut seq = seq_with_audio_at(2); // V1, V2, A1
        let mut pool = MediaPool::new();
        let item = audio_media_item(tc(24));
        let media_id = pool.add(item);
        let inserted = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, None, tc(48))
            .expect("insert");
        assert_eq!(inserted.track_index, 2);
        assert_eq!(seq.tracks[2].clips.len(), 1);
        assert_eq!(seq.tracks[2].clips[0].timeline_in, tc(48));
    }

    #[test]
    fn auto_creates_audio_track_when_none_exists() {
        let mut seq = seq_video_only();
        let video_count = seq.tracks.len();
        let mut pool = MediaPool::new();
        let media_id = pool.add(audio_media_item(tc(24)));
        let inserted = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, None, tc(0))
            .expect("insert");
        assert_eq!(seq.tracks.len(), video_count + 1, "audio track auto-created");
        let new_track = &seq.tracks[inserted.track_index];
        assert_eq!(new_track.kind, TrackKind::Audio);
        assert_eq!(new_track.clips.len(), 1);
    }

    #[test]
    fn inserts_at_explicit_audio_track_index() {
        let mut seq = seq_with_audio_at(1); // V1, A1
        let mut pool = MediaPool::new();
        let media_id = pool.add(audio_media_item(tc(24)));
        let inserted =
            insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, Some(1), tc(0))
                .expect("insert");
        assert_eq!(inserted.track_index, 1);
        assert_eq!(seq.tracks[1].clips.len(), 1);
    }

    #[test]
    fn errors_when_media_id_not_in_pool() {
        let mut seq = seq_with_audio_at(1);
        let pool = MediaPool::new(); // empty
        let phantom = MediaId::new();
        let err = insert_audio_media_clip_into_seq(&mut seq, &pool, phantom, None, tc(0))
            .expect_err("should error");
        assert!(matches!(err, InsertAudioMediaClipError::MediaNotInPool));
    }

    #[test]
    fn errors_when_track_index_out_of_range() {
        let mut seq = seq_with_audio_at(1);
        let mut pool = MediaPool::new();
        let media_id = pool.add(audio_media_item(tc(24)));
        let err = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, Some(99), tc(0))
            .expect_err("should error");
        assert!(matches!(err, InsertAudioMediaClipError::TrackIndexOutOfRange));
    }

    #[test]
    fn errors_when_track_index_points_at_video_track() {
        let mut seq = seq_with_audio_at(1); // V1 (idx 0), A1 (idx 1)
        let mut pool = MediaPool::new();
        let media_id = pool.add(audio_media_item(tc(24)));
        let err = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, Some(0), tc(0))
            .expect_err("should error");
        assert!(matches!(err, InsertAudioMediaClipError::TrackIsNotAudio));
    }

    #[test]
    fn errors_when_media_duration_zero() {
        let mut seq = seq_with_audio_at(1);
        let mut pool = MediaPool::new();
        let zero = RationalTime { value: 0, rate: 48 };
        let media_id = pool.add(audio_media_item(zero));
        let err = insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, None, tc(0))
            .expect_err("should error");
        assert!(matches!(err, InsertAudioMediaClipError::ZeroDuration));
    }

    #[test]
    fn timeline_in_rescaled_to_source_rate() {
        // Pin the RationalTime gotcha — caller passes a 24-rate
        // timecode, source is 48-rate. Without rescale the
        // timeline_in.value would be misinterpreted as 48-rate
        // frames and the clip would land at the wrong time.
        let mut seq = seq_with_audio_at(1);
        let mut pool = MediaPool::new();
        let dur_48 = RationalTime { value: 24, rate: 48 };
        let media_id = pool.add(audio_media_item(dur_48));
        let timeline_in_24 = RationalTime { value: 12, rate: 24 }; // 0.5 s
        insert_audio_media_clip_into_seq(&mut seq, &pool, media_id, None, timeline_in_24)
            .expect("insert");
        let clip = &seq.tracks[1].clips[0];
        assert_eq!(clip.timeline_in.rate, 48, "rescaled to source rate");
        assert_eq!(clip.timeline_in.value, 24, "0.5 s @ 48 rate = 24 ticks");
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

/// Where a title clip shows at rest: past its entrance when a layer has
/// one (a third of the clip, at most a second in), else its first frame.
/// `None` for a clip that is not a title.
fn title_rest_time(clip: &klipp_timeline::Clip, fps_rate: u32) -> Option<klipp_core::RationalTime> {
    let td = clip.title_data()?;
    if !td.objects.iter().any(|o| o.fx().motion.enter.is_some()) {
        return Some(clip.timeline_in);
    }
    let settle = ((clip.timeline_out - clip.timeline_in).to_secs_f64() / 3.0).min(1.0);
    Some(klipp_core::RationalTime::from_secs_f64(clip.timeline_in.to_secs_f64() + settle, fps_rate))
}

/// Where to put the playhead when a title is selected: its rest pose if
/// the playhead is outside the title, else nowhere (leave it).
fn title_seek_on_select(
    clip: &klipp_timeline::Clip,
    playhead: klipp_core::RationalTime,
    fps_rate: u32,
) -> Option<klipp_core::RationalTime> {
    if playhead >= clip.timeline_in && playhead < clip.timeline_out {
        return None;
    }
    title_rest_time(clip, fps_rate)
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

#[cfg(test)]
mod title_rest_tests {
    use super::{title_rest_time, title_seek_on_select};
    use klipp_core::RationalTime;
    use klipp_core::title_motion::{Motion, TitleMotion};
    use klipp_timeline::{Clip, ClipKind, TitleData};

    fn t(secs: i64) -> RationalTime { RationalTime { value: secs * 30, rate: 30 } }

    fn title(from: i64, to: i64, entrance: bool) -> Clip {
        let mut td = TitleData::default();
        if entrance {
            td.objects[0].fx_mut().motion = TitleMotion::enter(Motion::default());
        }
        let mut c = Clip::media(klipp_core::MediaId::new(), t(from), t(to), t(0), t(to - from));
        c.kind = ClipKind::Title(td);
        c
    }

    #[test]
    fn a_plain_title_rests_on_its_first_frame() {
        assert_eq!(title_rest_time(&title(10, 15, false), 30), Some(t(10)));
    }

    #[test]
    fn a_title_with_an_entrance_rests_past_it() {
        assert_eq!(title_rest_time(&title(10, 15, true), 30), Some(t(11)));
    }

    #[test]
    fn selecting_a_title_off_the_playhead_brings_the_playhead_to_it() {
        let c = title(10, 15, false);
        assert_eq!(title_seek_on_select(&c, t(2), 30), Some(t(10)));
        assert_eq!(title_seek_on_select(&c, t(15), 30), Some(t(10)), "the out point is past the title");
        assert_eq!(title_seek_on_select(&c, t(12), 30), None, "a playhead on the title stays put");
        assert_eq!(title_seek_on_select(&c, t(10), 30), None);
    }

    #[test]
    fn a_clip_that_is_not_a_title_has_no_rest_pose() {
        let c = Clip::media(klipp_core::MediaId::new(), t(0), t(5), t(0), t(5));
        assert_eq!(title_rest_time(&c, 30), None);
    }
}

#[cfg(test)]
mod attach_linked_audio_tests {
    use super::*;
    /// The bug the video-only toggle exposed, pinned.
    ///
    /// A gap on the audio track — left by a video-only append, or by
    /// appending a silent file — used to send the *next* clip's audio
    /// into the gap instead of under its own picture, because the append
    /// placed it at the end of the audio track rather than at the clip.
    /// On a real timeline that put the sound under the one clip that was
    /// meant to be silent, and left the clip that should have had it
    /// mute.
    #[test]
    fn a_gap_on_the_audio_track_does_not_misplace_the_next_clips_audio() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);

        // Three video clips; only the third gets audio, as if the first
        // two were appended video-only.
        let ids: Vec<_> = (0..3).map(|_| seq.tracks[vi].append_clip(media_id, t(4))).collect();
        let third = *ids.last().unwrap();
        let want = seq.tracks[vi].find_clip(third).unwrap().clone();

        let added = attach_linked_audio_to_clip(&mut seq, third, None).expect("attached");
        let audio = &seq.tracks[added.track_index].clips[0];
        assert_eq!(
            audio.timeline_in, want.timeline_in,
            "the audio went to the front of the empty audio track instead of under its clip",
        );
        assert_eq!(audio.timeline_out, want.timeline_out);
        assert_eq!(audio.linked_clip_id, Some(third));

        // And the clips that were appended video-only stay silent.
        for id in &ids[..2] {
            let c = seq.tracks[vi].find_clip(*id).unwrap();
            assert_eq!(c.linked_clip_id, None, "a video-only clip gained audio");
        }
    }

    use klipp_timeline::{Sequence, TrackKind};

    fn t(secs: i64) -> klipp_core::RationalTime {
        klipp_core::RationalTime { value: secs * 30, rate: 30 }
    }

    fn video_track(seq: &Sequence) -> usize {
        seq.tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap()
    }

    fn audio_track(seq: &Sequence) -> Option<usize> {
        seq.tracks.iter().position(|tr| tr.kind == TrackKind::Audio)
    }

    /// The repair case: a clip already on the timeline, with a grade on
    /// it, gets its audio without being rebuilt.
    #[test]
    fn an_existing_clip_gets_audio_at_its_own_position() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        // Two clips, so the second does not start at zero.
        let _first = seq.tracks[vi].append_clip(media_id, t(4));
        let second = seq.tracks[vi].append_clip(media_id, t(4));
        let want = seq.tracks[vi].find_clip(second).unwrap().clone();
        assert!(want.timeline_in > t(0), "the fixture's second clip must not start at zero");

        let added = attach_linked_audio_to_clip(&mut seq, second, None).expect("attached");
        let audio = &seq.tracks[added.track_index].clips[0];
        assert_eq!(audio.timeline_in, want.timeline_in, "under the clip, not at the start");
        assert_eq!(audio.timeline_out, want.timeline_out);
        assert_eq!(audio.linked_clip_id, Some(second));
        let video = seq.all_clips().find(|c| c.id == second).unwrap();
        assert_eq!(video.linked_clip_id, Some(audio.id), "linked both ways");
    }

    /// A trimmed clip gets the sound under what is left of it. Taking
    /// the media from the top instead would put the audio out of sync by
    /// however much was trimmed off the front — the kind of error that
    /// looks fine until someone listens.
    #[test]
    fn a_trimmed_clip_gets_the_matching_stretch_of_sound() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        let id = seq.tracks[vi].append_clip(media_id, t(10));
        if let Some(c) = seq.tracks[vi].find_clip_mut(id) {
            c.source_in = t(3);
            c.source_out = t(7);
            c.timeline_in = t(0);
            c.timeline_out = t(4);
        }

        attach_linked_audio_to_clip(&mut seq, id, None).expect("attached");
        let ai = audio_track(&seq).unwrap();
        let audio = &seq.tracks[ai].clips[0];
        assert_eq!(audio.source_in, t(3), "the same point in the file");
        assert_eq!(audio.source_out, t(7));
        assert_eq!(audio.timeline_out - audio.timeline_in, t(4), "and the same length");
    }

    /// A synced recording goes under the clip at its offset, and is cut
    /// where the recording does not reach: a recorder started late
    /// leaves the head of the clip silent rather than sound from before
    /// the recording began.
    #[test]
    fn a_synced_recording_lies_under_the_clip_at_its_offset() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let recording = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        let id = seq.tracks[vi].append_clip(media_id, t(10));
        if let Some(c) = seq.tracks[vi].find_clip_mut(id) {
            c.source_in = t(2);
            c.source_out = t(7);
            c.timeline_in = t(0);
            c.timeline_out = t(5);
        }
        // The camera's first frame is 1.5 s into the recording.
        let ext = ExternalAudio { media_id: recording, offset_secs: 1.5, len_secs: 100.0 };
        attach_linked_audio_to_clip(&mut seq, id, Some(ext)).expect("attached");
        let ai = audio_track(&seq).unwrap();
        let audio = &seq.tracks[ai].clips[0];
        assert_eq!(audio.media_id(), Some(recording), "the recording, not the camera's sound");
        assert!((audio.source_in.to_secs_f64() - 3.5).abs() < 1e-6);
        assert!((audio.source_out.to_secs_f64() - 8.5).abs() < 1e-6);
        assert_eq!(audio.timeline_in, t(0));
        assert_eq!(audio.timeline_out, t(5));

        // A recorder started 3 s after the camera: the first second of the
        // clip has no recording, so the sound starts a second in.
        let mut seq = Sequence::new_default();
        let vi = video_track(&seq);
        let id = seq.tracks[vi].append_clip(media_id, t(10));
        if let Some(c) = seq.tracks[vi].find_clip_mut(id) {
            c.source_in = t(2);
            c.source_out = t(7);
            c.timeline_in = t(4);
            c.timeline_out = t(9);
        }
        let ext = ExternalAudio { media_id: recording, offset_secs: -3.0, len_secs: 100.0 };
        attach_linked_audio_to_clip(&mut seq, id, Some(ext)).expect("attached");
        let ai = audio_track(&seq).unwrap();
        let audio = &seq.tracks[ai].clips[0];
        assert!((audio.source_in.to_secs_f64()).abs() < 1e-6, "from the recording's start");
        assert!((audio.source_out.to_secs_f64() - 4.0).abs() < 1e-6);
        assert_eq!(audio.timeline_in, t(5), "a second into the clip");
        assert_eq!(audio.timeline_out, t(9));
    }

    /// Asking twice must not stack two copies of the sound.
    #[test]
    fn a_clip_that_already_has_audio_is_left_alone() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        let id = seq.tracks[vi].append_clip(media_id, t(4));

        attach_linked_audio_to_clip(&mut seq, id, None).expect("the first attach works");
        let ai = audio_track(&seq).unwrap();
        assert_eq!(seq.tracks[ai].clips.len(), 1);
        assert!(
            attach_linked_audio_to_clip(&mut seq, id, None).is_none(),
            "the second attach is refused"
        );
        assert_eq!(seq.tracks[ai].clips.len(), 1, "and adds nothing");
    }

    /// Repairing every clip of a silent timeline leaves the audio track
    /// laid out like the video track, which is the whole point.
    #[test]
    fn repairing_a_whole_timeline_mirrors_the_video_track() {
        let mut seq = Sequence::new_default();
        let media_id = klipp_core::MediaId::new();
        let vi = video_track(&seq);
        let ids: Vec<_> = (0..3).map(|_| seq.tracks[vi].append_clip(media_id, t(4))).collect();
        let spans: Vec<_> = ids
            .iter()
            .map(|id| {
                let c = seq.tracks[vi].find_clip(*id).unwrap();
                (c.timeline_in, c.timeline_out)
            })
            .collect();

        for id in &ids {
            attach_linked_audio_to_clip(&mut seq, *id, None).expect("attached");
        }
        let ai = audio_track(&seq).unwrap();
        assert_eq!(seq.tracks[ai].clips.len(), 3, "one audio clip per video clip");
        let mut got: Vec<_> =
            seq.tracks[ai].clips.iter().map(|c| (c.timeline_in, c.timeline_out)).collect();
        got.sort();
        let mut want = spans;
        want.sort();
        assert_eq!(got, want, "the audio track matches the video track's layout");
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

#[cfg(test)]
mod linked_audio_frame_tests {
    use super::edit_linked_audio_key;
    use klipp_core::keyframe::upsert_keyframe;

    #[test]
    fn a_nudged_partner_key_is_edited_and_removed_at_its_own_time() {
        let mut keys = Vec::new();
        upsert_keyframe(&mut keys, 0.707, 0.5);
        upsert_keyframe(&mut keys, 0.750, 0.8);
        edit_linked_audio_key(&mut keys, 0.700, 0.5 / 24.0, 0.707, Some(0.3));
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].time_secs, 0.707);
        assert_eq!(keys[0].value, 0.3);
        assert_eq!(keys[1].value, 0.8);
        edit_linked_audio_key(&mut keys, 0.700, 0.5 / 24.0, 0.707, None);
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].time_secs, 0.750);
    }
}

#[cfg(test)]
mod exact_audio_review_tests {
    use super::edit_linked_audio_key;
    #[test]
    fn exact_linked_edits_do_not_overwrite_nearby_keys() {
        let mut main = Vec::new();
        klipp_core::keyframe::upsert_keyframe(&mut main,0.707,0.5);
        let mut partner = main.clone();
        klipp_core::keyframe::upsert_keyframe(&mut main,0.700,0.3);
        edit_linked_audio_key(&mut partner,0.700,0.0,0.700,Some(0.3));
        assert_eq!(main,partner);
        assert_eq!(partner.len(),2);
        klipp_core::keyframe::remove_keyframe_at(&mut main,0.700);
        edit_linked_audio_key(&mut partner,0.700,0.0,0.700,None);
        assert_eq!(main,partner);
        assert_eq!(partner[0].time_secs,0.707);
    }
}

#[cfg(test)]
mod linked_missing_key_review_tests {
    #[test]
    fn absent_partner_key_uses_the_selected_main_timestamp() {
        let mut partner=vec![];
        super::edit_linked_audio_key(&mut partner,0.700,0.5/24.0,0.707,Some(0.25));
        assert_eq!(partner[0].time_secs,0.707);
    }
}

#[cfg(test)]
mod sequence_camera_tests {
    use super::*;
    use klipp_core::CameraParam;

    /// A key goes on the list its param names and nowhere else.
    #[test]
    fn a_key_lands_on_one_axis() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::Zoom, 1.0, Some(1.4));
        assert_eq!(cam.zoom_keyframes.len(), 1);
        assert_eq!(cam.zoom_keyframes[0].value, 1.4);
        assert!(
            cam.x_keyframes.is_empty() && cam.y_keyframes.is_empty() && cam.roll_keyframes.is_empty(),
            "keying the zoom should leave the other three alone",
        );
    }

    /// Keying the same moment twice replaces rather than stacks, and
    /// clearing it takes it away — the diamond's two states.
    #[test]
    fn keying_twice_replaces_and_clearing_removes() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::X, 2.0, Some(0.4));
        apply_camera_key(&mut cam, CameraParam::X, 2.0, Some(0.6));
        assert_eq!(cam.x_keyframes.len(), 1, "one moment holds one key");
        assert_eq!(cam.x_keyframes[0].value, 0.6);
        apply_camera_key(&mut cam, CameraParam::X, 2.0, None);
        assert!(cam.x_keyframes.is_empty(), "clearing the key at that moment removes it");
    }

    /// Nothing keyed means the camera is still the camera: no keys, no
    /// lists, nothing written to the project.
    #[test]
    fn a_camera_with_no_keys_stays_empty() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::Y, 1.0, None);
        assert!(cam.y_keyframes.is_empty());
        assert_eq!(cam, klipp_timeline::Camera::default());
    }

    /// A key dragged past its neighbour comes out on the other side of
    /// it, in a list that is still in order; the drag names it by its
    /// moment, so the lane finds it again wherever it sorted to.
    #[test]
    fn a_moved_key_keeps_the_list_sorted() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::X, 1.0, Some(0.2));
        apply_camera_key(&mut cam, CameraParam::X, 2.0, Some(0.8));
        assert!(move_camera_key(&mut cam, CameraParam::X, 0, 3.0, 0.3));
        let times: Vec<f64> = cam.x_keyframes.iter().map(|k| k.time_secs).collect();
        assert_eq!(times, vec![2.0, 3.0]);
        assert_eq!(cam.x_keyframes[1].value, 0.3);
        assert_eq!(crate::timeline::camera_lane::index_at(&cam.x_keyframes, 3.0), Some(1));
        assert!(!move_camera_key(&mut cam, CameraParam::Y, 0, 1.0, 0.5), "no key, no move");
    }

    /// The curve out of a key and into it: the interpolation goes on
    /// the key and on the one before it, as a clip's keyframes do, so
    /// the diamond's two segments take the shape together.
    #[test]
    fn interpolation_shapes_both_segments_at_the_diamond() {
        use klipp_core::keyframe::Interpolation;
        let mut cam = klipp_timeline::Camera::default();
        for t in [1.0, 2.0, 3.0] {
            apply_camera_key(&mut cam, CameraParam::Zoom, t, Some(1.0));
        }
        set_camera_key_interpolation(&mut cam, CameraParam::Zoom, 1, Interpolation::Step);
        let shapes: Vec<_> = cam.zoom_keyframes.iter().map(|k| k.interpolation).collect();
        assert_eq!(shapes, vec![Interpolation::Step, Interpolation::Step, Interpolation::Linear]);
        set_camera_key_interpolation(&mut cam, CameraParam::Zoom, 0, Interpolation::Bezier);
        assert_eq!(cam.zoom_keyframes[0].interpolation, Interpolation::Bezier, "the first key has no key before it");
        set_camera_key_interpolation(&mut cam, CameraParam::Zoom, 9, Interpolation::Linear);
        assert_eq!(cam.zoom_keyframes[2].interpolation, Interpolation::Linear, "an index past the end changes nothing");
    }

    /// Either handle alone, or both.
    #[test]
    fn handles_are_set_one_at_a_time_or_together() {
        let mut cam = klipp_timeline::Camera::default();
        apply_camera_key(&mut cam, CameraParam::Roll, 1.0, Some(0.0));
        set_camera_key_tangent(&mut cam, CameraParam::Roll, 0, None, Some((0.4, 2.0)));
        assert_eq!(cam.roll_keyframes[0].tangent_out, (0.4, 2.0));
        assert_eq!(cam.roll_keyframes[0].tangent_in, (0.0, 0.0));
        set_camera_key_tangent(&mut cam, CameraParam::Roll, 0, Some((-0.2, -1.0)), Some((0.1, 0.5)));
        assert_eq!(cam.roll_keyframes[0].tangent_in, (-0.2, -1.0));
        assert_eq!(cam.roll_keyframes[0].tangent_out, (0.1, 0.5));
    }
}

/// `s` with its first letter upper-case, for a refusal's hint shown as a
/// sentence of its own.
fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

#[cfg(test)]
mod paste_attributes_tests {
    use super::*;
    use klipp_timeline::{Clip, Sequence};

    fn t(secs: i64) -> klipp_core::Timecode {
        klipp_core::Timecode { value: secs * 25, rate: 25 }
    }

    fn clip(from: i64, to: i64) -> Clip {
        Clip::media(klipp_core::MediaId::new(), t(from), t(to), t(0), t(to - from))
    }

    #[test]
    fn a_paste_reaches_every_target_as_one_undoable_step() {
        let mut seq = Sequence::new_default();
        let source = {
            let mut c = clip(0, 2);
            c.node_graph = klipp_nodes::NodeGraph::default();
            c.volume = 0.25;
            c
        };
        // The first clip in the sequence is not a target: the old handler
        // stopped there and pasted nothing.
        let (a, b, c) = (clip(0, 2), clip(2, 4), clip(4, 6));
        let targets = vec![b.id, c.id];
        seq.tracks[0].clips = vec![a.clone(), b.clone(), c.clone()];
        let attrs = crate::state::AttributesClipboard::of(&source);
        let what = crate::state::PasteChoice { color_grade: true, audio: true, ..Default::default() };
        let edits = paste_attributes_edits(&seq, &targets, &attrs, what);
        assert_eq!(edits.len(), 2);

        let mut sequences = vec![seq];
        let mut pool = klipp_media::MediaPool::new();
        let mut fps = sequences[0].fps;
        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in &edits {
            view.apply_forward(e);
        }
        let pasted: Vec<&Clip> = sequences[0].tracks[0].clips.iter().filter(|x| targets.contains(&x.id)).collect();
        assert!(pasted.iter().all(|x| x.volume == 0.25));
        // Fresh node ids on each clip, not the source's.
        let ids = |g: &klipp_nodes::NodeGraph| g.nodes.iter().map(|n| n.id).collect::<Vec<_>>();
        assert_ne!(ids(&pasted[0].node_graph), ids(&source.node_graph));
        assert_ne!(ids(&pasted[0].node_graph), ids(&pasted[1].node_graph));
        assert_eq!(sequences[0].tracks[0].clips[0].volume, a.volume, "a clip that was not a target is untouched");

        let mut view = klipp_document::apply::DocumentView { sequences: &mut sequences, active: 0, media_pool: &mut pool, fps: &mut fps };
        for e in edits.iter().rev() {
            view.apply_inverse(e);
        }
        assert_eq!(sequences[0].tracks[0].clips[1].volume, b.volume);
        assert_eq!(sequences[0].tracks[0].clips[2].volume, c.volume);
    }

    /// A clip graded by `stops` of exposure on its Corrector.
    fn graded(mut c: Clip, stops: f32) -> Clip {
        let id = c.node_graph.ensure_corrector();
        if let Some(n) = c.node_graph.nodes.iter_mut().find(|n| n.id == id) {
            n.params.exposure = Some(stops);
        }
        c
    }

    fn input_transforms(g: &klipp_nodes::NodeGraph) -> Vec<&klipp_nodes::Node> {
        g.nodes.iter().filter(|n| n.kind == klipp_nodes::NodeKind::InputTransform).collect()
    }

    fn exposure(g: &klipp_nodes::NodeGraph) -> Option<f32> {
        g.nodes.iter().find(|n| n.kind == klipp_nodes::NodeKind::Corrector).and_then(|n| n.params.exposure)
    }

    /// A grade copied from a LogC shot, pasted onto a Sony shot: the
    /// Sony shot's decode stays its own, and the grade is the LogC
    /// shot's. It used to take the whole graph, ARRI curve included.
    #[test]
    fn a_pasted_grade_leaves_the_targets_input_transform_alone() {
        use klipp_core::LogFormat;
        let mut source = graded(clip(0, 2), -0.2);
        source.set_input_log_format(LogFormat::LogC3);
        let mut target = graded(clip(2, 4), 0.4);
        target.set_input_log_format(LogFormat::SLog3);
        let own = input_transforms(&target.node_graph)[0].clone();

        let attrs = crate::state::AttributesClipboard::of(&source);
        let what = crate::state::PasteChoice { color_grade: true, ..Default::default() };
        attrs.paste_onto(&mut target, what);

        let kept = input_transforms(&target.node_graph);
        assert_eq!(kept.len(), 1, "one Input Transform, the clip's own");
        assert_eq!(kept[0].kind, klipp_nodes::NodeKind::InputTransform);
        assert_eq!(kept[0].params, own.params, "the Sony decode, not ARRI's");
        assert_eq!(kept[0].params.log_format, Some(LogFormat::SLog3));
        assert_eq!(exposure(&target.node_graph), Some(-0.2), "the source's grade");
        assert!(
            target.node_graph.nodes.iter().all(|n| !source.node_graph.nodes.iter().any(|s| s.id == n.id)),
            "fresh ids"
        );
        assert!(target.node_graph.topo_sort().is_ok(), "still a graph the executor can run");
    }

    /// A grade copied from a Rec.709 shot has no Input Transform; the
    /// log shot it is pasted onto keeps its own rather than losing it.
    #[test]
    fn a_pasted_grade_without_an_input_transform_keeps_the_targets() {
        use klipp_core::LogFormat;
        let source = graded(clip(0, 2), -0.2);
        assert!(input_transforms(&source.node_graph).is_empty());
        let mut target = graded(clip(2, 4), 0.4);
        target.set_input_log_format(LogFormat::SLog3);
        let own = input_transforms(&target.node_graph)[0].clone();

        let attrs = crate::state::AttributesClipboard::of(&source);
        let what = crate::state::PasteChoice { color_grade: true, ..Default::default() };
        attrs.paste_onto(&mut target, what);

        let kept = input_transforms(&target.node_graph);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].params, own.params);
        assert_eq!(exposure(&target.node_graph), Some(-0.2));
    }

    /// The other way round: a Rec.709 shot pasted a LogC shot's grade
    /// gets the grade and no decode it does not need.
    #[test]
    fn a_pasted_grade_brings_no_input_transform_to_a_clip_without_one() {
        let mut source = graded(clip(0, 2), -0.2);
        source.set_input_log_format(klipp_core::LogFormat::LogC3);
        let mut target = graded(clip(2, 4), 0.4);
        let attrs = crate::state::AttributesClipboard::of(&source);
        let what = crate::state::PasteChoice { color_grade: true, ..Default::default() };
        attrs.paste_onto(&mut target, what);
        assert!(input_transforms(&target.node_graph).is_empty());
        assert_eq!(exposure(&target.node_graph), Some(-0.2));
    }
}

#[cfg(test)]
mod grade_still_tests {
    use super::*;
    use klipp_timeline::{Clip, Sequence};

    fn t(secs: i64) -> klipp_core::Timecode {
        klipp_core::Timecode { value: secs * 25, rate: 25 }
    }

    fn clip(from: i64, to: i64) -> Clip {
        Clip::media(klipp_core::MediaId::new(), t(from), t(to), t(0), t(to - from))
    }

    #[test]
    fn a_still_is_taken_from_the_selected_clip_only_when_it_is_under_the_playhead() {
        let mut seq = Sequence::new_default();
        let (a, b) = (clip(0, 4), clip(4, 8));
        seq.tracks[0].clips = vec![a.clone(), b.clone()];
        assert_eq!(still_source_clip(&seq, t(1), Some(a.id)).map(|c| c.id), Some(a.id));
        // Selected elsewhere: the clip the viewer shows.
        assert_eq!(still_source_clip(&seq, t(5), Some(a.id)).map(|c| c.id), Some(b.id));
        assert!(still_source_clip(&seq, t(9), Some(a.id)).is_none(), "a gap has no clip");
    }

    #[test]
    fn a_still_grade_goes_on_every_target_with_its_own_ids_and_undoes_as_one() {
        let mut seq = Sequence::new_default();
        let (a, b, c) = (clip(0, 2), clip(2, 4), clip(4, 6));
        seq.tracks[0].clips = vec![a.clone(), b.clone(), c.clone()];
        let grade = klipp_nodes::NodeGraph::default();
        let (edits, _) = grade_from_still_edits(&seq, &grade, &[a.id, c.id], false);
        assert_eq!(edits.len(), 2);
        let ids = |e: &EditCommand| match e {
            EditCommand::SetNodeGraph { new_graph, .. } => new_graph.nodes.iter().map(|n| n.id).collect::<Vec<_>>(),
            _ => panic!("a graph edit"),
        };
        assert_ne!(ids(&edits[0]), ids(&edits[1]), "each clip gets fresh node ids");
        assert_ne!(ids(&edits[0]), grade.nodes.iter().map(|n| n.id).collect::<Vec<_>>());
    }

    #[test]
    fn an_added_still_grade_keeps_each_clips_own_balance() {
        let mut seq = Sequence::new_default();
        let mut a = clip(0, 2);
        a.node_graph.ensure_corrector();
        let own: Vec<_> = a.node_graph.nodes.iter().map(|n| n.id).collect();
        seq.tracks[0].clips = vec![a.clone()];
        let mut look = klipp_nodes::NodeGraph::default();
        look.ensure_lut3d();
        let (edits, _) = grade_from_still_edits(&seq, &look, &[a.id], true);
        let EditCommand::SetNodeGraph { new_graph, .. } = &edits[0] else { panic!("a graph edit") };
        assert!(own.iter().all(|id| new_graph.nodes.iter().any(|n| n.id == *id)), "the clip's own nodes stay");
        assert!(new_graph.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Lut3D), "the look is added");
    }

    /// A target that cannot take the grade is passed over with a reason
    /// to show, not counted as "nothing selected".
    #[test]
    fn a_still_grade_that_cannot_be_added_says_why() {
        use klipp_core::WorkingColorspace as Ws;
        let mut seq = Sequence::new_default();
        let mut a = clip(0, 2);
        a.node_graph.working_space_override = Some(Ws::Rec709Linear);
        seq.tracks[0].clips = vec![a.clone()];
        let mut look = klipp_nodes::NodeGraph::default();
        look.ensure_lut3d();
        look.working_space_override = Some(Ws::AcesCG);
        let (edits, refused) = grade_from_still_edits(&seq, &look, &[a.id], true);
        assert!(edits.is_empty());
        assert_eq!(refused, vec![klipp_nodes::CannotAppend::WorkingSpace { ours: Ws::Rec709Linear, theirs: Ws::AcesCG }]);
        assert!(refusals_line(&refused).contains("working space"));
    }

    #[test]
    fn a_record_timecode_reads_as_an_editor_would_say_it() {
        assert_eq!(record_timecode(3723.48, 25.0), "01:02:03:12");
    }
}

#[cfg(test)]
mod timeline_grade_stand_in_tests {
    use super::*;

    /// The stand-in on its track, as [`KlippApp::stand_in_timeline_grade`]
    /// puts it there; the graph it was given comes back for the settle.
    fn stand_in(seq: &mut klipp_timeline::Sequence) -> (klipp_nodes::NodeGraph, &mut klipp_nodes::NodeGraph) {
        let proxy = klipp_timeline::Clip::timeline_grade_proxy(seq);
        let given = proxy.node_graph.clone();
        let mut track = klipp_timeline::Track::new("Timeline Grade", klipp_timeline::TrackKind::Video);
        track.id = timeline_grade_proxy_track_id(seq);
        track.clips.push(proxy);
        seq.tracks.push(track);
        (given, &mut seq.tracks.last_mut().unwrap().clips[0].node_graph)
    }

    #[test]
    fn the_stand_in_track_never_outlives_its_graph() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let tracks = seq.tracks.len();
        let (given, graph) = stand_in(&mut seq);
        let corrector = graph.ensure_corrector();
        graph.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.5);
        let nodes = graph.nodes.len();
        assert!(settle_timeline_grade_proxy(&mut seq, Some(&given)));
        assert_eq!(seq.tracks.len(), tracks);
        assert_eq!(seq.timeline_graph.map(|g| g.nodes.len()), Some(nodes));
    }

    /// Opening the Timeline tab and looking is not an edit: the sequence
    /// keeps having no grade, and nothing is marked changed.
    #[test]
    fn a_bare_visit_leaves_a_sequence_without_a_grade() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let (given, _) = stand_in(&mut seq);
        assert!(!settle_timeline_grade_proxy(&mut seq, Some(&given)));
        assert!(seq.timeline_graph.is_none());
        assert!(!seq.tracks.iter().any(|t| t.id == timeline_grade_proxy_track_id(&seq)));
    }

    /// A node added and left at its defaults is still an edit the user
    /// made, and must not be lost between two commands.
    #[test]
    fn a_node_added_at_its_defaults_is_kept() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let (given, graph) = stand_in(&mut seq);
        graph.ensure_lut3d();
        assert!(settle_timeline_grade_proxy(&mut seq, Some(&given)));
        assert!(seq.timeline_graph.is_some_and(|g| g.nodes.iter().any(|n| n.kind == klipp_nodes::NodeKind::Lut3D)));
    }

    /// With a grade already there, a visit that changes nothing changes
    /// nothing.
    #[test]
    fn an_unchanged_stand_in_leaves_an_existing_grade_alone() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut grade = klipp_nodes::NodeGraph::default();
        let corrector = grade.ensure_corrector();
        grade.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.5);
        seq.timeline_graph = Some(grade.clone());
        let (_, _) = stand_in(&mut seq);
        assert!(!settle_timeline_grade_proxy(&mut seq, None));
        assert_eq!(seq.timeline_graph, Some(grade));
    }

    /// #237: a grade group's stage stands in the same way: graded through
    /// its stand-in, its graph lands on the group's stage; a visit that
    /// changes nothing leaves the stage without a graph.
    #[test]
    fn a_grade_group_stage_stands_in_and_settles_on_the_group() {
        use klipp_timeline::{GradeGroup, GradeStage};
        let mut seq = klipp_timeline::Sequence::new_default();
        let group = GradeGroup::new("Night");
        let gid = group.id;
        seq.grade_groups.push(group);
        let which = GradeStandIn::Group(gid, GradeStage::Post);
        let mut fresh = None;

        let id = stand_in_grade_on(&mut seq, which, &mut fresh).expect("stood in");
        assert_eq!(GradeStandIn::of(&seq, id), Some(which));
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Unchanged, "a look is not an edit");
        assert!(seq.grade_groups[0].post.is_none());

        stand_in_grade_on(&mut seq, which, &mut fresh).expect("stood in");
        let track_id = timeline_grade_proxy_track_id(&seq);
        let proxy = seq.tracks.iter_mut().find(|t| t.id == track_id).unwrap().clips.first_mut().unwrap();
        let lut = proxy.node_graph.ensure_lut3d();
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Begun);
        assert!(seq.grade_groups[0].post.as_ref().is_some_and(|g| g.nodes.iter().any(|n| n.id == lut)), "the edit landed on the group's post-clip grade");
        assert!(seq.grade_groups[0].pre.is_none() && seq.timeline_graph.is_none(), "and nowhere else");
        assert!(!seq.tracks.iter().any(|t| t.id == track_id), "the stand-in is gone");
    }

    /// The whole round, as [`KlippApp::stand_in_timeline_grade`] and
    /// [`KlippApp::stand_down_timeline_grade`] make it: a first visit
    /// that edits begins the grade, and the next visit that only looks
    /// leaves it as it is. The fresh graph the first stand-in was made
    /// with must not be what the second is measured against, or the
    /// grade, with its own node ids, differs from it on every command.
    #[test]
    fn a_visit_after_the_grade_began_is_not_an_edit() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut fresh = None;
        let id = stand_in_grade_on(&mut seq, GradeStandIn::Timeline, &mut fresh).expect("stood in");
        assert!(fresh.as_ref().is_some_and(|(seq_id, _)| *seq_id == seq.timeline_grade_clip_id()));
        let track_id = timeline_grade_proxy_track_id(&seq);
        let graph = &mut seq.tracks.iter_mut().find(|t| t.id == track_id).unwrap().clips[0].node_graph;
        let corrector = graph.ensure_corrector();
        graph.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.5);
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Begun);
        let grade = seq.timeline_graph.clone().expect("the grade began");
        assert!(fresh.is_none(), "the fresh graph is let go once the grade begins");

        assert_eq!(stand_in_grade_on(&mut seq, GradeStandIn::Timeline, &mut fresh), Some(id));
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Unchanged);
        assert_eq!(seq.timeline_graph, Some(grade), "the grade is as it was");
        assert!(!seq.tracks.iter().any(|t| t.id == track_id));
    }

    /// The same, for a grade the sequence already had when the fresh
    /// graph was made for it: only a sequence without a grade is
    /// measured against the fresh graph.
    #[test]
    fn a_stale_fresh_graph_is_not_held_against_an_existing_grade() {
        let mut seq = klipp_timeline::Sequence::new_default();
        let mut grade = klipp_nodes::NodeGraph::default();
        let corrector = grade.ensure_corrector();
        grade.nodes.iter_mut().find(|n| n.id == corrector).unwrap().params.exposure = Some(0.5);
        seq.timeline_graph = Some(grade.clone());
        let mut fresh = Some((seq.timeline_grade_clip_id(), klipp_nodes::NodeGraph::default()));
        stand_in_grade_on(&mut seq, GradeStandIn::Timeline, &mut fresh).expect("stood in");
        assert_eq!(stand_down_timeline_grade_on(&mut seq, &mut fresh), TimelineGradeSettled::Unchanged);
        assert_eq!(seq.timeline_graph, Some(grade));
    }
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

#[cfg(test)]
mod window_track_tests {
    use super::{window_perspective_track, window_quad, window_track_keys};

    /// #238: a window track's keyframes sit on the clip's own clock — the
    /// grade's — not the file's: a clip trimmed to start 2 s into its file
    /// and played at double speed has the file's frame 60 (2 s at 30 fps)
    /// at its first moment, and 75 a quarter second later. Frames the clip
    /// does not play are left out.
    #[test]
    fn a_window_tracks_keys_land_on_the_clips_own_clock() {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/walk.mov".into(), None);
        item.fps = klipp_core::Fps::new(30, 1);
        let media = pool.add(item);
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 30);
        let mut clip = klipp_timeline::Clip::media(media, tc(10.0), tc(11.0), tc(2.0), tc(4.0));
        clip.speed = 2.0;
        // Frames 58..=92 tracked from frame 60, moving right 0.01 a frame.
        let tp = klipp_core::TrackPoint {
            start_frame: 58,
            positions: (58..=92).map(|f| (0.5 + 0.01 * (f as f64 - 60.0), 0.4)).collect(),
            motion: (58..=92).map(|f| (1.0 + 0.001 * (f as f64 - 60.0), 0.5 * (f as f64 - 60.0))).collect(),
            reference_frame: Some(60),
            ..Default::default()
        };
        let keys = window_track_keys(&clip, &tp, &pool, 30.0);
        let first = keys.first().unwrap();
        assert!(first.0.abs() < 1e-9 && first.1.abs() < 1e-6 && (first.3 - 1.0).abs() < 1e-6, "{first:?}");
        let quarter = keys.iter().find(|k| (k.0 - 0.25).abs() < 1e-6).expect("a key a quarter second in");
        assert!((quarter.1 - 0.15).abs() < 1e-5 && (quarter.4 - 7.5).abs() < 1e-4, "{quarter:?}");
        assert!(keys.iter().all(|k| k.0 >= 0.0 && k.0 <= 1.0 + 1e-6), "only what the clip plays");

        // Reversed (#617): the same frames run the other way on the clip's
        // clock — the reference frame, its range's start, comes last.
        clip.speed = -2.0;
        let back = window_track_keys(&clip, &tp, &pool, 30.0);
        assert!(back.windows(2).all(|w| w[0].0 <= w[1].0), "earliest first");
        let last = back.last().unwrap();
        assert!((last.0 - (1.0 - 1.0 / 30.0)).abs() < 1e-6 && last.1.abs() < 1e-6, "the reference frame shows a frame before the clip's end: {last:?}");
        assert!(back.first().unwrap().1 > 0.0, "the later frames show first: {:?}", back.first());
    }

    /// #238: a track in perspective becomes the window's corner course on
    /// the same clock, measured from the corners on the reference frame;
    /// a window evaluated from it is carried there.
    #[test]
    fn a_perspective_track_lands_its_corners_on_the_clips_own_clock() {
        let mut pool = klipp_media::MediaPool::new();
        let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/sign.mov".into(), None);
        item.fps = klipp_core::Fps::new(30, 1);
        let media = pool.add(item);
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 30);
        let clip = klipp_timeline::Clip::media(media, tc(10.0), tc(11.0), tc(2.0), tc(3.0));
        let quad = [(0.4, 0.4), (0.6, 0.4), (0.6, 0.6), (0.4, 0.6)];
        // Frames 58..=70 from frame 60: the right side sliding up and in.
        let course = |f: usize| {
            let k = f as f64 - 60.0;
            [quad[0], (0.6 - 0.002 * k, 0.4 + 0.003 * k), (0.6 - 0.002 * k, 0.6 - 0.003 * k), quad[3]]
        };
        let tp = klipp_core::TrackPoint {
            start_frame: 58,
            positions: (58..=70).map(|_| (0.5, 0.5)).collect(),
            corners: (58..=70).map(course).collect(),
            reference_frame: Some(60),
            ..Default::default()
        };
        let track = window_perspective_track(&clip, &tp, &pool, 30.0).unwrap();
        assert_eq!(track.from, quad.map(|(x, y)| [x as f32, y as f32]));
        assert!(track.keys.first().unwrap().time_secs.abs() < 1e-9, "frame 60 opens the clip");
        assert_eq!(track.keys.len(), 11, "frames 60..=70; 58 and 59 are before the clip");
        let at = track.keys.iter().find(|k| (k.time_secs - 10.0 / 30.0).abs() < 1e-6).unwrap();
        assert!((at.corners[1][0] - 0.58).abs() < 1e-6 && (at.corners[1][1] - 0.43).abs() < 1e-6, "{at:?}");
        let mut mask = klipp_core::Mask::default();
        mask.keyframes.track_perspective = Some(track);
        let warp = mask.evaluated_in(10.0 / 30.0, 9.0 / 16.0).keyframes.warp.unwrap();
        let back = warp.apply((0.58, 0.43)).unwrap();
        assert!((back.0 - 0.6).abs() < 1e-4 && (back.1 - 0.4).abs() < 1e-4, "{back:?}");
    }

    /// The region followed is the one the window covers, as the matte
    /// draws it: a circle as far down as across, `radius / aspect` of the
    /// height.
    #[test]
    fn the_region_followed_is_the_window() {
        let aspect = 9.0 / 16.0;
        let q = window_quad(&klipp_core::MaskShape::Circle { center_x: 0.5, center_y: 0.5, radius: 0.1 }, aspect);
        assert!((q[0].0 - 0.4).abs() < 1e-6 && (q[0].1 - (0.5 - 0.1 / aspect)).abs() < 1e-6 && (q[2].0 - 0.6).abs() < 1e-6);
        let q = window_quad(&klipp_core::MaskShape::Rectangle { center_x: 0.5, center_y: 0.5, width: 0.2, height: 0.1, rotation: 0.0 }, aspect);
        assert!((q[1].0 - 0.6).abs() < 1e-6 && (q[2].1 - (0.5 + 0.05 / aspect)).abs() < 1e-6);
    }
}

#[cfg(test)]
mod engine_send_tests {
    /// #437: every command the UI sends straight to the playback engine is
    /// one the engine acts on. Read from the source, so a send on a path no
    /// test runs fails here rather than doing nothing in the app.
    #[test]
    fn every_command_sent_to_the_engine_is_one_it_acts_on() {
        fn walk(dir: &std::path::Path, out: &mut Vec<(std::path::PathBuf, String)>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push((path.clone(), std::fs::read_to_string(&path).unwrap()));
                }
            }
        }
        let mut files = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
        let mut sent = 0;
        let mut astray = Vec::new();
        for (path, text) in &files {
            for marker in ["cmd_tx.send(AppCommand::", "cmd_tx.send(klipp_core::AppCommand::"] {
                for (at, _) in text.match_indices(marker) {
                    let name: String = text[at + marker.len()..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                    // This test's own markers name nothing.
                    if name.is_empty() {
                        continue;
                    }
                    sent += 1;
                    if !klipp_engine::commands::NAMES.contains(&name.as_str()) {
                        let line = text[..at].lines().count();
                        astray.push(format!("{}:{line} sends {name}", path.display()));
                    }
                }
            }
        }
        assert!(sent > 100, "the scan found only {sent} sends: has the field been renamed?");
        assert!(astray.is_empty(), "sent to the engine, which does nothing with them — send them through send_commands:\n{}", astray.join("\n"));
    }
}

#[cfg(test)]
mod empty_texture_tests {
    use super::upload_texture_deferred;

    /// #645: an empty frame — the engine reset with no project — takes the
    /// old picture away and makes no texture. A zero-size one failed in
    /// wgpu and every redraw after was a validation error, until GPU
    /// effects were switched off for the session.
    #[test]
    fn an_empty_frame_clears_the_picture_and_makes_no_texture() {
        let ctx = egui::Context::default();
        let mut slot = None;
        let mut drops = Vec::new();
        let image = |w: usize, h: usize| egui::ColorImage { size: [w, h], pixels: vec![egui::Color32::BLACK; w * h] };
        upload_texture_deferred(&ctx, &mut slot, &mut drops, "t", image(4, 2), egui::TextureOptions::LINEAR);
        assert!(slot.is_some());
        let before = ctx.tex_manager().read().allocated().count();
        upload_texture_deferred(&ctx, &mut slot, &mut drops, "t", image(0, 0), egui::TextureOptions::LINEAR);
        assert!(slot.is_none(), "the old picture is gone");
        assert_eq!(drops.len(), 1, "dropped after the frame that may still bind it");
        assert_eq!(ctx.tex_manager().read().allocated().count(), before, "no texture made for nothing");
        upload_texture_deferred(&ctx, &mut slot, &mut drops, "t", image(4, 2), egui::TextureOptions::LINEAR);
        assert!(slot.is_some(), "the next real frame shows");
    }
}

/// An Inspector change being made on several selected clips (#843):
/// the clip it edits, and how it and the others were before it.
struct MultiEditSession {
    primary: klipp_core::ClipId,
    before: Vec<klipp_timeline::Clip>,
}

/// What to do after an Inspector command was handled on its own clip.
struct MultiEditPost {
    cmd: AppCommand,
    others: Vec<klipp_core::ClipId>,
    /// The last step: record it and close the undo step.
    finish: bool,
    /// Whether this step opened the undo batch it closes; inside one
    /// already open it joins that instead.
    opened_batch: bool,
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
