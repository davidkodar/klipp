use crate::state::AppState;
use klipp_core::{AppCommand, ClipId, RationalTime, Timecode};
use klipp_engine::PlaybackState;
use klipp_timeline::TrackKind;

const TRACK_HEADER_WIDTH: f32 = 80.0;
const RULER_HEIGHT: f32 = 24.0;
const VIDEO_TRACK_HEIGHT: f32 = 56.0;
const AUDIO_TRACK_HEIGHT: f32 = 40.0;
const MEDIA_POOL_HEIGHT: f32 = 50.0;
const TRIM_HANDLE_WIDTH: f32 = 8.0;

const CLIP_COLORS: &[(u8, u8, u8)] = &[
    (50, 90, 160),   // blue
    (50, 130, 80),   // green
    (160, 90, 50),   // orange
    (130, 50, 130),  // purple
    (50, 130, 130),  // teal
    (160, 50, 80),   // red
];

fn clip_color(index: usize) -> egui::Color32 {
    let (r, g, b) = CLIP_COLORS[index % CLIP_COLORS.len()];
    egui::Color32::from_rgb(r, g, b)
}

fn clip_color_bright(index: usize) -> egui::Color32 {
    let (r, g, b) = CLIP_COLORS[index % CLIP_COLORS.len()];
    egui::Color32::from_rgb(r.saturating_add(30), g.saturating_add(30), b.saturating_add(30))
}

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
                    state.timeline_zoom = (state.timeline_zoom * factor).clamp(10.0, 500.0);
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
        bg_painter.rect_filled(header_rect, 0.0, egui::Color32::from_rgb(20, 20, 24));

        // Separator line between header and content
        bg_painter.line_segment(
            [
                egui::pos2(header_rect.right(), header_rect.top()),
                egui::pos2(header_rect.right(), header_rect.bottom()),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(42, 42, 48)),
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

        // Zoom buttons in ruler area (right side)
        self.draw_zoom_buttons(ui, &ruler_rect, state);

        // Ruler click to seek
        let ruler_response = ui.allocate_rect(ruler_rect, egui::Sense::click());
        if ruler_response.clicked() {
            if let Some(pos) = ruler_response.interact_pointer_pos {
                let x_in_content = pos.x - content_rect.left() + state.timeline_scroll_x;
                let secs = x_in_content / state.timeline_zoom;
                let tc = RationalTime::from_secs_f64(secs as f64, state.fps.rate());
                let dur = state.sequence_duration();
                let clamped = RationalTime { value: tc.value.max(0).min(dur.value), rate: tc.rate };
                cmds.push(AppCommand::SeekTo(clamped));
            }
        }

        // Media pool bar (below ruler, above tracks)
        let pool_rect = egui::Rect::from_min_size(
            egui::pos2(content_rect.left(), content_rect.top() + RULER_HEIGHT),
            egui::vec2(content_rect.width(), MEDIA_POOL_HEIGHT),
        );
        painter.rect_filled(pool_rect, 0.0, egui::Color32::from_rgb(20, 20, 24));
        self.draw_media_pool(ui, &painter, &pool_rect, state, &mut cmds);

        // Draw tracks: Video 2, Video 1, Audio 1
        let tracks_start_y = content_rect.top() + RULER_HEIGHT + MEDIA_POOL_HEIGHT;
        let mut track_y = tracks_start_y;

        let track_count = state.sequence.tracks.len();
        for track_idx in 0..track_count {
            let track_kind = state.sequence.tracks[track_idx].kind;
            let track_name = state.sequence.tracks[track_idx].name.clone();
            let track_height = match track_kind {
                TrackKind::Video => VIDEO_TRACK_HEIGHT,
                TrackKind::Audio => AUDIO_TRACK_HEIGHT,
            };

            // Draw track header label
            let label_pos = egui::pos2(
                header_rect.left() + 8.0,
                track_y + track_height / 2.0,
            );
            let label_color = match track_kind {
                TrackKind::Video => egui::Color32::from_rgb(140, 140, 150),
                TrackKind::Audio => egui::Color32::from_rgb(100, 150, 110),
            };
            bg_painter.text(
                label_pos,
                egui::Align2::LEFT_CENTER,
                &track_name,
                egui::FontId::proportional(11.0),
                label_color,
            );

            // Track separator
            bg_painter.line_segment(
                [
                    egui::pos2(panel_rect.left(), track_y),
                    egui::pos2(panel_rect.right(), track_y),
                ],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(35, 35, 40)),
            );

            let track_rect = egui::Rect::from_min_size(
                egui::pos2(content_rect.left(), track_y),
                egui::vec2(content_rect.width(), track_height),
            );

            // Draw clips for this track
            let is_primary_video = track_name == "Video 1";
            self.draw_track_clips(
                ui, &painter, &track_rect, &content_rect,
                state, &mut cmds, track_idx, is_primary_video,
            );

            track_y += track_height;
        }

        // Playhead line
        let seq_dur = state.sequence_duration();
        if state.playback != PlaybackState::Idle || seq_dur.value > 0 {
            let playhead_secs = state.playhead.to_secs_f64() as f32;
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

    #[allow(clippy::too_many_arguments)]
    fn draw_track_clips(
        &mut self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        track_rect: &egui::Rect,
        content_rect: &egui::Rect,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
        track_idx: usize,
        interactive: bool,
    ) {
        let track_kind = state.sequence.tracks[track_idx].kind;
        let clips: Vec<_> = state.sequence.tracks[track_idx].clips.clone();

        let zoom = state.timeline_zoom;
        let scroll = state.timeline_scroll_x;
        let fps = state.fps;
        let track_height = track_rect.height();

        let pointer_pos = ui.input(|i| i.pointer.interact_pos());
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        let pointer_released = ui.input(|i| i.pointer.primary_released());

        for (clip_index, clip) in clips.iter().enumerate() {
            let clip_x =
                content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
            let clip_w = clip.duration().to_secs_f64() as f32 * zoom;

            let clip_rect = egui::Rect::from_min_size(
                egui::pos2(clip_x, track_rect.top() + 2.0),
                egui::vec2(clip_w, track_height - 4.0),
            );

            // Skip if not visible
            if clip_rect.right() < content_rect.left()
                || clip_rect.left() > content_rect.right()
            {
                continue;
            }

            let is_selected = state.selected_clip == Some(clip.id);
            let base_color = match track_kind {
                TrackKind::Video => clip_color(clip_index),
                TrackKind::Audio => egui::Color32::from_rgb(42, 90, 58),
            };
            let bright_color = match track_kind {
                TrackKind::Video => clip_color_bright(clip_index),
                TrackKind::Audio => egui::Color32::from_rgb(72, 120, 88),
            };

            let (fill, border_color, border_width) = if is_selected {
                (bright_color, egui::Color32::from_rgb(120, 160, 240), 2.0)
            } else {
                (base_color, bright_color, 1.0)
            };

            // Draw thumbnail background for video clips
            if track_kind == TrackKind::Video {
                if let Some(tex) = state.thumbnail_textures.get(&clip.media_id) {
                    painter.image(
                        tex.id(),
                        clip_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                    let (r, g, b) = CLIP_COLORS[clip_index % CLIP_COLORS.len()];
                    painter.rect_filled(
                        clip_rect,
                        3.0,
                        egui::Color32::from_rgba_premultiplied(r, g, b, 102),
                    );
                } else {
                    painter.rect_filled(clip_rect, 3.0, fill);
                }
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
                    let font_size = if track_kind == TrackKind::Audio { 10.0 } else { 11.0 };
                    let max_text_width = clip_rect.width() - 8.0;
                    let text_pos =
                        egui::pos2(clip_rect.left() + 4.0, clip_rect.center().y);
                    painter.text(
                        text_pos,
                        egui::Align2::LEFT_CENTER,
                        truncate_text(&media.filename, max_text_width, font_size),
                        egui::FontId::proportional(font_size),
                        egui::Color32::WHITE,
                    );
                }
            }

            if !interactive {
                continue;
            }

            // Interaction: clip selection, drag, trim (only for primary video track)
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

            // Trim handle hover
            if let Some(pos) = pointer_pos {
                if clip_rect.contains(pos) {
                    let near_left = (pos.x - clip_rect.left()).abs() < TRIM_HANDLE_WIDTH;
                    let near_right = (pos.x - clip_rect.right()).abs() < TRIM_HANDLE_WIDTH;

                    if near_left || near_right {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    }

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

        if !interactive {
            return;
        }

        // Handle ongoing trim
        if let Some(ref trim) = self.trimming_clip {
            if let Some(pos) = pointer_pos {
                if pointer_down {
                    let dx = pos.x - trim.start_x;
                    let dt_secs = dx as f64 / zoom as f64;
                    let dt_frames = RationalTime::from_secs_f64(dt_secs, fps.rate());

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
                        .unwrap_or(RationalTime { value: i64::MAX, rate: fps.rate() });

                    match trim.edge {
                        TrimEdge::Left => {
                            let new_source_in = RationalTime {
                                value: (trim.original_source_in.value + dt_frames.value)
                                    .max(0)
                                    .min(trim.original_source_out.value - 1),
                                rate: trim.original_source_in.rate,
                            };
                            let delta = new_source_in - trim.original_source_in;
                            let new_timeline_in = trim.original_timeline_in + delta;

                            if let Some(track) = state.sequence.video_track_mut() {
                                if let Some(c) = track.find_clip_mut(trim.clip_id) {
                                    c.source_in = new_source_in;
                                    c.timeline_in = new_timeline_in;
                                }
                            }

                            let tc_str = new_source_in.to_timecode_string();
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
                            let new_source_out = RationalTime {
                                value: (trim.original_source_out.value + dt_frames.value)
                                    .max(trim.original_source_in.value + 1)
                                    .min(media_duration.value),
                                rate: trim.original_source_out.rate,
                            };
                            let delta = new_source_out - trim.original_source_out;
                            let new_timeline_out = trim.original_timeline_out + delta;

                            if let Some(track) = state.sequence.video_track_mut() {
                                if let Some(c) = track.find_clip_mut(trim.clip_id) {
                                    c.source_out = new_source_out;
                                    c.timeline_out = new_timeline_out;
                                }
                            }

                            let tc_str = new_source_out.to_timecode_string();
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
                    if let Some(c) = track.find_clip(trim.clip_id) {
                        cmds.push(AppCommand::TrimClip {
                            clip_id: trim.clip_id,
                            new_source_in: c.source_in,
                            new_source_out: c.source_out,
                            new_timeline_in: c.timeline_in,
                            new_timeline_out: c.timeline_out,
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
                    let dt_frames = RationalTime::from_secs_f64(dt_secs, fps.rate());
                    let new_timeline_in = RationalTime {
                        value: (drag.original_timeline_in.value + dt_frames.value).max(0),
                        rate: drag.original_timeline_in.rate,
                    };

                    let snap_threshold_secs = 8.0 / zoom as f64;
                    let snap_threshold = RationalTime::from_secs_f64(snap_threshold_secs, fps.rate());

                    let mut final_in = new_timeline_in;
                    if let Some(track) = state.sequence.video_track() {
                        if let Some(clip) = track.find_clip(drag.clip_id) {
                            let dur = clip.duration();
                            let new_out = new_timeline_in + dur;

                            for other in &track.clips {
                                if other.id == drag.clip_id {
                                    continue;
                                }
                                let gap_right =
                                    (new_out.value - other.timeline_in.value).abs();
                                let gap_left =
                                    (new_timeline_in.value - other.timeline_out.value).abs();

                                if gap_right < snap_threshold.value {
                                    final_in = RationalTime {
                                        value: other.timeline_in.value - dur.value,
                                        rate: other.timeline_in.rate,
                                    };
                                }
                                if gap_left < snap_threshold.value {
                                    final_in = other.timeline_out;
                                }

                                let would_overlap = final_in < other.timeline_out
                                    && final_in + dur > other.timeline_in;
                                if would_overlap && other.id != drag.clip_id {
                                    final_in = drag.original_timeline_in;
                                }
                            }
                        }
                    }

                    if let Some(track) = state.sequence.video_track_mut() {
                        if let Some(clip) = track.find_clip_mut(drag.clip_id) {
                            let dur = clip.duration();
                            clip.timeline_in = RationalTime { value: final_in.value.max(0), rate: final_in.rate };
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
                        + c.timeline_in.to_secs_f64() as f32 * zoom
                        - scroll;
                    let cw = c.duration().to_secs_f64() as f32 * zoom;
                    let cr = egui::Rect::from_min_size(
                        egui::pos2(cx, track_rect.top() + 2.0),
                        egui::vec2(cw, track_height - 4.0),
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

    fn draw_zoom_buttons(
        &self,
        ui: &mut egui::Ui,
        ruler_rect: &egui::Rect,
        state: &mut AppState,
    ) {
        let btn_size = egui::vec2(20.0, 18.0);
        let right_margin = 4.0;

        // Fit button
        let fit_rect = egui::Rect::from_min_size(
            egui::pos2(ruler_rect.right() - right_margin - btn_size.x * 3.0 - 8.0, ruler_rect.top() + 3.0),
            btn_size,
        );
        let fit_resp = ui.allocate_rect(fit_rect, egui::Sense::click());
        let painter = ui.painter_at(fit_rect);
        painter.rect_filled(fit_rect, 2.0, if fit_resp.hovered() {
            egui::Color32::from_rgb(55, 55, 65)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        });
        painter.text(fit_rect.center(), egui::Align2::CENTER_CENTER, "Fit",
            egui::FontId::proportional(9.0), egui::Color32::from_rgb(160, 160, 170));
        if fit_resp.clicked() {
            let dur_secs = state.sequence_duration().to_secs_f64() as f32;
            if dur_secs > 0.0 {
                let content_width = ruler_rect.width() - 20.0;
                state.timeline_zoom = (content_width / dur_secs).clamp(10.0, 500.0);
                state.timeline_scroll_x = 0.0;
            }
        }

        // - button
        let minus_rect = egui::Rect::from_min_size(
            egui::pos2(fit_rect.right() + 4.0, ruler_rect.top() + 3.0),
            btn_size,
        );
        let minus_resp = ui.allocate_rect(minus_rect, egui::Sense::click());
        let painter = ui.painter_at(minus_rect);
        painter.rect_filled(minus_rect, 2.0, if minus_resp.hovered() {
            egui::Color32::from_rgb(55, 55, 65)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        });
        painter.text(minus_rect.center(), egui::Align2::CENTER_CENTER, "\u{2212}",
            egui::FontId::proportional(12.0), egui::Color32::from_rgb(160, 160, 170));
        if minus_resp.clicked() {
            state.timeline_zoom = (state.timeline_zoom / 1.5).clamp(10.0, 500.0);
        }

        // + button
        let plus_rect = egui::Rect::from_min_size(
            egui::pos2(minus_rect.right() + 2.0, ruler_rect.top() + 3.0),
            btn_size,
        );
        let plus_resp = ui.allocate_rect(plus_rect, egui::Sense::click());
        let painter = ui.painter_at(plus_rect);
        painter.rect_filled(plus_rect, 2.0, if plus_resp.hovered() {
            egui::Color32::from_rgb(55, 55, 65)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        });
        painter.text(plus_rect.center(), egui::Align2::CENTER_CENTER, "+",
            egui::FontId::proportional(12.0), egui::Color32::from_rgb(160, 160, 170));
        if plus_resp.clicked() {
            state.timeline_zoom = (state.timeline_zoom * 1.5).clamp(10.0, 500.0);
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
                break;
            }

            let thumb_rect = egui::Rect::from_min_size(
                egui::pos2(x, y),
                egui::vec2(item_size, item_size - 8.0),
            );

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
