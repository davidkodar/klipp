use crate::theme::KlippTheme;
use crate::widgets::scrubby_param::{scrubby_param_row, KeyframeState};
use klipp_audio::ladspa::LadspaPluginInfo;
use klipp_core::{AppCommand, AudioEffectInstance, ClipId, Keyframe};
use std::collections::HashMap;

/// Build the KeyframeState the scrubby_param widget needs for one
/// audio-effect param. Mirrors `effect_keyframe_state` in the video
/// inspector, but reads from the clip's audio_effect_keyframes map.
fn audio_kf_state(
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    effect_index: usize,
    param_name: &str,
    clip_time_secs: f64,
    keyframe_tolerance: f64,
) -> KeyframeState {
    let key = format!("{effect_index}:{param_name}");
    let kfs = audio_effect_keyframes
        .get(&key)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    crate::inspector::keyframe_selection::state(kfs, clip_time_secs, keyframe_tolerance)
}

/// [`audio_kf_state`] for the row drawn next, which it names as that
/// effect parameter's track for the Inspector to show in focus (#2296).
fn audio_kf_row(
    ui: &egui::Ui,
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    effect_index: usize,
    param_name: &str,
    clip_time_secs: f64,
    keyframe_tolerance: f64,
) -> KeyframeState {
    let keys = audio_effect_keyframes.get(&format!("{effect_index}:{param_name}")).map_or(0, Vec::len);
    if keys > 0 {
        crate::param_focus::tag_counted(ui, crate::state::KeyframeParamKind::AudioEffect { effect_index, param_name: param_name.to_string() }, keys);
    }
    audio_kf_state(audio_effect_keyframes, effect_index, param_name, clip_time_secs, keyframe_tolerance)
}

/// Evaluate a kf'd audio-effect param at the playhead, falling back to
/// the static value when no kfs exist.
fn audio_kf_eval(
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    effect_index: usize,
    param_name: &str,
    clip_time_secs: f64,
    default: f32,
    tolerance: f64,
) -> f32 {
    let key = format!("{effect_index}:{param_name}");
    if let Some(kfs) = audio_effect_keyframes.get(&key) {
        if !kfs.is_empty() {
            return crate::inspector::keyframe_selection::editable_value(kfs, clip_time_secs, default, tolerance);
        }
    }
    default
}

fn has_audio_kfs(
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    effect_index: usize,
    param_name: &str,
) -> bool {
    let key = format!("{effect_index}:{param_name}");
    audio_effect_keyframes.get(&key).is_some_and(|kfs| !kfs.is_empty())
}

/// Dispatch BeginAudioChainDrag / EndAudioChainDrag on drag boundaries
/// so per-tick UpdateAudioEffectParam / SetAudioEffectWetDry calls during
/// the drag skip their own history push. Without this, the user moves
/// a slider once and the history accumulates 60+ entries per second —
/// each one bumps `ops_since_save`, which trips the autosave threshold,
/// which fires "Auto saved" toasts continuously while the slider moves.
fn dispatch_audio_chain_drag(
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
    clip_id: ClipId,
    effect_index: usize,
    param_name: &str,
    cmds: &mut Vec<AppCommand>,
) {
    let drag_key = format!("audio_chain:{}:{}:{}", clip_id.0, effect_index, param_name);
    if resp.drag_started {
        cmds.push(AppCommand::BeginAudioChainDrag { drag_key: drag_key.clone(), clip_id });
    }
    if resp.drag_stopped {
        cmds.push(AppCommand::EndAudioChainDrag { drag_key, clip_id });
    }
}

/// Wire a scrubby_param row's response to the AudioEffectKeyframe
/// command family — auto-update, diamond toggle, prev/next nav. Mirrors
/// the post-loop block in `inspector::effects_tab::show_builtin_effect_params`.
fn dispatch_audio_kf_response(
    resp: &crate::widgets::scrubby_param::ScrubbyParamResponse,
    value: f32,
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    clip_id: ClipId,
    effect_index: usize,
    param_name: &str,
    clip_time_secs: f64,
    clip_timeline_in_secs: f64,
    clip_rate: u32,
    cmds: &mut Vec<AppCommand>,
    keyframe_tolerance: f64,
) {
    // Auto-update: when the user drags the value and kfs exist, lay
    // down a kf at the playhead so the change actually animates.
    if resp.changed && has_audio_kfs(audio_effect_keyframes, effect_index, param_name) {
        cmds.push(AppCommand::AddAudioEffectKeyframe {
            clip_id,
            effect_index,
            param_name: param_name.to_string(),
            time_secs: clip_time_secs,
            value,
        });
    }

    if resp.keyframe_toggled {
        let key = format!("{effect_index}:{param_name}");
        let has_at_playhead = audio_effect_keyframes
            .get(&key)
            .is_some_and(|kfs| crate::inspector::keyframe_selection::nearest(kfs, clip_time_secs, keyframe_tolerance).is_some());
        if has_at_playhead {
            cmds.push(AppCommand::RemoveAudioEffectKeyframe {
                clip_id,
                effect_index,
                param_name: param_name.to_string(),
                time_secs: clip_time_secs,
            });
        } else {
            cmds.push(AppCommand::AddAudioEffectKeyframe {
                clip_id,
                effect_index,
                param_name: param_name.to_string(),
                time_secs: clip_time_secs,
                value,
            });
        }
    }

    if resp.keyframe_nav_prev {
        let key = format!("{effect_index}:{param_name}");
        if let Some(kfs) = audio_effect_keyframes.get(&key) {
            if let Some(prev_t) = crate::inspector::keyframe_selection::state(kfs, clip_time_secs, keyframe_tolerance).prev_keyframe_time
            {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(
                    clip_timeline_in_secs + prev_t,
                    clip_rate,
                )));
            }
        }
    }

    if resp.keyframe_nav_next {
        let key = format!("{effect_index}:{param_name}");
        if let Some(kfs) = audio_effect_keyframes.get(&key) {
            if let Some(next_t) = crate::inspector::keyframe_selection::state(kfs, clip_time_secs, keyframe_tolerance).next_keyframe_time
            {
                cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(
                    clip_timeline_in_secs + next_t,
                    clip_rate,
                )));
            }
        }
    }
}

