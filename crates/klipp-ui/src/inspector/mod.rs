pub mod video_tab;
pub mod audio_tab;
pub mod audio_fx_inspector;
pub mod effects_tab;
pub mod info_tab;
pub mod modifier_ui;

use crate::layout_state::InspectorTab;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

#[derive(Default)]
pub struct NewInspectorPanel {
    show_add_builtin: bool,
    show_add_ofx: bool,
    pub tag_input: String,
    pub duck: audio_tab::DuckSettings,
}


impl NewInspectorPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        theme: &KlippTheme,
        active_tab: &mut InspectorTab,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // Resolve selected clip
        let clip_id = state.selected_clip;
        let clip_info = clip_id.and_then(|cid| {
            state
                .sequence()
                .tracks
                .iter()
                .enumerate()
                .find_map(|(ti, t)| t.find_clip(cid).map(|c| (ti, c)))
        });
        let media = clip_info
            .and_then(|(_, c)| c.media_id().and_then(|mid| state.media_pool.get(&mid)));

        // ── Header ──
        let header_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), 42.0),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(header_rect), |ui| {
            ui.painter().rect_filled(header_rect, 0.0, theme.bg_deep);
            ui.painter().hline(
                header_rect.x_range(),
                header_rect.max.y,
                egui::Stroke::new(1.0, theme.border),
            );
            ui.add_space(9.0);
            ui.vertical(|ui| {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("INSPECTOR")
                        .size(10.0)
                        .color(theme.text_dim),
                );
                let (clip_name, name_color) = if let Some((_, c)) = clip_info {
                    let name = match &c.kind {
                        klipp_timeline::ClipKind::Title(td) => {
                            let text_preview = if td.text.len() > 30 { format!("{}...", &td.text[..27]) } else { td.text.clone() };
                            format!("Title: {}", text_preview)
                        }
                        klipp_timeline::ClipKind::Gap => "Gap".into(),
                        _ => media.map(|m| m.filename.clone()).unwrap_or_else(|| "Clip".into()),
                    };
                    (name, theme.text_primary)
                } else {
                    ("No selection".into(), theme.text_secondary)
                };
                ui.label(
                    egui::RichText::new(clip_name)
                        .size(13.0)
                        .strong()
                        .color(name_color),
                );
            });
        });
        ui.advance_cursor_after_rect(header_rect);

        // ── Tab bar ──
        let tab_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            egui::vec2(ui.available_width(), 28.0),
        );
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(tab_rect), |ui| {
            ui.painter().rect_filled(tab_rect, 0.0, theme.bg_deep);
            ui.painter().hline(
                tab_rect.x_range(),
                tab_rect.max.y,
                egui::Stroke::new(1.0, theme.border),
            );
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let is_title_clip = clip_info.as_ref().map_or(false, |(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Title(_)));
                let tabs: Vec<(InspectorTab, &str)> = if is_title_clip {
                    vec![
                        (InspectorTab::Title, "TITLE"),
                        (InspectorTab::Video, "VIDEO"),
                        (InspectorTab::Effects, "EFFECTS"),
                        (InspectorTab::Info, "INFO"),
                    ]
                } else {
                    vec![
                        (InspectorTab::Video, "VIDEO"),
                        (InspectorTab::Audio, "AUDIO"),
                        (InspectorTab::Effects, "EFFECTS"),
                        (InspectorTab::Info, "INFO"),
                    ]
                };
                let tab_w = ui.available_width() / tabs.len() as f32;
                for (tab, label) in &tabs {
                    let is_active = *active_tab == *tab;

                    let (tab_rect, resp) = ui.allocate_exact_size(
                        egui::vec2(tab_w, 26.0),
                        egui::Sense::click(),
                    );

                    let text_color = if is_active {
                        theme.text_accent
                    } else if resp.hovered() {
                        theme.text_secondary
                    } else {
                        theme.text_dim
                    };

                    ui.painter().text(
                        tab_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        *label,
                        egui::FontId::proportional(10.0),
                        text_color,
                    );

                    if is_active {
                        ui.painter().hline(
                            tab_rect.x_range(),
                            tab_rect.max.y - 1.0,
                            egui::Stroke::new(2.0, theme.accent),
                        );
                    }
                    if resp.clicked() {
                        *active_tab = *tab;
                    }
                }
            });
        });
        ui.advance_cursor_after_rect(tab_rect);

        // Auto-switch to Title tab when a title clip is selected (and away when not)
        let is_title_clip = clip_info.as_ref().map_or(false, |(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Title(_)));
        if is_title_clip && *active_tab != InspectorTab::Title {
            *active_tab = InspectorTab::Title;
        } else if !is_title_clip && *active_tab == InspectorTab::Title {
            *active_tab = InspectorTab::Video;
        }

        // ── Tab content ──
        egui::ScrollArea::vertical().id_salt("inspector_body_scroll").show(ui, |ui| {
            ui.add_space(10.0);
            match active_tab {
                InspectorTab::Video => {
                    if let Some((_, clip)) = clip_info {
                        let clip_time_secs = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
                        video_tab::show(ui, theme, clip, &mut cmds, clip_time_secs);
                    } else {
                        no_selection(ui, theme);
                    }
                }
                InspectorTab::Audio => {
                    if let Some((track_idx, clip)) = clip_info {
                        let peak = audio_tab::find_clip_peak_levels(clip, track_idx, state);
                        let media_peaks: Option<Vec<f32>> = clip.media_id()
                            .and_then(|mid| state.media_pool.get(&mid))
                            .and_then(|item| item.audio_cache.as_ref())
                            .map(|cache| cache.peaks.iter().map(|(lo, hi)| lo.abs().max(hi.abs())).collect());
                        let peaks_slice = media_peaks.as_deref();
                        let source_channels = clip.media_id()
                            .and_then(|mid| state.media_pool.get(&mid))
                            .and_then(|item| item.audio_channels);
                        let clip_time_secs = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
                        let track_infos: Vec<audio_tab::TrackInfo> = state.sequence().tracks.iter().enumerate()
                            .map(|(i, t)| audio_tab::TrackInfo {
                                index: i,
                                name: if t.name.is_empty() { format!("Track {}", i + 1) } else { t.name.clone() },
                            })
                            .collect();
                        audio_tab::show(ui, theme, clip, &mut cmds, peak, peaks_slice, source_channels, clip_time_secs, &mut self.duck, &track_infos);
                    } else {
                        no_selection(ui, theme);
                    }
                }
                InspectorTab::Effects => {
                    if let Some((_, clip)) = clip_info {
                        let clip_time_secs = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
                        effects_tab::show(ui, theme, state, clip, &mut cmds, &mut self.show_add_builtin, &mut self.show_add_ofx, clip_time_secs);
                    } else {
                        no_selection(ui, theme);
                    }
                }
                InspectorTab::Title => {
                    if let Some((_, clip)) = clip_info {
                        if let klipp_timeline::ClipKind::Title(_) = &clip.kind {
                            show_title_editor(ui, theme, clip, state, &mut cmds);
                        } else {
                            no_selection(ui, theme);
                        }
                    } else {
                        no_selection(ui, theme);
                    }
                }
                InspectorTab::Info => {
                    if let (Some((track_idx, clip)), Some(media)) = (clip_info, media) {
                        info_tab::show(ui, state, theme, track_idx, clip, media, &mut self.tag_input, &mut cmds);
                    } else {
                        no_selection(ui, theme);
                    }
                }
            }
        });

        cmds
    }
}

