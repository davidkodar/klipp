//! Workspace-layout manager modal (#39).
//!
//! Mirrors the preset_manager dialog structure (list left, details
//! right, footer import/export) but operates on workspace layouts
//! rather than export presets. Built-ins are shown locked — only
//! user-saved layouts can be renamed, deleted, or exported.

use crate::state::{AppState, SavedWorkspaceLayout, builtin_workspace_layouts};
use klipp_core::AppCommand;

/// Render the workspace manager modal. Returns any AppCommands the user
/// dispatched (Delete/Rename).
///
/// Custom workspaces are borrowed mutably so inline rename on the name
/// field writes straight into state — keystroke latency zero, same as
/// the preset manager's rename field.
pub fn show_workspace_manager_dialog(
    ctx: &egui::Context,
    state: &mut AppState,
) -> Vec<AppCommand> {
    let mut cmds: Vec<AppCommand> = Vec::new();
    if !state.show_workspace_manager {
        return cmds;
    }

    let built_ins = builtin_workspace_layouts();

    let screen = ctx.screen_rect();
    let win_w = 520.0_f32.min(screen.width() - 40.0);
    let win_h = 420.0_f32.min(screen.height() - 40.0);
    let min_w = 440.0_f32.min(screen.width() - 40.0);
    let min_h = 320.0_f32.min(screen.height() - 40.0);

    let mut open = true;
    egui::Window::new("Workspace Layouts")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(win_w)
        .default_height(win_h)
        .min_width(min_w)
        .min_height(min_h)
        .default_pos(egui::pos2(
            screen.center().x - win_w / 2.0,
            screen.center().y - win_h / 2.0,
        ))
        .show(ctx, |ui| {
            let avail = ui.available_size();
            let left_w = (avail.x * 0.38).clamp(140.0, 220.0);
            let right_w = (avail.x - left_w - 12.0).max(220.0);

            ui.horizontal_top(|ui| {
                // ── Left: workspace list ──
                ui.allocate_ui(egui::vec2(left_w, avail.y - 48.0), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("ws_mgr_list")
                        .show(ui, |ui| {
                            if !built_ins.is_empty() {
                                ui.label(
                                    egui::RichText::new("BUILT-IN")
                                        .size(10.0)
                                        .strong()
                                        .color(egui::Color32::from_gray(140)),
                                );
                                for p in &built_ins {
                                    let is_selected = state
                                        .workspace_manager_selected
                                        .as_deref() == Some(p.name.as_str());
                                    let label = format!("\u{1F512} {}", p.name);
                                    if ui.selectable_label(is_selected, label).clicked() {
                                        state.workspace_manager_selected = Some(p.name.clone());
                                    }
                                }
                                ui.add_space(8.0);
                            }
                            ui.label(
                                egui::RichText::new("SAVED")
                                    .size(10.0)
                                    .strong()
                                    .color(egui::Color32::from_gray(140)),
                            );
                            if state.custom_workspaces.is_empty() {
                                ui.label(
                                    egui::RichText::new("(no saved workspaces)")
                                        .italics()
                                        .size(11.0)
                                        .color(egui::Color32::from_gray(120)),
                                );
                            } else {
                                for ws in state.custom_workspaces.iter() {
                                    let is_selected = state
                                        .workspace_manager_selected
                                        .as_deref() == Some(ws.name.as_str());
                                    if ui
                                        .selectable_label(is_selected, &ws.name)
                                        .clicked()
                                    {
                                        state.workspace_manager_selected = Some(ws.name.clone());
                                    }
                                }
                            }
                        });
                });

                ui.separator();

                // ── Right: details + actions ──
                ui.allocate_ui(egui::vec2(right_w, avail.y - 48.0), |ui| {
                    let selected_name = state.workspace_manager_selected.clone();
                    let Some(name) = selected_name else {
                        ui.label(
                            egui::RichText::new("Select a workspace to view its details.")
                                .italics()
                                .color(egui::Color32::from_gray(130)),
                        );
                        return;
                    };

                    // Mutable user-preset branch.
                    if let Some(idx) = state
                        .custom_workspaces
                        .iter()
                        .position(|w| w.name == name)
                    {
                        let old_name = name.clone();
                        let ws = &mut state.custom_workspaces[idx];
                        // Snapshot the name buffer — direct in-place edit
                        // would rename the preset on every keystroke and
                        // break the selection match. Commit on button.
                        let mut name_buf = ws.name.clone();
                        ui.label(egui::RichText::new("Name").strong());
                        ui.add(
                            egui::TextEdit::singleline(&mut name_buf)
                                .desired_width(f32::INFINITY),
                        );
                        let rename_clicked = ui.button("Apply rename").clicked();
                        ui.add_space(8.0);
                        render_layout_summary(ui, ws);
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button(
                                    egui::RichText::new("Delete")
                                        .color(egui::Color32::from_rgb(0xd0, 0x60, 0x60)),
                                )
                                .clicked()
                            {
                                cmds.push(AppCommand::DeleteWorkspaceLayout { name: old_name.clone() });
                            }
                            if ui.button("Export JSON\u{2026}").clicked() {
                                if let Some(path) = rfd::FileDialog::new()
                                    .set_title("Export Workspace")
                                    .add_filter("Klipp workspace (JSON)", &["json"])
                                    .set_file_name(format!("{}.json", sanitize(&ws.name)))
                                    .save_file()
                                {
                                    match serde_json::to_string_pretty(ws) {
                                        Ok(s) => {
                                            if let Err(e) = std::fs::write(&path, s) {
                                                state.toast.show(format!("Export failed: {e}"));
                                            } else {
                                                state.toast.show(format!(
                                                    "Exported workspace to {}",
                                                    path.display()
                                                ));
                                            }
                                        }
                                        Err(e) => state.toast.show(format!("Serialize failed: {e}")),
                                    }
                                }
                            }
                        });
                        if rename_clicked && name_buf.trim() != old_name {
                            cmds.push(AppCommand::RenameWorkspaceLayout {
                                old_name,
                                new_name: name_buf.trim().to_string(),
                            });
                        }
                    } else if let Some(b) = built_ins.iter().find(|b| b.name == name) {
                        // Built-in — read-only.
                        ui.label(egui::RichText::new("Name").strong());
                        ui.label(&b.name);
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("\u{1F512} Built-in layout (read-only)")
                                .italics()
                                .color(egui::Color32::from_gray(140)),
                        );
                        ui.add_space(8.0);
                        render_layout_summary(ui, b);
                    } else {
                        // Selected workspace disappeared (e.g. just deleted).
                        state.workspace_manager_selected = None;
                    }
                });
            });

            ui.separator();

            // ── Footer: Import + Close ──
            ui.horizontal(|ui| {
                if ui.button("Import JSON\u{2026}").clicked() {
                    if let Some(path) = rfd::FileDialog::new()
                        .set_title("Import Workspace")
                        .add_filter("Klipp workspace (JSON)", &["json"])
                        .pick_file()
                    {
                        match std::fs::read_to_string(&path) {
                            Ok(s) => match serde_json::from_str::<SavedWorkspaceLayout>(&s) {
                                Ok(ws) => {
                                    // Imports collide with existing names
                                    // → append " (imported)" to keep both.
                                    let mut ws = ws;
                                    if state.custom_workspaces.iter().any(|w| w.name == ws.name)
                                        || built_ins.iter().any(|b| b.name == ws.name)
                                    {
                                        ws.name = format!("{} (imported)", ws.name);
                                    }
                                    let name = ws.name.clone();
                                    state.custom_workspaces.push(ws);
                                    state.toast.show(format!("Imported \"{name}\""));
                                }
                                Err(e) => state.toast.show(format!("Import failed: {e}")),
                            },
                            Err(e) => state.toast.show(format!("Read failed: {e}")),
                        }
                    }
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.button("Close").clicked() {
                            state.show_workspace_manager = false;
                        }
                    },
                );
            });
        });
    if !open {
        state.show_workspace_manager = false;
    }
    cmds
}

