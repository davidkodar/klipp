use crate::theme::KlippTheme;

#[derive(Default)]
pub struct ColorWheelState {
    texture: Option<egui::TextureHandle>,
}

pub struct WheelResponse {
    pub changed: bool,
    pub drag_started: bool,
    pub drag_ended: bool,
}

/// Renders a color wheel with puck, RGB readout, and master slider.
/// Returns WheelResponse indicating interaction state.
pub fn color_wheel(
    ui: &mut egui::Ui,
    state: &mut ColorWheelState,
    _theme: &KlippTheme,
    label: &str,
    offset: &mut [f32; 2],
    master: &mut f32,
) -> WheelResponse {
    color_wheel_sized(ui, state, _theme, label, offset, master, 108.0, None)
}

/// Like `color_wheel` but with a custom diameter (in pixels).
pub fn color_wheel_sized(
    ui: &mut egui::Ui,
    state: &mut ColorWheelState,
    _theme: &KlippTheme,
    label: &str,
    offset: &mut [f32; 2],
    master: &mut f32,
    wheel_diameter: f32,
    forced_width: Option<f32>,
) -> WheelResponse {
    let mut changed = false;
    let mut drag_started = false;
    let mut drag_ended = false;
    let wheel_radius = wheel_diameter / 2.0;
    let rgb_row_h = 16.0;
    let master_row_h = 14.0;
    let label_h = 14.0;
    let spacing = 4.0;

    // Pre-compute pill dimensions so widget width fits them
    let pill_font_size = if wheel_diameter < 65.0 { 7.0 } else { 8.0 };
    let pill_w = (wheel_diameter * 0.34).max(28.0);
    let pill_gap = 2.0;
    let pills_total_w = pill_w * 3.0 + pill_gap * 2.0;

    let total_h = label_h + wheel_diameter + spacing + rgb_row_h + spacing + master_row_h;
    let total_w = forced_width.unwrap_or_else(|| pills_total_w.max(wheel_diameter + 8.0) + 4.0);

    let (full_rect, _) = ui.allocate_exact_size(
        egui::vec2(total_w, total_h),
        egui::Sense::hover(),
    );

    let center_x = full_rect.center().x;

    // ── Label ──
    let text_accent = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);
    let painter = ui.painter_at(full_rect);
    painter.text(
        egui::pos2(center_x, full_rect.min.y + label_h / 2.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(9.0),
        text_accent,
    );

    // ── Wheel texture ──
    let wheel_top = full_rect.min.y + label_h;
    let wheel_rect = egui::Rect::from_min_size(
        egui::pos2(center_x - wheel_radius, wheel_top),
        egui::vec2(wheel_diameter, wheel_diameter),
    );

    // Generate texture once (use a fixed high-res texture, scaled to display size)
    if state.texture.is_none() {
        let tex_radius = 54u32;
        let pixels = klipp_color::generate_wheel_texture(tex_radius);
        let size = (tex_radius * 2) as usize;
        let image = egui::ColorImage::from_rgba_unmultiplied([size, size], &pixels);
        state.texture = Some(crate::app::alloc_texture_logged(ui.ctx(), "color_wheel", image, egui::TextureOptions::LINEAR));
    }

    if let Some(tex) = &state.texture {
        painter.image(
            tex.id(),
            wheel_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    // ── Guide lines ──
    let guide_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 20);
    let guide_stroke = egui::Stroke::new(0.5_f32, guide_color);
    let wc = wheel_rect.center();
    // Concentric circles at 33% and 66% radius
    painter.circle_stroke(wc, wheel_radius * 0.33, guide_stroke);
    painter.circle_stroke(wc, wheel_radius * 0.66, guide_stroke);
    // Crosshair
    painter.line_segment([egui::pos2(wc.x - wheel_radius, wc.y), egui::pos2(wc.x + wheel_radius, wc.y)], guide_stroke);
    painter.line_segment([egui::pos2(wc.x, wc.y - wheel_radius), egui::pos2(wc.x, wc.y + wheel_radius)], guide_stroke);
    // Outer ring
    // Outer ring removed — texture has feathered antialiased edge

    // ── Wheel interaction ──
    let wheel_resp = ui.allocate_rect(wheel_rect, egui::Sense::click_and_drag());
    drag_started |= wheel_resp.drag_started();
    drag_ended |= wheel_resp.drag_stopped();

    if wheel_resp.dragged() || wheel_resp.clicked() {
        if let Some(pos) = wheel_resp.interact_pointer_pos() {
            let dx = pos.x - wc.x;
            let dy = pos.y - wc.y;
            // Scale: puck at outer edge = offset 2.0
            let scale = 1.0 / (wheel_radius * 0.5);
            let mut ox = dx * scale;
            let mut oy = dy * scale;
            // Clamp to full wheel radius (offset 2.0 = outer edge)
            let mag = (ox * ox + oy * oy).sqrt();
            if mag > 2.0 {
                ox = ox / mag * 2.0;
                oy = oy / mag * 2.0;
            }
            offset[0] = ox;
            offset[1] = oy;
            changed = true;
        }
    }

    if wheel_resp.double_clicked() {
        offset[0] = 0.0;
        offset[1] = 0.0;
        *master = 0.0;
        changed = true;
    }

    // Right-click: reset wheel + master to neutral
    if wheel_resp.secondary_clicked() {
        offset[0] = 0.0;
        offset[1] = 0.0;
        *master = 0.0;
        changed = true;
    }

    // ── Puck ──
    let puck_x = wc.x + offset[0] * wheel_radius * 0.5;
    let puck_y = wc.y + offset[1] * wheel_radius * 0.5;
    let puck_center = egui::pos2(puck_x, puck_y);
    let puck_r = (wheel_radius * 0.09).clamp(4.0, 6.0);
    let shadow_off = (wheel_radius * 0.015).max(1.0);
    // Shadow
    painter.circle_filled(
        egui::pos2(puck_x + shadow_off, puck_y + shadow_off),
        puck_r,
        egui::Color32::from_rgba_unmultiplied(0, 0, 0, 90),
    );
    // Puck body
    painter.circle_filled(puck_center, puck_r, egui::Color32::WHITE);
    painter.circle_stroke(puck_center, puck_r, egui::Stroke::new(1.0_f32, egui::Color32::from_rgba_unmultiplied(0, 0, 0, 140)));

    // ── RGB readout ──
    let rgb_top = wheel_rect.max.y + spacing;
    let (rgb_r, rgb_g, rgb_b) = klipp_color::wheel_offset_to_rgb(offset[0], offset[1], *master);

    let pill_h = if wheel_diameter < 65.0 { 12.0 } else { 13.0 };
    let pills_total = pills_total_w;
    let pills_left = center_x - pills_total / 2.0;

    let pill_colors: [(f32, egui::Color32, egui::Color32); 3] = [
        (rgb_r, egui::Color32::from_rgb(0xa8, 0x40, 0x40), egui::Color32::from_rgb(0xe0, 0x70, 0x70)),
        (rgb_g, egui::Color32::from_rgb(0x40, 0x70, 0x40), egui::Color32::from_rgb(0x70, 0xc0, 0x70)),
        (rgb_b, egui::Color32::from_rgb(0x40, 0x40, 0x90), egui::Color32::from_rgb(0x70, 0x70, 0xe0)),
    ];

    let neutral_text = egui::Color32::from_rgb(0x90, 0x90, 0xa0);

    for (i, &(val, border_c, text_c)) in pill_colors.iter().enumerate() {
        let px = pills_left + i as f32 * (pill_w + pill_gap);
        let pill_rect = egui::Rect::from_min_size(
            egui::pos2(px, rgb_top),
            egui::vec2(pill_w, pill_h),
        );
        let bg = egui::Color32::from_rgb(0x16, 0x16, 0x1a);
        let is_zero = val.abs() < 0.005;
        let border = if is_zero { egui::Color32::from_rgb(0x2a, 0x2a, 0x32) } else { border_c };
        let text = if is_zero { neutral_text } else { text_c };
        painter.rect_filled(pill_rect, 3.0, bg);
        painter.rect_stroke(pill_rect, 3.0, egui::Stroke::new(0.5_f32, border));
        painter.text(
            pill_rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{val:+.2}"),
            egui::FontId::monospace(pill_font_size),
            text,
        );
    }

    // ── Master slider ──
    let master_top = rgb_top + rgb_row_h + spacing;
    let m_label_w = 10.0;
    let m_value_w = if wheel_diameter < 65.0 { 24.0 } else { 30.0 };
    let track_left = center_x - pills_total / 2.0 + m_label_w + 2.0;
    let track_w = (pills_total - m_label_w - m_value_w - 4.0).max(20.0);
    let track_y = master_top + master_row_h / 2.0;
    let track_bg = egui::Color32::from_rgb(0x2a, 0x2a, 0x32);

    // "M" label
    painter.text(
        egui::pos2(center_x - pills_total / 2.0, track_y),
        egui::Align2::LEFT_CENTER,
        "M",
        egui::FontId::proportional(9.0),
        egui::Color32::from_rgb(0x50, 0x50, 0x60),
    );

    // Track
    let track_rect = egui::Rect::from_min_size(
        egui::pos2(track_left, track_y - 1.5),
        egui::vec2(track_w, 3.0),
    );
    painter.rect_filled(track_rect, 2.0, track_bg);

    // Master slider interaction
    let master_hit = egui::Rect::from_min_size(
        egui::pos2(track_left - 4.0, master_top),
        egui::vec2(track_w + 8.0, master_row_h),
    );
    let master_resp = ui.allocate_rect(master_hit, egui::Sense::click_and_drag());
    drag_started |= master_resp.drag_started();
    drag_ended |= master_resp.drag_stopped();

    if master_resp.dragged() || master_resp.clicked() {
        if let Some(pos) = master_resp.interact_pointer_pos() {
            let t = ((pos.x - track_left) / track_w).clamp(0.0, 1.0);
            *master = t * 2.0 - 1.0; // map 0..1 to -1..1
            changed = true;
        }
    }
    if master_resp.double_clicked() || master_resp.secondary_clicked() {
        *master = 0.0;
        changed = true;
    }

    // Thumb
    let master_t = (*master + 1.0) / 2.0; // -1..1 to 0..1
    let thumb_x = track_left + master_t * track_w;
    painter.circle_filled(egui::pos2(thumb_x, track_y), 4.0, text_accent);

    // Value (compact format for small wheels)
    let is_default = master.abs() < 0.005;
    let val_color = if is_default { neutral_text } else { text_accent };
    let master_font = if wheel_diameter < 65.0 { 7.0 } else { 8.0 };
    painter.text(
        egui::pos2(center_x + pills_total / 2.0, track_y),
        egui::Align2::RIGHT_CENTER,
        format!("{master:+.2}"),
        egui::FontId::monospace(master_font),
        val_color,
    );

    WheelResponse {
        changed,
        drag_started,
        drag_ended,
    }
}
