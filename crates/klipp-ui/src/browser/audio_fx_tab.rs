use crate::widgets::menu::MenuRows;
use crate::browser::effect_registry::{EffectEntry, EffectSource};
use crate::browser::effects_tab::{EffectCategory, EffectSort};
use crate::drag_drop::DragPayload;
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

/// Source filter for the Audio FX tab. Mirrors `effects_tab::SourceFilter`
/// but with an `Ladspa` segment in place of `Ofx` — the audio side has no
/// OpenFX plugins, only built-ins and LADSPA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum AudioSourceFilter {
    #[default]
    All,
    Builtin,
    Ladspa,
    Lv2,
    Clap,
}


/// One entry in the audition source dropdown — derived from the
/// timeline's current selection (one entry per selected clip with a
/// resolvable media filename). The caller (`browser/mod.rs`) builds
/// this list because it has the AppState handle needed to resolve
/// `ClipId → Sequence::find → MediaPool::get → filename`.
#[derive(Debug, Clone)]
pub struct AuditionSourceOption {
    pub clip_id: klipp_core::ClipId,
    pub media_id: klipp_core::MediaId,
    pub label: String,
}

const CATEGORY_SIDEBAR_W: f32 = 180.0;

/// Public colour mapping for audio effect type IDs — keyed off the same
/// type_id strings used throughout the audio chain. The tile in the
/// FX library, the timeline bypass strips, and any future automation
/// indicators all share this palette so a "Reverb" looks the same
/// everywhere. LADSPA plugins fall back to `theme.accent` (single hue
/// for the whole class — distinguishing 100+ third-party plugins by
/// colour would just produce noise).
pub fn audio_effect_color_for_type(type_id: &str, theme: &KlippTheme) -> egui::Color32 {
    if type_id.starts_with("ladspa:") {
        return theme.accent;
    }
    match type_id {
        "eq" => egui::Color32::from_rgb(0x2d, 0x6f, 0xa8), // blue
        "pitch_shift" => egui::Color32::from_rgb(0xc0, 0x5e, 0x9e), // pink
        "compressor" | "limiter" | "gate" | "multiband_compressor" => {
            egui::Color32::from_rgb(0x3a, 0x8c, 0x6e) // green
        }
        "deesser" | "noise_reduction" | "declick" => {
            egui::Color32::from_rgb(0xa9, 0xa3, 0x4e) // olive
        }
        "normalize" => egui::Color32::from_rgb(0x70, 0x80, 0x88), // grey
        "reverb" => egui::Color32::from_rgb(0x4a, 0x6e, 0xa8),
        "delay" => egui::Color32::from_rgb(0x6a, 0x80, 0xc0),
        "chorus" => egui::Color32::from_rgb(0x9a, 0x6e, 0xc8),
        _ => egui::Color32::from_rgb(0x50, 0x50, 0x60),
    }
}

/// Stable per-effect tile colour. Both built-in and LADSPA effects key
/// off the audio sub-category so a Reverb plugin reads the same in the
/// row icon, the timeline strip, and the badge — regardless of where
/// the effect came from. Earlier this returned `theme.accent` for
/// LADSPA, which collided badly with the amber accent on themes where
/// `accent` is yellow (amber-on-amber, unreadable per the
/// amber-text-contrast feedback rule).
fn tile_color(entry: &EffectEntry, theme: &KlippTheme) -> egui::Color32 {
    match &entry.source {
        EffectSource::Audio(type_id) => audio_effect_color_for_type(type_id, theme),
        EffectSource::Ladspa(_) | EffectSource::Lv2(_) | EffectSource::Clap(_) => {
            audio_category_color(entry.category, theme)
        }
        _ => egui::Color32::from_rgb(0x50, 0x50, 0x60),
    }
}

/// Category-driven colour for audio effects. Mirrors the per-type
/// palette in `audio_effect_color_for_type` but keyed on the parsed
/// category so LADSPA plugins (which carry no type_id we recognise)
/// still get a meaningful hue.
fn audio_category_color(cat: EffectCategory, theme: &KlippTheme) -> egui::Color32 {
    match cat {
        EffectCategory::AudioEQ => egui::Color32::from_rgb(0x2d, 0x6f, 0xa8),
        EffectCategory::AudioDynamics => egui::Color32::from_rgb(0x3a, 0x8c, 0x6e),
        EffectCategory::AudioReverb => egui::Color32::from_rgb(0x4a, 0x6e, 0xa8),
        EffectCategory::AudioDelay => egui::Color32::from_rgb(0x6a, 0x80, 0xc0),
        EffectCategory::AudioModulation => egui::Color32::from_rgb(0x9a, 0x6e, 0xc8),
        EffectCategory::AudioPitch => egui::Color32::from_rgb(0xc0, 0x5e, 0x9e),
        EffectCategory::AudioRestoration => egui::Color32::from_rgb(0xa9, 0xa3, 0x4e),
        EffectCategory::AudioUtility => egui::Color32::from_rgb(0x70, 0x80, 0x88),
        _ => {
            // Unknown / non-audio category — fall back to dim grey so
            // the row still reads. Should not normally happen since
            // every audio entry routes through one of the above.
            theme.text_dim
        }
    }
}

/// Map an entry's source to the `AddAudioEffect` `effect_type` string.
fn effect_type_for_dispatch(entry: &EffectEntry) -> Option<String> {
    match &entry.source {
        EffectSource::Audio(type_id) => Some(type_id.clone()),
        EffectSource::Ladspa(plugin) => Some(format!(
            "ladspa:{}:{}",
            plugin.path.to_string_lossy(),
            plugin.label
        )),
        EffectSource::Lv2(plugin) => Some(plugin.type_id()),
        EffectSource::Clap(plugin) => Some(plugin.type_id()),
        _ => None,
    }
}

