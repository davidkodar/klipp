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

        // ── Transition inspector (short-circuit clip inspector) ──
        if let Some((track_idx, trans_id)) = state.selected_transition {
            if let Some(track) = state.sequence().tracks.get(track_idx) {
                if let Some(transition) = track.transitions.iter().find(|t| t.id == trans_id) {
                    show_transition_inspector(ui, theme, track_idx, transition, &mut cmds);
                    return cmds;
                }
            }
        }

        // Resolve selected clip (timeline) or selected media (bin)
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

        // Fallback: if no timeline clip selected, check media browser selection
        let bin_media = if clip_info.is_none() {
            state.selected_media_ids.iter().next()
                .and_then(|mid| state.media_pool.get(mid))
        } else {
            None
        };


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
                } else if let Some(bm) = bin_media {
                    (bm.filename.clone(), theme.text_primary)
                } else {
                    ("No clip selected".into(), theme.text_dim)
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

        // ── Amber context banner (timeline clip only) ──
        if let Some((track_idx, clip)) = clip_info {
            let track_name = state.sequence().tracks.get(track_idx)
                .map(|t| if t.name.is_empty() { format!("Track {}", track_idx + 1) } else { t.name.clone() })
                .unwrap_or_default();
            let tc = clip.timeline_in.to_timecode_string();

            let banner_h = 24.0;
            let banner_rect = egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(ui.available_width(), banner_h),
            );
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(banner_rect), |ui| {
                // Amber background
                ui.painter().rect_filled(banner_rect, 0.0,
                    egui::Color32::from_rgba_premultiplied(232, 160, 32, 30));
                ui.painter().hline(banner_rect.x_range(), banner_rect.max.y,
                    egui::Stroke::new(1.0, egui::Color32::from_rgba_premultiplied(232, 160, 32, 60)));

                // Color bar on left
                let bar_rect = egui::Rect::from_min_size(
                    banner_rect.min, egui::vec2(4.0, banner_h),
                );
                let role_color = theme.clip_role_color(clip.role);
                ui.painter().rect_filled(bar_rect, 0.0, role_color);

                // Text — dark colors for readability on amber background
                let banner_text = egui::Color32::from_rgb(180, 120, 20);
                let banner_tc = egui::Color32::from_rgb(140, 100, 30);
                ui.painter().text(
                    egui::pos2(banner_rect.min.x + 12.0, banner_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    format!("Timeline clip \u{00B7} {track_name}"),
                    egui::FontId::proportional(10.0),
                    banner_text,
                );
                ui.painter().text(
                    egui::pos2(banner_rect.max.x - 10.0, banner_rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    &tc,
                    egui::FontId::monospace(10.0),
                    banner_tc,
                );
            });
            ui.advance_cursor_after_rect(banner_rect);
        }

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
                let is_title_clip = clip_info.as_ref().is_some_and(|(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Title(_)));
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
                        (InspectorTab::Color, "COLOR"),
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
        let is_title_clip = clip_info.as_ref().is_some_and(|(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Title(_)));
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
                        video_tab::show(ui, theme, state, Some((clip, clip_time_secs, true)), bin_media, &mut cmds);
                    } else {
                        video_tab::show(ui, theme, state, None, bin_media, &mut cmds);
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
                        let is_timeline = true; // clip_info comes from timeline selection
                        audio_tab::show(ui, theme, state, Some((clip, track_idx, is_timeline)), &mut cmds, peak, peaks_slice, source_channels, clip_time_secs, &mut self.duck, &track_infos);
                    } else if let Some(bm) = bin_media {
                        // Bin media selected — show audio info for the media item
                        let source_channels = bm.audio_channels;
                        let track_infos: Vec<audio_tab::TrackInfo> = state.sequence().tracks.iter().enumerate()
                            .map(|(i, t)| audio_tab::TrackInfo {
                                index: i,
                                name: if t.name.is_empty() { format!("Track {}", i + 1) } else { t.name.clone() },
                            })
                            .collect();
                        // Create a temporary clip for the bin media (same pattern as video tab)
                        let temp_clip = klipp_timeline::Clip {
                            id: klipp_core::ClipId::new(),
                            kind: klipp_timeline::ClipKind::Media { media_id: bm.id },
                            timeline_in: klipp_core::Timecode { value: 0, rate: bm.duration.rate },
                            timeline_out: bm.duration,
                            source_in: klipp_core::Timecode { value: 0, rate: bm.duration.rate },
                            source_out: bm.duration,
                            node_graph: klipp_nodes::NodeGraph::default(),
                            effects: Vec::new(),
                            builtin_effects: Vec::new(),
                            builtin_effects_enabled: Vec::new(),
                            linked_clip_id: None,
                            group_id: None,
                            magnetic_anchor: None,
                            gain: 1.0, volume: 1.0, pan: 0.0,
                            fade_in_secs: 0.0, fade_out_secs: 0.0,
                            fade_in_curve: klipp_timeline::FadeCurve::default(),
                            fade_out_curve: klipp_timeline::FadeCurve::default(),
                            channel_config: klipp_timeline::ChannelConfig::default(),
                            volume_keyframes: Vec::new(), pan_keyframes: Vec::new(), gain_keyframes: Vec::new(),
                            opacity_keyframes: Vec::new(),
                            transform_pos_x_keyframes: Vec::new(), transform_pos_y_keyframes: Vec::new(),
                            transform_scale_keyframes: Vec::new(), transform_rotation_keyframes: Vec::new(),
                            clip_markers: Vec::new(),
                            effect_keyframes: std::collections::HashMap::new(),
                            role: klipp_core::ClipRole::default(),
                            param_modifiers: std::collections::HashMap::new(),
                            speed: 1.0,
                            speed_keyframes: Vec::new(),
                            source_rotation: 0,
                            enabled: true,
                            solo: false,
                            grade_versions: Vec::new(),
                            active_grade_version: None,
                            multicam_group: None,
                            multicam_angle: 0,
                            multicam_cuts: Vec::new(),
                            audio_effects: Vec::new(),
                            stabilization_data: None,
                            smoothed_trajectory: None,
                            track_points: Vec::new(),
                        };
                        audio_tab::show(ui, theme, state, Some((&temp_clip, 0, false)), &mut cmds, (0.0, 0.0), None, source_channels, 0.0, &mut self.duck, &track_infos);
                    } else {
                        audio_tab::show(ui, theme, state, None, &mut cmds, (0.0, 0.0), None, None, 0.0, &mut self.duck, &[]);
                    }
                }
                InspectorTab::Effects => {
                    if let Some((_, clip)) = clip_info {
                        let clip_time_secs = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);
                        effects_tab::show(ui, theme, state, clip, &mut cmds, &mut self.show_add_builtin, &mut self.show_add_ofx, clip_time_secs);
                    } else {
                        effects_tab::show_empty(ui, theme, &mut cmds);
                    }
                }
                InspectorTab::Color => {
                    crate::panels::inspector::InspectorPanel::show_color_and_luts(ui, state, &mut cmds);
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
                        info_tab::show_clip(ui, state, theme, track_idx, clip, media, &mut self.tag_input, &mut cmds);
                    } else if let Some(bm) = bin_media {
                        info_tab::show_bin_media(ui, state, theme, bm, &mut self.tag_input, &mut cmds);
                    } else {
                        info_tab::show_no_selection(ui, state, theme);
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
    // Text preview (read-only) — editing happens in the viewer (double-click)
    ui.label(egui::RichText::new("Text").strong().size(11.0));
    let preview = if td.text.len() > 80 {
        format!("{}...", &td.text[..77])
    } else {
        td.text.clone()
    };
    ui.label(egui::RichText::new(&preview).size(11.0).color(egui::Color32::from_rgb(180, 180, 190)));
    if state.viewer_text_edit {
        if let Some((s, e)) = state.viewer_text_selection.or(state.viewer_text_selection_locked) {
            let selected_text = if e <= td.text.len() { &td.text[s..e] } else { "" };
            if !selected_text.is_empty() {
                ui.label(egui::RichText::new(format!("Selected: \"{}\"", selected_text)).size(9.0).color(egui::Color32::from_rgb(255, 195, 50)));
            }
        }
    } else {
        ui.label(egui::RichText::new("Double-click in viewer to edit").size(9.0).color(egui::Color32::from_rgb(100, 100, 110)));
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

    // Font size — selection-aware
    // Selection comes from the viewer's text edit mode (double-click to edit, select text).
    // The locked selection persists when the user clicks inspector controls.
    let effective_selection = state.viewer_text_selection
        .or(state.viewer_text_selection_locked);
    let has_selection = state.viewer_text_edit && effective_selection.is_some_and(|(s, e)| s < e);
    let sel_range = effective_selection.unwrap_or((0, 0));

    // Determine effective font size for display
    let (display_size, size_is_mixed) = if has_selection && !td.style_runs.is_empty() {
        let mut sizes: Vec<f32> = Vec::new();
        for run in &td.style_runs {
            if run.start < sel_range.1 && run.end > sel_range.0 {
                sizes.push(run.font_size.unwrap_or(td.font_size));
            }
        }
        if sizes.is_empty() {
            (td.font_size, false)
        } else if sizes.iter().all(|s| (*s - sizes[0]).abs() < 0.5) {
            (sizes[0], false)
        } else {
            (sizes[0], true)
        }
    } else if state.viewer_text_edit {
        // No selection — show font size at cursor position
        (state.viewer_text_cursor_font_size.unwrap_or(td.font_size), false)
    } else {
        (td.font_size, false)
    };

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Size").strong().size(11.0));
        if size_is_mixed {
            ui.label(egui::RichText::new("Mixed").size(9.0).color(egui::Color32::from_rgb(255, 195, 50)));
        }
    });
    let mut font_size = display_size;
    if ui.add(egui::Slider::new(&mut font_size, 12.0..=300.0).step_by(1.0).suffix(" pt")).changed() {
        if has_selection && sel_range.0 < sel_range.1 {
            // Apply to selection only
            cmds.push(AppCommand::ApplyTitleStyle {
                clip_id, range_start: sel_range.0, range_end: sel_range.1,
                property: "font_size".into(), value: format!("{font_size}"),
            });
        } else {
            cmds.push(AppCommand::UpdateTitleData { clip_id, text: td.text.clone(), font_size, font_name: td.font_name.clone(), font_style: font_style_to_u8(td.font_style), color: td.color });
        }
    }
    ui.add_space(8.0);

    // Text color — selection-aware
    let (display_color, color_is_mixed) = if has_selection && !td.style_runs.is_empty() {
        let mut colors: Vec<[u8; 4]> = Vec::new();
        for run in &td.style_runs {
            if run.start < sel_range.1 && run.end > sel_range.0 {
                colors.push(run.color.unwrap_or(td.color));
            }
        }
        if colors.is_empty() {
            (td.color, false)
        } else if colors.iter().all(|c| *c == colors[0]) {
            (colors[0], false)
        } else {
            (colors[0], true)
        }
    } else if state.viewer_text_edit {
        // No selection — show color at cursor position
        (state.viewer_text_cursor_color.unwrap_or(td.color), false)
    } else {
        (td.color, false)
    };

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Color").strong().size(11.0));
        if color_is_mixed {
            ui.label(egui::RichText::new("Mixed").size(9.0).color(egui::Color32::from_rgb(255, 195, 50)));
        }
    });
    let mut color = egui::Color32::from_rgba_premultiplied(display_color[0], display_color[1], display_color[2], display_color[3]);
    if ui.color_edit_button_srgba(&mut color).changed() {
        if has_selection && sel_range.0 < sel_range.1 {
            let c = [color.r(), color.g(), color.b(), color.a()];
            cmds.push(AppCommand::ApplyTitleStyle {
                clip_id, range_start: sel_range.0, range_end: sel_range.1,
                property: "color".into(), value: serde_json::to_string(&c).unwrap_or_default(),
            });
        } else {
            cmds.push(AppCommand::UpdateTitleData { clip_id, text: td.text.clone(), font_size: td.font_size, font_name: td.font_name.clone(), font_style: font_style_to_u8(td.font_style), color: [color.r(), color.g(), color.b(), color.a()] });
        }
    }

    // Quick color presets
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let preset_colors: &[(&str, [u8; 4])] = &[
            ("W", [255, 255, 255, 255]),
            ("R", [255, 60, 60, 255]),
            ("O", [255, 160, 0, 255]),
            ("Y", [255, 220, 50, 255]),
            ("G", [50, 200, 80, 255]),
            ("B", [60, 140, 255, 255]),
            ("P", [180, 80, 220, 255]),
        ];
        for (label, c) in preset_colors {
            let btn_color = egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
            let resp = ui.add(egui::Button::new(
                egui::RichText::new(*label).size(9.0).color(
                    if c[0] as u16 + c[1] as u16 + c[2] as u16 > 400 { egui::Color32::BLACK } else { egui::Color32::WHITE }
                )
            ).fill(btn_color).min_size(egui::vec2(18.0, 16.0)));
            if resp.clicked() {
                if has_selection && sel_range.0 < sel_range.1 {
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id, range_start: sel_range.0, range_end: sel_range.1,
                        property: "color".into(), value: serde_json::to_string(c).unwrap_or_default(),
                    });
                } else {
                    cmds.push(AppCommand::UpdateTitleData { clip_id, text: td.text.clone(), font_size: td.font_size, font_name: td.font_name.clone(), font_style: font_style_to_u8(td.font_style), color: *c });
                }
            }
        }
    });

    // Per-character color effects (apply to selection or whole text)
    if has_selection && sel_range.1 > sel_range.0 {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Effects:").size(9.0).color(egui::Color32::from_rgb(140, 140, 150)));
            // Rainbow — each character gets a different color
            if ui.small_button("Rainbow").clicked() {
                let rainbow: &[[u8; 4]] = &[
                    [255, 50, 50, 255],   // red
                    [255, 150, 0, 255],   // orange
                    [255, 230, 0, 255],   // yellow
                    [50, 200, 50, 255],   // green
                    [50, 150, 255, 255],  // blue
                    [100, 50, 255, 255],  // indigo
                    [200, 50, 200, 255],  // violet
                ];
                let sel_text = &td.text[sel_range.0..sel_range.1.min(td.text.len())];
                let mut byte_pos = sel_range.0;
                for (ci, ch) in sel_text.chars().enumerate() {
                    let color_idx = ci % rainbow.len();
                    let c = rainbow[color_idx];
                    let char_end = byte_pos + ch.len_utf8();
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id, range_start: byte_pos, range_end: char_end,
                        property: "color".into(), value: serde_json::to_string(&c).unwrap_or_default(),
                    });
                    byte_pos = char_end;
                }
            }
            // Fire — red to yellow gradient per character
            if ui.small_button("Fire").clicked() {
                let sel_text = &td.text[sel_range.0..sel_range.1.min(td.text.len())];
                let char_count = sel_text.chars().count().max(1);
                let mut byte_pos = sel_range.0;
                for (ci, ch) in sel_text.chars().enumerate() {
                    let t = ci as f32 / (char_count - 1).max(1) as f32;
                    let r = 255u8;
                    let g = (50.0 + t * 200.0) as u8;
                    let b = (0.0 + t * 30.0) as u8;
                    let c = [r, g, b, 255];
                    let char_end = byte_pos + ch.len_utf8();
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id, range_start: byte_pos, range_end: char_end,
                        property: "color".into(), value: serde_json::to_string(&c).unwrap_or_default(),
                    });
                    byte_pos = char_end;
                }
            }
            // Ocean — blue to cyan gradient
            if ui.small_button("Ocean").clicked() {
                let sel_text = &td.text[sel_range.0..sel_range.1.min(td.text.len())];
                let char_count = sel_text.chars().count().max(1);
                let mut byte_pos = sel_range.0;
                for (ci, ch) in sel_text.chars().enumerate() {
                    let t = ci as f32 / (char_count - 1).max(1) as f32;
                    let r = (20.0 + t * 30.0) as u8;
                    let g = (80.0 + t * 140.0) as u8;
                    let b = (200.0 + t * 55.0) as u8;
                    let c = [r, g, b, 255];
                    let char_end = byte_pos + ch.len_utf8();
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id, range_start: byte_pos, range_end: char_end,
                        property: "color".into(), value: serde_json::to_string(&c).unwrap_or_default(),
                    });
                    byte_pos = char_end;
                }
            }
            // Sunset — purple to orange
            if ui.small_button("Sunset").clicked() {
                let sel_text = &td.text[sel_range.0..sel_range.1.min(td.text.len())];
                let char_count = sel_text.chars().count().max(1);
                let mut byte_pos = sel_range.0;
                for (ci, ch) in sel_text.chars().enumerate() {
                    let t = ci as f32 / (char_count - 1).max(1) as f32;
                    let r = (150.0 + t * 105.0) as u8;
                    let g = (30.0 + t * 100.0) as u8;
                    let b = (200.0 - t * 170.0) as u8;
                    let c = [r, g, b, 255];
                    let char_end = byte_pos + ch.len_utf8();
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id, range_start: byte_pos, range_end: char_end,
                        property: "color".into(), value: serde_json::to_string(&c).unwrap_or_default(),
                    });
                    byte_pos = char_end;
                }
            }
        });
    }
    ui.add_space(4.0);

    // Show style runs indicator
    if !td.style_runs.is_empty() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{} style runs", td.style_runs.len())).size(9.0).color(egui::Color32::from_rgb(120, 120, 130)));
            if ui.small_button("Clear All").clicked() {
                cmds.push(AppCommand::ClearTitleStyleRuns { clip_id });
            }
        });
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
        cmds.push(AppCommand::SetTitleShadow { clip_id, enabled: shadow_enabled, color: shadow.color, offset_x: shadow.offset_x, offset_y: shadow.offset_y, blur: shadow.blur });
    }
    if has_shadow {
        let mut ox = shadow.offset_x;
        let mut oy = shadow.offset_y;
        let mut sb = shadow.blur;
        let mut s_changed = false;
        s_changed |= ui.add(egui::Slider::new(&mut ox, -20.0..=20.0).text("Offset X").step_by(0.5)).changed();
        s_changed |= ui.add(egui::Slider::new(&mut oy, -20.0..=20.0).text("Offset Y").step_by(0.5)).changed();
        s_changed |= ui.add(egui::Slider::new(&mut sb, 0.0..=30.0).text("Blur").step_by(0.5)).changed();
        let mut s_col = egui::Color32::from_rgba_premultiplied(shadow.color[0], shadow.color[1], shadow.color[2], shadow.color[3]);
        s_changed |= ui.color_edit_button_srgba(&mut s_col).changed();
        if s_changed {
            cmds.push(AppCommand::SetTitleShadow { clip_id, enabled: true, color: [s_col.r(), s_col.g(), s_col.b(), s_col.a()], offset_x: ox, offset_y: oy, blur: sb });
        }
    }
    ui.add_space(8.0);

    // Outline
    ui.separator();
    ui.label(egui::RichText::new("Outline").strong().size(11.0));
    let has_outline = td.outline.is_some();
    let outline = td.outline.unwrap_or(klipp_timeline::TextOutline { color: [255, 255, 255, 255], width: 2.0 });
    let mut outline_enabled = has_outline;
    if ui.checkbox(&mut outline_enabled, "Enable Outline").changed() {
        cmds.push(AppCommand::SetTitleOutline { clip_id, enabled: outline_enabled, color: outline.color, width: outline.width });
    }
    if has_outline {
        let mut ow = outline.width;
        let mut o_changed = false;
        o_changed |= ui.add(egui::Slider::new(&mut ow, 0.5..=12.0).text("Width").step_by(0.5).suffix(" px")).changed();
        let mut o_col = egui::Color32::from_rgba_premultiplied(outline.color[0], outline.color[1], outline.color[2], outline.color[3]);
        o_changed |= ui.color_edit_button_srgba(&mut o_col).changed();
        if o_changed {
            cmds.push(AppCommand::SetTitleOutline {
                clip_id, enabled: true,
                color: [o_col.r(), o_col.g(), o_col.b(), o_col.a()],
                width: ow,
            });
        }
    }
    ui.add_space(8.0);

    // Glow
    ui.separator();
    ui.label(egui::RichText::new("Glow").strong().size(11.0));
    let has_glow = td.glow.is_some();
    let glow = td.glow.unwrap_or(klipp_timeline::TextGlow { color: [0, 200, 255, 255], radius: 10.0, intensity: 0.8 });
    let mut glow_enabled = has_glow;
    if ui.checkbox(&mut glow_enabled, "Enable Glow").changed() {
        cmds.push(AppCommand::SetTitleGlow { clip_id, enabled: glow_enabled, color: glow.color, radius: glow.radius, intensity: glow.intensity });
    }
    if has_glow {
        let mut gr = glow.radius;
        let mut gi = glow.intensity;
        let mut g_changed = false;
        g_changed |= ui.add(egui::Slider::new(&mut gr, 1.0..=40.0).text("Radius").step_by(0.5).suffix(" px")).changed();
        g_changed |= ui.add(egui::Slider::new(&mut gi, 0.0..=1.0).text("Intensity").step_by(0.05)).changed();
        let mut g_col = egui::Color32::from_rgba_premultiplied(glow.color[0], glow.color[1], glow.color[2], glow.color[3]);
        g_changed |= ui.color_edit_button_srgba(&mut g_col).changed();
        if g_changed {
            cmds.push(AppCommand::SetTitleGlow {
                clip_id, enabled: true,
                color: [g_col.r(), g_col.g(), g_col.b(), g_col.a()],
                radius: gr, intensity: gi,
            });
        }
    }
    ui.add_space(8.0);

    // Gradient Fill
    ui.separator();
    ui.label(egui::RichText::new("Gradient Fill").strong().size(11.0));
    let has_gradient = td.gradient.is_some();
    let gradient = td.gradient.clone().unwrap_or(klipp_timeline::TextGradient { top_color: [255, 200, 50, 255], bottom_color: [255, 50, 50, 255] });
    let mut gradient_enabled = has_gradient;
    if ui.checkbox(&mut gradient_enabled, "Enable Gradient").changed() {
        cmds.push(AppCommand::SetTitleGradient { clip_id, enabled: gradient_enabled, top_color: gradient.top_color, bottom_color: gradient.bottom_color });
    }
    if has_gradient {
        let mut grad_changed = false;
        let mut top_col = egui::Color32::from_rgba_premultiplied(gradient.top_color[0], gradient.top_color[1], gradient.top_color[2], gradient.top_color[3]);
        let mut bot_col = egui::Color32::from_rgba_premultiplied(gradient.bottom_color[0], gradient.bottom_color[1], gradient.bottom_color[2], gradient.bottom_color[3]);
        ui.horizontal(|ui| {
            ui.label("Top:");
            grad_changed |= ui.color_edit_button_srgba(&mut top_col).changed();
        });
        ui.horizontal(|ui| {
            ui.label("Bottom:");
            grad_changed |= ui.color_edit_button_srgba(&mut bot_col).changed();
        });
        if grad_changed {
            cmds.push(AppCommand::SetTitleGradient {
                clip_id, enabled: true,
                top_color: [top_col.r(), top_col.g(), top_col.b(), top_col.a()],
                bottom_color: [bot_col.r(), bot_col.g(), bot_col.b(), bot_col.a()],
            });
        }
    }
    ui.add_space(8.0);

    // ── Multiple Outlines ──
    ui.separator();
    ui.label(egui::RichText::new("Outline Layers").strong().size(11.0));
    {
        let outlines = &td.outlines;
        let mut new_outlines = outlines.clone();
        let mut outlines_changed = false;

        for (i, ol) in outlines.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("#{}", i + 1));
                let mut col = egui::Color32::from_rgba_premultiplied(ol.color[0], ol.color[1], ol.color[2], ol.color[3]);
                if ui.color_edit_button_srgba(&mut col).changed() {
                    new_outlines[i].color = [col.r(), col.g(), col.b(), col.a()];
                    outlines_changed = true;
                }
                let mut w_val = ol.width;
                if ui.add(egui::DragValue::new(&mut w_val).range(0.5..=20.0).speed(0.5).suffix(" px")).changed() {
                    new_outlines[i].width = w_val;
                    outlines_changed = true;
                }
                if ui.small_button("\u{2715}").clicked() {
                    new_outlines.remove(i);
                    outlines_changed = true;
                }
            });
        }
        if ui.small_button("+ Add Outline Layer").clicked() {
            new_outlines.push(klipp_timeline::TextOutline {
                color: [0, 0, 0, 255],
                width: 4.0,
            });
            outlines_changed = true;
        }
        if outlines_changed {
            let outline_tuples: Vec<([u8; 4], f32)> = new_outlines.iter().map(|o| (o.color, o.width)).collect();
            cmds.push(AppCommand::SetTitleOutlines { clip_id, outlines: outline_tuples });
        }
    }
    ui.add_space(8.0);

    // ── Inner Shadow ──
    ui.separator();
    ui.label(egui::RichText::new("Inner Shadow").strong().size(11.0));
    let has_inner = td.inner_shadow.is_some();
    let inner = td.inner_shadow.unwrap_or(klipp_timeline::InnerShadow { color: [0, 0, 0, 180], offset_x: 2.0, offset_y: 2.0, blur: 3.0 });
    let mut inner_enabled = has_inner;
    if ui.checkbox(&mut inner_enabled, "Enable").changed() {
        cmds.push(AppCommand::SetTitleInnerShadow { clip_id, enabled: inner_enabled, color: inner.color, offset_x: inner.offset_x, offset_y: inner.offset_y, blur: inner.blur });
    }
    if has_inner {
        let mut ix = inner.offset_x;
        let mut iy = inner.offset_y;
        let mut ib = inner.blur;
        let mut i_changed = false;
        let mut i_col = egui::Color32::from_rgba_premultiplied(inner.color[0], inner.color[1], inner.color[2], inner.color[3]);
        i_changed |= ui.color_edit_button_srgba(&mut i_col).changed();
        i_changed |= ui.add(egui::Slider::new(&mut ix, -20.0..=20.0).text("Offset X").step_by(0.5)).changed();
        i_changed |= ui.add(egui::Slider::new(&mut iy, -20.0..=20.0).text("Offset Y").step_by(0.5)).changed();
        i_changed |= ui.add(egui::Slider::new(&mut ib, 0.0..=20.0).text("Blur").step_by(0.5)).changed();
        if i_changed {
            cmds.push(AppCommand::SetTitleInnerShadow {
                clip_id, enabled: true,
                color: [i_col.r(), i_col.g(), i_col.b(), i_col.a()],
                offset_x: ix, offset_y: iy, blur: ib,
            });
        }
    }
    ui.add_space(8.0);

    // ── Pattern Fill ──
    ui.separator();
    ui.label(egui::RichText::new("Pattern Fill").strong().size(11.0));
    {
        let pattern_idx: usize = match &td.pattern_fill {
            None => 0,
            Some(klipp_timeline::TextPattern::HorizontalStripes { .. }) => 1,
            Some(klipp_timeline::TextPattern::VerticalStripes { .. }) => 2,
            Some(klipp_timeline::TextPattern::Checkerboard { .. }) => 3,
            Some(klipp_timeline::TextPattern::DiagonalStripes { .. }) => 4,
        };
        let labels = ["None", "H-Stripes", "V-Stripes", "Checker", "Diagonal"];
        let mut new_idx = pattern_idx;
        egui::ComboBox::from_id_salt("title_pattern_type")
            .selected_text(labels[pattern_idx])
            .width(120.0)
            .show_ui(ui, |ui| {
                for (i, label) in labels.iter().enumerate() {
                    if ui.selectable_label(pattern_idx == i, *label).clicked() {
                        new_idx = i;
                    }
                }
            });
        if new_idx != pattern_idx {
            cmds.push(AppCommand::SetTitlePattern {
                clip_id,
                pattern_kind: new_idx as u8,
                color_a: [255, 255, 255, 255],
                color_b: [0, 0, 0, 255],
                size: 10.0,
            });
        }
        // Show pattern params if active
        if let Some(pf) = &td.pattern_fill {
            let (mut ca, mut cb, mut size) = match pf {
                klipp_timeline::TextPattern::HorizontalStripes { color_a, color_b, stripe_width } => (*color_a, *color_b, *stripe_width),
                klipp_timeline::TextPattern::VerticalStripes { color_a, color_b, stripe_width } => (*color_a, *color_b, *stripe_width),
                klipp_timeline::TextPattern::Checkerboard { color_a, color_b, cell_size } => (*color_a, *color_b, *cell_size),
                klipp_timeline::TextPattern::DiagonalStripes { color_a, color_b, stripe_width } => (*color_a, *color_b, *stripe_width),
            };
            let mut pf_changed = false;
            let mut col_a = egui::Color32::from_rgba_premultiplied(ca[0], ca[1], ca[2], ca[3]);
            let mut col_b = egui::Color32::from_rgba_premultiplied(cb[0], cb[1], cb[2], cb[3]);
            ui.horizontal(|ui| {
                ui.label("A:");
                pf_changed |= ui.color_edit_button_srgba(&mut col_a).changed();
                ui.label("B:");
                pf_changed |= ui.color_edit_button_srgba(&mut col_b).changed();
            });
            pf_changed |= ui.add(egui::Slider::new(&mut size, 2.0..=50.0).text("Size").step_by(1.0).suffix(" px")).changed();
            if pf_changed {
                ca = [col_a.r(), col_a.g(), col_a.b(), col_a.a()];
                cb = [col_b.r(), col_b.g(), col_b.b(), col_b.a()];
                cmds.push(AppCommand::SetTitlePattern {
                    clip_id,
                    pattern_kind: pattern_idx as u8,
                    color_a: ca,
                    color_b: cb,
                    size,
                });
            }
        }
    }
    ui.add_space(8.0);

    // ── Texture Fill ──
    ui.separator();
    ui.label(egui::RichText::new("Texture Fill").strong().size(11.0));
    {
        let has_texture = td.texture_fill_path.is_some();
        let mut tex_enabled = has_texture;
        if ui.checkbox(&mut tex_enabled, "Enable").changed()
            && !tex_enabled {
                cmds.push(AppCommand::SetTitleTextureFill { clip_id, path: None });
            }
        if has_texture {
            if let Some(ref tex_path) = td.texture_fill_path {
                let display_name = std::path::Path::new(tex_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(tex_path);
                ui.horizontal(|ui| {
                    ui.label("File:");
                    ui.label(egui::RichText::new(display_name).monospace().size(10.0));
                });
            }
        }
        if ui.button("Browse...").clicked() {
            if let Some(path) = rfd::FileDialog::new()
                .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "tiff", "tga"])
                .pick_file()
            {
                cmds.push(AppCommand::SetTitleTextureFill {
                    clip_id,
                    path: Some(path.to_string_lossy().to_string()),
                });
            }
        }
    }
    ui.add_space(8.0);

    // ── 3D Extrusion ──
    ui.separator();
    ui.label(egui::RichText::new("3D Extrusion").strong().size(11.0));
    let has_ext = td.extrusion.is_some();
    let ext = td.extrusion.unwrap_or(klipp_timeline::TextExtrusion {
        depth: 10.0,
        angle: 135.0,
        face_color: [0, 0, 0, 0],
        side_color: [80, 60, 40, 255],
        lighting: 0.5,
    });
    let mut ext_enabled = has_ext;
    if ui.checkbox(&mut ext_enabled, "Enable").changed() {
        cmds.push(AppCommand::SetTitleExtrusion {
            clip_id, enabled: ext_enabled,
            depth: ext.depth, angle: ext.angle,
            face_color: ext.face_color, side_color: ext.side_color, lighting: ext.lighting,
        });
    }
    if has_ext {
        let mut depth = ext.depth;
        let mut angle = ext.angle;
        let mut lighting = ext.lighting;
        let mut ext_changed = false;
        ext_changed |= ui.add(egui::Slider::new(&mut depth, 1.0..=50.0).text("Depth").step_by(0.5).suffix(" px")).changed();
        ext_changed |= ui.add(egui::Slider::new(&mut angle, 0.0..=360.0).text("Angle").step_by(1.0).suffix("\u{00b0}")).changed();
        let mut face_col = egui::Color32::from_rgba_premultiplied(ext.face_color[0], ext.face_color[1], ext.face_color[2], ext.face_color[3]);
        let mut side_col = egui::Color32::from_rgba_premultiplied(ext.side_color[0], ext.side_color[1], ext.side_color[2], ext.side_color[3]);
        ui.horizontal(|ui| {
            ui.label("Face:");
            ext_changed |= ui.color_edit_button_srgba(&mut face_col).changed();
        });
        ui.horizontal(|ui| {
            ui.label("Side:");
            ext_changed |= ui.color_edit_button_srgba(&mut side_col).changed();
        });
        ext_changed |= ui.add(egui::Slider::new(&mut lighting, 0.0..=1.0).text("Lighting").step_by(0.05)).changed();
        if ext_changed {
            cmds.push(AppCommand::SetTitleExtrusion {
                clip_id, enabled: true,
                depth, angle,
                face_color: [face_col.r(), face_col.g(), face_col.b(), face_col.a()],
                side_color: [side_col.r(), side_col.g(), side_col.b(), side_col.a()],
                lighting,
            });
        }
    }
    ui.add_space(8.0);

    // ── Typography ──
    ui.separator();
    ui.label(egui::RichText::new("Typography").strong().size(11.0));
    let mut tracking = td.tracking;
    if ui.add(egui::DragValue::new(&mut tracking).range(-10.0..=50.0).speed(0.2).suffix(" px").prefix("Tracking: ").fixed_decimals(1)).changed() {
        cmds.push(AppCommand::SetTitleTracking { clip_id, tracking });
    }
    let mut line_sp = td.line_spacing;
    if ui.add(egui::DragValue::new(&mut line_sp).range(0.5..=4.0).speed(0.05).suffix("x").prefix("Line Spacing: ").fixed_decimals(2)).changed() {
        cmds.push(AppCommand::SetTitleLineSpacing { clip_id, line_spacing: line_sp });
    }
    let mut write_on = td.write_on;
    if ui.add(egui::Slider::new(&mut write_on, 0.0..=1.0).text("Write-On").step_by(0.01)).changed() {
        cmds.push(AppCommand::SetTitleWriteOn { clip_id, write_on });
    }
    ui.add_space(8.0);

    // ── Text Path ──
    ui.separator();
    ui.label(egui::RichText::new("Text Path").strong().size(11.0));
    {
        let path_idx: usize = match &td.text_path {
            None => 0,
            Some(klipp_timeline::TextPath::Arc { .. }) => 1,
            Some(klipp_timeline::TextPath::Circle { .. }) => 2,
            Some(klipp_timeline::TextPath::Wave { .. }) => 3,
        };
        let path_labels = ["None", "Arc", "Circle", "Wave"];
        let mut new_idx = path_idx;
        egui::ComboBox::from_label("Type")
            .selected_text(path_labels[path_idx])
            .width(100.0)
            .show_ui(ui, |ui| {
                for (i, label) in path_labels.iter().enumerate() {
                    if ui.selectable_label(path_idx == i, *label).clicked() {
                        new_idx = i;
                    }
                }
            });
        if new_idx != path_idx {
            let (pk, p1, p2, p3) = match new_idx {
                1 => (1u8, 200.0f32, 0.0f32, 180.0f32),
                2 => (2, 200.0, 0.0, 0.0),
                3 => (3, 30.0, 2.0, 0.0),
                _ => (0, 0.0, 0.0, 0.0),
            };
            cmds.push(AppCommand::SetTitleTextPath { clip_id, path_kind: pk, p1, p2, p3 });
        }
        // Show path params if active
        if let Some(path) = &td.text_path {
            let mut path_changed = false;
            match path {
                klipp_timeline::TextPath::Arc { radius, start_angle, sweep } => {
                    let (mut r, mut sa, mut sw) = (*radius, *start_angle, *sweep);
                    path_changed |= ui.add(egui::DragValue::new(&mut r).range(50.0..=800.0).speed(1.0).suffix(" px").prefix("Radius: ")).changed();
                    path_changed |= ui.add(egui::DragValue::new(&mut sa).range(-360.0..=360.0).speed(1.0).suffix("\u{00b0}").prefix("Start: ")).changed();
                    path_changed |= ui.add(egui::DragValue::new(&mut sw).range(-360.0..=360.0).speed(1.0).suffix("\u{00b0}").prefix("Sweep: ")).changed();
                    if path_changed {
                        cmds.push(AppCommand::SetTitleTextPath { clip_id, path_kind: 1, p1: r, p2: sa, p3: sw });
                    }
                }
                klipp_timeline::TextPath::Circle { radius } => {
                    let mut r = *radius;
                    path_changed |= ui.add(egui::DragValue::new(&mut r).range(50.0..=800.0).speed(1.0).suffix(" px").prefix("Radius: ")).changed();
                    if path_changed {
                        cmds.push(AppCommand::SetTitleTextPath { clip_id, path_kind: 2, p1: r, p2: 0.0, p3: 0.0 });
                    }
                }
                klipp_timeline::TextPath::Wave { amplitude, frequency, phase } => {
                    let (mut a, mut f, mut p) = (*amplitude, *frequency, *phase);
                    path_changed |= ui.add(egui::DragValue::new(&mut a).range(5.0..=200.0).speed(0.5).suffix(" px").prefix("Amplitude: ")).changed();
                    path_changed |= ui.add(egui::DragValue::new(&mut f).range(0.1..=10.0).speed(0.05).prefix("Frequency: ").fixed_decimals(1)).changed();
                    path_changed |= ui.add(egui::DragValue::new(&mut p).range(0.0..=360.0).speed(1.0).suffix("\u{00b0}").prefix("Phase: ")).changed();
                    if path_changed {
                        cmds.push(AppCommand::SetTitleTextPath { clip_id, path_kind: 3, p1: a, p2: f, p3: p });
                    }
                }
            }
        }
    }
    ui.add_space(8.0);

    // ── Character Animation ──
    ui.separator();
    ui.label(egui::RichText::new("Character Animation").strong().size(11.0));
    {
        let mut enabled = td.char_animation.is_some();
        if ui.checkbox(&mut enabled, "Enable").changed() {
            cmds.push(AppCommand::SetTitleCharAnimation {
                clip_id,
                enabled,
                stagger: 0.05,
                entrance: 1, // FadeIn
                wave_amplitude: 0.0,
                wave_speed: 1.0,
            });
        }
        if let Some(anim) = &td.char_animation {
            let mut stagger = anim.stagger;
            let mut wave_amp = anim.wave_amplitude;
            let mut wave_spd = anim.wave_speed;
            let entrance_idx: usize = match anim.entrance {
                klipp_timeline::CharEntrance::None => 0,
                klipp_timeline::CharEntrance::FadeIn => 1,
                klipp_timeline::CharEntrance::SlideUp => 2,
                klipp_timeline::CharEntrance::SlideDown => 3,
                klipp_timeline::CharEntrance::ScaleUp => 4,
            };
            let entrance_labels = ["None", "Fade In", "Slide Up", "Slide Down", "Scale Up"];
            let mut new_entrance_idx = entrance_idx;
            egui::ComboBox::from_label("Entrance")
                .selected_text(entrance_labels[entrance_idx])
                .width(100.0)
                .show_ui(ui, |ui| {
                    for (i, label) in entrance_labels.iter().enumerate() {
                        if ui.selectable_label(entrance_idx == i, *label).clicked() {
                            new_entrance_idx = i;
                        }
                    }
                });
            let mut anim_changed = new_entrance_idx != entrance_idx;
            anim_changed |= ui.add(egui::DragValue::new(&mut stagger).range(0.0..=0.5).speed(0.005).suffix(" s").prefix("Stagger: ").fixed_decimals(3)).changed();
            anim_changed |= ui.add(egui::DragValue::new(&mut wave_amp).range(0.0..=100.0).speed(0.5).suffix(" px").prefix("Wave Amp: ").fixed_decimals(1)).changed();
            anim_changed |= ui.add(egui::DragValue::new(&mut wave_spd).range(0.1..=10.0).speed(0.05).suffix(" Hz").prefix("Wave Speed: ").fixed_decimals(1)).changed();
            if anim_changed {
                cmds.push(AppCommand::SetTitleCharAnimation {
                    clip_id,
                    enabled: true,
                    stagger,
                    entrance: new_entrance_idx as u8,
                    wave_amplitude: wave_amp,
                    wave_speed: wave_spd,
                });
            }
        }
    }
    ui.add_space(8.0);

    // ── Motion Blur ──
    ui.separator();
    ui.label(egui::RichText::new("Motion Blur").strong().size(11.0));
    {
        let mut enabled = td.motion_blur.is_some();
        if ui.checkbox(&mut enabled, "Enable").changed() {
            cmds.push(AppCommand::SetTitleMotionBlur {
                clip_id,
                enabled,
                shutter_angle: 180.0,
                samples: 8,
            });
        }
        if let Some(mb) = &td.motion_blur {
            let mut shutter = mb.shutter_angle;
            let mut samples = mb.samples as i32;
            let mut mb_changed = false;
            ui.horizontal(|ui| {
                ui.label("Shutter Angle:");
                mb_changed |= ui.add(egui::DragValue::new(&mut shutter).range(1.0..=360.0).speed(1.0).suffix("\u{00b0}").fixed_decimals(0)).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Samples:");
                mb_changed |= ui.add(egui::DragValue::new(&mut samples).range(2..=32).speed(0.1)).changed();
            });
            if mb_changed {
                cmds.push(AppCommand::SetTitleMotionBlur {
                    clip_id,
                    enabled: true,
                    shutter_angle: shutter,
                    samples: samples as u8,
                });
            }
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
        ui.label("Duration:");
        if ui.add(egui::DragValue::new(&mut fade_dur).range(0.1..=30.0).speed(0.05).suffix("s").fixed_decimals(2)).changed() {
            // We store this via a mutable borrow trick — emit a command
            cmds.push(AppCommand::SetTitleFadeDuration { duration: fade_dur });
        }
    });
    ui.add_space(4.0);

    // Fade presets row
    ui.horizontal(|ui| {
        if ui.small_button("Fade In").clicked() {
            emit_fade_in(cmds, clip_id, fade_dur, clip_dur_secs);
            cmds.push(AppCommand::ClearTitleOverlay);
        }
        if ui.small_button("Fade Out").clicked() {
            cmds.push(AppCommand::ClearVideoParamKeyframes { clip_id, param: klipp_core::VideoParam::Opacity });
            emit_fade_out(cmds, clip_id, fade_dur, clip_dur_secs);
        }
        if ui.small_button("Fade In+Out").clicked() {
            emit_fade_in(cmds, clip_id, fade_dur, clip_dur_secs);
            emit_fade_out(cmds, clip_id, fade_dur, clip_dur_secs);
            cmds.push(AppCommand::ClearTitleOverlay);
        }
    });
    ui.add_space(4.0);

    // Slide presets row
    ui.horizontal(|ui| {
        ui.label("Slide:");
        if ui.small_button("Left").on_hover_text("Slide in from left").clicked() {
            emit_slide_in(cmds, clip_id, fade_dur, 0.0, 0.0, -960.0, 0.0, true);
            cmds.push(AppCommand::ClearTitleOverlay);
        }
        if ui.small_button("Right").on_hover_text("Slide in from right").clicked() {
            emit_slide_in(cmds, clip_id, fade_dur, 0.0, 0.0, 960.0, 0.0, true);
            cmds.push(AppCommand::ClearTitleOverlay);
        }
        if ui.small_button("Up").on_hover_text("Slide in from bottom").clicked() {
            emit_slide_in(cmds, clip_id, fade_dur, 0.0, 0.0, 0.0, 540.0, false);
            cmds.push(AppCommand::ClearTitleOverlay);
        }
        if ui.small_button("Down").on_hover_text("Slide in from top").clicked() {
            emit_slide_in(cmds, clip_id, fade_dur, 0.0, 0.0, 0.0, -540.0, false);
            cmds.push(AppCommand::ClearTitleOverlay);
        }
    });
    ui.add_space(4.0);

    // Scale presets row
    ui.horizontal(|ui| {
        if ui.small_button("Pop In").clicked() {
            emit_pop_in(cmds, clip_id, fade_dur);
            cmds.push(AppCommand::ClearTitleOverlay);
        }
        if ui.small_button("Grow In").clicked() {
            emit_grow_in(cmds, clip_id, fade_dur);
            cmds.push(AppCommand::ClearTitleOverlay);
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
    cmds.push(AppCommand::ClearVideoParamKeyframes { clip_id, param: klipp_core::VideoParam::Opacity });
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
        cmds.push(AppCommand::ClearVideoParamKeyframes { clip_id, param: klipp_core::VideoParam::PositionX });
        cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionX, time_secs: 0.0, value: start_x });
        cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionX, time_secs: d, value: dest_x });
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::PositionX, keyframe_index: 0, interpolation: klipp_core::Interpolation::Bezier });
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::PositionX, keyframe_index: 1, interpolation: klipp_core::Interpolation::Bezier });
        cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::PositionX, keyframe_index: 0, tangent_in: None, tangent_out: Some((0.42 * span, 0.0)) });
        cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::PositionX, keyframe_index: 1, tangent_in: Some((-0.42 * span, 0.0)), tangent_out: None });
    } else {
        cmds.push(AppCommand::ClearVideoParamKeyframes { clip_id, param: klipp_core::VideoParam::PositionY });
        cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionY, time_secs: 0.0, value: start_y });
        cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::PositionY, time_secs: d, value: dest_y });
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::PositionY, keyframe_index: 0, interpolation: klipp_core::Interpolation::Bezier });
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::PositionY, keyframe_index: 1, interpolation: klipp_core::Interpolation::Bezier });
        cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::PositionY, keyframe_index: 0, tangent_in: None, tangent_out: Some((0.42 * span, 0.0)) });
        cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::PositionY, keyframe_index: 1, tangent_in: Some((-0.42 * span, 0.0)), tangent_out: None });
    }
}

