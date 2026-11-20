//! Klipp's colour field: a swatch that opens a picker with the channel
//! numbers and egui's saturation/value square, hue and alpha bars.
//!
//! egui's own colour popup keeps the colour as a hue, saturation and value
//! and turns it back into bytes every frame, so a typed channel can be
//! stored one off (R 240 stored as 241) and a channel nobody touched can
//! drift by one. Here the bytes are the value: a typed channel is stored as
//! typed, the square and bars write only while they are being moved, and
//! opening the popup writes nothing.
//!
//! The drawing of the swatch, the square and the bars follows egui's
//! `widgets/color_picker.rs` (egui 0.34, MIT OR Apache-2.0), so the field
//! looks as it did.

use crate::colour32::Colour029;
use egui::ecolor::{Color32, Hsva, HsvaGamma, Rgba};
use egui::epaint::{Mesh, Shape, Stroke, StrokeKind};
use egui::{
    lerp, pos2, remap_clamp, vec2, Align2, Id, RectAlign, Painter, Popup, PopupCloseBehavior, Rect,
    Response, Sense, Ui, Vec2, Widget as _, WidgetInfo, WidgetType,
};

/// The picker's width, as egui's.
const SLIDER_WIDTH: f32 = 275.0;

/// Vertices per side of the square and along the bars, as egui's.
const N: u32 = 6 * 6;

/// The colour field on any `Ui`, one method for each colour shape Klipp
/// stores. Each returns the swatch's response, changed when the colour was.
pub trait ColourField {
    /// A colour as egui's `Color32` (premultiplied), with alpha.
    fn colour_srgba(&mut self, colour: &mut Color32) -> Response;
    /// sRGB bytes without alpha.
    fn colour_srgb(&mut self, colour: &mut [u8; 3]) -> Response;
    /// sRGB bytes with alpha, unmultiplied.
    fn colour_srgba_unmultiplied(&mut self, colour: &mut [u8; 4]) -> Response;
    /// Linear-light floats with alpha, unmultiplied.
    fn colour_rgba_unmultiplied(&mut self, colour: &mut [f32; 4]) -> Response;
}

impl ColourField for Ui {
    fn colour_srgba(&mut self, colour: &mut Color32) -> Response {
        let mut bytes = colour.unmul();
        let response = edit(self, &mut bytes, true);
        if response.changed() {
            *colour = crate::colour32::rgba(bytes[0], bytes[1], bytes[2], bytes[3]);
        }
        response
    }

    fn colour_srgb(&mut self, colour: &mut [u8; 3]) -> Response {
        let mut bytes = [colour[0], colour[1], colour[2], 255];
        let response = edit(self, &mut bytes, false);
        if response.changed() {
            colour.copy_from_slice(&bytes[..3]);
        }
        response
    }

    fn colour_srgba_unmultiplied(&mut self, colour: &mut [u8; 4]) -> Response {
        edit(self, colour, true)
    }

    fn colour_rgba_unmultiplied(&mut self, colour: &mut [f32; 4]) -> Response {
        let [r, g, b, a] = *colour;
        let mut bytes = Rgba::from_rgba_unmultiplied(r, g, b, a).to_srgba_unmultiplied();
        let response = edit(self, &mut bytes, true);
        if response.changed() {
            *colour = Rgba::from_srgba_unmultiplied(bytes[0], bytes[1], bytes[2], bytes[3])
                .to_rgba_unmultiplied();
        }
        response
    }
}

/// The swatch and its popup over `value`, unmultiplied sRGB bytes.
fn edit(ui: &mut Ui, value: &mut [u8; 4], alpha: bool) -> Response {
    let popup_id = ui.auto_id_with("popup");
    let open = Popup::is_id_open(ui.ctx(), popup_id);
    let mut button = swatch(ui, shown(*value), open);
    if ui.style().explanation_tooltips {
        button = button.on_hover_text("Click to edit color");
    }
    let mut changed = false;
    // A plain popup, not a menu: a menu's style takes the frames off the
    // channel boxes. It opens below and right of the swatch, as before.
    Popup::from_toggle_button_response(&button)
        .id(popup_id)
        .align(RectAlign { parent: Align2::RIGHT_BOTTOM, child: Align2::LEFT_TOP })
        .close_behavior(PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.spacing_mut().slider_width = SLIDER_WIDTH;
            changed = picker(ui, popup_id.with("hsv"), value, alpha);
        });
    if changed {
        button.mark_changed();
    }
    button
}

fn shown([r, g, b, a]: [u8; 4]) -> Color32 {
    crate::colour32::rgba(r, g, b, a)
}

