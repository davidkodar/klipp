use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_timeline::Clip;

pub fn show(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    clip: &Clip,
    _cmds: &mut Vec<AppCommand>,
) {
    // ── Applied Effects stack ──
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("APPLIED EFFECTS")
                .size(10.0)
                .strong()
                .color(theme.text_dim),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new("drag to reorder")
                    .size(9.0)
                    .color(theme.text_dim),
            );
        });
    });
    ui.add_space(7.0);

    // Show applied effects from clip
    let effects = &clip.effects;
    if effects.is_empty() {
        ui.label(
            egui::RichText::new("No effects applied")
                .size(11.0)
                .color(theme.text_dim),
        );
        ui.add_space(8.0);
    } else {
        for effect in effects {
            let effect_name = effect.plugin_name.clone();
            let enabled = effect.enabled;

            egui::Frame::none()
                .fill(theme.bg_raised)
                .stroke(egui::Stroke::new(1.0, theme.border))
                .rounding(5.0)
                .inner_margin(egui::Margin::symmetric(6.0, 5.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Enable toggle
                        let toggle_size = egui::vec2(14.0, 14.0);
                        let (toggle_rect, toggle_resp) =
                            ui.allocate_exact_size(toggle_size, egui::Sense::click());
                        let painter = ui.painter();
                        if enabled {
                            painter.rect_filled(toggle_rect, 3.0, theme.accent);
                            painter.text(
                                toggle_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "\u{2713}",
                                egui::FontId::proportional(9.0),
                                egui::Color32::BLACK,
                            );
                        } else {
                            painter.rect_filled(toggle_rect, 3.0, theme.accent_subtle);
                            painter.rect_stroke(
                                toggle_rect,
                                3.0,
                                egui::Stroke::new(1.0, theme.border_light),
                            );
                        }
                        let _ = toggle_resp;

                        // Effect name
                        ui.label(
                            egui::RichText::new(&effect_name)
                                .size(11.0)
                                .color(theme.text_primary),
                        );

                        // Expand arrow
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    egui::RichText::new("\u{25BE}")
                                        .size(9.0)
                                        .color(theme.text_dim),
                                );
                            },
                        );
                    });
                });
            ui.add_space(4.0);
        }
    }

    // ── Add Effect button ──
    ui.add_space(4.0);
    let add_btn = egui::Button::new(
        egui::RichText::new("\u{FF0B} Add Effect")
            .size(11.0)
            .color(theme.text_dim),
    )
    .fill(egui::Color32::TRANSPARENT)
    .stroke(egui::Stroke::new(
        1.0,
        egui::Stroke { width: 1.0, color: theme.border_light }.color, // dashed approximation
    ))
    .rounding(5.0)
    .min_size(egui::vec2(ui.available_width() - 20.0, 28.0));

    let resp = ui.add(add_btn);
    if resp.hovered() {
        // Hover handled by egui button
    }
}
