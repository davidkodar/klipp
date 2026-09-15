use crate::browser::effects_tab::{EffectSort, EffectViewMode};
use crate::drag_drop::DragPayload;
use crate::icons::Icon;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, FourPointGradient, GeneratorData};

/// Built-in vs user-imported source filter for the Generators tab.
/// Mirrors `TitlesSourceFilter` / `TransitionsSourceFilter` so the
/// segmented control reads the same across the Library tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum GeneratorsSourceFilter {
    #[default]
    All,
    Builtin,
    User,
}


struct GeneratorTemplate {
    name: &'static str,
    category: &'static str,
    create_data: fn() -> GeneratorData,
    /// `true` for generators whose visible content advances over the
    /// clip (e.g. countdown). Drives the "Animated only" filter and
    /// the per-card ANIMATED badge so the pattern matches the other
    /// Library tabs.
    animated: bool,
}

// ---------------------------------------------------------------------------
// Generator factories
// ---------------------------------------------------------------------------

fn solid_black() -> GeneratorData { GeneratorData::SolidColor { color: [0, 0, 0, 255] } }
fn solid_white() -> GeneratorData { GeneratorData::SolidColor { color: [255, 255, 255, 255] } }
fn solid_red() -> GeneratorData { GeneratorData::SolidColor { color: [220, 50, 50, 255] } }
fn solid_green() -> GeneratorData { GeneratorData::SolidColor { color: [50, 180, 80, 255] } }
fn solid_blue() -> GeneratorData { GeneratorData::SolidColor { color: [50, 100, 220, 255] } }
fn solid_cyan() -> GeneratorData { GeneratorData::SolidColor { color: [60, 200, 210, 255] } }
fn solid_magenta() -> GeneratorData { GeneratorData::SolidColor { color: [210, 60, 180, 255] } }
fn solid_yellow() -> GeneratorData { GeneratorData::SolidColor { color: [230, 210, 50, 255] } }
fn solid_grey() -> GeneratorData { GeneratorData::SolidColor { color: [128, 128, 128, 255] } }

fn gradient(top: [u8; 4], bottom: [u8; 4], angle_deg: f32) -> GeneratorData {
    GeneratorData::Gradient { top, bottom, angle_deg, four_point: None }
}
fn gradient_bw() -> GeneratorData { gradient([0, 0, 0, 255], [255, 255, 255, 255], 0.0) }
fn gradient_wb() -> GeneratorData { gradient([255, 255, 255, 255], [0, 0, 0, 255], 0.0) }
fn gradient_blue() -> GeneratorData { gradient([80, 140, 220, 255], [0, 0, 30, 255], 0.0) }
fn gradient_sunset() -> GeneratorData { gradient([255, 140, 60, 255], [90, 40, 120, 255], 0.0) }
fn gradient_dawn() -> GeneratorData { gradient([255, 200, 220, 255], [120, 100, 200, 255], 0.0) }
fn gradient_forest() -> GeneratorData { gradient([40, 90, 60, 255], [10, 20, 15, 255], 0.0) }
fn gradient_amber() -> GeneratorData { gradient([255, 180, 80, 255], [60, 25, 10, 255], 0.0) }
// Diagonal preset showcases the new `angle_deg` axis without needing
// the user to dig into the inspector first.
fn gradient_diagonal_neon() -> GeneratorData {
    gradient([255, 80, 200, 255], [40, 220, 255, 255], 45.0)
}
// Cinematic 4-corner wash — each corner a different colour. Demonstrates
// the new `four_point` mode straight from the browser.
fn gradient_4pt_warm_cool() -> GeneratorData {
    GeneratorData::Gradient {
        top: [0, 0, 0, 0],
        bottom: [0, 0, 0, 0],
        angle_deg: 0.0,
        four_point: Some(FourPointGradient {
            top_left: [255, 140, 60, 255],
            top_right: [255, 80, 120, 255],
            bottom_left: [40, 90, 180, 255],
            bottom_right: [80, 200, 220, 255],
        }),
    }
}

