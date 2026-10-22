use crate::state::AppState;
use crate::theme::KlippTheme;
use klipp_core::AppCommand;

const TAB_HEIGHT: f32 = 24.0;
const TAB_PAD_H: f32 = 12.0;
const TAB_GAP: f32 = 1.0;
const CLOSE_SIZE: f32 = 14.0;
const ADD_BTN_W: f32 = 28.0;

/// Draw the sequence tab bar. Returns commands and the height consumed.
pub fn show(
    ui: &mut egui::Ui,
    state: &mut AppState,
    theme: &KlippTheme,
) -> Vec<AppCommand> {
    let mut cmds = Vec::new();
    let rect = ui.max_rect();

    // Background
    ui.painter().rect_filled(rect, 0.0, theme.bg_panel);
    ui.painter().hline(
        rect.x_range(),
        rect.max.y,
        egui::Stroke::new(1.0_f32, theme.border),
    );

    let mut x = rect.min.x + 4.0;
    let y = rect.min.y;

    // Render each open sequence tab
    let open_tabs = open_tab_count(state);
    for i in 0..state.sequences.len() {
        let is_active = i == state.active_sequence_idx;
        if !is_active && state.closed_sequence_tabs.contains(&state.sequences[i].id) {
            continue;
        }
        let name = state.sequences[i].name.clone();

        // Measure text width
        let font = egui::FontId::proportional(11.0);
        let text_width = ui.painter().layout_no_wrap(
            name.clone(),
            font.clone(),
            theme.text_primary,
        ).rect.width();

        let tab_w = text_width + TAB_PAD_H * 2.0 + CLOSE_SIZE + 4.0;
        let tab_rect = egui::Rect::from_min_size(
            egui::pos2(x, y),
            egui::vec2(tab_w, TAB_HEIGHT),
        );

        // Interaction
        let resp = ui.allocate_rect(tab_rect, egui::Sense::click());
        let hovered = resp.hovered();

        // Background
        let bg = if is_active {
            theme.bg_raised
        } else if hovered {
            theme.bg_hover
        } else {
            theme.bg_panel
        };
        ui.painter().rect_filled(tab_rect, 0.0, bg);

        // Active indicator — amber bottom border
        if is_active {
            ui.painter().hline(
                tab_rect.x_range(),
                tab_rect.max.y - 1.0,
                egui::Stroke::new(2.0_f32, theme.accent),
            );
        }

        // Tab name
        let text_color = if is_active {
            theme.text_primary
        } else if hovered {
            theme.text_secondary
        } else {
            theme.text_dim
        };
        ui.painter().text(
            egui::pos2(tab_rect.min.x + TAB_PAD_H, tab_rect.center().y),
            egui::Align2::LEFT_CENTER,
            &name,
            font,
            text_color,
        );

        // Close button (x)
        let close_rect = egui::Rect::from_min_size(
            egui::pos2(tab_rect.max.x - CLOSE_SIZE - 4.0, tab_rect.center().y - CLOSE_SIZE / 2.0),
            egui::vec2(CLOSE_SIZE, CLOSE_SIZE),
        );
        let can_close = open_tabs > 1;
        let close_resp = ui.allocate_rect(close_rect, if can_close { egui::Sense::click() } else { egui::Sense::hover() });
        if can_close && (close_resp.hovered() || is_active) {
            let close_color = if close_resp.hovered() {
                theme.text_primary
            } else {
                theme.text_dim
            };
            // Draw X
            let cx = close_rect.center();
            let s = 3.5;
            ui.painter().line_segment(
                [egui::pos2(cx.x - s, cx.y - s), egui::pos2(cx.x + s, cx.y + s)],
                egui::Stroke::new(1.2_f32, close_color),
            );
            ui.painter().line_segment(
                [egui::pos2(cx.x + s, cx.y - s), egui::pos2(cx.x - s, cx.y + s)],
                egui::Stroke::new(1.2_f32, close_color),
            );
        }

        // Click to switch
        if resp.clicked() && !close_resp.hovered() && !is_active {
            cmds.push(AppCommand::SetActiveSequence { sequence_idx: i });
        }

        // × closes the tab: the sequence stays in the project (#906).
        // It deleted the sequence, with no question and no undo.
        let close_resp = close_resp.on_hover_text("Close tab (the sequence stays in the project)");
        if can_close && close_resp.clicked() {
            state.closed_sequence_tabs.insert(state.sequences[i].id);
            if is_active {
                if let Some(next) = tab_after_closing(state, i) {
                    cmds.push(AppCommand::SetActiveSequence { sequence_idx: next });
                }
            }
        }

        // Right-click context menu
        resp.context_menu(|ui| {
            if ui.button("Rename...").clicked() {
                state.renaming_sequence_idx = Some(i);
                state.rename_sequence_buf = name.clone();
                ui.close_menu();
            }
            if ui.button("Duplicate").clicked() {
                cmds.push(AppCommand::DuplicateSequence {
                    sequence_idx: i,
                    new_name: format!("{} (copy)", name),
                });
                ui.close_menu();
            }
            // A copy at another aspect, each picture following its subject
            // (#2211).
            ui.menu_button("New Version at Aspect", |ui| {
                use crate::app::aspect_version::{aspect_size, has_aspect, ASPECTS};
                let frame = state.sequences[i].resolution;
                for (label, a, b) in ASPECTS.into_iter().filter(|(_, a, b)| !has_aspect(frame, (*a, *b))) {
                    let (w, h) = aspect_size(frame, (a, b));
                    if ui.button(format!("{label}  ({w}\u{00d7}{h})")).on_hover_text("A copy at this frame, each picture following its subject; pictures with no clear subject are marked for review").clicked() {
                        cmds.push(AppCommand::NewAspectVersion { sequence_idx: i, width: w, height: h, name: format!("{name} {label}") });
                        ui.close_menu();
                    }
                }
            });
            if ui.button("Sequence Settings...").clicked() {
                cmds.push(AppCommand::OpenSequenceSettings { sequence_idx: i });
                ui.close_menu();
            }
            // Stacked above the cut, to skim and take ranges from (#82).
            let as_selects = state.selects_sequence == Some(state.sequences[i].id);
            if ui.add_enabled(!is_active && !as_selects, egui::Button::new("Open as Selects")).on_disabled_hover_text(if is_active { "This is the sequence being cut" } else { "Open above the cut already" }).clicked() {
                state.selects_sequence = Some(state.sequences[i].id);
                state.selects_range = None;
                ui.close_menu();
            }
            if ui.add_enabled(can_close, egui::Button::new("Close Tab")).clicked() {
                state.closed_sequence_tabs.insert(state.sequences[i].id);
                if is_active {
                    if let Some(next) = tab_after_closing(state, i) {
                        cmds.push(AppCommand::SetActiveSequence { sequence_idx: next });
                    }
                }
                ui.close_menu();
            }
            ui.separator();
            let can_delete = state.sequences.len() > 1;
            if ui.add_enabled(can_delete, egui::Button::new(
                egui::RichText::new("Delete Sequence…").color(egui::Color32::from_rgb(200, 60, 60))
            )).clicked() {
                // Asked first: a deleted sequence and its undo history
                // cannot be brought back.
                state.confirm_delete_sequence = Some(state.sequences[i].id);
                ui.close_menu();
            }
        });

        x += tab_w + TAB_GAP;
    }

    // "+" button to add new sequence
    let add_rect = egui::Rect::from_min_size(
        egui::pos2(x + 4.0, y),
        egui::vec2(ADD_BTN_W, TAB_HEIGHT),
    );
    let add_resp = ui.allocate_rect(add_rect, egui::Sense::click());
    let add_color = if add_resp.hovered() {
        theme.text_primary
    } else {
        theme.text_dim
    };
    ui.painter().text(
        add_rect.center(),
        egui::Align2::CENTER_CENTER,
        "+",
        egui::FontId::proportional(16.0),
        add_color,
    );
    let add_resp = add_resp.on_hover_text("New sequence");

    // Every sequence in the project, to open one whose tab was closed.
    let list_rect = egui::Rect::from_min_size(
        egui::pos2(add_rect.max.x, y),
        egui::vec2(ADD_BTN_W - 8.0, TAB_HEIGHT),
    );
    let list_resp = ui.allocate_rect(list_rect, egui::Sense::click());
    ui.painter().text(
        list_rect.center(),
        egui::Align2::CENTER_CENTER,
        "▾",
        egui::FontId::proportional(13.0),
        if list_resp.hovered() { theme.text_primary } else { theme.text_dim },
    );
    let list_resp = list_resp.on_hover_text("Open a sequence");
    let popup_id = ui.make_persistent_id("sequence_list_popup");
    if list_resp.clicked() {
        ui.memory_mut(|m| m.toggle_popup(popup_id));
    }
    egui::popup_below_widget(ui, popup_id, &list_resp, egui::PopupCloseBehavior::CloseOnClick, |ui| {
        ui.set_min_width(200.0);
        for (i, seq) in state.sequences.iter().enumerate() {
            let open = i == state.active_sequence_idx || !state.closed_sequence_tabs.contains(&seq.id);
            let label = if open { format!("✓ {}", seq.name) } else { format!("   {}", seq.name) };
            if ui.selectable_label(i == state.active_sequence_idx, label).clicked() {
                cmds.push(AppCommand::SetActiveSequence { sequence_idx: i });
            }
        }
    });

    if add_resp.clicked() {
        // New sequence: inherit fps from active, but resolution comes from
        // the "New sequence resolution" pref when set (idx > 0). Idx 0 =
        // Automatic, which keeps the historical inherit-from-active
        // behaviour. Lets users pin a vertical/square default if their
        // workflow is mainly that orientation.
        use crate::panels::new_project_dialog::RESOLUTION_PRESETS;
        let active = &state.sequences[state.active_sequence_idx];
        let n = state.sequences.len() + 1;
        let pref_idx = state.default_sequence_resolution_idx;
        let (width, height) = if pref_idx > 0 && pref_idx < RESOLUTION_PRESETS.len() {
            let (_, w, h) = RESOLUTION_PRESETS[pref_idx];
            if w > 0 && h > 0 { (w, h) } else { (active.resolution.width, active.resolution.height) }
        } else {
            (active.resolution.width, active.resolution.height)
        };
        cmds.push(AppCommand::AddSequence {
            name: format!("Sequence {n}"),
            fps_num: active.fps.num,
            fps_den: active.fps.den,
            width,
            height,
        });
    }

    // Inline rename popup
    if let Some(rename_idx) = state.renaming_sequence_idx {
        if rename_idx < state.sequences.len() {
            let mut open = true;
            egui::Window::new("Rename Sequence")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ui.ctx(), |ui| {
                    ui.horizontal(|ui| {
                        let resp = ui.text_edit_singleline(&mut state.rename_sequence_buf);
                        if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            let new_name = state.rename_sequence_buf.trim().to_string();
                            if !new_name.is_empty() {
                                cmds.push(AppCommand::RenameSequence {
                                    sequence_idx: rename_idx,
                                    new_name,
                                });
                            }
                            state.renaming_sequence_idx = None;
                        }
                        if ui.button("OK").clicked() {
                            let new_name = state.rename_sequence_buf.trim().to_string();
                            if !new_name.is_empty() {
                                cmds.push(AppCommand::RenameSequence {
                                    sequence_idx: rename_idx,
                                    new_name,
                                });
                            }
                            state.renaming_sequence_idx = None;
                        }
                    });
                });
            if !open {
                state.renaming_sequence_idx = None;
            }
        } else {
            state.renaming_sequence_idx = None;
        }
    }

    // Inside an opened compound clip: the way back, right-aligned. Each
    // parent is a step up (as many as it takes), and ‹ goes up one (#910).
    // Nothing in the app led out of a compound clip before.
    if let Some(crumbs) = breadcrumb(state) {
        let font = egui::FontId::proportional(11.0);
        let mut right = rect.max.x - 8.0;
        for (i, (name, up)) in crumbs.iter().enumerate().rev() {
            let current = *up == 0;
            let text = if i == 0 { name.clone() } else { format!("\u{203A} {name}") };
            let w = ui.painter().layout_no_wrap(text.clone(), font.clone(), theme.text_primary).rect.width();
            let r = egui::Rect::from_min_size(egui::pos2(right - w, y), egui::vec2(w, TAB_HEIGHT));
            let resp = ui.allocate_rect(r, if current { egui::Sense::hover() } else { egui::Sense::click() });
            let color = if current { theme.text_primary } else if resp.hovered() { theme.accent } else { theme.text_dim };
            ui.painter().text(egui::pos2(r.min.x, r.center().y), egui::Align2::LEFT_CENTER, &text, font.clone(), color);
            if !current && resp.on_hover_text(format!("Back to {name}")).clicked() {
                cmds.extend(std::iter::repeat_n(AppCommand::CloseCompoundClip, *up));
            }
            right -= w + 6.0;
        }
        let back = egui::Rect::from_min_size(egui::pos2(right - 18.0, y), egui::vec2(18.0, TAB_HEIGHT));
        let resp = ui.allocate_rect(back, egui::Sense::click());
        ui.painter().text(
            back.center(),
            egui::Align2::CENTER_CENTER,
            "\u{2039}",
            egui::FontId::proportional(15.0),
            if resp.hovered() { theme.accent } else { theme.text_secondary },
        );
        let shortcut = crate::keybindings::KeyBindings::defaults().display_for("leave_compound");
        if resp.on_hover_text(format!("Leave the compound clip ({shortcut})")).clicked() {
            cmds.push(AppCommand::CloseCompoundClip);
        }
    }

    cmds
}