/// Render the body of the "+ Add Effect" popup. Contents:
///   - Search box (filters by name; for LADSPA also matches maker + label)
///   - "BUILT-IN" section with the 13 built-in audio effects
///   - "LADSPA" section grouped by maker (alphabetical)
///
/// Calls `on_select(effect_type)` when the user picks an item; the caller
/// is responsible for pushing the right `AppCommand::AddAudioEffect`.
/// `popup_anchor_id` keys the search-query memory so each popup keeps its
/// own filter state across frames.
fn add_effect_picker(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    ladspa_plugins: &[LadspaPluginInfo],
    popup_anchor_id: egui::Id,
    on_select: &mut dyn FnMut(String),
) {
    // Width: wide enough for typical LADSPA plugin names without truncation,
    // but not so wide it dominates the inspector. ~280 px matches the
    // patterns used elsewhere in the UI (AI Models dialog).
    ui.set_min_width(280.0);
    ui.set_max_width(320.0);

    // ── Search box ──
    let query_key = popup_anchor_id.with("search_query");
    let mut query: String = ui
        .ctx()
        .data_mut(|d| d.get_temp::<String>(query_key).unwrap_or_default());
    let resp = ui.add(
        egui::TextEdit::singleline(&mut query)
            .hint_text("Search effects...")
            .desired_width(f32::INFINITY),
    );
    if resp.changed() {
        ui.ctx().data_mut(|d| d.insert_temp(query_key, query.clone()));
    }
    let search_lower = query.to_lowercase();

    ui.add_space(4.0);

    // The popup uses CloseOnClick — any button click closes the popup. To
    // avoid losing the popup on every keystroke in the search box, the
    // outer wiring (egui::popup_below_widget) handles this fine because
    // TextEdit::changed() doesn't fire popup-close. But we still want to
    // be defensive: don't bail on empty results, just show a message.

    let max_height = 320.0_f32;
    egui::ScrollArea::vertical()
        .max_height(max_height)
        .show(ui, |ui| {
            // ── Built-in section ──
            let builtins: Vec<(&'static str, &'static str)> = klipp_audio::dsp::available_effects()
                .into_iter()
                .filter(|(_, name)| {
                    search_lower.is_empty() || name.to_lowercase().contains(&search_lower)
                })
                .collect();

            if !builtins.is_empty() {
                ui.label(
                    egui::RichText::new("BUILT-IN")
                        .size(10.0)
                        .strong()
                        .color(theme.text_dim),
                );
                ui.add_space(2.0);
                for (type_id, name) in &builtins {
                    if ui.button(*name).clicked() {
                        on_select(type_id.to_string());
                    }
                }
            }

            // ── LADSPA section, grouped by maker ──
            let mut visible_ladspa: Vec<&LadspaPluginInfo> = ladspa_plugins
                .iter()
                .filter(|p| {
                    if search_lower.is_empty() {
                        return true;
                    }
                    let display = if p.name.is_empty() { &p.label } else { &p.name };
                    display.to_lowercase().contains(&search_lower)
                        || p.maker.to_lowercase().contains(&search_lower)
                        || p.label.to_lowercase().contains(&search_lower)
                })
                .collect();
            visible_ladspa.sort_by(|a, b| {
                a.maker
                    .to_lowercase()
                    .cmp(&b.maker.to_lowercase())
                    .then_with(|| {
                        let an = if a.name.is_empty() { &a.label } else { &a.name };
                        let bn = if b.name.is_empty() { &b.label } else { &b.name };
                        an.to_lowercase().cmp(&bn.to_lowercase())
                    })
            });

            if !visible_ladspa.is_empty() {
                if !builtins.is_empty() {
                    ui.add_space(8.0);
                }
                ui.label(
                    egui::RichText::new("LADSPA")
                        .size(10.0)
                        .strong()
                        .color(theme.text_dim),
                );
                ui.add_space(2.0);

                let mut last_maker: Option<String> = None;
                for plugin in &visible_ladspa {
                    let maker = if plugin.maker.is_empty() {
                        "Other".to_string()
                    } else {
                        plugin.maker.clone()
                    };
                    if last_maker.as_ref() != Some(&maker) {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(maker.to_uppercase())
                                .size(10.0)
                                .color(theme.text_secondary),
                        );
                        last_maker = Some(maker);
                    }

                    let display = if plugin.name.is_empty() {
                        &plugin.label
                    } else {
                        &plugin.name
                    };
                    if ui.button(display).clicked() {
                        let type_id = format!(
                            "ladspa:{}:{}",
                            plugin.path.to_string_lossy(),
                            plugin.label
                        );
                        on_select(type_id);
                    }
                }
            }

            // ── LV2 section ──
            // Pulled from the process-global registry populated at
            // startup so we don't need a `lv2_plugins` parameter on
            // every caller path. Symbol-keyed so future port reorders
            // don't break saved selections.
            let lv2_plugins_all = klipp_audio::lv2::registry_snapshot();
            let mut visible_lv2: Vec<&klipp_audio::lv2::Lv2PluginInfo> = lv2_plugins_all
                .iter()
                .filter(|p| {
                    if search_lower.is_empty() {
                        return true;
                    }
                    p.name.to_lowercase().contains(&search_lower)
                        || p.uri.to_lowercase().contains(&search_lower)
                })
                .collect();
            visible_lv2.sort_by_key(|a| a.name.to_lowercase());

            if !visible_lv2.is_empty() {
                if !builtins.is_empty() || !visible_ladspa.is_empty() {
                    ui.add_space(8.0);
                }
                ui.label(
                    egui::RichText::new("LV2")
                        .size(10.0)
                        .strong()
                        .color(theme.text_dim),
                );
                ui.add_space(2.0);
                for plugin in &visible_lv2 {
                    if ui.button(&plugin.name).clicked() {
                        on_select(plugin.type_id());
                    }
                }
            }

            // ── CLAP section ──
            let clap_plugins_all = klipp_audio::clap::registry_snapshot();
            let mut visible_clap: Vec<&klipp_audio::clap::ClapPluginInfo> = clap_plugins_all
                .iter()
                .filter(|p| {
                    if search_lower.is_empty() {
                        return true;
                    }
                    p.name.to_lowercase().contains(&search_lower)
                        || p.id.to_lowercase().contains(&search_lower)
                        || p.vendor.to_lowercase().contains(&search_lower)
                })
                .collect();
            visible_clap.sort_by_key(|a| a.name.to_lowercase());

            if !visible_clap.is_empty() {
                if !builtins.is_empty() || !visible_ladspa.is_empty() || !visible_lv2.is_empty() {
                    ui.add_space(8.0);
                }
                ui.label(
                    egui::RichText::new("CLAP")
                        .size(10.0)
                        .strong()
                        .color(theme.text_dim),
                );
                ui.add_space(2.0);
                for plugin in &visible_clap {
                    if ui.button(&plugin.name).clicked() {
                        on_select(plugin.type_id());
                    }
                }
            }

            if builtins.is_empty()
                && visible_ladspa.is_empty()
                && visible_lv2.is_empty()
                && visible_clap.is_empty()
            {
                ui.add_space(8.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new(if search_lower.is_empty() {
                            "No audio effects available".to_string()
                        } else {
                            format!("No effects match \"{query}\"")
                        })
                        .size(10.0)
                        .color(theme.text_dim),
                    );
                });
            }
        });

    // Footer count line so the user knows how many things are searchable.
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(format!(
            "{} built-in   |   {} LADSPA   |   {} LV2   |   {} CLAP",
            klipp_audio::dsp::available_effects().len(),
            ladspa_plugins.len(),
            klipp_audio::lv2::registry_snapshot().len(),
            klipp_audio::clap::registry_snapshot().len(),
        ))
        .size(10.0)
        .color(theme.text_dim),
    );
}

