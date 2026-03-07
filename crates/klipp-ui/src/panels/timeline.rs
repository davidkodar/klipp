use crate::state::AppState;
use klipp_core::{AppCommand, ClipId, Timecode};
use klipp_engine::PlaybackState;

const TRACK_HEADER_WIDTH: f32 = 160.0;
const RULER_HEIGHT: f32 = 24.0;
const TRACK_HEIGHT: f32 = 56.0;
const MEDIA_POOL_HEIGHT: f32 = 50.0;
const TRIM_HANDLE_WIDTH: f32 = 8.0;

#[derive(Default)]
pub struct TimelinePanel {
    dragging_clip: Option<DragState>,
    trimming_clip: Option<TrimState>,
}

struct DragState {
    clip_id: ClipId,
    start_x: f32,
    original_timeline_in: Timecode,
}

struct TrimState {
    clip_id: ClipId,
    edge: TrimEdge,
    start_x: f32,
    original_source_in: Timecode,
    original_source_out: Timecode,
    original_timeline_in: Timecode,
    original_timeline_out: Timecode,
}

#[derive(Clone, Copy)]
enum TrimEdge {
    Left,
    Right,
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
            header_rect.top() + RULER_HEIGHT + MEDIA_POOL_HEIGHT + TRACK_HEIGHT / 2.0,
        );
        bg_painter.text(
            track_label_pos,
            egui::Align2::LEFT_CENTER,
            "Video 1",
            egui::FontId::proportional(12.0),
            egui::Color32::from_rgb(140, 140, 150),
        );

        // Timeline content area
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
        self.draw_ruler_ticks(&painter, &ruler_rect, state);

        // Ruler click to seek
        let ruler_response = ui.allocate_rect(ruler_rect, egui::Sense::click());
        if ruler_response.clicked() {
            if let Some(pos) = ruler_response.interact_pointer_pos {
                let x_in_content = pos.x - content_rect.left() + state.timeline_scroll_x;
                let secs = x_in_content / state.timeline_zoom;
                let tc = Timecode::from_secs(secs as f64, &state.fps);
                let dur = state.sequence_duration();
                let clamped = Timecode(tc.0.max(0).min(dur.0));
                cmds.push(AppCommand::SeekTo(clamped));
            }
        }

        // Media pool bar (below ruler, above track)
        let pool_rect = egui::Rect::from_min_size(
            egui::pos2(content_rect.left(), content_rect.top() + RULER_HEIGHT),
            egui::vec2(content_rect.width(), MEDIA_POOL_HEIGHT),
        );
        painter.rect_filled(pool_rect, 0.0, egui::Color32::from_rgb(20, 20, 24));
        self.draw_media_pool(ui, &painter, &pool_rect, state, &mut cmds);

        // Track row area
        let track_y = content_rect.top() + RULER_HEIGHT + MEDIA_POOL_HEIGHT;
        let track_rect = egui::Rect::from_min_size(
            egui::pos2(content_rect.left(), track_y),
            egui::vec2(content_rect.width(), TRACK_HEIGHT),
        );

        // Draw clips
        self.draw_clips(ui, &painter, &track_rect, &content_rect, state, &mut cmds);

        // Playhead line
        let seq_dur = state.sequence_duration();
        if state.playback != PlaybackState::Idle || seq_dur.0 > 0 {
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

    fn draw_clips(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        track_rect: &egui::Rect,
        content_rect: &egui::Rect,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let clips: Vec<_> = state
            .sequence
            .video_track()
            .map(|t| t.clips.clone())
            .unwrap_or_default();

        let zoom = state.timeline_zoom;
        let scroll = state.timeline_scroll_x;
        let fps = state.fps;

        // Track the pointer position for interactions
        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        let pointer_released = ui.input(|i| i.pointer.primary_released());

        for clip in &clips {
            let clip_x =
                content_rect.left() + clip.timeline_in.to_secs(&fps) as f32 * zoom - scroll;
            let clip_w = clip.duration().to_secs(&fps) as f32 * zoom;

            let clip_rect = egui::Rect::from_min_size(
                egui::pos2(clip_x, track_rect.top() + 2.0),
                egui::vec2(clip_w, TRACK_HEIGHT - 4.0),
            );

            // Skip if not visible
            if clip_rect.right() < content_rect.left()
                || clip_rect.left() > content_rect.right()
            {
                continue;
            }

            let is_selected = state.selected_clip == Some(clip.id);

            // Determine fill and border based on selection
            let (fill, border_color, border_width) = if is_selected {
                (
                    egui::Color32::from_rgb(70, 120, 200),
                    egui::Color32::from_rgb(120, 160, 240),
                    2.0,
                )
            } else {
                (
                    egui::Color32::from_rgb(50, 90, 160),
                    egui::Color32::from_rgb(80, 120, 200),
                    1.0,
                )
            };

            // Draw thumbnail background if available
            if let Some(tex) = state.thumbnail_textures.get(&clip.media_id) {
                painter.image(
                    tex.id(),
                    clip_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                // Color overlay at 40% opacity
                painter.rect_filled(
                    clip_rect,
                    3.0,
                    egui::Color32::from_rgba_premultiplied(50, 90, 160, 102),
                );
            } else {
                painter.rect_filled(clip_rect, 3.0, fill);
            }

            // Border
            painter.rect_stroke(
                clip_rect,
                3.0,
                egui::Stroke::new(border_width, border_color),
            );

            // Clip name label
            if clip_rect.width() > 40.0 {
                if let Some(media) = state.media_pool.get(&clip.media_id) {
                    let max_text_width = clip_rect.width() - 8.0;
                    let text_pos =
                        egui::pos2(clip_rect.left() + 4.0, clip_rect.center().y);
                    painter.text(
                        text_pos,
                        egui::Align2::LEFT_CENTER,
                        truncate_text(&media.filename, max_text_width, 11.0),
                        egui::FontId::proportional(11.0),
                        egui::Color32::WHITE,
                    );
                }
            }

            // Interaction: clip selection, drag, trim
            let clip_response =
                ui.allocate_rect(clip_rect, egui::Sense::click_and_drag());

            // Right-click context menu
            clip_response.context_menu(|ui| {
                if ui.button("Remove Clip").clicked() {
                    cmds.push(AppCommand::RemoveClip { clip_id: clip.id });
                    ui.close_menu();
                }
                ui.separator();
                let r = ui.add_enabled(false, egui::Button::new("Properties..."));
                r.on_disabled_hover_text("(coming soon)");
            });

            if clip_response.clicked() {
                cmds.push(AppCommand::SelectClip(Some(clip.id)));
            }

            // Check for trim handle hover
            if let Some(pos) = pointer_pos {
                if clip_rect.contains(pos) {
                    let near_left = (pos.x - clip_rect.left()).abs() < TRIM_HANDLE_WIDTH;
                    let near_right = (pos.x - clip_rect.right()).abs() < TRIM_HANDLE_WIDTH;

                    if near_left || near_right {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }

                    // Start trimming
                    if clip_response.drag_started() {
                        if near_left {
                            self.trimming_clip = Some(TrimState {
                                clip_id: clip.id,
                                edge: TrimEdge::Left,
                                start_x: pos.x,
                                original_source_in: clip.source_in,
                                original_source_out: clip.source_out,
                                original_timeline_in: clip.timeline_in,
                                original_timeline_out: clip.timeline_out,
                            });
                        } else if near_right {
                            self.trimming_clip = Some(TrimState {
                                clip_id: clip.id,
                                edge: TrimEdge::Right,
                                start_x: pos.x,
                                original_source_in: clip.source_in,
                                original_source_out: clip.source_out,
                                original_timeline_in: clip.timeline_in,
                                original_timeline_out: clip.timeline_out,
                            });
                        } else {
                            // Start drag to reorder
                            self.dragging_clip = Some(DragState {
                                clip_id: clip.id,
                                start_x: pos.x,
                                original_timeline_in: clip.timeline_in,
                            });
                        }
                    }
                }
            }
        }

        // Handle ongoing trim
        if let Some(ref trim) = self.trimming_clip {
            if let Some(pos) = pointer_pos {
                if pointer_down {
                    let dx = pos.x - trim.start_x;
                    let dt_secs = dx as f64 / zoom as f64;
                    let dt_frames = Timecode::from_secs(dt_secs, &fps);

                    let media_duration = state
                        .media_pool
                        .get(
                            &state
                                .sequence
                                .video_track()
                                .and_then(|t| t.find_clip(trim.clip_id))
                                .map(|c| c.media_id)
                                .unwrap_or(klipp_core::MediaId::nil()),
                        )
                        .map(|m| m.duration)
                        .unwrap_or(Timecode(i64::MAX));

                    match trim.edge {
                        TrimEdge::Left => {
                            let new_source_in = Timecode(
                                (trim.original_source_in.0 + dt_frames.0)
                                    .max(0)
                                    .min(trim.original_source_out.0 - 1),
                            );
                            let delta = Timecode(new_source_in.0 - trim.original_source_in.0);
                            let new_timeline_in = trim.original_timeline_in + delta;

                            if let Some(track) = state.sequence.video_track_mut() {
                                if let Some(clip) = track.find_clip_mut(trim.clip_id) {
                                    clip.source_in = new_source_in;
                                    clip.timeline_in = new_timeline_in;
                                }
                            }

                            // Tooltip
                            let tc_str = new_source_in.format_hmsf(&fps);
                            egui::show_tooltip_at_pointer(
                                ui.ctx(),
                                ui.layer_id(),
                                ui.id().with("trim_tooltip"),
                                |ui: &mut egui::Ui| {
                                    ui.label(format!("In: {tc_str}"));
                                },
                            );
                        }
                        TrimEdge::Right => {
                            let new_source_out = Timecode(
                                (trim.original_source_out.0 + dt_frames.0)
                                    .max(trim.original_source_in.0 + 1)
                                    .min(media_duration.0),
                            );
                            let delta =
                                Timecode(new_source_out.0 - trim.original_source_out.0);
                            let new_timeline_out = trim.original_timeline_out + delta;

                            if let Some(track) = state.sequence.video_track_mut() {
                                if let Some(clip) = track.find_clip_mut(trim.clip_id) {
                                    clip.source_out = new_source_out;
                                    clip.timeline_out = new_timeline_out;
                                }
                            }

                            let tc_str = new_source_out.format_hmsf(&fps);
                            egui::show_tooltip_at_pointer(
                                ui.ctx(),
                                ui.layer_id(),
                                ui.id().with("trim_tooltip"),
                                |ui: &mut egui::Ui| {
                                    ui.label(format!("Out: {tc_str}"));
                                },
                            );
                        }
                    }
                }
            }

            if pointer_released {
                if let Some(track) = state.sequence.video_track() {
                    if let Some(clip) = track.find_clip(trim.clip_id) {
                        cmds.push(AppCommand::TrimClip {
                            clip_id: trim.clip_id,
                            new_source_in: clip.source_in,
                            new_source_out: clip.source_out,
                            new_timeline_in: clip.timeline_in,
                            new_timeline_out: clip.timeline_out,
                        });
                    }
                }
                self.trimming_clip = None;
            }
        }

        // Handle ongoing drag
        if let Some(ref drag) = self.dragging_clip {
            if let Some(pos) = pointer_pos {
                if pointer_down {
                    let dx = pos.x - drag.start_x;
                    let dt_secs = dx as f64 / zoom as f64;
                    let dt_frames = Timecode::from_secs(dt_secs, &fps);
                    let new_timeline_in =
                        Timecode((drag.original_timeline_in.0 + dt_frames.0).max(0));

                    // Check for overlap with other clips (snap within 8px)
                    let snap_threshold_secs = 8.0 / zoom as f64;
                    let snap_threshold = Timecode::from_secs(snap_threshold_secs, &fps);

                    let mut final_in = new_timeline_in;
                    if let Some(track) = state.sequence.video_track() {
                        if let Some(clip) = track.find_clip(drag.clip_id) {
                            let dur = clip.duration();
                            let new_out = new_timeline_in + dur;

                            for other in &track.clips {
                                if other.id == drag.clip_id {
                                    continue;
                                }
                                // Snap to adjacent clips
                                let gap_right =
                                    (new_out.0 - other.timeline_in.0).abs();
                                let gap_left =
                                    (new_timeline_in.0 - other.timeline_out.0).abs();

                                if gap_right < snap_threshold.0 {
                                    final_in =
                                        Timecode(other.timeline_in.0 - dur.0);
                                }
                                if gap_left < snap_threshold.0 {
                                    final_in = other.timeline_out;
                                }

                                // Prevent overlap
                                let would_overlap = final_in < other.timeline_out
                                    && final_in + dur > other.timeline_in;
                                if would_overlap && other.id != drag.clip_id {
                                    // Don't move
                                    final_in = drag.original_timeline_in;
                                }
                            }
                        }
                    }

                    if let Some(track) = state.sequence.video_track_mut() {
                        if let Some(clip) = track.find_clip_mut(drag.clip_id) {
                            let dur = clip.duration();
                            clip.timeline_in = Timecode(final_in.0.max(0));
                            clip.timeline_out = clip.timeline_in + dur;
                        }
                    }
                }
            }

            if pointer_released {
                let clip_id = drag.clip_id;
                if let Some(track) = state.sequence.video_track() {
                    if let Some(clip) = track.find_clip(clip_id) {
                        cmds.push(AppCommand::MoveClip {
                            clip_id,
                            new_timeline_in: clip.timeline_in,
                        });
                    }
                }
                self.dragging_clip = None;
            }
        }

        // Click on empty track area to deselect
        if let Some(pos) = pointer_pos {
            if track_rect.contains(pos) && pointer_released {
                let on_clip = clips.iter().any(|c| {
                    let cx = content_rect.left()
                        + c.timeline_in.to_secs(&fps) as f32 * zoom
                        - scroll;
                    let cw = c.duration().to_secs(&fps) as f32 * zoom;
                    let cr = egui::Rect::from_min_size(
                        egui::pos2(cx, track_rect.top() + 2.0),
                        egui::vec2(cw, TRACK_HEIGHT - 4.0),
                    );
                    cr.contains(pos)
                });
                if !on_clip
                    && self.dragging_clip.is_none()
                    && self.trimming_clip.is_none()
                {
                    cmds.push(AppCommand::SelectClip(None));
                }
            }
        }
    }

    fn draw_media_pool(
        &self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        pool_rect: &egui::Rect,
        state: &AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        painter.text(
            egui::pos2(pool_rect.left() + 4.0, pool_rect.top() + 4.0),
            egui::Align2::LEFT_TOP,
            "Media",
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgb(100, 100, 110),
        );

        let item_size = 40.0;
        let padding = 4.0;
        let mut x = pool_rect.left() + 4.0;
        let y = pool_rect.top() + 16.0;

        let items: Vec<_> = state.media_pool.all().collect();
        for item in &items {
            if x + item_size > pool_rect.right() {
                break; // No scrolling for now — just clip
            }

            let thumb_rect = egui::Rect::from_min_size(
                egui::pos2(x, y),
                egui::vec2(item_size, item_size - 8.0),
            );

            // Draw thumbnail or grey placeholder
            if let Some(tex) = state.thumbnail_textures.get(&item.id) {
                painter.image(
                    tex.id(),
                    thumb_rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
            } else {
                painter.rect_filled(thumb_rect, 2.0, egui::Color32::from_rgb(60, 60, 70));
            }

            painter.rect_stroke(
                thumb_rect,
                2.0,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 80, 90)),
            );

            // Click to append
            let resp = ui.allocate_rect(thumb_rect, egui::Sense::click());
            if resp.clicked() {
                cmds.push(AppCommand::AppendClip { media_id: item.id });
            }
            if resp.hovered() {
                egui::show_tooltip_at_pointer(
                    ui.ctx(),
                    ui.layer_id(),
                    ui.id().with(("pool_thumb", item.id)),
                    |ui: &mut egui::Ui| {
                        ui.label(&item.filename);
                    },
                );
            }

            x += item_size + padding;
        }
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
