pub mod toolbar;
pub mod track_headers;
pub mod ruler;
pub mod playhead;
pub mod skimmer;
pub mod sequence_tabs;
pub mod time_utils;
pub mod range_visuals;
pub mod grid_snap;
pub mod camera_lane;
pub mod mask_lane;
pub mod markers;

use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// New timeline panel that wraps the existing TimelinePanel
/// with the redesigned visual shell (toolbar, track headers, ruler).
///
/// Phase 7A: Structure + track headers + ruler (new visual shell)
/// Phase 7B: Clip rendering delegated to existing panels/timeline.rs
/// Phase 7C: Full migration of interaction logic
#[derive(Default)]
pub struct NewTimelinePanel {
    last_adaptive_fit: Option<(u32, u32, u32, u32)>,
}

impl NewTimelinePanel {
    /// The timeline, or on the Colour page the strip of shots. There it
    /// only selects, whatever tool the Edit page left armed: the tool
    /// was shared and nothing on the Colour page showed it, so with the
    /// Blade armed a click to pick a shot to grade cut it (#323). No
    /// trims, fades or moves start there either. The Edit page's tool is
    /// left as it was, unless a shortcut picked another one meanwhile.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        old_timeline: &mut crate::panels::timeline::TimelinePanel,
        kb: &crate::keybindings::KeyBindings,
        adaptive_fit: bool,
    ) -> Vec<AppCommand> {
        let strip = state.show_color_panel;
        old_timeline.select_only = strip;
        let edit_tool = state.active_tool;
        if strip {
            state.active_tool = klipp_core::Tool::Select;
        }
        let cmds = self.show_timeline(ui, state, theme, old_timeline, kb, adaptive_fit);
        if strip && state.active_tool == klipp_core::Tool::Select {
            state.active_tool = edit_tool;
        }
        cmds
    }

    fn show_timeline(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        old_timeline: &mut crate::panels::timeline::TimelinePanel,
        kb: &crate::keybindings::KeyBindings,
        adaptive_fit: bool,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let full_rect = ui.max_rect();

        // In the Color workspace the timeline is a strip for moving
        // between shots: no toolbar, no sequence tabs.
        let strip = state.show_color_panel;

        // ── Timeline toolbar (34px) ──
        let toolbar_h = if strip { 0.0 } else { 34.0 };
        let toolbar_rect = egui::Rect::from_min_size(
            full_rect.min,
            egui::vec2(full_rect.width(), toolbar_h),
        );
        if !strip {
            crate::ui_util::allocate_clipped_ui(ui, toolbar_rect, |ui| {
                ui.painter().rect_filled(toolbar_rect, 0.0, theme.bg_panel);
                ui.painter().hline(
                    toolbar_rect.x_range(),
                    toolbar_rect.max.y,
                    egui::Stroke::new(1.0_f32, theme.border),
                );
                toolbar::show(ui, state, theme, &mut cmds, kb);
            });
        }

        // ── Sequence tabs (24px) ──
        let tabs_h = if strip { 0.0 } else { sequence_tabs::HEIGHT };
        let tabs_rect = egui::Rect::from_min_size(
            egui::pos2(full_rect.min.x, toolbar_rect.max.y),
            egui::vec2(full_rect.width(), tabs_h),
        );
        if !strip {
            crate::ui_util::allocate_clipped_ui(ui, tabs_rect, |ui| {
                let tab_cmds = sequence_tabs::show(ui, state, theme);
                cmds.extend(tab_cmds);
            });
        }

        // ── The selects strip, stacked above the cut (#82) ──
        let strip_h = if strip || state.selects_sequence.is_none() {
            0.0
        } else {
            state.selects_height.clamp(60.0, (full_rect.height() - tabs_rect.max.y + full_rect.min.y - 80.0).max(60.0))
        };
        if strip_h > 0.0 {
            let strip_rect = egui::Rect::from_min_size(egui::pos2(full_rect.min.x, tabs_rect.max.y), egui::vec2(full_rect.width(), strip_h));
            crate::ui_util::allocate_clipped_ui(ui, strip_rect, |ui| {
                crate::panels::selects_strip::show(ui, state, theme, strip_rect, &mut cmds);
            });
            // Its foot drags to resize it.
            let grip = egui::Rect::from_min_size(egui::pos2(strip_rect.min.x, strip_rect.max.y - 3.0), egui::vec2(strip_rect.width(), 6.0));
            let resize = ui.interact(grip, ui.id().with("selects_resize"), egui::Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeVertical);
            if resize.dragged() {
                state.selects_height = (state.selects_height + resize.drag_delta().y).clamp(60.0, 400.0);
            }
        }

        // Content area below tabs (and the selects strip)
        let content_rect = egui::Rect::from_min_max(
            egui::pos2(full_rect.min.x, tabs_rect.max.y + strip_h),
            full_rect.max,
        );

        // ── Track headers (resizable) ──
        let header_w = state.track_header_width;
        let header_rect = egui::Rect::from_min_size(
            content_rect.min,
            egui::vec2(header_w, content_rect.height()),
        );

        // ── Resize handle between headers and timeline ──
        let handle_rect = egui::Rect::from_min_size(
            egui::pos2(header_rect.max.x - 4.0, content_rect.min.y),
            egui::vec2(8.0, content_rect.height()),
        );
        let handle_resp = ui.interact(handle_rect, ui.id().with("track_header_resize"), egui::Sense::drag());
        if handle_resp.dragged() {
            state.track_header_width = (state.track_header_width + handle_resp.drag_delta().x)
                .clamp(100.0, 320.0);
        }
        if handle_resp.hovered() || handle_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        // ── Timeline content (right of headers) ──
        let timeline_content_rect = egui::Rect::from_min_max(
            egui::pos2(content_rect.min.x + header_w, content_rect.min.y),
            content_rect.max,
        );

        // Draw track headers
        // allocate_clipped_ui sets clip_rect = header_rect for us, so scrolled-out
        // headers cannot paint into the ruler/status bar/clip area.
        crate::ui_util::allocate_clipped_ui(ui, header_rect, |ui| {
            ui.painter().rect_filled(header_rect, 0.0, theme.bg_deep);
            ui.painter().vline(
                header_rect.max.x,
                header_rect.y_range(),
                egui::Stroke::new(1.0_f32, theme.border),
            );
            let header_cmds = track_headers::show(ui, state, theme, header_rect);
            cmds.extend(header_cmds);
        });

        // ── New adaptive ruler (22px at top of content area) ──
        let ruler_h = crate::theme::RULER_HEIGHT;
        let ruler_rect = egui::Rect::from_min_size(
            timeline_content_rect.min,
            egui::vec2(timeline_content_rect.width(), ruler_h),
        );
        // Use the existing timeline_zoom (already pps) for compatibility
        let pps = state.timeline_zoom;
        let scroll_x = state.timeline_scroll_x;
        let duration_secs = state.sequence_duration().to_secs_f64();

        // The camera's lane sits under the ruler, above the tracks, when shown.
        let lane_h = camera_lane::height(state);
        let lane_rect = egui::Rect::from_min_size(
            egui::pos2(timeline_content_rect.min.x, timeline_content_rect.min.y + ruler_h),
            egui::vec2(timeline_content_rect.width(), lane_h),
        );

        // Compute tracks_rect early — needed by both range visuals and clip delegation
        let tracks_rect = egui::Rect::from_min_max(
            egui::pos2(timeline_content_rect.min.x, timeline_content_rect.min.y + ruler_h + lane_h),
            timeline_content_rect.max,
        );

        // Recompute both timeline axes only when the available viewport or
        // content footprint changes. This preserves manual interaction while
        // keeping the opt-in adaptive mode responsive to resize and layout changes.
        if adaptive_fit {
            let duration_secs = state.sequence_duration().to_secs_f64() as f32;
            let total_track_h: f32 = state.sequence().tracks.iter()
                .map(crate::panels::timeline::track_row_height)
                .sum();
            let signature = (
                tracks_rect.width().to_bits(),
                tracks_rect.height().to_bits(),
                duration_secs.to_bits(),
                total_track_h.to_bits(),
            );
            if self.last_adaptive_fit != Some(signature) {
                let raw_zoom = if duration_secs > 0.01 {
                    tracks_rect.width() / (duration_secs * 1.05)
                } else { 0.0 };
                state.timeline_zoom = raw_zoom.clamp(0.1, 2000.0);
                state.timeline_scroll_x = 0.0;
                state.timeline_track_scale = crate::panels::timeline::fit_track_scale(
                    total_track_h,
                    tracks_rect.height(),
                );
                state.timeline_scroll_y = 0.0;
                self.last_adaptive_fit = Some(signature);
            }
        } else {
            self.last_adaptive_fit = None;
        }

        let ruler_painter = ui.painter().with_clip_rect(ruler_rect);
        ruler::draw_ruler(&ruler_painter, ruler_rect, pps, scroll_x, duration_secs, theme);
        ruler::draw_render_status(&ruler_painter, ruler_rect, pps, scroll_x, &state.render_status, f64::from(state.fps.rate()), theme);

        // Draw range selection visuals (ruler fill + handles + lollipops + labels)
        // Clip handles/lollipops to the full timeline area (ruler + tracks)
        let timeline_clip_rect = egui::Rect::from_min_max(
            ruler_rect.min,
            tracks_rect.max,
        );
        let timeline_painter = ui.painter().with_clip_rect(timeline_clip_rect);
        let loop_pulsing = state.loop_playback_enabled
            && state.playback == klipp_engine::PlaybackState::Playing;
        range_visuals::draw_range(
            &ruler_painter,
            &timeline_painter,
            ruler_rect,
            tracks_rect.bottom(),
            state,
            theme,
            pps,
            scroll_x,
            loop_pulsing,
        );
        // Request repaint during loop pulse animation for smooth sine wave
        if loop_pulsing {
            ui.ctx().request_repaint();
        }

        // ── Ruler interaction: seek, range drag, handle drag ──
        let ruler_resp = ui.interact(ruler_rect, ui.id().with("new_ruler_seek"), egui::Sense::click_and_drag());
        // Sequence markers, over the ruler so their flags take the clicks
        // and drags on them (#909).
        markers::show(ui, ruler_rect, state, pps, scroll_x, &mut cmds);
        // The ruler's right-click menu: snapping, grid, a marker or In/Out
        // here, zoom to fit. It was only on the ruler the app no longer
        // draws (#912).
        old_timeline.show_ruler_context_menu(state, &mut cmds, &ruler_resp, &ruler_rect);

        // Clear snap line each frame
        state.range_snap_line_x = None;

        // Compute handle x positions for hit testing
        let handle_in_x = state.sequence().in_point
            .map(|t| ruler_rect.left() + time_utils::time_to_x(t.to_secs_f64(), pps, scroll_x));
        let handle_out_x = state.sequence().out_point
            .map(|t| ruler_rect.left() + time_utils::time_to_x(t.to_secs_f64(), pps, scroll_x));

        // Helper: convert mouse x on ruler to clamped RationalTime
        // Rebind fps_rate locally so the closure doesn't borrow `state` —
        // otherwise the push_history call below would clash with the
        // closure's still-live capture.
        let fps_rate = state.fps.rate();
        let mouse_to_time = |mouse_x: f32| -> klipp_core::RationalTime {
            let secs = time_utils::x_to_time(mouse_x - ruler_rect.left(), pps, scroll_x);
            let clamped = secs.max(0.0).min(duration_secs);
            klipp_core::RationalTime::from_secs_f64(clamped, fps_rate)
        };

        // Hover cursor: ResizeHorizontal near handles
        let mut handle_hovered = false;
        if let Some(pos) = ui.input(|i| i.pointer.hover_pos()) {
            if ruler_rect.contains(pos)
                && state.range_drag.is_none()
                && range_visuals::hit_test_handle(pos.x, handle_in_x, handle_out_x).is_some()
            {
                handle_hovered = true;
            }
        }
        if handle_hovered || state.range_drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }

        // On drag_started: determine if handle drag or ruler range drag
        if ruler_resp.drag_started() {
            if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                if let Some(kind) = range_visuals::hit_test_handle(pos.x, handle_in_x, handle_out_x) {
                    // Start handle drag
                    state.range_drag = Some(crate::state::RangeDragState {
                        kind,
                        original_in: state.sequence().in_point,
                        original_out: state.sequence().out_point,
                    });
                } else {
                    // Empty ruler press — seek the playhead IMMEDIATELY at
                    // press time. Without this, egui's click_and_drag
                    // categorises taps with any small mouse drift as
                    // a drag (not a click), and `.clicked()` never fires,
                    // so single-tap-to-seek silently drops. Seeking on
                    // press gives instant feedback; if the user then
                    // moves > 4px the dragged branch promotes this to a
                    // range create. Mirrors the OLD timeline's
                    // scrub-on-press behaviour (panels/timeline.rs:3654).
                    let tc = mouse_to_time(pos.x);
                    let prev_playhead = state.playhead;
                    state.playhead = tc;
                    state.playhead_smooth_secs = tc.to_secs_f64();
                    state.playhead_anchor = tc;
                    state.playhead_anchor_wall = std::time::Instant::now();
                    cmds.push(AppCommand::SeekTo(tc));
                    tracing::info!(
                        "PLAYHEAD_DIAG event=ruler_press pos_x={:.1} ruler_left={:.1} prev={:.3}s new={:.3}s",
                        pos.x, ruler_rect.left(), prev_playhead.to_secs_f64(), tc.to_secs_f64(),
                    );
                    state.ruler_press_origin = Some(pos);
                }
            }
        }

        // On dragged: handle drag logic or threshold-based ruler range drag
        if ruler_resp.dragged() {
            if let Some(pos) = ui.input(|i| i.pointer.latest_pos()) {
                let snap_threshold = 10.0 / pps as f64;
                let snap_targets = range_visuals::collect_snap_targets(state);
                // Resolve the user's grid-snap interval at current zoom.
                // None when GridInterval::Off (back-compat default) — the
                // post-pass in find_nearest_snap is then a no-op.
                let grid_interval_secs = grid_snap::resolve_grid_interval_secs(
                    state.grid_snap_interval,
                    state.fps.rate(),
                    pps,
                );

                if let Some(ref drag) = state.range_drag {
                    // Active drag — update in/out points
                    let raw_tc = mouse_to_time(pos.x);
                    let raw_secs = raw_tc.to_secs_f64();

                    // Apply snap (master gate; per-target gating
                    // happens inside find_nearest_snap via SnapConfig).
                    let (snapped_tc, snapped_secs) = if state.snap.master {
                        range_visuals::find_nearest_snap(raw_secs, &snap_targets, snap_threshold, grid_interval_secs, state.fps.rate(), Some(&state.snap))
                            .map(|(tc, secs, _kind)| (tc, secs))
                            .unwrap_or((raw_tc, raw_secs))
                    } else {
                        (raw_tc, raw_secs)
                    };

                    // Show snap line if snapped
                    if state.snap.master && (snapped_secs - raw_secs).abs() > 0.0001 {
                        let snap_x = ruler_rect.left() + time_utils::time_to_x(snapped_secs, pps, scroll_x);
                        state.range_snap_line_x = Some(snap_x);
                    }

                    match drag.kind {
                        crate::state::RangeDragKind::InHandle => {
                            // Clamp: in can't pass out
                            let ok = state.sequence().out_point
                                .map(|out| snapped_tc < out)
                                .unwrap_or(true);
                            if ok {
                                cmds.push(AppCommand::SetSequenceInPoint(snapped_tc));
                            }
                        }
                        crate::state::RangeDragKind::OutHandle => {
                            // Clamp: out can't pass in
                            let ok = state.sequence().in_point
                                .map(|inp| snapped_tc > inp)
                                .unwrap_or(true);
                            if ok {
                                cmds.push(AppCommand::SetSequenceOutPoint(snapped_tc));
                            }
                        }
                        crate::state::RangeDragKind::RulerCreate { anchor_secs } => {
                            let anchor_tc = klipp_core::RationalTime::from_secs_f64(anchor_secs, state.fps.rate());
                            if snapped_secs < anchor_secs {
                                cmds.push(AppCommand::SetSequenceInPoint(snapped_tc));
                                cmds.push(AppCommand::SetSequenceOutPoint(anchor_tc));
                            } else {
                                cmds.push(AppCommand::SetSequenceInPoint(anchor_tc));
                                cmds.push(AppCommand::SetSequenceOutPoint(snapped_tc));
                            }
                        }
                    }
                } else if let Some(origin) = state.ruler_press_origin {
                    // Check 4px drag threshold
                    let dist = (pos.x - origin.x).abs();
                    if dist > 4.0 {
                        // Decide: extend existing range edge vs create new range
                        let anchor_secs = time_utils::x_to_time(
                            origin.x - ruler_rect.left(), pps, scroll_x,
                        ).max(0.0).min(duration_secs);
                        let drag_kind = if let (Some(in_pt), Some(out_pt)) =
                            (state.sequence().in_point, state.sequence().out_point)
                        {
                            let in_secs = in_pt.to_secs_f64();
                            let out_secs = out_pt.to_secs_f64();
                            let range_dur = (out_secs - in_secs).max(0.01);
                            // "Near" = within 20% of range duration from either edge
                            let margin = range_dur * 0.20;
                            let near_in = anchor_secs >= in_secs - margin && anchor_secs <= in_secs + margin;
                            let near_out = anchor_secs >= out_secs - margin && anchor_secs <= out_secs + margin;
                            let inside = anchor_secs > in_secs + margin && anchor_secs < out_secs - margin;

                            if near_in {
                                crate::state::RangeDragKind::InHandle
                            } else if near_out {
                                crate::state::RangeDragKind::OutHandle
                            } else if inside {
                                // Click inside range but not near edges → extend nearest edge
                                let mid = (in_secs + out_secs) / 2.0;
                                if anchor_secs <= mid {
                                    crate::state::RangeDragKind::InHandle
                                } else {
                                    crate::state::RangeDragKind::OutHandle
                                }
                            } else {
                                // Click far outside range → create new range
                                crate::state::RangeDragKind::RulerCreate { anchor_secs }
                            }
                        } else {
                            crate::state::RangeDragKind::RulerCreate { anchor_secs }
                        };
                        state.range_drag = Some(crate::state::RangeDragState {
                            kind: drag_kind,
                            original_in: state.sequence().in_point,
                            original_out: state.sequence().out_point,
                        });
                        state.ruler_press_origin = None;
                    }
                }
            }
        }

        // On drag_stopped: push undo entry if values changed
        if ruler_resp.drag_stopped() {
            if let Some(drag) = state.range_drag.take() {
                let (new_in, new_out) = {
                    let seq = state.sequence();
                    (seq.in_point, seq.out_point)
                };
                if drag.original_in != new_in || drag.original_out != new_out {
                    state.push_history(crate::history::EditCommand::SetRange {
                        old_in: drag.original_in,
                        old_out: drag.original_out,
                        new_in,
                        new_out,
                    });
                }
            }
            state.ruler_press_origin = None;
        }

        // Click (no drag): seek playhead.
        // Optimistic UI update — move the playhead display instantly
        // so the user sees the ruler react at click-time, not after
        // the engine's decoder has finished seeking (which can take
        // 500 ms – 2 s on a 4K H.264 clip while it forward-decodes
        // from the nearest keyframe). The engine will confirm the
        // same position via `PlayheadMoved` when the seek completes;
        // that arrives at the same value we just set, so no visible
        // jump. Playback is already stopped at click-time because a
        // clicked ruler implies a paused seek, not live playback.
        if ruler_resp.clicked() && state.range_drag.is_none() {
            // Use the response's own press position (interact_pointer_pos)
            // — it's only Some when this widget actually owns the press,
            // so a release that drifted onto the ruler from a drag
            // elsewhere can't fire here. See feedback_egui_drag_phantom.
            // Previous attempt used Pointer::interact_pos() (input-level)
            // which returns None after release, dropping legitimate
            // clicks.
            if let Some(pos) = ruler_resp.interact_pointer_pos() {
                let tc = mouse_to_time(pos.x);
                let prev_playhead = state.playhead;
                state.playhead = tc;
                state.playhead_smooth_secs = tc.to_secs_f64();
                state.playhead_anchor = tc;
                state.playhead_anchor_wall = std::time::Instant::now();
                cmds.push(AppCommand::SeekTo(tc));
                tracing::info!(
                    "PLAYHEAD_DIAG event=ruler_click pos_x={:.1} ruler_left={:.1} prev={:.3}s new={:.3}s",
                    pos.x, ruler_rect.left(), prev_playhead.to_secs_f64(), tc.to_secs_f64(),
                );
            }
        }

        // Double-click: clear range
        if ruler_resp.double_clicked() {
            cmds.push(AppCommand::ClearSequenceInPoint);
            cmds.push(AppCommand::ClearSequenceOutPoint);
        }

        // Draw snap line during range drag
        if let Some(snap_x) = state.range_snap_line_x {
            let parent_painter = ui.painter();
            let snap_secs = time_utils::x_to_time(snap_x - ruler_rect.left(), pps, scroll_x);
            range_visuals::draw_snap_line(
                parent_painter, snap_x, ruler_rect.top(), tracks_rect.bottom(),
                snap_secs, state.timecode_display(), theme,
            );
        }
        if lane_h > 0.0 {
            crate::ui_util::allocate_clipped_ui(ui, lane_rect, |ui| {
                camera_lane::show(ui, state, theme, lane_rect, pps, scroll_x, &mut cmds);
            });
        }

        // allocate_clipped_ui sets clip_rect = tracks_rect, preventing the old
        // timeline panel from bleeding into the inspector or status bar.
        // The Text tool works in the viewer; on the tracks it selects, as
        // Select does (#1713). Left armed unless the tracks picked another.
        let text_tool = state.active_tool == klipp_core::Tool::Text;
        if text_tool {
            state.active_tool = klipp_core::Tool::Select;
        }
        crate::ui_util::allocate_clipped_ui(ui, tracks_rect, |ui| {
            let old_cmds = old_timeline.show(ui, state);
            cmds.extend(old_cmds);
        });
        if text_tool && state.active_tool == klipp_core::Tool::Select {
            state.active_tool = klipp_core::Tool::Text;
        }
        markers::draw_lines(ui, tracks_rect, ruler_rect.left(), state, pps, scroll_x);
        // A trim amount being typed for the selected edit points (#913).
        // A dynamic trim under way (#914): how far, and the keys.
        if let (Some(cut), None) = (state.dynamic_trim, &state.trim_entry) {
            let fps = state.sequence().fps.as_f64();
            let frames = ((state.playhead.to_secs_f64() - cut.to_secs_f64()) * fps).round() as i64;
            let text = format!("Dynamic trim  {frames:+} frames \u{2014} K to commit, Esc to cancel");
            let font = egui::FontId::monospace(12.0);
            let galley = ui.painter().layout_no_wrap(text, font, theme.text_primary);
            let at = egui::pos2(tracks_rect.left() + 12.0, tracks_rect.bottom() - galley.size().y - 14.0);
            let bg = egui::Rect::from_min_size(at, galley.size()).expand(6.0);
            let painter = ui.painter().with_clip_rect(tracks_rect);
            painter.rect_filled(bg, 4.0, theme.bg_raised);
            painter.rect_stroke(bg, 4.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(120, 220, 120)));
            painter.galley(at, galley, theme.text_primary);
        }
        if let Some(typed) = &state.trim_entry {
            let what = if state.selected_edits.is_empty() { "Move" } else { "Trim" };
            let text = format!("{what}  {typed}   frames, or s:ff \u{2014} Enter to {}, Esc to cancel", what.to_lowercase());
            let font = egui::FontId::monospace(12.0);
            let galley = ui.painter().layout_no_wrap(text, font, theme.text_primary);
            let at = egui::pos2(tracks_rect.left() + 12.0, tracks_rect.bottom() - galley.size().y - 14.0);
            let bg = egui::Rect::from_min_size(at, galley.size()).expand(6.0);
            let painter = ui.painter().with_clip_rect(tracks_rect);
            painter.rect_filled(bg, 4.0, theme.bg_raised);
            painter.rect_stroke(bg, 4.0, egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(120, 220, 120)));
            painter.galley(at, galley, theme.text_primary);
        }

        // The menu on a camera key, opened on the lane or in the curve
        // editor, is drawn here once a frame.
        camera_lane::show_key_menu(ui, state, &mut cmds);

        cmds
    }
}

