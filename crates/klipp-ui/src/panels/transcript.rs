//! The Transcript panel: what is said on the timeline, as text.
//!
//! The words are the ones the timeline plays — each clip's transcript
//! moved to where the clip puts it, from the clips whose sound is heard —
//! gathered into paragraphs at pauses and cuts. The word being said at
//! the playhead is marked, and followed while playing. Clicking a word
//! seeks to it; shift-click selects a run of words, and deleting them
//! cuts that stretch out of every unlocked track and closes up, as one
//! undo step. The sweeps offer every filler and every long pause as cuts
//! to review and apply together.
//!
//! Edits go out as document ops ([`klipp_document::ops::Op::Cut`]), the
//! same vocabulary the assistant uses, so a cut made here and one an
//! agent makes are the same edit.

use klipp_core::AppCommand;
use klipp_document::ops::HeardWord;

use crate::state::{AppState, DocOpsRequest};
use crate::theme::KlippTheme;
use crate::transcript::{self, format_secs, Sweep, TranscriptSource};

#[derive(Default)]
pub struct TranscriptPanel {
    /// The words, and the fingerprint of the timeline and analyses they
    /// were worked out for.
    words: Vec<HeardWord>,
    made_for: Option<u64>,
    /// Media on the timeline whose sound is heard but not transcribed.
    untranscribed: Vec<klipp_core::MediaId>,
    /// Selected words: where the selection started, and where it ends.
    selection: Option<(usize, usize)>,
    follow: bool,
    /// The sweep under review: each proposed cut, and whether it is ticked.
    sweep: Option<Sweep>,
    pause_longer_than: f64,
}

impl TranscriptPanel {
    pub fn new() -> Self {
        Self { follow: true, pause_longer_than: 1.0, ..Self::default() }
    }

    /// Draw the panel. Cuts go into `state.pending_doc_ops`; seeking and
    /// transcribing come back as commands.
    pub fn show(&mut self, ctx: &egui::Context, state: &mut AppState, source: &TranscriptSource, theme: &KlippTheme) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        self.refresh(state, source);
        let mut open = state.show_transcript_panel;
        let window = egui::Window::new("Transcript")
            .open(&mut open)
            .default_width(460.0)
            .default_height(520.0)
            .resizable(true)
            .show(ctx, |ui| self.contents(ui, state, theme, &mut cmds));
        // For the next frame: the global shortcuts run before the panel
        // draws, so they and the panel both go by the last frame's answer.
        state.transcript_panel_hovered = open && window.is_some_and(|w| w.response.contains_pointer());
        state.show_transcript_panel = open;
        cmds
    }

    /// Work the words out again when the timeline or the analyses changed.
    /// A sweep under review is fitted to the new words rather than
    /// dropped: a transcription landing, or an agent's edit over the
    /// socket, should not throw away forty ticks' worth of reviewing.
    fn refresh(&mut self, state: &mut AppState, source: &TranscriptSource) {
        let key = fingerprint(state);
        if self.made_for == Some(key) {
            return;
        }
        source.sync(state.analysis_generation);
        let seq = state.sequence();
        self.words = klipp_document::ops::heard_words(seq, &state.media_pool, source);
        self.untranscribed = heard_media(state).into_iter().filter(|id| {
            state.media_pool.get(id).is_some_and(|item| klipp_document::ops::Transcripts::words(source, item).is_none())
        }).collect();
        self.made_for = Some(key);
        // Indices into the old words mean nothing now.
        self.selection = None;
        if let Some(sweep) = &mut self.sweep {
            let was = sweep.cuts.len();
            let dropped = sweep.refit(&self.words);
            if dropped > 0 {
                state.toast.show(format!("{}: the timeline changed; {dropped} of the {was} cuts no longer apply and were dropped", sweep.title));
            }
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, cmds: &mut Vec<AppCommand>) {
        if !self.untranscribed.is_empty() {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} clip(s) on the timeline have no transcript yet.", self.untranscribed.len()))
                        .color(theme.text_secondary),
                );
                if ui.button("Transcribe").on_hover_text("Run speech-to-text on them now, whatever the preference").clicked() {
                    cmds.extend(self.untranscribed.iter().map(|&media_id| AppCommand::TranscribeClip { media_id }));
                }
            });
            ui.separator();
        }
        if self.words.is_empty() {
            ui.label("Nothing is said on the timeline yet — or nothing on it has been transcribed.");
            return;
        }

        self.toolbar(ui, state, theme);
        ui.separator();
        if let Some(sweep) = &mut self.sweep {
            if review_sweep(ui, sweep, state) {
                self.sweep = None;
            }
            return;
        }

        let playhead = state.playhead.to_secs_f64();
        let current = transcript::word_at(&self.words, playhead).filter(|&i| playhead < self.words[i].end + transcript::PARAGRAPH_PAUSE_SECS);
        let selected = self.selection.map(|(a, b)| (a.min(b), a.max(b)));
        let starts = transcript::paragraph_starts(&self.words);
        let follow = self.follow && state.playback == klipp_engine::PlaybackState::Playing;
        let shift = ui.input(|i| i.modifiers.shift);
        let mut clicked = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (p, &start) in starts.iter().enumerate() {
                let end = starts.get(p + 1).copied().unwrap_or(self.words.len());
                ui.label(egui::RichText::new(format_secs(self.words[start].start)).size(10.0).color(theme.text_dim));
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for i in start..end {
                        let word = &self.words[i];
                        let in_selection = selected.is_some_and(|(a, b)| (a..=b).contains(&i));
                        let mut text = egui::RichText::new(&word.text);
                        if Some(i) == current {
                            text = text.color(theme.accent).strong();
                        }
                        if transcript::is_filler(&word.text) {
                            text = text.italics();
                        }
                        let r = ui.selectable_label(in_selection, text);
                        if r.clicked() {
                            clicked = Some(i);
                        }
                        if follow && Some(i) == current {
                            r.scroll_to_me(Some(egui::Align::Center));
                        }
                    }
                });
                ui.add_space(6.0);
            }
        });
        if let Some(i) = clicked {
            self.selection = match (shift, self.selection) {
                (true, Some((anchor, _))) => Some((anchor, i)),
                _ => Some((i, i)),
            };
            let fps = state.sequence().fps;
            let frames = (self.words[i].start * fps.as_f64()).round() as i64;
            cmds.push(AppCommand::SeekTo(klipp_core::Timecode { value: frames * i64::from(fps.den), rate: fps.num }));
        }

        // Delete / Backspace over the panel cuts the selection. The
        // global Delete shortcut, which ran earlier this frame, stood
        // aside on the same flag, so the key is taken once.
        let delete = state.transcript_panel_hovered
            && !ui.ctx().wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace));
        if delete {
            self.cut_selection(state);
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme) {
        ui.horizontal_wrapped(|ui| {
            let selected = self.selection.map(|(a, b)| (a.min(b), a.max(b)));
            let label = match selected {
                Some((a, b)) => format!("Cut {} Word{}", b - a + 1, if a == b { "" } else { "s" }),
                None => "Cut Words".to_string(),
            };
            if ui
                .add_enabled(selected.is_some(), egui::Button::new(label))
                .on_hover_text("Take the selected words out of every unlocked track and close up (Delete)")
                .clicked()
            {
                self.cut_selection(state);
            }
            if ui.button("Fillers\u{2026}").on_hover_text("Every um and uh, to review and cut together").clicked() {
                self.sweep = Some(Sweep::fillers(&self.words));
            }
            ui.add(egui::DragValue::new(&mut self.pause_longer_than).range(0.5..=10.0).speed(0.05).suffix(" s"))
                .on_hover_text("How long a pause has to be to count as long");
            if ui.button("Long Pauses\u{2026}").on_hover_text("Every pause longer than that, cut down to 0.4 s").clicked() {
                self.sweep = Some(Sweep::pauses(&self.words, self.pause_longer_than, 0.4));
            }
            ui.checkbox(&mut self.follow, "Follow").on_hover_text("Keep the word being said in view while playing");
            ui.label(egui::RichText::new(format!("{} words", self.words.len())).size(10.0).color(theme.text_dim));
        });
    }

    fn cut_selection(&mut self, state: &mut AppState) {
        let Some((a, b)) = self.selection.map(|(a, b)| (a.min(b), a.max(b))) else { return };
        let range = transcript::cut_for_words(&self.words, a, b);
        let label = if a == b { format!("Cut \u{201C}{}\u{201D}", self.words[a].text) } else { format!("Cut {} words", b - a + 1) };
        state.pending_doc_ops.push(DocOpsRequest { label, ops: transcript::cut_ops(vec![range]), independent: false });
        self.selection = None;
    }
}

