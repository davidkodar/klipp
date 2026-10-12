//! Find and replace in titles (#1717): what to look for and its
//! replacement, match case and whole word, the subtitles too, over the
//! sequence or every sequence; the matches listed, a click going to one;
//! replace the one picked, or all, as one undo step in each sequence.

use crate::state::AppState;
use klipp_core::{AppCommand, Timecode};
use klipp_document::find::{find_in_titles, TextMatch};

/// The dialog, while `state.show_title_find` is set. Its commands.
pub fn show_title_find_dialog(ctx: &egui::Context, state: &mut AppState) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    // The matches, by sequence: the active one, or all of them.
    let scope: Vec<usize> = if state.title_find_project { (0..state.sequences.len()).collect() } else { vec![state.active_sequence_idx] };
    let matches: Vec<(usize, TextMatch)> = scope.iter().flat_map(|&i| find_in_titles(&state.sequences[i], &state.title_find).into_iter().map(move |m| (i, m))).collect();
    let mut open = state.show_title_find;
    egui::Window::new("Find and Replace in Titles")
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(460.0)
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            egui::Grid::new("title_find_fields").num_columns(2).show(ui, |ui| {
                ui.label("Find");
                if ui.text_edit_singleline(&mut state.title_find.find).changed() {
                    state.title_find_selected = None;
                }
                ui.end_row();
                ui.label("Replace with");
                ui.text_edit_singleline(&mut state.title_find_replace);
                ui.end_row();
            });
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut state.title_find.match_case, "Match case");
                ui.checkbox(&mut state.title_find.whole_word, "Whole word");
                ui.checkbox(&mut state.title_find.subtitles, "Subtitles too");
                if state.sequences.len() > 1 {
                    ui.checkbox(&mut state.title_find_project, "Every sequence");
                }
            });
            ui.separator();
            if state.title_find.find.is_empty() {
                ui.label(egui::RichText::new("Type the words to find.").weak());
            } else {
                ui.label(format!("{} match{}", matches.len(), if matches.len() == 1 { "" } else { "es" }));
            }
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                for (k, (seq_idx, m)) in matches.iter().enumerate() {
                    let fps = state.sequences[*seq_idx].fps.rate();
                    let at = Timecode::from_secs_f64(m.at_secs, fps);
                    let where_ = if state.title_find_project { format!("{} · ", state.sequences[*seq_idx].name) } else { String::new() };
                    let label = format!("{where_}{}  {}", state.timecode_display().position(at), m.line.trim());
                    if ui.selectable_label(state.title_find_selected == Some(k), label).clicked() {
                        state.title_find_selected = Some(k);
                        // Go to it: its sequence, the title selected, the
                        // playhead on it.
                        if *seq_idx != state.active_sequence_idx {
                            cmds.push(AppCommand::SetActiveSequence { sequence_idx: *seq_idx });
                        }
                        cmds.push(AppCommand::SelectClip(Some(m.clip_id)));
                        cmds.push(AppCommand::SeekTo(at));
                    }
                }
            });
            ui.separator();
            ui.horizontal(|ui| {
                let replace = |only: Option<&TextMatch>, project: bool| AppCommand::ReplaceInTitles {
                    find: state.title_find.find.clone(),
                    replace: state.title_find_replace.clone(),
                    match_case: state.title_find.match_case,
                    whole_word: state.title_find.whole_word,
                    subtitles: state.title_find.subtitles,
                    project,
                    only: only.map(|m| (m.clip_id, m.object.0, m.start, m.end)),
                };
                let picked = state.title_find_selected.and_then(|k| matches.get(k));
                if ui.add_enabled(picked.is_some(), egui::Button::new("Replace")).on_disabled_hover_text("Pick a match in the list").clicked() {
                    if let Some((seq_idx, m)) = picked {
                        // The one picked, in its own sequence.
                        if *seq_idx != state.active_sequence_idx {
                            cmds.push(AppCommand::SetActiveSequence { sequence_idx: *seq_idx });
                        }
                        cmds.push(replace(Some(m), false));
                    }
                }
                if ui.add_enabled(!matches.is_empty(), egui::Button::new(format!("Replace All ({})", matches.len()))).clicked() {
                    cmds.push(replace(None, state.title_find_project));
                }
            });
        });
    state.show_title_find = open;
    cmds
}
