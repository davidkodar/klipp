use crate::widgets::menu::SubmenuRows;
use crate::icons::Icon;
use crate::state::{AppState, PanelKind};
use klipp_core::{AppCommand, MarkerColor, MarkerId, MarkerKind, RationalTime};

#[derive(Default)]
pub struct MarkersPanel {
    editing_label: Option<MarkerId>,
    label_buf: String,
    /// Which marker (if any) currently has its comment field
    /// expanded for editing. Mirrors `editing_label` for the
    /// new comment surface.
    editing_comment: Option<MarkerId>,
    comment_buf: String,
    /// Case-insensitive substring filter on marker label, comment,
    /// and kind label. When non-empty, the rendered list is narrowed
    /// to matches; pressing Enter while focused jumps the playhead
    /// to the first match.
    search_query: String,
    /// Only open review comments listed (#2214).
    open_reviews_only: bool,
}

/// Case-insensitive substring match against a marker's label, comment,
/// and kind. Any one matching is enough. Empty query matches
/// everything. The kind is matched against both its display label
/// ("To Do", "Best Take") and its space-free form ("todo",
/// "besttake") so users can find a kind either way they type it.
/// Pulled out of show() so unit tests can exercise the predicate
/// without the egui surface.
pub fn marker_matches(label: &str, comment: &str, kind: MarkerKind, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    let kind_label = kind.label().to_lowercase();
    let kind_compact: String = kind_label.chars().filter(|c| !c.is_whitespace()).collect();
    label.to_lowercase().contains(&q)
        || comment.to_lowercase().contains(&q)
        || kind_label.contains(&q)
        || kind_compact.contains(&q)
}

impl MarkersPanel {
    pub fn show(&mut self, ctx: &egui::Context, state: &mut AppState) -> Vec<AppCommand> {
        let mut cmds = Vec::new();
        // A marker asked for by its flag on the ruler, or Edit Marker…:
        // its label opened for editing, and nothing filtering it out.
        if let Some(id) = state.edit_marker_request.take() {
            if let Some(m) = state.sequence().markers.iter().find(|m| m.id == id) {
                self.editing_label = Some(id);
                self.label_buf = m.label.clone();
                self.search_query.clear();
            }
        }
        let mut open = state.show_markers_panel;

        let response = egui::Window::new("Markers")
            .open(&mut open)
            .default_width(280.0)
            .default_height(300.0)
            .resizable(true)
            .show(ctx, |ui| {
                // Search field. Filters the rendered list by
                // case-insensitive substring match on label. Enter
                // jumps to the first matching marker (point or range
                // start). Honours Ctrl+F focus_search_request.
                ui.horizontal(|ui| {
                    let search = egui::TextEdit::singleline(&mut self.search_query)
                        .hint_text("Filter markers\u{2026}")
                        .desired_width(ui.available_width());
                    let resp = ui.add(search);
                    if state.focus_search_request == Some(PanelKind::Markers) {
                        resp.request_focus();
                        state.focus_search_request = None;
                    }
                    if resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        // Jump to first match. Lookup runs against
                        // the live sequence rather than caching, so
                        // the filtered set is always fresh.
                        let q = self.search_query.clone();
                        if let Some(first) = state
                            .sequence()
                            .markers
                            .iter()
                            .find(|m| marker_matches(&m.label, &m.comment, m.kind, &q))
                        {
                            cmds.push(AppCommand::JumpToMarker { marker_id: first.id });
                        }
                    }
                });

                ui.separator();

                // Add marker button + export.
                ui.horizontal(|ui| {
                    if ui.button("+ Add Marker").clicked() {
                        cmds.push(AppCommand::AddMarker {
                            position: state.playhead,
                            label: String::new(),
                            color: MarkerColor::Blue,
                        });
                    }
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.menu_rows("Review", |ui| {
                                if ui.button("Export Review Copy\u{2026}").on_hover_text("Render the sequence with its timecode and review round burned in, and a comment file beside it").clicked() {
                                    let name = format!("{}-review.mp4", state.sequence().name.replace(['/', '\\'], "-"));
                                    crate::file_dialog::spawn(move || {
                                        rfd::FileDialog::new().set_title("Export Review Copy").add_filter("MP4 video", &["mp4"]).set_file_name(&name).save_file()
                                            .map(|path| vec![AppCommand::ExportReviewCopy { path }]).unwrap_or_default()
                                    });
                                    ui.close_menu();
                                }
                                if ui.button("Import Review Comments\u{2026}").on_hover_text("Add a reviewer's comments, with their timecodes, as review markers").clicked() {
                                    crate::file_dialog::spawn(move || {
                                        rfd::FileDialog::new().set_title("Import Review Comments").add_filter("Comments", &["csv", "tsv", "txt"]).pick_file()
                                            .map(|path| vec![AppCommand::ImportReviewComments { path }]).unwrap_or_default()
                                    });
                                    ui.close_menu();
                                }
                                if ui.button("Export Review List\u{2026}").on_hover_text("Write the review comments and where each stands, for the next round").clicked() {
                                    crate::file_dialog::spawn(move || {
                                        rfd::FileDialog::new().set_title("Export Review List").add_filter("CSV", &["csv"]).set_file_name("review-list.csv").save_file()
                                            .map(|path| vec![AppCommand::ExportReviewList { path }]).unwrap_or_default()
                                    });
                                    ui.close_menu();
                                }
                                ui.separator();
                                ui.checkbox(&mut self.open_reviews_only, "Show only open review comments");
                            });
                            if ui
                                .small_button("\u{2B07} YouTube Chapters")
                                .on_hover_text("Export markers as YouTube chapter timestamps")
                                .clicked()
                            {
                                crate::file_dialog::spawn(move || {
                                    rfd::FileDialog::new()
                                        .set_title("Export YouTube Chapters")
                                        .add_filter("Text file", &["txt"])
                                        .set_file_name("chapters.txt")
                                        .save_file()
                                        .map(|path| vec![AppCommand::ExportYouTubeChapters { path }])
                                        .unwrap_or_default()
                                });
                            }
                        },
                    );
                });

