pub(crate) mod keyframe_selection;
mod vector_path;
pub mod key_column;
mod gradient_editor;
pub mod letter_picker;
pub mod tracker_picker;
pub mod video_tab;
pub mod piece_transition;
pub mod sequence_view;
pub(crate) mod rows;
pub mod placement_by_path;
pub mod audio_tab;
pub mod audio_fx_inspector;
pub mod color_and_luts;
pub mod effects_tab;
pub mod generated;
pub mod grade_view;
pub mod stack_view;
pub(crate) mod side_matte;
pub mod info_tab;
pub mod modifier_ui;
pub mod title_tab;
pub mod title_animators;
pub mod title_layers;
pub mod font_picker;
pub mod title_text3d;
pub mod title_material;
mod title_features;

use crate::icons::Icon;
use crate::layout_state::InspectorTab;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

fn inspector_scope_banner(ui: &mut egui::Ui, theme: &KlippTheme, scope: &str, detail: &str) {
    egui::Frame::none()
        .fill(theme.accent_subtle)
        .stroke(egui::Stroke::new(1.0_f32, theme.accent))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(8.0, 5.0))
        .outer_margin(egui::Margin::symmetric(0.0, 4.0))
        .show(ui, |ui| {
            // Wrapped, so a narrow column takes it on more lines rather
            // than widening to fit it and pushing every row after it off
            // the edge.
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(scope).size(10.0).strong().color(theme.accent));
                ui.add(egui::Label::new(egui::RichText::new(detail).size(10.0).color(theme.text_secondary)).wrap());
            });
        });
}

#[derive(Default)]
pub struct NewInspectorPanel {
    pub tag_input: String,
    pub duck: audio_tab::DuckSettings,
    /// Clip id for which `duck` was last initialised. When the selection
    /// changes, we re-seed `duck` (prefs defaults + role-aware sidechain
    /// choice) so every new clip gets sensible starting values without
    /// overriding the user's in-session adjustments on the current clip.
    pub last_duck_clip: Option<klipp_core::ClipId>,
    /// Chips for the Title tab, rendered by the title engine.
    pub title_chips: title_tab::TitleChips,
    /// The last selected clip used for synthetic-tab routing. This keeps
    /// title/generator clips opening on their useful tab once, without
    /// overriding a user's later click on Video, Effects, or Info.
    pub last_synthetic_clip: Option<klipp_core::ClipId>,
    /// The clip name being typed (#920), with the clip it was taken
    /// from: refilled from the clip when another is selected.
    clip_name_edit: Option<(klipp_core::ClipId, String)>,
    /// When the parameter in focus last came into focus (#2296), so the
    /// tab follows it once and the user's own tab click is kept after.
    last_focus_at: Option<f64>,
    /// An animated parameter picked here this frame, a chip or a row, for
    /// the app to bring into focus (#2296).
    pub focus_request: Option<crate::state::KeyframeParamKind>,
    /// When the focus last selected a title track's layer, so it does so
    /// once per focus and the user's own layer pick is kept after.
    pub layer_followed: Option<f64>,
    /// The Keyframes column (#2305): shown, and how wide, as the layout
    /// keeps them; and what it shows this frame, for the app to draw.
    pub key_column: bool,
    pub key_column_w: f32,
    pub key_area: Option<key_column::KeyArea>,
    /// The Animated strip opened past its count (#2296).
    animated_open: bool,
    /// The selected clip's grade as the Colour page's rows read it, taken
    /// by the app before the Colour tab draws (#2952): its Basic
    /// Correction rows are those rows, on the Corrector the Colour page
    /// edits, or the one its first edit makes.
    pub(crate) colour_grade: Option<crate::inspector::grade_view::GradeSnapshot>,
    /// Auto Colour pressed on the Colour tab this frame, for the app to run.
    pub auto_colour: bool,
}

impl NewInspectorPanel {
    /// Drop every cached texture this panel owns, while wgpu is
    /// still alive. Same contract as the browser's.
    pub fn clear_textures_for_shutdown(&mut self) {
        self.title_chips.drain();
    }
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
        self.title_chips.begin_frame(ui.ctx());

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
        // tracks-iter walk — closes (steady state
        // is one Track::find_clip rather than tracks × clips).
        let clip_info = state.selected_clip_loc();
        let selected_clip_id = clip_info.map(|(_, clip)| clip.id);
        let clip_changed = self.last_synthetic_clip != selected_clip_id;
        self.last_synthetic_clip = selected_clip_id;
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
        let (clip_name, name_dim) = if let Some((_, c)) = clip_info {
            let name = match &c.kind {
                klipp_timeline::ClipKind::Title(td) => {
                    let text_preview = crate::ui_util::elide(&td.text(), 30);
                    format!("Title: {}", text_preview)
                }
                klipp_timeline::ClipKind::Gap => "Gap".into(),
                _ => c.label(media.map(|m| m.filename.as_str()).unwrap_or("Clip")).to_string(),
            };
            (name, false)
        } else if let Some(bm) = bin_media {
            (bm.filename.clone(), false)
        } else {
            // Name what the panel is showing, not what it is missing.
            // With nothing selected the body is the sequence's own
            // properties -- resolution, frame rate, audio output,
            // loudness -- so "No clip selected" described the selection
            // while everything under it described the sequence.
            let seq_name = state.sequence().name.clone();
            let title = if seq_name.trim().is_empty() {
                "Sequence".to_string()
            } else {
                format!("Sequence: {seq_name}")
            };
            (title, true)
        };
        if crate::widgets::inspector_group::panel_title_row(ui, &clip_name, name_dim, theme) {
            cmds.push(AppCommand::ToggleInspectorPanel);
        }

        // ── Several clips selected (#843) ──
        // A change here is made on every selected clip that has the
        // setting; the values shown are this clip's, and the line names
        // the ones that differ across the selection.
        if let Some((_, primary)) = clip_info {
            let many = state.selected_clips.len() > 1 && state.selected_clips.contains(&primary.id);
            if many {
                let seq = state.sequence();
                let clips: Vec<&klipp_timeline::Clip> = state.selected_clips.iter().filter_map(|id| seq.all_clips().find(|c| c.id == *id)).collect();
                let mixed = crate::multi_edit::mixed(&clips);
                let mut line = format!("Editing {} selected clips: a change is made on each that has the setting.", clips.len());
                if !mixed.is_empty() {
                    line.push_str(&format!(" Mixed (this clip's shown): {}.", mixed.join(", ")));
                }
                ui.add_space(2.0);
                ui.label(egui::RichText::new(line).size(11.0).color(theme.text_accent));
                ui.add_space(2.0);
            }
        }

        // ── Amber context banner (timeline clip only) ──
        if let Some((track_idx, clip)) = clip_info {
            let track_name = state.sequence().tracks.get(track_idx)
                .map(|t| if t.name.is_empty() { format!("Track {}", track_idx + 1) } else { t.name.clone() })
                .unwrap_or_default();
            let tc = state.timecode_display().position(clip.timeline_in);

            let banner_h = 24.0;
            let banner_rect = egui::Rect::from_min_size(
                ui.cursor().min,
                egui::vec2(ui.available_width(), banner_h),
            );
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(banner_rect), |ui| {
                // Amber background
                ui.painter().rect_filled(banner_rect, 0.0, theme.bg_panel_alt);
                ui.painter().hline(banner_rect.x_range(), banner_rect.max.y, egui::Stroke::new(1.0_f32, theme.border));

                // Color bar on left
                let bar_rect = egui::Rect::from_min_size(
                    banner_rect.min, egui::vec2(4.0, banner_h),
                );
                let role_color = theme.clip_role_color(clip.role);
                ui.painter().rect_filled(bar_rect, 0.0, role_color);

                // Text — dark colors for readability on amber background
                let banner_text = theme.text_secondary;
                let banner_tc = theme.text_secondary;
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

        // ── The clip's own name and colour (#920) ──
        // Of every selected clip when several are.
        if let Some((_, clip)) = clip_info {
            let targets: Vec<klipp_core::ClipId> = if state.selected_clips.len() > 1 && state.selected_clips.contains(&clip.id) {
                state.selected_clips.clone()
            } else {
                vec![clip.id]
            };
            show_clip_name_row(ui, theme, clip, &targets, &mut self.clip_name_edit, &mut cmds);
        }

        if let Some((_, clip)) = clip_info {
            if matches!(clip.kind, klipp_timeline::ClipKind::TemplatePrecomposition { .. }) {
                show_template_controls(ui, state, theme, clip, &mut cmds);
            }
        }

        // ── Animated strip (#2296) ──
        // The clip's animated parameters, on every tab: a chip opens the
        // tab that holds one and brings it into focus.
        let animated = clip_info.map(|(_, c)| crate::param_focus::animated(c)).unwrap_or_default();
        if !animated.is_empty() {
            // Up to this many chips show at once; more fold behind a count.
            const SHOWN: usize = 4;
            let accent = theme.accent;
            let folded = animated.len() > SHOWN && !self.animated_open;
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
                let count = egui::RichText::new(format!("\u{25C6} {} animated", animated.len())).size(10.0).color(accent);
                if animated.len() > SHOWN {
                    let tip = if folded { "Show every animated parameter" } else { "Show fewer" };
                    if ui.add(egui::Button::new(count).frame(false).small()).on_hover_text(tip).clicked() {
                        self.animated_open = !self.animated_open;
                    }
                } else {
                    ui.label(egui::RichText::new("Animated").size(10.0).color(accent));
                }
                if folded {
                    return;
                }
                for (kind, label, tab) in &animated {
                    let chip = egui::Button::new(egui::RichText::new(format!("\u{25C6} {label}")).size(10.0).color(accent))
                        .fill(accent.gamma_multiply(0.12))
                        .stroke(egui::Stroke::new(1.0_f32, accent.gamma_multiply(0.45)))
                        .rounding(8.0)
                        .small();
                    if ui.add(chip).on_hover_text("Show it here and its curve in the Curve Editor").clicked() {
                        *active_tab = *tab;
                        self.focus_request = Some(kind.clone());
                    }
                }
            });
            ui.add_space(2.0);
        }
        // The tab follows a parameter newly in focus, wherever it was picked.
        if let (Some(focus), Some((_, clip))) = (&state.param_focus, clip_info) {
            if focus.clip_id == clip.id && self.last_focus_at != Some(focus.at) {
                self.last_focus_at = Some(focus.at);
                if let Some(tab) = crate::param_focus::tab_of(clip, &focus.kind) {
                    *active_tab = tab;
                }
            }
        }

        // ── Tab bar ──
        let is_title_clip = clip_info.as_ref().is_some_and(|(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Title(_)));
        let is_generator_clip = clip_info.as_ref().is_some_and(|(_, c)| matches!(c.kind, klipp_timeline::ClipKind::Generator(_)));
        let tabs: Vec<(InspectorTab, &str, &str)> = if is_title_clip {
            vec![
                (InspectorTab::Title, "TITLE", "TTL"),
                (InspectorTab::Video, "CLIP VIDEO", "CLIP"),
                (InspectorTab::Effects, "EFFECTS", "FX"),
                (InspectorTab::Info, "INFO", "INFO"),
            ]
        } else if is_generator_clip {
            vec![
                (InspectorTab::Generator, "GENERATOR", "GEN"),
                (InspectorTab::Video, "VIDEO", "VID"),
                (InspectorTab::Effects, "EFFECTS", "FX"),
                (InspectorTab::Info, "INFO", "INFO"),
            ]
        } else {
            vec![
                (InspectorTab::Video, "VIDEO", "VID"),
                (InspectorTab::Audio, "AUDIO", "AUD"),
                (InspectorTab::Color, "COLOUR", "COL"),
                (InspectorTab::Effects, "EFFECTS", "FX"),
                (InspectorTab::Info, "INFO", "INFO"),
            ]
        };
        // A tab holding an animated parameter is marked, its tip naming them (#2296).
        let marks: Vec<Option<String>> = tabs.iter().map(|(tab, ..)| {
            let names: Vec<&str> = animated.iter().filter(|(_, _, t)| t == tab).map(|(_, l, _)| l.as_str()).collect();
            (!names.is_empty()).then(|| format!("Animated here: {}", names.join(", ")))
        }).collect();
        let marked = |name: &str, i: usize| if marks[i].is_some() { format!("{name} \u{25C6}") } else { name.to_string() };
        let labels: Vec<String> = tabs.iter().enumerate().map(|(i, t)| marked(t.1, i)).collect();
        let brief: Vec<String> = tabs.iter().enumerate().map(|(i, t)| marked(t.2, i)).collect();
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let brief: Vec<&str> = brief.iter().map(String::as_str).collect();
        let active = tabs.iter().position(|t| t.0 == *active_tab).unwrap_or(0);
        if let Some(i) = crate::widgets::inspector_group::tab_strip_with_tips(ui, &labels, &brief, &marks, active, theme) {
            *active_tab = tabs[i].0;
        }

        // Auto-switch to Title/Generator tab when a synthetic clip is selected (and away when not)
        if clip_changed && is_title_clip && *active_tab != InspectorTab::Title {
            *active_tab = InspectorTab::Title;
        } else if clip_changed && is_generator_clip && *active_tab != InspectorTab::Generator {
            *active_tab = InspectorTab::Generator;
        } else if (!is_title_clip && *active_tab == InspectorTab::Title)
            || (!is_generator_clip && *active_tab == InspectorTab::Generator)
        {
            // The selected clip no longer has the synthetic tab that is
            // showing, so fall back to Video.
            *active_tab = InspectorTab::Video;
        }

        // ── Tab content ──
        // Keep the content rail tied to the panel's current width.  The
        // vertical scroll area used to auto-size itself from the widest
        // child, so a compact Inspector could grow an invisible horizontal
        // extent and clip the controls at the right edge.  A fixed-width
        // content rail lets rows wrap instead of asking users to resize the
        // panel just to reach a field.
        // The Keyframes column's switch (#2305), right of the tabs' row.
        self.key_area = None;
        if clip_info.is_some() {
            let fits = ui.available_width() >= key_column::ROWS_MIN_W + key_column::MIN_W;
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let text = egui::RichText::new("\u{25C6} Keyframes").size(10.0);
                    let toggle = ui.add_enabled(fits, egui::SelectableLabel::new(self.key_column && fits, text));
                    let toggle = if fits { toggle.on_hover_text("Show each parameter's keys beside it") } else { toggle.on_disabled_hover_text("Widen the Inspector to show each parameter's keys beside it") };
                    if toggle.clicked() {
                        self.key_column = !self.key_column;
                    }
                });
            });
        }
        if let Some((_, clip)) = clip_info {
            crate::param_focus::begin_rows(ui.ctx(), state.param_focus.as_ref(), clip, self.key_column);
        }
        egui::ScrollArea::vertical()
            .id_salt("inspector_body_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
            let full = ui.available_width();
            // The rows keep their room; the column takes the rest of its width.
            let column_w = if self.key_column && clip_info.is_some() && full >= key_column::ROWS_MIN_W + key_column::MIN_W {
                self.key_column_w.clamp(key_column::MIN_W, full - key_column::ROWS_MIN_W)
            } else {
                0.0
            };
            let content_width = full - column_w;
            let top = ui.cursor().min;
            ui.set_width(full);
            let rows_drawn = ui.allocate_ui_with_layout(egui::vec2(content_width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
            ui.set_width(content_width);
            ui.add_space(10.0);
            match active_tab {
                InspectorTab::Video => {
                    if let Some((_, clip)) = clip_info {
                        let clip_time_secs = keyframe_selection::clip_time(state, clip);
                        if matches!(clip.kind, klipp_timeline::ClipKind::Title(_)) {
                            inspector_scope_banner(
                                ui,
                                theme,
                                "WHOLE CLIP",
                                "These controls affect the complete title clip and its video frame.",
                            );
                        }
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
                        let clip_time_secs = keyframe_selection::clip_time(state, clip);
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
                            picture: Default::default(),
                            linked_clip_id: None,
                            linked_title_asset_id: None,
                            group_id: None,
                            grade_group: None,
                            magnetic_anchor: None,
                            gain: 1.0, volume: 1.0, pan: 0.0,
                            fade_in_secs: 0.0, fade_out_secs: 0.0,
                            fade_in_curve: klipp_timeline::FadeCurve::default(),
                            fade_out_curve: klipp_timeline::FadeCurve::default(),
                            channel_config: klipp_timeline::ChannelConfig::default(),
                            volume_keyframes: Vec::new(), pan_keyframes: Vec::new(), gain_keyframes: Vec::new(),
                            matte_feather_keyframes: Vec::new(), matte_choke_keyframes: Vec::new(),
                            clip_markers: Vec::new(),
                            frame_glide: None,
                            role: klipp_core::ClipRole::default(),
                            role_source: klipp_core::RoleSource::Unset,
                            matched_to: None,
                            speed: 1.0,
                            speed_keyframes: Vec::new(),
                            keep_pitch: false,
                            hold: None,
                            source_rotation: 0,
                            enabled: true,
                            solo: false,
                            blend_mode: klipp_core::BlendMode::Normal,
                            depth: 1.0,
                            matte_source: None,
                            matte_stencil: None,
                            consumed_as_matte: false,
                            transform_link: None,
                            multicam_group: None,
                            multicam_angle: 0,
                            multicam_cuts: Vec::new(),
                            multicam_sound: Default::default(),
                            audio_effects: Vec::new(),
                            audio_effect_keyframes: std::collections::HashMap::new(),
                            track_points: Vec::new(),
                            surfaces: Vec::new(),
                            surface_pin: None,
                            name: None,
                            colour_label: 0,
                            grade_flag: Default::default(),
                            precomp: None,
                            baked_own: None,
                        };
                        audio_tab::show(ui, theme, state, Some((&temp_clip, 0, false)), &mut cmds, (0.0, 0.0), None, source_channels, 0.0, &mut self.duck, &track_infos);
                    } else {
                        audio_tab::show(ui, theme, state, None, &mut cmds, (0.0, 0.0), None, None, 0.0, &mut self.duck, &[]);
                    }
                }
                InspectorTab::Effects => {
                    if let Some((_, clip)) = clip_info {
                        let clip_time_secs = keyframe_selection::clip_time(state, clip);
                        stack_view::show(ui, theme, state, clip, &mut cmds, clip_time_secs);
                    } else {
                        effects_tab::show_empty(ui, theme, &mut cmds);
                    }
                }
                InspectorTab::Color => {
                    crate::inspector::color_and_luts::show_color_and_luts(ui, state, theme, self.colour_grade.as_ref(), &mut self.auto_colour, &mut cmds);
                }
                InspectorTab::Title => {
                    match clip_info {
                        Some((_, clip)) if matches!(clip.kind, klipp_timeline::ClipKind::Title(_)) => {
                            // 12 px horizontal content rail — every row in
                            // show_title_editor inherits the same horizontal
                            // inset, so nothing touches the inspector edges.
                            egui::Frame::none()
                                .inner_margin(egui::Margin {
                                    left: 12.0, right: 12.0, top: 0.0, bottom: 0.0,
                                })
                                .show(ui, |ui| {
                                    let full = |ui: &mut egui::Ui, cmds: &mut Vec<AppCommand>, chips: &mut title_tab::TitleChips| {
                                        inspector_scope_banner(
                                            ui,
                                            theme,
                                            "TEXT LAYER",
                                            "These controls affect the selected text layer. Use CLIP VIDEO for the whole title clip.",
                                        );
                                        show_title_editor(ui, theme, clip, state, cmds, chips);
                                    };
                                    // A template's fields first, the rest
                                    // folded away (#1139): simple first.
                                    let td = clip.title_data().expect("a title");
                                    if title_tab::template_fields(ui, theme, clip.id, td, &mut cmds) {
                                        ui.add_space(4.0);
                                        egui::CollapsingHeader::new("All settings")
                                            .id_salt(("title_all_settings", clip.id))
                                            .default_open(false)
                                            .show(ui, |ui| full(ui, &mut cmds, &mut self.title_chips));
                                    } else {
                                        full(ui, &mut cmds, &mut self.title_chips);
                                    }
                                });
                        }
                        _ => title_empty_state(ui, theme, &mut cmds),
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
                        info_tab::show_no_selection(ui, state, theme, &mut cmds);
                    }
                }
            }
            });
            // The Keyframes column beside the rows (#2305), and the edge
            // that sets its width.
            if let (true, Some((_, clip))) = (column_w > 0.0, clip_info) {
                let seen = ui.clip_rect();
                // Never past the panel's visible edge, or the last keys are cut.
                let right = (top.x + full).min(seen.max.x).min(ui.ctx().screen_rect().max.x);
                let column = egui::Rect::from_x_y_ranges(top.x + content_width..=right, seen.min.y.max(top.y)..=seen.max.y.min(rows_drawn.response.rect.max.y));
                let edge = egui::Rect::from_x_y_ranges(column.min.x - 3.0..=column.min.x + 3.0, column.y_range());
                let grip = ui.interact(edge, ui.id().with("key_column_edge"), egui::Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
                if grip.dragged() {
                    self.key_column_w = (self.key_column_w - grip.drag_delta().x).clamp(key_column::MIN_W, (full - key_column::ROWS_MIN_W).max(key_column::MIN_W));
                }
                self.key_area = Some(key_column::KeyArea { clip_id: clip.id, column, rows: crate::param_focus::take_rows(ui.ctx()) });
            }
        });

        for cmd in &mut cmds {
            match cmd {
                AppCommand::AddAudioKeyframe { clip_id, param, time_secs, value } => {
                    *cmd = AppCommand::EditAudioKeyframeAtFrame { clip_id: *clip_id, param: *param, time_secs: *time_secs, value: Some(*value) };
                }
                AppCommand::RemoveAudioKeyframe { clip_id, param, time_secs } => {
                    *cmd = AppCommand::EditAudioKeyframeAtFrame { clip_id: *clip_id, param: *param, time_secs: *time_secs, value: None };
                }
                _ => keyframe_selection::resolve_command(state.sequence(), cmd),
            }
        }
        // A row's name or diamond clicked brings its parameter into focus.
        if let Some(kind) = crate::param_focus::take_request(ui.ctx()) {
            self.focus_request = Some(kind);
        }
        cmds
    }
}

fn show_template_controls(
    ui: &mut egui::Ui,
    state: &AppState,
    theme: &KlippTheme,
    clip: &klipp_timeline::Clip,
    cmds: &mut Vec<AppCommand>,
) {
    let klipp_timeline::ClipKind::TemplatePrecomposition { template_instance, .. } = &clip.kind else { return };
    let Some(template) = state.composition_templates.templates().find(|template| template.id == template_instance.template_id) else { return };
    ui.heading(egui::RichText::new("TEMPLATE").color(theme.text_primary).size(12.0));
    ui.add_space(4.0);
    for (name, parameter) in &template.parameters {
        let mut value = template_instance.overrides.get(name).cloned().unwrap_or_else(|| parameter.default.clone());
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.label(&parameter.label);
            match &mut value {
                crate::parameterized_templates::TemplateValue::Text(text) => {
                    changed = ui.text_edit_singleline(text).changed();
                }
                crate::parameterized_templates::TemplateValue::Number(number) => {
                    let mut drag = egui::DragValue::new(number).speed(0.01);
                    if let Some(range) = &parameter.range {
                        if let (Some(min), Some(max)) = (range.min, range.max) { drag = drag.range(min..=max); }
                    }
                    changed = ui.add(drag).changed();
                    if changed {
                        if let Some(range) = &parameter.range {
                            if let Some(min) = range.min { *number = number.max(min); }
                            if let Some(max) = range.max { *number = number.min(max); }
                        }
                    }
                }
                crate::parameterized_templates::TemplateValue::Bool(enabled) => {
                    changed = ui.checkbox(enabled, "").changed();
                }
                crate::parameterized_templates::TemplateValue::Enum(choice) => {
                    egui::ComboBox::from_id_salt((clip.id, name))
                        .selected_text(choice.clone())
                        .show_ui(ui, |ui| {
                            for option in &parameter.choices { changed |= ui.selectable_value(choice, option.clone(), option).changed(); }
                        });
                }
                crate::parameterized_templates::TemplateValue::Color(color) => {
                    changed = ui.color_edit_button_rgba_unmultiplied(color).changed();
                }
            }
        });
        if changed {
            cmds.push(AppCommand::SetTemplateOverride { clip_id: clip.id, parameter: name.clone(), value });
        }
        template_link_rows(ui, theme, clip.id, template_instance, template, name, &parameter.label, cmds);
    }
}

/// What a control drives (#2757), one row a target: its name, its value
/// (a drag sets it by hand, the control kept working through an offset),
/// a badge naming the control or "unlinked" (the row greyed), the offset
/// beside the value, and Unlink / Relink / Reset offset.
#[allow(clippy::too_many_arguments)]
fn template_link_rows(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    instance: &klipp_core::TemplateInstance,
    template: &crate::parameterized_templates::CompositionTemplate,
    parameter: &str,
    control_label: &str,
    cmds: &mut Vec<AppCommand>,
) {
    let links: Vec<&klipp_core::TemplateLink> = instance.links.iter().filter(|l| l.parameter == parameter).collect();
    if links.is_empty() {
        return;
    }
    let names = template.composition().map(klipp_timeline::composition_template::number_targets).unwrap_or_default();
    let control = instance.control(parameter);
    for link in links {
        let label = names.iter().find(|(_, p)| *p == link.property).map_or(link.property.as_str(), |(l, _)| l.as_str());
        let colour = if link.linked { theme.text_primary } else { theme.text_dim };
        let set = |action| AppCommand::SetTemplateLink { clip_id, property: link.property.clone(), action };
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(egui::RichText::new(label).color(colour).size(11.0));
            let mut value = link.effective(control);
            if ui.add(egui::DragValue::new(&mut value).speed(0.01).max_decimals(2)).changed() {
                cmds.push(set(klipp_core::LinkAction::Set(value)));
            }
            let (badge, tip) = link_badge(link, control_label);
            ui.label(egui::RichText::new(badge).color(if link.linked { theme.text_accent } else { theme.text_dim }).size(10.0)).on_hover_text(tip);
            if link.linked {
                if ui.small_button("Unlink").on_hover_text("Keep this value whatever the control does").clicked() {
                    cmds.push(set(klipp_core::LinkAction::Unlink));
                }
                if link.offset != 0.0 && ui.small_button("Reset").on_hover_text("Drop the offset: follow the control exactly").clicked() {
                    cmds.push(set(klipp_core::LinkAction::ResetOffset));
                }
            } else if ui.small_button("Relink").on_hover_text("Follow the control again from this value: nothing jumps").clicked() {
                cmds.push(set(klipp_core::LinkAction::Relink));
            }
        });
    }
}

/// The badge beside a target's value, and its hover text: the control it
/// follows and the offset a hand edit left ("Grow +0.12"), or "unlinked".
fn link_badge(link: &klipp_core::TemplateLink, control: &str) -> (String, String) {
    if !link.linked {
        return ("unlinked".into(), format!("Not following {control}: Relink to follow it again"));
    }
    let curve = link.curve.name().replace('_', " ");
    if link.offset.abs() < 1e-9 {
        (format!("⇄ {control}"), format!("Follows {control}, {} to {}, {curve}", link.to[0], link.to[1]))
    } else {
        (format!("⇄ {control} {:+.2}", link.offset), format!("Follows {control}, {} to {}, {curve}, offset {:+.2} from a hand edit", link.to[0], link.to[1], link.offset))
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

/// What a gradient's Grain slider does (#2073).
const GENERATOR_GRAIN_HINT: &str = "Fine grain, in code values, that keeps a dark gradient smooth after an 8-bit export. 0 turns it off.";

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
    (6, "Gold"),
    (7, "Silver"),
    (8, "Chrome"),
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
/// the keyframe vec using the same frame window as video and audio.
fn title_object_keyframe_state(
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    keyframe_tolerance: f64,
) -> crate::widgets::scrubby_param::KeyframeState {
    crate::inspector::keyframe_selection::state(keyframes, clip_time_secs, keyframe_tolerance)
}

/// Translate a `ScrubbyParamResponse` for one position axis into the
/// corresponding AppCommand(s). Three affordances:
/// - Diamond click: toggle a keyframe at the playhead.
/// - Scrubby drag (when keyframes exist): upsert a keyframe at the
///   playhead with the new value (handler uses the shared timestamp tolerance).
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
    keyframe_tolerance: f64,
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
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some()
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
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
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
    keyframe_tolerance: f64,
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
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some()
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
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
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
    keyframe_tolerance: f64,
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
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some()
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
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
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
    keyframe_tolerance: f64,
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
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some()
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
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
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

/// Translate a `ScrubbyParamResponse` for the keyframable
/// TextOutline.width on a given stack index. The wire param_key is
/// built per-call from `klipp_core::title_effect_keys::outline_width_key(index)`,
/// and the static-fallback path differs between index 0 (uses
/// SetTitleOutline) and higher indices (uses SetTitleOutlineLayer).
fn handle_outline_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    outline_index: usize,
    new_value: f32,
    cur: &klipp_timeline::TextOutline,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
    keyframe_tolerance: f64,
) {
    let param_key = klipp_core::title_effect_keys::outline_width_key(outline_index);
    let push_static_fallback = |cmds: &mut Vec<AppCommand>, new_width: f32| {
        // Index 0 routes through SetTitleOutline (the legacy
        // single-outline command). Higher indices use the
        // SetTitleOutlineLayer command which carries the index.
        if outline_index == 0 {
            cmds.push(AppCommand::SetTitleOutline {
                clip_id, enabled: cur.enabled,
                color: cur.color, width: new_width,
            });
        } else {
            cmds.push(AppCommand::SetTitleOutlineLayer {
                clip_id, index: outline_index as u32,
                enabled: cur.enabled,
                color: cur.color, width: new_width,
            });
        }
    };
    let Some(oid) = target_oid else {
        if resp.changed {
            push_static_fallback(cmds, new_value);
        }
        return;
    };
    let object_id = oid.0;

    if resp.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some()
        {
            cmds.push(AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.clone(),
                time_secs: clip_time_secs,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.clone(),
                time_secs: clip_time_secs,
                value: new_value,
            });
        }
        return;
    }

    if resp.changed {
        if keyframes.is_empty() {
            push_static_fallback(cmds, new_value);
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id,
                object_id,
                param_key: param_key.clone(),
                time_secs: clip_time_secs,
                value: new_value,
            });
        }
    }

    if let Some(interp) = resp.keyframe_set_interpolation {
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
        {
            cmds.push(AppCommand::SetTitleObjectEffectKeyframeInterpolation {
                clip_id,
                object_id,
                param_key,
                keyframe_index: idx,
                interpolation: interp,
            });
        }
    }
}

