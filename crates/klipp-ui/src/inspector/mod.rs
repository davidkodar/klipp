pub mod video_tab;
pub mod audio_tab;
pub mod audio_fx_inspector;
pub mod color_and_luts;
pub mod effects_tab;
pub mod info_tab;
pub mod modifier_ui;

use crate::icons::Icon;
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
    /// Clip id for which `duck` was last initialised. When the selection
    /// changes, we re-seed `duck` (prefs defaults + role-aware sidechain
    /// choice) so every new clip gets sensible starting values without
    /// overriding the user's in-session adjustments on the current clip.
    pub last_duck_clip: Option<klipp_core::ClipId>,
}


impl NewInspectorPanel {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        state: &AppState,
        prefs: &crate::prefs::Preferences,
        theme: &KlippTheme,
        active_tab: &mut InspectorTab,
    ) -> Vec<AppCommand> {
        let mut cmds = Vec::new();

        // ── Transition inspector (short-circuit clip inspector) ──
        if let Some((track_idx, trans_id)) = state.selected_transition {
            if let Some(track) = state.sequence().tracks.get(track_idx) {
                if let Some(transition) = track.transitions.iter().find(|t| t.id == trans_id) {
                    show_transition_inspector(ui, theme, track_idx, transition, state, &mut cmds);
                    return cmds;
                }
            }
        }

        // Resolve selected clip (timeline) or selected media (bin).
        // `selected_clip_loc()` is the cached form of the previous
        // tracks-iter walk — closes CODE_AUDIT U-11 (steady state
        // is one Track::find_clip rather than tracks × clips).
        let clip_info = state.selected_clip_loc();
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
                egui::Stroke::new(1.0_f32, theme.border),
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
                            let text_preview = if td.text().len() > 30 { format!("{}...", &td.text()[..27]) } else { td.text().clone() };
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
                    egui::Color32::from_rgba_premultiplied(theme.accent.r(), theme.accent.g(), theme.accent.b(), 30));
                ui.painter().hline(banner_rect.x_range(), banner_rect.max.y,
                    egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_premultiplied(theme.accent.r(), theme.accent.g(), theme.accent.b(), 60)));

                // Color bar on left
                let bar_rect = egui::Rect::from_min_size(
                    banner_rect.min, egui::vec2(4.0, banner_h),
                );
                let role_color = theme.clip_role_color(clip.role);
                ui.painter().rect_filled(bar_rect, 0.0, role_color);

                // Text — dark colors for readability on amber background
                let banner_text = theme.text_on_accent;
                let banner_tc = theme.text_on_accent;
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
                egui::Stroke::new(1.0_f32, theme.border),
            );
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                let is_title_clip = clip_info.as_ref().is_some_and(|(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Title(_)));
                let is_generator_clip = clip_info.as_ref().is_some_and(|(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Generator(_)));
                let tabs: Vec<(InspectorTab, &str)> = if is_title_clip {
                    vec![
                        (InspectorTab::Title, "TITLE"),
                        (InspectorTab::Video, "VIDEO"),
                        (InspectorTab::Effects, "EFFECTS"),
                        (InspectorTab::Info, "INFO"),
                    ]
                } else if is_generator_clip {
                    vec![
                        (InspectorTab::Generator, "GENERATOR"),
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
                            egui::Stroke::new(2.0_f32, theme.accent),
                        );
                    }
                    if resp.clicked() {
                        *active_tab = *tab;
                    }
                }
            });
        });
        ui.advance_cursor_after_rect(tab_rect);

        // Auto-switch to Title/Generator tab when a synthetic clip is selected (and away when not)
        let is_title_clip = clip_info.as_ref().is_some_and(|(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Title(_)));
        let is_generator_clip = clip_info.as_ref().is_some_and(|(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Generator(_)));
        if is_title_clip && *active_tab != InspectorTab::Title {
            *active_tab = InspectorTab::Title;
        } else if is_generator_clip && *active_tab != InspectorTab::Generator {
            *active_tab = InspectorTab::Generator;
        } else if !is_title_clip && *active_tab == InspectorTab::Title {
            *active_tab = InspectorTab::Video;
        } else if !is_generator_clip && *active_tab == InspectorTab::Generator {
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
                            .filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Audio)
                            .map(|(i, t)| audio_tab::TrackInfo {
                                index: i,
                                name: if t.name.is_empty() { format!("Track {}", i + 1) } else { t.name.clone() },
                            })
                            .collect();
                        // Re-seed DuckSettings when the selected clip changes: pull
                        // defaults from prefs and pick the first Dialogue-bearing audio
                        // track as the sidechain source (falls back to first audio track).
                        if self.last_duck_clip != Some(clip.id) {
                            self.duck = audio_tab::DuckSettings::from_prefs(&prefs.audio);
                            let dialogue_track = state.sequence().tracks.iter().enumerate()
                                .find(|(_, t)| {
                                    t.kind == klipp_timeline::TrackKind::Audio
                                        && t.clips.iter().any(|c| c.role == klipp_core::ClipRole::Dialogue)
                                })
                                .map(|(i, _)| i);
                            let first_audio = track_infos.first().map(|ti| ti.index);
                            self.duck.sidechain_track = dialogue_track.or(first_audio).unwrap_or(0);
                            self.last_duck_clip = Some(clip.id);
                        }
                        let is_timeline = true; // clip_info comes from timeline selection
                        audio_tab::show(ui, theme, state, Some((clip, track_idx, is_timeline)), &mut cmds, peak, peaks_slice, source_channels, clip_time_secs, &mut self.duck, &track_infos);
                    } else if let Some(bm) = bin_media {
                        // Bin media selected — show audio info for the media item
                        let source_channels = bm.audio_channels;
                        let track_infos: Vec<audio_tab::TrackInfo> = state.sequence().tracks.iter().enumerate()
                            .filter(|(_, t)| t.kind == klipp_timeline::TrackKind::Audio)
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
                            linked_title_asset_id: None,
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
                            audio_effect_keyframes: std::collections::HashMap::new(),
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
                    crate::inspector::color_and_luts::show_color_and_luts(ui, state, theme, &mut cmds);
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
                InspectorTab::Generator => {
                    if let Some((_, clip)) = clip_info {
                        if let klipp_timeline::ClipKind::Generator(_) = &clip.kind {
                            show_generator_editor(ui, theme, clip, &mut cmds);
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

/// Inspector controls for a Shape layer: fill / stroke / corner
/// egui ctx data key under which the user's set of expanded Title-
/// inspector group names is stored. Same naming convention as
/// `klipp_collapsed_sections` (the section_card persistence used by
/// the Video / Audio / Info inspectors). The startup hook seeds this
/// from `prefs.title.inspector_expanded_groups`; the debounced
/// preference-save hook copies it back so the user's expansion
/// choices survive restarts.
pub const TITLE_EXPANDED_GROUPS_KEY: &str = "klipp_title_expanded_groups";

/// Title-inspector preset registry — display name + numeric id sent
/// in `AppCommand::ApplyTitlePreset`. Id `0` is the inert "None"
/// sentinel so reopening the dropdown shows a stable label without
/// re-dispatching the last applied preset. Adding a new preset
/// requires extending this list AND the matching arm in
/// `apply_title_preset` (`klipp-ui/src/app.rs`).
const PRESET_REGISTRY: &[(u8, &str)] = &[
    (0, "None"),
    (1, "Neon"),
    (2, "Classic"),
    (3, "Bold Shadow"),
    (4, "Outlined"),
    (5, "VHS"),
];

/// Render a collapsible group header with status indicator and return
/// `true` when the body should be drawn. Expansion state is per-user
/// (not per-clip) and persisted in `prefs.title.inspector_expanded_groups`
/// — toggling `Effects` open on one title sets it open on every title.
/// Default is empty (every group collapsed) on a fresh install.
fn title_group_is_open(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    _clip_id: klipp_core::ClipId,
    title: &str,
    status: crate::widgets::inspector_group::GroupStatus,
) -> bool {
    use crate::widgets::inspector_group::inspector_group_header;
    let key = egui::Id::new(TITLE_EXPANDED_GROUPS_KEY);
    let is_open: bool = ui.ctx().data(|d| {
        d.get_temp::<std::collections::HashSet<String>>(key)
            .map(|set| set.contains(title))
            .unwrap_or(false)
    });
    if inspector_group_header(ui, title, is_open, status, theme).clicked() {
        ui.ctx().data_mut(|d| {
            let set: &mut std::collections::HashSet<String> =
                d.get_temp_mut_or_default(key);
            if is_open {
                set.remove(title);
            } else {
                set.insert(title.to_string());
            }
        });
        // Reflect the just-applied toggle in this frame so the body
        // expansion matches the visual chevron immediately rather
        // than one frame later.
        return !is_open;
    }
    is_open
}

/// Build a `KeyframeState` describing how `clip_time_secs` relates to
/// the keyframe vec. Mirrors `inspector::video_tab::keyframe_state` —
/// duplicated here to avoid making the video_tab helper public, since
/// the title path has its own playhead semantics that may diverge
/// later (per-clip vs per-object keyframe targets).
fn title_object_keyframe_state(
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
) -> crate::widgets::scrubby_param::KeyframeState {
    let kf_tolerance = 0.02;
    crate::widgets::scrubby_param::KeyframeState {
        has_keyframe_at_playhead: keyframes
            .iter()
            .any(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance),
        has_any_keyframes: !keyframes.is_empty(),
        prev_keyframe_time: keyframes
            .iter()
            .rfind(|k| k.time_secs < clip_time_secs - kf_tolerance)
            .map(|k| k.time_secs),
        next_keyframe_time: keyframes
            .iter()
            .find(|k| k.time_secs > clip_time_secs + kf_tolerance)
            .map(|k| k.time_secs),
        current_interpolation: keyframes
            .iter()
            .find(|k| (k.time_secs - clip_time_secs).abs() < kf_tolerance)
            .map(|k| k.interpolation),
    }
}

/// Translate a `ScrubbyParamResponse` for one position axis into the
/// corresponding AppCommand(s). Three affordances:
/// - Diamond click: toggle a keyframe at the playhead.
/// - Scrubby drag (when keyframes exist): upsert a keyframe at the
///   playhead with the new value (handler dedups within ±20ms).
/// - Scrubby drag (when no keyframes): SetTitleObjectTransform updates
///   the static `center` fallback. Falls back to SetTitlePosition when
///   no specific object is selected.
/// - Right-click → set interpolation: dispatched per-axis when the
///   keyframe at the playhead exists.
fn handle_position_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    axis: klipp_core::TitlePositionAxis,
    full_x: f32,
    full_y: f32,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
) {
    let Some(oid) = target_oid else {
        return;
    };
    let object_id = oid.0;
    let new_axis_value = match axis {
        klipp_core::TitlePositionAxis::X => full_x,
        klipp_core::TitlePositionAxis::Y => full_y,
    };

    if resp.keyframe_toggled {
        if keyframes
            .iter()
            .any(|k| (k.time_secs - clip_time_secs).abs() < 0.02)
        {
            cmds.push(AppCommand::RemoveTitleObjectPositionKeyframe {
                clip_id,
                object_id,
                axis,
                time_secs: clip_time_secs,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectPositionKeyframe {
                clip_id,
                object_id,
                axis,
                time_secs: clip_time_secs,
                value: new_axis_value,
            });
        }
        return;
    }

    if resp.changed {
        if keyframes.is_empty() {
            // No keyframes yet — edit the static center fallback.
            // SetTitlePosition is clip-wide; the layer-specific path
            // is intentionally minimal here so existing on-canvas
            // drags + Inspector value typing keep working.
            cmds.push(AppCommand::SetTitlePosition {
                clip_id,
                x: full_x,
                y: full_y,
            });
        } else {
            // Keyframes exist — drag becomes an upsert at the playhead.
            cmds.push(AppCommand::AddTitleObjectPositionKeyframe {
                clip_id,
                object_id,
                axis,
                time_secs: clip_time_secs,
                value: new_axis_value,
            });
        }
    }

    if let Some(interp) = resp.keyframe_set_interpolation {
        if let Some((idx, _)) = keyframes
            .iter()
            .enumerate()
            .find(|(_, k)| (k.time_secs - clip_time_secs).abs() < 0.02)
        {
            cmds.push(AppCommand::SetTitleObjectPositionKeyframeInterpolation {
                clip_id,
                object_id,
                axis,
                keyframe_index: idx,
                interpolation: interp,
            });
        }
    }
}

/// Translate a `ScrubbyParamResponse` for one keyframable TextGlow
/// param into the right AppCommand(s). Same three affordances as the
/// position axis handler but routed through the per-effect command
/// family with a stringly-typed `param_key`:
/// - Diamond click: toggle a keyframe at the playhead.
/// - Scrubby drag (when keyframes exist): upsert at the playhead.
/// - Scrubby drag (when no keyframes): SetTitleGlow updates the
///   static fallback. Static-only when no object is selected.
/// - Right-click → set interpolation: dispatched per-param.
fn handle_glow_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    param_key: &'static str,
    new_value: f32,
    cur: &klipp_timeline::TextGlow,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
) {
    let Some(oid) = target_oid else {
        // No specific layer selected — fall back to the static
        // SetTitleGlow path. Drag-only; the diamond does nothing
        // because we have no object to attach a keyframe to.
        if resp.changed {
            let (radius, intensity) = match param_key {
                klipp_core::title_effect_keys::GLOW_RADIUS => (new_value, cur.intensity),
                klipp_core::title_effect_keys::GLOW_INTENSITY => (cur.radius, new_value),
                _ => (cur.radius, cur.intensity),
            };
            cmds.push(AppCommand::SetTitleGlow {
                clip_id, enabled: true,
                color: cur.color, radius, intensity,
            });
        }
        return;
    };
    let object_id = oid.0;

    if resp.keyframe_toggled {
        if keyframes
            .iter()
            .any(|k| (k.time_secs - clip_time_secs).abs() < 0.02)
        {
            cmds.push(AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                time_secs: clip_time_secs,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                time_secs: clip_time_secs,
                value: new_value,
            });
        }
        return;
    }

    if resp.changed {
        if keyframes.is_empty() {
            // No keyframes — drag edits the static fallback through
            // the existing SetTitleGlow command, preserving the
            // un-keyframed fields.
            let (radius, intensity) = match param_key {
                klipp_core::title_effect_keys::GLOW_RADIUS => (new_value, cur.intensity),
                klipp_core::title_effect_keys::GLOW_INTENSITY => (cur.radius, new_value),
                _ => (cur.radius, cur.intensity),
            };
            cmds.push(AppCommand::SetTitleGlow {
                clip_id, enabled: true,
                color: cur.color, radius, intensity,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                time_secs: clip_time_secs,
                value: new_value,
            });
        }
    }

    if let Some(interp) = resp.keyframe_set_interpolation {
        if let Some((idx, _)) = keyframes
            .iter()
            .enumerate()
            .find(|(_, k)| (k.time_secs - clip_time_secs).abs() < 0.02)
        {
            cmds.push(AppCommand::SetTitleObjectEffectKeyframeInterpolation {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                keyframe_index: idx,
                interpolation: interp,
            });
        }
    }
}

/// Translate a `ScrubbyParamResponse` for one keyframable TitleShadow
/// param into the right AppCommand(s). Mirrors `handle_glow_axis_response`
/// for the three shadow axes (offset_x / offset_y / blur).
fn handle_shadow_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    param_key: &'static str,
    new_value: f32,
    cur: &klipp_timeline::TitleShadow,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
) {
    // Map the param key + new value back to the (offset_x, offset_y,
    // blur) triple SetTitleShadow expects when no keyframes exist.
    let to_static_triple = |new: f32| -> (f32, f32, f32) {
        match param_key {
            klipp_core::title_effect_keys::SHADOW_OFFSET_X => (new, cur.offset_y, cur.blur),
            klipp_core::title_effect_keys::SHADOW_OFFSET_Y => (cur.offset_x, new, cur.blur),
            klipp_core::title_effect_keys::SHADOW_BLUR => (cur.offset_x, cur.offset_y, new),
            _ => (cur.offset_x, cur.offset_y, cur.blur),
        }
    };
    let Some(oid) = target_oid else {
        if resp.changed {
            let (offset_x, offset_y, blur) = to_static_triple(new_value);
            cmds.push(AppCommand::SetTitleShadow {
                clip_id, enabled: true,
                color: cur.color, offset_x, offset_y, blur,
            });
        }
        return;
    };
    let object_id = oid.0;

    if resp.keyframe_toggled {
        if keyframes
            .iter()
            .any(|k| (k.time_secs - clip_time_secs).abs() < 0.02)
        {
            cmds.push(AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                time_secs: clip_time_secs,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                time_secs: clip_time_secs,
                value: new_value,
            });
        }
        return;
    }

    if resp.changed {
        if keyframes.is_empty() {
            let (offset_x, offset_y, blur) = to_static_triple(new_value);
            cmds.push(AppCommand::SetTitleShadow {
                clip_id, enabled: true,
                color: cur.color, offset_x, offset_y, blur,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                time_secs: clip_time_secs,
                value: new_value,
            });
        }
    }

    if let Some(interp) = resp.keyframe_set_interpolation {
        if let Some((idx, _)) = keyframes
            .iter()
            .enumerate()
            .find(|(_, k)| (k.time_secs - clip_time_secs).abs() < 0.02)
        {
            cmds.push(AppCommand::SetTitleObjectEffectKeyframeInterpolation {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                keyframe_index: idx,
                interpolation: interp,
            });
        }
    }
}

