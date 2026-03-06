use crate::state::AppState;
use klipp_core::{AppCommand, Timecode};
use klipp_engine::PlaybackState;

const TRACK_HEADER_WIDTH: f32 = 160.0;
const RULER_HEIGHT: f32 = 24.0;
const TRACK_HEIGHT: f32 = 56.0;

pub struct TimelinePanel;

impl Default for TimelinePanel {
    fn default() -> Self {
        Self
    }
}

impl TimelinePanel {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &mut AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let available = ui.available_size();

        // Handle zoom with Ctrl+scroll
        let scroll_delta = ui.input(|i| i.smooth_scroll_delta);
        if ui.rect_contains_pointer(ui.available_rect_before_wrap()) {
            let ctrl = ui.input(|i| i.modifiers.ctrl);
            if ctrl {
                let zoom_delta = ui.input(|i| i.smooth_scroll_delta.y);
                if zoom_delta != 0.0 {
                    let factor = if zoom_delta > 0.0 { 1.2 } else { 1.0 / 1.2 };
                    state.timeline_zoom = (state.timeline_zoom * factor).clamp(10.0, 1000.0);
                }
            } else if scroll_delta.x.abs() > 0.0 || scroll_delta.y.abs() > 0.0 {
                // Horizontal scroll — use y as horizontal when no modifier
                state.timeline_scroll_x =
                    (state.timeline_scroll_x - scroll_delta.y).max(0.0);
            }
        }

        let panel_rect = egui::Rect::from_min_size(ui.cursor().min, available);
        let _alloc = ui.allocate_rect(panel_rect, egui::Sense::hover());

        let bg_painter = ui.painter_at(panel_rect);

        // Background
        bg_painter.rect_filled(panel_rect, 0.0, egui::Color32::from_rgb(25, 25, 30));

        // Track header area
        let header_rect = egui::Rect::from_min_size(
            panel_rect.min,
            egui::vec2(TRACK_HEADER_WIDTH, available.y),
        );
        bg_painter.rect_filled(header_rect, 0.0, egui::Color32::from_rgb(22, 22, 26));

        // Track label
        let track_label_pos = egui::pos2(
            header_rect.left() + 8.0,
            header_rect.top() + RULER_HEIGHT + TRACK_HEIGHT / 2.0,
        );
        bg_painter.text(
            track_label_pos,
            egui::Align2::LEFT_CENTER,
            "Video 1",
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgb(140, 140, 150),
        );

        // Timeline content area (right of header) — clipped so nothing bleeds
        // past the track header into the left edge
        let content_rect = egui::Rect::from_min_max(
            egui::pos2(panel_rect.left() + TRACK_HEADER_WIDTH, panel_rect.top()),
            panel_rect.max,
        );
        let painter = bg_painter.with_clip_rect(content_rect);

        // Time ruler
        let ruler_rect = egui::Rect::from_min_size(
            content_rect.min,
            egui::vec2(content_rect.width(), RULER_HEIGHT),
        );
        painter.rect_filled(ruler_rect, 0.0, egui::Color32::from_rgb(30, 30, 34));

        // Draw ruler ticks
        self.draw_ruler_ticks(&painter, &ruler_rect, state);

        // Ruler click to seek
        let ruler_response = ui.allocate_rect(ruler_rect, egui::Sense::click());
        if ruler_response.clicked() {
            if let Some(pos) = ruler_response.interact_pointer_pos {
                let x_in_content = pos.x - content_rect.left() + state.timeline_scroll_x;
                let secs = x_in_content / state.timeline_zoom;
                let tc = Timecode::from_secs(secs as f64, &state.fps);
                let clamped = Timecode(tc.0.max(0).min(state.duration.0));
                cmds.push(AppCommand::SeekTo(clamped));
            }
        }

        // Track row area
        let track_rect = egui::Rect::from_min_size(
            egui::pos2(content_rect.left(), content_rect.top() + RULER_HEIGHT),
            egui::vec2(content_rect.width(), TRACK_HEIGHT),
        );

        // Draw clip block if media is loaded
        if state.loaded_media.is_some() && state.playback != PlaybackState::Idle {
            let duration_secs = state.duration.to_secs(&state.fps) as f32;
            let clip_width = duration_secs * state.timeline_zoom;
            let clip_x = content_rect.left() - state.timeline_scroll_x;

            let clip_rect = egui::Rect::from_min_size(
                egui::pos2(clip_x, track_rect.top() + 2.0),
                egui::vec2(clip_width, TRACK_HEIGHT - 4.0),
            );

            // Clip within visible area
            let visible_clip = clip_rect.intersect(track_rect);
            if visible_clip.width() > 0.0 && visible_clip.height() > 0.0 {
                painter.rect(
                    clip_rect,
                    3.0,
                    egui::Color32::from_rgb(50, 90, 160),
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 120, 200)),
                );