/// One-line description shown under the effect name. Built-ins keep a
/// hand-curated description; LADSPA shows "by {maker}" for provenance.
fn description_for(entry: &EffectEntry) -> String {
    match &entry.source {
        EffectSource::Audio(type_id) => match type_id.as_str() {
            "compressor" => "Dynamic range control".into(),
            "eq" => "6-band parametric equaliser".into(),
            "limiter" => "Peak protection".into(),
            "gate" => "Silence low-level noise".into(),
            "high_pass" => "Remove low-frequency rumble and wind".into(),
            "denoiser" => "Learn and reduce steady background noise".into(),
            "voice_isolation" => "Separate speech from wind, traffic and room noise (AI)".into(),
            "deesser" => "Reduce sibilance in vocals".into(),
            "normalize" => "Set target loudness (LUFS)".into(),
            "noise_reduction" => "Per-band noise gating".into(),
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

/// Sub-category icon for the sidebar rows. Falls back to `Waveform`
/// where the bundled Phosphor set has no exact match — the column
/// reads cleaner with every row aligned.
fn sidebar_icon_for(cat: EffectCategory) -> Icon {
    match cat {
        EffectCategory::AudioAll => Icon::List,
        EffectCategory::AudioEQ => Icon::Equalizer,
        EffectCategory::AudioDynamics => Icon::Waveform,
        EffectCategory::AudioReverb => Icon::SpeakerHigh,
        EffectCategory::AudioDelay => Icon::ArrowCounterClockwise,
        EffectCategory::AudioModulation => Icon::Waveform,
        EffectCategory::AudioPitch => Icon::MusicNote,
        EffectCategory::AudioRestoration => Icon::ArrowCounterClockwise,
        EffectCategory::AudioUtility => Icon::Gear,
        _ => Icon::Waveform,
    }
}

/// Display label for an audio sub-category sidebar row.
fn sidebar_label_for(cat: EffectCategory) -> &'static str {
    match cat {
        EffectCategory::AudioAll => "All",
        EffectCategory::AudioEQ => "EQ",
        EffectCategory::AudioDynamics => "Dynamics",
        EffectCategory::AudioReverb => "Reverb",
        EffectCategory::AudioDelay => "Delay",
        EffectCategory::AudioModulation => "Modulation",
        EffectCategory::AudioPitch => "Pitch",
        EffectCategory::AudioRestoration => "Restoration",
        EffectCategory::AudioUtility => "Utility",
        _ => "",
    }
}

const AUDIO_SUB_CATEGORIES: [EffectCategory; 9] = [
    EffectCategory::AudioAll,
    EffectCategory::AudioEQ,
    EffectCategory::AudioDynamics,
    EffectCategory::AudioReverb,
    EffectCategory::AudioDelay,
    EffectCategory::AudioModulation,
    EffectCategory::AudioPitch,
    EffectCategory::AudioRestoration,
    EffectCategory::AudioUtility,
];

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &mut String,
    active_category: &mut EffectCategory,
    source_filter: &mut AudioSourceFilter,
    sort_mode: &mut EffectSort,
    audition_playing: &mut bool,
    audition_source: &mut Option<klipp_core::ClipId>,
    source_options: &[AuditionSourceOption],
    audition_path: Option<&std::path::Path>,
    audition_media_id: Option<klipp_core::MediaId>,
    audition_peaks: Option<&[(f32, f32)]>,
    audition_failed: bool,
    audition_duration_secs: Option<f64>,
    audition_volume: &mut f32,
    audition_anchor_secs: &mut f64,
    audition_anchor_at: &mut Option<std::time::Instant>,
    selected: &mut Option<String>,
    auditioning: &mut Option<String>,
    hide_unsupported: &mut bool,
    favorite_effects: &mut Vec<String>,
    recent_effects: &[String],
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let avail = ui.available_rect_before_wrap();

    // Reset to AudioAll if the saved category drifted out of the audio
    // bucket (e.g. via legacy state).
    if !AUDIO_SUB_CATEGORIES.contains(active_category)
        && !matches!(
            *active_category,
            EffectCategory::Favourites | EffectCategory::Recent | EffectCategory::Installed
        )
    {
        *active_category = EffectCategory::AudioAll;
    }

    // ── Split: category sidebar (left) | content (right) ──
    let sidebar_w = CATEGORY_SIDEBAR_W.min(avail.width() * 0.40);
    let sidebar_rect = egui::Rect::from_min_size(
        avail.min,
        egui::vec2(sidebar_w, avail.height()),
    );
    let content_rect = egui::Rect::from_min_max(
        egui::pos2(avail.min.x + sidebar_w, avail.min.y),
        avail.max,
    );

    // ── Sidebar ─────────────────────────────────────────────────────
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
        ui.set_min_size(sidebar_rect.size());
        ui.painter().rect_filled(sidebar_rect, 0.0, theme.bg_panel);
        ui.painter().vline(
            sidebar_rect.max.x,
            sidebar_rect.y_range(),
            egui::Stroke::new(1.0_f32, theme.border),
        );

        egui::ScrollArea::vertical()
            .id_salt("audio_fx_sidebar_scroll")
            .show(ui, |ui| {
                ui.add_space(6.0);

                // ── General section: Favorites / Recent / Installed ──
                let installed_total = state
                    .effect_registry
                    .effects
                    .iter()
                    .filter(|e| {
                        matches!(
                            e.source,
                            EffectSource::Audio(_) | EffectSource::Ladspa(_) | EffectSource::Lv2(_) | EffectSource::Clap(_)
                        )
                    })
                    .count();
                let fav_audio_count = favorite_effects
                    .iter()
                    .filter(|n| {
                        state.effect_registry.effects.iter().any(|e| {
                            &e.name == *n
                                && matches!(
                                    e.source,
                                    EffectSource::Audio(_) | EffectSource::Ladspa(_) | EffectSource::Lv2(_) | EffectSource::Clap(_)
                                )
                        })
                    })
                    .count();
                let recent_audio_count = recent_effects
                    .iter()
                    .filter(|n| {
                        state.effect_registry.effects.iter().any(|e| {
                            &e.name == *n
                                && matches!(
                                    e.source,
                                    EffectSource::Audio(_) | EffectSource::Ladspa(_) | EffectSource::Lv2(_) | EffectSource::Clap(_)
                                )
                        })
                    })
                    .count();

                sidebar_row(
                    ui,
                    theme,
                    Some(Icon::Star),
                    "Favorites",
                    fav_audio_count,
                    *active_category == EffectCategory::Favourites,
                    "audio_general_favourites",
                    &mut |c| *active_category = c,
                    EffectCategory::Favourites,
                );
                sidebar_row(
                    ui,
                    theme,
                    Some(Icon::ArrowCounterClockwise),
                    "Recent",
                    recent_audio_count,
                    *active_category == EffectCategory::Recent,
                    "audio_general_recent",
                    &mut |c| *active_category = c,
                    EffectCategory::Recent,
                );
                sidebar_row(
                    ui,
                    theme,
                    Some(Icon::UploadSimple),
                    "Installed",
                    installed_total,
                    *active_category == EffectCategory::Installed,
                    "audio_general_installed",
                    &mut |c| *active_category = c,
                    EffectCategory::Installed,
                );

                ui.add_space(8.0);
                sidebar_section_separator(ui, theme);

                // ── AUDIO FX section ──
                sidebar_section_header(ui, theme, Some(Icon::SpeakerHigh), "Audio FX");

                for cat in AUDIO_SUB_CATEGORIES {
                    let count = audio_count_in_registry(state, cat);
                    sidebar_row(
                        ui,
                        theme,
                        Some(sidebar_icon_for(cat)),
                        sidebar_label_for(cat),
                        count,
                        *active_category == cat,
                        &format!("audio_cat_{:?}", cat),
                        &mut |c| *active_category = c,
                        cat,
                    );
                }
                ui.add_space(8.0);
            });
    });

    // ── Right pane ──────────────────────────────────────────────────
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content_rect), |ui| {
        ui.set_min_size(content_rect.size());

        ui.add_space(8.0);

        // ── Audition transport ──
        let audition_cmds = audition_player(
            ui,
            theme,
            audition_playing,
            audition_source,
            source_options,
            audition_path,
            audition_media_id,
            audition_peaks,
            audition_failed,
            audition_duration_secs,
            audition_volume,
            audition_anchor_secs,
            audition_anchor_at,
        );
        cmds.extend(audition_cmds);

        ui.add_space(8.0);

        // ── Search bar ──
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                super::browser_search_field(ui, theme, search_query, "Search audio effects\u{2026}", state);
            });
        ui.add_space(6.0);

        // ── Source filter row + eye toggle ──
        let query_snapshot = search_query.clone();
        let scoped: Vec<&EffectEntry> = state
            .effect_registry
            .filtered_full(*active_category, &query_snapshot, favorite_effects, recent_effects)
            .into_iter()
            .filter(|e| {
                matches!(
                    e.source,
                    EffectSource::Audio(_) | EffectSource::Ladspa(_) | EffectSource::Lv2(_) | EffectSource::Clap(_)
                )
            })
            .collect();
        let all_count = scoped.len();
        let builtin_count = scoped
            .iter()
            .filter(|e| matches!(e.source, EffectSource::Audio(_)))
            .count();
        let ladspa_count = scoped
            .iter()
            .filter(|e| matches!(e.source, EffectSource::Ladspa(_)))
            .count();
        let lv2_count = scoped
            .iter()
            .filter(|e| matches!(e.source, EffectSource::Lv2(_)))
            .count();
        let clap_count = scoped
            .iter()
            .filter(|e| matches!(e.source, EffectSource::Clap(_)))
            .count();

        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    segmented_audio_source_filter(
                        ui,
                        theme,
                        source_filter,
                        all_count,
                        builtin_count,
                        ladspa_count,
                        lv2_count,
                        clap_count,
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        eye_toggle_button(ui, theme, hide_unsupported);
                    });
                });
            });

        ui.add_space(8.0);

        // ── Apply source filter to scope ──
        let mut filtered: Vec<EffectEntry> = scoped
            .into_iter()
            .filter(|e| match *source_filter {
                AudioSourceFilter::All => true,
                AudioSourceFilter::Builtin => matches!(e.source, EffectSource::Audio(_)),
                AudioSourceFilter::Ladspa => matches!(e.source, EffectSource::Ladspa(_)),
                AudioSourceFilter::Lv2 => matches!(e.source, EffectSource::Lv2(_)),
                AudioSourceFilter::Clap => matches!(e.source, EffectSource::Clap(_)),
            })
            .cloned()
            .collect();

        // Sort
        match *sort_mode {
            EffectSort::NameAsc => {
                filtered.sort_by_key(|a| a.name.to_lowercase());
            }
            EffectSort::NameDesc => {
                filtered.sort_by_key(|a| std::cmp::Reverse(a.name.to_lowercase()));
            }
            EffectSort::Category => {
                filtered.sort_by(|a, b| {
                    a.grouping
                        .to_lowercase()
                        .cmp(&b.grouping.to_lowercase())
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
            EffectSort::Source => {
                filtered.sort_by(|a, b| {
                    a.source_label
                        .cmp(b.source_label)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
        }

        // ── Result strip: count + sort dropdown ──
        result_strip(ui, theme, filtered.len(), *active_category, sort_mode);
        ui.add_space(4.0);

        // ── List ──
        egui::ScrollArea::vertical()
            .id_salt("audio_fx_list_scroll")
            .show(ui, |ui| {
                if filtered.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        let msg = if !search_query.is_empty() {
                            format!("No effects match \"{}\"", search_query)
                        } else {
                            "No audio effects in this category".to_string()
                        };
                        ui.label(
                            egui::RichText::new(msg)
                                .size(11.0)
                                .color(theme.text_dim),
                        );
                    });
                    return;
                }

                for entry in &filtered {
                    let is_selected = selected.as_deref() == Some(entry.name.as_str());
                    let is_playing = auditioning.as_deref() == Some(entry.name.as_str());
                    let resp = audio_fx_row(ui, theme, entry, is_selected);
                    let row_id = ui.make_persistent_id(("audio_fx_row", &entry.name));
                    let click_resp = ui.interact(
                        resp.rect,
                        row_id,
                        egui::Sense::click_and_drag(),
                    );

                    // Per-row play button — independent from selection.
                    // Clicking the row body just selects (highlights) the
                    // effect; clicking the play button is the only thing
                    // that toggles audition playback.
                    let play_rect = play_button_rect(resp.rect);
                    let play_id = row_id.with("play");
                    let play_resp = ui.interact(play_rect, play_id, egui::Sense::click());
                    paint_row_play_button(
                        ui,
                        theme,
                        play_rect,
                        play_resp.hovered() || click_resp.hovered(),
                        is_playing,
                        theme.accent,
                    );
                    let play_clicked = play_resp.clicked();
                    if play_clicked {
                        if is_playing {
                            *auditioning = None;
                        } else {
                            // Starting audition on a different effect
                            // implicitly stops the previous one (single
                            // audition slot for now). It does NOT change
                            // the selection — that stays under user
                            // control via row clicks.
                            *auditioning = Some(entry.name.clone());
                        }
                    }
                    let _ = play_resp.on_hover_text(if is_playing {
                        "Stop audition"
                    } else {
                        "Audition (coming soon)"
                    });

                    if click_resp.clicked() && !play_clicked {
                        // Body click → select only, never start playback.
                        *selected = Some(entry.name.clone());
                    }

                    if click_resp.double_clicked() {
                        if let Some(type_id) = effect_type_for_dispatch(entry) {
                            // Every selected clip that can take it, as
                            // one undo step, as the VFX tab does (#842):
                            // this went onto the one clip last clicked.
                            let targets = crate::effect_targets::targets(
                                state.sequence(),
                                state.selected_clip,
                                &state.selected_clips,
                                None,
                                crate::effect_targets::EffectKind::Sound,
                            );
                            if targets.clips.is_empty() {
                                state.toast.show("Select a clip first".to_string());
                            } else {
                                cmds.extend(crate::effect_targets::commands(&targets, |clip_id| {
                                    vec![AppCommand::AddAudioEffect { clip_id, effect_type: type_id.clone() }]
                                }));
                                state.toast.show(crate::effect_targets::said(&entry.name, &targets));
                            }
                        }
                    }

                    if click_resp.drag_started() {
                        if let Some(type_id) = effect_type_for_dispatch(entry) {
                            state.drag_drop.begin(DragPayload::AudioEffect {
                                name: entry.name.clone(),
                                type_id,
                            });
                        }
                    }

                    // Right-click: favourite toggle (matches video FX
                    // tab's affordance).
                    click_resp.context_menu_rows(|ui| {
                        let is_fav = favorite_effects.iter().any(|n| n == &entry.name);
                        let label = if is_fav {
                            "Remove from Favorites"
                        } else {
                            "Add to Favorites"
                        };
                        if ui.button(label).clicked() {
                            if let Some(idx) =
                                favorite_effects.iter().position(|n| n == &entry.name)
                            {
                                favorite_effects.remove(idx);
                            } else {
                                favorite_effects.push(entry.name.clone());
                            }
                            ui.close_menu();
                        }
                    });
                }
            });
    });

    cmds
}