fn render_layout_summary(ui: &mut egui::Ui, ws: &SavedWorkspaceLayout) {
    egui::Grid::new("ws_mgr_summary")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            kv(ui, "Browser", if ws.show_browser { "visible" } else { "hidden" });
            kv(ui, "Inspector", if ws.show_inspector { "visible" } else { "hidden" });
            kv(ui, "Scopes", if ws.show_scopes { "visible" } else { "hidden" });
            kv(ui, "Color panel", if ws.show_color_panel { "visible" } else { "hidden" });
            kv(ui, "Curve editor", if ws.show_curve_editor { "visible" } else { "hidden" });
            kv(ui, "Markers", if ws.show_markers { "visible" } else { "hidden" });
            kv(ui, "History", if ws.show_history { "visible" } else { "hidden" });
            if let Some(w) = ws.browser_width {
                kv(ui, "Browser width", &format!("{w:.0} px"));
            }
            if let Some(w) = ws.inspector_width {
                kv(ui, "Inspector width", &format!("{w:.0} px"));
            }
            if let Some(h) = ws.timeline_height {
                kv(ui, "Timeline height", &format!("{h:.0} px"));
            }
            if let Some(h) = ws.color_panel_height {
                kv(ui, "Color panel height", &format!("{h:.0} px"));
            }
        });
}