/// Emit scale keyframes for a "pop in" effect with overshoot.
fn emit_pop_in(cmds: &mut Vec<AppCommand>, clip_id: klipp_core::ClipId, anim_dur: f32) {
    let d = anim_dur as f64;
    let t_overshoot = d * 0.6;
    let t_settle = d;
    cmds.push(AppCommand::ClearVideoParamKeyframes { clip_id, param: klipp_core::VideoParam::Scale });
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: 0.0, value: 0.0 });
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: t_overshoot, value: 1.1 });
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: t_settle, value: 1.0 });
    for i in 0..3 {
        cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: i, interpolation: klipp_core::Interpolation::Bezier });
    }
    let span1 = t_overshoot;
    let span2 = t_settle - t_overshoot;
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 0, tangent_in: None, tangent_out: Some((0.42 * span1, 0.0)) });
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 1, tangent_in: Some((-0.42 * span1, 0.0)), tangent_out: Some((0.42 * span2, 0.0)) });
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 2, tangent_in: Some((-0.42 * span2, 0.0)), tangent_out: None });
}

/// Emit scale keyframes for a "grow in" effect.
fn emit_grow_in(cmds: &mut Vec<AppCommand>, clip_id: klipp_core::ClipId, fade_dur: f32) {
    let d = fade_dur as f64;
    cmds.push(AppCommand::ClearVideoParamKeyframes { clip_id, param: klipp_core::VideoParam::Scale });
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: 0.0, value: 0.5 });
    cmds.push(AppCommand::AddVideoKeyframe { clip_id, param: klipp_core::VideoParam::Scale, time_secs: d, value: 1.0 });
    cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 0, interpolation: klipp_core::Interpolation::Bezier });
    cmds.push(AppCommand::SetVideoKeyframeInterpolation { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 1, interpolation: klipp_core::Interpolation::Bezier });
    let span = d;
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 0, tangent_in: None, tangent_out: Some((0.42 * span, 0.0)) });
    cmds.push(AppCommand::SetVideoKeyframeTangent { clip_id, param: klipp_core::VideoParam::Scale, keyframe_index: 1, tangent_in: Some((-0.42 * span, 0.0)), tangent_out: None });
}