fn color_bars() -> GeneratorData { GeneratorData::ColorBars }
fn countdown() -> GeneratorData { GeneratorData::Countdown }

// Vector shape factories. Defaults are mid-spec — 6-sided polygon
// (hexagon) and 5-pointed star with 0.4 inner-ratio (the classic
// pentagram look). Users tune via the inspector once on the timeline.
fn shape_triangle() -> GeneratorData {
    GeneratorData::Polygon { sides: 3, color: [255, 255, 255, 255], outline_width_px: 0.0, fill: true }
}
fn shape_square() -> GeneratorData {
    GeneratorData::Polygon { sides: 4, color: [255, 255, 255, 255], outline_width_px: 0.0, fill: true }
}
fn shape_pentagon() -> GeneratorData {
    GeneratorData::Polygon { sides: 5, color: [255, 255, 255, 255], outline_width_px: 0.0, fill: true }
}
fn shape_hexagon() -> GeneratorData {
    GeneratorData::Polygon { sides: 6, color: [255, 255, 255, 255], outline_width_px: 0.0, fill: true }
}
fn shape_octagon() -> GeneratorData {
    GeneratorData::Polygon { sides: 8, color: [255, 255, 255, 255], outline_width_px: 0.0, fill: true }
}
fn shape_circle_approx() -> GeneratorData {
    // 24-sided polygon reads as a circle at typical viewer sizes.
    GeneratorData::Polygon { sides: 24, color: [255, 255, 255, 255], outline_width_px: 0.0, fill: true }
}
fn shape_star_5() -> GeneratorData {
    GeneratorData::Star { points: 5, inner_radius_pct: 0.40, color: [255, 220, 60, 255], outline_width_px: 0.0, fill: true }
}
fn shape_star_6() -> GeneratorData {
    GeneratorData::Star { points: 6, inner_radius_pct: 0.50, color: [255, 220, 60, 255], outline_width_px: 0.0, fill: true }
}
fn color_bars_100() -> GeneratorData { GeneratorData::ColorBars100 }
fn timer_count_up() -> GeneratorData {
    GeneratorData::Timer {
        mode: klipp_core::TimerMode::CountUp,
        initial_seconds: 0.0,
        format: klipp_core::TimerFormat::MmSs,
        text_color: [255, 255, 255, 255],
        background_color: [0, 0, 0, 255],
    }
}
fn timer_count_down() -> GeneratorData {
    GeneratorData::Timer {
        mode: klipp_core::TimerMode::CountDown,
        initial_seconds: 60.0,
        format: klipp_core::TimerFormat::MmSs,
        text_color: [255, 255, 255, 255],
        background_color: [0, 0, 0, 255],
    }
}
fn slate() -> GeneratorData {
    GeneratorData::Slate {
        project: String::from("Project Name"),
        scene: String::new(),
        take: String::new(),
        director: String::new(),
        camera: String::new(),
        date: String::new(),
        framerate: String::new(),
        notes: String::new(),
        text_color: [255, 255, 255, 255],
        background_color: [0, 0, 0, 255],
    }
}

const CATEGORIES: &[&str] = &["Solid Color", "Gradient", "Test Pattern", "Countdown", "Slate", "Shape"];

