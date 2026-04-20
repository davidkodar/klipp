//! "Change Project Frame Rate?" modal — shown when the user drops a clip
//! onto a sequence whose frame rate differs from the media's, and the
//! sequence already has clips on it (so the silent first-clip auto-set
//! path in `AppendClip*` / `ImportMediaLinked` does not apply).
//!
//! Mirrors the DaVinci Resolve prompt: "Change" updates the sequence fps
//! and proceeds with the drop; "Don't Change" proceeds without changing
//! fps; closing the dialog cancels the drop entirely.

use klipp_core::{AppCommand, Fps};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameRateMismatchChoice {
    Change,
    DontChange,
    Cancel,
}

pub struct FrameRateMismatchDialog {
    pub sequence_idx: usize,
    pub media_fps: Fps,
    pub sequence_fps: Fps,
    /// The append/import command that triggered the prompt. Re-dispatched
    /// after the user picks Change / Don't Change, with the app's
    /// `suppress_fps_prompt` flag set so we don't re-prompt in a loop.
    pub pending_cmd: AppCommand,
}

impl FrameRateMismatchDialog {
    pub fn show(&mut self, ctx: &egui::Context) -> Option<FrameRateMismatchChoice> {
        let mut choice: Option<FrameRateMismatchChoice> = None;

        egui::Area::new(egui::Id::new("fps_mismatch_backdrop"))
            .fixed_pos(egui::pos2(0.0, 0.0))
            .order(egui::Order::PanelResizeLine)
            .show(ctx, |ui| {
                let screen = ctx.screen_rect();
                let resp = ui.allocate_rect(screen, egui::Sense::click());
                ui.painter().rect_filled(screen, 0.0, egui::Color32::from_black_alpha(120));
                if resp.clicked() {
                    choice = Some(FrameRateMismatchChoice::Cancel);
                }
            });

        egui::Window::new("Change Project Frame Rate?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.add_space(4.0);
                ui.label(format!(
                    "The selected clip has a different frame rate to the project. \
                     Would you like to change the project frame rate to {:.2} frames per second?",
                    self.media_fps.as_f64()
                ));
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Project: {:.3} fps   ·   Clip: {:.3} fps",
                        self.sequence_fps.as_f64(),
                        self.media_fps.as_f64()
                    ))
                    .weak(),
                );
                ui.add_space(16.0);
                ui.horizontal(|ui| {
                    if ui.button("Don't Change").clicked() {
                        choice = Some(FrameRateMismatchChoice::DontChange);
                    }
                    ui.add_space(8.0);
                    if ui.button(egui::RichText::new("Change").strong()).clicked() {
                        choice = Some(FrameRateMismatchChoice::Change);
                    }
                });
            });

        choice
    }
}