/// Translate a `ScrubbyParamResponse` for one keyframable InnerShadow
/// param into the right AppCommand(s). Mirrors
/// `handle_shadow_axis_response` for the inner-shadow path; the only
/// differences are the param-key constants and the static-fallback
/// command (`SetTitleInnerShadow`).
fn handle_inner_shadow_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    param_key: &'static str,
    new_value: f32,
    cur: &klipp_timeline::InnerShadow,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
) {
    let to_static_triple = |new: f32| -> (f32, f32, f32) {
        match param_key {
            klipp_core::title_effect_keys::INNER_SHADOW_OFFSET_X => (new, cur.offset_y, cur.blur),
            klipp_core::title_effect_keys::INNER_SHADOW_OFFSET_Y => (cur.offset_x, new, cur.blur),
            klipp_core::title_effect_keys::INNER_SHADOW_BLUR => (cur.offset_x, cur.offset_y, new),
            _ => (cur.offset_x, cur.offset_y, cur.blur),
        }
    };
    let Some(oid) = target_oid else {
        if resp.changed {
            let (offset_x, offset_y, blur) = to_static_triple(new_value);
            cmds.push(AppCommand::SetTitleInnerShadow {
                clip_id, enabled: true,
                color: cur.color, offset_x, offset_y, blur,
            });
        }
        return;
    };
    let object_id = oid.0;

    if resp.keyframe_toggled {
        if keyframes
            .iter()
            .any(|k| (k.time_secs - clip_time_secs).abs() < 0.02)
        {
            cmds.push(AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                time_secs: clip_time_secs,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                time_secs: clip_time_secs,
                value: new_value,
            });
        }
        return;
    }

    if resp.changed {
        if keyframes.is_empty() {
            let (offset_x, offset_y, blur) = to_static_triple(new_value);
            cmds.push(AppCommand::SetTitleInnerShadow {
                clip_id, enabled: true,
                color: cur.color, offset_x, offset_y, blur,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                time_secs: clip_time_secs,
                value: new_value,
            });
        }
    }

    if let Some(interp) = resp.keyframe_set_interpolation {
        if let Some((idx, _)) = keyframes
            .iter()
            .enumerate()
            .find(|(_, k)| (k.time_secs - clip_time_secs).abs() < 0.02)
        {
            cmds.push(AppCommand::SetTitleObjectEffectKeyframeInterpolation {
                clip_id,
                object_id,
                param_key: param_key.to_string(),
                keyframe_index: idx,
                interpolation: interp,
            });
        }
    }
}

/// Shared Position block: X/Y inputs + three sticky-active quick-preset
/// buttons (Top / Center / Lower 3rd). Active highlight stays while X/Y
/// are within ±0.005 of the canonical coords AND that preset was the
/// most-recently-clicked one (stored in egui ctx data, scoped per
/// clip_id).
fn show_title_position_block(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    td: &klipp_timeline::TitleData,
    state: &AppState,
    cmds: &mut Vec<AppCommand>,
    clip_time_secs: f64,
) {
    use crate::state::TitlePositionPreset;
    use crate::widgets::inspector_group::quick_preset_button;
    use crate::widgets::scrubby_param::scrubby_param_row;

    let target_oid = state
        .selected_title_object
        .filter(|(c, _)| *c == clip_id)
        .map(|(_, oid)| oid);
    let target_obj = target_oid.and_then(|oid| td.object(oid));
    // Pos comes from keyframe-evaluated center when keyframes exist —
    // otherwise the static fallback. Same call the renderer uses, so
    // the inspector value tracks the rendered value at the playhead.
    let pos = target_obj
        .map(|o| o.transform.evaluated_center(clip_time_secs))
        .unwrap_or_else(|| td.position());

    let kfs_x: &[klipp_core::Keyframe] = target_obj
        .map(|o| o.transform.center_x_keyframes.as_slice())
        .unwrap_or(&[]);
    let kfs_y: &[klipp_core::Keyframe] = target_obj
        .map(|o| o.transform.center_y_keyframes.as_slice())
        .unwrap_or(&[]);
    let ks_x = title_object_keyframe_state(kfs_x, clip_time_secs);
    let ks_y = title_object_keyframe_state(kfs_y, clip_time_secs);

    ui.label(
        egui::RichText::new("POSITION")
            .size(10.0)
            .color(theme.text_dim),
    );
    ui.add_space(4.0);

    // X / Y stacked scrubby rows with per-layer keyframe affordances.
    // The diamond toggles a keyframe at the playhead; scrubby drag
    // upserts when keyframes exist, otherwise it edits the static
    // fallback through SetTitlePosition.
    let mut x = pos.x;
    let mut y = pos.y;
    let resp_x = scrubby_param_row(
        ui, "X", &mut x, 0.0..=1.0, 0.5, 0.001, 2, "",
        target_oid.map(|_| ks_x),
    );
    let resp_y = scrubby_param_row(
        ui, "Y", &mut y, 0.0..=1.0, 0.5, 0.001, 2, "",
        target_oid.map(|_| ks_y),
    );
    handle_position_axis_response(
        cmds, clip_id, target_oid, klipp_core::TitlePositionAxis::X,
        x, pos.y, kfs_x, clip_time_secs, &resp_x,
    );
    handle_position_axis_response(
        cmds, clip_id, target_oid, klipp_core::TitlePositionAxis::Y,
        pos.x, y, kfs_y, clip_time_secs, &resp_y,
    );
    // Static-fallback path: no per-layer object selected (legacy
    // behaviour) — fall back to the clip-level SetTitlePosition.
    if target_oid.is_none() && (resp_x.changed || resp_y.changed) {
        cmds.push(AppCommand::SetTitlePosition { clip_id, x, y });
    }

    ui.add_space(6.0);

    let preset_key = egui::Id::new(("klipp_title_pos_preset_clicked", clip_id));
    let last_clicked: Option<TitlePositionPreset> =
        ui.ctx().data(|d| d.get_temp(preset_key));
    let active_preset = last_clicked.filter(|p| p.matches(pos.x, pos.y));

    ui.horizontal(|ui| {
        let avail = ui.available_width();
        let third = (avail - 12.0) / 3.0;
        for preset in [
            TitlePositionPreset::Center,
            TitlePositionPreset::LowerThird,
            TitlePositionPreset::Top,
        ] {
            let label = match preset {
                TitlePositionPreset::Top => "Top",
                TitlePositionPreset::Center => "Center",
                TitlePositionPreset::LowerThird => "Lower 3rd",
            };
            let is_active = active_preset == Some(preset);
            ui.allocate_ui_with_layout(
                egui::vec2(third, 30.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    if quick_preset_button(ui, label, is_active, theme).clicked() {
                        let (px, py) = preset.coords();
                        cmds.push(AppCommand::SetTitlePosition { clip_id, x: px, y: py });
                        ui.ctx().data_mut(|d| {
                            d.insert_temp::<TitlePositionPreset>(preset_key, preset);
                        });
                    }
                },
            );
            ui.add_space(6.0);
        }
    });
}

/// New TextBox pinned-core block per the redesign:
/// editable textarea → font + size row → B/I/U/S + L/C/R row →
/// color swatch + alpha slider + % row → Position block. Reads the
/// selected layer's TextBox so multi-layer titles show the selected
/// layer's properties; selection-aware on size + colour preserves the
/// existing per-range editing feature.
fn show_title_pinned_core_textbox(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    td: &klipp_timeline::TitleData,
    state: &AppState,
    cmds: &mut Vec<AppCommand>,
    tb: &klipp_timeline::TextBox,
    clip_time_secs: f64,
) {
    let effective_selection = state
        .viewer_text_selection
        .or(state.viewer_text_selection_locked);
    let has_selection =
        state.viewer_text_edit && effective_selection.is_some_and(|(s, e)| s < e);
    let sel_range = effective_selection.unwrap_or((0, 0));

    let (display_size, size_is_mixed) =
        if has_selection && !tb.style_runs.is_empty() {
            let mut sizes: Vec<f32> = Vec::new();
            for run in &tb.style_runs {
                if run.start < sel_range.1 && run.end > sel_range.0 {
                    sizes.push(run.font_size.unwrap_or(tb.font_size));
                }
            }
            if sizes.is_empty() {
                (tb.font_size, false)
            } else if sizes.iter().all(|s| (*s - sizes[0]).abs() < 0.5) {
                (sizes[0], false)
            } else {
                (sizes[0], true)
            }
        } else if state.viewer_text_edit {
            (state.viewer_text_cursor_font_size.unwrap_or(tb.font_size), false)
        } else {
            (tb.font_size, false)
        };

    let (display_color, color_is_mixed) =
        if has_selection && !tb.style_runs.is_empty() {
            let mut colors: Vec<[u8; 4]> = Vec::new();
            for run in &tb.style_runs {
                if run.start < sel_range.1 && run.end > sel_range.0 {
                    colors.push(run.color.unwrap_or(tb.color));
                }
            }
            if colors.is_empty() {
                (tb.color, false)
            } else if colors.iter().all(|c| *c == colors[0]) {
                (colors[0], false)
            } else {
                (colors[0], true)
            }
        } else if state.viewer_text_edit {
            (state.viewer_text_cursor_color.unwrap_or(tb.color), false)
        } else {
            (tb.color, false)
        };

    // 1. Editable textarea — primary edit path.
    let mut text_buf = tb.text.clone();
    let text_resp = ui.add(
        egui::TextEdit::multiline(&mut text_buf)
            .desired_width(f32::INFINITY)
            .desired_rows(2)
            .font(egui::FontId::proportional(13.0)),
    );
    if text_resp.changed() {
        cmds.push(AppCommand::UpdateTitleData {
            clip_id,
            text: text_buf,
            font_size: tb.font_size,
            font_name: tb.font_name.clone(),
            font_style: font_style_to_u8(tb.font_style),
            color: tb.color,
        });
    }
    ui.add_space(8.0);

    // 2. Font ComboBox + size DragValue.
    let display_name = if tb.font_name == "sans-serif" || tb.font_name.is_empty() {
        "Noto Sans (Default)"
    } else {
        tb.font_name.as_str()
    };
    ui.horizontal(|ui| {
        let total = ui.available_width();
        let size_w = 70.0;
        let combo_w = (total - size_w - 8.0).max(80.0);
        egui::ComboBox::from_id_salt("title_pinned_font")
            .selected_text(display_name)
            .width(combo_w)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(
                        tb.font_name == "sans-serif" || tb.font_name.is_empty(),
                        "Noto Sans (Default)",
                    )
                    .clicked()
                {
                    cmds.push(AppCommand::UpdateTitleData {
                        clip_id,
                        text: tb.text.clone(),
                        font_size: tb.font_size,
                        font_name: "sans-serif".to_string(),
                        font_style: font_style_to_u8(tb.font_style),
                        color: tb.color,
                    });
                }
                ui.separator();
                for family in &state.available_fonts {
                    if ui.selectable_label(tb.font_name == *family, family).clicked() {
                        cmds.push(AppCommand::UpdateTitleData {
                            clip_id,
                            text: tb.text.clone(),
                            font_size: tb.font_size,
                            font_name: family.clone(),
                            font_style: font_style_to_u8(tb.font_style),
                            color: tb.color,
                        });
                    }
                }
            });
        ui.add_space(8.0);
        let mut size = display_size;
        let size_resp = ui.add(
            egui::DragValue::new(&mut size)
                .range(12.0..=300.0)
                .speed(1.0)
                .suffix(" pt"),
        );
        if size_resp.changed() {
            if has_selection && sel_range.0 < sel_range.1 {
                cmds.push(AppCommand::ApplyTitleStyle {
                    clip_id,
                    range_start: sel_range.0,
                    range_end: sel_range.1,
                    property: "font_size".into(),
                    value: format!("{size}"),
                });
            } else {
                cmds.push(AppCommand::UpdateTitleData {
                    clip_id,
                    text: tb.text.clone(),
                    font_size: size,
                    font_name: tb.font_name.clone(),
                    font_style: font_style_to_u8(tb.font_style),
                    color: tb.color,
                });
            }
        }
        if size_is_mixed {
            ui.label(egui::RichText::new("Mixed").size(9.0).color(theme.accent));
        }
    });
    ui.add_space(8.0);

    // 3. B / I / U / S buttons (left) + L / C / R alignment (right).
    let is_bold = matches!(
        tb.font_style,
        klipp_timeline::FontStyle::Bold | klipp_timeline::FontStyle::BoldItalic
    );
    let is_italic = matches!(
        tb.font_style,
        klipp_timeline::FontStyle::Italic | klipp_timeline::FontStyle::BoldItalic
    );
    ui.horizontal(|ui| {
        if ui
            .selectable_label(is_bold, egui::RichText::new(" B ").strong().size(13.0))
            .clicked()
        {
            let new_style = match (!is_bold, is_italic) {
                (true, true) => klipp_timeline::FontStyle::BoldItalic,
                (true, false) => klipp_timeline::FontStyle::Bold,
                (false, true) => klipp_timeline::FontStyle::Italic,
                (false, false) => klipp_timeline::FontStyle::Regular,
            };
            cmds.push(AppCommand::UpdateTitleData {
                clip_id,
                text: tb.text.clone(),
                font_size: tb.font_size,
                font_name: tb.font_name.clone(),
                font_style: font_style_to_u8(new_style),
                color: tb.color,
            });
        }
        if ui
            .selectable_label(is_italic, egui::RichText::new(" I ").italics().size(13.0))
            .clicked()
        {
            let new_style = match (is_bold, !is_italic) {
                (true, true) => klipp_timeline::FontStyle::BoldItalic,
                (true, false) => klipp_timeline::FontStyle::Bold,
                (false, true) => klipp_timeline::FontStyle::Italic,
                (false, false) => klipp_timeline::FontStyle::Regular,
            };
            cmds.push(AppCommand::UpdateTitleData {
                clip_id,
                text: tb.text.clone(),
                font_size: tb.font_size,
                font_name: tb.font_name.clone(),
                font_style: font_style_to_u8(new_style),
                color: tb.color,
            });
        }
        if ui
            .selectable_label(tb.underline, egui::RichText::new(" U ").underline().size(13.0))
            .clicked()
        {
            cmds.push(AppCommand::SetTitleUnderline { clip_id, underline: !tb.underline });
        }
        if ui
            .selectable_label(
                tb.strikethrough,
                egui::RichText::new(" S ").strikethrough().size(13.0),
            )
            .clicked()
        {
            cmds.push(AppCommand::SetTitleStrikethrough {
                clip_id,
                strikethrough: !tb.strikethrough,
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let cur = match tb.alignment {
                klipp_timeline::TextAlignment::Right => 2u8,
                klipp_timeline::TextAlignment::Center => 1,
                klipp_timeline::TextAlignment::Left => 0,
            };
            for (val, label) in [(2u8, " R "), (1, " C "), (0, " L ")] {
                if ui
                    .selectable_label(cur == val, egui::RichText::new(label).size(13.0))
                    .clicked()
                    && cur != val
                {
                    cmds.push(AppCommand::SetTitleAlignment { clip_id, alignment: val });
                }
            }
        });
    });
    ui.add_space(8.0);

    // 4. Color swatch + opacity slider + % display in one row.
    let mut col = egui::Color32::from_rgba_premultiplied(
        display_color[0],
        display_color[1],
        display_color[2],
        display_color[3],
    );
    let mut color_changed = false;
    let mut new_alpha_u8: Option<u8> = None;
    ui.horizontal(|ui| {
        if ui.color_edit_button_srgba(&mut col).changed() {
            color_changed = true;
        }
        ui.add_space(6.0);
        let mut opacity_pct = ((display_color[3] as f32 / 255.0) * 100.0).round();
        let pct_label_w = 36.0;
        let slider_w = (ui.available_width() - pct_label_w - 8.0).max(60.0);
        ui.spacing_mut().slider_width = slider_w;
        if ui
            .add(
                egui::Slider::new(&mut opacity_pct, 0.0..=100.0)
                    .step_by(1.0)
                    .show_value(false),
            )
            .changed()
        {
            new_alpha_u8 = Some((opacity_pct / 100.0 * 255.0).round() as u8);
        }
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!("{}%", opacity_pct as u32))
                .size(11.0)
                .color(theme.text_secondary),
        );
        if color_is_mixed {
            ui.label(egui::RichText::new("Mixed").size(9.0).color(theme.accent));
        }
    });
    if let Some(a) = new_alpha_u8 {
        col = egui::Color32::from_rgba_premultiplied(col.r(), col.g(), col.b(), a);
        color_changed = true;
    }
    if color_changed {
        let new_rgba = [col.r(), col.g(), col.b(), col.a()];
        if has_selection && sel_range.0 < sel_range.1 {
            cmds.push(AppCommand::ApplyTitleStyle {
                clip_id,
                range_start: sel_range.0,
                range_end: sel_range.1,
                property: "color".into(),
                value: serde_json::to_string(&new_rgba).unwrap_or_default(),
            });
        } else {
            cmds.push(AppCommand::UpdateTitleData {
                clip_id,
                text: tb.text.clone(),
                font_size: tb.font_size,
                font_name: tb.font_name.clone(),
                font_style: font_style_to_u8(tb.font_style),
                color: new_rgba,
            });
        }
    }
    ui.add_space(10.0);

    // 5. Position block.
    show_title_position_block(ui, theme, clip_id, td, state, cmds, clip_time_secs);
}

