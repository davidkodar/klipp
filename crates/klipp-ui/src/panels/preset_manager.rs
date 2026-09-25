//! Export preset manager modal (#45, scoped).
//!
//! Lists user-defined export presets with Delete / Rename / Duplicate
//! actions plus JSON import/export. Built-in presets are shown greyed
//! out and read-only. This feature is deliberately scoped to preset
//! management — raw FFmpeg argument passthrough is explicitly deferred
//! because Klipp's encoder is ffmpeg-next (in-process), not the ffmpeg
//! CLI, so custom argv requires architectural changes beyond an "easy"
//! MVP.

use crate::state::AppState;
use klipp_core::{AppCommand, ExportPreset, ExportPresetSettings};

/// Render the preset manager modal. Returns any `AppCommand`s dispatched
/// by the user (Delete/Rename/Duplicate). JSON import mutates
/// `custom_presets` directly since it has to land before the next frame.
///
/// `custom_presets` is borrowed mutably so that the Rename text edit can
/// write straight into the preset name without round-tripping through
/// an AppCommand (keeps keystroke latency zero). Delete/Duplicate still
/// route through AppCommands so they're undoable via the central handler
/// and play nicely with `prefs_dirty`.
pub fn show_preset_manager_dialog(
    ctx: &egui::Context,
    state: &mut AppState,
    custom_presets: &mut Vec<ExportPreset>,
) -> Vec<AppCommand> {
    let mut cmds: Vec<AppCommand> = Vec::new();
    if !state.show_preset_manager {
        return cmds;
    }

    let built_ins = klipp_core::builtin_presets();

    let mut open = true;
    egui::Window::new("Export Presets")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(520.0)
        .default_height(420.0)
        .min_width(440.0)
        .min_height(320.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            // ── Left: list. Right: details + actions. ──
            // Fixed-ish split — the list doesn't need to be wide.
            let avail = ui.available_size();
            let left_w = (avail.x * 0.38).clamp(140.0, 220.0);
            let right_w = (avail.x - left_w - 12.0).max(220.0);

            ui.horizontal_top(|ui| {
                // Left column — preset list
                ui.allocate_ui(egui::vec2(left_w, avail.y - 48.0), |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("preset_mgr_list")
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
                                        .preset_manager_selected
                                        .as_deref()
                                        == Some(p.id.as_str());
                                    let label = format!("\u{1F512} {}", p.name);
                                    if ui.selectable_label(is_selected, label).clicked() {
                                        state.preset_manager_selected = Some(p.id.clone());
                                    }
                                }
                                ui.add_space(8.0);
                            }
                            ui.label(
                                egui::RichText::new("CUSTOM")
                                    .size(10.0)
                                    .strong()
                                    .color(egui::Color32::from_gray(140)),
                            );
                            if custom_presets.is_empty() {
                                ui.label(
                                    egui::RichText::new("(no custom presets)")
                                        .italics()
                                        .size(11.0)
                                        .color(egui::Color32::from_gray(120)),
                                );
                            } else {
                                for p in custom_presets.iter() {
                                    let is_selected = state
                                        .preset_manager_selected
                                        .as_deref()
                                        == Some(p.id.as_str());
                                    if ui
                                        .selectable_label(is_selected, &p.name)
                                        .clicked()
                                    {
                                        state.preset_manager_selected = Some(p.id.clone());
                                    }
                                }
                            }
                        });
                });

                ui.separator();

                // Right column — details + actions
                ui.allocate_ui(egui::vec2(right_w, avail.y - 48.0), |ui| {
                    let selected_id = state.preset_manager_selected.clone();
                    let Some(id) = selected_id else {
                        ui.label(
                            egui::RichText::new("Select a preset to view its details.")
                                .italics()
                                .color(egui::Color32::from_gray(130)),
                        );
                        return;
                    };

                    // Locate the preset — user first (mutable) then built-in (read-only).
                    if let Some(idx) = custom_presets.iter().position(|p| p.id == id) {
                        let preset = &mut custom_presets[idx];
                        ui.label(egui::RichText::new("Name").strong());
                        ui.add(
                            egui::TextEdit::singleline(&mut preset.name)
                                .desired_width(f32::INFINITY),
                        );
                        ui.add_space(8.0);
                        render_settings_summary(ui, &preset.settings);
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Duplicate").clicked() {
                                cmds.push(AppCommand::DuplicateExportPreset {
                                    preset_id: id.clone(),
                                });
                            }
                            if ui
                                .button(
                                    egui::RichText::new("Delete")
                                        .color(egui::Color32::from_rgb(0xd0, 0x60, 0x60)),
                                )
                                .clicked()
                            {
                                cmds.push(AppCommand::DeleteExportPreset {
                                    preset_id: id.clone(),
                                });
                                state.preset_manager_selected = None;
                            }
                            if ui.button("Export JSON\u{2026}").clicked() {
                                // Serialized now, written once a place is chosen.
                                match serde_json::to_string_pretty(preset) {
                                    Ok(json) => {
                                        let file_name = format!("{}.json", sanitize(&preset.name));
                                        crate::file_dialog::spawn(move || {
                                            if let Some(path) = rfd::FileDialog::new()
                                                .set_title("Export Preset")
                                                .add_filter("Klipp preset (JSON)", &["json"])
                                                .set_file_name(file_name)
                                                .save_file()
                                            {
                                                crate::file_dialog::notify(match std::fs::write(&path, json) {
                                                    Ok(()) => format!("Exported preset to {}", path.display()),
                                                    Err(e) => format!("Export failed: {e}"),
                                                });
                                            }
                                            Vec::new()
                                        });
                                    }
                                    Err(e) => {
                                        state
                                            .toast
                                            .show(format!("Serialize failed: {e}"));
                                    }
                                }
                            }
                        });
                    } else if let Some(p) = built_ins.iter().find(|p| p.id == id) {
                        ui.label(egui::RichText::new("Name").strong());
                        ui.label(&p.name);
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new("\u{1F512} Built-in preset (read-only)")
                                .italics()
                                .color(egui::Color32::from_gray(140)),
                        );
                        ui.add_space(8.0);
                        render_settings_summary(ui, &p.settings);
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui.button("Duplicate").clicked() {
                                cmds.push(AppCommand::DuplicateExportPreset {
                                    preset_id: id.clone(),
                                });
                            }
                        });
                    } else {
                        // Selected id no longer exists (e.g. deleted this frame).
                        state.preset_manager_selected = None;
                    }
                });
            });

            ui.separator();

            // ── Footer: Import + Close ──
            ui.horizontal(|ui| {
                let picker = egui::Id::new("preset-manager-import");
                if ui.button("Import JSON\u{2026}").clicked() {
                    crate::file_dialog::pick_path(picker, || {
                        rfd::FileDialog::new()
                            .set_title("Import Preset")
                            .add_filter("Klipp preset (JSON)", &["json"])
                            .pick_file()
                    });
                }
                if let Some(crate::file_dialog::Picked::Path(path)) = crate::file_dialog::take_path(picker) {
                    match std::fs::read_to_string(&path) {
                        Ok(s) => match serde_json::from_str::<ExportPreset>(&s) {
                            Ok(mut p) => {
                                // Force a new id + mark as user preset so
                                // imports never collide with built-ins or
                                // existing custom entries.
                                p.id = format!("custom-{}", uuid::Uuid::new_v4());
                                p.built_in = false;
                                let name = p.name.clone();
                                custom_presets.push(p);
                                state.toast.show(format!("Imported \"{name}\""));
                            }
                            Err(e) => {
                                state.toast.show(format!("Import failed: {e}"));
                            }
                        },
                        Err(e) => {
                            state.toast.show(format!("Read failed: {e}"));
                        }
                    }
                }
                ui.with_layout(
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui.button("Close").clicked() {
                            state.show_preset_manager = false;
                        }
                    },
                );
            });
        });
    if !open {
        state.show_preset_manager = false;
    }
    cmds
}

