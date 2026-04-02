use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// Convert linear volume (0.0..2.0) to decibels.
/// Returns -inf for 0.0, 0 dB for 1.0, +6 dB for 2.0.
fn vol_to_db(vol: f32) -> f32 {
    if vol <= 0.0001 {
        -60.0
    } else {
        20.0 * vol.log10()
    }
}

/// Format dB value for display.
fn format_db(db: f32) -> String {
    if db <= -59.0 {
        "-inf".to_string()
    } else {
        format!("{:+.1}", db)
    }
}

/// Width of a single fader strip.
const STRIP_W: f32 = 64.0;
/// Height of the vertical fader track.
const FADER_TRACK_H: f32 = 120.0;

/// Render a single vertical fader strip. Returns commands if values changed.
fn fader_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    track_index: usize,
    name: &str,
    volume: &mut f32,
    pan: &mut f32,
    muted: &mut bool,
    solo: &mut bool,
    cmds: &mut Vec<AppCommand>,
) {
    let strip_rect = ui.available_rect_before_wrap();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(STRIP_W, strip_rect.height()),
        egui::Sense::hover(),
    );

    let painter = ui.painter_at(rect);
    let cx = rect.center().x;
    let mut y = rect.min.y + 6.0;

    // -- Track name label --
    painter.text(
        egui::pos2(cx, y + 6.0),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::proportional(10.0),
        theme.text_primary,
    );
    y += 18.0;

    // -- Pan slider (horizontal) --
    let pan_w = STRIP_W - 12.0;
    let pan_rect = egui::Rect::from_min_size(egui::pos2(rect.min.x + 6.0, y), egui::vec2(pan_w, 14.0));

    // Pan track background
    painter.rect_filled(
        egui::Rect::from_min_size(egui::pos2(pan_rect.min.x, pan_rect.center().y - 1.5), egui::vec2(pan_w, 3.0)),
        1.5,
        theme.bg_raised,
    );
    // Pan center tick
    painter.line_segment(
        [egui::pos2(pan_rect.center().x, pan_rect.center().y - 4.0), egui::pos2(pan_rect.center().x, pan_rect.center().y + 4.0)],
        egui::Stroke::new(1.0, theme.border_light),
    );
    // Pan thumb position
    let pan_t = (*pan + 1.0) / 2.0; // 0..1
    let pan_x = pan_rect.min.x + pan_t * pan_w;
    painter.circle_filled(egui::pos2(pan_x, pan_rect.center().y), 4.0, theme.text_secondary);

    // Pan interaction
    let pan_resp = ui.allocate_rect(pan_rect.expand(2.0), egui::Sense::click_and_drag());
    if pan_resp.dragged() {
        let delta = pan_resp.drag_delta().x;
        *pan = (*pan + delta / (pan_w * 0.5)).clamp(-1.0, 1.0);
        cmds.push(AppCommand::SetTrackPan { track_index, pan: *pan });
    }
    if pan_resp.double_clicked() {
        *pan = 0.0;
        cmds.push(AppCommand::SetTrackPan { track_index, pan: 0.0 });
    }
    // Pan label
    let pan_label = if pan.abs() < 0.01 { "C".to_string() } else { format!("{:+.0}", *pan * 100.0) };
    painter.text(
        egui::pos2(cx, pan_rect.max.y + 1.0),
        egui::Align2::CENTER_TOP,
        &pan_label,
        egui::FontId::monospace(7.0),
        theme.text_dim,
    );
    y = pan_rect.max.y + 12.0;

    // -- Vertical volume fader --
    let fader_top = y;
    let fader_bottom = fader_top + FADER_TRACK_H;

    // Fader track background (thin vertical line)
    let track_rect = egui::Rect::from_min_max(
        egui::pos2(cx - 1.5, fader_top),
        egui::pos2(cx + 1.5, fader_bottom),
    );
    painter.rect_filled(track_rect, 1.0, theme.bg_raised);

    // dB scale marks on the left side
    let db_marks: &[(f32, &str)] = &[
        (2.0, "+6"),
        (1.0, "0"),
        (0.5, "-6"),
        (0.25, "-12"),
        (0.0, "-inf"),
    ];
    for &(vol_val, label) in db_marks {
        let t = vol_val / 2.0; // normalize to 0..1
        let mark_y = fader_bottom - t * FADER_TRACK_H;
        // Tick mark
        painter.line_segment(
            [egui::pos2(cx - 6.0, mark_y), egui::pos2(cx - 2.0, mark_y)],
            egui::Stroke::new(1.0, theme.border_light),
        );
        painter.text(
            egui::pos2(cx - 8.0, mark_y),
            egui::Align2::RIGHT_CENTER,
            label,
            egui::FontId::monospace(6.0),
            theme.text_dim,
        );
    }

    // Unity mark (0dB = volume 1.0) — prominent
    let unity_y = fader_bottom - (1.0 / 2.0) * FADER_TRACK_H;
    painter.line_segment(
        [egui::pos2(cx - 8.0, unity_y), egui::pos2(cx + 8.0, unity_y)],
        egui::Stroke::new(1.0, theme.accent_dim),
    );

    // Fill from bottom to thumb
    let vol_t = (*volume / 2.0).clamp(0.0, 1.0);
    let thumb_y = fader_bottom - vol_t * FADER_TRACK_H;
    let fill_color = if *muted {
        theme.text_dim
    } else if *volume > 1.0 {
        egui::Color32::from_rgb(0xc0, 0x40, 0x40) // red for above unity
    } else {
        theme.accent
    };
    if *volume > 0.001 {
        let fill_rect = egui::Rect::from_min_max(
            egui::pos2(cx - 1.5, thumb_y),
            egui::pos2(cx + 1.5, fader_bottom),
        );
        painter.rect_filled(fill_rect, 0.0, fill_color);
    }

    // Thumb (fader handle — wider than the track)
    let thumb_rect = egui::Rect::from_center_size(
        egui::pos2(cx, thumb_y),
        egui::vec2(16.0, 8.0),
    );
    painter.rect_filled(thumb_rect, 2.0, egui::Color32::from_rgb(0xd0, 0xd0, 0xd8));
    painter.rect_stroke(thumb_rect, 2.0, egui::Stroke::new(1.0, fill_color));
    // Center line on thumb
    painter.line_segment(
        [egui::pos2(cx - 5.0, thumb_y), egui::pos2(cx + 5.0, thumb_y)],
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0x60, 0x60, 0x68)),
    );

    // Fader interaction
    let fader_hit = egui::Rect::from_min_max(
        egui::pos2(cx - 12.0, fader_top - 4.0),
        egui::pos2(cx + 12.0, fader_bottom + 4.0),
    );
    let fader_resp = ui.allocate_rect(fader_hit, egui::Sense::click_and_drag());
    if fader_resp.dragged() {
        let delta = fader_resp.drag_delta().y;
        // Dragging UP increases volume
        let step = -delta / FADER_TRACK_H * 2.0;
        *volume = (*volume + step).clamp(0.0, 2.0);
        cmds.push(AppCommand::SetTrackVolume { track_index, volume: *volume });
    }
    if fader_resp.clicked() {
        if let Some(pos) = fader_resp.interact_pointer_pos() {
            let t = 1.0 - ((pos.y - fader_top) / FADER_TRACK_H).clamp(0.0, 1.0);
            *volume = t * 2.0;
            cmds.push(AppCommand::SetTrackVolume { track_index, volume: *volume });
        }
    }
    if fader_resp.double_clicked() {
        *volume = 1.0; // Reset to unity
        cmds.push(AppCommand::SetTrackVolume { track_index, volume: 1.0 });
    }

    y = fader_bottom + 4.0;

    // -- dB readout --
    let db = vol_to_db(*volume);
    let db_text = format_db(db);
    let db_color = if *muted { theme.text_dim } else { theme.text_secondary };
    painter.text(
        egui::pos2(cx, y + 5.0),
        egui::Align2::CENTER_CENTER,
        &format!("{} dB", db_text),
        egui::FontId::monospace(8.0),
        db_color,
    );
    y += 16.0;

    // -- Mute / Solo buttons --
    let btn_size = egui::vec2(22.0, 16.0);
    let btn_gap = 4.0;
    let btns_w = btn_size.x * 2.0 + btn_gap;
    let btn_left = cx - btns_w / 2.0;

    // Mute button
    let mute_rect = egui::Rect::from_min_size(egui::pos2(btn_left, y), btn_size);
    let mute_bg = if *muted {
        egui::Color32::from_rgb(0xc0, 0x30, 0x30) // red when active
    } else {
        theme.bg_raised
    };
    let mute_text_color = if *muted { egui::Color32::WHITE } else { theme.text_secondary };
    painter.rect_filled(mute_rect, 3.0, mute_bg);
    painter.rect_stroke(mute_rect, 3.0, egui::Stroke::new(1.0, theme.border));
    painter.text(
        mute_rect.center(),
        egui::Align2::CENTER_CENTER,
        "M",
        egui::FontId::proportional(9.0),
        mute_text_color,
    );
    let mute_resp = ui.allocate_rect(mute_rect, egui::Sense::click());
    if mute_resp.clicked() {
        *muted = !*muted;
        cmds.push(AppCommand::SetTrackMute { track_index, muted: *muted });
    }

    // Solo button
    let solo_rect = egui::Rect::from_min_size(egui::pos2(btn_left + btn_size.x + btn_gap, y), btn_size);
    let solo_bg = if *solo {
        theme.accent // amber when active
    } else {
        theme.bg_raised
    };
    let solo_text_color = if *solo { theme.text_on_accent } else { theme.text_secondary };
    painter.rect_filled(solo_rect, 3.0, solo_bg);
    painter.rect_stroke(solo_rect, 3.0, egui::Stroke::new(1.0, theme.border));
    painter.text(
        solo_rect.center(),
        egui::Align2::CENTER_CENTER,
        "S",
        egui::FontId::proportional(9.0),
        solo_text_color,
    );
    let solo_resp = ui.allocate_rect(solo_rect, egui::Sense::click());
    if solo_resp.clicked() {
        *solo = !*solo;
        cmds.push(AppCommand::SetTrackSolo { track_index, solo: *solo });
    }
}

