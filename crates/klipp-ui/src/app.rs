use crate::panels::inspector::InspectorPanel;
use crate::panels::timeline::TimelinePanel;
use crate::panels::toolbar::ToolbarPanel;
use crate::panels::transport::TransportBar;
use crate::panels::viewer::ViewerPanel;
use crate::state::{AppState, LoadedMedia};
use crossbeam_channel::{Receiver, Sender};
use klipp_core::{AppCommand, AppEvent};
use klipp_engine::{PlaybackState, SharedFrameTexture};

pub struct KlippApp {
    cmd_tx: Sender<AppCommand>,
    event_rx: Receiver<AppEvent>,
    frame_texture: SharedFrameTexture,
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
        frame_texture: SharedFrameTexture,
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
            frame_texture,
            state: AppState::default(),
            toolbar: ToolbarPanel::default(),
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
                    let filename = self
                        .toolbar
                        .open_path
                        .split('/')
                        .next_back()
                        .unwrap_or(&self.toolbar.open_path)
                        .to_string();
                    self.state.loaded_media = Some(LoadedMedia {
                        media_id,
                        path: std::path::PathBuf::from(&self.toolbar.open_path),
                        filename,
                        duration,
                        fps,
                        resolution,
                    });
                    self.state.duration = duration;
                    self.state.fps = fps;
                    self.state.playback = PlaybackState::Stopped;
                    self.toolbar.show_open_modal = false;
                    self.toolbar.open_error = None;
                }
                AppEvent::MediaLoadFailed { media_id: _, error } => {
                    self.toolbar.open_error = Some(error);
                }
                AppEvent::FrameReady => {
                    self.update_video_texture(ctx);
                    ctx.request_repaint();
                }
            }
        }
    }

    fn update_video_texture(&mut self, ctx: &egui::Context) {
        let mut ft = self.frame_texture.lock().unwrap();
        if !ft.dirty {
            return;
        }

        let w = ft.width as usize;
        let h = ft.height as usize;
        let rgba = &ft.rgba_data;

        if rgba.len() != w * h * 4 {
            ft.dirty = false;
            return;
        }

        let pixels: Vec<egui::Color32> = rgba
            .chunks_exact(4)
            .map(|p| egui::Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect();

        let image = egui::ColorImage {
            size: [w, h],
            pixels,
        };

        let tex_handle =
            ctx.load_texture("klipp-video-frame", image, egui::TextureOptions::LINEAR);
        self.viewer.video_texture_id = Some(tex_handle.id());

        ft.dirty = false;
    }

    fn send_commands(&self, cmds: Vec<AppCommand>) {
        for cmd in cmds {
            let _ = self.cmd_tx.send(cmd);
        }
    }
}

impl eframe::App for KlippApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. Drain events from engine
        self.drain_events(ctx);

        let mut all_cmds = Vec::new();

        // 2. Handle keyboard shortcuts
        let kb_cmds = self.transport.handle_keyboard(ctx, &self.state);
        all_cmds.extend(kb_cmds);

        // 3. Draw toolbar (top panel)
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

        // Open file modal (drawn on top of everything)
        let modal_cmds = self.toolbar.show_open_modal(ctx);
        all_cmds.extend(modal_cmds);

        // 4. Draw transport bar (bottom-ish panel)
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

        // 5. Draw timeline (bottom panel — takes remaining bottom space)
        let available_height = ctx.available_rect().height();
        let timeline_height = (available_height * (1.0 - self.state.upper_timeline_split))
            .max(180.0);

        egui::TopBottomPanel::bottom("timeline")
            .exact_height(timeline_height)
            .show(ctx, |ui| {
                let cmds = self.timeline.show(ui, &mut self.state);
                all_cmds.extend(cmds);
            });

        // 6. Draw viewer + inspector (central area, side by side)
        egui::CentralPanel::default().show(ctx, |ui| {
            let total_width = ui.available_width();
            let viewer_width = total_width * self.state.viewer_inspector_split;
            let inspector_width = total_width - viewer_width;

            ui.horizontal(|ui| {
                // Viewer
                ui.allocate_ui(
                    egui::vec2(viewer_width, ui.available_height()),
                    |ui| {
                        let ft = &self.frame_texture;
                        let cmds = self.viewer.show(ui, &self.state, ft);
                        all_cmds.extend(cmds);
                    },
                );

                // Separator / drag handle
                ui.separator();

                // Inspector
                ui.allocate_ui(
                    egui::vec2(inspector_width - 8.0, ui.available_height()),
                    |ui| {
                        let cmds = self.inspector.show(ui, &self.state);
                        all_cmds.extend(cmds);
                    },
                );
            });
        });

        // 7. Process quit
        let has_quit = all_cmds.iter().any(|c| matches!(c, AppCommand::Quit));

        // 8. Send all commands to engine
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