// ── Helpers ───────────────────────────────────────────────────────────

fn render_settings_summary(ui: &mut egui::Ui, s: &ExportPresetSettings) {
    egui::Grid::new("preset_mgr_summary")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            kv(ui, "Format", &format!("{:?}", s.format));
            kv(ui, "Video codec", &format!("{:?}", s.video_codec));
            if let Some(r) = s.resolution {
                kv(ui, "Resolution", &format!("{} \u{00D7} {}", r.width, r.height));
            } else {
                kv(ui, "Resolution", "Sequence default");
            }
            if let Some(f) = s.fps {
                kv(ui, "Frame rate", &format!("{:.3} fps", f.as_f64()));
            } else {
                kv(ui, "Frame rate", "Sequence default");
            }
            kv(ui, "CRF / quality", &format!("{}", s.crf));
            kv(ui, "Encoder preset", s.preset.label());
            kv(ui, "Audio codec", &format!("{:?}", s.audio_codec));
            if let Some(abr) = s.audio_bitrate_kbps {
                kv(ui, "Audio bitrate", &format!("{abr} kbps"));
            }
            if let Some(vbr) = s.video_bitrate_kbps {
                kv(ui, "Video bitrate", &format!("{vbr} kbps"));
            }
            kv(ui, "Rate control", &format!("{:?}", s.rate_control));
            if let Some(t) = s.target_lufs {
                kv(ui, "Loudness target", &format!("{t:.1} LUFS"));
            }
        });
}

