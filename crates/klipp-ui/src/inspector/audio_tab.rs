use crate::widgets::menu::MenuRows;
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use crate::widgets::scrubby_param::{EasePreset, KeyframeState};
use crate::widgets::waveform;
use klipp_core::{AppCommand, AudioParam, Timecode};
use klipp_timeline::{ChannelConfig, Clip, FadeCurve};

/// Find peak levels for the clip's audio. If the clip is on a video track
/// with a linked audio clip, use the audio track's peaks.
pub fn find_clip_peak_levels(clip: &Clip, track_idx: usize, state: &AppState) -> (f32, f32) {
    let audio_track_idx = if let Some(linked_id) = clip.linked_clip_id {
        state.sequence().tracks.iter().enumerate()
            .find(|(_, t)| {
                t.kind == klipp_timeline::TrackKind::Audio
                    && t.clips.iter().any(|c| c.id == linked_id)
            })
            .map(|(i, _)| i)
            .unwrap_or(track_idx)
    } else {
        track_idx
    };
    state.track_peak_levels
        .get(audio_track_idx)
        .copied()
        .unwrap_or((0.0, 0.0))
}

pub struct DuckSettings {
    pub sidechain_track: usize,
    pub level: f32,
    pub attack: f32,
    pub release: f32,
    pub threshold: f32,
}

impl Default for DuckSettings {
    fn default() -> Self {
        Self {
            sidechain_track: 0,
            level: 0.2,
            attack: 0.1,
            release: 0.3,
            threshold: 0.1,
        }
    }
}

impl DuckSettings {
    /// Build a fresh DuckSettings seeded from the user's audio prefs.
    /// Caller is responsible for overriding `sidechain_track` afterwards
    /// (role-aware track selection lives in the inspector dispatcher).
    pub fn from_prefs(prefs: &crate::prefs::AudioPrefs) -> Self {
        Self {
            sidechain_track: 0,
            level: prefs.duck_level,
            attack: prefs.duck_attack_secs,
            release: prefs.duck_release_secs,
            threshold: prefs.duck_threshold,
        }
    }
}

pub struct TrackInfo {
    pub index: usize,
    pub name: String,
}

/// clip_ctx: Some((clip, track_idx, is_timeline_clip)) or None for no-selection state.
pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip_ctx: Option<(&Clip, usize, bool)>,
    cmds: &mut Vec<AppCommand>,
    peak: (f32, f32),
    waveform_peaks: Option<&[f32]>,
    source_channels: Option<u16>,
    clip_time_secs: f64,
    duck: &mut DuckSettings,
    tracks: &[TrackInfo],
) {
    match clip_ctx {
        None => show_sequence_properties(ui, theme, state),
        Some((clip, track_idx, is_timeline)) => {
            show_clip_audio(ui, theme, state, clip, track_idx, is_timeline, cmds, peak, waveform_peaks, source_channels, clip_time_secs, duck, tracks);
        }
    }
}

// ══════════════════════════════════════════════════
// State 1: No clip selected — the sound going out
// ══════════════════════════════════════════════════

/// With no clip selected: the output the sound goes to and the master
/// meter. The sequence's name and format are on the Video tab (and its
/// counts on the Info tab), the master volume on the transport and the
/// mixer's master fader, proxies on the Video tab, and the delivery
/// format on the Export page; none is repeated here.
fn show_sequence_properties(ui: &mut egui::Ui, theme: &KlippTheme, state: &AppState) {
    use super::rows::{colour_row, text_row};

    section_card(ui, "OUTPUT", theme, |ui| {
        if state.audio_backend_ok {
            colour_row(ui, theme, "Device", "Open", theme.marker_success);
            let khz = state.audio_sample_rate as f32 / 1000.0;
            let rate = if khz == khz.floor() { format!("{} kHz", khz as u32) } else { format!("{khz:.1} kHz") };
            text_row(ui, theme, "Sample rate", rate);
        } else {
            colour_row(ui, theme, "Device", "Not open", theme.marker_error);
        }
        ui.add_space(4.0);
    });

    section_card(ui, "LOUDNESS METER", theme, |ui| {
        crate::panels::audio_mixer::draw_inspector_meter(ui, theme, state, (state.master_peak_left, state.master_peak_right));
    });

    ui.add_space(12.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new("Select a clip to see its sound.").size(12.0).color(theme.text_secondary));
    });
}

// ══════════════════════════════════════════════════
// State 2 & 3: Clip selected — audio controls
// ══════════════════════════════════════════════════

/// Response from a custom slider row, mirrors the fields needed by handle_audio_param / handle_fade_param.
#[derive(Default)]
struct SliderResponse {
    changed: bool,
    drag_started: bool,
    drag_stopped: bool,
    keyframe_toggled: bool,
    keyframe_nav_prev: bool,
    keyframe_nav_next: bool,
    keyframe_set_interpolation: Option<klipp_core::Interpolation>,
    keyframe_ease_preset: Option<EasePreset>,
}

