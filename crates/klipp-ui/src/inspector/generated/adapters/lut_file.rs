//! A 3D LUT effect's file (#1746): its name, Browse, the bundled looks and
//! Clear, as the grade's LUT node offers them, written as the effect's
//! `path` parameter: a look from the bundled library by its name there
//! (`bundled:<name>`), so the project opens on any machine.
use crate::widgets::menu::SubmenuRows;

use crate::inspector::generated::custom::CustomEditor;
use crate::inspector::generated::RowCtx;
use klipp_core::AppCommand;
use klipp_effects::registry::ParamDesc;
use klipp_graph_format::path::NodePath;
use klipp_graph_format::ParamValue;
use serde_json::Value;

pub struct LutFileEditor;

impl CustomEditor for LutFileEditor {
    fn show(&self, ui: &mut egui::Ui, ctx: &mut RowCtx, _desc: &ParamDesc, stored: &Value) {
        let path = stored.as_str().unwrap_or_default().to_owned();
        let with = |addr: NodePath, path: String| AppCommand::SetParamAt { addr, key: "path".into(), value: ParamValue::Text(path) };
        ui.horizontal(|ui| {
            ui.label("LUT file");
            if path.is_empty() {
                ui.label(egui::RichText::new("None chosen").color(ctx.theme.text_dim));
            } else {
                let p = std::path::Path::new(&path);
                let name = p.file_stem().map_or_else(|| path.clone(), |s| s.to_string_lossy().into_owned());
                ui.label(name).on_hover_text(&path);
            }
        });
        // A file this machine doesn't have: the effect passes the picture,
        // and says so here as the viewer does.
        if !path.is_empty() && !klipp_core::assets::resolve_lut_path(&path, klipp_core::assets::bundled_luts_dir()).exists() {
            ui.colored_label(ctx.theme.marker_warning, "\u{26a0} LUT file not found").on_hover_text(format!("{path} isn't on this machine: the picture passes through until it's found or another is chosen"));
        }
        ui.horizontal(|ui| {
            if ui.button("Browse\u{2026}").on_hover_text("Choose a .cube, .3dl or .csp file").clicked() {
                let addr = ctx.addr.clone();
                crate::file_dialog::spawn_pick(
                    || rfd::FileDialog::new().set_title("LUT for this effect").add_filter("LUT files", &["cube", "3dl", "csp"]).pick_file(),
                    move |chosen| vec![with(addr, portable(&chosen))],
                );
            }
            if let Some(dir) = klipp_core::assets::bundled_luts_dir() {
                ui.menu_rows("Bundled", |ui| {
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for (category, files) in crate::panels::node_inspector::bundled_luts(dir) {
                            ui.menu_rows(category, |ui| {
                                for file in files {
                                    let name = file.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                                    let file = portable(&file);
                                    if ui.selectable_label(file == path, name).clicked() {
                                        ctx.cmds.push(with(ctx.addr.clone(), file));
                                        ui.close_menu();
                                    }
                                }
                            });
                        }
                    });
                });
            }
            if !path.is_empty() && ui.button("Clear").clicked() {
                ctx.cmds.push(with(ctx.addr.clone(), String::new()));
            }
        });
    }
}

/// How the project stores `file`.
fn portable(file: &std::path::Path) -> String {
    klipp_core::assets::portable_lut_path(file, klipp_core::assets::bundled_luts_dir())
}
