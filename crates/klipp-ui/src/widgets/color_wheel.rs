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
    theme: &KlippTheme,
    label: &str,
    offset: &mut [f32; 2],
    master: &mut f32,
) -> WheelResponse {
    color_wheel_sized(ui, state, theme, label, offset, master, 108.0, None)
}

/// Like `color_wheel` but with a custom diameter (in pixels).
pub fn color_wheel_sized(
    ui: &mut egui::Ui,
    state: &mut ColorWheelState,
    theme: &KlippTheme,
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
    let text_accent = theme.accent;
    let painter = ui.painter_at(full_rect);
    painter.text(
        egui::pos2(center_x, full_rect.min.y + label_h / 2.0),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(10.0),
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

    // A drag moves the puck from where it is, at half the wheel's scale
    // (a full radius of travel is half the range) and a tenth of that
    // with Shift held: grading moves are small — a lift of 0.01 is a
    // third of a pixel at the wheel's own scale — and the puck must not
    // jump to the pointer on the first press. A click still places it.
    let fine = ui.input(|i| i.modifiers.shift);
    let wheel_resp = wheel_resp.on_hover_text("Drag to adjust, Shift for fine steps.\nClick to place, double-click to reset.");
    if wheel_resp.dragged() {
        let d = wheel_resp.drag_delta();
        if d != egui::Vec2::ZERO {
            let rate = 1.0 / wheel_radius * if fine { 0.1 } else { 1.0 };
            let mut ox = offset[0] + d.x * rate;
            let mut oy = offset[1] + d.y * rate;
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
    } else if wheel_resp.clicked() {
        if let Some(pos) = wheel_resp.interact_pointer_pos() {
            let dx = pos.x - wc.x;
            let dy = pos.y - wc.y;
            // Scale: puck at outer edge = offset 2.0
            let scale = 1.0 / (wheel_radius * 0.5);
            let mut ox = dx * scale;
            let mut oy = dy * scale;
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

    let neutral_text = theme.text_dim;

    // The pills are the wheel's only numeric surface, and they used to
    // be `painter.text` — you could read a channel but not set one.
    // Matching two shots means typing `+0.043`, not dragging a puck at
    // it by eye, so each pill is now a `DragValue`: drag to scrub one
    // channel, double-click to type, select and copy.
    //
    // `scrubby_param_row`, which the issue suggested reusing, is a
    // 22 px inspector row — label, range fill, value, reset icon,
    // keyframe diamond. It cannot live in a 37x13 pill. Its useful
    // half is `DragValue`, which is what this uses directly.
    //
    // What a pill shows is derived: `wheel_offset_to_rgb` of the
    // puck and the master. So setting one channel means rebuilding the
    // triple and running `rgb_to_wheel_offset` back to a puck position
    // — the inverse that already exists, with a round-trip test. A
    // triple whose spread is wider than the rim can reach clamps to the
    // rim, so a typed extreme lands where the wheel can actually go and
    // the puck shows you that.
    let mut rgb = [rgb_r, rgb_g, rgb_b];
    let mut rgb_edited = false;

    for (i, &(val, border_c, text_c)) in pill_colors.iter().enumerate() {
        let px = pills_left + i as f32 * (pill_w + pill_gap);
        let pill_rect = egui::Rect::from_min_size(
            egui::pos2(px, rgb_top),
            egui::vec2(pill_w, pill_h),
        );
        let bg = theme.bg_deep;
        let is_zero = val.abs() < 0.005;
        // The channel's own colour, dimmed at rest rather than dropped.
        //
        // At neutral all three pills fell back to the panel border and
        // became indistinguishable: three identical "+0.000" boxes with
        // nothing saying which was red. Since a wheel sits at neutral
        // most of the time, that is most of the time. Tinted faintly
        // instead, so the channels are identifiable without shouting.
        let border = if is_zero {
            egui::Color32::from_rgba_unmultiplied(border_c.r(), border_c.g(), border_c.b(), 90)
        } else {
            border_c
        };
        let text = if is_zero { neutral_text } else { text_c };
        painter.rect_filled(pill_rect, 2.0, bg);
        painter.rect_stroke(pill_rect, 2.0, egui::Stroke::new(0.5_f32, border));

        // The pill's own paint stays; the DragValue sits on it with no
        // fill or stroke of its own, so the channel colouring survives
        // and the field is the whole pill.
        let mut v = val;
        // Three decimals where they fit: a fine step is 0.001.
        let decimals = if pill_w >= 32.0 { 3 } else { 2 };
        let resp = {
            let mut pill_ui = ui.new_child(
                egui::UiBuilder::new().max_rect(pill_rect).layout(
                    egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                ),
            );
            let st = pill_ui.style_mut();
            st.spacing.interact_size = egui::vec2(pill_w, pill_h);
            st.spacing.button_padding = egui::vec2(0.0, 0.0);
            st.override_font_id = Some(egui::FontId::monospace(pill_font_size));
            for w in [
                &mut st.visuals.widgets.inactive,
                &mut st.visuals.widgets.hovered,
                &mut st.visuals.widgets.active,
                &mut st.visuals.widgets.open,
            ] {
                w.weak_bg_fill = egui::Color32::TRANSPARENT;
                w.bg_fill = egui::Color32::TRANSPARENT;
                w.bg_stroke = egui::Stroke::NONE;
                w.fg_stroke.color = text;
                w.rounding = egui::Rounding::same(2.0);
            }
            // Hover needs to say "this is a field" without repainting
            // the border the channel colour is carried by.
            st.visuals.widgets.hovered.weak_bg_fill = theme.bg_hover;
            pill_ui.add(
                egui::DragValue::new(&mut v)
                    .speed(0.002)
                    .range(-1.0..=1.0)
                    // The sign is always shown, as the painted pills
                    // showed it and as the master slider beneath still
                    // does: these are offsets from neutral, and "+0.08"
                    // reads as a direction where "0.08" reads as a
                    // level. `fixed_decimals` alone drops the plus.
                    .custom_formatter(move |n, _| format!("{n:+.decimals$}"))
                    // And accepted back, so a value can be copied out of
                    // one pill and pasted into another.
                    .custom_parser(|text| text.trim().trim_start_matches('+').parse().ok()),
            )
        };
        if resp.drag_started() {
            drag_started = true;
        }
        if resp.changed() {
            rgb[i] = v;
            rgb_edited = true;
        }
        if resp.drag_stopped() || resp.lost_focus() {
            drag_ended = true;
        }
    }

    if rgb_edited {
        let (x, y, m) = klipp_color::rgb_to_wheel_offset((rgb[0], rgb[1], rgb[2]));
        offset[0] = x;
        offset[1] = y;
        *master = m;
        changed = true;
    }

    // ── Master slider ──
    let master_top = rgb_top + rgb_row_h + spacing;
    let m_label_w = 10.0;
    let m_value_w = if wheel_diameter < 65.0 { 24.0 } else { 30.0 };
    let track_left = center_x - pills_total / 2.0 + m_label_w + 2.0;
    let track_w = (pills_total - m_label_w - m_value_w - 4.0).max(20.0);
    let track_y = master_top + master_row_h / 2.0;
    let track_bg = theme.bg_raised;

    // "M" label
    painter.text(
        egui::pos2(center_x - pills_total / 2.0, track_y),
        egui::Align2::LEFT_CENTER,
        // "Y" for the luma master. "M" on a colour wheel reads as
        // Magenta first — an ambiguity the one place it should not be.
        "Y",
        egui::FontId::proportional(10.0),
        theme.text_dim,
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

    // The same as the wheel: a drag moves the thumb from where it is,
    // half the track's scale and a tenth with Shift; a click places it.
    if master_resp.dragged() {
        let dx = master_resp.drag_delta().x;
        if dx != 0.0 {
            let rate = 1.0 / track_w * if fine { 0.1 } else { 1.0 };
            *master = (*master + dx * rate).clamp(-1.0, 1.0);
            changed = true;
        }
    } else if master_resp.clicked() {
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
        format!("{master:+.3}"),
        egui::FontId::monospace(master_font),
        val_color,
    );

    WheelResponse {
        changed,
        drag_started,
        drag_ended,
    }
}
