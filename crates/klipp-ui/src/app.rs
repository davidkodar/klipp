use crate::history::EditCommand;
use crate::panels::inspector::InspectorPanel;
use crate::panels::timeline::TimelinePanel;
use crate::panels::toolbar::ToolbarPanel;
use crate::panels::transport::TransportBar;
use crate::panels::viewer::ViewerPanel;
use crate::state::{AppState, ClipboardContent, UnsavedAction};
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{
    AppCommand, AppEvent, ExportAudioCodec, ExportFormat, ExportSettings,
    ExportVideoCodec, MediaId, Timecode, Tool, ViewerMode,
};
use klipp_engine::{PlaybackState, SharedFrameBuffer};
use klipp_media::{MediaItem, ThumbnailEntry};
use klipp_timeline::Sequence;
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

pub struct KlippApp {
    cmd_tx: Sender<AppCommand>,
    event_rx: Receiver<AppEvent>,
    frame_buffer: SharedFrameBuffer,
    sequence: Arc<RwLock<Sequence>>,
    media_pool_shared: Arc<RwLock<klipp_media::MediaPool>>,
    state: AppState,
    toolbar: ToolbarPanel,
    viewer: ViewerPanel,
    inspector: InspectorPanel,
    transport: TransportBar,
    timeline: TimelinePanel,
    thumb_tx: Sender<ThumbnailResult>,
    thumb_rx: Receiver<ThumbnailResult>,
    filmstrip_tx: Sender<FilmstripResult>,
    filmstrip_rx: Receiver<FilmstripResult>,
    audio_conform_tx: Sender<AudioConformResult>,
    audio_conform_rx: Receiver<AudioConformResult>,
    export_child: Option<std::process::Child>,
    /// Track proxy ffmpeg child PIDs so we can kill them on exit
    proxy_children: Vec<Arc<std::sync::atomic::AtomicU32>>,
    export_event_tx: Sender<AppEvent>,
    export_event_rx: Receiver<AppEvent>,
    // Export dialog state
    export_output_path: String,
    export_format: ExportFormat,
    export_video_codec: ExportVideoCodec,
    export_crf: u32,
    export_audio_codec: ExportAudioCodec,
    export_audio_bitrate: u32,
    export_confirm_overwrite: bool,
    export_marked_range: bool,
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
        let (export_event_tx, export_event_rx) = crossbeam_channel::unbounded();
        let mut visuals = egui::Visuals::dark();
        visuals.window_fill = egui::Color32::from_rgb(8, 8, 13);
        visuals.panel_fill = egui::Color32::from_rgb(28, 28, 32);
        visuals.faint_bg_color = egui::Color32::from_rgb(22, 22, 26);
        visuals.extreme_bg_color = egui::Color32::from_rgb(15, 15, 18);
        visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(28, 28, 32);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(40, 40, 48);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(55, 55, 65);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(70, 70, 85);
        visuals.selection.bg_fill = egui::Color32::from_rgb(50, 90, 160);
        cc.egui_ctx.set_visuals(visuals);

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
            transport: TransportBar,
            timeline: TimelinePanel::default(),
            thumb_tx,
            thumb_rx,
            filmstrip_tx,
            filmstrip_rx,
            audio_conform_tx,
            audio_conform_rx,
            export_child: None,
            proxy_children: Vec::new(),
            export_event_tx,
            export_event_rx,
            export_output_path: String::new(),
            export_format: ExportFormat::Mp4,
            export_video_codec: ExportVideoCodec::H264,
            export_crf: 23,
            export_audio_codec: ExportAudioCodec::Aac,
            export_audio_bitrate: 128,
            export_confirm_overwrite: false,
            export_marked_range: false,
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
                        waveform_data: None,
                        audio_cache: None,
                    };

                    self.state.media_pool.add(media_item);
                    self.sync_media_pool_to_engine();
                    self.state.fps = fps;
                    self.state.playback = PlaybackState::Stopped;

                    // If this was a pending load (from Open File), also append to timeline
                    if self.state.pending_media_path.is_some() {
                        self.state.pending_media_path = None;
                        let clip_id = self
                            .state
                            .sequence
                            .video_track_mut()
                            .unwrap()
                            .append_clip(media_id, duration);
                        self.sync_sequence_to_engine();
                        self.state.selected_clip = Some(clip_id);
                        self.state.dirty = true;
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
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("klipp")
            .join("proxies");
        let _ = std::fs::create_dir_all(&cache_dir);
        let proxy_path = cache_dir.join(format!("{}.mp4", media_id));
        if proxy_path.exists() {
            if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                item.proxy_path = Some(proxy_path);
                item.proxy_ready = true;
            }
            return;
        }
        let source_path = item.path.clone();
        let tx = self.export_event_tx.clone();
        let proxy_out = proxy_path.clone();
        let child_pid: Arc<std::sync::atomic::AtomicU32> = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let pid_clone = child_pid.clone();
        self.proxy_children.push(child_pid);
        std::thread::spawn(move || {
            let _ = tx.send(AppEvent::ProxyGenerationStarted { media_id });
            let child = std::process::Command::new("ffmpeg")
                .arg("-i").arg(&source_path)
                .arg("-vf").arg("scale=960:-2")
                .arg("-c:v").arg("libx264").arg("-crf").arg("28").arg("-preset").arg("ultrafast")
                .arg("-c:a").arg("aac").arg("-b:a").arg("64k")
                .arg("-y").arg(&proxy_out)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
            match child {
                Ok(mut child) => {
                    pid_clone.store(child.id(), std::sync::atomic::Ordering::Relaxed);
                    let status = child.wait();
                    pid_clone.store(0, std::sync::atomic::Ordering::Relaxed);
                    match status {
                        Ok(s) if s.success() => {
                            let _ = tx.send(AppEvent::ProxyGenerationComplete { media_id, path: proxy_out });
                        }
                        Ok(_) => {
                            let _ = tx.send(AppEvent::ProxyGenerationFailed { media_id, error: "ffmpeg failed".into() });
                        }
                        Err(e) => {
                            let _ = tx.send(AppEvent::ProxyGenerationFailed { media_id, error: e.to_string() });
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::ProxyGenerationFailed { media_id, error: e.to_string() });
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
                        let track_index = self.state.sequence.tracks.iter()
                            .position(|t| t.kind == klipp_timeline::TrackKind::Video)
                            .unwrap_or(0);
                        let clip_id = self
                            .state
                            .sequence
                            .video_track_mut()
                            .unwrap()
                            .append_clip(*media_id, duration);
                        let clip = self.state.sequence.video_track().unwrap()
                            .find_clip(clip_id).unwrap().clone();
                        self.state.history.push(EditCommand::AppendClip {
                            clip,
                            track_index,
                        });
                        self.sync_sequence_to_engine();
                        self.state.selected_clip = Some(clip_id);
                        self.state.dirty = true;
                    }
                    continue;
                }
                AppCommand::AppendClipToTrack { media_id, track_index } => {
                    if let Some(item) = self.state.media_pool.get(media_id) {
                        let duration = item.duration;
                        if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                            let clip_id = track.append_clip(*media_id, duration);
                            let clip = track.find_clip(clip_id).unwrap().clone();
                            self.state.history.push(EditCommand::AppendClip {
                                clip,
                                track_index: *track_index,
                            });
                            self.sync_sequence_to_engine();
                            self.state.selected_clip = Some(clip_id);
                            self.state.dirty = true;
                        }
                    }
                    continue;
                }
                AppCommand::RemoveClip { clip_id } => {
                    let track_index = self.state.sequence.tracks.iter()
                        .position(|t| t.kind == klipp_timeline::TrackKind::Video)
                        .unwrap_or(0);
                    if let Some(track) = self.state.sequence.video_track_mut() {
                        // Record shifted clips before ripple remove
                        let pos = track.clips.iter().position(|c| c.id == *clip_id);
                        if let Some(pos) = pos {
                            let shifted: Vec<_> = track.clips[pos + 1..].iter()
                                .map(|c| (c.id, c.timeline_in, c.timeline_out))
                                .collect();
                            if let Some(removed) = track.ripple_remove(*clip_id) {
                                self.state.history.push(EditCommand::RippleRemove {
                                    removed_clip: removed,
                                    track_index,
                                    position: pos,
                                    shifted_clips: shifted,
                                });
                            }
                        }
                        self.sync_sequence_to_engine();
                        if self.state.selected_clip == Some(*clip_id) {
                            self.state.selected_clip = None;
                        }
                        self.state.dirty = true;
                    }
                    continue;
                }
                AppCommand::MoveClip {
                    clip_id,
                    new_timeline_in,
                } => {
                    if let Some(track) = self.state.sequence.video_track_mut() {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            let old_in = clip.timeline_in;
                            let old_out = clip.timeline_out;
                            let dur = clip.duration();
                            clip.timeline_in = *new_timeline_in;
                            clip.timeline_out = *new_timeline_in + dur;
                            self.state.history.push(EditCommand::MoveClip {
                                clip_id: *clip_id,
                                old_timeline_in: old_in,
                                old_timeline_out: old_out,
                                new_timeline_in: clip.timeline_in,
                                new_timeline_out: clip.timeline_out,
                            });
                        }
                        track.sort_clips();
                        self.sync_sequence_to_engine();
                        self.state.dirty = true;
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
                    if let Some(track) = self.state.sequence.video_track_mut() {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
                            self.state.history.push(EditCommand::TrimClip {
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
                        self.sync_sequence_to_engine();
                        self.state.dirty = true;
                    }
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
                AppCommand::ShowExportDialog => {
                    self.state.show_export_dialog = true;
                    if self.export_output_path.is_empty() {
                        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                        self.export_output_path = format!("{home}/output.mp4");
                    }
                    continue;
                }
                AppCommand::ExportSequence { settings } => {
                    self.start_export(settings.clone());
                    continue;
                }
                AppCommand::CancelExport => {
                    self.cancel_export();
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
                    continue;
                }
                AppCommand::SetTrackMute { track_index, muted } => {
                    if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                        track.muted = *muted;
                    }
                    continue;
                }
                AppCommand::SetTrackSolo { track_index } => {
                    let solo_idx = *track_index;
                    for (i, track) in self.state.sequence.tracks.iter_mut().enumerate() {
                        track.solo = i == solo_idx;
                    }
                    continue;
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

                // Phase 4: Blade
                AppCommand::BladeClip { track_index, clip_id, cut_at } => {
                    let ti = *track_index;
                    let cid = *clip_id;
                    let cut = *cut_at;
                    if let Some(track) = self.state.sequence.tracks.get_mut(ti) {
                        // Find original clip before blade
                        let original = track.find_clip(cid).cloned();
                        let position = track.clips.iter().position(|c| c.id == cid);
                        if let (Some(original), Some(pos)) = (original, position) {
                            if let Some((clip_a, clip_b)) = track.blade_at(cid, cut) {
                                self.state.history.push(EditCommand::BladeClip {
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
                            if let Some(media) = self.state.media_pool.get(&clip.media_id).cloned() {
                                let pos = track.clips.iter().position(|c| c.id == cid).unwrap_or(0);
                                let shifted: Vec<_> = track.clips[pos + 1..].iter()
                                    .map(|c| (c.id, c.timeline_in, c.timeline_out))
                                    .collect();
                                self.state.clipboard = Some(ClipboardContent {
                                    clips: vec![(clip.clone(), media.clone())],
                                    source_track_index: track_index,
                                });
                                track.ripple_remove(cid);
                                self.state.history.push(EditCommand::CutClip {
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
                            if let Some(media) = self.state.media_pool.get(&clip.media_id).cloned() {
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
                                let timeline_in = track.clips.last()
                                    .map(|c| c.timeline_out)
                                    .unwrap_or(Timecode::zero());
                                let dur = clip.duration();
                                let new_clip = klipp_timeline::Clip {
                                    id: new_id,
                                    media_id: clip.media_id,
                                    timeline_in,
                                    timeline_out: timeline_in + dur,
                                    source_in: clip.source_in,
                                    source_out: clip.source_out,
                                    color_grade: clip.color_grade,
                                    effects: clip.effects.clone(),
                                };
                                track.clips.push(new_clip.clone());
                                track.sort_clips();
                                self.state.history.push(EditCommand::PasteClip {
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
                AppCommand::InsertSourceAtPlayhead => {
                    if let Some(mid) = self.state.source_media_id {
                        if let Some(item) = self.state.media_pool.get(&mid).cloned() {
                            let source_in = item.default_source_in.unwrap_or(Timecode { value: 0, rate: item.duration.rate });
                            let source_out = item.default_source_out.unwrap_or(item.duration);
                            let dur = source_out - source_in;
                            if let Some(track) = self.state.sequence.video_track_mut() {
                                let new_id = klipp_core::ClipId::new();
                                let timeline_in = self.state.playhead;
                                let new_clip = klipp_timeline::Clip {
                                    id: new_id,
                                    media_id: mid,
                                    timeline_in,
                                    timeline_out: timeline_in + dur,
                                    source_in,
                                    source_out,
                                    color_grade: klipp_timeline::ClipColorGrade::default(),
                                    effects: Vec::new(),
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
                // Phase 4: Proxy
                AppCommand::GenerateProxy { media_id } => {
                    self.spawn_proxy_generation(*media_id);
                    continue;
                }
                AppCommand::SetUseProxies(enabled) => {
                    self.state.use_proxies = *enabled;
                    continue;
                }
                AppCommand::RelinkMedia { media_id, new_path } => {
                    if let Some(item) = self.state.media_pool.get_mut(media_id) {
                        item.path = new_path.clone();
                        item.offline = !new_path.exists();
                    }
                    self.state.offline_media_ids.retain(|id| id != media_id);
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
            EditCommand::AppendClip { clip, .. } => {
                if let Some(track) = self.state.sequence.video_track_mut() {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::RemoveClip { clip, track_index, position } => {
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, clip.clone());
                }
            }
            EditCommand::RippleRemove { removed_clip, track_index, position, shifted_clips } => {
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
            EditCommand::MoveClip { clip_id, old_timeline_in, old_timeline_out, .. } => {
                if let Some(track) = self.state.sequence.video_track_mut() {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.timeline_in = *old_timeline_in;
                        clip.timeline_out = *old_timeline_out;
                    }
                    track.sort_clips();
                }
            }
            EditCommand::TrimClip {
                clip_id,
                old_source_in, old_source_out,
                old_timeline_in, old_timeline_out, ..
            } => {
                if let Some(track) = self.state.sequence.video_track_mut() {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.source_in = *old_source_in;
                        clip.source_out = *old_source_out;
                        clip.timeline_in = *old_timeline_in;
                        clip.timeline_out = *old_timeline_out;
                    }
                }
            }
            EditCommand::BladeClip { track_index, original_clip, clip_a, clip_b, position } => {
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    // Undo: remove clip_a and clip_b, insert original
                    track.clips.retain(|c| c.id != clip_a.id && c.id != clip_b.id);
                    let pos = (*position).min(track.clips.len());
                    track.clips.insert(pos, original_clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::CutClip { clip, track_index, position, shifted_clips, .. } => {
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
            EditCommand::PasteClip { clip, track_index } => {
                // Undo paste = remove the pasted clip
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.remove_clip(clip.id);
                }
            }
        }
    }

    fn apply_edit_forward(&mut self, cmd: &EditCommand) {
        match cmd {
            EditCommand::AppendClip { clip, track_index } => {
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
            EditCommand::RemoveClip { clip, .. } => {
                if let Some(track) = self.state.sequence.video_track_mut() {
                    track.remove_clip(clip.id);
                }
            }
            EditCommand::RippleRemove { removed_clip, .. } => {
                if let Some(track) = self.state.sequence.video_track_mut() {
                    track.ripple_remove(removed_clip.id);
                }
            }
            EditCommand::MoveClip { clip_id, new_timeline_in, new_timeline_out, .. } => {
                if let Some(track) = self.state.sequence.video_track_mut() {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.timeline_in = *new_timeline_in;
                        clip.timeline_out = *new_timeline_out;
                    }
                    track.sort_clips();
                }
            }
            EditCommand::TrimClip {
                clip_id,
                new_source_in, new_source_out,
                new_timeline_in, new_timeline_out, ..
            } => {
                if let Some(track) = self.state.sequence.video_track_mut() {
                    if let Some(clip) = track.find_clip_mut(*clip_id) {
                        clip.source_in = *new_source_in;
                        clip.source_out = *new_source_out;
                        clip.timeline_in = *new_timeline_in;
                        clip.timeline_out = *new_timeline_out;
                    }
                }
            }
            EditCommand::BladeClip { track_index, original_clip, clip_a, clip_b, .. } => {
                // Redo blade: remove original, insert clip_a and clip_b
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.remove_clip(original_clip.id);
                    track.clips.push(clip_a.clone());
                    track.clips.push(clip_b.clone());
                    track.sort_clips();
                }
            }
            EditCommand::CutClip { clip, .. } => {
                // Redo cut = ripple remove
                if let Some(track) = self.state.sequence.video_track_mut() {
                    track.ripple_remove(clip.id);
                }
            }
            EditCommand::PasteClip { clip, track_index } => {
                // Redo paste = re-insert
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.clips.push(clip.clone());
                    track.sort_clips();
                }
            }
        }
    }

    fn do_save_project(&mut self, path: &std::path::Path) {
        match klipp_project::save_project(path, &self.state.sequence, &self.state.media_pool) {
            Ok(()) => {
                self.state.project_path = Some(path.to_path_buf());
                self.state.dirty = false;
                tracing::info!("Project saved to {:?}", path);
            }
            Err(e) => {
                tracing::error!("Failed to save project: {e}");
            }
        }
    }

    fn do_open_project(&mut self, path: &std::path::Path) {
        match klipp_project::load_project(path) {
            Ok((sequence, media_pool)) => {
                self.state.fps = sequence.fps;
                self.state.sequence = sequence;
                self.state.media_pool = media_pool;
                self.state.project_path = Some(path.to_path_buf());
                self.state.dirty = false;
                self.state.selected_clip = None;
                self.state.playback = PlaybackState::Idle;
                self.state.playhead = Timecode::zero();
                self.state.thumbnail_cache = klipp_media::ThumbnailCache::new();
                self.state.thumbnail_textures.clear();
                self.sync_sequence_to_engine();

                // Detect offline media
                self.state.offline_media_ids = self.state.media_pool.all()
                    .filter(|item| item.offline)
                    .map(|item| item.id)
                    .collect();
                if !self.state.offline_media_ids.is_empty() {
                    self.state.show_relink_dialog = true;
                }

                // Trigger thumbnail generation for all media
                let thumb_items: Vec<_> = self
                    .state
                    .media_pool
                    .all()
                    .filter(|item| !item.offline)
                    .map(|item| (item.id, item.path.clone(), item.duration))
                    .collect();
                for (id, path, duration) in thumb_items {
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
                    .pick_folder()
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
                if self.export_output_path.is_empty() {
                    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                    self.export_output_path = format!("{home}/output.mp4");
                }
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

            // Skip single-key shortcuts while typing in a text field
            if text_editing {
                return;
            }

            // Delete/Backspace — remove selected clip
            if i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RemoveClip { clip_id });
                }
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
            // Ctrl+B — Split at playhead
            if i.modifiers.ctrl && i.key_pressed(egui::Key::B) {
                // Find clip at playhead on primary video track
                let track_index = self.state.sequence.tracks.iter()
                    .position(|t| t.name == "Video 1")
                    .unwrap_or(0);
                if let Some(track) = self.state.sequence.tracks.get(track_index) {
                    if let Some((_, clip)) = track.clip_at_timecode(self.state.playhead) {
                        cmds.push(AppCommand::BladeClip {
                            track_index,
                            clip_id: clip.id,
                            cut_at: self.state.playhead,
                        });
                    }
                }
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
        });

        cmds
    }
}

impl Drop for KlippApp {
    fn drop(&mut self) {
        // Kill export child process
        if let Some(ref mut child) = self.export_child {
            let _ = child.kill();
        }
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
        self.drain_export_events();
        self.poll_render_queue();
        self.upload_frame_to_egui(ctx);

        // Continuous repaint during playback (must be AFTER drain_events so Playing state is set)
        if self.state.playback == PlaybackState::Playing {
            ctx.request_repaint();
        }

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

        // 4b. Export dialog
        let export_cmds = self.show_export_dialog(ctx);
        all_cmds.extend(export_cmds);

        // 4c. Render queue window
        self.show_render_queue_window(ctx);

        // Handle fullscreen toggle
        if self.state.fullscreen_viewer {
            // Fullscreen viewer: just show the video frame
            egui::CentralPanel::default().show(ctx, |ui| {
                let cmds = self.viewer.show_fullscreen(ui, &mut self.state);
                all_cmds.extend(cmds);
            });
        } else {
            // 5. Toolbar (top)
            egui::TopBottomPanel::top("toolbar")
                .exact_height(40.0)
                .show(ctx, |ui| {
                    ui.painter().rect_filled(
                        ui.available_rect_before_wrap(),
                        0.0,
                        egui::Color32::from_rgb(32, 32, 35),
                    );
                    let cmds = self.toolbar.show(ui, &mut self.state);
                    all_cmds.extend(cmds);
                });

            // 6. Timeline (bottom-most)
            let available_height = ctx.available_rect().height();
            let timeline_height = (available_height * (1.0 - self.state.upper_timeline_split))
                .max(180.0);

            egui::TopBottomPanel::bottom("timeline")
                .exact_height(timeline_height)
                .show(ctx, |ui| {
                    let cmds = self.timeline.show(ui, &mut self.state);
                    all_cmds.extend(cmds);
                });

            // 7. Transport bar (above timeline)
            egui::TopBottomPanel::bottom("transport")
                .exact_height(48.0)
                .show(ctx, |ui| {
                    ui.painter().rect_filled(
                        ui.available_rect_before_wrap(),
                        0.0,
                        egui::Color32::from_rgb(28, 28, 32),
                    );
                    let cmds = self.transport.show(ui, &mut self.state);
                    all_cmds.extend(cmds);
                });

            // 8. Inspector (right side panel)
            let inspector_width = ctx.available_rect().width()
                * (1.0 - self.state.viewer_inspector_split);
            egui::SidePanel::right("inspector")
                .exact_width(inspector_width)
                .resizable(true)
                .show(ctx, |ui| {
                    let cmds = self.inspector.show(ui, &self.state);
                    all_cmds.extend(cmds);
                });

            // 9. Viewer (central panel)
            egui::CentralPanel::default().show(ctx, |ui| {
                let cmds = self.viewer.show(ui, &self.state);
                all_cmds.extend(cmds);
            });
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

        // 11. Send all commands
        self.send_commands(all_cmds);

        // 11b. Handle fullscreen viewport transition (send only once)
        if self.state.fullscreen_changed {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(
                self.state.fullscreen_viewer,
            ));
            self.state.fullscreen_changed = false;
        }

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

        if self.state.playback == PlaybackState::Playing || self.state.export_in_progress {
            ctx.request_repaint();
        }
    }
}

impl KlippApp {
    fn drain_export_events(&mut self) {
        while let Ok(event) = self.export_event_rx.try_recv() {
            match event {
                AppEvent::ExportProgress { percent, .. } => {
                    self.state.export_progress = percent;
                }
                AppEvent::ExportComplete { path } => {
                    self.state.export_in_progress = false;
                    self.export_child = None;
                    tracing::info!("Export complete: {:?}", path);
                }
                AppEvent::ExportError { message } => {
                    self.state.export_in_progress = false;
                    self.state.export_error = Some(message);
                    self.export_child = None;
                }
                AppEvent::ProxyGenerationComplete { media_id, path } => {
                    if let Some(item) = self.state.media_pool.get_mut(&media_id) {
                        item.proxy_path = Some(path);
                        item.proxy_ready = true;
                    }
                }
                AppEvent::ProxyGenerationFailed { media_id, error } => {
                    tracing::warn!("Proxy generation failed for {:?}: {}", media_id, error);
                }
                _ => {}
            }
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
        let mut cmds = Vec::new();

        // Progress window
        if self.state.export_in_progress {
            egui::Window::new("Exporting...")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    let progress = self.state.export_progress / 100.0;
                    ui.add(egui::ProgressBar::new(progress).show_percentage());
                    ui.add_space(8.0);
                    if ui.button("Cancel").clicked() {
                        cmds.push(AppCommand::CancelExport);
                    }
                });
            return cmds;
        }

        // Error display
        if let Some(ref error) = self.state.export_error.clone() {
            let mut show = true;
            egui::Window::new("Export Error")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .open(&mut show)
                .show(ctx, |ui| {
                    ui.colored_label(egui::Color32::from_rgb(255, 100, 100), error);
                    if ui.button("OK").clicked() {
                        self.state.export_error = None;
                    }
                });
            if !show {
                self.state.export_error = None;
            }
            return cmds;
        }

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
                egui::Grid::new("export_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Output file:");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.export_output_path)
                                .desired_width(300.0));
                            if ui.button("...").clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_title("Export to...")
                                    .save_file()
                                {
                                    self.export_output_path = path.to_string_lossy().to_string();
                                }
                            }
                        });
                        ui.end_row();

                        ui.label("Format:");
                        egui::ComboBox::from_id_salt("export_format")
                            .selected_text(self.export_format.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_format, ExportFormat::Mp4, "MP4");
                                ui.selectable_value(&mut self.export_format, ExportFormat::Mkv, "MKV");
                                ui.selectable_value(&mut self.export_format, ExportFormat::Mov, "MOV");
                            });
                        ui.end_row();

                        ui.label("Video codec:");
                        egui::ComboBox::from_id_salt("export_vcodec")
                            .selected_text(self.export_video_codec.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_video_codec, ExportVideoCodec::H264, "H.264");
                                ui.selectable_value(&mut self.export_video_codec, ExportVideoCodec::H265, "H.265");
                                if self.export_format == ExportFormat::Mov {
                                    ui.selectable_value(&mut self.export_video_codec, ExportVideoCodec::ProRes, "ProRes");
                                }
                            });
                        ui.end_row();

                        ui.label("Quality (CRF):");
                        ui.add(egui::Slider::new(&mut self.export_crf, 0..=51));
                        ui.end_row();

                        ui.label("Audio codec:");
                        egui::ComboBox::from_id_salt("export_acodec")
                            .selected_text(self.export_audio_codec.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.export_audio_codec, ExportAudioCodec::Aac, "AAC");
                                ui.selectable_value(&mut self.export_audio_codec, ExportAudioCodec::Mp3, "MP3");
                                ui.selectable_value(&mut self.export_audio_codec, ExportAudioCodec::Flac, "FLAC");
                                ui.selectable_value(&mut self.export_audio_codec, ExportAudioCodec::Copy, "Copy Source");
                            });
                        ui.end_row();

                        ui.label("Audio bitrate:");
                        egui::ComboBox::from_id_salt("export_abitrate")
                            .selected_text(format!("{} kbps", self.export_audio_bitrate))
                            .show_ui(ui, |ui| {
                                for rate in [128, 192, 256, 320] {
                                    ui.selectable_value(&mut self.export_audio_bitrate, rate, format!("{rate} kbps"));
                                }
                            });
                        ui.end_row();
                    });

                // Export range checkbox
                if let (Some(in_pt), Some(out_pt)) = (self.state.sequence.in_point, self.state.sequence.out_point) {
                    let in_str = in_pt.to_timecode_string();
                    let out_str = out_pt.to_timecode_string();
                    ui.checkbox(
                        &mut self.export_marked_range,
                        format!("Export marked range only (In: {} \u{2192} Out: {})", in_str, out_str),
                    );
                }

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.state.show_export_dialog = false;
                    }
                    if ui.button("Add to Queue").clicked() {
                        let path = PathBuf::from(&self.export_output_path);
                        let settings = ExportSettings {
                            output_path: path.clone(),
                            format: self.export_format,
                            video_codec: self.export_video_codec,
                            resolution: None,
                            fps: None,
                            crf: self.export_crf,
                            audio_codec: self.export_audio_codec,
                            audio_bitrate_kbps: self.export_audio_bitrate,
                        };
                        let export_range = if self.export_marked_range {
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
                    }
                    if ui.button("Export").clicked() {
                        let path = PathBuf::from(&self.export_output_path);
                        if path.exists() {
                            self.export_confirm_overwrite = true;
                        } else {
                            self.state.show_export_dialog = false;
                            cmds.push(AppCommand::ExportSequence {
                                settings: ExportSettings {
                                    output_path: path,
                                    format: self.export_format,
                                    video_codec: self.export_video_codec,
                                    resolution: None,
                                    fps: None,
                                    crf: self.export_crf,
                                    audio_codec: self.export_audio_codec,
                                    audio_bitrate_kbps: self.export_audio_bitrate,
                                },
                            });
                        }
                    }
                });
            });

        if !open {
            self.state.show_export_dialog = false;
            self.export_confirm_overwrite = false;
        }

        // Overwrite confirmation dialog
        if self.export_confirm_overwrite {
            egui::Window::new("Overwrite File?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(format!(
                        "\"{}\" already exists. Overwrite?",
                        self.export_output_path
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.export_confirm_overwrite = false;
                        }
                        if ui.button("Overwrite").clicked() {
                            self.export_confirm_overwrite = false;
                            self.state.show_export_dialog = false;
                            cmds.push(AppCommand::ExportSequence {
                                settings: ExportSettings {
                                    output_path: PathBuf::from(&self.export_output_path),
                                    format: self.export_format,
                                    video_codec: self.export_video_codec,
                                    resolution: None,
                                    fps: None,
                                    crf: self.export_crf,
                                    audio_codec: self.export_audio_codec,
                                    audio_bitrate_kbps: self.export_audio_bitrate,
                                },
                            });
                        }
                    });
                });
        }

        cmds
    }

    fn start_export(&mut self, settings: ExportSettings) {
        // Validate output path
        if settings.output_path.as_os_str().is_empty() {
            self.state.export_error = Some("No output path specified.".into());
            return;
        }

        // Check ffmpeg availability
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            self.state.export_error = Some("ffmpeg not found. Install ffmpeg to export.".into());
            return;
        }

        // Determine export range
        let export_range = if self.export_marked_range {
            if let (Some(in_pt), Some(out_pt)) = (self.state.sequence.in_point, self.state.sequence.out_point) {
                Some((in_pt, out_pt))
            } else {
                None
            }
        } else {
            None
        };

        let total_duration = if let Some((in_pt, out_pt)) = export_range {
            (out_pt - in_pt).to_secs_f64()
        } else {
            self.state.sequence.total_duration().to_secs_f64()
        };
        let res = self.state.sequence.resolution;

        // Build ffmpeg command: use individual inputs + filter_complex to handle
        // mixed resolutions/codecs, rather than concat demuxer which can't cope.
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.arg("-y");

        // Add each clip as a separate input with seek/duration
        let mut clip_entries: Vec<(String, f64, f64)> = Vec::new();
        if let Some(track) = self.state.sequence.video_track() {
            for clip in &track.clips {
                // Apply range filtering: skip clips outside the marked range
                if let Some((range_in, range_out)) = export_range {
                    if clip.timeline_out <= range_in || clip.timeline_in >= range_out {
                        continue; // clip is entirely outside the range
                    }
                }

                if let Some(media) = self.state.media_pool.get(&clip.media_id) {
                    let path_str = media.path.to_string_lossy().to_string();

                    // Trim clip source in/out to fit within export range
                    let (src_in, src_out) = if let Some((range_in, range_out)) = export_range {
                        let clip_start = clip.timeline_in.max(range_in);
                        let clip_end = clip.timeline_out.min(range_out);
                        let offset_start = clip_start - clip.timeline_in;
                        let offset_end = clip_end - clip.timeline_in;
                        (clip.source_in + offset_start, clip.source_in + offset_end)
                    } else {
                        (clip.source_in, clip.source_out)
                    };

                    let in_secs = src_in.to_secs_f64();
                    let dur_secs = src_out.to_secs_f64() - in_secs;
                    clip_entries.push((path_str, in_secs, dur_secs));
                }
            }
        }

        for (path, ss, t) in &clip_entries {
            cmd.arg("-ss").arg(format!("{:.6}", ss));
            cmd.arg("-t").arg(format!("{:.6}", t));
            cmd.arg("-i").arg(path);
        }

        // Build filter_complex: scale all inputs to sequence resolution, then concat
        let n = clip_entries.len();
        let mut filter = String::new();
        for i in 0..n {
            filter.push_str(&format!(
                "[{i}:v]scale={w}:{h}:force_original_aspect_ratio=decrease,pad={w}:{h}:(ow-iw)/2:(oh-ih)/2,setsar=1[v{i}];",
                i = i, w = res.width, h = res.height
            ));
        }
        // Concat video streams
        for i in 0..n {
            filter.push_str(&format!("[v{i}]"));
        }
        filter.push_str(&format!("concat=n={n}:v=1:a=0[outv]"));

        cmd.arg("-filter_complex").arg(&filter);
        cmd.arg("-map").arg("[outv]");
        // Don't try to concat audio in filter_complex — it fails if any input lacks audio.
        // Instead, take audio from the first input that has it, or skip audio.
        cmd.arg("-map").arg("0:a?");

        cmd.arg("-c:v").arg(settings.video_codec.ffmpeg_codec())
            .arg("-crf").arg(settings.crf.to_string())
            .arg("-preset").arg("medium");

        for arg in settings.audio_codec.ffmpeg_args(settings.audio_bitrate_kbps) {
            cmd.arg(arg);
        }

        cmd.arg("-progress").arg("pipe:1")
            .arg(&settings.output_path)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        tracing::info!("Export ffmpeg command: {:?}", cmd);

        match cmd.spawn() {
            Ok(mut child) => {
                self.state.export_in_progress = true;
                self.state.export_progress = 0.0;

                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                let tx = self.export_event_tx.clone();
                let output_path = settings.output_path.clone();

                // Monitor progress in a thread
                std::thread::spawn(move || {
                    if let Some(stdout) = stdout {
                        use std::io::BufRead;
                        let reader = std::io::BufReader::new(stdout);
                        for line in reader.lines().map_while(Result::ok) {
                            if let Some(time_str) = line.strip_prefix("out_time_us=") {
                                if let Ok(us) = time_str.trim().parse::<i64>() {
                                    let secs = us as f64 / 1_000_000.0;
                                    let percent = if total_duration > 0.0 {
                                        (secs / total_duration * 100.0).min(100.0) as f32
                                    } else {
                                        0.0
                                    };
                                    let _ = tx.send(AppEvent::ExportProgress {
                                        percent,
                                        time_secs: secs,
                                    });
                                }
                            }
                        }
                    }

                    // Read stderr for error info
                    let stderr_text = stderr.map(|s| {
                        use std::io::Read;
                        let mut buf = String::new();
                        let mut reader = std::io::BufReader::new(s);
                        let _ = reader.read_to_string(&mut buf);
                        buf
                    }).unwrap_or_default();

                    // Wait for child (it may have already exited by now since stdout closed)
                    // The child handle was moved into the app struct, so we check stderr for errors
                    if !stderr_text.is_empty() {
                        // Log last few lines of stderr
                        let last_lines: String = stderr_text.lines().rev().take(10).collect::<Vec<_>>()
                            .into_iter().rev().collect::<Vec<_>>().join("\n");
                        tracing::info!("ffmpeg stderr (last lines):\n{}", last_lines);
                    }

                    let _ = tx.send(AppEvent::ExportComplete { path: output_path });
                });

                self.export_child = Some(child);
            }
            Err(e) => {
                self.state.export_error = Some(format!("Failed to start ffmpeg: {e}"));
            }
        }
    }

    fn cancel_export(&mut self) {
        if let Some(ref mut child) = self.export_child {
            let _ = child.kill();
        }
        self.export_child = None;
        self.state.export_in_progress = false;
        self.state.export_progress = 0.0;
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