/// Total audio effects (built-in + LADSPA) in a given audio sub-category.
fn audio_count_in_registry(state: &AppState, cat: EffectCategory) -> usize {
    state
        .effect_registry
        .effects
        .iter()
        .filter(|e| {
            let is_audio = matches!(
                e.source,
                EffectSource::Audio(_) | EffectSource::Ladspa(_) | EffectSource::Lv2(_) | EffectSource::Clap(_)
            );
            if !is_audio {
                return false;
            }
            match cat {
                EffectCategory::AudioAll => true,
                _ => e.category == cat,
            }
        })
        .count()
}

// ─────────────────────────── Audition transport ───────────────────────────

/// Decorative audition transport at the top of the Audio FX tab. Big
/// orange play button on the left, a generated waveform fill across the
/// middle, time readout on the right, and a meter strip below.
///
/// Static visuals for now — clicking play toggles the icon but there's
/// no audio engine wired up. Source dropdown is a label only; the
/// real menu lands when per-clip and reference-loop sources are
/// connected.
#[allow(clippy::too_many_arguments)]
fn audition_player(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    playing: &mut bool,
    audition_source: &mut Option<klipp_core::ClipId>,
    source_options: &[AuditionSourceOption],
    audition_path: Option<&std::path::Path>,
    audition_media_id: Option<klipp_core::MediaId>,
    audition_peaks: Option<&[(f32, f32)]>,
    audition_failed: bool,
    audition_duration_secs: Option<f64>,
    audition_volume: &mut f32,
    audition_anchor_secs: &mut f64,
    audition_anchor_at: &mut Option<std::time::Instant>,
) -> Vec<AppCommand> {
    let mut cmds: Vec<AppCommand> = Vec::new();
    // Auto-stop when the picked source changes underneath us — keeps
    // the play state honest if the user changes the timeline selection
    // mid-audition.
    if *playing && audition_path.is_none() {
        cmds.push(AppCommand::AuditionEnd);
        *playing = false;
        *audition_anchor_at = None;
    }
    // Resolve the live position (anchor + elapsed-since-anchor). The
    // skim engine carries its own internal cursor; we mirror it on the
    // UI thread with wall clock to avoid round-tripping every frame.
    let duration_secs = audition_duration_secs.unwrap_or(0.0);
    let position_secs: f64 = if *playing {
        let elapsed = audition_anchor_at
            .map(|at| at.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        (*audition_anchor_secs + elapsed).clamp(0.0, duration_secs.max(0.0))
    } else {
        audition_anchor_secs.clamp(0.0, duration_secs.max(0.0))
    };
    // Clamp at end-of-file: the skim feeder loops the buffer, but for
    // visual purposes we stop at the duration so the playhead doesn't
    // walk off the end. Stopping playback when we hit it would feel
    // more correct, but the engine doesn't surface "ended" yet.
    if *playing && duration_secs > 0.0 && position_secs >= duration_secs {
        cmds.push(AppCommand::AuditionEnd);
        *playing = false;
        *audition_anchor_at = None;
        *audition_anchor_secs = duration_secs;
    }
    egui::Frame::none()
        .fill(theme.bg_raised)
        .stroke(egui::Stroke::new(1.0_f32, theme.border))
        .rounding(4.0)
        .inner_margin(egui::Margin::symmetric(12.0, 10.0))
        .outer_margin(egui::Margin::symmetric(8.0, 0.0))
        .show(ui, |ui| {
            // Header row: AUDITION label + Source selector.
            //
            // The "Source:" prefix lives on the left of the source
            // pill on wide panels and gets dropped on narrow ones —
            // otherwise it competes with the pill's label and ends up
            // overlapping it (the previous right-to-left layout
            // collided when the pill rendered at full width).
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("\u{1F50A} AUDITION")
                        .size(10.0)
                        .strong()
                        .color(theme.text_dim),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Compute the pill label from the timeline
                    // selection. Picks:
                    //   - explicitly chosen clip → its filename
                    //   - else first selected clip → its filename
                    //   - else falls back to the placeholder
                    //
                    // The "Source:" prefix label is intentionally
                    // dropped — on the typical browser-panel width the
                    // pill plus the prefix don't both fit, and any
                    // attempt to render the prefix conditionally ended
                    // up overlapping the pill (the "output.mp4" /
                    // "Source:" garbled-text bug). The pill's caret
                    // already advertises "click for source picker".
                    let pill_label =
                        resolve_source_label(*audition_source, source_options);
                    audition_source_pill(
                        ui,
                        theme,
                        &pill_label,
                        audition_source,
                        source_options,
                    );
                });
            });

            ui.add_space(6.0);

            // Player row: play button · waveform · time · level meter.
            ui.horizontal(|ui| {
                // ── Play / pause button ──
                let play_size = 36.0;
                let (play_rect, play_resp) = ui.allocate_exact_size(
                    egui::vec2(play_size, play_size),
                    egui::Sense::click(),
                );
                let accent = theme.accent;
                let painter = ui.painter();
                painter.circle_filled(play_rect.center(), play_size * 0.5, accent);
                if *playing {
                    let bar_w = 4.0;
                    let bar_h = 13.0;
                    let cx = play_rect.center().x;
                    let cy = play_rect.center().y;
                    painter.rect_filled(
                        egui::Rect::from_center_size(
                            egui::pos2(cx - 4.5, cy),
                            egui::vec2(bar_w, bar_h),
                        ),
                        1.0,
                        theme.text_on_accent,
                    );
                    painter.rect_filled(
                        egui::Rect::from_center_size(
                            egui::pos2(cx + 4.5, cy),
                            egui::vec2(bar_w, bar_h),
                        ),
                        1.0,
                        theme.text_on_accent,
                    );
                } else {
                    let cx = play_rect.center().x;
                    let cy = play_rect.center().y;
                    let r = 7.0;
                    let pts = [
                        egui::pos2(cx - r * 0.55, cy - r),
                        egui::pos2(cx - r * 0.55, cy + r),
                        egui::pos2(cx + r * 0.95, cy),
                    ];
                    painter.add(egui::Shape::convex_polygon(
                        pts.to_vec(),
                        theme.text_on_accent,
                        egui::Stroke::NONE,
                    ));
                }
                if play_resp.clicked() {
                    if *playing {
                        tracing::info!("audition: stop clicked");
                        cmds.push(AppCommand::AuditionEnd);
                        *playing = false;
                        // Freeze the displayed playhead at the current
                        // wall-clock-derived position so it doesn't
                        // jump back to 0.
                        *audition_anchor_secs = position_secs;
                        *audition_anchor_at = None;
                    } else {
                        tracing::info!(
                            "audition: play clicked path={:?} media_id={:?}",
                            audition_path,
                            audition_media_id,
                        );
                        let start = audition_anchor_secs
                            .clamp(0.0, duration_secs.max(0.0));
                        // AuditionStart takes the audition lock so
                        // cursor-skim handlers can't silence the
                        // session when the mouse stops moving.
                        // Without it, hovering the timeline at zero
                        // velocity would call `silence()` on the
                        // shared slot and kill audio mid-track.
                        if let (Some(path), Some(mid)) =
                            (audition_path, audition_media_id)
                        {
                            cmds.push(AppCommand::AuditionStart {
                                media_id: mid,
                                source_path: path.to_path_buf(),
                                time_secs: start,
                            });
                            *playing = true;
                            *audition_anchor_secs = start;
                            *audition_anchor_at =
                                Some(std::time::Instant::now());
                        } else {
                            tracing::warn!(
                                "audition: play clicked but no path/media_id resolved"
                            );
                        }
                    }
                }
                let _ = play_resp.on_hover_text(if audition_path.is_some() {
                    "Play / pause audition"
                } else {
                    "Select a clip on the timeline first"
                });

                ui.add_space(8.0);

                // ── Waveform ──
                //
                // Renders the real audio peaks for the active source
                // when they're cached; falls back to a "loading" hint
                // while the worker computes them. Each peak is a
                // (min, max) pair scaled into a vertical bar.
                //
                // Click anywhere on the waveform to seek the audition
                // playhead to that point. The whole rect is a
                // click-and-drag target so dragging scrubs while held.
                let avail_w = ui.available_width();
                let wave_w = (avail_w - 130.0).max(160.0);
                let wave_h = 96.0;
                let (wave_rect, wave_resp) = ui.allocate_exact_size(
                    egui::vec2(wave_w, wave_h),
                    egui::Sense::click_and_drag(),
                );
                let painter = ui.painter();
                painter.rect_filled(
                    wave_rect,
                    egui::Rounding::same(4.0),
                    theme.bg_panel,
                );
                painter.rect_stroke(
                    wave_rect,
                    egui::Rounding::same(4.0),
                    egui::Stroke::new(1.0_f32, theme.border),
                );
                let bar_color = egui::Color32::from_rgb(0x3a, 0x8c, 0x6e);

                if audition_failed {
                    painter.text(
                        wave_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Couldn't read audio from source",
                        egui::FontId::proportional(10.0),
                        theme.marker_warning,
                    );
                } else if let Some(peaks) = audition_peaks {
                    if peaks.is_empty() {
                        painter.text(
                            wave_rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "No audio in source",
                            egui::FontId::proportional(10.0),
                            theme.text_dim,
                        );
                    } else {
                        // Normalize so the loudest peak fills the rect.
                        // Most program audio has peaks well below 1.0
                        // (typical broadcast / dialog sits around 0.3),
                        // so naïve absolute scaling left the waveform
                        // looking 30% as tall as the rect even on
                        // active passages.
                        let max_amp = peaks
                            .iter()
                            .map(|(mn, mx)| mx.max(-mn).abs())
                            .fold(0.0f32, f32::max)
                            .max(0.05); // floor, so silence doesn't divide-by-tiny
                        let gain = (1.0 / max_amp).min(8.0);
                        let bar_pitch = 3.0;
                        let bar_w = 2.0;
                        let bar_count = ((wave_rect.width() - 6.0) / bar_pitch) as usize;
                        let inner_h = wave_rect.height() - 6.0;
                        let center_y = wave_rect.center().y;
                        for i in 0..bar_count {
                            // Map bar index → peak index by interpolation
                            // across however many peaks the worker
                            // produced (typically 144 for FILE_AUDIO_PEAKS_BUCKETS).
                            let pi = (i as f32 / bar_count.max(1) as f32
                                * peaks.len() as f32)
                                as usize;
                            let pi = pi.min(peaks.len() - 1);
                            let (min, max) = peaks[pi];
                            // Symmetric magnitude × gain → fills the rect.
                            let amp = (max.max(-min).abs() * gain).clamp(0.0, 1.0);
                            let h = (amp * inner_h).max(1.5);
                            let x = wave_rect.min.x + 3.0 + i as f32 * bar_pitch;
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(x, center_y - h * 0.5),
                                    egui::vec2(bar_w, h),
                                ),
                                1.0,
                                bar_color,
                            );
                        }
                    }
                } else if audition_path.is_some() {
                    // Peaks worker is still running — show a quiet
                    // hint so the user knows it's not stuck. Force a
                    // repaint while we wait so the moment results
                    // land in the channel they get drained on the
                    // next frame instead of stalling until the user
                    // moves the mouse.
                    painter.text(
                        wave_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Loading waveform\u{2026}",
                        egui::FontId::proportional(10.0),
                        theme.text_dim,
                    );
                    ui.ctx().request_repaint_after(std::time::Duration::from_millis(150));
                } else {
                    painter.text(
                        wave_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Select a clip on the timeline",
                        egui::FontId::proportional(10.0),
                        theme.text_dim,
                    );
                }

                // ── Playhead ──
                if duration_secs > 0.0 && audition_path.is_some() {
                    let inner_left = wave_rect.min.x + 3.0;
                    let inner_w = wave_rect.width() - 6.0;
                    let frac = (position_secs / duration_secs).clamp(0.0, 1.0) as f32;
                    let head_x = inner_left + frac * inner_w;
                    let line_y0 = wave_rect.min.y + 2.0;
                    let line_y1 = wave_rect.max.y - 2.0;
                    ui.painter().line_segment(
                        [egui::pos2(head_x, line_y0), egui::pos2(head_x, line_y1)],
                        egui::Stroke::new(1.5_f32, theme.accent),
                    );
                    // Small triangle "tag" at top so the head reads
                    // even when the underlying waveform is dense.
                    let tri = [
                        egui::pos2(head_x - 3.5, line_y0),
                        egui::pos2(head_x + 3.5, line_y0),
                        egui::pos2(head_x, line_y0 + 4.0),
                    ];
                    ui.painter().add(egui::Shape::convex_polygon(
                        tri.to_vec(),
                        theme.accent,
                        egui::Stroke::NONE,
                    ));
                }

                // ── Click / drag → seek ──
                if duration_secs > 0.0 && audition_path.is_some() {
                    let pos_to_secs = |x: f32| -> f64 {
                        let inner_left = wave_rect.min.x + 3.0;
                        let inner_w = (wave_rect.width() - 6.0).max(1.0);
                        let frac = ((x - inner_left) / inner_w).clamp(0.0, 1.0) as f64;
                        frac * duration_secs
                    };
                    if wave_resp.clicked() || wave_resp.dragged() {
                        if let Some(p) =
                            wave_resp.interact_pointer_pos().or_else(|| wave_resp.hover_pos())
                        {
                            let new_pos = pos_to_secs(p.x);
                            *audition_anchor_secs = new_pos;
                            // Only rebase the wall-clock anchor while
                            // we're playing — paused seeks just freeze
                            // the displayed position at the new point.
                            if *playing {
                                *audition_anchor_at = Some(std::time::Instant::now());
                                cmds.push(AppCommand::AuditionSeek { time_secs: new_pos });
                            } else {
                                *audition_anchor_at = None;
                            }
                        }
                    }
                    // While playing, request repaints so the playhead
                    // animates smoothly without the user having to
                    // move the mouse.
                    if *playing {
                        ui.ctx().request_repaint();
                    }
                }

                ui.add_space(8.0);

                // ── Time readout ──
                //
                // Position is computed from the wall-clock anchor on
                // the UI thread, which is good enough for a visual
                // playhead. The skim engine has the authoritative
                // cursor; we don't round-trip it every frame to keep
                // the UI cheap.
                let total_label = audition_duration_secs
                    .map(format_mmss)
                    .unwrap_or_else(|| "--:--".to_string());
                let pos_label = if audition_path.is_some() {
                    format_mmss(position_secs)
                } else {
                    "0:00".to_string()
                };
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(pos_label)
                            .size(11.0)
                            .strong()
                            .color(theme.text_primary)
                            .monospace(),
                    );
                    ui.label(
                        egui::RichText::new(format!("/ {}", total_label))
                            .size(10.0)
                            .color(theme.text_dim)
                            .monospace(),
                    );
                });

                ui.add_space(8.0);

                // ── Volume slider + VU meter (stacked) ──
                //
                // Volume is a draggable horizontal slider that pushes
                // straight to the active SkimAudioSession via
                // `AuditionSetVolume`. Below it sits a small VU meter
                // sourced from the cached peaks at the live playhead
                // position — a real audio-thread tap would be more
                // honest, but the peaks already track the source's
                // RMS curve closely enough for visual feedback, and
                // they don't require crossing the realtime boundary.
                ui.vertical(|ui| {
                    // Volume slider
                    let prev_vol = *audition_volume;
                    let slider_w = 64.0;
                    let slider_h = 14.0;
                    let (slider_rect, slider_resp) = ui.allocate_exact_size(
                        egui::vec2(slider_w, slider_h),
                        egui::Sense::click_and_drag(),
                    );
                    let track_h = 4.0;
                    let track_rect = egui::Rect::from_center_size(
                        slider_rect.center(),
                        egui::vec2(slider_w, track_h),
                    );
                    let painter = ui.painter();
                    painter.rect_filled(
                        track_rect,
                        egui::Rounding::same(2.0),
                        theme.bg_panel,
                    );
                    // 1.0 sits at ~66% of the slider width (range
                    // 0.0–1.5) so unity gets a natural detent.
                    const VOL_MAX: f32 = 1.5;
                    let frac = (*audition_volume / VOL_MAX).clamp(0.0, 1.0);
                    let fill_w = slider_w * frac;
                    let fill_rect = egui::Rect::from_min_size(
                        slider_rect.min
                            + egui::vec2(0.0, slider_h * 0.5 - track_h * 0.5),
                        egui::vec2(fill_w, track_h),
                    );
                    painter.rect_filled(
                        fill_rect,
                        egui::Rounding::same(2.0),
                        theme.accent,
                    );
                    // Knob
                    let knob_x = slider_rect.min.x + fill_w;
                    let knob_y = slider_rect.center().y;
                    let knob_r = 5.0;
                    painter.circle_filled(
                        egui::pos2(knob_x, knob_y),
                        knob_r,
                        theme.accent,
                    );
                    painter.circle_stroke(
                        egui::pos2(knob_x, knob_y),
                        knob_r,
                        egui::Stroke::new(1.0_f32, theme.text_on_accent),
                    );
                    if slider_resp.dragged() || slider_resp.clicked() {
                        if let Some(p) = slider_resp
                            .interact_pointer_pos()
                            .or_else(|| slider_resp.hover_pos())
                        {
                            let new_frac = ((p.x - slider_rect.min.x)
                                / slider_w.max(1.0))
                            .clamp(0.0, 1.0);
                            *audition_volume = new_frac * VOL_MAX;
                        }
                    }
                    // Double-click resets to unity.
                    if slider_resp.double_clicked() {
                        *audition_volume = 1.0;
                    }
                    let _ = slider_resp.on_hover_text(format!(
                        "Volume: {:.0}%",
                        *audition_volume * 100.0
                    ));
                    if (*audition_volume - prev_vol).abs() > 0.001 {
                        cmds.push(AppCommand::AuditionSetVolume(*audition_volume));
                    }

                    // VU meter — derive level from current peak bucket
                    // when the audition is rolling, otherwise stay
                    // dim. Two-band: green up to ~0.85, yellow
                    // toward unity, red over 1.0 (none of which can
                    // happen with 0–1 normalised peaks, but the bands
                    // give the meter the right colour grammar).
                    ui.add_space(3.0);
                    let vu_w = slider_w;
                    let vu_h = 6.0;
                    let (vu_rect, _) = ui.allocate_exact_size(
                        egui::vec2(vu_w, vu_h),
                        egui::Sense::hover(),
                    );
                    let painter = ui.painter();
                    painter.rect_filled(
                        vu_rect,
                        egui::Rounding::same(2.0),
                        theme.bg_panel,
                    );
                    let level: f32 = if *playing {
                        if let (Some(peaks), Some(dur)) =
                            (audition_peaks, audition_duration_secs)
                        {
                            if dur > 0.0 && !peaks.is_empty() {
                                let frac = (position_secs / dur)
                                    .clamp(0.0, 1.0)
                                    as f32;
                                let pi = (frac * peaks.len() as f32) as usize;
                                let pi = pi.min(peaks.len() - 1);
                                let (mn, mx) = peaks[pi];
                                let amp = mx.max(-mn).abs();
                                // Multiply by current volume so the
                                // VU follows the slider too.
                                (amp * *audition_volume).clamp(0.0, 1.5)
                            } else {
                                0.0
                            }
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };
                    let lvl_w = (vu_w * (level / 1.5).clamp(0.0, 1.0)).max(0.0);
                    if lvl_w > 0.0 {
                        let lvl_rect = egui::Rect::from_min_size(
                            vu_rect.min,
                            egui::vec2(lvl_w, vu_h),
                        );
                        // Pick colour from level: green / yellow / red.
                        let colour = if level > 1.0 {
                            egui::Color32::from_rgb(0xd0, 0x4a, 0x4a)
                        } else if level > 0.85 {
                            egui::Color32::from_rgb(0xd8, 0xb6, 0x4a)
                        } else {
                            egui::Color32::from_rgb(0x3a, 0x8c, 0x6e)
                        };
                        painter.rect_filled(
                            lvl_rect,
                            egui::Rounding::same(2.0),
                            colour,
                        );
                    }
                });
            });
        });
    cmds
}