/// Indexed-stack variant of `handle_shadow_axis_response`.
/// Dispatches with the per-index `shadow.{idx}.field` param_key and
/// the per-index `SetTitleShadowLayer` static fallback so layers >= 1
/// keyframe / edit independently of `shadows[0]`.
#[allow(clippy::too_many_arguments)]
fn handle_shadow_layer_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    layer_index: usize,
    field: &'static str,
    new_value: f32,
    cur: &klipp_timeline::TitleShadow,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
    keyframe_tolerance: f64,
) {
    let param_key = match field {
        "offset_x" => klipp_core::title_effect_keys::shadow_offset_x_key(layer_index),
        "offset_y" => klipp_core::title_effect_keys::shadow_offset_y_key(layer_index),
        "blur" => klipp_core::title_effect_keys::shadow_blur_key(layer_index),
        _ => return,
    };
    let push_static_fallback = |cmds: &mut Vec<AppCommand>, new: f32| {
        let (offset_x, offset_y, blur) = match field {
            "offset_x" => (new, cur.offset_y, cur.blur),
            "offset_y" => (cur.offset_x, new, cur.blur),
            "blur" => (cur.offset_x, cur.offset_y, new),
            _ => (cur.offset_x, cur.offset_y, cur.blur),
        };
        cmds.push(AppCommand::SetTitleShadowLayer {
            clip_id,
            index: layer_index as u32,
            enabled: cur.enabled,
            color: cur.color,
            offset_x, offset_y, blur,
        });
    };
    let Some(oid) = target_oid else {
        if resp.changed { push_static_fallback(cmds, new_value); }
        return;
    };
    let object_id = oid.0;
    if resp.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some() {
            cmds.push(AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id, object_id, param_key: param_key.clone(),
                time_secs: clip_time_secs,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id, object_id, param_key: param_key.clone(),
                time_secs: clip_time_secs, value: new_value,
            });
        }
        return;
    }
    if resp.changed {
        if keyframes.is_empty() {
            push_static_fallback(cmds, new_value);
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id, object_id, param_key: param_key.clone(),
                time_secs: clip_time_secs, value: new_value,
            });
        }
    }
    if let Some(interp) = resp.keyframe_set_interpolation {
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
        {
            cmds.push(AppCommand::SetTitleObjectEffectKeyframeInterpolation {
                clip_id, object_id, param_key,
                keyframe_index: idx, interpolation: interp,
            });
        }
    }
}

/// Indexed-stack variant for InnerShadow. Mirrors
/// `handle_shadow_layer_axis_response` with the inner-shadow command +
/// key prefix.
#[allow(clippy::too_many_arguments)]
fn handle_inner_shadow_layer_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    layer_index: usize,
    field: &'static str,
    new_value: f32,
    cur: &klipp_timeline::InnerShadow,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
    keyframe_tolerance: f64,
) {
    let param_key = match field {
        "offset_x" => klipp_core::title_effect_keys::inner_shadow_offset_x_key(layer_index),
        "offset_y" => klipp_core::title_effect_keys::inner_shadow_offset_y_key(layer_index),
        "blur" => klipp_core::title_effect_keys::inner_shadow_blur_key(layer_index),
        _ => return,
    };
    let push_static_fallback = |cmds: &mut Vec<AppCommand>, new: f32| {
        let (offset_x, offset_y, blur) = match field {
            "offset_x" => (new, cur.offset_y, cur.blur),
            "offset_y" => (cur.offset_x, new, cur.blur),
            "blur" => (cur.offset_x, cur.offset_y, new),
            _ => (cur.offset_x, cur.offset_y, cur.blur),
        };
        cmds.push(AppCommand::SetTitleInnerShadowLayer {
            clip_id,
            index: layer_index as u32,
            enabled: cur.enabled,
            color: cur.color,
            offset_x, offset_y, blur,
        });
    };
    let Some(oid) = target_oid else {
        if resp.changed { push_static_fallback(cmds, new_value); }
        return;
    };
    let object_id = oid.0;
    if resp.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some() {
            cmds.push(AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id, object_id, param_key: param_key.clone(),
                time_secs: clip_time_secs,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id, object_id, param_key: param_key.clone(),
                time_secs: clip_time_secs, value: new_value,
            });
        }
        return;
    }
    if resp.changed {
        if keyframes.is_empty() {
            push_static_fallback(cmds, new_value);
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id, object_id, param_key: param_key.clone(),
                time_secs: clip_time_secs, value: new_value,
            });
        }
    }
    if let Some(interp) = resp.keyframe_set_interpolation {
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
        {
            cmds.push(AppCommand::SetTitleObjectEffectKeyframeInterpolation {
                clip_id, object_id, param_key,
                keyframe_index: idx, interpolation: interp,
            });
        }
    }
}

/// Indexed-stack variant for Glow. `field` ∈ {"radius", "intensity"}.
#[allow(clippy::too_many_arguments)]
fn handle_glow_layer_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    layer_index: usize,
    field: &'static str,
    new_value: f32,
    cur: &klipp_timeline::TextGlow,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
    keyframe_tolerance: f64,
) {
    let param_key = match field {
        "radius" => klipp_core::title_effect_keys::glow_radius_key(layer_index),
        "intensity" => klipp_core::title_effect_keys::glow_intensity_key(layer_index),
        _ => return,
    };
    let push_static_fallback = |cmds: &mut Vec<AppCommand>, new: f32| {
        let (radius, intensity) = match field {
            "radius" => (new, cur.intensity),
            "intensity" => (cur.radius, new),
            _ => (cur.radius, cur.intensity),
        };
        cmds.push(AppCommand::SetTitleGlowLayer {
            clip_id,
            index: layer_index as u32,
            enabled: cur.enabled,
            color: cur.color,
            radius, intensity,
        });
    };
    let Some(oid) = target_oid else {
        if resp.changed { push_static_fallback(cmds, new_value); }
        return;
    };
    let object_id = oid.0;
    if resp.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some() {
            cmds.push(AppCommand::RemoveTitleObjectEffectKeyframe {
                clip_id, object_id, param_key: param_key.clone(),
                time_secs: clip_time_secs,
            });
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id, object_id, param_key: param_key.clone(),
                time_secs: clip_time_secs, value: new_value,
            });
        }
        return;
    }
    if resp.changed {
        if keyframes.is_empty() {
            push_static_fallback(cmds, new_value);
        } else {
            cmds.push(AppCommand::AddTitleObjectEffectKeyframe {
                clip_id, object_id, param_key: param_key.clone(),
                time_secs: clip_time_secs, value: new_value,
            });
        }
    }
    if let Some(interp) = resp.keyframe_set_interpolation {
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
        {
            cmds.push(AppCommand::SetTitleObjectEffectKeyframeInterpolation {
                clip_id, object_id, param_key,
                keyframe_index: idx, interpolation: interp,
            });
        }
    }
}

/// Translate a `ScrubbyParamResponse` for the keyframable
/// MotionBlur shutter_angle into the right AppCommand(s). MotionBlur
/// has just one keyframable param, so this is the simplest of the
/// per-effect helpers.
fn handle_motion_blur_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    new_value: f32,
    cur: &klipp_timeline::MotionBlur,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
    keyframe_tolerance: f64,
) {
    let param_key = klipp_core::title_effect_keys::MOTION_BLUR_SHUTTER_ANGLE;
    let Some(oid) = target_oid else {
        if resp.changed {
            cmds.push(AppCommand::SetTitleMotionBlur {
                clip_id, enabled: cur.enabled,
                shutter_angle: new_value, samples: cur.samples,
            });
        }
        return;
    };
    let object_id = oid.0;

    if resp.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some()
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
            cmds.push(AppCommand::SetTitleMotionBlur {
                clip_id, enabled: cur.enabled,
                shutter_angle: new_value, samples: cur.samples,
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
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
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

/// Translate a `ScrubbyParamResponse` for one keyframable
/// TextExtrusion param into the right AppCommand(s). Mirrors
/// `handle_shadow_axis_response`; the only differences are the
/// param-key constants and the static-fallback command (`SetTitleExtrusion`,
/// which carries face_color + side_color alongside the three numeric
/// params).
fn handle_extrusion_axis_response(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    param_key: &'static str,
    new_value: f32,
    cur: &klipp_timeline::TextExtrusion,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
    keyframe_tolerance: f64,
) {
    let to_static_triple = |new: f32| -> (f32, f32, f32) {
        match param_key {
            klipp_core::title_effect_keys::EXTRUSION_DEPTH => (new, cur.angle, cur.lighting),
            klipp_core::title_effect_keys::EXTRUSION_ANGLE => (cur.depth, new, cur.lighting),
            klipp_core::title_effect_keys::EXTRUSION_LIGHTING => (cur.depth, cur.angle, new),
            _ => (cur.depth, cur.angle, cur.lighting),
        }
    };
    let Some(oid) = target_oid else {
        if resp.changed {
            let (depth, angle, lighting) = to_static_triple(new_value);
            cmds.push(AppCommand::SetTitleExtrusion {
                clip_id, enabled: true,
                depth, angle,
                face_color: cur.face_color, side_color: cur.side_color,
                lighting,
            });
        }
        return;
    };
    let object_id = oid.0;

    if resp.keyframe_toggled {
        if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some()
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
            let (depth, angle, lighting) = to_static_triple(new_value);
            cmds.push(AppCommand::SetTitleExtrusion {
                clip_id, enabled: true,
                depth, angle,
                face_color: cur.face_color, side_color: cur.side_color,
                lighting,
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
        if let Some(idx) = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance)
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

/// What a keyable text row did (#1511): its diamond, an edited value,
/// or an interpolation picked for the key at the playhead.
#[derive(Default)]
struct TextKeyEdit {
    toggled: bool,
    changed: bool,
    interpolation: Option<klipp_core::Interpolation>,
}

impl From<&crate::widgets::scrubby_param::ScrubbyParamResponse> for TextKeyEdit {
    fn from(r: &crate::widgets::scrubby_param::ScrubbyParamResponse) -> Self {
        Self { toggled: r.keyframe_toggled, changed: r.changed, interpolation: r.keyframe_set_interpolation }
    }
}

/// The commands for a keyable text param (size, tracking, line spacing):
/// the diamond adds a key at the playhead or takes it away; an edit sets
/// the value (`set`) while it has no keys, and keys it at the playhead
/// once it has. Without a layer to key, an edit only sets it.
#[allow(clippy::too_many_arguments)]
fn handle_text_key_edit(
    cmds: &mut Vec<AppCommand>,
    clip_id: klipp_core::ClipId,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    param_key: &str,
    value: f32,
    keyframes: &[klipp_core::Keyframe],
    clip_time_secs: f64,
    keyframe_tolerance: f64,
    edit: TextKeyEdit,
    set: impl FnOnce(f32) -> AppCommand,
) {
    let Some(object_id) = target_oid.map(|o| o.0) else {
        if edit.changed {
            cmds.push(set(value));
        }
        return;
    };
    let at_key = keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance);
    if edit.toggled {
        cmds.push(match at_key {
            Some(_) => AppCommand::RemoveTitleObjectEffectKeyframe { clip_id, object_id, param_key: param_key.into(), time_secs: clip_time_secs },
            None => AppCommand::AddTitleObjectEffectKeyframe { clip_id, object_id, param_key: param_key.into(), time_secs: clip_time_secs, value },
        });
        return;
    }
    if edit.changed {
        cmds.push(if keyframes.is_empty() {
            set(value)
        } else {
            AppCommand::AddTitleObjectEffectKeyframe { clip_id, object_id, param_key: param_key.into(), time_secs: clip_time_secs, value }
        });
    }
    if let (Some(interpolation), Some(keyframe_index)) = (edit.interpolation, at_key) {
        cmds.push(AppCommand::SetTitleObjectEffectKeyframeInterpolation { clip_id, object_id, param_key: param_key.into(), keyframe_index, interpolation });
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
    let keyframe_tolerance = crate::inspector::keyframe_selection::frame_tolerance(state.sequence());
    use crate::state::TitlePositionPreset;
    use crate::widgets::inspector_group::quick_preset_button;

    let target_oid = state
        .selected_title_object
        .filter(|(c, _)| *c == clip_id)
        .map(|(_, oid)| oid);
    let target_obj = target_oid.and_then(|oid| td.object(oid));
    if target_obj.is_some_and(|o| matches!(&o.content, klipp_timeline::TitleObjectContent::Shape(shape) if shape.fit.is_some())) {
        ui.label("Position and size follow the text layer. Adjust its position or the backing padding.");
        return;
    }

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

    // X / Y compact 50/50 framed inputs. Default visual: just the
    // axis letter on the left and the DragValue on the right —
    // matching the reference. Diamond shows only when keyframes
    // exist on that axis (option b), keeping the unanimated default
    // state visually clean.
    let mut x = pos.x;
    let mut y = pos.y;
    let avail = ui.available_width();
    let gap = 8.0;
    let col_w = ((avail - gap) * 0.5).floor();
    let (resp_x, resp_y) = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            let ks = |kfs| title_object_keyframe_state(kfs, clip_time_secs, keyframe_tolerance);
            let rx = compact_axis_input(ui, theme, "X", &mut x, col_w, kfs_x, ks(kfs_x));
            let ry = compact_axis_input(ui, theme, "Y", &mut y, col_w, kfs_y, ks(kfs_y));
            (rx, ry)
        })
        .inner;
    handle_position_axis_response(
        cmds, clip_id, target_oid, klipp_core::TitlePositionAxis::X,
        x, pos.y, kfs_x, clip_time_secs, &resp_x,
        keyframe_tolerance,
    );
    handle_position_axis_response(
        cmds, clip_id, target_oid, klipp_core::TitlePositionAxis::Y,
        pos.x, y, kfs_y, clip_time_secs, &resp_y,
        keyframe_tolerance,
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
        // Force a known item_spacing so the math is predictable —
        // egui's default item_spacing.x stacks on top of any
        // ui.add_space() and would push "Top" past the right edge.
        // With item_spacing.x = 6 and no explicit add_space inside
        // the loop, we get exactly two 6 px gaps between three
        // equal-width thirds, and the row sits flush within the
        // 12 px content rail.
        ui.spacing_mut().item_spacing.x = 6.0;
        let avail = ui.available_width();
        let gap = 6.0;
        let third = ((avail - gap * 2.0) / 3.0).floor();
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
    _td: &klipp_timeline::TitleData,
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
            // As it is at the playhead, keyed or not (#1511).
            (tb.at(clip_time_secs).font_size, false)
        };
    // The whole text's size is keyable on its layer; a selection's is a
    // run's and is not.
    let size_target = state.selected_title_object.filter(|(c, _)| *c == clip_id).map(|(_, oid)| oid).filter(|_| !has_selection);
    let keyframe_tolerance = keyframe_selection::frame_tolerance(state.sequence());

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
            // As it is at the playhead, keyed or not (#1511).
            (tb.at(clip_time_secs).color, false)
        };

    // A selection's font, style, tracking and baseline offset, read
    // as its size and colour are (#1505).
    let from_runs = has_selection && !tb.style_runs.is_empty();
    let (display_font, font_is_mixed) = if from_runs {
        selection_run_value(tb, sel_range, |r| r.font_name.clone(), tb.font_name.clone())
    } else {
        (tb.font_name.clone(), false)
    };
    let (display_style, style_is_mixed) = if from_runs {
        selection_run_value(tb, sel_range, |r| r.font_style, tb.font_style)
    } else {
        (tb.font_style, false)
    };
    let (display_tracking, tracking_is_mixed) = if from_runs {
        selection_run_value(tb, sel_range, |r| r.tracking, tb.tracking)
    } else {
        (tb.tracking, false)
    };
    let (display_baseline, baseline_is_mixed) = if from_runs {
        selection_run_value(tb, sel_range, |r| r.baseline_offset, 0.0)
    } else {
        (0.0, false)
    };
    // And its underline (#1710).
    let (display_underline, underline_is_mixed) = if from_runs {
        selection_run_value(tb, sel_range, |r| r.underline, tb.underline)
    } else {
        (tb.underline, false)
    };
    // And its reading gloss (#1704).
    let (display_ruby, ruby_is_mixed) = if from_runs {
        selection_run_value(tb, sel_range, |r| r.ruby.clone(), String::new())
    } else {
        (String::new(), false)
    };

    // 1. Editable textarea — primary edit path. Wrapped in a Frame
    //    so the textarea reads as a pill matching the size / % / X / Y
    //    inputs; .frame(false) on the TextEdit drops its default
    //    border so the outer Frame is the only visual chrome.
    let mut text_buf = tb.text.clone();
    let text_resp = egui::Frame::none()
        .fill(theme.bg_raised)
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut text_buf)
                    .desired_width(f32::INFINITY)
                    .desired_rows(2)
                    .frame(false)
                    .font(egui::FontId::proportional(13.0)),
            )
        })
        .inner;
    if state.focus_title_text > 0 && !text_resp.has_focus() {
        text_resp.request_focus();
    }
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

    // 2. Font ComboBox + size DragValue. Over a selection the font
    //    picked sets the selection's (#1505), as its size does.
    let display_name = if display_font == "sans-serif" || display_font.is_empty() {
        "Noto Sans (Default)"
    } else {
        display_font.as_str()
    };
    let mut missing_font: Option<String> = None;
    ui.horizontal(|ui| {
        let total = ui.available_width();
        let size_w = 70.0;
        let combo_w = (total - size_w - 8.0).max(80.0);
        // The font picker (#1719): each family in its own face, the
        // title's words as the sample, starred and recent ones first.
        let choice = ui.vertical(|ui| {
            ui.set_width(combo_w);
            font_picker::picker(ui, theme, ("title_pinned_font", clip_id.0), if font_is_mixed { "Mixed" } else { display_name }, &tb.text, &state.available_fonts, &state.favourite_fonts, &state.recent_fonts, &state.font_previews)
        }).inner;
        if let Some(family) = choice.toggle_favourite {
            cmds.push(AppCommand::ToggleFavouriteFont { family });
        }
        // Fetched to preview, not chosen: it moves up into the list in
        // its own face once it is here (#2732).
        if let Some(family) = &choice.preview {
            crate::missing_fonts::fetch_family(family);
        }
        // A font this computer lacks draws in a fallback: say so, with
        // a way to replace it everywhere it is used (#1720).
        if !font_is_mixed && !crate::missing_fonts::installed(&display_font) {
            missing_font = Some(display_font.clone());
        }
        if let (Some(family), Some(weight)) = (&choice.family, choice.weight) {
            if !(has_selection && sel_range.0 < sel_range.1) {
                cmds.push(AppCommand::SetTitleFontVariation { clip_id, weight: Some(weight), axes: tb.font_axes.clone() });
            }
            let _ = family;
        }
        if let Some(family) = &choice.family {
            cmds.push(AppCommand::NoteRecentFont { family: family.clone() });
            crate::missing_fonts::fetch_family(family);
        }
        if let Some(family) = choice.family {
            if has_selection && sel_range.0 < sel_range.1 {
                cmds.push(AppCommand::ApplyTitleStyle {
                    clip_id,
                    range_start: sel_range.0,
                    range_end: sel_range.1,
                    property: "font_name".into(),
                    value: family,
                });
            } else {
                cmds.push(AppCommand::UpdateTitleData {
                    clip_id,
                    text: tb.text.clone(),
                    font_size: tb.font_size,
                    font_name: family,
                    font_style: font_style_to_u8(tb.font_style),
                    color: tb.color,
                });
            }
        }
        ui.add_space(8.0);
        let mut size = display_size;
        let size_resp = egui::Frame::none()
            .fill(theme.bg_raised)
            .rounding(4.0)
            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
            .show(ui, |ui| {
                ui.add(
                    egui::DragValue::new(&mut size)
                        .range(12.0..=300.0)
                        .speed(1.0)
                        .suffix(" pt"),
                )
            })
            .inner;
        let key = size_target.map(|_| {
            let ks = title_object_keyframe_state(&tb.font_size_keyframes, clip_time_secs, keyframe_tolerance);
            crate::param_focus::tag_keys(ui, &tb.font_size_keyframes);
            crate::widgets::scrubby_param::key_controls(ui, ks)
        });
        if has_selection && sel_range.0 < sel_range.1 {
            if size_resp.changed() {
                cmds.push(AppCommand::ApplyTitleStyle {
                    clip_id,
                    range_start: sel_range.0,
                    range_end: sel_range.1,
                    property: "font_size".into(),
                    value: format!("{size}"),
                });
            }
        } else {
            let edit = TextKeyEdit { toggled: key.is_some_and(|k| k.toggled), changed: size_resp.changed(), interpolation: None };
            handle_text_key_edit(cmds, clip_id, size_target, klipp_core::title_effect_keys::TEXT_SIZE, size, &tb.font_size_keyframes, clip_time_secs, keyframe_tolerance, edit,
                |font_size| AppCommand::UpdateTitleData {
                    clip_id,
                    text: tb.text.clone(),
                    font_size,
                    font_name: tb.font_name.clone(),
                    font_style: font_style_to_u8(tb.font_style),
                    color: tb.color,
                });
        }
        if size_is_mixed {
            ui.label(egui::RichText::new("Mixed").size(rows::TEXT).color(theme.text_secondary));
        }
    });
    if let Some(family) = missing_font {
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new(format!("\u{26a0} {family} is not installed: drawn in a fallback")).size(crate::theme::type_scale::CAPTION).color(theme.marker_warning));
            if ui.small_button("Replace\u{2026}").on_hover_text("Pick a font for every title using this one").clicked() {
                cmds.push(AppCommand::ShowMissingFonts { family: Some(family.clone()) });
            }
        });
    }
    ui.add_space(8.0);

    // 2a. The family's weights, and a variable face's axes as sliders
    //     (#1504); over a selection they set the selection's.
    show_title_weight_row(ui, theme, clip_id, tb, &display_font, display_style, has_selection.then_some(sel_range).filter(|s| s.0 < s.1), cmds);

    // 2b. OpenType features (#1710): the selection's, or the box's.
    title_features::show_title_features_row(ui, theme, clip_id, tb, &display_font, display_style, has_selection.then_some(sel_range).filter(|s| s.0 < s.1), cmds);

    // 3. B / I / U / S pill buttons (left) + L / C / R / J alignment
    //    pill buttons (right). Both groups share a consistent
    //    button height + inactive fill so the row reads as one
    //    coordinated control strip.
    //    Over a selection B and I set the selection's style (#1505),
    //    and U its underline (#1710).
    let is_bold = matches!(
        display_style,
        klipp_timeline::FontStyle::Bold | klipp_timeline::FontStyle::BoldItalic
    );
    let is_italic = matches!(
        display_style,
        klipp_timeline::FontStyle::Italic | klipp_timeline::FontStyle::BoldItalic
    );
    let set_style = |cmds: &mut Vec<AppCommand>, bold: bool, italic: bool| {
        let new_style = match (bold, italic) {
            (true, true) => klipp_timeline::FontStyle::BoldItalic,
            (true, false) => klipp_timeline::FontStyle::Bold,
            (false, true) => klipp_timeline::FontStyle::Italic,
            (false, false) => klipp_timeline::FontStyle::Regular,
        };
        if has_selection && sel_range.0 < sel_range.1 {
            cmds.push(AppCommand::ApplyTitleStyle {
                clip_id,
                range_start: sel_range.0,
                range_end: sel_range.1,
                property: "font_style".into(),
                value: serde_json::to_string(&new_style).unwrap_or_default(),
            });
        } else {
            cmds.push(AppCommand::UpdateTitleData {
                clip_id,
                text: tb.text.clone(),
                font_size: tb.font_size,
                font_name: tb.font_name.clone(),
                font_style: font_style_to_u8(new_style),
                color: tb.color,
            });
        }
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if pill_toggle_button(ui, theme, "B", is_bold, |r| r.strong().size(13.0)).clicked() {
            set_style(cmds, !is_bold, is_italic);
        }
        if pill_toggle_button(ui, theme, "I", is_italic, |r| r.italics().size(13.0)).clicked() {
            set_style(cmds, is_bold, !is_italic);
        }
        if pill_toggle_button(ui, theme, "U", display_underline && !underline_is_mixed, |r| r.underline().size(13.0)).clicked() {
            if has_selection && sel_range.0 < sel_range.1 {
                // A mixed selection is underlined throughout.
                cmds.push(AppCommand::ApplyTitleStyle {
                    clip_id,
                    range_start: sel_range.0,
                    range_end: sel_range.1,
                    property: "underline".into(),
                    value: (underline_is_mixed || !display_underline).to_string(),
                });
            } else {
                cmds.push(AppCommand::SetTitleUnderline { clip_id, underline: !tb.underline });
            }
        }
        if pill_toggle_button(ui, theme, "S", tb.strikethrough, |r| r.strikethrough().size(13.0))
            .clicked()
        {
            cmds.push(AppCommand::SetTitleStrikethrough {
                clip_id,
                strikethrough: !tb.strikethrough,
            });
        }
        // Superscript and subscript, for a selection (#1721): on the run
        // the selection starts in, a second press takes it off.
        if has_selection && sel_range.0 < sel_range.1 {
            let script = tb.style_runs.iter().find(|r| r.start <= sel_range.0 && sel_range.0 < r.end).and_then(|r| r.script);
            for (label, which, tip, value) in [
                ("x\u{00b2}", klipp_timeline::Script::Superscript, "Superscript", "superscript"),
                ("x\u{2082}", klipp_timeline::Script::Subscript, "Subscript", "subscript"),
            ] {
                let on = script == Some(which);
                if pill_toggle_button(ui, theme, label, on, |r| r.size(12.0)).on_hover_text(tip).clicked() {
                    cmds.push(AppCommand::ApplyTitleStyle {
                        clip_id,
                        range_start: sel_range.0,
                        range_end: sel_range.1,
                        property: "script".into(),
                        value: if on { "none".into() } else { value.into() },
                    });
                }
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let cur = match tb.alignment {
                klipp_timeline::TextAlignment::Justify => 3u8,
                klipp_timeline::TextAlignment::Right => 2,
                klipp_timeline::TextAlignment::Center => 1,
                klipp_timeline::TextAlignment::Left => 0,
            };
            // Right-to-left layout: order in the slice is J, R, C, L
            // because that's how they paint from the right edge
            // inward.
            for (val, icon) in [
                (3u8, Icon::TextAlignJustify),
                (2u8, Icon::TextAlignRight),
                (1u8, Icon::TextAlignCenter),
                (0u8, Icon::TextAlignLeft),
            ] {
                if pill_toggle_icon_button(ui, theme, icon, cur == val).clicked()
                    && cur != val
                {
                    cmds.push(AppCommand::SetTitleAlignment { clip_id, alignment: val });
                }
            }
        });
    });
    ui.add_space(8.0);

    // 3a. A selection's tracking and baseline offset (#1505); the
    //     box's own are in the Typography group.
    if has_selection && sel_range.0 < sel_range.1 {
        ui.horizontal(|ui| {
            if style_is_mixed {
                ui.label(egui::RichText::new("Mixed style").size(rows::TEXT).color(theme.text_secondary));
            }
            if underline_is_mixed {
                ui.label(egui::RichText::new("Mixed underline").size(rows::TEXT).color(theme.text_secondary));
            }
            let mut tracking = display_tracking;
            if ui.add(egui::DragValue::new(&mut tracking).range(-10.0..=50.0).speed(0.2).suffix(" px").prefix("Tracking: ").fixed_decimals(1)).changed() {
                cmds.push(AppCommand::ApplyTitleStyle {
                    clip_id,
                    range_start: sel_range.0,
                    range_end: sel_range.1,
                    property: "tracking".into(),
                    value: format!("{tracking}"),
                });
            }
            if tracking_is_mixed {
                ui.label(egui::RichText::new("Mixed").size(rows::TEXT).color(theme.text_secondary));
            }
            let mut baseline = display_baseline;
            if ui.add(egui::DragValue::new(&mut baseline).range(-200.0..=200.0).speed(0.5).suffix(" px").prefix("Raise: ").fixed_decimals(0))
                .on_hover_text("Moves the selection up off the line, or down with a negative value")
                .changed()
            {
                cmds.push(AppCommand::ApplyTitleStyle {
                    clip_id,
                    range_start: sel_range.0,
                    range_end: sel_range.1,
                    property: "baseline_offset".into(),
                    value: format!("{baseline}"),
                });
            }
            if baseline_is_mixed {
                ui.label(egui::RichText::new("Mixed").size(rows::TEXT).color(theme.text_secondary));
            }
        });
        // Its reading (ruby, #1704): set small above it, or right of a
        // vertical column; emptied, the gloss goes.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Ruby").size(rows::TEXT).color(theme.text_secondary));
            let mut gloss = display_ruby.clone();
            let edit = ui.add(egui::TextEdit::singleline(&mut gloss).hint_text("Reading").desired_width(160.0))
                .on_hover_text("A reading set small beside the selection: above it, or to the right of a vertical column");
            if edit.changed() {
                cmds.push(AppCommand::ApplyTitleStyle {
                    clip_id,
                    range_start: sel_range.0,
                    range_end: sel_range.1,
                    property: "ruby".into(),
                    value: gloss,
                });
            }
            if ruby_is_mixed {
                ui.label(egui::RichText::new("Mixed").size(rows::TEXT).color(theme.text_secondary));
            }
        });
        ui.add_space(8.0);
    }

    // 3b. Where the text sits up and down its box (left) and, for
    //     justified text, where each paragraph's last line goes
    //     (right) (#1506).
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let cur = match tb.vertical_align {
            klipp_timeline::TextVerticalAlign::Top => 0u8,
            klipp_timeline::TextVerticalAlign::Middle => 1,
            klipp_timeline::TextVerticalAlign::Bottom => 2,
        };
        for (val, label) in [(0u8, "Top"), (1u8, "Middle"), (2u8, "Bottom")] {
            if pill_toggle_button(ui, theme, label, cur == val, |r| r.size(11.0)).clicked() && cur != val {
                cmds.push(AppCommand::SetTitleVerticalAlign { clip_id, vertical_align: val });
            }
        }
        if tb.alignment == klipp_timeline::TextAlignment::Justify {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let cur = match tb.last_line {
                    klipp_timeline::LastLineAlign::Justify => 2u8,
                    klipp_timeline::LastLineAlign::Center => 1,
                    klipp_timeline::LastLineAlign::Left => 0,
                };
                for (val, icon) in [
                    (2u8, Icon::TextAlignJustify),
                    (1u8, Icon::TextAlignCenter),
                    (0u8, Icon::TextAlignLeft),
                ] {
                    if pill_toggle_icon_button(ui, theme, icon, cur == val)
                        .on_hover_text("Last line")
                        .clicked()
                        && cur != val
                    {
                        cmds.push(AppCommand::SetTitleLastLine { clip_id, last_line: val });
                    }
                }
                ui.label(egui::RichText::new("Last").size(rows::TEXT).color(theme.text_secondary));
            });
        }
    });
    ui.add_space(4.0);

    // 3c. What a text too long for its box does (#1722): wraps and sets
    //     smaller, keeps its line and shrinks to a minimum, or grows the
    //     box (and a backing fitted to it).
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (cur, min) = match tb.fit {
            klipp_timeline::TextFit::Wrap => (0u8, 24.0),
            klipp_timeline::TextFit::Shrink { min_size } => (1, min_size),
            klipp_timeline::TextFit::Grow => (2, 24.0),
        };
        ui.label(egui::RichText::new("Fit").size(rows::TEXT).color(theme.text_secondary));
        for (val, label, tip) in [
            (0u8, "Wrap", "Long text wraps onto more lines, set smaller if it would be taller than the box."),
            (1u8, "Shrink", "Long text keeps its lines and is set smaller to fit the box, down to the minimum size."),
            (2u8, "Grow", "Long text wraps at its size and the box grows with it; a backing fitted to the text grows too."),
        ] {
            if pill_toggle_button(ui, theme, label, cur == val, |r| r.size(11.0)).on_hover_text(tip).clicked() && cur != val {
                cmds.push(AppCommand::SetTitleFit { clip_id, fit: val, min_size: min });
            }
        }
        if cur == 1 {
            let mut m = min;
            let resp = ui
                .add(egui::DragValue::new(&mut m).range(4.0..=1000.0).speed(0.5).fixed_decimals(0).prefix("min "))
                .on_hover_text("The smallest the text is set, in the same units as its size");
            if resp.drag_started() {
                cmds.push(AppCommand::BeginTitleObjectDrag { clip_id });
            }
            if resp.changed() && m != min {
                cmds.push(AppCommand::SetTitleFit { clip_id, fit: 1, min_size: m });
            }
            if resp.drag_stopped() {
                cmds.push(AppCommand::EndTitleObjectDrag { clip_id });
            }
        }
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
    let mut color_key_toggled = false;
    ui.horizontal(|ui| {
        // Larger swatch to match the reference (~44x28 px). egui's
        // default color_edit_button is tiny; bumping interact_size
        // in a scoped Ui sizes the swatch up without custom popup code.
        let swatch_changed = ui
            .scope(|ui| {
                ui.spacing_mut().interact_size = egui::vec2(44.0, 28.0);
                ui.color_edit_button_srgba(&mut col).changed()
            })
            .inner;
        if swatch_changed {
            color_changed = true;
        }
        ui.add_space(6.0);
        let mut opacity_pct = ((display_color[3] as f32 / 255.0) * 100.0).round();
        // Right to left from the row's edge (#1916): the key mark, "Mixed",
        // the % readout, then the slider in what is left, so the row fits
        // the inspector at any width; budgeting the slider first left the
        // mark out and pushed every row below past the panel's edge.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if size_target.is_some() {
                let ks = title_object_keyframe_state(&tb.color_keyframes.r, clip_time_secs, keyframe_tolerance);
                crate::param_focus::tag_keys(ui, &tb.color_keyframes.r);
                color_key_toggled = crate::widgets::scrubby_param::key_controls(ui, ks).toggled;
            }
            if color_is_mixed {
                ui.label(egui::RichText::new("Mixed").size(rows::TEXT).color(theme.text_secondary));
            }
            egui::Frame::none()
                .fill(theme.bg_raised)
                .rounding(4.0)
                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(format!("{}%", opacity_pct as u32))
                            .size(11.0)
                            .color(theme.text_secondary),
                    );
                });
            ui.add_space(8.0);
            // Slider sits in a framed pill matching the % readout beside
            // it, so the swatch / slider / % row reads as three
            // equal-height components.
            let slider_outer_w = ui.available_width().max(36.0);
            let slider_changed = egui::Frame::none()
                .fill(theme.bg_raised)
                .rounding(4.0)
                .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                .show(ui, |ui| {
                    ui.set_width(slider_outer_w - 16.0);
                    ui.spacing_mut().slider_width = slider_outer_w - 16.0;
                    ui.add(
                        egui::Slider::new(&mut opacity_pct, 0.0..=100.0)
                            .step_by(1.0)
                            .show_value(false),
                    )
                    .changed()
                })
                .inner;
            if slider_changed {
                new_alpha_u8 = Some((opacity_pct / 100.0 * 255.0).round() as u8);
            }
        });
    });
    if let Some(a) = new_alpha_u8 {
        col = egui::Color32::from_rgba_premultiplied(col.r(), col.g(), col.b(), a);
        color_changed = true;
    }
    let keyed_colour = size_target.filter(|_| color_key_toggled || (color_changed && !tb.color_keyframes.is_empty()));
    if let Some(oid) = keyed_colour {
        let new_rgba = [col.r(), col.g(), col.b(), col.a()];
        let at_key = keyframe_selection::nearest(&tb.color_keyframes.r, clip_time_secs, keyframe_tolerance).is_some();
        cmds.push(AppCommand::SetTitleObjectColourKeyframe {
            clip_id,
            object_id: oid.0,
            param_key: klipp_core::title_effect_keys::TEXT_COLOR.into(),
            time_secs: clip_time_secs,
            color: (!(color_key_toggled && at_key)).then_some(new_rgba),
        });
    } else if color_changed {
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
    ui.add_space(4.0);
}

