use crate::icons::Icon;
use crate::state::{AppState, PlayheadFollow};
use klipp_core::{AppCommand, ClipId, RationalTime, Timecode, Tool};
use klipp_engine::PlaybackState;
use klipp_timeline::TrackKind;

const TRACK_HEADER_MIN_WIDTH: f32 = 80.0;
const TRACK_HEADER_MAX_WIDTH: f32 = 320.0;
const RULER_HEIGHT: f32 = 24.0;
const VIDEO_TRACK_HEIGHT: f32 = 50.0; // matches theme::TRACK_VIDEO_H
const AUDIO_TRACK_HEIGHT: f32 = 48.0; // taller for visible waveforms
const MEDIA_POOL_HEIGHT: f32 = 50.0;
const TRIM_HANDLE_WIDTH: f32 = 14.0;
const CLIP_GAP: f32 = 3.0; // visual gap between adjacent clips (px per side)
const STATUS_BAR_HEIGHT: f32 = 18.0;
const OVERVIEW_BAR_HEIGHT: f32 = 24.0;

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
    /// When true, skip internal toolbar/headers/status rendering (embedded in new shell)
    pub embedded: bool,
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
    /// Track rects from last frame — used for cross-track drag detection
    track_rects: Vec<(usize, TrackKind, egui::Rect)>,
}

struct DragState {
    clip_id: ClipId,
    track_idx: usize,
    start_x: f32,
    original_timeline_in: Timecode,
    linked_clip_id: Option<ClipId>,
    linked_original_in: Option<Timecode>,
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

        // Reset hovered clip each frame
        state.hovered_clip = None;