pub const HEIGHT: f32 = TAB_HEIGHT;

/// How many tabs are open: the active one always, and every other
/// sequence whose tab was not closed.
fn open_tab_count(state: &AppState) -> usize {
    state
        .sequences
        .iter()
        .enumerate()
        .filter(|(i, s)| *i == state.active_sequence_idx || !state.closed_sequence_tabs.contains(&s.id))
        .count()
}

/// The tab to show when the active one at `closing` is closed: the next
/// open tab to its right, else the nearest to its left.
fn tab_after_closing(state: &AppState, closing: usize) -> Option<usize> {
    let open = |i: usize| i != closing && !state.closed_sequence_tabs.contains(&state.sequences[i].id);
    (closing + 1..state.sequences.len()).find(|&i| open(i)).or_else(|| (0..closing).rev().find(|&i| open(i)))
}

/// The way back out of an opened compound clip: each sequence from the
/// outermost to the one shown, with how many steps up it is (0 for the
/// one shown), or `None` when no compound clip is open.
fn breadcrumb(state: &AppState) -> Option<Vec<(String, usize)>> {
    if state.compound_clip_stack.is_empty() {
        return None;
    }
    let depth = state.compound_clip_stack.len();
    let mut crumbs: Vec<(String, usize)> = state
        .compound_clip_stack
        .iter()
        .enumerate()
        .filter_map(|(i, (idx, _))| state.sequences.get(*idx).map(|s| (s.name.clone(), depth - i)))
        .collect();
    crumbs.push((state.sequence().name.clone(), 0));
    Some(crumbs)
}