/// Quick-style row: per-letter or selection-wide colour presets, the
/// per-character effect chips (Rainbow / Fire / Ocean / Sunset) when
/// there's a live selection, and the style-runs indicator. Renders as a
/// labelled mini-section inside the Typography group so the controls
/// finally have a permanent home.
fn show_title_quick_style_subsection(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    td: &klipp_timeline::TitleData,
    state: &AppState,
    cmds: &mut Vec<AppCommand>,
) {
    let effective_selection = state
        .viewer_text_selection
        .or(state.viewer_text_selection_locked);
    let has_selection =
        state.viewer_text_edit && effective_selection.is_some_and(|(s, e)| s < e);
    let sel_range = effective_selection.unwrap_or((0, 0));

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new("Quick Style")
            .size(10.0)
            .color(theme.text_secondary)
            .strong(),
    );
    ui.add_space(2.0);

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Quick").size(9.0).color(theme.text_dim));
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
            let resp = ui.add(
                egui::Button::new(egui::RichText::new(*label).size(9.0).color(
                    if c[0] as u16 + c[1] as u16 + c[2] as u16 > 400 {
                        egui::Color32::BLACK
                    } else {
                        egui::Color32::WHITE
                    },
                ))
                .fill(btn_color)
                .min_size(egui::vec2(18.0, 16.0)),
            );
            if resp.clicked() {
                if has_selection && sel_range.0 < sel_range.1 {
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id,
                        range_start: sel_range.0,
                        range_end: sel_range.1,
                        property: "color".into(),
                        value: serde_json::to_string(c).unwrap_or_default(),
                    });
                } else {
                    cmds.push(AppCommand::UpdateTitleData {
                        clip_id,
                        text: td.text().clone(),
                        font_size: td.font_size(),
                        font_name: td.font_name().clone(),
                        font_style: font_style_to_u8(td.font_style()),
                        color: *c,
                    });
                }
            }
        }
    });

    if has_selection && sel_range.1 > sel_range.0 {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Effects:").size(9.0).color(theme.text_dim));
            if ui.small_button("Rainbow").clicked() {
                let rainbow: &[[u8; 4]] = &[
                    [255, 50, 50, 255], [255, 150, 0, 255], [255, 230, 0, 255],
                    [50, 200, 50, 255], [50, 150, 255, 255], [100, 50, 255, 255],
                    [200, 50, 200, 255],
                ];
                let sel_text = &td.text()[sel_range.0..sel_range.1.min(td.text().len())];
                let mut byte_pos = sel_range.0;
                for (ci, ch) in sel_text.chars().enumerate() {
                    let color_idx = ci % rainbow.len();
                    let c = rainbow[color_idx];
                    let char_end = byte_pos + ch.len_utf8();
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id,
                        range_start: byte_pos,
                        range_end: char_end,
                        property: "color".into(),
                        value: serde_json::to_string(&c).unwrap_or_default(),
                    });
                    byte_pos = char_end;
                }
            }
            if ui.small_button("Fire").clicked() {
                let sel_text = &td.text()[sel_range.0..sel_range.1.min(td.text().len())];
                let char_count = sel_text.chars().count().max(1);
                let mut byte_pos = sel_range.0;
                for (ci, ch) in sel_text.chars().enumerate() {
                    let t = ci as f32 / (char_count - 1).max(1) as f32;
                    let c = [255u8, (50.0 + t * 200.0) as u8, (0.0 + t * 30.0) as u8, 255];
                    let char_end = byte_pos + ch.len_utf8();
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id,
                        range_start: byte_pos,
                        range_end: char_end,
                        property: "color".into(),
                        value: serde_json::to_string(&c).unwrap_or_default(),
                    });
                    byte_pos = char_end;
                }
            }
            if ui.small_button("Ocean").clicked() {
                let sel_text = &td.text()[sel_range.0..sel_range.1.min(td.text().len())];
                let char_count = sel_text.chars().count().max(1);
                let mut byte_pos = sel_range.0;
                for (ci, ch) in sel_text.chars().enumerate() {
                    let t = ci as f32 / (char_count - 1).max(1) as f32;
                    let c = [
                        (20.0 + t * 30.0) as u8,
                        (80.0 + t * 140.0) as u8,
                        (200.0 + t * 55.0) as u8,
                        255u8,
                    ];
                    let char_end = byte_pos + ch.len_utf8();
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id,
                        range_start: byte_pos,
                        range_end: char_end,
                        property: "color".into(),
                        value: serde_json::to_string(&c).unwrap_or_default(),
                    });
                    byte_pos = char_end;
                }
            }
            if ui.small_button("Sunset").clicked() {
                let sel_text = &td.text()[sel_range.0..sel_range.1.min(td.text().len())];
                let char_count = sel_text.chars().count().max(1);
                let mut byte_pos = sel_range.0;
                for (ci, ch) in sel_text.chars().enumerate() {
                    let t = ci as f32 / (char_count - 1).max(1) as f32;
                    let c = [
                        (150.0 + t * 105.0) as u8,
                        (30.0 + t * 100.0) as u8,
                        (200.0 - t * 170.0) as u8,
                        255u8,
                    ];
                    let char_end = byte_pos + ch.len_utf8();
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id,
                        range_start: byte_pos,
                        range_end: char_end,
                        property: "color".into(),
                        value: serde_json::to_string(&c).unwrap_or_default(),
                    });
                    byte_pos = char_end;
                }
            }
        });
    }

    if !td.style_runs().is_empty() {
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} style runs", td.style_runs().len()))
                    .size(9.0).color(theme.text_dim),
            );
            if ui.small_button("Clear All").clicked() {
                cmds.push(AppCommand::ClearTitleStyleRuns { clip_id });
            }
        });
    }
}

