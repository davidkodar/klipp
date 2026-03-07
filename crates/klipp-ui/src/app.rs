use crate::panels::inspector::InspectorPanel;
use crate::panels::timeline::TimelinePanel;
use crate::panels::toolbar::ToolbarPanel;
use crate::panels::transport::TransportBar;
use crate::panels::viewer::ViewerPanel;
use crate::state::{AppState, UnsavedAction};
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{AppCommand, AppEvent, MediaId, Timecode};
use klipp_engine::{PlaybackState, SharedFrameBuffer};
use klipp_media::{MediaItem, ThumbnailEntry};
use klipp_timeline::Sequence;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

struct ThumbnailResult {
    media_id: MediaId,
    rgba: Vec<u8>,
    width: u32,
    height: u32,
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

        Self {
            cmd_tx,
            event_rx,
            frame_buffer,
            sequence,
            media_pool_shared,
            state: AppState::default(),
            toolbar: ToolbarPanel,
            viewer: ViewerPanel::default(),
            inspector: InspectorPanel,
            transport: TransportBar,
            timeline: TimelinePanel::default(),
            thumb_tx,
            thumb_rx,
        }
    }

    fn drain_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::PlaybackStarted => {
                    self.state.playback = PlaybackState::Playing;
                }
                AppEvent::PlaybackPaused => {
                    self.state.playback = PlaybackState::Paused;
                }
                AppEvent::PlaybackStopped => {
                    self.state.playback = PlaybackState::Stopped;
                }
                AppEvent::PlayheadMoved(tc) => {
                    self.state.playhead = tc;
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

                    // Start thumbnail generation
                    self.spawn_thumbnail(media_id, path, duration, fps);
                }
                AppEvent::MediaLoadFailed { media_id: _, error } => {
                    self.state.pending_media_path = None;
                    tracing::error!("Failed to load media: {error}");
                }
                AppEvent::FrameReady => {
                    self.upload_frame_to_egui(ctx);
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
                AppEvent::ThumbnailReady { .. } => {
                    // Thumbnails come through thumb_rx channel instead
                }
            }
        }
    }

    fn spawn_thumbnail(
        &mut self,
        media_id: MediaId,
        path: PathBuf,
        duration: Timecode,
        fps: klipp_core::Fps,
    ) {
        self.state.thumbnail_cache.set_pending(media_id);
        let tx = self.thumb_tx.clone();
        let duration_secs = duration.to_secs(&fps);
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

    fn drain_thumbnails(&mut self) {
        while let Ok(result) = self.thumb_rx.try_recv() {
            self.state.thumbnail_cache.set_ready(
                result.media_id,
                result.rgba,
                result.width,
                result.height,
            );
        }
    }

    fn upload_frame_to_egui(&mut self, ctx: &egui::Context) {
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

        let pixels: Vec<egui::Color32> = fb
            .data
            .chunks_exact(4)
            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect();

        let image = egui::ColorImage {
            size: [w, h],
            pixels,
        };

        self.viewer.video_texture = Some(ctx.load_texture(
            "klipp-video-frame",
            image,
            egui::TextureOptions::LINEAR,
        ));

        fb.dirty = false;
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
                        let clip_id = self
                            .state
                            .sequence
                            .video_track_mut()
                            .unwrap()
                            .append_clip(*media_id, duration);
                        self.sync_sequence_to_engine();
                        self.state.selected_clip = Some(clip_id);
                        self.state.dirty = true;
                    }
                    continue;
                }
                AppCommand::RemoveClip { clip_id } => {
                    if let Some(track) = self.state.sequence.video_track_mut() {
                        track.remove_clip(*clip_id);
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
                            let dur = clip.duration();
                            clip.timeline_in = *new_timeline_in;
                            clip.timeline_out = *new_timeline_in + dur;
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
                } => {
                    if let Some(track) = self.state.sequence.video_track_mut() {
                        if let Some(clip) = track.find_clip_mut(*clip_id) {
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
                AppCommand::SelectClip(id) => {
                    self.state.selected_clip = *id;
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
        self.sync_sequence_to_engine();
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

                // Trigger thumbnail generation for all media
                let thumb_items: Vec<_> = self
                    .state
                    .media_pool
                    .all()
                    .filter(|item| !item.offline)
                    .map(|item| (item.id, item.path.clone(), item.duration, item.fps))
                    .collect();
                for (id, path, duration, fps) in thumb_items {
                    self.spawn_thumbnail(id, path, duration, fps);
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

        if ctx.memory(|m| m.focused().is_some()) {
            return cmds;
        }

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
            // Ctrl+Z — placeholder
            if i.modifiers.ctrl && i.key_pressed(egui::Key::Z) {
                tracing::info!("Undo not yet implemented");
            }
            // Delete/Backspace — remove selected clip
            if i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace) {
                if let Some(clip_id) = self.state.selected_clip {
                    cmds.push(AppCommand::RemoveClip { clip_id });
                }
            }
        });

        cmds
    }
}

impl eframe::App for KlippApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. Drain events from engine
        self.drain_events(ctx);
        self.drain_thumbnails();
        self.upload_frame_to_egui(ctx);

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

        // 5. Toolbar (top)
        egui::TopBottomPanel::top("toolbar")
            .exact_height(40.0)
            .show(ctx, |ui| {
                ui.painter().rect_filled(
                    ui.available_rect_before_wrap(),
                    0.0,
                    egui::Color32::from_rgb(32, 32, 35),
                );
                let cmds = self.toolbar.show(ui, &self.state);
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
                let cmds = self.transport.show(ui, &self.state);
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

        // 10. Process quit
        let has_quit = all_cmds.iter().any(|c| matches!(c, AppCommand::Quit));

        // 11. Send all commands
        self.send_commands(all_cmds);

        // 12. Update window title
        self.update_window_title(ctx);

        if has_quit {
            if self.state.dirty {
                self.state.show_unsaved_dialog = true;
                self.state.unsaved_dialog_action = UnsavedAction::Quit;
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        if self.state.playback == PlaybackState::Playing {
            ctx.request_repaint();
        }
    }
}

impl KlippApp {
    fn update_thumbnail_textures(&mut self, ctx: &egui::Context) {
        // Check for any ready thumbnails that don't have textures yet
        let media_ids: Vec<MediaId> = self.state.media_pool.all().map(|m| m.id).collect();
        for media_id in media_ids {
            if self.state.thumbnail_textures.contains_key(&media_id) {
                continue;
            }
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