                ui.separator();

                // Marker list
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let mut remove_id = None;
                    let fps_rate = state.sequence().fps.rate();
                    let query = self.search_query.clone();
                    let open_only = self.open_reviews_only;
                    let markers: Vec<_> = state.sequence().markers.iter()
                        .filter(|m| marker_matches(&m.label, &m.comment, m.kind, &query))
                        .filter(|m| !open_only || m.review.as_ref().is_some_and(|r| r.status == klipp_core::ReviewStatus::Open))
                        .map(|m| (m.id, m.position, m.label.clone(), m.color, m.duration, m.comment.clone(), m.kind, m.review.clone()))
                        .collect();

                    for (id, position, label, color, duration, comment, kind, review) in &markers {
                        // Outer vertical group so the optional comment
                        // sub-row stacks under the main row for the
                        // same marker without competing with siblings.
                        ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            // Color button
                            let (r, g, b) = color.to_rgb();
                            let btn_color = egui::Color32::from_rgb(r, g, b);
                            let btn = egui::Button::new("●")
                                .fill(egui::Color32::TRANSPARENT);
                            let resp = ui.add(btn);
                            ui.painter().text(
                                resp.rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "●",
                                egui::FontId::proportional(13.0),
                                btn_color,
                            );
                            if resp.clicked() {
                                // Cycle to next color
                                let colors = MarkerColor::all();
                                let idx = colors.iter().position(|c| c == color).unwrap_or(0);
                                let next = colors[(idx + 1) % colors.len()];
                                cmds.push(AppCommand::UpdateMarkerColor {
                                    marker_id: *id,
                                    color: next,
                                });
                            }

                            // Kind dropdown — compact, single-letter
                            // badge button for the current kind. The
                            // popup lists every kind with its full
                            // label so users learn what each badge means.
                            let kind_badge = if kind.badge().is_empty() { "·" } else { kind.badge() };
                            egui::ComboBox::from_id_salt(("marker_kind", *id))
                                .selected_text(
                                    egui::RichText::new(kind_badge)
                                        .size(11.0)
                                        .monospace()
                                )
                                .width(28.0)
                                .show_ui(ui, |ui| {
                                    for k in MarkerKind::all() {
                                        let label = if k.badge().is_empty() {
                                            k.label().to_string()
                                        } else {
                                            format!("{}  {}", k.badge(), k.label())
                                        };
                                        if ui.selectable_label(*kind == *k, label).clicked() {
                                            cmds.push(AppCommand::UpdateMarkerKind {
                                                marker_id: *id,
                                                kind: *k,
                                            });
                                        }
                                    }
                                })
                                .response
                                .on_hover_text(format!("Marker kind: {}", kind.label()));

                            // Timecode
                            let tc_str = format_timecode_short(position.to_secs_f64());
                            if ui.small_button(&tc_str).clicked() {
                                cmds.push(AppCommand::JumpToMarker { marker_id: *id });
                            }

                            // Label (inline edit)
                            if self.editing_label == Some(*id) {
                                let resp = ui.text_edit_singleline(&mut self.label_buf);
                                if resp.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                                    cmds.push(AppCommand::UpdateMarkerLabel {
                                        marker_id: *id,
                                        label: self.label_buf.clone(),
                                    });
                                    self.editing_label = None;
                                }
                            } else {
                                let display = if label.is_empty() { "(no label)" } else { label.as_str() };
                                let resp = ui.label(
                                    egui::RichText::new(display)
                                        .size(11.0)
                                        .color(if label.is_empty() {
                                            egui::Color32::from_rgb(100, 100, 110)
                                        } else {
                                            egui::Color32::from_rgb(200, 200, 210)
                                        }),
                                );
                                if resp.double_clicked() {
                                    self.editing_label = Some(*id);
                                    self.label_buf = label.clone();
                                }
                            }

