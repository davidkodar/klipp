//! First Cut's button gate and its dialog (#3308): what it asks, and the
//! `first_cut` op it sends as one undo step.

use super::*;
use klipp_document::ops::first_cut::Tightness;

impl KlippApp {
    /// Whether a clip with speech is on the timeline, for the toolbar's
    /// button: worked out again only when the sequence, its cut or the
    /// analysis changes, never every frame.
    pub(super) fn refresh_first_cut_gate(&mut self) {
        let key = {
            let seq = self.state.sequence();
            (seq.id, seq.cut_stamp(), self.state.analysis_generation)
        };
        if self.first_cut_gate_key == Some(key) {
            return;
        }
        self.first_cut_gate_key = Some(key);
        self.transcript_source.sync(self.state.analysis_generation);
        self.state.first_cut_speech = klipp_document::ops::first_cut::has_speech(self.state.sequence(), &self.state.media_pool, &self.transcript_source);
    }

    /// The dialog: the steps, how tight the pauses are cut, a music bed
    /// only when chosen, and the files to transcribe first. Making it
    /// sends the `first_cut` op as one undo step.
    pub(super) fn show_first_cut_dialog(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        if !self.state.show_first_cut_dialog {
            return;
        }
        self.transcript_source.sync(self.state.analysis_generation);
        let untranscribed = crate::first_cut::untranscribed(self.state.sequence(), &self.state.media_pool, &self.transcript_source);
        let music = crate::first_cut::music_choices(&self.state.media_pool);
        let speech = self.state.first_cut_speech;
        let mut c = self.state.first_cut_choices.clone();
        let (mut open, mut run, mut cancel, mut transcribe) = (true, false, false, false);
        egui::Window::new("First Cut")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.set_min_width(340.0);
                ui.label("A rough cut of what is said, as one undo step.");
                ui.add_space(6.0);
                ui.checkbox(&mut c.silences, "Take out pauses");
                ui.add_enabled_ui(c.silences, |ui| {
                    ui.horizontal(|ui| {
                        ui.add_space(22.0);
                        ui.radio_value(&mut c.tightness, Tightness::Gentle, "Gentle").on_hover_text("Pauses of a second or more");
                        ui.radio_value(&mut c.tightness, Tightness::Normal, "Normal").on_hover_text("Pauses of 0.6 s or more");
                        ui.radio_value(&mut c.tightness, Tightness::Tight, "Tight").on_hover_text("Pauses of 0.35 s or more");
                    });
                });
                ui.checkbox(&mut c.fillers, "Take out filler words");
                ui.checkbox(&mut c.retakes, "Keep the last take of a line said twice");
                ui.checkbox(&mut c.punch, "Punch in on every other piece of a shot");
                ui.checkbox(&mut c.captions, "Add captions, where there are none");
                ui.horizontal(|ui| {
                    ui.label("Music bed:");
                    let name = c.music.and_then(|id| music.iter().find(|(m, _)| *m == id)).map(|(_, n)| n.clone()).unwrap_or_else(|| "None".into());
                    egui::ComboBox::from_id_salt("first_cut_music").selected_text(name).show_ui(ui, |ui| {
                        ui.selectable_value(&mut c.music, None, "None");
                        for (id, n) in &music {
                            ui.selectable_value(&mut c.music, Some(*id), n);
                        }
                    });
                });
                if music.is_empty() {
                    ui.weak("Import a sound file to offer it as a music bed.");
                }
                if !untranscribed.is_empty() {
                    ui.add_space(6.0);
                    let names: Vec<&str> = untranscribed.iter().map(|(_, n)| n.as_str()).collect();
                    ui.label(format!("Not transcribed yet: {}. Their pauses, filler words and retakes are found once they are.", names.join(", ")));
                    if ui.button("Transcribe them").clicked() {
                        transcribe = true;
                    }
                }
                if !speech {
                    ui.add_space(6.0);
                    ui.weak("No clip with speech is on the timeline yet.");
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(speech && c.any(), egui::Button::new("Make the First Cut")).clicked() {
                        run = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        self.state.first_cut_choices = c.clone();
        if transcribe {
            cmds.extend(untranscribed.iter().map(|&(media_id, _)| AppCommand::TranscribeClip { media_id }));
        }
        if run {
            self.state.pending_doc_ops.push(crate::state::DocOpsRequest { label: "First Cut".into(), ops: vec![c.op()], independent: false });
        }
        if run || cancel || !open {
            self.state.show_first_cut_dialog = false;
        }
    }
}
