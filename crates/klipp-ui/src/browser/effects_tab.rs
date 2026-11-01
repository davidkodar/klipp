use crate::widgets::menu::MenuRows;
use crate::browser::effect_registry::EffectSource;
use crate::drag_drop::DragPayload;
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectCategory {
    /// UI-only pseudo-category: shows only effects the user has starred.
    /// Not stored on `EffectEntry`; the filter reads from prefs.
    Favourites,
    /// UI-only pseudo-category: shows the most recently applied effects
    /// (session-only, MRU order). Backed by `BrowserPanel::recent_effects`.
    Recent,
    /// UI-only pseudo-category: shows every effect the registry knows
    /// about, video and audio combined.
    Installed,
    All,
    /// One-click combinations of built-in effects.
    Looks,
    /// The bundled LUT presets (#3255), each put on the clip as its own
    /// 3D LUT effect.
    Luts,
    Blur,
    Color,
    Stylize,
    Keying,
    Distort,
    Light,
    /// Grain, halation and the ageing of a picture.
    Film,
    // Audio categories
    AudioAll,
    AudioEQ,
    AudioDynamics,
    AudioReverb,
    AudioDelay,
    AudioModulation,
    AudioPitch,
    AudioRestoration,
    AudioUtility,
}

/// Mutually-exclusive source filter. Replaces the previous three
/// competing colored toggles (Built-in / OFX / Hide unsupported) with a
/// single segmented control. "Hide unsupported" remains a separate
/// AppState bool — its toggle UI lands in a follow-up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum SourceFilter {
    /// Klipp's own effects: GPU, with a CPU twin. The default view.
    #[default]
    Builtin,
    /// Plugins found on this machine (frei0r, OFX): CPU-bound, slower.
    Installed,
    All,
}


/// Effects-grid layout mode. Grid renders 2-column thumbnail cards;
/// List would render a denser one-row-per-effect list. List rendering
/// itself is out of scope for the current pass — the toggle exists so
/// the segmented icon-button state has a real binding to flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum EffectViewMode {
    #[default]
    Grid,
    List,
}


/// Sort mode for the effects grid. Drives the result strip's sort
/// dropdown. `Name` is the alphabetical default; `Category` and
/// `Source` group entries before sorting alphabetically within each
/// group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum EffectSort {
    #[default]
    NameAsc,
    NameDesc,
    Category,
    Source,
}


impl EffectSort {
    pub fn label(self) -> &'static str {
        match self {
            EffectSort::NameAsc => "A\u{2192}Z",
            EffectSort::NameDesc => "Z\u{2192}A",
            EffectSort::Category => "Category",
            EffectSort::Source => "Source",
        }
    }
}

