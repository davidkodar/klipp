use crate::browser::effect_registry::{EffectEntry, EffectSource};
use crate::drag_drop::DragPayload;
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// Stable per-effect tile colour. Returns `theme.accent` for LADSPA so the
/// whole class is visually identifiable even when the catalogue grows. For
/// built-ins the colour is keyed off the underlying type_id so renaming a
/// display label doesn't reshuffle the visual layout.
fn tile_color(entry: &EffectEntry, theme: &KlippTheme) -> egui::Color32 {
    match &entry.source {
        EffectSource::Audio(type_id) => match type_id.as_str() {
            "eq" | "pitch_shift" => egui::Color32::from_rgb(0x2d, 0x6f, 0xa8), // blue
            "compressor" | "limiter" | "gate" | "deesser" | "normalize"
            | "multiband_compressor" | "noise_reduction" => {
                egui::Color32::from_rgb(0x3a, 0x8c, 0x6e) // green
            }
            "reverb" | "delay" => egui::Color32::from_rgb(0x4a, 0x6e, 0xa8), // light-blue
            "chorus" => egui::Color32::from_rgb(0x4a, 0x6e, 0xa8),
            "declick" => egui::Color32::from_rgb(0x7a, 0x4f, 0xa8), // purple
            _ => egui::Color32::from_rgb(0x50, 0x50, 0x60),
        },
        EffectSource::Ladspa(_) => theme.accent,
        _ => egui::Color32::from_rgb(0x50, 0x50, 0x60),
    }
}

/// Map an entry's source to the `AddAudioEffect` `effect_type` string.
/// Built-ins use the bare type_id (e.g. `"compressor"`); LADSPA plugins use
/// the `"ladspa:{path}:{label}"` composite parsed by `dsp::create_effect`.
fn effect_type_for_dispatch(entry: &EffectEntry) -> Option<String> {
    match &entry.source {
        EffectSource::Audio(type_id) => Some(type_id.clone()),
        EffectSource::Ladspa(plugin) => Some(format!(
            "ladspa:{}:{}",
            plugin.path.to_string_lossy(),
            plugin.label
        )),
        _ => None,
    }
}

