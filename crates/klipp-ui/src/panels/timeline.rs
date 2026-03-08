use crate::state::{AppState, PlayheadFollow};
use klipp_core::{AppCommand, ClipId, RationalTime, Timecode, Tool};
use klipp_engine::PlaybackState;
use klipp_timeline::TrackKind;

const TRACK_HEADER_WIDTH: f32 = 80.0;
const RULER_HEIGHT: f32 = 24.0;
const VIDEO_TRACK_HEIGHT: f32 = 56.0;
const AUDIO_TRACK_HEIGHT: f32 = 40.0;
const MEDIA_POOL_HEIGHT: f32 = 50.0;
const TRIM_HANDLE_WIDTH: f32 = 14.0;
const CLIP_GAP: f32 = 3.0; // visual gap between adjacent clips (px per side)
const STATUS_BAR_HEIGHT: f32 = 18.0;

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
    snap_line_x: Option<f32>,
    /// Stores the pointer position at initial mouse-down, before egui's drag threshold kicks in.
    /// Used so trim/move detection uses the click position, not the drifted drag_started position.
    press_origin: Option<egui::Pos2>,
    /// Filmstrip stats for status bar: (visible_unique, total)
    filmstrip_visible: usize,
    filmstrip_total: usize,
    /// Last filmstrip refinement request time — debounce to avoid spamming
    last_refine_request: Option<std::time::Instant>,
}

struct DragState {
    clip_id: ClipId,
    track_idx: usize,
    start_x: f32,
    original_timeline_in: Timecode,
}

struct TrimState {
    clip_id: ClipId,
    track_idx: usize,
    edge: TrimEdge,
    start_x: f32,
    original_source_in: Timecode,
    original_source_out: Timecode,
    original_timeline_in: Timecode,
    original_timeline_out: Timecode,
    /// For ripple trim: original positions of clips after the trimmed clip
    subsequent_clips: Vec<(ClipId, Timecode, Timecode)>, // (id, original_timeline_in, original_timeline_out)
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

        let full_rect = egui::Rect::from_min_size(ui.cursor().min, available);
        ui.advance_cursor_after_rect(full_rect);

        let status_rect = egui::Rect::from_min_max(
            egui::pos2(full_rect.left(), full_rect.bottom() - STATUS_BAR_HEIGHT),
            full_rect.max,
        );
        let panel_rect = egui::Rect::from_min_max(
            full_rect.min,
            egui::pos2(full_rect.right(), status_rect.top()),
        );

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

        // In/Out markers on ruler
        self.draw_in_out_markers(&painter, &ruler_rect, &content_rect, state);