const CATEGORY_SIDEBAR_W: f32 = 168.0;

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &mut String,
    active_category: &mut EffectCategory,
    favorite_effects: &mut Vec<String>,
    recent_effects: &mut Vec<String>,
    view_mode: &mut EffectViewMode,
    sort_mode: &mut EffectSort,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let avail = ui.available_rect_before_wrap();

    // ── Split: category sidebar (left) | thumbnail grid (right) ──
    let sidebar_w = CATEGORY_SIDEBAR_W.min(avail.width() * 0.40);
    let sidebar_rect = egui::Rect::from_min_size(
        avail.min,
        egui::vec2(sidebar_w, avail.height()),
    );
    let grid_rect = egui::Rect::from_min_max(
        egui::pos2(avail.min.x + sidebar_w, avail.min.y),
        avail.max,
    );

    // ── Category sidebar (3 sections: General / Video FX / Audio FX) ──
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(sidebar_rect), |ui| {
        ui.set_min_size(sidebar_rect.size());

        // Soft sidebar fill that lifts above the panel base.
        ui.painter().rect_filled(sidebar_rect, 0.0, theme.bg_panel);
        ui.painter().vline(
            sidebar_rect.max.x,
            sidebar_rect.y_range(),
            egui::Stroke::new(1.0_f32, theme.border),
        );

        egui::ScrollArea::vertical()
            .id_salt("effects_category_scroll")
            .show(ui, |ui| {
                ui.add_space(6.0);

                // Counts that don't depend on the search query — sidebar
                // numbers reflect the registry, not the current filter.
                let video_total = state
                    .effect_registry
                    .filtered_with_favorites(EffectCategory::All, "", &[])
                    .len();
                let _audio_total = state
                    .effect_registry
                    .filtered_with_favorites(EffectCategory::AudioAll, "", &[])
                    .len();
                let installed_total = state.effect_registry.effects.len();

                // ── General section: Favorites / Recent / Installed ──
                sidebar_row(
                    ui,
                    theme,
                    Some(Icon::Star),
                    "Favorites",
                    favorite_effects.len(),
                    *active_category == EffectCategory::Favourites,
                    "general_favourites",
                    &mut |c| *active_category = c,
                    EffectCategory::Favourites,
                );
                sidebar_row(
                    ui,
                    theme,
                    // No clock icon in the bundled Phosphor set —
                    // ArrowCounterClockwise reads as "history" until a
                    // proper Clock SVG lands. Flagged as TODO.
                    Some(Icon::ArrowCounterClockwise),
                    "Recent",
                    recent_effects.len(),
                    *active_category == EffectCategory::Recent,
                    "general_recent",
                    &mut |c| *active_category = c,
                    EffectCategory::Recent,
                );
                sidebar_row(
                    ui,
                    theme,
                    // No download/install icon in the bundled set;
                    // UploadSimple is closest in shape. TODO: drop in a
                    // proper Phosphor `download-simple` SVG.
                    Some(Icon::UploadSimple),
                    "Installed",
                    installed_total,
                    *active_category == EffectCategory::Installed,
                    "general_installed",
                    &mut |c| *active_category = c,
                    EffectCategory::Installed,
                );

                ui.add_space(8.0);
                sidebar_section_separator(ui, theme);

                // ── VIDEO FX section ──
                sidebar_section_header(ui, theme, Some(Icon::FilmStrip), "Video FX");

                let video_categories: [(EffectCategory, &str, Option<Icon>); 10] = [
                    (EffectCategory::All, "All", Some(Icon::List)),
                    (EffectCategory::Looks, "Looks", Some(Icon::Star)),
                    (EffectCategory::Luts, "LUTs", Some(Icon::FilmStrip)),
                    // No drop/blur icon — leaving blank reads cleaner than
                    // a wrong-fit stand-in. TODO: bespoke blur glyph.
                    (EffectCategory::Blur, "Blur", None),
                    (EffectCategory::Color, "Colour", Some(Icon::Eyedropper)),
                    (EffectCategory::Stylize, "Stylize", Some(Icon::Sliders)),
                    // No keying-specific icon — leaving blank. TODO.
                    (EffectCategory::Keying, "Keying", None),
                    (EffectCategory::Distort, "Distort", Some(Icon::ArrowsOut)),
                    (EffectCategory::Light, "Light", Some(Icon::Lightning)),
                    (EffectCategory::Film, "Film", Some(Icon::FilmStrip)),
                ];
                for (cat, label, icon) in &video_categories {
                    let count = if *cat == EffectCategory::All {
                        video_total
                    } else {
                        state.effect_registry.filtered_with_favorites(*cat, "", &[]).len()
                    };
                    sidebar_row(
                        ui,
                        theme,
                        *icon,
                        label,
                        count,
                        *active_category == *cat,
                        &format!("video_cat_{:?}", cat),
                        &mut |c| *active_category = c,
                        *cat,
                    );
                }

                ui.add_space(8.0);
            });
    });

    // ── Thumbnail grid pane ──
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(grid_rect), |ui| {
        ui.set_min_size(grid_rect.size());

        // ── Search bar (shared style, see browser::browser_search_field) ──
        ui.add_space(6.0);
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                super::browser_search_field(ui, theme, search_query, "Search effects\u{2026}", state);
            });
        ui.add_space(6.0);

        // ── Filter row: segmented source filter (All / Built-in / OFX) ──
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(6.0);

            // Counts feed the segmented control's embedded badges. They
            // honour the active category + search query (and recents/favs)
            // so the user can always see how many entries each segment
            // would return at the current scope.
            let scoped = state
                .effect_registry
                .filtered_full(*active_category, search_query, favorite_effects, recent_effects);
            let all_count = scoped.len();
            let builtin_count = scoped
                .iter()
                .filter(|fx| {
                    matches!(
                        fx.source,
                        EffectSource::Builtin(_)
                            | EffectSource::Look(_)
                            | EffectSource::Audio(_)
                            | EffectSource::Ladspa(_)
                    )
                })
                .count();
            let ofx_count = scoped
                .iter()
                .filter(|fx| matches!(fx.source, EffectSource::Ofx(_) | EffectSource::Frei0r(_)))
                .count();

            segmented_source_filter(
                ui,
                theme,
                &mut state.effect_source_filter,
                all_count,
                builtin_count,
                ofx_count,
            );

            ui.add_space(6.0);
            // GPU-only toggle pill — accent-tinted when active.
            gpu_only_pill(ui, theme, &mut state.effect_filter_gpu);

            // Right-aligned controls: hide-unsupported, grid, list.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(6.0);
                view_mode_button(ui, theme, view_mode, EffectViewMode::List, Icon::List, "List view");
                view_mode_button(ui, theme, view_mode, EffectViewMode::Grid, Icon::GridFour, "Grid view");
                hide_unsupported_button(ui, theme, &mut state.effect_filter_hide_unsupported);
            });
        });
        ui.add_space(6.0);

        // Apply filters. Cloning into an owned Vec releases the borrow
        // on `state.effect_registry` so the per-row loop can pass `state`
        // mutably (toast, drag_drop, effect_thumbnails). EffectEntry is
        // a few strings + an enum — ~200 entries clone in ~microseconds
        // per frame, dwarfed by the egui paint cost.
        let mut filtered: Vec<crate::browser::effect_registry::EffectEntry> = state
            .effect_registry
            .filtered_full(*active_category, search_query, favorite_effects, recent_effects)
            .into_iter()
            .filter(|fx| {
                let is_builtin = matches!(
                    fx.source,
                    EffectSource::Builtin(_) | EffectSource::Look(_) | EffectSource::Audio(_) | EffectSource::Ladspa(_)
                );
                let is_installed = matches!(fx.source, EffectSource::Ofx(_) | EffectSource::Frei0r(_));

                // Source filter: Klipp's own, installed plugins, or both.
                match state.effect_source_filter {
                    SourceFilter::All => {}
                    SourceFilter::Builtin => {
                        if !is_builtin {
                            return false;
                        }
                    }
                    SourceFilter::Installed => {
                        if !is_installed {
                            return false;
                        }
                    }
                }

                // GPU-only filter — derived from EffectEntry::supports_gpu,
                // which consults `available_plugins` for the OFX flag.
                if state.effect_filter_gpu && !fx.supports_gpu(&state.available_plugins) {
                    return false;
                }

                // Hide unsupported (stereo, render-only)
                if state.effect_filter_hide_unsupported {
                    let id = match &fx.source {
                        EffectSource::Ofx(id) => id.as_str(),
                        _ => "",
                    };
                    if id.contains("anaglyph") || id.contains("joinView") || id.contains("mixView")
                        || id.contains("oneView") || id.contains("sideBySide") {
                        return false;
                    }
                }

                true
            })
            .cloned()
            .collect();

        // Re-sort according to the user-picked sort mode (registry's
        // default is alphabetical, which matches `NameAsc` — only the
        // other modes need a re-sort).
        match *sort_mode {
            EffectSort::NameAsc => {} // already sorted
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
        let count = filtered.len();

        // ── Result strip: live count + sort dropdown ──
        result_strip(
            ui,
            theme,
            count,
            *active_category,
            state.effect_filter_gpu,
            sort_mode,
        );
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .id_salt("browser_effects_scroll")
            .show(ui, |ui| {
                let grid_w = ui.available_width() - 8.0;
                let spacing = 6.0;
                let card_w = ((grid_w - spacing) / 2.0).floor();
                let preview_h = (card_w * 9.0 / 16.0).round();
                let card_h = preview_h + 36.0; // preview + name + category subtitle

                // Favourite toggles are collected into this local during the
                // card loop and applied afterwards — same snapshot-then-mutate
                // pattern as folder favourites (mod.rs:2086). Avoids a
                // borrow conflict between `favorite_effects.contains(...)`
                // in the loop body and the push/retain we'd otherwise do
                // in the same expression.
                let mut pending_fav_toggle: Option<String> = None;
                let render_grid = *view_mode == EffectViewMode::Grid;
                let list_row_h = 48.0;

                // Wrap the per-item loop in either an egui::Grid (grid
                // view) or a plain vertical flow (list view). The
                // post-render interaction code below is identical between
                // the two — only the render call and the favorite star
                // position differ.
                let render_items = |ui: &mut egui::Ui,
                                    pending_fav_toggle: &mut Option<String>,
                                    cmds: &mut Vec<AppCommand>,
                                    state: &mut AppState,
                                    recent_effects: &mut Vec<String>| {
                    for (i, fx) in filtered.iter().enumerate() {
                            // A handle, not a borrow of the state the hover
                            // preview below writes.
                            let thumb = state.effect_thumbnails.get_texture(&fx.name).cloned();
                            let thumb = thumb.as_ref();
                            let is_pending = state.effect_thumbnails.is_ofx_pending(&fx.name);
                            let supports_gpu = fx.supports_gpu(&state.available_plugins);
                            let temporal = fx.is_temporal(&state.available_plugins);
                            let resp = if render_grid {
                                effect_card(
                                    ui,
                                    theme,
                                    fx,
                                    card_w,
                                    card_h,
                                    preview_h,
                                    thumb,
                                    is_pending,
                                    supports_gpu,
                                    temporal,
                                )
                            } else {
                                effect_list_row(
                                    ui,
                                    theme,
                                    fx,
                                    list_row_h,
                                    thumb,
                                    is_pending,
                                    supports_gpu,
                                )
                            };

                            // ── Favourite star ──
                            // Grid: top-LEFT of thumb, hover-reveal with
                            // a dark scrim so it stays legible over any
                            // thumbnail content.
                            // List: right edge, vertically centered, no
                            // scrim — the row background already gives
                            // it contrast.
                            let is_fav = favorite_effects.iter().any(|n| n == &fx.name);
                            let card_hovered = resp.hovered();
                            // Shown on the clip while hovered (#3210).
                            let resp = match card_hovered.then(|| crate::app::hover_preview::want_effect(state, &fx.name, &fx.source, crate::app::hover_preview::across(&resp))).flatten().and_then(|key| crate::app::hover_preview::note_for(state, &key)) {
                                Some(note) => resp.on_hover_text(note),
                                None => resp,
                            };
                            let star_rect = if render_grid {
                                egui::Rect::from_min_size(
                                    egui::pos2(resp.rect.min.x + 6.0, resp.rect.min.y + 6.0),
                                    egui::vec2(22.0, 22.0),
                                )
                            } else {
                                egui::Rect::from_min_size(
                                    egui::pos2(
                                        resp.rect.max.x - 28.0,
                                        resp.rect.center().y - 11.0,
                                    ),
                                    egui::vec2(22.0, 22.0),
                                )
                            };
                            let star_resp = ui.interact(
                                star_rect,
                                ui.id().with(("fav_star", &fx.name)),
                                egui::Sense::click(),
                            );
                            let star_visible = is_fav || card_hovered || star_resp.hovered();
                            if star_visible {
                                if render_grid {
                                    let bg_alpha = if is_fav { 180 } else { 130 };
                                    ui.painter().rect_filled(
                                        star_rect,
                                        4.0,
                                        egui::Color32::from_rgba_unmultiplied(0, 0, 0, bg_alpha),
                                    );
                                }
                                let star_glyph = if is_fav { "\u{2605}" } else { "\u{2606}" };
                                let star_color = if is_fav || star_resp.hovered() {
                                    egui::Color32::from_rgb(255, 215, 106)
                                } else if render_grid {
                                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200)
                                } else {
                                    // List view: tint with the panel's
                                    // dim text colour so light themes
                                    // don't get a washed-out white star.
                                    theme.text_dim
                                };
                                ui.painter().text(
                                    star_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    star_glyph,
                                    egui::FontId::proportional(13.0),
                                    star_color,
                                );
                            }
                            let star_resp = star_resp.on_hover_text(if is_fav {
                                "Remove from favourites"
                            } else {
                                "Add to favourites"
                            });
                            if star_resp.clicked() {
                                *pending_fav_toggle = Some(fx.name.clone());
                            }

                            // Right-click menu on the card — same Add/Remove
                            // toggle entry as the star button for discoverability.
                            resp.context_menu_rows(|ui| {
                                let label = if is_fav {
                                    "Remove from Favourites"
                                } else {
                                    "Add to Favourites"
                                };
                                if ui.button(label).clicked() {
                                    *pending_fav_toggle = Some(fx.name.clone());
                                    ui.close_menu();
                                }
                            });

                            if resp.hovered() {
                                if let EffectSource::Ofx(ref plugin_id) = fx.source {
                                    if thumb.is_none() && !is_pending {
                                        if let Some(ref_frame) = &state.effect_registry.reference_frame {
                                            state.effect_thumbnails.request_ofx_render(
                                                &fx.name,
                                                plugin_id,
                                                ref_frame,
                                            );
                                        }
                                    }
                                }
                            }

                            if resp.double_clicked() {
                                // Every selected clip that can take it, as
                                // one undo step (#842).
                                let kind = match &fx.source {
                                    EffectSource::Audio(_) | EffectSource::Ladspa(_) | EffectSource::Lv2(_) | EffectSource::Clap(_) => crate::effect_targets::EffectKind::Sound,
                                    _ => crate::effect_targets::EffectKind::Picture,
                                };
                                let targets = crate::effect_targets::targets(state.sequence(), state.selected_clip, &state.selected_clips, None, kind);
                                if targets.clips.is_empty() {
                                    state.toast.show("Select a clip first".to_string());
                                } else {
                                    push_recent(recent_effects, &fx.name);
                                    let source = fx.source.clone();
                                    match crate::effect_targets::all_or_none(&targets, |clip_id| effect_commands_in(state, &source, clip_id)) {
                                        Ok(adds) => {
                                            cmds.extend(adds);
                                            state.toast.show(crate::effect_targets::said(&fx.name, &targets));
                                        }
                                        Err(why) => state.toast.show(format!("\"{}\" wasn't applied: {why}", fx.name)),
                                    }
                                }
                            }

                            if resp.drag_started() {
                                push_recent(recent_effects, &fx.name);
                                match &fx.source {
                                    EffectSource::Audio(type_id) => {
                                        state.drag_drop.begin(DragPayload::AudioEffect {
                                            name: fx.name.clone(),
                                            type_id: type_id.clone(),
                                        });
                                    }
                                    EffectSource::Ladspa(plugin) => {
                                        let type_id = format!(
                                            "ladspa:{}:{}",
                                            plugin.path.display(),
                                            plugin.label
                                        );
                                        state.drag_drop.begin(DragPayload::AudioEffect {
                                            name: fx.name.clone(),
                                            type_id,
                                        });
                                    }
                                    EffectSource::Lv2(plugin) => {
                                        // LV2 rides the audio-effect drop
                                        // target the same way LADSPA does;
                                        // type_id = "lv2:{uri}".
                                        state.drag_drop.begin(DragPayload::AudioEffect {
                                            name: fx.name.clone(),
                                            type_id: plugin.type_id(),
                                        });
                                    }
                                    // A LUT preset is put on by a click, as
                                    // the clip's look; it has no drop.
                                    EffectSource::LutLook(_) => {}
                                    EffectSource::Clap(plugin) => {
                                        // CLAP also rides the audio-effect
                                        // drop target. type_id = "clap:{plugin_id}".
                                        state.drag_drop.begin(DragPayload::AudioEffect {
                                            name: fx.name.clone(),
                                            type_id: plugin.type_id(),
                                        });
                                    }
                                    _ => {
                                        let (is_builtin, plugin_id) = match &fx.source {
                                            EffectSource::Builtin(_) | EffectSource::Look(_) => (true, String::new()),
                                            EffectSource::Ofx(id) => (false, id.clone()),
                                            // frei0r is a "builtin" effect from the
                                            // drop-target's POV (lands in
                                            // clip.builtin_effects()), but plugin_id
                                            // carries the type_id so the drop handler
                                            // can construct the Frei0r variant with
                                            // proper params.
                                            EffectSource::Frei0r(plugin) => {
                                                (true, plugin.type_id())
                                            }
                                            EffectSource::Audio(_)
                                            | EffectSource::Ladspa(_)
                                            | EffectSource::Lv2(_)
                                            | EffectSource::Clap(_)
                                            | EffectSource::LutLook(_) => {
                                                unreachable!()
                                            }
                                        };
                                        state.drag_drop.begin(DragPayload::Effect {
                                            name: fx.name.clone(),
                                            is_builtin,
                                            plugin_id,
                                        });
                                    }
                                }
                            }

                            // Grid view places two cards per row — list
                            // view is single-column so the end_row call
                            // is grid-only.
                            if render_grid && i % 2 == 1 {
                                ui.end_row();
                            }
                        }
                };

                if render_grid {
                    egui::Grid::new("effects_grid")
                        .num_columns(2)
                        .spacing([spacing, spacing])
                        .min_col_width(card_w)
                        .show(ui, |ui| {
                            render_items(ui, &mut pending_fav_toggle, &mut cmds, state, recent_effects);
                        });
                } else {
                    render_items(ui, &mut pending_fav_toggle, &mut cmds, state, recent_effects);
                }

                // Apply the deferred favourite toggle (star click or
                // right-click menu). One toggle per frame is enough — egui
                // only fires one click per repaint, so Option<String> is
                // sufficient.
                if let Some(name) = pending_fav_toggle {
                    if let Some(idx) = favorite_effects.iter().position(|n| n == &name) {
                        favorite_effects.remove(idx);
                    } else {
                        favorite_effects.push(name);
                    }
                }

                // Status bar at bottom
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("{count} items"))
                            .size(10.0)
                            .color(theme.text_dim),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("Drag to timeline to apply")
                                .size(10.0)
                                .color(theme.text_dim),
                        );
                    });
                });
            });
    });

    cmds
}