/// The weight row of the Title tab (#1504): a picker of the weights
/// `family` has (the style's own, "By style", first), and a slider for
/// each axis of its variable face. With `selection` the weight and
/// axes set are the selection's, through `ApplyTitleStyle`; without,
/// the text box's. Nothing for a family of one weight and no axes.
#[allow(clippy::too_many_arguments)]
fn show_title_weight_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    tb: &klipp_timeline::TextBox,
    family: &str,
    style: klipp_timeline::FontStyle,
    selection: Option<(usize, usize)>,
    cmds: &mut Vec<AppCommand>,
) {
    use klipp_title::font_discovery::{discover_system_fonts, family_faces, weight_name};
    let registry = discover_system_fonts();
    let italic = matches!(style, klipp_timeline::FontStyle::Italic | klipp_timeline::FontStyle::BoldItalic);
    let faces = family_faces(&registry, family);
    let mut weights: Vec<u16> = faces.iter().filter(|f| f.italic == italic).map(|f| f.weight).collect();
    if weights.is_empty() {
        weights = faces.iter().map(|f| f.weight).collect();
    }
    weights.dedup();
    let axes: Vec<klipp_title::variable::VariationAxis> = registry
        .get(family)
        .and_then(|infos| infos.iter().find(|i| !i.axes.is_empty() && i.italic == italic).or_else(|| infos.iter().find(|i| !i.axes.is_empty())))
        .map(|i| i.axes.clone())
        .unwrap_or_default();
    if weights.len() < 2 && axes.is_empty() {
        return;
    }
    let (weight, weight_mixed) = match selection {
        Some(sel) => selection_run_value(tb, sel, |r| r.font_weight.map(Some), tb.font_weight),
        None => (tb.font_weight, false),
    };
    let axis_value = |tag: &str| -> (Option<f32>, bool) {
        let of = |list: &[klipp_timeline::FontAxis]| list.iter().rev().find(|(t, _)| t == tag).map(|(_, v)| *v);
        match selection {
            Some(sel) => selection_run_value(tb, sel, |r| of(&r.font_axes).map(Some), of(&tb.font_axes)),
            None => (of(&tb.font_axes), false),
        }
    };
    let set_weight = |cmds: &mut Vec<AppCommand>, w: Option<u16>| match selection {
        // A run cannot go back to its style's weight once it names
        // one; "By style" over a selection names the style's.
        Some(sel) => cmds.push(AppCommand::ApplyTitleStyle {
            clip_id,
            range_start: sel.0,
            range_end: sel.1,
            property: "font_weight".into(),
            value: w.unwrap_or(if matches!(style, klipp_timeline::FontStyle::Bold | klipp_timeline::FontStyle::BoldItalic) { 700 } else { 400 }).to_string(),
        }),
        None => cmds.push(AppCommand::SetTitleFontVariation { clip_id, weight: w, axes: tb.font_axes.clone() }),
    };
    let set_axis = |cmds: &mut Vec<AppCommand>, tag: &str, value: f32| match selection {
        Some(sel) => cmds.push(AppCommand::ApplyTitleStyle {
            clip_id,
            range_start: sel.0,
            range_end: sel.1,
            property: "font_axes".into(),
            value: serde_json::to_string(&[(tag, value)]).unwrap_or_default(),
        }),
        None => {
            let mut axes: Vec<klipp_timeline::FontAxis> = tb.font_axes.iter().filter(|(t, _)| t != tag).cloned().collect();
            axes.push((tag.to_string(), value));
            cmds.push(AppCommand::SetTitleFontVariation { clip_id, weight: tb.font_weight, axes });
        }
    };
    if weights.len() >= 2 || axes.iter().any(|a| a.tag == "wght") {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Weight").size(rows::TEXT).color(theme.text_secondary));
            let label = |w: Option<u16>| w.map_or_else(|| "By style".to_string(), |w| format!("{} {w}", weight_name(w)));
            let mut picked: Option<Option<u16>> = None;
            egui::ComboBox::from_id_salt("title_pinned_weight")
                .selected_text(if weight_mixed { "Mixed".to_string() } else { label(weight) })
                .show_ui(ui, |ui| {
                    if ui.selectable_label(weight.is_none(), label(None)).clicked() {
                        picked = Some(None);
                    }
                    for &w in &weights {
                        if ui.selectable_label(weight == Some(w), label(Some(w))).clicked() {
                            picked = Some(Some(w));
                        }
                    }
                });
            if let Some(w) = picked {
                set_weight(cmds, w);
            }
        });
    }
    for axis in &axes {
        let (value, mixed) = axis_value(&axis.tag);
        // The weight axis follows the weight named, else the axis's default.
        let shown = value.or_else(|| (axis.tag == "wght").then(|| weight.map(f32::from)).flatten()).unwrap_or(axis.default);
        let mut v = shown;
        ui.horizontal(|ui| {
            let name = match axis.tag.as_str() {
                "wght" => "Weight axis",
                "wdth" => "Width",
                "slnt" => "Slant",
                "ital" => "Italic",
                "opsz" => "Optical size",
                other => other,
            };
            if ui.add(egui::Slider::new(&mut v, axis.min..=axis.max).text(name)).changed() {
                set_axis(cmds, &axis.tag, v);
            }
            if mixed {
                ui.label(egui::RichText::new("Mixed").size(rows::TEXT).color(theme.text_secondary));
            }
        });
    }
    ui.add_space(8.0);
}

