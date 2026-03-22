use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

const TAB_HEIGHT: f32 = 24.0;
const TAB_PAD_H: f32 = 12.0;
const TAB_GAP: f32 = 1.0;
const CLOSE_SIZE: f32 = 14.0;
const ADD_BTN_W: f32 = 28.0;

/// Draw the sequence tab bar. Returns commands and the height consumed.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let rect = ui.max_rect();

    // Background
    ui.painter().rect_filled(rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        rect.x_range(),
        rect.max.y,
        egui::Stroke::new(1.0, theme.border),
    );

    let mut x = rect.min.x + 4.0;
    let y = rect.min.y;

    // Render each sequence tab
    for i in 0..state.sequences.len() {
        let is_active = i == state.active_sequence_idx;
        let name = state.sequences[i].name.clone();

        // Measure text width
        let font = egui::FontId::proportional(11.0);
        let text_width = ui.painter().layout_no_wrap(
            name.clone(),
            font.clone(),
            theme.text_primary,
        ).rect.width();

        let tab_w = text_width + TAB_PAD_H * 2.0 + CLOSE_SIZE + 4.0;
        let tab_rect = egui::Rect::from_min_size(
            egui::pos2(x, y),
            egui::vec2(tab_w, TAB_HEIGHT),
        );

        // Interaction
        let resp = ui.allocate_rect(tab_rect, egui::Sense::click());
        let hovered = resp.hovered();

        // Background
        let bg = if is_active {
            theme.bg_raised
        } else if hovered {
            theme.bg_hover
        } else {
            theme.bg_panel
        };
        ui.painter().rect_filled(tab_rect, 0.0, bg);

        // Active indicator — amber bottom border
        if is_active {
            ui.painter().hline(
                tab_rect.x_range(),
                tab_rect.max.y - 1.0,
                egui::Stroke::new(2.0, theme.accent),
            );
        }

        // Tab name
        let text_color = if is_active {
            theme.text_primary
        } else if hovered {
            theme.text_secondary
        } else {
            theme.text_dim
        };
        ui.painter().text(
            egui::pos2(tab_rect.min.x + TAB_PAD_H, tab_rect.center().y),
            egui::Align2::LEFT_CENTER,
            &name,
            font,
            text_color,
        );

        // Close button (x)
        let close_rect = egui::Rect::from_min_size(
            egui::pos2(tab_rect.max.x - CLOSE_SIZE - 4.0, tab_rect.center().y - CLOSE_SIZE / 2.0),
            egui::vec2(CLOSE_SIZE, CLOSE_SIZE),
        );
        let close_resp = ui.allocate_rect(close_rect, egui::Sense::click());
        if close_resp.hovered() || is_active {
            let close_color = if close_resp.hovered() {
                theme.text_primary
            } else {
                theme.text_dim
            };
            // Draw X
            let cx = close_rect.center();
            let s = 3.5;
            ui.painter().line_segment(
                [egui::pos2(cx.x - s, cx.y - s), egui::pos2(cx.x + s, cx.y + s)],
                egui::Stroke::new(1.2, close_color),
            );
            ui.painter().line_segment(
                [egui::pos2(cx.x + s, cx.y - s), egui::pos2(cx.x - s, cx.y + s)],
                egui::Stroke::new(1.2, close_color),
            );
        }

        // Click to switch
        if resp.clicked() && !close_resp.hovered() && !is_active {
            cmds.push(AppCommand::SetActiveSequence { sequence_idx: i });
        }

        // Close button click — delete sequence
        if close_resp.clicked() && state.sequences.len() > 1 {
            cmds.push(AppCommand::DeleteSequence { sequence_idx: i });
        }

        // Right-click context menu
        resp.context_menu(|ui| {
            if ui.button("Rename...").clicked() {
                state.renaming_sequence_idx = Some(i);
                state.rename_sequence_buf = name.clone();
                ui.close_menu();
            }
            if ui.button("Duplicate").clicked() {
                cmds.push(AppCommand::DuplicateSequence {
                    sequence_idx: i,
                    new_name: format!("{} (copy)", name),
                });
                ui.close_menu();
            }
            ui.separator();
            let can_delete = state.sequences.len() > 1;
            if ui.add_enabled(can_delete, egui::Button::new(
                egui::RichText::new("Delete").color(egui::Color32::from_rgb(200, 60, 60))
            )).clicked() {
                cmds.push(AppCommand::DeleteSequence { sequence_idx: i });
                ui.close_menu();
            }
        });

        x += tab_w + TAB_GAP;
    }

    // "+" button to add new sequence
    let add_rect = egui::Rect::from_min_size(
        egui::pos2(x + 4.0, y),
        egui::vec2(ADD_BTN_W, TAB_HEIGHT),
    );
    let add_resp = ui.allocate_rect(add_rect, egui::Sense::click());
    let add_color = if add_resp.hovered() {
        theme.text_primary
    } else {
        theme.text_dim
    };
    ui.painter().text(
        add_rect.center(),
        egui::Align2::CENTER_CENTER,
        "+",
        egui::FontId::proportional(16.0),
        add_color,
    );
    if add_resp.clicked() {
        // Add a new sequence with same settings as the active one
        let active = &state.sequences[state.active_sequence_idx];
        let n = state.sequences.len() + 1;
        cmds.push(AppCommand::AddSequence {
            name: format!("Sequence {n}"),
            fps_num: active.fps.num,
            fps_den: active.fps.den,
            width: active.resolution.width,
            height: active.resolution.height,
        });
    }

    // Inline rename popup
    if let Some(rename_idx) = state.renaming_sequence_idx {
        if rename_idx < state.sequences.len() {
            let mut open = true;
            egui::Window::new("Rename Sequence")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        let resp = ui.text_edit_singleline(&mut state.rename_sequence_buf);
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let new_name = state.rename_sequence_buf.trim().to_string();
                            if !new_name.is_empty() {
                                cmds.push(AppCommand::RenameSequence {
                                    sequence_idx: rename_idx,
                                    new_name,
                                });
                            }
                            state.renaming_sequence_idx = None;
                        }
                        if ui.button("OK").clicked() {
                            let new_name = state.rename_sequence_buf.trim().to_string();
                            if !new_name.is_empty() {
                                cmds.push(AppCommand::RenameSequence {
                                    sequence_idx: rename_idx,
                                    new_name,
                                });
                            }
                            state.renaming_sequence_idx = None;
                        }
                    });
                });
            if !open {
                state.renaming_sequence_idx = None;
            }
        } else {
            state.renaming_sequence_idx = None;
        }
    }

    cmds
}

pub const HEIGHT: f32 = TAB_HEIGHT;