fn audition_source_pill(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    audition_source: &mut Option<klipp_core::ClipId>,
    source_options: &[AuditionSourceOption],
) {
    // Pill width adapts strictly to available space — never forces a
    // minimum, otherwise the pill overruns the right-to-left
    // allocation and paints on top of the "Source:" label (the
    // garbled overlap users hit on narrow panels).
    let pad_x = 10.0;
    let caret_w = 14.0;
    let h = 24.0;
    // min/max, not clamp: a non-finite available width settles on a
    // usable number here, where clamp would propagate a NaN into every
    // width derived from it below.
    #[allow(clippy::manual_clamp)]
    let max_w = ui.available_width().min(220.0).max(60.0);
    let label_inner_w = (max_w - pad_x * 2.0 - caret_w).max(20.0);
    let truncated_label = truncate_to_width(
        ui,
        label,
        label_inner_w,
        &egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    let label_galley = ui.painter().layout_no_wrap(
        truncated_label,
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    let w = pad_x * 2.0 + label_galley.size().x + caret_w;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(w, h),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let bg = if hovered { theme.bg_raised } else { theme.bg_panel };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(
        rect,
        12.0,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.painter().galley(
        egui::pos2(
            rect.min.x + pad_x,
            rect.center().y - label_galley.size().y * 0.5,
        ),
        label_galley.clone(),
        theme.text_secondary,
    );
    let caret_x = rect.max.x - caret_w;
    let caret_size = 10.0;
    let caret_rect = egui::Rect::from_min_size(
        egui::pos2(caret_x, rect.center().y - caret_size * 0.5),
        egui::vec2(caret_size, caret_size),
    );
    let img = Icon::CaretDown.image(caret_size).tint(theme.text_dim);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(caret_rect),
        |ui| {
            ui.add(img);
        },
    );

    // Popup menu — opened on click, anchored under the pill. Each
    // entry corresponds to one selected clip on the timeline; clicking
    // an entry stores its ClipId as the audition source.
    let popup_id = ui.make_persistent_id("audition_source_popup");
    if resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    let resp_for_hover = resp.clone();
    let _ = if source_options.is_empty() {
        resp_for_hover.on_hover_text("Select a clip on the timeline to audition against")
    } else {
        resp_for_hover.on_hover_text("Pick audition source")
    };
    egui::popup::popup_below_widget(
        ui,
        popup_id,
        &resp,
        egui::PopupCloseBehavior::CloseOnClick,
        |ui| {
            ui.set_min_width(rect.width().max(180.0));
            if source_options.is_empty() {
                ui.label(
                    egui::RichText::new("No clips selected")
                        .size(11.0)
                        .color(theme.text_dim),
                );
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "Select a clip on the timeline to audition with its audio.",
                    )
                    .size(10.0)
                    .color(theme.text_dim),
                );
                return;
            }
            for opt in source_options {
                let is_active = *audition_source == Some(opt.clip_id);
                let resp = ui.selectable_label(is_active, &opt.label);
                if resp.clicked() {
                    *audition_source = Some(opt.clip_id);
                }
            }
        },
    );
}

