/// Draw the skimmer line: 1 px polarity-aware overlay at 35 %
/// opacity at the mouse X position. Only visible when skimming is
/// enabled and mouse is in the tracks area.
pub fn draw(
    painter: &egui::Painter,
    x: f32,
    top: f32,
    bottom: f32,
    theme: &crate::theme::KlippTheme,
) {
    painter.vline(
        x,
        top..=bottom,
        egui::Stroke::new(1.0_f32, theme.overlay(89)),
    );
}
