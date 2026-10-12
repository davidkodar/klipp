//! The gradient editor (#1509): a fill's kind, its angle or its centre
//! and radius, and its stops. A click on the bar adds a stop in the
//! colour the bar has there; a stop's handle is dragged along the bar,
//! between its neighbours, so the stops keep their order; the picked
//! stop takes a colour, a position typed in, or its removal while more
//! than two are left. A drag, of a handle or of a number, is one undo
//! step; every other edit is one of its own.

use klipp_core::fill_gradient::{GradientKind, GradientRamp, GradientStop, MAX_GRADIENT_STOPS};
use klipp_core::{AppCommand, ClipId};
use klipp_timeline::TextGradient;

/// The editor for `gradient`, on the title's text when `object_id` is
/// `None`, else on that shape. Edits go out as `SetTitleGradientStops`,
/// switched on.
pub(super) fn show(
    ui: &mut egui::Ui,
    clip_id: ClipId,
    object_id: Option<u32>,
    gradient: &TextGradient,
    cmds: &mut Vec<AppCommand>,
) {
    let mut stops = gradient.stops();
    let mut kind = gradient.kind;
    let mut changed = false;
    let (mut drag_started, mut drag_stopped) = (false, false);
    // A number's edit, and whether a drag of it began or ended.
    let track = |r: &egui::Response, changed: &mut bool, started: &mut bool, stopped: &mut bool| {
        *started |= r.drag_started();
        *stopped |= r.drag_stopped();
        *changed |= r.changed();
    };
    let selected_id = ui.id().with(("gradient_stop", clip_id.0, object_id));
    let mut selected = ui.ctx().data(|d| d.get_temp::<usize>(selected_id)).unwrap_or(0).min(stops.len() - 1);

    // Kind, and its geometry.
    ui.horizontal(|ui| {
        let linear = matches!(kind, GradientKind::Linear { .. });
        if ui.selectable_label(linear, "Linear").clicked() && !linear {
            kind = GradientKind::Linear { angle_deg: 0.0 };
            changed = true;
        }
        if ui.selectable_label(!linear, "Radial").clicked() && linear {
            kind = GradientKind::Radial { centre: [0.5, 0.5], radius: 0.5 };
            changed = true;
        }
    });
    ui.horizontal(|ui| match &mut kind {
        GradientKind::Linear { angle_deg } => {
            ui.label("Angle");
            let r = ui.add(egui::DragValue::new(angle_deg).range(-360.0..=360.0).speed(1.0).suffix("°").fixed_decimals(0));
            track(&r, &mut changed, &mut drag_started, &mut drag_stopped);
        }
        GradientKind::Radial { centre, radius } => {
            ui.label("Centre");
            let r = ui.add(egui::DragValue::new(&mut centre[0]).range(-1.0..=2.0).speed(0.005).fixed_decimals(2));
            track(&r, &mut changed, &mut drag_started, &mut drag_stopped);
            let r = ui.add(egui::DragValue::new(&mut centre[1]).range(-1.0..=2.0).speed(0.005).fixed_decimals(2));
            track(&r, &mut changed, &mut drag_started, &mut drag_stopped);
            ui.label("Radius");
            let r = ui.add(egui::DragValue::new(radius).range(0.01..=2.0).speed(0.005).fixed_decimals(2));
            track(&r, &mut changed, &mut drag_started, &mut drag_stopped);
        }
    });

    // The bar, and a handle under it for each stop.
    let width = ui.available_width().max(80.0);
    let (bar, bar_resp) = ui.allocate_exact_size(egui::vec2(width, 18.0), egui::Sense::click());
    let (handles, _) = ui.allocate_exact_size(egui::vec2(width, 12.0), egui::Sense::hover());
    let ramp = GradientRamp::new(&stops);
    let painter = ui.painter();
    const SEGMENTS: usize = 64;
    for s in 0..SEGMENTS {
        let (a, b) = (s as f32 / SEGMENTS as f32, (s + 1) as f32 / SEGMENTS as f32);
        let c = ramp.at((a + b) * 0.5);
        let r = egui::Rect::from_min_max(egui::pos2(bar.left() + a * bar.width(), bar.top()), egui::pos2(bar.left() + b * bar.width(), bar.bottom()));
        painter.rect_filled(r, 0.0, egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]));
    }
    painter.rect_stroke(bar, 2.0, egui::Stroke::new(1.0_f32, egui::Color32::from_gray(90)));
    for i in 0..stops.len() {
        let x = bar.left() + stops[i].position * bar.width();
        let hit = egui::Rect::from_center_size(egui::pos2(x, handles.center().y), egui::vec2(12.0, 14.0));
        let h = ui.interact(hit, ui.id().with(("gradient_handle", clip_id.0, object_id, i)), egui::Sense::click_and_drag());
        if h.clicked() || h.drag_started() {
            selected = i;
        }
        drag_started |= h.drag_started();
        drag_stopped |= h.drag_stopped();
        if h.dragged() {
            // Between its neighbours, so the stops keep their order.
            let lo = if i == 0 { 0.0 } else { stops[i - 1].position };
            let hi = stops.get(i + 1).map_or(1.0, |s| s.position);
            let p = (stops[i].position + ui.input(|inp| inp.pointer.delta().x) / bar.width()).clamp(lo, hi);
            if p != stops[i].position {
                stops[i].position = p;
                changed = true;
            }
        }
        let c = stops[i].color;
        let tip = egui::pos2(x, handles.top());
        let points = vec![tip, egui::pos2(x - 5.0, handles.bottom()), egui::pos2(x + 5.0, handles.bottom())];
        let edge = if i == selected { egui::Color32::WHITE } else { egui::Color32::from_gray(110) };
        ui.painter().add(egui::Shape::convex_polygon(points, egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], 255), egui::Stroke::new(1.5_f32, edge)));
    }
    if bar_resp.clicked() && stops.len() < MAX_GRADIENT_STOPS {
        if let Some(pos) = bar_resp.interact_pointer_pos() {
            let position = ((pos.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
            let at = stops.partition_point(|s| s.position <= position);
            stops.insert(at, GradientStop { position, color: ramp.at(position) });
            selected = at;
            changed = true;
        }
    }

    // The picked stop.
    ui.horizontal(|ui| {
        let lo = if selected == 0 { 0.0 } else { stops[selected - 1].position };
        let hi = stops.get(selected + 1).map_or(1.0, |s| s.position);
        let stop = &mut stops[selected];
        let mut c = egui::Color32::from_rgba_unmultiplied(stop.color[0], stop.color[1], stop.color[2], stop.color[3]);
        if ui.color_edit_button_srgba(&mut c).changed() {
            stop.color = c.to_srgba_unmultiplied();
            changed = true;
        }
        let mut percent = stop.position * 100.0;
        let r = ui.add(egui::DragValue::new(&mut percent).range(0.0..=100.0).speed(0.5).suffix(" %").fixed_decimals(0));
        if r.changed() {
            stop.position = (percent / 100.0).clamp(lo, hi);
        }
        track(&r, &mut changed, &mut drag_started, &mut drag_stopped);
        if ui.add_enabled(stops.len() > 2, egui::Button::new("Remove")).clicked() {
            stops.remove(selected);
            selected = selected.min(stops.len() - 1);
            changed = true;
        }
    });
    ui.ctx().data_mut(|d| d.insert_temp(selected_id, selected));

    if drag_started {
        cmds.push(AppCommand::BeginTitleGradientDrag { clip_id });
    }
    if changed {
        cmds.push(AppCommand::SetTitleGradientStops { clip_id, object_id, enabled: true, stops, kind });
    }
    if drag_stopped {
        cmds.push(AppCommand::EndTitleGradientDrag { clip_id });
    }
}
