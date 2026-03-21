use crate::theme::KlippTheme;

/// Show a styled tooltip on hover with an optional keyboard shortcut badge.
/// Call this on any `egui::Response` to attach a tooltip.
pub fn styled_tooltip(
    response: &egui::Response,
    theme: &KlippTheme,
    label: &str,
    shortcut: Option<&str>,
) {
    response.clone().on_hover_ui_at_pointer(|ui| {
        egui::Frame::none()
            .fill(egui::Color32::from_rgb(0x1e, 0x1e, 0x26))
            .stroke(egui::Stroke::new(1.0, theme.border_light))
            .rounding(5.0)
            .inner_margin(egui::Margin::symmetric(10.0, 5.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(
                        egui::RichText::new(label)
                            .size(11.0)
                            .color(theme.text_primary),
                    );
                    if let Some(key) = shortcut {
                        egui::Frame::none()
                            .fill(theme.bg_raised)
                            .stroke(egui::Stroke::new(1.0, theme.border_light))
                            .rounding(3.0)
                            .inner_margin(egui::Margin::symmetric(4.0, 1.0))
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(key)
                                        .size(10.0)
                                        .color(theme.accent)
                                        .monospace(),
                                );
                            });
                    }
                });
            });
    });
}
