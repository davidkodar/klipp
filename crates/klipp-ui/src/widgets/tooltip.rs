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
        // Tooltip bg uses bg_raised so it stays readable on both
        // dark and light themes — was hardcoded 0x1e1e26 (always
        // dark) which paired with theme.text_primary turned into
        // dark-on-dark on light themes.
        egui::Frame::none()
            .fill(theme.bg_raised)
            .stroke(egui::Stroke::new(1.0_f32, theme.border_light))
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
                        // Shortcut badge sits on top of the tooltip
                        // surface, so it needs to step DOWN one
                        // elevation rather than stay equal — pick
                        // bg_panel_alt for the inverse of the
                        // previous always-bg_raised choice.
                        egui::Frame::none()
                            .fill(theme.bg_panel_alt)
                            .stroke(egui::Stroke::new(1.0_f32, theme.border_light))
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