/// The value a text selection shows for one style-run field, and
/// whether the selection mixes values: each run over `sel`'s value of
/// the field, or the box's `base` where the run leaves it, as the size
/// and colour of a selection are read (#1505).
fn selection_run_value<T: Clone + PartialEq>(
    tb: &klipp_timeline::TextBox,
    sel: (usize, usize),
    field: impl Fn(&klipp_timeline::StyleRun) -> Option<T>,
    base: T,
) -> (T, bool) {
    let values: Vec<T> = tb
        .style_runs
        .iter()
        .filter(|run| run.start < sel.1 && run.end > sel.0)
        .map(|run| field(run).unwrap_or_else(|| base.clone()))
        .collect();
    match values.first() {
        None => (base, false),
        Some(first) => (first.clone(), values.iter().any(|v| v != first)),
    }
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
            .size(rows::TEXT)
            .color(theme.text_secondary)
            .strong(),
    );
    ui.add_space(2.0);

    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Quick").size(rows::TEXT).color(theme.text_secondary));
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
                egui::Button::new(egui::RichText::new(*label).size(10.0).color(
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
            ui.label(egui::RichText::new("Effects").size(rows::TEXT).color(theme.text_secondary));
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
                    .size(10.0).color(theme.text_dim),
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
/// The layer's stack — glow, outline, drop shadow, inner shadow and
/// their secondary layers — for any layer, and the fills (gradient,
/// pattern, texture) when the layer is a text: `tb` is `None` for a
/// shape or a picture, which has a face of its own.
fn show_title_effects_group(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    tb: Option<&klipp_timeline::TextBox>,
    fx: &klipp_timeline::LayerFx,
    cmds: &mut Vec<AppCommand>,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    clip_time_secs: f64,
    keyframe_tolerance: f64,
) {
    use crate::widgets::inspector_group::{inspector_subgroup_card, GroupStatus};

    let glow_enabled = fx.glows.first().is_some_and(|g| g.enabled);
    let outline_enabled = fx.outlines.first().is_some_and(|o| o.enabled);
    let shadow_enabled = fx.shadows.first().is_some_and(|s| s.enabled);
    let inner_enabled = fx.inner_shadows.first().is_some_and(|i| i.enabled);
    let gradient_enabled = tb.is_some_and(|tb| tb.gradient.as_ref().is_some_and(|g| g.enabled));
    let pattern_enabled = tb.is_some_and(|tb| tb.pattern_fill.is_some() && tb.pattern_fill_enabled);
    // Two distinct concepts for Texture Fill:
    //   - `texture_toggle_on` drives the subcard toggle. Reflects
    //     just the user-set flag so clicking the toggle round-trips
    //     even when no file has been picked yet (the toggle would
    //     otherwise snap back to off because the path is None).
    //   - `texture_active` is the strict "actually rendering"
    //     predicate used by the active-count badge + summary chip
    //     so a toggle-on without a file isn't counted as a live
    //     effect.
    let texture_toggle_on = tb.is_some_and(|tb| tb.texture_fill.as_ref().is_some_and(|t| t.enabled));
    let texture_active = tb.is_some_and(|tb| tb.texture_fill.as_ref().is_some_and(|t| t.is_active()));

    let mut active_count: u32 = 0;
    for on in [
        glow_enabled, outline_enabled, shadow_enabled, inner_enabled,
        gradient_enabled, pattern_enabled, texture_active,
    ] {
        if on { active_count += 1; }
    }
    if fx.outlines.len() > 1 { active_count += 1; }

    let status = GroupStatus {
        count: if active_count > 0 { Some(active_count) } else { None },
        active: active_count > 0,
    };
    if !title_group_is_open(ui, theme, clip_id, "Text effects", status) {
        return;
    }
    ui.add_space(6.0);

    // 1. Glow — primary at glows[0]. Stack rows for glows[1..] live
    // in subcard #2 (Glow Layers).
    {
        let cur = fx.glows.first().cloned().unwrap_or(klipp_timeline::TextGlow {
            color: [0, 200, 255, 255],
            radius: 10.0,
            intensity: 0.8,
            enabled: false,
            radius_keyframes: Vec::new(),
            intensity_keyframes: Vec::new(),
        });
        let mut enabled = glow_enabled;
        let display_radius = cur.evaluated_radius(clip_time_secs);
        let display_intensity = cur.evaluated_intensity(clip_time_secs);
        let summary = if enabled {
            format!("{} px · {}%", display_radius.round() as u32, (display_intensity * 100.0).round() as u32)
        } else { String::new() };
        let kfs_radius: &[klipp_core::Keyframe] = fx
            .glows
            .first()
            .map(|g| g.radius_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_intensity: &[klipp_core::Keyframe] = fx
            .glows
            .first()
            .map(|g| g.intensity_keyframes.as_slice())
            .unwrap_or(&[]);
        inspector_subgroup_card(ui, "Glow", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut radius = display_radius;
            let mut intensity = display_intensity;
            let mut col = egui::Color32::from_rgba_premultiplied(cur.color[0], cur.color[1], cur.color[2], cur.color[3]);
            let color_changed = effect_color_row(ui, theme, "Colour", &mut col, ("glow", clip_id));
            let pill_radius = format!("{} px", radius.round() as i32);
            let resp_radius = effect_param_row(
                ui, theme, "Radius", &mut radius, 1.0..=40.0,
                &pill_radius, kfs_radius,
            );
            let pill_intensity = format!("{}%", (intensity * 100.0).round() as i32);
            let resp_intensity = effect_param_row(
                ui, theme, "Intensity", &mut intensity, 0.0..=1.0,
                &pill_intensity, kfs_intensity,
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
                keyframe_tolerance,
            );
            handle_glow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::GLOW_INTENSITY,
                intensity, &cur, kfs_intensity, clip_time_secs, &resp_intensity,
                keyframe_tolerance,
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

    // 2. Glow Layers — secondary glow stack (`glows[1..]`).
    // Mirrors Drop Shadow Layers; per-row color + radius + intensity
    // DragValues + × remove + +Add Layer.
    {
        let mut enabled = fx.glow_stack_enabled;
        let layer_count = fx.glows.len().saturating_sub(1);
        let summary = if enabled && layer_count > 0 {
            format!("{} layer{}", layer_count, if layer_count == 1 { "" } else { "s" })
        } else {
            String::new()
        };
        inspector_subgroup_card(ui, "Glow Layers", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let glows = &fx.glows;
            let mut structural_change: Option<Vec<klipp_timeline::TextGlow>> = None;
            for i in 1..glows.len() {
                let g = glows[i].clone();
                let display_radius = g.evaluated_radius(clip_time_secs);
                let display_intensity = g.evaluated_intensity(clip_time_secs);
                let kfs_r: &[klipp_core::Keyframe] = g.radius_keyframes.as_slice();
                let kfs_i: &[klipp_core::Keyframe] = g.intensity_keyframes.as_slice();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("#{}", i)).size(rows::TEXT).color(theme.text_secondary));
                    let mut col = egui::Color32::from_rgba_premultiplied(g.color[0], g.color[1], g.color[2], g.color[3]);
                    if ui.color_edit_button_srgba(&mut col).changed() {
                        cmds.push(AppCommand::SetTitleGlowLayer {
                            clip_id,
                            index: i as u32,
                            enabled: g.enabled,
                            color: [col.r(), col.g(), col.b(), col.a()],
                            radius: g.radius,
                            intensity: g.intensity,
                        });
                    }
                    let mut radius = display_radius;
                    crate::param_focus::tag_keys(ui, kfs_r);
                    let r_radius = rows::param_row(
                        ui, theme, "r", &mut radius, 1.0..=80.0, 10.0, 0.5, 1, " px",
                        target_oid.map(|_| title_object_keyframe_state(kfs_r, clip_time_secs, keyframe_tolerance)),
                    );
                    handle_glow_layer_axis_response(
                        cmds, clip_id, target_oid, i, "radius",
                        radius, &g, kfs_r, clip_time_secs, &r_radius,
                        keyframe_tolerance,
                    );
                    let mut intensity = display_intensity;
                    crate::param_focus::tag_keys(ui, kfs_i);
                    let r_intensity = rows::param_row(
                        ui, theme, "i", &mut intensity, 0.0..=2.0, 1.0, 0.02, 2, "",
                        target_oid.map(|_| title_object_keyframe_state(kfs_i, clip_time_secs, keyframe_tolerance)),
                    );
                    handle_glow_layer_axis_response(
                        cmds, clip_id, target_oid, i, "intensity",
                        intensity, &g, kfs_i, clip_time_secs, &r_intensity,
                        keyframe_tolerance,
                    );
                    if ui.small_button("\u{00D7}").clicked() {
                        let mut new_vec = glows.clone();
                        new_vec.remove(i);
                        structural_change = Some(new_vec);
                    }
                });
            }
            if ui.small_button("+ Add Layer").clicked() {
                let mut new_vec = glows.clone();
                if new_vec.is_empty() {
                    new_vec.push(klipp_timeline::TextGlow {
                        color: [0, 200, 255, 255],
                        radius: 10.0,
                        intensity: 0.8,
                        enabled: false,
                        radius_keyframes: Vec::new(),
                        intensity_keyframes: Vec::new(),
                    });
                }
                new_vec.push(klipp_timeline::TextGlow {
                    color: [255, 255, 255, 255],
                    radius: 20.0,
                    intensity: 0.6,
                    enabled: true,
                    radius_keyframes: Vec::new(),
                    intensity_keyframes: Vec::new(),
                });
                structural_change = Some(new_vec);
            }
            if let Some(new_glows) = structural_change {
                let tuples: Vec<([u8; 4], f32, f32)> = new_glows
                    .iter()
                    .map(|g| (g.color, g.radius, g.intensity))
                    .collect();
                cmds.push(AppCommand::SetTitleGlows { clip_id, glows: tuples });
            }
        });
        if enabled != fx.glow_stack_enabled {
            cmds.push(AppCommand::SetTitleGlowStackEnabled { clip_id, enabled });
        }
    }
    ui.add_space(6.0);

    // 4. Outline (singular = outlines[0])
    {
        let cur = fx.outlines.first().cloned().unwrap_or(klipp_timeline::TextOutline {
            color: [255, 255, 255, 255],
            width: 2.0,
            enabled: false,
            width_keyframes: Vec::new(),
        });
        let mut enabled = outline_enabled;
        let display_width = cur.evaluated_width(clip_time_secs);
        let summary = if enabled { format!("{} px", display_width.round() as u32) } else { String::new() };
        let kfs_width: &[klipp_core::Keyframe] = fx
            .outlines
            .first()
            .map(|o| o.width_keyframes.as_slice())
            .unwrap_or(&[]);
        inspector_subgroup_card(ui, "Outline", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut width = display_width;
            let mut col = egui::Color32::from_rgba_premultiplied(cur.color[0], cur.color[1], cur.color[2], cur.color[3]);
            let color_changed = effect_color_row(ui, theme, "Colour", &mut col, ("outline", clip_id));
            let pill_width = format!("{} px", width.round() as i32);
            let resp_width = effect_param_row(
                ui, theme, "Width", &mut width, 0.5..=12.0,
                &pill_width, kfs_width,
            );
            // Color always goes through the static SetTitleOutline —
            // not keyframable in this slice.
            if color_changed {
                cmds.push(AppCommand::SetTitleOutline {
                    clip_id, enabled: true,
                    color: [col.r(), col.g(), col.b(), col.a()], width: cur.width,
                });
            }
            handle_outline_axis_response(
                cmds, clip_id, target_oid,
                0,
                width, &cur, kfs_width, clip_time_secs, &resp_width,
                keyframe_tolerance,
            );
        });
        if enabled != outline_enabled {
            cmds.push(AppCommand::SetTitleOutline {
                clip_id, enabled, color: cur.color, width: cur.width,
            });
        }
    }
    ui.add_space(6.0);

    // 5. Outline Layers — soft-disable card hosting the secondary
    // outline stack (`outlines[1..]`). Primary outline (#2 above) is
    // gated by its own toggle and unaffected by this card. Each row
    // is keyframe-aware — width edits dispatch through the per-effect
    // path when keyframes exist, the per-index SetTitleOutlineLayer
    // path otherwise — so structural ops (add / remove) and value
    // ops (color / width) preserve siblings' keyframes correctly.
    {
        let mut enabled = fx.outline_stack_enabled;
        let layer_count = fx.outlines.len().saturating_sub(1);
        let summary = if enabled && layer_count > 0 {
            format!("{} layer{}", layer_count, if layer_count == 1 { "" } else { "s" })
        } else {
            String::new()
        };
        inspector_subgroup_card(ui, "Outline Layers", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let outlines = &fx.outlines;
            let mut structural_change: Option<Vec<klipp_timeline::TextOutline>> = None;
            for i in 1..outlines.len() {
                let ol = outlines[i].clone();
                let display_width = ol.evaluated_width(clip_time_secs);
                let kfs_width: &[klipp_core::Keyframe] = ol.width_keyframes.as_slice();
                let ks_width = title_object_keyframe_state(kfs_width, clip_time_secs, keyframe_tolerance);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("#{}", i)).size(rows::TEXT).color(theme.text_secondary));
                    let mut col = egui::Color32::from_rgba_premultiplied(ol.color[0], ol.color[1], ol.color[2], ol.color[3]);
                    if ui.color_edit_button_srgba(&mut col).changed() {
                        cmds.push(AppCommand::SetTitleOutlineLayer {
                            clip_id,
                            index: i as u32,
                            enabled: ol.enabled,
                            color: [col.r(), col.g(), col.b(), col.a()],
                            width: ol.width,
                        });
                    }
                    let mut w_val = display_width;
                    crate::param_focus::tag_keys(ui, kfs_width);
                    let resp_width = rows::param_row(
                        ui, theme, "", &mut w_val, 0.5..=20.0, 4.0, 0.5, 1, " px",
                        target_oid.map(|_| ks_width),
                    );
                    handle_outline_axis_response(
                        cmds, clip_id, target_oid,
                        i,
                        w_val, &ol, kfs_width, clip_time_secs, &resp_width,
                        keyframe_tolerance,
                    );
                    if ui.small_button("\u{00D7}").clicked() {
                        let mut new_vec = outlines.clone();
                        new_vec.remove(i);
                        structural_change = Some(new_vec);
                    }
                });
            }
            if ui.small_button("+ Add Layer").clicked() {
                let mut new_vec = outlines.clone();
                // Seed a primary outline at index 0 if it's missing,
                // so the new stacked layer doesn't shadow-promote into
                // the primary slot. The seed is enabled=false so it
                // doesn't render until the user touches the primary
                // Outline subcard.
                if new_vec.is_empty() {
                    new_vec.push(klipp_timeline::TextOutline {
                        color: [255, 255, 255, 255],
                        width: 2.0,
                        enabled: false,
                        width_keyframes: Vec::new(),
                    });
                }
                new_vec.push(klipp_timeline::TextOutline {
                    color: [0, 0, 0, 255],
                    width: 4.0,
                    enabled: true,
                    width_keyframes: Vec::new(),
                });
                structural_change = Some(new_vec);
            }
            if let Some(new_outlines) = structural_change {
                let outline_tuples: Vec<([u8; 4], f32)> =
                    new_outlines.iter().map(|o| (o.color, o.width)).collect();
                cmds.push(AppCommand::SetTitleOutlines { clip_id, outlines: outline_tuples });
            }
        });
        if enabled != fx.outline_stack_enabled {
            cmds.push(AppCommand::SetTitleOutlineStackEnabled { clip_id, enabled });
        }
    }
    ui.add_space(6.0);

    // 6. Drop Shadow — primary at shadows[0]. Keyframe-aware sliders
    // for offset/blur on the primary; colour is static. Stack rows
    // for shadows[1..] live in subcard #5 (Drop Shadow Layers).
    {
        let cur = fx.shadows.first().cloned().unwrap_or(klipp_timeline::TitleShadow {
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
        let kfs_ox: &[klipp_core::Keyframe] = fx
            .shadows
            .first()
            .map(|s| s.offset_x_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_oy: &[klipp_core::Keyframe] = fx
            .shadows
            .first()
            .map(|s| s.offset_y_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_blur: &[klipp_core::Keyframe] = fx
            .shadows
            .first()
            .map(|s| s.blur_keyframes.as_slice())
            .unwrap_or(&[]);
        inspector_subgroup_card(ui, "Drop Shadow", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut ox = display_ox;
            let mut oy = display_oy;
            let mut blur = display_blur;
            let mut col = egui::Color32::from_rgba_premultiplied(cur.color[0], cur.color[1], cur.color[2], cur.color[3]);
            let color_changed = effect_color_row(ui, theme, "Colour", &mut col, ("drop_shadow", clip_id));
            let pill_ox = format!("{} px", ox.round() as i32);
            let resp_ox = effect_param_row(
                ui, theme, "Offset X", &mut ox, -20.0..=20.0,
                &pill_ox, kfs_ox,
            );
            let pill_oy = format!("{} px", oy.round() as i32);
            let resp_oy = effect_param_row(
                ui, theme, "Offset Y", &mut oy, -20.0..=20.0,
                &pill_oy, kfs_oy,
            );
            let pill_blur = format!("{} px", blur.round() as i32);
            let resp_blur = effect_param_row(
                ui, theme, "Blur", &mut blur, 0.0..=30.0,
                &pill_blur, kfs_blur,
            );
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
                keyframe_tolerance,
            );
            handle_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::SHADOW_OFFSET_Y,
                oy, &cur, kfs_oy, clip_time_secs, &resp_oy,
                keyframe_tolerance,
            );
            handle_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::SHADOW_BLUR,
                blur, &cur, kfs_blur, clip_time_secs, &resp_blur,
                keyframe_tolerance,
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

    // 7. Drop Shadow Layers — secondary stack (`shadows[1..]`).
    // Each row uses keyframe-aware scrubby sliders so per-index
    // offset/blur keyframes work the same as the primary card.
    {
        let mut enabled = fx.shadow_stack_enabled;
        let layer_count = fx.shadows.len().saturating_sub(1);
        let summary = if enabled && layer_count > 0 {
            format!("{} layer{}", layer_count, if layer_count == 1 { "" } else { "s" })
        } else {
            String::new()
        };
        inspector_subgroup_card(ui, "Drop Shadow Layers", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let shadows = &fx.shadows;
            let mut structural_change: Option<Vec<klipp_timeline::TitleShadow>> = None;
            for i in 1..shadows.len() {
                let sh = shadows[i].clone();
                let display_ox = sh.evaluated_offset_x(clip_time_secs);
                let display_oy = sh.evaluated_offset_y(clip_time_secs);
                let display_bl = sh.evaluated_blur(clip_time_secs);
                let kfs_ox: &[klipp_core::Keyframe] = sh.offset_x_keyframes.as_slice();
                let kfs_oy: &[klipp_core::Keyframe] = sh.offset_y_keyframes.as_slice();
                let kfs_bl: &[klipp_core::Keyframe] = sh.blur_keyframes.as_slice();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("#{}", i)).size(rows::TEXT).color(theme.text_secondary));
                    let mut col = egui::Color32::from_rgba_premultiplied(sh.color[0], sh.color[1], sh.color[2], sh.color[3]);
                    if ui.color_edit_button_srgba(&mut col).changed() {
                        cmds.push(AppCommand::SetTitleShadowLayer {
                            clip_id,
                            index: i as u32,
                            enabled: sh.enabled,
                            color: [col.r(), col.g(), col.b(), col.a()],
                            offset_x: sh.offset_x,
                            offset_y: sh.offset_y,
                            blur: sh.blur,
                        });
                    }
                    let mut ox = display_ox;
                    crate::param_focus::tag_keys(ui, kfs_ox);
                    let r_ox = rows::param_row(
                        ui, theme, "x", &mut ox, -20.0..=20.0, 4.0, 0.5, 1, " px",
                        target_oid.map(|_| title_object_keyframe_state(kfs_ox, clip_time_secs, keyframe_tolerance)),
                    );
                    handle_shadow_layer_axis_response(
                        cmds, clip_id, target_oid, i, "offset_x",
                        ox, &sh, kfs_ox, clip_time_secs, &r_ox,
                        keyframe_tolerance,
                    );
                    let mut oy = display_oy;
                    crate::param_focus::tag_keys(ui, kfs_oy);
                    let r_oy = rows::param_row(
                        ui, theme, "y", &mut oy, -20.0..=20.0, 4.0, 0.5, 1, " px",
                        target_oid.map(|_| title_object_keyframe_state(kfs_oy, clip_time_secs, keyframe_tolerance)),
                    );
                    handle_shadow_layer_axis_response(
                        cmds, clip_id, target_oid, i, "offset_y",
                        oy, &sh, kfs_oy, clip_time_secs, &r_oy,
                        keyframe_tolerance,
                    );
                    let mut bl = display_bl;
                    crate::param_focus::tag_keys(ui, kfs_bl);
                    let r_bl = rows::param_row(
                        ui, theme, "blur", &mut bl, 0.0..=60.0, 4.0, 0.5, 1, " px",
                        target_oid.map(|_| title_object_keyframe_state(kfs_bl, clip_time_secs, keyframe_tolerance)),
                    );
                    handle_shadow_layer_axis_response(
                        cmds, clip_id, target_oid, i, "blur",
                        bl, &sh, kfs_bl, clip_time_secs, &r_bl,
                        keyframe_tolerance,
                    );
                    if ui.small_button("\u{00D7}").clicked() {
                        let mut new_vec = shadows.clone();
                        new_vec.remove(i);
                        structural_change = Some(new_vec);
                    }
                });
            }
            if ui.small_button("+ Add Layer").clicked() {
                let mut new_vec = shadows.clone();
                // Seed a primary shadow at index 0 if missing so the
                // new stacked layer doesn't promote into the primary
                // slot. The seed is enabled=false so it doesn't render
                // until the user enables the primary Drop Shadow card.
                if new_vec.is_empty() {
                    new_vec.push(klipp_timeline::TitleShadow {
                        color: [0, 0, 0, 180],
                        offset_x: 3.0,
                        offset_y: 3.0,
                        blur: 0.0,
                        enabled: false,
                        offset_x_keyframes: Vec::new(),
                        offset_y_keyframes: Vec::new(),
                        blur_keyframes: Vec::new(),
                    });
                }
                new_vec.push(klipp_timeline::TitleShadow {
                    color: [0, 0, 0, 200],
                    offset_x: 6.0,
                    offset_y: 6.0,
                    blur: 4.0,
                    enabled: true,
                    offset_x_keyframes: Vec::new(),
                    offset_y_keyframes: Vec::new(),
                    blur_keyframes: Vec::new(),
                });
                structural_change = Some(new_vec);
            }
            if let Some(new_shadows) = structural_change {
                let tuples: Vec<([u8; 4], f32, f32, f32)> = new_shadows
                    .iter()
                    .map(|s| (s.color, s.offset_x, s.offset_y, s.blur))
                    .collect();
                cmds.push(AppCommand::SetTitleShadows { clip_id, shadows: tuples });
            }
        });
        if enabled != fx.shadow_stack_enabled {
            cmds.push(AppCommand::SetTitleShadowStackEnabled { clip_id, enabled });
        }
    }
    ui.add_space(6.0);

    // 8. Inner Shadow — primary at inner_shadows[0]. Stack rows for
    // inner_shadows[1..] live in subcard #7 (Inner Shadow Layers).
    {
        let cur = fx.inner_shadows.first().cloned().unwrap_or(klipp_timeline::InnerShadow {
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
        let kfs_ox: &[klipp_core::Keyframe] = fx
            .inner_shadows
            .first()
            .map(|s| s.offset_x_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_oy: &[klipp_core::Keyframe] = fx
            .inner_shadows
            .first()
            .map(|s| s.offset_y_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_blur: &[klipp_core::Keyframe] = fx
            .inner_shadows
            .first()
            .map(|s| s.blur_keyframes.as_slice())
            .unwrap_or(&[]);
        inspector_subgroup_card(ui, "Inner Shadow", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let mut ox = display_ox;
            let mut oy = display_oy;
            let mut blur = display_blur;
            let mut col = egui::Color32::from_rgba_premultiplied(cur.color[0], cur.color[1], cur.color[2], cur.color[3]);
            let color_changed = effect_color_row(ui, theme, "Colour", &mut col, ("inner_shadow", clip_id));
            let pill_ox = format!("{} px", ox.round() as i32);
            let resp_ox = effect_param_row(
                ui, theme, "Offset X", &mut ox, -20.0..=20.0,
                &pill_ox, kfs_ox,
            );
            let pill_oy = format!("{} px", oy.round() as i32);
            let resp_oy = effect_param_row(
                ui, theme, "Offset Y", &mut oy, -20.0..=20.0,
                &pill_oy, kfs_oy,
            );
            let pill_blur = format!("{} px", blur.round() as i32);
            let resp_blur = effect_param_row(
                ui, theme, "Blur", &mut blur, 0.0..=20.0,
                &pill_blur, kfs_blur,
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
                keyframe_tolerance,
            );
            handle_inner_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::INNER_SHADOW_OFFSET_Y,
                oy, &cur, kfs_oy, clip_time_secs, &resp_oy,
                keyframe_tolerance,
            );
            handle_inner_shadow_axis_response(
                cmds, clip_id, target_oid,
                klipp_core::title_effect_keys::INNER_SHADOW_BLUR,
                blur, &cur, kfs_blur, clip_time_secs, &resp_blur,
                keyframe_tolerance,
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

    // 9. Inner Shadow Layers — secondary stack (`inner_shadows[1..]`).
    // Mirrors Drop Shadow Layers in shape and behaviour.
    {
        let mut enabled = fx.inner_shadow_stack_enabled;
        let layer_count = fx.inner_shadows.len().saturating_sub(1);
        let summary = if enabled && layer_count > 0 {
            format!("{} layer{}", layer_count, if layer_count == 1 { "" } else { "s" })
        } else {
            String::new()
        };
        inspector_subgroup_card(ui, "Inner Shadow Layers", &mut enabled,
            (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
            let inners = &fx.inner_shadows;
            let mut structural_change: Option<Vec<klipp_timeline::InnerShadow>> = None;
            for i in 1..inners.len() {
                let sh = inners[i].clone();
                let display_ox = sh.evaluated_offset_x(clip_time_secs);
                let display_oy = sh.evaluated_offset_y(clip_time_secs);
                let display_bl = sh.evaluated_blur(clip_time_secs);
                let kfs_ox: &[klipp_core::Keyframe] = sh.offset_x_keyframes.as_slice();
                let kfs_oy: &[klipp_core::Keyframe] = sh.offset_y_keyframes.as_slice();
                let kfs_bl: &[klipp_core::Keyframe] = sh.blur_keyframes.as_slice();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("#{}", i)).size(rows::TEXT).color(theme.text_secondary));
                    let mut col = egui::Color32::from_rgba_premultiplied(sh.color[0], sh.color[1], sh.color[2], sh.color[3]);
                    if ui.color_edit_button_srgba(&mut col).changed() {
                        cmds.push(AppCommand::SetTitleInnerShadowLayer {
                            clip_id,
                            index: i as u32,
                            enabled: sh.enabled,
                            color: [col.r(), col.g(), col.b(), col.a()],
                            offset_x: sh.offset_x,
                            offset_y: sh.offset_y,
                            blur: sh.blur,
                        });
                    }
                    let mut ox = display_ox;
                    crate::param_focus::tag_keys(ui, kfs_ox);
                    let r_ox = rows::param_row(
                        ui, theme, "x", &mut ox, -20.0..=20.0, 2.0, 0.5, 1, " px",
                        target_oid.map(|_| title_object_keyframe_state(kfs_ox, clip_time_secs, keyframe_tolerance)),
                    );
                    handle_inner_shadow_layer_axis_response(
                        cmds, clip_id, target_oid, i, "offset_x",
                        ox, &sh, kfs_ox, clip_time_secs, &r_ox,
                        keyframe_tolerance,
                    );
                    let mut oy = display_oy;
                    crate::param_focus::tag_keys(ui, kfs_oy);
                    let r_oy = rows::param_row(
                        ui, theme, "y", &mut oy, -20.0..=20.0, 2.0, 0.5, 1, " px",
                        target_oid.map(|_| title_object_keyframe_state(kfs_oy, clip_time_secs, keyframe_tolerance)),
                    );
                    handle_inner_shadow_layer_axis_response(
                        cmds, clip_id, target_oid, i, "offset_y",
                        oy, &sh, kfs_oy, clip_time_secs, &r_oy,
                        keyframe_tolerance,
                    );
                    let mut bl = display_bl;
                    crate::param_focus::tag_keys(ui, kfs_bl);
                    let r_bl = rows::param_row(
                        ui, theme, "blur", &mut bl, 0.0..=60.0, 3.0, 0.5, 1, " px",
                        target_oid.map(|_| title_object_keyframe_state(kfs_bl, clip_time_secs, keyframe_tolerance)),
                    );
                    handle_inner_shadow_layer_axis_response(
                        cmds, clip_id, target_oid, i, "blur",
                        bl, &sh, kfs_bl, clip_time_secs, &r_bl,
                        keyframe_tolerance,
                    );
                    if ui.small_button("\u{00D7}").clicked() {
                        let mut new_vec = inners.clone();
                        new_vec.remove(i);
                        structural_change = Some(new_vec);
                    }
                });
            }
            if ui.small_button("+ Add Layer").clicked() {
                let mut new_vec = inners.clone();
                if new_vec.is_empty() {
                    new_vec.push(klipp_timeline::InnerShadow {
                        color: [0, 0, 0, 180],
                        offset_x: 2.0,
                        offset_y: 2.0,
                        blur: 3.0,
                        enabled: false,
                        offset_x_keyframes: Vec::new(),
                        offset_y_keyframes: Vec::new(),
                        blur_keyframes: Vec::new(),
                    });
                }
                new_vec.push(klipp_timeline::InnerShadow {
                    color: [0, 0, 0, 200],
                    offset_x: 4.0,
                    offset_y: 4.0,
                    blur: 6.0,
                    enabled: true,
                    offset_x_keyframes: Vec::new(),
                    offset_y_keyframes: Vec::new(),
                    blur_keyframes: Vec::new(),
                });
                structural_change = Some(new_vec);
            }
            if let Some(new_inners) = structural_change {
                let tuples: Vec<([u8; 4], f32, f32, f32)> = new_inners
                    .iter()
                    .map(|s| (s.color, s.offset_x, s.offset_y, s.blur))
                    .collect();
                cmds.push(AppCommand::SetTitleInnerShadows { clip_id, shadows: tuples });
            }
        });
        if enabled != fx.inner_shadow_stack_enabled {
            cmds.push(AppCommand::SetTitleInnerShadowStackEnabled { clip_id, enabled });
        }
    }
    ui.add_space(6.0);

    // The fills are a text's own; a shape or a picture has a face of
    // its own and no fill to pick.
    if let Some(tb) = tb {
        // 10. Gradient Fill
        {
            let cur = tb.gradient.as_ref().cloned().unwrap_or(klipp_timeline::TextGradient::pair(
                [255, 200, 50, 255], [255, 50, 50, 255], false,
            ));
            let mut enabled = gradient_enabled;
            let summary = if enabled { gradient_summary(&cur) } else { String::new() };
            let cur_top = cur.top_color;
            let cur_bot = cur.bottom_color;
            inspector_subgroup_card(ui, "Gradient Fill", &mut enabled,
                (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
                // As it is at the playhead, its stops keyed on the layer (#1511).
                let keys = target_oid.map(|oid| gradient_editor::StopKeys { object_id: oid.0, keys: &cur.stop_keyframes, time_secs: clip_time_secs, tolerance: keyframe_tolerance });
                gradient_editor::show(ui, clip_id, None, &cur.at(clip_time_secs), keys, cmds);
            });
            if enabled != gradient_enabled {
                cmds.push(AppCommand::SetTitleGradient {
                    clip_id, enabled, top_color: cur_top, bottom_color: cur_bot,
                });
            }
        }
        ui.add_space(6.0);

        // 11. Pattern Fill
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
                // Kind row — laid out with the same 70 px label column +
                // 8 px spacing as the rest of the inspector so the
                // "Kind" / "A" / "B" / "Size" labels line up vertically.
                let mut new_idx = pat_idx;
                let label_w: f32 = 70.0;
                let row_h: f32 = 28.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (label_rect, _) = ui.allocate_exact_size(
                        egui::vec2(label_w, row_h),
                        egui::Sense::hover(),
                    );
                    ui.painter().text(
                        egui::pos2(label_rect.min.x, label_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "Kind",
                        egui::FontId::proportional(11.0),
                        theme.text_dim,
                    );
                    egui::ComboBox::from_id_salt("title_pattern_kind")
                        .selected_text(labels[new_idx])
                        .width(ui.available_width())
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
                    let (ca0, cb0, mut size) = match pf {
                        klipp_timeline::TextPattern::HorizontalStripes { color_a, color_b, stripe_width } => (*color_a, *color_b, *stripe_width),
                        klipp_timeline::TextPattern::VerticalStripes { color_a, color_b, stripe_width } => (*color_a, *color_b, *stripe_width),
                        klipp_timeline::TextPattern::Checkerboard { color_a, color_b, cell_size } => (*color_a, *color_b, *cell_size),
                        klipp_timeline::TextPattern::DiagonalStripes { color_a, color_b, stripe_width } => (*color_a, *color_b, *stripe_width),
                    };
                    let mut col_a = egui::Color32::from_rgba_premultiplied(ca0[0], ca0[1], ca0[2], ca0[3]);
                    let mut col_b = egui::Color32::from_rgba_premultiplied(cb0[0], cb0[1], cb0[2], cb0[3]);
                    // A and B each get their own row with the new
                    // effect_color_row treatment (label + swatch + hex
                    // input) so they line up with every other Color row
                    // in the inspector.
                    let a_changed = effect_color_row(ui, theme, "A", &mut col_a, ("pattern_a", clip_id));
                    let b_changed = effect_color_row(ui, theme, "B", &mut col_b, ("pattern_b", clip_id));
                    let size_pill = format!("{} px", size.round() as i32);
                    let resp_size = effect_param_row(
                        ui, theme, "Size", &mut size, 2.0..=50.0,
                        &size_pill, &[],
                    );
                    if a_changed || b_changed || resp_size.changed {
                        let ca = [col_a.r(), col_a.g(), col_a.b(), col_a.a()];
                        let cb = [col_b.r(), col_b.g(), col_b.b(), col_b.a()];
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

        // 12. Texture Fill
        {
            let mut enabled = texture_toggle_on;
            // Read current state — fall back to defaults when no
            // TextureFill exists yet so the inspector renders sensible
            // values even before the user enables.
            let cur_tf: klipp_timeline::TextureFill = tb
                .texture_fill
                .clone()
                .unwrap_or_default();
            let cur_preset: Option<klipp_timeline::TexturePresetId> = match &cur_tf.source {
                klipp_timeline::TextureSource::Preset(id) => Some(*id),
                _ => None,
            };
            let source_label = match &cur_tf.source {
                klipp_timeline::TextureSource::None => "(none)".to_string(),
                klipp_timeline::TextureSource::File(p) => std::path::Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("(file)")
                    .to_string(),
                klipp_timeline::TextureSource::Preset(id) => preset_display_name(*id).to_string(),
            };
            let summary = if texture_active { source_label.clone() } else { String::new() };
            inspector_subgroup_card(ui, "Texture Fill", &mut enabled,
                (!summary.is_empty()).then_some(summary.as_str()), theme, |ui| {
                // ── Source row: label + current source name (read-only).
                let label_w: f32 = 70.0;
                let row_h: f32 = 28.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (label_rect, _) = ui.allocate_exact_size(
                        egui::vec2(label_w, row_h), egui::Sense::hover(),
                    );
                    ui.painter().text(
                        egui::pos2(label_rect.min.x, label_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "Source",
                        egui::FontId::proportional(11.0),
                        theme.text_dim,
                    );
                    ui.label(
                        egui::RichText::new(&source_label)
                            .size(11.0)
                            .monospace()
                            .color(theme.text_secondary),
                    );
                });

                // ── Browse / Preset dropdown / Clear row.
                ui.horizontal(|ui| {
                    if ui.button("Browse\u{2026}").clicked() {
                        crate::file_dialog::spawn(move || {
                            rfd::FileDialog::new()
                                .add_filter("Images", &{
                                    let mut exts: Vec<&str> = klipp_core::media_extensions::IMAGE_EXTENSIONS.to_vec();
                                    exts.push("tga");
                                    exts
                                })
                                .pick_file()
                                .map(|path| vec![AppCommand::SetTitleTextureFill {
                                    clip_id, path: Some(path.to_string_lossy().to_string()),
                                }])
                                .unwrap_or_default()
                        });
                    }
                    // Procedural-preset picker. Selecting any entry
                    // dispatches SetTitleTexturePreset, swapping the
                    // source while preserving every other param.
                    let preset_text = cur_preset
                        .map(preset_display_name)
                        .unwrap_or("Preset\u{2026}");
                    egui::ComboBox::from_id_salt("title_texture_preset")
                        .selected_text(preset_text)
                        .show_ui(ui, |ui| {
                            for (id, name) in [
                                (0u8, "Paper Grain"),
                                (1u8, "Scanlines"),
                                (2u8, "Diagonal Hatch"),
                                (3u8, "Gradient Sweep"),
                            ] {
                                let active = matches!(cur_preset, Some(p) if preset_id_to_u8(p) == id);
                                if ui.selectable_label(active, name).clicked() {
                                    cmds.push(AppCommand::SetTitleTexturePreset {
                                        clip_id, preset_id: id,
                                    });
                                }
                            }
                        });
                    // Clear nukes the entire TextureFill — different
                    // from "no preset" since it also drops the params.
                    let has_source = !matches!(cur_tf.source, klipp_timeline::TextureSource::None);
                    if ui.add_enabled(has_source, egui::Button::new("Clear")).clicked() {
                        cmds.push(AppCommand::SetTitleTextureFill {
                            clip_id, path: None,
                        });
                    }
                });

                // ── Tile mode dropdown — same label-column alignment.
                let tile_mode_idx = match cur_tf.tile_mode {
                    klipp_timeline::TextureTileMode::Stretch => 0u8,
                    klipp_timeline::TextureTileMode::Tile => 1,
                    klipp_timeline::TextureTileMode::Fit => 2,
                    klipp_timeline::TextureTileMode::Cover => 3,
                };
                let mut new_tile = tile_mode_idx;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let (label_rect, _) = ui.allocate_exact_size(
                        egui::vec2(label_w, row_h), egui::Sense::hover(),
                    );
                    ui.painter().text(
                        egui::pos2(label_rect.min.x, label_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        "Tile",
                        egui::FontId::proportional(11.0),
                        theme.text_dim,
                    );
                    let tile_labels = ["Stretch", "Tile", "Fit", "Cover"];
                    egui::ComboBox::from_id_salt("title_texture_tile_mode")
                        .selected_text(tile_labels[tile_mode_idx as usize])
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (i, label) in tile_labels.iter().enumerate() {
                                if ui.selectable_label(new_tile == i as u8, *label).clicked() {
                                    new_tile = i as u8;
                                }
                            }
                        });
                });

                // ── Scale / Offset X / Offset Y / Rotation / Opacity
                //    rows via effect_param_row so they look identical
                //    to every other inspector slider.
                let mut scale = cur_tf.scale;
                let mut offset_x = cur_tf.offset_x;
                let mut offset_y = cur_tf.offset_y;
                let mut rotation = cur_tf.rotation;
                let mut opacity = cur_tf.opacity;

                let pill_scale = format!("{:.2}\u{00d7}", scale);
                let resp_scale = effect_param_row(
                    ui, theme, "Scale", &mut scale, 0.1..=5.0, &pill_scale, &[],
                );
                let pill_ox = format!("{} px", offset_x.round() as i32);
                let resp_ox = effect_param_row(
                    ui, theme, "Offset X", &mut offset_x, -200.0..=200.0, &pill_ox, &[],
                );
                let pill_oy = format!("{} px", offset_y.round() as i32);
                let resp_oy = effect_param_row(
                    ui, theme, "Offset Y", &mut offset_y, -200.0..=200.0, &pill_oy, &[],
                );
                let pill_rot = format!("{}\u{00b0}", rotation.round() as i32);
                let resp_rot = effect_param_row(
                    ui, theme, "Rotation", &mut rotation, -180.0..=180.0, &pill_rot, &[],
                );
                let pill_op = format!("{}%", (opacity * 100.0).round() as i32);
                let resp_op = effect_param_row(
                    ui, theme, "Opacity", &mut opacity, 0.0..=1.0, &pill_op, &[],
                );

                // ── Tint row via effect_color_row (label + swatch + hex).
                let mut tint_col = egui::Color32::from_rgba_premultiplied(
                    cur_tf.tint[0], cur_tf.tint[1], cur_tf.tint[2], cur_tf.tint[3],
                );
                let tint_changed = effect_color_row(
                    ui, theme, "Tint", &mut tint_col, ("texture_tint", clip_id),
                );

                // Dispatch a single SetTitleTextureParams command if
                // anything changed. The handler preserves the source,
                // so this cleanly composes with the source-picker
                // dispatches above.
                let any_param_changed = new_tile != tile_mode_idx
                    || resp_scale.changed
                    || resp_ox.changed
                    || resp_oy.changed
                    || resp_rot.changed
                    || resp_op.changed
                    || tint_changed;
                if any_param_changed {
                    cmds.push(AppCommand::SetTitleTextureParams {
                        clip_id,
                        tile_mode: new_tile,
                        scale,
                        offset_x,
                        offset_y,
                        rotation,
                        opacity,
                        tint: [tint_col.r(), tint_col.g(), tint_col.b(), tint_col.a()],
                    });
                }
            });
            if enabled != texture_toggle_on {
                cmds.push(AppCommand::SetTitleTextureEnabled { clip_id, enabled });
            }
        }

    }
    ui.add_space(10.0);
}

/// radius. Phase-1 ships only the rectangle primitive, so the
/// editor is small. Future shape kinds (ellipse, polygon, path)
/// add more controls inside the same `match shape.kind` arm.
/// One image file, chosen by the user. Blocks, so it runs inside
/// `file_dialog::spawn`.
fn pick_image_file() -> Option<String> {
    rfd::FileDialog::new()
        .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
        .pick_file()
        .map(|p| p.to_string_lossy().to_string())
}

/// The editor of an image layer: the file, and how it sits in its box.
fn show_title_image_editor(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    object_id: klipp_timeline::TitleObjectId,
    img: &klipp_timeline::ImageLayer,
    cmds: &mut Vec<AppCommand>,
) {
    ui.label(egui::RichText::new("Image").strong().size(12.0));
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let name = std::path::Path::new(&img.path).file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        ui.add(egui::Label::new(egui::RichText::new(name).size(crate::theme::type_scale::CONTROL)).truncate())
            .on_hover_text(&img.path);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(egui::Button::new(egui::RichText::new("Replace\u{2026}").size(crate::theme::type_scale::CAPTION))).clicked() {
                let object_id = object_id.0;
                crate::file_dialog::spawn(move || {
                    pick_image_file()
                        .map(|path| vec![AppCommand::SetTitleImagePath { clip_id, object_id, path }])
                        .unwrap_or_default()
                });
            }
        });
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("In its box").size(crate::theme::type_scale::CONTROL).color(theme.text_secondary));
        let labels: Vec<&str> = klipp_timeline::ImageFit::ALL.iter().map(|f| f.label()).collect();
        let active = klipp_timeline::ImageFit::ALL.iter().position(|f| *f == img.fit).unwrap_or(0);
        if let Some(i) = crate::widgets::segmented::segmented(ui, &labels, active, theme, egui::FontId::proportional(crate::theme::type_scale::CAPTION), crate::theme::control::COMPACT) {
            cmds.push(AppCommand::SetTitleImageFit { clip_id, object_id: object_id.0, fit: i as u8 });
        }
    });
}

/// The Title tab with nothing to edit: the one-click way in.
fn title_empty_state(ui: &mut egui::Ui, theme: &KlippTheme, cmds: &mut Vec<AppCommand>) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new("No title selected").size(crate::theme::type_scale::TITLE).color(theme.text_secondary));
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("Add one at the playhead, or pick a design in Library \u{203a} Titles.")
                .size(crate::theme::type_scale::CAPTION)
                .color(theme.text_dim),
        );
        ui.add_space(10.0);
        let b = ui.add(egui::Button::new(egui::RichText::new("Add Title").size(crate::theme::type_scale::CONTROL)).min_size(egui::vec2(120.0, crate::theme::control::REGULAR)));
        if b.on_hover_text("Ctrl+T").clicked() {
            cmds.push(AppCommand::AddTitleAtPlayhead { over_clip: None });
        }
    });
}

/// "Linear 45° · 3 stops": a gradient in a few words, for its card.
fn gradient_summary(g: &klipp_timeline::TextGradient) -> String {
    let n = g.stops().len();
    match g.kind {
        klipp_timeline::GradientKind::Linear { angle_deg: 0.0 } => format!("Linear · {n} stops"),
        klipp_timeline::GradientKind::Linear { angle_deg } => format!("Linear {angle_deg:.0}° · {n} stops"),
        klipp_timeline::GradientKind::Radial { .. } => format!("Radial · {n} stops"),
    }
}

/// A shape's gradient fill (#1509): switched on, it fills the shape's
/// box in place of its colour.
fn shape_gradient(
    ui: &mut egui::Ui,
    clip_id: klipp_core::ClipId,
    object_id: klipp_timeline::TitleObjectId,
    shape: &klipp_timeline::Shape,
    cmds: &mut Vec<AppCommand>,
) {
    let cur = shape.gradient.clone().unwrap_or_else(|| {
        let base = shape.fill.unwrap_or([60, 60, 70, 255]);
        klipp_timeline::TextGradient::pair(base, [base[0] / 2, base[1] / 2, base[2] / 2, base[3]], false)
    });
    let mut on = cur.enabled;
    if ui.checkbox(&mut on, "Gradient fill").changed() {
        cmds.push(AppCommand::SetTitleGradientStops { clip_id, object_id: Some(object_id.0), enabled: on, stops: cur.stops(), kind: cur.kind });
    }
    if on {
        ui.push_id(("shape_gradient", object_id.0), |ui| gradient_editor::show(ui, clip_id, Some(object_id.0), &cur, None, cmds));
    }
}

fn show_title_shape_editor(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    object_id: klipp_timeline::TitleObjectId,
    shape: &klipp_timeline::Shape,
    title: &klipp_timeline::TitleData,
    cmds: &mut Vec<AppCommand>,
    time: f64,
    tolerance: f64,
) {
    if let klipp_timeline::ShapeKind::Path(path)=&shape.kind {shape_gradient(ui,clip_id,object_id,shape,cmds);vector_path::show(ui,clip_id,object_id,path,time,tolerance,cmds);return;}
    if ui.button("Start editable path").clicked() {cmds.push(AppCommand::SetTitleVectorPath{clip_id,object_id:object_id.0,path:klipp_timeline::VectorPath::default()});}
    ui.label(egui::RichText::new("Shape").strong().size(12.0));
    ui.add_space(6.0);

    let mut target = shape.fit.map(|fit| fit.target.0);
    let old_target = target;
    let selected = target.and_then(|id| title.object(klipp_timeline::TitleObjectId(id)))
        .map(|o| o.name.clone()).unwrap_or_else(|| "Fixed size".into());
    egui::ComboBox::from_id_salt(("shape_fit", object_id.0))
        .selected_text(selected)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut target, None, "Fixed size");
            for object in &title.objects {
                if matches!(object.content, klipp_timeline::TitleObjectContent::TextBox(_)) {
                    ui.selectable_value(&mut target, Some(object.id.0), format!("Fit to {}", object.name));
                }
            }
        });
    let padding = shape.fit.map(|f| f.padding).unwrap_or(klipp_timeline::TitlePosition { x: 20.0, y: 12.0 });
    let (mut px, mut py) = (padding.x, padding.y);
    let mut changed = target != old_target;
    if target.is_some() {
        changed |= ui.add(egui::Slider::new(&mut px, 0.0..=200.0).text("Horizontal padding").suffix(" px @ 1080p")).changed();
        changed |= ui.add(egui::Slider::new(&mut py, 0.0..=200.0).text("Vertical padding").suffix(" px @ 1080p")).changed();
    }
    if changed {
        cmds.push(AppCommand::SetTitleShapeFit { clip_id, object_id: object_id.0, target_id: target, padding_x: px, padding_y: py });
    }

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
            ui.label(egui::RichText::new("Colour").size(rows::TEXT).color(theme.text_secondary));
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

    shape_gradient(ui, clip_id, object_id, shape, cmds);

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
            ui.label(egui::RichText::new("Colour").size(rows::TEXT).color(theme.text_secondary));
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
        klipp_timeline::ShapeKind::Path(_) => {}
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
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(6.0, 6.0))
        .show(ui, |ui| {
            if total == 0 {
                ui.label(
                    egui::RichText::new("No layers — add one below.")
                        .size(11.0)
                        .color(theme.text_dim),
                );
            }
            // The rows: select, drag to restack, hide, lock, rename,
            // duplicate, delete (#1715).
            title_layers::show(ui, theme, clip_id, td, selected_oid, cmds);
        });

    // ── Add buttons ──
    // Dashed-border ghost style sized 50/50 across the panel — reads
    // as an "add a new thing" affordance rather than a regular action
    // button. The dashed stroke is hand-rolled because egui's Stroke
    // is solid only.
    ui.add_space(6.0);
    let btn_h = 32.0;
    let gap = 8.0;
    let avail = ui.available_width();
    let col_w = ((avail - gap * 2.0) / 3.0).floor();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for (kind, label, hover) in [
            (0u8, "\u{002B} Text", "Add a text layer"),
            (1u8, "\u{002B} Shape", "Add a shape layer: a rectangle, ellipse, triangle, hexagon, star, arrow or line"),
            (2u8, "\u{002B} Image", "Add an image layer: a logo, a bug, a still"),
        ] {
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(col_w, btn_h),
                egui::Sense::click(),
            );
            let resp = resp.on_hover_text(hover);
            let stroke_color = if resp.hovered() {
                theme.text_secondary
            } else {
                theme.border
            };
            let label_color = if resp.hovered() {
                theme.text_primary
            } else {
                theme.text_secondary
            };
            paint_dashed_rect(ui, rect, stroke_color, 4.0, 4.0, 1.0, 4.0);
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::FontId::proportional(11.0),
                label_color,
            );
            if kind == 1 {
                // The shape is chosen from a menu under the button (#67).
                let popup_id = ui.make_persistent_id(("title_shape_menu", clip_id));
                if resp.clicked() {
                    ui.memory_mut(|m| m.toggle_popup(popup_id));
                }
                egui::popup::popup_below_widget(ui, popup_id, &resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                    ui.set_min_width(140.0);
                    for (shape_kind, name) in TITLE_SHAPES {
                        if ui.button(*name).clicked() {
                            cmds.push(AppCommand::AddTitleObject { clip_id, kind: *shape_kind });
                        }
                    }
                });
            } else if resp.clicked() {
                if kind == 2 {
                    crate::file_dialog::spawn(move || {
                        pick_image_file()
                            .map(|path| vec![AppCommand::AddTitleImageObject { clip_id, path }])
                            .unwrap_or_default()
                    });
                } else {
                    cmds.push(AppCommand::AddTitleObject { clip_id, kind });
                }
            }
        }
    });
}

