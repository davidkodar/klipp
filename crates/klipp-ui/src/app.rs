use crate::history::EditCommand;
use crate::layout_state::LayoutState;
use crate::panels::inspector::InspectorPanel;
use crate::panels::markers::MarkersPanel;
use crate::browser::BrowserPanel;
use crate::inspector::NewInspectorPanel;
use crate::panels::media_panel::MediaPanel;
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
    ExportPresetSettings, ExportSettings, ExportVideoCodec, MarkerId, MediaId,
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
    #[allow(dead_code)]
    inspector: InspectorPanel, // kept for potential fallback; old code fully superseded by new_inspector
    markers: MarkersPanel,
    #[allow(dead_code)]
    media_panel: MediaPanel, // kept for potential fallback; old code fully superseded by browser
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
}

impl KlippApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cmd_tx: Sender<AppCommand>,
        event_rx: Receiver<AppEvent>,
        frame_buffer: SharedFrameBuffer,
        sequence: Arc<RwLock<Sequence>>,
        media_pool_shared: Arc<RwLock<klipp_media::MediaPool>>,
    ) -> Self {
        let (thumb_tx, thumb_rx) = crossbeam_channel::unbounded();
        let (filmstrip_tx, filmstrip_rx) = crossbeam_channel::unbounded();
        let (audio_conform_tx, audio_conform_rx) = crossbeam_channel::unbounded();
        let (proxy_event_tx, proxy_event_rx) = crossbeam_channel::unbounded();
        let (proxy_progress_tx, proxy_progress_rx) = crossbeam_channel::unbounded();
        let theme = KlippTheme::default();
        theme.apply_to_egui(&cc.egui_ctx);
        theme.apply_fonts(&cc.egui_ctx);
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
            inspector: InspectorPanel::default(),
            markers: MarkersPanel::default(),
            media_panel: MediaPanel::default(),
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
            new_inspector: NewInspectorPanel,
            new_timeline: crate::timeline::NewTimelinePanel,
        };
        // Discover OFX plugins at startup
        let mut ofx_host = klipp_plugin::OfxHost::new();
        let count = ofx_host.discover_plugins();
        app.state.available_plugins = ofx_host.plugin_infos();
        tracing::info!("OFX: discovered {} plugin(s)", count);
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
                }
                AppEvent::PlaybackPaused => {
                    self.state.playback = PlaybackState::Paused;
                }
                AppEvent::PlaybackStopped => {
                    self.state.playback = PlaybackState::Stopped;
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
                } => {
                    let filename = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());
                    let _is_audio_only = video_codec.is_empty();

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
                        offline: false,
                        proxy_path: None,
                        proxy_ready: false,
                        default_source_in: None,
                        default_source_out: None,
                        rating: 0,
                        tags: Vec::new(),
                        user_notes: String::new(),
                        bin: None,
                        waveform_data: None,
                        audio_cache: None,
                    };

                    self.state.media_pool.add(media_item);
                    self.sync_media_pool_to_engine();
                    self.state.fps = fps;
                    self.state.playback = PlaybackState::Stopped;

                    // Clear pending path — media goes to pool only.
                    // User adds to timeline via double-click or drag-and-drop.
                    if self.state.pending_media_path.is_some() {
                        self.state.pending_media_path = None;
                    }

                    // Start thumbnail + filmstrip generation + audio conforming
                    self.spawn_thumbnail(media_id, path.clone(), duration);
                    self.spawn_filmstrip(media_id, path.clone(), duration);
                    if has_audio {
                        self.spawn_audio_conform(media_id, path);
                    }
                    self.state.filmstrip_generating = true;
                    self.state.filmstrip_generate_start = std::time::Instant::now();
                    self.state.filmstrip_toast_dismissed = false;

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
        std::thread::spawn(move || {
            match klipp_engine::generate_thumbnail(&path, seek_secs) {
                Ok((rgba, w, h)) => {
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

    fn spawn_filmstrip(
        &mut self,
        media_id: MediaId,
        path: PathBuf,
        duration: Timecode,
    ) {
        let tx = self.filmstrip_tx.clone();
        let duration_secs = duration.to_secs_f64();
        // Target ~60 filmstrip frames max — enough for visual coverage without
        // hammering the CPU. For a 5-min clip this means one frame every ~5 seconds.
        let interval = (duration_secs / 60.0).max(1.0);
        std::thread::Builder::new()
            .name("klipp-filmstrip".into())
            .spawn(move || {
                match klipp_engine::generate_filmstrip_incremental(&path, duration_secs, interval, 48, |time, rgba, w, h| {
                    let _ = tx.send(FilmstripResult::Frame {
                        media_id,
                        frame: klipp_media::FilmstripFrame {
                            time_secs: time,
                            rgba,
                            width: w,
                            height: h,
                        },
                    });
                }) {
                    Ok(count) => {
                        tracing::info!("Filmstrip: generated {count} frames for {:?}", path);
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

                // Skip if already conformed
                if klipp_media::audio_cache::cache_exists(&cache_dir, media_id) {
                    tracing::info!("Audio cache already exists for {:?}, loading peaks", media_id);
                    let peaks_path = cache_dir.join(format!("{}.peaks", media_id));
                    let pcm_path = cache_dir.join(format!("{}.pcm", media_id));
                    match klipp_media::audio_cache::load_peaks(&peaks_path) {
                        Ok(peaks) => {
                            // Compute duration from PCM file size
                            let pcm_size = std::fs::metadata(&pcm_path)
                                .map(|m| m.len())
                                .unwrap_or(0);
                            let duration_secs = pcm_size as f64
                                / (48000.0 * 2.0 * 4.0); // sample_rate * channels * sizeof(f32)
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
                            // Fall through to re-conform
                            do_conform(media_id, &path, &cache_dir, &tx);
                        }
                    }
                    return;
                }

                do_conform(media_id, &path, &cache_dir, &tx);
            })
            .ok();

        fn do_conform(
            media_id: MediaId,
            path: &Path,
            cache_dir: &Path,
            tx: &Sender<AudioConformResult>,
        ) {
            match klipp_media::audio_cache::conform_audio(media_id, path, cache_dir) {
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
        // Process up to 10 filmstrip frames per UI tick to avoid stalling the UI
        for _ in 0..10 {
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
                    self.state.filmstrip_generating = false;
                }
                Err(_) => break,
            }
        }
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

        // Zero-copy: RGBA [u8;4] has the same layout as egui::Color32.
        // Take ownership of the data to avoid a copy.
        let rgba_bytes = std::mem::take(&mut fb.data);
        fb.dirty = false;
        drop(fb);

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

    fn sync_sequence_to_engine(&self) {
        let mut seq = self.sequence.write().unwrap();
        *seq = self.state.sequence.clone();
    }

    fn sync_media_pool_to_engine(&self) {
        let mut pool = self.media_pool_shared.write().unwrap();
        *pool = self.state.media_pool.clone();
    }

    fn send_commands(&mut self, cmds: Vec<AppCommand>) {
        for cmd in cmds {
            match &cmd {
                AppCommand::LoadMediaFile { ref path, .. } => {
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
                        let track_index = self.state.sequence.tracks.iter()
                            .position(|t| t.kind == target_kind)
                            .unwrap_or(0);
                        if let Some(track) = self.state.sequence.tracks.get_mut(track_index) {
                            let clip_id = track.append_clip(*media_id, duration);
                            let clip = track.find_clip(clip_id).unwrap().clone();
                            self.state.history.push(EditCommand::LegacyAppendClip {
                                clip,
                                track_index,
                            });
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
                        if let Some(track) = self.state.sequence.tracks.get(*track_index) {
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
                                    &mut self.state.sequence, clip);
                                // Get the clip id from the result
                                if let klipp_timeline::magnetic_commands::MagneticEditCommand::AppendPrimary { ref appended_clip, .. } = result.command {
                                    self.state.selected_clip = Some(appended_clip.id);
                                }
                                self.state.history.push(EditCommand::Magnetic(result.command));
                                self.sync_sequence_to_engine();
                                self.state.dirty = true;
                            } else {
                                // Legacy path for Free tracks
                                let track = self.state.sequence.tracks.get_mut(*track_index).unwrap();
                                let clip_id = track.append_clip(*media_id, duration);
                                let clip = track.find_clip(clip_id).unwrap().clone();
                                self.state.history.push(EditCommand::LegacyAppendClip {
                                    clip,
                                    track_index: *track_index,
                                });
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
                        let duration = item.duration;
                        let has_video = !item.video_codec.is_empty();
                        let has_audio = item.has_audio;

                        if has_video && has_audio {
                            // Create linked V+A pair
                            let video_track_idx = self.state.sequence.tracks.iter()
                                .position(|t| t.kind == klipp_timeline::TrackKind::Video);
                            let audio_track_idx = self.state.sequence.tracks.iter()
                                .position(|t| t.kind == klipp_timeline::TrackKind::Audio);

                            if let (Some(vi), Some(ai)) = (video_track_idx, audio_track_idx) {
                                let video_clip_id = self.state.sequence.tracks[vi].append_clip(*media_id, duration);
                                let audio_clip_id = self.state.sequence.tracks[ai].append_clip(*media_id, duration);

                                // Link them
                                if let Some(vc) = self.state.sequence.tracks[vi].find_clip_mut(video_clip_id) {
                                    vc.linked_clip_id = Some(audio_clip_id);
                                }
                                if let Some(ac) = self.state.sequence.tracks[ai].find_clip_mut(audio_clip_id) {
                                    ac.linked_clip_id = Some(video_clip_id);
                                }

                                self.state.selected_clip = Some(video_clip_id);
                                self.sync_sequence_to_engine();
                                self.state.dirty = true;
                            }
                        } else {
                            // Single-stream: just append to matching track
                            let target_kind = if has_video {
                                klipp_timeline::TrackKind::Video
                            } else {
                                klipp_timeline::TrackKind::Audio
                            };
                            let track_idx = self.state.sequence.tracks.iter()
                                .position(|t| t.kind == target_kind)
                                .unwrap_or(0);
                            if let Some(track) = self.state.sequence.tracks.get_mut(track_idx) {
                                let clip_id = track.append_clip(*media_id, duration);
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
                    let track_index = self.state.sequence.tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == *clip_id));

                    if let Some(ti) = track_index {
                        if self.is_magnetic_track(ti) {
                            // Magnetic path: ripple delete
                            let result = klipp_timeline::magnetic::MagneticEditor::ripple_delete(
                                &mut self.state.sequence, *clip_id);
                            self.state.history.push(EditCommand::Magnetic(result.command));
                            if self.state.selected_clip == Some(*clip_id) {
                                self.state.selected_clip = None;
                            }
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        } else {
                            // Legacy path (existing code)
                            let linked_id = self.state.sequence.tracks.iter()
                                .flat_map(|t| t.clips.iter())
                                .find(|c| c.id == *clip_id)
                                .and_then(|c| c.linked_clip_id);

                            let track = &mut self.state.sequence.tracks[ti];
                            let pos = track.clips.iter().position(|c| c.id == *clip_id);
                            if let Some(pos) = pos {
                                let shifted: Vec<_> = track.clips[pos + 1..].iter()
                                    .map(|c| (c.id, c.timeline_in, c.timeline_out))
                                    .collect();
                                if let Some(removed) = track.ripple_remove(*clip_id) {
                                    self.state.history.push(EditCommand::LegacyRippleRemove {
                                        removed_clip: removed,
                                        track_index: ti,
                                        position: pos,
                                        shifted_clips: shifted,
                                    });
                                }
                            }
                            if self.state.selected_clip == Some(*clip_id) {
                                self.state.selected_clip = None;
                            }
                            // Also remove linked partner
                            if let Some(lid) = linked_id {
                                let ltrack_index = self.state.sequence.tracks.iter()
                                    .position(|t| t.clips.iter().any(|c| c.id == lid));
                                if let Some(lti) = ltrack_index {
                                    let track = &mut self.state.sequence.tracks[lti];
                                    let pos = track.clips.iter().position(|c| c.id == lid);
                                    if let Some(pos) = pos {
                                        let shifted: Vec<_> = track.clips[pos + 1..].iter()
                                            .map(|c| (c.id, c.timeline_in, c.timeline_out))
                                            .collect();
                                        if let Some(removed) = track.ripple_remove(lid) {
                                            self.state.history.push(EditCommand::LegacyRippleRemove {
                                                removed_clip: removed,
                                                track_index: lti,
                                                position: pos,
                                                shifted_clips: shifted,
                                            });
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
                        let clip = self.state.sequence.tracks.iter()
                            .flat_map(|t| t.clips.iter())
                            .find(|c| c.id == *clip_id);
                        match clip {
                            Some(c) => (c.group_id, *new_timeline_in - c.timeline_in),
                            None => (None, klipp_core::RationalTime { value: 0, rate: 1 }),
                        }
                    };

                    if let Some(track) = self.state.sequence.tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                    {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let old_in = clip.timeline_in;
                            let old_out = clip.timeline_out;
                            let dur = clip.duration();
                            clip.timeline_in = *new_timeline_in;
                            clip.timeline_out = *new_timeline_in + dur;
                            self.state.history.push(EditCommand::LegacyMoveClip {
                                clip_id: *clip_id,
                                old_timeline_in: old_in,
                                old_timeline_out: old_out,
                                new_timeline_in: clip.timeline_in,
                                new_timeline_out: clip.timeline_out,
                            });
                        }
                        track.sort_clips();
                    }

                    // Propagate move to grouped clips
                    if let Some(gid) = group_id {
                        if delta.value != 0 {
                            for track in &mut self.state.sequence.tracks {
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
                    if self.state.sequence.tracks.get(*to_track).is_some_and(|t| t.locked) { continue; }
                    let from = *from_track;
                    let to = *to_track;
                    let new_in = *new_timeline_in;
                    if let Some(src_track) = self.state.sequence.tracks.get_mut(from) {
                        if let Some(clip) = src_track.find_clip(*clip_id).cloned() {
                            let old_in = clip.timeline_in;
                            let old_out = clip.timeline_out;
                            let dur = clip.duration();
                            let mut moved_clip = clip.clone();
                            moved_clip.timeline_in = new_in;
                            moved_clip.timeline_out = new_in + dur;
                            src_track.remove_clip(*clip_id);

                            if let Some(dst_track) = self.state.sequence.tracks.get_mut(to) {
                                dst_track.clips.push(moved_clip.clone());
                                dst_track.sort_clips();
                            }

                            self.state.history.push(EditCommand::LegacyMoveClipToTrack {
                                clip: moved_clip,
                                old_timeline_in: old_in,
                                old_timeline_out: old_out,
                                new_timeline_in: new_in,
                                from_track: from,
                                to_track: to,
                            });
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
                    let linked_id = self.state.sequence.tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == *clip_id)
                        .and_then(|c| c.linked_clip_id);

                    // Trim main clip
                    if let Some(track) = self.state.sequence.tracks.iter_mut()
                        .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
                    {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            self.state.history.push(EditCommand::LegacyTrimClip {
                                clip_id: *clip_id,
                                old_source_in: clip.source_in,
                                old_source_out: clip.source_out,
                                old_timeline_in: clip.timeline_in,
                                old_timeline_out: clip.timeline_out,
                                new_source_in: *new_source_in,
                                new_source_out: *new_source_out,
                                new_timeline_in: *new_timeline_in,
                                new_timeline_out: *new_timeline_out,
                            });
                            clip.source_in = *new_source_in;
                            clip.source_out = *new_source_out;
                            clip.timeline_in = *new_timeline_in;
                            clip.timeline_out = *new_timeline_out;
                        }
                    }
                    // Trim linked partner by same values
                    if let Some(lid) = linked_id {
                        if let Some(track) = self.state.sequence.tracks.iter_mut()
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
                AppCommand::Undo => {
                    self.apply_undo();
                    continue;
                }
                AppCommand::Redo => {
                    self.apply_redo();
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
                        self.export_dialog.output_path = format!("{home}/output.mp4");
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
                    // Route through RenderQueue
                    let export_range = if self.export_dialog.marked_range {
                        if let (Some(inp), Some(outp)) = (self.state.sequence.in_point, self.state.sequence.out_point) {
                            Some((inp, outp))
                        } else { None }
                    } else { None };
                    let snapshot = klipp_export::RenderSnapshot::from_sequence(
                        &self.state.sequence,
                        &self.state.media_pool,
                        export_range,
                    );
                    let label = settings.output_path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Render".into());
                    self.state.render_queue.add_job(settings.output_path.clone(), settings.clone(), label, snapshot);
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
                    if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                        track.opacity = opacity.clamp(0.0, 1.0);
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::SetTrackVisibility { track_index, visible } => {
                    if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                        track.visible = *visible;
                    }
                    continue;
                }
                AppCommand::SetTrackVolume { track_index, volume } => {
                    if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                        track.volume = volume.clamp(0.0, 1.3);
                    }
                    // Fall through to forward to engine (updates AudioMixer)
                }
                AppCommand::SetTrackMute { track_index, muted } => {
                    if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                        track.muted = *muted;
                    }
                    // Fall through to forward to engine
                }
                AppCommand::SetTrackSolo { track_index, solo } => {
                    if *solo {
                        // Enable solo on this track, disable on all others
                        let solo_idx = *track_index;
                        for (i, track) in self.state.sequence.tracks.iter_mut().enumerate() {
                            track.solo = i == solo_idx;
                        }
                    } else {
                        // Unsolo: clear solo on all tracks
                        for track in self.state.sequence.tracks.iter_mut() {
                            track.solo = false;
                        }
                    }
                    // Fall through to forward to engine
                }
                AppCommand::SetTrackPan { track_index, pan } => {
                    if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                        track.pan = pan.clamp(-1.0, 1.0);
                    }
                    // Fall through to forward to engine
                }
                // Color grading
                AppCommand::SetClipExposure { clip_id, exposure } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.color_grade.enabled = true;
                            clip.color_grade.exposure = *exposure;
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::SetClipContrast { clip_id, contrast } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.color_grade.enabled = true;
                            clip.color_grade.contrast = *contrast;
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::SetClipSaturation { clip_id, saturation } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.color_grade.enabled = true;
                            clip.color_grade.saturation = *saturation;
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::SetClipTemperature { clip_id, temperature } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.color_grade.enabled = true;
                            clip.color_grade.temperature = *temperature;
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::SetClipTint { clip_id, tint } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.color_grade.enabled = true;
                            clip.color_grade.tint = *tint;
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
                }
                AppCommand::ResetClipColorGrade { clip_id } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.color_grade = klipp_timeline::ClipColorGrade::default();
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    continue;
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
                    for track in &mut self.state.sequence.tracks {
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
                    for track in &mut self.state.sequence.tracks {
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
                    for track in &mut self.state.sequence.tracks {
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
                    for track in &mut self.state.sequence.tracks {
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

                // Built-in effects
                AppCommand::AddBuiltinEffect { clip_id, ref effect } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            clip.builtin_effects.push(effect.clone());
                            break;
                        }
                    }
                    self.sync_sequence_to_engine();
                    // When not playing, re-seek to force re-render with updated effects
                    if self.state.playback != PlaybackState::Playing {
                        let _ = self.cmd_tx.send(AppCommand::SeekTo(self.state.playhead));
                    }
                    continue;
                }
                AppCommand::RemoveBuiltinEffect { clip_id, index } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if *index < clip.builtin_effects.len() {
                                clip.builtin_effects.remove(*index);
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
                AppCommand::UpdateBuiltinEffect { clip_id, index, ref effect } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            if let Some(fx) = clip.builtin_effects.get_mut(*index) {
                                *fx = effect.clone();
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
                AppCommand::ReorderBuiltinEffect { clip_id, from_index, to_index } => {
                    for track in &mut self.state.sequence.tracks {
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

                // Transitions
                AppCommand::AddTransition { track_index, clip_a_id, clip_b_id, kind_id, duration_frames } => {
                    let ti = *track_index;
                    let kind = klipp_engine::transition::transition_kind_from_id(*kind_id);
                    if let Some(track) = self.state.sequence.tracks.get_mut(ti) {
                        let fps_rate = self.state.sequence.fps.rate();
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
                    if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                        track.transitions.retain(|t| t.id != *transition_id);
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTransitionDuration { track_index, transition_id, duration_frames } => {
                    if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                        if let Some(t) = track.transitions.iter_mut().find(|t| t.id == *transition_id) {
                            let fps_rate = self.state.sequence.fps.rate();
                            t.duration = klipp_core::RationalTime::from_frames(*duration_frames as i64, fps_rate);
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTransitionKind { track_index, transition_id, kind_id } => {
                    if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
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
                    let fps_rate = self.state.sequence.fps.rate();
                    let duration = klipp_core::RationalTime::from_frames(*duration_frames as i64, fps_rate);
                    let timeline_out = *timeline_in + duration;
                    let clip = klipp_timeline::Clip {
                        id: klipp_core::ClipId::new(),
                        kind: klipp_timeline::ClipKind::Title(klipp_timeline::TitleData::default()),
                        timeline_in: *timeline_in,
                        timeline_out,
                        source_in: klipp_core::RationalTime::from_frames(0, fps_rate),
                        source_out: duration,
                        color_grade: klipp_timeline::ClipColorGrade::default(),
                        builtin_effects: Vec::new(),
                        effects: Vec::new(),
                        linked_clip_id: None,
                        group_id: None,
                        magnetic_anchor: None,
                    };
                    if let Some(track) = self.state.sequence.tracks.get_mut(ti) {
                        track.clips.push(clip);
                        track.clips.sort_by_key(|c| c.timeline_in.value);
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UpdateTitleData { clip_id, text, font_size, color } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                            if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                                td.text = text.clone();
                                td.font_size = *font_size;
                                td.color = *color;
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTitleAlignment { clip_id, alignment } => {
                    for track in &mut self.state.sequence.tracks {
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
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTitlePosition { clip_id, x, y } => {
                    for track in &mut self.state.sequence.tracks {
                        if let Some(clip) = track.clips.iter_mut().find(|c| c.id == *clip_id) {
                            if let klipp_timeline::ClipKind::Title(ref mut td) = clip.kind {
                                td.position = klipp_timeline::TitlePosition { x: *x, y: *y };
                            }
                        }
                    }
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTitleBackground { clip_id, bg_kind, color } => {
                    for track in &mut self.state.sequence.tracks {
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
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::SetTitleShadow { clip_id, enabled, color, offset_x, offset_y } => {
                    for track in &mut self.state.sequence.tracks {
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
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }

                // Phase 4: Blade
                AppCommand::BladeClip { track_index, clip_id, cut_at } => {
                    if self.state.sequence.tracks.get(*track_index).is_some_and(|t| t.locked) { continue; }
                    let ti = *track_index;
                    let cid = *clip_id;
                    let cut = *cut_at;

                    if self.is_magnetic_track(ti) {
                        // Magnetic blade
                        let result = klipp_timeline::magnetic::MagneticEditor::blade(
                            &mut self.state.sequence, cid, cut);
                        self.state.history.push(EditCommand::Magnetic(result.command));
                        self.state.dirty = true;
                    } else {
                        // Legacy blade (existing code)
                        if let Some(track) = self.state.sequence.tracks.get_mut(ti) {
                            let original = track.find_clip(cid).cloned();
                            let position = track.clips.iter().position(|c| c.id == cid);
                            if let (Some(original), Some(pos)) = (original, position) {
                                if let Some((clip_a, clip_b)) = track.blade_at(cid, cut) {
                                    self.state.history.push(EditCommand::LegacyBladeClip {
                                        track_index: ti,
                                        original_clip: original,
                                        clip_a,
                                        clip_b,
                                        position: pos,
                                    });
                                    self.state.dirty = true;
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
                    let track_index = self.state.sequence.tracks.iter()
                        .position(|t| t.find_clip(cid).is_some())
                        .unwrap_or(0);
                    if let Some(track) = self.state.sequence.tracks.get_mut(track_index) {
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
                                self.state.history.push(EditCommand::LegacyCutClip {
                                    clip,
                                    media_item: media,
                                    track_index,
                                    position: pos,
                                    shifted_clips: shifted,
                                });
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
                    let track_index = self.state.sequence.tracks.iter()
                        .position(|t| t.find_clip(cid).is_some())
                        .unwrap_or(0);
                    if let Some(track) = self.state.sequence.tracks.get(track_index) {
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
                            if let Some(track) = self.state.sequence.tracks.get_mut(track_index) {
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
                                    color_grade: clip.color_grade,
                                    effects: clip.effects.clone(),
                                    builtin_effects: clip.builtin_effects.clone(),
                                    linked_clip_id: None,
                                    group_id: None,
                                    magnetic_anchor: None,
                                };
                                Self::overwrite_insert_clip_on_track(track, new_clip.clone());
                                self.state.history.push(EditCommand::LegacyPasteClip {
                                    clip: new_clip,
                                    track_index,
                                });
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
                    self.state.sequence.in_point = Some(*tc);
                    continue;
                }
                AppCommand::SetSequenceOutPoint(tc) => {
                    self.state.sequence.out_point = Some(*tc);
                    continue;
                }
                AppCommand::ClearSequenceInPoint => {
                    self.state.sequence.in_point = None;
                    continue;
                }
                AppCommand::ClearSequenceOutPoint => {
                    self.state.sequence.out_point = None;
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
                            let timeline_in = self.state.sequence.in_point.unwrap_or(self.state.playhead);
                            if let Some(track) = self.state.sequence.tracks.iter_mut()
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
                                    color_grade: klipp_timeline::ClipColorGrade::default(),
                                    effects: Vec::new(),
                                    builtin_effects: Vec::new(),
                                    linked_clip_id: None,
                                    group_id: None,
                                    magnetic_anchor: None,
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
                            let timeline_in = self.state.sequence.in_point.unwrap_or(self.state.playhead);
                            if let Some(track) = self.state.sequence.tracks.iter_mut()
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
                                    color_grade: klipp_timeline::ClipColorGrade::default(),
                                    effects: Vec::new(),
                                    builtin_effects: Vec::new(),
                                    linked_clip_id: None,
                                    group_id: None,
                                    magnetic_anchor: None,
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
                    if let (Some(in_pt), Some(out_pt)) = (self.state.sequence.in_point, self.state.sequence.out_point) {
                        if in_pt < out_pt {
                            for track in &mut self.state.sequence.tracks {
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
                    if let (Some(in_pt), Some(out_pt)) = (self.state.sequence.in_point, self.state.sequence.out_point) {
                        if in_pt < out_pt {
                            let gap = out_pt - in_pt;
                            for track in &mut self.state.sequence.tracks {
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
                    let usage = media_usage_count(*media_id, &self.state.sequence);
                    if usage > 0 {
                        self.state.show_remove_media_confirm = Some(*media_id);
                    } else {
                        self.state.media_pool.remove(media_id);
                        self.sync_media_pool_to_engine();
                    }
                    continue;
                }
                AppCommand::ConfirmRemoveFromPool { media_id } => {
                    self.state.media_pool.remove(media_id);
                    self.state.show_remove_media_confirm = None;
                    self.sync_media_pool_to_engine();
                    continue;
                }
                AppCommand::SetMediaRating { media_id, rating } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        item.rating = (*rating).min(5);
                    }
                    continue;
                }
                AppCommand::SetMediaTags { media_id, tags } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        item.tags = tags.clone();
                    }
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
                AppCommand::CreateBin { name } => {
                    // Bins are implicit — just log. Items get assigned via MoveItemToBin.
                    tracing::info!("Bin created: {}", name);
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
                        }
                    }
                    tracing::info!("Bin deleted: {}", name);
                    continue;
                }
                AppCommand::MoveItemToBin { media_id, bin } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        item.bin = bin.clone();
                    }
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
                    continue;
                }
                AppCommand::ToggleFullscreen => {
                    self.state.fullscreen_viewer = !self.state.fullscreen_viewer;
                    self.state.fullscreen_changed = true;
                    continue;
                }
                AppCommand::NewProject => {
                    if self.state.dirty {
                        self.state.show_unsaved_dialog = true;
                        self.state.unsaved_dialog_action = UnsavedAction::NewProject;
                        continue;
                    }
                    self.do_new_project();
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
                AppCommand::AddVideoTrack => {
                    let name = self.state.sequence.next_track_name(klipp_timeline::TrackKind::Video);
                    self.state.sequence.add_video_track(name);
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::AddAudioTrack => {
                    let name = self.state.sequence.next_track_name(klipp_timeline::TrackKind::Audio);
                    self.state.sequence.add_audio_track(name);
                    self.sync_sequence_to_engine();
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveTrack { track_id } => {
                    let tid = *track_id;
                    let has_clips = self.state.sequence.tracks.iter()
                        .find(|t| t.id == tid)
                        .map(|t| !t.clips.is_empty())
                        .unwrap_or(false);
                    if has_clips {
                        // Show confirmation dialog
                        self.state.confirm_remove_track = Some(tid);
                    } else {
                        let _ = self.state.sequence.remove_track(tid);
                        self.sync_sequence_to_engine();
                        self.state.dirty = true;
                    }
                    continue;
                }
                AppCommand::RenameTrack { track_id, ref name } => {
                    if let Some(track) = self.state.sequence.tracks.iter_mut().find(|t| t.id == *track_id) {
                        track.name = name.clone();
                    }
                    self.state.dirty = true;
                    continue;
                }
                // Markers
                AppCommand::AddMarker { position, label, color } => {
                    self.state.sequence.markers.push(Marker {
                        id: MarkerId::new(),
                        position: *position,
                        label: label.clone(),
                        color: *color,
                    });
                    self.state.sequence.markers.sort_by(|a, b| {
                        a.position.to_secs_f64().partial_cmp(&b.position.to_secs_f64()).unwrap()
                    });
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::RemoveMarker { marker_id } => {
                    self.state.sequence.markers.retain(|m| m.id != *marker_id);
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UpdateMarkerLabel { marker_id, label } => {
                    if let Some(m) = self.state.sequence.markers.iter_mut().find(|m| m.id == *marker_id) {
                        m.label = label.clone();
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::UpdateMarkerColor { marker_id, color } => {
                    if let Some(m) = self.state.sequence.markers.iter_mut().find(|m| m.id == *marker_id) {
                        m.color = *color;
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::JumpToMarker { marker_id } => {
                    if let Some(m) = self.state.sequence.markers.iter().find(|m| m.id == *marker_id) {
                        let pos = m.position;
                        self.state.playhead = pos;
                        let _ = self.cmd_tx.send(AppCommand::SeekTo(pos));
                    }
                    continue;
                }
                // Linked clips
                AppCommand::UnlinkClip { clip_id } => {
                    let cid = *clip_id;
                    // Find the linked partner
                    let partner_id = self.state.sequence.tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == cid)
                        .and_then(|c| c.linked_clip_id);
                    // Clear linked_clip_id on both
                    for track in &mut self.state.sequence.tracks {
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
                    for track in &mut self.state.sequence.tracks {
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
                    if let Some(track) = self.state.sequence.tracks.iter_mut().find(|t| t.id == *track_id) {
                        track.locked = *locked;
                    }
                    self.state.dirty = true;
                    continue;
                }
                AppCommand::GroupClips { clip_ids } => {
                    let gid = GroupId::new();
                    for track in &mut self.state.sequence.tracks {
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
                    for track in &mut self.state.sequence.tracks {
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
                    for track in &mut self.state.sequence.tracks {
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
        self.state.sequence = Sequence::new_default();
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
        self.sync_sequence_to_engine();
    }

    fn apply_undo(&mut self) {
        if let Some(cmd) = self.state.history.undo() {
            self.apply_edit_inverse(&cmd);
            self.sync_sequence_to_engine();
            self.state.dirty = true;
        }
    }

    fn apply_redo(&mut self) {
        if let Some(cmd) = self.state.history.redo() {
            self.apply_edit_forward(&cmd);
            self.sync_sequence_to_engine();
            self.state.dirty = true;
        }
    }

    fn apply_edit_inverse(&mut self, cmd: &EditCommand) {
        match cmd {
            EditCommand::LegacyAppendClip { clip, track_index } => {
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyRemoveClip { clip, track_index, position } => {
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, clip.clone());
                }
            }
            EditCommand::LegacyRippleRemove { removed_clip, track_index, position, shifted_clips } => {
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
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
                if let Some(track) = self.state.sequence.tracks.iter_mut()
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
                if let Some(track) = self.state.sequence.tracks.iter_mut()
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
            EditCommand::LegacyBladeClip { track_index, original_clip, clip_a, clip_b, position } => {
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    // Undo: remove clip_a and clip_b, insert original
                    track.clips.retain(|c| c.id != clip_a.id && c.id != clip_b.id);
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, original_clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyCutClip { clip, track_index, position, shifted_clips, .. } => {
                // Undo cut = re-insert clip + restore shifted positions
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
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
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyMoveClipToTrack { clip, old_timeline_in, old_timeline_out, from_track, to_track, .. } => {
                // Undo: move clip back from to_track to from_track with original position
                if let Some(dst) = self.state.sequence.tracks.get_mut(*to_track) {
                    dst.remove_clip(clip.id);
                }
                if let Some(src) = self.state.sequence.tracks.get_mut(*from_track) {
                    let mut restored = clip.clone();
                    restored.timeline_in = *old_timeline_in;
                    restored.timeline_out = *old_timeline_out;
                    src.clips.push(restored);
                    src.sort_clips();
                }
            }
            EditCommand::Magnetic(cmd) => {
                klipp_timeline::magnetic::MagneticEditor::apply_inverse(
                    &mut self.state.sequence, cmd);
            }
        }
    }

    fn apply_edit_forward(&mut self, cmd: &EditCommand) {
        match cmd {
            EditCommand::LegacyAppendClip { clip, track_index } => {
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyRemoveClip { clip, .. } => {
                if let Some(track) = self.state.sequence.tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == clip.id))
                {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::LegacyRippleRemove { removed_clip, .. } => {
                if let Some(track) = self.state.sequence.tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == removed_clip.id))
                {
                    track.ripple_remove(removed_clip.id);
                }
            }
            EditCommand::LegacyMoveClip { clip_id, new_timeline_in, new_timeline_out, .. } => {
                if let Some(track) = self.state.sequence.tracks.iter_mut()
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
                if let Some(track) = self.state.sequence.tracks.iter_mut()
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
            EditCommand::LegacyBladeClip { track_index, original_clip, clip_a, clip_b, .. } => {
                // Redo blade: remove original, insert clip_a and clip_b
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.remove_clip(original_clip.id);
                    track.clips.push(clip_a.clone());
                    track.clips.push(clip_b.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyCutClip { clip, .. } => {
                // Redo cut = ripple remove
                if let Some(track) = self.state.sequence.tracks.iter_mut()
                    .find(|t| t.clips.iter().any(|c| c.id == clip.id))
                {
                    track.ripple_remove(clip.id);
                }
            }
            EditCommand::LegacyPasteClip { clip, track_index } => {
                // Redo paste = re-insert
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::LegacyMoveClipToTrack { clip, from_track, to_track, new_timeline_in, .. } => {
                // Redo: move from from_track to to_track at new position
                if let Some(src) = self.state.sequence.tracks.get_mut(*from_track) {
                    src.remove_clip(clip.id);
                }
                if let Some(dst) = self.state.sequence.tracks.get_mut(*to_track) {
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
                    &mut self.state.sequence, cmd);
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
                color_grade: clip.color_grade,
                effects: clip.effects.clone(),
                builtin_effects: clip.builtin_effects.clone(),
                linked_clip_id: clip.linked_clip_id,
                group_id: clip.group_id,
                magnetic_anchor: None,
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
                color_grade: clip.color_grade,
                effects: clip.effects.clone(),
                builtin_effects: clip.builtin_effects.clone(),
                linked_clip_id: clip.linked_clip_id,
                group_id: clip.group_id,
                magnetic_anchor: None,
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
        };
        match klipp_project::save_project_with_color(path, &self.state.sequence, &self.state.media_pool, &color) {
            Ok(()) => {
                self.state.project_path = Some(path.to_path_buf());
                self.state.dirty = false;
                tracing::info!("Project saved to {:?}", path);
                self.prefs.add_recent_project(path.to_path_buf());
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
            Ok((sequence, media_pool, color_settings)) => {
                self.state.fps = sequence.fps;
                self.state.sequence = sequence;
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

                self.state.project_path = Some(path.to_path_buf());
                self.state.dirty = false;
                self.state.selected_clip = None;
                self.prefs.add_recent_project(path.to_path_buf());
                self.prefs_dirty = true;
                self.state.playback = PlaybackState::Idle;
                self.state.playhead = Timecode::zero();
                self.state.thumbnail_cache = klipp_media::ThumbnailCache::new();
                self.state.thumbnail_textures.clear();
                self.state.filmstrip_textures.clear();
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

                // Reload media files into the engine and trigger thumbnails
                let media_items: Vec<_> = self
                    .state
                    .media_pool
                    .all()
                    .filter(|item| !item.offline)
                    .map(|item| (item.id, item.path.clone(), item.duration))
                    .collect();
                for (id, path, duration) in media_items {
                    // Re-open decoders in the engine
                    self.cmd_tx.send(AppCommand::LoadMediaFile {
                        path: path.clone(),
                        media_id: id,
                    }).ok();
                    self.spawn_thumbnail(id, path, duration);
                }

                tracing::info!("Project loaded from {:?}", path);
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
        egui::Window::new("Unsaved Changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label("Save changes before closing?");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Discard").clicked() {
                        self.state.dirty = false;
                        self.state.show_unsaved_dialog = false;
                        let action = self.state.unsaved_dialog_action.clone();
                        self.state.unsaved_dialog_action = UnsavedAction::None;
                        match action {
                            UnsavedAction::NewProject => self.do_new_project(),
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
                        // After save, perform the pending action
                        let action = self.state.unsaved_dialog_action.clone();
                        self.state.unsaved_dialog_action = UnsavedAction::None;
                        match action {
                            UnsavedAction::NewProject => self.do_new_project(),
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

        // Only skip single-key shortcuts when a text edit is focused (editing_timecode).
        // Ctrl+ shortcuts always work. Previously this checked focused().is_some()
        // which blocked shortcuts whenever ANY widget (including buttons) had focus.
        let text_editing = self.state.editing_timecode;

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
                    self.export_dialog.output_path = format!("{home}/output.mp4");
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
                    let gid = self.state.sequence.tracks.iter()
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

            // G (no modifiers) — Connect selected clip to nearest primary clip
            if !i.modifiers.ctrl && !i.modifiers.shift && i.key_pressed(egui::Key::G) {
                if let Some(clip_id) = self.state.selected_clip {
                    let clip_track = self.state.sequence.tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == clip_id));
                    if let Some(ti) = clip_track {
                        if self.state.sequence.tracks[ti].magnetic_role != klipp_timeline::TrackRole::Primary {
                            let clip_in = self.state.sequence.tracks[ti].clips.iter()
                                .find(|c| c.id == clip_id)
                                .map(|c| c.timeline_in);
                            if let Some(clip_in) = clip_in {
                                if let Some(primary_idx) = self.state.sequence.primary_track_index() {
                                    let best_anchor = self.state.sequence.tracks[primary_idx].clips.iter()
                                        .filter(|c| !c.kind.is_gap())
                                        .min_by_key(|c| (c.timeline_in - clip_in).abs().value.unsigned_abs());
                                    if let Some(anchor) = best_anchor {
                                        let anchor_id = anchor.id;
                                        let result = klipp_timeline::magnetic::MagneticEditor::connect_clip(
                                            &mut self.state.sequence,
                                            clip_id,
                                            anchor_id,
                                            klipp_timeline::AnchorAlignment::StartAligned,
                                        );
                                        self.state.history.push(crate::history::EditCommand::Magnetic(result.command));
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
                    let has_anchor = self.state.sequence.tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .any(|c| c.id == clip_id && c.magnetic_anchor.is_some());
                    if has_anchor {
                        let result = klipp_timeline::magnetic::MagneticEditor::disconnect_clip(
                            &mut self.state.sequence, clip_id);
                        self.state.history.push(crate::history::EditCommand::Magnetic(result.command));
                        self.sync_sequence_to_engine();
                        self.state.dirty = true;
                    }
                }
            }

            // Shift+S (no ctrl) — Promote track to Connected Storyline / Break apart
            if !i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::S) {
                if let Some(clip_id) = self.state.selected_clip {
                    let clip_track = self.state.sequence.tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == clip_id));
                    if let Some(ti) = clip_track {
                        if self.state.sequence.tracks[ti].magnetic_role == klipp_timeline::TrackRole::Connected {
                            if let Some(primary_idx) = self.state.sequence.primary_track_index() {
                                let first_clip_in = self.state.sequence.tracks[ti].clips.first()
                                    .map(|c| c.timeline_in);
                                if let Some(clip_in) = first_clip_in {
                                    let best_anchor = self.state.sequence.tracks[primary_idx].clips.iter()
                                        .filter(|c| !c.kind.is_gap())
                                        .min_by_key(|c| (c.timeline_in - clip_in).abs().value.unsigned_abs());
                                    if let Some(anchor) = best_anchor {
                                        let anchor_id = anchor.id;
                                        let result = klipp_timeline::magnetic::MagneticEditor::promote_to_storyline(
                                            &mut self.state.sequence, ti, anchor_id);
                                        self.state.history.push(crate::history::EditCommand::Magnetic(result.command));
                                        self.sync_sequence_to_engine();
                                        self.state.dirty = true;
                                    }
                                }
                            }
                        } else if self.state.sequence.tracks[ti].magnetic_role == klipp_timeline::TrackRole::ConnectedStoryline {
                            let result = klipp_timeline::magnetic::MagneticEditor::break_apart_storyline(
                                &mut self.state.sequence, ti);
                            self.state.history.push(crate::history::EditCommand::Magnetic(result.command));
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        }
                    }
                }
            }

            // Shift+Delete — Lift (leave gap, no ripple) on magnetic tracks
            if i.modifiers.shift && (i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
                if let Some(clip_id) = self.state.selected_clip {
                    let track_idx = self.state.sequence.tracks.iter()
                        .position(|t| t.clips.iter().any(|c| c.id == clip_id));
                    if let Some(ti) = track_idx {
                        if self.is_magnetic_track(ti) {
                            let result = klipp_timeline::magnetic::MagneticEditor::lift(
                                &mut self.state.sequence, clip_id);
                            self.state.history.push(crate::history::EditCommand::Magnetic(result.command));
                            self.state.selected_clip = None;
                            self.sync_sequence_to_engine();
                            self.state.dirty = true;
                        }
                    }
                }
            }
            // Delete/Backspace — remove selected clip (plain, no shift)
            else if !i.modifiers.shift && (i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RemoveClip { clip_id });
                }
            }

            // W — Toggle Insert/Overwrite mode
            if i.key_pressed(egui::Key::W) {
                self.state.edit_mode = match self.state.edit_mode {
                    klipp_timeline::magnetic::EditMode::Insert => klipp_timeline::magnetic::EditMode::Overwrite,
                    klipp_timeline::magnetic::EditMode::Overwrite => klipp_timeline::magnetic::EditMode::Insert,
                };
            }
            // Ctrl+X — Cut
            if i.modifiers.ctrl && i.key_pressed(egui::Key::X) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::CutClip { clip_id });
                }
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
                for (track_index, track) in self.state.sequence.tracks.iter().enumerate() {
                    if let Some((_, clip)) = track.clip_at_timecode(ph) {
                        cmds.push(AppCommand::BladeClip {
                            track_index,
                            clip_id: clip.id,
                            cut_at: ph,
                        });
                    }
                }
            }
            // Ctrl+T — Create title clip at playhead on a free video track (or create new one)
            if i.modifiers.ctrl && i.key_pressed(egui::Key::T) {
                let ph = self.state.playhead;
                // Find a video track that has no clip at the playhead
                let track_index = self.state.sequence.tracks.iter()
                    .enumerate()
                    .find(|(_, t)| {
                        t.kind == klipp_timeline::TrackKind::Video
                            && t.clip_at_timecode(ph).is_none()
                    })
                    .map(|(i, _)| i);
                let track_index = match track_index {
                    Some(i) => i,
                    None => {
                        // All video tracks occupied — add a new video track
                        let _new_idx = self.state.sequence.tracks.len();
                        let track_num = self.state.sequence.tracks.iter()
                            .filter(|t| t.kind == klipp_timeline::TrackKind::Video)
                            .count() + 1;
                        let track = klipp_timeline::Track::new(
                            &format!("Video {track_num}"),
                            klipp_timeline::TrackKind::Video,
                        );
                        self.state.sequence.tracks.insert(0, track); // insert at top
                        0
                    }
                };
                let fps_rate = self.state.sequence.fps.rate();
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
                    cmds.push(AppCommand::ClearSequenceInPoint);
                } else if self.state.viewer_mode == ViewerMode::Source {
                    tracing::debug!("I key (Source mode) -> Set source in point at {:?}", self.state.source_playhead);
                    cmds.push(AppCommand::SetSourceInPoint(self.state.source_playhead));
                } else {
                    tracing::debug!("I key (Program mode) -> Set sequence in point at {:?}", self.state.playhead);
                    cmds.push(AppCommand::SetSequenceInPoint(self.state.playhead));
                }
            }
            if i.key_pressed(egui::Key::O) && !i.modifiers.ctrl {
                if i.modifiers.shift {
                    tracing::debug!("Shift+O -> Clear sequence out point");
                    cmds.push(AppCommand::ClearSequenceOutPoint);
                } else if self.state.viewer_mode == ViewerMode::Source {
                    tracing::debug!("O key (Source mode) -> Set source out point at {:?}", self.state.source_playhead);
                    cmds.push(AppCommand::SetSourceOutPoint(self.state.source_playhead));
                } else {
                    tracing::debug!("O key (Program mode) -> Set sequence out point at {:?}", self.state.playhead);
                    cmds.push(AppCommand::SetSequenceOutPoint(self.state.playhead));
                }
            }
            // , — Insert edit (ripple)
            if i.events.iter().any(|e| matches!(e, egui::Event::Text(s) if s == ",")) {
                cmds.push(AppCommand::InsertEdit);
            }
            // . — Overwrite edit
            if i.events.iter().any(|e| matches!(e, egui::Event::Text(s) if s == ".")) {
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
            // M — Add marker
            if i.key_pressed(egui::Key::M) && !i.modifiers.ctrl {
                cmds.push(AppCommand::AddMarker {
                    position: self.state.playhead,
                    label: String::new(),
                    color: klipp_core::MarkerColor::Blue,
                });
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
                    let clip = self.state.sequence.tracks.iter()
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

fn apply_preset_to_dialog(dialog: &mut ExportDialogState, settings: &ExportPresetSettings) {
    dialog.format = settings.format;
    dialog.video_codec = settings.video_codec;
    dialog.crf = settings.crf;
    dialog.audio_codec = settings.audio_codec;
    dialog.audio_bitrate = settings.audio_bitrate_kbps;
}

impl eframe::App for KlippApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Smooth playhead interpolation BEFORE draining events
        if self.state.playback == PlaybackState::Playing {
            let wall_elapsed = self.state.playhead_anchor_wall.elapsed().as_secs_f64();
            let anchor_secs = self.state.playhead_anchor.to_secs_f64();
            let speed = self.state.playback_speed;
            let interpolated_secs = anchor_secs + wall_elapsed * speed;
            let dur_secs = self.state.sequence_duration().to_secs_f64();
            let clamped = interpolated_secs.clamp(0.0, dur_secs);

            self.state.playhead_smooth_secs = clamped;
            self.state.playhead = Timecode::from_secs_f64(clamped, self.state.fps.rate());
        }

        // 1. Drain events from engine
        self.drain_events(ctx);
        self.drain_thumbnails();
        self.drain_audio_conforms();
        self.drain_proxy_events();
        self.drain_proxy_progress();
        self.poll_render_queue();
        self.upload_frame_to_egui(ctx);

        // Debounced preferences save (at most once per second)
        if self.prefs_dirty && self.prefs_last_save.elapsed().as_secs() >= 1 {
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
                };
                match klipp_project::save_project_with_color(&autosave_path, &self.state.sequence, &self.state.media_pool, &color) {
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
            let usage = media_usage_count(media_id, &self.state.sequence);
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
                            let _ = self.state.sequence.remove_track_with_clips(track_id);
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

        // 4d. Preferences window
        if self.state.show_preferences {
            let (still_open, pref_cmds) = self.preferences_panel.show(
                ctx,
                &mut self.prefs,
                &mut self.prefs_dirty,
            );
            all_cmds.extend(pref_cmds);
            if !still_open {
                self.state.show_preferences = false;
            }
        }

        // Handle fullscreen toggle
        if self.state.fullscreen_viewer {
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

                    // Timeline (bottom of center)
                    let max_timeline_h = main_height * theme::TIMELINE_MAX_H_PCT;
                    let timeline_h = self
                        .layout
                        .timeline_height
                        .clamp(theme::TIMELINE_MIN_H, max_timeline_h);

                    let timeline_rect = egui::Rect::from_min_size(
                        egui::pos2(center_left, main_bottom - timeline_h),
                        egui::vec2(center_w, timeline_h),
                    );

                    // Horizontal resizer (between viewer and timeline)
                    let h_resizer_rect = egui::Rect::from_min_size(
                        egui::pos2(
                            center_left,
                            timeline_rect.min.y - theme::RESIZER_THICKNESS,
                        ),
                        egui::vec2(center_w, theme::RESIZER_THICKNESS),
                    );

                    // Viewer (top of center, above horizontal resizer)
                    let viewer_rect = egui::Rect::from_min_max(
                        egui::pos2(center_left, main_top),
                        egui::pos2(center_left + center_w, h_resizer_rect.min.y),
                    );

                    // ── Draw resizers ──
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
                        // Dragging right shrinks inspector, dragging left grows it
                        self.layout.inspector_width = (self.layout.inspector_width
                            - right_delta)
                            .clamp(theme::INSPECTOR_MIN_W, theme::INSPECTOR_MAX_W);
                    }
                    if h_delta != 0.0 {
                        // Dragging up increases timeline height
                        self.layout.timeline_height = (self.layout.timeline_height
                            - h_delta)
                            .clamp(theme::TIMELINE_MIN_H, max_timeline_h);
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
                    });

                    // Inspector (right panel)
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inspector_rect), |ui| {
                        ui.set_min_size(inspector_rect.size());
                        ui.painter()
                            .rect_filled(inspector_rect, 0.0, theme.bg_panel);
                        let cmds = self.new_inspector.show(
                            ui,
                            &self.state,
                            &self.theme,
                            &mut self.layout.active_inspector_tab,
                        );
                        all_cmds.extend(cmds);
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
                        let cmds = self.viewer.show(ui, &self.state, &self.theme);
                        all_cmds.extend(cmds);
                    });

                    // Timeline (center bottom)
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
                                ui.colored_label(egui::Color32::from_rgb(255, 100, 100), "\u{2717}");
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

        // 12. Filmstrip generation toast
        if self.state.filmstrip_generating && !self.state.filmstrip_toast_dismissed {
            let elapsed = self.state.filmstrip_generate_start.elapsed().as_secs_f64();
            egui::Area::new(egui::Id::new("filmstrip_toast"))
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -80.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style())
                        .fill(egui::Color32::from_rgb(35, 35, 42))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 70, 85)))
                        .rounding(6.0)
                        .inner_margin(egui::Margin::symmetric(16.0, 10.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Generating filmstrip frames... ({:.1}s)",
                                        elapsed
                                    ))
                                    .color(egui::Color32::from_rgb(200, 200, 210))
                                    .size(12.0),
                                );
                                ui.add_space(8.0);
                                if ui.small_button("\u{2715}").on_hover_text("Dismiss").clicked() {
                                    self.state.filmstrip_toast_dismissed = true;
                                }
                            });
                        });
                });
            ctx.request_repaint();
        }

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
                                        if ui.small_button("\u{2715}").on_hover_text("Dismiss").clicked() {
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

        if has_quit {
            if self.state.dirty {
                self.state.show_unsaved_dialog = true;
                self.state.unsaved_dialog_action = UnsavedAction::Quit;
            } else {
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
        self.state.sequence.tracks.iter()
            .find(|t| t.clips.iter().any(|c| c.id == *clip_id))
            .is_some_and(|t| t.locked)
    }

    /// Check if a track uses magnetic editing (Primary, Connected, or ConnectedStoryline).
    fn is_magnetic_track(&self, track_index: usize) -> bool {
        matches!(
            self.state.sequence.tracks.get(track_index).map(|t| &t.magnetic_role),
            Some(klipp_timeline::TrackRole::Primary | klipp_timeline::TrackRole::Connected | klipp_timeline::TrackRole::ConnectedStoryline)
        )
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
                for job in &self.state.render_queue.jobs {
                    ui.horizontal(|ui| {
                        let (icon, color) = match job.status {
                            klipp_export::RenderJobStatus::Queued => {
                                ("\u{23F3}", egui::Color32::from_rgb(180, 180, 100))
                            }
                            klipp_export::RenderJobStatus::Rendering => {
                                ("\u{25B6}", egui::Color32::from_rgb(100, 180, 255))
                            }
                            klipp_export::RenderJobStatus::Complete => {
                                ("\u{2713}", egui::Color32::from_rgb(100, 200, 120))
                            }
                            klipp_export::RenderJobStatus::Failed => {
                                ("\u{2717}", egui::Color32::from_rgb(255, 100, 100))
                            }
                            klipp_export::RenderJobStatus::Cancelled => {
                                ("\u{2014}", egui::Color32::from_rgb(120, 120, 120))
                            }
                        };
                        ui.colored_label(color, icon);
                        ui.label(&job.label);
                    });

                    // Progress bar
                    let progress = egui::ProgressBar::new(job.progress)
                        .show_percentage();
                    ui.add(progress);

                    if let Some(err) = &job.error {
                        ui.colored_label(egui::Color32::from_rgb(255, 100, 100), err);
                    }

                    ui.horizontal(|ui| {
                        match job.status {
                            klipp_export::RenderJobStatus::Queued
                            | klipp_export::RenderJobStatus::Rendering => {
                                if ui.small_button("Cancel").clicked() {
                                    cancel_id = Some(job.id);
                                }
                            }
                            _ => {}
                        }
                    });
                    ui.separator();
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

                        ui.label("Format:");
                        egui::ComboBox::from_id_salt("export_format")
                            .selected_text(self.export_dialog.format.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_dialog.format, ExportFormat::Mp4, "MP4");
                                ui.selectable_value(&mut self.export_dialog.format, ExportFormat::Mkv, "MKV");
                                ui.selectable_value(&mut self.export_dialog.format, ExportFormat::Mov, "MOV");
                            });
                        ui.end_row();

                        ui.label("Video codec:");
                        egui::ComboBox::from_id_salt("export_vcodec")
                            .selected_text(self.export_dialog.video_codec.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::H264, "H.264");
                                ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::H265, "H.265");
                                if self.export_dialog.format == ExportFormat::Mov {
                                    ui.selectable_value(&mut self.export_dialog.video_codec, ExportVideoCodec::ProRes, "ProRes");
                                }
                            });
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
                if let (Some(in_pt), Some(out_pt)) = (self.state.sequence.in_point, self.state.sequence.out_point) {
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
                            audio_codec: self.export_dialog.audio_codec,
                            audio_bitrate_kbps: self.export_dialog.audio_bitrate,
                        };
                        let export_range = if self.export_dialog.marked_range {
                            if let (Some(inp), Some(outp)) = (self.state.sequence.in_point, self.state.sequence.out_point) {
                                Some((inp, outp))
                            } else { None }
                        } else { None };
                        let snapshot = klipp_export::RenderSnapshot::from_sequence(
                            &self.state.sequence,
                            &self.state.media_pool,
                            export_range,
                        );
                        let label = path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "Render".into());
                        self.state.render_queue.add_job(path, settings, label, snapshot);
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
                                    audio_codec: self.export_dialog.audio_codec,
                                    audio_bitrate_kbps: self.export_dialog.audio_bitrate,
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
                                    audio_codec: self.export_dialog.audio_codec,
                                    audio_bitrate_kbps: self.export_dialog.audio_bitrate,
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
                                    audio_codec: self.export_dialog.audio_codec,
                                    audio_bitrate_kbps: self.export_dialog.audio_bitrate,
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

            // Upload filmstrip textures if not yet done (independent of thumbnail)
            if !self.state.filmstrip_textures.contains_key(&media_id) {
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

/// Count how many clips in the sequence reference this media_id.
fn media_usage_count(media_id: MediaId, sequence: &Sequence) -> usize {
    sequence.tracks.iter()
        .flat_map(|t| &t.clips)
        .filter(|c| c.media_id() == Some(media_id))
        .count()
}

/// Returns the autosave path for a given project path: `<stem>_autosave.klipp`
fn autosave_path_for(project_path: &Path) -> PathBuf {
    let stem = project_path.file_stem().unwrap_or_default().to_string_lossy();
    project_path.with_file_name(format!("{stem}_autosave.klipp"))
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
