use crate::theme::KlippTheme;

pub struct VerticalSliderResponse {
    pub changed: bool,
    pub drag_started: bool,
    pub drag_ended: bool,
}

/// Custom-painted vertical centre-origin slider.
/// 3px wide track, 8px amber thumb, value label below in monospace.
/// Drag up = increase, drag down = decrease. Double-click resets to 0.
pub fn vertical_slider(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    track_height: f32,
) -> VerticalSliderResponse {
    let mut changed = false;

    let label_h = 14.0;
    let val_h = 12.0;
    let total_h = label_h + track_height + val_h + 4.0;
    let total_w = 18.0;

    let (full_rect, _) = ui.allocate_exact_size(
        egui::vec2(total_w, total_h),
        egui::Sense::hover(),
    );

    let painter = ui.painter_at(full_rect);
    let cx = full_rect.center().x;
    let text_dim = egui::Color32::from_rgb(0x40, 0x40, 0x50);
    let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
    let track_bg = egui::Color32::from_rgb(0x1e, 0x1e, 0x26);

    // Label above
    painter.text(
        egui::pos2(cx, full_rect.min.y + label_h / 2.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(8.0),
        text_dim,
    );

    // Track
    let track_top = full_rect.min.y + label_h;
    let track_rect = egui::Rect::from_min_size(
        egui::pos2(cx - 1.5, track_top),
        egui::vec2(3.0, track_height),
    );
    painter.rect_filled(track_rect, 2.0, track_bg);

    // Centre tick mark (visible against track)
    let mid_y = track_top + track_height / 2.0;
    painter.line_segment(
        [egui::pos2(cx - 4.0, mid_y), egui::pos2(cx + 4.0, mid_y)],
        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(0x48, 0x48, 0x55)),
    );

    // Compute thumb position: range maps to track_top..track_top+track_height (inverted: top=max, bottom=min)
    let range_span = range.end() - range.start();
    let t = (*value - range.start()) / range_span; // 0..1
    let thumb_y = track_top + track_height - t * track_height; // inverted: high value = top

    // Fill from centre to thumb
    let fill_color = text_accent;
    if (*value).abs() > 0.001 {
        let fill_min_y = thumb_y.min(mid_y);
        let fill_max_y = thumb_y.max(mid_y);
        let fill_rect = egui::Rect::from_min_max(
            egui::pos2(cx - 1.5, fill_min_y),
            egui::pos2(cx + 1.5, fill_max_y),
        );
        painter.rect_filled(fill_rect, 0.0, fill_color);
    }

    // Thumb (8px diameter)
    let thumb_center = egui::pos2(cx, thumb_y);
    painter.circle_filled(thumb_center, 4.0, egui::Color32::from_rgb(0xe0, 0xe0, 0xe8));
    painter.circle_stroke(thumb_center, 4.0, egui::Stroke::new(1.5_f32, text_accent));

    // Value label below (compact: show 1 decimal for small sliders)
    let val_text = if track_height < 50.0 {
        format!("{:+.1}", *value)
    } else {
        format!("{:+.2}", *value)
    };
    let val_font = if track_height < 50.0 { 7.0 } else { 8.0 };
    let val_color = if (*value).abs() > 0.001 { text_accent } else { egui::Color32::from_rgb(0x50, 0x50, 0x60) };
    painter.text(
        egui::pos2(cx, full_rect.max.y - val_h / 2.0),
        egui::Align2::CENTER_CENTER,
        &val_text,
        egui::FontId::monospace(val_font),
        val_color,
    );

    // Interaction: drag on the track+thumb area
    let hit_rect = egui::Rect::from_min_size(
        egui::pos2(cx - 9.0, track_top - 4.0),
        egui::vec2(18.0, track_height + 8.0),
    );
    let resp = ui.allocate_rect(hit_rect, egui::Sense::click_and_drag());
    let drag_started = resp.drag_started();
    let drag_ended = resp.drag_stopped();

    if resp.dragged() {
        let delta = resp.drag_delta().y;
        // Dragging UP increases value
        let step = -delta / track_height * range_span;
        *value = (*value + step).clamp(*range.start(), *range.end());
        changed = true;
    }

    if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let t = 1.0 - ((pos.y - track_top) / track_height).clamp(0.0, 1.0);
            *value = range.start() + t * range_span;
            changed = true;
        }
    }

    if resp.double_clicked() {
        *value = 0.0;
        changed = true;
    }

    VerticalSliderResponse {
        changed,
        drag_started,
        drag_ended,
    }
}