fn font_style_to_u8(style: klipp_timeline::FontStyle) -> u8 {
    match style {
        klipp_timeline::FontStyle::Regular => 0,
        klipp_timeline::FontStyle::Bold => 1,
        klipp_timeline::FontStyle::Italic => 2,
        klipp_timeline::FontStyle::BoldItalic => 3,
    }
}

fn show_title_editor(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    clip: &klipp_timeline::Clip,
    state: &AppState,
    cmds: &mut Vec<AppCommand>,
) {
    let clip_id = clip.id;
    let td = match &clip.kind {
        klipp_timeline::ClipKind::Title(td) => td,
        _ => return,
    };
    // Text input
    let mut text = td.text.clone();
    ui.label(egui::RichText::new("Text").strong().size(11.0));
    let response = ui.add(
        egui::TextEdit::multiline(&mut text)
            .desired_width(f32::INFINITY)
            .desired_rows(3)
            .font(egui::TextStyle::Monospace),
    );
    if response.changed() {
        cmds.push(AppCommand::UpdateTitleData {
            clip_id, text: text.clone(), font_size: td.font_size,
            font_name: td.font_name.clone(), font_style: font_style_to_u8(td.font_style), color: td.color,
        });
    }
    ui.add_space(8.0);

    // Font family
    ui.label(egui::RichText::new("Font").strong().size(11.0));
    let current_font = &td.font_name;
    let display_name = if current_font == "sans-serif" || current_font.is_empty() {
        "Noto Sans (Default)"
    } else { current_font };
    egui::ComboBox::from_id_salt("new_insp_title_font")
        .selected_text(display_name)
        .width(ui.available_width() - 8.0)
        .show_ui(ui, |ui| {
            if ui.selectable_label(current_font == "sans-serif" || current_font.is_empty(), "Noto Sans (Default)").clicked() {
                cmds.push(AppCommand::UpdateTitleData {
                    clip_id, text: td.text.clone(), font_size: td.font_size,
                    font_name: "sans-serif".to_string(), font_style: font_style_to_u8(td.font_style), color: td.color,
                });
            }
            ui.separator();
            for family in &state.available_fonts {
                if ui.selectable_label(current_font == family, family).clicked() {
                    cmds.push(AppCommand::UpdateTitleData {
                        clip_id, text: td.text.clone(), font_size: td.font_size,
                        font_name: family.clone(), font_style: font_style_to_u8(td.font_style), color: td.color,
                    });
                }
            }
        });
    ui.add_space(4.0);

    // Bold / Italic toggles
    let is_bold = matches!(td.font_style, klipp_timeline::FontStyle::Bold | klipp_timeline::FontStyle::BoldItalic);
    let is_italic = matches!(td.font_style, klipp_timeline::FontStyle::Italic | klipp_timeline::FontStyle::BoldItalic);
    ui.horizontal(|ui| {
        let bold_text = if is_bold { egui::RichText::new(" B ").strong() } else { egui::RichText::new(" B ").color(egui::Color32::from_rgb(120, 120, 130)) };
        let italic_text = if is_italic { egui::RichText::new(" I ").italics() } else { egui::RichText::new(" I ").color(egui::Color32::from_rgb(120, 120, 130)) };
        if ui.selectable_label(is_bold, bold_text).clicked() {
            let new_style = match (!is_bold, is_italic) {
                (true, true) => klipp_timeline::FontStyle::BoldItalic, (true, false) => klipp_timeline::FontStyle::Bold,
                (false, true) => klipp_timeline::FontStyle::Italic, (false, false) => klipp_timeline::FontStyle::Regular,
            };
            cmds.push(AppCommand::UpdateTitleData { clip_id, text: td.text.clone(), font_size: td.font_size, font_name: td.font_name.clone(), font_style: font_style_to_u8(new_style), color: td.color });
        }
        if ui.selectable_label(is_italic, italic_text).clicked() {
            let new_style = match (is_bold, !is_italic) {
                (true, true) => klipp_timeline::FontStyle::BoldItalic, (true, false) => klipp_timeline::FontStyle::Bold,
                (false, true) => klipp_timeline::FontStyle::Italic, (false, false) => klipp_timeline::FontStyle::Regular,
            };
            cmds.push(AppCommand::UpdateTitleData { clip_id, text: td.text.clone(), font_size: td.font_size, font_name: td.font_name.clone(), font_style: font_style_to_u8(new_style), color: td.color });
        }
    });
    ui.add_space(8.0);

    // Font size
    let mut font_size = td.font_size;
    ui.label(egui::RichText::new("Size").strong().size(11.0));
    if ui.add(egui::Slider::new(&mut font_size, 12.0..=300.0).step_by(1.0).suffix(" pt")).changed() {
        cmds.push(AppCommand::UpdateTitleData { clip_id, text: td.text.clone(), font_size, font_name: td.font_name.clone(), font_style: font_style_to_u8(td.font_style), color: td.color });
    }
    ui.add_space(8.0);

    // Text color
    ui.label(egui::RichText::new("Color").strong().size(11.0));
    let mut color = egui::Color32::from_rgba_premultiplied(td.color[0], td.color[1], td.color[2], td.color[3]);
    if ui.color_edit_button_srgba(&mut color).changed() {
        cmds.push(AppCommand::UpdateTitleData { clip_id, text: td.text.clone(), font_size: td.font_size, font_name: td.font_name.clone(), font_style: font_style_to_u8(td.font_style), color: [color.r(), color.g(), color.b(), color.a()] });
    }
    ui.add_space(8.0);

    // Alignment
    ui.label(egui::RichText::new("Alignment").strong().size(11.0));
    ui.horizontal(|ui| {
        let current = match td.alignment { klipp_timeline::TextAlignment::Left => 0u8, klipp_timeline::TextAlignment::Center => 1, klipp_timeline::TextAlignment::Right => 2 };
        for (val, label) in [(0, "Left"), (1, "Center"), (2, "Right")] {
            if ui.selectable_label(current == val, label).clicked() && current != val {
                cmds.push(AppCommand::SetTitleAlignment { clip_id, alignment: val });
            }
        }
    });
    ui.add_space(8.0);

    // Position
    ui.label(egui::RichText::new("Position").strong().size(11.0));
    let mut pos_x = td.position.x;
    let mut pos_y = td.position.y;
    let mut pos_changed = false;
    pos_changed |= ui.add(egui::Slider::new(&mut pos_x, 0.0..=1.0).text("X").step_by(0.01)).changed();
    pos_changed |= ui.add(egui::Slider::new(&mut pos_y, 0.0..=1.0).text("Y").step_by(0.01)).changed();
    if pos_changed {
        cmds.push(AppCommand::SetTitlePosition { clip_id, x: pos_x, y: pos_y });
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.small_button("Center").clicked() { cmds.push(AppCommand::SetTitlePosition { clip_id, x: 0.5, y: 0.5 }); }
        if ui.small_button("Lower Third").clicked() { cmds.push(AppCommand::SetTitlePosition { clip_id, x: 0.5, y: 0.85 }); }
        if ui.small_button("Top").clicked() { cmds.push(AppCommand::SetTitlePosition { clip_id, x: 0.5, y: 0.15 }); }
    });
    ui.add_space(8.0);

    // Background
    ui.label(egui::RichText::new("Background").strong().size(11.0));
    let bg_kind: u8 = match &td.background { klipp_timeline::TitleBackground::None => 0, klipp_timeline::TitleBackground::Color { .. } => 1, klipp_timeline::TitleBackground::FullFrame { .. } => 2 };
    let bg_color = match &td.background { klipp_timeline::TitleBackground::Color { color, .. } => *color, klipp_timeline::TitleBackground::FullFrame { color } => *color, _ => [0, 0, 0, 180] };
    ui.horizontal(|ui| {
        for (val, label) in [(0, "None"), (1, "Box"), (2, "Full")] {
            if ui.selectable_label(bg_kind == val, label).clicked() && bg_kind != val {
                cmds.push(AppCommand::SetTitleBackground { clip_id, bg_kind: val, color: bg_color });
            }
        }
    });
    if bg_kind > 0 {
        let mut bg_col = egui::Color32::from_rgba_premultiplied(bg_color[0], bg_color[1], bg_color[2], bg_color[3]);
        if ui.color_edit_button_srgba(&mut bg_col).changed() {
            cmds.push(AppCommand::SetTitleBackground { clip_id, bg_kind, color: [bg_col.r(), bg_col.g(), bg_col.b(), bg_col.a()] });
        }
    }
    ui.add_space(8.0);

    // Shadow
    ui.label(egui::RichText::new("Shadow").strong().size(11.0));
    let has_shadow = td.shadow.is_some();
    let shadow = td.shadow.unwrap_or(klipp_timeline::TitleShadow { color: [0, 0, 0, 180], offset_x: 3.0, offset_y: 3.0, blur: 0.0 });
    let mut shadow_enabled = has_shadow;
    if ui.checkbox(&mut shadow_enabled, "Enable Shadow").changed() {
        cmds.push(AppCommand::SetTitleShadow { clip_id, enabled: shadow_enabled, color: shadow.color, offset_x: shadow.offset_x, offset_y: shadow.offset_y });
    }
    if has_shadow {
        let mut ox = shadow.offset_x;
        let mut oy = shadow.offset_y;
        let mut s_changed = false;
        s_changed |= ui.add(egui::Slider::new(&mut ox, -20.0..=20.0).text("Offset X").step_by(0.5)).changed();
        s_changed |= ui.add(egui::Slider::new(&mut oy, -20.0..=20.0).text("Offset Y").step_by(0.5)).changed();
        let mut s_col = egui::Color32::from_rgba_premultiplied(shadow.color[0], shadow.color[1], shadow.color[2], shadow.color[3]);
        s_changed |= ui.color_edit_button_srgba(&mut s_col).changed();
        if s_changed {
            cmds.push(AppCommand::SetTitleShadow { clip_id, enabled: true, color: [s_col.r(), s_col.g(), s_col.b(), s_col.a()], offset_x: ox, offset_y: oy });
        }
    }
    ui.add_space(12.0);

    // ── Animation ──
    ui.label(egui::RichText::new("Animation").strong().size(11.0));
    ui.add_space(4.0);

    let clip_dur_secs = clip.duration().to_secs_f64() as f32;

    // Fade duration scrubby slider
    let mut fade_dur = state.title_fade_duration;
    ui.horizontal(|ui| {
        ui.label("Fade Duration:");
        if ui.add(egui::DragValue::new(&mut fade_dur).range(0.1..=3.0).speed(0.02).suffix("s").fixed_decimals(2)).changed() {
            // We store this via a mutable borrow trick — emit a command
            cmds.push(AppCommand::SetTitleFadeDuration { duration: fade_dur });
        }
    });
    ui.add_space(4.0);

    // Fade presets row
    ui.horizontal(|ui| {
        if ui.small_button("Fade In").clicked() {
            emit_fade_in(cmds, clip_id, fade_dur, clip_dur_secs);
        }
        if ui.small_button("Fade Out").clicked() {
            emit_fade_out(cmds, clip_id, fade_dur, clip_dur_secs);
        }
        if ui.small_button("Fade In+Out").clicked() {
            emit_fade_in(cmds, clip_id, fade_dur, clip_dur_secs);
            emit_fade_out(cmds, clip_id, fade_dur, clip_dur_secs);
        }
    });
    ui.add_space(4.0);

    ui.add_space(4.0);

    // Clear all animation
    if ui.small_button("Clear Animation").clicked() {
        // Remove all opacity, position, and scale keyframes
        for kf in &clip.opacity_keyframes {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param: klipp_core::VideoParam::Opacity, time_secs: kf.time_secs });
        }
        for kf in &clip.transform_pos_x_keyframes {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionX, time_secs: kf.time_secs });
        }
        for kf in &clip.transform_pos_y_keyframes {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionY, time_secs: kf.time_secs });
        }
        for kf in &clip.transform_scale_keyframes {
            cmds.push(AppCommand::RemoveVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: kf.time_secs });
        }
    }
}