        // Handle scroll (horizontal pan) and Ctrl+scroll (cursor-anchored zoom).
        // egui 0.29: Ctrl+scroll → zoom_delta() (smooth_scroll_delta is zero).
        //            Normal scroll → smooth_scroll_delta (zoom_delta() is 1.0).
        // raw_scroll_delta is never consumed and always has the raw wheel input.
        let avail_rect = ui.available_rect_before_wrap();
        if ui.rect_contains_pointer(avail_rect) {
            // Read zoom_delta first (immutable — always available)
            let (zoom_factor, pointer_pos) = ui.input(|i| {
                (i.zoom_delta(), i.pointer.interact_pos())
            });

            // Ctrl+scroll → zoom (egui sets zoom_delta != 1.0)
            if (zoom_factor - 1.0).abs() > 0.001 {
                let old_zoom = state.timeline_zoom;
                let new_zoom = (old_zoom * zoom_factor).clamp(0.1, 2000.0);

                let content_left = avail_rect.left() + state.track_header_width;
                if let Some(cursor_pos) = pointer_pos {
                    let cursor_x = cursor_pos.x - content_left;
                    let time_at_cursor = (state.timeline_scroll_x + cursor_x) / old_zoom;
                    state.timeline_scroll_x = (time_at_cursor * new_zoom - cursor_x).max(0.0);
                }
                state.timeline_zoom = new_zoom;
            } else {
                // Normal scroll → horizontal pan.
                // Consume smooth_scroll_delta via input_mut so egui doesn't also scroll the panel.
                let smooth_sy = ui.input_mut(|i| {
                    let sy = i.smooth_scroll_delta.y;
                    i.smooth_scroll_delta = egui::Vec2::ZERO;
                    sy
                });
                if smooth_sy.abs() > 0.1 {
                    state.timeline_scroll_x =
                        (state.timeline_scroll_x - smooth_sy).max(0.0);
                    // User took manual control — disengage playhead follow
                    state.playhead_follow = PlayheadFollow::Off;
                }
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

        // Intersect painter clip with UI clip to prevent bleed
        let effective_clip = panel_rect.intersect(ui.clip_rect());
        let bg_painter = ui.painter_at(effective_clip);

        // Background
        bg_painter.rect_filled(panel_rect, 0.0, egui::Color32::from_rgb(25, 25, 30));

        // Track header area — skip when embedded (new track_headers.rs handles it)
        let header_rect = if self.embedded {
            // In embedded mode, use zero-width header (content fills full width)
            egui::Rect::from_min_size(panel_rect.min, egui::vec2(0.0, available.y))
        } else {
            let rect = egui::Rect::from_min_size(
                panel_rect.min,
                egui::vec2(state.track_header_width, available.y),
            );
            bg_painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(20, 20, 24));
            bg_painter.line_segment(
                [egui::pos2(rect.right(), rect.top()), egui::pos2(rect.right(), rect.bottom())],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(42, 42, 48)),
            );
            // Track header resize handle
            let handle_rect = egui::Rect::from_min_size(
                egui::pos2(rect.right() - 2.0, panel_rect.top()),
                egui::vec2(4.0, panel_rect.height()),
            );
            let handle_response = ui.interact(handle_rect, ui.id().with("header_resize"), egui::Sense::drag());
            if handle_response.dragged() {
                state.track_header_width = (state.track_header_width + handle_response.drag_delta().x)
                    .clamp(TRACK_HEADER_MIN_WIDTH, TRACK_HEADER_MAX_WIDTH);
            }
            if handle_response.hovered() || handle_response.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                bg_painter.line_segment(
                    [egui::pos2(rect.right() - 1.0, panel_rect.top()), egui::pos2(rect.right() - 1.0, panel_rect.bottom())],
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(80, 80, 100)),
                );
            }
            rect
        };

        // Timeline content area — starts after header (0px in embedded mode)
        let content_left = if self.embedded {
            panel_rect.left()
        } else {
            panel_rect.left() + state.track_header_width
        };
        let content_rect = egui::Rect::from_min_max(
            egui::pos2(content_left, panel_rect.top()),
            panel_rect.max,
        );
        state.last_timeline_content_width = content_rect.width();
        let painter = bg_painter.with_clip_rect(content_rect.intersect(ui.clip_rect()));

        // Overview / minimap bar (above ruler) — skip when embedded
        let overview_height = if state.show_overview_bar && !self.embedded { OVERVIEW_BAR_HEIGHT } else { 0.0 };
        if state.show_overview_bar && !self.embedded {
            let overview_rect = egui::Rect::from_min_size(
                content_rect.min,
                egui::vec2(content_rect.width(), OVERVIEW_BAR_HEIGHT),
            );
            self.draw_overview_bar(&painter, &overview_rect, state);
        }

        // Overview bar interaction — raw pointer (ui.interact doesn't work in manual layout)
        // Skip when embedded — the new timeline shell handles its own overview
        let mut overview_bar_consumed = false;
        if state.show_overview_bar && !self.embedded {
            let overview_rect = egui::Rect::from_min_size(
                content_rect.min,
                egui::vec2(content_rect.width(), OVERVIEW_BAR_HEIGHT),
            );
            let dur_secs = state.sequence().total_duration().to_secs_f64() as f32;
            if dur_secs > 0.0 {
                let px_per_sec = overview_rect.width() / dur_secs;
                let zoom = state.timeline_zoom;
                let content_w = state.last_timeline_content_width;
                let (pointer_pos, primary_down, primary_released, drag_delta) = ui.input(|i| (
                    i.pointer.latest_pos(),
                    i.pointer.primary_down(),
                    i.pointer.primary_released(),
                    i.pointer.delta(),
                ));
                let in_overview = pointer_pos.is_some_and(|p| overview_rect.contains(p));

                // Drag: pointer is in overview and button held with movement
                if in_overview && primary_down && drag_delta.x.abs() > 0.5 {
                    overview_bar_consumed = true;
                    let delta_secs = drag_delta.x / px_per_sec;
                    state.timeline_scroll_x = (state.timeline_scroll_x + delta_secs * zoom).max(0.0);
                }
                // Click: pointer released in overview — jump viewport
                if in_overview && primary_released {
                    overview_bar_consumed = true;
                    if let Some(pos) = pointer_pos {
                        let click_secs = (pos.x - overview_rect.left()) / px_per_sec;
                        let view_width_secs = content_w / zoom;
                        let new_start_secs = click_secs - view_width_secs / 2.0;
                        state.timeline_scroll_x = (new_start_secs * zoom).max(0.0);
                    }
                }
            }
        }

        // Time ruler — skip when embedded (new ruler.rs handles it)
        let ruler_height_used = if self.embedded { 0.0 } else { RULER_HEIGHT };
        let ruler_rect = egui::Rect::from_min_size(
            egui::pos2(content_rect.left(), content_rect.top() + overview_height),
            egui::vec2(content_rect.width(), ruler_height_used),
        );
        if !self.embedded {
            painter.rect_filled(ruler_rect, 0.0, egui::Color32::from_rgb(30, 30, 34));
            self.draw_ruler_ticks(&painter, &ruler_rect, state);
        }

        // Sequence markers + ruler click — skip when embedded
        if !self.embedded {
            let zoom = state.timeline_zoom;
            let scroll = state.timeline_scroll_x;
            for marker in &state.sequence().markers {
                let x = content_rect.left() + marker.position.to_secs_f64() as f32 * zoom - scroll;
                if x >= ruler_rect.left() - 6.0 && x <= ruler_rect.right() + 6.0 {
                    let (r, g, b) = marker.color.to_rgb();
                    let color = egui::Color32::from_rgb(r, g, b);
                    let top = ruler_rect.bottom() - 10.0;
                    let bot = ruler_rect.bottom() - 1.0;
                    let half_w = 4.0;
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(x - half_w, top),
                            egui::pos2(x + half_w, top),
                            egui::pos2(x, bot),
                        ],
                        color,
                        egui::Stroke::NONE,
                    ));
                }
            }

            let ruler_response = ui.interact(ruler_rect, ui.id().with("ruler"), egui::Sense::click_and_drag());
            if !overview_bar_consumed && (ruler_response.clicked() || ruler_response.dragged()) {
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
        }

        // Media pool, zoom buttons, toggle buttons — skip when embedded
        if !self.embedded {
            let pool_rect = egui::Rect::from_min_size(
                egui::pos2(content_rect.left(), content_rect.top() + RULER_HEIGHT),
                egui::vec2(content_rect.width(), MEDIA_POOL_HEIGHT),
            );
            painter.rect_filled(pool_rect, 0.0, egui::Color32::from_rgb(20, 20, 24));
            self.draw_media_pool(ui, &painter, &pool_rect, state, &mut cmds);
            self.draw_zoom_buttons(ui, &header_rect, &ruler_rect, state);
            self.draw_toggle_buttons(ui, &header_rect, content_rect.top() + RULER_HEIGHT, state, &mut cmds);
        }

        // Reset snap line
        self.snap_line_x = None;

        // Draw tracks: Video 2, Video 1, Audio 1
        let pool_h = if self.embedded { 0.0 } else { MEDIA_POOL_HEIGHT };
        let tracks_start_y = content_rect.top() + overview_height + ruler_height_used + pool_h;
        let mut track_y = tracks_start_y;

        let track_count = state.sequence().tracks.len();

        // Pre-compute all track rects so cross-track drag can find any track
        self.track_rects.clear();
        {
            let mut pre_y = tracks_start_y;
            for ti in 0..track_count {
                let tk = state.sequence().tracks[ti].kind;
                let th = match tk {
                    TrackKind::Audio => AUDIO_TRACK_HEIGHT,
                    _ => VIDEO_TRACK_HEIGHT,
                };
                let rect = egui::Rect::from_min_size(
                    egui::pos2(content_rect.left(), pre_y),
                    egui::vec2(content_rect.width(), th),
                );
                self.track_rects.push((ti, tk, rect));
                pre_y += th;
            }
        }

        for track_idx in 0..track_count {
            let track_kind = state.sequence().tracks[track_idx].kind;
            let track_name = state.sequence().tracks[track_idx].name.clone();
            let track_height = match track_kind {
                TrackKind::Audio => AUDIO_TRACK_HEIGHT,
                _ => VIDEO_TRACK_HEIGHT,
            };

            // Draw track header label and controls — skip when embedded
            if !self.embedded {
            let label_color = match track_kind {
                TrackKind::Audio => egui::Color32::from_rgb(100, 150, 110),
                TrackKind::Text | TrackKind::Subtitle => egui::Color32::from_rgb(180, 150, 100),
                _ => egui::Color32::from_rgb(140, 140, 150),
            };
            let track_locked = state.sequence().tracks[track_idx].locked;

            // Lock state read above — button placed in track controls block to avoid context menu overlap

            // Dim header tint for locked tracks
            if track_locked {
                let dim_rect = egui::Rect::from_min_size(
                    egui::pos2(header_rect.left(), track_y),
                    egui::vec2(state.track_header_width, track_height),
                );
                bg_painter.rect_filled(dim_rect, 0.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 60));
            }

            bg_painter.text(
                egui::pos2(header_rect.left() + 4.0, track_y + 8.0),
                egui::Align2::LEFT_TOP,
                &track_name,
                egui::FontId::proportional(10.0),
                if track_locked { egui::Color32::from_rgb(90, 90, 100) } else { label_color },
            );

            // Track control buttons
            {
                let track = &state.sequence().tracks[track_idx];
                match track_kind {
                    TrackKind::Video | TrackKind::Text | TrackKind::Subtitle => {
                        // [L] lock toggle
                        let l_rect = egui::Rect::from_min_size(
                            egui::pos2(header_rect.right() - 42.0, track_y + 4.0),
                            egui::vec2(18.0, 14.0),
                        );
                        let l_resp = ui.interact(l_rect, ui.id().with(("lock_btn", track_idx)), egui::Sense::click());
                        let l_bg = if track_locked {
                            egui::Color32::from_rgb(160, 120, 40)
                        } else if l_resp.hovered() {
                            egui::Color32::from_rgb(50, 50, 60)
                        } else {
                            egui::Color32::from_rgb(35, 35, 42)
                        };
                        bg_painter.rect_filled(l_rect, 2.0, l_bg);
                        let l_icon = if track_locked { Icon::Lock } else { Icon::LockOpen };
                        let l_color = if track_locked { egui::Color32::WHITE } else { egui::Color32::from_rgb(100, 100, 110) };
                        l_icon.image(10.0).tint(l_color).paint_at(ui, l_rect.shrink(2.0));
                        if l_resp.clicked() {
                            let track_id = state.sequence().tracks[track_idx].id;
                            cmds.push(AppCommand::SetTrackLocked { track_id, locked: !track_locked });
                        }

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
                        let v_icon = if track.visible { Icon::Eye } else { Icon::EyeSlash };
                        v_icon.image(10.0).tint(v_col).paint_at(ui, v_rect.shrink(2.0));
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
                        // [L] lock toggle (top-right of audio header)
                        let al_rect = egui::Rect::from_min_size(
                            egui::pos2(header_rect.right() - 22.0, track_y + 4.0),
                            egui::vec2(18.0, 14.0),
                        );
                        let al_resp = ui.interact(al_rect, ui.id().with(("lock_btn", track_idx)), egui::Sense::click());
                        let al_bg = if track_locked {
                            egui::Color32::from_rgb(160, 120, 40)
                        } else if al_resp.hovered() {
                            egui::Color32::from_rgb(50, 50, 60)
                        } else {
                            egui::Color32::from_rgb(35, 35, 42)
                        };
                        bg_painter.rect_filled(al_rect, 2.0, al_bg);
                        let al_icon = if track_locked { Icon::Lock } else { Icon::LockOpen };
                        let al_color = if track_locked { egui::Color32::WHITE } else { egui::Color32::from_rgb(100, 100, 110) };
                        al_icon.image(10.0).tint(al_color).paint_at(ui, al_rect.shrink(2.0));
                        if al_resp.clicked() {
                            let track_id = state.sequence().tracks[track_idx].id;
                            cmds.push(AppCommand::SetTrackLocked { track_id, locked: !track_locked });
                        }

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
                        let m_icon = if track.muted { Icon::SpeakerSlash } else { Icon::SpeakerHigh };
                        m_icon.image(10.0).tint(egui::Color32::WHITE).paint_at(ui, m_rect.shrink(1.0));
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
                            cmds.push(AppCommand::SetTrackSolo { track_index: track_idx, solo: !track.solo });
                        }

                        // Volume drag control (like video opacity)
                        let vol_rect = egui::Rect::from_min_size(
                            egui::pos2(s_rect.right() + 4.0, btn_y),
                            egui::vec2(header_rect.right() - s_rect.right() - 8.0, 12.0),
                        );
                        let vol_resp = ui.interact(
                            vol_rect,
                            ui.id().with(("vol_ctrl", track_idx)),
                            egui::Sense::click_and_drag(),
                        );
                        bg_painter.rect_filled(vol_rect, 2.0, egui::Color32::from_rgb(30, 30, 38));
                        let fill_w = vol_rect.width() * (track.volume / 1.3);
                        let fill_rect = egui::Rect::from_min_size(
                            vol_rect.left_top(),
                            egui::vec2(fill_w, vol_rect.height()),
                        );
                        bg_painter.rect_filled(fill_rect, 2.0, egui::Color32::from_rgb(50, 100, 60));
                        let vol_pct = (track.volume * 100.0) as u32;
                        bg_painter.text(
                            vol_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            format!("{}%", vol_pct),
                            egui::FontId::proportional(9.0),
                            egui::Color32::from_rgb(180, 200, 180),
                        );
                        if vol_resp.double_clicked() {
                            cmds.push(AppCommand::SetTrackVolume {
                                track_index: track_idx,
                                volume: 1.0,
                            });
                        } else if vol_resp.dragged() {
                            let dx = vol_resp.drag_delta().x;
                            let new_vol = (track.volume + dx / vol_rect.width() * 1.3).clamp(0.0, 1.3);
                            cmds.push(AppCommand::SetTrackVolume {
                                track_index: track_idx,
                                volume: new_vol,
                            });
                        }

                        // Pan slider (horizontal drag, double-click resets to center)
                        let pan_y = btn_y - 14.0;
                        let pan_rect = egui::Rect::from_min_size(
                            egui::pos2(header_rect.left() + 4.0, pan_y),
                            egui::vec2(header_rect.width() - 8.0, 10.0),
                        );
                        let pan_resp = ui.interact(
                            pan_rect,
                            ui.id().with(("pan_ctrl", track_idx)),
                            egui::Sense::click_and_drag(),
                        );
                        bg_painter.rect_filled(pan_rect, 2.0, egui::Color32::from_rgb(30, 30, 38));
                        // Center marker
                        let cx = pan_rect.center().x;
                        bg_painter.line_segment(
                            [egui::pos2(cx, pan_rect.top()), egui::pos2(cx, pan_rect.bottom())],
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(60, 60, 70)),
                        );
                        // Pan indicator
                        let pan_x = cx + (track.pan * pan_rect.width() * 0.5);
                        bg_painter.rect_filled(
                            egui::Rect::from_center_size(egui::pos2(pan_x, pan_rect.center().y), egui::vec2(4.0, 8.0)),
                            1.0,
                            egui::Color32::from_rgb(140, 180, 150),
                        );
                        // Pan label
                        let pan_label = if track.pan.abs() < 0.05 {
                            "C".to_string()
                        } else if track.pan < 0.0 {
                            format!("L{}", (track.pan.abs() * 100.0) as u32)
                        } else {
                            format!("R{}", (track.pan * 100.0) as u32)
                        };
                        bg_painter.text(
                            egui::pos2(pan_rect.right() - 2.0, pan_rect.center().y),
                            egui::Align2::RIGHT_CENTER,
                            &pan_label,
                            egui::FontId::proportional(7.0),
                            egui::Color32::from_rgb(140, 140, 150),
                        );
                        if pan_resp.dragged() {
                            let dx = pan_resp.drag_delta().x;
                            let new_pan = (track.pan + dx / (pan_rect.width() * 0.5)).clamp(-1.0, 1.0);
                            cmds.push(AppCommand::SetTrackPan {
                                track_index: track_idx,
                                pan: new_pan,
                            });
                        } else if pan_resp.double_clicked() {
                            cmds.push(AppCommand::SetTrackPan {
                                track_index: track_idx,
                                pan: 0.0,
                            });
                        }

                        // VU meter bars (stereo, horizontal)
                        let vu_y = track_y + 4.0;
                        let vu_w = header_rect.width() - 8.0;
                        let vu_h = 3.0;
                        let (peak_l, peak_r) = state.track_peak_levels
                            .get(track_idx)
                            .copied()
                            .unwrap_or((0.0, 0.0));
                        for (ch, peak) in [(0, peak_l), (1, peak_r)] {
                            let bar_rect = egui::Rect::from_min_size(
                                egui::pos2(header_rect.left() + 4.0, vu_y + ch as f32 * (vu_h + 1.0)),
                                egui::vec2(vu_w, vu_h),
                            );
                            bg_painter.rect_filled(bar_rect, 1.0, egui::Color32::from_rgb(20, 20, 25));
                            let fill = (peak.min(1.0) * vu_w).max(0.0);
                            if fill > 0.0 {
                                let color = if peak > 0.9 {
                                    egui::Color32::from_rgb(220, 50, 50)
                                } else if peak > 0.7 {
                                    egui::Color32::from_rgb(220, 180, 40)
                                } else {
                                    egui::Color32::from_rgb(50, 180, 70)
                                };
                                let fill_rect = egui::Rect::from_min_size(
                                    bar_rect.left_top(),
                                    egui::vec2(fill, vu_h),
                                );
                                bg_painter.rect_filled(fill_rect, 1.0, color);
                            }
                        }
                    }
                }
            }

            // Track header right-click context menu
            {
                // Context menu area: only the track name area at top of header
                let ctx_h = 22.0;
                let hdr_rect = egui::Rect::from_min_size(
                    egui::pos2(header_rect.left(), track_y),
                    egui::vec2(state.track_header_width, ctx_h),
                );
                let hdr_resp = ui.interact(
                    hdr_rect,
                    ui.id().with(("track_hdr_ctx", track_idx)),
                    egui::Sense::click(),
                );
                let track_id = state.sequence().tracks[track_idx].id;
                hdr_resp.context_menu(|ui| {
                    ui.set_min_width(160.0);
                    if ui.button("Add Track Above").clicked() {
                        cmds.push(AppCommand::AddTrackAbove { track_index: track_idx });
                        ui.close_menu();
                    }
                    if ui.button("Add Track Below").clicked() {
                        cmds.push(AppCommand::AddTrackBelow { track_index: track_idx });
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("Rename Track").clicked() {
                        // For now, auto-rename to next name
                        ui.close_menu();
                    }
                    ui.separator();
                    let has_clips = !state.sequence().tracks[track_idx].clips.is_empty();
                    let remove_resp = ui.add_enabled(!has_clips, egui::Button::new("Remove Track"));
                    if !has_clips && remove_resp.clicked() {
                        cmds.push(AppCommand::RemoveTrack { track_id });
                        ui.close_menu();
                    }
                    if has_clips {
                        remove_resp.on_disabled_hover_text("Remove clips first");
                    }
                    if ui.button("Remove Track (with clips)").clicked() {
                        cmds.push(AppCommand::RemoveTrack { track_id });
                        ui.close_menu();
                    }
                });
            }

            // Track separator
            bg_painter.line_segment(
                [
                    egui::pos2(panel_rect.left(), track_y),
                    egui::pos2(panel_rect.right(), track_y),
                ],
                egui::Stroke::new(1.0, egui::Color32::from_rgb(35, 35, 40)),
            );

            } // end if !self.embedded (track header controls)

            let track_rect = egui::Rect::from_min_size(
                egui::pos2(content_rect.left(), track_y),
                egui::vec2(content_rect.width(), track_height),
            );

            // Draw clips for this track (clipped to lane rect)
            let lane_painter = painter.with_clip_rect(track_rect);
            self.draw_track_clips(
                ui, &lane_painter, &track_rect, &content_rect,
                state, &mut cmds, track_idx, true,
            );

            // Media pool drag-and-drop: highlight track when dragging over it
            if let Some(drag_media_id) = state.dragging_media {
                let hover_pos = ui.input(|i| i.pointer.hover_pos());
                let any_down = ui.input(|i| i.pointer.any_down());
                if let Some(pos) = hover_pos {
                    if track_rect.contains(pos) {
                        // Check media type vs track type compatibility:
                        // Audio-only → audio tracks only. Video → video tracks only.
                        let is_audio_only = state.media_pool.get(&drag_media_id)
                            .map(|item| item.video_codec.is_empty())
                            .unwrap_or(false);
                        let compatible = match track_kind {
                            TrackKind::Audio => is_audio_only,
                            _ => !is_audio_only,
                        };
                        if compatible {
                            // Highlight drop target (cyan = valid)
                            painter.rect_stroke(
                                track_rect,
                                0.0,
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 180, 255)),
                            );
                            // Drop on release
                            if !any_down {
                                state.dragging_media = None;
                                tracing::info!("Media drop: {:?} -> track {} ({})", drag_media_id, track_idx, track_name);
                                cmds.push(AppCommand::AppendClipToTrack {
                                    media_id: drag_media_id,
                                    track_index: track_idx,
                                });
                            }
                        } else {
                            // Show red border for invalid drop target
                            painter.rect_stroke(
                                track_rect,
                                0.0,
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(200, 60, 60)),
                            );
                        }
                    }
                }
            }

            track_y += track_height;
        }

        // Media drag released outside any track — auto-create track if enabled
        if let Some(drag_media_id) = state.dragging_media {
            if !ui.input(|i| i.pointer.any_down()) {
                let mut handled = false;
                if state.auto_create_tracks {
                    if let Some(cursor_pos) = ui.input(|i| i.pointer.hover_pos()) {
                        let is_audio_only = state.media_pool.get(&drag_media_id)
                            .map(|item| item.video_codec.is_empty())
                            .unwrap_or(false);
                        // Check if cursor is above topmost track or below bottommost track
                        let first_rect_top = self.track_rects.first().map(|(_, _, r)| r.top());
                        let last_rect_bottom = self.track_rects.last().map(|(_, _, r)| r.bottom());
                        let above_all = first_rect_top.is_some_and(|top| cursor_pos.y < top);
                        let below_all = last_rect_bottom.is_some_and(|bot| cursor_pos.y > bot);

                        if above_all && !is_audio_only {
                            // Create new video track at top
                            let name = state.sequence().next_track_name(TrackKind::Video);
                            let track_idx = {
                                state.sequence_mut().add_video_track(name);
                                // New video track inserted before first audio track — find it
                                // It's the topmost video track (index 0 after insertion)
                                0
                            };
                            cmds.push(AppCommand::AppendClipToTrack {
                                media_id: drag_media_id,
                                track_index: track_idx,
                            });
                            handled = true;
                            tracing::info!("Auto-created video track for media drop above timeline");
                        } else if below_all && is_audio_only {
                            // Create new audio track at bottom
                            let name = state.sequence().next_track_name(TrackKind::Audio);
                            state.sequence_mut().add_audio_track(name);
                            let track_idx = state.sequence().tracks.len() - 1;
                            cmds.push(AppCommand::AppendClipToTrack {
                                media_id: drag_media_id,
                                track_index: track_idx,
                            });
                            handled = true;
                            tracing::info!("Auto-created audio track for media drop below timeline");
                        } else if below_all && !is_audio_only {
                            // Video media below all tracks — create video track above audio
                            let name = state.sequence().next_track_name(TrackKind::Video);
                            state.sequence_mut().add_video_track(name);
                            // add_video_track inserts before first audio track
                            let track_idx = state.sequence().tracks.iter()
                                .rposition(|t| t.kind == TrackKind::Video)
                                .unwrap_or(0);
                            cmds.push(AppCommand::AppendClipToTrack {
                                media_id: drag_media_id,
                                track_index: track_idx,
                            });
                            handled = true;
                            tracing::info!("Auto-created video track for media drop below timeline");
                        } else if above_all && is_audio_only {
                            // Audio media above all tracks — create audio track at bottom
                            let name = state.sequence().next_track_name(TrackKind::Audio);
                            state.sequence_mut().add_audio_track(name);
                            let track_idx = state.sequence().tracks.len() - 1;
                            cmds.push(AppCommand::AppendClipToTrack {
                                media_id: drag_media_id,
                                track_index: track_idx,
                            });
                            handled = true;
                            tracing::info!("Auto-created audio track for media drop above timeline");
                        }
                    }
                }
                if handled {
                    state.dirty = true;
                    cmds.push(AppCommand::SequenceUpdated);
                }
                state.dragging_media = None;
            }
        }

        // Draw media drag ghost — a translucent clip preview following the cursor
        if let Some(drag_media_id) = state.dragging_media {
            if let Some(cursor_pos) = ui.input(|i| i.pointer.latest_pos()) {
                let item_name = state.media_pool.get(&drag_media_id)
                    .map(|m| m.filename.clone())
                    .unwrap_or_default();
                let item_dur = state.media_pool.get(&drag_media_id)
                    .map(|m| m.duration.to_secs_f64() as f32)
                    .unwrap_or(1.0);

                let ghost_w = (item_dur * state.timeline_zoom).clamp(60.0, 300.0);
                let ghost_h = 36.0;
                let ghost_rect = egui::Rect::from_min_size(
                    egui::pos2(cursor_pos.x - ghost_w * 0.5, cursor_pos.y - ghost_h * 0.5),
                    egui::vec2(ghost_w, ghost_h),
                );

                // Use a top-level painter so the ghost draws above everything
                let ghost_painter = ui.ctx().layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    ui.id().with("drag_ghost"),
                ));

                // Clip background
                ghost_painter.rect_filled(ghost_rect, 4.0,
                    egui::Color32::from_rgba_unmultiplied(60, 80, 140, 200));
                ghost_painter.rect_stroke(ghost_rect, 4.0,
                    egui::Stroke::new(1.5, egui::Color32::from_rgba_unmultiplied(120, 160, 240, 220)));

                // Thumbnail if available
                if let Some(tex) = state.thumbnail_textures.get(&drag_media_id) {
                    let thumb_rect = egui::Rect::from_min_size(
                        ghost_rect.min + egui::vec2(3.0, 3.0),
                        egui::vec2(ghost_h - 6.0, ghost_h - 6.0),
                    );
                    ghost_painter.image(
                        tex.id(),
                        thumb_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                }

                // Filename label
                let label_x = ghost_rect.left() + ghost_h + 2.0;
                let truncated = if item_name.len() > 20 {
                    format!("{}...", &item_name[..17])
                } else {
                    item_name
                };
                ghost_painter.text(
                    egui::pos2(label_x, ghost_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &truncated,
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );

                // Duration label
                let dur_label = format!("{:.1}s", item_dur);
                ghost_painter.text(
                    egui::pos2(ghost_rect.right() - 4.0, ghost_rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    &dur_label,
                    egui::FontId::monospace(9.0),
                    egui::Color32::from_rgb(180, 180, 200),
                );

                // Request repaint during drag for smooth following
                ui.ctx().request_repaint();
            }
        }

        // Range guide lines — 1px amber vertical lines at in/out points through track area
        {
            let theme = crate::theme::KlippTheme::default();
            let zoom = state.timeline_zoom;
            let scroll = state.timeline_scroll_x;
            if let Some(in_pt) = state.sequence().in_point {
                let x = content_rect.left() + in_pt.to_secs_f64() as f32 * zoom - scroll;
                if x >= content_rect.left() && x <= content_rect.right() {
                    crate::timeline::range_visuals::draw_guide_line(&painter, x, tracks_start_y, content_rect.bottom(), &theme);
                }
            }
            if let Some(out_pt) = state.sequence().out_point {
                let x = content_rect.left() + out_pt.to_secs_f64() as f32 * zoom - scroll;
                if x >= content_rect.left() && x <= content_rect.right() {
                    crate::timeline::range_visuals::draw_guide_line(&painter, x, tracks_start_y, content_rect.bottom(), &theme);
                }
            }
        }

        // Playhead line + follow mode
        let seq_dur = state.sequence_duration();
        if state.playback != PlaybackState::Idle || seq_dur.value > 0 {
            let playhead_secs = state.playhead_smooth_secs as f32;
            let view_width = content_rect.width();

            // Auto-scroll based on follow mode — only during playback, not on user clicks
            if state.playback == PlaybackState::Playing {
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
                        // Keep playhead at ~25% from left edge during playback
                        let target_scroll =
                            (playhead_secs * state.timeline_zoom - view_width * 0.25).max(0.0);
                        // Smooth lerp to avoid jarring jumps
                        state.timeline_scroll_x +=
                            (target_scroll - state.timeline_scroll_x) * 0.15;
                    }
                    PlayheadFollow::Off => {}
                }
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

        // Snap indicator line (amber, full height, with timecode label)
        if let Some(snap_x) = self.snap_line_x {
            if snap_x >= content_rect.left() && snap_x <= content_rect.right() {
                let snap_color = egui::Color32::from_rgb(232, 160, 32);
                // Thick amber line spanning full track area
                painter.line_segment(
                    [
                        egui::pos2(snap_x, tracks_start_y),
                        egui::pos2(snap_x, content_rect.bottom()),
                    ],
                    egui::Stroke::new(2.0, snap_color),
                );
                // Glow effect — wider translucent line behind
                painter.line_segment(
                    [
                        egui::pos2(snap_x, tracks_start_y),
                        egui::pos2(snap_x, content_rect.bottom()),
                    ],
                    egui::Stroke::new(6.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 50)),
                );
                // Timecode label with background pill at top of snap line
                let snap_secs = (snap_x - content_rect.left() + state.timeline_scroll_x) / state.timeline_zoom;
                let snap_tc = RationalTime::from_secs_f64(snap_secs as f64, state.fps.rate());
                let label = snap_tc.to_timecode_string();
                let label_pos = egui::pos2(snap_x + 4.0, tracks_start_y + 3.0);
                // Background pill for readability
                let text_galley = painter.layout_no_wrap(
                    label.clone(),
                    egui::FontId::monospace(9.0),
                    snap_color,
                );
                let pill_rect = egui::Rect::from_min_size(
                    egui::pos2(label_pos.x - 2.0, label_pos.y - 1.0),
                    egui::vec2(text_galley.size().x + 4.0, text_galley.size().y + 2.0),
                );
                painter.rect_filled(pill_rect, 2.0, egui::Color32::from_rgba_unmultiplied(20, 20, 24, 220));
                painter.text(
                    label_pos,
                    egui::Align2::LEFT_TOP,
                    &label,
                    egui::FontId::monospace(9.0),
                    snap_color,
                );
            }
        }

        // --- Status bar --- (skip when embedded)
        if self.embedded { return cmds; }
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
        let total_clips: usize = state.sequence().tracks.iter().map(|t| t.clips.len()).sum();
        let mut left_text = format!("{} clip{}", total_clips, if total_clips != 1 { "s" } else { "" });

        if let Some(clip_id) = state.selected_clip {
            if let Some(clip) = state.sequence().tracks.iter().flat_map(|t| t.clips.iter()).find(|c| c.id == clip_id) {
                if let Some(media) = clip.media_id().and_then(|mid| state.media_pool.get(&mid)) {
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
        // Show zoom as human-readable time span visible in the viewport
        let view_secs = content_rect.width() / state.timeline_zoom;
        let zoom_label = if view_secs >= 7200.0 {
            format!("{:.0}h", view_secs / 3600.0)
        } else if view_secs >= 120.0 {
            format!("{:.0}m", view_secs / 60.0)
        } else if view_secs >= 1.0 {
            format!("{:.0}s", view_secs)
        } else {
            let fps = state.sequence().fps.num as f32 / state.sequence().fps.den as f32;
            format!("{:.0}f", view_secs * fps)
        };
        let mut right_text = format!("Zoom: {zoom_label} visible");
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

        // ── Audio keyframe rubber band drag update ─────────────────────
        // Process dragging_audio_keyframe: update position on drag, clear on release.
        let pointer_released = ui.input(|i| i.pointer.primary_released());
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        if let Some(ref drag_kf) = state.dragging_audio_keyframe.clone() {
            if pointer_released || !pointer_down {
                // Release: clear drag state
                state.dragging_audio_keyframe = None;
            } else if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                // Find the clip's screen rect to map position back to time/value
                let zoom = state.timeline_zoom;
                let scroll = state.timeline_scroll_x;
                // Find clip in sequence to get its rect
                for (ti, track) in state.sequence().tracks.iter().enumerate() {
                    if let Some(clip) = track.find_clip(drag_kf.clip_id) {
                        let track_height = match track.kind {
                            TrackKind::Audio => AUDIO_TRACK_HEIGHT,
                            _ => VIDEO_TRACK_HEIGHT,
                        };
                        // Find track_rect from track_rects cache
                        if let Some((_, _, track_rect)) = self.track_rects.iter().find(|(idx, _, _)| *idx == ti) {
                            let content_left = track_rect.left(); // approximation — content_rect not available here
                            let clip_rect_approx = {
                                let cx = content_left + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                                let cw = clip.duration().to_secs_f64() as f32 * zoom;
                                egui::Rect::from_min_size(
                                    egui::pos2(cx + CLIP_GAP, track_rect.top() + 2.0),
                                    egui::vec2((cw - CLIP_GAP * 2.0).max(2.0), track_height - 4.0),
                                )
                            };
                            let clip_dur_secs = clip.duration().to_secs_f64();
                            if clip_dur_secs > 0.0 && clip_rect_approx.width() > 0.0 {
                                let frac = ((pos.x - clip_rect_approx.left()) / clip_rect_approx.width()).clamp(0.0, 1.0) as f64;
                                let new_time = frac * clip_dur_secs;
                                let normalized = ((clip_rect_approx.bottom() - pos.y) / clip_rect_approx.height()).clamp(0.0, 1.0);
                                // Map normalized position to param range
                                let (p_min, p_range) = match drag_kf.param {
                                    klipp_core::AudioParam::Volume => (0.0_f32, 2.0_f32),
                                    klipp_core::AudioParam::Pan => (-1.0_f32, 2.0_f32),
                                    klipp_core::AudioParam::Gain => (0.0_f32, 4.0_f32),
                                };
                                let new_value = p_min + normalized * p_range;
                                cmds.push(AppCommand::MoveAudioKeyframe {
                                    clip_id: drag_kf.clip_id,
                                    param: drag_kf.param,
                                    index: drag_kf.keyframe_index,
                                    new_time_secs: new_time,
                                    new_value,
                                });
                            }
                        }
                        break;
                    }
                }
            }
        }

        // ── Flat rubber band drag release ────────────────────────────────
        if state.dragging_rubber_band.is_some() && (pointer_released || !pointer_down) {
            state.dragging_rubber_band = None;
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
        let track_kind = state.sequence().tracks[track_idx].kind;
        let clips: Vec<_> = state.sequence().tracks[track_idx].clips.clone();

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

            // Gap clip rendering — hatched pattern with duration label
            if clip.kind.is_gap() {
                let gap_bg = egui::Color32::from_rgb(0x0a, 0x0a, 0x0c);
                painter.rect_filled(clip_rect, 3.0, gap_bg);

                // Diagonal hatched stripes
                let stripe_color = egui::Color32::from_rgba_unmultiplied(232, 160, 32, 25);
                let stripe_spacing = 8.0_f32;
                let clip_painter = painter.with_clip_rect(clip_rect);
                let mut sx = clip_rect.min.x - clip_rect.height();
                while sx < clip_rect.max.x {
                    let p1 = egui::pos2(sx, clip_rect.max.y);
                    let p2 = egui::pos2(sx + clip_rect.height(), clip_rect.min.y);
                    clip_painter.line_segment([p1, p2], egui::Stroke::new(1.0, stripe_color));
                    sx += stripe_spacing;
                }

                // Amber dashed border
                painter.rect_stroke(
                    clip_rect,
                    3.0,
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 80)),
                );

                // Duration label centered
                let dur = clip.duration();
                let secs = dur.to_secs_f64();
                let label = if secs >= 1.0 {
                    format!("{:.1}s", secs)
                } else {
                    format!("{:.0}f", dur.value as f64)
                };
                painter.text(
                    clip_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    &label,
                    egui::FontId::monospace(9.0),
                    egui::Color32::from_rgba_unmultiplied(232, 160, 32, 120),
                );

                // Gap clips ARE selectable — handle click (only inside visible content)
                if let Some(pos) = pointer_pos {
                    if clip_rect.contains(pos) && content_rect.contains(pos) {
                        if pointer_released {
                            state.selected_clip = Some(clip.id);
                        }
                        state.hovered_clip = Some(clip.id);
                    }
                }

                // Selection highlight
                if state.selected_clip == Some(clip.id) {
                    painter.rect_stroke(
                        clip_rect,
                        3.0,
                        egui::Stroke::new(2.0, egui::Color32::from_rgb(120, 160, 240)),
                    );
                }

                continue; // skip normal clip rendering
            }

            let is_selected = state.selected_clip == Some(clip.id);
            let is_multi_selected = state.selected_clips.contains(&clip.id);
            let is_hovered = state.hovered_clip == Some(clip.id);
            let is_title = clip.title_data().is_some();
            let is_offline = clip.media_id().and_then(|mid| state.media_pool.get(&mid))
                .map(|m| m.offline)
                .unwrap_or(false);
            let is_being_dragged = self.dragging_clip.as_ref()
                .is_some_and(|d| d.clip_id == clip.id);

            let base_color = if is_title {
                egui::Color32::from_rgb(60, 45, 90) // purple tint for title clips
            } else if is_offline {
                egui::Color32::from_rgb(58, 32, 32)
            } else {
                match track_kind {
                    TrackKind::Audio => egui::Color32::from_rgb(42, 90, 58),
                    _ => clip_color(clip_index),
                }
            };
            let bright_color = if is_title {
                egui::Color32::from_rgb(90, 65, 130)
            } else if is_offline {
                egui::Color32::from_rgb(80, 45, 45)
            } else {
                match track_kind {
                    TrackKind::Audio => egui::Color32::from_rgb(72, 120, 88),
                    _ => clip_color_bright(clip_index),
                }
            };

            let (fill, border_color, border_width) = if is_being_dragged {
                // Semi-transparent when being dragged so clips underneath are visible
                let f = egui::Color32::from_rgba_unmultiplied(base_color.r(), base_color.g(), base_color.b(), 140);
                (f, egui::Color32::from_rgb(232, 160, 32), 2.0)
            } else if is_selected {
                (bright_color, egui::Color32::from_rgb(120, 160, 240), 2.0)
            } else if is_multi_selected {
                (bright_color, egui::Color32::from_rgb(80, 130, 220), 1.5)
            } else if is_hovered {
                (base_color, egui::Color32::from_rgb(160, 160, 180), 1.0)
            } else {
                (base_color, bright_color, 1.0)
            };

            // Apply drag transparency to filmstrip images
            let filmstrip_tint = if is_being_dragged {
                egui::Color32::from_rgba_unmultiplied(255, 255, 255, 140)
            } else {
                egui::Color32::WHITE
            };

            // Draw thumbnail/filmstrip background for video/text/subtitle clips
            if track_kind != TrackKind::Audio {
                let mut drew_filmstrip = false;
                if let Some(filmstrip) = clip.media_id().and_then(|mid| state.filmstrip_textures.get(&mid)) {
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
                                filmstrip_tint,
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
                            let min_interval = clip.media_id().and_then(|mid| state.thumbnail_cache.filmstrip_min_interval(&mid));
                            let debounce_ok = self.last_refine_request
                                .map(|t| t.elapsed().as_secs_f64() > 0.5)
                                .unwrap_or(true);
                            if let Some(min_iv) = min_interval {
                                if secs_per_tile < min_iv * 0.5 && !state.filmstrip_generating && debounce_ok {
                                    let vis_start_frac = ((first_visible as f32 * tile_w) / clip_rect.width().max(1.0)) as f64;
                                    let vis_end_frac = ((last_visible as f32 * tile_w) / clip_rect.width().max(1.0)).min(1.0) as f64;
                                    let range_start = clip_in_secs + vis_start_frac * (clip_out_secs - clip_in_secs);
                                    let range_end = clip_in_secs + vis_end_frac * (clip_out_secs - clip_in_secs);
                                    let new_interval = secs_per_tile.max(0.5);
                                    if let Some(mid) = clip.media_id() {
                                        cmds.push(AppCommand::RefineFilmstrip {
                                            media_id: mid,
                                            start_secs: range_start,
                                            end_secs: range_end,
                                            interval_secs: new_interval,
                                        });
                                        self.last_refine_request = Some(std::time::Instant::now());
                                    }
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
                    if let Some(tex) = clip.media_id().and_then(|mid| state.thumbnail_textures.get(&mid)) {
                        painter.image(
                            tex.id(),
                            clip_rect,
                            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                            filmstrip_tint,
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
                // Non-video clip (audio, etc): fill background, then rubber band, then waveform on top
                painter.rect_filled(clip_rect, 3.0, fill);
            }

            // ── Waveform overlay (drawn AFTER rubber band so it's on top) ──
            if track_kind == TrackKind::Audio && !clip.kind.is_gap() {
                // Draw waveform overlay for clips with audio peak data
                // Try waveform_data first, then fall back to audio_cache.peaks
                if let Some(mid) = clip.media_id() {
                    let wf_data = state.media_pool.get(&mid).and_then(|m| m.waveform_data.as_ref().cloned());
                    let ac_peaks = state.media_pool.get(&mid)
                        .and_then(|m| m.audio_cache.as_ref())
                        .map(|c| c.peaks.clone());
                    let peaks_ref = wf_data.or(ac_peaks);
                    if let Some(ref peaks) = peaks_ref {
                        if !peaks.is_empty() {
                            // Map clip source range to peak indices
                            // Each peak bin covers PEAK_BIN_SIZE/48000 seconds
                            const PEAK_BIN_SECS: f64 = 256.0 / 48000.0;
                            let src_in = clip.source_in.to_secs_f64();
                            let src_out = clip.source_out.to_secs_f64();
                            let first_bin = (src_in / PEAK_BIN_SECS) as usize;
                            let last_bin = ((src_out / PEAK_BIN_SECS) as usize).min(peaks.len());

                            if first_bin < last_bin {
                                let clip_peaks = &peaks[first_bin..last_bin];
                                // Bright waveform: lighter than background for clear visibility
                                let waveform_color = egui::Color32::from_rgba_premultiplied(
                                    bright_color.r().saturating_add(100).min(255),
                                    bright_color.g().saturating_add(100).min(255),
                                    bright_color.b().saturating_add(100).min(255),
                                    220,
                                );

                                let clip_painter = ui.painter().with_clip_rect(clip_rect);
                                let center_y = clip_rect.center().y;
                                let half_h = clip_rect.height() * 0.48; // nearly full height
                                let w = clip_rect.width();

                                // Draw min/max envelope with sqrt scaling.
                                // sqrt(amplitude) makes quiet parts much more visible while
                                // preserving the shape. This is how most NLEs display waveforms.
                                // e.g. peak=0.003 -> sqrt(0.003)=0.055 -> 5.5% of height (visible!)
                                //      peak=1.0   -> sqrt(1.0)=1.0     -> 100% of height
                                let num_cols = (w as usize).max(1);
                                for col in 0..num_cols {
                                    let x = clip_rect.left() + col as f32;
                                    let peak_idx = col * clip_peaks.len() / num_cols;
                                    let (pmin, pmax) = clip_peaks[peak_idx.min(clip_peaks.len() - 1)];

                                    let y_top = center_y - pmax.abs().sqrt().min(1.0) * half_h;
                                    let y_bot = center_y + pmin.abs().sqrt().min(1.0) * half_h;

                                    if (y_bot - y_top) > 0.5 {
                                        clip_painter.line_segment(
                                            [egui::pos2(x, y_top), egui::pos2(x, y_bot)],
                                            egui::Stroke::new(1.0, waveform_color),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // ── Audio parameter rubber band overlay ──────────────────────
            // Drawn on audio track clips. Displays the parameter selected by rubber_band_param.
            // For linked audio clips, reads keyframes from the linked video clip.
            if track_kind == TrackKind::Audio && clip_rect.width() > 8.0 && !clip.kind.is_gap() {
                let rb_param = state.rubber_band_param;

                // Resolve the clip that holds the keyframes (may be linked video clip)
                // Clone keyframes to avoid borrow conflict with state mutation for drag interaction
                let resolve_clip = |c: &klipp_timeline::Clip| -> (f32, Vec<klipp_core::Keyframe>, klipp_core::ClipId, f32, f32, f32, f32, klipp_timeline::FadeCurve, klipp_timeline::FadeCurve, klipp_timeline::ChannelConfig) {
                    let (static_val, kfs) = match rb_param {
                        klipp_core::AudioParam::Volume => (c.volume, c.volume_keyframes.clone()),
                        klipp_core::AudioParam::Pan => (c.pan, c.pan_keyframes.clone()),
                        klipp_core::AudioParam::Gain => (c.gain, c.gain_keyframes.clone()),
                    };
                    (static_val, kfs, c.id, c.gain, c.pan, c.fade_in_secs, c.fade_out_secs, c.fade_in_curve, c.fade_out_curve, c.channel_config)
                };

                let (kf_static_val, kf_keyframes, kf_clip_id, kf_gain, kf_pan, kf_fade_in, kf_fade_out, kf_fade_in_curve, kf_fade_out_curve, kf_channel_config) = if let Some(linked_id) = clip.linked_clip_id {
                    let mut found = resolve_clip(clip);
                    for t in &state.sequence().tracks {
                        if let Some(vc) = t.find_clip(linked_id) {
                            found = resolve_clip(vc);
                            break;
                        }
                    }
                    found
                } else {
                    resolve_clip(clip)
                };
                // Also capture static volume for the flat-line drag command (always volume)
                let kf_volume = if let Some(linked_id) = clip.linked_clip_id {
                    let mut v = clip.volume;
                    for t in &state.sequence().tracks {
                        if let Some(vc) = t.find_clip(linked_id) { v = vc.volume; break; }
                    }
                    v
                } else {
                    clip.volume
                };
                let kf_keyframes = &kf_keyframes[..];

                // Param range configuration
                let (param_min, param_max, _param_default) = match rb_param {
                    klipp_core::AudioParam::Volume => (0.0_f32, 2.0_f32, 1.0_f32),
                    klipp_core::AudioParam::Pan => (-1.0_f32, 1.0_f32, 0.0_f32),
                    klipp_core::AudioParam::Gain => (0.0_f32, 4.0_f32, 1.0_f32),
                };
                let param_range = param_max - param_min;

                let clip_dur_secs = clip.duration().to_secs_f64();
                let clip_painter = ui.painter().with_clip_rect(clip_rect);

                // Map param value to Y position within clip rect
                // min = bottom, max = top
                let value_to_y = |val: f32| -> f32 {
                    let normalized = ((val - param_min) / param_range).clamp(0.0, 1.0);
                    clip_rect.bottom() - normalized * clip_rect.height()
                };

                // Map time (relative to clip start) to X position
                let time_to_x = |t: f64| -> f32 {
                    let frac = if clip_dur_secs > 0.0 { (t / clip_dur_secs).clamp(0.0, 1.0) } else { 0.0 };
                    clip_rect.left() + frac as f32 * clip_rect.width()
                };

                // Color per param: amber=Volume, cyan=Pan, green=Gain
                let (line_color, line_color_dim, diamond_fill_sel, diamond_fill_norm, _diamond_stroke_color) = match rb_param {
                    klipp_core::AudioParam::Volume => (
                        egui::Color32::from_rgba_unmultiplied(255, 195, 50, 220),
                        egui::Color32::from_rgba_unmultiplied(240, 180, 50, 140),
                        egui::Color32::from_rgba_unmultiplied(255, 210, 60, 250),
                        egui::Color32::from_rgba_unmultiplied(255, 195, 50, 200),
                        egui::Color32::from_rgba_unmultiplied(200, 150, 30, 240),
                    ),
                    klipp_core::AudioParam::Pan => (
                        egui::Color32::from_rgba_unmultiplied(50, 195, 255, 220),
                        egui::Color32::from_rgba_unmultiplied(50, 180, 240, 140),
                        egui::Color32::from_rgba_unmultiplied(60, 210, 255, 250),
                        egui::Color32::from_rgba_unmultiplied(50, 195, 255, 200),
                        egui::Color32::from_rgba_unmultiplied(30, 150, 200, 240),
                    ),
                    klipp_core::AudioParam::Gain => (
                        egui::Color32::from_rgba_unmultiplied(50, 220, 100, 220),
                        egui::Color32::from_rgba_unmultiplied(50, 200, 90, 140),
                        egui::Color32::from_rgba_unmultiplied(60, 240, 110, 250),
                        egui::Color32::from_rgba_unmultiplied(50, 220, 100, 200),
                        egui::Color32::from_rgba_unmultiplied(30, 170, 60, 240),
                    ),
                };
                let shadow_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 150);
                let diamond_fill = if is_selected || is_multi_selected { diamond_fill_sel } else { diamond_fill_norm };
                let _diamond_shadow_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 160);
                let active_line_color = if is_selected || is_multi_selected { line_color } else { line_color_dim };
                let rb_line_width = if is_selected || is_multi_selected { 2.5 } else { 2.0 };
                let rb_shadow_width = rb_line_width + 1.0;

                // Draw param pill badge at top-left of clip (below clip name area)
                let param_label = match rb_param {
                    klipp_core::AudioParam::Volume => "Vol",
                    klipp_core::AudioParam::Pan => "Pan",
                    klipp_core::AudioParam::Gain => "Gain",
                };
                // Measure text width to size the pill dynamically
                let pill_font = egui::FontId::proportional(9.0);
                let pill_text_galley = ui.painter().layout_no_wrap(param_label.to_string(), pill_font.clone(), egui::Color32::WHITE);
                let pill_w = pill_text_galley.size().x + 8.0; // 4px padding each side
                let pill_h = 14.0;
                let pill_pos = egui::pos2(clip_rect.left() + 4.0, clip_rect.top() + 14.0);
                let pill_rect = egui::Rect::from_min_size(pill_pos, egui::vec2(pill_w, pill_h));
                // Only draw and handle clicks if clip is wide enough
                if clip_rect.width() > 30.0 {
                    // Background: rubber band color at ~40% opacity
                    let pill_bg = match rb_param {
                        klipp_core::AudioParam::Volume => egui::Color32::from_rgba_unmultiplied(255, 195, 50, 100),
                        klipp_core::AudioParam::Pan => egui::Color32::from_rgba_unmultiplied(50, 195, 255, 100),
                        klipp_core::AudioParam::Gain => egui::Color32::from_rgba_unmultiplied(50, 220, 100, 100),
                    };
                    clip_painter.rect_filled(pill_rect, 4.0, pill_bg);
                    clip_painter.text(
                        pill_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        param_label,
                        pill_font,
                        egui::Color32::WHITE,
                    );
                    // Click pill to cycle param
                    if interactive {
                        if let Some(pos) = pointer_pos {
                            if pill_rect.contains(pos) && ui.input(|i| i.pointer.primary_clicked()) {
                                state.rubber_band_param = match rb_param {
                                    klipp_core::AudioParam::Volume => klipp_core::AudioParam::Pan,
                                    klipp_core::AudioParam::Pan => klipp_core::AudioParam::Gain,
                                    klipp_core::AudioParam::Gain => klipp_core::AudioParam::Volume,
                                };
                            }
                        }
                    }
                }

                if kf_keyframes.is_empty() {
                    // No keyframes: draw a single horizontal line at the static value
                    let y = value_to_y(kf_static_val);
                    // Shadow line behind for contrast
                    clip_painter.line_segment(
                        [egui::pos2(clip_rect.left(), y), egui::pos2(clip_rect.right(), y)],
                        egui::Stroke::new(rb_shadow_width, shadow_color),
                    );
                    clip_painter.line_segment(
                        [egui::pos2(clip_rect.left(), y), egui::pos2(clip_rect.right(), y)],
                        egui::Stroke::new(rb_line_width, active_line_color),
                    );
                } else {
                    // Draw line segments between evaluated points
                    // Start with value at clip start, then each keyframe, then value at clip end
                    let src_in_secs = clip.source_in.to_secs_f64();
                    let src_out_secs = clip.source_out.to_secs_f64();

                    // Build list of points along the rubber band
                    let mut points: Vec<egui::Pos2> = Vec::new();

                    // Start point: evaluate at source_in
                    let start_val = klipp_core::keyframe::evaluate(kf_keyframes, 0.0, kf_static_val);
                    points.push(egui::pos2(clip_rect.left(), value_to_y(start_val)));

                    // Each keyframe that falls within clip range
                    for kf in kf_keyframes.iter() {
                        // kf.time_secs is relative to source_in
                        if kf.time_secs >= 0.0 && kf.time_secs <= (src_out_secs - src_in_secs) {
                            let x = time_to_x(kf.time_secs);
                            let y = value_to_y(kf.value);
                            points.push(egui::pos2(x, y));
                        }
                    }

                    // End point: evaluate at clip end
                    let end_t = src_out_secs - src_in_secs;
                    let end_val = klipp_core::keyframe::evaluate(kf_keyframes, end_t, kf_static_val);
                    points.push(egui::pos2(clip_rect.right(), value_to_y(end_val)));

                    // Draw shadow line segments first for contrast
                    for i in 0..points.len().saturating_sub(1) {
                        clip_painter.line_segment(
                            [points[i], points[i + 1]],
                            egui::Stroke::new(rb_shadow_width, shadow_color),
                        );
                    }
                    // Draw colored line segments on top
                    for i in 0..points.len().saturating_sub(1) {
                        clip_painter.line_segment(
                            [points[i], points[i + 1]],
                            egui::Stroke::new(rb_line_width, active_line_color),
                        );
                    }

                    // Draw diamond markers at each keyframe
                    // Check if pointer is near any keyframe for hover effect
                    let hovered_kf_idx: Option<usize> = pointer_pos.and_then(|pos| {
                        if !clip_rect.contains(pos) { return None; }
                        let mut best: Option<(usize, f32)> = None;
                        for (ki, kf) in kf_keyframes.iter().enumerate() {
                            if kf.time_secs >= 0.0 && kf.time_secs <= (src_out_secs - src_in_secs) {
                                let kx = time_to_x(kf.time_secs);
                                let ky = value_to_y(kf.value);
                                let dist = ((pos.x - kx).powi(2) + (pos.y - ky).powi(2)).sqrt();
                                if dist < 10.0 {
                                    if best.map_or(true, |(_, bd)| dist < bd) {
                                        best = Some((ki, dist));
                                    }
                                }
                            }
                        }
                        best.map(|(i, _)| i)
                    });

                    for (ki, kf) in kf_keyframes.iter().enumerate() {
                        if kf.time_secs >= 0.0 && kf.time_secs <= (src_out_secs - src_in_secs) {
                            let cx = time_to_x(kf.time_secs);
                            let cy = value_to_y(kf.value);
                            let is_hovered = hovered_kf_idx == Some(ki);
                            let d = if is_hovered { 9.0_f32 } else { 8.0_f32 };
                            // Dark outline diamond behind for contrast
                            let outline_d = d + 1.5;
                            let outline_diamond = vec![
                                egui::pos2(cx, cy - outline_d),
                                egui::pos2(cx + outline_d, cy),
                                egui::pos2(cx, cy + outline_d),
                                egui::pos2(cx - outline_d, cy),
                            ];
                            clip_painter.add(egui::Shape::convex_polygon(
                                outline_diamond,
                                egui::Color32::from_rgba_unmultiplied(0, 0, 0, 200),
                                egui::Stroke::NONE,
                            ));
                            // Brighten fill on hover
                            let fill = if is_hovered {
                                egui::Color32::from_rgba_unmultiplied(
                                    diamond_fill.r().saturating_add(30),
                                    diamond_fill.g().saturating_add(30),
                                    diamond_fill.b().saturating_add(30),
                                    255,
                                )
                            } else {
                                diamond_fill
                            };
                            let diamond = vec![
                                egui::pos2(cx, cy - d),
                                egui::pos2(cx + d, cy),
                                egui::pos2(cx, cy + d),
                                egui::pos2(cx - d, cy),
                            ];
                            clip_painter.add(egui::Shape::convex_polygon(
                                diamond,
                                fill,
                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(20, 20, 20, 220)),
                            ));
                        }
                    }
                }

                // ── Rubber band interaction ──────────────────────────────────
                if interactive {
                    if let Some(pos) = pointer_pos {
                        if clip_rect.contains(pos) && content_rect.contains(pos) {
                            let alt_held = ui.input(|i| i.modifiers.alt);
                            let src_in_secs = clip.source_in.to_secs_f64();
                            let src_out_secs = clip.source_out.to_secs_f64();

                            // Helper: X position to time (relative to source_in)
                            let x_to_time = |x: f32| -> f64 {
                                let frac = ((x - clip_rect.left()) / clip_rect.width()).clamp(0.0, 1.0) as f64;
                                frac * clip_dur_secs
                            };
                            // Helper: Y position to param value
                            let y_to_value = |y: f32| -> f32 {
                                let normalized = ((clip_rect.bottom() - y) / clip_rect.height()).clamp(0.0, 1.0);
                                param_min + normalized * param_range
                            };

                            // Check if hovering near a keyframe diamond (for drag start)
                            let mut nearest_kf_idx: Option<usize> = None;
                            let mut nearest_kf_dist = f32::MAX;
                            for (ki, kf) in kf_keyframes.iter().enumerate() {
                                if kf.time_secs >= 0.0 && kf.time_secs <= (src_out_secs - src_in_secs) {
                                    let kx = time_to_x(kf.time_secs);
                                    let ky = value_to_y(kf.value);
                                    let dist = ((pos.x - kx).powi(2) + (pos.y - ky).powi(2)).sqrt();
                                    if dist < 10.0 && dist < nearest_kf_dist {
                                        nearest_kf_dist = dist;
                                        nearest_kf_idx = Some(ki);
                                    }
                                }
                            }

                            // Check if hovering near the rubber band line
                            let rubber_band_y = if kf_keyframes.is_empty() {
                                value_to_y(kf_static_val)
                            } else {
                                let t = x_to_time(pos.x);
                                value_to_y(klipp_core::keyframe::evaluate(kf_keyframes, t, kf_static_val))
                            };
                            let near_line = (pos.y - rubber_band_y).abs() < 6.0;

                            // ── Cursor feedback ──
                            let is_dragging_this_rb = state.dragging_rubber_band == Some(kf_clip_id);
                            let is_dragging_this_kf = state.dragging_audio_keyframe.as_ref().map_or(false, |d| d.clip_id == kf_clip_id);
                            if is_dragging_this_rb || is_dragging_this_kf {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                            } else if nearest_kf_idx.is_some() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
                            } else if near_line {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                            }

                            // ── Double-click diamond to delete keyframe ──
                            if let Some(ki) = nearest_kf_idx {
                                if ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary)) {
                                    let kf = &kf_keyframes[ki];
                                    cmds.push(AppCommand::RemoveAudioKeyframe {
                                        clip_id: kf_clip_id,
                                        param: rb_param,
                                        time_secs: kf.time_secs,
                                    });
                                }
                            }

                            // Start keyframe drag on primary click near a diamond
                            if nearest_kf_idx.is_some() && pointer_down && state.dragging_audio_keyframe.is_none() && !alt_held {
                                if let Some(ki) = nearest_kf_idx {
                                    state.dragging_audio_keyframe = Some(crate::state::DraggingKeyframe {
                                        clip_id: kf_clip_id,
                                        param: rb_param,
                                        keyframe_index: ki,
                                    });
                                }
                            }

                            // ── Flat-line drag: drag rubber band to change volume (no keyframes) ──
                            if kf_keyframes.is_empty() && near_line && pointer_down
                                && state.dragging_audio_keyframe.is_none()
                                && state.dragging_rubber_band.is_none()
                                && !alt_held
                            {
                                state.dragging_rubber_band = Some(kf_clip_id);
                            }

                            // Process active flat-line rubber band drag
                            if state.dragging_rubber_band == Some(kf_clip_id) {
                                if pointer_down {
                                    let new_val = y_to_value(pos.y);
                                    let (drag_gain, drag_volume, drag_pan) = match rb_param {
                                        klipp_core::AudioParam::Volume => (kf_gain, new_val, kf_pan),
                                        klipp_core::AudioParam::Pan => (kf_gain, kf_volume, new_val),
                                        klipp_core::AudioParam::Gain => (new_val, kf_volume, kf_pan),
                                    };
                                    cmds.push(AppCommand::UpdateClipAudio {
                                        clip_id: kf_clip_id,
                                        gain: drag_gain,
                                        volume: drag_volume,
                                        pan: drag_pan,
                                        fade_in_secs: kf_fade_in,
                                        fade_out_secs: kf_fade_out,
                                        fade_in_curve: kf_fade_in_curve,
                                        fade_out_curve: kf_fade_out_curve,
                                        channel_config: kf_channel_config,
                                    });
                                    // Show tooltip with param-appropriate format
                                    let tooltip_text = match rb_param {
                                        klipp_core::AudioParam::Volume => format!("{:.0}%", new_val * 100.0),
                                        klipp_core::AudioParam::Pan => {
                                            if new_val.abs() < 0.01 { "C".to_string() }
                                            else if new_val < 0.0 { format!("L{:.0}", -new_val * 100.0) }
                                            else { format!("R{:.0}", new_val * 100.0) }
                                        }
                                        klipp_core::AudioParam::Gain => format!("{:.1}x", new_val),
                                    };
                                    clip_painter.text(
                                        egui::pos2(pos.x, pos.y - 14.0),
                                        egui::Align2::CENTER_BOTTOM,
                                        tooltip_text,
                                        egui::FontId::proportional(10.0),
                                        active_line_color,
                                    );
                                }
                            }

                            // Alt+click on line to add keyframe
                            if alt_held && near_line && ui.input(|i| i.pointer.primary_clicked()) {
                                let t = x_to_time(pos.x);
                                let v = y_to_value(pos.y);
                                cmds.push(AppCommand::AddAudioKeyframe {
                                    clip_id: kf_clip_id,
                                    param: rb_param,
                                    time_secs: t,
                                    value: v,
                                });
                            }

                            // Show value tooltip when hovering near a keyframe
                            if let Some(ki) = nearest_kf_idx {
                                let kf = &kf_keyframes[ki];
                                let tooltip_text = match rb_param {
                                    klipp_core::AudioParam::Volume => format!("{:.1}%", kf.value * 100.0),
                                    klipp_core::AudioParam::Pan => {
                                        if kf.value.abs() < 0.01 { "C".to_string() }
                                        else if kf.value < 0.0 { format!("L{:.0}", -kf.value * 100.0) }
                                        else { format!("R{:.0}", kf.value * 100.0) }
                                    }
                                    klipp_core::AudioParam::Gain => format!("{:.2}x", kf.value),
                                };
                                let kx = time_to_x(kf.time_secs);
                                let ky = value_to_y(kf.value);
                                clip_painter.text(
                                    egui::pos2(kx, ky - 14.0),
                                    egui::Align2::CENTER_BOTTOM,
                                    tooltip_text,
                                    egui::FontId::proportional(9.0),
                                    active_line_color,
                                );
                            }
                        }
                    }
                }
            }

            // ── Audio fade in/out visual indicators ────────────────────────
            // Drawn after rubber band so fade overlays are clearly visible.
            // Uses the same resolved fade params from the rubber band section.
            if track_kind == TrackKind::Audio && clip_rect.width() > 8.0 && !clip.kind.is_gap() {
                // Resolve fade params (same pattern as rubber band)
                let resolve_fade = |c: &klipp_timeline::Clip| -> (f32, f32, klipp_timeline::FadeCurve, klipp_timeline::FadeCurve) {
                    (c.fade_in_secs, c.fade_out_secs, c.fade_in_curve, c.fade_out_curve)
                };
                let (fi_secs, fo_secs, fi_curve, fo_curve) = if let Some(linked_id) = clip.linked_clip_id {
                    let mut found = resolve_fade(clip);
                    for t in &state.sequence().tracks {
                        if let Some(vc) = t.find_clip(linked_id) {
                            found = resolve_fade(vc);
                            break;
                        }
                    }
                    found
                } else {
                    resolve_fade(clip)
                };

                let clip_dur_secs = clip.duration().to_secs_f64() as f32;
                let fade_clip_painter = ui.painter().with_clip_rect(clip_rect);

                // Curve evaluation: maps t in 0..1 through the selected curve shape
                let eval_curve = |t: f32, curve: klipp_timeline::FadeCurve| -> f32 {
                    match curve {
                        klipp_timeline::FadeCurve::Linear => t,
                        klipp_timeline::FadeCurve::Exponential => t * t,
                        klipp_timeline::FadeCurve::Logarithmic => 1.0 - (1.0 - t) * (1.0 - t),
                        klipp_timeline::FadeCurve::SCurve => 3.0 * t * t - 2.0 * t * t * t,
                    }
                };

                let fade_overlay_color = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 60);
                let fade_line_color = egui::Color32::from_rgba_unmultiplied(255, 195, 50, 200);
                let fade_line_shadow = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 120);
                let num_segments: usize = 24;

                // ── Fade In ──
                if fi_secs > 0.001 && clip_dur_secs > 0.001 {
                    let fade_in_px = (fi_secs / clip_dur_secs) * clip_rect.width();
                    let fade_in_px = fade_in_px.min(clip_rect.width());

                    // Dark overlay rectangle
                    let fade_rect = egui::Rect::from_min_max(
                        clip_rect.left_top(),
                        egui::pos2(clip_rect.left() + fade_in_px, clip_rect.bottom()),
                    );
                    fade_clip_painter.rect_filled(fade_rect, 0.0, fade_overlay_color);

                    // Build curve polyline: from (left, bottom) up to (left + fade_in_px, top-region)
                    // The curve represents the gain going from 0 (bottom) to 1 (rubber band level)
                    let mut points: Vec<egui::Pos2> = Vec::with_capacity(num_segments + 1);
                    for i in 0..=num_segments {
                        let t = i as f32 / num_segments as f32;
                        let gain = eval_curve(t, fi_curve);
                        let x = clip_rect.left() + t * fade_in_px;
                        // Map gain 0..1 to bottom..top of clip rect
                        let y = clip_rect.bottom() - gain * clip_rect.height();
                        points.push(egui::pos2(x, y));
                    }

                    // Shadow
                    for i in 0..points.len().saturating_sub(1) {
                        fade_clip_painter.line_segment(
                            [points[i], points[i + 1]],
                            egui::Stroke::new(2.5, fade_line_shadow),
                        );
                    }
                    // Colored curve line
                    for i in 0..points.len().saturating_sub(1) {
                        fade_clip_painter.line_segment(
                            [points[i], points[i + 1]],
                            egui::Stroke::new(1.5, fade_line_color),
                        );
                    }

                    // Vertical boundary line at fade end
                    fade_clip_painter.line_segment(
                        [
                            egui::pos2(clip_rect.left() + fade_in_px, clip_rect.top()),
                            egui::pos2(clip_rect.left() + fade_in_px, clip_rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 195, 50, 80)),
                    );
                }

                // ── Fade Out ──
                if fo_secs > 0.001 && clip_dur_secs > 0.001 {
                    let fade_out_px = (fo_secs / clip_dur_secs) * clip_rect.width();
                    let fade_out_px = fade_out_px.min(clip_rect.width());
                    let fade_start_x = clip_rect.right() - fade_out_px;

                    // Dark overlay rectangle
                    let fade_rect = egui::Rect::from_min_max(
                        egui::pos2(fade_start_x, clip_rect.top()),
                        clip_rect.right_bottom(),
                    );
                    fade_clip_painter.rect_filled(fade_rect, 0.0, fade_overlay_color);

                    // Build curve polyline: from (fade_start_x, top-region) down to (right, bottom)
                    // t goes 0..1 across the fade-out region; gain goes 1→0
                    let mut points: Vec<egui::Pos2> = Vec::with_capacity(num_segments + 1);
                    for i in 0..=num_segments {
                        let t = i as f32 / num_segments as f32;
                        // remaining = 1 - t, so gain fades from 1 to 0
                        let remaining = 1.0 - t;
                        let gain = eval_curve(remaining, fo_curve);
                        let x = fade_start_x + t * fade_out_px;
                        let y = clip_rect.bottom() - gain * clip_rect.height();
                        points.push(egui::pos2(x, y));
                    }

                    // Shadow
                    for i in 0..points.len().saturating_sub(1) {
                        fade_clip_painter.line_segment(
                            [points[i], points[i + 1]],
                            egui::Stroke::new(2.5, fade_line_shadow),
                        );
                    }
                    // Colored curve line
                    for i in 0..points.len().saturating_sub(1) {
                        fade_clip_painter.line_segment(
                            [points[i], points[i + 1]],
                            egui::Stroke::new(1.5, fade_line_color),
                        );
                    }

                    // Vertical boundary line at fade start
                    fade_clip_painter.line_segment(
                        [
                            egui::pos2(fade_start_x, clip_rect.top()),
                            egui::pos2(fade_start_x, clip_rect.bottom()),
                        ],
                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(255, 195, 50, 80)),
                    );
                }
            }

            // Border
            painter.rect_stroke(
                clip_rect,
                3.0,
                egui::Stroke::new(border_width, border_color),
            );

            // Linked-clip indicator — subtle lock icon at top-right corner
            if clip.linked_clip_id.is_some() && clip_rect.width() > 20.0 {
                let icon_rect = egui::Rect::from_center_size(
                    egui::pos2(clip_rect.right() - 8.0, clip_rect.top() + 8.0),
                    egui::vec2(9.0, 9.0),
                );
                crate::icons::Icon::Lock.image(9.0)
                    .tint(egui::Color32::from_rgba_premultiplied(255, 255, 255, 120))
                    .paint_at(ui, icon_rect);
            }

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
                if let Some(title_data) = clip.title_data() {
                    // Title clip: show "T" badge and text preview
                    let font_size = 11.0;
                    let max_text_width = clip_rect.width() - 28.0;
                    // "T" badge
                    let badge_rect = egui::Rect::from_min_size(
                        egui::pos2(clip_rect.left() + 3.0, clip_rect.center().y - 8.0),
                        egui::vec2(16.0, 16.0),
                    );
                    painter.rect_filled(badge_rect, 3.0, egui::Color32::from_rgba_premultiplied(200, 180, 255, 180));
                    painter.text(
                        badge_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "T",
                        egui::FontId::proportional(10.0),
                        egui::Color32::BLACK,
                    );
                    // Text preview
                    if max_text_width > 10.0 {
                        let text_pos = egui::pos2(clip_rect.left() + 22.0, clip_rect.center().y);
                        let truncated = truncate_text(&title_data.text, max_text_width, font_size);
                        let font = egui::FontId::proportional(font_size);
                        let shadow_color = egui::Color32::from_rgba_premultiplied(0, 0, 0, 200);
                        for &(dx, dy) in &[(-1.0f32, 0.0f32), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                            painter.text(
                                egui::pos2(text_pos.x + dx, text_pos.y + dy),
                                egui::Align2::LEFT_CENTER,
                                truncated.clone(),
                                font.clone(),
                                shadow_color,
                            );
                        }
                        painter.text(
                            text_pos,
                            egui::Align2::LEFT_CENTER,
                            truncated,
                            font,
                            egui::Color32::from_rgb(220, 200, 255),
                        );
                    }
                } else if let Some(media) = clip.media_id().and_then(|mid| state.media_pool.get(&mid)) {
                    let font_size = if matches!(track_kind, TrackKind::Audio) { 10.0 } else { 11.0 };
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
                    let truncated = truncate_text(&label, max_text_width, font_size);
                    let font = egui::FontId::proportional(font_size);
                    // Draw dark outline/shadow for readability on any background
                    let shadow_offsets: &[(f32, f32)] = &[
                        (-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0),
                    ];
                    let shadow_color = egui::Color32::from_rgba_premultiplied(0, 0, 0, 200);
                    for &(dx, dy) in shadow_offsets {
                        painter.text(
                            egui::pos2(text_pos.x + dx, text_pos.y + dy),
                            egui::Align2::LEFT_CENTER,
                            truncated.clone(),
                            font.clone(),
                            shadow_color,
                        );
                    }
                    painter.text(
                        text_pos,
                        egui::Align2::LEFT_CENTER,
                        truncated,
                        font,
                        label_color,
                    );

                    // Proxy badge [P] — orange pill when using proxy for this clip
                    if media.proxy_ready && state.use_proxies && clip_rect.width() > 30.0 {
                        let badge_font = egui::FontId::proportional(8.0);
                        let badge_pos = egui::pos2(clip_rect.right() - 12.0, clip_rect.top() + 8.0);
                        let badge_rect = egui::Rect::from_center_size(
                            badge_pos,
                            egui::vec2(12.0, 11.0),
                        );
                        painter.rect_filled(badge_rect, 3.0, egui::Color32::from_rgb(220, 170, 40));
                        painter.text(
                            badge_pos,
                            egui::Align2::CENTER_CENTER,
                            "P",
                            badge_font,
                            egui::Color32::from_rgb(30, 30, 35),
                        );
                    }
                }
            }

            // Group badge [G] — bottom-left
            if clip.group_id.is_some() && clip_rect.width() > 30.0 {
                let g_pos = egui::pos2(clip_rect.left() + 10.0, clip_rect.bottom() - 8.0);
                let g_rect = egui::Rect::from_center_size(g_pos, egui::vec2(14.0, 11.0));
                painter.rect_filled(g_rect, 3.0, egui::Color32::from_rgb(80, 140, 220));
                painter.text(g_pos, egui::Align2::CENTER_CENTER, "G",
                    egui::FontId::proportional(8.0), egui::Color32::WHITE);
            }

            // Locked track overlay: semi-transparent dark with diagonal lines
            if state.sequence().tracks[track_idx].locked {
                painter.rect_filled(
                    clip_rect,
                    3.0,
                    egui::Color32::from_rgba_premultiplied(0, 0, 0, 80),
                );
                // Diagonal hatch lines
                let step = 8.0;
                let mut lx = clip_rect.left() - clip_rect.height();
                while lx < clip_rect.right() {
                    let x0 = lx.max(clip_rect.left());
                    let y0_raw = clip_rect.top() + (x0 - lx);
                    let y0 = y0_raw.max(clip_rect.top());
                    let x1 = (lx + clip_rect.height()).min(clip_rect.right());
                    let y1_raw = clip_rect.bottom() - (lx + clip_rect.height() - x1);
                    let y1 = y1_raw.min(clip_rect.bottom());
                    painter.line_segment(
                        [egui::pos2(x0, y0), egui::pos2(x1, y1)],
                        egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 30)),
                    );
                    lx += step;
                }
            }

            // Range selection overlay — amber tint on clips within in/out range
            if let (Some(in_pt), Some(out_pt)) = (state.sequence().in_point, state.sequence().out_point) {
                let range_in_secs = in_pt.to_secs_f64();
                let range_out_secs = out_pt.to_secs_f64();
                let clip_in_secs = clip.timeline_in.to_secs_f64();
                let clip_out_secs = clip.timeline_out.to_secs_f64();
                if clip_out_secs > range_in_secs && clip_in_secs < range_out_secs {
                    let range_in_x = content_rect.left() + range_in_secs as f32 * zoom - scroll;
                    let range_out_x = content_rect.left() + range_out_secs as f32 * zoom - scroll;
                    let overlay_left = clip_rect.left().max(range_in_x);
                    let overlay_right = clip_rect.right().min(range_out_x);
                    if overlay_right > overlay_left {
                        let overlay_rect = egui::Rect::from_min_max(
                            egui::pos2(overlay_left, clip_rect.top()),
                            egui::pos2(overlay_right, clip_rect.bottom()),
                        );
                        crate::timeline::range_visuals::draw_clip_overlay(painter, overlay_rect, &crate::theme::KlippTheme::default());
                    }
                }
            }

            if !interactive {
                continue;
            }

            // Hover detection — only if pointer is inside the visible content area
            // Blocked when a modal dialog is open to prevent background interaction
            if let Some(pos) = pointer_pos {
                if clip_rect.contains(pos) && content_rect.contains(pos) && !state.has_modal_open() {
                    state.hovered_clip = Some(clip.id);

                    // Timeline skimming: show frame at hover position in viewer
                    if state.skimming_enabled && !state.has_modal_open() {
                        if let Some(media_id) = clip.media_id() {
                            state.browser_skim_active_this_frame = true;

                            // Map hover X to source time
                            let frac = ((pos.x - clip_rect.min.x) / clip_rect.width()).clamp(0.0, 1.0);
                            let clip_dur = clip.duration().to_secs_f64();
                            let source_in_secs = clip.source_in.to_secs_f64();
                            let skim_time = source_in_secs + clip_dur * frac as f64;

                            // Check if skim file exists for this media
                            let has_skim_file = state.media_pool.get(&media_id)
                                .and_then(|m| m.skim_file.as_ref())
                                .is_some_and(|p| p.exists());

                            if has_skim_file && state.browser_skim_pending.is_none() {
                                cmds.push(AppCommand::BrowserSkimFrame {
                                    media_id,
                                    time_secs: skim_time,
                                });
                            }

                            // Fallback: poster thumbnail
                            if state.browser_skim_texture.is_none() {
                                if let Some(tex) = state.thumbnail_textures.get(&media_id) {
                                    state.browser_skim_texture = Some(tex.clone());
                                    state.browser_skim_media_id = Some(media_id);
                                }
                            } else {
                                state.browser_skim_media_id = Some(media_id);
                            }

                            state.viewer_mode = klipp_core::ViewerMode::Source;
                            state.source_media_id = Some(media_id);

                            // Draw skim position indicator on clip
                            painter.line_segment(
                                [
                                    egui::Pos2::new(pos.x, clip_rect.min.y),
                                    egui::Pos2::new(pos.x, clip_rect.max.y),
                                ],
                                egui::Stroke::new(1.5, egui::Color32::WHITE),
                            );

                            // Audio skimming on timeline hover
                            if state.audio_skimming_enabled {
                                // Speed calculation
                                let now = std::time::Instant::now();
                                let dt = now.duration_since(state.skim_last_update).as_secs_f32();
                                let dx = (pos.x - state.skim_last_x).abs();
                                let raw_speed = if dt > 0.001 { dx / dt } else { 0.0 };
                                state.skim_speed_px_per_sec = state.skim_speed_px_per_sec * 0.7 + raw_speed * 0.3;
                                state.skim_last_x = pos.x;
                                state.skim_last_update = now;

                                if let Some(source_path) = state.media_pool.get(&media_id).map(|m| m.path.clone()) {
                                    let is_new = state.skim_audio_session.is_none();
                                    let elapsed = state.skim_last_audio_dispatch.elapsed().as_millis() as u64;

                                    if is_new {
                                        state.skim_last_audio_dispatch = std::time::Instant::now();
                                        cmds.push(AppCommand::BrowserSkimAudioStart {
                                            media_id,
                                            source_path,
                                            time_secs: skim_time,
                                        });
                                    } else if elapsed >= 16 {
                                        state.skim_last_audio_dispatch = std::time::Instant::now();
                                        cmds.push(AppCommand::BrowserSkimAudioUpdate {
                                            media_id,
                                            time_secs: skim_time,
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Blade cursor/line with snap support
            if state.active_tool == Tool::Blade {
                if let Some(pos) = pointer_pos {
                    if clip_rect.contains(pos) && content_rect.contains(pos) {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                        let blade_snap = state.snap_enabled && !ui.input(|i| i.modifiers.shift);
                        let raw_x = pos.x - content_rect.left() + scroll;
                        let raw_secs = (raw_x as f64 / zoom as f64).max(0.0);
                        let mut blade_x = pos.x;

                        if blade_snap {
                            let snap_thresh_secs = 20.0 / zoom as f64;
                            // Snap targets: playhead, clip edges, in/out markers
                            let mut best_dist = f64::MAX;
                            let mut best_secs = raw_secs;

                            // Playhead
                            let ph_secs = state.playhead.to_secs_f64();
                            let d = (raw_secs - ph_secs).abs();
                            if d < snap_thresh_secs && d < best_dist {
                                best_dist = d;
                                best_secs = ph_secs;
                            }
                            // All clip edges
                            for track in &state.sequence().tracks {
                                for c in &track.clips {
                                    for &edge_secs in &[c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64()] {
                                        let d = (raw_secs - edge_secs).abs();
                                        if d < snap_thresh_secs && d < best_dist {
                                            best_dist = d;
                                            best_secs = edge_secs;
                                        }
                                    }
                                }
                            }
                            // In/out markers
                            if let Some(inp) = state.sequence().in_point {
                                let d = (raw_secs - inp.to_secs_f64()).abs();
                                if d < snap_thresh_secs && d < best_dist {
                                    best_dist = d;
                                    best_secs = inp.to_secs_f64();
                                }
                            }
                            if let Some(outp) = state.sequence().out_point {
                                let d = (raw_secs - outp.to_secs_f64()).abs();
                                if d < snap_thresh_secs && d < best_dist {
                                    best_secs = outp.to_secs_f64();
                                }
                            }

                            if best_dist < f64::MAX {
                                blade_x = content_rect.left() + best_secs as f32 * zoom - scroll;
                            }
                        }

                        // Draw blade line (yellow if snapped, red if unsnapped)
                        let snapped = (blade_x - pos.x).abs() > 0.5;
                        let blade_color = if snapped {
                            egui::Color32::from_rgb(255, 220, 60)
                        } else {
                            egui::Color32::from_rgb(255, 60, 60)
                        };
                        painter.line_segment(
                            [
                                egui::pos2(blade_x, clip_rect.top()),
                                egui::pos2(blade_x, clip_rect.bottom()),
                            ],
                            egui::Stroke::new(if snapped { 2.0 } else { 1.0 }, blade_color),
                        );
                    }
                }
            }

        }

        // Draw transition indicators between clips
        if let Some(track) = state.sequence().tracks.get(track_idx) {
            for transition in &track.transitions {
                // Find clip_b (the incoming clip) to determine the transition position
                let clip_b = track.find_clip(transition.clip_b_id);
                if let Some(clip_b) = clip_b {
                    let trans_start_secs = clip_b.timeline_in.to_secs_f64();
                    let trans_dur_secs = transition.duration.to_secs_f64();
                    let trans_start_x = content_rect.left() + (trans_start_secs * zoom as f64) as f32 - scroll;
                    let trans_end_x = content_rect.left() + ((trans_start_secs + trans_dur_secs) * zoom as f64) as f32 - scroll;

                    // Only draw if visible
                    if trans_end_x > content_rect.left() && trans_start_x < content_rect.right() {
                        let trans_w = (trans_end_x - trans_start_x).max(4.0);
                        let indicator_rect = egui::Rect::from_min_size(
                            egui::pos2(trans_start_x, track_rect.top()),
                            egui::vec2(trans_w, track_height),
                        );
                        // Semi-transparent overlay
                        painter.rect_filled(
                            indicator_rect,
                            0.0,
                            egui::Color32::from_rgba_premultiplied(255, 180, 40, 35),
                        );
                        // Diamond marker at center
                        let cx = (trans_start_x + trans_end_x) / 2.0;
                        let cy = track_rect.center().y;
                        let d = 5.0;
                        let diamond = vec![
                            egui::pos2(cx, cy - d),
                            egui::pos2(cx + d, cy),
                            egui::pos2(cx, cy + d),
                            egui::pos2(cx - d, cy),
                        ];
                        painter.add(egui::Shape::convex_polygon(
                            diamond,
                            egui::Color32::from_rgb(255, 200, 60),
                            egui::Stroke::new(1.0, egui::Color32::from_rgb(200, 150, 30)),
                        ));
                        // Label if wide enough
                        if trans_w > 40.0 {
                            let kind_label = match transition.kind {
                                klipp_timeline::TransitionKind::CrossDissolve => "Dissolve",
                                klipp_timeline::TransitionKind::WipeLeft => "Wipe L",
                                klipp_timeline::TransitionKind::WipeRight => "Wipe R",
                                klipp_timeline::TransitionKind::WipeUp => "Wipe U",
                                klipp_timeline::TransitionKind::WipeDown => "Wipe D",
                                klipp_timeline::TransitionKind::DipToBlack => "Dip Blk",
                                klipp_timeline::TransitionKind::DipToWhite => "Dip Wht",
                            };
                            painter.text(
                                egui::pos2(cx, cy - d - 8.0),
                                egui::Align2::CENTER_BOTTOM,
                                kind_label,
                                egui::FontId::proportional(9.0),
                                egui::Color32::from_rgb(255, 220, 100),
                            );
                        }
                    }
                }
            }
        }

        // Draw anchor connection lines for connected clips
        for clip in &clips {
            if let Some(ref anchor) = clip.magnetic_anchor {
                if let Some(primary_idx) = state.sequence().primary_track_index() {
                    let anchor_clip = state.sequence().tracks[primary_idx].clips.iter()
                        .find(|c| c.id == anchor.anchor_clip_id);
                    if let Some(ac) = anchor_clip {
                        let anchor_x = content_rect.left()
                            + (ac.timeline_in.to_secs_f64() as f32 + anchor.offset.to_secs_f64() as f32) * zoom
                            - scroll;

                        // Only draw if visible
                        if anchor_x >= content_rect.left() - 10.0 && anchor_x <= content_rect.right() + 10.0 {
                            let _clip_x = content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                            let clip_top = track_rect.top();

                            // Find primary track rect for the bottom of the line
                            let primary_bottom = self.track_rects.iter()
                                .find(|(idx, _, _)| *idx == primary_idx)
                                .map(|(_, _, r)| r.bottom())
                                .unwrap_or(track_rect.bottom());

                            let line_color = egui::Color32::from_rgba_unmultiplied(232, 160, 32, 160);

                            // Dashed vertical line
                            let dash_len = 5.0_f32;
                            let gap_len = 3.0_f32;
                            let start_y = clip_top.min(primary_bottom);
                            let end_y = clip_top.max(primary_bottom);
                            let mut y = start_y;
                            while y < end_y {
                                let dash_end = (y + dash_len).min(end_y);
                                painter.line_segment(
                                    [egui::pos2(anchor_x, y), egui::pos2(anchor_x, dash_end)],
                                    egui::Stroke::new(1.0, line_color),
                                );
                                y += dash_len + gap_len;
                            }

                            // Anchor dot on the primary track
                            painter.circle_filled(
                                egui::pos2(anchor_x, primary_bottom),
                                3.0,
                                line_color,
                            );
                        }
                    }
                }
            }
        }

        // Draw collision indicators for overlapping clips on connected tracks
        if state.sequence().tracks.get(track_idx).is_some_and(|t| {
            t.magnetic_role == klipp_timeline::TrackRole::Connected
                || t.magnetic_role == klipp_timeline::TrackRole::ConnectedStoryline
        }) {
            for (ci, clip) in clips.iter().enumerate() {
                if clip.kind.is_gap() { continue; }
                for (oi, other) in clips.iter().enumerate() {
                    if ci >= oi { continue; }
                    if other.kind.is_gap() { continue; }
                    // Check overlap
                    if clip.timeline_in < other.timeline_out && clip.timeline_out > other.timeline_in {
                        let overlap_start = if clip.timeline_in > other.timeline_in { clip.timeline_in } else { other.timeline_in };
                        let overlap_end = if clip.timeline_out < other.timeline_out { clip.timeline_out } else { other.timeline_out };
                        let ox = content_rect.left() + overlap_start.to_secs_f64() as f32 * zoom - scroll;
                        let ow = (overlap_end - overlap_start).to_secs_f64() as f32 * zoom;
                        let overlap_rect = egui::Rect::from_min_size(
                            egui::pos2(ox, track_rect.top() + 2.0),
                            egui::vec2(ow, track_rect.height() - 4.0),
                        );
                        if overlap_rect.right() < content_rect.left() || overlap_rect.left() > content_rect.right() {
                            continue;
                        }
                        // Amber striped overlay
                        let stripe_color = egui::Color32::from_rgba_unmultiplied(255, 200, 0, 80);
                        let stripe_spacing = 6.0_f32;
                        let clip_painter = painter.with_clip_rect(overlap_rect);
                        let mut sx = overlap_rect.min.x - overlap_rect.height();
                        while sx < overlap_rect.max.x {
                            let p1 = egui::pos2(sx, overlap_rect.max.y);
                            let p2 = egui::pos2(sx + overlap_rect.height(), overlap_rect.min.y);
                            clip_painter.line_segment([p1, p2], egui::Stroke::new(1.5, stripe_color));
                            sx += stripe_spacing;
                        }
                        // Amber border
                        painter.rect_stroke(overlap_rect, 2.0,
                            egui::Stroke::new(1.5, egui::Color32::from_rgb(0xff, 0xc8, 0x00)));
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
            ui.set_min_width(160.0);
            if let Some(clip_id) = state.selected_clip {
                if ui.button("Remove Clip").clicked() {
                    cmds.push(AppCommand::RemoveClip { clip_id });
                    ui.close_menu();
                }

                // Link/Unlink
                let clip_info = state.sequence().tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| c.id == clip_id);
                if let Some(clip) = clip_info {
                    if clip.linked_clip_id.is_some() {
                        if ui.button("Unlink Clip").clicked() {
                            cmds.push(AppCommand::UnlinkClip { clip_id });
                            ui.close_menu();
                        }
                    } else {
                        // Find a candidate clip on another track at the same position
                        let candidate = state.sequence().tracks.iter().flat_map(|t| t.clips.iter())
                            .find(|c| {
                                c.id != clip_id
                                    && c.linked_clip_id.is_none()
                                    && c.timeline_in == clip.timeline_in
                            });
                        if let Some(partner) = candidate {
                            let partner_id = partner.id;
                            if ui.button("Link to Matching Clip").clicked() {
                                cmds.push(AppCommand::LinkClips { clip_a: clip_id, clip_b: partner_id });
                                ui.close_menu();
                            }
                        }
                    }
                }

                // Reveal source in media browser
                if let Some(clip) = clip_info {
                    if let Some(mid) = clip.media_id() {
                        if ui.button("Reveal Source in Browser").clicked() {
                            cmds.push(AppCommand::RevealMediaInBrowser { media_id: mid });
                            ui.close_menu();
                        }
                    }
                }

                ui.separator();

                // Three-point edit operations
                let has_source = state.source_media_id.is_some();
                let has_in_out = state.sequence().in_point.is_some() && state.sequence().out_point.is_some();
                if ui.add_enabled(has_source, egui::Button::new("Insert (,)")).clicked() {
                    cmds.push(AppCommand::InsertEdit);
                    ui.close_menu();
                }
                if ui.add_enabled(has_source, egui::Button::new("Overwrite (.)")).clicked() {
                    cmds.push(AppCommand::OverwriteEdit);
                    ui.close_menu();
                }
                if ui.add_enabled(has_in_out, egui::Button::new("Lift (;)")).clicked() {
                    cmds.push(AppCommand::LiftEdit);
                    ui.close_menu();
                }
                if ui.add_enabled(has_in_out, egui::Button::new("Extract (')")).clicked() {
                    cmds.push(AppCommand::ExtractEdit);
                    ui.close_menu();
                }

                // Transitions
                ui.separator();
                // Find adjacent clip pairs involving the selected clip
                if let Some(track) = state.sequence().tracks.get(track_idx) {
                    let clip_pos = track.clips.iter().position(|c| c.id == clip_id);
                    if let Some(pos) = clip_pos {
                        // Check if there's a clip after this one (transition at the cut point)
                        if pos + 1 < track.clips.len() {
                            let next_clip = &track.clips[pos + 1];
                            let has_transition = track.transitions.iter().any(|t| {
                                t.clip_a_id == clip_id && t.clip_b_id == next_clip.id
                            });
                            let next_id = next_clip.id;
                            if has_transition {
                                // Show existing transition with option to change/remove
                                let tid = track.transitions.iter()
                                    .find(|t| t.clip_a_id == clip_id && t.clip_b_id == next_id)
                                    .map(|t| t.id);
                                if let Some(tid) = tid {
                                    if ui.button("Remove Transition →").clicked() {
                                        cmds.push(AppCommand::RemoveTransition {
                                            track_index: track_idx,
                                            transition_id: tid,
                                        });
                                        ui.close_menu();
                                    }
                                }
                            } else {
                                ui.menu_button("Add Transition →", |ui| {
                                    for &(kind_id, label) in klipp_engine::transition::all_transition_kinds() {
                                        if ui.button(label).clicked() {
                                            cmds.push(AppCommand::AddTransition {
                                                track_index: track_idx,
                                                clip_a_id: clip_id,
                                                clip_b_id: next_id,
                                                kind_id,
                                                duration_frames: 24,
                                            });
                                            ui.close_menu();
                                        }
                                    }
                                });
                            }
                        }
                        // Check if there's a clip before this one
                        if pos > 0 {
                            let prev_clip = &track.clips[pos - 1];
                            let has_transition = track.transitions.iter().any(|t| {
                                t.clip_a_id == prev_clip.id && t.clip_b_id == clip_id
                            });
                            let prev_id = prev_clip.id;
                            if has_transition {
                                let tid = track.transitions.iter()
                                    .find(|t| t.clip_a_id == prev_id && t.clip_b_id == clip_id)
                                    .map(|t| t.id);
                                if let Some(tid) = tid {
                                    if ui.button("Remove Transition ←").clicked() {
                                        cmds.push(AppCommand::RemoveTransition {
                                            track_index: track_idx,
                                            transition_id: tid,
                                        });
                                        ui.close_menu();
                                    }
                                }
                            } else {
                                ui.menu_button("Add Transition ←", |ui| {
                                    for &(kind_id, label) in klipp_engine::transition::all_transition_kinds() {
                                        if ui.button(label).clicked() {
                                            cmds.push(AppCommand::AddTransition {
                                                track_index: track_idx,
                                                clip_a_id: prev_id,
                                                clip_b_id: clip_id,
                                                kind_id,
                                                duration_frames: 24,
                                            });
                                            ui.close_menu();
                                        }
                                    }
                                });
                            }
                        }
                    }
                }

                ui.separator();

                // Group / Ungroup
                let has_multi = state.selected_clips.len() >= 2
                    || (state.selected_clips.len() == 1 && state.selected_clip.is_some_and(|id| !state.selected_clips.contains(&id)));
                if ui.add_enabled(has_multi, egui::Button::new("Group").shortcut_text("Ctrl+G")).clicked() {
                    let mut ids = state.selected_clips.clone();
                    if let Some(primary) = state.selected_clip {
                        if !ids.contains(&primary) {
                            ids.push(primary);
                        }
                    }
                    cmds.push(AppCommand::GroupClips { clip_ids: ids });
                    ui.close_menu();
                }
                let clip_group = state.sequence().tracks.iter()
                    .flat_map(|t| t.clips.iter())
                    .find(|c| Some(c.id) == state.selected_clip)
                    .and_then(|c| c.group_id);
                if ui.add_enabled(clip_group.is_some(), egui::Button::new("Ungroup").shortcut_text("Ctrl+Shift+G")).clicked() {
                    if let Some(gid) = clip_group {
                        cmds.push(AppCommand::UngroupClips { group_id: gid });
                    }
                    ui.close_menu();
                }

                // Magnetic connect/disconnect
                ui.separator();
                let is_on_primary = state.sequence().tracks.get(track_idx)
                    .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary);
                if !is_on_primary {
                    let clip_data = state.sequence().tracks.iter()
                        .flat_map(|t| t.clips.iter())
                        .find(|c| c.id == clip_id);
                    if let Some(cd) = clip_data {
                        if cd.magnetic_anchor.is_some() {
                            ui.label("\u{26D3} Connected to Primary");
                            if ui.button("Disconnect (Shift+G)").clicked() {
                                ui.close_menu();
                            }
                        } else if ui.button("Connect to Primary (G)").clicked() {
                            ui.close_menu();
                        }
                    }
                    // Storyline promotion
                    let track_role = state.sequence().tracks.get(track_idx)
                        .map(|t| t.magnetic_role.clone());
                    if let Some(role) = track_role {
                        if role == klipp_timeline::TrackRole::Connected {
                            if ui.button("Promote to Storyline (Shift+S)").clicked() {
                                ui.close_menu();
                            }
                        } else if role == klipp_timeline::TrackRole::ConnectedStoryline {
                            ui.label("\u{1F517} Connected Storyline");
                            if ui.button("Break Apart Storyline (Shift+S)").clicked() {
                                ui.close_menu();
                            }
                        }
                    }
                }

                // Collision resolution (connected/storyline tracks only)
                if !is_on_primary {
                    let collision = state.sequence().tracks.get(track_idx).and_then(|track| {
                        let clip = track.clips.iter().find(|c| c.id == clip_id)?;
                        // Find overlapping clip on same track
                        track.clips.iter().find(|other| {
                            other.id != clip_id
                                && !other.kind.is_gap()
                                && clip.timeline_in < other.timeline_out
                                && clip.timeline_out > other.timeline_in
                        }).map(|other| other.id)
                    });
                    if let Some(colliding_id) = collision {
                        ui.separator();
                        if ui.button("Push to Resolve").clicked() {
                            let result = klipp_timeline::magnetic::MagneticEditor::resolve_collision_push(
                                state.sequence_mut(), clip_id, colliding_id);
                            state.history.push(crate::history::HistoryEntry {
                                sequence_idx: state.active_sequence_idx,
                                command: crate::history::EditCommand::Magnetic(result.command),
                            });
                            state.dirty = true;
                            cmds.push(AppCommand::SequenceUpdated);
                            ui.close_menu();
                        }
                    }
                }

                // Lock Track
                ui.separator();
                let track_locked = state.sequence().tracks.get(track_idx).is_some_and(|t| t.locked);
                let lock_label = if track_locked { "Unlock Track" } else { "Lock Track" };
                if ui.button(lock_label).clicked() {
                    let track_id = state.sequence().tracks[track_idx].id;
                    cmds.push(AppCommand::SetTrackLocked { track_id, locked: !track_locked });
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
                let x_in_content = pos.x - content_rect.left() + scroll;
                let click_secs = (x_in_content as f64 / zoom as f64).max(0.0);
                let click_tc = RationalTime::from_secs_f64(click_secs, fps.rate());

                if let Some((_, hit_clip)) = self.hit_test_clip_body(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    if state.active_tool == Tool::Blade {
                        // Snap blade position when snap is enabled (hold Shift to override)
                        let mut cut_secs = click_secs;
                        let blade_snap = state.snap_enabled && !ui.input(|i| i.modifiers.shift);
                        if blade_snap {
                            let snap_thresh_secs = 20.0 / zoom as f64;
                            let mut best_dist = f64::MAX;
                            // Playhead
                            let ph_secs = state.playhead.to_secs_f64();
                            let d = (click_secs - ph_secs).abs();
                            if d < snap_thresh_secs && d < best_dist {
                                best_dist = d;
                                cut_secs = ph_secs;
                            }
                            // Clip edges
                            for track in &state.sequence().tracks {
                                for c in &track.clips {
                                    for &edge_secs in &[c.timeline_in.to_secs_f64(), c.timeline_out.to_secs_f64()] {
                                        let d = (click_secs - edge_secs).abs();
                                        if d < snap_thresh_secs && d < best_dist {
                                            best_dist = d;
                                            cut_secs = edge_secs;
                                        }
                                    }
                                }
                            }
                            // In/out markers
                            if let Some(inp) = state.sequence().in_point {
                                let d = (click_secs - inp.to_secs_f64()).abs();
                                if d < snap_thresh_secs && d < best_dist {
                                    best_dist = d;
                                    cut_secs = inp.to_secs_f64();
                                }
                            }
                            if let Some(outp) = state.sequence().out_point {
                                let d = (click_secs - outp.to_secs_f64()).abs();
                                if d < snap_thresh_secs && d < best_dist {
                                    cut_secs = outp.to_secs_f64();
                                }
                            }
                            let _ = best_dist;
                        }
                        let cut_tc = RationalTime::from_secs_f64(cut_secs, fps.rate());
                        cmds.push(AppCommand::BladeClip {
                            track_index: track_idx,
                            clip_id: hit_clip.id,
                            cut_at: cut_tc,
                        });
                    } else if state.active_tool == Tool::Range {
                        // Range tool: click on clip = set range to clip boundaries
                        cmds.push(AppCommand::SetSequenceInPoint(hit_clip.timeline_in));
                        cmds.push(AppCommand::SetSequenceOutPoint(hit_clip.timeline_out));
                        state.toast.show("Range: clip selected");
                    } else {
                        let modifiers = ui.input(|i| i.modifiers);
                        if modifiers.shift || modifiers.ctrl || modifiers.command {
                            // Multi-select: toggle in selected_clips
                            if let Some(pos) = state.selected_clips.iter().position(|&id| id == hit_clip.id) {
                                state.selected_clips.remove(pos);
                            } else {
                                state.selected_clips.push(hit_clip.id);
                            }
                        } else {
                            // If clip is grouped, select all group members
                            if let Some(gid) = hit_clip.group_id {
                                state.selected_clips = state.sequence().tracks.iter()
                                    .flat_map(|t| t.clips.iter())
                                    .filter(|c| c.group_id == Some(gid))
                                    .map(|c| c.id)
                                    .collect();
                            } else {
                                state.selected_clips.clear();
                            }
                        }
                        cmds.push(AppCommand::SelectClip(Some(hit_clip.id)));
                    }
                } else {
                    // Clicked empty area — move playhead and deselect
                    cmds.push(AppCommand::SeekTo(click_tc));
                    if self.dragging_clip.is_none() && self.trimming_clip.is_none() {
                        state.selected_clips.clear();
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
                // Range tool: drag anywhere in track area sets range
                else if state.active_tool == Tool::Range {
                    let x_in_content = pos.x - content_rect.left() + scroll;
                    let anchor_secs = (x_in_content as f64 / zoom as f64).max(0.0);
                    state.range_drag = Some(crate::state::RangeDragState {
                        kind: crate::state::RangeDragKind::RulerCreate { anchor_secs },
                        original_in: state.sequence().in_point,
                        original_out: state.sequence().out_point,
                    });
                }
                // Second: check if pointer is inside a clip body (drag/move)
                else if let Some((_cr, hit_clip)) = self.hit_test_clip_body(&clips, content_rect, track_rect, zoom, scroll, track_height, pos) {
                    tracing::debug!("drag_started MOVE: clip={:?}", hit_clip.id);
                    if state.active_tool == Tool::Select {
                        let linked_id = hit_clip.linked_clip_id;
                        let linked_orig = linked_id.and_then(|lid| {
                            state.sequence().tracks.iter()
                                .flat_map(|t| t.clips.iter())
                                .find(|c| c.id == lid)
                                .map(|c| c.timeline_in)
                        });
                        self.dragging_clip = Some(DragState {
                            clip_id: hit_clip.id,
                            track_idx,
                            start_x: pos.x,
                            original_timeline_in: hit_clip.timeline_in,
                            linked_clip_id: linked_id,
                            linked_original_in: linked_orig,
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
            for track in &state.sequence().tracks {
                for clip in &track.clips {
                    targets.push((clip.timeline_in.to_secs_f64(), clip.timeline_in, Some(clip.id)));
                    targets.push((clip.timeline_out.to_secs_f64(), clip.timeline_out, Some(clip.id)));
                }
            }
            targets.push((state.playhead.to_secs_f64(), state.playhead, None));
            targets.push((0.0, RationalTime { value: 0, rate: fps.rate() }, None));
            // Snap to in/out markers if set
            if let Some(inp) = state.sequence().in_point {
                targets.push((inp.to_secs_f64(), inp, None));
            }
            if let Some(outp) = state.sequence().out_point {
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

        // Handle Range tool drag in track area (update in/out as user drags)
        if state.active_tool == Tool::Range {
            if let Some(ref drag) = state.range_drag.clone() {
                if let crate::state::RangeDragKind::RulerCreate { anchor_secs } = drag.kind {
                    if let Some(pos) = pointer_pos {
                        if pointer_down {
                            let x_in_content = pos.x - content_rect.left() + scroll;
                            let current_secs = (x_in_content as f64 / zoom as f64).max(0.0);
                            let anchor_tc = RationalTime::from_secs_f64(anchor_secs, fps.rate());
                            let current_tc = RationalTime::from_secs_f64(current_secs, fps.rate());
                            if current_secs < anchor_secs {
                                cmds.push(AppCommand::SetSequenceInPoint(current_tc));
                                cmds.push(AppCommand::SetSequenceOutPoint(anchor_tc));
                            } else {
                                cmds.push(AppCommand::SetSequenceInPoint(anchor_tc));
                                cmds.push(AppCommand::SetSequenceOutPoint(current_tc));
                            }
                        }
                        if pointer_released {
                            if let Some(drag) = state.range_drag.take() {
                                let new_in = state.sequence().in_point;
                                let new_out = state.sequence().out_point;
                                if drag.original_in != new_in || drag.original_out != new_out {
                                    state.history.push(crate::history::HistoryEntry {
                                        sequence_idx: state.active_sequence_idx,
                                        command: crate::history::EditCommand::SetRange {
                                            old_in: drag.original_in,
                                            old_out: drag.original_out,
                                            new_in,
                                            new_out,
                                        },
                                    });
                                }
                            }
                        }
                    }
                }
            }
            // Show crosshair cursor when Range tool is active
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }

        // Handle ongoing trim (only process on the trim's own track to avoid duplicate tooltips)
        if let Some(ref trim) = self.trimming_clip {
            if trim.track_idx != track_idx {
                // Skip — will be handled when we draw the correct track
            } else if let Some(pos) = pointer_pos {
                if pointer_down {
                    let dx = pos.x - trim.start_x;
                    let dt_secs = dx as f64 / zoom as f64;
                    let dt_frames = RationalTime::from_secs_f64(dt_secs, fps.rate());

                    let media_duration = state.sequence().tracks.get(trim.track_idx)
                        .and_then(|t| t.find_clip(trim.clip_id))
                        .and_then(|c| c.media_id())
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
                                // Clamp so timeline_in doesn't go negative
                                if new_timeline_in.value < 0 {
                                    new_timeline_in.value = 0;
                                }

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

                                if let Some(track) = state.sequence_mut().tracks.get_mut(trim.track_idx) {
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

                                if let Some(track) = state.sequence_mut().tracks.get_mut(trim.track_idx) {
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

                            if let Some(track) = state.sequence_mut().tracks.get_mut(trim.track_idx) {
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

            if pointer_released && trim.track_idx == track_idx {
                if let Some(track) = state.sequence().tracks.get(trim.track_idx) {
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

        // Handle ongoing drag (only process once, on the drag's source track)
        if let Some(ref drag) = self.dragging_clip {
            if drag.track_idx != track_idx {
                // Skip — will be handled when we draw the correct track
            } else if let Some(pos) = pointer_pos {
                // Determine which track the pointer is hovering over
                let hover_track_idx = self.track_rects.iter()
                    .find(|(_, _, rect)| rect.contains(pos))
                    .map(|(idx, _, _)| *idx);

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

                    if let Some(track) = state.sequence().tracks.get(drag.track_idx) {
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

                    let is_primary = state.sequence().tracks.get(drag.track_idx)
                        .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary);

                    if is_primary {
                        // Alt override: temporarily treat as Overwrite when Alt is held
                        let alt_held = ui.input(|i| i.modifiers.alt);
                        let effective_mode = if alt_held {
                            klipp_timeline::magnetic::EditMode::Overwrite
                        } else {
                            state.edit_mode
                        };

                        // PRIMARY TRACK: Don't move clip in sequence.
                        // Keep it at original position. Draw a floating ghost + insertion indicator.
                        let clip_dur = state.sequence().tracks.get(drag.track_idx)
                            .and_then(|t| t.find_clip(drag.clip_id))
                            .map(|c| c.duration().to_secs_f64() as f32)
                            .unwrap_or(1.0);

                        // Ghost clip following cursor (drawn on tooltip layer so it's above everything)
                        let ghost_x = content_rect.left() + final_in.to_secs_f64() as f32 * zoom - scroll;
                        let ghost_w = clip_dur * zoom;
                        let ghost_rect = egui::Rect::from_min_size(
                            egui::pos2(ghost_x, track_rect.top() + 2.0),
                            egui::vec2(ghost_w, track_rect.height() - 4.0),
                        );

                        // Draw ghost with 50% opacity
                        let ghost_fill = egui::Color32::from_rgba_unmultiplied(60, 80, 140, 128);
                        let ghost_border = egui::Color32::from_rgba_unmultiplied(120, 160, 240, 180);
                        painter.rect_filled(ghost_rect, 3.0, ghost_fill);
                        painter.rect_stroke(ghost_rect, 3.0, egui::Stroke::new(2.0, ghost_border));

                        // Draw filmstrip in ghost if available
                        if let Some(media_id) = state.sequence().tracks.get(drag.track_idx)
                            .and_then(|t| t.find_clip(drag.clip_id))
                            .and_then(|c| c.media_id())
                        {
                            if let Some(tex) = state.thumbnail_textures.get(&media_id) {
                                let thumb_painter = painter.with_clip_rect(ghost_rect);
                                thumb_painter.image(
                                    tex.id(),
                                    ghost_rect,
                                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 128),
                                );
                            }
                        }

                        // Insertion indicator: find where the clip would land between existing clips
                        let target_secs = final_in.to_secs_f64();
                        if let Some(track) = state.sequence().tracks.get(drag.track_idx) {
                            // Find insertion point (between which clips)
                            let mut insert_x = content_rect.left() - scroll; // before all clips
                            for c in &track.clips {
                                if c.id == drag.clip_id { continue; }
                                let c_mid = (c.timeline_in.to_secs_f64() + c.timeline_out.to_secs_f64()) / 2.0;
                                if target_secs > c_mid {
                                    insert_x = content_rect.left() + c.timeline_out.to_secs_f64() as f32 * zoom - scroll;
                                }
                            }
                            // Draw insertion line — amber for Insert, blue for Overwrite
                            let insert_color = match effective_mode {
                                klipp_timeline::magnetic::EditMode::Insert => egui::Color32::from_rgb(232, 160, 32),
                                klipp_timeline::magnetic::EditMode::Overwrite => egui::Color32::from_rgb(60, 120, 220),
                            };
                            painter.line_segment(
                                [egui::pos2(insert_x, track_rect.top()), egui::pos2(insert_x, track_rect.bottom())],
                                egui::Stroke::new(3.0, insert_color),
                            );
                            // Small triangles at top and bottom of insertion line
                            let tri_size = 5.0;
                            painter.add(egui::Shape::convex_polygon(
                                vec![
                                    egui::pos2(insert_x, track_rect.top()),
                                    egui::pos2(insert_x - tri_size, track_rect.top() - tri_size),
                                    egui::pos2(insert_x + tri_size, track_rect.top() - tri_size),
                                ],
                                insert_color, egui::Stroke::NONE,
                            ));
                            painter.add(egui::Shape::convex_polygon(
                                vec![
                                    egui::pos2(insert_x, track_rect.bottom()),
                                    egui::pos2(insert_x - tri_size, track_rect.bottom() + tri_size),
                                    egui::pos2(insert_x + tri_size, track_rect.bottom() + tri_size),
                                ],
                                insert_color, egui::Stroke::NONE,
                            ));
                        }

                        // Drop zone highlight for primary track — amber glow at ghost position
                        if snapped {
                            let drop_rect = egui::Rect::from_min_size(
                                egui::pos2(ghost_x - 1.0, track_rect.top() + 1.0),
                                egui::vec2(ghost_w + 2.0, track_rect.height() - 2.0),
                            );
                            painter.rect_stroke(drop_rect, 4.0,
                                egui::Stroke::new(3.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 80)));
                            painter.rect_stroke(drop_rect, 4.0,
                                egui::Stroke::new(1.5, egui::Color32::from_rgb(232, 160, 32)));
                        }

                        // ── Items 11-14: Cascade preview, ripple arrows, connected previews, anchor snap ──
                        if let Some(track) = state.sequence().tracks.get(drag.track_idx) {
                            let amber = egui::Color32::from_rgb(232, 160, 32);
                            let drag_clip_in = drag.original_timeline_in;
                            let delta_secs = final_in.to_secs_f64() - drag_clip_in.to_secs_f64();

                            // Item 12: Ripple direction arrows on primary clips in Insert mode
                            if effective_mode == klipp_timeline::magnetic::EditMode::Insert {
                                let insert_secs = final_in.to_secs_f64();
                                for c in &track.clips {
                                    if c.id == drag.clip_id { continue; }
                                    let c_in_secs = c.timeline_in.to_secs_f64();
                                    // Clips to the right of insertion point will ripple right
                                    let will_ripple_right = c_in_secs >= insert_secs && delta_secs > 0.01;
                                    let will_ripple_left = c_in_secs >= drag_clip_in.to_secs_f64() && delta_secs < -0.01 && c_in_secs > insert_secs;
                                    if will_ripple_right || will_ripple_left {
                                        let c_rect = Self::clip_screen_rect(c, content_rect, track_rect, zoom, scroll, track_height);
                                        if c_rect.right() < content_rect.left() || c_rect.left() > content_rect.right() {
                                            continue; // off-screen
                                        }
                                        let c_w = c_rect.width();
                                        let arrow_right = will_ripple_right;
                                        let arrow_h = 6.0_f32;
                                        let arrow_w = 8.0_f32;
                                        let cy = c_rect.center().y;

                                        // Determine number of arrows based on clip width
                                        let arrow_count = if c_w > 40.0 { 3 } else if c_w > 20.0 { 1 } else { 0 };
                                        if arrow_count > 0 {
                                            let spacing = c_w / (arrow_count as f32 + 1.0);
                                            for i in 0..arrow_count {
                                                let ax = c_rect.left() + spacing * (i as f32 + 1.0);
                                                let points = if arrow_right {
                                                    vec![
                                                        egui::pos2(ax - arrow_w / 2.0, cy - arrow_h / 2.0),
                                                        egui::pos2(ax + arrow_w / 2.0, cy),
                                                        egui::pos2(ax - arrow_w / 2.0, cy + arrow_h / 2.0),
                                                    ]
                                                } else {
                                                    vec![
                                                        egui::pos2(ax + arrow_w / 2.0, cy - arrow_h / 2.0),
                                                        egui::pos2(ax - arrow_w / 2.0, cy),
                                                        egui::pos2(ax + arrow_w / 2.0, cy + arrow_h / 2.0),
                                                    ]
                                                };
                                                painter.add(egui::Shape::convex_polygon(
                                                    points,
                                                    egui::Color32::from_rgba_unmultiplied(232, 160, 32, 180),
                                                    egui::Stroke::NONE,
                                                ));
                                            }
                                        }
                                    }
                                }
                            }

                            // Items 11 + 13: Cascade preview — connected clips show preview positions
                            if delta_secs.abs() > 0.001 {
                                for (other_ti, other_track) in state.sequence().tracks.iter().enumerate() {
                                    if other_ti == drag.track_idx { continue; }
                                    let is_connected = other_track.magnetic_role == klipp_timeline::TrackRole::Connected
                                        || other_track.magnetic_role == klipp_timeline::TrackRole::ConnectedStoryline;
                                    if !is_connected { continue; }

                                    let other_track_rect = self.track_rects.iter()
                                        .find(|(idx, _, _)| *idx == other_ti)
                                        .map(|(_, _, r)| *r);
                                    let other_track_rect = match other_track_rect {
                                        Some(r) => r,
                                        None => continue,
                                    };
                                    let other_h = match other_track.kind {
                                        TrackKind::Audio => AUDIO_TRACK_HEIGHT,
                                        _ => VIDEO_TRACK_HEIGHT,
                                    };

                                    for clip in &other_track.clips {
                                        if clip.kind.is_gap() { continue; }
                                        // Check if this clip is anchored to a primary clip that would move
                                        let would_cascade = if other_track.magnetic_role == klipp_timeline::TrackRole::ConnectedStoryline {
                                            // Storyline: all clips cascade if any primary anchor moves
                                            other_track.storyline_anchor.as_ref().is_some_and(|sa| {
                                                track.clips.iter().any(|pc| pc.id == sa.anchor_clip_id)
                                            })
                                        } else {
                                            clip.magnetic_anchor.as_ref().is_some_and(|anchor| {
                                                // Cascade if the anchor is on a primary clip that ripples
                                                track.clips.iter().any(|pc| pc.id == anchor.anchor_clip_id)
                                            })
                                        };
                                        if !would_cascade { continue; }

                                        // Compute preview position with cascade delta
                                        let delta_rt = final_in - drag_clip_in;
                                        let preview_in = clip.timeline_in + delta_rt;
                                        let preview_in_secs = preview_in.to_secs_f64();
                                        let clip_dur_secs = clip.duration().to_secs_f64();

                                        // Item 11: Draw connected clip at preview position with 60% opacity
                                        let px = content_rect.left() + preview_in_secs as f32 * zoom - scroll;
                                        let pw = clip_dur_secs as f32 * zoom;
                                        let preview_rect = egui::Rect::from_min_size(
                                            egui::pos2(px, other_track_rect.top() + 2.0),
                                            egui::vec2(pw.max(2.0), other_h - 4.0),
                                        );
                                        if preview_rect.right() >= content_rect.left() && preview_rect.left() <= content_rect.right() {
                                            // 60% opacity fill
                                            painter.rect_filled(preview_rect, 3.0,
                                                egui::Color32::from_rgba_unmultiplied(60, 80, 140, 153));
                                            // Dashed amber border
                                            let dash_color = egui::Color32::from_rgba_unmultiplied(232, 160, 32, 180);
                                            let dash_len = 5.0_f32;
                                            let gap_len = 3.0_f32;
                                            // Top edge
                                            let mut dx = preview_rect.left();
                                            while dx < preview_rect.right() {
                                                let end_x = (dx + dash_len).min(preview_rect.right());
                                                painter.line_segment(
                                                    [egui::pos2(dx, preview_rect.top()), egui::pos2(end_x, preview_rect.top())],
                                                    egui::Stroke::new(1.5, dash_color),
                                                );
                                                dx += dash_len + gap_len;
                                            }
                                            // Bottom edge
                                            dx = preview_rect.left();
                                            while dx < preview_rect.right() {
                                                let end_x = (dx + dash_len).min(preview_rect.right());
                                                painter.line_segment(
                                                    [egui::pos2(dx, preview_rect.bottom()), egui::pos2(end_x, preview_rect.bottom())],
                                                    egui::Stroke::new(1.5, dash_color),
                                                );
                                                dx += dash_len + gap_len;
                                            }
                                        }

                                        // Item 13: Ghost outline at destination (30% opacity) if different from current
                                        let current_secs = clip.timeline_in.to_secs_f64();
                                        if (preview_in_secs - current_secs).abs() > 0.001 {
                                            let dest_x = content_rect.left() + preview_in_secs as f32 * zoom - scroll;
                                            let dest_rect = egui::Rect::from_min_size(
                                                egui::pos2(dest_x, other_track_rect.top() + 2.0),
                                                egui::vec2(pw.max(2.0), other_h - 4.0),
                                            );
                                            if dest_rect.right() >= content_rect.left() && dest_rect.left() <= content_rect.right() {
                                                painter.rect_stroke(dest_rect, 3.0,
                                                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 77)));
                                            }
                                        }

                                        // Draw preview anchor line from connected clip to primary
                                        if let Some(anchor) = &clip.magnetic_anchor {
                                            let anchor_clip = track.clips.iter().find(|pc| pc.id == anchor.anchor_clip_id);
                                            if let Some(ac) = anchor_clip {
                                                let anchor_preview_in = ac.timeline_in + delta_rt;
                                                let anchor_x = content_rect.left()
                                                    + (anchor_preview_in + anchor.offset).to_secs_f64() as f32 * zoom - scroll;
                                                let from_y = preview_rect.top();
                                                let to_y = track_rect.bottom();
                                                let (top_y, bottom_y) = if from_y < to_y { (from_y, to_y) } else { (to_y, from_y) };
                                                // Dashed amber anchor line
                                                let mut dy = top_y;
                                                while dy < bottom_y {
                                                    let end_y = (dy + 5.0).min(bottom_y);
                                                    painter.line_segment(
                                                        [egui::pos2(anchor_x, dy), egui::pos2(anchor_x, end_y)],
                                                        egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 120)),
                                                    );
                                                    dy += 8.0;
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            // Item 14: Anchor snap indicator during non-primary drag
                            // (This renders when dragging clips near/far from primary on connected tracks)
                            // Already handled in the non-primary drag section above.
                            let _ = amber; // suppress unused
                        }

                    } else {
                        // NON-PRIMARY: Legacy free movement
                        let delta = final_in - drag.original_timeline_in;

                        if let Some(track) = state.sequence_mut().tracks.get_mut(drag.track_idx) {
                            if let Some(clip) = track.find_clip_mut(drag.clip_id) {
                                let dur = clip.duration();
                                clip.timeline_in = final_in;
                                clip.timeline_out = clip.timeline_in + dur;
                            }
                        }

                        // Move linked partner by same delta (absolute from its original position)
                        if let (Some(lid), Some(orig_in)) = (drag.linked_clip_id, drag.linked_original_in) {
                            for track in &mut state.sequence_mut().tracks {
                                if let Some(lclip) = track.find_clip_mut(lid) {
                                    let dur = lclip.duration();
                                    let new_in = orig_in + delta;
                                    lclip.timeline_in = if new_in.value < 0 {
                                        RationalTime { value: 0, rate: new_in.rate }
                                    } else {
                                        new_in
                                    };
                                    lclip.timeline_out = lclip.timeline_in + dur;
                                    break;
                                }
                            }
                        }

                        // Drop zone highlight — amber glow around the clip when snapped (non-primary only)
                        if snapped {
                            if let Some(track) = state.sequence().tracks.get(drag.track_idx) {
                                if let Some(clip) = track.find_clip(drag.clip_id) {
                                    let dur_secs = clip.duration().to_secs_f64() as f32;
                                    let drop_x = content_rect.left() + clip.timeline_in.to_secs_f64() as f32 * zoom - scroll;
                                    let drop_w = dur_secs * zoom;
                                    let drop_rect = egui::Rect::from_min_size(
                                        egui::pos2(drop_x - 1.0, track_rect.top() + 1.0),
                                        egui::vec2(drop_w + 2.0, track_rect.height() - 2.0),
                                    );
                                    painter.rect_stroke(drop_rect, 4.0,
                                        egui::Stroke::new(3.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 80)));
                                    painter.rect_stroke(drop_rect, 4.0,
                                        egui::Stroke::new(1.5, egui::Color32::from_rgb(232, 160, 32)));
                                }
                            }
                        }
                    }

                    // Auto-connect/disconnect indicator during non-primary drag
                    if !is_primary {
                        let primary_rect = self.track_rects.iter()
                            .find(|(idx, _, _)| state.sequence().tracks.get(*idx)
                                .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary))
                            .map(|(_, _, r)| *r);
                        if let Some(primary_r) = primary_rect {
                            let drag_clip_y = if let Some(pos) = pointer_pos { pos.y } else { track_rect.center().y };
                            let primary_center_y = primary_r.center().y;
                            let dist_px = (drag_clip_y - primary_center_y).abs();
                            let track_h = track_rect.height();
                            let dist_tracks = dist_px / track_h;

                            let clip_is_connected = state.sequence().tracks.get(drag.track_idx)
                                .and_then(|t| t.find_clip(drag.clip_id))
                                .is_some_and(|c| c.magnetic_anchor.is_some());

                            let connect_threshold = klipp_timeline::magnetic::AUTO_CONNECT_TRACK_DISTANCE;
                            let disconnect_threshold = klipp_timeline::magnetic::AUTO_DISCONNECT_TRACK_DISTANCE;

                            if dist_tracks < connect_threshold && !clip_is_connected {
                                // Will auto-connect on drop — show chain icon + dashed anchor line
                                if let Some(pos) = pointer_pos {
                                    let amber = egui::Color32::from_rgb(232, 160, 32);
                                    let pill_rect = egui::Rect::from_min_size(
                                        egui::pos2(pos.x + 12.0, pos.y - 20.0),
                                        egui::vec2(90.0, 16.0),
                                    );
                                    painter.rect_filled(pill_rect, 4.0, egui::Color32::from_rgba_unmultiplied(30, 30, 30, 220));
                                    painter.rect_stroke(pill_rect, 4.0, egui::Stroke::new(1.0, amber));
                                    painter.text(pill_rect.center(), egui::Align2::CENTER_CENTER,
                                        "\u{229E} Will connect", egui::FontId::monospace(8.0), amber);

                                    // Dashed amber vertical line from dragged clip to primary
                                    let clip_x = state.sequence().tracks.get(drag.track_idx)
                                        .and_then(|t| t.find_clip(drag.clip_id))
                                        .map(|c| content_rect.left() + c.timeline_in.to_secs_f64() as f32 * zoom - scroll);
                                    if let Some(anchor_x) = clip_x {
                                        let (top_y, bottom_y) = if primary_r.center().y > track_rect.center().y {
                                            (track_rect.bottom(), primary_r.top())
                                        } else {
                                            (primary_r.bottom(), track_rect.top())
                                        };
                                        let mut dy = top_y;
                                        let dash_len = 5.0_f32;
                                        let gap_len = 3.0_f32;
                                        while dy < bottom_y {
                                            let end_y = (dy + dash_len).min(bottom_y);
                                            painter.line_segment(
                                                [egui::pos2(anchor_x, dy), egui::pos2(anchor_x, end_y)],
                                                egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(232, 160, 32, 160)),
                                            );
                                            dy += dash_len + gap_len;
                                        }
                                    }
                                }
                            } else if dist_tracks > disconnect_threshold && clip_is_connected {
                                // Will auto-disconnect on drop — show broken chain icon + red dashed line
                                if let Some(pos) = pointer_pos {
                                    let warn_color = egui::Color32::from_rgb(192, 96, 96);
                                    let pill_rect = egui::Rect::from_min_size(
                                        egui::pos2(pos.x + 12.0, pos.y - 20.0),
                                        egui::vec2(100.0, 16.0),
                                    );
                                    painter.rect_filled(pill_rect, 4.0, egui::Color32::from_rgba_unmultiplied(30, 30, 30, 220));
                                    painter.rect_stroke(pill_rect, 4.0, egui::Stroke::new(1.0, warn_color));
                                    painter.text(pill_rect.center(), egui::Align2::CENTER_CENTER,
                                        "\u{229F} Will disconnect", egui::FontId::monospace(8.0), warn_color);

                                    // Existing anchor line turns red-dashed to indicate it will break
                                    if let Some(anchor) = state.sequence().tracks.get(drag.track_idx)
                                        .and_then(|t| t.find_clip(drag.clip_id))
                                        .and_then(|c| c.magnetic_anchor.clone())
                                    {
                                        let anchor_clip_in = state.sequence().tracks.iter()
                                            .flat_map(|t| t.clips.iter())
                                            .find(|c| c.id == anchor.anchor_clip_id)
                                            .map(|c| c.timeline_in);
                                        if let Some(ac_in) = anchor_clip_in {
                                            let anchor_x = content_rect.left()
                                                + (ac_in + anchor.offset).to_secs_f64() as f32 * zoom - scroll;
                                            let (top_y, bottom_y) = if primary_r.center().y > track_rect.center().y {
                                                (track_rect.bottom(), primary_r.top())
                                            } else {
                                                (primary_r.bottom(), track_rect.top())
                                            };
                                            let mut dy = top_y;
                                            while dy < bottom_y {
                                                let end_y = (dy + 5.0).min(bottom_y);
                                                painter.line_segment(
                                                    [egui::pos2(anchor_x, dy), egui::pos2(anchor_x, end_y)],
                                                    egui::Stroke::new(1.0, egui::Color32::from_rgba_unmultiplied(192, 96, 96, 160)),
                                                );
                                                dy += 8.0;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Highlight target track if different from source
                    if let Some(target_idx) = hover_track_idx {
                        if target_idx != drag.track_idx {
                            let source_kind = state.sequence().tracks.get(drag.track_idx)
                                .map(|t| t.kind);
                            let target_kind = state.sequence().tracks.get(target_idx)
                                .map(|t| t.kind);
                            if source_kind == target_kind {
                                if let Some((_, _, rect)) = self.track_rects.iter()
                                    .find(|(idx, _, _)| *idx == target_idx)
                                {
                                    painter.rect_stroke(
                                        *rect, 0.0,
                                        egui::Stroke::new(2.0, egui::Color32::from_rgb(100, 180, 255)),
                                    );
                                }
                            }
                        }
                    }

                    let _ = snapped; // suppress unused warning
                }
            }

            if pointer_released && drag.track_idx == track_idx {
                let clip_id = drag.clip_id;
                let from_track = drag.track_idx;

                // Check if pointer is over a different compatible track
                let target_track = pointer_pos.and_then(|pos| {
                    self.track_rects.iter()
                        .find(|(_, _, rect)| rect.contains(pos))
                        .map(|(idx, _, _)| *idx)
                });

                if let Some(to_track) = target_track {
                    if to_track != from_track {
                        let source_kind = state.sequence().tracks.get(from_track).map(|t| t.kind);
                        let target_kind = state.sequence().tracks.get(to_track).map(|t| t.kind);
                        if source_kind == target_kind {
                            // Cross-track move
                            if let Some(track) = state.sequence().tracks.get(from_track) {
                                if let Some(clip) = track.find_clip(clip_id) {
                                    cmds.push(AppCommand::MoveClipToTrack {
                                        clip_id,
                                        new_timeline_in: clip.timeline_in,
                                        from_track,
                                        to_track,
                                    });
                                }
                            }
                        } else {
                            // Incompatible track type — revert position
                            if let Some(track) = state.sequence_mut().tracks.get_mut(from_track) {
                                if let Some(clip) = track.find_clip_mut(clip_id) {
                                    let dur = clip.duration();
                                    clip.timeline_in = drag.original_timeline_in;
                                    clip.timeline_out = drag.original_timeline_in + dur;
                                }
                            }
                        }
                    } else {
                        // Same track move
                        let is_primary = state.sequence().tracks.get(from_track)
                            .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary);
                        if is_primary {
                            // Magnetic: route by edit mode (Insert = ripple move, Overwrite = replace)
                            let target_time = pointer_pos.map(|pos| {
                                let x_in_content = pos.x - content_rect.left() + scroll;
                                let secs = (x_in_content as f64 / zoom as f64).max(0.0);
                                RationalTime::from_secs_f64(secs, fps.rate())
                            });
                            if let Some(target_time) = target_time {
                                if let Some(track) = state.sequence_mut().tracks.get_mut(from_track) {
                                    track.sort_clips();
                                }
                                // Alt override: temporarily treat as Overwrite when Alt is held
                                let alt_held = ui.input(|i| i.modifiers.alt);
                                let effective_mode = if alt_held {
                                    klipp_timeline::magnetic::EditMode::Overwrite
                                } else {
                                    state.edit_mode
                                };
                                let result = match effective_mode {
                                    klipp_timeline::magnetic::EditMode::Insert => {
                                        klipp_timeline::magnetic::MagneticEditor::move_primary(
                                            state.sequence_mut(), clip_id, target_time)
                                    }
                                    klipp_timeline::magnetic::EditMode::Overwrite => {
                                        // Remove clip from track first, then overwrite at target
                                        let clip_data = state.sequence().tracks.get(from_track)
                                            .and_then(|t| t.find_clip(clip_id)).cloned();
                                        if let Some(clip_data) = clip_data {
                                            // Remove the clip being dragged
                                            if let Some(track) = state.sequence_mut().tracks.get_mut(from_track) {
                                                track.clips.retain(|c| c.id != clip_id);
                                            }
                                            klipp_timeline::magnetic::MagneticEditor::overwrite_primary(
                                                state.sequence_mut(), clip_data, target_time)
                                        } else {
                                            klipp_timeline::magnetic::MagneticEditor::move_primary(
                                                state.sequence_mut(), clip_id, target_time)
                                        }
                                    }
                                };
                                state.history.push(crate::history::HistoryEntry { sequence_idx: state.active_sequence_idx, command: crate::history::EditCommand::Magnetic(result.command) });
                                state.dirty = true;
                                cmds.push(AppCommand::SequenceUpdated);
                            }
                        } else {
                            // Legacy: free move
                            if let Some(track) = state.sequence().tracks.get(from_track) {
                                if let Some(clip) = track.find_clip(clip_id) {
                                    cmds.push(AppCommand::MoveClip {
                                        clip_id,
                                        new_timeline_in: clip.timeline_in,
                                    });
                                }
                            }

                            // Auto-connect/disconnect on drop for non-primary tracks
                            let primary_rect = self.track_rects.iter()
                                .find(|(idx, _, _)| state.sequence().tracks.get(*idx)
                                    .is_some_and(|t| t.magnetic_role == klipp_timeline::TrackRole::Primary))
                                .map(|(pi, _, r)| (*pi, *r));
                            if let (Some(pos), Some((primary_ti, primary_r))) = (pointer_pos, primary_rect) {
                                let dist_px = (pos.y - primary_r.center().y).abs();
                                let track_h = track_rect.height().max(1.0);
                                let dist_tracks = dist_px / track_h;

                                let clip_is_connected = state.sequence().tracks.get(from_track)
                                    .and_then(|t| t.find_clip(clip_id))
                                    .is_some_and(|c| c.magnetic_anchor.is_some());

                                if dist_tracks < klipp_timeline::magnetic::AUTO_CONNECT_TRACK_DISTANCE && !clip_is_connected {
                                    // Auto-connect: find nearest primary clip
                                    let clip_in = state.sequence().tracks.get(from_track)
                                        .and_then(|t| t.find_clip(clip_id))
                                        .map(|c| c.timeline_in);
                                    if let Some(clip_in) = clip_in {
                                        let anchor_id = state.sequence().tracks[primary_ti].clips.iter()
                                            .min_by_key(|c| {
                                                let mid = c.timeline_in + (c.timeline_out - c.timeline_in);
                                                let diff = mid - clip_in;
                                                diff.value.unsigned_abs()
                                            })
                                            .map(|c| c.id);
                                        if let Some(anchor_id) = anchor_id {
                                            let result = klipp_timeline::magnetic::MagneticEditor::connect_clip(
                                                state.sequence_mut(), clip_id, anchor_id,
                                                klipp_timeline::AnchorAlignment::StartAligned);
                                            state.history.push(crate::history::HistoryEntry {
                                                sequence_idx: state.active_sequence_idx,
                                                command: crate::history::EditCommand::Magnetic(result.command),
                                            });
                                            state.dirty = true;
                                            cmds.push(AppCommand::SequenceUpdated);
                                        }
                                    }
                                } else if dist_tracks > klipp_timeline::magnetic::AUTO_DISCONNECT_TRACK_DISTANCE && clip_is_connected {
                                    // Auto-disconnect
                                    let result = klipp_timeline::magnetic::MagneticEditor::disconnect_clip(
                                        state.sequence_mut(), clip_id);
                                    state.history.push(crate::history::HistoryEntry {
                                        sequence_idx: state.active_sequence_idx,
                                        command: crate::history::EditCommand::Magnetic(result.command),
                                    });
                                    state.dirty = true;
                                    cmds.push(AppCommand::SequenceUpdated);
                                }
                            }
                        }
                    }
                } else if state.auto_create_tracks {
                    // Dropped outside all tracks — auto-create a matching track
                    if let Some(pos) = pointer_pos {
                        let source_kind = state.sequence().tracks.get(from_track).map(|t| t.kind);
                        let first_rect_top = self.track_rects.first().map(|(_, _, r)| r.top());
                        let last_rect_bottom = self.track_rects.last().map(|(_, _, r)| r.bottom());
                        let above_all = first_rect_top.is_some_and(|top| pos.y < top);
                        let below_all = last_rect_bottom.is_some_and(|bot| pos.y > bot);

                        if let Some(kind) = source_kind {
                            let should_create = match kind {
                                TrackKind::Video | TrackKind::Text | TrackKind::Subtitle => above_all || below_all,
                                TrackKind::Audio => below_all || above_all,
                            };
                            if should_create {
                                let name = state.sequence().next_track_name(kind);
                                let new_track_idx = match kind {
                                    TrackKind::Video => {
                                        state.sequence_mut().add_video_track(name);
                                        // Find the newly added video track
                                        state.sequence().tracks.iter()
                                            .rposition(|t| t.kind == TrackKind::Video)
                                            .unwrap_or(0)
                                    }
                                    TrackKind::Audio => {
                                        state.sequence_mut().add_audio_track(name);
                                        state.sequence().tracks.len() - 1
                                    }
                                    TrackKind::Text => {
                                        state.sequence_mut().add_text_track(name);
                                        state.sequence().tracks.iter()
                                            .rposition(|t| t.kind == TrackKind::Text)
                                            .unwrap_or(0)
                                    }
                                    TrackKind::Subtitle => {
                                        state.sequence_mut().add_subtitle_track(name);
                                        state.sequence().tracks.len() - 1
                                    }
                                };
                                // Move clip to the new track
                                if let Some(track) = state.sequence().tracks.get(from_track) {
                                    if let Some(clip) = track.find_clip(clip_id) {
                                        cmds.push(AppCommand::MoveClipToTrack {
                                            clip_id,
                                            new_timeline_in: clip.timeline_in,
                                            from_track,
                                            to_track: new_track_idx,
                                        });
                                    }
                                }
                                state.dirty = true;
                                cmds.push(AppCommand::SequenceUpdated);
                                tracing::info!("Auto-created {:?} track for clip drag outside timeline", kind);
                            }
                        }
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
            egui::FontId::proportional(8.0), egui::Color32::from_rgb(220, 220, 230));
        if fit_resp.clicked() {
            let dur_secs = state.sequence_duration().to_secs_f64() as f32;
            if dur_secs > 0.0 {
                let content_width = ruler_rect.width() - 20.0;
                // Allow zoom down to 0.1 px/sec for long clips (e.g. 12 min movie)
                state.timeline_zoom = (content_width / dur_secs).clamp(0.1, 2000.0);
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
        Icon::Minus.bold(12.0).tint(egui::Color32::from_rgb(220, 220, 230)).paint_at(ui, minus_rect.shrink(2.0));
        if minus_resp.clicked() {
            state.timeline_zoom = (state.timeline_zoom / 1.5).clamp(0.1, 2000.0);
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
        Icon::Plus.bold(12.0).tint(egui::Color32::from_rgb(220, 220, 230)).paint_at(ui, plus_rect.shrink(2.0));
        if plus_resp.clicked() {
            state.timeline_zoom = (state.timeline_zoom * 1.5).clamp(0.1, 2000.0);
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
                cmds.push(AppCommand::ImportMediaLinked { media_id: item.id });
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

        // Proxy toggle
        let proxy_w = 40.0;
        let proxy_rect = egui::Rect::from_min_size(
            egui::pos2(x + (btn_w + 2.0) * 2.0 + follow_w + 4.0, y_offset + 2.0),
            egui::vec2(proxy_w, btn_h),
        );
        let proxy_resp = ui.interact(proxy_rect, ui.id().with("proxy_toggle"), egui::Sense::click());
        let proxy_col = if state.use_proxies {
            egui::Color32::from_rgb(180, 140, 30)
        } else {
            egui::Color32::from_rgb(40, 40, 48)
        };
        painter.rect_filled(proxy_rect, 2.0, proxy_col);
        painter.text(proxy_rect.center(), egui::Align2::CENTER_CENTER, "Proxy",
            egui::FontId::proportional(8.0), egui::Color32::WHITE);
        if proxy_resp.clicked() {
            cmds.push(AppCommand::SetUseProxies(!state.use_proxies));
        }
        proxy_resp.on_hover_text("Use proxy files for playback (lower quality, faster decode)");
    }

    fn draw_overview_bar(
        &self,
        painter: &egui::Painter,
        rect: &egui::Rect,
        state: &AppState,
    ) {
        // Background
        painter.rect_filled(*rect, 0.0, egui::Color32::from_rgb(16, 16, 20));
        // Bottom border
        painter.line_segment(
            [
                egui::pos2(rect.left(), rect.bottom()),
                egui::pos2(rect.right(), rect.bottom()),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 60)),
        );

        // Compute total sequence duration
        let dur_secs = state.sequence().total_duration().to_secs_f64() as f32;
        if dur_secs <= 0.0 {
            return;
        }

        let bar_width = rect.width();
        let px_per_sec = bar_width / dur_secs;

        // Viewport rectangle bounds (needed for dimming)
        let zoom = state.timeline_zoom;
        let scroll = state.timeline_scroll_x;
        let content_width = state.last_timeline_content_width;
        let view_start_secs = scroll / zoom;
        let view_end_secs = (scroll + content_width) / zoom;
        let vp_x0 = (rect.left() + view_start_secs * px_per_sec).max(rect.left());
        let vp_x1 = (rect.left() + view_end_secs * px_per_sec).min(rect.right());

        // Draw clips as tiny colored rectangles with lane gaps
        let track_count = state.sequence().tracks.len().max(1) as f32;
        let lane_gap = 1.0_f32;
        let total_gap = lane_gap * (track_count - 1.0).max(0.0);
        let lane_h = ((rect.height() - 4.0 - total_gap) / track_count).max(2.0);

        for (track_idx, track) in state.sequence().tracks.iter().enumerate() {
            let lane_y = rect.top() + 2.0 + track_idx as f32 * (lane_h + lane_gap);

            for clip in &track.clips {
                let x0 = rect.left() + clip.timeline_in.to_secs_f64() as f32 * px_per_sec;
                let x1 = rect.left() + clip.timeline_out.to_secs_f64() as f32 * px_per_sec;
                if x1 < rect.left() || x0 > rect.right() {
                    continue;
                }
                let cx0 = x0.max(rect.left());
                let cx1 = x1.min(rect.right());
                // Minimum 2px width so tiny clips are visible
                let cx1 = cx1.max(cx0 + 2.0);
                let clip_rect = egui::Rect::from_min_max(
                    egui::pos2(cx0, lane_y),
                    egui::pos2(cx1, (lane_y + lane_h).min(rect.bottom() - 2.0)),
                );
                let (r, g, b) = CLIP_COLORS[track_idx % CLIP_COLORS.len()];
                // Brighter fill inside viewport, dimmer outside
                let in_viewport = cx1 >= vp_x0 && cx0 <= vp_x1;
                let color = if in_viewport {
                    egui::Color32::from_rgb(r.saturating_add(20), g.saturating_add(20), b.saturating_add(20))
                } else {
                    egui::Color32::from_rgb(r / 2, g / 2, b / 2)
                };
                painter.rect_filled(clip_rect, 1.0, color);
                // Subtle outline
                painter.rect_stroke(
                    clip_rect,
                    1.0,
                    egui::Stroke::new(0.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 25)),
                );
            }
        }

        // Dim areas outside viewport
        let dim = egui::Color32::from_rgba_premultiplied(0, 0, 0, 100);
        if vp_x0 > rect.left() {
            painter.rect_filled(
                egui::Rect::from_min_max(rect.min, egui::pos2(vp_x0, rect.bottom())),
                0.0,
                dim,
            );
        }
        if vp_x1 < rect.right() {
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(vp_x1, rect.top()), rect.max),
                0.0,
                dim,
            );
        }

        // Viewport border
        let vp_rect = egui::Rect::from_min_max(
            egui::pos2(vp_x0, rect.top()),
            egui::pos2(vp_x1, rect.bottom()),
        );
        painter.rect_stroke(
            vp_rect,
            0.0,
            egui::Stroke::new(1.5, egui::Color32::from_rgba_premultiplied(255, 255, 255, 120)),
        );

        // Draw playhead as thin red vertical line
        let playhead_secs = state.playhead.to_secs_f64() as f32;
        let ph_x = rect.left() + playhead_secs * px_per_sec;
        if ph_x >= rect.left() && ph_x <= rect.right() {
            painter.line_segment(
                [egui::pos2(ph_x, rect.top()), egui::pos2(ph_x, rect.bottom())],
                egui::Stroke::new(1.5, egui::Color32::from_rgb(220, 50, 50)),
            );
        }

        // (interaction handled in show() to avoid z-order conflicts)
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

        // Choose a "nice" major interval in seconds — covers frame-level to multi-hour
        let major_interval = if min_label_interval_secs > 3600.0 { 3600.0 }  // 1h
            else if min_label_interval_secs > 1800.0 { 1800.0 }  // 30m
            else if min_label_interval_secs > 600.0 { 600.0 }    // 10m
            else if min_label_interval_secs > 300.0 { 300.0 }    // 5m
            else if min_label_interval_secs > 120.0 { 120.0 }    // 2m
            else if min_label_interval_secs > 60.0 { 60.0 }      // 1m
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
                        let total_secs = t.round() as i64;
                        let hh = total_secs / 3600;
                        let mm = (total_secs / 60) % 60;
                        let ss = total_secs % 60;
                        if major_interval >= 3600.0 {
                            format!("{hh}h{mm:02}m")
                        } else if major_interval >= 60.0 {
                            if hh > 0 {
                                format!("{hh}:{mm:02}:{ss:02}")
                            } else {
                                format!("{mm}:{ss:02}")
                            }
                        } else if major_interval <= 1.0 {
                            let total_ms = (t * 1000.0) as i64;
                            let ms = total_ms % 1000;
                            format!("{mm}:{ss:02}.{ms:03}")
                        } else if major_interval <= 5.0 {
                            let total_ms = (t * 1000.0) as i64;
                            let tenths = (total_ms % 1000) / 100;
                            if tenths > 0 {
                                format!("{mm}:{ss:02}.{tenths}")
                            } else {
                                format!("{mm}:{ss:02}")
                            }
                        } else if hh > 0 {
                            format!("{hh}:{mm:02}:{ss:02}")
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
        let minor_interval = if major_interval >= 600.0 { major_interval / 5.0 }   // 10m+ → 5 subdivisions
            else if major_interval >= 60.0 { major_interval / 6.0 }   // 1m+ → 10s subdivisions
            else if major_interval >= 10.0 { major_interval / 5.0 }
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