const GENERATORS: &[GeneratorTemplate] = &[
    // Solid Color
    GeneratorTemplate { name: "Black",       category: "Solid Color", create_data: solid_black,   animated: false },
    GeneratorTemplate { name: "White",       category: "Solid Color", create_data: solid_white,   animated: false },
    GeneratorTemplate { name: "Red",         category: "Solid Color", create_data: solid_red,     animated: false },
    GeneratorTemplate { name: "Green",       category: "Solid Color", create_data: solid_green,   animated: false },
    GeneratorTemplate { name: "Blue",        category: "Solid Color", create_data: solid_blue,    animated: false },
    GeneratorTemplate { name: "Cyan",        category: "Solid Color", create_data: solid_cyan,    animated: false },
    GeneratorTemplate { name: "Magenta",     category: "Solid Color", create_data: solid_magenta, animated: false },
    GeneratorTemplate { name: "Yellow",      category: "Solid Color", create_data: solid_yellow,  animated: false },
    GeneratorTemplate { name: "50% Grey",    category: "Solid Color", create_data: solid_grey,    animated: false },
    // Gradient
    GeneratorTemplate { name: "Black \u{2192} White", category: "Gradient", create_data: gradient_bw,     animated: false },
    GeneratorTemplate { name: "White \u{2192} Black", category: "Gradient", create_data: gradient_wb,     animated: false },
    GeneratorTemplate { name: "Blue Fade",            category: "Gradient", create_data: gradient_blue,   animated: false },
    GeneratorTemplate { name: "Sunset",               category: "Gradient", create_data: gradient_sunset, animated: false },
    GeneratorTemplate { name: "Dawn",                 category: "Gradient", create_data: gradient_dawn,   animated: false },
    GeneratorTemplate { name: "Forest",               category: "Gradient", create_data: gradient_forest, animated: false },
    GeneratorTemplate { name: "Amber",                category: "Gradient", create_data: gradient_amber,  animated: false },
    GeneratorTemplate { name: "Neon Diagonal",        category: "Gradient", create_data: gradient_diagonal_neon, animated: false },
    GeneratorTemplate { name: "4-Corner Wash",        category: "Gradient", create_data: gradient_4pt_warm_cool, animated: false },
    // Test Pattern
    GeneratorTemplate { name: "SMPTE 75% Bars",  category: "Test Pattern", create_data: color_bars,     animated: false },
    GeneratorTemplate { name: "SMPTE 100% Bars", category: "Test Pattern", create_data: color_bars_100, animated: false },
    // Countdown
    GeneratorTemplate { name: "Countdown",         category: "Countdown",    create_data: countdown,         animated: true },
    GeneratorTemplate { name: "Timer (Count Up)",  category: "Countdown",    create_data: timer_count_up,    animated: true },
    GeneratorTemplate { name: "Timer (Count Down)", category: "Countdown",   create_data: timer_count_down,  animated: true },
    // Slate
    GeneratorTemplate { name: "Slate",             category: "Slate",        create_data: slate,             animated: false },
    // Shape
    GeneratorTemplate { name: "Triangle",       category: "Shape", create_data: shape_triangle,        animated: false },
    GeneratorTemplate { name: "Square",         category: "Shape", create_data: shape_square,          animated: false },
    GeneratorTemplate { name: "Pentagon",       category: "Shape", create_data: shape_pentagon,        animated: false },
    GeneratorTemplate { name: "Hexagon",        category: "Shape", create_data: shape_hexagon,         animated: false },
    GeneratorTemplate { name: "Octagon",        category: "Shape", create_data: shape_octagon,         animated: false },
    GeneratorTemplate { name: "Circle (24-gon)", category: "Shape", create_data: shape_circle_approx,  animated: false },
    GeneratorTemplate { name: "Star (5-pt)",    category: "Shape", create_data: shape_star_5,          animated: false },
    GeneratorTemplate { name: "Star (6-pt)",    category: "Shape", create_data: shape_star_6,          animated: false },
];