#[cfg(test)]
mod colour_strip_tests {
    use super::*;
    use klipp_core::{AppCommand, MediaId, RationalTime, Tool};
    use klipp_timeline::{Clip, TrackKind};

    /// Clicks over the whole strip, with the Blade armed on the Edit
    /// page: on the Colour page none of them cuts, and the Blade is
    /// still armed afterwards (#323).
    #[test]
    fn the_colour_strip_selects_whatever_tool_edit_left_armed() {
        let mut state = AppState::default();
        let v1 = state.sequences[0].tracks.iter().position(|t| matches!(t.kind, TrackKind::Video)).expect("V1");
        let long = RationalTime { value: 24 * 600, rate: 24 };
        let zero = RationalTime { value: 0, rate: 24 };
        state.sequences[0].tracks[v1].clips.push(Clip::media(MediaId::new(), zero, long, zero, long));
        state.show_color_panel = true;
        state.active_tool = Tool::Blade;

        let ctx = egui::Context::default();
        let theme = KlippTheme::default();
        let kb = crate::keybindings::KeyBindings::default();
        let mut panel = NewTimelinePanel::default();
        let mut old = crate::panels::timeline::TimelinePanel::default();
        old.embedded = true;
        let mut all = Vec::new();
        let mut time = 0.0;
        let mut frame = |events: Vec<egui::Event>, state: &mut AppState, all: &mut Vec<AppCommand>| {
            time += 0.5;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 300.0))),
                time: Some(time),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    all.extend(panel.show(ui, state, &theme, &mut old, &kb, false));
                });
            });
        };
        frame(Vec::new(), &mut state, &mut all);
        for y in (5..300).step_by(10) {
            for x in (60..780).step_by(60) {
                let at = egui::pos2(x as f32, y as f32);
                let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
                frame(vec![egui::Event::PointerMoved(at), button(true)], &mut state, &mut all);
                frame(vec![button(false)], &mut state, &mut all);
            }
        }
        assert!(
            !all.iter().any(|c| matches!(c, AppCommand::BladeClip { .. })),
            "a click on the Colour page cut a clip",
        );
        assert_eq!(state.active_tool, Tool::Blade, "the Edit page's tool was not left as it was");
        assert!(old.select_only);
    }
}

