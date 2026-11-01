//! Check Spelling in Titles (#3236): every word marked in the titles and
//! captions, in the sequence or every sequence, listed in order; a click
//! goes to one. For the one picked: what it might have been, and Change,
//! Change All, Skip, Ignore in This Project or Add to Dictionary. A change
//! is Find and Replace's, a whole word as written, so it keeps the text's
//! styled runs and is one undo step.

use crate::spelling::Found;
use crate::state::AppState;
use klipp_core::{AppCommand, Timecode};

/// The dialog's own state.
#[derive(Debug, Clone, Default)]
pub struct SpellingDialog {
    /// The row picked.
    pub selected: Option<usize>,
    /// What the picked word changes to.
    pub change_to: String,
    pub every_sequence: bool,
    /// Words passed over this time: (title, layer, start, word).
    pub skipped: Vec<(klipp_core::ClipId, u32, usize, String)>,
    /// The picked word's suggestions, by word and language.
    suggested: Option<(String, String, Vec<String>)>,
}

/// The dialog, while `state.show_spelling_dialog` is set. Its commands.
pub fn show_spelling_dialog(ctx: &egui::Context, state: &mut AppState) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let scope: Vec<usize> = if state.spelling_dialog.every_sequence { (0..state.sequences.len()).collect() } else { vec![state.active_sequence_idx] };
    let (found, missing) = crate::spelling::find_all(&state.spelling, ctx, &state.sequences, &scope);
    let dialog = &state.spelling_dialog;
    let found: Vec<Found> = found.into_iter().filter(|f| !dialog.skipped.iter().any(|(c, o, s, w)| *c == f.clip_id && *o == f.object.0 && *s == f.start && *w == f.word)).collect();
    let mut open = state.show_spelling_dialog;
    egui::Window::new("Check Spelling in Titles")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(480.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            if state.sequences.len() > 1 {
                ui.checkbox(&mut state.spelling_dialog.every_sequence, "Every sequence");
            }
            if !state.spelling.enabled {
                ui.label(egui::RichText::new("Spelling is off in Preferences → General.").weak());
            }
            for m in &missing {
                ui.label(egui::RichText::new(m).weak());
            }
            ui.label(match found.len() {
                0 => "No word is marked in the titles or captions.".to_string(),
                1 => "1 word marked".to_string(),
                n => format!("{n} words marked"),
            });
            // A row picked past the end, after a change, is the last one.
            if let Some(k) = state.spelling_dialog.selected {
                state.spelling_dialog.selected = (!found.is_empty()).then(|| k.min(found.len() - 1));
            }
            egui::ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                for (k, f) in found.iter().enumerate() {
                    let fps = state.sequences[f.sequence].fps.rate();
                    let at = Timecode::from_secs_f64(f.at_secs, fps);
                    let where_ = if state.spelling_dialog.every_sequence { format!("{} · ", state.sequences[f.sequence].name) } else { String::new() };
                    let label = format!("{where_}{}  {}  \u{2014} {}", state.timecode_display().position(at), f.word, f.line.trim());
                    if ui.selectable_label(state.spelling_dialog.selected == Some(k), label).clicked() {
                        state.spelling_dialog.selected = Some(k);
                        state.spelling_dialog.suggested = None;
                        if f.sequence != state.active_sequence_idx {
                            cmds.push(AppCommand::SetActiveSequence { sequence_idx: f.sequence });
                        }
                        cmds.push(AppCommand::SelectClip(Some(f.clip_id)));
                        cmds.push(AppCommand::SeekTo(at));
                    }
                }
            });
            let Some(f) = state.spelling_dialog.selected.and_then(|k| found.get(k)) else { return };
            ui.separator();
            // The picked word's suggestions, worked out once.
            let fresh = state.spelling_dialog.suggested.as_ref().is_some_and(|(w, l, _)| *w == f.word && *l == f.language);
            if !fresh {
                let suggestions = state.spelling.suggestions(&f.word, &f.language);
                state.spelling_dialog.change_to = suggestions.first().cloned().unwrap_or_else(|| f.word.clone());
                state.spelling_dialog.suggested = Some((f.word.clone(), f.language.clone(), suggestions));
            }
            let suggestions = state.spelling_dialog.suggested.as_ref().map(|s| s.2.clone()).unwrap_or_default();
            ui.label(format!("\u{201c}{}\u{201d} is not in the {} dictionary.", f.word, f.language));
            ui.horizontal_wrapped(|ui| {
                if suggestions.is_empty() {
                    ui.label(egui::RichText::new("No suggestions").weak());
                }
                for s in &suggestions {
                    if ui.selectable_label(state.spelling_dialog.change_to == *s, s).clicked() {
                        state.spelling_dialog.change_to = s.clone();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Change to");
                ui.text_edit_singleline(&mut state.spelling_dialog.change_to);
            });
            let to = state.spelling_dialog.change_to.clone();
            let can_change = !to.trim().is_empty() && to != f.word;
            let active = state.active_sequence_idx;
            ui.horizontal_wrapped(|ui| {
                let change = |only: bool, project: bool| AppCommand::ReplaceInTitles {
                    find: f.word.clone(),
                    replace: to.clone(),
                    match_case: true,
                    whole_word: true,
                    subtitles: true,
                    project,
                    only: only.then_some((f.clip_id, f.object.0, f.start, f.end)),
                };
                let to_its_sequence = |cmds: &mut Vec<AppCommand>| {
                    if f.sequence != active {
                        cmds.push(AppCommand::SetActiveSequence { sequence_idx: f.sequence });
                    }
                };
                if ui.add_enabled(can_change, egui::Button::new("Change")).on_hover_text("This one").clicked() {
                    to_its_sequence(&mut cmds);
                    cmds.push(change(true, false));
                }
                let times = found.iter().filter(|g| g.word == f.word).count();
                if ui.add_enabled(can_change, egui::Button::new(format!("Change All ({times})"))).on_hover_text("Every title and caption it is written in, as written").clicked() {
                    cmds.push(change(false, state.spelling_dialog.every_sequence));
                }
                if ui.button("Skip").on_hover_text("Leave this one as it is, this time").clicked() {
                    state.spelling_dialog.skipped.push((f.clip_id, f.object.0, f.start, f.word.clone()));
                }
                if ui.button("Ignore in This Project").on_hover_text("Taken as right in this project: it goes on the brief's vocabulary").clicked() {
                    to_its_sequence(&mut cmds);
                    cmds.push(AppCommand::SpellingIgnore { word: f.word.clone() });
                }
                if ui.button("Add to Dictionary").on_hover_text("Taken as right in every project").clicked() {
                    cmds.push(AppCommand::SpellingLearn { word: f.word.clone() });
                }
            });
        });
    if !open {
        // Closed, the next check starts afresh.
        state.spelling_dialog.skipped.clear();
        state.spelling_dialog.selected = None;
    }
    state.show_spelling_dialog = open;
    cmds
}

