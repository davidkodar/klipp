use crate::theme::KlippTheme;
use klipp_core::AppCommand;
use klipp_nodes::NodeGraph;

/// Render the keyframe curve editor.
/// Session 1 stub: draws an empty dark area. Full node-keyframe UI comes in later sessions.
pub fn show_keyframe_editor(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    _clip_id: klipp_core::ClipId,
    _node_graph: &NodeGraph,
    clip_duration_secs: f64,
    clip_time_secs: f64,
    height: f32,
) -> Vec<AppCommand> {
    let bg = egui::Color32::from_rgb(0x16, 0x16, 0x1a);
    let grid_color = egui::Color32::from_rgb(0x22, 0x22, 0x28);
    let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
    let playhead_color = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);

    let (rect, _response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);

    // Background
    painter.rect_filled(rect, 2.0, bg);

    let duration = clip_duration_secs.max(1.0);
    let pad_x = 40.0;
    let pad_y = 16.0;
    let plot_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + pad_x, rect.min.y + 4.0),
        egui::pos2(rect.max.x - 4.0, rect.max.y - pad_y),
    );

    if plot_rect.width() < 10.0 || plot_rect.height() < 10.0 {
        return Vec::new();
    }

    // Grid lines
    for i in 0..=4 {
        let t = i as f32 / 4.0;
        let x = plot_rect.min.x + t * plot_rect.width();
        painter.line_segment(
            [egui::pos2(x, plot_rect.min.y), egui::pos2(x, plot_rect.max.y)],
            egui::Stroke::new(0.5_f32, grid_color),
        );
        let secs = t as f64 * duration;
        let label = format!("{:.1}s", secs);
        painter.text(
            egui::pos2(x, plot_rect.max.y + 2.0),
            egui::Align2::CENTER_TOP,
            label,
            egui::FontId::monospace(8.0),
            text_dim,
        );

        let y = plot_rect.min.y + t * plot_rect.height();
        painter.line_segment(
            [egui::pos2(plot_rect.min.x, y), egui::pos2(plot_rect.max.x, y)],
            egui::Stroke::new(0.5_f32, grid_color),
        );
    }

    // Playhead vertical line
    let playhead_x = plot_rect.min.x + (clip_time_secs / duration) as f32 * plot_rect.width();
    if playhead_x >= plot_rect.min.x && playhead_x <= plot_rect.max.x {
        painter.line_segment(
            [egui::pos2(playhead_x, plot_rect.min.y), egui::pos2(playhead_x, plot_rect.max.y)],
            egui::Stroke::new(1.5_f32, playhead_color),
        );
    }

    // Hint label
    painter.text(
        plot_rect.center(),
        egui::Align2::CENTER_CENTER,
        "Press Shift+K to open the Curve Editor",
        egui::FontId::monospace(10.0),
        text_dim,
    );

    Vec::new()
}