                            // A review comment's round and where it stands (#2214).
                            if let Some(r) = review {
                                egui::ComboBox::from_id_salt(("review_status", *id))
                                    .selected_text(egui::RichText::new(r.status.label()).size(11.0))
                                    .width(64.0)
                                    .show_ui(ui, |ui| {
                                        for s in klipp_core::ReviewStatus::all() {
                                            if ui.selectable_label(r.status == *s, s.label()).clicked() {
                                                cmds.push(AppCommand::SetReviewStatus { marker_id: *id, status: *s });
                                            }
                                        }
                                    })
                                    .response
                                    .on_hover_text(format!("Review round {}", r.round));
                            }

                            // Right-side controls: delete, duration editor.
                            // Items are added right-to-left; visual order is
                            // [duration editor] [✕ delete].
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.add(egui::Button::image(Icon::X.image(10.0).tint(egui::Color32::from_rgb(220, 220, 230)))).clicked() {
                                    remove_id = Some(*id);
                                }
                                // Range-duration control — + Range promotes a
                                // point marker to a 5s range; an existing
                                // range shows its duration and a ✕ to clear.
                                // Drag-to-adjust is deferred polish.
                                if let Some(dur) = duration {
                                    if ui
                                        .small_button("\u{00D7}")
                                        .on_hover_text("Clear range (revert to point marker)")
                                        .clicked()
                                    {
                                        cmds.push(AppCommand::SetMarkerDuration {
                                            marker_id: *id,
                                            duration: None,
                                        });
                                    }
                                    let secs = dur.to_secs_f64();
                                    ui.label(
                                        egui::RichText::new(format!("{secs:.1}s"))
                                            .size(11.0)
                                            .monospace()
                                            .color(egui::Color32::from_rgb(160, 170, 190)),
                                    );
                                } else if ui
                                    .small_button("+ Range")
                                    .on_hover_text("Convert to range marker (5s default)")
                                    .clicked()
                                {
                                    let default_dur = RationalTime::from_secs_f64(5.0, fps_rate);
                                    cmds.push(AppCommand::SetMarkerDuration {
                                        marker_id: *id,
                                        duration: Some(default_dur),
                                    });
                                }
                            });
                        });

                        // Comment sub-row — expanded text-edit when
                        // the user clicks "+ Add comment" or has an
                        // existing comment to edit; collapsed
                        // single-button row otherwise. Indented to
                        // visually subordinate it to the main row.
                        ui.horizontal(|ui| {
                            ui.add_space(28.0); // align under the colour cycler
                            if self.editing_comment == Some(*id) {
                                let resp = ui.add(
                                    egui::TextEdit::multiline(&mut self.comment_buf)
                                        .desired_rows(2)
                                        .desired_width(f32::INFINITY)
                                        .hint_text("Comment / note\u{2026}")
                                        .font(egui::FontId::proportional(11.0)),
                                );
                                // Commit on lost focus. Multi-line
                                // TextEdit consumes Enter for newlines
                                // (intended), so we don't gate on
                                // Enter the way the label edit does.
                                if resp.lost_focus() {
                                    cmds.push(AppCommand::UpdateMarkerComment {
                                        marker_id: *id,
                                        comment: self.comment_buf.clone(),
                                    });
                                    self.editing_comment = None;
                                }
                            } else if comment.is_empty() {
                                if ui
                                    .small_button("\u{271A} Comment")
                                    .on_hover_text("Add a comment / note for this marker")
                                    .clicked()
                                {
                                    self.editing_comment = Some(*id);
                                    self.comment_buf = String::new();
                                }
                            } else {
                                // Existing comment — show preview;
                                // double-click to edit. Truncate to
                                // ~60 chars in the row so long notes
                                // don't bloat the panel.
                                let preview: String = if comment.chars().count() > 60 {
                                    let head: String = comment.chars().take(57).collect();
                                    format!("{head}\u{2026}")
                                } else {
                                    comment.clone()
                                };
                                let resp = ui.label(
                                    egui::RichText::new(preview)
                                        .size(11.0)
                                        .italics()
                                        .color(egui::Color32::from_rgb(140, 150, 170)),
                                )
                                .on_hover_text(comment.as_str());
                                if resp.double_clicked() {
                                    self.editing_comment = Some(*id);
                                    self.comment_buf = comment.clone();
                                }
                            }
                        });
                        }); // end vertical
                        ui.add_space(2.0);
                    }

                    if let Some(id) = remove_id {
                        cmds.push(AppCommand::RemoveMarker { marker_id: id });
                    }

                    if markers.is_empty() {
                        let msg = if self.search_query.is_empty() {
                            "No markers. Press M to add one.".to_string()
                        } else {
                            format!("No markers match \u{201C}{}\u{201D}", self.search_query)
                        };
                        ui.label(
                            egui::RichText::new(msg)
                                .size(11.0)
                                .color(egui::Color32::from_rgb(100, 100, 110)),
                        );
                    }
                });
            });

        // Mark the panel active when any pointer press lands inside
        // its window rect this frame. Scopes Ctrl+F (and any future
        // panel-keyed shortcuts) to this panel when it has focus.
        if let Some(inner) = response.as_ref() {
            crate::state::note_active_panel(state, PanelKind::Markers, ctx, inner.response.rect);
        }

        state.show_markers_panel = open;
        cmds
    }
}

