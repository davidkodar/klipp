use crate::drag_drop::DragPayload;
use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::{AppCommand, GeneratorData};

struct GeneratorTemplate {
    name: &'static str,
    create_data: fn() -> GeneratorData,
}

fn solid_black() -> GeneratorData { GeneratorData::SolidColor { color: [0, 0, 0, 255] } }
fn solid_white() -> GeneratorData { GeneratorData::SolidColor { color: [255, 255, 255, 255] } }
fn solid_red() -> GeneratorData { GeneratorData::SolidColor { color: [220, 50, 50, 255] } }
fn solid_green() -> GeneratorData { GeneratorData::SolidColor { color: [50, 180, 80, 255] } }
fn solid_blue() -> GeneratorData { GeneratorData::SolidColor { color: [50, 100, 220, 255] } }
fn solid_cyan() -> GeneratorData { GeneratorData::SolidColor { color: [60, 200, 210, 255] } }
fn solid_magenta() -> GeneratorData { GeneratorData::SolidColor { color: [210, 60, 180, 255] } }
fn solid_yellow() -> GeneratorData { GeneratorData::SolidColor { color: [230, 210, 50, 255] } }
fn solid_grey() -> GeneratorData { GeneratorData::SolidColor { color: [128, 128, 128, 255] } }

fn gradient_bw() -> GeneratorData { GeneratorData::Gradient { top: [0, 0, 0, 255], bottom: [255, 255, 255, 255] } }
fn gradient_wb() -> GeneratorData { GeneratorData::Gradient { top: [255, 255, 255, 255], bottom: [0, 0, 0, 255] } }
fn gradient_blue() -> GeneratorData { GeneratorData::Gradient { top: [80, 140, 220, 255], bottom: [0, 0, 30, 255] } }
fn gradient_sunset() -> GeneratorData { GeneratorData::Gradient { top: [255, 140, 60, 255], bottom: [90, 40, 120, 255] } }

fn color_bars() -> GeneratorData { GeneratorData::ColorBars }
fn countdown() -> GeneratorData { GeneratorData::Countdown }

const GENERATORS: &[GeneratorTemplate] = &[
    GeneratorTemplate { name: "Black",           create_data: solid_black },
    GeneratorTemplate { name: "White",           create_data: solid_white },
    GeneratorTemplate { name: "Red",             create_data: solid_red },
    GeneratorTemplate { name: "Green",           create_data: solid_green },
    GeneratorTemplate { name: "Blue",            create_data: solid_blue },
    GeneratorTemplate { name: "Cyan",            create_data: solid_cyan },
    GeneratorTemplate { name: "Magenta",         create_data: solid_magenta },
    GeneratorTemplate { name: "Yellow",          create_data: solid_yellow },
    GeneratorTemplate { name: "50% Grey",        create_data: solid_grey },
    GeneratorTemplate { name: "Black \u{2192} White", create_data: gradient_bw },
    GeneratorTemplate { name: "White \u{2192} Black", create_data: gradient_wb },
    GeneratorTemplate { name: "Blue Fade",       create_data: gradient_blue },
    GeneratorTemplate { name: "Sunset",          create_data: gradient_sunset },
    GeneratorTemplate { name: "SMPTE 75% Bars",  create_data: color_bars },
    GeneratorTemplate { name: "Countdown",       create_data: countdown },
];

pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
    search_query: &str,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let search_lower = search_query.to_lowercase();

    let filtered: Vec<_> = GENERATORS
        .iter()
        .filter(|t| search_lower.is_empty() || t.name.to_lowercase().contains(&search_lower))
        .collect();

    egui::ScrollArea::vertical()
        .id_salt("browser_generators_scroll")
        .show(ui, |ui| {
            let avail_w = ui.available_width() - 16.0;
            let card_w = (avail_w - 6.0) / 2.0;

            egui::Grid::new("generators_grid")
                .num_columns(2)
                .spacing([6.0, 6.0])
                .min_col_width(card_w)
                .show(ui, |ui| {
                    for (i, tmpl) in filtered.iter().enumerate() {
                        let resp = generator_card(ui, theme, tmpl, card_w);

                        if resp.double_clicked() {
                            cmds.push(AppCommand::CreateGeneratorClip {
                                track_index: None,
                                data: (tmpl.create_data)(),
                            });
                            state.toast.show(format!(
                                "\"{}\" generator added to timeline",
                                tmpl.name,
                            ));
                        }

                        if resp.drag_started() {
                            state.drag_drop.begin(DragPayload::Generator {
                                name: tmpl.name.to_string(),
                                data: (tmpl.create_data)(),
                            });
                        }

                        if i % 2 == 1 {
                            ui.end_row();
                        }
                    }
                    if filtered.len() % 2 == 1 {
                        ui.end_row();
                    }
                });
        });

    cmds
}