/// Cut a label to one line of `max_w`, with an ellipsis.
fn truncate_name(text: &str, max_w: f32, ui: &egui::Ui) -> String {
    let font = egui::FontId::proportional(crate::theme::type_scale::BODY);
    let fits = |t: &str| ui.painter().layout_no_wrap(t.to_string(), font.clone(), egui::Color32::WHITE).size().x <= max_w;
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut n = chars.len();
    while n > 1 {
        n -= 1;
        let candidate: String = chars[..n].iter().collect::<String>() + "\u{2026}";
        if fits(&candidate) {
            return candidate;
        }
    }
    "\u{2026}".to_string()
}

/// FCP-style effect card. Thumbnail + name below; the on-thumbnail
/// favorite star is overlaid by the caller (loop), but the GPU/CPU +
/// Source badge pair is painted in here so it's always tied to the
/// thumbnail rect.
fn effect_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    fx: &crate::browser::effect_registry::EffectEntry,
    width: f32,
    card_h: f32,
    preview_h: f32,
    thumbnail: Option<&egui::TextureHandle>,
    is_pending: bool,
    supports_gpu: bool,
    temporal: bool,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, card_h),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let card_rect = rect;
    let painter = ui.painter();

    // The card: the same field, border and hover as a media card.
    let rounding = egui::Rounding::same(crate::theme::radius::PANEL);
    painter.rect_filled(card_rect, rounding, theme.bg_panel_alt);
    let preview_rect = egui::Rect::from_min_size(card_rect.min, egui::vec2(width, preview_h));

    if let Some(tex) = thumbnail {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.rect_filled(preview_rect, rounding, egui::Color32::BLACK);
        painter.image(tex.id(), preview_rect, uv, egui::Color32::WHITE);
    } else {
        paint_effect_preview(painter, preview_rect.shrink(1.0), fx, theme);

        if is_pending {
            let center = preview_rect.center();
            let t = ui.input(|i| i.time) as f32;
            let angle = t * 4.0;
            for i in 0..8 {
                let a = angle + i as f32 * std::f32::consts::TAU / 8.0;
                let alpha = (255.0 * (1.0 - i as f32 / 8.0)) as u8;
                let dot = egui::pos2(center.x + a.cos() * 6.0, center.y + a.sin() * 6.0);
                painter.circle_filled(dot, 1.5, egui::Color32::from_white_alpha(alpha));
            }
            ui.ctx().request_repaint();
        }
    }

    let stroke_color = if hovered { theme.border_light } else { theme.border };
    painter.rect_stroke(card_rect, rounding, egui::Stroke::new(1.0_f32, stroke_color));

    // Faint centre divider on hover: a hint of the before/after split.
    if hovered {
        let mid_x = preview_rect.center().x;
        painter.line_segment(
            [
                egui::pos2(mid_x, preview_rect.min.y + 4.0),
                egui::pos2(mid_x, preview_rect.max.y - 4.0),
            ],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(255, 255, 255, 90)),
        );
    }

    // One mark on the picture: GPU, when the effect runs in real time.
    // The source is in the caption below.
    if supports_gpu {
        paint_thumb_badge(
            ui,
            theme,
            egui::pos2(card_rect.max.x - 6.0, card_rect.min.y + 6.0),
            "GPU",
            ThumbBadgeKind::Gpu,
        );
    }

    // ── Card metadata: Name (top), then "Category · Source" below ──
    let painter = ui.painter();
    let name_y = card_rect.min.y + preview_h + 6.0;
    let display_name = display_name_for(fx);
    let name_galley = painter.layout_no_wrap(
        truncate_name(&display_name, width - 12.0, ui),
        egui::FontId::proportional(crate::theme::type_scale::BODY),
        theme.text_primary,
    );
    painter.galley(
        egui::pos2(card_rect.min.x + 6.0, name_y),
        name_galley.clone(),
        theme.text_primary,
    );

    // Category · Source — single line. "Category · Source" format
    // makes provenance scannable at a glance. Source pill on the
    // thumbnail still owns the visual emphasis; this is just the
    // textual companion.
    let meta_y = name_y + name_galley.size().y + 2.0;
    let meta_text = format!(
        "{} \u{00B7} {}{}",
        category_label(fx),
        fx.source_label,
        if supports_gpu { "" } else { " \u{00B7} CPU" },
    );
    let meta_galley = painter.layout_no_wrap(
        truncate_name(&meta_text, width - 12.0, ui),
        egui::FontId::proportional(crate::theme::type_scale::CAPTION),
        theme.text_dim,
    );
    painter.galley(
        egui::pos2(card_rect.min.x + 6.0, meta_y),
        meta_galley,
        theme.text_dim,
    );

    // Tooltip with full info
    resp.clone().on_hover_ui(|ui| {
        ui.label(egui::RichText::new(&fx.name).strong().size(12.0));
        if !fx.grouping.is_empty() {
            ui.label(egui::RichText::new(&fx.grouping).size(10.0).color(theme.text_dim));
        }
        if !fx.description.is_empty() {
            ui.add_space(2.0);
            ui.label(egui::RichText::new(&fx.description).size(10.0).color(theme.text_secondary));
        }
        ui.label(
            egui::RichText::new(fx.source_label)
                .size(10.0)
                .color(theme.text_dim),
        );
        // Limitation notices in tooltip
        let name = &fx.name;
        let plugin_id = match &fx.source {
            crate::browser::effect_registry::EffectSource::Ofx(id) => id.as_str(),
            _ => "",
        };
        if plugin_id.contains("cimg") || plugin_id.contains("CImg") || plugin_id.contains("PIK")
            || plugin_id.contains("Keyer") || plugin_id.contains("IDistort")
            || plugin_id.contains("STMap") || plugin_id.contains("VectorToColor")
        {
            ui.label(egui::RichText::new("CPU only — adjust while paused, apply on export").size(10.0).color(theme.marker_warning));
        }
        // As the plug-in declared it (it reads frames at other times).
        if temporal {
            ui.label(egui::RichText::new("Render only — not real-time, applied on export").size(10.0).color(theme.marker_error));
        }
        if plugin_id.contains("anaglyph") || plugin_id.contains("joinView") || plugin_id.contains("mixView")
            || plugin_id.contains("oneView") || plugin_id.contains("sideBySide")
            || name.contains("Anaglyph") || name.contains("Join Views") || name.contains("Mix Views")
            || name.contains("One View") || name.contains("Side By Side")
        {
            ui.label(egui::RichText::new("Requires multi-view — stereo 3D not supported yet").size(10.0).color(theme.marker_error));
        }
        ui.label(egui::RichText::new("Double-click to apply").size(10.0).color(theme.text_dim));
    });

    resp
}