/// Custom slider row: label + value input box + optional keyframe diamond on one line,
/// then a slider track on the line below.
///
/// `id_salt`: unique per-slider id salt.
/// `label`: display name.
/// `value`: mutable current value.
/// `range`: min..=max.
/// `default_val`: value shown as "no change" (used for center-origin on Pan).
/// `speed`: drag speed for DragValue.
/// `decimals`: decimal places for display.
/// `suffix`: text appended to value (e.g. " s").
/// `kf_state`: optional keyframe state for the diamond.
/// `center_origin`: if true, slider fills from center (for bipolar params like Pan).
#[allow(clippy::too_many_arguments)]
fn slider_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    _id_salt: &str,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    default_val: f32,
    speed: f64,
    decimals: usize,
    suffix: &str,
    kf_state: Option<KeyframeState>,
    center_origin: bool,
) -> SliderResponse {
    let mut resp = SliderResponse::default();
    let w = ui.available_width();
    let pad = 14.0;
    let row_h = 30.0;
    // The row's track, when the caller named it (#2296).
    let tag = crate::param_focus::take_tag(ui);
    let mut row_clicked = false;

    let old_value = *value;
    let min = *range.start();
    let max = *range.end();

    // ── Top row: label left, value input + keyframe diamond right ──
    let (top_rect, top_resp) = ui.allocate_exact_size(egui::vec2(w, row_h), if tag.is_some() { egui::Sense::click() } else { egui::Sense::hover() });
    row_clicked |= top_resp.clicked();

    // Label
    ui.painter().text(
        egui::pos2(top_rect.min.x + pad, top_rect.center().y),
        egui::Align2::LEFT_CENTER,
        label, egui::FontId::proportional(super::rows::TEXT), theme.text_secondary,
    );

    // Keyframe diamond (rightmost element)
    let kf_right = top_rect.max.x - pad;
    if let Some(ref ks) = kf_state {
        let diamond_x = kf_right - 5.0;
        let diamond_y = top_rect.center().y;
        let diamond_rect = egui::Rect::from_center_size(
            egui::pos2(diamond_x, diamond_y), egui::vec2(16.0, 16.0),
        );
        let diamond_resp = ui.allocate_rect(diamond_rect, egui::Sense::click());

        let hovered = diamond_resp.hovered();
        let at_playhead = ks.has_keyframe_at_playhead;

        // Draw diamond
        if at_playhead {
            draw_keyframe_diamond(ui, egui::pos2(diamond_x, diamond_y), theme.marker_warning, true);
        } else if hovered {
            draw_keyframe_diamond(ui, egui::pos2(diamond_x, diamond_y), theme.text_secondary, false);
        } else {
            draw_keyframe_diamond(ui, egui::pos2(diamond_x, diamond_y),
                egui::Color32::from_rgba_premultiplied(theme.text_dim.r(), theme.text_dim.g(), theme.text_dim.b(), 90), false);
        }

        if diamond_resp.clicked() {
            resp.keyframe_toggled = true;
            row_clicked = true;
        }
        if let Some(text) = tag.as_ref().and_then(|t| crate::param_focus::diamond_hover(t.keys(), at_playhead)) {
            diamond_resp.clone().on_hover_text(text);
        }

        // Right-click context menu for interpolation/ease
        diamond_resp.context_menu_rows(|ui| {
            if at_playhead {
                ui.label(egui::RichText::new("Interpolation").size(11.0).color(theme.text_dim));
                if ui.selectable_label(ks.current_interpolation == Some(klipp_core::Interpolation::Linear), "Linear").clicked() {
                    resp.keyframe_set_interpolation = Some(klipp_core::Interpolation::Linear);
                    ui.close_menu();
                }
                if ui.selectable_label(ks.current_interpolation == Some(klipp_core::Interpolation::Step), "Step").clicked() {
                    resp.keyframe_set_interpolation = Some(klipp_core::Interpolation::Step);
                    ui.close_menu();
                }
                if ui.selectable_label(ks.current_interpolation == Some(klipp_core::Interpolation::Bezier), "Bezier").clicked() {
                    resp.keyframe_set_interpolation = Some(klipp_core::Interpolation::Bezier);
                    ui.close_menu();
                }
                ui.separator();
                ui.label(egui::RichText::new("Ease Preset").size(11.0).color(theme.text_dim));
                if ui.button("Ease In").clicked() {
                    resp.keyframe_ease_preset = Some(EasePreset::EaseIn);
                    ui.close_menu();
                }
                if ui.button("Ease Out").clicked() {
                    resp.keyframe_ease_preset = Some(EasePreset::EaseOut);
                    ui.close_menu();
                }
                if ui.button("Ease Both").clicked() {
                    resp.keyframe_ease_preset = Some(EasePreset::EaseBoth);
                    ui.close_menu();
                }
            } else {
                if ui.button("Add Keyframe").clicked() {
                    resp.keyframe_toggled = true;
                    ui.close_menu();
                }
            }
        });

        // Nav arrows (prev/next keyframe) — draw small arrows flanking the diamond
        if ks.has_any_keyframes {
            // Prev arrow
            if ks.prev_keyframe_time.is_some() {
                let prev_rect = egui::Rect::from_center_size(
                    egui::pos2(diamond_x - 14.0, diamond_y), egui::vec2(12.0, 14.0),
                );
                let prev_resp = ui.allocate_rect(prev_rect, egui::Sense::click());
                let arrow_col = if prev_resp.hovered() { theme.text_primary } else { theme.text_dim };
                ui.painter().text(
                    prev_rect.center(), egui::Align2::CENTER_CENTER,
                    "\u{25C0}", egui::FontId::proportional(10.0), arrow_col,
                );
                if prev_resp.clicked() { resp.keyframe_nav_prev = true; }
            }
            // Next arrow
            if ks.next_keyframe_time.is_some() {
                let next_rect = egui::Rect::from_center_size(
                    egui::pos2(diamond_x + 14.0, diamond_y), egui::vec2(12.0, 14.0),
                );
                let next_resp = ui.allocate_rect(next_rect, egui::Sense::click());
                let arrow_col = if next_resp.hovered() { theme.text_primary } else { theme.text_dim };
                ui.painter().text(
                    next_rect.center(), egui::Align2::CENTER_CENTER,
                    "\u{25B6}", egui::FontId::proportional(10.0), arrow_col,
                );
                if next_resp.clicked() { resp.keyframe_nav_next = true; }
            }
        }
    }

    // Value input box (DragValue) — positioned to the left of the keyframe area
    let input_w = 60.0;
    let input_h = 26.0;
    let kf_area_w = if kf_state.is_some() { 44.0 } else { 0.0 };
    let input_right = kf_right - kf_area_w;
    let input_rect = egui::Rect::from_min_size(
        egui::pos2(input_right - input_w, top_rect.center().y - input_h / 2.0),
        egui::vec2(input_w, input_h),
    );

    // Draw the bordered input box background
    ui.painter().rect(input_rect, 4.0, theme.bg_raised, egui::Stroke::new(1.0_f32, theme.border));

    // Place DragValue inside the input rect
    let dv_resp = ui.allocate_new_ui(egui::UiBuilder::new().max_rect(input_rect.shrink(2.0)), |ui| {
        ui.style_mut().visuals.widgets.inactive.bg_fill = egui::Color32::TRANSPARENT;
        ui.style_mut().visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        ui.style_mut().visuals.widgets.hovered.bg_fill = egui::Color32::TRANSPARENT;
        ui.style_mut().visuals.widgets.hovered.bg_stroke = egui::Stroke::NONE;
        ui.style_mut().visuals.widgets.active.bg_fill = egui::Color32::TRANSPARENT;
        ui.style_mut().visuals.widgets.active.bg_stroke = egui::Stroke::NONE;
        ui.style_mut().visuals.selection.bg_fill = theme.accent.linear_multiply(0.3);

        let dv = egui::DragValue::new(value)
            .range(min..=max)
            .speed(speed)
            .custom_formatter(move |v, _| {
                let s = format!("{:.prec$}{suffix}", v, prec = decimals, suffix = suffix);
                s
            })
            .custom_parser(|s| {
                let cleaned: String = s.chars().filter(|c| c.is_ascii_digit() || *c == '.' || *c == '-').collect();
                cleaned.parse::<f64>().ok()
            })
            .update_while_editing(false);

        ui.add_sized(
            egui::vec2(input_w - 4.0, input_h - 4.0),
            dv,
        )
    });

    let dv_inner = &dv_resp.inner;
    if dv_inner.dragged() && !ui.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
        // drag ended
    }
    if dv_inner.drag_started() {
        resp.drag_started = true;
    }
    if dv_inner.drag_stopped() {
        resp.drag_stopped = true;
    }
    if dv_inner.changed() || (*value - old_value).abs() > f32::EPSILON {
        resp.changed = true;
    }

    // ── Slider track below the top row ──
    let track_h = 4.0;
    let track_pad_y = 4.0;
    let track_total_h = track_h + track_pad_y * 2.0;
    let (track_area, _) = ui.allocate_exact_size(egui::vec2(w, track_total_h), egui::Sense::hover());
    let track_left = track_area.min.x + pad;
    let track_right = track_area.max.x - pad;
    let track_w = track_right - track_left;
    let track_y = track_area.center().y - track_h / 2.0;
    let track_rect = egui::Rect::from_min_size(
        egui::pos2(track_left, track_y), egui::vec2(track_w, track_h),
    );

    // Track background
    ui.painter().rect_filled(track_rect, 2.0, theme.bg_raised);

    // Filled portion
    let t = ((*value - min) / (max - min)).clamp(0.0, 1.0);
    if center_origin {
        let center_t = ((default_val - min) / (max - min)).clamp(0.0, 1.0);
        let center_x = track_left + center_t * track_w;
        let value_x = track_left + t * track_w;
        let fill_left = center_x.min(value_x);
        let fill_right = center_x.max(value_x);
        if (fill_right - fill_left) > 0.5 {
            ui.painter().rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(fill_left, track_y),
                    egui::pos2(fill_right, track_y + track_h),
                ),
                2.0, theme.accent,
            );
        }
    } else {
        let fill_w = t * track_w;
        if fill_w > 0.5 {
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(track_left, track_y), egui::vec2(fill_w, track_h),
                ),
                2.0, theme.accent,
            );
        }
    }

    // Thumb
    let thumb_r = 4.0;
    let thumb_x = track_left + t * track_w;
    let thumb_y = track_y + track_h / 2.0;
    ui.painter().circle_filled(
        egui::pos2(thumb_x, thumb_y), thumb_r, theme.text_primary,
    );
    ui.painter().circle_stroke(
        egui::pos2(thumb_x, thumb_y), thumb_r, egui::Stroke::new(1.0_f32, theme.border),
    );

    // Track interaction — wider hit area
    let hit_rect = egui::Rect::from_min_max(
        egui::pos2(track_left - 2.0, track_y - 6.0),
        egui::pos2(track_right + 2.0, track_y + track_h + 6.0),
    );
    let track_resp = ui.allocate_rect(hit_rect, egui::Sense::click_and_drag());

    if track_resp.drag_started() {
        resp.drag_started = true;
    }
    if track_resp.dragged() || track_resp.clicked() {
        if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
            let new_t = ((pos.x - track_left) / track_w).clamp(0.0, 1.0);
            *value = min + new_t * (max - min);
            resp.changed = true;
        }
    }
    if track_resp.drag_stopped() {
        resp.drag_stopped = true;
    }
    if let Some(tag) = &tag {
        crate::param_focus::row_drawn(ui, tag, top_rect, row_clicked);
    }

    resp
}

