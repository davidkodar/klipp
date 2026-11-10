use crate::theme::KlippTheme;

/// Render a role pill button matching the reference design.
/// Dark tinted background, colored border, role-colored text.
pub fn role_pill(
    ui: &mut egui::Ui,
    theme: &KlippTheme,
    label: &str,
    role_color: egui::Color32,
    active: bool,
) -> bool {
    let (bg, text_color, stroke) = role_pill_colours(theme, role_color, active);
    let btn = egui::Button::new(
        egui::RichText::new(label).size(10.0).color(text_color),
    )
    .fill(bg)
    .stroke(stroke)
    .rounding(4.0)
    .min_size(egui::vec2(0.0, 18.0));

    ui.add(btn).clicked()
}

/// A role pill's fill, text and outline. On a dark theme the role's
/// colour on a dark tint of it. On a light theme a pale tint of it on the
/// card, the text the role's colour darkened until it reads (4.5:1) and
/// the outline until it shows (3:1): the dark tint drew black blocks there.
pub fn role_pill_colours(theme: &KlippTheme, role_color: egui::Color32, active: bool) -> (egui::Color32, egui::Color32, egui::Stroke) {
    if theme.is_light() {
        let bg = crate::theme::omarchy::mix(role_color, theme.bg_raised, if active { 0.72 } else { 0.88 });
        let text = theme.readable(role_color, &[bg], crate::theme::TEXT_CONTRAST);
        let edge = theme.readable(role_color, &[bg, theme.bg_panel], crate::theme::UI_CONTRAST);
        let stroke = if active { egui::Stroke::new(1.5_f32, theme.text_primary) } else { egui::Stroke::new(1.0_f32, edge) };
        return (bg, text, stroke);
    }
    let tint = |pct: u16| egui::Color32::from_rgb(
        (role_color.r() as u16 * pct / 100) as u8,
        (role_color.g() as u16 * pct / 100) as u8,
        (role_color.b() as u16 * pct / 100) as u8,
    );
    let bg = if active { tint(30) } else { tint(15) };
    let border = egui::Color32::from_rgba_premultiplied(role_color.r(), role_color.g(), role_color.b(), if active { 200 } else { 100 });
    let text = if active {
        role_color
    } else {
        egui::Color32::from_rgba_premultiplied(role_color.r(), role_color.g(), role_color.b(), 160)
    };
    let stroke = if active {
        egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(230, 230, 235))
    } else {
        egui::Stroke::new(1.0_f32, border)
    };
    (bg, text, stroke)
}
