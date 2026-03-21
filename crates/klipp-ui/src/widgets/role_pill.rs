use crate::theme::KlippTheme;

/// Render a role pill button. Returns true if clicked.
pub fn role_pill(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    role_color: egui::Color32,
    active: bool,
) -> bool {
    let opacity = if active { 1.0 } else { 0.45 };
    let bg = egui::Color32::from_rgba_premultiplied(
        role_color.r(),
        role_color.g(),
        role_color.b(),
        (0.2 * 255.0 * opacity) as u8,
    );
    let border = egui::Color32::from_rgba_premultiplied(
        role_color.r(),
        role_color.g(),
        role_color.b(),
        (0.4 * 255.0 * opacity) as u8,
    );
    // Lighter version for text
    let text_color = egui::Color32::from_rgba_premultiplied(
        (role_color.r() as u16 * 150 / 100).min(255) as u8,
        (role_color.g() as u16 * 150 / 100).min(255) as u8,
        (role_color.b() as u16 * 150 / 100).min(255) as u8,
        (255.0 * opacity) as u8,
    );

    let btn = egui::Button::new(
        egui::RichText::new(label).size(10.0).strong().color(text_color),
    )
    .fill(bg)
    .stroke(egui::Stroke::new(1.0, border))
    .rounding(20.0)
    .min_size(egui::vec2(0.0, 20.0));

    let _ = theme; // theme passed for future use
    ui.add(btn).clicked()
}
