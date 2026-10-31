use crate::widgets::menu::MenuRows;
use crate::browser::generator_previews::GeneratorPreviews;
use crate::browser::effects_tab::{EffectSort, EffectViewMode};
use crate::drag_drop::DragPayload;
use crate::browser::library_card::LibraryList;
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;


// The curated set, one generator per job, is Klipp's file (#2887):
// `assets/presets/generators/builtin.toml`.
type GeneratorTemplate = klipp_core::generator_entries::GeneratorEntry;

fn generators() -> &'static [GeneratorTemplate] {
    klipp_core::generator_entries::generator_entries()
}

const GENERATORS_SIDEBAR_W: f32 = 180.0;

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &mut String,
    active_category: &mut Option<&'static str>,
    list: &mut Option<LibraryList>,
    favourites: &mut Vec<String>,
    animated_only: &mut bool,
    view_mode: &mut EffectViewMode,
    sort_mode: &mut EffectSort,
    selected: &mut Option<String>,
    previews: &mut GeneratorPreviews,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let avail = ui.available_rect_before_wrap();
    // By name; a snapshot, since the cards take `state`.
    let recent = state.library_recent.generators.clone();

    // ── Sidebar (left) | content (right) ──
    let sidebar_w = GENERATORS_SIDEBAR_W.min(avail.width() * 0.40);
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
            .id_salt("generators_sidebar_scroll")
            .show(ui, |ui| {
                ui.add_space(6.0);

                let installed_total = generators().len();
                let in_list = |keys: &[String]| generators().iter().filter(|t| keys.iter().any(|k| k == t.name.as_str())).count();
                for (target, icon, label, count) in [
                    (LibraryList::Favourites, Icon::Star, "Favorites", in_list(favourites)),
                    (LibraryList::Recent, Icon::ArrowCounterClockwise, "Recent", in_list(&recent)),
                ] {
                    if gen_general_row(ui, theme, icon, label, count, *list == Some(target), &format!("generators_general_{label}")) {
                        *list = Some(target);
                        *active_category = None;
                    }
                }

                ui.add_space(8.0);
                gen_section_separator(ui, theme);
                gen_section_header(ui, theme, Some(Icon::Star), "Generators");

                gen_sidebar_row(
                    ui, theme, Some(Icon::List), "All",
                    installed_total, active_category.is_none() && list.is_none(),
                    "generators_cat_all",
                    &mut |opt| {
                        *active_category = opt;
                        *list = None;
                    },
                    None,
                );
                for cat in &klipp_core::generator_entries::generator_categories() {
                    let count = generators().iter().filter(|t| t.category == *cat).count();
                    let is_active = *active_category == Some(*cat) && list.is_none();
                    gen_sidebar_row(
                        ui, theme, Some(category_icon(cat)), cat,
                        count, is_active,
                        &format!("generators_cat_{cat}"),
                        &mut |opt| {
                            *active_category = opt;
                            *list = None;
                        },
                        Some(*cat),
                    );
                }
                ui.add_space(8.0);
            });
    });

    // ── Right pane ──────────────────────────────────────────────────
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(content_rect), |ui| {
        ui.set_min_size(content_rect.size());

        ui.add_space(8.0);

        // Search bar
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                super::browser_search_field(ui, theme, search_query, "Search generators\u{2026}", state);
            });
        ui.add_space(6.0);

        // Filter row: Animated only + view toggle. Every generator is
        // built in, so there is no source to filter by.
        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    animated_only_pill(ui, theme, animated_only);
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            view_mode_button(ui, theme, view_mode, EffectViewMode::List, Icon::List, "List view");
                            view_mode_button(ui, theme, view_mode, EffectViewMode::Grid, Icon::GridFour, "Grid view");
                        },
                    );
                });
            });

        ui.add_space(8.0);

        // Filter the registry
        let search_lower = search_query.to_lowercase();
        let mut filtered: Vec<&GeneratorTemplate> = generators()
            .iter()
            .filter(|t| match active_category {
                Some(cat) => t.category == *cat,
                None => true,
            })
            .filter(|t| match list {
                Some(LibraryList::Favourites) => favourites.iter().any(|k| k == t.name.as_str()),
                Some(LibraryList::Recent) => recent.iter().any(|k| k == t.name.as_str()),
                None => true,
            })
            .filter(|t| !*animated_only || t.animated)
            .filter(|t| {
                if search_lower.is_empty() {
                    return true;
                }
                t.name.as_str().to_lowercase().contains(&search_lower)
                    || t.category.to_lowercase().contains(&search_lower)
            })
            .collect();
        match *sort_mode {
            EffectSort::NameAsc => filtered.sort_by_key(|a| a.name.to_lowercase()),
            EffectSort::NameDesc => filtered.sort_by_key(|a| std::cmp::Reverse(a.name.to_lowercase())),
            EffectSort::Category => filtered.sort_by(|a, b| {
                a.category.to_lowercase().cmp(&b.category.to_lowercase())
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }),
            EffectSort::Source => filtered.sort_by_key(|a| a.name.to_lowercase()),
        }
        // Recent reads newest first, whatever the sort.
        if *list == Some(LibraryList::Recent) {
            filtered.sort_by_key(|t| recent.iter().position(|k| k == t.name.as_str()));
        }

        // Result strip: count + sort dropdown
        gen_result_strip(ui, theme, filtered.len(), sort_mode);
        ui.add_space(4.0);

        // Card grid / list
        egui::ScrollArea::vertical()
            .id_salt("generators_grid_scroll")
            .show(ui, |ui| {
                if filtered.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        let msg = if !search_query.is_empty() {
                            format!("No generators match \"{}\"", search_query)
                        } else if *list == Some(LibraryList::Favourites) {
                            "No favourites yet. Star a generator to keep it here".to_string()
                        } else if *list == Some(LibraryList::Recent) {
                            "Generators you add to the timeline show here".to_string()
                        } else {
                            "No generators in this category".to_string()
                        };
                        ui.label(egui::RichText::new(msg).size(11.0).color(theme.text_dim));
                    });
                    return;
                }

                let avail_w = ui.available_width() - 8.0;
                let spacing = 6.0;
                let render_grid = *view_mode == EffectViewMode::Grid;

                if render_grid {
                    let card_w = ((avail_w - spacing) / 2.0).floor();
                    let card_h = (card_w * 9.0 / 16.0).round().max(72.0) + crate::browser::library_card::CAPTION_H;
                    egui::Grid::new("generators_grid")
                        .num_columns(2)
                        .spacing([spacing, spacing])
                        .min_col_width(card_w)
                        .show(ui, |ui| {
                            for (i, tmpl) in filtered.iter().enumerate() {
                                let is_selected = selected.as_deref() == Some(tmpl.name.as_str());
                                let tex = preview_texture(ui, previews, tmpl);
                                let resp = generator_card(ui, theme, tmpl, card_w, card_h, is_selected, tex.as_ref());
                                let active = resp.hovered() || is_selected;
                                gen_update_anim_state(ui, tmpl.name.as_str(), active && tmpl.animated);
                                if active && tmpl.animated {
                                    ui.ctx().request_repaint();
                                }
                                let is_fav = favourites.iter().any(|k| k == tmpl.name.as_str());
                                let mut toggle = crate::browser::library_card::favourite_star(ui, theme, &resp, true, is_fav, tmpl.name.as_str());
                                resp.context_menu_rows(|ui| toggle |= crate::browser::library_card::favourite_menu_item(ui, is_fav));
                                if toggle {
                                    crate::browser::library_card::toggle_favourite(favourites, tmpl.name.as_str());
                                }
                                handle_card_interaction(state, &mut cmds, selected, tmpl, &resp);
                                if i % 2 == 1 {
                                    ui.end_row();
                                }
                            }
                            if filtered.len() % 2 == 1 {
                                ui.end_row();
                            }
                        });
                } else {
                    for tmpl in filtered.iter() {
                        let is_selected = selected.as_deref() == Some(tmpl.name.as_str());
                        let tex = preview_texture(ui, previews, tmpl);
                        let resp = generator_list_row(ui, theme, tmpl, avail_w, is_selected, tex.as_ref());
                        let active = resp.hovered() || is_selected;
                        gen_update_anim_state(ui, tmpl.name.as_str(), active && tmpl.animated);
                        if active && tmpl.animated {
                            ui.ctx().request_repaint();
                        }
                        let is_fav = favourites.iter().any(|k| k == tmpl.name.as_str());
                        let mut toggle = crate::browser::library_card::favourite_star(ui, theme, &resp, false, is_fav, tmpl.name.as_str());
                        resp.context_menu_rows(|ui| toggle |= crate::browser::library_card::favourite_menu_item(ui, is_fav));
                        if toggle {
                            crate::browser::library_card::toggle_favourite(favourites, tmpl.name.as_str());
                        }
                        handle_card_interaction(state, &mut cmds, selected, tmpl, &resp);
                        ui.add_space(2.0);
                    }
                }
            });
    });

    cmds
}

