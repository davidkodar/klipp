use crate::theme::KlippTheme;

/// Draw the playhead: 1px accent vertical line + triangle cap at top.
/// This is used by the new timeline layout. The old timeline also draws its own.
pub fn draw(
    painter: &egui::Painter,
    x: f32,
    top: f32,
    bottom: f32,
    theme: &KlippTheme,
) {
    // 1px accent line spanning full track area
    painter.vline(
        x,
        top..=bottom,
        egui::Stroke::new(1.0, theme.accent),
    );

    // Triangle cap (9x8px, pointing down)
    let tri_w = 9.0;
    let tri_h = 8.0;
    let tri_top = top - 1.0;
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(x - tri_w / 2.0, tri_top),
            egui::pos2(x + tri_w / 2.0, tri_top),
            egui::pos2(x, tri_top + tri_h),
        ],
        theme.accent,
        egui::Stroke::NONE,
    ));
}