/// Simple slider row without keyframe support (for duck settings etc.)
#[allow(clippy::too_many_arguments)]
fn show_clip_audio(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    state: &AppState,
    clip: &Clip,
    track_idx: usize,
    is_timeline: bool,
    cmds: &mut Vec<AppCommand>,
    peak: (f32, f32),
    waveform_peaks: Option<&[f32]>,
    source_channels: Option<u16>,
    clip_time_secs: f64,
    duck: &mut DuckSettings,
    tracks: &[TrackInfo],
) {
    let keyframe_tolerance = crate::inspector::keyframe_selection::frame_tolerance(state.sequence());
    let clip_id = clip.id;
    let clip_timeline_in_secs = clip.timeline_in.to_secs_f64();
    let clip_rate = clip.timeline_in.rate;

    let mut gain = crate::inspector::keyframe_selection::editable_value(&clip.gain_keyframes, clip_time_secs, clip.gain, keyframe_tolerance);
    let mut volume = crate::inspector::keyframe_selection::editable_value(&clip.volume_keyframes, clip_time_secs, clip.volume, keyframe_tolerance);
    let mut pan = crate::inspector::keyframe_selection::editable_value(&clip.pan_keyframes, clip_time_secs, clip.pan, keyframe_tolerance);
    let mut fade_in = clip.fade_in_secs;
    let mut fade_out = clip.fade_out_secs;
    let mut fade_in_curve = clip.fade_in_curve;
    let mut fade_out_curve = clip.fade_out_curve;
    let mut channel_config = clip.channel_config;

    // ── ROLE section ──
    section_card(ui, "ROLE", theme, |ui| {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            let current_role = clip.role;
            let role_color = theme.clip_role_color(current_role);

            // Green status dot
            let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().circle_filled(dot_rect.center(), 4.0, role_color);

            let mut new_role = current_role;
            egui::ComboBox::from_id_salt("clip_role")
                .selected_text(current_role.label())
                .width(120.0)
                .show_ui(ui, |ui| {
                    for &role in klipp_core::ClipRole::all() {
                        let rc = theme.clip_role_color(role);
                        let text = egui::RichText::new(role.label()).color(rc);
                        ui.selectable_value(&mut new_role, role, text);
                    }
                });
            if new_role != current_role {
                cmds.push(AppCommand::SetClipRole { clip_id, role: new_role });
            }
        });
        // What the sound was heard as (#2216), when that is not the role
        // it has: a sure label is applied by itself, an unsure one is
        // offered here.
        let heard = clip.media_id().and_then(|m| state.media_pool.get(&m)).and_then(|m| m.audio_role).filter(|p| p.role != clip.role);
        if let Some(p) = heard {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                ui.label(egui::RichText::new(format!("Sounds like {} ({:.0}%)", p.role.label(), p.confidence * 100.0)).small().weak());
                if ui.small_button("Use as role").on_hover_text("Set the clip's role to what its sound was classified as").clicked() {
                    cmds.push(AppCommand::SetClipRole { clip_id, role: p.role });
                }
            });
        }
        ui.add_space(4.0);
    });

    ui.add_space(8.0);

    // ── AUDIO section ──
    section_card(ui, "AUDIO", theme, |ui| {
        let w = ui.available_width();
        let pad = 14.0;
        let row_h = 30.0;

        // Global reset button — only when any value differs from default
        let is_default = (gain - 1.0).abs() < f32::EPSILON
            && (volume - 1.0).abs() < f32::EPSILON
            && pan.abs() < f32::EPSILON
            && fade_in.abs() < f32::EPSILON
            && fade_out.abs() < f32::EPSILON
            && channel_config == ChannelConfig::Default;
        if !is_default {
            ui.horizontal(|ui| {
                ui.add_space(pad);
                let (reset_rect, reset_resp) = ui.allocate_exact_size(
                    egui::vec2(16.0, 16.0), egui::Sense::click(),
                );
                let tint = if reset_resp.hovered() {
                    theme.text_primary
                } else {
                    egui::Color32::from_rgba_premultiplied(theme.text_dim.r(), theme.text_dim.g(), theme.text_dim.b(), 100)
                };
                Icon::ArrowCounterClockwise.image(12.0).tint(tint).paint_at(ui, reset_rect);
                if reset_resp.on_hover_text("Reset all audio to defaults").clicked() {
                    gain = 1.0;
                    volume = 1.0;
                    pan = 0.0;
                    fade_in = 0.0;
                    fade_out = 0.0;
                    fade_in_curve = FadeCurve::default();
                    fade_out_curve = FadeCurve::default();
                    channel_config = ChannelConfig::default();
                    cmds.push(AppCommand::UpdateClipAudio {
                        clip_id, gain, volume, pan,
                        fade_in_secs: fade_in, fade_out_secs: fade_out,
                        fade_in_curve, fade_out_curve, channel_config,
                    });
                }
                ui.label(
                    egui::RichText::new("Reset to defaults")
                        .size(super::rows::TEXT)
                        .color(theme.text_secondary),
                );
            });
            ui.add_space(2.0);
        }

        // Source channel info
        if let Some(ch) = source_channels {
            // What the file holds, as its stream says.
            let text = match ch {
                1 => "Mono (1ch)".to_string(),
                2 => "Stereo (2ch)".to_string(),
                6 => "5.1 (6ch)".to_string(),
                8 => "7.1 (8ch)".to_string(),
                n => format!("{n} channels"),
            };
            super::rows::text_row(ui, theme, "Source", text);
        }

        // Channel routing dropdown
        {
            let prev = channel_config;
            let (row_rect, _) = ui.allocate_exact_size(egui::vec2(w, row_h + 4.0), egui::Sense::hover());
            ui.painter().text(
                egui::pos2(row_rect.min.x + pad, row_rect.center().y),
                egui::Align2::LEFT_CENTER, "Channel",
                egui::FontId::proportional(super::rows::TEXT), theme.text_secondary,
            );
            let combo_w = 110.0;
            let combo_rect = egui::Rect::from_min_size(
                egui::pos2(row_rect.max.x - pad - combo_w, row_rect.center().y - 13.0),
                egui::vec2(combo_w, 26.0),
            );
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(combo_rect), |ui| {
                egui::ComboBox::from_id_salt("channel_config")
                    .selected_text(channel_config_label(channel_config))
                    .width(combo_w - 10.0)
                    .show_ui(ui, |ui| {
                        for &variant in CHANNEL_CONFIG_VARIANTS {
                            ui.selectable_value(&mut channel_config, variant, channel_config_label(variant));
                        }
                    });
            });
            if channel_config != prev {
                cmds.push(AppCommand::UpdateClipAudio {
                    clip_id, gain, volume, pan,
                    fade_in_secs: fade_in, fade_out_secs: fade_out,
                    fade_in_curve, fade_out_curve, channel_config,
                });
            }
        }

        ui.add_space(4.0);

        // Keyframe state helpers
        let kf_tolerance = keyframe_tolerance;
        let gain_ks = build_kf_state(&clip.gain_keyframes, clip_time_secs, kf_tolerance);
        let vol_ks = build_kf_state(&clip.volume_keyframes, clip_time_secs, kf_tolerance);
        let pan_ks = build_kf_state(&clip.pan_keyframes, clip_time_secs, kf_tolerance);

        let gain_prev_kf = gain_ks.prev_keyframe_time;
        let gain_next_kf = gain_ks.next_keyframe_time;
        let vol_prev_kf = vol_ks.prev_keyframe_time;
        let vol_next_kf = vol_ks.next_keyframe_time;
        let pan_prev_kf = pan_ks.prev_keyframe_time;
        let pan_next_kf = pan_ks.next_keyframe_time;

        let drag_key = format!("{:?}:audio", clip_id);

        // ── Gain slider ──
        crate::param_focus::tag(ui, clip, crate::state::KeyframeParamKind::Audio(AudioParam::Gain));
        let gain_resp = slider_row(
            ui, theme, "gain", "Gain", &mut gain, 0.0..=4.0, 1.0, 0.005, 2, "",
            Some(gain_ks), false,
        );
        handle_audio_param(cmds, &gain_resp, &drag_key, clip, clip_id, gain, volume, pan, fade_in, fade_out, fade_in_curve, fade_out_curve, channel_config, AudioParam::Gain, clip_time_secs, kf_tolerance, clip_timeline_in_secs, clip_rate, gain_prev_kf, gain_next_kf);

        // ── Volume slider ──
        ui.add_space(4.0);
        crate::param_focus::tag(ui, clip, crate::state::KeyframeParamKind::Audio(AudioParam::Volume));
        let vol_resp = slider_row(
            ui, theme, "volume", "Volume", &mut volume, 0.0..=2.0, 1.0, 0.005, 2, "",
            Some(vol_ks), false,
        );
        handle_audio_param(cmds, &vol_resp, &drag_key, clip, clip_id, gain, volume, pan, fade_in, fade_out, fade_in_curve, fade_out_curve, channel_config, AudioParam::Volume, clip_time_secs, kf_tolerance, clip_timeline_in_secs, clip_rate, vol_prev_kf, vol_next_kf);

        // ── Pan slider ──
        ui.add_space(4.0);
        crate::param_focus::tag(ui, clip, crate::state::KeyframeParamKind::Audio(AudioParam::Pan));
        let pan_resp = slider_row(
            ui, theme, "pan", "Pan", &mut pan, -1.0..=1.0, 0.0, 0.005, 2, "",
            Some(pan_ks), true,
        );
        handle_audio_param(cmds, &pan_resp, &drag_key, clip, clip_id, gain, volume, pan, fade_in, fade_out, fade_in_curve, fade_out_curve, channel_config, AudioParam::Pan, clip_time_secs, kf_tolerance, clip_timeline_in_secs, clip_rate, pan_prev_kf, pan_next_kf);

        // ── Fade In ──
        ui.add_space(4.0);
        let fi_resp = slider_row(
            ui, theme, "fade_in", "Fade In", &mut fade_in, 0.0..=10.0, 0.0, 0.02, 2, " s",
            None, false,
        );
        handle_fade_param(cmds, &fi_resp, &drag_key, clip, clip_id, gain, volume, pan, fade_in, fade_out, fade_in_curve, fade_out_curve, channel_config);

        // Fade In Curve
        {
            let prev = fade_in_curve;
            let (row_rect, _) = ui.allocate_exact_size(egui::vec2(w, row_h + 4.0), egui::Sense::hover());
            ui.painter().text(
                egui::pos2(row_rect.min.x + pad, row_rect.center().y),
                egui::Align2::LEFT_CENTER, "Curve",
                egui::FontId::proportional(super::rows::TEXT), theme.text_secondary,
            );
            let combo_w = 110.0;
            let combo_rect = egui::Rect::from_min_size(
                egui::pos2(row_rect.max.x - pad - combo_w, row_rect.center().y - 13.0),
                egui::vec2(combo_w, 26.0),
            );
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(combo_rect), |ui| {
                egui::ComboBox::from_id_salt("fade_in_curve")
                    .selected_text(fade_curve_label(fade_in_curve))
                    .width(combo_w - 10.0)
                    .show_ui(ui, |ui| {
                        for variant in FADE_CURVES {
                            ui.selectable_value(&mut fade_in_curve, *variant, fade_curve_label(*variant));
                        }
                    });
            });
            if fade_in_curve != prev {
                cmds.push(AppCommand::UpdateClipAudio {
                    clip_id, gain, volume, pan,
                    fade_in_secs: fade_in, fade_out_secs: fade_out,
                    fade_in_curve, fade_out_curve, channel_config,
                });
            }
        }

        // ── Fade Out ──
        ui.add_space(4.0);
        let fo_resp = slider_row(
            ui, theme, "fade_out", "Fade Out", &mut fade_out, 0.0..=10.0, 0.0, 0.02, 2, " s",
            None, false,
        );
        handle_fade_param(cmds, &fo_resp, &drag_key, clip, clip_id, gain, volume, pan, fade_in, fade_out, fade_in_curve, fade_out_curve, channel_config);

        // Fade Out Curve
        {
            let prev = fade_out_curve;
            let (row_rect, _) = ui.allocate_exact_size(egui::vec2(w, row_h + 4.0), egui::Sense::hover());
            ui.painter().text(
                egui::pos2(row_rect.min.x + pad, row_rect.center().y),
                egui::Align2::LEFT_CENTER, "Curve",
                egui::FontId::proportional(super::rows::TEXT), theme.text_secondary,
            );
            let combo_w = 110.0;
            let combo_rect = egui::Rect::from_min_size(
                egui::pos2(row_rect.max.x - pad - combo_w, row_rect.center().y - 13.0),
                egui::vec2(combo_w, 26.0),
            );
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(combo_rect), |ui| {
                egui::ComboBox::from_id_salt("fade_out_curve")
                    .selected_text(fade_curve_label(fade_out_curve))
                    .width(combo_w - 10.0)
                    .show_ui(ui, |ui| {
                        for variant in FADE_CURVES {
                            ui.selectable_value(&mut fade_out_curve, *variant, fade_curve_label(*variant));
                        }
                    });
            });
            if fade_out_curve != prev {
                cmds.push(AppCommand::UpdateClipAudio {
                    clip_id, gain, volume, pan,
                    fade_in_secs: fade_in, fade_out_secs: fade_out,
                    fade_in_curve, fade_out_curve, channel_config,
                });
            }
        }
        ui.add_space(4.0);
    });

    ui.add_space(8.0);

    // ── BEHAVIORS section ──
    section_card(ui, "BEHAVIORS", theme, |ui| {
        ui.add_space(4.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: 14.0, right: 14.0, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
                use klipp_core::ParamId;
                let audio_params = [ParamId::Volume, ParamId::Pan, ParamId::Gain];
                super::modifier_ui::show_behaviors_content(ui, theme, clip, &audio_params, cmds);
            });
        ui.add_space(4.0);
    });

    ui.add_space(8.0);

    // ── LEVEL section ──
    section_card(ui, "LEVEL", theme, |ui| {
        ui.add_space(6.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: 14.0, right: 14.0, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
                draw_stereo_meter(ui, theme, peak.0, peak.1);
            });
        ui.add_space(6.0);
    });

    ui.add_space(8.0);

    // ── WAVEFORM section ──
    section_card(ui, "WAVEFORM", theme, |ui| {
        ui.add_space(6.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: 14.0, right: 14.0, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
        egui::Frame::default()
            .fill(theme.bg_raised)
            .stroke(egui::Stroke::new(1.0_f32, theme.border))
            .rounding(4.0)
            .inner_margin(egui::Margin::symmetric(4.0, 4.0))
            .show(ui, |ui| {
                // Measured inside the frame's margin: from outside it, the
                // waveform came out 8 points wider than the column.
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 48.0), egui::Sense::hover());
                let clip_peaks = waveform_peaks.map(|peaks| {
                    if peaks.is_empty() { return peaks; }
                    let total_dur = peaks.len() as f64;
                    let src_in = clip.source_in.to_secs_f64();
                    let src_out = clip.source_out.to_secs_f64();
                    let clip_dur = (src_out - src_in).max(0.0);
                    let media_dur = src_out.max(clip_dur + src_in);
                    if media_dur <= 0.0 { return peaks; }
                    let start = ((src_in / media_dur) * total_dur) as usize;
                    let end = (((src_in + clip_dur) / media_dur) * total_dur) as usize;
                    &peaks[start.min(peaks.len())..end.min(peaks.len())]
                });
                waveform::draw_role_waveform(ui.painter(), rect, theme.role_dialogue, clip_peaks);
            });
            }); // close padding frame
        ui.add_space(4.0);
    });

    ui.add_space(8.0);

    // ── AUDIO EFFECTS section ──
    section_card(ui, "AUDIO EFFECTS", theme, |ui| {
        ui.add_space(4.0);
        egui::Frame::default()
            .inner_margin(egui::Margin { left: 14.0, right: 14.0, top: 0.0, bottom: 0.0 })
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE)
            .shadow(egui::Shadow::NONE)
            .show(ui, |ui| {
                let clip_timeline_in_secs = clip.timeline_in.to_secs_f64();
                let clip_rate = clip.timeline_in.rate;
                super::audio_fx_inspector::show_content(
                    ui, theme, clip_id,
                    &clip.audio_effects, &clip.audio_effect_keyframes,
                    clip_time_secs, clip_timeline_in_secs, clip_rate,
                    &state.ladspa_plugins, cmds,
                    keyframe_tolerance,
                );
            });
        ui.add_space(4.0);
    });

    // ── AUTO DUCK section (only for timeline clips) ──
    if is_timeline {
        ui.add_space(8.0);
        section_card(ui, "AUTO DUCK", theme, |ui| {
            let w = ui.available_width();
            let pad = 14.0;
            let row_h = 30.0;

            // Sidechain dropdown — right-aligned to match slider value boxes
            {
                let (row_rect, _) = ui.allocate_exact_size(egui::vec2(w, row_h + 4.0), egui::Sense::hover());
                ui.painter().text(
                    egui::pos2(row_rect.min.x + pad, row_rect.center().y),
                    egui::Align2::LEFT_CENTER, "Sidechain",
                    egui::FontId::proportional(super::rows::TEXT), theme.text_secondary,
                );
                let combo_w = 110.0;
                let combo_rect = egui::Rect::from_min_size(
                    egui::pos2(row_rect.max.x - pad - combo_w, row_rect.center().y - 13.0),
                    egui::vec2(combo_w, 26.0),
                );
                let current_label = tracks.iter()
                    .find(|t| t.index == duck.sidechain_track)
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| "None".to_string());
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(combo_rect), |ui| {
                    egui::ComboBox::from_id_salt("duck_sidechain")
                        .selected_text(&current_label)
                        .width(combo_w - 10.0)
                        .show_ui(ui, |ui| {
                            for t in tracks {
                                ui.selectable_value(&mut duck.sidechain_track, t.index, &t.name);
                            }
                        });
                });
            }

            // Duck Level
            let empty_ks = Some(KeyframeState {
                keys_in_frame: 0,
                has_keyframe_at_playhead: false,
                has_any_keyframes: false,
                prev_keyframe_time: None,
                next_keyframe_time: None,
                current_interpolation: None,
            });
            slider_row(ui, theme, "duck_level", "Duck Level", &mut duck.level, 0.0..=1.0, 0.2, 0.005, 2, "", empty_ks, false);
            ui.add_space(4.0);
            slider_row(ui, theme, "duck_attack", "Attack", &mut duck.attack, 0.01..=2.0, 0.1, 0.01, 2, " s", empty_ks, false);
            ui.add_space(4.0);
            slider_row(ui, theme, "duck_release", "Release", &mut duck.release, 0.01..=2.0, 0.3, 0.01, 2, " s", empty_ks, false);
            ui.add_space(4.0);
            slider_row(ui, theme, "duck_threshold", "Threshold", &mut duck.threshold, 0.0..=1.0, 0.1, 0.005, 2, "", empty_ks, false);

            ui.add_space(4.0);
            // Self-ducking guard — target track cannot be its own sidechain source.
            let is_self_duck = duck.sidechain_track == track_idx;
            ui.horizontal(|ui| {
                ui.add_space(14.0);
                let btn = ui.add_enabled(
                    !is_self_duck,
                    egui::Button::new(
                        egui::RichText::new("Apply Auto Duck").size(11.0).color(theme.text_primary),
                    )
                    .fill(theme.accent.linear_multiply(0.3))
                    .stroke(egui::Stroke::new(1.0_f32, theme.accent))
                    .min_size(egui::vec2(ui.available_width() - 14.0, 24.0)),
                );
                if btn.clicked() {
                    cmds.push(AppCommand::AutoDuckAudio {
                        target_clip_id: clip_id,
                        sidechain_track_idx: duck.sidechain_track,
                        duck_level: duck.level,
                        attack_secs: duck.attack,
                        release_secs: duck.release,
                        threshold: duck.threshold,
                    });
                }
            });
            if is_self_duck {
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.label(
                        egui::RichText::new("Sidechain source cannot be the same track as the target.")
                            .size(super::rows::TEXT)
                            .color(theme.marker_warning),
                    );
                });
            }
            ui.add_space(4.0);
        });
    }

    ui.add_space(16.0);
}