fn handle_card_interaction(
    state: &mut AppState,
    cmds: &mut Vec<AppCommand>,
    selected: &mut Option<String>,
    tmpl: &GeneratorTemplate,
    resp: &egui::Response,
) {
    // Shown where a double-click would put it while hovered (#3210).
    if resp.hovered() {
        let place = crate::app::hover_preview::Place::Generator(tmpl.data.clone());
        crate::app::hover_preview::want_placed(state, &format!("generator:{}", tmpl.name.as_str()), place, crate::app::hover_preview::across(resp));
    }
    if resp.clicked() {
        *selected = Some(tmpl.name.as_str().to_string());
    }
    if resp.double_clicked() {
        crate::browser::library_card::push_recent(&mut state.library_recent.generators, tmpl.name.as_str());
        cmds.push(AppCommand::CreateGeneratorClip {
            track_index: None,
            data: tmpl.data.clone(),
        });
        state.toast.show(format!("\"{}\" generator added to timeline", tmpl.name.as_str()));
    }
    if resp.drag_started() {
        state.drag_drop.begin(DragPayload::Generator {
            name: tmpl.name.as_str().to_string(),
            data: tmpl.data.clone(),
        });
    }
}

fn generator_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    tmpl: &GeneratorTemplate,
    width: f32,
    height: f32,
    is_selected: bool,
    preview: Option<&egui::TextureHandle>,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, height),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let caption = format!(
        "{} \u{00B7} Built-in{}",
        tmpl.category,
        if tmpl.animated { " \u{00B7} Animated" } else { "" },
    );
    crate::browser::library_card::library_card(ui, rect, tmpl.name.as_str(), &caption, is_selected, hovered, theme, |p, area| {
        paint_preview(p, area, preview, theme);
    });

    resp
}