/// Pick the displayed pill label from the audition state. Falls back
/// through: explicit pick → first selected clip → placeholder.
fn resolve_source_label(
    audition_source: Option<klipp_core::ClipId>,
    source_options: &[AuditionSourceOption],
) -> String {
    if let Some(picked) = audition_source {
        if let Some(opt) = source_options.iter().find(|o| o.clip_id == picked) {
            return opt.label.clone();
        }
    }
    if let Some(first) = source_options.first() {
        if source_options.len() > 1 {
            return format!("{} (+{} more)", first.label, source_options.len() - 1);
        }
        return first.label.clone();
    }
    // No timeline clip selected — show a hint that doubles as
    // discoverability for the dropdown ("click to pick a source").
    "No clip selected".to_string()
}

// ──────────────────────────── Source segmented ────────────────────────────

fn segmented_audio_source_filter(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    filter: &mut AudioSourceFilter,
    all_count: usize,
    builtin_count: usize,
    ladspa_count: usize,
    lv2_count: usize,
    clap_count: usize,
) {
    let outer_h = 26.0;
    let pad = 2.0;
    let segments = [
        (AudioSourceFilter::All, "All", all_count),
        (AudioSourceFilter::Builtin, "Built-in", builtin_count),
        (AudioSourceFilter::Ladspa, "LADSPA", ladspa_count),
        (AudioSourceFilter::Lv2, "LV2", lv2_count),
        (AudioSourceFilter::Clap, "CLAP", clap_count),
    ];

    let mut seg_widths = [0.0f32; 5];
    let mut total_w = pad * 2.0;
    for (i, (_, label, count)) in segments.iter().enumerate() {
        let label_galley = ui.painter().layout_no_wrap(
            (*label).to_string(),
            egui::FontId::proportional(11.0),
            theme.text_secondary,
        );
        let count_galley = ui.painter().layout_no_wrap(
            format!("{}", count),
            egui::FontId::monospace(10.0),
            theme.text_dim,
        );
        let w = label_galley.size().x + 6.0 + count_galley.size().x + 22.0;
        seg_widths[i] = w;
        total_w += w + 1.0;
    }
    total_w -= 1.0;

    let (outer_rect, _) = ui.allocate_exact_size(
        egui::vec2(total_w, outer_h),
        egui::Sense::hover(),
    );

    ui.painter().rect_filled(outer_rect, 4.0, theme.bg_panel);
    ui.painter().rect_stroke(
        outer_rect,
        5.0,
        egui::Stroke::new(1.0_f32, theme.border),
    );

    let mut x = outer_rect.min.x + pad;
    for (i, (variant, label, count)) in segments.iter().enumerate() {
        let seg_rect = egui::Rect::from_min_size(
            egui::pos2(x, outer_rect.min.y + pad),
            egui::vec2(seg_widths[i], outer_h - pad * 2.0),
        );
        let resp = ui.interact(
            seg_rect,
            ui.id().with(("audio_source_seg", i)),
            egui::Sense::click(),
        );
        let is_active = *filter == *variant;
        let hovered = resp.hovered();

        if is_active {
            ui.painter().rect_filled(seg_rect, 2.0, theme.bg_raised);
        } else if hovered {
            ui.painter().rect_filled(seg_rect, 2.0, theme.accent_subtle);
        }

        let label_color = if is_active || hovered {
            theme.text_primary
        } else {
            theme.text_secondary
        };
        let count_color = if is_active { theme.accent } else { theme.text_dim };

        let label_galley = ui.painter().layout_no_wrap(
            (*label).to_string(),
            egui::FontId::proportional(11.0),
            label_color,
        );
        let count_galley = ui.painter().layout_no_wrap(
            format!("{}", count),
            egui::FontId::monospace(10.0),
            count_color,
        );

        let inner_w = label_galley.size().x + 6.0 + count_galley.size().x;
        let lx = seg_rect.center().x - inner_w * 0.5;
        ui.painter().galley(
            egui::pos2(lx, seg_rect.center().y - label_galley.size().y * 0.5),
            label_galley.clone(),
            label_color,
        );
        ui.painter().galley(
            egui::pos2(
                lx + label_galley.size().x + 6.0,
                seg_rect.center().y - count_galley.size().y * 0.5,
            ),
            count_galley,
            count_color,
        );

        if resp.clicked() {
            *filter = *variant;
        }

        x += seg_widths[i] + 1.0;
    }
}