fn transition_kind_id(kind: klipp_timeline::TransitionKind) -> u8 {
    klipp_core::spec_for_kind(kind).kind_id
}

fn show_transition_inspector(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    track_idx: usize,
    transition: &klipp_timeline::Transition,
    cmds: &mut Vec<AppCommand>,
) {
    // Header
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
            ui.label(
                egui::RichText::new("Transition")
                    .size(13.0)
                    .strong()
                    .color(theme.text_primary),
            );
        });
    });
    ui.advance_cursor_after_rect(header_rect);

    ui.add_space(12.0);

    let current_kind_id = transition_kind_id(transition.kind);
    let kinds = klipp_engine::transition::all_transition_kinds();
    let current_name = kinds
        .iter()
        .find(|(id, _)| *id == current_kind_id)
        .map(|(_, n)| *n)
        .unwrap_or("Unknown");

    egui::Frame::none()
        .fill(theme.bg_raised)
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("TRANSITION")
                    .size(10.0)
                    .color(theme.text_dim),
            );
            ui.add_space(8.0);

            // Kind dropdown
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Type")
                        .color(theme.text_secondary)
                        .size(11.0),
                );
                egui::ComboBox::from_id_salt("transition_kind_picker")
                    .selected_text(current_name)
                    .show_ui(ui, |ui| {
                        for (kid, name) in kinds {
                            let mut selected = *kid == current_kind_id;
                            if ui.selectable_value(&mut selected, true, *name).clicked()
                                && *kid != current_kind_id
                            {
                                cmds.push(AppCommand::SetTransitionKind {
                                    track_index: track_idx,
                                    transition_id: transition.id,
                                    kind_id: *kid,
                                });
                            }
                        }
                    });
            });

            ui.add_space(6.0);

            // Duration scrubby
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Duration")
                        .color(theme.text_secondary)
                        .size(11.0),
                );
                let mut frames = transition.duration.value.max(1) as u32;
                let resp = ui.add(
                    egui::DragValue::new(&mut frames)
                        .range(1..=600)
                        .suffix(" fr"),
                );
                if resp.changed() {
                    cmds.push(AppCommand::SetTransitionDuration {
                        track_index: track_idx,
                        transition_id: transition.id,
                        duration_frames: frames,
                    });
                }
            });

            ui.add_space(6.0);

            // Alignment selector — universal across all kinds.
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Alignment")
                        .color(theme.text_secondary)
                        .size(11.0),
                );
                use klipp_core::TransitionAlignment;
                const ALIGNMENTS: &[(TransitionAlignment, &str)] = &[
                    (TransitionAlignment::Center, "Center"),
                    (TransitionAlignment::StartAtCut, "Start at Cut"),
                    (TransitionAlignment::EndAtCut, "End at Cut"),
                ];
                let current_label = ALIGNMENTS
                    .iter()
                    .find(|(a, _)| *a == transition.alignment)
                    .map(|(_, l)| *l)
                    .unwrap_or("Center");
                egui::ComboBox::from_id_salt("transition_alignment")
                    .selected_text(current_label)
                    .show_ui(ui, |ui| {
                        for (alignment, label) in ALIGNMENTS {
                            let is_current = *alignment == transition.alignment;
                            if ui.selectable_label(is_current, *label).clicked() && !is_current {
                                cmds.push(AppCommand::SetTransitionAlignment {
                                    track_index: track_idx,
                                    transition_id: transition.id,
                                    alignment: *alignment,
                                });
                            }
                        }
                    });
            });

            // Per-kind parameters
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::WipeLeft
                    | klipp_timeline::TransitionKind::WipeRight
                    | klipp_timeline::TransitionKind::WipeUp
                    | klipp_timeline::TransitionKind::WipeDown
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Feather")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut feather = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut feather)
                            .range(0.0..=32.0)
                            .speed(0.5)
                            .suffix(" px"),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = feather;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::BlurDissolve) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Blur Radius")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut radius = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut radius)
                            .range(4.0..=128.0)
                            .speed(1.0)
                            .suffix(" px"),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = radius;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::DipToBlack | klipp_timeline::TransitionKind::DipToWhite
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Dip Color")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut color = egui::Color32::from_rgb(
                        (transition.user_params.color[0] * 255.0) as u8,
                        (transition.user_params.color[1] * 255.0) as u8,
                        (transition.user_params.color[2] * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        let mut new_params = transition.user_params;
                        new_params.color = [
                            color.r() as f32 / 255.0,
                            color.g() as f32 / 255.0,
                            color.b() as f32 / 255.0,
                            1.0,
                        ];
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Hold")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut hold = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut hold)
                            .range(0.0..=0.45)
                            .speed(0.01),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = hold;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::Flash) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Flash Color")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut color = egui::Color32::from_rgb(
                        (transition.user_params.color[0] * 255.0) as u8,
                        (transition.user_params.color[1] * 255.0) as u8,
                        (transition.user_params.color[2] * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        let mut new_params = transition.user_params;
                        new_params.color = [
                            color.r() as f32 / 255.0,
                            color.g() as f32 / 255.0,
                            color.b() as f32 / 255.0,
                            1.0,
                        ];
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Intensity")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut intensity = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut intensity)
                            .range(0.0..=2.0)
                            .speed(0.05),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = intensity;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Peak Position")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut peak = transition.user_params.param1;
                    let resp = ui.add(
                        egui::DragValue::new(&mut peak)
                            .range(0.1..=0.9)
                            .speed(0.01),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param1 = peak;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::LightLeak) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Leak Color")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut color = egui::Color32::from_rgb(
                        (transition.user_params.color[0] * 255.0) as u8,
                        (transition.user_params.color[1] * 255.0) as u8,
                        (transition.user_params.color[2] * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        let mut new_params = transition.user_params;
                        new_params.color = [
                            color.r() as f32 / 255.0,
                            color.g() as f32 / 255.0,
                            color.b() as f32 / 255.0,
                            1.0,
                        ];
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Intensity")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut intensity = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut intensity)
                            .range(0.0..=2.0)
                            .speed(0.05),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = intensity;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::CircleWipeIn
                    | klipp_timeline::TransitionKind::CircleWipeOut
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Softness", 0u8, 0.0..=4.0, 0.05),
                    ("Center X", 1u8, 0.0..=1.0, 0.01),
                    ("Center Y", 2u8, 0.0..=1.0, 0.01),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            1 => transition.user_params.param1,
                            _ => transition.user_params.param2,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                1 => new_params.param1 = v,
                                _ => new_params.param2 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::BarnDoorOpen
                    | klipp_timeline::TransitionKind::BarnDoorClose
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Softness")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v).range(0.0..=4.0).speed(0.05),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Orientation")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let vertical = transition.user_params.param1 > 0.5;
                    let btn_label = if vertical { "Vertical" } else { "Horizontal" };
                    if ui.button(btn_label).clicked() {
                        let mut new_params = transition.user_params;
                        new_params.param1 = if vertical { 0.0 } else { 1.0 };
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::PageTurn
                    | klipp_timeline::TransitionKind::PageTurnReverse
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Shadow")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v).range(0.0..=1.0).speed(0.01),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Direction")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let rtl = transition.user_params.param1 < 0.5;
                    let btn_label = if rtl { "Right to Left" } else { "Left to Right" };
                    if ui.button(btn_label).clicked() {
                        let mut new_params = transition.user_params;
                        new_params.param1 = if rtl { 1.0 } else { 0.0 };
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::SlideLeft
                    | klipp_timeline::TransitionKind::SlideRight
                    | klipp_timeline::TransitionKind::SlideUp
                    | klipp_timeline::TransitionKind::SlideDown
                    | klipp_timeline::TransitionKind::CoverLeft
                    | klipp_timeline::TransitionKind::CoverRight
                    | klipp_timeline::TransitionKind::UncoverLeft
                    | klipp_timeline::TransitionKind::UncoverRight
                    | klipp_timeline::TransitionKind::SqueezeH
                    | klipp_timeline::TransitionKind::SqueezeV
            ) {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("No additional parameters")
                        .color(theme.text_dim)
                        .size(10.0)
                        .italics(),
                );
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::BoxWipeIn
                    | klipp_timeline::TransitionKind::BoxWipeOut
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Softness", 0u8, 0.0..=4.0, 0.05),
                    ("Square", 1u8, 0.0..=1.0, 0.01),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::DiagonalWipeTLBR
                    | klipp_timeline::TransitionKind::DiagonalWipeTRBL
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Softness")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v).range(0.0..=4.0).speed(0.05),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::CheckerboardWipe) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Softness", 0u8, 0.0..=4.0, 0.05),
                    ("Grid Size", 1u8, 2.0..=24.0, 0.5),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::VenetianBlindHorizontal
                    | klipp_timeline::TransitionKind::VenetianBlindVertical
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Softness", 0u8, 0.0..=4.0, 0.05),
                    ("Blinds", 1u8, 2.0..=20.0, 0.5),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::SplitWipeHorizontal
                    | klipp_timeline::TransitionKind::SplitWipeVertical
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Softness")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v).range(0.0..=4.0).speed(0.05),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::StarWipe) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Softness", 0u8, 0.0..=4.0, 0.05),
                    ("Points", 1u8, 3.0..=12.0, 0.5),
                    ("Pointiness", 2u8, 0.2..=0.8, 0.01),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            1 => transition.user_params.param1,
                            _ => transition.user_params.param2,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                1 => new_params.param1 = v,
                                _ => new_params.param2 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::ZoomInDissolve
                    | klipp_timeline::TransitionKind::ZoomOutDissolve
                    | klipp_timeline::TransitionKind::CrossZoom
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Zoom Amount")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v)
                            .range(1.1..=2.0)
                            .speed(0.05)
                            .suffix("×"),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                if matches!(transition.kind, klipp_timeline::TransitionKind::CrossZoom) {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("Easing")
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let eased = transition.user_params.param1 > 0.5;
                        let btn_label = if eased { "Ease In-Out" } else { "Linear" };
                        if ui.button(btn_label).clicked() {
                            let mut new_params = transition.user_params;
                            new_params.param1 = if eased { 0.0 } else { 1.0 };
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::BokehDissolve
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Bokeh Size", 0u8, 0.0..=2.0, 0.05),
                    ("Brightness", 1u8, 0.0..=1.0, 0.01),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::AnamorphicFlare
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Flare Color")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut color = egui::Color32::from_rgb(
                        (transition.user_params.color[0] * 255.0) as u8,
                        (transition.user_params.color[1] * 255.0) as u8,
                        (transition.user_params.color[2] * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        let mut new_params = transition.user_params;
                        new_params.color = [
                            color.r() as f32 / 255.0,
                            color.g() as f32 / 255.0,
                            color.b() as f32 / 255.0,
                            1.0,
                        ];
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                for (label, field, range, speed) in [
                    ("Intensity", 0u8, 0.0..=2.0, 0.05),
                    ("Streak Width", 1u8, 0.05..=0.5, 0.01),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::DreamSequence
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Bloom", 0u8, 0.0..=2.0, 0.05),
                    ("Warmth", 1u8, 0.0..=1.0, 0.01),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::ProjectorFlicker
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Flicker", 0u8, 0.0..=2.0, 0.05),
                    ("Gate Weave", 1u8, 0.0..=20.0, 0.5),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::Halation) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Halo Color")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut color = egui::Color32::from_rgb(
                        (transition.user_params.color[0] * 255.0) as u8,
                        (transition.user_params.color[1] * 255.0) as u8,
                        (transition.user_params.color[2] * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        let mut new_params = transition.user_params;
                        new_params.color = [
                            color.r() as f32 / 255.0,
                            color.g() as f32 / 255.0,
                            color.b() as f32 / 255.0,
                            1.0,
                        ];
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                for (label, field, range, speed) in [
                    ("Threshold", 0u8, 0.0..=1.0, 0.01),
                    ("Spread", 1u8, 0.0..=2.0, 0.05),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::GlitchRGBSplit
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Intensity", 0u8, 0.0..=2.0, 0.05),
                    ("Block Size", 1u8, 2.0..=32.0, 0.5),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::StaticTV) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Intensity", 0u8, 0.0..=2.0, 0.05),
                    ("Scan Line Size", 1u8, 1.0..=8.0, 0.5),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::VHSTracking) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Intensity", 0u8, 0.0..=2.0, 0.05),
                    ("Bands", 1u8, 2.0..=16.0, 0.5),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::Swirl) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Angle")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v)
                            .range(0.0..=360.0)
                            .speed(1.0)
                            .suffix("°"),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Falloff")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param1;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v).range(0.1..=3.0).speed(0.05),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param1 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::WaveDistortion
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Amplitude", 0u8, 0.0..=60.0, 0.5),
                    ("Frequency", 1u8, 1.0..=16.0, 0.1),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Direction")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let dirs = ["Horizontal", "Vertical", "Both"];
                    let current = transition.user_params.param2.round() as usize;
                    egui::ComboBox::from_id_salt("wave_dir")
                        .selected_text(dirs.get(current).copied().unwrap_or("Horizontal"))
                        .show_ui(ui, |ui| {
                            for (i, name) in dirs.iter().enumerate() {
                                if ui.selectable_label(current == i, *name).clicked() {
                                    let mut new_params = transition.user_params;
                                    new_params.param2 = i as f32;
                                    cmds.push(AppCommand::SetTransitionParam {
                                        track_index: track_idx,
                                        transition_id: transition.id,
                                        user_params: new_params,
                                    });
                                }
                            }
                        });
                });
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::ColorizeFlash
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Flash Color")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut color = egui::Color32::from_rgb(
                        (transition.user_params.color[0] * 255.0) as u8,
                        (transition.user_params.color[1] * 255.0) as u8,
                        (transition.user_params.color[2] * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        let mut new_params = transition.user_params;
                        new_params.color = [
                            color.r() as f32 / 255.0,
                            color.g() as f32 / 255.0,
                            color.b() as f32 / 255.0,
                            1.0,
                        ];
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                for (label, field, range, speed) in [
                    ("Intensity", 0u8, 0.0..=1.0, 0.01),
                    ("Peak Position", 1u8, 0.1..=0.9, 0.01),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::HueRotate) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Rotations")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v)
                            .range(0.5..=3.0)
                            .speed(0.05)
                            .suffix("×"),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::NegativeFlash
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Intensity", 0u8, 0.0..=1.0, 0.01),
                    ("Peak Position", 1u8, 0.1..=0.9, 0.01),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::Sepia) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Strength", 0u8, 0.0..=1.0, 0.01),
                    ("Warmth", 1u8, 0.5..=2.0, 0.05),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::GlitchDissolve
            ) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Intensity", 0u8, 0.0..=2.0, 0.05),
                    ("Block Size", 1u8, 4.0..=64.0, 1.0),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            _ => transition.user_params.param1,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                _ => new_params.param1 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::Desaturate) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Depth")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v).range(0.0..=1.0).speed(0.01),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::AdditiveDissolve
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Intensity")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v).range(0.0..=2.0).speed(0.05),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::FadeToColor) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Fade Color")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut color = egui::Color32::from_rgb(
                        (transition.user_params.color[0] * 255.0) as u8,
                        (transition.user_params.color[1] * 255.0) as u8,
                        (transition.user_params.color[2] * 255.0) as u8,
                    );
                    if ui.color_edit_button_srgba(&mut color).changed() {
                        let mut new_params = transition.user_params;
                        new_params.color = [
                            color.r() as f32 / 255.0,
                            color.g() as f32 / 255.0,
                            color.b() as f32 / 255.0,
                            1.0,
                        ];
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Hold")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v).range(0.0..=0.45).speed(0.01),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::FilmBurn) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Temperature", 0u8, 0.0..=1.0, 0.01),
                    ("Speed", 1u8, 0.5..=2.0, 0.05),
                    ("Grain", 2u8, 0.0..=1.0, 0.01),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            1 => transition.user_params.param1,
                            _ => transition.user_params.param2,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                1 => new_params.param1 = v,
                                _ => new_params.param2 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(
                transition.kind,
                klipp_timeline::TransitionKind::ChromaticAberration
            ) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Offset")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v)
                            .range(0.0..=60.0)
                            .speed(0.5)
                            .suffix(" px"),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Direction")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let dirs = ["Horizontal", "Vertical", "Diagonal"];
                    let current = transition.user_params.param1.round() as usize;
                    egui::ComboBox::from_id_salt("chroma_dir")
                        .selected_text(dirs.get(current).copied().unwrap_or("Horizontal"))
                        .show_ui(ui, |ui| {
                            for (i, name) in dirs.iter().enumerate() {
                                if ui.selectable_label(current == i, *name).clicked() {
                                    let mut new_params = transition.user_params;
                                    new_params.param1 = i as f32;
                                    cmds.push(AppCommand::SetTransitionParam {
                                        track_index: track_idx,
                                        transition_id: transition.id,
                                        user_params: new_params,
                                    });
                                }
                            }
                        });
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::Ripple) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Amplitude", 0u8, 0.0..=60.0, 0.5),
                    ("Frequency", 1u8, 1.0..=20.0, 0.1),
                    ("Speed", 2u8, 0.5..=3.0, 0.05),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = match field {
                            0 => transition.user_params.param0,
                            1 => transition.user_params.param1,
                            _ => transition.user_params.param2,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                1 => new_params.param1 = v,
                                _ => new_params.param2 = v,
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::Pixelate) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Max Pixel Size")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut v)
                            .range(4.0..=128.0)
                            .speed(1.0)
                            .suffix(" px"),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = v;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Mode")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let cross = transition.user_params.param1 > 0.5;
                    let btn_label = if cross { "Cross Pixelate" } else { "Pixelate & Dissolve" };
                    if ui.button(btn_label).clicked() {
                        let mut new_params = transition.user_params;
                        new_params.param1 = if cross { 0.0 } else { 1.0 };
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::ClockWipe) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Softness", 0u8, 0.0..=4.0, 0.05),
                    ("Start Angle", 1u8, 0.0..=360.0, 1.0),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(label)
                                .color(theme.text_secondary)
                                .size(11.0),
                        );
                        let mut v = if field == 0 {
                            transition.user_params.param0
                        } else {
                            transition.user_params.param1
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            if field == 0 {
                                new_params.param0 = v;
                            } else {
                                new_params.param1 = v;
                            }
                            cmds.push(AppCommand::SetTransitionParam {
                                track_index: track_idx,
                                transition_id: transition.id,
                                user_params: new_params,
                            });
                        }
                    });
                }
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Direction")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let ccw = transition.user_params.param2 > 0.5;
                    let btn_label = if ccw { "Counter-Clockwise" } else { "Clockwise" };
                    if ui.button(btn_label).clicked() {
                        let mut new_params = transition.user_params;
                        new_params.param2 = if ccw { 0.0 } else { 1.0 };
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::LuminanceDissolve) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Softness")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut softness = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut softness)
                            .range(0.0..=3.0)
                            .speed(0.05),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = softness;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::PushLeft) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Direction")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    const DIRECTIONS: &[&str] = &["Left", "Right", "Up", "Down"];
                    let current_idx =
                        (transition.user_params.param0.round() as usize).min(DIRECTIONS.len() - 1);
                    egui::ComboBox::from_id_salt("push_direction")
                        .selected_text(DIRECTIONS[current_idx])
                        .show_ui(ui, |ui| {
                            for (i, name) in DIRECTIONS.iter().enumerate() {
                                let is_current = i == current_idx;
                                if ui.selectable_label(is_current, *name).clicked() && !is_current {
                                    let mut new_params = transition.user_params;
                                    new_params.param0 = i as f32;
                                    cmds.push(AppCommand::SetTransitionParam {
                                        track_index: track_idx,
                                        transition_id: transition.id,
                                        user_params: new_params,
                                    });
                                }
                            }
                        });
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::Spin) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Rotations")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut rotations = transition.user_params.param0;
                    let resp = ui.add(
                        egui::DragValue::new(&mut rotations)
                            .range(0.25..=3.0)
                            .speed(0.05)
                            .suffix("\u{00D7}"),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param0 = rotations;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Zoom")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut zoom = transition.user_params.param1;
                    let resp = ui.add(
                        egui::DragValue::new(&mut zoom)
                            .range(0.0..=1.0)
                            .speed(0.01),
                    );
                    if resp.changed() {
                        let mut new_params = transition.user_params;
                        new_params.param1 = zoom;
                        cmds.push(AppCommand::SetTransitionParam {
                            track_index: track_idx,
                            transition_id: transition.id,
                            user_params: new_params,
                        });
                    }
                });
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::CrossDissolve) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Easing")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    const CURVES: &[&str] = &["Linear", "Ease In", "Ease Out", "Ease In-Out", "Sine"];
                    let current_idx = (transition.user_params.param0 as usize).min(CURVES.len() - 1);
                    egui::ComboBox::from_id_salt("transition_easing")
                        .selected_text(CURVES[current_idx])
                        .show_ui(ui, |ui| {
                            for (i, name) in CURVES.iter().enumerate() {
                                let is_current = i == current_idx;
                                if ui.selectable_label(is_current, *name).clicked() && !is_current {
                                    let mut new_params = transition.user_params;
                                    new_params.param0 = i as f32;
                                    cmds.push(AppCommand::SetTransitionParam {
                                        track_index: track_idx,
                                        transition_id: transition.id,
                                        user_params: new_params,
                                    });
                                }
                            }
                        });
                });
            }

            ui.add_space(12.0);

            // Delete button
            if ui
                .add(egui::Button::new(
                    egui::RichText::new("Remove Transition")
                        .color(egui::Color32::from_rgb(255, 180, 180))
                        .size(11.0),
                ))
                .clicked()
            {
                cmds.push(AppCommand::RemoveTransition {
                    track_index: track_idx,
                    transition_id: transition.id,
                });
            }
        });
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