const GENERATORS_SIDEBAR_W: f32 = 180.0;

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &mut String,
    active_category: &mut Option<&'static str>,
    source_filter: &mut GeneratorsSourceFilter,
    animated_only: &mut bool,
    view_mode: &mut EffectViewMode,
    sort_mode: &mut EffectSort,
    selected: &mut Option<String>,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let avail = ui.available_rect_before_wrap();

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

                // Counts (registry-wide). Favorites/Recent are stubs
                // mirroring the Audio FX / Titles sidebars until a
                // persistence layer lands.
                let installed_total = GENERATORS.len();
                gen_general_row(ui, theme, Icon::Star, "Favorites", 0, false, "generators_general_favourites");
                gen_general_row(ui, theme, Icon::ArrowCounterClockwise, "Recent", 0, false, "generators_general_recent");
                gen_general_row(ui, theme, Icon::UploadSimple, "Installed", installed_total, false, "generators_general_installed");

                ui.add_space(8.0);
                gen_section_separator(ui, theme);
                gen_section_header(ui, theme, Some(Icon::Star), "Generators");

                gen_sidebar_row(
                    ui, theme, Some(Icon::List), "All",
                    installed_total, active_category.is_none(),
                    "generators_cat_all",
                    &mut |opt| *active_category = opt,
                    None,
                );
                for cat in CATEGORIES {
                    let count = GENERATORS.iter().filter(|t| t.category == *cat).count();
                    let is_active = *active_category == Some(*cat);
                    gen_sidebar_row(
                        ui, theme, Some(category_icon(cat)), cat,
                        count, is_active,
                        &format!("generators_cat_{cat}"),
                        &mut |opt| *active_category = opt,
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

        // Filter row: All / Built-in / User + Animated only + view toggle
        let scoped_total = GENERATORS
            .iter()
            .filter(|t| match active_category {
                Some(cat) => t.category == *cat,
                None => true,
            })
            .count();
        // Every entry is Built-in today (no user-importable generators
        // yet); User stays at zero so the chip honestly reports.
        let builtin_count = scoped_total;
        let user_count: usize = 0;

        egui::Frame::none()
            .inner_margin(egui::Margin::symmetric(8.0, 0.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    gen_segmented_filter(
                        ui, theme, source_filter,
                        scoped_total, builtin_count, user_count,
                    );
                    ui.add_space(6.0);
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
        let mut filtered: Vec<&GeneratorTemplate> = GENERATORS
            .iter()
            .filter(|t| match active_category {
                Some(cat) => t.category == *cat,
                None => true,
            })
            .filter(|_| match *source_filter {
                GeneratorsSourceFilter::All | GeneratorsSourceFilter::Builtin => true,
                GeneratorsSourceFilter::User => false,
            })
            .filter(|t| !*animated_only || t.animated)
            .filter(|t| {
                if search_lower.is_empty() {
                    return true;
                }
                t.name.to_lowercase().contains(&search_lower)
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
                                let is_selected = selected.as_deref() == Some(tmpl.name);
                                let resp = generator_card(ui, theme, tmpl, card_w, card_h, is_selected);
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
                        let is_selected = selected.as_deref() == Some(tmpl.name);
                        let resp = generator_list_row(ui, theme, tmpl, avail_w, is_selected);
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
    if resp.clicked() {
        *selected = Some(tmpl.name.to_string());
    }
    if resp.double_clicked() {
        cmds.push(AppCommand::CreateGeneratorClip {
            track_index: None,
            data: (tmpl.create_data)(),
        });
        state.toast.show(format!("\"{}\" generator added to timeline", tmpl.name));
    }
    if resp.drag_started() {
        state.drag_drop.begin(DragPayload::Generator {
            name: tmpl.name.to_string(),
            data: (tmpl.create_data)(),
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
    let data = (tmpl.create_data)();
    crate::browser::library_card::library_card(ui, rect, tmpl.name, &caption, is_selected, hovered, theme, |p, preview| {
        paint_generator_preview(p, preview.shrink(1.0), &data);
    });

    resp
}

fn generator_list_row(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    tmpl: &GeneratorTemplate,
    width: f32,
    is_selected: bool,
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
    ui.painter().rect_filled(rect, 4.0, bg);

    // Thumb (40×24) on the left
    let thumb_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x + 6.0, rect.center().y - 12.0),
        egui::vec2(40.0, 24.0),
    );
    let data = (tmpl.create_data)();
    paint_generator_preview(ui.painter(), thumb_rect, &data);

    // Name
    ui.painter().text(
        egui::pos2(thumb_rect.max.x + 10.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        tmpl.name,
        egui::FontId::proportional(11.0),
        theme.text_primary,
    );

    // Right-side ANIMATED + BUILT-IN chips
    let mut x = rect.max.x - 6.0;
    let badge_w = 64.0;
    let badge_h = 16.0;
    let badge_rect = egui::Rect::from_min_size(
        egui::pos2(x - badge_w, rect.center().y - badge_h * 0.5),
        egui::vec2(badge_w, badge_h),
    );
    paint_corner_badge(
        ui.painter(), theme, badge_rect, "BUILT-IN",
        theme.text_secondary, theme.bg_panel,
    );
    x -= badge_w + 6.0;
    if tmpl.animated {
        let badge_rect = egui::Rect::from_min_size(
            egui::pos2(x - badge_w, rect.center().y - badge_h * 0.5),
            egui::vec2(badge_w, badge_h),
        );
        paint_animated_chip(ui.painter(), theme, badge_rect);
    }

    if is_selected {
        ui.painter().rect_stroke(
            rect, 4.0, egui::Stroke::new(1.0_f32, theme.accent),
        );
    }

    resp
}

fn paint_corner_badge(
    p: &egui::Painter,
    theme: &KlippTheme,
    rect: egui::Rect,
    label: &str,
    fg: egui::Color32,
    bg: egui::Color32,
) {
    p.rect_filled(rect, 2.0, bg);
    p.rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, theme.border));
    p.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(10.0),
        fg,
    );
}

fn paint_animated_chip(p: &egui::Painter, theme: &KlippTheme, rect: egui::Rect) {
    // Opaque underlay first so the chip never lets the preview
    // (e.g. countdown circle stroke) bleed through the semi-
    // transparent accent fill on top.
    p.rect_filled(rect, 2.0, theme.bg_panel);
    p.rect_filled(rect, 2.0, theme.accent_subtle);
    p.rect_stroke(rect, 2.0, egui::Stroke::new(1.0_f32, theme.accent_dim));
    p.text(
        egui::pos2(rect.min.x + 6.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        "+ ANIMATED",
        egui::FontId::proportional(10.0),
        theme.accent,
    );
}

fn paint_generator_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    data: &GeneratorData,
) {
    let rounding = egui::Rounding::same(4.0);

    match data {
        GeneratorData::SolidColor { color } => {
            let c = egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
            painter.rect_filled(rect, rounding, c);
        }

        GeneratorData::Gradient { top, bottom, angle_deg, four_point } => {
            // Cards are small; we sample the gradient on a coarse
            // grid (32×18 cells) and paint each cell as a tiny rect.
            // That's enough resolution for the thumbnail to read
            // honestly at any angle / 4-corner combination without
            // doing a per-pixel bilinear blend.
            painter.rect_filled(rect, rounding, egui::Color32::BLACK);
            let cols = 32usize;
            let rows = 18usize;
            let cell_w = rect.width() / cols as f32;
            let cell_h = rect.height() / rows as f32;
            for ry in 0..rows {
                for cx in 0..cols {
                    let tx = cx as f32 / (cols - 1).max(1) as f32;
                    let ty = ry as f32 / (rows - 1).max(1) as f32;
                    let color = sample_gradient(top, bottom, *angle_deg, *four_point, tx, ty);
                    let cell_rect = egui::Rect::from_min_size(
                        egui::pos2(
                            rect.min.x + cx as f32 * cell_w,
                            rect.min.y + ry as f32 * cell_h,
                        ),
                        egui::vec2(cell_w + 0.5, cell_h + 0.5),
                    );
                    painter.rect_filled(
                        cell_rect.intersect(rect),
                        egui::Rounding::ZERO,
                        color,
                    );
                }
            }
            // Re-stroke the rounded outline so the cells don't show
            // square corners.
            painter.rect_stroke(rect, rounding, egui::Stroke::NONE);
        }

        GeneratorData::ColorBars => {
            // SMPTE 75% bars (values mirror klipp_core::generator::render_color_bars).
            const BARS: [[u8; 4]; 7] = [
                [191, 191, 191, 255],
                [191, 191, 0, 255],
                [0, 191, 191, 255],
                [0, 191, 0, 255],
                [191, 0, 191, 255],
                [191, 0, 0, 255],
                [0, 0, 191, 255],
            ];
            painter.rect_filled(rect, rounding, egui::Color32::BLACK);
            let bar_w = rect.width() / 7.0;
            for (i, bar) in BARS.iter().enumerate() {
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + i as f32 * bar_w, rect.min.y),
                    egui::vec2(bar_w + 0.5, rect.height()),
                );
                painter.rect_filled(
                    bar_rect.intersect(rect),
                    egui::Rounding::ZERO,
                    egui::Color32::from_rgba_premultiplied(bar[0], bar[1], bar[2], bar[3]),
                );
            }
        }

        GeneratorData::ColorBars100 => {
            // SMPTE 100% bars — same layout as ColorBars at full intensity.
            const BARS: [[u8; 4]; 7] = [
                [255, 255, 255, 255],
                [255, 255, 0, 255],
                [0, 255, 255, 255],
                [0, 255, 0, 255],
                [255, 0, 255, 255],
                [255, 0, 0, 255],
                [0, 0, 255, 255],
            ];
            painter.rect_filled(rect, rounding, egui::Color32::BLACK);
            let bar_w = rect.width() / 7.0;
            for (i, bar) in BARS.iter().enumerate() {
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + i as f32 * bar_w, rect.min.y),
                    egui::vec2(bar_w + 0.5, rect.height()),
                );
                painter.rect_filled(
                    bar_rect.intersect(rect),
                    egui::Rounding::ZERO,
                    egui::Color32::from_rgba_premultiplied(bar[0], bar[1], bar[2], bar[3]),
                );
            }
        }

        GeneratorData::Countdown => {
            painter.rect_filled(rect, rounding, egui::Color32::BLACK);
            // Reserve a top strip for the corner badges (BUILT-IN /
            // ANIMATED) so the ring stroke never crowds them. Numeral
            // and ring centre in the remaining space; both scale with
            // the card so wide and narrow cards both keep clearance.
            let badge_strip = 28.0_f32;
            let inner_top = (rect.min.y + badge_strip).min(rect.max.y);
            let inner_rect = egui::Rect::from_min_max(
                egui::pos2(rect.min.x, inner_top),
                rect.max,
            );
            let ring_radius = (inner_rect.height() * 0.40).min(inner_rect.width() * 0.18);
            let ring_centre = inner_rect.center();
            let ring_stroke = egui::Stroke::new(1.5_f32, egui::Color32::from_white_alpha(140));
            painter.circle_stroke(ring_centre, ring_radius, ring_stroke);
            painter.text(
                ring_centre,
                egui::Align2::CENTER_CENTER,
                "5",
                egui::FontId::proportional(inner_rect.height() * 0.55),
                egui::Color32::WHITE,
            );
        }

        GeneratorData::Timer { background_color, text_color, .. } => {
            // Card preview: the timer's own colours + a "00:00" sample.
            let bg = egui::Color32::from_rgba_premultiplied(
                background_color[0], background_color[1], background_color[2], background_color[3],
            );
            let fg = egui::Color32::from_rgba_premultiplied(
                text_color[0], text_color[1], text_color[2], text_color[3],
            );
            painter.rect_filled(rect, rounding, bg);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "00:00",
                egui::FontId::monospace(rect.height() * 0.40),
                fg,
            );
        }

        GeneratorData::Slate { background_color, text_color, .. } => {
            // Card preview: the slate's own colours + the literal word
            // "SLATE" so the card is recognisable independent of the
            // user's specific field values.
            let bg = egui::Color32::from_rgba_premultiplied(
                background_color[0], background_color[1], background_color[2], background_color[3],
            );
            let fg = egui::Color32::from_rgba_premultiplied(
                text_color[0], text_color[1], text_color[2], text_color[3],
            );
            painter.rect_filled(rect, rounding, bg);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "SLATE",
                egui::FontId::proportional(rect.height() * 0.30),
                fg,
            );
        }

        GeneratorData::StillFrame { .. } => {
            // Freeze frames live on the timeline only, not in the
            // Library — paint a neutral placeholder if one ever shows.
            painter.rect_filled(rect, rounding, egui::Color32::from_gray(60));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "\u{2744}",
                egui::FontId::proportional(rect.height() * 0.5),
                egui::Color32::WHITE,
            );
        }

        GeneratorData::Polygon { sides, color, .. } => {
            // Card preview: dark backdrop + the filled n-gon at 70 %
            // of the card's shorter side. egui's convex_polygon
            // takes the same vertex layout as the production
            // rasteriser (clockwise from 12 o'clock).
            painter.rect_filled(rect, rounding, egui::Color32::from_gray(28));
            let n = (*sides).max(3) as usize;
            let cx = rect.center().x;
            let cy = rect.center().y;
            let radius = rect.height().min(rect.width()) * 0.35;
            let verts: Vec<egui::Pos2> = (0..n).map(|i| {
                let theta = -std::f32::consts::FRAC_PI_2
                    + (i as f32) * std::f32::consts::TAU / (n as f32);
                egui::pos2(cx + radius * theta.cos(), cy + radius * theta.sin())
            }).collect();
            let c = egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
            painter.add(egui::Shape::convex_polygon(verts, c, egui::Stroke::NONE));
        }

        GeneratorData::Star { points, inner_radius_pct, color, .. } => {
            // Star is non-convex so we use a Path shape instead of
            // convex_polygon (which would fill the convex hull and
            // mask the points). Outer radius matches the polygon
            // preview's 35 % of the shorter side.
            painter.rect_filled(rect, rounding, egui::Color32::from_gray(28));
            let n_points = (*points).max(3) as usize;
            let cx = rect.center().x;
            let cy = rect.center().y;
            let outer = rect.height().min(rect.width()) * 0.35;
            let inner = outer * inner_radius_pct.clamp(0.05, 0.95);
            let total = n_points * 2;
            let mut verts: Vec<egui::Pos2> = Vec::with_capacity(total);
            for i in 0..total {
                let r = if i % 2 == 0 { outer } else { inner };
                let theta = -std::f32::consts::FRAC_PI_2
                    + (i as f32) * std::f32::consts::TAU / (total as f32);
                verts.push(egui::pos2(cx + r * theta.cos(), cy + r * theta.sin()));
            }
            let c = egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
            // Closed path filled with the chosen colour. egui's
            // tessellator handles concave fans correctly even for
            // 5-pointed stars.
            painter.add(egui::Shape::Path(egui::epaint::PathShape {
                points: verts,
                closed: true,
                fill: c,
                stroke: egui::epaint::PathStroke::NONE,
            }));
        }
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