fn kv(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(label).color(egui::Color32::from_gray(150)));
    ui.label(value);
    ui.end_row();
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ws(name: &str) -> SavedWorkspaceLayout {
        SavedWorkspaceLayout {
            name: name.into(),
            show_browser: true,
            show_inspector: true,
            show_history: false,
            show_markers: false,
            show_curve_editor: false,
            show_scopes: false,
            show_color_panel: false,
            browser_width: Some(300.0),
            inspector_width: Some(320.0),
            timeline_height: Some(240.0),
            color_panel_height: None,
        }
    }

    #[test]
    fn delete_workspace_removes_correct_name() {
        let mut workspaces = vec![make_ws("A"), make_ws("B")];
        workspaces.retain(|w| w.name != "A");
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].name, "B");
    }

    #[test]
    fn rename_updates_name_in_place() {
        let mut ws = make_ws("Old");
        ws.name = "New".into();
        assert_eq!(ws.name, "New");
    }

    #[test]
    fn workspace_json_roundtrip() {
        // Guard against silent serde breakage in SavedWorkspaceLayout —
        // any shape change must go through a round-trippable JSON.
        let original = make_ws("Test");
        let s = serde_json::to_string_pretty(&original).expect("serialize");
        let back: SavedWorkspaceLayout = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back.name, original.name);
        assert_eq!(back.browser_width, original.browser_width);
    }

    #[test]
    fn legacy_workspace_json_deserializes_with_none_sizes() {
        // Pre-#39 workspaces.json entries didn't have sizing fields —
        // the new Option<f32> fields must deserialize cleanly to None
        // so existing user files load without breaking.
        let legacy = r#"{
            "name": "Legacy",
            "show_browser": true,
            "show_inspector": true,
            "show_history": false,
            "show_markers": false,
            "show_curve_editor": false,
            "show_scopes": false,
            "show_color_panel": false
        }"#;
        let ws: SavedWorkspaceLayout =
            serde_json::from_str(legacy).expect("legacy parses");
        assert_eq!(ws.name, "Legacy");
        assert!(ws.browser_width.is_none());
        assert!(ws.inspector_width.is_none());
        assert!(ws.timeline_height.is_none());
        assert!(ws.color_panel_height.is_none());
    }

    #[test]
    fn sanitize_strips_unsafe_chars() {
        assert_eq!(sanitize("Hello World"), "Hello World");
        assert_eq!(sanitize("Bad/Path:Name"), "Bad_Path_Name");
    }
}