fn format_timecode_short(secs: f64) -> String {
    let m = (secs / 60.0) as u32;
    let s = secs % 60.0;
    format!("{:02}:{:05.2}", m, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> MarkerKind { MarkerKind::None }

    #[test]
    fn empty_query_matches_everything() {
        assert!(marker_matches("", "", none(), ""));
        assert!(marker_matches("anything", "", none(), ""));
        assert!(marker_matches("Cut on action", "", none(), ""));
    }

    #[test]
    fn query_is_case_insensitive_substring_on_label() {
        assert!(marker_matches("Cut on action", "", none(), "cut"));
        assert!(marker_matches("Cut on action", "", none(), "CUT"));
        assert!(marker_matches("Cut on action", "", none(), "ACTION"));
        assert!(marker_matches("Cut on action", "", none(), "n a"));
    }

    #[test]
    fn non_matching_query_filters_out() {
        assert!(!marker_matches("Cut on action", "", none(), "music"));
        assert!(!marker_matches("Best take", "", none(), "cut"));
        assert!(!marker_matches("", "", none(), "anything"));
    }

    #[test]
    fn query_matches_comment() {
        // Label doesn't match — comment carries the search hit.
        assert!(marker_matches("M3", "trim two frames off", none(), "trim"));
        assert!(marker_matches("M3", "Trim two frames off", none(), "TRIM"));
        assert!(!marker_matches("M3", "trim two frames off", none(), "music"));
    }

    #[test]
    fn query_matches_kind_label() {
        // Searching "chapter" finds any marker whose kind is Chapter,
        // even if its label and comment don't mention it.
        assert!(marker_matches("M1", "", MarkerKind::Chapter, "chapter"));
        assert!(marker_matches("M2", "", MarkerKind::ToDo, "todo"));
        assert!(marker_matches("M3", "", MarkerKind::BestTake, "BEST"));
        // None has the literal label "None" — also searchable, which
        // is fine and explicit (lets users find "untyped" markers).
        assert!(marker_matches("M4", "", MarkerKind::None, "none"));
    }

    #[test]
    fn query_can_match_any_of_three_fields() {
        // Same query "scene" can hit label, comment, or kind label —
        // pick whichever field has the substring.
        assert!(marker_matches("Scene 2", "no notes", MarkerKind::Chapter, "scene"));
        assert!(marker_matches("Untitled", "remember scene 4", MarkerKind::None, "scene"));
        assert!(marker_matches("Untitled", "no notes", MarkerKind::Section, "section"));
    }
}
