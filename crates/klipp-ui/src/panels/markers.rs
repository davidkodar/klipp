use crate::icons::Icon;
use crate::state::AppState;
use klipp_core::{AppCommand, MarkerColor, MarkerId};

#[derive(Default)]
pub struct MarkersPanel {
    editing_label: Option<MarkerId>,
    label_buf: String,
}

impl MarkersPanel {
    pub fn show(&mut self, ctx: &egui::Context, state: &mut AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        let mut open = state.show_markers_panel;

        egui::Window::new("Markers")
            .open(&mut open)
            .default_width(280.0)
            .default_height(300.0)
            .resizable(true)
            .show(ctx, |ui| {
                // Add marker button
                ui.horizontal(|ui| {
                    if ui.button("+ Add Marker").clicked() {
                        cmds.push(AppCommand::AddMarker {
                            position: state.playhead,
                            label: String::new(),
                            color: MarkerColor::Blue,
                        });
                    }
                });

                ui.separator();

                // Marker list
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut remove_id = None;
                    let markers: Vec<_> = state.sequence().markers.iter().map(|m| {
                        (m.id, m.position, m.label.clone(), m.color)
                    }).collect();

                    for (id, position, label, color) in &markers {
                        ui.horizontal(|ui| {
                            // Color button
                            let (r, g, b) = color.to_rgb();
                            let btn_color = egui::Color32::from_rgb(r, g, b);
                            let btn = egui::Button::new("●")
                                .fill(egui::Color32::TRANSPARENT);
                            let resp = ui.add(btn);
                            ui.painter().text(
                                resp.rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "●",
                                egui::FontId::proportional(14.0),
                                btn_color,
                            );
                            if resp.clicked() {
                                // Cycle to next color
                                let colors = MarkerColor::all();
                                let idx = colors.iter().position(|c| c == color).unwrap_or(0);
                                let next = colors[(idx + 1) % colors.len()];
                                cmds.push(AppCommand::UpdateMarkerColor {
                                    marker_id: *id,
                                    color: next,
                                });
                            }

                            // Timecode
                            let tc_str = format_timecode_short(position.to_secs_f64());
                            if ui.small_button(&tc_str).clicked() {
                                cmds.push(AppCommand::JumpToMarker { marker_id: *id });
                            }

                            // Label (inline edit)
                            if self.editing_label == Some(*id) {
                                let resp = ui.text_edit_singleline(&mut self.label_buf);
                                if resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    cmds.push(AppCommand::UpdateMarkerLabel {
                                        marker_id: *id,
                                        label: self.label_buf.clone(),
                                    });
                                    self.editing_label = None;
                                }
                            } else {
                                let display = if label.is_empty() { "(no label)" } else { label.as_str() };
                                let resp = ui.label(
                                    egui::RichText::new(display)
                                        .size(11.0)
                                        .color(if label.is_empty() {
                                            egui::Color32::from_rgb(100, 100, 110)
                                        } else {
                                            egui::Color32::from_rgb(200, 200, 210)
                                        }),
                                );
                                if resp.double_clicked() {
                                    self.editing_label = Some(*id);
                                    self.label_buf = label.clone();
                                }
                            }

                            // Delete button
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::image(Icon::X.image(10.0).tint(egui::Color32::from_rgb(220, 220, 230)))).clicked() {
                                    remove_id = Some(*id);
                                }
                            });
                        });
                    }

                    if let Some(id) = remove_id {
                        cmds.push(AppCommand::RemoveMarker { marker_id: id });
                    }

                    if markers.is_empty() {
                        ui.label(
                            egui::RichText::new("No markers. Press M to add one.")
                                .size(11.0)
                                .color(egui::Color32::from_rgb(100, 100, 110)),
                        );
                    }
                });
            });

        state.show_markers_panel = open;
        cmds
    }
}

fn format_timecode_short(secs: f64) -> String {
    let m = (secs / 60.0) as u32;
    let s = secs % 60.0;
    format!("{:02}:{:05.2}", m, s)
}