/// Emit opacity keyframes for a fade-in effect.
fn emit_fade_in(cmds: &mut Vec<AppCommand>, clip_id: klipp_core::ClipId, fade_dur: f32, _clip_dur: f32) {
    let d = fade_dur as f64;
    // Keyframe at t=0: opacity 0.0
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Opacity, time_secs: 0.0, value: 0.0 });
    // Keyframe at t=fade_dur: opacity 1.0
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Opacity, time_secs: d, value: 1.0 });
    // Set Bezier interpolation on both — index 0 is t=0, index 1 is t=fade_dur (ease-out: fast start, slow end)
    cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::Opacity, keyframe_index: 0, interpolation: klipp_core::Interpolation::Bezier });
    cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::Opacity, keyframe_index: 1, interpolation: klipp_core::Interpolation::Bezier });
    // Ease-out tangents: out tangent on first KF points right, in tangent on second KF
    let span = d;
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Opacity, keyframe_index: 0, tangent_in: None, tangent_out: Some((0.42 * span, 0.0)) });
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Opacity, keyframe_index: 1, tangent_in: Some((-0.42 * span, 0.0)), tangent_out: None });
}

/// Emit opacity keyframes for a fade-out effect.
fn emit_fade_out(cmds: &mut Vec<AppCommand>, clip_id: klipp_core::ClipId, fade_dur: f32, clip_dur: f32) {
    let d = fade_dur as f64;
    let end = clip_dur as f64;
    let start = (end - d).max(0.0);
    // Keyframe at t=clip_dur-fade_dur: opacity 1.0
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Opacity, time_secs: start, value: 1.0 });
    // Keyframe at t=clip_dur: opacity 0.0
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Opacity, time_secs: end, value: 0.0 });
    // Set Bezier with ease-in tangents on ALL existing opacity keyframes (indices determined after add+sort)
    // We emit for indices 0..4 to cover both fade-in+out case (4 KFs) and fade-out only (2 KFs)
    // SetVideoKeyframeInterpolation silently ignores out-of-bounds indices
    for i in 0..4 {
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::Opacity, keyframe_index: i, interpolation: klipp_core::Interpolation::Bezier });
    }
}

