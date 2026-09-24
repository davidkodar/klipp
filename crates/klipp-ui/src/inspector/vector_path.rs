use klipp_core::{AppCommand, ClipId};
use klipp_timeline::{TitleObjectId, VectorPath};

pub(super) fn show(
    ui: &mut egui::Ui,
    clip_id: ClipId,
    object: TitleObjectId,
    path: &VectorPath,
    time: f64,
    tolerance: f64,
    cmds: &mut Vec<AppCommand>,
) {
    let mut next = path.clone();
    let selected = path
        .keyframes
        .iter()
        .enumerate()
        .filter(|(_, k)| (k.time_secs - time).abs() <= tolerance + 1e-6)
        .min_by(|(_, a), (_, b)| {
            (a.time_secs - time)
                .abs()
                .total_cmp(&(b.time_secs - time).abs())
        })
        .map(|(i, _)| i);
    let key_time = selected.map_or(time, |i| path.keyframes[i].time_secs);
    let mut pose = selected.map_or_else(|| path.at(time), |i| path.keyframes[i].pose.clone());
    let mut changed = false;
    let mut direct = false;
    ui.label("Editable path · drag points or handles");
    ui.horizontal(|ui| {
        if ui
            .button(if selected.is_some() {
                "◆ Remove key"
            } else {
                "◇ Key pose"
            })
            .clicked()
        {
            if let Some(i) = selected {
                next.keyframes.remove(i);
            } else if let Err(e) = next.key(time, pose.clone()) {
                ui.label(e);
            }
            direct = true;
        }
        if ui.button("Clear keys").clicked() {
            next.pose = pose.clone();
            next.keyframes.clear();
            direct = true;
        }
    });
    if let Some(i) = selected.filter(|_| !direct) {
        egui::ComboBox::from_id_salt(("path_easing", object.0))
            .selected_text(next.keyframes[i].easing.label())
            .show_ui(ui, |ui| {
                for e in klipp_timeline::Easing::ALL {
                    direct |= ui
                        .selectable_value(&mut next.keyframes[i].easing, e, e.label())
                        .changed();
                }
            });
    }
    let size = egui::vec2(ui.available_width().min(360.0), 220.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let map = |p: [f32; 2]| {
        egui::pos2(
            rect.left() + p[0] * rect.width(),
            rect.top() + p[1] * rect.height(),
        )
    };
    ui.painter()
        .rect_stroke(rect, 0.0, egui::Stroke::new(1.0_f32, egui::Color32::GRAY));
    for (ci, c) in pose.contours.iter_mut().enumerate() {
        let n = if c.closed {
            c.vertices.len()
        } else {
            c.vertices.len().saturating_sub(1)
        };
        for i in 0..n {
            let a = c.vertices[i];
            let b = c.vertices[(i + 1) % c.vertices.len()];
            ui.painter()
                .add(egui::epaint::CubicBezierShape::from_points_stroke(
                    [map(a.point), map(a.outgoing), map(b.incoming), map(b.point)],
                    false,
                    egui::Color32::TRANSPARENT,
                    egui::Stroke::new(1.5_f32, egui::Color32::LIGHT_BLUE),
                ));
        }
        for (vi, v) in c.vertices.iter_mut().enumerate() {
            for p in [v.incoming, v.outgoing] {
                ui.painter().line_segment(
                    [map(v.point), map(p)],
                    egui::Stroke::new(1.0_f32, egui::Color32::GRAY),
                );
            }
            for (hi, p) in [&mut v.incoming, &mut v.outgoing, &mut v.point]
                .into_iter()
                .enumerate()
            {
                let center = map(*p);
                let response = ui.interact(
                    egui::Rect::from_center_size(center, egui::vec2(12.0, 12.0)),
                    ui.id().with((object.0, ci, vi, hi)),
                    egui::Sense::drag(),
                );
                ui.painter().circle_filled(
                    center,
                    if hi == 2 { 4.0 } else { 2.5 },
                    if hi == 2 {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::YELLOW
                    },
                );
                if response.dragged() {
                    let d = ui.input(|i| i.pointer.delta());
                    p[0] = (p[0] + d.x / rect.width()).clamp(-4.0, 5.0);
                    p[1] = (p[1] + d.y / rect.height()).clamp(-4.0, 5.0);
                    changed = true;
                }
            }
        }
        ui.collapsing(
            format!("Contour {} · {} vertices", ci + 1, c.vertices.len()),
            |ui| {
                ui.add_enabled_ui(path.keyframes.is_empty(), |ui| {
                    changed |= ui.checkbox(&mut c.closed, "Closed").changed();
                    if c.vertices.len() < 256 && ui.button("Add vertex").clicked() {
                        let point = [0.5, 0.5];
                        c.vertices.push(klipp_timeline::PathVertex {
                            point,
                            incoming: point,
                            outgoing: point,
                        });
                        changed = true;
                    }
                    if c.vertices.len() > 2 && ui.button("Remove last vertex").clicked() {
                        c.vertices.pop();
                        changed = true;
                    }
                });
                for (i, v) in c.vertices.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("Point {}", i + 1));
                        for p in [&mut v.incoming, &mut v.outgoing, &mut v.point] {
                            for x in p {
                                changed |= ui
                                    .add(egui::DragValue::new(x).speed(0.005).range(-4.0..=5.0))
                                    .changed();
                            }
                        }
                    });
                }
            },
        );
    }
    ui.add_enabled_ui(path.keyframes.is_empty(), |ui| {
        if pose.contours.len() < 16 && ui.button("Add contour").clicked() {
            pose.contours
                .push(VectorPath::default().pose.contours.remove(0));
            changed = true;
        }
        if pose.contours.len() > 1 && ui.button("Remove last contour").clicked() {
            pose.contours.pop();
            changed = true;
        }
    });
    if ui.button("Export path JSON…").clicked() {
        if let Some(file) = rfd::FileDialog::new()
            .add_filter("Editable path", &["json"])
            .save_file()
        {
            if let Err(e) = serde_json::to_string_pretty(path)
                .map_err(|e| e.to_string())
                .and_then(|s| std::fs::write(file, s).map_err(|e| e.to_string()))
            {
                ui.label(e);
            }
        }
    }
    ui.horizontal(|ui| {
        ui.label("Fill");
        changed |= ui
            .color_edit_button_rgba_unmultiplied(&mut pose.fill)
            .changed();
        ui.label("Stroke");
        changed |= ui
            .color_edit_button_rgba_unmultiplied(&mut pose.stroke)
            .changed();
    });
    changed |= ui
        .add(egui::Slider::new(&mut pose.stroke_width, 0.0..=200.0).text("Stroke px @ 1080p"))
        .changed();
    if ui.button("Import path JSON…").clicked() {
        if let Some(file) = rfd::FileDialog::new()
            .add_filter("Editable path", &["json"])
            .pick_file()
        {
            match read_path_file(&file)
                .and_then(|s| serde_json::from_str::<VectorPath>(&s).map_err(|e| e.to_string()))
                .and_then(|p| {
                    p.validate()?;
                    Ok(p)
                }) {
                Ok(p) => {
                    next = p;
                    direct = true;
                }
                Err(e) => {
                    ui.label(e);
                }
            }
        }
    }
    if changed {
        if path.keyframes.is_empty() {
            next.pose = pose;
        } else if let Err(e) = next.key(key_time, pose) {
            ui.label(e);
            return;
        }
    }
    if changed || direct {
        match next.validate() {
            Ok(()) => cmds.push(AppCommand::SetTitleVectorPath {
                clip_id,
                object_id: object.0,
                path: next,
            }),
            Err(e) => {
                ui.label(e);
            }
        }
    }
}

fn read_path_file(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut text = String::new();
    file.take(2_000_001)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > 2_000_000 {
        return Err("Path file exceeds 2 MB".into());
    }
    Ok(text)
}