/// Show the audio effects chain for a clip.
/// Returns commands for the app to process.
#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    audio_effects: &[AudioEffectInstance],
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    clip_time_secs: f64,
    clip_timeline_in_secs: f64,
    clip_rate: u32,
    ladspa_plugins: &[LadspaPluginInfo],
    cmds: &mut Vec<AppCommand>,
    keyframe_tolerance: f64,
) {
    // ── Section header ──
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("AUDIO EFFECTS")
                .size(10.0)
                .strong()
                .color(theme.text_dim),
        );

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Add Effect dropdown
            let btn = ui.add(
                egui::Button::new(
                    egui::RichText::new("+ Add Effect")
                        .size(10.0)
                        .color(theme.text_accent),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0_f32, theme.accent.linear_multiply(0.5))),
            );
            let popup_id = ui.id().with("add_audio_fx_popup");
            if btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_below_widget(ui, popup_id, &btn, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                add_effect_picker(ui, theme, ladspa_plugins, popup_id, &mut |type_id| {
                    cmds.push(AppCommand::AddAudioEffect { clip_id, effect_type: type_id });
                });
            });
        });
    });

    ui.add_space(4.0);

    show_content(
        ui, theme, clip_id, audio_effects, audio_effect_keyframes,
        clip_time_secs, clip_timeline_in_secs, clip_rate,
        ladspa_plugins, cmds,
        keyframe_tolerance,
    );
}

