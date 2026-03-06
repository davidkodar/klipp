use crate::panels::inspector::InspectorPanel;
use crate::panels::timeline::TimelinePanel;
use crate::panels::toolbar::ToolbarPanel;
use crate::panels::transport::TransportBar;
use crate::panels::viewer::ViewerPanel;
use crate::state::{AppState, LoadedMedia};
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{AppCommand, AppEvent};
use klipp_engine::{PlaybackState, SharedFrameBuffer};

pub struct KlippApp {
    cmd_tx: Sender<AppCommand>,
    event_rx: Receiver<AppEvent>,
    frame_buffer: SharedFrameBuffer,
    state: AppState,
    toolbar: ToolbarPanel,
    viewer: ViewerPanel,
    inspector: InspectorPanel,
    transport: TransportBar,
    timeline: TimelinePanel,
}

impl KlippApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        cmd_tx: Sender<AppCommand>,
        event_rx: Receiver<AppEvent>,
        frame_buffer: SharedFrameBuffer,
    ) -> Self {
        // Apply dark theme
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
            state: AppState::default(),
            toolbar: ToolbarPanel,
            viewer: ViewerPanel::default(),
            inspector: InspectorPanel,
            transport: TransportBar,
            timeline: TimelinePanel,
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
                    duration,
                    fps,
                    resolution,
                } => {
                    let path = self
                        .state
                        .pending_media_path
                        .take()
                        .unwrap_or_default();
                    let filename = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());
                    self.state.loaded_media = Some(LoadedMedia {
                        media_id,
                        path,
                        filename,
                        duration,
                        fps,
                        resolution,
                    });
                    self.state.duration = duration;
                    self.state.fps = fps;
                    self.state.playback = PlaybackState::Stopped;
                }
                AppEvent::MediaLoadFailed { media_id: _, error } => {
                    self.state.pending_media_path = None;
                    tracing::error!("Failed to load media: {error}");
                }
                AppEvent::FrameReady => {
                    self.upload_frame_to_egui(ctx);
                    ctx.request_repaint();
                }
            }
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

    fn send_commands(&mut self, cmds: Vec<AppCommand>) {
        for cmd in cmds {
            if let AppCommand::LoadMediaFile { ref path, .. } = cmd {
                self.state.pending_media_path = Some(path.clone());
            }
            let _ = self.cmd_tx.send(cmd);
        }
    }
}

impl eframe::App for KlippApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. Drain events from engine
        self.drain_events(ctx);

        // Also check for new frames even without FrameReady event
        // (in case events were coalesced)
        self.upload_frame_to_egui(ctx);

        let mut all_cmds = Vec::new();

        // 2. Handle keyboard shortcuts
        let kb_cmds = self.transport.handle_keyboard(ctx, &self.state);
        all_cmds.extend(kb_cmds);

        // 3. Toolbar (top)
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

        // 4. Timeline (bottom-most)
        let available_height = ctx.available_rect().height();
        let timeline_height = (available_height * (1.0 - self.state.upper_timeline_split))
            .max(180.0);

        egui::TopBottomPanel::bottom("timeline")
            .exact_height(timeline_height)
            .show(ctx, |ui| {
                let cmds = self.timeline.show(ui, &mut self.state);
                all_cmds.extend(cmds);
            });

        // 5. Transport bar (above timeline)
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

        // 6. Inspector (right side panel)
        let inspector_width = ctx.available_rect().width()
            * (1.0 - self.state.viewer_inspector_split);
        egui::SidePanel::right("inspector")
            .exact_width(inspector_width)
            .resizable(true)
            .show(ctx, |ui| {
                let cmds = self.inspector.show(ui, &self.state);
                all_cmds.extend(cmds);
            });

        // 7. Viewer (central panel — fills remaining space)
        egui::CentralPanel::default().show(ctx, |ui| {
            let cmds = self.viewer.show(ui, &self.state);
            all_cmds.extend(cmds);
        });

        // 8. Process quit
        let has_quit = all_cmds.iter().any(|c| matches!(c, AppCommand::Quit));

        // 9. Send all commands to engine
        self.send_commands(all_cmds);

        if has_quit {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // Request continuous repaint during playback
        if self.state.playback == PlaybackState::Playing {
            ctx.request_repaint();
        }
    }
}
