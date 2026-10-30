use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

// ═══════════════════════════════════════════════════════════════════
// Constants
// ═══════════════════════════════════════════════════════════════════

/// Width of a single fader strip (fader + peak meter).
const STRIP_W: f32 = 100.0;
/// Width of the master section (fader + peak bars + LUFS meter + gap).
const MASTER_W: f32 = 176.0;
/// The shortest a fader track gets before the mixer scrolls instead.
const FADER_MIN_H: f32 = 60.0;
/// The tallest a fader track grows in a tall panel.
const FADER_MAX_H: f32 = 300.0;
/// Width of a single peak-meter bar (L or R).
const PEAK_BAR_W: f32 = 6.0;
/// Width of the LUFS meter bar.
const LUFS_BAR_W: f32 = 18.0;
/// Gap between fader and meter bars.
const METER_GAP: f32 = 5.0;
/// Horizontal padding inside each strip.
const STRIP_PAD: f32 = 6.0;
/// Gap between bottom of meter bars and text readouts.
const READOUT_GAP: f32 = 8.0;
/// Bottom padding inside each strip.
const STRIP_BOTTOM_PAD: f32 = 14.0;
/// A track strip's height less its fader: mode, name, pan, readout,
/// mute and solo.
const STRIP_FIXED_H: f32 = 52.0 + READOUT_GAP + 16.0 + 16.0 + SENDS_ROW_H + STRIP_BOTTOM_PAD;
/// The row under a strip's mute and solo that opens its sends (#2861).
const SENDS_ROW_H: f32 = 20.0;
/// The master strip's height less its fader: label, meter scale,
/// four readout rows and the reset button.
const MASTER_FIXED_H: f32 = 40.0 + READOUT_GAP + 43.0 + 14.0 + STRIP_BOTTOM_PAD;
/// The shortest strip: below it the panel scrolls rather than hide a
/// strip's mute and solo (#2693).
pub(crate) const STRIP_MIN_H: f32 = FADER_MIN_H + if STRIP_FIXED_H > MASTER_FIXED_H { STRIP_FIXED_H } else { MASTER_FIXED_H };
/// Room under the strips for the horizontal scroll bar.
const SCROLLBAR_ROOM: f32 = 8.0;
/// The mixer header's height.
const HEADER_H: f32 = 20.0;
/// The panel height that shows every strip whole without scrolling.
pub(crate) const MIXER_MIN_H: f32 = HEADER_H + STRIP_MIN_H + SCROLLBAR_ROOM;

/// Whether a track can be heard, and so has a strip: an audio track, or
/// a video track with a clip that plays its own sound there. A video
/// clip whose linked partner sits on an audio track is heard from that
/// track instead, as the engine plays it.
pub(crate) fn is_heard(seq: &klipp_timeline::Sequence, track: &klipp_timeline::Track, pool: &klipp_media::MediaPool) -> bool {
    use klipp_timeline::TrackKind;
    match track.kind {
        TrackKind::Audio => true,
        TrackKind::Video => track.clips.iter().any(|clip| {
            let has_sound = clip.media_id().and_then(|id| pool.get(&id)).is_some_and(|item| item.has_audio);
            let partner_on_audio = clip.linked_clip_id.is_some_and(|lid| {
                seq.tracks.iter().any(|t| t.kind == TrackKind::Audio && t.clips.iter().any(|c| c.id == lid))
            });
            has_sound && !partner_on_audio
        }),
        TrackKind::Text | TrackKind::Subtitle => false,
    }
}

/// A strip's fader track, fitted to the strip's height.
fn fader_height(strip_h: f32, fixed_h: f32) -> f32 {
    (strip_h - fixed_h).clamp(FADER_MIN_H, FADER_MAX_H)
}

// ═══════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════

fn vol_to_db(vol: f32) -> f32 {
    if vol <= 0.0001 { -60.0 } else { 20.0 * vol.log10() }
}

fn format_db(db: f32) -> String {
    if db <= -59.0 { "-inf".to_string() } else { format!("{:+.1}", db) }
}

fn format_lufs(v: f32) -> String {
    if v.is_finite() { format!("{:.1}", v) } else { "\u{2212}\u{221e}".to_string() }
}

/// Map a dB value to a 0..1 position on the meter.
/// -60 dB = 0.0, 0 dB = 0.909, +6 dB = 1.0.
fn db_to_t(db: f32) -> f32 {
    ((db + 60.0) / 66.0).clamp(0.0, 1.0)
}

/// Map LUFS to 0..1 position. -60 LUFS = 0.0, 0 LUFS = 1.0.
fn lufs_to_t(lufs: f32) -> f32 {
    if !lufs.is_finite() { return 0.0; }
    ((lufs + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Color for a LUFS level relative to target (for bars).
fn lufs_bar_color(lufs: f32, target: f32, theme: &KlippTheme) -> egui::Color32 {
    if !lufs.is_finite() {
        return theme.bg_raised;
    }
    let diff = lufs - target;
    if diff > 2.0 {
        theme.marker_error
    } else if diff >= -2.0 {
        theme.marker_success
    } else if diff >= -8.0 {
        theme.marker_warning
    } else {
        theme.marker_info
    }
}

/// Color for LUFS text readout.
fn lufs_text_color(lufs: f32, target: f32, theme: &KlippTheme) -> egui::Color32 {
    if !lufs.is_finite() {
        return theme.text_dim;
    }
    let diff = lufs - target;
    if diff > 2.0 {
        theme.marker_error
    } else if diff.abs() <= 2.0 {
        theme.marker_success
    } else {
        theme.marker_warning
    }
}

// ═══════════════════════════════════════════════════════════════════
// Peak meter bar
// ═══════════════════════════════════════════════════════════════════

/// A bus's left and right meters, side by side from `x` (#2861).
pub(crate) fn draw_bus_meters(painter: &egui::Painter, x: f32, top: f32, bottom: f32, left: f32, right: f32, theme: &KlippTheme) {
    draw_peak_bar(painter, x, top, bottom, PEAK_BAR_W, left, theme);
    draw_peak_bar(painter, x + PEAK_BAR_W + 1.0, top, bottom, PEAK_BAR_W, right, theme);
}

fn draw_peak_bar(
    painter: &egui::Painter,
    x: f32,
    top: f32,
    bottom: f32,
    bar_w: f32,
    peak_linear: f32,
    theme: &KlippTheme,
) {
    let h = bottom - top;
    let bar_rect = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(bar_w, h));
    // Dark fill + visible border so empty meters are still visible
    painter.rect_filled(bar_rect, 1.0, theme.bg_deep);
    painter.rect_stroke(bar_rect, 1.0, egui::Stroke::new(0.5_f32, theme.bg_raised));

    let db = vol_to_db(peak_linear);
    let t = db_to_t(db);
    if t <= 0.0 { return; }

    let zones: &[(f32, f32, egui::Color32)] = &[
        (0.0, db_to_t(-6.0), theme.marker_success),
        (db_to_t(-6.0), db_to_t(-1.0), theme.marker_warning),
        (db_to_t(-1.0), 1.0, theme.marker_error),
    ];

    for &(zone_start, zone_end, color) in zones {
        let seg_start = zone_start.max(0.0);
        let seg_end = zone_end.min(t);
        if seg_end <= seg_start { continue; }
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x, bottom - seg_end * h),
                egui::pos2(x + bar_w, bottom - seg_start * h),
            ),
            0.0, color,
        );
    }

    // Peak hold line
    let hold_y = bottom - t * h;
    painter.line_segment(
        [egui::pos2(x, hold_y), egui::pos2(x + bar_w, hold_y)],
        egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
    );
}

// ═══════════════════════════════════════════════════════════════════
// LUFS meter bar
// ═══════════════════════════════════════════════════════════════════

