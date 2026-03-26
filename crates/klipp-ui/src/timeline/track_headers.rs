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
        egui::Stroke::new(1.0, theme.border),
    );
    ui.painter().text(
        egui::pos2(spacer_rect.min.x + 6.0, spacer_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "TRACKS",
        egui::FontId::monospace(9.0),
        theme.text_dim,
    );

    let mut cmds = Vec::new();

    // ── Track headers ──
    let mut y = header_rect.min.y + ruler_h;
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

    for (track_idx, track) in state.sequence().tracks.iter().enumerate() {
        let track_h = match track.kind {
            TrackKind::Audio => crate::theme::TRACK_AUDIO_H,
            _ => crate::theme::TRACK_VIDEO_H,
        };

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
            egui::Stroke::new(1.0, theme.border),
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

        // Track name
        let name_x = track_rect.min.x + crate::theme::CLIP_ROLE_BAR_W + 6.0;
        ui.painter().text(
            egui::pos2(name_x, track_rect.center().y),
            egui::Align2::LEFT_CENTER,
            &track.name,
            egui::FontId::proportional(10.0),
            theme.text_secondary,
        );

        // Magnetic role badge (small pill after track name)
        let role_badge = match track.magnetic_role {
            klipp_timeline::TrackRole::Primary => Some(("P", egui::Color32::from_rgb(60, 130, 220))),
            klipp_timeline::TrackRole::Connected => Some(("C", egui::Color32::from_rgb(200, 150, 40))),
            klipp_timeline::TrackRole::ConnectedStoryline => Some(("CS", egui::Color32::from_rgb(180, 120, 200))),
            klipp_timeline::TrackRole::Free => None,
        };
        if let Some((label, color)) = role_badge {
            let badge_w = if label.len() > 1 { 18.0 } else { 14.0 };
            let badge_rect = egui::Rect::from_min_size(
                egui::pos2(name_x + 2.0, track_rect.center().y + 6.0),
                egui::vec2(badge_w, 10.0),
            );
            ui.painter().rect_filled(badge_rect, 3.0, color);
            ui.painter().text(
                badge_rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::monospace(7.0),
                egui::Color32::WHITE,
            );
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
                        1.0, egui::Color32::from_rgb(25, 25, 30),
                    );
                    // Fill
                    let fill_w = (peak.min(1.0) * meter_w).max(0.0);
                    if fill_w > 0.0 {
                        let color = if peak > 0.9 { egui::Color32::from_rgb(220, 50, 50) }
                            else if peak > 0.7 { egui::Color32::from_rgb(220, 180, 40) }
                            else { egui::Color32::from_rgb(50, 180, 70) };
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

        // Mute button (right side, 16x16)
        let btn_size = 16.0;
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
            egui::Color32::from_rgb(200, 170, 40) // bright gold when active
        } else if solo_resp.hovered() {
            theme.bg_raised
        } else {
            egui::Color32::TRANSPARENT
        };
        let solo_text_color = if solo_active { egui::Color32::from_rgb(30, 25, 10) } else { theme.text_dim };
        ui.painter().rect_filled(solo_rect, 3.0, solo_bg);
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
        header_resp.context_menu(|ui| {
            ui.set_min_width(180.0);
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
                klipp_timeline::TrackRole::Primary => {
                    // No storyline options for primary track
                }
            }
        });

        y += track_h;
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
            TrackAction::BreakApartStoryline(ti) => {
                let result = klipp_timeline::magnetic::MagneticEditor::break_apart_storyline(
                    state.sequence_mut(), ti);
                state.history.push(crate::history::HistoryEntry {
                    sequence_idx: state.active_sequence_idx,
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