/// The rendered frame, or a quiet field while it renders.
fn paint_preview(p: &egui::Painter, area: egui::Rect, preview: Option<&egui::TextureHandle>, theme: &KlippTheme) {
    match preview {
        Some(t) => {
            p.image(t.id(), area, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
        None => {
            p.rect_filled(area, 0.0, theme.bg_raised);
        }
    }
}

/// The card's frame: at rest a fixed moment, hovered the loop playing,
/// with the next frames queued ahead and the nearest cached frame
/// standing in while one renders.
fn preview_texture(ui: &egui::Ui, previews: &mut GeneratorPreviews, tmpl: &GeneratorTemplate) -> Option<egui::TextureHandle> {
    use crate::browser::generator_previews::{FRAME_STEP, LOOP_SECS, REST_TIME};
    let elapsed = if tmpl.animated { gen_read_anim_elapsed(ui, tmpl.name.as_str()) } else { 0.0 };
    let time = if elapsed > 0.0 { GeneratorPreviews::hover_time(elapsed) } else { REST_TIME };
    if elapsed > 0.0 {
        for k in 1..=6 {
            let ahead = GeneratorPreviews::hover_time((time + k as f32 * FRAME_STEP) % LOOP_SECS);
            let key = GeneratorPreviews::key(tmpl.name.as_str(), ahead);
            previews.request(&key, ahead, || tmpl.data.clone());
        }
    }
    let key = GeneratorPreviews::key(tmpl.name.as_str(), time);
    previews
        .get_or_request(&key, time, || tmpl.data.clone())
        .cloned()
        .or_else(|| previews.nearest_for(tmpl.name.as_str(), time).cloned())
}

fn gen_read_anim_elapsed(ui: &egui::Ui, name: &str) -> f32 {
    let active_id = ui.id().with(("gen_anim_active", name));
    let start_id = ui.id().with(("gen_anim_start", name));
    let now = ui.ctx().input(|i| i.time);
    let active: bool = ui.memory(|m| m.data.get_temp(active_id)).unwrap_or(false);
    if !active {
        return 0.0;
    }
    let start: f64 = ui.memory(|m| m.data.get_temp(start_id)).unwrap_or(now);
    (now - start).max(0.0) as f32
}

fn gen_update_anim_state(ui: &egui::Ui, name: &str, active: bool) {
    let active_id = ui.id().with(("gen_anim_active", name));
    let start_id = ui.id().with(("gen_anim_start", name));
    let now = ui.ctx().input(|i| i.time);
    let prev: bool = ui.memory(|m| m.data.get_temp(active_id)).unwrap_or(false);
    ui.memory_mut(|m| {
        m.data.insert_temp(active_id, active);
        if active && !prev {
            m.data.insert_temp(start_id, now);
        }
        if !active {
            m.data.remove::<f64>(start_id);
        }
    });
}

fn generator_list_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    tmpl: &GeneratorTemplate,
    width: f32,
    is_selected: bool,
    preview: Option<&egui::TextureHandle>,
) -> egui::Response {
    let row_h = 40.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, row_h),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let bg = if is_selected {
        theme.accent_subtle
    } else if hovered {
        theme.bg_raised
    } else {
        theme.bg_panel
    };
    ui.painter().rect_filled(rect, crate::theme::radius::PANEL, bg);

    let thumb_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + 6.0, rect.center().y - 14.0),
        egui::vec2(50.0, 28.0),
    );
    paint_preview(ui.painter(), thumb_rect, preview, theme);

    ui.painter().text(
        egui::pos2(thumb_rect.max.x + 10.0, rect.center().y - 7.0),
        egui::Align2::LEFT_CENTER,
        tmpl.name.as_str(),
        egui::FontId::proportional(crate::theme::type_scale::BODY),
        theme.text_primary,
    );
    let meta = format!("{} \u{00B7} Built-in{}", tmpl.category, if tmpl.animated { " \u{00B7} Animated" } else { "" });
    ui.painter().text(
        egui::pos2(thumb_rect.max.x + 10.0, rect.center().y + 8.0),
        egui::Align2::LEFT_CENTER,
        meta,
        egui::FontId::proportional(crate::theme::type_scale::CAPTION),
        theme.text_dim,
    );

    if is_selected {
        ui.painter().rect_stroke(rect, crate::theme::radius::PANEL, egui::Stroke::new(1.5_f32, theme.accent));
    }

    resp
}