fn draw_lufs_bar(
    painter: &egui::Painter,
    x: f32,
    top: f32,
    bottom: f32,
    bar_w: f32,
    momentary: f32,
    short_term: f32,
    integrated: f32,
    true_peak_dbfs: f32,
    target: f32,
    theme: &KlippTheme,
) {
    let h = bottom - top;
    let bar_rect = egui::Rect::from_min_size(egui::pos2(x, top), egui::vec2(bar_w, h));
    painter.rect_filled(bar_rect, 1.0, theme.bg_deep);
    painter.rect_stroke(bar_rect, 1.0, egui::Stroke::new(0.5_f32, theme.bg_raised));

    // Green target zone (subtle background)
    let on_lo = lufs_to_t(target - 2.0);
    let on_hi = lufs_to_t(target + 2.0);
    if on_hi > on_lo {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x, bottom - on_hi * h),
                egui::pos2(x + bar_w, bottom - on_lo * h),
            ),
            0.0, theme.marker_success.linear_multiply(0.10),
        );
    }

    // Integrated (widest, dimmest)
    let i_t = lufs_to_t(integrated);
    if i_t > 0.0 {
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(x, bottom - i_t * h), egui::pos2(x + bar_w, bottom)),
            0.0, lufs_bar_color(integrated, target, theme).linear_multiply(0.35),
        );
    }

    // Short-term (medium)
    let st_t = lufs_to_t(short_term);
    if st_t > 0.0 {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x + 1.0, bottom - st_t * h),
                egui::pos2(x + bar_w - 1.0, bottom),
            ),
            0.0, lufs_bar_color(short_term, target, theme).linear_multiply(0.55),
        );
    }

    // Momentary (narrow center, brightest)
    let m_t = lufs_to_t(momentary);
    if m_t > 0.0 {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x + 3.0, bottom - m_t * h),
                egui::pos2(x + bar_w - 3.0, bottom),
            ),
            0.0, lufs_bar_color(momentary, target, theme).linear_multiply(0.85),
        );
    }

    // Target line
    let target_t = lufs_to_t(target);
    if target_t > 0.0 && target_t < 1.0 {
        let ty = bottom - target_t * h;
        painter.line_segment(
            [egui::pos2(x - 1.0, ty), egui::pos2(x + bar_w + 1.0, ty)],
            egui::Stroke::new(1.0_f32, theme.marker_success),
        );
    }

    // True peak indicator tick on right
    if true_peak_dbfs.is_finite() {
        let tp_t = db_to_t(true_peak_dbfs);
        if tp_t > 0.0 {
            let tp_y = bottom - tp_t * h;
            let tp_color = if true_peak_dbfs > -1.0 {
                theme.marker_error
            } else { theme.text_secondary };
            painter.line_segment(
                [egui::pos2(x + bar_w, tp_y), egui::pos2(x + bar_w + 3.0, tp_y)],
                egui::Stroke::new(2.0_f32, tp_color),
            );
        }
    }

    // Scale ticks on left
    for &lufs_val in &[-48.0_f32, -36.0, -24.0, -18.0, -12.0, -6.0, 0.0] {
        let tick_t = lufs_to_t(lufs_val);
        let tick_y = bottom - tick_t * h;
        painter.line_segment(
            [egui::pos2(x - 2.0, tick_y), egui::pos2(x, tick_y)],
            egui::Stroke::new(1.0_f32, theme.border),
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// Track fader strip
// ═══════════════════════════════════════════════════════════════════

/// A strip's automation (#2306): its track's mode, none for a track that
/// isn't an audio track (only those are automated); the volume and pan it
/// shows in place of the controls as set, moved or from the keys at the
/// playhead; and whether a move of each is live rather than set.
pub(crate) struct StripAutomation {
    pub mode: Option<klipp_core::automation::AutomationMode>,
    pub shown_volume: Option<f32>,
    pub shown_pan: Option<f32>,
    pub live_volume: bool,
    pub live_pan: bool,
}

/// Whether a move of `param` on `track` is live, played over its keys
/// and recorded by a pass, rather than setting the control (#2306): it
/// plays from keys, or playback is on and the mode records. Only an audio
/// track's are.
pub(crate) fn goes_live(track: &klipp_timeline::Track, param: klipp_core::automation::TrackParam, playing: bool) -> bool {
    use klipp_core::automation::AutomationMode;
    if track.kind != klipp_timeline::TrackKind::Audio {
        return false;
    }
    track.automation.plays(param) || playing && matches!(track.automation.mode, AutomationMode::Touch | AutomationMode::Latch | AutomationMode::Write)
}

/// A control moved to `value`: live, for the pass and the engine, or set,
/// one undoable step as before.
fn set_control(
    track_index: usize,
    param: klipp_core::automation::TrackParam,
    value: f32,
    live: bool,
    target: &mut f32,
    lives: &mut Vec<(klipp_core::automation::TrackParam, f32)>,
    cmds: &mut Vec<AppCommand>,
) {
    use klipp_core::automation::TrackParam;
    if live {
        lives.push((param, value));
        return;
    }
    *target = value;
    cmds.push(match param {
        TrackParam::Volume => AppCommand::SetTrackVolume { track_index, volume: value },
        TrackParam::Pan => AppCommand::SetTrackPan { track_index, pan: value },
    });
}

#[allow(clippy::too_many_arguments)]
fn fader_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    strip_h: f32,
    track_index: usize,
    name: &str,
    track_id: klipp_core::TrackId,
    volume: &mut f32,
    pan: &mut f32,
    muted: &mut bool,
    solo: &mut bool,
    peak_l: f32,
    peak_r: f32,
    editing_name: &mut Option<(usize, String)>,
    automation: &StripAutomation,
    touches: &mut Vec<(klipp_core::automation::TrackParam, bool)>,
    lives: &mut Vec<(klipp_core::automation::TrackParam, f32)>,
    sends: usize,
    cmds: &mut Vec<AppCommand>,
) -> egui::Response {
    use klipp_core::automation::{AutomationMode, TrackParam};
    // What the controls show: as moved, or the keys' values when played.
    let shown_pan = automation.shown_pan.unwrap_or(*pan);
    let shown_volume = automation.shown_volume.unwrap_or(*volume);
    let fader_h = fader_height(strip_h, STRIP_FIXED_H);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(STRIP_W, strip_h),
        egui::Sense::hover(),
    );

    let painter = ui.painter_at(rect);
    // Strip background + border (consistent with master)
    painter.rect_filled(rect, 2.0, theme.bg_panel);
    painter.rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, theme.border));

    let inner_left = rect.min.x + STRIP_PAD;
    let inner_right = rect.max.x - STRIP_PAD;
    let inner_w = inner_right - inner_left;
    let fader_cx = inner_left + 22.0;
    let meter_x = inner_right - PEAK_BAR_W * 2.0 - 3.0;
    let mut y = rect.min.y + 10.0;

    // -- Automation mode (#2306): a click steps through them --
    if let Some(mode) = automation.mode {
        let mode_rect = egui::Rect::from_min_size(egui::pos2(inner_right - 28.0, rect.min.y + 1.0), egui::vec2(28.0, 9.0));
        let mode_resp = ui
            .allocate_rect(mode_rect, egui::Sense::click())
            .on_hover_text("Automation: Off (the controls as set), Read (play the track's keys), Touch (record while a control is held), Latch (record from its first touch until playback stops), Write (record the whole pass). Click for the next.");
        let recording = matches!(mode, AutomationMode::Touch | AutomationMode::Latch | AutomationMode::Write);
        let mode_colour = if recording { theme.marker_error } else if mode_resp.hovered() { theme.text_primary } else { theme.text_dim };
        painter.text(mode_rect.right_center(), egui::Align2::RIGHT_CENTER, mode.label(), egui::FontId::proportional(8.0), mode_colour);
        if mode_resp.clicked() {
            let all = AutomationMode::ALL;
            let next = all[(all.iter().position(|m| *m == mode).unwrap_or(0) + 1) % all.len()];
            cmds.push(AppCommand::SetTrackAutomationMode { track_index, mode: next });
        }
    }

    // -- Track name (double-click to rename) --
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(inner_left, y),
        egui::vec2(inner_w, 14.0),
    );

    let is_editing = editing_name.as_ref().is_some_and(|(idx, _)| *idx == track_index);
    if is_editing {
        let edit_buf = &mut editing_name.as_mut().unwrap().1;
        let resp = ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(name_rect),
            |ui| {
                let te = egui::TextEdit::singleline(edit_buf)
                    .font(egui::FontId::proportional(10.0))
                    .desired_width(STRIP_W - 8.0)
                    .frame(true);
                ui.add(te)
            },
        ).inner;
        // Submit on enter or focus lost
        if resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let new_name = editing_name.take().unwrap().1;
            if !new_name.is_empty() {
                cmds.push(AppCommand::RenameTrack { track_id, name: new_name });
            }
        }
    } else {
        painter.text(
            egui::pos2(rect.center().x, y + 6.0),
            egui::Align2::CENTER_CENTER,
            name,
            egui::FontId::proportional(10.0),
            theme.text_primary,
        );
        let name_resp = ui.allocate_rect(name_rect, egui::Sense::click());
        if name_resp.double_clicked() {
            *editing_name = Some((track_index, name.to_string()));
        }
    }
    y += 16.0;

    // -- Pan slider (horizontal, prominent) --
    let pan_w = inner_w - 4.0;
    let pan_rect = egui::Rect::from_min_size(egui::pos2(inner_left + 2.0, y), egui::vec2(pan_w, 16.0));
    // Track groove
    let groove = egui::Rect::from_min_size(
        egui::pos2(pan_rect.min.x, pan_rect.center().y - 2.0),
        egui::vec2(pan_w, 4.0),
    );
    painter.rect_filled(groove, 2.0, theme.bg_raised);
    painter.rect_stroke(groove, 2.0, egui::Stroke::new(0.5_f32, theme.border_light));
    // Center tick
    painter.line_segment(
        [egui::pos2(pan_rect.center().x, pan_rect.center().y - 5.0),
         egui::pos2(pan_rect.center().x, pan_rect.center().y + 5.0)],
        egui::Stroke::new(1.0_f32, theme.border_light),
    );
    // Fill from center to thumb
    let pan_t = (shown_pan + 1.0) / 2.0;
    let pan_x = pan_rect.min.x + pan_t * pan_w;
    let center_x = pan_rect.center().x;
    let fill_l = center_x.min(pan_x);
    let fill_r = center_x.max(pan_x);
    if (fill_r - fill_l) > 1.0 {
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(fill_l, pan_rect.center().y - 1.5),
                egui::pos2(fill_r, pan_rect.center().y + 1.5),
            ),
            1.0, theme.accent.linear_multiply(0.5),
        );
    }
    // Thumb (rounded rect, bigger)
    let thumb_w = 10.0;
    let thumb_h = 12.0;
    let thumb_rect = egui::Rect::from_center_size(
        egui::pos2(pan_x, pan_rect.center().y),
        egui::vec2(thumb_w, thumb_h),
    );
    painter.rect_filled(thumb_rect, 2.0, theme.text_secondary);
    painter.rect_stroke(thumb_rect, 2.0, egui::Stroke::new(0.5_f32, theme.border));
    // Pan interaction
    let pan_resp = ui.allocate_rect(pan_rect.expand(2.0), egui::Sense::click_and_drag());
    // Held, the pan is recorded by the track's mode (#2306), moving from
    // where it shows.
    if pan_resp.drag_started() {
        touches.push((TrackParam::Pan, true));
    }
    if pan_resp.drag_stopped() {
        touches.push((TrackParam::Pan, false));
    }
    let mut moved_pan = None;
    if pan_resp.dragged() {
        moved_pan = Some((shown_pan + pan_resp.drag_delta().x / (pan_w * 0.5)).clamp(-1.0, 1.0));
    }
    if pan_resp.double_clicked() {
        moved_pan = Some(0.0);
    }
    if let Some(v) = moved_pan {
        set_control(track_index, TrackParam::Pan, v, automation.live_pan, pan, lives, cmds);
    }
    // Pan label below
    let pan_label = if shown_pan.abs() < 0.01 { "C".to_string() } else { format!("{:+.0}", shown_pan * 100.0) };
    painter.text(
        egui::pos2(rect.center().x, pan_rect.max.y + 2.0),
        egui::Align2::CENTER_TOP, &pan_label,
        egui::FontId::monospace(10.0), theme.text_dim,
    );
    y = pan_rect.max.y + 10.0;

    // -- Vertical volume fader --
    let fader_top = y;
    let fader_bottom = fader_top + fader_h;

    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(fader_cx - 1.5, fader_top), egui::pos2(fader_cx + 1.5, fader_bottom)),
        1.0, theme.bg_raised,
    );

    // dB scale marks
    for &(vol_val, label) in &[(2.0_f32, "+6"), (1.0, "0"), (0.5, "-6"), (0.25, "-12"), (0.0, "-inf")] {
        let t = vol_val / 2.0;
        let mark_y = fader_bottom - t * fader_h;
        painter.line_segment(
            [egui::pos2(fader_cx - 6.0, mark_y), egui::pos2(fader_cx - 2.0, mark_y)],
            egui::Stroke::new(1.0_f32, theme.border_light),
        );
        painter.text(
            egui::pos2(fader_cx - 8.0, mark_y), egui::Align2::RIGHT_CENTER,
            label, egui::FontId::monospace(6.0), theme.text_dim,
        );
    }

    // Unity mark
    let unity_y = fader_bottom - 0.5 * fader_h;
    painter.line_segment(
        [egui::pos2(fader_cx - 8.0, unity_y), egui::pos2(fader_cx + 8.0, unity_y)],
        egui::Stroke::new(1.0_f32, theme.accent_dim),
    );

    // Fill + thumb
    let vol_t = (shown_volume / 2.0).clamp(0.0, 1.0);
    let thumb_y = fader_bottom - vol_t * fader_h;
    let fill_color = if *muted { theme.text_dim } else if shown_volume > 1.0 {
        theme.marker_error
    } else { theme.accent };
    if shown_volume > 0.001 {
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(fader_cx - 1.5, thumb_y), egui::pos2(fader_cx + 1.5, fader_bottom)),
            0.0, fill_color,
        );
    }
    let thumb_rect = egui::Rect::from_center_size(egui::pos2(fader_cx, thumb_y), egui::vec2(16.0, 8.0));
    painter.rect_filled(thumb_rect, 2.0, theme.text_primary);
    painter.rect_stroke(thumb_rect, 2.0, egui::Stroke::new(1.0_f32, fill_color));
    painter.line_segment(
        [egui::pos2(fader_cx - 5.0, thumb_y), egui::pos2(fader_cx + 5.0, thumb_y)],
        egui::Stroke::new(1.0_f32, theme.text_dim),
    );

    // Fader interaction
    let fader_hit = egui::Rect::from_min_max(
        egui::pos2(fader_cx - 12.0, fader_top - 4.0),
        egui::pos2(fader_cx + 12.0, fader_bottom + 4.0),
    );
    let fader_resp = ui.allocate_rect(fader_hit, egui::Sense::click_and_drag());
    // Held, the fader is recorded by the track's mode (#2306), moving from
    // where it shows.
    if fader_resp.drag_started() {
        touches.push((TrackParam::Volume, true));
    }
    if fader_resp.drag_stopped() {
        touches.push((TrackParam::Volume, false));
    }
    let mut moved_volume = None;
    if fader_resp.dragged() {
        let step = -fader_resp.drag_delta().y / fader_h * 2.0;
        moved_volume = Some((shown_volume + step).clamp(0.0, 2.0));
    }
    if fader_resp.clicked() {
        if let Some(pos) = fader_resp.interact_pointer_pos() {
            let t = 1.0 - ((pos.y - fader_top) / fader_h).clamp(0.0, 1.0);
            moved_volume = Some(t * 2.0);
        }
    }
    if fader_resp.double_clicked() {
        moved_volume = Some(1.0);
    }
    if let Some(v) = moved_volume {
        set_control(track_index, TrackParam::Volume, v, automation.live_volume, volume, lives, cmds);
    }

    // -- Peak meter bars (right side) --
    draw_peak_bar(&painter, meter_x, fader_top, fader_bottom, PEAK_BAR_W, peak_l, theme);
    draw_peak_bar(&painter, meter_x + PEAK_BAR_W + 2.0, fader_top, fader_bottom, PEAK_BAR_W, peak_r, theme);

    // Meter scale ticks (between fader and meter bars)
    let tick_x = meter_x - 2.0;
    for &db_val in &[0.0_f32, -6.0, -12.0, -24.0, -48.0] {
        let tick_t = db_to_t(db_val);
        let tick_y = fader_bottom - tick_t * fader_h;
        painter.line_segment(
            [egui::pos2(tick_x - 2.0, tick_y), egui::pos2(tick_x, tick_y)],
            egui::Stroke::new(1.0_f32, theme.border),
        );
    }

    y = fader_bottom + READOUT_GAP;

    // -- dB readout --
    let db = vol_to_db(shown_volume);
    let db_color = if *muted { theme.text_dim } else { theme.text_secondary };
    painter.text(
        egui::pos2(rect.center().x, y + 5.0), egui::Align2::CENTER_CENTER,
        format!("{} dB", format_db(db)), egui::FontId::monospace(10.0), db_color,
    );
    y += 16.0;

    // -- Mute / Solo buttons --
    let btn_size = egui::vec2(22.0, 16.0);
    let btn_gap = 4.0;
    let btns_w = btn_size.x * 2.0 + btn_gap;
    let btn_left = rect.center().x - btns_w / 2.0;

    // Mute
    let mute_rect = egui::Rect::from_min_size(egui::pos2(btn_left, y), btn_size);
    let mute_bg = if *muted { theme.marker_error } else { theme.bg_raised };
    let mute_tc = if *muted { egui::Color32::WHITE } else { theme.text_secondary };
    painter.rect_filled(mute_rect, 2.0, mute_bg);
    painter.rect_stroke(mute_rect, 2.0, egui::Stroke::new(1.0_f32, theme.border));
    painter.text(mute_rect.center(), egui::Align2::CENTER_CENTER, "M", egui::FontId::proportional(10.0), mute_tc);
    let mute_resp = ui.allocate_rect(mute_rect, egui::Sense::click());
    if mute_resp.clicked() {
        *muted = !*muted;
        cmds.push(AppCommand::SetTrackMute { track_index, muted: *muted });
    }

    // Solo
    let solo_rect = egui::Rect::from_min_size(egui::pos2(btn_left + btn_size.x + btn_gap, y), btn_size);
    let solo_bg = if *solo { theme.accent } else { theme.bg_raised };
    let solo_tc = if *solo { theme.text_on_accent } else { theme.text_secondary };
    painter.rect_filled(solo_rect, 2.0, solo_bg);
    painter.rect_stroke(solo_rect, 2.0, egui::Stroke::new(1.0_f32, theme.border));
    painter.text(solo_rect.center(), egui::Align2::CENTER_CENTER, "S", egui::FontId::proportional(10.0), solo_tc);
    let solo_resp = ui.allocate_rect(solo_rect, egui::Sense::click());
    if solo_resp.clicked() {
        *solo = !*solo;
        cmds.push(AppCommand::SetTrackSolo { track_index, solo: *solo });
    }
    y += btn_size.y + 4.0;

    // -- Sends (#2861): opens the track's sends to the aux buses --
    let sends_rect = egui::Rect::from_min_size(egui::pos2(inner_left, y), egui::vec2(inner_w, SENDS_ROW_H - 4.0));
    let sends_resp = ui.allocate_rect(sends_rect, egui::Sense::click())
        .on_hover_text("This track's sends to the aux buses: some of its sound into a bus, for one reverb many tracks share");
    let sends_bg = if sends_resp.hovered() { theme.bg_raised } else { theme.bg_deep };
    painter.rect_filled(sends_rect, 2.0, sends_bg);
    painter.rect_stroke(sends_rect, 2.0, egui::Stroke::new(1.0_f32, theme.border));
    let label = if sends == 0 { "Sends".to_string() } else { format!("Sends {sends}") };
    let colour = if sends == 0 { theme.text_dim } else { theme.accent };
    painter.text(sends_rect.center(), egui::Align2::CENTER_CENTER, label, egui::FontId::proportional(9.0), colour);
    sends_resp
}