        // Ruler click to seek
        let ruler_response = ui.interact(ruler_rect, ui.id().with("ruler"), egui::Sense::click_and_drag());
        if ruler_response.clicked() || ruler_response.dragged() {
            if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                let x_in_content = pos.x - content_rect.left() + state.timeline_scroll_x;
                let secs = x_in_content / state.timeline_zoom;
                let tc = RationalTime::from_secs_f64(secs as f64, state.fps.rate());
                let dur = state.sequence_duration();
                let clamped = if dur.value > 0 {
                    RationalTime { value: tc.value.max(0).min(dur.value), rate: tc.rate }
                } else {
                    RationalTime { value: tc.value.max(0), rate: tc.rate }
                };
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

        // Zoom buttons in header ruler row
        self.draw_zoom_buttons(ui, &header_rect, &ruler_rect, state);

        // Snap/Ripple toggle buttons in header area
        self.draw_toggle_buttons(ui, &header_rect, content_rect.top() + RULER_HEIGHT, state, &mut cmds);

        // Reset snap line
        self.snap_line_x = None;

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

            // Draw track header label and controls
            let label_color = match track_kind {
                TrackKind::Video => egui::Color32::from_rgb(140, 140, 150),
                TrackKind::Audio => egui::Color32::from_rgb(100, 150, 110),
            };
            bg_painter.text(
                egui::pos2(header_rect.left() + 4.0, track_y + 8.0),
                egui::Align2::LEFT_TOP,
                &track_name,
                egui::FontId::proportional(10.0),
                label_color,
            );

            // Track control buttons
            {
                let track = &state.sequence.tracks[track_idx];
                match track_kind {
                    TrackKind::Video => {
                        // [V] visibility toggle
                        let v_rect = egui::Rect::from_min_size(
                            egui::pos2(header_rect.right() - 22.0, track_y + 4.0),
                            egui::vec2(18.0, 14.0),
                        );
                        let v_resp = ui.interact(v_rect, ui.id().with(("vis_btn", track_idx)), egui::Sense::click());
                        let v_col = if track.visible {
                            egui::Color32::from_rgb(120, 120, 140)
                        } else {
                            egui::Color32::from_rgb(60, 60, 70)
                        };
                        bg_painter.rect_filled(v_rect, 2.0, if v_resp.hovered() {
                            egui::Color32::from_rgb(50, 50, 60)
                        } else {
                            egui::Color32::from_rgb(35, 35, 42)
                        });
                        bg_painter.text(v_rect.center(), egui::Align2::CENTER_CENTER, "V",
                            egui::FontId::proportional(9.0), v_col);
                        if v_resp.clicked() {
                            cmds.push(AppCommand::SetTrackVisibility {
                                track_index: track_idx,
                                visible: !track.visible,
                            });
                        }

                        // Opacity control (drag to adjust, click to reset to 100%)
                        let opacity_pct = (track.opacity * 100.0) as u32;
                        let opacity_rect = egui::Rect::from_min_size(
                            egui::pos2(header_rect.left() + 2.0, track_y + track_height - 18.0),
                            egui::vec2(header_rect.width() - 26.0, 14.0),
                        );
                        let opacity_resp = ui.interact(
                            opacity_rect,
                            ui.id().with(("opacity_ctrl", track_idx)),
                            egui::Sense::click_and_drag(),
                        );

                        // Background bar showing opacity level
                        bg_painter.rect_filled(opacity_rect, 2.0, egui::Color32::from_rgb(30, 30, 38));
                        let fill_w = opacity_rect.width() * track.opacity;
                        let fill_rect = egui::Rect::from_min_size(
                            opacity_rect.left_top(),
                            egui::vec2(fill_w, opacity_rect.height()),
                        );
                        bg_painter.rect_filled(fill_rect, 2.0, egui::Color32::from_rgb(60, 80, 120));
                        bg_painter.text(
                            opacity_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("{}%", opacity_pct),
                            egui::FontId::proportional(9.0),
                            egui::Color32::from_rgb(180, 180, 200),
                        );

                        if opacity_resp.double_clicked() {
                            // Double-click resets to 100%
                            cmds.push(AppCommand::SetTrackOpacity {
                                track_index: track_idx,
                                opacity: 1.0,
                            });
                        } else if opacity_resp.dragged() {
                            let dx = opacity_resp.drag_delta().x;
                            let new_opacity = (track.opacity + dx / opacity_rect.width()).clamp(0.0, 1.0);
                            cmds.push(AppCommand::SetTrackOpacity {
                                track_index: track_idx,
                                opacity: new_opacity,
                            });
                        }
                    }
                    TrackKind::Audio => {
                        let btn_y = track_y + track_height - 16.0;
                        // [M] mute
                        let m_rect = egui::Rect::from_min_size(
                            egui::pos2(header_rect.left() + 4.0, btn_y),
                            egui::vec2(16.0, 12.0),
                        );
                        let m_resp = ui.interact(m_rect, ui.id().with(("mute_btn", track_idx)), egui::Sense::click());
                        let m_bg = if track.muted {
                            egui::Color32::from_rgb(160, 50, 50)
                        } else if m_resp.hovered() {
                            egui::Color32::from_rgb(50, 50, 60)
                        } else {
                            egui::Color32::from_rgb(35, 35, 42)
                        };
                        bg_painter.rect_filled(m_rect, 2.0, m_bg);
                        bg_painter.text(m_rect.center(), egui::Align2::CENTER_CENTER, "M",
                            egui::FontId::proportional(9.0), egui::Color32::WHITE);
                        if m_resp.clicked() {
                            cmds.push(AppCommand::SetTrackMute {
                                track_index: track_idx,
                                muted: !track.muted,
                            });
                        }

                        // [S] solo
                        let s_rect = egui::Rect::from_min_size(
                            egui::pos2(m_rect.right() + 2.0, btn_y),
                            egui::vec2(16.0, 12.0),
                        );
                        let s_resp = ui.interact(s_rect, ui.id().with(("solo_btn", track_idx)), egui::Sense::click());
                        let s_bg = if track.solo {
                            egui::Color32::from_rgb(50, 120, 180)
                        } else if s_resp.hovered() {
                            egui::Color32::from_rgb(50, 50, 60)
                        } else {
                            egui::Color32::from_rgb(35, 35, 42)
                        };
                        bg_painter.rect_filled(s_rect, 2.0, s_bg);
                        bg_painter.text(s_rect.center(), egui::Align2::CENTER_CENTER, "S",
                            egui::FontId::proportional(9.0), egui::Color32::WHITE);
                        if s_resp.clicked() {
                            cmds.push(AppCommand::SetTrackSolo { track_index: track_idx });
                        }

                        // Volume label
                        let vol_pct = (track.volume * 100.0) as u32;
                        bg_painter.text(
                            egui::pos2(s_rect.right() + 4.0, btn_y + 6.0),
                            egui::Align2::LEFT_CENTER,
                            format!("{}%", vol_pct),
                            egui::FontId::proportional(9.0),
                            egui::Color32::from_rgb(90, 90, 100),
                        );
                    }
                }
            }

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

            // Media pool drag-and-drop: highlight track when dragging over it
            if state.dragging_media.is_some() {
                let hover_pos = ui.input(|i| i.pointer.hover_pos());
                let any_down = ui.input(|i| i.pointer.any_down());
                if let Some(pos) = hover_pos {
                    if track_rect.contains(pos) && track_kind == TrackKind::Video {
                        // Highlight drop target
                        painter.rect_stroke(
                            track_rect,
                            0.0,
                            egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 180, 255)),
                        );
                        // Drop on release
                        if !any_down {
                            if let Some(media_id) = state.dragging_media.take() {
                                tracing::info!("Media drop: {:?} -> track {} ({})", media_id, track_idx, track_name);
                                cmds.push(AppCommand::AppendClipToTrack {
                                    media_id,
                                    track_index: track_idx,
                                });
                            }
                        }
                    }
                }
            }

            track_y += track_height;
        }

        // Clear media drag if pointer released outside any track
        if state.dragging_media.is_some() && !ui.input(|i| i.pointer.any_down()) {
            state.dragging_media = None;
        }

        // Playhead line + follow mode
        let seq_dur = state.sequence_duration();
        if state.playback != PlaybackState::Idle || seq_dur.value > 0 {
            let playhead_secs = state.playhead_smooth_secs as f32;
            let view_width = content_rect.width();

            // Auto-scroll based on follow mode
            match state.playhead_follow {
                PlayheadFollow::Page => {
                    let playhead_x_abs = playhead_secs * state.timeline_zoom;
                    let view_end = state.timeline_scroll_x + view_width;
                    if playhead_x_abs > view_end || playhead_x_abs < state.timeline_scroll_x {
                        // Jump one page so playhead is at ~10% from left
                        state.timeline_scroll_x =
                            (playhead_x_abs - view_width * 0.1).max(0.0);
                    }
                }
                PlayheadFollow::Smooth => {
                    if state.playback == PlaybackState::Playing {
                        // Keep playhead at ~25% from left edge during playback
                        let target_scroll =
                            (playhead_secs * state.timeline_zoom - view_width * 0.25).max(0.0);
                        // Smooth lerp to avoid jarring jumps
                        state.timeline_scroll_x +=
                            (target_scroll - state.timeline_scroll_x) * 0.15;
                    }
                }
                PlayheadFollow::Off => {}
            }

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

        // Snap indicator line (yellow)
        if let Some(snap_x) = self.snap_line_x {
            if snap_x >= content_rect.left() && snap_x <= content_rect.right() {
                painter.line_segment(
                    [
                        egui::pos2(snap_x, tracks_start_y),
                        egui::pos2(snap_x, content_rect.bottom()),
                    ],
                    egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 220, 60)),
                );
            }
        }

        // --- Status bar ---
        let status_painter = ui.painter_at(status_rect);
        status_painter.rect_filled(status_rect, 0.0, egui::Color32::from_rgb(18, 18, 22));
        status_painter.line_segment(
            [status_rect.left_top(), status_rect.right_top()],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(48, 48, 56)),
        );

        let dim = egui::Color32::from_rgb(100, 100, 110);
        let bright = egui::Color32::from_rgb(170, 170, 180);
        let font = egui::FontId::proportional(9.5);
        let sep = "  \u{2022}  "; // bullet separator
        let cy = status_rect.center().y;

        // Left side: clip count + selected clip info
        let total_clips: usize = state.sequence.tracks.iter().map(|t| t.clips.len()).sum();
        let mut left_text = format!("{} clip{}", total_clips, if total_clips != 1 { "s" } else { "" });

        if let Some(clip_id) = state.selected_clip {
            if let Some(clip) = state.sequence.tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip_id) {
                if let Some(media) = state.media_pool.get(&clip.media_id) {
                    left_text += &format!("{sep}{}", media.filename);
                }
                let dur = clip.duration().to_timecode_string();
                left_text += &format!("{sep}{dur}");
            }
        }

        status_painter.text(
            egui::pos2(status_rect.left() + 8.0, cy),
            egui::Align2::LEFT_CENTER,
            left_text,
            font.clone(),
            bright,
        );

        // Right side: filmstrip + zoom
        let mut right_text = format!("Zoom: {:.0} px/s", state.timeline_zoom);
        if self.filmstrip_total > 0 {
            right_text = format!(
                "Filmstrip: {}/{}{sep}{}",
                self.filmstrip_visible, self.filmstrip_total, right_text
            );
        }
        status_painter.text(
            egui::pos2(status_rect.right() - 8.0, cy),
            egui::Align2::RIGHT_CENTER,
            right_text,
            font,
            dim,
        );

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

            // Visual rect has a small gap so adjacent clips don't touch
            let clip_rect = egui::Rect::from_min_size(
                egui::pos2(clip_x + CLIP_GAP, track_rect.top() + 2.0),
                egui::vec2((clip_w - CLIP_GAP * 2.0).max(2.0), track_height - 4.0),
            );

            // Skip if not visible
            if clip_rect.right() < content_rect.left()
                || clip_rect.left() > content_rect.right()
            {
                continue;
            }

            let is_selected = state.selected_clip == Some(clip.id);
            let is_offline = state.media_pool.get(&clip.media_id)
                .map(|m| m.offline)
                .unwrap_or(false);

            let base_color = if is_offline {
                egui::Color32::from_rgb(58, 32, 32)
            } else {
                match track_kind {
                    TrackKind::Video => clip_color(clip_index),
                    TrackKind::Audio => egui::Color32::from_rgb(42, 90, 58),
                }
            };
            let bright_color = if is_offline {
                egui::Color32::from_rgb(80, 45, 45)
            } else {
                match track_kind {
                    TrackKind::Video => clip_color_bright(clip_index),
                    TrackKind::Audio => egui::Color32::from_rgb(72, 120, 88),
                }
            };

            let (fill, border_color, border_width) = if is_selected {
                (bright_color, egui::Color32::from_rgb(120, 160, 240), 2.0)
            } else {
                (base_color, bright_color, 1.0)
            };

            // Draw thumbnail/filmstrip background for video clips
            if track_kind == TrackKind::Video {
                let mut drew_filmstrip = false;
                if let Some(filmstrip) = state.filmstrip_textures.get(&clip.media_id) {
                    if !filmstrip.is_empty() {
                        drew_filmstrip = true;
                        let clip_in_secs = clip.source_in.to_secs_f64();
                        let clip_out_secs = clip.source_out.to_secs_f64();
                        let (_, first_tex) = &filmstrip[0];
                        let frame_w = first_tex.size()[0] as f32;
                        let frame_h = first_tex.size()[1] as f32;
                        let frame_aspect = frame_w / frame_h.max(1.0);
                        let tile_h = clip_rect.height();
                        let tile_w = tile_h * frame_aspect;

                        let clip_painter = ui.painter().with_clip_rect(clip_rect);

                        // Only draw tiles that are actually visible in the viewport
                        let num_tiles = ((clip_rect.width() / tile_w).ceil() as usize).max(1);
                        let view_left = content_rect.left();
                        let view_right = content_rect.right();

                        // Compute first/last visible tile indices
                        let first_visible = if clip_rect.left() >= view_left {
                            0
                        } else {
                            ((view_left - clip_rect.left()) / tile_w).floor() as usize
                        };
                        let last_visible = if clip_rect.right() <= view_right {
                            num_tiles
                        } else {
                            (((view_right - clip_rect.left()) / tile_w).ceil() as usize).min(num_tiles)
                        };

                        let sep_w = 1.0_f32;
                        let mut prev_idx: Option<usize> = None;
                        let mut visible_count = 0usize;

                        for ti in first_visible..last_visible {
                            let x0 = clip_rect.left() + ti as f32 * tile_w;
                            let x1 = if ti == num_tiles - 1 {
                                clip_rect.right()
                            } else {
                                x0 + tile_w
                            };

                            let tile_center_x = (x0 + x1) * 0.5;
                            let frac = ((tile_center_x - clip_rect.left()) / clip_rect.width().max(1.0)) as f64;
                            let source_time = clip_in_secs + frac * (clip_out_secs - clip_in_secs);

                            let idx = match filmstrip.binary_search_by(|f| {
                                f.0.partial_cmp(&source_time).unwrap_or(std::cmp::Ordering::Equal)
                            }) {
                                Ok(i) => i,
                                Err(i) => {
                                    if i == 0 { 0 }
                                    else if i >= filmstrip.len() { filmstrip.len() - 1 }
                                    else {
                                        let d_left = (filmstrip[i - 1].0 - source_time).abs();
                                        let d_right = (filmstrip[i].0 - source_time).abs();
                                        if d_left <= d_right { i - 1 } else { i }
                                    }
                                }
                            };

                            // Draw separator when frame changes
                            let draw_sep = prev_idx.is_some_and(|pi| pi != idx);
                            let img_x0 = if draw_sep { x0 + sep_w } else { x0 };
                            prev_idx = Some(idx);
                            visible_count += 1;

                            if draw_sep {
                                clip_painter.line_segment(
                                    [egui::pos2(x0, clip_rect.top()), egui::pos2(x0, clip_rect.bottom())],
                                    egui::Stroke::new(sep_w, egui::Color32::from_rgba_premultiplied(0, 0, 0, 140)),
                                );
                            }

                            let (_, ref tex) = filmstrip[idx];
                            let tile_rect = egui::Rect::from_min_max(
                                egui::pos2(img_x0, clip_rect.top()),
                                egui::pos2(x1, clip_rect.bottom()),
                            );
                            // Draw dark base behind the frame so dark content is still distinguishable
                            clip_painter.rect_filled(tile_rect, 0.0, egui::Color32::from_rgb(35, 35, 40));
                            clip_painter.image(
                                tex.id(),
                                tile_rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                            // Subtle bottom gradient to lift dark frames
                            let grad_h = 4.0;
                            let grad_rect = egui::Rect::from_min_max(
                                egui::pos2(img_x0, clip_rect.bottom() - grad_h),
                                egui::pos2(x1, clip_rect.bottom()),
                            );
                            clip_painter.rect_filled(
                                grad_rect,
                                0.0,
                                egui::Color32::from_rgba_premultiplied(60, 60, 70, 80),
                            );
                        }

                        self.filmstrip_visible = visible_count;
                        self.filmstrip_total = filmstrip.len();

                        // Request refinement if tiles are too sparse for current zoom.
                        // Debounce: only request once every 2 seconds.
                        if visible_count > 3 && filmstrip.len() > 1 {
                            let secs_per_tile = (clip_out_secs - clip_in_secs) / num_tiles.max(1) as f64;
                            let min_interval = state.thumbnail_cache.filmstrip_min_interval(&clip.media_id);
                            let debounce_ok = self.last_refine_request
                                .map(|t| t.elapsed().as_secs_f64() > 2.0)
                                .unwrap_or(true);
                            if let Some(min_iv) = min_interval {
                                if secs_per_tile < min_iv * 0.5 && !state.filmstrip_generating && debounce_ok {
                                    let vis_start_frac = ((first_visible as f32 * tile_w) / clip_rect.width().max(1.0)) as f64;
                                    let vis_end_frac = ((last_visible as f32 * tile_w) / clip_rect.width().max(1.0)).min(1.0) as f64;
                                    let range_start = clip_in_secs + vis_start_frac * (clip_out_secs - clip_in_secs);
                                    let range_end = clip_in_secs + vis_end_frac * (clip_out_secs - clip_in_secs);
                                    let new_interval = secs_per_tile.max(0.5);
                                    cmds.push(AppCommand::RefineFilmstrip {
                                        media_id: clip.media_id,
                                        start_secs: range_start,
                                        end_secs: range_end,
                                        interval_secs: new_interval,
                                    });
                                    self.last_refine_request = Some(std::time::Instant::now());
                                }
                            }
                        }

                        // Very subtle color tint at top edge only (like Resolve)
                        let (r, g, b) = CLIP_COLORS[clip_index % CLIP_COLORS.len()];
                        let tint_h = 3.0;
                        let tint_rect = egui::Rect::from_min_max(
                            clip_rect.left_top(),
                            egui::pos2(clip_rect.right(), clip_rect.top() + tint_h),
                        );
                        painter.rect_filled(
                            tint_rect,
                            0.0,
                            egui::Color32::from_rgba_premultiplied(r, g, b, 180),
                        );
                    }
                }
                if !drew_filmstrip {
                    // Fallback: single thumbnail or solid color
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

            // Always-visible trim handle bars on both edges
            if clip_rect.width() > 12.0 {
                let handle_w = 5.0;
                let handle_inset = 2.0; // vertical inset from clip edges
                let handle_color = egui::Color32::from_rgba_premultiplied(200, 200, 210, 60);
                let grip_color = egui::Color32::from_rgba_premultiplied(255, 255, 255, 40);

                // Left handle
                let lh = egui::Rect::from_min_size(
                    egui::pos2(clip_rect.left(), clip_rect.top() + handle_inset),
                    egui::vec2(handle_w, clip_rect.height() - handle_inset * 2.0),
                );
                painter.rect_filled(lh, 2.0, handle_color);
                // Grip lines (3 small horizontal dashes)
                let cx = lh.center().x;
                let cy = lh.center().y;
                for dy in [-5.0_f32, 0.0, 5.0] {
                    painter.line_segment(
                        [egui::pos2(cx - 1.5, cy + dy), egui::pos2(cx + 1.5, cy + dy)],
                        egui::Stroke::new(1.0, grip_color),
                    );
                }

                // Right handle
                let rh = egui::Rect::from_min_size(
                    egui::pos2(clip_rect.right() - handle_w, clip_rect.top() + handle_inset),
                    egui::vec2(handle_w, clip_rect.height() - handle_inset * 2.0),
                );
                painter.rect_filled(rh, 2.0, handle_color);
                for dy in [-5.0_f32, 0.0, 5.0] {
                    painter.line_segment(
                        [egui::pos2(rh.center().x - 1.5, cy + dy), egui::pos2(rh.center().x + 1.5, cy + dy)],
                        egui::Stroke::new(1.0, grip_color),
                    );
                }
            }

            // Clip name label
            if clip_rect.width() > 40.0 {
                if let Some(media) = state.media_pool.get(&clip.media_id) {
                    let font_size = if track_kind == TrackKind::Audio { 10.0 } else { 11.0 };
                    let max_text_width = clip_rect.width() - 8.0;
                    let label = if is_offline {
                        format!("\u{26A0} offline: {}", media.filename)
                    } else {
                        media.filename.clone()
                    };
                    let label_color = if is_offline {
                        egui::Color32::from_rgb(255, 180, 80)
                    } else {
                        egui::Color32::WHITE
                    };
                    let text_pos =
                        egui::pos2(clip_rect.left() + 4.0, clip_rect.center().y);
                    painter.text(
                        text_pos,
                        egui::Align2::LEFT_CENTER,
                        truncate_text(&label, max_text_width, font_size),
                        egui::FontId::proportional(font_size),
                        label_color,
                    );

                    // Proxy badge [P]
                    if media.proxy_ready && state.use_proxies && clip_rect.width() > 30.0 {
                        let badge_pos = egui::pos2(clip_rect.right() - 12.0, clip_rect.top() + 8.0);
                        painter.text(
                            badge_pos,
                            egui::Align2::CENTER_CENTER,
                            "P",
                            egui::FontId::proportional(8.0),
                            egui::Color32::from_rgb(100, 180, 255),
                        );
                    }
                }
            }

            if !interactive {
                continue;
            }

            // Blade cursor/line (visual only, no interaction needed)
            if state.active_tool == Tool::Blade {
                if let Some(pos) = pointer_pos {
                    if clip_rect.contains(pos) {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                        painter.line_segment(
                            [
                                egui::pos2(pos.x, clip_rect.top()),
                                egui::pos2(pos.x, clip_rect.bottom()),
                            ],
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(255, 60, 60)),
                        );
                    }
                }
            }

        }

        // Trim handle hover highlight — edge-first detection
        if state.active_tool == Tool::Select && self.trimming_clip.is_none() && self.dragging_clip.is_none() {
            if let Some(pos) = pointer_pos {
                if let Some((cr, _, edge)) = self.hit_test_edge(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                    let handle_w = 6.0;
                    let edge_x = match edge {
                        TrimEdge::Left => cr.left(),
                        TrimEdge::Right => cr.right() - handle_w,
                    };
                    // Bright highlight bar
                    let handle_rect = egui::Rect::from_min_size(
                        egui::pos2(edge_x, cr.top()),
                        egui::vec2(handle_w, cr.height()),
                    );
                    painter.rect_filled(
                        handle_rect,
                        2.0,
                        egui::Color32::from_rgba_premultiplied(100, 180, 255, 180),
                    );
                    // Bright grip lines
                    let cx = handle_rect.center().x;
                    let cy = handle_rect.center().y;
                    for dy in [-6.0_f32, -2.0, 2.0, 6.0] {
                        painter.line_segment(
                            [egui::pos2(cx - 1.5, cy + dy), egui::pos2(cx + 1.5, cy + dy)],
                            egui::Stroke::new(1.0, egui::Color32::WHITE),
                        );
                    }
                }
            }
        }

        // Active trim highlight — show bright handle during active trim
        if let Some(ref trim) = self.trimming_clip {
            if trim.track_idx == track_idx {
                if let Some(clip) = clips.iter().find(|c| c.id == trim.clip_id) {
                    let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);
                    let handle_w = 6.0;
                    let edge_x = match trim.edge {
                        TrimEdge::Left => cr.left(),
                        TrimEdge::Right => cr.right() - handle_w,
                    };
                    let handle_rect = egui::Rect::from_min_size(
                        egui::pos2(edge_x, cr.top()),
                        egui::vec2(handle_w, cr.height()),
                    );
                    painter.rect_filled(
                        handle_rect,
                        2.0,
                        egui::Color32::from_rgba_premultiplied(255, 200, 60, 200),
                    );
                }
            }
        }

        if !interactive {
            return;
        }

        // Single interaction rect for the whole track — avoids per-clip hit-test priority
        // issues in egui. We do our own hit-testing against clip rects.
        let track_interact_id = ui.id().with(("track_interact", track_idx));
        let track_response = ui.interact(*track_rect, track_interact_id, egui::Sense::click_and_drag());

        // Capture press origin on initial mouse-down (before drag threshold)
        if pointer_down && self.press_origin.is_none() && self.dragging_clip.is_none() && self.trimming_clip.is_none() {
            if let Some(pos) = pointer_pos {
                if track_rect.contains(pos) {
                    self.press_origin = Some(pos);
                }
            }
        }
        // Clear press origin when pointer released without starting drag/trim
        if !pointer_down && self.dragging_clip.is_none() && self.trimming_clip.is_none() {
            self.press_origin = None;
        }

        // Debug: log all interaction states
        if track_response.drag_started() {
            tracing::debug!("track {} drag_started, pointer={:?}, press_origin={:?}", track_idx, pointer_pos, self.press_origin);
        }
        if track_response.clicked() {
            tracing::debug!("track {} clicked, pointer={:?}", track_idx, pointer_pos);
        }
        if track_response.hovered() && pointer_down {
            // Only log occasionally to avoid spam
            static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let c = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if c.is_multiple_of(60) {
                tracing::debug!("track {} hovered+pointer_down, dragging={}, trimming={}", track_idx, self.dragging_clip.is_some(), self.trimming_clip.is_some());
            }
        }

        // Right-click context menu on clips
        if track_response.secondary_clicked() {
            if let Some(pos) = pointer_pos {
                if let Some((_, hit_clip)) = self.hit_test_clip_body(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    // Store the clip id so the context menu can use it
                    state.selected_clip = Some(hit_clip.id);
                }
            }
        }
        track_response.context_menu(|ui| {
            if let Some(clip_id) = state.selected_clip {
                if ui.button("Remove Clip").clicked() {
                    cmds.push(AppCommand::RemoveClip { clip_id });
                    ui.close_menu();
                }
                ui.separator();
                let r = ui.add_enabled(false, egui::Button::new("Properties..."));
                r.on_disabled_hover_text("(coming soon)");
            }
        });

        // Click handling
        if track_response.clicked() {
            if let Some(pos) = track_response.interact_pointer_pos {
                // Always move playhead to click position (standard NLE behavior)
                let x_in_content = pos.x - content_rect.left() + scroll;
                let click_secs = (x_in_content as f64 / zoom as f64).max(0.0);
                let click_tc = RationalTime::from_secs_f64(click_secs, fps.rate());
                cmds.push(AppCommand::SeekTo(click_tc));

                if let Some((_, hit_clip)) = self.hit_test_clip_body(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    if state.active_tool == Tool::Blade {
                        let cut_tc = RationalTime::from_secs_f64(click_secs, fps.rate());
                        cmds.push(AppCommand::BladeClip {
                            track_index: track_idx,
                            clip_id: hit_clip.id,
                            cut_at: cut_tc,
                        });
                    } else {
                        cmds.push(AppCommand::SelectClip(Some(hit_clip.id)));
                    }
                } else {
                    // Clicked empty area — deselect
                    if self.dragging_clip.is_none() && self.trimming_clip.is_none() {
                        cmds.push(AppCommand::SelectClip(None));
                    }
                }
            }
        }

        // Drag start: edge-first detection for trim, then clip-body for move.
        // Uses press_origin (where user first clicked) instead of current pointer pos,
        // because egui delays drag_started() until pointer moves ~4px from click point.
        if track_response.drag_started() {
            let decision_pos = self.press_origin.or(pointer_pos);
            if let Some(pos) = decision_pos {
                // First: check if pointer is near any clip edge (trim)
                if let Some((_cr, hit_clip, edge)) = self.hit_test_edge(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    tracing::debug!("drag_started TRIM: clip={:?}, edge={:?}", hit_clip.id, match edge { TrimEdge::Left => "left", TrimEdge::Right => "right" });
                    // Collect subsequent clips for ripple trim
                    let subsequent = if state.ripple_enabled {
                        clips.iter()
                            .filter(|c| c.id != hit_clip.id && c.timeline_in >= hit_clip.timeline_in)
                            .map(|c| (c.id, c.timeline_in, c.timeline_out))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    self.trimming_clip = Some(TrimState {
                        clip_id: hit_clip.id,
                        track_idx,
                        edge,
                        start_x: pos.x,
                        original_source_in: hit_clip.source_in,
                        original_source_out: hit_clip.source_out,
                        original_timeline_in: hit_clip.timeline_in,
                        original_timeline_out: hit_clip.timeline_out,
                        subsequent_clips: subsequent,
                    });
                }
                // Second: check if pointer is inside a clip body (drag/move)
                else if let Some((_cr, hit_clip)) = self.hit_test_clip_body(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    tracing::debug!("drag_started MOVE: clip={:?}", hit_clip.id);
                    if state.active_tool == Tool::Select {
                        self.dragging_clip = Some(DragState {
                            clip_id: hit_clip.id,
                            track_idx,
                            start_x: pos.x,
                            original_timeline_in: hit_clip.timeline_in,
                        });
                    }
                }
            }
        }

        // Collect snap targets upfront (avoids borrow conflicts with state mutation)
        let snap_enabled = state.snap_enabled && !ui.input(|i| i.modifiers.shift);
        let snap_threshold_secs = 10.0 / zoom as f64;
        // Collect all snap points: (time_secs, RationalTime) for each clip edge + playhead + zero
        let snap_targets: Vec<(f64, RationalTime, Option<ClipId>)> = {
            let mut targets = Vec::new();
            for track in &state.sequence.tracks {
                for clip in &track.clips {
                    targets.push((clip.timeline_in.to_secs_f64(), clip.timeline_in, Some(clip.id)));
                    targets.push((clip.timeline_out.to_secs_f64(), clip.timeline_out, Some(clip.id)));
                }
            }
            targets.push((state.playhead.to_secs_f64(), state.playhead, None));
            targets.push((0.0, RationalTime { value: 0, rate: fps.rate() }, None));
            // Snap to in/out markers if set
            if let Some(inp) = state.sequence.in_point {
                targets.push((inp.to_secs_f64(), inp, None));
            }
            if let Some(outp) = state.sequence.out_point {
                targets.push((outp.to_secs_f64(), outp, None));
            }
            targets
        };
        let find_snap = |edge_time: RationalTime, exclude_clip: ClipId| -> Option<(RationalTime, f64)> {
            if !snap_enabled { return None; }
            let edge_secs = edge_time.to_secs_f64();
            let mut best: Option<(f64, RationalTime)> = None;
            for &(target_secs, target_tc, owner) in &snap_targets {
                if owner == Some(exclude_clip) { continue; }
                let dist = (edge_secs - target_secs).abs();
                if dist < snap_threshold_secs && (best.is_none() || dist < best.unwrap().0) {
                    best = Some((dist, target_tc));
                }
            }
            best.map(|(_, target)| (target, target.to_secs_f64()))
        };

        // Handle ongoing trim
        if let Some(ref trim) = self.trimming_clip {
            if let Some(pos) = pointer_pos {
                if pointer_down {
                    let dx = pos.x - trim.start_x;
                    let dt_secs = dx as f64 / zoom as f64;
                    let dt_frames = RationalTime::from_secs_f64(dt_secs, fps.rate());

                    let media_duration = state.sequence.tracks.get(trim.track_idx)
                        .and_then(|t| t.find_clip(trim.clip_id))
                        .map(|c| c.media_id)
                        .and_then(|mid| state.media_pool.get(&mid))
                        .map(|m| m.duration)
                        .unwrap_or(RationalTime { value: i64::MAX, rate: fps.rate() });

                    match trim.edge {
                        TrimEdge::Left => {
                            let new_source_in = {
                                let sum = trim.original_source_in + dt_frames;
                                let zero = RationalTime { value: 0, rate: sum.rate };
                                let max_in = trim.original_source_out - RationalTime { value: 1, rate: trim.original_source_out.rate };
                                if sum < zero { zero }
                                else if sum > max_in { max_in }
                                else { sum }
                            };
                            let source_delta = new_source_in - trim.original_source_in;

                            if trim.subsequent_clips.is_empty() {
                                // Non-ripple: move timeline_in, keep timeline_out fixed
                                let mut new_timeline_in = trim.original_timeline_in + source_delta;

                                // Snap the left edge
                                if let Some((snap_target, snap_secs)) = find_snap(new_timeline_in, trim.clip_id) {
                                    let snap_delta = snap_target - trim.original_timeline_in;
                                    let snapped_source_in = trim.original_source_in + snap_delta;
                                    let zero = RationalTime { value: 0, rate: snapped_source_in.rate };
                                    let max_in = trim.original_source_out - RationalTime { value: 1, rate: trim.original_source_out.rate };
                                    if snapped_source_in >= zero && snapped_source_in <= max_in {
                                        new_timeline_in = snap_target;
                                        self.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                                    }
                                }

                                let final_delta = new_timeline_in - trim.original_timeline_in;
                                let final_source_in = trim.original_source_in + final_delta;

                                if let Some(track) = state.sequence.tracks.get_mut(trim.track_idx) {
                                    if let Some(c) = track.find_clip_mut(trim.clip_id) {
                                        c.source_in = final_source_in;
                                        c.timeline_in = new_timeline_in;
                                    }
                                }

                                let tc_str = new_timeline_in.to_timecode_string();
                                egui::show_tooltip_at_pointer(
                                    ui.ctx(),
                                    ui.layer_id(),
                                    ui.id().with("trim_tooltip"),
                                    |ui: &mut egui::Ui| {
                                        ui.label(format!("In: {tc_str}"));
                                    },
                                );
                            } else {
                                // Ripple: keep timeline_in fixed, shrink/grow timeline_out,
                                // shift subsequent clips by the duration change
                                let final_source_in = new_source_in;
                                // Duration change: negative when clip gets shorter (source_in moved right)
                                let duration_delta = trim.original_source_in - final_source_in; // negative = shorter
                                let new_timeline_out = trim.original_timeline_out + duration_delta;

                                // Snap the new timeline_out
                                let mut snapped_timeline_out = new_timeline_out;
                                if let Some((snap_target, snap_secs)) = find_snap(new_timeline_out, trim.clip_id) {
                                    snapped_timeline_out = snap_target;
                                    self.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                                }

                                // Recompute source_in from the snapped duration
                                let snapped_duration = snapped_timeline_out - trim.original_timeline_in;
                                let snapped_source_in = trim.original_source_out - snapped_duration;
                                let zero = RationalTime { value: 0, rate: snapped_source_in.rate };
                                let max_in = trim.original_source_out - RationalTime { value: 1, rate: trim.original_source_out.rate };
                                let clamped_source_in = if snapped_source_in < zero { zero }
                                    else if snapped_source_in > max_in { max_in }
                                    else { snapped_source_in };

                                // Recalculate timeline_out from clamped source
                                let final_duration = trim.original_source_out - clamped_source_in;
                                let final_timeline_out = trim.original_timeline_in + final_duration;
                                let ripple_delta = final_timeline_out - trim.original_timeline_out;

                                if let Some(track) = state.sequence.tracks.get_mut(trim.track_idx) {
                                    if let Some(c) = track.find_clip_mut(trim.clip_id) {
                                        c.source_in = clamped_source_in;
                                        // timeline_in stays at original
                                        c.timeline_in = trim.original_timeline_in;
                                        c.timeline_out = final_timeline_out;
                                    }
                                    for (sub_id, orig_in, orig_out) in &trim.subsequent_clips {
                                        if let Some(sc) = track.find_clip_mut(*sub_id) {
                                            sc.timeline_in = *orig_in + ripple_delta;
                                            sc.timeline_out = *orig_out + ripple_delta;
                                        }
                                    }
                                }

                                let tc_str = clamped_source_in.to_timecode_string();
                                egui::show_tooltip_at_pointer(
                                    ui.ctx(),
                                    ui.layer_id(),
                                    ui.id().with("trim_tooltip"),
                                    |ui: &mut egui::Ui| {
                                        ui.label(format!("In: {tc_str}"));
                                    },
                                );
                            }
                        }
                        TrimEdge::Right => {
                            let new_source_out = {
                                let sum = trim.original_source_out + dt_frames;
                                let min_out = trim.original_source_in + RationalTime { value: 1, rate: trim.original_source_in.rate };
                                if sum < min_out { min_out }
                                else if sum > media_duration { media_duration }
                                else { sum }
                            };
                            let delta = new_source_out - trim.original_source_out;
                            let mut new_timeline_out = trim.original_timeline_out + delta;

                            // Snap the right edge (timeline_out)
                            if let Some((snap_target, snap_secs)) = find_snap(new_timeline_out, trim.clip_id) {
                                let snap_delta = snap_target - trim.original_timeline_out;
                                let snapped_source_out = trim.original_source_out + snap_delta;
                                let min_out = trim.original_source_in + RationalTime { value: 1, rate: trim.original_source_in.rate };
                                if snapped_source_out >= min_out && snapped_source_out <= media_duration {
                                    new_timeline_out = snap_target;
                                    self.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                                }
                            }

                            let final_delta = new_timeline_out - trim.original_timeline_out;
                            let final_source_out = trim.original_source_out + final_delta;

                            if let Some(track) = state.sequence.tracks.get_mut(trim.track_idx) {
                                if let Some(c) = track.find_clip_mut(trim.clip_id) {
                                    c.source_out = final_source_out;
                                    c.timeline_out = new_timeline_out;
                                }
                                // Ripple: shift subsequent clips by the out-point change
                                if !trim.subsequent_clips.is_empty() {
                                    let ripple_delta = new_timeline_out - trim.original_timeline_out;
                                    for (sub_id, orig_in, orig_out) in &trim.subsequent_clips {
                                        if let Some(sc) = track.find_clip_mut(*sub_id) {
                                            sc.timeline_in = *orig_in + ripple_delta;
                                            sc.timeline_out = *orig_out + ripple_delta;
                                        }
                                    }
                                }
                            }

                            let tc_str = new_timeline_out.to_timecode_string();
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
                if let Some(track) = state.sequence.tracks.get(trim.track_idx) {
                    if let Some(c) = track.find_clip(trim.clip_id) {
                        cmds.push(AppCommand::TrimClip {
                            clip_id: trim.clip_id,
                            new_source_in: c.source_in,
                            new_source_out: c.source_out,
                            new_timeline_in: c.timeline_in,
                            new_timeline_out: c.timeline_out,
                            ripple: state.ripple_enabled,
                        });
                    }
                }
                self.trimming_clip = None;
                self.press_origin = None;
            }
        }

        // Handle ongoing drag
        if let Some(ref drag) = self.dragging_clip {
            if let Some(pos) = pointer_pos {
                if pointer_down {
                    let dx = pos.x - drag.start_x;
                    let dt_secs = dx as f64 / zoom as f64;
                    let dt_frames = RationalTime::from_secs_f64(dt_secs, fps.rate());
                    let new_timeline_in = {
                        let sum = drag.original_timeline_in + dt_frames;
                        if sum.value < 0 {
                            RationalTime { value: 0, rate: sum.rate }
                        } else {
                            sum
                        }
                    };

                    let mut final_in = new_timeline_in;
                    let mut snapped = false;

                    if let Some(track) = state.sequence.tracks.get(drag.track_idx) {
                        if let Some(clip) = track.find_clip(drag.clip_id) {
                            let dur = clip.duration();
                            let new_out = new_timeline_in + dur;

                            // Snap left edge
                            if let Some((snap_target, snap_secs)) = find_snap(new_timeline_in, drag.clip_id) {
                                final_in = snap_target;
                                snapped = true;
                                self.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                            }
                            // Snap right edge (if no left-edge snap found, or if right is closer)
                            if let Some((snap_target, snap_secs)) = find_snap(new_out, drag.clip_id) {
                                let right_snap_in = snap_target - dur;
                                // Prefer whichever snap is closer in pixels
                                let left_dist = if snapped {
                                    ((final_in.to_secs_f64() - new_timeline_in.to_secs_f64()).abs() * zoom as f64) as f32
                                } else {
                                    f32::MAX
                                };
                                let right_dist = ((snap_target.to_secs_f64() - new_out.to_secs_f64()).abs() * zoom as f64) as f32;
                                if !snapped || right_dist < left_dist {
                                    final_in = right_snap_in;
                                    snapped = true;
                                    self.snap_line_x = Some(content_rect.left() + snap_secs as f32 * zoom - state.timeline_scroll_x);
                                }
                            }
                        }
                    }

                    // Clamp to >= 0
                    if final_in.value < 0 {
                        final_in.value = 0;
                    }

                    if let Some(track) = state.sequence.tracks.get_mut(drag.track_idx) {
                        if let Some(clip) = track.find_clip_mut(drag.clip_id) {
                            let dur = clip.duration();
                            clip.timeline_in = final_in;
                            clip.timeline_out = clip.timeline_in + dur;
                        }
                    }

                    let _ = snapped; // suppress unused warning
                }
            }

            if pointer_released {
                let clip_id = drag.clip_id;
                if let Some(track) = state.sequence.tracks.get(drag.track_idx) {
                    if let Some(clip) = track.find_clip(clip_id) {
                        cmds.push(AppCommand::MoveClip {
                            clip_id,
                            new_timeline_in: clip.timeline_in,
                        });
                    }
                }
                self.dragging_clip = None;
                self.press_origin = None;
            }
        }
    }

    /// Compute the visual rect for a clip on screen.
    fn clip_screen_rect(
        clip: &klipp_timeline::Clip,
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
    ) -> egui::Rect {
        let clip_x = content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
        let clip_w = clip.duration().to_secs_f64() as f32 * zoom;
        egui::Rect::from_min_size(
            egui::pos2(clip_x + CLIP_GAP, track_rect.top() + 2.0),
            egui::vec2((clip_w - CLIP_GAP * 2.0).max(2.0), track_height - 4.0),
        )
    }

    /// Edge-first hit-test: find the closest clip edge within TRIM_HANDLE_WIDTH.
    /// Returns (clip_rect, clip, TrimEdge) if an edge is close enough.
    /// This ensures that at shared borders between adjacent clips, each clip's
    /// edge is independently grabbable — like professional NLEs.
    #[allow(clippy::too_many_arguments)]
    fn hit_test_edge<'a>(
        &self,
        clips: &'a [klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        pos: egui::Pos2,
    ) -> Option<(egui::Rect, &'a klipp_timeline::Clip, TrimEdge)> {
        // Check vertical bounds
        let y_top = track_rect.top();
        let y_bot = track_rect.top() + track_height;
        if pos.y < y_top || pos.y > y_bot {
            return None;
        }

        // Collect all edges with their distance to pointer
        let mut best: Option<(f32, egui::Rect, &klipp_timeline::Clip, TrimEdge)> = None;

        for clip in clips {
            let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);

            let dist_left = (pos.x - cr.left()).abs();
            let dist_right = (pos.x - cr.right()).abs();

            if dist_left < TRIM_HANDLE_WIDTH
                && (best.is_none() || dist_left < best.unwrap().0)
            {
                best = Some((dist_left, cr, clip, TrimEdge::Left));
            }
            if dist_right < TRIM_HANDLE_WIDTH
                && (best.is_none() || dist_right < best.unwrap().0)
            {
                best = Some((dist_right, cr, clip, TrimEdge::Right));
            }
        }

        best.map(|(_, cr, clip, edge)| (cr, clip, edge))
    }

    /// Hit-test: find which clip body (not edge) is under the pointer.
    /// Used for drag/move and selection. Does NOT match edge zones.
    #[allow(clippy::too_many_arguments)]
    fn hit_test_clip_body<'a>(
        &self,
        clips: &'a [klipp_timeline::Clip],
        content_rect: &egui::Rect,
        track_rect: &egui::Rect,
        zoom: f32,
        scroll: f32,
        track_height: f32,
        pos: egui::Pos2,
    ) -> Option<(egui::Rect, &'a klipp_timeline::Clip)> {
        for clip in clips.iter().rev() {
            let cr = Self::clip_screen_rect(clip, content_rect, track_rect, zoom, scroll, track_height);
            if cr.contains(pos) {
                return Some((cr, clip));
            }
        }
        None
    }

    fn draw_zoom_buttons(
        &self,
        ui: &mut egui::Ui,
        header_rect: &egui::Rect,
        ruler_rect: &egui::Rect,
        state: &mut AppState,
    ) {
        let btn_w = 22.0;
        let btn_h = 16.0;
        let y = ruler_rect.top() + (RULER_HEIGHT - btn_h) / 2.0;
        let mut x = header_rect.left() + 2.0;
        let painter = ui.painter();

        // Fit button
        let fit_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(btn_w, btn_h));
        let fit_resp = ui.interact(fit_rect, ui.id().with("zoom_fit"), egui::Sense::click());
        painter.rect_filled(fit_rect, 2.0, if fit_resp.hovered() {
            egui::Color32::from_rgb(55, 55, 65)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        });
        painter.text(fit_rect.center(), egui::Align2::CENTER_CENTER, "Fit",
            egui::FontId::proportional(8.0), egui::Color32::from_rgb(160, 160, 170));
        if fit_resp.clicked() {
            let dur_secs = state.sequence_duration().to_secs_f64() as f32;
            if dur_secs > 0.0 {
                let content_width = ruler_rect.width() - 20.0;
                // Allow zoom down to 0.1 px/sec for long clips (e.g. 12 min movie)
                state.timeline_zoom = (content_width / dur_secs).clamp(0.1, 500.0);
                state.timeline_scroll_x = 0.0;
            }
        }
        x += btn_w + 2.0;

        // - button
        let minus_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(btn_w, btn_h));
        let minus_resp = ui.interact(minus_rect, ui.id().with("zoom_minus"), egui::Sense::click());
        painter.rect_filled(minus_rect, 2.0, if minus_resp.hovered() {
            egui::Color32::from_rgb(55, 55, 65)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        });
        painter.text(minus_rect.center(), egui::Align2::CENTER_CENTER, "\u{2212}",
            egui::FontId::proportional(11.0), egui::Color32::from_rgb(160, 160, 170));
        if minus_resp.clicked() {
            state.timeline_zoom = (state.timeline_zoom / 1.5).clamp(10.0, 500.0);
        }
        x += btn_w + 2.0;

        // + button
        let plus_rect = egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(btn_w, btn_h));
        let plus_resp = ui.interact(plus_rect, ui.id().with("zoom_plus"), egui::Sense::click());
        painter.rect_filled(plus_rect, 2.0, if plus_resp.hovered() {
            egui::Color32::from_rgb(55, 55, 65)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        });
        painter.text(plus_rect.center(), egui::Align2::CENTER_CENTER, "+",
            egui::FontId::proportional(11.0), egui::Color32::from_rgb(160, 160, 170));
        if plus_resp.clicked() {
            state.timeline_zoom = (state.timeline_zoom * 1.5).clamp(10.0, 500.0);
        }
    }

    fn draw_media_pool(
        &self,
        ui: &mut egui::Ui,
        painter: &egui::Painter,
        pool_rect: &egui::Rect,
        state: &mut AppState,
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

            let resp = ui.interact(thumb_rect, ui.id().with(("pool_item", item.id.0)), egui::Sense::click_and_drag());
            if resp.clicked() {
                // Single click: load in source monitor
                cmds.push(AppCommand::LoadSourceMedia { media_id: item.id });
            }
            if resp.double_clicked() {
                cmds.push(AppCommand::AppendClip { media_id: item.id });
            }
            // Drag: start dragging media to a track
            if resp.drag_started() {
                state.dragging_media = Some(item.id);
            }
            if resp.hovered() && state.dragging_media.is_none() {
                egui::show_tooltip_at_pointer(
                    ui.ctx(),
                    ui.layer_id(),
                    ui.id().with(("pool_thumb", item.id)),
                    |ui: &mut egui::Ui| {
                        ui.label(format!("{} (drag to track)", &item.filename));
                    },
                );
            }

            x += item_size + padding;
        }
    }

    fn draw_toggle_buttons(
        &self,
        ui: &mut egui::Ui,
        header_rect: &egui::Rect,
        y_offset: f32,
        state: &mut AppState,
        cmds: &mut Vec<AppCommand>,
    ) {
        let painter = ui.painter_at(*header_rect);
        let btn_w = 36.0;
        let btn_h = 14.0;
        let x = header_rect.left() + 2.0;

        // Snap toggle
        let snap_rect = egui::Rect::from_min_size(
            egui::pos2(x, y_offset + 2.0),
            egui::vec2(btn_w, btn_h),
        );
        let snap_resp = ui.interact(snap_rect, ui.id().with("snap_toggle"), egui::Sense::click());
        let snap_col = if state.snap_enabled {
            egui::Color32::from_rgb(60, 120, 180)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        };
        painter.rect_filled(snap_rect, 2.0, snap_col);
        painter.text(snap_rect.center(), egui::Align2::CENTER_CENTER, "Snap",
            egui::FontId::proportional(8.0), egui::Color32::WHITE);
        if snap_resp.clicked() {
            cmds.push(AppCommand::SetSnapEnabled(!state.snap_enabled));
        }

        // Ripple toggle
        let ripple_rect = egui::Rect::from_min_size(
            egui::pos2(x + btn_w + 2.0, y_offset + 2.0),
            egui::vec2(btn_w, btn_h),
        );
        let ripple_resp = ui.interact(ripple_rect, ui.id().with("ripple_toggle"), egui::Sense::click());
        let ripple_col = if state.ripple_enabled {
            egui::Color32::from_rgb(60, 120, 180)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        };
        painter.rect_filled(ripple_rect, 2.0, ripple_col);
        painter.text(ripple_rect.center(), egui::Align2::CENTER_CENTER, "Ripple",
            egui::FontId::proportional(8.0), egui::Color32::WHITE);
        if ripple_resp.clicked() {
            cmds.push(AppCommand::SetRippleEnabled(!state.ripple_enabled));
        }

        // Follow playhead toggle (cycles Off → Page → Smooth)
        let follow_w = 50.0;
        let follow_rect = egui::Rect::from_min_size(
            egui::pos2(x + (btn_w + 2.0) * 2.0, y_offset + 2.0),
            egui::vec2(follow_w, btn_h),
        );
        let follow_resp = ui.interact(follow_rect, ui.id().with("follow_toggle"), egui::Sense::click());
        let follow_col = if state.playhead_follow != PlayheadFollow::Off {
            egui::Color32::from_rgb(60, 120, 180)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        };
        painter.rect_filled(follow_rect, 2.0, follow_col);
        let follow_label = format!("Follow: {}", state.playhead_follow.label());
        painter.text(follow_rect.center(), egui::Align2::CENTER_CENTER, follow_label,
            egui::FontId::proportional(7.0), egui::Color32::WHITE);
        if follow_resp.clicked() {
            state.playhead_follow = state.playhead_follow.next();
        }
        follow_resp.on_hover_text("Playhead follow mode: Off / Page / Smooth");
    }

    fn draw_in_out_markers(
        &self,
        painter: &egui::Painter,
        ruler_rect: &egui::Rect,
        content_rect: &egui::Rect,
        state: &AppState,
    ) {
        let zoom = state.timeline_zoom;
        let scroll = state.timeline_scroll_x;
        let marker_color = egui::Color32::from_rgb(220, 160, 60);

        if let Some(in_pt) = state.sequence.in_point {
            let x = content_rect.left() + in_pt.to_secs_f64() as f32 * zoom - scroll;
            if x >= ruler_rect.left() && x <= ruler_rect.right() {
                painter.text(
                    egui::pos2(x, ruler_rect.bottom() - 2.0),
                    egui::Align2::LEFT_BOTTOM,
                    "[",
                    egui::FontId::monospace(14.0),
                    marker_color,
                );
            }
        }

        if let Some(out_pt) = state.sequence.out_point {
            let x = content_rect.left() + out_pt.to_secs_f64() as f32 * zoom - scroll;
            if x >= ruler_rect.left() && x <= ruler_rect.right() {
                painter.text(
                    egui::pos2(x, ruler_rect.bottom() - 2.0),
                    egui::Align2::RIGHT_BOTTOM,
                    "]",
                    egui::FontId::monospace(14.0),
                    marker_color,
                );
            }
        }

        // Highlight region between in and out
        if let (Some(in_pt), Some(out_pt)) = (state.sequence.in_point, state.sequence.out_point) {
            let x1 = content_rect.left() + in_pt.to_secs_f64() as f32 * zoom - scroll;
            let x2 = content_rect.left() + out_pt.to_secs_f64() as f32 * zoom - scroll;
            let x1 = x1.max(ruler_rect.left());
            let x2 = x2.min(ruler_rect.right());
            if x2 > x1 {
                let highlight = egui::Rect::from_min_max(
                    egui::pos2(x1, ruler_rect.top()),
                    egui::pos2(x2, ruler_rect.bottom()),
                );
                painter.rect_filled(
                    highlight,
                    0.0,
                    egui::Color32::from_rgba_premultiplied(220, 160, 60, 25),
                );
            }
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
        let fps = state.fps.as_f64() as f32;
        let rate = state.fps.rate();

        // Determine tick interval based on zoom level
        // At low zoom: major ticks every 10s or 5s; at high zoom: every second or per-frame
        let secs_per_pixel = 1.0 / zoom;
        let min_label_spacing_px = 60.0; // minimum pixels between labels
        let min_label_interval_secs = secs_per_pixel * min_label_spacing_px;

        // Choose a "nice" major interval in seconds
        let major_interval = if min_label_interval_secs > 60.0 { 60.0 }
            else if min_label_interval_secs > 30.0 { 30.0 }
            else if min_label_interval_secs > 10.0 { 10.0 }
            else if min_label_interval_secs > 5.0 { 5.0 }
            else if min_label_interval_secs > 2.0 { 2.0 }
            else if min_label_interval_secs > 1.0 { 1.0 }
            else if min_label_interval_secs > 0.5 { 0.5 }
            else { 1.0 / fps }; // per-frame at extreme zoom

        let start_secs = (scroll / zoom) - 1.0;
        let end_secs = (scroll + width) / zoom + 1.0;

        // Draw major ticks with labels
        let mut t = (start_secs / major_interval).floor() * major_interval;
        while t <= end_secs {
            if t >= 0.0 {
                let x = ruler_rect.left() + t * zoom - scroll;
                if x >= ruler_rect.left() - 10.0 && x <= ruler_rect.right() + 10.0 {
                    painter.line_segment(
                        [
                            egui::pos2(x, ruler_rect.bottom() - 12.0),
                            egui::pos2(x, ruler_rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(80, 80, 90)),
                    );

                    // Format label based on zoom level
                    let label = if major_interval < 1.0 {
                        // Show HH:MM:SS:FF at frame-level zoom
                        let tc = RationalTime::from_secs_f64(t as f64, rate);
                        tc.to_timecode_string()
                    } else {
                        let total_ms = (t * 1000.0) as i64;
                        let hh = total_ms / 3_600_000;
                        let mm = (total_ms / 60_000) % 60;
                        let ss = (total_ms / 1_000) % 60;
                        let ms = total_ms % 1000;
                        if hh > 0 {
                            format!("{hh}:{mm:02}:{ss:02}.{ms:03}")
                        } else if major_interval <= 1.0 {
                            // Show ms when interval is 1s or less
                            format!("{mm}:{ss:02}.{ms:03}")
                        } else if major_interval <= 5.0 {
                            // Show tenths at 2-5s intervals
                            let tenths = ms / 100;
                            if tenths > 0 {
                                format!("{mm}:{ss:02}.{tenths}")
                            } else {
                                format!("{mm}:{ss:02}")
                            }
                        } else {
                            format!("{mm}:{ss:02}")
                        }
                    };

                    painter.text(
                        egui::pos2(x + 3.0, ruler_rect.top() + 4.0),
                        egui::Align2::LEFT_TOP,
                        label,
                        egui::FontId::monospace(10.0),
                        egui::Color32::from_rgb(140, 140, 150),
                    );
                }
            }
            t += major_interval;
        }

        // Draw minor ticks (subdivisions between major ticks)
        let minor_interval = if major_interval >= 10.0 { major_interval / 5.0 }
            else if major_interval >= 2.0 { 1.0 }
            else if major_interval >= 1.0 { 0.5 }
            else { major_interval / 2.0 };

        if minor_interval < major_interval {
            let mut t = (start_secs / minor_interval).floor() * minor_interval;
            while t <= end_secs {
                if t >= 0.0 {
                    // Skip positions that coincide with major ticks
                    let is_major = (t / major_interval).round() * major_interval;
                    if (t - is_major).abs() > 0.001 {
                        let x = ruler_rect.left() + t * zoom - scroll;
                        if x >= ruler_rect.left() && x <= ruler_rect.right() {
                            painter.line_segment(
                                [
                                    egui::pos2(x, ruler_rect.bottom() - 6.0),
                                    egui::pos2(x, ruler_rect.bottom()),
                                ],
                                egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 70)),
                            );
                        }
                    }
                }
                t += minor_interval;
            }
        }

        // Draw per-frame ticks at high zoom (when frames are visible)
        let frame_width = zoom / fps;
        if frame_width >= 4.0 && major_interval >= (1.0 / fps) * 1.5 {
            let frame_secs = 1.0 / fps;
            let mut t = (start_secs / frame_secs).floor() * frame_secs;
            while t <= end_secs {
                if t >= 0.0 {
                    let x = ruler_rect.left() + t * zoom - scroll;
                    if x >= ruler_rect.left() && x <= ruler_rect.right() {
                        painter.line_segment(
                            [
                                egui::pos2(x, ruler_rect.bottom() - 3.0),
                                egui::pos2(x, ruler_rect.bottom()),
                            ],
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 60)),
                        );
                    }
                }
                t += frame_secs;
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