/// Dense list-view row: small leading thumbnail, name + "Category · Source"
/// metadata stacked, GPU/CPU + Source badges anchored on the right with
/// a reserved gutter on the far right for the favorite star (drawn by
/// the caller). Same `Sense::click_and_drag` as `effect_card`, so the
/// caller's interaction tail (double-click apply, drag-to-timeline,
/// context menu, OFX thumbnail request) reuses the returned response.
fn effect_list_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    fx: &crate::browser::effect_registry::EffectEntry,
    row_h: f32,
    thumbnail: Option<&egui::TextureHandle>,
    is_pending: bool,
    supports_gpu: bool,
) -> egui::Response {
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click_and_drag(),
    );
    let hovered = resp.hovered();
    let painter = ui.painter();

    // Row background — subtle bg_raised on hover, clean separator line
    // along the bottom.
    if hovered {
        painter.rect_filled(rect, 4.0, theme.bg_raised);
    }
    painter.hline(
        rect.x_range(),
        rect.max.y - 0.5,
        egui::Stroke::new(1.0_f32, theme.border),
    );

    // Thumbnail at left — 16:9, padded 6px from row edges.
    let thumb_w = 64.0;
    let thumb_h = thumb_w * 9.0 / 16.0;
    let thumb_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + 6.0, rect.center().y - thumb_h * 0.5),
        egui::vec2(thumb_w, thumb_h),
    );
    let rounding = egui::Rounding::same(4.0);
    painter.rect_filled(thumb_rect, rounding, theme.bg_panel);
    painter.rect_stroke(thumb_rect, rounding, egui::Stroke::new(1.0_f32, theme.border));
    if let Some(tex) = thumbnail {
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(tex.id(), thumb_rect, uv, egui::Color32::WHITE);
    } else {
        paint_effect_preview(painter, thumb_rect.shrink(1.0), fx, theme);
        if is_pending {
            let center = thumb_rect.center();
            let t = ui.input(|i| i.time) as f32;
            for i in 0..6 {
                let a = t * 4.0 + i as f32 * std::f32::consts::TAU / 6.0;
                let alpha = (255.0 * (1.0 - i as f32 / 6.0)) as u8;
                let dot = egui::pos2(center.x + a.cos() * 4.0, center.y + a.sin() * 4.0);
                painter.circle_filled(dot, 1.0, egui::Color32::from_white_alpha(alpha));
            }
            ui.ctx().request_repaint();
        }
    }

    // Right-side: GPU/CPU + Source pills, right-aligned, with a
    // reserved gutter for the favorite star painted by the caller.
    let perf_label = if supports_gpu { "GPU" } else { "CPU" };
    let source_label = match &fx.source {
        EffectSource::Ofx(_) => "OFX",
        EffectSource::Builtin(_) | EffectSource::Audio(_) => "Built-in",
        EffectSource::Ladspa(_) => "LADSPA",
        EffectSource::Frei0r(_) => "frei0r",
        EffectSource::Lv2(_) => "LV2",
        EffectSource::Clap(_) => "CLAP",
        EffectSource::Look(_) => "Look",
        EffectSource::LutLook(_) => "LUT",
    };
    let star_reserve = 30.0;
    let badge_y = rect.center().y - 8.0;
    let src_rect = paint_thumb_badge(
        ui,
        theme,
        egui::pos2(rect.max.x - star_reserve, badge_y),
        source_label,
        ThumbBadgeKind::Source,
    );
    let perf_rect = paint_thumb_badge(
        ui,
        theme,
        egui::pos2(src_rect.min.x - 4.0, badge_y),
        perf_label,
        if supports_gpu { ThumbBadgeKind::Gpu } else { ThumbBadgeKind::Cpu },
    );

    // Text block between thumbnail and badges.
    let painter = ui.painter();
    let text_x = thumb_rect.max.x + 10.0;
    let text_max_x = perf_rect.min.x - 8.0;
    let text_w = (text_max_x - text_x).max(40.0);
    let display_name = display_name_for(fx);
    let name_galley = painter.layout(
        display_name,
        egui::FontId::proportional(12.5),
        theme.text_primary,
        text_w,
    );
    let meta_text = format!("{} \u{00B7} {}", display_category_for(fx), fx.source_label);
    let meta_galley = painter.layout(
        meta_text,
        egui::FontId::proportional(10.0),
        theme.text_dim,
        text_w,
    );
    let total_text_h = name_galley.size().y + meta_galley.size().y + 1.0;
    let name_y = rect.center().y - total_text_h * 0.5;
    painter.galley(
        egui::pos2(text_x, name_y),
        name_galley.clone(),
        theme.text_primary,
    );
    painter.galley(
        egui::pos2(text_x, name_y + name_galley.size().y + 1.0),
        meta_galley,
        theme.text_dim,
    );

    resp
}

fn category_color(cat: EffectCategory, theme: &KlippTheme) -> egui::Color32 {
    match cat {
        // Favourites / Recent / Installed are pseudo-categories for
        // filtering only; cards still paint using their real category
        // colour via `EffectEntry.category`. Use a generic dim tint here
        // as a harmless fallback.
        EffectCategory::Favourites
        | EffectCategory::Recent
        | EffectCategory::Installed => theme.text_dim,
        EffectCategory::All => theme.text_dim,
        EffectCategory::Blur => egui::Color32::from_rgb(0x4a, 0x9a, 0xde),
        EffectCategory::Color => theme.accent,
        EffectCategory::Stylize => egui::Color32::from_rgb(0xa0, 0x4a, 0xda),
        EffectCategory::Keying => theme.role_dialogue,
        EffectCategory::Distort => egui::Color32::from_rgb(0xe8, 0x50, 0x20),
        EffectCategory::Light => egui::Color32::from_rgb(0xf0, 0xd0, 0x60),
        EffectCategory::Film => egui::Color32::from_rgb(0xc8, 0x9a, 0x5a),
        EffectCategory::Looks | EffectCategory::Luts => theme.accent,
        EffectCategory::AudioAll => egui::Color32::from_rgb(0x60, 0xc0, 0x80),
        EffectCategory::AudioEQ => egui::Color32::from_rgb(0x2d, 0x6f, 0xa8),
        EffectCategory::AudioDynamics => egui::Color32::from_rgb(0x3a, 0x8c, 0x6e),
        EffectCategory::AudioReverb => egui::Color32::from_rgb(0x4a, 0x6e, 0xa8),
        EffectCategory::AudioDelay => egui::Color32::from_rgb(0x6a, 0x80, 0xc0),
        EffectCategory::AudioModulation => egui::Color32::from_rgb(0x9a, 0x6e, 0xc8),
        EffectCategory::AudioPitch => egui::Color32::from_rgb(0xc0, 0x5e, 0x9e),
        EffectCategory::AudioRestoration => egui::Color32::from_rgb(0xa9, 0xa3, 0x4e),
        EffectCategory::AudioUtility => egui::Color32::from_rgb(0x70, 0x80, 0x88),
    }
}