/// Effects collapsible group with 7 subcards (Glow, Outline, Drop Shadow,
/// Inner Shadow, Gradient Fill, Pattern Fill, Texture Fill) followed by
/// the Outline Layers stack. Soft-disable on each subcard preserves
/// params via the `enabled` field. Group expansion is sticky per
/// `clip_id` via egui ctx data.
fn show_title_effects_group(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    tb: &klipp_timeline::TextBox,
    cmds: &mut Vec<AppCommand>,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    clip_time_secs: f64,
) {
    use crate::widgets::inspector_group::{inspector_subgroup_card, GroupStatus};

    let glow_enabled = tb.glow.as_ref().is_some_and(|g| g.enabled);
    let outline_enabled = tb.outlines.first().is_some_and(|o| o.enabled);
    let shadow_enabled = tb.shadow.as_ref().is_some_and(|s| s.enabled);
    let inner_enabled = tb.inner_shadow.as_ref().is_some_and(|i| i.enabled);
    let gradient_enabled = tb.gradient.as_ref().is_some_and(|g| g.enabled);
    let pattern_enabled = tb.pattern_fill.is_some() && tb.pattern_fill_enabled;
    let texture_enabled = tb.texture_fill_path.is_some() && tb.texture_fill_enabled;

    let mut active_count: u32 = 0;
    for on in [
        glow_enabled, outline_enabled, shadow_enabled, inner_enabled,
        gradient_enabled, pattern_enabled, texture_enabled,
    ] {
        if on { active_count += 1; }
    }
    if tb.outlines.len() > 1 { active_count += 1; }

    let status = GroupStatus {
        count: if active_count > 0 { Some(active_count) } else { None },
        active: active_count > 0,
    };
    if !title_group_is_open(ui, theme, clip_id, "Effects", status) {
        return;
    }
    ui.add_space(6.0);

    // 1. Glow
    {
        let cur = tb.glow.clone().unwrap_or(klipp_timeline::TextGlow {
            color: [0, 200, 255, 255],
            radius: 10.0,
            intensity: 0.8,
            enabled: false,
            radius_keyframes: Vec::new(),
            intensity_keyframes: Vec::new(),
        });
        let mut enabled = glow_enabled;
        // Display the animated radius/intensity value at the playhead
        // when keyframes exist — matches what the renderer is showing
        // right now. Falls back to the static value when unanimated.
        let display_radius = cur.evaluated_radius(clip_time_secs);
        let display_intensity = cur.evaluated_intensity(clip_time_secs);
        let summary = if enabled {
            format!("{} px · {}%", display_radius.round() as u32, (display_intensity * 100.0).round() as u32)
        } else { String::new() };
        let kfs_radius: &[klipp_core::Keyframe] = tb
            .glow
            .as_ref()
            .map(|g| g.radius_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_intensity: &[klipp_core::Keyframe] = tb
            .glow
            .as_ref()
            .map(|g| g.intensity_keyframes.as_slice())
            .unwrap_or(&[]);
        let ks_radius = title_object_keyframe_state(kfs_radius, clip_time_secs);
        let ks_intensity = title_object_keyframe_state(kfs_intensity, clip_time_secs);
        inspector_subgroup_card(ui, "Glow", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut radius = display_radius;
            let mut intensity = display_intensity;
            let mut col = egui::Color32::from_rgba_premultiplied(cur.color[0], cur.color[1], cur.color[2], cur.color[3]);
            let mut color_changed = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Color").size(11.0).color(theme.text_dim));
                color_changed = ui.color_edit_button_srgba(&mut col).changed();
            });
            let resp_radius = crate::widgets::scrubby_param::scrubby_param_row(
                ui, "Radius", &mut radius, 1.0..=40.0, 10.0, 0.5, 1, " px",
                target_oid.map(|_| ks_radius),
            );
            let resp_intensity = crate::widgets::scrubby_param::scrubby_param_row(
                ui, "Intensity", &mut intensity, 0.0..=1.0, 0.8, 0.05, 2, "",
                target_oid.map(|_| ks_intensity),
            );
            // Color change always goes through the static SetTitleGlow
            // path — colour isn't keyframable in this slice.
            if color_changed {
                cmds.push(AppCommand::SetTitleGlow {
                    clip_id, enabled: true,
                    color: [col.r(), col.g(), col.b(), col.a()],
                    radius: cur.radius, intensity: cur.intensity,
                });
            }
            handle_glow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::GLOW_RADIUS,
                radius, &cur, kfs_radius, clip_time_secs, &resp_radius,
            );
            handle_glow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::GLOW_INTENSITY,
                intensity, &cur, kfs_intensity, clip_time_secs, &resp_intensity,
            );
        });
        if enabled != glow_enabled {
            cmds.push(AppCommand::SetTitleGlow {
                clip_id, enabled,
                color: cur.color, radius: cur.radius, intensity: cur.intensity,
            });
        }
    }
    ui.add_space(6.0);

    // 2. Outline (singular = outlines[0])
    {
        let cur = tb.outlines.first().copied().unwrap_or(klipp_timeline::TextOutline {
            color: [255, 255, 255, 255], width: 2.0, enabled: false,
        });
        let mut enabled = outline_enabled;
        let summary = if enabled { format!("{} px", cur.width) } else { String::new() };
        inspector_subgroup_card(ui, "Outline", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut width = cur.width;
            let mut col = egui::Color32::from_rgba_premultiplied(cur.color[0], cur.color[1], cur.color[2], cur.color[3]);
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Color").size(11.0).color(theme.text_dim));
                changed |= ui.color_edit_button_srgba(&mut col).changed();
            });
            changed |= crate::widgets::scrubby_param::scrubby_param_row(
                ui, "Width", &mut width, 0.5..=12.0, 2.0, 0.5, 1, " px", None,
            ).changed;
            if changed {
                cmds.push(AppCommand::SetTitleOutline {
                    clip_id, enabled: true,
                    color: [col.r(), col.g(), col.b(), col.a()], width,
                });
            }
        });
        if enabled != outline_enabled {
            cmds.push(AppCommand::SetTitleOutline {
                clip_id, enabled, color: cur.color, width: cur.width,
            });
        }
    }
    ui.add_space(6.0);

    // 3. Drop Shadow
    {
        let cur = tb.shadow.clone().unwrap_or(klipp_timeline::TitleShadow {
            color: [0, 0, 0, 180],
            offset_x: 3.0,
            offset_y: 3.0,
            blur: 0.0,
            enabled: false,
            offset_x_keyframes: Vec::new(),
            offset_y_keyframes: Vec::new(),
            blur_keyframes: Vec::new(),
        });
        let mut enabled = shadow_enabled;
        // Display animated values at the playhead — matches what the
        // renderer is showing right now.
        let display_ox = cur.evaluated_offset_x(clip_time_secs);
        let display_oy = cur.evaluated_offset_y(clip_time_secs);
        let display_blur = cur.evaluated_blur(clip_time_secs);
        let summary = if enabled {
            format!("{} px · {}%", display_blur.round() as u32, (cur.color[3] as f32 / 255.0 * 100.0).round() as u32)
        } else { String::new() };
        let kfs_ox: &[klipp_core::Keyframe] = tb
            .shadow
            .as_ref()
            .map(|s| s.offset_x_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_oy: &[klipp_core::Keyframe] = tb
            .shadow
            .as_ref()
            .map(|s| s.offset_y_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_blur: &[klipp_core::Keyframe] = tb
            .shadow
            .as_ref()
            .map(|s| s.blur_keyframes.as_slice())
            .unwrap_or(&[]);
        let ks_ox = title_object_keyframe_state(kfs_ox, clip_time_secs);
        let ks_oy = title_object_keyframe_state(kfs_oy, clip_time_secs);
        let ks_blur = title_object_keyframe_state(kfs_blur, clip_time_secs);
        inspector_subgroup_card(ui, "Drop Shadow", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut ox = display_ox;
            let mut oy = display_oy;
            let mut blur = display_blur;
            let mut col = egui::Color32::from_rgba_premultiplied(cur.color[0], cur.color[1], cur.color[2], cur.color[3]);
            let mut color_changed = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Color").size(11.0).color(theme.text_dim));
                color_changed = ui.color_edit_button_srgba(&mut col).changed();
            });
            let resp_ox = crate::widgets::scrubby_param::scrubby_param_row(
                ui, "Offset X", &mut ox, -20.0..=20.0, 3.0, 0.5, 1, " px",
                target_oid.map(|_| ks_ox),
            );
            let resp_oy = crate::widgets::scrubby_param::scrubby_param_row(
                ui, "Offset Y", &mut oy, -20.0..=20.0, 3.0, 0.5, 1, " px",
                target_oid.map(|_| ks_oy),
            );
            let resp_blur = crate::widgets::scrubby_param::scrubby_param_row(
                ui, "Blur", &mut blur, 0.0..=30.0, 0.0, 0.5, 1, " px",
                target_oid.map(|_| ks_blur),
            );
            // Color change always goes through the static SetTitleShadow
            // path — colour isn't keyframable in this slice.
            if color_changed {
                cmds.push(AppCommand::SetTitleShadow {
                    clip_id, enabled: true,
                    color: [col.r(), col.g(), col.b(), col.a()],
                    offset_x: cur.offset_x, offset_y: cur.offset_y, blur: cur.blur,
                });
            }
            handle_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::SHADOW_OFFSET_X,
                ox, &cur, kfs_ox, clip_time_secs, &resp_ox,
            );
            handle_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::SHADOW_OFFSET_Y,
                oy, &cur, kfs_oy, clip_time_secs, &resp_oy,
            );
            handle_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::SHADOW_BLUR,
                blur, &cur, kfs_blur, clip_time_secs, &resp_blur,
            );
        });
        if enabled != shadow_enabled {
            cmds.push(AppCommand::SetTitleShadow {
                clip_id, enabled,
                color: cur.color, offset_x: cur.offset_x, offset_y: cur.offset_y, blur: cur.blur,
            });
        }
    }
    ui.add_space(6.0);

    // 4. Inner Shadow
    {
        let cur = tb.inner_shadow.clone().unwrap_or(klipp_timeline::InnerShadow {
            color: [0, 0, 0, 180],
            offset_x: 2.0,
            offset_y: 2.0,
            blur: 3.0,
            enabled: false,
            offset_x_keyframes: Vec::new(),
            offset_y_keyframes: Vec::new(),
            blur_keyframes: Vec::new(),
        });
        let mut enabled = inner_enabled;
        // Display animated values at the playhead.
        let display_ox = cur.evaluated_offset_x(clip_time_secs);
        let display_oy = cur.evaluated_offset_y(clip_time_secs);
        let display_blur = cur.evaluated_blur(clip_time_secs);
        let summary = if enabled {
            format!("{} px · {}%", display_blur.round() as u32, (cur.color[3] as f32 / 255.0 * 100.0).round() as u32)
        } else { String::new() };
        let kfs_ox: &[klipp_core::Keyframe] = tb
            .inner_shadow
            .as_ref()
            .map(|s| s.offset_x_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_oy: &[klipp_core::Keyframe] = tb
            .inner_shadow
            .as_ref()
            .map(|s| s.offset_y_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_blur: &[klipp_core::Keyframe] = tb
            .inner_shadow
            .as_ref()
            .map(|s| s.blur_keyframes.as_slice())
            .unwrap_or(&[]);
        let ks_ox = title_object_keyframe_state(kfs_ox, clip_time_secs);
        let ks_oy = title_object_keyframe_state(kfs_oy, clip_time_secs);
        let ks_blur = title_object_keyframe_state(kfs_blur, clip_time_secs);
        inspector_subgroup_card(ui, "Inner Shadow", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut ox = display_ox;
            let mut oy = display_oy;
            let mut blur = display_blur;
            let mut col = egui::Color32::from_rgba_premultiplied(cur.color[0], cur.color[1], cur.color[2], cur.color[3]);
            let mut color_changed = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Color").size(11.0).color(theme.text_dim));
                color_changed = ui.color_edit_button_srgba(&mut col).changed();
            });
            let resp_ox = crate::widgets::scrubby_param::scrubby_param_row(
                ui, "Offset X", &mut ox, -20.0..=20.0, 2.0, 0.5, 1, " px",
                target_oid.map(|_| ks_ox),
            );
            let resp_oy = crate::widgets::scrubby_param::scrubby_param_row(
                ui, "Offset Y", &mut oy, -20.0..=20.0, 2.0, 0.5, 1, " px",
                target_oid.map(|_| ks_oy),
            );
            let resp_blur = crate::widgets::scrubby_param::scrubby_param_row(
                ui, "Blur", &mut blur, 0.0..=20.0, 3.0, 0.5, 1, " px",
                target_oid.map(|_| ks_blur),
            );
            if color_changed {
                cmds.push(AppCommand::SetTitleInnerShadow {
                    clip_id, enabled: true,
                    color: [col.r(), col.g(), col.b(), col.a()],
                    offset_x: cur.offset_x, offset_y: cur.offset_y, blur: cur.blur,
                });
            }
            handle_inner_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::INNER_SHADOW_OFFSET_X,
                ox, &cur, kfs_ox, clip_time_secs, &resp_ox,
            );
            handle_inner_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::INNER_SHADOW_OFFSET_Y,
                oy, &cur, kfs_oy, clip_time_secs, &resp_oy,
            );
            handle_inner_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::INNER_SHADOW_BLUR,
                blur, &cur, kfs_blur, clip_time_secs, &resp_blur,
            );
        });
        if enabled != inner_enabled {
            cmds.push(AppCommand::SetTitleInnerShadow {
                clip_id, enabled,
                color: cur.color, offset_x: cur.offset_x, offset_y: cur.offset_y, blur: cur.blur,
            });
        }
    }
    ui.add_space(6.0);

    // 5. Gradient Fill
    {
        let cur = tb.gradient.as_ref().cloned().unwrap_or(klipp_timeline::TextGradient {
            top_color: [255, 200, 50, 255], bottom_color: [255, 50, 50, 255], enabled: false,
        });
        let mut enabled = gradient_enabled;
        let summary = if enabled { "Linear · 2 stops".to_string() } else { String::new() };
        let cur_top = cur.top_color;
        let cur_bot = cur.bottom_color;
        inspector_subgroup_card(ui, "Gradient Fill", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut top_col = egui::Color32::from_rgba_premultiplied(cur_top[0], cur_top[1], cur_top[2], cur_top[3]);
            let mut bot_col = egui::Color32::from_rgba_premultiplied(cur_bot[0], cur_bot[1], cur_bot[2], cur_bot[3]);
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Top").size(11.0).color(theme.text_dim));
                changed |= ui.color_edit_button_srgba(&mut top_col).changed();
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Bottom").size(11.0).color(theme.text_dim));
                changed |= ui.color_edit_button_srgba(&mut bot_col).changed();
            });
            if changed {
                cmds.push(AppCommand::SetTitleGradient {
                    clip_id, enabled: true,
                    top_color: [top_col.r(), top_col.g(), top_col.b(), top_col.a()],
                    bottom_color: [bot_col.r(), bot_col.g(), bot_col.b(), bot_col.a()],
                });
            }
        });
        if enabled != gradient_enabled {
            cmds.push(AppCommand::SetTitleGradient {
                clip_id, enabled, top_color: cur_top, bottom_color: cur_bot,
            });
        }
    }
    ui.add_space(6.0);

    // 6. Pattern Fill
    {
        let mut enabled = pattern_enabled;
        let pat_idx: usize = match &tb.pattern_fill {
            None => 0,
            Some(klipp_timeline::TextPattern::HorizontalStripes { .. }) => 1,
            Some(klipp_timeline::TextPattern::VerticalStripes { .. }) => 2,
            Some(klipp_timeline::TextPattern::Checkerboard { .. }) => 3,
            Some(klipp_timeline::TextPattern::DiagonalStripes { .. }) => 4,
        };
        let labels = ["None", "H-Stripes", "V-Stripes", "Checker", "Diagonal"];
        let summary = if enabled { labels[pat_idx].to_string() } else { String::new() };
        inspector_subgroup_card(ui, "Pattern Fill", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut new_idx = pat_idx;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Kind").size(11.0).color(theme.text_dim));
                egui::ComboBox::from_id_salt("title_pattern_kind")
                    .selected_text(labels[new_idx])
                    .show_ui(ui, |ui| {
                        for (i, label) in labels.iter().enumerate() {
                            if ui.selectable_label(new_idx == i, *label).clicked() {
                                new_idx = i;
                            }
                        }
                    });
            });
            if new_idx != pat_idx {
                cmds.push(AppCommand::SetTitlePattern {
                    clip_id, pattern_kind: new_idx as u8,
                    color_a: [255, 255, 255, 255], color_b: [0, 0, 0, 255], size: 10.0,
                });
            }
            if let Some(pf) = &tb.pattern_fill {
                let (mut ca, mut cb, mut size) = match pf {
                    klipp_timeline::TextPattern::HorizontalStripes { color_a, color_b, stripe_width } => (*color_a, *color_b, *stripe_width),
                    klipp_timeline::TextPattern::VerticalStripes { color_a, color_b, stripe_width } => (*color_a, *color_b, *stripe_width),
                    klipp_timeline::TextPattern::Checkerboard { color_a, color_b, cell_size } => (*color_a, *color_b, *cell_size),
                    klipp_timeline::TextPattern::DiagonalStripes { color_a, color_b, stripe_width } => (*color_a, *color_b, *stripe_width),
                };
                let mut col_a = egui::Color32::from_rgba_premultiplied(ca[0], ca[1], ca[2], ca[3]);
                let mut col_b = egui::Color32::from_rgba_premultiplied(cb[0], cb[1], cb[2], cb[3]);
                let mut changed = false;
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("A").size(11.0).color(theme.text_dim));
                    changed |= ui.color_edit_button_srgba(&mut col_a).changed();
                    ui.add_space(8.0);
                    ui.label(egui::RichText::new("B").size(11.0).color(theme.text_dim));
                    changed |= ui.color_edit_button_srgba(&mut col_b).changed();
                });
                changed |= crate::widgets::scrubby_param::scrubby_param_row(
                    ui, "Size", &mut size, 2.0..=50.0, 10.0, 1.0, 0, " px", None,
                ).changed;
                if changed {
                    ca = [col_a.r(), col_a.g(), col_a.b(), col_a.a()];
                    cb = [col_b.r(), col_b.g(), col_b.b(), col_b.a()];
                    cmds.push(AppCommand::SetTitlePattern {
                        clip_id, pattern_kind: pat_idx as u8,
                        color_a: ca, color_b: cb, size,
                    });
                }
            }
        });
        if enabled != pattern_enabled {
            if enabled && tb.pattern_fill.is_none() {
                cmds.push(AppCommand::SetTitlePattern {
                    clip_id, pattern_kind: 1,
                    color_a: [255, 255, 255, 255], color_b: [0, 0, 0, 255], size: 10.0,
                });
            }
            cmds.push(AppCommand::SetTitlePatternEnabled { clip_id, enabled });
        }
    }
    ui.add_space(6.0);

    // 7. Texture Fill
    {
        let mut enabled = texture_enabled;
        let cur_path = tb.texture_fill_path.clone();
        let summary = if enabled {
            cur_path.as_ref()
                .and_then(|p| std::path::Path::new(p).file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("(no file)")
                .to_string()
        } else { String::new() };
        inspector_subgroup_card(ui, "Texture Fill", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("File").size(11.0).color(theme.text_dim));
                let display_name = cur_path.as_ref()
                    .and_then(|p| std::path::Path::new(p).file_name())
                    .and_then(|n| n.to_str())
                    .unwrap_or("(none)");
                ui.label(egui::RichText::new(display_name).monospace().size(11.0).color(theme.text_secondary));
            });
            if ui.button("Browse\u{2026}").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Images", &{
                        let mut exts: Vec<&str> = klipp_core::media_extensions::IMAGE_EXTENSIONS.to_vec();
                        exts.push("tga");
                        exts
                    })
                    .pick_file()
                {
                    cmds.push(AppCommand::SetTitleTextureFill {
                        clip_id, path: Some(path.to_string_lossy().to_string()),
                    });
                }
            }
        });
        if enabled != texture_enabled {
            cmds.push(AppCommand::SetTitleTextureEnabled { clip_id, enabled });
        }
    }
    ui.add_space(10.0);

    // 8. Outline Layers stack — outlines beyond outlines[0].
    {
        ui.label(egui::RichText::new("OUTLINE LAYERS").size(10.0).color(theme.text_dim));
        ui.add_space(4.0);
        let outlines = &tb.outlines;
        let mut new_outlines = outlines.clone();
        let mut outlines_changed = false;
        for i in 1..outlines.len() {
            let ol = outlines[i];
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(format!("#{}", i)).size(11.0).color(theme.text_dim));
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
                if ui.small_button("\u{00D7}").clicked() {
                    new_outlines.remove(i);
                    outlines_changed = true;
                }
            });
        }
        if ui.small_button("+ Add Layer").clicked() {
            new_outlines.push(klipp_timeline::TextOutline {
                color: [0, 0, 0, 255], width: 4.0, enabled: true,
            });
            outlines_changed = true;
        }
        if outlines_changed {
            let outline_tuples: Vec<([u8; 4], f32)> =
                new_outlines.iter().map(|o| (o.color, o.width)).collect();
            cmds.push(AppCommand::SetTitleOutlines { clip_id, outlines: outline_tuples });
        }
    }
}

/// radius. Phase-1 ships only the rectangle primitive, so the
/// editor is small. Future shape kinds (ellipse, polygon, path)
/// add more controls inside the same `match shape.kind` arm.
fn show_title_shape_editor(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    object_id: klipp_timeline::TitleObjectId,
    shape: &klipp_timeline::Shape,
    cmds: &mut Vec<AppCommand>,
) {
    ui.label(egui::RichText::new("Shape").strong().size(12.0));
    ui.add_space(6.0);

    // Fill (None / Color).
    let mut fill_enabled = shape.fill.is_some();
    let cur_fill = shape.fill.unwrap_or([60, 60, 70, 220]);
    if ui.checkbox(&mut fill_enabled, "Fill").changed() {
        cmds.push(AppCommand::SetTitleShapeFill {
            clip_id,
            object_id: object_id.0,
            color: if fill_enabled { Some(cur_fill) } else { None },
        });
    }
    if fill_enabled {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Colour").size(11.0).color(theme.text_secondary));
            let mut c = egui::Color32::from_rgba_premultiplied(
                cur_fill[0], cur_fill[1], cur_fill[2], cur_fill[3],
            );
            if ui.color_edit_button_srgba(&mut c).changed() {
                cmds.push(AppCommand::SetTitleShapeFill {
                    clip_id,
                    object_id: object_id.0,
                    color: Some([c.r(), c.g(), c.b(), c.a()]),
                });
            }
        });
    }

    ui.add_space(8.0);

    // Stroke (None / Color + width).
    let mut stroke_enabled = shape.stroke.is_some();
    let (cur_stroke_color, cur_stroke_width) = match shape.stroke {
        Some(s) => (s.color, s.width),
        None => ([255, 255, 255, 255], 2.0),
    };
    if ui.checkbox(&mut stroke_enabled, "Stroke").changed() {
        cmds.push(AppCommand::SetTitleShapeStroke {
            clip_id,
            object_id: object_id.0,
            color: if stroke_enabled { Some(cur_stroke_color) } else { None },
            width: cur_stroke_width,
        });
    }
    if stroke_enabled {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Colour").size(11.0).color(theme.text_secondary));
            let mut c = egui::Color32::from_rgba_premultiplied(
                cur_stroke_color[0], cur_stroke_color[1], cur_stroke_color[2], cur_stroke_color[3],
            );
            if ui.color_edit_button_srgba(&mut c).changed() {
                cmds.push(AppCommand::SetTitleShapeStroke {
                    clip_id,
                    object_id: object_id.0,
                    color: Some([c.r(), c.g(), c.b(), c.a()]),
                    width: cur_stroke_width,
                });
            }
        });
        let mut w = cur_stroke_width;
        if ui.add(egui::Slider::new(&mut w, 0.0..=20.0)
            .text("Width").suffix(" px"))
            .changed()
        {
            cmds.push(AppCommand::SetTitleShapeStroke {
                clip_id,
                object_id: object_id.0,
                color: Some(cur_stroke_color),
                width: w,
            });
        }
    }

    ui.add_space(8.0);

    // Rectangle-specific: corner radius. Future shape kinds add
    // their own per-kind controls in the matching arm.
    match shape.kind {
        klipp_timeline::ShapeKind::Rectangle { corner_radius } => {
            let mut r = corner_radius;
            if ui.add(egui::Slider::new(&mut r, 0.0..=200.0)
                .text("Corner radius").suffix(" px @ 1080p"))
                .changed()
            {
                cmds.push(AppCommand::SetTitleShapeCornerRadius {
                    clip_id,
                    object_id: object_id.0,
                    radius: r,
                });
            }
        }
    }
}