// ═══════════════════════════════════════════════════════════════════
// Master strip
// ═══════════════════════════════════════════════════════════════════

#[allow(clippy::too_many_arguments)]
fn master_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    strip_h: f32,
    master_volume: &mut f32,
    master_peak_l: f32,
    master_peak_r: f32,
    lufs_momentary: f32,
    lufs_short_term: f32,
    lufs_integrated: f32,
    lufs_lra: f32,
    lufs_true_peak_dbfs: f32,
    lufs_target: f32,
    tp_oversample: bool,
    clip_indicator: bool,
    cmds: &mut Vec<AppCommand>,
) {
    let fader_h = fader_height(strip_h, MASTER_FIXED_H);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(MASTER_W, strip_h),
        egui::Sense::hover(),
    );

    let painter = ui.painter_at(rect);

    // Master background — just slightly lighter than track strips for subtle distinction
    painter.rect_filled(rect, 2.0, theme.bg_panel_alt);
    painter.rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, theme.border));

    let mut y = rect.min.y + 10.0;

    let fader_cx = rect.min.x + 30.0;
    let peak_l_x = rect.min.x + 56.0;
    let peak_r_x = peak_l_x + PEAK_BAR_W + 3.0;
    let lufs_x = peak_r_x + PEAK_BAR_W + METER_GAP + 3.0;
    let lufs_label_x = lufs_x + LUFS_BAR_W + 4.0;

    // -- "MASTER" label + clip indicator --
    let label_cx = rect.min.x + 40.0;
    painter.text(
        egui::pos2(label_cx, y + 6.0), egui::Align2::CENTER_CENTER,
        "MASTER", egui::FontId::proportional(10.0), theme.accent,
    );

    // Clip indicator (latched red dot, click to clear)
    let clip_rect = egui::Rect::from_center_size(
        egui::pos2(rect.max.x - 14.0, y + 6.0), egui::vec2(12.0, 12.0),
    );
    let clip_color = if clip_indicator {
        theme.marker_error
    } else {
        theme.bg_raised
    };
    painter.circle_filled(clip_rect.center(), 5.0, clip_color);
    if clip_indicator {
        painter.text(
            egui::pos2(clip_rect.center().x, clip_rect.center().y),
            egui::Align2::CENTER_CENTER, "C",
            egui::FontId::monospace(6.0), egui::Color32::WHITE,
        );
    }
    let clip_resp = ui.allocate_rect(clip_rect.expand(2.0), egui::Sense::click());
    if clip_resp.clicked() {
        cmds.push(AppCommand::ResetLufsMeter);
    }

    y += 20.0;

    // LUFS scale labels
    let fader_top = y + 10.0;
    let fader_bottom = fader_top + fader_h;
    for &(lufs_val, label) in &[
        (0.0_f32, "0"), (-6.0, "-6"), (-12.0, "-12"), (-18.0, "-18"),
        (-24.0, "-24"), (-36.0, "-36"), (-48.0, "-48"),
    ] {
        let tick_t = lufs_to_t(lufs_val);
        let tick_y = fader_bottom - tick_t * fader_h;
        painter.text(
            egui::pos2(lufs_label_x, tick_y), egui::Align2::LEFT_CENTER,
            label, egui::FontId::monospace(6.0), theme.text_dim,
        );
    }

    // -- Volume fader --
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(fader_cx - 1.5, fader_top), egui::pos2(fader_cx + 1.5, fader_bottom)),
        1.0, theme.bg_raised,
    );
    let unity_y = fader_bottom - 0.5 * fader_h;
    painter.line_segment(
        [egui::pos2(fader_cx - 8.0, unity_y), egui::pos2(fader_cx + 8.0, unity_y)],
        egui::Stroke::new(1.0_f32, theme.accent_dim),
    );

    let vol_t = (*master_volume / 2.0).clamp(0.0, 1.0);
    let thumb_y = fader_bottom - vol_t * fader_h;
    let fill_color = if *master_volume > 1.0 {
        theme.marker_error
    } else { theme.accent };
    if *master_volume > 0.001 {
        painter.rect_filled(
            egui::Rect::from_min_max(egui::pos2(fader_cx - 1.5, thumb_y), egui::pos2(fader_cx + 1.5, fader_bottom)),
            0.0, fill_color,
        );
    }
    let thumb_rect = egui::Rect::from_center_size(egui::pos2(fader_cx, thumb_y), egui::vec2(18.0, 8.0));
    painter.rect_filled(thumb_rect, 2.0, theme.text_primary);
    painter.rect_stroke(thumb_rect, 2.0, egui::Stroke::new(1.0_f32, fill_color));
    painter.line_segment(
        [egui::pos2(fader_cx - 6.0, thumb_y), egui::pos2(fader_cx + 6.0, thumb_y)],
        egui::Stroke::new(1.0_f32, theme.text_dim),
    );

    let fader_hit = egui::Rect::from_min_max(
        egui::pos2(fader_cx - 12.0, fader_top - 4.0),
        egui::pos2(fader_cx + 12.0, fader_bottom + 4.0),
    );
    let fader_resp = ui.allocate_rect(fader_hit, egui::Sense::click_and_drag());
    if fader_resp.dragged() {
        let step = -fader_resp.drag_delta().y / fader_h * 2.0;
        *master_volume = (*master_volume + step).clamp(0.0, 2.0);
        cmds.push(AppCommand::SetMasterVolume(*master_volume));
    }
    if fader_resp.clicked() {
        if let Some(pos) = fader_resp.interact_pointer_pos() {
            let t = 1.0 - ((pos.y - fader_top) / fader_h).clamp(0.0, 1.0);
            *master_volume = t * 2.0;
            cmds.push(AppCommand::SetMasterVolume(*master_volume));
        }
    }
    if fader_resp.double_clicked() {
        *master_volume = 1.0;
        cmds.push(AppCommand::SetMasterVolume(1.0));
    }

    // -- Peak + LUFS bars --
    draw_peak_bar(&painter, peak_l_x, fader_top, fader_bottom, PEAK_BAR_W, master_peak_l, theme);
    draw_peak_bar(&painter, peak_r_x, fader_top, fader_bottom, PEAK_BAR_W, master_peak_r, theme);
    draw_lufs_bar(
        &painter, lufs_x, fader_top, fader_bottom, LUFS_BAR_W,
        lufs_momentary, lufs_short_term, lufs_integrated, lufs_true_peak_dbfs, lufs_target, theme,
    );

    y = fader_bottom + READOUT_GAP;

    // -- Numerical readouts (two columns) --
    let col1_x = rect.min.x + 10.0;
    let col2_x = rect.min.x + MASTER_W / 2.0 + 4.0;

    // Row 1: dB | Integrated
    painter.text(
        egui::pos2(col1_x, y + 4.0), egui::Align2::LEFT_CENTER,
        format!("{} dB", format_db(vol_to_db(*master_volume))),
        egui::FontId::monospace(10.0), theme.text_secondary,
    );
    painter.text(
        egui::pos2(col2_x, y + 4.0), egui::Align2::LEFT_CENTER,
        format!("I:{}", format_lufs(lufs_integrated)),
        egui::FontId::monospace(10.0), lufs_text_color(lufs_integrated, lufs_target, theme),
    );
    y += 11.0;

    // Row 2: Peak | Short-term
    let peak_db = vol_to_db(master_peak_l.max(master_peak_r));
    let peak_color = if peak_db > -1.0 { theme.marker_error } else { theme.text_dim };
    painter.text(
        egui::pos2(col1_x, y + 4.0), egui::Align2::LEFT_CENTER,
        format!("Pk{}", format_db(peak_db)),
        egui::FontId::monospace(10.0), peak_color,
    );
    painter.text(
        egui::pos2(col2_x, y + 4.0), egui::Align2::LEFT_CENTER,
        format!("S:{}", format_lufs(lufs_short_term)),
        egui::FontId::monospace(10.0), theme.text_dim,
    );
    y += 10.0;

    // Row 3: TP | Momentary
    let tp_color = if lufs_true_peak_dbfs > -1.0 && lufs_true_peak_dbfs.is_finite() {
        theme.marker_error
    } else { theme.text_dim };
    let tp_text = format!("TP{}", format_lufs(lufs_true_peak_dbfs));
    painter.text(
        egui::pos2(col1_x, y + 4.0), egui::Align2::LEFT_CENTER,
        &tp_text,
        egui::FontId::monospace(10.0), tp_color,
    );
    // Invisible hover rect over the TP label so the user gets a
    // tooltip explaining whether 4× oversampling is engaged.
    let tp_hover_rect = egui::Rect::from_min_size(
        egui::pos2(col1_x - 2.0, y),
        egui::vec2(60.0, 10.0),
    );
    let tp_hover_resp = ui.allocate_rect(tp_hover_rect, egui::Sense::hover());
    let tp_tip = if tp_oversample {
        "True peak (4\u{00d7} oversampled, BS.1770-4 compliant)"
    } else {
        "True peak (sample-accurate \u{2014} enable oversampling in Audio prefs for BS.1770-4 compliance)"
    };
    tp_hover_resp.on_hover_text(tp_tip);
    painter.text(
        egui::pos2(col2_x, y + 4.0), egui::Align2::LEFT_CENTER,
        format!("M:{}", format_lufs(lufs_momentary)),
        egui::FontId::monospace(10.0), theme.text_dim,
    );
    y += 10.0;

    // Row 4: LRA
    painter.text(
        egui::pos2(col1_x, y + 4.0), egui::Align2::LEFT_CENTER,
        format!("LRA {:.1} LU", lufs_lra),
        egui::FontId::monospace(10.0), theme.text_dim,
    );
    y += 12.0;

    // Reset button
    let btn_rect = egui::Rect::from_min_size(egui::pos2(rect.center().x - 16.0, y), egui::vec2(32.0, 14.0));
    painter.rect_filled(btn_rect, 2.0, theme.bg_raised);
    painter.rect_stroke(btn_rect, 2.0, egui::Stroke::new(1.0_f32, theme.border));
    painter.text(btn_rect.center(), egui::Align2::CENTER_CENTER, "RST",
        egui::FontId::monospace(10.0), theme.text_secondary);
    let rst_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
    if rst_resp.clicked() {
        cmds.push(AppCommand::ResetLufsMeter);
    }
}

