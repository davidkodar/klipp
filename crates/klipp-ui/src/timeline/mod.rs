pub mod toolbar;
pub mod track_headers;
pub mod ruler;
pub mod playhead;
pub mod skimmer;
pub mod sequence_tabs;
pub mod time_utils;
pub mod range_visuals;
pub mod grid_snap;

use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// New timeline panel that wraps the existing TimelinePanel
/// with the redesigned visual shell (toolbar, track headers, ruler).
///
/// Phase 7A: Structure + track headers + ruler (new visual shell)
/// Phase 7B: Clip rendering delegated to existing panels/timeline.rs
/// Phase 7C: Full migration of interaction logic
pub struct NewTimelinePanel;

impl NewTimelinePanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut AppState,
        theme: &KlippTheme,
        old_timeline: &mut crate::panels::timeline::TimelinePanel,
        kb: &crate::keybindings::KeyBindings,
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

        // Content area below tabs
        let content_rect = egui::Rect::from_min_max(
            egui::pos2(full_rect.min.x, tabs_rect.max.y),
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

        // Compute tracks_rect early — needed by both range visuals and clip delegation
        let tracks_rect = egui::Rect::from_min_max(
            egui::pos2(timeline_content_rect.min.x, timeline_content_rect.min.y + ruler_h),
            timeline_content_rect.max,
        );

        let ruler_painter = ui.painter().with_clip_rect(ruler_rect);
        ruler::draw_ruler(&ruler_painter, ruler_rect, pps, scroll_x, duration_secs, theme);

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
                snap_secs, state.fps.rate(), theme,
            );
        }
        // allocate_clipped_ui sets clip_rect = tracks_rect, preventing the old
        // timeline panel from bleeding into the inspector or status bar.
        crate::ui_util::allocate_clipped_ui(ui, tracks_rect, |ui| {
            let old_cmds = old_timeline.show(ui, state);
            cmds.extend(old_cmds);
        });

        cmds
    }
}