/// Compact framed axis input used by the Position block. Shape:
/// [ X        0.15 ◇ ]  with the diamond visible ONLY when keyframes
/// exist on this axis (option b — keeps the unanimated default
/// visually clean per the redesign reference). The returned
/// ScrubbyParamResponse-shaped struct lets the existing
/// `handle_position_axis_response` consume it without changes.
fn compact_axis_input(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    value: &mut f32,
    col_w: f32,
    keyframes: &[klipp_core::Keyframe],
    ks: crate::widgets::scrubby_param::KeyframeState,
) -> crate::widgets::scrubby_param::ScrubbyParamResponse {
    let has_kfs = !keyframes.is_empty();
    let mut changed = false;
    let mut keyframe_toggled = false;
    // The axis's track, found by its keys, for the Inspector's focus (#2296).
    crate::param_focus::tag_keys(ui, keyframes);
    let tag = crate::param_focus::take_tag(ui);
    let mut picked = false;
    let framed = egui::Frame::none()
        .fill(theme.bg_raised)
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
        .show(ui, |ui| {
            ui.set_width(col_w - 16.0);
            ui.horizontal(|ui| {
                let name = ui.add(egui::Label::new(
                    egui::RichText::new(label)
                        .size(rows::TEXT)
                        .color(theme.text_secondary),
                ).sense(if tag.is_some() { egui::Sense::click() } else { egui::Sense::hover() }));
                picked |= name.clicked();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Diamond only when this axis already has
                    // keyframes, drawn as a number row's: filled on a
                    // key, an outline between keys (#2296).
                    if has_kfs {
                        let diamond = crate::widgets::scrubby_param::diamond_button(ui, &ks, tag.as_ref().map(|t| t.keys()));
                        if diamond.clicked() {
                            keyframe_toggled = true;
                            picked = true;
                        }
                    }
                    if has_kfs {
                        crate::widgets::scrubby_param::tint_animated(ui);
                    }
                    let dv_resp = ui.add(
                        egui::DragValue::new(value)
                            .range(0.0..=1.0)
                            .speed(0.001)
                            .fixed_decimals(2),
                    );
                    if dv_resp.changed() {
                        changed = true;
                    }
                });
            });
        });
    if let Some(tag) = &tag {
        crate::param_focus::row_drawn(ui, tag, framed.response.rect, picked);
    }
    crate::widgets::scrubby_param::ScrubbyParamResponse {
        changed,
        drag_started: false,
        drag_stopped: false,
        lost_focus: false,
        reset_clicked: false,
        keyframe_toggled,
        keyframe_nav_prev: false,
        keyframe_nav_next: false,
        keyframe_set_interpolation: None,
        keyframe_ease_preset: None,
    }
}

/// Color row used inside effect subcards: `[ label  swatch  hex_input ]`.
/// Two ways to change the colour:
///   - click the swatch → standard egui colour picker; `*color` updates
///     live as the user picks
///   - type a `#RRGGBB` hex into the input, press Enter or click away;
///     valid hex applies to `*color`, invalid silently reverts the
///     buffer back to the current colour's hex
///
/// `id_salt` distinguishes the per-effect persistent buffer in
/// `ui.ctx().data` so two color rows on the same screen don't share
/// state. Returns true on any colour change so the caller can dispatch
/// the right `SetTitle*` command.
fn effect_color_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    color: &mut egui::Color32,
    id_salt: impl std::hash::Hash,
) -> bool {
    let label_w = 70.0;
    let swatch_w = 28.0;
    let row_h = 28.0;
    let mut changed = false;
    let buf_id = ui.id().with(("effect_color_hex_buf", id_salt));
    let current_hex = format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b());

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;

        // Label column — same dim style + width as effect_param_row.
        let (label_rect, _) = ui.allocate_exact_size(
            egui::vec2(label_w, row_h),
            egui::Sense::hover(),
        );
        ui.painter().text(
            egui::pos2(label_rect.min.x, label_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(rows::TEXT),
            theme.text_secondary,
        );

        // Swatch — opens egui's colour picker on click; live-applies.
        let swatch_changed = ui
            .scope(|ui| {
                ui.spacing_mut().interact_size = egui::vec2(swatch_w, row_h);
                ui.color_edit_button_srgba(color).changed()
            })
            .inner;
        if swatch_changed {
            changed = true;
        }

        // Hex input — buffer in ctx.data so user typing isn't clobbered
        // every frame by the current-color sync.
        let mut hex_buf: String = ui.ctx().data_mut(|d| {
            d.get_temp::<String>(buf_id).unwrap_or_else(|| current_hex.clone())
        });
        let hex_resp = egui::Frame::none()
            .fill(theme.bg_raised)
            .rounding(4.0)
            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
            .show(ui, |ui| {
                ui.set_width(ui.available_width().max(40.0));
                ui.add(
                    egui::TextEdit::singleline(&mut hex_buf)
                        .frame(false)
                        .desired_width(f32::INFINITY)
                        .font(egui::FontId::monospace(11.0)),
                )
            })
            .inner;

        // On focus loss (which Enter triggers): try to parse + apply.
        // Silent revert to the current color's hex if invalid — the
        // ctx.data sync below overwrites the user's typo with the
        // canonical hex.
        if hex_resp.lost_focus() {
            if let Some(parsed) = parse_hex_color(&hex_buf) {
                let new_rgb = [parsed.r(), parsed.g(), parsed.b()];
                let cur_rgb = [color.r(), color.g(), color.b()];
                if new_rgb != cur_rgb {
                    *color = egui::Color32::from_rgba_premultiplied(
                        parsed.r(),
                        parsed.g(),
                        parsed.b(),
                        color.a(),
                    );
                    changed = true;
                }
            }
        }

        // Persistent buffer state: while focused, save what the user
        // typed; otherwise sync to the canonical hex from `*color`
        // (handles both picker-driven changes and the silent-revert
        // case after a bad hex submit).
        if hex_resp.has_focus() {
            ui.ctx().data_mut(|d| d.insert_temp(buf_id, hex_buf));
        } else {
            let canon = format!("#{:02X}{:02X}{:02X}", color.r(), color.g(), color.b());
            ui.ctx().data_mut(|d| d.insert_temp(buf_id, canon));
        }
    });

    changed
}

fn preset_display_name(id: klipp_timeline::TexturePresetId) -> &'static str {
    match id {
        klipp_timeline::TexturePresetId::PaperGrain => "Paper Grain",
        klipp_timeline::TexturePresetId::Scanlines => "Scanlines",
        klipp_timeline::TexturePresetId::DiagonalHatch => "Diagonal Hatch",
        klipp_timeline::TexturePresetId::GradientSweep => "Gradient Sweep",
    }
}

fn preset_id_to_u8(id: klipp_timeline::TexturePresetId) -> u8 {
    match id {
        klipp_timeline::TexturePresetId::PaperGrain => 0,
        klipp_timeline::TexturePresetId::Scanlines => 1,
        klipp_timeline::TexturePresetId::DiagonalHatch => 2,
        klipp_timeline::TexturePresetId::GradientSweep => 3,
    }
}

/// Parse `#RRGGBB` (case-insensitive, leading `#` optional) into a
/// `Color32` with full opacity. Returns `None` for any other shape —
/// callers use `None` as "silently revert".
fn parse_hex_color(s: &str) -> Option<egui::Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(egui::Color32::from_rgb(r, g, b))
}

/// Three-column row used inside effect subcards:
/// `[ label_col  thin_slider  value_pill  diamond? ]`. Replaces the
/// scrubby_param_row inside Glow / Drop Shadow / Inner Shadow / etc.
/// — the scrubby drag-on-label affordance is loud, the slider already
/// gives drag-to-edit, and the reference design wants a clean dim
/// label. Diamond renders only when `keyframes` is non-empty (option b
/// from the audit). Returns the same response shape as
/// `scrubby_param_row` so the existing `handle_*_axis_response`
/// helpers continue to work unchanged.
fn effect_param_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    pill_text: &str,
    keyframes: &[klipp_core::Keyframe],
) -> crate::widgets::scrubby_param::ScrubbyParamResponse {
    let label_w = 70.0;
    let pill_w = 64.0;
    let row_h = 28.0;
    let mut changed = false;
    let mut keyframe_toggled = false;
    let has_kfs = !keyframes.is_empty();

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;

        // Label column — dim, vertically centred, fixed width so
        // every row in the subcard lines up.
        let (label_rect, _) = ui.allocate_exact_size(
            egui::vec2(label_w, row_h),
            egui::Sense::hover(),
        );
        ui.painter().text(
            egui::pos2(label_rect.min.x, label_rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(rows::TEXT),
            theme.text_secondary,
        );

        // Reserve room for value pill + optional diamond on the
        // right; the slider fills whatever's left.
        let diamond_w = if has_kfs { 14.0 + 6.0 } else { 0.0 };
        let avail = ui.available_width();
        let slider_w = (avail - pill_w - 8.0 - diamond_w).max(40.0);

        // Slider — thin track + dot handle, no fill highlight.
        // Visuals scoped so the override doesn't leak to other widgets.
        let slider_resp = ui
            .scope(|ui| {
                ui.style_mut().spacing.slider_rail_height = 3.0;
                let visuals = &mut ui.visuals_mut();
                // Disable the "fill" segment left of the handle by
                // matching its colour to the unfilled track.
                visuals.selection.bg_fill = theme.border;
                visuals.widgets.inactive.bg_fill = theme.border;
                visuals.widgets.hovered.bg_fill = theme.border;
                ui.spacing_mut().slider_width = slider_w;
                ui.add(
                    egui::Slider::new(value, range)
                        .show_value(false),
                )
            })
            .inner;
        if slider_resp.changed() {
            changed = true;
        }

        // Value pill — framed, right-aligned text.
        egui::Frame::none()
            .fill(theme.bg_raised)
            .rounding(4.0)
            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
            .show(ui, |ui| {
                ui.set_width(pill_w - 16.0);
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        ui.label(
                            egui::RichText::new(pill_text)
                                .size(11.0)
                                .color(theme.text_secondary),
                        );
                    },
                );
            });

        // Diamond — visible only when this param already has
        // keyframes. First-keyframe-add path is via the right-click
        // context menu (or future scrubby variants), keeping the
        // unanimated default visually clean.
        if has_kfs {
            ui.add_space(2.0);
            let diamond = egui::Button::new(
                egui::RichText::new("\u{25C6}")
                    .size(10.0)
                    .color(theme.accent),
            )
            .frame(false)
            .min_size(egui::vec2(14.0, 14.0));
            if ui.add(diamond).clicked() {
                keyframe_toggled = true;
            }
        }
    });

    crate::widgets::scrubby_param::ScrubbyParamResponse {
        changed,
        drag_started: false,
        drag_stopped: false,
        lost_focus: false,
        reset_clicked: false,
        keyframe_toggled,
        keyframe_nav_prev: false,
        keyframe_nav_next: false,
        keyframe_set_interpolation: None,
        keyframe_ease_preset: None,
    }
}

/// Compact pill-style toggle button used by the BIUS + LCR rows in
/// the pinned core. Active state shows the accent fill; inactive uses
/// `bg_raised` so the buttons read as one coordinated control strip
/// rather than four free-standing text labels (which `selectable_label`
/// produces). The closure lets the caller layer style modifiers
/// (`strong`, `italics`, `underline`, `strikethrough`) onto the label.
fn pill_toggle_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    active: bool,
    style_label: impl FnOnce(egui::RichText) -> egui::RichText,
) -> egui::Response {
    let fill = if active { theme.accent } else { theme.bg_raised };
    let text_color = if active { theme.text_on_accent } else { theme.text_secondary };
    let rich = style_label(egui::RichText::new(label).color(text_color));
    let btn = egui::Button::new(rich)
        .fill(fill)
        .min_size(egui::vec2(28.0, 28.0))
        .rounding(4.0);
    ui.add(btn)
}

/// Icon-image variant of `pill_toggle_button`. Same 28×28 pill shape
/// and accent-on-active fill, but the body is a tinted SVG icon
/// instead of a text glyph. Used by the L / C / R alignment row so
/// the controls match standard NLE / word-processor alignment glyphs
/// rather than letter labels.
fn pill_toggle_icon_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Icon,
    active: bool,
) -> egui::Response {
    let fill = if active { theme.accent } else { theme.bg_raised };
    let tint = if active { theme.text_on_accent } else { theme.text_secondary };
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(28.0, 28.0),
        egui::Sense::click(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, egui::Rounding::same(4.0), fill);
    let icon_size = 14.0;
    let icon_rect = egui::Rect::from_center_size(
        rect.center(),
        egui::vec2(icon_size, icon_size),
    );
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| {
        ui.add(icon.image(icon_size).tint(tint));
    });
    resp
}

/// The shapes the title tab's Shape button offers, by the `kind` its
/// `AddTitleObject` carries: 1 the rectangle, the rest ready-made paths
/// (#67).
const TITLE_SHAPES: &[(u8, &str)] = &[(1, "Rectangle"), (3, "Ellipse"), (4, "Triangle"), (5, "Hexagon"), (6, "Star"), (7, "Arrow"), (8, "Line")];

/// The ready-made path for a shape `kind` from [`TITLE_SHAPES`], or
/// `None` for the rectangle and the other layer kinds.
pub(crate) fn title_shape_preset(kind: u8) -> Option<klipp_timeline::ShapePreset> {
    use klipp_timeline::ShapePreset;
    Some(match kind {
        3 => ShapePreset::Ellipse,
        4 => ShapePreset::Triangle,
        5 => ShapePreset::Polygon { sides: 6 },
        6 => ShapePreset::Star { points: 5, inner: 0.45 },
        7 => ShapePreset::Arrow,
        8 => ShapePreset::Line,
        _ => return None,
    })
}

/// Paint a dashed rectangle outline. egui's `Stroke` only supports
/// solid lines so we lay down four `Shape::dashed_line` edges by hand.
/// `dash_len` / `gap_len` set the on/off pattern; `corner_radius` is
/// matched by inset so visually the rectangle looks rounded — the
/// dashes themselves stay axis-aligned.
fn paint_dashed_rect(
    ui: &egui::Ui,
    rect: egui::Rect,
    stroke_color: egui::Color32,
    dash_len: f32,
    gap_len: f32,
    width: f32,
    corner_radius: f32,
) {
    let stroke = egui::Stroke::new(width, stroke_color);
    let inset = corner_radius.max(0.0);
    let p_tl = egui::pos2(rect.min.x + inset, rect.min.y);
    let p_tr = egui::pos2(rect.max.x - inset, rect.min.y);
    let p_bl = egui::pos2(rect.min.x + inset, rect.max.y);
    let p_br = egui::pos2(rect.max.x - inset, rect.max.y);
    let p_lt = egui::pos2(rect.min.x, rect.min.y + inset);
    let p_lb = egui::pos2(rect.min.x, rect.max.y - inset);
    let p_rt = egui::pos2(rect.max.x, rect.min.y + inset);
    let p_rb = egui::pos2(rect.max.x, rect.max.y - inset);
    let painter = ui.painter();
    for shape in egui::Shape::dashed_line(&[p_tl, p_tr], stroke, dash_len, gap_len) {
        painter.add(shape);
    }
    for shape in egui::Shape::dashed_line(&[p_bl, p_br], stroke, dash_len, gap_len) {
        painter.add(shape);
    }
    for shape in egui::Shape::dashed_line(&[p_lt, p_lb], stroke, dash_len, gap_len) {
        painter.add(shape);
    }
    for shape in egui::Shape::dashed_line(&[p_rt, p_rb], stroke, dash_len, gap_len) {
        painter.add(shape);
    }
}

/// The layer's motion blur — the compositor smears the layer along its
/// path by it — for any layer: the text's, or a shape's or picture's.
#[allow(clippy::too_many_arguments)]
fn show_title_motion_blur_group(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    fx: &klipp_timeline::LayerFx,
    cmds: &mut Vec<AppCommand>,
    target_oid: Option<klipp_timeline::TitleObjectId>,
    clip_time_secs: f64,
    keyframe_tolerance: f64,
) {
    {
        let status = crate::widgets::inspector_group::GroupStatus {
            count: None,
            active: fx.motion_blur.clone().is_some_and(|m| m.enabled),
        };
        if title_group_is_open(ui, theme, clip_id, "Motion Blur", status) {
            ui.add_space(6.0);
    // ── Motion Blur ──
    {
        let mut enabled = fx.motion_blur.clone().is_some();
        if ui.checkbox(&mut enabled, "Enable").changed() {
            cmds.push(AppCommand::SetTitleMotionBlur {
                clip_id,
                enabled,
                shutter_angle: 180.0,
                samples: 8,
            });
        }
        if let Some(mb) = fx.motion_blur.clone() {
            // Display animated shutter_angle at the playhead.
            let display_shutter = mb.evaluated_shutter_angle(clip_time_secs);
            let kfs_shutter: &[klipp_core::Keyframe] = fx
                .motion_blur
                .as_ref()
                .map(|m| m.shutter_angle_keyframes.as_slice())
                .unwrap_or(&[]);
            let ks_shutter = title_object_keyframe_state(kfs_shutter, clip_time_secs, keyframe_tolerance);

            let mut shutter = display_shutter;
            crate::param_focus::tag_keys(ui, kfs_shutter);
            let resp_shutter = rows::param_row(
                ui, theme, "Shutter Angle", &mut shutter, 1.0..=360.0, 180.0, 1.0, 0, "\u{00b0}",
                target_oid.map(|_| ks_shutter),
            );
            handle_motion_blur_axis_response(
                cmds, clip_id, target_oid,
                shutter, &mb, kfs_shutter, clip_time_secs, &resp_shutter,
                keyframe_tolerance,
            );

            // Samples is integer-valued — not keyframable; keeps the
            // simple DragValue UI.
            let mut samples = mb.samples as i32;
            let samples_resp = ui.horizontal(|ui| {
                ui.label("Samples:");
                ui.add(egui::DragValue::new(&mut samples).range(2..=32).speed(0.1)).changed()
            });
            if samples_resp.inner {
                cmds.push(AppCommand::SetTitleMotionBlur {
                    clip_id,
                    enabled: true,
                    shutter_angle: mb.shutter_angle,
                    samples: samples as u8,
                });
            }
        }
    }
    ui.add_space(12.0);

            ui.add_space(8.0);
        }
    }
}

/// The cards a shape or a picture shares with a text now that its
/// motion and effects are its own: the motion card with its loops,
/// the effects group, the motion blur — each reading and writing the
/// selected layer's stack, with the layer itself in the chips.
#[allow(clippy::too_many_arguments)]
fn show_title_layer_stack(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    td: &klipp_timeline::TitleData,
    oid: klipp_timeline::TitleObjectId,
    chips: &mut title_tab::TitleChips,
    cmds: &mut Vec<AppCommand>,
    clip_time_secs: f64,
    keyframe_tolerance: f64,
    frame: klipp_core::Resolution,
    clip_secs: f64,
) {
    let Some(object) = td.object(oid) else { return };
    let fx = object.fx();
    let subject = title_tab::ChipSubject::Layer(object.content.clone());
    // A shape or picture travels its box, a fraction of the frame, in
    // 1080p pixels as the speed is.
    let s = klipp_core::title_motion::frame_scale(frame.width as f32, frame.height as f32);
    let layer = (object.transform.size.x * frame.width as f32 / s, object.transform.size.y * frame.height as f32 / s);
    let travel = title_tab::Travel::new(frame, layer, clip_secs);
    ui.add_space(10.0);
    title_tab::show_motion_card(ui, theme, clip_id, &subject, fx, travel, chips, cmds);
    ui.add_space(10.0);
    show_title_effects_group(ui, theme, clip_id, None, fx, cmds, Some(oid), clip_time_secs, keyframe_tolerance);
    ui.add_space(8.0);
    show_title_motion_blur_group(ui, theme, clip_id, fx, cmds, Some(oid), clip_time_secs, keyframe_tolerance);
}

