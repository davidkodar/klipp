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
    /// The sequence camera's shutter: degrees (0 off) and samples.
    pub shutter_deg: f32,
    pub shutter_samples: u8,
    /// A title's white in an HDR delivery, cd/m².
    pub title_nits: f32,
}

impl SequenceSettingsDialog {
    pub fn open(sequence_idx: usize, width: u32, height: u32, fps_num: u32, fps_den: u32, (shutter_deg, shutter_samples): (f32, u8), title_nits: f32) -> Self {
        let resolution_idx = RESOLUTION_PRESETS
            .iter()
            .position(|(_, w, h)| *w == width && *h == height)
            .unwrap_or_else(|| RESOLUTION_PRESETS.len() - 1); // "Custom..."
        let fps_idx = FPS_PRESETS
            .iter()
            .position(|(_, n, d)| *n == fps_num && *d == fps_den)
            .unwrap_or_else(|| FPS_PRESETS.len() - 1);
        Self { sequence_idx, width, height, fps_num, fps_den, resolution_idx, fps_idx, shutter_deg, shutter_samples, title_nits }
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
        original_shutter: (f32, u8),
        original_title_nits: f32,
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

                ui.add_space(12.0);

                // ── Motion blur ──
                ui.label(egui::RichText::new("Motion blur").strong());
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Shutter");
                    ui.add(egui::DragValue::new(&mut self.shutter_deg).range(0.0..=360.0).speed(1.0).suffix("\u{00b0}"))
                        .on_hover_text("How long the shutter stays open, as a share of a frame. 180\u{00b0} is the film look; 0 is no motion blur.");
                    ui.add_space(12.0);
                    ui.label("Samples");
                    ui.add_enabled(self.shutter_deg > 0.0, egui::DragValue::new(&mut self.shutter_samples).range(2..=32))
                        .on_hover_text("Instants blended across the open shutter. More is smoother and slower to render.");
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    for (label, deg) in [("Off", 0.0), ("90\u{00b0}", 90.0), ("180\u{00b0}", 180.0), ("360\u{00b0}", 360.0)] {
                        if ui.selectable_label(self.shutter_deg == deg, label).clicked() {
                            self.shutter_deg = deg;
                        }
                    }
                });

                ui.add_space(12.0);

                // ── HDR ──
                ui.label(egui::RichText::new("HDR").strong());
                ui.add_space(4.0);
                let range = klipp_timeline::TITLE_NITS_RANGE;
                ui.horizontal(|ui| {
                    ui.label("Title brightness");
                    ui.add(egui::DragValue::new(&mut self.title_nits).range(*range.start()..=*range.end()).speed(1.0).suffix(" nits"))
                        .on_hover_text(
                            "How bright a title's white is in an HDR (PQ or HLG) export and in the viewer's HDR preview. \
                             203 nits is where SDR white sits; brighter stands out more. Held under the export's peak. \
                             An SDR export is not affected.",
                        );
                    if ui.small_button("Reset").on_hover_text("Back to 203 nits, SDR white").clicked() {
                        self.title_nits = klipp_timeline::DEFAULT_TITLE_NITS;
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
            if (self.shutter_deg, self.shutter_samples) != original_shutter {
                cmds.push(AppCommand::SetSequenceShutter {
                    sequence_idx: self.sequence_idx,
                    shutter_deg: self.shutter_deg,
                    samples: self.shutter_samples,
                });
            }
            if self.title_nits != original_title_nits {
                cmds.push(AppCommand::SetSequenceTitleNits { sequence_idx: self.sequence_idx, nits: self.title_nits });
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_offers_the_shutter_and_says_nothing_when_it_is_left_alone() {
        let ctx = egui::Context::default();
        let mut dlg = SequenceSettingsDialog::open(0, 1920, 1080, 24, 1, (180.0, 8), 203.0);
        let mut text = String::new();
        let mut cmds = Vec::new();
        // Two passes: egui lays a window out before it paints it.
        for _ in 0..2 {
            let out = ctx.run(egui::RawInput::default(), |ctx| {
                let (_, c) = dlg.show(ctx, 1920, 1080, 24, 1, (180.0, 8), 203.0);
                cmds = c;
            });
            text.clear();
            fn walk(shape: &egui::Shape, out: &mut String) {
                match shape {
                    egui::Shape::Text(t) => {
                        out.push_str(t.galley.text());
                        out.push('\n');
                    }
                    egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            out.shapes.iter().for_each(|c| walk(&c.shape, &mut text));
        }
        for label in ["Motion blur", "Shutter", "Samples", "Title brightness"] {
            assert!(text.contains(label), "no {label} in:\n{text}");
        }
        assert!(cmds.is_empty(), "nothing is sent until Apply");
    }
}
