use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::tooltip::styled_tooltip;
use klipp_timeline::TrackKind;

/// Render the 110px track header column: ruler spacer + per-track headers.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    header_rect: egui::Rect,
) {
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

    // ── Track headers ──
    let mut y = header_rect.min.y + ruler_h;
    let mut toggle_mute: Option<usize> = None;
    let mut toggle_lock: Option<usize> = None;

    for (track_idx, track) in state.sequence.tracks.iter().enumerate() {
        let track_h = match track.kind {
            TrackKind::Video => crate::theme::TRACK_VIDEO_H,
            TrackKind::Audio => crate::theme::TRACK_AUDIO_H,
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

        // Track name
        let name_x = track_rect.min.x + crate::theme::CLIP_ROLE_BAR_W + 6.0;
        ui.painter().text(
            egui::pos2(name_x, track_rect.center().y),
            egui::Align2::LEFT_CENTER,
            &track.name,
            egui::FontId::proportional(10.0),
            theme.text_secondary,
        );

        // Mute button (right side, 16x16)
        let btn_size = 16.0;
        let mute_rect = egui::Rect::from_min_size(
            egui::pos2(
                track_rect.max.x - btn_size * 2.0 - 8.0,
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
        ui.painter().text(
            mute_rect.center(),
            egui::Align2::CENTER_CENTER,
            if mute_active { "\u{1F507}" } else { "\u{1F50A}" },
            egui::FontId::proportional(9.0),
            mute_color,
        );
        styled_tooltip(&mute_resp, theme, "Mute track", None);
        if mute_resp.clicked() {
            toggle_mute = Some(track_idx);
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
        ui.painter().text(
            lock_rect.center(),
            egui::Align2::CENTER_CENTER,
            if lock_active { "\u{1F512}" } else { "\u{1F513}" },
            egui::FontId::proportional(9.0),
            lock_color,
        );
        styled_tooltip(&lock_resp, theme, "Lock track", None);
        if lock_resp.clicked() {
            toggle_lock = Some(track_idx);
        }

        y += track_h;
    }

    // Apply deferred toggles
    if let Some(idx) = toggle_mute {
        state.sequence.tracks[idx].muted = !state.sequence.tracks[idx].muted;
    }
    if let Some(idx) = toggle_lock {
        state.sequence.tracks[idx].locked = !state.sequence.tracks[idx].locked;
    }
}