/// Show the audio effects content without the section header.
/// Used when the header is provided by a section_card wrapper.
///
/// `audio_effect_keyframes` is `clip.audio_effect_keyframes` — passed
/// through so per-param diamonds can read kf state and the bypass +
/// wet/dry diamonds can read their reserved tracks. `clip_time_secs`
/// is clip-relative time at the playhead. `clip_timeline_in_secs` and
/// `clip_rate` are for prev/next-keyframe seek dispatch.
#[allow(clippy::too_many_arguments)]
pub fn show_content(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    audio_effects: &[AudioEffectInstance],
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    clip_time_secs: f64,
    clip_timeline_in_secs: f64,
    clip_rate: u32,
    ladspa_plugins: &[LadspaPluginInfo],
    cmds: &mut Vec<AppCommand>,
    keyframe_tolerance: f64,
) {
    // "+ Add Effect" button
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let btn = ui.add(
                egui::Button::new(
                    egui::RichText::new("+ Add Effect")
                        .size(10.0)
                        .color(theme.text_accent),
                )
                .fill(egui::Color32::TRANSPARENT)
                .stroke(egui::Stroke::new(1.0_f32, theme.accent.linear_multiply(0.5))),
            );
            let popup_id = ui.id().with("add_audio_fx_popup_card");
            if btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            egui::popup_below_widget(ui, popup_id, &btn, egui::PopupCloseBehavior::CloseOnClick, |ui| {
                add_effect_picker(ui, theme, ladspa_plugins, popup_id, &mut |type_id| {
                    cmds.push(AppCommand::AddAudioEffect { clip_id, effect_type: type_id });
                });
            });
        });
    });

    ui.add_space(4.0);

    if audio_effects.is_empty() {
        ui.label(
            egui::RichText::new("No audio effects")
                .size(10.0)
                .color(theme.text_dim),
        );
        return;
    }

    // ── Effect cards ──
    let mut remove_idx = None;
    for (idx, inst) in audio_effects.iter().enumerate() {
        // LADSPA lookup — avoid dlopen-per-frame by short-circuiting on the
        // "ladspa:" prefix and resolving the display name from the discovery
        // cache.
        let ladspa_info: Option<&LadspaPluginInfo> =
            parse_ladspa_type_id(&inst.effect_type).and_then(|(path, label)| {
                ladspa_plugins.iter().find(|p| {
                    p.path.to_string_lossy() == path && p.label == label
                })
            });

        // LV2 lookup mirrors the LADSPA shape but uses URI as the
        // stable identifier (lv2:{uri}). Resolved against the
        // process-global registry populated at startup so we don't
        // need to thread state through.
        let lv2_info: Option<klipp_audio::lv2::Lv2PluginInfo> = inst
            .effect_type
            .strip_prefix("lv2:")
            .and_then(klipp_audio::lv2::lookup_by_uri);

        // CLAP lookup uses the reverse-DNS plugin ID as the stable
        // identifier (clap:{plugin_id}). Same registry pattern.
        let clap_info: Option<klipp_audio::clap::ClapPluginInfo> = inst
            .effect_type
            .strip_prefix("clap:")
            .and_then(klipp_audio::clap::lookup_by_id);

        let effect_name = if let Some(info) = ladspa_info {
            if info.name.is_empty() { info.label.clone() } else { info.name.clone() }
        } else if inst.effect_type.starts_with("ladspa:") {
            // Plugin uninstalled — show the raw effect_type so the user knows
            // what's missing.
            inst.effect_type.clone()
        } else if let Some(ref info) = lv2_info {
            info.name.clone()
        } else if inst.effect_type.starts_with("lv2:") {
            inst.effect_type.clone()
        } else if let Some(ref info) = clap_info {
            info.name.clone()
        } else if inst.effect_type.starts_with("clap:") {
            inst.effect_type.clone()
        } else {
            klipp_audio::dsp::create_effect(&inst.effect_type, 48000)
                .map(|fx| fx.name().to_string())
                .unwrap_or_else(|| inst.effect_type.clone())
        };

        let card_id = ui.id().with(("audio_fx_card", idx));
        let collapsed_key = card_id.with("collapsed");
        let is_collapsed: bool = ui.ctx().data_mut(|d| d.get_temp(collapsed_key).unwrap_or(false));

        egui::Frame::none()
            .fill(theme.bg_raised)
            .stroke(egui::Stroke::new(1.0_f32, theme.border))
            .rounding(4.0)
            .inner_margin(egui::Margin::symmetric(8.0, 6.0))
            .outer_margin(egui::Margin::symmetric(0.0, 2.0))
            .show(ui, |ui| {
                // ── Header row ──
                ui.horizontal(|ui| {
                    // Collapse triangle
                    let arrow = if is_collapsed { "\u{25B6}" } else { "\u{25BC}" };
                    let arrow_resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(arrow)
                                .size(10.0)
                                .color(theme.text_dim),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if arrow_resp.clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(collapsed_key, !is_collapsed));
                    }

                    // Effect name
                    let name_resp = ui.add(
                        egui::Label::new(
                            egui::RichText::new(&effect_name)
                                .size(11.0)
                                .strong()
                                .color(if inst.enabled { theme.text_primary } else { theme.text_dim }),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if name_resp.clicked() {
                        ui.ctx().data_mut(|d| d.insert_temp(collapsed_key, !is_collapsed));
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Remove button
                        let rm_resp = ui.add(
                            egui::Label::new(
                                egui::RichText::new("\u{00D7}")
                                    .size(11.0)
                                    .color(theme.text_dim),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if rm_resp.clicked() {
                            remove_idx = Some(idx);
                        }
                        if rm_resp.hovered() {
                            ui.painter().rect_filled(rm_resp.rect, 2.0, egui::Color32::from_rgba_premultiplied(200, 60, 60, 40));
                        }

                        // Enabled checkbox
                        let check_text = if inst.enabled { "\u{2611}" } else { "\u{2610}" };
                        let check_color = if inst.enabled { theme.accent } else { theme.text_dim };
                        let check_resp = ui.add(
                            egui::Label::new(
                                egui::RichText::new(check_text)
                                    .size(13.0)
                                    .color(check_color),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if check_resp.clicked() {
                            cmds.push(AppCommand::ToggleAudioEffect {
                                clip_id,
                                effect_index: idx,
                            });
                        }

                        // Bypass-keyframe diamond, just left of the enabled
                        // checkbox. Reads the reserved __bypass kf track;
                        // clicking adds/removes a kf at the playhead with
                        // value mirroring the current enabled state.
                        let bypass_kfs = audio_effect_keyframes
                            .get(&format!("{idx}:__bypass"));
                        let has_any_bypass = bypass_kfs.is_some_and(|v| !v.is_empty());
                        let has_at_playhead = bypass_kfs.is_some_and(|v| {
                            crate::inspector::keyframe_selection::nearest(v, clip_time_secs, keyframe_tolerance).is_some()
                        });
                        let diamond_color = if has_at_playhead {
                            theme.accent
                        } else if has_any_bypass {
                            theme.accent_dim
                        } else {
                            theme.text_dim
                        };
                        let dia_resp = ui.add(
                            egui::Label::new(
                                egui::RichText::new("\u{25C6}")
                                    .size(11.0)
                                    .color(diamond_color),
                            )
                            .sense(egui::Sense::click()),
                        );
                        if dia_resp.clicked() {
                            if has_at_playhead {
                                cmds.push(AppCommand::RemoveAudioEffectKeyframe {
                                    clip_id,
                                    effect_index: idx,
                                    param_name: "__bypass".to_string(),
                                    time_secs: clip_time_secs,
                                });
                            } else {
                                // 1.0 = active, 0.0 = bypassed; mirror current enabled state.
                                let value = if inst.enabled { 1.0 } else { 0.0 };
                                cmds.push(AppCommand::AddAudioEffectKeyframe {
                                    clip_id,
                                    effect_index: idx,
                                    param_name: "__bypass".to_string(),
                                    time_secs: clip_time_secs,
                                    value,
                                });
                            }
                        }
                        dia_resp.on_hover_text("Toggle bypass keyframe at playhead");
                    });
                });

                if !is_collapsed {
                    ui.add_space(4.0);

                    // ── Parameters ──
                    if let Some(info) = ladspa_info {
                        render_ladspa_params(
                            ui, theme, clip_id, idx, inst, info,
                            audio_effect_keyframes, clip_time_secs,
                            clip_timeline_in_secs, clip_rate, cmds,
                            keyframe_tolerance,
                        );
                    } else if inst.effect_type.starts_with("ladspa:") {
                        ui.label(
                            egui::RichText::new(format!(
                                "LADSPA plugin not found: {}",
                                inst.effect_type.strip_prefix("ladspa:").unwrap_or("")
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(200, 140, 60)),
                        );
                    } else if let Some(ref info) = lv2_info {
                        render_lv2_params(
                            ui, theme, clip_id, idx, inst, info,
                            audio_effect_keyframes, clip_time_secs,
                            clip_timeline_in_secs, clip_rate, cmds,
                            keyframe_tolerance,
                        );
                    } else if inst.effect_type.starts_with("lv2:") {
                        ui.label(
                            egui::RichText::new(format!(
                                "LV2 plugin not found: {}",
                                inst.effect_type.strip_prefix("lv2:").unwrap_or("")
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(200, 140, 60)),
                        );
                    } else if let Some(ref info) = clap_info {
                        render_clap_params(
                            ui, theme, clip_id, idx, inst, info,
                            audio_effect_keyframes, clip_time_secs,
                            clip_timeline_in_secs, clip_rate, cmds,
                            keyframe_tolerance,
                        );
                    } else if inst.effect_type.starts_with("clap:") {
                        ui.label(
                            egui::RichText::new(format!(
                                "CLAP plugin not found: {}",
                                inst.effect_type.strip_prefix("clap:").unwrap_or("")
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_rgb(200, 140, 60)),
                        );
                    } else if let Some(effect) =
                        klipp_audio::dsp::create_effect(&inst.effect_type, 48000)
                    {
                        let descs = effect.param_descriptors();
                        for desc in descs {
                            // Voice Isolation's mode is a two-way switch, drawn below.
                            if inst.effect_type == "voice_isolation" && desc.name == "mode" { continue; }
                            let static_value = inst.params.iter()
                                .find(|(k, _)| k == desc.name)
                                .map(|(_, v)| *v)
                                .unwrap_or(desc.default);

                            // Display the kf-evaluated value when kfs exist
                            // so the slider reflects the actual signal.
                            let mut val = audio_kf_eval(
                                audio_effect_keyframes,
                                idx,
                                desc.name,
                                clip_time_secs,
                                static_value, keyframe_tolerance);
                            let (display_scale, unit_suffix) = param_display(desc.unit);

                            // Fractions are stored 0..1 and shown as percent;
                            // only the slider sees the scaled value.
                            let range = (desc.max - desc.min) * display_scale;
                            let speed = (range * 0.003) as f64;
                            let decimals = if range > 100.0 { 0 } else if range > 10.0 { 1 } else { 2 };
                            let mut shown = val * display_scale;
                            let kf_state = if desc.is_keyframable() {
                                Some(audio_kf_row(ui, audio_effect_keyframes, idx, desc.name, clip_time_secs, keyframe_tolerance))
                            } else {
                                None
                            };

                            let resp = scrubby_param_row(
                                ui,
                                desc.label,
                                &mut shown,
                                desc.min * display_scale..=desc.max * display_scale,
                                desc.default * display_scale,
                                speed,
                                decimals,
                                unit_suffix,
                                kf_state,
                            );
                            val = shown / display_scale;

                            // Plain drag with no kfs → static param update.
                            // Drag with kfs → AddAudioEffectKeyframe at the
                            // playhead (handled inside dispatch_audio_kf_response).
                            if resp.changed && !has_audio_kfs(audio_effect_keyframes, idx, desc.name) {
                                cmds.push(AppCommand::UpdateAudioEffectParam {
                                    clip_id,
                                    effect_index: idx,
                                    param_name: desc.name.to_string(),
                                    value: val,
                                });
                            }
                            dispatch_audio_chain_drag(&resp, clip_id, idx, desc.name, cmds);
                            dispatch_audio_kf_response(
                                &resp, val, audio_effect_keyframes,
                                clip_id, idx, desc.name,
                                clip_time_secs, clip_timeline_in_secs, clip_rate, cmds,
                                keyframe_tolerance,
                            );
                        }
                    }

                    if inst.effect_type == "denoiser" {
                        show_denoiser_controls(ui, theme, clip_id, idx, inst, clip_time_secs, cmds);
                    }
                    if inst.effect_type == "voice_isolation" {
                        show_voice_isolation_controls(ui, theme, clip_id, idx, inst, cmds);
                    }
                    if inst.effect_type == "normalize" {
                        if let Some(note) = normalize_note(inst) {
                            ui.label(egui::RichText::new(note).size(crate::theme::type_scale::CAPTION).color(theme.text_dim));
                        }
                    }

                    ui.add_space(4.0);

                    // ── Wet/Dry slider with keyframe diamond ──
                    // Reserved param key __wetdry routes to the slot's
                    // wet/dry-keyframe track in the chain.
                    let static_wet_dry = inst.wet_dry;
                    let mut wet_dry = audio_kf_eval(
                        audio_effect_keyframes,
                        idx,
                        "__wetdry",
                        clip_time_secs,
                        static_wet_dry, keyframe_tolerance);
                    let kf_state = Some(audio_kf_row(ui,
                        audio_effect_keyframes, idx, "__wetdry", clip_time_secs,
                        keyframe_tolerance,
                    ));
                    let resp = scrubby_param_row(
                        ui,
                        "Wet/Dry",
                        &mut wet_dry,
                        0.0..=1.0,
                        1.0,
                        0.005,
                        2,
                        "",
                        kf_state,
                    );
                    if resp.changed && !has_audio_kfs(audio_effect_keyframes, idx, "__wetdry") {
                        cmds.push(AppCommand::SetAudioEffectWetDry {
                            clip_id,
                            effect_index: idx,
                            wet_dry,
                        });
                    }
                    dispatch_audio_chain_drag(&resp, clip_id, idx, "__wetdry", cmds);
                    dispatch_audio_kf_response(
                        &resp, wet_dry, audio_effect_keyframes,
                        clip_id, idx, "__wetdry",
                        clip_time_secs, clip_timeline_in_secs, clip_rate, cmds,
                        keyframe_tolerance,
                    );
                }
            });
    }

    if let Some(idx) = remove_idx {
        cmds.push(AppCommand::RemoveAudioEffect {
            clip_id,
            effect_index: idx,
        });
    }
}

/// Parse `"ladspa:{path}:{label}"` into `(path, label)`. Returns None for
/// non-LADSPA effect_types or malformed strings. Splits on the LAST `:` so
/// Windows paths with drive letters don't get chopped.
fn parse_ladspa_type_id(type_id: &str) -> Option<(&str, &str)> {
    let rest = type_id.strip_prefix("ladspa:")?;
    let last_colon = rest.rfind(':')?;
    Some((&rest[..last_colon], &rest[last_colon + 1..]))
}

/// Render parameter sliders for a LADSPA plugin. Uses the port metadata
/// from discovery (min / max / integer / logarithmic / toggle) to pick
/// an appropriate widget per port. Toggle ports stay as plain
/// checkboxes (no obvious kf semantic). All other ports get the
/// keyframe-aware scrubby_param row.
#[allow(clippy::too_many_arguments)]
fn render_ladspa_params(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    effect_index: usize,
    inst: &AudioEffectInstance,
    info: &LadspaPluginInfo,
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    clip_time_secs: f64,
    clip_timeline_in_secs: f64,
    clip_rate: u32,
    cmds: &mut Vec<AppCommand>,
    keyframe_tolerance: f64,
) {
    if info.control_inputs.is_empty() {
        ui.label(
            egui::RichText::new("(no parameters)")
                .size(10.0)
                .color(theme.text_dim),
        );
        return;
    }

    for port in &info.control_inputs {
        let static_value = inst
            .params
            .iter()
            .find(|(k, _)| k == &port.name)
            .map(|(_, v)| *v)
            .unwrap_or(port.default);

        let label = if port.name.is_empty() {
            format!("port {}", port.index)
        } else {
            port.name.clone()
        };

        if port.is_toggle {
            // Toggles get a checkbox — no kf for binary state (use
            // bypass-kf at the slot level instead).
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(&label)
                        .size(10.0)
                        .color(theme.text_secondary),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut on = static_value >= 0.5;
                    let resp = ui.checkbox(&mut on, "");
                    if resp.changed() {
                        cmds.push(AppCommand::UpdateAudioEffectParam {
                            clip_id,
                            effect_index,
                            param_name: port.name.clone(),
                            value: if on { 1.0 } else { 0.0 },
                        });
                    }
                });
            });
            continue;
        }

        // Non-toggle ports: keyframable scrubby_param row.
        let mut val = audio_kf_eval(
            audio_effect_keyframes,
            effect_index,
            &port.name,
            clip_time_secs,
            static_value, keyframe_tolerance);
        let range = (port.max - port.min).max(f32::EPSILON);
        let speed = (range * 0.003).max(0.0001) as f64;
        let decimals = if port.is_integer {
            0
        } else if range > 100.0 {
            1
        } else if range > 10.0 {
            2
        } else {
            3
        };
        let unit_suffix = match klipp_audio::ladspa::unit_hint_for_port_name(&port.name) {
            klipp_audio::dsp::ParamUnit::Db => " dB",
            klipp_audio::dsp::ParamUnit::Hz => " Hz",
            klipp_audio::dsp::ParamUnit::Ms => " ms",
            klipp_audio::dsp::ParamUnit::Percent | klipp_audio::dsp::ParamUnit::Fraction => "%",
            klipp_audio::dsp::ParamUnit::Ratio => ":1",
            klipp_audio::dsp::ParamUnit::None => "",
        };
        let kf_state = Some(audio_kf_row(ui,
            audio_effect_keyframes, effect_index, &port.name, clip_time_secs,
            keyframe_tolerance,
        ));

        let resp = scrubby_param_row(
            ui,
            &label,
            &mut val,
            port.min..=port.max,
            port.default,
            speed,
            decimals,
            unit_suffix,
            kf_state,
        );

        if resp.changed && !has_audio_kfs(audio_effect_keyframes, effect_index, &port.name) {
            cmds.push(AppCommand::UpdateAudioEffectParam {
                clip_id,
                effect_index,
                param_name: port.name.clone(),
                value: val,
            });
        }
        dispatch_audio_chain_drag(&resp, clip_id, effect_index, &port.name, cmds);
        dispatch_audio_kf_response(
            &resp, val, audio_effect_keyframes,
            clip_id, effect_index, &port.name,
            clip_time_secs, clip_timeline_in_secs, clip_rate, cmds,
            keyframe_tolerance,
        );
    }
}

/// Render LV2 control-input ports as keyframable scrubby_param rows.
/// Values are addressed in `inst.params` by stable `lv2:symbol`
/// (NEVER by index) so a future plugin version that reorders ports
/// doesn't break saved projects.
#[allow(clippy::too_many_arguments)]
fn render_lv2_params(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    effect_index: usize,
    inst: &AudioEffectInstance,
    info: &klipp_audio::lv2::Lv2PluginInfo,
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    clip_time_secs: f64,
    clip_timeline_in_secs: f64,
    clip_rate: u32,
    cmds: &mut Vec<AppCommand>,
    keyframe_tolerance: f64,
) {
    if info.control_inputs.is_empty() {
        ui.label(
            egui::RichText::new("(no parameters)")
                .size(10.0)
                .color(theme.text_dim),
        );
        return;
    }

    for port in &info.control_inputs {
        // KEY by symbol — NOT by name. The symbol is the stable
        // contract across plugin versions.
        let static_value = inst
            .params
            .iter()
            .find(|(k, _)| k == &port.symbol)
            .map(|(_, v)| *v)
            .unwrap_or(port.default);

        let label = if port.name.is_empty() {
            port.symbol.clone()
        } else {
            port.name.clone()
        };

        // LV2 ports use Option for min/max — fall back to a neutral
        // [0, 1] window when absent (uncommon but legal per spec).
        let min = port.min.unwrap_or(0.0);
        let max = port.max.unwrap_or(1.0).max(min + f32::EPSILON);

        let mut val = audio_kf_eval(
            audio_effect_keyframes,
            effect_index,
            &port.symbol,
            clip_time_secs,
            static_value, keyframe_tolerance);
        let range = max - min;
        let speed = (range * 0.003).max(0.0001) as f64;
        let decimals = if range > 100.0 {
            1
        } else if range > 10.0 {
            2
        } else {
            3
        };
        let kf_state = Some(audio_kf_row(ui,
            audio_effect_keyframes,
            effect_index,
            &port.symbol,
            clip_time_secs,
            keyframe_tolerance,
        ));

        let resp = scrubby_param_row(
            ui,
            &label,
            &mut val,
            min..=max,
            port.default,
            speed,
            decimals,
            "",
            kf_state,
        );

        if resp.changed && !has_audio_kfs(audio_effect_keyframes, effect_index, &port.symbol) {
            cmds.push(AppCommand::UpdateAudioEffectParam {
                clip_id,
                effect_index,
                param_name: port.symbol.clone(),
                value: val,
            });
        }
        dispatch_audio_chain_drag(&resp, clip_id, effect_index, &port.symbol, cmds);
        dispatch_audio_kf_response(
            &resp, val, audio_effect_keyframes,
            clip_id, effect_index, &port.symbol,
            clip_time_secs, clip_timeline_in_secs, clip_rate, cmds,
            keyframe_tolerance,
        );
    }
}

/// Render CLAP control parameters as keyframable scrubby_param rows.
/// Values are addressed in `inst.params` by stable u32 ParamID
/// (hex-formatted). v1 stub runtime — these sliders update the
/// saved param values, but the audio process pass-throughs until
/// CLAP-NEXT-RUNTIME-WIRING ships.
#[allow(clippy::too_many_arguments)]
fn render_clap_params(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    effect_index: usize,
    inst: &AudioEffectInstance,
    info: &klipp_audio::clap::ClapPluginInfo,
    audio_effect_keyframes: &HashMap<String, Vec<Keyframe>>,
    clip_time_secs: f64,
    clip_timeline_in_secs: f64,
    clip_rate: u32,
    cmds: &mut Vec<AppCommand>,
    keyframe_tolerance: f64,
) {
    if info.params.is_empty() {
        ui.label(
            egui::RichText::new(
                "(CLAP runtime not yet wired — discovery + UI ready, audio passthrough until \
                 CLAP-NEXT-RUNTIME-WIRING ships)",
            )
            .size(10.0)
            .color(theme.text_dim),
        );
        return;
    }

    for port in &info.params {
        let key = port.key();
        let static_value = inst
            .params
            .iter()
            .find(|(k, _)| k == &key)
            .map(|(_, v)| *v)
            .unwrap_or(port.default);

        let label = if port.name.is_empty() {
            key.clone()
        } else {
            port.name.clone()
        };
        let mut val = audio_kf_eval(
            audio_effect_keyframes,
            effect_index,
            &key,
            clip_time_secs,
            static_value, keyframe_tolerance);
        let range = (port.max - port.min).max(f32::EPSILON);
        let speed = (range * 0.003).max(0.0001) as f64;
        let decimals = if range > 100.0 {
            1
        } else if range > 10.0 {
            2
        } else {
            3
        };
        let kf_state = Some(audio_kf_row(ui,
            audio_effect_keyframes,
            effect_index,
            &key,
            clip_time_secs,
            keyframe_tolerance,
        ));

        let resp = scrubby_param_row(
            ui,
            &label,
            &mut val,
            port.min..=port.max,
            port.default,
            speed,
            decimals,
            "",
            kf_state,
        );

        if resp.changed && !has_audio_kfs(audio_effect_keyframes, effect_index, &key) {
            cmds.push(AppCommand::UpdateAudioEffectParam {
                clip_id,
                effect_index,
                param_name: key.clone(),
                value: val,
            });
        }
        dispatch_audio_chain_drag(&resp, clip_id, effect_index, &key, cmds);
        dispatch_audio_kf_response(
            &resp, val, audio_effect_keyframes,
            clip_id, effect_index, &key,
            clip_time_secs, clip_timeline_in_secs, clip_rate, cmds,
            keyframe_tolerance,
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// Helper logic tests — diamond rendering itself needs egui to spin
// up which makes a full UI unit test heavy. Pin the pure helpers
// (state lookup, kf evaluation, has-kfs check) so the diamond's
// behavior is correct even when the visual smoke test isn't run.
// ═══════════════════════════════════════════════════════════════════
/// How a built-in parameter is shown: a scale from its stored value to
/// the slider, and the unit suffix. Fractions (0..1) read as percentages.
fn param_display(unit: klipp_audio::dsp::ParamUnit) -> (f32, &'static str) {
    use klipp_audio::dsp::ParamUnit;
    match unit {
        ParamUnit::Db => (1.0, " dB"),
        ParamUnit::Hz => (1.0, " Hz"),
        ParamUnit::Ms => (1.0, " ms"),
        ParamUnit::Percent => (1.0, "%"),
        ParamUnit::Fraction => (100.0, "%"),
        ParamUnit::Ratio => (1.0, ":1"),
        ParamUnit::None => (1.0, ""),
    }
}

fn show_voice_isolation_controls(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    idx: usize,
    inst: &AudioEffectInstance,
    cmds: &mut Vec<AppCommand>,
) {
    use klipp_audio::dsp::voice_isolation as vi;
    let param = |name: &str, default: f32| {
        inst.params.iter().find(|(k, _)| k == name).map(|(_, v)| *v).unwrap_or(default)
    };
    let reduction = param("reduction", 30.0);
    let remove_voice = param("mode", 0.0) > 0.5;

    ui.add_space(4.0);
    if !vi::model_installed() {
        ui.label(
            egui::RichText::new("Needs the DeepFilterNet3 model (8 MB). Klipp downloads it only when you ask; until then this effect leaves the sound as it is.")
                .size(10.0)
                .color(egui::Color32::from_rgb(200, 140, 60)),
        );
        if ui.button("Download model\u{2026}").clicked() {
            cmds.push(AppCommand::OpenModelDownloadDialog { model_id: vi::MODEL_ID.to_string() });
        }
        ui.add_space(4.0);
    }

    ui.label(egui::RichText::new("MODE").size(10.0).strong().color(theme.text_dim));
    ui.horizontal(|ui| {
        for (label, hint, value) in [
            ("Isolate voice", "Keep the speech, lower everything else", 0.0),
            ("Remove voice", "Keep everything else, lower the speech", 1.0),
        ] {
            let active = (value > 0.5) == remove_voice;
            if ui.selectable_label(active, label).on_hover_text(hint).clicked() && !active {
                cmds.push(AppCommand::SetAudioEffectParams {
                    clip_id,
                    effect_index: idx,
                    params: vec![("mode".into(), value)],
                });
            }
        }
    });

    ui.add_space(4.0);
    ui.label(egui::RichText::new("PRESETS").size(10.0).strong().color(theme.text_dim));
    audio_preset_buttons(ui, "voice_isolation", clip_id, idx, inst, cmds);
    let what = if remove_voice { "speech" } else { "background" };
    let amount = if reduction >= vi::FULL_REDUCTION_DB {
        format!("The {what} is removed as far as the model can.")
    } else {
        format!("The {what} is lowered by up to {reduction:.0} dB.")
    };
    ui.label(egui::RichText::new(format!("{amount} Adds 40 ms of delay, compensated on export.")).size(10.0).color(theme.text_secondary));
}

/// The preset buttons of the audio effect `type_id` (#2887): Klipp's
/// presets file's, each lit when the effect stands at its settings, a
/// click setting them.
fn audio_preset_buttons(ui: &mut egui::Ui, type_id: &str, clip_id: ClipId, idx: usize, inst: &AudioEffectInstance, cmds: &mut Vec<AppCommand>) {
    let now = |name: &str| inst.params.iter().find(|(k, _)| k == name).map(|(_, v)| *v);
    ui.horizontal_wrapped(|ui| {
        for preset in klipp_core::audio_effect_registry::presets_of(type_id) {
            let values = preset.values();
            let active = values.iter().all(|(k, v)| now(k).is_some_and(|n| (n - v).abs() <= 0.005f32.max(v.abs() * 0.002)));
            if ui.selectable_label(active, &preset.name).on_hover_text(&preset.description).clicked() {
                cmds.push(AppCommand::SetAudioEffectParams { clip_id, effect_index: idx, params: values });
            }
        }
    });
}


fn show_denoiser_controls(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip_id: ClipId,
    idx: usize,
    inst: &AudioEffectInstance,
    clip_time_secs: f64,
    cmds: &mut Vec<AppCommand>,
) {
    let param = |name: &str, default: f32| {
        inst.params.iter().find(|(k, _)| k == name).map(|(_, v)| *v).unwrap_or(default)
    };
    let learned = param("profile_learned", 0.0) > 0.5;
    let learned_at = param("profile_at", -1.0);

    ui.add_space(4.0);
    ui.label(egui::RichText::new("PRESETS").size(10.0).strong().color(theme.text_dim));
    audio_preset_buttons(ui, "denoiser", clip_id, idx, inst, cmds);

    ui.add_space(4.0);
    ui.label(egui::RichText::new("NOISE PROFILE").size(10.0).strong().color(theme.text_dim));
    let status = if learned {
        let secs = learned_at.max(0.0);
        format!(
            "Learned from {}:{:04.1} into the clip. Reduction and floor apply to that noise.",
            (secs / 60.0).floor() as u32,
            secs % 60.0,
        )
    } else {
        "Adaptive: tracks the steady noise floor automatically. Learn a profile for the most precise result.".to_string()
    };
    ui.label(egui::RichText::new(status).size(10.0).color(theme.text_secondary));
    ui.horizontal_wrapped(|ui| {
        if ui
            .button("Learn at Playhead")
            .on_hover_text("Learn from the Learn Window starting at the playhead. Park it on noise only: no speech or music.")
            .clicked()
        {
            cmds.push(AppCommand::LearnDenoiserProfile {
                clip_id,
                effect_index: idx,
                at_clip_secs: Some(clip_time_secs.max(0.0)),
            });
        }
        if ui
            .button("Auto Learn")
            .on_hover_text("Find the quietest stretch of this clip and learn the noise from it")
            .clicked()
        {
            cmds.push(AppCommand::LearnDenoiserProfile { clip_id, effect_index: idx, at_clip_secs: None });
        }
        if ui
            .add_enabled(learned, egui::Button::new("Clear"))
            .on_hover_text("Forget the learned profile and go back to adaptive tracking")
            .clicked()
        {
            cmds.push(AppCommand::SetAudioEffectParams {
                clip_id,
                effect_index: idx,
                params: vec![("profile_learned".into(), 0.0)],
            });
        }
    });
}


/// What a Normalize says under its controls (#2778): in Clip mode, that
/// it is measuring the clip (unity until then), or what it measured and
/// the gain it holds; in Ride mode, nothing.
pub(crate) fn normalize_note(inst: &klipp_core::AudioEffectInstance) -> Option<String> {
    let get = |n: &str| inst.params.iter().find(|(k, _)| k == n).map(|(_, v)| *v);
    if get("mode").unwrap_or(1.0) <= 0.5 {
        return None;
    }
    let target = get("target_lufs").unwrap_or(-14.0);
    Some(match get("measured_lufs").filter(|v| v.is_finite()) {
        None => "Measuring the clip's loudness\u{2026} (no gain until then)".to_string(),
        Some(m) if m <= -70.0 => "The clip is silent: no gain".to_string(),
        Some(m) => {
            let gain = 20.0 * klipp_audio::dsp::normalize::Normalize::clip_gain(target, m).log10();
            format!("Clip measured at {m:.1} LUFS: {gain:+.1} dB held over it")
        }
    })
}

#[cfg(test)]
mod helper_tests {
    use super::*;
    use klipp_core::Interpolation;

    #[test]
    fn fractions_read_as_percent_and_percent_params_stay_as_they_are() {
        use klipp_audio::dsp::ParamUnit;
        // The Broadband Denoiser stores Reduction 0..1; full reduction
        // used to read "1.00%".
        let (scale, suffix) = param_display(ParamUnit::Fraction);
        assert_eq!((1.0 * scale, suffix), (100.0, "%"));
        // Chorus mix and friends already store 0..100.
        assert_eq!(param_display(ParamUnit::Percent), (1.0, "%"));
        let denoiser = klipp_audio::dsp::create_effect("denoiser", 48_000).unwrap();
        for name in ["strength", "floor", "wind"] {
            let desc = denoiser.param_descriptors().iter().find(|d| d.name == name).unwrap();
            assert_eq!(desc.unit, ParamUnit::Fraction, "{name} is stored 0..1");
        }
    }

    fn kf(t: f64, v: f32) -> Keyframe {
        Keyframe {
            time_secs: t, value: v,
            interpolation: Interpolation::Linear,
            tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0),
            handles: Default::default(),
        }
    }

    #[test]
    fn has_audio_kfs_reports_correctly() {
        let mut map: HashMap<String, Vec<Keyframe>> = HashMap::new();
        assert!(!has_audio_kfs(&map, 0, "gain"));
        map.insert("0:gain".to_string(), vec![kf(0.0, 1.0)]);
        assert!(has_audio_kfs(&map, 0, "gain"));
        // Different effect index — must not match.
        assert!(!has_audio_kfs(&map, 1, "gain"));
        // Different param name — must not match.
        assert!(!has_audio_kfs(&map, 0, "threshold"));
        // Empty Vec at the right key still counts as "no kfs" — the
        // handler should clean these up but UI must not over-report.
        map.insert("0:threshold".to_string(), vec![]);
        assert!(!has_audio_kfs(&map, 0, "threshold"));
    }

    #[test]
    fn audio_kf_eval_falls_back_to_default_when_no_track() {
        let map: HashMap<String, Vec<Keyframe>> = HashMap::new();
        assert_eq!(audio_kf_eval(&map, 0, "gain", 0.5, 0.7, 0.0), 0.7);
    }

    #[test]
    fn audio_kf_eval_uses_track_value_when_present() {
        let mut map: HashMap<String, Vec<Keyframe>> = HashMap::new();
        map.insert("0:gain".to_string(), vec![kf(0.0, 0.0), kf(1.0, 1.0)]);
        // Linear ramp 0 → 1 over [0, 1]; midpoint = 0.5.
        let v = audio_kf_eval(&map, 0, "gain", 0.5, 99.0, 0.0);
        assert!((v - 0.5).abs() < 1e-3, "expected ~0.5, got {v}");
    }

    #[test]
    fn audio_kf_state_at_playhead_detects_within_tolerance() {
        let keyframe_tolerance = 0.5 / 24.0;
        let mut map: HashMap<String, Vec<Keyframe>> = HashMap::new();
        map.insert("0:gain".to_string(), vec![kf(0.5, 0.5)]);
        // Timestamp conversion roundoff still identifies the current key.
        let s = audio_kf_state(&map, 0, "gain", 0.5 + 1e-8, keyframe_tolerance);
        assert!(s.has_keyframe_at_playhead);
        assert!(s.has_any_keyframes);
        // Outside tolerance — must not register
        let s = audio_kf_state(&map, 0, "gain", 0.6, keyframe_tolerance);
        assert!(!s.has_keyframe_at_playhead);
        assert!(s.has_any_keyframes);
    }

    #[test]
    fn audio_kf_state_keeps_adjacent_frames_distinct() {
        for fps in [60.0, 120.0, 60000.0 / 1001.0] {
            let dt = 1.0 / fps;
            let keyframe_tolerance = 0.5 / fps;
            let map = HashMap::from([("0:gain".to_string(), vec![
                kf(0.0, 0.0), kf(dt, 1.0), kf(2.0 * dt, 2.0),
            ])]);
            let state = audio_kf_state(&map, 0, "gain", dt, keyframe_tolerance);
            assert!(state.has_keyframe_at_playhead);
            assert_eq!(state.prev_keyframe_time, Some(0.0));
            assert_eq!(state.next_keyframe_time, Some(2.0 * dt));
            assert!(!audio_kf_state(&map, 0, "gain", 3.0 * dt, keyframe_tolerance).has_keyframe_at_playhead);
        }
    }

    #[test]
    fn audio_kf_state_finds_prev_and_next() {
        let keyframe_tolerance = 0.5 / 24.0;
        let mut map: HashMap<String, Vec<Keyframe>> = HashMap::new();
        map.insert("0:gain".to_string(), vec![kf(0.0, 0.0), kf(1.0, 1.0), kf(2.0, 2.0)]);
        let s = audio_kf_state(&map, 0, "gain", 1.5, keyframe_tolerance);
        assert_eq!(s.prev_keyframe_time, Some(1.0));
        assert_eq!(s.next_keyframe_time, Some(2.0));
        // At the very start of the track, prev is None.
        let s = audio_kf_state(&map, 0, "gain", 0.0, keyframe_tolerance);
        assert_eq!(s.prev_keyframe_time, None);
        assert_eq!(s.next_keyframe_time, Some(1.0));
    }

    #[test]
    fn audio_kf_state_reports_no_kfs_for_unknown_param() {
        let keyframe_tolerance = 0.5 / 24.0;
        let map: HashMap<String, Vec<Keyframe>> = HashMap::new();
        let s = audio_kf_state(&map, 0, "gain", 0.0, keyframe_tolerance);
        assert!(!s.has_any_keyframes);
        assert!(!s.has_keyframe_at_playhead);
        assert!(s.prev_keyframe_time.is_none());
        assert!(s.next_keyframe_time.is_none());
    }

    #[test]
    fn audio_kf_eval_works_for_reserved_keys() {
        // The reserved __bypass and __wetdry tracks use the same
        // namespace as regular params; eval must work transparently.
        let mut map: HashMap<String, Vec<Keyframe>> = HashMap::new();
        map.insert("0:__bypass".to_string(), vec![
            Keyframe { time_secs: 0.0, value: 0.0, interpolation: Interpolation::Step,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() },
            Keyframe { time_secs: 1.0, value: 1.0, interpolation: Interpolation::Step,
                tangent_in: (0.0, 0.0), tangent_out: (0.0, 0.0), handles: Default::default() },
        ]);
        // Step interp before second kf: still 0.0
        assert_eq!(audio_kf_eval(&map, 0, "__bypass", 0.5, 1.0, 0.0), 0.0);
        // After second kf: 1.0
        assert_eq!(audio_kf_eval(&map, 0, "__bypass", 1.5, 0.0, 0.0), 1.0);
    }
}