/// The popup's contents. The hue, saturation and value the square and bars
/// show are kept between frames while the bytes stay what they were, so a
/// grey keeps its hue; they are taken again from the bytes otherwise.
fn picker(ui: &mut Ui, key: Id, value: &mut [u8; 4], alpha: bool) -> bool {
    let from_bytes = |v: [u8; 4]| HsvaGamma::from(Hsva::from_srgba_unmultiplied(v));
    let mut hsv = ui
        .data(|d| d.get_temp::<(HsvaGamma, [u8; 4])>(key))
        .filter(|(_, seen)| seen == value)
        .map_or_else(|| from_bytes(*value), |(h, _)| h);
    let mut changed = false;
    ui.vertical(|ui| {
        let mut bytes = *value;
        let typed = ui
            .horizontal(|ui| {
                let [r, g, b, a] = bytes;
                if ui
                    .button("📋")
                    .on_hover_text("Click to copy color values")
                    .clicked()
                {
                    ui.copy_text(if alpha {
                        format!("{r}, {g}, {b}, {a}")
                    } else {
                        format!("{r}, {g}, {b}")
                    });
                }
                let channels = if alpha { 4 } else { 3 };
                let mut typed = false;
                for (byte, prefix) in bytes
                    .iter_mut()
                    .zip(["R ", "G ", "B ", "A "])
                    .take(channels)
                {
                    typed |= super::number_field(byte)
                        .speed(0.5)
                        .prefix(prefix)
                        .ui(ui)
                        .changed();
                }
                typed
            })
            .inner;
        if typed && bytes != *value {
            *value = bytes;
            hsv = from_bytes(bytes);
            changed = true;
        }

        let size = vec2(ui.spacing().slider_width, ui.spacing().interact_size.y);
        let (rect, _) = ui.allocate_at_least(size, Sense::hover());
        if ui.is_rect_visible(rect) {
            show_colour_at(ui.painter(), shown(*value), rect);
        }

        let opaque = HsvaGamma { a: 1.0, ..hsv };
        let mut moved = slider_2d(ui, &mut hsv.s, &mut hsv.v, |s, v| {
            HsvaGamma { s, v, ..opaque }.into()
        });
        moved |= slider_1d(ui, &mut hsv.h, |h| {
            HsvaGamma {
                h,
                s: 1.0,
                v: 1.0,
                a: 1.0,
            }
            .into()
        })
        .on_hover_text("Hue")
        .changed();
        if alpha {
            moved |= slider_1d(ui, &mut hsv.a, |a| HsvaGamma { a, ..opaque }.into())
                .on_hover_text("Alpha")
                .changed();
        }
        if moved {
            let mut bytes = Hsva::from(hsv).to_srgba_unmultiplied();
            if !alpha {
                bytes[3] = value[3];
            }
            if bytes != *value {
                *value = bytes;
                changed = true;
            }
        }
    });
    ui.data_mut(|d| d.insert_temp(key, (hsv, *value)));
    changed
}

/// The swatch, drawn as egui's colour button.
fn swatch(ui: &mut Ui, colour: Color32, open: bool) -> Response {
    let size = ui.spacing().interact_size;
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    response.widget_info(|| WidgetInfo::new(WidgetType::ColorButton));
    if ui.is_rect_visible(rect) {
        let visuals = if open {
            &ui.visuals().widgets.open
        } else {
            ui.style().interact(&response)
        };
        let rect = rect.expand(visuals.expansion);
        let stroke_width = 1.0;
        show_colour_at(ui.painter(), colour, rect.shrink(stroke_width));
        // The checkers don't round, so the corners can't round much more.
        let corner_radius = visuals.corner_radius.at_most(2);
        ui.painter().rect_stroke(
            rect,
            corner_radius,
            (stroke_width, visuals.bg_fill),
            StrokeKind::Inside,
        );
    }
    response
}

fn contrast(colour: impl Into<Rgba>) -> Color32 {
    if colour.into().intensity() < 0.5 {
        Color32::WHITE
    } else {
        Color32::BLACK
    }
}

fn checkers(painter: &Painter, rect: Rect) {
    let rect = rect.shrink(0.5);
    if !rect.is_positive() {
        return;
    }
    let cell = Vec2::splat(rect.height() / 2.0);
    let n = (rect.width() / cell.x).round() as u32;
    let mut mesh = Mesh::default();
    mesh.add_colored_rect(rect, Color32::from_gray(32));
    let mut top = true;
    for i in 0..n {
        let x = lerp(rect.left()..=rect.right(), i as f32 / n as f32);
        let y = if top { rect.top() } else { rect.center().y };
        mesh.add_colored_rect(
            Rect::from_min_size(pos2(x, y), cell),
            Color32::from_gray(128),
        );
        top = !top;
    }
    painter.add(Shape::mesh(mesh));
}

/// A colour over checkers: a see-through colour shows its opaque half too.
fn show_colour_at(painter: &Painter, colour: Color32, rect: Rect) {
    if colour.is_opaque() {
        painter.rect_filled(rect, 0.0, colour);
        return;
    }
    checkers(painter, rect);
    if colour != Color32::TRANSPARENT {
        painter.rect_filled(
            Rect::from_min_max(rect.left_top(), rect.center_bottom()),
            0.0,
            colour,
        );
        painter.rect_filled(
            Rect::from_min_max(rect.center_top(), rect.right_bottom()),
            0.0,
            colour.to_opaque(),
        );
    }
}