// ────────────────────────────── Sidebar widgets ──────────────────────────────

/// Single category row: leading icon (or reserved blank space when
/// `icon` is `None`), label, right-aligned count badge, full-row click
/// target, and a 2px accent stripe + accent_subtle fill when active.
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
    let row_h = 22.0;
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());

    let hovered = resp.hovered() || click_resp.hovered();
    let painter = ui.painter();

    // Active fill — translucent accent overlay matching the mockup's
    // `--accent-soft` recipe. Hover is a softer panel-raised tint so it
    // never competes with the active state visually.
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

    // Layout: indent · icon (13px) · gap · label · …badge
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

    // Label, vertically centered.
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

    // Right-aligned count, monospace tabular-nums for stable column edge.
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

/// Section header (e.g. "VIDEO FX") with leading icon. Renders above the
/// rows in that section. Visually quiet — uppercase, dim, small.
fn sidebar_section_header(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
) {
    let row_h = 18.0;
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

// ───────────────────────────── Source segmented ─────────────────────────────

/// Single coherent segmented control replacing the previous three
/// competing colored pills. Three buttons share an outer pill body; the
/// active segment paints with `bg_raised` to lift out of the bg_panel
/// trough. Embedded count badges in tabular monospace.
fn segmented_source_filter(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    filter: &mut SourceFilter,
    all_count: usize,
    builtin_count: usize,
    ofx_count: usize,
) {
    let outer_h = 24.0;
    let pad = 2.0;
    let segments = [
        (SourceFilter::Builtin, "Built-in", builtin_count),
        // "Plug-ins", not "Installed": this pill counts OFX and frei0r
        // plugins, while the sidebar row three centimetres away says
        // "Installed" and counts the whole registry. One word cannot
        // mean 102 things and 401 things on the same screen.
        (SourceFilter::Installed, "Plug-ins", ofx_count),
        (SourceFilter::All, "All", all_count),
    ];

    // Pre-measure each segment so total width is known before allocating.
    let mut seg_widths = [0.0f32; 3];
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
        let w = label_galley.size().x + 5.0 + count_galley.size().x + 18.0;
        seg_widths[i] = w;
        total_w += w + 1.0;
    }
    total_w -= 1.0; // no trailing gap

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
            ui.id().with(("source_seg", i)),
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

        let inner_w = label_galley.size().x + 5.0 + count_galley.size().x;
        let lx = seg_rect.center().x - inner_w * 0.5;
        ui.painter().galley(
            egui::pos2(lx, seg_rect.center().y - label_galley.size().y * 0.5),
            label_galley.clone(),
            label_color,
        );
        ui.painter().galley(
            egui::pos2(
                lx + label_galley.size().x + 5.0,
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

/// "GPU only" filter pill — shaped like the segmented buttons (24px tall,
/// rounded), but tinted with `accent_subtle` + accent border when active.
fn gpu_only_pill(ui: &mut egui::Ui, theme: &KlippTheme, active: &mut bool) {
    let label_galley = ui.painter().layout_no_wrap(
        "GPU only".to_string(),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    let icon_size = 11.0;
    let pad_x = 10.0;
    let gap = 5.0;
    let w = pad_x * 2.0 + icon_size + gap + label_galley.size().x;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(w, 24.0),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let (bg, border, fg) = if *active {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if hovered {
        (theme.bg_raised, theme.border, theme.text_primary)
    } else {
        (theme.bg_panel, theme.border, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, border));
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + pad_x, rect.center().y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    let img = Icon::Lightning.image(icon_size).tint(fg);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    ui.painter().text(
        egui::pos2(
            rect.min.x + pad_x + icon_size + gap,
            rect.center().y,
        ),
        egui::Align2::LEFT_CENTER,
        "GPU only",
        egui::FontId::proportional(11.0),
        fg,
    );
    if resp.clicked() {
        *active = !*active;
    }
}

/// Compact 24×24 icon button for the filter row. Active state mirrors
/// the `gpu_only_pill` — accent border + accent_subtle fill.
fn icon_filter_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Icon,
    is_active: bool,
    tooltip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(24.0, 24.0),
        egui::Sense::click(),
    );
    let hovered = resp.hovered();
    let (bg, border, fg) = if is_active {
        (theme.accent_subtle, theme.accent, theme.accent)
    } else if hovered {
        (theme.bg_raised, theme.border, theme.text_primary)
    } else {
        (theme.bg_panel, theme.border, theme.text_secondary)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, border));
    let size = 12.0;
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(size, size));
    let img = icon.image(size).tint(fg);
    ui.allocate_new_ui(
        egui::UiBuilder::new().max_rect(icon_rect),
        |ui| {
            ui.add(img);
        },
    );
    resp.on_hover_text(tooltip)
}

fn view_mode_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    current: &mut EffectViewMode,
    target: EffectViewMode,
    icon: Icon,
    tooltip: &str,
) {
    let resp = icon_filter_button(ui, theme, icon, *current == target, tooltip);
    if resp.clicked() {
        *current = target;
    }
}

fn hide_unsupported_button(ui: &mut egui::Ui, theme: &KlippTheme, active: &mut bool) {
    // EyeSlash when active (= currently hiding), Eye when inactive
    // (= currently showing all). The tooltip stays consistent so users
    // don't have to track the visual flip.
    let icon = if *active { Icon::EyeSlash } else { Icon::Eye };
    let resp = icon_filter_button(
        ui,
        theme,
        icon,
        *active,
        "Hide unsupported (stereo / render-only)",
    );
    if resp.clicked() {
        *active = !*active;
    }
}

/// Result strip below the filter row — live count of currently-visible
/// effects + an active-filter description (e.g. "GPU-supported only") +
/// a sort dropdown anchored to the right.
fn result_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    count: usize,
    cat: EffectCategory,
    gpu_only: bool,
    sort_mode: &mut EffectSort,
) {
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(avail_w, 22.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        rect.x_range(),
        rect.max.y,
        egui::Stroke::new(1.0_f32, theme.border),
    );

    // Left: count + scope description.
    let scope = match cat {
        EffectCategory::Favourites => "Favorites",
        EffectCategory::Recent => "Recent",
        EffectCategory::Installed => "Installed",
        EffectCategory::AudioAll
        | EffectCategory::AudioEQ
        | EffectCategory::AudioDynamics
        | EffectCategory::AudioReverb
        | EffectCategory::AudioDelay
        | EffectCategory::AudioModulation
        | EffectCategory::AudioPitch
        | EffectCategory::AudioRestoration
        | EffectCategory::AudioUtility => "Audio FX",
        _ => "Video FX",
    };
    let mut left = format!("{} {}", count, scope);
    if gpu_only {
        left.push_str(" \u{00B7} GPU-supported only");
    }
    ui.painter().text(
        egui::pos2(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        left,
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );

    // Right: sort dropdown — egui ComboBox anchored inside the strip.
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
            egui::ComboBox::from_id_salt("effect_sort_dd")
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

// ────────────────────────── Thumbnail badge widget ──────────────────────────

#[derive(Debug, Clone, Copy)]
enum ThumbBadgeKind {
    /// Green-tinted "GPU" pill — uses `marker_success`.
    Gpu,
    /// Neutral grey "CPU" pill.
    Cpu,
    /// Monochrome blur-style "OFX" / "Built-in" / "LADSPA" source pill.
    Source,
}

/// Paint a single on-thumbnail badge, anchored by its TOP-RIGHT corner.
/// Returns the painted rect so callers can stack badges left-to-right
/// from the right edge.
fn paint_thumb_badge(
    ui: &egui::Ui,
    theme: &KlippTheme,
    top_right: egui::Pos2,
    label: &str,
    kind: ThumbBadgeKind,
) -> egui::Rect {
    let painter = ui.painter();
    let font = egui::FontId::proportional(10.0);
    let label_color = match kind {
        ThumbBadgeKind::Gpu => theme.text_on_accent,
        ThumbBadgeKind::Cpu => egui::Color32::from_rgba_unmultiplied(255, 255, 255, 230),
        ThumbBadgeKind::Source => egui::Color32::from_rgba_unmultiplied(255, 255, 255, 230),
    };
    let galley = painter.layout_no_wrap(label.to_uppercase(), font, label_color);
    let pad_x = 6.0;
    let h = 16.0;
    let w = galley.size().x + pad_x * 2.0;
    let rect = egui::Rect::from_min_size(
        egui::pos2(top_right.x - w, top_right.y),
        egui::vec2(w, h),
    );
    let bg = match kind {
        ThumbBadgeKind::Gpu => egui::Color32::from_rgba_unmultiplied(
            theme.marker_success.r(),
            theme.marker_success.g(),
            theme.marker_success.b(),
            220,
        ),
        ThumbBadgeKind::Cpu => egui::Color32::from_rgba_unmultiplied(0, 0, 0, 165),
        ThumbBadgeKind::Source => egui::Color32::from_rgba_unmultiplied(0, 0, 0, 165),
    };
    painter.rect_filled(rect, 2.0, bg);
    // Hairline highlight to lift the badge off arbitrary thumbnail
    // backgrounds (mirrors the mockup's `border-color: rgba(255,255,255,.08)`).
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(255, 255, 255, 24),
        ),
    );
    let text_pos = egui::pos2(rect.min.x + pad_x, rect.center().y - galley.size().y * 0.5);
    painter.galley(text_pos, galley, label_color);
    rect
}