                // Clip name label
                if clip_rect.width() > 40.0 {
                    if let Some(media) = &state.loaded_media {
                        let max_text_width = clip_rect.width() - 8.0;
                        let clip_name = &media.filename;
                        let text_pos = egui::pos2(
                            clip_rect.left() + 4.0,
                            clip_rect.center().y,
                        );
                        painter.text(
                            text_pos,
                            egui::Align2::LEFT_CENTER,
                            truncate_text(clip_name, max_text_width, 11.0),
                            egui::FontId::proportional(11.0),
                            egui::Color32::WHITE,
                        );
                    }
                }
            }
        }

        // Playhead line
        if state.playback != PlaybackState::Idle {
            let playhead_secs = state.playhead.to_secs(&state.fps) as f32;
            let playhead_x =
                content_rect.left() + playhead_secs * state.timeline_zoom - state.timeline_scroll_x;

            if playhead_x >= content_rect.left() && playhead_x <= content_rect.right() {
                painter.line_segment(
                    [
                        egui::pos2(playhead_x, content_rect.top()),
                        egui::pos2(playhead_x, content_rect.bottom()),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 80, 80)),
                );
            }
        }

        cmds
    }

    fn draw_ruler_ticks(
        &self,
        painter: &egui::Painter,
        ruler_rect: &egui::Rect,
        state: &AppState,
    ) {
        let zoom = state.timeline_zoom;
        let scroll = state.timeline_scroll_x;
        let width = ruler_rect.width();

        // Determine tick interval based on zoom
        let _secs_visible = width / zoom;
        let start_sec = (scroll / zoom).floor() as i64;
        let end_sec = ((scroll + width) / zoom).ceil() as i64 + 1;

        for sec in start_sec..end_sec {
            if sec < 0 {
                continue;
            }
            let x = ruler_rect.left() + sec as f32 * zoom - scroll;
            if x < ruler_rect.left() || x > ruler_rect.right() {
                continue;
            }

            // Second tick + label
            painter.line_segment(
                [
                    egui::pos2(x, ruler_rect.bottom() - 12.0),
                    egui::pos2(x, ruler_rect.bottom()),
                ],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 80, 90)),
            );

            let mins = sec / 60;
            let secs = sec % 60;
            let label = format!("{mins}:{secs:02}");
            painter.text(
                egui::pos2(x + 3.0, ruler_rect.top() + 4.0),
                egui::Align2::LEFT_TOP,
                label,
                egui::FontId::proportional(11.0),
                egui::Color32::from_rgb(140, 140, 150),
            );

            // Half-second ticks at higher zoom
            if zoom >= 100.0 {
                let half_x = x + zoom * 0.5;
                if half_x >= ruler_rect.left() && half_x <= ruler_rect.right() {
                    painter.line_segment(
                        [
                            egui::pos2(half_x, ruler_rect.bottom() - 6.0),
                            egui::pos2(half_x, ruler_rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 70)),
                    );
                }
            }

            // 10-frame ticks at very high zoom
            if zoom >= 200.0 {
                let fps = state.fps.as_f64() as f32;
                let frame_interval = 10.0 / fps;
                let mut t = frame_interval;
                while t < 1.0 {
                    let tick_x = x + t * zoom;
                    if tick_x >= ruler_rect.left() && tick_x <= ruler_rect.right() {
                        painter.line_segment(
                            [
                                egui::pos2(tick_x, ruler_rect.bottom() - 3.0),
                                egui::pos2(tick_x, ruler_rect.bottom()),
                            ],
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 60)),
                        );
                    }
                    t += frame_interval;
                }
            }
        }
    }
}

fn truncate_text(text: &str, max_width: f32, font_size: f32) -> String {
    // Rough estimate: ~0.6 * font_size per character for proportional font
    let char_width = font_size * 0.6;
    let max_chars = (max_width / char_width) as usize;
    if text.len() <= max_chars {
        text.to_string()
    } else if max_chars > 3 {
        format!("{}...", &text[..max_chars - 3])
    } else {
        text[..max_chars.min(text.len())].to_string()
    }
}
