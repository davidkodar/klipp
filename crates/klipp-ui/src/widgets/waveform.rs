/// Draw a decorative or data-driven waveform visualization.
///
/// Bars are 1px wide with 1px gap, centered vertically, with heights from peak data.
/// If `peaks` is None, draws decorative pseudo-random bars.
pub fn draw_waveform(
    painter: &egui::Painter,
    rect: egui::Rect,
    color: egui::Color32,
    peaks: Option<&[f32]>,
) {
    let bar_w = 1.0;
    let gap = 1.0;
    let step = bar_w + gap;
    let bar_count = (rect.width() / step) as usize;
    if bar_count == 0 {
        return;
    }

    let center_y = rect.center().y;
    let max_h = rect.height() * 0.9;

    for i in 0..bar_count {
        let x = rect.min.x + i as f32 * step;

        let amplitude = if let Some(peaks) = peaks {
            if peaks.is_empty() {
                0.0
            } else {
                let idx = i * peaks.len() / bar_count;
                peaks[idx.min(peaks.len() - 1)].clamp(0.0, 1.0)
            }
        } else {
            // Decorative pseudo-random
            ((i * 37 + 13) % 35 + 3) as f32 / 38.0
        };

        let bar_h = (amplitude * max_h).max(2.0);
        painter.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(x, center_y - bar_h / 2.0),
                egui::vec2(bar_w, bar_h),
            ),
            0.0,
            color,
        );
    }
}

/// Draw a waveform with the role color at 60% opacity.
pub fn draw_role_waveform(
    painter: &egui::Painter,
    rect: egui::Rect,
    role_color: egui::Color32,
    peaks: Option<&[f32]>,
) {
    let color = egui::Color32::from_rgba_premultiplied(
        role_color.r(),
        role_color.g(),
        role_color.b(),
        153, // ~60%
    );
    draw_waveform(painter, rect, color, peaks);
}