/// Review a sweep: each cut with a tick, and apply the ticked ones as one
/// step. `true` when the review is over.
fn review_sweep(ui: &mut egui::Ui, sweep: &mut Sweep, state: &mut AppState) -> bool {
    ui.label(egui::RichText::new(format!("{}: {} found", sweep.title, sweep.cuts.len())).strong());
    if sweep.cuts.is_empty() {
        ui.label("Nothing to cut.");
        return ui.button("Back").clicked();
    }
    let mut done = false;
    ui.horizontal(|ui| {
        let ticked = sweep.ticked();
        if ui.add_enabled(!ticked.is_empty(), egui::Button::new(format!("Cut {} Ticked", ticked.len()))).clicked() {
            // Each cut stands on its own: one that is refused (through a
            // retimed clip, say) is reported, and the rest are made.
            let label = format!("Cut {} {}", ticked.len(), sweep.title.to_lowercase());
            state.pending_doc_ops.push(DocOpsRequest { label, ops: transcript::cut_ops(ticked), independent: true });
            done = true;
        }
        if ui.button("Back").clicked() {
            done = true;
        }
    });
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for cut in &mut sweep.cuts {
            ui.checkbox(&mut cut.ticked, cut.label.as_str());
        }
    });
    done
}

/// The media whose sound the timeline plays.
fn heard_media(state: &AppState) -> Vec<klipp_core::MediaId> {
    let mut ids: Vec<klipp_core::MediaId> = state
        .sequence()
        .tracks
        .iter()
        .filter(|t| matches!(t.kind, klipp_timeline::TrackKind::Audio | klipp_timeline::TrackKind::Video) && !t.muted)
        .flat_map(|t| t.clips.iter())
        .filter_map(|c| c.media_id())
        .filter(|id| state.media_pool.get(id).is_some_and(|m| m.has_audio && !m.offline))
        .collect();
    ids.sort_unstable_by_key(|id| id.0);
    ids.dedup();
    ids
}

/// What the words depend on: where every clip sits and plays from, the
/// tracks' mute, the analyses. Cheap enough for every frame.
fn fingerprint(state: &AppState) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    state.analysis_generation.hash(&mut h);
    state.active_sequence_idx.hash(&mut h);
    for t in &state.sequence().tracks {
        (t.muted, t.clips.len()).hash(&mut h);
        for c in &t.clips {
            (c.id, c.timeline_in.value, c.timeline_in.rate, c.source_in.value, c.source_out.value, c.enabled, c.linked_clip_id).hash(&mut h);
            c.speed.to_bits().hash(&mut h);
            c.volume.to_bits().hash(&mut h);
        }
    }
    h.finish()
}