/// Sample a gradient at normalized coordinates `(tx, ty)` (0..1 across
/// the rect). Mirrors the renderer's logic in
/// `klipp_core::generator::render_gradient` /
/// `render_four_point_gradient` so card previews and on-timeline
/// renders agree.
fn sample_gradient(
    top: &[u8; 4],
    bottom: &[u8; 4],
    angle_deg: f32,
    four_point: Option<klipp_core::FourPointGradient>,
    tx: f32,
    ty: f32,
) -> egui::Color32 {
    if let Some(c) = four_point {
        let r_top_h = lerp_u8(c.top_left[0], c.top_right[0], tx);
        let g_top_h = lerp_u8(c.top_left[1], c.top_right[1], tx);
        let b_top_h = lerp_u8(c.top_left[2], c.top_right[2], tx);
        let a_top_h = lerp_u8(c.top_left[3], c.top_right[3], tx);
        let r_bot_h = lerp_u8(c.bottom_left[0], c.bottom_right[0], tx);
        let g_bot_h = lerp_u8(c.bottom_left[1], c.bottom_right[1], tx);
        let b_bot_h = lerp_u8(c.bottom_left[2], c.bottom_right[2], tx);
        let a_bot_h = lerp_u8(c.bottom_left[3], c.bottom_right[3], tx);
        return egui::Color32::from_rgba_premultiplied(
            lerp_u8(r_top_h, r_bot_h, ty),
            lerp_u8(g_top_h, g_bot_h, ty),
            lerp_u8(b_top_h, b_bot_h, ty),
            lerp_u8(a_top_h, a_bot_h, ty),
        );
    }
    // 2-point at angle. tx/ty are in normalized image space; project
    // onto the gradient axis and lerp.
    let theta = angle_deg.to_radians();
    let dir_x = theta.sin();
    let dir_y = theta.cos();
    // Centre offset = position relative to (0.5, 0.5)
    let dx = tx - 0.5;
    let dy = ty - 0.5;
    let proj = dx * dir_x + dy * dir_y;
    let half_extent = (dir_x.abs() + dir_y.abs()) * 0.5;
    let extent = (half_extent * 2.0).max(f32::EPSILON);
    let t = ((proj + half_extent) / extent).clamp(0.0, 1.0);
    egui::Color32::from_rgba_premultiplied(
        lerp_u8(top[0], bottom[0], t),
        lerp_u8(top[1], bottom[1], t),
        lerp_u8(top[2], bottom[2], t),
        lerp_u8(top[3], bottom[3], t),
    )
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
        "Solid Color" => Icon::Eyedropper,
        "Gradient" => Icon::Sliders,
        "Test Pattern" => Icon::GridFour,
        "Countdown" => Icon::ArrowCounterClockwise,
        _ => Icon::Star,
    }
}