// ═══════════════════════════════════════════════════════════════════
// Public: show the audio mixer panel
// ═══════════════════════════════════════════════════════════════════

/// Persistent state for the mixer panel (track name editing).
/// Stored in AppState to survive across frames.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let full_rect = ui.max_rect();

    // Background + header
    ui.painter().rect_filled(full_rect, 0.0, theme.bg_deep);
    ui.painter().hline(full_rect.x_range(), full_rect.min.y, egui::Stroke::new(1.0_f32, theme.border));

    let header_rect = egui::Rect::from_min_size(full_rect.min, egui::vec2(full_rect.width(), HEADER_H));
    ui.painter().rect_filled(header_rect, 0.0, theme.bg_panel);
    ui.painter().text(
        egui::pos2(header_rect.min.x + 10.0, header_rect.center().y),
        egui::Align2::LEFT_CENTER, "Audio Mixer",
        egui::FontId::proportional(10.0), theme.text_secondary,
    );
    // The loudness playback is heard at, as the export will deliver it;
    // a click turns that off or on (#1565).
    if let Some(label) = state.delivery_loudness.label() {
        let rect = egui::Rect::from_min_max(egui::pos2(header_rect.center().x, header_rect.min.y), header_rect.max - egui::vec2(10.0, 0.0));
        let response = ui.allocate_rect(rect, egui::Sense::click())
            .on_hover_text("Playback is gained to the loudness the export normalises to, so it sounds as the file will. Click to turn it off or on.");
        let colour = if response.hovered() { theme.text_primary } else { theme.text_secondary };
        ui.painter().text(egui::pos2(rect.max.x, rect.center().y), egui::Align2::RIGHT_CENTER, label, egui::FontId::proportional(10.0), colour);
        if response.clicked() {
            cmds.push(AppCommand::SetHearDeliveryLoudness(!state.delivery_loudness.enabled));
        }
    }
    ui.painter().hline(full_rect.x_range(), header_rect.max.y, egui::Stroke::new(1.0_f32, theme.border));

    let content_rect = egui::Rect::from_min_max(
        egui::pos2(full_rect.min.x, header_rect.max.y), full_rect.max,
    );

    let playhead_secs = state.playhead.to_secs_f64();
    let playing = state.playback == klipp_engine::PlaybackState::Playing;
    let mut touches: Vec<(usize, klipp_core::automation::TrackParam, bool)> = Vec::new();
    let mut lives: Vec<(usize, klipp_core::TrackId, klipp_core::automation::TrackParam, f32)> = Vec::new();
    // Each strip's automation (#2306): a control shows its value as moved,
    // or its keys' value at the playhead when played from them.
    let strip_automation: Vec<StripAutomation> = state.sequences[state.active_sequence_idx].tracks.iter().map(|t| {
        use klipp_core::automation::TrackParam;
        let pass = &state.automation_pass;
        let shown = |param: TrackParam, set: f32| pass.live(t.id, param).or_else(|| t.automation.plays(param).then(|| t.automation.value_at(param, playhead_secs, set)));
        StripAutomation {
            mode: (t.kind == klipp_timeline::TrackKind::Audio).then_some(t.automation.mode),
            shown_volume: shown(TrackParam::Volume, t.volume),
            shown_pan: shown(TrackParam::Pan, t.pan),
            live_volume: goes_live(t, TrackParam::Volume, playing),
            live_pan: goes_live(t, TrackParam::Pan, playing),
        }
    }).collect();
    let seq = &mut state.sequences[state.active_sequence_idx];
    let track_count = seq.tracks.len();

    // Collect track data with track IDs and kinds; a track that cannot
    // be heard gets no strip (#2696).
    let mut track_data: Vec<(String, klipp_core::TrackId, klipp_timeline::TrackKind, f32, f32, bool, bool, bool)> = seq.tracks.iter()
        .map(|t| {
            let heard = is_heard(seq, t, &state.media_pool);
            (t.name.clone(), t.id, t.kind, t.volume, t.pan, t.muted, t.solo, heard)
        })
        .collect();
    let mut master_vol = state.master_volume;

    let peak_levels: Vec<(f32, f32)> = (0..track_count)
        .map(|i| state.track_peak_levels.get(i).copied().unwrap_or((0.0, 0.0)))
        .collect();

    // Use a persistent editing_name state via a simple approach:
    // store it temporarily in a local and write back.
    let mut editing_name = state.mixer_editing_name.take();

    // Clip content to mixer panel bounds
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content_rect), |ui| {
        ui.set_clip_rect(content_rect);
        ui.set_min_size(content_rect.size());

        // Layout: scrollable tracks on left, fixed master on right
        let master_total_w = MASTER_W + 12.0; // master width + gap
        let tracks_max_w = (content_rect.width() - master_total_w).max(60.0);
        // Every strip fills the panel's height, its fader taking what the
        // controls leave; a panel too short for that scrolls up and down
        // so no strip's mute and solo are cut off (#2693).
        let strip_h = (content_rect.height() - SCROLLBAR_ROOM).max(STRIP_MIN_H);

        egui::ScrollArea::vertical()
            .id_salt("mixer_strips_vscroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // The strips' own height, which may be more than the
                // panel shows: the scroll area above takes up the rest.
                let row = egui::vec2(ui.available_width(), strip_h + SCROLLBAR_ROOM);
                ui.allocate_ui_with_layout(row, egui::Layout::left_to_right(egui::Align::Min), |ui| {
                    // ── Scrollable track strips ──
                    egui::ScrollArea::horizontal()
                        .id_salt("mixer_tracks_scroll")
                        .max_width(tracks_max_w)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.add_space(6.0);

                                for i in 0..track_count {
                                    let (ref name, track_id, ref _kind, ref mut vol, ref mut pan, ref mut muted, ref mut solo, heard) = track_data[i];
                                    if !heard {
                                        continue;
                                    }
                                    let (pl, pr) = peak_levels[i];
                                    let mut strip_touches = Vec::new();
                                    let mut strip_lives = Vec::new();
                                    // A strip of its own: the controls it
                                    // allocates last would otherwise leave
                                    // the row's cursor inside it, and the
                                    // next strip drawn over its right side
                                    // (#2699).
                                    let sends = state.sequence().tracks.get(i).map_or(0, |t| t.sends.len());
                                    ui.allocate_ui_with_layout(egui::vec2(STRIP_W, strip_h), egui::Layout::top_down(egui::Align::Min), |ui| {
                                        let sends_button = fader_strip(
                                            ui, theme, strip_h, i, name, track_id,
                                            vol, pan, muted, solo,
                                            pl, pr,
                                            &mut editing_name,
                                            &strip_automation[i],
                                            &mut strip_touches,
                                            &mut strip_lives,
                                            sends,
                                            &mut cmds,
                                        );
                                        super::aux_routing::sends_popup(ui, theme, state, &mut cmds, track_id, &sends_button);
                                    });
                                    touches.extend(strip_touches.into_iter().map(|(param, down)| (i, param, down)));
                                    lives.extend(strip_lives.into_iter().map(|(param, v)| (i, track_id, param, v)));
                                    // Gap between strips
                                    ui.add_space(4.0);
                                }
                                // The aux buses (#2861), after the tracks.
                                ui.add_space(8.0);
                                for b in 0..state.sequence().buses.len() {
                                    super::aux_routing::bus_strip(ui, theme, state, &mut cmds, b, STRIP_W, strip_h);
                                    ui.add_space(4.0);
                                }
                                ui.vertical(|ui| super::aux_routing::add_bus_button(ui, theme, state, &mut cmds));
                            });
                        });

                    // ── Gap + fixed master strip ──
                    ui.add_space(12.0);

                    master_strip(
                        ui, theme, strip_h, &mut master_vol,
                        state.master_peak_left,
                        state.master_peak_right,
                        state.lufs_momentary_smooth,
                        state.lufs_short_term_smooth,
                        state.lufs_integrated,
                        state.lufs_lra,
                        state.lufs_true_peak_dbfs,
                        state.lufs_target,
                        state.tp_oversample,
                        state.clip_indicator,
                        &mut cmds,
                    );
                });
            });
    });

    state.mixer_editing_name = editing_name;
    // The controls held now, for Touch and Latch to record (#2306).
    for (i, param, down) in touches {
        if down {
            state.automation_held.insert((i, param));
        } else {
            state.automation_held.remove(&(i, param));
        }
    }
    // A drag holds its control; a click or double-click is a touch of
    // one frame, so it is recorded as a jump where the mode records.
    for (i, id, param, value) in lives {
        if state.automation_held.contains(&(i, param)) {
            state.automation_pass.set_live(id, param, value);
        } else {
            state.automation_pass.touch_once(id, param, value);
        }
    }

    // Write back track data
    let seq = &mut state.sequences[state.active_sequence_idx];
    for (i, (_, _, _, vol, pan, muted, solo, _)) in track_data.iter().enumerate() {
        if i < seq.tracks.len() {
            seq.tracks[i].volume = *vol;
            seq.tracks[i].pan = *pan;
            seq.tracks[i].muted = *muted;
            seq.tracks[i].solo = *solo;
        }
    }
    state.master_volume = master_vol;

    // Clear clip indicator on reset
    if cmds.iter().any(|c| matches!(c, AppCommand::ResetLufsMeter)) {
        state.clip_indicator = false;
    }

    cmds
}