/// The menus the user opens on the timeline, drawn as the app draws them
/// (the embedded timeline, its track headers and clips) and opened with a
/// right-click: their items must be there. Tests of each item's handler
/// passed while the items were added to a header the app never draws,
/// and nothing said so (#846, found by hand in #859).
#[cfg(test)]
mod menu_smoke_tests {
    use super::*;
    use klipp_core::{AppCommand, MediaId, RationalTime};
    use klipp_timeline::{Clip, TrackKind};

    struct Timeline {
        ctx: egui::Context,
        state: AppState,
        panel: NewTimelinePanel,
        old: crate::panels::timeline::TimelinePanel,
        time: f64,
        /// What the last frame asked the app to do.
        sent: Vec<AppCommand>,
        /// The last frame's shapes.
        shapes: Vec<egui::epaint::ClippedShape>,
        /// The window's height.
        height: f32,
    }

    impl Timeline {
        /// The default sequence with a long clip on its first video track.
        fn new() -> Self {
            let mut state = AppState::default();
            let v1 = state.sequences[0].tracks.iter().position(|t| matches!(t.kind, TrackKind::Video)).expect("a video track");
            let (zero, long) = (RationalTime { value: 0, rate: 24 }, RationalTime { value: 24 * 600, rate: 24 });
            let mut item = klipp_media::MediaItem::synthetic_for_test("/tmp/smoke.mov".into(), None);
            item.duration = long;
            let media: MediaId = state.media_pool.add(item);
            state.sequences[0].tracks[v1].clips.push(Clip::media(media, zero, long, zero, long));
            let mut old = crate::panels::timeline::TimelinePanel::default();
            old.embedded = true;
            Self { ctx: egui::Context::default(), state, panel: NewTimelinePanel::default(), old, time: 0.0, sent: Vec::new(), shapes: Vec::new(), height: 600.0 }
        }