// ────────────────────────── Sidebar primitives ──────────────────────────
//
// The visual rules (row geometry, hover/active treatment, count
// pill, icon tinting) duplicate the equivalents in `titles_tab` and
// `transitions_tab`. Sharing them via a common module is a clean
// refactor but out of scope for this redesign — keep the duplication
// localized so the three tabs stay self-contained.

fn gen_sidebar_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
    count: usize,
    is_active: bool,
    id_salt: &str,
    on_click: &mut dyn FnMut(Option<&'static str>),
    target_cat: Option<&'static str>,
) {
    let row_h = 26.0;
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());
    let hovered = resp.hovered() || click_resp.hovered();

    if is_active {
        ui.painter().rect_filled(rect, 0.0, theme.accent_subtle);
        let stripe = egui::Rect::from_min_size(rect.min, egui::vec2(2.0, rect.height()));
        ui.painter().rect_filled(stripe, 0.0, theme.accent);
    } else if hovered {
        ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
    }

    let indent = 14.0;
    let icon_size = 13.0;
    let icon_gap = 8.0;
    let label_x = rect.min.x + indent + icon_size + icon_gap;
    let label_color = if is_active || hovered { theme.text_primary } else { theme.text_secondary };
    let icon_tint = if is_active { theme.accent } else { theme.text_dim };

    if let Some(icon) = icon {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
            egui::vec2(icon_size, icon_size),
        );
        let img = icon.image(icon_size).tint(icon_tint);
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| { ui.add(img); });
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

    let count_color = if is_active { theme.accent } else { theme.text_dim };
    let count_galley = ui.painter().layout_no_wrap(
        format!("{}", count),
        egui::FontId::monospace(10.0),
        count_color,
    );
    ui.painter().galley(
        egui::pos2(rect.max.x - 14.0 - count_galley.size().x, rect.center().y - count_galley.size().y * 0.5),
        count_galley,
        count_color,
    );

    if click_resp.clicked() || resp.clicked() {
        on_click(target_cat);
    }
}