// ───────────────────────────── Eye toggle button ─────────────────────────────

fn eye_toggle_button(ui: &mut egui::Ui, theme: &KlippTheme, hide_unsupported: &mut bool) {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(28.0, 26.0),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let (bg, border, fg) = if *hide_unsupported {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if hovered {
        (theme.bg_raised, theme.border, theme.text_primary)
    } else {
        (theme.bg_panel, theme.border, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, border));

    let icon = if *hide_unsupported { Icon::EyeSlash } else { Icon::Eye };
    let icon_size = 13.0;
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(icon_size, icon_size));
    let img = icon.image(icon_size).tint(fg);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    if resp.clicked() {
        *hide_unsupported = !*hide_unsupported;
    }
    let _ = resp.on_hover_text("Hide unsupported plugins");
}

// ───────────────────────────── Sidebar widgets ─────────────────────────────

fn sidebar_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
    count: usize,
    is_active: bool,
    id_salt: &str,
    on_click: &mut dyn FnMut(EffectCategory),
    target_cat: EffectCategory,
) {
    let row_h = 26.0;
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());
    let hovered = resp.hovered() || click_resp.hovered();
    let painter = ui.painter();

    if is_active {
        painter.rect_filled(rect, 0.0, theme.accent_subtle);
        let stripe_rect = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(2.0, rect.height()),
        );
        painter.rect_filled(stripe_rect, 0.0, theme.accent);
    } else if hovered {
        painter.rect_filled(rect, 0.0, theme.bg_raised);
    }

    let indent = 14.0;
    let icon_size = 13.0;
    let icon_gap = 8.0;
    let label_x = rect.min.x + indent + icon_size + icon_gap;
    let label_color = if is_active || hovered {
        theme.text_primary
    } else {
        theme.text_secondary
    };
    let icon_tint = if is_active { theme.accent } else { theme.text_dim };

    if let Some(icon) = icon {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
            egui::vec2(icon_size, icon_size),
        );
        let img = icon.image(icon_size).tint(icon_tint);
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(icon_rect),
            |ui| {
                ui.add(img);
            },
        );
    }

    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(11.0),
        label_color,
    );
    ui.painter().galley(
        egui::pos2(label_x, rect.center().y - label_galley.size().y * 0.5),
        label_galley,
        label_color,
    );

    let count_str = format!("{}", count);
    let count_color = if is_active { theme.accent } else { theme.text_dim };
    let count_galley = ui.painter().layout_no_wrap(
        count_str,
        egui::FontId::monospace(10.0),
        count_color,
    );
    let count_pos = egui::pos2(
        rect.max.x - 14.0 - count_galley.size().x,
        rect.center().y - count_galley.size().y * 0.5,
    );
    ui.painter().galley(count_pos, count_galley, count_color);

    if click_resp.clicked() || resp.clicked() {
        on_click(target_cat);
    }
}

