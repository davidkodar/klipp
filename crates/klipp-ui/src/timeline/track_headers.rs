use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::tooltip::styled_tooltip;
use klipp_core::AppCommand;
use klipp_timeline::TrackKind;

/// Render the 110px track header column: ruler spacer + per-track headers.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    header_rect: egui::Rect,
) -> Vec<AppCommand> {
    let ruler_h = crate::theme::RULER_HEIGHT;

    // ── Ruler spacer (22px) ──
    let spacer_rect = egui::Rect::from_min_size(
        header_rect.min,
        egui::vec2(header_rect.width(), ruler_h),
    );
    ui.painter().rect_filled(spacer_rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        spacer_rect.x_range(),
        spacer_rect.max.y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.painter().text(
        egui::pos2(spacer_rect.min.x + 6.0, spacer_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "TRACKS",
        egui::FontId::monospace(10.0),
        theme.text_dim,
    );
    super::camera_lane::header_toggle(ui, state, theme, spacer_rect);

    // ── The camera lane's cell, under the spacer, when the lane is shown ──
    let lane_h = super::camera_lane::height(state);
    if lane_h > 0.0 {
        let lane_cell = egui::Rect::from_min_size(
            egui::pos2(header_rect.min.x, spacer_rect.max.y),
            egui::vec2(header_rect.width(), lane_h),
        );
        super::camera_lane::draw_header(ui, theme, lane_cell);
    }

    let mut cmds = Vec::new();

    // ── Track headers ──
    // Apply the same vertical scroll offset the clip area uses so rows stay aligned.
    let scroll_y = state.timeline_scroll_y;
    let mut y = header_rect.min.y + ruler_h + lane_h - scroll_y;
    let visible_top = header_rect.min.y + ruler_h + lane_h;
    let visible_bottom = header_rect.max.y;
    let mut toggle_mute: Option<usize> = None;
    let mut toggle_lock: Option<usize> = None;
    // Deferred magnetic operation from context menu (track_idx, action)
    #[derive(Clone)]
    enum TrackAction {
        PromoteToStoryline(usize),
        BreakApartStoryline(usize),
        SetAsPrimary(usize),
    }
    let mut deferred_action: Option<TrackAction> = None;

    let track_count = state.sequence().tracks.len();
    for track_idx in 0..track_count {
        let track = &state.sequences[state.active_sequence_idx].tracks[track_idx];
        let track_h = crate::panels::timeline::effective_track_height(track);

        // Vertical visibility culling — skip rows above/below the visible header strip.
        if y + track_h < visible_top {
            y += track_h;
            continue;
        }
        if y > visible_bottom {
            break;
        }

        let track_rect = egui::Rect::from_min_size(
            egui::pos2(header_rect.min.x, y),
            egui::vec2(header_rect.width(), track_h),
        );

        // Background
        let bg = if ui.rect_contains_pointer(track_rect) {
            theme.bg_hover
        } else {
            theme.bg_deep
        };
        ui.painter().rect_filled(track_rect, 0.0, bg);

        // Bottom border
        ui.painter().hline(
            track_rect.x_range(),
            track_rect.max.y,
            egui::Stroke::new(1.0_f32, theme.border),
        );

        // Role color bar (3px left edge)
        let role_color = match track.kind {
            TrackKind::Video => {
                // Determine role by track name
                if track.name.to_lowercase().contains("b-roll") || track.name.to_lowercase().contains("broll") {
                    theme.role_broll
                } else if track.name.to_lowercase().contains("title") {
                    theme.role_title
                } else {
                    theme.role_video
                }
            }
            TrackKind::Audio => {
                if track.name.to_lowercase().contains("dialogue") || track.name.to_lowercase().contains("vo") {
                    theme.role_dialogue
                } else if track.name.to_lowercase().contains("music") || track.name.to_lowercase().contains("score") {
                    theme.role_music
                } else if track.name.to_lowercase().contains("sfx") {
                    theme.role_sfx
                } else {
                    theme.role_dialogue
                }
            }
            TrackKind::Text => theme.role_title,
            TrackKind::Subtitle => theme.role_title,
        };
        let bar_rect = egui::Rect::from_min_size(
            track_rect.min,
            egui::vec2(crate::theme::CLIP_ROLE_BAR_W, track_h),
        );
        ui.painter().rect_filled(
            bar_rect,
            egui::Rounding { nw: 0.0, ne: 2.0, sw: 0.0, se: 2.0 },
            role_color,
        );

        // Right-click context menu on track header area (allocated FIRST so buttons sit on top)
        let header_resp = ui.allocate_rect(track_rect, egui::Sense::click());

        // Layer index badge (V1, V2, A1, etc.) — fixed, shows compositing order
        let badge_x = track_rect.min.x + crate::theme::CLIP_ROLE_BAR_W + 4.0;
        let layer_label = {
            let prefix = match track.kind {
                klipp_timeline::TrackKind::Video => "V",
                klipp_timeline::TrackKind::Audio => "A",
                klipp_timeline::TrackKind::Text => "T",
                klipp_timeline::TrackKind::Subtitle => "S",
            };
            // Extract number from track name (e.g., "Video 3" → "3")
            let num = track.name.split_whitespace().last()
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            if num > 0 { format!("{}{}", prefix, num) } else { prefix.to_string() }
        };
        let badge_color = match track.kind {
            klipp_timeline::TrackKind::Video => theme.role_video,
            klipp_timeline::TrackKind::Audio => theme.role_dialogue,
            klipp_timeline::TrackKind::Text => theme.role_title,
            klipp_timeline::TrackKind::Subtitle => theme.role_title,
        };
        let badge_w = if layer_label.len() > 2 { 22.0 } else { 18.0 };
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(badge_x, track_rect.center().y - 6.0),
            egui::vec2(badge_w, 12.0),
        );
        ui.painter().rect_filled(badge_rect, 2.0, badge_color);
        ui.painter().text(
            badge_rect.center(),
            egui::Align2::CENTER_CENTER,
            &layer_label,
            egui::FontId::monospace(10.0),
            egui::Color32::WHITE,
        );

        // Track name (after the badge)
        let name_x = badge_rect.right() + 4.0;
        ui.painter().text(
            egui::pos2(name_x, track_rect.center().y),
            egui::Align2::LEFT_CENTER,
            &track.name,
            egui::FontId::proportional(10.0),
            theme.text_secondary,
        );

        // Magnetic role badge (small pill after track name)
        let role_badge = match track.magnetic_role {
            klipp_timeline::TrackRole::Primary => Some(("P", theme.marker_info)),
            klipp_timeline::TrackRole::Connected => Some(("C", theme.accent)),
            klipp_timeline::TrackRole::ConnectedStoryline => Some(("CS", theme.role_graphics)),
            klipp_timeline::TrackRole::Free => None,
        };
        if let Some((label, color)) = role_badge {
            let badge_w = if label.len() > 1 { 18.0 } else { 14.0 };
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(name_x + 2.0, track_rect.center().y + 6.0),
                egui::vec2(badge_w, 10.0),
            );
            ui.painter().rect_filled(badge_rect, 2.0, color);
            ui.painter().text(
                badge_rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::monospace(10.0),
                egui::Color32::WHITE,
            );
        }

        // Track opacity slider (video tracks only) — horizontal bar at bottom of header
        if track.kind == TrackKind::Video {
            let opacity_pct = (track.opacity * 100.0) as u32;
            let slider_h = 12.0;
            let slider_rect = egui::Rect::from_min_size(
                egui::pos2(track_rect.min.x + 4.0, track_rect.max.y - slider_h - 2.0),
                egui::vec2(track_rect.width() - 8.0, slider_h),
            );
            let slider_resp = ui.interact(
                slider_rect, ui.id().with(("track_opacity", track_idx)),
                egui::Sense::click_and_drag(),
            );
            // Background
            ui.painter().rect_filled(slider_rect, 2.0, theme.bg_panel_alt);
            // Fill bar
            let fill_w = slider_rect.width() * track.opacity;
            let fill_rect = egui::Rect::from_min_size(
                slider_rect.left_top(), egui::vec2(fill_w, slider_rect.height()),
            );
            ui.painter().rect_filled(fill_rect, 2.0, theme.role_video);
            // Label
            ui.painter().text(
                slider_rect.center(), egui::Align2::CENTER_CENTER,
                format!("{}%", opacity_pct), egui::FontId::proportional(10.0),
                theme.text_secondary,
            );
            // Interaction
            if slider_resp.double_clicked() {
                cmds.push(AppCommand::SetTrackOpacity { track_index: track_idx, opacity: 1.0 });
            } else if slider_resp.dragged() {
                let dx = slider_resp.drag_delta().x;
                let new_opacity = (track.opacity + dx / slider_rect.width()).clamp(0.0, 1.0);
                cmds.push(AppCommand::SetTrackOpacity { track_index: track_idx, opacity: new_opacity });
            }
        }

        // Per-track VU meter (audio tracks only)
        if track.kind == TrackKind::Audio {
            if let Some(&(peak_l, peak_r)) = state.track_peak_levels.get(track_idx) {
                let meter_w = 40.0;
                let bar_h = 3.0;
                let meter_x = name_x + 2.0;
                let meter_y = track_rect.center().y + 8.0; // below track name

                for (ch, peak) in [(0, peak_l), (1, peak_r)] {
                    let y = meter_y + ch as f32 * (bar_h + 1.0);
                    // Background
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(egui::pos2(meter_x, y), egui::vec2(meter_w, bar_h)),
                        1.0, theme.bg_deep,
                    );
                    // Fill
                    let fill_w = (peak.min(1.0) * meter_w).max(0.0);
                    if fill_w > 0.0 {
                        let color = if peak > 0.9 { theme.marker_error }
                            else if peak > 0.7 { theme.marker_warning }
                            else { theme.marker_success };
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(egui::pos2(meter_x, y), egui::vec2(fill_w, bar_h)),
                            1.0, color,
                        );
                    }
                }
                // Request repaint during playback for animation
                if peak_l > 0.001 || peak_r > 0.001 {
                    ui.ctx().request_repaint();
                }
            }
        }

        // Automation-lane expand/collapse button (audio tracks only).
        // Sits left of the record-arm. ▸ collapsed → ▾ expanded; click
        // dispatches `ToggleAudioAutomationLane`. Same data the inline
        // rubber band uses, just rendered at full lane height when open.
        let btn_size = 16.0;
        if track.kind == TrackKind::Audio {
            let lane_rect_btn = egui::Rect::from_min_size(
                egui::pos2(
                    track_rect.max.x - btn_size * 5.0 - 20.0,
                    track_rect.center().y - btn_size / 2.0,
                ),
                egui::vec2(btn_size, btn_size),
            );
            let lane_resp = ui.allocate_rect(lane_rect_btn, egui::Sense::click());
            let lane_open = track.automation_lane_visible.is_some();
            let lane_color = if lane_open { theme.accent } else { theme.text_dim };
            ui.painter().rect_filled(
                lane_rect_btn,
                3.0,
                if lane_resp.hovered() { theme.bg_raised } else { egui::Color32::TRANSPARENT },
            );
            let lane_glyph = if lane_open { "\u{25BE}" } else { "\u{25B8}" }; // ▾ vs ▸
            ui.painter().text(
                lane_rect_btn.center(),
                egui::Align2::CENTER_CENTER,
                lane_glyph,
                egui::FontId::proportional(12.0),
                lane_color,
            );
            let lane_tip = if lane_open {
                "Hide automation lane"
            } else {
                "Show automation lane"
            };
            styled_tooltip(&lane_resp, theme, lane_tip, None);
            if lane_resp.clicked() {
                cmds.push(AppCommand::ToggleAudioAutomationLane { track_idx });
            }
        }

        // Record-arm button (audio tracks only) — dim red when un-armed,
        // bright red when armed. Appears to the left of the mute button.
        if track.kind == TrackKind::Audio {
            let arm_rect = egui::Rect::from_min_size(
                egui::pos2(
                    track_rect.max.x - btn_size * 4.0 - 16.0,
                    track_rect.center().y - btn_size / 2.0,
                ),
                egui::vec2(btn_size, btn_size),
            );
            let arm_resp = ui.allocate_rect(arm_rect, egui::Sense::click());
            let arm_active = track.record_armed;
            let arm_color = if arm_active {
                theme.marker_error
            } else {
                theme.accent_dim
            };
            ui.painter().rect_filled(
                arm_rect,
                3.0,
                if arm_resp.hovered() { theme.bg_raised } else { egui::Color32::TRANSPARENT },
            );
            // U+25CF BLACK CIRCLE — covered by egui's default font fallback
            // chain (U+23FA, the proper record symbol, is not).
            ui.painter().text(
                arm_rect.center(),
                egui::Align2::CENTER_CENTER,
                "\u{25CF}",
                egui::FontId::proportional(11.0),
                arm_color,
            );
            let tip = if arm_active { "Un-arm for recording" } else { "Arm for recording" };
            styled_tooltip(&arm_resp, theme, tip, None);
            if arm_resp.clicked() {
                cmds.push(AppCommand::ArmTrack {
                    track_id: track.id,
                    armed: !arm_active,
                });
            }
        }

        // Mute button (right side, 16x16)
        let mute_rect = egui::Rect::from_min_size(
            egui::pos2(
                track_rect.max.x - btn_size * 3.0 - 12.0,
                track_rect.center().y - btn_size / 2.0,
            ),
            egui::vec2(btn_size, btn_size),
        );
        let mute_resp = ui.allocate_rect(mute_rect, egui::Sense::click());
        let mute_active = track.muted;
        let mute_color = if mute_active { theme.accent } else { theme.text_dim };
        ui.painter().rect_filled(
            mute_rect,
            3.0,
            if mute_resp.hovered() { theme.bg_raised } else { egui::Color32::TRANSPARENT },
        );
        let mute_icon = if mute_active { Icon::SpeakerSlash } else { Icon::SpeakerHigh };
        mute_icon.image(12.0).tint(mute_color).paint_at(ui, mute_rect.shrink(2.0));
        styled_tooltip(&mute_resp, theme, "Mute track", None);
        if mute_resp.clicked() {
            toggle_mute = Some(track_idx);
        }

        // Solo button (16x16)
        let solo_rect = egui::Rect::from_min_size(
            egui::pos2(
                track_rect.max.x - btn_size * 2.0 - 8.0,
                track_rect.center().y - btn_size / 2.0,
            ),
            egui::vec2(btn_size, btn_size),
        );
        let solo_resp = ui.allocate_rect(solo_rect, egui::Sense::click());
        let solo_active = track.solo;
        let solo_bg = if solo_active {
            theme.accent // bright gold when active
        } else if solo_resp.hovered() {
            theme.bg_raised
        } else {
            egui::Color32::TRANSPARENT
        };
        let solo_text_color = if solo_active { theme.text_on_accent } else { theme.text_dim };
        ui.painter().rect_filled(solo_rect, 2.0, solo_bg);
        ui.painter().text(
            solo_rect.center(),
            egui::Align2::CENTER_CENTER,
            "S",
            egui::FontId::proportional(10.0),
            solo_text_color,
        );
        styled_tooltip(&solo_resp, theme, "Solo track", None);
        if solo_resp.clicked() {
            cmds.push(AppCommand::SetTrackSolo { track_index: track_idx, solo: !solo_active });
        }

        // Lock button (16x16)
        let lock_rect = egui::Rect::from_min_size(
            egui::pos2(
                track_rect.max.x - btn_size - 4.0,
                track_rect.center().y - btn_size / 2.0,
            ),
            egui::vec2(btn_size, btn_size),
        );
        let lock_resp = ui.allocate_rect(lock_rect, egui::Sense::click());
        let lock_active = track.locked;
        let lock_color = if lock_active { theme.accent } else { theme.text_dim };
        ui.painter().rect_filled(
            lock_rect,
            3.0,
            if lock_resp.hovered() { theme.bg_raised } else { egui::Color32::TRANSPARENT },
        );
        let lock_icon = if lock_active { Icon::Lock } else { Icon::LockOpen };
        lock_icon.image(12.0).tint(lock_color).paint_at(ui, lock_rect.shrink(2.0));
        styled_tooltip(&lock_resp, theme, "Lock track", None);
        if lock_resp.clicked() {
            toggle_lock = Some(track_idx);
        }

        let track_role = track.magnetic_role.clone();
        let track_id = track.id;
        let track_kind = track.kind;
        header_resp.context_menu(|ui| {
            ui.set_min_width(180.0);

            // Add track options
            ui.menu_button("Add Track Above", |ui| {
                for (label, kind) in [("Video Track", TrackKind::Video), ("Audio Track", TrackKind::Audio), ("Text Track", TrackKind::Text), ("Subtitle Track", TrackKind::Subtitle)] {
                    if ui.button(label).clicked() {
                        let seq = &mut state.sequences[state.active_sequence_idx];
                        let name = seq.next_track_name(kind);
                        let new_track = klipp_timeline::Track::new(&name, kind);
                        seq.tracks.insert(track_idx, new_track);
                        seq.renumber_tracks();
                        cmds.push(AppCommand::SequenceUpdated);
                        ui.close_menu();
                    }
                }
            });
            ui.menu_button("Add Track Below", |ui| {
                for (label, kind) in [("Video Track", TrackKind::Video), ("Audio Track", TrackKind::Audio), ("Text Track", TrackKind::Text), ("Subtitle Track", TrackKind::Subtitle)] {
                    if ui.button(label).clicked() {
                        let seq = &mut state.sequences[state.active_sequence_idx];
                        let name = seq.next_track_name(kind);
                        let new_track = klipp_timeline::Track::new(&name, kind);
                        let idx = (track_idx + 1).min(seq.tracks.len());
                        seq.tracks.insert(idx, new_track);
                        seq.renumber_tracks();
                        cmds.push(AppCommand::SequenceUpdated);
                        ui.close_menu();
                    }
                }
            });

            ui.separator();

            // Magnetic role options
            match track_role {
                klipp_timeline::TrackRole::Connected => {
                    if ui.button("Create Storyline (Shift+S)").clicked() {
                        deferred_action = Some(TrackAction::PromoteToStoryline(track_idx));
                        ui.close_menu();
                    }
                }
                klipp_timeline::TrackRole::ConnectedStoryline => {
                    if ui.button("Break Apart Storyline").clicked() {
                        deferred_action = Some(TrackAction::BreakApartStoryline(track_idx));
                        ui.close_menu();
                    }
                }
                klipp_timeline::TrackRole::Free => {
                    if ui.button("Set as Primary").clicked() {
                        deferred_action = Some(TrackAction::SetAsPrimary(track_idx));
                        ui.close_menu();
                    }
                }
                klipp_timeline::TrackRole::Primary => {}
            }

            ui.separator();

            // Remove track
            let has_clips = !state.sequence().tracks[track_idx].clips.is_empty();
            if ui.add_enabled(!has_clips, egui::Button::new("Remove Track")).clicked() {
                cmds.push(AppCommand::RemoveTrack { track_id });
                ui.close_menu();
            }
            if has_clips
                && ui.button("Remove Track (with clips)").clicked() {
                    cmds.push(AppCommand::RemoveTrack { track_id });
                    ui.close_menu();
                }
        });

        // ── Track height resize handle (bottom edge of header) ──
        let handle_rect = egui::Rect::from_min_size(
            egui::pos2(track_rect.left(), track_rect.max.y - 3.0),
            egui::vec2(track_rect.width(), 6.0),
        );
        let handle_resp = ui.interact(
            handle_rect,
            egui::Id::new(("track_resize_new", track_idx)),
            egui::Sense::click_and_drag(),
        );

        if handle_resp.hovered() || handle_resp.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            ui.painter().line_segment(
                [
                    egui::pos2(track_rect.left(), track_rect.max.y - 1.0),
                    egui::pos2(track_rect.right(), track_rect.max.y - 1.0),
                ],
                egui::Stroke::new(1.0_f32, theme.border_light),
            );
        }

        // Drag start — snapshot original height
        if handle_resp.drag_started() {
            state.timeline_resizing_track = Some((
                track_idx,
                handle_resp.interact_pointer_pos().map_or(0.0, |p| p.y),
                track_h,
            ));
        }

        // Dragging — update custom_height directly for smooth per-frame feedback.
        // The committed value is routed through SetTrackCustomHeight on drag_stopped.
        if handle_resp.dragged() {
            if let Some((ridx, start_y, orig_h)) = state.timeline_resizing_track {
                if ridx == track_idx {
                    if let Some(pos) = handle_resp.interact_pointer_pos() {
                        let delta = pos.y - start_y;
                        let new_h = (orig_h + delta).clamp(32.0, 300.0);
                        state.sequence_mut().tracks[track_idx].custom_height = Some(new_h);
                        state.dirty = true;
                    }
                }
            }
        }

        // Drag end — route the final committed value through the command so it
        // enters the undo/engine pipeline once per gesture.
        if handle_resp.drag_stopped() {
            state.timeline_resizing_track = None;
            let final_h = state.sequence().tracks[track_idx].custom_height;
            cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: final_h });
        }

        // Double-click — collapse/expand toggle with height memory
        if handle_resp.double_clicked() {
            let min_h = 32.0;
            let new_h = if track_h <= min_h + 2.0 {
                // Expand — restore pre-collapse height or default by kind
                state
                    .timeline_track_pre_collapse
                    .remove(&track_idx)
                    .unwrap_or(match track_kind {
                        TrackKind::Audio => crate::panels::timeline::AUDIO_TRACK_HEIGHT,
                        TrackKind::Text | TrackKind::Subtitle => {
                            crate::panels::timeline::TEXT_TRACK_HEIGHT
                        }
                        _ => crate::panels::timeline::VIDEO_TRACK_HEIGHT,
                    })
            } else {
                // Collapse — save current height first
                state.timeline_track_pre_collapse.insert(track_idx, track_h);
                min_h
            };
            cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(new_h) });
        }

        // Right-click context menu — preset heights
        handle_resp.context_menu(|ui| {
            if ui.button("Small (32px)").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(32.0) });
                ui.close_menu();
            }
            if ui.button("Medium (64px)").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(64.0) });
                ui.close_menu();
            }
            if ui.button("Large (120px)").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(120.0) });
                ui.close_menu();
            }
            if ui.button("Expanded (200px)").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: Some(200.0) });
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Reset to default").clicked() {
                cmds.push(AppCommand::SetTrackCustomHeight { track_id, height: None });
                ui.close_menu();
            }
        });

        y += track_h;
    }

    // ── Empty area below tracks: right-click to add track ──
    let empty_rect = egui::Rect::from_min_max(
        egui::pos2(header_rect.min.x, y),
        header_rect.max,
    );
    if empty_rect.height() > 10.0 {
        let empty_resp = ui.interact(empty_rect, ui.id().with("empty_header_ctx"), egui::Sense::click());
        empty_resp.context_menu(|ui| {
            ui.set_min_width(160.0);
            ui.label(egui::RichText::new("Add Track").strong().size(11.0));
            ui.separator();
            for (label, kind) in [("Video Track", TrackKind::Video), ("Audio Track", TrackKind::Audio), ("Text Track", TrackKind::Text), ("Subtitle Track", TrackKind::Subtitle)] {
                if ui.button(label).clicked() {
                    let seq = &mut state.sequences[state.active_sequence_idx];
                    let name = seq.next_track_name(kind);
                    match kind {
                        TrackKind::Video => { seq.add_video_track(name); }
                        TrackKind::Audio => { seq.add_audio_track(name); }
                        TrackKind::Text => { seq.add_text_track(name); }
                        TrackKind::Subtitle => {
                            let track = klipp_timeline::Track::new(&name, kind);
                            seq.tracks.push(track);
                            seq.renumber_tracks();
                        }
                    }
                    cmds.push(AppCommand::SequenceUpdated);
                    ui.close_menu();
                }
            }
        });
    }

    // Apply deferred toggles
    if let Some(idx) = toggle_mute {
        let muted = state.sequence().tracks[idx].muted;
        state.sequence_mut().tracks[idx].muted = !muted;
        cmds.push(AppCommand::SetTrackMute { track_index: idx, muted: !muted });
    }
    if let Some(idx) = toggle_lock {
        let locked = state.sequence().tracks[idx].locked;
        state.sequence_mut().tracks[idx].locked = !locked;
    }

    // Apply deferred magnetic track actions
    if let Some(action) = deferred_action {
        match action {
            TrackAction::PromoteToStoryline(ti) => {
                // Find nearest primary clip as anchor (same logic as Shift+S in app.rs)
                let primary_idx = state.sequence().primary_track_index();
                if let Some(pi) = primary_idx {
                    let first_clip_in = state.sequence().tracks[ti].clips.first()
                        .map(|c| c.timeline_in);
                    if let Some(clip_in) = first_clip_in {
                        let anchor_id = state.sequence().tracks[pi].clips.iter()
                            .min_by_key(|c| {
                                let mid = c.timeline_in + (c.timeline_out - c.timeline_in);
                                let diff = mid - clip_in;
                                diff.value.unsigned_abs()
                            })
                            .map(|c| c.id);
                        if let Some(anchor_id) = anchor_id {
                            let result = klipp_timeline::magnetic::MagneticEditor::promote_to_storyline(
                                state.sequence_mut(), ti, anchor_id);
                            state.active_history_mut().push(crate::history::HistoryEntry {
                                command: crate::history::EditCommand::Magnetic(result.command),
                            });
                            state.dirty = true;
                            cmds.push(AppCommand::SequenceUpdated);
                        }
                    }
                }
            }
            TrackAction::BreakApartStoryline(ti) => {
                let result = klipp_timeline::magnetic::MagneticEditor::break_apart_storyline(
                    state.sequence_mut(), ti);
                state.active_history_mut().push(crate::history::HistoryEntry {
                    command: crate::history::EditCommand::Magnetic(result.command),
                });
                state.dirty = true;
                cmds.push(AppCommand::SequenceUpdated);
            }
            TrackAction::SetAsPrimary(ti) => {
                // Clear old primary, set new one
                for track in &mut state.sequence_mut().tracks {
                    if track.magnetic_role == klipp_timeline::TrackRole::Primary {
                        track.magnetic_role = klipp_timeline::TrackRole::Free;
                    }
                }
                state.sequence_mut().tracks[ti].magnetic_role = klipp_timeline::TrackRole::Primary;
                state.dirty = true;
                cmds.push(AppCommand::SequenceUpdated);
            }
        }
    }

    cmds
}
