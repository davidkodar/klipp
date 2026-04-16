/// Draw the skimmer line: 1px white at 35% opacity at the mouse X position.
/// Only visible when skimming is enabled and mouse is in the tracks area.
pub fn draw(
    painter: &egui::Painter,
    x: f32,
    top: f32,
    bottom: f32,
) {
    painter.vline(
        x,
        top..=bottom,
        egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_premultiplied(255, 255, 255, 89), // ~35%
        ),
    );
}