// ══════════════════════════════════════════════════
// Helpers
// ══════════════════════════════════════════════════

fn build_kf_state(keyframes: &[klipp_core::Keyframe], time: f64, tol: f64) -> KeyframeState {
    crate::inspector::keyframe_selection::state(keyframes, time, tol)
}

#[allow(clippy::too_many_arguments)]
fn handle_audio_param(
    cmds: &mut Vec<AppCommand>,
    resp: &SliderResponse,
    drag_key: &str,
    clip: &Clip,
    clip_id: klipp_core::ClipId,
    gain: f32, volume: f32, pan: f32,
    fade_in: f32, fade_out: f32,
    fade_in_curve: FadeCurve, fade_out_curve: FadeCurve,
    channel_config: ChannelConfig,
    param: AudioParam,
    clip_time_secs: f64,
    kf_tolerance: f64,
    clip_timeline_in_secs: f64,
    clip_rate: u32,
    prev_kf: Option<f64>,
    next_kf: Option<f64>,
) {
    let keyframes = match param {
        AudioParam::Gain => &clip.gain_keyframes,
        AudioParam::Volume => &clip.volume_keyframes,
        AudioParam::Pan => &clip.pan_keyframes,
    };
    let value = match param {
        AudioParam::Gain => gain,
        AudioParam::Volume => volume,
        AudioParam::Pan => pan,
    };

    if resp.drag_started {
        cmds.push(AppCommand::SetRubberBandParam(param));
        cmds.push(AppCommand::BeginAudioDrag {
            key: drag_key.to_string(), clip_id,
            old_gain: clip.gain, old_volume: clip.volume, old_pan: clip.pan,
            old_fade_in: clip.fade_in_secs, old_fade_out: clip.fade_out_secs,
        });
    }
    if resp.changed {
        cmds.push(AppCommand::UpdateClipAudio {
            clip_id, gain, volume, pan,
            fade_in_secs: fade_in, fade_out_secs: fade_out,
            fade_in_curve, fade_out_curve, channel_config,
        });
        if !keyframes.is_empty() {
            cmds.push(AppCommand::AddAudioKeyframe { clip_id, param, time_secs: clip_time_secs, value });
        }
    }
    if resp.drag_stopped {
        cmds.push(AppCommand::EndAudioDrag {
            key: drag_key.to_string(), clip_id,
            new_gain: gain, new_volume: volume, new_pan: pan,
            new_fade_in: fade_in, new_fade_out: fade_out,
        });
    }
    if let Some(interp) = resp.keyframe_set_interpolation {
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetAudioKeyframeInterpolation { clip_id, param, keyframe_index: ki, interpolation: interp });
        }
    }
    if let Some(preset) = resp.keyframe_ease_preset {
        if let Some(ki) = crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, kf_tolerance) {
            cmds.push(AppCommand::SetAudioKeyframeInterpolation { clip_id, param, keyframe_index: ki, interpolation: klipp_core::Interpolation::Bezier });
            for idx in [ki.saturating_sub(1), ki] {
                if idx >= keyframes.len() { continue; }
                let (t_in, t_out) = preset.compute_tangents(keyframes, idx);
                cmds.push(AppCommand::SetAudioKeyframeTangent { clip_id, param, keyframe_index: idx, tangent_in: t_in, tangent_out: t_out, handles: None });
            }
        }
    }
    if resp.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, kf_tolerance).is_some() {
            cmds.push(AppCommand::RemoveAudioKeyframe { clip_id, param, time_secs: clip_time_secs });
        } else {
            cmds.push(AppCommand::AddAudioKeyframe { clip_id, param, time_secs: clip_time_secs, value });
        }
        cmds.push(AppCommand::SetRubberBandParam(param));
    }
    if resp.keyframe_nav_prev {
        if let Some(t) = prev_kf {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }
    if resp.keyframe_nav_next {
        if let Some(t) = next_kf {
            cmds.push(AppCommand::SeekTo(Timecode::from_secs_f64(clip_timeline_in_secs + t, clip_rate)));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_fade_param(
    cmds: &mut Vec<AppCommand>,
    resp: &SliderResponse,
    drag_key: &str,
    clip: &Clip,
    clip_id: klipp_core::ClipId,
    gain: f32, volume: f32, pan: f32,
    fade_in: f32, fade_out: f32,
    fade_in_curve: FadeCurve, fade_out_curve: FadeCurve,
    channel_config: ChannelConfig,
) {
    if resp.drag_started {
        cmds.push(AppCommand::BeginAudioDrag {
            key: drag_key.to_string(), clip_id,
            old_gain: clip.gain, old_volume: clip.volume, old_pan: clip.pan,
            old_fade_in: clip.fade_in_secs, old_fade_out: clip.fade_out_secs,
        });
    }
    if resp.changed {
        cmds.push(AppCommand::UpdateClipAudio {
            clip_id, gain, volume, pan,
            fade_in_secs: fade_in, fade_out_secs: fade_out,
            fade_in_curve, fade_out_curve, channel_config,
        });
    }
    if resp.drag_stopped {
        cmds.push(AppCommand::EndAudioDrag {
            key: drag_key.to_string(), clip_id,
            new_gain: gain, new_volume: volume, new_pan: pan,
            new_fade_in: fade_in, new_fade_out: fade_out,
        });
    }
}

// ── Section card (reusable collapsible) ──

fn section_card(
    ui: &mut egui::Ui,
    title: &str,
    theme: &KlippTheme,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    crate::widgets::inspector_group::section_card(ui, title, None, theme, add_contents);
}

/// Draw a padded horizontal separator line inside a card.
fn draw_keyframe_diamond(ui: &mut egui::Ui, center: egui::Pos2, color: egui::Color32, filled: bool) {
    let s = 5.0;
    let pts = vec![
        egui::pos2(center.x, center.y - s),
        egui::pos2(center.x + s, center.y),
        egui::pos2(center.x, center.y + s),
        egui::pos2(center.x - s, center.y),
    ];
    if filled {
        ui.painter().add(egui::Shape::convex_polygon(pts, color, egui::Stroke::NONE));
    } else {
        ui.painter().add(egui::Shape::convex_polygon(pts, egui::Color32::TRANSPARENT, egui::Stroke::new(1.5_f32, color)));
    }
}

pub(crate) const FADE_CURVES: &[FadeCurve] = &[
    FadeCurve::Linear, FadeCurve::Exponential, FadeCurve::Logarithmic, FadeCurve::SCurve,
    FadeCurve::ConstantPower,
];

const CHANNEL_CONFIG_VARIANTS: &[ChannelConfig] = &[
    ChannelConfig::Default, ChannelConfig::MonoDownmix, ChannelConfig::SwapLR,
    ChannelConfig::LeftOnly, ChannelConfig::RightOnly,
];

fn channel_config_label(config: ChannelConfig) -> &'static str {
    match config {
        ChannelConfig::Default => "Default",
        ChannelConfig::MonoDownmix => "Mono Downmix",
        ChannelConfig::SwapLR => "Swap L/R",
        ChannelConfig::Surround51 => "5.1 Surround",
        ChannelConfig::Surround71 => "7.1 Surround",
        ChannelConfig::LeftOnly => "Left Only",
        ChannelConfig::RightOnly => "Right Only",
    }
}

pub(crate) fn fade_curve_label(curve: FadeCurve) -> &'static str {
    match curve {
        FadeCurve::Linear => "Linear",
        FadeCurve::Exponential => "Exponential",
        FadeCurve::Logarithmic => "Logarithmic",
        FadeCurve::SCurve => "S-Curve",
        // Named for the job rather than the maths: an editor reaches
        // for this when two sounds overlap, not when one ends.
        FadeCurve::ConstantPower => "Constant Power (crossfade)",
    }
}

// ── Level Meter ──

#[derive(Clone, Default)]
struct MeterState {
    smooth_l: f32,
    smooth_r: f32,
    peak_hold_l: f32,
    peak_hold_r: f32,
    peak_hold_age_l: f32,
    peak_hold_age_r: f32,
}

fn draw_stereo_meter(ui: &mut egui::Ui, theme: &KlippTheme, peak_l: f32, peak_r: f32) {
    let dt = ui.input(|i| i.stable_dt).min(0.1);
    let meter_id = ui.id().with("audio_meter_state");
    let mut ms: MeterState = ui.ctx().data_mut(|d| d.get_temp(meter_id).unwrap_or_default());

    let release_coeff = (-dt / 0.15).exp();
    ms.smooth_l = if peak_l > ms.smooth_l { peak_l } else { ms.smooth_l * release_coeff };
    ms.smooth_r = if peak_r > ms.smooth_r { peak_r } else { ms.smooth_r * release_coeff };

    let hold_time = 2.0;
    let hold_decay = 0.3;
    if peak_l >= ms.peak_hold_l { ms.peak_hold_l = peak_l; ms.peak_hold_age_l = 0.0; }
    else { ms.peak_hold_age_l += dt; if ms.peak_hold_age_l > hold_time { ms.peak_hold_l = (ms.peak_hold_l - hold_decay * dt).max(0.0); } }
    if peak_r >= ms.peak_hold_r { ms.peak_hold_r = peak_r; ms.peak_hold_age_r = 0.0; }
    else { ms.peak_hold_age_r += dt; if ms.peak_hold_age_r > hold_time { ms.peak_hold_r = (ms.peak_hold_r - hold_decay * dt).max(0.0); } }

    ui.ctx().data_mut(|d| d.insert_temp(meter_id, ms.clone()));

    let meter_w = ui.available_width() - 20.0;
    let bar_h = 6.0;
    let gap = 2.0;
    let label_w = 14.0;
    let total_h = bar_h * 2.0 + gap;
    let (meter_rect, _) = ui.allocate_exact_size(egui::vec2(meter_w, total_h), egui::Sense::hover());
    let painter = ui.painter();
    let bar_w = meter_w - label_w - 4.0;
    let bar_left = meter_rect.left() + label_w + 4.0;
    let max_level = 1.5f32;

    let smoothed = [ms.smooth_l, ms.smooth_r];
    let peak_holds = [ms.peak_hold_l, ms.peak_hold_r];

    for (ch, label) in ["L", "R"].iter().enumerate() {
        let y = meter_rect.top() + ch as f32 * (bar_h + gap);
        let level = smoothed[ch];
        let hold = peak_holds[ch];

        painter.text(
            egui::pos2(meter_rect.left() + label_w / 2.0, y + bar_h / 2.0),
            egui::Align2::CENTER_CENTER, *label, egui::FontId::monospace(10.0), theme.text_dim,
        );

        let bar = egui::Rect::from_min_size(egui::pos2(bar_left, y), egui::vec2(bar_w, bar_h));
        painter.rect_filled(bar, 2.0, theme.bg_deep);

        let fill = (level.min(max_level) / max_level * bar_w).max(0.0);
        if fill > 0.0 {
            let green_end = bar_w * 0.25;
            let yellow_end = bar_w * 0.7;
            let green_w = fill.min(green_end);
            if green_w > 0.0 {
                painter.rect_filled(egui::Rect::from_min_size(egui::pos2(bar_left, y), egui::vec2(green_w, bar_h)), 2.0, theme.marker_success);
            }
            if fill > green_end {
                let yellow_w = (fill - green_end).min(yellow_end - green_end);
                painter.rect_filled(egui::Rect::from_min_size(egui::pos2(bar_left + green_end, y), egui::vec2(yellow_w, bar_h)), 0.0, theme.marker_warning);
            }
            if fill > yellow_end {
                let red_w = fill - yellow_end;
                painter.rect_filled(egui::Rect::from_min_size(egui::pos2(bar_left + yellow_end, y), egui::vec2(red_w, bar_h)), 0.0, theme.marker_error);
            }
        }

        if hold > 0.005 {
            let hold_x = bar_left + (hold.min(max_level) / max_level * bar_w);
            let hold_color = if hold > 0.9 { theme.marker_error }
                else if hold > 0.7 { theme.marker_warning }
                else { theme.marker_success };
            painter.rect_filled(egui::Rect::from_min_size(egui::pos2(hold_x - 1.0, y), egui::vec2(2.0, bar_h)), 0.0, hold_color);
        }
    }

    ui.add_space(2.0);
    let (tick_rect, _) = ui.allocate_exact_size(egui::vec2(meter_w, 10.0), egui::Sense::hover());
    let tick_left = tick_rect.left() + label_w + 4.0;
    let tick_w = meter_w - label_w - 4.0;
    let db_marks: &[(f32, &str)] = &[(0.004, "-48"), (0.06, "-24"), (0.25, "-12"), (0.5, "-6"), (0.7, "-3"), (1.0, "0")];
    for &(level, label) in db_marks {
        let x = tick_left + (level / max_level) * tick_w;
        if x <= tick_left + tick_w {
            ui.painter().text(egui::pos2(x, tick_rect.center().y), egui::Align2::CENTER_CENTER, label, egui::FontId::monospace(10.0), theme.text_dim);
        }
    }

    if ms.smooth_l > 0.001 || ms.smooth_r > 0.001 || ms.peak_hold_l > 0.005 || ms.peak_hold_r > 0.005 {
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod sequence_properties_tests {
    use super::*;
    use crate::inspector::rows::test_frame::frame;

    fn texts(state: &AppState) -> Vec<String> {
        let ctx = egui::Context::default();
        let (texts, _) = frame(&ctx, Vec::new(), |ui, theme, _| show_sequence_properties(ui, theme, state));
        texts.into_iter().map(|(t, _)| t).collect()
    }

    /// With nothing selected the Audio tab shows the real output: the
    /// device's state and its sample rate, and the master meter. Nothing
    /// in it is a fixed word posing as a setting.
    #[test]
    fn the_output_is_shown_as_it_is() {
        let state = AppState { audio_backend_ok: true, audio_sample_rate: 44_100, ..Default::default() };
        let all = texts(&state);
        for label in ["OUTPUT", "Device", "Open", "Sample rate", "44.1 kHz", "LOUDNESS METER"] {
            assert!(all.iter().any(|t| t == label), "{label}: {all:?}");
        }
        for fake in ["24-bit", "Stereo (L/R)", "H.264 / AAC", "High", "SDR", "BT.709", "RENDER SETTINGS", "MASTER VOLUME", "Bit depth", "Channels"] {
            assert!(!all.iter().any(|t| t == fake), "{fake} is not a real setting here: {all:?}");
        }

        let state = AppState { audio_backend_ok: false, ..Default::default() };
        let all = texts(&state);
        assert!(all.iter().any(|t| t == "Not open"), "{all:?}");
        assert!(!all.iter().any(|t| t == "Sample rate"), "no rate without a device: {all:?}");
    }

    /// What left this view is still shown where it belongs: the
    /// sequence's name, format and proxies switch on the Video tab's
    /// nothing-selected view.
    #[test]
    fn what_moved_is_on_the_video_tab() {
        let state = AppState::default();
        let ctx = egui::Context::default();
        let (texts, _) = frame(&ctx, Vec::new(), |ui, theme, cmds| crate::inspector::sequence_view::show(ui, theme, &state, cmds));
        let all: Vec<String> = texts.into_iter().map(|(t, _)| t).collect();
        let seq = state.sequence();
        assert!(all.contains(&seq.name), "{all:?}");
        assert!(all.contains(&crate::inspector::sequence_view::format_line(&state)), "{all:?}");
        assert!(all.contains(&crate::inspector::sequence_view::colour_line(&state)), "{all:?}");
        assert!(all.iter().any(|t| t == "Use proxies"), "{all:?}");
    }
}

#[cfg(test)]
mod clip_audio_tests {
    use super::*;
    use crate::inspector::rows::test_frame::{click, frame};

    fn clip() -> Clip {
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 24);
        Clip::media(klipp_core::MediaId::new(), t(0.0), t(2.0), t(0.0), t(2.0))
    }

    fn draw(ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>, state: &AppState, clip: &Clip, duck: &mut DuckSettings) {
        let tracks = [TrackInfo { index: 0, name: "Music".into() }, TrackInfo { index: 1, name: "Voice".into() }];
        show(ui, theme, state, Some((clip, 1, true)), cmds, (0.0, 0.0), None, Some(2), 0.5, duck, &tracks);
    }

    /// A clip's Audio tab keeps every row; its source reads as the file's
    /// channels, with no bars of made-up heights; and no rule runs
    /// between rows, only under each section header.
    #[test]
    fn every_row_is_kept_without_rules() {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let clip = clip();
        let mut duck = DuckSettings::default();
        let (texts, _) = frame(&ctx, Vec::new(), |ui, theme, cmds| draw(ui, theme, cmds, &state, &clip, &mut duck));
        let all: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        for label in [
            "ROLE", "AUDIO", "Source", "Stereo (2ch)", "Channel", "Gain", "Volume", "Pan", "Fade In", "Curve", "Fade Out",
            "BEHAVIORS", "LEVEL", "WAVEFORM", "AUDIO EFFECTS", "AUTO DUCK", "Sidechain", "Duck Level", "Attack", "Release", "Threshold", "Apply Auto Duck",
        ] {
            assert!(all.contains(&label), "{label} is no longer shown: {all:?}");
        }
        let theme = KlippTheme::default();
        let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 6000.0))), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| draw(ui, &theme, &mut Vec::new(), &state, &clip, &mut duck));
        });
        let y = |l: &str| texts.iter().find(|(t, _)| t == l).unwrap().1.y;
        let headers = ["ROLE", "AUDIO", "BEHAVIORS", "LEVEL", "WAVEFORM", "AUDIO EFFECTS", "AUTO DUCK"];
        for s in &out.shapes {
            if let egui::Shape::LineSegment { points, .. } = &s.shape {
                if points[0].y == points[1].y && (points[1].x - points[0].x) > 100.0 {
                    assert!(headers.iter().any(|h| (points[0].y - y(h)).abs() < 20.0), "a rule at {} is not a header's", points[0].y);
                }
            }
        }
    }

    /// The volume's keyframe mark keys the volume at the playhead, by a
    /// real click.
    #[test]
    fn the_keyframe_mark_keys_the_volume() {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let clip = clip();
        let mut duck = DuckSettings::default();
        let mut d = |ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>| draw(ui, theme, cmds, &state, &clip, &mut duck);
        let (texts, _) = frame(&ctx, Vec::new(), &mut d);
        let vy = texts.iter().find(|(t, _)| t == "Volume").unwrap().1.y;
        let cmds = click(&ctx, egui::pos2(360.0 - 8.0 - 14.0 - 5.0, vy), &mut d);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::AddAudioKeyframe { param: AudioParam::Volume, .. })), "{cmds:?}");
    }

    /// #2962: Volume typed into after a double-click, while something
    /// drawn before the inspector comes and goes (a status message): the
    /// field keeps the keys, Ctrl+A selects its text, and Enter sets the
    /// volume typed on this clip, as the app places the inspector.
    #[test]
    fn volume_typed_after_a_double_click_survives_a_layout_shift() {
        let ctx = egui::Context::default();
        let state = AppState::default();
        let clip = clip();
        let mut duck = DuckSettings::default();
        let theme = KlippTheme::default();
        let mut run = |events: Vec<egui::Event>, busy: bool| {
            let mut cmds = Vec::new();
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 2000.0))), events, ..Default::default() };
            let out = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if busy {
                        ui.label("Saving…");
                    }
                    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 40.0), egui::vec2(360.0, 1900.0));
                    crate::ui_util::panel_ui(ui, rect, "inspector_panel", |ui| draw(ui, &theme, &mut cmds, &state, &clip, &mut duck));
                });
            });
            let texts: Vec<(String, egui::Pos2)> = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.pos + t.galley.size() / 2.0)),
                _ => None,
            }).collect();
            (texts, cmds)
        };
        let (texts, _) = run(Vec::new(), false);
        let vy = texts.iter().find(|(t, _)| t == "Volume").unwrap().1.y;
        let at = texts.iter().find(|(t, p)| t == "1.00" && (p.y - vy).abs() < 4.0).map(|(_, p)| *p).expect("Volume's 1.00");
        let b = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        for ev in [vec![egui::Event::PointerMoved(at)], vec![b(true)], vec![b(false)], vec![b(true)], vec![b(false)], vec![]] {
            run(ev, false);
        }
        let field = ctx.memory(|m| m.focused()).expect("the double-click gave Volume the keys");
        // The status message shows: the panels before the inspector drew
        // one more widget.
        run(vec![egui::Event::PointerGone], true);
        run(Vec::new(), true);
        assert_eq!(ctx.memory(|m| m.focused()), Some(field), "the field keeps the keys through the shift");
        let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
        let key = |key, modifiers| egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers };
        run(vec![key(egui::Key::A, ctrl)], true);
        let range = egui::TextEdit::load_state(&ctx, field).and_then(|s| s.cursor.char_range()).expect("a selection");
        assert_eq!((range.primary.index.min(range.secondary.index), range.primary.index.max(range.secondary.index)), (0, "1.00".len()), "Ctrl+A selected the field's text");
        assert!(ctx.wants_keyboard_input(), "and the field still has the keys, so Select All is not read");
        run(vec![egui::Event::Text("0.5".into())], false);
        let (_, cmds) = run(vec![key(egui::Key::Enter, egui::Modifiers::NONE)], false);
        let set: Vec<_> = cmds.iter().filter_map(|c| match c {
            AppCommand::UpdateClipAudio { clip_id, volume, .. } => Some((*clip_id, *volume)),
            _ => None,
        }).collect();
        assert_eq!(set, vec![(clip.id, 0.5)], "Enter set the volume typed, on this clip: {cmds:?}");
    }
}