/// Render the master fader strip.
fn master_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    master_volume: &mut f32,
    cmds: &mut Vec<AppCommand>,
) {
    let strip_rect = ui.available_rect_before_wrap();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(STRIP_W, strip_rect.height()),
        egui::Sense::hover(),
    );

    let painter = ui.painter_at(rect);
    let cx = rect.center().x;
    let mut y = rect.min.y + 6.0;

    // Separator line on left
    painter.line_segment(
        [egui::pos2(rect.min.x, rect.min.y + 4.0), egui::pos2(rect.min.x, rect.max.y - 4.0)],
        egui::Stroke::new(1.0, theme.border),
    );

    // -- "MASTER" label --
    painter.text(
        egui::pos2(cx, y + 6.0),
        egui::Align2::CENTER_CENTER,
        "MASTER",
        egui::FontId::proportional(9.0),
        theme.accent,
    );
    y += 30.0; // skip pan area (master has no pan)

    // -- Vertical volume fader --
    let fader_top = y;
    let fader_bottom = fader_top + FADER_TRACK_H;

    // Fader track background
    let track_rect = egui::Rect::from_min_max(
        egui::pos2(cx - 1.5, fader_top),
        egui::pos2(cx + 1.5, fader_bottom),
    );
    painter.rect_filled(track_rect, 1.0, theme.bg_raised);

    // Unity mark
    let unity_y = fader_bottom - (1.0 / 2.0) * FADER_TRACK_H;
    painter.line_segment(
        [egui::pos2(cx - 8.0, unity_y), egui::pos2(cx + 8.0, unity_y)],
        egui::Stroke::new(1.0, theme.accent_dim),
    );

    // Fill from bottom to thumb
    let vol_t = (*master_volume / 2.0).clamp(0.0, 1.0);
    let thumb_y = fader_bottom - vol_t * FADER_TRACK_H;
    let fill_color = if *master_volume > 1.0 {
        egui::Color32::from_rgb(0xc0, 0x40, 0x40)
    } else {
        theme.accent
    };
    if *master_volume > 0.001 {
        let fill_rect = egui::Rect::from_min_max(
            egui::pos2(cx - 1.5, thumb_y),
            egui::pos2(cx + 1.5, fader_bottom),
        );
        painter.rect_filled(fill_rect, 0.0, fill_color);
    }

    // Thumb
    let thumb_rect = egui::Rect::from_center_size(
        egui::pos2(cx, thumb_y),
        egui::vec2(18.0, 8.0),
    );
    painter.rect_filled(thumb_rect, 2.0, egui::Color32::from_rgb(0xd0, 0xd0, 0xd8));
    painter.rect_stroke(thumb_rect, 2.0, egui::Stroke::new(1.0, fill_color));
    painter.line_segment(
        [egui::pos2(cx - 6.0, thumb_y), egui::pos2(cx + 6.0, thumb_y)],
        egui::Stroke::new(1.0, egui::Color32::from_rgb(0x60, 0x60, 0x68)),
    );

    // Fader interaction
    let fader_hit = egui::Rect::from_min_max(
        egui::pos2(cx - 12.0, fader_top - 4.0),
        egui::pos2(cx + 12.0, fader_bottom + 4.0),
    );
    let fader_resp = ui.allocate_rect(fader_hit, egui::Sense::click_and_drag());
    if fader_resp.dragged() {
        let delta = fader_resp.drag_delta().y;
        let step = -delta / FADER_TRACK_H * 2.0;
        *master_volume = (*master_volume + step).clamp(0.0, 2.0);
        cmds.push(AppCommand::SetMasterVolume(*master_volume));
    }
    if fader_resp.clicked() {
        if let Some(pos) = fader_resp.interact_pointer_pos() {
            let t = 1.0 - ((pos.y - fader_top) / FADER_TRACK_H).clamp(0.0, 1.0);
            *master_volume = t * 2.0;
            cmds.push(AppCommand::SetMasterVolume(*master_volume));
        }
    }
    if fader_resp.double_clicked() {
        *master_volume = 1.0;
        cmds.push(AppCommand::SetMasterVolume(1.0));
    }

    y = fader_bottom + 4.0;

    // dB readout
    let db = vol_to_db(*master_volume);
    let db_text = format_db(db);
    painter.text(
        egui::pos2(cx, y + 5.0),
        egui::Align2::CENTER_CENTER,
        &format!("{} dB", db_text),
        egui::FontId::monospace(8.0),
        theme.text_secondary,
    );
}