/// One-line description shown under the effect name. Built-ins keep the
/// hand-curated descriptions from the previous hardcoded list (mapped here
/// by type_id so they survive registry refactors). LADSPA shows
/// "by {maker}" for provenance.
fn description_for(entry: &EffectEntry) -> String {
    match &entry.source {
        EffectSource::Audio(type_id) => match type_id.as_str() {
            "compressor" => "Dynamic range control".into(),
            "eq" => "6-band parametric equaliser".into(),
            "limiter" => "Peak protection".into(),
            "gate" => "Silence low-level noise".into(),
            "deesser" => "Reduce sibilance".into(),
            "normalize" => "Set target loudness (LUFS)".into(),
            "noise_reduction" => "Multi-band gate".into(),
            "declick" => "Remove pops & clicks".into(),
            "reverb" => "Room & space simulation".into(),
            "delay" => "Time-based repeat".into(),
            "chorus" => "Modulation & width".into(),
            "pitch_shift" => "Transpose audio pitch".into(),
            "multiband_compressor" => "Per-band compression".into(),
            _ => entry.grouping.clone(),
        },
        EffectSource::Ladspa(plugin) => {
            if plugin.maker.is_empty() {
                "LADSPA plugin".into()
            } else {
                format!("by {}", plugin.maker)
            }
        }
        _ => entry.description.clone(),
    }
}

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();

    // Live preview waveform (decorative)
    egui::Frame::none()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0_f32, theme.border))
        .rounding(6.0)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .outer_margin(egui::Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new("LIVE PREVIEW")
                    .size(10.0)
                    .strong()
                    .color(theme.text_dim),
            );
            ui.add_space(4.0);
            // Draw decorative waveform bars
            let (bar_rect, _) =
                ui.allocate_exact_size(egui::vec2(ui.available_width(), 36.0), egui::Sense::hover());
            let bar_count = (bar_rect.width() / 3.0) as usize;
            let painter = ui.painter();
            for i in 0..bar_count {
                let x = bar_rect.min.x + i as f32 * 3.0;
                let h = ((i * 37 + 13) % 30 + 4) as f32;
                let y_center = bar_rect.center().y;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(x, y_center - h / 2.0),
                        egui::vec2(2.0, h),
                    ),
                    1.0,
                    egui::Color32::from_rgb(0x3a, 0x8c, 0x6e),
                );
            }
        });

    // Build the visible list from the unified effect registry. Both
    // built-in audio effects (EffectSource::Audio) and discovered LADSPA
    // plugins (EffectSource::Ladspa) appear here; this view used to be a
    // hardcoded array that bypassed the registry entirely, which meant
    // discovered LADSPA plugins were invisible to the user.
    let search_lower = search_query.to_lowercase();
    let mut visible: Vec<&EffectEntry> = state
        .effect_registry
        .effects
        .iter()
        .filter(|e| matches!(e.source, EffectSource::Audio(_) | EffectSource::Ladspa(_)))
        .filter(|e| {
            if search_lower.is_empty() {
                return true;
            }
            if e.name.to_lowercase().contains(&search_lower) {
                return true;
            }
            if let EffectSource::Ladspa(p) = &e.source {
                if p.maker.to_lowercase().contains(&search_lower)
                    || p.label.to_lowercase().contains(&search_lower)
                {
                    return true;
                }
            }
            false
        })
        .collect();

    // Sort: built-ins first (registry order preserved), then LADSPA
    // grouped by maker. Within each group preserve registry order so the
    // layout is stable across launches.
    visible.sort_by_key(|e| match &e.source {
        EffectSource::Audio(_) => (0, String::new()),
        EffectSource::Ladspa(p) => (1, p.maker.to_lowercase()),
        _ => (2, String::new()),
    });

    let total_audio = state
        .effect_registry
        .effects
        .iter()
        .filter(|e| matches!(e.source, EffectSource::Audio(_) | EffectSource::Ladspa(_)))
        .count();
    let ladspa_count = state
        .effect_registry
        .effects
        .iter()
        .filter(|e| matches!(e.source, EffectSource::Ladspa(_)))
        .count();
    let builtin_count = total_audio.saturating_sub(ladspa_count);

    // Header line giving honest counts.
    egui::Frame::none()
        .inner_margin(egui::Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{builtin_count} built-in   |   {ladspa_count} LADSPA"
                ))
                .size(9.0)
                .color(theme.text_dim),
            );
        });

    egui::ScrollArea::vertical()
        .id_salt("browser_audio_scroll")
        .show(ui, |ui| {
            ui.add_space(4.0);

            if visible.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    if total_audio == 0 {
                        ui.label(
                            egui::RichText::new("No audio effects available")
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                    } else if !search_lower.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "No effects match \"{search_query}\""
                            ))
                            .size(11.0)
                            .color(theme.text_dim),
                        );
                    }
                });
                return;
            }

            let mut last_group: Option<String> = None;
            for entry in &visible {
                // Maker-bucket headers in the LADSPA section so CALF, SWH,
                // LSP, etc. read as separate groups. Built-ins all sit
                // under one implicit group (the "Built-in" type and the
                // sort order make it visually obvious).
                if let EffectSource::Ladspa(p) = &entry.source {
                    let group = if p.maker.is_empty() {
                        "Other".to_string()
                    } else {
                        p.maker.clone()
                    };
                    if last_group.as_ref() != Some(&group) {
                        ui.add_space(6.0);
                        egui::Frame::none()
                            .inner_margin(egui::Margin::symmetric(10.0, 4.0))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(group.to_uppercase())
                                        .size(9.0)
                                        .strong()
                                        .color(theme.text_dim),
                                );
                            });
                        last_group = Some(group);
                    }
                } else {
                    last_group = None;
                }

                let color = tile_color(entry, theme);
                let inner = egui::Frame::none()
                    .fill(theme.bg_raised)
                    .stroke(egui::Stroke::new(1.0_f32, theme.border))
                    .rounding(6.0)
                    .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                    .outer_margin(egui::Margin::symmetric(8.0, 2.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let icon_bg = egui::Color32::from_rgba_premultiplied(
                                color.r(),
                                color.g(),
                                color.b(),
                                33,
                            );
                            let icon_border = egui::Color32::from_rgba_premultiplied(
                                color.r(),
                                color.g(),
                                color.b(),
                                69,
                            );
                            let (icon_rect, _) = ui.allocate_exact_size(
                                egui::vec2(32.0, 32.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(icon_rect, 6.0, icon_bg);
                            ui.painter().rect_stroke(
                                icon_rect,
                                6.0,
                                egui::Stroke::new(1.0_f32, icon_border),
                            );
                            let wf_rect = egui::Rect::from_center_size(
                                icon_rect.center(),
                                egui::vec2(14.0, 14.0),
                            );
                            Icon::Waveform.image(14.0).tint(color).paint_at(ui, wf_rect);

                            ui.add_space(6.0);

                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(&entry.name)
                                            .size(11.0)
                                            .strong()
                                            .color(theme.text_primary),
                                    );
                                    if matches!(entry.source, EffectSource::Ladspa(_)) {
                                        ui.add_space(4.0);
                                        ui.label(
                                            egui::RichText::new("LADSPA")
                                                .size(8.0)
                                                .strong()
                                                .color(theme.accent),
                                        );
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(description_for(entry))
                                        .size(10.0)
                                        .color(theme.text_dim),
                                );
                            });
                        });
                    });

                let tile_rect = inner.response.rect;
                let interact_id = ui.make_persistent_id(("audio_fx_tile", &entry.name));
                let resp = ui.interact(tile_rect, interact_id, egui::Sense::click_and_drag());

                if resp.double_clicked() {
                    if let Some(type_id) = effect_type_for_dispatch(entry) {
                        if let Some(clip_id) = state.selected_clip {
                            cmds.push(AppCommand::AddAudioEffect {
                                clip_id,
                                effect_type: type_id,
                            });
                            state
                                .toast
                                .show(format!("\"{}\" added to clip", entry.name));
                        } else {
                            state.toast.show("Select a clip first".to_string());
                        }
                    }
                }

                if resp.drag_started() {
                    if let Some(type_id) = effect_type_for_dispatch(entry) {
                        state.drag_drop.begin(DragPayload::AudioEffect {
                            name: entry.name.clone(),
                            type_id,
                        });
                    }
                }
            }
        });

    cmds
}
