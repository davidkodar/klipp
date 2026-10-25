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
use crate::widgets::menu::{SubmenuRows, MenuRows};

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
    /// A word being corrected (#531): its index and the text as typed.
    correcting: Option<(usize, String)>,
    /// Each transcribed clip heard on the timeline (#531): the media, its
    /// name, and the model and language its transcript was made with.
    transcribed: Vec<(klipp_core::MediaId, String, String, Option<String>)>,
}

/// The languages offered (#531), by ISO 639-1 code and name; Detect is
/// `None`.
const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"), ("sv", "Swedish"), ("no", "Norwegian"), ("da", "Danish"), ("fi", "Finnish"),
    ("de", "German"), ("fr", "French"), ("es", "Spanish"), ("it", "Italian"), ("pt", "Portuguese"),
    ("nl", "Dutch"), ("pl", "Polish"), ("ru", "Russian"), ("uk", "Ukrainian"), ("tr", "Turkish"),
    ("ar", "Arabic"), ("hi", "Hindi"), ("ja", "Japanese"), ("ko", "Korean"), ("zh", "Chinese"),
];

fn language_name(code: Option<&str>) -> String {
    match code {
        None => "Detect".to_string(),
        Some(c) => LANGUAGES.iter().find(|(k, _)| *k == c).map(|(_, n)| n.to_string()).unwrap_or_else(|| c.to_string()),
    }
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
        self.transcribed = heard_media(state).into_iter().filter_map(|id| {
            let item = state.media_pool.get(&id)?;
            let t = klipp_analysis::report::MediaAnalysis::load(&item.path).transcript?;
            Some((id, item.filename.clone(), t.model, t.language))
        }).collect();
        self.correcting = None;
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

        self.toolbar(ui, state, theme, cmds);
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
                        // Correcting a word whisper misheard (#531).
                        r.context_menu_rows(|ui| {
                            if ui.button("Correct\u{2026}").clicked() {
                                self.correcting = Some((i, word.text.clone()));
                                ui.close_menu();
                            }
                        });
                        if follow && Some(i) == current {
                            r.scroll_to_me(Some(egui::Align::Center));
                        }
                    }
                });
                ui.add_space(6.0);
            }
        });
        self.correction_editor(ui.ctx(), state, cmds);
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

    fn toolbar(&mut self, ui: &mut egui::Ui, state: &mut AppState, theme: &KlippTheme, cmds: &mut Vec<AppCommand>) {
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
        self.languages_and_models(ui, state, theme, cmds);
    }

    /// The project's language, and each clip's model and language with
    /// Re-transcribe (#531).
    fn languages_and_models(&mut self, ui: &mut egui::Ui, state: &AppState, theme: &KlippTheme, cmds: &mut Vec<AppCommand>) {
        let current = state.sequence().brief.as_ref().and_then(|b| b.language.clone());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Language").size(11.0).color(theme.text_secondary));
            egui::ComboBox::from_id_salt("transcript_language")
                .selected_text(language_name(current.as_deref()))
                .show_ui(ui, |ui| {
                    if ui.selectable_label(current.is_none(), "Detect").on_hover_text("Detected per file, from the first thirty seconds").clicked() {
                        cmds.push(AppCommand::SetTranscriptLanguage { language: None });
                    }
                    for (code, name) in LANGUAGES {
                        if ui.selectable_label(current.as_deref() == Some(*code), *name).clicked() {
                            cmds.push(AppCommand::SetTranscriptLanguage { language: Some((*code).to_string()) });
                        }
                    }
                })
                .response
                .on_hover_text("The language the speech is in, kept on the brief. Transcripts detected in another language are made again.");
        });
        if self.transcribed.is_empty() {
            return;
        }
        egui::CollapsingHeader::new(egui::RichText::new(format!("Transcribed clips ({})", self.transcribed.len())).size(11.0))
            .id_salt("transcript_clips")
            .show(ui, |ui| {
                let models = klipp_analysis::transcript::installed_models();
                for (media_id, name, model, language) in &self.transcribed {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(name).size(11.0));
                        ui.label(egui::RichText::new(format!("{} \u{00B7} {}", klipp_analysis::transcript::model_label(model), language_name(language.as_deref()))).size(10.0).color(theme.text_dim));
                        ui.menu_rows("Re-transcribe", |ui| {
                            for m in &models {
                                for (lang, label) in [(current.clone(), language_name(current.as_deref())), (None, "Detect".to_string())] {
                                    if ui.button(format!("{} in {label}", klipp_analysis::transcript::model_label(m))).clicked() {
                                        cmds.push(AppCommand::RetranscribeClip { media_id: *media_id, model: Some((*m).to_string()), language: lang.clone() });
                                        ui.close_menu();
                                    }
                                    if current.is_none() {
                                        break;
                                    }
                                }
                            }
                        });
                    });
                }
            });
    }

    /// The word being corrected (#531): its text in a small window;
    /// Enter or Correct saves it to the clip's transcript, Escape or
    /// Cancel leaves it.
    fn correction_editor(&mut self, ctx: &egui::Context, state: &AppState, cmds: &mut Vec<AppCommand>) {
        let Some((i, buf)) = self.correcting.as_mut() else { return };
        let i = *i;
        let mut done: Option<bool> = None;
        egui::Window::new("Correct Word")
            .id(egui::Id::new("transcript_correct_word"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                let field = ui.add(egui::TextEdit::singleline(buf).desired_width(220.0));
                field.request_focus();
                if ui.input(|inp| inp.key_pressed(egui::Key::Enter)) {
                    done = Some(true);
                }
                if ui.input(|inp| inp.key_pressed(egui::Key::Escape)) {
                    done = Some(false);
                }
                ui.horizontal(|ui| {
                    if ui.button("Correct").clicked() {
                        done = Some(true);
                    }
                    if ui.button("Cancel").clicked() {
                        done = Some(false);
                    }
                });
            });
        let Some(save) = done else { return };
        let text = self.correcting.take().map(|(_, b)| b).unwrap_or_default();
        if !save {
            return;
        }
        if let Some(cmd) = self.words.get(i).and_then(|w| correction_for(state, w, &text)) {
            cmds.push(cmd);
        }
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
            let label = format!("Cut {{n}} {}", sweep.title.to_lowercase());
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

/// The command that corrects `word` to `text` (#531): the word's moment
/// in its clip's media, by where the clip plays from and its speed.
fn correction_for(state: &AppState, word: &HeardWord, text: &str) -> Option<AppCommand> {
    if text.trim().is_empty() || text.trim() == word.text {
        return None;
    }
    let seq = state.sequence();
    let (ti, ci) = seq.find_clip(word.clip_id)?;
    let clip = &seq.tracks[ti].clips[ci];
    let media_id = clip.media_id()?;
    let mid = (word.start + word.end) / 2.0;
    let source_secs = clip.source_in.to_secs_f64() + (mid - clip.timeline_in.to_secs_f64()) * clip.speed.abs();
    Some(AppCommand::CorrectTranscriptWord { media_id, source_secs, text: text.trim().to_string() })
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

#[cfg(test)]
mod correction_tests {
    use super::correction_for;
    use crate::state::AppState;
    use klipp_core::{AppCommand, RationalTime};

    #[test]
    fn a_word_is_corrected_at_its_moment_in_the_clips_media() {
        let mut state = AppState::default();
        let tc = |secs: i64| RationalTime { value: secs * 24, rate: 24 };
        let media = klipp_core::MediaId::new();
        // Plays the media from 10 s, placed at 4 s on the timeline.
        let clip = klipp_timeline::Clip::media(media, tc(4), tc(8), tc(10), tc(14));
        let clip_id = clip.id;
        state.sequences[0].tracks[0].clips.push(clip);
        let word = klipp_document::ops::HeardWord { start: 5.0, end: 5.5, text: "matchy".into(), after_cut: false, clip_id, media_id: media, role: klipp_core::ClipRole::Dialogue, clip_in: 4.0, clip_out: 8.0 };
        let Some(AppCommand::CorrectTranscriptWord { media_id, source_secs, text }) = correction_for(&state, &word, " Omarchy ") else { panic!("a correction") };
        assert_eq!((media_id, text.as_str()), (media, "Omarchy"));
        assert!((source_secs - 11.25).abs() < 1e-9, "the word's middle, 1.25 s into the clip, is 11.25 s into its media: {source_secs}");
        assert!(correction_for(&state, &word, "matchy").is_none(), "the same text changes nothing");
        assert!(correction_for(&state, &word, "  ").is_none());
    }
}