fn gen_general_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Icon,
    label: &str,
    count: usize,
    is_active: bool,
    id_salt: &str,
) -> bool {
    let row_h = 26.0;
    let avail_w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(avail_w, row_h),
        egui::Sense::click(),
    );
    let click_resp = ui.interact(rect, ui.id().with(id_salt), egui::Sense::click());
    let hovered = resp.hovered() || click_resp.hovered();
    if is_active {
        ui.painter().rect_filled(rect, 0.0, theme.accent_subtle);
    } else if hovered {
        ui.painter().rect_filled(rect, 0.0, theme.bg_raised);
    }
    let indent = 14.0;
    let icon_size = 13.0;
    let icon_gap = 8.0;
    let label_x = rect.min.x + indent + icon_size + icon_gap;
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + indent, rect.center().y - icon_size * 0.5),
        egui::vec2(icon_size, icon_size),
    );
    let img = icon.image(icon_size).tint(theme.text_dim);
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| { ui.add(img); });
    let label_galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    ui.painter().galley(
        egui::pos2(label_x, rect.center().y - label_galley.size().y * 0.5),
        label_galley,
        theme.text_secondary,
    );
    let count_galley = ui.painter().layout_no_wrap(
        format!("{}", count),
        egui::FontId::monospace(10.0),
        theme.text_dim,
    );
    ui.painter().galley(
        egui::pos2(rect.max.x - 14.0 - count_galley.size().x, rect.center().y - count_galley.size().y * 0.5),
        count_galley,
        theme.text_dim,
    );
    resp.clicked() || click_resp.clicked()
}

fn gen_section_separator(ui: &mut egui::Ui, theme: &KlippTheme) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 1.0),
        egui::Sense::hover(),
    );
    ui.painter().hline(
        (rect.min.x + 8.0)..=(rect.max.x - 8.0),
        rect.center().y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.add_space(4.0);
}

fn gen_section_header(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    icon: Option<Icon>,
    label: &str,
) {
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 20.0),
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
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| { ui.add(img); });
    }
    ui.painter().text(
        egui::pos2(label_x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label.to_uppercase(),
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );
}

fn category_icon(cat: &str) -> Icon {
    match cat {
        "Backgrounds" => Icon::GridFour,
        "Test" => Icon::Sliders,
        "Timing" => Icon::ArrowCounterClockwise,
        "Slate" => Icon::FilmSlate,
        _ => Icon::Star,
    }
}

fn animated_only_pill(ui: &mut egui::Ui, theme: &KlippTheme, active: &mut bool) {
    let label_galley = ui.painter().layout_no_wrap(
        "Animated only".to_string(),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );
    let icon_size = 11.0;
    let pad_x = 10.0;
    let gap = 5.0;
    let w = pad_x * 2.0 + icon_size + gap + label_galley.size().x;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 26.0), egui::Sense::click());
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
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| { ui.add(img); });
    ui.painter().text(
        egui::pos2(rect.min.x + pad_x + icon_size + gap, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "Animated only",
        egui::FontId::proportional(11.0),
        fg,
    );
    if resp.clicked() {
        *active = !*active;
    }
}

fn view_mode_button(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    current: &mut EffectViewMode,
    target: EffectViewMode,
    icon: Icon,
    tooltip: &str,
) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::click());
    let is_active = *current == target;
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
    let size = 13.0;
    let icon_rect = egui::Rect::from_center_size(rect.center(), egui::vec2(size, size));
    let img = icon.image(size).tint(fg);
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(icon_rect), |ui| { ui.add(img); });
    let _ = resp.clone().on_hover_text(tooltip);
    if resp.clicked() {
        *current = target;
    }
}

fn gen_result_strip(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    count: usize,
    sort_mode: &mut EffectSort,
) {
    let avail_w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(avail_w, 24.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        rect.x_range(),
        rect.max.y,
        egui::Stroke::new(1.0_f32, theme.border),
    );
    ui.painter().text(
        egui::pos2(rect.min.x + 12.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{} Generators", count),
        egui::FontId::proportional(11.0),
        theme.text_secondary,
    );

    let dropdown_w = 116.0;
    let dropdown_rect = egui::Rect::from_min_max(
        egui::pos2(rect.max.x - dropdown_w - 8.0, rect.min.y + 1.0),
        egui::pos2(rect.max.x - 8.0, rect.max.y - 1.0),
    );
    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(dropdown_rect), |ui| {
        ui.style_mut().visuals.widgets.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
        ui.style_mut().visuals.widgets.inactive.bg_stroke = egui::Stroke::NONE;
        let label = format!("Sort: {}", sort_mode.label());
        egui::ComboBox::from_id_salt("generators_sort_dd")
            .selected_text(egui::RichText::new(label).size(11.0).color(theme.text_dim))
            .width(dropdown_w - 4.0)
            .show_ui(ui, |ui| {
                for variant in [EffectSort::NameAsc, EffectSort::NameDesc, EffectSort::Category] {
                    ui.selectable_value(sort_mode, variant, variant.label());
                }
            });
    });
}
