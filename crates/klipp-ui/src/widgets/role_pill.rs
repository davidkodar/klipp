use crate::theme::KlippTheme;

/// Render a role pill button matching the reference design.
/// Dark tinted background, colored border, role-colored text.
pub fn role_pill(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    label: &str,
    role_color: egui::Color32,
    active: bool,
) -> bool {
    // Reference style: dark background with color tint, colored border, colored text
    let bg = if active {
        // Active: slightly more visible tint
        egui::Color32::from_rgba_premultiplied(
            (role_color.r() as u16 * 30 / 100) as u8,
            (role_color.g() as u16 * 30 / 100) as u8,
            (role_color.b() as u16 * 30 / 100) as u8,
            255,
        )
    } else {
        // Inactive: very dark tint
        egui::Color32::from_rgba_premultiplied(
            (role_color.r() as u16 * 15 / 100) as u8,
            (role_color.g() as u16 * 15 / 100) as u8,
            (role_color.b() as u16 * 15 / 100) as u8,
            255,
        )
    };

    let border_alpha = if active { 200u8 } else { 100 };
    let border = egui::Color32::from_rgba_premultiplied(
        role_color.r(),
        role_color.g(),
        role_color.b(),
        border_alpha,
    );

    // Text: role color, brighter when active
    let text_color = if active {
        role_color
    } else {
        egui::Color32::from_rgba_premultiplied(
            role_color.r(),
            role_color.g(),
            role_color.b(),
            160,
        )
    };

    let stroke = if active {
        egui::Stroke::new(1.5, egui::Color32::from_rgb(230, 230, 235))
    } else {
        egui::Stroke::new(1.0, border)
    };

    let btn = egui::Button::new(
        egui::RichText::new(label).size(9.0).color(text_color),
    )
    .fill(bg)
    .stroke(stroke)
    .rounding(4.0)
    .min_size(egui::vec2(0.0, 18.0));

    ui.add(btn).clicked()
}