fn generator_card(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    tmpl: &GeneratorTemplate,
    width: f32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, 76.0),
        egui::Sense::click_and_drag(),
    );

    let hovered = resp.hovered();
    let border_color = if hovered { theme.accent_dim } else { theme.border };

    // Card background
    ui.painter().rect_filled(rect, 6.0, theme.bg_raised);
    ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(1.0_f32, border_color));

    // Preview area (top 56px)
    let preview_rect = egui::Rect::from_min_size(rect.min, egui::vec2(width, 56.0));
    let data = (tmpl.create_data)();
    paint_generator_preview(ui.painter(), preview_rect, &data);

    // Name label (bottom 20px)
    let name_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.min.y + 56.0),
        egui::vec2(width, 20.0),
    );
    ui.painter().text(
        name_rect.center(),
        egui::Align2::CENTER_CENTER,
        tmpl.name,
        egui::FontId::proportional(10.0),
        theme.text_dim,
    );

    resp
}

fn paint_generator_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    data: &GeneratorData,
) {
    let rounding = egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 };

    match data {
        GeneratorData::SolidColor { color } => {
            let c = egui::Color32::from_rgba_premultiplied(color[0], color[1], color[2], color[3]);
            painter.rect_filled(rect, rounding, c);
        }

        GeneratorData::Gradient { top, bottom } => {
            // Approximate gradient with horizontal strips
            let strips = 28;
            let strip_h = rect.height() / strips as f32;
            for i in 0..strips {
                let t = if strips <= 1 { 0.0 } else { i as f32 / (strips - 1) as f32 };
                let r = lerp_u8(top[0], bottom[0], t);
                let g = lerp_u8(top[1], bottom[1], t);
                let b = lerp_u8(top[2], bottom[2], t);
                let a = lerp_u8(top[3], bottom[3], t);
                let y = rect.min.y + i as f32 * strip_h;
                let strip_rounding = if i == 0 {
                    egui::Rounding { nw: 6.0, ne: 6.0, sw: 0.0, se: 0.0 }
                } else {
                    egui::Rounding::ZERO
                };
                let strip_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x, y),
                    egui::vec2(rect.width(), strip_h + 0.5),
                );
                painter.rect_filled(strip_rect, strip_rounding, egui::Color32::from_rgba_premultiplied(r, g, b, a));
            }
        }

        GeneratorData::ColorBars => {
            // SMPTE 75% bars (values duplicated from klipp_core::generator::render_color_bars)
            const BARS: [[u8; 4]; 7] = [
                [191, 191, 191, 255],
                [191, 191, 0, 255],
                [0, 191, 191, 255],
                [0, 191, 0, 255],
                [191, 0, 191, 255],
                [191, 0, 0, 255],
                [0, 0, 191, 255],
            ];
            // Background with rounded corners
            painter.rect_filled(rect, rounding, egui::Color32::BLACK);
            let bar_w = rect.width() / 7.0;
            for (i, bar) in BARS.iter().enumerate() {
                let bar_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.min.x + i as f32 * bar_w, rect.min.y),
                    egui::vec2(bar_w + 0.5, rect.height()),
                );
                painter.rect_filled(bar_rect, egui::Rounding::ZERO, egui::Color32::from_rgba_premultiplied(bar[0], bar[1], bar[2], bar[3]));
            }
        }

        GeneratorData::Countdown => {
            painter.rect_filled(rect, rounding, egui::Color32::BLACK);
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "5",
                egui::FontId::proportional(28.0),
                egui::Color32::WHITE,
            );
        }

        GeneratorData::StillFrame { .. } => {
            // Freeze frames live on the timeline only, not in the generator
            // browser — render a neutral placeholder if one ever shows up.
            painter.rect_filled(rect, rounding, egui::Color32::from_gray(60));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "❄",
                egui::FontId::proportional(24.0),
                egui::Color32::WHITE,
            );
        }
    }
}

fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}