/// MRU push — insert/move `name` to position 0, dedupe, cap at the
/// short list size users care about.
fn push_recent(recent: &mut Vec<String>, name: &str) {
    const MAX_RECENT: usize = 12;
    if let Some(idx) = recent.iter().position(|n| n == name) {
        recent.remove(idx);
    }
    recent.insert(0, name.to_string());
    recent.truncate(MAX_RECENT);
}

// ────────────────────────── Display name cleanup ──────────────────────────

/// Polished display name for the card title. Drops the dead-weight
/// "Plugin" suffix and applies a small override map for the worst
/// auto-generated OFX names. Falls back to the entry's stored name
/// when no override hits.
pub fn display_name_for(fx: &crate::browser::effect_registry::EffectEntry) -> String {
    use crate::browser::effect_registry::EffectSource;
    if let EffectSource::Ofx(id) = &fx.source {
        if let Some(clean) = display_name_override(id.as_str()) {
            return clean.to_string();
        }
    }
    let name = fx.name.trim().trim_end_matches(" Plugin").trim();
    if name.is_empty() { fx.name.clone() } else { name.to_string() }
}

/// Category-as-text for the card metadata line. Uses the same string
/// the registry exposes today — `EffectEntry::grouping` — but falls
/// back to a category-derived label when grouping is empty.
pub fn display_category_for(fx: &crate::browser::effect_registry::EffectEntry) -> String {
    if !fx.grouping.is_empty() {
        return fx.grouping.clone();
    }
    category_label(fx).to_string()
}

/// The category alone, never the maker: what a card's caption leads with.
pub fn category_label(fx: &crate::browser::effect_registry::EffectEntry) -> &'static str {
    match fx.category {
        EffectCategory::Blur => "Blur",
        EffectCategory::Color => "Colour",
        EffectCategory::Stylize => "Stylize",
        EffectCategory::Keying => "Keying",
        EffectCategory::Distort => "Distort",
        EffectCategory::Light => "Light",
        EffectCategory::Film => "Film",
        EffectCategory::Looks => "Looks",
        EffectCategory::Luts => "LUT",
        EffectCategory::AudioEQ => "EQ",
        EffectCategory::AudioDynamics => "Dynamics",
        EffectCategory::AudioReverb => "Reverb",
        EffectCategory::AudioDelay => "Delay",
        EffectCategory::AudioModulation => "Modulation",
        EffectCategory::AudioPitch => "Pitch",
        EffectCategory::AudioRestoration => "Restoration",
        EffectCategory::AudioUtility => "Utility",
        _ => "Effect",
    }
}

/// Hand-curated cleanups for the worst auto-generated OFX names. The
/// mechanical `format_ofx_name` insertion of spaces between
/// CamelCase characters does fine for most plugins (`GaussianBlur` →
/// "Gaussian Blur") but trips on a handful of acronym-heavy or
/// numerical names. Keyed by the OFX plugin identifier.
fn display_name_override(id: &str) -> Option<&'static str> {
    // Match-arm form so the table is easy to scan and extend; trailing
    // wildcard returns None.
    match id {
        // RGB / acronym-heavy — the auto-formatter inserts a stray
        // space before "DPlugin"
        "net.sf.openfx.AdjustRoDPlugin" => Some("Adjust RGB"),
        "net.sf.openfx.adjustRoDPlugin" => Some("Adjust RGB"),
        // Numerals butted against words
        "net.sf.openfx.Card3D" => Some("Card 3D"),
        "net.sf.openfx.Position" => Some("Position 2D"),
        // Lowercase names that arrive title-cased
        "net.sf.openfx.anaglyphPlugin" => Some("Anaglyph"),
        "net.sf.openfx.deinterlace" => Some("Deinterlace"),
        "net.sf.openfx.SideBySidePlugin" => Some("Side By Side"),
        "net.sf.openfx.JoinViewsPlugin" => Some("Join Views"),
        "net.sf.openfx.MixViewsPlugin" => Some("Mix Views"),
        "net.sf.openfx.OneViewPlugin" => Some("One View"),
        // CImg* tier — strip the prefix, the source pill already says
        // OFX so the family is obvious from context.
        "net.sf.cimg.CImgBlur" => Some("Blur (CImg)"),
        "net.sf.cimg.CImgBloom" => Some("Bloom (CImg)"),
        "net.sf.cimg.CImgSharpen" => Some("Sharpen (CImg)"),
        _ => None,
    }
}