/// Emit position keyframes for slide-in effect.
#[allow(clippy::too_many_arguments)]
fn emit_slide_in(cmds: &mut Vec<AppCommand>, clip_id: klipp_core::ClipId, fade_dur: f32, dest_x: f32, dest_y: f32, start_x: f32, start_y: f32, is_x: bool) {
    let d = fade_dur as f64;
    let span = d;
    if is_x {
        cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionX, time_secs: 0.0, value: start_x });
        cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionX, time_secs: d, value: dest_x });
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::PositionX, keyframe_index: 0, interpolation: klipp_core::Interpolation::Bezier });
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::PositionX, keyframe_index: 1, interpolation: klipp_core::Interpolation::Bezier });
        cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::PositionX, keyframe_index: 0, tangent_in: None, tangent_out: Some((0.42 * span, 0.0)) });
        cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::PositionX, keyframe_index: 1, tangent_in: Some((-0.42 * span, 0.0)), tangent_out: None });
    } else {
        cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionY, time_secs: 0.0, value: start_y });
        cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionY, time_secs: d, value: dest_y });
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::PositionY, keyframe_index: 0, interpolation: klipp_core::Interpolation::Bezier });
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::PositionY, keyframe_index: 1, interpolation: klipp_core::Interpolation::Bezier });
        cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::PositionY, keyframe_index: 0, tangent_in: None, tangent_out: Some((0.42 * span, 0.0)) });
        cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::PositionY, keyframe_index: 1, tangent_in: Some((-0.42 * span, 0.0)), tangent_out: None });
    }
}