/// Show the audio mixer panel. Returns commands for track audio changes.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let full_rect = ui.max_rect();

    // Background
    ui.painter().rect_filled(full_rect, 0.0, theme.bg_deep);
    // Top border
    ui.painter().hline(
        full_rect.x_range(),
        full_rect.min.y,
        egui::Stroke::new(1.0, theme.border),
    );

    // Header bar
    let header_h = 20.0;
    let header_rect = egui::Rect::from_min_size(full_rect.min, egui::vec2(full_rect.width(), header_h));
    ui.painter().rect_filled(header_rect, 0.0, theme.bg_panel);
    ui.painter().text(
        egui::pos2(header_rect.min.x + 10.0, header_rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Audio Mixer",
        egui::FontId::proportional(10.0),
        theme.text_secondary,
    );
    // Bottom border of header
    ui.painter().hline(
        full_rect.x_range(),
        header_rect.max.y,
        egui::Stroke::new(1.0, theme.border),
    );

    // Content area below header
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(full_rect.min.x, header_rect.max.y),
        full_rect.max,
    );

    // Gather track info from active sequence
    let seq = &mut state.sequences[state.active_sequence_idx];
    let track_count = seq.tracks.len();

    // Collect track data we need (to avoid borrow issues)
    let mut track_data: Vec<(String, f32, f32, bool, bool)> = seq.tracks.iter()
        .map(|t| (t.name.clone(), t.volume, t.pan, t.muted, t.solo))
        .collect();
    let mut master_vol = state.master_volume;

    // Render strips in a horizontal layout
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content_rect), |ui| {
        ui.set_min_size(content_rect.size());

        // Use a horizontal layout with scroll if needed
        egui::ScrollArea::horizontal()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(4.0);

                    for i in 0..track_count {
                        let (ref name, ref mut vol, ref mut pan, ref mut muted, ref mut solo) = track_data[i];
                        fader_strip(
                            ui, theme, i, name,
                            vol, pan, muted, solo,
                            &mut cmds,
                        );
                        // Thin separator between strips
                        let sep_rect = ui.available_rect_before_wrap();
                        ui.painter().line_segment(
                            [egui::pos2(sep_rect.min.x, content_rect.min.y + 4.0),
                             egui::pos2(sep_rect.min.x, content_rect.max.y - 4.0)],
                            egui::Stroke::new(1.0, theme.border),
                        );
                    }

                    // Master fader on the right
                    master_strip(ui, theme, &mut master_vol, &mut cmds);
                });
            });
    });

    // Write back track data changes (for immediate visual feedback)
    let seq = &mut state.sequences[state.active_sequence_idx];
    for (i, (_, vol, pan, muted, solo)) in track_data.iter().enumerate() {
        if i < seq.tracks.len() {
            seq.tracks[i].volume = *vol;
            seq.tracks[i].pan = *pan;
            seq.tracks[i].muted = *muted;
            seq.tracks[i].solo = *solo;
        }
    }
    state.master_volume = master_vol;

    cmds
}