/// A bar over 0..=1; changed while pressed or dragged on.
fn slider_1d(ui: &mut Ui, value: &mut f32, colour_at: impl Fn(f32) -> Color32) -> Response {
    let size = vec2(ui.spacing().slider_width, ui.spacing().interact_size.y);
    let (rect, mut response) = ui.allocate_at_least(size, Sense::click_and_drag());
    if let Some(p) = response.interact_pointer_pos() {
        *value = remap_clamp(p.x, rect.left()..=rect.right(), 0.0..=1.0);
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        checkers(ui.painter(), rect);
        let mut mesh = Mesh::default();
        for i in 0..=N {
            let t = i as f32 / N as f32;
            let colour = colour_at(t);
            let x = lerp(rect.left()..=rect.right(), t);
            mesh.colored_vertex(pos2(x, rect.top()), colour);
            mesh.colored_vertex(pos2(x, rect.bottom()), colour);
            if i < N {
                mesh.add_triangle(2 * i, 2 * i + 1, 2 * i + 2);
                mesh.add_triangle(2 * i + 1, 2 * i + 2, 2 * i + 3);
            }
        }
        ui.painter().add(Shape::mesh(mesh));
        ui.painter()
            .rect_stroke(rect, 0.0, visuals.bg_stroke, StrokeKind::Inside);
        let x = lerp(rect.left()..=rect.right(), *value);
        let r = rect.height() / 4.0;
        let picked = colour_at(*value);
        ui.painter().add(Shape::convex_polygon(
            vec![
                pos2(x, rect.center().y),
                pos2(x + r, rect.bottom()),
                pos2(x - r, rect.bottom()),
            ],
            picked,
            Stroke::new(visuals.fg_stroke.width, contrast(picked)),
        ));
    }
    response
}

/// The saturation/value square; true while pressed or dragged on.
fn slider_2d(
    ui: &mut Ui,
    x_value: &mut f32,
    y_value: &mut f32,
    colour_at: impl Fn(f32, f32) -> Color32,
) -> bool {
    let (rect, response) = ui.allocate_at_least(
        Vec2::splat(ui.spacing().slider_width),
        Sense::click_and_drag(),
    );
    let mut moved = false;
    if let Some(p) = response.interact_pointer_pos() {
        *x_value = remap_clamp(p.x, rect.left()..=rect.right(), 0.0..=1.0);
        *y_value = remap_clamp(p.y, rect.bottom()..=rect.top(), 0.0..=1.0);
        moved = true;
    }
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let mut mesh = Mesh::default();
        for xi in 0..=N {
            for yi in 0..=N {
                let (xt, yt) = (xi as f32 / N as f32, yi as f32 / N as f32);
                mesh.colored_vertex(
                    pos2(
                        lerp(rect.left()..=rect.right(), xt),
                        lerp(rect.bottom()..=rect.top(), yt),
                    ),
                    colour_at(xt, yt),
                );
                if xi < N && yi < N {
                    let tl = yi * (N + 1) + xi;
                    mesh.add_triangle(tl, tl + 1, tl + N + 1);
                    mesh.add_triangle(tl + 1, tl + N + 1, tl + N + 2);
                }
            }
        }
        ui.painter().add(Shape::mesh(mesh));
        ui.painter()
            .rect_stroke(rect, 0.0, visuals.bg_stroke, StrokeKind::Inside);
        let x = lerp(rect.left()..=rect.right(), *x_value);
        let y = lerp(rect.bottom()..=rect.top(), *y_value);
        let picked = colour_at(*x_value, *y_value);
        ui.painter().add(egui::epaint::CircleShape {
            center: pos2(x, y),
            radius: rect.width() / 12.0,
            fill: picked,
            stroke: Stroke::new(visuals.fg_stroke.width, contrast(picked)),
        });
    }
    moved
}

#[cfg(test)]
mod tests {
    /// egui's own colour buttons store a typed channel one off; every
    /// colour in Klipp goes through this field instead.
    #[test]
    fn no_colour_goes_through_eguis_own_popup() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        let mut dirs = vec![src];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && !path.ends_with("widgets/colour_field.rs")
                {
                    let text = std::fs::read_to_string(&path).unwrap();
                    for (n, line) in text.lines().enumerate() {
                        if line.contains("color_edit_button")
                            || line.contains("color_picker::color_")
                        {
                            found.push(format!("{}:{}", path.display(), n + 1));
                        }
                    }
                }
            }
        }
        assert!(
            found.is_empty(),
            "use widgets::colour_field::ColourField instead: {found:#?}"
        );
    }
}