/// Emit scale keyframes for a "pop in" effect with overshoot.
fn emit_pop_in(cmds: &mut Vec<AppCommand>, clip_id: klipp_core::ClipId) {
    // t=0: scale 0.0, t=0.3: scale 1.1 (overshoot), t=0.5: scale 1.0 (settle)
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: 0.0, value: 0.0 });
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: 0.3, value: 1.1 });
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: 0.5, value: 1.0 });
    // Set Bezier on all three
    for i in 0..3 {
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: i, interpolation: klipp_core::Interpolation::Bezier });
    }
    // Tangents: ease-out on first segment, ease-both on overshoot->settle
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 0, tangent_in: None, tangent_out: Some((0.42 * 0.3, 0.0)) });
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 1, tangent_in: Some((-0.42 * 0.3, 0.0)), tangent_out: Some((0.42 * 0.2, 0.0)) });
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 2, tangent_in: Some((-0.42 * 0.2, 0.0)), tangent_out: None });
}

/// Emit scale keyframes for a "grow in" effect.
fn emit_grow_in(cmds: &mut Vec<AppCommand>, clip_id: klipp_core::ClipId, fade_dur: f32) {
    let d = fade_dur as f64;
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: 0.0, value: 0.5 });
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: d, value: 1.0 });
    cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 0, interpolation: klipp_core::Interpolation::Bezier });
    cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 1, interpolation: klipp_core::Interpolation::Bezier });
    let span = d;
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 0, tangent_in: None, tangent_out: Some((0.42 * span, 0.0)) });
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 1, tangent_in: Some((-0.42 * span, 0.0)), tangent_out: None });
}

fn no_selection(ui: &mut egui::Ui, theme: &KlippTheme) {
    ui.centered_and_justified(|ui| {
        ui.label(
            egui::RichText::new("No selection")
                .size(11.0)
                .color(theme.text_dim),
        );
    });
}