        /// One frame with `events`: the text drawn, with where.
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Pos2)> {
            self.time += 0.5;
            let input = egui::RawInput {
                // Tall enough for the whole clip menu, as any real window is.
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, self.height))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (theme, kb) = (KlippTheme::default(), crate::keybindings::KeyBindings::default());
            let (state, panel, old) = (&mut self.state, &mut self.panel, &mut self.old);
            let mut cmds: Vec<AppCommand> = Vec::new();
            let out = self.ctx.run(input, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    cmds = panel.show(ui, state, &theme, old, &kb, false);
                });
            });
            // What the app does between frames with a selection the
            // timeline asked for: a clip's menu shows the selected clip's.
            for cmd in &cmds {
                if let AppCommand::SelectClip(id) = cmd {
                    self.state.selected_clip = *id;
                }
            }
            self.sent = cmds;
            self.shapes = out.shapes.clone();
            out.shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                    _ => None,
                })
                .collect()
        }

        /// Right-click at `at` and the menu it opens: the text drawn then.
        fn right_click(&mut self, at: egui::Pos2) -> Vec<String> {
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
            self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
            self.frame(vec![button(false)]);
            self.frame(Vec::new()).into_iter().map(|(t, _)| t).collect()
        }

        /// Where the track header of the first track of `kind` shows its name.
        fn header(&mut self, kind: TrackKind) -> egui::Pos2 {
            let name = self.state.sequences[0].tracks.iter().find(|t| t.kind == kind).expect("a track of that kind").name.clone();
            let drawn = self.frame(Vec::new());
            drawn.into_iter().find(|(t, _)| *t == name).unwrap_or_else(|| panic!("{name}'s header is drawn")).1
        }
    }

    fn missing<'a>(menu: &[String], items: &[&'a str]) -> Vec<&'a str> {
        items.iter().copied().filter(|item| !menu.iter().any(|t| t.starts_with(item))).collect()
    }

    #[test]
    fn a_track_headers_menu_has_its_items() {
        let mut timeline = Timeline::new();
        for kind in [TrackKind::Video, TrackKind::Audio] {
            let at = timeline.header(kind);
            let menu = timeline.right_click(at);
            let lacking = missing(&menu, &[
                "Select All Clips on Track",
                "Select Forward on Track",
                "Select Backward on Track",
                "Add Track Above",
                "Rename Track",
                "Track Effects",
                "Copy Track Attributes",
                "Paste Track Attributes",
            ]);
            assert!(lacking.is_empty(), "the {kind:?} track header's menu lacks {lacking:?}; it shows {menu:?}");
            // Closed again before the next one.
            timeline.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        }
    }

    /// A subtitle track with captions offers Translate Captions, and a
    /// pair in it sends that track's command (#2213).
    #[test]
    fn a_caption_tracks_menu_translates_it() {
        let mut timeline = Timeline::new();
        let mut track = klipp_timeline::Track::new("Subtitle 1", TrackKind::Subtitle);
        let (zero, two) = (RationalTime { value: 0, rate: 24 }, RationalTime { value: 48, rate: 24 });
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Hello".into(), ..Default::default() });
        track.clips.push(Clip { kind: klipp_timeline::ClipKind::Title(td), ..Clip::gap(zero, two) });
        let track_id = track.id;
        timeline.state.sequences[0].tracks.push(track);
        let at = timeline.header(TrackKind::Subtitle);
        let menu = timeline.right_click(at);
        let item = timeline.frame(Vec::new()).into_iter().find(|(t, _)| t.starts_with("Translate Captions")).unwrap_or_else(|| panic!("the caption track's menu translates: {menu:?}")).1;
        timeline.frame(vec![egui::Event::PointerMoved(item)]);
        let drawn = timeline.frame(vec![egui::Event::PointerMoved(item)]);
        let at = drawn.iter().find(|(t, _)| t == "English \u{2192} German").unwrap_or_else(|| panic!("a pair to pick: {drawn:?}")).1;
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        timeline.frame(vec![egui::Event::PointerMoved(at)]);
        timeline.frame(vec![egui::Event::PointerMoved(at), button(true)]);
        timeline.frame(vec![button(false)]);
        assert!(timeline.sent.iter().any(|c| matches!(c, AppCommand::TranslateCaptions { track_id: t, source, target, .. } if *t == track_id && source == "en" && target == "de")), "{:?}", timeline.sent);
        // A track without captions doesn't offer it.
        for _ in 0..2 {
            timeline.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        }
        let at = timeline.header(TrackKind::Video);
        let menu = timeline.right_click(at);
        assert!(!menu.iter().any(|t| t.starts_with("Translate Captions")), "a video track has no captions: {menu:?}");
    }

    /// A video track header's Track Effects menu offers Grade in Colour,
    /// and a click on it sends the track's command (#1408); an audio
    /// track's doesn't offer it.
    #[test]
    fn a_video_track_headers_effects_menu_grades_it_in_colour() {
        let mut timeline = Timeline::new();
        for kind in [TrackKind::Video, TrackKind::Audio] {
            let track_id = timeline.state.sequences[0].tracks.iter().find(|t| t.kind == kind).unwrap().id;
            let at = timeline.header(kind);
            timeline.right_click(at);
            // Hovering Track Effects opens it.
            let drawn = timeline.frame(Vec::new());
            let effects = drawn.iter().find(|(t, _)| t.starts_with("Track Effects")).unwrap_or_else(|| panic!("{kind:?}: a Track Effects item")).1;
            timeline.frame(vec![egui::Event::PointerMoved(effects)]);
            let drawn = timeline.frame(vec![egui::Event::PointerMoved(effects)]);
            let item = drawn.iter().find(|(t, _)| t == "Grade in Colour").map(|(_, p)| *p);
            match kind {
                TrackKind::Video => {
                    let at = item.unwrap_or_else(|| panic!("the video track's Track Effects offers Grade in Colour: {drawn:?}"));
                    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
                    timeline.frame(vec![egui::Event::PointerMoved(at)]);
                    timeline.frame(vec![egui::Event::PointerMoved(at), button(true)]);
                    timeline.frame(vec![button(false)]);
                    assert!(timeline.sent.iter().any(|c| matches!(c, AppCommand::GradeTrackInColour { track_id: t } if *t == track_id)), "{:?}", timeline.sent);
                }
                _ => assert!(item.is_none(), "an audio track has no grade to grade"),
            }
            timeline.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
            timeline.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        }
    }

    /// Controls once drawn only by the timeline the app no longer shows
    /// (#912): the ripple switch is on the drawn toolbar, and the ruler
    /// opens its menu on a right-click.
    #[test]
    fn the_ripple_switch_and_the_ruler_menu_are_on_the_drawn_timeline() {
        let mut timeline = Timeline::new();
        let drawn = timeline.frame(Vec::new());
        assert!(drawn.iter().any(|(t, _)| t == "RIPPLE"), "the ripple switch is drawn: {drawn:?}");
        let ruler_label = drawn.iter().find(|(t, _)| t == "0:00").expect("the ruler is drawn").1;
        let menu = timeline.right_click(ruler_label + egui::vec2(30.0, 0.0));
        assert!(menu.iter().any(|t| t == "Snap targets"), "the ruler's menu opens: {menu:?}");
    }

    #[test]
    fn a_clips_menu_has_its_items() {
        let mut timeline = Timeline::new();
        // On the clip, where it shows its name.
        let drawn = timeline.frame(Vec::new());
        let at = drawn.into_iter().find(|(t, _)| t == "smoke.mov").expect("the clip is drawn").1;
        let menu = timeline.right_click(at);
        let lacking = missing(&menu, &["Rename Clip", "Clip Colour", "Grade Flag", "Rhythm Edits", "Speed", "Make Subclip", "Select Same Source", "Cut", "Copy", "Paste at Playhead", "Paste Insert at Playhead", "Duplicate Clip"]);
        assert!(lacking.is_empty(), "the clip's menu lacks {lacking:?}; it shows {menu:?}");
    }

    /// #239: a video clip whose file is there offers Track Subject, and
    /// clicking it asks for the subject's matte on that clip.
    #[test]
    fn a_clips_menu_tracks_its_subject() {
        let dir = std::env::temp_dir().join(format!("klipp-track-menu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("smoke.mov");
        std::fs::write(&file, b"x").unwrap();
        let mut timeline = Timeline::new();
        // The analysis items sit low in a long menu.
        timeline.height = 1400.0;
        let media = timeline.state.media_pool.all().next().unwrap().id;
        timeline.state.media_pool.get_mut(&media).unwrap().path = file;
        let v1 = timeline.state.sequences[0].tracks.iter().position(|t| matches!(t.kind, TrackKind::Video)).unwrap();
        let id = timeline.state.sequences[0].tracks[v1].clips[0].id;
        let drawn = timeline.frame(Vec::new());
        let at = drawn.into_iter().find(|(t, _)| t == "smoke.mov").expect("the clip is drawn").1;
        timeline.right_click(at);
        let drawn = timeline.frame(Vec::new());
        let item = drawn.iter().find(|(t, _)| t == "Track Subject").unwrap_or_else(|| panic!("Track Subject in the menu: {drawn:?}")).1;
        let button = |pressed| egui::Event::PointerButton { pos: item, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        timeline.frame(vec![egui::Event::PointerMoved(item), button(true)]);
        let mut sent = std::mem::take(&mut timeline.sent);
        timeline.frame(vec![button(false)]);
        sent.extend(std::mem::take(&mut timeline.sent));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(sent.iter().any(|c| matches!(c, AppCommand::TrackSubject { clip_id, model, into: Some(klipp_core::SubjectMatteUse::Grade) } if *clip_id == id && model == "subject")), "{sent:?}");
    }

    /// #73: the same menu offers Cut Out Subject, which asks for the
    /// subject's matte to cut the clip out.
    #[test]
    fn a_clips_menu_cuts_its_subject_out() {
        let dir = std::env::temp_dir().join(format!("klipp-cut-menu-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("smoke.mov");
        std::fs::write(&file, b"x").unwrap();
        let mut timeline = Timeline::new();
        // The analysis items sit low in a long menu.
        timeline.height = 1400.0;
        let media = timeline.state.media_pool.all().next().unwrap().id;
        timeline.state.media_pool.get_mut(&media).unwrap().path = file;
        let v1 = timeline.state.sequences[0].tracks.iter().position(|t| matches!(t.kind, TrackKind::Video)).unwrap();
        let id = timeline.state.sequences[0].tracks[v1].clips[0].id;
        let drawn = timeline.frame(Vec::new());
        let at = drawn.into_iter().find(|(t, _)| t == "smoke.mov").expect("the clip is drawn").1;
        timeline.right_click(at);
        let drawn = timeline.frame(Vec::new());
        let item = drawn.iter().find(|(t, _)| t == "Cut Out Subject").unwrap_or_else(|| panic!("Cut Out Subject in the menu: {drawn:?}")).1;
        let button = |pressed| egui::Event::PointerButton { pos: item, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        timeline.frame(vec![egui::Event::PointerMoved(item), button(true)]);
        let mut sent = std::mem::take(&mut timeline.sent);
        timeline.frame(vec![button(false)]);
        sent.extend(std::mem::take(&mut timeline.sent));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(sent.iter().any(|c| matches!(c, AppCommand::TrackSubject { clip_id, model, into: Some(klipp_core::SubjectMatteUse::CutOut) } if *clip_id == id && model == "subject")), "{sent:?}");
    }

    #[test]
    fn restore_background_menu_works_for_an_offline_still_matte() {
        use klipp_timeline::GradeAccess;
        let mut timeline = Timeline::new();
        timeline.height = 1500.0;
        let v1 = timeline.state.sequences[0].tracks.iter().position(|t| matches!(t.kind, TrackKind::Video)).unwrap();
        let clip = &mut timeline.state.sequences[0].tracks[v1].clips[0];
        let id = clip.id;
        let mut graph = clip.grade().clone();
        let matte = klipp_nodes::NodeId::new_v4();
        graph.nodes.push(klipp_nodes::Node {
            id: matte, kind: klipp_nodes::NodeKind::ExternalMatte, label: "Still matte".into(), enabled: true,
            params: klipp_nodes::NodeParams { matte_image_path: Some("/offline/matte.png".into()), ..Default::default() },
            keyframes: Default::default(), output_link: None, input_links: Vec::new(),
        });
        graph.edges.push(klipp_nodes::Edge { from_node: matte, from_port: 0, to_node: graph.output_node_id, to_port: 1 });
        clip.set_grade(graph);
        let drawn = timeline.frame(Vec::new());
        let at = drawn.into_iter().find(|(t, _)| t == "smoke.mov").expect("the clip is drawn").1;
        timeline.right_click(at);
        let drawn = timeline.frame(Vec::new());
        let item = drawn.iter().find(|(t, _)| t == "Restore Background").expect("restore in the menu").1;
        let button = |pressed| egui::Event::PointerButton { pos: item, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        timeline.frame(vec![egui::Event::PointerMoved(item), button(true)]);
        let mut sent = std::mem::take(&mut timeline.sent);
        timeline.frame(vec![button(false)]);
        sent.extend(std::mem::take(&mut timeline.sent));
        assert!(sent.iter().any(|c| matches!(c, AppCommand::RestoreBackground { clip_id } if *clip_id == id)), "{sent:?}");
    }

    /// #1844: double-clicking a multicam clip on the timeline turns
    /// multicam mode on with that clip opened; double-clicking a clip that
    /// is not one does not.
    #[test]
    fn double_clicking_a_multicam_clip_turns_the_mode_on() {
        for multicam in [true, false] {
            let mut timeline = Timeline::new();
            let v1 = timeline.state.sequences[0].tracks.iter().position(|t| matches!(t.kind, TrackKind::Video)).unwrap();
            let id = timeline.state.sequences[0].tracks[v1].clips[0].id;
            if multicam {
                timeline.state.sequences[0].tracks[v1].clips[0].multicam_group = Some(klipp_core::GroupId::new());
            }
            let drawn = timeline.frame(Vec::new());
            let at = drawn.into_iter().find(|(t, _)| t == "smoke.mov").expect("the clip is drawn").1;
            let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            timeline.frame(vec![egui::Event::PointerMoved(at), button(true), button(false), button(true), button(false)]);
            let mut sent = std::mem::take(&mut timeline.sent);
            timeline.frame(Vec::new());
            sent.extend(std::mem::take(&mut timeline.sent));
            let opened = sent.iter().any(|c| matches!(c, AppCommand::SetMulticamMode { on: true, open: Some(o) } if *o == id));
            assert_eq!(opened, multicam, "multicam {multicam}: {sent:?}");
        }
    }

    /// A sounding clip's menu adds a fade of the default fade's length
    /// and curve (Preferences > Audio): Add Fade In with 1.25 s and an
    /// exponential curve set sends exactly that, and another default
    /// sends the other, so the fade is the preference's and not fixed.
    #[test]
    fn add_fade_in_uses_the_default_fade() {
        for (secs, curve) in [(1.25_f32, klipp_core::FadeCurve::Exponential), (0.4, klipp_core::FadeCurve::Linear)] {
            let mut timeline = Timeline::new();
            let a1 = timeline.state.sequences[0].tracks.iter().position(|t| matches!(t.kind, TrackKind::Audio)).expect("an audio track");
            let (zero, long) = (RationalTime { value: 0, rate: 24 }, RationalTime { value: 24 * 600, rate: 24 });
            let media = timeline.state.media_pool.all().next().unwrap().id;
            timeline.state.sequences[0].tracks[a1].clips.push(Clip::media(media, zero, long, zero, long));
            let audio_id = timeline.state.sequences[0].tracks[a1].clips[0].id;
            timeline.state.default_fade_secs = secs;
            timeline.state.default_fade_curve = curve;
            let drawn = timeline.frame(Vec::new());
            // The audio clip's name is drawn on its track, below the video's.
            let at = drawn.iter().filter(|(t, _)| t == "smoke.mov").map(|(_, p)| *p).max_by(|a, b| a.y.total_cmp(&b.y)).expect("the audio clip is drawn");
            timeline.right_click(at);
            let item = timeline.frame(Vec::new()).into_iter().find(|(t, _)| t == "Add Fade In").expect("Add Fade In in the menu").1;
            let button = |pressed| egui::Event::PointerButton { pos: item, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            timeline.frame(vec![egui::Event::PointerMoved(item), button(true)]);
            let mut sent = std::mem::take(&mut timeline.sent);
            timeline.frame(vec![button(false)]);
            sent.extend(std::mem::take(&mut timeline.sent));
            let fade = sent.iter().find_map(|c| match c {
                AppCommand::UpdateClipAudio { clip_id, fade_in_secs, fade_in_curve, fade_out_secs, .. } if *clip_id == audio_id => Some((*fade_in_secs, *fade_in_curve, *fade_out_secs)),
                _ => None,
            });
            assert_eq!(fade, Some((secs, curve, 0.0)), "{sent:?}");
        }
    }

    /// A clip with sound has Normalize in its menu (#1843), which opens the
    /// dialog for that clip; one with none doesn't offer it.
    #[test]
    fn a_sounding_clips_menu_normalizes_it() {
        for has_audio in [true, false] {
            let mut timeline = Timeline::new();
            let media = timeline.state.media_pool.all().next().unwrap().id;
            timeline.state.media_pool.get_mut(&media).unwrap().has_audio = has_audio;
            let clip_id = timeline.state.sequences[0].all_clips().next().unwrap().id;
            let drawn = timeline.frame(Vec::new());
            let at = drawn.into_iter().find(|(t, _)| t == "smoke.mov").expect("the clip is drawn").1;
            let menu = timeline.right_click(at);
            let item = timeline.frame(Vec::new()).into_iter().find(|(t, _)| t == "Normalize\u{2026}").map(|(_, p)| p);
            let Some(item) = item else {
                assert!(!has_audio, "a clip with sound offers Normalize: {menu:?}");
                continue;
            };
            assert!(has_audio, "a clip with no sound doesn't offer Normalize");
            let button = |pressed| egui::Event::PointerButton { pos: item, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            timeline.frame(vec![egui::Event::PointerMoved(item), button(true)]);
            let mut sent = std::mem::take(&mut timeline.sent);
            timeline.frame(vec![button(false)]);
            sent.extend(std::mem::take(&mut timeline.sent));
            assert!(sent.iter().any(|c| matches!(c, AppCommand::OpenNormalizeDialog { clip_id: Some(id) } if *id == clip_id)), "{sent:?}");
        }
    }

    /// A clip with sound offers Fit Music to Length (#2210), which opens
    /// the dialog for that clip.
    #[test]
    fn a_sounding_clips_menu_fits_it_to_a_length() {
        let mut timeline = Timeline::new();
        let media = timeline.state.media_pool.all().next().unwrap().id;
        timeline.state.media_pool.get_mut(&media).unwrap().has_audio = true;
        let clip_id = timeline.state.sequences[0].all_clips().next().unwrap().id;
        let at = timeline.frame(Vec::new()).into_iter().find(|(t, _)| t == "smoke.mov").expect("the clip is drawn").1;
        let menu = timeline.right_click(at);
        let item = timeline.frame(Vec::new()).into_iter().find(|(t, _)| t == "Fit Music to Length\u{2026}").map(|(_, p)| p).unwrap_or_else(|| panic!("offered: {menu:?}"));
        let button = |pressed| egui::Event::PointerButton { pos: item, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        timeline.frame(vec![egui::Event::PointerMoved(item), button(true)]);
        let mut sent = std::mem::take(&mut timeline.sent);
        timeline.frame(vec![button(false)]);
        sent.extend(std::mem::take(&mut timeline.sent));
        assert!(sent.iter().any(|c| matches!(c, AppCommand::OpenMusicFitDialog { clip_id: id } if *id == clip_id)), "{sent:?}");
    }

    /// On the Colour page's strip, a clip's menu flags where its grade
    /// stands (#899): Grade Flag, then a flag, sends that flag for the
    /// clip; a flagged clip shows a pennant in the flag's colour.
    #[test]
    fn the_colour_strip_flags_a_clips_grade() {
        let mut timeline = Timeline::new();
        timeline.state.show_color_panel = true;
        let clip = timeline.state.sequences[0].all_clips().next().unwrap().id;
        let drawn = timeline.frame(Vec::new());
        let at = drawn.into_iter().find(|(t, _)| t == "smoke.mov").expect("the clip is drawn on the strip").1;
        timeline.right_click(at);
        let drawn = timeline.frame(Vec::new());
        let item = drawn.iter().find(|(t, _)| t == "Grade Flag").unwrap_or_else(|| panic!("a Grade Flag item: {drawn:?}")).1;
        timeline.frame(vec![egui::Event::PointerMoved(item)]);
        let drawn = timeline.frame(vec![egui::Event::PointerMoved(item)]);
        let flag = drawn.iter().find(|(t, _)| t.ends_with("Needs Work")).unwrap_or_else(|| panic!("a Needs Work flag: {drawn:?}")).1;
        let button = |pressed| egui::Event::PointerButton { pos: flag, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        timeline.frame(vec![egui::Event::PointerMoved(flag)]);
        timeline.frame(vec![egui::Event::PointerMoved(flag), button(true)]);
        timeline.frame(vec![button(false)]);
        assert!(timeline.sent.iter().any(|c| matches!(c, AppCommand::SetClipGradeFlags { clip_ids, flag } if *clip_ids == vec![clip] && *flag == klipp_timeline::GradeFlag::NeedsWork)), "{:?}", timeline.sent);

        // Flagged, the clip shows its pennant; unflagged, none.
        let pennant = |t: &mut Timeline, flag: klipp_timeline::GradeFlag| {
            let [r, g, b] = flag.rgb();
            t.frame(Vec::new());
            t.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Path(p) if p.closed && p.points.len() == 3 && p.fill == egui::Color32::from_rgb(r, g, b)))
        };
        assert!(!pennant(&mut timeline, klipp_timeline::GradeFlag::NeedsWork));
        for c in timeline.state.sequences[0].all_clips_mut() {
            c.grade_flag = klipp_timeline::GradeFlag::NeedsWork;
        }
        assert!(pennant(&mut timeline, klipp_timeline::GradeFlag::NeedsWork), "no pennant on the flagged clip");
    }

    /// On the Colour strip, another clip's menu wipes the clip the viewer
    /// shows against it (#899).
    #[test]
    fn the_colour_strip_wipes_against_another_clip() {
        let mut timeline = Timeline::new();
        timeline.state.show_color_panel = true;
        let v1 = timeline.state.sequences[0].tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let track = &mut timeline.state.sequences[0].tracks[v1];
        let media = track.clips[0].media_id().unwrap();
        let t = |secs: i64| RationalTime { value: 24 * secs, rate: 24 };
        track.clips = vec![Clip::media(media, t(0), t(4), t(0), t(4)), Clip::media(media, t(4), t(600), t(4), t(600))];
        track.clips[0].name = Some("First".into());
        track.clips[1].name = Some("Second".into());
        let (first, second) = (track.clips[0].id, track.clips[1].id);
        timeline.state.selected_clip = Some(first);
        let drawn = timeline.frame(Vec::new());
        let at = drawn.iter().find(|(t, _)| t == "Second").expect("the second clip is drawn").1;
        // The viewer shows the first, at the playhead; the right-click
        // selects the second.
        let menu = timeline.right_click(at);
        let drawn = timeline.frame(Vec::new());
        let item = drawn.iter().find(|(t, _)| t == "Wipe Against This Clip").unwrap_or_else(|| panic!("no wipe item: {menu:?}")).1;
        let button = |pressed| egui::Event::PointerButton { pos: item, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        timeline.frame(vec![egui::Event::PointerMoved(item)]);
        timeline.frame(vec![egui::Event::PointerMoved(item), button(true)]);
        timeline.frame(vec![button(false)]);
        assert!(timeline.sent.iter().any(|c| matches!(c, AppCommand::WipeAgainstClip { target: Some(klipp_core::WipeClip::Clip(id)) } if *id == second)), "{:?}", timeline.sent);
    }

    /// A grey ramp, 2 s at 24 fps, written once with the ffmpeg CLI; `None`
    /// without it.
    fn ramp() -> Option<std::path::PathBuf> {
        static RAMP: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
        RAMP.get_or_init(|| {
            let path = std::env::temp_dir().join(format!("klipp-graded-strip-{}.mkv", std::process::id()));
            let ok = std::process::Command::new("ffmpeg")
                .args(["-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", "color=c=black:s=64x36:r=24:d=2,geq=lum='60+2*N':cb=128:cr=128", "-c:v", "ffv1", "-pix_fmt", "yuv420p"])
                .arg(&path)
                .status()
                .is_ok_and(|s| s.success());
            ok.then_some(path)
        })
        .clone()
    }

    /// The Colour page's strip shows each clip graded (#899): its graded
    /// thumbnail is drawn along the clip; a change to its grade makes a
    /// new one, drawn once the change settles, the old one up meanwhile.
    #[test]
    fn the_colour_strip_shows_each_clip_graded() {
        let Some(path) = ramp() else {
            eprintln!("skipping: ffmpeg could not write the source");
            return;
        };
        let mut timeline = Timeline::new();
        let v1 = timeline.state.sequences[0].tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let mut item = klipp_media::MediaItem::synthetic_for_test(path, None);
        let rt = |f| RationalTime { value: f, rate: 24 };
        item.duration = rt(48);
        item.fps = klipp_core::Fps::new(24, 1);
        item.resolution = klipp_core::Resolution { width: 64, height: 36 };
        let media = timeline.state.media_pool.add(item);
        timeline.state.sequences[0].tracks[v1].clips = vec![Clip::media(media, rt(0), rt(48), rt(0), rt(48))];
        let clip = timeline.state.sequences[0].tracks[v1].clips[0].id;
        timeline.state.show_color_panel = true;
        let ws = klipp_core::WorkingColorspace::Rec709Linear;
        let update = |t: &mut Timeline, now: f64| {
            let s = &mut t.state;
            s.graded_thumbs.update(&t.ctx, now, &s.sequences[0], &s.media_pool, ws, None);
            s.graded_thumbs.settle(&t.ctx);
        };
        update(&mut timeline, 0.0);
        let texture = timeline.state.graded_thumbs.texture(clip).expect("the clip's graded thumbnail").id();
        timeline.frame(Vec::new());
        let drawn = |t: &Timeline, id: egui::TextureId| t.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill_texture_id == id) || matches!(&s.shape, egui::Shape::Mesh(m) if m.texture_id == id));
        assert!(drawn(&timeline, texture), "the graded thumbnail isn't drawn on the strip");

        // A grade change: the thumbnail is stale until it is drawn again.
        let c = timeline.state.sequences[0].tracks[v1].clips.iter_mut().find(|c| c.id == clip).unwrap();
        klipp_timeline::GradeAccess::edit_grade(c, |g| {
            let id = g.ensure_corrector();
            g.nodes.iter_mut().find(|n| n.id == id).unwrap().params.exposure = Some(1.0);
        });
        let graded = timeline.state.sequences[0].tracks[v1].clips[0].clone();
        assert!(!timeline.state.graded_thumbs.is_current(&graded, ws));
        update(&mut timeline, 1.0);
        assert!(timeline.state.graded_thumbs.is_current(&graded, ws), "the new grade's thumbnail wasn't drawn");
        let new = timeline.state.graded_thumbs.texture(clip).unwrap().id();
        timeline.frame(Vec::new());
        assert!(drawn(&timeline, new));

        // On the Edit page, the media's own filmstrip, not the grade.
        timeline.state.show_color_panel = false;
        timeline.frame(Vec::new());
        assert!(!drawn(&timeline, new), "the Edit page's timeline shows the graded thumbnail");
    }

    /// A right-click on the cut between two clips opens the edit point's
    /// menu (#925), not either clip's.
    #[test]
    fn an_edit_points_menu_has_its_items() {
        let mut timeline = Timeline::new();
        let v1 = timeline.state.sequences[0].tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let track = &mut timeline.state.sequences[0].tracks[v1];
        let media = track.clips[0].media_id().unwrap();
        let t = |secs: i64| RationalTime { value: 24 * secs, rate: 24 };
        track.clips = vec![Clip::media(media, t(0), t(4), t(0), t(4)), Clip::media(media, t(4), t(600), t(4), t(600))];
        track.clips[1].name = Some("Second".into());
        let drawn = timeline.frame(Vec::new());
        let second = drawn.into_iter().find(|(t, _)| t == "Second").expect("the second clip is drawn").1;
        // Leftward from the second clip's name to its start, the cut.
        let menu = (0..200)
            .map(|step| second - egui::vec2(step as f32 * 2.0, 0.0))
            .find_map(|at| {
                let menu = timeline.right_click(at);
                timeline.frame(vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
                menu.iter().any(|t| t == "Move Playhead Here").then_some(menu)
            })
            .expect("a right-click on the cut opens the edit point's menu");
        let lacking = missing(&menu, &["Add Cross Dissolve", "Add Transition", "Move Playhead Here", "Select Clip Before", "Select Clip After", "Select Both Clips"]);
        assert!(lacking.is_empty(), "the edit point's menu lacks {lacking:?}; it shows {menu:?}");
        assert!(!menu.iter().any(|t| t.starts_with("Rename Clip")), "and not a clip's: {menu:?}");
    }

    #[test]
    fn rename_clip_from_its_menu_names_it() {
        let mut timeline = Timeline::new();
        let drawn = timeline.frame(Vec::new());
        let at = drawn.into_iter().find(|(t, _)| t == "smoke.mov").expect("the clip is drawn").1;
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers: egui::Modifiers::NONE };
        timeline.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
        timeline.frame(vec![button(at, false)]);
        let menu = timeline.frame(Vec::new());
        let item = menu.into_iter().find(|(t, _)| t.starts_with("Rename Clip")).expect("the menu has Rename Clip").1;
        let click = |pressed| egui::Event::PointerButton { pos: item, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        timeline.frame(vec![egui::Event::PointerMoved(item), click(true)]);
        timeline.frame(vec![click(false)]);
        let window = timeline.frame(Vec::new());
        assert!(window.iter().any(|(t, _)| t == "Rename Clip"), "the rename window opens: {window:?}");
        timeline.frame(vec![egui::Event::Text("Hero shot".into())]);
        timeline.frame(vec![egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
        let clip_id = timeline.state.sequences[0].tracks.iter().find(|t| t.kind == TrackKind::Video).unwrap().clips[0].id;
        assert!(
            timeline.sent.iter().any(|c| matches!(c, AppCommand::RenameClips { clip_ids, name } if *clip_ids == vec![clip_id] && name.as_deref() == Some("Hero shot"))),
            "Enter renames the clip: {:?}", timeline.sent
        );
        let after = timeline.frame(Vec::new());
        assert!(!after.iter().any(|(t, _)| t == "Rename Clip"), "and the window closes");
    }

    #[test]
    fn a_clip_shows_its_own_name_and_its_speed() {
        let mut timeline = Timeline::new();
        let clip = &mut timeline.state.sequences[0].tracks.iter_mut().find(|t| t.kind == TrackKind::Video).unwrap().clips[0];
        clip.name = Some("Hero shot".into());
        clip.colour_label = 4;
        clip.speed = 1.5;
        let drawn: Vec<String> = timeline.frame(Vec::new()).into_iter().map(|(t, _)| t).collect();
        assert!(drawn.iter().any(|t| t == "Hero shot"), "the clip's own name is drawn: {drawn:?}");
        assert!(!drawn.iter().any(|t| t == "smoke.mov"), "and not its media's: {drawn:?}");
        assert!(drawn.iter().any(|t| t == "150%"), "its speed badge is drawn: {drawn:?}");
    }
}