/// Draw the Layers panel: a Photoshop-style stack of objects, with
/// visibility / lock toggles, click-to-select, and a delete button.
/// Plus an "Add Text" / "Add Shape" footer to spawn new objects.
/// The list paints back-to-front so the topmost row in the UI is
/// the topmost rendered layer (matches the convention from FCP /
/// AE / Premiere Essential Graphics).
fn show_title_layers_panel(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    td: &klipp_timeline::TitleData,
    state: &AppState,
    cmds: &mut Vec<AppCommand>,
) {
    let selected_oid = state.selected_title_object
        .filter(|(c, _)| *c == clip_id)
        .map(|(_, oid)| oid);

    // Iterate top-down (last in objects = front layer = top of list).
    let total = td.objects.len();
    egui::Frame::none()
        .fill(theme.bg_panel)
        .stroke(egui::Stroke::new(1.0_f32, theme.border))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(6.0, 6.0))
        .show(ui, |ui| {
            if total == 0 {
                ui.label(
                    egui::RichText::new("No layers — add one below.")
                        .size(11.0)
                        .color(theme.text_dim),
                );
            }
            for (rev_idx, obj) in td.objects.iter().enumerate().rev() {
                show_title_layer_row(ui, theme, clip_id, td, obj, rev_idx, total, selected_oid, cmds);
            }
        });

    // ── Add buttons ──
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new("\u{002B} Text").size(11.0).color(theme.text_primary),
                )
                .fill(theme.bg_raised),
            )
            .on_hover_text("Add a new TextBox layer")
            .clicked()
        {
            cmds.push(AppCommand::AddTitleObject { clip_id, kind: 0 });
        }
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new("\u{002B} Shape").size(11.0).color(theme.text_primary),
                )
                .fill(theme.bg_raised),
            )
            .on_hover_text("Add a new Shape layer (rectangle)")
            .clicked()
        {
            cmds.push(AppCommand::AddTitleObject { clip_id, kind: 1 });
        }
    });
}

/// Render one layer row inside the layers panel. Order:
/// `[eye] [lock] [type-badge] {name}    {Front|Mid|Back|N/total} [^] [×]`.
/// Single ^ steps one position toward Front; right-click on the row body
/// opens a 4-way reorder context menu (Bring to Front, Bring Forward,
/// Send Backward, Send to Back). Selection styling is an accent outline,
/// not a fill, to leave the row's icon foregrounds legible.
fn show_title_layer_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    td: &klipp_timeline::TitleData,
    obj: &klipp_timeline::TitleObject,
    rev_idx: usize,
    total: usize,
    selected_oid: Option<klipp_timeline::TitleObjectId>,
    cmds: &mut Vec<AppCommand>,
) {
    use klipp_timeline::TitleObjectContent;

    let is_selected = Some(obj.id) == selected_oid;
    let row_h = 32.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        egui::Sense::click(),
    );

    if is_selected {
        ui.painter().rect(
            rect.shrink(1.0),
            egui::Rounding::same(5.0),
            theme.accent_subtle,
            egui::Stroke::new(1.5_f32, theme.accent),
        );
    } else if resp.hovered() {
        ui.painter()
            .rect_filled(rect, egui::Rounding::same(5.0), theme.bg_hover);
    }

    if resp.clicked() {
        cmds.push(AppCommand::SelectTitleObject {
            clip_id,
            object_id: Some(obj.id.0),
        });
    }

    // Right-click context menu: 4-way z-order.
    let snapshot_order: Vec<u32> = td.objects.iter().map(|o| o.id.0).collect();
    let pos_in_vec = snapshot_order
        .iter()
        .position(|&id| id == obj.id.0)
        .unwrap_or(0);
    let stack_len = snapshot_order.len();
    resp.context_menu(|ui| {
        let can_forward = pos_in_vec + 1 < stack_len;
        let can_backward = pos_in_vec > 0;
        if ui.add_enabled(can_forward, egui::Button::new("Bring to Front")).clicked() {
            let mut order = snapshot_order.clone();
            let layer = order.remove(pos_in_vec);
            order.push(layer);
            cmds.push(AppCommand::ReorderTitleObjects { clip_id, new_order: order });
            ui.close_menu();
        }
        if ui.add_enabled(can_forward, egui::Button::new("Bring Forward")).clicked() {
            let mut order = snapshot_order.clone();
            order.swap(pos_in_vec, pos_in_vec + 1);
            cmds.push(AppCommand::ReorderTitleObjects { clip_id, new_order: order });
            ui.close_menu();
        }
        if ui.add_enabled(can_backward, egui::Button::new("Send Backward")).clicked() {
            let mut order = snapshot_order.clone();
            order.swap(pos_in_vec, pos_in_vec - 1);
            cmds.push(AppCommand::ReorderTitleObjects { clip_id, new_order: order });
            ui.close_menu();
        }
        if ui.add_enabled(can_backward, egui::Button::new("Send to Back")).clicked() {
            let mut order = snapshot_order.clone();
            let layer = order.remove(pos_in_vec);
            order.insert(0, layer);
            cmds.push(AppCommand::ReorderTitleObjects { clip_id, new_order: order });
            ui.close_menu();
        }
    });

    // Layout cursor — left-side icons.
    let mut x = rect.min.x + 8.0;
    let icon_size = 16.0;
    let icon_y = rect.center().y - icon_size * 0.5;

    // Eye toggle.
    let eye_rect = egui::Rect::from_min_size(egui::pos2(x, icon_y), egui::vec2(icon_size, icon_size));
    let eye_resp = ui.interact(eye_rect, ui.id().with(("title_layer_eye", obj.id.0)), egui::Sense::click());
    let eye_icon = if obj.visible { Icon::Eye } else { Icon::EyeSlash };
    let eye_tint = if obj.visible { theme.text_secondary } else { theme.text_dim };
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(eye_rect), |ui| {
        ui.add(eye_icon.image(icon_size).tint(eye_tint));
    });
    if eye_resp.clicked() {
        cmds.push(AppCommand::SetTitleObjectVisible { clip_id, object_id: obj.id.0, visible: !obj.visible });
    }
    x += icon_size + 6.0;

    // Lock toggle.
    let lock_rect = egui::Rect::from_min_size(egui::pos2(x, icon_y), egui::vec2(icon_size, icon_size));
    let lock_resp = ui.interact(lock_rect, ui.id().with(("title_layer_lock", obj.id.0)), egui::Sense::click());
    let lock_icon = if obj.locked { Icon::Lock } else { Icon::LockOpen };
    let lock_tint = if obj.locked { theme.accent } else { theme.text_dim };
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(lock_rect), |ui| {
        ui.add(lock_icon.image(icon_size).tint(lock_tint));
    });
    if lock_resp.clicked() {
        cmds.push(AppCommand::SetTitleObjectLocked { clip_id, object_id: obj.id.0, locked: !obj.locked });
    }
    x += icon_size + 8.0;

    // Type badge — small filled square with kind letter.
    let badge_size = 20.0;
    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(x, rect.center().y - badge_size * 0.5),
        egui::vec2(badge_size, badge_size),
    );
    let (badge_fill, badge_text_color, badge_letter) = match &obj.content {
        TitleObjectContent::TextBox(_) => (theme.accent, theme.text_on_accent, "T"),
        TitleObjectContent::Shape(_) => (theme.bg_raised, theme.text_primary, "S"),
    };
    ui.painter().rect_filled(badge_rect, egui::Rounding::same(4.0), badge_fill);
    ui.painter().text(
        badge_rect.center(),
        egui::Align2::CENTER_CENTER,
        badge_letter,
        egui::FontId::proportional(12.0),
        badge_text_color,
    );
    x += badge_size + 8.0;

    // Display name.
    let display_name = if obj.name.is_empty() {
        match &obj.content {
            TitleObjectContent::TextBox(tb) => {
                let trimmed = tb.text.lines().next().unwrap_or("").trim();
                if trimmed.is_empty() {
                    format!("Text {}", obj.id.0 + 1)
                } else if trimmed.chars().count() > 24 {
                    format!("{}\u{2026}", trimmed.chars().take(22).collect::<String>())
                } else {
                    trimmed.to_string()
                }
            }
            TitleObjectContent::Shape(_) => format!("Shape {}", obj.id.0 + 1),
        }
    } else {
        obj.name.clone()
    };
    let label_color = if !obj.visible {
        theme.text_dim
    } else if is_selected {
        theme.text_primary
    } else {
        theme.text_secondary
    };
    ui.painter().text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        display_name,
        egui::FontId::proportional(12.5),
        label_color,
    );

    // Right-side: stack-position label, ^ chevron (toward Front), × delete.
    let icon_btn_size = 16.0;
    let mut x_right = rect.max.x - 8.0;

    // Delete (×) — rightmost.
    let del_rect = egui::Rect::from_min_size(
        egui::pos2(x_right - icon_btn_size, rect.center().y - icon_btn_size * 0.5),
        egui::vec2(icon_btn_size, icon_btn_size),
    );
    let del_resp = ui.interact(del_rect, ui.id().with(("title_layer_del", obj.id.0)), egui::Sense::click());
    let del_tint = if del_resp.hovered() { theme.marker_error } else { theme.text_dim };
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(del_rect), |ui| {
        ui.add(Icon::X.image(icon_btn_size).tint(del_tint));
    });
    if del_resp.clicked() {
        cmds.push(AppCommand::DeleteTitleObject { clip_id, object_id: obj.id.0 });
    }
    x_right -= icon_btn_size + 6.0;

    // ^ chevron — single-step toward Front.
    let up_rect = egui::Rect::from_min_size(
        egui::pos2(x_right - icon_btn_size, rect.center().y - icon_btn_size * 0.5),
        egui::vec2(icon_btn_size, icon_btn_size),
    );
    let up_enabled = rev_idx + 1 < total;
    let up_resp = ui.interact(up_rect, ui.id().with(("title_layer_up", obj.id.0)), egui::Sense::click());
    let up_tint = if !up_enabled {
        theme.text_dim.gamma_multiply(0.4)
    } else if up_resp.hovered() {
        theme.accent
    } else {
        theme.text_secondary
    };
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(up_rect), |ui| {
        ui.add(Icon::CaretUp.image(icon_btn_size).tint(up_tint));
    });
    if up_enabled && up_resp.clicked() {
        let mut order: Vec<u32> = td.objects.iter().map(|o| o.id.0).collect();
        if let Some(pos) = order.iter().position(|&id| id == obj.id.0) {
            if pos + 1 < order.len() {
                order.swap(pos, pos + 1);
                cmds.push(AppCommand::ReorderTitleObjects { clip_id, new_order: order });
            }
        }
    }
    x_right -= icon_btn_size + 8.0;

    // Stack-position label.
    let stack_label = if total <= 3 {
        if rev_idx + 1 == total {
            "Front".to_string()
        } else if rev_idx == 0 {
            "Back".to_string()
        } else {
            "Mid".to_string()
        }
    } else {
        format!("{}/{}", rev_idx + 1, total)
    };
    let stack_galley = ui.painter().layout_no_wrap(
        stack_label,
        egui::FontId::proportional(11.0),
        theme.text_dim,
    );
    ui.painter().galley(
        egui::pos2(
            x_right - stack_galley.size().x,
            rect.center().y - stack_galley.size().y * 0.5,
        ),
        stack_galley,
        theme.text_dim,
    );
}

