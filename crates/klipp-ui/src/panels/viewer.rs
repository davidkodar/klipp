use crate::state::AppState;
use klipp_core::AppCommand;
use klipp_engine::{PlaybackState, SharedFrameTexture};

#[derive(Default)]
pub struct ViewerPanel {
    pub video_texture_id: Option<egui::TextureId>,
}

impl ViewerPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        frame_texture: &SharedFrameTexture,
    ) -> Vec<AppCommand> {
        let available = ui.available_size();

        if state.loaded_media.is_none() || state.playback == PlaybackState::Idle {
            // Idle state — dark background with hint text
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
            return Vec::new();
        }

        // We have media loaded — render the frame
        let ft = frame_texture.lock().unwrap();
        let video_w = ft.width as f32;
        let video_h = ft.height as f32;

        if video_w == 0.0 || video_h == 0.0 {
            drop(ft);
            return Vec::new();
        }

        // Calculate letterboxed rect
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

        // Fill background black
        let bg_resp = ui.allocate_rect(panel_rect, egui::Sense::hover());
        ui.painter()
            .rect_filled(bg_resp.rect, 0.0, egui::Color32::BLACK);

        drop(ft);

        // Draw the video texture if we have one registered
        if let Some(tex_id) = self.video_texture_id {
            ui.painter().image(
                tex_id,
                image_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }

        // Timecode overlay — bottom-left of image rect
        let tc_text = state.playhead.format_hmsf(&state.fps);
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

        // Resolution overlay — bottom-right
        if let Some(media) = &state.loaded_media {
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

        Vec::new()
    }
}