fn show_title_editor(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &klipp_timeline::Clip,
    state: &AppState,
    cmds: &mut Vec<AppCommand>,
    chips: &mut title_tab::TitleChips,
) {
    let keyframe_tolerance = crate::inspector::keyframe_selection::frame_tolerance(state.sequence());
    let clip_id = clip.id;
    let td = match &clip.kind {
        klipp_timeline::ClipKind::Title(td) => td,
        _ => return,
    };
    let clip_time_secs = keyframe_selection::clip_time(state, clip);
    // A loop's window is the clip's span on the timeline, as the
    // renderer gives it.
    let clip_secs = (clip.timeline_out - clip.timeline_in).to_secs_f64();

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
                                    .size(10.0)
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

    crate::widgets::inspector_group::section_card(ui, "LAYERS", Some(format!("{}", td.objects.len())), theme, |ui| {
        show_title_layers_panel(ui, theme, clip_id, td, state, cmds);
        ui.add_space(4.0);
        rows::rail_row(ui, theme, "Template", |ui| {
            if ui
                .small_button("Save as Template\u{2026}")
                .on_hover_text("Keep this title in the Titles browser, under User")
                .clicked()
            {
                cmds.push(AppCommand::SaveTitleTemplate { clip_id });
            }
        });
    });

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
            show_title_shape_editor(ui, theme, clip_id, oid, shape, td, cmds, (state.playhead.to_secs_f64()-clip.timeline_in.to_secs_f64()).max(0.0), 0.5/state.sequence().fps.as_f64());
            ui.add_space(8.0);
            crate::widgets::inspector_group::section_card(ui, "POSITION", None, theme, |ui| show_title_position_block(ui, theme, clip_id, td, state, cmds, clip_time_secs));
            show_title_layer_stack(ui, theme, clip_id, td, oid, chips, cmds, clip_time_secs, keyframe_tolerance, state.sequence().resolution, clip_secs);
        }
        return;
    }
    if let Some(klipp_timeline::TitleObjectContent::Image(img)) = kind_ref {
        if let Some(oid) = target_oid {
            show_title_image_editor(ui, theme, clip_id, oid, img, cmds);
            ui.add_space(8.0);
            crate::widgets::inspector_group::section_card(ui, "POSITION", None, theme, |ui| show_title_position_block(ui, theme, clip_id, td, state, cmds, clip_time_secs));
            show_title_layer_stack(ui, theme, clip_id, td, oid, chips, cmds, clip_time_secs, keyframe_tolerance, state.sequence().resolution, clip_secs);
        }
        return;
    }
    if let Some(object) = target_oid.and_then(|id| td.object(id)).or_else(|| td.objects.iter().find(|o| matches!(o.content, klipp_timeline::TitleObjectContent::TextBox(_)))) {
        let t = &object.transform;
        let mut w = t.size.x * 100.0;
        let mut h = t.size.y * 100.0;
        let mut rotation = t.rotation;
        let mut anchor_x = t.anchor_norm.x * 100.0;
        let mut anchor_y = t.anchor_norm.y * 100.0;
        let mut property_changes: Vec<(klipp_core::TitleTransformProperty, f32)> = Vec::new();
        let mut toggles: Vec<klipp_core::TitleTransformProperty> = Vec::new();
        // A keyframe mark: filled where the property has a key at the
        // playhead.
        let mark = |keys: &[klipp_core::Keyframe]| if keys.iter().any(|k| (k.time_secs - clip_time_secs).abs() < 1e-6) { "\u{25c6}" } else { "\u{25c7}" };
        crate::widgets::inspector_group::section_card(ui, "TRANSFORM", None, theme, |ui| {
            // Right to left: the mark at the edge, the value before it.
            rows::rail_row(ui, theme, "Width", |ui| {
                if ui.small_button(mark(&t.size_x_keyframes)).on_hover_text("Animate width at the playhead").clicked() { toggles.push(klipp_core::TitleTransformProperty::Width); }
                if ui.add(egui::DragValue::new(&mut w).range(1.0..=400.0).suffix(" %")).changed() { property_changes.push((klipp_core::TitleTransformProperty::Width, w / 100.0)); }
            });
            rows::rail_row(ui, theme, "Height", |ui| {
                if ui.small_button(mark(&t.size_y_keyframes)).on_hover_text("Animate height at the playhead").clicked() { toggles.push(klipp_core::TitleTransformProperty::Height); }
                if ui.add(egui::DragValue::new(&mut h).range(1.0..=400.0).suffix(" %")).changed() { property_changes.push((klipp_core::TitleTransformProperty::Height, h / 100.0)); }
            });
            rows::rail_row(ui, theme, "Rotation", |ui| {
                if ui.small_button(mark(&t.rotation_keyframes)).on_hover_text("Animate rotation at the playhead").clicked() { toggles.push(klipp_core::TitleTransformProperty::Rotation); }
                if ui.add(egui::DragValue::new(&mut rotation).speed(0.5).range(-360.0..=360.0).suffix("°")).changed() { property_changes.push((klipp_core::TitleTransformProperty::Rotation, rotation)); }
            });
            rows::rail_row(ui, theme, "Anchor", |ui| {
                if ui.small_button(mark(&t.anchor_y_keyframes)).on_hover_text("Animate the anchor's Y at the playhead").clicked() { toggles.push(klipp_core::TitleTransformProperty::AnchorY); }
                if ui.add(egui::DragValue::new(&mut anchor_y).range(0.0..=100.0).suffix(" Y")).changed() { property_changes.push((klipp_core::TitleTransformProperty::AnchorY, anchor_y / 100.0)); }
                if ui.small_button(mark(&t.anchor_x_keyframes)).on_hover_text("Animate the anchor's X at the playhead").clicked() { toggles.push(klipp_core::TitleTransformProperty::AnchorX); }
                if ui.add(egui::DragValue::new(&mut anchor_x).range(0.0..=100.0).suffix(" X")).changed() { property_changes.push((klipp_core::TitleTransformProperty::AnchorX, anchor_x / 100.0)); }
            });
            ui.horizontal(|ui| {
                ui.add_space(rows::RAIL_PAD);
                if ui.small_button("Center").on_hover_text("Center this layer in the frame").clicked() {
                    cmds.push(AppCommand::SetTitleObjectTransform {
                        clip_id, object_id: object.id.0, center_x: 0.5, center_y: 0.5,
                        size_x: t.size.x, size_y: t.size.y, rotation: t.rotation,
                        anchor_x: t.anchor_norm.x, anchor_y: t.anchor_norm.y,
                    });
                }
                if ui.small_button("Reset transform").clicked() {
                    cmds.push(AppCommand::SetTitleObjectTransform {
                        clip_id, object_id: object.id.0, center_x: t.center.x, center_y: t.center.y,
                        size_x: t.size.x, size_y: t.size.y, rotation: 0.0,
                        anchor_x: 0.5, anchor_y: 0.5,
                    });
                }
            });
        });
        for property in toggles {
            let exists = t.transform_keyframes_ref(property).iter().any(|k| (k.time_secs - clip_time_secs).abs() < 1e-6);
            if exists {
                cmds.push(AppCommand::RemoveTitleObjectTransformKeyframe { clip_id, object_id: object.id.0, property, time_secs: clip_time_secs });
            } else {
                let value = match property { klipp_core::TitleTransformProperty::Width => w / 100.0, klipp_core::TitleTransformProperty::Height => h / 100.0, klipp_core::TitleTransformProperty::Rotation => rotation, klipp_core::TitleTransformProperty::AnchorX => anchor_x / 100.0, klipp_core::TitleTransformProperty::AnchorY => anchor_y / 100.0 };
                cmds.push(AppCommand::AddTitleObjectTransformKeyframe { clip_id, object_id: object.id.0, property, time_secs: clip_time_secs, value });
            }
        }
        for (property, value) in property_changes {
            let has_track = !t.transform_keyframes_ref(property).is_empty();
            if has_track {
                cmds.push(AppCommand::AddTitleObjectTransformKeyframe { clip_id, object_id: object.id.0, property, time_secs: clip_time_secs, value });
            } else {
                cmds.push(AppCommand::SetTitleObjectTransform {
                    clip_id, object_id: object.id.0, center_x: t.center.x, center_y: t.center.y,
                    size_x: if property == klipp_core::TitleTransformProperty::Width { value } else { t.size.x },
                    size_y: if property == klipp_core::TitleTransformProperty::Height { value } else { t.size.y },
                    rotation: if property == klipp_core::TitleTransformProperty::Rotation { value } else { t.rotation },
                    anchor_x: if property == klipp_core::TitleTransformProperty::AnchorX { value } else { t.anchor_norm.x },
                    anchor_y: if property == klipp_core::TitleTransformProperty::AnchorY { value } else { t.anchor_norm.y },
                });
            }
        }
    }

    let tb_target: Option<&klipp_timeline::TextBox> = match kind_ref {
        Some(klipp_timeline::TitleObjectContent::TextBox(tb)) => Some(tb),
        _ => td.primary_text(),
    };
    if target_oid.is_none() && tb_target.is_some() {
        ui.label(
            egui::RichText::new("No layer selected — editing the primary text layer")
                .size(rows::TEXT)
                .color(theme.text_secondary),
        );
        ui.add_space(4.0);
    }
    // The stack shown beside it is the targeted object's — where a
    // layer's motion and effects live — or the primary text's.
    let fx: &klipp_timeline::LayerFx = target_oid
        .and_then(|id| td.object(id))
        .map(klipp_timeline::TitleObject::fx)
        .unwrap_or_else(|| td.primary_fx());
    if let Some(tb) = tb_target {
        crate::widgets::inspector_group::section_card(ui, "TEXT", None, theme, |ui| show_title_pinned_core_textbox(ui, theme, clip_id, td, state, cmds, tb, clip_time_secs));
        crate::widgets::inspector_group::section_card(ui, "POSITION", None, theme, |ui| show_title_position_block(ui, theme, clip_id, td, state, cmds, clip_time_secs));
    }

    // Where this title's look comes from and goes: its style, its
    // template, and copying it between titles.
    crate::widgets::inspector_group::section_card(ui, "STYLE", None, theme, |ui| {
        // The brief's style this title follows (#66), where it differs from
        // it, and the way back to it.
        if let Some(name) = td.style.as_deref() {
            let diverged = state.sequence().all_clips().find(|c| c.id == clip_id).map(|c| klipp_document::ops::style_divergence(state.sequence(), c)).unwrap_or_default();
            ui.horizontal_wrapped(|ui| {
            ui.add_space(rows::RAIL_PAD);
                ui.label(egui::RichText::new(format!("Follows style \u{201c}{name}\u{201d}")).size(rows::TEXT).color(theme.text_secondary))
                    .on_hover_text("A change to this style in the brief carries this title with it, in every field it still has as the style has it");
                // This title's look to every title of its style (#1716); a
                // template's titles have their own line below.
                if td.template.is_none() && ui.small_button("Update All").on_hover_text("Give every title that follows this style this title's look, each keeping its own words").clicked() {
                    cmds.push(AppCommand::UpdateTitlesLike { clip_id });
                }
                if diverged.is_empty() {
                    return;
                }
                ui.label(egui::RichText::new(format!("\u{00b7} differs in {}", diverged.join(", "))).size(rows::TEXT).color(theme.marker_warning));
                if ui.small_button("Revert to Style").on_hover_text("Give this title every field of its style again").clicked() {
                    cmds.push(AppCommand::RevertTitleToStyle { clip_id });
                }
            });
            ui.add_space(6.0);
        }

        // The template this title was made from (#1716), and the way to give
        // its look to every title made from it.
        if let Some(template) = td.template.as_deref() {
            let name = template.strip_prefix("user:").unwrap_or(template);
            ui.horizontal_wrapped(|ui| {
            ui.add_space(rows::RAIL_PAD);
                ui.label(egui::RichText::new(format!("Made from template \u{201c}{name}\u{201d}")).size(rows::TEXT).color(theme.text_secondary));
                if ui.small_button("Update All").on_hover_text("Give every title made from this template this title's look, each keeping its own words").clicked() {
                    cmds.push(AppCommand::UpdateTitlesLike { clip_id });
                }
            });
            ui.add_space(6.0);
        }

        // A title's look, copied, pasted, or taken from another title clicked
        // in the timeline or viewer (#1718); a paste goes on every selected
        // title, each keeping its words.
        ui.horizontal_wrapped(|ui| {
            ui.add_space(rows::RAIL_PAD);
            if ui.small_button("Copy Style").on_hover_text("Keep this title's look to paste on others").clicked() {
                cmds.push(AppCommand::CopyTitleStyle { clip_id });
            }
            let targets: Vec<klipp_core::ClipId> = if state.selected_clips.len() > 1 && state.selected_clips.contains(&clip_id) { state.selected_clips.clone() } else { vec![clip_id] };
            if ui.add_enabled(state.title_style_clipboard.is_some(), egui::Button::new("Paste Style").small())
                .on_hover_text(if targets.len() > 1 { "Give the selected titles the copied look, each keeping its words" } else { "Give this title the copied look, keeping its words" })
                .on_disabled_hover_text("Copy a title's style first")
                .clicked()
            {
                cmds.push(AppCommand::PasteTitleStyle { from: None, to: targets });
            }
            let picking = state.title_style_pick == Some(clip_id);
            if ui.selectable_label(picking, "\u{1f489} Take Style From\u{2026}").on_hover_text("Click a title in the timeline or the viewer to give this one its look").clicked() {
                cmds.push(AppCommand::PickTitleStyle { target: (!picking).then_some(clip_id) });
            }
        });
    });

    // ── Motion and Style: the first tier, always open ──
    if let Some(tb) = tb_target {
        // The text travels its own size at 1080p, as the renderer
        // measures it for a roll.
        let travel = title_tab::Travel::new(state.sequence().resolution, tb.loop_extent(tb.font_size), clip_secs);
        title_tab::show_motion_card(ui, theme, clip_id, &title_tab::ChipSubject::Text(tb.clone()), fx, travel, chips, cmds);
        ui.add_space(8.0);
        // Part of the text, unit by unit (#1510).
        title_animators::show_animators(ui, theme, clip_id, target_oid, fx, clip_secs, clip_time_secs, keyframe_tolerance, cmds);
        ui.add_space(10.0);
        title_tab::show_style_card(ui, theme, clip_id, tb, fx, chips, cmds);
        ui.add_space(10.0);
    }

    // ── Advanced: every other group, collapsed by default ──
    {
        let status = crate::widgets::inspector_group::GroupStatus { count: None, active: false };
    ui.label(
        egui::RichText::new("More text effects, typography and layer controls")
            .size(rows::TEXT)
            .color(theme.text_secondary),
    );
    if !title_group_is_open(ui, theme, clip_id, "Advanced title tools", status) {
            return;
        }
    }
    ui.add_space(4.0);

    // ── Effects group ──
    if let Some(tb) = tb_target {
        show_title_effects_group(ui, theme, clip_id, Some(tb), fx, cmds, target_oid, clip_time_secs, keyframe_tolerance);
        ui.add_space(8.0);
    }

    {
        let style_runs_n = td.style_runs().len() as u32;
        let _grp_count = (td.tracking() != 0.0) as u32 + ((td.line_spacing() - 1.0).abs() > 1e-6) as u32 + (td.baseline_shift() != 0.0) as u32 + (td.case_transform() != klipp_timeline::TextCase::None) as u32 + (td.drop_cap_scale() >= 1.0) as u32 + td.primary_text().map_or(0u32, |tb| { (tb.columns != 1) as u32 + tb.vertical_layout as u32 + (tb.text_direction == klipp_timeline::TextDirection::Rtl) as u32 + tb.underline as u32 + tb.strikethrough as u32 + (tb.paragraph != klipp_timeline::Paragraph::default()) as u32 }) + (style_runs_n > 0) as u32;
        let status = crate::widgets::inspector_group::GroupStatus {
            count: if _grp_count > 0 { Some(_grp_count) } else { None },
            active: td.tracking() != 0.0 || (td.line_spacing() - 1.0).abs() > 1e-6 || td.baseline_shift() != 0.0 || td.case_transform() != klipp_timeline::TextCase::None || td.drop_cap_scale() >= 1.0 || td.primary_text().is_some_and(|tb| tb.columns != 1 || tb.vertical_layout || tb.text_direction == klipp_timeline::TextDirection::Rtl || tb.underline || tb.strikethrough || tb.paragraph != klipp_timeline::Paragraph::default()) || style_runs_n > 0,
        };
        if title_group_is_open(ui, theme, clip_id, "Typography", status) {
            ui.add_space(6.0);
    // Typography
    // Tracking and line spacing, keyable on a layer (#1511): shown as
    // they are at the playhead.
    {
        use klipp_core::title_effect_keys as k;
        let keyed = tb_target.map(|tb| tb.at(clip_time_secs));
        let no_keys: &[klipp_core::Keyframe] = &[];
        let keys = |key: &str| tb_target.and_then(|tb| tb.effect_keyframes_for(key)).unwrap_or(no_keys);
        let ks = |key: &str| target_oid.map(|_| title_object_keyframe_state(keys(key), clip_time_secs, keyframe_tolerance));
        let mut tracking = keyed.as_ref().map_or(td.tracking(), |tb| tb.tracking);
        crate::param_focus::tag_keys(ui, keys(k::TEXT_TRACKING));
        let resp = rows::param_row(ui, theme, "Tracking", &mut tracking, -10.0..=200.0, 0.0, 0.2, 1, " px", ks(k::TEXT_TRACKING));
        handle_text_key_edit(cmds, clip_id, target_oid, k::TEXT_TRACKING, tracking, keys(k::TEXT_TRACKING), clip_time_secs, keyframe_tolerance, (&resp).into(),
            |tracking| AppCommand::SetTitleTracking { clip_id, tracking });
        let mut line_sp = keyed.as_ref().map_or(td.line_spacing(), |tb| tb.line_spacing);
        crate::param_focus::tag_keys(ui, keys(k::TEXT_LINE_SPACING));
        let resp = rows::param_row(ui, theme, "Line spacing", &mut line_sp, 0.5..=4.0, 1.0, 0.05, 2, "x", ks(k::TEXT_LINE_SPACING));
        handle_text_key_edit(cmds, clip_id, target_oid, k::TEXT_LINE_SPACING, line_sp, keys(k::TEXT_LINE_SPACING), clip_time_secs, keyframe_tolerance, (&resp).into(),
            |line_spacing| AppCommand::SetTitleLineSpacing { clip_id, line_spacing });
        // The box's paragraphs (#1721), keyable as the type is.
        let para = keyed.as_ref().map(|tb| tb.paragraph.clone()).unwrap_or_default();
        // A first line below 0 hangs out of the indent.
        for (label, key, value, range) in [
            ("Space before", k::TEXT_SPACE_BEFORE, para.space_before, 0.0..=400.0),
            ("Space after", k::TEXT_SPACE_AFTER, para.space_after, 0.0..=400.0),
            ("Indent", k::TEXT_INDENT_START, para.indent_start, 0.0..=1000.0),
            ("End indent", k::TEXT_INDENT_END, para.indent_end, 0.0..=1000.0),
            ("First line", k::TEXT_INDENT_FIRST, para.indent_first, -1000.0..=1000.0),
        ] {
            let mut v = value;
            let resp = rows::param_row(ui, theme, label, &mut v, range, 0.0, 0.5, 0, " px", ks(key));
            handle_text_key_edit(cmds, clip_id, target_oid, key, v, keys(key), clip_time_secs, keyframe_tolerance, (&resp).into(),
                |value| AppCommand::SetTitleParagraph { clip_id, key: key.into(), value });
        }
        // Fewer one-word last lines (#1721): the fewest words a wrapped
        // paragraph ends on.
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Last line").size(rows::TEXT).color(theme.text_secondary));
            let cur = para.min_last_words.min(4);
            for (v, label, tip) in [(0u8, "Any", "Lines break where they fall"), (2, "2+ words", "A paragraph never ends on one word alone"), (3, "3+ words", "A paragraph ends on three words or more")] {
                if pill_toggle_button(ui, theme, label, cur == v || (v == 0 && cur == 1), |r| r.size(11.0)).on_hover_text(tip).clicked() && cur != v {
                    cmds.push(AppCommand::SetTitleMinLastWords { clip_id, words: v });
                }
            }
        });
        // Its paragraphs as a list (#1721).
        {
            use klipp_timeline::{ListMarker, NumberStyle};
            let (style, indent) = match para.list.as_ref() {
                None => (0u8, keyed.as_ref().map_or(60.0, |tb| (tb.font_size * 1.5).round())),
                Some(l) => (
                    match &l.marker {
                        ListMarker::Bullet { .. } => 1,
                        ListMarker::Numbered { style: NumberStyle::Arabic, .. } => 2,
                        ListMarker::Numbered { style: NumberStyle::LowerAlpha, .. } => 3,
                        ListMarker::Numbered { style: NumberStyle::UpperAlpha, .. } => 4,
                        ListMarker::Numbered { style: NumberStyle::LowerRoman, .. } => 5,
                        ListMarker::Numbered { style: NumberStyle::UpperRoman, .. } => 6,
                    },
                    l.indent,
                ),
            };
            let styles = ["None", "Bullets", "1. 2. 3.", "a. b. c.", "A. B. C.", "i. ii. iii.", "I. II. III."];
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("List").size(rows::TEXT).color(theme.text_secondary));
                egui::ComboBox::from_id_salt(("title_list", clip_id)).selected_text(styles[style as usize % styles.len()]).width(110.0).show_ui(ui, |ui| {
                    for (v, name) in styles.iter().enumerate() {
                        if ui.selectable_label(style as usize == v, *name).clicked() && style as usize != v {
                            cmds.push(AppCommand::SetTitleList { clip_id, style: v as u8, indent });
                        }
                    }
                });
                if style != 0 {
                    let mut i = indent;
                    let resp = ui.add(egui::DragValue::new(&mut i).range(0.0..=2000.0).speed(0.5).fixed_decimals(0).prefix("indent ").suffix(" px"));
                    if resp.drag_started() {
                        cmds.push(AppCommand::BeginTitleObjectDrag { clip_id });
                    }
                    if resp.changed() && i != indent {
                        cmds.push(AppCommand::SetTitleList { clip_id, style, indent: i });
                    }
                    if resp.drag_stopped() {
                        cmds.push(AppCommand::EndTitleObjectDrag { clip_id });
                    }
                }
            });
        }
        // Its tab stops (#1721): where a tab in the text moves what follows.
        let tabs: Vec<(f32, u8)> = para
            .tabs
            .iter()
            .map(|t| (t.at, match t.align {
                klipp_timeline::TabAlign::Left => 0u8,
                klipp_timeline::TabAlign::Center => 1,
                klipp_timeline::TabAlign::Right => 2,
                klipp_timeline::TabAlign::Decimal => 3,
            }))
            .collect();
        let names = ["Left", "Center", "Right", "Decimal"];
        let mut edited: Option<Vec<(f32, u8)>> = None;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Tab stops").size(rows::TEXT).color(theme.text_secondary));
            if ui.small_button("+ Stop").on_hover_text("A tab stop past the last, or at 200 px").clicked() {
                let mut t = tabs.clone();
                t.push((t.last().map_or(200.0, |l| l.0 + 200.0), 0));
                edited = Some(t);
            }
        });
        for (n, &(at, align)) in tabs.iter().enumerate() {
            ui.horizontal(|ui| {
                let mut a = at;
                let resp = ui.add(egui::DragValue::new(&mut a).range(0.0..=4000.0).speed(1.0).fixed_decimals(0).suffix(" px"));
                if resp.drag_started() {
                    cmds.push(AppCommand::BeginTitleObjectDrag { clip_id });
                }
                if resp.changed() && a != at {
                    let mut t = tabs.clone();
                    t[n].0 = a;
                    edited = Some(t);
                }
                if resp.drag_stopped() {
                    cmds.push(AppCommand::EndTitleObjectDrag { clip_id });
                }
                egui::ComboBox::from_id_salt(("tab_align", clip_id, n)).selected_text(names[align as usize % 4]).width(80.0).show_ui(ui, |ui| {
                    for (v, name) in names.iter().enumerate() {
                        if ui.selectable_label(align as usize == v, *name).clicked() && align as usize != v {
                            let mut t = tabs.clone();
                            t[n].1 = v as u8;
                            edited = Some(t);
                        }
                    }
                });
                if ui.small_button("\u{00d7}").on_hover_text("Remove this tab stop").clicked() {
                    let mut t = tabs.clone();
                    t.remove(n);
                    edited = Some(t);
                }
            });
        }
        if let Some(stops) = edited {
            cmds.push(AppCommand::SetTitleTabStops { clip_id, stops });
        }
    }
    let mut write_on = td.write_on();
    rows::rail_row(ui, theme, "Write-on", |ui| {
        if ui.add(egui::Slider::new(&mut write_on, 0.0..=1.0).step_by(0.01)).changed() {
            cmds.push(AppCommand::SetTitleWriteOn { clip_id, write_on });
        }
    });
    let mut baseline = td.baseline_shift();
    rows::rail_row(ui, theme, "Baseline shift", |ui| {
        if ui.add(
            egui::DragValue::new(&mut baseline)
                .range(-200.0..=200.0)
                .speed(0.5)
                .suffix(" px")
                .fixed_decimals(0),
        ).changed() {
            cmds.push(AppCommand::SetTitleBaselineShift { clip_id, shift_px: baseline });
        }
    });
    rows::rail_row(ui, theme, "Columns", |ui| {
        let mut cols = td.primary_text().map_or(1u8, |tb| tb.columns) as i32;
        if ui.add(
            egui::DragValue::new(&mut cols)
                .range(1..=8)
                .speed(0.05),
        ).changed() {
            cmds.push(AppCommand::SetTitleColumns { clip_id, columns: cols.clamp(1, 8) as u8 });
        }
    });
    let vertical = td.primary_text().is_some_and(|tb| tb.vertical_layout);
    rows::rail_row(ui, theme, "Vertical", |ui| {
        if rows::switch(ui, theme, vertical).clicked() {
            cmds.push(AppCommand::SetTitleVerticalLayout { clip_id, vertical: !vertical });
        }
    });
    // Vertical text's columns run from the right, as Japanese and
    // Chinese are set, unless asked otherwise (#1507).
    if vertical {
        let from_left = td.primary_text().is_some_and(|tb| tb.vertical_columns_ltr);
        rows::rail_row(ui, theme, "Columns from the left", |ui| {
            if rows::switch(ui, theme, from_left).clicked() {
                cmds.push(AppCommand::SetTitleVerticalColumnsLtr { clip_id, left_to_right: !from_left });
            }
        });
    }
    rows::rail_row(ui, theme, "Direction", |ui| {
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
    {
        // Drop cap scale: 0 = off, >=1 enlarges first character.
        // Surface as a normal numeric drag clamped to a useful
        // range; the handler folds anything below 1.0 back to 0.0
        // so an off→on→off toggle is a single drag.
        let mut dc = td.drop_cap_scale();
        let resp = rows::rail_row_inner(ui, theme, "Drop cap", |ui| ui.add(
            egui::DragValue::new(&mut dc)
                .range(0.0..=8.0)
                .speed(0.05)
                .fixed_decimals(2)
                .custom_formatter(|n, _| {
                    if n < 1.0 { "off".to_string() } else { format!("{n:.2}\u{00d7}") }
                })
                .custom_parser(|s| {
                    let t = s.trim().to_lowercase();
                    if t == "off" || t.is_empty() { Some(0.0) }
                    else { t.trim_end_matches('×').trim_end_matches("x").trim().parse::<f64>().ok() }
                }),
        ));
        if resp.changed() {
            cmds.push(AppCommand::SetTitleDropCap { clip_id, scale: dc });
        }
    }
    let underline = td.underline();
    rows::rail_row(ui, theme, "Underline", |ui| {
        if rows::switch(ui, theme, underline).clicked() {
            cmds.push(AppCommand::SetTitleUnderline { clip_id, underline: !underline });
        }
    });
    let strikethrough = td.strikethrough();
    rows::rail_row(ui, theme, "Strikethrough", |ui| {
        if rows::switch(ui, theme, strikethrough).clicked() {
            cmds.push(AppCommand::SetTitleStrikethrough { clip_id, strikethrough: !strikethrough });
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
        rows::rail_row(ui, theme, "Case", |ui| {
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
            active: !matches!(td.background, klipp_timeline::TitleBackground::None) || td.objects.iter().any(|o| (o.opacity - 1.0).abs() > 1e-6) || td.mixes_layers(),
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
            let keyframes = obj.opacity_keyframes.as_slice();
            let mut op = obj.evaluated_opacity(clip_time_secs);
            crate::param_focus::tag_keys(ui, keyframes);
            let keyframe_state = title_object_keyframe_state(keyframes, clip_time_secs, keyframe_tolerance);
            let response = rows::param_row(
                ui,
                theme,
                "Layer opacity",
                &mut op,
                0.0..=1.0,
                1.0,
                0.01,
                2,
                "",
                Some(keyframe_state),
            );
            if response.keyframe_toggled {
                if crate::inspector::keyframe_selection::nearest(keyframes, clip_time_secs, keyframe_tolerance).is_some() {
                    cmds.push(AppCommand::RemoveTitleObjectOpacityKeyframe { clip_id, object_id, time_secs: clip_time_secs });
                } else {
                    cmds.push(AppCommand::AddTitleObjectOpacityKeyframe { clip_id, object_id, time_secs: clip_time_secs, value: op });
                }
            } else if response.changed {
                if keyframes.is_empty() {
                    cmds.push(AppCommand::SetTitleObjectOpacity { clip_id, object_id, opacity: op });
                } else {
                    cmds.push(AppCommand::AddTitleObjectOpacityKeyframe { clip_id, object_id, time_secs: clip_time_secs, value: op });
                }
            }
            // How the layer mixes with the layers below it, and the
            // layer that masks it (#1513).
            let mut blend = obj.blend;
            rows::rail_row(ui, theme, "Blend", |ui| {
                egui::ComboBox::from_id_salt(("title_layer_blend", object_id))
                    .selected_text(blend.display_name())
                    .show_ui(ui, |ui| {
                        for mode in klipp_core::BlendMode::ALL {
                            ui.selectable_value(&mut blend, mode, mode.display_name());
                        }
                    });
            });
            if blend != obj.blend {
                cmds.push(AppCommand::SetTitleObjectBlend { clip_id, object_id, blend });
            }
            let label = |o: &klipp_timeline::TitleObject| if o.name.is_empty() { format!("Layer {}", o.id.0 + 1) } else { o.name.clone() };
            let current = obj.mask.map(|m| (m.source.0, m.kind));
            let mut source = current.map(|(id, _)| id);
            let mut kind = current.map_or(klipp_core::layer_mask::LayerMaskKind::Alpha, |(_, k)| k);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Mask with").size(rows::TEXT).color(theme.text_secondary));
                let selected = source.and_then(|id| td.object(klipp_timeline::TitleObjectId(id))).map_or_else(|| "None".to_string(), label);
                egui::ComboBox::from_id_salt(("title_layer_mask", object_id))
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut source, None, "None");
                        for other in td.objects.iter().filter(|o| o.id != obj.id) {
                            ui.selectable_value(&mut source, Some(other.id.0), label(other));
                        }
                    });
                if source.is_some() {
                    egui::ComboBox::from_id_salt(("title_layer_mask_kind", object_id))
                        .selected_text(kind.display_name())
                        .show_ui(ui, |ui| {
                            for k in klipp_core::layer_mask::LayerMaskKind::ALL {
                                ui.selectable_value(&mut kind, k, k.display_name());
                            }
                        });
                }
            });
            let mask = source.map(|id| (id, kind));
            if mask != current {
                cmds.push(AppCommand::SetTitleObjectMask { clip_id, object_id, mask });
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

    // Readability (#2734): the picture darkened or lightened round the text.
    let protect = td.protect.code() as u8;
    ui.horizontal(|ui| {
        ui.label("Readability");
        for (val, label, tip) in [
            (0, "Off", "Leave the picture behind the text as it is"),
            (1, "Auto", "Darken or lighten the picture round the text only where it would be hard to read"),
            (2, "Always", "A soft halo behind the text whatever is under it"),
        ] {
            if ui.selectable_label(protect == val, label).on_hover_text(tip).clicked() && protect != val {
                cmds.push(AppCommand::SetTitleContrastProtect { clip_id, mode: val });
            }
        }
    });
    ui.add_space(8.0);

            ui.add_space(8.0);
        }
    }
    ui.add_space(2.0);

    show_title_motion_blur_group(ui, theme, clip_id, fx, cmds, target_oid, clip_time_secs, keyframe_tolerance);
    ui.add_space(2.0);

    // A metal face (#2706), flat or 3D.
    if let Some(tb) = tb_target {
        let status = crate::widgets::inspector_group::GroupStatus { count: None, active: tb.material.is_some() };
        if title_group_is_open(ui, theme, clip_id, "Metal", status) {
            ui.add_space(6.0);
            title_material::show(ui, theme, clip_id, tb, cmds);
            ui.add_space(8.0);
        }
        ui.add_space(2.0);
    }

    {
        let status = crate::widgets::inspector_group::GroupStatus {
            count: None,
            active: td.extrusion().is_some_and(|e| e.enabled) || td.primary_text().is_some_and(|tb| tb.three_d.as_ref().is_some_and(|t| t.enabled)) || td.primary_text().is_some_and(|tb| tb.chromatic_aberration_px != 0.0 || tb.glitch_intensity != 0.0) || td.primary_fx().motion_blur.as_ref().is_some_and(|m| m.enabled) || td.objects.iter().any(|o| o.transform.rotation != 0.0),
        };
        if title_group_is_open(ui, theme, clip_id, "3D & Distortion", status) {
            ui.add_space(6.0);
    // Real 3D first (#1512): the older extrusion below it.
    if let Some(tb) = tb_target {
        title_text3d::show(ui, theme, clip_id, target_oid, tb, clip_time_secs, keyframe_tolerance, cmds);
        ui.add_space(8.0);
    }
    // 3D Extrusion (pseudo-depth)
    let has_ext = td.extrusion().is_some();
    let ext = td.extrusion().unwrap_or(klipp_timeline::TextExtrusion {
        depth: 10.0,
        angle: 135.0,
        face_color: [0, 0, 0, 0],
        side_color: [80, 60, 40, 255],
        lighting: 0.5,
        enabled: true,
        depth_keyframes: Vec::new(),
        angle_keyframes: Vec::new(),
        lighting_keyframes: Vec::new(),
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
        // Display animated values at the playhead.
        let display_depth = ext.evaluated_depth(clip_time_secs);
        let display_angle = ext.evaluated_angle(clip_time_secs);
        let display_lighting = ext.evaluated_lighting(clip_time_secs);
        let kfs_depth: &[klipp_core::Keyframe] = td
            .primary_text()
            .and_then(|tb| tb.extrusion.as_ref())
            .map(|e| e.depth_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_angle: &[klipp_core::Keyframe] = td
            .primary_text()
            .and_then(|tb| tb.extrusion.as_ref())
            .map(|e| e.angle_keyframes.as_slice())
            .unwrap_or(&[]);
        let kfs_lighting: &[klipp_core::Keyframe] = td
            .primary_text()
            .and_then(|tb| tb.extrusion.as_ref())
            .map(|e| e.lighting_keyframes.as_slice())
            .unwrap_or(&[]);
        let ks_depth = title_object_keyframe_state(kfs_depth, clip_time_secs, keyframe_tolerance);
        let ks_angle = title_object_keyframe_state(kfs_angle, clip_time_secs, keyframe_tolerance);
        let ks_lighting = title_object_keyframe_state(kfs_lighting, clip_time_secs, keyframe_tolerance);

        let mut depth = display_depth;
        let mut angle = display_angle;
        let mut lighting = display_lighting;
        crate::param_focus::tag_keys(ui, kfs_depth);
        let resp_depth = rows::param_row(
            ui, theme, "Depth", &mut depth, 1.0..=50.0, 10.0, 0.5, 1, " px",
            target_oid.map(|_| ks_depth),
        );
        crate::param_focus::tag_keys(ui, kfs_angle);
        let resp_angle = rows::param_row(
            ui, theme, "Angle", &mut angle, 0.0..=360.0, 135.0, 1.0, 0, "\u{00b0}",
            target_oid.map(|_| ks_angle),
        );
        let mut face_col = egui::Color32::from_rgba_premultiplied(ext.face_color[0], ext.face_color[1], ext.face_color[2], ext.face_color[3]);
        let mut side_col = egui::Color32::from_rgba_premultiplied(ext.side_color[0], ext.side_color[1], ext.side_color[2], ext.side_color[3]);
        let mut color_changed = false;
        ui.horizontal(|ui| {
            ui.label("Face:");
            color_changed |= ui.color_edit_button_srgba(&mut face_col).changed();
        });
        ui.horizontal(|ui| {
            ui.label("Side:");
            color_changed |= ui.color_edit_button_srgba(&mut side_col).changed();
        });
        crate::param_focus::tag_keys(ui, kfs_lighting);
        let resp_lighting = rows::param_row(
            ui, theme, "Lighting", &mut lighting, 0.0..=1.0, 0.5, 0.05, 2, "",
            target_oid.map(|_| ks_lighting),
        );
        // Color edit always goes through the static SetTitleExtrusion
        // path — face_color / side_color aren't keyframable in this slice.
        if color_changed {
            cmds.push(AppCommand::SetTitleExtrusion {
                clip_id, enabled: true,
                depth: ext.depth, angle: ext.angle,
                face_color: [face_col.r(), face_col.g(), face_col.b(), face_col.a()],
                side_color: [side_col.r(), side_col.g(), side_col.b(), side_col.a()],
                lighting: ext.lighting,
            });
        }
        handle_extrusion_axis_response(
            cmds, clip_id, target_oid,
            klipp_core::title_effect_keys::EXTRUSION_DEPTH,
            depth, &ext, kfs_depth, clip_time_secs, &resp_depth,
            keyframe_tolerance,
        );
        handle_extrusion_axis_response(
            cmds, clip_id, target_oid,
            klipp_core::title_effect_keys::EXTRUSION_ANGLE,
            angle, &ext, kfs_angle, clip_time_secs, &resp_angle,
            keyframe_tolerance,
        );
        handle_extrusion_axis_response(
            cmds, clip_id, target_oid,
            klipp_core::title_effect_keys::EXTRUSION_LIGHTING,
            lighting, &ext, kfs_lighting, clip_time_secs, &resp_lighting,
            keyframe_tolerance,
        );
    }
    ui.add_space(8.0);

    {
        // Chromatic aberration: 0 = off, sub-pixel = silent, max ~40px.
        let mut ca = td.primary_text().map_or(0.0, |tb| tb.chromatic_aberration_px);
        let resp = rows::rail_row_inner(ui, theme, "Chromatic aberration", |ui| ui.add(
            egui::DragValue::new(&mut ca)
                .range(0.0..=40.0)
                .speed(0.1)
                .fixed_decimals(1)
                .suffix(" px"),
        ));
        if resp.changed() {
            cmds.push(AppCommand::SetTitleChromaticAberration { clip_id, offset_px: ca });
        }
    }
    {
        // Glitch intensity 0..1.
        let mut g = td.primary_text().map_or(0.0, |tb| tb.glitch_intensity);
        let resp = rows::rail_row_inner(ui, theme, "Glitch", |ui| ui.add(
            egui::Slider::new(&mut g, 0.0..=1.0)
                .step_by(0.01),
        ));
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
        // The fixed shapes, then each path layer of the title to follow
        // (#1508).
        let path_layers: Vec<(klipp_timeline::TitleObjectId, String)> = td
            .objects
            .iter()
            .filter(|o| matches!(&o.content, klipp_timeline::TitleObjectContent::Shape(sh) if matches!(sh.kind, klipp_timeline::ShapeKind::Path(_))))
            .map(|o| (o.id, if o.name.is_empty() { format!("Path {}", o.id.0 + 1) } else { o.name.clone() }))
            .collect();
        let path_idx: usize = match &td.text_path() {
            None => 0,
            Some(klipp_timeline::TextPath::Arc { .. }) => 1,
            Some(klipp_timeline::TextPath::Circle { .. }) => 2,
            Some(klipp_timeline::TextPath::Wave { .. }) => 3,
            Some(klipp_timeline::TextPath::Layer { layer, .. }) => path_layers.iter().position(|(id, _)| id == layer).map_or(usize::MAX, |i| 4 + i),
        };
        let mut path_labels: Vec<String> = ["None", "Arc", "Circle", "Wave"].map(String::from).to_vec();
        path_labels.extend(path_layers.iter().map(|(_, name)| name.clone()));
        let mut new_idx = path_idx;
        egui::ComboBox::from_label("Follow")
            .selected_text(path_labels.get(path_idx).map_or("A missing path", |l| l.as_str()))
            .width(120.0)
            .show_ui(ui, |ui| {
                for (i, label) in path_labels.iter().enumerate() {
                    if ui.selectable_label(path_idx == i, label.as_str()).clicked() {
                        new_idx = i;
                    }
                }
            });
        if new_idx != path_idx {
            if let Some((layer, _)) = new_idx.checked_sub(4).and_then(|i| path_layers.get(i)) {
                cmds.push(AppCommand::SetTitleTextPathLayer { clip_id, layer: layer.0, contour: 0, offset: 0.0, align: 0, flip: false });
            } else {
                let (pk, p1, p2, p3) = match new_idx {
                    1 => (1u8, 200.0f32, 0.0f32, 180.0f32),
                    2 => (2, 200.0, 0.0, 0.0),
                    3 => (3, 30.0, 2.0, 0.0),
                    _ => (0, 0.0, 0.0, 0.0),
                };
                cmds.push(AppCommand::SetTitleTextPath { clip_id, path_kind: pk, p1, p2, p3 });
            }
        }
        if ui.button("Draw a path for this text").on_hover_text("Adds a curve the text follows; drag its points to shape it").clicked() {
            cmds.push(AppCommand::DrawTitleTextPath { clip_id });
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
                klipp_timeline::TextPath::Layer { layer, contour, offset, align, flip, .. } => {
                    // Start along the path, keyable so the text travels.
                    use klipp_core::title_effect_keys as k;
                    let no_keys: &[klipp_core::Keyframe] = &[];
                    let keys = td.primary_text().and_then(|tb| tb.effect_keyframes_for(k::TEXT_PATH_OFFSET)).unwrap_or(no_keys);
                    crate::param_focus::tag_keys(ui, keys);
                    let ks = target_oid.map(|_| title_object_keyframe_state(keys, clip_time_secs, keyframe_tolerance));
                    let mut at = path.offset_at(clip_time_secs);
                    let align_id = match align { klipp_timeline::PathTextAlign::Start => 0u8, klipp_timeline::PathTextAlign::Center => 1, klipp_timeline::PathTextAlign::End => 2 };
                    let (layer, contour, offset, flip) = (layer.0, *contour, *offset, *flip);
                    let resp = rows::param_row(ui, theme, "Start", &mut at, 0.0..=1.0, 0.0, 0.005, 3, "", ks);
                    handle_text_key_edit(cmds, clip_id, target_oid, k::TEXT_PATH_OFFSET, at, keys, clip_time_secs, keyframe_tolerance, (&resp).into(),
                        |offset| AppCommand::SetTitleTextPathLayer { clip_id, layer, contour, offset, align: align_id, flip });
                    let mut new_align = align_id;
                    ui.horizontal(|ui| {
                        ui.label("Align");
                        for (id, label) in [(0u8, "Start"), (1, "Centre"), (2, "End")] {
                            ui.selectable_value(&mut new_align, id, label);
                        }
                    });
                    let mut new_flip = flip;
                    ui.checkbox(&mut new_flip, "Flip side");
                    if new_align != align_id || new_flip != flip {
                        cmds.push(AppCommand::SetTitleTextPathLayer { clip_id, layer, contour, offset, align: new_align, flip: new_flip });
                    }
                    if !path_layers.iter().any(|(id, _)| id.0 == layer) {
                        ui.label(egui::RichText::new("Its path layer is gone: the text is laid straight.").size(10.0).color(theme.accent));
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
    // On an audio track a transition is a crossfade, whatever kind it
    // carries: there is no look to choose and no look's parameters.
    let is_crossfade = state
        .sequence()
        .tracks
        .get(track_idx)
        .is_some_and(|t| t.kind == klipp_timeline::TrackKind::Audio);
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
                egui::RichText::new(if is_crossfade { "Crossfade" } else { "Transition" })
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
                egui::RichText::new(if is_crossfade { "CROSSFADE" } else { "TRANSITION" })
                    .size(10.0)
                    .color(theme.text_dim),
            );
            ui.add_space(8.0);

            if is_crossfade {
                ui.label(
                    egui::RichText::new(
                        "Constant power: the outgoing sound plays on into its handle and falls as the incoming one rises from before its in point, holding the level through the join.",
                    )
                    .size(11.0)
                    .color(theme.text_secondary),
                );
            }

            // Kind dropdown
            if !is_crossfade {
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
            }

            ui.add_space(6.0);

            // Duration scrubby
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Duration")
                        .color(theme.text_secondary)
                        .size(11.0),
                );
                // A drag is one edit, made when it ends: the value in
                // flight is held here meanwhile, since each change goes
                // through the op path as an undo step of its own.
                // Shown in frames of the sequence: the duration's ticks
                // are frames only at a whole-number rate, and at
                // 24000/1001 a 24-frame transition is 24024 of them,
                // which clamped to the range and went out as an edit
                // every frame.
                let fps = state.sequence().fps;
                let current = ((transition.duration.to_secs_f64() * fps.as_f64()).round() as u32).max(1);
                let draft_id = ui.id().with(("transition_duration", transition.id));
                let mut frames = ui.data(|d| d.get_temp::<u32>(draft_id)).unwrap_or(current);
                // A typed value lands when the field is left, not on
                // each keystroke: "120" is one edit, not 1, 12 and 120,
                // each checked and perhaps refused in turn.
                let resp = ui.add(
                    egui::DragValue::new(&mut frames)
                        .range(1..=600)
                        .suffix(" fr")
                        .update_while_editing(false),
                );
                if resp.dragged() {
                    ui.data_mut(|d| d.insert_temp(draft_id, frames));
                } else {
                    let done = resp.drag_stopped() || resp.changed();
                    ui.data_mut(|d| d.remove::<u32>(draft_id));
                    if done && frames != current {
                        cmds.push(AppCommand::SetTransitionDuration {
                            track_index: track_idx,
                            transition_id: transition.id,
                            duration_frames: frames,
                        });
                    }
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

            if is_crossfade {
                return;
            }

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
                        egui::RichText::new("Dip Colour")
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
                        egui::RichText::new("Flash Colour")
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
                        egui::RichText::new("Leak Colour")
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
                    | klipp_timeline::TransitionKind::SmoothCut
            ) {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("No additional parameters")
                        .color(theme.text_dim)
                        .size(10.0)
                        .italics(),
                );
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::SmoothCut) {
                ui.label(
                    egui::RichText::new(
                        "Morphs the last frame before the cut into the first after it; the picture holds still meanwhile",
                    )
                    .color(theme.text_dim)
                    .size(10.0),
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
                        egui::RichText::new("Flare Colour")
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
                        egui::RichText::new("Halo Colour")
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
                        egui::RichText::new("Flash Colour")
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
                        egui::RichText::new("Fade Colour")
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
            if matches!(transition.kind, klipp_timeline::TransitionKind::WhipPan) {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Blur")
                            .color(theme.text_secondary)
                            .size(11.0),
                    );
                    let mut v = transition.user_params.param1;
                    let resp = ui.add(egui::DragValue::new(&mut v).range(0.0..=3.0).speed(0.02));
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
            if klipp_core::piece_transition::is_piece_kind(transition.kind) {
                if let Some(user_params) = piece_transition::show(ui, theme, transition.kind, &transition.user_params) {
                    cmds.push(AppCommand::SetTransitionParam { track_index: track_idx, transition_id: transition.id, user_params });
                }
                // The subject's matte: how much of the transition it
                // covers, and Track (#985 phase 3).
                if let Some((_, incoming)) = klipp_compose::subject_matte::transition_subject_of(transition.kind, &transition.user_params) {
                    let side = if incoming { transition.clip_b_id } else { transition.clip_a_id };
                    let card = crate::subject_track::transition_card(transition.id);
                    cmds.extend(crate::subject_track::transition_card_ui(ui, card.as_ref(), transition.id, side, theme));
                }
            }
            if matches!(transition.kind, klipp_timeline::TransitionKind::WaterDrop) {
                ui.add_space(6.0);
                for (label, field, range, speed) in [
                    ("Ring width", 0u8, 0.04..=0.5, 0.005),
                    ("Refraction", 1u8, 0.0..=80.0, 0.5),
                    ("Ripples", 2u8, 0.0..=8.0, 0.1),
                    ("Highlight", 3u8, 0.0..=1.0, 0.01),
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
                            2 => transition.user_params.param2,
                            _ => transition.user_params.param3,
                        };
                        let resp = ui.add(
                            egui::DragValue::new(&mut v).range(range).speed(speed),
                        );
                        if resp.changed() {
                            let mut new_params = transition.user_params;
                            match field {
                                0 => new_params.param0 = v,
                                1 => new_params.param1 = v,
                                2 => new_params.param2 = v,
                                _ => new_params.param3 = v,
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
            if matches!(transition.kind, klipp_timeline::TransitionKind::PushLeft | klipp_timeline::TransitionKind::WhipPan) {
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
                        .size(10.0)
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
                                .size(10.0)
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
            ui.label(egui::RichText::new("Solid Colour").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Colour").strong().size(11.0));
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

        klipp_core::GeneratorData::Gradient { top, bottom, angle_deg, four_point, grain } => {
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
                            grain,
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
                            grain,
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
                                grain,
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
                                grain,
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
                                grain,
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
                                grain,
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
                                grain,
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
                                grain,
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
                                grain,
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
                                grain,
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
            ui.add_space(6.0);
            let mut grain2 = grain;
            if ui
                .add(egui::Slider::new(&mut grain2, 0.0..=klipp_effects::generator_grain::MAX_GENERATOR_GRAIN).text("Grain"))
                .on_hover_text(GENERATOR_GRAIN_HINT)
                .changed()
            {
                cmds.push(AppCommand::UpdateGeneratorData {
                    clip_id,
                    data: klipp_core::GeneratorData::Gradient { top, bottom, angle_deg, four_point, grain: grain2 },
                });
            }
        }

        klipp_core::GeneratorData::ColorBars => {
            ui.label(egui::RichText::new("SMPTE 75% Colour Bars").size(11.0).color(theme.text_secondary));
            ui.add_space(2.0);
            ui.label(egui::RichText::new("No editable parameters.").size(10.0).color(theme.text_dim));
        }

        klipp_core::GeneratorData::ColorBars100 => {
            ui.label(egui::RichText::new("SMPTE 100% Colour Bars").size(11.0).color(theme.text_secondary));
            ui.add_space(2.0);
            ui.label(egui::RichText::new("Full-intensity broadcast bars. No editable parameters.").size(10.0).color(theme.text_dim));
        }

        klipp_core::GeneratorData::ColorBarsSmpte => {
            ui.label(egui::RichText::new("SMPTE Colour Bars").size(11.0).color(theme.text_secondary));
            ui.add_space(2.0);
            ui.label(egui::RichText::new("The full pattern: bars, castellations, −I/white/+Q and PLUGE for setting a monitor. No editable parameters.").size(10.0).color(theme.text_dim));
        }

        klipp_core::GeneratorData::Countdown => {
            ui.label(egui::RichText::new("Countdown Timer").size(11.0).color(theme.text_secondary));
            ui.add_space(2.0);
            ui.label(egui::RichText::new("Duration determined by clip length.").size(10.0).color(theme.text_dim));
        }

        klipp_core::GeneratorData::Timer { mode, initial_seconds, format, text_color, background_color } => {
            ui.label(egui::RichText::new("Timer").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);

            let mut new_mode = mode;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Mode").strong().size(11.0));
                egui::ComboBox::from_id_salt(("timer_mode", clip_id))
                    .selected_text(match new_mode {
                        klipp_core::TimerMode::CountUp => "Count Up",
                        klipp_core::TimerMode::CountDown => "Count Down",
                    })
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(matches!(new_mode, klipp_core::TimerMode::CountUp), "Count Up").clicked() {
                            new_mode = klipp_core::TimerMode::CountUp;
                        }
                        if ui.selectable_label(matches!(new_mode, klipp_core::TimerMode::CountDown), "Count Down").clicked() {
                            new_mode = klipp_core::TimerMode::CountDown;
                        }
                    });
            });

            let mut new_format = format;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Format").strong().size(11.0));
                egui::ComboBox::from_id_salt(("timer_format", clip_id))
                    .selected_text(match new_format {
                        klipp_core::TimerFormat::Seconds => "Seconds (SS)",
                        klipp_core::TimerFormat::MmSs => "MM:SS",
                        klipp_core::TimerFormat::HhMmSs => "HH:MM:SS",
                        klipp_core::TimerFormat::HhMmSsMs => "HH:MM:SS.mmm",
                    })
                    .show_ui(ui, |ui| {
                        for (label, val) in [
                            ("Seconds (SS)", klipp_core::TimerFormat::Seconds),
                            ("MM:SS", klipp_core::TimerFormat::MmSs),
                            ("HH:MM:SS", klipp_core::TimerFormat::HhMmSs),
                            ("HH:MM:SS.mmm", klipp_core::TimerFormat::HhMmSsMs),
                        ] {
                            if ui.selectable_label(new_format == val, label).clicked() {
                                new_format = val;
                            }
                        }
                    });
            });

            let mut new_initial = initial_seconds;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Initial").strong().size(11.0));
                ui.add(egui::Slider::new(&mut new_initial, 0.0..=3600.0).suffix(" s"));
            });

            let mut new_text = text_color;
            let mut new_bg = background_color;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Text").strong().size(11.0));
                let mut c = egui::Color32::from_rgba_premultiplied(new_text[0], new_text[1], new_text[2], new_text[3]);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    new_text = [c.r(), c.g(), c.b(), c.a()];
                }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Background").strong().size(11.0));
                let mut c = egui::Color32::from_rgba_premultiplied(new_bg[0], new_bg[1], new_bg[2], new_bg[3]);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    new_bg = [c.r(), c.g(), c.b(), c.a()];
                }
            });

            if new_mode != mode || new_format != format
                || (new_initial - initial_seconds).abs() > 1e-4
                || new_text != text_color || new_bg != background_color
            {
                cmds.push(AppCommand::UpdateGeneratorData {
                    clip_id,
                    data: klipp_core::GeneratorData::Timer {
                        mode: new_mode,
                        initial_seconds: new_initial,
                        format: new_format,
                        text_color: new_text,
                        background_color: new_bg,
                    },
                });
            }
        }

        klipp_core::GeneratorData::Slate {
            project, scene, take, director, camera, date, framerate, notes,
            text_color, background_color,
        } => {
            ui.label(egui::RichText::new("Slate").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);

            // Each field gets a labelled single-line text input. A
            // change in any one rebuilds the full GeneratorData::Slate.
            let mut p = project;
            let mut sc = scene;
            let mut tk = take;
            let mut dr = director;
            let mut cm = camera;
            let mut dt = date;
            let mut fr = framerate;
            let mut nt = notes;
            let mut new_text = text_color;
            let mut new_bg = background_color;
            let mut changed = false;

            for (label, value) in [
                ("Project",   &mut p),
                ("Scene",     &mut sc),
                ("Take",      &mut tk),
                ("Director",  &mut dr),
                ("Camera",    &mut cm),
                ("Date",      &mut dt),
                ("Framerate", &mut fr),
                ("Notes",     &mut nt),
            ] {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).strong().size(11.0).color(theme.text_dim));
                    if ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY)).changed() {
                        changed = true;
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Text").strong().size(11.0));
                let mut c = egui::Color32::from_rgba_premultiplied(new_text[0], new_text[1], new_text[2], new_text[3]);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    new_text = [c.r(), c.g(), c.b(), c.a()];
                    changed = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Background").strong().size(11.0));
                let mut c = egui::Color32::from_rgba_premultiplied(new_bg[0], new_bg[1], new_bg[2], new_bg[3]);
                if ui.color_edit_button_srgba(&mut c).changed() {
                    new_bg = [c.r(), c.g(), c.b(), c.a()];
                    changed = true;
                }
            });

            if changed {
                cmds.push(AppCommand::UpdateGeneratorData {
                    clip_id,
                    data: klipp_core::GeneratorData::Slate {
                        project: p,
                        scene: sc,
                        take: tk,
                        director: dr,
                        camera: cm,
                        date: dt,
                        framerate: fr,
                        notes: nt,
                        text_color: new_text,
                        background_color: new_bg,
                    },
                });
            }
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

        klipp_core::GeneratorData::LivingGradient { a, b, angle_deg, drift_deg_per_sec, glow, grain } => {
            ui.label(egui::RichText::new("Living Gradient").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);
            let (mut a2, mut b2, mut angle, mut drift, mut glow2, mut grain2) = (a, b, angle_deg, drift_deg_per_sec, glow, grain);
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Colours").strong().size(11.0));
                let mut ca = egui::Color32::from_rgba_premultiplied(a2[0], a2[1], a2[2], a2[3]);
                if ui.color_edit_button_srgba(&mut ca).changed() { a2 = [ca.r(), ca.g(), ca.b(), ca.a()]; changed = true; }
                let mut cb = egui::Color32::from_rgba_premultiplied(b2[0], b2[1], b2[2], b2[3]);
                if ui.color_edit_button_srgba(&mut cb).changed() { b2 = [cb.r(), cb.g(), cb.b(), cb.a()]; changed = true; }
            });
            if ui.add(egui::Slider::new(&mut angle, 0.0..=360.0).text("Angle").suffix("\u{00b0}")).changed() { changed = true; }
            if ui.add(egui::Slider::new(&mut drift, -30.0..=30.0).text("Drift").suffix("\u{00b0}/s")).changed() { changed = true; }
            if ui.add(egui::Slider::new(&mut glow2, 0.0..=1.0).text("Light")).changed() { changed = true; }
            if ui.add(egui::Slider::new(&mut grain2, 0.0..=klipp_effects::generator_grain::MAX_GENERATOR_GRAIN).text("Grain")).on_hover_text(GENERATOR_GRAIN_HINT).changed() { changed = true; }
            if changed {
                cmds.push(AppCommand::UpdateGeneratorData {
                    clip_id,
                    data: klipp_core::GeneratorData::LivingGradient { a: a2, b: b2, angle_deg: angle, drift_deg_per_sec: drift, glow: glow2, grain: grain2 },
                });
            }
        }

        klipp_core::GeneratorData::Grain { color, amount, size_px, speed } => {
            ui.label(egui::RichText::new("Grain").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);
            let (mut col, mut amt, mut size, mut spd) = (color, amount, size_px, speed);
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Colour").strong().size(11.0));
                let mut c = egui::Color32::from_rgba_premultiplied(col[0], col[1], col[2], col[3]);
                if ui.color_edit_button_srgba(&mut c).changed() { col = [c.r(), c.g(), c.b(), c.a()]; changed = true; }
            });
            if ui.add(egui::Slider::new(&mut amt, 0.0..=1.0).text("Amount")).changed() { changed = true; }
            if ui.add(egui::Slider::new(&mut size, 1.0..=8.0).text("Size").suffix(" px")).changed() { changed = true; }
            if ui.add(egui::Slider::new(&mut spd, 0.0..=60.0).text("Speed").suffix(" fps")).changed() { changed = true; }
            if changed {
                cmds.push(AppCommand::UpdateGeneratorData {
                    clip_id,
                    data: klipp_core::GeneratorData::Grain { color: col, amount: amt, size_px: size, speed: spd },
                });
            }
        }

        klipp_core::GeneratorData::Placeholder { label, color } => {
            ui.label(egui::RichText::new("Placeholder").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);
            let mut text = label.clone();
            let mut col = color;
            let mut changed = false;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Label").strong().size(11.0));
                if ui.text_edit_singleline(&mut text).changed() { changed = true; }
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Colour").strong().size(11.0));
                let mut c = egui::Color32::from_rgba_premultiplied(col[0], col[1], col[2], col[3]);
                if ui.color_edit_button_srgba(&mut c).changed() { col = [c.r(), c.g(), c.b(), c.a()]; changed = true; }
            });
            if changed {
                cmds.push(AppCommand::UpdateGeneratorData {
                    clip_id,
                    data: klipp_core::GeneratorData::Placeholder { label: text, color: col },
                });
            }
        }

        klipp_core::GeneratorData::Polygon { sides, color, outline_width_px, fill } => {
            ui.label(egui::RichText::new("Polygon").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);

            let mut sides_val = sides as i32;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Sides").strong().size(11.0));
                if ui.add(egui::Slider::new(&mut sides_val, 3..=24)).changed() {
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::Polygon {
                            sides: sides_val.max(3) as u32,
                            color, outline_width_px, fill,
                        },
                    });
                }
            });

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Colour").strong().size(11.0));
                let mut c = egui::Color32::from_rgba_premultiplied(
                    color[0], color[1], color[2], color[3],
                );
                if ui.color_edit_button_srgba(&mut c).changed() {
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::Polygon {
                            sides, color: [c.r(), c.g(), c.b(), c.a()],
                            outline_width_px, fill,
                        },
                    });
                }
            });

            let mut fill_val = fill;
            if ui.checkbox(&mut fill_val, "Fill").changed() {
                cmds.push(AppCommand::UpdateGeneratorData {
                    clip_id,
                    data: klipp_core::GeneratorData::Polygon {
                        sides, color, outline_width_px,
                        fill: fill_val,
                    },
                });
            }

            let mut stroke_val = outline_width_px;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Outline").strong().size(11.0));
                if ui.add(egui::Slider::new(&mut stroke_val, 0.0..=20.0).suffix(" px")).changed() {
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::Polygon {
                            sides, color, outline_width_px: stroke_val, fill,
                        },
                    });
                }
            });
        }

        klipp_core::GeneratorData::Star { points, inner_radius_pct, color, outline_width_px, fill } => {
            ui.label(egui::RichText::new("Star").size(11.0).color(theme.text_secondary));
            ui.add_space(4.0);

            let mut pts_val = points as i32;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Points").strong().size(11.0));
                if ui.add(egui::Slider::new(&mut pts_val, 3..=16)).changed() {
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::Star {
                            points: pts_val.max(3) as u32,
                            inner_radius_pct, color, outline_width_px, fill,
                        },
                    });
                }
            });

            let mut inner_val = inner_radius_pct;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Inner radius").strong().size(11.0));
                if ui.add(egui::Slider::new(&mut inner_val, 0.05..=0.95).step_by(0.01)).changed() {
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::Star {
                            points, inner_radius_pct: inner_val,
                            color, outline_width_px, fill,
                        },
                    });
                }
            });

            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Colour").strong().size(11.0));
                let mut c = egui::Color32::from_rgba_premultiplied(
                    color[0], color[1], color[2], color[3],
                );
                if ui.color_edit_button_srgba(&mut c).changed() {
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::Star {
                            points, inner_radius_pct,
                            color: [c.r(), c.g(), c.b(), c.a()],
                            outline_width_px, fill,
                        },
                    });
                }
            });

            let mut fill_val = fill;
            if ui.checkbox(&mut fill_val, "Fill").changed() {
                cmds.push(AppCommand::UpdateGeneratorData {
                    clip_id,
                    data: klipp_core::GeneratorData::Star {
                        points, inner_radius_pct, color, outline_width_px,
                        fill: fill_val,
                    },
                });
            }

            let mut stroke_val = outline_width_px;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Outline").strong().size(11.0));
                if ui.add(egui::Slider::new(&mut stroke_val, 0.0..=20.0).suffix(" px")).changed() {
                    cmds.push(AppCommand::UpdateGeneratorData {
                        clip_id,
                        data: klipp_core::GeneratorData::Star {
                            points, inner_radius_pct, color,
                            outline_width_px: stroke_val, fill,
                        },
                    });
                }
            });
        }
    }
}