fn paint_effect_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    fx: &crate::browser::effect_registry::EffectEntry,
    theme: &KlippTheme,
) {
    let cat_color = category_color(fx.category, theme);
    let dim = cat_color.linear_multiply(0.3);

    match fx.category {
        EffectCategory::Blur => {
            for i in 0..5 {
                let y = rect.min.y + i as f32 * (rect.height() / 5.0);
                let alpha = 60 + i * 30;
                painter.rect_filled(
                    egui::Rect::from_min_size(
                        egui::pos2(rect.min.x, y),
                        egui::vec2(rect.width(), rect.height() / 5.0 - 1.0),
                    ),
                    0.0,
                    cat_color.linear_multiply(alpha as f32 / 255.0),
                );
            }
        }
        EffectCategory::Color => {
            painter.rect_filled(rect, 0.0, dim);
            let half = egui::Rect::from_min_size(
                egui::pos2(rect.center().x, rect.min.y),
                egui::vec2(rect.width() / 2.0, rect.height()),
            );
            painter.rect_filled(half, 0.0, cat_color.linear_multiply(0.2));
        }
        EffectCategory::Keying => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x0a, 0x30, 0x0a));
        }
        EffectCategory::Film | EffectCategory::Looks | EffectCategory::Luts => {
            painter.rect_filled(rect, 0.0, dim);
        }
        EffectCategory::Light => {
            painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(0x10, 0x10, 0x10));
            painter.circle_filled(
                rect.center(),
                rect.height() * 0.3,
                cat_color.linear_multiply(0.4),
            );
        }
        EffectCategory::Distort => {
            painter.rect_filled(rect, 0.0, dim);
            for i in 0..6 {
                let x = rect.min.x + (i as f32 / 5.0) * rect.width();
                painter.line_segment(
                    [egui::pos2(x, rect.min.y), egui::pos2(x + 10.0, rect.max.y)],
                    egui::Stroke::new(1.0_f32, cat_color.linear_multiply(0.3)),
                );
            }
        }
        EffectCategory::AudioEQ | EffectCategory::AudioDynamics | EffectCategory::AudioReverb | EffectCategory::AudioAll => {
            let bg = egui::Color32::from_rgb(0x12, 0x18, 0x14);
            painter.rect_filled(rect, 0.0, bg);

            let type_id = match &fx.source {
                crate::browser::effect_registry::EffectSource::Audio(id) => id.as_str(),
                _ => "",
            };
            let cx = rect.center().x;
            let cy = rect.center().y;
            let w = rect.width();
            let h = rect.height();
            let accent = cat_color.linear_multiply(0.7);
            let dim_line = cat_color.linear_multiply(0.25);

            match type_id {
                "eq" => {
                    // Parametric EQ: frequency response curve
                    let n = 40;
                    let points: Vec<egui::Pos2> = (0..=n).map(|i| {
                        let t = i as f32 / n as f32;
                        let x = rect.min.x + t * w;
                        let freq = t * 3.0 * std::f32::consts::PI;
                        let y = cy - (freq.sin() * 0.15 + (freq * 2.5).sin() * 0.1 + (freq * 0.4).cos() * 0.08) * h;
                        egui::pos2(x, y)
                    }).collect();
                    // Grid lines
                    for i in 1..4 {
                        let y = rect.min.y + i as f32 * h / 4.0;
                        painter.line_segment([egui::pos2(rect.min.x, y), egui::pos2(rect.max.x, y)], egui::Stroke::new(0.5_f32, dim_line));
                    }
                    // Curve
                    for pair in points.windows(2) {
                        painter.line_segment([pair[0], pair[1]], egui::Stroke::new(2.0_f32, accent));
                    }
                }
                "compressor" => {
                    // Compressor: knee curve (input vs output)
                    let margin = 8.0;
                    let plot = rect.shrink(margin);
                    // Diagonal reference line
                    painter.line_segment([plot.min, plot.max], egui::Stroke::new(0.5_f32, dim_line));
                    // Compression knee curve
                    let n = 30;
                    let points: Vec<egui::Pos2> = (0..=n).map(|i| {
                        let t = i as f32 / n as f32;
                        let input = t;
                        let threshold = 0.5;
                        let ratio = 0.3;
                        let output = if input < threshold { input } else { threshold + (input - threshold) * ratio };
                        egui::pos2(plot.min.x + t * plot.width(), plot.max.y - output * plot.height())
                    }).collect();
                    for pair in points.windows(2) {
                        painter.line_segment([pair[0], pair[1]], egui::Stroke::new(2.0_f32, accent));
                    }
                    // Threshold marker
                    let tx = plot.min.x + 0.5 * plot.width();
                    painter.line_segment([egui::pos2(tx, plot.min.y), egui::pos2(tx, plot.max.y)], egui::Stroke::new(0.5_f32, egui::Color32::from_rgba_unmultiplied(255, 180, 60, 80)));
                }
                "limiter" => {
                    // Limiter: hard ceiling line with clipped waveform
                    let ceiling_y = rect.min.y + h * 0.3;
                    painter.line_segment([egui::pos2(rect.min.x, ceiling_y), egui::pos2(rect.max.x, ceiling_y)], egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(220, 80, 60)));
                    // Waveform hitting the ceiling
                    let n = 50;
                    for i in 0..n {
                        let t = i as f32 / n as f32;
                        let x = rect.min.x + t * w;
                        let amp = (t * 8.0 * std::f32::consts::PI).sin() * (0.3 + t * 0.5);
                        let bar_h = (amp.abs() * h * 0.45).min(cy - ceiling_y);
                        painter.line_segment([egui::pos2(x, cy + bar_h), egui::pos2(x, cy - bar_h)], egui::Stroke::new(1.5_f32, accent));
                    }
                }
                "gate" => {
                    // Noise Gate: waveform with gaps (gated silence)
                    let n = 60;
                    for i in 0..n {
                        let t = i as f32 / n as f32;
                        let x = rect.min.x + t * w;
                        let amp = (t * 12.0 * std::f32::consts::PI).sin() * ((t * 3.0).sin().abs() * 0.8 + 0.1);
                        let gated = amp.abs() > 0.35;
                        if gated {
                            painter.line_segment([egui::pos2(x, cy - amp * h * 0.35), egui::pos2(x, cy + amp * h * 0.35)], egui::Stroke::new(1.5_f32, accent));
                        } else {
                            painter.line_segment([egui::pos2(x, cy - 0.5), egui::pos2(x, cy + 0.5)], egui::Stroke::new(0.5_f32, dim_line));
                        }
                    }
                }
                "reverb" => {
                    // Reverb: impulse with exponential decay tail
                    let n = 60;
                    for i in 0..n {
                        let t = i as f32 / n as f32;
                        let x = rect.min.x + t * w;
                        let envelope = if t < 0.05 { t / 0.05 } else { (-3.0 * (t - 0.05)).exp() };
                        let noise = ((t * 80.0).sin() * (t * 130.0).cos()) * envelope;
                        let bar_h = noise.abs() * h * 0.4;
                        let alpha = (envelope * 200.0).min(255.0) as u8;
                        let color = egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha);
                        painter.line_segment([egui::pos2(x, cy - bar_h), egui::pos2(x, cy + bar_h)], egui::Stroke::new(1.5_f32, color));
                    }
                }
                "delay" => {
                    // Delay: repeated diminishing impulses
                    for echo in 0..4 {
                        let offset = echo as f32 * 0.22;
                        let amp = 0.8_f32.powi(echo);
                        let alpha = (amp * 220.0) as u8;
                        let color = egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha);
                        let ex = rect.min.x + (0.1 + offset) * w;
                        let bar_w = w * 0.08;
                        for i in 0..8 {
                            let t = i as f32 / 7.0;
                            let x = ex + t * bar_w;
                            let wave = (t * 4.0 * std::f32::consts::PI).sin() * amp * h * 0.3;
                            painter.line_segment([egui::pos2(x, cy - wave), egui::pos2(x, cy + wave)], egui::Stroke::new(1.5_f32, color));
                        }
                    }
                }
                "chorus" => {
                    // Chorus: multiple overlapping slightly offset waves
                    for layer in 0..3 {
                        let phase = layer as f32 * 0.8;
                        let offset_y = (layer as f32 - 1.0) * 3.0;
                        let alpha = 180 - layer * 40;
                        let color = egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha as u8);
                        let n = 40;
                        let points: Vec<egui::Pos2> = (0..=n).map(|i| {
                            let t = i as f32 / n as f32;
                            let x = rect.min.x + t * w;
                            let y = cy + offset_y + (t * 6.0 * std::f32::consts::PI + phase).sin() * h * 0.2;
                            egui::pos2(x, y)
                        }).collect();
                        for pair in points.windows(2) {
                            painter.line_segment([pair[0], pair[1]], egui::Stroke::new(1.5_f32, color));
                        }
                    }
                }
                "deesser" => {
                    // De-Esser: waveform with high-freq notch
                    let n = 50;
                    let points: Vec<egui::Pos2> = (0..=n).map(|i| {
                        let t = i as f32 / n as f32;
                        let x = rect.min.x + t * w;
                        let base = (t * 5.0 * std::f32::consts::PI).sin() * 0.3;
                        let hf = (t * 25.0 * std::f32::consts::PI).sin() * 0.15;
                        let suppress = if t > 0.4 && t < 0.7 { 0.2 } else { 1.0 };
                        let y = cy - (base + hf * suppress) * h * 0.35;
                        egui::pos2(x, y)
                    }).collect();
                    for pair in points.windows(2) {
                        painter.line_segment([pair[0], pair[1]], egui::Stroke::new(1.5_f32, accent));
                    }
                    // Suppression zone
                    let zone = egui::Rect::from_min_max(
                        egui::pos2(rect.min.x + 0.4 * w, rect.min.y),
                        egui::pos2(rect.min.x + 0.7 * w, rect.max.y),
                    );
                    painter.rect_filled(zone, 0.0, egui::Color32::from_rgba_unmultiplied(220, 80, 60, 20));
                }
                "normalize" => {
                    // Normalize: small waveform scaled up to fill
                    let n = 50;
                    // Before (small)
                    for i in 0..n/2 {
                        let t = i as f32 / (n as f32 / 2.0);
                        let x = rect.min.x + t * w * 0.45;
                        let amp = (t * 10.0 * std::f32::consts::PI).sin() * 0.15;
                        painter.line_segment([egui::pos2(x, cy - amp * h), egui::pos2(x, cy + amp * h)], egui::Stroke::new(1.0_f32, dim_line.linear_multiply(1.5)));
                    }
                    // Arrow
                    painter.line_segment([egui::pos2(cx - 4.0, cy), egui::pos2(cx + 4.0, cy)], egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(200, 200, 200)));
                    painter.line_segment([egui::pos2(cx + 1.0, cy - 3.0), egui::pos2(cx + 4.0, cy)], egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(200, 200, 200)));
                    painter.line_segment([egui::pos2(cx + 1.0, cy + 3.0), egui::pos2(cx + 4.0, cy)], egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(200, 200, 200)));
                    // After (full)
                    for i in 0..n/2 {
                        let t = i as f32 / (n as f32 / 2.0);
                        let x = rect.min.x + 0.55 * w + t * w * 0.45;
                        let amp = (t * 10.0 * std::f32::consts::PI).sin() * 0.4;
                        painter.line_segment([egui::pos2(x, cy - amp * h), egui::pos2(x, cy + amp * h)], egui::Stroke::new(1.5_f32, accent));
                    }
                }
                "pitch_shift" => {
                    // Pitch Shift: sine wave morphing to different frequency
                    let n = 40;
                    let points: Vec<egui::Pos2> = (0..=n).map(|i| {
                        let t = i as f32 / n as f32;
                        let x = rect.min.x + t * w;
                        let freq = 4.0 + t * 4.0; // increasing frequency
                        let y = cy - (t * freq * std::f32::consts::PI).sin() * h * 0.3;
                        egui::pos2(x, y)
                    }).collect();
                    for pair in points.windows(2) {
                        painter.line_segment([pair[0], pair[1]], egui::Stroke::new(2.0_f32, accent));
                    }
                }
                "noise_reduction" | "declick" => {
                    // Noise Reduction / De-click: noisy waveform → clean waveform
                    let n = 50;
                    // Noisy (left half)
                    for i in 0..n/2 {
                        let t = i as f32 / (n as f32 / 2.0);
                        let x = rect.min.x + t * w * 0.45;
                        let signal = (t * 8.0 * std::f32::consts::PI).sin() * 0.25;
                        let noise = ((t * 50.0).sin() * (t * 73.0).cos()) * 0.12;
                        painter.line_segment([egui::pos2(x, cy - (signal + noise) * h), egui::pos2(x, cy + (signal + noise).abs() * h * 0.3)], egui::Stroke::new(1.0_f32, dim_line.linear_multiply(1.5)));
                    }
                    // Arrow
                    painter.line_segment([egui::pos2(cx - 4.0, cy), egui::pos2(cx + 4.0, cy)], egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(200, 200, 200)));
                    // Clean (right half)
                    for i in 0..n/2 {
                        let t = i as f32 / (n as f32 / 2.0);
                        let x = rect.min.x + 0.55 * w + t * w * 0.45;
                        let signal = (t * 8.0 * std::f32::consts::PI).sin() * 0.25;
                        painter.line_segment([egui::pos2(x, cy - signal * h), egui::pos2(x, cy + signal.abs() * h * 0.3)], egui::Stroke::new(1.5_f32, accent));
                    }
                }
                _ => {
                    // Generic audio: simple waveform
                    let n = 40;
                    for i in 0..n {
                        let t = i as f32 / n as f32;
                        let x = rect.min.x + t * w;
                        let amp = (t * 8.0 * std::f32::consts::PI).sin() * 0.3;
                        painter.line_segment([egui::pos2(x, cy - amp * h), egui::pos2(x, cy + amp * h)], egui::Stroke::new(1.5_f32, accent));
                    }
                }
            }
        }
        _ => {
            painter.rect_filled(rect, 0.0, dim);
        }
    }
}