fn sidebar_section_header(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
) {
    let row_h = 20.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), row_h),
        egui::Sense::hover(),
    );
    let indent = 14.0;
    let icon_size = 11.0;
    let icon_gap = 6.0;
    let label_x = if icon.is_some() {
        rect.min.x + indent + icon_size + icon_gap
    } else {
        rect.min.x + indent
    };

    if let Some(icon) = icon {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
            egui::vec2(icon_size, icon_size),
        );
        let img = icon.image(icon_size).tint(theme.text_dim);
        ui.allocate_new_ui(
            egui::UiBuilder::new().max_rect(icon_rect),
            |ui| {
                ui.add(img);
            },
        );
    }

    ui.painter().text(
        egui::pos2(label_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label.to_uppercase(),
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );
}

fn sidebar_section_separator(ui: &mut egui::Ui, theme: &KlippTheme) {
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(avail_w, 1.0),
        egui::Sense::hover(),
    );
    ui.painter().hline(
        (rect.min.x + 8.0)..=(rect.max.x - 8.0),
        rect.center().y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.add_space(4.0);
}

// ───────────────────────────── Result strip ─────────────────────────────

fn result_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    count: usize,
    cat: EffectCategory,
    sort_mode: &mut EffectSort,
) {
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(avail_w, 24.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        rect.x_range(),
        rect.max.y,
        egui::Stroke::new(1.0_f32, theme.border),
    );

    let scope_label = match cat {
        EffectCategory::Favourites => "Favorites",
        EffectCategory::Recent => "Recent",
        EffectCategory::Installed => "Installed",
        _ => "Audio FX",
    };
    let left = format!("{} {}", count, scope_label);
    ui.painter().text(
        egui::pos2(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        left,
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );

    let dropdown_w = 116.0;
    let dropdown_rect = egui::Rect::from_min_max(
        egui::pos2(rect.max.x - dropdown_w - 8.0, rect.min.y + 1.0),
        egui::pos2(rect.max.x - 8.0, rect.max.y - 1.0),
    );
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(dropdown_rect),
        |ui| {
            ui.style_mut().visuals.widgets.inactive.weak_bg_fill =
                egui::Color32::TRANSPARENT;
            ui.style_mut().visuals.widgets.inactive.bg_stroke =
                egui::Stroke::NONE;
            let label = format!("Sort: {}", sort_mode.label());
            egui::ComboBox::from_id_salt("audio_fx_sort_dd")
                .selected_text(
                    egui::RichText::new(label).size(11.0).color(theme.text_dim),
                )
                .width(dropdown_w - 4.0)
                .show_ui(ui, |ui| {
                    for variant in [
                        EffectSort::NameAsc,
                        EffectSort::NameDesc,
                        EffectSort::Category,
                        EffectSort::Source,
                    ] {
                        ui.selectable_value(sort_mode, variant, variant.label());
                    }
                });
        },
    );
}

// ───────────────────────────── List row ─────────────────────────────

const ROW_PLAY_W: f32 = 28.0;
const ROW_HEIGHT: f32 = 56.0;

/// Play-button rect, vertically centered on the row and inset from the
/// row's left edge. The same offset is used by `audio_fx_row` to
/// position the icon tile, so the play button and the rest of the row
/// chrome share one baseline.
fn play_button_rect(row_rect: egui::Rect) -> egui::Rect {
    let cx = row_rect.min.x + 12.0 + ROW_PLAY_W * 0.5;
    egui::Rect::from_center_size(
        egui::pos2(cx, row_rect.center().y),
        egui::vec2(ROW_PLAY_W, ROW_PLAY_W),
    )
}

fn paint_row_play_button(
    ui: &egui::Ui,
    theme: &KlippTheme,
    rect: egui::Rect,
    hovered: bool,
    selected: bool,
    accent: egui::Color32,
) {
    let painter = ui.painter();

    if selected {
        // Filled accent disk with a stop-square glyph — matches the
        // mockup's "currently auditioning" affordance.
        painter.circle_filled(rect.center(), rect.width() * 0.5, accent);
        let sq = 8.0;
        painter.rect_filled(
            egui::Rect::from_center_size(rect.center(), egui::vec2(sq, sq)),
            1.0,
            theme.text_on_accent,
        );
        return;
    }

    // Idle: subtle disk, hover lifts it. No border ring — keeps the
    // row visually quiet.
    let fill = if hovered { theme.bg_raised } else { theme.bg_panel };
    let glyph_color = if hovered { theme.text_primary } else { theme.text_secondary };
    painter.circle_filled(rect.center(), rect.width() * 0.5, fill);

    // Play triangle, slightly nudged right so it reads as
    // forward-pointing rather than off-centre.
    let cx = rect.center().x + 1.0;
    let cy = rect.center().y;
    let r = 5.0;
    let pts = [
        egui::pos2(cx - r * 0.55, cy - r),
        egui::pos2(cx - r * 0.55, cy + r),
        egui::pos2(cx + r * 0.95, cy),
    ];
    painter.add(egui::Shape::convex_polygon(
        pts.to_vec(),
        glyph_color,
        egui::Stroke::NONE,
    ));
}

