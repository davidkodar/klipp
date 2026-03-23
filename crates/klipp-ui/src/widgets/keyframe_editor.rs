use crate::theme::KlippTheme;
use klipp_core::{AppCommand, ColorGradeParam};
use klipp_core::keyframe::{Interpolation, KeyframeTracks};

/// Colour palette for parameter curves in the keyframe editor.
fn param_color(param: ColorGradeParam) -> egui::Color32 {
    match param {
        ColorGradeParam::Exposure => egui::Color32::from_rgb(0xe8, 0xa0, 0x20),
        ColorGradeParam::Contrast => egui::Color32::from_rgb(0x80, 0x80, 0xc0),
        ColorGradeParam::Saturation => egui::Color32::from_rgb(0x60, 0xc0, 0x60),
        ColorGradeParam::Temperature => egui::Color32::from_rgb(0xe0, 0x60, 0x40),
        ColorGradeParam::Tint => egui::Color32::from_rgb(0x40, 0xc0, 0x40),
        ColorGradeParam::Highlights => egui::Color32::from_rgb(0xf0, 0xd0, 0x50),
        ColorGradeParam::Shadows => egui::Color32::from_rgb(0x50, 0x50, 0xc0),
        ColorGradeParam::Whites => egui::Color32::from_rgb(0xf0, 0xf0, 0x80),
        ColorGradeParam::Blacks => egui::Color32::from_rgb(0x40, 0x40, 0x90),
        ColorGradeParam::Hue => egui::Color32::from_rgb(0xc0, 0x40, 0xc0),
        ColorGradeParam::GlobalMix => egui::Color32::from_rgb(0x90, 0x90, 0x90),
        _ => egui::Color32::from_rgb(0x80, 0x80, 0x80),
    }
}

/// Value range for a parameter (min, max) — used for Y axis scaling.
fn param_range(param: ColorGradeParam) -> (f32, f32) {
    match param {
        ColorGradeParam::Exposure => (-5.0, 5.0),
        ColorGradeParam::Contrast => (-1.0, 1.0),
        ColorGradeParam::Saturation => (0.0, 3.0),
        ColorGradeParam::Temperature | ColorGradeParam::Tint => (-100.0, 100.0),
        ColorGradeParam::Highlights | ColorGradeParam::Shadows => (-1.0, 1.0),
        ColorGradeParam::Whites | ColorGradeParam::Blacks => (-1.0, 1.0),
        ColorGradeParam::Hue => (0.0, 360.0),
        ColorGradeParam::GlobalMix | ColorGradeParam::CurvesMix | ColorGradeParam::HueSatMix => (0.0, 1.0),
        _ => (-1.0, 1.0),
    }
}