#[cfg(test)]
mod tests {
    use super::*;
    use klipp_timeline::{ClipKind, TextBox, TitleData};

    /// A sequence with two titles, one with "Teh" twice, against the test
    /// dictionaries.
    fn state() -> AppState {
        let mut state = AppState { spelling: crate::spelling::tests::test_spelling(), ..Default::default() };
        let t = |s: f64| klipp_core::RationalTime::from_secs_f64(s, 25);
        for (k, text) in ["Teh lazy dog", "Teh brown fox"].into_iter().enumerate() {
            let mut clip = klipp_timeline::Clip::gap(t(k as f64 * 2.0), t(k as f64 * 2.0 + 1.0));
            clip.kind = ClipKind::Title(TitleData::single_text(TextBox { text: text.into(), language: Some("en".into()), ..Default::default() }));
            state.sequences[0].tracks[0].clips.push(clip);
        }
        state
    }

    type Drawn = (Vec<AppCommand>, Vec<(String, egui::Pos2)>);

    /// One frame of the dialog: its commands, and each text drawn with
    /// where.
    fn frame_of(ctx: &egui::Context, clock: &mut f64, state: &mut AppState, events: Vec<egui::Event>) -> Drawn {
        *clock += 0.1;
        let mut cmds = Vec::new();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900.0, 700.0))), time: Some(*clock), events, ..Default::default() };
        let out = ctx.run(input, |ctx| cmds = show_spelling_dialog(ctx, state));
        let texts = out.shapes.iter().filter_map(|s| match &s.shape {
            egui::Shape::Text(t) => Some((t.galley.text().to_owned(), t.visual_bounding_rect().center())),
            _ => None,
        }).collect();
        (cmds, texts)
    }

    /// A click on the text `label` (else the one starting with it), pressed in a frame and let go
    /// in the next: what either sent.
    fn click(frame: &mut impl FnMut(&mut AppState, Vec<egui::Event>) -> Drawn, state: &mut AppState, label: &str) -> Vec<AppCommand> {
        let (_, texts) = frame(state, Vec::new());
        let at = texts.iter().find(|(t, _)| t == label).or_else(|| texts.iter().find(|(t, _)| t.starts_with(label))).map(|(_, p)| *p).unwrap_or_else(|| panic!("no {label:?} in {:?}", texts.iter().map(|t| &t.0).collect::<Vec<_>>()));
        let mut cmds = Vec::new();
        for pressed in [true, false] {
            cmds.extend(frame(state, vec![egui::Event::PointerMoved(at), egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]).0);
        }
        cmds
    }

    /// #3236's dialog: both words listed in order with their lines; a
    /// row picked goes to its title and offers the dictionary's word;
    /// Change is the Find and Replace of that word there alone, Change All
    /// of it everywhere, both whole and as written; Skip passes it over
    /// this time; Ignore sends its command.
    #[test]
    fn the_dialog_lists_the_words_and_changes_one_or_all() {
        let mut state = state();
        state.show_spelling_dialog = true;
        let ctx = egui::Context::default();
        let mut clock = 0.0;
        let mut frame = |state: &mut AppState, events: Vec<egui::Event>| frame_of(&ctx, &mut clock, state, events);
        // A window is laid out unseen in its first frame.
        frame(&mut state, Vec::new());
        let (_, texts) = frame(&mut state, Vec::new());
        assert!(texts.iter().any(|(t, _)| t == "2 words marked"), "{texts:?}");
        let rows: Vec<&String> = texts.iter().map(|(t, _)| t).filter(|t| t.contains("\u{2014}")).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].contains("Teh lazy dog") && rows[1].contains("Teh brown fox"), "{rows:?}");

        let second = state.sequences[0].tracks[0].clips[1].id;
        let rows: Vec<String> = rows.into_iter().cloned().collect();
        let cmds = click(&mut frame, &mut state, &rows[1]);
        assert!(cmds.iter().any(|c| matches!(c, AppCommand::SelectClip(Some(id)) if *id == second)), "goes to it: {cmds:?}");
        assert_eq!(state.spelling_dialog.change_to, "The", "the dictionary's word to change to");

        let cmds = click(&mut frame, &mut state, "Change All");
        assert!(matches!(&cmds[..], [AppCommand::ReplaceInTitles { find, replace, match_case: true, whole_word: true, subtitles: true, project: false, only: None }] if find == "Teh" && replace == "The"), "{cmds:?}");
        let cmds = click(&mut frame, &mut state, "Change");
        assert!(matches!(&cmds[..], [AppCommand::ReplaceInTitles { only: Some((id, 0, 0, 3)), .. }] if *id == second), "{cmds:?}");

        let cmds = click(&mut frame, &mut state, "Skip");
        assert!(cmds.is_empty());
        let (_, texts) = frame(&mut state, Vec::new());
        assert!(texts.iter().any(|(t, _)| t == "1 word marked"), "the skipped one passed over");
        assert_eq!(state.spelling_dialog.selected, Some(0), "the one left picked");
        let cmds = click(&mut frame, &mut state, "Ignore in This Project");
        assert!(matches!(&cmds[..], [AppCommand::SpellingIgnore { word }] if word == "Teh"), "{cmds:?}");
    }
}