fn audio_fx_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    entry: &EffectEntry,
    is_selected: bool,
) -> egui::Response {
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, ROW_HEIGHT),
        egui::Sense::hover(),
    );
    let row_rect = rect;

    // Flat by default — no border, no rounded card. The mockup's row
    // chrome only appears on the selected row, where we paint a
    // soft accent-tinted background + a 2px accent stripe on the
    // left edge to match the "currently auditioning" affordance.
    //
    // `from_rgba_unmultiplied` is required: the previous
    // `from_rgba_premultiplied` call passed raw accent RGB with a low
    // alpha, which egui interpreted as "already-multiplied" and ended
    // up compositing at ~91% opacity. The highlight read as a solid
    // block taller than the row instead of the intended soft tint.
    if is_selected {
        // Highlight sits INSIDE the row, sized to the icon-tile band
        // rather than the full allocation. The 56-px row carries
        // padding that the visual content doesn't actually use; if
        // we paint the highlight at the full row bounds it reads as
        // taller than the unselected rows next to it.
        const HIGHLIGHT_HEIGHT: f32 = 40.0; // ~icon-tile + 4px padding
        const HIGHLIGHT_INSET_X: f32 = 8.0;
        let bg = egui::Color32::from_rgba_unmultiplied(
            theme.accent.r(),
            theme.accent.g(),
            theme.accent.b(),
            28,
        );
        let inner = egui::Rect::from_min_size(
            egui::pos2(
                row_rect.min.x + HIGHLIGHT_INSET_X,
                row_rect.center().y - HIGHLIGHT_HEIGHT * 0.5,
            ),
            egui::vec2(
                row_rect.width() - HIGHLIGHT_INSET_X * 2.0,
                HIGHLIGHT_HEIGHT,
            ),
        );
        ui.painter()
            .rect_filled(inner, egui::Rounding::same(4.0), bg);
        let stripe_rect = egui::Rect::from_min_size(
            inner.min,
            egui::vec2(2.0, inner.height()),
        );
        ui.painter()
            .rect_filled(stripe_rect, egui::Rounding::same(1.0), theme.accent);
    }

    // Reserve play button slot at left (painted by caller).
    let after_play_x = row_rect.min.x + 12.0 + ROW_PLAY_W + 10.0;

    // ── Colored icon tile ──
    let color = tile_color(entry, theme);
    let tile_size = 36.0;
    let tile_rect = egui::Rect::from_min_size(
        egui::pos2(after_play_x, row_rect.center().y - tile_size * 0.5),
        egui::vec2(tile_size, tile_size),
    );
    let icon_bg = egui::Color32::from_rgba_premultiplied(
        color.r(),
        color.g(),
        color.b(),
        40,
    );
    let icon_border = egui::Color32::from_rgba_premultiplied(
        color.r(),
        color.g(),
        color.b(),
        90,
    );
    ui.painter().rect_filled(tile_rect, egui::Rounding::same(4.0), icon_bg);
    ui.painter().rect_stroke(
        tile_rect,
        egui::Rounding::same(4.0),
        egui::Stroke::new(1.0_f32, icon_border),
    );
    let glyph = category_glyph_icon(entry.category);
    let glyph_size = 18.0;
    let glyph_rect = egui::Rect::from_center_size(
        tile_rect.center(),
        egui::vec2(glyph_size, glyph_size),
    );
    let img = glyph.image(glyph_size).tint(color);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(glyph_rect),
        |ui| {
            ui.add(img);
        },
    );

    // ── Text block: name + category · description ──
    let text_x = tile_rect.max.x + 12.0;

    let text_max_x = row_rect.max.x - 12.0;
    let text_w = (text_max_x - text_x).max(40.0);

    // Single-line layouts with manual ellipsis truncation. The earlier
    // `painter.layout(text, font, color, text_w)` wraps to multiple
    // lines, which on long LADSPA descriptions (e.g. "Achim Settelmeier
    // <…> (adapted by Josh Green and Hexasoft) · by Achim Settelmeier
    // <…>") spilled the wrapped text down into the next row.
    let name_font = egui::FontId::proportional(12.5);
    let sub_font = egui::FontId::proportional(11.0);
    let name_truncated = truncate_to_width(
        ui,
        &entry.name,
        text_w,
        &name_font,
        theme.text_primary,
    );
    let name_galley = ui.painter().layout_no_wrap(
        name_truncated,
        name_font,
        theme.text_primary,
    );

    // Category, then what the effect does for a built-in, or where a
    // plugin comes from (its format and maker), never a URI.
    let what = description_for(entry);
    let plugin_line = what.starts_with("LV2 plugin:") || what.starts_with("CLAP plugin:") || what.starts_with("LADSPA plugin:");
    let category = crate::browser::effects_tab::category_label(entry);
    let sub_text = if plugin_line {
        if entry.grouping.is_empty() || entry.grouping == entry.source_label {
            format!("{category} \u{00B7} {}", entry.source_label)
        } else {
            format!("{category} \u{00B7} {} \u{00B7} {}", entry.source_label, entry.grouping)
        }
    } else {
        format!("{category} \u{00B7} {what}")
    };
    let sub_truncated = truncate_to_width(
        ui,
        &sub_text,
        text_w,
        &sub_font,
        theme.text_dim,
    );
    let sub_galley = ui.painter().layout_no_wrap(
        sub_truncated,
        sub_font,
        theme.text_dim,
    );

    let total_h = name_galley.size().y + sub_galley.size().y + 2.0;
    let name_y = row_rect.center().y - total_h * 0.5;
    ui.painter().galley(
        egui::pos2(text_x, name_y),
        name_galley.clone(),
        theme.text_primary,
    );
    ui.painter().galley(
        egui::pos2(text_x, name_y + name_galley.size().y + 2.0),
        sub_galley,
        theme.text_dim,
    );

    resp
}

/// Trim `text` so it renders at most `max_w` pixels wide in `font`,
/// appending an ellipsis when truncation happens. Cheap linear walk —
/// a single row's text is short enough that binary-search is overkill.
fn truncate_to_width(
    ui: &egui::Ui,
    text: &str,
    max_w: f32,
    font: &egui::FontId,
    color: egui::Color32,
) -> String {
    let painter = ui.painter();
    let full_w = painter
        .layout_no_wrap(text.to_string(), font.clone(), color)
        .size()
        .x;
    if full_w <= max_w {
        return text.to_string();
    }
    let ellipsis = "\u{2026}";
    let chars: Vec<char> = text.chars().collect();
    // Walk back from the end, dropping one character at a time until
    // the truncated string + ellipsis fits.
    let mut len = chars.len();
    while len > 0 {
        let mut s: String = chars[..len].iter().collect();
        s.push_str(ellipsis);
        let w = painter
            .layout_no_wrap(s.clone(), font.clone(), color)
            .size()
            .x;
        if w <= max_w {
            return s;
        }
        len -= 1;
    }
    ellipsis.to_string()
}

/// Format a duration in seconds as `M:SS` (or `H:MM:SS` if it crosses
/// the hour boundary). Used for the audition transport readout.
fn format_mmss(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let hh = total / 3600;
    let mm = (total / 60) % 60;
    let ss = total % 60;
    if hh > 0 {
        format!("{hh}:{mm:02}:{ss:02}")
    } else {
        format!("{mm}:{ss:02}")
    }
}

/// Pick a category-flavoured glyph for the row's icon tile. Re-uses the
/// already-bundled Phosphor set; categories without an exact match
/// fall back to `Waveform` (every audio effect is a waveform shaper at
/// some level).
fn category_glyph_icon(cat: EffectCategory) -> Icon {
    match cat {
        EffectCategory::AudioEQ => Icon::Equalizer,
        EffectCategory::AudioDynamics => Icon::Waveform,
        EffectCategory::AudioReverb => Icon::SpeakerHigh,
        EffectCategory::AudioDelay => Icon::ArrowCounterClockwise,
        EffectCategory::AudioModulation => Icon::Waveform,
        EffectCategory::AudioPitch => Icon::MusicNote,
        EffectCategory::AudioRestoration => Icon::Lock,
        EffectCategory::AudioUtility => Icon::Sliders,
        _ => Icon::Waveform,
    }
}