fn show_title_editor(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &klipp_timeline::Clip,
    state: &AppState,
    cmds: &mut Vec<AppCommand>,
) {
    let clip_id = clip.id;
    let td = match &clip.kind {
        klipp_timeline::ClipKind::Title(td) => td,
        _ => return,
    };
    let clip_time_secs = (state.playhead_smooth_secs - clip.timeline_in.to_secs_f64()).max(0.0);

    // ── Project Asset banner ──
    // Only when the clip is linked. Shows the asset name + how
    // many other instances share it + a one-click Detach. The
    // banner uses the accent_subtle / accent border treatment so
    // it reads as "informational, not actionable" — the actual
    // detach action is on the explicit button.
    if let Some(asset_id) = clip.linked_title_asset_id {
        if let Some(asset) = state.title_assets.get(&asset_id) {
            // Count linked instances across all sequences.
            let mut instance_count = 0usize;
            for seq in &state.sequences {
                for track in &seq.tracks {
                    for c in &track.clips {
                        if c.linked_title_asset_id == Some(asset_id) {
                            instance_count += 1;
                        }
                    }
                }
            }
            egui::Frame::none()
                .fill(theme.accent_subtle)
                .stroke(egui::Stroke::new(1.0_f32, theme.accent))
                .rounding(4.0)
                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                .outer_margin(egui::Margin::symmetric(0.0, 4.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let icon_size = 12.0;
                        let img = crate::icons::Icon::Star.image(icon_size).tint(theme.accent);
                        ui.add(img);
                        ui.vertical(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Linked to \u{201C}{}\u{201D}",
                                    asset.display_name
                                ))
                                .size(11.0)
                                .strong()
                                .color(theme.text_primary),
                            );
                            let footer = if instance_count > 1 {
                                format!(
                                    "Edits sync across {instance_count} instances"
                                )
                            } else {
                                "Edits will sync to all linked instances".to_string()
                            };
                            ui.label(
                                egui::RichText::new(footer)
                                    .size(9.5)
                                    .color(theme.text_secondary),
                            );
                        });
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui
                                    .small_button("Detach")
                                    .on_hover_text(
                                        "Stop syncing this clip with the asset",
                                    )
                                    .clicked()
                                {
                                    cmds.push(AppCommand::DetachTitleAsset { clip_id });
                                }
                            },
                        );
                    });
                });
            ui.add_space(4.0);
        }
    }

    // ── Preset dropdown ──
    // Quick-style picker. Selecting a preset flattens its mutations
    // into the active TextBox via ApplyTitlePreset; there's no
    // "currently selected" preset on the data, so the combobox
    // resets to "None" after each apply. Adding a new preset =
    // extend PRESET_REGISTRY here AND the matching arm in
    // apply_title_preset (app.rs).
    {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("PRESET")
                    .size(10.0)
                    .color(theme.text_dim),
            );
            ui.add_space(8.0);
            let combo_w = ui.available_width() - 8.0;
            egui::ComboBox::from_id_salt("title_preset_dropdown")
                .selected_text("None")
                .width(combo_w)
                .show_ui(ui, |ui| {
                    for (id, label) in PRESET_REGISTRY {
                        if *id == 0 {
                            // "None" sentinel — keeps the closed combobox
                            // showing a stable label without dispatching
                            // anything when the user reopens it.
                            let _ = ui.selectable_label(true, *label);
                        } else if ui.selectable_label(false, *label).clicked() {
                            cmds.push(AppCommand::ApplyTitlePreset {
                                clip_id,
                                preset_id: *id,
                            });
                        }
                    }
                });
        });
        ui.add_space(8.0);
    }

    // ── Layers panel ──
    // Shows every object in the title (back → front), with
    // visibility / lock toggles, click-to-select, delete. This is
    // the primary entry point to multi-object titles — the user
    // adds new TextBox / Shape layers here and switches between
    // them. Selection feeds `state.selected_title_object`, which
    // both the inspector and the on-canvas editor share.
    ui.label(egui::RichText::new("Layers").strong().size(11.0));
    ui.add_space(4.0);
    show_title_layers_panel(ui, theme, clip_id, td, state, cmds);
    ui.separator();
    ui.add_space(4.0);

    // ── Polymorphic pinned core ──
    // Branch on the selected layer's kind. Shape layers get the existing
    // shape editor + the shared Position block and bail before any
    // text-only sections; TextBox layers fall through to the new pinned
    // core (textarea / font+size / B/I/U/S + L/C/R / colour / position)
    // and the legacy compat strip for selection-aware tools.
    let target_oid = state
        .selected_title_object
        .filter(|(c, _)| *c == clip_id)
        .map(|(_, oid)| oid);
    let kind_ref = target_oid.and_then(|oid| td.object(oid)).map(|o| &o.content);
    if let Some(klipp_timeline::TitleObjectContent::Shape(shape)) = kind_ref {
        if let Some(oid) = target_oid {
            show_title_shape_editor(ui, theme, clip_id, oid, shape, cmds);
            ui.add_space(8.0);
            show_title_position_block(ui, theme, clip_id, td, state, cmds, clip_time_secs);
        }
        return;
    }
    let tb_target: Option<&klipp_timeline::TextBox> = match kind_ref {
        Some(klipp_timeline::TitleObjectContent::TextBox(tb)) => Some(tb),
        _ => td.primary_text(),
    };
    if let Some(tb) = tb_target {
        show_title_pinned_core_textbox(ui, theme, clip_id, td, state, cmds, tb, clip_time_secs);
    }
    ui.add_space(8.0);

    // ── Effects group ──
    if let Some(tb) = tb_target {
        show_title_effects_group(ui, theme, clip_id, tb, cmds, target_oid, clip_time_secs);
        ui.add_space(8.0);
    }

    {
        let style_runs_n = td.style_runs().len() as u32;
        let _grp_count = (td.tracking() != 0.0) as u32 + ((td.line_spacing() - 1.0).abs() > 1e-6) as u32 + (td.baseline_shift() != 0.0) as u32 + (td.case_transform() != klipp_timeline::TextCase::None) as u32 + (td.drop_cap_scale() >= 1.0) as u32 + td.primary_text().map_or(0u32, |tb| { (tb.columns != 1) as u32 + tb.vertical_layout as u32 + ((tb.text_direction == klipp_timeline::TextDirection::Rtl)) as u32 + tb.underline as u32 + tb.strikethrough as u32 }) + (style_runs_n > 0) as u32;
        let status = crate::widgets::inspector_group::GroupStatus {
            count: if _grp_count > 0 { Some(_grp_count) } else { None },
            active: td.tracking() != 0.0 || (td.line_spacing() - 1.0).abs() > 1e-6 || td.baseline_shift() != 0.0 || td.case_transform() != klipp_timeline::TextCase::None || td.drop_cap_scale() >= 1.0 || td.primary_text().is_some_and(|tb| tb.columns != 1 || tb.vertical_layout || tb.text_direction == klipp_timeline::TextDirection::Rtl || tb.underline || tb.strikethrough) || style_runs_n > 0,
        };
        if title_group_is_open(ui, theme, clip_id, "Typography", status) {
            ui.add_space(6.0);
    // Typography
    let mut tracking = td.tracking();
    if ui.add(egui::DragValue::new(&mut tracking).range(-10.0..=50.0).speed(0.2).suffix(" px").prefix("Tracking: ").fixed_decimals(1)).changed() {
        cmds.push(AppCommand::SetTitleTracking { clip_id, tracking });
    }
    let mut line_sp = td.line_spacing();
    if ui.add(egui::DragValue::new(&mut line_sp).range(0.5..=4.0).speed(0.05).suffix("x").prefix("Line Spacing: ").fixed_decimals(2)).changed() {
        cmds.push(AppCommand::SetTitleLineSpacing { clip_id, line_spacing: line_sp });
    }
    let mut write_on = td.write_on();
    if ui.add(egui::Slider::new(&mut write_on, 0.0..=1.0).text("Write-On").step_by(0.01)).changed() {
        cmds.push(AppCommand::SetTitleWriteOn { clip_id, write_on });
    }
    let mut baseline = td.baseline_shift();
    if ui.add(
        egui::DragValue::new(&mut baseline)
            .range(-200.0..=200.0)
            .speed(0.5)
            .suffix(" px")
            .prefix("Baseline shift: ")
            .fixed_decimals(0),
    ).changed() {
        cmds.push(AppCommand::SetTitleBaselineShift { clip_id, shift_px: baseline });
    }
    ui.horizontal(|ui| {
        // Columns: 1..=8 with reset.
        let mut cols = td.primary_text().map_or(1u8, |tb| tb.columns) as i32;
        if ui.add(
            egui::DragValue::new(&mut cols)
                .range(1..=8)
                .speed(0.05)
                .prefix("Columns: "),
        ).changed() {
            cmds.push(AppCommand::SetTitleColumns { clip_id, columns: cols.clamp(1, 8) as u8 });
        }
        let mut vertical = td.primary_text().map_or(false, |tb| tb.vertical_layout);
        if ui.checkbox(&mut vertical, "Vertical").changed() {
            cmds.push(AppCommand::SetTitleVerticalLayout { clip_id, vertical });
        }
        let dir_idx = match td.primary_text().map_or(klipp_timeline::TextDirection::Ltr, |tb| tb.text_direction) {
            klipp_timeline::TextDirection::Ltr => 0u8,
            klipp_timeline::TextDirection::Rtl => 1u8,
        };
        let mut sel = dir_idx;
        egui::ComboBox::from_id_salt("title_text_direction")
            .selected_text(if dir_idx == 1 { "RTL" } else { "LTR" })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut sel, 0, "LTR");
                ui.selectable_value(&mut sel, 1, "RTL");
            });
        if sel != dir_idx {
            cmds.push(AppCommand::SetTitleDirection { clip_id, direction: sel });
        }
    });
    ui.add_space(4.0);
    {
        // Drop cap scale: 0 = off, >=1 enlarges first character.
        // Surface as a normal numeric drag clamped to a useful
        // range; the handler folds anything below 1.0 back to 0.0
        // so an off→on→off toggle is a single drag.
        let mut dc = td.drop_cap_scale();
        let resp = ui.add(
            egui::DragValue::new(&mut dc)
                .range(0.0..=8.0)
                .speed(0.05)
                .fixed_decimals(2)
                .prefix("Drop cap: ")
                .custom_formatter(|n, _| {
                    if n < 1.0 { "off".to_string() } else { format!("{n:.2}\u{00d7}") }
                })
                .custom_parser(|s| {
                    let t = s.trim().to_lowercase();
                    if t == "off" || t.is_empty() { Some(0.0) }
                    else { t.trim_end_matches('×').trim_end_matches("x").trim().parse::<f64>().ok() }
                }),
        );
        if resp.changed() {
            cmds.push(AppCommand::SetTitleDropCap { clip_id, scale: dc });
        }
    }
    ui.horizontal(|ui| {
        let mut underline = td.underline();
        if ui.checkbox(&mut underline, "Underline").changed() {
            cmds.push(AppCommand::SetTitleUnderline { clip_id, underline });
        }
        let mut strikethrough = td.strikethrough();
        if ui.checkbox(&mut strikethrough, "Strikethrough").changed() {
            cmds.push(AppCommand::SetTitleStrikethrough { clip_id, strikethrough });
        }
    });
    {
        let current_case = td.case_transform();
        let case_idx: u8 = match current_case {
            klipp_timeline::TextCase::None => 0,
            klipp_timeline::TextCase::Uppercase => 1,
            klipp_timeline::TextCase::Lowercase => 2,
            klipp_timeline::TextCase::SmallCaps => 3,
        };
        let case_label = match current_case {
            klipp_timeline::TextCase::None => "None",
            klipp_timeline::TextCase::Uppercase => "UPPERCASE",
            klipp_timeline::TextCase::Lowercase => "lowercase",
            klipp_timeline::TextCase::SmallCaps => "Small Caps",
        };
        let mut sel = case_idx;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Case:").size(11.0).color(theme.text_secondary));
            egui::ComboBox::from_id_salt("title_case_transform")
                .selected_text(case_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut sel, 0, "None");
                    ui.selectable_value(&mut sel, 1, "UPPERCASE");
                    ui.selectable_value(&mut sel, 2, "lowercase");
                    ui.selectable_value(&mut sel, 3, "Small Caps");
                });
        });
        if sel != case_idx {
            cmds.push(AppCommand::SetTitleCaseTransform { clip_id, case_kind: sel });
        }
    }
    ui.add_space(8.0);

            show_title_quick_style_subsection(ui, theme, clip_id, td, state, cmds);
            ui.add_space(8.0);
        }
    }
    ui.add_space(2.0);

    {
        let status = crate::widgets::inspector_group::GroupStatus {
            count: None,
            active: !matches!(td.background, klipp_timeline::TitleBackground::None) || td.objects.iter().any(|o| (o.opacity - 1.0).abs() > 1e-6),
        };
        if title_group_is_open(ui, theme, clip_id, "Background & Layer Opacity", status) {
            ui.add_space(6.0);
    // Per-object opacity. Targets the same object as the other
    // typography / styling commands (selected layer, falling back to
    // the first object). Independent of clip-level opacity_keyframes
    // — each layer can be individually faded.
    {
        let target_oid: Option<klipp_timeline::TitleObjectId> = state
            .selected_title_object
            .filter(|(c, _)| *c == clip_id)
            .map(|(_, oid)| oid);
        let target_obj = match target_oid {
            Some(oid) => td.object(oid),
            None => td.objects.first(),
        };
        if let Some(obj) = target_obj {
            let object_id = obj.id.0;
            let mut op = obj.opacity;
            ui.label(egui::RichText::new("Layer Opacity").strong().size(11.0));
            if ui.add(egui::Slider::new(&mut op, 0.0..=1.0).step_by(0.01)).changed() {
                cmds.push(AppCommand::SetTitleObjectOpacity { clip_id, object_id, opacity: op });
            }
            ui.add_space(8.0);
        }
    }

    // Background
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
        // Round-trip stable: convert alpha to percentage and back to ensure the slider
        // doesn't report .changed() every frame due to float rounding.
        let bg_opacity_raw = (bg_color[3] as f32 / 255.0) * 100.0;
        let mut bg_opacity = bg_opacity_raw.round();
        if ui.add(egui::Slider::new(&mut bg_opacity, 0.0..=100.0).text("Opacity").step_by(1.0).suffix("%")).changed() {
            let a = (bg_opacity / 100.0 * 255.0).round() as u8;
            if a != bg_color[3] {
                cmds.push(AppCommand::SetTitleBackground { clip_id, bg_kind, color: [bg_color[0], bg_color[1], bg_color[2], a] });
            }
        }
    }
    ui.add_space(8.0);

            ui.add_space(8.0);
        }
    }
    ui.add_space(2.0);

    {
        let status = crate::widgets::inspector_group::GroupStatus {
            count: None,
            active: !matches!(td.animation_type, klipp_timeline::TitleAnimationType::Static),
        };
        if title_group_is_open(ui, theme, clip_id, "Animation", status) {
            ui.add_space(6.0);
    // Animation
    // Renamed from "Title Animation" so the direction picker reads as
    // the canonical place to choose scroll direction — the browser
    // ships generic Credits Roll / News Ticker presets and users
    // change direction here rather than picking a per-direction
    // template. Scroll/crawl variants auto-generate
    // transform_pos_x/y_keyframes; Typewriter overrides write_on
    // per-frame at render time; Static clears any existing keyframes.
    {
        use klipp_timeline::TitleAnimationType as T;
        // Single source of truth: every variant's display label, kind
        // code, primary parameter, and (where applicable) secondary
        // parameter all flow through these helpers. Adding a new
        // variant means extending the enum, then this block in one
        // place — no risk of the combo, the spinner, and the encoder
        // drifting out of sync.
        let current_kind: u8 = match &td.animation_type {
            T::Static => 0,
            T::ScrollUp { .. } => 1,
            T::ScrollDown { .. } => 2,
            T::CrawlLeft { .. } => 3,
            T::CrawlRight { .. } => 4,
            T::Typewriter { .. } => 5,
            T::FadeIn { .. } => 6,
            T::FadeOut { .. } => 7,
            T::FadeInOut { .. } => 8,
            T::Flash { .. } => 9,
            T::Blink { .. } => 10,
            T::Pulse { .. } => 11,
            T::ZoomIn { .. } => 12,
            T::ZoomOut { .. } => 13,
            T::SlideInLeft { .. } => 14,
            T::SlideInRight { .. } => 15,
            T::SlideInTop { .. } => 16,
            T::SlideInBottom { .. } => 17,
            T::SpinIn { .. } => 18,
            T::BounceIn { .. } => 19,
            T::Wave { .. } => 20,
            T::Shake { .. } => 21,
            T::ColorCycle { .. } => 22,
            T::Marquee { .. } => 23,
            T::TypewriterOut { .. } => 24,
            T::Counter { .. } => 25,
            T::OutlineDrawOn { .. } => 26,
            T::ColorBlend { .. } => 27,
        };
        let kind_label = |k: u8| -> &'static str {
            match k {
                0 => "Static (no animation)",
                1 => "Scroll \u{2191} (bottom \u{2192} top)",
                2 => "Scroll \u{2193} (top \u{2192} bottom)",
                3 => "Crawl \u{2190} (right \u{2192} left)",
                4 => "Crawl \u{2192} (left \u{2192} right)",
                5 => "Typewriter",
                6 => "Fade In",
                7 => "Fade Out",
                8 => "Fade In + Out",
                9 => "Flash",
                10 => "Blink (custom duty)",
                11 => "Pulse (smooth)",
                12 => "Zoom In",
                13 => "Zoom Out",
                14 => "Slide In \u{2190} (from left)",
                15 => "Slide In \u{2192} (from right)",
                16 => "Slide In \u{2191} (from top)",
                17 => "Slide In \u{2193} (from bottom)",
                18 => "Spin In",
                19 => "Bounce In",
                20 => "Wave (continuous)",
                21 => "Shake (continuous)",
                22 => "Colour Cycle (continuous)",
                23 => "Marquee (continuous loop)",
                24 => "Typewriter Out (delete-on)",
                25 => "Counter (animated value)",
                26 => "Outline Draw-On",
                27 => "Colour Blend (two-colour)",
                _ => "Static (no animation)",
            }
        };
        // Sensible defaults when switching kinds. Picked to look right
        // out of the box on a typical 3-5s title clip.
        let kind_defaults = |k: u8| -> (f32, f32) {
            match k {
                1 | 2 | 3 | 4 => (0.0, 0.0),     // scroll/crawl: Auto
                5 => (15.0, 0.0),                // Typewriter chars/sec
                6 | 7 => (0.5, 0.0),             // FadeIn/Out 0.5s
                8 => (0.5, 0.0),                 // FadeInOut fade_secs
                9 => (4.0, 0.0),                 // Flash 4 Hz
                10 => (4.0, 0.3),                // Blink 4 Hz, 30% on
                11 => (1.0, 0.4),                // Pulse 1 Hz, min_op 0.4
                12 => (0.5, 1.5),                // ZoomIn 0.5s from 1.5x
                13 => (0.5, 1.5),                // ZoomOut 0.5s to 1.5x
                14 | 15 | 16 | 17 => (0.5, 0.0), // Slide 0.5s
                18 => (0.5, -180.0),             // SpinIn 0.5s, from -180°
                19 => (0.5, 0.0),                // BounceIn 0.5s
                20 => (20.0, 1.0),               // Wave 20px @ 1Hz
                21 => (10.0, 12.0),              // Shake 10px @ 12Hz
                22 => (0.5, 0.0),                // ColorCycle 0.5 Hz hue rotation
                23 => (200.0, 0.0),              // Marquee 200 px/s
                24 => (15.0, 0.0),               // TypewriterOut 15 chars/sec
                25 => (2.0, 0.0),                // Counter default duration 2s
                26 => (0.5, 0.0),                // OutlineDrawOn 0.5s
                27 => (1.0, 0.0),                // ColorBlend 1 Hz default
                _ => (0.0, 0.0),
            }
        };

        let mut selected = current_kind;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Animation:").size(11.0).color(theme.text_secondary));
            egui::ComboBox::from_id_salt("title_animation")
                .selected_text(kind_label(current_kind))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selected, 0, kind_label(0));
                    ui.label(egui::RichText::new("Continuous").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 1, kind_label(1));
                    ui.selectable_value(&mut selected, 2, kind_label(2));
                    ui.selectable_value(&mut selected, 3, kind_label(3));
                    ui.selectable_value(&mut selected, 4, kind_label(4));
                    ui.selectable_value(&mut selected, 5, kind_label(5));
                    ui.label(egui::RichText::new("Fade").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 6, kind_label(6));
                    ui.selectable_value(&mut selected, 7, kind_label(7));
                    ui.selectable_value(&mut selected, 8, kind_label(8));
                    ui.label(egui::RichText::new("Flashing").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 9, kind_label(9));
                    ui.selectable_value(&mut selected, 10, kind_label(10));
                    ui.selectable_value(&mut selected, 11, kind_label(11));
                    ui.label(egui::RichText::new("Zoom").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 12, kind_label(12));
                    ui.selectable_value(&mut selected, 13, kind_label(13));
                    ui.label(egui::RichText::new("Slide In").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 14, kind_label(14));
                    ui.selectable_value(&mut selected, 15, kind_label(15));
                    ui.selectable_value(&mut selected, 16, kind_label(16));
                    ui.selectable_value(&mut selected, 17, kind_label(17));
                    ui.label(egui::RichText::new("Pose Entrance").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 18, kind_label(18));
                    ui.selectable_value(&mut selected, 19, kind_label(19));
                    ui.label(egui::RichText::new("Continuous Motion").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 20, kind_label(20));
                    ui.selectable_value(&mut selected, 21, kind_label(21));
                    ui.label(egui::RichText::new("Colour").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 22, kind_label(22));
                    ui.label(egui::RichText::new("Looping").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 23, kind_label(23));
                    ui.label(egui::RichText::new("Reveal / Counter").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 24, kind_label(24));
                    ui.selectable_value(&mut selected, 25, kind_label(25));
                    ui.selectable_value(&mut selected, 26, kind_label(26));
                    ui.label(egui::RichText::new("Two-Colour Blend").size(10.0).color(theme.text_dim));
                    ui.selectable_value(&mut selected, 27, kind_label(27));
                });
        });
        if selected != current_kind {
            let (speed, param2) = kind_defaults(selected);
            cmds.push(AppCommand::SetTitleAnimation {
                clip_id,
                animation_kind: selected,
                speed,
                param2,
            });
        }

        // ── Per-variant parameter spinners ──
        // The Static / Typewriter / scroll-crawl variants keep their
        // existing UX. New variants get a single-purpose spinner per
        // parameter so the user can tune the effect without leaving
        // the Inspector.
        match &td.animation_type {
            T::ScrollUp { speed_px_per_sec }
            | T::ScrollDown { speed_px_per_sec }
            | T::CrawlLeft { speed_px_per_sec }
            | T::CrawlRight { speed_px_per_sec } => {
                let mut v = *speed_px_per_sec;
                ui.horizontal(|ui| {
                    ui.label("Speed:");
                    let resp = ui.add(
                        egui::DragValue::new(&mut v)
                            .range(0.0..=2000.0)
                            .speed(1.0)
                            .min_decimals(0)
                            .max_decimals(2)
                            .custom_formatter(|n, _| {
                                if n < 0.005 {
                                    "Auto".to_string()
                                } else if (n - n.round()).abs() < 1e-3 {
                                    format!("{n:.0} px/s")
                                } else {
                                    format!("{n:.2} px/s")
                                }
                            })
                            .custom_parser(|s| {
                                let t = s.trim().to_lowercase();
                                if t == "auto" || t.is_empty() {
                                    Some(0.0)
                                } else {
                                    t.trim_end_matches("px/s").trim().parse::<f64>().ok()
                                }
                            }),
                    );
                    if resp.changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: current_kind, speed: v, param2: 0.0,
                        });
                    }
                });
            }
            T::Typewriter { chars_per_sec } => {
                let mut v = *chars_per_sec;
                ui.horizontal(|ui| {
                    ui.label("Chars/sec:");
                    if ui.add(egui::DragValue::new(&mut v).range(1.0..=100.0).speed(0.5).fixed_decimals(1)).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 5, speed: v, param2: 0.0,
                        });
                    }
                });
            }
            T::TypewriterOut { chars_per_sec } => {
                let mut v = *chars_per_sec;
                ui.horizontal(|ui| {
                    ui.label("Chars/sec:");
                    if ui.add(egui::DragValue::new(&mut v).range(1.0..=100.0).speed(0.5).fixed_decimals(1)).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 24, speed: v, param2: 0.0,
                        });
                    }
                });
            }
            T::Counter { from_value, to_value, duration_secs, decimals } => {
                let mut from_v = *from_value;
                let mut to_v = *to_value;
                let mut dur = *duration_secs;
                let mut dec = *decimals as i32;
                let dispatch = |cmds: &mut Vec<AppCommand>, from_v: f64, to_v: f64, dur: f32, dec: u8| {
                    cmds.push(AppCommand::SetTitleCounterAnimation {
                        clip_id,
                        from_value: from_v,
                        to_value: to_v,
                        duration_secs: dur,
                        decimals: dec,
                    });
                };
                ui.horizontal(|ui| {
                    ui.label("From:");
                    if ui.add(egui::DragValue::new(&mut from_v).speed(1.0).max_decimals(3)).changed() {
                        dispatch(cmds, from_v, to_v, dur, dec.clamp(0, 3) as u8);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("To:");
                    if ui.add(egui::DragValue::new(&mut to_v).speed(1.0).max_decimals(3)).changed() {
                        dispatch(cmds, from_v, to_v, dur, dec.clamp(0, 3) as u8);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Duration:");
                    if ui.add(egui::DragValue::new(&mut dur).range(0.05..=60.0).speed(0.1).fixed_decimals(2).suffix(" s")).changed() {
                        dispatch(cmds, from_v, to_v, dur, dec.clamp(0, 3) as u8);
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Decimals:");
                    if ui.add(egui::DragValue::new(&mut dec).range(0..=3).speed(0.05)).changed() {
                        dispatch(cmds, from_v, to_v, dur, dec.clamp(0, 3) as u8);
                    }
                });
            }
            T::FadeIn { duration_secs } | T::FadeOut { duration_secs } | T::FadeInOut { fade_secs: duration_secs } => {
                let mut v = *duration_secs;
                ui.horizontal(|ui| {
                    ui.label("Duration:");
                    if ui.add(egui::DragValue::new(&mut v).range(0.05..=10.0).speed(0.05).fixed_decimals(2).suffix(" s")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: current_kind, speed: v, param2: 0.0,
                        });
                    }
                });
            }
            T::Flash { rate_hz } => {
                let mut v = *rate_hz;
                ui.horizontal(|ui| {
                    ui.label("Rate:");
                    if ui.add(egui::DragValue::new(&mut v).range(0.5..=20.0).speed(0.1).fixed_decimals(1).suffix(" Hz")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 9, speed: v, param2: 0.0,
                        });
                    }
                });
            }
            T::Blink { rate_hz, duty_cycle } => {
                let mut r = *rate_hz;
                let mut d = *duty_cycle;
                ui.horizontal(|ui| {
                    ui.label("Rate:");
                    if ui.add(egui::DragValue::new(&mut r).range(0.5..=20.0).speed(0.1).fixed_decimals(1).suffix(" Hz")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 10, speed: r, param2: d,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Duty cycle:");
                    if ui.add(egui::DragValue::new(&mut d).range(0.05..=0.95).speed(0.01).fixed_decimals(2)).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 10, speed: r, param2: d,
                        });
                    }
                });
            }
            T::Pulse { rate_hz, min_opacity } => {
                let mut r = *rate_hz;
                let mut m = *min_opacity;
                ui.horizontal(|ui| {
                    ui.label("Rate:");
                    if ui.add(egui::DragValue::new(&mut r).range(0.1..=10.0).speed(0.05).fixed_decimals(2).suffix(" Hz")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 11, speed: r, param2: m,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Min opacity:");
                    if ui.add(egui::DragValue::new(&mut m).range(0.0..=0.95).speed(0.01).fixed_decimals(2)).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 11, speed: r, param2: m,
                        });
                    }
                });
            }
            T::ZoomIn { duration_secs, from_scale } | T::ZoomOut { duration_secs, to_scale: from_scale } => {
                let mut dur = *duration_secs;
                let mut scale = *from_scale;
                let scale_label = if matches!(td.animation_type, T::ZoomIn { .. }) { "From scale:" } else { "To scale:" };
                ui.horizontal(|ui| {
                    ui.label("Duration:");
                    if ui.add(egui::DragValue::new(&mut dur).range(0.05..=10.0).speed(0.05).fixed_decimals(2).suffix(" s")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: current_kind, speed: dur, param2: scale,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(scale_label);
                    if ui.add(egui::DragValue::new(&mut scale).range(0.05..=8.0).speed(0.02).fixed_decimals(2).suffix("\u{00d7}")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: current_kind, speed: dur, param2: scale,
                        });
                    }
                });
            }
            T::SlideInLeft { duration_secs }
            | T::SlideInRight { duration_secs }
            | T::SlideInTop { duration_secs }
            | T::SlideInBottom { duration_secs }
            | T::BounceIn { duration_secs } => {
                let mut v = *duration_secs;
                ui.horizontal(|ui| {
                    ui.label("Duration:");
                    if ui.add(egui::DragValue::new(&mut v).range(0.05..=10.0).speed(0.05).fixed_decimals(2).suffix(" s")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: current_kind, speed: v, param2: 0.0,
                        });
                    }
                });
            }
            T::SpinIn { duration_secs, from_degrees } => {
                let mut dur = *duration_secs;
                let mut deg = *from_degrees;
                ui.horizontal(|ui| {
                    ui.label("Duration:");
                    if ui.add(egui::DragValue::new(&mut dur).range(0.05..=10.0).speed(0.05).fixed_decimals(2).suffix(" s")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 18, speed: dur, param2: deg,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("From angle:");
                    if ui.add(egui::DragValue::new(&mut deg).range(-720.0..=720.0).speed(2.0).fixed_decimals(0).suffix("\u{00b0}")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 18, speed: dur, param2: deg,
                        });
                    }
                });
            }
            T::OutlineDrawOn { duration_secs } => {
                let mut v = *duration_secs;
                ui.horizontal(|ui| {
                    ui.label("Duration:");
                    if ui.add(egui::DragValue::new(&mut v).range(0.05..=10.0).speed(0.05).fixed_decimals(2).suffix(" s")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 26, speed: v, param2: 0.0,
                        });
                    }
                });
            }
            T::ColorBlend { color_a, color_b, rate_hz } => {
                let mut a = egui::Color32::from_rgba_unmultiplied(color_a[0], color_a[1], color_a[2], color_a[3]);
                let mut b = egui::Color32::from_rgba_unmultiplied(color_b[0], color_b[1], color_b[2], color_b[3]);
                let mut rate = *rate_hz;
                let dispatch = |cmds: &mut Vec<AppCommand>, a: egui::Color32, b: egui::Color32, rate: f32| {
                    cmds.push(AppCommand::SetTitleColorBlend {
                        clip_id,
                        color_a: [a.r(), a.g(), a.b(), a.a()],
                        color_b: [b.r(), b.g(), b.b(), b.a()],
                        rate_hz: rate,
                    });
                };
                ui.horizontal(|ui| {
                    ui.label("Colour A:");
                    if ui.color_edit_button_srgba(&mut a).changed() { dispatch(cmds, a, b, rate); }
                });
                ui.horizontal(|ui| {
                    ui.label("Colour B:");
                    if ui.color_edit_button_srgba(&mut b).changed() { dispatch(cmds, a, b, rate); }
                });
                ui.horizontal(|ui| {
                    ui.label("Rate:");
                    if ui.add(egui::DragValue::new(&mut rate).range(0.05..=10.0).speed(0.05).fixed_decimals(2).suffix(" Hz")).changed() {
                        dispatch(cmds, a, b, rate);
                    }
                });
            }
            T::Marquee { speed_px_per_sec } => {
                let mut v = *speed_px_per_sec;
                ui.horizontal(|ui| {
                    ui.label("Speed:");
                    let resp = ui.add(
                        egui::DragValue::new(&mut v)
                            .range(-2000.0..=2000.0)
                            .speed(2.0)
                            .min_decimals(0)
                            .max_decimals(0)
                            .suffix(" px/s"),
                    );
                    if resp.changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 23, speed: v, param2: 0.0,
                        });
                    }
                });
                ui.label(
                    egui::RichText::new("Positive = right \u{2192} left, negative = left \u{2192} right")
                        .size(10.0)
                        .color(theme.text_dim),
                );
            }
            T::ColorCycle { rate_hz } => {
                let mut v = *rate_hz;
                ui.horizontal(|ui| {
                    ui.label("Rate:");
                    if ui.add(egui::DragValue::new(&mut v).range(0.05..=10.0).speed(0.05).fixed_decimals(2).suffix(" Hz")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: 22, speed: v, param2: 0.0,
                        });
                    }
                });
            }
            T::Wave { amplitude_px, rate_hz } | T::Shake { amplitude_px, rate_hz } => {
                let mut amp = *amplitude_px;
                let mut rate = *rate_hz;
                ui.horizontal(|ui| {
                    ui.label("Amplitude:");
                    if ui.add(egui::DragValue::new(&mut amp).range(0.0..=200.0).speed(0.5).fixed_decimals(1).suffix(" px")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: current_kind, speed: amp, param2: rate,
                        });
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Rate:");
                    if ui.add(egui::DragValue::new(&mut rate).range(0.1..=30.0).speed(0.1).fixed_decimals(2).suffix(" Hz")).changed() {
                        cmds.push(AppCommand::SetTitleAnimation {
                            clip_id, animation_kind: current_kind, speed: amp, param2: rate,
                        });
                    }
                });
            }
            T::Static => {}
        }
    }
    ui.add_space(8.0);

    // ── Motion Blur ──
    ui.label(egui::RichText::new("Motion Blur").strong().size(11.0));
    {
        let mut enabled = td.motion_blur().is_some();
        if ui.checkbox(&mut enabled, "Enable").changed() {
            cmds.push(AppCommand::SetTitleMotionBlur {
                clip_id,
                enabled,
                shutter_angle: 180.0,
                samples: 8,
            });
        }
        if let Some(mb) = &td.motion_blur() {
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
            ui.add_space(8.0);
        }
    }
    ui.add_space(2.0);

    {
        let status = crate::widgets::inspector_group::GroupStatus {
            count: None,
            active: td.extrusion().is_some_and(|e| e.enabled) || td.primary_text().is_some_and(|tb| tb.chromatic_aberration_px != 0.0 || tb.glitch_intensity != 0.0 || tb.motion_blur.is_some_and(|m| m.enabled)) || td.objects.iter().any(|o| o.transform.rotation != 0.0),
        };
        if title_group_is_open(ui, theme, clip_id, "3D & Distortion", status) {
            ui.add_space(6.0);
    // 3D Extrusion (pseudo-depth)
    let has_ext = td.extrusion().is_some();
    let ext = td.extrusion().unwrap_or(klipp_timeline::TextExtrusion {
        depth: 10.0,
        angle: 135.0,
        face_color: [0, 0, 0, 0],
        side_color: [80, 60, 40, 255],
        lighting: 0.5,
     enabled: true });
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

    {
        // Chromatic aberration: 0 = off, sub-pixel = silent, max ~40px.
        let mut ca = td.primary_text().map_or(0.0, |tb| tb.chromatic_aberration_px);
        let resp = ui.add(
            egui::DragValue::new(&mut ca)
                .range(0.0..=40.0)
                .speed(0.1)
                .fixed_decimals(1)
                .prefix("Chromatic aberration: ")
                .suffix(" px"),
        );
        if resp.changed() {
            cmds.push(AppCommand::SetTitleChromaticAberration { clip_id, offset_px: ca });
        }
    }
    {
        // Glitch intensity 0..1.
        let mut g = td.primary_text().map_or(0.0, |tb| tb.glitch_intensity);
        let resp = ui.add(
            egui::Slider::new(&mut g, 0.0..=1.0)
                .step_by(0.01)
                .text("Glitch"),
        );
        if resp.changed() {
            cmds.push(AppCommand::SetTitleGlitch { clip_id, intensity: g });
        }
    }
            ui.add_space(8.0);
        }
    }
    ui.add_space(2.0);

    {
        let status = crate::widgets::inspector_group::GroupStatus {
            count: None,
            active: td.text_path().is_some(),
        };
        if title_group_is_open(ui, theme, clip_id, "Text Path", status) {
            ui.add_space(6.0);
    // Text Path
    {
        let path_idx: usize = match &td.text_path() {
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
        if let Some(path) = &td.text_path() {
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

            ui.add_space(8.0);
        }
    }
    ui.add_space(2.0);

    {
        let status = crate::widgets::inspector_group::GroupStatus {
            count: None,
            active: td.char_animation().is_some_and(|c| c.enabled),
        };
        if title_group_is_open(ui, theme, clip_id, "Character Animation", status) {
            ui.add_space(6.0);
    // Character Animation
    {
        let mut enabled = td.char_animation().is_some();
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
        if let Some(anim) = &td.char_animation() {
            let mut stagger = anim.stagger;
            let mut wave_amp = anim.wave_amplitude;
            let mut wave_spd = anim.wave_speed;
            let entrance_idx: usize = match anim.entrance {
                klipp_timeline::CharEntrance::None => 0,
                klipp_timeline::CharEntrance::FadeIn => 1,
                klipp_timeline::CharEntrance::SlideUp => 2,
                klipp_timeline::CharEntrance::SlideDown => 3,
                klipp_timeline::CharEntrance::SlideLeft => 4,
                klipp_timeline::CharEntrance::SlideRight => 5,
                klipp_timeline::CharEntrance::ScaleUp => 6,
                klipp_timeline::CharEntrance::ScaleDown => 7,
                klipp_timeline::CharEntrance::Pop => 8,
                klipp_timeline::CharEntrance::BlurIn => 9,
            };
            let entrance_labels = [
                "None", "Fade In", "Slide Up", "Slide Down", "Slide Left", "Slide Right",
                "Scale Up", "Scale Down", "Pop", "Blur In",
            ];
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

            ui.add_space(8.0);
        }
    }
    ui.add_space(2.0);

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
    state: &crate::state::AppState,
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
            egui::Stroke::new(1.0_f32, theme.border),
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
                        .color(theme.marker_error)
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

    // ── Handle shortfall warning ──
    let seq = state.sequence();
    if let Some((ti_a, ci_a)) = seq.find_clip(transition.clip_a_id) {
        if let Some((ti_b, ci_b)) = seq.find_clip(transition.clip_b_id) {
            let clip_a = &seq.tracks[ti_a].clips[ci_a];
            let clip_b = &seq.tracks[ti_b].clips[ci_b];
            let ha = klipp_validation::handles::compute_handle_availability(
                transition, clip_a, clip_b, &state.media_pool,
            );
            let warn_color = theme.marker_error;
            let amber_color = theme.accent;
            let rate = transition.duration.rate.max(1) as f64;

            if ha.has_shortfall() || ha.has_unknown() {
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                if ha.has_shortfall() {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new("\u{26a0} Handle shortfall")
                                .color(warn_color)
                                .size(11.0)
                                .strong(),
                        );
                    });
                    ui.add_space(4.0);

                    for (label, needed, avail_opt, shortfall) in [
                        ("Clip A (left)", ha.clip_a_needed, ha.clip_a_available, ha.shortfall_a()),
                        ("Clip B (right)", ha.clip_b_needed, ha.clip_b_available, ha.shortfall_b()),
                    ] {
                        if shortfall > 0 {
                            if let Some(avail) = avail_opt {
                                let ms = (shortfall as f64 / rate * 1000.0).round() as i64;
                                ui.label(
                                    egui::RichText::new(label)
                                        .color(theme.text_secondary)
                                        .size(10.0),
                                );
                                ui.horizontal(|ui| {
                                    ui.add_space(8.0);
                                    ui.label(egui::RichText::new(format!(
                                        "Needed: {}  Available: {}  Short by: {} ({} ms)",
                                        needed, avail, shortfall, ms
                                    )).color(warn_color).size(10.0));
                                });
                                ui.add_space(2.0);
                            }
                        }
                    }

                    ui.add_space(4.0);
                    let fix_target = if ha.shortfall_a() > 0 && ha.shortfall_b() > 0 {
                        "both clips"
                    } else if ha.shortfall_a() > 0 {
                        "clip A"
                    } else {
                        "clip B"
                    };
                    ui.label(
                        egui::RichText::new(format!(
                            "Fix: Trim {} later, shorten this transition, or accept freeze-frame.",
                            fix_target
                        ))
                        .color(theme.text_dim)
                        .size(9.0)
                        .italics(),
                    );
                }

                // Unknown availability (non-media clips)
                for (label, avail) in [
                    ("Clip A (left)", ha.clip_a_available),
                    ("Clip B (right)", ha.clip_b_available),
                ] {
                    if avail.is_none() {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}: handle availability unknown (not a media clip)",
                                    label
                                ))
                                .color(amber_color)
                                .size(9.0)
                                .italics(),
                            );
                        });
                    }
                }
            }
        }
    }
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

