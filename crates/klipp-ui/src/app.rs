use crate::history::EditCommand;
use crate::panels::inspector::InspectorPanel;
use crate::panels::timeline::TimelinePanel;
use crate::panels::toolbar::ToolbarPanel;
use crate::panels::transport::TransportBar;
use crate::panels::viewer::ViewerPanel;
use crate::state::{AppState, UnsavedAction};
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{
    AppCommand, AppEvent, ExportAudioCodec, ExportFormat, ExportSettings,
    ExportVideoCodec, MediaId, Timecode,
};
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
    export_child: Option<std::process::Child>,
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
            export_child: None,
            export_event_tx,
            export_event_rx,
            export_output_path: String::new(),
            export_format: ExportFormat::Mp4,
            export_video_codec: ExportVideoCodec::H264,
            export_crf: 23,
            export_audio_codec: ExportAudioCodec::Aac,
            export_audio_bitrate: 128,
            export_confirm_overwrite: false,
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
                    self.spawn_thumbnail(media_id, path, duration);
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
                AppEvent::ExportProgress { .. }
                | AppEvent::ExportComplete { .. }
                | AppEvent::ExportError { .. } => {
                    // Export events come through export_event_rx channel
                }
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

        // Reuse existing texture if possible, otherwise create new
        if let Some(ref mut tex) = self.viewer.video_texture {
            tex.set(image, egui::TextureOptions::LINEAR);
        } else {
            self.viewer.video_texture = Some(ctx.load_texture(
                "klipp-video-frame",
                image,
                egui::TextureOptions::LINEAR,
            ));
        }

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
                        let track_index = self.state.sequence.tracks.iter()
                            .position(|t| t.kind == klipp_timeline::TrackKind::Video)
                            .unwrap_or(0);
                        let clip_id = self
                            .state
                            .sequence
                            .video_track_mut()
                            .unwrap()
                            .append_clip(*media_id, duration);
                        let clip = *self.state.sequence.video_track().unwrap()
                            .find_clip(clip_id).unwrap();
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
                    self.state.playback_speed = *speed;
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
                    track.clips.insert(pos, *clip);
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
                    track.clips.insert(pos, *removed_clip);
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
        }
    }

    fn apply_edit_forward(&mut self, cmd: &EditCommand) {
        match cmd {
            EditCommand::AppendClip { clip, track_index } => {
                if let Some(track) = self.state.sequence.tracks.get_mut(*track_index) {
                    track.clips.push(*clip);
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
        self.drain_export_events();
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

        // 4b. Export dialog
        let export_cmds = self.show_export_dialog(ctx);
        all_cmds.extend(export_cmds);

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
                _ => {}
            }
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

                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.state.show_export_dialog = false;
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

        // Build concat demuxer file
        let concat_path = std::env::temp_dir().join("klipp_export_concat.txt");
        let mut concat_content = String::new();
        if let Some(track) = self.state.sequence.video_track() {
            tracing::info!("Export: Video 1 track has {} clips", track.clips.len());
            for (i, clip) in track.clips.iter().enumerate() {
                if let Some(media) = self.state.media_pool.get(&clip.media_id) {
                    let path_str = media.path.to_string_lossy().replace('\'', "'\\''");
                    let in_secs = clip.source_in.to_secs_f64();
                    let out_secs = clip.source_out.to_secs_f64();
                    tracing::info!(
                        "Export clip {}: media={}, inpoint={:.6}, outpoint={:.6}",
                        i, path_str, in_secs, out_secs
                    );
                    concat_content.push_str(&format!(
                        "file '{}'\ninpoint {:.6}\noutpoint {:.6}\n",
                        path_str, in_secs, out_secs
                    ));
                } else {
                    tracing::warn!("Export clip {}: media_id {:?} not found in pool!", i, clip.media_id);
                }
            }
        } else {
            tracing::warn!("Export: no Video 1 track found!");
        }

        if concat_content.is_empty() {
            self.state.export_error = Some("No clips to export.".into());
            return;
        }

        if let Err(e) = std::fs::write(&concat_path, &concat_content) {
            self.state.export_error = Some(format!("Failed to write concat file: {e}"));
            return;
        }

        let total_duration = self.state.sequence.total_duration().to_secs_f64();
        let res = self.state.sequence.resolution;

        // Build ffmpeg command: use individual inputs + filter_complex to handle
        // mixed resolutions/codecs, rather than concat demuxer which can't cope.
        let mut cmd = std::process::Command::new("ffmpeg");
        cmd.arg("-y");

        // Add each clip as a separate input with seek/duration
        let mut clip_entries: Vec<(String, f64, f64)> = Vec::new();
        if let Some(track) = self.state.sequence.video_track() {
            for clip in &track.clips {
                if let Some(media) = self.state.media_pool.get(&clip.media_id) {
                    let path_str = media.path.to_string_lossy().to_string();
                    let in_secs = clip.source_in.to_secs_f64();
                    let dur_secs = clip.source_out.to_secs_f64() - in_secs;
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