/// Render the keyframe curve editor.
/// Shows time on X axis, value on Y axis. One coloured curve per parameter track.
/// Returns commands to dispatch.
pub fn show_keyframe_editor(
    ui: &mut egui::Ui,
    _theme: &KlippTheme,
    clip_id: klipp_core::ClipId,
    tracks: &KeyframeTracks,
    clip_duration_secs: f64,
    clip_time_secs: f64,
    height: f32,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();

    let bg = egui::Color32::from_rgb(0x16, 0x16, 0x1a);
    let grid_color = egui::Color32::from_rgb(0x22, 0x22, 0x28);
    let text_dim = egui::Color32::from_rgb(0x50, 0x50, 0x60);
    let playhead_color = egui::Color32::from_rgb(0xe8, 0xa0, 0x20);

    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter_at(rect);

    // Background
    painter.rect_filled(rect, 2.0, bg);

    let duration = clip_duration_secs.max(1.0);
    let pad_x = 40.0; // left margin for value labels
    let pad_y = 16.0; // bottom margin for time labels
    let plot_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + pad_x, rect.min.y + 4.0),
        egui::pos2(rect.max.x - 4.0, rect.max.y - pad_y),
    );

    if plot_rect.width() < 10.0 || plot_rect.height() < 10.0 {
        return cmds;
    }

    // Grid lines (4 vertical, 4 horizontal)
    for i in 0..=4 {
        let t = i as f32 / 4.0;
        let x = plot_rect.min.x + t * plot_rect.width();
        painter.line_segment(
            [egui::pos2(x, plot_rect.min.y), egui::pos2(x, plot_rect.max.y)],
            egui::Stroke::new(0.5, grid_color),
        );
        // Time label
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
            egui::Stroke::new(0.5, grid_color),
        );
    }

    // Playhead vertical line
    let playhead_x = plot_rect.min.x + (clip_time_secs / duration) as f32 * plot_rect.width();
    if playhead_x >= plot_rect.min.x && playhead_x <= plot_rect.max.x {
        painter.line_segment(
            [egui::pos2(playhead_x, plot_rect.min.y), egui::pos2(playhead_x, plot_rect.max.y)],
            egui::Stroke::new(1.5, playhead_color),
        );
    }

    // Convert time/value to screen position
    let to_screen = |time: f64, value: f32, v_min: f32, v_max: f32| -> egui::Pos2 {
        let x = plot_rect.min.x + (time / duration) as f32 * plot_rect.width();
        let t = if (v_max - v_min).abs() > f32::EPSILON {
            (value - v_min) / (v_max - v_min)
        } else {
            0.5
        };
        let y = plot_rect.max.y - t * plot_rect.height(); // Y inverted
        egui::pos2(x.clamp(plot_rect.min.x, plot_rect.max.x), y.clamp(plot_rect.min.y, plot_rect.max.y))
    };

    // Draw each parameter track
    for track in &tracks.tracks {
        if track.keyframes.is_empty() { continue; }

        let color = param_color(track.param);
        let (v_min, v_max) = param_range(track.param);

        // Draw interpolated curve (sample at pixel resolution)
        let num_samples = plot_rect.width() as usize;
        let mut points = Vec::with_capacity(num_samples);
        for i in 0..num_samples {
            let t = i as f64 / num_samples as f64 * duration;
            if let Some(val) = track.evaluate(t) {
                points.push(to_screen(t, val, v_min, v_max));
            }
        }
        if points.len() >= 2 {
            for pair in points.windows(2) {
                painter.line_segment([pair[0], pair[1]], egui::Stroke::new(1.5, color));
            }
        }

        // Draw keyframe diamonds + tangent handles
        for (kf_idx, kf) in track.keyframes.iter().enumerate() {
            let pos = to_screen(kf.time_secs, kf.value, v_min, v_max);
            let ds = 4.0;

            // Diamond shape
            let diamond_pts = vec![
                egui::pos2(pos.x, pos.y - ds),
                egui::pos2(pos.x + ds, pos.y),
                egui::pos2(pos.x, pos.y + ds),
                egui::pos2(pos.x - ds, pos.y),
            ];
            painter.add(egui::Shape::convex_polygon(
                diamond_pts,
                color,
                egui::Stroke::new(1.0, egui::Color32::WHITE),
            ));

            // Diamond drag interaction
            let kf_id = ui.id().with(("kf_drag", track.param as u8, kf_idx));
            let kf_rect = egui::Rect::from_center_size(pos, egui::vec2(14.0, 14.0));
            let kf_resp = ui.interact(kf_rect, kf_id, egui::Sense::click_and_drag());

            if kf_resp.dragged() {
                let d = kf_resp.drag_delta();
                let new_time = kf.time_secs + (d.x as f64 / plot_rect.width() as f64 * duration);
                let new_value = kf.value - d.y / plot_rect.height() * (v_max - v_min);
                let new_time = new_time.clamp(0.0, duration);
                let new_value = new_value.clamp(v_min, v_max);
                // Remove old, add at new position
                cmds.push(AppCommand::RemoveKeyframe {
                    clip_id, param: track.param, time_secs: kf.time_secs,
                });
                cmds.push(AppCommand::AddKeyframe {
                    clip_id, param: track.param, time_secs: new_time, value: new_value,
                });
            }

            // Right-click context menu for interpolation mode
            kf_resp.context_menu(|ui| {
                ui.label(egui::RichText::new("Interpolation").size(10.0).strong());
                if ui.selectable_label(kf.interpolation == Interpolation::Linear, "Linear").clicked() {
                    cmds.push(AppCommand::SetKeyframeInterpolation {
                        clip_id, param: track.param, time_secs: kf.time_secs,
                        interpolation: Interpolation::Linear,
                    });
                    ui.close_menu();
                }
                if ui.selectable_label(kf.interpolation == Interpolation::Bezier, "Bezier").clicked() {
                    cmds.push(AppCommand::SetKeyframeInterpolation {
                        clip_id, param: track.param, time_secs: kf.time_secs,
                        interpolation: Interpolation::Bezier,
                    });
                    ui.close_menu();
                }
                if ui.selectable_label(kf.interpolation == Interpolation::Step, "Step").clicked() {
                    cmds.push(AppCommand::SetKeyframeInterpolation {
                        clip_id, param: track.param, time_secs: kf.time_secs,
                        interpolation: Interpolation::Step,
                    });
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Delete").clicked() {
                    cmds.push(AppCommand::RemoveKeyframe {
                        clip_id, param: track.param, time_secs: kf.time_secs,
                    });
                    ui.close_menu();
                }
            });

            // Bezier tangent handles
            if kf.interpolation == Interpolation::Bezier {
                let handle_color = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 120);

                // Out tangent handle
                if kf.tangent_out != (0.0, 0.0) || kf_idx < track.keyframes.len() - 1 {
                    let out_pos = to_screen(
                        kf.time_secs + kf.tangent_out.0,
                        kf.value + kf.tangent_out.1,
                        v_min, v_max,
                    );
                    painter.line_segment([pos, out_pos], egui::Stroke::new(0.8, handle_color));
                    painter.circle_filled(out_pos, 3.0, handle_color);

                    let out_id = ui.id().with(("kf_tout", track.param as u8, kf_idx));
                    let out_rect = egui::Rect::from_center_size(out_pos, egui::vec2(10.0, 10.0));
                    let out_resp = ui.interact(out_rect, out_id, egui::Sense::drag());
                    if out_resp.dragged() {
                        let d = out_resp.drag_delta();
                        let dt = d.x as f64 / plot_rect.width() as f64 * duration;
                        let dv = -d.y / plot_rect.height() * (v_max - v_min);
                        cmds.push(AppCommand::SetKeyframeTangents {
                            clip_id, param: track.param, time_secs: kf.time_secs,
                            tangent_in: kf.tangent_in,
                            tangent_out: (kf.tangent_out.0 + dt, kf.tangent_out.1 + dv),
                        });
                    }
                }

                // In tangent handle
                if kf.tangent_in != (0.0, 0.0) || kf_idx > 0 {
                    let in_pos = to_screen(
                        kf.time_secs + kf.tangent_in.0,
                        kf.value + kf.tangent_in.1,
                        v_min, v_max,
                    );
                    painter.line_segment([pos, in_pos], egui::Stroke::new(0.8, handle_color));
                    painter.circle_filled(in_pos, 3.0, handle_color);

                    let in_id = ui.id().with(("kf_tin", track.param as u8, kf_idx));
                    let in_rect = egui::Rect::from_center_size(in_pos, egui::vec2(10.0, 10.0));
                    let in_resp = ui.interact(in_rect, in_id, egui::Sense::drag());
                    if in_resp.dragged() {
                        let d = in_resp.drag_delta();
                        let dt = d.x as f64 / plot_rect.width() as f64 * duration;
                        let dv = -d.y / plot_rect.height() * (v_max - v_min);
                        cmds.push(AppCommand::SetKeyframeTangents {
                            clip_id, param: track.param, time_secs: kf.time_secs,
                            tangent_in: (kf.tangent_in.0 + dt, kf.tangent_in.1 + dv),
                            tangent_out: kf.tangent_out,
                        });
                    }
                }
            }
        }

        // Parameter label
        let label_y = plot_rect.min.y + 12.0 * tracks.tracks.iter()
            .position(|t| t.param == track.param).unwrap_or(0) as f32;
        painter.text(
            egui::pos2(rect.min.x + 4.0, label_y + 4.0),
            egui::Align2::LEFT_TOP,
            format!("{:?}", track.param),
            egui::FontId::monospace(8.0),
            color,
        );
    }

    // Click on empty area to add keyframe (for first visible track)
    if response.clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            if plot_rect.contains(pos) {
                let t = (pos.x - plot_rect.min.x) / plot_rect.width();
                let time = t as f64 * duration;
                // Add to first non-empty track
                if let Some(track) = tracks.tracks.first() {
                    let (v_min, v_max) = param_range(track.param);
                    let yt = 1.0 - (pos.y - plot_rect.min.y) / plot_rect.height();
                    let value = v_min + yt * (v_max - v_min);
                    cmds.push(AppCommand::AddKeyframe {
                        clip_id, param: track.param, time_secs: time, value,
                    });
                }
            }
        }
    }

    cmds
}
