use crate::history::{EditCommand, HistoryEntry, KeyframeSnapshot};
use crate::icons::Icon;
use crate::layout_state::LayoutState;
use crate::panels::inspector::InspectorPanel;
use crate::panels::markers::MarkersPanel;
use crate::browser::BrowserPanel;
use crate::inspector::NewInspectorPanel;
use crate::panels::preferences::PreferencesPanel;
use crate::panels::timeline::TimelinePanel;
use crate::panels::toolbar::ToolbarPanel;
use crate::panels::transport::TransportBar;
use crate::panels::viewer::ViewerPanel;
use crate::state::{AppState, ClipboardContent, UnsavedAction};
use crate::theme::{self, KlippTheme};
use crate::widgets::resizer;
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{
    AppCommand, AppEvent, ClipId, GroupId, ExportAudioCodec, ExportFormat, ExportPreset,
    EncoderPreset, ExportPresetSettings, ExportSettings, ExportVideoCodec, MarkerId, MediaId,
    Timecode, Tool, ViewerMode, builtin_presets,
};
use klipp_engine::{PlaybackState, SharedFrameBuffer};
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

struct ExportDialogState {
    output_path: String,
    format: ExportFormat,
    video_codec: ExportVideoCodec,
    crf: u32,
    preset: EncoderPreset,
    audio_codec: ExportAudioCodec,
    audio_bitrate: u32,
    confirm_overwrite: bool,
    marked_range: bool,
    selected_preset_id: Option<String>,
    save_preset_name: String,
    show_save_preset: bool,
}

impl Default for ExportDialogState {
    fn default() -> Self {
        Self {
            output_path: String::new(),
            format: ExportFormat::Mp4,
            video_codec: ExportVideoCodec::H264,
            crf: 23,
            preset: EncoderPreset::default(),
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate: 128,
            confirm_overwrite: false,
            marked_range: false,
            selected_preset_id: None,
            save_preset_name: String::new(),
            show_save_preset: false,
        }
    }
}

pub struct KlippApp {
    cmd_tx: Sender<AppCommand>,
    event_rx: Receiver<AppEvent>,
    frame_buffer: SharedFrameBuffer,
    sequence: Arc<RwLock<Sequence>>,
    media_pool_shared: Arc<RwLock<klipp_media::MediaPool>>,
    state: AppState,
    toolbar: ToolbarPanel,
    viewer: ViewerPanel,
    title_renderer: klipp_title::TitleRenderer,
    #[allow(dead_code)]
    inspector: InspectorPanel, // kept for potential fallback; old code fully superseded by new_inspector
    markers: MarkersPanel,
    preferences_panel: PreferencesPanel,
    transport: TransportBar,
    timeline: TimelinePanel,
    thumb_tx: Sender<ThumbnailResult>,
    thumb_rx: Receiver<ThumbnailResult>,
    filmstrip_tx: Sender<FilmstripResult>,
    filmstrip_rx: Receiver<FilmstripResult>,
    audio_conform_tx: Sender<AudioConformResult>,
    audio_conform_rx: Receiver<AudioConformResult>,
    /// Track proxy ffmpeg child PIDs so we can kill them on exit
    proxy_children: Vec<Arc<std::sync::atomic::AtomicU32>>,
    /// Channel for proxy generation events (started/complete/failed)
    proxy_event_tx: Sender<AppEvent>,
    proxy_event_rx: Receiver<AppEvent>,
    // Export dialog state
    export_dialog: ExportDialogState,
    // Preferences
    prefs: crate::prefs::Preferences,
    prefs_dirty: bool,
    prefs_last_save: std::time::Instant,
    // Proxy progress channel: (media_id, progress 0.0..1.0)
    proxy_progress_tx: Sender<(MediaId, f32)>,
    proxy_progress_rx: Receiver<(MediaId, f32)>,
    // Auto-save
    last_autosave: std::time::Instant,
    // GUI redesign
    theme: KlippTheme,
    layout: LayoutState,
    layout_dirty: bool,
    browser: BrowserPanel,
    new_inspector: NewInspectorPanel,
    new_timeline: crate::timeline::NewTimelinePanel,
    // Skim audio — shared with engine's cpal callback
    skim_consumer: klipp_engine::audio_output::SkimConsumer,
    /// cpal device sample rate (for skim audio resampling)
    cpal_sample_rate: u32,
    // Launch screen
    launch_screen: crate::panels::launch_screen::LaunchScreen,
    // New Project dialog
    new_project_dialog: crate::panels::new_project_dialog::NewProjectDialog,
    // Node graph and inspector panels (Color workspace)
    node_graph_panel: crate::panels::node_graph::NodeGraphPanel,
    node_inspector_panel: crate::panels::node_inspector::NodeInspectorPanel,
    /// CPU-only graph executor for UI-thread re-grading (instant slider feedback).
    ui_executor: klipp_nodes::GraphExecutor,
    /// File watcher for external media modification detection.
    /// Must be kept alive — dropping stops watching.
    #[allow(dead_code)]
    media_watcher: Option<notify::RecommendedWatcher>,
    // Curve editor panel (spline editor for keyframe animation)
    curve_editor: crate::panels::curve_editor::CurveEditorPanel,
}

impl KlippApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cmd_tx: Sender<AppCommand>,
        event_rx: Receiver<AppEvent>,
        frame_buffer: SharedFrameBuffer,
        sequence: Arc<RwLock<Sequence>>,
        media_pool_shared: Arc<RwLock<klipp_media::MediaPool>>,
        skim_consumer: klipp_engine::audio_output::SkimConsumer,
    ) -> Self {
        let (thumb_tx, thumb_rx) = crossbeam_channel::unbounded();
        let (filmstrip_tx, filmstrip_rx) = crossbeam_channel::unbounded();
        let (audio_conform_tx, audio_conform_rx) = crossbeam_channel::unbounded();
        let (proxy_event_tx, proxy_event_rx) = crossbeam_channel::unbounded();
        let (proxy_progress_tx, proxy_progress_rx) = crossbeam_channel::unbounded();
        let theme = KlippTheme::default();
        theme.apply_to_egui(&cc.egui_ctx);
        theme.apply_fonts(&cc.egui_ctx);
        crate::icons::install(&cc.egui_ctx);
        let layout = LayoutState::load();

        let mut app = Self {
            cmd_tx,
            event_rx,
            frame_buffer,
            sequence,
            media_pool_shared,
            state: AppState::default(),
            toolbar: ToolbarPanel,
            viewer: ViewerPanel::default(),
            title_renderer: klipp_title::TitleRenderer::new(),
            inspector: InspectorPanel::default(),
            markers: MarkersPanel::default(),
            preferences_panel: PreferencesPanel::default(),
            transport: TransportBar,
            timeline: {
                let mut tl = TimelinePanel::default();
                tl.embedded = true;
                tl
            },
            thumb_tx,
            thumb_rx,
            filmstrip_tx,
            filmstrip_rx,
            audio_conform_tx,
            audio_conform_rx,
            proxy_children: Vec::new(),
            proxy_event_tx,
            proxy_event_rx,
            export_dialog: ExportDialogState::default(),
            prefs: crate::prefs::Preferences::load(),
            prefs_dirty: false,
            prefs_last_save: std::time::Instant::now(),
            proxy_progress_tx,
            proxy_progress_rx,
            last_autosave: std::time::Instant::now(),
            theme,
            layout,
            layout_dirty: false,
            browser: BrowserPanel::default(),
            new_inspector: NewInspectorPanel::default(),
            new_timeline: crate::timeline::NewTimelinePanel,
            skim_consumer,
            cpal_sample_rate: klipp_engine::audio_output::probe_sample_rate(),
            launch_screen: crate::panels::launch_screen::LaunchScreen::default(),
            new_project_dialog: crate::panels::new_project_dialog::NewProjectDialog::new(None),
            node_graph_panel: crate::panels::node_graph::NodeGraphPanel::default(),
            node_inspector_panel: crate::panels::node_inspector::NodeInspectorPanel::default(),
            ui_executor: klipp_nodes::GraphExecutor::new_cpu_only(),
            media_watcher: None,
            curve_editor: crate::panels::curve_editor::CurveEditorPanel::new(),
        };
        // Sync preferences to state
        app.state.audio_skimming_enabled = app.prefs.playback.skim_audio;
        app.state.loop_playback_enabled = app.prefs.playback.loop_playback;
        app.state.auto_create_tracks = app.prefs.editing.auto_create_tracks;
        app.state.history.set_max_size(app.prefs.editing.undo_limit);
        app.state.audio_fade_overlay_mode = app.prefs.audio.fade_overlay_mode;
        app.state.audio_show_rubber_band = app.prefs.audio.show_rubber_band;
        app.state.audio_show_waveform = app.prefs.audio.show_waveform;
        app.state.audio_anti_click_fade = app.prefs.audio.anti_click_fade;
        app.state.audio_show_crossfade_indicators = app.prefs.audio.show_crossfade_indicators;
        app.browser.browser_view_mode = app.prefs.editing.media_browser_view_mode;
        // Sync loop state to engine (engine defaults to loop_enabled=false)
        if app.state.loop_playback_enabled {
            let _ = app.cmd_tx.send(AppCommand::SetLoopPlayback(true));
        }
        // Sync anti-click fade to engine
        let _ = app.cmd_tx.send(AppCommand::SetAntiClickFade(app.state.audio_anti_click_fade));

        // Discover system fonts for title clip font selector
        let font_registry = klipp_title::font_discovery::discover_system_fonts();
        app.state.available_fonts = font_registry.keys().cloned().collect();
        tracing::info!("UI font discovery: {} families available", app.state.available_fonts.len());

        // Enrich any stale recent project entries with metadata from disk
        let mut prefs_changed = app.prefs.enrich_recent_projects();

        // Generate poster frame thumbnails for recent projects that don't have one
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

        if prefs_changed {
            let _ = app.prefs.save();
        }

        // Discover OFX plugins at startup
        let mut ofx_host = klipp_plugin::OfxHost::new();
        let count = ofx_host.discover_plugins();
        app.state.available_plugins = ofx_host.plugin_infos();
        app.state.effect_registry.merge_ofx_plugins(&app.state.available_plugins);
        tracing::info!("OFX: discovered {} plugin(s), {} total effects in registry", count, app.state.effect_registry.effects.len());
        drop(ofx_host); // UI doesn't need the host — engine has its own
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
        if now.duration_since(self.state.perf_last_update).as_millis() >= 250 {
            self.state.perf_last_update = now;

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
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::PlaybackStarted => {
                    self.state.playback = PlaybackState::Playing;
                    self.state.playhead_anchor = self.state.playhead;
                    self.state.playhead_anchor_wall = std::time::Instant::now();
                    // Re-engage playhead follow on play (user scroll disables it)
                    if self.state.playhead_follow == crate::state::PlayheadFollow::Off {
                        self.state.playhead_follow = crate::state::PlayheadFollow::Page;
                    }
                }
                AppEvent::PlaybackPaused => {
                    self.state.playback = PlaybackState::Paused;
                }
                AppEvent::PlaybackStopped => {
                    self.state.playback = PlaybackState::Stopped;
                }
                AppEvent::LoopedBack => {
                    // Engine looped back — the following PlayheadMoved carries the new position.
                    // Pre-reset smooth playhead so it doesn't animate from old position.
                    // The PlayheadMoved handler will set the final anchor.
                }
                AppEvent::PlayheadMoved(tc) => {
                    if self.state.playback == PlaybackState::Playing {
                        // During playback: only update anchor, don't touch playhead
                        self.state.playhead_anchor = tc;
                        self.state.playhead_anchor_wall = std::time::Instant::now();
                    } else {
                        // When not playing: set playhead directly
                        self.state.playhead = tc;
                        self.state.playhead_smooth_secs = tc.to_secs_f64();
                        self.state.playhead_anchor = tc;
                        self.state.playhead_anchor_wall = std::time::Instant::now();
                    }
                }
                AppEvent::MediaLoaded {
                    media_id,
                    path,
                    duration,
                    fps,
                    resolution,
                    video_codec,
                    audio_codec,
                    has_audio,
                    file_size_bytes,
                    sample_rate,
                    audio_channels,
                    is_still,
                    file_modified_at,
                    media_created_at,
                    colour_space,
                } => {
                    let filename = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());
                    let _is_audio_only = video_codec.is_empty();

                    // For still images: override duration and fps with defaults
                    let (duration, fps, is_still) = if is_still {
                        let still_dur_secs = self.prefs.editing.default_still_duration_secs;
                        let still_fps = klipp_core::Fps::pal_25(); // sensible default
                        let still_duration = klipp_core::RationalTime::from_secs_f64(
                            still_dur_secs,
                            still_fps.rate(),
                        );
                        (still_duration, still_fps, true)
                    } else {
                        (duration, fps, false)
                    };

                    let media_item = MediaItem {
                        id: media_id,
                        path: path.clone(),
                        filename,
                        duration,
                        fps,
                        resolution,
                        video_codec,
                        audio_codec,
                        has_audio,
                        file_size_bytes,
                        sample_rate,
                        audio_channels,
                        offline: false,
                        is_still,
                        proxy_path: None,
                        proxy_ready: false,
                        default_source_in: None,
                        default_source_out: None,
                        rating: 0,
                        tags: Vec::new(),
                        user_notes: String::new(),
                        bin: None,
                        bin_id: None,
                        added_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        fingerprint: compute_media_fingerprint(&path),
                        file_modified_at,
                        media_created_at,
                        color_label: Default::default(),
                        colour_space: klipp_media::ColourSpace::from_u8(colour_space),
                        needs_reprobe: false,
                        log_format: Default::default(),
                        skim_file: None,
                        waveform_data: None,
                        audio_cache: None,
                    };

                    self.state.media_pool.add(media_item.clone());
                    self.state.history.push(crate::history::HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
                        command: crate::history::EditCommand::AddMediaToPool { item: media_item },
                    });
                    self.state.dirty = true;
                    self.sync_media_pool_to_engine();
                    self.state.fps = fps;
                    self.state.playback = PlaybackState::Stopped;

                    // Clear pending path — media goes to pool only.
                    // User adds to timeline via double-click or drag-and-drop.
                    if self.state.pending_media_path.is_some() {
                        self.state.pending_media_path = None;
                    }

                    // Start thumbnail (always) + optional filmstrip/audio/skim generation
                    self.spawn_thumbnail(media_id, path.clone(), duration);
                    let filmstrips_enabled = self.state.project_auto_filmstrips
                        .unwrap_or(self.prefs.editing.auto_generate_filmstrips);
                    let skim_enabled = self.state.project_auto_skim
                        .unwrap_or(self.prefs.editing.auto_generate_skim);
                    if filmstrips_enabled {
                        self.spawn_filmstrip(media_id, path.clone(), duration);
                        self.state.filmstrip_generating = true;
                        self.state.filmstrip_generate_start = std::time::Instant::now();
                        self.state.filmstrip_toast_dismissed = false;
                    }
                    if has_audio {
                        self.spawn_audio_conform(media_id, path.clone());
                    }
                    if skim_enabled {
                        self.state.pending_skim_transcodes.push(media_id);
                    }

                    // Don't auto-generate proxies — CUDA NVDEC + 1080p downscale
                    // handles playback fine. Proxies can be requested manually via
                    // the GenerateProxy command if needed.
                }
                AppEvent::MediaLoadFailed { media_id: _, error } => {
                    self.state.pending_media_path = None;
                    tracing::error!("Failed to load media: {error}");
                }
                AppEvent::FrameReady => {
                    self.upload_frame_to_egui(ctx);
                    self.state.perf_frame_count += 1;
                    ctx.request_repaint();
                }
                AppEvent::ClipAppended { .. }
                | AppEvent::ClipRemoved { .. }
                | AppEvent::ClipMoved { .. }
                | AppEvent::ClipTrimmed { .. }
                | AppEvent::ClipSelected(_) => {}
                AppEvent::ProjectSaved { .. }
                | AppEvent::ProjectLoaded { .. }
                | AppEvent::ProjectError { .. } => {}
                AppEvent::ExportProgress { .. }
                | AppEvent::ExportComplete { .. }
                | AppEvent::ExportError { .. } => {
                    // Export events come through export_event_rx channel
                }
                AppEvent::ThumbnailReady { .. } => {
                    // Thumbnails come through thumb_rx channel instead
                }
                AppEvent::EngineStats { dropped_frames, hw_decode } => {
                    self.state.perf_dropped_frames = dropped_frames;
                    self.state.perf_hw_decode = hw_decode;
                }
                AppEvent::SourceFrameReady
                | AppEvent::SourceMediaLoaded { .. }
                | AppEvent::ProxyGenerationStarted { .. }
                | AppEvent::ProxyGenerationComplete { .. }
                | AppEvent::ProxyGenerationFailed { .. }
                | AppEvent::AudioConformStarted { .. }
                | AppEvent::AudioConformComplete { .. }
                | AppEvent::AudioConformFailed { .. } => {
                    // Audio conform events come through audio_conform_rx channel
                }
                AppEvent::TrackPeakLevels { track_index, left, right } => {
                    // Grow peak levels vec if needed
                    while self.state.track_peak_levels.len() <= track_index {
                        self.state.track_peak_levels.push((0.0, 0.0));
                    }
                    let (ref mut dl, ref mut dr) = self.state.track_peak_levels[track_index];
                    // Peak hold: only update if new value is higher (decay happens per-frame)
                    *dl = dl.max(left);
                    *dr = dr.max(right);
                }
                AppEvent::MasterPeakLevels { left, right } => {
                    self.state.master_peak_left = self.state.master_peak_left.max(left);
                    self.state.master_peak_right = self.state.master_peak_right.max(right);
                }
                // Skim events come through proxy_event_rx, handled in drain_proxy_events
                AppEvent::SkimTranscodeProgress { .. }
                | AppEvent::SkimTranscodeComplete { .. }
                | AppEvent::SkimTranscodeFailed { .. }
                | AppEvent::BrowserSkimFrameReady { .. }
                | AppEvent::MediaFileChanged { .. } => {}
            }
        }
    }

    fn spawn_thumbnail(
        &mut self,
        media_id: MediaId,
        path: PathBuf,
        duration: Timecode,
    ) {
        self.state.thumbnail_cache.set_pending(media_id);
        let tx = self.thumb_tx.clone();
        let duration_secs = duration.to_secs_f64();
        let seek_secs = duration_secs * 0.1;
        let cache_key = klipp_media::disk_cache::compute_cache_key(&path);
        std::thread::spawn(move || {
            // Try loading from disk cache first
            if let Some(ref key) = cache_key {
                if klipp_media::disk_cache::thumbnail_cache_exists(key) {
                    match klipp_media::disk_cache::load_thumbnail_cache(key) {
                        Ok((rgba, w, h)) => {
                            tracing::info!("Thumbnail loaded from cache for {:?}", path);
                            let _ = tx.send(ThumbnailResult {
                                media_id,
                                rgba,
                                width: w,
                                height: h,
                            });
                            return;
                        }
                        Err(e) => {
                            tracing::warn!("Thumbnail cache load failed, regenerating: {e}");
                        }
                    }
                }
            }

            // Generate from source
            match klipp_engine::generate_thumbnail_non_black(&path, seek_secs, duration_secs) {
                Ok((rgba, w, h)) => {
                    // Write to cache
                    if let Some(ref key) = cache_key {
                        if let Err(e) = klipp_media::disk_cache::write_thumbnail_cache(key, &rgba, w, h) {
                            tracing::warn!("Failed to write thumbnail cache: {e}");
                        }
                    }
                    let _ = tx.send(ThumbnailResult {
                        media_id,
                        rgba,
                        width: w,
                        height: h,
                    });
                }
                Err(e) => {
                    tracing::warn!("Thumbnail generation failed for {:?}: {}", media_id, e);
                }
            }
        });
    }

    /// Queue a filmstrip for generation. Deduplicates and serializes — only one
    /// filmstrip generates at a time to avoid exhausting GPU NVDEC sessions.
    fn spawn_filmstrip(
        &mut self,
        media_id: MediaId,
        path: PathBuf,
        duration: Timecode,
    ) {
        // Deduplicate: skip if already in progress or already queued
        if self.state.filmstrip_in_progress.contains(&media_id) {
            return;
        }
        if self.state.filmstrip_queue.iter().any(|(id, _, _)| *id == media_id) {
            return;
        }
        // Already have filmstrip frames in memory? Skip.
        if self.state.thumbnail_cache.filmstrip(&media_id).is_some_and(|f| !f.is_empty()) {
            return;
        }

        self.state.filmstrip_queue.push((media_id, path, duration));
        self.state.filmstrip_total_queued += 1;

        // Pump the queue if nothing is currently generating
        self.pump_filmstrip_queue();
    }

    /// Start the next filmstrip generation if nothing is in progress.
    fn pump_filmstrip_queue(&mut self) {
        if !self.state.filmstrip_in_progress.is_empty() {
            return; // One is already running
        }
        let Some((media_id, path, duration)) = self.state.filmstrip_queue.first().cloned() else {
            return;
        };
        self.state.filmstrip_queue.remove(0);
        self.state.filmstrip_in_progress.insert(media_id);

        let tx = self.filmstrip_tx.clone();
        let duration_secs = duration.to_secs_f64();
        let cache_key = klipp_media::disk_cache::compute_cache_key(&path);
        let generate_if_missing = self.state.project_auto_filmstrips
            .unwrap_or(self.prefs.editing.auto_generate_filmstrips);
        let interval = (duration_secs / 400.0).clamp(0.2, 1.5);
        std::thread::Builder::new()
            .name("klipp-filmstrip".into())
            .spawn(move || {
                // Try loading from disk cache first
                if let Some(ref key) = cache_key {
                    if klipp_media::disk_cache::filmstrip_cache_exists(key) {
                        match klipp_media::disk_cache::load_filmstrip_cache(key) {
                            Ok(frames) => {
                                tracing::info!("Filmstrip loaded from cache: {} frames for {:?}", frames.len(), path);
                                for frame in frames {
                                    let _ = tx.send(FilmstripResult::Frame {
                                        media_id,
                                        frame,
                                    });
                                }
                                let _ = tx.send(FilmstripResult::Done { media_id });
                                return;
                            }
                            Err(e) => {
                                tracing::warn!("Filmstrip cache load failed, regenerating: {e}");
                            }
                        }
                    }
                }

                // Skip fresh generation if preference is off (cache was already checked above)
                if !generate_if_missing {
                    tracing::info!("Filmstrip generation disabled by preference, skipping {:?}", path);
                    let _ = tx.send(FilmstripResult::Done { media_id });
                    return;
                }

                // Generate from source and collect frames for cache write
                let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
                let frames_clone = frames.clone();
                match klipp_engine::generate_filmstrip_incremental(&path, duration_secs, interval, 48, |time, rgba, w, h| {
                    let frame = klipp_media::FilmstripFrame {
                        time_secs: time,
                        rgba,
                        width: w,
                        height: h,
                    };
                    frames_clone.lock().unwrap().push(frame.clone());
                    let _ = tx.send(FilmstripResult::Frame {
                        media_id,
                        frame,
                    });
                }) {
                    Ok(count) => {
                        tracing::info!("Filmstrip: generated {count} frames for {:?}", path);
                        // Write to cache
                        if let Some(ref key) = cache_key {
                            let cached_frames = frames.lock().unwrap();
                            if let Err(e) = klipp_media::disk_cache::write_filmstrip_cache(key, &cached_frames) {
                                tracing::warn!("Failed to write filmstrip cache: {e}");
                            } else {
                                tracing::info!("Filmstrip cache written: {} frames", cached_frames.len());
                            }
                        }
                        let _ = tx.send(FilmstripResult::Done { media_id });
                    }
                    Err(e) => {
                        tracing::warn!("Filmstrip generation failed for {:?}: {}", media_id, e);
                        let _ = tx.send(FilmstripResult::Done { media_id });
                    }
                }
            })
            .ok();
    }

    fn spawn_audio_conform(&mut self, media_id: MediaId, path: PathBuf) {
        let tx = self.audio_conform_tx.clone();
        // Use XDG cache dir for audio cache (same area as proxies)
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("klipp")
            .join("audio-cache");
        let cache_key = klipp_media::disk_cache::compute_cache_key(&path);
        std::thread::Builder::new()
            .name("klipp-audio-conform".into())
            .spawn(move || {
                // Ensure cache directory exists
                if let Err(e) = std::fs::create_dir_all(&cache_dir) {
                    tracing::warn!("Failed to create audio cache dir: {e}");
                    let _ = tx.send(AudioConformResult::Failed {
                        media_id,
                        error: format!("cache dir: {e}"),
                    });
                    return;
                }

                // Check content-based cache first, then fall back to MediaId-based
                let (exists, pcm_path, peaks_path) = if let Some(ref key) = cache_key {
                    klipp_media::disk_cache::audio_conform_cache_exists(key, media_id)
                } else {
                    // No cache key (offline file?) — use old MediaId-based paths
                    let pcm = cache_dir.join(format!("{media_id}.pcm"));
                    let peaks = cache_dir.join(format!("{media_id}.peaks"));
                    let exists = pcm.exists() && peaks.exists();
                    (exists, pcm, peaks)
                };

                if exists {
                    tracing::info!("Audio cache hit for {:?}", path);
                    match klipp_media::audio_cache::load_peaks(&peaks_path) {
                        Ok(peaks) => {
                            let pcm_size = std::fs::metadata(&pcm_path)
                                .map(|m| m.len())
                                .unwrap_or(0);
                            let duration_secs = pcm_size as f64
                                / (48000.0 * 2.0 * 4.0);
                            let _ = tx.send(AudioConformResult::Complete {
                                media_id,
                                entry: klipp_media::AudioCacheEntry {
                                    media_id,
                                    pcm_path,
                                    peaks_path,
                                    sample_rate: 48000,
                                    channels: 2,
                                    duration_secs,
                                    peaks,
                                },
                            });
                        }
                        Err(e) => {
                            tracing::warn!("Failed to load cached peaks, re-conforming: {e}");
                            do_conform(media_id, &path, &cache_dir, &cache_key, &tx);
                        }
                    }
                    return;
                }

                do_conform(media_id, &path, &cache_dir, &cache_key, &tx);
            })
            .ok();

        fn do_conform(
            media_id: MediaId,
            path: &Path,
            cache_dir: &Path,
            cache_key: &Option<String>,
            tx: &Sender<AudioConformResult>,
        ) {
            // Use content-based key for filenames if available, else MediaId
            let file_key = cache_key.as_deref()
                .map(|k| k.to_string())
                .unwrap_or_else(|| media_id.to_string());
            let pcm_path = cache_dir.join(format!("{file_key}.pcm"));
            let peaks_path = cache_dir.join(format!("{file_key}.peaks"));

            match klipp_media::audio_cache::conform_audio_to_paths(media_id, path, &pcm_path, &peaks_path) {
                Ok(entry) => {
                    let _ = tx.send(AudioConformResult::Complete { media_id, entry });
                }
                Err(e) => {
                    tracing::warn!("Audio conforming failed for {:?}: {e}", media_id);
                    let _ = tx.send(AudioConformResult::Failed {
                        media_id,
                        error: e.to_string(),
                    });
                }
            }
        }
    }

    fn drain_audio_conforms(&mut self) {
        let mut changed = false;
        while let Ok(result) = self.audio_conform_rx.try_recv() {
            match result {
                AudioConformResult::Complete { media_id, entry } => {
                    tracing::info!(
                        "Audio conform complete for {:?}: {:.1}s, {} peaks",
                        media_id,
                        entry.duration_secs,
                        entry.peaks.len(),
                    );
                    if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                        // Use conform peaks as waveform data for timeline display
                        if item.waveform_data.is_none() {
                            item.waveform_data = Some(entry.peaks.clone());
                        }
                        item.audio_cache = Some(entry);
                        changed = true;
                    }
                }
                AudioConformResult::Failed { media_id, error } => {
                    tracing::warn!("Audio conform failed for {:?}: {}", media_id, error);
                }
            }
        }
        if changed {
            self.sync_media_pool_to_engine();
        }
    }

    fn spawn_proxy_generation(&mut self, media_id: MediaId) {
        let item = match self.state.media_pool.get(&media_id) {
            Some(item) => item.clone(),
            None => return,
        };
        if item.proxy_ready {
            return;
        }
        if self.state.proxy_generating.contains(&media_id) {
            tracing::info!("Proxy already generating for {:?}, skipping", media_id);
            return;
        }
        let cache_dir = self.prefs.proxy.proxy_cache_dir.clone().unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("klipp")
                .join("proxies")
        });
        let _ = std::fs::create_dir_all(&cache_dir);
        let proxy_path = cache_dir.join(format!("{}.mp4", media_id));
        if proxy_path.exists() {
            let file_size = std::fs::metadata(&proxy_path).map(|m| m.len()).unwrap_or(0);
            tracing::info!(
                "Proxy already exists for {}: {} ({:.1} MB)",
                item.filename, proxy_path.display(), file_size as f64 / (1024.0 * 1024.0)
            );
            if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                item.proxy_path = Some(proxy_path);
                item.proxy_ready = true;
            }
            return;
        }
        let res_label = self.prefs.proxy.proxy_resolution.label();
        let scale_arg = self.prefs.proxy.proxy_resolution.ffmpeg_scale_arg().to_string();
        tracing::info!(
            "Generating proxy for {} — resolution: {}, scale: {}, output: {}",
            item.filename, res_label, scale_arg, proxy_path.display()
        );
        self.state.proxy_generating.insert(media_id);
        self.state.proxy_start_times.insert(media_id, std::time::Instant::now());
        self.state.proxy_toast_dismissed = false; // Show toast for new proxy
        self.state.proxy_status_message = Some((
            format!("Generating proxy for {}... ({})", item.filename, res_label),
            std::time::Instant::now(),
        ));
        let source_path = item.path.clone();
        let total_duration_secs = item.duration.to_secs_f64();
        let tx = self.proxy_event_tx.clone();
        let progress_tx = self.proxy_progress_tx.clone();
        let proxy_out = proxy_path.clone();
        let child_pid: Arc<std::sync::atomic::AtomicU32> = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let pid_clone = child_pid.clone();
        self.proxy_children.push(child_pid);
        std::thread::spawn(move || {
            let _ = tx.send(AppEvent::ProxyGenerationStarted { media_id });

            // Helper: spawn ffmpeg with progress tracking, wait for result
            let run_ffmpeg = |args: &[&str], pid_ref: &Arc<std::sync::atomic::AtomicU32>,
                              ptx: &Sender<(MediaId, f32)>, dur: f64|
                -> Result<(), String> {
                let mut cmd = std::process::Command::new("ffmpeg");
                cmd.arg("-i").arg(&source_path);
                for a in args { cmd.arg(a); }
                cmd.arg("-progress").arg("pipe:1")
                   .arg("-y").arg(&proxy_out)
                   .stdout(std::process::Stdio::piped())
                   .stderr(std::process::Stdio::piped());
                let mut child = cmd.spawn().map_err(|e| e.to_string())?;
                pid_ref.store(child.id(), std::sync::atomic::Ordering::Relaxed);
                let stdout = child.stdout.take();
                let ptx2 = ptx.clone();
                let reader_handle = std::thread::spawn(move || {
                    if let Some(stdout) = stdout {
                        use std::io::BufRead;
                        let reader = std::io::BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if let Some(time_str) = line.strip_prefix("out_time_us=") {
                                if let Ok(us) = time_str.trim().parse::<i64>() {
                                    let secs = us as f64 / 1_000_000.0;
                                    let pct = if dur > 0.0 {
                                        (secs / dur).min(1.0) as f32
                                    } else { 0.0 };
                                    let _ = ptx2.send((media_id, pct));
                                }
                            }
                        }
                    }
                });
                let status = child.wait().map_err(|e| e.to_string())?;
                let _ = reader_handle.join();
                pid_ref.store(0, std::sync::atomic::Ordering::Relaxed);
                if status.success() { Ok(()) } else { Err("ffmpeg exited with error".into()) }
            };

            // Try GPU (NVENC) first — much faster on NVIDIA GPUs
            let nvenc_args: Vec<&str> = vec![
                "-vf", &scale_arg,
                "-c:v", "h264_nvenc", "-preset", "p1", "-cq", "30",
                "-c:a", "aac", "-b:a", "64k",
            ];
            let result = run_ffmpeg(&nvenc_args, &pid_clone, &progress_tx, total_duration_secs);

            let result = match result {
                Ok(()) => {
                    tracing::info!("Proxy generated with NVENC (GPU)");
                    Ok(())
                }
                Err(nvenc_err) => {
                    tracing::info!("NVENC failed ({}), retrying with CPU (libx264)", nvenc_err);
                    let _ = std::fs::remove_file(&proxy_out); // clean up partial file
                    let _ = progress_tx.send((media_id, 0.0)); // reset progress
                    let cpu_args: Vec<&str> = vec![
                        "-vf", &scale_arg,
                        "-c:v", "libx264", "-crf", "28", "-preset", "ultrafast",
                        "-c:a", "aac", "-b:a", "64k",
                    ];
                    let r = run_ffmpeg(&cpu_args, &pid_clone, &progress_tx, total_duration_secs);
                    if r.is_ok() { tracing::info!("Proxy generated with libx264 (CPU)"); }
                    r
                }
            };

            match result {
                Ok(()) => {
                    let _ = tx.send(AppEvent::ProxyGenerationComplete { media_id, path: proxy_out });
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::ProxyGenerationFailed { media_id, error: e });
                }
            }
        });
    }

    fn drain_thumbnails(&mut self) {
        while let Ok(result) = self.thumb_rx.try_recv() {
            self.state.thumbnail_cache.set_ready(
                result.media_id,
                result.rgba,
                result.width,
                result.height,
            );
        }
        // Process up to 50 filmstrip frames per UI tick for fast progressive updates
        for _ in 0..50 {
            match self.filmstrip_rx.try_recv() {
                Ok(FilmstripResult::Frame { media_id, frame }) => {
                    self.state.thumbnail_cache.push_filmstrip_frame(media_id, frame);
                    // Clear cached textures so they get rebuilt with the new frame
                    self.state.filmstrip_textures.remove(&media_id);
                }
                Ok(FilmstripResult::Done { media_id }) => {
                    let count = self.state.thumbnail_cache
                        .filmstrip(&media_id)
                        .map(|f| f.len())
                        .unwrap_or(0);
                    tracing::info!("Filmstrip complete for {:?}: {count} frames", media_id);
                    self.state.filmstrip_in_progress.remove(&media_id);
                    self.state.filmstrip_completed += 1;
                    if self.state.filmstrip_queue.is_empty() && self.state.filmstrip_in_progress.is_empty() {
                        self.state.filmstrip_generating = false;
                        self.state.filmstrip_total_queued = 0;
                        self.state.filmstrip_completed = 0;
                    }
                    // Start next queued filmstrip
                    self.pump_filmstrip_queue();
                }
                Err(_) => break,
            }
        }
    }

    fn poll_effect_thumbnails(&mut self, ctx: &egui::Context) {
        // Generate all effect thumbnails using the embedded reference image.
        // This runs once at startup — no viewer frame needed.
        let effects: Vec<_> = self.state.effect_registry.effects.iter()
            .map(|e| (e.name.clone(), e.source.clone()))
            .collect();
        self.state.effect_thumbnails.generate_all_if_needed(&effects);
        // Poll completed thumbnails
        self.state.effect_thumbnails.poll_and_upload(ctx);
    }

    fn upload_frame_to_egui(&mut self, ctx: &egui::Context) {
        let t_total = std::time::Instant::now();
        let mut fb = self.frame_buffer.lock().unwrap();
        if !fb.dirty {
            return;
        }

        let w = fb.width as usize;
        let h = fb.height as usize;

        if fb.data.len() != w * h * 4 {
            fb.dirty = false;
            return;
        }

        // Capture reference frame for effect thumbnails (before we take ownership).
        // Skip all-black frames (common at clip start) — check a few sample pixels.
        let has_content = fb.data.len() >= 64 && {
            let mid = fb.data.len() / 2;
            let q1 = fb.data.len() / 4;
            let q3 = 3 * fb.data.len() / 4;
            // Check R channel at several points
            fb.data[q1] > 0 || fb.data[mid] > 0 || fb.data[q3] > 0
        };
        if has_content {
            self.state.effect_registry.set_reference_frame(&fb.data, fb.width, fb.height);
        }

        // Compute curve histograms for the color panel (throttled)
        if self.state.show_color_panel {
            let hist = klipp_color::scopes::compute_histogram(&fb.data, fb.width, fb.height);
            self.state.curve_histograms = Some(hist);
        }

        // Process white balance eyedropper sample
        if let Some((nx, ny)) = self.state.wb_sample_request.take() {
            let px = (nx * (fb.width - 1) as f32) as usize;
            let py = (ny * (fb.height - 1) as f32) as usize;
            let idx = (py * fb.width as usize + px) * 4;
            if idx + 2 < fb.data.len() {
                let r = fb.data[idx] as f32 / 255.0;
                let g = fb.data[idx + 1] as f32 / 255.0;
                let b = fb.data[idx + 2] as f32 / 255.0;
                // Compute correction to neutralize the sampled color
                // Temperature controls R-B balance, Tint controls G vs (R+B)/2
                let temp = -(r - b) * 200.0;
                let tint = -(g - (r + b) / 2.0) * 200.0;
                if let Some(clip_id) = self.state.selected_clip {
                    let node_id = Self::find_or_create_node(&mut self.state, clip_id, klipp_nodes::NodeKind::ColorBalance);
                    self.cmd_tx.send(AppCommand::SetNodeParam {
                        clip_id, node_id, param: "temperature".into(),
                        value: temp.clamp(-100.0, 100.0),
                    }).ok();
                    self.cmd_tx.send(AppCommand::SetNodeParam {
                        clip_id, node_id, param: "tint".into(),
                        value: tint.clamp(-100.0, 100.0),
                    }).ok();
                }
            }
        }

        // Process qualifier eyedropper sample
        if let Some((nx, ny)) = self.state.qualifier_sample_request.take() {
            let px = (nx * (fb.width - 1) as f32) as usize;
            let py = (ny * (fb.height - 1) as f32) as usize;
            let idx = (py * fb.width as usize + px) * 4;
            if idx + 2 < fb.data.len() {
                let r = fb.data[idx] as f32 / 255.0;
                let g = fb.data[idx + 1] as f32 / 255.0;
                let b = fb.data[idx + 2] as f32 / 255.0;
                // RGB → HSL
                let max_c = r.max(g).max(b);
                let min_c = r.min(g).min(b);
                let delta = max_c - min_c;
                let l = (max_c + min_c) * 0.5;
                let s = if delta < 0.0001 {
                    0.0
                } else {
                    (delta / (1.0 - (2.0 * l - 1.0).abs())).clamp(0.0, 1.0)
                };
                let h = if delta < 0.0001 {
                    0.0
                } else if max_c == r {
                    ((g - b) / delta).rem_euclid(6.0) / 6.0
                } else if max_c == g {
                    ((b - r) / delta + 2.0) / 6.0
                } else {
                    ((r - g) / delta + 4.0) / 6.0
                };
                let hue_degrees = h * 360.0;

                if let Some(clip_id) = self.state.selected_clip {
                    // Build qualifier with sampled values + sensible defaults
                    let node_id = Self::find_or_create_node(&mut self.state, clip_id, klipp_nodes::NodeKind::Qualifier);
                    let mut qual = klipp_core::QualifierSettings::default();
                    qual.enabled = true;
                    qual.hue_center = hue_degrees;
                    qual.hue_width = 30.0;
                    qual.sat_low = (s - 0.2).max(0.0);
                    qual.sat_high = (s + 0.2).min(1.0);
                    self.cmd_tx.send(AppCommand::SetNodeQualifier {
                        clip_id, node_id, qualifier: qual,
                    }).ok();
                }
            }
        }

        // Take ownership of frame data
        let mut rgba_bytes = std::mem::take(&mut fb.data);
        let original_bytes = std::mem::take(&mut fb.original_data);
        fb.dirty = false;
        drop(fb);

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

        // Apply viewer overlay (zebra / false color)
        match self.state.viewer_overlay {
            crate::state::ViewerOverlay::Zebra => {
                let fc = self.state.frame_count as usize;
                for y in 0..h {
                    for x in 0..w {
                        let idx = (y * w + x) * 4;
                        let r = rgba_bytes[idx];
                        let g = rgba_bytes[idx + 1];
                        let b = rgba_bytes[idx + 2];
                        // Check for clipping (near black or near white)
                        let clipped = r > 235 || g > 235 || b > 235 || r < 16 || g < 16 || b < 16;
                        if clipped {
                            // Animated diagonal stripes
                            let stripe = (x + y + fc * 2) % 8 < 4;
                            if stripe {
                                // Red/yellow stripe on clipped areas
                                let is_over = r > 235 || g > 235 || b > 235;
                                if is_over {
                                    rgba_bytes[idx] = 255;
                                    rgba_bytes[idx + 1] = 0;
                                    rgba_bytes[idx + 2] = 0;
                                } else {
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
            crate::state::ViewerOverlay::FalseColor => {
                for pixel in rgba_bytes.chunks_exact_mut(4) {
                    let luma = (0.2126 * pixel[0] as f32 + 0.7152 * pixel[1] as f32 + 0.0722 * pixel[2] as f32) as u8;
                    // Standard broadcast false color palette
                    let (r, g, b) = match luma {
                        0..=10    => (0x40, 0x00, 0x80), // deep purple — black crush
                        11..=20   => (0x80, 0x00, 0xc0), // purple — very underexposed
                        21..=35   => (0x00, 0x00, 0xff), // blue — underexposed
                        36..=55   => (0x00, 0x80, 0xc0), // cyan — dark
                        56..=75   => (0x00, 0x90, 0x40), // dark green — below mid
                        76..=95   => (0x00, 0xb0, 0x00), // green — proper skin shadow
                        96..=115  => (0x40, 0xc0, 0x00), // lime — mid grey
                        116..=135 => (0x80, 0xc0, 0x00), // yellow-green — proper exposure
                        136..=165 => (0xc0, 0xc0, 0x00), // yellow — bright
                        166..=195 => (0xe0, 0x80, 0x00), // orange — highlight
                        196..=220 => (0xff, 0x40, 0x00), // red-orange — hot
                        221..=240 => (0xff, 0x00, 0x00), // red — near clip
                        241..=255 => (0xff, 0x40, 0x80), // pink-white — clipped
                    };
                    pixel[0] = r;
                    pixel[1] = g;
                    pixel[2] = b;
                }
            }
            crate::state::ViewerOverlay::None => {}
        }

        let t_convert = std::time::Instant::now();
        // Safety: Color32 is repr(C) [u8; 4], same as our RGBA layout.
        // Vec reinterpret: same alignment (1), element size (4), length and capacity divide evenly.
        let pixels = {
            let mut bytes = std::mem::ManuallyDrop::new(rgba_bytes);
            let ptr = bytes.as_mut_ptr() as *mut egui::Color32;
            let len = bytes.len() / 4;
            let cap = bytes.capacity() / 4;
            unsafe { Vec::from_raw_parts(ptr, len, cap) }
        };
        let convert_ms = t_convert.elapsed().as_secs_f64() * 1000.0;

        let t_texture = std::time::Instant::now();
        let image = egui::ColorImage {
            size: [w, h],
            pixels,
        };

        // Always recreate texture to avoid stale handles after fullscreen transitions
        self.viewer.video_texture = Some(ctx.load_texture(
            "klipp-video-frame",
            image,
            egui::TextureOptions::LINEAR,
        ));
        let texture_ms = t_texture.elapsed().as_secs_f64() * 1000.0;

        let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
        if total_ms > 2.0 {
            tracing::info!(
                "PERF upload_frame: {w}x{h} total={total_ms:.1}ms (convert={convert_ms:.1}ms texture={texture_ms:.1}ms)"
            );
        }
    }

    /// Find an existing node of the given kind in the selected clip's node graph,
    /// or create one if it doesn't exist. Returns the node's UUID.
    fn find_or_create_node(
        state: &mut crate::state::AppState,
        clip_id: klipp_core::ClipId,
        kind: klipp_nodes::NodeKind,
    ) -> uuid::Uuid {
        let seq = &mut state.sequences[state.active_sequence_idx];
        for track in &mut seq.tracks {
            if let Some(clip) = track.find_clip_mut(clip_id) {
                // Look for an existing node of this kind
                if let Some(node) = clip.node_graph.nodes.iter().find(|n| n.kind == kind) {
                    return node.id;
                }
                // None found — create a new node and wire it before the Output node
                let new_id = uuid::Uuid::new_v4();
                let label = kind.display_name().to_string();
                let new_node = klipp_nodes::Node {
                    id: new_id,
                    kind,
                    label,
                    enabled: true,
                    params: klipp_nodes::NodeParams::default(),
                    keyframes: klipp_nodes::NodeKeyframeTracks::default(),
                };
                clip.node_graph.nodes.push(new_node);
                // Insert the new node just before the output node:
                // find the edge going into the output node's port 0, reroute it
                // through the new node.
                let output_id = clip.node_graph.output_node_id;
                if let Some(edge_idx) = clip.node_graph.edges.iter().position(
                    |e| e.to_node == output_id && e.to_port == 0,
                ) {
                    let prev_node = clip.node_graph.edges[edge_idx].from_node;
                    let prev_port = clip.node_graph.edges[edge_idx].from_port;
                    // prev_node -> new_node -> output
                    clip.node_graph.edges[edge_idx] = klipp_nodes::Edge {
                        from_node: new_id,
                        from_port: 0,
                        to_node: output_id,
                        to_port: 0,
                    };
                    clip.node_graph.edges.push(klipp_nodes::Edge {
                        from_node: prev_node,
                        from_port: prev_port,
                        to_node: new_id,
                        to_port: 0,
                    });
                }
                return new_id;
            }
        }
        // Fallback: clip not found — return nil (command handler will no-op)
        uuid::Uuid::nil()
    }

    /// Render a title clip to the viewer's overlay texture for instant UI preview.
    /// This is the FCP approach: title is a separate layer composited by the viewer,
    /// not written to the shared frame buffer (which avoids race conditions with the engine).
    /// Mark a title clip for overlay texture refresh on the next frame.
    fn render_title_to_viewer(&mut self, clip_id: klipp_core::ClipId) {
        // Clear old overlay immediately to prevent ghosting during position/alignment changes
        self.viewer.title_overlay_texture = None;
        self.state.pending_title_preview_clip = Some(clip_id);
    }

    /// Upload title overlay texture if pending. Called from update() where ctx is available.
    fn upload_title_overlay(&mut self, ctx: &egui::Context) {
        let clip_id = match self.state.pending_title_preview_clip.take() {
            Some(id) => id,
            None => return,
        };
        let seq = &self.state.sequences[self.state.active_sequence_idx];
        let td = seq.tracks.iter()
            .find_map(|t| t.clips.iter().find(|c| c.id == clip_id))
            .and_then(|c| c.title_data());
        if let Some(td) = td {
            let td = td.clone();
            let (w, h) = (seq.resolution.width, seq.resolution.height);
            let title_rgba = self.title_renderer.render(&td, w, h);

            let pixels: Vec<egui::Color32> = title_rgba
                .chunks_exact(4)
                .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
                .collect();

            let image = egui::ColorImage {
                size: [w as usize, h as usize],
                pixels,
            };

            self.viewer.title_overlay_texture = Some(ctx.load_texture(
                "klipp-title-overlay",
                image,
                egui::TextureOptions::LINEAR,
            ));
            self.viewer.title_overlay_size = (w, h);
            ctx.request_repaint();
        }
    }

    fn sync_sequence_to_engine(&self) {
        let mut seq = self.sequence.write().unwrap();
        *seq = self.state.sequences[self.state.active_sequence_idx].clone();
    }

    /// Sync sequence and re-grade the cached frame directly on the UI thread.
    /// Zero latency — scopes see the result on the same frame as the slider change.
    fn sync_and_refresh_frame(&mut self) {
        self.sync_sequence_to_engine();

        // Re-grade on UI thread using the pre-grade frame from the shared buffer
        let pregrade = {
            let fb = self.frame_buffer.lock().unwrap();
            if fb.pregrade_data.is_empty() { return; }
            (fb.pregrade_data.clone(), fb.width, fb.height)
        };

        // Find the current clip's node graph
        let clip_id = self.state.selected_clip;
        if let Some(cid) = clip_id {
            let seq_idx = self.state.active_sequence_idx;
            if let Some(clip) = self.state.sequences[seq_idx].tracks.iter()
                .find_map(|t| t.clips.iter().find(|c| c.id == cid))
            {
                let graph = &clip.node_graph;
                if !graph.is_identity() {
                    let clip_time = 0.0; // Approximate — exact time not critical for preview
                    match self.ui_executor.execute(
                        &pregrade.0, graph, pregrade.1, pregrade.2,
                        clip_time, 0, None, None,
                    ) {
                        Ok((graded, _)) => {
                            let mut fb = self.frame_buffer.lock().unwrap();
                            fb.write_frame(graded, pregrade.1, pregrade.2);
                        }
                        Err(_) => {}
                    }
                } else {
                    let mut fb = self.frame_buffer.lock().unwrap();
                    fb.write_frame(pregrade.0, pregrade.1, pregrade.2);
                }
            }
        }

        // Also tell the engine so it stays in sync
        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
    }

    fn sync_media_pool_to_engine(&self) {
        let mut pool = self.media_pool_shared.write().unwrap();
        *pool = self.state.media_pool.clone();
    }

    fn send_commands(&mut self, cmds: Vec<AppCommand>) {
        if !cmds.is_empty() {
            self.state.media_usage_cache_dirty = true;
        }
        for cmd in cmds {
            match &cmd {
                AppCommand::LoadMediaFile { ref path, .. } => {
                    // Duplicate check: if this exact path is already in the pool,
                    // skip the engine probe and select the existing item.
                    if let Some(existing) = self.state.media_pool.find_by_path(path) {
                        let existing_id = existing.id;
                        self.state.toast.show(format!(
                            "'{}' is already in the media pool",
                            path.file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                        ));
                        self.state.source_media_id = Some(existing_id);
                        continue; // skip engine probe
                    }
                    // Content-based duplicate check: same file at different path
                    if let Some(fp) = compute_media_fingerprint(path) {
                        if let Some(existing) = self.state.media_pool.find_by_fingerprint(fp) {
                            let existing_id = existing.id;
                            let existing_name = existing.filename.clone();
                            self.state.toast.show(format!(
                                "'{}' appears to be the same file as '{}'",
                                path.file_name().unwrap_or_default().to_string_lossy(),
                                existing_name,
                            ));
                            self.state.source_media_id = Some(existing_id);
                            continue; // skip engine probe
                        }
                    }
                    self.state.pending_media_path = Some(path.clone());
                }
                AppCommand::ImportMedia { ref path } => {
                    // Import media: load file to get metadata, then add to pool
                    let media_id = klipp_core::MediaId::new();
                    self.state.pending_media_path = None; // Don't auto-append
                    let _ = self.cmd_tx.send(AppCommand::LoadMediaFile {
                        path: path.clone(),
                        media_id,
                    });
                    continue;
                }
                AppCommand::AppendClip { media_id } => {
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        let duration = item.duration;
                        let is_audio_only = item.video_codec.is_empty();
                        // Route: audio-only → first audio track, video → first video track
                        let target_kind = if is_audio_only {
                            klipp_timeline::TrackKind::Audio
                        } else {
                            klipp_timeline::TrackKind::Video
                        };
                        let track_index = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                            .position(|t| t.kind == target_kind)
                            .unwrap_or(0);
                        if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(track_index) {
                            let clip_id = track.append_clip(*media_id, duration);
                            let clip = track.find_clip(clip_id).unwrap().clone();
                            self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyAppendClip {
                                clip,
                                track_index,
                            } });
                            self.sync_sequence_to_engine();
                            self.state.selected_clip = Some(clip_id);
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::AppendClipToTrack { media_id, track_index } => {
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        let duration = item.duration;
                        if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get(*track_index) {
                            if track.magnetic_role == klipp_timeline::TrackRole::Primary {
                                // Magnetic path: append via MagneticEditor
                                let source_in = klipp_core::RationalTime { value: 0, rate: duration.rate };
                                let source_out = duration;
                                let clip = klipp_timeline::Clip::media(
                                    *media_id,
                                    klipp_core::RationalTime::ZERO, // will be set by append_primary
                                    klipp_core::RationalTime::ZERO,
                                    source_in,
                                    source_out,
                                );
                                let result = klipp_timeline::magnetic::MagneticEditor::append_primary(
                                    &mut self.state.sequences[self.state.active_sequence_idx], clip);
                                // Get the clip id from the result
                                if let klipp_timeline::magnetic_commands::MagneticEditCommand::AppendPrimary { ref appended_clip, .. } = result.command {
                                    self.state.selected_clip = Some(appended_clip.id);
                                }
                                self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::Magnetic(result.command) });
                                self.sync_sequence_to_engine();
                                self.state.dirty = true;
                            } else {
                                // Legacy path for Free tracks
                                let track = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index).unwrap();
                                let clip_id = track.append_clip(*media_id, duration);
                                let clip = track.find_clip(clip_id).unwrap().clone();
                                self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyAppendClip {
                                    clip,
                                    track_index: *track_index,
                                } });
                                self.sync_sequence_to_engine();
                                self.state.selected_clip = Some(clip_id);
                                self.state.dirty = true;
                            }
                        }
                    }
                    continue;
                }
                AppCommand::ImportMediaLinked { media_id } => {
                    if let Some(item) = self.state.media_pool.get(media_id).cloned() {
                        // Check for browser range selection (trimmed source in/out)
                        let range = self.state.browser_range_selection.take()
                            .filter(|r| r.media_id == *media_id);

                        let (use_duration, source_in_override, source_out_override) = if let Some(ref sel) = range {
                            let rate = item.fps.num;
                            let src_in = klipp_core::RationalTime {
                                value: (sel.range_in_secs * rate as f64).round() as i64,
                                rate,
                            };
                            let src_out = klipp_core::RationalTime {
                                value: (sel.range_out_secs * rate as f64).round() as i64,
                                rate,
                            };
                            let dur = src_out - src_in;
                            (dur, Some(src_in), Some(src_out))
                        } else {
                            (item.duration, None, None)
                        };

                        let has_video = !item.video_codec.is_empty();
                        let has_audio = item.has_audio;

                        // Helper closure to apply range override to a clip
                        let apply_range = |track: &mut klipp_timeline::Track, clip_id: klipp_core::ClipId| {
                            if let (Some(si), Some(so)) = (source_in_override, source_out_override) {
                                if let Some(c) = track.find_clip_mut(clip_id) {
                                    c.source_in = si;
                                    c.source_out = so;
                                }
                            }
                        };

                        if has_video && has_audio {
                            let video_track_idx = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                                .position(|t| t.kind == klipp_timeline::TrackKind::Video);
                            let audio_track_idx = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                                .position(|t| t.kind == klipp_timeline::TrackKind::Audio);

                            if let (Some(vi), Some(ai)) = (video_track_idx, audio_track_idx) {
                                let video_clip_id = self.state.sequences[self.state.active_sequence_idx].tracks[vi].append_clip(*media_id, use_duration);
                                apply_range(&mut self.state.sequences[self.state.active_sequence_idx].tracks[vi], video_clip_id);
                                let audio_clip_id = self.state.sequences[self.state.active_sequence_idx].tracks[ai].append_clip(*media_id, use_duration);
                                apply_range(&mut self.state.sequences[self.state.active_sequence_idx].tracks[ai], audio_clip_id);

                                if let Some(vc) = self.state.sequences[self.state.active_sequence_idx].tracks[vi].find_clip_mut(video_clip_id) {
                                    vc.linked_clip_id = Some(audio_clip_id);
                                }
                                if let Some(ac) = self.state.sequences[self.state.active_sequence_idx].tracks[ai].find_clip_mut(audio_clip_id) {
                                    ac.linked_clip_id = Some(video_clip_id);
                                }

                                self.state.selected_clip = Some(video_clip_id);
                                self.sync_sequence_to_engine();
                                self.state.dirty = true;
                            }
                        } else {
                            let target_kind = if has_video {
                                klipp_timeline::TrackKind::Video
                            } else {
                                klipp_timeline::TrackKind::Audio
                            };
                            let track_idx = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                                .position(|t| t.kind == target_kind)
                                .unwrap_or(0);
                            if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(track_idx) {
                                let clip_id = track.append_clip(*media_id, use_duration);
                                apply_range(track, clip_id);
                                self.state.selected_clip = Some(clip_id);
                                self.sync_sequence_to_engine();
                                self.state.dirty = true;
                            }
                        }
                    }
                    continue;
                }
                AppCommand::RemoveClip { clip_id } => {
                    if self.clip_track_is_locked(clip_id) { continue; }

                    // Check if this clip is on a magnetic track
                    let track_index = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == *clip_id));

                    if let Some(ti) = track_index {
                        if self.is_magnetic_track(ti) {
                            // Magnetic path: ripple delete
                            let result = klipp_timeline::magnetic::MagneticEditor::ripple_delete(
                                &mut self.state.sequences[self.state.active_sequence_idx], *clip_id);
                            self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::Magnetic(result.command) });
                            if self.state.selected_clip == Some(*clip_id) {
                                self.state.selected_clip = None;
                            }
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        } else {
                            // Legacy path (existing code)
                            let linked_id = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                                .flat_map(|t| t.clips.iter())
                                .find(|c| c.id == *clip_id)
                                .and_then(|c| c.linked_clip_id);

                            let track = &mut self.state.sequences[self.state.active_sequence_idx].tracks[ti];
                            let pos = track.clips.iter().position(|c| c.id == *clip_id);
                            if let Some(pos) = pos {
                                let shifted: Vec<_> = track.clips[pos + 1..].iter()
                                    .map(|c| (c.id, c.timeline_in, c.timeline_out))
                                    .collect();
                                if let Some(removed) = track.ripple_remove(*clip_id) {
                                    self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyRippleRemove {
                                        removed_clip: removed,
                                        track_index: ti,
                                        position: pos,
                                        shifted_clips: shifted,
                                    } });
                                }
                            }
                            if self.state.selected_clip == Some(*clip_id) {
                                self.state.selected_clip = None;
                            }
                            // Also remove linked partner
                            if let Some(lid) = linked_id {
                                let ltrack_index = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                                    .position(|t| t.clips.iter().any(|c| c.id == lid));
                                if let Some(lti) = ltrack_index {
                                    let track = &mut self.state.sequences[self.state.active_sequence_idx].tracks[lti];
                                    let pos = track.clips.iter().position(|c| c.id == lid);
                                    if let Some(pos) = pos {
                                        let shifted: Vec<_> = track.clips[pos + 1..].iter()
                                            .map(|c| (c.id, c.timeline_in, c.timeline_out))
                                            .collect();
                                        if let Some(removed) = track.ripple_remove(lid) {
                                            self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyRippleRemove {
                                                removed_clip: removed,
                                                track_index: lti,
                                                position: pos,
                                                shifted_clips: shifted,
                                            } });
                                        }
                                    }
                                }
                            }
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::MoveClip {
                    clip_id,
                    new_timeline_in,
                } => {
                    if self.clip_track_is_locked(clip_id) { continue; }
                    // Timeline already moved both the clip and its linked partner during drag.
                    // Here we just record undo history and sync.
                    // Find group_id and compute delta before moving
                    let (group_id, delta) = {
                        let clip = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == *clip_id);
                        match clip {
                            Some(c) => (c.group_id, *new_timeline_in - c.timeline_in),
                            None => (None, klipp_core::RationalTime { value: 0, rate: 1 }),
                        }
                    };

                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                    {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let old_in = clip.timeline_in;
                            let old_out = clip.timeline_out;
                            let dur = clip.duration();
                            clip.timeline_in = *new_timeline_in;
                            clip.timeline_out = *new_timeline_in + dur;
                            self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyMoveClip {
                                clip_id: *clip_id,
                                old_timeline_in: old_in,
                                old_timeline_out: old_out,
                                new_timeline_in: clip.timeline_in,
                                new_timeline_out: clip.timeline_out,
                            } });
                        }
                        track.sort_clips();
                    }

                    // Propagate move to grouped clips
                    if let Some(gid) = group_id {
                        if delta.value != 0 {
                            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                                for clip in &mut track.clips {
                                    if clip.group_id == Some(gid) && clip.id != *clip_id {
                                        clip.timeline_in += delta;
                                        clip.timeline_out += delta;
                                    }
                                }
                                track.sort_clips();
                            }
                        }
                    }

                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::MoveClipToTrack { clip_id, new_timeline_in, from_track, to_track } => {
                    if self.clip_track_is_locked(clip_id) { continue; }
                    if self.state.sequences[self.state.active_sequence_idx].tracks.get(*to_track).is_some_and(|t| t.locked) { continue; }
                    let from = *from_track;
                    let to = *to_track;
                    let new_in = *new_timeline_in;
                    if let Some(src_track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(from) {
                        if let Some(clip) = src_track.find_clip(*clip_id).cloned() {
                            let old_in = clip.timeline_in;
                            let old_out = clip.timeline_out;
                            let dur = clip.duration();
                            let mut moved_clip = clip.clone();
                            moved_clip.timeline_in = new_in;
                            moved_clip.timeline_out = new_in + dur;
                            src_track.remove_clip(*clip_id);

                            if let Some(dst_track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(to) {
                                dst_track.clips.push(moved_clip.clone());
                                dst_track.sort_clips();
                            }

                            self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyMoveClipToTrack {
                                clip: moved_clip,
                                old_timeline_in: old_in,
                                old_timeline_out: old_out,
                                new_timeline_in: new_in,
                                from_track: from,
                                to_track: to,
                            } });
                            self.sync_sequence_to_engine();
                            // Force engine to re-evaluate decoders at current playhead
                            let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::TrimClip {
                    clip_id,
                    new_source_in,
                    new_source_out,
                    new_timeline_in,
                    new_timeline_out,
                    ripple: _,
                } => {
                    if self.clip_track_is_locked(clip_id) { continue; }
                    // Get linked partner info before mutating
                    let linked_id = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == *clip_id)
                        .and_then(|c| c.linked_clip_id);

                    // Trim main clip
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                    {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyTrimClip {
                                clip_id: *clip_id,
                                old_source_in: clip.source_in,
                                old_source_out: clip.source_out,
                                old_timeline_in: clip.timeline_in,
                                old_timeline_out: clip.timeline_out,
                                new_source_in: *new_source_in,
                                new_source_out: *new_source_out,
                                new_timeline_in: *new_timeline_in,
                                new_timeline_out: *new_timeline_out,
                            } });
                            clip.source_in = *new_source_in;
                            clip.source_out = *new_source_out;
                            clip.timeline_in = *new_timeline_in;
                            clip.timeline_out = *new_timeline_out;
                        }
                    }
                    // Trim linked partner by same values
                    if let Some(lid) = linked_id {
                        if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                            .find(|t| t.clips.iter().any(|c| c.id == lid))
                        {
                            if let Some(lclip) = track.find_clip_mut(lid) {
                                lclip.source_in = *new_source_in;
                                lclip.source_out = *new_source_out;
                                lclip.timeline_in = *new_timeline_in;
                                lclip.timeline_out = *new_timeline_out;
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RollTrimClip {
                    clip_a_id,
                    clip_b_id,
                    new_a_source_out,
                    new_a_timeline_out,
                    new_b_source_in,
                    new_b_timeline_in,
                } => {
                    if self.clip_track_is_locked(clip_a_id) || self.clip_track_is_locked(clip_b_id) { continue; }

                    // Save old values for undo, then apply
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    let old_a = seq.tracks.iter().flat_map(|t| t.clips.iter())
                        .find(|c| c.id == *clip_a_id)
                        .map(|c| (c.source_out, c.timeline_out));
                    let old_b = seq.tracks.iter().flat_map(|t| t.clips.iter())
                        .find(|c| c.id == *clip_b_id)
                        .map(|c| (c.source_in, c.timeline_in));

                    if let (Some((old_a_src_out, old_a_tl_out)), Some((old_b_src_in, old_b_tl_in))) = (old_a, old_b) {
                        self.state.history.push(HistoryEntry {
                            sequence_idx: self.state.active_sequence_idx,
                            command: EditCommand::LegacyRollTrimClip {
                                clip_a_id: *clip_a_id,
                                clip_b_id: *clip_b_id,
                                old_a_source_out: old_a_src_out,
                                old_a_timeline_out: old_a_tl_out,
                                old_b_source_in: old_b_src_in,
                                old_b_timeline_in: old_b_tl_in,
                                new_a_source_out: *new_a_source_out,
                                new_a_timeline_out: *new_a_timeline_out,
                                new_b_source_in: *new_b_source_in,
                                new_b_timeline_in: *new_b_timeline_in,
                            },
                        });

                        // Apply to clip A
                        let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                        for track in &mut seq.tracks {
                            if let Some(c) = track.find_clip_mut(*clip_a_id) {
                                c.source_out = *new_a_source_out;
                                c.timeline_out = *new_a_timeline_out;
                            }
                            if let Some(c) = track.find_clip_mut(*clip_b_id) {
                                c.source_in = *new_b_source_in;
                                c.timeline_in = *new_b_timeline_in;
                            }
                        }

                        // Also roll-trim linked partners
                        let linked_a = seq.tracks.iter().flat_map(|t| t.clips.iter())
                            .find(|c| c.id == *clip_a_id).and_then(|c| c.linked_clip_id);
                        let linked_b = seq.tracks.iter().flat_map(|t| t.clips.iter())
                            .find(|c| c.id == *clip_b_id).and_then(|c| c.linked_clip_id);
                        if let Some(la) = linked_a {
                            for track in &mut seq.tracks {
                                if let Some(c) = track.find_clip_mut(la) {
                                    c.source_out = *new_a_source_out;
                                    c.timeline_out = *new_a_timeline_out;
                                }
                            }
                        }
                        if let Some(lb) = linked_b {
                            for track in &mut seq.tracks {
                                if let Some(c) = track.find_clip_mut(lb) {
                                    c.source_in = *new_b_source_in;
                                    c.timeline_in = *new_b_timeline_in;
                                }
                            }
                        }
                    }

                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SlipClip { clip_id, new_source_in: _, new_source_out: _ } => {
                    if self.clip_track_is_locked(clip_id) { continue; }
                    // Slip was already applied during drag.  Undo entry was pushed by the timeline panel.
                    // Just sync the engine.
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SlideClip {
                    clip_id,
                    new_timeline_in: _, new_timeline_out: _,
                    left_clip_id: _, new_left_out: _,
                    right_clip_id: _, new_right_in: _,
                } => {
                    if self.clip_track_is_locked(clip_id) { continue; }
                    // Slide was already applied during drag.  Undo entry was pushed by the timeline panel.
                    // Just sync the engine.
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::Undo => {
                    self.apply_undo();
                    continue;
                }
                AppCommand::Redo => {
                    self.apply_redo();
                    continue;
                }
                AppCommand::SetRubberBandParam(param) => {
                    self.state.rubber_band_param = *param;
                    continue;
                }
                AppCommand::SetVideoRubberBandParam(param) => {
                    self.state.video_rubber_band_param = *param;
                    continue;
                }
                AppCommand::SequenceUpdated => {
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::ShowRenderQueue => {
                    self.state.show_render_queue = true;
                    continue;
                }
                AppCommand::ClearCompletedJobs => {
                    self.state.render_queue.clear_completed();
                    continue;
                }
                AppCommand::ShowPreferences => {
                    self.state.show_preferences = true;
                    continue;
                }
                AppCommand::ShowExportDialog => {
                    self.state.show_export_dialog = true;
                    if self.export_dialog.output_path.is_empty() {
                        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                        self.export_dialog.output_path = format!("{home}/output.{}", self.export_dialog.format.extension());
                    }
                    // Restore last-used preset
                    if self.export_dialog.selected_preset_id.is_none() {
                        if let Some(ref last_id) = self.prefs.export.last_preset_id {
                            self.export_dialog.selected_preset_id = Some(last_id.clone());
                            let all_presets = builtin_presets();
                            let custom = &self.prefs.export.custom_presets;
                            if let Some(preset) = all_presets.iter().chain(custom.iter()).find(|p| &p.id == last_id) {
                                apply_preset_to_dialog(&mut self.export_dialog, &preset.settings);
                            }
                        }
                    }
                    continue;
                }
                AppCommand::ExportSequence { settings } => {
                    // Pre-export check: scan for offline media in the sequence
                    let offline_clips: Vec<(String, String)> = self.state.sequences[self.state.active_sequence_idx]
                        .tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .filter_map(|clip| {
                            if let klipp_timeline::ClipKind::Media { media_id } = &clip.kind {
                                let item = self.state.media_pool.get(media_id)?;
                                if item.offline {
                                    let tc = clip.timeline_in.to_timecode_string();
                                    return Some((item.filename.clone(), tc));
                                }
                            }
                            None
                        })
                        .collect();
                    if !offline_clips.is_empty() {
                        self.state.export_offline_warning = Some((offline_clips, Box::new(settings.clone())));
                        continue;
                    }
                    // Route through RenderQueue — snapshot reads in/out from sequence directly
                    let snapshot = klipp_export::RenderSnapshot::from_sequence(
                        &self.state.sequences[self.state.active_sequence_idx],
                        &self.state.media_pool,
                        &settings,
                        self.state.global_lut_path.clone(),
                        self.export_dialog.marked_range,
                    );
                    let label = settings.output_path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Render".into());
                    match snapshot {
                        Ok(snap) => {
                            self.state.render_queue.add_job(settings.output_path.clone(), settings.clone(), label, snap);
                        }
                        Err(e) => {
                            tracing::error!("Failed to create render snapshot: {e}");
                        }
                    }
                    self.state.show_render_queue = true;
                    continue;
                }
                AppCommand::ForceExportSequence { settings } => {
                    // Same as ExportSequence but skips offline check (user confirmed)
                    self.state.export_offline_warning = None;
                    let snapshot = klipp_export::RenderSnapshot::from_sequence(
                        &self.state.sequences[self.state.active_sequence_idx],
                        &self.state.media_pool,
                        &settings,
                        None,
                        self.export_dialog.marked_range,
                    );
                    let label = settings.output_path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Render".into());
                    match snapshot {
                        Ok(snap) => {
                            self.state.render_queue.add_job(settings.output_path.clone(), settings.as_ref().clone(), label, snap);
                        }
                        Err(e) => {
                            tracing::error!("Failed to create render snapshot: {e}");
                        }
                    }
                    self.state.show_render_queue = true;
                    continue;
                }
                AppCommand::CancelExport => {
                    // Cancel active render queue job
                    if let Some(job) = self.state.render_queue.jobs.iter().find(|j| j.status == klipp_export::RenderJobStatus::Rendering) {
                        let id = job.id;
                        self.state.render_queue.cancel_job(id);
                    }
                    continue;
                }
                AppCommand::SetSpeed(speed) => {
                    // Re-anchor interpolation at current position with new speed
                    self.state.playhead_anchor = self.state.playhead;
                    self.state.playhead_anchor_wall = std::time::Instant::now();
                    self.state.playback_speed = *speed;
                }
                AppCommand::SelectClip(id) => {
                    self.state.selected_clip = *id;
                    continue;
                }
                // Phase 4: Track properties
                AppCommand::SetTrackOpacity { track_index, opacity } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.opacity = opacity.clamp(0.0, 1.0);
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::SetTrackVisibility { track_index, visible } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.visible = *visible;
                    }
                    continue;
                }
                AppCommand::SetTrackVolume { track_index, volume } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.volume = volume.clamp(0.0, 1.3);
                    }
                    // Fall through to forward to engine (updates AudioMixer)
                }
                AppCommand::SetTrackMute { track_index, muted } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.muted = *muted;
                    }
                    // Fall through to forward to engine
                }
                AppCommand::SetTrackSolo { track_index, solo } => {
                    if *solo {
                        // Enable solo on this track, disable on all others
                        let solo_idx = *track_index;
                        for (i, track) in self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().enumerate() {
                            track.solo = i == solo_idx;
                        }
                    } else {
                        // Unsolo: clear solo on all tracks
                        for track in self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut() {
                            track.solo = false;
                        }
                    }
                    // Fall through to forward to engine
                }
                AppCommand::SetTrackPan { track_index, pan } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.pan = pan.clamp(-1.0, 1.0);
                    }
                    // Fall through to forward to engine
                }
                // Node graph structure commands
                AppCommand::AddNode { clip_id, ref kind, position } => {
                    let node_kind = match kind.as_str() {
                        "Exposure" => klipp_nodes::NodeKind::Exposure,
                        "Contrast" => klipp_nodes::NodeKind::Contrast,
                        "Saturation" => klipp_nodes::NodeKind::Saturation,
                        "ToneControls" => klipp_nodes::NodeKind::ToneControls,
                        "ColorWheels" => klipp_nodes::NodeKind::ColorWheels,
                        "WheelSatBri" => klipp_nodes::NodeKind::WheelSatBri,
                        "RGBCurves" => klipp_nodes::NodeKind::RGBCurves,
                        "HueSatCurves" => klipp_nodes::NodeKind::HueSatCurves,
                        "ColorBalance" => klipp_nodes::NodeKind::ColorBalance,
                        "InputTransform" => klipp_nodes::NodeKind::InputTransform,
                        "Qualifier" => klipp_nodes::NodeKind::Qualifier,
                        "Mask" => klipp_nodes::NodeKind::Mask,
                        "CombineMattes" => klipp_nodes::NodeKind::CombineMattes,
                        "ApplyMatte" => klipp_nodes::NodeKind::ApplyMatte,
                        "Blend" => klipp_nodes::NodeKind::Blend,
                        "Layer" => klipp_nodes::NodeKind::Layer,
                        "Lut3D" => klipp_nodes::NodeKind::Lut3D,
                        "FilmGrain" => klipp_nodes::NodeKind::FilmGrain,
                        "ColorMatch" => klipp_nodes::NodeKind::ColorMatch,
                        _ => { continue; }
                    };
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let old_graph = clip.node_graph.clone();
                            let new_id = klipp_nodes::NodeId::new_v4();
                            clip.node_graph.nodes.push(klipp_nodes::Node {
                                id: new_id,
                                kind: node_kind.clone(),
                                label: node_kind.display_name().to_string(),
                                enabled: true,
                                params: klipp_nodes::NodeParams::default(),
                                keyframes: klipp_nodes::NodeKeyframeTracks::default(),
                            });
                            clip.node_graph.node_positions.insert(
                                new_id,
                                klipp_nodes::NodePos { x: position.0, y: position.1 },
                            );
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::SetNodeGraph {
                                    clip_id: *clip_id,
                                    old_graph,
                                    new_graph: clip.node_graph.clone(),
                                },
                            });
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::RemoveNode { clip_id, node_id } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let old_graph = clip.node_graph.clone();
                            clip.node_graph.nodes.retain(|n| n.id != *node_id);
                            clip.node_graph.edges.retain(|e| e.from_node != *node_id && e.to_node != *node_id);
                            clip.node_graph.node_positions.remove(node_id);
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::SetNodeGraph {
                                    clip_id: *clip_id,
                                    old_graph,
                                    new_graph: clip.node_graph.clone(),
                                },
                            });
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::ConnectNodes { clip_id, from, from_port, to, to_port } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let old_graph = clip.node_graph.clone();
                            // Remove existing edge to this input port
                            clip.node_graph.edges.retain(|e| !(e.to_node == *to && e.to_port == *to_port));
                            clip.node_graph.edges.push(klipp_nodes::Edge {
                                from_node: *from,
                                from_port: *from_port,
                                to_node: *to,
                                to_port: *to_port,
                            });
                            // Validate no cycle
                            if clip.node_graph.topo_sort().is_err() {
                                clip.node_graph = old_graph;
                            } else {
                                self.state.history.push(crate::history::HistoryEntry {
                                    sequence_idx: self.state.active_sequence_idx,
                                    command: crate::history::EditCommand::SetNodeGraph {
                                        clip_id: *clip_id,
                                        old_graph,
                                        new_graph: clip.node_graph.clone(),
                                    },
                                });
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::DisconnectEdge { clip_id, to, to_port } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let old_graph = clip.node_graph.clone();
                            clip.node_graph.edges.retain(|e| !(e.to_node == *to && e.to_port == *to_port));
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::SetNodeGraph {
                                    clip_id: *clip_id,
                                    old_graph,
                                    new_graph: clip.node_graph.clone(),
                                },
                            });
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::MoveNode { clip_id, node_id, delta_x, delta_y } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let pos = clip.node_graph.node_positions.entry(*node_id).or_default();
                            pos.x += delta_x;
                            pos.y += delta_y;
                            break;
                        }
                    }
                    // No undo for continuous drag — undo pushed on drag_stopped via SetNodeGraph
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::ToggleNodeEnabled { clip_id, node_id } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let old_graph = clip.node_graph.clone();
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.enabled = !node.enabled;
                            }
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::SetNodeGraph {
                                    clip_id: *clip_id,
                                    old_graph,
                                    new_graph: clip.node_graph.clone(),
                                },
                            });
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::RenameNode { clip_id, node_id, ref label } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.label = label.clone();
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::SetScopeTapNode { .. } => {
                    // Scope tap is forwarded to engine
                    continue;
                }
                AppCommand::SetNodeParam { clip_id, node_id, param, value } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.params.apply_override(param, *value);
                                node.enabled = true;
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::SetNodeCurves { clip_id, node_id, curves } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.params.curves = Some(curves.clone());
                                node.enabled = true;
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::SetNodeHueSatCurves { clip_id, node_id, huesat } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.params.huesat = Some(huesat.clone());
                                node.enabled = true;
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::SetNodeQualifier { clip_id, node_id, qualifier } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.params.qualifier = Some(qualifier.clone());
                                node.enabled = true;
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::SetNodeMasks { clip_id, node_id, masks } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.params.masks = Some(masks.clone());
                                node.enabled = true;
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::AddKeyframe { clip_id, node_id, ref param, time_secs, value } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.keyframes.track_mut(param).set_keyframe(*time_secs, *value);
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::RemoveKeyframe { clip_id, node_id, ref param, time_secs } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.keyframes.track_mut(param).remove_keyframe(*time_secs);
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::ClearKeyframes { clip_id, node_id, ref param } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                node.keyframes.tracks.retain(|t| t.param_name != *param);
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::SetKeyframeInterpolation { clip_id, node_id, ref param, time_secs, interpolation } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                let track = node.keyframes.track_mut(param);
                                let epsilon = 0.01;
                                if let Some(kf) = track.keyframes.iter_mut().find(|kf| (kf.time_secs - *time_secs).abs() < epsilon) {
                                    kf.interpolation = *interpolation;
                                }
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::SetKeyframeTangents { clip_id, node_id, ref param, time_secs, tangent_in, tangent_out } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(node) = clip.node_graph.nodes.iter_mut().find(|n| n.id == *node_id) {
                                let track = node.keyframes.track_mut(param);
                                let epsilon = 0.01;
                                if let Some(kf) = track.keyframes.iter_mut().find(|kf| (kf.time_secs - *time_secs).abs() < epsilon) {
                                    kf.tangent_in = *tangent_in;
                                    kf.tangent_out = *tangent_out;
                                }
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::SetClipRole { clip_id, role } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.role = *role;
                            break;
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetClipSpeed { clip_id, speed } => {
                    let speed = speed.clamp(0.1, 16.0);
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    if let Some((ti, ci)) = seq.find_clip(* clip_id) {
                        let clip = &mut seq.tracks[ti].clips[ci];
                        let old_speed = clip.speed;
                        clip.speed = speed;
                        // Adjust timeline duration: new_duration = source_duration / speed
                        let source_dur_secs = clip.source_duration().to_secs_f64();
                        let new_dur_secs = source_dur_secs / speed;
                        let new_duration = klipp_core::RationalTime::from_secs_f64(new_dur_secs, clip.timeline_in.rate);
                        let old_timeline_out = clip.timeline_out;
                        clip.timeline_out = clip.timeline_in + new_duration;
                        // Push undo
                        let cid = *clip_id;
                        let new_timeline_out = clip.timeline_out;
                        self.state.history.push(crate::history::HistoryEntry {
                            sequence_idx: self.state.active_sequence_idx,
                            command: crate::history::EditCommand::SetClipSpeed {
                                clip_id: cid,
                                old_speed,
                                new_speed: speed,
                                old_timeline_out,
                                new_timeline_out,
                            },
                        });
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetClipLogFormat { clip_id, log_format } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            // Find the InputTransform node specifically
                            if let Some(node) = clip.node_graph.nodes.iter_mut()
                                .find(|n| n.kind == klipp_nodes::NodeKind::InputTransform)
                            {
                                node.params.log_format = Some(*log_format);
                            }
                            break;
                        }
                    }
                    self.sync_and_refresh_frame();
                    continue;
                }
                AppCommand::ToggleColorPanel => {
                    // Toggle between Edit and Color workspace
                    use crate::state::Workspace;
                    if self.state.active_workspace == Workspace::Color {
                        self.state.active_workspace = Workspace::Edit;
                        self.state.show_color_panel = false;
                    } else {
                        self.state.active_workspace = Workspace::Color;
                        self.state.show_color_panel = true;
                        self.state.show_scopes = true;
                    }
                    continue;
                }
                AppCommand::ToggleBeforeAfter => {
                    self.state.before_after_active = !self.state.before_after_active;
                    // Fall through to forward to engine (sets bypass_color_grade)
                }

                // Global LUT — update UI state and forward to engine
                AppCommand::LoadGlobalLut { ref path } => {
                    self.state.global_lut_path = Some(path.clone());
                }
                AppCommand::ClearGlobalLut => {
                    self.state.global_lut_path = None;
                }
                AppCommand::SetGlobalLutOpacity(opacity) => {
                    self.state.global_lut_opacity = *opacity;
                }

                // Effects
                AppCommand::AddEffect { clip_id, ref plugin_name } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.effects.push(klipp_plugin::AppliedEffect {
                                plugin_name: plugin_name.clone(),
                                enabled: true,
                                param_values: std::collections::HashMap::new(),
                            });
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::RemoveEffect { clip_id, effect_index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if *effect_index < clip.effects.len() {
                                clip.effects.remove(*effect_index);
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::SetEffectEnabled { clip_id, effect_index, enabled } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(effect) = clip.effects.get_mut(*effect_index) {
                                effect.enabled = *enabled;
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::ReorderEffect { clip_id, from_index, to_index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let from = *from_index;
                            let to = *to_index;
                            if from < clip.effects.len() && to < clip.effects.len() {
                                let effect = clip.effects.remove(from);
                                clip.effects.insert(to, effect);
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }

                AppCommand::SetEffectParam { clip_id, effect_index, ref param_name, ref value } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(effect) = clip.effects.get_mut(*effect_index) {
                                effect.param_values.insert(param_name.clone(), value.clone());
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }

                // Built-in effects
                AppCommand::AddBuiltinEffect { clip_id, ref effect } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.builtin_effects.push(effect.clone());
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    if self.state.playback != PlaybackState::Playing {
                        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    }
                    continue;
                }
                AppCommand::RemoveBuiltinEffect { clip_id, index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if *index < clip.builtin_effects.len() {
                                clip.builtin_effects.remove(*index);
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    if self.state.playback != PlaybackState::Playing {
                        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    }
                    continue;
                }
                AppCommand::UpdateBuiltinEffect { clip_id, index, ref effect } => {
                    let mut old_effect = None;
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(fx) = clip.builtin_effects.get_mut(*index) {
                                old_effect = Some(fx.clone());
                                *fx = effect.clone();
                            }
                            break;
                        }
                    }
                    // Push undo if NOT in a coalescing drag
                    if let Some(old_fx) = old_effect {
                        let drag_key = format!("{:?}:builtin:{}", clip_id, index);
                        if !self.state.history.is_dragging(&drag_key) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditBuiltinEffect {
                                    clip_id: *clip_id,
                                    effect_index: *index,
                                    old_effect: old_fx,
                                    new_effect: effect.clone(),
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    if self.state.playback != PlaybackState::Playing {
                        // Re-process cached frame with updated effects (instant, no re-decode)
                        let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    }
                    continue;
                }
                // Undo coalescing — drag gesture signals for builtin effects
                AppCommand::AddAudioKeyframe { clip_id, param, time_secs, value } => {
                    let mut linked_partner_id = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    let kf = klipp_core::Keyframe {
                        time_secs: *time_secs,
                        value: *value,
                        interpolation: klipp_core::Interpolation::Linear,
                        tangent_out: (0.0, 0.0),
                        tangent_in: (0.0, 0.0),
                    };
                    // Capture old state for linked partner before main clip mutation
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            keyframes.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
                            keyframes.push(kf.clone());
                            keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    // Also add to linked partner clip
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut partner.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut partner.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut partner.gain_keyframes,
                                };
                                keyframes.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
                                keyframes.push(kf);
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                break;
                            }
                        }
                    }
                    // Capture new state and push history
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Add Audio Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveAudioKeyframe { clip_id, param, time_secs } => {
                    let mut linked_partner_id = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            keyframes.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
                            break;
                        }
                    }
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut partner.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut partner.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut partner.gain_keyframes,
                                };
                                keyframes.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
                                break;
                            }
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Remove Audio Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::AddVideoKeyframe { clip_id, param, time_secs, value } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let kf = klipp_core::Keyframe {
                                time_secs: *time_secs,
                                value: *value,
                                interpolation: klipp_core::Interpolation::Linear,
                                tangent_out: (0.0, 0.0),
                                tangent_in: (0.0, 0.0),
                            };
                            let keyframes = match param {
                                klipp_core::VideoParam::Opacity => &mut clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &mut clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &mut clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &mut clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &mut clip.transform_rotation_keyframes,
                            };
                            keyframes.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
                            keyframes.push(kf);
                            keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Add Video Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetVideoKeyframeInterpolation { clip_id, param, keyframe_index, interpolation } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::VideoParam::Opacity => &mut clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &mut clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &mut clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &mut clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &mut clip.transform_rotation_keyframes,
                            };
                            let ki = *keyframe_index;
                            let len = keyframes.len();
                            if ki < len {
                                keyframes[ki].interpolation = *interpolation;
                            }
                            if ki > 0 {
                                keyframes[ki - 1].interpolation = *interpolation;
                            }
                            if *interpolation == klipp_core::Interpolation::Bezier {
                                for idx in [ki.saturating_sub(1), ki] {
                                    if idx >= len { continue; }
                                    let prev_t = if idx > 0 { keyframes[idx - 1].time_secs } else { keyframes[idx].time_secs - 1.0 };
                                    let next_t = if idx + 1 < len { keyframes[idx + 1].time_secs } else { keyframes[idx].time_secs + 1.0 };
                                    let span_out = (next_t - keyframes[idx].time_secs).abs() * 0.33;
                                    let span_in = (keyframes[idx].time_secs - prev_t).abs() * 0.33;
                                    let kf = &mut keyframes[idx];
                                    if kf.tangent_out == (0.0, 0.0) || kf.tangent_out.0.abs() < 0.5 {
                                        kf.tangent_out = (span_out, 0.0);
                                    }
                                    if kf.tangent_in == (0.0, 0.0) || kf.tangent_in.0.abs() < 0.5 {
                                        kf.tangent_in = (-span_in, 0.0);
                                    }
                                }
                            }
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Set Keyframe Interpolation",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetVideoKeyframeTangent { clip_id, param, keyframe_index, tangent_in, tangent_out } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::VideoParam::Opacity => &mut clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &mut clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &mut clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &mut clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &mut clip.transform_rotation_keyframes,
                            };
                            if let Some(kf) = keyframes.get_mut(*keyframe_index) {
                                if let Some(ti) = tangent_in {
                                    kf.tangent_in = *ti;
                                }
                                if let Some(to) = tangent_out {
                                    kf.tangent_out = *to;
                                }
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveVideoKeyframe { clip_id, param, time_secs } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::VideoParam::Opacity => &mut clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &mut clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &mut clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &mut clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &mut clip.transform_rotation_keyframes,
                            };
                            keyframes.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Remove Video Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::MoveVideoKeyframe { clip_id, param, index, new_time_secs, new_value } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::VideoParam::Opacity => &mut clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &mut clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &mut clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &mut clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &mut clip.transform_rotation_keyframes,
                            };
                            if let Some(kf) = keyframes.get_mut(*index) {
                                kf.time_secs = *new_time_secs;
                                kf.value = *new_value;
                            }
                            keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::ScaleAudioKeyframeTiming { clip_id, param, factor } => {
                    let mut linked_partner_id = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    let all_ids: Vec<klipp_core::ClipId> = {
                        let mut ids = vec![*clip_id];
                        if let Some(lid) = linked_partner_id { ids.push(lid); }
                        ids
                    };
                    for target_id in &all_ids {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(clip) = track.find_clip_mut(*target_id) {
                                let clip_dur = clip.duration().to_secs_f64();
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                                };
                                for kf in keyframes.iter_mut() {
                                    kf.time_secs *= factor;
                                    // Scale bezier tangent time components
                                    kf.tangent_in.0 *= *factor;
                                    kf.tangent_out.0 *= *factor;
                                }
                                // Clamp and remove out-of-range
                                keyframes.retain(|kf| kf.time_secs >= 0.0 && kf.time_secs <= clip_dur);
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                break;
                            }
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Scale Audio Keyframe Timing",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::ScaleVideoKeyframeTiming { clip_id, param, factor } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let clip_dur = clip.duration().to_secs_f64();
                            let keyframes = match param {
                                klipp_core::VideoParam::Opacity => &mut clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &mut clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &mut clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &mut clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &mut clip.transform_rotation_keyframes,
                            };
                            for kf in keyframes.iter_mut() {
                                kf.time_secs *= factor;
                                kf.tangent_in.0 *= *factor;
                                kf.tangent_out.0 *= *factor;
                            }
                            keyframes.retain(|kf| kf.time_secs >= 0.0 && kf.time_secs <= clip_dur);
                            keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Scale Video Keyframe Timing",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::InsertAudioKeyframeBump { clip_id, param, center_time, bump_value, base_value, duration_secs, ramp_secs } => {
                    let mut linked_partner_id = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    let bump_kfs: Vec<(f64, f32)> = vec![
                        (center_time - duration_secs / 2.0 - ramp_secs, *base_value),
                        (center_time - duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0 + ramp_secs, *base_value),
                    ];
                    let all_ids: Vec<klipp_core::ClipId> = {
                        let mut ids = vec![*clip_id];
                        if let Some(lid) = linked_partner_id { ids.push(lid); }
                        ids
                    };
                    for target_id in &all_ids {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(clip) = track.find_clip_mut(*target_id) {
                                let clip_dur = clip.duration().to_secs_f64();
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                                };
                                for &(t, v) in &bump_kfs {
                                    if t >= 0.0 && t <= clip_dur {
                                        keyframes.push(klipp_core::Keyframe {
                                            time_secs: t,
                                            value: v,
                                            interpolation: klipp_core::Interpolation::Linear,
                                            tangent_out: (0.0, 0.0),
                                            tangent_in: (0.0, 0.0),
                                        });
                                    }
                                }
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                break;
                            }
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Insert Audio Keyframe Bump",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::AutoDuckAudio { target_clip_id, sidechain_track_idx, duck_level, attack_secs, release_secs, threshold } => {
                    // 1. Capture old keyframe state for undo
                    let mut old_state = None;
                    let mut target_timeline_start = 0.0_f64;
                    let mut target_timeline_end = 0.0_f64;
                    let mut target_clip_dur_secs = 0.0_f64;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *target_clip_id) {
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                            target_timeline_start = c.timeline_in.to_secs_f64();
                            target_timeline_end = c.timeline_out.to_secs_f64();
                            target_clip_dur_secs = target_timeline_end - target_timeline_start;
                        }
                    }
                    if old_state.is_none() || target_clip_dur_secs <= 0.0 {
                        continue;
                    }

                    // 2. Detect active regions on the sidechain track using PCM data or clip boundaries
                    let active_regions: Vec<(f64, f64)> = {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let sidechain_track_idx = *sidechain_track_idx;
                        if sidechain_track_idx >= seq.tracks.len() {
                            continue;
                        }
                        let track = &seq.tracks[sidechain_track_idx];

                        // Try PCM-based analysis first
                        let mut pcm_regions: Vec<(f64, f64)> = Vec::new();
                        let mut used_pcm = false;

                        for sc_clip in &track.clips {
                            let sc_start = sc_clip.timeline_in.to_secs_f64();
                            let sc_end = sc_clip.timeline_out.to_secs_f64();
                            // Skip clips that don't overlap the target
                            if sc_end <= target_timeline_start || sc_start >= target_timeline_end {
                                continue;
                            }

                            // Try to read PCM data for this clip
                            let pcm_data = sc_clip.media_id()
                                .and_then(|mid| self.state.media_pool.get(&mid))
                                .and_then(|item| item.audio_cache.as_ref())
                                .and_then(|cache| {
                                    std::fs::read(&cache.pcm_path).ok().map(|bytes| (bytes, cache.sample_rate, cache.channels))
                                });

                            if let Some((pcm_bytes, sample_rate, channels)) = pcm_data {
                                used_pcm = true;
                                // SAFETY: PCM cache files are written as f32 slices by audio_cache::conform_audio
                                let samples: &[f32] = unsafe {
                                    std::slice::from_raw_parts(
                                        pcm_bytes.as_ptr() as *const f32,
                                        pcm_bytes.len() / std::mem::size_of::<f32>(),
                                    )
                                };
                                let window_samples = (sample_rate as f64 * 0.05) as usize * channels as usize; // 50ms windows
                                if window_samples == 0 { continue; }

                                let src_in_secs = sc_clip.source_in.to_secs_f64();
                                let total_mono_samples = samples.len() / channels as usize;
                                let total_dur = total_mono_samples as f64 / sample_rate as f64;

                                // Analyze overlapping region
                                let analyze_start = target_timeline_start.max(sc_start);
                                let analyze_end = target_timeline_end.min(sc_end);

                                let window_secs = 0.05_f64;
                                let mut t = analyze_start;
                                let mut region_start: Option<f64> = None;

                                while t < analyze_end {
                                    // Map timeline time to source time
                                    let source_t = src_in_secs + (t - sc_start);
                                    if source_t < 0.0 || source_t >= total_dur {
                                        t += window_secs;
                                        continue;
                                    }
                                    let sample_idx = (source_t * sample_rate as f64) as usize * channels as usize;
                                    let end_idx = ((sample_idx + window_samples).min(samples.len())).max(sample_idx);

                                    // Compute RMS
                                    let window = &samples[sample_idx..end_idx];
                                    let rms = if window.is_empty() {
                                        0.0
                                    } else {
                                        let sum: f64 = window.iter().map(|&s| (s as f64) * (s as f64)).sum();
                                        (sum / window.len() as f64).sqrt() as f32
                                    };

                                    if rms > *threshold {
                                        if region_start.is_none() {
                                            region_start = Some(t);
                                        }
                                    } else if let Some(rs) = region_start.take() {
                                        pcm_regions.push((rs, t));
                                    }
                                    t += window_secs;
                                }
                                // Close any open region
                                if let Some(rs) = region_start {
                                    pcm_regions.push((rs, analyze_end));
                                }
                            } else {
                                // Fallback: use clip boundaries as active region
                                let region_start = sc_start.max(target_timeline_start);
                                let region_end = sc_end.min(target_timeline_end);
                                if region_end > region_start {
                                    pcm_regions.push((region_start, region_end));
                                }
                            }
                        }

                        // Merge adjacent/overlapping regions (with a small gap tolerance)
                        if used_pcm && !pcm_regions.is_empty() {
                            pcm_regions.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
                            let mut merged: Vec<(f64, f64)> = Vec::new();
                            for r in pcm_regions {
                                if let Some(last) = merged.last_mut() {
                                    if r.0 <= last.1 + 0.05 { // 50ms merge gap
                                        last.1 = last.1.max(r.1);
                                        continue;
                                    }
                                }
                                merged.push(r);
                            }
                            merged
                        } else {
                            pcm_regions
                        }
                    };

                    if active_regions.is_empty() {
                        continue;
                    }

                    // 3. Generate volume keyframes on the target clip
                    let attack = *attack_secs as f64;
                    let release = *release_secs as f64;
                    let duck = *duck_level;
                    let mut keyframes: Vec<klipp_core::Keyframe> = Vec::new();

                    for &(region_start, region_end) in &active_regions {
                        // Convert to clip-local time (relative to target clip's timeline_in)
                        let ramp_down_start = (region_start - target_timeline_start - attack).max(0.0);
                        let duck_start = (region_start - target_timeline_start).max(0.0);
                        let duck_end = (region_end - target_timeline_start).min(target_clip_dur_secs);
                        let ramp_up_end = (region_end - target_timeline_start + release).min(target_clip_dur_secs);

                        // Before duck: normal volume
                        if ramp_down_start < duck_start {
                            keyframes.push(klipp_core::Keyframe {
                                time_secs: ramp_down_start,
                                value: 1.0,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_in: (0.0, 0.0),
                                tangent_out: (attack / 3.0, 0.0),
                            });
                        }

                        // Duck start
                        keyframes.push(klipp_core::Keyframe {
                            time_secs: duck_start,
                            value: duck,
                            interpolation: klipp_core::Interpolation::Bezier,
                            tangent_in: (-attack / 3.0, 0.0),
                            tangent_out: (0.0, 0.0),
                        });

                        // Duck end
                        keyframes.push(klipp_core::Keyframe {
                            time_secs: duck_end,
                            value: duck,
                            interpolation: klipp_core::Interpolation::Bezier,
                            tangent_in: (0.0, 0.0),
                            tangent_out: (release / 3.0, 0.0),
                        });

                        // After duck: normal volume
                        if ramp_up_end > duck_end {
                            keyframes.push(klipp_core::Keyframe {
                                time_secs: ramp_up_end,
                                value: 1.0,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_in: (-release / 3.0, 0.0),
                                tangent_out: (0.0, 0.0),
                            });
                        }
                    }

                    // Remove duplicate times (keep the last keyframe at each time)
                    keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                    keyframes.dedup_by(|a, b| (a.time_secs - b.time_secs).abs() < 0.01);

                    // 4. Apply keyframes to the target clip
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*target_clip_id) {
                            clip.volume_keyframes = keyframes;
                            break;
                        }
                    }

                    // 5. Push undo history
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *target_clip_id).map(KeyframeSnapshot::from_clip);
                        if let Some(new_snap) = new_state {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *target_clip_id,
                                    linked_clip_id: None,
                                    description: "Auto Duck Audio",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::InsertVideoKeyframeBump { clip_id, param, center_time, bump_value, base_value, duration_secs, ramp_secs } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    let bump_kfs: Vec<(f64, f32)> = vec![
                        (center_time - duration_secs / 2.0 - ramp_secs, *base_value),
                        (center_time - duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0, *bump_value),
                        (center_time + duration_secs / 2.0 + ramp_secs, *base_value),
                    ];
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let clip_dur = clip.duration().to_secs_f64();
                            let keyframes = match param {
                                klipp_core::VideoParam::Opacity => &mut clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &mut clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &mut clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &mut clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &mut clip.transform_rotation_keyframes,
                            };
                            for &(t, v) in &bump_kfs {
                                if t >= 0.0 && t <= clip_dur {
                                    keyframes.push(klipp_core::Keyframe {
                                        time_secs: t,
                                        value: v,
                                        interpolation: klipp_core::Interpolation::Linear,
                                        tangent_out: (0.0, 0.0),
                                        tangent_in: (0.0, 0.0),
                                    });
                                }
                            }
                            keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Insert Video Keyframe Bump",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::ApplyKenBurns { clip_id, start_scale, end_scale, start_pos_x, start_pos_y, end_pos_x, end_pos_y } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let duration = clip.source_out.to_secs_f64() - clip.source_in.to_secs_f64();
                            let ease_t = 0.42 * duration;

                            // Clear existing transform keyframes
                            clip.transform_scale_keyframes.clear();
                            clip.transform_pos_x_keyframes.clear();
                            clip.transform_pos_y_keyframes.clear();

                            // Ensure a Transform builtin effect exists
                            if !clip.builtin_effects.iter().any(|e| matches!(e, klipp_effects::BuiltinEffect::Transform { .. })) {
                                clip.builtin_effects.push(klipp_effects::BuiltinEffect::Transform {
                                    position_x: 0.0, position_y: 0.0,
                                    scale_x: *start_scale, scale_y: *start_scale,
                                    rotation: 0.0, anchor_x: 0.5, anchor_y: 0.5,
                                });
                            }

                            // Scale keyframes
                            clip.transform_scale_keyframes.push(klipp_core::Keyframe {
                                time_secs: 0.0, value: *start_scale,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (ease_t, 0.0), tangent_in: (0.0, 0.0),
                            });
                            clip.transform_scale_keyframes.push(klipp_core::Keyframe {
                                time_secs: duration, value: *end_scale,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (0.0, 0.0), tangent_in: (-ease_t, 0.0),
                            });

                            // Position X keyframes
                            clip.transform_pos_x_keyframes.push(klipp_core::Keyframe {
                                time_secs: 0.0, value: *start_pos_x,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (ease_t, 0.0), tangent_in: (0.0, 0.0),
                            });
                            clip.transform_pos_x_keyframes.push(klipp_core::Keyframe {
                                time_secs: duration, value: *end_pos_x,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (0.0, 0.0), tangent_in: (-ease_t, 0.0),
                            });

                            // Position Y keyframes
                            clip.transform_pos_y_keyframes.push(klipp_core::Keyframe {
                                time_secs: 0.0, value: *start_pos_y,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (ease_t, 0.0), tangent_in: (0.0, 0.0),
                            });
                            clip.transform_pos_y_keyframes.push(klipp_core::Keyframe {
                                time_secs: duration, value: *end_pos_y,
                                interpolation: klipp_core::Interpolation::Bezier,
                                tangent_out: (0.0, 0.0), tangent_in: (-ease_t, 0.0),
                            });
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Apply Ken Burns",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::ClearTransformKeyframes { clip_id } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.transform_scale_keyframes.clear();
                            clip.transform_pos_x_keyframes.clear();
                            clip.transform_pos_y_keyframes.clear();
                            clip.transform_rotation_keyframes.clear();
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Clear Transform Keyframes",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::ClearVideoParamKeyframes { clip_id, param } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            match param {
                                klipp_core::VideoParam::Opacity => clip.opacity_keyframes.clear(),
                                klipp_core::VideoParam::PositionX => clip.transform_pos_x_keyframes.clear(),
                                klipp_core::VideoParam::PositionY => clip.transform_pos_y_keyframes.clear(),
                                klipp_core::VideoParam::Scale => clip.transform_scale_keyframes.clear(),
                                klipp_core::VideoParam::Rotation => clip.transform_rotation_keyframes.clear(),
                            }
                            break;
                        }
                    }
                    continue;
                }
                // ── Keyframe clipboard: Copy ──────────────────────────────
                AppCommand::CopyAudioKeyframes { clip_id, param } => {
                    if let Some(clip) = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id)
                    {
                        let kfs = match param {
                            klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                            klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                            klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                        };
                        self.state.keyframe_clipboard = Some(crate::state::KeyframeClipboard {
                            keyframes: kfs.clone(),
                            source_duration_secs: clip.duration().to_secs_f64(),
                        });
                    }
                    continue;
                }
                AppCommand::CopyVideoKeyframes { clip_id, param } => {
                    if let Some(clip) = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id)
                    {
                        let kfs = match param {
                            klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                            klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                            klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                            klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                            klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                        };
                        self.state.keyframe_clipboard = Some(crate::state::KeyframeClipboard {
                            keyframes: kfs.clone(),
                            source_duration_secs: clip.duration().to_secs_f64(),
                        });
                    }
                    continue;
                }

                // ── Keyframe clipboard: Paste ──────────────────────────────
                AppCommand::PasteAudioKeyframes { clip_id, param } => {
                    if let Some(ref cb) = self.state.keyframe_clipboard.clone() {
                        let old_state = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let target_dur = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(|c| c.duration().to_secs_f64());
                        if let Some(target_dur) = target_dur {
                            let scale = if cb.source_duration_secs > 0.0 { target_dur / cb.source_duration_secs } else { 1.0 };
                            let mut new_kfs: Vec<klipp_core::Keyframe> = cb.keyframes.iter().map(|kf| {
                                let mut k = kf.clone();
                                k.time_secs *= scale;
                                k
                            }).collect();
                            new_kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                                if let Some(clip) = track.find_clip_mut(*clip_id) {
                                    let target = match param {
                                        klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                        klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                        klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                                    };
                                    *target = new_kfs;
                                    break;
                                }
                            }
                            if let Some(old_snap) = old_state {
                                let seq = &self.state.sequences[self.state.active_sequence_idx];
                                if let Some(new_snap) = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                                    self.state.history.push(HistoryEntry {
                                        sequence_idx: self.state.active_sequence_idx,
                                        command: EditCommand::EditKeyframes {
                                            clip_id: *clip_id,
                                            linked_clip_id: None,
                                            description: "Paste Audio Keyframes",
                                            old_state: old_snap,
                                            new_state: new_snap,
                                            old_linked_state: None,
                                            new_linked_state: None,
                                        },
                                    });
                                }
                            }
                            self.sync_sequence_to_engine();
                            let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::PasteVideoKeyframes { clip_id, param } => {
                    if let Some(ref cb) = self.state.keyframe_clipboard.clone() {
                        let old_state = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let target_dur = self.state.sequences[self.state.active_sequence_idx]
                            .all_clips().find(|c| c.id == *clip_id).map(|c| c.duration().to_secs_f64());
                        if let Some(target_dur) = target_dur {
                            let scale = if cb.source_duration_secs > 0.0 { target_dur / cb.source_duration_secs } else { 1.0 };
                            let mut new_kfs: Vec<klipp_core::Keyframe> = cb.keyframes.iter().map(|kf| {
                                let mut k = kf.clone();
                                k.time_secs *= scale;
                                k
                            }).collect();
                            new_kfs.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                                if let Some(clip) = track.find_clip_mut(*clip_id) {
                                    let target = match param {
                                        klipp_core::VideoParam::Opacity => &mut clip.opacity_keyframes,
                                        klipp_core::VideoParam::PositionX => &mut clip.transform_pos_x_keyframes,
                                        klipp_core::VideoParam::PositionY => &mut clip.transform_pos_y_keyframes,
                                        klipp_core::VideoParam::Scale => &mut clip.transform_scale_keyframes,
                                        klipp_core::VideoParam::Rotation => &mut clip.transform_rotation_keyframes,
                                    };
                                    *target = new_kfs;
                                    break;
                                }
                            }
                            if let Some(old_snap) = old_state {
                                let seq = &self.state.sequences[self.state.active_sequence_idx];
                                if let Some(new_snap) = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                                    self.state.history.push(HistoryEntry {
                                        sequence_idx: self.state.active_sequence_idx,
                                        command: EditCommand::EditKeyframes {
                                            clip_id: *clip_id,
                                            linked_clip_id: None,
                                            description: "Paste Video Keyframes",
                                            old_state: old_snap,
                                            new_state: new_snap,
                                            old_linked_state: None,
                                            new_linked_state: None,
                                        },
                                    });
                                }
                            }
                            self.sync_sequence_to_engine();
                            let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }

                // ── Reverse keyframes ──────────────────────────────────────
                AppCommand::ReverseAudioKeyframes { clip_id, param } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            if keyframes.len() >= 2 {
                                let max_time = keyframes.last().map(|k| k.time_secs).unwrap_or(0.0);
                                for kf in keyframes.iter_mut() {
                                    kf.time_secs = max_time - kf.time_secs;
                                    let old_in = kf.tangent_in;
                                    let old_out = kf.tangent_out;
                                    kf.tangent_in = (-old_out.0, old_out.1);
                                    kf.tangent_out = (-old_in.0, old_in.1);
                                }
                                keyframes.reverse();
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            }
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(new_snap) = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Reverse Audio Keyframes",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::ReverseVideoKeyframes { clip_id, param } => {
                    let old_state = self.state.sequences[self.state.active_sequence_idx]
                        .all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let keyframes = match param {
                                klipp_core::VideoParam::Opacity => &mut clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &mut clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &mut clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &mut clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &mut clip.transform_rotation_keyframes,
                            };
                            if keyframes.len() >= 2 {
                                let max_time = keyframes.last().map(|k| k.time_secs).unwrap_or(0.0);
                                for kf in keyframes.iter_mut() {
                                    kf.time_secs = max_time - kf.time_secs;
                                    let old_in = kf.tangent_in;
                                    let old_out = kf.tangent_out;
                                    kf.tangent_in = (-old_out.0, old_out.1);
                                    kf.tangent_out = (-old_in.0, old_in.1);
                                }
                                keyframes.reverse();
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            }
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(new_snap) = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Reverse Video Keyframes",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }

                AppCommand::AddEffectKeyframe { clip_id, effect_index, param_name, time_secs, value } => {
                    let key = format!("{}:{}", effect_index, param_name);
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let kf = klipp_core::Keyframe {
                                time_secs: *time_secs,
                                value: *value,
                                interpolation: klipp_core::Interpolation::Linear,
                                tangent_out: (0.0, 0.0),
                                tangent_in: (0.0, 0.0),
                            };
                            let keyframes = clip.effect_keyframes.entry(key.clone()).or_default();
                            keyframes.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
                            keyframes.push(kf);
                            keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Add Effect Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveEffectKeyframe { clip_id, effect_index, param_name, time_secs } => {
                    let key = format!("{}:{}", effect_index, param_name);
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(keyframes) = clip.effect_keyframes.get_mut(&key) {
                                keyframes.retain(|k| (k.time_secs - time_secs).abs() >= 0.02);
                                if keyframes.is_empty() {
                                    clip.effect_keyframes.remove(&key);
                                }
                            }
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Remove Effect Keyframe",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::MoveEffectKeyframe { clip_id, effect_index, param_name, keyframe_index, new_time_secs, new_value } => {
                    let key = format!("{}:{}", effect_index, param_name);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(keyframes) = clip.effect_keyframes.get_mut(&key) {
                                if let Some(kf) = keyframes.get_mut(*keyframe_index) {
                                    kf.time_secs = *new_time_secs;
                                    kf.value = *new_value;
                                }
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetEffectKeyframeInterpolation { clip_id, effect_index, param_name, keyframe_index, interpolation } => {
                    let key = format!("{}:{}", effect_index, param_name);
                    let old_state = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(keyframes) = clip.effect_keyframes.get_mut(&key) {
                                let ki = *keyframe_index;
                                let len = keyframes.len();
                                if ki < len {
                                    keyframes[ki].interpolation = *interpolation;
                                }
                                if ki > 0 {
                                    keyframes[ki - 1].interpolation = *interpolation;
                                }
                                if *interpolation == klipp_core::Interpolation::Bezier {
                                    for idx in [ki.saturating_sub(1), ki] {
                                        if idx >= len { continue; }
                                        let prev_t = if idx > 0 { keyframes[idx - 1].time_secs } else { keyframes[idx].time_secs - 1.0 };
                                        let next_t = if idx + 1 < len { keyframes[idx + 1].time_secs } else { keyframes[idx].time_secs + 1.0 };
                                        let span_out = (next_t - keyframes[idx].time_secs).abs() * 0.33;
                                        let span_in = (keyframes[idx].time_secs - prev_t).abs() * 0.33;
                                        let kf = &mut keyframes[idx];
                                        if kf.tangent_out == (0.0, 0.0) || kf.tangent_out.0.abs() < 0.5 {
                                            kf.tangent_out = (span_out, 0.0);
                                        }
                                        if kf.tangent_in == (0.0, 0.0) || kf.tangent_in.0.abs() < 0.5 {
                                            kf.tangent_in = (-span_in, 0.0);
                                        }
                                    }
                                }
                            }
                            break;
                        }
                    }
                    if let Some(old_snap) = old_state {
                        if let Some(new_snap) = self.state.sequences[self.state.active_sequence_idx].all_clips()
                            .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: None,
                                    description: "Set Keyframe Interpolation",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state: None,
                                    new_linked_state: None,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::MoveAudioKeyframe { clip_id, param, index, new_time_secs, new_value } => {
                    let mut linked_partner_id = None;
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            linked_partner_id = clip.linked_clip_id;
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            if let Some(kf) = keyframes.get_mut(*index) {
                                kf.time_secs = *new_time_secs;
                                kf.value = *new_value;
                            }
                            keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    // Also move on linked partner clip
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut partner.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut partner.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut partner.gain_keyframes,
                                };
                                if let Some(kf) = keyframes.get_mut(*index) {
                                    kf.time_secs = *new_time_secs;
                                    kf.value = *new_value;
                                }
                                keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                                break;
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetAudioKeyframeInterpolation { clip_id, param, keyframe_index, interpolation } => {
                    // Capture old state for undo
                    let mut linked_partner_id_interp = None;
                    let mut old_state = None;
                    let mut old_linked_state = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id_interp = c.linked_clip_id;
                            old_state = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id_interp {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_state = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    // Apply to target clip AND linked partner
                    let target_ids: Vec<klipp_core::ClipId> = {
                        let mut ids = vec![*clip_id];
                        if let Some(lid) = linked_partner_id_interp {
                            ids.push(lid);
                        }
                        ids
                    };
                    for target_id in &target_ids {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*target_id) {
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            let ki = *keyframe_index;
                            let len = keyframes.len();
                            if ki < len {
                                keyframes[ki].interpolation = *interpolation;
                            }
                            if ki > 0 {
                                keyframes[ki - 1].interpolation = *interpolation;
                            }
                            if *interpolation == klipp_core::Interpolation::Bezier {
                                for idx in [ki.saturating_sub(1), ki] {
                                    if idx >= len { continue; }
                                    let prev_t = if idx > 0 { keyframes[idx - 1].time_secs } else { keyframes[idx].time_secs - 1.0 };
                                    let next_t = if idx + 1 < len { keyframes[idx + 1].time_secs } else { keyframes[idx].time_secs + 1.0 };
                                    let span_out = (next_t - keyframes[idx].time_secs).abs() * 0.33;
                                    let span_in = (keyframes[idx].time_secs - prev_t).abs() * 0.33;
                                    let kf = &mut keyframes[idx];
                                    if kf.tangent_out == (0.0, 0.0) || kf.tangent_out.0.abs() < 0.5 {
                                        kf.tangent_out = (span_out, 0.0);
                                    }
                                    if kf.tangent_in == (0.0, 0.0) || kf.tangent_in.0.abs() < 0.5 {
                                        kf.tangent_in = (-span_in, 0.0);
                                    }
                                }
                            }
                            break;
                        }
                    }
                    } // end for target_id
                    if let Some(old_snap) = old_state {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        let new_state = seq.all_clips().find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip);
                        let new_linked_state = linked_partner_id_interp.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                        if let Some(new_snap) = new_state {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id_interp,
                                    description: "Set Keyframe Interpolation",
                                    old_state: old_snap,
                                    new_state: new_snap,
                                    old_linked_state,
                                    new_linked_state,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetAudioKeyframeTangent { clip_id, param, keyframe_index, tangent_in, tangent_out } => {
                    let mut linked_partner_id = None;
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            linked_partner_id = clip.linked_clip_id;
                            let keyframes = match param {
                                klipp_core::AudioParam::Volume => &mut clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &mut clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &mut clip.gain_keyframes,
                            };
                            if let Some(kf) = keyframes.get_mut(*keyframe_index) {
                                if let Some(ti) = tangent_in {
                                    kf.tangent_in = *ti;
                                }
                                if let Some(to) = tangent_out {
                                    kf.tangent_out = *to;
                                }
                            }
                            break;
                        }
                    }
                    // Also set tangent on linked partner clip
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &mut partner.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &mut partner.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &mut partner.gain_keyframes,
                                };
                                if let Some(kf) = keyframes.get_mut(*keyframe_index) {
                                    if let Some(ti) = tangent_in {
                                        kf.tangent_in = *ti;
                                    }
                                    if let Some(to) = tangent_out {
                                        kf.tangent_out = *to;
                                    }
                                }
                                break;
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UpdateClipAudio { clip_id, gain, volume, pan, fade_in_secs, fade_out_secs, fade_in_curve, fade_out_curve, channel_config } => {
                    let mut old_vals = None;
                    let mut linked_partner_id = None;
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            old_vals = Some((clip.gain, clip.volume, clip.pan, clip.fade_in_secs, clip.fade_out_secs, clip.channel_config));
                            linked_partner_id = clip.linked_clip_id;
                            clip.gain = *gain;
                            clip.volume = *volume;
                            clip.pan = *pan;
                            clip.fade_in_secs = *fade_in_secs;
                            clip.fade_out_secs = *fade_out_secs;
                            clip.fade_in_curve = *fade_in_curve;
                            clip.fade_out_curve = *fade_out_curve;
                            clip.channel_config = *channel_config;
                            break;
                        }
                    }
                    // Also sync audio params to the linked partner clip
                    // so both video and audio clips have the same values
                    if let Some(partner_id) = linked_partner_id {
                        for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                            if let Some(partner) = track.find_clip_mut(partner_id) {
                                partner.gain = *gain;
                                partner.volume = *volume;
                                partner.pan = *pan;
                                partner.fade_in_secs = *fade_in_secs;
                                partner.fade_out_secs = *fade_out_secs;
                                partner.fade_in_curve = *fade_in_curve;
                                partner.fade_out_curve = *fade_out_curve;
                                partner.channel_config = *channel_config;
                                break;
                            }
                        }
                    }
                    if let Some((og, ov, op, ofi, ofo, occ)) = old_vals {
                        let drag_key = format!("{:?}:audio", clip_id);
                        if !self.state.history.is_dragging(&drag_key) {
                            self.state.history.push(HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditClipAudio {
                                    clip_id: *clip_id,
                                    old_gain: og,
                                    old_volume: ov,
                                    old_pan: op,
                                    old_fade_in: ofi,
                                    old_fade_out: ofo,
                                    old_channel_config: occ,
                                    new_gain: *gain,
                                    new_volume: *volume,
                                    new_pan: *pan,
                                    new_fade_in: *fade_in_secs,
                                    new_fade_out: *fade_out_secs,
                                    new_channel_config: *channel_config,
                                },
                            });
                        }
                    }
                    self.sync_sequence_to_engine();
                    // Instant clip gain + volume + pan + channel config — bypass command channel latency
                    let _ = self.cmd_tx.send(AppCommand::SetClipGain(*gain));
                    let _ = self.cmd_tx.send(AppCommand::SetClipVolume(*volume));
                    let _ = self.cmd_tx.send(AppCommand::SetClipPan(*pan));
                    let _ = self.cmd_tx.send(AppCommand::SetClipChannelConfig(channel_config_to_u32(*channel_config)));
                    // Also sync sequence for persistence + paused frame refresh
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    continue;
                }
                AppCommand::BeginKeyframeDrag { ref key, clip_id } => {
                    // Capture keyframe snapshot at drag start for undo coalescing
                    let mut linked_partner_id = None;
                    let mut old_snap = None;
                    let mut old_linked_snap = None;
                    {
                        let seq = &self.state.sequences[self.state.active_sequence_idx];
                        if let Some(c) = seq.all_clips().find(|c| c.id == *clip_id) {
                            linked_partner_id = c.linked_clip_id;
                            old_snap = Some(KeyframeSnapshot::from_clip(c));
                        }
                        if let Some(pid) = linked_partner_id {
                            if let Some(p) = seq.all_clips().find(|c| c.id == pid) {
                                old_linked_snap = Some(KeyframeSnapshot::from_clip(p));
                            }
                        }
                    }
                    if let Some(old_state) = old_snap {
                        self.state.history.begin_drag(
                            key.clone(),
                            HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: EditCommand::EditKeyframes {
                                    clip_id: *clip_id,
                                    linked_clip_id: linked_partner_id,
                                    description: "Move Keyframe",
                                    old_state: old_state.clone(),
                                    new_state: old_state, // placeholder, replaced at end_drag
                                    old_linked_state: old_linked_snap.clone(),
                                    new_linked_state: old_linked_snap,
                                },
                            },
                        );
                    }
                    continue;
                }
                AppCommand::EndKeyframeDrag { ref key, clip_id } => {
                    let _ = key;
                    // Read the old state from the pending entry
                    let (old_state, linked_clip_id, old_linked_state, description) = self.state.history.pending_old_entry_ref()
                        .and_then(|e| match &e.command {
                            EditCommand::EditKeyframes { old_state, linked_clip_id, old_linked_state, description, .. } =>
                                Some((old_state.clone(), *linked_clip_id, old_linked_state.clone(), *description)),
                            _ => None,
                        })
                        .unwrap_or_else(|| {
                            // Fallback: capture current state (no-op undo)
                            let snap = self.state.sequences[self.state.active_sequence_idx].all_clips()
                                .find(|c| c.id == *clip_id).map(KeyframeSnapshot::from_clip)
                                .unwrap_or_else(KeyframeSnapshot::empty);
                            (snap.clone(), None, None, "Move Keyframe")
                        });
                    // Capture current (post-drag) state
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let new_state = seq.all_clips().find(|c| c.id == *clip_id)
                        .map(KeyframeSnapshot::from_clip)
                        .unwrap_or_else(|| old_state.clone());
                    let new_linked_state = linked_clip_id.and_then(|pid| seq.all_clips().find(|c| c.id == pid).map(KeyframeSnapshot::from_clip));
                    self.state.history.end_drag(HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
                        command: EditCommand::EditKeyframes {
                            clip_id: *clip_id,
                            linked_clip_id,
                            description,
                            old_state,
                            new_state,
                            old_linked_state,
                            new_linked_state,
                        },
                    });
                    continue;
                }
                AppCommand::BeginAudioDrag { ref key, clip_id, old_gain, old_volume, old_pan, old_fade_in, old_fade_out } => {
                    // Look up current channel_config from the clip (drag commands don't carry it)
                    let cc = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id)
                        .map(|c| c.channel_config)
                        .unwrap_or_default();
                    self.state.history.begin_drag(
                        key.clone(),
                        HistoryEntry {
                            sequence_idx: self.state.active_sequence_idx,
                            command: EditCommand::EditClipAudio {
                                clip_id: *clip_id,
                                old_gain: *old_gain,
                                old_volume: *old_volume,
                                old_pan: *old_pan,
                                old_fade_in: *old_fade_in,
                                old_fade_out: *old_fade_out,
                                old_channel_config: cc,
                                new_gain: *old_gain,
                                new_volume: *old_volume,
                                new_pan: *old_pan,
                                new_fade_in: *old_fade_in,
                                new_fade_out: *old_fade_out,
                                new_channel_config: cc,
                            },
                        },
                    );
                    continue;
                }
                AppCommand::EndAudioDrag { ref key, clip_id, new_gain, new_volume, new_pan, new_fade_in, new_fade_out } => {
                    let _ = key;
                    let cc = self.state.sequences[self.state.active_sequence_idx].all_clips()
                        .find(|c| c.id == *clip_id)
                        .map(|c| c.channel_config)
                        .unwrap_or_default();
                    let (og, ov, op, ofi, ofo, occ) = self.state.history.pending_old_entry_ref()
                        .and_then(|e| match &e.command {
                            EditCommand::EditClipAudio { old_gain, old_volume, old_pan, old_fade_in, old_fade_out, old_channel_config, .. } => {
                                Some((*old_gain, *old_volume, *old_pan, *old_fade_in, *old_fade_out, *old_channel_config))
                            }
                            _ => None,
                        })
                        .unwrap_or((*new_gain, *new_volume, *new_pan, *new_fade_in, *new_fade_out, cc));
                    self.state.history.end_drag(HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
                        command: EditCommand::EditClipAudio {
                            clip_id: *clip_id,
                            old_gain: og,
                            old_volume: ov,
                            old_pan: op,
                            old_fade_in: ofi,
                            old_fade_out: ofo,
                            old_channel_config: occ,
                            new_gain: *new_gain,
                            new_volume: *new_volume,
                            new_pan: *new_pan,
                            new_fade_in: *new_fade_in,
                            new_fade_out: *new_fade_out,
                            new_channel_config: cc,
                        },
                    });
                    continue;
                }
                AppCommand::BeginEffectDrag { ref key, clip_id, effect_index, ref old_effect } => {
                    self.state.history.begin_drag(
                        key.clone(),
                        HistoryEntry {
                            sequence_idx: self.state.active_sequence_idx,
                            command: EditCommand::EditBuiltinEffect {
                                clip_id: *clip_id,
                                effect_index: *effect_index,
                                old_effect: old_effect.clone(),
                                new_effect: old_effect.clone(), // placeholder, replaced at end_drag
                            },
                        },
                    );
                    continue;
                }
                AppCommand::EndEffectDrag { ref key, clip_id, effect_index, ref new_effect } => {
                    let _ = key; // key is used to match begin_drag
                    // Read the old_effect from the pending entry
                    let old_effect = self.state.history.pending_old_entry_ref()
                        .and_then(|e| match &e.command {
                            EditCommand::EditBuiltinEffect { old_effect, .. } => Some(old_effect.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| new_effect.clone());
                    self.state.history.end_drag(HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
                        command: EditCommand::EditBuiltinEffect {
                            clip_id: *clip_id,
                            effect_index: *effect_index,
                            old_effect,
                            new_effect: new_effect.clone(),
                        },
                    });
                    continue;
                }
                AppCommand::ReorderBuiltinEffect { clip_id, from_index, to_index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let from = *from_index;
                            let to = *to_index;
                            if from < clip.builtin_effects.len() && to < clip.builtin_effects.len() {
                                let fx = clip.builtin_effects.remove(from);
                                clip.builtin_effects.insert(to, fx);
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    if self.state.playback != PlaybackState::Playing {
                        let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                    }
                    continue;
                }

                // Audio DSP effects
                AppCommand::AddAudioEffect { clip_id, ref effect_type } => {
                    // Build default instance from effect descriptors
                    let instance = {
                        let default_params: Vec<(String, f32)> = klipp_audio::dsp::create_effect(effect_type, 48000)
                            .map(|fx| fx.param_descriptors().iter().map(|d| (d.name.to_string(), d.default)).collect())
                            .unwrap_or_default();
                        klipp_core::AudioEffectInstance {
                            effect_type: effect_type.clone(),
                            enabled: true,
                            wet_dry: 1.0,
                            params: default_params,
                        }
                    };
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.audio_effects.push(instance.clone());
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    continue;
                }
                AppCommand::RemoveAudioEffect { clip_id, effect_index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if *effect_index < clip.audio_effects.len() {
                                clip.audio_effects.remove(*effect_index);
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    continue;
                }
                AppCommand::UpdateAudioEffectParam { clip_id, effect_index, ref param_name, value } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(inst) = clip.audio_effects.get_mut(*effect_index) {
                                if let Some(p) = inst.params.iter_mut().find(|(k, _)| k == param_name) {
                                    p.1 = *value;
                                } else {
                                    inst.params.push((param_name.clone(), *value));
                                }
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::ToggleAudioEffect { clip_id, effect_index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(inst) = clip.audio_effects.get_mut(*effect_index) {
                                inst.enabled = !inst.enabled;
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetAudioEffectWetDry { clip_id, effect_index, wet_dry } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(inst) = clip.audio_effects.get_mut(*effect_index) {
                                inst.wet_dry = *wet_dry;
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::ReorderAudioEffect { clip_id, from_index, to_index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let from = *from_index;
                            let to = *to_index;
                            if from < clip.audio_effects.len() && to < clip.audio_effects.len() {
                                let fx = clip.audio_effects.remove(from);
                                clip.audio_effects.insert(to, fx);
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }

                // Parameter modifiers (behaviors)
                AppCommand::AddModifier { clip_id, ref param, ref modifier } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.param_modifiers.entry(param.clone()).or_default().modifiers.push(modifier.clone());
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveModifier { clip_id, ref param, modifier_index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(stack) = clip.param_modifiers.get_mut(param) {
                                if *modifier_index < stack.modifiers.len() {
                                    stack.modifiers.remove(*modifier_index);
                                }
                                if stack.modifiers.is_empty() {
                                    clip.param_modifiers.remove(param);
                                }
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UpdateModifier { clip_id, ref param, modifier_index, ref modifier } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(stack) = clip.param_modifiers.get_mut(param) {
                                if let Some(m) = stack.modifiers.get_mut(*modifier_index) {
                                    *m = modifier.clone();
                                }
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::ToggleModifier { clip_id, ref param, modifier_index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(stack) = clip.param_modifiers.get_mut(param) {
                                if let Some(m) = stack.modifiers.get_mut(*modifier_index) {
                                    match m {
                                        klipp_core::Modifier::Oscillate { enabled, .. } |
                                        klipp_core::Modifier::Wiggle { enabled, .. } |
                                        klipp_core::Modifier::Throw { enabled, .. } |
                                        klipp_core::Modifier::Ramp { enabled, .. } |
                                        klipp_core::Modifier::Loop { enabled, .. } |
                                        klipp_core::Modifier::PingPong { enabled, .. } |
                                        klipp_core::Modifier::Clamp { enabled, .. } => {
                                            *enabled = !*enabled;
                                        }
                                    }
                                }
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SequenceUpdated);
                    self.state.dirty = true;
                    continue;
                }

                // Transitions
                AppCommand::AddTransition { track_index, clip_a_id, clip_b_id, kind_id, duration_frames } => {
                    let ti = *track_index;
                    let kind = klipp_engine::transition::transition_kind_from_id(*kind_id);
                    let fps_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(ti) {
                        let duration = klipp_core::RationalTime::from_frames(*duration_frames as i64, fps_rate);
                        // Remove any existing transition between these two clips
                        track.transitions.retain(|t| {
                            !(t.clip_a_id == *clip_a_id && t.clip_b_id == *clip_b_id)
                        });
                        track.transitions.push(klipp_timeline::Transition {
                            id: klipp_core::TransitionId::new(),
                            kind,
                            duration,
                            clip_a_id: *clip_a_id,
                            clip_b_id: *clip_b_id,
                        });
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveTransition { track_index, transition_id } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        track.transitions.retain(|t| t.id != *transition_id);
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTransitionDuration { track_index, transition_id, duration_frames } => {
                    let fps_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        if let Some(t) = track.transitions.iter_mut().find(|t| t.id == *transition_id) {
                            t.duration = klipp_core::RationalTime::from_frames(*duration_frames as i64, fps_rate);
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTransitionKind { track_index, transition_id, kind_id } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                        if let Some(t) = track.transitions.iter_mut().find(|t| t.id == *transition_id) {
                            t.kind = klipp_engine::transition::transition_kind_from_id(*kind_id);
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }

                // Title clips
                AppCommand::CreateTitleClip { track_index, timeline_in, duration_frames } => {
                    let ti = *track_index;
                    // Check if this track was just created (it's a fresh text track with no clips)
                    let created_track = self.state.sequences[self.state.active_sequence_idx].tracks
                        .get(ti)
                        .map_or(false, |t| t.kind == klipp_timeline::TrackKind::Text && t.clips.is_empty());
                    let fps_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
                    let duration = klipp_core::RationalTime::from_frames(*duration_frames as i64, fps_rate);
                    let timeline_out = *timeline_in + duration;
                    let title_data = self.state.pending_title_data.take()
                        .unwrap_or_default();
                    let mut clip = klipp_timeline::Clip {
                        id: klipp_core::ClipId::new(),
                        kind: klipp_timeline::ClipKind::Title(title_data),
                        timeline_in: *timeline_in,
                        timeline_out,
                        source_in: klipp_core::RationalTime::from_frames(0, fps_rate),
                        source_out: duration,
                        node_graph: klipp_nodes::NodeGraph::default(),
                        builtin_effects: Vec::new(),
                        effects: Vec::new(),
                        linked_clip_id: None,
                        group_id: None,
                        magnetic_anchor: None,
                        gain: 1.0,
                        volume: 1.0,
                        pan: 0.0,
                        fade_in_secs: 0.0,
                        fade_out_secs: 0.0,
                        fade_in_curve: klipp_timeline::FadeCurve::default(),
                        fade_out_curve: klipp_timeline::FadeCurve::default(),
                        channel_config: klipp_timeline::ChannelConfig::default(),
                        volume_keyframes: Vec::new(),
                        pan_keyframes: Vec::new(),
                        gain_keyframes: Vec::new(),
                        opacity_keyframes: Vec::new(),
                        transform_pos_x_keyframes: Vec::new(),
                        transform_pos_y_keyframes: Vec::new(),
                        transform_scale_keyframes: Vec::new(),
                        transform_rotation_keyframes: Vec::new(),
                        clip_markers: Vec::new(),
                        effect_keyframes: std::collections::HashMap::new(),
                        role: klipp_core::ClipRole::default(),
                        param_modifiers: std::collections::HashMap::new(),
                        speed: 1.0,
                        audio_effects: Vec::new(),
                    };
                    // Apply baked-in title animation keyframes
                    if let Some(anim_str) = self.state.pending_title_animation.take() {
                        let clip_dur = 5.0_f64;
                        apply_title_animation(&mut clip, &anim_str, clip_dur);
                        // Clear title overlay so it starts invisible for animated titles
                        self.state.clear_title_overlay = true;
                    }
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(ti) {
                        track.clips.push(clip.clone());
                        track.clips.sort_by_key(|c| c.timeline_in.value);
                    }
                    self.state.history.push(crate::history::HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
                        command: crate::history::EditCommand::AddTitleClip {
                            clip,
                            track_index: ti,
                            created_track,
                        },
                    });
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UpdateTitleData { clip_id, text, font_size, font_name, font_style, color } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                            if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                                td.text = text.clone();
                                td.font_size = *font_size;
                                td.font_name = font_name.clone();
                                td.font_style = match font_style {
                                    1 => klipp_timeline::FontStyle::Bold,
                                    2 => klipp_timeline::FontStyle::Italic,
                                    3 => klipp_timeline::FontStyle::BoldItalic,
                                    _ => klipp_timeline::FontStyle::Regular,
                                };
                                td.color = *color;
                            }
                        }
                    }
                    self.render_title_to_viewer(*clip_id);
                    self.sync_sequence_to_engine();
                    // Keep engine playhead in sync (needed for V2 overlay during playback)
                    let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTitleAlignment { clip_id, alignment } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                            if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                                td.alignment = match alignment {
                                    0 => klipp_timeline::TextAlignment::Left,
                                    2 => klipp_timeline::TextAlignment::Right,
                                    _ => klipp_timeline::TextAlignment::Center,
                                };
                            }
                        }
                    }
                    self.render_title_to_viewer(*clip_id);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTitlePosition { clip_id, x, y } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                            if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                                td.position = klipp_timeline::TitlePosition { x: *x, y: *y };
                            }
                        }
                    }
                    self.render_title_to_viewer(*clip_id);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTitleBackground { clip_id, bg_kind, color } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                            if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                                td.background = match bg_kind {
                                    1 => klipp_timeline::TitleBackground::Color { color: *color, padding: 20.0 },
                                    2 => klipp_timeline::TitleBackground::FullFrame { color: *color },
                                    _ => klipp_timeline::TitleBackground::None,
                                };
                            }
                        }
                    }
                    self.render_title_to_viewer(*clip_id);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTitleShadow { clip_id, enabled, color, offset_x, offset_y } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                            if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                                if *enabled {
                                    td.shadow = Some(klipp_timeline::TitleShadow {
                                        color: *color,
                                        offset_x: *offset_x,
                                        offset_y: *offset_y,
                                        blur: 0.0,
                                    });
                                } else {
                                    td.shadow = None;
                                }
                            }
                        }
                    }
                    self.render_title_to_viewer(*clip_id);
                    self.sync_sequence_to_engine();
                    let _ = self.cmd_tx.send(AppCommand::SetPlayhead(self.state.playhead));
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTitleFadeDuration { duration } => {
                    self.state.title_fade_duration = *duration;
                    continue;
                }
                AppCommand::ClearTitleOverlay => {
                    self.viewer.title_overlay_texture = None;
                    continue;
                }

                // Phase 4: Blade
                AppCommand::BladeClip { track_index, clip_id, cut_at } => {
                    if self.state.sequences[self.state.active_sequence_idx].tracks.get(*track_index).is_some_and(|t| t.locked) { continue; }
                    let ti = *track_index;
                    let cid = *clip_id;
                    let cut = *cut_at;

                    // Find linked partner clip info BEFORE the blade modifies the sequence
                    let linked_info = self.state.sequences[self.state.active_sequence_idx].tracks[ti]
                        .find_clip(cid)
                        .and_then(|c| c.linked_clip_id)
                        .and_then(|lid| {
                            self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                                .enumerate()
                                .find_map(|(track_i, track)| {
                                    track.find_clip(lid).map(|_| (track_i, lid))
                                })
                        });

                    if self.is_magnetic_track(ti) {
                        // Magnetic blade
                        let result = klipp_timeline::magnetic::MagneticEditor::blade(
                            &mut self.state.sequences[self.state.active_sequence_idx], cid, cut);
                        self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::Magnetic(result.command) });
                        self.state.dirty = true;
                    } else {
                        // Legacy blade (existing code)
                        if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(ti) {
                            let original = track.find_clip(cid).cloned();
                            let position = track.clips.iter().position(|c| c.id == cid);
                            if let (Some(original), Some(pos)) = (original, position) {
                                if let Some((clip_a, clip_b)) = track.blade_at(cid, cut) {
                                    self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyBladeClip {
                                        track_index: ti,
                                        original_clip: original,
                                        clip_a,
                                        clip_b,
                                        position: pos,
                                    } });
                                    self.state.dirty = true;
                                }
                            }
                        }
                    }

                    // Blade the linked partner clip at the same cut point and re-link
                    if let Some((partner_track_i, partner_id)) = linked_info {
                        // Check partner clip spans the cut point and get track role
                        let partner_spans = self.state.sequences[self.state.active_sequence_idx]
                            .tracks[partner_track_i]
                            .find_clip(partner_id)
                            .is_some_and(|c| c.timeline_in < cut && c.timeline_out > cut);
                        let partner_is_magnetic = self.is_magnetic_track(partner_track_i);

                        if partner_spans {
                            if partner_is_magnetic {
                                let result = klipp_timeline::magnetic::MagneticEditor::blade(
                                    &mut self.state.sequences[self.state.active_sequence_idx], partner_id, cut);
                                self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::Magnetic(result.command) });
                            } else {
                                let track = &mut self.state.sequences[self.state.active_sequence_idx].tracks[partner_track_i];
                                let original = track.find_clip(partner_id).cloned();
                                let position = track.clips.iter().position(|c| c.id == partner_id);
                                if let (Some(original), Some(pos)) = (original, position) {
                                    if let Some((pa, pb)) = track.blade_at(partner_id, cut) {
                                        self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyBladeClip {
                                            track_index: partner_track_i,
                                            original_clip: original,
                                            clip_a: pa,
                                            clip_b: pb,
                                            position: pos,
                                        } });
                                    }
                                }
                            }

                            // Re-link: find the two halves on each track and cross-link them
                            let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                            let video_halves: Vec<ClipId> = seq.tracks[ti].clips.iter()
                                .filter(|c| c.timeline_out == cut || c.timeline_in == cut)
                                .map(|c| c.id)
                                .collect();
                            let audio_halves: Vec<ClipId> = seq.tracks[partner_track_i].clips.iter()
                                .filter(|c| c.timeline_out == cut || c.timeline_in == cut)
                                .map(|c| c.id)
                                .collect();

                            for vid_id in &video_halves {
                                let vid_out = seq.tracks[ti].find_clip(*vid_id).map(|c| c.timeline_out);
                                for aud_id in &audio_halves {
                                    let aud_out = seq.tracks[partner_track_i].find_clip(*aud_id).map(|c| c.timeline_out);
                                    if vid_out == aud_out {
                                        if let Some(vc) = seq.tracks[ti].find_clip_mut(*vid_id) {
                                            vc.linked_clip_id = Some(*aud_id);
                                        }
                                        if let Some(ac) = seq.tracks[partner_track_i].find_clip_mut(*aud_id) {
                                            ac.linked_clip_id = Some(*vid_id);
                                        }
                                    }
                                }
                            }
                        }
                    }

                    self.sync_sequence_to_engine();
                    continue;
                }
                // Phase 4: Cut/Copy/Paste
                AppCommand::CutClip { clip_id } => {
                    let cid = *clip_id;
                    let track_index = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .position(|t| t.find_clip(cid).is_some())
                        .unwrap_or(0);
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(track_index) {
                        if let Some(clip) = track.find_clip(cid).cloned() {
                            if let Some(media) = clip.media_id().and_then(|mid| self.state.media_pool.get(&mid)).cloned() {
                                let pos = track.clips.iter().position(|c| c.id == cid).unwrap_or(0);
                                let shifted: Vec<_> = track.clips[pos + 1..].iter()
                                    .map(|c| (c.id, c.timeline_in, c.timeline_out))
                                    .collect();
                                self.state.clipboard = Some(ClipboardContent {
                                    clips: vec![(clip.clone(), media.clone())],
                                    source_track_index: track_index,
                                });
                                track.ripple_remove(cid);
                                self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyCutClip {
                                    clip,
                                    media_item: media,
                                    track_index,
                                    position: pos,
                                    shifted_clips: shifted,
                                } });
                                if self.state.selected_clip == Some(cid) {
                                    self.state.selected_clip = None;
                                }
                                self.state.dirty = true;
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::CopyClip { clip_id } => {
                    let cid = *clip_id;
                    let track_index = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .position(|t| t.find_clip(cid).is_some())
                        .unwrap_or(0);
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get(track_index) {
                        if let Some(clip) = track.find_clip(cid).cloned() {
                            if let Some(media) = clip.media_id().and_then(|mid| self.state.media_pool.get(&mid)).cloned() {
                                self.state.clipboard = Some(ClipboardContent {
                                    clips: vec![(clip, media)],
                                    source_track_index: track_index,
                                });
                            }
                        }
                    }
                    continue;
                }
                AppCommand::PasteClip => {
                    if let Some(ref cb) = self.state.clipboard.clone() {
                        for (clip, _media) in &cb.clips {
                            let track_index = cb.source_track_index;
                            if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(track_index) {
                                let new_id = klipp_core::ClipId::new();
                                let timeline_in = self.state.playhead;
                                let dur = clip.duration();
                                let new_clip = klipp_timeline::Clip {
                                    id: new_id,
                                    kind: clip.kind.clone(),
                                    timeline_in,
                                    timeline_out: timeline_in + dur,
                                    source_in: clip.source_in,
                                    source_out: clip.source_out,
                                    node_graph: clip.node_graph.clone(),
                                    effects: clip.effects.clone(),
                                    builtin_effects: clip.builtin_effects.clone(),
                                    linked_clip_id: None,
                                    group_id: None,
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
                                    clip_markers: clip.clip_markers.clone(),
                                    effect_keyframes: clip.effect_keyframes.clone(),
                                    role: clip.role,
                                    param_modifiers: clip.param_modifiers.clone(),
                                    speed: clip.speed,
                                    audio_effects: clip.audio_effects.clone(),
                                };
                                Self::overwrite_insert_clip_on_track(track, new_clip.clone());
                                self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::LegacyPasteClip {
                                    clip: new_clip,
                                    track_index,
                                } });
                                self.state.selected_clip = Some(new_id);
                                self.state.dirty = true;
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                // Phase 4: Sequence markers
                AppCommand::SetSequenceInPoint(tc) => {
                    self.state.sequences[self.state.active_sequence_idx].in_point = Some(*tc);
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::SetSequenceOutPoint(tc) => {
                    self.state.sequences[self.state.active_sequence_idx].out_point = Some(*tc);
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::ClearSequenceInPoint => {
                    self.state.sequences[self.state.active_sequence_idx].in_point = None;
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::ClearSequenceOutPoint => {
                    self.state.sequences[self.state.active_sequence_idx].out_point = None;
                    self.sync_sequence_to_engine();
                    continue;
                }
                // Phase 4: Source monitor
                AppCommand::LoadSourceMedia { media_id } => {
                    // Clear stale source I/O from previous source clip
                    if let Some(old_mid) = self.state.source_media_id {
                        if old_mid != *media_id {
                            if let Some(old_item) = self.state.media_pool.get_mut(&old_mid) {
                                old_item.default_source_in = None;
                                old_item.default_source_out = None;
                            }
                        }
                    }
                    self.state.source_media_id = Some(*media_id);
                    self.state.source_playhead = Timecode::zero();
                    self.state.viewer_mode = ViewerMode::Source;
                    continue;
                }
                AppCommand::SetSourceInPoint(tc) => {
                    if let Some(mid) = self.state.source_media_id {
                        if let Some(item) = self.state.media_pool.get_mut(&mid) {
                            item.default_source_in = Some(*tc);
                        }
                    }
                    continue;
                }
                AppCommand::SetSourceOutPoint(tc) => {
                    if let Some(mid) = self.state.source_media_id {
                        if let Some(item) = self.state.media_pool.get_mut(&mid) {
                            item.default_source_out = Some(*tc);
                        }
                    }
                    continue;
                }
                AppCommand::InsertSourceAtPlayhead | AppCommand::OverwriteEdit => {
                    if let Some(mid) = self.state.source_media_id {
                        if let Some(item) = self.state.media_pool.get(&mid).cloned() {
                            let raw_in = item.default_source_in.unwrap_or(Timecode { value: 0, rate: item.duration.rate });
                            let raw_out = item.default_source_out.unwrap_or(item.duration);
                            // If in/out are equal or both zero, use the full clip
                            let (source_in, source_out) = if raw_in >= raw_out {
                                (Timecode { value: 0, rate: item.duration.rate }, item.duration)
                            } else {
                                (raw_in, raw_out)
                            };
                            let dur = source_out - source_in;
                            let is_audio_only = item.video_codec.is_empty();
                            let target_kind = if is_audio_only {
                                klipp_timeline::TrackKind::Audio
                            } else {
                                klipp_timeline::TrackKind::Video
                            };
                            let timeline_in = self.state.sequences[self.state.active_sequence_idx].in_point.unwrap_or(self.state.playhead);
                            if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                                .find(|t| t.kind == target_kind)
                            {
                                let new_id = klipp_core::ClipId::new();
                                let new_clip = klipp_timeline::Clip {
                                    id: new_id,
                                    kind: klipp_timeline::ClipKind::Media { media_id: mid },
                                    timeline_in,
                                    timeline_out: timeline_in + dur,
                                    source_in,
                                    source_out,
                                    node_graph: klipp_nodes::NodeGraph::default(),
                                    effects: Vec::new(),
                                    builtin_effects: Vec::new(),
                                    linked_clip_id: None,
                                    group_id: None,
                                    magnetic_anchor: None,
                                    gain: 1.0,
                                    volume: 1.0,
                                    pan: 0.0,
                                    fade_in_secs: 0.0,
                                    fade_out_secs: 0.0,
                                    fade_in_curve: klipp_timeline::FadeCurve::default(),
                                    fade_out_curve: klipp_timeline::FadeCurve::default(),
                                    channel_config: klipp_timeline::ChannelConfig::default(),
                                    volume_keyframes: Vec::new(),
                                    pan_keyframes: Vec::new(),
                                    gain_keyframes: Vec::new(),
                                    opacity_keyframes: Vec::new(),
                                    transform_pos_x_keyframes: Vec::new(),
                                    transform_pos_y_keyframes: Vec::new(),
                                    transform_scale_keyframes: Vec::new(),
                                    transform_rotation_keyframes: Vec::new(),
                                    clip_markers: Vec::new(),
                                    effect_keyframes: std::collections::HashMap::new(),
                                    role: klipp_core::ClipRole::default(),
                                    param_modifiers: std::collections::HashMap::new(),
                                    speed: 1.0,
                                    audio_effects: Vec::new(),
                                };
                                Self::overwrite_insert_clip_on_track(track, new_clip);
                                self.state.selected_clip = Some(new_id);
                                self.state.dirty = true;
                                self.sync_sequence_to_engine();
                            }
                        }
                    }
                    continue;
                }
                AppCommand::InsertEdit => {
                    // Ripple insert: push existing clips right, then insert source clip
                    if let Some(mid) = self.state.source_media_id {
                        if let Some(item) = self.state.media_pool.get(&mid).cloned() {
                            let raw_in = item.default_source_in.unwrap_or(Timecode { value: 0, rate: item.duration.rate });
                            let raw_out = item.default_source_out.unwrap_or(item.duration);
                            let (source_in, source_out) = if raw_in >= raw_out {
                                (Timecode { value: 0, rate: item.duration.rate }, item.duration)
                            } else {
                                (raw_in, raw_out)
                            };
                            let dur = source_out - source_in;
                            let is_audio_only = item.video_codec.is_empty();
                            let target_kind = if is_audio_only {
                                klipp_timeline::TrackKind::Audio
                            } else {
                                klipp_timeline::TrackKind::Video
                            };
                            let timeline_in = self.state.sequences[self.state.active_sequence_idx].in_point.unwrap_or(self.state.playhead);
                            if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                                .find(|t| t.kind == target_kind)
                            {
                                // Ripple: shift all clips at or after timeline_in to the right by dur
                                for clip in &mut track.clips {
                                    if clip.timeline_in >= timeline_in {
                                        clip.timeline_in += dur;
                                        clip.timeline_out += dur;
                                    } else if clip.timeline_out > timeline_in {
                                        // Clip spans the insert point — split it
                                        // For simplicity, just shift it right (overwrite would split)
                                        clip.timeline_in += dur;
                                        clip.timeline_out += dur;
                                    }
                                }
                                let new_id = klipp_core::ClipId::new();
                                let new_clip = klipp_timeline::Clip {
                                    id: new_id,
                                    kind: klipp_timeline::ClipKind::Media { media_id: mid },
                                    timeline_in,
                                    timeline_out: timeline_in + dur,
                                    source_in,
                                    source_out,
                                    node_graph: klipp_nodes::NodeGraph::default(),
                                    effects: Vec::new(),
                                    builtin_effects: Vec::new(),
                                    linked_clip_id: None,
                                    group_id: None,
                                    magnetic_anchor: None,
                                    gain: 1.0,
                                    volume: 1.0,
                                    pan: 0.0,
                                    fade_in_secs: 0.0,
                                    fade_out_secs: 0.0,
                                    fade_in_curve: klipp_timeline::FadeCurve::default(),
                                    fade_out_curve: klipp_timeline::FadeCurve::default(),
                                    channel_config: klipp_timeline::ChannelConfig::default(),
                                    volume_keyframes: Vec::new(),
                                    pan_keyframes: Vec::new(),
                                    gain_keyframes: Vec::new(),
                                    opacity_keyframes: Vec::new(),
                                    transform_pos_x_keyframes: Vec::new(),
                                    transform_pos_y_keyframes: Vec::new(),
                                    transform_scale_keyframes: Vec::new(),
                                    transform_rotation_keyframes: Vec::new(),
                                    clip_markers: Vec::new(),
                                    effect_keyframes: std::collections::HashMap::new(),
                                    role: klipp_core::ClipRole::default(),
                                    param_modifiers: std::collections::HashMap::new(),
                                    speed: 1.0,
                                    audio_effects: Vec::new(),
                                };
                                track.clips.push(new_clip);
                                track.sort_clips();
                                self.state.selected_clip = Some(new_id);
                                self.state.dirty = true;
                                self.sync_sequence_to_engine();
                            }
                        }
                    }
                    continue;
                }
                AppCommand::LiftEdit => {
                    // Remove content between sequence in/out, leave gap
                    if let (Some(in_pt), Some(out_pt)) = (self.state.sequences[self.state.active_sequence_idx].in_point, self.state.sequences[self.state.active_sequence_idx].out_point) {
                        if in_pt < out_pt {
                            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                                Self::lift_range(track, in_pt, out_pt);
                            }
                            self.state.dirty = true;
                            self.sync_sequence_to_engine();
                        }
                    }
                    continue;
                }
                AppCommand::ExtractEdit => {
                    // Remove content between sequence in/out, ripple left to close gap
                    if let (Some(in_pt), Some(out_pt)) = (self.state.sequences[self.state.active_sequence_idx].in_point, self.state.sequences[self.state.active_sequence_idx].out_point) {
                        if in_pt < out_pt {
                            let gap = out_pt - in_pt;
                            for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                                Self::lift_range(track, in_pt, out_pt);
                                // Ripple: shift clips at or after out_pt left by gap
                                for clip in &mut track.clips {
                                    if clip.timeline_in >= out_pt {
                                        clip.timeline_in -= gap;
                                        clip.timeline_out -= gap;
                                    }
                                }
                            }
                            self.state.dirty = true;
                            self.sync_sequence_to_engine();
                        }
                    }
                    continue;
                }
                // Magnetic: Ripple trim head to playhead
                AppCommand::RippleTrimHead { clip_id } => {
                    if self.clip_track_is_locked(clip_id) { continue; }
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    if let Some((track_idx, clip_idx)) = seq.find_clip(*clip_id) {
                        let clip = &seq.tracks[track_idx].clips[clip_idx];
                        let playhead = self.state.playhead;
                        // Only trim if playhead is inside the clip
                        if playhead > clip.timeline_in && playhead < clip.timeline_out {
                            let new_timeline_in = playhead;
                            if self.is_magnetic_track(track_idx) {
                                let result = klipp_timeline::magnetic::MagneticEditor::trim_head(
                                    &mut self.state.sequences[self.state.active_sequence_idx], *clip_id, new_timeline_in);
                                self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::Magnetic(result.command) });
                            } else {
                                // Non-magnetic: adjust clip in-place
                                let clip = &mut self.state.sequences[self.state.active_sequence_idx].tracks[track_idx].clips[clip_idx];
                                let delta = new_timeline_in - clip.timeline_in;
                                clip.source_in = clip.source_in + delta;
                                clip.timeline_in = new_timeline_in;
                            }
                            self.state.dirty = true;
                            self.sync_sequence_to_engine();
                        }
                    }
                    continue;
                }
                // Magnetic: Ripple trim tail to playhead
                AppCommand::RippleTrimTail { clip_id } => {
                    if self.clip_track_is_locked(clip_id) { continue; }
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    if let Some((track_idx, clip_idx)) = seq.find_clip(*clip_id) {
                        let clip = &seq.tracks[track_idx].clips[clip_idx];
                        let playhead = self.state.playhead;
                        // Only trim if playhead is inside the clip
                        if playhead > clip.timeline_in && playhead < clip.timeline_out {
                            let new_timeline_out = playhead;
                            if self.is_magnetic_track(track_idx) {
                                let result = klipp_timeline::magnetic::MagneticEditor::trim_tail(
                                    &mut self.state.sequences[self.state.active_sequence_idx], *clip_id, new_timeline_out);
                                self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::Magnetic(result.command) });
                            } else {
                                // Non-magnetic: adjust clip in-place
                                let clip = &mut self.state.sequences[self.state.active_sequence_idx].tracks[track_idx].clips[clip_idx];
                                let delta = new_timeline_out - clip.timeline_out;
                                clip.source_out = clip.source_out + delta;
                                clip.timeline_out = new_timeline_out;
                            }
                            self.state.dirty = true;
                            self.sync_sequence_to_engine();
                        }
                    }
                    continue;
                }
                // Magnetic: Insert 1-second gap at playhead
                AppCommand::InsertGapAtPlayhead => {
                    let playhead = self.state.playhead;
                    let rate = if playhead.rate > 1 { playhead.rate } else { 24 };
                    let one_sec = klipp_core::RationalTime::from_frames(rate as i64, rate);
                    let result = klipp_timeline::magnetic::MagneticEditor::insert_gap(
                        &mut self.state.sequences[self.state.active_sequence_idx], one_sec, playhead);
                    self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: EditCommand::Magnetic(result.command) });
                    self.state.dirty = true;
                    self.sync_sequence_to_engine();
                    continue;
                }
                // Phase 4: Proxy
                AppCommand::GenerateProxy { media_id } => {
                    self.spawn_proxy_generation(*media_id);
                    continue;
                }
                AppCommand::CancelProxy { media_id } => {
                    // Kill all running proxy ffmpeg processes — we don't track which PID
                    // maps to which media_id, so kill all running ones. This is safe because
                    // proxy generation is infrequent and usually one-at-a-time.
                    for pid_arc in &self.proxy_children {
                        let pid = pid_arc.load(std::sync::atomic::Ordering::Relaxed);
                        if pid != 0 {
                            #[cfg(unix)]
                            {
                                use std::process::Command;
                                let _ = Command::new("kill").arg(pid.to_string()).spawn();
                            }
                        }
                    }
                    self.state.proxy_generating.remove(media_id);
                    let filename = self.state.media_pool.get(media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_default();
                    tracing::info!("Proxy generation cancelled for {}", filename);
                    self.state.proxy_status_message = Some((
                        format!("Proxy cancelled for {}", filename),
                        std::time::Instant::now(),
                    ));
                    continue;
                }
                AppCommand::SetUseProxies(enabled) => {
                    self.state.use_proxies = *enabled;
                    // Forward to engine (don't continue — let it send to cmd_tx)
                }
                AppCommand::RelinkMedia { media_id, new_path } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        item.path = new_path.clone();
                        item.offline = !new_path.exists();
                    }
                    self.state.offline_media_ids.retain(|id| id != media_id);
                    continue;
                }
                // Media pool management
                AppCommand::RemoveFromPool { media_id } => {
                    let usage = media_usage_count(*media_id, &self.state.sequences);
                    if usage > 0 {
                        self.state.show_remove_media_confirm = Some(*media_id);
                    } else if let Some(item) = self.state.media_pool.get(media_id).cloned() {
                        self.state.history.push(crate::history::HistoryEntry {
                            sequence_idx: self.state.active_sequence_idx,
                            command: crate::history::EditCommand::RemoveMediaFromPool {
                                item,
                                clip_count: 0,
                            },
                        });
                        self.state.media_pool.remove(media_id);
                        self.state.dirty = true;
                        self.sync_media_pool_to_engine();
                    }
                    continue;
                }
                AppCommand::ConfirmRemoveFromPool { media_id } => {
                    // End audio skim if the removed media might be active
                    if self.state.skim_audio_session.is_some() {
                        if let Some(ref session) = self.state.skim_audio_session {
                            session.stop();
                        }
                        klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                        klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                        self.state.skim_audio_session = None;
                    }
                    let clip_count = media_usage_count(*media_id, &self.state.sequences);
                    if let Some(item) = self.state.media_pool.get(media_id).cloned() {
                        self.state.history.push(crate::history::HistoryEntry {
                            sequence_idx: self.state.active_sequence_idx,
                            command: crate::history::EditCommand::RemoveMediaFromPool {
                                item,
                                clip_count,
                            },
                        });
                    }
                    self.state.media_pool.remove(media_id);
                    self.state.show_remove_media_confirm = None;
                    self.state.dirty = true;
                    self.sync_media_pool_to_engine();
                    continue;
                }
                AppCommand::SetMediaRating { media_id, rating } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        let old_rating = item.rating;
                        let new_rating = (*rating).min(5);
                        if old_rating != new_rating {
                            item.rating = new_rating;
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::SetMediaRating {
                                    media_id: *media_id,
                                    old_rating,
                                    new_rating,
                                },
                            });
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::SetMediaTags { media_id, tags } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        let old_tags = item.tags.clone();
                        if old_tags != *tags {
                            item.tags = tags.clone();
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::SetMediaTags {
                                    media_id: *media_id,
                                    old_tags,
                                    new_tags: tags.clone(),
                                },
                            });
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::SetMediaColorLabel { media_id, label } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        let old_label = item.color_label.to_u8();
                        if old_label != *label {
                            item.color_label = klipp_media::ColorLabel::from_u8(*label);
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::SetMediaColorLabel {
                                    media_id: *media_id,
                                    old_label,
                                    new_label: *label,
                                },
                            });
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::DeleteUnusedMedia => {
                    let unused_count = self.state.media_pool.all()
                        .filter(|item| media_usage_count(item.id, &self.state.sequences) == 0)
                        .count();
                    if unused_count == 0 {
                        self.state.toast.show("All media is used in the timeline");
                    } else {
                        let total_bytes: u64 = self.state.media_pool.all()
                            .filter(|item| media_usage_count(item.id, &self.state.sequences) == 0)
                            .map(|m| m.file_size_bytes)
                            .sum();
                        self.state.show_delete_unused_confirm = Some((unused_count, total_bytes));
                    }
                    continue;
                }
                AppCommand::ConfirmDeleteUnusedMedia => {
                    let unused_ids: Vec<klipp_core::MediaId> = self.state.media_pool.all()
                        .filter(|item| media_usage_count(item.id, &self.state.sequences) == 0)
                        .map(|m| m.id)
                        .collect();
                    for id in &unused_ids {
                        if let Some(item) = self.state.media_pool.get(id).cloned() {
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::RemoveMediaFromPool {
                                    item,
                                    clip_count: 0,
                                },
                            });
                        }
                        self.state.media_pool.remove(id);
                    }
                    self.state.show_delete_unused_confirm = None;
                    self.state.dirty = true;
                    self.sync_media_pool_to_engine();
                    self.state.toast.show(format!("Removed {} unused media items", unused_ids.len()));
                    continue;
                }
                AppCommand::RevealInFileManager { path } => {
                    if let Some(parent) = path.parent() {
                        #[cfg(target_os = "linux")]
                        {
                            let _ = std::process::Command::new("xdg-open")
                                .arg(parent)
                                .spawn();
                        }
                    }
                    continue;
                }
                AppCommand::RevealMediaInBrowser { media_id } => {
                    // Switch to Media tab
                    self.layout.active_browser_tab = crate::layout_state::BrowserTab::Media;
                    // Clear search so item is visible
                    self.browser.search_query.clear();
                    // Select the item
                    self.state.selected_media_ids.clear();
                    self.state.selected_media_ids.insert(*media_id);
                    self.state.last_clicked_media_id = Some(*media_id);
                    // Switch to correct bin
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        self.browser.media_selected_bin = item.bin.clone();
                    }
                    // Request scroll on next frame
                    self.browser.scroll_to_media_id = Some(*media_id);
                    continue;
                }
                AppCommand::ImportImageSequence { pattern, start_frame, frame_count, fps } => {
                    // Import an image sequence as a single media item.
                    // Compute duration from frame count and fps.
                    let fps_val = klipp_core::Fps::new((*fps * 1000.0) as u32, 1000);
                    let duration = klipp_core::RationalTime {
                        value: *frame_count as i64,
                        rate: fps_val.rate(),
                    };
                    // Use the pattern path as the media path (ffmpeg image2 demuxer)
                    let path = std::path::PathBuf::from(pattern);
                    let display_name = path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| format!("Sequence ({frame_count} frames)"));
                    // Probe first frame for resolution
                    let first_frame_path = pattern.replacen(
                        &format!("%0{}d", pattern.matches('0').count().max(1)),
                        &format!("{start_frame}"),
                        1,
                    );
                    let file_size: u64 = std::fs::metadata(&first_frame_path).map(|m| m.len()).unwrap_or(0)
                        * (*frame_count as u64);
                    let media_item = klipp_media::MediaItem {
                        id: klipp_core::MediaId::new(),
                        path,
                        filename: display_name,
                        duration,
                        fps: fps_val,
                        resolution: klipp_core::Resolution { width: 1920, height: 1080 }, // will be overridden by probe
                        video_codec: "ImageSequence".to_string(),
                        audio_codec: String::new(),
                        has_audio: false,
                        file_size_bytes: file_size,
                        sample_rate: None,
                        audio_channels: None,
                        offline: false,
                        is_still: false,
                        proxy_path: None,
                        proxy_ready: false,
                        default_source_in: None,
                        default_source_out: None,
                        rating: 0,
                        tags: vec!["sequence".to_string()],
                        user_notes: format!("{frame_count} frames, start #{start_frame}"),
                        bin: None,
                        bin_id: None,
                        added_at: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        fingerprint: None,
                        file_modified_at: None,
                        media_created_at: None,
                        color_label: Default::default(),
                        colour_space: Default::default(),
                        needs_reprobe: false,
                        log_format: Default::default(),
                        skim_file: None,
                        waveform_data: None,
                        audio_cache: None,
                    };
                    self.state.media_pool.add(media_item.clone());
                    self.state.history.push(crate::history::HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
                        command: crate::history::EditCommand::AddMediaToPool { item: media_item },
                    });
                    self.state.dirty = true;
                    self.sync_media_pool_to_engine();
                    self.state.toast.show(format!("Imported image sequence: {frame_count} frames at {fps}fps"));
                    continue;
                }
                AppCommand::CreateBin { name } => {
                    // Create a root-level bin in the bins collection
                    let _bin_id = self.state.media_pool.find_or_create_bin(&name);
                    tracing::info!("Bin created: {}", name);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::CreateSubBin { parent_id, name } => {
                    let bin = klipp_media::Bin {
                        id: klipp_core::BinId::new(),
                        name: name.clone(),
                        parent_id: Some(*parent_id),
                    };
                    self.state.media_pool.bins.push(bin);
                    tracing::info!("Sub-bin created: {} (parent: {})", name, parent_id);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::DeleteBin { name } => {
                    // Move all items in this bin back to root
                    let ids: Vec<MediaId> = self.state.media_pool.all()
                        .filter(|m| m.bin.as_deref() == Some(name.as_str()))
                        .map(|m| m.id)
                        .collect();
                    for id in ids {
                        if let Some(item) = self.state.media_pool.get_mut(&id) {
                            item.bin = None;
                            item.bin_id = None;
                        }
                    }
                    // Remove the bin from the collection
                    self.state.media_pool.bins.retain(|b| b.name != *name);
                    tracing::info!("Bin deleted: {}", name);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::MoveItemToBin { media_id, bin } => {
                    // Also sync bin_id from bin name
                    let new_bin_id = bin.as_ref().map(|name| self.state.media_pool.find_or_create_bin(name));
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        let from_bin = item.bin.clone();
                        if from_bin != *bin {
                            item.bin = bin.clone();
                            item.bin_id = new_bin_id;
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::MoveMediaToBin {
                                    media_id: *media_id,
                                    from_bin,
                                    to_bin: bin.clone(),
                                },
                            });
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::RenameBin { old_name, new_name } => {
                    let ids: Vec<MediaId> = self.state.media_pool.all()
                        .filter(|m| m.bin.as_deref() == Some(old_name.as_str()))
                        .map(|m| m.id)
                        .collect();
                    for id in ids {
                        if let Some(item) = self.state.media_pool.get_mut(&id) {
                            item.bin = Some(new_name.clone());
                        }
                    }
                    // Record in history for atomic undo
                    self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: crate::history::EditCommand::RenameBin {
                        old_name: old_name.clone(),
                        new_name: new_name.clone(),
                    } });
                    self.state.dirty = true;
                    tracing::info!("Bin renamed: {} -> {}", old_name, new_name);
                    continue;
                }
                AppCommand::StartSkimTranscode { media_id } => {
                    self.handle_skim_transcode(*media_id);
                    continue;
                }
                AppCommand::RegenerateSkimTranscode { media_id } => {
                    let mid = *media_id;
                    // Delete existing skim file and cache
                    if let Some(item) = self.state.media_pool.get_mut(&mid) {
                        if let Some(ref skim_path) = item.skim_file {
                            let _ = std::fs::remove_file(skim_path);
                        }
                        item.skim_file = None;
                    }
                    self.state.skim_transcoding.remove(&mid);
                    self.handle_skim_transcode(mid);
                    continue;
                }
                AppCommand::RegenerateFilmstrip { media_id } => {
                    let mid = *media_id;
                    // Delete disk cache
                    if let Some(item) = self.state.media_pool.get(&mid) {
                        if let Some(key) = klipp_media::disk_cache::compute_cache_key(&item.path) {
                            let cache_path = klipp_media::disk_cache::cache_base_dir()
                                .join("filmstrips")
                                .join(format!("{key}.strip"));
                            let _ = std::fs::remove_file(&cache_path);
                        }
                    }
                    // Clear in-memory filmstrip
                    self.state.thumbnail_cache.set_filmstrip(mid, Vec::new());
                    self.state.filmstrip_textures.remove(&mid);
                    self.state.filmstrip_in_progress.remove(&mid);
                    // Re-queue
                    if let Some(item) = self.state.media_pool.get(&mid) {
                        let path = item.path.clone();
                        let duration = item.duration;
                        self.state.filmstrip_generating = true;
                        self.state.filmstrip_generate_start = std::time::Instant::now();
                        self.spawn_filmstrip(mid, path, duration);
                    }
                    continue;
                }
                AppCommand::BrowserSkimFrame { media_id, time_secs } => {
                    // Decode a frame from the skim file at the given time
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        if let Some(skim_path) = item.skim_file.as_ref().filter(|p| p.exists()) {
                            let path = skim_path.clone();
                            let mid = *media_id;
                            let t = *time_secs;
                            let event_tx = self.proxy_event_tx.clone();
                            std::thread::spawn(move || {
                                match klipp_engine::generate_thumbnail_at_size(&path, t, 960) {
                                    Ok((rgba, w, h)) => {
                                        let _ = event_tx.send(AppEvent::BrowserSkimFrameReady {
                                            media_id: mid,
                                            rgba,
                                            width: w,
                                            height: h,
                                        });
                                    }
                                    Err(e) => {
                                        tracing::debug!("Skim frame decode failed: {}", e);
                                    }
                                }
                            });
                        }
                    }
                    continue;
                }
                // Audio skimming — continuous PCM feeder
                AppCommand::BrowserSkimAudioStart { media_id, source_path, time_secs } => {
                    // Stop any existing session
                    if let Some(ref session) = self.state.skim_audio_session {
                        session.stop();
                    }
                    self.state.skim_audio_session = None;

                    // Look up PCM path from the media item's audio cache entry (content-hash based)
                    // Fall back to media-ID-based path for older caches
                    let pcm_path = self.state.media_pool.get(media_id)
                        .and_then(|item| item.audio_cache.as_ref())
                        .map(|cache| cache.pcm_path.clone())
                        .unwrap_or_else(|| {
                            let cache_dir = dirs::cache_dir()
                                .unwrap_or_else(|| PathBuf::from("/tmp"))
                                .join("klipp")
                                .join("audio-cache");
                            cache_dir.join(format!("{}.pcm", media_id))
                        });

                    if let Some(session) = klipp_engine::skim_audio::SkimAudioSession::new(
                        &pcm_path,
                        source_path.clone(),
                        self.cpal_sample_rate,
                        self.skim_consumer.clone(),
                        self.state.master_volume,
                    ) {
                        session.seek_and_play(*time_secs);
                        self.state.skim_audio_session = Some(session);
                    }
                    continue;
                }
                AppCommand::BrowserSkimAudioUpdate { time_secs, .. } => {
                    if let Some(ref session) = self.state.skim_audio_session {
                        // Update volume dynamically
                        let vol = if self.state.master_muted { 0.0 } else { self.state.master_volume };
                        session.set_volume(vol);

                        let mode = klipp_engine::skim_audio::determine_skim_mode(self.state.skim_speed_px_per_sec);
                        if mode == klipp_engine::skim_audio::SkimMode::Playing {
                            session.seek_and_play(*time_secs);
                        } else {
                            session.silence();
                        }
                    }
                    continue;
                }
                AppCommand::BrowserSkimAudioEnd => {
                    if let Some(ref session) = self.state.skim_audio_session {
                        session.stop();
                    }
                    // Disconnect skim consumer from cpal callback
                    klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                    klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                    self.state.skim_audio_session = None;
                    self.state.skim_speed_px_per_sec = 0.0;
                    continue;
                }
                // Phase 4: Snap & Ripple toggles
                AppCommand::SetSnapEnabled(enabled) => {
                    self.state.snap_enabled = *enabled;
                    continue;
                }
                AppCommand::SetRippleEnabled(enabled) => {
                    self.state.ripple_enabled = *enabled;
                    continue;
                }
                // Phase 4: UI modes
                AppCommand::SetViewerMode(mode) => {
                    self.state.viewer_mode = *mode;
                    continue;
                }
                AppCommand::SetActiveTool(tool) => {
                    self.state.active_tool = *tool;
                    // Sync layout tool
                    self.layout.active_tool = match tool {
                        Tool::Select => crate::layout_state::EditTool::Select,
                        Tool::Blade => crate::layout_state::EditTool::Blade,
                        Tool::Range => crate::layout_state::EditTool::Range,
                    };
                    // Clear range drag state on tool change
                    self.state.range_drag = None;
                    self.state.ruler_press_origin = None;
                    continue;
                }
                AppCommand::ToggleLoopPlayback => {
                    self.state.loop_playback_enabled = !self.state.loop_playback_enabled;
                    self.prefs.playback.loop_playback = self.state.loop_playback_enabled;
                    self.prefs.save().ok();
                    let msg = if self.state.loop_playback_enabled { "Loop On" } else { "Loop Off" };
                    self.state.toast.show(msg);
                    // Forward to engine as SetLoopPlayback
                    let _ = self.cmd_tx.send(AppCommand::SetLoopPlayback(self.state.loop_playback_enabled));
                    continue;
                }
                AppCommand::SetLoopPlayback(enabled) => {
                    self.state.loop_playback_enabled = *enabled;
                    // Fall through to forward to engine
                }
                AppCommand::ToggleFullscreen => {
                    self.state.fullscreen_viewer = !self.state.fullscreen_viewer;
                    self.state.fullscreen_changed = true;
                    continue;
                }
                AppCommand::NewProject => {
                    if self.state.dirty {
                        self.state.show_unsaved_dialog = true;
                        self.state.unsaved_dialog_action = UnsavedAction::OpenNewProjectDialog;
                        continue;
                    }
                    self.new_project_dialog.reset(
                        self.prefs.general.default_project_dir.as_ref(),
                    );
                    self.state.show_new_project_dialog = true;
                    continue;
                }
                AppCommand::SaveProject => {
                    if let Some(ref path) = self.state.project_path.clone() {
                        self.do_save_project(path);
                    } else {
                        // Trigger Save As
                        if let Some(path) = rfd::FileDialog::new()
                            .set_title("Save Project")
                            .pick_folder()
                        {
                            let path = ensure_klipp_extension(path);
                            self.do_save_project(&path);
                        }
                    }
                    continue;
                }
                AppCommand::SaveProjectAs { path } => {
                    let path = ensure_klipp_extension(path.clone());
                    self.do_save_project(&path);
                    continue;
                }
                AppCommand::OpenProject { path } => {
                    if self.state.dirty {
                        self.state.show_unsaved_dialog = true;
                        self.state.unsaved_dialog_action =
                            UnsavedAction::OpenProject(path.clone());
                        continue;
                    }
                    self.do_open_project(path);
                    continue;
                }
                AppCommand::RemoveRecentProject { path } => {
                    self.prefs.general.recent_projects.retain(|rp| rp.path != *path);
                    self.prefs_dirty = true;
                    continue;
                }
                AppCommand::DeleteProject { path } => {
                    if path.exists() {
                        if let Err(e) = std::fs::remove_dir_all(path) {
                            tracing::error!("Failed to delete project: {e}");
                        } else {
                            tracing::info!("Deleted project {:?}", path);
                        }
                    }
                    self.prefs.general.recent_projects.retain(|rp| rp.path != *path);
                    self.prefs_dirty = true;
                    continue;
                }
                AppCommand::RenameProjectOnDisk { old_path, new_name } => {
                    if let Some(parent) = old_path.parent() {
                        let new_dir_name = format!("{new_name}.klipp");
                        let new_path = parent.join(&new_dir_name);
                        if new_path.exists() {
                            tracing::error!("Cannot rename: {:?} already exists", new_path);
                        } else if let Err(e) = std::fs::rename(old_path, &new_path) {
                            tracing::error!("Failed to rename project: {e}");
                        } else {
                            tracing::info!("Renamed project {:?} -> {:?}", old_path, new_path);
                            // Update recent projects entry
                            for rp in &mut self.prefs.general.recent_projects {
                                if rp.path == *old_path {
                                    rp.path = new_path.clone();
                                    rp.name = new_name.clone();
                                    break;
                                }
                            }
                            // Update current project path if this is the open project
                            if self.state.project_path.as_ref() == Some(old_path) {
                                self.state.project_path = Some(new_path);
                            }
                            self.prefs_dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::CreateProject {
                    name, location, fps_num, fps_den, width, height, sequence_name,
                } => {
                    let project_dir = location.join(format!("{name}.klipp"));
                    let fps = klipp_core::Fps::new(*fps_num, *fps_den);
                    let resolution = klipp_core::Resolution { width: *width, height: *height };
                    let seq_name = if sequence_name.is_empty() {
                        name.clone()
                    } else {
                        sequence_name.clone()
                    };
                    let seq = klipp_timeline::Sequence {
                        id: klipp_core::SequenceId::new(),
                        name: seq_name,
                        fps,
                        resolution,
                        tracks: {
                            let mut tracks = vec![
                                klipp_timeline::Track::new("Video 2", klipp_timeline::TrackKind::Video),
                                klipp_timeline::Track::new("Video 1", klipp_timeline::TrackKind::Video),
                                klipp_timeline::Track::new("Audio 1", klipp_timeline::TrackKind::Audio),
                            ];
                            tracks[1].magnetic_role = klipp_timeline::TrackRole::Primary;
                            tracks[2].magnetic_role = klipp_timeline::TrackRole::Connected;
                            tracks
                        },
                        in_point: None,
                        out_point: None,
                        markers: Vec::new(),
                        last_playhead: None,
                    };
                    self.state.sequences = vec![seq];
                    self.state.active_sequence_idx = 0;
                    self.state.fps = fps;
                    self.state.media_pool = klipp_media::MediaPool::new();
                    self.state.project_path = Some(project_dir.clone());
                    self.state.created_at = Some(klipp_project::iso8601_now());
                    self.state.dirty = false;
                    self.state.selected_clip = None;
                    self.state.selected_clips.clear();
                    self.state.hovered_clip = None;
                    self.state.playback = PlaybackState::Idle;
                    self.state.playhead = Timecode::zero();
                    self.state.thumbnail_cache = klipp_media::ThumbnailCache::new();
                    self.state.thumbnail_textures.clear();
                    self.state.filmstrip_textures.clear();
                    self.state.filmstrip_in_progress.clear();
                    self.state.filmstrip_queue.clear();
                    self.state.filmstrip_total_queued = 0;
                    self.state.filmstrip_completed = 0;
                    self.state.history = crate::history::CommandHistory::new();
                    self.sync_sequence_to_engine();
                    // Save immediately so the .klipp directory is created
                    self.do_save_project(&project_dir);
                    // Update default project dir pref
                    self.prefs.general.default_project_dir = Some(location.clone());
                    self.prefs_dirty = true;
                    tracing::info!("Created new project at {:?}", project_dir);
                    continue;
                }
                AppCommand::SetActiveSequence { sequence_idx } => {
                    if *sequence_idx < self.state.sequences.len()
                        && *sequence_idx != self.state.active_sequence_idx
                    {
                        // Save current playhead to outgoing sequence
                        let playhead_rt = klipp_core::RationalTime {
                            value: self.state.playhead.value,
                            rate: self.state.playhead.rate,
                        };
                        self.state.sequences[self.state.active_sequence_idx].last_playhead =
                            Some(playhead_rt);
                        // Switch
                        self.state.active_sequence_idx = *sequence_idx;
                        // Restore playhead from incoming sequence
                        let restored = self.state.sequences[*sequence_idx]
                            .last_playhead
                            .map(|rt| Timecode { value: rt.value, rate: rt.rate })
                            .unwrap_or(Timecode::zero());
                        self.state.playhead = restored;
                        self.state.playhead_smooth_secs = restored.to_secs_f64();
                        self.state.playhead_anchor = restored;
                        self.state.playhead_anchor_wall = std::time::Instant::now();
                        self.state.playhead_speed = 1.0;
                        // Sync fps
                        self.state.fps = self.state.sequences[*sequence_idx].fps;
                        // Stop playback
                        self.cmd_tx.send(AppCommand::Stop).ok();
                        self.sync_sequence_to_engine();
                        // Clear selection
                        self.state.selected_clip = None;
                        self.state.selected_clips.clear();
                        self.state.hovered_clip = None;
                    }
                    continue;
                }
                AppCommand::AddSequence { name, fps_num, fps_den, width, height } => {
                    let seq = klipp_timeline::Sequence {
                        id: klipp_core::SequenceId::new(),
                        name: name.clone(),
                        fps: klipp_core::Fps::new(*fps_num, *fps_den),
                        resolution: klipp_core::Resolution { width: *width, height: *height },
                        tracks: {
                            let mut tracks = vec![
                                klipp_timeline::Track::new("Video 2", klipp_timeline::TrackKind::Video),
                                klipp_timeline::Track::new("Video 1", klipp_timeline::TrackKind::Video),
                                klipp_timeline::Track::new("Audio 1", klipp_timeline::TrackKind::Audio),
                            ];
                            tracks[1].magnetic_role = klipp_timeline::TrackRole::Primary;
                            tracks[2].magnetic_role = klipp_timeline::TrackRole::Connected;
                            tracks
                        },
                        in_point: None,
                        out_point: None,
                        markers: Vec::new(),
                        last_playhead: None,
                    };
                    self.state.sequences.push(seq);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::DuplicateSequence { sequence_idx, new_name } => {
                    if *sequence_idx < self.state.sequences.len() {
                        let mut dup = self.state.sequences[*sequence_idx].clone();
                        dup.id = klipp_core::SequenceId::new();
                        dup.name = new_name.clone();
                        self.state.sequences.push(dup);
                        self.state.dirty = true;
                    }
                    continue;
                }
                AppCommand::RenameSequence { sequence_idx, new_name } => {
                    if *sequence_idx < self.state.sequences.len() {
                        self.state.sequences[*sequence_idx].name = new_name.clone();
                        self.state.dirty = true;
                    }
                    continue;
                }
                AppCommand::DeleteSequence { sequence_idx } => {
                    if self.state.sequences.len() > 1 && *sequence_idx < self.state.sequences.len() {
                        self.state.sequences.remove(*sequence_idx);
                        // Adjust active index
                        if self.state.active_sequence_idx >= self.state.sequences.len() {
                            self.state.active_sequence_idx = self.state.sequences.len() - 1;
                        } else if self.state.active_sequence_idx > *sequence_idx {
                            self.state.active_sequence_idx -= 1;
                        }
                        self.state.fps = self.state.sequences[self.state.active_sequence_idx].fps;
                        self.state.dirty = true;
                        self.sync_sequence_to_engine();
                    }
                    continue;
                }
                AppCommand::ReorderSequences { from_idx, to_idx } => {
                    let len = self.state.sequences.len();
                    if *from_idx < len && *to_idx < len && from_idx != to_idx {
                        let seq = self.state.sequences.remove(*from_idx);
                        self.state.sequences.insert(*to_idx, seq);
                        // Adjust active index
                        if self.state.active_sequence_idx == *from_idx {
                            self.state.active_sequence_idx = *to_idx;
                        } else if *from_idx < self.state.active_sequence_idx
                            && *to_idx >= self.state.active_sequence_idx
                        {
                            self.state.active_sequence_idx -= 1;
                        } else if *from_idx > self.state.active_sequence_idx
                            && *to_idx <= self.state.active_sequence_idx
                        {
                            self.state.active_sequence_idx += 1;
                        }
                        self.state.dirty = true;
                    }
                    continue;
                }
                AppCommand::CreateWorkspace { path, name } => {
                    match klipp_project::workspace::create_workspace(path, name) {
                        Ok(()) => {
                            tracing::info!("Created workspace at {:?}", path);
                            self.state.toast.show(format!("Workspace \"{name}\" created"));
                        }
                        Err(e) => {
                            tracing::error!("Failed to create workspace: {e}");
                        }
                    }
                    continue;
                }
                AppCommand::RefineFilmstrip { media_id, start_secs, end_secs, interval_secs } => {
                    // Spawn a targeted filmstrip pass for the requested range
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        let path = item.path.clone();
                        let tx = self.filmstrip_tx.clone();
                        let mid = *media_id;
                        let s = *start_secs;
                        let e = *end_secs;
                        let iv = *interval_secs;
                        std::thread::Builder::new()
                            .name("klipp-filmstrip-refine".into())
                            .spawn(move || {
                                let _ = klipp_engine::generate_filmstrip_incremental(
                                    &path,
                                    e,
                                    iv,
                                    48,
                                    |time, rgba, w, h| {
                                        if time >= s - 0.01 && time <= e + 0.01 {
                                            let _ = tx.send(FilmstripResult::Frame {
                                                media_id: mid,
                                                frame: klipp_media::FilmstripFrame {
                                                    time_secs: time,
                                                    rgba,
                                                    width: w,
                                                    height: h,
                                                },
                                            });
                                        }
                                    },
                                );
                            })
                            .ok();
                    }
                    continue;
                }
                // Dynamic tracks
                AppCommand::SyncAutoCreateTracks(enabled) => {
                    self.state.auto_create_tracks = *enabled;
                    continue;
                }
                AppCommand::AddVideoTrack => {
                    let name = self.state.sequences[self.state.active_sequence_idx].next_track_name(klipp_timeline::TrackKind::Video);
                    self.state.sequences[self.state.active_sequence_idx].add_video_track(name);
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::AddAudioTrack => {
                    let name = self.state.sequences[self.state.active_sequence_idx].next_track_name(klipp_timeline::TrackKind::Audio);
                    self.state.sequences[self.state.active_sequence_idx].add_audio_track(name);
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::AddTrackAbove { track_index } => {
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    let ti = *track_index;
                    if let Some(kind) = seq.tracks.get(ti).map(|t| t.kind) {
                        let name = seq.next_track_name(kind);
                        let track = klipp_timeline::Track::new(&name, kind);
                        seq.tracks.insert(ti, track);
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::AddTrackBelow { track_index } => {
                    let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                    let ti = *track_index;
                    if let Some(kind) = seq.tracks.get(ti).map(|t| t.kind) {
                        let name = seq.next_track_name(kind);
                        let track = klipp_timeline::Track::new(&name, kind);
                        seq.tracks.insert((ti + 1).min(seq.tracks.len()), track);
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveTrack { track_id } => {
                    let tid = *track_id;
                    let has_clips = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .find(|t| t.id == tid)
                        .map(|t| !t.clips.is_empty())
                        .unwrap_or(false);
                    if has_clips {
                        // Show confirmation dialog
                        self.state.confirm_remove_track = Some(tid);
                    } else {
                        let _ = self.state.sequences[self.state.active_sequence_idx].remove_track(tid);
                        self.sync_sequence_to_engine();
                        self.state.dirty = true;
                    }
                    continue;
                }
                AppCommand::RenameTrack { track_id, ref name } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().find(|t| t.id == *track_id) {
                        track.name = name.clone();
                    }
                    self.state.dirty = true;
                    continue;
                }
                // Markers
                AppCommand::AddMarker { position, label, color } => {
                    self.state.sequences[self.state.active_sequence_idx].markers.push(Marker {
                        id: MarkerId::new(),
                        position: *position,
                        label: label.clone(),
                        color: *color,
                    });
                    self.state.sequences[self.state.active_sequence_idx].markers.sort_by(|a, b| {
                        a.position.to_secs_f64().partial_cmp(&b.position.to_secs_f64()).unwrap()
                    });
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveMarker { marker_id } => {
                    self.state.sequences[self.state.active_sequence_idx].markers.retain(|m| m.id != *marker_id);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UpdateMarkerLabel { marker_id, label } => {
                    if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter_mut().find(|m| m.id == *marker_id) {
                        m.label = label.clone();
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UpdateMarkerColor { marker_id, color } => {
                    if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter_mut().find(|m| m.id == *marker_id) {
                        m.color = *color;
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::JumpToMarker { marker_id } => {
                    if let Some(m) = self.state.sequences[self.state.active_sequence_idx].markers.iter().find(|m| m.id == *marker_id) {
                        let pos = m.position;
                        self.state.playhead = pos;
                        let _ = self.cmd_tx.send(AppCommand::SeekTo(pos));
                    }
                    continue;
                }
                // Per-clip markers
                AppCommand::AddClipMarker { clip_id, time_secs, name, color } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.clip_markers.push(klipp_timeline::ClipMarker {
                                time_secs: *time_secs,
                                name: name.clone(),
                                color: *color,
                            });
                            clip.clip_markers.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveClipMarker { clip_id, index } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if *index < clip.clip_markers.len() {
                                clip.clip_markers.remove(*index);
                            }
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                // Linked clips
                AppCommand::UnlinkClip { clip_id } => {
                    let cid = *clip_id;
                    // Find the linked partner
                    let partner_id = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == cid)
                        .and_then(|c| c.linked_clip_id);
                    // Clear linked_clip_id on both
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(c) = track.find_clip_mut(cid) {
                            c.linked_clip_id = None;
                        }
                        if let Some(pid) = partner_id {
                            if let Some(c) = track.find_clip_mut(pid) {
                                c.linked_clip_id = None;
                            }
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::LinkClips { clip_a, clip_b } => {
                    let a = *clip_a;
                    let b = *clip_b;
                    // Set bidirectional link
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(c) = track.find_clip_mut(a) {
                            c.linked_clip_id = Some(b);
                        }
                        if let Some(c) = track.find_clip_mut(b) {
                            c.linked_clip_id = Some(a);
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
                // Track locking
                AppCommand::SetTrackLocked { track_id, locked } => {
                    if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut().find(|t| t.id == *track_id) {
                        track.locked = *locked;
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::GroupClips { clip_ids } => {
                    let gid = GroupId::new();
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        for clip in &mut track.clips {
                            if clip_ids.contains(&clip.id) {
                                clip.group_id = Some(gid);
                            }
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UngroupClips { group_id } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        for clip in &mut track.clips {
                            if clip.group_id == Some(*group_id) {
                                clip.group_id = None;
                            }
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UngroupClip { clip_id } => {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        for clip in &mut track.clips {
                            if clip.id == *clip_id {
                                clip.group_id = None;
                            }
                        }
                    }
                    self.state.dirty = true;
                    continue;
                }
                _ => {}
            }
            let _ = self.cmd_tx.send(cmd);
        }
    }

    fn do_new_project(&mut self) {
        self.state.sequences = vec![Sequence::new_default()];
        self.state.active_sequence_idx = 0;
        self.state.media_pool = klipp_media::MediaPool::new();
        self.state.project_path = None;
        self.state.dirty = false;
        self.state.selected_clip = None;
        self.state.selected_clips.clear();
        self.state.hovered_clip = None;
        self.state.playback = PlaybackState::Idle;
        self.state.playhead = Timecode::zero();
        self.state.thumbnail_cache = klipp_media::ThumbnailCache::new();
        self.state.thumbnail_textures.clear();
        self.state.filmstrip_textures.clear();
        self.state.filmstrip_in_progress.clear();
        self.state.filmstrip_queue.clear();
        self.sync_sequence_to_engine();
    }

    fn apply_undo(&mut self) {
        if let Some(entry) = self.state.history.undo() {
            if entry.sequence_idx != self.state.active_sequence_idx {
                self.state.active_sequence_idx = entry.sequence_idx;
                self.state.fps = self.state.sequences[self.state.active_sequence_idx].fps;
                let name = self.state.sequences[self.state.active_sequence_idx].name.clone();
                self.state.toast.show(format!("Undo: switched to \"{name}\""));
            }
            self.apply_edit_inverse(&entry.command);
            self.sync_sequence_to_engine();
            self.state.dirty = true;
            self.state.media_usage_cache_dirty = true;
        }
    }

    fn apply_redo(&mut self) {
        if let Some(entry) = self.state.history.redo() {
            if entry.sequence_idx != self.state.active_sequence_idx {
                self.state.active_sequence_idx = entry.sequence_idx;
                self.state.fps = self.state.sequences[self.state.active_sequence_idx].fps;
                let name = self.state.sequences[self.state.active_sequence_idx].name.clone();
                self.state.toast.show(format!("Redo: switched to \"{name}\""));
            }
            self.apply_edit_forward(&entry.command);
            self.sync_sequence_to_engine();
            self.state.dirty = true;
            self.state.media_usage_cache_dirty = true;
        }
    }

    fn apply_edit_inverse(&mut self, cmd: &EditCommand) {
        match cmd {
            EditCommand::AddTitleClip { clip, track_index, created_track } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if *created_track {
                    // Remove the entire auto-created track
                    if *track_index < seq.tracks.len() {
                        seq.tracks.remove(*track_index);
                    }
                } else if let Some(track) = seq.tracks.get_mut(*track_index) {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyAppendClip { clip, track_index } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyRemoveClip { clip, track_index, position } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, clip.clone());
                }
            }
            EditCommand::LegacyRippleRemove { removed_clip, track_index, position, shifted_clips } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    // Restore shifted clips to their original positions
                    for (cid, old_in, old_out) in shifted_clips {
                        if let Some(c) = track.clips.iter_mut().find(|c| c.id == *cid) {
                            c.timeline_in = *old_in;
                            c.timeline_out = *old_out;
                        }
                    }
                    // Re-insert the removed clip
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, removed_clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyMoveClip { clip_id, old_timeline_in, old_timeline_out, .. } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.timeline_in = *old_timeline_in;
                        clip.timeline_out = *old_timeline_out;
                    }
                    track.sort_clips();
                }
            }
            EditCommand::LegacyTrimClip {
                clip_id,
                old_source_in, old_source_out,
                old_timeline_in, old_timeline_out, ..
            } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.source_in = *old_source_in;
                        clip.source_out = *old_source_out;
                        clip.timeline_in = *old_timeline_in;
                        clip.timeline_out = *old_timeline_out;
                    }
                }
            }
            EditCommand::LegacyRollTrimClip {
                clip_a_id, clip_b_id,
                old_a_source_out, old_a_timeline_out,
                old_b_source_in, old_b_timeline_in, ..
            } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_a_id) {
                        c.source_out = *old_a_source_out;
                        c.timeline_out = *old_a_timeline_out;
                    }
                    if let Some(c) = track.find_clip_mut(*clip_b_id) {
                        c.source_in = *old_b_source_in;
                        c.timeline_in = *old_b_timeline_in;
                    }
                }
            }
            EditCommand::LegacySlipClip {
                clip_id,
                old_source_in, old_source_out, ..
            } => {
                // Undo slip: restore original source in/out
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                // Also undo linked partner
                let linked_id = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).and_then(|c| c.linked_clip_id);
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_id) {
                        c.source_in = *old_source_in;
                        c.source_out = *old_source_out;
                    }
                }
                if let Some(lid) = linked_id {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            c.source_in = *old_source_in;
                            c.source_out = *old_source_out;
                        }
                    }
                }
            }
            EditCommand::LegacySlideClip {
                clip_id,
                old_timeline_in, old_timeline_out,
                left_clip_id, old_left_out,
                right_clip_id, old_right_in, ..
            } => {
                // Undo slide: restore original timeline positions for clip and neighbors
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                // Also undo linked partner
                let linked_id = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).and_then(|c| c.linked_clip_id);
                let delta = *old_timeline_in - seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).map(|c| c.timeline_in).unwrap_or(*old_timeline_in);
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_id) {
                        c.timeline_in = *old_timeline_in;
                        c.timeline_out = *old_timeline_out;
                    }
                    if let Some(lid) = left_clip_id {
                        if let (Some(c), Some(old_out)) = (track.find_clip_mut(*lid), old_left_out) {
                            // Restore left neighbor's out point (and source_out by same delta)
                            let current_out = c.timeline_out;
                            let dt = *old_out - current_out;
                            c.timeline_out = *old_out;
                            c.source_out = c.source_out + dt;
                        }
                    }
                    if let Some(rid) = right_clip_id {
                        if let (Some(c), Some(old_in)) = (track.find_clip_mut(*rid), old_right_in) {
                            let current_in = c.timeline_in;
                            let dt = *old_in - current_in;
                            c.timeline_in = *old_in;
                            c.source_in = c.source_in + dt;
                        }
                    }
                    track.sort_clips();
                }
                // Undo linked partner
                if let Some(lid) = linked_id {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            let dur = c.duration();
                            c.timeline_in = c.timeline_in + delta;
                            c.timeline_out = c.timeline_in + dur;
                        }
                    }
                }
            }
            EditCommand::LegacyBladeClip { track_index, original_clip, clip_a, clip_b, position } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    // Undo: remove clip_a and clip_b, insert original
                    track.clips.retain(|c| c.id != clip_a.id && c.id != clip_b.id);
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, original_clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyCutClip { clip, track_index, position, shifted_clips, .. } => {
                // Undo cut = re-insert clip + restore shifted positions
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    for (cid, old_in, old_out) in shifted_clips {
                        if let Some(c) = track.clips.iter_mut().find(|c| c.id == *cid) {
                            c.timeline_in = *old_in;
                            c.timeline_out = *old_out;
                        }
                    }
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyPasteClip { clip, track_index } => {
                // Undo paste = remove the pasted clip
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyMoveClipToTrack { clip, old_timeline_in, old_timeline_out, from_track, to_track, .. } => {
                // Undo: move clip back from to_track to from_track with original position
                if let Some(dst) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*to_track) {
                    dst.remove_clip(clip.id);
                }
                if let Some(src) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*from_track) {
                    let mut restored = clip.clone();
                    restored.timeline_in = *old_timeline_in;
                    restored.timeline_out = *old_timeline_out;
                    src.clips.push(restored);
                    src.sort_clips();
                }
            }
            EditCommand::Magnetic(cmd) => {
                klipp_timeline::magnetic::MagneticEditor::apply_inverse(
                    &mut self.state.sequences[self.state.active_sequence_idx], cmd);
            }
            EditCommand::AddMediaToPool { item } => {
                // Undo import: remove the item from pool
                self.state.media_pool.remove(&item.id);
                self.sync_media_pool_to_engine();
            }
            EditCommand::RemoveMediaFromPool { item, .. } => {
                // Undo delete: re-insert the full MediaItem snapshot
                self.state.media_pool.add(item.clone());
                self.sync_media_pool_to_engine();
            }
            EditCommand::MoveMediaToBin { media_id, from_bin, .. } => {
                // Undo: restore to original bin
                if let Some(item) = self.state.media_pool.get_mut(media_id) {
                    item.bin = from_bin.clone();
                }
            }
            EditCommand::SetMediaTags { media_id, old_tags, .. } => {
                if let Some(item) = self.state.media_pool.get_mut(media_id) {
                    item.tags = old_tags.clone();
                }
            }
            EditCommand::SetMediaRating { media_id, old_rating, .. } => {
                if let Some(item) = self.state.media_pool.get_mut(media_id) {
                    item.rating = *old_rating;
                }
            }
            EditCommand::SetMediaColorLabel { media_id, old_label, .. } => {
                if let Some(item) = self.state.media_pool.get_mut(media_id) {
                    item.color_label = klipp_media::ColorLabel::from_u8(*old_label);
                }
            }
            EditCommand::RenameBin { old_name, new_name } => {
                // Undo: rename back from new_name to old_name
                let ids: Vec<MediaId> = self.state.media_pool.all()
                    .filter(|m| m.bin.as_deref() == Some(new_name.as_str()))
                    .map(|m| m.id)
                    .collect();
                for id in ids {
                    if let Some(item) = self.state.media_pool.get_mut(&id) {
                        item.bin = Some(old_name.clone());
                    }
                }
            }
            EditCommand::SetRange { old_in, old_out, .. } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                seq.in_point = *old_in;
                seq.out_point = *old_out;
            }
            EditCommand::SetNodeGraph { clip_id, old_graph, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.node_graph = old_graph.clone();
                        break;
                    }
                }
            }
            EditCommand::EditKeyframes { clip_id, linked_clip_id, old_state, old_linked_state, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        old_state.apply_to_clip(clip);
                        break;
                    }
                }
                if let (Some(partner_id), Some(partner_state)) = (linked_clip_id, old_linked_state) {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*partner_id) {
                            partner_state.apply_to_clip(clip);
                            break;
                        }
                    }
                }
            }
            EditCommand::EditBuiltinEffect { clip_id, effect_index, old_effect, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(fx) = clip.builtin_effects.get_mut(*effect_index) {
                            *fx = old_effect.clone();
                        }
                        break;
                    }
                }
            }
            EditCommand::EditClipAudio { clip_id, old_gain, old_volume, old_pan, old_fade_in, old_fade_out, old_channel_config, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.gain = *old_gain;
                        clip.volume = *old_volume;
                        clip.pan = *old_pan;
                        clip.fade_in_secs = *old_fade_in;
                        clip.fade_out_secs = *old_fade_out;
                        clip.channel_config = *old_channel_config;
                        break;
                    }
                }
            }
            EditCommand::SetClipSpeed { clip_id, old_speed, old_timeline_out, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.speed = *old_speed;
                        clip.timeline_out = *old_timeline_out;
                        break;
                    }
                }
            }
        }
    }

    fn apply_edit_forward(&mut self, cmd: &EditCommand) {
        match cmd {
            EditCommand::AddTitleClip { clip, track_index, created_track } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                if *created_track {
                    // Re-create the track
                    let track_num = seq.tracks.iter().filter(|t| t.kind == klipp_timeline::TrackKind::Text).count() + 1;
                    let mut track = klipp_timeline::Track::new(&format!("Text {track_num}"), klipp_timeline::TrackKind::Text);
                    track.clips.push(clip.clone());
                    let idx = (*track_index).min(seq.tracks.len());
                    seq.tracks.insert(idx, track);
                } else if let Some(track) = seq.tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyAppendClip { clip, track_index } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyRemoveClip { clip, .. } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == clip.id))
                {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyRippleRemove { removed_clip, .. } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == removed_clip.id))
                {
                    track.ripple_remove(removed_clip.id);
                }
            }
            EditCommand::LegacyMoveClip { clip_id, new_timeline_in, new_timeline_out, .. } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.timeline_in = *new_timeline_in;
                        clip.timeline_out = *new_timeline_out;
                    }
                    track.sort_clips();
                }
            }
            EditCommand::LegacyTrimClip {
                clip_id,
                new_source_in, new_source_out,
                new_timeline_in, new_timeline_out, ..
            } => {
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.source_in = *new_source_in;
                        clip.source_out = *new_source_out;
                        clip.timeline_in = *new_timeline_in;
                        clip.timeline_out = *new_timeline_out;
                    }
                }
            }
            EditCommand::LegacyRollTrimClip {
                clip_a_id, clip_b_id,
                new_a_source_out, new_a_timeline_out,
                new_b_source_in, new_b_timeline_in, ..
            } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_a_id) {
                        c.source_out = *new_a_source_out;
                        c.timeline_out = *new_a_timeline_out;
                    }
                    if let Some(c) = track.find_clip_mut(*clip_b_id) {
                        c.source_in = *new_b_source_in;
                        c.timeline_in = *new_b_timeline_in;
                    }
                }
            }
            EditCommand::LegacySlipClip {
                clip_id,
                new_source_in, new_source_out, ..
            } => {
                // Redo slip: apply new source in/out
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let linked_id = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).and_then(|c| c.linked_clip_id);
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_id) {
                        c.source_in = *new_source_in;
                        c.source_out = *new_source_out;
                    }
                }
                if let Some(lid) = linked_id {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            c.source_in = *new_source_in;
                            c.source_out = *new_source_out;
                        }
                    }
                }
            }
            EditCommand::LegacySlideClip {
                clip_id,
                new_timeline_in, new_timeline_out,
                left_clip_id, new_left_out,
                right_clip_id, new_right_in, ..
            } => {
                // Redo slide: apply new timeline positions for clip and neighbors
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                let linked_id = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).and_then(|c| c.linked_clip_id);
                let old_in = seq.tracks.iter().flat_map(|t| t.clips.iter())
                    .find(|c| c.id == *clip_id).map(|c| c.timeline_in).unwrap_or(*new_timeline_in);
                let delta = *new_timeline_in - old_in;
                for track in &mut seq.tracks {
                    if let Some(c) = track.find_clip_mut(*clip_id) {
                        c.timeline_in = *new_timeline_in;
                        c.timeline_out = *new_timeline_out;
                    }
                    if let Some(lid) = left_clip_id {
                        if let (Some(c), Some(new_out)) = (track.find_clip_mut(*lid), new_left_out) {
                            let current_out = c.timeline_out;
                            let dt = *new_out - current_out;
                            c.timeline_out = *new_out;
                            c.source_out = c.source_out + dt;
                        }
                    }
                    if let Some(rid) = right_clip_id {
                        if let (Some(c), Some(new_in)) = (track.find_clip_mut(*rid), new_right_in) {
                            let current_in = c.timeline_in;
                            let dt = *new_in - current_in;
                            c.timeline_in = *new_in;
                            c.source_in = c.source_in + dt;
                        }
                    }
                    track.sort_clips();
                }
                if let Some(lid) = linked_id {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(c) = track.find_clip_mut(lid) {
                            let dur = c.duration();
                            c.timeline_in = c.timeline_in + delta;
                            c.timeline_out = c.timeline_in + dur;
                        }
                    }
                }
            }
            EditCommand::LegacyBladeClip { track_index, original_clip, clip_a, clip_b, .. } => {
                // Redo blade: remove original, insert clip_a and clip_b
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    track.remove_clip(original_clip.id);
                    track.clips.push(clip_a.clone());
                    track.clips.push(clip_b.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyCutClip { clip, .. } => {
                // Redo cut = ripple remove
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == clip.id))
                {
                    track.ripple_remove(clip.id);
                }
            }
            EditCommand::LegacyPasteClip { clip, track_index } => {
                // Redo paste = re-insert
                if let Some(track) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyMoveClipToTrack { clip, from_track, to_track, new_timeline_in, .. } => {
                // Redo: move from from_track to to_track at new position
                if let Some(src) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*from_track) {
                    src.remove_clip(clip.id);
                }
                if let Some(dst) = self.state.sequences[self.state.active_sequence_idx].tracks.get_mut(*to_track) {
                    let mut moved = clip.clone();
                    moved.timeline_in = *new_timeline_in;
                    let dur = clip.timeline_out - clip.timeline_in;
                    moved.timeline_out = *new_timeline_in + dur;
                    dst.clips.push(moved);
                    dst.sort_clips();
                }
            }
            EditCommand::Magnetic(cmd) => {
                klipp_timeline::magnetic::MagneticEditor::apply_forward(
                    &mut self.state.sequences[self.state.active_sequence_idx], cmd);
            }
            EditCommand::AddMediaToPool { item } => {
                // Redo import: re-add the item
                self.state.media_pool.add(item.clone());
                self.sync_media_pool_to_engine();
            }
            EditCommand::RemoveMediaFromPool { item, .. } => {
                // Redo delete: remove again
                self.state.media_pool.remove(&item.id);
                self.sync_media_pool_to_engine();
            }
            EditCommand::MoveMediaToBin { media_id, to_bin, .. } => {
                if let Some(item) = self.state.media_pool.get_mut(media_id) {
                    item.bin = to_bin.clone();
                }
            }
            EditCommand::SetMediaTags { media_id, new_tags, .. } => {
                if let Some(item) = self.state.media_pool.get_mut(media_id) {
                    item.tags = new_tags.clone();
                }
            }
            EditCommand::SetMediaRating { media_id, new_rating, .. } => {
                if let Some(item) = self.state.media_pool.get_mut(media_id) {
                    item.rating = *new_rating;
                }
            }
            EditCommand::SetMediaColorLabel { media_id, new_label, .. } => {
                if let Some(item) = self.state.media_pool.get_mut(media_id) {
                    item.color_label = klipp_media::ColorLabel::from_u8(*new_label);
                }
            }
            EditCommand::RenameBin { old_name, new_name } => {
                // Redo: rename from old_name to new_name
                let ids: Vec<MediaId> = self.state.media_pool.all()
                    .filter(|m| m.bin.as_deref() == Some(old_name.as_str()))
                    .map(|m| m.id)
                    .collect();
                for id in ids {
                    if let Some(item) = self.state.media_pool.get_mut(&id) {
                        item.bin = Some(new_name.clone());
                    }
                }
            }
            EditCommand::SetRange { new_in, new_out, .. } => {
                let seq = &mut self.state.sequences[self.state.active_sequence_idx];
                seq.in_point = *new_in;
                seq.out_point = *new_out;
            }
            EditCommand::SetNodeGraph { clip_id, new_graph, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.node_graph = new_graph.clone();
                        break;
                    }
                }
            }
            EditCommand::EditKeyframes { clip_id, linked_clip_id, new_state, new_linked_state, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        new_state.apply_to_clip(clip);
                        break;
                    }
                }
                if let (Some(partner_id), Some(partner_state)) = (linked_clip_id, new_linked_state) {
                    for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                        if let Some(clip) = track.find_clip_mut(*partner_id) {
                            partner_state.apply_to_clip(clip);
                            break;
                        }
                    }
                }
            }
            EditCommand::EditBuiltinEffect { clip_id, effect_index, new_effect, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        if let Some(fx) = clip.builtin_effects.get_mut(*effect_index) {
                            *fx = new_effect.clone();
                        }
                        break;
                    }
                }
            }
            EditCommand::EditClipAudio { clip_id, new_gain, new_volume, new_pan, new_fade_in, new_fade_out, new_channel_config, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.gain = *new_gain;
                        clip.volume = *new_volume;
                        clip.pan = *new_pan;
                        clip.fade_in_secs = *new_fade_in;
                        clip.fade_out_secs = *new_fade_out;
                        clip.channel_config = *new_channel_config;
                        break;
                    }
                }
            }
            EditCommand::SetClipSpeed { clip_id, new_speed, new_timeline_out, .. } => {
                for track in &mut self.state.sequences[self.state.active_sequence_idx].tracks {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.speed = *new_speed;
                        clip.timeline_out = *new_timeline_out;
                        break;
                    }
                }
            }
        }
    }

    /// Overwrite-insert a clip onto a track at a given position.
    /// Trims or removes any overlapping clips in the range [timeline_in, timeline_out].
    fn overwrite_insert_clip_on_track(
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
                linked_clip_id: clip.linked_clip_id,
                group_id: clip.group_id,
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
                clip_markers: clip.clip_markers.clone(),
                effect_keyframes: clip.effect_keyframes.clone(),
                role: clip.role,
                param_modifiers: clip.param_modifiers.clone(),
                speed: clip.speed,
                audio_effects: clip.audio_effects.clone(),
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
                linked_clip_id: clip.linked_clip_id,
                group_id: clip.group_id,
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
                clip_markers: clip.clip_markers.clone(),
                effect_keyframes: clip.effect_keyframes.clone(),
                role: clip.role,
                param_modifiers: clip.param_modifiers.clone(),
                speed: clip.speed,
                audio_effects: clip.audio_effects.clone(),
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

    fn do_save_project(&mut self, path: &std::path::Path) {
        let color = klipp_project::ColorSettings {
            global_lut_path: self.state.global_lut_path.clone(),
            global_lut_opacity: self.state.global_lut_opacity,
            track_header_width: self.state.track_header_width,
            auto_generate_filmstrips: self.state.project_auto_filmstrips,
            auto_generate_skim: self.state.project_auto_skim,
        };
        let active_seq_id = self.state.sequences[self.state.active_sequence_idx].id.to_string();
        match klipp_project::save_project_with_color(path, &self.state.sequences, &self.state.media_pool, &color, Some(&active_seq_id), self.state.created_at.as_deref()) {
            Ok(()) => {
                self.state.project_path = Some(path.to_path_buf());
                self.state.dirty = false;
                tracing::info!("Project saved to {:?}", path);
                let rp = crate::prefs::RecentProject {
                    path: path.to_path_buf(),
                    name: self.state.sequences[self.state.active_sequence_idx].name.clone(),
                    last_saved: Some(crate::app::iso8601_now()),
                    duration_secs: Some(self.state.sequence_duration().to_secs_f64()),
                    fps: {
                        let f = self.state.sequences[self.state.active_sequence_idx].fps;
                        if f.den == 1 {
                            Some(format!("{}fps", f.num))
                        } else {
                            Some(format!("{:.3}fps", f.num as f64 / f.den as f64))
                        }
                    },
                    resolution: Some(format!(
                        "{}×{}",
                        self.state.sequences[self.state.active_sequence_idx].resolution.width,
                        self.state.sequences[self.state.active_sequence_idx].resolution.height
                    )),
                    thumbnail_path: generate_poster_frame(&self.frame_buffer, path),
                    total_size_bytes: Some(self.state.media_pool.all().map(|m| m.file_size_bytes).sum()),
                };
                self.prefs.add_recent_project(rp);
                self.prefs_dirty = true;
                // Delete autosave file on clean save
                let autosave = autosave_path_for(path);
                if autosave.exists() {
                    let _ = std::fs::remove_dir_all(&autosave);
                    tracing::info!("Removed autosave {:?}", autosave);
                }
            }
            Err(e) => {
                tracing::error!("Failed to save project: {e}");
            }
        }
    }

    fn do_open_project(&mut self, path: &std::path::Path) {
        // Check for newer autosave
        let autosave = autosave_path_for(path);
        if autosave.exists() {
            // Compare modification times
            let orig_modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
            let auto_modified = std::fs::metadata(&autosave).and_then(|m| m.modified()).ok();
            if let (Some(orig_t), Some(auto_t)) = (orig_modified, auto_modified) {
                if auto_t > orig_t {
                    self.state.show_autosave_recovery = true;
                    self.state.autosave_recovery_path = Some(autosave);
                    self.state.autosave_original_path = Some(path.to_path_buf());
                    return;
                }
            }
        }
        self.do_open_project_inner(path);
    }

    fn do_open_project_inner(&mut self, path: &std::path::Path) {
        match klipp_project::load_project(path) {
            Ok((sequences, media_pool, color_settings, active_id, created_at)) => {
                self.state.fps = sequences.first().map(|s| s.fps).unwrap_or(klipp_core::Fps { num: 24, den: 1 });
                let active_idx = active_id.as_deref()
                    .and_then(|id| sequences.iter().position(|s| s.id.to_string() == id))
                    .unwrap_or(0);
                self.state.sequences = sequences;
                self.state.active_sequence_idx = active_idx;
                self.state.created_at = created_at;
                self.state.media_pool = media_pool;

                // Restore global LUT
                self.state.global_lut_opacity = color_settings.global_lut_opacity;
                if let Some(ref lut_path) = color_settings.global_lut_path {
                    if lut_path.exists() {
                        self.state.global_lut_path = Some(lut_path.clone());
                        self.cmd_tx.send(AppCommand::LoadGlobalLut { path: lut_path.clone() }).ok();
                        self.cmd_tx.send(AppCommand::SetGlobalLutOpacity(color_settings.global_lut_opacity)).ok();
                    } else {
                        tracing::warn!("Global LUT not found, skipping: {:?}", lut_path);
                        self.state.global_lut_path = None;
                    }
                } else {
                    self.state.global_lut_path = None;
                }
                // Restore track header width
                self.state.track_header_width = color_settings.track_header_width;

                // Restore per-project media generation overrides
                self.state.project_auto_filmstrips = color_settings.auto_generate_filmstrips;
                self.state.project_auto_skim = color_settings.auto_generate_skim;

                self.state.project_path = Some(path.to_path_buf());
                self.state.dirty = false;
                self.state.selected_clip = None;
                let rp = crate::prefs::RecentProject {
                    path: path.to_path_buf(),
                    name: self.state.sequences[self.state.active_sequence_idx].name.clone(),
                    last_saved: None,
                    duration_secs: Some(self.state.sequence_duration().to_secs_f64()),
                    fps: {
                        let f = self.state.sequences[self.state.active_sequence_idx].fps;
                        if f.den == 1 {
                            Some(format!("{}fps", f.num))
                        } else {
                            Some(format!("{:.3}fps", f.num as f64 / f.den as f64))
                        }
                    },
                    resolution: Some(format!(
                        "{}×{}",
                        self.state.sequences[self.state.active_sequence_idx].resolution.width,
                        self.state.sequences[self.state.active_sequence_idx].resolution.height
                    )),
                    thumbnail_path: None,
                    total_size_bytes: Some(self.state.media_pool.all().map(|m| m.file_size_bytes).sum()),
                };
                self.prefs.add_recent_project(rp);
                self.prefs_dirty = true;
                self.state.playback = PlaybackState::Idle;
                self.state.playhead = Timecode::zero();
                self.state.thumbnail_cache = klipp_media::ThumbnailCache::new();
                self.state.thumbnail_textures.clear();
                self.state.filmstrip_textures.clear();
                self.state.filmstrip_in_progress.clear();
                self.state.filmstrip_queue.clear();
                // Stop the engine so decoders are released and frame buffer is stale-free
                self.cmd_tx.send(AppCommand::Stop).ok();
                // Clear the shared frame buffer so the viewer doesn't show the old frame
                if let Ok(mut fb) = self.frame_buffer.lock() {
                    fb.clear();
                }
                self.sync_sequence_to_engine();

                // Detect offline media
                self.state.offline_media_ids = self.state.media_pool.all()
                    .filter(|item| item.offline)
                    .map(|item| item.id)
                    .collect();
                if !self.state.offline_media_ids.is_empty() {
                    self.state.show_relink_dialog = true;
                }

                // Reload media files into the engine and trigger thumbnails/filmstrips/audio/skim
                let media_items: Vec<_> = self
                    .state
                    .media_pool
                    .all()
                    .filter(|item| !item.offline)
                    .map(|item| (item.id, item.path.clone(), item.duration, item.has_audio))
                    .collect();
                for (id, path, duration, has_audio) in media_items {
                    // Re-open decoders in the engine
                    self.cmd_tx.send(AppCommand::LoadMediaFile {
                        path: path.clone(),
                        media_id: id,
                    }).ok();
                    self.spawn_thumbnail(id, path.clone(), duration);
                    self.spawn_filmstrip(id, path.clone(), duration);
                    if has_audio {
                        self.spawn_audio_conform(id, path.clone());
                    }
                    self.state.pending_skim_transcodes.push(id);
                }
                self.state.filmstrip_generating = true;

                tracing::info!("Project loaded from {:?}", path);

                // Start file watcher for all media paths
                let media_paths: Vec<std::path::PathBuf> = self.state.media_pool.all()
                    .filter(|m| !m.offline)
                    .map(|m| m.path.clone())
                    .collect();
                self.media_watcher = crate::media_watcher::start_media_watcher(
                    &media_paths,
                    self.proxy_event_tx.clone(), // reuse proxy event channel for file watcher events
                );
            }
            Err(e) => {
                tracing::error!("Failed to load project: {e}");
            }
        }
    }

    fn show_unsaved_dialog(&mut self, ctx: &egui::Context) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        if !self.state.show_unsaved_dialog {
            return cmds;
        }

        let mut open = true;
        let project_name = self.state.project_path.as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled".to_string());

        egui::Window::new("Save Changes?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!(
                    "Do you want to save changes to \"{}\" before closing?",
                    project_name
                ));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Your changes will be lost if you don't save them.")
                        .size(11.0)
                        .color(egui::Color32::from_rgb(160, 160, 165)),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Don't Save").clicked() {
                        self.state.dirty = false;
                        self.state.show_unsaved_dialog = false;
                        // Clean up autosave backup since user chose not to save
                        if let Some(ref path) = self.state.project_path {
                            let autosave = autosave_path_for(path);
                            if autosave.exists() {
                                let _ = std::fs::remove_dir_all(&autosave);
                            }
                        }
                        let action = self.state.unsaved_dialog_action.clone();
                        self.state.unsaved_dialog_action = UnsavedAction::None;
                        match action {
                            UnsavedAction::NewProject => self.do_new_project(),
                            UnsavedAction::OpenNewProjectDialog => {
                                self.new_project_dialog.reset(
                                    self.prefs.general.default_project_dir.as_ref(),
                                );
                                self.state.show_new_project_dialog = true;
                            }
                            UnsavedAction::OpenProject(p) => self.do_open_project(&p),
                            UnsavedAction::Quit => cmds.push(AppCommand::Quit),
                            UnsavedAction::None => {}
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        self.state.show_unsaved_dialog = false;
                        self.state.unsaved_dialog_action = UnsavedAction::None;
                    }
                    if ui.button("Save").clicked() {
                        self.state.show_unsaved_dialog = false;
                        cmds.push(AppCommand::SaveProject);
                        let action = self.state.unsaved_dialog_action.clone();
                        self.state.unsaved_dialog_action = UnsavedAction::None;
                        match action {
                            UnsavedAction::NewProject => self.do_new_project(),
                            UnsavedAction::OpenNewProjectDialog => {
                                self.new_project_dialog.reset(
                                    self.prefs.general.default_project_dir.as_ref(),
                                );
                                self.state.show_new_project_dialog = true;
                            }
                            UnsavedAction::OpenProject(p) => self.do_open_project(&p),
                            UnsavedAction::Quit => cmds.push(AppCommand::Quit),
                            UnsavedAction::None => {}
                        }
                    }
                });
            });

        if !open {
            self.state.show_unsaved_dialog = false;
            self.state.unsaved_dialog_action = UnsavedAction::None;
        }

        cmds
    }

    fn update_window_title(&self, ctx: &egui::Context) {
        let title = match &self.state.project_path {
            None => "Klipp".to_string(),
            Some(p) => {
                let name = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("Untitled");
                if self.state.dirty {
                    format!("Klipp \u{2014} {} *", name)
                } else {
                    format!("Klipp \u{2014} {}", name)
                }
            }
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Skip single-key shortcuts when ANY text edit widget has focus (search bars,
        // title text editor, timecode entry, rename fields, etc.).
        // Ctrl+ shortcuts still work (standard convention: Ctrl combos always fire).
        let text_editing = ctx.wants_keyboard_input() || self.state.editing_timecode;

        ctx.input(|i| {
            // Ctrl+S — Save
            if i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::S) {
                cmds.push(AppCommand::SaveProject);
            }
            // Ctrl+Shift+S — Save As
            if i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::S) {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Save Project As")
                    .add_filter("Klipp Project", &["klipp"])
                    .set_file_name("project.klipp")
                    .save_file()
                {
                    cmds.push(AppCommand::SaveProjectAs { path });
                }
            }
            // Ctrl+O — Open Project
            if i.modifiers.ctrl && i.key_pressed(egui::Key::O) {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title("Open Project")
                    .pick_folder()
                {
                    cmds.push(AppCommand::OpenProject { path });
                }
            }
            // Ctrl+I — Import Media
            if i.modifiers.ctrl && i.key_pressed(egui::Key::I) {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter(
                        "Video",
                        &["mp4", "mkv", "mov", "avi", "webm", "m4v", "mts"],
                    )
                    .pick_file()
                {
                    cmds.push(AppCommand::ImportMedia { path });
                }
            }
            // Ctrl+E — Export
            if i.modifiers.ctrl && i.key_pressed(egui::Key::E) {
                self.state.show_export_dialog = true;
                if self.export_dialog.output_path.is_empty() {
                    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                    self.export_dialog.output_path = format!("{home}/output.{}", self.export_dialog.format.extension());
                }
            }
            // Ctrl+, — Preferences
            if i.modifiers.ctrl && i.key_pressed(egui::Key::Comma) {
                self.state.show_preferences = true;
            }
            // Ctrl+Z — Undo, Ctrl+Shift+Z or Ctrl+Y — Redo
            if i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::Z) {
                cmds.push(AppCommand::Undo);
            }
            if i.modifiers.ctrl && (i.key_pressed(egui::Key::Y)
                || (i.modifiers.shift && i.key_pressed(egui::Key::Z)))
            {
                cmds.push(AppCommand::Redo);
            }

            // Ctrl+G — Group selected clips
            if i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::G) {
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
            // Ctrl+Shift+G — Ungroup
            if i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::G) {
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

            // Skip single-key shortcuts while typing in a text field
            if text_editing {
                return;
            }

            // S (no modifiers) — Toggle skimming
            if !i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::S) {
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
            if !i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::G) {
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
                                        self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: crate::history::EditCommand::Magnetic(result.command) });
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
            if !i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::G) {
                if let Some(clip_id) = self.state.selected_clip {
                    let has_anchor = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .any(|c| c.id == clip_id && c.magnetic_anchor.is_some());
                    if has_anchor {
                        let result = klipp_timeline::magnetic::MagneticEditor::disconnect_clip(
                            &mut self.state.sequences[self.state.active_sequence_idx], clip_id);
                        self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: crate::history::EditCommand::Magnetic(result.command) });
                        self.sync_sequence_to_engine();
                        self.state.dirty = true;
                    }
                }
            }

            // Shift+S (no ctrl) — Promote/Break storyline OR toggle audio skimming
            if !i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::S) {
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
                                        self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: crate::history::EditCommand::Magnetic(result.command) });
                                        self.sync_sequence_to_engine();
                                        self.state.dirty = true;
                                        handled_storyline = true;
                                    }
                                }
                            }
                        } else if self.state.sequences[self.state.active_sequence_idx].tracks[ti].magnetic_role == klipp_timeline::TrackRole::ConnectedStoryline {
                            let result = klipp_timeline::magnetic::MagneticEditor::break_apart_storyline(
                                &mut self.state.sequences[self.state.active_sequence_idx], ti);
                            self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: crate::history::EditCommand::Magnetic(result.command) });
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                            handled_storyline = true;
                        }
                    }
                }
                // If no storyline action applied, toggle audio skimming
                if !handled_storyline {
                    self.state.audio_skimming_enabled = !self.state.audio_skimming_enabled;
                    self.prefs.playback.skim_audio = self.state.audio_skimming_enabled;
                    self.prefs_dirty = true;
                    // End active session when disabling
                    if !self.state.audio_skimming_enabled {
                        if let Some(ref session) = self.state.skim_audio_session {
                            session.stop();
                        }
                        klipp_engine::audio_output::flush_skim_consumer(&self.skim_consumer);
                        klipp_engine::audio_output::set_skim_consumer(&self.skim_consumer, None);
                        self.state.skim_audio_session = None;
                        self.state.skim_speed_px_per_sec = 0.0;
                    }
                    let msg = if self.state.audio_skimming_enabled {
                        "Audio Skimming On"
                    } else {
                        "Audio Skimming Off (video only)"
                    };
                    self.state.toast.show(msg);
                }
            }

            // Delete/Backspace — Remove selected keyframe (if one is selected on timeline)
            let mut keyframe_deleted = false;
            if !i.modifiers.shift && (i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
                if let Some(sk) = self.state.selected_keyframe.take() {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let clip = seq.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == sk.clip_id);
                    if let Some(clip) = clip {
                        let kf_time = match &sk.param_kind {
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
                                };
                                kfs.get(sk.keyframe_index).map(|kf| kf.time_secs)
                            }
                        };
                        if let Some(time_secs) = kf_time {
                            match sk.param_kind {
                                crate::state::KeyframeParamKind::Audio(param) => {
                                    cmds.push(AppCommand::RemoveAudioKeyframe { clip_id: sk.clip_id, param, time_secs });
                                }
                                crate::state::KeyframeParamKind::Video(param) => {
                                    cmds.push(AppCommand::RemoveVideoKeyframe { clip_id: sk.clip_id, param, time_secs });
                                }
                            }
                            keyframe_deleted = true;
                        }
                    }
                }
            }

            // Shift+Delete — Lift (leave gap, no ripple) on magnetic tracks
            if !keyframe_deleted && i.modifiers.shift && (i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
                if let Some(clip_id) = self.state.selected_clip {
                    let track_idx = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == clip_id));
                    if let Some(ti) = track_idx {
                        if self.is_magnetic_track(ti) {
                            let result = klipp_timeline::magnetic::MagneticEditor::lift(
                                &mut self.state.sequences[self.state.active_sequence_idx], clip_id);
                            self.state.history.push(HistoryEntry { sequence_idx: self.state.active_sequence_idx, command: crate::history::EditCommand::Magnetic(result.command) });
                            self.state.selected_clip = None;
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        }
                    }
                }
            }
            // Delete/Backspace — remove selected clip OR extract range (plain, no shift)
            else if !keyframe_deleted && !i.modifiers.shift && (i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RemoveClip { clip_id });
                } else if self.state.selected_clips.is_empty()
                    && self.state.sequence().in_point.is_some()
                    && self.state.sequence().out_point.is_some()
                {
                    cmds.push(AppCommand::ExtractEdit);
                }
            }

            // W — Toggle Insert/Overwrite mode
            if i.key_pressed(egui::Key::W) {
                self.state.edit_mode = match self.state.edit_mode {
                    klipp_timeline::magnetic::EditMode::Insert => klipp_timeline::magnetic::EditMode::Overwrite,
                    klipp_timeline::magnetic::EditMode::Overwrite => klipp_timeline::magnetic::EditMode::Insert,
                };
            }
            // Alt+[ — Ripple trim head to playhead
            if i.modifiers.alt && i.key_pressed(egui::Key::OpenBracket) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RippleTrimHead { clip_id });
                }
            }
            // Alt+] — Ripple trim tail to playhead
            if i.modifiers.alt && i.key_pressed(egui::Key::CloseBracket) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RippleTrimTail { clip_id });
                }
            }
            // Ctrl+X — Cut
            if i.modifiers.ctrl && i.key_pressed(egui::Key::X) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::CutClip { clip_id });
                }
            }
            // B key (bare) — toggle before/after
            if !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.shift
                && i.key_pressed(egui::Key::B)
            {
                cmds.push(AppCommand::ToggleBeforeAfter);
            }
            // C key (bare) — toggle color panel
            if !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.shift
                && i.key_pressed(egui::Key::C)
            {
                cmds.push(AppCommand::ToggleColorPanel);
            }
            // Z key (bare) — toggle zebra overlay
            if !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.shift
                && i.key_pressed(egui::Key::Z)
            {
                use crate::state::ViewerOverlay;
                self.state.viewer_overlay = match self.state.viewer_overlay {
                    ViewerOverlay::Zebra => ViewerOverlay::None,
                    _ => ViewerOverlay::Zebra,
                };
            }
            // F key (bare) — toggle false color overlay
            if !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.shift
                && i.key_pressed(egui::Key::F)
            {
                use crate::state::ViewerOverlay;
                self.state.viewer_overlay = match self.state.viewer_overlay {
                    ViewerOverlay::FalseColor => ViewerOverlay::None,
                    _ => ViewerOverlay::FalseColor,
                };
            }
            // V key (bare) — toggle scopes
            if !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.shift
                && i.key_pressed(egui::Key::V)
            {
                self.state.show_scopes = !self.state.show_scopes;
            }
            // Ctrl+C — Copy
            if i.modifiers.ctrl && i.key_pressed(egui::Key::C) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::CopyClip { clip_id });
                }
            }
            // Ctrl+V — Paste
            if i.modifiers.ctrl && i.key_pressed(egui::Key::V) {
                cmds.push(AppCommand::PasteClip);
            }
            // Ctrl+B — Split at playhead (all tracks with clips at playhead)
            if i.modifiers.ctrl && i.key_pressed(egui::Key::B) {
                let ph = self.state.playhead;
                for (track_index, track) in self.state.sequences[self.state.active_sequence_idx].tracks.iter().enumerate() {
                    if let Some((_, clip)) = track.clip_at_timecode(ph) {
                        cmds.push(AppCommand::BladeClip {
                            track_index,
                            clip_id: clip.id,
                            cut_at: ph,
                        });
                    }
                }
            }
            // Ctrl+L — Toggle loop playback
            if i.modifiers.ctrl && i.key_pressed(egui::Key::L) {
                cmds.push(AppCommand::ToggleLoopPlayback);
            }
            // Ctrl+T — Create title clip at playhead on a free text track (or create new one)
            if i.modifiers.ctrl && i.key_pressed(egui::Key::T) {
                let ph = self.state.playhead;
                // Find a text track that has no clip at the playhead
                let track_index = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                    .enumerate()
                    .find(|(_, t)| {
                        t.kind == klipp_timeline::TrackKind::Text
                            && t.clip_at_timecode(ph).is_none()
                    })
                    .map(|(i, _)| i);
                let track_index = match track_index {
                    Some(i) => i,
                    None => {
                        // No free text track — create one
                        let track_num = self.state.sequences[self.state.active_sequence_idx].tracks.iter()
                            .filter(|t| t.kind == klipp_timeline::TrackKind::Text)
                            .count() + 1;
                        let track = klipp_timeline::Track::new(
                            &format!("Text {track_num}"),
                            klipp_timeline::TrackKind::Text,
                        );
                        self.state.sequences[self.state.active_sequence_idx].tracks.insert(0, track); // insert at top
                        0
                    }
                };
                let fps_rate = self.state.sequences[self.state.active_sequence_idx].fps.rate();
                let duration_frames = fps_rate * 5; // 5 seconds default
                cmds.push(AppCommand::CreateTitleClip {
                    track_index,
                    timeline_in: ph,
                    duration_frames,
                });
            }
            // B — Blade tool
            if !i.modifiers.ctrl && i.key_pressed(egui::Key::B) {
                tracing::debug!("B key pressed -> Blade tool");
                cmds.push(AppCommand::SetActiveTool(Tool::Blade));
            }
            // V — Select tool
            if !i.modifiers.ctrl && i.key_pressed(egui::Key::V) {
                tracing::debug!("V key pressed -> Select tool");
                cmds.push(AppCommand::SetActiveTool(Tool::Select));
            }
            // R — Range tool toggle
            if !i.modifiers.ctrl && i.key_pressed(egui::Key::R) {
                if self.state.active_tool == Tool::Range {
                    cmds.push(AppCommand::SetActiveTool(Tool::Select));
                } else {
                    cmds.push(AppCommand::SetActiveTool(Tool::Range));
                }
            }
            // X — Set range to clip at playhead / Alt+X — Clear range
            if i.key_pressed(egui::Key::X) && !i.modifiers.ctrl {
                if i.modifiers.alt {
                    // Alt+X: Clear both in and out
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    if old_in.is_some() || old_out.is_some() {
                        cmds.push(AppCommand::ClearSequenceInPoint);
                        cmds.push(AppCommand::ClearSequenceOutPoint);
                        self.state.history.push(crate::history::HistoryEntry {
                            sequence_idx: self.state.active_sequence_idx,
                            command: crate::history::EditCommand::SetRange {
                                old_in, old_out, new_in: None, new_out: None,
                            },
                        });
                        self.state.toast.show("Range cleared");
                    }
                } else if !i.modifiers.shift {
                    // X: Set range to clip at playhead
                    let ph = self.state.playhead;
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let mut found = false;
                    for track in &seq.tracks {
                        if let Some((_, clip)) = track.clip_at_timecode(ph) {
                            let old_in = seq.in_point;
                            let old_out = seq.out_point;
                            cmds.push(AppCommand::SetSequenceInPoint(clip.timeline_in));
                            cmds.push(AppCommand::SetSequenceOutPoint(clip.timeline_out));
                            self.state.history.push(crate::history::HistoryEntry {
                                sequence_idx: self.state.active_sequence_idx,
                                command: crate::history::EditCommand::SetRange {
                                    old_in, old_out,
                                    new_in: Some(clip.timeline_in),
                                    new_out: Some(clip.timeline_out),
                                },
                            });
                            self.state.toast.show("Range: clip selected");
                            found = true;
                            break;
                        }
                    }
                    let _ = found;
                }
            }
            // F — Toggle fullscreen viewer
            if i.key_pressed(egui::Key::F) && !i.modifiers.ctrl {
                cmds.push(AppCommand::ToggleFullscreen);
            }
            // P — Toggle performance overlay
            if i.key_pressed(egui::Key::P) && !i.modifiers.ctrl {
                self.state.show_perf_overlay = !self.state.show_perf_overlay;
            }
            // N — Toggle snap
            if i.key_pressed(egui::Key::N) && !i.modifiers.ctrl {
                self.state.snap_enabled = !self.state.snap_enabled;
            }
            // I/O — In/Out markers (context-dependent on viewer mode)
            if i.key_pressed(egui::Key::I) && !i.modifiers.ctrl {
                if i.modifiers.shift {
                    tracing::debug!("Shift+I -> Clear sequence in point");
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    cmds.push(AppCommand::ClearSequenceInPoint);
                    self.state.history.push(crate::history::HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
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
                    self.state.history.push(crate::history::HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in, new_out: old_out,
                        },
                    });
                }
            }
            if i.key_pressed(egui::Key::O) && !i.modifiers.ctrl {
                if i.modifiers.shift {
                    tracing::debug!("Shift+O -> Clear sequence out point");
                    let old_in = self.state.sequence().in_point;
                    let old_out = self.state.sequence().out_point;
                    cmds.push(AppCommand::ClearSequenceOutPoint);
                    self.state.history.push(crate::history::HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
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
                    self.state.history.push(crate::history::HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: old_in, new_out,
                        },
                    });
                }
            }

            // Alt+X — clear both in/out points (standard NLE shortcut)
            if i.modifiers.alt && i.key_pressed(egui::Key::X) {
                let old_in = self.state.sequence().in_point;
                let old_out = self.state.sequence().out_point;
                if old_in.is_some() || old_out.is_some() {
                    cmds.push(AppCommand::ClearSequenceInPoint);
                    cmds.push(AppCommand::ClearSequenceOutPoint);
                    self.state.history.push(crate::history::HistoryEntry {
                        sequence_idx: self.state.active_sequence_idx,
                        command: crate::history::EditCommand::SetRange {
                            old_in, old_out, new_in: None, new_out: None,
                        },
                    });
                }
            }

            // Escape — clear active range drag OR return Range tool to Select
            if i.key_pressed(egui::Key::Escape) {
                if let Some(drag) = self.state.range_drag.take() {
                    self.state.sequences[self.state.active_sequence_idx].in_point = drag.original_in;
                    self.state.sequences[self.state.active_sequence_idx].out_point = drag.original_out;
                } else if self.state.active_tool == Tool::Range {
                    cmds.push(AppCommand::SetActiveTool(Tool::Select));
                }
                self.state.ruler_press_origin = None;
            }
            // , or F10 — Insert edit (ripple)
            if i.events.iter().any(|e| matches!(e, egui::Event::Text(s) if s == ","))
                || i.key_pressed(egui::Key::F10)
            {
                cmds.push(AppCommand::InsertEdit);
            }
            // . or F11 — Overwrite edit
            if i.events.iter().any(|e| matches!(e, egui::Event::Text(s) if s == "."))
                || i.key_pressed(egui::Key::F11)
            {
                cmds.push(AppCommand::OverwriteEdit);
            }
            // ; — Lift edit
            if i.key_pressed(egui::Key::Semicolon)
                || i.events.iter().any(|e| matches!(e, egui::Event::Text(s) if s == ";"))
            {
                cmds.push(AppCommand::LiftEdit);
            }
            // ' — Extract edit
            if i.events.iter().any(|e| matches!(e, egui::Event::Text(s) if s == "'")) {
                cmds.push(AppCommand::ExtractEdit);
            }
            // Alt+M — Toggle matte preview
            if i.key_pressed(egui::Key::M) && i.modifiers.alt && !i.modifiers.ctrl {
                self.state.matte_preview_active = !self.state.matte_preview_active;
                cmds.push(AppCommand::SetMattePreview(self.state.matte_preview_active));
            }
            // K — Toggle keyframe at playhead for the active parameter
            if i.key_pressed(egui::Key::K) && !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.shift {
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
                            let tolerance = 0.02;
                            if track_kind == klipp_timeline::TrackKind::Audio {
                                let param = self.state.rubber_band_param;
                                let keyframes = match param {
                                    klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                    klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                    klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                                };
                                let existing = keyframes.iter().find(|kf| (kf.time_secs - clip_time_secs).abs() < tolerance);
                                if let Some(kf) = existing {
                                    cmds.push(AppCommand::RemoveAudioKeyframe { clip_id, param, time_secs: kf.time_secs });
                                } else {
                                    let default_val = match param {
                                        klipp_core::AudioParam::Volume => clip.volume,
                                        klipp_core::AudioParam::Pan => clip.pan,
                                        klipp_core::AudioParam::Gain => clip.gain,
                                    };
                                    let value = klipp_core::keyframe::evaluate(keyframes, clip_time_secs, default_val);
                                    cmds.push(AppCommand::AddAudioKeyframe { clip_id, param, time_secs: clip_time_secs, value });
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
                                };
                                let existing = keyframes.iter().find(|kf| (kf.time_secs - clip_time_secs).abs() < tolerance);
                                if let Some(kf) = existing {
                                    cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param, time_secs: kf.time_secs });
                                } else {
                                    let default_val = match param {
                                        klipp_core::VideoParam::Opacity => opacity_static,
                                        klipp_core::VideoParam::PositionX => transform.0,
                                        klipp_core::VideoParam::PositionY => transform.1,
                                        klipp_core::VideoParam::Scale => transform.2,
                                        klipp_core::VideoParam::Rotation => transform.3,
                                    };
                                    let value = klipp_core::keyframe::evaluate(keyframes, clip_time_secs, default_val);
                                    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param, time_secs: clip_time_secs, value });
                                }
                            }
                        }
                    }
                }
            }
            // Shift+K — Toggle keyframe editor panel
            if i.key_pressed(egui::Key::K) && i.modifiers.shift && !i.modifiers.ctrl && !i.modifiers.alt {
                self.state.show_keyframe_editor = !self.state.show_keyframe_editor;
            }
            // Alt+Left — Navigate to previous keyframe
            if i.modifiers.alt && !i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowLeft) {
                if let Some(clip_id) = self.state.selected_clip {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let clip_and_kind = seq.tracks.iter().find_map(|t| {
                        t.clips.iter().find(|c| c.id == clip_id).map(|c| (c, t.kind))
                    });
                    if let Some((clip, track_kind)) = clip_and_kind {
                        let clip_time_secs = self.state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64();
                        let keyframes = if track_kind == klipp_timeline::TrackKind::Audio {
                            match self.state.rubber_band_param {
                                klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                            }
                        } else {
                            match self.state.video_rubber_band_param {
                                klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                            }
                        };
                        // Find previous keyframe (strictly before current time with tolerance)
                        let prev = keyframes.iter().rev()
                            .find(|kf| kf.time_secs < clip_time_secs - 0.001);
                        if let Some(kf) = prev {
                            let abs_secs = clip.timeline_in.to_secs_f64() + kf.time_secs;
                            let rate = self.state.playhead.rate;
                            self.state.playhead = klipp_core::Timecode::from_secs_f64(abs_secs, rate);
                            let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                        }
                    }
                }
            }
            // Alt+Right — Navigate to next keyframe
            if i.modifiers.alt && !i.modifiers.ctrl && i.key_pressed(egui::Key::ArrowRight) {
                if let Some(clip_id) = self.state.selected_clip {
                    let seq = &self.state.sequences[self.state.active_sequence_idx];
                    let clip_and_kind = seq.tracks.iter().find_map(|t| {
                        t.clips.iter().find(|c| c.id == clip_id).map(|c| (c, t.kind))
                    });
                    if let Some((clip, track_kind)) = clip_and_kind {
                        let clip_time_secs = self.state.playhead.to_secs_f64() - clip.timeline_in.to_secs_f64();
                        let keyframes = if track_kind == klipp_timeline::TrackKind::Audio {
                            match self.state.rubber_band_param {
                                klipp_core::AudioParam::Volume => &clip.volume_keyframes,
                                klipp_core::AudioParam::Pan => &clip.pan_keyframes,
                                klipp_core::AudioParam::Gain => &clip.gain_keyframes,
                            }
                        } else {
                            match self.state.video_rubber_band_param {
                                klipp_core::VideoParam::Opacity => &clip.opacity_keyframes,
                                klipp_core::VideoParam::PositionX => &clip.transform_pos_x_keyframes,
                                klipp_core::VideoParam::PositionY => &clip.transform_pos_y_keyframes,
                                klipp_core::VideoParam::Scale => &clip.transform_scale_keyframes,
                                klipp_core::VideoParam::Rotation => &clip.transform_rotation_keyframes,
                            }
                        };
                        // Find next keyframe (strictly after current time with tolerance)
                        let next = keyframes.iter()
                            .find(|kf| kf.time_secs > clip_time_secs + 0.001);
                        if let Some(kf) = next {
                            let abs_secs = clip.timeline_in.to_secs_f64() + kf.time_secs;
                            let rate = self.state.playhead.rate;
                            self.state.playhead = klipp_core::Timecode::from_secs_f64(abs_secs, rate);
                            let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                        }
                    }
                }
            }
            // M — Add sequence marker (no modifiers, no shift)
            if i.key_pressed(egui::Key::M) && !i.modifiers.ctrl && !i.modifiers.alt && !i.modifiers.shift {
                cmds.push(AppCommand::AddMarker {
                    position: self.state.playhead,
                    label: String::new(),
                    color: klipp_core::MarkerColor::Blue,
                });
            }
            // Shift+M — Add per-clip marker at playhead on selected clip
            if i.key_pressed(egui::Key::M) && i.modifiers.shift && !i.modifiers.ctrl && !i.modifiers.alt {
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
            if i.key_pressed(egui::Key::Z) && !i.modifiers.ctrl && !i.modifiers.shift {
                let dur_secs = self.state.sequence_duration().to_secs_f64() as f32;
                if dur_secs > 0.0 {
                    let content_width = self.state.last_timeline_content_width - 20.0;
                    self.state.timeline_zoom = (content_width / dur_secs).clamp(0.1, 2000.0);
                    self.state.timeline_scroll_x = 0.0;
                }
            }
            // Shift+Z — Zoom to selection
            if i.key_pressed(egui::Key::Z) && i.modifiers.shift && !i.modifiers.ctrl {
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
                let new_zoom = (old_zoom * 1.5).clamp(0.1, 2000.0);
                self.state.timeline_scroll_x = (playhead_secs * new_zoom
                    - (playhead_secs * old_zoom - self.state.timeline_scroll_x))
                    .max(0.0);
                self.state.timeline_zoom = new_zoom;
            }
            // - — Zoom out (playhead-centred)
            if i.key_pressed(egui::Key::Minus) && !i.modifiers.ctrl {
                let playhead_secs = self.state.playhead_smooth_secs as f32;
                let old_zoom = self.state.timeline_zoom;
                let new_zoom = (old_zoom / 1.5).clamp(0.1, 2000.0);
                self.state.timeline_scroll_x = (playhead_secs * new_zoom
                    - (playhead_secs * old_zoom - self.state.timeline_scroll_x))
                    .max(0.0);
                self.state.timeline_zoom = new_zoom;
            }
        });

        cmds
    }
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
    }
}

fn apply_preset_to_dialog(dialog: &mut ExportDialogState, settings: &ExportPresetSettings) {
    dialog.format = settings.format;
    dialog.video_codec = settings.video_codec;
    dialog.crf = settings.crf;
    dialog.audio_codec = settings.audio_codec;
    dialog.preset = settings.preset;
    dialog.audio_bitrate = settings.audio_bitrate_kbps.unwrap_or(192);
}

impl eframe::App for KlippApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Check if any popup/menu/combobox/foreground layer is active (blocks background interaction)
        let popup_open = ctx.memory(|m| m.any_popup_open());
        // Also check if pointer is over a foreground layer (menus, context menus)
        let pointer_over_foreground = ctx.input(|i| i.pointer.hover_pos())
            .and_then(|pos| ctx.layer_id_at(pos))
            .is_some_and(|layer| layer.order == egui::Order::Foreground || layer.order == egui::Order::Tooltip);
        self.state.modal_or_popup_open = popup_open || pointer_over_foreground;

        // Smooth playhead interpolation BEFORE draining events
        if self.state.playback == PlaybackState::Playing {
            let wall_elapsed = self.state.playhead_anchor_wall.elapsed().as_secs_f64();
            let anchor_secs = self.state.playhead_anchor.to_secs_f64();
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
        self.drain_events(ctx);
        self.drain_thumbnails();
        // Keep repainting while filmstrip is generating so timeline updates progressively
        if self.state.filmstrip_generating {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        self.drain_audio_conforms();
        self.drain_proxy_events();
        self.drain_proxy_progress();
        self.poll_render_queue();
        self.upload_frame_to_egui(ctx);
        self.upload_title_overlay(ctx);
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

        // Window title with project name and dirty indicator
        {
            let title = match &self.state.project_path {
                Some(p) => {
                    let name = p.file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Untitled".into());
                    if self.state.dirty {
                        format!("Klipp \u{2014} {name} \u{2022}")
                    } else {
                        format!("Klipp \u{2014} {name}")
                    }
                }
                None => "Klipp".to_string(),
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }

        // Debounced preferences save (at most once per second)
        if self.prefs_dirty && self.prefs_last_save.elapsed().as_secs() >= 1 {
            // Sync prefs to state
            self.state.history.set_max_size(self.prefs.editing.undo_limit);
            self.state.audio_fade_overlay_mode = self.prefs.audio.fade_overlay_mode;
            self.state.audio_show_rubber_band = self.prefs.audio.show_rubber_band;
            self.state.audio_show_waveform = self.prefs.audio.show_waveform;
            self.state.audio_anti_click_fade = self.prefs.audio.anti_click_fade;
            self.state.audio_show_crossfade_indicators = self.prefs.audio.show_crossfade_indicators;
            let _ = self.cmd_tx.send(AppCommand::SetAntiClickFade(self.state.audio_anti_click_fade));
            if let Err(e) = self.prefs.save() {
                tracing::warn!("Failed to save preferences: {e}");
            }
            self.prefs_dirty = false;
            self.prefs_last_save = std::time::Instant::now();
        }

        // Auto-save
        let interval = self.prefs.general.auto_save_interval_secs;
        if interval > 0
            && self.state.dirty
            && self.state.project_path.is_some()
            && self.last_autosave.elapsed().as_secs() >= interval as u64
        {
            if let Some(ref project_path) = self.state.project_path.clone() {
                let autosave_path = autosave_path_for(project_path);
                let color = klipp_project::ColorSettings {
                    global_lut_path: self.state.global_lut_path.clone(),
                    global_lut_opacity: self.state.global_lut_opacity,
                    track_header_width: self.state.track_header_width,
                    auto_generate_filmstrips: self.state.project_auto_filmstrips,
                    auto_generate_skim: self.state.project_auto_skim,
                };
                let active_seq_id = self.state.sequences[self.state.active_sequence_idx].id.to_string();
                match klipp_project::save_project_with_color(&autosave_path, &self.state.sequences, &self.state.media_pool, &color, Some(&active_seq_id), self.state.created_at.as_deref()) {
                    Ok(()) => tracing::info!("Auto-saved to {:?}", autosave_path),
                    Err(e) => tracing::warn!("Auto-save failed: {e}"),
                }
            }
            self.last_autosave = std::time::Instant::now();
        }

        // Continuous repaint during playback (must be AFTER drain_events so Playing state is set)
        if self.state.playback == PlaybackState::Playing {
            ctx.request_repaint();
        }

        // VU meter peak decay (~10dB/sec at 60fps → ~0.016 per frame)
        let decay = 0.016;
        for (l, r) in &mut self.state.track_peak_levels {
            *l = (*l - decay).max(0.0);
            *r = (*r - decay).max(0.0);
        }
        self.state.master_peak_left = (self.state.master_peak_left - decay).max(0.0);
        self.state.master_peak_right = (self.state.master_peak_right - decay).max(0.0);

        // Update performance stats
        if self.state.show_perf_overlay {
            self.update_perf_stats(ctx);
        }

        // Update thumbnails: convert ready thumbnails to textures
        self.update_thumbnail_textures(ctx);

        // Reset skim flag for this frame (set by browser or timeline hover)
        self.state.browser_skim_active_this_frame = false;

        // Process pending skim transcodes (queued during drain_events)
        let pending_skims: Vec<MediaId> = self.state.pending_skim_transcodes.drain(..).collect();
        for mid in pending_skims {
            // Re-use the command processing path
            self.handle_skim_transcode(mid);
        }

        // Upload pending skim frame texture
        // TODO: apply clip effects during skim (requires stable OFX render on UI thread)
        if let Some((media_id, rgba, width, height)) = self.state.browser_skim_pending.take() {
            // Write to frame buffer so scopes update during skimming
            {
                let mut fb = self.frame_buffer.lock().unwrap();
                fb.write_frame(rgba.clone(), width, height);
            }
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [width as usize, height as usize],
                &rgba,
            );
            let tex = ctx.load_texture("browser_skim", image, egui::TextureOptions::LINEAR);
            self.state.browser_skim_texture = Some(tex);
            self.state.browser_skim_media_id = Some(media_id);
        }

        let mut all_cmds = Vec::new();

        // 2. Handle global keyboard shortcuts
        let kb_cmds = self.handle_global_shortcuts(ctx);
        all_cmds.extend(kb_cmds);

        // 3. Handle transport keyboard shortcuts
        let transport_cmds = self.transport.handle_keyboard(ctx, &self.state);
        all_cmds.extend(transport_cmds);

        // 4. Unsaved changes dialog
        let dialog_cmds = self.show_unsaved_dialog(ctx);
        all_cmds.extend(dialog_cmds);

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
                            all_cmds.push(AppCommand::ConfirmRemoveFromPool { media_id });
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
                            all_cmds.push(AppCommand::ConfirmDeleteUnusedMedia);
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
                    all_cmds.push(AppCommand::ForceExportSequence { settings });
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
        all_cmds.extend(export_cmds);

        // 4c. Render queue window
        self.show_render_queue_window(ctx);

        // 4c1. New Project dialog
        if self.state.show_new_project_dialog {
            if let Some(cmd) = self.new_project_dialog.show(
                ctx,
                &mut self.state.show_new_project_dialog,
                &self.theme,
            ) {
                all_cmds.push(cmd);
            }
        }

        // 4c2. Consolidate Project dialog
        if self.state.show_consolidate_dialog {
            if let Some(ref project_path) = self.state.project_path.clone() {
                let mut open = true;
                // Count media files outside the project directory
                let project_dir = project_path.clone();
                let external_media: Vec<_> = self.state.media_pool.all()
                    .filter(|item| !item.offline && !item.path.starts_with(&project_dir))
                    .map(|item| (item.id, item.path.clone(), item.file_size_bytes))
                    .collect();
                let file_count = external_media.len();
                let total_bytes: u64 = external_media.iter().map(|(_, _, s)| s).sum();

                egui::Window::new("Consolidate Project")
                    .open(&mut open)
                    .collapsible(false)
                    .resizable(false)
                    .default_width(380.0)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.label("Copy all referenced media files into the project folder for portability.");
                        ui.add_space(8.0);
                        if file_count == 0 {
                            ui.label(egui::RichText::new("All media is already inside the project folder.")
                                .color(self.theme.text_secondary));
                        } else {
                            ui.label(format!("Media to copy: {} file(s)", file_count));
                            ui.label(format!("Total size: {}", format_size_brief(total_bytes)));
                            ui.add_space(12.0);
                            ui.horizontal(|ui| {
                                if ui.button("Cancel").clicked() {
                                    self.state.show_consolidate_dialog = false;
                                }
                                if ui.button("Consolidate").clicked() {
                                    self.state.show_consolidate_dialog = false;
                                    let media_dir = project_dir.join("media").join("files");
                                    if let Err(e) = std::fs::create_dir_all(&media_dir) {
                                        tracing::error!("Failed to create media/files dir: {e}");
                                    } else {
                                        let mut copied = 0usize;
                                        for (media_id, src_path, _) in &external_media {
                                            let filename = src_path.file_name().unwrap_or_default();
                                            let dst = media_dir.join(filename);
                                            match std::fs::copy(src_path, &dst) {
                                                Ok(_) => {
                                                    // Update media item path
                                                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                                                        item.path = dst;
                                                    }
                                                    copied += 1;
                                                }
                                                Err(e) => {
                                                    tracing::error!("Failed to copy {:?}: {e}", src_path);
                                                }
                                            }
                                        }
                                        self.state.dirty = true;
                                        self.state.toast.show(format!("Consolidated {copied} file(s)"));
                                        tracing::info!("Consolidated {copied}/{file_count} media files");
                                    }
                                }
                            });
                        }
                    });
                if !open {
                    self.state.show_consolidate_dialog = false;
                }
            } else {
                self.state.show_consolidate_dialog = false;
            }
        }

        // 4c3. Project Info window
        if self.state.show_project_info {
            let mut still_open = true;
            crate::panels::project_info::show_project_info(
                ctx,
                &mut self.state,
                &self.theme,
                &mut still_open,
            );
            if !still_open {
                self.state.show_project_info = false;
            }
        }

        // 4d. Preferences window
        if self.state.show_preferences {
            let (still_open, pref_cmds) = self.preferences_panel.show(
                ctx,
                &mut self.prefs,
                &mut self.prefs_dirty,
            );
            all_cmds.extend(pref_cmds);
            // Sync skim_audio pref → state (checkbox may have changed)
            if self.state.audio_skimming_enabled != self.prefs.playback.skim_audio {
                self.state.audio_skimming_enabled = self.prefs.playback.skim_audio;
                if !self.state.audio_skimming_enabled && self.state.skim_audio_session.is_some() {
                    all_cmds.push(AppCommand::BrowserSkimAudioEnd);
                }
            }
            if !still_open {
                self.state.show_preferences = false;
            }
        }

        // Handle fullscreen toggle
        if self.state.project_path.is_none() {
            // Launch screen: no project open
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(self.theme.bg_void))
                .show(ctx, |ui| {
                    let recent = self.prefs.general.recent_projects.clone();
                    let cmds = self.launch_screen.show(ctx, ui, &recent, &self.theme);
                    all_cmds.extend(cmds);
                });
        } else if self.state.fullscreen_viewer {
            // Fullscreen viewer: just show the video frame
            egui::CentralPanel::default().show(ctx, |ui| {
                let cmds = self.viewer.show_fullscreen(ui, &mut self.state);
                all_cmds.extend(cmds);
            });
        } else {
            // ── Manual rect layout ──
            // Use CentralPanel as the container, then compute rects manually.
            egui::CentralPanel::default()
                .frame(egui::Frame::none().fill(self.theme.bg_void))
                .show(ctx, |ui| {
                    let full = ui.available_rect_before_wrap();
                    let theme = &self.theme;

                    // ── Toolbar (top 44px) ──
                    let toolbar_rect = egui::Rect::from_min_size(
                        full.min,
                        egui::vec2(full.width(), theme::TOOLBAR_HEIGHT),
                    );

                    // ── Status bar (bottom 26px) ──
                    let statusbar_rect = egui::Rect::from_min_size(
                        egui::pos2(full.min.x, full.max.y - theme::STATUSBAR_HEIGHT),
                        egui::vec2(full.width(), theme::STATUSBAR_HEIGHT),
                    );

                    // ── Main row (between toolbar and status bar) ──
                    let main_top = toolbar_rect.max.y;
                    let main_bottom = statusbar_rect.min.y;
                    let main_height = main_bottom - main_top;

                    // Browser (left)
                    let browser_w = self.layout.browser_width;
                    let browser_rect = egui::Rect::from_min_size(
                        egui::pos2(full.min.x, main_top),
                        egui::vec2(browser_w, main_height),
                    );

                    // Left resizer
                    let left_resizer_rect = egui::Rect::from_min_size(
                        egui::pos2(full.min.x + browser_w, main_top),
                        egui::vec2(theme::RESIZER_THICKNESS, main_height),
                    );

                    // Inspector (right) — clamp to available space
                    let max_inspector_w = (full.width() - browser_w - theme::RESIZER_THICKNESS * 2.0 - 100.0).max(theme::INSPECTOR_MIN_W);
                    let inspector_w = self.layout.inspector_width.min(max_inspector_w);
                    let inspector_rect = egui::Rect::from_min_size(
                        egui::pos2(full.max.x - inspector_w, main_top),
                        egui::vec2(inspector_w, main_height),
                    );

                    // Right resizer
                    let right_resizer_rect = egui::Rect::from_min_size(
                        egui::pos2(
                            full.max.x - inspector_w - theme::RESIZER_THICKNESS,
                            main_top,
                        ),
                        egui::vec2(theme::RESIZER_THICKNESS, main_height),
                    );

                    // Center column (between left resizer and right resizer)
                    let center_left = left_resizer_rect.max.x;
                    let center_right = right_resizer_rect.min.x;
                    let center_w = (center_right - center_left).max(100.0);

                    // ── Color workspace: viewer + toggle bar + scopes + nodes ──
                    let in_color_ws = self.state.show_color_panel;
                    let show_scopes_bar = in_color_ws && self.state.show_scopes;
                    let show_nodes_bar = in_color_ws && self.state.show_node_strip;

                    // In Color workspace: allocate bottom space for toggle + scopes + nodes
                    // Use ratio to split between viewer+timeline and bottom panels
                    let toggle_bar_h: f32 = if in_color_ws { 22.0 } else { 0.0 };
                    let center_total = main_bottom - main_top;
                    let has_bottom_panels = show_scopes_bar || show_nodes_bar;

                    // Bottom panels (scopes + nodes) share a flexible height controlled by the ratio
                    let fixed_h = toggle_bar_h + theme::RESIZER_THICKNESS;
                    let flexible_h = (center_total - fixed_h).max(200.0);
                    let bottom_panel_h: f32 = if has_bottom_panels {
                        let viewer_ratio = self.layout.color_viewer_height_ratio.clamp(0.3, 0.75);
                        (flexible_h * (1.0 - viewer_ratio)).clamp(80.0, flexible_h * 0.6)
                    } else {
                        0.0
                    };

                    // Split bottom panel between scopes and nodes
                    let (scopes_h, nodes_h) = match (show_scopes_bar, show_nodes_bar) {
                        (true, true) => (bottom_panel_h * 0.6, bottom_panel_h * 0.4),
                        (true, false) => (bottom_panel_h, 0.0),
                        (false, true) => (0.0, bottom_panel_h),
                        (false, false) => (0.0, 0.0),
                    };

                    let color_bottom_total = toggle_bar_h + scopes_h + nodes_h;
                    let color_bottom_top = main_bottom - color_bottom_total;

                    let toggle_bar_rect = egui::Rect::from_min_size(
                        egui::pos2(center_left, color_bottom_top),
                        egui::vec2(center_w, toggle_bar_h),
                    );
                    let scopes_rect = egui::Rect::from_min_size(
                        egui::pos2(center_left, toggle_bar_rect.max.y),
                        egui::vec2(center_w, scopes_h),
                    );
                    let nodes_rect = egui::Rect::from_min_size(
                        egui::pos2(center_left, scopes_rect.max.y),
                        egui::vec2(center_w, nodes_h),
                    );

                    let _color_panel_open = false;

                    // Timeline: hidden in Color workspace, normal otherwise
                    // Timeline sits above the scopes/nodes area (or fills to bottom if none)
                    let timeline_bottom = color_bottom_top;
                    let max_timeline_h = (timeline_bottom - main_top - theme::RESIZER_THICKNESS) * theme::TIMELINE_MAX_H_PCT;
                    let timeline_min_h = theme::TIMELINE_MIN_H;
                    let timeline_h = self
                        .layout
                        .timeline_height
                        .clamp(timeline_min_h, max_timeline_h);

                    let timeline_rect = egui::Rect::from_min_size(
                        egui::pos2(center_left, timeline_bottom - timeline_h),
                        egui::vec2(center_w, timeline_h),
                    );

                    // Curve editor (between viewer and timeline when visible)
                    let curve_editor_h: f32 = if self.state.show_curve_editor { 180.0 } else { 0.0 };
                    let curve_editor_rect = egui::Rect::from_min_size(
                        egui::pos2(center_left, timeline_rect.min.y - curve_editor_h - if curve_editor_h > 0.0 { theme::RESIZER_THICKNESS } else { 0.0 }),
                        egui::vec2(center_w, curve_editor_h),
                    );

                    // Horizontal resizer (between viewer and curve editor / timeline)
                    let h_resizer_rect = egui::Rect::from_min_size(
                        egui::pos2(
                            center_left,
                            curve_editor_rect.min.y - theme::RESIZER_THICKNESS,
                        ),
                        egui::vec2(center_w, theme::RESIZER_THICKNESS),
                    );

                    // Viewer (top of center, above horizontal resizer)
                    let viewer_rect = egui::Rect::from_min_max(
                        egui::pos2(center_left, main_top),
                        egui::pos2(center_left + center_w, h_resizer_rect.min.y),
                    );

                    // ── Render panels into their rects ──

                    // Toolbar
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(toolbar_rect), |ui| {
                        ui.set_min_size(toolbar_rect.size());
                        ui.painter().rect_filled(
                            toolbar_rect,
                            0.0,
                            theme.bg_deep,
                        );
                        // Bottom border
                        ui.painter().hline(
                            toolbar_rect.x_range(),
                            toolbar_rect.max.y,
                            egui::Stroke::new(1.0, theme.border),
                        );
                        let recent = self.prefs.general.recent_projects.clone();
                        let cmds = self.toolbar.show(
                            ui,
                            &mut self.state,
                            &recent,
                            &self.theme,
                            &mut self.layout.active_tool,
                        );
                        all_cmds.extend(cmds);
                    });

                    // Transport (rendered below toolbar, overlapping for now —
                    // will be merged into toolbar in Phase 3)
                    // For now, render transport inside a thin strip at the
                    // bottom of the toolbar area.
                    // (Transport is currently 48px; we compress it into toolbar for now.)

                    // Status bar
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(statusbar_rect), |ui| {
                        ui.set_min_size(statusbar_rect.size());
                        ui.painter()
                            .rect_filled(statusbar_rect, 0.0, theme.bg_void);
                        ui.painter().hline(
                            statusbar_rect.x_range(),
                            statusbar_rect.min.y,
                            egui::Stroke::new(1.0, theme.border),
                        );
                        crate::panels::status_bar::show(ui, &self.state, &self.theme);
                    });

                    // Browser (left panel)
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(browser_rect), |ui| {
                        ui.set_min_size(browser_rect.size());
                        ui.painter()
                            .rect_filled(browser_rect, 0.0, theme.bg_panel);
                        let cmds = self.browser.show(
                            ui,
                            &mut self.state,
                            &self.theme,
                            &mut self.layout.active_browser_tab,
                        );
                        all_cmds.extend(cmds);
                        // Sync view mode to prefs if changed
                        if self.browser.browser_view_mode != self.prefs.editing.media_browser_view_mode {
                            self.prefs.editing.media_browser_view_mode = self.browser.browser_view_mode;
                            let _ = self.prefs.save();
                        }
                    });

                    // Inspector (right panel) — contextual based on workspace
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inspector_rect), |ui| {
                        ui.set_min_size(inspector_rect.size());
                        ui.painter().rect_filled(inspector_rect, 0.0, theme.bg_panel);

                        let in_color_workspace = self.state.show_color_panel
                            && self.state.selected_clip.is_some();

                        if in_color_workspace {
                            // Color workspace: tabbed simple inspector
                            let clip_id = self.state.selected_clip.unwrap();
                            let seq_idx = self.state.active_sequence_idx;
                            let clip = self.state.sequences[seq_idx].tracks.iter_mut()
                                .find_map(|t| t.find_clip_mut(clip_id));
                            if let Some(clip) = clip {
                                if self.state.show_node_strip && self.state.selected_node_id.is_some() {
                                    // Node strip visible + node selected → show that node's params
                                    let cmds = self.node_inspector_panel.show(
                                        ui, &self.state.selected_node_id, &mut clip.node_graph, clip_id, &self.theme,
                                    );
                                    all_cmds.extend(cmds);
                                } else {
                                    // Simple tabbed mode (default)
                                    let cmds = self.node_inspector_panel.show_simple(
                                        ui, &mut clip.node_graph, clip_id,
                                        &mut self.state.selected_node_id,
                                        &mut self.state.color_tab,
                                        &self.theme,
                                    );
                                    all_cmds.extend(cmds);
                                }
                            }
                        } else {
                            // Edit workspace: standard clip inspector
                            let cmds = self.new_inspector.show(
                                ui, &mut self.state, &self.theme,
                                &mut self.layout.active_inspector_tab,
                            );
                            all_cmds.extend(cmds);
                        }
                    });

                    // Viewer (center top) — pure black, absolutely no border
                    // Paint black fill slightly expanded to cover any sub-pixel gaps
                    ui.painter().rect_filled(viewer_rect.expand(1.0), 0.0, egui::Color32::BLACK);
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(viewer_rect), |ui| {
                        // Zero out ALL spacing and strokes for borderless viewer
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
                        let cmds = self.viewer.show(ui, &mut self.state, &self.theme);
                        all_cmds.extend(cmds);
                    });

                    // Curve editor (between viewer and timeline)
                    if self.state.show_curve_editor && curve_editor_h > 0.0 {
                        self.curve_editor.sync_param_from_state(&self.state);
                        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(curve_editor_rect), |ui| {
                            ui.set_min_size(curve_editor_rect.size());
                            let cmds = self.curve_editor.show(ui, &mut self.state, &self.theme);
                            all_cmds.extend(cmds);
                        });
                    }

                    // Timeline (center bottom) — always visible
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(timeline_rect), |ui| {
                        ui.set_min_size(timeline_rect.size());
                        let cmds = self.new_timeline.show(
                            ui,
                            &mut self.state,
                            &self.theme,
                            &mut self.timeline,
                        );
                        all_cmds.extend(cmds);
                    });

                    // ── Color workspace bottom panels ──
                    if in_color_ws {
                        // Resizer between viewer and bottom panels (draggable)
                        if show_scopes_bar || show_nodes_bar {
                            let resizer_rect = egui::Rect::from_min_size(
                                egui::pos2(center_left, toggle_bar_rect.min.y - theme::RESIZER_THICKNESS),
                                egui::vec2(center_w, theme::RESIZER_THICKNESS),
                            );
                            let delta = resizer::horizontal_resizer(ui, resizer_rect, theme);
                            if delta != 0.0 {
                                // Adjust the viewer ratio based on drag
                                let new_ratio = self.layout.color_viewer_height_ratio + delta / flexible_h;
                                self.layout.color_viewer_height_ratio = new_ratio.clamp(0.3, 0.75);
                                self.layout_dirty = true;
                            }
                        }

                        // Toggle bar: [Scopes] [Nodes] toggle buttons
                        if toggle_bar_h > 0.0 {
                            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(toggle_bar_rect), |ui| {
                                ui.set_min_size(toggle_bar_rect.size());
                                ui.painter().rect_filled(toggle_bar_rect, 0.0, theme.bg_panel);
                                ui.painter().hline(
                                    toggle_bar_rect.x_range(),
                                    toggle_bar_rect.min.y,
                                    egui::Stroke::new(0.5, theme.border),
                                );
                                ui.horizontal_centered(|ui| {
                                    ui.add_space(8.0);
                                    let scope_label = if self.state.show_scopes {
                                        egui::RichText::new("Scopes").size(10.0).color(theme.text_accent)
                                    } else {
                                        egui::RichText::new("Scopes").size(10.0).color(theme.text_dim)
                                    };
                                    if ui.selectable_label(self.state.show_scopes, scope_label).clicked() {
                                        self.state.show_scopes = !self.state.show_scopes;
                                        ui.ctx().request_repaint();
                                    }
                                    // Node strip toggle removed — node system disabled

                                    // Scope mode selector (when scopes visible)
                                    if self.state.show_scopes {
                                        ui.separator();
                                        use crate::state::ScopeMode;
                                        for (mode, label) in [
                                            (ScopeMode::Waveform, "Wave"),
                                            (ScopeMode::Parade, "Parade"),
                                            (ScopeMode::Vectorscope, "Vector"),
                                            (ScopeMode::Histogram, "Hist"),
                                        ] {
                                            let active = self.state.scope_mode == mode;
                                            let text = egui::RichText::new(label).size(9.0);
                                            let text = if active { text.color(theme.text_accent) } else { text.color(theme.text_dim) };
                                            if ui.selectable_label(active, text).clicked() {
                                                self.state.scope_mode = mode;
                                                // Force immediate repaint so scopes update instantly
                                                ui.ctx().request_repaint();
                                            }
                                        }
                                    }
                                });
                            });
                        }

                        // Scopes panel — read live state (toggle bar may have changed it this frame)
                        if self.state.show_scopes {
                            // Compute rect dynamically from current state
                            let live_scopes_h = if scopes_h > 0.0 { scopes_h } else {
                                // Scopes just got enabled this frame — give it reasonable default
                                let ratio = self.layout.color_viewer_height_ratio.clamp(0.3, 0.75);
                                let flex = (main_bottom - main_top - toggle_bar_h - theme::RESIZER_THICKNESS).max(200.0);
                                let total_bottom = (flex * (1.0 - ratio)).clamp(80.0, flex * 0.6);
                                if show_nodes_bar { total_bottom * 0.6 } else { total_bottom }
                            };
                            let live_scopes_rect = egui::Rect::from_min_size(
                                egui::pos2(center_left, toggle_bar_rect.max.y),
                                egui::vec2(center_w, live_scopes_h),
                            );
                            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(live_scopes_rect), |ui| {
                                ui.set_min_size(live_scopes_rect.size());
                                ui.painter().rect_filled(live_scopes_rect, 0.0, theme.bg_deep);
                                crate::widgets::scopes::show_scopes(
                                    ui,
                                    &mut self.state.scope_state,
                                    self.state.scope_mode,
                                    &self.frame_buffer,
                                );
                            });
                            // Force repaint to recalculate layout on next frame
                            ui.ctx().request_repaint();
                        }

                        // Node strip panel — read live state
                        if self.state.show_node_strip {
                            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(nodes_rect), |ui| {
                                ui.set_min_size(nodes_rect.size());
                                ui.painter().rect_filled(nodes_rect, 0.0, theme.bg_deep);
                                let clip_id = self.state.selected_clip;
                                if let Some(cid) = clip_id {
                                    let seq_idx = self.state.active_sequence_idx;
                                    if let Some(clip) = self.state.sequences[seq_idx].tracks.iter_mut()
                                        .find_map(|t| t.find_clip_mut(cid))
                                    {
                                        let cmds = self.node_graph_panel.show(
                                            ui, &mut self.state.selected_node_id,
                                            &mut clip.node_graph, cid, &self.theme,
                                        );
                                        all_cmds.extend(cmds);
                                    } else {
                                        ui.painter().text(
                                            nodes_rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "Clip not found in sequence",
                                            egui::FontId::proportional(12.0),
                                            egui::Color32::from_rgb(100, 100, 110),
                                        );
                                    }
                                } else {
                                    ui.painter().text(
                                        nodes_rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        "Select a clip to view its node graph",
                                        egui::FontId::proportional(12.0),
                                        egui::Color32::from_rgb(100, 100, 110),
                                    );
                                }
                            });
                        }
                    }

                    // ── Draw resizers AFTER panels (so they get interaction priority) ──
                    let left_delta =
                        resizer::vertical_resizer(ui, left_resizer_rect, theme);
                    let right_delta =
                        resizer::vertical_resizer(ui, right_resizer_rect, theme);
                    let h_delta =
                        resizer::horizontal_resizer(ui, h_resizer_rect, theme);

                    // Apply resizer deltas
                    if left_delta != 0.0 {
                        self.layout.browser_width = (self.layout.browser_width
                            + left_delta)
                            .clamp(theme::BROWSER_MIN_W, theme::BROWSER_MAX_W);
                    }
                    if right_delta != 0.0 {
                        self.layout.inspector_width = (self.layout.inspector_width
                            - right_delta)
                            .clamp(theme::INSPECTOR_MIN_W, theme::INSPECTOR_MAX_W);
                    }
                    if h_delta != 0.0 {
                        self.layout.timeline_height = (self.layout.timeline_height
                            - h_delta)
                            .clamp(timeline_min_h, max_timeline_h);
                    }
                    // Track layout dirty for persistence
                    let resizer_active = left_delta != 0.0
                        || right_delta != 0.0
                        || h_delta != 0.0;
                    if resizer_active {
                        self.layout_dirty = true;
                    }
                    if self.layout_dirty
                        && !ui.input(|i| i.pointer.any_pressed() || i.pointer.is_decidedly_dragging())
                    {
                        self.layout.save();
                        self.layout_dirty = false;
                    }

                    // Transport is now integrated into the toolbar (P1-2).
                    // Keyboard shortcuts still handled by self.transport.handle_keyboard().
                });
        }

        // Markers panel
        if self.state.show_markers_panel {
            let cmds = self.markers.show(ctx, &mut self.state);
            all_cmds.extend(cmds);
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
                        ui.label(egui::RichText::new("Klipp").strong().size(20.0));
                        ui.add_space(4.0);
                        ui.label(format!("Version {}-dev", env!("CARGO_PKG_VERSION")));
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
            egui::Window::new("Missing Media")
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("The following files could not be found:");
                    ui.add_space(8.0);
                    let offline_ids = self.state.offline_media_ids.clone();
                    for mid in &offline_ids {
                        if let Some(item) = self.state.media_pool.get(mid) {
                            ui.horizontal(|ui| {
                                ui.add(Icon::XCircle.image(14.0).tint(egui::Color32::from_rgb(255, 100, 100)));
                                ui.label(&item.filename);
                                ui.label(item.path.to_string_lossy().to_string());
                            });
                        }
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Skip All").clicked() {
                            self.state.show_relink_dialog = false;
                        }
                        if ui.button("Relink All...").clicked() {
                            if let Some(dir) = rfd::FileDialog::new()
                                .set_title("Select folder to search for media")
                                .pick_folder()
                            {
                                let offline = self.state.offline_media_ids.clone();
                                for mid in &offline {
                                    if let Some(item) = self.state.media_pool.get(mid) {
                                        let filename = item.filename.clone();
                                        // Search recursively for matching filename
                                        if let Some(found) = find_file_recursive(&dir, &filename) {
                                            all_cmds.push(AppCommand::RelinkMedia {
                                                media_id: *mid,
                                                new_path: found,
                                            });
                                        }
                                    }
                                }
                                self.state.show_relink_dialog = false;
                            }
                        }
                    });
                });
            if !open {
                self.state.show_relink_dialog = false;
            }
        }

        // Ensure continuous repaint during playback (especially in fullscreen)
        if self.state.playback == PlaybackState::Playing {
            ctx.request_repaint();
        }

        // 10. Process quit
        let has_quit = all_cmds.iter().any(|c| matches!(c, AppCommand::Quit));

        // Save layout state on quit
        if has_quit {
            self.layout.save();
        }

        // 11. Send all commands
        self.send_commands(all_cmds);

        // 11.5. Upload title overlay texture if a title property was changed this frame
        self.upload_title_overlay(ctx);

        // Clear title overlay during playback (engine handles compositing then)
        if self.state.playback == PlaybackState::Playing {
            self.viewer.title_overlay_texture = None;
        }

        // 11a. Skim cleanup: if no panel set the skim flag this frame, clear skim state
        let lost_focus = !ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if (!self.state.browser_skim_active_this_frame && self.state.browser_skim_media_id.is_some()) || lost_focus {
            if self.state.browser_skim_media_id.is_some() {
                self.state.browser_skim_texture = None;
                self.state.browser_skim_media_id = None;
                self.state.viewer_mode = ViewerMode::Program;
            }
            // End audio skim session when mouse leaves or app loses focus
            if self.state.skim_audio_session.is_some() {
                self.send_commands(vec![AppCommand::BrowserSkimAudioEnd]);
            }
        }

        // 11b. Handle fullscreen viewport transition (send only once)
        if self.state.fullscreen_changed {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                self.state.fullscreen_viewer,
            ));
            self.state.fullscreen_changed = false;
        }

        // 11c. Toast notifications + drag-drop cleanup
        self.state.toast.render(ctx, &self.theme);
        self.state.toast.cleanup();
        self.state.drag_drop.frame_start();

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
                                .fill(egui::Color32::from_rgb(35, 35, 42))
                                .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 70, 85)))
                                .rounding(6.0)
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
                                                    .color(egui::Color32::from_rgb(200, 200, 210))
                                                    .size(12.0),
                                            );
                                        } else {
                                            ui.label(
                                                egui::RichText::new(&msg_clone)
                                                    .color(egui::Color32::from_rgb(200, 200, 210))
                                                    .size(12.0),
                                            );
                                        }
                                        ui.add_space(8.0);
                                        if ui.add(egui::Button::image(Icon::X.image(12.0).tint(egui::Color32::from_rgb(220, 220, 230)))).on_hover_text("Dismiss").clicked() {
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

        // 13. Update window title
        self.update_window_title(ctx);

        // Intercept window X button close — treat same as File → Quit
        let window_close_requested = ctx.input(|i| i.viewport().close_requested());
        if window_close_requested {
            if self.state.dirty && self.state.project_path.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.state.show_unsaved_dialog = true;
                self.state.unsaved_dialog_action = UnsavedAction::Quit;
            } else {
                // Not dirty — clean up and allow close
                if let Some(ref path) = self.state.project_path {
                    let autosave = autosave_path_for(path);
                    if autosave.exists() {
                        let _ = std::fs::remove_dir_all(&autosave);
                    }
                }
                // Don't cancel — let the close proceed
            }
        }

        if has_quit {
            if self.state.dirty && self.state.project_path.is_some() {
                self.state.show_unsaved_dialog = true;
                self.state.unsaved_dialog_action = UnsavedAction::Quit;
            } else {
                if let Some(ref path) = self.state.project_path {
                    let autosave = autosave_path_for(path);
                    if autosave.exists() {
                        let _ = std::fs::remove_dir_all(&autosave);
                    }
                }
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        if self.state.playback == PlaybackState::Playing || self.state.render_queue.has_active_job() {
            ctx.request_repaint();
        }
    }
}

impl KlippApp {
    /// Check if the track containing a given clip is locked.
    fn clip_track_is_locked(&self, clip_id: &ClipId) -> bool {
        self.state.sequences[self.state.active_sequence_idx].tracks.iter()
            .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
            .is_some_and(|t| t.locked)
    }

    /// Check if a track uses magnetic editing (Primary, Connected, or ConnectedStoryline).
    fn is_magnetic_track(&self, track_index: usize) -> bool {
        matches!(
            self.state.sequences[self.state.active_sequence_idx].tracks.get(track_index).map(|t| &t.magnetic_role),
            Some(klipp_timeline::TrackRole::Primary | klipp_timeline::TrackRole::Connected | klipp_timeline::TrackRole::ConnectedStoryline)
        )
    }

    fn handle_skim_transcode(&mut self, media_id: MediaId) {
        if self.state.skim_transcoding.contains(&media_id) {
            return;
        }
        if let Some(item) = self.state.media_pool.get(&media_id) {
            if item.skim_file.as_ref().is_some_and(|p| p.exists()) {
                return;
            }
            let source_path = item.path.clone();
            let duration_secs = item.duration.to_secs_f64();
            let filename = item.filename.clone();

            // Use content-based cache key for skim path
            let skim_dir = dirs::cache_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
                .join("klipp")
                .join("skim");
            let _ = std::fs::create_dir_all(&skim_dir);
            let skim_path = klipp_media::disk_cache::skim_transcode_path(
                &klipp_media::disk_cache::compute_cache_key(&source_path)
                    .unwrap_or_else(|| media_id.to_string()),
                media_id,
            );

            // Cache hit: skim already exists for this source file
            if skim_path.exists() {
                tracing::info!("Skim cache hit for {:?}", source_path);
                if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                    item.skim_file = Some(skim_path);
                }
                return;
            }

            // No cache — skip fresh generation if preference is off
            let skim_enabled = self.state.project_auto_skim
                .unwrap_or(self.prefs.editing.auto_generate_skim);
            if !skim_enabled {
                tracing::info!("Skim generation disabled by preference, skipping {:?}", source_path);
                return;
            }

            self.state.skim_transcoding.insert(media_id);
            self.state.skim_transcode_status = Some((filename, 0.0));

            let event_tx = self.proxy_event_tx.clone();
            std::thread::spawn(move || {
                let run_ffmpeg = |args: &[&str]| -> Result<std::process::ExitStatus, std::io::Error> {
                    use std::io::BufRead;
                    // Use -progress pipe:1 for clean key=value progress on stdout
                    let mut full_args: Vec<&str> = args.to_vec();
                    full_args.extend_from_slice(&["-progress", "pipe:1"]);

                    let mut child = std::process::Command::new("ffmpeg")
                        .args(&full_args)
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::null())
                        .spawn()?;

                    // Parse stdout for progress lines: "out_time_us=12345678"
                    if let Some(stdout) = child.stdout.take() {
                        let reader = std::io::BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if let Some(val) = line.strip_prefix("out_time_us=") {
                                if let Ok(us) = val.parse::<i64>() {
                                    if duration_secs > 0.0 && us > 0 {
                                        let secs = us as f64 / 1_000_000.0;
                                        let progress = (secs / duration_secs).clamp(0.0, 1.0) as f32;
                                        let _ = event_tx.send(AppEvent::SkimTranscodeProgress {
                                            media_id,
                                            progress,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    child.wait()
                };

                let src = source_path.to_str().unwrap_or("");
                let dst = skim_path.to_str().unwrap_or("");

                // Try NVENC first (GPU), fall back to libx264 (CPU)
                let result = run_ffmpeg(&[
                    "-y", "-hwaccel", "cuda",
                    "-i", src,
                    "-c:v", "h264_nvenc", "-g", "1", "-preset", "p1",
                    "-rc", "constqp", "-qp", "18",
                    "-an", "-vf", "scale_cuda=960:-2", dst,
                ]);

                let result = match &result {
                    Ok(status) if status.success() => result,
                    _ => {
                        tracing::info!("NVENC skim transcode failed, falling back to libx264");
                        run_ffmpeg(&[
                            "-y", "-i", src,
                            "-c:v", "libx264", "-g", "1", "-crf", "18",
                            "-preset", "ultrafast",
                            "-an", "-vf", "scale=960:-2", dst,
                        ])
                    }
                };

                match result {
                    Ok(status) if status.success() => {
                        let _ = event_tx.send(AppEvent::SkimTranscodeComplete {
                            media_id,
                            path: skim_path,
                        });
                    }
                    Ok(status) => {
                        let _ = event_tx.send(AppEvent::SkimTranscodeFailed {
                            media_id,
                            error: format!("ffmpeg exited with {}", status),
                        });
                    }
                    Err(e) => {
                        let _ = event_tx.send(AppEvent::SkimTranscodeFailed {
                            media_id,
                            error: format!("Failed to run ffmpeg: {}", e),
                        });
                    }
                }
            });
        }
    }

    fn drain_proxy_events(&mut self) {
        while let Ok(event) = self.proxy_event_rx.try_recv() {
            match event {
                AppEvent::ProxyGenerationStarted { media_id } => {
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_else(|| format!("{media_id}"));
                    tracing::info!("Proxy generation started for {}", filename);
                }
                AppEvent::ProxyGenerationComplete { media_id, path } => {
                    self.state.proxy_generating.remove(&media_id);
                    self.state.proxy_progress.remove(&media_id);
                    let elapsed_secs = self.state.proxy_start_times.remove(&media_id)
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0);
                    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_else(|| format!("{media_id}"));
                    tracing::info!(
                        "Proxy complete for {}: {} ({:.1} MB, {}s)",
                        filename, path.display(), file_size as f64 / (1024.0 * 1024.0), elapsed_secs
                    );
                    let time_str = if elapsed_secs >= 60 {
                        format!("{}m {:02}s", elapsed_secs / 60, elapsed_secs % 60)
                    } else {
                        format!("{}s", elapsed_secs)
                    };
                    self.state.proxy_status_message = Some((
                        format!("Proxy ready for {} ({:.1} MB, took {})", filename, file_size as f64 / (1024.0 * 1024.0), time_str),
                        std::time::Instant::now(),
                    ));
                    if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                        item.proxy_path = Some(path);
                        item.proxy_ready = true;
                    }
                    self.sync_media_pool_to_engine();
                }
                AppEvent::ProxyGenerationFailed { media_id, error } => {
                    self.state.proxy_generating.remove(&media_id);
                    self.state.proxy_progress.remove(&media_id);
                    self.state.proxy_start_times.remove(&media_id);
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_else(|| format!("{media_id}"));
                    tracing::warn!("Proxy generation failed for {}: {}", filename, error);
                    self.state.proxy_status_message = Some((
                        format!("Proxy failed for {}: {}", filename, error),
                        std::time::Instant::now(),
                    ));
                }
                AppEvent::SkimTranscodeProgress { media_id, progress } => {
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_default();
                    self.state.skim_transcode_status = Some((filename, progress));
                }
                AppEvent::SkimTranscodeComplete { media_id, path } => {
                    self.state.skim_transcoding.remove(&media_id);
                    if self.state.skim_transcoding.is_empty() {
                        self.state.skim_transcode_status = None;
                        self.state.skim_transcode_done_at = Some(std::time::Instant::now());
                    }
                    if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                        item.skim_file = Some(path.clone());
                    }
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_default();
                    tracing::info!("Skim transcode complete for {}: {}", filename, path.display());
                }
                AppEvent::SkimTranscodeFailed { media_id, error } => {
                    self.state.skim_transcoding.remove(&media_id);
                    if self.state.skim_transcoding.is_empty() {
                        self.state.skim_transcode_status = None;
                    }
                    let filename = self.state.media_pool.get(&media_id)
                        .map(|m| m.filename.clone())
                        .unwrap_or_default();
                    tracing::warn!("Skim transcode failed for {}: {}", filename, error);
                }
                AppEvent::BrowserSkimFrameReady { media_id, rgba, width, height } => {
                    self.state.browser_skim_pending = Some((media_id, rgba, width, height));
                }
                AppEvent::MediaFileChanged { path, deleted } => {
                    // Find media item(s) matching this path
                    if let Some(item) = self.state.media_pool.find_by_path(&path) {
                        let id = item.id;
                        if deleted || !path.exists() {
                            if let Some(item) = self.state.media_pool.get_mut(&id) {
                                item.offline = true;
                            }
                            tracing::warn!("Media file deleted externally: {:?}", path);
                        } else {
                            if let Some(item) = self.state.media_pool.get_mut(&id) {
                                item.needs_reprobe = true;
                            }
                            tracing::info!("Media file modified externally: {:?}", path);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn drain_proxy_progress(&mut self) {
        while let Ok((media_id, pct)) = self.proxy_progress_rx.try_recv() {
            self.state.proxy_progress.insert(media_id, pct);
        }
    }

    fn poll_render_queue(&mut self) {
        let events = self.state.render_queue.poll();
        for event in events {
            match event {
                klipp_export::RenderQueueEvent::Complete { id, output_path } => {
                    tracing::info!("Render job {} complete: {}", id, output_path.display());
                }
                klipp_export::RenderQueueEvent::Failed { id, error } => {
                    tracing::error!("Render job {} failed: {}", id, error);
                }
                klipp_export::RenderQueueEvent::Progress { .. } => {}
            }
        }
    }

    fn show_render_queue_window(&mut self, ctx: &egui::Context) {
        if !self.state.show_render_queue {
            return;
        }
        let mut open = true;
        egui::Window::new("Render Queue")
            .open(&mut open)
            .default_width(500.0)
            .resizable(true)
            .show(ctx, |ui| {
                if self.state.render_queue.jobs.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(80, 80, 90),
                        "No render jobs",
                    );
                    return;
                }

                let mut cancel_id = None;
                let dim = egui::Color32::from_rgb(110, 115, 125);
                let subtle = egui::Color32::from_rgb(140, 150, 165);

                for job in &self.state.render_queue.jobs {
                    // ── File name + status icon ──
                    ui.horizontal(|ui| {
                        let (icon, color) = match job.status {
                            klipp_export::RenderJobStatus::Queued => ("||", dim),
                            klipp_export::RenderJobStatus::Rendering => (">", egui::Color32::from_rgb(100, 180, 255)),
                            klipp_export::RenderJobStatus::Complete => ("OK", egui::Color32::from_rgb(100, 200, 120)),
                            klipp_export::RenderJobStatus::Failed => ("!!", egui::Color32::from_rgb(255, 100, 100)),
                            klipp_export::RenderJobStatus::Cancelled => ("--", dim),
                        };
                        ui.colored_label(color, icon);
                        ui.strong(&job.label);

                        // Codec badge
                        let codec_badge = job.settings.video_codec.label();
                        ui.colored_label(dim, format!(" \u{2022} {codec_badge}"));
                    });

                    // ── Progress bar (primary: percentage + ETA) ──
                    let elapsed_secs = job.started_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);

                    let bar_text = match job.status {
                        klipp_export::RenderJobStatus::Rendering => {
                            let pct = (job.progress * 100.0) as u32;
                            let remaining_str = if job.progress > 0.01 && elapsed_secs > 2 {
                                let total_est = elapsed_secs as f64 / job.progress as f64;
                                let remaining = (total_est - elapsed_secs as f64).max(0.0) as u64;
                                let m = remaining / 60;
                                let s = remaining % 60;
                                if m > 0 {
                                    format!(" \u{2014} {m}m {s:02}s remaining")
                                } else {
                                    format!(" \u{2014} {s}s remaining")
                                }
                            } else {
                                " \u{2014} calculating\u{2026}".to_string()
                            };
                            format!("{pct}%{remaining_str}")
                        }
                        klipp_export::RenderJobStatus::Complete => {
                            match (job.started_at, job.completed_at) {
                                (Some(start), Some(end)) => {
                                    let secs = end.duration_since(start).as_secs();
                                    let m = secs / 60;
                                    let s = secs % 60;
                                    let elapsed = if m > 0 { format!("{m}m {s:02}s") } else { format!("{s}s") };
                                    let avg_speed = if secs > 0 && job.total_frames > 0 && job.timeline_fps > 0.0 {
                                        let render_fps = job.total_frames as f64 / secs as f64;
                                        format!(" \u{2022} {:.1}\u{00D7} real-time", render_fps / job.timeline_fps)
                                    } else {
                                        String::new()
                                    };
                                    format!("Completed in {elapsed}{avg_speed}")
                                }
                                _ => "Completed".to_string(),
                            }
                        }
                        klipp_export::RenderJobStatus::Failed => "Export failed".to_string(),
                        klipp_export::RenderJobStatus::Cancelled => "Cancelled".to_string(),
                        klipp_export::RenderJobStatus::Queued => "Waiting in queue\u{2026}".to_string(),
                    };

                    // Draw custom progress bar
                    let bar_height = 22.0;
                    let (bar_rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), bar_height),
                        egui::Sense::hover(),
                    );
                    let painter = ui.painter_at(bar_rect);
                    painter.rect_filled(bar_rect, 4.0, egui::Color32::from_rgb(35, 37, 42));
                    let fill_width = bar_rect.width() * job.progress;
                    if fill_width > 0.5 {
                        let fill_rect = egui::Rect::from_min_size(bar_rect.min, egui::vec2(fill_width, bar_height));
                        let fill_color = match job.status {
                            klipp_export::RenderJobStatus::Rendering => egui::Color32::from_rgb(55, 130, 210),
                            klipp_export::RenderJobStatus::Complete => egui::Color32::from_rgb(50, 170, 85),
                            klipp_export::RenderJobStatus::Failed => egui::Color32::from_rgb(190, 55, 55),
                            _ => egui::Color32::from_rgb(80, 85, 95),
                        };
                        painter.rect_filled(fill_rect, 4.0, fill_color);
                    }
                    // White text with black shadow
                    let font = egui::FontId::proportional(12.0);
                    let text_pos = bar_rect.center();
                    painter.text(text_pos + egui::vec2(1.0, 1.0), egui::Align2::CENTER_CENTER, &bar_text, font.clone(), egui::Color32::from_rgb(0, 0, 0));
                    painter.text(text_pos, egui::Align2::CENTER_CENTER, &bar_text, font, egui::Color32::WHITE);

                    ui.add_space(2.0);

                    // ── Clip duration (separate line, clear context) ──
                    if job.status == klipp_export::RenderJobStatus::Rendering
                        && job.total_frames > 0
                        && job.timeline_fps > 0.0
                    {
                        let dur_secs = job.total_frames as f64 / job.timeline_fps;
                        let dm = dur_secs as u64 / 60;
                        let ds = dur_secs as u64 % 60;
                        ui.colored_label(subtle, format!("Clip duration: {dm}:{ds:02}"));
                    }

                    // ── Render stage (what's happening now) ──
                    if job.status == klipp_export::RenderJobStatus::Rendering && !job.render_stage.is_empty() {
                        ui.colored_label(subtle, &job.render_stage);
                    }

                    // ── Speed line (secondary — smaller, dimmer) ──
                    if job.status == klipp_export::RenderJobStatus::Rendering && elapsed_secs > 0 && job.frames_done > 0 {
                        let render_fps = job.frames_done as f64 / elapsed_secs as f64;
                        let speed = if job.timeline_fps > 0.0 { render_fps / job.timeline_fps } else { 0.0 };
                        ui.colored_label(dim, format!("{:.1}\u{00D7} real-time \u{2022} {:.0} fps", speed, render_fps));
                    }

                    // ── Hardware info (tertiary — smallest, dimmest) ──
                    let bg_dim = egui::Color32::from_rgb(90, 95, 105);
                    if !job.hw_info.is_empty() {
                        let hw_clean = job.hw_info
                            .replace("Decode: ", "")
                            .replace("Encode: ", "")
                            .replace(" | ", " \u{2022} ");
                        ui.colored_label(bg_dim, format!("GPU: {hw_clean}"));
                    }

                    // ── Error message ──
                    if let Some(err) = &job.error {
                        ui.colored_label(egui::Color32::from_rgb(255, 100, 100), err);
                    }

                    ui.add_space(4.0);

                    // ── Action buttons + collapsible details ──
                    ui.horizontal(|ui| {
                        match job.status {
                            klipp_export::RenderJobStatus::Queued
                            | klipp_export::RenderJobStatus::Rendering => {
                                let btn = egui::Button::new("Stop Render")
                                    .fill(egui::Color32::from_rgb(160, 50, 50));
                                if ui.add(btn).clicked() {
                                    cancel_id = Some(job.id);
                                }
                                ui.add_space(8.0);
                            }
                            _ => {}
                        }

                        // Collapsible details (default: hidden)
                        // Use a stable ID not dependent on ui.id() (which changes inside horizontal)
                        let details_id = egui::Id::new("render_details").with(job.id);
                        let mut show_details = ui.data_mut(|d| *d.get_temp_mut_or_default::<bool>(details_id));
                        let details_text = if show_details { "v Details" } else { "> Details" };
                        let details_btn = ui.add(
                            egui::Button::new(egui::RichText::new(details_text).color(bg_dim).size(12.0))
                                .frame(false)
                        );
                        if details_btn.clicked() {
                            show_details = !show_details;
                        }
                        ui.data_mut(|d| d.insert_temp(details_id, show_details));
                    });

                    // ── Details panel (collapsed by default) ──
                    let details_id = egui::Id::new("render_details").with(job.id);
                    let show_details = ui.data_mut(|d| *d.get_temp_mut_or_default::<bool>(details_id));
                    if show_details {
                        ui.add_space(2.0);
                        ui.indent("render_details", |ui| {
                            let res = job.settings.resolution.as_ref()
                                .map(|r| format!("{}×{}", r.width, r.height))
                                .unwrap_or_else(|| "Sequence resolution".into());
                            let crf_str = if job.settings.video_codec.uses_crf() {
                                format!("CRF {}", job.settings.crf)
                            } else {
                                "N/A".into()
                            };
                            let audio_str = job.settings.audio_codec.label();
                            let bitrate_str = job.settings.audio_bitrate_kbps
                                .map(|b| format!("{b} kbps"))
                                .unwrap_or_else(|| "default".into());

                            ui.colored_label(bg_dim, format!("Codec: {}", job.settings.video_codec.label()));
                            ui.colored_label(bg_dim, format!("Resolution: {}", res));
                            ui.colored_label(bg_dim, format!("Quality: {}", crf_str));
                            ui.colored_label(bg_dim, format!("Audio: {} {}", audio_str, bitrate_str));
                            ui.colored_label(bg_dim, format!("Frames: {}", job.total_frames));
                        });
                    }

                    ui.add_space(4.0);
                    ui.separator();
                    ui.add_space(4.0);
                }

                if let Some(id) = cancel_id {
                    self.state.render_queue.cancel_job(id);
                }

                ui.add_space(4.0);
                if ui.button("Clear Completed").clicked() {
                    self.state.render_queue.clear_completed();
                }
            });
        if !open {
            self.state.show_render_queue = false;
        }
    }

    fn show_export_dialog(&mut self, ctx: &egui::Context) -> Vec<AppCommand> {
        // Build combined preset list for the combo box
        let all_presets: Vec<ExportPreset> = builtin_presets()
            .into_iter()
            .chain(self.prefs.export.custom_presets.iter().cloned())
            .collect();
        let mut cmds = Vec::new();

        if !self.state.show_export_dialog {
            return cmds;
        }

        let mut open = true;
        egui::Window::new("Export Sequence")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                // Preset selector
                ui.horizontal(|ui| {
                    ui.label("Preset:");
                    let selected_label = self.export_dialog.selected_preset_id.as_ref()
                        .and_then(|id| all_presets.iter().find(|p| &p.id == id))
                        .map(|p| p.name.as_str())
                        .unwrap_or("Custom");
                    egui::ComboBox::from_id_salt("export_preset")
                        .selected_text(selected_label)
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(self.export_dialog.selected_preset_id.is_none(), "Custom").clicked() {
                                self.export_dialog.selected_preset_id = None;
                            }
                            ui.separator();
                            for preset in &all_presets {
                                let label = if preset.built_in {
                                    preset.name.clone()
                                } else {
                                    format!("⚙ {}", preset.name)
                                };
                                let is_selected = self.export_dialog.selected_preset_id.as_ref() == Some(&preset.id);
                                if ui.selectable_label(is_selected, &label).clicked() {
                                    self.export_dialog.selected_preset_id = Some(preset.id.clone());
                                    apply_preset_to_dialog(&mut self.export_dialog, &preset.settings);
                                }
                            }
                        });
                    if ui.button("Save as Preset").clicked() {
                        self.export_dialog.show_save_preset = true;
                        self.export_dialog.save_preset_name.clear();
                    }
                });
                ui.add_space(4.0);

                egui::Grid::new("export_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Output file:");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.export_dialog.output_path)
                                .desired_width(300.0));
                            if ui.button("...").clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_title("Export to...")
                                    .save_file()
                                {
                                    self.export_dialog.output_path = path.to_string_lossy().to_string();
                                }
                            }
                        });
                        ui.end_row();

                        // Auto-update file extension when format changes
                        let old_ext = self.export_dialog.format.extension();

                        ui.label("Format:");
                        let prev_format = self.export_dialog.format;
                        egui::ComboBox::from_id_salt("export_format")
                            .selected_text(self.export_dialog.format.label())
                            .show_ui(ui, |ui| {
                                // Only show MOV for ProRes/DNxHD codecs; show all for H.264/H.265
                                let codec_requires_mov = self.export_dialog.video_codec.requires_mov();
                                if codec_requires_mov {
                                    ui.selectable_value(&mut self.export_dialog.format, ExportFormat::Mov, "MOV");
                                } else {
                                    ui.selectable_value(&mut self.export_dialog.format, ExportFormat::Mp4, "MP4");
                                    ui.selectable_value(&mut self.export_dialog.format, ExportFormat::Mkv, "MKV");
                                    ui.selectable_value(&mut self.export_dialog.format, ExportFormat::Mov, "MOV");
                                }
                            });
                        // If format changed away from MOV, reset codec if it requires MOV
                        if self.export_dialog.format != prev_format
                            && self.export_dialog.format != ExportFormat::Mov
                            && self.export_dialog.video_codec.requires_mov()
                        {
                            self.export_dialog.video_codec = ExportVideoCodec::H264;
                        }
                        // Update file extension when format changes
                        if self.export_dialog.format != prev_format {
                            let new_ext = self.export_dialog.format.extension();
                            if self.export_dialog.output_path.ends_with(&format!(".{old_ext}")) {
                                let base = &self.export_dialog.output_path[..self.export_dialog.output_path.len() - old_ext.len()];
                                self.export_dialog.output_path = format!("{base}{new_ext}");
                            }
                        }
                        ui.end_row();

                        ui.label("Video codec:");
                        let prev_codec = self.export_dialog.video_codec;
                        egui::ComboBox::from_id_salt("export_vcodec")
                            .selected_text(self.export_dialog.video_codec.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::H264, "\u{26A1} H.264 — GPU accelerated");
                                ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::H265, "\u{26A1} H.265 — GPU accelerated, best quality/size");
                                if self.export_dialog.format == ExportFormat::Mov {
                                    ui.separator();
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::ProRes422, "ProRes 422 — CPU only, NLE interchange");
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::ProRes422HQ, "ProRes 422 HQ — CPU only, high quality");
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::ProRes4444, "ProRes 4444 — CPU only, alpha/mastering");
                                    ui.separator();
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::DNxHD115, "DNxHD 115 — CPU only, Avid interchange");
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::DNxHD145, "DNxHD 145 — CPU only, Avid high bitrate");
                                }
                            });
                        ui.end_row();

                        // Codec hint
                        ui.label("");
                        ui.colored_label(
                            egui::Color32::from_rgb(130, 150, 170),
                            self.export_dialog.video_codec.hint(),
                        );
                        ui.end_row();

                        // Auto-switch to MOV when ProRes/DNxHD codec is selected
                        if self.export_dialog.video_codec != prev_codec
                            && self.export_dialog.video_codec.requires_mov()
                        {
                            let old_fmt_ext = self.export_dialog.format.extension();
                            self.export_dialog.format = ExportFormat::Mov;
                            let new_fmt_ext = "mov";
                            if self.export_dialog.output_path.ends_with(&format!(".{old_fmt_ext}")) {
                                let base = &self.export_dialog.output_path[..self.export_dialog.output_path.len() - old_fmt_ext.len()];
                                self.export_dialog.output_path = format!("{base}{new_fmt_ext}");
                            }
                        }
                        ui.end_row();

                        ui.label("Quality (CRF):");
                        ui.add(egui::Slider::new(&mut self.export_dialog.crf, 0..=51));
                        ui.end_row();

                        ui.label("Audio codec:");
                        egui::ComboBox::from_id_salt("export_acodec")
                            .selected_text(self.export_dialog.audio_codec.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_dialog.audio_codec, ExportAudioCodec::Aac, "AAC");
                                ui.selectable_value(&mut self.export_dialog.audio_codec, ExportAudioCodec::Mp3, "MP3");
                                ui.selectable_value(&mut self.export_dialog.audio_codec, ExportAudioCodec::Flac, "FLAC");
                                ui.selectable_value(&mut self.export_dialog.audio_codec, ExportAudioCodec::Copy, "Copy Source");
                            });
                        ui.end_row();

                        ui.label("Audio bitrate:");
                        egui::ComboBox::from_id_salt("export_abitrate")
                            .selected_text(format!("{} kbps", self.export_dialog.audio_bitrate))
                            .show_ui(ui, |ui| {
                                for rate in [128, 192, 256, 320] {
                                    ui.selectable_value(&mut self.export_dialog.audio_bitrate, rate, format!("{rate} kbps"));
                                }
                            });
                        ui.end_row();
                    });

                // Export range checkbox
                if let (Some(in_pt), Some(out_pt)) = (self.state.sequences[self.state.active_sequence_idx].in_point, self.state.sequences[self.state.active_sequence_idx].out_point) {
                    let in_str = in_pt.to_timecode_string();
                    let out_str = out_pt.to_timecode_string();
                    ui.checkbox(
                        &mut self.export_dialog.marked_range,
                        format!("Export marked range only (In: {} \u{2192} Out: {})", in_str, out_str),
                    );
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.state.show_export_dialog = false;
                    }
                    if ui.button("Add to Queue").clicked() {
                        let path = PathBuf::from(&self.export_dialog.output_path);
                        let settings = ExportSettings {
                            output_path: path.clone(),
                            format: self.export_dialog.format,
                            video_codec: self.export_dialog.video_codec,
                            resolution: None,
                            fps: None,
                            crf: self.export_dialog.crf,
                            preset: self.export_dialog.preset,
                            audio_codec: self.export_dialog.audio_codec,
                            audio_bitrate_kbps: Some(self.export_dialog.audio_bitrate),
                        };
                        let snapshot = klipp_export::RenderSnapshot::from_sequence(
                            &self.state.sequences[self.state.active_sequence_idx],
                            &self.state.media_pool,
                            &settings,
                            self.state.global_lut_path.clone(),
                            self.export_dialog.marked_range,
                        );
                        let label = path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Render".into());
                        match snapshot {
                            Ok(snap) => {
                                self.state.render_queue.add_job(path, settings, label, snap);
                            }
                            Err(e) => {
                                tracing::error!("Failed to create render snapshot: {e}");
                            }
                        }
                        self.state.show_export_dialog = false;
                        self.state.show_render_queue = true;
                        self.prefs.export.last_preset_id = self.export_dialog.selected_preset_id.clone();
                        self.prefs_dirty = true;
                    }
                    if ui.button("Export").clicked() {
                        let path = PathBuf::from(&self.export_dialog.output_path);
                        if path.exists() {
                            self.export_dialog.confirm_overwrite = true;
                        } else {
                            self.state.show_export_dialog = false;
                            self.prefs.export.last_preset_id = self.export_dialog.selected_preset_id.clone();
                            self.prefs_dirty = true;
                            cmds.push(AppCommand::ExportSequence {
                                settings: ExportSettings {
                                    output_path: path,
                                    format: self.export_dialog.format,
                                    video_codec: self.export_dialog.video_codec,
                                    resolution: None,
                                    fps: None,
                                    crf: self.export_dialog.crf,
                                    preset: self.export_dialog.preset,
                                    audio_codec: self.export_dialog.audio_codec,
                                    audio_bitrate_kbps: Some(self.export_dialog.audio_bitrate),
                                },
                            });
                        }
                    }
                });
            });

        if !open {
            self.state.show_export_dialog = false;
            self.export_dialog.confirm_overwrite = false;
        }

        // Overwrite confirmation dialog
        if self.export_dialog.confirm_overwrite {
            egui::Window::new("Overwrite File?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "\"{}\" already exists. Overwrite?",
                        self.export_dialog.output_path
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.export_dialog.confirm_overwrite = false;
                        }
                        if ui.button("Overwrite").clicked() {
                            self.export_dialog.confirm_overwrite = false;
                            self.state.show_export_dialog = false;
                            self.prefs.export.last_preset_id = self.export_dialog.selected_preset_id.clone();
                            self.prefs_dirty = true;
                            cmds.push(AppCommand::ExportSequence {
                                settings: ExportSettings {
                                    output_path: PathBuf::from(&self.export_dialog.output_path),
                                    format: self.export_dialog.format,
                                    video_codec: self.export_dialog.video_codec,
                                    resolution: None,
                                    fps: None,
                                    crf: self.export_dialog.crf,
                                    preset: self.export_dialog.preset,
                                    audio_codec: self.export_dialog.audio_codec,
                                    audio_bitrate_kbps: Some(self.export_dialog.audio_bitrate),
                                },
                            });
                        }
                    });
                });
        }

        // Save as Preset dialog
        if self.export_dialog.show_save_preset {
            egui::Window::new("Save as Preset")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Preset name:");
                        ui.text_edit_singleline(&mut self.export_dialog.save_preset_name);
                    });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.export_dialog.show_save_preset = false;
                        }
                        let name_valid = !self.export_dialog.save_preset_name.trim().is_empty();
                        if ui.add_enabled(name_valid, egui::Button::new("Save")).clicked() {
                            let id = format!("custom-{}", uuid::Uuid::new_v4());
                            let preset = ExportPreset {
                                id: id.clone(),
                                name: self.export_dialog.save_preset_name.trim().to_string(),
                                built_in: false,
                                settings: ExportPresetSettings {
                                    format: self.export_dialog.format,
                                    video_codec: self.export_dialog.video_codec,
                                    resolution: None,
                                    fps: None,
                                    crf: self.export_dialog.crf,
                                    preset: self.export_dialog.preset,
                                    audio_codec: self.export_dialog.audio_codec,
                                    audio_bitrate_kbps: Some(self.export_dialog.audio_bitrate),
                                },
                            };
                            self.prefs.export.custom_presets.push(preset);
                            self.export_dialog.selected_preset_id = Some(id);
                            self.export_dialog.show_save_preset = false;
                            self.prefs_dirty = true;
                        }
                    });
                });
        }

        cmds
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
                        let tex = ctx.load_texture(
                            format!("thumb-{}", media_id),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.state.thumbnail_textures.insert(media_id, tex);
                    }
                }
            }

            // Upload filmstrip textures — rebuild when new frames arrive from background thread
            let existing_count = self.state.filmstrip_textures.get(&media_id).map(|t| t.len()).unwrap_or(0);
            let cache_count = self.state.thumbnail_cache.filmstrip(&media_id).map(|f| f.len()).unwrap_or(0);
            if cache_count > existing_count {
                if let Some(frames) = self.state.thumbnail_cache.filmstrip(&media_id) {
                    if !frames.is_empty() {
                        let textures: Vec<(f64, egui::TextureHandle)> = frames
                            .iter()
                            .enumerate()
                            .filter_map(|(i, frame)| {
                                let w = frame.width as usize;
                                let h = frame.height as usize;
                                if frame.rgba.len() != w * h * 4 {
                                    return None;
                                }
                                let pixels: Vec<egui::Color32> = frame
                                    .rgba
                                    .chunks_exact(4)
                                    .map(|p| {
                                        egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])
                                    })
                                    .collect();
                                let image = egui::ColorImage {
                                    size: [w, h],
                                    pixels,
                                };
                                let tex = ctx.load_texture(
                                    format!("filmstrip-{}-{}", media_id, i),
                                    image,
                                    egui::TextureOptions::LINEAR,
                                );
                                Some((frame.time_secs, tex))
                            })
                            .collect();
                        self.state.filmstrip_textures.insert(media_id, textures);
                    }
                }
            }
        }
    }
}

fn find_file_recursive(dir: &std::path::Path, filename: &str) -> Option<PathBuf> {
    for entry in walkdir::WalkDir::new(dir).max_depth(5).into_iter().filter_map(|e| e.ok()) {
        if entry.file_name().to_string_lossy() == filename {
            return Some(entry.path().to_path_buf());
        }
    }
    None
}

/// Count how many clips across all sequences reference this media_id.
fn media_usage_count(media_id: MediaId, sequences: &[Sequence]) -> usize {
    sequences.iter()
        .flat_map(|seq| seq.tracks.iter())
        .flat_map(|t| &t.clips)
        .filter(|c| c.media_id() == Some(media_id))
        .count()
}

/// Compute a content fingerprint: FNV-1a hash of first 64KB + file_size.
/// Returns None if the file can't be read.
fn compute_media_fingerprint(path: &Path) -> Option<u64> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 65536];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    let file_size = std::fs::metadata(path).ok()?.len();
    let mut hash: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
    for &byte in buf.iter().chain(file_size.to_le_bytes().iter()) {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3); // FNV-1a prime
    }
    Some(hash)
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

    // Only image extensions
    let ext_lower = ext.to_lowercase();
    if !matches!(ext_lower.as_str(), "png" | "jpg" | "jpeg" | "tiff" | "tif" | "bmp" | "webp" | "exr") {
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
fn autosave_path_for(project_path: &Path) -> PathBuf {
    let stem = project_path.file_stem().unwrap_or_default().to_string_lossy();
    project_path.with_file_name(format!("{stem}_autosave.klipp"))
}

/// Generate an ISO 8601 UTC timestamp string.
pub fn iso8601_now() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    // Simple UTC date-time without external crate
    let days = secs / 86400;
    let time_secs = secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;
    // Days since 1970-01-01
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut year = 1970u64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap(year);
    let month_days: [u64; 12] = [
        31,
        if leap { 29 } else { 28 },
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
    ];
    let mut month = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
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
) -> Option<PathBuf> {
    let fb = frame_buffer.lock().ok()?;
    if fb.data.is_empty() || fb.width == 0 || fb.height == 0 {
        return None;
    }

    // Downsample to 320×180 using nearest neighbor
    let dst_w: u32 = 320;
    let dst_h: u32 = 180;
    let src_w = fb.width;
    let src_h = fb.height;
    let mut rgb_data = vec![0u8; (dst_w * dst_h * 3) as usize];

    for dy in 0..dst_h {
        for dx in 0..dst_w {
            let sx = (dx as u64 * src_w as u64 / dst_w as u64) as u32;
            let sy = (dy as u64 * src_h as u64 / dst_h as u64) as u32;
            let src_idx = ((sy * src_w + sx) * 4) as usize;
            let dst_idx = ((dy * dst_w + dx) * 3) as usize;
            if src_idx + 2 < fb.data.len() && dst_idx + 2 < rgb_data.len() {
                rgb_data[dst_idx] = fb.data[src_idx];
                rgb_data[dst_idx + 1] = fb.data[src_idx + 1];
                rgb_data[dst_idx + 2] = fb.data[src_idx + 2];
            }
        }
    }

    // Determine output path
    let cache_dir = dirs::cache_dir()?.join("klipp").join("thumbnails");
    std::fs::create_dir_all(&cache_dir).ok()?;
    let stem = project_path.file_stem()?.to_string_lossy();
    let out_path = cache_dir.join(format!("{stem}.png"));

    // Write PNG
    let file = std::fs::File::create(&out_path).ok()?;
    let w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, dst_w, dst_h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(&rgb_data).ok()?;

    Some(out_path)
}

fn format_size_brief(bytes: u64) -> String {
    if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
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
/// Animation format: `"type_duration"` or `"type1+type2_duration"` where duration is in seconds.
/// Examples: `"fade_in_0.5"`, `"slide_left_1.0"`, `"fade_in+slide_left_0.8"`, `"fade_in_out_0.5"`
fn apply_title_animation(clip: &mut klipp_timeline::Clip, anim_str: &str, clip_dur: f64) {
    use klipp_core::{Interpolation, Keyframe};

    // Helper to create a bezier keyframe with ease tangents
    let bezier_kf = |t: f64, v: f32, t_in: (f64, f32), t_out: (f64, f32)| -> Keyframe {
        Keyframe {
            time_secs: t,
            value: v,
            interpolation: Interpolation::Bezier,
            tangent_in: t_in,
            tangent_out: t_out,
        }
    };

    // Parse compound animations (e.g. "fade_in+slide_left_0.8")
    // Split on '+' for combinations, last segment contains the duration suffix
    let parts: Vec<&str> = anim_str.split('+').collect();

    for part in &parts {
        // Extract the animation type and duration from the last underscore-separated number
        let (anim_type, dur) = parse_anim_part(part);
        let d = dur as f64;
        let span = d;

        match anim_type {
            "fade_in" => {
                clip.opacity_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(d, 1.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "fade_out" => {
                let start = (clip_dur - d).max(0.0);
                clip.opacity_keyframes.push(bezier_kf(start, 1.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(clip_dur, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "fade_in_out" => {
                let out_start = (clip_dur - d).max(d);
                clip.opacity_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(d, 1.0, (-0.42 * span, 0.0), (0.42 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(out_start, 1.0, (-0.42 * span, 0.0), (0.42 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(clip_dur, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "slide_left" => {
                clip.transform_pos_x_keyframes.push(bezier_kf(0.0, -960.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_pos_x_keyframes.push(bezier_kf(d, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "slide_right" => {
                clip.transform_pos_x_keyframes.push(bezier_kf(0.0, 960.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_pos_x_keyframes.push(bezier_kf(d, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "slide_up" => {
                clip.transform_pos_y_keyframes.push(bezier_kf(0.0, 540.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_pos_y_keyframes.push(bezier_kf(d, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "slide_down" => {
                clip.transform_pos_y_keyframes.push(bezier_kf(0.0, -540.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_pos_y_keyframes.push(bezier_kf(d, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "slide_out_left" => {
                let start = (clip_dur - d).max(0.0);
                clip.transform_pos_x_keyframes.push(bezier_kf(start, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_pos_x_keyframes.push(bezier_kf(clip_dur, -960.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "slide_out_right" => {
                let start = (clip_dur - d).max(0.0);
                clip.transform_pos_x_keyframes.push(bezier_kf(start, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_pos_x_keyframes.push(bezier_kf(clip_dur, 960.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "slide_out_up" => {
                let start = (clip_dur - d).max(0.0);
                clip.transform_pos_y_keyframes.push(bezier_kf(start, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_pos_y_keyframes.push(bezier_kf(clip_dur, -540.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "slide_out_down" => {
                let start = (clip_dur - d).max(0.0);
                clip.transform_pos_y_keyframes.push(bezier_kf(start, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_pos_y_keyframes.push(bezier_kf(clip_dur, 540.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "pop_in" => {
                let t_over = d * 0.6;
                let t_settle = d;
                let span1 = t_over;
                let span2 = t_settle - t_over;
                clip.transform_scale_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.42 * span1, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(t_over, 1.1, (-0.42 * span1, 0.0), (0.42 * span2, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(t_settle, 1.0, (-0.42 * span2, 0.0), (0.0, 0.0)));
            }
            "pop_out" => {
                let start = (clip_dur - d).max(0.0);
                let t_over = start + d * 0.4;
                let span1 = d * 0.4;
                let span2 = d * 0.6;
                clip.transform_scale_keyframes.push(bezier_kf(start, 1.0, (0.0, 0.0), (0.42 * span1, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(t_over, 1.1, (-0.42 * span1, 0.0), (0.42 * span2, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(clip_dur, 0.0, (-0.42 * span2, 0.0), (0.0, 0.0)));
            }
            "grow_in" => {
                clip.transform_scale_keyframes.push(bezier_kf(0.0, 0.5, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(d, 1.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "shrink_out" => {
                let start = (clip_dur - d).max(0.0);
                clip.transform_scale_keyframes.push(bezier_kf(start, 1.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(clip_dur, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "zoom_in" => {
                clip.transform_scale_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(d, 1.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "zoom_out" => {
                let start = (clip_dur - d).max(0.0);
                clip.transform_scale_keyframes.push(bezier_kf(start, 1.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(clip_dur, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            // ── Advanced animations ──
            "spin_in" => {
                clip.transform_rotation_keyframes.push(bezier_kf(0.0, 360.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_rotation_keyframes.push(bezier_kf(d, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(d, 1.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "spin_out" => {
                let start = (clip_dur - d).max(0.0);
                clip.transform_rotation_keyframes.push(bezier_kf(start, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_rotation_keyframes.push(bezier_kf(clip_dur, 360.0, (-0.42 * span, 0.0), (0.0, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(start, 1.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(clip_dur, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "swirl_in" => {
                clip.transform_rotation_keyframes.push(bezier_kf(0.0, 720.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_rotation_keyframes.push(bezier_kf(d, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(d, 1.0, (-0.42 * span, 0.0), (0.0, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(d * 0.3, 1.0, (-0.42 * span * 0.3, 0.0), (0.0, 0.0)));
            }
            "bounce_in" => {
                let s = |f: f64| f * d;
                clip.transform_scale_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.1 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(s(0.4), 1.2, (-0.1 * d, 0.0), (0.05 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(s(0.6), 0.9, (-0.05 * d, 0.0), (0.05 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(s(0.8), 1.05, (-0.05 * d, 0.0), (0.05 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(d, 1.0, (-0.05 * d, 0.0), (0.0, 0.0)));
            }
            "elastic" => {
                let s = |f: f64| f * d;
                clip.transform_scale_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.08 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(s(0.3), 1.3, (-0.08 * d, 0.0), (0.04 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(s(0.5), 0.85, (-0.04 * d, 0.0), (0.04 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(s(0.7), 1.1, (-0.04 * d, 0.0), (0.04 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(s(0.85), 0.95, (-0.04 * d, 0.0), (0.04 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(d, 1.0, (-0.04 * d, 0.0), (0.0, 0.0)));
            }
            "shake" => {
                let shake_dur = d.min(clip_dur);
                let n = 10;
                for i in 0..=n {
                    let t = (i as f64 / n as f64) * shake_dur;
                    let amp = 20.0 * (1.0 - i as f32 / n as f32);
                    let val = if i == 0 || i == n { 0.0 } else if i % 2 == 0 { amp } else { -amp };
                    clip.transform_pos_x_keyframes.push(Keyframe {
                        time_secs: t, value: val, interpolation: Interpolation::Linear,
                        tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    });
                }
            }
            "swing_in" => {
                let s = |f: f64| f * d;
                clip.transform_rotation_keyframes.push(bezier_kf(0.0, -30.0, (0.0, 0.0), (0.1 * d, 0.0)));
                clip.transform_rotation_keyframes.push(bezier_kf(s(0.4), 10.0, (-0.1 * d, 0.0), (0.08 * d, 0.0)));
                clip.transform_rotation_keyframes.push(bezier_kf(s(0.7), -5.0, (-0.08 * d, 0.0), (0.06 * d, 0.0)));
                clip.transform_rotation_keyframes.push(bezier_kf(d, 0.0, (-0.06 * d, 0.0), (0.0, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.1 * d, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(s(0.2), 1.0, (-0.1 * d, 0.0), (0.0, 0.0)));
            }
            "explode_out" => {
                let start = (clip_dur - d).max(0.0);
                clip.transform_scale_keyframes.push(bezier_kf(start, 1.0, (0.0, 0.0), (0.2 * span, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(clip_dur, 3.0, (-0.2 * span, 0.0), (0.0, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(start, 1.0, (0.0, 0.0), (0.2 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(clip_dur, 0.0, (-0.2 * span, 0.0), (0.0, 0.0)));
            }
            "implode_in" => {
                clip.transform_scale_keyframes.push(bezier_kf(0.0, 3.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(d, 1.0, (-0.42 * span, 0.0), (0.0, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(d, 1.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "pulse" => {
                let s = |f: f64| f * d;
                clip.transform_scale_keyframes.push(bezier_kf(0.0, 1.0, (0.0, 0.0), (0.1 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(s(0.5), 1.15, (-0.1 * d, 0.0), (0.1 * d, 0.0)));
                clip.transform_scale_keyframes.push(bezier_kf(d, 1.0, (-0.1 * d, 0.0), (0.0, 0.0)));
            }
            "rotate_in" => {
                clip.transform_rotation_keyframes.push(bezier_kf(0.0, -90.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_rotation_keyframes.push(bezier_kf(d, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(0.0, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(d, 1.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "rotate_out" => {
                let start = (clip_dur - d).max(0.0);
                clip.transform_rotation_keyframes.push(bezier_kf(start, 0.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.transform_rotation_keyframes.push(bezier_kf(clip_dur, 90.0, (-0.42 * span, 0.0), (0.0, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(start, 1.0, (0.0, 0.0), (0.42 * span, 0.0)));
                clip.opacity_keyframes.push(bezier_kf(clip_dur, 0.0, (-0.42 * span, 0.0), (0.0, 0.0)));
            }
            "glitch" => {
                // Rapid opacity flicker
                let n = 8;
                let flicker_dur = d.min(clip_dur);
                for i in 0..=n {
                    let t = (i as f64 / n as f64) * flicker_dur;
                    let val = if i == n { 1.0 } else if i % 2 == 0 { 0.0 } else { 1.0 };
                    clip.opacity_keyframes.push(Keyframe {
                        time_secs: t, value: val, interpolation: Interpolation::Step,
                        tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                    });
                }
            }
            "float_up" => {
                // Slow continuous upward drift over entire clip
                clip.transform_pos_y_keyframes.push(bezier_kf(0.0, 50.0, (0.0, 0.0), (0.33 * clip_dur, 0.0)));
                clip.transform_pos_y_keyframes.push(bezier_kf(clip_dur, -50.0, (-0.33 * clip_dur, 0.0), (0.0, 0.0)));
            }
            // ── Continuous motion via modifiers ──
            "scroll_up" => {
                use klipp_core::modifier::*;
                clip.transform_pos_y_keyframes.push(Keyframe {
                    time_secs: 0.0, value: 600.0, interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                });
                clip.param_modifiers.entry(ParamId::PositionY).or_default().modifiers.push(
                    Modifier::Throw { velocity: -100.0, acceleration: 0.0, blend: ModifierBlend::Add, enabled: true }
                );
            }
            "scroll_down" => {
                use klipp_core::modifier::*;
                clip.transform_pos_y_keyframes.push(Keyframe {
                    time_secs: 0.0, value: -600.0, interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                });
                clip.param_modifiers.entry(ParamId::PositionY).or_default().modifiers.push(
                    Modifier::Throw { velocity: 100.0, acceleration: 0.0, blend: ModifierBlend::Add, enabled: true }
                );
            }
            "ticker_left" => {
                use klipp_core::modifier::*;
                clip.transform_pos_x_keyframes.push(Keyframe {
                    time_secs: 0.0, value: 1000.0, interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                });
                clip.param_modifiers.entry(ParamId::PositionX).or_default().modifiers.push(
                    Modifier::Throw { velocity: -200.0, acceleration: 0.0, blend: ModifierBlend::Add, enabled: true }
                );
            }
            "ticker_right" => {
                use klipp_core::modifier::*;
                clip.transform_pos_x_keyframes.push(Keyframe {
                    time_secs: 0.0, value: -1000.0, interpolation: Interpolation::Linear,
                    tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
                });
                clip.param_modifiers.entry(ParamId::PositionX).or_default().modifiers.push(
                    Modifier::Throw { velocity: 200.0, acceleration: 0.0, blend: ModifierBlend::Add, enabled: true }
                );
            }
            "spinning" => {
                use klipp_core::modifier::*;
                clip.param_modifiers.entry(ParamId::Rotation).or_default().modifiers.push(
                    Modifier::Throw { velocity: 90.0, acceleration: 0.0, blend: ModifierBlend::Add, enabled: true }
                );
            }
            "pulsing" => {
                use klipp_core::modifier::*;
                clip.param_modifiers.entry(ParamId::Scale).or_default().modifiers.push(
                    Modifier::Oscillate { amplitude: 0.1, frequency: 1.5, phase: 0.0, waveform: Waveform::Sine, blend: ModifierBlend::Add, enabled: true }
                );
            }
            "breathing" => {
                use klipp_core::modifier::*;
                clip.param_modifiers.entry(ParamId::Opacity).or_default().modifiers.push(
                    Modifier::Oscillate { amplitude: 0.3, frequency: 0.5, phase: 0.0, waveform: Waveform::Sine, blend: ModifierBlend::Add, enabled: true }
                );
            }
            "floating" => {
                use klipp_core::modifier::*;
                clip.param_modifiers.entry(ParamId::PositionY).or_default().modifiers.push(
                    Modifier::Wiggle { amplitude: 15.0, frequency: 0.5, blend: ModifierBlend::Add, enabled: true }
                );
            }
            _ => {}
        }
    }

    // Sort keyframes by time after all animations applied
    clip.opacity_keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    clip.transform_pos_x_keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    clip.transform_pos_y_keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    clip.transform_scale_keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    clip.transform_rotation_keyframes.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
}

/// Parse an animation part string like "fade_in_0.5" into ("fade_in", 0.5f32).
fn parse_anim_part(s: &str) -> (&str, f32) {
    // Find the last '_' followed by a number
    if let Some(pos) = s.rfind('_') {
        let (name, dur_str) = s.split_at(pos);
        let dur_str = &dur_str[1..]; // skip the '_'
        if let Ok(dur) = dur_str.parse::<f32>() {
            return (name, dur);
        }
    }
    (s, 0.5) // default 0.5s
}