// ═══════════════════════════════════════════════════════════════════
// Inspector LUFS meter widget
// ═══════════════════════════════════════════════════════════════════

pub fn draw_inspector_meter(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    peak: (f32, f32),
) {
    ui.add_space(8.0);

    let avail = ui.available_rect_before_wrap();
    let meter_h = 120.0_f32.min(avail.height() - 70.0);
    let total_h = meter_h + 64.0; // bar + gap + 3 rows of text
    let (rect, _) = ui.allocate_exact_size(egui::vec2(avail.width().min(220.0), total_h), egui::Sense::hover());

    let painter = ui.painter_at(rect);
    let bar_top = rect.min.y + 2.0;
    let bar_bottom = bar_top + meter_h;

    // Peak bars (wider for inspector)
    let peak_bar_w = 7.0;
    let mut x = rect.min.x + 6.0;
    draw_peak_bar(&painter, x, bar_top, bar_bottom, peak_bar_w, peak.0, theme);
    x += peak_bar_w + 3.0;
    draw_peak_bar(&painter, x, bar_top, bar_bottom, peak_bar_w, peak.1, theme);
    x += peak_bar_w + 6.0;

    // LUFS bar (wider for inspector)
    let lufs_w = 22.0;
    draw_lufs_bar(
        &painter, x, bar_top, bar_bottom, lufs_w,
        state.lufs_momentary_smooth, state.lufs_short_term_smooth, state.lufs_integrated,
        state.lufs_true_peak_dbfs, state.lufs_target, theme,
    );

    // Scale labels
    let label_x = x + lufs_w + 5.0;
    for &(lufs_val, label) in &[
        (0.0_f32, "0"), (-6.0, "-6"), (-12.0, "-12"), (-18.0, "-18"),
        (-24.0, "-24"), (-36.0, "-36"), (-48.0, "-48"),
    ] {
        let tick_t = lufs_to_t(lufs_val);
        let tick_y = bar_bottom - tick_t * meter_h;
        painter.text(
            egui::pos2(label_x, tick_y), egui::Align2::LEFT_CENTER,
            label, egui::FontId::monospace(10.0), theme.text_dim,
        );
    }

    // Clip indicator
    if state.clip_indicator {
        painter.text(
            egui::pos2(label_x + 32.0, bar_top + 6.0), egui::Align2::LEFT_CENTER,
            "CLIP", egui::FontId::monospace(10.0), theme.marker_error,
        );
    }

    // Numerical readouts — with gap below bar
    let ry = bar_bottom + READOUT_GAP;
    painter.text(
        egui::pos2(rect.min.x + 6.0, ry + 5.0), egui::Align2::LEFT_CENTER,
        format!("I: {} LUFS", format_lufs(state.lufs_integrated)),
        egui::FontId::monospace(10.0),
        lufs_text_color(state.lufs_integrated, state.lufs_target, theme),
    );
    painter.text(
        egui::pos2(rect.min.x + 6.0, ry + 19.0), egui::Align2::LEFT_CENTER,
        format!("S: {}   M: {}   TP: {} dB",
            format_lufs(state.lufs_short_term),
            format_lufs(state.lufs_momentary),
            format_lufs(state.lufs_true_peak_dbfs)),
        egui::FontId::monospace(10.0),
        theme.text_dim,
    );
    painter.text(
        egui::pos2(rect.min.x + 6.0, ry + 33.0), egui::Align2::LEFT_CENTER,
        format!("LRA: {:.1} LU", state.lufs_lra),
        egui::FontId::monospace(10.0),
        theme.text_dim,
    );

    ui.add_space(8.0);
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    /// #2306: a fader moved over keys, or while a pass records it, is live:
    /// no set command (no undo step of its own), the value as set kept. A
    /// track that doesn't play from keys and isn't recorded is set as
    /// before.
    #[test]
    fn a_recorded_or_keyed_control_moves_live_not_set() {
        use klipp_core::automation::{AutomationMode, TrackParam};
        let mut t = klipp_timeline::Track::new("A", klipp_timeline::TrackKind::Audio);
        let cases = [
            (AutomationMode::Read, false, false, false),
            (AutomationMode::Touch, false, false, false),
            (AutomationMode::Touch, false, true, true),
            (AutomationMode::Latch, false, true, true),
            (AutomationMode::Write, false, true, true),
            (AutomationMode::Read, true, false, true),
            (AutomationMode::Read, true, true, true),
            (AutomationMode::Off, true, true, false),
        ];
        let mut video = klipp_timeline::Track::new("V", klipp_timeline::TrackKind::Video);
        video.automation.mode = AutomationMode::Write;
        assert!(!goes_live(&video, TrackParam::Volume, true), "a video track's strip sets its controls");
        for (mode, keyed, playing, live) in cases {
            t.automation.mode = mode;
            t.automation.lanes.clear();
            if keyed {
                t.automation.lanes.insert(TrackParam::Volume, vec![klipp_core::Keyframe { time_secs: 0.0, value: 0.5, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() }]);
            }
            assert_eq!(goes_live(&t, TrackParam::Volume, playing), live, "{mode:?} keyed {keyed} playing {playing}");
            let (mut volume, mut lives, mut cmds) = (1.0_f32, Vec::new(), Vec::new());
            set_control(0, TrackParam::Volume, 0.25, live, &mut volume, &mut lives, &mut cmds);
            if live {
                assert!(cmds.is_empty() && volume == 1.0 && lives == vec![(TrackParam::Volume, 0.25)]);
            } else {
                assert!(matches!(cmds[..], [AppCommand::SetTrackVolume { volume: v, .. }] if v == 0.25) && volume == 0.25 && lives.is_empty());
            }
        }
    }

    /// One frame of the mixer in a panel `panel_h` tall, as the main
    /// layout hosts it, with the pointer pressed or released at `click`.
    /// The shapes drawn and the commands sent.
    fn mixer_frame(ctx: &egui::Context, state: &mut AppState, panel_h: f32, click: Option<(egui::Pos2, bool)>) -> (Vec<egui::epaint::ClippedShape>, Vec<AppCommand>) {
        let mut input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0))),
            ..Default::default()
        };
        if let Some((pos, pressed)) = click {
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        }
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        let out = ctx.run(input, |ctx| {
            egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                let rect = egui::Rect::from_min_size(egui::pos2(0.0, 300.0), egui::vec2(1200.0, panel_h));
                crate::ui_util::allocate_clipped_ui(ui, rect, |ui| {
                    ui.set_min_size(rect.size());
                    cmds = show(ui, state, &theme);
                });
            });
        });
        (out.shapes, cmds)
    }

    /// A text drawn whole, inside what its clip lets be seen.
    fn shown_whole(shapes: &[egui::epaint::ClippedShape], text: &str) -> Vec<egui::Rect> {
        shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text() == text => {
                let r = egui::Rect::from_min_size(t.pos, t.galley.size());
                s.clip_rect.contains_rect(r).then_some(r)
            }
            _ => None,
        }).collect()
    }

    /// #2693: at the panel's old fixed height and at its new minimum,
    /// every strip shows its mute and solo, the master its reset, and a
    /// click on a mute mutes that track.
    #[test]
    fn every_strip_control_shows_and_works_at_the_panels_height() {
        for panel_h in [220.0, MIXER_MIN_H] {
            let ctx = egui::Context::default();
            let mut state = AppState::default();
            let seq = &state.sequences[state.active_sequence_idx];
            let tracks = seq.tracks.iter().filter(|t| is_heard(seq, t, &state.media_pool)).count();
            assert!(tracks > 0, "the default sequence has a track to hear");
            mixer_frame(&ctx, &mut state, panel_h, None);
            let (shapes, _) = mixer_frame(&ctx, &mut state, panel_h, None);
            let mutes = shown_whole(&shapes, "M");
            assert_eq!(mutes.len(), tracks, "{panel_h} px: every heard track's mute is seen");
            assert_eq!(shown_whole(&shapes, "S").len(), tracks, "{panel_h} px: every heard track's solo is seen");
            assert_eq!(shown_whole(&shapes, "RST").len(), 1, "{panel_h} px: the master's reset is seen");

            let at = mutes[0].center();
            mixer_frame(&ctx, &mut state, panel_h, Some((at, true)));
            let (_, cmds) = mixer_frame(&ctx, &mut state, panel_h, Some((at, false)));
            assert!(cmds.iter().any(|c| matches!(c, AppCommand::SetTrackMute { muted: true, .. })), "{panel_h} px: the mute is clicked: {cmds:?}");
        }
    }

    /// #2861: every track strip shows its Sends whole at the panel's
    /// heights; "+ Bus" adds a bus, one undo step, whose strip then shows
    /// its name, level and mute; a click on a strip's Sends opens them.
    #[test]
    fn buses_are_added_and_a_tracks_sends_open_from_its_strip() {
        for panel_h in [220.0, MIXER_MIN_H] {
            let ctx = egui::Context::default();
            let mut state = AppState::default();
            let seq = &state.sequences[state.active_sequence_idx];
            let tracks = seq.tracks.iter().filter(|t| is_heard(seq, t, &state.media_pool)).count();
            mixer_frame(&ctx, &mut state, panel_h, None);
            let (shapes, _) = mixer_frame(&ctx, &mut state, panel_h, None);
            let sends = shown_whole(&shapes, "Sends");
            assert_eq!(sends.len(), tracks, "{panel_h} px: every strip's sends are seen");
            let levels = shown_whole(&shapes, "+0.0 dB").len();
            let add = shown_whole(&shapes, "+ Bus");
            assert_eq!(add.len(), 1, "{panel_h} px");

            let at = add[0].center();
            mixer_frame(&ctx, &mut state, panel_h, Some((at, true)));
            let (_, cmds) = mixer_frame(&ctx, &mut state, panel_h, Some((at, false)));
            assert!(cmds.iter().any(|c| matches!(c, AppCommand::RoutingChanged)), "{cmds:?}");
            assert_eq!(state.sequence().buses.len(), 1);
            assert_eq!(state.active_history_mut().undo_label(), Some("Aux Routing"));
            mixer_frame(&ctx, &mut state, panel_h, None);
            let (shapes, _) = mixer_frame(&ctx, &mut state, panel_h, None);
            assert_eq!(shown_whole(&shapes, "Bus 1").len(), 1, "{panel_h} px: the bus's strip");
            assert_eq!(shown_whole(&shapes, "+0.0 dB").len(), levels + 1, "{panel_h} px: its level beside the tracks' and master's");
            assert_eq!(shown_whole(&shapes, "M").len(), tracks + 1, "{panel_h} px: its mute beside the tracks'");

            let at = sends[0].center();
            mixer_frame(&ctx, &mut state, panel_h, Some((at, true)));
            mixer_frame(&ctx, &mut state, panel_h, Some((at, false)));
            let (shapes, _) = mixer_frame(&ctx, &mut state, panel_h, None);
            assert_eq!(shown_whole(&shapes, "Send to").len(), 1, "{panel_h} px: the sends open");
        }
    }

    /// #2696: only tracks that can be heard get a strip. A text track
    /// gets none, nor does a video track whose clip is heard from its
    /// linked partner on an audio track; a video track whose clip plays
    /// its own sound keeps its strip, and its strip still works the
    /// track it stands for.
    #[test]
    fn only_tracks_that_can_be_heard_get_a_strip() {
        use klipp_timeline::{Clip, Track, TrackKind};
        let tc = |secs: f64| klipp_core::Timecode::from_secs_f64(secs, 24);
        let mut state = AppState::default();
        let media = klipp_core::MediaId::new();
        let mut item = klipp_media::MediaItem::new_synthetic_title(media, "shot".into(), tc(4.0), klipp_core::Fps::new(24, 1));
        item.has_audio = true;
        state.media_pool.add(item);
        let clip = || Clip::media(media, tc(0.0), tc(4.0), tc(0.0), tc(4.0));

        let mut own_sound = Track::new("V1", TrackKind::Video);
        own_sound.clips.push(clip());
        let mut partner = Track::new("A1", TrackKind::Audio);
        partner.clips.push(clip());
        let mut linked = Track::new("V2", TrackKind::Video);
        linked.clips.push(Clip { linked_clip_id: Some(partner.clips[0].id), ..clip() });
        let text = Track::new("T1", TrackKind::Text);
        let seq = &mut state.sequences[state.active_sequence_idx];
        seq.tracks = vec![text, linked, own_sound, partner];

        let seq = &state.sequences[state.active_sequence_idx];
        let heard: Vec<bool> = seq.tracks.iter().map(|t| is_heard(seq, t, &state.media_pool)).collect();
        assert_eq!(heard, [false, false, true, true]);

        let ctx = egui::Context::default();
        mixer_frame(&ctx, &mut state, 260.0, None);
        let (shapes, _) = mixer_frame(&ctx, &mut state, 260.0, None);
        let mut mutes = shown_whole(&shapes, "M");
        assert_eq!(mutes.len(), 2, "a strip for V1 and A1 only");
        assert_eq!(shown_whole(&shapes, "S").len(), 2);
        mutes.sort_by(|a, b| a.min.x.total_cmp(&b.min.x));
        let at = mutes[0].center();
        mixer_frame(&ctx, &mut state, 260.0, Some((at, true)));
        let (_, cmds) = mixer_frame(&ctx, &mut state, 260.0, Some((at, false)));
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SetTrackMute { track_index: 2, muted: true })), "the first strip mutes V1, track 2: {cmds:?}");
    }

    /// #2699: strips sit side by side without covering each other, so
    /// each one's automation mode is seen whole on its own strip.
    #[test]
    fn each_strip_shows_its_whole_automation_mode() {
        use klipp_core::automation::AutomationMode;
        let mut state = AppState::default();
        let seq = &mut state.sequences[state.active_sequence_idx];
        seq.tracks.push(klipp_timeline::Track::new("A2", klipp_timeline::TrackKind::Audio));
        seq.tracks.push(klipp_timeline::Track::new("A3", klipp_timeline::TrackKind::Audio));
        for t in seq.tracks.iter_mut() {
            t.automation.mode = AutomationMode::Read;
        }
        let ctx = egui::Context::default();
        mixer_frame(&ctx, &mut state, 260.0, None);
        let (shapes, _) = mixer_frame(&ctx, &mut state, 260.0, None);
        // The track strips' backgrounds, in the order drawn.
        let strips: Vec<egui::Rect> = shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Rect(r) if r.rect.width() == STRIP_W && r.fill == KlippTheme::default().bg_panel => Some(r.rect),
            _ => None,
        }).collect();
        assert_eq!(strips.len(), 3, "{strips:?}");
        for pair in strips.windows(2) {
            assert!(pair[0].max.x <= pair[1].min.x, "strips overlap: {pair:?}");
        }
        let labels = shown_whole(&shapes, "Read");
        assert_eq!(labels.len(), 3);
        for (label, strip) in labels.iter().zip(&strips) {
            assert!(strip.contains_rect(*label), "{label:?} in {strip:?}");
        }
    }

    #[test]
    fn mixer_layout_constants_valid() {
        // Master strip must be narrower than any reasonable panel width
        assert!(MASTER_W > 0.0 && MASTER_W < 400.0);
        // Track strip must be narrower than master
        assert!(STRIP_W > 0.0 && STRIP_W < MASTER_W);
        // Fader track must fit in the minimum strip height with room for controls
        assert!(FADER_MIN_H + STRIP_FIXED_H <= STRIP_MIN_H, "Fader needs room for name, pan, buttons");
        assert!(FADER_MIN_H + MASTER_FIXED_H <= STRIP_MIN_H, "Master fader needs room for its readouts");
        // Minimum panel height must be at least as tall as strip min height
        assert!(MIXER_MIN_H >= STRIP_MIN_H + HEADER_H);
        assert!(fader_height(STRIP_MIN_H, MASTER_FIXED_H) >= FADER_MIN_H);
        // Peak bar width must be positive and reasonable
        assert!(PEAK_BAR_W > 0.0 && PEAK_BAR_W < 20.0);
        // LUFS bar must be wider than peak bars
        assert!(LUFS_BAR_W > PEAK_BAR_W);
    }

    #[test]
    fn mixer_scroll_area_width_leaves_room_for_master() {
        // Simulate various panel widths
        for panel_w in [400.0, 600.0, 800.0, 1200.0, 1920.0] {
            let master_total_w = MASTER_W + 12.0;
            let tracks_max_w = (panel_w - master_total_w).max(60.0);
            // Track scroll area must leave room for master
            assert!(tracks_max_w + master_total_w <= panel_w,
                "Panel {panel_w}: tracks {tracks_max_w} + master {master_total_w} > panel");
            // Track scroll area must be at least 60px
            assert!(tracks_max_w >= 60.0, "Panel {panel_w}: tracks area too narrow");
        }
    }

    #[test]
    fn mixer_master_width_constant() {
        // Master strip width should not depend on track count
        let master_w_0_tracks = MASTER_W;
        let master_w_16_tracks = MASTER_W;
        assert_eq!(master_w_0_tracks, master_w_16_tracks);
    }

    #[test]
    fn db_to_t_range() {
        assert_eq!(db_to_t(-60.0), 0.0);
        assert!((db_to_t(0.0) - 60.0 / 66.0).abs() < 0.01);
        assert_eq!(db_to_t(6.0), 1.0);
        // Out of range clamps
        assert_eq!(db_to_t(-100.0), 0.0);
        assert_eq!(db_to_t(20.0), 1.0);
    }

    #[test]
    fn lufs_to_t_range() {
        assert_eq!(lufs_to_t(f32::NEG_INFINITY), 0.0);
        assert_eq!(lufs_to_t(-60.0), 0.0);
        assert_eq!(lufs_to_t(0.0), 1.0);
        assert_eq!(lufs_to_t(-30.0), 0.5);
    }

    #[test]
    fn format_helpers() {
        assert_eq!(format_db(-60.0), "-inf");
        assert_eq!(format_db(0.0), "+0.0");
        assert_eq!(format_lufs(f32::NEG_INFINITY), "\u{2212}\u{221e}");
        assert_eq!(format_lufs(-23.0), "-23.0");
    }

    #[test]
    fn vol_to_db_basic() {
        assert_eq!(vol_to_db(0.0), -60.0);
        assert!((vol_to_db(1.0) - 0.0).abs() < 0.01);
        assert!((vol_to_db(2.0) - 6.02).abs() < 0.1);
        assert!((vol_to_db(0.5) - (-6.02)).abs() < 0.1);
    }
}