#[cfg(test)]
mod tests {
    use super::{open_tab_count, tab_after_closing};
    use crate::state::AppState;

    fn three() -> AppState {
        let mut state = AppState::default();
        let first = state.sequences[0].clone();
        for name in ["B", "C"] {
            let mut s = first.clone();
            s.id = klipp_core::SequenceId::new();
            s.name = name.into();
            state.sequences.push(s);
        }
        state
    }

    #[test]
    fn closing_a_tab_keeps_the_sequence_and_moves_to_a_neighbour() {
        let mut state = three();
        state.active_sequence_idx = 1;
        state.closed_sequence_tabs.insert(state.sequences[1].id);
        assert_eq!(state.sequences.len(), 3, "the sequence stays in the project");
        assert_eq!(tab_after_closing(&state, 1), Some(2), "the tab to the right");
        state.closed_sequence_tabs.insert(state.sequences[2].id);
        assert_eq!(tab_after_closing(&state, 1), Some(0), "else the one to the left");
    }

    #[test]
    fn the_active_tab_counts_as_open_even_if_it_was_closed() {
        let mut state = three();
        for s in &state.sequences {
            state.closed_sequence_tabs.insert(s.id);
        }
        assert_eq!(open_tab_count(&state), 1, "the last tab cannot be closed");
    }
}

#[cfg(test)]
mod breadcrumb_tests {
    use super::breadcrumb;
    use crate::state::AppState;

    #[test]
    fn the_way_back_names_each_parent_and_how_far_up_it_is() {
        let mut state = AppState::default();
        assert!(breadcrumb(&state).is_none(), "no compound clip open");
        let mut inner = state.sequences[0].clone();
        inner.id = klipp_core::SequenceId::new();
        inner.name = "Compound 1".into();
        state.sequences.push(inner);
        let main = state.sequences[0].name.clone();
        state.compound_clip_stack.push((0, state.playhead));
        state.active_sequence_idx = 1;
        assert_eq!(breadcrumb(&state), Some(vec![(main, 1), ("Compound 1".to_string(), 0)]));
    }
}