/// The picture effects `source` puts on a clip, with their own values: a
/// built-in effect, a look's chain, a frei0r plugin at its declared
/// defaults. `None` for a plug-in or sound effect, added by its type.
pub(crate) fn picture_effects(source: &EffectSource) -> Option<Vec<klipp_effects::BuiltinEffect>> {
    match source {
        EffectSource::Builtin(effect) => Some(vec![effect.clone()]),
        EffectSource::Look(effects) => Some(effects.clone()),
        EffectSource::Frei0r(plugin) => {
            let params: Vec<klipp_frei0r::Frei0rValue> = plugin.params.iter().map(|p| klipp_frei0r::Frei0rValue::default_for(p.kind)).collect();
            Some(vec![klipp_effects::BuiltinEffect::Frei0r { type_id: plugin.type_id(), params }])
        }
        _ => None,
    }
}

/// [`picture_effects`] as `state`'s sequence takes them, from the browser
/// or a drop: starting values are authored at 1080p, and their pixel
/// sizes go on at the sequence's height over 1080, so a 4K sequence's
/// blur reads as the 1080p one does (#3243).
pub(crate) fn starting_effects(state: &AppState, source: &EffectSource) -> Option<Vec<klipp_effects::BuiltinEffect>> {
    let k = state.sequence().resolution.height.max(1) as f32 / 1080.0;
    Some(picture_effects(source)?.into_iter().map(|e| e.at_render_scale(k)).collect())
}

/// The commands that put `source` on `clip_id`: its picture effects (a
/// built-in effect, a look's every effect, a frei0r plug-in at its declared
/// defaults) by path, after the clip's stack (M3 steps 14 and 19), or
/// refused with the clip's reason; an OFX plug-in or a sound effect by its
/// type.
pub(crate) fn effect_commands_in(state: &AppState, source: &EffectSource, clip_id: klipp_core::ClipId) -> Result<Vec<AppCommand>, String> {
    Ok(match source {
        EffectSource::Builtin(_) | EffectSource::Look(_) | EffectSource::Frei0r(_) => {
            let effects = starting_effects(state, source).unwrap_or_default();
            let clip = state.sequence().tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == clip_id).ok_or("the clip isn't in the sequence")?;
            return crate::inspector::stack_view::add_effects_command(state, clip, &effects);
        }
        EffectSource::LutLook(path) => vec![crate::browser::effect_registry::lut_command(clip_id, path)],
        EffectSource::Ofx(plugin_name) => vec![AppCommand::AddEffect { clip_id, plugin_name: plugin_name.clone() }],
        EffectSource::Audio(type_id) => vec![AppCommand::AddAudioEffect { clip_id, effect_type: type_id.clone() }],
        EffectSource::Ladspa(plugin) => vec![AppCommand::AddAudioEffect { clip_id, effect_type: format!("ladspa:{}:{}", plugin.path.display(), plugin.label) }],
        // LV2 and CLAP ride the same audio pipeline as LADSPA.
        EffectSource::Lv2(plugin) => vec![AppCommand::AddAudioEffect { clip_id, effect_type: plugin.type_id() }],
        EffectSource::Clap(plugin) => vec![AppCommand::AddAudioEffect { clip_id, effect_type: plugin.type_id() }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browser::effect_registry::{EffectEntry, EffectSource};

    /// #3243: a start authored at 1080p goes on a 4K sequence at twice
    /// its pixel sizes, so the blur reads as it does at 1080p; a ratio is
    /// left as it is.
    #[test]
    fn starting_values_go_on_at_the_sequences_size() {
        let mut state = AppState::default();
        let blur = EffectSource::Builtin(klipp_effects::BuiltinEffect::GaussianBlur { radius: 12.0 });
        let sharpen = EffectSource::Builtin(klipp_effects::BuiltinEffect::Sharpen { amount: 0.6 });
        state.sequence_mut().resolution = klipp_core::Resolution { width: 1920, height: 1080 };
        assert!(matches!(starting_effects(&state, &blur).unwrap()[..], [klipp_effects::BuiltinEffect::GaussianBlur { radius }] if radius == 12.0));
        state.sequence_mut().resolution = klipp_core::Resolution { width: 3840, height: 2160 };
        assert!(matches!(starting_effects(&state, &blur).unwrap()[..], [klipp_effects::BuiltinEffect::GaussianBlur { radius }] if radius == 24.0));
        assert!(matches!(starting_effects(&state, &sharpen).unwrap()[..], [klipp_effects::BuiltinEffect::Sharpen { amount }] if amount == 0.6));
    }

    fn make_ofx_entry(id: &str, name: &str) -> EffectEntry {
        EffectEntry {
            name: name.into(),
            category: EffectCategory::Color,
            source: EffectSource::Ofx(id.into()),
            source_label: "OFX",
            grouping: "Colour".into(),
            description: String::new(),
        }
    }

    #[test]
    fn display_name_strips_plugin_suffix() {
        let fx = make_ofx_entry("net.sf.openfx.Saturation", "Saturation Plugin");
        assert_eq!(display_name_for(&fx), "Saturation");
    }

    #[test]
    fn display_name_override_map_wins_over_stored_name() {
        let fx = make_ofx_entry("net.sf.openfx.AdjustRoDPlugin", "Adjust Ro DPlugin");
        assert_eq!(display_name_for(&fx), "Adjust RGB");
    }

    #[test]
    fn display_name_override_handles_card3d() {
        let fx = make_ofx_entry("net.sf.openfx.Card3D", "Card 3 D");
        assert_eq!(display_name_for(&fx), "Card 3D");
    }

    #[test]
    fn display_name_falls_back_when_no_override() {
        let fx = make_ofx_entry("net.sf.openfx.GaussianBlur", "Gaussian Blur");
        assert_eq!(display_name_for(&fx), "Gaussian Blur");
    }

    #[test]
    fn push_recent_dedupes_and_caps() {
        let mut r: Vec<String> = Vec::new();
        for i in 0..20 {
            push_recent(&mut r, &format!("fx_{}", i));
        }
        assert_eq!(r.len(), 12, "MRU list capped at 12");
        assert_eq!(r[0], "fx_19", "most-recent at the front");

        // Re-pushing an existing entry moves it to the front, doesn't dupe.
        push_recent(&mut r, "fx_15");
        assert_eq!(r[0], "fx_15");
        assert_eq!(r.iter().filter(|n| n.as_str() == "fx_15").count(), 1);
    }
}