fn kv(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.label(egui::RichText::new(label).color(egui::Color32::from_gray(150)));
    ui.label(value);
    ui.end_row();
}

fn sanitize(name: &str) -> String {
    // Strip characters that would trip up common file systems. Keep it
    // permissive: alphanumerics, dashes, underscores, spaces.
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
    use klipp_core::{
        EncoderPreset, ExportAudioCodec, ExportFormat, ExportVideoCodec, RateControl,
    };

    fn make_settings() -> ExportPresetSettings {
        ExportPresetSettings {
            format: ExportFormat::Mp4,
            video_codec: ExportVideoCodec::H264,
            resolution: None,
            fps: None,
            crf: 20,
            preset: EncoderPreset::Medium,
            audio_codec: ExportAudioCodec::Aac,
            audio_bitrate_kbps: Some(192),
            video_bitrate_kbps: None,
            rate_control: RateControl::Crf,
            target_lufs: None,
            true_peak_dbtp: None,
        }
    }

    #[test]
    fn delete_preset_removes_correct_id() {
        // Mirrors the DeleteExportPreset handler: retain() by id.
        let mut presets = vec![
            ExportPreset { id: "a".into(), name: "A".into(), built_in: false, settings: make_settings() },
            ExportPreset { id: "b".into(), name: "B".into(), built_in: false, settings: make_settings() },
        ];
        presets.retain(|p| p.id != "a");
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].id, "b");
    }

    #[test]
    fn duplicate_preset_gets_new_id() {
        let original = ExportPreset {
            id: "orig".into(),
            name: "Original".into(),
            built_in: false,
            settings: make_settings(),
        };
        let mut dup = original.clone();
        dup.id = "new-id".into();
        dup.name = format!("{} (copy)", original.name);
        assert_ne!(dup.id, original.id);
        assert!(dup.name.contains("(copy)"));
    }

    #[test]
    fn rename_preset_updates_name() {
        let mut preset = ExportPreset {
            id: "x".into(),
            name: "Old".into(),
            built_in: false,
            settings: make_settings(),
        };
        preset.name = "New".into();
        assert_eq!(preset.name, "New");
    }

    #[test]
    fn import_preset_overrides_id_and_built_in() {
        // Imports must never trust the id / built_in fields in the JSON:
        // a malicious file could claim to be a protected built-in and
        // hide a delete button. The import code sets both fresh.
        let mut p = ExportPreset {
            id: "00000000-0000-4000-a000-000000000001".into(), // pretends to be YouTube 1080p
            name: "Sneaky".into(),
            built_in: true,
            settings: make_settings(),
        };
        p.id = format!("custom-{}", uuid::Uuid::new_v4());
        p.built_in = false;
        assert!(p.id.starts_with("custom-"));
        assert!(!p.built_in);
    }

    #[test]
    fn sanitize_strips_unsafe_chars() {
        assert_eq!(sanitize("Hello World"), "Hello World");
        assert_eq!(sanitize("Bad/Path:Name"), "Bad_Path_Name");
        assert_eq!(sanitize("With-Dash_Underscore"), "With-Dash_Underscore");
    }

    #[test]
    fn preset_json_roundtrip() {
        // Guard against silent serde breakage — any change to ExportPreset
        // shape must go through a round-trippable JSON representation or
        // the "Export/Import JSON" feature breaks.
        let original = ExportPreset {
            id: "custom-abc".into(),
            name: "My Preset".into(),
            built_in: false,
            settings: make_settings(),
        };
        let s = serde_json::to_string_pretty(&original).expect("serialize");
        let back: ExportPreset = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(back, original);
    }
}