/// The Inspector's Name row (#920): the clip's own name, typed and
/// committed on Enter or when the field is left, and its colour label as
/// swatches. A blank name gives back the media's.
fn show_clip_name_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &klipp_timeline::Clip,
    targets: &[klipp_core::ClipId],
    edit: &mut Option<(klipp_core::ClipId, String)>,
    cmds: &mut Vec<AppCommand>,
) {
    // Refilled from the clip whenever it isn't being typed in, so an undo
    // or a rename from the clip's menu shows here.
    let field_id = ui.id().with("clip_name_field");
    let typing = ui.memory(|m| m.has_focus(field_id));
    if !typing || edit.as_ref().map(|(id, _)| *id) != Some(clip.id) {
        *edit = Some((clip.id, clip.name.clone().unwrap_or_default()));
    }
    let Some((_, buf)) = edit.as_mut() else { return };
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Name").size(11.0).color(theme.text_secondary));
        let field = ui.add(egui::TextEdit::singleline(buf).id(field_id).hint_text("The media's name").desired_width(f32::INFINITY));
        if field.lost_focus() && buf.trim() != clip.name.as_deref().unwrap_or("").trim() {
            cmds.push(AppCommand::RenameClips { clip_ids: targets.to_vec(), name: Some(buf.clone()) });
        }
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Colour").size(11.0).color(theme.text_secondary));
        for label in klipp_media::ColorLabel::ALL {
            let (rect, response) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
            let chosen = clip.colour_label == label.to_u8();
            match label.rgb() {
                Some((r, g, b)) => { ui.painter().circle_filled(rect.center(), 6.0, egui::Color32::from_rgb(r, g, b)); }
                None => { ui.painter().circle_stroke(rect.center(), 5.5, egui::Stroke::new(1.0_f32, theme.text_secondary)); }
            }
            if chosen {
                ui.painter().circle_stroke(rect.center(), 7.5, egui::Stroke::new(1.5_f32, theme.text_primary));
            }
            if response.on_hover_text(label.label()).clicked() && !chosen {
                cmds.push(AppCommand::SetClipColourLabels { clip_ids: targets.to_vec(), label: label.to_u8() });
            }
        }
    });
    ui.add_space(2.0);
}

#[cfg(test)]
mod selection_underline_tests {
    use super::*;

    /// One frame of the Title tab's pinned block for `td`, with text
    /// `sel` selected in the viewer: the commands it pushed and the
    /// texts it drew, by their centres.
    fn frame(ctx: &egui::Context, td: &klipp_timeline::TitleData, sel: (usize, usize), events: Vec<egui::Event>) -> (Vec<AppCommand>, Vec<(String, egui::Pos2)>) {
        let state = AppState { viewer_text_edit: true, viewer_text_selection: Some(sel), ..Default::default() };
        let tb = td.primary_text().unwrap();
        let mut cmds = Vec::new();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 2000.0))), events, ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show_title_pinned_core_textbox(ui, &KlippTheme::default(), klipp_core::ClipId(uuid::Uuid::nil()), td, &state, &mut cmds, tb, 0.0);
                });
            },
        );
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        (cmds, texts)
    }

    /// A click on the text `label`: what the press and the release
    /// pushed, and the texts drawn before it.
    fn click(ctx: &egui::Context, td: &klipp_timeline::TitleData, sel: (usize, usize), label: &str) -> (Vec<AppCommand>, Vec<String>) {
        let texts = frame(ctx, td, sel, Vec::new()).1;
        let pos = texts.iter().find(|(t, _)| t == label).map(|(_, p)| *p).unwrap_or_else(|| panic!("{label} is drawn"));
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(ctx, td, sel, vec![egui::Event::PointerMoved(pos), button(true)]).0;
        cmds.extend(frame(ctx, td, sel, vec![button(false)]).0);
        (cmds, texts.into_iter().map(|(t, _)| t).collect())
    }

    /// #1721: with text selected, x² sets it as a superscript in one
    /// command and, on a superscript, takes it off; with none selected
    /// neither button shows.
    #[test]
    fn superscript_and_subscript_set_a_selections_run() {
        let ctx = egui::Context::default();
        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "mc2 and H2O".into(), font_name: "Inter".into(), ..Default::default() });
        let (cmds, texts) = click(&ctx, &td, (2, 3), "x\u{00b2}");
        assert!(matches!(&cmds[..], [AppCommand::ApplyTitleStyle { range_start: 2, range_end: 3, property, value, .. }] if property == "script" && value == "superscript"), "{cmds:?}");
        assert!(texts.iter().any(|t| t == "x\u{2082}"), "and subscript beside it");
        td.primary_text_mut().unwrap().style_runs.push(klipp_timeline::StyleRun { script: Some(klipp_timeline::Script::Superscript), ..klipp_timeline::StyleRun::default_for_range(2, 3) });
        let (cmds, _) = click(&ctx, &td, (2, 3), "x\u{00b2}");
        assert!(matches!(&cmds[..], [AppCommand::ApplyTitleStyle { value, .. }] if value == "none"), "a second press takes it off: {cmds:?}");
        let texts = frame(&ctx, &td, (0, 0), Vec::new()).1;
        assert!(!texts.iter().any(|(t, _)| t == "x\u{00b2}"), "no selection, no button");
    }

    /// #1721: the Typography group has the paragraph rows, each showing
    /// the box's setting.
    #[test]
    fn the_typography_group_has_the_paragraph_rows() {
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(egui::Id::new(TITLE_EXPANDED_GROUPS_KEY));
            set.extend(["Advanced title tools".to_string(), "Typography".to_string()]);
        });
        let mut tb = klipp_timeline::TextBox { text: "Title text".into(), font_name: "Inter".into(), ..Default::default() };
        tb.paragraph.indent_first = -36.0;
        tb.paragraph.indent_start = 36.0;
        tb.paragraph.tabs = vec![klipp_timeline::TabStop { at: 240.0, align: klipp_timeline::TabAlign::Decimal }];
        tb.paragraph.list = Some(klipp_timeline::ListStyle { marker: klipp_timeline::ListMarker::Numbered { style: klipp_timeline::NumberStyle::LowerRoman, start: 1, suffix: ".".into() }, indent: 48.0 });
        let td = klipp_timeline::TitleData::single_text(tb);
        let clip = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(klipp_core::Timecode::zero(), klipp_core::Timecode { value: 48, rate: 24 }) };
        let state = AppState::default();
        let mut chips = title_tab::TitleChips::default();
        let mut frame = |events: Vec<egui::Event>| {
            let mut cmds = Vec::new();
            let out = ctx.run(
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(420.0, 6000.0))), events, ..Default::default() },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| show_title_editor(ui, &KlippTheme::default(), &clip, &state, &mut cmds, &mut chips));
                    });
                },
            );
            let texts: Vec<(String, egui::Pos2)> = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
                _ => None,
            }).collect();
            (cmds, texts)
        };
        frame(Vec::new());
        let (_, texts) = frame(Vec::new());
        let has = |label: &str| texts.iter().any(|(t, _)| t == label);
        for label in ["Space before", "Space after", "Indent", "End indent", "First line", "Tab stops", "240 px", "Decimal", "List", "i. ii. iii.", "indent 48 px", "Last line", "2+ words"] {
            assert!(has(label), "no {label} in {:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>());
        }
        assert!(texts.iter().any(|(t, _)| t.contains("-36")), "the hanging first line shows");
        // + Stop adds one past the last.
        let pos = texts.iter().find(|(t, _)| t == "+ Stop").expect("+ Stop is drawn").1;
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        let mut cmds = frame(vec![egui::Event::PointerMoved(pos), button(true)]).0;
        cmds.extend(frame(vec![button(false)]).0);
        assert!(matches!(&cmds[..], [AppCommand::SetTitleTabStops { stops, .. }] if stops == &vec![(240.0, 3), (440.0, 0)]), "{cmds:?}");
    }

    /// #1722: the box's fit is a row of its own: Shrink and Grow each set
    /// it in one command, and a shrinking box shows its minimum size.
    #[test]
    fn the_fit_row_sets_the_boxs_fit() {
        let ctx = egui::Context::default();
        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Title text".into(), font_name: "Inter".into(), ..Default::default() });
        let (cmds, texts) = click(&ctx, &td, (0, 0), "Shrink");
        assert!(matches!(cmds[..], [AppCommand::SetTitleFit { fit: 1, min_size, .. }] if min_size == 24.0), "{cmds:?}");
        assert!(texts.iter().any(|t| t == "Wrap") && texts.iter().any(|t| t == "Grow"), "the row is drawn");
        assert!(!texts.iter().any(|t| t.starts_with("min ")), "no minimum while it wraps");
        td.primary_text_mut().unwrap().fit = klipp_timeline::TextFit::Shrink { min_size: 30.0 };
        let (cmds, texts) = click(&ctx, &td, (0, 0), "Grow");
        assert!(matches!(cmds[..], [AppCommand::SetTitleFit { fit: 2, .. }]), "{cmds:?}");
        assert!(texts.iter().any(|t| t == "min 30"), "a shrinking box shows its minimum: {texts:?}");
    }

    /// #1704: over a selection the Ruby field sets the selection's
    /// reading: typed into, it sends the gloss for the selection's range;
    /// a selection already glossed shows its gloss, and a key edits it;
    /// a selection glossed in part reads as mixed.
    #[test]
    fn the_ruby_field_sets_the_selections_reading() {
        let ctx = egui::Context::default();
        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Title text".into(), font_name: "Inter".into(), ..Default::default() });
        let type_into = |td: &klipp_timeline::TitleData, at: &str, events: Vec<egui::Event>| {
            let (_, texts) = click(&ctx, td, (0, 5), at);
            assert!(texts.iter().any(|t| t == "Ruby"), "the row is drawn: {texts:?}");
            let mut cmds = frame(&ctx, td, (0, 5), events).0;
            cmds.extend(frame(&ctx, td, (0, 5), Vec::new()).0);
            cmds
        };
        let cmds = type_into(&td, "Reading", vec![egui::Event::Text("かき".into())]);
        match &cmds[..] {
            [AppCommand::ApplyTitleStyle { range_start: 0, range_end: 5, property, value, .. }] => assert_eq!((property.as_str(), value.as_str()), ("ruby", "かき")),
            other => panic!("one command, the selection's gloss: {other:?}"),
        }
        // Glossed already: it shows, and Backspace takes off what the
        // caret is after or the click selected.
        td.primary_text_mut().unwrap().style_runs = vec![klipp_timeline::StyleRun { ruby: Some("ふり".into()), ..klipp_timeline::StyleRun::default_for_range(0, 5) }];
        let key = |pressed| egui::Event::Key { key: egui::Key::Backspace, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
        let cmds = type_into(&td, "ふり", vec![key(true), key(false)]);
        assert!(
            matches!(&cmds[..], [AppCommand::ApplyTitleStyle { property, value, .. }] if property == "ruby" && value.len() < "ふり".len() && ("ふり".starts_with(value.as_str()) || "ふり".ends_with(value.as_str()))),
            "{cmds:?}"
        );
        // Glossed in part: mixed.
        td.primary_text_mut().unwrap().style_runs = vec![
            klipp_timeline::StyleRun { ruby: Some("ふり".into()), ..klipp_timeline::StyleRun::default_for_range(0, 2) },
            klipp_timeline::StyleRun::default_for_range(2, 5),
        ];
        let texts = frame(&ctx, &td, (0, 5), Vec::new()).1;
        let row_y = texts.iter().find(|(t, _)| t == "Ruby").unwrap().1.y;
        assert!(texts.iter().any(|(t, p)| t == "Mixed" && (p.y - row_y).abs() < 4.0), "{texts:?}");
    }

    /// #1710: over a selection U underlines the selection's run, in one
    /// command, and a selection part underlined reads as mixed and is
    /// underlined throughout; the box's underline is left alone.
    #[test]
    fn u_over_a_selection_underlines_its_run_in_one_step() {
        let ctx = egui::Context::default();
        let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Title text".into(), font_name: "Inter".into(), ..Default::default() });
        let (cmds, texts) = click(&ctx, &td, (0, 5), "U");
        match &cmds[..] {
            [AppCommand::ApplyTitleStyle { range_start: 0, range_end: 5, property, value, .. }] => assert_eq!((property.as_str(), value.as_str()), ("underline", "true")),
            other => panic!("one command, the run's underline: {other:?}"),
        }
        assert!(texts.iter().any(|t| t == "Small caps"), "the features row is in the block");
        let tb = td.primary_text_mut().unwrap();
        let mut first = klipp_timeline::StyleRun::default_for_range(0, 3);
        first.underline = Some(true);
        tb.style_runs = vec![first, klipp_timeline::StyleRun::default_for_range(3, 5)];
        let (cmds, texts) = click(&ctx, &td, (0, 5), "U");
        assert!(texts.iter().any(|t| t == "Mixed underline"), "{texts:?}");
        assert!(matches!(&cmds[..], [AppCommand::ApplyTitleStyle { property, value, .. }] if property == "underline" && value == "true"), "{cmds:?}");
        td.primary_text_mut().unwrap().style_runs[1].underline = Some(true);
        let (cmds, _) = click(&ctx, &td, (0, 5), "U");
        assert!(matches!(&cmds[..], [AppCommand::ApplyTitleStyle { property, value, .. }] if property == "underline" && value == "false"), "{cmds:?}");
    }
}

