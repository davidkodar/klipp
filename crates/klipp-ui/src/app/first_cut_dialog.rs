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
        crate::widgets::window("First Cut")
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
        if run || cancel || !open {
            self.state.show_first_cut_dialog = false;
        }
        if run {
            self.make_first_cut(&c);
        }
    }

    /// The cut made as one undo step, and kept with the sequence as it was
    /// so the panel can take its changes back one at a time.
    fn make_first_cut(&mut self, choices: &crate::first_cut::Choices) {
        use klipp_document::ops::first_cut;
        self.transcript_source.sync(self.state.analysis_generation);
        let before = self.state.sequence().clone();
        let changes = first_cut::plan(&before, &self.state.media_pool, &self.transcript_source, &choices.steps());
        if changes.is_empty() {
            self.state.toast.show("First Cut: no clip is heard speaking. Transcribe or analyze its sound first.");
            return;
        }
        let ops = first_cut::ops(&before, &changes, &[], choices.music.as_ref());
        let plan = match klipp_document::ops::resolve_with(&before, &self.state.media_pool, &ops, &self.transcript_source) {
            Ok(plan) => plan,
            Err(r) => {
                self.state.toast.show(format!("First Cut: not done, {}", r.why));
                return;
            }
        };
        let edits: Vec<EditCommand> = plan.steps.into_iter().map(|s| s.edit).collect();
        self.apply_edits_as_one(&edits, "First Cut");
        let n = changes.len();
        let notes = crate::first_cut::found_nothing(choices, &changes);
        let mut session = crate::first_cut::Session { sequence: before.id, before, music: choices.music, changes, steps: Vec::new(), notes };
        session.record(self.state.active_history().undo_count(), Vec::new());
        self.state.first_cut_session = Some(session);
        self.state.show_first_cut_panel = true;
        self.state.toast.show(format!("First Cut: {n} changes, each listed in the First Cut panel"));
    }

    /// The changes First Cut's step in force leaves out: `None` unless
    /// that step is on top of the open sequence's history.
    fn first_cut_in_force(&self) -> Option<Vec<usize>> {
        let session = self.state.first_cut_session.as_ref()?;
        let history = self.state.active_history();
        session.in_force(self.state.sequence().id, history.undo_count(), history.undo_label()).map(<[usize]>::to_vec)
    }

    /// Change `i` taken back, or put back: the cut made again from the
    /// sequence as it was without the changes left out, swapped in as one
    /// undo step. Refused unless one of First Cut's own steps is in force.
    fn flip_first_cut_change(&mut self, i: usize) {
        let Some(session) = self.state.first_cut_session.clone() else { return };
        let Some(now) = self.first_cut_in_force() else {
            self.state.toast.show("First Cut: Undo or Redo back to its step to change it");
            return;
        };
        let excluded = crate::first_cut::Session::flipped(&now, i);
        self.transcript_source.sync(self.state.analysis_generation);
        let new = match klipp_document::ops::first_cut::tracks_without(&session.before, &self.state.media_pool, &self.transcript_source, &session.changes, &excluded, session.music.as_ref()) {
            Ok(tracks) => tracks,
            Err(r) => {
                self.state.toast.show(format!("First Cut: not changed, {}", r.why));
                return;
            }
        };
        let old = self.state.sequence().tracks.clone();
        let label = if excluded.contains(&i) { "First Cut: change taken back" } else { "First Cut: change put back" };
        self.apply_edits_as_one(&[EditCommand::SetTracks { description: "First Cut", old, new }], label);
        let depth = self.state.active_history().undo_count();
        if let Some(s) = self.state.first_cut_session.as_mut() {
            s.record(depth, excluded);
        }
    }

    /// The First Cut panel: each change with its time, ticked while it is
    /// in the cut. A click goes to it; unticking takes it back.
    pub(super) fn show_first_cut_panel(&mut self, ctx: &egui::Context, cmds: &mut Vec<AppCommand>) {
        let Some(session) = self.state.first_cut_session.clone() else { return };
        if !self.state.show_first_cut_panel {
            return;
        }
        // No First Cut step left to Undo or Redo to on this sequence: the
        // panel goes, its changes no longer to be had (FC7).
        let open_seq = self.state.sequence().id;
        let reachable = {
            let history = self.state.active_history();
            let (undo, redo) = (history.undo_entries(), history.redo_entries());
            let label_at = |d: usize| -> Option<&str> {
                if d >= 1 && d <= undo.len() {
                    Some(undo[d - 1].label())
                } else {
                    redo.len().checked_sub(d.checked_sub(undo.len())?).and_then(|i| redo.get(i)).map(|e| e.label())
                }
            };
            session.sequence != open_seq || session.reachable(open_seq, label_at)
        };
        if !reachable {
            self.state.show_first_cut_panel = false;
            self.state.first_cut_session = None;
            return;
        }
        let in_force = self.first_cut_in_force();
        let editable = in_force.is_some();
        let excluded = session.shown(in_force.as_deref()).to_vec();
        let rate = self.state.sequence().fps.num.max(1);
        let (mut open, mut flip, mut go) = (true, None, None);
        // At the top right under the menus, clear of the menu bar and the
        // Media panel (#3445).
        let screen = ctx.screen_rect();
        crate::widgets::window("First Cut changes")
            .constrain_to(crate::widgets::below_menu_bar(ctx))
            .collapsible(true)
            .resizable(true)
            .default_width(360.0)
            .pivot(egui::Align2::RIGHT_TOP)
            .default_pos(egui::pos2(screen.right() - 16.0, screen.top() + 72.0))
            .open(&mut open)
            .show(ctx, |ui| {
                if !editable {
                    ui.weak("The timeline is not at a First Cut step: Undo or Redo back to one to change it.");
                    ui.add_space(4.0);
                }
                for note in &session.notes {
                    ui.label(egui::RichText::new(note).weak());
                }
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for (i, change) in session.changes.iter().enumerate() {
                        ui.horizontal(|ui| {
                            let mut kept = !excluded.contains(&i);
                            if ui.add_enabled(editable, egui::Checkbox::without_text(&mut kept)).on_hover_text("Untick to take this change back").changed() {
                                flip = Some(i);
                            }
                            // Named by where it is now, where a click takes it
                            // (FC6: "pause at 0:10.7" went to 0:07.22).
                            let label = klipp_document::ops::first_cut::label_now(&session.changes, &excluded, i);
                            let text = if !excluded.contains(&i) { egui::RichText::new(label) } else { egui::RichText::new(label).weak() };
                            if change.kind.cuts() {
                                if ui.link(text).on_hover_text("Go to it").clicked() {
                                    go = Some(klipp_document::ops::first_cut::now_at(&session.changes, &excluded, i));
                                }
                            } else {
                                ui.label(text);
                            }
                        });
                    }
                });
            });
        if !open {
            self.state.show_first_cut_panel = false;
        }
        if let Some(at) = go {
            cmds.push(AppCommand::SeekTo(klipp_core::Timecode::from_secs_f64(at, rate)));
        }
        if let Some(i) = flip {
            self.flip_first_cut_change(i);
        }
    }
}
