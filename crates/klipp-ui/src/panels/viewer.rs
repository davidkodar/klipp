use crate::state::AppState;
use klipp_core::{AppCommand, ViewerMode};
use klipp_engine::PlaybackState;

#[derive(Default)]
pub struct ViewerPanel {
    pub video_texture: Option<egui::TextureHandle>,
}

impl ViewerPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Viewer tabs
        ui.horizontal(|ui| {
            let source_color = if state.viewer_mode == ViewerMode::Source {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_rgb(100, 100, 110)
            };
            let program_color = if state.viewer_mode == ViewerMode::Program {
                egui::Color32::WHITE
            } else {
                egui::Color32::from_rgb(100, 100, 110)
            };

            if ui.add(egui::Button::new(
                egui::RichText::new("Source").color(source_color).size(11.0)
            ).frame(false)).clicked() {
                cmds.push(AppCommand::SetViewerMode(ViewerMode::Source));
            }
            if ui.add(egui::Button::new(
                egui::RichText::new("Program").color(program_color).size(11.0)
            ).frame(false)).clicked() {
                cmds.push(AppCommand::SetViewerMode(ViewerMode::Program));
            }
        });

        ui.separator();

        match state.viewer_mode {
            ViewerMode::Program => {
                self.show_program_viewer(ui, state, &mut cmds);
            }
            ViewerMode::Source => {
                self.show_source_viewer(ui, state, &mut cmds);
            }
        }

        cmds
    }

    pub fn show_fullscreen(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let available = ui.available_size();

        let panel_rect = egui::Rect::from_min_size(ui.cursor().min, available);
        let resp = ui.allocate_rect(panel_rect, egui::Sense::hover());
        ui.painter().rect_filled(panel_rect, 0.0, egui::Color32::BLACK);

        if let Some(tex) = &self.video_texture {
            let tex_size = tex.size_vec2();
            if tex_size.x > 0.0 && tex_size.y > 0.0 {
                let video_aspect = tex_size.x / tex_size.y;
                let panel_aspect = available.x / available.y;
                let (dw, dh) = if video_aspect > panel_aspect {
                    (available.x, available.x / video_aspect)
                } else {
                    (available.y * video_aspect, available.y)
                };
                let image_rect = egui::Rect::from_center_size(
                    panel_rect.center(),
                    egui::vec2(dw, dh),
                );
                ui.painter().image(
                    tex.id(),
                    image_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            }
        }

        // Mouse movement shows overlay, timer hides it after 3 seconds
        let dt = ui.input(|i| i.unstable_dt) as f64;
        if resp.hovered() && ui.input(|i| i.pointer.velocity().length() > 1.0) {
            state.fullscreen_overlay_visible = true;
            state.fullscreen_overlay_timer = 3.0;
        }
        if state.fullscreen_overlay_timer > 0.0 {
            state.fullscreen_overlay_timer -= dt;
            if state.fullscreen_overlay_timer <= 0.0 {
                state.fullscreen_overlay_visible = false;
                state.fullscreen_overlay_timer = 0.0;
            }
        }

        // Transport overlay (bottom bar)
        if state.fullscreen_overlay_visible {
            let bar_h = 48.0;
            let bar_rect = egui::Rect::from_min_size(
                egui::pos2(panel_rect.left(), panel_rect.bottom() - bar_h),
                egui::vec2(panel_rect.width(), bar_h),
            );
            ui.painter().rect_filled(
                bar_rect,
                0.0,
                egui::Color32::from_rgba_premultiplied(0, 0, 0, 180),
            );

            // Timecode
            let tc_text = state.playhead.to_timecode_string();
            ui.painter().text(
                egui::pos2(bar_rect.left() + 16.0, bar_rect.center().y),
                egui::Align2::LEFT_CENTER,
                &tc_text,
                egui::FontId::monospace(16.0),
                egui::Color32::from_rgba_premultiplied(255, 255, 255, 220),
            );

            // Playback state
            let playback_text = match state.playback {
                PlaybackState::Playing => "\u{25B6} Playing",
                PlaybackState::Paused => "\u{23F8} Paused",
                PlaybackState::Idle | PlaybackState::Stopped => "\u{23F9} Idle",
            };
            ui.painter().text(
                egui::pos2(bar_rect.right() - 16.0, bar_rect.center().y),
                egui::Align2::RIGHT_CENTER,
                playback_text,
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgba_premultiplied(200, 200, 210, 200),
            );

            ui.ctx().request_repaint();
        }

        // Escape exits fullscreen (F is handled by global shortcuts)
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            cmds.push(AppCommand::ToggleFullscreen);
        }

        // Always request repaint during fullscreen so playback frames update
        if state.playback == PlaybackState::Playing {
            ui.ctx().request_repaint();
        }

        cmds
    }

    fn show_program_viewer(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        _cmds: &mut Vec<AppCommand>,
    ) {
        let available = ui.available_size();

        let current_media = state
            .sequence
            .video_track()
            .and_then(|t| t.clip_at_timecode(state.playhead))
            .and_then(|(_, clip)| state.media_pool.get(&clip.media_id));

        let has_content = state.playback != PlaybackState::Idle || current_media.is_some();

        if !has_content {
            let rect = ui.allocate_rect(
                egui::Rect::from_min_size(ui.cursor().min, available),
                egui::Sense::hover(),
            );
            ui.painter().rect_filled(
                rect.rect,
                0.0,
                egui::Color32::from_rgb(15, 15, 18),
            );
            ui.painter().text(
                rect.rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop a video file or use File > Open",
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(80, 80, 90),
            );
            return;
        }

        let (video_w, video_h) = current_media
            .or_else(|| state.media_pool.all().next())
            .map(|m| (m.resolution.width as f32, m.resolution.height as f32))
            .unwrap_or((1920.0, 1080.0));

        if video_w == 0.0 || video_h == 0.0 {
            return;
        }

        let video_aspect = video_w / video_h;
        let panel_aspect = available.x / available.y;
        let (display_w, display_h) = if video_aspect > panel_aspect {
            (available.x, available.x / video_aspect)
        } else {
            (available.y * video_aspect, available.y)
        };

        let panel_rect = egui::Rect::from_min_size(ui.cursor().min, available);
        let image_rect = egui::Rect::from_center_size(
            panel_rect.center(),
            egui::vec2(display_w, display_h),
        );

        let bg_resp = ui.allocate_rect(panel_rect, egui::Sense::hover());
        ui.painter()
            .rect_filled(bg_resp.rect, 0.0, egui::Color32::BLACK);

        if let Some(tex) = &self.video_texture {
            ui.painter().image(
                tex.id(),
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        // Timecode overlay
        let tc_text = state.playhead.to_timecode_string();
        let tc_font = egui::FontId::monospace(13.0);
        let tc_galley =
            ui.painter()
                .layout_no_wrap(tc_text, tc_font.clone(), egui::Color32::WHITE);
        let tc_size = tc_galley.size();
        let tc_pos = egui::pos2(
            image_rect.left() + 4.0,
            image_rect.bottom() - tc_size.y - 4.0,
        );
        let tc_bg_rect = egui::Rect::from_min_size(
            egui::pos2(tc_pos.x - 4.0, tc_pos.y - 4.0),
            egui::vec2(tc_size.x + 8.0, tc_size.y + 8.0),
        );
        ui.painter().rect_filled(
            tc_bg_rect,
            0.0,
            egui::Color32::from_rgba_premultiplied(0, 0, 0, 160),
        );
        ui.painter().galley(tc_pos, tc_galley, egui::Color32::WHITE);

        // Resolution overlay
        if let Some(media) = current_media.or_else(|| state.media_pool.all().next()) {
            let res_text = format!(
                "{}x{}  {:.3} fps",
                media.resolution.width,
                media.resolution.height,
                state.fps.as_f64()
            );
            let res_font = egui::FontId::monospace(13.0);
            let res_galley =
                ui.painter()
                    .layout_no_wrap(res_text, res_font, egui::Color32::WHITE);
            let res_size = res_galley.size();
            let res_pos = egui::pos2(
                image_rect.right() - res_size.x - 4.0,
                image_rect.bottom() - res_size.y - 4.0,
            );
            let res_bg_rect = egui::Rect::from_min_size(
                egui::pos2(res_pos.x - 4.0, res_pos.y - 4.0),
                egui::vec2(res_size.x + 8.0, res_size.y + 8.0),
            );
            ui.painter().rect_filled(
                res_bg_rect,
                0.0,
                egui::Color32::from_rgba_premultiplied(0, 0, 0, 160),
            );
            ui.painter()
                .galley(res_pos, res_galley, egui::Color32::WHITE);
        }
    }

    fn show_source_viewer(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let available = ui.available_size();

        let media = state.source_media_id
            .and_then(|mid| state.media_pool.get(&mid));

        if let Some(media) = media {
            // Show media info and insert button
            let panel_rect = egui::Rect::from_min_size(ui.cursor().min, available);
            let bg = ui.allocate_rect(panel_rect, egui::Sense::hover());
            ui.painter().rect_filled(bg.rect, 0.0, egui::Color32::from_rgb(15, 15, 18));

            // Source info
            ui.painter().text(
                egui::pos2(panel_rect.center().x, panel_rect.center().y - 20.0),
                egui::Align2::CENTER_CENTER,
                format!("Source: {}", media.filename),
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(180, 180, 190),
            );

            // In/Out display
            let in_str = media.default_source_in
                .map(|t| t.to_timecode_string())
                .unwrap_or_else(|| "00:00:00:00".to_string());
            let out_str = media.default_source_out
                .map(|t| t.to_timecode_string())
                .unwrap_or_else(|| media.duration.to_timecode_string());
            ui.painter().text(
                egui::pos2(panel_rect.center().x, panel_rect.center().y + 10.0),
                egui::Align2::CENTER_CENTER,
                format!("In: {}  Out: {}", in_str, out_str),
                egui::FontId::monospace(12.0),
                egui::Color32::from_rgb(200, 160, 80),
            );

            // Insert button area
            let btn_rect = egui::Rect::from_min_size(
                egui::pos2(panel_rect.center().x - 80.0, panel_rect.center().y + 40.0),
                egui::vec2(160.0, 28.0),
            );
            let btn_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
            let fill = if btn_resp.hovered() {
                egui::Color32::from_rgb(60, 60, 70)
            } else {
                egui::Color32::from_rgb(45, 45, 55)
            };
            ui.painter().rect_filled(btn_rect, 4.0, fill);
            ui.painter().text(
                btn_rect.center(),
                egui::Align2::CENTER_CENTER,
                "\u{2190} Insert at Playhead",
                egui::FontId::proportional(12.0),
                egui::Color32::WHITE,
            );
            if btn_resp.clicked() {
                cmds.push(AppCommand::InsertSourceAtPlayhead);
            }
        } else {
            let panel_rect = egui::Rect::from_min_size(ui.cursor().min, available);
            let bg = ui.allocate_rect(panel_rect, egui::Sense::hover());
            ui.painter().rect_filled(bg.rect, 0.0, egui::Color32::from_rgb(15, 15, 18));
            ui.painter().text(
                panel_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Click a media item in the pool to load it here",
                egui::FontId::proportional(14.0),
                egui::Color32::from_rgb(80, 80, 90),
            );
        }
    }
}