#[cfg(test)]
mod text_key_tests {
    use super::*;

    fn edit(cmds: &mut Vec<AppCommand>, target: Option<u32>, keys: &[klipp_core::Keyframe], e: TextKeyEdit) {
        handle_text_key_edit(cmds, klipp_core::ClipId(uuid::Uuid::nil()), target.map(klipp_timeline::TitleObjectId), klipp_core::title_effect_keys::TEXT_TRACKING, 40.0, keys, 1.0, 0.01, e,
            |tracking| AppCommand::SetTitleTracking { clip_id: klipp_core::ClipId(uuid::Uuid::nil()), tracking });
    }

    /// A text row sets its value while it has no keys, keys the playhead
    /// once it has, and its diamond adds or takes away the key there
    /// (#1511).
    #[test]
    fn a_text_row_sets_or_keys_its_value() {
        let key = |t: f64| klipp_core::Keyframe { time_secs: t, value: 0.0, interpolation: klipp_core::Interpolation::Linear, tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
        let changed = || TextKeyEdit { changed: true, ..Default::default() };
        let toggled = || TextKeyEdit { toggled: true, ..Default::default() };
        let run = |target: Option<u32>, keys: &[klipp_core::Keyframe], e: TextKeyEdit| {
            let mut cmds = Vec::new();
            edit(&mut cmds, target, keys, e);
            cmds
        };
        assert!(matches!(run(None, &[], changed())[..], [AppCommand::SetTitleTracking { tracking, .. }] if tracking == 40.0));
        assert!(matches!(run(Some(3), &[], changed())[..], [AppCommand::SetTitleTracking { .. }]));
        assert!(matches!(&run(Some(3), &[key(0.0)], changed())[..], [AppCommand::AddTitleObjectEffectKeyframe { object_id: 3, time_secs, value, param_key, .. }] if *time_secs == 1.0 && *value == 40.0 && param_key == "text.tracking"));
        assert!(matches!(run(Some(3), &[key(0.0)], toggled())[..], [AppCommand::AddTitleObjectEffectKeyframe { .. }]));
        assert!(matches!(run(Some(3), &[key(1.0)], toggled())[..], [AppCommand::RemoveTitleObjectEffectKeyframe { .. }]));
        assert!(run(None, &[], toggled()).is_empty(), "no layer, nothing to key");
    }
}


#[cfg(test)]
mod title_row_style_tests {
    use super::*;

    /// One frame of the whole Title tab for a one-layer title with its
    /// layer selected: the commands, the texts by their centres, and
    /// the hairlines wider than a dash, by their height.
    fn frame(ctx: &egui::Context, clip: &klipp_timeline::Clip, state: &AppState, events: Vec<egui::Event>) -> (Vec<AppCommand>, Vec<(String, egui::Pos2)>, Vec<f32>) {
        let mut cmds = Vec::new();
        let mut chips = title_tab::TitleChips::default();
        let out = ctx.run(
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(380.0, 9000.0))), events, ..Default::default() },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| show_title_editor(ui, &KlippTheme::default(), clip, state, &mut cmds, &mut chips));
            },
        );
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos + t.galley.size() / 2.0)),
            _ => None,
        }).collect();
        let rules = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::LineSegment { points, .. } if points[0].y == points[1].y && (points[1].x - points[0].x).abs() > 100.0 => Some(points[0].y),
            _ => None,
        }).collect();
        (cmds, texts, rules)
    }

    /// #1916: at the inspector's narrowest and its default width, nothing
    /// of the Title tab is drawn past its right edge, where the panel
    /// clips it: every row's controls are in reach.
    #[test]
    fn the_title_tab_fits_the_inspector_at_its_narrow_widths() {
        let (clip, state) = title();
        let mut failures = Vec::new();
        for width in [crate::theme::INSPECTOR_MIN_W, 300.0, crate::theme::INSPECTOR_DEFAULT_W] {
            let ctx = egui::Context::default();
            ctx.data_mut(|d| {
                let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(egui::Id::new(TITLE_EXPANDED_GROUPS_KEY));
                for g in ["Advanced title tools", "Typography", "Text effects", "Background & Layer Opacity", "Motion Blur", "3D & Distortion", "Text Path"] {
                    set.insert(g.to_string());
                }
            });
            let mut cmds = Vec::new();
            let mut chips = title_tab::TitleChips::default();
            let mut run = || ctx.run(
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 9000.0))), ..Default::default() },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| show_title_editor(ui, &KlippTheme::default(), &clip, &state, &mut cmds, &mut chips));
                },
            );
            // A second frame: the first lays out what it learns sizes from.
            run();
            let out = run();
            let texts: Vec<(String, egui::Rect)> = out.shapes.iter().filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_string(), egui::Rect::from_min_size(t.pos, t.galley.size()))),
                _ => None,
            }).collect();
            let past: Vec<String> = out.shapes.iter()
                .map(|s| s.shape.visual_bounding_rect())
                // Not the panel's own background, whose stroke sits on its
                // edge, grown to the widest row (it read 17 px over at every
                // width before this was fixed).
                .filter(|r| r.is_finite() && r.height() < 4000.0 && r.max.x > width + 0.5)
                .map(|r| {
                    let near = texts.iter().filter(|(_, t)| (t.center().y - r.center().y).abs() < 10.0).map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(" | ");
                    format!("x {:.0}..{:.0} at y {:.0} [{near}]", r.min.x, r.max.x, r.center().y)
                })
                .collect();
            if !past.is_empty() {
                failures.push(format!("at {width} px wide, drawn past the right edge:\n{}", past.join("\n")));
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n\n"));
    }

    /// #2943: one click on a number field (the title's Width, "100 %")
    /// gives it the keys, sent as two frames or as one, as a pointer tool
    /// does; and Ctrl+A there selects the field's text, all of it.
    #[test]
    fn a_single_click_on_a_number_field_types_into_it_and_ctrl_a_selects_its_text() {
        let (clip, state) = title();
        let at = |ctx: &egui::Context| {
            frame(ctx, &clip, &state, Vec::new());
            frame(ctx, &clip, &state, Vec::new()).1.into_iter().find(|(t, _)| t == "100 %").map(|(_, p)| p).expect("Width's 100 %")
        };
        let b = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        for one_frame in [false, true] {
            let ctx = egui::Context::default();
            let pos = at(&ctx);
            frame(&ctx, &clip, &state, vec![egui::Event::PointerMoved(pos)]);
            if one_frame {
                frame(&ctx, &clip, &state, vec![b(pos, true), b(pos, false)]);
            } else {
                frame(&ctx, &clip, &state, vec![b(pos, true)]);
                frame(&ctx, &clip, &state, vec![b(pos, false)]);
            }
            frame(&ctx, &clip, &state, Vec::new());
            let focused = ctx.memory(|m| m.focused()).expect("a field has the keys");
            assert!(ctx.wants_keyboard_input(), "one click, typing");
            // Ctrl+A: the field's text, start to end.
            let ctrl = egui::Modifiers { ctrl: true, command: true, ..Default::default() };
            frame(&ctx, &clip, &state, vec![egui::Event::Key { key: egui::Key::A, physical_key: Some(egui::Key::A), pressed: true, repeat: false, modifiers: ctrl }]);
            frame(&ctx, &clip, &state, Vec::new());
            let range = egui::TextEdit::load_state(&ctx, focused).and_then(|s| s.cursor.char_range()).expect("a selection");
            let (a, b) = (range.primary.index.min(range.secondary.index), range.primary.index.max(range.secondary.index));
            assert_eq!((a, b), (0, "100".len()), "all of the field's text");
            assert_eq!(ctx.memory(|m| m.focused()), Some(focused), "and the field keeps the keys");
        }
    }

    fn title() -> (klipp_timeline::Clip, AppState) {
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Title text".into(), font_name: "Inter".into(), ..Default::default() });
        let oid = td.objects[0].id;
        let clip = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(klipp_core::Timecode::zero(), klipp_core::Timecode { value: 48, rate: 24 }) };
        let state = AppState { selected_title_object: Some((clip.id, oid)), ..Default::default() };
        (clip, state)
    }

    /// The layers, transform, text, position and style blocks are
    /// sections with the shared header; every control they had is still
    /// drawn; and the only rules above the first-tier cards are the
    /// sections' own (the rule under the layer list is gone).
    #[test]
    fn the_top_of_the_title_tab_keeps_every_control_in_sections() {
        let ctx = egui::Context::default();
        let (clip, state) = title();
        let (_, texts, rules) = frame(&ctx, &clip, &state, Vec::new());
        let all: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        for label in [
            "LAYERS", "Text 1", "+ Text", "+ Shape", "+ Image", "Save as Template\u{2026}",
            "TRANSFORM", "Width", "Height", "Rotation", "Anchor", "50 X", "50 Y", "Center", "Reset transform",
            "TEXT", "Title text", "Inter", "Font features", "Fit", "Wrap", "Shrink", "Grow",
            "POSITION", "X", "Y", "Lower 3rd", "Top",
            "STYLE", "Copy Style", "Paste Style",
            "ANIMATION",
        ] {
            assert!(all.contains(&label), "{label} is no longer shown: {all:?}");
        }
        let y = |l: &str| texts.iter().find(|(t, _)| t == l).unwrap().1.y;
        let headers = ["LAYERS", "TRANSFORM", "TEXT", "POSITION", "STYLE"];
        let above: Vec<f32> = rules.into_iter().filter(|r| *r < y("ANIMATION") - 10.0).collect();
        assert_eq!(above.len(), headers.len(), "a rule under each header and none between rows: {above:?}");
        for h in headers {
            assert!(above.iter().any(|r| (r - y(h)).abs() < 16.0), "{h}'s own rule: {above:?}");
        }
    }

    /// The advanced groups opened: every row they had is drawn, a value
    /// is no longer its own label ("Columns: 1", "Case:"), the switches
    /// that replaced checkboxes answer real clicks, and the only rules
    /// are the groups' headers'.
    #[test]
    fn the_advanced_groups_keep_every_row() {
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            let set: &mut std::collections::HashSet<String> = d.get_temp_mut_or_default(egui::Id::new(TITLE_EXPANDED_GROUPS_KEY));
            for g in ["Advanced title tools", "Typography", "Text effects", "Background & Layer Opacity", "Motion Blur", "3D & Distortion", "Text Path"] {
                set.insert(g.to_string());
            }
        });
        let (clip, state) = title();
        let (_, texts, rules) = frame(&ctx, &clip, &state, Vec::new());
        let all: Vec<&str> = texts.iter().map(|(t, _)| t.as_str()).collect();
        for label in [
            "Tracking", "Line spacing", "Space before", "Space after", "Indent", "End indent", "First line", "Last line", "List", "Tab stops",
            "Write-on", "Baseline shift", "Columns", "Vertical", "Direction", "Drop cap", "off", "Underline", "Strikethrough", "Case",
            "Quick Style", "Quick", "Layer opacity", "Blend", "Mask with", "Chromatic aberration", "Glitch",
            "Glow", "Outline", "Drop Shadow", "Inner Shadow", "Gradient Fill", "Pattern Fill", "Texture Fill", "3D text",
        ] {
            assert!(all.contains(&label), "{label} is no longer shown: {all:?}");
        }
        for old in ["Columns: 1", "Drop cap: off", "Baseline shift: 0 px", "Case:", "Blend:", "Mask with:", "Write-On"] {
            assert!(!all.contains(&old), "{old} is a label inside its value: {all:?}");
        }
        let y = |l: &str| texts.iter().find(|(t, _)| t == l).unwrap().1.y;
        let groups = ["Advanced title tools", "Text effects", "Typography", "Background & Layer Opacity", "Motion Blur", "3D & Distortion", "Text Path"];
        for r in rules.iter().filter(|r| **r > y("Advanced title tools") - 10.0) {
            assert!(groups.iter().any(|g| (r - y(g)).abs() < 24.0), "a rule at {r} is not a group header's: {rules:?}");
        }

        let switch_of = |l: &str| egui::pos2(380.0 - 8.0 - rows::RAIL_PAD - 16.0, y(l));
        let click = |at: egui::Pos2| {
            let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            let mut cmds = frame(&ctx, &clip, &state, vec![egui::Event::PointerMoved(at), press(true)]).0;
            cmds.extend(frame(&ctx, &clip, &state, vec![press(false)]).0);
            cmds
        };
        let cmds = click(switch_of("Underline"));
        assert!(matches!(cmds.as_slice(), [AppCommand::SetTitleUnderline { underline: true, .. }]), "{cmds:?}");
        let cmds = click(switch_of("Vertical"));
        assert!(matches!(cmds.as_slice(), [AppCommand::SetTitleVerticalLayout { vertical: true, .. }]), "{cmds:?}");
    }

    /// The transform's keyframe marks and the template button answer
    /// real clicks with the commands they sent before.
    #[test]
    fn the_transform_and_template_controls_do_what_they_say() {
        let ctx = egui::Context::default();
        let (clip, state) = title();
        let (_, texts, _) = frame(&ctx, &clip, &state, Vec::new());
        let click = |at: egui::Pos2| {
            let press = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
            let mut cmds = frame(&ctx, &clip, &state, vec![egui::Event::PointerMoved(at), press(true)]).0;
            cmds.extend(frame(&ctx, &clip, &state, vec![press(false)]).0);
            cmds
        };
        let width_y = texts.iter().find(|(t, _)| t == "Width").unwrap().1.y;
        let mark = texts.iter().find(|(t, p)| t == "\u{25c7}" && (p.y - width_y).abs() < 4.0).expect("width's mark").1;
        let cmds = click(mark);
        assert!(matches!(cmds.as_slice(), [AppCommand::AddTitleObjectTransformKeyframe { property: klipp_core::TitleTransformProperty::Width, value, .. }] if *value == 1.0), "{cmds:?}");
        let save = texts.iter().find(|(t, _)| t == "Save as Template\u{2026}").unwrap().1;
        let cmds = click(save);
        assert!(matches!(cmds.as_slice(), [AppCommand::SaveTitleTemplate { .. }]), "{cmds:?}");
    }
}

/// The Inspector fits its column at every width it can be given: nothing
/// it draws runs past the column's right edge, where it would be cut off
/// (a long line that won't wrap widened the whole column, and every
/// right-aligned row after it went off the edge).
#[cfg(test)]
mod fits_its_column_tests {
    use super::*;
    use crate::layout_state::InspectorTab;

    /// The right-most point drawn by one frame of the Inspector in a
    /// column `width` wide, on `tab`, with `clip` selected.
    fn rightmost(width: f32, clip: klipp_timeline::Clip, tab: InspectorTab) -> (f32, f32, Vec<String>) {
        rightmost_with(width, clip, tab, false)
    }

    /// [`rightmost`], with the Keyframes column switched on or off (#2305).
    fn rightmost_with(width: f32, clip: klipp_timeline::Clip, tab: InspectorTab, keys: bool) -> (f32, f32, Vec<String>) {
        let mut state = AppState::default();
        let v = state.sequences[0].tracks.iter().position(|t| t.kind == klipp_timeline::TrackKind::Video).unwrap();
        state.selected_clip = Some(clip.id);
        state.sequences[0].tracks[v].clips.push(clip);
        let prefs = crate::prefs::Preferences::default();
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        let mut panel = NewInspectorPanel { key_column: keys, key_column_w: 150.0, ..Default::default() };
        let mut right = 0.0_f32;
        let mut edge = 0.0_f32;
        let mut over = Vec::new();
        // Twice: the first frame settles the layout's memory.
        for _ in 0..2 {
            let mut active = tab;
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1600.0))), ..Default::default() }, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    edge = ui.max_rect().right();
                    let _ = panel.show(ui, &state, &prefs, &KlippTheme::default(), &mut active);
                });
            });
            right = out.shapes.iter().map(|s| s.shape.visual_bounding_rect()).filter(|r| r.is_finite()).map(|r| r.right()).fold(0.0, f32::max);
            // What runs over, by its text where it has one.
            over = out.shapes.iter().filter(|s| s.shape.visual_bounding_rect().right() > edge + 1.0).map(|s| match &s.shape {
                egui::Shape::Text(t) => format!("text {:?} to {:.0}", t.galley.text(), s.shape.visual_bounding_rect().right()),
                other => format!("{} to {:.0}", match other { egui::Shape::Rect(_) => "rect", egui::Shape::LineSegment { points, .. } => return format!("line {points:?}"), egui::Shape::Path(_) => "path", _ => "shape" }, s.shape.visual_bounding_rect().right()),
            }).collect();
        }
        (right, edge, over)
    }

    /// #2703: a title on a text track offers effects: its EFFECTS tab
    /// has the add menu, and adding one makes the commands a shot's
    /// would.
    #[test]
    fn a_title_offers_effects() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Klipp".into(), ..Default::default() });
        let title = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(tc(0.0), tc(4.0)) };
        let mut state = AppState::default();
        let mut text = klipp_timeline::Track::new("T1", klipp_timeline::TrackKind::Text);
        text.clips.push(title.clone());
        state.sequences[0].tracks.insert(0, text);
        state.selected_clip = Some(title.id);
        let prefs = crate::prefs::Preferences::default();
        let ctx = egui::Context::default();
        crate::icons::install(&ctx);
        let mut panel = NewInspectorPanel::default();
        let mut texts = Vec::new();
        for _ in 0..2 {
            let mut active = InspectorTab::Effects;
            let out = ctx.run(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(360.0, 1600.0))), ..Default::default() }, |ctx| {
                egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
                    let _ = panel.show(ui, &state, &prefs, &KlippTheme::default(), &mut active);
                });
            });
            texts = out.shapes.iter().filter_map(|s| match &s.shape { egui::Shape::Text(t) => Some(t.galley.text().to_string()), _ => None }).collect();
        }
        assert!(texts.iter().any(|t| t == "+ Add effect"), "the title's EFFECTS tab: {texts:?}");
        let cmds = crate::inspector::stack_view::add_effects_command(&state, &title, &[klipp_effects::BuiltinEffect::Glow { radius: 20.0, intensity: 0.5, threshold: 0.0 }]).expect("a title takes an effect");
        assert!(!cmds.is_empty());
    }

    #[test]
    fn nothing_runs_past_the_column_at_its_narrowest() {
        let tc = |s: f64| klipp_core::Timecode::from_secs_f64(s, 24);
        let td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Hello".into(), ..Default::default() });
        let title = klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(tc(0.0), tc(4.0)) };
        let media = klipp_timeline::Clip::media(klipp_core::MediaId::new(), tc(0.0), tc(4.0), tc(0.0), tc(4.0));
        // With an effect on it, and a title of several layers, as the
        // Inspector most often shows them.
        let with_effect = klipp_timeline::test_clip::TestClip::of(media.clone()).effect(klipp_effects::BuiltinEffect::GaussianBlur { radius: 3.0 }).build();
        let layered = {
            let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "A long line of title text".into(), ..Default::default() });
            let mut more = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Second".into(), ..Default::default() });
            // Its own id, as a second layer has.
            more.objects[0].id = klipp_timeline::TitleObjectId(td.objects.iter().map(|o| o.id.0).max().unwrap_or(0) + 1);
            td.objects.append(&mut more.objects);
            klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(tc(0.0), tc(4.0)) }
        };
        // Animated, so the Animated strip and the tab marks are drawn (#2296).
        let animated = klipp_timeline::test_clip::TestClip::of(with_effect.clone()).lane("opacity", 1.0, 0.5).lane("pos_x", 1.0, 10.0).lane("pos_y", 1.0, 10.0).lane("scale", 1.0, 1.5).lane("rotation", 1.0, 30.0).build();
        let animated_title = {
            let mut td = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Moving".into(), ..Default::default() });
            let key = |t: f64, v: f32| klipp_core::Keyframe { time_secs: t, value: v, interpolation: Default::default(), tangent_out: (0.0, 0.0), tangent_in: (0.0, 0.0), handles: Default::default() };
            td.objects[0].transform.center_x_keyframes = vec![key(0.5, 0.2), key(1.5, 0.8)];
            td.objects[0].transform.center_y_keyframes = vec![key(0.5, 0.2), key(1.5, 0.8)];
            klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(td), ..klipp_timeline::Clip::gap(tc(0.0), tc(4.0)) }
        };
        for width in [crate::theme::INSPECTOR_MIN_W, 300.0, 360.0] {
            for tab in [InspectorTab::Title, InspectorTab::Video] {
                let (right, edge, over) = rightmost(width, animated_title.clone(), tab);
                assert!(right <= edge + 1.0, "an animated title's {tab:?} tab at {width}: drawn to {right}, the column ends at {edge}: {over:#?}");
            }
            for tab in [InspectorTab::Video, InspectorTab::Audio, InspectorTab::Effects] {
                // With the Keyframes column on, it fits or stands aside.
                let (right, edge, over) = rightmost_with(width, animated.clone(), tab, true);
                assert!(right <= edge + 1.0, "an animated clip's {tab:?} tab with its keys at {width}: drawn to {right}, the column ends at {edge}: {over:#?}");
                let (right, edge, over) = rightmost(width, animated.clone(), tab);
                assert!(right <= edge + 1.0, "an animated clip's {tab:?} tab at {width}: drawn to {right}, the column ends at {edge}: {over:#?}");
            }
            for tab in [InspectorTab::Title, InspectorTab::Video, InspectorTab::Effects, InspectorTab::Info] {
                let (right, edge, over) = rightmost(width, title.clone(), tab);
                assert!(right <= edge + 1.0, "a title clip's {tab:?} tab at {width}: drawn to {right}, the column ends at {edge}: {over:#?}");
            }
            for tab in [InspectorTab::Video, InspectorTab::Audio, InspectorTab::Color, InspectorTab::Effects, InspectorTab::Info] {
                for (what, clip) in [("media", &media), ("effected", &with_effect)] {
                    let (right, edge, over) = rightmost(width, clip.clone(), tab);
                    assert!(right <= edge + 1.0, "a {what} clip's {tab:?} tab at {width}: drawn to {right}, the column ends at {edge}: {over:#?}");
                }
            }
            for tab in [InspectorTab::Title, InspectorTab::Video] {
                let (right, edge, over) = rightmost(width, layered.clone(), tab);
                assert!(right <= edge + 1.0, "a layered title's {tab:?} tab at {width}: drawn to {right}, the column ends at {edge}: {over:#?}");
            }
        }
    }
}

#[cfg(test)]
mod template_link_rows_tests {
    use super::*;

    /// The words a frame of `ui` paints.
    fn painted(ctx: &egui::Context, mut ui: impl FnMut(&mut egui::Ui)) -> Vec<String> {
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |u| ui(u));
        });
        out.shapes.iter().filter_map(|c| match &c.shape { egui::Shape::Text(t) => Some(t.galley.text().to_string()), _ => None }).collect()
    }

    /// #2757: a placed template's card lists each target under its
    /// control, named as the save offers it, with the badge naming the
    /// control and a hand edit's offset, Unlink and Reset; an unlinked
    /// target says so and offers Relink.
    #[test]
    fn the_card_shows_each_targets_link_and_offset() {
        use klipp_timeline::composition_template::{add_controls, instance_for, template_from_scene, CompositionTemplateStore, ControlSpec};
        let mut scene = klipp_timeline::Sequence::new_default();
        let title = klipp_timeline::TitleData::single_text(klipp_timeline::TextBox { text: "Launch".into(), ..Default::default() });
        scene.tracks[0].clips.push(klipp_timeline::Clip { kind: klipp_timeline::ClipKind::Title(title), ..klipp_timeline::Clip::gap(klipp_core::RationalTime::from_secs_f64(0.0, 24), klipp_core::RationalTime::from_secs_f64(3.0, 24)) });
        let mut template = template_from_scene(&scene, &[], "Card", None).unwrap();
        let controls: Vec<ControlSpec> = serde_json::from_value(serde_json::json!([{ "label": "Grow", "drives": [
            { "target": "Text 1 size", "to": [40, 80] }, { "target": "Text 1 opacity", "to": [0, 1] }] }])).unwrap();
        add_controls(&mut template, &scene, &controls).unwrap();
        let dir = std::env::temp_dir().join(format!("klipp-link-rows-{}", uuid::Uuid::new_v4()));
        let mut state = AppState { composition_templates: CompositionTemplateStore::load(Some(dir.clone())), ..Default::default() };
        state.composition_templates.save(template.clone()).unwrap();
        let mut instance = instance_for(&template);
        instance.act_on_link("object:0.size", klipp_core::LinkAction::Set(50.0)).unwrap();
        instance.act_on_link("object:0.opacity", klipp_core::LinkAction::Unlink).unwrap();
        let mut clip = klipp_timeline::Clip::precomposition(scene.id, klipp_core::RationalTime::from_secs_f64(0.0, 24), klipp_core::RationalTime::from_secs_f64(3.0, 24));
        clip.kind = klipp_timeline::ClipKind::TemplatePrecomposition { sequence_id: scene.id, template_instance: instance };
        let theme = KlippTheme::default();
        let mut cmds = Vec::new();
        let words = painted(&egui::Context::default(), |ui| show_template_controls(ui, &state, &theme, &clip, &mut cmds));
        let _ = std::fs::remove_dir_all(&dir);
        let has = |w: &str| words.iter().any(|x| x == w);
        assert!(has("Grow") && has("Text 1 size") && has("Text 1 opacity"), "{words:?}");
        assert!(has("⇄ Grow +10.00"), "size 50 by hand at 0 (mapped 40): offset +10: {words:?}");
        assert!(has("unlinked") && has("Relink"), "{words:?}");
        assert!(has("Unlink") && has("Reset"), "{words:?}");
        assert!(cmds.is_empty(), "drawing alone changes nothing");
    }
}