fn show_generator_editor(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &klipp_timeline::Clip,
    cmds: &mut Vec<AppCommand>,
) {
    let gd = match &clip.kind {
        klipp_timeline::ClipKind::Generator(gd) => gd.clone(),
        _ => return,
    };
    let clip_id = clip.id;

    ui.add_space(8.0);
    ui.label(egui::RichText::new("Generator").strong().size(12.0));
    ui.add_space(6.0);

    // Clip-level opacity (applies to the entire generator output)
    {
        let mut opacity_pct = (crate::inspector::video_tab::get_opacity(clip) * 100.0).round();
        if ui.add(egui::Slider::new(&mut opacity_pct, 0.0..=100.0).text("Opacity").step_by(1.0).suffix("%")).changed() {
            let new_val = opacity_pct / 100.0;
            crate::inspector::video_tab::emit_opacity(cmds, clip, clip_id, new_val);
        }
    }
    ui.add_space(6.0);

    match gd {
        klipp_core::GeneratorData::SolidColor { color } => {
            ui.label(egui::RichText::new("Solid Color").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Color").strong().size(11.0));
                let mut c = egui::Color32::from_rgba_premultiplied(
                    color[0], color[1], color[2], color[3],
                );
                if ui.color_edit_button_srgba(&mut c).changed() {
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::SolidColor {
                            color: [c.r(), c.g(), c.b(), c.a()],
                        },
                    });
                }
            });
        }

        klipp_core::GeneratorData::Gradient { top, bottom, angle_deg, four_point } => {
            ui.label(egui::RichText::new("Gradient").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);

            // Mode toggle: 2-point (angled) vs 4-point (corner).
            let mut is_four_point = four_point.is_some();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Mode").strong().size(11.0));
                if ui
                    .selectable_label(!is_four_point, "2-point")
                    .clicked()
                    && is_four_point
                {
                    is_four_point = false;
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::Gradient {
                            top,
                            bottom,
                            angle_deg,
                            four_point: None,
                        },
                    });
                }
                if ui
                    .selectable_label(is_four_point, "4-point")
                    .clicked()
                    && !is_four_point
                {
                    // Seed corners from the current 2-point pair so
                    // the mode swap is visually continuous: top
                    // colour → top corners, bottom colour → bottom
                    // corners. The user can then tune each corner
                    // independently.
                    is_four_point = true;
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::Gradient {
                            top,
                            bottom,
                            angle_deg,
                            four_point: Some(klipp_core::FourPointGradient {
                                top_left: top,
                                top_right: top,
                                bottom_left: bottom,
                                bottom_right: bottom,
                            }),
                        },
                    });
                }
            });
            ui.add_space(6.0);

            if let Some(corners) = four_point {
                // 2×2 colour-picker grid mirrors the visual frame so
                // "top-left" actually edits the top-left corner.
                ui.horizontal(|ui| {
                    let mut tl = egui::Color32::from_rgba_premultiplied(
                        corners.top_left[0], corners.top_left[1],
                        corners.top_left[2], corners.top_left[3],
                    );
                    let mut tr = egui::Color32::from_rgba_premultiplied(
                        corners.top_right[0], corners.top_right[1],
                        corners.top_right[2], corners.top_right[3],
                    );
                    if ui.color_edit_button_srgba(&mut tl).changed() {
                        let mut new_corners = corners;
                        new_corners.top_left = [tl.r(), tl.g(), tl.b(), tl.a()];
                        cmds.push(AppCommand::UpdateGeneratorData {
                            clip_id,
                            data: klipp_core::GeneratorData::Gradient {
                                top, bottom, angle_deg,
                                four_point: Some(new_corners),
                            },
                        });
                    }
                    ui.label(egui::RichText::new("TL").size(10.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    if ui.color_edit_button_srgba(&mut tr).changed() {
                        let mut new_corners = corners;
                        new_corners.top_right = [tr.r(), tr.g(), tr.b(), tr.a()];
                        cmds.push(AppCommand::UpdateGeneratorData {
                            clip_id,
                            data: klipp_core::GeneratorData::Gradient {
                                top, bottom, angle_deg,
                                four_point: Some(new_corners),
                            },
                        });
                    }
                    ui.label(egui::RichText::new("TR").size(10.0).color(theme.text_dim));
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let mut bl = egui::Color32::from_rgba_premultiplied(
                        corners.bottom_left[0], corners.bottom_left[1],
                        corners.bottom_left[2], corners.bottom_left[3],
                    );
                    let mut br = egui::Color32::from_rgba_premultiplied(
                        corners.bottom_right[0], corners.bottom_right[1],
                        corners.bottom_right[2], corners.bottom_right[3],
                    );
                    if ui.color_edit_button_srgba(&mut bl).changed() {
                        let mut new_corners = corners;
                        new_corners.bottom_left = [bl.r(), bl.g(), bl.b(), bl.a()];
                        cmds.push(AppCommand::UpdateGeneratorData {
                            clip_id,
                            data: klipp_core::GeneratorData::Gradient {
                                top, bottom, angle_deg,
                                four_point: Some(new_corners),
                            },
                        });
                    }
                    ui.label(egui::RichText::new("BL").size(10.0).color(theme.text_dim));
                    ui.add_space(8.0);
                    if ui.color_edit_button_srgba(&mut br).changed() {
                        let mut new_corners = corners;
                        new_corners.bottom_right = [br.r(), br.g(), br.b(), br.a()];
                        cmds.push(AppCommand::UpdateGeneratorData {
                            clip_id,
                            data: klipp_core::GeneratorData::Gradient {
                                top, bottom, angle_deg,
                                four_point: Some(new_corners),
                            },
                        });
                    }
                    ui.label(egui::RichText::new("BR").size(10.0).color(theme.text_dim));
                });
            } else {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Start").strong().size(11.0));
                    let mut c = egui::Color32::from_rgba_premultiplied(
                        top[0], top[1], top[2], top[3],
                    );
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        cmds.push(AppCommand::UpdateGeneratorData {
                            clip_id,
                            data: klipp_core::GeneratorData::Gradient {
                                top: [c.r(), c.g(), c.b(), c.a()],
                                bottom,
                                angle_deg,
                                four_point: None,
                            },
                        });
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("End").strong().size(11.0));
                    let mut c = egui::Color32::from_rgba_premultiplied(
                        bottom[0], bottom[1], bottom[2], bottom[3],
                    );
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        cmds.push(AppCommand::UpdateGeneratorData {
                            clip_id,
                            data: klipp_core::GeneratorData::Gradient {
                                top,
                                bottom: [c.r(), c.g(), c.b(), c.a()],
                                angle_deg,
                                four_point: None,
                            },
                        });
                    }
                });
                ui.add_space(6.0);
                // Angle slider — 0 = top→bottom (legacy default), 90 =
                // left→right, 180 = bottom→top, 270 = right→left.
                let mut angle = angle_deg;
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Slider::new(&mut angle, 0.0..=360.0)
                                .text("Angle")
                                .step_by(1.0)
                                .suffix("\u{00B0}"),
                        )
                        .changed()
                    {
                        cmds.push(AppCommand::UpdateGeneratorData {
                            clip_id,
                            data: klipp_core::GeneratorData::Gradient {
                                top, bottom,
                                angle_deg: angle,
                                four_point: None,
                            },
                        });
                    }
                });
                // Quick-direction buttons (cardinals) — quicker than
                // dragging the slider for the common cases.
                ui.horizontal(|ui| {
                    let mut emit = |a: f32| {
                        cmds.push(AppCommand::UpdateGeneratorData {
                            clip_id,
                            data: klipp_core::GeneratorData::Gradient {
                                top, bottom,
                                angle_deg: a,
                                four_point: None,
                            },
                        });
                    };
                    if ui.small_button("\u{2193}").on_hover_text("Top → Bottom").clicked() { emit(0.0); }
                    if ui.small_button("\u{2192}").on_hover_text("Left → Right").clicked() { emit(90.0); }
                    if ui.small_button("\u{2191}").on_hover_text("Bottom → Top").clicked() { emit(180.0); }
                    if ui.small_button("\u{2190}").on_hover_text("Right → Left").clicked() { emit(270.0); }
                    if ui.small_button("\u{2198}").on_hover_text("TL → BR (45°)").clicked() { emit(135.0); }
                    if ui.small_button("\u{2197}").on_hover_text("BL → TR (45°)").clicked() { emit(45.0); }
                });
            }
        }

        klipp_core::GeneratorData::ColorBars => {
            ui.label(egui::RichText::new("SMPTE 75% Color Bars").size(11.0).color(theme.text_secondary));
            ui.add_space(2.0);
            ui.label(egui::RichText::new("No editable parameters.").size(10.0).color(theme.text_dim));
        }

        klipp_core::GeneratorData::Countdown => {
            ui.label(egui::RichText::new("Countdown Timer").size(11.0).color(theme.text_secondary));
            ui.add_space(2.0);
            ui.label(egui::RichText::new("Duration determined by clip length.").size(10.0).color(theme.text_dim));
        }

        klipp_core::GeneratorData::StillFrame { png_path, width, height } => {
            ui.label(egui::RichText::new("Freeze Frame").size(11.0).color(theme.text_secondary));
            ui.add_space(2.0);
            ui.label(egui::RichText::new(format!("{}×{}", width, height)).size(10.0).color(theme.text_dim));
            ui.label(
                egui::RichText::new(
                    png_path.file_name().and_then(|n| n.to_str()).unwrap_or("<unknown>"),
                )
                .size(10.0)
                .color(theme.text_dim),
            );
        }
    }
}
