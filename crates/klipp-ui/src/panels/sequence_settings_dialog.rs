//! "Sequence Settings…" modal — lets the user change a sequence's frame
//! resolution and frame rate from a preset list (or via custom width/height
//! and fps numerator/denominator). Reuses the preset tables defined in
//! `new_project_dialog` so the same shorthand is offered everywhere.

use crate::panels::new_project_dialog::{FPS_PRESETS, RESOLUTION_PRESETS};
use klipp_core::AppCommand;

/// Per-invocation state for the dialog. `None` means the dialog is closed.
pub struct SequenceSettingsDialog {
    pub sequence_idx: usize,
    pub width: u32,
    pub height: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    /// Index into `RESOLUTION_PRESETS`; the entry whose w/h matches the
    /// current `width`/`height`. Falls back to the "Custom..." row if no
    /// preset matches, which unlocks the manual width/height editors.
    resolution_idx: usize,
    /// Index into `FPS_PRESETS`. Same behaviour — defaults to last (Custom)
    /// when no preset matches.
    fps_idx: usize,
}

impl SequenceSettingsDialog {
    pub fn open(sequence_idx: usize, width: u32, height: u32, fps_num: u32, fps_den: u32) -> Self {
        let resolution_idx = RESOLUTION_PRESETS
            .iter()
            .position(|(_, w, h)| *w == width && *h == height)
            .unwrap_or_else(|| RESOLUTION_PRESETS.len() - 1); // "Custom..."
        let fps_idx = FPS_PRESETS
            .iter()
            .position(|(_, n, d)| *n == fps_num && *d == fps_den)
            .unwrap_or_else(|| FPS_PRESETS.len() - 1);
        Self { sequence_idx, width, height, fps_num, fps_den, resolution_idx, fps_idx }
    }

    /// Show the modal. Returns `(still_open, commands)`. When the user clicks
    /// "Apply", emits `SetSequenceResolution` and/or `SetSequenceFps` (only
    /// when the value actually changed) and closes.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        original_width: u32,
        original_height: u32,
        original_fps_num: u32,
        original_fps_den: u32,
    ) -> (bool, Vec<AppCommand>) {
        let mut open = true;
        let mut cmds = Vec::new();
        let mut apply = false;

        // Skip the "Automatic (from first clip)" entries — those only make
        // sense when creating a brand-new project. For an existing sequence
        // we always have concrete dimensions / fps.
        let resolution_options: Vec<&(&str, u32, u32)> = RESOLUTION_PRESETS
            .iter()
            .filter(|(label, _, _)| *label != "Automatic (from first clip)")
            .collect();
        let fps_options: Vec<&(&str, u32, u32)> = FPS_PRESETS
            .iter()
            .filter(|(label, _, _)| *label != "Automatic (from first clip)")
            .collect();

        // Backdrop
        egui::Area::new(egui::Id::new("seq_settings_backdrop"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::PanelResizeLine)
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                let resp = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(120));
                if resp.clicked() { open = false; }
            });

        egui::Window::new("Sequence Settings")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.add_space(4.0);

                // ── Resolution ──
                ui.label(egui::RichText::new("Resolution").strong());
                ui.add_space(4.0);
                let res_label = resolution_options
                    .iter()
                    .position(|(_, w, h)| *w == self.width && *h == self.height)
                    .map(|i| resolution_options[i].0)
                    .unwrap_or("Custom...");
                egui::ComboBox::from_id_salt("seq_settings_res")
                    .selected_text(res_label)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (i, (label, w, h)) in resolution_options.iter().enumerate() {
                            let is_selected = self.width == *w && self.height == *h
                                && *label != "Custom...";
                            if ui.selectable_label(is_selected, *label).clicked() {
                                self.resolution_idx = i;
                                if *label != "Custom..." {
                                    self.width = *w;
                                    self.height = *h;
                                }
                            }
                        }
                    });

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Width");
                    ui.add(egui::DragValue::new(&mut self.width).range(2..=8192).speed(2.0));
                    ui.add_space(12.0);
                    ui.label("Height");
                    ui.add(egui::DragValue::new(&mut self.height).range(2..=8192).speed(2.0));
                });

                ui.add_space(12.0);

                // ── Frame rate ──
                ui.label(egui::RichText::new("Frame rate").strong());
                ui.add_space(4.0);
                let fps_label = fps_options
                    .iter()
                    .position(|(_, n, d)| *n == self.fps_num && *d == self.fps_den)
                    .map(|i| fps_options[i].0)
                    .unwrap_or("Custom");
                egui::ComboBox::from_id_salt("seq_settings_fps")
                    .selected_text(fps_label)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (i, (label, n, d)) in fps_options.iter().enumerate() {
                            let is_selected = self.fps_num == *n && self.fps_den == *d;
                            if ui.selectable_label(is_selected, *label).clicked() {
                                self.fps_idx = i;
                                self.fps_num = *n;
                                self.fps_den = *d;
                            }
                        }
                    });

                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() { open = false; }
                    ui.add_space(8.0);
                    if ui.button(egui::RichText::new("Apply").strong()).clicked() {
                        apply = true;
                        open = false;
                    }
                });
            });

        if apply {
            if self.width != original_width || self.height != original_height {
                cmds.push(AppCommand::SetSequenceResolution {
                    sequence_idx: self.sequence_idx,
                    width: self.width,
                    height: self.height,
                });
            }
            if self.fps_num != original_fps_num || self.fps_den != original_fps_den {
                cmds.push(AppCommand::SetSequenceFps {
                    sequence_idx: self.sequence_idx,
                    fps_num: self.fps_num,
                    fps_den: self.fps_den,
                });
            }
        }

        (open, cmds)
    }
}