fn gen_segmented_filter(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    filter: &mut GeneratorsSourceFilter,
    all_count: usize,
    builtin_count: usize,
    user_count: usize,
) {
    let outer_h = 26.0;
    let pad = 2.0;
    let segments = [
        (GeneratorsSourceFilter::All, "All", all_count),
        (GeneratorsSourceFilter::Builtin, "Built-in", builtin_count),
        (GeneratorsSourceFilter::User, "User", user_count),
    ];
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
        let w = label_galley.size().x + 6.0 + count_galley.size().x + 22.0;
        seg_widths[i] = w;
        total_w += w + 1.0;
    }
    total_w -= 1.0;
    let (outer_rect, _) = ui.allocate_exact_size(egui::vec2(total_w, outer_h), egui::Sense::hover());
    ui.painter().rect_filled(outer_rect, 4.0, theme.bg_panel);
    ui.painter().rect_stroke(outer_rect, 4.0, egui::Stroke::new(1.0_f32, theme.border));
    let mut x = outer_rect.min.x + pad;
    for (i, (variant, label, count)) in segments.iter().enumerate() {
        let seg_rect = egui::Rect::from_min_size(
            egui::pos2(x, outer_rect.min.y + pad),
            egui::vec2(seg_widths[i], outer_h - pad * 2.0),
        );
        let resp = ui.interact(seg_rect, ui.id().with(("generators_source_seg", i)), egui::Sense::click());
        let is_active = *filter == *variant;
        let hovered = resp.hovered();
        if is_active {
            ui.painter().rect_filled(seg_rect, 2.0, theme.bg_raised);
        } else if hovered {
            ui.painter().rect_filled(seg_rect, 2.0, theme.accent_subtle);
        }
        let label_color = if is_active || hovered { theme.text_primary } else { theme.text_secondary };
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
            egui::pos2(lx + label_galley.size().x + 6.0, seg_rect.center().y - count_galley.size().y * 0.5),
            count_galley,
            count_color,
        );
        if resp.clicked() {
            *filter = *variant;
        }
        x += seg_widths[i] + 1.0;
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
